mod common;
use common::*;

async fn serve(state: vigil::app::AppState) -> std::net::SocketAddr {
    let app = vigil::app::router(state);
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let a = l.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(l, app).await.unwrap();
    });
    a
}

#[tokio::test]
async fn port_type_without_host_is_422() {
    let env = test_state().await;
    let a = serve(env.state.clone()).await;
    let r = reqwest::Client::new()
        .post(format!("http://{a}/api/monitors"))
        .json(&serde_json::json!({"name":"p","type":"port"}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 422);
}

#[tokio::test]
async fn dns_type_without_record_type_is_422() {
    let env = test_state().await;
    let a = serve(env.state.clone()).await;
    let r = reqwest::Client::new()
        .post(format!("http://{a}/api/monitors"))
        .json(&serde_json::json!({"name":"d","type":"dns","host":"x"}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 422);
}

#[tokio::test]
async fn http_type_without_url_is_422() {
    let env = test_state().await;
    let a = serve(env.state.clone()).await;
    let r = reqwest::Client::new()
        .post(format!("http://{a}/api/monitors"))
        .json(&serde_json::json!({"name":"h","type":"http"}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 422);
}

#[tokio::test]
async fn port_type_with_host_and_port_is_200() {
    let env = test_state().await;
    let a = serve(env.state.clone()).await;
    let r = reqwest::Client::new()
        .post(format!("http://{a}/api/monitors"))
        .json(&serde_json::json!({"name":"ok","type":"port","host":"h","port":80}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
}

/// Proves the dispatcher (`probe::run`) is actually wired through the
/// `/test-check` endpoint. The `tcp` prober is still a placeholder in this
/// task (Task 3 fills it in), so this exercises the `http` arm of the
/// dispatch instead of `port` — but it goes through `probe::run`, not
/// `probe::http::probe` directly, so it still proves the dispatch wiring.
#[tokio::test]
async fn test_check_dispatches_through_probe_run_for_http() {
    let env = test_state().await;
    let a = serve(env.state.clone()).await;

    let s = wiremock::MockServer::start().await;
    wiremock::Mock::given(wiremock::matchers::method("GET"))
        .respond_with(wiremock::ResponseTemplate::new(200))
        .mount(&s)
        .await;

    let out: serde_json::Value = reqwest::Client::new()
        .post(format!("http://{a}/api/monitors/test-check"))
        .json(&serde_json::json!({"name":"t","type":"http","url": s.uri()}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert_eq!(out["ok"], serde_json::Value::Bool(true), "expected ok:true, got {out}");
}
