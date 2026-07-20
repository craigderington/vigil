//! Computes the cached `ReportSummary` for a UTC month, entirely from durable
//! tables (incidents + check_aggregates_daily + notification_log + ssl_certs +
//! domain_info) so any past month is reproducible. Mirrors digest.rs's
//! single-pass fleet approach, extended to per-monitor rows + a month window.

use serde::{Deserialize, Serialize};

use crate::app::AppState;
use crate::digest::round2;
use crate::maintenance_windows::{self, resolve};
use crate::models::{DomainInfo, Monitor, SslCert};
use crate::report::{month_bounds, month_label, prior_month};
use crate::uptime::{self, Span};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FleetReport {
    pub uptime_pct: Option<f64>,
    pub uptime_delta: Option<f64>,
    pub incidents: i64,
    pub downtime_seconds: i64,
    pub mttr_seconds: Option<i64>,
    pub longest_outage: Option<LongestOutage>,
    pub monitors_total: i64,
    pub clean_monitors: i64,
    pub ssl_alerts: i64,
    pub domain_alerts: i64,
    pub expiring_30d: i64,
    pub expiring_60d: i64,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LongestOutage { pub monitor: String, pub seconds: i64 }

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MonitorReport {
    pub id: i64, pub name: String, pub r#type: String,
    pub uptime_pct: Option<f64>, pub incidents: i64, pub downtime_seconds: i64,
    pub mttr_seconds: Option<i64>, pub avg_ms: Option<i64>, pub p95_ms: Option<i64>,
    pub end_status: String,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ReportIncident {
    pub monitor_name: String, pub started_at: i64, pub resolved_at: Option<i64>,
    pub duration_seconds: Option<i64>, pub cause: Option<String>,
    pub status_code: Option<i64>, pub error_message: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ExpiryItem {
    pub monitor: String, pub kind: String, pub days_remaining: Option<i64>, pub flag: String,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ReportSummary {
    pub period: String, pub label: String, pub generated_at: i64,
    pub fleet: FleetReport,
    pub cert_outlook: Vec<ExpiryItem>,
    pub monitors: Vec<MonitorReport>,
    pub incidents: Vec<ReportIncident>,
}

fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64
}

/// Fleet uptime % for a month only (no delta, no per-monitor) — used for the
/// prior-month delta. Calls neither `compute` nor itself (no recursion).
pub async fn fleet_uptime_for(state: &AppState, period: &str) -> anyhow::Result<Option<f64>> {
    let (ds, de) = month_bounds(period);
    let windows = maintenance_windows::active_windows(&state.db).await;
    let monitors: Vec<Monitor> = sqlx::query_as("SELECT * FROM monitors").fetch_all(&state.db).await?;
    let (mut total_down, mut total_denom) = (0i64, 0i64);
    for m in &monitors {
        if m.is_paused { continue; }
        if !had_any(state, m.id, period, ds, de).await? { continue; }
        let (down, denom) = monitor_uptime(state, m, ds, de, &windows).await?;
        total_down += down; total_denom += denom;
    }
    Ok(if total_denom > 0 { Some(round2((1.0 - total_down as f64 / total_denom as f64) * 100.0)) } else { None })
}

/// Durable presence test for a month (M1): an aggregate row OR an incident
/// overlapping the month — NEVER raw `checks` (pruned ~30d).
async fn had_any(state: &AppState, id: i64, period: &str, ds: i64, de: i64) -> anyhow::Result<bool> {
    let (m1, m2) = (format!("{period}-01"), format!("{}-01", crate::report::next_month(period)));
    let agg: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM check_aggregates_daily WHERE monitor_id = ? AND day >= ? AND day < ?)",
    ).bind(id).bind(&m1).bind(&m2).fetch_one(&state.db).await?;
    if agg { return Ok(true); }
    let inc: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM incidents WHERE monitor_id = ? AND started_at < ? AND (resolved_at IS NULL OR resolved_at > ?))",
    ).bind(id).bind(de).bind(ds).fetch_one(&state.db).await?;
    Ok(inc)
}

