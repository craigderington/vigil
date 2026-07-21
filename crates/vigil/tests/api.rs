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
        .json(&serde_json::json!({"name":"mail","type":"email","config":r#"{"host":"h","port":25,"security":"none","from":"f@b","to":["a@b"]}"#}))
        .send().await.unwrap().json().await.unwrap();
    assert!(created["id"].as_i64().is_some());
    let list: serde_json::Value = c.get(format!("http://{a}/api/channels")).send().await.unwrap().json().await.unwrap();
    assert_eq!(list.as_array().unwrap().len(), 1);
}
#[tokio::test] async fn monitor_notifications_roundtrip() {
    let env = test_state().await; let a = serve(env.state.clone()).await; let c = reqwest::Client::new();
    // create a channel and a monitor
    let ch: serde_json::Value = c.post(format!("http://{a}/api/channels")).json(&serde_json::json!({"name":"m","type":"email","config":"{}"})).send().await.unwrap().json().await.unwrap();
    let mon: serde_json::Value = c.post(format!("http://{a}/api/monitors")).json(&serde_json::json!({"name":"x","url":"https://e.com"})).send().await.unwrap().json().await.unwrap();
    let (cid, mid) = (ch["id"].as_i64().unwrap(), mon["id"].as_i64().unwrap());
    c.put(format!("http://{a}/api/monitors/{mid}/notifications")).json(&serde_json::json!([{"channel_id":cid,"triggers":["down","recovered"]}])).send().await.unwrap();
    let got: serde_json::Value = c.get(format!("http://{a}/api/monitors/{mid}/notifications")).send().await.unwrap().json().await.unwrap();
    assert_eq!(got.as_array().unwrap().len(), 1);
    assert_eq!(got[0]["channel_id"].as_i64(), Some(cid));
}
#[tokio::test] async fn settings_get_and_put() {
    let env = test_state().await; let a = serve(env.state.clone()).await; let c = reqwest::Client::new();
    let s: serde_json::Value = c.get(format!("http://{a}/api/settings")).send().await.unwrap().json().await.unwrap();
    assert_eq!(s["cooldown_minutes"].as_i64(), Some(15));
    c.put(format!("http://{a}/api/settings")).json(&serde_json::json!({"cooldown_minutes":30})).send().await.unwrap();
    let s2: serde_json::Value = c.get(format!("http://{a}/api/settings")).send().await.unwrap().json().await.unwrap();
    assert_eq!(s2["cooldown_minutes"].as_i64(), Some(30));
}
#[tokio::test] async fn channel_config_stored_verbatim_not_double_encoded() {
    let env = test_state().await; let a = serve(env.state.clone()).await; let c = reqwest::Client::new();
    let cfg = r#"{"host":"h","port":25,"security":"none","from":"a@b.com","to":["c@b.com"]}"#;
    let created: serde_json::Value = c.post(format!("http://{a}/api/channels"))
        .json(&serde_json::json!({"name":"E","type":"email","config":cfg})).send().await.unwrap()
        .json().await.unwrap();
    let id = created["id"].as_i64().unwrap();
    // GET it back and confirm config parses to an OBJECT (host=="h"), i.e. NOT double-encoded
    let list: serde_json::Value = c.get(format!("http://{a}/api/channels")).send().await.unwrap().json().await.unwrap();
    let stored = list.as_array().unwrap().iter().find(|ch| ch["id"].as_i64()==Some(id)).unwrap();
    let config_str = stored["config"].as_str().expect("config should be a string");
    let parsed: serde_json::Value = serde_json::from_str(config_str).expect("stored config must be a plain JSON object string");
    assert_eq!(parsed["host"].as_str(), Some("h"), "config must round-trip as an object, not a double-encoded string");
    // and /test must NOT return a parse error (it'll fail to connect since no SMTP server, but not 'expected struct')
    let test_resp: serde_json::Value = c.post(format!("http://{a}/api/channels/{id}/test")).send().await.unwrap().json().await.unwrap();
    let err = test_resp["error"].as_str().unwrap_or("");
    assert!(!err.contains("expected struct"), "test must not fail on config parse; got: {err}");
}
#[tokio::test]
async fn reorder_persists_sort_order() {
    let env = test_state().await;
    let a = serve(env.state.clone()).await;
    let c = reqwest::Client::new();

    let mut ids = vec![];
    for n in ["a", "b", "c"] {
        let m: serde_json::Value = c.post(format!("http://{a}/api/monitors"))
            .json(&serde_json::json!({"name": n, "url": "https://e.com"}))
            .send().await.unwrap().json().await.unwrap();
        ids.push(m["id"].as_i64().unwrap());
    }

    // reverse the order
    let new_order = vec![ids[2], ids[1], ids[0]];
    let r = c.post(format!("http://{a}/api/monitors/reorder"))
        .json(&new_order).send().await.unwrap();
    assert!(r.status().is_success(), "reorder status: {}", r.status());

    let list: serde_json::Value = c.get(format!("http://{a}/api/monitors"))
        .send().await.unwrap().json().await.unwrap();
    let got: Vec<i64> = list.as_array().unwrap().iter().map(|m| m["id"].as_i64().unwrap()).collect();
    assert_eq!(got, new_order, "GET /monitors returns the new order");
    let sort_orders: Vec<i64> = list.as_array().unwrap().iter().map(|m| m["sort_order"].as_i64().unwrap()).collect();
    assert_eq!(sort_orders, vec![0, 1, 2], "sort_order == array index");

    // Lenient contract: an unknown id in the body is a harmless no-op (0 rows),
    // the known ids still get their positions.
    let with_unknown = vec![ids[0], 9999, ids[1]];
    let r2 = c.post(format!("http://{a}/api/monitors/reorder"))
        .json(&with_unknown).send().await.unwrap();
    assert!(r2.status().is_success(), "unknown id must not error: {}", r2.status());
    let list2: serde_json::Value = c.get(format!("http://{a}/api/monitors"))
        .send().await.unwrap().json().await.unwrap();
    let so = |id: i64| -> Option<i64> {
        list2.as_array().unwrap().iter()
            .find(|m| m["id"].as_i64() == Some(id)).unwrap()["sort_order"].as_i64()
    };
    assert_eq!(so(ids[0]), Some(0), "ids[0] -> index 0");
    assert_eq!(so(ids[1]), Some(2), "ids[1] -> index 2 (index 1's id 9999 doesn't exist)");
}
