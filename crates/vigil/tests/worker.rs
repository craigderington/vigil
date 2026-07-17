mod common;
use common::*;

#[tokio::test]
async fn run_check_writes_check_row_and_sets_next_run() {
    let s = wiremock::MockServer::start().await;
    wiremock::Mock::given(wiremock::matchers::any())
        .respond_with(wiremock::ResponseTemplate::new(200)).mount(&s).await;
    let env = test_state().await;
    let mid: i64 = sqlx::query_scalar(
        "INSERT INTO monitors (name,type,url,expected_status_codes,interval_seconds,timeout_seconds,\
         confirmation_threshold,recovery_threshold,retry_interval_seconds,status,created_at,updated_at)\
         VALUES ('w','http',?, '200-299',300,30,1,1,30,'pending',0,0) RETURNING id")
        .bind(s.uri()).fetch_one(&env.state.db).await.unwrap();
    vigil::worker::run_check(&env.state, mid).await;
    let checks: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM checks WHERE monitor_id=? AND status='up'").bind(mid).fetch_one(&env.state.db).await.unwrap();
    assert_eq!(checks, 1);
    let nra: Option<i64> = sqlx::query_scalar("SELECT next_run_at FROM monitors WHERE id=?").bind(mid).fetch_one(&env.state.db).await.unwrap();
    assert!(nra.unwrap_or(0) > 0, "worker must set next_run_at");
}
