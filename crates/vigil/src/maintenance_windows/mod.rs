//! Maintenance windows (P4.2): scheduled downtime that suppresses alerts
//! and/or checks for in-scope monitors, one-off or cron-recurring. See
//! [`resolve`] for the pure scope/time-window math (no I/O, heavily unit
//! tested); this file adds the one DB read the resolve fns' callers need,
//! plus (Task 5) the live MAINTENANCE display driver: maintenance is a
//! CLIENT-SIDE overlay, not a monitor `status` column, so the frontend
//! needs the backend to actively push which monitors are currently under a
//! window rather than inferring it from `status` alone. `run`/`eval_once`
//! below fill that role — see their docs for the diff-and-publish design.

pub mod resolve;

use std::collections::HashSet;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::app::AppState;
use crate::events::Event;
use crate::models::{MaintenanceWindow, Ts};
use crate::settings_store;
use resolve::{maintenance_for, parse_tags, Suppression};

fn now() -> Ts {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs() as i64
}

/// All `is_active` maintenance windows, unfiltered by scope or time — the
/// engine/API/report callers run each one through
/// `resolve::window_active_at` / `resolve::monitor_in_scope` /
/// `resolve::maintenance_for` per monitor. A query failure (shouldn't
/// happen against a live pool) degrades to "no active windows" rather than
/// propagating, matching the fail-open, `unwrap_or_default` posture of
/// `settings_store`'s read helpers — a DB hiccup here should never itself
/// block a probe or an alert.
pub async fn active_windows(pool: &sqlx::SqlitePool) -> Vec<MaintenanceWindow> {
    sqlx::query_as("SELECT * FROM maintenance_windows WHERE is_active = 1")
        .fetch_all(pool)
        .await
        .unwrap_or_default()
}

/// Every monitor id currently under SOME active maintenance window (either
/// `suppress` mode — this is the client-facing "show the maintenance pill"
/// question, not the alert/checks-suppression split those modes drive
/// elsewhere). Loads `active_windows` plus every monitor's `(id, tags)`,
/// then keeps ids where `resolve::maintenance_for` resolves to anything
/// other than `Suppression::None`. Both queries degrading to empty on a DB
/// hiccup (`unwrap_or_default`, matching `active_windows`'s own fail-open
/// posture) yields an empty result rather than propagating an error — a
/// transient DB blip should never crash the snapshot builder or the
/// evaluator loop, just momentarily under-report maintenance.
pub async fn monitors_in_maintenance(pool: &sqlx::SqlitePool) -> Vec<i64> {
    let windows = active_windows(pool).await;
    if windows.is_empty() {
        return Vec::new();
    }
    let rows: Vec<(i64, Option<String>)> = sqlx::query_as("SELECT id, tags FROM monitors")
        .fetch_all(pool)
        .await
        .unwrap_or_default();
    let now = now();
    rows.into_iter()
        .filter(|(id, tags)| {
            let tags = parse_tags(tags.as_deref().unwrap_or(""));
            maintenance_for(&windows, *id, &tags, now) != Suppression::None
        })
        .map(|(id, _)| id)
        .collect()
}

/// One evaluator pass: computes the current in-maintenance set (via
/// [`monitors_in_maintenance`]), diffs it against `prev`, and publishes
/// `Event::MaintenanceChanged{id, in_maintenance}` for every monitor that
/// entered (`in_maintenance: true`) or exited (`in_maintenance: false`)
/// since the last pass — then updates `prev` in place to the new set so the
/// next call diffs from here, not from the stale one. A monitor whose
/// membership is unchanged between passes publishes nothing, so a steady
/// state (the common case) is silent.
///
/// `pub` — besides `run`'s own loop, `tests/maintenance_evaluator.rs` calls
/// this directly, one pass at a time, rather than waiting on `run`'s sleep
/// interval.
///
/// The bus send is `let _ = state.bus.send(...)`, deliberately not
/// `?`/unwrapped: a tokio `broadcast::Sender::send` returns `Err` when zero
/// receivers are currently subscribed, which is the common case (no SSE
/// client connected) — that is not a failure this loop should log or abort
/// over, matching every other event-publish site in this codebase (see
/// `engine.rs`, `heartbeat.rs`, `cert_scheduler.rs`).
pub async fn eval_once(state: &AppState, prev: &mut HashSet<i64>) {
    let current: HashSet<i64> = monitors_in_maintenance(&state.db).await.into_iter().collect();

    for id in current.difference(prev) {
        let _ = state.bus.send(Event::MaintenanceChanged { id: *id, in_maintenance: true });
    }
    for id in prev.difference(&current) {
        let _ = state.bus.send(Event::MaintenanceChanged { id: *id, in_maintenance: false });
    }

    *prev = current;
}

/// The maintenance evaluator loop: wakes every `maintenance.tick_seconds`
/// (default 30) and runs [`eval_once`] against a task-local `HashSet` that
/// persists across iterations for the lifetime of the app — this is the
/// live driver behind the frontend's MAINTENANCE pill (Task 7), since
/// maintenance is a client-side overlay rather than a monitor `status`.
pub async fn run(state: AppState) {
    let mut prev: HashSet<i64> = HashSet::new();
    loop {
        let tick = settings_store::maintenance_tick_seconds(&state.db).await;
        tokio::time::sleep(Duration::from_secs(tick.max(1) as u64)).await;
        eval_once(&state, &mut prev).await;
    }
}
