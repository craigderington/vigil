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
use crate::models::{CreateMonitorDto, Monitor, ProbeOutcome, UpdateMonitorDto};
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
fn validate_monitor_dto(
    r#type: &str,
    url: &Option<String>,
    host: &Option<String>,
    port: &Option<i64>,
    keyword: &Option<String>,
    keyword_mode: &Option<String>,
    dns_record_type: &Option<String>,
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
        // "http" and anything else (ssl-only, future types) default to the
        // original P1 behavior: a url is required.
        _ => {
            if blank(url) {
                return Err("url is required".to_string());
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
         status, is_paused, created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 'pending', 0, ?, ?) \
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

    if let Err(msg) = validate_monitor_dto(
        &existing.r#type,
        &url,
        &host,
        &port,
        &keyword,
        &keyword_mode,
        &dns_record_type,
    ) {
        return Err((StatusCode::UNPROCESSABLE_ENTITY, msg));
    }

    let ts = now();
    sqlx::query(
        "UPDATE monitors SET name=?, url=?, method=?, headers=?, body=?, auth_type=?, auth_ref=?, \
         expected_status_codes=?, interval_seconds=?, timeout_seconds=?, follow_redirects=?, \
         verify_ssl=?, confirmation_threshold=?, recovery_threshold=?, retry_interval_seconds=?, \
         host=?, port=?, keyword=?, keyword_mode=?, keyword_case_sensitive=?, dns_record_type=?, \
         dns_expected_value=?, updated_at=? WHERE id=?",
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

#[derive(Deserialize)]
pub struct StatsQuery {
    range: Option<String>,
}

pub async fn stats(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Query(q): Query<StatsQuery>,
) -> ApiResult<Value> {
    let window: i64 = if q.range.as_deref() == Some("7d") { 7 * 86400 } else { 86400 };
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

    let avg_ms: Option<f64> = sqlx::query_scalar(
        "SELECT AVG(response_time_ms) FROM checks WHERE monitor_id = ? AND checked_at >= ? \
         AND response_time_ms IS NOT NULL",
    )
    .bind(id)
    .bind(window_start)
    .fetch_one(&state.db)
    .await
    .map_err(db_err)?;

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
