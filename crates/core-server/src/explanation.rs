//! Evidence-bounded explanation jobs are persisted before the GPU agent sees course text.

use crate::app::AppState;
use aialra_event_store::{ModelJobRecord, NewModelJob};
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap, HashSet};
use uuid::Uuid;

const QUALITY_REPAIR_TRIGGER: &str = "quality_contract_v50";
const MAX_QUALITY_REPAIRS_PER_ENSURE: usize = 32;
const MIN_REPAIR_GROUP_PARAGRAPHS: usize = 6;

pub fn requeue_versioned_content_repairs(state: &AppState, session_id: &str) -> Result<usize> {
    state
        .store
        .requeue_failed_explanation_content_for_trigger(session_id, QUALITY_REPAIR_TRIGGER)
}

pub fn enqueue_explanation(
    state: &AppState,
    session_id: &str,
    trigger: &str,
) -> Result<ModelJobRecord> {
    enqueue_explanation_with_materials(state, session_id, trigger, None, &[])
}

/// Enqueue an explanation only when explicitly requested, with a small set of
/// extracted pages ranked against the current transcript or a caller query.
pub fn enqueue_explanation_with_materials(
    state: &AppState,
    session_id: &str,
    trigger: &str,
    query: Option<&str>,
    asset_ids: &[String],
) -> Result<ModelJobRecord> {
    state
        .store
        .get_session(session_id)?
        .context("session not found")?;
    let events = state.store.list_events(session_id)?;
    let segments = collect_segments(&events);
    if segments.is_empty() {
        bail!("at least one stable segment is required before explanation");
    }
    let pages = collect_relevant_pages(&events, &segments, query, asset_ids);
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
    let pages = collect_relevant_pages(&events, &segments, None, &[]);
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

/// Append corrected revisions for legacy cards that plainly violate the
/// learner-facing writing contract. Existing events remain immutable, and the
/// versioned idempotency key makes repeated page visits harmless.
pub fn enqueue_quality_repairs(state: &AppState, session_id: &str) -> Result<usize> {
    let session = state
        .store
        .get_session(session_id)?
        .context("session not found")?;
    let events = state.store.list_events(session_id)?;
    let paragraph_text = events
        .iter()
        .filter(|event| event.event_type == "paragraph.finalized")
        .filter_map(|event| {
            Some((
                event.payload.get("paragraph_id")?.as_str()?.to_owned(),
                event.payload.get("text")?.as_str()?.to_owned(),
            ))
        })
        .collect::<BTreeMap<_, _>>();
    let paragraph_order = events
        .iter()
        .filter(|event| event.event_type == "paragraph.finalized")
        .filter_map(|event| {
            event
                .payload
                .get("paragraph_id")?
                .as_str()
                .map(str::to_owned)
        })
        .collect::<Vec<_>>();
    let paragraph_ids = paragraph_text.keys().cloned().collect::<HashSet<_>>();
    let mut cards = Vec::<(Vec<String>, Value, String)>::new();
    let mut latest_card_by_paragraph = HashMap::<String, String>::new();
    for event in events
        .iter()
        .filter(|event| event.event_type == "explanation.card.created")
    {
        let result = &event.payload["result"];
        let ids = result["evidence_segment_ids"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect::<Vec<_>>();
        if ids.is_empty()
            || !ids.iter().any(|id| paragraph_ids.contains(id))
            || !card_has_visible_content(result)
        {
            continue;
        }
        let card_id = event
            .payload
            .get("card_id")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .unwrap_or_else(|| event.event_id.to_string());
        for id in &ids {
            if paragraph_ids.contains(id) {
                latest_card_by_paragraph.insert(id.clone(), card_id.clone());
            }
        }
        cards.push((ids, result.clone(), card_id));
    }
    let chinese = session
        .target_language
        .to_ascii_lowercase()
        .starts_with("zh");
    let mut queued = 0;
    let mut planned = HashSet::new();
    for (ids, result, card_id) in cards {
        if queued == MAX_QUALITY_REPAIRS_PER_ENSURE {
            break;
        }
        let is_latest_visible = ids.iter().any(|id| {
            paragraph_ids.contains(id)
                && latest_card_by_paragraph
                    .get(id)
                    .is_some_and(|latest| latest == &card_id)
        });
        if !is_latest_visible {
            continue;
        }
        let source_characters = ids
            .iter()
            .filter_map(|id| paragraph_text.get(id))
            .map(|text| text.chars().count())
            .sum();
        if has_current_teaching_quality(&result, source_characters, chinese) {
            continue;
        }
        let repair_ids = expanded_repair_evidence(&ids, &paragraph_order);
        let repair_key = repair_ids.join(":");
        if !planned.insert(repair_key.clone()) {
            continue;
        }
        let repair_segments = repair_ids
            .iter()
            .filter_map(|id| Some(json!({"id": id, "text": paragraph_text.get(id)?})))
            .collect::<Vec<_>>();
        let repair_pages = collect_relevant_pages(&events, &repair_segments, None, &[]);
        let repair_key = explanation_evidence_key(&repair_segments, &repair_pages);
        let idempotency_key = format!("explain:{session_id}:{QUALITY_REPAIR_TRIGGER}:{repair_key}");
        if state
            .store
            .get_model_job_by_key(&idempotency_key)?
            .is_some()
        {
            continue;
        }
        enqueue_explanation_for_paragraphs(state, session_id, QUALITY_REPAIR_TRIGGER, &repair_ids)?;
        queued += 1;
    }
    Ok(queued)
}

/// Match the course document's visibility rule: cards without any renderable
/// teaching section never appear to learners and must not trigger repairs.
fn card_has_visible_content(result: &Value) -> bool {
    let summary = result["paragraph_summary"]
        .as_str()
        .or_else(|| result["summary"].as_str())
        .is_some_and(|text| !text.trim().is_empty());
    let sections = &result["teaching_sections"];
    let text_sections = ["chapter_bridge", "main_content", "content_explanation"]
        .iter()
        .any(|field| {
            sections[*field]
                .as_str()
                .is_some_and(|text| !text.trim().is_empty())
        });
    let terms = sections["professional_terms"]
        .as_array()
        .or_else(|| result["terms"].as_array())
        .or_else(|| result["rare_terms"].as_array())
        .is_some_and(|items| {
            items.iter().any(|item| {
                item["term"]
                    .as_str()
                    .is_some_and(|text| !text.trim().is_empty())
                    || item["explanation"]
                        .as_str()
                        .is_some_and(|text| !text.trim().is_empty())
                    || item["one_line"]
                        .as_str()
                        .is_some_and(|text| !text.trim().is_empty())
            })
        });
    let misconceptions = match &sections["misconceptions"] {
        Value::String(text) => !text.trim().is_empty(),
        Value::Array(items) => items
            .iter()
            .any(|item| item.as_str().is_some_and(|text| !text.trim().is_empty())),
        _ => false,
    };
    summary || text_sections || terms || misconceptions
}

/// Existing cards are current only when they carry the published structure
/// version and pass the learner-facing quality checks.
fn has_current_teaching_quality(result: &Value, source_characters: usize, chinese: bool) -> bool {
    result["teaching_sections"]["version"] == 1
        && !explanation_needs_quality_repair(result, source_characters, chinese)
}

/// A tiny final group is usually the tail of the preceding explanation rather
/// than a useful teaching unit. Rebuild it with contiguous neighbouring
/// paragraphs while preserving every original paragraph and its order.
fn expanded_repair_evidence(ids: &[String], paragraph_order: &[String]) -> Vec<String> {
    if ids.len() >= 4 || paragraph_order.len() <= ids.len() {
        return ids.to_vec();
    }
    let positions = ids
        .iter()
        .filter_map(|id| paragraph_order.iter().position(|candidate| candidate == id))
        .collect::<Vec<_>>();
    if positions.len() != ids.len() || positions.windows(2).any(|pair| pair[1] != pair[0] + 1) {
        return ids.to_vec();
    }
    let first = positions[0];
    let last = positions[positions.len() - 1] + 1;
    let target = MIN_REPAIR_GROUP_PARAGRAPHS.min(paragraph_order.len());
    let mut start = first.saturating_sub(target.saturating_sub(ids.len()));
    let mut end = last;
    if end - start < target {
        end = (start + target).min(paragraph_order.len());
        start = end.saturating_sub(target);
    }
    paragraph_order[start..end].to_vec()
}

pub(crate) fn explanation_needs_quality_repair(
    result: &Value,
    source_characters: usize,
    chinese: bool,
) -> bool {
    let summary = result["paragraph_summary"]
        .as_str()
        .or_else(|| result["summary"].as_str())
        .unwrap_or_default()
        .trim();
    let transcript_narration = [
        "当我在讲解",
        "你会看到我所说",
        "老师说",
        "讲者提到",
        "本段话讲了",
        "本段内容讲了",
        "让我们来看",
    ];
    if summary.is_empty()
        || transcript_narration
            .iter()
            .any(|phrase| summary.contains(phrase))
        || summary.chars().count() > 1200
        || (source_characters >= 240 && summary.chars().count() < 80)
        || repetition_collapse(summary)
    {
        return true;
    }
    if !chinese {
        return false;
    }
    result["terms"]
        .as_array()
        .into_iter()
        .flatten()
        .any(|term| {
            let definition = term["explanation"].as_str().unwrap_or_default();
            let name = term["term"].as_str().unwrap_or_default();
            definition.chars().count() < 50
                || definition.chars().count() > 240
                || definition.matches('；').count() < 2
                || redundant_bilingual_name(name)
        })
}

fn repetition_collapse(value: &str) -> bool {
    let compact = value
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect::<Vec<_>>();
    let mut sentences = std::collections::HashMap::<String, usize>::new();
    for part in value.split(['。', '！', '？', '!', '?', '；', ';', '\n']) {
        let normalized = part
            .chars()
            .filter(|character| {
                !character.is_whitespace()
                    && !matches!(
                        character,
                        '，' | ','
                            | '：'
                            | ':'
                            | '“'
                            | '”'
                            | '"'
                            | '‘'
                            | '’'
                            | '、'
                            | '（'
                            | '）'
                            | '('
                            | ')'
                            | '['
                            | ']'
                            | '{'
                            | '}'
                    )
            })
            .collect::<String>();
        if normalized.chars().count() >= 12
            && *sentences
                .entry(normalized)
                .and_modify(|count| *count += 1)
                .or_insert(1)
                >= 3
        {
            return true;
        }
    }
    if compact.len() < 80 {
        return false;
    }
    let mut windows = std::collections::HashMap::<String, usize>::new();
    for window in compact.windows(16) {
        let pattern = window.iter().collect::<String>();
        if *windows
            .entry(pattern)
            .and_modify(|count| *count += 1)
            .or_insert(1)
            >= 5
        {
            return true;
        }
    }
    false
}

fn redundant_bilingual_name(value: &str) -> bool {
    let Some(open) = value.find(['(', '（']) else {
        return false;
    };
    let Some(close) = value.rfind([')', '）']) else {
        return false;
    };
    let opening_len = value[open..]
        .chars()
        .next()
        .map(char::len_utf8)
        .unwrap_or_default();
    if close < open + opening_len {
        return false;
    }
    let outer = value[..open].trim();
    let inner = value[open + opening_len..close].trim();
    !outer.is_empty() && outer.eq_ignore_ascii_case(inner)
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
    let evidence_key = explanation_evidence_key(&segments, &pages);
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

/// Keep coverage for the legacy deferred upload queue record format.
#[cfg(test)]
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
    let segments = collect_segments(&events);
    let pages = collect_relevant_pages(&events, &segments, None, &[]);
    Ok((segments, pages))
}

fn collect_segments(events: &[aialra_event_protocol::EventEnvelope]) -> Vec<Value> {
    let has_paragraphs = events
        .iter()
        .any(|event| event.event_type == "paragraph.finalized");
    let segments = events
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
    complete_recent_segments(segments)
}

const MAX_EXPLANATION_PAGES: usize = 4;
const MAX_EXPLANATION_PAGE_CHARS: usize = 8_000;

fn collect_relevant_pages(
    events: &[aialra_event_protocol::EventEnvelope],
    segments: &[Value],
    query: Option<&str>,
    asset_ids: &[String],
) -> Vec<Value> {
    let query = query
        .filter(|value| !value.trim().is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| {
            segments
                .iter()
                .filter_map(|segment| segment["text"].as_str())
                .collect::<Vec<_>>()
                .join(" ")
        });
    let candidates = events
        .iter()
        .filter_map(|event| {
            if event.event_type != "asset.page.extracted" {
                return None;
            }
            let asset_id = event.payload.get("asset_id")?.as_str()?;
            if !asset_ids.is_empty() && !asset_ids.iter().any(|candidate| candidate == asset_id) {
                return None;
            }
            Some(json!({
                "id": event.payload.get("page_id")?.as_str()?,
                "asset_id": asset_id,
                "title": event.payload.get("title")?.as_str()?,
                "text": event.payload.get("text")?.as_str()?
            }))
        })
        .collect::<Vec<_>>();
    select_relevant_pages(candidates, &query, asset_ids)
}

fn select_relevant_pages(
    candidates: Vec<Value>,
    query: &str,
    selected_asset_ids: &[String],
) -> Vec<Value> {
    let query_terms = lexical_terms(query);
    let mut ranked = candidates
        .into_iter()
        .enumerate()
        .filter_map(|(order, page)| {
            let explicitly_selected = page["asset_id"]
                .as_str()
                .is_some_and(|id| selected_asset_ids.iter().any(|selected| selected == id));
            if !selected_asset_ids.is_empty() && !explicitly_selected {
                return None;
            }
            let title = page["title"].as_str().unwrap_or_default();
            let text = page["text"].as_str().unwrap_or_default();
            let title_terms = lexical_terms(title);
            let page_terms = lexical_terms(text);
            let overlap = query_terms.intersection(&page_terms).count();
            let title_overlap = query_terms.intersection(&title_terms).count();
            let score = overlap + title_overlap * 2;
            (score > 0 || (query_terms.is_empty() && explicitly_selected))
                .then_some((score, order, page))
        })
        .collect::<Vec<_>>();
    ranked.sort_by(|left, right| right.0.cmp(&left.0).then_with(|| right.1.cmp(&left.1)));

    let mut selected = Vec::new();
    let mut characters = 0;
    for (_, _, page) in ranked {
        if selected.len() == MAX_EXPLANATION_PAGES {
            break;
        }
        let length = page["text"].as_str().unwrap_or_default().chars().count();
        if length > MAX_EXPLANATION_PAGE_CHARS || characters + length > MAX_EXPLANATION_PAGE_CHARS {
            continue;
        }
        characters += length;
        selected.push(page);
    }
    selected
}

fn lexical_terms(value: &str) -> HashSet<String> {
    let characters = value
        .chars()
        .flat_map(char::to_lowercase)
        .filter(|character| character.is_alphanumeric())
        .collect::<Vec<_>>();
    let mut terms = HashSet::new();
    for character in &characters {
        if character.is_ascii_alphanumeric() {
            terms.insert(character.to_string());
        }
    }
    for pair in characters.windows(2) {
        terms.insert(pair.iter().collect());
    }
    for token in value
        .split(|character: char| !character.is_alphanumeric())
        .map(str::to_lowercase)
        .filter(|token| token.chars().count() >= 3)
    {
        terms.insert(token);
    }
    terms
}

fn complete_recent_segments(newest_first: Vec<Value>) -> Vec<Value> {
    // Recent-material requests have a bounded context, not permission to cut
    // every paragraph at an equal character quota. Keep a contiguous suffix
    // of complete sources, including one oversized latest paragraph intact.
    let mut selected = Vec::new();
    let mut characters = 0;
    for segment in newest_first {
        let length = segment["text"].as_str().unwrap_or_default().chars().count();
        if !selected.is_empty() && characters + length > MAX_EXPLANATION_CHARS {
            break;
        }
        characters += length;
        selected.push(segment);
    }
    selected.reverse();
    selected
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
        "content_group_id": content_group_id,
        "coverage_contract": "all_sources_v1"
    })
}

