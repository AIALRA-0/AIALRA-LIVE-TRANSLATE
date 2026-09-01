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
use std::collections::HashSet;
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
    job_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct WorkerRequest {
    worker_id: String,
}

#[derive(Debug, Deserialize)]
pub struct CompleteRequest {
    worker_id: String,
    /// The worker must echo the durable job key so a stale or cross-job result
    /// cannot be attached to the current lease by accident.
    idempotency_key: String,
    result: Value,
    elapsed_ms: u64,
    runtime_proof: RuntimeProof,
}

#[derive(Debug, Deserialize)]
pub struct RuntimeProof {
    /// The authenticated worker that observed the provider result.
    worker_id: String,
    /// Provider string returned by the model adapter or document parser.
    provider: String,
    /// `cuda` for language/vision inference, or `cpu` for deterministic format parsers.
    execution_device: String,
    /// Model or parser identifier without the execution-device suffix.
    model: String,
    /// Bounded wall-clock evidence, never a host or device identifier.
    observed_at_unix_ms: u64,
}

#[derive(Debug, Deserialize)]
pub struct FailRequest {
    worker_id: String,
    error_kind: String,
    retryable: bool,
    retry_after_seconds: i64,
    error_stage: Option<String>,
    diagnostic_id: Option<String>,
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
        if let Some(job) = state.store.lease_model_job_for(
            &request.worker_id,
            &capabilities,
            LEASE_SECONDS,
            request.job_id.as_deref(),
        )? {
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
    validate_worker_id(&request.worker_id)?;
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
    let worker_id = headers
        .get("x-aialra-worker-id")
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| ApiError::unauthorized("worker identity is required for job input"))?;
    validate_worker_id(worker_id)?;
    let job = state
        .store
        .get_model_job(&job_id)?
        .ok_or_else(|| ApiError::not_found("model job not found"))?;
    if job.status != "leased" || job.lease_owner.as_deref() != Some(worker_id) {
        return Err(ApiError::conflict(
            "model job input is not leased to this worker",
        ));
    }
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
    if request.idempotency_key != job.idempotency_key {
        return Err(ApiError::conflict(
            "model result idempotency key does not match the leased job",
        ));
    }
    validate_runtime_proof(
        &job,
        &request.worker_id,
        &request.result,
        &request.runtime_proof,
    )?;
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
        json!({
            "job_id": job_id,
            "job_type": job.job_type,
            "elapsed_ms": request.elapsed_ms,
            "provider": request.runtime_proof.provider,
            "execution_device": request.runtime_proof.execution_device,
            "model": request.runtime_proof.model,
            "runtime_proof_at_unix_ms": request.runtime_proof.observed_at_unix_ms
        }),
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
    validate_worker_id(&request.worker_id)?;
    let error_stage = validate_error_stage(request.error_stage.as_deref())?;
    let diagnostic_id = validate_diagnostic_id(request.diagnostic_id.as_deref())?;
    let error_kind = sanitize_error_kind(&request.error_kind);
    let job = state
        .store
        .get_model_job(&job_id)?
        .ok_or_else(|| ApiError::not_found("model job not found"))?;
    let status = state
        .store
        .retry_or_fail_model_job(
            &job_id,
            &request.worker_id,
            &error_kind,
            request.retryable,
            request.retry_after_seconds,
        )?
        .ok_or_else(|| ApiError::conflict("model job failure lost its lease"))?;
    let event_type = if status == "queued" {
        "model.job.retry_scheduled"
    } else {
        "model.job.failed"
    };
    let mut event_payload = json!({
        "job_id": job_id,
        "job_type": job.job_type,
        "error_kind": error_kind,
    });
    if let Some(value) = error_stage {
        event_payload["error_stage"] = json!(value);
    }
    if let Some(value) = diagnostic_id {
        event_payload["diagnostic_id"] = json!(value);
    }
    let _ = state.emit_idempotent(
        &format!("{job_id}:{event_type}:{}", job.attempts),
        &job.session_id,
        "model_scheduler",
        event_type,
        0,
        &job_id,
        None,
        event_payload,
    );
    if status == "failed" && job.job_type == "summarize" {
        // A final summary is a retryable background projection. Keep the
        // course facts complete and expose the failed summary separately so a
        // user can request it again without marking the whole session failed.
        state.emit_idempotent(
            &format!("{job_id}:summary_failed"),
            &job.session_id,
            "gpu_summarizer",
            "session.summary.failed",
            0,
            &job_id,
            None,
            json!({
                "job_id": job_id,
                "error_kind": error_kind,
                "manual_retry_available": true,
            }),
        )?;
    }
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
    state.emit_idempotent(
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
            "elapsed_ms": elapsed_ms,
            "display_mode": "internal_fragment"
        }),
    )?;
    maybe_finalize_paragraph(state, &job.session_id, false)?;
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
    let paragraph_id = job
        .input
        .get("paragraph_id")
        .and_then(Value::as_str)
        .or_else(|| job.input.get("segment_id").and_then(Value::as_str))
        .filter(|value| !value.is_empty())
        .ok_or_else(|| ApiError::bad_request("model job input is missing paragraph_id"))?
        .to_owned();
    state.emit_idempotent(
        &format!("{}:translation_final", job.id),
        &job.session_id,
        "gpu_translation",
        "translation.finalized",
        0,
        &job.id,
        None,
        json!({
            "paragraph_id": paragraph_id,
            "segment_id": paragraph_id,
            "translation_id": format!("tr_{paragraph_id}"),
            "source_text": translation.source_text,
            "text": translation.text,
            "provider": translation.provider,
            "elapsed_ms": elapsed_ms
        }),
    )?;
    maybe_enqueue_coherent_explanation(state, &job.session_id, &job.id)?;
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
    if maybe_finalize_paragraph(state, session_id, true)?.is_some() {
        return Ok(());
    }
    let events = state.store.list_events(session_id)?;
    let has_segments = events.iter().any(|event| {
        matches!(
            event.event_type.as_str(),
            "paragraph.finalized" | "segment.finalized"
        )
    });
    let has_summary = events
        .iter()
        .any(|event| event.event_type == "session.summary.created");
    let failed_non_summary = state.store.has_failed_non_summary_job(session_id)?;
    if !failed_non_summary && counts.failed == 0 && has_segments && !has_summary {
        enqueue_summary(state, session_id, "recording_stopped")?;
        return Ok(());
    }
    let (next, event_type, payload) = if failed_non_summary {
        (
            SessionState::Failed,
            "session.failed",
            json!({"failed_model_jobs": counts.failed}),
        )
    } else {
        (
            SessionState::Completed,
            "session.completed",
            json!({
                "model_queue_drained": true,
                "summary_available": has_summary,
                "summary_retryable": !has_summary && counts.failed > 0,
            }),
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
    if events
        .iter()
        .any(|event| event.event_type == "session.summary.created")
    {
        return Err(ApiError::conflict("session summary already exists"));
    }
    let has_paragraphs = events
        .iter()
        .any(|event| event.event_type == "paragraph.finalized");
    let all_segments = events
        .iter()
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
                "text": event.payload.get("text")?.as_str()?,
            }))
        })
        .collect::<Vec<_>>();
    let segments = evenly_sample(&all_segments, 64);
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
    let rolling_summaries = evenly_sample(&all_rolling_summaries, 24);
    let evidence_key = segments
        .iter()
        .filter_map(|item| item.get("id").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join(":");
    let idempotency_key = format!("summarize:{session_id}:{evidence_key}");
    if let Some(existing) = state.store.get_model_job_by_key(&idempotency_key)?
        && existing.status == "failed"
    {
        state
            .store
            .requeue_failed_summary_by_key(&idempotency_key)?;
    }
    Ok(state.enqueue_job(NewModelJob {
        id: format!("job_{}", Uuid::now_v7().simple()),
        session_id: session_id.to_owned(),
        job_type: "summarize".to_owned(),
        priority: 20,
        input: json!({"segments": segments, "asset_pages": pages, "rolling_summaries": rolling_summaries, "target_language": session.target_language, "trigger": trigger}),
        input_object_hash: None,
        idempotency_key,
    })?)
}

