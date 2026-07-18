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
#[tokio::test]
async fn default_user_agent_is_sent() {
    // A monitor with no custom headers must send Vigil's default identifying
    // User-Agent, so WAF/bot-protected sites (which 4xx UA-less clients) don't
    // read as a false DOWN.
    let s = wiremock::MockServer::start().await;
    wiremock::Mock::given(wiremock::matchers::any())
        .respond_with(wiremock::ResponseTemplate::new(200))
        .mount(&s)
        .await;

    let o = vigil::probe::http::probe(&m(&s.uri(), "200-299")).await;
    assert!(o.ok);

    let reqs = s.received_requests().await.unwrap();
    let ua_values: Vec<&str> = reqs[0]
        .headers
        .get_all("user-agent")
        .iter()
        .map(|v| v.to_str().unwrap())
        .collect();
    assert_eq!(ua_values.len(), 1, "exactly one User-Agent header, got {ua_values:?}");
    assert_eq!(ua_values[0], vigil::probe::http::DEFAULT_USER_AGENT);
}

#[tokio::test]
async fn custom_user_agent_overrides_default() {
    // A monitor that sets its own User-Agent header must send exactly that
    // (not the default, and not two User-Agent headers).
    let s = wiremock::MockServer::start().await;
    wiremock::Mock::given(wiremock::matchers::any())
        .respond_with(wiremock::ResponseTemplate::new(200))
        .mount(&s)
        .await;

    let mut mon = m(&s.uri(), "200-299");
    mon.headers = Some(r#"{"User-Agent":"custom-agent/9"}"#.into());
    let o = vigil::probe::http::probe(&mon).await;
    assert!(o.ok);

    let reqs = s.received_requests().await.unwrap();
    let ua_values: Vec<&str> = reqs[0]
        .headers
        .get_all("user-agent")
        .iter()
        .map(|v| v.to_str().unwrap())
        .collect();
    assert_eq!(ua_values, vec!["custom-agent/9"], "custom UA must win with no duplicate");
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
