//! Owner-scoped projects, durable observer updates, and exclusive recorder leases

use crate::app::{ApiError, AppState};
use crate::audio::flush_session_buffers;
use crate::identity::{CurrentUser, valid_identifier};
use crate::jobs::finish_session_after_stop;
use aialra_core_domain::SessionState;
use aialra_event_store::{
    LeaseAcquireOutcome, NewProject, NewSession, ProjectRecord, ProjectUpdateRecord,
    RecordingLeaseRecord, SessionRecord,
};
use async_stream::stream;
use axum::Json;
use axum::extract::{Extension, Path, State};
use axum::http::HeaderMap;
use axum::response::sse::{Event, KeepAlive, Sse};
use base64::Engine;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::convert::Infallible;
use std::time::Duration;
use uuid::Uuid;

const LEASE_SECONDS: i64 = 45;

#[derive(Debug, Deserialize)]
pub struct CreateProjectRequest {
    title: String,
    #[serde(default = "default_source_language")]
    source_language: String,
    #[serde(default = "default_target_language")]
    target_language: String,
}

#[derive(Debug, Deserialize)]
pub struct UpdateProjectRequest {
    title: String,
}

#[derive(Debug, Deserialize)]
pub struct CreateProjectSessionRequest {
    title: String,
    consent_confirmed: bool,
    device_id: String,
}

#[derive(Debug, Deserialize)]
pub struct AcquireLeaseRequest {
    device_id: String,
}

#[derive(Debug, Deserialize)]
pub struct LeaseSecretRequest {
    device_id: String,
    lease_token: String,
}

#[derive(Debug, Serialize)]
pub struct LeaseResponse {
    project_id: String,
    session_id: String,
    holder_device_id: String,
    generation: u64,
    expires_at: chrono::DateTime<Utc>,
    lease_token: String,
}

pub async fn list_projects(
    State(state): State<AppState>,
    Extension(user): Extension<CurrentUser>,
) -> Result<Json<Vec<ProjectRecord>>, ApiError> {
    Ok(Json(state.store.list_projects(&user.0)?))
}

pub async fn create_project(
    State(state): State<AppState>,
    Extension(user): Extension<CurrentUser>,
    Json(request): Json<CreateProjectRequest>,
) -> Result<Json<ProjectRecord>, ApiError> {
    validate_title(&request.title)?;
    let record = state.store.create_project(&NewProject {
        id: format!("project_{}", Uuid::now_v7().simple()),
        owner_subject: user.0,
        title: request.title.trim().to_owned(),
        source_language: request.source_language,
        target_language: request.target_language,
    })?;
    state.record_project_update(
        &record.id,
        None,
        "project.created",
        json!({"project": record}),
    )?;
    if crate::readweave::configured() {
        state.record_project_update(&record.id, None, "readweave.egress.authorized", json!({"scope": ["stable_transcript", "translation", "explanation", "asset_index"], "raw_audio": false, "raw_assets": false}))?;
    }
    Ok(Json(record))
}

pub async fn get_project(
    State(state): State<AppState>,
    Extension(user): Extension<CurrentUser>,
    Path(project_id): Path<String>,
) -> Result<Json<ProjectRecord>, ApiError> {
    Ok(Json(owned_project(&state, &user.0, &project_id)?))
}

pub async fn update_project(
    State(state): State<AppState>,
    Extension(user): Extension<CurrentUser>,
    Path(project_id): Path<String>,
    Json(request): Json<UpdateProjectRequest>,
) -> Result<Json<ProjectRecord>, ApiError> {
    validate_title(&request.title)?;
    owned_project(&state, &user.0, &project_id)?;
    let record = state
        .store
        .update_project_title(&project_id, &user.0, request.title.trim())?
        .ok_or_else(|| ApiError::not_found("project not found"))?;
    state.record_project_update(
        &project_id,
        None,
        "project.updated",
        json!({"project": record}),
    )?;
    Ok(Json(record))
}

pub async fn list_project_sessions(
    State(state): State<AppState>,
    Extension(user): Extension<CurrentUser>,
    Path(project_id): Path<String>,
) -> Result<Json<Vec<SessionRecord>>, ApiError> {
    owned_project(&state, &user.0, &project_id)?;
    Ok(Json(state.store.list_project_sessions(&project_id)?))
}

