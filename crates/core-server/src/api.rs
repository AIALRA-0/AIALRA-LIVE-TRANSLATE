//! Versioned HTTP and Server-Sent Events endpoints for the desktop UI.

use crate::app::{ApiError, AppState};
use crate::explanation::enqueue_explanation;
use crate::identity::CurrentUser;
use aialra_event_store::{AssetRecord, NewModelJob, SessionRecord};
use axum::body::Body;
use axum::extract::{Extension, Multipart, Path, State};
use axum::http::{HeaderValue, header};
use axum::response::Response;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::{Json, response::IntoResponse};
use chrono::Utc;
use serde_json::{Value, json};
use std::convert::Infallible;
use std::time::Duration;
use uuid::Uuid;

const MAX_ASSET_BYTES: usize = 50 * 1024 * 1024;

pub async fn health(State(state): State<AppState>) -> impl IntoResponse {
    let queue = state.store.model_queue_counts(None).ok();
    let worker = state.store.latest_worker().ok().flatten().map(|record| {
        let online = (Utc::now() - record.last_seen_at).num_seconds() <= 30;
        json!({
            "id": record.id,
            "online": online,
            "capabilities": record.capabilities,
            "model_metadata": record.model_metadata,
            "active_job_id": record.active_job_id,
            "last_seen_at": record.last_seen_at
        })
    });
    Json(json!({
        "status": "ok",
        "service": "aialra-core",
        "version": env!("CARGO_PKG_VERSION"),
        "worker": worker,
        "model_queue": queue,
        "local_only_default": true,
        "deployment_mode": std::env::var("AIALRA_DEPLOYMENT_MODE").unwrap_or_else(|_| "local".to_owned()),
        "processing_location": std::env::var("AIALRA_PROCESSING_LOCATION").unwrap_or_else(|_| "本机处理".to_owned())
    }))
}

pub async fn create_session(
    State(_state): State<AppState>,
    Extension(_user): Extension<CurrentUser>,
) -> Result<Json<SessionRecord>, ApiError> {
    Err(ApiError::conflict(
        "project-scoped session creation is required",
    ))
}

pub async fn list_sessions(
    State(state): State<AppState>,
    Extension(user): Extension<CurrentUser>,
) -> Result<Json<Vec<SessionRecord>>, ApiError> {
    Ok(Json(state.store.list_sessions_for_owner(&user.0)?))
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
    State(_state): State<AppState>,
    Path(_session_id): Path<String>,
) -> Result<Json<SessionRecord>, ApiError> {
    Err(ApiError::conflict("recording lease is required"))
}

pub async fn stop_session(
    State(_state): State<AppState>,
    Path(_session_id): Path<String>,
) -> Result<Json<SessionRecord>, ApiError> {
    Err(ApiError::conflict("recording lease is required"))
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

    let job = state.enqueue_job(NewModelJob {
        id: format!("job_{}", Uuid::now_v7().simple()),
        session_id: session_id.clone(),
        job_type: "asset_parse".to_owned(),
        priority: 10,
        input: json!({
            "asset_id": asset_id,
            "file_name": sanitize_file_name(&file_name),
            "media_type": media_type
        }),
        input_object_hash: Some(stored.hash),
        idempotency_key: format!("asset_parse:{session_id}:{asset_id}"),
    })?;
    Ok(Json(
        json!({"asset_id": asset_id, "job_id": job.id, "page_ids": []}),
    ))
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
    let job = enqueue_explanation(&state, &session_id, "manual")?;
    Ok(Json(json!({"job_id": job.id, "status": job.status})))
}

fn sanitize_file_name(value: &str) -> String {
    // Original names remain display metadata and lose path separators and control characters.
    value
        .chars()
        .filter(|character| !character.is_control() && !matches!(character, '/' | '\\' | ':'))
        .take(180)
        .collect::<String>()
}
