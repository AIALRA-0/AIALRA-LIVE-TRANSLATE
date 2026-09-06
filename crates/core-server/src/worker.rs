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
    pub source_text: Option<String>,
    pub text: String,
    pub provider: String,
    #[serde(default)]
    pub source_language: Option<String>,
    #[serde(default)]
    pub target_language: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct ExplanationResponse {
    #[serde(alias = "summary")]
    pub paragraph_summary: String,
    #[serde(default)]
    pub terms: Vec<ExplanationTerm>,
    pub evidence_segment_ids: Vec<String>,
    pub asset_page_ids: Vec<String>,
    pub provider: String,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct ExplanationTerm {
    pub term: String,
    pub explanation: String,
    pub evidence_segment_ids: Vec<String>,
    pub asset_page_ids: Vec<String>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct RareTerm {
    pub term: String,
    pub one_line: String,
    pub evidence_segment_ids: Vec<String>,
    pub asset_page_ids: Vec<String>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct SummaryResponse {
    pub overview: String,
    pub key_points: Vec<String>,
    pub terminology: Vec<RareTerm>,
    #[serde(default)]
    pub open_questions: Vec<String>,
    pub evidence_segment_ids: Vec<String>,
    pub asset_page_ids: Vec<String>,
    pub provider: String,
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