pub async fn create_project_session(
    State(state): State<AppState>,
    Extension(user): Extension<CurrentUser>,
    Path(project_id): Path<String>,
    Json(request): Json<CreateProjectSessionRequest>,
) -> Result<Json<SessionRecord>, ApiError> {
    let project = owned_project(&state, &user.0, &project_id)?;
    validate_title(&request.title)?;
    validate_device_id(&request.device_id)?;
    if !request.consent_confirmed {
        return Err(ApiError::bad_request("recording consent is required"));
    }
    let session_id = format!("session_{}", Uuid::now_v7().simple());
    state.store.create_session(&NewSession {
        id: session_id.clone(),
        title: request.title.trim().to_owned(),
        source_language: project.source_language,
        target_language: project.target_language,
        privacy_mode: "local_only".to_owned(),
        consent_confirmed: true,
        demo_mode: false,
    })?;
    state
        .store
        .attach_session_to_project(&project_id, &session_id, &user.0, &request.device_id)?;
    state.emit(
        &session_id,
        "core",
        "consent.recorded",
        0,
        &format!("create_{session_id}"),
        None,
        json!({"confirmed": true, "privacy_mode": "local_only"}),
    )?;
    state.emit(
        &session_id,
        "core",
        "session.created",
        0,
        &format!("create_{session_id}"),
        None,
        json!({"title": request.title.trim()}),
    )?;
    let ready = state
        .store
        .transition_session(&session_id, SessionState::Ready)?;
    state.emit(
        &session_id,
        "core",
        "session.ready",
        0,
        &format!("create_{session_id}"),
        None,
        json!({}),
    )?;
    Ok(Json(ready))
}

pub async fn acquire_recording(
    State(state): State<AppState>,
    Extension(user): Extension<CurrentUser>,
    Path((project_id, session_id)): Path<(String, String)>,
    Json(request): Json<AcquireLeaseRequest>,
) -> Result<Json<LeaseResponse>, ApiError> {
    owned_project_session(&state, &user.0, &project_id, &session_id)?;
    validate_device_id(&request.device_id)?;
    let token = new_lease_token();
    let hash = hash_token(&token);
    let lease = match state.store.acquire_recording_lease(
        &project_id,
        &session_id,
        &request.device_id,
        &hash,
        LEASE_SECONDS,
    )? {
        LeaseAcquireOutcome::Acquired(record) => record,
        LeaseAcquireOutcome::Conflict(record) => {
            return Err(ApiError::conflict(format!(
                "recording_lease_conflict:{}:{}",
                record.session_id, record.expires_at
            )));
        }
    };
    let session = state
        .store
        .get_session(&session_id)?
        .ok_or_else(|| ApiError::not_found("session not found"))?;
    if session.state == SessionState::Ready {
        state
            .store
            .transition_session(&session_id, SessionState::Recording)?;
        state.emit(&session_id, "core", "session.recording.started", 0, &format!("lease_{}", lease.generation), None, json!({"visible_recording_required": true, "holder_device_id": request.device_id, "generation": lease.generation}))?;
    } else if !matches!(
        session.state,
        SessionState::Recording | SessionState::Degraded
    ) {
        let _ = state
            .store
            .release_recording_lease(&project_id, &session_id, &hash);
        return Err(ApiError::conflict("session is not available for recording"));
    }
    state.record_project_update(
        &project_id,
        Some(&session_id),
        "recording.lease.acquired",
        public_lease(&lease),
    )?;
    Ok(Json(lease_response(lease, token)))
}

pub async fn renew_recording(
    State(state): State<AppState>,
    Extension(user): Extension<CurrentUser>,
    Path((project_id, session_id)): Path<(String, String)>,
    Json(request): Json<LeaseSecretRequest>,
) -> Result<Json<Value>, ApiError> {
    let session = owned_project_session(&state, &user.0, &project_id, &session_id)?;
    if !matches!(
        session.state,
        SessionState::Recording | SessionState::Degraded
    ) {
        return Err(ApiError::conflict("session is not recording"));
    }
    validate_device_id(&request.device_id)?;
    let lease = state
        .store
        .renew_recording_lease(
            &project_id,
            &session_id,
            &request.device_id,
            &hash_token(&request.lease_token),
            LEASE_SECONDS,
        )?
        .ok_or_else(|| ApiError::conflict("recording lease expired or changed"))?;
    state.record_project_update(
        &project_id,
        Some(&session_id),
        "recording.lease.renewed",
        public_lease(&lease),
    )?;
    Ok(Json(public_lease(&lease)))
}

