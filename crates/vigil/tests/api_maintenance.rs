//! P4.2 Task 6: `/api/maintenance-windows*` CRUD + the body-driven scope
//! preview. Mirrors `tests/api_incidents.rs`'s app-router harness.

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

fn now_epoch() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64
}

/// Inserts a monitor directly with an explicit `tags` JSON array, bypassing
/// the monitor-create API (whose DTO has no `tags` field) — needed for the
/// preview test's "monitors tagged prod" fixture.
async fn create_monitor_with_tags(pool: &sqlx::SqlitePool, name: &str, tags: &str) -> i64 {
    sqlx::query_scalar(
        "INSERT INTO monitors (name,type,url,expected_status_codes,interval_seconds,timeout_seconds,\
         confirmation_threshold,recovery_threshold,retry_interval_seconds,status,tags,created_at,updated_at) \
         VALUES (?, 'http', 'https://x', '200-299', 300, 30, 3, 1, 30, 'up', ?, 0, 0) RETURNING id",
    )
    .bind(name)
    .bind(tags)
    .fetch_one(pool)
    .await
    .unwrap()
}

fn valid_body(name: &str) -> serde_json::Value {
    let now = now_epoch();
    serde_json::json!({
        "name": name,
        "scope": "all",
        "starts_at": now,
        "ends_at": now + 3600,
    })
}

