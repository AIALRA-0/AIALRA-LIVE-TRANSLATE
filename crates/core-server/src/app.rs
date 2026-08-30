//! Shared state and safe event emission.

use crate::dingtalk::DingtalkClient;
use aialra_asset_store::ObjectStore;
use aialra_event_protocol::EventEnvelope;
use aialra_event_store::{
    EventStore, ModelJobRecord, NewModelJob, ProjectUpdateRecord, WorkspaceUpdateRecord,
};
use anyhow::{Context, Result};
use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Serialize;
use serde_json::Value;
use std::path::Path;
use std::sync::{Arc, Mutex};
use tokio::sync::broadcast;
use uuid::Uuid;

const REPLAYABLE_EVENT_BUFFER: usize = 512;
const AUDIO_WAKE_BUFFER: usize = 2_048;

#[derive(Clone)]
pub struct AppState {
    pub store: EventStore,
    pub objects: ObjectStore,
    pub dingtalk: DingtalkClient,
    pub events: broadcast::Sender<EventEnvelope>,
    pub sequence_lock: Arc<Mutex<()>>,
    pub project_updates: broadcast::Sender<ProjectUpdateRecord>,
    pub workspace_updates: broadcast::Sender<WorkspaceUpdateRecord>,
    pub audio_pending: broadcast::Sender<(String, String)>,
}

impl AppState {
    pub fn open(data_dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(data_dir).context("create data directory")?;
        let store = EventStore::open(data_dir.join("aialra.sqlite"))?;
        let objects = ObjectStore::new(data_dir.join("objects"))?;
        // Events and project updates are durable in SQLite, so lagging clients reconnect with
        // their cursor instead of forcing the process to retain a long lecture in memory.
        let (events, _) = broadcast::channel(REPLAYABLE_EVENT_BUFFER);
        let (project_updates, _) = broadcast::channel(REPLAYABLE_EVENT_BUFFER);
        let (workspace_updates, _) = broadcast::channel(REPLAYABLE_EVENT_BUFFER);
        let (audio_pending, _) = broadcast::channel(AUDIO_WAKE_BUFFER);
        Ok(Self {
            store,
            objects,
            dingtalk: DingtalkClient::from_env(),
            events,
            sequence_lock: Arc::new(Mutex::new(())),
            project_updates,
            workspace_updates,
            audio_pending,
        })
    }

    /// The lock and persisted maximum prevent duplicate source sequences during concurrent emissions.
    pub fn next_sequence(&self, session_id: &str, source_id: &str) -> Result<u64> {
        let _guard = self
            .sequence_lock
            .lock()
            .map_err(|_| anyhow::anyhow!("sequence lock poisoned"))?;
        self.store.next_sequence(session_id, source_id)
    }

    /// Persisting before broadcasting prevents the live UI from showing an event that replay cannot recover.
    #[allow(clippy::too_many_arguments)]
    pub fn emit(
        &self,
        session_id: &str,
        source_id: &str,
        event_type: &str,
        monotonic_ns: u64,
        correlation_id: &str,
        causation_id: Option<String>,
        payload: Value,
    ) -> Result<EventEnvelope> {
        let sequence = self.next_sequence(session_id, source_id)?;
        let event = EventEnvelope::new(
            session_id,
            source_id,
            sequence,
            event_type,
            monotonic_ns,
            correlation_id,
            causation_id,
            payload,
        )?;
        if self.store.insert_event(&event)? {
            let _ = self.events.send(event.clone());
            self.record_session_event_update(&event)?;
        }
        Ok(event)
    }

    /// A stable UUID turns a retried model completion into the same append-only event.
    #[allow(clippy::too_many_arguments)]
    pub fn emit_idempotent(
        &self,
        stable_key: &str,
        session_id: &str,
        source_id: &str,
        event_type: &str,
        monotonic_ns: u64,
        correlation_id: &str,
        causation_id: Option<String>,
        payload: Value,
    ) -> Result<EventEnvelope> {
        let _guard = self
            .sequence_lock
            .lock()
            .map_err(|_| anyhow::anyhow!("sequence lock poisoned"))?;
        let sequence = self.store.next_sequence(session_id, source_id)?;
        let mut event = EventEnvelope::new(
            session_id,
            source_id,
            sequence,
            event_type,
            monotonic_ns,
            correlation_id,
            causation_id,
            payload,
        )?;
        event.event_id = Uuid::new_v5(&Uuid::NAMESPACE_OID, stable_key.as_bytes());
        if self.store.insert_event(&event)? {
            let _ = self.events.send(event.clone());
            self.record_session_event_update(&event)?;
        }
        Ok(event)
    }

