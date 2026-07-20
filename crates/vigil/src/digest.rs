//! P4.3 daily digest. Computes yesterday's (UTC) fleet uptime, incidents and
//! upcoming SSL/domain expirations LIVE from `incidents` + `uptime::compute`
//! + maintenance intervals (NOT the aggregate table, which is untimely at
//! fire time and does not exclude maintenance). See §4.5 of the spec.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::Serialize;

use crate::app::AppState;
use crate::maintenance_windows::{self, resolve};
use crate::models::{DomainInfo, Monitor, SslCert};
use crate::notify::dispatch;
use crate::rollup;
use crate::settings_store;
use crate::uptime::{self, Span};

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct FleetSummary {
    pub uptime_pct: Option<f64>,
    pub monitors_total: i64,
    pub clean_monitors: i64,
    pub incidents: i64,
    pub downtime_seconds: i64,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct DigestIncident {
    pub monitor_name: String,
    pub started_at: i64,
    pub resolved_at: Option<i64>,
    pub duration_seconds: Option<i64>,
    pub cause: Option<String>,
    pub status_code: Option<i64>,
    pub error_message: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct DigestDown {
    pub monitor_name: String,
    pub since: i64,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct DigestExpiration {
    pub monitor_name: String,
    pub kind: String, // "ssl" | "domain"
    pub days_remaining: Option<i64>,
    pub flag: String, // "expiring" | "invalid" | "unknown"
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct DigestSummary {
    pub day: String,
    pub fleet: FleetSummary,
    pub incidents: Vec<DigestIncident>,
    pub currently_down: Vec<DigestDown>,
    pub expirations: Vec<DigestExpiration>,
}

fn round2(x: f64) -> f64 {
    (x * 100.0).round() / 100.0
}

/// Build the digest for a completed UTC `day` ("YYYY-MM-DD").
pub async fn build(state: &AppState, day: &str) -> anyhow::Result<DigestSummary> {
    let (ds, de) = rollup::day_bounds(day);
    let windows = maintenance_windows::active_windows(&state.db).await;

    let monitors: Vec<Monitor> = sqlx::query_as("SELECT * FROM monitors").fetch_all(&state.db).await?;
    let monitors_total = monitors.len() as i64;

    let mut total_down = 0i64;
    let mut total_denom = 0i64;
    let mut clean = 0i64;

    for m in &monitors {
        if m.is_paused {
            continue;
        }
        let raw: Vec<(i64, Option<i64>)> = sqlx::query_as(
            "SELECT started_at, resolved_at FROM incidents \
             WHERE monitor_id = ? AND started_at < ? AND (resolved_at IS NULL OR resolved_at > ?)",
        )
        .bind(m.id)
        .bind(de)
        .bind(ds)
        .fetch_all(&state.db)
        .await?;
        let spans: Vec<Span> = raw.into_iter().map(|(start, end)| Span { start, end }).collect();

        let has_checks: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM checks WHERE monitor_id = ? AND checked_at >= ? AND checked_at < ?)",
        )
        .bind(m.id)
        .bind(ds)
        .bind(de)
        .fetch_one(&state.db)
        .await?;
        let is_heartbeat = m.r#type == "heartbeat";
        let armed = m.last_ping_at.is_some();
        let had_any = has_checks || (is_heartbeat && armed);
        if !had_any {
            continue; // exclude no-data monitors from the fleet weighting
        }

        let tags = resolve::parse_tags(m.tags.as_deref().unwrap_or(""));
        let maint = resolve::maintenance_intervals(&windows, m.id, &tags, ds, de);
        let u = uptime::compute(&spans, ds, de, had_any, &maint);
        let eff_denom: i64 = resolve::subtract_intervals((ds, de), &maint)
            .iter()
            .map(|(s, e)| e - s)
            .sum();
        total_down += u.downtime_seconds;
        total_denom += eff_denom;
        if u.downtime_seconds == 0 {
            clean += 1;
        }
    }

    let fleet_uptime = if total_denom > 0 {
        Some(round2((1.0 - total_down as f64 / total_denom as f64) * 100.0))
    } else {
        None
    };

    // Incidents overlapping the day (started_at < de AND (unresolved OR resolved_at > ds)).
    let inc_rows: Vec<(i64, Option<i64>, Option<String>, Option<i64>, Option<String>, String)> = sqlx::query_as(
        "SELECT i.started_at, i.resolved_at, i.cause, i.status_code, i.error_message, m.name \
         FROM incidents i JOIN monitors m ON m.id = i.monitor_id \
         WHERE i.started_at < ? AND (i.resolved_at IS NULL OR i.resolved_at > ?) \
         ORDER BY i.started_at",
    )
    .bind(de)
    .bind(ds)
    .fetch_all(&state.db)
    .await?;
    let incidents: Vec<DigestIncident> = inc_rows
        .into_iter()
        .map(|(started_at, resolved_at, cause, status_code, error_message, name)| DigestIncident {
            monitor_name: name,
            started_at,
            resolved_at,
            duration_seconds: resolved_at.map(|r| r - started_at),
            cause,
            status_code,
            error_message,
        })
        .collect();

    // Currently down at send time.
    let down_rows: Vec<(String, i64)> = sqlx::query_as(
        "SELECT m.name, i.started_at FROM incidents i JOIN monitors m ON m.id = i.monitor_id \
         WHERE i.resolved_at IS NULL ORDER BY i.started_at",
    )
    .fetch_all(&state.db)
    .await?;
    let currently_down: Vec<DigestDown> = down_rows
        .into_iter()
        .map(|(monitor_name, since)| DigestDown { monitor_name, since })
        .collect();

    // Expirations (inside warning window OR invalid/unqueryable).
    let mut expirations = Vec::new();
    for m in &monitors {
        if m.ssl_check_enabled {
            let cert: Option<SslCert> = sqlx::query_as("SELECT * FROM ssl_certs WHERE monitor_id = ?")
                .bind(m.id)
                .fetch_optional(&state.db)
                .await?;
            if let Some(c) = cert {
                let max_t = serde_json::from_str::<Vec<i64>>(&m.ssl_alert_days)
                    .unwrap_or_default()
                    .into_iter()
                    .max()
                    .unwrap_or(0);
                let invalid = c.is_valid == Some(false);
                let expiring = c.days_remaining.map(|d| d <= max_t).unwrap_or(false);
                if invalid || expiring {
                    expirations.push(DigestExpiration {
                        monitor_name: m.name.clone(),
                        kind: "ssl".to_string(),
                        days_remaining: c.days_remaining,
                        flag: if invalid { "invalid" } else { "expiring" }.to_string(),
                    });
                }
            }
        }
        if m.domain_check_enabled {
            let dom: Option<DomainInfo> = sqlx::query_as("SELECT * FROM domain_info WHERE monitor_id = ?")
                .bind(m.id)
                .fetch_optional(&state.db)
                .await?;
            if let Some(d) = dom {
                let max_t = serde_json::from_str::<Vec<i64>>(&m.domain_alert_days)
                    .unwrap_or_default()
                    .into_iter()
                    .max()
                    .unwrap_or(0);
                let unknown = d.queryable == Some(false);
                let expiring = d.days_remaining.map(|dd| dd <= max_t).unwrap_or(false);
                if unknown || expiring {
                    expirations.push(DigestExpiration {
                        monitor_name: m.name.clone(),
                        kind: "domain".to_string(),
                        days_remaining: d.days_remaining,
                        flag: if unknown { "unknown" } else { "expiring" }.to_string(),
                    });
                }
            }
        }
    }

    Ok(DigestSummary {
        day: day.to_string(),
        fleet: FleetSummary {
            uptime_pct: fleet_uptime,
            monitors_total,
            clean_monitors: clean,
            incidents: incidents.len() as i64,
            downtime_seconds: total_down,
        },
        incidents,
        currently_down,
        expirations,
    })
}

fn now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs() as i64
}

#[derive(Debug, PartialEq)]
pub enum SendOutcome {
    Delivered,
    NothingToSend,
    AllFailed,
}

/// "HH:MM" (UTC) → seconds into the day. Falls back to 08:00 on any parse error.
pub fn parse_digest_time(s: &str) -> i64 {
    let parts: Vec<&str> = s.split(':').collect();
    if parts.len() == 2 {
        if let (Ok(h), Ok(m)) = (parts[0].parse::<i64>(), parts[1].parse::<i64>()) {
            if (0..24).contains(&h) && (0..60).contains(&m) {
                return h * 3600 + m * 60;
            }
        }
    }
    tracing::warn!(input = %s, "invalid digest_time; falling back to 08:00");
    8 * 3600
}

/// Pure scheduler decision: fire iff now has passed today's fire instant and
/// we have not already sent for `today` (lexicographic "YYYY-MM-DD" compare).
pub fn should_send(now_ts: i64, today: &str, last_sent_day: &str, fire_offset: i64) -> bool {
    let (today_start, _) = rollup::day_bounds(today);
    now_ts >= today_start + fire_offset && last_sent_day < today
}

async fn log_digest(state: &AppState, channel_id: Option<i64>, success: bool, error: Option<&str>) -> anyhow::Result<()> {
    sqlx::query(
        "INSERT INTO notification_log (monitor_id, channel_id, incident_id, trigger, sent_at, success, error) \
         VALUES (NULL, ?, NULL, 'digest', ?, ?, ?)",
    )
    .bind(channel_id)
    .bind(now())
    .bind(success)
    .bind(error)
    .execute(&state.db)
    .await?;
    Ok(())
}

/// Render a plaintext digest email. UTC throughout.
fn render_digest(s: &DigestSummary) -> (String, String) {
    let up = s.fleet.uptime_pct.map(|p| format!("{p:.2}%")).unwrap_or_else(|| "n/a".to_string());
    let subject = format!("Vigil daily digest — {} — {} uptime", s.day, up);
    let mut body = String::new();
    body.push_str(&format!("Vigil daily digest for {} (UTC)\n\n", s.day));
    body.push_str(&format!(
        "Fleet uptime: {up}\nMonitors: {} ({} clean)\nIncidents: {}\nTotal downtime: {}s\n\n",
        s.fleet.monitors_total, s.fleet.clean_monitors, s.fleet.incidents, s.fleet.downtime_seconds
    ));
    if s.incidents.is_empty() {
        body.push_str("No incidents.\n");
    } else {
        body.push_str("Incidents:\n");
        for i in &s.incidents {
            let dur = i.duration_seconds.map(|d| format!("{d}s")).unwrap_or_else(|| "ongoing".to_string());
            body.push_str(&format!(
                "  - {} | started {} | {} | {}\n",
                i.monitor_name, i.started_at, dur, i.cause.as_deref().unwrap_or("-")
            ));
        }
    }
    if !s.currently_down.is_empty() {
        body.push_str("\nCurrently down:\n");
        for d in &s.currently_down {
            body.push_str(&format!("  - {} (since {})\n", d.monitor_name, d.since));
        }
    }
    if !s.expirations.is_empty() {
        body.push_str("\nUpcoming expirations:\n");
        for e in &s.expirations {
            let days = e.days_remaining.map(|d| format!("{d}d")).unwrap_or_else(|| "unknown".to_string());
            body.push_str(&format!("  - {} {} [{}] {}\n", e.monitor_name, e.kind, e.flag, days));
        }
    }
    (subject, body)
}

/// Send the digest to every active email channel in `notify.digest_recipients`.
pub async fn send(state: &AppState, summary: &DigestSummary) -> SendOutcome {
    let ids = settings_store::digest_recipients(&state.db).await;
    let mut channels: Vec<(i64, String)> = Vec::new();
    for id in &ids {
        let cfg: Option<String> = sqlx::query_scalar(
            "SELECT config FROM notification_channels WHERE id = ? AND type = 'email' AND is_active = 1",
        )
        .bind(id)
        .fetch_optional(&state.db)
        .await
        .ok()
        .flatten();
        if let Some(cfg) = cfg {
            channels.push((*id, cfg));
        }
    }

    if channels.is_empty() {
        let _ = log_digest(state, None, false, Some("no deliverable email recipients")).await;
        tracing::warn!("digest enabled but no deliverable email recipients");
        return SendOutcome::NothingToSend;
    }

    let (subject, body) = render_digest(summary);
    let mut any_ok = false;
    for (id, cfg) in channels {
        let r = dispatch::send_email_via_channel(state.transport.as_ref(), &cfg, &subject, &body, None).await;
        let (ok, err) = match &r {
            Ok(()) => (true, None),
            Err(e) => (false, Some(e.to_string())),
        };
        any_ok |= ok;
        let _ = log_digest(state, Some(id), ok, err.as_deref()).await;
    }
    if any_ok {
        SendOutcome::Delivered
    } else {
        SendOutcome::AllFailed
    }
}

/// Seed the once-per-day marker to today on a brand-new instance (absent
/// marker), so a fresh install does not fire for a day it wasn't monitoring.
pub async fn seed_marker_if_absent(state: &AppState) -> anyhow::Result<()> {
    let existing: Option<String> =
        sqlx::query_scalar("SELECT value FROM settings WHERE key = 'notify.digest_last_sent_day'")
            .fetch_optional(&state.db)
            .await?;
    if existing.is_none() {
        settings_store::set(&state.db, "notify.digest_last_sent_day", &rollup::day_str(now())).await?;
    }
    Ok(())
}

/// One scheduler evaluation: if due, build yesterday's digest, send it, and
/// advance the marker ONLY on a delivered / nothing-to-send outcome (a total
/// send failure leaves the marker so the next tick retries within the day).
pub async fn tick_once(state: &AppState) -> anyhow::Result<()> {
    let now_ts = now();
    let today = rollup::day_str(now_ts);
    let last = settings_store::get(&state.db, "notify.digest_last_sent_day", "").await;
    let offset = parse_digest_time(&settings_store::digest_time(&state.db).await);
    if !should_send(now_ts, &today, &last, offset) {
        return Ok(());
    }
    let yesterday = rollup::day_str(now_ts - 86_400);
    let summary = build(state, &yesterday).await?;
    match send(state, &summary).await {
        SendOutcome::Delivered | SendOutcome::NothingToSend => {
            settings_store::set(&state.db, "notify.digest_last_sent_day", &today).await?;
        }
        SendOutcome::AllFailed => {
            tracing::warn!("digest send failed for all recipients; will retry next tick");
        }
    }
    Ok(())
}

/// The digest scheduler loop.
pub async fn run(state: AppState) {
    if let Err(error) = seed_marker_if_absent(&state).await {
        tracing::error!(%error, "digest marker seed failed");
    }
    loop {
        let tick = settings_store::digest_tick_seconds(&state.db).await;
        if settings_store::digest_enabled(&state.db).await {
            if let Err(error) = tick_once(&state).await {
                tracing::error!(%error, "digest tick failed");
            }
        }
        tokio::time::sleep(Duration::from_secs(tick.max(1) as u64)).await;
    }
}
