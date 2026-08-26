//! Reliable PCM ingress shared by browsers, Android, and the DingTalk mini-app foreground probe.

use crate::app::AppState;
use crate::explanation::create_explanation;
use crate::worker::{AsrRequest, GlossaryConstraint, TranslationRequest};
use aialra_core_domain::SessionState;
use aialra_event_store::AudioChunkRecord;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, State};
use axum::response::Response;
use base64::Engine;
use chrono::Utc;
use futures_util::{SinkExt, StreamExt};
use serde_json::json;
use std::sync::atomic::Ordering;
use uuid::Uuid;

const HEADER_BYTES: usize = 16;
const SAMPLE_RATE: u32 = 16_000;
const CHANNELS: u16 = 1;
const PCM_BYTES_PER_SECOND: usize = SAMPLE_RATE as usize * 2;
const MIN_ASR_WINDOW_BYTES: usize = PCM_BYTES_PER_SECOND * 2;
const MAX_ASR_WINDOW_BYTES: usize = PCM_BYTES_PER_SECOND * 8;
const SILENCE_LOOKBACK_BYTES: usize = PCM_BYTES_PER_SECOND;
const SILENCE_MEAN_ABSOLUTE_PCM: i64 = 550;
const MAX_FRAME_BYTES: usize = PCM_BYTES_PER_SECOND * 3 + HEADER_BYTES;

pub async fn audio_websocket(
    State(state): State<AppState>,
    Path((session_id, source_id)): Path<(String, String)>,
    upgrade: WebSocketUpgrade,
) -> Response {
    upgrade
        .max_message_size(MAX_FRAME_BYTES)
        .on_upgrade(move |socket| handle_socket(state, session_id, source_id, socket))
}

async fn handle_socket(state: AppState, session_id: String, source_id: String, socket: WebSocket) {
    let Some(session) = state.store.get_session(&session_id).ok().flatten() else {
        return;
    };
    if !matches!(
        session.state,
        SessionState::Recording | SessionState::Degraded
    ) {
        return;
    }

    let (mut sender, mut receiver) = socket.split();
    while let Some(Ok(message)) = receiver.next().await {
        match message {
            Message::Binary(frame) => {
                let result = persist_frame(&state, &session_id, &source_id, &frame).await;
                let response = match result {
                    Ok((sequence, duplicate)) => json!({
                        "type": "audio.ack",
                        "sequence": sequence,
                        "duplicate": duplicate
                    }),
                    Err(error) => json!({
                        "type": "audio.error",
                        "message": error.to_string()
                    }),
                };
                if sender
                    .send(Message::Text(response.to_string().into()))
                    .await
                    .is_err()
                {
                    break;
                }
            }
            Message::Ping(bytes) => match sender.send(Message::Pong(bytes)).await {
                Ok(()) => {}
                Err(_) => break,
            },
            Message::Close(_) => break,
            _ => {}
        }
    }
}

