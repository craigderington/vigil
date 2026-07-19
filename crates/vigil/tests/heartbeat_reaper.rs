//! P4.1 Task 6: the heartbeat reaper — the other half of heartbeat liveness.
//! `record_ping` (Task 5) arms/recovers on an inbound ping; `reap_once`
//! drives a pinged-then-silent heartbeat DOWN once it's overdue by
//! `interval_seconds + heartbeat_grace_seconds`, opening an incident and
//! dispatching `heartbeat_missed`. The due-query is single-arm
//! (`last_ping_at IS NOT NULL`) so a never-pinged heartbeat (the switch is
//! unarmed, per Task 5) is never reaped, and the DOWN `UPDATE` re-asserts
//! the staleness predicate so a ping that lands between the SELECT and the
//! UPDATE can never be raced into a false incident. Heartbeats aren't
//! anchor-gated — a missed ping is real signal regardless of the local
//! connection's state.
//!
//! `reap_one_respects_fresh_db_row_over_stale_snapshot` below is the actual
//! regression test for that SELECT-to-UPDATE race: it calls the (now `pub`)
//! `reap_one` directly with a deliberately stale in-memory snapshot against
//! a fresh db row, bypassing `reap_once`'s due-SELECT entirely so only the
//! UPDATE's own staleness predicate can prevent a false DOWN.
//! `fresh_row_filtered_at_due_select` is a distinct, narrower case (a fresh
//! row never matches the due-SELECT in the first place).

mod common;
use common::*;

/// Real wall-clock epoch seconds — `reap_once`'s own `now()` reads
/// `SystemTime::now()`, so tests that need to land inside/outside the
/// overdue window must compute offsets from the same clock (mirrors
/// `certcheck_ssl.rs`'s `now_epoch` convention).
fn now_epoch() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

/// Inserts a heartbeat monitor directly (bypassing the API) with explicit
/// `interval_seconds`/`heartbeat_grace_seconds`/`status`/`last_ping_at`/
/// `created_at`. Returns the monitor id.
#[allow(clippy::too_many_arguments)]
async fn seed_heartbeat(
    pool: &sqlx::SqlitePool,
    status: &str,
    is_paused: bool,
    interval_seconds: i64,
    grace_seconds: i64,
    last_ping_at: Option<i64>,
    created_at: i64,
) -> i64 {
    let token = vigil::heartbeat::generate_token();
    sqlx::query_scalar(
        "INSERT INTO monitors (name, type, heartbeat_token, interval_seconds, heartbeat_grace_seconds, \
         confirmation_threshold, recovery_threshold, last_ping_at, status, is_paused, created_at, updated_at) \
         VALUES (?, 'heartbeat', ?, ?, ?, 1, 1, ?, ?, ?, ?, ?) RETURNING id",
    )
    .bind("cron")
    .bind(&token)
    .bind(interval_seconds)
    .bind(grace_seconds)
    .bind(last_ping_at)
    .bind(status)
    .bind(is_paused)
    .bind(created_at)
    .bind(created_at)
    .fetch_one(pool)
    .await
    .unwrap()
}

/// Attaches an active `email` notification channel to `monitor_id` with the
/// given `triggers` JSON array string. Deliberately separate from
/// `common::seed_monitor_with_email_channel`, which hardcodes
/// `["down","recovered"]` — a `heartbeat_missed` dispatch would be filtered
/// out by `deliver()` against that trigger set (S3 of the task brief), so
/// the reaper's own alert test needs a channel explicitly opted into
/// `heartbeat_missed`.
async fn attach_email_channel(pool: &sqlx::SqlitePool, monitor_id: i64, triggers: &str) -> i64 {
    let config = r#"{"host":"h","port":25,"security":"none","from":"f@b","to":["a@b"]}"#;
    let cid: i64 = sqlx::query_scalar(
        "INSERT INTO notification_channels (name, type, config, is_active, created_at) \
         VALUES (?, 'email', ?, 1, ?) RETURNING id",
    )
    .bind("seed-channel")
    .bind(config)
    .bind(now_epoch())
    .fetch_one(pool)
    .await
    .unwrap();

    sqlx::query("INSERT INTO monitor_notifications (monitor_id, channel_id, triggers) VALUES (?, ?, ?)")
        .bind(monitor_id)
        .bind(cid)
        .bind(triggers)
        .execute(pool)
        .await
        .unwrap();

    cid
}

