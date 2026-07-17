//! Typed access to the `settings` key/value table: SMTP defaults, anchor
//! hosts, notification cooldown, retention, etc. Unset keys fall back to
//! the Global-Constraints defaults baked into each typed helper.

use sqlx::SqlitePool;

const DEFAULT_COOLDOWN_MINUTES: i64 = 15;
const DEFAULT_RETENTION_DAYS: i64 = 30;
const DEFAULT_ANCHORS: &str = "1.1.1.1:443,8.8.8.8:443";

/// Reads `key`, returning `default` if the row is absent.
pub async fn get(pool: &SqlitePool, key: &str, default: &str) -> String {
    sqlx::query_scalar::<_, String>("SELECT value FROM settings WHERE key = ?")
        .bind(key)
        .fetch_optional(pool)
        .await
        .ok()
        .flatten()
        .unwrap_or_else(|| default.to_string())
}

/// Upserts `key` = `value`.
pub async fn set(pool: &SqlitePool, key: &str, value: &str) -> anyhow::Result<()> {
    sqlx::query(
        "INSERT INTO settings (key, value) VALUES (?, ?) \
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
    )
    .bind(key)
    .bind(value)
    .execute(pool)
    .await?;
    Ok(())
}

/// `notify.cooldown_minutes` — minutes to suppress a repeat send for the
/// same (monitor, trigger). Default 15.
pub async fn cooldown_minutes(pool: &SqlitePool) -> i64 {
    get(pool, "notify.cooldown_minutes", &DEFAULT_COOLDOWN_MINUTES.to_string())
        .await
        .parse()
        .unwrap_or(DEFAULT_COOLDOWN_MINUTES)
}

/// `retention.raw_days` — how many days of raw `checks` rows to keep
/// before nightly rollup pruning. Default 30.
pub async fn retention_days(pool: &SqlitePool) -> i64 {
    get(pool, "retention.raw_days", &DEFAULT_RETENTION_DAYS.to_string())
        .await
        .parse()
        .unwrap_or(DEFAULT_RETENTION_DAYS)
}

/// `anchors` — comma-separated `host:port` list used by the internet-sanity
/// gate. Default `1.1.1.1:443,8.8.8.8:443`.
pub async fn anchors(pool: &SqlitePool) -> Vec<String> {
    get(pool, "anchors", DEFAULT_ANCHORS)
        .await
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}
