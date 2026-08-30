//! Reliable PCM ingress shared by browsers, Android, and the DingTalk mini-app foreground probe.

use crate::app::ApiError;
use crate::app::AppState;
use crate::jobs::enqueue_asr;
use crate::projects::hash_token;
use aialra_core_domain::SessionState;
use aialra_event_store::{AudioChunkRecord, AudioWindowRecord};
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::response::Response;
use chrono::Utc;
use futures_util::{SinkExt, StreamExt};
use serde_json::json;

const HEADER_BYTES: usize = 16;
const SAMPLE_RATE: u32 = 16_000;
const CHANNELS: u16 = 1;
const PCM_BYTES_PER_SECOND: usize = SAMPLE_RATE as usize * 2;
const MIN_ASR_WINDOW_BYTES: usize = PCM_BYTES_PER_SECOND * 3 / 2;
const MAX_ASR_WINDOW_BYTES: usize = PCM_BYTES_PER_SECOND * 5;
const SILENCE_LOOKBACK_BYTES: usize = PCM_BYTES_PER_SECOND * 450 / 1_000;
const SILENCE_MEAN_ABSOLUTE_PCM: i64 = 550;
const MAX_FRAME_BYTES: usize = PCM_BYTES_PER_SECOND * 3 + HEADER_BYTES;
const FIRST_AUDIO_SEQUENCE: u64 = 1;

pub async fn audio_websocket(
    State(state): State<AppState>,
    Path((session_id, source_id)): Path<(String, String)>,
    headers: HeaderMap,
    upgrade: WebSocketUpgrade,
) -> Result<Response, ApiError> {
    let lease_token = extract_lease_token(&headers)
        .ok_or_else(|| ApiError::unauthorized("recording lease is required"))?;
    let token_hash = hash_token(lease_token);
    state
        .store
        .validate_recording_lease(&session_id, &token_hash)?
        .ok_or_else(|| ApiError::conflict("recording lease expired or changed"))?;
    Ok(upgrade
        .protocols(["aialra.audio.v1"])
        .max_message_size(MAX_FRAME_BYTES)
        .on_upgrade(move |socket| handle_socket(state, session_id, source_id, token_hash, socket)))
}

async fn handle_socket(
    state: AppState,
    session_id: String,
    source_id: String,
    token_hash: String,
    socket: WebSocket,
) {
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
                if state
                    .store
                    .validate_recording_lease(&session_id, &token_hash)
                    .ok()
                    .flatten()
                    .is_none()
                {
                    let _ = sender.send(Message::Text(json!({"type": "audio.error", "message": "recording lease expired or changed"}).to_string().into())).await;
                    break;
                }
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

        let _ = state
            .audio_pending
            .send((session_id.to_owned(), source_id.to_owned()));
    }
    Ok((sequence, !inserted))
}

fn trailing_audio_is_silent(pcm: &[u8]) -> bool {
    // A 450 ms quiet tail closes a phrase after the 1.5 second minimum window.
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
    let mut queued = 0;
    for (pending_session, source_id) in state.store.list_audio_sources_with_pending_chunks()? {
        if pending_session == session_id {
            queued += assemble_source(state, session_id, &source_id, true)?;
        }
    }
    Ok(queued)
}

/// Startup replay rebuilds every acknowledged window that was not committed before a Core restart.
pub fn recover_audio_assembly(state: &AppState) -> anyhow::Result<usize> {
    let mut queued = 0;
    for (session_id, source_id) in state.store.list_audio_sources_with_pending_chunks()? {
        let seal_tail = state
            .store
            .get_session(&session_id)?
            .map(|session| {
                !matches!(
                    session.state,
                    SessionState::Recording | SessionState::Degraded
                )
            })
            .unwrap_or(false);
        queued += assemble_source(state, &session_id, &source_id, seal_tail)?;
    }
    Ok(queued)
}

/// A single background assembler keeps model work off the ACK request path.
pub async fn run_audio_assembler(state: AppState) {
    let mut receiver = state.audio_pending.subscribe();
    loop {
        match receiver.recv().await {
            Ok((session_id, source_id)) => {
                let worker_state = state.clone();
                let result = tokio::task::spawn_blocking(move || {
                    assemble_source(&worker_state, &session_id, &source_id, false)
                })
                .await;
                if let Err(error) = result {
                    tracing::warn!(error_kind = "audio_assembler_join_failed", error = %error, "audio assembler task failed");
                } else if let Ok(Err(error)) = result {
                    tracing::warn!(error_kind = "audio_assembler_failed", error = %error, "audio assembler will retry from durable chunks");
                }
            }
            Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                tracing::warn!(
                    skipped,
                    "audio assembler notification lagged; durable startup scan remains available"
                );
                let worker_state = state.clone();
                let _ = tokio::task::spawn_blocking(move || recover_audio_assembly(&worker_state))
                    .await;
            }
            Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
        }
    }
}

