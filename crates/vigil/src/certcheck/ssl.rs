//! SSL/TLS certificate check (§6 of the spec).
//!
//! Performs a real TLS handshake against `host:port` using a custom
//! [`rustls::client::danger::ServerCertVerifier`] that *always* accepts the
//! server's certificate chain — so the handshake completes even for
//! expired, self-signed, or otherwise untrusted certs — while capturing
//! the exact chain the server sent. The captured leaf certificate is then
//! parsed independently with `x509-parser`, hostname-matched per RFC 6125
//! against its SANs (falling back to the legacy CN only when no SANs are
//! present), and the chain is separately checked against the Mozilla root
//! store (`webpki-roots`) to determine trust.

use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;
use tokio_rustls::rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use tokio_rustls::rustls::client::verify_server_cert_signed_by_trust_anchor;
use tokio_rustls::rustls::crypto::CryptoProvider;
use tokio_rustls::rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use tokio_rustls::rustls::server::ParsedCertificate;
use tokio_rustls::rustls::{ClientConfig, DigitallySignedStruct, RootCertStore, SignatureScheme};
use x509_parser::prelude::*;

/// Outcome of a single SSL/TLS certificate check against `host:port`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SslResult {
    pub issuer: Option<String>,
    pub subject: Option<String>,
    pub valid_from: Option<i64>,
    pub valid_until: Option<i64>,
    pub days_remaining: Option<i64>,
    pub is_valid: bool,
    pub chain_ok: bool,
    pub hostname_match: bool,
    pub self_signed: bool,
    pub error: Option<String>,
}

impl SslResult {
    /// Builds the well-formed "couldn't check" result: every fact field is
    /// `None`/`false` and `error` carries the reason. Kept as a single
    /// constructor so every failure exit keeps the same shape.
    fn error(msg: impl Into<String>) -> Self {
        SslResult { error: Some(msg.into()), ..Default::default() }
    }
}

fn now_epoch() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs() as i64
}

/// A `ServerCertVerifier` that accepts any certificate/chain the server
/// presents (so invalid/expired/self-signed/untrusted certs can still be
/// inspected) while capturing exactly what was sent, for later independent
/// analysis. Signature verification (both TLS1.2 and TLS1.3) still
/// delegates to the real ring-backed algorithms so the handshake itself is
/// cryptographically sound — only *trust* is skipped, not the crypto.
#[derive(Debug)]
struct Capturing {
    provider: Arc<CryptoProvider>,
    captured: Arc<Mutex<Option<Vec<CertificateDer<'static>>>>>,
}

