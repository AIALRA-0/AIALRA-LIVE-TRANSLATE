//! Reliable PCM ingress shared by browsers, Android, and the DingTalk mini-app foreground probe.

use crate::app::AppState;
use crate::jobs::enqueue_asr;
use aialra_core_domain::SessionState;
use aialra_event_store::AudioChunkRecord;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, State};
use axum::response::Response;
use chrono::Utc;
use futures_util::{SinkExt, StreamExt};
use serde_json::json;

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
            enqueue_asr(state, session_id, source_id, captured_at_ms, &window)?;
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

/// Stopping a session seals every short tail so the final spoken phrase is not lost.
pub fn flush_session_buffers(state: &AppState, session_id: &str) -> anyhow::Result<usize> {
    let prefix = format!("{session_id}:");
    let drained = {
        let mut buffers = state
            .audio_buffers
            .lock()
            .map_err(|_| anyhow::anyhow!("audio buffer lock poisoned"))?;
        let keys = buffers
            .keys()
            .filter(|key| key.starts_with(&prefix))
            .cloned()
            .collect::<Vec<_>>();
        keys.into_iter()
            .filter_map(|key| buffers.remove(&key).map(|bytes| (key, bytes)))
            .collect::<Vec<_>>()
    };
    let captured_at_ms = Utc::now().timestamp_millis().max(0) as u64;
    let mut queued = 0;
    for (key, pcm) in drained {
        if pcm.is_empty() {
            continue;
        }
        let source_id = key.trim_start_matches(&prefix);
        enqueue_asr(state, session_id, source_id, captured_at_ms, &pcm)?;
        queued += 1;
    }
    Ok(queued)
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
