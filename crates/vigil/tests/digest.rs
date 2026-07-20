mod common;
use common::test_state;
use vigil::digest::build;
use vigil::rollup::{day_bounds, day_str};

fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64
}

// yesterday's date string + its [ds,de) bounds
fn yesterday() -> (String, i64, i64) {
    let d = day_str(now() - 86_400);
    let (ds, de) = day_bounds(&d);
    (d, ds, de)
}

async fn seed_monitor(db: &sqlx::SqlitePool, name: &str, kind: &str) -> i64 {
    sqlx::query_scalar(
        "INSERT INTO monitors (name, type, url, created_at, updated_at) VALUES (?, ?, 'https://x', 0, 0) RETURNING id",
    ).bind(name).bind(kind).fetch_one(db).await.unwrap()
}

#[tokio::test]
async fn build_counts_yesterday_incident_downtime() {
    let env = test_state().await;
    let (day, ds, _de) = yesterday();
    let mid = seed_monitor(&env.state.db, "api", "http").await;
    // a 1-hour outage inside yesterday, and one check so had_any_check=true
    sqlx::query("INSERT INTO incidents (monitor_id, started_at, resolved_at, cause) VALUES (?, ?, ?, 'timeout')")
        .bind(mid).bind(ds + 3600).bind(ds + 7200).execute(&env.state.db).await.unwrap();
    sqlx::query("INSERT INTO checks (monitor_id, checked_at, status) VALUES (?, ?, 'up')")
        .bind(mid).bind(ds + 100).execute(&env.state.db).await.unwrap();

    let s = build(&env.state, &day).await.unwrap();
    assert_eq!(s.day, day);
    assert_eq!(s.fleet.monitors_total, 1);
    assert_eq!(s.fleet.incidents, 1);
    assert_eq!(s.fleet.downtime_seconds, 3600);
    assert_eq!(s.fleet.clean_monitors, 0);
    assert!(s.fleet.uptime_pct.unwrap() < 100.0 && s.fleet.uptime_pct.unwrap() > 95.0);
    assert_eq!(s.incidents.len(), 1);
    assert_eq!(s.incidents[0].monitor_name, "api");
    assert_eq!(s.incidents[0].duration_seconds, Some(3600));
}

#[tokio::test]
async fn maintenance_covered_outage_is_excluded_from_uptime() {
    let env = test_state().await;
    let (day, ds, de) = yesterday();
    let mid = seed_monitor(&env.state.db, "api", "http").await;
    sqlx::query("INSERT INTO checks (monitor_id, checked_at, status) VALUES (?, ?, 'up')")
        .bind(mid).bind(ds + 100).execute(&env.state.db).await.unwrap();
    // outage fully inside a maintenance window covering the whole day for this monitor
    sqlx::query("INSERT INTO incidents (monitor_id, started_at, resolved_at, cause) VALUES (?, ?, ?, 'timeout')")
        .bind(mid).bind(ds + 3600).bind(ds + 7200).execute(&env.state.db).await.unwrap();
    let target = format!("[{mid}]");
    // Window covers only the OUTAGE (ds+3000..ds+7800 ⊇ the ds+3600..ds+7200
    // incident), NOT the whole day — otherwise eff_denom=0 → uptime_pct=None
    // and the Some(100.0) assertion would fail (uptime.rs eff_denom<=0 branch).
    let _ = de;
    sqlx::query(
        "INSERT INTO maintenance_windows (name, scope, target_ref, starts_at, ends_at, recurrence, suppress, is_active, created_at) \
         VALUES ('w','monitors',?,?,?,NULL,'alerts',1,0)",
    ).bind(target).bind(ds + 3000).bind(ds + 7800).execute(&env.state.db).await.unwrap();

    let s = build(&env.state, &day).await.unwrap();
    assert_eq!(s.fleet.uptime_pct, Some(100.0), "outage fully inside maintenance → excluded, fleet 100%");
    assert_eq!(s.fleet.clean_monitors, 1);
    assert_eq!(s.fleet.downtime_seconds, 0);
}

#[tokio::test]
async fn armed_heartbeat_counts_as_having_data() {
    let env = test_state().await;
    let (day, _ds, _de) = yesterday();
    let mid = seed_monitor(&env.state.db, "cron", "heartbeat").await;
    sqlx::query("UPDATE monitors SET last_ping_at = ? WHERE id = ?").bind(now()).bind(mid).execute(&env.state.db).await.unwrap();

    let s = build(&env.state, &day).await.unwrap();
    // no incidents, armed → clean, fleet 100%
    assert_eq!(s.fleet.clean_monitors, 1);
    assert_eq!(s.fleet.uptime_pct, Some(100.0));
}

#[tokio::test]
async fn expirations_surface_invalid_cert_and_unqueryable_domain() {
    let env = test_state().await;
    let (day, _ds, _de) = yesterday();
    let mid = seed_monitor(&env.state.db, "site", "http").await;
    sqlx::query("UPDATE monitors SET ssl_check_enabled = 1, domain_check_enabled = 1 WHERE id = ?")
        .bind(mid).execute(&env.state.db).await.unwrap();
    sqlx::query("INSERT INTO ssl_certs (monitor_id, is_valid, days_remaining, invalid_alerted) VALUES (?, 0, -2, 0)")
        .bind(mid).execute(&env.state.db).await.unwrap();
    sqlx::query("INSERT INTO domain_info (monitor_id, queryable, days_remaining) VALUES (?, 0, NULL)")
        .bind(mid).execute(&env.state.db).await.unwrap();

    let s = build(&env.state, &day).await.unwrap();
    let ssl = s.expirations.iter().find(|e| e.kind == "ssl").unwrap();
    assert_eq!(ssl.flag, "invalid");
    let dom = s.expirations.iter().find(|e| e.kind == "domain").unwrap();
    assert_eq!(dom.flag, "unknown");
    assert_eq!(dom.days_remaining, None);
}

#[tokio::test]
async fn quiet_day_is_all_green_and_sendable() {
    let env = test_state().await;
    let (day, ds, _de) = yesterday();
    let mid = seed_monitor(&env.state.db, "ok", "http").await;
    sqlx::query("INSERT INTO checks (monitor_id, checked_at, status) VALUES (?, ?, 'up')")
        .bind(mid).bind(ds + 100).execute(&env.state.db).await.unwrap();

    let s = build(&env.state, &day).await.unwrap();
    assert_eq!(s.fleet.uptime_pct, Some(100.0));
    assert_eq!(s.fleet.clean_monitors, 1);
    assert!(s.incidents.is_empty());
    assert!(s.currently_down.is_empty());
}
