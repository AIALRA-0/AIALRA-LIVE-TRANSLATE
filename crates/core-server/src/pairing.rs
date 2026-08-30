use crate::app::{ApiError, AppState};
use crate::identity::{CurrentUser, valid_identifier};
use crate::projects::owned_project;
use aialra_event_store::DevicePairingCodeRecord;
use axum::Json;
use axum::extract::{Extension, Path, State};
use base64::Engine;
use chrono::{Duration, Utc};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use uuid::Uuid;

#[derive(Debug, Deserialize)]
pub struct ExchangeRequest {
    code: String,
    device_id: String,
}

pub async fn create_pairing_code(
    State(state): State<AppState>,
    Extension(user): Extension<CurrentUser>,
    Path((project_id, session_id)): Path<(String, String)>,
) -> Result<Json<Value>, ApiError> {
    owned_project(&state, &user.0, &project_id)?;
    let project = state
        .store
        .project_for_session(&session_id)?
        .ok_or_else(|| ApiError::not_found("session not found"))?;
    if project.id != project_id || project.owner_subject != user.0 {
        return Err(ApiError::not_found("session not found"));
    }
    let compact = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(Uuid::now_v7().as_bytes())
        .chars()
        .take(12)
        .collect::<String>()
        .to_ascii_uppercase();
    let code = format!("{}-{}-{}", &compact[0..4], &compact[4..8], &compact[8..12]);
    let created_at = Utc::now();
    let expires_at = created_at + Duration::minutes(5);
    state
        .store
        .create_device_pairing_code(&DevicePairingCodeRecord {
            code_hash: hash_secret(&normalize_code(&code)),
            owner_subject: user.0,
            project_id: project_id.clone(),
            session_id: session_id.clone(),
            expires_at,
            created_at,
        })?;
    state.record_project_update(
        &project_id,
        Some(&session_id),
        "device.pairing.created",
        json!({"expires_at": expires_at}),
    )?;
    Ok(Json(json!({"code": code, "expires_at": expires_at})))
}

pub async fn exchange_pairing_code(
    State(state): State<AppState>,
    Json(request): Json<ExchangeRequest>,
) -> Result<Json<Value>, ApiError> {
    if !valid_identifier(&request.device_id) || request.device_id.len() < 8 {
        return Err(ApiError::bad_request("invalid device identifier"));
    }
    let normalized = normalize_code(&request.code);
    if normalized.len() != 12 {
        return Err(ApiError::bad_request("invalid pairing code"));
    }
    let credential_token = random_token();
    let expires_at = Utc::now() + Duration::days(30);
    let credential = state
        .store
        .exchange_device_pairing_code(
            &hash_secret(&normalized),
            &hash_secret(&credential_token),
            &request.device_id,
            expires_at,
        )?
        .ok_or_else(|| {
            ApiError::unauthorized("pairing code is invalid, expired, or already used")
        })?;
    Ok(Json(json!({
        "device_token": credential_token,
        "project_id": credential.project_id,
        "session_id": credential.session_id,
        "expires_at": credential.expires_at,
    })))
}

pub fn hash_secret(value: &str) -> String {
    hex::encode(Sha256::digest(value.as_bytes()))
}

fn normalize_code(value: &str) -> String {
    value
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .collect::<String>()
        .to_ascii_uppercase()
}

fn random_token() -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(Uuid::now_v7().as_bytes())
        + &base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(Uuid::now_v7().as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pairing_code_normalization_is_stable() {
        assert_eq!(normalize_code("abcd-Efgh-1234"), "ABCDEFGH1234");
    }
}
