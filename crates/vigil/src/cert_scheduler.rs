//! Slow-cadence certificate/domain-expiry scheduler (§3, §4, §6 of the
//! spec): refreshes a monitor's TLS cert data (default every 12h) and
//! domain registration expiry (default every 24h), evaluates tiered
//! fire-once alerts (`ssl_invalid`, `ssl_expiring`, `domain_expiring`,
//! anchor-gated), and emits `Event::CertUpdated` on every refresh so the
//! frontend can re-fetch the monitor's cert/domain card.
//!
//! `refresh_ssl`/`refresh_domain` are `pub` for two callers: this module's
//! own `run` loop, and the on-demand refresh API (`get_ssl`/`get_domain`'s
//! sibling `refresh_ssl(id)`/`refresh_domain(id)` commands).
//!
//! **Fire-once bookkeeping is the crux of this module.** `alerted_days` and
//! `invalid_alerted` (on `ssl_certs`) and `alerted_days` (on `domain_info`)
//! must be written by exactly one code path each — the bookkeeping
//! `UPDATE` at the end of `refresh_ssl`/`refresh_domain` — never folded
//! into the data-only upserts (`ssl::persist_ssl`, `persist_domain_data`
//! below). A full-row upsert that touched those columns on every fast
//! probe would reset the fire-once state on every refresh and re-alert
//! forever.

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tokio::sync::Semaphore;

use crate::app::AppState;
use crate::certcheck::{alerts, domain, ssl};
use crate::events::Event;
use crate::models::{Connectivity, Monitor, Trigger};
use crate::notify::dispatch::send_alert;
use crate::notify::AlertCtx;
use crate::settings_store;

fn now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs() as i64
}

/// Derives the `(host, port)` to run a TLS handshake against for a
/// cert-tracked monitor: `ssl`-type monitors use `host`/`port` directly
/// (mirrors `ssl::ssl_probe`'s own derivation); any other type (an
/// http/keyword monitor with `ssl_check_enabled`) derives it from an
/// `https://` `url`. Returns `None` when no target can be derived (missing
/// host, unparsable url, or a non-https url) — `refresh_ssl` reports that
/// as a well-formed check error rather than panicking.
fn ssl_target(m: &Monitor) -> Option<(String, u16)> {
    if m.r#type == "ssl" {
        let host = m.host.clone().filter(|h| !h.is_empty())?;
        let port = m.port.unwrap_or(443) as u16;
        return Some((host, port));
    }
    let url = m.url.as_deref()?;
    let parsed = reqwest::Url::parse(url).ok()?;
    if parsed.scheme() != "https" {
        return None;
    }
    let host = parsed.host_str()?.to_string();
    let port = parsed.port_or_known_default().unwrap_or(443);
    Some((host, port))
}

/// Derives the host to query for domain-registration expiry: prefers the
/// monitor's explicit `host` (port/ping/dns/ssl monitors), falling back to
/// the host parsed out of `url` (http/keyword monitors).
fn domain_host(m: &Monitor) -> Option<String> {
    if let Some(h) = m.host.clone().filter(|h| !h.is_empty()) {
        return Some(h);
    }
    let url = m.url.as_deref()?;
    reqwest::Url::parse(url).ok()?.host_str().map(str::to_string)
}

/// A monitor's own url/host, for `AlertCtx.url` — the same fallback either
/// alert type wants.
fn monitor_ctx_url(m: &Monitor) -> String {
    m.url.clone().unwrap_or_else(|| m.host.clone().unwrap_or_default())
}

