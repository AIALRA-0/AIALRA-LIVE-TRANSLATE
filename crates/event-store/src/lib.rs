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
        Ok(changed == 1)
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
    let parse_time = |value: String| {
        DateTime::parse_from_rfc3339(&value)
            .map(|time| time.with_timezone(&Utc))
            .map_err(|_| rusqlite::Error::InvalidQuery)
    };
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
}
