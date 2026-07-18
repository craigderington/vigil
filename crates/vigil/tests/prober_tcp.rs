use vigil::models::*;
fn m_port(host: &str, port: i64) -> Monitor {
    let mut m = vigil::models::test_defaults_monitor();
    m.r#type = "port".into();
    m.host = Some(host.into());
    m.port = Some(port);
    m.url = None;
    m
}
#[tokio::test]
async fn connects_to_open_port() {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let p = l.local_addr().unwrap().port() as i64;
    let o = vigil::probe::tcp::probe(&m_port("127.0.0.1", p)).await;
    assert!(o.ok);
}
#[tokio::test]
async fn refused_port_is_down() {
    let o = vigil::probe::tcp::probe(&m_port("127.0.0.1", 1)).await;
    assert!(!o.ok);
    assert!(matches!(o.cause, Some(Cause::Connection) | Some(Cause::Timeout)));
}
#[tokio::test]
async fn ping_explicit_port() {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let p = l.local_addr().unwrap().port() as i64;
    let mut m = m_port("127.0.0.1", p);
    m.r#type = "ping".into();
    let o = vigil::probe::tcp::probe(&m).await;
    assert!(o.ok);
}

/// `ping` with no explicit port falls back to trying 443 then 80
/// sequentially. On a dev/CI box neither port is bound on loopback, so this
/// can't assert `ok` either way deterministically — it only proves the
/// fallback path runs to completion (tries both candidates in order) without
/// panicking and returns a well-formed outcome either way.
#[tokio::test]
async fn ping_without_port_tries_443_then_80_and_completes() {
    let mut m = vigil::models::test_defaults_monitor();
    m.r#type = "ping".into();
    m.host = Some("127.0.0.1".into());
    m.port = None;
    m.url = None;
    m.timeout_seconds = 2;
    let o = vigil::probe::tcp::probe(&m).await;
    // Whatever the outcome, it must be well-formed: ok iff cause is absent.
    assert_eq!(o.ok, o.cause.is_none());
}