/// Returns `(downtime_seconds, eff_denom)` for one had-data monitor.
async fn monitor_uptime(state: &AppState, m: &Monitor, ds: i64, de: i64, windows: &[crate::models::MaintenanceWindow]) -> anyhow::Result<(i64, i64)> {
    let raw: Vec<(i64, Option<i64>)> = sqlx::query_as(
        "SELECT started_at, resolved_at FROM incidents WHERE monitor_id = ? AND started_at < ? AND (resolved_at IS NULL OR resolved_at > ?)",
    ).bind(m.id).bind(de).bind(ds).fetch_all(&state.db).await?;
    let spans: Vec<Span> = raw.into_iter().map(|(start, end)| Span { start, end }).collect();
    let tags = resolve::parse_tags(m.tags.as_deref().unwrap_or(""));
    let maint = resolve::maintenance_intervals(windows, m.id, &tags, ds, de);
    let u = uptime::compute(&spans, ds, de, true, &maint);
    let eff_denom: i64 = resolve::subtract_intervals((ds, de), &maint).iter().map(|(s, e)| e - s).sum();
    Ok((u.downtime_seconds, eff_denom))
}

pub async fn compute(state: &AppState, period: &str) -> anyhow::Result<ReportSummary> {
    let (ds, de) = month_bounds(period);
    let windows = maintenance_windows::active_windows(&state.db).await;
    let monitors: Vec<Monitor> = sqlx::query_as("SELECT * FROM monitors").fetch_all(&state.db).await?;

    let (mut total_down, mut total_denom, mut reporting, mut clean) = (0i64, 0i64, 0i64, 0i64);
    let mut monitor_rows: Vec<MonitorReport> = Vec::new();

    for m in &monitors {
        let has_data = !m.is_paused && had_any(state, m.id, period, ds, de).await?;
        // incident overlap spans, fetched ONCE (clipped both ends where used).
        let raw: Vec<(i64, Option<i64>)> = sqlx::query_as(
            "SELECT started_at, resolved_at FROM incidents WHERE monitor_id = ? AND started_at < ? AND (resolved_at IS NULL OR resolved_at > ?)",
        ).bind(m.id).bind(de).bind(ds).fetch_all(&state.db).await?;
        let inc_count = raw.len() as i64;
        let mttr = mean_resolved_in_window(&raw, ds, de);
        // Compute uptime ONCE per monitor (single source; no double compute).
        let (uptime_pct, downtime, end_status) = if has_data {
            let tags = resolve::parse_tags(m.tags.as_deref().unwrap_or(""));
            let maint = resolve::maintenance_intervals(&windows, m.id, &tags, ds, de);
            let u = uptime::compute(&to_spans(&raw), ds, de, true, &maint);
            let eff_denom: i64 = resolve::subtract_intervals((ds, de), &maint).iter().map(|(s, e)| e - s).sum();
            total_down += u.downtime_seconds;
            total_denom += eff_denom;
            reporting += 1;
            if u.uptime_pct.is_some() && u.downtime_seconds == 0 { clean += 1; }
            (u.uptime_pct, u.downtime_seconds, end_status_at(&raw, de))
        } else if m.is_paused {
            (None, 0, "paused".to_string())
        } else {
            (None, 0, "no data".to_string())
        };
        monitor_rows.push(MonitorReport {
            id: m.id, name: m.name.clone(), r#type: m.r#type.clone(),
            uptime_pct, incidents: inc_count, downtime_seconds: downtime, mttr_seconds: mttr,
            avg_ms: monthly_avg_ms(state, m.id, period).await?,
            p95_ms: monthly_p95_ms(state, m.id, ds, de).await?,
            end_status,
        });
    }
    // Worst-uptime-first (CLAUDE.md §13.1); None/paused/no-data sort last.
    monitor_rows.sort_by(|a, b| {
        a.uptime_pct.unwrap_or(f64::INFINITY)
            .partial_cmp(&b.uptime_pct.unwrap_or(f64::INFINITY))
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let fleet_uptime = if total_denom > 0 { Some(round2((1.0 - total_down as f64 / total_denom as f64) * 100.0)) } else { None };
    let prior = fleet_uptime_for(state, &prior_month(period)).await?;
    let uptime_delta = match (fleet_uptime, prior) { (Some(c), Some(p)) => Some(round2(c - p)), _ => None };

    // Fleet-wide incident log + longest outage (both-ends clipped)
    let inc_rows: Vec<(String, i64, Option<i64>, Option<String>, Option<i64>, Option<String>)> = sqlx::query_as(
        "SELECT m.name, i.started_at, i.resolved_at, i.cause, i.status_code, i.error_message \
         FROM incidents i JOIN monitors m ON m.id = i.monitor_id \
         WHERE i.started_at < ? AND (i.resolved_at IS NULL OR i.resolved_at > ?) ORDER BY m.name, i.started_at",
    ).bind(de).bind(ds).fetch_all(&state.db).await?;
    let mut incidents = Vec::new();
    let mut longest: Option<LongestOutage> = None;
    let mut all_resolved_durs: Vec<i64> = Vec::new();
    for (name, started_at, resolved_at, cause, status_code, error_message) in inc_rows {
        let dur = clip(started_at, resolved_at, ds, de);
        if longest.as_ref().map(|l| dur > l.seconds).unwrap_or(true) {
            longest = Some(LongestOutage { monitor: name.clone(), seconds: dur });
        }
        if let Some(r) = resolved_at { if r >= ds && r < de { all_resolved_durs.push(r - started_at); } }
        incidents.push(ReportIncident { monitor_name: name, started_at, resolved_at, duration_seconds: Some(dur), cause, status_code, error_message });
    }
    let fleet_mttr = if all_resolved_durs.is_empty() { None } else { Some(all_resolved_durs.iter().sum::<i64>() / all_resolved_durs.len() as i64) };

    // Alert counts (DISTINCT events) + cert outlook
    let ssl_alerts = distinct_alert_count(state, ds, de, "trigger IN ('ssl_expiring','ssl_invalid')").await?;
    let domain_alerts = distinct_alert_count(state, ds, de, "trigger = 'domain_expiring'").await?;
    let (cert_outlook, expiring_30d, expiring_60d) = cert_outlook(state, &monitors).await?;

    Ok(ReportSummary {
        period: period.to_string(), label: month_label(period), generated_at: now(),
        fleet: FleetReport {
            uptime_pct: fleet_uptime, uptime_delta, incidents: incidents.len() as i64,
            downtime_seconds: total_down, mttr_seconds: fleet_mttr, longest_outage: longest,
            monitors_total: reporting, clean_monitors: clean, ssl_alerts, domain_alerts,
            expiring_30d, expiring_60d,
        },
        cert_outlook, monitors: monitor_rows, incidents,
    })
}

fn to_spans(raw: &[(i64, Option<i64>)]) -> Vec<Span> {
    raw.iter().map(|&(start, end)| Span { start, end }).collect()
}
fn clip(started_at: i64, resolved_at: Option<i64>, ds: i64, de: i64) -> i64 {
    (resolved_at.unwrap_or(de).min(de)) - started_at.max(ds)
}
fn mean_resolved_in_window(raw: &[(i64, Option<i64>)], ds: i64, de: i64) -> Option<i64> {
    let durs: Vec<i64> = raw.iter().filter_map(|&(s, r)| r.filter(|&r| r >= ds && r < de).map(|r| r - s)).collect();
    if durs.is_empty() { None } else { Some(durs.iter().sum::<i64>() / durs.len() as i64) }
}
fn end_status_at(raw: &[(i64, Option<i64>)], de: i64) -> String {
    let open_at_end = raw.iter().any(|&(s, r)| s < de && r.map(|r| r >= de).unwrap_or(true));
    if open_at_end { "down".to_string() } else { "up".to_string() }
}

async fn monthly_avg_ms(state: &AppState, id: i64, period: &str) -> anyhow::Result<Option<i64>> {
    let (m1, m2) = (format!("{period}-01"), format!("{}-01", crate::report::next_month(period)));
    let row: Option<(Option<f64>, Option<i64>)> = sqlx::query_as(
        "SELECT SUM(avg_response_ms * up_count), SUM(up_count) FROM check_aggregates_daily \
         WHERE monitor_id = ? AND day >= ? AND day < ? AND avg_response_ms IS NOT NULL",
    ).bind(id).bind(&m1).bind(&m2).fetch_optional(&state.db).await?;
    Ok(match row { Some((Some(w), Some(n))) if n > 0 => Some((w / n as f64).round() as i64), _ => None })
}

async fn monthly_p95_ms(state: &AppState, id: i64, ds: i64, de: i64) -> anyhow::Result<Option<i64>> {
    // Only when the WHOLE month is within retention (else pruned → biased → None).
    let retention_days = crate::settings_store::get(&state.db, "retention.raw_days", "30").await.parse::<i64>().unwrap_or(30);
    if ds < now() - retention_days * 86400 { return Ok(None); }
    let mut v: Vec<i64> = sqlx::query_scalar(
        "SELECT response_time_ms FROM checks WHERE monitor_id = ? AND checked_at >= ? AND checked_at < ? AND response_time_ms IS NOT NULL",
    ).bind(id).bind(ds).bind(de).fetch_all(&state.db).await?;
    if v.is_empty() { return Ok(None); }
    v.sort_unstable();
    let idx = (((v.len() as f64) * 0.95).ceil() as usize).saturating_sub(1).min(v.len() - 1);
    Ok(Some(v[idx]))
}

async fn distinct_alert_count(state: &AppState, ds: i64, de: i64, pred: &str) -> anyhow::Result<i64> {
    let sql = format!(
        "SELECT COUNT(*) FROM (SELECT DISTINCT monitor_id, trigger, sent_at FROM notification_log WHERE sent_at >= ? AND sent_at < ? AND {pred})",
    );
    Ok(sqlx::query_scalar(&sql).bind(ds).bind(de).fetch_one(&state.db).await?)
}

async fn cert_outlook(state: &AppState, monitors: &[Monitor]) -> anyhow::Result<(Vec<ExpiryItem>, i64, i64)> {
    let (mut out, mut e30, mut e60) = (Vec::new(), 0i64, 0i64);
    for m in monitors {
        if m.ssl_check_enabled {
            if let Some(c) = sqlx::query_as::<_, SslCert>("SELECT * FROM ssl_certs WHERE monitor_id = ?").bind(m.id).fetch_optional(&state.db).await? {
                let max_t = serde_json::from_str::<Vec<i64>>(&m.ssl_alert_days).unwrap_or_default().into_iter().max().unwrap_or(0);
                let flag = if c.is_valid == Some(false) { "invalid" } else if c.days_remaining.map(|d| d <= max_t).unwrap_or(false) { "expiring" } else { "ok" };
                tally(&mut e30, &mut e60, c.days_remaining);
                out.push(ExpiryItem { monitor: m.name.clone(), kind: "ssl".into(), days_remaining: c.days_remaining, flag: flag.into() });
            }
        }
        if m.domain_check_enabled {
            if let Some(d) = sqlx::query_as::<_, DomainInfo>("SELECT * FROM domain_info WHERE monitor_id = ?").bind(m.id).fetch_optional(&state.db).await? {
                let max_t = serde_json::from_str::<Vec<i64>>(&m.domain_alert_days).unwrap_or_default().into_iter().max().unwrap_or(0);
                let flag = if d.queryable == Some(false) { "unknown" } else if d.days_remaining.map(|dd| dd <= max_t).unwrap_or(false) { "expiring" } else { "ok" };
                tally(&mut e30, &mut e60, d.days_remaining);
                out.push(ExpiryItem { monitor: m.name.clone(), kind: "domain".into(), days_remaining: d.days_remaining, flag: flag.into() });
            }
        }
    }
    Ok((out, e30, e60))
}
fn tally(e30: &mut i64, e60: &mut i64, days: Option<i64>) {
    if let Some(d) = days { if d <= 30 { *e30 += 1; } if d <= 60 { *e60 += 1; } }
}
