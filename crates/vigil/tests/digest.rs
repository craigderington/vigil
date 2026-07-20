mod common;
use common::{test_state, test_state_failing_transport};
use vigil::digest::build;
use vigil::digest::{parse_digest_time, seed_marker_if_absent, send, should_send, tick_once, SendOutcome};
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

#[test]
fn parse_digest_time_and_should_send() {
    assert_eq!(parse_digest_time("08:00"), 8 * 3600);
    assert_eq!(parse_digest_time("07:30"), 7 * 3600 + 1800);
    assert_eq!(parse_digest_time("nonsense"), 8 * 3600); // fallback
    assert_eq!(parse_digest_time("99:99"), 8 * 3600); // out of range → fallback

    let (_d, ds, _de) = yesterday(); // reuse helper; ds is a day start
    // today at fire offset 0: any time in today >= today_start
    let today = day_str(now());
    let (today_start, _) = day_bounds(&today);
    assert!(should_send(today_start + 10, &today, "", 0));
    assert!(!should_send(today_start + 10, &today, &today, 0), "already sent today");
    assert!(!should_send(today_start - 5, &today, "", 10), "before fire time");
    let _ = ds;
}

#[tokio::test]
async fn send_fans_out_to_email_recipients_and_logs() {
    let env = test_state().await;
    // one active email channel
    let cid: i64 = sqlx::query_scalar(
        "INSERT INTO notification_channels (name, type, config, is_active, created_at) \
         VALUES ('e','email','{\"host\":\"h\",\"port\":25,\"security\":\"none\",\"from\":\"f@b\",\"to\":[\"a@b\"]}',1,0) RETURNING id",
    ).fetch_one(&env.state.db).await.unwrap();
    vigil::settings_store::set(&env.state.db, "notify.digest_recipients", &format!("[{cid}]")).await.unwrap();

    let summary = build(&env.state, &day_str(now() - 86_400)).await.unwrap();
    let outcome = send(&env.state, &summary).await;
    assert!(matches!(outcome, SendOutcome::Delivered));
    assert_eq!(env.sent.lock().unwrap().len(), 1);
    let logged: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM notification_log WHERE trigger = 'digest' AND success = 1")
        .fetch_one(&env.state.db).await.unwrap();
    assert_eq!(logged, 1);
}

#[tokio::test]
async fn send_with_no_recipients_audits_and_returns_nothing_to_send() {
    let env = test_state().await; // digest_recipients default []
    let summary = build(&env.state, &day_str(now() - 86_400)).await.unwrap();
    let outcome = send(&env.state, &summary).await;
    assert!(matches!(outcome, SendOutcome::NothingToSend));
    let logged: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM notification_log WHERE trigger = 'digest' AND success = 0 AND error = 'no deliverable email recipients'",
    ).fetch_one(&env.state.db).await.unwrap();
    assert_eq!(logged, 1, "the dead switch must leave an audit row");
}

#[tokio::test]
async fn send_all_failed_returns_all_failed() {
    let env = test_state_failing_transport().await;
    let cid: i64 = sqlx::query_scalar(
        "INSERT INTO notification_channels (name, type, config, is_active, created_at) \
         VALUES ('e','email','{\"host\":\"h\",\"port\":25,\"security\":\"none\",\"from\":\"f@b\",\"to\":[\"a@b\"]}',1,0) RETURNING id",
    ).fetch_one(&env.state.db).await.unwrap();
    vigil::settings_store::set(&env.state.db, "notify.digest_recipients", &format!("[{cid}]")).await.unwrap();
    let summary = build(&env.state, &day_str(now() - 86_400)).await.unwrap();
    assert!(matches!(send(&env.state, &summary).await, SendOutcome::AllFailed));
}

#[tokio::test]
async fn tick_advances_marker_on_success_but_not_on_all_failed() {
    // success path
    let env = test_state().await;
    let cid: i64 = sqlx::query_scalar(
        "INSERT INTO notification_channels (name, type, config, is_active, created_at) \
         VALUES ('e','email','{\"host\":\"h\",\"port\":25,\"security\":\"none\",\"from\":\"f@b\",\"to\":[\"a@b\"]}',1,0) RETURNING id",
    ).fetch_one(&env.state.db).await.unwrap();
    let s = &env.state;
    vigil::settings_store::set(&s.db, "notify.digest_enabled", "1").await.unwrap();
    vigil::settings_store::set(&s.db, "notify.digest_time", "00:00").await.unwrap(); // always past
    vigil::settings_store::set(&s.db, "notify.digest_recipients", &format!("[{cid}]")).await.unwrap();
    tick_once(s).await.unwrap();
    let today = day_str(now());
    assert_eq!(vigil::settings_store::get(&s.db, "notify.digest_last_sent_day", "").await, today);

    // all-failed path: marker must NOT advance
    let env = test_state_failing_transport().await;
    let cid: i64 = sqlx::query_scalar(
        "INSERT INTO notification_channels (name, type, config, is_active, created_at) \
         VALUES ('e','email','{\"host\":\"h\",\"port\":25,\"security\":\"none\",\"from\":\"f@b\",\"to\":[\"a@b\"]}',1,0) RETURNING id",
    ).fetch_one(&env.state.db).await.unwrap();
    let s = &env.state;
    vigil::settings_store::set(&s.db, "notify.digest_enabled", "1").await.unwrap();
    vigil::settings_store::set(&s.db, "notify.digest_time", "00:00").await.unwrap();
    vigil::settings_store::set(&s.db, "notify.digest_recipients", &format!("[{cid}]")).await.unwrap();
    tick_once(s).await.unwrap();
    assert_eq!(vigil::settings_store::get(&s.db, "notify.digest_last_sent_day", "").await, "",
        "a total send failure must NOT advance the marker (retry next tick)");
}

#[tokio::test]
async fn seed_marker_only_when_absent() {
    let env = test_state().await;
    seed_marker_if_absent(&env.state).await.unwrap();
    let seeded = vigil::settings_store::get(&env.state.db, "notify.digest_last_sent_day", "").await;
    assert_eq!(seeded, day_str(now()), "fresh instance seeds today");
    // a present marker is left untouched
    vigil::settings_store::set(&env.state.db, "notify.digest_last_sent_day", "2020-01-01").await.unwrap();
    seed_marker_if_absent(&env.state).await.unwrap();
    assert_eq!(vigil::settings_store::get(&env.state.db, "notify.digest_last_sent_day", "").await, "2020-01-01");
}
