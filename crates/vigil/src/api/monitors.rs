//! `/api/monitors*` handlers: CRUD, pause/resume, check-now, the
//! non-persisting test-check probe, and the stats rollup used by the
//! detail-panel "24h/7d" tiles.

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};

use super::{db_err, now};
use crate::app::{AppState, SchedCmd};
use crate::models::{CreateMonitorDto, DomainInfo, Monitor, ProbeOutcome, SslCert, UpdateMonitorDto};
use crate::{probe, uptime};

type ApiResult<T> = Result<Json<T>, (StatusCode, String)>;

async fn fetch_monitor(pool: &sqlx::SqlitePool, id: i64) -> Result<Option<Monitor>, sqlx::Error> {
    sqlx::query_as::<_, Monitor>("SELECT * FROM monitors WHERE id = ?")
        .bind(id)
        .fetch_optional(pool)
        .await
}

fn not_found() -> (StatusCode, String) {
    (StatusCode::NOT_FOUND, "monitor not found".to_string())
}

/// Per-type required-field validation (spec §5). `r#type` picks which
/// fields are mandatory; everything else is an already-merged candidate
/// value (DTO override applied over the existing row, for `update`). The
/// interval-floor check is separate and unconditional, so it isn't covered
/// here. Returns `Err(message)` suitable for a 422 response.
///
/// `ssl_check_enabled` (P3 §6): the SSL add-on only makes sense where a TLS
/// handshake target exists — `http`/`keyword` monitors with an `https://`
/// url, or the dedicated `ssl` type (which requires `host`). Allowing it on
/// `port`/`ping`/`dns` would let the cert scheduler select a monitor with no
/// TLS target, producing a false "errored" cert row, so it's rejected here.
#[allow(clippy::too_many_arguments)]
fn validate_monitor_dto(
    r#type: &str,
    url: &Option<String>,
    host: &Option<String>,
    port: &Option<i64>,
    keyword: &Option<String>,
    keyword_mode: &Option<String>,
    dns_record_type: &Option<String>,
    ssl_check_enabled: bool,
) -> Result<(), String> {
    fn blank(s: &Option<String>) -> bool {
        s.as_deref().map(str::trim).unwrap_or("").is_empty()
    }

    match r#type {
        "keyword" => {
            if blank(url) {
                return Err("url is required".to_string());
            }
            if blank(keyword) {
                return Err("keyword is required".to_string());
            }
            if blank(keyword_mode) {
                return Err("keyword_mode is required".to_string());
            }
        }
        "port" => {
            if blank(host) {
                return Err("host is required".to_string());
            }
            if port.is_none() {
                return Err("port is required".to_string());
            }
        }
        "ping" => {
            if blank(host) {
                return Err("host is required".to_string());
            }
        }
        "dns" => {
            if blank(host) {
                return Err("host is required".to_string());
            }
            if blank(dns_record_type) {
                return Err("dns_record_type is required".to_string());
            }
        }
        "ssl" => {
            if blank(host) {
                return Err("host is required".to_string());
            }
        }
        // "http" and anything else default to the original P1 behavior: a
        // url is required.
        _ => {
            if blank(url) {
                return Err("url is required".to_string());
            }
        }
    }

    if ssl_check_enabled {
        match r#type {
            "http" | "keyword" => {
                if !url.as_deref().unwrap_or("").starts_with("https://") {
                    return Err("ssl_check_enabled requires an https:// url".to_string());
                }
            }
            "ssl" => {} // host already required above
            _ => {
                return Err(
                    "ssl_check_enabled is only supported on http/keyword (https://) or ssl monitors"
                        .to_string(),
                );
            }
        }
    }
    Ok(())
}

pub async fn list(State(state): State<AppState>) -> ApiResult<Vec<Monitor>> {
    let rows = sqlx::query_as::<_, Monitor>("SELECT * FROM monitors ORDER BY sort_order, id")
        .fetch_all(&state.db)
        .await
        .map_err(db_err)?;
    Ok(Json(rows))
}

