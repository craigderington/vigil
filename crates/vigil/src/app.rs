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
    pub sched_tx: tokio::sync::mpsc::UnboundedSender<SchedCmd>,
    pub anchor: Arc<crate::anchor::AnchorGate>,
}

/// Commands sent to the scheduler task: recompute a monitor's schedule
/// (created/updated), drop it (deleted/paused), or run it immediately
/// (`check_now`).
#[derive(Clone, Copy, Debug)]
pub enum SchedCmd {
    Upsert(i64),
    Remove(i64),
    CheckNow(i64),
}

/// A router exposing only `/healthz`. Used by the Docker `HEALTHCHECK`
/// (via the `healthcheck` subcommand) and by tests. Later tasks extend this
/// into the full application router.
pub fn router_health_only() -> axum::Router {
    axum::Router::new().route("/healthz", axum::routing::get(|| async { "ok" }))
}
