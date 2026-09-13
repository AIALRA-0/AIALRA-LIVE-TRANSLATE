//! Small, owner-scoped document operations. Notes are append-only; audio stays private.
use crate::app::{ApiError, AppState};
use aialra_event_protocol::EventEnvelope;
use aialra_event_store::AudioChunkRecord;
use axum::{
    Json,
    body::Body,
    extract::{Path, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Deserialize)]
pub struct SaveNote {
    pub text: String,
    pub base_revision: u64,
}

pub async fn get_note(
    State(state): State<AppState>,
    Path(session): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let note = state.store.latest_user_note(&session)?;
    Ok(Json(
        json!({"revision": note.as_ref().map_or(0, |n| n.sequence), "text": note.as_ref().and_then(|n| n.payload.get("text")).and_then(Value::as_str).unwrap_or("")}),
    ))
}

pub async fn save_note(
    State(state): State<AppState>,
    Path(session): Path<String>,
    Json(request): Json<SaveNote>,
) -> Result<Json<Value>, ApiError> {
    if request.text.len() > 64 * 1024 {
        return Err(ApiError::bad_request("笔记不能超过 64 KiB，请分节整理"));
    }
    // All Core event writers share this lock. Never read a revision and append
    // outside it: a second tab must receive a conflict, not overwrite the first.
    let event = {
        let _guard = state
            .sequence_lock
            .lock()
            .map_err(|_| anyhow::anyhow!("note lock unavailable"))?;
        let current = state.store.latest_user_note(&session)?;
        let revision = current.as_ref().map_or(0, |event| event.sequence);
        if current
            .as_ref()
            .and_then(|n| n.payload.get("text"))
            .and_then(Value::as_str)
            == Some(request.text.as_str())
        {
            return Ok(Json(json!({"revision": revision, "text": request.text})));
        }
        if revision != request.base_revision {
            return Err(ApiError::conflict_with_code(
                "笔记已在另一页面更新，本页草稿已保留，请先读取最新版本",
                "note_revision_conflict",
            ));
        }
        let event = EventEnvelope::new(
            &session,
            "user_notes",
            revision + 1,
            "user.note.saved",
            0,
            "user_notes",
            None,
            json!({"text": request.text}),
        )
        .map_err(anyhow::Error::from)?;
        state.store.insert_event(&event)?;
        event
    };
    let _ = state.events.send(event.clone());
    Ok(Json(
        json!({"revision": event.sequence, "text": request.text}),
    ))
}

pub async fn paragraph_audio(
    State(state): State<AppState>,
    Path((session, paragraph)): Path<(String, String)>,
) -> Result<Response, ApiError> {
    // Session ownership is checked by the same middleware as event replay.
    // Resolve evidence inside this session; never accept an object hash from a client.
    let event = state
        .store
        .document_event(&session, &paragraph)?
        .ok_or_else(|| ApiError::not_found("段落不存在"))?;
    let ids = event
        .payload
        .get("segment_ids")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_else(|| vec![Value::String(paragraph)]);
    if ids.len() > 32 {
        return Err(ApiError::bad_request("段落过长，请选择较短片段"));
    }
    let mut pcm = Vec::new();
    for id in ids {
        let segment = state
            .store
            .document_event(&session, id.as_str().unwrap_or_default())?
            .ok_or_else(|| ApiError::not_found("音频证据不可用"))?;
        let job = state
            .store
            .get_model_job(&segment.correlation_id)?
            .filter(|j| j.session_id == session && j.job_type == "asr")
            .ok_or_else(|| ApiError::not_found("这段历史没有可播放音频"))?;
        let hash = job
            .input_object_hash
            .ok_or_else(|| ApiError::not_found("这段历史没有可播放音频"))?;
        let bytes = state.objects.read(&hash)?;
        if pcm.len() + bytes.len() > 16_000 * 2 * 120 {
            return Err(ApiError::bad_request("单次回放最长两分钟"));
        }
        pcm.extend(bytes);
    }
    if pcm.is_empty() || pcm.len() % 2 != 0 {
        return Err(ApiError::not_found("没有完整音频可播放"));
    }
    let wav = pcm_wav(&pcm);
    Ok((
        [
            (header::CONTENT_TYPE, "audio/wav"),
            (header::CACHE_CONTROL, "no-store"),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
        ],
        wav,
    )
        .into_response())
}

