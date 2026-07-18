//! Nightly maintenance: daily rollup catch-up, raw `checks` retention
//! pruning, and a weekly `PRAGMA incremental_vacuum` pass. Replaces the
//! Task 14 stub.
//!
//! Retention is read from `settings_store::retention_days` on every pass
//! (so a Settings-screen edit takes effect on the next tick without a
//! restart) and pruning runs once per ~24h loop iteration. Rollup catch-up
//! runs first each pass, before pruning, so a day's `checks` are always
//! aggregated into `check_aggregates_daily` before they become eligible for
//! deletion. Every 7th pass also runs `incremental_vacuum` to reclaim space
//! freed by pruning and rollup deletes, since the database is opened with
//! `SqliteAutoVacuum::Incremental` (see `db::connect`) rather than eager
//! auto-vacuum.

use crate::models::Ts;

/// Deletes raw `checks` rows older than `retention_days` relative to `now`.
/// Returns the number of rows removed.
pub async fn prune_old_checks(
    pool: &sqlx::SqlitePool,
    retention_days: i64,
    now: Ts,
) -> anyhow::Result<u64> {
    let cutoff = now - retention_days * 86400;
    let r = sqlx::query("DELETE FROM checks WHERE checked_at < ?")
        .bind(cutoff)
        .execute(pool)
        .await?;
    Ok(r.rows_affected())
}

/// Background task: every ~24h, prune old `checks` per the configured
/// retention window, and every 7th pass also run `PRAGMA
/// incremental_vacuum`. Runs once immediately on startup (rather than
/// waiting a full day for the first prune), then sleeps between passes.
/// Errors are logged, never fatal — a bad pass shouldn't kill the loop.
pub async fn run(state: crate::app::AppState) {
    let mut day: u64 = 0;
    loop {
        let retention_days = crate::settings_store::retention_days(&state.db).await;
        let now: Ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);

        if let Err(error) = crate::rollup::rollup_catch_up(&state.db, retention_days).await {
            tracing::error!(%error, "nightly maintenance: rollup_catch_up failed");
        }

        match prune_old_checks(&state.db, retention_days, now).await {
            Ok(removed) => {
                tracing::info!(removed, retention_days, "nightly maintenance: pruned old checks");
            }
            Err(error) => {
                tracing::error!(%error, "nightly maintenance: prune_old_checks failed");
            }
        }

        if day.is_multiple_of(7) {
            if let Err(error) = sqlx::query("PRAGMA incremental_vacuum")
                .execute(&state.db)
                .await
            {
                tracing::warn!(%error, "nightly maintenance: incremental_vacuum failed");
            } else {
                tracing::info!("nightly maintenance: incremental_vacuum complete");
            }
        }

        day += 1;
        tokio::time::sleep(std::time::Duration::from_secs(24 * 3600)).await;
    }
}
