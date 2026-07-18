//! Production `HttpSender`: POSTs webhook/discord/ntfy notifications over a
//! shared `reqwest::Client` (rustls, no openssl/native-tls — see the
//! `rustls-tls` feature in `Cargo.toml`). `tests/notify_multi.rs` and
//! `tests/common` use `RecordingHttpSender` (`notify::mod`) instead of this
//! — the payload-building helpers below are unit-tested directly in this
//! module so the JSON shape is still covered without a live HTTP round
//! trip.

use std::collections::HashMap;

use serde_json::{json, Value};

use super::{HttpSender, NotifyMsg};

/// Default webhook JSON template (§7 message template variables). Channels
/// may override via `config.template`.
const DEFAULT_WEBHOOK_TEMPLATE: &str = r#"{"monitor":"{{monitor_name}}","url":"{{url}}","status":"{{status}}","status_code":"{{status_code}}","error":"{{error}}","response_time_ms":"{{response_time_ms}}","checked_at":"{{checked_at}}"}"#;

pub struct ReqwestHttpSender {
    client: reqwest::Client,
}

impl ReqwestHttpSender {
    pub fn new() -> Self {
        ReqwestHttpSender {
            client: reqwest::Client::new(),
        }
    }
}

impl Default for ReqwestHttpSender {
    fn default() -> Self {
        Self::new()
    }
}

/// Substitutes `{{var}}` placeholders (§7 template variables) in `template`
/// with `msg`'s fields, JSON-string-escaping each value first — the
/// template is expected to already quote each placeholder (as the default
/// template does), so the substituted text must be safe to sit *inside* a
/// JSON string literal, not a bare JSON value.
fn substitute_vars(template: &str, msg: &NotifyMsg) -> String {
    let opt = |v: Option<i64>| v.map(|v| v.to_string()).unwrap_or_default();
    let vars: HashMap<&str, String> = HashMap::from([
        ("monitor_name", msg.monitor_name.clone()),
        ("url", msg.url.clone()),
        ("status", msg.status.clone()),
        ("status_code", opt(msg.status_code)),
        ("error", msg.error.clone().unwrap_or_default()),
        ("response_time_ms", opt(msg.response_time_ms)),
        ("duration", opt(msg.duration)),
        ("ssl_days", opt(msg.ssl_days)),
        ("domain_days", opt(msg.domain_days)),
        ("checked_at", msg.checked_at.to_string()),
        ("incident_url", msg.incident_url.clone().unwrap_or_default()),
        ("subject", msg.subject.clone()),
        ("body", msg.body.clone()),
    ]);

    let mut out = template.to_string();
    for (key, value) in vars {
        // `serde_json::to_string` on a `String` yields a quoted, escaped
        // JSON string literal (e.g. `"a \"b\" c"`); strip the surrounding
        // quotes so it can be spliced into the template's own quotes.
        let escaped = serde_json::to_string(&value).unwrap_or_default();
        let inner = escaped.get(1..escaped.len().saturating_sub(1)).unwrap_or("");
        out = out.replace(&format!("{{{{{key}}}}}"), inner);
    }
    out
}

fn discord_color(status: &str) -> u32 {
    match status {
        "down" => 0xF2_6D_6D,
        "up" | "recovered" => 0x35_D0_7F,
        _ => 0xF5_A6_23,
    }
}

fn build_discord_payload(msg: &NotifyMsg) -> Value {
    json!({
        "content": msg.subject,
        "embeds": [{
            "title": msg.subject,
            "description": msg.body,
            "color": discord_color(&msg.status),
        }]
    })
}

#[async_trait::async_trait]
impl HttpSender for ReqwestHttpSender {
    async fn send(
        &self,
        channel_type: &str,
        config: &Value,
        msg: &NotifyMsg,
    ) -> anyhow::Result<()> {
        match channel_type {
            "webhook" => self.send_webhook(config, msg).await,
            "discord" => self.send_discord(config, msg).await,
            "ntfy" => self.send_ntfy(config, msg).await,
            other => anyhow::bail!("unsupported notification channel type: {other}"),
        }
    }
}

