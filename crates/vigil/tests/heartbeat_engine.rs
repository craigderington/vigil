mod common; use common::*; use vigil::models::*;

/// `bulk_set_unknown` reacts to a lost anchor by flipping the whole fleet to
/// `unknown` — except heartbeat monitors, which have no "probe" of their own
/// to suppress; they're driven by the reaper (Tasks 5/6), not the anchor.
#[tokio::test]
async fn bulk_set_unknown_skips_heartbeats() {
    let env = test_state().await;
    let now = 1_700_000_000i64;

    let http_id: i64 = sqlx::query_scalar(
        "INSERT INTO monitors (name, type, url, status, is_paused, created_at, updated_at) \
         VALUES (?, 'http', ?, 'up', 0, ?, ?) RETURNING id",
    )
    .bind("http-mon")
    .bind("https://x")
    .bind(now)
    .bind(now)
    .fetch_one(&env.state.db)
    .await
    .unwrap();

    let hb_id: i64 = sqlx::query_scalar(
        "INSERT INTO monitors (name, type, status, is_paused, created_at, updated_at) \
         VALUES (?, 'heartbeat', 'up', 0, ?, ?) RETURNING id",
    )
    .bind("hb-mon")
    .bind(now)
    .bind(now)
    .fetch_one(&env.state.db)
    .await
    .unwrap();

    vigil::engine::bulk_set_unknown(&env.state).await.unwrap();

    let http_status: String = sqlx::query_scalar("SELECT status FROM monitors WHERE id=?")
        .bind(http_id)
        .fetch_one(&env.state.db)
        .await
        .unwrap();
    let hb_status: String = sqlx::query_scalar("SELECT status FROM monitors WHERE id=?")
        .bind(hb_id)
        .fetch_one(&env.state.db)
        .await
        .unwrap();

    assert_eq!(http_status, "unknown", "non-heartbeat monitor must be swept to unknown");
    assert_eq!(hb_status, "up", "heartbeat monitor must be excluded from the fleet-wide sweep");
}

/// Compile-level proof that `apply_result` now takes the anchor as a
/// parameter rather than reading it internally: passing `Connectivity::Online`
/// explicitly must still open an incident on a failing probe.
#[tokio::test]
async fn apply_result_takes_anchor() {
    let env = test_state().await;
    let mid = seed_monitor_with_email_channel(&env.state.db).await;
    sqlx::query("UPDATE monitors SET confirmation_threshold=1 WHERE id=?")
        .bind(mid)
        .execute(&env.state.db)
        .await
        .unwrap();
    let m: Monitor = sqlx::query_as("SELECT * FROM monitors WHERE id=?")
        .bind(mid)
        .fetch_one(&env.state.db)
        .await
        .unwrap();

    let out = ProbeOutcome {
        ok: false,
        response_time_ms: Some(5),
        status_code: Some(503),
        error_message: None,
        resolved_ip: None,
        cause: Some(Cause::Status),
    };

    let ao = vigil::engine::apply_result(&env.state, &m, &out, Connectivity::Online)
        .await
        .unwrap();
    assert!(ao.incident_id.is_some(), "ok:false with anchor Online must open an incident");

    let open: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM incidents WHERE monitor_id=? AND resolved_at IS NULL",
    )
    .bind(mid)
    .fetch_one(&env.state.db)
    .await
    .unwrap();
    assert_eq!(open, 1);
}
