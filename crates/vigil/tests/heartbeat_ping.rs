//! P4.1 Task 5: `GET|POST /ping/:token` receiver — the atomic recovery gate.
//! `record_ping` runs the whole read-old-status/update/close-incident
//! sequence as one `BEGIN IMMEDIATE` transaction so a concurrent reaper can
//! never interleave, and `pending -> up` is treated as *arming* (no
//! incident to close, no `recovered` notification) while `down -> up` is a
//! real recovery.

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

const NOW: i64 = 1_700_000_000;

/// Inserts a heartbeat monitor directly (bypassing the API, which forces
/// `status='pending'`) with a generated token and the given `status` /
/// `is_paused`. If `with_channel`, attaches an active email channel with
/// `triggers = ["down","recovered"]` (mirrors
/// `common::seed_monitor_with_email_channel`, but for a heartbeat monitor).
/// Returns `(monitor_id, token)`.
async fn seed_heartbeat(
    pool: &sqlx::SqlitePool,
    status: &str,
    is_paused: bool,
    with_channel: bool,
) -> (i64, String) {
    let token = vigil::heartbeat::generate_token();
    let mid: i64 = sqlx::query_scalar(
        "INSERT INTO monitors (name, type, heartbeat_token, heartbeat_grace_seconds, \
         confirmation_threshold, recovery_threshold, status, is_paused, created_at, updated_at) \
         VALUES (?, 'heartbeat', ?, 60, 1, 1, ?, ?, ?, ?) RETURNING id",
    )
    .bind("cron")
    .bind(&token)
    .bind(status)
    .bind(is_paused)
    .bind(NOW)
    .bind(NOW)
    .fetch_one(pool)
    .await
    .unwrap();

    if with_channel {
        let config = r#"{"host":"h","port":25,"security":"none","from":"f@b","to":["a@b"]}"#;
        let cid: i64 = sqlx::query_scalar(
            "INSERT INTO notification_channels (name, type, config, is_active, created_at) \
             VALUES (?, 'email', ?, 1, ?) RETURNING id",
        )
        .bind("seed-channel")
        .bind(config)
        .bind(NOW)
        .fetch_one(pool)
        .await
        .unwrap();

        sqlx::query(
            "INSERT INTO monitor_notifications (monitor_id, channel_id, triggers) VALUES (?, ?, ?)",
        )
        .bind(mid)
        .bind(cid)
        .bind(r#"["down","recovered"]"#)
        .execute(pool)
        .await
        .unwrap();
    }

    (mid, token)
}

#[tokio::test]
async fn ping_unknown_token_404() {
    let env = test_state().await;
    let a = serve(env.state.clone()).await;

    let r = reqwest::Client::new()
        .get(format!("http://{a}/ping/doesnotexist"))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 404);
}

#[tokio::test]
async fn ping_updates_last_ping_at() {
    let env = test_state().await;
    let a = serve(env.state.clone()).await;
    let (mid, token) = seed_heartbeat(&env.state.db, "pending", false, false).await;

    let r = reqwest::Client::new().post(format!("http://{a}/ping/{token}")).send().await.unwrap();
    assert_eq!(r.status(), 200);

    let last_ping_at: Option<i64> =
        sqlx::query_scalar("SELECT last_ping_at FROM monitors WHERE id=?")
            .bind(mid)
            .fetch_one(&env.state.db)
            .await
            .unwrap();
    assert!(last_ping_at.is_some(), "last_ping_at must be set on ping");
}

