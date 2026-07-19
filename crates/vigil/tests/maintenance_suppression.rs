//! P4.2 Task 4: the three maintenance-suppression effects. During an active
//! maintenance window, alerts are muted (both `alerts` and `checks`
//! suppress kinds) via `dispatch::deliver`'s single funnel; `suppress =
//! 'checks'` additionally pauses probing (`worker::run_check`) and reaping
//! (`heartbeat::reap_once`). The incident itself STILL opens under an
//! `alerts`-only window — only the notification is muted; uptime exclusion
//! (`maintenance_intervals`, P4.2 Task 3) is what nets the outage back out
//! of the uptime %, not a missing incident row.

mod common;
use common::*;
use vigil::models::*;

fn now_epoch() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64
}

/// Inserts an active, one-off, `monitors`-scoped maintenance window
/// covering `[starts_at, ends_at]` for exactly `monitor_id`, with the given
/// `suppress` mode (`"alerts"` | `"checks"`). Scoped to a single monitor
/// (not `scope: "all"`) so tests that seed multiple monitors under
/// different windows don't cross-contaminate each other.
async fn insert_active_window(pool: &sqlx::SqlitePool, monitor_id: i64, starts_at: i64, ends_at: i64, suppress: &str) -> i64 {
    let target = format!("[{monitor_id}]");
    sqlx::query_scalar(
        "INSERT INTO maintenance_windows \
         (name, scope, target_ref, starts_at, ends_at, recurrence, suppress, is_active, created_at) \
         VALUES ('w', 'monitors', ?, ?, ?, NULL, ?, 1, ?) RETURNING id",
    )
    .bind(target)
    .bind(starts_at)
    .bind(ends_at)
    .bind(suppress)
    .bind(starts_at)
    .fetch_one(pool)
    .await
    .unwrap()
}

/// Inserts a heartbeat monitor directly with explicit `last_ping_at`, fixed
/// interval/grace (60s/60s), and `status='up'`. Mirrors
/// `tests/heartbeat_reaper.rs`'s `seed_heartbeat` helper.
async fn seed_heartbeat(pool: &sqlx::SqlitePool, last_ping_at: i64, created_at: i64) -> i64 {
    let token = vigil::heartbeat::generate_token();
    sqlx::query_scalar(
        "INSERT INTO monitors (name, type, heartbeat_token, interval_seconds, heartbeat_grace_seconds, \
         confirmation_threshold, recovery_threshold, last_ping_at, status, is_paused, created_at, updated_at) \
         VALUES (?, 'heartbeat', ?, 60, 60, 1, 1, ?, 'up', 0, ?, ?) RETURNING id",
    )
    .bind("hb")
    .bind(&token)
    .bind(last_ping_at)
    .bind(created_at)
    .bind(created_at)
    .fetch_one(pool)
    .await
    .unwrap()
}

/// Attaches an active `email` channel to `monitor_id` opted into `triggers`
/// (a JSON array string, e.g. `r#"["heartbeat_missed"]"#`).
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

/// `dispatch::deliver`'s maintenance guard: a monitor under an active
/// `alerts`-suppressing window gets NO notification on a DOWN transition,
/// but `engine::apply_result` still opens the incident normally — the guard
/// mutes only the notification funnel, not incident bookkeeping.
#[tokio::test]
async fn alert_suppressed_but_incident_opens() {
    let env = test_state().await;
    let n = now_epoch();
    let mid = seed_monitor_with_email_channel(&env.state.db).await;
    sqlx::query("UPDATE monitors SET confirmation_threshold=1, recovery_threshold=1 WHERE id=?")
        .bind(mid)
        .execute(&env.state.db)
        .await
        .unwrap();

    insert_active_window(&env.state.db, mid, n - 300, n + 300, "alerts").await;

    let m: Monitor = sqlx::query_as("SELECT * FROM monitors WHERE id=?").bind(mid).fetch_one(&env.state.db).await.unwrap();
    let out = ProbeOutcome {
        ok: false,
        response_time_ms: Some(5),
        status_code: Some(503),
        error_message: None,
        resolved_ip: None,
        cause: Some(Cause::Status),
    };
    let ao = vigil::engine::apply_result(&env.state, &m, &out, Connectivity::Online).await.unwrap();
    assert!(ao.incident_id.is_some(), "apply_result must still report an opened incident");

    let open: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM incidents WHERE monitor_id=? AND resolved_at IS NULL")
        .bind(mid)
        .fetch_one(&env.state.db)
        .await
        .unwrap();
    assert_eq!(open, 1, "the incident must still open under an alerts-only maintenance window");

    assert_eq!(
        env.sent.lock().unwrap().len(),
        0,
        "the down alert must be suppressed by the active alerts window"
    );
}

