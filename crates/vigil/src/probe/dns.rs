//! DNS prober (hickory-resolver).
//!
//! Resolves `host` for `dns_record_type` (A/AAAA/CNAME/MX/TXT/NS) and, when
//! `dns_expected_value` is set, requires at least one returned record to
//! contain it (case-insensitive substring match). The actual lookup goes
//! through an injectable `resolve` closure (`probe_with`) so tests can
//! exercise the match/outcome logic without touching the network; `probe`
//! wires that closure up to a real, cached hickory-resolver `TokioResolver`.

use crate::models::{Cause, Monitor, ProbeOutcome};
use hickory_resolver::TokioResolver;
use hickory_resolver::config::ResolverConfig;
use hickory_resolver::proto::rr::{RData, RecordType};
use std::future::Future;
use std::str::FromStr;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

fn config_error(msg: &str) -> ProbeOutcome {
    ProbeOutcome {
        ok: false,
        response_time_ms: None,
        status_code: None,
        error_message: Some(msg.to_string()),
        resolved_ip: None,
        cause: Some(Cause::Dns),
    }
}

/// Runs the DNS check for `m`, using `resolve(host, record_type)` to obtain
/// canonical string forms of the matching records. Injectable so tests can
/// avoid the network entirely; `probe` below is the real-resolver wrapper.
///
/// The `resolve` call is bounded by `m.timeout_seconds`, matching
/// `probe::tcp` and `probe::http` — a hung resolver must not hang the check
/// forever.
///
/// - Timeout elapses before `resolve` completes → failed outcome,
///   `Cause::Timeout`, "dns resolve timed out".
/// - `Err` from `resolve` (lookup/network failure, e.g. NXDOMAIN) → failed
///   outcome, `Cause::Dns`, message preserved.
/// - `Ok(records)` empty → failed outcome, `Cause::Dns`, "no records".
/// - `Ok(records)` non-empty:
///   - `dns_expected_value` is `None` → success (any record matches).
///   - `dns_expected_value` is `Some(v)` → success iff at least one record
///     contains `v` as a case-insensitive substring; otherwise failed with
///     `Cause::Dns`.
/// - `resolved_ip` is the first record's string form for A/AAAA record
///   types, `None` for everything else.
pub async fn probe_with<F, Fut>(m: &Monitor, resolve: F) -> ProbeOutcome
where
    F: Fn(String, String) -> Fut,
    Fut: Future<Output = Result<Vec<String>, String>>,
{
    let Some(host) = m.host.clone() else {
        return config_error("monitor has no host configured");
    };
    let Some(record_type) = m.dns_record_type.clone() else {
        return config_error("monitor has no dns_record_type configured");
    };

    let start = Instant::now();
    let timed = tokio::time::timeout(
        Duration::from_secs(m.timeout_seconds as u64),
        resolve(host, record_type.clone()),
    )
    .await;
    let elapsed = start.elapsed().as_millis() as i64;

    let result = match timed {
        Err(_) => {
            return ProbeOutcome {
                ok: false,
                response_time_ms: Some(elapsed),
                status_code: None,
                error_message: Some("dns resolve timed out".to_string()),
                resolved_ip: None,
                cause: Some(Cause::Timeout),
            };
        }
        Ok(result) => result,
    };

    let records = match result {
        Err(e) => {
            return ProbeOutcome {
                ok: false,
                response_time_ms: Some(elapsed),
                status_code: None,
                error_message: Some(e),
                resolved_ip: None,
                cause: Some(Cause::Dns),
            };
        }
        Ok(records) => records,
    };

    if records.is_empty() {
        return ProbeOutcome {
            ok: false,
            response_time_ms: Some(elapsed),
            status_code: None,
            error_message: Some("no records".to_string()),
            resolved_ip: None,
            cause: Some(Cause::Dns),
        };
    }

    let resolved_ip = if matches!(record_type.to_ascii_uppercase().as_str(), "A" | "AAAA") {
        records.first().cloned()
    } else {
        None
    };

    let matched = match &m.dns_expected_value {
        None => true,
        Some(expected) => {
            let expected = expected.to_ascii_lowercase();
            records
                .iter()
                .any(|r| r.to_ascii_lowercase().contains(&expected))
        }
    };

    if matched {
        ProbeOutcome {
            ok: true,
            response_time_ms: Some(elapsed),
            status_code: None,
            error_message: None,
            resolved_ip,
            cause: None,
        }
    } else {
        ProbeOutcome {
            ok: false,
            response_time_ms: Some(elapsed),
            status_code: None,
            error_message: Some("expected value not found".to_string()),
            resolved_ip,
            cause: Some(Cause::Dns),
        }
    }
}

/// The real-resolver entry point used by `probe::run`.
pub async fn probe(m: &Monitor) -> ProbeOutcome {
    probe_with(m, real_resolve).await
}

/// A single lazily-built, process-wide `TokioResolver`, mirroring the
/// cached-client pattern in `probe::http` — building a resolver re-reads
/// and re-parses `/etc/resolv.conf`, which is wasted work to repeat on
/// every probe tick.
fn resolver() -> &'static TokioResolver {
    static RESOLVER: OnceLock<TokioResolver> = OnceLock::new();
    RESOLVER.get_or_init(|| {
        // Prefer the system's real resolv.conf/DNS config; fall back to a
        // sane default (public root config) if that can't be read, e.g. in
        // a minimal sandbox with no /etc/resolv.conf.
        let builder = TokioResolver::builder_tokio().unwrap_or_else(|_| {
            TokioResolver::builder_with_config(
                ResolverConfig::default(),
                hickory_resolver::net::runtime::TokioRuntimeProvider::default(),
            )
        });
        builder.build().unwrap_or_else(|_| {
            TokioResolver::builder_with_config(
                ResolverConfig::default(),
                hickory_resolver::net::runtime::TokioRuntimeProvider::default(),
            )
            .build()
            .expect("default hickory resolver config must build")
        })
    })
}

/// Resolves `host` for `record_type` against the cached `TokioResolver`,
/// returning canonical string forms of the matching records: A/AAAA →
/// address string; CNAME/NS → target name with the trailing root dot
/// trimmed; MX → `"{preference} {exchange}"`; TXT → the concatenated text
/// data decoded as UTF-8 (lossily, since TXT data is arbitrary bytes).
async fn real_resolve(host: String, record_type: String) -> Result<Vec<String>, String> {
    let rt = RecordType::from_str(&record_type.to_ascii_uppercase())
        .map_err(|e| format!("invalid dns_record_type '{record_type}': {e}"))?;

    let lookup = resolver()
        .lookup(host.as_str(), rt)
        .await
        .map_err(|e| e.to_string())?;

    let mut out = Vec::new();
    for record in lookup.answers() {
        match &record.data {
            RData::A(a) => out.push(a.0.to_string()),
            RData::AAAA(aaaa) => out.push(aaaa.0.to_string()),
            RData::CNAME(name) => out.push(name.0.to_string().trim_end_matches('.').to_string()),
            RData::NS(name) => out.push(name.0.to_string().trim_end_matches('.').to_string()),
            RData::MX(mx) => out.push(format!(
                "{} {}",
                mx.preference,
                mx.exchange.to_string().trim_end_matches('.')
            )),
            RData::TXT(txt) => {
                let joined: String = txt
                    .txt_data
                    .iter()
                    .map(|chunk| String::from_utf8_lossy(chunk))
                    .collect();
                out.push(joined);
            }
            _ => {}
        }
    }
    Ok(out)
}