pub async fn get_one(State(state): State<AppState>, Path(id): Path<i64>) -> ApiResult<Monitor> {
    let row = fetch_monitor(&state.db, id).await.map_err(db_err)?;
    row.map(Json).ok_or_else(not_found)
}

pub async fn create(
    State(state): State<AppState>,
    Json(dto): Json<CreateMonitorDto>,
) -> ApiResult<Monitor> {
    if let Err(msg) = validate_monitor_dto(
        &dto.r#type,
        &dto.url,
        &dto.host,
        &dto.port,
        &dto.keyword,
        &dto.keyword_mode,
        &dto.dns_record_type,
        dto.ssl_check_enabled,
    ) {
        return Err((StatusCode::UNPROCESSABLE_ENTITY, msg));
    }
    if dto.interval_seconds < 15 {
        return Err((StatusCode::UNPROCESSABLE_ENTITY, "interval must be >= 15s".to_string()));
    }

    let ts = now();
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO monitors (name, type, url, method, headers, body, auth_type, auth_ref, \
         expected_status_codes, interval_seconds, timeout_seconds, follow_redirects, verify_ssl, \
         confirmation_threshold, recovery_threshold, retry_interval_seconds, \
         host, port, keyword, keyword_mode, keyword_case_sensitive, dns_record_type, dns_expected_value, \
         ssl_check_enabled, ssl_alert_days, domain_check_enabled, domain_alert_days, \
         status, is_paused, created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 'pending', 0, ?, ?) \
         RETURNING id",
    )
    .bind(&dto.name)
    .bind(&dto.r#type)
    .bind(&dto.url)
    .bind(&dto.method)
    .bind(&dto.headers)
    .bind(&dto.body)
    .bind(&dto.auth_type)
    .bind(&dto.auth_ref)
    .bind(&dto.expected_status_codes)
    .bind(dto.interval_seconds)
    .bind(dto.timeout_seconds)
    .bind(dto.follow_redirects)
    .bind(dto.verify_ssl)
    .bind(dto.confirmation_threshold)
    .bind(dto.recovery_threshold)
    .bind(dto.retry_interval_seconds)
    .bind(&dto.host)
    .bind(dto.port)
    .bind(&dto.keyword)
    .bind(&dto.keyword_mode)
    .bind(dto.keyword_case_sensitive)
    .bind(&dto.dns_record_type)
    .bind(&dto.dns_expected_value)
    .bind(dto.ssl_check_enabled)
    .bind(&dto.ssl_alert_days)
    .bind(dto.domain_check_enabled)
    .bind(&dto.domain_alert_days)
    .bind(ts)
    .bind(ts)
    .fetch_one(&state.db)
    .await
    .map_err(db_err)?;

    let _ = state.sched_tx.send(SchedCmd::Upsert(id));

    let m = fetch_monitor(&state.db, id).await.map_err(db_err)?.ok_or_else(not_found)?;
    Ok(Json(m))
}

