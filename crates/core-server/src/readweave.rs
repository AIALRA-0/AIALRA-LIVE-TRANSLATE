//! ReadWeave is an eventually consistent projection of append-only AIALRA events

use crate::app::{ApiError, AppState};
use aialra_event_store::{
    ConnectorJobRecord, NewConnectorJob, RemoteObjectDetailsRecord, RemoteObjectMapRecord,
    WorkspaceFolderRecord,
};
use anyhow::{Context, Result, bail};
use chrono::Utc;
use reqwest::Client;
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::time::Duration;
use uuid::Uuid;

const CONNECTOR: &str = "readweave";
const WORKER_ID: &str = "readweave-projector";
const MANAGED_BEGIN: &str = "<!-- AIALRA:BEGIN -->";
const MANAGED_END: &str = "<!-- AIALRA:END -->";

#[derive(Clone)]
struct ReadWeaveClient {
    http: Client,
    base_url: String,
    token: String,
    root_parent_id: String,
    public_url: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CreateNoteResponse {
    note: CreatedNote,
    branch: CreatedBranch,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreatedNote {
    note_id: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreatedBranch {
    branch_id: String,
    note_id: String,
    parent_note_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RemoteNote {
    parent_branch_ids: Vec<String>,
}

#[derive(Debug)]
struct CreatedPlacement {
    note_id: String,
    branch_id: String,
}

enum ProjectionError {
    Conflict(anyhow::Error),
    Retry(anyhow::Error),
}

impl ReadWeaveClient {
    fn from_env() -> Option<Self> {
        let base_url = std::env::var("AIALRA_READWEAVE_BASE_URL")
            .ok()?
            .trim_end_matches('/')
            .to_owned();
        let token = std::env::var("AIALRA_READWEAVE_ETAPI_TOKEN").ok()?;
        if base_url.is_empty() || token.is_empty() {
            return None;
        }
        Some(Self {
            http: Client::builder()
                .timeout(Duration::from_secs(20))
                .build()
                .ok()?,
            base_url,
            token,
            root_parent_id: std::env::var("AIALRA_READWEAVE_ROOT_NOTE_ID")
                .unwrap_or_else(|_| "root".to_owned()),
            public_url: std::env::var("AIALRA_READWEAVE_PUBLIC_URL")
                .ok()
                .map(|value| value.trim_end_matches('/').to_owned()),
        })
    }

    fn endpoint(&self, path: &str) -> String {
        format!("{}/etapi{}", self.base_url, path)
    }

    async fn create_note(
        &self,
        parent: &str,
        title: &str,
        content: &str,
    ) -> Result<CreatedPlacement> {
        let response = self.http.post(self.endpoint("/create-note"))
            .header("Authorization", &self.token)
            .json(&json!({"parentNoteId": parent, "title": title, "type": "text", "content": content}))
            .send().await.context("send ReadWeave create-note")?
            .error_for_status().context("ReadWeave create-note rejected")?
            .json::<CreateNoteResponse>().await.context("decode ReadWeave create-note")?;
        Ok(CreatedPlacement {
            note_id: response.note.note_id,
            branch_id: response.branch.branch_id,
        })
    }

    async fn patch_title(&self, note_id: &str, title: &str) -> Result<()> {
        self.http
            .patch(self.endpoint(&format!("/notes/{note_id}")))
            .header("Authorization", &self.token)
            .json(&json!({"title": title}))
            .send()
            .await
            .context("send ReadWeave title update")?
            .error_for_status()
            .context("ReadWeave title update rejected")?;
        Ok(())
    }

    async fn create_branch(
        &self,
        note_id: &str,
        parent_id: &str,
        position: i64,
    ) -> Result<CreatedBranch> {
        self.http
            .post(self.endpoint("/branches"))
            .header("Authorization", &self.token)
            .json(&json!({
                "noteId": note_id,
                "parentNoteId": parent_id,
                "prefix": "",
                "notePosition": position,
                "isExpanded": false
            }))
            .send()
            .await
            .context("send ReadWeave branch creation")?
            .error_for_status()
            .context("ReadWeave branch creation rejected")?
            .json::<CreatedBranch>()
            .await
            .context("decode ReadWeave branch")
    }

    async fn delete_branch(&self, branch_id: &str) -> Result<()> {
        self.http
            .delete(self.endpoint(&format!("/branches/{branch_id}")))
            .header("Authorization", &self.token)
            .send()
            .await
            .context("send ReadWeave branch deletion")?
            .error_for_status()
            .context("ReadWeave branch deletion rejected")?;
        Ok(())
    }

    async fn find_branch(&self, note_id: &str, parent_id: &str) -> Result<Option<CreatedBranch>> {
        let note = self
            .http
            .get(self.endpoint(&format!("/notes/{note_id}")))
            .header("Authorization", &self.token)
            .send()
            .await
            .context("send ReadWeave note read")?
            .error_for_status()
            .context("ReadWeave note read rejected")?
            .json::<RemoteNote>()
            .await
            .context("decode ReadWeave note")?;
        for branch_id in note.parent_branch_ids {
            let branch = self
                .http
                .get(self.endpoint(&format!("/branches/{branch_id}")))
                .header("Authorization", &self.token)
                .send()
                .await
                .context("send ReadWeave branch read")?
                .error_for_status()
                .context("ReadWeave branch read rejected")?
                .json::<CreatedBranch>()
                .await
                .context("decode ReadWeave branch")?;
            if branch.note_id == note_id && branch.parent_note_id == parent_id {
                return Ok(Some(branch));
            }
        }
        Ok(None)
    }

    async fn get_content(&self, note_id: &str) -> Result<String> {
        self.http
            .get(self.endpoint(&format!("/notes/{note_id}/content")))
            .header("Authorization", &self.token)
            .send()
            .await
            .context("send ReadWeave content read")?
            .error_for_status()
            .context("ReadWeave content read rejected")?
            .text()
            .await
            .context("decode ReadWeave content")
    }

    async fn put_content(&self, note_id: &str, content: String) -> Result<()> {
        self.http
            .post(self.endpoint(&format!("/notes/{note_id}/revision")))
            .header("Authorization", &self.token)
            .send()
            .await
            .context("send ReadWeave revision")?
            .error_for_status()
            .context("ReadWeave revision rejected")?;
        self.http
            .put(self.endpoint(&format!("/notes/{note_id}/content")))
            .header("Authorization", &self.token)
            .header("Content-Type", "text/plain; charset=utf-8")
            .body(content)
            .send()
            .await
            .context("send ReadWeave content update")?
            .error_for_status()
            .context("ReadWeave content update rejected")?;
        Ok(())
    }
}

pub fn configured() -> bool {
    [
        std::env::var("AIALRA_READWEAVE_BASE_URL").ok(),
        std::env::var("AIALRA_READWEAVE_ETAPI_TOKEN").ok(),
    ]
    .into_iter()
    .all(|value| value.is_some_and(|value| !value.trim().is_empty()))
}

pub fn enqueue_projection(state: &AppState, session_id: &str, immediate: bool) -> Result<()> {
    if !configured() {
        return Ok(());
    }
    let Some(project) = state.store.project_for_session(session_id)? else {
        return Ok(());
    };
    let bucket = if immediate {
        format!("final-{}", Uuid::now_v7().simple())
    } else {
        (Utc::now().timestamp() / 30).to_string()
    };
    state.store.enqueue_connector_job(&NewConnectorJob {
        id: format!("connector_{}", Uuid::now_v7().simple()),
        project_id: project.id,
        session_id: Some(session_id.to_owned()),
        connector: CONNECTOR.to_owned(),
        job_type: "reconcile_session".to_owned(),
        payload: json!({"session_id": session_id}),
        idempotency_key: format!("readweave:{session_id}:{bucket}"),
        delay_seconds: if immediate { 0 } else { 30 },
    })?;
    Ok(())
}

pub fn enqueue_manual_reconcile(state: &AppState, project_id: &str) -> Result<()> {
    for session in state.store.list_project_sessions(project_id)? {
        state.store.enqueue_connector_job(&NewConnectorJob {
            id: format!("connector_{}", Uuid::now_v7().simple()),
            project_id: project_id.to_owned(),
            session_id: Some(session.id.clone()),
            connector: CONNECTOR.to_owned(),
            job_type: "reconcile_session".to_owned(),
            payload: json!({"session_id": session.id}),
            idempotency_key: format!(
                "readweave:{}:manual:{}",
                session.id,
                Uuid::now_v7().simple()
            ),
            delay_seconds: 0,
        })?;
    }
    Ok(())
}

pub async fn run_connector_loop(state: AppState) {
    // Deployment configuration is immutable for the process lifetime, and one client keeps a
    // bounded connection pool instead of rebuilding TLS state on every five-second poll.
    let client = ReadWeaveClient::from_env();
    let mut timer = tokio::time::interval(Duration::from_secs(5));
    loop {
        timer.tick().await;
        let Some(client) = client.as_ref() else {
            continue;
        };
        let job = match state.store.lease_connector_job(WORKER_ID, 60) {
            Ok(Some(job)) => job,
            Ok(None) => continue,
            Err(error) => {
                tracing::warn!(error_kind = "connector_lease_failed", error = %error, "ReadWeave connector lease failed");
                continue;
            }
        };
        match project_job(&state, client, &job).await {
            Ok(()) => {
                let _ = state.store.complete_connector_job(&job.id, WORKER_ID);
                let _ = state.record_project_update(
                    &job.project_id,
                    job.session_id.as_deref(),
                    "readweave.synced",
                    json!({"job_id": job.id}),
                );
            }
            Err(ProjectionError::Conflict(error)) => {
                let _ = state.store.retry_connector_job(
                    &job.id,
                    WORKER_ID,
                    "managed_region_conflict",
                    3_600,
                    true,
                );
                let _ = state.record_project_update(
                    &job.project_id,
                    job.session_id.as_deref(),
                    "readweave.conflict",
                    json!({"job_id": job.id}),
                );
                tracing::warn!(job_id = %job.id, error_kind = "managed_region_conflict", error = %error, "ReadWeave projection stopped on conflict");
            }
            Err(ProjectionError::Retry(error)) => {
                let delay = 2_i64.saturating_pow(job.attempts.min(5)).min(30);
                let _ = state.store.retry_connector_job(
                    &job.id,
                    WORKER_ID,
                    "readweave_unavailable",
                    delay,
                    false,
                );
                tracing::warn!(job_id = %job.id, error_kind = "readweave_unavailable", error = %error, "ReadWeave projection will retry");
            }
        }
        release_projection_memory();
    }
}

#[cfg(all(target_os = "linux", target_env = "gnu"))]
fn release_projection_memory() {
    // A live transcript is rebuilt as one growing ReadWeave document every 30 seconds. The
    // temporary event vectors and HTML strings are short-lived, but glibc can retain their freed
    // arenas indefinitely. Returning free pages after each projection keeps Core RSS tied to the
    // active working set rather than lecture duration.
    unsafe {
        libc::malloc_trim(0);
    }
}

#[cfg(not(all(target_os = "linux", target_env = "gnu")))]
fn release_projection_memory() {}

struct NoteSeed<'a> {
    object_type: &'a str,
    local_id: &'a str,
    parent_id: &'a str,
    title: &'a str,
    initial_content: String,
    position: i64,
}

async fn project_job(
    state: &AppState,
    client: &ReadWeaveClient,
    job: &ConnectorJobRecord,
) -> Result<(), ProjectionError> {
    let session_id = job
        .session_id
        .as_deref()
        .ok_or_else(|| ProjectionError::Retry(anyhow::anyhow!("connector job has no session")))?;
    let project = state
        .store
        .get_project(&job.project_id)
        .map_err(ProjectionError::Retry)?
        .ok_or_else(|| ProjectionError::Retry(anyhow::anyhow!("project not found")))?;
    let session = state
        .store
        .get_session(session_id)
        .map_err(ProjectionError::Retry)?
        .ok_or_else(|| ProjectionError::Retry(anyhow::anyhow!("session not found")))?;
    let events = state
        .store
        .list_events(session_id)
        .map_err(ProjectionError::Retry)?;

    let root = ensure_note(
        state,
        client,
        NoteSeed {
            object_type: "root",
            local_id: "aialra-root",
            parent_id: &client.root_parent_id,
            title: "AIALRA 课程",
            initial_content: managed_region("<p>AIALRA 自动课程笔记</p>"),
            position: 100,
        },
    )
    .await?;
    let project_parent =
        ensure_workspace_path(state, client, &project.owner_subject, &project.id, &root).await?;
    let project_note = ensure_note(
        state,
        client,
        NoteSeed {
            object_type: "project",
            local_id: &project.id,
            parent_id: &project_parent,
            title: &project.title,
            initial_content: managed_region("<p>课程项目</p>"),
            position: 100,
        },
    )
    .await?;
    let session_title = format!(
        "{} {}",
        session.created_at.format("%Y-%m-%d %H%M"),
        session.title
    );
    let session_note = ensure_note(
        state,
        client,
        NoteSeed {
            object_type: "session",
            local_id: &session.id,
            parent_id: &project_note,
            title: &session_title,
            initial_content: managed_region("<p>课程会话</p>"),
            position: 100,
        },
    )
    .await?;
    let overview_id = format!("{}:overview", session.id);
    let overview = ensure_note(
        state,
        client,
        NoteSeed {
            object_type: "section",
            local_id: &overview_id,
            parent_id: &session_note,
            title: "00 课程概览",
            initial_content: managed_region(""),
            position: 10,
        },
    )
    .await?;
    let transcript_id = format!("{}:transcript", session.id);
    let transcript = ensure_note(
        state,
        client,
        NoteSeed {
            object_type: "section",
            local_id: &transcript_id,
            parent_id: &session_note,
            title: "01 实时转写与翻译",
            initial_content: managed_region(""),
            position: 20,
        },
    )
    .await?;
    let explanations_id = format!("{}:explanations", session.id);
    let explanations = ensure_note(
        state,
        client,
        NoteSeed {
            object_type: "section",
            local_id: &explanations_id,
            parent_id: &session_note,
            title: "02 生僻词与补充解释",
            initial_content: managed_region(""),
            position: 30,
        },
    )
    .await?;
    let assets_id = format!("{}:assets", session.id);
    let assets = ensure_note(
        state,
        client,
        NoteSeed {
            object_type: "section",
            local_id: &assets_id,
            parent_id: &session_note,
            title: "03 课件与证据索引",
            initial_content: managed_region(""),
            position: 40,
        },
    )
    .await?;
    let user_notes_id = format!("{}:user", session.id);
    ensure_note(
        state,
        client,
        NoteSeed {
            object_type: "user_notes",
            local_id: &user_notes_id,
            parent_id: &session_note,
            title: "99 我的笔记",
            initial_content: "<p>这里的内容只由你编辑，AIALRA 不会覆盖</p>".to_owned(),
            position: 990,
        },
    )
    .await?;

    update_managed_note(
        state,
        client,
        "section",
        &format!("{}:overview", session.id),
        &overview,
        &render_overview(&session, &events),
    )
    .await?;
    update_managed_note(
        state,
        client,
        "section",
        &format!("{}:transcript", session.id),
        &transcript,
        &render_transcript(&events),
    )
    .await?;
    update_managed_note(
        state,
        client,
        "section",
        &format!("{}:explanations", session.id),
        &explanations,
        &render_explanations(&events),
    )
    .await?;
    update_managed_note(
        state,
        client,
        "section",
        &format!("{}:assets", session.id),
        &assets,
        &render_assets(&events),
    )
    .await?;
    Ok(())
}

async fn ensure_workspace_path(
    state: &AppState,
    client: &ReadWeaveClient,
    owner_subject: &str,
    project_id: &str,
    root_id: &str,
) -> Result<String, ProjectionError> {
    let Some(folder_id) = state
        .store
        .get_workspace_project_placement(project_id)
        .map_err(ProjectionError::Retry)?
        .and_then(|placement| placement.folder_id)
    else {
        return Ok(root_id.to_owned());
    };
    let folders = state
        .store
        .list_workspace_folders(owner_subject)
        .map_err(ProjectionError::Retry)?;
    let mut chain = Vec::<WorkspaceFolderRecord>::new();
    let mut cursor = Some(folder_id);
    while let Some(folder_id) = cursor {
        let folder = folders
            .iter()
            .find(|folder| folder.id == folder_id && folder.archived_at.is_none())
            .cloned()
            .ok_or_else(|| {
                ProjectionError::Retry(anyhow::anyhow!(
                    "workspace folder missing during ReadWeave projection"
                ))
            })?;
        cursor = folder.parent_id.clone();
        chain.push(folder);
        if chain.len() > 5 {
            return Err(ProjectionError::Retry(anyhow::anyhow!(
                "workspace folder depth exceeds projection limit"
            )));
        }
    }
    chain.reverse();
    let mut parent = root_id.to_owned();
    for folder in chain {
        parent = ensure_note(
            state,
            client,
            NoteSeed {
                object_type: "workspace_folder",
                local_id: &folder.id,
                parent_id: &parent,
                title: &folder.title,
                initial_content: managed_region("<p>AIALRA 工作区文件夹</p>"),
                position: folder
                    .sort_order
                    .saturating_mul(10)
                    .clamp(0, i32::MAX as i64),
            },
        )
        .await?;
    }
    Ok(parent)
}

async fn ensure_note(
    state: &AppState,
    client: &ReadWeaveClient,
    seed: NoteSeed<'_>,
) -> Result<String, ProjectionError> {
    let NoteSeed {
        object_type,
        local_id,
        parent_id,
        title,
        initial_content,
        position,
    } = seed;
    if let Some(record) = state
        .store
        .get_remote_object_map(CONNECTOR, object_type, local_id)
        .map_err(ProjectionError::Retry)?
    {
        client
            .patch_title(&record.remote_id, title)
            .await
            .map_err(ProjectionError::Retry)?;
        ensure_parent_branch(state, client, &record, parent_id, position).await?;
        return Ok(record.remote_id);
    }
    let placement = client
        .create_note(parent_id, title, &initial_content)
        .await
        .map_err(ProjectionError::Retry)?;
    let now = Utc::now();
    state
        .store
        .upsert_remote_object_map(&RemoteObjectMapRecord {
            connector: CONNECTOR.to_owned(),
            object_type: object_type.to_owned(),
            local_id: local_id.to_owned(),
            remote_id: placement.note_id.clone(),
            remote_parent_id: Some(parent_id.to_owned()),
            content_hash: Some(content_hash(&initial_content)),
            created_at: now,
            updated_at: now,
        })
        .map_err(ProjectionError::Retry)?;
    state
        .store
        .upsert_remote_object_details(&RemoteObjectDetailsRecord {
            connector: CONNECTOR.to_owned(),
            object_type: object_type.to_owned(),
            local_id: local_id.to_owned(),
            remote_branch_id: Some(placement.branch_id),
            node_type: Some(object_type.to_owned()),
            last_synced_version: None,
            updated_at: now,
        })
        .map_err(ProjectionError::Retry)?;
    Ok(placement.note_id)
}

async fn ensure_parent_branch(
    state: &AppState,
    client: &ReadWeaveClient,
    mapping: &RemoteObjectMapRecord,
    desired_parent: &str,
    position: i64,
) -> Result<(), ProjectionError> {
    if mapping.remote_parent_id.as_deref() == Some(desired_parent) {
        return Ok(());
    }
    let old_parent = mapping.remote_parent_id.as_deref().ok_or_else(|| {
        ProjectionError::Conflict(anyhow::anyhow!("ReadWeave mapping has no current parent"))
    })?;
    let old_branch = match state
        .store
        .get_remote_object_details(CONNECTOR, &mapping.object_type, &mapping.local_id)
        .map_err(ProjectionError::Retry)?
        .and_then(|details| details.remote_branch_id)
    {
        Some(branch_id) => Some(branch_id),
        None => client
            .find_branch(&mapping.remote_id, old_parent)
            .await
            .map_err(ProjectionError::Retry)?
            .map(|branch| branch.branch_id),
    }
    .ok_or_else(|| {
        ProjectionError::Conflict(anyhow::anyhow!(
            "ReadWeave source branch cannot be identified safely"
        ))
    })?;

    // The new branch must exist before the old branch is deleted. ETAPI deletes the note when
    // the last branch is deleted, so reversing this order would make a workspace move destructive.
    let new_branch = client
        .create_branch(&mapping.remote_id, desired_parent, position)
        .await
        .map_err(ProjectionError::Retry)?;
    if new_branch.branch_id != old_branch {
        client
            .delete_branch(&old_branch)
            .await
            .map_err(ProjectionError::Retry)?;
    }
    let now = Utc::now();
    state
        .store
        .upsert_remote_object_map(&RemoteObjectMapRecord {
            remote_parent_id: Some(desired_parent.to_owned()),
            updated_at: now,
            ..mapping.clone()
        })
        .map_err(ProjectionError::Retry)?;
    state
        .store
        .upsert_remote_object_details(&RemoteObjectDetailsRecord {
            connector: CONNECTOR.to_owned(),
            object_type: mapping.object_type.clone(),
            local_id: mapping.local_id.clone(),
            remote_branch_id: Some(new_branch.branch_id),
            node_type: Some(mapping.object_type.clone()),
            last_synced_version: None,
            updated_at: now,
        })
        .map_err(ProjectionError::Retry)?;
    Ok(())
}

async fn update_managed_note(
    state: &AppState,
    client: &ReadWeaveClient,
    object_type: &str,
    local_id: &str,
    remote_id: &str,
    body: &str,
) -> Result<(), ProjectionError> {
    let managed = managed_region(body);
    let hash = content_hash(&managed);
    let current = client
        .get_content(remote_id)
        .await
        .map_err(ProjectionError::Retry)?;
    let merged = match merge_managed_content(&current, &managed) {
        Ok(merged) => merged,
        Err(conflict) => {
            ensure_recovery_note(state, client, local_id, &managed).await?;
            return Err(ProjectionError::Conflict(conflict));
        }
    };
    if merged != current {
        drop(current);
        drop(managed);
        client
            .put_content(remote_id, merged)
            .await
            .map_err(ProjectionError::Retry)?;
    }
    let now = Utc::now();
    let previous = state
        .store
        .get_remote_object_map(CONNECTOR, object_type, local_id)
        .map_err(ProjectionError::Retry)?
        .ok_or_else(|| ProjectionError::Retry(anyhow::anyhow!("remote mapping disappeared")))?;
    state
        .store
        .upsert_remote_object_map(&RemoteObjectMapRecord {
            content_hash: Some(hash),
            updated_at: now,
            ..previous
        })
        .map_err(ProjectionError::Retry)?;
    Ok(())
}

async fn ensure_recovery_note(
    state: &AppState,
    client: &ReadWeaveClient,
    local_id: &str,
    managed: &str,
) -> Result<(), ProjectionError> {
    let recovery_id = format!("recovery:{local_id}");
    if state
        .store
        .get_remote_object_map(CONNECTOR, "recovery", &recovery_id)
        .map_err(ProjectionError::Retry)?
        .is_some()
    {
        return Ok(());
    }
    let source = state
        .store
        .get_remote_object_map(CONNECTOR, "section", local_id)
        .map_err(ProjectionError::Retry)?
        .ok_or_else(|| {
            ProjectionError::Retry(anyhow::anyhow!("conflicted section mapping is missing"))
        })?;
    let parent = source.remote_parent_id.as_deref().ok_or_else(|| {
        ProjectionError::Retry(anyhow::anyhow!("conflicted section parent is missing"))
    })?;
    let title = format!("AIALRA 恢复副本 {}", Utc::now().format("%Y-%m-%d %H%M%S"));
    let placement = client
        .create_note(parent, &title, managed)
        .await
        .map_err(ProjectionError::Retry)?;
    let now = Utc::now();
    state
        .store
        .upsert_remote_object_map(&RemoteObjectMapRecord {
            connector: CONNECTOR.to_owned(),
            object_type: "recovery".to_owned(),
            local_id: recovery_id.clone(),
            remote_id: placement.note_id,
            remote_parent_id: Some(parent.to_owned()),
            content_hash: Some(content_hash(managed)),
            created_at: now,
            updated_at: now,
        })
        .map_err(ProjectionError::Retry)?;
    state
        .store
        .upsert_remote_object_details(&RemoteObjectDetailsRecord {
            connector: CONNECTOR.to_owned(),
            object_type: "recovery".to_owned(),
            local_id: recovery_id,
            remote_branch_id: Some(placement.branch_id),
            node_type: Some("recovery".to_owned()),
            last_synced_version: None,
            updated_at: now,
        })
        .map_err(ProjectionError::Retry)?;
    Ok(())
}

fn merge_managed_content(current: &str, managed: &str) -> Result<String> {
    let Some(start) = current.find(MANAGED_BEGIN) else {
        if current.trim().is_empty() {
            return Ok(managed.to_owned());
        }
        bail!("managed begin marker is missing")
    };
    let Some(relative_end) = current[start..].find(MANAGED_END) else {
        bail!("managed end marker is missing")
    };
    let end = start + relative_end + MANAGED_END.len();
    Ok(format!(
        "{}{}{}",
        &current[..start],
        managed,
        &current[end..]
    ))
}

fn managed_region(body: &str) -> String {
    format!("{MANAGED_BEGIN}\n{body}\n{MANAGED_END}")
}
fn content_hash(value: &str) -> String {
    hex::encode(Sha256::digest(value.as_bytes()))
}
fn html(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn render_overview(
    session: &aialra_event_store::SessionRecord,
    events: &[aialra_event_protocol::EventEnvelope],
) -> String {
    let final_summary = events
        .iter()
        .rev()
        .find(|event| event.event_type == "session.summary.created")
        .and_then(|event| event.payload.get("result"));
    let summary = final_summary
        .and_then(|result| result.get("overview"))
        .and_then(Value::as_str)
        .or_else(|| {
            events
                .iter()
                .rev()
                .find(|event| event.event_type == "explanation.card.created")
                .and_then(|event| event.payload.get("result"))
                .and_then(|result| result.get("summary"))
                .and_then(Value::as_str)
        })
        .map(html)
        .unwrap_or_else(|| "等待课程讲解".to_owned());
    let key_points = final_summary
        .and_then(|result| result.get("key_points"))
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(|item| format!("<li>{}</li>", html(item)))
                .collect::<String>()
        })
        .filter(|items| !items.is_empty())
        .unwrap_or_else(|| "<li>等待最终总结</li>".to_owned());
    format!(
        "<h2>课程信息</h2><ul><li>名称：{}</li><li>时间：{}</li><li>语言：{} → {}</li><li>状态：{:?}</li></ul><h2>课程总结</h2><p>{summary}</p><h3>关键知识点</h3><ul>{key_points}</ul>",
        html(&session.title),
        session.created_at.to_rfc3339(),
        html(&session.source_language),
        html(&session.target_language),
        session.state
    )
}

fn render_transcript(events: &[aialra_event_protocol::EventEnvelope]) -> String {
    let translations = events
        .iter()
        .filter(|event| event.event_type == "translation.finalized")
        .filter_map(|event| {
            Some((
                event.payload.get("segment_id")?.as_str()?.to_owned(),
                event.payload.get("text")?.as_str()?.to_owned(),
            ))
        })
        .collect::<std::collections::HashMap<_, _>>();
    let rows = events.iter().filter(|event| event.event_type == "segment.finalized").map(|event| {
        let segment = event.payload.get("segment_id").and_then(Value::as_str).unwrap_or("unknown");
        let original = event.payload.get("text").and_then(Value::as_str).unwrap_or("");
        let translated = translations.get(segment).map(String::as_str).unwrap_or("等待翻译");
        let provider = event.payload.get("provider").and_then(Value::as_str).unwrap_or("unknown");
        format!("<section><h3>{}</h3><p><strong>原文</strong> {}</p><p><strong>译文</strong> {}</p><p><small>{} · {} · {}</small></p></section>", event.ingested_at.format("%H:%M:%S"), html(original), html(translated), html(provider), html(segment), event.event_id)
    }).collect::<String>();
    if rows.is_empty() {
        "<p>等待稳定字幕</p>".to_owned()
    } else {
        rows
    }
}

fn render_explanations(events: &[aialra_event_protocol::EventEnvelope]) -> String {
    let rows = events
        .iter()
        .filter(|event| event.event_type == "explanation.card.created")
        .map(|event| {
            let result = event.payload.get("result").unwrap_or(&Value::Null);
            let summary = result
                .get("summary")
                .and_then(Value::as_str)
                .unwrap_or("等待讲解总结");
            let context = render_text_items(
                result.get("missing_context"),
                "text",
                "暂无需要补充的背景",
            );
            let rare_terms = result
                .get("rare_terms")
                .and_then(Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .map(|item| {
                            let term = item.get("term").and_then(Value::as_str).unwrap_or("术语");
                            let one_line = item
                                .get("one_line")
                                .and_then(Value::as_str)
                                .unwrap_or("等待解释");
                            format!("<li><strong>{}</strong>：{}</li>", html(term), html(one_line))
                        })
                        .collect::<String>()
                })
                .filter(|items| !items.is_empty())
                .unwrap_or_else(|| "<li>暂无需要单独解释的生僻词</li>".to_owned());
            let asr_errors = render_string_list(
                result.get("possible_asr_errors"),
                "暂无疑似识别错误",
            );
            let review_questions = render_string_list(
                result.get("review_questions"),
                "暂无复习问题",
            );
            let evidence = render_string_list(
                result.get("evidence_segment_ids"),
                "暂无字幕证据",
            );
            let page_evidence = render_string_list(
                result.get("asset_page_ids"),
                "暂无课件页证据",
            );
            format!(
                "<section><h3>{}</h3><p>{}</p><h4>补充背景</h4><ul>{}</ul><h4>生僻词</h4><ul>{}</ul><h4>疑似识别错误</h4><ul>{}</ul><h4>复习问题</h4><ul>{}</ul><p><small>字幕证据：{} · 课件证据：{} · 事件 {}</small></p></section>",
                event.ingested_at.format("%H:%M:%S"),
                html(summary),
                context,
                rare_terms,
                asr_errors,
                review_questions,
                evidence,
                page_evidence,
                event.event_id
            )
        })
        .collect::<String>();
    if rows.is_empty() {
        "<p>等待补充解释</p>".to_owned()
    } else {
        rows
    }
}

fn render_text_items(value: Option<&Value>, field: &str, empty: &str) -> String {
    value
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.get(field).and_then(Value::as_str))
                .map(|item| format!("<li>{}</li>", html(item)))
                .collect::<String>()
        })
        .filter(|items| !items.is_empty())
        .unwrap_or_else(|| format!("<li>{}</li>", html(empty)))
}

