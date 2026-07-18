//! Tests for `vigil::certcheck::ssl`:
//! - `hostname_matches` — pure RFC 6125 vectors.
//! - `check` — a real TLS handshake against a local self-signed server,
//!   proving the capturing verifier lets the handshake complete even
//!   though the cert isn't trusted, and that the result is parsed
//!   correctly.
//! - an optional, network-gated smoke test against a real public host.

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;
use tokio_rustls::rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, UnixTime};
use tokio_rustls::rustls::{self, RootCertStore, ServerConfig};

use vigil::certcheck::ssl;

fn now_epoch() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs() as i64
}

// ---- hostname_matches (pure, RFC 6125) ----

#[test]
fn hostname_matches_exact() {
    assert!(ssl::hostname_matches(&["example.com".to_string()], None, "example.com"));
}

#[test]
fn hostname_matches_wildcard_single_label() {
    assert!(ssl::hostname_matches(&["*.example.com".to_string()], None, "a.example.com"));
}

#[test]
fn hostname_matches_wildcard_does_not_match_apex() {
    assert!(!ssl::hostname_matches(&["*.example.com".to_string()], None, "example.com"));
}

#[test]
fn hostname_matches_wildcard_does_not_span_multiple_labels() {
    assert!(!ssl::hostname_matches(&["*.example.com".to_string()], None, "a.b.example.com"));
}

#[test]
fn hostname_matches_is_case_insensitive() {
    assert!(ssl::hostname_matches(&["EXAMPLE.com".to_string()], None, "example.COM"));
    assert!(ssl::hostname_matches(&["*.EXAMPLE.com".to_string()], None, "a.example.COM"));
}

#[test]
fn hostname_matches_cn_fallback_only_when_sans_empty() {
    // SANs present but non-matching -> CN must NOT be consulted.
    assert!(!ssl::hostname_matches(
        &["other.com".to_string()],
        Some("example.com"),
        "example.com"
    ));
    // SANs empty -> CN fallback applies.
    assert!(ssl::hostname_matches(&[], Some("example.com"), "example.com"));
    // SANs empty, no CN either -> no match.
    assert!(!ssl::hostname_matches(&[], None, "example.com"));
}

// ---- check() against a local self-signed server ----

/// Spins up a `tokio-rustls` TLS server on 127.0.0.1 with a freshly minted
/// self-signed cert (SAN = "localhost"), then points `ssl::check` at it by
/// IP. Because the connection is made by IP rather than by the cert's SAN,
/// hostname matching is expected to fail — the test only asserts on
/// `self_signed`, `chain_ok`, `valid_until`, and `error`.
#[tokio::test]
async fn local_self_signed_cert() {
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
            // Complete the server-side handshake; no application data is
            // needed for the client's handshake future to resolve.
            let _ = acceptor.accept(stream).await;
        }
    });

    let result = ssl::check("127.0.0.1", port, 5).await;

    assert_eq!(result.error, None, "handshake should complete via the capturing verifier");
    assert!(result.self_signed, "single-cert chain issued to itself is self-signed");
    assert!(!result.chain_ok, "a self-signed cert is not in the Mozilla root store");
    assert!(result.valid_until.is_some());
    assert!(
        result.valid_until.unwrap() > now_epoch(),
        "freshly minted cert should not be expired"
    );
}

// ---- verify_chain: chain_ok decoupled from hostname_match (Finding 1) ----

/// Proves `chain_ok` and `hostname_match` are independent facts, entirely
/// offline (no network, no real TLS handshake): a self-signed cert added to
/// a `RootCertStore` *as its own trust anchor* verifies as chain-trusted
/// via `ssl::verify_chain` — while that same cert's SAN (`localhost`)
/// plainly does not match an unrelated hostname (`127.0.0.1`) per
/// `ssl::hostname_matches`. A chain-trusted-but-wrong-hostname cert must
/// read as `chain_ok: true, hostname_match: false`, never as a broken
/// chain — this is the real regression guard for Finding 1 (previously,
/// `chain_ok` was computed via `WebPkiServerVerifier::verify_server_cert`,
/// which checks chain trust *and* hostname in one pass, so a wrong-host
/// cert always read as `chain_ok: false` even when genuinely CA-trusted).
#[test]
fn verify_chain_is_independent_of_hostname() {
    let rcgen::CertifiedKey { cert, .. } =
        rcgen::generate_simple_self_signed(vec!["localhost".to_string()])
            .expect("generate self-signed cert");
    let cert_der: CertificateDer<'static> = cert.der().clone();

    let provider = rustls::crypto::ring::default_provider();

    // The leaf IS the trust anchor: a self-signed cert added to its own
    // RootCertStore must verify as chain-trusted, with zero intermediates.
    let mut own_root = RootCertStore::empty();
    own_root.add(cert_der.clone()).expect("self-signed cert is a well-formed trust anchor");
    assert!(
        ssl::verify_chain(&cert_der, &[], &own_root, UnixTime::now(), &provider),
        "a self-signed cert trusted as its own root anchor must chain-verify"
    );

    // Same cert, checked against a hostname NOT in its SANs: this must be
    // false, but must NOT have influenced the chain_ok result above.
    assert!(
        !ssl::hostname_matches(&["localhost".to_string()], None, "127.0.0.1"),
        "the cert's SAN (localhost) does not match 127.0.0.1"
    );

    // Same cert, checked against an unrelated (empty) root store: this
    // must be false — proving verify_chain isn't vacuously true.
    let unrelated_roots = RootCertStore::empty();
    assert!(
        !ssl::verify_chain(&cert_der, &[], &unrelated_roots, UnixTime::now(), &provider),
        "a root store that doesn't contain the cert (or its issuer) must not trust it"
    );
}

// ---- optional live smoke test (network-gated) ----

/// Best-effort smoke test against a real public host. Never fails the
/// suite when the network is unavailable (sandboxed/offline CI) — it logs
/// and returns instead.
#[tokio::test]
async fn live_smoke_one_dot_one() {
    let result = ssl::check("one.one.one.one", 443, 5).await;
    if let Some(err) = &result.error {
        eprintln!("live_smoke_one_dot_one: skipping assertions, network check failed: {err}");
        return;
    }
    assert!(result.is_valid, "expected a valid cert from a well-known public host");
    assert!(result.days_remaining.unwrap_or(0) > 0);
}
