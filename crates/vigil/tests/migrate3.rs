#[tokio::test]
async fn migration_0003_applies_on_fresh_and_v2() {
    // fresh DB: connect() applies every migration in order (1, 2, 3, ... and
    // whatever is newest — 4 as of P4 task 1). This test only cares that
    // 0003 (specifically) applied, so it asserts on the version-3 row below
    // rather than on MAX(version), which later migrations will move.
    let d = tempfile::tempdir().unwrap();
    let pool = vigil::db::connect(d.path().join("f.db").to_str().unwrap()).await.unwrap();
    // add-on columns on monitors + the two new tables are all selectable
    sqlx::query("SELECT ssl_check_enabled, ssl_alert_days, domain_check_enabled, domain_alert_days FROM monitors")
        .fetch_optional(&pool)
        .await
        .unwrap();
    sqlx::query(
        "SELECT monitor_id, issuer, subject, valid_from, valid_until, days_remaining, is_valid, \
         chain_ok, hostname_match, self_signed, error, alerted_days, invalid_alerted, last_checked \
         FROM ssl_certs",
    )
    .fetch_optional(&pool)
    .await
    .unwrap();
    sqlx::query(
        "SELECT monitor_id, registrar, expiry_date, days_remaining, name_servers, status_codes, \
         queryable, source, alerted_days, last_checked FROM domain_info",
    )
    .fetch_optional(&pool)
    .await
    .unwrap();
    let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM schema_migrations WHERE version=3")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(n, 1);
}

#[tokio::test]
async fn upgrade_from_v2_db_applies_0003_onward_and_preserves_data() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("v2.db");
    let ps = path.to_str().unwrap().to_string();
    // 1) Simulate a real P2 (versions 1+2) database: apply 0001+0002, record
    //    versions 1 and 2, insert a monitor.
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
    //    migration newer than 2 (0003 onward; 0004 as of P4 task 1), and
    //    must NOT re-apply 0001/0002. This test only cares that 0003
    //    (specifically) applied, so it asserts on the version-3 row rather
    //    than on MAX(version)/COUNT(*), which later migrations will move.
    let pool = vigil::db::connect(&ps).await.unwrap();
    let n03: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM schema_migrations WHERE version=3")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(n03, 1, "version 3 recorded");
    let n01: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM schema_migrations WHERE version=1")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(n01, 1, "version 1 still recorded exactly once (0001 not re-applied)");
    let n02: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM schema_migrations WHERE version=2")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(n02, 1, "version 2 still recorded exactly once (0002 not re-applied)");
    // 0001/0002 did NOT re-run: a re-run would DROP/recreate tables and lose the legacy row.
    let legacy: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM monitors WHERE name='legacy'").fetch_one(&pool).await.unwrap();
    assert_eq!(legacy, 1, "0001/0002 must not re-run — legacy data preserved");
    // 0003 applied: new columns/tables exist, and the legacy row got the new
    // columns' defaults.
    let (ssl_enabled, ssl_days, domain_enabled, domain_days): (i64, String, i64, String) = sqlx::query_as(
        "SELECT ssl_check_enabled, ssl_alert_days, domain_check_enabled, domain_alert_days \
         FROM monitors WHERE name='legacy'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(ssl_enabled, 0);
    assert_eq!(ssl_days, "[30,14,7,3,1]");
    assert_eq!(domain_enabled, 0);
    assert_eq!(domain_days, "[45,30,14,7]");
    sqlx::query("SELECT monitor_id, issuer, invalid_alerted FROM ssl_certs").fetch_optional(&pool).await.unwrap();
    sqlx::query("SELECT monitor_id, registrar, source FROM domain_info").fetch_optional(&pool).await.unwrap();
}
