mod common;
use common::{test_state, test_state_failing_transport};
use vigil::digest::SendOutcome;
use vigil::report::{generate, send_report_email};

async fn seed_email_channel(db: &sqlx::SqlitePool) -> i64 {
    sqlx::query_scalar("INSERT INTO notification_channels (name, type, config, is_active, created_at) VALUES ('e','email','{\"host\":\"h\",\"port\":25,\"security\":\"none\",\"from\":\"f@b\",\"to\":[\"a@b\"]}',1,0) RETURNING id")
        .fetch_one(db).await.unwrap()
}

#[tokio::test]
async fn generate_upserts_idempotently() {
    let env = test_state().await;
    let r1 = generate(&env.state, "2026-03").await.unwrap();
    let r2 = generate(&env.state, "2026-03").await.unwrap();
    assert_eq!(r1.period_start, r2.period_start);
    let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM reports").fetch_one(&env.state.db).await.unwrap();
    assert_eq!(n, 1, "regenerate overwrites, one row per month");
    assert!(r2.summary_json.contains("\"period\":\"2026-03\""));
}

#[tokio::test]
async fn email_fans_out_and_sets_emailed_at() {
    let env = test_state().await;
    let cid = seed_email_channel(&env.state.db).await;
    vigil::settings_store::set(&env.state.db, "report_recipients", &format!("[{cid}]")).await.unwrap();
    let r = generate(&env.state, "2026-03").await.unwrap();
    let outcome = send_report_email(&env.state, &r).await;
    assert!(matches!(outcome, SendOutcome::Delivered));
    assert_eq!(env.sent.lock().unwrap().len(), 1);
    let logged: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM notification_log WHERE trigger='report' AND success=1").fetch_one(&env.state.db).await.unwrap();
    assert_eq!(logged, 1);
    let emailed: Option<i64> = sqlx::query_scalar("SELECT emailed_at FROM reports WHERE id=?").bind(r.id).fetch_one(&env.state.db).await.unwrap();
    assert!(emailed.is_some());
}

#[tokio::test]
async fn email_no_recipients_is_nothing_to_send_with_audit() {
    let env = test_state().await;
    let r = generate(&env.state, "2026-03").await.unwrap();
    let outcome = send_report_email(&env.state, &r).await;
    assert!(matches!(outcome, SendOutcome::NothingToSend));
    let logged: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM notification_log WHERE trigger='report' AND success=0").fetch_one(&env.state.db).await.unwrap();
    assert_eq!(logged, 1);
}

#[tokio::test]
async fn email_all_failed() {
    let env = test_state_failing_transport().await;
    let cid = seed_email_channel(&env.state.db).await;
    vigil::settings_store::set(&env.state.db, "report_recipients", &format!("[{cid}]")).await.unwrap();
    let r = generate(&env.state, "2026-03").await.unwrap();
    assert!(matches!(send_report_email(&env.state, &r).await, SendOutcome::AllFailed));
}
