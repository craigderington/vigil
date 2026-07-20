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

// Full round-trip: state captured in an export is restored by importing it,
// including a monitor and a settings value — and a channel secret survives.
#[tokio::test]
async fn export_import_round_trip_restores_data_and_secrets() {
    let env = test_state().await;
    let a = serve(env.state.clone()).await;
    let c = reqwest::Client::new();

    // seed: a monitor + a webhook channel whose config holds a secret token
    let created: serde_json::Value = c.post(format!("http://{a}/api/monitors"))
        .json(&serde_json::json!({"name":"keep","url":"https://example.com"}))
        .send().await.unwrap().json().await.unwrap();
    let mid = created["id"].as_i64().unwrap();
    sqlx::query("INSERT INTO notification_channels (name, type, config, is_active, created_at) VALUES ('hook','webhook',?,1,0)")
        .bind(r#"{"url":"http://x","token":"SECRET123"}"#)
        .execute(&env.state.db).await.unwrap();

    // EXPORT the current state
    let backup = c.get(format!("http://{a}/api/backup/export")).send().await.unwrap().bytes().await.unwrap().to_vec();

    // MUTATE: delete the monitor and change a setting away from its default
    c.delete(format!("http://{a}/api/monitors/{mid}")).send().await.unwrap();
    c.put(format!("http://{a}/api/settings")).json(&serde_json::json!({"retention_days": 99})).send().await.unwrap();
    let list: serde_json::Value = c.get(format!("http://{a}/api/monitors")).send().await.unwrap().json().await.unwrap();
    assert_eq!(list.as_array().unwrap().len(), 0, "precondition: monitor deleted");

    // IMPORT the backup — atomic replace
    let resp = c.post(format!("http://{a}/api/backup/import")).body(backup).send().await.unwrap();
    assert!(resp.status().is_success(), "import status: {}", resp.status());
    let res: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(res["ok"].as_bool(), Some(true));

    // monitor restored, setting reverted to default (30), secret intact
    let list: serde_json::Value = c.get(format!("http://{a}/api/monitors")).send().await.unwrap().json().await.unwrap();
    assert_eq!(list.as_array().unwrap().len(), 1, "monitor restored by import");
    let settings: serde_json::Value = c.get(format!("http://{a}/api/settings")).send().await.unwrap().json().await.unwrap();
    assert_eq!(settings["retention_days"].as_i64(), Some(30), "setting reverted to backup value");
    let cfg: String = sqlx::query_scalar("SELECT config FROM notification_channels WHERE name='hook'")
        .fetch_one(&env.state.db).await.unwrap();
    assert!(cfg.contains("SECRET123"), "channel secret survived round-trip: {cfg}");
}

// A pre-import safety snapshot is written to the data dir before the replace.
#[tokio::test]
async fn import_writes_pre_import_snapshot() {
    let env = test_state().await;
    let a = serve(env.state.clone()).await;
    let c = reqwest::Client::new();
    let backup = c.get(format!("http://{a}/api/backup/export")).send().await.unwrap().bytes().await.unwrap().to_vec();
    let resp = c.post(format!("http://{a}/api/backup/import")).body(backup).send().await.unwrap();
    assert!(resp.status().is_success());

    let dir = std::path::Path::new(&*env.state.db_path).parent().unwrap();
    let has_snapshot = std::fs::read_dir(dir).unwrap()
        .filter_map(|e| e.ok())
        .any(|e| e.file_name().to_string_lossy().starts_with("pre-import-"));
    assert!(has_snapshot, "a pre-import-*.db snapshot must exist in {dir:?}");
}

// Garbage / too-short uploads are rejected before any write; live data intact.
#[tokio::test]
async fn import_rejects_non_sqlite_and_preserves_data() {
    let env = test_state().await;
    let a = serve(env.state.clone()).await;
    let c = reqwest::Client::new();
    c.post(format!("http://{a}/api/monitors"))
        .json(&serde_json::json!({"name":"stay","url":"https://example.com"})).send().await.unwrap();

    for body in [b"abc".to_vec(), b"not a sqlite database at all!!!".to_vec()] {
        let resp = c.post(format!("http://{a}/api/backup/import")).body(body).send().await.unwrap();
        assert_eq!(resp.status(), 400, "non-sqlite upload must be 400");
    }
    let list: serde_json::Value = c.get(format!("http://{a}/api/monitors")).send().await.unwrap().json().await.unwrap();
    assert_eq!(list.as_array().unwrap().len(), 1, "live data untouched by rejected imports");
}

// A backup from a newer schema version is rejected (can't downgrade).
#[tokio::test]
async fn import_rejects_newer_schema_version() {
    let env = test_state().await;
    let a = serve(env.state.clone()).await;
    let c = reqwest::Client::new();

    let dir = tempfile::tempdir().unwrap();
    let craft = dir.path().join("craft.db");
    let pool = vigil::db::connect(craft.to_str().unwrap()).await.unwrap();
    sqlx::query("INSERT INTO schema_migrations (version, applied_at) VALUES (7, 0)").execute(&pool).await.unwrap();
    let clean = dir.path().join("craft-clean.db");
    sqlx::query(&format!("VACUUM INTO '{}'", clean.display())).execute(&pool).await.unwrap();
    drop(pool);
    let bytes = std::fs::read(&clean).unwrap();

    let resp = c.post(format!("http://{a}/api/backup/import")).body(bytes).send().await.unwrap();
    assert_eq!(resp.status(), 400);
    let msg = resp.text().await.unwrap();
    assert!(msg.to_lowercase().contains("newer"), "expected a 'newer version' message, got: {msg}");
}

// An OLDER backup is migrated up before the replace. This is the ONLY test that
// exercises the §5.3 migrate step + the §5.5 ATTACH-of-a-freshly-migrated file
// (the path the spec flags as fragile) — so if the WAL/ATTACH fallback is ever
// needed, this test is what forces it.
#[tokio::test]
async fn import_upgrades_older_backup() {
    let env = test_state().await;
    let a = serve(env.state.clone()).await;
    let c = reqwest::Client::new();

    // Craft a "v5" backup: a full v6 DB minus the reports table and its v6
    // migration marker. Structurally identical to a real v5 DB, because
    // migration 0006 only ADDS the reports table (no ALTERs to other tables).
    let dir = tempfile::tempdir().unwrap();
    let older = dir.path().join("older.db");
    let pool = vigil::db::connect(older.to_str().unwrap()).await.unwrap();
    sqlx::query("INSERT INTO monitors (name, type, url, created_at, updated_at) VALUES ('older','http','https://e.com',0,0)")
        .execute(&pool).await.unwrap();
    sqlx::query("DROP TABLE reports").execute(&pool).await.unwrap();
    sqlx::query("DELETE FROM schema_migrations WHERE version = 6").execute(&pool).await.unwrap();
    let clean = dir.path().join("older-clean.db");
    sqlx::query(&format!("VACUUM INTO '{}'", clean.display())).execute(&pool).await.unwrap();
    drop(pool);
    let bytes = std::fs::read(&clean).unwrap();

    let resp = c.post(format!("http://{a}/api/backup/import")).body(bytes).send().await.unwrap();
    assert!(resp.status().is_success(), "import status: {}", resp.status());
    let res: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(res["migrated"].as_bool(), Some(true), "older backup should be migrated up");
    assert_eq!(res["backup_version"].as_i64(), Some(5));
    assert_eq!(res["schema_version"].as_i64(), Some(6));

    let list: serde_json::Value = c.get(format!("http://{a}/api/monitors")).send().await.unwrap().json().await.unwrap();
    assert_eq!(list.as_array().unwrap().len(), 1, "monitor from the older backup is present after migrate+replace");
}
