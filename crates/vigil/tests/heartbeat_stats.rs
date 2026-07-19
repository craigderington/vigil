//! P4.1 Task 7: `stats`/`bars` heartbeat special-cases. Heartbeats never
//! write `checks` rows (they're ping/reaper-driven, not probe-driven), so
//! the two `checks`-gated read-path helpers (`had_any_check` in `stats`,
//! `has_data` in `bars`) would otherwise show a heartbeat's uptime/downtime
//! as "no data" even while it's DOWN with a recorded incident. These tests
//! prove the incident-derived special-case instead.

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

/// Inserts a heartbeat monitor directly (bypassing the API, mirroring
/// `heartbeat_reaper.rs`'s `seed_heartbeat`) with explicit `last_ping_at`/
/// `created_at`. Returns the monitor id.
async fn seed_heartbeat(
    pool: &sqlx::SqlitePool,
    last_ping_at: Option<i64>,
    created_at: i64,
) -> i64 {
    let token = vigil::heartbeat::generate_token();
    sqlx::query_scalar(
        "INSERT INTO monitors (name, type, heartbeat_token, interval_seconds, heartbeat_grace_seconds, \
         confirmation_threshold, recovery_threshold, last_ping_at, status, is_paused, created_at, updated_at) \
         VALUES (?, 'heartbeat', ?, 60, 60, 1, 1, ?, 'up', 0, ?, ?) RETURNING id",
    )
    .bind("cron")
    .bind(&token)
    .bind(last_ping_at)
    .bind(created_at)
    .bind(created_at)
    .fetch_one(pool)
    .await
    .unwrap()
}

/// A DOWN heartbeat with a resolved incident (and no `checks` rows at all)
/// must still report real downtime from `stats` — proving the
/// `had_any_check || (is_heartbeat && armed)` special-case.
#[tokio::test]
async fn heartbeat_downtime_counts() {
    let env = test_state().await;
    let a = serve(env.state.clone()).await;
    let c = reqwest::Client::new();
    let n = now_epoch();

    // Armed (pinged at least once) so the special-case fires; created well
    // before the 24h window so it doesn't interfere with `first_active_day`
    // logic in the bars test (this test only hits `stats`).
    let mid = seed_heartbeat(&env.state.db, Some(n - 100), n - 10 * 86400).await;

    // A resolved incident spanning 1h, entirely within the last 24h window,
    // and NO `checks` rows for this monitor at all.
    let started = n - 7200; // 2h ago
    let resolved = n - 3600; // 1h ago -> 1h span
    sqlx::query("INSERT INTO incidents (monitor_id, started_at, resolved_at) VALUES (?, ?, ?)")
        .bind(mid)
        .bind(started)
        .bind(resolved)
        .execute(&env.state.db)
        .await
        .unwrap();

    let checks_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM checks WHERE monitor_id = ?")
        .bind(mid)
        .fetch_one(&env.state.db)
        .await
        .unwrap();
    assert_eq!(checks_count, 0, "sanity: heartbeats must never write checks rows");

    let resp = c.get(format!("http://{a}/api/monitors/{mid}/stats?range=24h")).send().await.unwrap();
    let status = resp.status();
    let text = resp.text().await.unwrap();
    assert!(status.is_success(), "status={status} body={text}");
    let s: serde_json::Value = serde_json::from_str(&text).unwrap();

    assert!(
        s["uptime_pct"].as_f64().is_some(),
        "an armed heartbeat with a recorded incident must report a real uptime_pct, got {s}"
    );
    let downtime = s["downtime_seconds"].as_i64().expect("downtime_seconds must be numeric");
    assert!(downtime >= 3600, "expected downtime_seconds >= 3600 (the 1h incident), got {downtime}");
}

/// A never-pinged heartbeat must still report `uptime_pct: None` (matching
/// the "waiting" UI) — the special-case is gated on `armed`, not just
/// `is_heartbeat`.
#[tokio::test]
async fn never_pinged_heartbeat_stats_is_null() {
    let env = test_state().await;
    let a = serve(env.state.clone()).await;
    let c = reqwest::Client::new();
    let n = now_epoch();

    let mid = seed_heartbeat(&env.state.db, None, n - 10 * 86400).await;

    let s: serde_json::Value = c
        .get(format!("http://{a}/api/monitors/{mid}/stats?range=24h"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert!(
        s["uptime_pct"].is_null(),
        "a never-pinged (unarmed) heartbeat must report uptime_pct: None, got {s}"
    );
}

/// An armed heartbeat's clean day (no incident, no checks, no rollup) must
/// still render `has_data == true` (green), not muted — proving the bars
/// `is_heartbeat` special-case is incident-derived rather than
/// rollup/raw-check-derived.
#[tokio::test]
async fn heartbeat_bars_have_data() {
    let env = test_state().await;
    let a = serve(env.state.clone()).await;
    let c = reqwest::Client::new();
    let n = now_epoch();

    // Created 10 days ago, pinged recently (armed) -> first_active_day is
    // well before the 7-day window queried below, and there are no
    // incidents/checks/rollups at all, so every day in range is "clean".
    let mid = seed_heartbeat(&env.state.db, Some(n - 60), n - 10 * 86400).await;

    let s: serde_json::Value = c
        .get(format!("http://{a}/api/monitors/{mid}/bars?days=7"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let arr = s.as_array().expect("bars must be an array");
    assert_eq!(arr.len(), 7, "expected 7 rows, got {}", arr.len());

    // Today (the last row, oldest-first ordering) is the armed clean day.
    let today = arr.last().unwrap();
    assert_eq!(
        today["has_data"], true,
        "an armed heartbeat's clean day must render has_data=true (green), got {today}"
    );
}
