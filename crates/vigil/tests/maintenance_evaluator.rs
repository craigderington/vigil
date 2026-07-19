//! P4.2 Task 5: the live MAINTENANCE display driver. Maintenance is a
//! CLIENT-SIDE overlay, not a monitor `status` — the backend publishes (a)
//! `maintenance_ids` in the SSE `Snapshot` (the initial set for a fresh
//! client) and (b) an `Event::MaintenanceChanged{id, in_maintenance}` per
//! monitor that enters/exits an active window, via the evaluator
//! (`maintenance_windows::eval_once`). Both `eval_once` and
//! `monitors_in_maintenance` are `pub` specifically so this integration
//! test can drive them directly, one pass at a time, rather than waiting on
//! `run`'s sleep loop.

mod common;
use common::*;
use std::collections::HashSet;
use vigil::events::Event;

fn now_epoch() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64
}

async fn serve(state: vigil::app::AppState) -> std::net::SocketAddr {
    let app = vigil::app::router(state);
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let a = l.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(l, app).await.unwrap();
    });
    a
}

async fn seed_monitor(pool: &sqlx::SqlitePool) -> i64 {
    let n = now_epoch();
    sqlx::query_scalar(
        "INSERT INTO monitors (name, type, url, created_at, updated_at) VALUES (?, 'http', ?, ?, ?) RETURNING id",
    )
    .bind("m")
    .bind("https://example.com")
    .bind(n)
    .bind(n)
    .fetch_one(pool)
    .await
    .unwrap()
}

/// Inserts an active, one-off, `monitors`-scoped maintenance window
/// covering `[starts_at, ends_at]` for exactly `monitor_id`. Mirrors
/// `tests/maintenance_suppression.rs`'s `insert_active_window` helper
/// (each integration-test binary is its own crate, so it can't be shared
/// via `mod common` without becoming part of that shared surface).
async fn insert_active_window(pool: &sqlx::SqlitePool, monitor_id: i64, starts_at: i64, ends_at: i64) -> i64 {
    let target = format!("[{monitor_id}]");
    sqlx::query_scalar(
        "INSERT INTO maintenance_windows \
         (name, scope, target_ref, starts_at, ends_at, recurrence, suppress, is_active, created_at) \
         VALUES ('w', 'monitors', ?, ?, ?, NULL, 'alerts', 1, ?) RETURNING id",
    )
    .bind(target)
    .bind(starts_at)
    .bind(ends_at)
    .bind(starts_at)
    .fetch_one(pool)
    .await
    .unwrap()
}

#[tokio::test]
async fn eval_once_publishes_enter_then_exit_and_updates_prev() {
    let env = test_state().await;
    let n = now_epoch();
    let mid = seed_monitor(&env.state.db).await;
    let wid = insert_active_window(&env.state.db, mid, n - 300, n + 300).await;

    // Subscribe BEFORE calling eval_once, mirroring sse.rs's own
    // subscribe-before-snapshot ordering: a broadcast::Receiver only sees
    // messages sent after it subscribes.
    let mut rx = env.state.bus.subscribe();

    let mut prev: HashSet<i64> = HashSet::new();
    vigil::maintenance_windows::eval_once(&env.state, &mut prev).await;

    match rx.try_recv().expect("expected a MaintenanceChanged event on enter") {
        Event::MaintenanceChanged { id, in_maintenance } => {
            assert_eq!(id, mid);
            assert!(in_maintenance, "monitor must be reported as entering maintenance");
        }
        other => panic!("expected Event::MaintenanceChanged, got {other:?}"),
    }
    assert!(prev.contains(&mid), "eval_once must update prev to include the entered monitor");

    let ids = vigil::maintenance_windows::monitors_in_maintenance(&env.state.db).await;
    assert!(ids.contains(&mid), "monitors_in_maintenance must include an actively-windowed monitor");

    // Deactivate the window — the next pass must publish the exit.
    sqlx::query("UPDATE maintenance_windows SET is_active = 0 WHERE id = ?")
        .bind(wid)
        .execute(&env.state.db)
        .await
        .unwrap();

    vigil::maintenance_windows::eval_once(&env.state, &mut prev).await;

    match rx.try_recv().expect("expected a MaintenanceChanged event on exit") {
        Event::MaintenanceChanged { id, in_maintenance } => {
            assert_eq!(id, mid);
            assert!(!in_maintenance, "monitor must be reported as exiting maintenance");
        }
        other => panic!("expected Event::MaintenanceChanged, got {other:?}"),
    }
    assert!(!prev.contains(&mid), "eval_once must remove the exited monitor from prev");

    let ids_after = vigil::maintenance_windows::monitors_in_maintenance(&env.state.db).await;
    assert!(!ids_after.contains(&mid), "monitors_in_maintenance must exclude the now-inactive window's monitor");

    // A third pass with nothing changed must publish nothing further.
    vigil::maintenance_windows::eval_once(&env.state, &mut prev).await;
    assert!(
        rx.try_recv().is_err(),
        "eval_once must not publish a MaintenanceChanged event when the in-maintenance set is unchanged"
    );
}

#[tokio::test]
async fn monitors_in_maintenance_empty_when_no_active_windows() {
    let env = test_state().await;
    let mid = seed_monitor(&env.state.db).await;
    let ids = vigil::maintenance_windows::monitors_in_maintenance(&env.state.db).await;
    assert!(!ids.contains(&mid));
    assert!(ids.is_empty());
}

/// End-to-end: the SSE endpoint's first frame (the `Snapshot`) carries
/// `maintenance_ids` populated from the live DB state, not just an empty
/// placeholder field.
#[tokio::test]
async fn sse_snapshot_includes_maintenance_ids() {
    let env = test_state().await;
    let n = now_epoch();
    let mid = seed_monitor(&env.state.db).await;
    insert_active_window(&env.state.db, mid, n - 300, n + 300).await;

    let addr = serve(env.state.clone()).await;
    let mut resp = reqwest::Client::new().get(format!("http://{addr}/events")).send().await.unwrap();
    assert!(resp.status().is_success());

    let first = tokio::time::timeout(std::time::Duration::from_secs(5), resp.chunk())
        .await
        .expect("timed out waiting for SSE frame")
        .unwrap()
        .expect("stream ended with no data");
    let text = String::from_utf8_lossy(&first);
    let json_str = text
        .lines()
        .find_map(|l| l.strip_prefix("data: "))
        .expect("first SSE frame must have a data: line");
    let parsed: serde_json::Value = serde_json::from_str(json_str).unwrap();
    assert_eq!(parsed["event"], "snapshot");
    let maintenance_ids = parsed["data"]["maintenance_ids"].as_array().expect("maintenance_ids must be an array");
    assert!(
        maintenance_ids.iter().any(|v| v.as_i64() == Some(mid)),
        "snapshot maintenance_ids must include the actively-windowed monitor, got: {maintenance_ids:?}"
    );
}