impl ServerCertVerifier for Capturing {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, tokio_rustls::rustls::Error> {
        let mut chain = Vec::with_capacity(1 + intermediates.len());
        chain.push(end_entity.clone().into_owned());
        chain.extend(intermediates.iter().map(|c| c.clone().into_owned()));
        *self.captured.lock().unwrap() = Some(chain);
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, tokio_rustls::rustls::Error> {
        tokio_rustls::rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, tokio_rustls::rustls::Error> {
        tokio_rustls::rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider.signature_verification_algorithms.supported_schemes()
    }
}

/// RFC 6125 hostname matching, pure (no I/O). `sans` are the certificate's
/// `dNSName` Subject Alternative Names; `cn` is the legacy Common Name,
/// consulted **only** when `sans` is empty (SANs, when present, are
/// authoritative — a non-matching SAN list must not fall back to CN). A
/// `*.` prefix matches exactly one leftmost label — it neither matches the
/// bare apex domain nor spans multiple labels. Comparison is
/// case-insensitive.
pub fn hostname_matches(sans: &[String], cn: Option<&str>, host: &str) -> bool {
    let host = host.to_ascii_lowercase();
    if !sans.is_empty() {
        return sans.iter().any(|san| pattern_matches(san, &host));
    }
    cn.is_some_and(|cn| pattern_matches(cn, &host))
}

fn pattern_matches(pattern: &str, host_lower: &str) -> bool {
    let pattern = pattern.to_ascii_lowercase();
    match pattern.strip_prefix("*.") {
        Some(rest) => match host_lower.split_once('.') {
            Some((_label, tail)) => tail == rest,
            None => false,
        },
        None => pattern == host_lower,
    }
}

/// Chain-only trust verification: does `leaf` (with `intermediates`) chain
/// to a trust anchor in `roots` as of `now`? Deliberately independent of
/// hostname — unlike `WebPkiServerVerifier::verify_server_cert` (which
/// checks chain-trust *and* hostname in one pass, via an internal
/// `verify_server_name` call), `verify_server_cert_signed_by_trust_anchor`
/// takes no `ServerName` at all, so this result can never be conflated with
/// `hostname_match`. `SslResult.chain_ok` and `SslResult.hostname_match`
/// are separate facts about the certificate and must be computed
/// independently — a CA-trusted cert served for the wrong hostname is a
/// hostname problem, not a broken chain.
pub fn verify_chain(
    leaf: &CertificateDer<'_>,
    intermediates: &[CertificateDer<'_>],
    roots: &RootCertStore,
    now: UnixTime,
    provider: &CryptoProvider,
) -> bool {
    let Ok(parsed) = ParsedCertificate::try_from(leaf) else {
        return false;
    };
    verify_server_cert_signed_by_trust_anchor(
        &parsed,
        roots,
        intermediates,
        now,
        provider.signature_verification_algorithms.all,
    )
    .is_ok()
}

/// Runs a TLS handshake against `host:port` (SNI = `host`), captures the
/// served certificate chain, and reports what it found. Never panics on
/// network/parse failure — any connect/handshake/parse error yields a
/// well-formed `SslResult` with `error` set and every fact field cleared.
pub async fn check(host: &str, port: u16, timeout_secs: u64) -> SslResult {
    let timeout = std::time::Duration::from_secs(timeout_secs);

    let tcp = match tokio::time::timeout(timeout, TcpStream::connect((host, port))).await {
        Ok(Ok(stream)) => stream,
        Ok(Err(e)) => return SslResult::error(format!("connect failed: {e}")),
        Err(_) => return SslResult::error(format!("timed out connecting to {host}:{port}")),
    };

    let provider = Arc::new(tokio_rustls::rustls::crypto::ring::default_provider());
    let captured: Arc<Mutex<Option<Vec<CertificateDer<'static>>>>> = Arc::new(Mutex::new(None));
    let capturing = Capturing { provider: provider.clone(), captured: captured.clone() };

    let client_config = match ClientConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()
    {
        Ok(b) => b
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(capturing))
            .with_no_client_auth(),
        Err(e) => return SslResult::error(format!("tls config error: {e}")),
    };
    let connector = TlsConnector::from(Arc::new(client_config));

    // Owned ServerName: `try_from(&str)` yields a borrowed name that fails
    // the connector's `ServerName<'static>` bound.
    let server_name = match ServerName::try_from(host.to_owned()) {
        Ok(n) => n,
        Err(e) => return SslResult::error(format!("invalid server name {host:?}: {e}")),
    };

    match tokio::time::timeout(timeout, connector.connect(server_name.clone(), tcp)).await {
        Ok(Ok(_tls)) => {}
        Ok(Err(e)) => return SslResult::error(format!("tls handshake failed: {e}")),
        Err(_) => return SslResult::error(format!("timed out during tls handshake to {host}:{port}")),
    }

    let Some(chain) = captured.lock().unwrap().take() else {
        return SslResult::error("tls handshake completed but no certificate chain was captured");
    };
    let Some(leaf_der) = chain.first() else {
        return SslResult::error("server presented an empty certificate chain");
    };

    let leaf = match parse_x509_certificate(leaf_der.as_ref()) {
        Ok((_, cert)) => cert,
        Err(e) => return SslResult::error(format!("failed to parse leaf certificate: {e}")),
    };

    let issuer = leaf.issuer().to_string();
    let subject = leaf.subject().to_string();
    let valid_from = leaf.validity().not_before.timestamp();
    let valid_until = leaf.validity().not_after.timestamp();
    // Floor division (not truncation) so a cert that expired within the
    // last 24h reports a negative `days_remaining` instead of `0` — `0`
    // would misleadingly read as "still valid, expires today."
    let days_remaining = (valid_until - now_epoch()).div_euclid(86_400);

    let sans: Vec<String> = leaf
        .subject_alternative_name()
        .ok()
        .flatten()
        .map(|ext| {
            ext.value
                .general_names
                .iter()
                .filter_map(|gn| match gn {
                    GeneralName::DNSName(s) => Some((*s).to_string()),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default();
    let cn = leaf.subject().iter_common_name().next().and_then(|atv| atv.as_str().ok());
    let hostname_match = hostname_matches(&sans, cn, host);

    let self_signed = issuer == subject || chain.len() == 1;

    // Chain trust against the Mozilla root store, computed independently
    // of hostname via `verify_chain` (see its doc comment) — using the
    // explicit ring provider (NOT the bare `builder()`, whose provider
    // resolves from crate features and would panic if a second crypto
    // provider ever leaked into the build).
    let mut roots = RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let chain_ok = verify_chain(leaf_der, &chain[1..], &roots, UnixTime::now(), &provider);

    let now = now_epoch();
    let is_valid = now >= valid_from && now <= valid_until && chain_ok && hostname_match;

    SslResult {
        issuer: Some(issuer),
        subject: Some(subject),
        valid_from: Some(valid_from),
        valid_until: Some(valid_until),
        days_remaining: Some(days_remaining),
        is_valid,
        chain_ok,
        hostname_match,
        self_signed,
        error: None,
    }
}