pub async fn update(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(dto): Json<UpdateMonitorDto>,
) -> ApiResult<Monitor> {
    let existing = fetch_monitor(&state.db, id).await.map_err(db_err)?.ok_or_else(not_found)?;

    if let Some(interval) = dto.interval_seconds {
        if interval < 15 {
            return Err((StatusCode::UNPROCESSABLE_ENTITY, "interval must be >= 15s".to_string()));
        }
    }
    let name = dto.name.unwrap_or(existing.name);
    let url = dto.url.or(existing.url);
    let method = dto.method.unwrap_or(existing.method);
    let headers = dto.headers.or(existing.headers);
    let body = dto.body.or(existing.body);
    let auth_type = dto.auth_type.or(existing.auth_type);
    let auth_ref = dto.auth_ref.or(existing.auth_ref);
    let expected_status_codes = dto.expected_status_codes.unwrap_or(existing.expected_status_codes);
    let interval_seconds = dto.interval_seconds.unwrap_or(existing.interval_seconds);
    let timeout_seconds = dto.timeout_seconds.unwrap_or(existing.timeout_seconds);
    let follow_redirects = dto.follow_redirects.unwrap_or(existing.follow_redirects);
    let verify_ssl = dto.verify_ssl.unwrap_or(existing.verify_ssl);
    let confirmation_threshold = dto.confirmation_threshold.unwrap_or(existing.confirmation_threshold);
    let recovery_threshold = dto.recovery_threshold.unwrap_or(existing.recovery_threshold);
    let retry_interval_seconds = dto.retry_interval_seconds.unwrap_or(existing.retry_interval_seconds);
    // `type` is NOT mutable on edit — set once at create; the form disables
    // the type selector in edit mode, so UpdateMonitorDto has no `type` field.
    let host = dto.host.or(existing.host);
    let port = dto.port.or(existing.port);
    let keyword = dto.keyword.or(existing.keyword);
    let keyword_mode = dto.keyword_mode.or(existing.keyword_mode);
    let keyword_case_sensitive = dto.keyword_case_sensitive.unwrap_or(existing.keyword_case_sensitive);
    let dns_record_type = dto.dns_record_type.or(existing.dns_record_type);
    let dns_expected_value = dto.dns_expected_value.or(existing.dns_expected_value);
    let ssl_check_enabled = dto.ssl_check_enabled.unwrap_or(existing.ssl_check_enabled);
    let ssl_alert_days = dto.ssl_alert_days.unwrap_or(existing.ssl_alert_days);
    let domain_check_enabled = dto.domain_check_enabled.unwrap_or(existing.domain_check_enabled);
    let domain_alert_days = dto.domain_alert_days.unwrap_or(existing.domain_alert_days);

    if let Err(msg) = validate_monitor_dto(
        &existing.r#type,
        &url,
        &host,
        &port,
        &keyword,
        &keyword_mode,
        &dns_record_type,
        ssl_check_enabled,
    ) {
        return Err((StatusCode::UNPROCESSABLE_ENTITY, msg));
    }

    let ts = now();
    sqlx::query(
        "UPDATE monitors SET name=?, url=?, method=?, headers=?, body=?, auth_type=?, auth_ref=?, \
         expected_status_codes=?, interval_seconds=?, timeout_seconds=?, follow_redirects=?, \
         verify_ssl=?, confirmation_threshold=?, recovery_threshold=?, retry_interval_seconds=?, \
         host=?, port=?, keyword=?, keyword_mode=?, keyword_case_sensitive=?, dns_record_type=?, \
         dns_expected_value=?, ssl_check_enabled=?, ssl_alert_days=?, domain_check_enabled=?, \
         domain_alert_days=?, updated_at=? WHERE id=?",
    )
    .bind(&name)
    .bind(&url)
    .bind(&method)
    .bind(&headers)
    .bind(&body)
    .bind(&auth_type)
    .bind(&auth_ref)
    .bind(&expected_status_codes)
    .bind(interval_seconds)
    .bind(timeout_seconds)
    .bind(follow_redirects)
    .bind(verify_ssl)
    .bind(confirmation_threshold)
    .bind(recovery_threshold)
    .bind(retry_interval_seconds)
    .bind(&host)
    .bind(port)
    .bind(&keyword)
    .bind(&keyword_mode)
    .bind(keyword_case_sensitive)
    .bind(&dns_record_type)
    .bind(&dns_expected_value)
    .bind(ssl_check_enabled)
    .bind(&ssl_alert_days)
    .bind(domain_check_enabled)
    .bind(&domain_alert_days)
    .bind(ts)
    .bind(id)
    .execute(&state.db)
    .await
    .map_err(db_err)?;

    let _ = state.sched_tx.send(SchedCmd::Upsert(id));

    let m = fetch_monitor(&state.db, id).await.map_err(db_err)?.ok_or_else(not_found)?;
    Ok(Json(m))
}