pub async fn stop_recording(
    State(state): State<AppState>,
    Extension(user): Extension<CurrentUser>,
    Path((project_id, session_id)): Path<(String, String)>,
    Json(request): Json<LeaseSecretRequest>,
) -> Result<Json<SessionRecord>, ApiError> {
    owned_project_session(&state, &user.0, &project_id, &session_id)?;
    let hash = hash_token(&request.lease_token);
    let lease = state
        .store
        .validate_recording_lease(&session_id, &hash)?
        .filter(|lease| {
            lease.project_id == project_id && lease.holder_device_id == request.device_id
        })
        .ok_or_else(|| ApiError::conflict("recording lease expired or changed"))?;
    state
        .store
        .transition_session(&session_id, SessionState::Stopping)?;
    state.emit(
        &session_id,
        "core",
        "session.stopping",
        0,
        &format!("stop_{session_id}"),
        None,
        json!({"generation": lease.generation}),
    )?;
    let sealed = flush_session_buffers(&state, &session_id)?;
    let processing = state
        .store
        .transition_session(&session_id, SessionState::Processing)?;
    state.emit(
        &session_id,
        "core",
        "session.processing",
        0,
        &format!("stop_{session_id}"),
        None,
        json!({"sealed_tail_windows": sealed}),
    )?;
    if !state
        .store
        .release_recording_lease(&project_id, &session_id, &hash)?
    {
        return Err(ApiError::conflict("recording lease changed during stop"));
    }
    state.record_project_update(
        &project_id,
        Some(&session_id),
        "recording.lease.released",
        json!({"generation": lease.generation}),
    )?;
    finish_session_after_stop(&state, &session_id)?;
    Ok(Json(
        state.store.get_session(&session_id)?.unwrap_or(processing),
    ))
}