struct PlaybackChunk {
    chunk: AudioChunkRecord,
    offset: u64,
}

fn playback_chunks(state: &AppState, session: &str) -> Result<(Vec<PlaybackChunk>, u64), ApiError> {
    state
        .store
        .get_session(session)?
        .ok_or_else(|| ApiError::not_found("课程不存在"))?;
    let chunks = state.store.list_session_audio_chunks(session)?;
    if chunks.is_empty() {
        return Err(ApiError::not_found("这节课程没有可回放的录音"));
    }
    let mut offset = 0u64;
    let mut entries = Vec::with_capacity(chunks.len());
    for chunk in chunks {
        if chunk.sample_rate != 16_000
            || chunk.channels != 1
            || chunk.encoding != "pcm_s16le"
            || chunk.size_bytes == 0
            || chunk.size_bytes % 2 != 0
        {
            return Err(ApiError::bad_request("这段历史录音的格式尚不能整课回放"));
        }
        entries.push(PlaybackChunk {
            chunk: chunk.clone(),
            offset,
        });
        offset = offset
            .checked_add(chunk.size_bytes)
            .ok_or_else(|| ApiError::bad_request("课程录音长度超出回放范围"))?;
    }
    if offset > (u32::MAX - 36) as u64 {
        return Err(ApiError::bad_request("课程录音长度超出回放范围"));
    }
    Ok((entries, offset))
}

/// The index contains playback offsets and wall-clock capture positions, not
/// object hashes or device identifiers. Gaps between recording runs stay visible.
pub async fn session_audio_index(
    State(state): State<AppState>,
    Path(session): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let (entries, bytes) = playback_chunks(&state, &session)?;
    let positions = entries
        .iter()
        .map(|entry| {
            json!({
                "captured_at_ms": entry.chunk.captured_at_ms,
                "duration_ms": entry.chunk.size_bytes * 1000 / 32_000,
                "playback_start_ms": entry.offset * 1000 / 32_000,
                "playback_end_ms": (entry.offset + entry.chunk.size_bytes) * 1000 / 32_000,
            })
        })
        .collect::<Vec<_>>();
    Ok(Json(
        json!({"duration_ms": bytes * 1000 / 32_000, "positions": positions}),
    ))
}

const MAX_PLAYBACK_RANGE: u64 = 1024 * 1024;

fn playback_range(request: Option<&HeaderValue>, total: u64) -> Result<(u64, u64, bool), ApiError> {
    let Some(value) = request else {
        return Ok((
            0,
            total.min(MAX_PLAYBACK_RANGE) - 1,
            total > MAX_PLAYBACK_RANGE,
        ));
    };
    let value = value
        .to_str()
        .map_err(|_| ApiError::bad_request("音频定位范围无效"))?;
    let part = value
        .strip_prefix("bytes=")
        .ok_or_else(|| ApiError::bad_request("音频定位范围无效"))?;
    let (start, end) = part
        .split_once('-')
        .ok_or_else(|| ApiError::bad_request("音频定位范围无效"))?;
    if part.contains(',') {
        return Err(ApiError::bad_request("一次只能读取一段音频"));
    }
    let (start, end) = if start.is_empty() {
        let count = end
            .parse::<u64>()
            .map_err(|_| ApiError::bad_request("音频定位范围无效"))?;
        if count == 0 {
            return Err(ApiError::bad_request("音频定位范围无效"));
        }
        (total.saturating_sub(count), total - 1)
    } else {
        let start = start
            .parse::<u64>()
            .map_err(|_| ApiError::bad_request("音频定位范围无效"))?;
        let end = if end.is_empty() {
            total - 1
        } else {
            end.parse::<u64>()
                .map_err(|_| ApiError::bad_request("音频定位范围无效"))?
        };
        (start, end)
    };
    if start >= total || end < start {
        return Err(ApiError::bad_request("音频定位范围无效"));
    }
    Ok((
        start,
        end.min(total - 1).min(start + MAX_PLAYBACK_RANGE - 1),
        true,
    ))
}