pub async fn delete(State(state): State<AppState>, Path(id): Path<i64>) -> ApiResult<Value> {
    sqlx::query("DELETE FROM monitors WHERE id = ?")
        .bind(id)
        .execute(&state.db)
        .await
        .map_err(db_err)?;
    let _ = state.sched_tx.send(SchedCmd::Remove(id));
    Ok(Json(json!({ "ok": true })))
}

pub async fn pause(State(state): State<AppState>, Path(id): Path<i64>) -> ApiResult<Value> {
    sqlx::query("UPDATE monitors SET is_paused = 1, status = 'paused' WHERE id = ?")
        .bind(id)
        .execute(&state.db)
        .await
        .map_err(db_err)?;
    let _ = state.sched_tx.send(SchedCmd::Remove(id));
    Ok(Json(json!({ "ok": true })))
}

pub async fn resume(State(state): State<AppState>, Path(id): Path<i64>) -> ApiResult<Value> {
    sqlx::query("UPDATE monitors SET is_paused = 0, status = 'pending', next_run_at = 0 WHERE id = ?")
        .bind(id)
        .execute(&state.db)
        .await
        .map_err(db_err)?;
    let _ = state.sched_tx.send(SchedCmd::Upsert(id));
    Ok(Json(json!({ "ok": true })))
}

pub async fn check_now(State(state): State<AppState>, Path(id): Path<i64>) -> ApiResult<Value> {
    let _ = state.sched_tx.send(SchedCmd::CheckNow(id));
    Ok(Json(json!({ "ok": true })))
}

/// Runs a real probe against the submitted DTO without persisting anything
/// — the editor panel's "Test check" button. Starts from
/// `models::test_defaults_monitor()` (a fully-defaulted `http` monitor) and
/// overrides only the fields the DTO carries.
pub async fn test_check(Json(dto): Json<CreateMonitorDto>) -> Json<ProbeOutcome> {
    let mut m = crate::models::test_defaults_monitor();
    m.name = dto.name;
    m.r#type = dto.r#type;
    m.url = dto.url;
    m.method = dto.method;
    m.headers = dto.headers;
    m.body = dto.body;
    m.auth_type = dto.auth_type;
    m.auth_ref = dto.auth_ref;
    m.expected_status_codes = dto.expected_status_codes;
    m.timeout_seconds = dto.timeout_seconds;
    m.follow_redirects = dto.follow_redirects;
    m.verify_ssl = dto.verify_ssl;
    m.host = dto.host;
    m.port = dto.port;
    m.keyword = dto.keyword;
    m.keyword_mode = dto.keyword_mode;
    m.keyword_case_sensitive = dto.keyword_case_sensitive;
    m.dns_record_type = dto.dns_record_type;
    m.dns_expected_value = dto.dns_expected_value;
    m.ssl_check_enabled = dto.ssl_check_enabled;
    m.ssl_alert_days = dto.ssl_alert_days;
    m.domain_check_enabled = dto.domain_check_enabled;
    m.domain_alert_days = dto.domain_alert_days;

    // `ssl`-type monitors have no `AppState`/pool here (`test_check` is a
    // stateless handler for an unsaved DTO), so this calls `ssl::check`
    // directly rather than `certcheck::ssl::ssl_probe` — the live "Test
    // check" button must never write an `ssl_certs` row for a monitor that
    // hasn't been saved yet.
    if m.r#type == "ssl" {
        let host = m.host.clone().unwrap_or_default();
        let port = m.port.unwrap_or(443) as u16;
        let r = crate::certcheck::ssl::check(&host, port, m.timeout_seconds as u64).await;
        return Json(ProbeOutcome {
            ok: r.is_valid,
            response_time_ms: None,
            status_code: None,
            error_message: r.error,
            resolved_ip: None,
            cause: if r.is_valid { None } else { Some(crate::models::Cause::Ssl) },
        });
    }

    let out = probe::run(&m).await;
    Json(out)
}

