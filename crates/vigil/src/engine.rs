//! Ties the pure state machine (`state::evaluate`) to persistence, events,
//! and notifications: `apply_result` runs one probe outcome through the
//! machine and updates the world accordingly; `bulk_set_unknown` +
//! `run_connectivity_reactor` implement the fleet-wide UNKNOWN reaction to
//! a lost anchor (the internet-sanity gate going offline).

use std::time::{SystemTime, UNIX_EPOCH};

use crate::app::AppState;
use crate::events::Event;
use crate::models::{Cause, Connectivity, Monitor, ProbeOutcome, Status, Trigger};
use crate::notify::dispatch;
use crate::state::{self, Transition};

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

pub struct ApplyOutcome {
    pub incident_id: Option<i64>,
    pub use_retry_interval: bool,
}

/// Runs one probe outcome through the state machine, persists the new
/// status/streaks, opens or closes an incident as the transition demands,
/// emits the corresponding events, and dispatches down/recovered
/// notifications. Does **not** touch `next_run_at` — the worker (Task 13)
/// owns scheduling. `anchor` is passed in rather than read internally so
/// heartbeat callers (which have no anchor-gated probe of their own) can
/// force `Connectivity::Online`.
pub async fn apply_result(
    state: &AppState,
    m: &Monitor,
    out: &ProbeOutcome,
    anchor: Connectivity,
) -> anyhow::Result<ApplyOutcome> {
    let now = now();

    let has_open: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM incidents WHERE monitor_id = ? AND resolved_at IS NULL",
    )
    .bind(m.id)
    .fetch_one(&state.db)
    .await?;
    let prev = if has_open > 0 { Status::Down } else { Status::Up };

    let inputs = state::Inputs {
        current: m.status,
        prev_confirmed: prev,
        consecutive_failures: m.consecutive_failures,
        consecutive_successes: m.consecutive_successes,
        outcome_ok: out.ok,
        anchor,
        th: state::Thresholds {
            confirmation: m.confirmation_threshold,
            recovery: m.recovery_threshold,
        },
    };
    let d = state::evaluate(&inputs);

    // All persistence for this transition (status/streaks + incident
    // open/close) happens in one transaction so a crash can never leave
    // the monitor's status inconsistent with its incident row. Events and
    // notifications are side effects that must happen only after a
    // successful commit, and never while holding a write transaction open
    // across a network call.
    let mut tx = state.db.begin().await?;

    sqlx::query(
        "UPDATE monitors SET status = ?, consecutive_failures = ?, consecutive_successes = ?, \
         last_checked_at = ?, updated_at = ? WHERE id = ?",
    )
    .bind(d.next_status.as_str())
    .bind(d.consecutive_failures)
    .bind(d.consecutive_successes)
    .bind(now)
    .bind(now)
    .bind(m.id)
    .execute(&mut *tx)
    .await?;

    let mut incident_id: Option<i64> = None;

    enum PostCommit {
        Opened { id: i64 },
        Resolved { id: i64, duration_seconds: i64 },
        NoIncident,
        NoOpenIncidentFound,
        None,
    }

    let post = match d.transition {
        Some(Transition::ToDownOpenIncident) => {
            let cause = match out.cause {
                Some(Cause::Timeout) => "timeout",
                Some(Cause::Status) => "status",
                Some(Cause::Connection) => "connection",
                Some(Cause::Dns) => "dns",
                Some(Cause::Keyword) => "keyword",
                Some(Cause::Ssl) => "ssl",
                Some(Cause::Heartbeat) => "heartbeat",
                None => "connection",
            };
            let id: i64 = sqlx::query_scalar(
                "INSERT INTO incidents (monitor_id, started_at, cause, status_code, error_message) \
                 VALUES (?, ?, ?, ?, ?) RETURNING id",
            )
            .bind(m.id)
            .bind(now)
            .bind(cause)
            .bind(out.status_code)
            .bind(&out.error_message)
            .fetch_one(&mut *tx)
            .await?;
            incident_id = Some(id);
            PostCommit::Opened { id }
        }
        Some(Transition::ToUpCloseIncident) => {
            let open: Option<(i64, i64)> = sqlx::query_as(
                "SELECT id, started_at FROM incidents WHERE monitor_id = ? AND resolved_at IS NULL \
                 ORDER BY started_at DESC LIMIT 1",
            )
            .bind(m.id)
            .fetch_optional(&mut *tx)
            .await?;

            if let Some((id, started_at)) = open {
                let dur = now - started_at;
                sqlx::query("UPDATE incidents SET resolved_at = ?, duration_seconds = ? WHERE id = ?")
                    .bind(now)
                    .bind(dur)
                    .bind(id)
                    .execute(&mut *tx)
                    .await?;
                incident_id = Some(id);
                PostCommit::Resolved { id, duration_seconds: dur }
            } else {
                PostCommit::NoOpenIncidentFound
            }
        }
        Some(Transition::ToUpNoIncident) | Some(Transition::ToUnknown) => PostCommit::NoIncident,
        None => PostCommit::None,
    };

    tx.commit().await?;

    // From here on, persistence has already succeeded: no `?` may skip the
    // unconditional MonitorUpdated emission or the Ok(ApplyOutcome) return
    // below. Notify-dispatch errors are logged, not propagated (Fix 1).
    match post {
        PostCommit::Opened { id } => {
            let down_trigger =
                if m.r#type == "heartbeat" { Trigger::HeartbeatMissed } else { Trigger::Down };
            emit_opened(state, m, id, m.status, d.next_status, down_trigger).await;
        }
        PostCommit::Resolved { id, duration_seconds } => {
            emit_resolved(state, m, id, m.status, d.next_status, duration_seconds).await;
        }
        PostCommit::NoIncident => {
            let _ = state.bus.send(Event::MonitorTransition {
                id: m.id,
                from: m.status,
                to: d.next_status,
                incident_id: None,
            });
        }
        PostCommit::NoOpenIncidentFound => {
            tracing::warn!(monitor_id = m.id, "ToUpCloseIncident but no open incident found");
        }
        PostCommit::None => {}
    }

    let _ = state.bus.send(Event::MonitorUpdated {
        id: m.id,
        status: d.next_status,
        response_time_ms: out.response_time_ms,
        checked_at: now,
    });

    Ok(ApplyOutcome { incident_id, use_retry_interval: d.use_retry_interval })
}

