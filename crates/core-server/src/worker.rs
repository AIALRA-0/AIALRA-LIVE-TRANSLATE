//! Loopback-only client for local ASR, translation, explanation, and document parsing.

use anyhow::{Context, Result};
use reqwest::multipart::{Form, Part};
use serde::{Deserialize, Serialize};

#[derive(Clone)]
pub struct WorkerClient {
    base_url: String,
    client: reqwest::Client,
}

impl WorkerClient {
    pub fn new(base_url: &str) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_owned(),
            client: reqwest::Client::new(),
        }
    }

    pub async fn health(&self) -> Result<WorkerHealth> {
        self.client
            .get(format!("{}/health", self.base_url))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await
            .context("decode worker health")
    }

    pub async fn transcribe(&self, request: &AsrRequest) -> Result<AsrResponse> {
        self.post_json("/v1/asr/transcribe", request).await
    }

    pub async fn translate(&self, request: &TranslationRequest) -> Result<TranslationResponse> {
        self.post_json("/v1/translate", request).await
    }

    pub async fn explain(&self, request: &ExplanationRequest) -> Result<ExplanationResponse> {
        self.post_json("/v1/explain", request).await
    }

    pub async fn parse_asset(
        &self,
        file_name: &str,
        media_type: &str,
        bytes: Vec<u8>,
    ) -> Result<AssetParseResponse> {
        // The original filename is metadata only; the worker receives bytes through multipart upload.
        let part = Part::bytes(bytes)
            .file_name(file_name.to_owned())
            .mime_str(media_type)?;
        self.client
            .post(format!("{}/v1/assets/parse", self.base_url))
            .multipart(Form::new().part("file", part))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await
            .context("decode asset parse response")
    }

    async fn post_json<T, R>(&self, path: &str, request: &T) -> Result<R>
    where
        T: Serialize + ?Sized,
        R: for<'de> Deserialize<'de>,
    {
        self.client
            .post(format!("{}{}", self.base_url, path))
            .json(request)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await
            .context("decode worker response")
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct WorkerHealth {
    pub status: String,
    pub asr_available: bool,
    pub ollama_available: bool,
    pub model: String,
}

#[derive(Debug, Serialize)]
pub struct AsrRequest {
    pub pcm_s16le_base64: String,
    pub sample_rate: u32,
    pub language: String,
    pub initial_prompt: String,
}

#[derive(Debug, Deserialize)]
pub struct AsrResponse {
    pub text: String,
    pub language: String,
    pub confidence: f32,
    pub duration_ms: u64,
    pub provider: String,
}

#[derive(Debug, Serialize)]
pub struct TranslationRequest {
    pub text: String,
    pub source_language: String,
    pub target_language: String,
    pub glossary: Vec<GlossaryConstraint>,
    pub context: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct GlossaryConstraint {
    pub source: String,
    pub preferred: String,
    pub do_not_translate: bool,
}

#[derive(Debug, Deserialize)]
pub struct TranslationResponse {
    pub text: String,
    pub provider: String,
}

#[derive(Debug, Serialize)]
pub struct ExplanationRequest {
    pub segments: Vec<EvidenceSegment>,
    pub asset_pages: Vec<EvidencePage>,
    pub target_language: String,
}

#[derive(Debug, Serialize)]
pub struct EvidenceSegment {
    pub id: String,
    pub text: String,
}

#[derive(Debug, Serialize)]
pub struct EvidencePage {
    pub id: String,
    pub title: String,
    pub text: String,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct ExplanationResponse {
    pub summary: String,
    pub missing_context: Vec<MissingContext>,
    pub rare_terms: Vec<RareTerm>,
    pub possible_asr_errors: Vec<String>,
    pub review_questions: Vec<String>,
    pub evidence_segment_ids: Vec<String>,
    pub asset_page_ids: Vec<String>,
    pub confidence: f32,
    pub provider: String,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct MissingContext {
    pub text: String,
    pub evidence_segment_ids: Vec<String>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct RareTerm {
    pub term: String,
    pub one_line: String,
    pub evidence_segment_ids: Vec<String>,
    pub asset_page_ids: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct AssetParseResponse {
    pub parser: String,
    pub pages: Vec<ParsedPage>,
}

#[derive(Debug, Deserialize)]
pub struct ParsedPage {
    pub page_number: u32,
    pub title: String,
    pub text: String,
}
