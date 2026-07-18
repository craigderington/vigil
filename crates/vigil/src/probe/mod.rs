pub mod dns;
pub mod http;
pub mod tcp;

use crate::models::{Monitor, ProbeOutcome};

/// Dispatches a probe to the prober matching `m.r#type`. `http` and
/// `keyword` monitors share the HTTP(S) prober (keyword-matching happens
/// inside it, per §3); `port` and `ping` share the TCP prober; `dns` gets
/// its own resolver-based prober. Unknown/future types fall back to the
/// HTTP prober rather than panicking.
pub async fn run(m: &Monitor) -> ProbeOutcome {
    match m.r#type.as_str() {
        "http" | "keyword" => http::probe(m).await,
        "port" | "ping" => tcp::probe(m).await,
        "dns" => dns::probe(m).await,
        _ => http::probe(m).await,
    }
}
