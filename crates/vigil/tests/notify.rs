mod common;
use common::*;
use vigil::models::Trigger;

#[test]
fn template_fills_variables() {
    let (sub, text, _html) =
        vigil::notify::templates::render(Trigger::Down, &ctx_with("api", "https://x", Some(503)));
    assert!(sub.contains("api"));
    assert!(text.contains("503"));
}

#[tokio::test]
async fn cooldown_suppresses_second_down() {
    let env = test_state().await;
    // seed a channel + attach a monitor with trigger down
    let mid = seed_monitor_with_email_channel(&env.state.db).await; // helper below
    let m: vigil::models::Monitor = sqlx::query_as("SELECT * FROM monitors WHERE id=?")
        .bind(mid)
        .fetch_one(&env.state.db)
        .await
        .unwrap();
    vigil::notify::dispatch::on_transition(&env.state, &m, Trigger::Down, Some(1))
        .await
        .unwrap();
    vigil::notify::dispatch::on_transition(&env.state, &m, Trigger::Down, Some(1))
        .await
        .unwrap();
    assert_eq!(
        env.sent.lock().unwrap().len(),
        1,
        "2nd down within cooldown is suppressed"
    );
}
