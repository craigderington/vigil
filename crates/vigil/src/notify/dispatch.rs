//! Turns a state-machine transition into sent (or suppressed) notification
//! mail: loads the monitor's active email channels, filters by
//! per-trigger opt-in, applies the (monitor, trigger) cooldown, renders
//! and sends, then logs the outcome to `notification_log`.

use std::time::{SystemTime, UNIX_EPOCH};

use serde::Deserialize;

use crate::app::AppState;
use crate::cooldown;
use crate::models::{Monitor, Trigger};
use crate::notify::{templates, EmailMsg, SmtpConfig, TemplateCtx};
use crate::settings_store;

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

/// A channel attached to the monitor, joined with its per-trigger opt-in
/// list from `monitor_notifications`.
#[derive(sqlx::FromRow)]
struct AttachedChannel {
    channel_id: i64,
    config: String,
    triggers: String,
}

/// The subset of `notification_channels.config` (type='email') needed to
/// send: `{host, port, security, from, to[]}`.
#[derive(Deserialize)]
struct EmailChannelConfig {
    host: String,
    port: u16,
    security: String,
    from: String,
    to: Vec<String>,
}

/// The status a trigger settles the monitor into, for template rendering.
fn trigger_status(trigger: Trigger) -> &'static str {
    match trigger {
        Trigger::Down => "down",
        Trigger::Recovered => "up",
    }
}

pub async fn on_transition(
    state: &AppState,
    m: &Monitor,
    trigger: Trigger,
    incident_id: Option<i64>,
) -> anyhow::Result<()> {
    let now = now();

    let cooldown_minutes = settings_store::cooldown_minutes(&state.db).await;

    let channels: Vec<AttachedChannel> = sqlx::query_as(
        "SELECT nc.id AS channel_id, nc.config AS config, mn.triggers AS triggers
         FROM monitor_notifications mn
         JOIN notification_channels nc ON nc.id = mn.channel_id
         WHERE mn.monitor_id = ? AND nc.is_active = 1 AND nc.type = 'email'",
    )
    .bind(m.id)
    .fetch_all(&state.db)
    .await?;

    let trigger_str = trigger.as_str();

    for ch in channels {
        let triggers: Vec<String> = serde_json::from_str(&ch.triggers).unwrap_or_default();
        if !triggers.iter().any(|t| t == trigger_str) {
            continue;
        }

        let last_sent: Option<i64> = sqlx::query_scalar(
            "SELECT MAX(sent_at) FROM notification_log WHERE monitor_id = ? AND trigger = ?",
        )
        .bind(m.id)
        .bind(trigger_str)
        .fetch_one(&state.db)
        .await?;

        if !cooldown::allowed(last_sent, now, cooldown_minutes) {
            continue; // suppressed by cooldown
        }

        let cfg: EmailChannelConfig = match serde_json::from_str(&ch.config) {
            Ok(c) => c,
            Err(e) => {
                log_result(state, m.id, ch.channel_id, incident_id, trigger_str, now, false, Some(&e.to_string()))
                    .await?;
                continue;
            }
        };

        let smtp_cfg = SmtpConfig {
            host: cfg.host,
            port: cfg.port,
            security: cfg.security,
        };

        let ctx = TemplateCtx {
            monitor_name: m.name.clone(),
            url: m.url.clone().unwrap_or_default(),
            status: trigger_status(trigger).to_string(),
            status_code: None,
            error: None,
            response_time_ms: None,
            duration: None,
            checked_at: now,
        };
        let (subject, body_text, body_html) = templates::render(trigger, &ctx);

        let msg = EmailMsg {
            to: cfg.to,
            from: cfg.from,
            subject,
            body_text,
            body_html,
        };

        let result = state.transport.send(&smtp_cfg, &msg).await;
        let (success, error) = match &result {
            Ok(()) => (true, None),
            Err(e) => (false, Some(e.to_string())),
        };

        log_result(state, m.id, ch.channel_id, incident_id, trigger_str, now, success, error.as_deref())
            .await?;
    }

    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn log_result(
    state: &AppState,
    monitor_id: i64,
    channel_id: i64,
    incident_id: Option<i64>,
    trigger: &str,
    sent_at: i64,
    success: bool,
    error: Option<&str>,
) -> anyhow::Result<()> {
    sqlx::query(
        "INSERT INTO notification_log (monitor_id, channel_id, incident_id, trigger, sent_at, success, error)
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(monitor_id)
    .bind(channel_id)
    .bind(incident_id)
    .bind(trigger)
    .bind(sent_at)
    .bind(success)
    .bind(error)
    .execute(&state.db)
    .await?;
    Ok(())
}
