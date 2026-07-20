#[tokio::test]
async fn migration_0005_applies_on_fresh_and_v4() {
    // fresh DB: connect() applies every migration in order (1, 2, 3, 4, 5, ...
    // and whatever is newest — 6 as of P4.4 task 1). This test only cares
    // that 0005 (specifically) applied, so it asserts on the version-5 row
    // below rather than on MAX(version), which later migrations will move
    // (same precedent as migrate3.rs / migrate4.rs).
    let d = tempfile::tempdir().unwrap();
    let pool = vigil::db::connect(d.path().join("f.db").to_str().unwrap()).await.unwrap();
    // maintenance_windows table is selectable
    sqlx::query("SELECT id, name, scope, target_ref, starts_at, ends_at, recurrence, suppress, is_active, created_at FROM maintenance_windows")
        .fetch_optional(&pool)
        .await
        .unwrap();
    let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM schema_migrations WHERE version=5")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(n, 1);
}

#[tokio::test]
async fn upgrade_from_v4_db_applies_only_0005_and_preserves_data() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("v4.db");
    let ps = path.to_str().unwrap().to_string();
    // 1) Simulate a real P4.1 (versions 1-4) database: apply 0001-0004,
    //    record versions 1-4, insert a monitor.
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
            include_str!("../migrations/0004_heartbeat.sql"),
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
        sqlx::query("INSERT INTO schema_migrations (version,applied_at) VALUES (4, 0)").execute(&pool).await.unwrap();
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
    // 2) Connect via the real version-ordered runner — must apply every
    //    migration newer than 4 (0005 onward; 0006 as of P4.4 task 1), and
    //    must NOT re-apply 0001-0004. This test only cares that 0005
    //    (specifically) applied, so it asserts on the version-5 row rather
    //    than on MAX(version)/COUNT(*), which later migrations will move
    //    (same precedent as migrate3.rs / migrate4.rs).
    let pool = vigil::db::connect(&ps).await.unwrap();
    let n05: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM schema_migrations WHERE version=5")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(n05, 1, "version 5 recorded");
    let n1234: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM schema_migrations WHERE version IN (1,2,3,4)")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(n1234, 4, "versions 1-4 still recorded exactly once each (not re-applied)");
    // 0001-0004 did NOT re-run: a re-run would DROP/recreate tables and lose
    // the legacy row.
    let legacy: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM monitors WHERE name='legacy'").fetch_one(&pool).await.unwrap();
    assert_eq!(legacy, 1, "0001-0004 must not re-run — legacy data preserved");
    // 0005 applied: new table exists and is empty.
    let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM maintenance_windows").fetch_one(&pool).await.unwrap();
    assert_eq!(n, 0);
}
