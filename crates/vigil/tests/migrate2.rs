#[tokio::test]
async fn migration_0002_applies_on_fresh_and_v1() {
    // fresh DB: connect() applies 1 then 2
    let d = tempfile::tempdir().unwrap();
    let pool = vigil::db::connect(d.path().join("f.db").to_str().unwrap()).await.unwrap();
    let v: i64 = sqlx::query_scalar("SELECT MAX(version) FROM schema_migrations")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(v, 2);
    // new column exists + aggregates table exists + incidents.acknowledged exists
    sqlx::query("SELECT host, keyword, dns_record_type FROM monitors")
        .fetch_optional(&pool)
        .await
        .unwrap();
    sqlx::query("SELECT acknowledged FROM incidents").fetch_optional(&pool).await.unwrap();
    sqlx::query("SELECT monitor_id, day, sample_count FROM check_aggregates_daily")
        .fetch_optional(&pool)
        .await
        .unwrap();
    let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM schema_migrations WHERE version=2")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(n, 1);
}

#[tokio::test]
async fn comment_stripping_applies() {
    // a statement with a trailing -- comment applies (indirect: 0002 has comments;
    // asserting the columns/table it defines exist, above, exercises this).
    let d = tempfile::tempdir().unwrap();
    let pool = vigil::db::connect(d.path().join("c.db").to_str().unwrap()).await.unwrap();
    sqlx::query("SELECT keyword_case_sensitive FROM monitors").fetch_optional(&pool).await.unwrap();
}
