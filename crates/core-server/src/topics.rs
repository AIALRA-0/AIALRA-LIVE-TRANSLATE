//! Background semantic grouping of immutable course paragraphs.

use crate::app::AppState;
use aialra_event_store::{ModelJobRecord, NewModelJob};
use anyhow::{Context, Result, bail};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::HashSet;
use uuid::Uuid;

const WINDOW_PARAGRAPHS: usize = 20;
const WINDOW_BYTES: usize = 4_000;

/// Counts throttle analysis, never decide where a topic ends.
pub fn enqueue_pending(state: &AppState, session_id: &str, force: bool) -> Result<bool> {
    if state
        .store
        .active_model_jobs_excluding(session_id, &["topic"], "")?
        > 0
    {
        return Ok(false);
    }
    let events = state.store.list_events(session_id)?;
    let mut assigned = events
        .iter()
        .filter(|event| event.event_type == "content.group.created")
        .filter_map(|event| event.payload["paragraph_ids"].as_array())
        .flatten()
        .filter_map(Value::as_str)
        .collect::<HashSet<_>>();
    // Existing published cards remain valid during upgrade; do not regenerate
    // their evidence under a new grouping algorithm.
    assigned.extend(
        events
            .iter()
            .filter(|event| event.event_type == "explanation.card.created")
            .filter_map(|event| event.payload["result"]["evidence_segment_ids"].as_array())
            .flatten()
            .filter_map(Value::as_str),
    );
    let mut segments = Vec::new();
    let mut characters = 0;
    let mut capacity = false;
    for event in events
        .iter()
        .filter(|event| event.event_type == "paragraph.finalized")
    {
        let Some(id) = event.payload["paragraph_id"].as_str() else {
            continue;
        };
        if assigned.contains(id) {
            if !segments.is_empty() {
                capacity = true;
                break;
            }
            continue;
        }
        let Some(text) = event.payload["text"].as_str() else {
            continue;
        };
        if segments.len() == WINDOW_PARAGRAPHS
            || (!segments.is_empty() && characters + text.len() > WINDOW_BYTES)
        {
            capacity = true;
            break;
        }
        characters += text.len();
        segments.push(json!({"id": id, "text": text}));
    }
    capacity |= segments.len() == WINDOW_PARAGRAPHS;
    if segments.is_empty() || (!force && !capacity && (segments.len() < 8 || characters < 1600)) {
        return Ok(false);
    }
    let ids = segments
        .iter()
        .filter_map(|s| s["id"].as_str())
        .collect::<Vec<_>>();
    let key = format!("topic:{session_id}:{force}:{capacity}:{}", ids.join(":"));
    if state.store.get_model_job_by_key(&key)?.is_some() {
        return Ok(false);
    }
    if !force && !capacity {
        let checked = events
            .iter()
            .rev()
            .find(|event| event.event_type == "topic.window.checked")
            .and_then(|event| event.payload["paragraph_ids"].as_array())
            .map(|values| {
                values
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<HashSet<_>>()
            })
            .unwrap_or_default();
        if ids.iter().filter(|id| !checked.contains(**id)).count() < 3 {
            return Ok(false);
        }
    }
    state.enqueue_job(NewModelJob {
        id: format!("job_{}", Uuid::now_v7().simple()),
        session_id: session_id.to_owned(),
        job_type: "topic".to_owned(),
        priority: 40,
        input: json!({"segments": segments, "force": force, "capacity": capacity}),
        input_object_hash: None,
        idempotency_key: key,
    })?;
    Ok(true)
}

#[derive(Deserialize)]
struct TopicResult {
    boundaries: Vec<usize>,
    provider: String,
}

