//! SQLite WAL event storage and rebuildable local projections.

use aialra_core_domain::SessionState;
use aialra_event_protocol::EventEnvelope;
use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::Path;
use std::sync::{Arc, Mutex};

const INITIAL_MIGRATION: &str = include_str!("../migrations/0001_initial.sql");
const MODEL_JOBS_MIGRATION: &str = include_str!("../migrations/0002_model_jobs.sql");
const PROJECTS_CONNECTORS_MIGRATION: &str =
    include_str!("../migrations/0003_projects_connectors.sql");
const JOB_METRICS_MIGRATION: &str = include_str!("../migrations/0004_job_metrics.sql");

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

    /// `INSERT OR IGNORE` turns retransmitted source events into a successful idempotent acknowledgement.
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
        Ok(changed == 1)
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
                now.to_rfc3339(),
            ],
        )?;
        drop(connection);
        self.get_model_job_by_key(&job.idempotency_key)?
            .context("model job disappeared after enqueue")
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

    /// Expired leases return to the queue before one compatible job is leased atomically.
    pub fn lease_model_job(
        &self,
        worker_id: &str,
        capabilities: &[String],
        lease_seconds: i64,
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
                "SELECT id, job_type FROM model_jobs WHERE status = 'queued' AND available_at <= ?1 ORDER BY priority DESC, created_at, id LIMIT 100",
            )?;
            let candidates = statement
                .query_map([now.to_rfc3339()], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            candidates
                .into_iter()
                .find(|(_, job_type)| capabilities.iter().any(|item| item == job_type))
                .map(|(id, _)| id)
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
        let attempts: Option<u32> = connection
            .query_row(
                "SELECT attempts FROM model_jobs WHERE id = ?1 AND status = 'leased' AND lease_owner = ?2",
                params![job_id, worker_id],
                |row| row.get(0),
            )
            .optional()?;
        let Some(attempts) = attempts else {
            return Ok(None);
        };
        let status = if retryable && attempts < 5 {
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
        created_at: DateTime::parse_from_rfc3339(&created)
            .map_err(|_| rusqlite::Error::InvalidQuery)?
            .with_timezone(&Utc),
        updated_at: DateTime::parse_from_rfc3339(&updated)
            .map_err(|_| rusqlite::Error::InvalidQuery)?
            .with_timezone(&Utc),
    })
}

fn parse_time(value: String) -> rusqlite::Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(&value)
        .map(|time| time.with_timezone(&Utc))
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