fn evidence_key(segments: &[Value]) -> String {
    segments
        .iter()
        .filter_map(|item| item.get("id").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join(":")
}

fn explanation_evidence_key(segments: &[Value], pages: &[Value]) -> String {
    let segment_key = evidence_key(segments);
    if pages.is_empty() {
        segment_key
    } else {
        format!("{segment_key}:materials:{}", evidence_key(pages))
    }
}

#[cfg(test)]
mod tests {
    use super::{
        MAX_EXPLANATION_PAGE_CHARS, MAX_EXPLANATION_PAGES, enqueue_deferred_explanation,
        select_relevant_pages,
    };
    use crate::app::AppState;
    use aialra_event_store::{NewModelJob, NewSession};
    use serde_json::{Value, json};

    #[test]
    fn quality_contract_rejects_narration_thin_summaries_and_shallow_definitions() {
        assert!(super::explanation_needs_quality_repair(
            &json!({"paragraph_summary": "当我在讲解故障模拟时，你会看到我所说的建模是什么意思", "terms": []}),
            400,
            true,
        ));
        assert!(super::explanation_needs_quality_repair(
            &json!({"paragraph_summary": "这部分完整解释了一个技术主题的目的、工作过程、适用条件和限制，并保留关键因果关系，足以让第一次接触该主题的读者继续学习", "terms": [{"term": "Alice (Alice)", "explanation": "这是一个在合成材料中出现的人名；它被错误识别成技术概念；它没有可核对的专业定义；因此不应进入知识补充"}]}),
            400,
            true,
        ));
        assert!(super::explanation_needs_quality_repair(
            &json!({"paragraph_summary": "内容太短", "terms": []}),
            400,
            true,
        ));
        assert!(super::explanation_needs_quality_repair(
            &json!({"paragraph_summary": "过长".repeat(601), "terms": []}),
            400,
            true,
        ));
        assert!(super::explanation_needs_quality_repair(
            &json!({"paragraph_summary": "这段内容解释算法怎样优化分区设计并节省成本；".repeat(18), "terms": []}),
            400,
            true,
        ));
        assert!(super::explanation_needs_quality_repair(
            &json!({"paragraph_summary": "这段内容先定义故障模型，再说明模型怎样把复杂电路抽象为可控制和可观察的测试对象，并进一步解释该抽象只覆盖测试目标，不等同于真实器件的全部物理行为", "terms": [{"explanation": "一家芯片公司"}]}),
            400,
            true,
        ));
        assert!(super::explanation_needs_quality_repair(
            &json!({"paragraph_summary": "这段内容先定义故障模型，再说明模型怎样把复杂电路抽象为可控制和可观察的测试对象，并进一步解释该抽象只覆盖测试目标，不等同于真实器件的全部物理行为", "terms": [{"term": "锁存器（Latch）", "explanation": format!("这是定义；{}；这里说明边界", "用于说明技术对象".repeat(40))}]}),
            400,
            true,
        ));
        assert!(!super::explanation_needs_quality_repair(
            &json!({"paragraph_summary": "这段内容先定义故障模型，再说明模型怎样把复杂电路抽象为可控制和可观察的测试对象。抽象后的模型让工程师能围绕明确故障设计测试，但它只覆盖测试目标，不能代表真实器件中的全部物理行为，因此使用时还要保留模型适用范围和实际电路条件。", "terms": [{"explanation": "晶圆代工厂是按客户设计制造芯片的专业制造企业；它负责工艺开发、晶圆生产和质量控制；客户提供电路设计，代工厂用制造流程把设计变为芯片；它与销售自有品牌芯片的厂商不同"}]}),
            400,
            true,
        ));
    }

    #[test]
    fn bilingual_name_check_handles_ascii_and_fullwidth_parentheses() {
        assert!(super::redundant_bilingual_name("Nandie（Nandie）"));
        assert!(super::redundant_bilingual_name("ASCII (ASCII)"));
        assert!(!super::redundant_bilingual_name("概率（P）"));
        assert!(!super::redundant_bilingual_name("台积电（TSMC）"));
    }

    #[test]
    fn tiny_legacy_tail_is_repaired_with_contiguous_context() {
        let order = (0..12).map(|index| format!("p{index}")).collect::<Vec<_>>();
        assert_eq!(
            super::expanded_repair_evidence(&["p10".into(), "p11".into()], &order),
            vec!["p6", "p7", "p8", "p9", "p10", "p11"]
        );
        assert_eq!(
            super::expanded_repair_evidence(&["p0".into(), "p1".into()], &order),
            vec!["p0", "p1", "p2", "p3", "p4", "p5"]
        );
        assert_eq!(
            super::expanded_repair_evidence(
                &["p2".into(), "p3".into(), "p4".into(), "p5".into()],
                &order,
            ),
            vec!["p2", "p3", "p4", "p5"]
        );
    }

    #[test]
    fn v49_repeated_card_queues_an_append_only_v50_repair_once() {
        let temp = tempfile::tempdir().unwrap();
        let state = AppState::open(temp.path()).unwrap();
        state
            .store
            .create_session(&NewSession {
                id: "session-quality-repair".to_owned(),
                title: "Synthetic quality repair".to_owned(),
                source_language: "en".to_owned(),
                target_language: "zh-CN".to_owned(),
                privacy_mode: "local_only".to_owned(),
                consent_confirmed: true,
                demo_mode: false,
            })
            .unwrap();
        for index in 0..4 {
            state.emit_idempotent(
                &format!("quality-paragraph-{index}"),
                "session-quality-repair",
                "fixture",
                "paragraph.finalized",
                index,
                &format!("paragraph-{index}"),
                None,
                json!({"paragraph_id": format!("paragraph-{index}"), "text": "A complete synthetic technical paragraph used only to verify queue behavior."}),
            ).unwrap();
        }
        state.emit_idempotent(
            "legacy-quality-card",
            "session-quality-repair",
            "fixture",
            "explanation.card.created",
            0,
            "legacy-card",
            None,
            json!({"trigger": "quality_contract_v49", "card_id": "legacy-card", "result": {"paragraph_summary": "这段内容解释算法怎样优化分区设计并节省成本；".repeat(18), "terms": [],
                "evidence_segment_ids": ["paragraph-0", "paragraph-1", "paragraph-2", "paragraph-3"]}}),
        ).unwrap();

        assert_eq!(
            super::enqueue_quality_repairs(&state, "session-quality-repair").unwrap(),
            1
        );
        assert_eq!(
            super::enqueue_quality_repairs(&state, "session-quality-repair").unwrap(),
            0
        );
        let repair = state
            .store
            .get_model_job_by_key(
                "explain:session-quality-repair:quality_contract_v50:paragraph-0:paragraph-1:paragraph-2:paragraph-3",
            )
            .unwrap()
            .unwrap();
        assert_eq!(repair.input["trigger"], "quality_contract_v50");
        let jobs = state
            .store
            .model_queue_counts(Some("session-quality-repair"))
            .unwrap();
        assert_eq!(jobs.queued, 1);
        assert_eq!(
            state
                .store
                .list_events("session-quality-repair")
                .unwrap()
                .iter()
                .filter(|event| event.event_type == "explanation.card.created")
                .count(),
            1
        );
    }

    #[test]
    fn v50_repairs_only_latest_visible_legacy_cards_and_skips_current_quality_cards() {
        let temp = tempfile::tempdir().unwrap();
        let state = AppState::open(temp.path()).unwrap();
        state
            .store
            .create_session(&NewSession {
                id: "session-latest-visible-repair".to_owned(),
                title: "Synthetic latest-visible repair".to_owned(),
                source_language: "en".to_owned(),
                target_language: "zh-CN".to_owned(),
                privacy_mode: "local_only".to_owned(),
                consent_confirmed: true,
                demo_mode: false,
            })
            .unwrap();
        for index in 0..8 {
            state
                .emit_idempotent(
                    &format!("latest-visible-paragraph-{index}"),
                    "session-latest-visible-repair",
                    "fixture",
                    "paragraph.finalized",
                    index,
                    &format!("paragraph-{index}"),
                    None,
                    json!({
                        "paragraph_id": format!("paragraph-{index}"),
                        "text": "A complete synthetic technical paragraph used only to verify append-only quality repair behavior."
                    }),
                )
                .unwrap();
        }
        let emit_card =
            |event_id: &str, card_id: &str, sequence, evidence: Vec<String>, mut result: Value| {
                result["evidence_segment_ids"] = json!(evidence);
                state.emit_idempotent(
                    event_id,
                    "session-latest-visible-repair",
                    "fixture",
                    "explanation.card.created",
                    sequence,
                    card_id,
                    None,
                    json!({"trigger": "manual", "card_id": card_id, "result": result}),
                )
            };

        // The later two cards fully supersede the broad historical card. One
        // already satisfies v1; only the latest legacy card should be repaired.
        emit_card(
            "superseded-historical-card",
            "historical-card",
            0,
            (0..8).map(|index| format!("paragraph-{index}")).collect(),
            json!({"paragraph_summary": "旧版历史讲解需要由当前质量结构替代。".repeat(12), "terms": []}),
        )
            .unwrap();
        let current_detail = "故障模型把需要检测的失效方式转成可验证的测试目标。";
        emit_card(
            "latest-current-card",
            "current-card",
            1,
            (0..4).map(|index| format!("paragraph-{index}")).collect(),
            json!({
                "paragraph_summary": format!("{current_detail}随后，测试流程依据该目标选择激励并检查输出，同时保留模型范围不能覆盖全部物理缺陷这一边界。这一限制提醒工程师，测试覆盖率描述的是模型中的故障集合，不代表芯片所有可能的真实失效都已经被验证。"),
                "terms": [],
                "teaching_sections": {
                    "version": 1,
                    "chapter_bridge": "",
                    "main_content": "故障模型确定测试要覆盖的失效目标。",
                    "content_explanation": current_detail,
                    "professional_terms": [],
                    "misconceptions": []
                }
            }),
        )
            .unwrap();
        emit_card(
            "latest-legacy-card",
            "legacy-card",
            2,
            (4..8).map(|index| format!("paragraph-{index}")).collect(),
            json!({"paragraph_summary": "旧版内容组讲解尚未具备课程讲解结构。".repeat(12), "terms": []}),
        )
            .unwrap();
        emit_card(
            "orphan-visible-card",
            "orphan-card",
            3,
            vec!["missing-paragraph".to_owned()],
            json!({"paragraph_summary": "这张旧卡引用的段落正文不存在，因此没有可重建的证据。".repeat(4), "terms": []}),
        )
            .unwrap();

        let queued =
            super::enqueue_quality_repairs(&state, "session-latest-visible-repair").unwrap();
        assert_eq!(queued, 1);
        assert_eq!(
            super::enqueue_quality_repairs(&state, "session-latest-visible-repair").unwrap(),
            0
        );
        let repair = state
            .store
            .get_model_job_by_key(
                "explain:session-latest-visible-repair:quality_contract_v50:paragraph-4:paragraph-5:paragraph-6:paragraph-7",
            )
            .unwrap()
            .unwrap();
        assert_eq!(repair.input["trigger"], "quality_contract_v50");
        assert!(state
            .store
            .get_model_job_by_key(
                "explain:session-latest-visible-repair:quality_contract_v50:paragraph-0:paragraph-1:paragraph-2:paragraph-3",
            )
            .unwrap()
            .is_none());
        assert!(state
            .store
            .get_model_job_by_key(
                "explain:session-latest-visible-repair:quality_contract_v50:paragraph-0:paragraph-1:paragraph-2:paragraph-3:paragraph-4:paragraph-5:paragraph-6:paragraph-7",
            )
            .unwrap()
            .is_none());
        assert_eq!(
            state
                .store
                .model_queue_counts(Some("session-latest-visible-repair"))
                .unwrap()
                .queued,
            1
        );
        assert_eq!(
            state
                .store
                .list_events("session-latest-visible-repair")
                .unwrap()
                .iter()
                .filter(|event| event.event_type == "explanation.card.created")
                .count(),
            4
        );
    }

    #[test]
    fn v46_card_with_current_sections_is_skipped_when_quality_passes() {
        let temp = tempfile::tempdir().unwrap();
        let state = AppState::open(temp.path()).unwrap();
        state
            .store
            .create_session(&NewSession {
                id: "session-compatible-repair".to_owned(),
                title: "Synthetic compatible repair".to_owned(),
                source_language: "en".to_owned(),
                target_language: "zh-CN".to_owned(),
                privacy_mode: "local_only".to_owned(),
                consent_confirmed: true,
                demo_mode: false,
            })
            .unwrap();
        for index in 0..4 {
            state.emit_idempotent(
                &format!("compatible-paragraph-{index}"),
                "session-compatible-repair",
                "fixture",
                "paragraph.finalized",
                index,
                &format!("paragraph-{index}"),
                None,
                json!({"paragraph_id": format!("paragraph-{index}"), "text": "A complete synthetic technical paragraph used only to verify compatibility."}),
            ).unwrap();
        }
        state.emit_idempotent(
            "compatible-quality-card",
            "session-compatible-repair",
            "fixture",
            "explanation.card.created",
            0,
            "compatible-card",
            None,
            json!({"trigger": "quality_contract_v46", "result": {
                "paragraph_summary": "这段合成材料完整说明测试目标怎样决定故障模型，再说明测试向量怎样激励电路并观察输出，最后保留抽象模型不能覆盖全部物理缺陷这一适用边界，内容仅用于验证兼容版本不会被重复排队",
                "terms": [{"term": "故障模型（Fault Model）", "explanation": "故障模型是对电路失效方式的抽象表示；它帮助测试流程选择需要激励和观察的目标；工程师按模型生成并评估测试向量；它适用于描述指定故障范围，不等同于器件中的全部物理缺陷"}],
                "teaching_sections": {"version": 1},
                "evidence_segment_ids": ["paragraph-0", "paragraph-1", "paragraph-2", "paragraph-3"]
            }}),
        ).unwrap();

        assert_eq!(
            super::enqueue_quality_repairs(&state, "session-compatible-repair").unwrap(),
            0
        );
    }

    #[test]
    fn recent_material_context_preserves_complete_contiguous_paragraphs() {
        let recent = json!({"id": "recent", "text": "后".repeat(900)});
        let middle = json!({"id": "middle", "text": "中".repeat(1300)});
        let old = json!({"id": "old", "text": "前".repeat(400)});
        let oldest = json!({"id": "oldest", "text": "short"});
        assert_eq!(
            super::complete_recent_segments(vec![recent.clone(), middle.clone(), old, oldest]),
            vec![middle, recent]
        );
        let oversized = json!({"id": "large", "text": "完整条件不能丢".repeat(600)});
        assert_eq!(
            super::complete_recent_segments(vec![oversized.clone()]),
            vec![oversized]
        );
        assert!(super::complete_recent_segments(vec![]).is_empty());
    }

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

    #[test]
    fn material_selection_ranks_related_pages_and_respects_asset_filter() {
        let pages = vec![
            json!({"id":"page-unrelated","asset_id":"asset-a","title":"地质构造","text":"岩石层的运动形成山脉"}),
            json!({"id":"page-related","asset_id":"asset-b","title":"光合作用","text":"植物利用光能把二氧化碳和水转化为糖"}),
            json!({"id":"page-filtered","asset_id":"asset-c","title":"光合作用","text":"叶绿体吸收光能并参与光合作用"}),
        ];
        let selected = select_relevant_pages(pages, "植物的光合作用", &["asset-b".to_owned()]);

        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0]["id"], "page-related");
    }

    #[test]
    fn material_selection_bounds_page_count_and_preserves_complete_pages() {
        let mut pages: Vec<serde_json::Value> = (0..8)
            .map(|index| {
                json!({
                    "id": format!("page-{index}"),
                    "asset_id": "asset-a",
                    "title": "Rust async worker",
                    "text": format!("Rust async worker scheduling example {index}")
                })
            })
            .collect();
        pages.push(json!({
            "id": "page-oversized",
            "asset_id": "asset-a",
            "title": "Rust async worker",
            "text": "Rust async worker ".repeat(MAX_EXPLANATION_PAGE_CHARS + 1)
        }));
        let selected = select_relevant_pages(pages, "Rust async worker", &[]);

        assert_eq!(selected.len(), MAX_EXPLANATION_PAGES);
        assert!(selected.iter().all(|page| {
            page["text"]
                .as_str()
                .unwrap()
                .starts_with("Rust async worker")
        }));
        assert!(selected.iter().all(|page| {
            page["text"].as_str().unwrap().chars().count() <= MAX_EXPLANATION_PAGE_CHARS
        }));
    }
}
