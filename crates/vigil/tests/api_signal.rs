mod common; use common::*;
async fn serve(state: vigil::app::AppState) -> std::net::SocketAddr {
    let app = vigil::app::router(state);
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let a = l.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(l, app).await.unwrap(); }); a
}

/// Seeds `n` checks for `id` spread evenly over the last `span_secs`
/// seconds (oldest first), alternating a couple of down checks in so
/// `series`/`bars` have real signal to bucket, plus one incident spanning
/// part of the window.
async fn seed_checks_and_incident(pool: &sqlx::SqlitePool, id: i64, now: i64) {
    // 40 checks over the last 20 days, response time 100-140ms, mostly up.
    for i in 0..40i64 {
        let checked_at = now - (20 * 86400) + i * (20 * 86400 / 40);
        let status = if i == 5 { "down" } else { "up" };
        sqlx::query(
            "INSERT INTO checks (monitor_id, checked_at, status, response_time_ms) VALUES (?, ?, ?, ?)",
        )
        .bind(id)
        .bind(checked_at)
        .bind(status)
        .bind(100 + (i % 5) * 10)
        .execute(pool)
        .await
        .unwrap();
    }
    // A short resolved incident 10 days ago (1 hour long) so bars/incidents
    // counting and has_data have something to see.
    let started = now - 10 * 86400;
    sqlx::query(
        "INSERT INTO incidents (monitor_id, started_at, resolved_at) VALUES (?, ?, ?)",
    )
    .bind(id)
    .bind(started)
    .bind(started + 3600)
    .execute(pool)
    .await
    .unwrap();
}

#[tokio::test]
async fn stats_30d_avg_ms_is_numeric() {
    let env = test_state().await;
    let a = serve(env.state.clone()).await;
    let c = reqwest::Client::new();
    let created: serde_json::Value = c
        .post(format!("http://{a}/api/monitors"))
        .json(&serde_json::json!({"name":"x","url":"https://e.com"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = created["id"].as_i64().unwrap();
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64;
    seed_checks_and_incident(&env.state.db, id, now).await;

    let resp = c
        .get(format!("http://{a}/api/monitors/{id}/stats?range=30d"))
        .send()
        .await
        .unwrap();
    let status = resp.status();
    let text = resp.text().await.unwrap();
    assert!(status.is_success(), "status={status} body={text}");
    let s: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert!(s["avg_ms"].as_f64().is_some(), "expected numeric avg_ms for 30d, got {s}");
    assert!(s["uptime_pct"].as_f64().is_some(), "expected numeric uptime_pct for 30d, got {s}");
    assert_eq!(s["incidents"].as_i64(), Some(1), "expected the seeded incident to be counted: {s}");
}

#[tokio::test]
async fn stats_90d_avg_ms_is_numeric() {
    let env = test_state().await;
    let a = serve(env.state.clone()).await;
    let c = reqwest::Client::new();
    let created: serde_json::Value = c
        .post(format!("http://{a}/api/monitors"))
        .json(&serde_json::json!({"name":"x","url":"https://e.com"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = created["id"].as_i64().unwrap();
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64;
    seed_checks_and_incident(&env.state.db, id, now).await;

    let s: serde_json::Value = c
        .get(format!("http://{a}/api/monitors/{id}/stats?range=90d"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(s["avg_ms"].as_f64().is_some(), "expected numeric avg_ms for 90d, got {s}");
}

#[tokio::test]
async fn series_24h_returns_bucketed_points() {
    let env = test_state().await;
    let a = serve(env.state.clone()).await;
    let c = reqwest::Client::new();
    let created: serde_json::Value = c
        .post(format!("http://{a}/api/monitors"))
        .json(&serde_json::json!({"name":"x","url":"https://e.com"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = created["id"].as_i64().unwrap();
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64;
    // A handful of checks inside the last 24h.
    for i in 0..6i64 {
        let checked_at = now - i * 3600;
        let status = if i == 2 { "down" } else { "up" };
        sqlx::query(
            "INSERT INTO checks (monitor_id, checked_at, status, response_time_ms) VALUES (?, ?, ?, ?)",
        )
        .bind(id)
        .bind(checked_at)
        .bind(status)
        .bind(150)
        .execute(&env.state.db)
        .await
        .unwrap();
    }

    let s: serde_json::Value = c
        .get(format!("http://{a}/api/monitors/{id}/series?range=24h"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let arr = s.as_array().expect("series must be an array");
    assert!(!arr.is_empty(), "expected non-empty series, got {s}");
    assert!(arr.len() <= 300, "series must be bucketed to <=300 points, got {}", arr.len());
    for point in arr {
        assert!(point["t"].as_i64().is_some(), "point missing t: {point}");
        assert!(point["status"].as_str().is_some(), "point missing status: {point}");
    }
    assert!(
        arr.iter().any(|p| p["status"] == "down"),
        "expected at least one down bucket, got {s}"
    );
}

#[tokio::test]
async fn bars_90d_has_data_true_for_days_with_signal() {
    let env = test_state().await;
    let a = serve(env.state.clone()).await;
    let c = reqwest::Client::new();
    let created: serde_json::Value = c
        .post(format!("http://{a}/api/monitors"))
        .json(&serde_json::json!({"name":"x","url":"https://e.com"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = created["id"].as_i64().unwrap();
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64;
    seed_checks_and_incident(&env.state.db, id, now).await;

    let s: serde_json::Value = c
        .get(format!("http://{a}/api/monitors/{id}/bars?days=90"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let arr = s.as_array().expect("bars must be an array");
    assert!(arr.len() <= 90 && !arr.is_empty(), "expected <=90 rows, got {}", arr.len());

    let has_data_count = arr.iter().filter(|r| r["has_data"] == true).count();
    assert!(has_data_count > 0, "expected at least one day with has_data=true, got {s}");

    // Oldest-to-newest ordering: the `day` strings should be non-decreasing.
    let days: Vec<&str> = arr.iter().map(|r| r["day"].as_str().unwrap()).collect();
    let mut sorted = days.clone();
    sorted.sort();
    assert_eq!(days, sorted, "bars must be ordered oldest->newest");

    for row in arr {
        assert!(row["incidents"].as_i64().is_some(), "row missing incidents: {row}");
        assert!(row["down_seconds"].as_i64().is_some(), "row missing down_seconds: {row}");
    }
}
