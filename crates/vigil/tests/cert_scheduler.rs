//! Tests for `vigil::cert_scheduler`: the fire-once bookkeeping is the
//! crux (see the module doc), so these tests are deliberately structured
//! to prove it directly rather than only exercising it incidentally.
//!
//! - `refresh_ssl_persists_and_alerts`: a real TLS handshake against a
//!   local self-signed server (same harness as `certcheck_ssl.rs`) drives
//!   `refresh_ssl` end-to-end for an http-type monitor with
//!   `ssl_check_enabled` (NOT `type='ssl'`, so `ssl_invalid` CAN fire — an
//!   `ssl`-type monitor surfaces invalidity via its own down path
//!   instead), proving the `ssl_certs` row is persisted AND the
//!   `ssl_invalid` alert is dispatched.
//! - `ssl_expiring_fires_once` / `domain_expiring_fires_once`: call the
//!   pure-ish `evaluate_*_alerts` step directly, twice, with the same
//!   `days_remaining` — proving fire-once without needing to control a
//!   live cert/domain's expiry.

mod common;
use common::*;

use std::sync::Arc;

use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;
use tokio_rustls::rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use tokio_rustls::rustls::{self, ServerConfig};

use vigil::cert_scheduler;
use vigil::models::Monitor;

const SEED_TS: i64 = 1_700_000_000;

async fn insert_http_monitor_with_ssl(pool: &sqlx::SqlitePool, url: &str) -> i64 {
    sqlx::query_scalar(
        "INSERT INTO monitors (name, type, url, ssl_check_enabled, created_at, updated_at) \
         VALUES (?, 'http', ?, 1, ?, ?) RETURNING id",
    )
    .bind("ssl-seed")
    .bind(url)
    .bind(SEED_TS)
    .bind(SEED_TS)
    .fetch_one(pool)
    .await
    .unwrap()
}

async fn fetch_monitor(pool: &sqlx::SqlitePool, id: i64) -> Monitor {
    sqlx::query_as("SELECT * FROM monitors WHERE id = ?")
        .bind(id)
        .fetch_one(pool)
        .await
        .unwrap()
}

/// Spins up a local TLS server on 127.0.0.1 with a freshly minted
/// self-signed cert (SAN = "localhost"), accepts exactly one connection,
/// and returns its port. Identical harness to
/// `certcheck_ssl.rs::local_self_signed_cert`. Because `refresh_ssl` will
/// connect by IP (127.0.0.1) rather than by the cert's SAN, both
/// `hostname_match` and `chain_ok` come back false — i.e. `is_valid` is
/// false with `error: None` (handshake completes), exactly the
/// captured-but-invalid case `ssl_invalid` is meant to catch.
async fn spawn_self_signed_server() -> u16 {
    let rcgen::CertifiedKey { cert, key_pair } =
        rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).expect("generate self-signed cert");

    let cert_der: CertificateDer<'static> = cert.der().clone();
    let key_der: PrivateKeyDer<'static> = PrivatePkcs8KeyDer::from(key_pair.serialize_der()).into();

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
async fn refresh_ssl_persists_and_alerts() {
    let env = test_state().await; // anchor Online
    let port = spawn_self_signed_server().await;
    let url = format!("https://127.0.0.1:{port}");
    let mid = insert_http_monitor_with_ssl(&env.state.db, &url).await;
    attach_webhook_channel(&env.state.db, mid, r#"["ssl_invalid"]"#).await;

    cert_scheduler::refresh_ssl(&env.state, mid).await.unwrap();

    let (is_valid, last_checked): (i64, Option<i64>) =
        sqlx::query_as("SELECT is_valid, last_checked FROM ssl_certs WHERE monitor_id = ?")
            .bind(mid)
            .fetch_one(&env.state.db)
            .await
            .unwrap();
    assert_eq!(is_valid, 0, "connecting by IP against a cert SANed for localhost must persist as invalid");
    assert!(last_checked.is_some(), "the bookkeeping UPDATE must always advance last_checked");

    let sent = env.sent_http.lock().unwrap();
    assert_eq!(
        sent.len(),
        1,
        "ssl_invalid must fire once for a captured-but-invalid cert on a non-ssl-type monitor"
    );
    assert_eq!(sent[0].0, "webhook");
}

#[tokio::test]
async fn ssl_expiring_fires_once() {
    let env = test_state().await;
    let mid = insert_http_monitor_with_ssl(&env.state.db, "https://example.com").await;
    attach_webhook_channel(&env.state.db, mid, r#"["ssl_expiring"]"#).await;
    let m = fetch_monitor(&env.state.db, mid).await;

    // First eval: never alerted, 5 days remaining crosses the default
    // [30,14,7,3,1] ssl_alert_days at the 7-day tier -> fires, sets
    // alerted_days = Some(7).
    let (new_alerted_1, new_invalid_1) =
        cert_scheduler::evaluate_ssl_alerts(&env.state, &m, Some(5), true, &None, None, false).await;
    assert_eq!(new_alerted_1, Some(7));
    assert!(!new_invalid_1);

    // Second eval: same 5 days remaining, old_alerted now Some(7) — no
    // lower tier (3, 1) is crossed (5 > 3), so this must NOT fire again.
    let (new_alerted_2, _) =
        cert_scheduler::evaluate_ssl_alerts(&env.state, &m, Some(5), true, &None, new_alerted_1, false).await;
    assert_eq!(new_alerted_2, Some(7));

    let count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM notification_log WHERE monitor_id = ? AND trigger = 'ssl_expiring'",
    )
    .bind(mid)
    .fetch_one(&env.state.db)
    .await
    .unwrap();
    assert_eq!(count, 1, "ssl_expiring must fire exactly once across two evals at the same tier, not twice");
}

#[tokio::test]
async fn domain_expiring_fires_once() {
    let env = test_state().await;
    let mid: i64 = sqlx::query_scalar(
        "INSERT INTO monitors (name, type, url, domain_check_enabled, created_at, updated_at) \
         VALUES (?, 'http', ?, 1, ?, ?) RETURNING id",
    )
    .bind("domain-seed")
    .bind("https://example.com")
    .bind(SEED_TS)
    .bind(SEED_TS)
    .fetch_one(&env.state.db)
    .await
    .unwrap();
    attach_webhook_channel(&env.state.db, mid, r#"["domain_expiring"]"#).await;
    let m = fetch_monitor(&env.state.db, mid).await;

    // Default domain_alert_days = [45,30,14,7]; 10 days remaining crosses
    // the 14-day tier first.
    let new_alerted_1 = cert_scheduler::evaluate_domain_alerts(&env.state, &m, Some(10), None).await;
    assert_eq!(new_alerted_1, Some(14));

    let new_alerted_2 = cert_scheduler::evaluate_domain_alerts(&env.state, &m, Some(10), new_alerted_1).await;
    assert_eq!(new_alerted_2, Some(14));

    let count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM notification_log WHERE monitor_id = ? AND trigger = 'domain_expiring'",
    )
    .bind(mid)
    .fetch_one(&env.state.db)
    .await
    .unwrap();
    assert_eq!(count, 1, "domain_expiring must fire exactly once across two evals at the same tier, not twice");
}
