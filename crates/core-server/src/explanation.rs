//! Evidence-bounded explanation jobs are persisted before the GPU agent sees course text.

use crate::app::AppState;
use aialra_event_store::{ModelJobRecord, NewModelJob};
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashSet};
use uuid::Uuid;

const QUALITY_REPAIR_TRIGGER: &str = "quality_contract_v48";
const COMPATIBLE_QUALITY_TRIGGERS: [&str; 2] = ["quality_contract_v46", "quality_contract_v47"];
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
    let mut latest = BTreeMap::<String, (Vec<String>, Value, String)>::new();
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
        if !ids.is_empty() {
            let trigger = event
                .payload
                .get("trigger")
                .and_then(Value::as_str)
                .unwrap_or("legacy")
                .to_owned();
            latest.insert(ids.join(":"), (ids, result.clone(), trigger));
        }
    }
    let chinese = session
        .target_language
        .to_ascii_lowercase()
        .starts_with("zh");
    let mut queued = 0;
    let mut planned = HashSet::new();
    for (_key, (ids, result, trigger)) in latest {
        if queued == MAX_QUALITY_REPAIRS_PER_ENSURE {
            break;
        }
        let source_characters = ids
            .iter()
            .filter_map(|id| paragraph_text.get(id))
            .map(|text| text.chars().count())
            .sum();
        if (trigger == QUALITY_REPAIR_TRIGGER
            || COMPATIBLE_QUALITY_TRIGGERS.contains(&trigger.as_str()))
            && ids.len() >= 4
            && !explanation_needs_quality_repair(&result, source_characters, chinese)
        {
            continue;
        }
        let repair_ids = expanded_repair_evidence(&ids, &paragraph_order);
        let repair_key = repair_ids.join(":");
        if !planned.insert(repair_key.clone()) {
            continue;
        }
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
    let segments = complete_recent_segments(segments);
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

#[cfg(test)]
mod tests {
    use super::enqueue_deferred_explanation;
    use crate::app::AppState;
    use aialra_event_store::{NewModelJob, NewSession};
    use serde_json::json;

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
    fn legacy_card_version_repair_is_append_only_and_idempotent() {
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
            json!({"trigger": "quality_contract_v45", "result": {"paragraph_summary": "这段合成材料完整说明测试目标怎样决定故障模型，再说明测试向量怎样激励电路并观察输出，最后保留抽象模型不能覆盖全部物理缺陷这一适用边界，内容仅用于验证版本化重生成队列", "terms": [],
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
    fn v46_card_remains_compatible_when_its_terms_fit_the_tighter_contract() {
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
}
