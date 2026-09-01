//! Authentik identity is accepted only from the trusted reverse-proxy boundary

use crate::app::{ApiError, AppState};
use crate::projects::hash_token;
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
    let pairing_exchange = is_pairing_exchange_path(request.uri().path());
    let deployment = std::env::var("AIALRA_DEPLOYMENT_MODE").unwrap_or_else(|_| "local".to_owned());
    if deployment != "local" {
        let configured_origins = std::env::var("AIALRA_ALLOWED_ORIGINS").unwrap_or_default();
        let origin = request
            .headers()
            .get("origin")
            .and_then(|value| value.to_str().ok());
        if !origin_is_allowed(origin, &configured_origins) {
            return Err(ApiError::forbidden("request origin is not allowed"));
        }
    }
    let bearer = request
        .headers()
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "));
    // Browser WebSockets cannot set an Authorization header, so the browser
    // carries the already-issued, project-scoped recording lease in the
    // WebSocket subprotocol.  The public proxy deliberately clears identity
    // headers on this route for Android; validating the lease here restores a
    // trusted owner without weakening ordinary API authentication.
    let audio_lease_subject = audio_lease_subject(&state, &request)?;
    // Pairing is deliberately the one unauthenticated production entry point:
    // the browser has already authenticated the one-time code, while the new
    // Android device has no Authentik session yet.  The handler still consumes
    // a short-lived, single-use code and returns only a device-scoped token.
    if pairing_exchange && bearer.is_none() {
        request
            .extensions_mut()
            .insert(CurrentUser("pairing-device".to_owned()));
        return Ok(next.run(request).await);
    }
    let subject = if let Some(token) = bearer {
        let credential = state
            .store
            .authenticate_device(&crate::pairing::hash_secret(token))?
            .ok_or_else(|| ApiError::unauthorized("device credential is invalid or expired"))?;
        if !device_scope_allows(
            request.uri().path(),
            &credential.project_id,
            &credential.session_id,
        ) {
            return Err(ApiError::forbidden(
                "device credential is limited to its paired recording session",
            ));
        }
        credential.owner_subject
    } else if let Some(subject) = audio_lease_subject {
        subject
    } else if deployment == "local" {
        "local-user".to_owned()
    } else {
        // In production the browser must arrive through the configured
        // reverse-proxy boundary.  The proxy overwrites both this marker and
        // the Authentik subject, so a direct request cannot self-assert an
        // identity header on the loopback service port.
        let require_proxy_marker = std::env::var("AIALRA_REQUIRE_AUTH_PROXY_MARKER")
            .map(|value| {
                !matches!(
                    value.trim().to_ascii_lowercase().as_str(),
                    "0" | "false" | "no"
                )
            })
            .unwrap_or(true);
        if require_proxy_marker {
            let proxy_marker = request
                .headers()
                .get("x-aialra-auth-proxy")
                .and_then(|value| value.to_str().ok());
            if proxy_marker != Some("1") {
                return Err(ApiError::unauthorized_with_code(
                    "登录状态未生效，请刷新页面后重试",
                    "auth_session_required",
                ));
            }
        }
        request
            .headers()
            .get("x-authentik-uid")
            .and_then(|value| value.to_str().ok())
            .filter(|value| valid_identifier(value))
            .map(str::to_owned)
            .ok_or_else(|| {
                ApiError::unauthorized_with_code(
                    "登录状态未生效，请刷新页面后重试",
                    "auth_identity_required",
                )
            })?
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

fn device_scope_allows(path: &str, project_id: &str, session_id: &str) -> bool {
    let project_recording_prefix =
        format!("/projects/{project_id}/sessions/{session_id}/recording/");
    let device_recording_prefix =
        format!("/device/projects/{project_id}/sessions/{session_id}/recording/");
    let session_audio_prefix = format!("/sessions/{session_id}/sources/");
    path.starts_with(&project_recording_prefix)
        || path.starts_with(&device_recording_prefix)
        || (path.starts_with(&session_audio_prefix) && path.ends_with("/audio"))
}

pub fn valid_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
}

