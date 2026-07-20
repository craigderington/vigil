//! REST handlers for monthly incident reports (CLAUDE.md §13, §10).
//! Thin wrappers over `report::{generate, compute, html, send_report_email}` —
//! all computation/caching lives in `crate::report`.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::Html;
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};

use super::{db_err, now};
use crate::app::AppState;
use crate::report::{self, compute::ReportSummary, Report};

type ApiResult<T> = Result<Json<T>, (StatusCode, String)>;

fn not_found() -> (StatusCode, String) { (StatusCode::NOT_FOUND, "report not found".into()) }
fn invalid(m: &str) -> (StatusCode, String) { (StatusCode::UNPROCESSABLE_ENTITY, m.to_string()) }

pub async fn list(State(state): State<AppState>) -> ApiResult<Value> {
    let rows: Vec<Report> = sqlx::query_as("SELECT * FROM reports ORDER BY period_start DESC").fetch_all(&state.db).await.map_err(db_err)?;
    let out: Vec<Value> = rows.iter().map(|r| {
        let s: Option<ReportSummary> = serde_json::from_str(&r.summary_json).ok();
        let f = s.as_ref().map(|s| &s.fleet);
        json!({ "id": r.id, "label": r.label, "period_start": r.period_start, "period_end": r.period_end,
            "generated_at": r.generated_at, "emailed_at": r.emailed_at,
            "headline": { "uptime_pct": f.and_then(|f| f.uptime_pct), "incidents": f.map(|f| f.incidents), "downtime_seconds": f.map(|f| f.downtime_seconds) } })
    }).collect();
    Ok(Json(json!(out)))
}

pub async fn get_one(State(state): State<AppState>, Path(id): Path<i64>) -> ApiResult<Value> {
    let r: Option<Report> = sqlx::query_as("SELECT * FROM reports WHERE id = ?").bind(id).fetch_optional(&state.db).await.map_err(db_err)?;
    let r = r.ok_or_else(not_found)?;
    let summary: ReportSummary = serde_json::from_str(&r.summary_json).map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(json!({ "id": r.id, "label": r.label, "period_start": r.period_start, "period_end": r.period_end,
        "generated_at": r.generated_at, "emailed_at": r.emailed_at, "summary": summary })))
}

#[derive(Deserialize)]
pub struct GenerateDto { pub period: String }

pub async fn generate(State(state): State<AppState>, Json(dto): Json<GenerateDto>) -> ApiResult<Value> {
    // "YYYY-MM", not in the future (compare to the current UTC month)
    let ok_shape = dto.period.len() == 7 && dto.period.as_bytes()[4] == b'-'
        && dto.period[..4].chars().all(|c| c.is_ascii_digit()) && dto.period[5..].chars().all(|c| c.is_ascii_digit());
    if !ok_shape { return Err(invalid("period must be YYYY-MM")); }
    if dto.period.as_str() > report::month_of(now()).as_str() { return Err(invalid("period is in the future")); }
    let r = report::generate(&state, &dto.period).await.map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(json!({ "id": r.id, "label": r.label, "period_start": r.period_start })))
}

pub async fn html(State(state): State<AppState>, Path(id): Path<i64>) -> Result<Html<String>, (StatusCode, String)> {
    let r: Option<Report> = sqlx::query_as("SELECT * FROM reports WHERE id = ?").bind(id).fetch_optional(&state.db).await.map_err(db_err)?;
    let r = r.ok_or_else(not_found)?;
    let summary: ReportSummary = serde_json::from_str(&r.summary_json).map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Html(report::html::render_html(&summary)))
}

pub async fn email(State(state): State<AppState>, Path(id): Path<i64>) -> ApiResult<Value> {
    let r: Option<Report> = sqlx::query_as("SELECT * FROM reports WHERE id = ?").bind(id).fetch_optional(&state.db).await.map_err(db_err)?;
    let r = r.ok_or_else(not_found)?;
    let outcome = report::send_report_email(&state, &r).await;
    Ok(Json(json!({ "ok": matches!(outcome, crate::digest::SendOutcome::Delivered), "outcome": format!("{outcome:?}") })))
}

pub async fn delete(State(state): State<AppState>, Path(id): Path<i64>) -> ApiResult<Value> {
    sqlx::query("DELETE FROM reports WHERE id = ?").bind(id).execute(&state.db).await.map_err(db_err)?;
    Ok(Json(json!({ "ok": true })))
}