/// Serve a bounded byte range of a virtual WAV. A multi-hour course never
/// becomes one in-memory audio file on either the server or the browser.
pub async fn session_audio(
    State(state): State<AppState>,
    Path(session): Path<String>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let (entries, pcm_size) = playback_chunks(&state, &session)?;
    let total = pcm_size + 44;
    let (start, end, partial) = playback_range(headers.get(header::RANGE), total)?;
    let mut body = Vec::with_capacity((end - start + 1) as usize);
    let wav_header = pcm_wav_header(pcm_size as u32);
    if start < 44 {
        body.extend_from_slice(&wav_header[start as usize..=(end.min(43)) as usize]);
    }
    for entry in &entries {
        let chunk_start = 44 + entry.offset;
        let chunk_end = chunk_start + entry.chunk.size_bytes - 1;
        if chunk_start > end || chunk_end < start {
            continue;
        }
        let bytes = state.objects.read(&entry.chunk.object_hash)?;
        if bytes.len() as u64 != entry.chunk.size_bytes {
            return Err(ApiError::bad_request("这段录音未能完整读取"));
        }
        let from = start.max(chunk_start) - chunk_start;
        let until = end.min(chunk_end) - chunk_start + 1;
        body.extend_from_slice(&bytes[from as usize..until as usize]);
    }
    if body.len() as u64 != end - start + 1 {
        return Err(ApiError::bad_request("这段录音未能完整读取"));
    }
    let mut response = Response::new(Body::from(body));
    *response.status_mut() = if partial {
        StatusCode::PARTIAL_CONTENT
    } else {
        StatusCode::OK
    };
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static("audio/wav"));
    response.headers_mut().insert(
        header::CONTENT_LENGTH,
        HeaderValue::from_str(&(end - start + 1).to_string())
            .map_err(|_| ApiError::bad_request("音频定位范围无效"))?,
    );
    response
        .headers_mut()
        .insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response.headers_mut().insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    if partial {
        response.headers_mut().insert(
            header::CONTENT_RANGE,
            HeaderValue::from_str(&format!("bytes {start}-{end}/{total}"))
                .map_err(|_| ApiError::bad_request("音频定位范围无效"))?,
        );
    }
    Ok(response)
}

fn pcm_wav_header(size: u32) -> Vec<u8> {
    let mut wav = Vec::with_capacity(44);
    wav.extend(b"RIFF");
    wav.extend((size + 36).to_le_bytes());
    wav.extend(b"WAVEfmt ");
    wav.extend(16u32.to_le_bytes());
    wav.extend(1u16.to_le_bytes());
    wav.extend(1u16.to_le_bytes());
    wav.extend(16_000u32.to_le_bytes());
    wav.extend(32_000u32.to_le_bytes());
    wav.extend(2u16.to_le_bytes());
    wav.extend(16u16.to_le_bytes());
    wav.extend(b"data");
    wav.extend(size.to_le_bytes());
    wav
}

fn pcm_wav(pcm: &[u8]) -> Vec<u8> {
    let mut wav = pcm_wav_header(pcm.len() as u32);
    wav.extend(pcm);
    wav
}

#[cfg(test)]
mod tests {
    use super::*;
    use aialra_event_store::{NewModelJob, NewSession};
    use chrono::Utc;

    fn test_state(path: &std::path::Path) -> AppState {
        let state = AppState::open(path).unwrap();
        state
            .store
            .create_session(&NewSession {
                id: "session_content_test".into(),
                title: "Synthetic content".into(),
                source_language: "en".into(),
                target_language: "zh-CN".into(),
                privacy_mode: "local_only".into(),
                consent_confirmed: true,
                demo_mode: false,
            })
            .unwrap();
        state
    }

    #[tokio::test]
    async fn notes_append_history_and_reject_stale_writers() {
        let dir = tempfile::tempdir().unwrap();
        let state = test_state(dir.path());
        let session = "session_content_test".to_owned();
        let _ = save_note(
            State(state.clone()),
            Path(session.clone()),
            Json(SaveNote {
                text: "first".into(),
                base_revision: 0,
            }),
        )
        .await
        .unwrap();
        assert!(
            save_note(
                State(state.clone()),
                Path(session.clone()),
                Json(SaveNote {
                    text: "stale".into(),
                    base_revision: 0
                })
            )
            .await
            .is_err()
        );
        let _ = save_note(
            State(state.clone()),
            Path(session.clone()),
            Json(SaveNote {
                text: "second".into(),
                base_revision: 1,
            }),
        )
        .await
        .unwrap();
        // A network retry does not create a third event.
        let _ = save_note(
            State(state.clone()),
            Path(session.clone()),
            Json(SaveNote {
                text: "second".into(),
                base_revision: 1,
            }),
        )
        .await
        .unwrap();
        assert_eq!(state.store.list_events(&session).unwrap().len(), 2);
        assert_eq!(
            state
                .store
                .latest_user_note(&session)
                .unwrap()
                .unwrap()
                .sequence,
            2
        );
    }

