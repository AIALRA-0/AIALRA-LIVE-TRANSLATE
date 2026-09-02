//! SQLite WAL event storage and rebuildable local projections.

use aialra_core_domain::SessionState;
use aialra_event_protocol::EventEnvelope;
use anyhow::{Context, Result, bail};
use chrono::{DateTime, NaiveDateTime, Utc};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::Path;
use std::sync::{Arc, Mutex};

const INITIAL_MIGRATION: &str = include_str!("../migrations/0001_initial.migration");
const MODEL_JOBS_MIGRATION: &str = include_str!("../migrations/0002_model_jobs.migration");
const PROJECTS_CONNECTORS_MIGRATION: &str =
    include_str!("../migrations/0003_projects_connectors.migration");
const JOB_METRICS_MIGRATION: &str = include_str!("../migrations/0004_job_metrics.migration");
const WORKSPACE_MIGRATION: &str = include_str!("../migrations/0005_workspace.migration");
const DEVICE_PAIRING_MIGRATION: &str = include_str!("../migrations/0006_device_pairing.migration");
const SUMMARY_MODEL_GATE_MIGRATION: &str =
    include_str!("../migrations/0007_summary_model_gate.migration");
const QUALITY_PIPELINE_MIGRATION: &str =
    include_str!("../migrations/0008_quality_pipeline.migration");
const WORKSPACE_TRASH_MIGRATION: &str =
    include_str!("../migrations/0009_workspace_trash.migration");

// The duplicate-event check reads the immutable identity and lineage fields
// together so a retransmission can be compared without silently widening its
// semantics.
type EventIdentityRow = (
    String,
    String,
    String,
    u64,
    String,
    String,
    String,
    Option<String>,
);

#[derive(Clone)]
pub struct EventStore {
    connection: Arc<Mutex<Connection>>,
}

