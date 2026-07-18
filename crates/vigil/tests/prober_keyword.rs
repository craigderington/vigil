use vigil::models::*;

fn km(url: &str, keyword: &str, mode: &str, case_sensitive: bool) -> Monitor {
    let mut m = vigil::models::test_defaults_monitor();
    m.r#type = "keyword".to_string();
    m.url = Some(url.into());
    m.keyword = Some(keyword.into());
    m.keyword_mode = Some(mode.into());
    m.keyword_case_sensitive = case_sensitive;
    m
}

async fn server_with_body(body: &str) -> wiremock::MockServer {
    let s = wiremock::MockServer::start().await;
    wiremock::Mock::given(wiremock::matchers::method("GET"))
        .respond_with(wiremock::ResponseTemplate::new(200).set_body_string(body))
        .mount(&s)
        .await;
    s
}

#[tokio::test]
async fn present_found_is_ok() {
    let s = server_with_body("hello WORLD").await;
    let o = vigil::probe::http::probe(&km(&s.uri(), "hello", "present", false)).await;
    assert!(o.ok);
    assert_eq!(o.cause, None);
}

#[tokio::test]
async fn present_missing_is_keyword_failure() {
    let s = server_with_body("hello WORLD").await;
    let o = vigil::probe::http::probe(&km(&s.uri(), "missing", "present", false)).await;
    assert!(!o.ok);
    assert_eq!(o.cause, Some(Cause::Keyword));
}

#[tokio::test]
async fn absent_present_keyword_is_keyword_failure() {
    let s = server_with_body("hello WORLD").await;
    let o = vigil::probe::http::probe(&km(&s.uri(), "hello", "absent", false)).await;
    assert!(!o.ok);
    assert_eq!(o.cause, Some(Cause::Keyword));
}

#[tokio::test]
async fn absent_missing_keyword_is_ok() {
    let s = server_with_body("hello WORLD").await;
    let o = vigil::probe::http::probe(&km(&s.uri(), "missing", "absent", false)).await;
    assert!(o.ok);
    assert_eq!(o.cause, None);
}

#[tokio::test]
async fn case_insensitive_default_matches_world() {
    let s = server_with_body("hello WORLD").await;
    let o = vigil::probe::http::probe(&km(&s.uri(), "world", "present", false)).await;
    assert!(o.ok);
}

#[tokio::test]
async fn case_sensitive_requires_exact_match() {
    let s = server_with_body("hello WORLD").await;
    let o = vigil::probe::http::probe(&km(&s.uri(), "world", "present", true)).await;
    assert!(!o.ok);
    assert_eq!(o.cause, Some(Cause::Keyword));
}
