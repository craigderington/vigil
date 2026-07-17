//! The notification `Transport` seam: `EmailMsg`/`SmtpConfig`/`TemplateCtx`
//! shared types, the `Transport` trait, and a `RecordingTransport` test
//! double. Task 11 adds `SmtpTransport`, message templates, and the
//! dispatch logic that turns state-machine transitions into sent mail.

use std::sync::{Arc, Mutex};

pub mod dispatch;
pub mod email;
pub mod templates;

pub use email::SmtpTransport;

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
}

#[async_trait::async_trait]
pub trait Transport: Send + Sync {
    async fn send(&self, cfg: &SmtpConfig, msg: &EmailMsg) -> anyhow::Result<()>;
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
