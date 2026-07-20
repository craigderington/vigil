mod common;
use common::test_state;
use vigil::report::compute::{compute, fleet_uptime_for};
use vigil::report::month_bounds;

async fn seed_http_monitor(db: &sqlx::SqlitePool, name: &str) -> i64 {
    sqlx::query_scalar("INSERT INTO monitors (name, type, url, created_at, updated_at) VALUES (?, 'http', 'https://x', 0, 0) RETURNING id")
        .bind(name).fetch_one(db).await.unwrap()
}
// Durable data for a month with NO raw checks (M1): an aggregate row + an incident.
async fn seed_month(db: &sqlx::SqlitePool, mid: i64, period: &str, day: &str, uptime: f64, avg_ms: f64, up: i64) {
    sqlx::query("INSERT INTO check_aggregates_daily (monitor_id, day, up_count, down_count, avg_response_ms, uptime_pct, incident_count, sample_count) VALUES (?, ?, ?, 0, ?, ?, 0, ?)")
        .bind(mid).bind(day).bind(up).bind(avg_ms).bind(uptime).bind(up).execute(db).await.unwrap();
    let _ = period;
}

#[tokio::test]
async fn compute_uses_durable_aggregates_not_raw_checks() {
    // The M1 must-fix: an old month with aggregates + an incident but ZERO raw checks
    // must still appear with correct uptime (raw-checks gating would blank it).
    let env = test_state().await;
    let (ds, de) = month_bounds("2026-03");
    let mid = seed_http_monitor(&env.state.db, "api").await;
    seed_month(&env.state.db, mid, "2026-03", "2026-03-05", 99.5, 140.0, 200).await;
    // one 1-hour incident inside March, resolved
    sqlx::query("INSERT INTO incidents (monitor_id, started_at, resolved_at, cause) VALUES (?, ?, ?, 'timeout')")
        .bind(mid).bind(ds + 3600).bind(ds + 7200).execute(&env.state.db).await.unwrap();

    let s = compute(&env.state, "2026-03").await.unwrap();
    assert_eq!(s.period, "2026-03");
    assert_eq!(s.label, "March 2026");
    assert_eq!(s.fleet.monitors_total, 1, "durable-gated monitor is included");
    assert_eq!(s.monitors.len(), 1);
    assert!(s.fleet.uptime_pct.unwrap() < 100.0 && s.fleet.uptime_pct.unwrap() > 99.0);
    assert_eq!(s.fleet.incidents, 1);
    assert_eq!(s.fleet.downtime_seconds, 3600);
    assert_eq!(s.monitors[0].avg_ms, Some(140));
    assert_eq!(s.monitors[0].end_status, "up");
    let _ = de;
}

#[tokio::test]
async fn incident_clipped_to_month_both_ends() {
    // Incident started 10 days BEFORE March, resolved on March 2 → only in-month part counts.
    let env = test_state().await;
    let (ds, _de) = month_bounds("2026-03");
    let mid = seed_http_monitor(&env.state.db, "api").await;
    seed_month(&env.state.db, mid, "2026-03", "2026-03-02", 90.0, 100.0, 100).await;
    sqlx::query("INSERT INTO incidents (monitor_id, started_at, resolved_at, cause) VALUES (?, ?, ?, 'timeout')")
        .bind(mid).bind(ds - 10 * 86400).bind(ds + 3600).execute(&env.state.db).await.unwrap();
    let s = compute(&env.state, "2026-03").await.unwrap();
    assert_eq!(s.incidents[0].duration_seconds, Some(3600), "duration clipped to in-month portion");
    assert_eq!(s.fleet.longest_outage.as_ref().unwrap().seconds, 3600);
}

