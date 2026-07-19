//! REST API mounted under `/api` by `app::router`. `/events`, `/ping/:token`,
//! `/healthz`, and the SPA static fallback are wired directly in
//! `app::router` rather than here (they aren't `/api/*` routes).

pub mod channels;
pub mod incidents;
pub mod monitors;
pub mod settings;
pub mod sse;
pub mod static_assets;

use std::time::{SystemTime, UNIX_EPOCH};

use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::Router;

use crate::app::AppState;

/// Current unix time in seconds, used for `created_at`/`updated_at` stamps
/// written by API handlers.
pub(crate) fn now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs() as i64
}

/// Maps any `sqlx::Error` to a `500` with the error's `Display` as the body
/// — good enough for a single-operator tool with no untrusted API clients.
pub(crate) fn db_err(e: sqlx::Error) -> (StatusCode, String) {
    (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/monitors", get(monitors::list).post(monitors::create))
        .route("/monitors/test-check", post(monitors::test_check))
        .route(
            "/monitors/:id",
            get(monitors::get_one).put(monitors::update).delete(monitors::delete),
        )
        .route("/monitors/:id/pause", post(monitors::pause))
        .route("/monitors/:id/resume", post(monitors::resume))
        .route("/monitors/:id/check-now", post(monitors::check_now))
        .route("/monitors/:id/stats", get(monitors::stats))
        .route("/monitors/:id/series", get(monitors::series))
        .route("/monitors/:id/bars", get(monitors::bars))
        .route("/monitors/:id/ssl", get(monitors::get_ssl))
        .route("/monitors/:id/domain", get(monitors::get_domain))
        .route("/monitors/:id/heartbeat", get(monitors::get_heartbeat))
        .route("/monitors/:id/refresh-ssl", post(monitors::refresh_ssl))
        .route("/monitors/:id/refresh-domain", post(monitors::refresh_domain))
        .route(
            "/monitors/:id/notifications",
            get(monitors::list_notifications).put(monitors::set_notifications),
        )
        .route("/incidents", get(incidents::list))
        .route("/incidents/:id/acknowledge", post(incidents::acknowledge))
        .route("/channels", get(channels::list).post(channels::create))
        .route(
            "/channels/:id",
            axum::routing::put(channels::update).delete(channels::delete),
        )
        .route("/channels/:id/test", post(channels::test))
        .route(
            "/settings",
            get(settings::get_settings).put(settings::update_settings),
        )
}