const PARAGRAPH_MIN_TERMINAL_CHARS: usize = 100;
const PARAGRAPH_HARD_CHARS: usize = 260;
const PARAGRAPH_HARD_SEGMENTS: usize = 4;
const AUTO_EXPLAIN_MIN_PARAGRAPHS: usize = 4;
const AUTO_EXPLAIN_MIN_CHARS: usize = 900;

fn maybe_finalize_paragraph(
    state: &AppState,
    session_id: &str,
    force: bool,
) -> Result<Option<aialra_event_protocol::EventEnvelope>, ApiError> {
    let events = state.store.list_events(session_id)?;
    let consumed = events
        .iter()
        .filter(|event| event.event_type == "paragraph.finalized")
        .flat_map(|event| {
            event
                .payload
                .get("segment_ids")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect::<Vec<_>>()
        })
        .collect::<HashSet<_>>();
    let pending = events
        .iter()
        .filter(|event| event.event_type == "segment.finalized")
        .filter_map(|event| {
            let id = event.payload.get("segment_id")?.as_str()?;
            (!consumed.contains(id)).then_some((
                event,
                id.to_owned(),
                event.payload.get("text")?.as_str()?.trim().to_owned(),
            ))
        })
        .filter(|(_, _, text)| !text.is_empty())
        .collect::<Vec<_>>();
    if pending.is_empty() {
        return Ok(None);
    }
    let text = join_caption_fragments(pending.iter().map(|(_, _, text)| text.as_str()));
    let trimmed = text.trim_end();
    let terminal = !trimmed.ends_with("...")
        && !trimmed.ends_with('…')
        && trimmed.chars().last().is_some_and(|character| {
            matches!(character, '.' | '?' | '!' | '。' | '？' | '！' | ':' | '：')
        });
    let ready = force
        || pending.len() >= PARAGRAPH_HARD_SEGMENTS
        || text.chars().count() >= PARAGRAPH_HARD_CHARS
        || (pending.len() >= 2 && text.chars().count() >= PARAGRAPH_MIN_TERMINAL_CHARS && terminal);
    if !ready {
        return Ok(None);
    }
    let session = state
        .store
        .get_session(session_id)?
        .ok_or_else(|| ApiError::not_found("session not found"))?;
    let ids = pending
        .iter()
        .map(|(_, id, _)| id.clone())
        .collect::<Vec<_>>();
    let evidence_key = ids.join(":");
    let paragraph_id = format!(
        "para_{}",
        Uuid::new_v5(&Uuid::NAMESPACE_OID, evidence_key.as_bytes()).simple()
    );
    let last = pending.last().expect("pending is non-empty").0;
    let provider = pending
        .iter()
        .filter_map(|(event, _, _)| event.payload.get("provider").and_then(Value::as_str))
        .next_back()
        .unwrap_or("unknown");
    let paragraph = state.emit_idempotent(
        &format!("{session_id}:paragraph:{evidence_key}"),
        session_id,
        "course_paragraph_assembler",
        "paragraph.finalized",
        last.captured_at_monotonic_ns,
        &paragraph_id,
        Some(last.event_id.to_string()),
        json!({
            "paragraph_id": paragraph_id.clone(),
            "segment_ids": ids,
            "text": text,
            "provider": provider,
            "assembly": "coherent-v1"
        }),
    )?;
    let context = events
        .iter()
        .rev()
        .filter(|event| event.event_type == "paragraph.finalized")
        .filter_map(|event| event.payload.get("text").and_then(Value::as_str))
        .take(2)
        .map(str::to_owned)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>();
    state.enqueue_job(NewModelJob {
        id: format!("job_{}", Uuid::now_v7().simple()),
        session_id: session_id.to_owned(),
        job_type: "translate".to_owned(),
        priority: 70,
        input: json!({
            "text": paragraph.payload.get("text").cloned().unwrap_or(Value::Null),
            "paragraph_id": paragraph_id.clone(),
            "source_language": session.source_language,
            "target_language": session.target_language,
            "glossary": [],
            "context": context
        }),
        input_object_hash: None,
        idempotency_key: format!("translate:{paragraph_id}"),
    })?;
    Ok(Some(paragraph))
}

