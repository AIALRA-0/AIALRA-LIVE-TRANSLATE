//! Validated result contracts returned by the private GPU agent.

use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize)]
pub struct AsrResponse {
    pub text: String,
    pub language: String,
    pub confidence: f32,
    pub duration_ms: u64,
    pub provider: String,
}

#[derive(Debug, Deserialize)]
pub struct TranslationResponse {
    pub text: String,
    pub provider: String,
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
