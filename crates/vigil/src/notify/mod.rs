//! The notification `Transport`/`HttpSender` seam: `EmailMsg`/`SmtpConfig`/
//! `TemplateCtx`/`NotifyMsg`/`AlertCtx` shared types, the `Transport` and
//! `HttpSender` traits, and `RecordingTransport`/`RecordingHttpSender` test
//! doubles. `dispatch::deliver` is the multi-channel core: it routes a
//! rendered `NotifyMsg` to every attached channel regardless of type (email
//! via `Transport`, everything else via `HttpSender`).

use std::sync::{Arc, Mutex};

use serde::Serialize;

pub mod dispatch;
pub mod email;
pub mod http;
pub mod templates;

pub use email::SmtpTransport;
pub use http::ReqwestHttpSender;

#[derive(Clone, Debug, PartialEq)]
pub struct EmailMsg {
    pub to: Vec<String>,
    pub from: String,
    pub subject: String,
    pub body_text: String,
    pub body_html: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SmtpConfig {
    pub host: String,
    pub port: u16,
    pub security: String, // none|starttls|tls
    pub username: Option<String>,
}

#[async_trait::async_trait]
pub trait Transport: Send + Sync {
    async fn send(&self, cfg: &SmtpConfig, msg: &EmailMsg) -> anyhow::Result<()>;
}

/// The credential-selection rule for SMTP auth: prefer the channel's
/// configured username; fall back to the From address when no username is
/// set (the common case for most SMTP relays). `SmtpConfig` has no `from`
/// field of its own — the From address lives on `EmailMsg` — so this takes
/// both explicitly rather than reaching into a combined struct.
pub fn auth_user(username: &Option<String>, from: &str) -> String {
    username.clone().unwrap_or_else(|| from.to_string())
}

/// A fully-rendered, channel-agnostic notification: carries both the
/// rendered `subject`/`body`/`body_html` (used directly by the email arm of
/// `dispatch::deliver`) and the raw §7 template variables (used by
/// `HttpSender` impls to fill a webhook/discord/ntfy payload template).
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct NotifyMsg {
    pub monitor_name: String,
    pub url: String,
    pub status: String,
    pub status_code: Option<i64>,
    pub error: Option<String>,
    pub response_time_ms: Option<i64>,
    pub duration: Option<i64>,
    pub ssl_days: Option<i64>,
    pub domain_days: Option<i64>,
    pub checked_at: crate::models::Ts,
    pub incident_url: Option<String>,
    pub subject: String,
    pub body: String,
    pub body_html: Option<String>,
}

/// Render context for the SSL/domain add-on alerts (`SslExpiring`,
/// `SslInvalid`, `DomainExpiring`) — the certificate/domain-expiry
/// counterpart of `TemplateCtx`.
#[derive(Clone, Debug)]
pub struct AlertCtx {
    pub monitor_name: String,
    pub url: String,
    pub ssl_days: Option<i64>,
    pub domain_days: Option<i64>,
    pub error: Option<String>,
    pub checked_at: crate::models::Ts,
}

/// A generic HTTP-based notification sender: webhook/discord/ntfy today,
/// any future push channel later. `config` is the channel's raw
/// `notification_channels.config` JSON; `channel_type` picks the payload
/// shape.
#[async_trait::async_trait]
pub trait HttpSender: Send + Sync {
    async fn send(
        &self,
        channel_type: &str,
        config: &serde_json::Value,
        msg: &NotifyMsg,
    ) -> anyhow::Result<()>;
}

pub struct RecordingHttpSender {
    pub sent_http: Arc<Mutex<Vec<(String, serde_json::Value, NotifyMsg)>>>,
}

#[async_trait::async_trait]
impl HttpSender for RecordingHttpSender {
    async fn send(
        &self,
        channel_type: &str,
        config: &serde_json::Value,
        msg: &NotifyMsg,
    ) -> anyhow::Result<()> {
        self.sent_http
            .lock()
            .unwrap()
            .push((channel_type.to_string(), config.clone(), msg.clone()));
        Ok(())
    }
}

#[derive(Clone, Debug)]
pub struct TemplateCtx {
    pub monitor_name: String,
    pub url: String,
    pub status: String,
    pub status_code: Option<i64>,
    pub error: Option<String>,
    pub response_time_ms: Option<i64>,
    pub duration: Option<i64>,
    pub checked_at: crate::models::Ts,
}

pub struct RecordingTransport {
    pub sent: Arc<Mutex<Vec<(SmtpConfig, EmailMsg)>>>,
}

#[async_trait::async_trait]
impl Transport for RecordingTransport {
    async fn send(&self, cfg: &SmtpConfig, msg: &EmailMsg) -> anyhow::Result<()> {
        self.sent.lock().unwrap().push((cfg.clone(), msg.clone()));
        Ok(())
    }
}
