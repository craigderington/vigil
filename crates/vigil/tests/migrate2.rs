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

#[tokio::test]
async fn upgrade_from_v1_db_applies_only_0002_and_preserves_data() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("v1.db");
    let ps = path.to_str().unwrap().to_string();
    // 1) Simulate a real P1 (version-1-only) database: apply 0001 + record version 1 + insert a legacy monitor.
    {
        use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
        use std::str::FromStr;
        let opts = SqliteConnectOptions::from_str(&format!("sqlite://{ps}")).unwrap()
            .create_if_missing(true).journal_mode(SqliteJournalMode::Wal);
        let pool = SqlitePoolOptions::new().max_connections(1).connect_with(opts).await.unwrap();
        sqlx::query("CREATE TABLE IF NOT EXISTS schema_migrations (version INTEGER PRIMARY KEY, applied_at INTEGER NOT NULL)")
            .execute(&pool).await.unwrap();
        let sql = include_str!("../migrations/0001_init.sql");
        // strip -- line comments then split on ; (mirrors the runner) to apply 0001 manually
        for raw in sql.split(';') {
            let cleaned: String = raw.lines().map(|l| l.split("--").next().unwrap_or("")).collect::<Vec<_>>().join("\n");
            let s = cleaned.trim();
            if !s.is_empty() { sqlx::query(s).execute(&pool).await.unwrap(); }
        }
        sqlx::query("INSERT INTO schema_migrations (version,applied_at) VALUES (1, 0)").execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO monitors (name,type,url,expected_status_codes,interval_seconds,timeout_seconds,confirmation_threshold,recovery_threshold,retry_interval_seconds,status,created_at,updated_at) VALUES ('legacy','http','https://x','200-299',300,30,3,1,30,'up',0,0)")
            .execute(&pool).await.unwrap();
        pool.close().await;
    }
    // 2) Connect via the real version-ordered runner — must apply ONLY 0002.
    let pool = vigil::db::connect(&ps).await.unwrap();
    let maxv: i64 = sqlx::query_scalar("SELECT MAX(version) FROM schema_migrations").fetch_one(&pool).await.unwrap();
    assert_eq!(maxv, 2);
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM schema_migrations").fetch_one(&pool).await.unwrap();
    assert_eq!(count, 2, "exactly versions 1 and 2 recorded (0001 not re-applied)");
    // 0001 did NOT re-run: a re-run would DROP/recreate tables and lose the legacy row.
    let legacy: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM monitors WHERE name='legacy'").fetch_one(&pool).await.unwrap();
    assert_eq!(legacy, 1, "0001 must not re-run — legacy data preserved");
    // 0002 applied: new columns/table exist.
    sqlx::query("SELECT host, dns_record_type FROM monitors").fetch_optional(&pool).await.unwrap();
    sqlx::query("SELECT acknowledged FROM incidents").fetch_optional(&pool).await.unwrap();
    sqlx::query("SELECT sample_count FROM check_aggregates_daily").fetch_optional(&pool).await.unwrap();
}
