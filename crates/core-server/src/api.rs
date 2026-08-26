//! Versioned HTTP and Server-Sent Events endpoints for the desktop UI.

use crate::app::{ApiError, AppState};
use crate::explanation::create_explanation;
use aialra_core_domain::SessionState;
use aialra_event_store::{AssetPageRecord, AssetRecord, NewSession, SessionRecord};
use axum::body::Body;
use axum::extract::{Multipart, Path, State};
use axum::http::{HeaderValue, header};
use axum::response::Response;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::{Json, response::IntoResponse};
use chrono::Utc;
use serde::Deserialize;
use serde_json::{Value, json};
use std::convert::Infallible;
use std::sync::atomic::Ordering;
use std::time::Duration;
use uuid::Uuid;

const MAX_ASSET_BYTES: usize = 50 * 1024 * 1024;

pub async fn health(State(state): State<AppState>) -> impl IntoResponse {
    // Worker failure degrades model capabilities while the durable core remains healthy.
    let worker = state.worker.health().await.ok();
    Json(json!({
        "status": "ok",
        "service": "aialra-core",
        "version": env!("CARGO_PKG_VERSION"),
        "worker": worker,
        "local_only_default": true,
        "deployment_mode": std::env::var("AIALRA_DEPLOYMENT_MODE").unwrap_or_else(|_| "local".to_owned()),
        "processing_location": std::env::var("AIALRA_PROCESSING_LOCATION").unwrap_or_else(|_| "本机处理".to_owned())
    }))
}

#[derive(Debug, Deserialize)]
pub struct CreateSessionRequest {
    pub title: String,
    pub source_language: String,
    pub target_language: String,
    pub consent_confirmed: bool,
    #[serde(default)]
    pub demo_mode: bool,
}

pub async fn create_session(
    State(state): State<AppState>,
    Json(request): Json<CreateSessionRequest>,
) -> Result<Json<SessionRecord>, ApiError> {
    if request.title.trim().is_empty() {
        return Err(ApiError::bad_request("session title is required"));
    }
    if !request.consent_confirmed && !request.demo_mode {
        return Err(ApiError::bad_request(
            "recording consent is required outside demo mode",
        ));
    }

    // UUIDv7 IDs are stable across audio, DingTalk businessOrder, assets, and exports.
    let session_id = format!("session_{}", Uuid::now_v7().simple());
    let session = NewSession {
        id: session_id.clone(),
        title: request.title.trim().to_owned(),
        source_language: request.source_language,
        target_language: request.target_language,
        privacy_mode: "local_only".to_owned(),
        consent_confirmed: request.consent_confirmed,
        demo_mode: request.demo_mode,
    };
    state.store.create_session(&session)?;
    let correlation = format!("create_{}", Uuid::now_v7().simple());
    state.emit(
        &session_id,
        "core",
        "consent.recorded",
        0,
        &correlation,
        None,
        json!({
            "confirmed": request.consent_confirmed,
            "demo_mode": request.demo_mode,
            "privacy_mode": "local_only"
        }),
    )?;
    state.emit(
        &session_id,
        "core",
        "session.created",
        0,
        &correlation,
        None,
        json!({"title": session.title}),
    )?;
    let ready = state
        .store
        .transition_session(&session_id, SessionState::Ready)?;
    state.emit(
        &session_id,
        "core",
        "session.ready",
        0,
        &correlation,
        None,
        json!({}),
    )?;
    Ok(Json(ready))
}

pub async fn list_sessions(
    State(state): State<AppState>,
) -> Result<Json<Vec<SessionRecord>>, ApiError> {
    Ok(Json(state.store.list_sessions()?))
}

pub async fn get_session(
    State(state): State<AppState>,
    Path(session_id): Path<String>,
) -> Result<Json<SessionRecord>, ApiError> {
    state
        .store
        .get_session(&session_id)?
        .map(Json)
        .ok_or_else(|| ApiError::not_found("session not found"))
}

pub async fn start_session(
    State(state): State<AppState>,
    Path(session_id): Path<String>,
) -> Result<Json<SessionRecord>, ApiError> {
    let record = state
        .store
        .transition_session(&session_id, SessionState::Recording)?;
    state.emit(
        &session_id,
        "core",
        "session.recording.started",
        0,
        &format!("start_{}", Uuid::now_v7().simple()),
        None,
        json!({"visible_recording_required": true}),
    )?;
    Ok(Json(record))
}

