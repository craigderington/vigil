mod common;
use common::{test_state, test_state_failing_transport};
use vigil::report::scheduler::{seed_marker_if_absent, should_run_today, tick_once};

fn ts(y: i32, mo: u32, d: u32, h: u32) -> i64 {
    chrono::NaiveDate::from_ymd_opt(y, mo, d).unwrap().and_hms_opt(h, 0, 0).unwrap().and_utc().timestamp()
}

#[test]
fn should_run_today_clamped_and_timed() {
    // day_of_month 1, 08:00 → fires on the 1st at/after 08:00
    assert!(should_run_today(ts(2026, 4, 1, 9), 1, 8 * 3600));
    assert!(!should_run_today(ts(2026, 4, 1, 7), 1, 8 * 3600)); // before 08:00
    // day 31 in April (30 days) clamps to 30 → fires on the 30th
    assert!(should_run_today(ts(2026, 4, 30, 9), 31, 8 * 3600));
    assert!(!should_run_today(ts(2026, 4, 29, 9), 31, 8 * 3600));
}

#[tokio::test]
async fn tick_backfills_missing_months_in_order() {
    let env = test_state().await;
    vigil::settings_store::set(&env.state.db, "report_auto_generate", "1").await.unwrap();
    vigil::settings_store::set(&env.state.db, "report_day_of_month", "1").await.unwrap();
    vigil::settings_store::set(&env.state.db, "report_time", "00:00").await.unwrap(); // day>=1 & 00:00 → always due
    // Marker EXACTLY two months behind the just-ended month → both missing months must
    // generate, in ascending order, and the marker must advance to the just-ended month.
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64;
    let target = vigil::report::prior_month(&vigil::report::month_of(now)); // just-ended month
    let first_missing = vigil::report::prior_month(&target);
    let two_behind = vigil::report::prior_month(&first_missing);
    vigil::settings_store::set(&env.state.db, "report.last_generated_period", &two_behind).await.unwrap();

    tick_once(&env.state).await.unwrap();

    let (ps_first, _) = vigil::report::month_bounds(&first_missing);
    let (ps_target, _) = vigil::report::month_bounds(&target);
    let rows: Vec<i64> = sqlx::query_scalar("SELECT period_start FROM reports ORDER BY period_start").fetch_all(&env.state.db).await.unwrap();
    assert_eq!(rows, vec![ps_first, ps_target], "exactly the two missing months, ascending");
    assert_eq!(vigil::settings_store::get(&env.state.db, "report.last_generated_period", "").await, target, "marker advanced to just-ended month");
}

#[tokio::test]
async fn tick_holds_marker_on_email_failure() {
    let env = test_state_failing_transport().await;
    let cid: i64 = sqlx::query_scalar("INSERT INTO notification_channels (name, type, config, is_active, created_at) VALUES ('e','email','{\"host\":\"h\",\"port\":25,\"security\":\"none\",\"from\":\"f@b\",\"to\":[\"a@b\"]}',1,0) RETURNING id").fetch_one(&env.state.db).await.unwrap();
    vigil::settings_store::set(&env.state.db, "report_auto_generate", "1").await.unwrap();
    vigil::settings_store::set(&env.state.db, "report_day_of_month", "1").await.unwrap();
    vigil::settings_store::set(&env.state.db, "report_time", "00:00").await.unwrap();
    vigil::settings_store::set(&env.state.db, "report_recipients", &format!("[{cid}]")).await.unwrap();
    // marker one month behind → generate the prior month, email fails → marker NOT advanced past it
    let prior = vigil::report::prior_month(&vigil::report::month_of(chrono::Utc::now().timestamp()));
    let before = vigil::report::prior_month(&prior);
    vigil::settings_store::set(&env.state.db, "report.last_generated_period", &before).await.unwrap();
    tick_once(&env.state).await.unwrap();
    let marker = vigil::settings_store::get(&env.state.db, "report.last_generated_period", "").await;
    assert_eq!(marker, before, "email AllFailed → marker held for retry");
}

#[tokio::test]
async fn seed_marker_only_when_absent() {
    let env = test_state().await;
    seed_marker_if_absent(&env.state).await.unwrap();
    let seeded = vigil::settings_store::get(&env.state.db, "report.last_generated_period", "").await;
    assert!(!seeded.is_empty());
    vigil::settings_store::set(&env.state.db, "report.last_generated_period", "2020-01").await.unwrap();
    seed_marker_if_absent(&env.state).await.unwrap();
    assert_eq!(vigil::settings_store::get(&env.state.db, "report.last_generated_period", "").await, "2020-01");
}