pub fn apply_result(state: &AppState, job: &ModelJobRecord, result: &Value) -> Result<()> {
    let result: TopicResult = serde_json::from_value(result.clone())?;
    if !result.provider.starts_with("ollama:") || !result.provider.ends_with("@cuda") {
        bail!("topic provider is not local CUDA");
    }
    let segments = job.input["segments"]
        .as_array()
        .context("topic input is missing")?;
    let ids = segments
        .iter()
        .map(|s| s["id"].as_str().context("topic source is missing"))
        .collect::<Result<Vec<_>>>()?;
    if ids.is_empty() || ids.iter().copied().collect::<HashSet<_>>().len() != ids.len() {
        bail!("topic source identifiers are invalid");
    }
    let force = job.input["force"].as_bool().unwrap_or(false);
    let capacity = job.input["capacity"].as_bool().unwrap_or(false);
    let mut previous = 0;
    for &cut in &result.boundaries {
        if cut < previous + 2 || cut + 2 > ids.len() {
            bail!("topic boundary lacks contiguous supporting paragraphs");
        }
        previous = cut;
    }
    // Validate the entire response before appending any group or work item.
    let events = state.store.list_events(&job.session_id)?;
    let known = events
        .iter()
        .filter(|event| event.event_type == "paragraph.finalized")
        .filter_map(|event| event.payload["paragraph_id"].as_str())
        .collect::<HashSet<_>>();
    if ids.iter().any(|id| !known.contains(id)) {
        bail!("topic source is not in this course");
    }
    let mut cuts = result.boundaries;
    if force || capacity {
        cuts.push(ids.len());
    }
    let mut start = 0;
    let mut proposed = Vec::new();
    for end in cuts {
        let selected = &ids[start..end];
        for existing in events
            .iter()
            .filter(|event| event.event_type == "content.group.created")
        {
            let previous_ids = existing.payload["paragraph_ids"]
                .as_array()
                .map(|items| items.iter().filter_map(Value::as_str).collect::<Vec<_>>())
                .unwrap_or_default();
            if previous_ids.iter().any(|id| selected.contains(id)) && previous_ids != selected {
                bail!("topic result overlaps an already committed group");
            }
        }
        proposed.push((start, end));
        start = end;
    }
    for (start, end) in proposed {
        let group_ids = ids[start..end]
            .iter()
            .map(|id| (*id).to_owned())
            .collect::<Vec<_>>();
        let group_key = format!("{}:{}", job.session_id, group_ids.join(":"));
        let group_id = format!(
            "group_{}",
            Uuid::new_v5(&Uuid::NAMESPACE_OID, group_key.as_bytes()).simple()
        );
        let reason = if end < ids.len() {
            "topic_change"
        } else if capacity {
            "capacity_continuation"
        } else {
            "recording_stopped"
        };
        state.emit_idempotent(
            &format!("topic-group:{group_key}"),
            &job.session_id,
            "topic_scheduler",
            "content.group.created",
            0,
            &group_id,
            None,
            json!({"group_id": group_id, "paragraph_ids": group_ids, "reason": reason}),
        )?;
        crate::explanation::enqueue_explanation_for_paragraphs(
            state,
            &job.session_id,
            "semantic_content_group",
            &group_ids,
        )?;
    }
    state.emit_idempotent(
        &format!("{}:topic-checked", job.id),
        &job.session_id,
        "topic_scheduler",
        "topic.window.checked",
        0,
        &job.id,
        None,
        json!({"paragraph_ids": ids, "force": force, "capacity": capacity,
               "provider": result.provider}),
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use aialra_event_store::NewSession;

    fn setup() -> (tempfile::TempDir, AppState) {
        let temp = tempfile::tempdir().unwrap();
        let state = AppState::open(temp.path()).unwrap();
        state
            .store
            .create_session(&NewSession {
                id: "session_topic_test".into(),
                title: "Synthetic topics".into(),
                source_language: "en".into(),
                target_language: "zh-CN".into(),
                privacy_mode: "local_only".into(),
                consent_confirmed: true,
                demo_mode: false,
            })
            .unwrap();
        for i in 0..12 {
            state.emit_idempotent(&format!("topic-test-{i}"), "session_topic_test", "fixture",
                "paragraph.finalized", i, &format!("para-{i}"), None,
                json!({"paragraph_id": format!("para-{i}"), "text": "Synthetic complete paragraph. ".repeat(7)}))
                .unwrap();
        }
        (temp, state)
    }

    fn lease(state: &AppState, force: bool) -> ModelJobRecord {
        assert!(enqueue_pending(state, "session_topic_test", force).unwrap());
        state
            .store
            .lease_model_job("topic-worker", &["topic".into()], 60)
            .unwrap()
            .unwrap()
    }

    #[test]
    fn semantic_groups_keep_open_tail_and_stop_seals_it_once() {
        let (_temp, state) = setup();
        let job = lease(&state, false);
        let result = json!({"boundaries": [4, 8], "provider": "ollama:synthetic@cuda"});
        apply_result(&state, &job, &result).unwrap();
        apply_result(&state, &job, &result).unwrap();
        state
            .store
            .complete_model_job(&job.id, "topic-worker", &result)
            .unwrap();
        let events = state.store.list_events("session_topic_test").unwrap();
        let groups = events
            .iter()
            .filter(|e| e.event_type == "content.group.created")
            .collect::<Vec<_>>();
        assert_eq!(groups.len(), 2);
        assert_eq!(
            groups[0].payload["paragraph_ids"],
            json!(["para-0", "para-1", "para-2", "para-3"])
        );
        assert_eq!(state.store.model_queue_counts(None).unwrap().queued, 2);
        let tail = lease(&state, true);
        assert_eq!(tail.input["segments"].as_array().unwrap().len(), 4);
        let tail_result = json!({"boundaries": [], "provider": "ollama:synthetic@cuda"});
        apply_result(&state, &tail, &tail_result).unwrap();
        state
            .store
            .complete_model_job(&tail.id, "topic-worker", &tail_result)
            .unwrap();
        assert!(!enqueue_pending(&state, "session_topic_test", true).unwrap());
        let events = state.store.list_events("session_topic_test").unwrap();
        let ids = events
            .iter()
            .filter(|e| e.event_type == "content.group.created")
            .flat_map(|e| e.payload["paragraph_ids"].as_array().unwrap().iter())
            .collect::<Vec<_>>();
        assert_eq!(ids.len(), 12);
        assert_eq!(
            ids.iter()
                .filter_map(|id| id.as_str())
                .collect::<HashSet<_>>()
                .len(),
            12
        );
    }

    #[test]
    fn uninterrupted_topic_never_generates_a_card_just_because_the_window_is_checked() {
        let (_temp, state) = setup();
        let job = lease(&state, false);
        let result = json!({"boundaries": [], "provider": "ollama:synthetic@cuda"});
        apply_result(&state, &job, &result).unwrap();
        state
            .store
            .complete_model_job(&job.id, "topic-worker", &result)
            .unwrap();
        assert_eq!(state.store.model_queue_counts(None).unwrap().queued, 0);
        assert!(!enqueue_pending(&state, "session_topic_test", false).unwrap());
        assert!(enqueue_pending(&state, "session_topic_test", true).unwrap());
    }

    #[test]
    fn invalid_boundary_rejects_the_entire_response_before_creating_groups() {
        let (_temp, state) = setup();
        let job = lease(&state, false);
        for cuts in [
            json!([4, 5]),
            json!([8, 4]),
            json!([4, 99]),
            json!([true]),
            json!([-1]),
        ] {
            assert!(
                apply_result(
                    &state,
                    &job,
                    &json!({"boundaries": cuts, "provider": "ollama:synthetic@cuda"})
                )
                .is_err()
            );
        }
        assert!(
            !state
                .store
                .list_events("session_topic_test")
                .unwrap()
                .iter()
                .any(|e| e.event_type == "content.group.created")
        );
        let mut foreign = job.clone();
        foreign.input["segments"][0]["id"] = json!("para-other-course");
        assert!(
            apply_result(
                &state,
                &foreign,
                &json!({"boundaries": [4], "provider": "ollama:synthetic@cuda"})
            )
            .is_err()
        );
    }
}
