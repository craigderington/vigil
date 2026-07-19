//! One probe-and-reschedule cycle for a single monitor: probe → record the
//! raw `checks` row → run it through `engine::apply_result` → compute and
//! persist the next `next_run_at` → tell the scheduler the check completed.
//!
//! Every exit path — monitor missing, monitor paused, or the normal
//! probe/apply/reschedule flow (including an `apply_result` error) — sends
//! exactly one `SchedCmd::Complete(id)` before returning. The scheduler's
//! in-flight guard (`SchedState`) depends on that: a monitor that fires
//! without ever completing would stay marked in-flight forever and never
//! be scheduled again.

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

/// Signals the scheduler that this monitor's check finished, so it can
/// clear the in-flight guard (and, seeing the monitor paused/missing/its
/// freshly-persisted `next_run_at`, decide whether/when to re-heap it).
/// Send errors are ignored: no scheduler task is running in some tests, and
/// a closed channel just means "nothing to re-heap" — not a failure of the
/// check itself.
fn signal_complete(state: &AppState, monitor_id: i64) {
    let _ = state.sched_tx.send(SchedCmd::Complete(monitor_id));
}

/// Runs a single check for `monitor_id`. Returns early (after signaling
/// completion) if the monitor was deleted or paused between being
/// scheduled and firing — that's a normal race, not an error.
pub async fn run_check(state: &AppState, monitor_id: i64) {
    let m: Option<Monitor> = sqlx::query_as("SELECT * FROM monitors WHERE id = ?")
        .bind(monitor_id)
        .fetch_optional(&state.db)
        .await
        .ok()
        .flatten();

    let Some(m) = m else {
        signal_complete(state, monitor_id);
        return;
    };
    if m.is_paused {
        signal_complete(state, monitor_id);
        return;
    }
    // Belt-and-suspenders: the scheduler (catch-up, reschedule_from_db)
    // never enqueues a heartbeat monitor, but if one ever slips through,
    // never let it reach the prober — a heartbeat has no url/host, so
    // `probe::run`'s HTTP fallback would connection-error into a false
    // DOWN, fighting the ping-driven state.
    if m.r#type == "heartbeat" {
        signal_complete(state, monitor_id);
        return;
    }

    let out = if m.r#type == "ssl" {
        crate::certcheck::ssl::ssl_probe(state, &m).await
    } else {
        probe::run(&m).await
    };
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

    // On an apply_result error, don't drop the monitor from the schedule —
    // treat it as "retry soon" (use_retry_interval = true) rather than
    // silently returning without ever rescheduling it.
    let use_retry_interval = match engine::apply_result(state, &m, &out, state.anchor.current().await).await {
        Ok(a) => a.use_retry_interval,
        Err(e) => {
            tracing::error!(monitor_id, error = %e, "apply_result failed; retrying soon");
            true
        }
    };

    let base = if use_retry_interval { m.retry_interval_seconds } else { m.interval_seconds };
    let next = scheduler::next_run_with_jitter(now, base);

    if let Err(e) = sqlx::query("UPDATE monitors SET next_run_at = ? WHERE id = ?")
        .bind(next)
        .bind(monitor_id)
        .execute(&state.db)
        .await
    {
        tracing::error!(monitor_id, error = %e, "failed to persist next_run_at");
    }

    signal_complete(state, monitor_id);
}
