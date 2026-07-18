//! TCP Port / Ping prober.
//!
//! `port` monitors connect to `m.host:m.port` (both required — enforced by
//! `validate_monitor_dto`). `ping` monitors do the same when a port is
//! explicitly configured; with no port they try 443 then 80 sequentially
//! (real ICMP needs elevated privilege the app can't assume it has, so a
//! successful TCP connect on either "always listening" port stands in for
//! reachability — see spec §3's ping note).

use crate::models::{Cause, Monitor, ProbeOutcome};
use std::time::{Duration, Instant};
use tokio::net::TcpStream;

fn config_error(msg: &str) -> ProbeOutcome {
    ProbeOutcome {
        ok: false,
        response_time_ms: None,
        status_code: None,
        error_message: Some(msg.to_string()),
        resolved_ip: None,
        cause: Some(Cause::Connection),
    }
}

/// Attempts a single TCP connect to `host:port` within `timeout_seconds`.
/// Always returns a well-formed `ProbeOutcome` — never panics on network
/// failure.
async fn try_connect(host: &str, port: i64, timeout_seconds: i64) -> ProbeOutcome {
    let start = Instant::now();
    let addr = format!("{host}:{port}");
    match tokio::time::timeout(Duration::from_secs(timeout_seconds as u64), TcpStream::connect(&addr)).await {
        Ok(Ok(stream)) => {
            let elapsed = start.elapsed().as_millis() as i64;
            ProbeOutcome {
                ok: true,
                response_time_ms: Some(elapsed),
                status_code: None,
                error_message: None,
                resolved_ip: stream.peer_addr().ok().map(|a| a.ip().to_string()),
                cause: None,
            }
        }
        Ok(Err(e)) => {
            let elapsed = start.elapsed().as_millis() as i64;
            ProbeOutcome {
                ok: false,
                response_time_ms: Some(elapsed),
                status_code: None,
                error_message: Some(e.to_string()),
                resolved_ip: None,
                cause: Some(Cause::Connection),
            }
        }
        Err(_) => {
            let elapsed = start.elapsed().as_millis() as i64;
            ProbeOutcome {
                ok: false,
                response_time_ms: Some(elapsed),
                status_code: None,
                error_message: Some(format!("timed out connecting to {addr}")),
                resolved_ip: None,
                cause: Some(Cause::Timeout),
            }
        }
    }
}

pub async fn probe(m: &Monitor) -> ProbeOutcome {
    let Some(host) = m.host.as_deref() else {
        return config_error("monitor has no host configured");
    };

    let candidates: Vec<i64> = match (m.r#type.as_str(), m.port) {
        (_, Some(port)) => vec![port],
        ("ping", None) => vec![443, 80],
        // `port` monitors require a port (enforced at the API layer); with
        // none given here there's nothing to connect to.
        _ => return config_error("monitor has no port configured"),
    };

    let mut last = None;
    for port in candidates {
        let outcome = try_connect(host, port, m.timeout_seconds).await;
        if outcome.ok {
            return outcome;
        }
        last = Some(outcome);
    }
    // Unreachable in practice — `candidates` is never empty — but keep the
    // function total rather than unwrapping.
    last.unwrap_or_else(|| config_error("no candidate ports"))
}
