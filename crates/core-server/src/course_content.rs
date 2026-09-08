//! Small, owner-scoped document operations. Notes are append-only; audio stays private.
use crate::app::{ApiError, AppState};
use aialra_event_protocol::EventEnvelope;
use axum::{
    Json,
    extract::{Path, State},
    http::header,
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

fn pcm_wav(pcm: &[u8]) -> Vec<u8> {
    let size = pcm.len() as u32;
    let mut wav = Vec::with_capacity(pcm.len() + 44);
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
    wav.extend(pcm);
    wav
}

#[cfg(test)]
mod tests {
    use super::*;
    use aialra_event_store::{NewModelJob, NewSession};

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
}