/// A single monitor→channel attachment as exchanged with the frontend: the
/// channel id plus the list of triggers (`down`, `recovered`, ...) that
/// should fire notifications on that channel for this monitor.
#[derive(Clone, Debug, Deserialize, serde::Serialize)]
pub struct MonitorNotification {
    pub channel_id: i64,
    pub triggers: Vec<String>,
}

#[derive(sqlx::FromRow)]
struct MonitorNotificationRow {
    channel_id: i64,
    triggers: String,
}

pub async fn list_notifications(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> ApiResult<Vec<MonitorNotification>> {
    let rows: Vec<MonitorNotificationRow> = sqlx::query_as(
        "SELECT channel_id, triggers FROM monitor_notifications WHERE monitor_id = ?",
    )
    .bind(id)
    .fetch_all(&state.db)
    .await
    .map_err(db_err)?;

    let out = rows
        .into_iter()
        .map(|r| MonitorNotification {
            channel_id: r.channel_id,
            triggers: serde_json::from_str(&r.triggers).unwrap_or_default(),
        })
        .collect();
    Ok(Json(out))
}

pub async fn set_notifications(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(items): Json<Vec<MonitorNotification>>,
) -> ApiResult<Value> {
    let mut tx = state.db.begin().await.map_err(db_err)?;

    sqlx::query("DELETE FROM monitor_notifications WHERE monitor_id = ?")
        .bind(id)
        .execute(&mut *tx)
        .await
        .map_err(db_err)?;

    for item in &items {
        let triggers_json = serde_json::to_string(&item.triggers).unwrap_or_else(|_| "[]".to_string());
        sqlx::query(
            "INSERT INTO monitor_notifications (monitor_id, channel_id, triggers) VALUES (?, ?, ?)",
        )
        .bind(id)
        .bind(item.channel_id)
        .bind(triggers_json)
        .execute(&mut *tx)
        .await
        .map_err(db_err)?;
    }

    tx.commit().await.map_err(db_err)?;
    Ok(Json(json!({ "ok": true })))
}

/// Maps a `range` query value to a window length in seconds. Unknown or
/// absent values default to 24h (the P1 behavior).
fn range_window_seconds(range: Option<&str>) -> i64 {
    match range {
        Some("7d") => 7 * 86400,
        Some("30d") => 30 * 86400,
        Some("90d") => 90 * 86400,
        _ => 86400,
    }
}

#[derive(Deserialize)]
pub struct StatsQuery {
    range: Option<String>,
}

pub async fn stats(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Query(q): Query<StatsQuery>,
) -> ApiResult<Value> {
    let window = range_window_seconds(q.range.as_deref());
    let ts = now();
    let window_start = ts - window;

    let raw_spans: Vec<(i64, Option<i64>)> = sqlx::query_as(
        "SELECT started_at, resolved_at FROM incidents WHERE monitor_id = ? \
         AND (resolved_at IS NULL OR resolved_at >= ?)",
    )
    .bind(id)
    .bind(window_start)
    .fetch_all(&state.db)
    .await
    .map_err(db_err)?;
    let spans: Vec<uptime::Span> = raw_spans
        .into_iter()
        .map(|(start, end)| uptime::Span { start, end })
        .collect();

    let had_any_check: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM checks WHERE monitor_id = ? AND checked_at >= ?)",
    )
    .bind(id)
    .bind(window_start)
    .fetch_one(&state.db)
    .await
    .map_err(db_err)?;

    let u = uptime::compute(&spans, window_start, ts, had_any_check);

    // 24h/7d: a straight average over raw `checks` is cheap and precise
    // enough. 30d/90d: raw checks that old may already be pruned (retention
    // default 30d), so fold in the daily rollups (count-weighted by
    // `sample_count`) plus today's not-yet-rolled-up raw checks as one more
    // weighted term.
    let avg_ms: Option<f64> = if window <= 7 * 86400 {
        sqlx::query_scalar(
            "SELECT AVG(response_time_ms) FROM checks WHERE monitor_id = ? AND checked_at >= ? \
             AND response_time_ms IS NOT NULL",
        )
        .bind(id)
        .bind(window_start)
        .fetch_one(&state.db)
        .await
        .map_err(db_err)?
    } else {
        let retention = crate::settings_store::retention_days(&state.db).await;
        crate::rollup::ensure_aggregates(&state.db, id, retention)
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

        let window_start_day = crate::rollup::day_str(window_start);
        let (agg_sum, agg_n): (Option<f64>, Option<f64>) = sqlx::query_as(
            "SELECT CAST(SUM(avg_response_ms * sample_count) AS REAL), CAST(SUM(sample_count) AS REAL) \
             FROM check_aggregates_daily \
             WHERE monitor_id = ? AND day >= ? AND avg_response_ms IS NOT NULL",
        )
        .bind(id)
        .bind(&window_start_day)
        .fetch_one(&state.db)
        .await
        .map_err(db_err)?;

        // Today's aggregate row doesn't exist yet (`ensure_aggregates` never
        // rolls up the current day), so blend in today's raw checks as one
        // more weighted term. Bound to today's UTC calendar day only (not a
        // rolling last-24h window) — a rolling window re-includes the tail
        // of yesterday's checks, which are already baked into yesterday's
        // aggregate row, double-counting them.
        let today_start = crate::rollup::day_bounds(&crate::rollup::day_str(ts)).0;
        let (raw_sum, raw_n): (Option<f64>, Option<f64>) = sqlx::query_as(
            "SELECT CAST(SUM(response_time_ms) AS REAL), CAST(COUNT(response_time_ms) AS REAL) \
             FROM checks WHERE monitor_id = ? AND checked_at >= ? AND response_time_ms IS NOT NULL",
        )
        .bind(id)
        .bind(today_start)
        .fetch_one(&state.db)
        .await
        .map_err(db_err)?;

        let total_sum = agg_sum.unwrap_or(0.0) + raw_sum.unwrap_or(0.0);
        let total_n = agg_n.unwrap_or(0.0) + raw_n.unwrap_or(0.0);
        if total_n > 0.0 { Some(total_sum / total_n) } else { None }
    };

    // Reuse the already-computed `spans` (overlapping the window) rather than
    // a separately-scoped `started_at >= window_start` query — that narrower
    // window would undercount incidents still open from before the window
    // (downtime shows, but the count of incidents responsible for it is 0).
    let incidents = spans.len() as i64;

    Ok(Json(json!({
        "uptime_pct": u.uptime_pct,
        "downtime_seconds": u.downtime_seconds,
        "avg_ms": avg_ms,
        "incidents": incidents,
    })))
}