fn assemble_source(
    state: &AppState,
    session_id: &str,
    source_id: &str,
    seal_tail: bool,
) -> anyhow::Result<usize> {
    let chunks = state
        .store
        .list_unassembled_audio_chunks(session_id, source_id)?;
    if chunks.is_empty() {
        return Ok(0);
    }
    let mut expected = state
        .store
        .audio_assembly_cursor(session_id, source_id)?
        .map(|value| value.saturating_add(1))
        .unwrap_or(FIRST_AUDIO_SEQUENCE);
    let mut index = 0_usize;
    let mut queued = 0_usize;
    while index < chunks.len() {
        if chunks[index].sequence != expected {
            break;
        }
        let first_sequence = chunks[index].sequence;
        let captured_at_ms = chunks[index].captured_at_ms;
        let mut last_sequence = first_sequence;
        let mut pcm = Vec::new();
        let mut closed = false;
        while index < chunks.len() {
            let chunk = &chunks[index];
            if chunk.sequence != expected {
                break;
            }
            pcm.extend_from_slice(&state.objects.read(&chunk.object_hash)?);
            last_sequence = chunk.sequence;
            expected = expected.saturating_add(1);
            index += 1;
            let pause_after_minimum =
                pcm.len() >= MIN_ASR_WINDOW_BYTES && trailing_audio_is_silent(&pcm);
            if pause_after_minimum || pcm.len() >= MAX_ASR_WINDOW_BYTES {
                closed = true;
                break;
            }
        }
        if !(closed || seal_tail && index == chunks.len()) {
            break;
        }
        let stored = state.objects.put(&pcm)?;
        enqueue_asr(state, session_id, source_id, captured_at_ms, &pcm)?;
        state
            .store
            .record_audio_window_and_advance(&AudioWindowRecord {
                id: format!("window_{session_id}_{source_id}_{first_sequence}_{last_sequence}"),
                session_id: session_id.to_owned(),
                source_id: source_id.to_owned(),
                first_sequence,
                last_sequence,
                captured_at_ms,
                duration_ms: ((pcm.len() as u64 * 1_000) / PCM_BYTES_PER_SECOND as u64) as u32,
                object_hash: stored.hash,
                created_at: Utc::now(),
            })?;
        queued += 1;
    }
    Ok(queued)
}

fn extract_lease_token(headers: &HeaderMap) -> Option<&str> {
    headers
        .get("sec-websocket-protocol")?
        .to_str()
        .ok()?
        .split(',')
        .map(str::trim)
        .find_map(|value| value.strip_prefix("lease."))
        .filter(|value| !value.is_empty() && value.len() <= 128)
}

#[cfg(test)]
mod tests {
    use super::{SILENCE_LOOKBACK_BYTES, assemble_source, trailing_audio_is_silent};
    use crate::app::AppState;
    use crate::jobs::enqueue_asr;
    use aialra_event_store::{AudioChunkRecord, NewSession};
    use chrono::Utc;

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

    #[test]
    fn assembler_waits_for_a_missing_initial_frame() {
        let temp = tempfile::tempdir().unwrap();
        let state = AppState::open(temp.path()).unwrap();
        state
            .store
            .create_session(&NewSession {
                id: "session_out_of_order".to_owned(),
                title: "Synthetic course".to_owned(),
                source_language: "en".to_owned(),
                target_language: "zh-CN".to_owned(),
                privacy_mode: "local_only".to_owned(),
                consent_confirmed: true,
                demo_mode: false,
            })
            .unwrap();
        let pcm = vec![0_u8; 32_000];
        let stored = state.objects.put(&pcm).unwrap();
        let frame = |sequence| AudioChunkRecord {
            session_id: "session_out_of_order".to_owned(),
            source_id: "browser-mic-g1".to_owned(),
            sequence,
            captured_at_ms: sequence * 1_000,
            sample_rate: 16_000,
            channels: 1,
            encoding: "pcm_s16le".to_owned(),
            duration_ms: 1_000,
            object_hash: stored.hash.clone(),
            size_bytes: stored.size_bytes,
            acknowledged_at: Utc::now(),
        };

        state.store.insert_audio_chunk(&frame(2)).unwrap();
        assert_eq!(
            assemble_source(&state, "session_out_of_order", "browser-mic-g1", false).unwrap(),
            0
        );
        assert_eq!(
            state
                .store
                .audio_assembly_cursor("session_out_of_order", "browser-mic-g1")
                .unwrap(),
            None
        );

        state.store.insert_audio_chunk(&frame(1)).unwrap();
        assert_eq!(
            assemble_source(&state, "session_out_of_order", "browser-mic-g1", false).unwrap(),
            1
        );
        assert_eq!(
            state
                .store
                .audio_assembly_cursor("session_out_of_order", "browser-mic-g1")
                .unwrap(),
            Some(2)
        );
    }

    #[test]
    fn repeated_audio_at_different_times_creates_distinct_asr_jobs() {
        let temp = tempfile::tempdir().unwrap();
        let state = AppState::open(temp.path()).unwrap();
        state
            .store
            .create_session(&NewSession {
                id: "session_repeated_audio".to_owned(),
                title: "Synthetic repeated course".to_owned(),
                source_language: "en".to_owned(),
                target_language: "zh-CN".to_owned(),
                privacy_mode: "local_only".to_owned(),
                consent_confirmed: true,
                demo_mode: false,
            })
            .unwrap();
        let pcm = vec![7_u8; 64_000];
        enqueue_asr(&state, "session_repeated_audio", "network-g1", 1_000, &pcm).unwrap();
        enqueue_asr(&state, "session_repeated_audio", "network-g1", 9_000, &pcm).unwrap();
        enqueue_asr(&state, "session_repeated_audio", "network-g1", 1_000, &pcm).unwrap();
        assert_eq!(
            state
                .store
                .model_queue_counts(Some("session_repeated_audio"))
                .unwrap()
                .queued,
            2
        );
    }
}