fn render_string_list(value: Option<&Value>, empty: &str) -> String {
    value
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(|item| format!("<li>{}</li>", html(item)))
                .collect::<String>()
        })
        .filter(|items| !items.is_empty())
        .unwrap_or_else(|| format!("<li>{}</li>", html(empty)))
}

fn render_assets(events: &[aialra_event_protocol::EventEnvelope]) -> String {
    let rows = events
        .iter()
        .filter(|event| event.event_type == "asset.page.extracted")
        .map(|event| {
            let page_id = event
                .payload
                .get("page_id")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            let related = events
                .iter()
                .filter(|candidate| candidate.event_type == "explanation.card.created")
                .filter(|candidate| {
                    candidate
                        .payload
                        .get("result")
                        .and_then(|result| result.get("asset_page_ids"))
                        .and_then(Value::as_array)
                        .is_some_and(|ids| ids.iter().any(|id| id.as_str() == Some(page_id)))
                })
                .map(|candidate| candidate.event_id.to_string())
                .collect::<Vec<_>>();
            let related_text = if related.is_empty() {
                "等待相关讲解".to_owned()
            } else {
                related.join(", ")
            };
            format!(
                "<section><h3>{} · 第 {} 页</h3><p>{}</p><p><small>页面 {} · 相关讲解 {} · 事件 {}</small></p></section>",
                html(
                    event
                        .payload
                        .get("title")
                        .and_then(Value::as_str)
                        .unwrap_or("课程材料")
                ),
                event
                    .payload
                    .get("page_number")
                    .and_then(Value::as_u64)
                    .unwrap_or(0),
                html(
                    event
                        .payload
                        .get("text")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                ),
                html(page_id),
                html(&related_text),
                event.event_id
            )
        })
        .collect::<String>();
    if rows.is_empty() {
        "<p>尚未加入课程材料</p>".to_owned()
    } else {
        rows
    }
}

