//! `/api/channels*` handlers: notification-channel CRUD + a
//! "send a real test email" action reusing the app's `Transport`.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};

use super::{db_err, now};
use crate::app::AppState;
use crate::notify::{EmailMsg, NotifyMsg, SmtpConfig};

type ApiResult<T> = Result<Json<T>, (StatusCode, String)>;

#[derive(Clone, Debug, serde::Serialize, sqlx::FromRow)]
pub struct Channel {
    pub id: i64,
    pub name: String,
    pub r#type: String,
    pub config: String,
    pub is_active: bool,
    pub created_at: i64,
}

async fn fetch_channel(pool: &sqlx::SqlitePool, id: i64) -> Result<Option<Channel>, sqlx::Error> {
    sqlx::query_as::<_, Channel>("SELECT * FROM notification_channels WHERE id = ?")
        .bind(id)
        .fetch_optional(pool)
        .await
}

fn not_found() -> (StatusCode, String) {
    (StatusCode::NOT_FOUND, "channel not found".to_string())
}

pub async fn list(State(state): State<AppState>) -> ApiResult<Vec<Channel>> {
    let rows = sqlx::query_as::<_, Channel>("SELECT * FROM notification_channels ORDER BY id")
        .fetch_all(&state.db)
        .await
        .map_err(db_err)?;
    Ok(Json(rows))
}

#[derive(Deserialize)]
pub struct CreateChannelDto {
    pub name: String,
    pub r#type: String,
    pub config: String,
}

pub async fn create(
    State(state): State<AppState>,
    Json(dto): Json<CreateChannelDto>,
) -> ApiResult<Channel> {
    let ts = now();
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO notification_channels (name, type, config, is_active, created_at) \
         VALUES (?, ?, ?, 1, ?) RETURNING id",
    )
    .bind(&dto.name)
    .bind(&dto.r#type)
    .bind(&dto.config)
    .bind(ts)
    .fetch_one(&state.db)
    .await
    .map_err(db_err)?;

    let row = fetch_channel(&state.db, id).await.map_err(db_err)?.ok_or_else(not_found)?;
    Ok(Json(row))
}

#[derive(Deserialize)]
pub struct UpdateChannelDto {
    pub name: Option<String>,
    pub config: Option<String>,
    pub is_active: Option<bool>,
}

pub async fn update(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(dto): Json<UpdateChannelDto>,
) -> ApiResult<Channel> {
    let existing = fetch_channel(&state.db, id).await.map_err(db_err)?.ok_or_else(not_found)?;

    let name = dto.name.unwrap_or(existing.name);
    let config = dto.config.unwrap_or(existing.config);
    let is_active = dto.is_active.unwrap_or(existing.is_active);

    sqlx::query("UPDATE notification_channels SET name = ?, config = ?, is_active = ? WHERE id = ?")
        .bind(&name)
        .bind(&config)
        .bind(is_active)
        .bind(id)
        .execute(&state.db)
        .await
        .map_err(db_err)?;

    let row = fetch_channel(&state.db, id).await.map_err(db_err)?.ok_or_else(not_found)?;
    Ok(Json(row))
}

pub async fn delete(State(state): State<AppState>, Path(id): Path<i64>) -> ApiResult<Value> {
    sqlx::query("DELETE FROM notification_channels WHERE id = ?")
        .bind(id)
        .execute(&state.db)
        .await
        .map_err(db_err)?;
    Ok(Json(json!({ "ok": true })))
}

/// The subset of `notification_channels.config` (type='email') needed to
/// send a test message: `{host, port, security, from, to[], username?}`.
/// `username` is optional — see the identical copy in `notify/dispatch.rs`
/// (kept in sync deliberately rather than unified, per the existing
/// pre-P3 duplication).
#[derive(Deserialize)]
struct EmailChannelConfig {
    host: String,
    port: u16,
    security: String,
    from: String,
    to: Vec<String>,
    #[serde(default)]
    username: Option<String>,
}

/// Sends a real test notification over the channel's actual transport:
/// email via `Transport`, everything else (webhook/discord/ntfy) via
/// `HttpSender`, routed by `row.r#type`.
pub async fn test(State(state): State<AppState>, Path(id): Path<i64>) -> ApiResult<Value> {
    let row = fetch_channel(&state.db, id).await.map_err(db_err)?.ok_or_else(not_found)?;

    let result: anyhow::Result<()> = if row.r#type == "email" {
        match serde_json::from_str::<EmailChannelConfig>(&row.config) {
            Ok(cfg) => {
                let smtp_cfg = SmtpConfig {
                    host: cfg.host,
                    port: cfg.port,
                    security: cfg.security,
                    username: cfg.username,
                };
                let msg = EmailMsg {
                    to: cfg.to,
                    from: cfg.from,
                    subject: "Vigil test email".to_string(),
                    body_text: "This is a test from Vigil.".to_string(),
                    body_html: None,
                };
                state.transport.send(&smtp_cfg, &msg).await
            }
            Err(e) => Err(e.into()),
        }
    } else {
        match serde_json::from_str::<Value>(&row.config) {
            Ok(cfg) => {
                let msg = NotifyMsg {
                    monitor_name: "Vigil test".to_string(),
                    url: String::new(),
                    status: "test".to_string(),
                    status_code: None,
                    error: None,
                    response_time_ms: None,
                    duration: None,
                    ssl_days: None,
                    domain_days: None,
                    checked_at: now(),
                    incident_url: None,
                    subject: "Vigil test notification".to_string(),
                    body: "This is a test from Vigil.".to_string(),
                    body_html: None,
                };
                state.http_sender.send(&row.r#type, &cfg, &msg).await
            }
            Err(e) => Err(e.into()),
        }
    };

    Ok(Json(json!({
        "ok": result.is_ok(),
        "error": result.err().map(|e| e.to_string()),
    })))
}
