//! Owner-scoped workspace tree, device navigation, and project AI policy endpoints.

use crate::app::{ApiError, AppState};
use crate::identity::{CurrentUser, valid_identifier};
use crate::projects::owned_project;
use aialra_event_store::{
    NewWorkspaceFolder, NewWorkspaceTrashItem, WorkspaceDevicePreferenceRecord,
    WorkspaceTrashItemRecord, WorkspaceUpdateRecord,
};
use async_stream::stream;
use axum::Json;
use axum::extract::{Extension, Path, Query, State};
use axum::http::HeaderMap;
use axum::response::sse::{Event, KeepAlive, Sse};
use chrono::Utc;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
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

#[derive(Debug, Deserialize)]
pub struct PurgeRequest {
    confirmation: String,
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
        "trash": state.store.list_workspace_trash(&user.0)?,
        "preference": preference,
    })))
}

pub async fn list_trash(
    State(state): State<AppState>,
    Extension(user): Extension<CurrentUser>,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(
        json!({"items": state.store.list_workspace_trash(&user.0)?}),
    ))
}

pub async fn trash_entity(
    State(state): State<AppState>,
    Extension(user): Extension<CurrentUser>,
    Path((entity_type, entity_id)): Path<(String, String)>,
) -> Result<Json<Value>, ApiError> {
    let (items, _, _, session_ids) =
        workspace_selection(&state, &user.0, &entity_type, &entity_id)?;
    if items.is_empty() {
        return Err(ApiError::not_found("workspace item not found"));
    }
    ensure_sessions_can_move_to_trash(&state, &session_ids)?;
    if state
        .store
        .list_workspace_trash(&user.0)?
        .iter()
        .any(|item| item.entity_type == entity_type && item.entity_id == entity_id)
    {
        return Ok(Json(json!({"accepted": true, "already_in_trash": true})));
    }
    state.store.trash_workspace_items(&items)?;
    if state
        .record_workspace_update(
            &user.0,
            "workspace.trash.moved",
            json!({"entity_type": entity_type, "entity_id": entity_id}),
        )
        .is_err()
    {
        tracing::warn!(
            error_kind = "workspace_trash_notification_failed",
            "workspace trash move committed without notification"
        );
    }
    Ok(Json(json!({"accepted": true})))
}

pub async fn restore_entity(
    State(state): State<AppState>,
    Extension(user): Extension<CurrentUser>,
    Path((entity_type, entity_id)): Path<(String, String)>,
) -> Result<Json<Value>, ApiError> {
    let records = trash_selection(&state, &user.0, &entity_type, &entity_id)?;
    if records.is_empty() {
        return Err(ApiError::not_found("recycle-bin item not found"));
    }
    state.store.restore_workspace_items(&user.0, &records)?;
    if state
        .record_workspace_update(
            &user.0,
            "workspace.trash.restored",
            json!({"entity_type": entity_type, "entity_id": entity_id}),
        )
        .is_err()
    {
        tracing::warn!(
            error_kind = "workspace_trash_notification_failed",
            "workspace trash restore committed without notification"
        );
    }
    Ok(Json(json!({"accepted": true})))
}

