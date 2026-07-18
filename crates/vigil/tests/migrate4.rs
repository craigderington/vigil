#[tokio::test]
async fn migration_0004_applies_on_fresh_and_v3() {
    // fresh DB: connect() applies 1, 2, 3, then 4
    let d = tempfile::tempdir().unwrap();
    let pool = vigil::db::connect(d.path().join("f.db").to_str().unwrap()).await.unwrap();
    let v: i64 = sqlx::query_scalar("SELECT MAX(version) FROM schema_migrations")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(v, 4);
    // heartbeat columns on monitors are all selectable
    sqlx::query("SELECT heartbeat_token, heartbeat_grace_seconds, last_ping_at FROM monitors")
        .fetch_optional(&pool)
        .await
        .unwrap();
    let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM schema_migrations WHERE version=4")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(n, 1);
}

#[tokio::test]
async fn upgrade_from_v3_db_applies_only_0004_and_preserves_data() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("v3.db");
    let ps = path.to_str().unwrap().to_string();
    // 1) Simulate a real P3 (versions 1+2+3) database: apply 0001+0002+0003,
    //    record versions 1, 2 and 3, insert a monitor.
    {
        use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
        use std::str::FromStr;
        let opts = SqliteConnectOptions::from_str(&format!("sqlite://{ps}"))
            .unwrap()
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal);
        let pool = SqlitePoolOptions::new().max_connections(1).connect_with(opts).await.unwrap();
        sqlx::query("CREATE TABLE IF NOT EXISTS schema_migrations (version INTEGER PRIMARY KEY, applied_at INTEGER NOT NULL)")
            .execute(&pool).await.unwrap();
        for sql in [
            include_str!("../migrations/0001_init.sql"),
            include_str!("../migrations/0002_signal.sql"),
            include_str!("../migrations/0003_certs.sql"),
        ] {
            for raw in sql.split(';') {
                let cleaned: String =
                    raw.lines().map(|l| l.split("--").next().unwrap_or("")).collect::<Vec<_>>().join("\n");
                let s = cleaned.trim();
                if !s.is_empty() {
                    sqlx::query(s).execute(&pool).await.unwrap();
                }
            }
        }
        sqlx::query("INSERT INTO schema_migrations (version,applied_at) VALUES (1, 0)").execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO schema_migrations (version,applied_at) VALUES (2, 0)").execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO schema_migrations (version,applied_at) VALUES (3, 0)").execute(&pool).await.unwrap();
        sqlx::query(
            "INSERT INTO monitors (name,type,url,expected_status_codes,interval_seconds,timeout_seconds,\
             confirmation_threshold,recovery_threshold,retry_interval_seconds,status,created_at,updated_at) \
             VALUES ('legacy','http','https://x','200-299',300,30,3,1,30,'up',0,0)",
        )
        .execute(&pool)
        .await
        .unwrap();
        pool.close().await;
    }
    // 2) Connect via the real version-ordered runner — must apply ONLY 0004.
    let pool = vigil::db::connect(&ps).await.unwrap();
    let maxv: i64 = sqlx::query_scalar("SELECT MAX(version) FROM schema_migrations").fetch_one(&pool).await.unwrap();
    assert_eq!(maxv, 4);
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM schema_migrations").fetch_one(&pool).await.unwrap();
    assert_eq!(count, 4, "exactly versions 1, 2, 3 and 4 recorded (0001/0002/0003 not re-applied)");
    // 0001/0002/0003 did NOT re-run: a re-run would DROP/recreate tables and
    // lose the legacy row.
    let legacy: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM monitors WHERE name='legacy'").fetch_one(&pool).await.unwrap();
    assert_eq!(legacy, 1, "0001/0002/0003 must not re-run — legacy data preserved");
    // 0004 applied: new columns exist, and the legacy row's
    // heartbeat_grace_seconds backfilled to the default 60.
    let (token, grace, last_ping): (Option<String>, i64, Option<i64>) = sqlx::query_as(
        "SELECT heartbeat_token, heartbeat_grace_seconds, last_ping_at FROM monitors WHERE name='legacy'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(token, None);
    assert_eq!(grace, 60, "heartbeat_grace_seconds backfilled to default 60");
    assert_eq!(last_ping, None);
}
