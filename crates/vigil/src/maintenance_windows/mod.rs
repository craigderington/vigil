//! Maintenance windows (P4.2): scheduled downtime that suppresses alerts
//! and/or checks for in-scope monitors, one-off or cron-recurring. See
//! [`resolve`] for the pure scope/time-window math (no I/O, heavily unit
//! tested); this file adds the one DB read the resolve fns' callers need.

pub mod resolve;

use crate::models::MaintenanceWindow;

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
