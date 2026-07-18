mod common;
use common::*;

#[tokio::test]
async fn rollup_day_aggregates_checks_and_overlap_incident() {
    let (pool, _d) = fresh_pool().await;
    // monitor id 1
    sqlx::query("INSERT INTO monitors (name,type,url,expected_status_codes,interval_seconds,timeout_seconds,confirmation_threshold,recovery_threshold,retry_interval_seconds,status,created_at,updated_at) VALUES ('m','http','https://x','200-299',300,30,3,1,30,'up',0,0)").execute(&pool).await.unwrap();
    // day 2000-01-01 UTC bounds:
    let (ds, _de) = vigil::rollup::day_bounds("2000-01-01");
    // 3 up checks + 1 down check within the day
    for t in [ds+10, ds+20, ds+30] { sqlx::query("INSERT INTO checks (monitor_id,checked_at,status,response_time_ms) VALUES (1,?,'up',100)").bind(t).execute(&pool).await.unwrap(); }
    sqlx::query("INSERT INTO checks (monitor_id,checked_at,status,response_time_ms) VALUES (1,?,'down',null)").bind(ds+40).execute(&pool).await.unwrap();
    // an incident spanning from the previous day into this day (overlap): started ds-100, resolved ds+200 => 200s in-day
    sqlx::query("INSERT INTO incidents (monitor_id,started_at,resolved_at,duration_seconds,cause) VALUES (1,?,?,?, 'status')").bind(ds-100).bind(ds+200).bind(300).execute(&pool).await.unwrap();
    vigil::rollup::rollup_day(&pool, "2000-01-01").await.unwrap();
    let (up,down,samp): (i64,i64,i64) = sqlx::query_as("SELECT up_count,down_count,sample_count FROM check_aggregates_daily WHERE monitor_id=1 AND day='2000-01-01'").fetch_one(&pool).await.unwrap();
    assert_eq!((up,down,samp),(3,1,4));
    let down_secs: f64 = sqlx::query_scalar("SELECT (100.0 - uptime_pct)/100.0*86400 FROM check_aggregates_daily WHERE monitor_id=1 AND day='2000-01-01'").fetch_one(&pool).await.unwrap();
    assert!((down_secs-200.0).abs() < 10.0, "overlap incident contributes ~200s downtime (uptime_pct is 2-dp rounded, ~8.6s granularity), got {down_secs}");
}

#[test]
fn day_bounds_matches_known_epoch() {
    // 2000-01-01T00:00:00Z is a well-known epoch value.
    let (start, end) = vigil::rollup::day_bounds("2000-01-01");
    assert_eq!(start, 946_684_800);
    assert_eq!(end, 946_684_800 + 86_400);
}

#[tokio::test]
async fn rollup_day_is_idempotent() {
    let (pool, _d) = fresh_pool().await;
    sqlx::query("INSERT INTO monitors (name,type,url,expected_status_codes,interval_seconds,timeout_seconds,confirmation_threshold,recovery_threshold,retry_interval_seconds,status,created_at,updated_at) VALUES ('m','http','https://x','200-299',300,30,3,1,30,'up',0,0)").execute(&pool).await.unwrap();
    let (ds, _de) = vigil::rollup::day_bounds("2000-01-01");
    for t in [ds+10, ds+20, ds+30] { sqlx::query("INSERT INTO checks (monitor_id,checked_at,status,response_time_ms) VALUES (1,?,'up',100)").bind(t).execute(&pool).await.unwrap(); }
    sqlx::query("INSERT INTO checks (monitor_id,checked_at,status,response_time_ms) VALUES (1,?,'down',null)").bind(ds+40).execute(&pool).await.unwrap();

    vigil::rollup::rollup_day(&pool, "2000-01-01").await.unwrap();
    vigil::rollup::rollup_day(&pool, "2000-01-01").await.unwrap();

    let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM check_aggregates_daily WHERE monitor_id=1 AND day='2000-01-01'").fetch_one(&pool).await.unwrap();
    assert_eq!(n, 1, "rollup_day must upsert, not insert duplicate rows");

    let (up, down, samp): (i64, i64, i64) = sqlx::query_as("SELECT up_count,down_count,sample_count FROM check_aggregates_daily WHERE monitor_id=1 AND day='2000-01-01'").fetch_one(&pool).await.unwrap();
    assert_eq!((up, down, samp), (3, 1, 4), "re-rolling the same day should recompute the same values, not accumulate");
}