/// One bucketed point of the response-time/status series (§11.6 #4 — the
/// detail panel's response-time chart).
#[derive(serde::Serialize)]
pub struct SeriesPoint {
    t: i64,
    ms: Option<i64>,
    status: String,
}

#[derive(Deserialize)]
pub struct SeriesQuery {
    range: Option<String>,
}

/// Raw `checks` in the window, bucketed into at most 300 equal time-slots
/// (empty slots omitted) so a wide range never ships hundreds of thousands
/// of points to the frontend. Each slot's `ms` is the average response time
/// of checks landing in it; `status` is `"down"` if any check in the slot
/// was down, else `"up"`.
pub async fn series(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Query(q): Query<SeriesQuery>,
) -> ApiResult<Vec<SeriesPoint>> {
    const MAX_BUCKETS: i64 = 300;

    let window = range_window_seconds(q.range.as_deref());
    let ts = now();
    let window_start = ts - window;

    let rows: Vec<(i64, Option<i64>, String)> = sqlx::query_as(
        "SELECT checked_at, response_time_ms, status FROM checks \
         WHERE monitor_id = ? AND checked_at >= ? ORDER BY checked_at ASC",
    )
    .bind(id)
    .bind(window_start)
    .fetch_all(&state.db)
    .await
    .map_err(db_err)?;

    let slot_width = (window / MAX_BUCKETS).max(1);

    // slot -> (sum_ms, count_ms, any_down)
    let mut buckets: std::collections::BTreeMap<i64, (i64, i64, bool)> = std::collections::BTreeMap::new();
    for (checked_at, ms, status) in rows {
        let slot = ((checked_at - window_start) / slot_width).clamp(0, MAX_BUCKETS - 1);
        let entry = buckets.entry(slot).or_insert((0, 0, false));
        if let Some(v) = ms {
            entry.0 += v;
            entry.1 += 1;
        }
        if status == "down" {
            entry.2 = true;
        }
    }

    let points = buckets
        .into_iter()
        .map(|(slot, (sum, count, any_down))| SeriesPoint {
            t: window_start + slot * slot_width,
            ms: if count > 0 { Some(((sum as f64) / (count as f64)).round() as i64) } else { None },
            status: if any_down { "down" } else { "up" }.to_string(),
        })
        .collect();

    Ok(Json(points))
}

