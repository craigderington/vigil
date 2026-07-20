//! Typed access to the `settings` key/value table: SMTP defaults, anchor
//! hosts, notification cooldown, retention, etc. Unset keys fall back to
//! the Global-Constraints defaults baked into each typed helper.

use sqlx::SqlitePool;

const DEFAULT_COOLDOWN_MINUTES: i64 = 15;
const DEFAULT_RETENTION_DAYS: i64 = 30;
const DEFAULT_ANCHORS: &str = "1.1.1.1:443,8.8.8.8:443";
const DEFAULT_CERT_SSL_INTERVAL_SECONDS: i64 = 43_200; // 12h
const DEFAULT_CERT_DOMAIN_INTERVAL_SECONDS: i64 = 86_400; // 24h
const DEFAULT_CERT_TICK_SECONDS: i64 = 60;
const DEFAULT_CERT_CONCURRENCY: i64 = 5;
const DEFAULT_HEARTBEAT_TICK_SECONDS: i64 = 20;
const DEFAULT_MAINTENANCE_TICK_SECONDS: i64 = 30;
const DEFAULT_RENOTIFY_HOURS: i64 = 6;
const DEFAULT_RENOTIFY_TICK_SECONDS: i64 = 300;
const DEFAULT_DIGEST_TIME: &str = "08:00";
const DEFAULT_DIGEST_TICK_SECONDS: i64 = 60;
const DEFAULT_REPORT_DAY_OF_MONTH: i64 = 1;
const DEFAULT_REPORT_TIME: &str = "08:00";
const DEFAULT_REPORT_TICK_SECONDS: i64 = 300;

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

/// `cert.ssl_interval_seconds` — how often `cert_scheduler` re-checks a
/// monitor's TLS certificate. Default 43200 (12h) — deliberately decoupled
/// from the monitor's fast uptime `interval_seconds` (§4 of the spec).
pub async fn cert_ssl_interval_seconds(pool: &SqlitePool) -> i64 {
    get(pool, "cert.ssl_interval_seconds", &DEFAULT_CERT_SSL_INTERVAL_SECONDS.to_string())
        .await
        .parse()
        .unwrap_or(DEFAULT_CERT_SSL_INTERVAL_SECONDS)
}

/// `cert.domain_interval_seconds` — how often `cert_scheduler` re-checks a
/// monitor's domain registration expiry. Default 86400 (24h) — RDAP/WHOIS
/// registries dislike frequent queries (§6 of the spec).
pub async fn cert_domain_interval_seconds(pool: &SqlitePool) -> i64 {
    get(pool, "cert.domain_interval_seconds", &DEFAULT_CERT_DOMAIN_INTERVAL_SECONDS.to_string())
        .await
        .parse()
        .unwrap_or(DEFAULT_CERT_DOMAIN_INTERVAL_SECONDS)
}

/// `cert.tick_seconds` — how often `cert_scheduler::run`'s loop wakes to
/// re-select due monitors. Default 60.
pub async fn cert_tick_seconds(pool: &SqlitePool) -> i64 {
    get(pool, "cert.tick_seconds", &DEFAULT_CERT_TICK_SECONDS.to_string())
        .await
        .parse()
        .unwrap_or(DEFAULT_CERT_TICK_SECONDS)
}

/// `cert.concurrency` — the size of the semaphore bounding simultaneous
/// SSL/domain refreshes. Default 5.
pub async fn cert_concurrency(pool: &SqlitePool) -> i64 {
    get(pool, "cert.concurrency", &DEFAULT_CERT_CONCURRENCY.to_string())
        .await
        .parse()
        .unwrap_or(DEFAULT_CERT_CONCURRENCY)
}

/// `heartbeat.tick_seconds` — how often `heartbeat::run_reaper`'s loop
/// wakes to re-select and reap overdue heartbeat monitors. Default 20.
pub async fn heartbeat_tick_seconds(pool: &SqlitePool) -> i64 {
    get(pool, "heartbeat.tick_seconds", &DEFAULT_HEARTBEAT_TICK_SECONDS.to_string())
        .await
        .parse()
        .unwrap_or(DEFAULT_HEARTBEAT_TICK_SECONDS)
}

