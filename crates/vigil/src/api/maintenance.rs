//! `/api/maintenance-windows*` handlers: CRUD for scheduled maintenance
//! windows, plus a body-driven scope preview (`POST .../preview`) used by
//! the create/edit form to show "these N monitors, active now: yes/no"
//! before the window is saved. Follows the `api/channels.rs` precedent —
//! same `ApiResult`/`fetch_*`/`not_found` shape — reusing
//! `crate::models::MaintenanceWindow` directly (it already derives
//! `sqlx::FromRow` + `Serialize`) rather than a parallel local struct.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};

use super::{db_err, now};
use crate::app::AppState;
use crate::maintenance_windows::resolve;
use crate::models::{validate_window_dto, CreateMaintenanceWindowDto, MaintenanceWindow, UpdateMaintenanceWindowDto};

type ApiResult<T> = Result<Json<T>, (StatusCode, String)>;

async fn fetch_window(pool: &sqlx::SqlitePool, id: i64) -> Result<Option<MaintenanceWindow>, sqlx::Error> {
    sqlx::query_as::<_, MaintenanceWindow>("SELECT * FROM maintenance_windows WHERE id = ?")
        .bind(id)
        .fetch_optional(pool)
        .await
}

fn not_found() -> (StatusCode, String) {
    (StatusCode::NOT_FOUND, "maintenance window not found".to_string())
}

fn invalid(msg: String) -> (StatusCode, String) {
    (StatusCode::UNPROCESSABLE_ENTITY, msg)
}

/// The stored `target_ref` column: `NULL` for `scope: "all"` regardless of
/// what was passed (per `validate_window_dto`'s doc — "the API layer forces
/// it to NULL before storage"), otherwise the JSON-encoded form of the
/// validated `target_ref` value (a JSON string for `tag`, a JSON array of
/// ints for `monitors`).
fn target_ref_column(scope: &str, target_ref: &Option<Value>) -> Option<String> {
    if scope == "all" {
        None
    } else {
        target_ref.as_ref().map(|v| serde_json::to_string(v).unwrap_or_default())
    }
}

pub async fn list(State(state): State<AppState>) -> ApiResult<Vec<MaintenanceWindow>> {
    let rows = sqlx::query_as::<_, MaintenanceWindow>("SELECT * FROM maintenance_windows ORDER BY id")
        .fetch_all(&state.db)
        .await
        .map_err(db_err)?;
    Ok(Json(rows))
}

pub async fn create(
    State(state): State<AppState>,
    Json(dto): Json<CreateMaintenanceWindowDto>,
) -> ApiResult<MaintenanceWindow> {
    validate_window_dto(&dto.name, &dto.scope, &dto.target_ref, dto.starts_at, dto.ends_at, &dto.recurrence)
        .map_err(invalid)?;

    let target_ref = target_ref_column(&dto.scope, &dto.target_ref);
    let ts = now();
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO maintenance_windows (name, scope, target_ref, starts_at, ends_at, recurrence, suppress, is_active, created_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, 1, ?) RETURNING id",
    )
    .bind(&dto.name)
    .bind(&dto.scope)
    .bind(&target_ref)
    .bind(dto.starts_at)
    .bind(dto.ends_at)
    .bind(&dto.recurrence)
    .bind(&dto.suppress)
    .bind(ts)
    .fetch_one(&state.db)
    .await
    .map_err(db_err)?;

    let row = fetch_window(&state.db, id).await.map_err(db_err)?.ok_or_else(not_found)?;
    Ok(Json(row))
}

