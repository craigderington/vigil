//! P4.3 re-notify: re-fire the down alert for an ongoing, unacknowledged
//! outage on a global cadence (`notify.renotify_hours`, 0 = off) until it
//! resolves. Reuses the `dispatch::deliver` funnel (maintenance-mute,
//! per-channel cooldown, notification_log) — the log row is the clock.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::app::AppState;
use crate::models::{Connectivity, Monitor, Trigger};
use crate::notify::{dispatch, templates, NotifyMsg, TemplateCtx};
use crate::settings_store;

fn now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs() as i64
}

#[derive(sqlx::FromRow)]
struct OpenIncident {
    incident_id: i64,
    monitor_id: i64,
    started_at: i64,
}

/// Compact "6h 3m" style elapsed string.
fn format_elapsed(secs: i64) -> String {
    let s = secs.max(0);
    let h = s / 3600;
    let m = (s % 3600) / 60;
    if h > 0 {
        format!("{h}h {m}m")
    } else {
        format!("{m}m")
    }
}

/// Build a reminder message by rendering the base down/heartbeat alert, then
/// decorating it uniformly (works for BOTH triggers — no template change).
fn build_reminder_msg(m: &Monitor, trigger: Trigger, started_at: i64, now_ts: i64) -> NotifyMsg {
    let elapsed = now_ts - started_at;
    let ctx = TemplateCtx {
        monitor_name: m.name.clone(),
        url: m.url.clone().unwrap_or_default(),
        status: "down".to_string(),
        status_code: None,
        error: None,
        response_time_ms: None,
        duration: Some(elapsed),
        checked_at: now_ts,
    };
    let (subject, body_text, body_html) = templates::render(trigger, &ctx);
    NotifyMsg {
        monitor_name: ctx.monitor_name.clone(),
        url: ctx.url.clone(),
        status: ctx.status.clone(),
        status_code: None,
        error: None,
        response_time_ms: None,
        duration: Some(elapsed),
        ssl_days: None,
        domain_days: None,
        checked_at: now_ts,
        incident_url: None,
        subject: format!("Reminder: {subject}"),
        body: format!("{body_text}\n\nStill down for {}.", format_elapsed(elapsed)),
        body_html,
    }
}

/// One re-notify pass: fire an overdue reminder for every open, unacked,
/// confirmed-`down`, non-paused incident whose current-incident baseline is
/// older than `renotify_hours`.
pub async fn renotify_once(state: &AppState) -> anyhow::Result<()> {
    let hours = settings_store::renotify_hours(&state.db).await;
    if hours <= 0 {
        return Ok(());
    }
    if state.anchor.current().await == Connectivity::Offline {
        return Ok(());
    }
    let now_ts = now();
    let threshold = hours * 3600;

    let open: Vec<OpenIncident> = sqlx::query_as(
        "SELECT i.id AS incident_id, i.monitor_id, i.started_at \
         FROM incidents i JOIN monitors m ON m.id = i.monitor_id \
         WHERE i.resolved_at IS NULL AND i.acknowledged = 0 \
           AND m.is_paused = 0 AND m.status = 'down'",
    )
    .fetch_all(&state.db)
    .await?;

    for inc in open {
        let m: Option<Monitor> = sqlx::query_as("SELECT * FROM monitors WHERE id = ?")
            .bind(inc.monitor_id)
            .fetch_optional(&state.db)
            .await?;
        let Some(m) = m else { continue }; // deleted mid-pass → skip

        let last_reminder: Option<i64> = sqlx::query_scalar(
            "SELECT MAX(sent_at) FROM notification_log \
             WHERE incident_id = ? AND trigger IN ('down','heartbeat_missed')",
        )
        .bind(inc.incident_id)
        .fetch_one(&state.db)
        .await?;
        let baseline = last_reminder.unwrap_or(inc.started_at);
        if now_ts - baseline < threshold {
            continue;
        }

        // TOCTOU re-check: a recovery/ack may have landed since the batch scan.
        let still: Option<(Option<i64>, bool)> =
            sqlx::query_as("SELECT resolved_at, acknowledged FROM incidents WHERE id = ?")
                .bind(inc.incident_id)
                .fetch_optional(&state.db)
                .await?;
        if !matches!(still, Some((None, false))) {
            continue;
        }

        let trigger = if m.r#type == "heartbeat" {
            Trigger::HeartbeatMissed
        } else {
            Trigger::Down
        };
        let msg = build_reminder_msg(&m, trigger, inc.started_at, now_ts);
        dispatch::deliver(state, &m, trigger, &msg, Some(inc.incident_id)).await?;
    }
    Ok(())
}

/// The re-notify loop: scan every `notify.renotify_tick_seconds` (default 300).
pub async fn run(state: AppState) {
    loop {
        let tick = settings_store::renotify_tick_seconds(&state.db).await;
        if let Err(error) = renotify_once(&state).await {
            tracing::error!(%error, "renotify pass failed");
        }
        tokio::time::sleep(Duration::from_secs(tick.max(1) as u64)).await;
    }
}