#[tokio::test]
async fn maintenance_covered_outage_excluded_and_delta_none_without_prior() {
    let env = test_state().await;
    let (ds, _de) = month_bounds("2026-03");
    let mid = seed_http_monitor(&env.state.db, "api").await;
    seed_month(&env.state.db, mid, "2026-03", "2026-03-01", 100.0, 120.0, 100).await;
    sqlx::query("INSERT INTO incidents (monitor_id, started_at, resolved_at, cause) VALUES (?, ?, ?, 'timeout')")
        .bind(mid).bind(ds + 3600).bind(ds + 7200).execute(&env.state.db).await.unwrap();
    let target = format!("[{mid}]");
    sqlx::query("INSERT INTO maintenance_windows (name, scope, target_ref, starts_at, ends_at, recurrence, suppress, is_active, created_at) VALUES ('w','monitors',?,?,?,NULL,'alerts',1,0)")
        .bind(target).bind(ds + 3000).bind(ds + 7800).execute(&env.state.db).await.unwrap();
    let s = compute(&env.state, "2026-03").await.unwrap();
    assert_eq!(s.fleet.uptime_pct, Some(100.0));
    assert_eq!(s.fleet.clean_monitors, 1);
    assert_eq!(s.fleet.uptime_delta, None, "no prior month → delta None");
}

#[tokio::test]
async fn paused_and_nodata_monitors_get_rows_but_not_fleet_weight() {
    let env = test_state().await;
    let mid_ok = seed_http_monitor(&env.state.db, "ok").await;
    seed_month(&env.state.db, mid_ok, "2026-03", "2026-03-01", 100.0, 100.0, 50).await;
    let mid_paused = seed_http_monitor(&env.state.db, "paused").await;
    sqlx::query("UPDATE monitors SET is_paused = 1 WHERE id = ?").bind(mid_paused).execute(&env.state.db).await.unwrap();
    let _mid_nodata = seed_http_monitor(&env.state.db, "nodata").await;
    let s = compute(&env.state, "2026-03").await.unwrap();
    assert_eq!(s.monitors.len(), 3, "one row per monitor");
    assert_eq!(s.fleet.monitors_total, 1, "only the had-data non-paused monitor is weighted");
    assert!(s.monitors.iter().any(|m| m.end_status == "paused"));
    assert!(s.monitors.iter().any(|m| m.end_status == "no data"));
}

#[tokio::test]
async fn distinct_alert_counts_and_cert_outlook() {
    let env = test_state().await;
    let (ds, _de) = month_bounds("2026-03");
    let mid = seed_http_monitor(&env.state.db, "site").await;
    sqlx::query("UPDATE monitors SET ssl_check_enabled = 1 WHERE id = ?").bind(mid).execute(&env.state.db).await.unwrap();
    seed_month(&env.state.db, mid, "2026-03", "2026-03-01", 100.0, 100.0, 10).await;
    sqlx::query("INSERT INTO ssl_certs (monitor_id, is_valid, days_remaining, invalid_alerted) VALUES (?, 1, 12, 0)").bind(mid).execute(&env.state.db).await.unwrap();
    // one ssl_expiring alert fanned to TWO channels (same sent_at) → must count ONCE
    for ch in [1, 2] {
        sqlx::query("INSERT INTO notification_log (monitor_id, channel_id, incident_id, trigger, sent_at, success) VALUES (?, ?, NULL, 'ssl_expiring', ?, 1)")
            .bind(mid).bind(ch).bind(ds + 100).execute(&env.state.db).await.unwrap();
    }
    let s = compute(&env.state, "2026-03").await.unwrap();
    assert_eq!(s.fleet.ssl_alerts, 1, "channel fan-out counts as one alert event");
    let ssl = s.cert_outlook.iter().find(|e| e.kind == "ssl").unwrap();
    assert_eq!(ssl.flag, "expiring"); // 12 <= max(ssl_alert_days default 30)
    assert!(s.fleet.expiring_30d >= 1);
}

