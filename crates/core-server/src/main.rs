//! Local control plane, event API, reliable audio ingress, and static web host.

mod api;
mod app;
mod audio;
mod dingtalk;
mod explanation;
mod identity;
mod jobs;
mod pairing;
mod projects;
mod readweave;
mod worker;
mod workspace;

use anyhow::{Context, Result};
use app::AppState;
use axum::{
    Router, middleware,
    routing::{get, post},
};
use std::env;
use std::net::SocketAddr;
use std::path::PathBuf;
use tower_http::services::{ServeDir, ServeFile};
use tower_http::trace::TraceLayer;
use tracing::info;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<()> {
    // Structured logs contain IDs and metrics while avoiding transcript and asset contents.
    tracing_subscriber::fmt()
        .json()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    // All mutable user data lives under one configurable local directory.
    let data_dir =
        PathBuf::from(env::var("AIALRA_DATA_DIR").unwrap_or_else(|_| "./data".to_owned()));
    let state = AppState::open(&data_dir).context("initialize application state")?;
    if let Ok(subject) = env::var("AIALRA_LEGACY_OWNER_SUBJECT")
        && !subject.trim().is_empty()
    {
        let imported = projects::assign_legacy_sessions(&state, &subject)
            .map_err(|error| anyhow::anyhow!("assign legacy sessions: {error:?}"))?;
        info!(imported, "legacy session ownership migration checked");
    }
    let recovered_windows =
        audio::recover_audio_assembly(&state).context("recover durable audio assembly")?;
    info!(recovered_windows, "durable audio assembly recovered");

    // Versioned routes keep device and UI clients compatible across incremental releases.
    let api = Router::new()
        .route("/health", get(api::health))
        .route("/runtime/status", get(api::health))
        .route("/workspace", get(workspace::workspace_snapshot))
        .route("/workspace/stream", get(workspace::stream_workspace))
        .route("/workspace/folders", post(workspace::create_folder))
        .route(
            "/workspace/folders/{folder_id}",
            axum::routing::patch(workspace::update_folder),
        )
        .route(
            "/workspace/folders/{folder_id}/archive",
            post(workspace::archive_folder),
        )
        .route("/workspace/trash", get(workspace::list_trash))
        .route(
            "/workspace/trash/{entity_type}/{entity_id}",
            post(workspace::trash_entity),
        )
        .route(
            "/workspace/trash/{entity_type}/{entity_id}/restore",
            post(workspace::restore_entity),
        )
        .route(
            "/workspace/trash/{entity_type}/{entity_id}/purge",
            post(workspace::purge_entity),
        )
        .route(
            "/workspace/preferences/{device_id}",
            axum::routing::patch(workspace::update_preference),
        )
        .route(
            "/projects",
            get(projects::list_projects).post(projects::create_project),
        )
        .route(
            "/projects/{project_id}",
            get(projects::get_project).patch(projects::update_project),
        )
        .route(
            "/projects/{project_id}/placement",
            axum::routing::patch(workspace::update_project_placement),
        )
        .route(
            "/projects/{project_id}/ai-policy",
            get(workspace::get_ai_policy).patch(workspace::update_ai_policy),
        )
        .route(
            "/projects/{project_id}/stream",
            get(projects::stream_project),
        )
        .route(
            "/projects/{project_id}/sessions",
            get(projects::list_project_sessions).post(projects::create_project_session),
        )
        .route(
            "/projects/{project_id}/sessions/{session_id}",
            axum::routing::patch(workspace::update_session_metadata),
        )
        .route(
            "/projects/{project_id}/sessions/{session_id}/summary",
            post(projects::summarize_session),
        )
        .route(
            "/projects/{project_id}/sessions/{session_id}/device-pairing",
            post(pairing::create_pairing_code),
        )
        .route(
            "/projects/{project_id}/readweave",
            get(projects::readweave_status),
        )
        .route(
            "/projects/{project_id}/readweave/targets",
            get(projects::readweave_targets),
        )
        .route(
            "/projects/{project_id}/readweave/preview",
            get(projects::readweave_preview),
        )
        .route(
            "/projects/{project_id}/readweave/reconcile",
            post(projects::reconcile_readweave),
        )
        .route(
            "/projects/{project_id}/sessions/{session_id}/recording/acquire",
            post(projects::acquire_recording),
        )
        .route(
            "/device/projects/{project_id}/sessions/{session_id}/recording/acquire",
            post(projects::acquire_recording),
        )
        .route(
            "/projects/{project_id}/sessions/{session_id}/recording/renew",
            post(projects::renew_recording),
        )
        .route(
            "/device/projects/{project_id}/sessions/{session_id}/recording/renew",
            post(projects::renew_recording),
        )
        .route(
            "/projects/{project_id}/sessions/{session_id}/recording/stop",
            post(projects::stop_recording),
        )
        .route(
            "/device/projects/{project_id}/sessions/{session_id}/recording/stop",
            post(projects::stop_recording),
        )
        .route(
            "/sessions",
            get(api::list_sessions).post(api::create_session),
        )
        .route("/sessions/{session_id}", get(api::get_session))
        .route("/sessions/{session_id}/start", post(api::start_session))
        .route("/sessions/{session_id}/stop", post(api::stop_session))
        .route("/sessions/{session_id}/events", get(api::list_events))
        .route("/sessions/{session_id}/stream", get(api::stream_events))
        .route("/sessions/{session_id}/assets", post(api::upload_asset))
        .route(
            "/sessions/{session_id}/assets/{asset_id}/content",
            get(api::asset_content),
        )
        .route("/sessions/{session_id}/explain", post(api::explain_now))
        .route(
            "/sessions/{session_id}/dingtalk/capabilities",
            get(api::dingtalk_capabilities),
        )
        .route(
            "/sessions/{session_id}/dingtalk/start",
            post(api::start_dingtalk_recording),
        )
        .route(
            "/sessions/{session_id}/dingtalk/stop",
            post(api::stop_dingtalk_recording),
        )
        .route(
            "/sessions/{session_id}/sources/{source_id}/audio",
            get(audio::audio_websocket),
        )
        .layer(middleware::from_fn_with_state(
            state.clone(),
            identity::identity_and_session_scope,
        ));

    tokio::spawn(readweave::run_connector_loop(state.clone()));
    tokio::spawn(audio::run_audio_assembler(state.clone()));

    // Worker endpoints are blocked at the public proxy and require a second application token.
    let public_api = Router::new().route(
        "/device-pairing/exchange",
        post(pairing::exchange_pairing_code),
    );

    let internal = Router::new()
        .route("/workers/heartbeat", post(jobs::worker_heartbeat))
        .route("/jobs/lease", post(jobs::lease_job))
        .route("/jobs/{job_id}/renew", post(jobs::renew_job))
        .route("/jobs/{job_id}/input", get(jobs::job_input))
        .route("/jobs/{job_id}/complete", post(jobs::complete_job))
        .route("/jobs/{job_id}/fail", post(jobs::fail_job));

    // The Rust server serves the compiled React app in packaged mode and returns index.html for client routing.
    let web_dist = PathBuf::from("apps/web/dist");
    let static_files =
        ServeDir::new(&web_dist).fallback(ServeFile::new(web_dist.join("index.html")));
    let app = Router::new()
        .nest("/api/v1", api.merge(public_api))
        .nest("/internal/v1", internal)
        .fallback_service(static_files)
        // Keep request tracing useful without putting session IDs, project IDs,
        // query strings or temporary paths into ordinary application logs.
        .layer(TraceLayer::new_for_http().make_span_with(
            |request: &axum::http::Request<_>| {
                tracing::info_span!("http_request", method = %request.method())
            },
        ))
        .with_state(state);

    // Localhost is the default boundary; LAN access requires an explicit environment change.
    let bind = env::var("AIALRA_BIND").unwrap_or_else(|_| "127.0.0.1:8787".to_owned());
    let address: SocketAddr = bind.parse().context("parse AIALRA_BIND")?;
    let listener = tokio::net::TcpListener::bind(address).await?;
    info!(address = %address, "AIALRA core listening");
    axum::serve(listener, app).await?;
    Ok(())
}
