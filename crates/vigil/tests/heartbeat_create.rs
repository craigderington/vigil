//! P4.1 Task 2: heartbeat monitor creation — validation, forced thresholds,
//! token generation, and the dedicated `/heartbeat` token endpoint (the
//! ONLY place a `heartbeat_token` is ever exposed — `Monitor` itself has
//! `#[serde(skip_serializing)]` on the field).

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
async fn create_heartbeat_forces_thresholds() {
    let env = test_state().await;
    let a = serve(env.state.clone()).await;

    let r = reqwest::Client::new()
        .post(format!("http://{a}/api/monitors"))
        .json(&serde_json::json!({
            "name": "cron",
            "type": "heartbeat",
            "interval_seconds": 3600,
            "heartbeat_grace_seconds": 120
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);

    let body: serde_json::Value = r.json().await.unwrap();
    assert_eq!(body["status"], "pending");
    assert_eq!(body["confirmation_threshold"], 1);
    assert_eq!(body["recovery_threshold"], 1);
    assert_eq!(body["heartbeat_grace_seconds"], 120);
    assert!(body.get("heartbeat_token").is_none(), "heartbeat_token must never be serialized on a Monitor, got {body}");
}

#[tokio::test]
async fn heartbeat_token_only_via_dedicated_endpoint() {
    let env = test_state().await;
    let a = serve(env.state.clone()).await;
    let client = reqwest::Client::new();

    let created: serde_json::Value = client
        .post(format!("http://{a}/api/monitors"))
        .json(&serde_json::json!({"name": "cron", "type": "heartbeat", "interval_seconds": 60}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = created["id"].as_i64().unwrap();

    // dedicated endpoint returns the token + ping_path
    let hb: serde_json::Value = client
        .get(format!("http://{a}/api/monitors/{id}/heartbeat"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let token = hb["token"].as_str().expect("token field present");
    assert_eq!(token.len(), 32, "expected a 32-char token, got {token:?}");
    assert!(token.chars().all(|c| c.is_ascii_alphanumeric()), "expected alphanumeric token, got {token:?}");
    let ping_path = hb["ping_path"].as_str().expect("ping_path field present");
    assert!(ping_path.starts_with("/ping/"), "expected /ping/ prefix, got {ping_path:?}");

    // list and get_one never contain heartbeat_token
    let list: serde_json::Value =
        client.get(format!("http://{a}/api/monitors")).send().await.unwrap().json().await.unwrap();
    assert!(
        list.as_array().unwrap().iter().all(|m| m.get("heartbeat_token").is_none()),
        "list response leaked heartbeat_token: {list}"
    );

    let one: serde_json::Value = client
        .get(format!("http://{a}/api/monitors/{id}"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(one.get("heartbeat_token").is_none(), "get_one leaked heartbeat_token: {one}");

    // a non-heartbeat monitor's /heartbeat is 404
    let http_created: serde_json::Value = client
        .post(format!("http://{a}/api/monitors"))
        .json(&serde_json::json!({"name": "web", "type": "http", "url": "https://example.com"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let http_id = http_created["id"].as_i64().unwrap();
    let r = client.get(format!("http://{a}/api/monitors/{http_id}/heartbeat")).send().await.unwrap();
    assert_eq!(r.status(), 404);
}

#[tokio::test]
async fn heartbeat_rejects_ssl() {
    let env = test_state().await;
    let a = serve(env.state.clone()).await;
    let r = reqwest::Client::new()
        .post(format!("http://{a}/api/monitors"))
        .json(&serde_json::json!({
            "name": "cron",
            "type": "heartbeat",
            "interval_seconds": 60,
            "ssl_check_enabled": true
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 422);
}

#[tokio::test]
async fn heartbeat_rejects_domain() {
    let env = test_state().await;
    let a = serve(env.state.clone()).await;
    let r = reqwest::Client::new()
        .post(format!("http://{a}/api/monitors"))
        .json(&serde_json::json!({
            "name": "cron",
            "type": "heartbeat",
            "interval_seconds": 60,
            "ssl_check_enabled": false,
            "domain_check_enabled": true
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 422);
}

#[tokio::test]
async fn heartbeat_rejects_short_interval() {
    let env = test_state().await;
    let a = serve(env.state.clone()).await;
    let r = reqwest::Client::new()
        .post(format!("http://{a}/api/monitors"))
        .json(&serde_json::json!({"name": "cron", "type": "heartbeat", "interval_seconds": 10}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 422);
}

#[tokio::test]
async fn two_heartbeats_get_distinct_tokens() {
    let env = test_state().await;
    let a = serve(env.state.clone()).await;
    let client = reqwest::Client::new();

    let mut tokens = Vec::new();
    for _ in 0..2 {
        let created: serde_json::Value = client
            .post(format!("http://{a}/api/monitors"))
            .json(&serde_json::json!({"name": "cron", "type": "heartbeat", "interval_seconds": 60}))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let id = created["id"].as_i64().unwrap();
        let hb: serde_json::Value = client
            .get(format!("http://{a}/api/monitors/{id}/heartbeat"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        tokens.push(hb["token"].as_str().unwrap().to_string());
    }
    assert_ne!(tokens[0], tokens[1], "two heartbeats must get distinct tokens");
}