fn maybe_enqueue_coherent_explanation(
    state: &AppState,
    session_id: &str,
    current_job_id: &str,
) -> Result<(), ApiError> {
    if state.store.active_model_jobs_excluding(
        session_id,
        &["asr", "translate", "explain"],
        current_job_id,
    )? > 0
    {
        return Ok(());
    }
    let events = state.store.list_events(session_id)?;
    let paragraphs = events
        .iter()
        .filter(|event| event.event_type == "paragraph.finalized")
        .filter_map(|event| {
            Some((
                event.payload.get("paragraph_id")?.as_str()?.to_owned(),
                event.payload.get("text")?.as_str()?.to_owned(),
            ))
        })
        .collect::<Vec<_>>();
    // Explanation evidence uses the same stable IDs as the input window.  Once a
    // coherent paragraph exists those IDs are paragraph IDs, not the internal
    // acoustic fragment IDs.  Comparing the wrong namespace made every later
    // translation look like unexplained content and caused a card burst.
    let last_explained = events
        .iter()
        .filter(|event| event.event_type == "explanation.card.created")
        .filter_map(|event| event.payload.get("result"))
        .filter_map(|result| result.get("evidence_segment_ids"))
        .filter_map(Value::as_array)
        .flat_map(|ids| ids.iter().filter_map(Value::as_str))
        .filter_map(|id| {
            paragraphs
                .iter()
                .position(|(paragraph_id, _)| paragraph_id == id)
        })
        .max();
    let pending = &paragraphs[last_explained.map_or(0, |index| index + 1)..];
    let chars = pending
        .iter()
        .map(|(_, text)| text.chars().count())
        .sum::<usize>();
    if pending.len() < AUTO_EXPLAIN_MIN_PARAGRAPHS || chars < AUTO_EXPLAIN_MIN_CHARS {
        return Ok(());
    }
    crate::explanation::enqueue_explanation(state, session_id, "coherent_passage")?;
    Ok(())
}

