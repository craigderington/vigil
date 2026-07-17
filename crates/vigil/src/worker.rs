//! One probe-and-reschedule cycle for a single monitor: probe → record the
//! raw `checks` row → run it through `engine::apply_result` → compute and
//! persist the next `next_run_at` → tell the scheduler to re-heap.

use std::time::{SystemTime, UNIX_EPOCH};

use crate::app::{AppState, SchedCmd};
use crate::models::Monitor;
use crate::{engine, probe, scheduler};

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

/// Runs a single check for `monitor_id`. Silently returns (logging as
/// appropriate) if the monitor was deleted or paused between being
/// scheduled and firing — that's a normal race, not an error.
pub async fn run_check(state: &AppState, monitor_id: i64) {
    let m: Option<Monitor> = sqlx::query_as("SELECT * FROM monitors WHERE id = ?")
        .bind(monitor_id)
        .fetch_optional(&state.db)
        .await
        .ok()
        .flatten();

    let Some(m) = m else { return };
    if m.is_paused {
        return;
    }

    let out = probe::http::probe(&m).await;
    let now = now();

    let status = if out.ok { "up" } else { "down" };
    if let Err(e) = sqlx::query(
        "INSERT INTO checks (monitor_id, checked_at, status, response_time_ms, status_code, error_message, resolved_ip) \
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(monitor_id)
    .bind(now)
    .bind(status)
    .bind(out.response_time_ms)
    .bind(out.status_code)
    .bind(&out.error_message)
    .bind(&out.resolved_ip)
    .execute(&state.db)
    .await
    {
        tracing::error!(monitor_id, error = %e, "failed to insert check row");
    }

    let ao = match engine::apply_result(state, &m, &out).await {
        Ok(a) => a,
        Err(e) => {
            tracing::error!(monitor_id, error = %e, "apply_result failed");
            return;
        }
    };

    let base = if ao.use_retry_interval { m.retry_interval_seconds } else { m.interval_seconds };
    let next = scheduler::next_run_with_jitter(now, base);

    if let Err(e) = sqlx::query("UPDATE monitors SET next_run_at = ? WHERE id = ?")
        .bind(next)
        .bind(monitor_id)
        .execute(&state.db)
        .await
    {
        tracing::error!(monitor_id, error = %e, "failed to persist next_run_at");
    }

    // Ignored: no scheduler task is running in some tests, and a closed
    // channel just means "nothing to re-heap" — not a failure of the check
    // itself, which has already been recorded and applied above.
    let _ = state.sched_tx.send(SchedCmd::Upsert(monitor_id));
}
