#[tokio::test]
async fn migrations_apply_and_fk_cascade_works() {
    let dir = tempfile::tempdir().unwrap();
    let pool = vigil::db::connect(dir.path().join("t.db").to_str().unwrap()).await.unwrap();
    let mid: i64 = sqlx::query_scalar(
        "INSERT INTO monitors (name,type,url,expected_status_codes,interval_seconds,timeout_seconds,\
         confirmation_threshold,recovery_threshold,retry_interval_seconds,status,created_at,updated_at)\
         VALUES ('m','http','https://x','200-299',300,30,3,1,30,'pending',0,0) RETURNING id")
        .fetch_one(&pool).await.unwrap();
    sqlx::query("INSERT INTO checks (monitor_id,checked_at,status) VALUES (?,0,'up')")
        .bind(mid).execute(&pool).await.unwrap();
    sqlx::query("DELETE FROM monitors WHERE id=?").bind(mid).execute(&pool).await.unwrap();
    let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM checks").fetch_one(&pool).await.unwrap();
    assert_eq!(n, 0, "FK cascade must delete child rows");
}