/// `maintenance.tick_seconds` — how often `maintenance_windows::run`'s loop
/// wakes to re-evaluate which monitors are currently under an active
/// maintenance window and publish `Event::MaintenanceChanged` for any that
/// entered/exited since the last pass. Default 30.
pub async fn maintenance_tick_seconds(pool: &SqlitePool) -> i64 {
    get(pool, "maintenance.tick_seconds", &DEFAULT_MAINTENANCE_TICK_SECONDS.to_string())
        .await
        .parse()
        .unwrap_or(DEFAULT_MAINTENANCE_TICK_SECONDS)
}

/// `notify.renotify_hours` — reminder cadence for an ongoing outage. 0 disables.
pub async fn renotify_hours(pool: &SqlitePool) -> i64 {
    get(pool, "notify.renotify_hours", &DEFAULT_RENOTIFY_HOURS.to_string())
        .await
        .parse()
        .unwrap_or(DEFAULT_RENOTIFY_HOURS)
}

/// `notify.renotify_tick_seconds` — how often the re-notify scan runs.
pub async fn renotify_tick_seconds(pool: &SqlitePool) -> i64 {
    get(pool, "notify.renotify_tick_seconds", &DEFAULT_RENOTIFY_TICK_SECONDS.to_string())
        .await
        .parse()
        .unwrap_or(DEFAULT_RENOTIFY_TICK_SECONDS)
}

/// `notify.digest_enabled` — daily digest master switch. Stored "1"/"0";
/// any value other than "1" is false. First boolean settings helper.
pub async fn digest_enabled(pool: &SqlitePool) -> bool {
    get(pool, "notify.digest_enabled", "0").await == "1"
}

/// `notify.digest_time` — "HH:MM" UTC offset into the day the digest fires.
pub async fn digest_time(pool: &SqlitePool) -> String {
    get(pool, "notify.digest_time", DEFAULT_DIGEST_TIME).await
}

/// `notify.digest_tick_seconds` — digest scheduler granularity.
pub async fn digest_tick_seconds(pool: &SqlitePool) -> i64 {
    get(pool, "notify.digest_tick_seconds", &DEFAULT_DIGEST_TICK_SECONDS.to_string())
        .await
        .parse()
        .unwrap_or(DEFAULT_DIGEST_TICK_SECONDS)
}

/// `notify.digest_recipients` — email channel ids, stored as a JSON array string.
pub async fn digest_recipients(pool: &SqlitePool) -> Vec<i64> {
    serde_json::from_str(&get(pool, "notify.digest_recipients", "[]").await).unwrap_or_default()
}

/// `report_auto_generate` — monthly report auto-generate master switch.
/// Stored "1"/"0"; any value other than "1" is false. Default on.
pub async fn report_auto_generate(pool: &SqlitePool) -> bool {
    get(pool, "report_auto_generate", "1").await == "1"
}

/// `report_day_of_month` — the day of the UTC month the report scheduler
/// fires on (clamped to the month's actual length by the scheduler).
pub async fn report_day_of_month(pool: &SqlitePool) -> i64 {
    get(pool, "report_day_of_month", &DEFAULT_REPORT_DAY_OF_MONTH.to_string())
        .await
        .parse()
        .unwrap_or(DEFAULT_REPORT_DAY_OF_MONTH)
}

/// `report_time` — "HH:MM" UTC offset into the day the report fires.
pub async fn report_time(pool: &SqlitePool) -> String {
    get(pool, "report_time", DEFAULT_REPORT_TIME).await
}

/// `report_tick_seconds` — report scheduler granularity.
pub async fn report_tick_seconds(pool: &SqlitePool) -> i64 {
    get(pool, "report_tick_seconds", &DEFAULT_REPORT_TICK_SECONDS.to_string())
        .await
        .parse()
        .unwrap_or(DEFAULT_REPORT_TICK_SECONDS)
}
