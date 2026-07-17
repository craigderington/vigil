use vigil::{app, config::Config};

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

/// Bind `cfg.bind` and serve the router. For now this is `router_health_only()`;
/// later tasks replace it with the full application router.
async fn serve() {
    let cfg = Config::from_env();
    let listener = match tokio::net::TcpListener::bind(&cfg.bind).await {
        Ok(l) => l,
        Err(err) => {
            eprintln!("vigil: failed to bind {}: {err}", cfg.bind);
            std::process::exit(1);
        }
    };
    tracing::info!(bind = %cfg.bind, "vigil listening");
    if let Err(err) = axum::serve(listener, app::router_health_only()).await {
        eprintln!("vigil: server error: {err}");
        std::process::exit(1);
    }
}
