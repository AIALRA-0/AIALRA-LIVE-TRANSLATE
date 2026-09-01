//! Evidence-bounded explanation jobs are persisted before the GPU agent sees course text.

use crate::app::AppState;
use aialra_event_store::{ModelJobRecord, NewModelJob};
use anyhow::{Context, Result, bail};
use serde_json::json;
use uuid::Uuid;

pub fn enqueue_explanation(
    state: &AppState,
    session_id: &str,
    trigger: &str,
) -> Result<ModelJobRecord> {
    let session = state
        .store
        .get_session(session_id)?
        .context("session not found")?;
    let events = state.store.list_events(session_id)?;
    let has_paragraphs = events
        .iter()
        .any(|event| event.event_type == "paragraph.finalized");
    let mut segments = events
        .iter()
        .rev()
        .filter_map(|event| {
            if event.event_type
                != if has_paragraphs {
                    "paragraph.finalized"
                } else {
                    "segment.finalized"
                }
            {
                return None;
            }
            Some(json!({
                "id": event.payload.get(if has_paragraphs { "paragraph_id" } else { "segment_id" })?.as_str()?,
                "text": event.payload.get("text")?.as_str()?
            }))
        })
        .take(6)
        .collect::<Vec<_>>();
    segments.reverse();
    let mut pages = events
        .iter()
        .rev()
        .filter_map(|event| {
            if event.event_type != "asset.page.extracted" {
                return None;
            }
            Some(json!({
                "id": event.payload.get("page_id")?.as_str()?,
                "title": event.payload.get("title")?.as_str()?,
                "text": event.payload.get("text")?.as_str()?
            }))
        })
        .take(8)
        .collect::<Vec<_>>();
    pages.reverse();
    if segments.is_empty() {
        bail!("at least one stable segment is required before explanation");
    }
    let evidence_key = segments
        .iter()
        .filter_map(|item| item.get("id").and_then(|value| value.as_str()))
        .collect::<Vec<_>>()
        .join(":");
    state.enqueue_job(NewModelJob {
        id: format!("job_{}", Uuid::now_v7().simple()),
        session_id: session_id.to_owned(),
        job_type: "explain".to_owned(),
        priority: 30,
        input: json!({
            "segments": segments,
            "asset_pages": pages,
            "target_language": session.target_language,
            "trigger": trigger
        }),
        input_object_hash: None,
        idempotency_key: format!("explain:{session_id}:{trigger}:{evidence_key}"),
    })
}