/// Evaluates and (on fire) dispatches the two SSL add-on alerts —
/// `ssl_invalid` and tiered `ssl_expiring` — for one cert-check result.
/// Deliberately takes `old_alerted`/`old_invalid_alerted` as plain
/// parameters and returns the new values rather than reading/writing the
/// DB itself, so fire-once behavior is directly testable: seed the old
/// values, call this twice with the same `days_remaining`, and assert
/// exactly one `notification_log` row. The caller's bookkeeping `UPDATE` is
/// the only place that persists the returned values.
pub async fn evaluate_ssl_alerts(
    state: &AppState,
    m: &Monitor,
    days_remaining: Option<i64>,
    is_valid: bool,
    error: &Option<String>,
    old_alerted: Option<i64>,
    old_invalid_alerted: bool,
) -> (Option<i64>, bool) {
    let checked_at = now();
    let url = monitor_ctx_url(m);

    // ssl_invalid: only for a captured-but-invalid cert (error.is_none() —
    // a transport/handshake failure is a different problem), and never for
    // `ssl`-type monitors (they surface invalidity through their own
    // up/down path via `ssl::ssl_probe` instead).
    let new_invalid_alerted = if !is_valid && error.is_none() && m.r#type != "ssl" {
        if !old_invalid_alerted {
            let ctx = AlertCtx {
                monitor_name: m.name.clone(),
                url: url.clone(),
                ssl_days: days_remaining,
                domain_days: None,
                error: error.clone(),
                checked_at,
            };
            if let Err(e) = send_alert(state, m, Trigger::SslInvalid, &ctx).await {
                tracing::warn!(monitor_id = m.id, error = %e, "ssl_invalid alert dispatch failed");
            }
        }
        true
    } else if is_valid {
        false // invalid -> valid: reset fire-once state for next time
    } else {
        old_invalid_alerted
    };

    // ssl_expiring: tiered, fire-once, most-urgent-first (certcheck::alerts::tier).
    let mut new_alerted = old_alerted;
    if let Some(days) = days_remaining {
        let alert_days: Vec<i64> = serde_json::from_str(&m.ssl_alert_days).unwrap_or_default();
        let dec = alerts::tier(days, &alert_days, old_alerted);
        new_alerted = dec.new_alerted_days;
        if dec.fire {
            let ctx = AlertCtx {
                monitor_name: m.name.clone(),
                url,
                ssl_days: Some(days),
                domain_days: None,
                error: error.clone(),
                checked_at,
            };
            if let Err(e) = send_alert(state, m, Trigger::SslExpiring, &ctx).await {
                tracing::warn!(monitor_id = m.id, error = %e, "ssl_expiring alert dispatch failed");
            }
        }
    }

    (new_alerted, new_invalid_alerted)
}

/// Evaluates and (on fire) dispatches the tiered `domain_expiring` alert.
/// Same shape as `evaluate_ssl_alerts`: takes `old_alerted` as a plain
/// parameter, returns the new value, does no DB I/O of its own besides what
/// `notify::send_alert` performs — the caller's bookkeeping `UPDATE` is the
/// only writer of `domain_info.alerted_days`.
pub async fn evaluate_domain_alerts(
    state: &AppState,
    m: &Monitor,
    days_remaining: Option<i64>,
    old_alerted: Option<i64>,
) -> Option<i64> {
    let Some(days) = days_remaining else {
        return old_alerted;
    };
    let alert_days: Vec<i64> = serde_json::from_str(&m.domain_alert_days).unwrap_or_default();
    let dec = alerts::tier(days, &alert_days, old_alerted);
    if dec.fire {
        let ctx = AlertCtx {
            monitor_name: m.name.clone(),
            url: monitor_ctx_url(m),
            ssl_days: None,
            domain_days: Some(days),
            error: None,
            checked_at: now(),
        };
        if let Err(e) = send_alert(state, m, Trigger::DomainExpiring, &ctx).await {
            tracing::warn!(monitor_id = m.id, error = %e, "domain_expiring alert dispatch failed");
        }
    }
    dec.new_alerted_days
}

/// Refreshes one monitor's SSL certificate data and evaluates its add-on
/// alerts. Steps (in order — the ordering is load-bearing, see the module
/// doc): read the OLD fire-once state, run the TLS check, persist the cert
/// DATA ONLY (`ssl::persist_ssl`, never touches `alerted_days`/
/// `invalid_alerted`), evaluate alerts (anchor-gated), then the single
/// bookkeeping `UPDATE` that both advances `last_checked` and persists the
/// new fire-once state — always, even when the anchor is offline or no
/// alert fired, so the 12h cadence progresses and the due-query stops
/// re-selecting this monitor every tick.
pub async fn refresh_ssl(state: &AppState, monitor_id: i64) -> anyhow::Result<()> {
    let m: Option<Monitor> = sqlx::query_as("SELECT * FROM monitors WHERE id = ?")
        .bind(monitor_id)
        .fetch_optional(&state.db)
        .await?;
    let Some(m) = m else {
        return Ok(());
    };

    let row: Option<(Option<i64>, i64)> = sqlx::query_as(
        "SELECT alerted_days, invalid_alerted FROM ssl_certs WHERE monitor_id = ?",
    )
    .bind(monitor_id)
    .fetch_optional(&state.db)
    .await?;
    let (old_alerted, old_invalid_alerted) = row.map(|(a, inv)| (a, inv != 0)).unwrap_or((None, false));

    let timeout = m.timeout_seconds.max(1) as u64;
    let r = match ssl_target(&m) {
        Some((host, port)) => ssl::check(&host, port, timeout).await,
        None => ssl::SslResult {
            error: Some("no ssl target configured for this monitor".to_string()),
            ..Default::default()
        },
    };

    ssl::persist_ssl(&state.db, monitor_id, &r).await?;

    let (new_alerted, new_invalid_alerted) = if state.anchor.current().await == Connectivity::Offline {
        // Persisting cert data is always fine; only the ALERT dispatch is
        // gated on connectivity. Carry the old fire-once state forward
        // unchanged.
        (old_alerted, old_invalid_alerted)
    } else {
        evaluate_ssl_alerts(state, &m, r.days_remaining, r.is_valid, &r.error, old_alerted, old_invalid_alerted).await
    };

    let checked_at = now();
    sqlx::query(
        "UPDATE ssl_certs SET last_checked = ?, alerted_days = ?, invalid_alerted = ? WHERE monitor_id = ?",
    )
    .bind(checked_at)
    .bind(new_alerted)
    .bind(new_invalid_alerted as i64)
    .bind(monitor_id)
    .execute(&state.db)
    .await?;

    let _ = state.bus.send(Event::CertUpdated { id: monitor_id });
    Ok(())
}