async fn persist_frame(
    state: &AppState,
    session_id: &str,
    source_id: &str,
    frame: &[u8],
) -> anyhow::Result<(u64, bool)> {
    if frame.len() <= HEADER_BYTES || frame.len() > MAX_FRAME_BYTES {
        anyhow::bail!("audio frame has an invalid size");
    }
    let sequence = u64::from_be_bytes(frame[..8].try_into()?);
    let captured_at_ms = u64::from_be_bytes(frame[8..16].try_into()?);
    let pcm = &frame[HEADER_BYTES..];
    if !pcm.len().is_multiple_of(2) {
        anyhow::bail!("PCM frame must contain complete 16-bit samples");
    }
    let duration_ms = ((pcm.len() as u64 * 1_000) / (SAMPLE_RATE as u64 * 2)) as u32;
    let stored = state.objects.put(pcm)?;
    let inserted = state.store.insert_audio_chunk(&AudioChunkRecord {
        session_id: session_id.to_owned(),
        source_id: source_id.to_owned(),
        sequence,
        captured_at_ms,
        sample_rate: SAMPLE_RATE,
        channels: CHANNELS,
        encoding: "pcm_s16le".to_owned(),
        duration_ms,
        object_hash: stored.hash.clone(),
        size_bytes: stored.size_bytes,
        acknowledged_at: Utc::now(),
    })?;
    if inserted {
        let event = aialra_event_protocol::EventEnvelope::new(
            session_id,
            source_id,
            sequence,
            "audio.chunk.received",
            captured_at_ms.saturating_mul(1_000_000),
            format!("audio_{source_id}_{sequence}"),
            None,
            json!({
                "object_hash": stored.hash,
                "size_bytes": stored.size_bytes,
                "duration_ms": duration_ms,
                "sample_rate": SAMPLE_RATE,
                "channels": CHANNELS,
                "encoding": "pcm_s16le"
            }),
        )?;
        if state.store.insert_event(&event)? {
            let _ = state.events.send(event);
        }

        // A short pause closes a phrase early; uninterrupted speech is bounded at eight seconds.
        let key = format!("{session_id}:{source_id}");
        let maybe_window = {
            let mut buffers = state
                .audio_buffers
                .lock()
                .map_err(|_| anyhow::anyhow!("audio buffer lock poisoned"))?;
            let buffer = buffers.entry(key).or_default();
            buffer.extend_from_slice(pcm);
            let pause_after_minimum =
                buffer.len() >= MIN_ASR_WINDOW_BYTES && trailing_audio_is_silent(buffer);
            if pause_after_minimum || buffer.len() >= MAX_ASR_WINDOW_BYTES {
                Some(std::mem::take(buffer))
            } else {
                None
            }
        };
        if let Some(window) = maybe_window {
            state.pending_model_jobs.fetch_add(1, Ordering::SeqCst);
            let state = state.clone();
            let session_id = session_id.to_owned();
            let source_id = source_id.to_owned();
            tokio::spawn(async move {
                process_window(state.clone(), session_id, source_id, captured_at_ms, window).await;
                if state.pending_model_jobs.fetch_sub(1, Ordering::SeqCst) == 1 {
                    state.model_drained.notify_waiters();
                }
            });
        }
    }
    Ok((sequence, !inserted))
}

fn trailing_audio_is_silent(pcm: &[u8]) -> bool {
    // One second of low-amplitude PCM closes a phrase without sending its trailing silence onward.
    let lookback_start = pcm.len().saturating_sub(SILENCE_LOOKBACK_BYTES);
    let trailing = &pcm[lookback_start..];
    let mut total = 0_i64;
    let mut samples = 0_i64;
    for pair in trailing.chunks_exact(2) {
        let sample = i16::from_le_bytes([pair[0], pair[1]]) as i64;
        total += sample.abs();
        samples += 1;
    }
    samples > 0 && total / samples < SILENCE_MEAN_ABSOLUTE_PCM
}

