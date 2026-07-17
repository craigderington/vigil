mod common; use common::*; use vigil::models::*;
async fn run_once(state:&vigil::app::AppState, m:&Monitor, ok:bool) -> Monitor {
    let out = ProbeOutcome{ ok, response_time_ms:Some(5), status_code:Some(if ok{200}else{503}),
        error_message:None, resolved_ip:None, cause: if ok{None}else{Some(Cause::Status)} };
    vigil::engine::apply_result(state, m, &out).await.unwrap();
    sqlx::query_as("SELECT * FROM monitors WHERE id=?").bind(m.id).fetch_one(&state.db).await.unwrap()
}
#[tokio::test] async fn down_then_recover_opens_and_closes_incident() {
    let env = test_state().await;
    let mid = seed_monitor_with_email_channel(&env.state.db).await;
    sqlx::query("UPDATE monitors SET confirmation_threshold=1, recovery_threshold=1 WHERE id=?")
        .bind(mid).execute(&env.state.db).await.unwrap();
    let m: Monitor = sqlx::query_as("SELECT * FROM monitors WHERE id=?").bind(mid).fetch_one(&env.state.db).await.unwrap();
    let m = run_once(&env.state, &m, false).await;                 // fail once (threshold 1) => DOWN
    assert_eq!(m.status, Status::Down);
    let open: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM incidents WHERE monitor_id=? AND resolved_at IS NULL")
        .bind(mid).fetch_one(&env.state.db).await.unwrap();
    assert_eq!(open, 1);
    assert_eq!(env.sent.lock().unwrap().len(), 1);                 // down email
    let _m = run_once(&env.state, &m, true).await;                 // recover
    let open: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM incidents WHERE monitor_id=? AND resolved_at IS NULL")
        .bind(mid).fetch_one(&env.state.db).await.unwrap();
    assert_eq!(open, 0);
    assert_eq!(env.sent.lock().unwrap().len(), 2);                 // recovered email
}
#[tokio::test]
#[allow(unused_variables)] // `env` is intentionally rebound below to switch to the offline anchor
async fn offline_suppresses_alerts_and_keeps_incident_open() {
    let env = test_state().await; // NOTE: build with an Offline anchor:
    // helper variant test_state_offline() sets AnchorGate::with_prober(|| false) and calls probe_and_update once.
    let env = test_state_offline().await;
    let mid = seed_monitor_with_email_channel(&env.state.db).await;
    sqlx::query("UPDATE monitors SET confirmation_threshold=1 WHERE id=?").bind(mid).execute(&env.state.db).await.unwrap();
    let m: Monitor = sqlx::query_as("SELECT * FROM monitors WHERE id=?").bind(mid).fetch_one(&env.state.db).await.unwrap();
    let m = run_once(&env.state, &m, false).await;
    assert_eq!(m.status, Status::Unknown);
    assert_eq!(env.sent.lock().unwrap().len(), 0, "no alert while offline");
    let open: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM incidents WHERE monitor_id=?").bind(mid)
        .fetch_one(&env.state.db).await.unwrap();
    assert_eq!(open, 0, "offline never opens an incident");
}
