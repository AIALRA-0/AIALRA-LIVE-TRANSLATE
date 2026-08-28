//! Local control plane, event API, reliable audio ingress, and static web host.

mod api;
mod app;
mod audio;
mod dingtalk;
mod explanation;
mod jobs;
mod worker;

use anyhow::{Context, Result};
use app::AppState;
use axum::{
    Router,
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

    // Versioned routes keep device and UI clients compatible across incremental releases.
    let api = Router::new()
        .route("/health", get(api::health))
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
        );

    // Worker endpoints are blocked at the public proxy and require a second application token.
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
        ServeDir::new(&web_dist).not_found_service(ServeFile::new(web_dist.join("index.html")));
    let app = Router::new()
        .nest("/api/v1", api)
        .nest("/internal/v1", internal)
        .fallback_service(static_files)
        .layer(TraceLayer::new_for_http())
        .with_state(state);

    // Localhost is the default boundary; LAN access requires an explicit environment change.
    let bind = env::var("AIALRA_BIND").unwrap_or_else(|_| "127.0.0.1:8787".to_owned());
    let address: SocketAddr = bind.parse().context("parse AIALRA_BIND")?;
    let listener = tokio::net::TcpListener::bind(address).await?;
    info!(address = %address, "AIALRA core listening");
    axum::serve(listener, app).await?;
    Ok(())
}
