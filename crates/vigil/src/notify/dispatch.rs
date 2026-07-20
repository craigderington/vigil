//! Turns a state-machine transition (or an SSL/domain add-on alert) into a
//! rendered `NotifyMsg`, then hands it to `deliver` — the multi-channel
//! core that loads every active channel attached to the monitor (any
//! `notification_channels.type`, not just `email`), filters by per-trigger
//! opt-in, applies the per-`(monitor, channel, trigger)` cooldown, routes
//! to `Transport` (email) or `HttpSender` (webhook/discord/ntfy), and logs
//! the outcome to `notification_log`.

use std::time::{SystemTime, UNIX_EPOCH};

use serde::Deserialize;

use crate::app::AppState;
use crate::cooldown;
use crate::maintenance_windows::{
    self,
    resolve::{maintenance_for, parse_tags, Suppression},
};
use crate::models::{Monitor, Trigger};
use crate::notify::{templates, AlertCtx, EmailMsg, NotifyMsg, SmtpConfig, TemplateCtx};
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
    channel_type: String,
    config: String,
    triggers: String,
}

/// The subset of `notification_channels.config` (type='email') needed to
/// send: `{host, port, security, from, to[], username?}`. `username` is
/// optional — most SMTP relays authenticate with the From address, but some
/// (Mailgun, SendGrid, etc.) require a separate account/API-key username.
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

/// The status a trigger settles the monitor into, for template rendering.
/// Only meaningful on the up/down transition path (`on_transition`); the
/// SSL/domain add-on triggers don't settle an up/down status, so they fall
/// back to a generic label rather than `unreachable!()` — a future caller
/// reusing this on that path shouldn't panic.
fn trigger_status(trigger: Trigger) -> &'static str {
    match trigger {
        Trigger::Down => "down",
        Trigger::Recovered => "up",
        Trigger::HeartbeatMissed => "down",
        Trigger::SslExpiring | Trigger::SslInvalid | Trigger::DomainExpiring => "alert",
    }
}

/// Renders the up/down transition `NotifyMsg` and delivers it to every
/// attached channel.
pub async fn on_transition(
    state: &AppState,
    m: &Monitor,
    trigger: Trigger,
    incident_id: Option<i64>,
) -> anyhow::Result<()> {
    let checked_at = now();

    let ctx = TemplateCtx {
        monitor_name: m.name.clone(),
        url: m.url.clone().unwrap_or_default(),
        status: trigger_status(trigger).to_string(),
        status_code: None,
        error: None,
        response_time_ms: None,
        duration: None,
        checked_at,
    };
    let (subject, body_text, body_html) = templates::render(trigger, &ctx);

    let msg = NotifyMsg {
        monitor_name: ctx.monitor_name.clone(),
        url: ctx.url.clone(),
        status: ctx.status.clone(),
        status_code: ctx.status_code,
        error: ctx.error.clone(),
        response_time_ms: ctx.response_time_ms,
        duration: ctx.duration,
        ssl_days: None,
        domain_days: None,
        checked_at: ctx.checked_at,
        incident_url: None,
        subject,
        body: body_text,
        body_html,
    };

    deliver(state, m, trigger, &msg, incident_id).await
}

/// Renders the SSL/domain add-on alert `NotifyMsg` and delivers it to every
/// attached channel. No incident is associated with these alerts (they
/// don't open/close `incidents` rows), so `incident_id` is always `None`.
pub async fn send_alert(
    state: &AppState,
    m: &Monitor,
    trigger: Trigger,
    ctx: &AlertCtx,
) -> anyhow::Result<()> {
    let (subject, body_text, body_html) = templates::render_alert(trigger, ctx);

    let msg = NotifyMsg {
        monitor_name: ctx.monitor_name.clone(),
        url: ctx.url.clone(),
        status: trigger.as_str().to_string(),
        status_code: None,
        error: ctx.error.clone(),
        response_time_ms: None,
        duration: None,
        ssl_days: ctx.ssl_days,
        domain_days: ctx.domain_days,
        checked_at: ctx.checked_at,
        incident_url: None,
        subject,
        body: body_text,
        body_html,
    };

    deliver(state, m, trigger, &msg, None).await
}