async fn process_window(
    state: AppState,
    session_id: String,
    source_id: String,
    captured_at_ms: u64,
    pcm: Vec<u8>,
) {
    // The semaphore serializes only ASR work so slower translation cannot delay the next audio window.
    let Ok(asr_permit) = state.asr_slots.acquire().await else {
        return;
    };
    let Ok(Some(session)) = state.store.get_session(&session_id) else {
        return;
    };
    let correlation = format!("asr_{}", Uuid::now_v7().simple());
    let response = state
        .worker
        .transcribe(&AsrRequest {
            pcm_s16le_base64: base64::engine::general_purpose::STANDARD.encode(pcm),
            sample_rate: SAMPLE_RATE,
            language: session.source_language.clone(),
            initial_prompt: String::new(),
        })
        .await;
    let asr = match response {
        Ok(result) if !result.text.trim().is_empty() => result,
        Ok(_) => return,
        Err(error) => {
            let _ = state.emit(
                &session_id,
                "model_scheduler",
                "model.run.failed",
                captured_at_ms.saturating_mul(1_000_000),
                &correlation,
                None,
                json!({"role": "asr", "error_kind": "worker_unavailable", "source_id": source_id}),
            );
            tracing::warn!(error = %error, session_id, "ASR window failed");
            return;
        }
    };
    // Release ASR capacity before translation because the audio path has priority over downstream enrichment.
    drop(asr_permit);
    let segment_id = format!("seg_{}", Uuid::now_v7().simple());
    let partial = state.emit(
        &session_id,
        "local_asr",
        "asr.partial.updated",
        captured_at_ms.saturating_mul(1_000_000),
        &correlation,
        None,
        json!({"segment_id": segment_id, "text": asr.text, "provider": asr.provider}),
    );
    let finalized = state.emit(
        &session_id,
        "local_asr",
        "segment.finalized",
        captured_at_ms.saturating_mul(1_000_000),
        &correlation,
        partial.ok().map(|event| event.event_id.to_string()),
        json!({
            "segment_id": segment_id,
            "text": asr.text,
            "language": asr.language,
            "confidence": asr.confidence,
            "duration_ms": asr.duration_ms,
            "provider": asr.provider
        }),
    );
    let Ok(finalized) = finalized else {
        return;
    };
    let source_text = finalized
        .payload
        .get("text")
        .and_then(|value| value.as_str())
        .unwrap_or_default()
        .to_owned();
    match state
        .worker
        .translate(&TranslationRequest {
            text: source_text,
            source_language: session.source_language,
            target_language: session.target_language,
            glossary: Vec::<GlossaryConstraint>::new(),
            context: Vec::new(),
        })
        .await
    {
        Ok(translation) => {
            let _ = state.emit(
                &session_id,
                "local_translation",
                "translation.finalized",
                captured_at_ms.saturating_mul(1_000_000),
                &correlation,
                Some(finalized.event_id.to_string()),
                json!({"segment_id": segment_id, "translation_id": format!("tr_{segment_id}"), "text": translation.text, "provider": translation.provider}),
            );
            maybe_create_periodic_explanation(&state, &session_id).await;
        }
        Err(error) => {
            tracing::warn!(error = %error, session_id, "translation failed after ASR");
        }
    }
}

async fn maybe_create_periodic_explanation(state: &AppState, session_id: &str) {
    // A volume trigger avoids interrupting every short caption while still helping during a long lecture.
    let interval = std::env::var("AIALRA_EXPLAIN_EVERY_SEGMENTS")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(5);
    let Ok(events) = state.store.list_events(session_id) else {
        return;
    };
    let segment_count = events
        .iter()
        .filter(|event| event.event_type == "segment.finalized")
        .count();
    if segment_count > 0
        && segment_count.is_multiple_of(interval)
        && let Err(error) = create_explanation(state, session_id, "segment_volume").await
    {
        tracing::warn!(error = %error, session_id, "periodic explanation failed");
    }
}

#[cfg(test)]
mod tests {
    use super::{SILENCE_LOOKBACK_BYTES, trailing_audio_is_silent};

    #[test]
    fn audio_frame_header_round_trips() {
        // Browser and Android clients use the same big-endian sequence and capture-time header.
        let sequence = 42_u64;
        let captured = 1_234_u64;
        let mut frame = Vec::new();
        frame.extend_from_slice(&sequence.to_be_bytes());
        frame.extend_from_slice(&captured.to_be_bytes());
        frame.extend_from_slice(&[0_u8, 1_u8]);
        assert_eq!(u64::from_be_bytes(frame[..8].try_into().unwrap()), sequence);
        assert_eq!(
            u64::from_be_bytes(frame[8..16].try_into().unwrap()),
            captured
        );
    }

    #[test]
    fn trailing_silence_uses_pcm_amplitude() {
        // Quiet samples close a phrase while ordinary speech-level samples keep it open.
        let silence = vec![0_u8; SILENCE_LOOKBACK_BYTES];
        let speech = [0x10_u8, 0x27_u8].repeat(SILENCE_LOOKBACK_BYTES / 2);

        assert!(trailing_audio_is_silent(&silence));
        assert!(!trailing_audio_is_silent(&speech));
    }
}