pub async fn stop_session(
    State(state): State<AppState>,
    Path(session_id): Path<String>,
) -> Result<Json<SessionRecord>, ApiError> {
    let correlation = format!("stop_{}", Uuid::now_v7().simple());
    let stopping = state
        .store
        .transition_session(&session_id, SessionState::Stopping)?;
    state.emit(
        &session_id,
        "core",
        "session.stopping",
        0,
        &correlation,
        None,
        json!({}),
    )?;
    // Recording control returns immediately while a background task drains every accepted ASR window.
    tokio::spawn(async move {
        loop {
            let drained = state.model_drained.notified();
            if state.pending_model_jobs.load(Ordering::SeqCst) == 0 {
                break;
            }
            drained.await;
        }
        if state
            .store
            .transition_session(&session_id, SessionState::Completed)
            .is_ok()
        {
            let _ = state.emit(
                &session_id,
                "core",
                "session.completed",
                0,
                &correlation,
                None,
                json!({"model_queue_drained": true}),
            );
        }
    });
    Ok(Json(stopping))
}

pub async fn list_events(
    State(state): State<AppState>,
    Path(session_id): Path<String>,
) -> Result<Json<Vec<aialra_event_protocol::EventEnvelope>>, ApiError> {
    Ok(Json(state.store.list_events(&session_id)?))
}

