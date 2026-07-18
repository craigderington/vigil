//! `Transport` impl that actually sends mail over SMTP, via `lettre`
//! (rustls, no openssl/native-tls). Not exercised by the test suite
//! (tests use `RecordingTransport`) — this is the P1 production path.

use lettre::message::header::ContentType;
use lettre::transport::smtp::authentication::Credentials;
use lettre::{AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor};

use super::{EmailMsg, SmtpConfig, Transport};

/// SMTP sender. Holds the account password (if any) separately from
/// `SmtpConfig` — the config lives in the DB, the password comes from the
/// OS keychain (§12) and is never persisted alongside it.
pub struct SmtpTransport {
    password: Option<String>,
}

impl SmtpTransport {
    pub fn new(password: Option<String>) -> Self {
        SmtpTransport { password }
    }
}

#[async_trait::async_trait]
impl Transport for SmtpTransport {
    async fn send(&self, cfg: &SmtpConfig, msg: &EmailMsg) -> anyhow::Result<()> {
        let builder = match cfg.security.as_str() {
            "none" => AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(&cfg.host)
                .port(cfg.port),
            "starttls" => AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(&cfg.host)?
                .port(cfg.port),
            _ => AsyncSmtpTransport::<Tokio1Executor>::relay(&cfg.host)?.port(cfg.port),
        };

        let builder = if let Some(password) = &self.password {
            builder.credentials(Credentials::new(
                super::auth_user(&cfg.username, &msg.from),
                password.clone(),
            ))
        } else {
            builder
        };

        let transport: AsyncSmtpTransport<Tokio1Executor> = builder.build();

        let mut message_builder = Message::builder()
            .from(msg.from.parse()?)
            .subject(&msg.subject)
            .header(ContentType::TEXT_PLAIN);

        for to in &msg.to {
            message_builder = message_builder.to(to.parse()?);
        }

        let message = message_builder.body(msg.body_text.clone())?;

        AsyncTransport::send(&transport, message).await?;

        Ok(())
    }
}
