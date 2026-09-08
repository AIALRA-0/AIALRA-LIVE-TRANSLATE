//! Evidence-bounded explanation jobs are persisted before the GPU agent sees course text.

use crate::app::AppState;
use aialra_event_store::{ModelJobRecord, NewModelJob};
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use uuid::Uuid;

pub fn enqueue_explanation(
    state: &AppState,
    session_id: &str,
    trigger: &str,
) -> Result<ModelJobRecord> {
    state
        .store
        .get_session(session_id)?
        .context("session not found")?;
    let (segments, pages) = collect_evidence(state, session_id)?;
    if segments.is_empty() {
        bail!("at least one stable segment is required before explanation");
    }
    enqueue_with_evidence(state, session_id, trigger, segments, pages, false)
}

/// Enqueue one stable, not-yet-explained content group selected by the scheduler.
/// The selector lives in `jobs.rs`, while this function owns the exact evidence
/// payload and idempotency boundary used by both manual and automatic paths.
pub fn enqueue_explanation_for_paragraphs(
    state: &AppState,
    session_id: &str,
    trigger: &str,
    paragraph_ids: &[String],
) -> Result<ModelJobRecord> {
    let session = state
        .store
        .get_session(session_id)?
        .context("session not found")?;
    let events = state.store.list_events(session_id)?;
    let segments = events
        .iter()
        .filter(|event| event.event_type == "paragraph.finalized")
        .filter_map(|event| {
            let id = event.payload.get("paragraph_id")?.as_str()?;
            if !paragraph_ids.iter().any(|candidate| candidate == id) {
                return None;
            }
            Some(json!({"id": id, "text": event.payload.get("text")?.as_str()?}))
        })
        .collect::<Vec<_>>();
    // The scheduler already selects one bounded group. Preserve complete
    // paragraphs: equal per-paragraph truncation can remove the conclusion of
    // a long sentence while leaving unused space for shorter neighbours.
    let (_, pages) = collect_evidence(state, session_id)?;
    if segments.is_empty() {
        bail!("selected content group has no stable segments");
    }
    enqueue_with_evidence(state, session_id, trigger, segments, pages, false).with_context(|| {
        format!(
            "failed to enqueue content group for {}",
            session.target_language
        )
    })
}

fn enqueue_with_evidence(
    state: &AppState,
    session_id: &str,
    trigger: &str,
    segments: Vec<Value>,
    pages: Vec<Value>,
    deferred: bool,
) -> Result<ModelJobRecord> {
    let session = state
        .store
        .get_session(session_id)?
        .context("session not found")?;
    let evidence_key = evidence_key(&segments);
    state.enqueue_job(NewModelJob {
        id: format!("job_{}", Uuid::now_v7().simple()),
        session_id: session_id.to_owned(),
        job_type: "explain".to_owned(),
        priority: 30,
        input: explanation_input(segments, pages, &session.target_language, trigger, deferred),
        input_object_hash: None,
        idempotency_key: format!("explain:{session_id}:{trigger}:{evidence_key}"),
    })
}

/// Persist an explanation job at upload confirmation time. Its queue record is
/// immediately visible, but EventStore holds it until its material and
/// transcript dependencies are complete.
pub fn enqueue_deferred_explanation(
    state: &AppState,
    session_id: &str,
    asset_id: &str,
    parse_job_id: &str,
) -> Result<ModelJobRecord> {
    let session = state
        .store
        .get_session(session_id)?
        .context("session not found")?;
    let job = state.store.enqueue_or_merge_deferred_explanation(
        &NewModelJob {
            id: format!("job_{}", Uuid::now_v7().simple()),
            session_id: session_id.to_owned(),
            job_type: "explain".to_owned(),
            priority: 30,
            input: json!({
                "deferred_material": true,
                "asset_ids": [asset_id],
                "depends_on_job_ids": [parse_job_id],
                "segments": [],
                "asset_pages": [],
                "target_language": session.target_language,
                "trigger": "asset_upload"
            }),
            input_object_hash: None,
            idempotency_key: format!("explain:asset_upload:{session_id}:{asset_id}"),
        },
        asset_id,
        parse_job_id,
    )?;
    let _ = state.emit_idempotent(
        &format!("{}:queued", job.id),
        &job.session_id,
        "model_scheduler",
        "model.job.queued",
        0,
        &job.id,
        None,
        json!({"job_id": job.id, "job_type": job.job_type, "priority": job.priority}),
    );
    Ok(job)
}