/// Column-scoped, data-only upsert of a monitor's `domain_info` row —
/// registrar/expiry/days_remaining/name_servers/status_codes/queryable/
/// source facts only. Deliberately never touches `last_checked` or
/// `alerted_days`: those are `refresh_domain`'s bookkeeping columns, kept
/// separate for the same fire-once-safety reason as `ssl::persist_ssl`.
async fn persist_domain_data(
    pool: &sqlx::SqlitePool,
    monitor_id: i64,
    r: &domain::DomainResult,
    days_remaining: Option<i64>,
) -> Result<(), sqlx::Error> {
    let name_servers = serde_json::to_string(&r.name_servers).unwrap_or_default();
    let status_codes = serde_json::to_string(&r.status_codes).unwrap_or_default();
    sqlx::query(
        "INSERT INTO domain_info \
         (monitor_id, registrar, expiry_date, days_remaining, name_servers, status_codes, queryable, source) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?) \
         ON CONFLICT(monitor_id) DO UPDATE SET \
           registrar = excluded.registrar, \
           expiry_date = excluded.expiry_date, \
           days_remaining = excluded.days_remaining, \
           name_servers = excluded.name_servers, \
           status_codes = excluded.status_codes, \
           queryable = excluded.queryable, \
           source = excluded.source",
    )
    .bind(monitor_id)
    .bind(&r.registrar)
    .bind(r.expiry_date)
    .bind(days_remaining)
    .bind(name_servers)
    .bind(status_codes)
    .bind(r.queryable as i64)
    .bind(&r.source)
    .execute(pool)
    .await?;
    Ok(())
}

/// Advances `domain_info.last_checked` only — used for a `transient` check
/// result and for a monitor with no derivable host. Inserts a bare row
/// first if none exists yet (so the `UPDATE` has something to touch on the
/// very first check), then always advances the retry clock so the due-query
/// stops re-selecting this monitor every tick (without this, a
/// domain-enabled monitor stuck behind a 429 would be re-queried on every
/// scheduler tick, worsening the rate-limit).
async fn touch_domain_last_checked(pool: &sqlx::SqlitePool, monitor_id: i64, checked_at: i64) -> Result<(), sqlx::Error> {
    sqlx::query("INSERT OR IGNORE INTO domain_info (monitor_id) VALUES (?)")
        .bind(monitor_id)
        .execute(pool)
        .await?;
    sqlx::query("UPDATE domain_info SET last_checked = ? WHERE monitor_id = ?")
        .bind(checked_at)
        .bind(monitor_id)
        .execute(pool)
        .await?;
    Ok(())
}