pub fn status_payload(state: &AppState, project_id: &str) -> Result<Value, ApiError> {
    let status = state.store.connector_status(project_id)?;
    let project_map = state
        .store
        .get_remote_object_map(CONNECTOR, "project", project_id)?;
    let note_url = ReadWeaveClient::from_env()
        .and_then(|client| client.public_url)
        .zip(project_map.as_ref().map(|record| record.remote_id.clone()))
        .map(|(base, note)| format!("{base}/#root/{note}"));
    let mut targets = Vec::new();
    if let Some(base) = ReadWeaveClient::from_env().and_then(|client| client.public_url) {
        let sync_status = if status.conflicts > 0 {
            "conflict"
        } else if status.leased > 0 || status.queued > 0 {
            "syncing"
        } else {
            "synced"
        };
        if let Some(record) = project_map.as_ref() {
            targets.push(json!({"node_type": "project", "local_id": project_id, "title": "项目笔记", "note_url": format!("{base}/#root/{}", record.remote_id), "sync_status": sync_status}));
        }
        for session in state.store.list_project_sessions(project_id)? {
            let definitions = [
                (
                    "session",
                    "session",
                    session.id.clone(),
                    session.title.clone(),
                ),
                (
                    "overview",
                    "section",
                    format!("{}:overview", session.id),
                    "00 课程概览".to_owned(),
                ),
                (
                    "transcript",
                    "section",
                    format!("{}:transcript", session.id),
                    "01 实时转写与翻译".to_owned(),
                ),
                (
                    "explanations",
                    "section",
                    format!("{}:explanations", session.id),
                    "02 生僻词与补充解释".to_owned(),
                ),
                (
                    "assets",
                    "section",
                    format!("{}:assets", session.id),
                    "03 课件与证据索引".to_owned(),
                ),
                (
                    "user_notes",
                    "user_notes",
                    format!("{}:user", session.id),
                    "99 我的笔记".to_owned(),
                ),
            ];
            for (node_type, object_type, local_id, title) in definitions {
                if let Some(record) =
                    state
                        .store
                        .get_remote_object_map(CONNECTOR, object_type, &local_id)?
                {
                    targets.push(json!({"node_type": node_type, "local_id": local_id, "title": title, "note_url": format!("{base}/#root/{}", record.remote_id), "sync_status": sync_status}));
                }
            }
        }
    }
    Ok(
        json!({"configured": configured(), "queued": status.queued, "syncing": status.leased, "completed": status.completed, "conflicts": status.conflicts, "updated_at": status.updated_at, "note_url": note_url, "targets": targets}),
    )
}

