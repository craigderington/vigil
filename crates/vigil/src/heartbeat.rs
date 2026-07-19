//! Heartbeat (push) monitor support (§3 "Heartbeat (push)", §9
//! `heartbeat_token`). Holds the capability-token generator used by
//! `api::monitors::create`, the `/ping/:token` receiver (`record_ping` — the
//! atomic ping/conditional-recover transaction — + the axum handler), and
//! the reaper (`reap_once`/`run_reaper`): a periodic task that drives a
//! pinged-then-silent heartbeat DOWN once it's overdue by `interval_seconds
//! + heartbeat_grace_seconds`.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use sqlx::Connection;

use crate::app::AppState;
use crate::engine;
use crate::events::Event;
use crate::models::{Monitor, Status, Trigger};
use crate::settings_store;

/// A 32-char alphanumeric capability token for a heartbeat monitor's
/// `/ping/:token` push-URL. Not a guessable sequence — this is the sole
/// secret gating who can post a ping for a given monitor, so it's drawn
/// from `rand`'s CSPRNG-backed thread-local generator, not `id`-derived.
pub fn generate_token() -> String {
    use rand::{distributions::Alphanumeric, Rng};
    rand::thread_rng().sample_iter(&Alphanumeric).take(32).map(char::from).collect()
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

/// Records an inbound ping for `m`: sets `last_ping_at`/`updated_at`, and —
/// if the monitor was `down` or `pending` — flips it to `up`. The whole
/// read-old-status / conditional-update / close-incident sequence runs as
/// ONE `BEGIN IMMEDIATE` transaction (acquired on a single connection, not
/// the deferred `pool.begin()`): reading `old_status` under the write lock
/// means a concurrent reaper (which would otherwise race to mark the same
/// monitor `down`) cannot interleave between the read and the write. SQLite
/// also forbids a subquery inside `UPDATE ... RETURNING`, which rules out
/// doing this as a single statement.
///
/// `pending -> up` is treated as *arming*, not recovery: a never-pinged
/// heartbeat has no open incident, so only a `MonitorTransition` +
/// `MonitorUpdated` are emitted — never a `Recovered` notification or a
/// phantom `IncidentResolved`. Only `old_status == "down"` closes an
/// incident and dispatches `Recovered` (via `engine::emit_resolved`).
/// Events are emitted only after `COMMIT`, never while the transaction is
/// open.
pub(crate) async fn record_ping(state: &AppState, m: &Monitor) -> anyhow::Result<()> {
    let n = now();

    // A real sqlx `Transaction` guard, not a raw `BEGIN IMMEDIATE` execute:
    // `Transaction::drop` rolls back automatically if `tx` is dropped
    // without `commit()` (e.g. an early `?`-return below), so the
    // IMMEDIATE write lock can never be leaked on an error path. A raw
    // `sqlx::query("BEGIN IMMEDIATE").execute(..)` is dispatched as a plain
    // `Command::Execute`, not `Command::Begin` — sqlx never learns a
    // transaction is open, so `PoolConnection::drop` would NOT roll it
    // back, leaking the lock on a pooled connection.
    let mut conn = state.db.acquire().await?;
    let mut tx = conn.begin_with("BEGIN IMMEDIATE").await?;

    // Read the pre-update status under the write lock just acquired, so no
    // concurrent writer (the reaper) can change it between this read and
    // the UPDATE below.
    let old: String = sqlx::query_scalar("SELECT status FROM monitors WHERE id = ?")
        .bind(m.id)
        .fetch_one(&mut *tx)
        .await?;

    sqlx::query(
        "UPDATE monitors SET last_ping_at = ?1, updated_at = ?1, \
         status = CASE WHEN status IN ('down','pending') THEN 'up' ELSE status END \
         WHERE id = ?2",
    )
    .bind(n)
    .bind(m.id)
    .execute(&mut *tx)
    .await?;

    // Only a `down` heartbeat can have an open incident — `pending` never
    // opened one (there's nothing to have missed yet), and `up`/`paused`
    // either never opened one or already closed it.
    let mut closed: Option<(i64, i64)> = None;
    if old == "down" {
        let open: Option<(i64, i64)> = sqlx::query_as(
            "SELECT id, started_at FROM incidents WHERE monitor_id = ? AND resolved_at IS NULL \
             ORDER BY started_at DESC LIMIT 1",
        )
        .bind(m.id)
        .fetch_optional(&mut *tx)
        .await?;

        if let Some((iid, started_at)) = open {
            let dur = n - started_at;
            sqlx::query("UPDATE incidents SET resolved_at = ?, duration_seconds = ? WHERE id = ?")
                .bind(n)
                .bind(dur)
                .bind(iid)
                .execute(&mut *tx)
                .await?;
            closed = Some((iid, dur));
        }
    }

    tx.commit().await?;
    drop(conn);

    // Events/notifications are side effects that happen only after a
    // successful commit — never while holding the write transaction open.
    match old.as_str() {
        "down" => {
            if let Some((iid, dur)) = closed {
                engine::emit_resolved(state, m, iid, Status::Down, Status::Up, dur).await;
            }
            let _ = state.bus.send(Event::MonitorUpdated {
                id: m.id,
                status: Status::Up,
                response_time_ms: None,
                checked_at: n,
            });
        }
        "pending" => {
            // ARMING — a first ping is not a recovery: no incident exists
            // to close, so no `Recovered` notification is dispatched.
            let _ = state.bus.send(Event::MonitorTransition {
                id: m.id,
                from: Status::Pending,
                to: Status::Up,
                incident_id: None,
            });
            let _ = state.bus.send(Event::MonitorUpdated {
                id: m.id,
                status: Status::Up,
                response_time_ms: None,
                checked_at: n,
            });
        }
        _ => {
            // up / paused / maintenance: status is unchanged by the ping.
            let _ = state.bus.send(Event::MonitorUpdated {
                id: m.id,
                status: Status::from_db(&old),
                response_time_ms: None,
                checked_at: n,
            });
        }
    }

    Ok(())
}

/// Selects every heartbeat monitor overdue by more than `interval_seconds +
/// heartbeat_grace_seconds` since its last ping and reaps it (drives it
/// DOWN, opens an incident, dispatches `heartbeat_missed`) — the other half
/// of heartbeat liveness (`record_ping` above arms/recovers on an inbound
/// ping; this reaps silence).
///
/// **Single-arm due-query**: `type='heartbeat' AND is_paused=0 AND
/// status='up' AND last_ping_at IS NOT NULL AND <overdue>`. A never-pinged
/// heartbeat (`last_ping_at IS NULL`) is deliberately excluded, not an
/// OR-arm of "overdue OR never-pinged" — the switch only arms on the first
/// ping (`record_ping`'s doc), so a monitor that has never been pinged has
/// nothing to have "missed" yet.
///
/// Heartbeats are **not anchor-gated**: unlike `apply_result`, this never
/// reads `state.anchor` — a missed ping is real signal regardless of the
/// local connection's state, so a `DOWN` transition + alert fire even while
/// the anchor reports offline.
///
/// Per-monitor errors are logged and skipped so one bad row can't stall the
/// pass.
pub async fn reap_once(state: &AppState) -> anyhow::Result<()> {
    let n = now();

    let due: Vec<Monitor> = sqlx::query_as(
        "SELECT * FROM monitors WHERE type = 'heartbeat' AND is_paused = 0 AND status = 'up' \
         AND last_ping_at IS NOT NULL \
         AND ? > last_ping_at + interval_seconds + heartbeat_grace_seconds",
    )
    .bind(n)
    .fetch_all(&state.db)
    .await?;

    for m in due {
        if let Err(e) = reap_one(state, &m, n).await {
            tracing::warn!(monitor_id = m.id, error = %e, "heartbeat reap_one failed");
        }
    }

    Ok(())
}

/// Reaps a single overdue heartbeat monitor `m` (called only for rows the
/// due-query in `reap_once` already selected as stale at time `n`). Runs
/// the status-flip + incident-insert as ONE `BEGIN IMMEDIATE` transaction —
/// a real sqlx `Transaction` guard (`conn.begin_with`), not a raw
/// `sqlx::query("BEGIN IMMEDIATE").execute(..)`: `Transaction::drop` rolls
/// back automatically on an early `?`-return, so the IMMEDIATE write lock
/// can never be leaked on an error path (see `record_ping`'s doc for the
/// full rationale — Task 5's review proved the raw form leaks the lock).
///
/// The DOWN `UPDATE` **re-asserts the staleness predicate** (`status='up'
/// AND now > last_ping_at + interval_seconds + heartbeat_grace_seconds`),
/// so a ping that refreshed `last_ping_at` between `reap_once`'s SELECT and
/// this UPDATE makes `rows_affected == 0` — no false incident. An incident
/// is opened only when `rows_affected == 1`. Events/notifications are
/// dispatched only after a successful `COMMIT`, and only if the monitor
/// actually transitioned.
///
/// `pub` for the reap-vs-ping race regression test: an integration test
/// (`tests/heartbeat_reaper.rs`) can only call `pub` items, and exercising
/// the race requires feeding a deliberately stale in-memory `Monitor`
/// snapshot directly to this function (bypassing `reap_once`'s SELECT,
/// which would otherwise filter a fresh row out before `reap_one` is ever
/// invoked).
pub async fn reap_one(state: &AppState, m: &Monitor, n: i64) -> anyhow::Result<()> {
    let mut conn = state.db.acquire().await?;
    let mut tx = conn.begin_with("BEGIN IMMEDIATE").await?;

    // Load-bearing: re-checks staleness against the FRESH db row (not the
    // possibly-stale in-memory `m` snapshot passed in) so a ping that
    // refreshed last_ping_at between the due-SELECT and here makes
    // rows_affected==0 (no false incident). Do NOT drop as "redundant with
    // the SELECT" — `reap_once`'s SELECT can only see the world as of the
    // moment it ran; a concurrent `record_ping` between that SELECT and
    // this UPDATE is exactly the race this predicate closes.
    let r = sqlx::query(
        "UPDATE monitors SET status='down', updated_at=?1 \
         WHERE id=?2 AND status='up' \
         AND ?1 > last_ping_at + interval_seconds + heartbeat_grace_seconds",
    )
    .bind(n)
    .bind(m.id)
    .execute(&mut *tx)
    .await?;

    let mut incident_id: Option<i64> = None;
    if r.rows_affected() == 1 {
        let iid: i64 = sqlx::query_scalar(
            "INSERT INTO incidents (monitor_id, started_at, cause, error_message) \
             VALUES (?, ?, 'heartbeat', 'no ping within interval + grace') RETURNING id",
        )
        .bind(m.id)
        .bind(n)
        .fetch_one(&mut *tx)
        .await?;
        incident_id = Some(iid);
    }

    tx.commit().await?;
    drop(conn);

    // Events/notifications are side effects that happen only after a
    // successful commit, and only if this call actually transitioned the
    // monitor (a concurrent ping could have made rows_affected 0 above).
    if let Some(iid) = incident_id {
        engine::emit_opened(state, m, iid, Status::Up, Status::Down, Trigger::HeartbeatMissed).await;
        let _ = state.bus.send(Event::MonitorUpdated {
            id: m.id,
            status: Status::Down,
            response_time_ms: None,
            checked_at: n,
        });
    }

    Ok(())
}

/// The heartbeat reaper loop: wakes every `heartbeat.tick_seconds` (default
/// 20) and reaps overdue heartbeats. Runs for the lifetime of the app.
/// Errors from a pass are logged inside `reap_once`/`reap_one`, never
/// propagated here, so the loop itself can never die from a bad row.
pub async fn run_reaper(state: AppState) {
    loop {
        let tick = settings_store::heartbeat_tick_seconds(&state.db).await;
        tokio::time::sleep(Duration::from_secs(tick.max(1) as u64)).await;
        let _ = reap_once(&state).await;
    }
}

/// `GET|POST /ping/:token` — the heartbeat receiver. 404 on an unknown
/// token; 500 on a genuine DB error during the token lookup (logged, so a
/// locked-database condition doesn't masquerade as "unknown token" with no
/// server-log trace); otherwise records the ping and always returns 200 (a
/// `record_ping` error is logged, not surfaced to the caller — the ping
/// itself arrived and the job shouldn't retry/fail over an internal
/// bookkeeping error). Never logs the token in any branch.
pub async fn ping(State(state): State<AppState>, Path(token): Path<String>) -> impl IntoResponse {
    let lookup: Result<Option<Monitor>, sqlx::Error> =
        sqlx::query_as("SELECT * FROM monitors WHERE heartbeat_token = ?")
            .bind(&token)
            .fetch_optional(&state.db)
            .await;

    let m = match lookup {
        Ok(Some(m)) => m,
        Ok(None) => return (StatusCode::NOT_FOUND, "not found"),
        Err(e) => {
            tracing::error!(error = %e, "heartbeat ping: monitor lookup failed");
            return (StatusCode::INTERNAL_SERVER_ERROR, "internal error");
        }
    };

    if let Err(e) = record_ping(&state, &m).await {
        tracing::warn!(monitor_id = m.id, error = %e, "record_ping failed");
    }

    (StatusCode::OK, "ok")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generates_32_char_alphanumeric_token() {
        let t = generate_token();
        assert_eq!(t.len(), 32);
        assert!(t.chars().all(|c| c.is_ascii_alphanumeric()));
    }

    #[test]
    fn two_calls_differ() {
        assert_ne!(generate_token(), generate_token());
    }
}
