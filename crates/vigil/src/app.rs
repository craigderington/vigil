//! Axum router construction and shared application state.

use std::sync::Arc;

/// Shared application state, injected into every Tauri/axum handler and
/// background task. Constructed once at startup (or per-test via
/// `tests/common::test_state`).
#[derive(Clone)]
pub struct AppState {
    pub db: sqlx::SqlitePool,
    pub bus: crate::events::Bus,
    pub transport: Arc<dyn crate::notify::Transport>,
    pub http_sender: Arc<dyn crate::notify::HttpSender>,
    pub sched_tx: tokio::sync::mpsc::UnboundedSender<SchedCmd>,
    pub anchor: Arc<crate::anchor::AnchorGate>,
}

/// Commands sent to the scheduler task: recompute a monitor's schedule
/// (created/updated), drop it (deleted/paused), run it immediately
/// (`check_now`), or signal that a worker has finished a check
/// (`Complete`, sent exactly once per `worker::run_check` invocation so the
/// scheduler can clear its in-flight guard and allow the monitor to be
/// scheduled again).
#[derive(Clone, Copy, Debug)]
pub enum SchedCmd {
    Upsert(i64),
    Remove(i64),
    CheckNow(i64),
    Complete(i64),
}

/// A router exposing only `/healthz`. Used by the Docker `HEALTHCHECK`
/// (via the `healthcheck` subcommand) and by tests. Later tasks extend this
/// into the full application router.
pub fn router_health_only() -> axum::Router {
    axum::Router::new().route("/healthz", axum::routing::get(|| async { "ok" }))
}

/// The full application router: `/healthz`, the SSE event stream, the
/// heartbeat receiver, the `/api/*` REST surface, and (as the fallback,
/// for any unmatched GET) the built SPA's static assets.
pub fn router(state: AppState) -> axum::Router {
    axum::Router::new()
        .route("/healthz", axum::routing::get(|| async { "ok" }))
        .route("/events", axum::routing::get(crate::api::sse::sse_handler))
        .route(
            "/ping/:token",
            axum::routing::get(crate::heartbeat::ping).post(crate::heartbeat::ping),
        )
        .nest("/api", crate::api::routes())
        .fallback_service(crate::api::static_assets::service())
        .with_state(state)
}
