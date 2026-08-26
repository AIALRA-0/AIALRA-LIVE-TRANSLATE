//! Versioned append-only events exchanged by Rust, Python, TypeScript, Kotlin, and DingTalk adapters.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use thiserror::Error;
use uuid::Uuid;

pub const SCHEMA_VERSION: &str = "1.0.0";

/// The stable envelope carries ordering and lineage while payloads evolve by event type.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EventEnvelope {
    pub event_id: Uuid,
    pub schema_version: String,
    pub session_id: String,
    pub source_id: String,
    pub sequence: u64,
    pub event_type: String,
    pub captured_at_monotonic_ns: u64,
    pub captured_at_wall: DateTime<Utc>,
    pub ingested_at: DateTime<Utc>,
    pub correlation_id: String,
    pub causation_id: Option<String>,
    pub content_hash: String,
    pub payload: Value,
}

impl EventEnvelope {
    /// New events use UUIDv7 so identifiers remain sortable without depending on wall-clock order alone.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        session_id: impl Into<String>,
        source_id: impl Into<String>,
        sequence: u64,
        event_type: impl Into<String>,
        captured_at_monotonic_ns: u64,
        correlation_id: impl Into<String>,
        causation_id: Option<String>,
        payload: Value,
    ) -> Result<Self, ProtocolError> {
        let session_id = session_id.into();
        let source_id = source_id.into();
        let event_type = event_type.into();
        let correlation_id = correlation_id.into();
        validate_identifier("session_id", &session_id)?;
        validate_identifier("source_id", &source_id)?;
        validate_identifier("correlation_id", &correlation_id)?;
        validate_event_type(&event_type)?;

        let now = Utc::now();
        let content_hash = hash_payload(&payload)?;
        Ok(Self {
            event_id: Uuid::now_v7(),
            schema_version: SCHEMA_VERSION.to_owned(),
            session_id,
            source_id,
            sequence,
            event_type,
            captured_at_monotonic_ns,
            captured_at_wall: now,
            ingested_at: now,
            correlation_id,
            causation_id,
            content_hash,
            payload,
        })
    }

    /// Consumers isolate unsupported versions instead of interpreting fields with unknown semantics.
    pub fn validate_version(&self) -> Result<(), ProtocolError> {
        if self.schema_version == SCHEMA_VERSION {
            Ok(())
        } else {
            Err(ProtocolError::UnsupportedVersion(
                self.schema_version.clone(),
            ))
        }
    }
}

/// Canonical JSON hashing detects mutated payloads and accidental duplicate retransmissions.
pub fn hash_payload(payload: &Value) -> Result<String, ProtocolError> {
    let bytes = serde_json::to_vec(payload)?;
    let digest = Sha256::digest(bytes);
    Ok(format!("sha256:{digest:x}"))
}

fn validate_identifier(field: &'static str, value: &str) -> Result<(), ProtocolError> {
    if value.is_empty() || value.len() > 128 {
        return Err(ProtocolError::InvalidIdentifier(field));
    }
    Ok(())
}

fn validate_event_type(value: &str) -> Result<(), ProtocolError> {
    let valid = value.len() <= 128
        && value.len() >= 2
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"_.-".contains(&byte)
        });
    valid.then_some(()).ok_or(ProtocolError::InvalidEventType)
}

#[derive(Debug, Error)]
pub enum ProtocolError {
    #[error("invalid {0}")]
    InvalidIdentifier(&'static str),
    #[error("invalid event_type")]
    InvalidEventType,
    #[error("unsupported schema version {0}")]
    UnsupportedVersion(String),
    #[error("payload serialization failed: {0}")]
    Serialization(#[from] serde_json::Error),
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn identical_payloads_have_identical_hashes() {
        // Stable hashing makes duplicate audio and model events idempotent.
        let payload = json!({"segment_id": "seg_1", "text": "hello"});
        assert_eq!(
            hash_payload(&payload).unwrap(),
            hash_payload(&payload).unwrap()
        );
    }

    #[test]
    fn unknown_schema_version_is_rejected() {
        // A valid event can be isolated when its version is newer than this binary.
        let mut event = EventEnvelope::new(
            "session_1",
            "source_1",
            1,
            "segment.finalized",
            0,
            "corr_1",
            None,
            json!({"text": "hello"}),
        )
        .unwrap();
        event.schema_version = "2.0.0".to_owned();
        assert!(matches!(
            event.validate_version(),
            Err(ProtocolError::UnsupportedVersion(_))
        ));
    }
}
