//! Owner-scoped workspace tree, device navigation, and project AI policy endpoints.

use crate::app::{ApiError, AppState};
use crate::identity::{CurrentUser, valid_identifier};
use crate::projects::owned_project;
use aialra_event_store::{
    NewWorkspaceFolder, WorkspaceDevicePreferenceRecord, WorkspaceUpdateRecord,
};
use async_stream::stream;
use axum::Json;
use axum::extract::{Extension, Path, Query, State};
use axum::http::HeaderMap;
use axum::response::sse::{Event, KeepAlive, Sse};
use chrono::Utc;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::convert::Infallible;
use std::time::Duration;
use uuid::Uuid;

const MAX_FOLDER_DEPTH: usize = 5;

#[derive(Debug, Deserialize)]
pub struct WorkspaceQuery {
    device_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CreateFolderRequest {
    title: String,
    parent_id: Option<String>,
    #[serde(default)]
    sort_order: i64,
}

#[derive(Debug, Deserialize)]
pub struct UpdateFolderRequest {
    title: String,
    parent_id: Option<String>,
    #[serde(default)]
    sort_order: i64,
    #[serde(default)]
    archived: bool,
}

#[derive(Debug, Deserialize)]
pub struct ProjectPlacementRequest {
    folder_id: Option<String>,
    #[serde(default)]
    sort_order: i64,
    #[serde(default)]
    archived: bool,
}

#[derive(Debug, Deserialize)]
pub struct SessionMetadataRequest {
    title: Option<String>,
    #[serde(default)]
    pinned: bool,
    #[serde(default)]
    sort_order: i64,
    #[serde(default)]
    archived: bool,
}

#[derive(Debug, Deserialize)]
pub struct PreferenceRequest {
    active_project_id: Option<String>,
    active_session_id: Option<String>,
    language_view: String,
    #[serde(default)]
    sidebar_collapsed: bool,
}

#[derive(Debug, Deserialize)]
pub struct AiPolicyRequest {
    #[serde(default)]
    cloud_enabled: bool,
    #[serde(default)]
    allowed_modalities: Vec<String>,
}

pub async fn workspace_snapshot(
    State(state): State<AppState>,
    Extension(user): Extension<CurrentUser>,
    Query(query): Query<WorkspaceQuery>,
) -> Result<Json<Value>, ApiError> {
    let device_id = query
        .device_id
        .as_deref()
        .filter(|value| valid_identifier(value));
    let preference = match device_id {
        Some(device_id) => state.store.get_workspace_preference(&user.0, device_id)?,
        None => None,
    };
    let projects = state.store.list_projects(&user.0)?;
    let mut session_projects = serde_json::Map::new();
    for project in &projects {
        for session in state.store.list_project_sessions(&project.id)? {
            session_projects.insert(session.id, json!(project.id));
        }
    }
    Ok(Json(json!({
        "folders": state.store.list_workspace_folders(&user.0)?,
        "projects": projects,
        "project_placements": state.store.list_workspace_project_placements(&user.0)?,
        "sessions": state.store.list_sessions_for_owner(&user.0)?,
        "session_projects": session_projects,
        "session_metadata": state.store.list_workspace_session_metadata(&user.0)?,
        "preference": preference,
    })))
}

pub async fn create_folder(
    State(state): State<AppState>,
    Extension(user): Extension<CurrentUser>,
    Json(request): Json<CreateFolderRequest>,
) -> Result<Json<Value>, ApiError> {
    validate_title(&request.title)?;
    validate_parent(&state, &user.0, None, request.parent_id.as_deref())?;
    let record = state.store.create_workspace_folder(&NewWorkspaceFolder {
        id: format!("folder_{}", Uuid::now_v7().simple()),
        owner_subject: user.0.clone(),
        parent_id: request.parent_id,
        title: request.title.trim().to_owned(),
        sort_order: request.sort_order,
    })?;
    state.record_workspace_update(
        &user.0,
        "workspace.folder.created",
        json!({"folder": record}),
    )?;
    Ok(Json(serde_json::to_value(record)?))
}

pub async fn update_folder(
    State(state): State<AppState>,
    Extension(user): Extension<CurrentUser>,
    Path(folder_id): Path<String>,
    Json(request): Json<UpdateFolderRequest>,
) -> Result<Json<Value>, ApiError> {
    validate_title(&request.title)?;
    let existing = owned_folder(&state, &user.0, &folder_id)?;
    validate_parent(
        &state,
        &user.0,
        Some(&folder_id),
        request.parent_id.as_deref(),
    )?;
    if request.archived {
        ensure_folder_empty(&state, &user.0, &folder_id)?;
    }
    let record = state
        .store
        .update_workspace_folder(
            &folder_id,
            &user.0,
            request.title.trim(),
            request.parent_id.as_deref(),
            request.sort_order,
            request.archived,
        )?
        .ok_or_else(|| ApiError::not_found("workspace folder not found"))?;
    let update_type = if request.archived {
        "workspace.folder.archived"
    } else if existing.parent_id != record.parent_id {
        "workspace.folder.moved"
    } else {
        "workspace.folder.updated"
    };
    state.record_workspace_update(&user.0, update_type, json!({"folder": record}))?;
    enqueue_owner_readweave_reconcile(&state, &user.0)?;
    Ok(Json(serde_json::to_value(record)?))
}

pub async fn archive_folder(
    State(state): State<AppState>,
    Extension(user): Extension<CurrentUser>,
    Path(folder_id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let folder = owned_folder(&state, &user.0, &folder_id)?;
    ensure_folder_empty(&state, &user.0, &folder_id)?;
    let record = state
        .store
        .update_workspace_folder(
            &folder_id,
            &user.0,
            &folder.title,
            folder.parent_id.as_deref(),
            folder.sort_order,
            true,
        )?
        .ok_or_else(|| ApiError::not_found("workspace folder not found"))?;
    state.record_workspace_update(
        &user.0,
        "workspace.folder.archived",
        json!({"folder": record}),
    )?;
    Ok(Json(serde_json::to_value(record)?))
}

pub async fn update_project_placement(
    State(state): State<AppState>,
    Extension(user): Extension<CurrentUser>,
    Path(project_id): Path<String>,
    Json(request): Json<ProjectPlacementRequest>,
) -> Result<Json<Value>, ApiError> {
    owned_project(&state, &user.0, &project_id)?;
    if let Some(folder_id) = request.folder_id.as_deref() {
        owned_folder(&state, &user.0, folder_id)?;
    }
    let placement = state.store.update_workspace_project_placement(
        &project_id,
        request.folder_id.as_deref(),
        request.sort_order,
        request.archived,
    )?;
    state.record_workspace_update(
        &user.0,
        "workspace.project.placed",
        json!({"placement": placement}),
    )?;
    crate::readweave::enqueue_manual_reconcile(&state, &project_id)?;
    Ok(Json(serde_json::to_value(placement)?))
}

pub async fn update_session_metadata(
    State(state): State<AppState>,
    Extension(user): Extension<CurrentUser>,
    Path((project_id, session_id)): Path<(String, String)>,
    Json(request): Json<SessionMetadataRequest>,
) -> Result<Json<Value>, ApiError> {
    let project = state
        .store
        .project_for_session(&session_id)?
        .ok_or_else(|| ApiError::not_found("session not found"))?;
    if project.id != project_id || project.owner_subject != user.0 {
        return Err(ApiError::not_found("session not found"));
    }
    if let Some(title) = request.title.as_deref() {
        validate_title(title)?;
        state
            .store
            .update_session_title(&session_id, title.trim())?;
    }
    let metadata = state.store.update_workspace_session_metadata(
        &session_id,
        request.pinned,
        request.sort_order,
        request.archived,
    )?;
    state.record_workspace_update(&user.0, "workspace.session.updated", json!({"project_id": project_id, "session": state.store.get_session(&session_id)?, "metadata": metadata}))?;
    Ok(Json(
        json!({"session": state.store.get_session(&session_id)?, "metadata": metadata}),
    ))
}

pub async fn update_preference(
    State(state): State<AppState>,
    Extension(user): Extension<CurrentUser>,
    Path(device_id): Path<String>,
    Json(request): Json<PreferenceRequest>,
) -> Result<Json<Value>, ApiError> {
    if !valid_identifier(&device_id) || device_id.len() < 8 {
        return Err(ApiError::bad_request("invalid device identifier"));
    }
    if !matches!(
        request.language_view.as_str(),
        "bilingual" | "source" | "translation"
    ) {
        return Err(ApiError::bad_request("invalid language view"));
    }
    if let Some(project_id) = request.active_project_id.as_deref() {
        owned_project(&state, &user.0, project_id)?;
    }
    if let Some(session_id) = request.active_session_id.as_deref() {
        let project = state
            .store
            .project_for_session(session_id)?
            .ok_or_else(|| ApiError::not_found("session not found"))?;
        if project.owner_subject != user.0
            || request.active_project_id.as_deref() != Some(project.id.as_str())
        {
            return Err(ApiError::bad_request(
                "active session does not belong to active project",
            ));
        }
    }
    let preference = state
        .store
        .upsert_workspace_preference(&WorkspaceDevicePreferenceRecord {
            owner_subject: user.0,
            device_id,
            active_project_id: request.active_project_id,
            active_session_id: request.active_session_id,
            language_view: request.language_view,
            sidebar_collapsed: request.sidebar_collapsed,
            updated_at: Utc::now(),
        })?;
    Ok(Json(serde_json::to_value(preference)?))
}

pub async fn get_ai_policy(
    State(state): State<AppState>,
    Extension(user): Extension<CurrentUser>,
    Path(project_id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    owned_project(&state, &user.0, &project_id)?;
    Ok(Json(serde_json::to_value(
        state.store.get_project_ai_policy(&project_id)?,
    )?))
}

pub async fn update_ai_policy(
    State(state): State<AppState>,
    Extension(user): Extension<CurrentUser>,
    Path(project_id): Path<String>,
    Json(request): Json<AiPolicyRequest>,
) -> Result<Json<Value>, ApiError> {
    owned_project(&state, &user.0, &project_id)?;
    if request.cloud_enabled || !request.allowed_modalities.is_empty() {
        return Err(ApiError::bad_request(
            "cloud access requires a separate per-item authorization",
        ));
    }
    let policy = state
        .store
        .update_project_ai_policy(&project_id, false, &[])?;
    state.record_project_update(
        &project_id,
        None,
        "project.ai_policy.updated",
        json!({"cloud_enabled": false, "allowed_modalities": []}),
    )?;
    Ok(Json(serde_json::to_value(policy)?))
}

pub async fn stream_workspace(
    State(state): State<AppState>,
    Extension(user): Extension<CurrentUser>,
    headers: HeaderMap,
) -> Result<Sse<impl futures_util::Stream<Item = Result<Event, Infallible>>>, ApiError> {
    let cursor = headers
        .get("last-event-id")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<i64>().ok())
        .unwrap_or(0);
    let history = state.store.list_workspace_updates_after(&user.0, cursor)?;
    let mut receiver = state.workspace_updates.subscribe();
    let subject = user.0;
    let output = stream! {
        for update in history { yield Ok(workspace_sse_event(&update)); }
        loop {
            match receiver.recv().await {
                Ok(update) if update.owner_subject == subject => yield Ok(workspace_sse_event(&update)),
                Ok(_) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => break,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    };
    Ok(Sse::new(output).keep_alive(KeepAlive::new().interval(Duration::from_secs(15))))
}

fn workspace_sse_event(update: &WorkspaceUpdateRecord) -> Event {
    Event::default()
        .id(update.cursor.to_string())
        .event(&update.update_type)
        .json_data(update)
        .unwrap_or_else(|_| Event::default().event("serialization.error"))
}

fn validate_title(value: &str) -> Result<(), ApiError> {
    (!value.trim().is_empty() && value.chars().count() <= 160)
        .then_some(())
        .ok_or_else(|| {
            ApiError::bad_request("title is required and must not exceed 160 characters")
        })
}

fn owned_folder(
    state: &AppState,
    subject: &str,
    folder_id: &str,
) -> Result<aialra_event_store::WorkspaceFolderRecord, ApiError> {
    state
        .store
        .get_workspace_folder(folder_id)?
        .filter(|folder| folder.owner_subject == subject)
        .ok_or_else(|| ApiError::not_found("workspace folder not found"))
}

fn validate_parent(
    state: &AppState,
    subject: &str,
    moving_id: Option<&str>,
    parent_id: Option<&str>,
) -> Result<(), ApiError> {
    let folders = state.store.list_workspace_folders(subject)?;
    let parents = folders
        .iter()
        .map(|folder| (folder.id.as_str(), folder.parent_id.as_deref()))
        .collect::<HashMap<_, _>>();
    let mut current = parent_id;
    let mut depth = 1usize;
    while let Some(folder_id) = current {
        if Some(folder_id) == moving_id {
            return Err(ApiError::bad_request(
                "workspace folder cycle is not allowed",
            ));
        }
        let folder = folders
            .iter()
            .find(|folder| folder.id == folder_id && folder.archived_at.is_none())
            .ok_or_else(|| ApiError::not_found("parent workspace folder not found"))?;
        depth += 1;
        if depth > MAX_FOLDER_DEPTH {
            return Err(ApiError::bad_request(
                "workspace folder depth exceeds five levels",
            ));
        }
        current = parents.get(folder.id.as_str()).copied().flatten();
    }
    Ok(())
}

fn ensure_folder_empty(state: &AppState, subject: &str, folder_id: &str) -> Result<(), ApiError> {
    if state
        .store
        .list_workspace_folders(subject)?
        .iter()
        .any(|folder| {
            folder.parent_id.as_deref() == Some(folder_id) && folder.archived_at.is_none()
        })
    {
        return Err(ApiError::conflict("move or archive child folders first"));
    }
    if state
        .store
        .list_workspace_project_placements(subject)?
        .iter()
        .any(|placement| {
            placement.folder_id.as_deref() == Some(folder_id) && placement.archived_at.is_none()
        })
    {
        return Err(ApiError::conflict("move or archive projects first"));
    }
    Ok(())
}

fn enqueue_owner_readweave_reconcile(state: &AppState, subject: &str) -> Result<(), ApiError> {
    for project in state.store.list_projects(subject)? {
        crate::readweave::enqueue_manual_reconcile(state, &project.id)?;
    }
    Ok(())
}