/// Refresh and release one pending upload-triggered explanation. It is safe to
/// call after every completed model job because activation is an atomic queued
/// state update.
pub fn activate_deferred_explanation(
    state: &AppState,
    session_id: &str,
) -> Result<Option<ModelJobRecord>> {
    let Some(job) = state.store.find_pending_deferred_explanation(session_id)? else {
        return Ok(None);
    };
    let dependencies_ready = job
        .input
        .get("depends_on_job_ids")
        .and_then(Value::as_array)
        .is_some_and(|dependencies| {
            !dependencies.is_empty()
                && dependencies.iter().filter_map(Value::as_str).all(|id| {
                    state
                        .store
                        .get_model_job(id)
                        .ok()
                        .flatten()
                        .is_some_and(|dependency| dependency.status == "completed")
                })
        });
    if !dependencies_ready {
        return Ok(None);
    }
    let session = state
        .store
        .get_session(session_id)?
        .context("session not found")?;
    let (segments, pages) = collect_evidence(state, session_id)?;
    if segments.is_empty() {
        return Ok(None);
    }
    let input = explanation_input(
        segments,
        pages,
        &session.target_language,
        "asset_upload",
        false,
    );
    if !state.store.activate_model_job(&job.id, &input)? {
        return Ok(None);
    }
    state
        .store
        .get_model_job(&job.id)?
        .context("deferred explanation disappeared after activation")
        .map(Some)
}

const MAX_EXPLANATION_SEGMENTS: usize = 8;
const MAX_EXPLANATION_CHARS: usize = 2_400;

fn collect_evidence(state: &AppState, session_id: &str) -> Result<(Vec<Value>, Vec<Value>)> {
    let events = state.store.list_events(session_id)?;
    let has_paragraphs = events
        .iter()
        .any(|event| event.event_type == "paragraph.finalized");
    let mut segments = events
        .iter()
        .rev()
        .filter_map(|event| {
            let event_type = if has_paragraphs {
                "paragraph.finalized"
            } else {
                "segment.finalized"
            };
            if event.event_type != event_type {
                return None;
            }
            Some(json!({
                "id": event.payload.get(if has_paragraphs { "paragraph_id" } else { "segment_id" })?.as_str()?,
                "text": event.payload.get("text")?.as_str()?
            }))
        })
        .take(MAX_EXPLANATION_SEGMENTS)
        .collect::<Vec<_>>();
    segments.reverse();
    bound_segment_text(&mut segments);
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
        .take(12)
        .collect::<Vec<_>>();
    pages.reverse();
    Ok((segments, pages))
}

fn bound_segment_text(segments: &mut [Value]) {
    let per_segment_budget = MAX_EXPLANATION_CHARS / segments.len().max(1);
    for segment in segments {
        let Some(text) = segment.get_mut("text") else {
            continue;
        };
        let Some(raw) = text.as_str() else {
            continue;
        };
        if raw.chars().count() > per_segment_budget {
            *text = Value::String(raw.chars().take(per_segment_budget).collect());
        }
    }
}

fn explanation_input(
    segments: Vec<Value>,
    pages: Vec<Value>,
    target_language: &str,
    trigger: &str,
    deferred: bool,
) -> Value {
    let content_group_id = format!("group_{}", evidence_key(&segments));
    json!({
        "deferred_material": deferred,
        "segments": segments,
        "asset_pages": pages,
        "target_language": target_language,
        "trigger": trigger,
        "content_group_id": content_group_id
    })
}

fn evidence_key(segments: &[Value]) -> String {
    segments
        .iter()
        .filter_map(|item| item.get("id").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join(":")
}

#[cfg(test)]
mod tests {
    use super::enqueue_deferred_explanation;
    use crate::app::AppState;
    use aialra_event_store::{NewModelJob, NewSession};
    use serde_json::json;

    fn session() -> NewSession {
        NewSession {
            id: "session_material_queue".to_owned(),
            title: "Material queue".to_owned(),
            source_language: "en".to_owned(),
            target_language: "zh-CN".to_owned(),
            privacy_mode: "local_only".to_owned(),
            consent_confirmed: true,
            demo_mode: false,
        }
    }

    fn parse_job(id: &str) -> NewModelJob {
        NewModelJob {
            id: id.to_owned(),
            session_id: "session_material_queue".to_owned(),
            job_type: "asset_parse".to_owned(),
            priority: 10,
            input: json!({"asset_id": id}),
            input_object_hash: None,
            idempotency_key: format!("asset_parse:{id}"),
        }
    }

    #[test]
    fn confirmed_uploads_coalesce_into_one_waiting_explanation() {
        let temp = tempfile::tempdir().unwrap();
        let state = AppState::open(temp.path()).unwrap();
        state.store.create_session(&session()).unwrap();
        state
            .store
            .enqueue_model_job(&parse_job("job-parse-1"))
            .unwrap();
        state
            .store
            .enqueue_model_job(&parse_job("job-parse-2"))
            .unwrap();

        let first = enqueue_deferred_explanation(
            &state,
            "session_material_queue",
            "asset-1",
            "job-parse-1",
        )
        .unwrap();
        let second = enqueue_deferred_explanation(
            &state,
            "session_material_queue",
            "asset-2",
            "job-parse-2",
        )
        .unwrap();

        assert_eq!(first.id, second.id);
        assert_eq!(
            state
                .store
                .model_queue_counts(Some("session_material_queue"))
                .unwrap()
                .queued,
            3
        );
        let input = state.store.get_model_job(&first.id).unwrap().unwrap().input;
        assert_eq!(input["asset_ids"], json!(["asset-1", "asset-2"]));
        assert_eq!(
            input["depends_on_job_ids"],
            json!(["job-parse-1", "job-parse-2"])
        );
    }
}