#[tokio::test]
async fn ping_recovers_down_heartbeat() {
    let env = test_state().await;
    let a = serve(env.state.clone()).await;
    let (mid, token) = seed_heartbeat(&env.state.db, "down", false, true).await;
    let iid: i64 = sqlx::query_scalar(
        "INSERT INTO incidents (monitor_id, started_at, cause) VALUES (?, ?, 'heartbeat') RETURNING id",
    )
    .bind(mid)
    .bind(NOW - 500)
    .fetch_one(&env.state.db)
    .await
    .unwrap();

    let r = reqwest::Client::new().post(format!("http://{a}/ping/{token}")).send().await.unwrap();
    assert_eq!(r.status(), 200);

    let status: String = sqlx::query_scalar("SELECT status FROM monitors WHERE id=?")
        .bind(mid)
        .fetch_one(&env.state.db)
        .await
        .unwrap();
    assert_eq!(status, "up", "down heartbeat must recover to up on ping");

    let resolved_at: Option<i64> =
        sqlx::query_scalar("SELECT resolved_at FROM incidents WHERE id=?")
            .bind(iid)
            .fetch_one(&env.state.db)
            .await
            .unwrap();
    assert!(resolved_at.is_some(), "the open incident must be resolved");

    assert_eq!(
        env.sent.lock().unwrap().len(),
        1,
        "a recovered notification must be delivered for a real down->up recovery"
    );
}

#[tokio::test]
async fn first_ping_arms_pending_no_false_recovery() {
    let env = test_state().await;
    let a = serve(env.state.clone()).await;
    let (mid, token) = seed_heartbeat(&env.state.db, "pending", false, true).await;

    let r = reqwest::Client::new().post(format!("http://{a}/ping/{token}")).send().await.unwrap();
    assert_eq!(r.status(), 200);

    let status: String = sqlx::query_scalar("SELECT status FROM monitors WHERE id=?")
        .bind(mid)
        .fetch_one(&env.state.db)
        .await
        .unwrap();
    assert_eq!(status, "up", "the first ping must arm the heartbeat to up");

    assert!(
        env.sent.lock().unwrap().is_empty(),
        "arming a never-pinged pending heartbeat must NOT dispatch a recovered notification"
    );

    let incident_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM incidents WHERE monitor_id=?")
            .bind(mid)
            .fetch_one(&env.state.db)
            .await
            .unwrap();
    assert_eq!(incident_count, 0, "arming must not create a phantom incident");
}

#[tokio::test]
async fn ping_up_heartbeat_no_new_incident() {
    let env = test_state().await;
    let a = serve(env.state.clone()).await;
    let (mid, token) = seed_heartbeat(&env.state.db, "up", false, false).await;
    sqlx::query("UPDATE monitors SET last_ping_at=? WHERE id=?")
        .bind(NOW - 100)
        .bind(mid)
        .execute(&env.state.db)
        .await
        .unwrap();

    let r = reqwest::Client::new().post(format!("http://{a}/ping/{token}")).send().await.unwrap();
    assert_eq!(r.status(), 200);

    let (status, last_ping_at): (String, i64) =
        sqlx::query_as("SELECT status, last_ping_at FROM monitors WHERE id=?")
            .bind(mid)
            .fetch_one(&env.state.db)
            .await
            .unwrap();
    assert_eq!(status, "up");
    assert!(last_ping_at > NOW - 100, "last_ping_at must advance");

    let incident_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM incidents WHERE monitor_id=?")
            .bind(mid)
            .fetch_one(&env.state.db)
            .await
            .unwrap();
    assert_eq!(incident_count, 0, "an already-up heartbeat must not open an incident on ping");
}

#[tokio::test]
async fn ping_paused_heartbeat_stays_paused() {
    let env = test_state().await;
    let a = serve(env.state.clone()).await;
    let (mid, token) = seed_heartbeat(&env.state.db, "paused", true, false).await;

    let r = reqwest::Client::new().post(format!("http://{a}/ping/{token}")).send().await.unwrap();
    assert_eq!(r.status(), 200);

    let (status, last_ping_at): (String, Option<i64>) =
        sqlx::query_as("SELECT status, last_ping_at FROM monitors WHERE id=?")
            .bind(mid)
            .fetch_one(&env.state.db)
            .await
            .unwrap();
    assert_eq!(status, "paused", "a paused heartbeat must stay paused on ping");
    assert!(last_ping_at.is_some(), "last_ping_at must still be recorded while paused");
}
