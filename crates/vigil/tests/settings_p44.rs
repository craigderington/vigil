mod common;
use axum::extract::State;
use axum::Json;
use serde_json::json;
use vigil::api::settings::{get_settings, update_settings, UpdateSettingsDto};

#[tokio::test]
async fn report_settings_roundtrip() {
    let env = common::test_state().await;
    let dto = UpdateSettingsDto {
        anchors: None, cooldown_minutes: None, retention_days: None, accent: None,
        renotify_hours: None, digest_enabled: None, digest_time: None, digest_recipients: None,
        report_auto_generate: Some(false), report_day_of_month: Some(3),
        report_time: Some("07:15".into()), report_recipients: Some(json!([2, 5])),
    };
    let _ = update_settings(State(env.state.clone()), Json(dto)).await.unwrap();
    let got = get_settings(State(env.state.clone())).await.unwrap().0;
    assert_eq!(got["report_auto_generate"], false);
    assert_eq!(got["report_day_of_month"], 3);
    assert_eq!(got["report_time"], "07:15");
    assert_eq!(got["report_recipients"], json!([2, 5]));
}

#[tokio::test]
async fn accent_setting_default_and_roundtrip() {
    let env = common::test_state().await;

    // Default lives in the get_settings handler, not settings_store.
    let got = get_settings(State(env.state.clone())).await.unwrap().0;
    assert_eq!(got["accent"], "#3FC8E4");

    let dto = UpdateSettingsDto {
        anchors: None, cooldown_minutes: None, retention_days: None,
        accent: Some("the-open-yellow".into()),
        renotify_hours: None, digest_enabled: None, digest_time: None, digest_recipients: None,
        report_auto_generate: None, report_day_of_month: None,
        report_time: None, report_recipients: None,
    };
    let _ = update_settings(State(env.state.clone()), Json(dto)).await.unwrap();
    let got = get_settings(State(env.state.clone())).await.unwrap().0;
    assert_eq!(got["accent"], "the-open-yellow");
}
