mod common;
use common::{seed_monitor_with_email_channel, test_state, test_state_offline};
use vigil::renotify::renotify_once;

fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64
}

// Seed a down monitor (email channel) + an open, unacked incident started `age` secs ago.
async fn seed_down_incident(db: &sqlx::SqlitePool, age: i64) -> (i64, i64) {
    let mid = seed_monitor_with_email_channel(db).await;
    sqlx::query("UPDATE monitors SET status = 'down' WHERE id = ?").bind(mid).execute(db).await.unwrap();
    let iid: i64 = sqlx::query_scalar(
        "INSERT INTO incidents (monitor_id, started_at, acknowledged) VALUES (?, ?, 0) RETURNING id",
    ).bind(mid).bind(now() - age).fetch_one(db).await.unwrap();
    (mid, iid)
}

#[tokio::test]
async fn fires_reminder_when_overdue() {
    let env = test_state().await; // renotify_hours default 6
    seed_down_incident(&env.state.db, 7 * 3600).await; // 7h > 6h
    renotify_once(&env.state).await.unwrap();
    let sent = env.sent.lock().unwrap();
    assert_eq!(sent.len(), 1, "an overdue open incident must fire one reminder");
    assert!(sent[0].1.subject.starts_with("Reminder:"), "subject must be prefixed Reminder:");
    assert!(sent[0].1.body_text.contains("Still down for"), "body must carry elapsed");
}

#[tokio::test]
async fn does_not_fire_within_interval() {
    let env = test_state().await;
    seed_down_incident(&env.state.db, 2 * 3600).await; // 2h < 6h
    renotify_once(&env.state).await.unwrap();
    assert_eq!(env.sent.lock().unwrap().len(), 0);
}

#[tokio::test]
async fn does_not_fire_when_acknowledged_resolved_paused_or_unknown() {
    // acknowledged
    let env = test_state().await;
    let (_m, iid) = seed_down_incident(&env.state.db, 7 * 3600).await;
    sqlx::query("UPDATE incidents SET acknowledged = 1 WHERE id = ?").bind(iid).execute(&env.state.db).await.unwrap();
    renotify_once(&env.state).await.unwrap();
    assert_eq!(env.sent.lock().unwrap().len(), 0, "acknowledged incident is silent");

    // paused
    let env = test_state().await;
    let (mid, _iid) = seed_down_incident(&env.state.db, 7 * 3600).await;
    sqlx::query("UPDATE monitors SET is_paused = 1 WHERE id = ?").bind(mid).execute(&env.state.db).await.unwrap();
    renotify_once(&env.state).await.unwrap();
    assert_eq!(env.sent.lock().unwrap().len(), 0, "paused monitor is silent");

    // status not down (unknown) — the post-reconnect window
    let env = test_state().await;
    let (mid, _iid) = seed_down_incident(&env.state.db, 7 * 3600).await;
    sqlx::query("UPDATE monitors SET status = 'unknown' WHERE id = ?").bind(mid).execute(&env.state.db).await.unwrap();
    renotify_once(&env.state).await.unwrap();
    assert_eq!(env.sent.lock().unwrap().len(), 0, "unknown-status monitor must not re-notify");

    // resolved
    let env = test_state().await;
    let (_mid, iid) = seed_down_incident(&env.state.db, 7 * 3600).await;
    sqlx::query("UPDATE incidents SET resolved_at = ? WHERE id = ?").bind(now()).bind(iid).execute(&env.state.db).await.unwrap();
    renotify_once(&env.state).await.unwrap();
    assert_eq!(env.sent.lock().unwrap().len(), 0, "resolved incident is silent");
}

#[tokio::test]
async fn disabled_when_renotify_hours_zero() {
    let env = test_state().await;
    vigil::settings_store::set(&env.state.db, "notify.renotify_hours", "0").await.unwrap();
    seed_down_incident(&env.state.db, 99 * 3600).await;
    renotify_once(&env.state).await.unwrap();
    assert_eq!(env.sent.lock().unwrap().len(), 0);
}

#[tokio::test]
async fn skips_pass_when_connectivity_offline() {
    let env = test_state_offline().await; // existing helper; anchor.current() == Offline
    seed_down_incident(&env.state.db, 7 * 3600).await;
    renotify_once(&env.state).await.unwrap();
    assert_eq!(env.sent.lock().unwrap().len(), 0, "offline: do not remind about outages");
}

