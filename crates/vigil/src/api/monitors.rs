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
    // P1 monitors are always type='http'; the DTO has no `type` field.
    if dto.url.trim().is_empty() {
        return Err((StatusCode::UNPROCESSABLE_ENTITY, "url is required".to_string()));
    }
    if dto.interval_seconds < 15 {
        return Err((StatusCode::UNPROCESSABLE_ENTITY, "interval must be >= 15s".to_string()));
    }

    let ts = now();
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO monitors (name, type, url, method, headers, body, auth_type, auth_ref, \
         expected_status_codes, interval_seconds, timeout_seconds, follow_redirects, verify_ssl, \
         confirmation_threshold, recovery_threshold, retry_interval_seconds, status, is_paused, \
         created_at, updated_at) \
         VALUES (?, 'http', ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 'pending', 0, ?, ?) \
         RETURNING id",
    )
    .bind(&dto.name)
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
    if let Some(url) = &dto.url {
        if url.trim().is_empty() {
            return Err((StatusCode::UNPROCESSABLE_ENTITY, "url is required".to_string()));
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

    let ts = now();
    sqlx::query(
        "UPDATE monitors SET name=?, url=?, method=?, headers=?, body=?, auth_type=?, auth_ref=?, \
         expected_status_codes=?, interval_seconds=?, timeout_seconds=?, follow_redirects=?, \
         verify_ssl=?, confirmation_threshold=?, recovery_threshold=?, retry_interval_seconds=?, \
         updated_at=? WHERE id=?",
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
    m.url = Some(dto.url);
    m.method = dto.method;
    m.headers = dto.headers;
    m.body = dto.body;
    m.auth_type = dto.auth_type;
    m.auth_ref = dto.auth_ref;
    m.expected_status_codes = dto.expected_status_codes;
    m.timeout_seconds = dto.timeout_seconds;
    m.follow_redirects = dto.follow_redirects;
    m.verify_ssl = dto.verify_ssl;

    let out = probe::http::probe(&m).await;
    Json(out)
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
