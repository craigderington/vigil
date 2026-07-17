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
#[tokio::test] async fn stats_counts_open_incident_started_before_window() {
    // Regression for the /stats incident-count window mismatch: an incident
    // that opened before window_start and is still unresolved overlaps the
    // window (and so contributes downtime), so it must also be counted —
    // not just the incidents whose `started_at` itself falls inside window.
    let env = test_state().await; let a = serve(env.state.clone()).await; let c = reqwest::Client::new();
    let created: serde_json::Value = c.post(format!("http://{a}/api/monitors"))
        .json(&serde_json::json!({"name":"x","url":"https://e.com"})).send().await.unwrap().json().await.unwrap();
    let id = created["id"].as_i64().unwrap();
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64;
    sqlx::query("INSERT INTO incidents (monitor_id, started_at, resolved_at) VALUES (?, ?, NULL)")
        .bind(id).bind(now - 2 * 86400) // started 2 days ago, well before the 24h window, still open
        .execute(&env.state.db).await.unwrap();
    let s: serde_json::Value = c.get(format!("http://{a}/api/monitors/{id}/stats?range=24h")).send().await.unwrap()
        .json().await.unwrap();
    assert_eq!(s["incidents"].as_i64(), Some(1), "open incident predating the window must still be counted: {s}");
}
#[tokio::test] async fn sse_first_frame_is_snapshot() {
    let env = test_state().await; let a = serve(env.state.clone()).await;
    let mut resp = reqwest::Client::new().get(format!("http://{a}/events")).send().await.unwrap();
    assert!(resp.status().is_success());
    // Read the first chunk of the SSE stream (with a timeout so we don't hang on the open
    // stream). `futures_util`/`bytes_stream()` aren't dev-dependencies here, so read via
    // `Response::chunk()`, which is available without reqwest's "stream" feature.
    let first = tokio::time::timeout(std::time::Duration::from_secs(5), resp.chunk())
        .await
        .expect("timed out waiting for SSE frame")
        .unwrap()
        .expect("stream ended with no data");
    let text = String::from_utf8_lossy(&first);
    assert!(text.contains("snapshot"), "first SSE frame must be the snapshot, got: {text}");
}
#[tokio::test] async fn channels_crud() {
    let env = test_state().await; let a = serve(env.state.clone()).await; let c = reqwest::Client::new();
    let created: serde_json::Value = c.post(format!("http://{a}/api/channels"))
        .json(&serde_json::json!({"name":"mail","type":"email","config":{"host":"h","port":25,"security":"none","from":"f@b","to":["a@b"]}}))
        .send().await.unwrap().json().await.unwrap();
    assert!(created["id"].as_i64().is_some());
    let list: serde_json::Value = c.get(format!("http://{a}/api/channels")).send().await.unwrap().json().await.unwrap();
    assert_eq!(list.as_array().unwrap().len(), 1);
}
#[tokio::test] async fn settings_get_and_put() {
    let env = test_state().await; let a = serve(env.state.clone()).await; let c = reqwest::Client::new();
    let s: serde_json::Value = c.get(format!("http://{a}/api/settings")).send().await.unwrap().json().await.unwrap();
    assert_eq!(s["cooldown_minutes"].as_i64(), Some(15));
    c.put(format!("http://{a}/api/settings")).json(&serde_json::json!({"cooldown_minutes":30})).send().await.unwrap();
    let s2: serde_json::Value = c.get(format!("http://{a}/api/settings")).send().await.unwrap().json().await.unwrap();
    assert_eq!(s2["cooldown_minutes"].as_i64(), Some(30));
}
