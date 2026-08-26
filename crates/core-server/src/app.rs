//! Shared state and safe event emission.

use crate::dingtalk::DingtalkClient;
use crate::worker::WorkerClient;
use aialra_asset_store::ObjectStore;
use aialra_event_protocol::EventEnvelope;
use aialra_event_store::EventStore;
use anyhow::{Context, Result};
use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Serialize;
use serde_json::Value;
use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::AtomicUsize;
use std::sync::{Arc, Mutex};
use tokio::sync::{Notify, Semaphore, broadcast};

#[derive(Clone)]
pub struct AppState {
    pub store: EventStore,
    pub objects: ObjectStore,
    pub worker: WorkerClient,
    pub dingtalk: DingtalkClient,
    pub events: broadcast::Sender<EventEnvelope>,
    pub sequence_lock: Arc<Mutex<()>>,
    pub audio_buffers: Arc<Mutex<HashMap<String, Vec<u8>>>>,
    pub asr_slots: Arc<Semaphore>,
    pub pending_model_jobs: Arc<AtomicUsize>,
    pub model_drained: Arc<Notify>,
}

impl AppState {
    pub fn open(data_dir: &Path, worker_url: &str) -> Result<Self> {
        std::fs::create_dir_all(data_dir).context("create data directory")?;
        let store = EventStore::open(data_dir.join("aialra.sqlite"))?;
        let objects = ObjectStore::new(data_dir.join("objects"))?;
        let (events, _) = broadcast::channel(2_048);
        Ok(Self {
            store,
            objects,
            worker: WorkerClient::new(worker_url),
            dingtalk: DingtalkClient::from_env(),
            events,
            sequence_lock: Arc::new(Mutex::new(())),
            audio_buffers: Arc::new(Mutex::new(HashMap::new())),
            // One ASR task keeps the live path within a predictable GPU budget.
            asr_slots: Arc::new(Semaphore::new(1)),
            pending_model_jobs: Arc::new(AtomicUsize::new(0)),
            model_drained: Arc::new(Notify::new()),
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
        self.store.insert_event(&event)?;
        let _ = self.events.send(event.clone());
        Ok(event)
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