    #[tokio::test]
    async fn playback_resolves_only_session_owned_evidence() {
        let dir = tempfile::tempdir().unwrap();
        let state = test_state(dir.path());
        let pcm = state.objects.put(&[0, 0, 1, 0]).unwrap();
        state
            .store
            .enqueue_model_job(&NewModelJob {
                id: "job_clip_test".into(),
                session_id: "session_content_test".into(),
                job_type: "asr".into(),
                priority: 100,
                input: json!({}),
                input_object_hash: Some(pcm.hash),
                idempotency_key: "clip-test".into(),
            })
            .unwrap();
        state
            .emit(
                "session_content_test",
                "gpu_asr",
                "segment.finalized",
                0,
                "job_clip_test",
                None,
                json!({"segment_id":"seg_clip_test","text":"synthetic"}),
            )
            .unwrap();
        let response = paragraph_audio(
            State(state.clone()),
            Path(("session_content_test".into(), "seg_clip_test".into())),
        )
        .await
        .unwrap();
        assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
        assert_eq!(
            axum::body::to_bytes(response.into_body(), 1024)
                .await
                .unwrap()
                .len(),
            48
        );
        assert!(
            paragraph_audio(
                State(state),
                Path(("session_other".into(), "seg_clip_test".into()))
            )
            .await
            .is_err()
        );
    }
    #[test]
    fn playback_is_mono_pcm_with_exact_payload() {
        let wav = pcm_wav(&[0, 0, 1, 0]);
        assert_eq!(wav.len(), 48);
        assert_eq!(&wav[44..], &[0, 0, 1, 0]);
        assert_eq!(&wav[..4], b"RIFF");
    }

    #[test]
    fn multi_hour_playback_is_bounded_per_http_range() {
        let total = 170 * 1024 * 1024;
        let (start, end, partial) = playback_range(None, total).unwrap();
        assert_eq!((start, end, partial), (0, MAX_PLAYBACK_RANGE - 1, true));
        let range = HeaderValue::from_static("bytes=100000000-");
        let (start, end, partial) = playback_range(Some(&range), total).unwrap();
        assert_eq!(start, 100_000_000);
        assert_eq!(end - start + 1, MAX_PLAYBACK_RANGE);
        assert!(partial);
    }

    #[tokio::test]
    async fn whole_course_playback_seeks_across_chunks_without_loading_the_whole_recording() {
        let dir = tempfile::tempdir().unwrap();
        let state = test_state(dir.path());
        for (sequence, captured_at_ms, fill) in [(0, 1_000, 1u8), (1, 20_000, 2u8)] {
            let pcm = vec![fill; 3_200];
            let object = state.objects.put(&pcm).unwrap();
            state
                .store
                .insert_audio_chunk(&AudioChunkRecord {
                    session_id: "session_content_test".into(),
                    source_id: "synthetic".into(),
                    sequence,
                    captured_at_ms,
                    sample_rate: 16_000,
                    channels: 1,
                    encoding: "pcm_s16le".into(),
                    duration_ms: 100,
                    object_hash: object.hash,
                    size_bytes: pcm.len() as u64,
                    acknowledged_at: Utc::now(),
                })
                .unwrap();
        }
        let Json(index) =
            session_audio_index(State(state.clone()), Path("session_content_test".into()))
                .await
                .unwrap();
        assert_eq!(index["duration_ms"], 200);
        assert_eq!(index["positions"][1]["playback_start_ms"], 100);
        assert_eq!(index["positions"][1]["captured_at_ms"], 20_000);

        let mut headers = HeaderMap::new();
        headers.insert(header::RANGE, HeaderValue::from_static("bytes=3240-3247"));
        let response = session_audio(State(state), Path("session_content_test".into()), headers)
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(
            response.headers()[header::CONTENT_RANGE],
            "bytes 3240-3247/6444"
        );
        let body = axum::body::to_bytes(response.into_body(), 1024)
            .await
            .unwrap();
        assert_eq!(&body[..], &[1, 1, 1, 1, 2, 2, 2, 2]);
    }
}
