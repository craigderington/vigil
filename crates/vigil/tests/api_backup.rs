mod common;
use common::*;

async fn serve(state: vigil::app::AppState) -> std::net::SocketAddr {
    let app = vigil::app::router(state);
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let a = l.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(l, app).await.unwrap(); });
    a
}

#[tokio::test]
async fn export_returns_valid_sqlite_attachment() {
    let env = test_state().await;
    let a = serve(env.state.clone()).await;
    let c = reqwest::Client::new();

    let resp = c.get(format!("http://{a}/api/backup/export")).send().await.unwrap();
    assert!(resp.status().is_success(), "export status: {}", resp.status());
    let cd = resp.headers().get(reqwest::header::CONTENT_DISPOSITION).unwrap().to_str().unwrap().to_string();
    assert!(cd.contains("attachment; filename=\"vigil-backup-"), "content-disposition: {cd}");
    let bytes = resp.bytes().await.unwrap();
    assert!(bytes.len() >= 16, "body too short");
    assert_eq!(&bytes[..16], b"SQLite format 3\0", "export is not a SQLite database");
}

#[tokio::test]
async fn info_reports_schema_version_and_counts() {
    let env = test_state().await;
    let a = serve(env.state.clone()).await;
    let c = reqwest::Client::new();

    // seed one monitor so counts are non-trivial
    c.post(format!("http://{a}/api/monitors"))
        .json(&serde_json::json!({"name":"x","url":"https://example.com"}))
        .send().await.unwrap();

    let info: serde_json::Value = c.get(format!("http://{a}/api/backup/info")).send().await.unwrap().json().await.unwrap();
    assert_eq!(info["schema_version"].as_i64(), Some(6));
    assert_eq!(info["counts"]["monitors"].as_i64(), Some(1));
    assert!(info["db_size_bytes"].as_i64().unwrap() > 0);
}