#[tokio::test]
async fn create_valid_returns_200_and_row() {
    let env = test_state().await;
    let a = serve(env.state.clone()).await;
    let c = reqwest::Client::new();

    let resp = c
        .post(format!("http://{a}/api/maintenance-windows"))
        .json(&valid_body("nightly"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let row: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(row["name"].as_str(), Some("nightly"));
    assert_eq!(row["scope"].as_str(), Some("all"));
    assert_eq!(row["is_active"].as_bool(), Some(true));
    assert!(row["target_ref"].is_null());
    assert!(row["id"].as_i64().is_some());
}

#[tokio::test]
async fn create_rejects_bad_scope() {
    let env = test_state().await;
    let a = serve(env.state.clone()).await;
    let c = reqwest::Client::new();
    let now = now_epoch();
    let resp = c
        .post(format!("http://{a}/api/maintenance-windows"))
        .json(&serde_json::json!({"name":"w","scope":"bogus","starts_at":now,"ends_at":now+3600}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn create_rejects_monitors_scope_with_empty_array() {
    let env = test_state().await;
    let a = serve(env.state.clone()).await;
    let c = reqwest::Client::new();
    let now = now_epoch();
    let resp = c
        .post(format!("http://{a}/api/maintenance-windows"))
        .json(&serde_json::json!({"name":"w","scope":"monitors","target_ref":[],"starts_at":now,"ends_at":now+3600}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn create_rejects_tag_scope_with_non_string_target_ref() {
    let env = test_state().await;
    let a = serve(env.state.clone()).await;
    let c = reqwest::Client::new();
    let now = now_epoch();
    let resp = c
        .post(format!("http://{a}/api/maintenance-windows"))
        .json(&serde_json::json!({"name":"w","scope":"tag","target_ref":123,"starts_at":now,"ends_at":now+3600}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn create_rejects_ends_at_not_after_starts_at() {
    let env = test_state().await;
    let a = serve(env.state.clone()).await;
    let c = reqwest::Client::new();
    let now = now_epoch();
    let resp = c
        .post(format!("http://{a}/api/maintenance-windows"))
        .json(&serde_json::json!({"name":"w","scope":"all","starts_at":now,"ends_at":now}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn create_rejects_six_field_cron() {
    let env = test_state().await;
    let a = serve(env.state.clone()).await;
    let c = reqwest::Client::new();
    let now = now_epoch();
    let resp = c
        .post(format!("http://{a}/api/maintenance-windows"))
        .json(&serde_json::json!({
            "name":"w","scope":"all","starts_at":now,"ends_at":now+3600,
            "recurrence":"0 0 * * * *"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn list_returns_created_window() {
    let env = test_state().await;
    let a = serve(env.state.clone()).await;
    let c = reqwest::Client::new();
    let created: serde_json::Value = c
        .post(format!("http://{a}/api/maintenance-windows"))
        .json(&valid_body("w1"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = created["id"].as_i64().unwrap();

    let list: serde_json::Value = c
        .get(format!("http://{a}/api/maintenance-windows"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let arr = list.as_array().expect("expected array response");
    assert!(arr.iter().any(|w| w["id"].as_i64() == Some(id)));
}

#[tokio::test]
async fn update_toggles_is_active_merge_then_validate() {
    let env = test_state().await;
    let a = serve(env.state.clone()).await;
    let c = reqwest::Client::new();
    let created: serde_json::Value = c
        .post(format!("http://{a}/api/maintenance-windows"))
        .json(&valid_body("w2"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = created["id"].as_i64().unwrap();
    assert_eq!(created["is_active"].as_bool(), Some(true));

    let resp = c
        .put(format!("http://{a}/api/maintenance-windows/{id}"))
        .json(&serde_json::json!({"is_active": false}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::OK, "PUT must succeed (route must fully-qualify axum::routing::put)");
    let updated: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(updated["is_active"].as_bool(), Some(false));
    // Fields not present in the PATCH must be preserved from the merge.
    assert_eq!(updated["name"].as_str(), Some("w2"));
    assert_eq!(updated["scope"].as_str(), Some("all"));
}

#[tokio::test]
async fn preview_tag_scope_returns_matching_monitors_and_active_now() {
    let env = test_state().await;
    let a = serve(env.state.clone()).await;
    let c = reqwest::Client::new();
    let prod_a = create_monitor_with_tags(&env.state.db, "prod-a", r#"["prod"]"#).await;
    let prod_b = create_monitor_with_tags(&env.state.db, "prod-b", r#"["prod","web"]"#).await;
    let staging = create_monitor_with_tags(&env.state.db, "staging-a", r#"["staging"]"#).await;
    let now = now_epoch();

    let resp: serde_json::Value = c
        .post(format!("http://{a}/api/maintenance-windows/preview"))
        .json(&serde_json::json!({
            "scope": "tag",
            "target_ref": "prod",
            "starts_at": now - 60,
            "ends_at": now + 3600,
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    let ids: Vec<i64> = resp["affected_monitor_ids"]
        .as_array()
        .expect("affected_monitor_ids must be an array")
        .iter()
        .map(|v| v.as_i64().unwrap())
        .collect();
    assert!(ids.contains(&prod_a), "expected prod-tagged monitor in {ids:?}");
    assert!(ids.contains(&prod_b), "expected prod-tagged monitor in {ids:?}");
    assert!(!ids.contains(&staging), "staging-tagged monitor must be excluded: {ids:?}");
    assert_eq!(resp["active_now"].as_bool(), Some(true));
}

#[tokio::test]
async fn preview_without_duration_returns_null_active_now() {
    let env = test_state().await;
    let a = serve(env.state.clone()).await;
    let c = reqwest::Client::new();

    let resp: serde_json::Value = c
        .post(format!("http://{a}/api/maintenance-windows/preview"))
        .json(&serde_json::json!({"scope": "all"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert!(resp["active_now"].is_null(), "active_now must be null when starts_at/ends_at are omitted: {resp}");
}

#[tokio::test]
async fn delete_removes_window() {
    let env = test_state().await;
    let a = serve(env.state.clone()).await;
    let c = reqwest::Client::new();
    let created: serde_json::Value = c
        .post(format!("http://{a}/api/maintenance-windows"))
        .json(&valid_body("w3"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = created["id"].as_i64().unwrap();

    let resp = c.delete(format!("http://{a}/api/maintenance-windows/{id}")).send().await.unwrap();
    assert!(resp.status().is_success());

    let list: serde_json::Value = c
        .get(format!("http://{a}/api/maintenance-windows"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let arr = list.as_array().unwrap();
    assert!(!arr.iter().any(|w| w["id"].as_i64() == Some(id)));
}
