use std::sync::Arc;

use vigil::{anchor::AnchorGate, app, cert_scheduler, config::Config, db, digest, engine, heartbeat, maintenance, maintenance_windows, notify, renotify, report, scheduler, secrets, settings_store};

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();

    let mut args = std::env::args();
    let _bin = args.next();
    let subcommand = args.next();

    match subcommand.as_deref() {
        Some("healthcheck") => healthcheck().await,
        Some("serve") | None => serve().await,
        Some(other) => {
            eprintln!("vigil: unknown subcommand `{other}`");
            std::process::exit(2);
        }
    }
}

/// Extracts the port from a `host:port` bind string, e.g. `"0.0.0.0:8080"` ->
/// `"8080"`. Falls back to returning the input unchanged if no `:` is
/// present — this is a best-effort local check, not a strict parser.
fn port_from_bind(bind: &str) -> &str {
    bind.rsplit(':').next().unwrap_or("8080")
}

/// GET `http://127.0.0.1:{port}/healthz` (port derived from `VIGIL_BIND` via
/// `config::Config::from_env`) and exit 0 on a 2xx response, 1 otherwise.
/// Used as the Docker `HEALTHCHECK` probe (see the plan's Dockerfile step).
/// Always probes loopback, which is correct even when the app binds
/// `0.0.0.0` inside the container.
async fn healthcheck() {
    let port = port_from_bind(&Config::from_env().bind).to_string();
    let url = format!("http://127.0.0.1:{port}/healthz");
    match reqwest::get(&url).await {
        Ok(resp) if resp.status().is_success() => std::process::exit(0),
        Ok(resp) => {
            eprintln!("vigil healthcheck: unhealthy status {}", resp.status());
            std::process::exit(1);
        }
        Err(err) => {
            eprintln!("vigil healthcheck: request failed: {err}");
            std::process::exit(1);
        }
    }
}

/// Builds `AppState`, spawns the scheduler / anchor poller / connectivity
/// reactor / maintenance background tasks, and serves the full application
/// router on `cfg.bind`.
async fn serve() {
    let cfg = Config::from_env();

    let pool = match db::connect(&cfg.db_path).await {
        Ok(p) => p,
        Err(err) => {
            eprintln!("vigil: failed to open db {}: {err}", cfg.db_path);
            std::process::exit(1);
        }
    };

    let bus = tokio::sync::broadcast::channel(1024).0;
    let transport = Arc::new(notify::SmtpTransport::new(secrets::read_smtp_password()));
    let http_sender = Arc::new(notify::http::ReqwestHttpSender::new());
    let (sched_tx, sched_rx) = tokio::sync::mpsc::unbounded_channel();
    let anchors = settings_store::anchors(&pool).await;
    let anchor = Arc::new(AnchorGate::new(anchors, bus.clone()));

    let state = app::AppState {
        db: pool,
        bus,
        transport,
        http_sender,
        sched_tx,
        anchor: anchor.clone(),
    };

    let sem = Arc::new(tokio::sync::Semaphore::new(cfg.max_concurrency));
    tokio::spawn(scheduler::run_scheduler(state.clone(), sched_rx, sem));
    tokio::spawn(anchor.run_poller());
    tokio::spawn(engine::run_connectivity_reactor(state.clone()));
    tokio::spawn(maintenance::run(state.clone()));
    tokio::spawn(cert_scheduler::run(state.clone()));
    tokio::spawn(heartbeat::run_reaper(state.clone()));
    tokio::spawn(maintenance_windows::run(state.clone()));
    tokio::spawn(renotify::run(state.clone()));
    tokio::spawn(digest::run(state.clone()));
    tokio::spawn(report::scheduler::run(state.clone()));
    // One-shot rollup catch-up at startup, so a period of downtime doesn't
    // leave gaps in the 90-day uptime bars until the next nightly pass.
    // Spawned rather than awaited so it never delays serving traffic.
    tokio::spawn(startup_rollup_catch_up(state.clone()));

    let listener = match tokio::net::TcpListener::bind(&cfg.bind).await {
        Ok(l) => l,
        Err(err) => {
            eprintln!("vigil: failed to bind {}: {err}", cfg.bind);
            std::process::exit(1);
        }
    };
    tracing::info!(bind = %cfg.bind, "vigil listening");
    if let Err(err) = axum::serve(listener, app::router(state)).await {
        eprintln!("vigil: server error: {err}");
        std::process::exit(1);
    }
}

/// One-shot rollup catch-up run at boot (see `serve`). Errors are logged,
/// never fatal — the nightly `maintenance::run` pass will retry.
async fn startup_rollup_catch_up(state: app::AppState) {
    let retention_days = settings_store::retention_days(&state.db).await;
    match vigil::rollup::rollup_catch_up(&state.db, retention_days).await {
        Ok(()) => tracing::info!("startup rollup catch-up complete"),
        Err(error) => tracing::error!(%error, "startup rollup catch-up failed"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn port_from_bind_extracts_port() {
        assert_eq!(port_from_bind("0.0.0.0:8080"), "8080");
        assert_eq!(port_from_bind("127.0.0.1:9090"), "9090");
        assert_eq!(port_from_bind("garbage"), "garbage");
    }
}