pub fn preview_payload(state: &AppState, project_id: &str) -> Result<Value, ApiError> {
    let sessions = state.store.list_project_sessions(project_id)?;
    let mut previews = Vec::new();
    for session in sessions.into_iter().take(5) {
        let events = state.store.list_events(&session.id)?;
        let translations = events
            .iter()
            .filter(|event| event.event_type == "translation.finalized")
            .filter_map(|event| {
                Some((
                    event.payload.get("segment_id")?.as_str()?.to_owned(),
                    event.payload.get("text")?.as_str()?.to_owned(),
                ))
            })
            .collect::<std::collections::HashMap<_, _>>();
        let latest_entries = events
            .iter()
            .filter(|event| event.event_type == "segment.finalized")
            .rev()
            .take(3)
            .filter_map(|event| {
                let segment_id = event.payload.get("segment_id")?.as_str()?;
                let original = event.payload.get("text")?.as_str()?;
                Some(json!({
                    "segment_id": segment_id,
                    "original": original,
                    "translation": translations.get(segment_id),
                }))
            })
            .collect::<Vec<_>>();
        let explanations = events
            .iter()
            .filter(|event| event.event_type == "explanation.card.created")
            .count();
        previews.push(json!({
            "session_id": session.id, "title": session.title, "state": session.state,
            "latest_entries": latest_entries, "explanation_count": explanations,
        }));
    }
    Ok(json!({"sessions": previews}))
}

