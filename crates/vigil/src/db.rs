use sqlx::sqlite::{SqliteAutoVacuum, SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
use std::str::FromStr;

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

async fn run_migrations(pool: &sqlx::SqlitePool) -> anyhow::Result<()> {
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS schema_migrations (version INTEGER PRIMARY KEY, applied_at INTEGER NOT NULL)",
    )
    .execute(pool)
    .await?;
    let done: Option<i64> = sqlx::query_scalar("SELECT version FROM schema_migrations WHERE version=1")
        .fetch_optional(pool)
        .await?;
    if done.is_none() {
        let sql = include_str!("../migrations/0001_init.sql"); // path is relative to src/db.rs
        let mut tx = pool.begin().await?;
        for stmt in sql.split(';') {
            let s = stmt.trim();
            if !s.is_empty() {
                sqlx::query(s).execute(&mut *tx).await?;
            }
        }
        sqlx::query("INSERT INTO schema_migrations (version,applied_at) VALUES (1,0)")
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
    }
    Ok(())
}
