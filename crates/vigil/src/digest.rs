//! P4.3 daily digest. Computes yesterday's (UTC) fleet uptime, incidents and
//! upcoming SSL/domain expirations LIVE from `incidents` + `uptime::compute`
//! + maintenance intervals (NOT the aggregate table, which is untimely at
//! fire time and does not exclude maintenance). See §4.5 of the spec.

use serde::Serialize;

use crate::app::AppState;
use crate::maintenance_windows::{self, resolve};
use crate::models::{DomainInfo, Monitor, SslCert};
use crate::rollup;
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
