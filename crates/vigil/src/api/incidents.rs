//! `/api/incidents*` handlers: the global incident timeline (§11.8) and
//! per-incident acknowledge, used to silence re-notifications on an ongoing
//! outage without resolving it.

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};

use super::{db_err, now};
use crate::app::AppState;

type ApiResult<T> = Result<Json<T>, (StatusCode, String)>;

/// An incident joined with its monitor's name, as exchanged with the
/// frontend's global incident timeline (§11.8) and the detail panel's
/// per-monitor incident history (§11.6 #8).
#[derive(serde::Serialize)]
pub struct IncidentDto {
    pub id: i64,
    pub monitor_id: i64,
    pub monitor_name: String,
    pub started_at: i64,
    pub resolved_at: Option<i64>,
    pub duration_seconds: Option<i64>,
    pub cause: Option<String>,
    pub status_code: Option<i64>,
    pub error_message: Option<String>,
    pub acknowledged: bool,
}

/// Maps a `range` query value to a window length in seconds. Unknown or
/// absent values default to 30d (the incidents-list default, per spec §11.8
/// — distinct from the detail-panel stats endpoints, which default to 24h).
fn range_window_seconds(range: Option<&str>) -> i64 {
    match range {
        Some("24h") => 86400,
        Some("7d") => 7 * 86400,
        Some("90d") => 90 * 86400,
        _ => 30 * 86400,
    }
}

#[derive(Deserialize)]
pub struct ListQuery {
    monitor_id: Option<i64>,
    range: Option<String>,
}

type IncidentRow = (
    i64,
    i64,
    String,
    i64,
    Option<i64>,
    Option<i64>,
    Option<String>,
    Option<i64>,
    Option<String>,
    i64,
);

pub async fn list(
    State(state): State<AppState>,
    Query(q): Query<ListQuery>,
) -> ApiResult<Vec<IncidentDto>> {
    let window = range_window_seconds(q.range.as_deref());
    let window_start = now() - window;

    let rows: Vec<IncidentRow> = sqlx::query_as(
        "SELECT i.id, i.monitor_id, m.name, i.started_at, i.resolved_at, i.duration_seconds, \
         i.cause, i.status_code, i.error_message, i.acknowledged \
         FROM incidents i JOIN monitors m ON m.id = i.monitor_id \
         WHERE (? IS NULL OR i.monitor_id = ?) AND i.started_at >= ? \
         ORDER BY i.started_at DESC",
    )
    .bind(q.monitor_id)
    .bind(q.monitor_id)
    .bind(window_start)
    .fetch_all(&state.db)
    .await
    .map_err(db_err)?;

    let out = rows
        .into_iter()
        .map(
            |(
                id,
                monitor_id,
                monitor_name,
                started_at,
                resolved_at,
                duration_seconds,
                cause,
                status_code,
                error_message,
                acknowledged,
            )| IncidentDto {
                id,
                monitor_id,
                monitor_name,
                started_at,
                resolved_at,
                duration_seconds,
                cause,
                status_code,
                error_message,
                acknowledged: acknowledged != 0,
            },
        )
        .collect();

    Ok(Json(out))
}

pub async fn acknowledge(State(state): State<AppState>, Path(id): Path<i64>) -> ApiResult<Value> {
    sqlx::query("UPDATE incidents SET acknowledged = 1 WHERE id = ?")
        .bind(id)
        .execute(&state.db)
        .await
        .map_err(db_err)?;
    Ok(Json(json!({ "ok": true })))
}
