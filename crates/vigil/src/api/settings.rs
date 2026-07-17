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
    })
}

#[derive(Deserialize)]
pub struct UpdateSettingsDto {
    /// Either a JSON array of `host:port` strings or a single CSV string.
    pub anchors: Option<Value>,
    pub cooldown_minutes: Option<i64>,
    pub retention_days: Option<i64>,
    pub accent: Option<String>,
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

    Ok(Json(current_settings(&state).await))
}