#[tokio::test]
async fn delta_uses_prior_month_live() {
    let env = test_state().await;
    let mid = seed_http_monitor(&env.state.db, "api").await;
    // Feb: 100% ; Mar: 100% ; delta 0.0
    seed_month(&env.state.db, mid, "2026-02", "2026-02-10", 100.0, 100.0, 100).await;
    seed_month(&env.state.db, mid, "2026-03", "2026-03-10", 100.0, 100.0, 100).await;
    assert_eq!(fleet_uptime_for(&env.state, "2026-02").await.unwrap(), Some(100.0));
    let s = compute(&env.state, "2026-03").await.unwrap();
    assert_eq!(s.fleet.uptime_delta, Some(0.0));
}

#[tokio::test]
async fn open_incident_at_period_end_clips_and_marks_down() {
    // Incident starts inside March and is STILL OPEN (resolved_at NULL) → clips to de,
    // end_status "down". Exercises clip()'s end branch + end_status_at (finding F).
    let env = test_state().await;
    let (ds, de) = month_bounds("2026-03");
    let mid = seed_http_monitor(&env.state.db, "api").await;
    seed_month(&env.state.db, mid, "2026-03", "2026-03-31", 50.0, 100.0, 100).await;
    // opens 1 hour before month-end, never resolves
    sqlx::query("INSERT INTO incidents (monitor_id, started_at, resolved_at, cause) VALUES (?, ?, NULL, 'timeout')")
        .bind(mid).bind(de - 3600).execute(&env.state.db).await.unwrap();
    let s = compute(&env.state, "2026-03").await.unwrap();
    assert_eq!(s.incidents[0].duration_seconds, Some(3600), "open incident clips to period_end");
    assert_eq!(s.incidents[0].resolved_at, None);
    assert_eq!(s.monitors[0].end_status, "down");
    assert_eq!(s.fleet.longest_outage.as_ref().unwrap().seconds, 3600);
    let _ = ds;
}

#[tokio::test]
async fn p95_computed_when_month_within_retention() {
    // Raise retention so a RECENT month's raw checks survive → p95 value branch runs
    // (finding E). Use a month computed from now() to stay within retention + not future.
    let env = test_state().await;
    vigil::settings_store::set(&env.state.db, "retention.raw_days", "3650").await.unwrap();
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64;
    // use the PRIOR month so it is fully in the past (never future)
    let period = vigil::report::prior_month(&vigil::report::month_of(now));
    let (ds, _de) = month_bounds(&period);
    let mid = seed_http_monitor(&env.state.db, "api").await;
    let day = vigil::rollup::day_str(ds);
    seed_month(&env.state.db, mid, &period, &day, 100.0, 100.0, 5).await;
    // 5 checks: 100,110,120,130,500 → p95 index ceil(5*0.95)-1 = 4 → 500
    for (i, v) in [100, 110, 120, 130, 500].iter().enumerate() {
        sqlx::query("INSERT INTO checks (monitor_id, checked_at, status, response_time_ms) VALUES (?, ?, 'up', ?)")
            .bind(mid).bind(ds + 100 + i as i64).bind(v).execute(&env.state.db).await.unwrap();
    }
    let s = compute(&env.state, &period).await.unwrap();
    assert_eq!(s.monitors[0].p95_ms, Some(500));
}

#[tokio::test]
async fn monitor_rows_sorted_worst_uptime_first() {
    let env = test_state().await;
    let (ds, _de) = month_bounds("2026-03");
    let good = seed_http_monitor(&env.state.db, "good").await;
    seed_month(&env.state.db, good, "2026-03", "2026-03-01", 100.0, 100.0, 100).await;
    let bad = seed_http_monitor(&env.state.db, "bad").await;
    seed_month(&env.state.db, bad, "2026-03", "2026-03-01", 50.0, 100.0, 100).await;
    sqlx::query("INSERT INTO incidents (monitor_id, started_at, resolved_at, cause) VALUES (?, ?, ?, 'timeout')")
        .bind(bad).bind(ds + 3600).bind(ds + 3600 + 10 * 86400).execute(&env.state.db).await.unwrap();
    let s = compute(&env.state, "2026-03").await.unwrap();
    assert_eq!(s.monitors[0].name, "bad", "worst uptime first");
    assert_eq!(s.monitors[1].name, "good");
}