/// Post-commit side effects for a newly **opened** incident: emits
/// `IncidentOpened` + `MonitorTransition`, then dispatches `down_trigger`
/// (type-appropriate: `Down` for regular monitors, `HeartbeatMissed` for
/// heartbeats). Does **not** emit `MonitorUpdated` — callers own that emit
/// so it's sent exactly once per call site.
pub(crate) async fn emit_opened(
    state: &AppState,
    m: &Monitor,
    incident_id: i64,
    from: Status,
    to: Status,
    down_trigger: Trigger,
) {
    let _ = state.bus.send(Event::IncidentOpened { id: incident_id, monitor_id: m.id });
    let _ = state.bus.send(Event::MonitorTransition { id: m.id, from, to, incident_id: Some(incident_id) });
    if let Err(e) = dispatch::on_transition(state, m, down_trigger, Some(incident_id)).await {
        tracing::warn!(monitor_id = m.id, error = %e, "notify dispatch (down) failed");
    }
}

/// Post-commit side effects for a newly **resolved** incident: emits
/// `IncidentResolved` + `MonitorTransition`, then dispatches
/// `Trigger::Recovered`. Does **not** emit `MonitorUpdated` — callers own
/// that emit so it's sent exactly once per call site.
pub(crate) async fn emit_resolved(
    state: &AppState,
    m: &Monitor,
    incident_id: i64,
    from: Status,
    to: Status,
    duration_seconds: i64,
) {
    let _ = state.bus.send(Event::IncidentResolved { id: incident_id, monitor_id: m.id, duration_seconds });
    let _ = state.bus.send(Event::MonitorTransition { id: m.id, from, to, incident_id: Some(incident_id) });
    if let Err(e) = dispatch::on_transition(state, m, Trigger::Recovered, Some(incident_id)).await {
        tracing::warn!(monitor_id = m.id, error = %e, "notify dispatch (recovered) failed");
    }
}

/// Flips every non-paused monitor not already `unknown` into `UNKNOWN`
/// (the connectivity-lost state), emitting a transition + update event per
/// affected monitor. Used when the anchor gate reports the local
/// connection itself is down, so alerting is suppressed fleet-wide.
pub async fn bulk_set_unknown(state: &AppState) -> anyhow::Result<()> {
    let now = now();

    // The captured "old status" snapshot must be consistent with the rows
    // actually flipped to unknown, so both queries run in one transaction.
    // Events are emitted only after a successful commit.
    let mut tx = state.db.begin().await?;
    let rows: Vec<(i64, String)> = sqlx::query_as(
        "SELECT id, status FROM monitors WHERE is_paused = 0 AND status != 'unknown' AND type != 'heartbeat'",
    )
    .fetch_all(&mut *tx)
    .await?;

    sqlx::query(
        "UPDATE monitors SET status = 'unknown' WHERE is_paused = 0 AND status != 'unknown' AND type != 'heartbeat'",
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;

    for (id, old) in rows {
        let _ = state.bus.send(Event::MonitorTransition {
            id,
            from: Status::from_db(&old),
            to: Status::Unknown,
            incident_id: None,
        });
        let _ = state.bus.send(Event::MonitorUpdated {
            id,
            status: Status::Unknown,
            response_time_ms: None,
            checked_at: now,
        });
    }

    Ok(())
}

/// Background task: listens for `ConnectivityChanged { online: false }`
/// and reacts by marking the whole fleet `UNKNOWN`. Runs for the lifetime
/// of the app; exits only when the bus itself is closed.
pub async fn run_connectivity_reactor(state: AppState) {
    let mut rx = state.bus.subscribe();
    loop {
        match rx.recv().await {
            Ok(Event::ConnectivityChanged { online: false }) => {
                let _ = bulk_set_unknown(&state).await;
            }
            Ok(_) => {}
            Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
        }
    }
}