/// Refreshes one monitor's domain-registration expiry and evaluates its
/// `domain_expiring` alert. A `transient` check result (429/5xx/timeout —
/// see `certcheck::domain`'s module doc) does NOT overwrite any known
/// field or `queryable`; it only advances `last_checked` so the due-query's
/// retry clock progresses instead of hammering the registry every tick.
pub async fn refresh_domain(state: &AppState, monitor_id: i64) -> anyhow::Result<()> {
    let m: Option<Monitor> = sqlx::query_as("SELECT * FROM monitors WHERE id = ?")
        .bind(monitor_id)
        .fetch_optional(&state.db)
        .await?;
    let Some(m) = m else {
        return Ok(());
    };

    let checked_at = now();

    let Some(host) = domain_host(&m) else {
        touch_domain_last_checked(&state.db, monitor_id, checked_at).await?;
        let _ = state.bus.send(Event::CertUpdated { id: monitor_id });
        return Ok(());
    };

    let timeout = m.timeout_seconds.max(1) as u64;
    let r = domain::check(&host, timeout).await;

    if r.transient {
        touch_domain_last_checked(&state.db, monitor_id, checked_at).await?;
        let _ = state.bus.send(Event::CertUpdated { id: monitor_id });
        return Ok(());
    }

    let old_alerted: Option<i64> =
        sqlx::query_scalar::<_, Option<i64>>("SELECT alerted_days FROM domain_info WHERE monitor_id = ?")
            .bind(monitor_id)
            .fetch_optional(&state.db)
            .await?
            .flatten();

    // Floor division so a domain that expired within the last day reports a
    // negative days_remaining rather than a misleading 0 (mirrors
    // `ssl::check`'s `days_remaining` computation).
    let days_remaining = r.expiry_date.map(|exp| (exp - checked_at).div_euclid(86_400));

    persist_domain_data(&state.db, monitor_id, &r, days_remaining).await?;

    let new_alerted = if state.anchor.current().await == Connectivity::Offline {
        old_alerted
    } else {
        evaluate_domain_alerts(state, &m, days_remaining, old_alerted).await
    };

    sqlx::query("UPDATE domain_info SET last_checked = ?, alerted_days = ? WHERE monitor_id = ?")
        .bind(checked_at)
        .bind(new_alerted)
        .bind(monitor_id)
        .execute(&state.db)
        .await?;

    let _ = state.bus.send(Event::CertUpdated { id: monitor_id });
    Ok(())
}

/// Selects and refreshes every monitor whose SSL cert or domain-expiry data
/// is due, bounded by a `Semaphore(cert.concurrency)`. A monitor that has
/// never been checked (no row, or a row with `last_checked IS NULL`) is
/// always due — this is what gives the very first loop iteration its
/// startup catch-up behavior, with no separate special-cased pass needed.
/// Per-monitor errors are logged, not propagated, so one bad target can
/// never stall the whole refresh pass.
async fn refresh_due(state: &AppState) {
    let now = now();
    let ssl_cutoff = now - settings_store::cert_ssl_interval_seconds(&state.db).await;
    let domain_cutoff = now - settings_store::cert_domain_interval_seconds(&state.db).await;
    let concurrency = settings_store::cert_concurrency(&state.db).await.max(1) as usize;
    let sem = Arc::new(Semaphore::new(concurrency));

    let ssl_due: Vec<i64> = sqlx::query_scalar(
        "SELECT m.id FROM monitors m LEFT JOIN ssl_certs c ON c.monitor_id = m.id \
         WHERE (m.ssl_check_enabled = 1 OR m.type = 'ssl') AND m.is_paused = 0 \
         AND (c.last_checked IS NULL OR c.last_checked < ?)",
    )
    .bind(ssl_cutoff)
    .fetch_all(&state.db)
    .await
    .unwrap_or_default();

    let domain_due: Vec<i64> = sqlx::query_scalar(
        "SELECT m.id FROM monitors m LEFT JOIN domain_info d ON d.monitor_id = m.id \
         WHERE m.domain_check_enabled = 1 AND m.is_paused = 0 \
         AND (d.last_checked IS NULL OR d.last_checked < ?)",
    )
    .bind(domain_cutoff)
    .fetch_all(&state.db)
    .await
    .unwrap_or_default();

    let mut handles = Vec::with_capacity(ssl_due.len() + domain_due.len());

    for id in ssl_due {
        let st = state.clone();
        let permit = sem.clone();
        handles.push(tokio::spawn(async move {
            let Ok(_permit) = permit.acquire_owned().await else { return };
            if let Err(e) = refresh_ssl(&st, id).await {
                tracing::warn!(monitor_id = id, error = %e, "refresh_ssl failed");
            }
        }));
    }
    for id in domain_due {
        let st = state.clone();
        let permit = sem.clone();
        handles.push(tokio::spawn(async move {
            let Ok(_permit) = permit.acquire_owned().await else { return };
            if let Err(e) = refresh_domain(&st, id).await {
                tracing::warn!(monitor_id = id, error = %e, "refresh_domain failed");
            }
        }));
    }

    for h in handles {
        let _ = h.await;
    }
}

/// The slow-cadence cert/domain scheduler loop. Runs for the lifetime of
/// the app. Every tick, re-selects due monitors and refreshes them; the
/// very first iteration (before any sleep) is also the startup catch-up,
/// since a never-checked monitor is always due.
pub async fn run(state: AppState) {
    loop {
        let tick = settings_store::cert_tick_seconds(&state.db).await;
        refresh_due(&state).await;
        tokio::time::sleep(Duration::from_secs(tick.max(1) as u64)).await;
    }
}
