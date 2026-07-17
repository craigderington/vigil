//! Notification message templates: renders a `(subject, body_text,
//! body_html)` triple from a `Trigger` and `TemplateCtx`. §7 of the spec
//! ("Message template variables") — P1 emits plaintext only (`body_html`
//! is always `None`).

use crate::models::Trigger;
use crate::notify::TemplateCtx;

/// Renders the subject and body for a notification. `body_html` is always
/// `None` in P1 — HTML formatting is a later task.
pub fn render(trigger: Trigger, ctx: &TemplateCtx) -> (String, String, Option<String>) {
    let subject = match trigger {
        Trigger::Down => format!("🔴 {} is DOWN", ctx.monitor_name),
        Trigger::Recovered => format!("✅ {} recovered", ctx.monitor_name),
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
