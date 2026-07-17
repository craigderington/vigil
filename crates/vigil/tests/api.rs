mod common; use common::*;
async fn serve(state: vigil::app::AppState) -> std::net::SocketAddr {
    let app = vigil::app::router(state);
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let a = l.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(l, app).await.unwrap(); }); a
}
#[tokio::test] async fn crud_and_check_now() {
    let env = test_state().await; let a = serve(env.state.clone()).await; let c = reqwest::Client::new();
    let created: serde_json::Value = c.post(format!("http://{a}/api/monitors"))
        .json(&serde_json::json!({"name":"x","url":"https://example.com"})).send().await.unwrap()
        .json().await.unwrap();
    let id = created["id"].as_i64().unwrap();
    let list: serde_json::Value = c.get(format!("http://{a}/api/monitors")).send().await.unwrap().json().await.unwrap();
    assert_eq!(list.as_array().unwrap().len(), 1);
    assert!(c.post(format!("http://{a}/api/monitors/{id}/check-now")).send().await.unwrap().status().is_success());
    assert!(c.delete(format!("http://{a}/api/monitors/{id}")).send().await.unwrap().status().is_success());
}
#[tokio::test] async fn rejects_short_interval() {
    let env = test_state().await; let a = serve(env.state.clone()).await;
    let r = reqwest::Client::new().post(format!("http://{a}/api/monitors"))
        .json(&serde_json::json!({"name":"x","url":"https://e.com","interval_seconds":5})).send().await.unwrap();
    assert_eq!(r.status(), 422);
}
#[tokio::test] async fn stats_dash_when_no_checks() {
    let env = test_state().await; let a = serve(env.state.clone()).await; let c = reqwest::Client::new();
    let created: serde_json::Value = c.post(format!("http://{a}/api/monitors"))
        .json(&serde_json::json!({"name":"x","url":"https://e.com"})).send().await.unwrap().json().await.unwrap();
    let id = created["id"].as_i64().unwrap();
    let s: serde_json::Value = c.get(format!("http://{a}/api/monitors/{id}/stats?range=24h")).send().await.unwrap()
        .json().await.unwrap();
    assert!(s["uptime_pct"].is_null());
}
