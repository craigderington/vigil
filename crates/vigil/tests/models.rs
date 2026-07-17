use vigil::models::*;

#[test]
fn status_serializes_lowercase() {
    assert_eq!(serde_json::to_string(&Status::Up).unwrap(), "\"up\"");
    assert_eq!(Status::from_db("unknown"), Status::Unknown);
}

#[tokio::test]
async fn monitor_row_decodes() {
    let dir = tempfile::tempdir().unwrap();
    let pool = vigil::db::connect(dir.path().join("m.db").to_str().unwrap()).await.unwrap();
    sqlx::query("INSERT INTO monitors (name,type,url,expected_status_codes,interval_seconds,timeout_seconds,\
        confirmation_threshold,recovery_threshold,retry_interval_seconds,status,created_at,updated_at)\
        VALUES ('m','http','https://x','200-299',300,30,3,1,30,'up',0,0)").execute(&pool).await.unwrap();
    let m: Monitor = sqlx::query_as::<_, Monitor>("SELECT * FROM monitors WHERE id=1").fetch_one(&pool).await.unwrap();
    assert_eq!(m.status, Status::Up);
    assert_eq!(m.url.as_deref(), Some("https://x"));
}
