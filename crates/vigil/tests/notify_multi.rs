//! Multi-channel notify refactor: per-(monitor,channel,trigger) cooldown
//! (not the old single-channel-per-trigger bug), webhook payload shape, and
//! the SMTP username/From fallback helper.

mod common;
use common::*;
use vigil::models::Trigger;
use vigil::notify::auth_user;

/// Proves the cooldown key is `(monitor_id, channel_id, trigger)`, not the
/// old `(monitor_id, trigger)` — with the old key, attaching a 2nd channel
/// to the same monitor+trigger would only ever let ONE of the two channels
/// fire (whichever's cooldown row happened to satisfy `MAX(sent_at)`
/// first), silently dropping the other channel's alert.
#[tokio::test]
async fn two_channels_on_down_both_fire() {
    let env = test_state().await;
    let mid = seed_monitor_with_email_channel(&env.state.db).await;
    attach_webhook_channel(&env.state.db, mid, r#"["down","recovered"]"#).await;

    let m: vigil::models::Monitor = sqlx::query_as("SELECT * FROM monitors WHERE id=?")
        .bind(mid)
        .fetch_one(&env.state.db)
        .await
        .unwrap();

    vigil::notify::dispatch::on_transition(&env.state, &m, Trigger::Down, Some(1))
        .await
        .unwrap();

    assert_eq!(
        env.sent.lock().unwrap().len(),
        1,
        "email channel must fire on down"
    );
    assert_eq!(
        env.sent_http.lock().unwrap().len(),
        1,
        "webhook channel must ALSO fire on the same down — per-channel cooldown, not per-trigger"
    );
}

/// The recorded `NotifyMsg` handed to the http double, serialized, carries
/// the monitor name — i.e. the webhook channel's outgoing notification
/// really is populated from the transition, not a stub.
#[tokio::test]
async fn webhook_payload_shape() {
    let env = test_state().await;
    let mid = seed_monitor_with_webhook_channel(&env.state.db).await;

    let m: vigil::models::Monitor = sqlx::query_as("SELECT * FROM monitors WHERE id=?")
        .bind(mid)
        .fetch_one(&env.state.db)
        .await
        .unwrap();

    vigil::notify::dispatch::on_transition(&env.state, &m, Trigger::Down, Some(1))
        .await
        .unwrap();

    let sent = env.sent_http.lock().unwrap();
    assert_eq!(sent.len(), 1, "webhook channel must fire on down");
    let (channel_type, config, msg) = &sent[0];
    assert_eq!(channel_type, "webhook");
    assert_eq!(config["url"], "http://x");

    let json = serde_json::to_value(msg).unwrap();
    assert_eq!(json["monitor_name"], "seed", "payload must carry the monitor name");
    assert_eq!(json["status"], "down");
}

/// `auth_user` is the credential-selection helper used when building SMTP
/// `Credentials`: prefer the channel's configured username, falling back to
/// the From address when no username is set. `SmtpConfig` itself has no
/// `from` field (the From address lives on `EmailMsg`), so this helper
/// takes both explicitly.
#[test]
fn smtp_username_used_when_set() {
    assert_eq!(
        auth_user(&Some("apikey".to_string()), "no-reply@x.com"),
        "apikey"
    );
    assert_eq!(auth_user(&None, "no-reply@x.com"), "no-reply@x.com");
}
