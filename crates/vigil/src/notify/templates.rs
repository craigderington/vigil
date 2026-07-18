//! Notification message templates: renders a `(subject, body_text,
//! body_html)` triple from a `Trigger` and `TemplateCtx`/`AlertCtx`. §7 of
//! the spec ("Message template variables") — P1/P3 emit plaintext only
//! (`body_html` is always `None`).
//!
//! Two render entry points, not one: `render` covers the up/down transition
//! path (`TemplateCtx`, used by `dispatch::on_transition`) and
//! `render_alert` covers the SSL/domain add-on alerts (`AlertCtx`, used by
//! `dispatch::send_alert`). `Trigger` is a single enum shared by both paths,
//! so each render fn's match must stay exhaustive over all 5 variants even
//! though only 2 (or 3) are meaningful to it — the off-path arms return a
//! generic fallback rather than `unreachable!()`, since a future caller
//! could legitimately reach either fn with any trigger.

use crate::models::Trigger;
use crate::notify::{AlertCtx, TemplateCtx};

/// Renders the subject and body for an up/down transition notification.
/// `body_html` is always `None` in P1 — HTML formatting is a later task.
pub fn render(trigger: Trigger, ctx: &TemplateCtx) -> (String, String, Option<String>) {
    let subject = match trigger {
        Trigger::Down => format!("🔴 {} is DOWN", ctx.monitor_name),
        Trigger::Recovered => format!("✅ {} recovered", ctx.monitor_name),
        Trigger::SslExpiring | Trigger::SslInvalid | Trigger::DomainExpiring => {
            format!("{} notification", ctx.monitor_name)
        }
    };

    let mut lines = vec![
        format!("Monitor: {}", ctx.monitor_name),
        format!("URL: {}", ctx.url),
        format!("Status: {}", ctx.status),
        format!(
            "Status code: {}",
            ctx.status_code
                .map(|c| c.to_string())
                .unwrap_or_else(|| "n/a".to_string())
        ),
    ];
    if let Some(err) = &ctx.error {
        lines.push(format!("Error: {err}"));
    }
    if let Some(ms) = ctx.response_time_ms {
        lines.push(format!("Response time: {ms}ms"));
    }
    if let Some(dur) = ctx.duration {
        lines.push(format!("Duration: {dur}s"));
    }
    lines.push(format!("Checked at: {}", ctx.checked_at));

    let body_text = lines.join("\n");

    (subject, body_text, None)
}

/// Renders the subject and body for an SSL/domain add-on alert
/// (`SslExpiring`, `SslInvalid`, `DomainExpiring`). `body_html` is always
/// `None` in P3 — HTML formatting is a later task.
pub fn render_alert(trigger: Trigger, ctx: &AlertCtx) -> (String, String, Option<String>) {
    let subject = match trigger {
        Trigger::SslExpiring => format!("⚠️ {} SSL certificate expiring soon", ctx.monitor_name),
        Trigger::SslInvalid => format!("🔴 {} SSL certificate invalid", ctx.monitor_name),
        Trigger::DomainExpiring => format!("⚠️ {} domain registration expiring soon", ctx.monitor_name),
        Trigger::Down | Trigger::Recovered => format!("{} notification", ctx.monitor_name),
    };

    let mut lines = vec![
        format!("Monitor: {}", ctx.monitor_name),
        format!("URL: {}", ctx.url),
    ];
    if let Some(days) = ctx.ssl_days {
        lines.push(format!("SSL days remaining: {days}"));
    }
    if let Some(days) = ctx.domain_days {
        lines.push(format!("Domain days remaining: {days}"));
    }
    if let Some(err) = &ctx.error {
        lines.push(format!("Error: {err}"));
    }
    lines.push(format!("Checked at: {}", ctx.checked_at));

    let body_text = lines.join("\n");

    (subject, body_text, None)
}