#[cfg(test)]
mod tests {
    use super::*;
    use aialra_event_store::{NewProject, NewSession};

    #[test]
    fn managed_merge_preserves_user_content() {
        let current = format!(
            "<p>before</p>{}<p>old</p>{}<p>after</p>",
            MANAGED_BEGIN, MANAGED_END
        );
        let merged = merge_managed_content(&current, &managed_region("<p>new</p>")).unwrap();
        assert!(merged.contains("<p>before</p>"));
        assert!(merged.contains("<p>new</p>"));
        assert!(merged.contains("<p>after</p>"));
    }

    #[test]
    fn missing_markers_never_overwrite_manual_notes() {
        assert!(merge_managed_content("<p>manual</p>", &managed_region("<p>new</p>")).is_err());
    }

    #[test]
    fn explanation_projection_is_readable_and_escapes_model_text() {
        let event = aialra_event_protocol::EventEnvelope::new(
            "session_test",
            "gpu_explainer",
            0,
            "explanation.card.created",
            0,
            "explanation_test",
            None,
            json!({
                "result": {
                    "summary": "理解 <attention> 的作用",
                    "missing_context": [{"text": "注意力机制按相关性聚合信息"}],
                    "rare_terms": [{"term": "attention", "one_line": "按相关性选择上下文"}],
                    "possible_asr_errors": [],
                    "review_questions": ["为什么需要注意力机制"],
                    "evidence_segment_ids": ["segment_1"]
                }
            }),
        )
        .unwrap();
        let rendered = render_explanations(&[event]);
        assert!(rendered.contains("生僻词"));
        assert!(rendered.contains("attention"));
        assert!(rendered.contains("segment_1"));
        assert!(rendered.contains("&lt;attention&gt;"));
        assert!(!rendered.contains("<pre>"));
    }

