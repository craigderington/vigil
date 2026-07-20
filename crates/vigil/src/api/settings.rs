//! `/api/settings` — anchors, notification cooldown, retention, and the
//! appearance accent color. A thin typed wrapper over `settings_store`'s
//! key/value rows.

use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::app::AppState;
use crate::settings_store;

type ApiResult<T> = Result<Json<T>, (StatusCode, String)>;

fn set_err(e: anyhow::Error) -> (StatusCode, String) {
    (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
}

pub async fn get_settings(State(state): State<AppState>) -> ApiResult<Value> {
    Ok(Json(current_settings(&state).await))
}

async fn current_settings(state: &AppState) -> Value {
    json!({
        "anchors": settings_store::anchors(&state.db).await,
        "cooldown_minutes": settings_store::cooldown_minutes(&state.db).await,
        "retention_days": settings_store::retention_days(&state.db).await,
        "accent": settings_store::get(&state.db, "appearance.accent", "#3FC8E4").await,
        "renotify_hours": settings_store::renotify_hours(&state.db).await,
        "digest_enabled": settings_store::digest_enabled(&state.db).await,
        "digest_time": settings_store::digest_time(&state.db).await,
        "digest_recipients": settings_store::digest_recipients(&state.db).await,
        "report_auto_generate": settings_store::report_auto_generate(&state.db).await,
        "report_day_of_month": settings_store::report_day_of_month(&state.db).await,
        "report_time": settings_store::report_time(&state.db).await,
        "report_recipients": settings_store::report_recipients(&state.db).await,
    })
}

#[derive(Deserialize)]
pub struct UpdateSettingsDto {
    /// Either a JSON array of `host:port` strings or a single CSV string.
    pub anchors: Option<Value>,
    pub cooldown_minutes: Option<i64>,
    pub retention_days: Option<i64>,
    pub accent: Option<String>,
    pub renotify_hours: Option<i64>,
    pub digest_enabled: Option<bool>,
    pub digest_time: Option<String>,
    pub digest_recipients: Option<Value>,
    pub report_auto_generate: Option<bool>,
    pub report_day_of_month: Option<i64>,
    pub report_time: Option<String>,
    pub report_recipients: Option<Value>,
}

pub async fn update_settings(
    State(state): State<AppState>,
    Json(dto): Json<UpdateSettingsDto>,
) -> ApiResult<Value> {
    if let Some(anchors) = dto.anchors {
        let hosts: Vec<String> = match anchors {
            Value::Array(items) => items.iter().filter_map(|v| v.as_str().map(str::to_string)).collect(),
            Value::String(s) => s.split(',').map(str::trim).filter(|s| !s.is_empty()).map(str::to_string).collect(),
            _ => Vec::new(),
        };
        settings_store::set(&state.db, "anchors", &hosts.join(","))
            .await
            .map_err(set_err)?;
        state.anchor.set_hosts(hosts);
    }
    if let Some(cooldown) = dto.cooldown_minutes {
        settings_store::set(&state.db, "notify.cooldown_minutes", &cooldown.to_string())
            .await
            .map_err(set_err)?;
    }
    if let Some(retention) = dto.retention_days {
        settings_store::set(&state.db, "retention.raw_days", &retention.to_string())
            .await
            .map_err(set_err)?;
    }
    if let Some(accent) = dto.accent {
        settings_store::set(&state.db, "appearance.accent", &accent)
            .await
            .map_err(set_err)?;
    }
    if let Some(h) = dto.renotify_hours {
        settings_store::set(&state.db, "notify.renotify_hours", &h.to_string())
            .await
            .map_err(set_err)?;
    }
    if let Some(e) = dto.digest_enabled {
        settings_store::set(&state.db, "notify.digest_enabled", if e { "1" } else { "0" })
            .await
            .map_err(set_err)?;
    }
    if let Some(t) = dto.digest_time {
        settings_store::set(&state.db, "notify.digest_time", &t)
            .await
            .map_err(set_err)?;
    }
    if let Some(r) = dto.digest_recipients {
        let ids: Vec<i64> = match &r {
            Value::Array(a) => a.iter().filter_map(|v| v.as_i64()).collect(),
            _ => Vec::new(),
        };
        let encoded = serde_json::to_string(&ids).unwrap_or_else(|_| "[]".to_string());
        settings_store::set(&state.db, "notify.digest_recipients", &encoded)
            .await
            .map_err(set_err)?;
    }
    if let Some(b) = dto.report_auto_generate {
        settings_store::set(&state.db, "report_auto_generate", if b { "1" } else { "0" })
            .await
            .map_err(set_err)?;
    }
    if let Some(d) = dto.report_day_of_month {
        settings_store::set(&state.db, "report_day_of_month", &d.to_string())
            .await
            .map_err(set_err)?;
    }
    if let Some(t) = dto.report_time {
        settings_store::set(&state.db, "report_time", &t)
            .await
            .map_err(set_err)?;
    }
    if let Some(r) = dto.report_recipients {
        let ids: Vec<i64> = match &r {
            Value::Array(a) => a.iter().filter_map(|v| v.as_i64()).collect(),
            _ => Vec::new(),
        };
        settings_store::set(
            &state.db,
            "report_recipients",
            &serde_json::to_string(&ids).unwrap_or_else(|_| "[]".into()),
        )
        .await
        .map_err(set_err)?;
    }

    Ok(Json(current_settings(&state).await))
}
