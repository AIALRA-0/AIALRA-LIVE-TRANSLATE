//! Validated result contracts returned by the private GPU agent.

use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize)]
pub struct AsrResponse {
    pub text: String,
    pub language: String,
    pub confidence: f32,
    pub duration_ms: u64,
    pub provider: String,
    #[serde(default)]
    pub speaker_observation: Option<crate::speakers::Observation>,
}

#[derive(Debug, Deserialize)]
pub struct TranslationResponse {
    // An optional legacy source_text in the wire result is deliberately ignored.
    // Core binds the original text from the persisted translation job instead.
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub background_reference: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct RareTerm {
    pub term: String,
    pub one_line: String,
    pub evidence_segment_ids: Vec<String>,
    pub asset_page_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub background_reference: Option<String>,
}

/// Background references are separate from lecturer evidence and never fetched by Core.
pub fn valid_background_reference(reference: Option<&str>) -> bool {
    let Some(reference) = reference else {
        return true;
    };
    let Ok(url) = reqwest::Url::parse(reference) else {
        return false;
    };
    url.scheme() == "https"
        && url.username().is_empty()
        && url.password().is_none()
        && url.port().is_none()
        && url.query().is_none()
        && matches!(
            url.host_str(),
            Some(
                "ocw.mit.edu"
                    | "docs.amd.com"
                    | "www.nist.gov"
                    | "www.ti.com"
                    | "developerhelp.microchip.com"
                    | "www.intel.com"
                    | "www.rfc-editor.org"
                    | "limsk.ece.gatech.edu"
                    | "rocmdocs.amd.com"
            )
        )
}

#[cfg(test)]
mod reference_tests {
    use super::valid_background_reference;

    #[test]
    fn background_links_reject_private_or_executable_destinations() {
        assert!(valid_background_reference(None));
        assert!(valid_background_reference(Some(
            "https://www.rfc-editor.org/rfc/rfc3385"
        )));
        for value in [
            "javascript:alert(1)",
            "file:///private",
            "https://127.0.0.1/",
            "https://www.rfc-editor.org.evil.test/",
            "https://secret@www.rfc-editor.org/",
            "https://www.rfc-editor.org/?token=secret",
            "https://www.rfc-editor.org:8443/",
        ] {
            assert!(!valid_background_reference(Some(value)));
        }
    }
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