    #[test]
    fn in_page_preview_pairs_transcript_and_translation() {
        let temp = tempfile::tempdir().unwrap();
        let state = AppState::open(temp.path()).unwrap();
        state
            .store
            .create_project(&NewProject {
                id: "project_preview".to_owned(),
                owner_subject: "owner".to_owned(),
                title: "Preview course".to_owned(),
                source_language: "en".to_owned(),
                target_language: "zh-CN".to_owned(),
            })
            .unwrap();
        state
            .store
            .create_session(&NewSession {
                id: "session_preview".to_owned(),
                title: "Preview session".to_owned(),
                source_language: "en".to_owned(),
                target_language: "zh-CN".to_owned(),
                privacy_mode: "local_only".to_owned(),
                consent_confirmed: true,
                demo_mode: false,
            })
            .unwrap();
        state
            .store
            .attach_session_to_project(
                "project_preview",
                "session_preview",
                "owner",
                "browser-preview",
            )
            .unwrap();
        for (source, event_type, payload) in [
            (
                "gpu_asr",
                "segment.finalized",
                json!({"segment_id": "segment_one", "text": "attention mechanism"}),
            ),
            (
                "gpu_translator",
                "translation.finalized",
                json!({"segment_id": "segment_one", "text": "注意力机制"}),
            ),
        ] {
            let event = aialra_event_protocol::EventEnvelope::new(
                "session_preview",
                source,
                0,
                event_type,
                0,
                Uuid::now_v7().to_string(),
                None,
                payload,
            )
            .unwrap();
            state.store.insert_event(&event).unwrap();
        }

        let preview = preview_payload(&state, "project_preview").unwrap();
        let entry = &preview["sessions"][0]["latest_entries"][0];
        assert_eq!(entry["original"], "attention mechanism");
        assert_eq!(entry["translation"], "注意力机制");
    }
}