impl EventStore {
    /// Opening the database enables WAL and applies idempotent migrations before serving traffic.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        if let Some(parent) = path.as_ref().parent() {
            std::fs::create_dir_all(parent).context("create database parent directory")?;
        }
        let connection = Connection::open(path).context("open SQLite database")?;
        connection
            .pragma_update(None, "journal_mode", "WAL")
            .context("enable SQLite WAL")?;
        connection
            .pragma_update(None, "foreign_keys", "ON")
            .context("enable foreign keys")?;
        connection
            .pragma_update(None, "busy_timeout", 5_000)
            .context("configure SQLite busy timeout")?;
        connection
            .execute_batch(INITIAL_MIGRATION)
            .context("apply initial migration")?;
        connection.execute(
            "INSERT OR IGNORE INTO schema_migrations(version, applied_at) VALUES (1, ?1)",
            [Utc::now().to_rfc3339()],
        )?;
        connection
            .execute_batch(MODEL_JOBS_MIGRATION)
            .context("apply model jobs migration")?;
        connection.execute(
            "INSERT OR IGNORE INTO schema_migrations(version, applied_at) VALUES (2, ?1)",
            [Utc::now().to_rfc3339()],
        )?;
        connection
            .execute_batch(PROJECTS_CONNECTORS_MIGRATION)
            .context("apply projects and connectors migration")?;
        connection.execute(
            "INSERT OR IGNORE INTO schema_migrations(version, applied_at) VALUES (3, ?1)",
            [Utc::now().to_rfc3339()],
        )?;
        connection
            .execute_batch(JOB_METRICS_MIGRATION)
            .context("apply job metrics migration")?;
        connection.execute(
            "INSERT OR IGNORE INTO schema_migrations(version, applied_at) VALUES (4, ?1)",
            [Utc::now().to_rfc3339()],
        )?;
        let workspace_applied: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM schema_migrations WHERE version = 5)",
            [],
            |row| row.get(0),
        )?;
        if !workspace_applied {
            let transaction = connection.unchecked_transaction()?;
            transaction
                .execute_batch(WORKSPACE_MIGRATION)
                .context("apply workspace migration")?;
            transaction.execute(
                "INSERT INTO schema_migrations(version, applied_at) VALUES (5, ?1)",
                [Utc::now().to_rfc3339()],
            )?;
            transaction.commit()?;
        }
        let pairing_applied: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM schema_migrations WHERE version = 6)",
            [],
            |row| row.get(0),
        )?;
        if !pairing_applied {
            let transaction = connection.unchecked_transaction()?;
            transaction
                .execute_batch(DEVICE_PAIRING_MIGRATION)
                .context("apply device pairing migration")?;
            transaction.execute(
                "INSERT INTO schema_migrations(version, applied_at) VALUES (6, ?1)",
                [Utc::now().to_rfc3339()],
            )?;
            transaction.commit()?;
        }
        let summary_gate_applied: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM schema_migrations WHERE version = 7)",
            [],
            |row| row.get(0),
        )?;
        if !summary_gate_applied {
            let transaction = connection.unchecked_transaction()?;
            transaction
                .execute_batch(SUMMARY_MODEL_GATE_MIGRATION)
                .context("apply summary model gate migration")?;
            transaction.execute(
                "INSERT INTO schema_migrations(version, applied_at) VALUES (7, ?1)",
                [Utc::now().to_rfc3339()],
            )?;
            transaction.commit()?;
        }
        let quality_pipeline_applied: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM schema_migrations WHERE version = 8)",
            [],
            |row| row.get(0),
        )?;
        if !quality_pipeline_applied {
            let transaction = connection.unchecked_transaction()?;
            transaction
                .execute_batch(QUALITY_PIPELINE_MIGRATION)
                .context("apply quality pipeline migration")?;
            transaction.execute(
                "INSERT INTO schema_migrations(version, applied_at) VALUES (8, ?1)",
                [Utc::now().to_rfc3339()],
            )?;
            transaction.commit()?;
        }
        let workspace_trash_applied: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM schema_migrations WHERE version = 9)",
            [],
            |row| row.get(0),
        )?;
        if !workspace_trash_applied {
            let transaction = connection.unchecked_transaction()?;
            transaction
                .execute_batch(WORKSPACE_TRASH_MIGRATION)
                .context("apply workspace trash migration")?;
            transaction.execute(
                "INSERT INTO schema_migrations(version, applied_at) VALUES (9, ?1)",
                [Utc::now().to_rfc3339()],
            )?;
            transaction.commit()?;
        }
        Ok(Self {
            connection: Arc::new(Mutex::new(connection)),
        })
    }

    /// Real recording requires consent while deterministic demo sessions remain available for testing.
    pub fn create_session(&self, session: &NewSession) -> Result<SessionRecord> {
        if !session.consent_confirmed && !session.demo_mode {
            bail!("recording consent is required outside demo mode");
        }
        let now = Utc::now();
        let record = SessionRecord {
            id: session.id.clone(),
            title: session.title.clone(),
            state: SessionState::Created,
            source_language: session.source_language.clone(),
            target_language: session.target_language.clone(),
            privacy_mode: session.privacy_mode.clone(),
            consent_confirmed: session.consent_confirmed,
            demo_mode: session.demo_mode,
            created_at: now,
            updated_at: now,
        };
        let connection = self.lock()?;
        connection.execute(
            "INSERT INTO sessions(id, title, state, source_language, target_language, privacy_mode, consent_confirmed, demo_mode, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                record.id,
                record.title,
                state_name(record.state),
                record.source_language,
                record.target_language,
                record.privacy_mode,
                record.consent_confirmed,
                record.demo_mode,
                record.created_at.to_rfc3339(),
                record.updated_at.to_rfc3339(),
            ],
        )?;
        Ok(record)
    }

    /// State transitions are validated by the domain model before updating the current projection.
    pub fn transition_session(
        &self,
        session_id: &str,
        next: SessionState,
    ) -> Result<SessionRecord> {
        let current = self
            .get_session(session_id)?
            .with_context(|| format!("session {session_id} does not exist"))?;
        current.state.transition(next)?;
        let now = Utc::now();
        let connection = self.lock()?;
        connection.execute(
            "UPDATE sessions SET state = ?2, updated_at = ?3 WHERE id = ?1",
            params![session_id, state_name(next), now.to_rfc3339()],
        )?;
        drop(connection);
        self.get_session(session_id)?
            .context("session disappeared after transition")
    }

    pub fn get_session(&self, session_id: &str) -> Result<Option<SessionRecord>> {
        let connection = self.lock()?;
        connection
            .query_row(
                "SELECT id, title, state, source_language, target_language, privacy_mode, consent_confirmed, demo_mode, created_at, updated_at FROM sessions WHERE id = ?1",
                [session_id],
                map_session,
            )
            .optional()
            .context("query session")
    }

    pub fn update_session_title(
        &self,
        session_id: &str,
        title: &str,
    ) -> Result<Option<SessionRecord>> {
        let connection = self.lock()?;
        connection.execute(
            "UPDATE sessions SET title = ?2, updated_at = ?3 WHERE id = ?1",
            params![session_id, title, Utc::now().to_rfc3339()],
        )?;
        drop(connection);
        self.get_session(session_id)
    }

    pub fn list_sessions(&self) -> Result<Vec<SessionRecord>> {
        let connection = self.lock()?;
        let mut statement = connection.prepare(
            "SELECT id, title, state, source_language, target_language, privacy_mode, consent_confirmed, demo_mode, created_at, updated_at FROM sessions ORDER BY created_at DESC",
        )?;
        let records = statement
            .query_map([], map_session)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(records)
    }

    pub fn create_project(&self, project: &NewProject) -> Result<ProjectRecord> {
        let now = Utc::now();
        let connection = self.lock()?;
        connection.execute(
            "INSERT INTO projects(id, owner_subject, title, source_language, target_language, version, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, 1, ?6, ?6)",
            params![project.id, project.owner_subject, project.title, project.source_language, project.target_language, now.to_rfc3339()],
        )?;
        connection.execute(
            "INSERT INTO workspace_project_placements(project_id, updated_at) VALUES (?1, ?2)",
            params![project.id, now.to_rfc3339()],
        )?;
        connection.execute(
            "INSERT INTO project_ai_policies(project_id, updated_at) VALUES (?1, ?2)",
            params![project.id, now.to_rfc3339()],
        )?;
        drop(connection);
        self.get_project(&project.id)?
            .context("project disappeared after creation")
    }

    pub fn get_project(&self, project_id: &str) -> Result<Option<ProjectRecord>> {
        let connection = self.lock()?;
        connection
            .query_row(
                "SELECT id, owner_subject, title, source_language, target_language, version, created_at, updated_at FROM projects WHERE id = ?1",
                [project_id],
                map_project,
            )
            .optional()
            .context("query project")
    }

    pub fn list_projects(&self, owner_subject: &str) -> Result<Vec<ProjectRecord>> {
        let connection = self.lock()?;
        let mut statement = connection.prepare(
            "SELECT id, owner_subject, title, source_language, target_language, version, created_at, updated_at FROM projects WHERE owner_subject = ?1 ORDER BY updated_at DESC, id",
        )?;
        Ok(statement
            .query_map([owner_subject], map_project)?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn update_project_title(
        &self,
        project_id: &str,
        owner_subject: &str,
        title: &str,
    ) -> Result<Option<ProjectRecord>> {
        let now = Utc::now();
        let connection = self.lock()?;
        connection.execute(
            "UPDATE projects SET title = ?3, version = version + 1, updated_at = ?4 WHERE id = ?1 AND owner_subject = ?2",
            params![project_id, owner_subject, title, now.to_rfc3339()],
        )?;
        drop(connection);
        self.get_project(project_id)
    }

    pub fn update_project(
        &self,
        project_id: &str,
        owner_subject: &str,
        title: &str,
        source_language: &str,
        target_language: &str,
    ) -> Result<Option<ProjectRecord>> {
        let now = Utc::now();
        let connection = self.lock()?;
        connection.execute(
            "UPDATE projects SET title = ?3, source_language = ?4, target_language = ?5, version = version + 1, updated_at = ?6 WHERE id = ?1 AND owner_subject = ?2",
            params![project_id, owner_subject, title, source_language, target_language, now.to_rfc3339()],
        )?;
        drop(connection);
        self.get_project(project_id)
    }

    pub fn attach_session_to_project(
        &self,
        project_id: &str,
        session_id: &str,
        subject: &str,
        device_id: &str,
    ) -> Result<()> {
        let connection = self.lock()?;
        connection.execute(
            "INSERT INTO project_sessions(project_id, session_id, created_by_subject, created_by_device, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![project_id, session_id, subject, device_id, Utc::now().to_rfc3339()],
        )?;
        connection.execute(
            "INSERT INTO workspace_session_metadata(session_id, updated_at) VALUES (?1, ?2)",
            params![session_id, Utc::now().to_rfc3339()],
        )?;
        Ok(())
    }

    pub fn project_for_session(&self, session_id: &str) -> Result<Option<ProjectRecord>> {
        let connection = self.lock()?;
        connection
            .query_row(
                "SELECT p.id, p.owner_subject, p.title, p.source_language, p.target_language, p.version, p.created_at, p.updated_at FROM projects p JOIN project_sessions ps ON ps.project_id = p.id WHERE ps.session_id = ?1",
                [session_id],
                map_project,
            )
            .optional()
            .context("query project for session")
    }

    pub fn list_project_sessions(&self, project_id: &str) -> Result<Vec<SessionRecord>> {
        let connection = self.lock()?;
        let mut statement = connection.prepare(
            "SELECT s.id, s.title, s.state, s.source_language, s.target_language, s.privacy_mode, s.consent_confirmed, s.demo_mode, s.created_at, s.updated_at FROM sessions s JOIN project_sessions ps ON ps.session_id = s.id WHERE ps.project_id = ?1 ORDER BY s.created_at DESC",
        )?;
        Ok(statement
            .query_map([project_id], map_session)?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn list_sessions_for_owner(&self, owner_subject: &str) -> Result<Vec<SessionRecord>> {
        let connection = self.lock()?;
        let mut statement = connection.prepare(
            "SELECT s.id, s.title, s.state, s.source_language, s.target_language, s.privacy_mode, s.consent_confirmed, s.demo_mode, s.created_at, s.updated_at FROM sessions s JOIN project_sessions ps ON ps.session_id = s.id JOIN projects p ON p.id = ps.project_id WHERE p.owner_subject = ?1 ORDER BY s.created_at DESC",
        )?;
        Ok(statement
            .query_map([owner_subject], map_session)?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn list_unassigned_sessions(&self) -> Result<Vec<SessionRecord>> {
        let connection = self.lock()?;
        let mut statement = connection.prepare(
            "SELECT s.id, s.title, s.state, s.source_language, s.target_language, s.privacy_mode, s.consent_confirmed, s.demo_mode, s.created_at, s.updated_at FROM sessions s LEFT JOIN project_sessions ps ON ps.session_id = s.id WHERE ps.session_id IS NULL ORDER BY s.created_at",
        )?;
        Ok(statement
            .query_map([], map_session)?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn insert_project_update(
        &self,
        project_id: &str,
        session_id: Option<&str>,
        update_type: &str,
        payload: &Value,
    ) -> Result<ProjectUpdateRecord> {
        let created_at = Utc::now();
        let connection = self.lock()?;
        connection.execute(
            "INSERT INTO project_updates(project_id, session_id, update_type, payload_json, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![project_id, session_id, update_type, serde_json::to_string(payload)?, created_at.to_rfc3339()],
        )?;
        let cursor = connection.last_insert_rowid();
        Ok(ProjectUpdateRecord {
            cursor,
            project_id: project_id.to_owned(),
            session_id: session_id.map(str::to_owned),
            update_type: update_type.to_owned(),
            payload: payload.clone(),
            created_at,
        })
    }

    pub fn list_project_updates_after(
        &self,
        project_id: &str,
        cursor: i64,
    ) -> Result<Vec<ProjectUpdateRecord>> {
        let connection = self.lock()?;
        let mut statement = connection.prepare(
            "SELECT cursor, project_id, session_id, update_type, payload_json, created_at FROM project_updates WHERE project_id = ?1 AND cursor > ?2 ORDER BY cursor",
        )?;
        Ok(statement
            .query_map(params![project_id, cursor], map_project_update)?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn list_workspace_folders(
        &self,
        owner_subject: &str,
    ) -> Result<Vec<WorkspaceFolderRecord>> {
        let connection = self.lock()?;
        let mut statement = connection.prepare(
            "SELECT id, owner_subject, parent_id, title, sort_order, version, archived_at, created_at, updated_at FROM workspace_folders WHERE owner_subject = ?1 ORDER BY archived_at IS NOT NULL, sort_order, title, id",
        )?;
        Ok(statement
            .query_map([owner_subject], map_workspace_folder)?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn get_workspace_folder(&self, folder_id: &str) -> Result<Option<WorkspaceFolderRecord>> {
        let connection = self.lock()?;
        connection
            .query_row(
                "SELECT id, owner_subject, parent_id, title, sort_order, version, archived_at, created_at, updated_at FROM workspace_folders WHERE id = ?1",
                [folder_id],
                map_workspace_folder,
            )
            .optional()
            .context("query workspace folder")
    }

    pub fn create_workspace_folder(
        &self,
        folder: &NewWorkspaceFolder,
    ) -> Result<WorkspaceFolderRecord> {
        let now = Utc::now();
        let connection = self.lock()?;
        connection.execute(
            "INSERT INTO workspace_folders(id, owner_subject, parent_id, title, sort_order, version, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, 1, ?6, ?6)",
            params![folder.id, folder.owner_subject, folder.parent_id, folder.title, folder.sort_order, now.to_rfc3339()],
        )?;
        drop(connection);
        self.get_workspace_folder(&folder.id)?
            .context("workspace folder disappeared after creation")
    }

    pub fn update_workspace_folder(
        &self,
        folder_id: &str,
        owner_subject: &str,
        title: &str,
        parent_id: Option<&str>,
        sort_order: i64,
        archived: bool,
    ) -> Result<Option<WorkspaceFolderRecord>> {
        let now = Utc::now();
        let archived_at = archived.then(|| now.to_rfc3339());
        let connection = self.lock()?;
        connection.execute(
            "UPDATE workspace_folders SET title = ?3, parent_id = ?4, sort_order = ?5, archived_at = ?6, version = version + 1, updated_at = ?7 WHERE id = ?1 AND owner_subject = ?2",
            params![folder_id, owner_subject, title, parent_id, sort_order, archived_at, now.to_rfc3339()],
        )?;
        drop(connection);
        self.get_workspace_folder(folder_id)
    }

    pub fn list_workspace_project_placements(
        &self,
        owner_subject: &str,
    ) -> Result<Vec<WorkspaceProjectPlacementRecord>> {
        let connection = self.lock()?;
        let mut statement = connection.prepare(
            "SELECT w.project_id, w.folder_id, w.sort_order, w.archived_at, w.updated_at FROM workspace_project_placements w JOIN projects p ON p.id = w.project_id WHERE p.owner_subject = ?1 ORDER BY w.archived_at IS NOT NULL, w.sort_order, p.updated_at DESC",
        )?;
        Ok(statement
            .query_map([owner_subject], map_workspace_project_placement)?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn get_workspace_project_placement(
        &self,
        project_id: &str,
    ) -> Result<Option<WorkspaceProjectPlacementRecord>> {
        let connection = self.lock()?;
        connection
            .query_row(
                "SELECT project_id, folder_id, sort_order, archived_at, updated_at FROM workspace_project_placements WHERE project_id = ?1",
                [project_id],
                map_workspace_project_placement,
            )
            .optional()
            .context("query workspace project placement")
    }

    pub fn update_workspace_project_placement(
        &self,
        project_id: &str,
        folder_id: Option<&str>,
        sort_order: i64,
        archived: bool,
    ) -> Result<WorkspaceProjectPlacementRecord> {
        let now = Utc::now();
        let archived_at = archived.then(|| now.to_rfc3339());
        let connection = self.lock()?;
        connection.execute(
            "INSERT INTO workspace_project_placements(project_id, folder_id, sort_order, archived_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5) ON CONFLICT(project_id) DO UPDATE SET folder_id = excluded.folder_id, sort_order = excluded.sort_order, archived_at = excluded.archived_at, updated_at = excluded.updated_at",
            params![project_id, folder_id, sort_order, archived_at, now.to_rfc3339()],
        )?;
        connection.query_row(
            "SELECT project_id, folder_id, sort_order, archived_at, updated_at FROM workspace_project_placements WHERE project_id = ?1",
            [project_id],
            map_workspace_project_placement,
        ).context("query workspace project placement")
    }

    pub fn list_workspace_session_metadata(
        &self,
        owner_subject: &str,
    ) -> Result<Vec<WorkspaceSessionMetadataRecord>> {
        let connection = self.lock()?;
        let mut statement = connection.prepare(
            "SELECT w.session_id, w.pinned, w.sort_order, w.archived_at, w.updated_at FROM workspace_session_metadata w JOIN project_sessions ps ON ps.session_id = w.session_id JOIN projects p ON p.id = ps.project_id WHERE p.owner_subject = ?1 ORDER BY w.archived_at IS NOT NULL, w.pinned DESC, w.sort_order",
        )?;
        Ok(statement
            .query_map([owner_subject], map_workspace_session_metadata)?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn update_workspace_session_metadata(
        &self,
        session_id: &str,
        pinned: bool,
        sort_order: i64,
        archived: bool,
    ) -> Result<WorkspaceSessionMetadataRecord> {
        let now = Utc::now();
        let archived_at = archived.then(|| now.to_rfc3339());
        let connection = self.lock()?;
        connection.execute(
            "INSERT INTO workspace_session_metadata(session_id, pinned, sort_order, archived_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5) ON CONFLICT(session_id) DO UPDATE SET pinned = excluded.pinned, sort_order = excluded.sort_order, archived_at = excluded.archived_at, updated_at = excluded.updated_at",
            params![session_id, pinned, sort_order, archived_at, now.to_rfc3339()],
        )?;
        connection.query_row(
            "SELECT session_id, pinned, sort_order, archived_at, updated_at FROM workspace_session_metadata WHERE session_id = ?1",
            [session_id],
            map_workspace_session_metadata,
        ).context("query workspace session metadata")
    }

    pub fn move_workspace_folder_atomic(
        &self,
        folder_id: &str,
        owner_subject: &str,
        parent_id: Option<&str>,
        ordered_folder_ids: &[String],
    ) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        let mut connection = self.lock()?;
        let transaction = connection.transaction()?;
        let changed = transaction.execute(
            "UPDATE workspace_folders SET parent_id = ?3, version = version + 1, updated_at = ?4 WHERE id = ?1 AND owner_subject = ?2 AND archived_at IS NULL",
            params![folder_id, owner_subject, parent_id, now],
        )?;
        anyhow::ensure!(changed == 1, "workspace folder move target disappeared");
        for (index, id) in ordered_folder_ids.iter().enumerate() {
            let changed = transaction.execute(
                "UPDATE workspace_folders SET sort_order = ?4, updated_at = ?5 WHERE id = ?1 AND owner_subject = ?2 AND parent_id IS ?3 AND archived_at IS NULL",
                params![id, owner_subject, parent_id, (index as i64 + 1) * 10, now],
            )?;
            anyhow::ensure!(changed == 1, "workspace folder order changed concurrently");
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn move_workspace_project_atomic(
        &self,
        project_id: &str,
        owner_subject: &str,
        folder_id: Option<&str>,
        ordered_project_ids: &[String],
    ) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        let mut connection = self.lock()?;
        let transaction = connection.transaction()?;
        let changed = transaction.execute(
            "UPDATE workspace_project_placements SET folder_id = ?3, updated_at = ?4 WHERE project_id = ?1 AND archived_at IS NULL AND EXISTS (SELECT 1 FROM projects p WHERE p.id = workspace_project_placements.project_id AND p.owner_subject = ?2)",
            params![project_id, owner_subject, folder_id, now],
        )?;
        anyhow::ensure!(changed == 1, "workspace project move target disappeared");
        for (index, id) in ordered_project_ids.iter().enumerate() {
            let changed = transaction.execute(
                "UPDATE workspace_project_placements SET sort_order = ?4, updated_at = ?5 WHERE project_id = ?1 AND folder_id IS ?3 AND archived_at IS NULL AND EXISTS (SELECT 1 FROM projects p WHERE p.id = workspace_project_placements.project_id AND p.owner_subject = ?2)",
                params![id, owner_subject, folder_id, (index as i64 + 1) * 10, now],
            )?;
            anyhow::ensure!(changed == 1, "workspace project order changed concurrently");
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn reorder_workspace_sessions_atomic(
        &self,
        project_id: &str,
        owner_subject: &str,
        ordered_session_ids: &[String],
    ) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        let mut connection = self.lock()?;
        let transaction = connection.transaction()?;
        for (index, id) in ordered_session_ids.iter().enumerate() {
            let changed = transaction.execute(
                "UPDATE workspace_session_metadata SET sort_order = ?4, updated_at = ?5 WHERE session_id = ?1 AND archived_at IS NULL AND EXISTS (SELECT 1 FROM project_sessions ps JOIN projects p ON p.id = ps.project_id WHERE ps.session_id = ?1 AND ps.project_id = ?2 AND p.owner_subject = ?3)",
                params![id, project_id, owner_subject, (index as i64 + 1) * 10, now],
            )?;
            anyhow::ensure!(changed == 1, "workspace session reorder target disappeared");
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn list_workspace_trash(
        &self,
        owner_subject: &str,
    ) -> Result<Vec<WorkspaceTrashItemRecord>> {
        let connection = self.lock()?;
        let mut statement = connection.prepare(
            "SELECT owner_subject, entity_type, entity_id, original_parent_id, original_project_id, original_sort_order, original_pinned, deleted_at FROM workspace_trash_items WHERE owner_subject = ?1 ORDER BY deleted_at DESC, entity_type, entity_id",
        )?;
        Ok(statement
            .query_map([owner_subject], map_workspace_trash_item)?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Move a complete workspace selection into the recoverable recycle bin atomically.
    pub fn trash_workspace_items(&self, items: &[NewWorkspaceTrashItem]) -> Result<()> {
        if items.is_empty() {
            return Ok(());
        }
        let now = Utc::now().to_rfc3339();
        let mut connection = self.lock()?;
        let transaction = connection.transaction()?;
        for item in items {
            transaction.execute(
                "INSERT OR IGNORE INTO workspace_trash_items(owner_subject, entity_type, entity_id, original_parent_id, original_project_id, original_sort_order, original_pinned, deleted_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    item.owner_subject,
                    item.entity_type,
                    item.entity_id,
                    item.original_parent_id,
                    item.original_project_id,
                    item.original_sort_order,
                    item.original_pinned,
                    now,
                ],
            )?;
            match item.entity_type.as_str() {
                "folder" => {
                    transaction.execute(
                        "UPDATE workspace_folders SET archived_at = ?2, version = version + 1, updated_at = ?2 WHERE id = ?1 AND owner_subject = ?3",
                        params![item.entity_id, now, item.owner_subject],
                    )?;
                }
                "project" => {
                    transaction.execute(
                        "UPDATE workspace_project_placements SET archived_at = ?2, updated_at = ?2 WHERE project_id = ?1",
                        params![item.entity_id, now],
                    )?;
                }
                "session" => {
                    transaction.execute(
                        "UPDATE workspace_session_metadata SET archived_at = ?2, updated_at = ?2 WHERE session_id = ?1",
                        params![item.entity_id, now],
                    )?;
                }
                _ => bail!("unsupported workspace trash entity type"),
            }
        }
        transaction.commit()?;
        Ok(())
    }

    /// Restore a selected subtree while preserving its original location when it still exists.
    pub fn restore_workspace_items(
        &self,
        owner_subject: &str,
        items: &[WorkspaceTrashItemRecord],
    ) -> Result<()> {
        if items.is_empty() {
            return Ok(());
        }
        let restored = items
            .iter()
            .map(|item| (item.entity_type.as_str(), item.entity_id.as_str()))
            .collect::<std::collections::HashSet<_>>();
        let mut connection = self.lock()?;
        let transaction = connection.transaction()?;
        for item in items.iter().filter(|item| item.entity_type == "folder") {
            let parent = item.original_parent_id.as_deref().filter(|parent| {
                restored.contains(&("folder", *parent))
                    || transaction
                        .query_row(
                            "SELECT archived_at IS NULL FROM workspace_folders WHERE id = ?1 AND owner_subject = ?2",
                            params![parent, owner_subject],
                            |row| row.get::<_, bool>(0),
                        )
                        .unwrap_or(false)
            });
            transaction.execute(
                "UPDATE workspace_folders SET parent_id = ?2, sort_order = ?3, archived_at = NULL, version = version + 1, updated_at = ?4 WHERE id = ?1 AND owner_subject = ?5",
                params![item.entity_id, parent, item.original_sort_order, Utc::now().to_rfc3339(), owner_subject],
            )?;
        }
        for item in items.iter().filter(|item| item.entity_type == "project") {
            let folder = item.original_parent_id.as_deref().filter(|parent| {
                restored.contains(&("folder", *parent))
                    || transaction
                        .query_row(
                            "SELECT archived_at IS NULL FROM workspace_folders WHERE id = ?1 AND owner_subject = ?2",
                            params![parent, owner_subject],
                            |row| row.get::<_, bool>(0),
                        )
                        .unwrap_or(false)
            });
            transaction.execute(
                "UPDATE workspace_project_placements SET folder_id = ?2, sort_order = ?3, archived_at = NULL, updated_at = ?4 WHERE project_id = ?1",
                params![item.entity_id, folder, item.original_sort_order, Utc::now().to_rfc3339()],
            )?;
        }
        for item in items.iter().filter(|item| item.entity_type == "session") {
            transaction.execute(
                "UPDATE workspace_session_metadata SET pinned = ?2, sort_order = ?3, archived_at = NULL, updated_at = ?4 WHERE session_id = ?1",
                params![item.entity_id, item.original_pinned, item.original_sort_order, Utc::now().to_rfc3339()],
            )?;
        }
        for item in items {
            transaction.execute(
                "DELETE FROM workspace_trash_items WHERE owner_subject = ?1 AND entity_type = ?2 AND entity_id = ?3",
                params![owner_subject, item.entity_type, item.entity_id],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    /// Permanently remove one owner-scoped selection and return candidate object hashes.
    pub fn purge_workspace_items(
        &self,
        owner_subject: &str,
        folder_ids: &[String],
        project_ids: &[String],
        session_ids: &[String],
    ) -> Result<Vec<String>> {
        let mut connection = self.lock()?;
        let transaction = connection.transaction()?;
        let mut object_hashes = Vec::new();
        for session_id in session_ids {
            collect_session_object_hashes(&transaction, session_id, &mut object_hashes)?;
        }
        for session_id in session_ids {
            transaction.execute(
                "DELETE FROM project_updates WHERE session_id = ?1",
                [session_id],
            )?;
            transaction.execute(
                "DELETE FROM connector_jobs WHERE session_id = ?1",
                [session_id],
            )?;
            transaction.execute("DELETE FROM model_jobs WHERE session_id = ?1", [session_id])?;
            transaction.execute(
                "DELETE FROM device_pairing_codes WHERE session_id = ?1",
                [session_id],
            )?;
            transaction.execute(
                "DELETE FROM device_credentials WHERE session_id = ?1",
                [session_id],
            )?;
            transaction.execute(
                "DELETE FROM recording_leases WHERE session_id = ?1",
                [session_id],
            )?;
            transaction.execute(
                "DELETE FROM audio_assembly_cursors WHERE session_id = ?1",
                [session_id],
            )?;
            transaction.execute(
                "DELETE FROM audio_windows WHERE session_id = ?1",
                [session_id],
            )?;
            transaction.execute(
                "DELETE FROM audio_chunks WHERE session_id = ?1",
                [session_id],
            )?;
            transaction.execute(
                "DELETE FROM asset_pages WHERE asset_id IN (SELECT id FROM assets WHERE session_id = ?1)",
                [session_id],
            )?;
            transaction.execute("DELETE FROM assets WHERE session_id = ?1", [session_id])?;
            transaction.execute("DELETE FROM events WHERE session_id = ?1", [session_id])?;
            transaction.execute(
                "DELETE FROM workspace_device_preferences WHERE active_session_id = ?1",
                [session_id],
            )?;
            transaction.execute(
                "DELETE FROM workspace_session_metadata WHERE session_id = ?1",
                [session_id],
            )?;
            transaction.execute(
                "DELETE FROM project_sessions WHERE session_id = ?1",
                [session_id],
            )?;
            transaction.execute("DELETE FROM sessions WHERE id = ?1", [session_id])?;
            transaction.execute(
                "DELETE FROM remote_object_maps WHERE local_id = ?1 OR local_id LIKE (?1 || ':%')",
                [session_id],
            )?;
            transaction.execute(
                "DELETE FROM workspace_trash_items WHERE owner_subject = ?1 AND entity_type = 'session' AND entity_id = ?2",
                params![owner_subject, session_id],
            )?;
        }
        for project_id in project_ids {
            transaction.execute(
                "DELETE FROM project_updates WHERE project_id = ?1",
                [project_id],
            )?;
            transaction.execute(
                "DELETE FROM connector_jobs WHERE project_id = ?1",
                [project_id],
            )?;
            transaction.execute(
                "DELETE FROM recording_leases WHERE project_id = ?1",
                [project_id],
            )?;
            transaction.execute(
                "DELETE FROM workspace_device_preferences WHERE active_project_id = ?1",
                [project_id],
            )?;
            transaction.execute(
                "DELETE FROM project_ai_policies WHERE project_id = ?1",
                [project_id],
            )?;
            transaction.execute(
                "DELETE FROM workspace_project_placements WHERE project_id = ?1",
                [project_id],
            )?;
            transaction.execute(
                "DELETE FROM remote_object_maps WHERE object_type = 'project' AND local_id = ?1",
                [project_id],
            )?;
            transaction.execute(
                "DELETE FROM workspace_trash_items WHERE owner_subject = ?1 AND entity_type = 'project' AND entity_id = ?2",
                params![owner_subject, project_id],
            )?;
            transaction.execute("DELETE FROM projects WHERE id = ?1", [project_id])?;
        }
        // workspace_folders is self-referential without ON DELETE CASCADE;
        // delete descendants before their restored parent to keep the purge
        // transaction valid for nested recycle-bin selections.
        for folder_id in folder_ids.iter().rev() {
            transaction.execute("DELETE FROM remote_object_maps WHERE object_type = 'workspace_folder' AND local_id = ?1", [folder_id])?;
            transaction.execute(
                "DELETE FROM workspace_trash_items WHERE owner_subject = ?1 AND entity_type = 'folder' AND entity_id = ?2",
                params![owner_subject, folder_id],
            )?;
            transaction.execute(
                "DELETE FROM workspace_folders WHERE id = ?1 AND owner_subject = ?2",
                params![folder_id, owner_subject],
            )?;
        }
        transaction.commit()?;
        object_hashes.sort();
        object_hashes.dedup();
        Ok(object_hashes)
    }

    pub fn object_hash_is_referenced(&self, object_hash: &str) -> Result<bool> {
        let connection = self.lock()?;
        let count: u64 = connection.query_row(
            "SELECT (SELECT COUNT(*) FROM audio_chunks WHERE object_hash = ?1) + (SELECT COUNT(*) FROM audio_windows WHERE object_hash = ?1) + (SELECT COUNT(*) FROM assets WHERE object_hash = ?1) + (SELECT COUNT(*) FROM asset_pages WHERE object_hash = ?1) + (SELECT COUNT(*) FROM model_jobs WHERE input_object_hash = ?1)",
            [object_hash],
            |row| row.get(0),
        )?;
        Ok(count > 0)
    }

    pub fn get_workspace_preference(
        &self,
        owner_subject: &str,
        device_id: &str,
    ) -> Result<Option<WorkspaceDevicePreferenceRecord>> {
        let connection = self.lock()?;
        connection.query_row(
            "SELECT owner_subject, device_id, active_project_id, active_session_id, language_view, sidebar_collapsed, updated_at FROM workspace_device_preferences WHERE owner_subject = ?1 AND device_id = ?2",
            params![owner_subject, device_id],
            map_workspace_device_preference,
        ).optional().context("query workspace device preference")
    }

    pub fn upsert_workspace_preference(
        &self,
        preference: &WorkspaceDevicePreferenceRecord,
    ) -> Result<WorkspaceDevicePreferenceRecord> {
        let now = Utc::now();
        let connection = self.lock()?;
        connection.execute(
            "INSERT INTO workspace_device_preferences(owner_subject, device_id, active_project_id, active_session_id, language_view, sidebar_collapsed, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7) ON CONFLICT(owner_subject, device_id) DO UPDATE SET active_project_id = excluded.active_project_id, active_session_id = excluded.active_session_id, language_view = excluded.language_view, sidebar_collapsed = excluded.sidebar_collapsed, updated_at = excluded.updated_at",
            params![preference.owner_subject, preference.device_id, preference.active_project_id, preference.active_session_id, preference.language_view, preference.sidebar_collapsed, now.to_rfc3339()],
        )?;
        drop(connection);
        self.get_workspace_preference(&preference.owner_subject, &preference.device_id)?
            .context("workspace preference disappeared after update")
    }

    pub fn insert_workspace_update(
        &self,
        owner_subject: &str,
        update_type: &str,
        payload: &Value,
    ) -> Result<WorkspaceUpdateRecord> {
        let created_at = Utc::now();
        let connection = self.lock()?;
        connection.execute(
            "INSERT INTO workspace_updates(owner_subject, update_type, payload_json, created_at) VALUES (?1, ?2, ?3, ?4)",
            params![owner_subject, update_type, serde_json::to_string(payload)?, created_at.to_rfc3339()],
        )?;
        Ok(WorkspaceUpdateRecord {
            cursor: connection.last_insert_rowid(),
            owner_subject: owner_subject.to_owned(),
            update_type: update_type.to_owned(),
            payload: payload.clone(),
            created_at,
        })
    }

    pub fn list_workspace_updates_after(
        &self,
        owner_subject: &str,
        cursor: i64,
    ) -> Result<Vec<WorkspaceUpdateRecord>> {
        let connection = self.lock()?;
        let mut statement = connection.prepare(
            "SELECT cursor, owner_subject, update_type, payload_json, created_at FROM workspace_updates WHERE owner_subject = ?1 AND cursor > ?2 ORDER BY cursor",
        )?;
        Ok(statement
            .query_map(params![owner_subject, cursor], map_workspace_update)?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn get_project_ai_policy(&self, project_id: &str) -> Result<ProjectAiPolicyRecord> {
        let connection = self.lock()?;
        connection.query_row(
            "SELECT project_id, cloud_enabled, allowed_modalities_json, local_translation_model, local_explanation_model, local_summary_model, local_vision_model, updated_at FROM project_ai_policies WHERE project_id = ?1",
            [project_id],
            map_project_ai_policy,
        ).context("query project AI policy")
    }

    pub fn update_project_ai_policy(
        &self,
        project_id: &str,
        cloud_enabled: bool,
        allowed_modalities: &[String],
    ) -> Result<ProjectAiPolicyRecord> {
        let now = Utc::now();
        let connection = self.lock()?;
        connection.execute(
            "UPDATE project_ai_policies SET cloud_enabled = ?2, allowed_modalities_json = ?3, updated_at = ?4 WHERE project_id = ?1",
            params![project_id, cloud_enabled, serde_json::to_string(allowed_modalities)?, now.to_rfc3339()],
        )?;
        drop(connection);
        self.get_project_ai_policy(project_id)
    }

    /// Retransmissions are idempotent, but a changed payload is an explicit conflict.
    pub fn insert_event(&self, event: &EventEnvelope) -> Result<bool> {
        event.validate_version()?;
        let connection = self.lock()?;
        let changed = connection.execute(
            "INSERT OR IGNORE INTO events(event_id, schema_version, session_id, source_id, sequence, event_type, captured_at_monotonic_ns, captured_at_wall, ingested_at, correlation_id, causation_id, content_hash, payload_json) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            params![
                event.event_id.to_string(),
                event.schema_version,
                event.session_id,
                event.source_id,
                event.sequence,
                event.event_type,
                event.captured_at_monotonic_ns,
                event.captured_at_wall.to_rfc3339(),
                event.ingested_at.to_rfc3339(),
                event.correlation_id,
                event.causation_id,
                event.content_hash,
                serde_json::to_string(&event.payload)?,
            ],
        )?;
        if changed == 1 {
            return Ok(true);
        }

        let event_id = event.event_id.to_string();
        let existing_by_id: Option<EventIdentityRow> = connection
            .query_row(
                "SELECT content_hash, session_id, source_id, sequence, schema_version, event_type, correlation_id, causation_id FROM events WHERE event_id = ?1",
                [&event_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?, row.get(6)?, row.get(7)?)),
            )
            .optional()?;
        if let Some((
            content_hash,
            session_id,
            source_id,
            sequence,
            schema_version,
            event_type,
            correlation_id,
            causation_id,
        )) = existing_by_id
        {
            if content_hash == event.content_hash
                && session_id == event.session_id
                && source_id == event.source_id
                && sequence == event.sequence
                && schema_version == event.schema_version
                && event_type == event.event_type
                && correlation_id == event.correlation_id
                && causation_id == event.causation_id
            {
                return Ok(false);
            }
            anyhow::bail!("event id already exists with different content or lineage");
        }

        let existing_by_source: Option<(String, String, String)> = connection
            .query_row(
                "SELECT event_id, content_hash, event_type FROM events WHERE session_id = ?1 AND source_id = ?2 AND sequence = ?3",
                params![event.session_id, event.source_id, event.sequence],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        if let Some((_, content_hash, event_type)) = existing_by_source {
            if content_hash == event.content_hash && event_type == event.event_type {
                return Ok(false);
            }
            anyhow::bail!("event source sequence already exists with different content");
        }

        anyhow::bail!("event insertion was ignored by an unknown uniqueness conflict");
    }

    pub fn list_events(&self, session_id: &str) -> Result<Vec<EventEnvelope>> {
        let connection = self.lock()?;
        let mut statement = connection.prepare(
            "SELECT event_id, schema_version, session_id, source_id, sequence, event_type, captured_at_monotonic_ns, captured_at_wall, ingested_at, correlation_id, causation_id, content_hash, payload_json FROM events WHERE session_id = ?1 ORDER BY ingested_at, event_id",
        )?;
        let events = statement
            .query_map([session_id], map_event)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(events)
    }

    /// Sequence allocation resumes from persisted data after a process restart.
    pub fn next_sequence(&self, session_id: &str, source_id: &str) -> Result<u64> {
        let connection = self.lock()?;
        let maximum: Option<u64> = connection.query_row(
            "SELECT MAX(sequence) FROM events WHERE session_id = ?1 AND source_id = ?2",
            params![session_id, source_id],
            |row| row.get(0),
        )?;
        Ok(maximum.unwrap_or(0).saturating_add(1))
    }

    pub fn insert_audio_chunk(&self, chunk: &AudioChunkRecord) -> Result<bool> {
        let connection = self.lock()?;
        let changed = connection.execute(
            "INSERT OR IGNORE INTO audio_chunks(session_id, source_id, sequence, captured_at_ms, sample_rate, channels, encoding, duration_ms, object_hash, size_bytes, acknowledged_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                chunk.session_id,
                chunk.source_id,
                chunk.sequence,
                chunk.captured_at_ms,
                chunk.sample_rate,
                chunk.channels,
                chunk.encoding,
                chunk.duration_ms,
                chunk.object_hash,
                chunk.size_bytes,
                chunk.acknowledged_at.to_rfc3339(),
            ],
        )?;
        if changed == 0 {
            let existing: (u64, u32, u16, String, u32, String, u64) = connection.query_row(
                "SELECT captured_at_ms, sample_rate, channels, encoding, duration_ms, object_hash, size_bytes FROM audio_chunks WHERE session_id = ?1 AND source_id = ?2 AND sequence = ?3",
                params![chunk.session_id, chunk.source_id, chunk.sequence],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?, row.get(6)?)),
            )?;
            let incoming = (
                chunk.captured_at_ms,
                chunk.sample_rate,
                chunk.channels,
                chunk.encoding.clone(),
                chunk.duration_ms,
                chunk.object_hash.clone(),
                chunk.size_bytes,
            );
            if existing != incoming {
                anyhow::bail!("audio sequence already exists with different content or metadata");
            }
        }
        Ok(changed == 1)
    }

    /// Commit the immutable audio manifest and its durable ingest fact together.
    ///
    /// The object bytes are written by `ObjectStore` before this method is called.
    /// If either SQLite insert fails the transaction rolls back, so the caller never
    /// returns an ACK for a manifest without its corresponding fact.
    pub fn insert_audio_chunk_and_event(
        &self,
        chunk: &AudioChunkRecord,
        event: &EventEnvelope,
    ) -> Result<bool> {
        event.validate_version()?;
        let mut connection = self.lock()?;
        let transaction = connection.transaction()?;
        let changed = transaction.execute(
            "INSERT OR IGNORE INTO audio_chunks(session_id, source_id, sequence, captured_at_ms, sample_rate, channels, encoding, duration_ms, object_hash, size_bytes, acknowledged_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                chunk.session_id,
                chunk.source_id,
                chunk.sequence,
                chunk.captured_at_ms,
                chunk.sample_rate,
                chunk.channels,
                chunk.encoding,
                chunk.duration_ms,
                chunk.object_hash,
                chunk.size_bytes,
                chunk.acknowledged_at.to_rfc3339(),
            ],
        )?;
        if changed == 0 {
            let existing: (u64, u32, u16, String, u32, String, u64) = transaction.query_row(
                "SELECT captured_at_ms, sample_rate, channels, encoding, duration_ms, object_hash, size_bytes FROM audio_chunks WHERE session_id = ?1 AND source_id = ?2 AND sequence = ?3",
                params![chunk.session_id, chunk.source_id, chunk.sequence],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?, row.get(6)?)),
            )?;
            let incoming = (
                chunk.captured_at_ms,
                chunk.sample_rate,
                chunk.channels,
                chunk.encoding.clone(),
                chunk.duration_ms,
                chunk.object_hash.clone(),
                chunk.size_bytes,
            );
            if existing != incoming {
                anyhow::bail!("audio sequence already exists with different content or metadata");
            }
            // A pre-transaction deployment may have persisted the manifest
            // without its ingest fact.  Repair that narrow inconsistency while
            // still rejecting any event-id or event-cursor collision with a
            // different payload.
            let event_id = event.event_id.to_string();
            let existing_by_id: Option<EventIdentityRow> = transaction
                .query_row(
                    "SELECT content_hash, session_id, source_id, sequence, schema_version, event_type, correlation_id, causation_id FROM events WHERE event_id = ?1",
                    [&event_id],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?, row.get(6)?, row.get(7)?)),
                )
                .optional()?;
            if let Some((
                content_hash,
                session_id,
                source_id,
                _sequence,
                schema_version,
                event_type,
                correlation_id,
                causation_id,
            )) = existing_by_id
            {
                if content_hash == event.content_hash
                    && session_id == event.session_id
                    && source_id == event.source_id
                    && schema_version == event.schema_version
                    && event_type == event.event_type
                    && correlation_id == event.correlation_id
                    && causation_id == event.causation_id
                {
                    // The event sequence is the server-side timeline cursor, not
                    // the client audio sequence.  A retransmission can arrive
                    // after another source event advanced that cursor, so the
                    // deterministic audio commit ID is the idempotency boundary.
                    transaction.commit()?;
                    return Ok(false);
                }
                anyhow::bail!("audio ingest event id already exists with different content");
            }
            let existing_by_source: Option<(String, String, String)> = transaction
                .query_row(
                    "SELECT event_id, content_hash, event_type FROM events WHERE session_id = ?1 AND source_id = ?2 AND sequence = ?3",
                    params![event.session_id, event.source_id, event.sequence],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .optional()?;
            if let Some((_, content_hash, event_type)) = existing_by_source {
                if content_hash == event.content_hash && event_type == event.event_type {
                    transaction.commit()?;
                    return Ok(false);
                }
                anyhow::bail!("audio ingest event sequence already exists with different content");
            }
            transaction.execute(
                "INSERT INTO events(event_id, schema_version, session_id, source_id, sequence, event_type, captured_at_monotonic_ns, captured_at_wall, ingested_at, correlation_id, causation_id, content_hash, payload_json) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
                params![
                    event_id,
                    event.schema_version,
                    event.session_id,
                    event.source_id,
                    event.sequence,
                    event.event_type,
                    event.captured_at_monotonic_ns,
                    event.captured_at_wall.to_rfc3339(),
                    event.ingested_at.to_rfc3339(),
                    event.correlation_id,
                    event.causation_id,
                    event.content_hash,
                    serde_json::to_string(&event.payload)?,
                ],
            )?;
            transaction.commit()?;
            return Ok(true);
        }

        transaction.execute(
            "INSERT INTO events(event_id, schema_version, session_id, source_id, sequence, event_type, captured_at_monotonic_ns, captured_at_wall, ingested_at, correlation_id, causation_id, content_hash, payload_json) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            params![
                event.event_id.to_string(),
                event.schema_version,
                event.session_id,
                event.source_id,
                event.sequence,
                event.event_type,
                event.captured_at_monotonic_ns,
                event.captured_at_wall.to_rfc3339(),
                event.ingested_at.to_rfc3339(),
                event.correlation_id,
                event.causation_id,
                event.content_hash,
                serde_json::to_string(&event.payload)?,
            ],
        )?;
        transaction.commit()?;
        Ok(true)
    }

    pub fn list_unassembled_audio_chunks(
        &self,
        session_id: &str,
        source_id: &str,
    ) -> Result<Vec<AudioChunkRecord>> {
        let connection = self.lock()?;
        let cursor: i64 = connection
            .query_row(
                "SELECT last_assembled_sequence FROM audio_assembly_cursors WHERE session_id = ?1 AND source_id = ?2",
                params![session_id, source_id],
                |row| row.get(0),
            )
            .optional()?
            .unwrap_or(-1);
        let mut statement = connection.prepare(
            "SELECT session_id, source_id, sequence, captured_at_ms, sample_rate, channels, encoding, duration_ms, object_hash, size_bytes, acknowledged_at FROM audio_chunks WHERE session_id = ?1 AND source_id = ?2 AND sequence > ?3 ORDER BY sequence",
        )?;
        Ok(statement
            .query_map(params![session_id, source_id, cursor], map_audio_chunk)?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn audio_assembly_cursor(&self, session_id: &str, source_id: &str) -> Result<Option<u64>> {
        let connection = self.lock()?;
        connection
            .query_row(
                "SELECT last_assembled_sequence FROM audio_assembly_cursors WHERE session_id = ?1 AND source_id = ?2",
                params![session_id, source_id],
                |row| row.get(0),
            )
            .optional()
            .context("query audio assembly cursor")
    }

    pub fn list_audio_sources_with_pending_chunks(&self) -> Result<Vec<(String, String)>> {
        let connection = self.lock()?;
        let mut statement = connection.prepare(
            "SELECT DISTINCT c.session_id, c.source_id FROM audio_chunks c LEFT JOIN audio_assembly_cursors a ON a.session_id = c.session_id AND a.source_id = c.source_id WHERE c.sequence > COALESCE(a.last_assembled_sequence, -1) ORDER BY c.session_id, c.source_id",
        )?;
        Ok(statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn record_audio_window_and_advance(&self, window: &AudioWindowRecord) -> Result<bool> {
        let mut connection = self.lock()?;
        let transaction = connection.transaction()?;
        let inserted = transaction.execute(
            "INSERT OR IGNORE INTO audio_windows(id, session_id, source_id, first_sequence, last_sequence, captured_at_ms, duration_ms, object_hash, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![window.id, window.session_id, window.source_id, window.first_sequence, window.last_sequence, window.captured_at_ms, window.duration_ms, window.object_hash, window.created_at.to_rfc3339()],
        )? == 1;
        transaction.execute(
            "INSERT INTO audio_assembly_cursors(session_id, source_id, last_assembled_sequence, updated_at) VALUES (?1, ?2, ?3, ?4) ON CONFLICT(session_id, source_id) DO UPDATE SET last_assembled_sequence = MAX(last_assembled_sequence, excluded.last_assembled_sequence), updated_at = excluded.updated_at",
            params![window.session_id, window.source_id, window.last_sequence, Utc::now().to_rfc3339()],
        )?;
        transaction.commit()?;
        Ok(inserted)
    }

    pub fn acquire_recording_lease(
        &self,
        project_id: &str,
        session_id: &str,
        device_id: &str,
        token_hash: &str,
        ttl_seconds: i64,
    ) -> Result<LeaseAcquireOutcome> {
        let now = Utc::now();
        let expires_at = now + chrono::Duration::seconds(ttl_seconds.clamp(15, 120));
        let mut connection = self.lock()?;
        let transaction = connection.transaction()?;
        let existing = transaction
            .query_row(
                "SELECT project_id, session_id, holder_device_id, lease_token_hash, generation, heartbeat_at, expires_at FROM recording_leases WHERE project_id = ?1",
                [project_id],
                map_recording_lease,
            )
            .optional()?;
        if let Some(record) = existing.as_ref().filter(|record| record.expires_at > now) {
            return Ok(LeaseAcquireOutcome::Conflict(record.clone()));
        }
        let generation = existing.map(|record| record.generation + 1).unwrap_or(1);
        transaction.execute(
            "INSERT INTO recording_leases(project_id, session_id, holder_device_id, lease_token_hash, generation, heartbeat_at, expires_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7) ON CONFLICT(project_id) DO UPDATE SET session_id = excluded.session_id, holder_device_id = excluded.holder_device_id, lease_token_hash = excluded.lease_token_hash, generation = excluded.generation, heartbeat_at = excluded.heartbeat_at, expires_at = excluded.expires_at",
            params![project_id, session_id, device_id, token_hash, generation, now.to_rfc3339(), expires_at.to_rfc3339()],
        )?;
        transaction.commit()?;
        Ok(LeaseAcquireOutcome::Acquired(RecordingLeaseRecord {
            project_id: project_id.to_owned(),
            session_id: session_id.to_owned(),
            holder_device_id: device_id.to_owned(),
            lease_token_hash: token_hash.to_owned(),
            generation,
            heartbeat_at: now,
            expires_at,
        }))
    }

    pub fn renew_recording_lease(
        &self,
        project_id: &str,
        session_id: &str,
        device_id: &str,
        token_hash: &str,
        ttl_seconds: i64,
    ) -> Result<Option<RecordingLeaseRecord>> {
        let now = Utc::now();
        let expires_at = now + chrono::Duration::seconds(ttl_seconds.clamp(15, 120));
        let connection = self.lock()?;
        let changed = connection.execute(
            "UPDATE recording_leases SET heartbeat_at = ?5, expires_at = ?6 WHERE project_id = ?1 AND session_id = ?2 AND holder_device_id = ?3 AND lease_token_hash = ?4",
            params![project_id, session_id, device_id, token_hash, now.to_rfc3339(), expires_at.to_rfc3339()],
        )?;
        drop(connection);
        if changed == 0 {
            Ok(None)
        } else {
            self.get_recording_lease(project_id)
        }
    }

    pub fn get_recording_lease(&self, project_id: &str) -> Result<Option<RecordingLeaseRecord>> {
        let connection = self.lock()?;
        connection
            .query_row(
                "SELECT project_id, session_id, holder_device_id, lease_token_hash, generation, heartbeat_at, expires_at FROM recording_leases WHERE project_id = ?1",
                [project_id],
                map_recording_lease,
            )
            .optional()
            .context("query recording lease")
    }

    pub fn validate_recording_lease(
        &self,
        session_id: &str,
        token_hash: &str,
    ) -> Result<Option<RecordingLeaseRecord>> {
        let connection = self.lock()?;
        connection
            .query_row(
                "SELECT project_id, session_id, holder_device_id, lease_token_hash, generation, heartbeat_at, expires_at FROM recording_leases WHERE session_id = ?1 AND lease_token_hash = ?2 AND expires_at > ?3",
                params![session_id, token_hash, Utc::now().to_rfc3339()],
                map_recording_lease,
            )
            .optional()
            .context("validate recording lease")
    }

    pub fn release_recording_lease(
        &self,
        project_id: &str,
        session_id: &str,
        token_hash: &str,
    ) -> Result<bool> {
        let connection = self.lock()?;
        Ok(connection.execute(
            "DELETE FROM recording_leases WHERE project_id = ?1 AND session_id = ?2 AND lease_token_hash = ?3",
            params![project_id, session_id, token_hash],
        )? == 1)
    }

    pub fn insert_asset(&self, asset: &AssetRecord) -> Result<()> {
        let connection = self.lock()?;
        connection.execute(
            "INSERT INTO assets(id, session_id, original_name, media_type, object_hash, size_bytes, status, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![asset.id, asset.session_id, asset.original_name, asset.media_type, asset.object_hash, asset.size_bytes, asset.status, asset.created_at.to_rfc3339()],
        )?;
        Ok(())
    }

    pub fn get_asset(&self, session_id: &str, asset_id: &str) -> Result<Option<AssetRecord>> {
        let connection = self.lock()?;
        connection
            .query_row(
                "SELECT id, session_id, original_name, media_type, object_hash, size_bytes, status, created_at FROM assets WHERE session_id = ?1 AND id = ?2",
                params![session_id, asset_id],
                |row| {
                    let created: String = row.get(7)?;
                    Ok(AssetRecord {
                        id: row.get(0)?,
                        session_id: row.get(1)?,
                        original_name: row.get(2)?,
                        media_type: row.get(3)?,
                        object_hash: row.get(4)?,
                        size_bytes: row.get(5)?,
                        status: row.get(6)?,
                        created_at: DateTime::parse_from_rfc3339(&created)
                            .map_err(|_| rusqlite::Error::InvalidQuery)?
                            .with_timezone(&Utc),
                    })
                },
            )
            .optional()
            .context("query asset")
    }

    pub fn insert_asset_page(&self, page: &AssetPageRecord) -> Result<()> {
        let connection = self.lock()?;
        connection.execute(
            "INSERT OR REPLACE INTO asset_pages(id, asset_id, page_number, title, text_content, object_hash, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![page.id, page.asset_id, page.page_number, page.title, page.text_content, page.object_hash, page.created_at.to_rfc3339()],
        )?;
        Ok(())
    }

    /// Idempotency keys make queue creation safe when an audio frame or completion is retried.
    pub fn enqueue_model_job(&self, job: &NewModelJob) -> Result<ModelJobRecord> {
        let now = Utc::now();
        // Deferred explanation jobs are visible in the queue immediately, but
        // remain held until Core activates them after their material and
        // transcript dependencies are complete.
        let available_at = if job
            .input
            .get("deferred_material")
            .and_then(serde_json::Value::as_bool)
            == Some(true)
        {
            now + chrono::Duration::days(3650)
        } else {
            now
        };
        let connection = self.lock()?;
        connection.execute(
            "INSERT OR IGNORE INTO model_jobs(id, session_id, job_type, priority, status, input_json, input_object_hash, idempotency_key, attempts, available_at, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, 'queued', ?5, ?6, ?7, 0, ?8, ?8, ?8)",
            params![
                job.id,
                job.session_id,
                job.job_type,
                job.priority,
                serde_json::to_string(&job.input)?,
                job.input_object_hash,
                job.idempotency_key,
                available_at.to_rfc3339(),
            ],
        )?;
        drop(connection);
        self.get_model_job_by_key(&job.idempotency_key)?
            .context("model job disappeared after enqueue")
    }

    /// Atomically coalesce confirmed material uploads into one waiting explanation.
    ///
    /// The immediate transaction matters because two browser tabs can confirm different
    /// uploads at nearly the same time.  A read-then-update sequence could otherwise create
    /// two independent explanation jobs before either request observes the other one.
    pub fn enqueue_or_merge_deferred_explanation(
        &self,
        job: &NewModelJob,
        asset_id: &str,
        parse_job_id: &str,
    ) -> Result<ModelJobRecord> {
        let now = Utc::now();
        let available_at = now + chrono::Duration::days(3650);
        let mut connection = self.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let existing = {
            let mut statement = transaction.prepare(
                "SELECT id, input_json FROM model_jobs WHERE session_id = ?1 AND job_type = 'explain' AND status = 'queued' ORDER BY created_at, id",
            )?;
            let rows = statement.query_map([&job.session_id], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?;
            let mut found = None;
            for row in rows {
                let (id, input_json) = row?;
                let input: Value = serde_json::from_str(&input_json)?;
                if input.get("deferred_material").and_then(Value::as_bool) == Some(true) {
                    found = Some((id, input));
                    break;
                }
            }
            found
        };
        let result_id = if let Some((existing_id, mut input)) = existing {
            append_unique_json_string(&mut input, "asset_ids", asset_id);
            append_unique_json_string(&mut input, "depends_on_job_ids", parse_job_id);
            transaction.execute(
                "UPDATE model_jobs SET input_json = ?2, updated_at = ?3 WHERE id = ?1 AND status = 'queued'",
                params![existing_id, serde_json::to_string(&input)?, now.to_rfc3339()],
            )?;
            existing_id
        } else {
            transaction.execute(
                "INSERT INTO model_jobs(id, session_id, job_type, priority, status, input_json, input_object_hash, idempotency_key, attempts, available_at, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, 'queued', ?5, ?6, ?7, 0, ?8, ?8, ?8)",
                params![
                    job.id,
                    job.session_id,
                    job.job_type,
                    job.priority,
                    serde_json::to_string(&job.input)?,
                    job.input_object_hash,
                    job.idempotency_key,
                    available_at.to_rfc3339(),
                ],
            )?;
            job.id.clone()
        };
        transaction.commit()?;
        drop(connection);
        self.get_model_job(&result_id)?
            .context("deferred explanation disappeared after coalescing")
    }

    pub fn get_model_job(&self, job_id: &str) -> Result<Option<ModelJobRecord>> {
        let connection = self.lock()?;
        connection
            .query_row(
                "SELECT id, session_id, job_type, priority, status, input_json, input_object_hash, result_json, idempotency_key, attempts, available_at, lease_owner, lease_expires_at, last_error_kind, created_at, updated_at, completed_at FROM model_jobs WHERE id = ?1",
                [job_id],
                map_model_job,
            )
            .optional()
            .context("query model job")
    }

    pub fn get_model_job_by_key(&self, key: &str) -> Result<Option<ModelJobRecord>> {
        let connection = self.lock()?;
        connection
            .query_row(
                "SELECT id, session_id, job_type, priority, status, input_json, input_object_hash, result_json, idempotency_key, attempts, available_at, lease_owner, lease_expires_at, last_error_kind, created_at, updated_at, completed_at FROM model_jobs WHERE idempotency_key = ?1",
                [key],
                map_model_job,
            )
            .optional()
            .context("query model job by idempotency key")
    }

    /// Return the single queued material-triggered explanation that is waiting
    /// for its dependencies.  Leased explanations are deliberately excluded:
    /// their input has already been handed to a worker and must not change.
    pub fn find_pending_deferred_explanation(
        &self,
        session_id: &str,
    ) -> Result<Option<ModelJobRecord>> {
        let connection = self.lock()?;
        let mut statement = connection.prepare(
            "SELECT id, session_id, job_type, priority, status, input_json, input_object_hash, result_json, idempotency_key, attempts, available_at, lease_owner, lease_expires_at, last_error_kind, created_at, updated_at, completed_at FROM model_jobs WHERE session_id = ?1 AND job_type = 'explain' AND status = 'queued' ORDER BY created_at, id",
        )?;
        let mut rows = statement.query([session_id])?;
        while let Some(row) = rows.next()? {
            let job = map_model_job(row)?;
            if job.input.get("deferred_material").and_then(Value::as_bool) == Some(true) {
                return Ok(Some(job));
            }
        }
        Ok(None)
    }

    /// Merge new dependency metadata into a queued job without making it
    /// runnable.  This is used by consecutive confirmed uploads.
    pub fn update_model_job_input(&self, job_id: &str, input: &Value) -> Result<bool> {
        let connection = self.lock()?;
        Ok(connection.execute(
            "UPDATE model_jobs SET input_json = ?2, updated_at = ?3 WHERE id = ?1 AND status = 'queued'",
            params![job_id, serde_json::to_string(input)?, Utc::now().to_rfc3339()],
        )? == 1)
    }

    /// Activate a deferred explanation only after Core has materialized a
    /// stable transcript/material snapshot.  The update is atomic with the
    /// queued-state check so a worker cannot observe a half-activated job.
    pub fn activate_model_job(&self, job_id: &str, input: &Value) -> Result<bool> {
        let now = Utc::now().to_rfc3339();
        let connection = self.lock()?;
        Ok(connection.execute(
            "UPDATE model_jobs SET input_json = ?2, available_at = ?3, updated_at = ?3 WHERE id = ?1 AND status = 'queued'",
            params![job_id, serde_json::to_string(input)?, now],
        )? == 1)
    }

    /// A terminal material parse failure makes its waiting explanation
    /// explicitly failed instead of leaving an invisible queued tombstone.
    pub fn fail_deferred_explanations_for_dependency(
        &self,
        dependency_job_id: &str,
        error_kind: &str,
    ) -> Result<Vec<ModelJobRecord>> {
        let now = Utc::now().to_rfc3339();
        let mut connection = self.lock()?;
        let transaction = connection.transaction()?;
        let mut statement = transaction.prepare(
            "SELECT id, input_json FROM model_jobs WHERE job_type = 'explain' AND status = 'queued'",
        )?;
        let mut matching = Vec::new();
        let rows = statement.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        for row in rows {
            let (job_id, input_json) = row?;
            let input: Value = serde_json::from_str(&input_json)?;
            let depends = input
                .get("depends_on_job_ids")
                .and_then(Value::as_array)
                .is_some_and(|items| {
                    items
                        .iter()
                        .any(|item| item.as_str() == Some(dependency_job_id))
                });
            if depends && input.get("deferred_material").and_then(Value::as_bool) == Some(true) {
                matching.push(job_id);
            }
        }
        drop(statement);
        for job_id in &matching {
            transaction.execute(
                "UPDATE model_jobs SET status = 'failed', available_at = ?2, last_error_kind = ?3, updated_at = ?2, completed_at = ?2 WHERE id = ?1 AND status = 'queued'",
                params![job_id, now, error_kind],
            )?;
        }
        transaction.commit()?;
        drop(connection);
        matching
            .into_iter()
            .map(|job_id| {
                self.get_model_job(&job_id)?
                    .context("deferred explanation disappeared after failure")
            })
            .collect()
    }

    /// A user-triggered retry can reopen a visible summary failure without
    /// creating a second job for the same evidence snapshot.
    pub fn requeue_failed_summary_by_key(&self, key: &str) -> Result<Option<ModelJobRecord>> {
        let now = Utc::now().to_rfc3339();
        let connection = self.lock()?;
        connection.execute(
            "UPDATE model_jobs SET status = 'queued', attempts = 0, available_at = ?2, lease_owner = NULL, lease_expires_at = NULL, last_error_kind = NULL, updated_at = ?2, completed_at = NULL WHERE idempotency_key = ?1 AND job_type = 'summarize' AND status = 'failed'",
            params![key, now],
        )?;
        drop(connection);
        self.get_model_job_by_key(key)
    }

    /// Expired leases return to the queue before one compatible job is leased atomically.
    pub fn lease_model_job(
        &self,
        worker_id: &str,
        capabilities: &[String],
        lease_seconds: i64,
    ) -> Result<Option<ModelJobRecord>> {
        self.lease_model_job_for(worker_id, capabilities, lease_seconds, None)
    }

    /// Lease a compatible job, optionally restricting selection to one known job ID.
    ///
    /// The normal GPU agent leaves `requested_job_id` empty and keeps priority ordering. A
    /// targeted lease is useful for recovery tools and isolated authorization checks when a
    /// shared queue already contains unrelated work.
    pub fn lease_model_job_for(
        &self,
        worker_id: &str,
        capabilities: &[String],
        lease_seconds: i64,
        requested_job_id: Option<&str>,
    ) -> Result<Option<ModelJobRecord>> {
        let now = Utc::now();
        let expires = now + chrono::Duration::seconds(lease_seconds.clamp(15, 300));
        let mut connection = self.lock()?;
        let transaction = connection.transaction()?;
        transaction.execute(
            "UPDATE model_jobs SET status = 'queued', lease_owner = NULL, lease_expires_at = NULL, updated_at = ?1 WHERE status = 'leased' AND lease_expires_at <= ?1",
            [now.to_rfc3339()],
        )?;
        let selected_id = {
            let mut statement = transaction.prepare(
                "SELECT id, job_type, input_json FROM model_jobs WHERE status = 'queued' AND available_at <= ?1 AND (?2 IS NULL OR id = ?2) ORDER BY priority DESC, created_at, id LIMIT 100",
            )?;
            let candidates = statement
                .query_map(
                    rusqlite::params![now.to_rfc3339(), requested_job_id],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, String>(2)?,
                        ))
                    },
                )?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            candidates
                .into_iter()
                .find(|(_, job_type, input_json)| {
                    capabilities.iter().any(|item| item == job_type)
                        && deferred_model_job_ready(&transaction, input_json).unwrap_or(false)
                })
                .map(|(id, _, _)| id)
        };
        let Some(job_id) = selected_id else {
            transaction.commit()?;
            return Ok(None);
        };
        let changed = transaction.execute(
            "UPDATE model_jobs SET status = 'leased', attempts = attempts + 1, lease_owner = ?2, lease_expires_at = ?3, updated_at = ?4 WHERE id = ?1 AND status = 'queued'",
            params![job_id, worker_id, expires.to_rfc3339(), now.to_rfc3339()],
        )?;
        if changed == 1 {
            transaction.execute(
                "INSERT OR IGNORE INTO model_job_metrics(job_id, first_leased_at) VALUES (?1, ?2)",
                params![job_id, now.to_rfc3339()],
            )?;
        }
        transaction.commit()?;
        if changed != 1 {
            return Ok(None);
        }
        drop(connection);
        self.get_model_job(&job_id)
    }

    pub fn renew_model_job(
        &self,
        job_id: &str,
        worker_id: &str,
        lease_seconds: i64,
    ) -> Result<bool> {
        let now = Utc::now();
        let expires = now + chrono::Duration::seconds(lease_seconds.clamp(15, 300));
        let connection = self.lock()?;
        Ok(connection.execute(
            "UPDATE model_jobs SET lease_expires_at = ?3, updated_at = ?4 WHERE id = ?1 AND status = 'leased' AND lease_owner = ?2",
            params![job_id, worker_id, expires.to_rfc3339(), now.to_rfc3339()],
        )? == 1)
    }

    pub fn complete_model_job(
        &self,
        job_id: &str,
        worker_id: &str,
        result: &Value,
    ) -> Result<bool> {
        let now = Utc::now();
        let connection = self.lock()?;
        Ok(connection.execute(
            "UPDATE model_jobs SET status = 'completed', result_json = ?3, lease_owner = NULL, lease_expires_at = NULL, completed_at = ?4, updated_at = ?4 WHERE id = ?1 AND status = 'leased' AND lease_owner = ?2",
            params![job_id, worker_id, serde_json::to_string(result)?, now.to_rfc3339()],
        )? == 1)
    }

    pub fn retry_or_fail_model_job(
        &self,
        job_id: &str,
        worker_id: &str,
        error_kind: &str,
        retryable: bool,
        retry_after_seconds: i64,
    ) -> Result<Option<String>> {
        let now = Utc::now();
        let connection = self.lock()?;
        let attempt_info: Option<(u32, String)> = connection
            .query_row(
                "SELECT attempts, job_type FROM model_jobs WHERE id = ?1 AND status = 'leased' AND lease_owner = ?2",
                params![job_id, worker_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let Some((attempts, job_type)) = attempt_info else {
            return Ok(None);
        };
        // Background summaries are intentionally bounded more tightly than
        // realtime work.  Repeating a slow 14B generation five times made a
        // provider hiccup look like a frozen session and amplified GPU load.
        let max_attempts = match job_type.as_str() {
            "summarize" => 2,
            "explain" | "asset_parse" => 3,
            _ => 5,
        };
        let status = if retryable && attempts < max_attempts {
            "queued"
        } else {
            "failed"
        };
        let available = now + chrono::Duration::seconds(retry_after_seconds.clamp(1, 60));
        connection.execute(
            "UPDATE model_jobs SET status = ?3, available_at = ?4, lease_owner = NULL, lease_expires_at = NULL, last_error_kind = ?5, updated_at = ?6 WHERE id = ?1 AND lease_owner = ?2",
            params![job_id, worker_id, status, available.to_rfc3339(), error_kind, now.to_rfc3339()],
        )?;
        Ok(Some(status.to_owned()))
    }

    pub fn model_queue_counts(&self, session_id: Option<&str>) -> Result<ModelQueueCounts> {
        let connection = self.lock()?;
        let query = |status: &str| -> Result<u64> {
            let value = if let Some(session_id) = session_id {
                connection.query_row(
                    "SELECT COUNT(*) FROM model_jobs WHERE session_id = ?1 AND status = ?2",
                    params![session_id, status],
                    |row| row.get(0),
                )?
            } else {
                connection.query_row(
                    "SELECT COUNT(*) FROM model_jobs WHERE status = ?1",
                    [status],
                    |row| row.get(0),
                )?
            };
            Ok(value)
        };
        Ok(ModelQueueCounts {
            queued: query("queued")?,
            leased: query("leased")?,
            completed: query("completed")?,
            failed: query("failed")?,
        })
    }

    pub fn oldest_active_model_job_at(&self, job_type: &str) -> Result<Option<DateTime<Utc>>> {
        let connection = self.lock()?;
        let value = connection.query_row(
            "SELECT MIN(created_at) FROM model_jobs WHERE job_type = ?1 AND status IN ('queued', 'leased')",
            [job_type],
            |row| row.get::<_, Option<String>>(0),
        )?;
        Ok(value.map(parse_time).transpose()?)
    }

    /// Summary failures are visible and retryable, while failures in the
    /// realtime fact pipeline still make a session fail closed.
    pub fn has_failed_non_summary_job(&self, session_id: &str) -> Result<bool> {
        let connection = self.lock()?;
        let mut statement = connection.prepare(
            "SELECT job_type, input_json FROM model_jobs WHERE session_id = ?1 AND status = 'failed'",
        )?;
        let rows = statement.query_map([session_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        for row in rows {
            let (job_type, input_json) = row?;
            if job_type == "summarize" {
                continue;
            }
            let input: Value = serde_json::from_str(&input_json)?;
            if job_type == "explain"
                && input.get("deferred_material").and_then(Value::as_bool) == Some(true)
            {
                continue;
            }
            return Ok(true);
        }
        Ok(false)
    }

    /// Count queued or leased work of selected kinds while excluding the job whose
    /// completion is currently being committed.  This lets background teaching work
    /// wait for the realtime ASR and translation lanes instead of starting a long,
    /// non-preemptible generation in front of fresh captions.
    pub fn active_model_jobs_excluding(
        &self,
        session_id: &str,
        job_types: &[&str],
        excluded_job_id: &str,
    ) -> Result<u64> {
        if job_types.is_empty() {
            return Ok(0);
        }
        let connection = self.lock()?;
        let placeholders = (0..job_types.len())
            .map(|index| format!("?{}", index + 3))
            .collect::<Vec<_>>()
            .join(",");
        let query = format!(
            "SELECT COUNT(*) FROM model_jobs WHERE session_id = ?1 AND id <> ?2 AND status IN ('queued', 'leased') AND job_type IN ({placeholders})"
        );
        let mut values = Vec::<rusqlite::types::Value>::with_capacity(job_types.len() + 2);
        values.push(session_id.to_owned().into());
        values.push(excluded_job_id.to_owned().into());
        values.extend(job_types.iter().map(|value| (*value).to_owned().into()));
        Ok(connection.query_row(&query, rusqlite::params_from_iter(values), |row| row.get(0))?)
    }

    pub fn heartbeat_worker(&self, heartbeat: &WorkerHeartbeat) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        let connection = self.lock()?;
        connection.execute(
            "INSERT INTO worker_nodes(id, status, capabilities_json, model_metadata_json, active_job_id, last_seen_at, updated_at) VALUES (?1, 'online', ?2, ?3, ?4, ?5, ?5) ON CONFLICT(id) DO UPDATE SET status = 'online', capabilities_json = excluded.capabilities_json, model_metadata_json = excluded.model_metadata_json, active_job_id = excluded.active_job_id, last_seen_at = excluded.last_seen_at, updated_at = excluded.updated_at",
            params![heartbeat.id, serde_json::to_string(&heartbeat.capabilities)?, serde_json::to_string(&heartbeat.model_metadata)?, heartbeat.active_job_id, now],
        )?;
        Ok(())
    }

    pub fn latest_worker(&self) -> Result<Option<WorkerNodeRecord>> {
        let connection = self.lock()?;
        connection
            .query_row(
                "SELECT id, status, capabilities_json, model_metadata_json, active_job_id, last_seen_at FROM worker_nodes ORDER BY last_seen_at DESC LIMIT 1",
                [],
                |row| {
                    let capabilities: String = row.get(2)?;
                    let metadata: String = row.get(3)?;
                    let last_seen: String = row.get(5)?;
                    Ok(WorkerNodeRecord {
                        id: row.get(0)?,
                        status: row.get(1)?,
                        capabilities: serde_json::from_str(&capabilities)
                            .map_err(|_| rusqlite::Error::InvalidQuery)?,
                        model_metadata: serde_json::from_str(&metadata)
                            .map_err(|_| rusqlite::Error::InvalidQuery)?,
                        active_job_id: row.get(4)?,
                        last_seen_at: DateTime::parse_from_rfc3339(&last_seen)
                            .map_err(|_| rusqlite::Error::InvalidQuery)?
                            .with_timezone(&Utc),
                    })
                },
            )
            .optional()
            .context("query latest worker")
    }

    pub fn list_workers(&self) -> Result<Vec<WorkerNodeRecord>> {
        let connection = self.lock()?;
        let mut statement = connection.prepare(
            "SELECT id, status, capabilities_json, model_metadata_json, active_job_id, last_seen_at FROM worker_nodes ORDER BY last_seen_at DESC",
        )?;
        let rows = statement.query_map([], |row| {
            let capabilities: String = row.get(2)?;
            let metadata: String = row.get(3)?;
            let last_seen: String = row.get(5)?;
            Ok(WorkerNodeRecord {
                id: row.get(0)?,
                status: row.get(1)?,
                capabilities: serde_json::from_str(&capabilities)
                    .map_err(|_| rusqlite::Error::InvalidQuery)?,
                model_metadata: serde_json::from_str(&metadata)
                    .map_err(|_| rusqlite::Error::InvalidQuery)?,
                active_job_id: row.get(4)?,
                last_seen_at: DateTime::parse_from_rfc3339(&last_seen)
                    .map_err(|_| rusqlite::Error::InvalidQuery)?
                    .with_timezone(&Utc),
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn enqueue_connector_job(&self, job: &NewConnectorJob) -> Result<ConnectorJobRecord> {
        let now = Utc::now();
        let available_at = now + chrono::Duration::seconds(job.delay_seconds.clamp(0, 3_600));
        let connection = self.lock()?;
        connection.execute(
            "INSERT OR IGNORE INTO connector_jobs(id, project_id, session_id, connector, job_type, status, payload_json, idempotency_key, attempts, available_at, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, 'queued', ?6, ?7, 0, ?8, ?9, ?9)",
            params![job.id, job.project_id, job.session_id, job.connector, job.job_type, serde_json::to_string(&job.payload)?, job.idempotency_key, available_at.to_rfc3339(), now.to_rfc3339()],
        )?;
        drop(connection);
        self.get_connector_job_by_key(&job.idempotency_key)?
            .context("connector job disappeared after enqueue")
    }

    pub fn get_connector_job_by_key(&self, key: &str) -> Result<Option<ConnectorJobRecord>> {
        let connection = self.lock()?;
        connection
            .query_row(
                "SELECT id, project_id, session_id, connector, job_type, status, payload_json, idempotency_key, attempts, available_at, lease_owner, lease_expires_at, last_error_kind, created_at, updated_at, completed_at FROM connector_jobs WHERE idempotency_key = ?1",
                [key],
                map_connector_job,
            )
            .optional()
            .context("query connector job")
    }

    pub fn lease_connector_job(
        &self,
        owner: &str,
        lease_seconds: i64,
    ) -> Result<Option<ConnectorJobRecord>> {
        let now = Utc::now();
        let expires = now + chrono::Duration::seconds(lease_seconds.clamp(15, 300));
        let mut connection = self.lock()?;
        let transaction = connection.transaction()?;
        transaction.execute(
            "UPDATE connector_jobs SET status = 'queued', lease_owner = NULL, lease_expires_at = NULL, updated_at = ?1 WHERE status = 'leased' AND lease_expires_at <= ?1",
            [now.to_rfc3339()],
        )?;
        let selected: Option<String> = transaction
            .query_row(
                "SELECT id FROM connector_jobs WHERE status = 'queued' AND available_at <= ?1 ORDER BY created_at, id LIMIT 1",
                [now.to_rfc3339()],
                |row| row.get(0),
            )
            .optional()?;
        let Some(id) = selected else {
            transaction.commit()?;
            return Ok(None);
        };
        transaction.execute(
            "UPDATE connector_jobs SET status = 'leased', lease_owner = ?2, lease_expires_at = ?3, attempts = attempts + 1, updated_at = ?1 WHERE id = ?4 AND status = 'queued'",
            params![now.to_rfc3339(), owner, expires.to_rfc3339(), id],
        )?;
        transaction.commit()?;
        drop(connection);
        let connection = self.lock()?;
        connection
            .query_row(
                "SELECT id, project_id, session_id, connector, job_type, status, payload_json, idempotency_key, attempts, available_at, lease_owner, lease_expires_at, last_error_kind, created_at, updated_at, completed_at FROM connector_jobs WHERE id = ?1",
                [id],
                map_connector_job,
            )
            .optional()
            .context("query leased connector job")
    }

    pub fn complete_connector_job(&self, job_id: &str, owner: &str) -> Result<bool> {
        let now = Utc::now().to_rfc3339();
        let connection = self.lock()?;
        Ok(connection.execute(
            "UPDATE connector_jobs SET status = 'completed', lease_owner = NULL, lease_expires_at = NULL, updated_at = ?3, completed_at = ?3 WHERE id = ?1 AND status = 'leased' AND lease_owner = ?2",
            params![job_id, owner, now],
        )? == 1)
    }

    pub fn retry_connector_job(
        &self,
        job_id: &str,
        owner: &str,
        error_kind: &str,
        retry_after_seconds: i64,
        conflict: bool,
    ) -> Result<bool> {
        let now = Utc::now();
        let status = if conflict { "conflict" } else { "queued" };
        let available = now + chrono::Duration::seconds(retry_after_seconds.clamp(1, 3_600));
        let connection = self.lock()?;
        Ok(connection.execute(
            "UPDATE connector_jobs SET status = ?3, lease_owner = NULL, lease_expires_at = NULL, last_error_kind = ?4, available_at = ?5, updated_at = ?6 WHERE id = ?1 AND status = 'leased' AND lease_owner = ?2",
            params![job_id, owner, status, error_kind, available.to_rfc3339(), now.to_rfc3339()],
        )? == 1)
    }

    pub fn connector_status(&self, project_id: &str) -> Result<ConnectorStatusRecord> {
        let connection = self.lock()?;
        connection.query_row(
            "SELECT SUM(CASE WHEN j.status = 'queued' THEN 1 ELSE 0 END), SUM(CASE WHEN j.status = 'leased' THEN 1 ELSE 0 END), SUM(CASE WHEN j.status = 'completed' THEN 1 ELSE 0 END), SUM(CASE WHEN j.status = 'conflict' AND NOT EXISTS (SELECT 1 FROM connector_jobs resolved WHERE resolved.project_id = j.project_id AND COALESCE(resolved.session_id, '') = COALESCE(j.session_id, '') AND resolved.status = 'completed' AND resolved.updated_at > j.updated_at) THEN 1 ELSE 0 END), MAX(j.updated_at) FROM connector_jobs j WHERE j.project_id = ?1",
            [project_id],
            |row| Ok(ConnectorStatusRecord {
                queued: row.get::<_, Option<u64>>(0)?.unwrap_or(0),
                leased: row.get::<_, Option<u64>>(1)?.unwrap_or(0),
                completed: row.get::<_, Option<u64>>(2)?.unwrap_or(0),
                conflicts: row.get::<_, Option<u64>>(3)?.unwrap_or(0),
                updated_at: row.get(4)?,
            }),
        ).context("query connector status")
    }

    pub fn get_remote_object_map(
        &self,
        connector: &str,
        object_type: &str,
        local_id: &str,
    ) -> Result<Option<RemoteObjectMapRecord>> {
        let connection = self.lock()?;
        connection.query_row(
            "SELECT connector, object_type, local_id, remote_id, remote_parent_id, content_hash, created_at, updated_at FROM remote_object_maps WHERE connector = ?1 AND object_type = ?2 AND local_id = ?3",
            params![connector, object_type, local_id],
            map_remote_object_map,
        ).optional().context("query remote object map")
    }

    pub fn upsert_remote_object_map(&self, record: &RemoteObjectMapRecord) -> Result<()> {
        let connection = self.lock()?;
        connection.execute(
            "INSERT INTO remote_object_maps(connector, object_type, local_id, remote_id, remote_parent_id, content_hash, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8) ON CONFLICT(connector, object_type, local_id) DO UPDATE SET remote_id = excluded.remote_id, remote_parent_id = excluded.remote_parent_id, content_hash = excluded.content_hash, updated_at = excluded.updated_at",
            params![record.connector, record.object_type, record.local_id, record.remote_id, record.remote_parent_id, record.content_hash, record.created_at.to_rfc3339(), record.updated_at.to_rfc3339()],
        )?;
        Ok(())
    }

    pub fn get_remote_object_details(
        &self,
        connector: &str,
        object_type: &str,
        local_id: &str,
    ) -> Result<Option<RemoteObjectDetailsRecord>> {
        let connection = self.lock()?;
        connection
            .query_row(
                "SELECT connector, object_type, local_id, remote_branch_id, node_type, last_synced_version, updated_at FROM remote_object_details WHERE connector = ?1 AND object_type = ?2 AND local_id = ?3",
                params![connector, object_type, local_id],
                map_remote_object_details,
            )
            .optional()
            .context("query remote object details")
    }

    pub fn upsert_remote_object_details(&self, record: &RemoteObjectDetailsRecord) -> Result<()> {
        let connection = self.lock()?;
        connection.execute(
            "INSERT INTO remote_object_details(connector, object_type, local_id, remote_branch_id, node_type, last_synced_version, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7) ON CONFLICT(connector, object_type, local_id) DO UPDATE SET remote_branch_id = excluded.remote_branch_id, node_type = excluded.node_type, last_synced_version = excluded.last_synced_version, updated_at = excluded.updated_at",
            params![record.connector, record.object_type, record.local_id, record.remote_branch_id, record.node_type, record.last_synced_version, record.updated_at.to_rfc3339()],
        )?;
        Ok(())
    }

    pub fn create_device_pairing_code(&self, record: &DevicePairingCodeRecord) -> Result<()> {
        let connection = self.lock()?;
        connection.execute(
            "INSERT INTO device_pairing_codes(code_hash, owner_subject, project_id, session_id, expires_at, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![record.code_hash, record.owner_subject, record.project_id, record.session_id, record.expires_at.to_rfc3339(), record.created_at.to_rfc3339()],
        )?;
        Ok(())
    }

    pub fn exchange_device_pairing_code(
        &self,
        code_hash: &str,
        token_hash: &str,
        device_id: &str,
        credential_expires_at: DateTime<Utc>,
    ) -> Result<Option<DeviceCredentialRecord>> {
        let now = Utc::now();
        let connection = self.lock()?;
        let transaction = connection.unchecked_transaction()?;
        let pairing = transaction
            .query_row(
                "SELECT owner_subject, project_id, session_id, expires_at FROM device_pairing_codes WHERE code_hash = ?1 AND consumed_at IS NULL",
                [code_hash],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?, row.get::<_, String>(3)?)),
            )
            .optional()?;
        let Some((owner_subject, project_id, session_id, expires_at)) = pairing else {
            return Ok(None);
        };
        if parse_time(expires_at)? <= now {
            return Ok(None);
        }
        if transaction.execute(
            "UPDATE device_pairing_codes SET consumed_at = ?2 WHERE code_hash = ?1 AND consumed_at IS NULL",
            params![code_hash, now.to_rfc3339()],
        )? != 1 {
            return Ok(None);
        }
        transaction.execute(
            "INSERT INTO device_credentials(token_hash, owner_subject, project_id, session_id, device_id, expires_at, created_at, last_used_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)",
            params![token_hash, owner_subject, project_id, session_id, device_id, credential_expires_at.to_rfc3339(), now.to_rfc3339()],
        )?;
        transaction.commit()?;
        Ok(Some(DeviceCredentialRecord {
            token_hash: token_hash.to_owned(),
            owner_subject,
            project_id,
            session_id,
            device_id: device_id.to_owned(),
            expires_at: credential_expires_at,
            created_at: now,
            last_used_at: now,
        }))
    }

    pub fn authenticate_device(&self, token_hash: &str) -> Result<Option<DeviceCredentialRecord>> {
        let now = Utc::now();
        let connection = self.lock()?;
        let record = connection
            .query_row(
                "SELECT token_hash, owner_subject, project_id, session_id, device_id, expires_at, created_at, last_used_at FROM device_credentials WHERE token_hash = ?1 AND revoked_at IS NULL AND expires_at > ?2",
                params![token_hash, now.to_rfc3339()],
                map_device_credential,
            )
            .optional()?;
        if record.is_some() {
            connection.execute(
                "UPDATE device_credentials SET last_used_at = ?2 WHERE token_hash = ?1",
                params![token_hash, now.to_rfc3339()],
            )?;
        }
        Ok(record)
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, Connection>> {
        self.connection
            .lock()
            .map_err(|_| anyhow::anyhow!("SQLite mutex poisoned"))
    }
}

#[derive(Debug, Clone)]
pub struct NewSession {
    pub id: String,
    pub title: String,
    pub source_language: String,
    pub target_language: String,
    pub privacy_mode: String,
    pub consent_confirmed: bool,
    pub demo_mode: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionRecord {
    pub id: String,
    pub title: String,
    pub state: SessionState,
    pub source_language: String,
    pub target_language: String,
    pub privacy_mode: String,
    pub consent_confirmed: bool,
    pub demo_mode: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct NewProject {
    pub id: String,
    pub owner_subject: String,
    pub title: String,
    pub source_language: String,
    pub target_language: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectRecord {
    pub id: String,
    pub owner_subject: String,
    pub title: String,
    pub source_language: String,
    pub target_language: String,
    pub version: u64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectUpdateRecord {
    pub cursor: i64,
    pub project_id: String,
    pub session_id: Option<String>,
    pub update_type: String,
    pub payload: Value,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct NewWorkspaceFolder {
    pub id: String,
    pub owner_subject: String,
    pub parent_id: Option<String>,
    pub title: String,
    pub sort_order: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceFolderRecord {
    pub id: String,
    pub owner_subject: String,
    pub parent_id: Option<String>,
    pub title: String,
    pub sort_order: i64,
    pub version: u64,
    pub archived_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceProjectPlacementRecord {
    pub project_id: String,
    pub folder_id: Option<String>,
    pub sort_order: i64,
    pub archived_at: Option<DateTime<Utc>>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceSessionMetadataRecord {
    pub session_id: String,
    pub pinned: bool,
    pub sort_order: i64,
    pub archived_at: Option<DateTime<Utc>>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct NewWorkspaceTrashItem {
    pub owner_subject: String,
    pub entity_type: String,
    pub entity_id: String,
    pub original_parent_id: Option<String>,
    pub original_project_id: Option<String>,
    pub original_sort_order: i64,
    pub original_pinned: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceTrashItemRecord {
    pub owner_subject: String,
    pub entity_type: String,
    pub entity_id: String,
    pub original_parent_id: Option<String>,
    pub original_project_id: Option<String>,
    pub original_sort_order: i64,
    pub original_pinned: bool,
    pub deleted_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceDevicePreferenceRecord {
    pub owner_subject: String,
    pub device_id: String,
    pub active_project_id: Option<String>,
    pub active_session_id: Option<String>,
    pub language_view: String,
    pub sidebar_collapsed: bool,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceUpdateRecord {
    pub cursor: i64,
    pub owner_subject: String,
    pub update_type: String,
    pub payload: Value,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectAiPolicyRecord {
    pub project_id: String,
    pub cloud_enabled: bool,
    pub allowed_modalities: Vec<String>,
    pub local_translation_model: String,
    pub local_explanation_model: String,
    pub local_summary_model: String,
    pub local_vision_model: String,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecordingLeaseRecord {
    pub project_id: String,
    pub session_id: String,
    pub holder_device_id: String,
    #[serde(skip_serializing)]
    pub lease_token_hash: String,
    pub generation: u64,
    pub heartbeat_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub enum LeaseAcquireOutcome {
    Acquired(RecordingLeaseRecord),
    Conflict(RecordingLeaseRecord),
}

#[derive(Debug, Clone)]
pub struct AudioChunkRecord {
    pub session_id: String,
    pub source_id: String,
    pub sequence: u64,
    pub captured_at_ms: u64,
    pub sample_rate: u32,
    pub channels: u16,
    pub encoding: String,
    pub duration_ms: u32,
    pub object_hash: String,
    pub size_bytes: u64,
    pub acknowledged_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct AudioWindowRecord {
    pub id: String,
    pub session_id: String,
    pub source_id: String,
    pub first_sequence: u64,
    pub last_sequence: u64,
    pub captured_at_ms: u64,
    pub duration_ms: u32,
    pub object_hash: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct AssetRecord {
    pub id: String,
    pub session_id: String,
    pub original_name: String,
    pub media_type: String,
    pub object_hash: String,
    pub size_bytes: u64,
    pub status: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct AssetPageRecord {
    pub id: String,
    pub asset_id: String,
    pub page_number: u32,
    pub title: Option<String>,
    pub text_content: String,
    pub object_hash: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct NewModelJob {
    pub id: String,
    pub session_id: String,
    pub job_type: String,
    pub priority: i64,
    pub input: Value,
    pub input_object_hash: Option<String>,
    pub idempotency_key: String,
}

#[derive(Debug, Clone)]
pub struct NewConnectorJob {
    pub id: String,
    pub project_id: String,
    pub session_id: Option<String>,
    pub connector: String,
    pub job_type: String,
    pub payload: Value,
    pub idempotency_key: String,
    pub delay_seconds: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectorJobRecord {
    pub id: String,
    pub project_id: String,
    pub session_id: Option<String>,
    pub connector: String,
    pub job_type: String,
    pub status: String,
    pub payload: Value,
    pub idempotency_key: String,
    pub attempts: u32,
    pub available_at: DateTime<Utc>,
    pub lease_owner: Option<String>,
    pub lease_expires_at: Option<DateTime<Utc>>,
    pub last_error_kind: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectorStatusRecord {
    pub queued: u64,
    pub leased: u64,
    pub completed: u64,
    pub conflicts: u64,
    pub updated_at: Option<String>,
}

#[derive(Debug, Clone)]
pub struct RemoteObjectMapRecord {
    pub connector: String,
    pub object_type: String,
    pub local_id: String,
    pub remote_id: String,
    pub remote_parent_id: Option<String>,
    pub content_hash: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct RemoteObjectDetailsRecord {
    pub connector: String,
    pub object_type: String,
    pub local_id: String,
    pub remote_branch_id: Option<String>,
    pub node_type: Option<String>,
    pub last_synced_version: Option<i64>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct DevicePairingCodeRecord {
    pub code_hash: String,
    pub owner_subject: String,
    pub project_id: String,
    pub session_id: String,
    pub expires_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct DeviceCredentialRecord {
    pub token_hash: String,
    pub owner_subject: String,
    pub project_id: String,
    pub session_id: String,
    pub device_id: String,
    pub expires_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub last_used_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelJobRecord {
    pub id: String,
    pub session_id: String,
    pub job_type: String,
    pub priority: i64,
    pub status: String,
    pub input: Value,
    pub input_object_hash: Option<String>,
    pub result: Option<Value>,
    pub idempotency_key: String,
    pub attempts: u32,
    pub available_at: DateTime<Utc>,
    pub lease_owner: Option<String>,
    pub lease_expires_at: Option<DateTime<Utc>>,
    pub last_error_kind: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelQueueCounts {
    pub queued: u64,
    pub leased: u64,
    pub completed: u64,
    pub failed: u64,
}

#[derive(Debug, Clone)]
pub struct WorkerHeartbeat {
    pub id: String,
    pub capabilities: Vec<String>,
    pub model_metadata: Value,
    pub active_job_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkerNodeRecord {
    pub id: String,
    pub status: String,
    pub capabilities: Vec<String>,
    pub model_metadata: Value,
    pub active_job_id: Option<String>,
    pub last_seen_at: DateTime<Utc>,
}

fn collect_session_object_hashes(
    transaction: &rusqlite::Transaction<'_>,
    session_id: &str,
    hashes: &mut Vec<String>,
) -> Result<()> {
    for query in [
        "SELECT object_hash FROM audio_chunks WHERE session_id = ?1",
        "SELECT object_hash FROM audio_windows WHERE session_id = ?1",
        "SELECT object_hash FROM assets WHERE session_id = ?1",
        "SELECT ap.object_hash FROM asset_pages ap JOIN assets a ON a.id = ap.asset_id WHERE a.session_id = ?1 AND ap.object_hash IS NOT NULL",
        "SELECT input_object_hash FROM model_jobs WHERE session_id = ?1 AND input_object_hash IS NOT NULL",
    ] {
        let mut statement = transaction.prepare(query)?;
        let values = statement
            .query_map([session_id], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        hashes.extend(values);
    }
    Ok(())
}

fn state_name(state: SessionState) -> &'static str {
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

fn parse_state(value: String) -> rusqlite::Result<SessionState> {
    match value.as_str() {
        "created" => Ok(SessionState::Created),
        "ready" => Ok(SessionState::Ready),
        "recording" => Ok(SessionState::Recording),
        "degraded" => Ok(SessionState::Degraded),
        "stopping" => Ok(SessionState::Stopping),
        "processing" => Ok(SessionState::Processing),
        "completed" => Ok(SessionState::Completed),
        "failed" => Ok(SessionState::Failed),
        "archived" => Ok(SessionState::Archived),
        _ => Err(rusqlite::Error::InvalidQuery),
    }
}

fn map_session(row: &rusqlite::Row<'_>) -> rusqlite::Result<SessionRecord> {
    let created: String = row.get(8)?;
    let updated: String = row.get(9)?;
    Ok(SessionRecord {
        id: row.get(0)?,
        title: row.get(1)?,
        state: parse_state(row.get(2)?)?,
        source_language: row.get(3)?,
        target_language: row.get(4)?,
        privacy_mode: row.get(5)?,
        consent_confirmed: row.get(6)?,
        demo_mode: row.get(7)?,
        created_at: parse_time(created)?,
        updated_at: parse_time(updated)?,
    })
}

fn parse_time(value: String) -> rusqlite::Result<DateTime<Utc>> {
    if let Ok(time) = DateTime::parse_from_rfc3339(&value) {
        return Ok(time.with_timezone(&Utc));
    }
    NaiveDateTime::parse_from_str(&value, "%Y-%m-%d %H:%M:%S")
        .map(|time| DateTime::<Utc>::from_naive_utc_and_offset(time, Utc))
        .map_err(|_| rusqlite::Error::InvalidQuery)
}

fn map_project(row: &rusqlite::Row<'_>) -> rusqlite::Result<ProjectRecord> {
    Ok(ProjectRecord {
        id: row.get(0)?,
        owner_subject: row.get(1)?,
        title: row.get(2)?,
        source_language: row.get(3)?,
        target_language: row.get(4)?,
        version: row.get(5)?,
        created_at: parse_time(row.get(6)?)?,
        updated_at: parse_time(row.get(7)?)?,
    })
}

fn map_project_update(row: &rusqlite::Row<'_>) -> rusqlite::Result<ProjectUpdateRecord> {
    let payload: String = row.get(4)?;
    Ok(ProjectUpdateRecord {
        cursor: row.get(0)?,
        project_id: row.get(1)?,
        session_id: row.get(2)?,
        update_type: row.get(3)?,
        payload: serde_json::from_str(&payload).map_err(|_| rusqlite::Error::InvalidQuery)?,
        created_at: parse_time(row.get(5)?)?,
    })
}

fn map_workspace_folder(row: &rusqlite::Row<'_>) -> rusqlite::Result<WorkspaceFolderRecord> {
    let archived: Option<String> = row.get(6)?;
    Ok(WorkspaceFolderRecord {
        id: row.get(0)?,
        owner_subject: row.get(1)?,
        parent_id: row.get(2)?,
        title: row.get(3)?,
        sort_order: row.get(4)?,
        version: row.get(5)?,
        archived_at: archived.map(parse_time).transpose()?,
        created_at: parse_time(row.get(7)?)?,
        updated_at: parse_time(row.get(8)?)?,
    })
}

fn map_workspace_project_placement(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<WorkspaceProjectPlacementRecord> {
    let archived: Option<String> = row.get(3)?;
    Ok(WorkspaceProjectPlacementRecord {
        project_id: row.get(0)?,
        folder_id: row.get(1)?,
        sort_order: row.get(2)?,
        archived_at: archived.map(parse_time).transpose()?,
        updated_at: parse_time(row.get(4)?)?,
    })
}

fn map_workspace_session_metadata(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<WorkspaceSessionMetadataRecord> {
    let archived: Option<String> = row.get(3)?;
    Ok(WorkspaceSessionMetadataRecord {
        session_id: row.get(0)?,
        pinned: row.get(1)?,
        sort_order: row.get(2)?,
        archived_at: archived.map(parse_time).transpose()?,
        updated_at: parse_time(row.get(4)?)?,
    })
}

fn map_workspace_trash_item(row: &rusqlite::Row<'_>) -> rusqlite::Result<WorkspaceTrashItemRecord> {
    Ok(WorkspaceTrashItemRecord {
        owner_subject: row.get(0)?,
        entity_type: row.get(1)?,
        entity_id: row.get(2)?,
        original_parent_id: row.get(3)?,
        original_project_id: row.get(4)?,
        original_sort_order: row.get(5)?,
        original_pinned: row.get(6)?,
        deleted_at: parse_time(row.get(7)?)?,
    })
}

fn map_workspace_device_preference(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<WorkspaceDevicePreferenceRecord> {
    Ok(WorkspaceDevicePreferenceRecord {
        owner_subject: row.get(0)?,
        device_id: row.get(1)?,
        active_project_id: row.get(2)?,
        active_session_id: row.get(3)?,
        language_view: row.get(4)?,
        sidebar_collapsed: row.get(5)?,
        updated_at: parse_time(row.get(6)?)?,
    })
}

fn map_workspace_update(row: &rusqlite::Row<'_>) -> rusqlite::Result<WorkspaceUpdateRecord> {
    let payload: String = row.get(3)?;
    Ok(WorkspaceUpdateRecord {
        cursor: row.get(0)?,
        owner_subject: row.get(1)?,
        update_type: row.get(2)?,
        payload: serde_json::from_str(&payload).map_err(|_| rusqlite::Error::InvalidQuery)?,
        created_at: parse_time(row.get(4)?)?,
    })
}

fn map_project_ai_policy(row: &rusqlite::Row<'_>) -> rusqlite::Result<ProjectAiPolicyRecord> {
    let modalities: String = row.get(2)?;
    Ok(ProjectAiPolicyRecord {
        project_id: row.get(0)?,
        cloud_enabled: row.get(1)?,
        allowed_modalities: serde_json::from_str(&modalities)
            .map_err(|_| rusqlite::Error::InvalidQuery)?,
        local_translation_model: row.get(3)?,
        local_explanation_model: row.get(4)?,
        local_summary_model: row.get(5)?,
        local_vision_model: row.get(6)?,
        updated_at: parse_time(row.get(7)?)?,
    })
}

fn map_recording_lease(row: &rusqlite::Row<'_>) -> rusqlite::Result<RecordingLeaseRecord> {
    Ok(RecordingLeaseRecord {
        project_id: row.get(0)?,
        session_id: row.get(1)?,
        holder_device_id: row.get(2)?,
        lease_token_hash: row.get(3)?,
        generation: row.get(4)?,
        heartbeat_at: parse_time(row.get(5)?)?,
        expires_at: parse_time(row.get(6)?)?,
    })
}

fn map_audio_chunk(row: &rusqlite::Row<'_>) -> rusqlite::Result<AudioChunkRecord> {
    Ok(AudioChunkRecord {
        session_id: row.get(0)?,
        source_id: row.get(1)?,
        sequence: row.get(2)?,
        captured_at_ms: row.get(3)?,
        sample_rate: row.get(4)?,
        channels: row.get(5)?,
        encoding: row.get(6)?,
        duration_ms: row.get(7)?,
        object_hash: row.get(8)?,
        size_bytes: row.get(9)?,
        acknowledged_at: parse_time(row.get(10)?)?,
    })
}

fn map_connector_job(row: &rusqlite::Row<'_>) -> rusqlite::Result<ConnectorJobRecord> {
    let payload: String = row.get(6)?;
    let lease_expires: Option<String> = row.get(11)?;
    let completed: Option<String> = row.get(15)?;
    Ok(ConnectorJobRecord {
        id: row.get(0)?,
        project_id: row.get(1)?,
        session_id: row.get(2)?,
        connector: row.get(3)?,
        job_type: row.get(4)?,
        status: row.get(5)?,
        payload: serde_json::from_str(&payload).map_err(|_| rusqlite::Error::InvalidQuery)?,
        idempotency_key: row.get(7)?,
        attempts: row.get(8)?,
        available_at: parse_time(row.get(9)?)?,
        lease_owner: row.get(10)?,
        lease_expires_at: lease_expires.map(parse_time).transpose()?,
        last_error_kind: row.get(12)?,
        created_at: parse_time(row.get(13)?)?,
        updated_at: parse_time(row.get(14)?)?,
        completed_at: completed.map(parse_time).transpose()?,
    })
}

fn map_remote_object_map(row: &rusqlite::Row<'_>) -> rusqlite::Result<RemoteObjectMapRecord> {
    Ok(RemoteObjectMapRecord {
        connector: row.get(0)?,
        object_type: row.get(1)?,
        local_id: row.get(2)?,
        remote_id: row.get(3)?,
        remote_parent_id: row.get(4)?,
        content_hash: row.get(5)?,
        created_at: parse_time(row.get(6)?)?,
        updated_at: parse_time(row.get(7)?)?,
    })
}

fn map_remote_object_details(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<RemoteObjectDetailsRecord> {
    Ok(RemoteObjectDetailsRecord {
        connector: row.get(0)?,
        object_type: row.get(1)?,
        local_id: row.get(2)?,
        remote_branch_id: row.get(3)?,
        node_type: row.get(4)?,
        last_synced_version: row.get(5)?,
        updated_at: parse_time(row.get(6)?)?,
    })
}

fn map_device_credential(row: &rusqlite::Row<'_>) -> rusqlite::Result<DeviceCredentialRecord> {
    Ok(DeviceCredentialRecord {
        token_hash: row.get(0)?,
        owner_subject: row.get(1)?,
        project_id: row.get(2)?,
        session_id: row.get(3)?,
        device_id: row.get(4)?,
        expires_at: parse_time(row.get(5)?)?,
        created_at: parse_time(row.get(6)?)?,
        last_used_at: parse_time(row.get(7)?)?,
    })
}

fn map_event(row: &rusqlite::Row<'_>) -> rusqlite::Result<EventEnvelope> {
    let event_id: String = row.get(0)?;
    let captured: String = row.get(7)?;
    let ingested: String = row.get(8)?;
    let payload: String = row.get(12)?;
    Ok(EventEnvelope {
        event_id: event_id
            .parse()
            .map_err(|_| rusqlite::Error::InvalidQuery)?,
        schema_version: row.get(1)?,
        session_id: row.get(2)?,
        source_id: row.get(3)?,
        sequence: row.get(4)?,
        event_type: row.get(5)?,
        captured_at_monotonic_ns: row.get(6)?,
        captured_at_wall: DateTime::parse_from_rfc3339(&captured)
            .map_err(|_| rusqlite::Error::InvalidQuery)?
            .with_timezone(&Utc),
        ingested_at: DateTime::parse_from_rfc3339(&ingested)
            .map_err(|_| rusqlite::Error::InvalidQuery)?
            .with_timezone(&Utc),
        correlation_id: row.get(9)?,
        causation_id: row.get(10)?,
        content_hash: row.get(11)?,
        payload: serde_json::from_str(&payload).map_err(|_| rusqlite::Error::InvalidQuery)?,
    })
}

fn deferred_model_job_ready(
    transaction: &rusqlite::Transaction<'_>,
    input_json: &str,
) -> Result<bool> {
    let input: Value = serde_json::from_str(input_json)?;
    if input.get("deferred_material").and_then(Value::as_bool) != Some(true) {
        return Ok(true);
    }
    let Some(dependencies) = input.get("depends_on_job_ids").and_then(Value::as_array) else {
        return Ok(false);
    };
    if dependencies.is_empty()
        || input
            .get("segments")
            .and_then(Value::as_array)
            .is_none_or(Vec::is_empty)
    {
        return Ok(false);
    }
    for dependency in dependencies.iter().filter_map(Value::as_str) {
        let status: Option<String> = transaction
            .query_row(
                "SELECT status FROM model_jobs WHERE id = ?1",
                [dependency],
                |row| row.get(0),
            )
            .optional()?;
        if status.as_deref() != Some("completed") {
            return Ok(false);
        }
    }
    Ok(true)
}

fn append_unique_json_string(input: &mut Value, key: &str, value: &str) {
    let Some(object) = input.as_object_mut() else {
        return;
    };
    let values = object
        .entry(key.to_owned())
        .or_insert_with(|| Value::Array(Vec::new()));
    let Some(values) = values.as_array_mut() else {
        return;
    };
    if !values.iter().any(|item| item.as_str() == Some(value)) {
        values.push(Value::String(value.to_owned()));
    }
}

fn map_model_job(row: &rusqlite::Row<'_>) -> rusqlite::Result<ModelJobRecord> {
    let input: String = row.get(5)?;
    let result: Option<String> = row.get(7)?;
    let available: String = row.get(10)?;
    let lease_expires: Option<String> = row.get(12)?;
    let created: String = row.get(14)?;
    let updated: String = row.get(15)?;
    let completed: Option<String> = row.get(16)?;
    Ok(ModelJobRecord {
        id: row.get(0)?,
        session_id: row.get(1)?,
        job_type: row.get(2)?,
        priority: row.get(3)?,
        status: row.get(4)?,
        input: serde_json::from_str(&input).map_err(|_| rusqlite::Error::InvalidQuery)?,
        input_object_hash: row.get(6)?,
        result: result
            .map(|value| serde_json::from_str(&value))
            .transpose()
            .map_err(|_| rusqlite::Error::InvalidQuery)?,
        idempotency_key: row.get(8)?,
        attempts: row.get(9)?,
        available_at: parse_time(available)?,
        lease_owner: row.get(11)?,
        lease_expires_at: lease_expires.map(parse_time).transpose()?,
        last_error_kind: row.get(13)?,
        created_at: parse_time(created)?,
        updated_at: parse_time(updated)?,
        completed_at: completed.map(parse_time).transpose()?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use aialra_event_protocol::EventEnvelope;
    use serde_json::json;

    fn test_session() -> NewSession {
        NewSession {
            id: "session_test".to_owned(),
            title: "Synthetic queue test".to_owned(),
            source_language: "en".to_owned(),
            target_language: "zh-CN".to_owned(),
            privacy_mode: "local_only".to_owned(),
            consent_confirmed: true,
            demo_mode: false,
        }
    }

    fn named_session(id: &str) -> NewSession {
        NewSession {
            id: id.to_owned(),
            ..test_session()
        }
    }

    #[test]
    fn sqlite_legacy_timestamp_is_read_as_utc() {
        let parsed = parse_time("2026-08-28 08:57:06".to_owned()).unwrap();
        assert_eq!(parsed.to_rfc3339(), "2026-08-28T08:57:06+00:00");
    }

    #[test]
    fn reopening_database_preserves_append_only_events() {
        // The same on-disk database is opened twice to simulate a core process restart.
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("events.sqlite");
        let store = EventStore::open(&path).unwrap();
        store.create_session(&test_session()).unwrap();
        let event = EventEnvelope::new(
            "session_test",
            "test_asr",
            1,
            "segment.finalized",
            0,
            "corr_test",
            None,
            json!({"segment_id": "seg_1", "text": "hello"}),
        )
        .unwrap();
        assert!(store.insert_event(&event).unwrap());
        drop(store);

        let reopened = EventStore::open(&path).unwrap();
        assert_eq!(reopened.list_events("session_test").unwrap(), vec![event]);
    }

    #[test]
    fn duplicate_source_sequence_is_idempotent() {
        // A retransmitted source sequence is acknowledged without creating a second timeline event.
        let temp = tempfile::tempdir().unwrap();
        let store = EventStore::open(temp.path().join("events.sqlite")).unwrap();
        store.create_session(&test_session()).unwrap();
        let first = EventEnvelope::new(
            "session_test",
            "android_phone",
            7,
            "audio.chunk.received",
            0,
            "corr_7",
            None,
            json!({"object_hash": "sha256:00"}),
        )
        .unwrap();
        let second = EventEnvelope::new(
            "session_test",
            "android_phone",
            7,
            "audio.chunk.received",
            0,
            "corr_7_retry",
            None,
            json!({"object_hash": "sha256:00"}),
        )
        .unwrap();
        assert!(store.insert_event(&first).unwrap());
        assert!(!store.insert_event(&second).unwrap());
        assert_eq!(store.list_events("session_test").unwrap().len(), 1);
    }

    #[test]
    fn idempotent_event_key_cannot_silently_change_payload() {
        let temp = tempfile::tempdir().unwrap();
        let store = EventStore::open(temp.path().join("events.sqlite")).unwrap();
        store.create_session(&test_session()).unwrap();
        let first = EventEnvelope::new(
            "session_test",
            "gpu_asr",
            1,
            "segment.finalized",
            0,
            "corr_one",
            None,
            json!({"segment_id": "seg_1", "text": "original"}),
        )
        .unwrap();
        store.insert_event(&first).unwrap();
        let mut changed = EventEnvelope::new(
            "session_test",
            "gpu_asr",
            1,
            "segment.finalized",
            0,
            "corr_one",
            None,
            json!({"segment_id": "seg_1", "text": "changed"}),
        )
        .unwrap();
        changed.event_id = first.event_id;
        let error = store.insert_event(&changed).unwrap_err();
        assert!(error.to_string().contains("different content"));

        let mut changed_lineage = first.clone();
        changed_lineage.event_id = first.event_id;
        changed_lineage.correlation_id = "corr_changed".to_owned();
        let lineage_error = store.insert_event(&changed_lineage).unwrap_err();
        assert!(
            lineage_error
                .to_string()
                .contains("different content or lineage")
        );
    }

    #[test]
    fn audio_manifest_and_ingest_fact_commit_as_one_idempotent_unit() {
        let temp = tempfile::tempdir().unwrap();
        let store = EventStore::open(temp.path().join("events.sqlite")).unwrap();
        store.create_session(&test_session()).unwrap();
        let chunk = AudioChunkRecord {
            session_id: "session_test".to_owned(),
            source_id: "browser-mic".to_owned(),
            sequence: 1,
            captured_at_ms: 1_000,
            sample_rate: 16_000,
            channels: 1,
            encoding: "pcm_s16le".to_owned(),
            duration_ms: 1_000,
            object_hash: "sha256:fixture".to_owned(),
            size_bytes: 32_000,
            acknowledged_at: Utc::now(),
        };
        let event = EventEnvelope::new(
            "session_test",
            "browser-mic",
            1,
            "audio.chunk.received",
            0,
            "audio:session_test:browser-mic:1",
            None,
            json!({"object_hash": "sha256:fixture", "durable": true}),
        )
        .unwrap();
        assert!(store.insert_audio_chunk_and_event(&chunk, &event).unwrap());
        assert!(!store.insert_audio_chunk_and_event(&chunk, &event).unwrap());
        assert_eq!(store.list_events("session_test").unwrap().len(), 1);
        assert_eq!(
            store
                .list_unassembled_audio_chunks("session_test", "browser-mic")
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn audio_retransmission_can_advance_the_server_event_cursor() {
        let temp = tempfile::tempdir().unwrap();
        let store = EventStore::open(temp.path().join("events.sqlite")).unwrap();
        store.create_session(&test_session()).unwrap();
        let chunk = AudioChunkRecord {
            session_id: "session_test".to_owned(),
            source_id: "browser-mic".to_owned(),
            sequence: 7,
            captured_at_ms: 7_000,
            sample_rate: 16_000,
            channels: 1,
            encoding: "pcm_s16le".to_owned(),
            duration_ms: 1_000,
            object_hash: "sha256:fixture-7".to_owned(),
            size_bytes: 32_000,
            acknowledged_at: Utc::now(),
        };
        let first = EventEnvelope::new(
            "session_test",
            "browser-mic",
            1,
            "audio.chunk.received",
            0,
            "audio:session_test:browser-mic:7",
            None,
            json!({"object_hash": "sha256:fixture-7", "durable": true}),
        )
        .unwrap();
        assert!(store.insert_audio_chunk_and_event(&chunk, &first).unwrap());

        // Another event on this source advanced the server cursor before the ACK
        // was observed, so the retry has the same commit ID but sequence 3.
        let mut retry = first.clone();
        retry.sequence = 3;
        assert!(!store.insert_audio_chunk_and_event(&chunk, &retry).unwrap());
        assert_eq!(store.list_events("session_test").unwrap().len(), 1);
    }

    #[test]
    fn expired_lease_is_recovered_by_another_worker() {
        let temp = tempfile::tempdir().unwrap();
        let store = EventStore::open(temp.path().join("events.sqlite")).unwrap();
        store.create_session(&test_session()).unwrap();
        store
            .enqueue_model_job(&NewModelJob {
                id: "job_test".to_owned(),
                session_id: "session_test".to_owned(),
                job_type: "asr".to_owned(),
                priority: 100,
                input: json!({"sample_rate": 16_000}),
                input_object_hash: Some("sha256:fixture".to_owned()),
                idempotency_key: "asr:fixture".to_owned(),
            })
            .unwrap();
        let first = store
            .lease_model_job("worker_one", &["asr".to_owned()], 60)
            .unwrap()
            .unwrap();
        assert_eq!(first.lease_owner.as_deref(), Some("worker_one"));
        let first_leased_at: String = store
            .connection
            .lock()
            .unwrap()
            .query_row(
                "SELECT first_leased_at FROM model_job_metrics WHERE job_id = ?1",
                ["job_test"],
                |row| row.get(0),
            )
            .unwrap();
        store
            .connection
            .lock()
            .unwrap()
            .execute(
                "UPDATE model_jobs SET lease_expires_at = ?1 WHERE id = ?2",
                params![
                    (Utc::now() - chrono::Duration::seconds(1)).to_rfc3339(),
                    "job_test"
                ],
            )
            .unwrap();
        let recovered = store
            .lease_model_job("worker_two", &["asr".to_owned()], 60)
            .unwrap()
            .unwrap();
        assert_eq!(recovered.lease_owner.as_deref(), Some("worker_two"));
        assert_eq!(recovered.attempts, 2);
        let preserved_first_lease: String = store
            .connection
            .lock()
            .unwrap()
            .query_row(
                "SELECT first_leased_at FROM model_job_metrics WHERE job_id = ?1",
                ["job_test"],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(preserved_first_lease, first_leased_at);
    }

    #[test]
    fn enqueue_uses_idempotency_key() {
        let temp = tempfile::tempdir().unwrap();
        let store = EventStore::open(temp.path().join("events.sqlite")).unwrap();
        store.create_session(&test_session()).unwrap();
        let new_job = NewModelJob {
            id: "job_first".to_owned(),
            session_id: "session_test".to_owned(),
            job_type: "translate".to_owned(),
            priority: 70,
            input: json!({"text": "synthetic"}),
            input_object_hash: None,
            idempotency_key: "translate:segment_one".to_owned(),
        };
        let first = store.enqueue_model_job(&new_job).unwrap();
        let second = store
            .enqueue_model_job(&NewModelJob {
                id: "job_duplicate".to_owned(),
                ..new_job
            })
            .unwrap();
        assert_eq!(first.id, second.id);
        assert_eq!(store.model_queue_counts(None).unwrap().queued, 1);
    }

    #[test]
    fn confirmed_material_explanation_waits_until_dependencies_are_activated() {
        let temp = tempfile::tempdir().unwrap();
        let store = EventStore::open(temp.path().join("events.sqlite")).unwrap();
        store.create_session(&test_session()).unwrap();
        let parse = store
            .enqueue_model_job(&NewModelJob {
                id: "job-asset-parse".to_owned(),
                session_id: "session_test".to_owned(),
                job_type: "asset_parse".to_owned(),
                priority: 60,
                input: json!({"asset_id": "asset-1"}),
                input_object_hash: None,
                idempotency_key: "asset_parse:asset-1".to_owned(),
            })
            .unwrap();
        let explanation = store
            .enqueue_model_job(&NewModelJob {
                id: "job-material-explain".to_owned(),
                session_id: "session_test".to_owned(),
                job_type: "explain".to_owned(),
                priority: 40,
                input: json!({
                    "deferred_material": true,
                    "depends_on_job_ids": [parse.id],
                    "segments": [],
                    "asset_ids": ["asset-1"]
                }),
                input_object_hash: None,
                idempotency_key: "explain:material:session_test".to_owned(),
            })
            .unwrap();

        assert!(
            store
                .lease_model_job_for(
                    "explain-worker",
                    &["explain".to_owned()],
                    60,
                    Some(&explanation.id)
                )
                .unwrap()
                .is_none()
        );
        let leased_parse = store
            .lease_model_job_for(
                "asset-worker",
                &["asset_parse".to_owned()],
                60,
                Some(&parse.id),
            )
            .unwrap()
            .unwrap();
        store
            .complete_model_job(
                &leased_parse.id,
                "asset-worker",
                &json!({"pages": [{"page_id": "page-1"}]}),
            )
            .unwrap();
        assert!(
            store
                .lease_model_job_for(
                    "explain-worker",
                    &["explain".to_owned()],
                    60,
                    Some(&explanation.id)
                )
                .unwrap()
                .is_none()
        );

        let mut activated_input = explanation.input.clone();
        activated_input["segments"] = json!([{"id": "paragraph-1", "text": "stable"}]);
        activated_input["asset_pages"] = json!([{"id": "page-1", "text": "material"}]);
        assert!(
            store
                .activate_model_job(&explanation.id, &activated_input)
                .unwrap()
        );
        assert_eq!(
            store
                .lease_model_job_for(
                    "explain-worker",
                    &["explain".to_owned()],
                    60,
                    Some(&explanation.id)
                )
                .unwrap()
                .unwrap()
                .id,
            explanation.id
        );
    }

    #[test]
    fn failed_material_dependency_closes_its_waiting_explanation() {
        let temp = tempfile::tempdir().unwrap();
        let store = EventStore::open(temp.path().join("events.sqlite")).unwrap();
        store.create_session(&test_session()).unwrap();
        store
            .enqueue_model_job(&NewModelJob {
                id: "job-asset-parse-failed".to_owned(),
                session_id: "session_test".to_owned(),
                job_type: "asset_parse".to_owned(),
                priority: 60,
                input: json!({"asset_id": "asset-failed"}),
                input_object_hash: None,
                idempotency_key: "asset_parse:failed".to_owned(),
            })
            .unwrap();
        let explanation = store
            .enqueue_model_job(&NewModelJob {
                id: "job-material-explain-failed".to_owned(),
                session_id: "session_test".to_owned(),
                job_type: "explain".to_owned(),
                priority: 40,
                input: json!({
                    "deferred_material": true,
                    "depends_on_job_ids": ["job-asset-parse-failed"],
                    "segments": [],
                    "asset_ids": ["asset-failed"]
                }),
                input_object_hash: None,
                idempotency_key: "explain:material:failed".to_owned(),
            })
            .unwrap();

        let failed = store
            .fail_deferred_explanations_for_dependency(
                "job-asset-parse-failed",
                "material_parse_failed",
            )
            .unwrap();
        assert_eq!(failed.len(), 1);
        assert_eq!(failed[0].id, explanation.id);
        assert_eq!(failed[0].status, "failed");
        assert_eq!(
            failed[0].last_error_kind.as_deref(),
            Some("material_parse_failed")
        );
    }

    #[test]
    fn failed_summary_can_be_requeued_without_a_duplicate_job() {
        let temp = tempfile::tempdir().unwrap();
        let store = EventStore::open(temp.path().join("events.sqlite")).unwrap();
        store.create_session(&test_session()).unwrap();
        let new_job = NewModelJob {
            id: "job_summary".to_owned(),
            session_id: "session_test".to_owned(),
            job_type: "summarize".to_owned(),
            priority: 20,
            input: json!({"segments": [{"id": "para_1", "text": "synthetic"}]}),
            input_object_hash: None,
            idempotency_key: "summarize:session_test:para_1".to_owned(),
        };
        let first = store.enqueue_model_job(&new_job).unwrap();
        store
            .lease_model_job("worker_one", &["summarize".to_owned()], 60)
            .unwrap()
            .unwrap();
        assert_eq!(
            store
                .retry_or_fail_model_job(&first.id, "worker_one", "provider_timeout", false, 1,)
                .unwrap()
                .as_deref(),
            Some("failed")
        );
        let requeued = store
            .requeue_failed_summary_by_key(&new_job.idempotency_key)
            .unwrap()
            .unwrap();
        assert_eq!(requeued.id, first.id);
        assert_eq!(requeued.status, "queued");
        assert_eq!(requeued.attempts, 0);
        assert_eq!(store.model_queue_counts(None).unwrap().queued, 1);
    }

    #[test]
    fn projects_are_scoped_to_their_owner() {
        let temp = tempfile::tempdir().unwrap();
        let store = EventStore::open(temp.path().join("events.sqlite")).unwrap();
        store
            .create_project(&NewProject {
                id: "project_alice".to_owned(),
                owner_subject: "alice".to_owned(),
                title: "Alice course".to_owned(),
                source_language: "en".to_owned(),
                target_language: "zh-CN".to_owned(),
            })
            .unwrap();
        store
            .create_project(&NewProject {
                id: "project_bob".to_owned(),
                owner_subject: "bob".to_owned(),
                title: "Bob course".to_owned(),
                source_language: "en".to_owned(),
                target_language: "zh-CN".to_owned(),
            })
            .unwrap();

        let alice = store.list_projects("alice").unwrap();
        assert_eq!(alice.len(), 1);
        assert_eq!(alice[0].id, "project_alice");
        assert!(store.list_projects("unknown").unwrap().is_empty());
    }

    #[test]
    fn workspace_trash_restores_and_purges_nested_content_in_dependency_order() {
        let temp = tempfile::tempdir().unwrap();
        let store = EventStore::open(temp.path().join("events.sqlite")).unwrap();
        store
            .create_project(&NewProject {
                id: "project_trash".to_owned(),
                owner_subject: "owner".to_owned(),
                title: "Trash course".to_owned(),
                source_language: "en".to_owned(),
                target_language: "zh-CN".to_owned(),
            })
            .unwrap();
        store
            .create_workspace_folder(&NewWorkspaceFolder {
                id: "folder_root".to_owned(),
                owner_subject: "owner".to_owned(),
                parent_id: None,
                title: "Root".to_owned(),
                sort_order: 1,
            })
            .unwrap();
        store
            .create_workspace_folder(&NewWorkspaceFolder {
                id: "folder_child".to_owned(),
                owner_subject: "owner".to_owned(),
                parent_id: Some("folder_root".to_owned()),
                title: "Child".to_owned(),
                sort_order: 2,
            })
            .unwrap();
        store
            .update_workspace_project_placement("project_trash", Some("folder_child"), 3, false)
            .unwrap();
        store
            .create_session(&named_session("session_trash"))
            .unwrap();
        store
            .attach_session_to_project("project_trash", "session_trash", "owner", "browser")
            .unwrap();
        store
            .update_workspace_session_metadata("session_trash", true, 4, false)
            .unwrap();

        store
            .insert_audio_chunk(&AudioChunkRecord {
                session_id: "session_trash".to_owned(),
                source_id: "browser-mic".to_owned(),
                sequence: 1,
                captured_at_ms: 1_000,
                sample_rate: 16_000,
                channels: 1,
                encoding: "pcm_s16le".to_owned(),
                duration_ms: 1_000,
                object_hash: "sha256:shared-trash-object".to_owned(),
                size_bytes: 4,
                acknowledged_at: Utc::now(),
            })
            .unwrap();
        store
            .insert_asset(&AssetRecord {
                id: "asset_trash".to_owned(),
                session_id: "session_trash".to_owned(),
                original_name: "fixture.txt".to_owned(),
                media_type: "text/plain".to_owned(),
                object_hash: "sha256:unique-trash-object".to_owned(),
                size_bytes: 4,
                status: "stored".to_owned(),
                created_at: Utc::now(),
            })
            .unwrap();
        store
            .enqueue_model_job(&NewModelJob {
                id: "job_trash".to_owned(),
                session_id: "session_trash".to_owned(),
                job_type: "asr".to_owned(),
                priority: 10,
                input: json!({"sample_rate": 16_000}),
                input_object_hash: Some("sha256:shared-trash-object".to_owned()),
                idempotency_key: "asr:trash".to_owned(),
            })
            .unwrap();

        let items = vec![
            NewWorkspaceTrashItem {
                owner_subject: "owner".to_owned(),
                entity_type: "folder".to_owned(),
                entity_id: "folder_root".to_owned(),
                original_parent_id: None,
                original_project_id: None,
                original_sort_order: 1,
                original_pinned: false,
            },
            NewWorkspaceTrashItem {
                owner_subject: "owner".to_owned(),
                entity_type: "folder".to_owned(),
                entity_id: "folder_child".to_owned(),
                original_parent_id: Some("folder_root".to_owned()),
                original_project_id: None,
                original_sort_order: 2,
                original_pinned: false,
            },
            NewWorkspaceTrashItem {
                owner_subject: "owner".to_owned(),
                entity_type: "project".to_owned(),
                entity_id: "project_trash".to_owned(),
                original_parent_id: Some("folder_child".to_owned()),
                original_project_id: None,
                original_sort_order: 3,
                original_pinned: false,
            },
            NewWorkspaceTrashItem {
                owner_subject: "owner".to_owned(),
                entity_type: "session".to_owned(),
                entity_id: "session_trash".to_owned(),
                original_parent_id: None,
                original_project_id: Some("project_trash".to_owned()),
                original_sort_order: 4,
                original_pinned: true,
            },
        ];
        store.trash_workspace_items(&items).unwrap();
        let trashed = store.list_workspace_trash("owner").unwrap();
        assert_eq!(trashed.len(), 4);

        store.restore_workspace_items("owner", &trashed).unwrap();
        assert_eq!(store.list_workspace_trash("owner").unwrap().len(), 0);
        assert_eq!(
            store
                .get_workspace_folder("folder_child")
                .unwrap()
                .unwrap()
                .parent_id
                .as_deref(),
            Some("folder_root")
        );

        store.trash_workspace_items(&items).unwrap();
        let hashes = store
            .purge_workspace_items(
                "owner",
                &["folder_root".to_owned(), "folder_child".to_owned()],
                &["project_trash".to_owned()],
                &["session_trash".to_owned()],
            )
            .unwrap();
        assert!(hashes.contains(&"sha256:shared-trash-object".to_owned()));
        assert!(hashes.contains(&"sha256:unique-trash-object".to_owned()));
        assert!(store.get_project("project_trash").unwrap().is_none());
        assert!(store.get_session("session_trash").unwrap().is_none());
        assert!(store.get_workspace_folder("folder_root").unwrap().is_none());
        assert!(
            !store
                .object_hash_is_referenced("sha256:shared-trash-object")
                .unwrap()
        );
        assert!(store.list_workspace_trash("owner").unwrap().is_empty());
    }

    #[test]
    fn one_project_allows_only_one_live_recorder_and_increments_takeover_generation() {
        let temp = tempfile::tempdir().unwrap();
        let store = EventStore::open(temp.path().join("events.sqlite")).unwrap();
        store
            .create_project(&NewProject {
                id: "project_test".to_owned(),
                owner_subject: "owner".to_owned(),
                title: "Course".to_owned(),
                source_language: "en".to_owned(),
                target_language: "zh-CN".to_owned(),
            })
            .unwrap();
        store.create_session(&named_session("session_one")).unwrap();
        store.create_session(&named_session("session_two")).unwrap();
        store
            .attach_session_to_project("project_test", "session_one", "owner", "phone")
            .unwrap();
        store
            .attach_session_to_project("project_test", "session_two", "owner", "laptop")
            .unwrap();
        let first = store
            .acquire_recording_lease("project_test", "session_one", "phone", "hash_one", 45)
            .unwrap();
        let LeaseAcquireOutcome::Acquired(first) = first else {
            panic!("first recorder must acquire")
        };
        assert_eq!(first.generation, 1);
        let second = store
            .acquire_recording_lease("project_test", "session_two", "laptop", "hash_two", 45)
            .unwrap();
        assert!(matches!(second, LeaseAcquireOutcome::Conflict(_)));
        assert!(
            store
                .release_recording_lease("project_test", "session_one", "hash_one")
                .unwrap()
        );
        let takeover = store
            .acquire_recording_lease("project_test", "session_two", "laptop", "hash_two", 45)
            .unwrap();
        let LeaseAcquireOutcome::Acquired(takeover) = takeover else {
            panic!("released lease must allow takeover")
        };
        assert_eq!(
            takeover.generation, 1,
            "a clean release removes the expired generation record"
        );

        store
            .connection
            .lock()
            .unwrap()
            .execute(
                "UPDATE recording_leases SET expires_at = ?1 WHERE project_id = ?2",
                params![
                    (Utc::now() - chrono::Duration::seconds(1)).to_rfc3339(),
                    "project_test"
                ],
            )
            .unwrap();
        let resumed = store
            .renew_recording_lease("project_test", "session_two", "laptop", "hash_two", 45)
            .unwrap()
            .expect("the same recorder may recover after a long outage when no takeover occurred");
        assert_eq!(resumed.generation, 1);
        assert!(matches!(
            store
                .acquire_recording_lease("project_test", "session_one", "phone", "hash_three", 45,)
                .unwrap(),
            LeaseAcquireOutcome::Conflict(_)
        ));
        store
            .connection
            .lock()
            .unwrap()
            .execute(
                "UPDATE recording_leases SET expires_at = ?1 WHERE project_id = ?2",
                params![
                    (Utc::now() - chrono::Duration::seconds(1)).to_rfc3339(),
                    "project_test"
                ],
            )
            .unwrap();
        let expired_takeover = store
            .acquire_recording_lease("project_test", "session_one", "phone", "hash_three", 45)
            .unwrap();
        let LeaseAcquireOutcome::Acquired(expired_takeover) = expired_takeover else {
            panic!("expired lease must allow takeover")
        };
        assert_eq!(expired_takeover.generation, 2);
        assert!(
            store
                .validate_recording_lease("session_two", "hash_two")
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn different_projects_can_hold_live_recording_leases_at_the_same_time() {
        let temp = tempfile::tempdir().unwrap();
        let store = EventStore::open(temp.path().join("events.sqlite")).unwrap();
        for (project_id, session_id) in [
            ("project_alpha", "session_alpha"),
            ("project_beta", "session_beta"),
        ] {
            store
                .create_project(&NewProject {
                    id: project_id.to_owned(),
                    owner_subject: "owner".to_owned(),
                    title: project_id.to_owned(),
                    source_language: "en".to_owned(),
                    target_language: "zh-CN".to_owned(),
                })
                .unwrap();
            store.create_session(&named_session(session_id)).unwrap();
            store
                .attach_session_to_project(project_id, session_id, "owner", "browser")
                .unwrap();
        }

        assert!(matches!(
            store
                .acquire_recording_lease(
                    "project_alpha",
                    "session_alpha",
                    "browser-alpha",
                    "hash-alpha",
                    45,
                )
                .unwrap(),
            LeaseAcquireOutcome::Acquired(_)
        ));
        assert!(matches!(
            store
                .acquire_recording_lease(
                    "project_beta",
                    "session_beta",
                    "browser-beta",
                    "hash-beta",
                    45,
                )
                .unwrap(),
            LeaseAcquireOutcome::Acquired(_)
        ));
    }

    #[test]
    fn atomic_workspace_moves_normalize_destination_order() {
        let temp = tempfile::tempdir().unwrap();
        let store = EventStore::open(temp.path().join("events.sqlite")).unwrap();
        for (id, order) in [("folder_one", 40), ("folder_two", 40), ("folder_three", -7)] {
            store
                .create_workspace_folder(&NewWorkspaceFolder {
                    id: id.to_owned(),
                    owner_subject: "owner".to_owned(),
                    parent_id: None,
                    title: id.to_owned(),
                    sort_order: order,
                })
                .unwrap();
        }
        store
            .move_workspace_folder_atomic(
                "folder_three",
                "owner",
                None,
                &[
                    "folder_two".to_owned(),
                    "folder_three".to_owned(),
                    "folder_one".to_owned(),
                ],
            )
            .unwrap();
        let mut folders = store.list_workspace_folders("owner").unwrap();
        folders.sort_by_key(|folder| folder.sort_order);
        assert_eq!(
            folders
                .into_iter()
                .map(|folder| folder.id)
                .collect::<Vec<_>>(),
            vec!["folder_two", "folder_three", "folder_one"]
        );
    }

    #[test]
    fn audio_assembly_cursor_survives_database_reopen() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("events.sqlite");
        let store = EventStore::open(&path).unwrap();
        store
            .create_session(&named_session("session_audio"))
            .unwrap();
        for sequence in 1..=3 {
            store
                .insert_audio_chunk(&AudioChunkRecord {
                    session_id: "session_audio".to_owned(),
                    source_id: "browser-mic-g1".to_owned(),
                    sequence,
                    captured_at_ms: sequence * 1000,
                    sample_rate: 16_000,
                    channels: 1,
                    encoding: "pcm_s16le".to_owned(),
                    duration_ms: 1000,
                    object_hash: format!("sha256:chunk{sequence}"),
                    size_bytes: 32_000,
                    acknowledged_at: Utc::now(),
                })
                .unwrap();
        }
        store
            .record_audio_window_and_advance(&AudioWindowRecord {
                id: "window_one".to_owned(),
                session_id: "session_audio".to_owned(),
                source_id: "browser-mic-g1".to_owned(),
                first_sequence: 1,
                last_sequence: 2,
                captured_at_ms: 1000,
                duration_ms: 2000,
                object_hash: "sha256:window".to_owned(),
                created_at: Utc::now(),
            })
            .unwrap();
        drop(store);

        let reopened = EventStore::open(path).unwrap();
        assert_eq!(
            reopened
                .audio_assembly_cursor("session_audio", "browser-mic-g1")
                .unwrap(),
            Some(2)
        );
        let pending = reopened
            .list_unassembled_audio_chunks("session_audio", "browser-mic-g1")
            .unwrap();
        assert_eq!(
            pending
                .iter()
                .map(|chunk| chunk.sequence)
                .collect::<Vec<_>>(),
            vec![3]
        );
    }

    #[test]
    fn audio_retransmission_must_match_the_committed_frame() {
        let temp = tempfile::tempdir().unwrap();
        let store = EventStore::open(temp.path().join("events.sqlite")).unwrap();
        store
            .create_session(&named_session("session_audio_retransmit"))
            .unwrap();
        let original = AudioChunkRecord {
            session_id: "session_audio_retransmit".to_owned(),
            source_id: "browser-mic-g1".to_owned(),
            sequence: 1,
            captured_at_ms: 1_000,
            sample_rate: 16_000,
            channels: 1,
            encoding: "pcm_s16le".to_owned(),
            duration_ms: 1_000,
            object_hash: "sha256:original".to_owned(),
            size_bytes: 32_000,
            acknowledged_at: Utc::now(),
        };
        assert!(store.insert_audio_chunk(&original).unwrap());
        assert!(!store.insert_audio_chunk(&original).unwrap());
        let conflicting = AudioChunkRecord {
            object_hash: "sha256:different".to_owned(),
            ..original
        };
        let error = store.insert_audio_chunk(&conflicting).unwrap_err();
        assert!(error.to_string().contains("different content or metadata"));
    }

    #[test]
    fn connector_job_can_be_leased_without_relocking_the_store() {
        let temp = tempfile::tempdir().unwrap();
        let store = EventStore::open(temp.path().join("events.sqlite")).unwrap();
        store
            .create_project(&NewProject {
                id: "project_connector".to_owned(),
                owner_subject: "owner".to_owned(),
                title: "Course".to_owned(),
                source_language: "en".to_owned(),
                target_language: "zh-CN".to_owned(),
            })
            .unwrap();
        store
            .enqueue_connector_job(&NewConnectorJob {
                id: "connector_test".to_owned(),
                project_id: "project_connector".to_owned(),
                session_id: None,
                connector: "readweave".to_owned(),
                job_type: "reconcile".to_owned(),
                payload: json!({}),
                idempotency_key: "readweave:test".to_owned(),
                delay_seconds: 0,
            })
            .unwrap();
        let leased = store.lease_connector_job("projector", 60).unwrap().unwrap();
        assert_eq!(leased.id, "connector_test");
        assert_eq!(leased.lease_owner.as_deref(), Some("projector"));
    }

    #[test]
    fn later_success_resolves_active_connector_conflict_status() {
        let temp = tempfile::tempdir().unwrap();
        let store = EventStore::open(temp.path().join("events.sqlite")).unwrap();
        store
            .create_project(&NewProject {
                id: "project_conflict".to_owned(),
                owner_subject: "owner".to_owned(),
                title: "Course".to_owned(),
                source_language: "en".to_owned(),
                target_language: "zh-CN".to_owned(),
            })
            .unwrap();
        store
            .create_session(&named_session("session_conflict"))
            .unwrap();
        store
            .attach_session_to_project("project_conflict", "session_conflict", "owner", "browser")
            .unwrap();
        let job = |id: &str| NewConnectorJob {
            id: id.to_owned(),
            project_id: "project_conflict".to_owned(),
            session_id: Some("session_conflict".to_owned()),
            connector: "readweave".to_owned(),
            job_type: "reconcile".to_owned(),
            payload: json!({}),
            idempotency_key: format!("readweave:{id}"),
            delay_seconds: 0,
        };
        store.enqueue_connector_job(&job("conflicted")).unwrap();
        let conflicted = store.lease_connector_job("projector", 60).unwrap().unwrap();
        store
            .retry_connector_job(
                &conflicted.id,
                "projector",
                "managed_region_conflict",
                3_600,
                true,
            )
            .unwrap();
        assert_eq!(
            store
                .connector_status("project_conflict")
                .unwrap()
                .conflicts,
            1
        );

        std::thread::sleep(std::time::Duration::from_millis(2));
        store.enqueue_connector_job(&job("resolved")).unwrap();
        let resolved = store.lease_connector_job("projector", 60).unwrap().unwrap();
        assert!(
            store
                .complete_connector_job(&resolved.id, "projector")
                .unwrap()
        );
        assert_eq!(
            store
                .connector_status("project_conflict")
                .unwrap()
                .conflicts,
            0
        );
    }
}
