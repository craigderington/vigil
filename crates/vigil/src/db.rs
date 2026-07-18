use sqlx::sqlite::{SqliteAutoVacuum, SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
use std::str::FromStr;
use std::time::{SystemTime, UNIX_EPOCH};

/// Version-ordered migrations, applied in order; each version whose row is
/// absent from `schema_migrations` is applied once, in its own transaction.
/// On a P1 database, version 1 is already recorded (from the old hardcoded
/// runner), so only `0002` (and later) apply on upgrade.
const MIGRATIONS: &[(i64, &str)] = &[
    (1, include_str!("../migrations/0001_init.sql")),
    (2, include_str!("../migrations/0002_signal.sql")),
];

pub async fn connect(db_path: &str) -> anyhow::Result<sqlx::SqlitePool> {
    let opts = SqliteConnectOptions::from_str(&format!("sqlite://{db_path}"))?
        .create_if_missing(true)
        .foreign_keys(true)
        .journal_mode(SqliteJournalMode::Wal)
        .auto_vacuum(SqliteAutoVacuum::Incremental);
    let pool = SqlitePoolOptions::new().max_connections(5).connect_with(opts).await?;
    run_migrations(&pool).await?;
    Ok(pool)
}

/// Strips `--` line comments (to end-of-line, per line — our migrations
/// never have `--` inside a string literal, so this simple approach is
/// sufficient) then splits the joined SQL on `;` into individual statements.
fn split_statements(sql: &str) -> Vec<String> {
    let stripped: String = sql
        .lines()
        .map(|line| match line.find("--") {
            Some(idx) => &line[..idx],
            None => line,
        })
        .collect::<Vec<_>>()
        .join("\n");
    stripped.split(';').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect()
}

async fn run_migrations(pool: &sqlx::SqlitePool) -> anyhow::Result<()> {
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS schema_migrations (version INTEGER PRIMARY KEY, applied_at INTEGER NOT NULL)",
    )
    .execute(pool)
    .await?;
    let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs() as i64;
    for (v, sql) in MIGRATIONS {
        let done: Option<i64> = sqlx::query_scalar("SELECT version FROM schema_migrations WHERE version=?")
            .bind(v)
            .fetch_optional(pool)
            .await?;
        if done.is_some() {
            continue;
        }
        let mut tx = pool.begin().await?;
        for stmt in split_statements(sql) {
            sqlx::query(&stmt).execute(&mut *tx).await?;
        }
        sqlx::query("INSERT INTO schema_migrations (version, applied_at) VALUES (?, ?)")
            .bind(v)
            .bind(now)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
    }
    Ok(())
}
