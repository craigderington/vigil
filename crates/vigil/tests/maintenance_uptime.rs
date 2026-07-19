//! P4.2 Task 3: maintenance time must not count against uptime %. Proves
//! the live `GET /stats` read path excludes an active maintenance window's
//! time from both the downtime and the denominator, end to end (monitor +
//! resolved incident + an overlapping one-off `maintenance_windows` row).

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

/// Inserts an `is_active`, one-off, `scope: "all"` maintenance window
/// directly (bypassing the not-yet-wired API), mirroring migration
/// `0005_maintenance_windows.sql`'s columns.
async fn insert_active_window(pool: &sqlx::SqlitePool, starts_at: i64, ends_at: i64) -> i64 {
    sqlx::query_scalar(
        "INSERT INTO maintenance_windows \
         (name, scope, target_ref, starts_at, ends_at, recurrence, suppress, is_active, created_at) \
         VALUES ('planned', 'all', NULL, ?, ?, NULL, 'alerts', 1, ?) RETURNING id",
    )
    .bind(starts_at)
    .bind(ends_at)
    .bind(starts_at)
    .fetch_one(pool)
    .await
    .unwrap()
}

/// A resolved 1h incident that FULLY overlaps an active one-off maintenance
/// window must not count against uptime: `downtime_seconds` ~0 and
/// `uptime_pct` ~100, not the ~95.8% a naive 24h-window calculation would
/// report for an uncounted 1h outage.
#[tokio::test]
async fn incident_fully_inside_maintenance_window_excluded_from_stats() {
    let env = test_state().await;
    let a = serve(env.state.clone()).await;
    let c = reqwest::Client::new();
    let n = now_epoch();
    let id = create_monitor(&c, a, "api.myapp.com").await;

    // A resolved incident spanning 1h, well within the last 24h window.
    let started = n - 7200; // 2h ago
    let resolved = n - 3600; // 1h ago -> 1h span
    sqlx::query("INSERT INTO incidents (monitor_id, started_at, resolved_at) VALUES (?, ?, ?)")
        .bind(id)
        .bind(started)
        .bind(resolved)
        .execute(&env.state.db)
        .await
        .unwrap();

    // A maintenance window that fully covers the incident (and then some),
    // active right now.
    insert_active_window(&env.state.db, started - 300, resolved + 300).await;

    // A `checks` row so `had_any_check` is true (otherwise uptime_pct is
    // None regardless of maintenance, and the test wouldn't prove anything).
    sqlx::query("INSERT INTO checks (monitor_id, checked_at, status) VALUES (?, ?, 'up')")
        .bind(id)
        .bind(n - 60)
        .execute(&env.state.db)
        .await
        .unwrap();

    let resp = c.get(format!("http://{a}/api/monitors/{id}/stats?range=24h")).send().await.unwrap();
    let status = resp.status();
    let text = resp.text().await.unwrap();
    assert!(status.is_success(), "status={status} body={text}");
    let s: serde_json::Value = serde_json::from_str(&text).unwrap();

    let downtime = s["downtime_seconds"].as_i64().expect("downtime_seconds must be numeric");
    assert!(downtime <= 5, "expected downtime_seconds ~0 (excluded by maintenance), got {downtime}");

    let uptime_pct = s["uptime_pct"].as_f64().expect("uptime_pct must be numeric (had a check)");
    assert!(uptime_pct >= 99.0, "expected uptime_pct ~100 (excluded by maintenance), got {uptime_pct}");
}

/// Sanity control: the SAME incident with NO maintenance window present
/// must still count as real downtime — proves the exclusion above is
/// actually caused by the maintenance window, not some other effect.
#[tokio::test]
async fn incident_without_maintenance_window_counts_as_downtime() {
    let env = test_state().await;
    let a = serve(env.state.clone()).await;
    let c = reqwest::Client::new();
    let n = now_epoch();
    let id = create_monitor(&c, a, "api.myapp.com").await;

    let started = n - 7200;
    let resolved = n - 3600;
    sqlx::query("INSERT INTO incidents (monitor_id, started_at, resolved_at) VALUES (?, ?, ?)")
        .bind(id)
        .bind(started)
        .bind(resolved)
        .execute(&env.state.db)
        .await
        .unwrap();

    sqlx::query("INSERT INTO checks (monitor_id, checked_at, status) VALUES (?, ?, 'up')")
        .bind(id)
        .bind(n - 60)
        .execute(&env.state.db)
        .await
        .unwrap();

    let s: serde_json::Value = c
        .get(format!("http://{a}/api/monitors/{id}/stats?range=24h"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    let downtime = s["downtime_seconds"].as_i64().expect("downtime_seconds must be numeric");
    assert!(downtime >= 3500, "expected the full ~1h incident counted as downtime, got {downtime}");
}