#[tokio::test]
async fn overdue_heartbeat_goes_down() {
    let env = test_state().await;
    let n = now_epoch();
    // interval 60 + grace 60 = 120s window; last ping 200s ago is overdue.
    let mid = seed_heartbeat(&env.state.db, "up", false, 60, 60, Some(n - 200), n - 1000).await;
    attach_email_channel(&env.state.db, mid, r#"["heartbeat_missed"]"#).await;

    vigil::heartbeat::reap_once(&env.state).await.unwrap();

    let status: String = sqlx::query_scalar("SELECT status FROM monitors WHERE id=?")
        .bind(mid)
        .fetch_one(&env.state.db)
        .await
        .unwrap();
    assert_eq!(status, "down", "an overdue heartbeat must be reaped to down");

    let incident: Option<(i64, Option<String>)> = sqlx::query_as(
        "SELECT id, cause FROM incidents WHERE monitor_id=? AND resolved_at IS NULL",
    )
    .bind(mid)
    .fetch_optional(&env.state.db)
    .await
    .unwrap();
    let (_, cause) = incident.expect("reaping must open an incident");
    assert_eq!(cause.as_deref(), Some("heartbeat"), "incident cause must be 'heartbeat'");

    assert_eq!(
        env.sent.lock().unwrap().len(),
        1,
        "a heartbeat_missed notification must be delivered to the attached channel"
    );
}

#[tokio::test]
async fn within_grace_not_reaped() {
    let env = test_state().await;
    let n = now_epoch();
    // interval 60 + grace 60 = 120s window; last ping 30s ago is NOT overdue.
    let mid = seed_heartbeat(&env.state.db, "up", false, 60, 60, Some(n - 30), n - 1000).await;

    vigil::heartbeat::reap_once(&env.state).await.unwrap();

    let status: String = sqlx::query_scalar("SELECT status FROM monitors WHERE id=?")
        .bind(mid)
        .fetch_one(&env.state.db)
        .await
        .unwrap();
    assert_eq!(status, "up", "a heartbeat still inside its grace window must not be reaped");

    let incident_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM incidents WHERE monitor_id=?")
        .bind(mid)
        .fetch_one(&env.state.db)
        .await
        .unwrap();
    assert_eq!(incident_count, 0);
}

#[tokio::test]
async fn never_pinged_not_reaped() {
    let env = test_state().await;
    let n = now_epoch();
    let mid = seed_heartbeat(&env.state.db, "pending", false, 60, 60, None, n - 10_000).await;

    vigil::heartbeat::reap_once(&env.state).await.unwrap();

    let status: String = sqlx::query_scalar("SELECT status FROM monitors WHERE id=?")
        .bind(mid)
        .fetch_one(&env.state.db)
        .await
        .unwrap();
    assert_eq!(status, "pending", "a never-pinged heartbeat must stay pending (switch unarmed)");

    let incident_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM incidents WHERE monitor_id=?")
        .bind(mid)
        .fetch_one(&env.state.db)
        .await
        .unwrap();
    assert_eq!(incident_count, 0, "a never-pinged heartbeat must never open an incident");

    assert!(
        env.sent.lock().unwrap().is_empty(),
        "a never-pinged heartbeat must never dispatch an alert"
    );
}

/// NOTE on naming: this covers the due-SELECT filter case, not the
/// UPDATE-vs-race guard. Because `last_ping_at` here is already fresh
/// *before* `reap_once` even runs its SELECT, the row never matches the
/// due-query's own staleness predicate — `reap_one` is never invoked for
/// this monitor at all. This test would still pass even if the DOWN
/// UPDATE's staleness predicate in `reap_one` were deleted entirely; it
/// only proves the SELECT's filter works. The actual reap-vs-ping race
/// (a snapshot going stale *between* the SELECT and the UPDATE) is covered
/// separately by `reap_one_respects_fresh_db_row_over_stale_snapshot` below,
/// which calls `reap_one` directly with a stale snapshot against a fresh db
/// row.
#[tokio::test]
async fn fresh_row_filtered_at_due_select() {
    let env = test_state().await;
    let n = now_epoch();
    // status='up' but last_ping_at is essentially "now" — not stale at all,
    // so the due-query's own predicate excludes this row before reap_one is
    // ever called.
    let mid = seed_heartbeat(&env.state.db, "up", false, 60, 60, Some(n), n - 1000).await;

    vigil::heartbeat::reap_once(&env.state).await.unwrap();

    let status: String = sqlx::query_scalar("SELECT status FROM monitors WHERE id=?")
        .bind(mid)
        .fetch_one(&env.state.db)
        .await
        .unwrap();
    assert_eq!(status, "up", "a fresh row must be filtered out at the due-SELECT");

    let incident_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM incidents WHERE monitor_id=?")
        .bind(mid)
        .fetch_one(&env.state.db)
        .await
        .unwrap();
    assert_eq!(incident_count, 0, "no incident may be opened for a row the due-SELECT never selected");
}

/// The genuine reap-vs-ping race regression test. `fresh_row_filtered_at_due_select`
/// above only proves that a fresh row is filtered out at `reap_once`'s
/// due-SELECT — `reap_one` is never even invoked for it, so that test would
/// still pass if the DOWN `UPDATE`'s staleness predicate were deleted
/// entirely. This test instead calls `heartbeat::reap_one` (now `pub`)
/// directly with a deliberately STALE in-memory `Monitor` snapshot while the
/// DB row underneath is FRESH — exactly what happens when a real ping lands
/// via `record_ping` between `reap_once`'s SELECT and a queued `reap_one`
/// call for that same monitor. Only the UPDATE's own fresh-read predicate
/// (not the SELECT, which is bypassed here) can prevent the false DOWN.
#[tokio::test]
async fn reap_one_respects_fresh_db_row_over_stale_snapshot() {
    let env = test_state().await;
    let n = now_epoch();

    // interval 60 + grace 60 = 120s window. Seed the DB row as FRESH — a
    // ping just landed (last_ping_at = n, status = 'up').
    let mid = seed_heartbeat(&env.state.db, "up", false, 60, 60, Some(n), n - 1000).await;

    // Fetch the row back into a `Monitor`, then hand-corrupt the in-memory
    // copy to look overdue (last_ping_at = n - 10_000), simulating a
    // snapshot taken by `reap_once`'s due-SELECT *before* the ping above
    // refreshed the DB row.
    let mut stale_snapshot: vigil::models::Monitor =
        sqlx::query_as("SELECT * FROM monitors WHERE id = ?")
            .bind(mid)
            .fetch_one(&env.state.db)
            .await
            .unwrap();
    assert_eq!(stale_snapshot.last_ping_at, Some(n), "sanity: db row starts fresh");
    stale_snapshot.last_ping_at = Some(n - 10_000);

    // Call reap_one directly with the stale snapshot — reap_once's
    // due-SELECT is bypassed entirely, so only the UPDATE's own
    // fresh-db-read staleness predicate can save this monitor.
    vigil::heartbeat::reap_one(&env.state, &stale_snapshot, n).await.unwrap();

    let status: String = sqlx::query_scalar("SELECT status FROM monitors WHERE id=?")
        .bind(mid)
        .fetch_one(&env.state.db)
        .await
        .unwrap();
    assert_eq!(
        status, "up",
        "the UPDATE must re-read last_ping_at fresh from the db and refuse to reap a monitor \
         that was actually pinged, even when handed a stale in-memory snapshot"
    );

    let incident_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM incidents WHERE monitor_id=?")
        .bind(mid)
        .fetch_one(&env.state.db)
        .await
        .unwrap();
    assert_eq!(incident_count, 0, "no incident may be opened when the db row is actually fresh");
}

#[tokio::test]
async fn already_down_not_reopened() {
    let env = test_state().await;
    let n = now_epoch();
    let mid = seed_heartbeat(&env.state.db, "down", false, 60, 60, Some(n - 500), n - 1000).await;
    sqlx::query("INSERT INTO incidents (monitor_id, started_at, cause) VALUES (?, ?, 'heartbeat')")
        .bind(mid)
        .bind(n - 500)
        .execute(&env.state.db)
        .await
        .unwrap();

    vigil::heartbeat::reap_once(&env.state).await.unwrap();

    let status: String = sqlx::query_scalar("SELECT status FROM monitors WHERE id=?")
        .bind(mid)
        .fetch_one(&env.state.db)
        .await
        .unwrap();
    assert_eq!(status, "down");

    let open_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM incidents WHERE monitor_id=? AND resolved_at IS NULL",
    )
    .bind(mid)
    .fetch_one(&env.state.db)
    .await
    .unwrap();
    assert_eq!(open_count, 1, "an already-down heartbeat must not accumulate a second open incident");
}

