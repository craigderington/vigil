mod common;
use common::fresh_pool;
use vigil::settings_store as s;

#[tokio::test]
async fn renotify_and_digest_defaults_then_roundtrip() {
    let (pool, _dir) = fresh_pool().await;

    // defaults
    assert_eq!(s::renotify_hours(&pool).await, 6);
    assert_eq!(s::renotify_tick_seconds(&pool).await, 300);
    assert!(!s::digest_enabled(&pool).await);
    assert_eq!(s::digest_time(&pool).await, "08:00");
    assert_eq!(s::digest_tick_seconds(&pool).await, 60);
    assert_eq!(s::digest_recipients(&pool).await, Vec::<i64>::new());

    // round-trip
    s::set(&pool, "notify.renotify_hours", "12").await.unwrap();
    s::set(&pool, "notify.digest_enabled", "1").await.unwrap();
    s::set(&pool, "notify.digest_time", "07:30").await.unwrap();
    s::set(&pool, "notify.digest_recipients", "[3,5]").await.unwrap();

    assert_eq!(s::renotify_hours(&pool).await, 12);
    assert!(s::digest_enabled(&pool).await);
    assert_eq!(s::digest_time(&pool).await, "07:30");
    assert_eq!(s::digest_recipients(&pool).await, vec![3, 5]);
}

#[tokio::test]
async fn digest_enabled_is_false_for_any_non_one() {
    let (pool, _dir) = fresh_pool().await;
    s::set(&pool, "notify.digest_enabled", "0").await.unwrap();
    assert!(!s::digest_enabled(&pool).await);
    s::set(&pool, "notify.digest_enabled", "true").await.unwrap();
    assert!(!s::digest_enabled(&pool).await, "only \"1\" is true");
}

use axum::extract::State;
use axum::Json;
use serde_json::json;
use vigil::api::settings::{get_settings, update_settings, UpdateSettingsDto};

#[tokio::test]
async fn settings_put_then_get_roundtrips_digest_recipients_as_array() {
    let env = common::test_state().await;
    let state = env.state.clone();

    let dto = UpdateSettingsDto {
        anchors: None,
        cooldown_minutes: None,
        retention_days: None,
        accent: None,
        renotify_hours: Some(9),
        digest_enabled: Some(true),
        digest_time: Some("07:15".into()),
        digest_recipients: Some(json!([2, 4])),
    };
    update_settings(State(state.clone()), Json(dto)).await.unwrap();

    let got = get_settings(State(state)).await.unwrap().0;
    assert_eq!(got["renotify_hours"], 9);
    assert_eq!(got["digest_enabled"], true);
    assert_eq!(got["digest_time"], "07:15");
    assert_eq!(got["digest_recipients"], json!([2, 4]), "recipients GET must be a JSON array, not a string");
}
