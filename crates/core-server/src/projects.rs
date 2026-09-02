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
use axum::extract::{Extension, Path, Query, State};
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
    title: Option<String>,
    source_language: Option<String>,
    target_language: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CreateProjectSessionRequest {
    title: String,
    consent_confirmed: bool,
    device_id: String,
    source_language: Option<String>,
    target_language: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct RecordingStatusQuery {
    device_id: String,
}

#[derive(Debug, Serialize)]
struct RecordingAdmission {
    allowed: bool,
    reason: &'static str,
    retry_after_seconds: u64,
    max_asr_backlog_seconds: i64,
}

#[derive(Debug, Serialize)]
struct RecordingSessionStatus {
    session_id: String,
    session_title: String,
    state: &'static str,
    active_model_jobs: u64,
    recoverable: bool,
    reason: &'static str,
    updated_at: chrono::DateTime<Utc>,
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
    validate_language_pair(&request.source_language, &request.target_language)?;
    let record = state.store.create_project(&NewProject {
        id: format!("project_{}", Uuid::now_v7().simple()),
        owner_subject: user.0.clone(),
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
    state.record_workspace_update(
        &user.0,
        "workspace.project.created",
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
    if request.title.is_none()
        && request.source_language.is_none()
        && request.target_language.is_none()
    {
        return Err(ApiError::bad_request("至少需要更新一个项目字段"));
    }
    let existing = owned_project(&state, &user.0, &project_id)?;
    let title = request.title.as_deref().unwrap_or(&existing.title).trim();
    validate_title(title)?;
    let source_language = request
        .source_language
        .as_deref()
        .unwrap_or(&existing.source_language);
    let target_language = request
        .target_language
        .as_deref()
        .unwrap_or(&existing.target_language);
    validate_language_pair(source_language, target_language)?;
    let record = state
        .store
        .update_project(
            &project_id,
            &user.0,
            title,
            source_language,
            target_language,
        )?
        .ok_or_else(|| ApiError::not_found("project not found"))?;
    state.record_project_update(
        &project_id,
        None,
        "project.updated",
        json!({"project": record}),
    )?;
    state.record_workspace_update(
        &user.0,
        "workspace.project.updated",
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
    let source_language = request
        .source_language
        .as_deref()
        .unwrap_or(&project.source_language);
    let target_language = request
        .target_language
        .as_deref()
        .unwrap_or(&project.target_language);
    validate_language_pair(source_language, target_language)?;
    let session_id = format!("session_{}", Uuid::now_v7().simple());
    state.store.create_session(&NewSession {
        id: session_id.clone(),
        title: request.title.trim().to_owned(),
        source_language: source_language.to_owned(),
        target_language: target_language.to_owned(),
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
    state.record_workspace_update(
        &user.0,
        "workspace.session.created",
        json!({"project_id": project_id, "session": ready}),
    )?;
    Ok(Json(ready))
}

pub async fn recording_status(
    State(state): State<AppState>,
    Extension(user): Extension<CurrentUser>,
    Path(project_id): Path<String>,
    Query(query): Query<RecordingStatusQuery>,
) -> Result<Json<Value>, ApiError> {
    owned_project(&state, &user.0, &project_id)?;
    validate_device_id(&query.device_id)?;
    let now = Utc::now();
    let lease = if let Some(record) = state
        .store
        .get_recording_lease(&project_id)?
        .filter(|record| record.expires_at > now)
    {
        let session_title = state
            .store
            .get_session(&record.session_id)?
            .map(|session| session.title);
        Some(recording_status_lease(
            &record,
            &query.device_id,
            session_title.as_deref(),
        ))
    } else {
        None
    };
    let active_lease_session_id = lease
        .as_ref()
        .and_then(|value| value.get("session_id"))
        .and_then(Value::as_str);
    let sessions = state
        .store
        .list_project_sessions(&project_id)?
        .into_iter()
        .map(|session| recording_session_status(&state, session, active_lease_session_id))
        .collect::<Result<Vec<_>, ApiError>>()?;
    Ok(Json(json!({
        "project_id": project_id,
        "server_time": now,
        "lease": lease,
        "admission": recording_admission(&state)?,
        "sessions": sessions
    })))
}

pub async fn acquire_recording(
    State(state): State<AppState>,
    Extension(user): Extension<CurrentUser>,
    Path((project_id, session_id)): Path<(String, String)>,
    Json(request): Json<AcquireLeaseRequest>,
) -> Result<Json<LeaseResponse>, ApiError> {
    let session = owned_project_session(&state, &user.0, &project_id, &session_id)?;
    validate_device_id(&request.device_id)?;
    let now = Utc::now();
    let has_active_project_lease = state
        .store
        .get_recording_lease(&project_id)?
        .is_some_and(|record| record.expires_at > now);
    let active_model_jobs = state.store.model_queue_counts(Some(&session_id))?;
    if !has_active_project_lease
        && matches!(
            session.state,
            SessionState::Recording | SessionState::Degraded
        )
        && active_model_jobs.queued + active_model_jobs.leased > 0
    {
        return Err(ApiError::conflict_with_code(
            "本次课程仍有后台任务处理中，请等待队列排空后再继续收音",
            "recording_session_processing",
        ));
    }
    if recording_requires_admission(session.state, has_active_project_lease) {
        let admission = recording_admission(&state)?;
        if !admission.allowed {
            return Err(ApiError::unavailable_with_code(
                match admission.reason {
                    "asr_backlog" => "GPU 正在处理已有课程，新项目暂时不能开始录音，请稍后重试",
                    _ => "GPU 录音处理服务暂时不可用，新项目暂时不能开始录音",
                },
                "recording_capacity_unavailable",
            ));
        }
    }
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
        LeaseAcquireOutcome::Conflict(_record) => {
            return Err(ApiError::conflict_with_code(
                "另一台设备正在录音，请在该设备停止后重试",
                "recording_lease_conflict",
            ));
        }
    };
    if session.state == SessionState::Ready {
        state
            .store
            .transition_session(&session_id, SessionState::Recording)?;
        let _ = state.emit(&session_id, "core", "session.recording.started", 0, &format!("lease_{}", lease.generation), None, json!({"visible_recording_required": true, "holder_device_id": request.device_id, "generation": lease.generation}));
    } else if !matches!(
        session.state,
        SessionState::Recording | SessionState::Degraded
    ) {
        let _ = state
            .store
            .release_recording_lease(&project_id, &session_id, &hash);
        return Err(ApiError::conflict_with_code(
            "本次课程当前不能继续录音",
            "recording_session_unavailable",
        ));
    }
    if state
        .record_project_update(
            &project_id,
            Some(&session_id),
            "recording.lease.acquired",
            public_lease(&lease),
        )
        .is_err()
    {
        tracing::warn!(
            error_kind = "recording_lease_notification_failed",
            "recording lease committed without notification"
        );
    }
    if state
        .record_workspace_update(
            &user.0,
            "workspace.recording.changed",
            json!({"project_id": project_id, "session_id": session_id, "active": true, "generation": lease.generation}),
        )
        .is_err()
    {
        tracing::warn!(
            error_kind = "recording_workspace_notification_failed",
            "recording lease committed without workspace notification"
        );
    }
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
    let lease = match state.store.renew_recording_lease(
        &project_id,
        &session_id,
        &request.device_id,
        &hash_token(&request.lease_token),
        LEASE_SECONDS,
    )? {
        Some(lease) => lease,
        None => {
            let active_other =
                state
                    .store
                    .get_recording_lease(&project_id)?
                    .is_some_and(|current| {
                        current.expires_at > Utc::now()
                            && current.holder_device_id != request.device_id
                    });
            return Err(if active_other {
                ApiError::conflict_with_code(
                    "另一台设备已经接管本项目录音",
                    "recording_lease_conflict",
                )
            } else {
                ApiError::conflict_with_code("本机录音租约已到期或失效", "recording_lease_expired")
            });
        }
    };
    if state
        .record_project_update(
            &project_id,
            Some(&session_id),
            "recording.lease.renewed",
            public_lease(&lease),
        )
        .is_err()
    {
        tracing::warn!(
            error_kind = "recording_lease_notification_failed",
            "recording lease renewal committed without notification"
        );
    }
    let _ = state.record_workspace_update(
        &user.0,
        "workspace.recording.changed",
        json!({"project_id": project_id, "session_id": session_id, "active": true, "expires_at": lease.expires_at}),
    );
    Ok(Json(public_lease(&lease)))
}

pub async fn stop_recording(
    State(state): State<AppState>,
    Extension(user): Extension<CurrentUser>,
    Path((project_id, session_id)): Path<(String, String)>,
    Json(request): Json<LeaseSecretRequest>,
) -> Result<Json<SessionRecord>, ApiError> {
    owned_project_session(&state, &user.0, &project_id, &session_id)?;
    validate_device_id(&request.device_id)?;
    let hash = hash_token(&request.lease_token);
    let current = state
        .store
        .get_session(&session_id)?
        .ok_or_else(|| ApiError::not_found("session not found"))?;
    if matches!(
        current.state,
        SessionState::Completed | SessionState::Failed
    ) {
        return Ok(Json(current));
    }
    if current.state == SessionState::Processing {
        // A lost HTTP response after the lease release must be safe to retry.
        let _ = state
            .store
            .release_recording_lease(&project_id, &session_id, &hash);
        let _ = state.record_workspace_update(
            &user.0,
            "workspace.recording.changed",
            json!({"project_id": project_id, "session_id": session_id, "active": false}),
        );
        finish_session_after_stop(&state, &session_id)?;
        return Ok(Json(
            state.store.get_session(&session_id)?.unwrap_or(current),
        ));
    }
    let lease = match state
        .store
        .validate_recording_lease(&session_id, &hash)?
        .filter(|lease| {
            lease.project_id == project_id && lease.holder_device_id == request.device_id
        }) {
        Some(lease) => lease,
        None => {
            let active_other =
                state
                    .store
                    .get_recording_lease(&project_id)?
                    .is_some_and(|current| {
                        current.expires_at > Utc::now()
                            && (current.session_id != session_id
                                || current.holder_device_id != request.device_id)
                    });
            return Err(if active_other {
                ApiError::conflict_with_code(
                    "另一台设备已经接管本项目录音",
                    "recording_lease_conflict",
                )
            } else {
                ApiError::conflict_with_code(
                    "本机录音租约已到期或失效，未确认音频仍保留",
                    "recording_lease_expired",
                )
            });
        }
    };
    if matches!(
        current.state,
        SessionState::Recording | SessionState::Degraded
    ) {
        state
            .store
            .transition_session(&session_id, SessionState::Stopping)?;
        let _ = state.emit(
            &session_id,
            "core",
            "session.stopping",
            0,
            &format!("stop_{session_id}"),
            None,
            json!({"generation": lease.generation}),
        );
    } else if current.state != SessionState::Stopping {
        return Err(ApiError::conflict_with_code(
            "本次课程当前不能停止录音",
            "recording_session_unavailable",
        ));
    }
    let sealed = flush_session_buffers(&state, &session_id)?;
    let processing = if state
        .store
        .get_session(&session_id)?
        .is_some_and(|session| session.state == SessionState::Processing)
    {
        state.store.get_session(&session_id)?.unwrap_or(current)
    } else {
        let processing = state
            .store
            .transition_session(&session_id, SessionState::Processing)?;
        let _ = state.emit(
            &session_id,
            "core",
            "session.processing",
            0,
            &format!("stop_{session_id}"),
            None,
            json!({"sealed_tail_windows": sealed}),
        );
        processing
    };
    // Once the session is in `processing`, a lost response or a concurrent
    // release is already a successful stop from the user's perspective.  Do
    // not turn that idempotent state into a retry loop or lease conflict.
    let _ = state
        .store
        .release_recording_lease(&project_id, &session_id, &hash)?;
    let _ = state.record_project_update(
        &project_id,
        Some(&session_id),
        "recording.lease.released",
        json!({"generation": lease.generation}),
    );
    let _ = state.record_workspace_update(
        &user.0,
        "workspace.recording.changed",
        json!({"project_id": project_id, "session_id": session_id, "active": false}),
    );
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
    let mut receiver = state.project_updates.subscribe();
    let history = state
        .store
        .list_project_updates_after(&project_id, cursor)?;
    // Subscribe before loading history, then ignore broadcasts already covered
    // by the snapshot cursor.  This closes the refresh race without keeping an
    // unbounded set of event IDs in every browser connection.
    let watermark = history.last().map(|update| update.cursor).unwrap_or(cursor);
    let output = stream! {
        for update in history {
            yield Ok(project_sse_event(&update));
        }
        loop {
            match receiver.recv().await {
                Ok(update) if update.project_id == project_id && update.cursor > watermark => yield Ok(project_sse_event(&update)),
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

pub async fn readweave_targets(
    State(state): State<AppState>,
    Extension(user): Extension<CurrentUser>,
    Path(project_id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    owned_project(&state, &user.0, &project_id)?;
    let status = crate::readweave::status_payload(&state, &project_id)?;
    Ok(Json(
        status.get("targets").cloned().unwrap_or_else(|| json!([])),
    ))
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

pub async fn summarize_session(
    State(state): State<AppState>,
    Extension(user): Extension<CurrentUser>,
    Path((project_id, session_id)): Path<(String, String)>,
) -> Result<Json<Value>, ApiError> {
    owned_project_session(&state, &user.0, &project_id, &session_id)?;
    let job = crate::jobs::enqueue_summary(&state, &session_id, "manual")?;
    Ok(Json(json!({"job_id": job.id, "status": job.status})))
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

fn recording_status_lease(
    lease: &RecordingLeaseRecord,
    requesting_device_id: &str,
    session_title: Option<&str>,
) -> Value {
    json!({
        "session_id": lease.session_id,
        "session_title": session_title,
        "holder": if lease.holder_device_id == requesting_device_id { "self" } else { "other" },
        "generation": lease.generation,
        "expires_at": lease.expires_at
    })
}

fn recording_session_status(
    state: &AppState,
    session: SessionRecord,
    active_lease_session_id: Option<&str>,
) -> Result<RecordingSessionStatus, ApiError> {
    let active_model_jobs = state.store.model_queue_counts(Some(&session.id))?;
    let has_active_lease = active_lease_session_id == Some(session.id.as_str());
    let (recoverable, reason) = match session.state {
        SessionState::Ready => (false, "ready"),
        SessionState::Recording | SessionState::Degraded if has_active_lease => {
            (false, "active_recording")
        }
        SessionState::Recording | SessionState::Degraded
            if active_model_jobs.queued + active_model_jobs.leased == 0 =>
        {
            (true, "recovery_available")
        }
        SessionState::Recording | SessionState::Degraded => (false, "processing"),
        SessionState::Stopping | SessionState::Processing => (false, "processing"),
        SessionState::Completed
        | SessionState::Failed
        | SessionState::Created
        | SessionState::Archived => (false, "not_recordable"),
    };
    Ok(RecordingSessionStatus {
        session_id: session.id,
        session_title: session.title,
        state: session_state_name(session.state),
        active_model_jobs: active_model_jobs.queued + active_model_jobs.leased,
        recoverable,
        reason,
        updated_at: session.updated_at,
    })
}

fn session_state_name(state: SessionState) -> &'static str {
    match state {
        SessionState::Created => "created",
        SessionState::Ready => "ready",
        SessionState::Recording => "recording",
        SessionState::Degraded => "degraded",
        SessionState::Stopping => "stopping",
        SessionState::Processing => "processing",
        SessionState::Completed => "completed",
        SessionState::Failed => "failed",
        SessionState::Archived => "archived",
    }
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

const SOURCE_LANGUAGES: &[&str] = &["auto", "zh", "en", "ja", "ko", "es", "fr", "de"];
const TARGET_LANGUAGES: &[&str] = &["zh-CN", "en", "ja", "ko", "es", "fr", "de"];

fn validate_language_pair(source: &str, target: &str) -> Result<(), ApiError> {
    if !SOURCE_LANGUAGES.contains(&source) || !TARGET_LANGUAGES.contains(&target) {
        return Err(ApiError::bad_request("不支持所选讲授语言或翻译语言"));
    }
    Ok(())
}

fn recording_admission(state: &AppState) -> Result<RecordingAdmission, ApiError> {
    let now = Utc::now();
    let max_asr_backlog_seconds =
        std::env::var("AIALRA_RECORDING_ADMISSION_MAX_ASR_BACKLOG_SECONDS")
            .ok()
            .and_then(|value| value.parse::<i64>().ok())
            .unwrap_or(15)
            .clamp(3, 300);
    let worker = state.store.list_workers()?.into_iter().find(|record| {
        record.capabilities.iter().any(|item| item == "asr")
            && (now - record.last_seen_at).num_seconds() <= 30
    });
    let Some(worker) = worker else {
        return Ok(RecordingAdmission {
            allowed: false,
            reason: "asr_worker_offline",
            retry_after_seconds: 5,
            max_asr_backlog_seconds,
        });
    };
    let provider = worker
        .model_metadata
        .get("asr_provider")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let healthy = worker.model_metadata.get("status").and_then(Value::as_str) == Some("ok")
        && worker
            .model_metadata
            .get("asr_available")
            .and_then(Value::as_bool)
            == Some(true)
        && provider.ends_with("@cuda");
    if !healthy {
        return Ok(RecordingAdmission {
            allowed: false,
            reason: "asr_degraded",
            retry_after_seconds: 5,
            max_asr_backlog_seconds,
        });
    }
    if let Some(oldest) = state.store.oldest_active_model_job_at("asr")?
        && (now - oldest).num_seconds() > max_asr_backlog_seconds
    {
        return Ok(RecordingAdmission {
            allowed: false,
            reason: "asr_backlog",
            retry_after_seconds: 5,
            max_asr_backlog_seconds,
        });
    }
    Ok(RecordingAdmission {
        allowed: true,
        reason: "ok",
        retry_after_seconds: 0,
        max_asr_backlog_seconds,
    })
}

fn recording_requires_admission(state: SessionState, has_active_project_lease: bool) -> bool {
    !has_active_project_lease && !matches!(state, SessionState::Recording | SessionState::Degraded)
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
    use aialra_event_store::{NewModelJob, NewSession, WorkerHeartbeat};

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

    #[test]
    fn course_languages_are_validated_without_changing_old_defaults() {
        assert!(validate_language_pair("auto", "zh-CN").is_ok());
        assert!(validate_language_pair("ja", "en").is_ok());
        assert!(validate_language_pair("unsupported", "en").is_err());
        assert!(validate_language_pair("en", "auto").is_err());
        assert_eq!(default_source_language(), "en");
        assert_eq!(default_target_language(), "zh-CN");
    }

    #[test]
    fn recording_status_hides_device_and_secret_fields() {
        let lease = RecordingLeaseRecord {
            project_id: "project_test".to_owned(),
            session_id: "session_test".to_owned(),
            holder_device_id: "browser-private".to_owned(),
            lease_token_hash: "private-digest".to_owned(),
            generation: 2,
            heartbeat_at: Utc::now(),
            expires_at: Utc::now() + chrono::Duration::seconds(45),
        };
        let value = recording_status_lease(&lease, "browser-other", Some("测试课程"));
        assert_eq!(value["holder"], "other");
        assert!(value.get("holder_device_id").is_none());
        assert!(value.get("lease_token_hash").is_none());
        assert!(value.get("heartbeat_at").is_none());
    }

    #[test]
    fn recording_admission_requires_a_recent_healthy_cuda_asr_worker() {
        let temp = tempfile::tempdir().unwrap();
        let state = AppState::open(temp.path()).unwrap();
        let offline = recording_admission(&state).unwrap();
        assert!(!offline.allowed);
        assert_eq!(offline.reason, "asr_worker_offline");

        state
            .store
            .heartbeat_worker(&WorkerHeartbeat {
                id: "worker_test".to_owned(),
                capabilities: vec!["asr".to_owned()],
                model_metadata: json!({
                    "status": "ok",
                    "asr_available": true,
                    "asr_provider": "faster-whisper@cuda"
                }),
                active_job_id: None,
            })
            .unwrap();
        let healthy = recording_admission(&state).unwrap();
        assert!(healthy.allowed);
        assert_eq!(healthy.reason, "ok");
    }

    #[test]
    fn capacity_gate_never_blocks_an_existing_recording() {
        assert!(!recording_requires_admission(
            SessionState::Recording,
            false
        ));
        assert!(!recording_requires_admission(SessionState::Degraded, false));
        assert!(!recording_requires_admission(SessionState::Ready, true));
        assert!(recording_requires_admission(SessionState::Ready, false));
    }

    #[test]
    fn recording_status_marks_an_idle_unfinished_session_as_recoverable() {
        let temp = tempfile::tempdir().unwrap();
        let state = AppState::open(temp.path()).unwrap();
        state
            .store
            .create_session(&NewSession {
                id: "session_recoverable".to_owned(),
                title: "Recoverable course".to_owned(),
                source_language: "en".to_owned(),
                target_language: "zh-CN".to_owned(),
                privacy_mode: "local_only".to_owned(),
                consent_confirmed: true,
                demo_mode: false,
            })
            .unwrap();
        state
            .store
            .transition_session("session_recoverable", SessionState::Ready)
            .unwrap();
        state
            .store
            .transition_session("session_recoverable", SessionState::Recording)
            .unwrap();

        let recoverable = recording_session_status(
            &state,
            state
                .store
                .get_session("session_recoverable")
                .unwrap()
                .unwrap(),
            None,
        )
        .unwrap();
        assert!(recoverable.recoverable);
        assert_eq!(recoverable.reason, "recovery_available");

        state
            .store
            .enqueue_model_job(&NewModelJob {
                id: "job-recoverable-translate".to_owned(),
                session_id: "session_recoverable".to_owned(),
                job_type: "translate".to_owned(),
                priority: 70,
                input: json!({"text": "pending"}),
                input_object_hash: None,
                idempotency_key: "translate:recoverable".to_owned(),
            })
            .unwrap();
        let processing = recording_session_status(
            &state,
            state
                .store
                .get_session("session_recoverable")
                .unwrap()
                .unwrap(),
            None,
        )
        .unwrap();
        assert!(!processing.recoverable);
        assert_eq!(processing.reason, "processing");
        assert_eq!(processing.active_model_jobs, 1);
    }
}