#[tokio::test]
async fn reaper_ignores_non_heartbeat() {
    let env = test_state().await;
    let n = now_epoch();

    // An up http monitor with a stale last_checked_at — the reaper must
    // never touch it (guards the type filter against an accidental
    // OR-loosening of the due-query).
    let http_id: i64 = sqlx::query_scalar(
        "INSERT INTO monitors (name, type, url, status, is_paused, last_checked_at, created_at, updated_at) \
         VALUES (?, 'http', ?, 'up', 0, ?, ?, ?) RETURNING id",
    )
    .bind("http-mon")
    .bind("https://example.com")
    .bind(n - 10_000)
    .bind(n - 10_000)
    .bind(n - 10_000)
    .fetch_one(&env.state.db)
    .await
    .unwrap();

    let hb_id = seed_heartbeat(&env.state.db, "up", false, 60, 60, Some(n - 200), n - 1000).await;

    vigil::heartbeat::reap_once(&env.state).await.unwrap();

    let http_status: String = sqlx::query_scalar("SELECT status FROM monitors WHERE id=?")
        .bind(http_id)
        .fetch_one(&env.state.db)
        .await
        .unwrap();
    assert_eq!(http_status, "up", "a non-heartbeat monitor must never be touched by the reaper");

    let hb_status: String = sqlx::query_scalar("SELECT status FROM monitors WHERE id=?")
        .bind(hb_id)
        .fetch_one(&env.state.db)
        .await
        .unwrap();
    assert_eq!(hb_status, "down", "the overdue heartbeat must still be reaped");
}

#[tokio::test]
async fn offline_anchor_still_reaps() {
    let env = test_state_offline().await;
    let n = now_epoch();
    let mid = seed_heartbeat(&env.state.db, "up", false, 60, 60, Some(n - 200), n - 1000).await;

    vigil::heartbeat::reap_once(&env.state).await.unwrap();

    let status: String = sqlx::query_scalar("SELECT status FROM monitors WHERE id=?")
        .bind(mid)
        .fetch_one(&env.state.db)
        .await
        .unwrap();
    assert_eq!(status, "down", "heartbeats must reap even while the anchor reports offline");
}
