mod common;
use axum::extract::{Path, State};
use axum::Json;
use vigil::api::reports::{delete, generate as gen_handler, get_one, html, list, GenerateDto};

#[tokio::test]
async fn generate_then_list_then_get_then_html_then_delete() {
    let env = common::test_state().await;
    let g = gen_handler(State(env.state.clone()), Json(GenerateDto { period: "2026-03".into() })).await.unwrap().0;
    let id = g["id"].as_i64().unwrap();

    let listed = list(State(env.state.clone())).await.unwrap().0;
    assert_eq!(listed[0]["label"], "March 2026");
    // empty DB (no monitors) → fleet uptime None, 0 incidents
    assert!(listed[0]["headline"]["uptime_pct"].is_null());
    assert_eq!(listed[0]["headline"]["incidents"], 0);

    let one = get_one(State(env.state.clone()), Path(id)).await.unwrap().0;
    assert_eq!(one["summary"]["period"], "2026-03");

    let page = html(State(env.state.clone()), Path(id)).await.unwrap();
    assert!(page.0.contains("March 2026")); // axum::response::Html<String>

    let d = delete(State(env.state.clone()), Path(id)).await.unwrap().0;
    assert_eq!(d["ok"], true);
}

#[tokio::test]
async fn generate_rejects_future_and_malformed() {
    let env = common::test_state().await;
    assert!(gen_handler(State(env.state.clone()), Json(GenerateDto { period: "nope".into() })).await.is_err());
    assert!(gen_handler(State(env.state.clone()), Json(GenerateDto { period: "3000-01".into() })).await.is_err());
}
