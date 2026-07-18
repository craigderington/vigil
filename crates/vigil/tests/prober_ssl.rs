//! Task 6: `ssl` monitor type — the cert check drives the same up/down
//! state machine as every other monitor type, via `worker::run_check`.
//!
//! Spins up a local self-signed TLS server (same rcgen harness as
//! `tests/certcheck_ssl.rs`), persists an `ssl`-type monitor pointed at it
//! with `confirmation_threshold = 1`, and runs one `worker::run_check`
//! cycle. Asserts:
//! 1. an `ssl_certs` row was written with cert data populated, and
//!    `last_checked IS NULL` — proving the fast per-interval probe left the
//!    cadence marker alone for `cert_scheduler` (Task 7) to still see this
//!    monitor as "due" for the slow 12h expiry evaluation.
//! 2. the monitor's status went to `down` (self-signed → `is_valid=false`
//!    → `Cause::Ssl`, confirmed immediately since threshold = 1).

mod common;
use common::*;

use std::sync::Arc;

use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;
use tokio_rustls::rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use tokio_rustls::rustls::{self, ServerConfig};

/// Spins up a TLS server on `127.0.0.1` with a freshly minted self-signed
/// cert (SAN = "localhost") and returns its port. Accepts exactly one
/// connection then exits.
async fn spawn_self_signed_server() -> u16 {
    let rcgen::CertifiedKey { cert, key_pair } =
        rcgen::generate_simple_self_signed(vec!["localhost".to_string()])
            .expect("generate self-signed cert");

    let cert_der: CertificateDer<'static> = cert.der().clone();
    let key_der: PrivateKeyDer<'static> =
        PrivatePkcs8KeyDer::from(key_pair.serialize_der()).into();

    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let server_config = ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .expect("protocol versions")
        .with_no_client_auth()
        .with_single_cert(vec![cert_der], key_der)
        .expect("attach self-signed cert+key");
    let acceptor = TlsAcceptor::from(Arc::new(server_config));

    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let port = listener.local_addr().expect("local_addr").port();

    tokio::spawn(async move {
        if let Ok((stream, _)) = listener.accept().await {
            let _ = acceptor.accept(stream).await;
        }
    });

    port
}

#[tokio::test]
async fn ssl_monitor_run_check_persists_cert_and_goes_down() {
    let port = spawn_self_signed_server().await;
    let env = test_state().await;

    // confirmation_threshold = 1 so a single failed probe (self-signed,
    // connected by IP so hostname_match also fails) confirms DOWN
    // immediately.
    let mid: i64 = sqlx::query_scalar(
        "INSERT INTO monitors (name, type, host, port, confirmation_threshold, recovery_threshold, \
         retry_interval_seconds, status, created_at, updated_at) \
         VALUES ('ssl-test', 'ssl', '127.0.0.1', ?, 1, 1, 30, 'pending', 0, 0) RETURNING id",
    )
    .bind(port as i64)
    .fetch_one(&env.state.db)
    .await
    .unwrap();

    vigil::worker::run_check(&env.state, mid).await;

    // (1) ssl_certs row populated, last_checked left untouched (NULL) —
    // this is the cadence-ownership invariant cert_scheduler depends on.
    let row: (Option<String>, Option<i64>, Option<i64>, Option<i64>) = sqlx::query_as(
        "SELECT issuer, valid_until, is_valid, last_checked FROM ssl_certs WHERE monitor_id = ?",
    )
    .bind(mid)
    .fetch_one(&env.state.db)
    .await
    .expect("ssl_certs row must exist after run_check");

    let (issuer, valid_until, is_valid, last_checked) = row;
    assert!(issuer.is_some(), "cert issuer should be populated");
    assert!(valid_until.is_some(), "cert valid_until should be populated");
    assert_eq!(is_valid, Some(0), "self-signed cert connected by IP is not valid");
    assert_eq!(
        last_checked, None,
        "persist_ssl must NOT write last_checked — that's cert_scheduler's cadence marker (Task 7)"
    );

    // (2) monitor status went down (Cause::Ssl, confirmed at threshold=1).
    let status: String = sqlx::query_scalar("SELECT status FROM monitors WHERE id = ?")
        .bind(mid)
        .fetch_one(&env.state.db)
        .await
        .unwrap();
    assert_eq!(status, "down");

    let cause: Option<String> =
        sqlx::query_scalar("SELECT cause FROM incidents WHERE monitor_id = ? ORDER BY id DESC LIMIT 1")
            .bind(mid)
            .fetch_one(&env.state.db)
            .await
            .unwrap();
    assert_eq!(cause.as_deref(), Some("ssl"));
}
