//! Daily rollups: aggregates raw `checks` + `incidents` rows into
//! `check_aggregates_daily` (one row per monitor per completed UTC day).
//!
//! `rollup_day` computes/upserts a single day for every monitor that has at
//! least one check that day. `ensure_aggregates` catches up ONE monitor from
//! its last stored aggregate (or its oldest retained check) through
//! yesterday, bounded by retention so we never walk further back than raw
//! `checks` are guaranteed to exist. `rollup_catch_up` does this for every
//! monitor — called once at startup and nightly from `maintenance::run`
//! (before the prune step, so a day's checks are rolled up before they can
//! be pruned).

use crate::models::Ts;
use crate::uptime::{self, Span};
use sqlx::SqlitePool;

/// `(start, end)` UTC-epoch bounds of `day` ("YYYY-MM-DD"), `end` exclusive
/// (`start + 86400`). An unparsable `day` yields `(0, 86400)` — callers in
/// this module only ever pass days produced by `day_str`, so this is a
/// defensive fallback, not a validation path.
pub fn day_bounds(day: &str) -> (i64, i64) {
    let start = chrono::NaiveDate::parse_from_str(day, "%Y-%m-%d")
        .ok()
        .and_then(|d| d.and_hms_opt(0, 0, 0))
        .map(|dt| dt.and_utc().timestamp())
        .unwrap_or(0);
    (start, start + 86400)
}

/// Formats a UTC epoch second as its "YYYY-MM-DD" calendar day.
pub fn day_str(epoch: Ts) -> String {
    chrono::DateTime::<chrono::Utc>::from_timestamp(epoch, 0)
        .unwrap_or_default()
        .format("%Y-%m-%d")
        .to_string()
}

fn now_epoch() -> Ts {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Computes and upserts the `check_aggregates_daily` row for `day` for every
/// monitor with at least one `checks` row in `[ds, de)`. Monitors with no
/// checks that day are left untouched (no empty row is written) so the
/// absence of a row continues to mean "no data" downstream (§11.5).
pub async fn rollup_day(pool: &SqlitePool, day: &str) -> anyhow::Result<()> {
    let (ds, de) = day_bounds(day);
    let monitor_ids: Vec<i64> = sqlx::query_scalar(
        "SELECT DISTINCT monitor_id FROM checks WHERE checked_at >= ? AND checked_at < ?",
    )
    .bind(ds)
    .bind(de)
    .fetch_all(pool)
    .await?;

    for monitor_id in monitor_ids {
        rollup_monitor_day(pool, monitor_id, day, ds, de).await?;
    }
    Ok(())
}

async fn rollup_monitor_day(
    pool: &SqlitePool,
    monitor_id: i64,
    day: &str,
    ds: Ts,
    de: Ts,
) -> anyhow::Result<()> {
    let up_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM checks WHERE monitor_id = ? AND checked_at >= ? AND checked_at < ? AND status = 'up'",
    )
    .bind(monitor_id)
    .bind(ds)
    .bind(de)
    .fetch_one(pool)
    .await?;

    let down_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM checks WHERE monitor_id = ? AND checked_at >= ? AND checked_at < ? AND status = 'down'",
    )
    .bind(monitor_id)
    .bind(ds)
    .bind(de)
    .fetch_one(pool)
    .await?;

    let sample_count = up_count + down_count;

    let (avg_ms, min_ms, max_ms): (Option<f64>, Option<i64>, Option<i64>) = sqlx::query_as(
        "SELECT AVG(response_time_ms), MIN(response_time_ms), MAX(response_time_ms) \
         FROM checks WHERE monitor_id = ? AND checked_at >= ? AND checked_at < ? AND response_time_ms IS NOT NULL",
    )
    .bind(monitor_id)
    .bind(ds)
    .bind(de)
    .fetch_one(pool)
    .await?;

    let incident_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM incidents WHERE monitor_id = ? AND started_at >= ? AND started_at < ?",
    )
    .bind(monitor_id)
    .bind(ds)
    .bind(de)
    .fetch_one(pool)
    .await?;

    // Overlap spans: any incident touching [ds, de), including ones that
    // started earlier and/or are still open. `uptime::compute` clips each
    // span to the window itself.
    let overlap_spans: Vec<(Ts, Option<Ts>)> = sqlx::query_as(
        "SELECT started_at, resolved_at FROM incidents \
         WHERE monitor_id = ? AND started_at < ? AND (resolved_at IS NULL OR resolved_at > ?)",
    )
    .bind(monitor_id)
    .bind(de)
    .bind(ds)
    .fetch_all(pool)
    .await?;

    let spans: Vec<Span> = overlap_spans
        .into_iter()
        .map(|(start, end)| Span { start, end })
        .collect();

    // A completed day: evaluate uptime as of its end (`now = de`), always
    // `had_any_check = true` since we only get here for monitors that had
    // at least one check this day.
    // Rollups are computed once, at ingest time, and never retroactively
    // rewritten when a maintenance window is created/edited later — so daily
    // aggregates intentionally do not exclude maintenance (`&[]`). Only the
    // live `stats`/`bars` read paths (which recompute from `incidents` on
    // every request) apply the exclusion.
    let uptime = uptime::compute(&spans, ds, de, true, &[]);

    sqlx::query(
        "INSERT INTO check_aggregates_daily \
         (monitor_id, day, up_count, down_count, degraded_count, avg_response_ms, min_response_ms, max_response_ms, uptime_pct, incident_count, sample_count) \
         VALUES (?, ?, ?, ?, 0, ?, ?, ?, ?, ?, ?) \
         ON CONFLICT(monitor_id, day) DO UPDATE SET \
           up_count = excluded.up_count, \
           down_count = excluded.down_count, \
           degraded_count = excluded.degraded_count, \
           avg_response_ms = excluded.avg_response_ms, \
           min_response_ms = excluded.min_response_ms, \
           max_response_ms = excluded.max_response_ms, \
           uptime_pct = excluded.uptime_pct, \
           incident_count = excluded.incident_count, \
           sample_count = excluded.sample_count",
    )
    .bind(monitor_id)
    .bind(day)
    .bind(up_count)
    .bind(down_count)
    .bind(avg_ms)
    .bind(min_ms)
    .bind(max_ms)
    .bind(uptime.uptime_pct)
    .bind(incident_count)
    .bind(sample_count)
    .execute(pool)
    .await?;

    Ok(())
}

