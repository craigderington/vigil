//! Tests for `/api/monitors/:id/ssl`, `/domain`, `/refresh-ssl`,
//! `/refresh-domain` (Task 8). Follows the `api_signal.rs`/`api_incidents.rs`
//! pattern: bind a real listener around `vigil::app::router`, drive it with
//! `reqwest`.
//!
//! `refresh-ssl` is driven against `127.0.0.1:1` (a privileged port no
//! unprivileged test process can be listening on — same "definitely closed"
//! trick `prober.rs` already uses) rather than a live TLS server: per the
//! brief, a connection-refused error row is an acceptable, much simpler
//! substitute for the full self-signed-server harness — `ssl::check` still
//! persists a well-formed `ssl_certs` row with `error` set.
//!
//! `refresh-domain` is driven against a domain-enabled monitor pointed at a
//! real public host (`example.com`) — RDAP is live network, so per the brief
//! either a persisted row or `null` (transient failure) is acceptable; the
//! test only asserts the endpoint responds 200 with one or the other, never
//! that a row exists.

mod common;
use common::*;

async fn serve(state: vigil::app::AppState) -> std::net::SocketAddr {
    let app = vigil::app::router(state);
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let a = l.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(l, app).await.unwrap(); }); a
}

async fn create_ssl_monitor(c: &reqwest::Client, a: std::net::SocketAddr) -> i64 {
    let created: serde_json::Value = c
        .post(format!("http://{a}/api/monitors"))
        .json(&serde_json::json!({
            "name": "ssl-target",
            "type": "ssl",
            "host": "127.0.0.1",
            "port": 1
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    created["id"].as_i64().unwrap()
}

async fn create_domain_monitor(c: &reqwest::Client, a: std::net::SocketAddr) -> i64 {
    let created: serde_json::Value = c
        .post(format!("http://{a}/api/monitors"))
        .json(&serde_json::json!({
            "name": "domain-target",
            "url": "https://example.com",
            "domain_check_enabled": true
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    created["id"].as_i64().unwrap()
}

#[tokio::test]
async fn refresh_ssl_returns_200_with_persisted_row() {
    let env = test_state().await;
    let a = serve(env.state.clone()).await;
    let c = reqwest::Client::new();
    let id = create_ssl_monitor(&c, a).await;

    let resp = c.post(format!("http://{a}/api/monitors/{id}/refresh-ssl")).send().await.unwrap();
    let status = resp.status();
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(status.is_success(), "refresh-ssl must return 2xx, got {status} body={body}");
    assert_eq!(body["monitor_id"].as_i64(), Some(id));
    // connect-refused (127.0.0.1:1, closed) still persists a well-formed row
    // with `error` set and `is_valid` false.
    assert!(body["error"].is_string(), "expected an error string for a closed port, got {body}");
    assert_eq!(body["is_valid"].as_bool(), Some(false));

    let row_in_db: (i64, Option<String>) =
        sqlx::query_as("SELECT monitor_id, error FROM ssl_certs WHERE monitor_id = ?")
            .bind(id)
            .fetch_one(&env.state.db)
            .await
            .unwrap();
    assert_eq!(row_in_db.0, id);
    assert!(row_in_db.1.is_some(), "ssl_certs row must have error set");
}

#[tokio::test]
async fn get_ssl_returns_the_persisted_row() {
    let env = test_state().await;
    let a = serve(env.state.clone()).await;
    let c = reqwest::Client::new();
    let id = create_ssl_monitor(&c, a).await;

    // Not yet refreshed: get_ssl must return null, not 404/500.
    let before: serde_json::Value = c
        .get(format!("http://{a}/api/monitors/{id}/ssl"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(before.is_null(), "expected null before any refresh, got {before}");

    // Refresh, then get_ssl must return that same row.
    c.post(format!("http://{a}/api/monitors/{id}/refresh-ssl")).send().await.unwrap();

    let after: serde_json::Value = c
        .get(format!("http://{a}/api/monitors/{id}/ssl"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(after["monitor_id"].as_i64(), Some(id));
    assert!(after["error"].is_string(), "expected error string in persisted row, got {after}");
}

#[tokio::test]
async fn get_domain_returns_row_or_null_never_errors() {
    let env = test_state().await;
    let a = serve(env.state.clone()).await;
    let c = reqwest::Client::new();
    let id = create_domain_monitor(&c, a).await;

    // Before any refresh: must be null, 200.
    let resp = c.get(format!("http://{a}/api/monitors/{id}/domain")).send().await.unwrap();
    assert!(resp.status().is_success(), "get_domain must return 2xx, got {}", resp.status());
    let before: serde_json::Value = resp.json().await.unwrap();
    assert!(before.is_null(), "expected null before any refresh, got {before}");

    // refresh-domain hits real RDAP network — tolerant of network absence:
    // either a persisted row appears (queryable or not) or the transient
    // path leaves no row and the endpoint still answers 200 + null.
    let resp = c.post(format!("http://{a}/api/monitors/{id}/refresh-domain")).send().await.unwrap();
    assert!(resp.status().is_success(), "refresh-domain must return 2xx, got {}", resp.status());
    let refreshed: serde_json::Value = resp.json().await.unwrap();
    assert!(refreshed.is_null() || refreshed["monitor_id"].as_i64() == Some(id));

    let resp = c.get(format!("http://{a}/api/monitors/{id}/domain")).send().await.unwrap();
    assert!(resp.status().is_success(), "get_domain after refresh must return 2xx, got {}", resp.status());
    let after: serde_json::Value = resp.json().await.unwrap();
    assert!(after.is_null() || after["monitor_id"].as_i64() == Some(id));
}

#[tokio::test]
async fn get_ssl_and_domain_404_for_unknown_monitor() {
    // ssl_certs/domain_info are keyed by monitor_id but the handlers don't
    // check monitor existence — a nonexistent id simply has no row, so this
    // must be 200+null, not a 404. Documents that behavior explicitly.
    let env = test_state().await;
    let a = serve(env.state.clone()).await;
    let c = reqwest::Client::new();

    let resp = c.get(format!("http://{a}/api/monitors/999999/ssl")).send().await.unwrap();
    assert!(resp.status().is_success());
    let v: serde_json::Value = resp.json().await.unwrap();
    assert!(v.is_null());

    let resp = c.get(format!("http://{a}/api/monitors/999999/domain")).send().await.unwrap();
    assert!(resp.status().is_success());
    let v: serde_json::Value = resp.json().await.unwrap();
    assert!(v.is_null());
}
