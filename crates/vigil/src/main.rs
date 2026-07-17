use std::sync::Arc;

use vigil::{anchor::AnchorGate, app, config::Config, db, engine, maintenance, notify, scheduler, secrets, settings_store};

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

/// GET `http://127.0.0.1:8080/healthz` and exit 0 on a 2xx response, 1 otherwise.
/// Used as the Docker `HEALTHCHECK` probe (see the plan's Dockerfile step).
async fn healthcheck() {
    match reqwest::get("http://127.0.0.1:8080/healthz").await {
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
    let (sched_tx, sched_rx) = tokio::sync::mpsc::unbounded_channel();
    let anchors = settings_store::anchors(&pool).await;
    let anchor = Arc::new(AnchorGate::new(anchors, bus.clone()));

    let state = app::AppState {
        db: pool,
        bus,
        transport,
        sched_tx,
        anchor: anchor.clone(),
    };

    let sem = Arc::new(tokio::sync::Semaphore::new(cfg.max_concurrency));
    tokio::spawn(scheduler::run_scheduler(state.clone(), sched_rx, sem));
    tokio::spawn(anchor.run_poller());
    tokio::spawn(engine::run_connectivity_reactor(state.clone()));
    tokio::spawn(maintenance::run(state.clone()));

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
