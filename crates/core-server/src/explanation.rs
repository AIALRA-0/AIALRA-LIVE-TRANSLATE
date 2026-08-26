//! Evidence-linked explanation creation is shared by manual and volume-based triggers.

use crate::app::AppState;
use crate::worker::{EvidencePage, EvidenceSegment, ExplanationRequest};
use aialra_event_protocol::EventEnvelope;
use anyhow::{Context, Result, bail};
use serde_json::json;
use std::collections::HashSet;
use uuid::Uuid;

pub async fn create_explanation(
    state: &AppState,
    session_id: &str,
    trigger: &str,
) -> Result<EventEnvelope> {
    let session = state
        .store
        .get_session(session_id)?
        .context("session not found")?;
    let events = state.store.list_events(session_id)?;
    let mut segments: Vec<EvidenceSegment> = events
        .iter()
        .rev()
        .filter_map(|event| {
            if event.event_type != "segment.finalized" {
                return None;
            }
            Some(EvidenceSegment {
                id: event.payload.get("segment_id")?.as_str()?.to_owned(),
                text: event.payload.get("text")?.as_str()?.to_owned(),
            })
        })
        .take(12)
        .collect();
    segments.reverse();
    let mut pages: Vec<EvidencePage> = events
        .iter()
        .rev()
        .filter_map(|event| {
            if event.event_type != "asset.page.extracted" {
                return None;
            }
            Some(EvidencePage {
                id: event.payload.get("page_id")?.as_str()?.to_owned(),
                title: event.payload.get("title")?.as_str()?.to_owned(),
                text: event.payload.get("text")?.as_str()?.to_owned(),
            })
        })
        .take(8)
        .collect();
    pages.reverse();
    if segments.is_empty() {
        bail!("at least one stable segment is required before explanation");
    }
    let valid_segments: HashSet<String> = segments.iter().map(|item| item.id.clone()).collect();
    let valid_pages: HashSet<String> = pages.iter().map(|item| item.id.clone()).collect();
    let result = state
        .worker
        .explain(&ExplanationRequest {
            segments,
            asset_pages: pages,
            target_language: session.target_language,
        })
        .await?;
    if result
        .evidence_segment_ids
        .iter()
        .any(|id| !valid_segments.contains(id))
        || result
            .asset_page_ids
            .iter()
            .any(|id| !valid_pages.contains(id))
    {
        bail!("explanation provider returned an invalid evidence reference");
    }
    let card_id = format!("card_{}", Uuid::now_v7().simple());
    let payload = serde_json::to_value(&result)?;
    state.emit(
        session_id,
        "local_explainer",
        "explanation.card.created",
        0,
        &format!("explain_{}", Uuid::now_v7().simple()),
        None,
        json!({
            "card_id": card_id,
            "fact_type": "background_explanation",
            "trigger": trigger,
            "result": payload
        }),
    )
}