fn session_id_from_path(path: &str) -> Option<&str> {
    // Session-scoped routes exist both as `/sessions/{id}/...` compatibility
    // endpoints and beneath `/projects/{project}/sessions/{id}/...`.  Looking
    // for the named segment instead of assuming it is first keeps the common
    // identity middleware from leaving upload, DingTalk, or WebSocket routes
    // outside the owner check.
    let parts = path.trim_start_matches('/').split('/').collect::<Vec<_>>();
    parts
        .windows(2)
        .find(|pair| pair[0] == "sessions" && pair[1].starts_with("session_"))
        .map(|pair| pair[1])
}

fn audio_lease_subject(
    state: &AppState,
    request: &Request<Body>,
) -> Result<Option<String>, ApiError> {
    let path = request.uri().path();
    if !path.ends_with("/audio") {
        return Ok(None);
    }
    let Some(session_id) = session_id_from_path(path) else {
        return Ok(None);
    };
    let Some(protocols) = request
        .headers()
        .get("sec-websocket-protocol")
        .and_then(|value| value.to_str().ok())
    else {
        return Ok(None);
    };
    let Some(lease_token) = protocols
        .split(',')
        .map(str::trim)
        .find_map(|value| value.strip_prefix("lease."))
        .filter(|value| !value.is_empty())
    else {
        return Ok(None);
    };
    let Some(lease) = state
        .store
        .validate_recording_lease(session_id, &hash_token(lease_token))?
    else {
        return Ok(None);
    };
    Ok(state
        .store
        .get_project(&lease.project_id)?
        .map(|project| project.owner_subject))
}

fn is_pairing_exchange_path(path: &str) -> bool {
    matches!(
        path,
        "/device-pairing/exchange" | "/api/v1/device-pairing/exchange"
    )
}

fn origin_is_allowed(origin: Option<&str>, configured_origins: &str) -> bool {
    let allowed = configured_origins
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>();
    // Native clients do not send an Origin header.  A browser Origin must
    // match an explicit allowlist; an empty production list must not become a
    // permissive wildcard.
    origin.is_none() || origin.is_some_and(|value| allowed.contains(&value))
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
        assert_eq!(
            session_id_from_path("/projects/project_123/sessions/session_123/assets"),
            Some("session_123")
        );
        assert_eq!(session_id_from_path("/projects/project_123"), None);
    }

    #[test]
    fn paired_device_is_restricted_to_one_recording_session() {
        assert!(device_scope_allows(
            "/projects/project_1/sessions/session_1/recording/acquire",
            "project_1",
            "session_1"
        ));
        assert!(device_scope_allows(
            "/sessions/session_1/sources/android/audio",
            "project_1",
            "session_1"
        ));
        assert!(!device_scope_allows(
            "/projects/project_2/sessions/session_1/recording/acquire",
            "project_1",
            "session_1"
        ));
    }

    #[test]
    fn configured_origin_list_rejects_cross_site_browser_requests() {
        assert!(origin_is_allowed(
            Some("https://live.aialra.online"),
            "https://live.aialra.online"
        ));
        assert!(!origin_is_allowed(
            Some("https://attacker.invalid"),
            "https://live.aialra.online"
        ));
        assert!(!origin_is_allowed(Some("https://live.aialra.online"), ""));
        assert!(origin_is_allowed(None, "https://live.aialra.online"));
    }

    #[test]
    fn only_the_device_pairing_exchange_is_public() {
        assert!(is_pairing_exchange_path("/device-pairing/exchange"));
        assert!(is_pairing_exchange_path("/api/v1/device-pairing/exchange"));
        assert!(!is_pairing_exchange_path("/projects/project_1"));
    }

    #[test]
    fn audio_path_and_lease_protocol_are_scoped_to_the_session() {
        assert_eq!(
            session_id_from_path("/sessions/session_123/sources/browser-mic/audio"),
            Some("session_123")
        );
        let protocols = "aialra.audio.v1, lease.token-123";
        assert_eq!(
            protocols
                .split(',')
                .map(str::trim)
                .find_map(|value| value.strip_prefix("lease.")),
            Some("token-123")
        );
    }
}