pub async fn stream_project(
    State(state): State<AppState>,
    Extension(user): Extension<CurrentUser>,
    Path(project_id): Path<String>,
    headers: HeaderMap,
) -> Result<Sse<impl futures_util::Stream<Item = Result<Event, Infallible>>>, ApiError> {
    owned_project(&state, &user.0, &project_id)?;
    let cursor = headers
        .get("last-event-id")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<i64>().ok())
        .unwrap_or(0);
    let history = state
        .store
        .list_project_updates_after(&project_id, cursor)?;
    let mut receiver = state.project_updates.subscribe();
    let output = stream! {
        for update in history {
            yield Ok(project_sse_event(&update));
        }
        loop {
            match receiver.recv().await {
                Ok(update) if update.project_id == project_id => yield Ok(project_sse_event(&update)),
                Ok(_) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => break,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    };
    Ok(Sse::new(output).keep_alive(KeepAlive::new().interval(Duration::from_secs(15))))
}

pub async fn readweave_status(
    State(state): State<AppState>,
    Extension(user): Extension<CurrentUser>,
    Path(project_id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    owned_project(&state, &user.0, &project_id)?;
    Ok(Json(crate::readweave::status_payload(&state, &project_id)?))
}

pub async fn readweave_preview(
    State(state): State<AppState>,
    Extension(user): Extension<CurrentUser>,
    Path(project_id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    owned_project(&state, &user.0, &project_id)?;
    Ok(Json(crate::readweave::preview_payload(
        &state,
        &project_id,
    )?))
}

pub async fn reconcile_readweave(
    State(state): State<AppState>,
    Extension(user): Extension<CurrentUser>,
    Path(project_id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    owned_project(&state, &user.0, &project_id)?;
    crate::readweave::enqueue_manual_reconcile(&state, &project_id)?;
    state.record_project_update(&project_id, None, "readweave.reconcile.queued", json!({}))?;
    Ok(Json(json!({"queued": true})))
}

fn project_sse_event(update: &ProjectUpdateRecord) -> Event {
    Event::default()
        .id(update.cursor.to_string())
        .event(&update.update_type)
        .json_data(update)
        .unwrap_or_else(|_| Event::default().event("serialization.error"))
}

pub fn owned_project(
    state: &AppState,
    subject: &str,
    project_id: &str,
) -> Result<ProjectRecord, ApiError> {
    state
        .store
        .get_project(project_id)?
        .filter(|project| project.owner_subject == subject)
        .ok_or_else(|| ApiError::not_found("project not found"))
}

fn owned_project_session(
    state: &AppState,
    subject: &str,
    project_id: &str,
    session_id: &str,
) -> Result<SessionRecord, ApiError> {
    owned_project(state, subject, project_id)?;
    let project = state
        .store
        .project_for_session(session_id)?
        .ok_or_else(|| ApiError::not_found("session not found"))?;
    if project.id != project_id {
        return Err(ApiError::not_found("session not found"));
    }
    state
        .store
        .get_session(session_id)?
        .ok_or_else(|| ApiError::not_found("session not found"))
}

pub fn assign_legacy_sessions(state: &AppState, subject: &str) -> Result<usize, ApiError> {
    if !crate::identity::valid_identifier(subject) {
        return Err(ApiError::bad_request("legacy owner subject is invalid"));
    }
    let unassigned = state.store.list_unassigned_sessions()?;
    if unassigned.is_empty() {
        return Ok(0);
    }
    let project = if let Some(project) = state
        .store
        .list_projects(subject)?
        .into_iter()
        .find(|project| project.title == "历史课程")
    {
        project
    } else {
        state.store.create_project(&NewProject {
            id: format!("project_{}", Uuid::now_v7().simple()),
            owner_subject: subject.to_owned(),
            title: "历史课程".to_owned(),
            source_language: "en".to_owned(),
            target_language: "zh-CN".to_owned(),
        })?
    };
    let imported = unassigned.len();
    for session in unassigned {
        state.store.attach_session_to_project(
            &project.id,
            &session.id,
            subject,
            "legacy-import",
        )?;
    }
    state.record_project_update(
        &project.id,
        None,
        "project.legacy_imported",
        json!({"session_count": state.store.list_project_sessions(&project.id)?.len()}),
    )?;
    Ok(imported)
}

fn lease_response(lease: RecordingLeaseRecord, token: String) -> LeaseResponse {
    LeaseResponse {
        project_id: lease.project_id,
        session_id: lease.session_id,
        holder_device_id: lease.holder_device_id,
        generation: lease.generation,
        expires_at: lease.expires_at,
        lease_token: token,
    }
}

fn public_lease(lease: &RecordingLeaseRecord) -> Value {
    json!({"project_id": lease.project_id, "session_id": lease.session_id, "holder_device_id": lease.holder_device_id, "generation": lease.generation, "heartbeat_at": lease.heartbeat_at, "expires_at": lease.expires_at})
}

pub fn hash_token(token: &str) -> String {
    hex::encode(Sha256::digest(token.as_bytes()))
}

fn new_lease_token() -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(Uuid::now_v7().as_bytes())
        + &base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(Uuid::now_v7().as_bytes())
}

fn validate_title(value: &str) -> Result<(), ApiError> {
    (!value.trim().is_empty() && value.chars().count() <= 160)
        .then_some(())
        .ok_or_else(|| {
            ApiError::bad_request("title is required and must not exceed 160 characters")
        })
}

fn validate_device_id(value: &str) -> Result<(), ApiError> {
    (valid_identifier(value) && value.len() >= 8)
        .then_some(())
        .ok_or_else(|| ApiError::bad_request("device_id is invalid"))
}

fn default_source_language() -> String {
    "en".to_owned()
}
fn default_target_language() -> String {
    "zh-CN".to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lease_tokens_are_url_safe_and_hashable() {
        let token = new_lease_token();
        assert!(
            token
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
        );
        assert_eq!(hash_token(&token).len(), 64);
    }

    #[test]
    fn public_lease_never_exposes_the_token_hash() {
        let lease = RecordingLeaseRecord {
            project_id: "project_test".to_owned(),
            session_id: "session_test".to_owned(),
            holder_device_id: "browser-test".to_owned(),
            lease_token_hash: "private-digest".to_owned(),
            generation: 1,
            heartbeat_at: Utc::now(),
            expires_at: Utc::now() + chrono::Duration::seconds(45),
        };
        let value = public_lease(&lease);
        assert!(value.get("lease_token_hash").is_none());
        assert_eq!(value["generation"], 1);
    }
}