    /// Queue creation and its status event share one stable idempotency key.
    pub fn enqueue_job(&self, job: NewModelJob) -> Result<ModelJobRecord> {
        let record = self.store.enqueue_model_job(&job)?;
        let _ = self.emit_idempotent(
            &format!("{}:queued", record.id),
            &record.session_id,
            "model_scheduler",
            "model.job.queued",
            0,
            &record.id,
            None,
            serde_json::json!({
                "job_id": record.id,
                "job_type": record.job_type,
                "priority": record.priority
            }),
        );
        Ok(record)
    }

    pub fn record_project_update(
        &self,
        project_id: &str,
        session_id: Option<&str>,
        update_type: &str,
        payload: Value,
    ) -> Result<ProjectUpdateRecord> {
        let update =
            self.store
                .insert_project_update(project_id, session_id, update_type, &payload)?;
        let _ = self.project_updates.send(update.clone());
        Ok(update)
    }

    pub fn record_workspace_update(
        &self,
        owner_subject: &str,
        update_type: &str,
        payload: Value,
    ) -> Result<WorkspaceUpdateRecord> {
        let update = self
            .store
            .insert_workspace_update(owner_subject, update_type, &payload)?;
        let _ = self.workspace_updates.send(update.clone());
        Ok(update)
    }

    fn record_session_event_update(&self, event: &EventEnvelope) -> Result<()> {
        if let Some(project) = self.store.project_for_session(&event.session_id)? {
            self.record_project_update(
                &project.id,
                Some(&event.session_id),
                "session.event",
                serde_json::to_value(event)?,
            )?;
        }
        if matches!(
            event.event_type.as_str(),
            "segment.finalized"
                | "translation.finalized"
                | "explanation.card.created"
                | "session.summary.created"
                | "asset.page.extracted"
                | "session.processing"
                | "session.completed"
                | "session.failed"
        ) {
            let immediate = matches!(
                event.event_type.as_str(),
                "session.processing" | "session.completed" | "session.failed"
            );
            if let Err(error) =
                crate::readweave::enqueue_projection(self, &event.session_id, immediate)
            {
                tracing::warn!(error_kind = "readweave_enqueue_failed", error = %error, "ReadWeave projection enqueue failed without blocking the course pipeline");
            }
        }
        Ok(())
    }
}

#[derive(Debug)]
pub struct ApiError {
    status: StatusCode,
    message: String,
}

impl ApiError {
    pub fn bad_request(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message: message.into(),
        }
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            message: message.into(),
        }
    }

    pub fn unavailable(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::SERVICE_UNAVAILABLE,
            message: message.into(),
        }
    }

    pub fn unauthorized(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::UNAUTHORIZED,
            message: message.into(),
        }
    }

    pub fn forbidden(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::FORBIDDEN,
            message: message.into(),
        }
    }

    pub fn conflict(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::CONFLICT,
            message: message.into(),
        }
    }

    pub fn upstream(error: anyhow::Error) -> Self {
        tracing::warn!(error = %error, "upstream provider request failed");
        Self {
            status: StatusCode::BAD_GATEWAY,
            message: "DingTalk request failed; local recording can continue".to_owned(),
        }
    }
}

impl From<anyhow::Error> for ApiError {
    fn from(error: anyhow::Error) -> Self {
        tracing::error!(error = %error, "request failed");
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: "local service operation failed".to_owned(),
        }
    }
}

impl From<axum::extract::multipart::MultipartError> for ApiError {
    fn from(error: axum::extract::multipart::MultipartError) -> Self {
        tracing::warn!(error = %error, "multipart request rejected");
        Self::bad_request("invalid multipart upload")
    }
}

impl From<serde_json::Error> for ApiError {
    fn from(error: serde_json::Error) -> Self {
        tracing::error!(error = %error, "JSON serialization failed");
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: "local JSON operation failed".to_owned(),
        }
    }
}

#[derive(Serialize)]
struct ErrorBody {
    error: String,
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(ErrorBody {
                error: self.message,
            }),
        )
            .into_response()
    }
}