/// One day of the 90-day uptime bar (§11.5).
#[derive(serde::Serialize)]
pub struct BarRow {
    day: String,
    uptime_pct: Option<f64>,
    incidents: i64,
    down_seconds: i64,
    has_data: bool,
}

#[derive(Deserialize)]
pub struct BarsQuery {
    days: Option<i64>,
}

/// Per-day uptime/downtime/incident-count for the last `days` UTC days
/// (default 90, capped at 90), oldest first. Backs the signature 90-day
/// uptime bar. `has_data` distinguishes "100% up all day" from "no signal
/// at all for this day" (rendered as the muted `--border-default` segment)
/// — computed from a rollup row, a raw check, or an overlapping incident.
pub async fn bars(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Query(q): Query<BarsQuery>,
) -> ApiResult<Vec<BarRow>> {
    let days = q.days.unwrap_or(90).clamp(1, 90);

    let retention = crate::settings_store::retention_days(&state.db).await;
    crate::rollup::ensure_aggregates(&state.db, id, retention)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let ts = now();
    let today = crate::rollup::day_str(ts);
    let (today_start, today_end) = crate::rollup::day_bounds(&today);
    let range_start = today_start - (days - 1) * 86400;
    let retention_floor = ts - retention * 86400;

    // Every incident that could overlap any day in range, fetched once.
    let raw_spans: Vec<(i64, Option<i64>)> = sqlx::query_as(
        "SELECT started_at, resolved_at FROM incidents WHERE monitor_id = ? \
         AND started_at < ? AND (resolved_at IS NULL OR resolved_at > ?)",
    )
    .bind(id)
    .bind(today_end)
    .bind(range_start)
    .fetch_all(&state.db)
    .await
    .map_err(db_err)?;
    let all_spans: Vec<uptime::Span> =
        raw_spans.into_iter().map(|(start, end)| uptime::Span { start, end }).collect();

    // Days (UTC, "YYYY-MM-DD") with at least one raw check, fetched once.
    let check_days: std::collections::HashSet<String> = sqlx::query_scalar(
        "SELECT DISTINCT date(checked_at, 'unixepoch') FROM checks \
         WHERE monitor_id = ? AND checked_at >= ? AND checked_at < ?",
    )
    .bind(id)
    .bind(range_start)
    .bind(today_end)
    .fetch_all(&state.db)
    .await
    .map_err(db_err)?
    .into_iter()
    .collect();

    // Days with an existing rollup row, fetched once.
    let agg_days: std::collections::HashSet<String> = sqlx::query_scalar(
        "SELECT day FROM check_aggregates_daily WHERE monitor_id = ? AND day >= ? AND day <= ?",
    )
    .bind(id)
    .bind(crate::rollup::day_str(range_start))
    .bind(&today)
    .fetch_all(&state.db)
    .await
    .map_err(db_err)?
    .into_iter()
    .collect();

    let mut out = Vec::with_capacity(days as usize);
    for offset in (0..days).rev() {
        let day_start = today_start - offset * 86400;
        let day = crate::rollup::day_str(day_start);
        let (ds, de) = crate::rollup::day_bounds(&day);
        let clipped_end = de.min(ts).max(ds);

        let day_spans: Vec<uptime::Span> = all_spans
            .iter()
            .filter(|s| s.start < de && s.end.is_none_or(|e| e > ds))
            .copied()
            .collect();

        let u = uptime::compute(&day_spans, ds, clipped_end, true);

        let incidents =
            all_spans.iter().filter(|s| s.start >= ds && s.start < de).count() as i64;

        let has_data = agg_days.contains(&day)
            || (ds >= retention_floor && check_days.contains(&day))
            || !day_spans.is_empty();

        out.push(BarRow {
            day,
            uptime_pct: u.uptime_pct,
            incidents,
            down_seconds: u.downtime_seconds,
            has_data,
        });
    }

    Ok(Json(out))
}

