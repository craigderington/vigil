//! Axum router construction.

/// A router exposing only `/healthz`. Used by the Docker `HEALTHCHECK`
/// (via the `healthcheck` subcommand) and by tests. Later tasks extend this
/// into the full application router.
pub fn router_health_only() -> axum::Router {
    axum::Router::new().route("/healthz", axum::routing::get(|| async { "ok" }))
}
