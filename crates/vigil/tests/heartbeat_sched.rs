//! P4.1 Task 4: heartbeat monitors must never be probe-scheduled. They are
//! driven entirely by inbound pings + a reaper (Tasks 5/6); if the scheduler
//! ever enqueued one, `worker::run_check` would fall through to the HTTP
//! prober against a NULL url, producing a false DOWN that fights the
//! ping-driven state. These tests cover the three scheduler-facing
//! touch-points (catch-up on restart, `worker::run_check` guard, and the
//! `check_now` handler); `reschedule_from_db`'s exclusion (which covers all
//! `SchedCmd::Upsert` from create/update/resume) is exercised indirectly
//! through those same code paths in the other P4.1 test files.

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

/// Catch-up on restart (scheduler.rs `run_scheduler` bootstrap query) must
/// skip heartbeat monitors while still picking up ordinary ones.
#[tokio::test]
async fn heartbeat_not_caught_up_on_start() {
    let env = test_state().await;
    let now = 1_700_000_000i64;

    let hb_id: i64 = sqlx::query_scalar(
        "INSERT INTO monitors (name, type, status, is_paused, created_at, updated_at) \
         VALUES (?, 'heartbeat', 'pending', 0, ?, ?) RETURNING id",
    )
    .bind("hb-mon")
    .bind(now)
    .bind(now)
    .fetch_one(&env.state.db)
    .await
    .unwrap();

    let http_id: i64 = sqlx::query_scalar(
        "INSERT INTO monitors (name, type, url, status, is_paused, created_at, updated_at) \
         VALUES (?, 'http', ?, 'pending', 0, ?, ?) RETURNING id",
    )
    .bind("http-mon")
    .bind("https://example.com")
    .bind(now)
    .bind(now)
    .fetch_one(&env.state.db)
    .await
    .unwrap();

    let mut sched = vigil::scheduler::SchedState::new();
    vigil::scheduler::catch_up(&env.state.db, &mut sched).await;

    assert!(
        !sched.is_scheduled(hb_id),
        "heartbeat monitor must NOT be caught up on restart"
    );
    assert!(
        sched.is_scheduled(http_id),
        "non-heartbeat monitor must still be caught up on restart"
    );
}

/// `worker::run_check` must be a no-op for a heartbeat monitor — no `checks`
/// row, no status change — even if the scheduler somehow enqueued one
/// (belt-and-suspenders guard).
#[tokio::test]
async fn worker_run_check_heartbeat_is_noop() {
    let env = test_state().await;
    let now = 1_700_000_000i64;

    let hb_id: i64 = sqlx::query_scalar(
        "INSERT INTO monitors (name, type, status, is_paused, created_at, updated_at) \
         VALUES (?, 'heartbeat', 'pending', 0, ?, ?) RETURNING id",
    )
    .bind("hb-mon")
    .bind(now)
    .bind(now)
    .fetch_one(&env.state.db)
    .await
    .unwrap();

    vigil::worker::run_check(&env.state, hb_id).await;

    let checks: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM checks WHERE monitor_id = ?")
        .bind(hb_id)
        .fetch_one(&env.state.db)
        .await
        .unwrap();
    assert_eq!(checks, 0, "worker::run_check must never probe a heartbeat monitor");

    let status: String = sqlx::query_scalar("SELECT status FROM monitors WHERE id = ?")
        .bind(hb_id)
        .fetch_one(&env.state.db)
        .await
        .unwrap();
    assert_eq!(status, "pending", "heartbeat monitor status must be untouched");
}

/// `POST /api/monitors/:id/check-now` on a heartbeat monitor must be
/// rejected (not enqueued) rather than triggering a probe.
#[tokio::test]
async fn check_now_heartbeat_rejected() {
    let env = test_state().await;
    let now = 1_700_000_000i64;
    let a = serve(env.state.clone()).await;

    let hb_id: i64 = sqlx::query_scalar(
        "INSERT INTO monitors (name, type, status, is_paused, created_at, updated_at) \
         VALUES (?, 'heartbeat', 'pending', 0, ?, ?) RETURNING id",
    )
    .bind("hb-mon")
    .bind(now)
    .bind(now)
    .fetch_one(&env.state.db)
    .await
    .unwrap();

    let r = reqwest::Client::new()
        .post(format!("http://{a}/api/monitors/{hb_id}/check-now"))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let body: serde_json::Value = r.json().await.unwrap();
    assert_eq!(body["ok"], false, "check-now on a heartbeat must report ok:false, got {body}");
    assert!(body.get("error").is_some(), "expected an error message, got {body}");

    // Belt-and-suspenders: confirm nothing was actually enqueued/probed.
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    let checks: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM checks WHERE monitor_id = ?")
        .bind(hb_id)
        .fetch_one(&env.state.db)
        .await
        .unwrap();
    assert_eq!(checks, 0);
    let status: String = sqlx::query_scalar("SELECT status FROM monitors WHERE id = ?")
        .bind(hb_id)
        .fetch_one(&env.state.db)
        .await
        .unwrap();
    assert_eq!(status, "pending");
}