pub async fn dingtalk_capabilities(
    State(state): State<AppState>,
    Path(session_id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    state
        .store
        .get_session(&session_id)?
        .ok_or_else(|| ApiError::not_found("session not found"))?;
    Ok(Json(json!({
        "configured": state.dingtalk.configured(),
        "a1_recording_control": true,
        "post_recording_import": true,
        "incremental_pcm_verified": false,
        "incremental_transcript_verified": false,
        "foreground_miniapp_probe": true
    })))
}

pub async fn start_dingtalk_recording(
    State(state): State<AppState>,
    Path(session_id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let session = state
        .store
        .get_session(&session_id)?
        .ok_or_else(|| ApiError::not_found("session not found"))?;
    if !session.consent_confirmed {
        return Err(ApiError::bad_request(
            "recording consent is required before starting DingTalk A1",
        ));
    }
    if !state.dingtalk.configured() {
        return Err(ApiError::unavailable(
            "DingTalk credentials and operator identifiers are not configured",
        ));
    }
    let response = state
        .dingtalk
        .start_recording(&session_id)
        .await
        .map_err(ApiError::upstream)?;
    let event = state.emit(
        &session_id,
        "dingtalk_a1",
        "dingtalk.recording.started",
        0,
        &format!("dingtalk_start_{}", Uuid::now_v7().simple()),
        None,
        json!({
            "business_order": session_id,
            "realtime_pcm_verified": false,
            "provider_response": response
        }),
    )?;
    Ok(Json(json!({"event": event})))
}

pub async fn stop_dingtalk_recording(
    State(state): State<AppState>,
    Path(session_id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    state
        .store
        .get_session(&session_id)?
        .ok_or_else(|| ApiError::not_found("session not found"))?;
    if !state.dingtalk.configured() {
        return Err(ApiError::unavailable(
            "DingTalk credentials and operator identifiers are not configured",
        ));
    }
    let response = state
        .dingtalk
        .stop_recording(&session_id)
        .await
        .map_err(ApiError::upstream)?;
    let event = state.emit(
        &session_id,
        "dingtalk_a1",
        "dingtalk.recording.stopped",
        0,
        &format!("dingtalk_stop_{}", Uuid::now_v7().simple()),
        None,
        json!({"business_order": session_id, "provider_response": response}),
    )?;
    Ok(Json(json!({"event": event})))
}

pub async fn stream_events(
    State(state): State<AppState>,
    Path(session_id): Path<String>,
) -> Result<Sse<impl futures_util::Stream<Item = Result<Event, Infallible>>>, ApiError> {
    let history = state.store.list_events(&session_id)?;
    let mut receiver = state.events.subscribe();
    let stream = async_stream::stream! {
        // Replay precedes live events so a refreshed UI reconstructs the same timeline.
        for envelope in history {
            let data = serde_json::to_string(&envelope).unwrap_or_else(|_| "{}".to_owned());
            yield Ok(Event::default().id(envelope.event_id.to_string()).data(data));
        }
        loop {
            match receiver.recv().await {
                Ok(envelope) if envelope.session_id == session_id => {
                    let data = serde_json::to_string(&envelope).unwrap_or_else(|_| "{}".to_owned());
                    yield Ok(Event::default().id(envelope.event_id.to_string()).data(data));
                }
                Ok(_) => {}
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                    // The browser reconnects and receives persisted history when it falls behind.
                    break;
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    };
    Ok(Sse::new(stream).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(10))
            .text("heartbeat"),
    ))
}

pub async fn run_demo(
    State(state): State<AppState>,
    Path(session_id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let session = state
        .store
        .get_session(&session_id)?
        .ok_or_else(|| ApiError::not_found("session not found"))?;
    if !session.demo_mode {
        return Err(ApiError::bad_request("mock pipeline requires demo mode"));
    }
    if session.state == SessionState::Ready {
        state
            .store
            .transition_session(&session_id, SessionState::Recording)?;
    }

    // The deterministic script exercises partial, final, translation, explanation, and replay paths.
    let demo_session_id = session_id.clone();
    tokio::spawn(async move {
        let correlation = format!("demo_{}", Uuid::now_v7().simple());
        let segments = [
            (
                "seg_demo_1",
                "A pipeline hazard happens when overlapping instructions depend on the same resource.",
                "当重叠执行的指令依赖同一资源时，就会出现流水线冒险。",
            ),
            (
                "seg_demo_2",
                "Forwarding can provide a result before it is written back to the register file.",
                "数据前递可以在结果写回寄存器堆之前把它提供给后续指令。",
            ),
        ];
        for (index, (segment_id, text, translation)) in segments.into_iter().enumerate() {
            let monotonic = (index as u64 + 1) * 2_000_000_000;
            let partial = state.emit(
                &demo_session_id,
                "mock_asr",
                "asr.partial.updated",
                monotonic,
                &correlation,
                None,
                json!({"text": text.split('.').next().unwrap_or(text), "segment_id": segment_id}),
            );
            if partial.is_err() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(220)).await;
            let finalized = match state.emit(
                &demo_session_id,
                "mock_asr",
                "segment.finalized",
                monotonic,
                &correlation,
                partial.ok().map(|event| event.event_id.to_string()),
                json!({"segment_id": segment_id, "text": text, "start_ms": index * 2000, "end_ms": (index + 1) * 2000, "confidence": 0.99}),
            ) {
                Ok(event) => event,
                Err(_) => return,
            };
            tokio::time::sleep(Duration::from_millis(180)).await;
            let _ = state.emit(
                &demo_session_id,
                "mock_translation",
                "translation.finalized",
                monotonic,
                &correlation,
                Some(finalized.event_id.to_string()),
                json!({"segment_id": segment_id, "translation_id": format!("tr_{segment_id}"), "text": translation, "provider": "deterministic_mock"}),
            );
            tokio::time::sleep(Duration::from_millis(180)).await;
        }
        let _ = state.emit(
            &demo_session_id,
            "mock_explainer",
            "explanation.card.created",
            4_000_000_000,
            &correlation,
            None,
            json!({
                "card_id": "card_demo_1",
                "summary": "本段说明流水线冒险以及数据前递如何降低等待时间。",
                "rare_terms": [{
                    "term": "forwarding",
                    "one_line": "数据前递把尚未写回的计算结果直接送到需要它的后续流水级，从而减少停顿。",
                    "evidence_segment_ids": ["seg_demo_2"],
                    "asset_page_ids": []
                }],
                "review_questions": ["数据前递为什么无法解决所有流水线冒险？"],
                "evidence_segment_ids": ["seg_demo_1", "seg_demo_2"],
                "asset_page_ids": [],
                "fact_type": "background_explanation",
                "provider": "deterministic_mock",
                "confidence": 0.96
            }),
        );
    });
    Ok(Json(json!({"started": true, "session_id": session_id})))
}

pub async fn upload_asset(
    State(state): State<AppState>,
    Path(session_id): Path<String>,
    mut multipart: Multipart,
) -> Result<Json<Value>, ApiError> {
    state
        .store
        .get_session(&session_id)?
        .ok_or_else(|| ApiError::not_found("session not found"))?;
    let field = multipart
        .next_field()
        .await?
        .ok_or_else(|| ApiError::bad_request("asset file is required"))?;
    let file_name = field.file_name().unwrap_or("asset.bin").to_owned();
    let media_type = field
        .content_type()
        .unwrap_or("application/octet-stream")
        .to_owned();
    let bytes = field.bytes().await?;
    if bytes.len() > MAX_ASSET_BYTES {
        return Err(ApiError::bad_request(
            "asset exceeds 50 MiB bootstrap limit",
        ));
    }
    let stored = state.objects.put(&bytes)?;
    let asset_id = format!("asset_{}", Uuid::now_v7().simple());
    let correlation = format!("asset_{}", Uuid::now_v7().simple());
    state.store.insert_asset(&AssetRecord {
        id: asset_id.clone(),
        session_id: session_id.clone(),
        original_name: sanitize_file_name(&file_name),
        media_type: media_type.clone(),
        object_hash: stored.hash.clone(),
        size_bytes: stored.size_bytes,
        status: "stored".to_owned(),
        created_at: Utc::now(),
    })?;
    state.emit(
        &session_id,
        "asset_ingest",
        "asset.ingested",
        0,
        &correlation,
        None,
        json!({"asset_id": asset_id, "media_type": media_type, "object_hash": stored.hash, "size_bytes": stored.size_bytes}),
    )?;

    // Parsing is local and progressive; failure preserves the immutable original for retry.
    let parsed = state
        .worker
        .parse_asset(&file_name, &media_type, bytes.to_vec())
        .await?;
    let mut page_ids = Vec::new();
    for page in parsed.pages {
        let page_id = format!("page_{}_{}", asset_id, page.page_number);
        state.store.insert_asset_page(&AssetPageRecord {
            id: page_id.clone(),
            asset_id: asset_id.clone(),
            page_number: page.page_number,
            title: Some(page.title.clone()),
            text_content: page.text.clone(),
            object_hash: media_type
                .starts_with("image/")
                .then(|| stored.hash.clone()),
            created_at: Utc::now(),
        })?;
        state.emit(
            &session_id,
            "asset_parser",
            "asset.page.extracted",
            0,
            &correlation,
            None,
            json!({
                "asset_id": asset_id,
                "page_id": page_id,
                "page_number": page.page_number,
                "title": page.title,
                "text": page.text,
                "parser": parsed.parser,
                "media_type": media_type,
                "preview_url": media_type.starts_with("image/").then(|| format!("/api/v1/sessions/{session_id}/assets/{asset_id}/content"))
            }),
        )?;
        page_ids.push(page_id);
    }
    Ok(Json(json!({"asset_id": asset_id, "page_ids": page_ids})))
}

pub async fn asset_content(
    State(state): State<AppState>,
    Path((session_id, asset_id)): Path<(String, String)>,
) -> Result<Response, ApiError> {
    let asset = state
        .store
        .get_asset(&session_id, &asset_id)?
        .ok_or_else(|| ApiError::not_found("asset not found"))?;
    let bytes = state.objects.read(&asset.object_hash)?;
    let content_type = HeaderValue::from_str(&asset.media_type)
        .unwrap_or_else(|_| HeaderValue::from_static("application/octet-stream"));
    Ok(Response::builder()
        .header(header::CONTENT_TYPE, content_type)
        .header(
            header::CACHE_CONTROL,
            "private, max-age=31536000, immutable",
        )
        .header(header::X_CONTENT_TYPE_OPTIONS, "nosniff")
        .body(Body::from(bytes))
        .map_err(anyhow::Error::from)?)
}

pub async fn explain_now(
    State(state): State<AppState>,
    Path(session_id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let event = create_explanation(&state, &session_id, "manual").await?;
    Ok(Json(json!({"event": event})))
}

fn sanitize_file_name(value: &str) -> String {
    // Original names remain display metadata and lose path separators and control characters.
    value
        .chars()
        .filter(|character| !character.is_control() && !matches!(character, '/' | '\\' | ':'))
        .take(180)
        .collect::<String>()
}