/// The SSL cert card's data (§11.6 #6). `None`/`null` means no cert check
/// has ever run for this monitor yet (add-on just enabled, or the slow
/// cadence hasn't ticked) — not an error.
pub async fn get_ssl(State(state): State<AppState>, Path(id): Path<i64>) -> ApiResult<Option<SslCert>> {
    let row: Option<SslCert> = sqlx::query_as("SELECT * FROM ssl_certs WHERE monitor_id = ?")
        .bind(id)
        .fetch_optional(&state.db)
        .await
        .map_err(db_err)?;
    Ok(Json(row))
}

/// The domain-expiry card's data (§11.6 #7). `None`/`null` means no domain
/// check has ever run for this monitor yet, same as `get_ssl`.
pub async fn get_domain(State(state): State<AppState>, Path(id): Path<i64>) -> ApiResult<Option<DomainInfo>> {
    let row: Option<DomainInfo> = sqlx::query_as("SELECT * FROM domain_info WHERE monitor_id = ?")
        .bind(id)
        .fetch_optional(&state.db)
        .await
        .map_err(db_err)?;
    Ok(Json(row))
}

/// On-demand SSL refresh (the cert card's `Refresh` button). Runs the same
/// `cert_scheduler::refresh_ssl` the 12h slow-cadence scheduler uses, then
/// returns the freshly persisted row.
///
/// Known benign race (Task 7 review): this does read-old -> check -> write
/// with no per-monitor lock, so a manual refresh racing the scheduler's own
/// tick for the same monitor could double-fire one alert threshold. Single-
/// operator app, worst case one duplicate notification — not worth a
/// locking subsystem (YAGNI).
pub async fn refresh_ssl(State(state): State<AppState>, Path(id): Path<i64>) -> ApiResult<Option<SslCert>> {
    crate::cert_scheduler::refresh_ssl(&state, id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    get_ssl(State(state), Path(id)).await
}

/// On-demand domain-expiry refresh (the domain card's `Refresh` button).
/// Same shape and same known race as `refresh_ssl` above.
pub async fn refresh_domain(State(state): State<AppState>, Path(id): Path<i64>) -> ApiResult<Option<DomainInfo>> {
    crate::cert_scheduler::refresh_domain(&state, id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    get_domain(State(state), Path(id)).await
}
