//! Private persistent model-job API used only by authenticated GPU agents.

use crate::app::{ApiError, AppState};
use crate::worker::{
    AsrResponse, AssetParseResponse, ExplanationResponse, SummaryResponse, TranslationResponse,
};
use aialra_core_domain::SessionState;
use aialra_event_store::{AssetPageRecord, NewModelJob, WorkerHeartbeat};
use anyhow::Context;
use axum::Json;
use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::Response;
use chrono::Utc;
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::time::Duration;
use uuid::Uuid;

const LEASE_SECONDS: i64 = 60;
const LONG_POLL_SECONDS: u64 = 20;

#[derive(Debug, Deserialize)]
pub struct HeartbeatRequest {
    worker_id: String,
    capabilities: Vec<String>,
    model_metadata: Value,
    active_job_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct LeaseRequest {
    worker_id: String,
    capabilities: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct WorkerRequest {
    worker_id: String,
}

#[derive(Debug, Deserialize)]
pub struct CompleteRequest {
    worker_id: String,
    result: Value,
    elapsed_ms: u64,
}

#[derive(Debug, Deserialize)]
pub struct FailRequest {
    worker_id: String,
    error_kind: String,
    retryable: bool,
    retry_after_seconds: i64,
}

pub async fn worker_heartbeat(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<HeartbeatRequest>,
) -> Result<Json<Value>, ApiError> {
    authorize(&headers)?;
    validate_worker_id(&request.worker_id)?;
    state.store.heartbeat_worker(&WorkerHeartbeat {
        id: request.worker_id,
        capabilities: allowed_capabilities(request.capabilities),
        model_metadata: request.model_metadata,
        active_job_id: request.active_job_id,
    })?;
    Ok(Json(json!({"accepted": true})))
}

pub async fn lease_job(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<LeaseRequest>,
) -> Result<Response, ApiError> {
    authorize(&headers)?;
    validate_worker_id(&request.worker_id)?;
    let capabilities = allowed_capabilities(request.capabilities);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(LONG_POLL_SECONDS);
    loop {
        if let Some(job) =
            state
                .store
                .lease_model_job(&request.worker_id, &capabilities, LEASE_SECONDS)?
        {
            let _ = state.emit_idempotent(
                &format!("{}:leased:{}", job.id, job.attempts),
                &job.session_id,
                "model_scheduler",
                "model.job.leased",
                0,
                &job.id,
                None,
                json!({"job_id": job.id, "job_type": job.job_type, "attempt": job.attempts}),
            );
            return Ok((StatusCode::OK, Json(json!({"job": job}))).into_response());
        }
        if tokio::time::Instant::now() >= deadline {
            return Ok(StatusCode::NO_CONTENT.into_response());
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

pub async fn renew_job(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(job_id): Path<String>,
    Json(request): Json<WorkerRequest>,
) -> Result<Json<Value>, ApiError> {
    authorize(&headers)?;
    let renewed = state
        .store
        .renew_model_job(&job_id, &request.worker_id, LEASE_SECONDS)?;
    if !renewed {
        return Err(ApiError::conflict(
            "model job lease is no longer owned by this worker",
        ));
    }
    Ok(Json(json!({"renewed": true})))
}

pub async fn job_input(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(job_id): Path<String>,
) -> Result<Response, ApiError> {
    authorize(&headers)?;
    let job = state
        .store
        .get_model_job(&job_id)?
        .ok_or_else(|| ApiError::not_found("model job not found"))?;
    let object_hash = job
        .input_object_hash
        .ok_or_else(|| ApiError::not_found("model job has no binary input"))?;
    let bytes = state.objects.read(&object_hash)?;
    Ok(Response::builder()
        .header(header::CONTENT_TYPE, "application/octet-stream")
        .header(header::CACHE_CONTROL, "no-store")
        .header(
            "x-aialra-content-sha256",
            HeaderValue::from_str(object_hash.trim_start_matches("sha256:"))
                .map_err(anyhow::Error::from)?,
        )
        .body(Body::from(bytes))
        .map_err(anyhow::Error::from)?)
}

pub async fn complete_job(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(job_id): Path<String>,
    Json(request): Json<CompleteRequest>,
) -> Result<Json<Value>, ApiError> {
    authorize(&headers)?;
    let job = state
        .store
        .get_model_job(&job_id)?
        .ok_or_else(|| ApiError::not_found("model job not found"))?;
    if job.status == "completed" {
        return Ok(Json(json!({"accepted": true, "duplicate": true})));
    }
    if job.status != "leased" || job.lease_owner.as_deref() != Some(&request.worker_id) {
        return Err(ApiError::conflict(
            "model job lease is no longer owned by this worker",
        ));
    }
    apply_result(&state, &job, &request.result, request.elapsed_ms)?;
    if !state
        .store
        .complete_model_job(&job_id, &request.worker_id, &request.result)?
    {
        return Err(ApiError::conflict("model job completion lost its lease"));
    }
    let _ = state.emit_idempotent(
        &format!("{job_id}:completed"),
        &job.session_id,
        "model_scheduler",
        "model.job.completed",
        0,
        &job_id,
        None,
        json!({"job_id": job_id, "job_type": job.job_type, "elapsed_ms": request.elapsed_ms}),
    );
    finish_session_if_drained(&state, &job.session_id)?;
    Ok(Json(json!({"accepted": true, "duplicate": false})))
}

pub async fn fail_job(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(job_id): Path<String>,
    Json(request): Json<FailRequest>,
) -> Result<Json<Value>, ApiError> {
    authorize(&headers)?;
    let job = state
        .store
        .get_model_job(&job_id)?
        .ok_or_else(|| ApiError::not_found("model job not found"))?;
    let status = state
        .store
        .retry_or_fail_model_job(
            &job_id,
            &request.worker_id,
            &sanitize_error_kind(&request.error_kind),
            request.retryable,
            request.retry_after_seconds,
        )?
        .ok_or_else(|| ApiError::conflict("model job failure lost its lease"))?;
    let event_type = if status == "queued" {
        "model.job.retry_scheduled"
    } else {
        "model.job.failed"
    };
    let _ = state.emit_idempotent(
        &format!("{job_id}:{event_type}:{}", job.attempts),
        &job.session_id,
        "model_scheduler",
        event_type,
        0,
        &job_id,
        None,
        json!({"job_id": job_id, "job_type": job.job_type, "error_kind": sanitize_error_kind(&request.error_kind)}),
    );
    finish_session_if_drained(&state, &job.session_id)?;
    Ok(Json(json!({"accepted": true, "status": status})))
}

fn apply_result(
    state: &AppState,
    job: &aialra_event_store::ModelJobRecord,
    result: &Value,
    elapsed_ms: u64,
) -> Result<(), ApiError> {
    match job.job_type.as_str() {
        "asr" => apply_asr_result(state, job, result, elapsed_ms),
        "translate" => apply_translation_result(state, job, result, elapsed_ms),
        "explain" => apply_explanation_result(state, job, result, elapsed_ms),
        "summarize" => apply_summary_result(state, job, result, elapsed_ms),
        "asset_parse" => apply_asset_result(state, job, result, elapsed_ms),
        _ => Err(ApiError::bad_request("unsupported model job type")),
    }
}

fn apply_asr_result(
    state: &AppState,
    job: &aialra_event_store::ModelJobRecord,
    result: &Value,
    elapsed_ms: u64,
) -> Result<(), ApiError> {
    let asr: AsrResponse = serde_json::from_value(result.clone())?;
    require_provider(&asr.provider, "faster-whisper:", &["@cpu", "@cuda"])?;
    if asr.text.trim().is_empty() {
        state.emit_idempotent(
            &format!("{}:asr_no_speech", job.id),
            &job.session_id,
            "gpu_asr",
            "asr.window.no_speech",
            0,
            &job.id,
            None,
            json!({"provider": asr.provider, "duration_ms": asr.duration_ms, "elapsed_ms": elapsed_ms}),
        )?;
        return Ok(());
    }
    let session = state
        .store
        .get_session(&job.session_id)?
        .ok_or_else(|| ApiError::not_found("session not found"))?;
    let captured_at_ms = job
        .input
        .get("captured_at_ms")
        .and_then(Value::as_u64)
        .unwrap_or_default();
    let segment_id = format!("seg_{}", job.id.trim_start_matches("job_"));
    let partial = state.emit_idempotent(
        &format!("{}:asr_partial", job.id),
        &job.session_id,
        "gpu_asr",
        "asr.partial.updated",
        captured_at_ms.saturating_mul(1_000_000),
        &job.id,
        None,
        json!({"segment_id": segment_id, "text": asr.text, "provider": asr.provider, "elapsed_ms": elapsed_ms}),
    )?;
    let finalized = state.emit_idempotent(
        &format!("{}:segment_final", job.id),
        &job.session_id,
        "gpu_asr",
        "segment.finalized",
        captured_at_ms.saturating_mul(1_000_000),
        &job.id,
        Some(partial.event_id.to_string()),
        json!({
            "segment_id": segment_id,
            "text": asr.text,
            "language": asr.language,
            "confidence": asr.confidence,
            "duration_ms": asr.duration_ms,
            "provider": asr.provider,
            "elapsed_ms": elapsed_ms
        }),
    )?;
    state.enqueue_job(NewModelJob {
        id: format!("job_{}", Uuid::now_v7().simple()),
        session_id: job.session_id.clone(),
        job_type: "translate".to_owned(),
        priority: 70,
        input: json!({
            "text": finalized.payload.get("text").cloned().unwrap_or(Value::Null),
            "segment_id": segment_id,
            "source_language": session.source_language,
            "target_language": session.target_language,
            "glossary": [],
            "context": []
        }),
        input_object_hash: None,
        idempotency_key: format!("translate:{segment_id}"),
    })?;
    Ok(())
}

fn apply_translation_result(
    state: &AppState,
    job: &aialra_event_store::ModelJobRecord,
    result: &Value,
    elapsed_ms: u64,
) -> Result<(), ApiError> {
    let translation: TranslationResponse = serde_json::from_value(result.clone())?;
    require_provider(&translation.provider, "ollama:", &["@cuda"])?;
    let segment_id = required_input_string(&job.input, "segment_id")?;
    state.emit_idempotent(
        &format!("{}:translation_final", job.id),
        &job.session_id,
        "gpu_translation",
        "translation.finalized",
        0,
        &job.id,
        None,
        json!({
            "segment_id": segment_id,
            "translation_id": format!("tr_{segment_id}"),
            "text": translation.text,
            "provider": translation.provider,
            "elapsed_ms": elapsed_ms
        }),
    )?;
    let interval = std::env::var("AIALRA_EXPLAIN_EVERY_SEGMENTS")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(5);
    let segment_count = state
        .store
        .list_events(&job.session_id)?
        .into_iter()
        .filter(|event| event.event_type == "segment.finalized")
        .count();
    if segment_count > 0 && segment_count.is_multiple_of(interval) {
        crate::explanation::enqueue_explanation(state, &job.session_id, "segment_volume")?;
    }
    Ok(())
}

fn apply_explanation_result(
    state: &AppState,
    job: &aialra_event_store::ModelJobRecord,
    result: &Value,
    elapsed_ms: u64,
) -> Result<(), ApiError> {
    let explanation: ExplanationResponse = serde_json::from_value(result.clone())?;
    require_provider(&explanation.provider, "ollama:", &["@cuda"])?;
    let allowed_segments = job
        .input
        .get("segments")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|item| item.get("id").and_then(Value::as_str))
        .collect::<std::collections::HashSet<_>>();
    let allowed_pages = job
        .input
        .get("asset_pages")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|item| item.get("id").and_then(Value::as_str))
        .collect::<std::collections::HashSet<_>>();
    if explanation
        .evidence_segment_ids
        .iter()
        .any(|id| !allowed_segments.contains(id.as_str()))
        || explanation
            .asset_page_ids
            .iter()
            .any(|id| !allowed_pages.contains(id.as_str()))
    {
        return Err(ApiError::bad_request(
            "explanation returned an invalid evidence reference",
        ));
    }
    state.emit_idempotent(
        &format!("{}:explanation_card", job.id),
        &job.session_id,
        "gpu_explainer",
        "explanation.card.created",
        0,
        &job.id,
        None,
        json!({
            "card_id": format!("card_{}", job.id.trim_start_matches("job_")),
            "fact_type": "background_explanation",
            "trigger": job.input.get("trigger").cloned().unwrap_or(json!("manual")),
            "elapsed_ms": elapsed_ms,
            "result": explanation
        }),
    )?;
    Ok(())
}

fn apply_summary_result(
    state: &AppState,
    job: &aialra_event_store::ModelJobRecord,
    result: &Value,
    elapsed_ms: u64,
) -> Result<(), ApiError> {
    let summary: SummaryResponse = serde_json::from_value(result.clone())?;
    require_provider(&summary.provider, "ollama:", &["@cuda"])?;
    let allowed_segments = job
        .input
        .get("segments")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|item| item.get("id").and_then(Value::as_str))
        .collect::<std::collections::HashSet<_>>();
    let allowed_pages = job
        .input
        .get("asset_pages")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|item| item.get("id").and_then(Value::as_str))
        .collect::<std::collections::HashSet<_>>();
    if summary
        .evidence_segment_ids
        .iter()
        .any(|id| !allowed_segments.contains(id.as_str()))
        || summary
            .asset_page_ids
            .iter()
            .any(|id| !allowed_pages.contains(id.as_str()))
    {
        return Err(ApiError::bad_request(
            "summary returned an invalid evidence reference",
        ));
    }
    state.emit_idempotent(
        &format!("{}:session_summary", job.id),
        &job.session_id,
        "gpu_summarizer",
        "session.summary.created",
        0,
        &job.id,
        None,
        json!({"summary_id": format!("summary_{}", job.id.trim_start_matches("job_")), "elapsed_ms": elapsed_ms, "result": summary}),
    )?;
    Ok(())
}

fn apply_asset_result(
    state: &AppState,
    job: &aialra_event_store::ModelJobRecord,
    result: &Value,
    elapsed_ms: u64,
) -> Result<(), ApiError> {
    let parsed: AssetParseResponse = serde_json::from_value(result.clone())?;
    let asset_id = required_input_string(&job.input, "asset_id")?;
    let media_type = required_input_string(&job.input, "media_type")?;
    for page in parsed.pages {
        let page_id = format!("page_{asset_id}_{}", page.page_number);
        state.store.insert_asset_page(&AssetPageRecord {
            id: page_id.clone(),
            asset_id: asset_id.clone(),
            page_number: page.page_number,
            title: Some(page.title.clone()),
            text_content: page.text.clone(),
            object_hash: media_type
                .starts_with("image/")
                .then(|| job.input_object_hash.clone())
                .flatten(),
            created_at: Utc::now(),
        })?;
        state.emit_idempotent(
            &format!("{}:asset_page:{}", job.id, page.page_number),
            &job.session_id,
            "asset_parser",
            "asset.page.extracted",
            0,
            &job.id,
            None,
            json!({
                "asset_id": asset_id,
                "page_id": page_id,
                "page_number": page.page_number,
                "title": page.title,
                "text": page.text,
                "parser": parsed.parser,
                "media_type": media_type,
                "elapsed_ms": elapsed_ms,
                "preview_url": media_type.starts_with("image/").then(|| format!("/api/v1/sessions/{}/assets/{asset_id}/content", job.session_id))
            }),
        )?;
    }
    Ok(())
}

fn finish_session_if_drained(state: &AppState, session_id: &str) -> Result<(), ApiError> {
    let Some(session) = state.store.get_session(session_id)? else {
        return Ok(());
    };
    if session.state != SessionState::Processing {
        return Ok(());
    }
    let counts = state.store.model_queue_counts(Some(session_id))?;
    if counts.queued + counts.leased > 0 {
        return Ok(());
    }
    let events = state.store.list_events(session_id)?;
    let has_segments = events
        .iter()
        .any(|event| event.event_type == "segment.finalized");
    let has_summary = events
        .iter()
        .any(|event| event.event_type == "session.summary.created");
    if counts.failed == 0 && has_segments && !has_summary {
        enqueue_summary(state, session_id, "recording_stopped")?;
        return Ok(());
    }
    let (next, event_type, payload) = if counts.failed > 0 {
        (
            SessionState::Failed,
            "session.failed",
            json!({"failed_model_jobs": counts.failed}),
        )
    } else {
        (
            SessionState::Completed,
            "session.completed",
            json!({"model_queue_drained": true}),
        )
    };
    state.store.transition_session(session_id, next)?;
    state.emit_idempotent(
        &format!("{session_id}:{event_type}"),
        session_id,
        "core",
        event_type,
        0,
        &format!("finish_{session_id}"),
        None,
        payload,
    )?;
    Ok(())
}

pub fn enqueue_asr(
    state: &AppState,
    session_id: &str,
    source_id: &str,
    captured_at_ms: u64,
    pcm: &[u8],
) -> anyhow::Result<()> {
    if pcm.is_empty() {
        return Ok(());
    }
    let stored = state.objects.put(pcm)?;
    let session = state
        .store
        .get_session(session_id)?
        .context("session not found")?;
    state.enqueue_job(NewModelJob {
        id: format!("job_{}", Uuid::now_v7().simple()),
        session_id: session_id.to_owned(),
        job_type: "asr".to_owned(),
        priority: 100,
        input: json!({
            "source_id": source_id,
            "captured_at_ms": captured_at_ms,
            "sample_rate": 16_000,
            "language": session.source_language,
            "initial_prompt": ""
        }),
        input_object_hash: Some(stored.hash.clone()),
        idempotency_key: format!(
            "asr:{session_id}:{source_id}:{captured_at_ms}:{}",
            stored.hash
        ),
    })?;
    Ok(())
}

pub fn finish_session_after_stop(state: &AppState, session_id: &str) -> Result<(), ApiError> {
    finish_session_if_drained(state, session_id)
}

pub fn enqueue_summary(
    state: &AppState,
    session_id: &str,
    trigger: &str,
) -> Result<aialra_event_store::ModelJobRecord, ApiError> {
    let session = state
        .store
        .get_session(session_id)?
        .ok_or_else(|| ApiError::not_found("session not found"))?;
    let events = state.store.list_events(session_id)?;
    let all_segments = events
        .iter()
        .filter_map(|event| {
            if event.event_type != "segment.finalized" {
                return None;
            }
            Some(json!({
                "id": event.payload.get("segment_id")?.as_str()?,
                "text": event.payload.get("text")?.as_str()?,
            }))
        })
        .collect::<Vec<_>>();
    let segments = evenly_sample(&all_segments, 160);
    if segments.is_empty() {
        return Err(ApiError::bad_request(
            "stable transcript is required before summary",
        ));
    }
    let all_pages = events
        .iter()
        .filter_map(|event| {
            if event.event_type != "asset.page.extracted" {
                return None;
            }
            Some(json!({
                "id": event.payload.get("page_id")?.as_str()?,
                "title": event.payload.get("title").and_then(Value::as_str).unwrap_or(""),
                "text": event.payload.get("text").and_then(Value::as_str).unwrap_or(""),
            }))
        })
        .collect::<Vec<_>>();
    let pages = evenly_sample(&all_pages, 24);
    let all_rolling_summaries = events
        .iter()
        .filter(|event| event.event_type == "explanation.card.created")
        .filter_map(|event| {
            event
                .payload
                .get("result")?
                .get("summary")?
                .as_str()
                .map(str::to_owned)
        })
        .collect::<Vec<_>>();
    let rolling_summaries = evenly_sample(&all_rolling_summaries, 48);
    let evidence_key = segments
        .iter()
        .filter_map(|item| item.get("id").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join(":");
    Ok(state.enqueue_job(NewModelJob {
        id: format!("job_{}", Uuid::now_v7().simple()),
        session_id: session_id.to_owned(),
        job_type: "summarize".to_owned(),
        priority: 20,
        input: json!({"segments": segments, "asset_pages": pages, "rolling_summaries": rolling_summaries, "target_language": session.target_language, "trigger": trigger}),
        input_object_hash: None,
        idempotency_key: format!("summarize:{session_id}:{evidence_key}"),
    })?)
}

fn evenly_sample<T: Clone>(items: &[T], limit: usize) -> Vec<T> {
    if items.len() <= limit {
        return items.to_vec();
    }
    if limit <= 1 {
        return items.first().cloned().into_iter().collect();
    }
    (0..limit)
        .map(|index| {
            let source_index = index * (items.len() - 1) / (limit - 1);
            items[source_index].clone()
        })
        .collect()
}

fn authorize(headers: &HeaderMap) -> Result<(), ApiError> {
    let expected = std::env::var("AIALRA_WORKER_TOKEN_SHA256")
        .map_err(|_| ApiError::unavailable("worker gateway token is not configured"))?;
    let token = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .ok_or_else(|| ApiError::unauthorized("worker authentication is required"))?;
    let actual = hex::encode(Sha256::digest(token.as_bytes()));
    if actual.len() != expected.len()
        || !actual
            .bytes()
            .zip(expected.bytes())
            .fold(true, |equal, (left, right)| equal & (left == right))
    {
        return Err(ApiError::unauthorized("worker authentication failed"));
    }
    Ok(())
}

fn validate_worker_id(value: &str) -> Result<(), ApiError> {
    let valid = !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'));
    valid
        .then_some(())
        .ok_or_else(|| ApiError::bad_request("worker_id is invalid"))
}

fn allowed_capabilities(values: Vec<String>) -> Vec<String> {
    values
        .into_iter()
        .filter(|value| {
            matches!(
                value.as_str(),
                "asr" | "translate" | "explain" | "summarize" | "asset_parse"
            )
        })
        .collect()
}

fn require_provider(provider: &str, prefix: &str, devices: &[&str]) -> Result<(), ApiError> {
    if provider.starts_with(prefix) && devices.iter().any(|device| provider.ends_with(device)) {
        Ok(())
    } else {
        Err(ApiError::bad_request(
            "production model result did not prove an allowed local provider",
        ))
    }
}

fn required_input_string(input: &Value, key: &str) -> Result<String, ApiError> {
    input
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| ApiError::bad_request(format!("model job input is missing {key}")))
}

fn sanitize_error_kind(value: &str) -> String {
    value
        .chars()
        .filter(|character| character.is_ascii_alphanumeric() || matches!(character, '_' | '-'))
        .take(64)
        .collect()
}

use axum::response::IntoResponse;

#[cfg(test)]
mod tests {
    use super::{evenly_sample, require_provider};

    #[test]
    fn provider_gate_allows_cpu_asr_and_requires_cuda_llm() {
        assert!(
            require_provider(
                "faster-whisper:small@cpu",
                "faster-whisper:",
                &["@cpu", "@cuda"]
            )
            .is_ok()
        );
        assert!(require_provider("ollama:qwen2.5:3b-instruct@cuda", "ollama:", &["@cuda"]).is_ok());
        assert!(require_provider("ollama:qwen2.5:3b-instruct@cpu", "ollama:", &["@cuda"]).is_err());
    }

    #[test]
    fn summary_sampling_covers_the_whole_timeline_in_order() {
        let sampled = evenly_sample(&(0..100).collect::<Vec<_>>(), 5);
        assert_eq!(sampled, vec![0, 24, 49, 74, 99]);
        assert_eq!(evenly_sample(&[3, 5, 8], 5), vec![3, 5, 8]);
    }
}