/// `worker::run_check`'s maintenance guard: a monitor under an active
/// `checks`-suppressing window skips the probe entirely (no `checks` row,
/// status untouched) but MUST still advance `next_run_at` — leaving it
/// stale would make the scheduler re-heap the monitor at the same past
/// instant forever (a hot-loop for the whole window).
#[tokio::test]
async fn checks_window_skips_probe_and_advances_next_run() {
    let env = test_state().await;
    let n = now_epoch();
    let stale_next_run = n - 10_000;

    let mid: i64 = sqlx::query_scalar(
        "INSERT INTO monitors (name,type,url,expected_status_codes,interval_seconds,timeout_seconds,\
         confirmation_threshold,recovery_threshold,retry_interval_seconds,status,next_run_at,created_at,updated_at)\
         VALUES ('w','http','https://example.com','200-299',300,30,1,1,30,'up',?,?,?) RETURNING id",
    )
    .bind(stale_next_run)
    .bind(n)
    .bind(n)
    .fetch_one(&env.state.db)
    .await
    .unwrap();

    insert_active_window(&env.state.db, mid, n - 300, n + 300, "checks").await;

    vigil::worker::run_check(&env.state, mid).await;

    let checks: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM checks WHERE monitor_id=?")
        .bind(mid)
        .fetch_one(&env.state.db)
        .await
        .unwrap();
    assert_eq!(checks, 0, "no probe/checks row must be written while checks are suppressed");

    let (status, next_run_at): (String, i64) =
        sqlx::query_as("SELECT status, next_run_at FROM monitors WHERE id=?").bind(mid).fetch_one(&env.state.db).await.unwrap();
    assert_eq!(status, "up", "status must be untouched while checks are suppressed");
    assert_ne!(
        next_run_at, stale_next_run,
        "next_run_at must be advanced off its stale past value (hot-loop guard)"
    );
    assert!(
        next_run_at > n,
        "next_run_at must be advanced to roughly now + interval, got {next_run_at} (now={n})"
    );
}

/// `heartbeat::reap_once`'s maintenance filter is Checks-ONLY: an overdue
/// heartbeat under a `checks` window is skipped entirely (no reap, no
/// incident — checks themselves, i.e. the reap, are paused), while the same
/// overdue heartbeat under an `alerts` window is still reaped and opens an
/// incident (only its notification is muted, via `deliver`'s guard).
#[tokio::test]
async fn checks_window_skips_reaper_but_alerts_window_reaps() {
    let env = test_state().await;
    let n = now_epoch();

    let checks_mid = seed_heartbeat(&env.state.db, n - 200, n - 1000).await;
    insert_active_window(&env.state.db, checks_mid, n - 300, n + 300, "checks").await;

    let alerts_mid = seed_heartbeat(&env.state.db, n - 200, n - 1000).await;
    attach_email_channel(&env.state.db, alerts_mid, r#"["heartbeat_missed"]"#).await;
    insert_active_window(&env.state.db, alerts_mid, n - 300, n + 300, "alerts").await;

    vigil::heartbeat::reap_once(&env.state).await.unwrap();

    let checks_status: String =
        sqlx::query_scalar("SELECT status FROM monitors WHERE id=?").bind(checks_mid).fetch_one(&env.state.db).await.unwrap();
    assert_eq!(checks_status, "up", "a checks-window heartbeat must not be reaped at all");
    let checks_incidents: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM incidents WHERE monitor_id=?").bind(checks_mid).fetch_one(&env.state.db).await.unwrap();
    assert_eq!(checks_incidents, 0, "a checks-window heartbeat must never open an incident");

    let alerts_status: String =
        sqlx::query_scalar("SELECT status FROM monitors WHERE id=?").bind(alerts_mid).fetch_one(&env.state.db).await.unwrap();
    assert_eq!(alerts_status, "down", "an alerts-window heartbeat must still be reaped to down");
    let alerts_incidents: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM incidents WHERE monitor_id=?").bind(alerts_mid).fetch_one(&env.state.db).await.unwrap();
    assert_eq!(
        alerts_incidents, 1,
        "an alerts-window heartbeat must still open an incident — only the alert is muted"
    );

    assert_eq!(
        env.sent.lock().unwrap().len(),
        0,
        "the heartbeat_missed alert must be muted by the active alerts window"
    );
}