/// Merge-then-validate: `validate_window_dto` needs a COMPLETE window (it
/// checks `target_ref` shape against `scope`, and `ends_at` against
/// `starts_at`) — validating a partial PATCH in isolation would misfire,
/// e.g. rejecting a lone `{"is_active": false}` for "missing" fields that
/// are actually just unchanged. So: fetch the existing row, apply the
/// `Option` DTO fields over it (falling back to the existing value when a
/// field is omitted — `existing.target_ref`, stored as a raw JSON string,
/// is re-parsed to the `Value` shape `validate_window_dto` expects),
/// validate the MERGED window, then UPDATE.
pub async fn update(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(dto): Json<UpdateMaintenanceWindowDto>,
) -> ApiResult<MaintenanceWindow> {
    let existing = fetch_window(&state.db, id).await.map_err(db_err)?.ok_or_else(not_found)?;

    let name = dto.name.unwrap_or(existing.name);
    let scope = dto.scope.unwrap_or(existing.scope);
    let existing_target_ref: Option<Value> =
        existing.target_ref.as_deref().and_then(|raw| serde_json::from_str(raw).ok());
    let target_ref = dto.target_ref.or(existing_target_ref);
    let starts_at = dto.starts_at.unwrap_or(existing.starts_at);
    let ends_at = dto.ends_at.unwrap_or(existing.ends_at);
    let recurrence = dto.recurrence.or(existing.recurrence);
    let suppress = dto.suppress.unwrap_or(existing.suppress);
    let is_active = dto.is_active.unwrap_or(existing.is_active);

    validate_window_dto(&name, &scope, &target_ref, starts_at, ends_at, &recurrence).map_err(invalid)?;

    let target_ref_col = target_ref_column(&scope, &target_ref);

    sqlx::query(
        "UPDATE maintenance_windows SET name = ?, scope = ?, target_ref = ?, starts_at = ?, ends_at = ?, \
         recurrence = ?, suppress = ?, is_active = ? WHERE id = ?",
    )
    .bind(&name)
    .bind(&scope)
    .bind(&target_ref_col)
    .bind(starts_at)
    .bind(ends_at)
    .bind(&recurrence)
    .bind(&suppress)
    .bind(is_active)
    .bind(id)
    .execute(&state.db)
    .await
    .map_err(db_err)?;

    let row = fetch_window(&state.db, id).await.map_err(db_err)?.ok_or_else(not_found)?;
    Ok(Json(row))
}

pub async fn delete(State(state): State<AppState>, Path(id): Path<i64>) -> ApiResult<Value> {
    sqlx::query("DELETE FROM maintenance_windows WHERE id = ?")
        .bind(id)
        .execute(&state.db)
        .await
        .map_err(db_err)?;
    Ok(Json(json!({ "ok": true })))
}

/// Body-driven scope preview for the create/edit form: `{scope, target_ref,
/// recurrence?, starts_at?, ends_at?}` — NOT id-keyed, since the window
/// being previewed may not exist yet. Not run through `validate_window_dto`
/// (this is a preview, not a write — `resolve::monitor_in_scope` already
/// degrades safely to "no matches" on a malformed/unrecognized scope, so an
/// invalid in-progress form just previews as empty rather than erroring).
#[derive(Deserialize)]
pub struct PreviewDto {
    pub scope: String,
    #[serde(default)]
    pub target_ref: Option<Value>,
    #[serde(default)]
    pub recurrence: Option<String>,
    pub starts_at: Option<i64>,
    pub ends_at: Option<i64>,
}

pub async fn preview(State(state): State<AppState>, Json(body): Json<PreviewDto>) -> ApiResult<Value> {
    let rows: Vec<(i64, Option<String>)> = sqlx::query_as("SELECT id, tags FROM monitors")
        .fetch_all(&state.db)
        .await
        .map_err(db_err)?;

    // A transient (never persisted) window built the same way `create`
    // would store one, so `resolve::monitor_in_scope` /
    // `resolve::window_active_at` see exactly the shape they see for a
    // real row.
    let window = MaintenanceWindow {
        id: 0,
        name: String::new(),
        scope: body.scope.clone(),
        target_ref: target_ref_column(&body.scope, &body.target_ref),
        starts_at: body.starts_at.unwrap_or(0),
        ends_at: body.ends_at.unwrap_or(0),
        recurrence: body.recurrence.clone(),
        suppress: "alerts".to_string(),
        is_active: true,
        created_at: 0,
    };

    let affected_monitor_ids: Vec<i64> = rows
        .into_iter()
        .filter_map(|(id, tags)| {
            let tags = resolve::parse_tags(tags.as_deref().unwrap_or(""));
            resolve::monitor_in_scope(&window, id, &tags).then_some(id)
        })
        .collect();

    // "active now" is undefined without a duration — only compute it when
    // BOTH starts_at and ends_at were supplied (a create-form has them by
    // the time it's meaningful to preview activity).
    let active_now = match (body.starts_at, body.ends_at) {
        (Some(_), Some(_)) => Some(resolve::window_active_at(&window, now())),
        _ => None,
    };

    Ok(Json(json!({
        "affected_monitor_ids": affected_monitor_ids,
        "active_now": active_now,
    })))
}