/// Bounded catch-up for a single monitor: rolls up every completed UTC day
/// from the day after its last stored aggregate (or, if it has none, the
/// UTC day of its oldest retained check) through yesterday. Never walks
/// further back than `now - retention_days*86400` (raw checks that old may
/// already be pruned), and never iterates more than `retention_days` days
/// regardless, so a long-neglected monitor can't spin the loop unbounded.
pub async fn ensure_aggregates(
    pool: &SqlitePool,
    monitor_id: i64,
    retention_days: i64,
) -> anyhow::Result<()> {
    let now = now_epoch();
    let retention_floor = now - retention_days * 86400;
    let (today_start, _) = day_bounds(&day_str(now));

    let last_agg_day: Option<String> = sqlx::query_scalar(
        "SELECT MAX(day) FROM check_aggregates_daily WHERE monitor_id = ?",
    )
    .bind(monitor_id)
    .fetch_one(pool)
    .await?;

    let start = match last_agg_day {
        Some(d) => day_bounds(&d).0 + 86400,
        None => {
            let oldest: Option<Ts> = sqlx::query_scalar(
                "SELECT MIN(checked_at) FROM checks WHERE monitor_id = ?",
            )
            .bind(monitor_id)
            .fetch_one(pool)
            .await?;
            match oldest {
                Some(t) => day_bounds(&day_str(t)).0,
                None => return Ok(()), // nothing recorded yet for this monitor
            }
        }
    };

    let mut cursor = start.max(retention_floor);
    let mut iterations = 0i64;
    while cursor < today_start && iterations < retention_days {
        rollup_day(pool, &day_str(cursor)).await?;
        cursor += 86400;
        iterations += 1;
    }
    Ok(())
}

/// Runs `ensure_aggregates` for every monitor. Intended for the nightly
/// maintenance loop (before pruning old `checks`) and a one-shot call at
/// startup so a period of downtime doesn't leave gaps in the 90-day bars.
pub async fn rollup_catch_up(pool: &SqlitePool, retention_days: i64) -> anyhow::Result<()> {
    let monitor_ids: Vec<i64> = sqlx::query_scalar("SELECT id FROM monitors")
        .fetch_all(pool)
        .await?;
    for monitor_id in monitor_ids {
        ensure_aggregates(pool, monitor_id, retention_days).await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn day_bounds_known_epoch() {
        let (start, end) = day_bounds("2000-01-01");
        assert_eq!(start, 946_684_800);
        assert_eq!(end, 946_684_800 + 86_400);
    }

    #[test]
    fn day_str_round_trips_day_bounds() {
        let (start, _) = day_bounds("2024-03-15");
        assert_eq!(day_str(start), "2024-03-15");
    }

    #[test]
    fn day_bounds_bad_input_falls_back_to_epoch_zero() {
        let (start, end) = day_bounds("not-a-day");
        assert_eq!(start, 0);
        assert_eq!(end, 86400);
    }
}