#[tokio::test]
async fn baseline_is_incident_scoped_not_monitor_wide() {
    let env = test_state().await;
    // A PRIOR resolved incident with an old down send at now-8h.
    let mid = seed_monitor_with_email_channel(&env.state.db).await;
    sqlx::query("UPDATE monitors SET status = 'down' WHERE id = ?").bind(mid).execute(&env.state.db).await.unwrap();
    let old_iid: i64 = sqlx::query_scalar(
        "INSERT INTO incidents (monitor_id, started_at, resolved_at, acknowledged) VALUES (?, ?, ?, 0) RETURNING id",
    ).bind(mid).bind(now() - 9 * 3600).bind(now() - 8 * 3600).fetch_one(&env.state.db).await.unwrap();
    sqlx::query(
        "INSERT INTO notification_log (monitor_id, channel_id, incident_id, trigger, sent_at, success) VALUES (?, 1, ?, 'down', ?, 1)",
    ).bind(mid).bind(old_iid).bind(now() - 8 * 3600).execute(&env.state.db).await.unwrap();
    // A NEW incident started 1h ago with NO log row of its own.
    sqlx::query("INSERT INTO incidents (monitor_id, started_at, acknowledged) VALUES (?, ?, 0)")
        .bind(mid).bind(now() - 3600).execute(&env.state.db).await.unwrap();

    renotify_once(&env.state).await.unwrap();
    assert_eq!(env.sent.lock().unwrap().len(), 0,
        "new incident's baseline is its OWN start (1h), not the prior incident's 8h-old send");
}

#[tokio::test]
async fn baseline_advances_no_double_fire() {
    let env = test_state().await;
    // Disable deliver()'s 15-min cooldown so ONLY the re-notify baseline gates
    // the second pass — otherwise the cooldown would suppress it regardless and
    // the test could not distinguish a working baseline from a broken one.
    vigil::settings_store::set(&env.state.db, "notify.cooldown_minutes", "0").await.unwrap();
    seed_down_incident(&env.state.db, 7 * 3600).await;
    renotify_once(&env.state).await.unwrap();
    renotify_once(&env.state).await.unwrap();
    assert_eq!(env.sent.lock().unwrap().len(), 1, "baseline advanced → second immediate pass does not double-fire");
}

#[tokio::test]
async fn deleted_monitor_produces_no_reminder_and_no_panic() {
    let env = test_state().await;
    let (mid, _iid) = seed_down_incident(&env.state.db, 7 * 3600).await;
    // Deleting the monitor FK-cascades its incident (incidents.monitor_id
    // ON DELETE CASCADE), so the scan's JOIN drops it → no reminder, no panic.
    // (This verifies cascade cleanup. The `let Some(m) = m else { continue }`
    // in renotify_once is a defensive guard for a true mid-pass delete race,
    // near-unreachable given the JOIN — not exercised here.)
    sqlx::query("DELETE FROM monitors WHERE id = ?").bind(mid).execute(&env.state.db).await.unwrap();
    renotify_once(&env.state).await.unwrap(); // must not panic
    assert_eq!(env.sent.lock().unwrap().len(), 0);
}

#[tokio::test]
async fn heartbeat_monitor_renotifies_with_reminder() {
    let env = test_state().await;
    // A heartbeat monitor, status='down', with a channel subscribed to
    // heartbeat_missed (seed_monitor_with_email_channel only subscribes
    // ["down","recovered"], so it would wrongly send zero here).
    let mid: i64 = sqlx::query_scalar(
        "INSERT INTO monitors (name, type, url, status, created_at, updated_at) \
         VALUES ('cron','heartbeat',NULL,'down',0,0) RETURNING id",
    ).fetch_one(&env.state.db).await.unwrap();
    let cid: i64 = sqlx::query_scalar(
        "INSERT INTO notification_channels (name, type, config, is_active, created_at) \
         VALUES ('e','email','{\"host\":\"h\",\"port\":25,\"security\":\"none\",\"from\":\"f@b\",\"to\":[\"a@b\"]}',1,0) RETURNING id",
    ).fetch_one(&env.state.db).await.unwrap();
    sqlx::query("INSERT INTO monitor_notifications (monitor_id, channel_id, triggers) VALUES (?, ?, '[\"heartbeat_missed\"]')")
        .bind(mid).bind(cid).execute(&env.state.db).await.unwrap();
    sqlx::query("INSERT INTO incidents (monitor_id, started_at, acknowledged) VALUES (?, ?, 0)")
        .bind(mid).bind(now() - 7 * 3600).execute(&env.state.db).await.unwrap();

    renotify_once(&env.state).await.unwrap();
    let sent = env.sent.lock().unwrap();
    assert_eq!(sent.len(), 1, "heartbeat outage must re-notify via heartbeat_missed");
    assert!(sent[0].1.subject.starts_with("Reminder:"), "reminder prefix for heartbeat too");
    assert!(sent[0].1.body_text.contains("Still down for"));
}

#[tokio::test]
async fn first_down_alert_is_byte_identical_not_decorated() {
    // Guard the "decorate in renotify, DON'T touch templates" choice: the
    // initial (non-reminder) down alert must be exactly as it was pre-P4.3.
    let env = test_state().await;
    let mid = seed_monitor_with_email_channel(&env.state.db).await; // name 'seed'
    let m: vigil::models::Monitor = sqlx::query_as("SELECT * FROM monitors WHERE id = ?")
        .bind(mid).fetch_one(&env.state.db).await.unwrap();
    vigil::notify::dispatch::on_transition(&env.state, &m, vigil::models::Trigger::Down, Some(1)).await.unwrap();
    let sent = env.sent.lock().unwrap();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].1.subject, "🔴 seed is DOWN", "first-alert subject unchanged");
    assert!(!sent[0].1.body_text.contains("Still down for"), "first alert is not decorated");
}