pub async fn purge_entity(
    State(state): State<AppState>,
    Extension(user): Extension<CurrentUser>,
    Path((entity_type, entity_id)): Path<(String, String)>,
    Json(request): Json<PurgeRequest>,
) -> Result<Json<Value>, ApiError> {
    if request.confirmation != "永久删除" {
        return Err(ApiError::bad_request("请输入“永久删除”确认彻底删除"));
    }
    let records = trash_selection(&state, &user.0, &entity_type, &entity_id)?;
    if records.is_empty() {
        return Err(ApiError::not_found("recycle-bin item not found"));
    }
    for item in &records {
        if item.entity_type != "session" {
            continue;
        }
        let session = state
            .store
            .get_session(&item.entity_id)?
            .ok_or_else(|| ApiError::not_found("session not found"))?;
        if matches!(
            session.state,
            aialra_core_domain::SessionState::Recording
                | aialra_core_domain::SessionState::Degraded
                | aialra_core_domain::SessionState::Stopping
                | aialra_core_domain::SessionState::Processing
        ) {
            return Err(ApiError::conflict("请先停止录音并等待处理队列排空"));
        }
        let counts = state.store.model_queue_counts(Some(&item.entity_id))?;
        if counts.queued + counts.leased > 0 {
            return Err(ApiError::conflict("请先等待课程处理队列排空"));
        }
    }
    crate::readweave::purge_workspace_objects(&state, &records).await?;
    let folder_ids = records
        .iter()
        .filter(|item| item.entity_type == "folder")
        .map(|item| item.entity_id.clone())
        .collect::<Vec<_>>();
    let project_ids = records
        .iter()
        .filter(|item| item.entity_type == "project")
        .map(|item| item.entity_id.clone())
        .collect::<Vec<_>>();
    let session_ids = records
        .iter()
        .filter(|item| item.entity_type == "session")
        .map(|item| item.entity_id.clone())
        .collect::<Vec<_>>();
    let object_hashes =
        state
            .store
            .purge_workspace_items(&user.0, &folder_ids, &project_ids, &session_ids)?;
    let mut removed_objects = 0usize;
    for object_hash in object_hashes {
        if !state.store.object_hash_is_referenced(&object_hash)?
            && state.objects.remove(&object_hash)?
        {
            removed_objects += 1;
        }
    }
    if state
        .record_workspace_update(
            &user.0,
            "workspace.trash.purged",
            json!({"entity_type": entity_type, "entity_id": entity_id, "removed_objects": removed_objects}),
        )
        .is_err()
    {
        tracing::warn!(error_kind = "workspace_trash_notification_failed", "workspace purge committed without notification");
    }
    Ok(Json(
        json!({"accepted": true, "removed_objects": removed_objects}),
    ))
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
        let folder = owned_folder(&state, &user.0, folder_id)?;
        if folder.archived_at.is_some() {
            return Err(ApiError::conflict(
                "an archived workspace folder cannot receive active projects",
            ));
        }
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
    let mut receiver = state.workspace_updates.subscribe();
    let history = state.store.list_workspace_updates_after(&user.0, cursor)?;
    // The broadcast subscription precedes the durable snapshot.  A cursor
    // watermark prevents updates included in that snapshot from being emitted
    // a second time when the subscription catches up.
    let watermark = history.last().map(|update| update.cursor).unwrap_or(cursor);
    let subject = user.0;
    let output = stream! {
        for update in history { yield Ok(workspace_sse_event(&update)); }
        loop {
            match receiver.recv().await {
                Ok(update) if update.owner_subject == subject && update.cursor > watermark => yield Ok(workspace_sse_event(&update)),
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

fn ensure_sessions_can_move_to_trash(
    state: &AppState,
    session_ids: &[String],
) -> Result<(), ApiError> {
    for session_id in session_ids {
        let Some(session) = state.store.get_session(session_id)? else {
            continue;
        };
        if matches!(
            session.state,
            aialra_core_domain::SessionState::Recording
                | aialra_core_domain::SessionState::Degraded
                | aialra_core_domain::SessionState::Stopping
                | aialra_core_domain::SessionState::Processing
        ) {
            return Err(ApiError::conflict("请先停止录音并等待处理完成"));
        }
    }
    Ok(())
}

fn enqueue_owner_readweave_reconcile(state: &AppState, subject: &str) -> Result<(), ApiError> {
    for project in state.store.list_projects(subject)? {
        crate::readweave::enqueue_manual_reconcile(state, &project.id)?;
    }
    Ok(())
}

type WorkspaceSelection = (
    Vec<NewWorkspaceTrashItem>,
    Vec<String>,
    Vec<String>,
    Vec<String>,
);

fn workspace_selection(
    state: &AppState,
    subject: &str,
    entity_type: &str,
    entity_id: &str,
) -> Result<WorkspaceSelection, ApiError> {
    let folders = state.store.list_workspace_folders(subject)?;
    let projects = state.store.list_projects(subject)?;
    let placements = state.store.list_workspace_project_placements(subject)?;
    let sessions = state.store.list_sessions_for_owner(subject)?;
    let metadata = state.store.list_workspace_session_metadata(subject)?;
    let mut session_projects = HashMap::new();
    for project in &projects {
        for session in state.store.list_project_sessions(&project.id)? {
            session_projects.insert(session.id, project.id.clone());
        }
    }

    let mut folder_ids = Vec::new();
    let mut project_ids = Vec::new();
    let mut session_ids = Vec::new();
    match entity_type {
        "folder" => {
            if !folders.iter().any(|folder| folder.id == entity_id) {
                return Err(ApiError::not_found("workspace folder not found"));
            }
            folder_ids.push(entity_id.to_owned());
            let mut index = 0;
            while index < folder_ids.len() {
                let parent = folder_ids[index].clone();
                let child_folder_ids = folders
                    .iter()
                    .filter(|folder| folder.parent_id.as_deref() == Some(parent.as_str()))
                    .filter(|folder| !folder_ids.contains(&folder.id))
                    .map(|folder| folder.id.clone())
                    .collect::<Vec<_>>();
                folder_ids.extend(child_folder_ids);
                index += 1;
            }
            let folder_set = folder_ids.iter().collect::<HashSet<_>>();
            project_ids.extend(
                placements
                    .iter()
                    .filter(|placement| {
                        placement
                            .folder_id
                            .as_ref()
                            .is_some_and(|folder| folder_set.contains(folder))
                    })
                    .map(|placement| placement.project_id.clone()),
            );
            let project_set = project_ids.iter().collect::<HashSet<_>>();
            session_ids.extend(
                session_projects
                    .iter()
                    .filter(|(_, project)| project_set.contains(project))
                    .map(|(session, _)| session.clone()),
            );
        }
        "project" => {
            if !projects.iter().any(|project| project.id == entity_id) {
                return Err(ApiError::not_found("project not found"));
            }
            project_ids.push(entity_id.to_owned());
            session_ids.extend(
                session_projects
                    .iter()
                    .filter(|(_, project)| project.as_str() == entity_id)
                    .map(|(session, _)| session.clone()),
            );
        }
        "session" => {
            if !sessions.iter().any(|session| session.id == entity_id) {
                return Err(ApiError::not_found("session not found"));
            }
            session_ids.push(entity_id.to_owned());
        }
        _ => return Err(ApiError::bad_request("workspace item type is invalid")),
    }

    let folder_set = folder_ids.iter().collect::<HashSet<_>>();
    let project_set = project_ids.iter().collect::<HashSet<_>>();
    let mut items = Vec::new();
    items.extend(
        folders
            .iter()
            .filter(|folder| folder_set.contains(&folder.id))
            .map(|folder| NewWorkspaceTrashItem {
                owner_subject: subject.to_owned(),
                entity_type: "folder".to_owned(),
                entity_id: folder.id.clone(),
                original_parent_id: folder.parent_id.clone(),
                original_project_id: None,
                original_sort_order: folder.sort_order,
                original_pinned: false,
            }),
    );
    items.extend(
        placements
            .iter()
            .filter(|placement| project_set.contains(&placement.project_id))
            .map(|placement| NewWorkspaceTrashItem {
                owner_subject: subject.to_owned(),
                entity_type: "project".to_owned(),
                entity_id: placement.project_id.clone(),
                original_parent_id: placement.folder_id.clone(),
                original_project_id: None,
                original_sort_order: placement.sort_order,
                original_pinned: false,
            }),
    );
    for session_id in &session_ids {
        let session_metadata = metadata.iter().find(|item| item.session_id == *session_id);
        items.push(NewWorkspaceTrashItem {
            owner_subject: subject.to_owned(),
            entity_type: "session".to_owned(),
            entity_id: session_id.clone(),
            original_parent_id: None,
            original_project_id: session_projects.get(session_id).cloned(),
            original_sort_order: session_metadata.map_or(0, |item| item.sort_order),
            original_pinned: session_metadata.is_some_and(|item| item.pinned),
        });
    }
    Ok((items, folder_ids, project_ids, session_ids))
}

fn trash_selection(
    state: &AppState,
    subject: &str,
    entity_type: &str,
    entity_id: &str,
) -> Result<Vec<WorkspaceTrashItemRecord>, ApiError> {
    if !matches!(entity_type, "folder" | "project" | "session") {
        return Err(ApiError::bad_request("workspace item type is invalid"));
    }
    let all = state.store.list_workspace_trash(subject)?;
    if !all
        .iter()
        .any(|item| item.entity_type == entity_type && item.entity_id == entity_id)
    {
        return Ok(Vec::new());
    }
    let mut folders = HashSet::new();
    let mut projects = HashSet::new();
    let mut sessions = HashSet::new();
    match entity_type {
        "folder" => {
            folders.insert(entity_id.to_owned());
            let mut changed = true;
            while changed {
                changed = false;
                for item in &all {
                    if item.entity_type == "folder"
                        && item
                            .original_parent_id
                            .as_ref()
                            .is_some_and(|parent| folders.contains(parent))
                        && folders.insert(item.entity_id.clone())
                    {
                        changed = true;
                    }
                }
            }
            for item in &all {
                if item.entity_type == "project"
                    && item
                        .original_parent_id
                        .as_ref()
                        .is_some_and(|parent| folders.contains(parent))
                {
                    projects.insert(item.entity_id.clone());
                }
            }
            for item in &all {
                if item.entity_type == "session"
                    && item
                        .original_project_id
                        .as_ref()
                        .is_some_and(|project| projects.contains(project))
                {
                    sessions.insert(item.entity_id.clone());
                }
            }
        }
        "project" => {
            projects.insert(entity_id.to_owned());
            for item in &all {
                if item.entity_type == "session"
                    && item.original_project_id.as_deref() == Some(entity_id)
                {
                    sessions.insert(item.entity_id.clone());
                }
            }
        }
        "session" => {
            sessions.insert(entity_id.to_owned());
        }
        _ => unreachable!(),
    }
    Ok(all
        .into_iter()
        .filter(|item| match item.entity_type.as_str() {
            "folder" => folders.contains(&item.entity_id),
            "project" => projects.contains(&item.entity_id),
            "session" => sessions.contains(&item.entity_id),
            _ => false,
        })
        .collect())
}
