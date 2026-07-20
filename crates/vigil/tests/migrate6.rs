#[tokio::test]
async fn migration_0006_creates_reports_table() {
    let d = tempfile::tempdir().unwrap();
    let pool = vigil::db::connect(d.path().join("f.db").to_str().unwrap()).await.unwrap();
    let v: i64 = sqlx::query_scalar("SELECT MAX(version) FROM schema_migrations").fetch_one(&pool).await.unwrap();
    assert_eq!(v, 6);
    // reports table is selectable
    sqlx::query("SELECT id, period_start, period_end, label, generated_at, summary_json, html_path, pdf_path, emailed_at FROM reports")
        .fetch_optional(&pool).await.unwrap();
}
