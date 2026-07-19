//! Heartbeat (push) monitor support (§3 "Heartbeat (push)", §9
//! `heartbeat_token`). Holds the capability-token generator used by
//! `api::monitors::create`, and the `/ping/:token` receiver: `record_ping`
//! (the atomic ping/conditional-recover transaction) + the axum handler.
//! The reaper (missed-ping -> down) lands in a later P4.1 task.

use std::time::{SystemTime, UNIX_EPOCH};

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use sqlx::Connection;

use crate::app::AppState;
use crate::engine;
use crate::events::Event;
use crate::models::{Monitor, Status};

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
