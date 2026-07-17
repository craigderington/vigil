use vigil::models::*;
fn m(url: &str, codes: &str) -> Monitor {
    let mut m = vigil::models::test_defaults_monitor();
    m.url = Some(url.into());
    m.expected_status_codes = codes.into();
    m
} // small pub helper in models for tests
#[tokio::test]
async fn success_when_expected() {
    let s = wiremock::MockServer::start().await;
    wiremock::Mock::given(wiremock::matchers::method("GET"))
        .respond_with(wiremock::ResponseTemplate::new(200))
        .mount(&s)
        .await;
    let o = vigil::probe::http::probe(&m(&s.uri(), "200-299")).await;
    assert!(o.ok);
    assert_eq!(o.status_code, Some(200));
}
#[tokio::test]
async fn wrong_status_is_cause_status() {
    let s = wiremock::MockServer::start().await;
    wiremock::Mock::given(wiremock::matchers::any())
        .respond_with(wiremock::ResponseTemplate::new(503))
        .mount(&s)
        .await;
    let o = vigil::probe::http::probe(&m(&s.uri(), "200-299")).await;
    assert!(!o.ok);
    assert_eq!(o.cause, Some(Cause::Status));
}
#[tokio::test]
async fn refused_is_connection_or_dns() {
    let o = vigil::probe::http::probe(&m("http://127.0.0.1:1", "200-299")).await;
    assert!(!o.ok);
    assert!(matches!(o.cause, Some(Cause::Connection) | Some(Cause::Dns)));
}
#[test]
fn resolve_auth_forms() {
    std::env::set_var("VIGIL_TEST_TOK", "secret");
    assert_eq!(
        vigil::probe::http::resolve_auth(&Some("env:VIGIL_TEST_TOK".into())),
        Some("secret".into())
    );
    assert_eq!(
        vigil::probe::http::resolve_auth(&Some("inline:abc".into())),
        Some("abc".into())
    );
}
