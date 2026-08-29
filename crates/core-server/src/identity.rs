//! Authentik identity is accepted only from the trusted reverse-proxy boundary

use crate::app::{ApiError, AppState};
use axum::body::Body;
use axum::extract::{Request, State};
use axum::middleware::Next;
use axum::response::Response;

#[derive(Clone, Debug)]
pub struct CurrentUser(pub String);

pub async fn identity_and_session_scope(
    State(state): State<AppState>,
    mut request: Request<Body>,
    next: Next,
) -> Result<Response, ApiError> {
    let deployment = std::env::var("AIALRA_DEPLOYMENT_MODE").unwrap_or_else(|_| "local".to_owned());
    let subject = if deployment == "local" {
        "local-user".to_owned()
    } else {
        request
            .headers()
            .get("x-authentik-uid")
            .and_then(|value| value.to_str().ok())
            .filter(|value| valid_identifier(value))
            .map(str::to_owned)
            .ok_or_else(|| ApiError::unauthorized("trusted Authentik identity is required"))?
    };

    if let Some(session_id) = session_id_from_path(request.uri().path()) {
        let project = state
            .store
            .project_for_session(session_id)?
            .ok_or_else(|| ApiError::not_found("session not found"))?;
        if project.owner_subject != subject {
            return Err(ApiError::not_found("session not found"));
        }
    }
    request.extensions_mut().insert(CurrentUser(subject));
    Ok(next.run(request).await)
}

pub fn valid_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
}

fn session_id_from_path(path: &str) -> Option<&str> {
    let mut parts = path.trim_start_matches('/').split('/');
    if parts.next()? != "sessions" {
        return None;
    }
    parts.next().filter(|value| value.starts_with("session_"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_rejects_header_injection_characters() {
        assert!(valid_identifier("900347b8a29876b45ca6f75722635ecf"));
        assert!(!valid_identifier("user\r\nx-authentik-uid:attacker"));
    }

    #[test]
    fn session_path_is_scoped_without_matching_project_routes() {
        assert_eq!(
            session_id_from_path("/sessions/session_123/events"),
            Some("session_123")
        );
        assert_eq!(session_id_from_path("/projects/project_123"), None);
    }
}