/// The multi-channel delivery core. Loads every active channel attached to
/// `m` (any type), filters to those opted into `trigger`, applies the
/// per-`(monitor, channel, trigger)` cooldown, sends via `Transport`
/// (email) or `HttpSender` (everything else), and logs each attempt.
pub async fn deliver(
    state: &AppState,
    m: &Monitor,
    trigger: Trigger,
    msg: &NotifyMsg,
    incident_id: Option<i64>,
) -> anyhow::Result<()> {
    let sent_at = now();

    // A monitor under ANY active maintenance window (alerts- or
    // checks-suppressing — both mute alerts, `checks` additionally pauses
    // probing/reaping elsewhere) never gets an alert. The incident itself
    // still opens/closes normally (engine::apply_result / heartbeat::reap_one
    // don't consult maintenance at all) — only this notification funnel is
    // muted, so uptime exclusion (maintenance_intervals) is what nets the
    // outage back out of the uptime %, not a missing incident row.
    let windows = maintenance_windows::active_windows(&state.db).await;
    let tags = parse_tags(m.tags.as_deref().unwrap_or(""));
    if !matches!(maintenance_for(&windows, m.id, &tags, sent_at), Suppression::None) {
        tracing::debug!(monitor_id = m.id, "alert suppressed by maintenance window");
        return Ok(());
    }

    let cooldown_minutes = settings_store::cooldown_minutes(&state.db).await;
    let trigger_str = trigger.as_str();

    // Deliberately no `AND nc.type = 'email'` here — every active channel
    // type attached to this monitor is a candidate; routing by type happens
    // per-channel below.
    let channels: Vec<AttachedChannel> = sqlx::query_as(
        "SELECT nc.id AS channel_id, nc.type AS channel_type, nc.config AS config, mn.triggers AS triggers
         FROM monitor_notifications mn
         JOIN notification_channels nc ON nc.id = mn.channel_id
         WHERE mn.monitor_id = ? AND nc.is_active = 1",
    )
    .bind(m.id)
    .fetch_all(&state.db)
    .await?;

    for ch in channels {
        let triggers: Vec<String> = serde_json::from_str(&ch.triggers).unwrap_or_default();
        if !triggers.iter().any(|t| t == trigger_str) {
            continue;
        }

        // Per-(monitor, channel, trigger) cooldown — NOT per-(monitor,
        // trigger). The old key throttled every channel attached to a
        // monitor+trigger to a single combined send budget, so a 2nd
        // channel could be silently starved by a 1st channel's send.
        let last_sent: Option<i64> = sqlx::query_scalar(
            "SELECT MAX(sent_at) FROM notification_log
             WHERE monitor_id = ? AND channel_id = ? AND trigger = ?",
        )
        .bind(m.id)
        .bind(ch.channel_id)
        .bind(trigger_str)
        .fetch_one(&state.db)
        .await?;

        if !cooldown::allowed(last_sent, sent_at, cooldown_minutes) {
            continue; // suppressed by cooldown
        }

        let result = send_to_channel(state, &ch, msg).await;
        let (success, error) = match &result {
            Ok(()) => (true, None),
            Err(e) => (false, Some(e.to_string())),
        };

        log_result(
            state,
            m.id,
            ch.channel_id,
            incident_id,
            trigger_str,
            sent_at,
            success,
            error.as_deref(),
        )
        .await?;
    }

    Ok(())
}

/// Shared email-send: parse an `EmailChannelConfig` JSON, build the SMTP
/// config + message, and hand off to the transport. Used by both `deliver`'s
/// email arm and the daily digest (which bypasses `deliver`), so the two
/// never diverge (incl. the `username`/`from` handling).
pub async fn send_email_via_channel(
    transport: &dyn crate::notify::Transport,
    config_json: &str,
    subject: &str,
    body_text: &str,
    body_html: Option<String>,
) -> anyhow::Result<()> {
    let cfg: EmailChannelConfig = serde_json::from_str(config_json)?;
    let smtp_cfg = SmtpConfig {
        host: cfg.host,
        port: cfg.port,
        security: cfg.security,
        username: cfg.username,
    };
    let email_msg = EmailMsg {
        to: cfg.to,
        from: cfg.from,
        subject: subject.to_string(),
        body_text: body_text.to_string(),
        body_html,
    };
    transport.send(&smtp_cfg, &email_msg).await
}

async fn send_to_channel(
    state: &AppState,
    ch: &AttachedChannel,
    msg: &NotifyMsg,
) -> anyhow::Result<()> {
    if ch.channel_type == "email" {
        send_email_via_channel(
            state.transport.as_ref(),
            &ch.config,
            &msg.subject,
            &msg.body,
            msg.body_html.clone(),
        )
        .await
    } else {
        let config: serde_json::Value = serde_json::from_str(&ch.config)?;
        state.http_sender.send(&ch.channel_type, &config, msg).await
    }
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