impl ReqwestHttpSender {
    async fn send_webhook(&self, config: &Value, msg: &NotifyMsg) -> anyhow::Result<()> {
        let url = config
            .get("url")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("webhook channel config missing 'url'"))?;
        let method = config
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or("POST")
            .to_uppercase();
        let template = config
            .get("template")
            .and_then(Value::as_str)
            .unwrap_or(DEFAULT_WEBHOOK_TEMPLATE);
        let body = substitute_vars(template, msg);

        let method: reqwest::Method = method.parse().unwrap_or(reqwest::Method::POST);
        let mut req = self.client.request(method, url);

        if let Some(headers) = config.get("headers").and_then(Value::as_object) {
            for (k, v) in headers {
                if let Some(vs) = v.as_str() {
                    req = req.header(k.as_str(), vs);
                }
            }
        }

        let resp = req
            .header("Content-Type", "application/json")
            .body(body)
            .send()
            .await?;

        if !resp.status().is_success() {
            anyhow::bail!("webhook returned {}", resp.status());
        }
        Ok(())
    }

    async fn send_discord(&self, config: &Value, msg: &NotifyMsg) -> anyhow::Result<()> {
        let url = config
            .get("webhook_url")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("discord channel config missing 'webhook_url'"))?;

        let resp = self
            .client
            .post(url)
            .json(&build_discord_payload(msg))
            .send()
            .await?;

        if !resp.status().is_success() {
            anyhow::bail!("discord webhook returned {}", resp.status());
        }
        Ok(())
    }

    async fn send_ntfy(&self, config: &Value, msg: &NotifyMsg) -> anyhow::Result<()> {
        let server = config
            .get("server")
            .and_then(Value::as_str)
            .unwrap_or("https://ntfy.sh");
        let topic = config
            .get("topic")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("ntfy channel config missing 'topic'"))?;
        let url = format!("{}/{}", server.trim_end_matches('/'), topic);

        let mut req = self.client.post(&url).header("Title", msg.subject.clone());
        if let Some(priority) = config.get("priority").and_then(Value::as_str) {
            req = req.header("Priority", priority.to_string());
        }
        if let Some(tags) = config.get("tags").and_then(Value::as_str) {
            req = req.header("Tags", tags.to_string());
        }
        if let Some(token) = config.get("token").and_then(Value::as_str) {
            req = req.header("Authorization", format!("Bearer {token}"));
        }

        let resp = req.body(msg.body.clone()).send().await?;
        if !resp.status().is_success() {
            anyhow::bail!("ntfy returned {}", resp.status());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_msg() -> NotifyMsg {
        NotifyMsg {
            monitor_name: "api".into(),
            url: "https://api.example.com".into(),
            status: "down".into(),
            status_code: Some(503),
            error: Some("connection refused".into()),
            response_time_ms: None,
            duration: None,
            ssl_days: None,
            domain_days: None,
            checked_at: 1_700_000_000,
            incident_url: None,
            subject: "🔴 api is DOWN".into(),
            body: "Monitor: api\nStatus: down".into(),
            body_html: None,
        }
    }

    #[test]
    fn webhook_default_template_is_valid_json_with_monitor_name() {
        let body = substitute_vars(DEFAULT_WEBHOOK_TEMPLATE, &sample_msg());
        let v: Value = serde_json::from_str(&body).expect("substituted template must be valid JSON");
        assert_eq!(v["monitor"], "api");
        assert_eq!(v["status"], "down");
        assert_eq!(v["status_code"], "503");
    }

    #[test]
    fn webhook_template_escapes_quotes_in_error() {
        let mut msg = sample_msg();
        msg.error = Some(r#"said "no""#.to_string());
        let body = substitute_vars(DEFAULT_WEBHOOK_TEMPLATE, &msg);
        let v: Value = serde_json::from_str(&body).expect("must still be valid JSON when the error contains quotes");
        assert_eq!(v["error"], r#"said "no""#);
    }

    #[test]
    fn discord_payload_has_embed_with_monitor_name() {
        let msg = sample_msg();
        let payload = build_discord_payload(&msg);
        assert_eq!(payload["content"], msg.subject);
        assert_eq!(payload["embeds"][0]["title"], msg.subject);
        assert_eq!(payload["embeds"][0]["description"], msg.body);
        assert_eq!(payload["embeds"][0]["color"], 0xF2_6D_6D);
    }
}
