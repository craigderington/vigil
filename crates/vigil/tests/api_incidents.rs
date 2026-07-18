mod common; use common::*;
async fn serve(state: vigil::app::AppState) -> std::net::SocketAddr {
    let app = vigil::app::router(state);
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let a = l.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(l, app).await.unwrap(); }); a
}

async fn create_monitor(c: &reqwest::Client, a: std::net::SocketAddr, name: &str) -> i64 {
    let created: serde_json::Value = c
        .post(format!("http://{a}/api/monitors"))
        .json(&serde_json::json!({"name": name, "url": "https://e.com"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    created["id"].as_i64().unwrap()
}

async fn insert_incident(pool: &sqlx::SqlitePool, monitor_id: i64, started_at: i64) -> i64 {
    sqlx::query_scalar(
        "INSERT INTO incidents (monitor_id, started_at, resolved_at, cause) VALUES (?, ?, NULL, 'timeout') RETURNING id",
    )
    .bind(monitor_id)
    .bind(started_at)
    .fetch_one(pool)
    .await
    .unwrap()
}

#[tokio::test]
async fn list_returns_incident_with_monitor_name_and_unacknowledged() {
    let env = test_state().await;
    let a = serve(env.state.clone()).await;
    let c = reqwest::Client::new();
    let id = create_monitor(&c, a, "api.myapp.com").await;
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64;
    let incident_id = insert_incident(&env.state.db, id, now - 3600).await;

    let list: serde_json::Value = c
        .get(format!("http://{a}/api/incidents"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let arr = list.as_array().expect("expected array response");
    let found = arr
        .iter()
        .find(|i| i["id"].as_i64() == Some(incident_id))
        .expect("seeded incident must be present");
    assert_eq!(found["monitor_id"].as_i64(), Some(id));
    assert_eq!(found["monitor_name"].as_str(), Some("api.myapp.com"));
    assert_eq!(found["acknowledged"].as_bool(), Some(false));
    assert!(found["resolved_at"].is_null());
}

#[tokio::test]
async fn acknowledge_flips_flag() {
    let env = test_state().await;
    let a = serve(env.state.clone()).await;
    let c = reqwest::Client::new();
    let id = create_monitor(&c, a, "x").await;
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64;
    let incident_id = insert_incident(&env.state.db, id, now - 3600).await;

    let resp = c
        .post(format!("http://{a}/api/incidents/{incident_id}/acknowledge"))
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success(), "acknowledge must return 2xx, got {}", resp.status());

    let list: serde_json::Value = c
        .get(format!("http://{a}/api/incidents"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let arr = list.as_array().unwrap();
    let found = arr.iter().find(|i| i["id"].as_i64() == Some(incident_id)).unwrap();
    assert_eq!(found["acknowledged"].as_bool(), Some(true));
}

#[tokio::test]
async fn monitor_id_filter_excludes_other_monitors_incidents() {
    let env = test_state().await;
    let a = serve(env.state.clone()).await;
    let c = reqwest::Client::new();
    let id_a = create_monitor(&c, a, "monitor-a").await;
    let id_b = create_monitor(&c, a, "monitor-b").await;
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64;
    let incident_a = insert_incident(&env.state.db, id_a, now - 3600).await;
    let incident_b = insert_incident(&env.state.db, id_b, now - 3600).await;

    let list: serde_json::Value = c
        .get(format!("http://{a}/api/incidents?monitor_id={id_a}"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let arr = list.as_array().unwrap();
    let ids: Vec<i64> = arr.iter().map(|i| i["id"].as_i64().unwrap()).collect();
    assert!(ids.contains(&incident_a), "must include the filtered monitor's incident: {ids:?}");
    assert!(!ids.contains(&incident_b), "must exclude the other monitor's incident: {ids:?}");
}

#[tokio::test]
async fn range_default_excludes_incidents_older_than_30d() {
    let env = test_state().await;
    let a = serve(env.state.clone()).await;
    let c = reqwest::Client::new();
    let id = create_monitor(&c, a, "old").await;
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64;
    // 40 days ago: outside the default 30d window.
    let old_incident = insert_incident(&env.state.db, id, now - 40 * 86400).await;
    // 1 day ago: inside the default 30d window.
    let recent_incident = insert_incident(&env.state.db, id, now - 86400).await;

    let list: serde_json::Value = c
        .get(format!("http://{a}/api/incidents"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let ids: Vec<i64> = list.as_array().unwrap().iter().map(|i| i["id"].as_i64().unwrap()).collect();
    assert!(ids.contains(&recent_incident), "recent incident must be included: {ids:?}");
    assert!(!ids.contains(&old_incident), "40-day-old incident must be excluded by default 30d range: {ids:?}");
}