fn join_caption_fragments<'a>(parts: impl Iterator<Item = &'a str>) -> String {
    let mut output = String::new();
    for part in parts.map(str::trim).filter(|part| !part.is_empty()) {
        if output.is_empty() {
            output.push_str(part);
            continue;
        }
        let starts_with_closing_punctuation = part
            .chars()
            .next()
            .is_some_and(|character| "，。！？；：、,.!?;:)]}»”\"'".contains(character));
        let previous_is_cjk = output
            .chars()
            .rev()
            .find(|character| !character.is_whitespace())
            .is_some_and(is_cjk_character);
        let current_is_cjk = part
            .chars()
            .find(|character| !character.is_whitespace())
            .is_some_and(is_cjk_character);
        if !starts_with_closing_punctuation && !previous_is_cjk && !current_is_cjk {
            output.push(' ');
        }
        output.push_str(part);
    }
    output
}

fn is_cjk_character(character: char) -> bool {
    matches!(
        character as u32,
        0x3040..=0x30ff
            | 0x3400..=0x4dbf
            | 0x4e00..=0x9fff
            | 0xf900..=0xfaff
            | 0xac00..=0xd7af
    )
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

fn validate_runtime_proof(
    job: &aialra_event_store::ModelJobRecord,
    worker_id: &str,
    result: &Value,
    proof: &RuntimeProof,
) -> Result<(), ApiError> {
    if proof.worker_id != worker_id {
        return Err(ApiError::bad_request(
            "model runtime proof belongs to a different worker",
        ));
    }
    validate_worker_id(&proof.worker_id)?;
    let provider = result
        .get("provider")
        .or_else(|| result.get("parser"))
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| ApiError::bad_request("model result is missing provider proof"))?;
    if proof.provider != provider {
        return Err(ApiError::bad_request(
            "model runtime proof does not match the returned provider",
        ));
    }
    let (expected_model, expected_device) = if let Some((model, device)) = provider.rsplit_once('@')
    {
        if model.is_empty() || device.is_empty() {
            return Err(ApiError::bad_request("model provider proof is malformed"));
        }
        (model, device)
    } else if job.job_type == "asset_parse" {
        // Text/PDF/PPTX parsers are local CPU adapters and intentionally do not
        // claim CUDA inference.  Image parsing must still return an Ollama CUDA
        // provider and is rejected by the model-specific result gate below.
        (provider, "cpu")
    } else {
        return Err(ApiError::bad_request(
            "model provider proof lacks execution device",
        ));
    };
    if !matches!(expected_device, "cpu" | "cuda")
        || proof.execution_device != expected_device
        || proof.model != expected_model
    {
        return Err(ApiError::bad_request(
            "model runtime proof does not match provider execution details",
        ));
    }
    let now_ms = Utc::now().timestamp_millis().max(0) as u64;
    if proof.observed_at_unix_ms == 0 || now_ms.abs_diff(proof.observed_at_unix_ms) > 900_000 {
        return Err(ApiError::bad_request(
            "model runtime proof timestamp is outside the acceptance window",
        ));
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

fn validate_error_stage(value: Option<&str>) -> Result<Option<&str>, ApiError> {
    match value {
        None => Ok(None),
        Some(
            value @ ("gateway_response" | "job_payload" | "model_http" | "model_json"
            | "execution_device"),
        ) => Ok(Some(value)),
        Some(_) => Err(ApiError::bad_request("error_stage is invalid")),
    }
}

fn validate_diagnostic_id(value: Option<&str>) -> Result<Option<&str>, ApiError> {
    match value {
        None => Ok(None),
        Some(value)
            if value.len() == 21
                && value.starts_with("diag_")
                && value.as_bytes()[5..]
                    .iter()
                    .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f')) =>
        {
            Ok(Some(value))
        }
        Some(_) => Err(ApiError::bad_request("diagnostic_id is invalid")),
    }
}

use axum::response::IntoResponse;

#[cfg(test)]
mod tests {
    use super::{
        PARAGRAPH_HARD_SEGMENTS, enqueue_summary, evenly_sample, join_caption_fragments,
        maybe_enqueue_coherent_explanation, maybe_finalize_paragraph, require_provider,
        validate_diagnostic_id, validate_error_stage,
    };
    use crate::app::AppState;
    use aialra_event_store::NewSession;
    use serde_json::json;

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
    fn optional_failure_diagnostics_accept_old_clients_and_fixed_values() {
        assert_eq!(validate_error_stage(None).unwrap(), None);
        assert_eq!(validate_diagnostic_id(None).unwrap(), None);
        for stage in [
            "gateway_response",
            "job_payload",
            "model_http",
            "model_json",
            "execution_device",
        ] {
            assert_eq!(validate_error_stage(Some(stage)).unwrap(), Some(stage));
        }
        assert_eq!(
            validate_diagnostic_id(Some("diag_0123456789abcdef")).unwrap(),
            Some("diag_0123456789abcdef")
        );
    }

    #[test]
    fn optional_failure_diagnostics_reject_unbounded_values() {
        assert!(validate_error_stage(Some("provider_response_invalid")).is_err());
        assert!(validate_diagnostic_id(Some("diag_0123456789ABCDEf")).is_err());
        assert!(validate_diagnostic_id(Some("diag_0123")).is_err());
    }

    #[test]
    fn summary_sampling_covers_the_whole_timeline_in_order() {
        let sampled = evenly_sample(&(0..100).collect::<Vec<_>>(), 5);
        assert_eq!(sampled, vec![0, 24, 49, 74, 99]);
        assert_eq!(evenly_sample(&[3, 5, 8], 5), vec![3, 5, 8]);
    }

    #[test]
    fn completed_summary_cannot_be_enqueued_twice() {
        let temp = tempfile::tempdir().unwrap();
        let state = AppState::open(temp.path()).unwrap();
        state
            .store
            .create_session(&NewSession {
                id: "session_summary_once".to_owned(),
                title: "Summary once".to_owned(),
                source_language: "en".to_owned(),
                target_language: "zh-CN".to_owned(),
                privacy_mode: "local_only".to_owned(),
                consent_confirmed: true,
                demo_mode: false,
            })
            .unwrap();
        state
            .emit_idempotent(
                "summary-once-event",
                "session_summary_once",
                "gpu_summarizer",
                "session.summary.created",
                0,
                "summary-once-job",
                None,
                json!({"summary_id": "summary-once"}),
            )
            .unwrap();
        assert!(enqueue_summary(&state, "session_summary_once", "manual").is_err());
    }

    #[test]
    fn paragraph_assembly_keeps_caption_fragments_readable() {
        assert_eq!(
            join_caption_fragments(["The attention", " mechanism uses", " context."].into_iter()),
            "The attention mechanism uses context."
        );
        assert_eq!(
            join_caption_fragments(["这是一个", " 连贯的段落", "。"].into_iter()),
            "这是一个连贯的段落。"
        );
        assert_eq!(
            join_caption_fragments(["A model", " uses", " evidence."].into_iter()),
            "A model uses evidence."
        );
        assert_eq!(PARAGRAPH_HARD_SEGMENTS, 4);
    }

    #[test]
    fn acoustic_fragments_create_one_translation_job_for_one_coherent_paragraph() {
        let temp = tempfile::tempdir().unwrap();
        let state = AppState::open(temp.path()).unwrap();
        state
            .store
            .create_session(&NewSession {
                id: "session_paragraph".to_owned(),
                title: "Paragraph assembly".to_owned(),
                source_language: "en".to_owned(),
                target_language: "zh-CN".to_owned(),
                privacy_mode: "local_only".to_owned(),
                consent_confirmed: true,
                demo_mode: false,
            })
            .unwrap();
        for index in 1..=4 {
            state
                .emit_idempotent(
                    &format!("segment-{index}"),
                    "session_paragraph",
                    "gpu_asr",
                    "segment.finalized",
                    index,
                    &format!("segment-{index}"),
                    None,
                    json!({"segment_id": format!("seg-{index}"), "text": format!("fragment {index}"), "provider": "faster-whisper:small@cuda", "display_mode": "internal_fragment"}),
                )
                .unwrap();
            let paragraph = maybe_finalize_paragraph(&state, "session_paragraph", false).unwrap();
            assert_eq!(paragraph.is_some(), index == 4);
        }
        let events = state.store.list_events("session_paragraph").unwrap();
        assert_eq!(
            events
                .iter()
                .filter(|event| event.event_type == "paragraph.finalized")
                .count(),
            1
        );
        assert_eq!(
            state
                .store
                .model_queue_counts(Some("session_paragraph"))
                .unwrap()
                .queued,
            1
        );
    }

    #[test]
    fn automatic_explanation_waits_for_a_large_coherent_passage() {
        let temp = tempfile::tempdir().unwrap();
        let state = AppState::open(temp.path()).unwrap();
        state
            .store
            .create_session(&NewSession {
                id: "session_explanation".to_owned(),
                title: "Explanation gate".to_owned(),
                source_language: "en".to_owned(),
                target_language: "zh-CN".to_owned(),
                privacy_mode: "local_only".to_owned(),
                consent_confirmed: true,
                demo_mode: false,
            })
            .unwrap();
        for index in 1..=4 {
            state
                .emit_idempotent(
                    &format!("paragraph-{index}"),
                    "session_explanation",
                    "course_paragraph_assembler",
                    "paragraph.finalized",
                    index,
                    &format!("paragraph-{index}"),
                    None,
                    json!({"paragraph_id": format!("para-{index}"), "segment_ids": [format!("seg-{index}")], "text": "A coherent technical passage explains attention, representation learning, optimization, and the evidence needed to compare these mechanisms in a lecture setting. This paragraph intentionally carries enough semantic content for the teaching gate."}),
                )
                .unwrap();
        }
        maybe_enqueue_coherent_explanation(&state, "session_explanation", "current-translate")
            .unwrap();
        assert_eq!(
            state
                .store
                .model_queue_counts(Some("session_explanation"))
                .unwrap()
                .queued,
            1
        );
        let job = state
            .store
            .lease_model_job("test-worker", &["explain".to_owned()], 60)
            .unwrap()
            .unwrap();
        assert_eq!(job.job_type, "explain");
        assert_eq!(
            job.input.get("trigger").and_then(|value| value.as_str()),
            Some("coherent_passage")
        );
    }

    #[test]
    fn explanation_trigger_does_not_repeat_already_explained_paragraphs() {
        let temp = tempfile::tempdir().unwrap();
        let state = AppState::open(temp.path()).unwrap();
        state
            .store
            .create_session(&NewSession {
                id: "session_explanation_dedup".to_owned(),
                title: "Explanation deduplication".to_owned(),
                source_language: "en".to_owned(),
                target_language: "zh-CN".to_owned(),
                privacy_mode: "local_only".to_owned(),
                consent_confirmed: true,
                demo_mode: false,
            })
            .unwrap();
        let paragraph_text = "A coherent technical passage explains a mechanism, its assumptions, and the evidence needed to compare it in a lecture setting. ".repeat(8);
        for index in 1..=4 {
            state
                .emit_idempotent(
                    &format!("paragraph-dedup-{index}"),
                    "session_explanation_dedup",
                    "course_paragraph_assembler",
                    "paragraph.finalized",
                    index,
                    &format!("paragraph-dedup-{index}"),
                    None,
                    json!({"paragraph_id": format!("para-{index}"), "text": paragraph_text}),
                )
                .unwrap();
        }
        maybe_enqueue_coherent_explanation(
            &state,
            "session_explanation_dedup",
            "first-translation",
        )
        .unwrap();
        let first_job = state
            .store
            .lease_model_job("explain-worker", &["explain".to_owned()], 60)
            .unwrap()
            .unwrap();
        state
            .store
            .complete_model_job(&first_job.id, "explain-worker", &json!({"summary": "done"}))
            .unwrap();
        state
            .emit_idempotent(
                "explanation-dedup-result",
                "session_explanation_dedup",
                "gpu_explainer",
                "explanation.card.created",
                0,
                &first_job.id,
                None,
                json!({"result": {"evidence_segment_ids": ["para-1", "para-2", "para-3", "para-4"]}}),
            )
            .unwrap();

        maybe_enqueue_coherent_explanation(
            &state,
            "session_explanation_dedup",
            "second-translation",
        )
        .unwrap();
        assert_eq!(
            state
                .store
                .model_queue_counts(Some("session_explanation_dedup"))
                .unwrap()
                .queued,
            0
        );

        for index in 5..=8 {
            state
                .emit_idempotent(
                    &format!("paragraph-dedup-{index}"),
                    "session_explanation_dedup",
                    "course_paragraph_assembler",
                    "paragraph.finalized",
                    index,
                    &format!("paragraph-dedup-{index}"),
                    None,
                    json!({"paragraph_id": format!("para-{index}"), "text": paragraph_text}),
                )
                .unwrap();
        }
        maybe_enqueue_coherent_explanation(
            &state,
            "session_explanation_dedup",
            "third-translation",
        )
        .unwrap();
        assert_eq!(
            state
                .store
                .model_queue_counts(Some("session_explanation_dedup"))
                .unwrap()
                .queued,
            1
        );
    }
}
