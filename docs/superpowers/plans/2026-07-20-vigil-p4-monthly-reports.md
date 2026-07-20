# P4.4 Monthly Incident Reports — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Generate a monthly incident report (fleet + per-monitor uptime, incident log, cert/domain outlook) for the prior UTC month — auto-generated on a schedule, viewable in-app, exportable as self-contained HTML, and auto-emailed.

**Architecture:** A `report` module computes a `ReportSummary` from durable tables (`incidents`, `check_aggregates_daily`, `notification_log`, `ssl_certs`, `domain_info`) over a UTC month window, caches it as `summary_json` on a new `reports` row, renders it to self-contained HTML on demand, and emails it via the P4.3 `send_email_via_channel`. A monthly scheduler backfills any missing months. The frontend adds a Reports screen (month cards + iframe-embedded report).

**Tech Stack:** Rust (tokio, sqlx-sqlite, chrono, axum) + SolidJS/TS. No new crates.

## Global Constraints

- **rustls-only. NO new crates** — `chrono` is already in-tree (referenced fully-qualified; there are no `use chrono` lines — mirror that).
- **Exactly ONE migration: `0006_reports.sql`, and it MUST be registered in `db.rs`'s `MIGRATIONS` const** (this repo hardcodes migrations, not `sqlx::migrate!`; a bare `.sql` file is otherwise never applied).
- **UTC everywhere** — report months + `report_time` are UTC (`chrono::DateTime::<chrono::Utc>`, `NaiveDate`). No local-tz.
- **Durable compute:** `had_any` inclusion gates on `check_aggregates_daily` (+ incident overlap), **never raw `checks`** (pruned ~30d). `avg` weighted by `up_count`. `p95` only when the whole month is within retention. Alert counts = DISTINCT events.
- **Migration-free of secrets:** commit with `git commit -am` for tracked files; **new files staged by explicit `git add <path>`** (never `git add -A` — gitignored secret files must never be staged). SMTP password never touched.
- **Test isolation:** run the full Rust suite with `--test-threads=1`. Test output pristine (warnings are findings).
- **Branch:** `feat/p4-monthly-reports`; finish = local fast-forward merge to `master`, branch deleted, not pushed to origin.

---

## File Structure

| File | Change | Responsibility |
|---|---|---|
| `crates/vigil/migrations/0006_reports.sql` | create | `reports` table DDL. |
| `crates/vigil/src/db.rs` | modify | Append `(6, include_str!("../migrations/0006_reports.sql"))` to `MIGRATIONS`. |
| `crates/vigil/src/report/mod.rs` | create | `Report` model, `ReportSummary` + sub-structs, month helpers, `generate`, `send_report_email`. |
| `crates/vigil/src/report/compute.rs` | create | `compute` + `fleet_uptime_for`. |
| `crates/vigil/src/report/html.rs` | create | `render_html`. |
| `crates/vigil/src/report/scheduler.rs` | create | `should_run_today`, `tick_once` (backfill + retry), `run`, `seed_marker_if_absent`. |
| `crates/vigil/src/api/reports.rs` | create | list/get/generate/html/email/delete handlers. |
| `crates/vigil/src/lib.rs` | modify | `pub mod report;`. |
| `crates/vigil/src/main.rs` | modify | spawn `report::scheduler::run`. |
| `crates/vigil/src/api/mod.rs` | modify | `pub mod reports;` + routes. |
| `crates/vigil/src/digest.rs` | modify | make `round2`/`fmt_ts` `pub` (reused; O7). |
| `crates/vigil/src/settings_store.rs` | modify | `report_*` helpers + `DEFAULT_*` consts. |
| `crates/vigil/src/api/settings.rs` | modify | `report_*` on GET/PUT. |
| `web/src/api.ts` | modify | report fns + interfaces + `Settings`. |
| `web/src/components/Reports.tsx` | create | Reports screen. |
| `web/src/App.tsx` + `web/src/components/Rail.tsx` | modify | RailView `"reports"` + nav + Switch. |
| `web/src/components/Settings.tsx` | modify | Monthly-reports settings block. |
| `crates/vigil/tests/migrate6.rs`, `report_compute.rs`, `report_html.rs`, `report_generate.rs`, `report_scheduler.rs`, `report_api.rs`, `settings_p44.rs` | create | Rust tests. |
| `web/src/__tests__/reports.test.tsx` | create | Frontend test. |

**Verified interfaces (use EXACTLY):**
- `db.rs`: `const MIGRATIONS: &[(i64, &str)]`; `pub async fn connect(db_path: &str) -> anyhow::Result<SqlitePool>` runs migrations; `run_migrations` tracks `schema_migrations(version, applied_at)`, splits on `;` after stripping `--`.
- `check_aggregates_daily(monitor_id, day TEXT 'YYYY-MM-DD', up_count, down_count, degraded_count, avg_response_ms REAL, min/max_response_ms, uptime_pct REAL, incident_count, sample_count)` PK `(monitor_id, day)`.
- `uptime::compute(spans: &[Span], window_start: Ts, now: Ts, had_any_check: bool, maintenance: &[(Ts,Ts)]) -> Uptime{uptime_pct: Option<f64>, downtime_seconds: i64}`; `Span{start: Ts, end: Option<Ts>}`.
- `maintenance_windows::active_windows(pool) -> Vec<MaintenanceWindow>`; `resolve::maintenance_intervals(&windows, id: i64, tags: &[String], from: Ts, to: Ts) -> Vec<(Ts,Ts)>`; `resolve::subtract_intervals((Ts,Ts), &[(Ts,Ts)]) -> Vec<(Ts,Ts)>`; `resolve::parse_tags(&str) -> Vec<String>`.
- `dispatch::send_email_via_channel(transport: &dyn Transport, config_json: &str, subject: &str, body_text: &str, body_html: Option<String>) -> anyhow::Result<()>`; `digest::SendOutcome{Delivered, NothingToSend, AllFailed}` (pub, reuse).
- `Monitor{ id, name, r#type: String, tags: Option<String>, is_paused: bool, ssl_check_enabled: bool, ssl_alert_days: String, domain_check_enabled: bool, domain_alert_days: String }` (manual `FromRow`, `SELECT * FROM monitors`); `SslCert{ is_valid: Option<bool>, days_remaining: Option<i64>, .. }`; `DomainInfo{ queryable: Option<bool>, days_remaining: Option<i64>, .. }`.
- `settings_store::get(pool, key, default) -> String` / `set(pool, key, value) -> anyhow::Result<()>`.
- `AppState{ db, bus, transport: Arc<dyn Transport>, http_sender, sched_tx, anchor }`. Test harness `common::{test_state, fresh_pool, test_state_failing_transport}`, `env.sent` (`Arc<Mutex<Vec<(SmtpConfig, EmailMsg)>>>`).
- api handler modules each declare their own `type ApiResult<T> = Result<Json<T>, (StatusCode, String)>;`; helpers `super::{db_err, now}`. HTML handler uses `axum::response::Html<String>`.

---

### Task 1: Migration + `reports` table + `Report` model + month helpers

**Files:** Create `crates/vigil/migrations/0006_reports.sql`, `crates/vigil/src/report/mod.rs`; Modify `crates/vigil/src/db.rs`, `crates/vigil/src/lib.rs`; Test `crates/vigil/tests/migrate6.rs`, and `#[cfg(test)]` unit tests in `report/mod.rs`.

**Interfaces produced:** `report::{Report, month_of(i64)->String, month_bounds(&str)->(i64,i64), month_label(&str)->String, prior_month(&str)->String, next_month(&str)->String}`.

- [ ] **Step 1: Write the failing migration test** — `crates/vigil/tests/migrate6.rs`

```rust
#[tokio::test]
async fn migration_0006_creates_reports_table() {
    let d = tempfile::tempdir().unwrap();
    let pool = vigil::db::connect(d.path().join("f.db").to_str().unwrap()).await.unwrap();
    let v: i64 = sqlx::query_scalar("SELECT MAX(version) FROM schema_migrations").fetch_one(&pool).await.unwrap();
    assert_eq!(v, 6);
    // reports table is selectable
    sqlx::query("SELECT id, period_start, period_end, label, generated_at, summary_json, html_path, pdf_path, emailed_at FROM reports")
        .fetch_optional(&pool).await.unwrap();
}
```

- [ ] **Step 2: Write the failing month-helper unit tests** — append to `crates/vigil/src/report/mod.rs` a `#[cfg(test)] mod tests`

```rust
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn month_bounds_and_labels_utc() {
        // 2026-03: 2026-03-01T00:00Z .. 2026-04-01T00:00Z
        let (s, e) = month_bounds("2026-03");
        assert_eq!(s, 1_772_323_200); // 2026-03-01T00:00:00Z
        assert_eq!(e, 1_775_001_600); // 2026-04-01T00:00:00Z (March has 31 days)
        assert_eq!(month_label("2026-03"), "March 2026");
        assert_eq!(month_of(s), "2026-03");
        assert_eq!(month_of(e - 1), "2026-03");
    }
    #[test]
    fn prior_and_next_month_year_rollover() {
        assert_eq!(prior_month("2026-01"), "2025-12");
        assert_eq!(next_month("2025-12"), "2026-01");
        assert_eq!(prior_month("2026-03"), "2026-02");
        assert_eq!(next_month("2026-03"), "2026-04");
    }
    #[test]
    fn month_bounds_bad_input_is_safe() {
        let (s, e) = month_bounds("nonsense");
        assert!(e > s);
    }
}
```

- [ ] **Step 3: Run the tests to verify they fail**

Run: `cargo test --test migrate6 -- --test-threads=1` (FAIL: version 5 / no reports table) and `cargo test -p vigil --lib report:: -- --test-threads=1` (FAIL: module missing).

- [ ] **Step 4: Create the migration** — `crates/vigil/migrations/0006_reports.sql`

```sql
CREATE TABLE reports (
  id            INTEGER PRIMARY KEY,
  period_start  INTEGER NOT NULL,               -- first day of month, 00:00:00 UTC (epoch secs)
  period_end    INTEGER NOT NULL,               -- first day of next month, 00:00:00 UTC (exclusive)
  label         TEXT NOT NULL,                  -- "March 2026"
  generated_at  INTEGER NOT NULL,
  summary_json  TEXT NOT NULL,                  -- cached ReportSummary
  html_path     TEXT,
  pdf_path      TEXT,
  emailed_at    INTEGER,
  UNIQUE(period_start)
);
```

- [ ] **Step 5: Register the migration** — `crates/vigil/src/db.rs`, inside `MIGRATIONS` (after the `(5, ...)` line):

```rust
    (5, include_str!("../migrations/0005_maintenance_windows.sql")),
    (6, include_str!("../migrations/0006_reports.sql")),
];
```

- [ ] **Step 6: Create `report/mod.rs`** with the model + month helpers, and register the module

`crates/vigil/src/report/mod.rs`:
```rust
//! P4.4 monthly incident reports (CLAUDE.md §13). UTC calendar months,
//! computed from durable tables. See `compute` for the metrics, `html` for
//! rendering, `scheduler` for the monthly auto-generate loop.

pub mod compute;
pub mod html;
pub mod scheduler;

use serde::Serialize;

/// A stored report row (`reports` table).
#[derive(Clone, Debug, Serialize, sqlx::FromRow)]
pub struct Report {
    pub id: i64,
    pub period_start: i64,
    pub period_end: i64,
    pub label: String,
    pub generated_at: i64,
    pub summary_json: String,
    pub html_path: Option<String>,
    pub pdf_path: Option<String>,
    pub emailed_at: Option<i64>,
}

/// `"YYYY-MM"` → the 1st of that month as a `NaiveDate` (fallback 1970-01-01).
fn parse_month_first(period: &str) -> chrono::NaiveDate {
    let mut it = period.split('-');
    let y = it.next().and_then(|s| s.parse::<i32>().ok()).unwrap_or(1970);
    let m = it.next().and_then(|s| s.parse::<u32>().ok()).filter(|m| (1..=12).contains(m)).unwrap_or(1);
    chrono::NaiveDate::from_ymd_opt(y, m, 1).unwrap_or_else(|| chrono::NaiveDate::from_ymd_opt(1970, 1, 1).unwrap())
}

/// UTC `"YYYY-MM"` for the month containing `epoch`.
pub fn month_of(epoch: i64) -> String {
    chrono::DateTime::<chrono::Utc>::from_timestamp(epoch, 0).unwrap_or_default().format("%Y-%m").to_string()
}

/// `(start, end)` UTC epoch bounds for `"YYYY-MM"`: first-of-month 00:00 → first-of-next-month 00:00 (exclusive).
pub fn month_bounds(period: &str) -> (i64, i64) {
    let first = parse_month_first(period);
    let start = first.and_hms_opt(0, 0, 0).map(|dt| dt.and_utc().timestamp()).unwrap_or(0);
    let next = first.checked_add_months(chrono::Months::new(1)).unwrap_or(first);
    let end = next.and_hms_opt(0, 0, 0).map(|dt| dt.and_utc().timestamp()).unwrap_or(start + 2_678_400);
    (start, end)
}

/// `"March 2026"` for `"2026-03"`.
pub fn month_label(period: &str) -> String {
    parse_month_first(period).format("%B %Y").to_string()
}

/// The month before `period` (`"2026-01"` → `"2025-12"`).
pub fn prior_month(period: &str) -> String {
    let f = parse_month_first(period);
    f.checked_sub_months(chrono::Months::new(1)).unwrap_or(f).format("%Y-%m").to_string()
}

/// The month after `period`.
pub fn next_month(period: &str) -> String {
    let f = parse_month_first(period);
    f.checked_add_months(chrono::Months::new(1)).unwrap_or(f).format("%Y-%m").to_string()
}

// (`generate` + `send_report_email` are added in Task 4; `ReportSummary` + sub-structs
//  in Task 2 live in `compute.rs` and are re-exported below once they exist.)
```

**MANDATORY in this same step (else the `pub mod compute/html/scheduler` lines in `mod.rs` fail with `E0583: file not found`):** create the three stub files so `mod.rs` compiles now — Tasks 2/3 fill `compute.rs`/`html.rs`, Task 6 fills `scheduler.rs`:
```bash
printf '// filled in Task 2\n' > crates/vigil/src/report/compute.rs
printf '// filled in Task 3\n' > crates/vigil/src/report/html.rs
printf '// filled in Task 6\n' > crates/vigil/src/report/scheduler.rs
```
Then add to `crates/vigil/src/lib.rs` alongside the other `pub mod`s:
```rust
pub mod report;
```

- [ ] **Step 7: Run tests to verify they pass**

Run: `cargo test --test migrate6 -- --test-threads=1` (PASS, version 6) and `cargo test -p vigil --lib report:: -- --test-threads=1` (PASS, 3 tests). Confirm `cargo build -p vigil --tests` warning-clean.

- [ ] **Step 8: Commit**

```bash
git add crates/vigil/migrations/0006_reports.sql crates/vigil/src/report crates/vigil/tests/migrate6.rs
git commit -am "feat(p4.4): reports migration 0006 + Report model + UTC month helpers"
```
(`git add` the new files by name; `-am` picks up `db.rs`/`lib.rs`.)

---

### Task 2: `report::compute` + `fleet_uptime_for`

**Files:** Create/fill `crates/vigil/src/report/compute.rs`; Modify `crates/vigil/src/digest.rs` (make `round2`/`fmt_ts` `pub`); Test `crates/vigil/tests/report_compute.rs`.

**Interfaces produced:** `report::compute::{ReportSummary, FleetReport, MonitorReport, ReportIncident, ExpiryItem, compute(&AppState, &str) -> anyhow::Result<ReportSummary>, fleet_uptime_for(&AppState, &str) -> anyhow::Result<Option<f64>>}`. Re-export the structs from `report/mod.rs` (`pub use compute::{ReportSummary, ...}`).

**Interfaces consumed:** Task 1 month helpers; `uptime::compute`; `resolve::{maintenance_intervals, subtract_intervals, parse_tags}`; `maintenance_windows::active_windows`; `digest::{round2, fmt_ts}` (made pub here); `models::{Monitor, SslCert, DomainInfo}`.

- [ ] **Step 1: Make `round2`/`fmt_ts` public** — `crates/vigil/src/digest.rs`

```rust
pub fn round2(x: f64) -> f64 {
    (x * 100.0).round() / 100.0
}
```
```rust
pub fn fmt_ts(epoch: i64) -> String {
    chrono::DateTime::<chrono::Utc>::from_timestamp(epoch, 0)
        .map(|dt| dt.format("%Y-%m-%d %H:%M UTC").to_string())
        .unwrap_or_else(|| epoch.to_string())
}
```

- [ ] **Step 2: Write the failing tests** — `crates/vigil/tests/report_compute.rs`

```rust
mod common;
use common::test_state;
use vigil::report::compute::{compute, fleet_uptime_for};
use vigil::report::month_bounds;

async fn seed_http_monitor(db: &sqlx::SqlitePool, name: &str) -> i64 {
    sqlx::query_scalar("INSERT INTO monitors (name, type, url, created_at, updated_at) VALUES (?, 'http', 'https://x', 0, 0) RETURNING id")
        .bind(name).fetch_one(db).await.unwrap()
}
// Durable data for a month with NO raw checks (M1): an aggregate row + an incident.
async fn seed_month(db: &sqlx::SqlitePool, mid: i64, period: &str, day: &str, uptime: f64, avg_ms: f64, up: i64) {
    sqlx::query("INSERT INTO check_aggregates_daily (monitor_id, day, up_count, down_count, avg_response_ms, uptime_pct, incident_count, sample_count) VALUES (?, ?, ?, 0, ?, ?, 0, ?)")
        .bind(mid).bind(day).bind(up).bind(avg_ms).bind(uptime).bind(up).execute(db).await.unwrap();
    let _ = period;
}

#[tokio::test]
async fn compute_uses_durable_aggregates_not_raw_checks() {
    // The M1 must-fix: an old month with aggregates + an incident but ZERO raw checks
    // must still appear with correct uptime (raw-checks gating would blank it).
    let env = test_state().await;
    let (ds, de) = month_bounds("2026-03");
    let mid = seed_http_monitor(&env.state.db, "api").await;
    seed_month(&env.state.db, mid, "2026-03", "2026-03-05", 99.5, 140.0, 200).await;
    // one 1-hour incident inside March, resolved
    sqlx::query("INSERT INTO incidents (monitor_id, started_at, resolved_at, cause) VALUES (?, ?, ?, 'timeout')")
        .bind(mid).bind(ds + 3600).bind(ds + 7200).execute(&env.state.db).await.unwrap();

    let s = compute(&env.state, "2026-03").await.unwrap();
    assert_eq!(s.period, "2026-03");
    assert_eq!(s.label, "March 2026");
    assert_eq!(s.fleet.monitors_total, 1, "durable-gated monitor is included");
    assert_eq!(s.monitors.len(), 1);
    assert!(s.fleet.uptime_pct.unwrap() < 100.0 && s.fleet.uptime_pct.unwrap() > 99.0);
    assert_eq!(s.fleet.incidents, 1);
    assert_eq!(s.fleet.downtime_seconds, 3600);
    assert_eq!(s.monitors[0].avg_ms, Some(140));
    assert_eq!(s.monitors[0].end_status, "up");
    let _ = de;
}

#[tokio::test]
async fn incident_clipped_to_month_both_ends() {
    // Incident started 10 days BEFORE March, resolved on March 2 → only in-month part counts.
    let env = test_state().await;
    let (ds, _de) = month_bounds("2026-03");
    let mid = seed_http_monitor(&env.state.db, "api").await;
    seed_month(&env.state.db, mid, "2026-03", "2026-03-02", 90.0, 100.0, 100).await;
    sqlx::query("INSERT INTO incidents (monitor_id, started_at, resolved_at, cause) VALUES (?, ?, ?, 'timeout')")
        .bind(mid).bind(ds - 10 * 86400).bind(ds + 3600).execute(&env.state.db).await.unwrap();
    let s = compute(&env.state, "2026-03").await.unwrap();
    assert_eq!(s.incidents[0].duration_seconds, Some(3600), "duration clipped to in-month portion");
    assert_eq!(s.fleet.longest_outage.as_ref().unwrap().seconds, 3600);
}

#[tokio::test]
async fn maintenance_covered_outage_excluded_and_delta_none_without_prior() {
    let env = test_state().await;
    let (ds, _de) = month_bounds("2026-03");
    let mid = seed_http_monitor(&env.state.db, "api").await;
    seed_month(&env.state.db, mid, "2026-03", "2026-03-01", 100.0, 120.0, 100).await;
    sqlx::query("INSERT INTO incidents (monitor_id, started_at, resolved_at, cause) VALUES (?, ?, ?, 'timeout')")
        .bind(mid).bind(ds + 3600).bind(ds + 7200).execute(&env.state.db).await.unwrap();
    let target = format!("[{mid}]");
    sqlx::query("INSERT INTO maintenance_windows (name, scope, target_ref, starts_at, ends_at, recurrence, suppress, is_active, created_at) VALUES ('w','monitors',?,?,?,NULL,'alerts',1,0)")
        .bind(target).bind(ds + 3000).bind(ds + 7800).execute(&env.state.db).await.unwrap();
    let s = compute(&env.state, "2026-03").await.unwrap();
    assert_eq!(s.fleet.uptime_pct, Some(100.0));
    assert_eq!(s.fleet.clean_monitors, 1);
    assert_eq!(s.fleet.uptime_delta, None, "no prior month → delta None");
}

#[tokio::test]
async fn paused_and_nodata_monitors_get_rows_but_not_fleet_weight() {
    let env = test_state().await;
    let mid_ok = seed_http_monitor(&env.state.db, "ok").await;
    seed_month(&env.state.db, mid_ok, "2026-03", "2026-03-01", 100.0, 100.0, 50).await;
    let mid_paused = seed_http_monitor(&env.state.db, "paused").await;
    sqlx::query("UPDATE monitors SET is_paused = 1 WHERE id = ?").bind(mid_paused).execute(&env.state.db).await.unwrap();
    let _mid_nodata = seed_http_monitor(&env.state.db, "nodata").await;
    let s = compute(&env.state, "2026-03").await.unwrap();
    assert_eq!(s.monitors.len(), 3, "one row per monitor");
    assert_eq!(s.fleet.monitors_total, 1, "only the had-data non-paused monitor is weighted");
    assert!(s.monitors.iter().any(|m| m.end_status == "paused"));
    assert!(s.monitors.iter().any(|m| m.end_status == "no data"));
}

#[tokio::test]
async fn distinct_alert_counts_and_cert_outlook() {
    let env = test_state().await;
    let (ds, _de) = month_bounds("2026-03");
    let mid = seed_http_monitor(&env.state.db, "site").await;
    sqlx::query("UPDATE monitors SET ssl_check_enabled = 1 WHERE id = ?").bind(mid).execute(&env.state.db).await.unwrap();
    seed_month(&env.state.db, mid, "2026-03", "2026-03-01", 100.0, 100.0, 10).await;
    sqlx::query("INSERT INTO ssl_certs (monitor_id, is_valid, days_remaining, invalid_alerted) VALUES (?, 1, 12, 0)").bind(mid).execute(&env.state.db).await.unwrap();
    // one ssl_expiring alert fanned to TWO channels (same sent_at) → must count ONCE
    for ch in [1, 2] {
        sqlx::query("INSERT INTO notification_log (monitor_id, channel_id, incident_id, trigger, sent_at, success) VALUES (?, ?, NULL, 'ssl_expiring', ?, 1)")
            .bind(mid).bind(ch).bind(ds + 100).execute(&env.state.db).await.unwrap();
    }
    let s = compute(&env.state, "2026-03").await.unwrap();
    assert_eq!(s.fleet.ssl_alerts, 1, "channel fan-out counts as one alert event");
    let ssl = s.cert_outlook.iter().find(|e| e.kind == "ssl").unwrap();
    assert_eq!(ssl.flag, "expiring"); // 12 <= max(ssl_alert_days default 30)
    assert!(s.fleet.expiring_30d >= 1);
}

#[tokio::test]
async fn delta_uses_prior_month_live() {
    let env = test_state().await;
    let mid = seed_http_monitor(&env.state.db, "api").await;
    // Feb: 100% ; Mar: 100% ; delta 0.0
    seed_month(&env.state.db, mid, "2026-02", "2026-02-10", 100.0, 100.0, 100).await;
    seed_month(&env.state.db, mid, "2026-03", "2026-03-10", 100.0, 100.0, 100).await;
    assert_eq!(fleet_uptime_for(&env.state, "2026-02").await.unwrap(), Some(100.0));
    let s = compute(&env.state, "2026-03").await.unwrap();
    assert_eq!(s.fleet.uptime_delta, Some(0.0));
}

#[tokio::test]
async fn open_incident_at_period_end_clips_and_marks_down() {
    // Incident starts inside March and is STILL OPEN (resolved_at NULL) → clips to de,
    // end_status "down". Exercises clip()'s end branch + end_status_at (finding F).
    let env = test_state().await;
    let (ds, de) = month_bounds("2026-03");
    let mid = seed_http_monitor(&env.state.db, "api").await;
    seed_month(&env.state.db, mid, "2026-03", "2026-03-31", 50.0, 100.0, 100).await;
    // opens 1 hour before month-end, never resolves
    sqlx::query("INSERT INTO incidents (monitor_id, started_at, resolved_at, cause) VALUES (?, ?, NULL, 'timeout')")
        .bind(mid).bind(de - 3600).execute(&env.state.db).await.unwrap();
    let s = compute(&env.state, "2026-03").await.unwrap();
    assert_eq!(s.incidents[0].duration_seconds, Some(3600), "open incident clips to period_end");
    assert_eq!(s.incidents[0].resolved_at, None);
    assert_eq!(s.monitors[0].end_status, "down");
    assert_eq!(s.fleet.longest_outage.as_ref().unwrap().seconds, 3600);
    let _ = ds;
}

#[tokio::test]
async fn p95_computed_when_month_within_retention() {
    // Raise retention so a RECENT month's raw checks survive → p95 value branch runs
    // (finding E). Use a month computed from now() to stay within retention + not future.
    let env = test_state().await;
    vigil::settings_store::set(&env.state.db, "retention.raw_days", "3650").await.unwrap();
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64;
    // use the PRIOR month so it is fully in the past (never future)
    let period = vigil::report::prior_month(&vigil::report::month_of(now));
    let (ds, _de) = month_bounds(&period);
    let mid = seed_http_monitor(&env.state.db, "api").await;
    let day = vigil::rollup::day_str(ds);
    seed_month(&env.state.db, mid, &period, &day, 100.0, 100.0, 5).await;
    // 5 checks: 100,110,120,130,500 → p95 index ceil(5*0.95)-1 = 4 → 500
    for (i, v) in [100, 110, 120, 130, 500].iter().enumerate() {
        sqlx::query("INSERT INTO checks (monitor_id, checked_at, status, response_time_ms) VALUES (?, ?, 'up', ?)")
            .bind(mid).bind(ds + 100 + i as i64).bind(v).execute(&env.state.db).await.unwrap();
    }
    let s = compute(&env.state, &period).await.unwrap();
    assert_eq!(s.monitors[0].p95_ms, Some(500));
}

#[tokio::test]
async fn monitor_rows_sorted_worst_uptime_first() {
    let env = test_state().await;
    let (ds, _de) = month_bounds("2026-03");
    let good = seed_http_monitor(&env.state.db, "good").await;
    seed_month(&env.state.db, good, "2026-03", "2026-03-01", 100.0, 100.0, 100).await;
    let bad = seed_http_monitor(&env.state.db, "bad").await;
    seed_month(&env.state.db, bad, "2026-03", "2026-03-01", 50.0, 100.0, 100).await;
    sqlx::query("INSERT INTO incidents (monitor_id, started_at, resolved_at, cause) VALUES (?, ?, ?, 'timeout')")
        .bind(bad).bind(ds + 3600).bind(ds + 3600 + 10 * 86400).execute(&env.state.db).await.unwrap();
    let s = compute(&env.state, "2026-03").await.unwrap();
    assert_eq!(s.monitors[0].name, "bad", "worst uptime first");
    assert_eq!(s.monitors[1].name, "good");
}
```

- [ ] **Step 3: Run to verify they fail**

Run: `cargo test --test report_compute -- --test-threads=1` (FAIL: `compute` missing).

- [ ] **Step 4: Implement `report/compute.rs`**

```rust
//! Computes the cached `ReportSummary` for a UTC month, entirely from durable
//! tables (incidents + check_aggregates_daily + notification_log + ssl_certs +
//! domain_info) so any past month is reproducible. Mirrors digest.rs's
//! single-pass fleet approach, extended to per-monitor rows + a month window.

use serde::{Deserialize, Serialize};

use crate::app::AppState;
use crate::digest::round2;
use crate::maintenance_windows::{self, resolve};
use crate::models::{DomainInfo, Monitor, SslCert};
use crate::report::{month_bounds, month_label, prior_month};
use crate::uptime::{self, Span};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FleetReport {
    pub uptime_pct: Option<f64>,
    pub uptime_delta: Option<f64>,
    pub incidents: i64,
    pub downtime_seconds: i64,
    pub mttr_seconds: Option<i64>,
    pub longest_outage: Option<LongestOutage>,
    pub monitors_total: i64,
    pub clean_monitors: i64,
    pub ssl_alerts: i64,
    pub domain_alerts: i64,
    pub expiring_30d: i64,
    pub expiring_60d: i64,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LongestOutage { pub monitor: String, pub seconds: i64 }

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MonitorReport {
    pub id: i64, pub name: String, pub r#type: String,
    pub uptime_pct: Option<f64>, pub incidents: i64, pub downtime_seconds: i64,
    pub mttr_seconds: Option<i64>, pub avg_ms: Option<i64>, pub p95_ms: Option<i64>,
    pub end_status: String,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ReportIncident {
    pub monitor_name: String, pub started_at: i64, pub resolved_at: Option<i64>,
    pub duration_seconds: Option<i64>, pub cause: Option<String>,
    pub status_code: Option<i64>, pub error_message: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ExpiryItem {
    pub monitor: String, pub kind: String, pub days_remaining: Option<i64>, pub flag: String,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ReportSummary {
    pub period: String, pub label: String, pub generated_at: i64,
    pub fleet: FleetReport,
    pub cert_outlook: Vec<ExpiryItem>,
    pub monitors: Vec<MonitorReport>,
    pub incidents: Vec<ReportIncident>,
}

fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64
}

/// Fleet uptime % for a month only (no delta, no per-monitor) — used for the
/// prior-month delta. Calls neither `compute` nor itself (no recursion).
pub async fn fleet_uptime_for(state: &AppState, period: &str) -> anyhow::Result<Option<f64>> {
    let (ds, de) = month_bounds(period);
    let windows = maintenance_windows::active_windows(&state.db).await;
    let monitors: Vec<Monitor> = sqlx::query_as("SELECT * FROM monitors").fetch_all(&state.db).await?;
    let (mut total_down, mut total_denom) = (0i64, 0i64);
    for m in &monitors {
        if m.is_paused { continue; }
        if !had_any(state, m.id, period, ds, de).await? { continue; }
        let (down, denom) = monitor_uptime(state, m, ds, de, &windows).await?;
        total_down += down; total_denom += denom;
    }
    Ok(if total_denom > 0 { Some(round2((1.0 - total_down as f64 / total_denom as f64) * 100.0)) } else { None })
}

/// Durable presence test for a month (M1): an aggregate row OR an incident
/// overlapping the month — NEVER raw `checks` (pruned ~30d).
async fn had_any(state: &AppState, id: i64, period: &str, ds: i64, de: i64) -> anyhow::Result<bool> {
    let (m1, m2) = (format!("{period}-01"), format!("{}-01", crate::report::next_month(period)));
    let agg: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM check_aggregates_daily WHERE monitor_id = ? AND day >= ? AND day < ?)",
    ).bind(id).bind(&m1).bind(&m2).fetch_one(&state.db).await?;
    if agg { return Ok(true); }
    let inc: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM incidents WHERE monitor_id = ? AND started_at < ? AND (resolved_at IS NULL OR resolved_at > ?))",
    ).bind(id).bind(de).bind(ds).fetch_one(&state.db).await?;
    Ok(inc)
}

/// Returns `(downtime_seconds, eff_denom)` for one had-data monitor.
async fn monitor_uptime(state: &AppState, m: &Monitor, ds: i64, de: i64, windows: &[crate::models::MaintenanceWindow]) -> anyhow::Result<(i64, i64)> {
    let raw: Vec<(i64, Option<i64>)> = sqlx::query_as(
        "SELECT started_at, resolved_at FROM incidents WHERE monitor_id = ? AND started_at < ? AND (resolved_at IS NULL OR resolved_at > ?)",
    ).bind(m.id).bind(de).bind(ds).fetch_all(&state.db).await?;
    let spans: Vec<Span> = raw.into_iter().map(|(start, end)| Span { start, end }).collect();
    let tags = resolve::parse_tags(m.tags.as_deref().unwrap_or(""));
    let maint = resolve::maintenance_intervals(windows, m.id, &tags, ds, de);
    let u = uptime::compute(&spans, ds, de, true, &maint);
    let eff_denom: i64 = resolve::subtract_intervals((ds, de), &maint).iter().map(|(s, e)| e - s).sum();
    Ok((u.downtime_seconds, eff_denom))
}

pub async fn compute(state: &AppState, period: &str) -> anyhow::Result<ReportSummary> {
    let (ds, de) = month_bounds(period);
    let windows = maintenance_windows::active_windows(&state.db).await;
    let monitors: Vec<Monitor> = sqlx::query_as("SELECT * FROM monitors").fetch_all(&state.db).await?;

    let (mut total_down, mut total_denom, mut reporting, mut clean) = (0i64, 0i64, 0i64, 0i64);
    let mut monitor_rows: Vec<MonitorReport> = Vec::new();

    for m in &monitors {
        let has_data = !m.is_paused && had_any(state, m.id, period, ds, de).await?;
        // incident overlap spans, fetched ONCE (clipped both ends where used).
        let raw: Vec<(i64, Option<i64>)> = sqlx::query_as(
            "SELECT started_at, resolved_at FROM incidents WHERE monitor_id = ? AND started_at < ? AND (resolved_at IS NULL OR resolved_at > ?)",
        ).bind(m.id).bind(de).bind(ds).fetch_all(&state.db).await?;
        let inc_count = raw.len() as i64;
        let mttr = mean_resolved_in_window(&raw, ds, de);
        // Compute uptime ONCE per monitor (single source; no double compute).
        let (uptime_pct, downtime, end_status) = if has_data {
            let tags = resolve::parse_tags(m.tags.as_deref().unwrap_or(""));
            let maint = resolve::maintenance_intervals(&windows, m.id, &tags, ds, de);
            let u = uptime::compute(&to_spans(&raw), ds, de, true, &maint);
            let eff_denom: i64 = resolve::subtract_intervals((ds, de), &maint).iter().map(|(s, e)| e - s).sum();
            total_down += u.downtime_seconds;
            total_denom += eff_denom;
            reporting += 1;
            if u.uptime_pct.is_some() && u.downtime_seconds == 0 { clean += 1; }
            (u.uptime_pct, u.downtime_seconds, end_status_at(&raw, de))
        } else if m.is_paused {
            (None, 0, "paused".to_string())
        } else {
            (None, 0, "no data".to_string())
        };
        monitor_rows.push(MonitorReport {
            id: m.id, name: m.name.clone(), r#type: m.r#type.clone(),
            uptime_pct, incidents: inc_count, downtime_seconds: downtime, mttr_seconds: mttr,
            avg_ms: monthly_avg_ms(state, m.id, period).await?,
            p95_ms: monthly_p95_ms(state, m.id, ds, de).await?,
            end_status,
        });
    }
    // Worst-uptime-first (CLAUDE.md §13.1); None/paused/no-data sort last.
    monitor_rows.sort_by(|a, b| {
        a.uptime_pct.unwrap_or(f64::INFINITY)
            .partial_cmp(&b.uptime_pct.unwrap_or(f64::INFINITY))
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let fleet_uptime = if total_denom > 0 { Some(round2((1.0 - total_down as f64 / total_denom as f64) * 100.0)) } else { None };
    let prior = fleet_uptime_for(state, &prior_month(period)).await?;
    let uptime_delta = match (fleet_uptime, prior) { (Some(c), Some(p)) => Some(round2(c - p)), _ => None };

    // Fleet-wide incident log + longest outage (both-ends clipped)
    let inc_rows: Vec<(String, i64, Option<i64>, Option<String>, Option<i64>, Option<String>)> = sqlx::query_as(
        "SELECT m.name, i.started_at, i.resolved_at, i.cause, i.status_code, i.error_message \
         FROM incidents i JOIN monitors m ON m.id = i.monitor_id \
         WHERE i.started_at < ? AND (i.resolved_at IS NULL OR i.resolved_at > ?) ORDER BY m.name, i.started_at",
    ).bind(de).bind(ds).fetch_all(&state.db).await?;
    let mut incidents = Vec::new();
    let mut longest: Option<LongestOutage> = None;
    let mut all_resolved_durs: Vec<i64> = Vec::new();
    for (name, started_at, resolved_at, cause, status_code, error_message) in inc_rows {
        let dur = clip(started_at, resolved_at, ds, de);
        if longest.as_ref().map(|l| dur > l.seconds).unwrap_or(true) {
            longest = Some(LongestOutage { monitor: name.clone(), seconds: dur });
        }
        if let Some(r) = resolved_at { if r >= ds && r < de { all_resolved_durs.push(r - started_at); } }
        incidents.push(ReportIncident { monitor_name: name, started_at, resolved_at, duration_seconds: Some(dur), cause, status_code, error_message });
    }
    let fleet_mttr = if all_resolved_durs.is_empty() { None } else { Some(all_resolved_durs.iter().sum::<i64>() / all_resolved_durs.len() as i64) };

    // Alert counts (DISTINCT events) + cert outlook
    let ssl_alerts = distinct_alert_count(state, ds, de, "trigger IN ('ssl_expiring','ssl_invalid')").await?;
    let domain_alerts = distinct_alert_count(state, ds, de, "trigger = 'domain_expiring'").await?;
    let (cert_outlook, expiring_30d, expiring_60d) = cert_outlook(state, &monitors).await?;

    Ok(ReportSummary {
        period: period.to_string(), label: month_label(period), generated_at: now(),
        fleet: FleetReport {
            uptime_pct: fleet_uptime, uptime_delta, incidents: incidents.len() as i64,
            downtime_seconds: total_down, mttr_seconds: fleet_mttr, longest_outage: longest,
            monitors_total: reporting, clean_monitors: clean, ssl_alerts, domain_alerts,
            expiring_30d, expiring_60d,
        },
        cert_outlook, monitors: monitor_rows, incidents,
    })
}

fn to_spans(raw: &[(i64, Option<i64>)]) -> Vec<Span> {
    raw.iter().map(|&(start, end)| Span { start, end }).collect()
}
fn clip(started_at: i64, resolved_at: Option<i64>, ds: i64, de: i64) -> i64 {
    (resolved_at.unwrap_or(de).min(de)) - started_at.max(ds)
}
fn mean_resolved_in_window(raw: &[(i64, Option<i64>)], ds: i64, de: i64) -> Option<i64> {
    let durs: Vec<i64> = raw.iter().filter_map(|&(s, r)| r.filter(|&r| r >= ds && r < de).map(|r| r - s)).collect();
    if durs.is_empty() { None } else { Some(durs.iter().sum::<i64>() / durs.len() as i64) }
}
fn end_status_at(raw: &[(i64, Option<i64>)], de: i64) -> String {
    let open_at_end = raw.iter().any(|&(s, r)| s < de && r.map(|r| r >= de).unwrap_or(true));
    if open_at_end { "down".to_string() } else { "up".to_string() }
}

async fn monthly_avg_ms(state: &AppState, id: i64, period: &str) -> anyhow::Result<Option<i64>> {
    let (m1, m2) = (format!("{period}-01"), format!("{}-01", crate::report::next_month(period)));
    let row: Option<(Option<f64>, Option<i64>)> = sqlx::query_as(
        "SELECT SUM(avg_response_ms * up_count), SUM(up_count) FROM check_aggregates_daily \
         WHERE monitor_id = ? AND day >= ? AND day < ? AND avg_response_ms IS NOT NULL",
    ).bind(id).bind(&m1).bind(&m2).fetch_optional(&state.db).await?;
    Ok(match row { Some((Some(w), Some(n))) if n > 0 => Some((w / n as f64).round() as i64), _ => None })
}

async fn monthly_p95_ms(state: &AppState, id: i64, ds: i64, de: i64) -> anyhow::Result<Option<i64>> {
    // Only when the WHOLE month is within retention (else pruned → biased → None).
    let retention_days = crate::settings_store::get(&state.db, "retention.raw_days", "30").await.parse::<i64>().unwrap_or(30);
    if ds < now() - retention_days * 86400 { return Ok(None); }
    let mut v: Vec<i64> = sqlx::query_scalar(
        "SELECT response_time_ms FROM checks WHERE monitor_id = ? AND checked_at >= ? AND checked_at < ? AND response_time_ms IS NOT NULL",
    ).bind(id).bind(ds).bind(de).fetch_all(&state.db).await?;
    if v.is_empty() { return Ok(None); }
    v.sort_unstable();
    let idx = (((v.len() as f64) * 0.95).ceil() as usize).saturating_sub(1).min(v.len() - 1);
    Ok(Some(v[idx]))
}

async fn distinct_alert_count(state: &AppState, ds: i64, de: i64, pred: &str) -> anyhow::Result<i64> {
    let sql = format!(
        "SELECT COUNT(*) FROM (SELECT DISTINCT monitor_id, trigger, sent_at FROM notification_log WHERE sent_at >= ? AND sent_at < ? AND {pred})",
    );
    Ok(sqlx::query_scalar(&sql).bind(ds).bind(de).fetch_one(&state.db).await?)
}

async fn cert_outlook(state: &AppState, monitors: &[Monitor]) -> anyhow::Result<(Vec<ExpiryItem>, i64, i64)> {
    let (mut out, mut e30, mut e60) = (Vec::new(), 0i64, 0i64);
    for m in monitors {
        if m.ssl_check_enabled {
            if let Some(c) = sqlx::query_as::<_, SslCert>("SELECT * FROM ssl_certs WHERE monitor_id = ?").bind(m.id).fetch_optional(&state.db).await? {
                let max_t = serde_json::from_str::<Vec<i64>>(&m.ssl_alert_days).unwrap_or_default().into_iter().max().unwrap_or(0);
                let flag = if c.is_valid == Some(false) { "invalid" } else if c.days_remaining.map(|d| d <= max_t).unwrap_or(false) { "expiring" } else { "ok" };
                tally(&mut e30, &mut e60, c.days_remaining);
                out.push(ExpiryItem { monitor: m.name.clone(), kind: "ssl".into(), days_remaining: c.days_remaining, flag: flag.into() });
            }
        }
        if m.domain_check_enabled {
            if let Some(d) = sqlx::query_as::<_, DomainInfo>("SELECT * FROM domain_info WHERE monitor_id = ?").bind(m.id).fetch_optional(&state.db).await? {
                let max_t = serde_json::from_str::<Vec<i64>>(&m.domain_alert_days).unwrap_or_default().into_iter().max().unwrap_or(0);
                let flag = if d.queryable == Some(false) { "unknown" } else if d.days_remaining.map(|dd| dd <= max_t).unwrap_or(false) { "expiring" } else { "ok" };
                tally(&mut e30, &mut e60, d.days_remaining);
                out.push(ExpiryItem { monitor: m.name.clone(), kind: "domain".into(), days_remaining: d.days_remaining, flag: flag.into() });
            }
        }
    }
    Ok((out, e30, e60))
}
fn tally(e30: &mut i64, e60: &mut i64, days: Option<i64>) {
    if let Some(d) = days { if d <= 30 { *e30 += 1; } if d <= 60 { *e60 += 1; } }
}
```

> **Implementer note:** `monitor_uptime` (used only by `fleet_uptime_for`) takes `windows: &[crate::models::MaintenanceWindow]` fully-qualified — do NOT add a `use crate::models::MaintenanceWindow;` (it would be unused → warning). `to_spans` is shared by both `compute` and `monitor_uptime`.

Add to `report/mod.rs`:
```rust
pub use compute::{compute, fleet_uptime_for, ExpiryItem, FleetReport, LongestOutage, MonitorReport, ReportIncident, ReportSummary};
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test --test report_compute -- --test-threads=1` (all pass). Then `cargo test -p vigil -- --test-threads=1` to confirm no regression from the `digest.rs` `pub` change; `cargo build -p vigil --tests` warning-clean.

- [ ] **Step 6: Commit**
```bash
git add crates/vigil/src/report/compute.rs crates/vigil/tests/report_compute.rs
git commit -am "feat(p4.4): report compute (durable, per-monitor, both-ends clip, distinct alerts)"
```

---

### Task 3: `report::html::render_html`

**Files:** Create/fill `crates/vigil/src/report/html.rs`; Test `crates/vigil/tests/report_html.rs`.
**Interfaces produced:** `report::html::render_html(&ReportSummary) -> String`.

- [ ] **Step 1: Write the failing test** — `crates/vigil/tests/report_html.rs`

```rust
use vigil::report::compute::*;
use vigil::report::html::render_html;

fn sample() -> ReportSummary {
    ReportSummary {
        period: "2026-03".into(), label: "March 2026".into(), generated_at: 1_772_323_200,
        fleet: FleetReport {
            uptime_pct: Some(99.94), uptime_delta: Some(0.07), incidents: 2, downtime_seconds: 5220,
            mttr_seconds: Some(474), longest_outage: Some(LongestOutage { monitor: "api<x>".into(), seconds: 1980 }),
            monitors_total: 3, clean_monitors: 2, ssl_alerts: 1, domain_alerts: 0, expiring_30d: 1, expiring_60d: 1,
        },
        cert_outlook: vec![ExpiryItem { monitor: "api<x>".into(), kind: "ssl".into(), days_remaining: Some(12), flag: "expiring".into() }],
        monitors: vec![MonitorReport { id: 1, name: "api<x>".into(), r#type: "http".into(), uptime_pct: Some(99.7), incidents: 2, downtime_seconds: 5220, mttr_seconds: Some(474), avg_ms: Some(142), p95_ms: None, end_status: "up".into() }],
        incidents: vec![ReportIncident { monitor_name: "api<x>".into(), started_at: 1_772_323_200, resolved_at: Some(1_772_325_180), duration_seconds: Some(1980), cause: Some("timeout".into()), status_code: None, error_message: None }],
    }
}

#[test]
fn render_html_is_self_contained_and_escaped() {
    let h = render_html(&sample());
    assert!(h.contains("March 2026"));
    assert!(h.contains("99.94"));
    assert!(h.contains("@media print"), "carries a print stylesheet");
    assert!(h.contains("<style"), "inline CSS, self-contained");
    assert!(!h.contains("http://") && !h.contains("https://fonts"), "no external assets");
    // HTML-escaping: the monitor name 'api<x>' must not appear as a raw tag
    assert!(h.contains("api&lt;x&gt;"));
    assert!(!h.contains("api<x>"));
    // p95 None renders as a dash
    assert!(h.contains("—"));
}
```

- [ ] **Step 2: Run to verify it fails** — `cargo test --test report_html -- --test-threads=1` (FAIL: `render_html` missing).

- [ ] **Step 3: Implement `report/html.rs`** (hand-rolled string builder; escape every dynamic value)

```rust
//! Self-contained HTML rendering of a ReportSummary: inline navy-theme CSS +
//! a print stylesheet (Ctrl-P → PDF). One renderer for both export and the
//! in-app iframe view. No templating crate.

use crate::digest::fmt_ts;
use crate::report::compute::{ExpiryItem, MonitorReport, ReportIncident, ReportSummary};

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}
fn pct(v: Option<f64>) -> String { v.map(|p| format!("{p:.2}%")).unwrap_or_else(|| "—".into()) }
fn ms(v: Option<i64>) -> String { v.map(|n| format!("{n}ms")).unwrap_or_else(|| "—".into()) }
fn secs(v: i64) -> String { format!("{}m {}s", v / 60, v % 60) }
fn delta(v: Option<f64>) -> String {
    match v { Some(d) if d >= 0.0 => format!("▲ {d:.2}"), Some(d) => format!("▼ {:.2}", d.abs()), None => "—".into() }
}

const STYLE: &str = "\
body{background:#0A1220;color:#EAEDF3;font-family:Inter,system-ui,sans-serif;margin:0;padding:24px}\
h1,h2{color:#EAEDF3} .band{display:flex;flex-wrap:wrap;gap:24px;margin:16px 0}\
.tile{background:#16233A;border:1px solid #2A3A56;border-radius:10px;padding:12px 16px}\
.tile .n{font-family:'JetBrains Mono',monospace;font-size:24px;font-weight:600}\
table{width:100%;border-collapse:collapse;margin:12px 0}th,td{text-align:left;padding:6px 10px;border-bottom:1px solid #1E2C44;font-size:14px}\
.mono{font-family:'JetBrains Mono',monospace}.flag-expiring{color:#F5A623}.flag-invalid,.flag-unknown{color:#F26D6D}\
@media print{body{background:#fff;color:#111}.tile{background:#f4f6fa;border-color:#ccc}th,td{border-color:#ddd}section{break-inside:avoid}}\
";

pub fn render_html(s: &ReportSummary) -> String {
    let f = &s.fleet;
    let mut h = String::new();
    h.push_str(&format!("<!doctype html><html><head><meta charset=\"utf-8\"><title>Vigil report — {}</title><style>{STYLE}</style></head><body>", esc(&s.label)));
    h.push_str(&format!("<h1>Vigil monthly report — {}</h1>", esc(&s.label)));
    h.push_str(&format!("<p class=\"mono\">{} · generated {} · Vigil {}</p>", esc(&s.period), fmt_ts(s.generated_at), env!("CARGO_PKG_VERSION")));
    // hero band
    h.push_str("<div class=\"band\">");
    for (label, val) in [
        ("Uptime", format!("{} <small>({})</small>", pct(f.uptime_pct), delta(f.uptime_delta))),
        ("Incidents", f.incidents.to_string()),
        ("Downtime", secs(f.downtime_seconds)),
        ("MTTR", f.mttr_seconds.map(secs).unwrap_or_else(|| "—".into())),
        ("Longest outage", f.longest_outage.as_ref().map(|l| format!("{} ({})", esc(&l.monitor), secs(l.seconds))).unwrap_or_else(|| "—".into())),
        ("Clean", format!("{} / {}", f.clean_monitors, f.monitors_total)),
        ("SSL/domain alerts", format!("{} / {}", f.ssl_alerts, f.domain_alerts)),
        ("Expiring ≤30d / ≤60d", format!("{} / {}", f.expiring_30d, f.expiring_60d)),
    ] {
        h.push_str(&format!("<div class=\"tile\"><div>{label}</div><div class=\"n\">{val}</div></div>"));
    }
    h.push_str("</div>");
    // per-monitor table
    h.push_str("<section><h2>Per-monitor</h2><table><tr><th>Monitor</th><th>Type</th><th>Uptime</th><th>Incidents</th><th>Downtime</th><th>MTTR</th><th>Avg</th><th>p95</th><th>End</th></tr>");
    for m in &s.monitors { h.push_str(&monitor_row(m)); }
    h.push_str("</table></section>");
    // incident log
    h.push_str("<section><h2>Incidents</h2><table><tr><th>Monitor</th><th>Started</th><th>Duration</th><th>Cause</th><th>Resolved</th></tr>");
    if s.incidents.is_empty() { h.push_str("<tr><td colspan=5>No incidents.</td></tr>"); }
    for i in &s.incidents { h.push_str(&incident_row(i)); }
    h.push_str("</table></section>");
    // cert outlook
    h.push_str("<section><h2>Certificate &amp; domain outlook</h2><table><tr><th>Monitor</th><th>Kind</th><th>Days remaining</th><th>Status</th></tr>");
    if s.cert_outlook.is_empty() { h.push_str("<tr><td colspan=4>Nothing tracked.</td></tr>"); }
    for e in &s.cert_outlook { h.push_str(&outlook_row(e)); }
    h.push_str("</table></section></body></html>");
    h
}

fn monitor_row(m: &MonitorReport) -> String {
    format!("<tr><td>{}</td><td>{}</td><td class=\"mono\">{}</td><td class=\"mono\">{}</td><td class=\"mono\">{}</td><td class=\"mono\">{}</td><td class=\"mono\">{}</td><td class=\"mono\">{}</td><td>{}</td></tr>",
        esc(&m.name), esc(&m.r#type), pct(m.uptime_pct), m.incidents, secs(m.downtime_seconds),
        m.mttr_seconds.map(secs).unwrap_or_else(|| "—".into()), ms(m.avg_ms), ms(m.p95_ms), esc(&m.end_status))
}
fn incident_row(i: &ReportIncident) -> String {
    format!("<tr><td>{}</td><td class=\"mono\">{}</td><td class=\"mono\">{}</td><td>{}</td><td class=\"mono\">{}</td></tr>",
        esc(&i.monitor_name), fmt_ts(i.started_at), i.duration_seconds.map(secs).unwrap_or_else(|| "—".into()),
        esc(i.cause.as_deref().unwrap_or("-")), i.resolved_at.map(fmt_ts).unwrap_or_else(|| "ongoing".into()))
}
fn outlook_row(e: &ExpiryItem) -> String {
    format!("<tr><td>{}</td><td>{}</td><td class=\"mono\">{}</td><td class=\"flag-{}\">{}</td></tr>",
        esc(&e.monitor), esc(&e.kind), e.days_remaining.map(|d| d.to_string()).unwrap_or_else(|| "—".into()), esc(&e.flag), esc(&e.flag))
}
```
Add to `report/mod.rs`: `pub use html::render_html;` (or reference as `report::html::render_html`).

- [ ] **Step 4: Run to verify pass** — `cargo test --test report_html -- --test-threads=1` (PASS); build warning-clean.

- [ ] **Step 5: Commit**
```bash
git add crates/vigil/src/report/html.rs crates/vigil/tests/report_html.rs
git commit -am "feat(p4.4): self-contained HTML report render (navy + print stylesheet, escaped)"
```

---

### Task 4: `report::generate` + `send_report_email`

**Files:** Modify `crates/vigil/src/report/mod.rs`; Test `crates/vigil/tests/report_generate.rs`.
**Interfaces produced:** `report::generate(&AppState, &str) -> anyhow::Result<Report>`; `report::send_report_email(&AppState, &Report) -> digest::SendOutcome`.

- [ ] **Step 1: Write failing tests** — `crates/vigil/tests/report_generate.rs`

```rust
mod common;
use common::{test_state, test_state_failing_transport};
use vigil::digest::SendOutcome;
use vigil::report::{generate, send_report_email};

async fn seed_email_channel(db: &sqlx::SqlitePool) -> i64 {
    sqlx::query_scalar("INSERT INTO notification_channels (name, type, config, is_active, created_at) VALUES ('e','email','{\"host\":\"h\",\"port\":25,\"security\":\"none\",\"from\":\"f@b\",\"to\":[\"a@b\"]}',1,0) RETURNING id")
        .fetch_one(db).await.unwrap()
}

#[tokio::test]
async fn generate_upserts_idempotently() {
    let env = test_state().await;
    let r1 = generate(&env.state, "2026-03").await.unwrap();
    let r2 = generate(&env.state, "2026-03").await.unwrap();
    assert_eq!(r1.period_start, r2.period_start);
    let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM reports").fetch_one(&env.state.db).await.unwrap();
    assert_eq!(n, 1, "regenerate overwrites, one row per month");
    assert!(r2.summary_json.contains("\"period\":\"2026-03\""));
}

#[tokio::test]
async fn email_fans_out_and_sets_emailed_at() {
    let env = test_state().await;
    let cid = seed_email_channel(&env.state.db).await;
    vigil::settings_store::set(&env.state.db, "report_recipients", &format!("[{cid}]")).await.unwrap();
    let r = generate(&env.state, "2026-03").await.unwrap();
    let outcome = send_report_email(&env.state, &r).await;
    assert!(matches!(outcome, SendOutcome::Delivered));
    assert_eq!(env.sent.lock().unwrap().len(), 1);
    let logged: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM notification_log WHERE trigger='report' AND success=1").fetch_one(&env.state.db).await.unwrap();
    assert_eq!(logged, 1);
    let emailed: Option<i64> = sqlx::query_scalar("SELECT emailed_at FROM reports WHERE id=?").bind(r.id).fetch_one(&env.state.db).await.unwrap();
    assert!(emailed.is_some());
}

#[tokio::test]
async fn email_no_recipients_is_nothing_to_send_with_audit() {
    let env = test_state().await;
    let r = generate(&env.state, "2026-03").await.unwrap();
    let outcome = send_report_email(&env.state, &r).await;
    assert!(matches!(outcome, SendOutcome::NothingToSend));
    let logged: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM notification_log WHERE trigger='report' AND success=0").fetch_one(&env.state.db).await.unwrap();
    assert_eq!(logged, 1);
}

#[tokio::test]
async fn email_all_failed() {
    let env = test_state_failing_transport().await;
    let cid = seed_email_channel(&env.state.db).await;
    vigil::settings_store::set(&env.state.db, "report_recipients", &format!("[{cid}]")).await.unwrap();
    let r = generate(&env.state, "2026-03").await.unwrap();
    assert!(matches!(send_report_email(&env.state, &r).await, SendOutcome::AllFailed));
}
```

- [ ] **Step 2: Run to verify fail** — `cargo test --test report_generate -- --test-threads=1`.

- [ ] **Step 3: Implement in `report/mod.rs`** (append):

```rust
use crate::app::AppState;
use crate::digest::SendOutcome;
use crate::notify::dispatch;

fn now_ts() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64
}

/// Compute + UPSERT the report row for `period` (idempotent per month). No SSE event.
pub async fn generate(state: &AppState, period: &str) -> anyhow::Result<Report> {
    let summary = compute::compute(state, period).await?;
    let (ps, pe) = month_bounds(period);
    let json = serde_json::to_string(&summary)?;
    sqlx::query(
        "INSERT INTO reports (period_start, period_end, label, generated_at, summary_json, emailed_at) \
         VALUES (?, ?, ?, ?, ?, NULL) \
         ON CONFLICT(period_start) DO UPDATE SET label=excluded.label, generated_at=excluded.generated_at, summary_json=excluded.summary_json, emailed_at=NULL",
    ).bind(ps).bind(pe).bind(month_label(period)).bind(summary.generated_at).bind(&json).execute(&state.db).await?;
    let row: Report = sqlx::query_as("SELECT * FROM reports WHERE period_start = ?").bind(ps).fetch_one(&state.db).await?;
    Ok(row)
}

/// Email the rendered HTML report to `report_recipients` (mirrors digest::send).
pub async fn send_report_email(state: &AppState, report: &Report) -> SendOutcome {
    let ids: Vec<i64> = serde_json::from_str(&crate::settings_store::get(&state.db, "report_recipients", "[]").await).unwrap_or_default();
    let mut channels: Vec<(i64, String)> = Vec::new();
    for id in &ids {
        let cfg: Option<String> = sqlx::query_scalar("SELECT config FROM notification_channels WHERE id = ? AND type = 'email' AND is_active = 1")
            .bind(id).fetch_optional(&state.db).await.ok().flatten();
        if let Some(cfg) = cfg { channels.push((*id, cfg)); }
    }
    if channels.is_empty() {
        let _ = log_report(state, None, false, Some("no deliverable email recipients")).await;
        return SendOutcome::NothingToSend;
    }
    let summary: compute::ReportSummary = serde_json::from_str(&report.summary_json).unwrap_or_else(|_| panic!("report summary_json must parse"));
    let html = html::render_html(&summary);
    let subject = format!("Vigil monthly report — {} — {} uptime", report.label,
        summary.fleet.uptime_pct.map(|p| format!("{p:.2}%")).unwrap_or_else(|| "n/a".into()));
    let body_text = format!("Vigil monthly report for {}. Open the HTML version for the full report.", report.label);
    let mut any_ok = false;
    for (id, cfg) in channels {
        let r = dispatch::send_email_via_channel(state.transport.as_ref(), &cfg, &subject, &body_text, Some(html.clone())).await;
        let (ok, err) = match &r { Ok(()) => (true, None), Err(e) => (false, Some(e.to_string())) };
        any_ok |= ok;
        let _ = log_report(state, Some(id), ok, err.as_deref()).await;
    }
    if any_ok {
        let _ = sqlx::query("UPDATE reports SET emailed_at = ? WHERE id = ?").bind(now_ts()).bind(report.id).execute(&state.db).await;
        SendOutcome::Delivered
    } else {
        SendOutcome::AllFailed
    }
}

async fn log_report(state: &AppState, channel_id: Option<i64>, success: bool, error: Option<&str>) -> anyhow::Result<()> {
    sqlx::query("INSERT INTO notification_log (monitor_id, channel_id, incident_id, trigger, sent_at, success, error) VALUES (NULL, ?, NULL, 'report', ?, ?, ?)")
        .bind(channel_id).bind(now_ts()).bind(success).bind(error).execute(&state.db).await?;
    Ok(())
}
```

- [ ] **Step 4: Run pass** — `cargo test --test report_generate -- --test-threads=1`; build clean.
- [ ] **Step 5: Commit**
```bash
git commit -am "feat(p4.4): report generate (idempotent upsert) + auto-email fan-out (report trigger)"
```

---

### Task 5: `api/reports.rs` endpoints

**Files:** Create `crates/vigil/src/api/reports.rs`; Modify `crates/vigil/src/api/mod.rs`; Test `crates/vigil/tests/report_api.rs`.
**Interfaces produced (routes under `/api`):** `GET /reports`, `GET /reports/:id`, `POST /reports/generate`, `GET /reports/:id/html`, `POST /reports/:id/email`, `DELETE /reports/:id`.

- [ ] **Step 1: Write failing tests** — `crates/vigil/tests/report_api.rs`

```rust
mod common;
use axum::extract::{Path, State};
use axum::Json;
use serde_json::json;
use vigil::api::reports::{delete, generate as gen_handler, get_one, html, list, GenerateDto};

#[tokio::test]
async fn generate_then_list_then_get_then_html_then_delete() {
    let env = common::test_state().await;
    let g = gen_handler(State(env.state.clone()), Json(GenerateDto { period: "2026-03".into() })).await.unwrap().0;
    let id = g["id"].as_i64().unwrap();

    let listed = list(State(env.state.clone())).await.unwrap().0;
    assert_eq!(listed[0]["label"], "March 2026");
    // empty DB (no monitors) → fleet uptime None, 0 incidents
    assert!(listed[0]["headline"]["uptime_pct"].is_null());
    assert_eq!(listed[0]["headline"]["incidents"], 0);

    let one = get_one(State(env.state.clone()), Path(id)).await.unwrap().0;
    assert_eq!(one["summary"]["period"], "2026-03");

    let page = html(State(env.state.clone()), Path(id)).await.unwrap();
    assert!(page.0.contains("March 2026")); // axum::response::Html<String>

    let d = delete(State(env.state.clone()), Path(id)).await.unwrap().0;
    assert_eq!(d["ok"], true);
}

#[tokio::test]
async fn generate_rejects_future_and_malformed() {
    let env = common::test_state().await;
    assert!(gen_handler(State(env.state.clone()), Json(GenerateDto { period: "nope".into() })).await.is_err());
    assert!(gen_handler(State(env.state.clone()), Json(GenerateDto { period: "3000-01".into() })).await.is_err());
}
```

- [ ] **Step 2: Run fail** — `cargo test --test report_api -- --test-threads=1`.

- [ ] **Step 3: Implement `api/reports.rs`**

```rust
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::Html;
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};

use super::{db_err, now};
use crate::app::AppState;
use crate::report::{self, compute::ReportSummary, Report};

type ApiResult<T> = Result<Json<T>, (StatusCode, String)>;

fn not_found() -> (StatusCode, String) { (StatusCode::NOT_FOUND, "report not found".into()) }
fn invalid(m: &str) -> (StatusCode, String) { (StatusCode::UNPROCESSABLE_ENTITY, m.to_string()) }

pub async fn list(State(state): State<AppState>) -> ApiResult<Value> {
    let rows: Vec<Report> = sqlx::query_as("SELECT * FROM reports ORDER BY period_start DESC").fetch_all(&state.db).await.map_err(db_err)?;
    let out: Vec<Value> = rows.iter().map(|r| {
        let s: Option<ReportSummary> = serde_json::from_str(&r.summary_json).ok();
        let f = s.as_ref().map(|s| &s.fleet);
        json!({ "id": r.id, "label": r.label, "period_start": r.period_start, "period_end": r.period_end,
            "generated_at": r.generated_at, "emailed_at": r.emailed_at,
            "headline": { "uptime_pct": f.and_then(|f| f.uptime_pct), "incidents": f.map(|f| f.incidents), "downtime_seconds": f.map(|f| f.downtime_seconds) } })
    }).collect();
    Ok(Json(json!(out)))
}

pub async fn get_one(State(state): State<AppState>, Path(id): Path<i64>) -> ApiResult<Value> {
    let r: Option<Report> = sqlx::query_as("SELECT * FROM reports WHERE id = ?").bind(id).fetch_optional(&state.db).await.map_err(db_err)?;
    let r = r.ok_or_else(not_found)?;
    let summary: ReportSummary = serde_json::from_str(&r.summary_json).map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(json!({ "id": r.id, "label": r.label, "period_start": r.period_start, "period_end": r.period_end,
        "generated_at": r.generated_at, "emailed_at": r.emailed_at, "summary": summary })))
}

#[derive(Deserialize)]
pub struct GenerateDto { pub period: String }

pub async fn generate(State(state): State<AppState>, Json(dto): Json<GenerateDto>) -> ApiResult<Value> {
    // "YYYY-MM", not in the future (compare to the current UTC month)
    let ok_shape = dto.period.len() == 7 && dto.period.as_bytes()[4] == b'-'
        && dto.period[..4].chars().all(|c| c.is_ascii_digit()) && dto.period[5..].chars().all(|c| c.is_ascii_digit());
    if !ok_shape { return Err(invalid("period must be YYYY-MM")); }
    if dto.period.as_str() > report::month_of(now()).as_str() { return Err(invalid("period is in the future")); }
    let r = report::generate(&state, &dto.period).await.map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(json!({ "id": r.id, "label": r.label, "period_start": r.period_start })))
}

pub async fn html(State(state): State<AppState>, Path(id): Path<i64>) -> Result<Html<String>, (StatusCode, String)> {
    let r: Option<Report> = sqlx::query_as("SELECT * FROM reports WHERE id = ?").bind(id).fetch_optional(&state.db).await.map_err(db_err)?;
    let r = r.ok_or_else(not_found)?;
    let summary: ReportSummary = serde_json::from_str(&r.summary_json).map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Html(report::html::render_html(&summary)))
}

pub async fn email(State(state): State<AppState>, Path(id): Path<i64>) -> ApiResult<Value> {
    let r: Option<Report> = sqlx::query_as("SELECT * FROM reports WHERE id = ?").bind(id).fetch_optional(&state.db).await.map_err(db_err)?;
    let r = r.ok_or_else(not_found)?;
    let outcome = report::send_report_email(&state, &r).await;
    Ok(Json(json!({ "ok": matches!(outcome, crate::digest::SendOutcome::Delivered), "outcome": format!("{outcome:?}") })))
}

pub async fn delete(State(state): State<AppState>, Path(id): Path<i64>) -> ApiResult<Value> {
    sqlx::query("DELETE FROM reports WHERE id = ?").bind(id).execute(&state.db).await.map_err(db_err)?;
    Ok(Json(json!({ "ok": true })))
}
```
(`SendOutcome` needs `#[derive(Debug)]` — it already does from P4.3.)

Modify `crates/vigil/src/api/mod.rs`: add `pub mod reports;` to the module block, and these routes to the chain (after `/settings`):
```rust
        .route("/reports", get(reports::list))
        .route("/reports/generate", post(reports::generate))
        .route("/reports/:id", get(reports::get_one).delete(reports::delete))
        .route("/reports/:id/html", get(reports::html))
        .route("/reports/:id/email", post(reports::email))
```

- [ ] **Step 4: Run pass** — `cargo test --test report_api -- --test-threads=1`; build clean.
- [ ] **Step 5: Commit**
```bash
git add crates/vigil/src/api/reports.rs crates/vigil/tests/report_api.rs
git commit -am "feat(p4.4): reports API (list/get/generate/html/email/delete)"
```

---

### Task 6: Monthly scheduler

**Files:** Create/fill `crates/vigil/src/report/scheduler.rs`; Modify `crates/vigil/src/main.rs` (spawn), `crates/vigil/src/settings_store.rs` (report_tick_seconds + report_auto_generate + report_day_of_month + report_time — these are also used by Task 7, add here); Test `crates/vigil/tests/report_scheduler.rs`.
**Interfaces produced:** `report::scheduler::{should_run_today(now:i64, day_of_month:i64, time_offset:i64) -> bool, tick_once(&AppState), run(AppState), seed_marker_if_absent(&AppState)}`.

> Add the settings helpers needed by the scheduler here (Task 7 adds the API/TS surface). In `settings_store.rs`, add consts `DEFAULT_REPORT_DAY_OF_MONTH: i64 = 1`, `DEFAULT_REPORT_TIME: &str = "08:00"`, `DEFAULT_REPORT_TICK_SECONDS: i64 = 300`, and helpers `report_auto_generate(pool)->bool` (`get(pool,"report_auto_generate","1").await == "1"`), `report_day_of_month(pool)->i64`, `report_time(pool)->String`, `report_tick_seconds(pool)->i64` (mirror the digest helpers verbatim).

- [ ] **Step 1: Write failing tests** — `crates/vigil/tests/report_scheduler.rs`

```rust
mod common;
use common::{test_state, test_state_failing_transport};
use vigil::report::scheduler::{seed_marker_if_absent, should_run_today, tick_once};

fn ts(y: i32, mo: u32, d: u32, h: u32) -> i64 {
    chrono::NaiveDate::from_ymd_opt(y, mo, d).unwrap().and_hms_opt(h, 0, 0).unwrap().and_utc().timestamp()
}

#[test]
fn should_run_today_clamped_and_timed() {
    // day_of_month 1, 08:00 → fires on the 1st at/after 08:00
    assert!(should_run_today(ts(2026, 4, 1, 9), 1, 8 * 3600));
    assert!(!should_run_today(ts(2026, 4, 1, 7), 1, 8 * 3600)); // before 08:00
    // day 31 in April (30 days) clamps to 30 → fires on the 30th
    assert!(should_run_today(ts(2026, 4, 30, 9), 31, 8 * 3600));
    assert!(!should_run_today(ts(2026, 4, 29, 9), 31, 8 * 3600));
}

#[tokio::test]
async fn tick_backfills_missing_months_in_order() {
    let env = test_state().await;
    vigil::settings_store::set(&env.state.db, "report_auto_generate", "1").await.unwrap();
    vigil::settings_store::set(&env.state.db, "report_day_of_month", "1").await.unwrap();
    vigil::settings_store::set(&env.state.db, "report_time", "00:00").await.unwrap(); // day>=1 & 00:00 → always due
    // Marker EXACTLY two months behind the just-ended month → both missing months must
    // generate, in ascending order, and the marker must advance to the just-ended month.
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64;
    let target = vigil::report::prior_month(&vigil::report::month_of(now)); // just-ended month
    let first_missing = vigil::report::prior_month(&target);
    let two_behind = vigil::report::prior_month(&first_missing);
    vigil::settings_store::set(&env.state.db, "report.last_generated_period", &two_behind).await.unwrap();

    tick_once(&env.state).await.unwrap();

    let (ps_first, _) = vigil::report::month_bounds(&first_missing);
    let (ps_target, _) = vigil::report::month_bounds(&target);
    let rows: Vec<i64> = sqlx::query_scalar("SELECT period_start FROM reports ORDER BY period_start").fetch_all(&env.state.db).await.unwrap();
    assert_eq!(rows, vec![ps_first, ps_target], "exactly the two missing months, ascending");
    assert_eq!(vigil::settings_store::get(&env.state.db, "report.last_generated_period", "").await, target, "marker advanced to just-ended month");
}

#[tokio::test]
async fn tick_holds_marker_on_email_failure() {
    let env = test_state_failing_transport().await;
    let cid: i64 = sqlx::query_scalar("INSERT INTO notification_channels (name, type, config, is_active, created_at) VALUES ('e','email','{\"host\":\"h\",\"port\":25,\"security\":\"none\",\"from\":\"f@b\",\"to\":[\"a@b\"]}',1,0) RETURNING id").fetch_one(&env.state.db).await.unwrap();
    vigil::settings_store::set(&env.state.db, "report_auto_generate", "1").await.unwrap();
    vigil::settings_store::set(&env.state.db, "report_day_of_month", "1").await.unwrap();
    vigil::settings_store::set(&env.state.db, "report_time", "00:00").await.unwrap();
    vigil::settings_store::set(&env.state.db, "report_recipients", &format!("[{cid}]")).await.unwrap();
    // marker one month behind → generate the prior month, email fails → marker NOT advanced past it
    let prior = vigil::report::prior_month(&vigil::report::month_of(chrono::Utc::now().timestamp()));
    let before = vigil::report::prior_month(&prior);
    vigil::settings_store::set(&env.state.db, "report.last_generated_period", &before).await.unwrap();
    tick_once(&env.state).await.unwrap();
    let marker = vigil::settings_store::get(&env.state.db, "report.last_generated_period", "").await;
    assert_eq!(marker, before, "email AllFailed → marker held for retry");
}

#[tokio::test]
async fn seed_marker_only_when_absent() {
    let env = test_state().await;
    seed_marker_if_absent(&env.state).await.unwrap();
    let seeded = vigil::settings_store::get(&env.state.db, "report.last_generated_period", "").await;
    assert!(!seeded.is_empty());
    vigil::settings_store::set(&env.state.db, "report.last_generated_period", "2020-01").await.unwrap();
    seed_marker_if_absent(&env.state).await.unwrap();
    assert_eq!(vigil::settings_store::get(&env.state.db, "report.last_generated_period", "").await, "2020-01");
}
```

- [ ] **Step 2: Run fail** — `cargo test --test report_scheduler -- --test-threads=1`.

- [ ] **Step 3: Implement `report/scheduler.rs`**

```rust
//! Monthly report scheduler: on a configurable day-of-month + UTC time, backfill
//! every month between the marker and the just-ended month (idempotent), emailing
//! each; the marker advances only on a delivered/nothing-to-send outcome.

use std::time::Duration;

use crate::app::AppState;
use crate::digest::SendOutcome;
use crate::report::{self, month_of, next_month, prior_month};
use crate::settings_store;

fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64
}

/// Days in the UTC month containing `epoch`.
fn days_in_month(epoch: i64) -> u32 {
    let d = chrono::DateTime::<chrono::Utc>::from_timestamp(epoch, 0).unwrap_or_default().date_naive();
    let (y, m) = (d.format("%Y").to_string().parse::<i32>().unwrap_or(1970), d.format("%m").to_string().parse::<u32>().unwrap_or(1));
    let first_next = if m == 12 { chrono::NaiveDate::from_ymd_opt(y + 1, 1, 1) } else { chrono::NaiveDate::from_ymd_opt(y, m + 1, 1) };
    first_next.and_then(|nx| nx.pred_opt()).map(|last| chrono::Datelike::day0(&last) + 1).unwrap_or(28)
}

pub fn should_run_today(now_ts: i64, day_of_month: i64, time_offset: i64) -> bool {
    // chrono::Datelike methods called fully-qualified (no `use chrono` per Global Constraints).
    let dt = chrono::DateTime::<chrono::Utc>::from_timestamp(now_ts, 0).unwrap_or_default();
    let today = dt.date_naive();
    let eff_day = day_of_month.clamp(1, days_in_month(now_ts) as i64) as u32;
    let start_of_today = today.and_hms_opt(0, 0, 0).map(|d| d.and_utc().timestamp()).unwrap_or(0);
    chrono::Datelike::day(&today) >= eff_day && now_ts >= start_of_today + time_offset
}

fn parse_hm(s: &str) -> i64 {
    let mut it = s.split(':');
    let h = it.next().and_then(|x| x.parse::<i64>().ok()).filter(|h| (0..24).contains(h));
    let m = it.next().and_then(|x| x.parse::<i64>().ok()).filter(|m| (0..60).contains(m));
    match (h, m) { (Some(h), Some(m)) => h * 3600 + m * 60, _ => 8 * 3600 }
}

pub async fn seed_marker_if_absent(state: &AppState) -> anyhow::Result<()> {
    let existing: Option<String> = sqlx::query_scalar("SELECT value FROM settings WHERE key = 'report.last_generated_period'").fetch_optional(&state.db).await?;
    if existing.is_none() {
        settings_store::set(&state.db, "report.last_generated_period", &prior_month(&month_of(now()))).await?;
    }
    Ok(())
}

pub async fn tick_once(state: &AppState) -> anyhow::Result<()> {
    if !settings_store::report_auto_generate(&state.db).await { return Ok(()); }
    let now_ts = now();
    let target = prior_month(&month_of(now_ts));
    let day = settings_store::report_day_of_month(&state.db).await;
    let off = parse_hm(&settings_store::report_time(&state.db).await);
    if !should_run_today(now_ts, day, off) { return Ok(()); }
    let mut cursor = settings_store::get(&state.db, "report.last_generated_period", "").await;
    if cursor.is_empty() { cursor = prior_month(&target); } // safety; run() seeds first
    while next_month(&cursor).as_str() <= target.as_str() {
        let p = next_month(&cursor);
        let r = report::generate(state, &p).await?;
        let recips: Vec<i64> = serde_json::from_str(&settings_store::get(&state.db, "report_recipients", "[]").await).unwrap_or_default();
        let outcome = if recips.is_empty() { SendOutcome::NothingToSend } else { report::send_report_email(state, &r).await };
        match outcome {
            SendOutcome::Delivered | SendOutcome::NothingToSend => {
                settings_store::set(&state.db, "report.last_generated_period", &p).await?;
                cursor = p;
            }
            SendOutcome::AllFailed => break, // hold marker; retry next tick
        }
    }
    Ok(())
}

pub async fn run(state: AppState) {
    if let Err(e) = seed_marker_if_absent(&state).await { tracing::error!(error = %e, "report marker seed failed"); }
    loop {
        let tick = settings_store::report_tick_seconds(&state.db).await;
        if let Err(e) = tick_once(&state).await { tracing::error!(error = %e, "report tick failed"); }
        tokio::time::sleep(Duration::from_secs(tick.max(1) as u64)).await;
    }
}
```
Spawn in `main.rs` (add `report` to the `use vigil::{…}` line + a spawn after `digest::run`):
```rust
    tokio::spawn(report::scheduler::run(state.clone()));
```

- [ ] **Step 4: Run pass** — `cargo test --test report_scheduler -- --test-threads=1`; full suite green; build clean.
- [ ] **Step 5: Commit**
```bash
git add crates/vigil/src/report/scheduler.rs crates/vigil/tests/report_scheduler.rs
git commit -am "feat(p4.4): monthly report scheduler (UTC, backfill loop, email-retry marker)"
```

---

### Task 7: `report_*` settings (API + TS surface)

**Files:** Modify `crates/vigil/src/api/settings.rs`, `crates/vigil/src/settings_store.rs` (the `report_recipients` helper), `crates/vigil/tests/settings_p43.rs` (DTO literal fix — see below), `web/src/api.ts`; Test create `crates/vigil/tests/settings_p44.rs`.
(The `report_auto_generate`/`report_day_of_month`/`report_time`/`report_tick_seconds` helpers were added in Task 6.)

> **MUST also update `crates/vigil/tests/settings_p43.rs:48-57`** (Task 7 Step 3): it constructs `UpdateSettingsDto { … }` with today's exact 8 fields and no `..Default::default()` (the struct derives only `Deserialize`). Adding 4 fields to the DTO makes that literal fail with `E0063` and breaks the whole suite. Add `report_auto_generate: None, report_day_of_month: None, report_time: None, report_recipients: None,` to that literal. It's tracked → `git commit -am` captures it.

- [ ] **Step 1: Write failing test** — `crates/vigil/tests/settings_p44.rs`

```rust
mod common;
use axum::extract::State;
use axum::Json;
use serde_json::json;
use vigil::api::settings::{get_settings, update_settings, UpdateSettingsDto};

#[tokio::test]
async fn report_settings_roundtrip() {
    let env = common::test_state().await;
    let dto = UpdateSettingsDto {
        anchors: None, cooldown_minutes: None, retention_days: None, accent: None,
        renotify_hours: None, digest_enabled: None, digest_time: None, digest_recipients: None,
        report_auto_generate: Some(false), report_day_of_month: Some(3),
        report_time: Some("07:15".into()), report_recipients: Some(json!([2, 5])),
    };
    let _ = update_settings(State(env.state.clone()), Json(dto)).await.unwrap();
    let got = get_settings(State(env.state.clone())).await.unwrap().0;
    assert_eq!(got["report_auto_generate"], false);
    assert_eq!(got["report_day_of_month"], 3);
    assert_eq!(got["report_time"], "07:15");
    assert_eq!(got["report_recipients"], json!([2, 5]));
}
```

- [ ] **Step 2: Run fail** — `cargo test --test settings_p44 -- --test-threads=1`.

- [ ] **Step 3: Implement** — `crates/vigil/src/api/settings.rs`

Add to `current_settings` json! block:
```rust
        "report_auto_generate": settings_store::report_auto_generate(&state.db).await,
        "report_day_of_month": settings_store::report_day_of_month(&state.db).await,
        "report_time": settings_store::report_time(&state.db).await,
        "report_recipients": settings_store::report_recipients(&state.db).await,
```
Add a `settings_store::report_recipients(pool) -> Vec<i64>` helper (clone `digest_recipients`, key `"report_recipients"`, default `"[]"`).
Add to `UpdateSettingsDto`:
```rust
    pub report_auto_generate: Option<bool>,
    pub report_day_of_month: Option<i64>,
    pub report_time: Option<String>,
    pub report_recipients: Option<Value>,
```
Add to `update_settings`:
```rust
    if let Some(b) = dto.report_auto_generate {
        settings_store::set(&state.db, "report_auto_generate", if b { "1" } else { "0" }).await.map_err(set_err)?;
    }
    if let Some(d) = dto.report_day_of_month {
        settings_store::set(&state.db, "report_day_of_month", &d.to_string()).await.map_err(set_err)?;
    }
    if let Some(t) = dto.report_time {
        settings_store::set(&state.db, "report_time", &t).await.map_err(set_err)?;
    }
    if let Some(r) = dto.report_recipients {
        let ids: Vec<i64> = match &r { Value::Array(a) => a.iter().filter_map(|v| v.as_i64()).collect(), _ => Vec::new() };
        settings_store::set(&state.db, "report_recipients", &serde_json::to_string(&ids).unwrap_or_else(|_| "[]".into())).await.map_err(set_err)?;
    }
```
Extend `web/src/api.ts` `Settings` interface:
```typescript
  report_auto_generate: boolean;
  report_day_of_month: number;
  report_time: string;
  report_recipients: number[];
```

- [ ] **Step 4: Run pass** — `cargo test --test settings_p44 -- --test-threads=1`; build clean.
- [ ] **Step 5: Commit**
```bash
git add crates/vigil/tests/settings_p44.rs
git commit -am "feat(p4.4): report_* settings on /api/settings + store helpers"
```

---

### Task 8: Frontend — Reports screen + rail nav + api.ts + Settings block

**Files:** Create `web/src/components/Reports.tsx`, `web/src/__tests__/reports.test.tsx`; Modify `web/src/api.ts`, `web/src/App.tsx`, `web/src/components/Rail.tsx`, `web/src/components/Settings.tsx`.

- [ ] **Step 1: Write the failing test** — `web/src/__tests__/reports.test.tsx`

```tsx
import { render, screen, fireEvent } from "@solidjs/testing-library";
import { test, expect, vi } from "vitest";
import Reports from "../components/Reports";

function stub(reports: any[]) {
  const posts: any[] = [];
  vi.stubGlobal("fetch", vi.fn(async (url: any, opts?: any) => {
    const u = String(url);
    if (u === "/api/reports" && (!opts || !opts.method)) return { ok: true, json: async () => reports } as any;
    if (u === "/api/reports/generate" && opts?.method === "POST") { posts.push(JSON.parse(opts.body)); return { ok: true, json: async () => ({ id: 9, label: "March 2026", period_start: 0 }) } as any; }
    if (u === "/api/channels") return { ok: true, json: async () => [] } as any;
    return { ok: true, json: async () => [] } as any;
  }) as any);
  return posts;
}

test("renders month cards and generates a report", async () => {
  const posts = stub([{ id: 1, label: "February 2026", period_start: 100, generated_at: 0, emailed_at: null, headline: { uptime_pct: 99.9, incidents: 1, downtime_seconds: 60 } }]);
  render(() => <Reports />);
  expect(await screen.findByText("February 2026")).toBeTruthy();
  fireEvent.input(screen.getByLabelText(/month/i), { target: { value: "2026-03" } });
  fireEvent.click(screen.getByRole("button", { name: /generate/i }));
  await vi.waitFor(() => expect(posts.length).toBe(1));
  expect(posts[0].period).toBe("2026-03");
});

test("empty state", async () => {
  stub([]);
  render(() => <Reports />);
  expect(await screen.findByText(/no reports yet/i)).toBeTruthy();
});
```

- [ ] **Step 2: Run fail** — `cd web && npx vitest run reports`.

- [ ] **Step 3: Add api.ts fns** — `web/src/api.ts`

```typescript
export interface ReportCard {
  id: number; label: string; period_start: number; period_end?: number;
  generated_at: number; emailed_at: number | null;
  headline: { uptime_pct: number | null; incidents: number | null; downtime_seconds: number | null };
}
export function listReports(): Promise<ReportCard[]> {
  return fetch("/api/reports").then((r) => json(r));
}
export function getReport(id: number): Promise<any> {
  return fetch(`/api/reports/${id}`).then((r) => json(r));
}
export function generateReport(period: string): Promise<{ id: number; label: string }> {
  return fetch("/api/reports/generate", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ period }) }).then((r) => json(r));
}
export function reportHtml(id: number): Promise<string> {
  return fetch(`/api/reports/${id}/html`).then((r) => r.text()); // HTML, not JSON
}
export function emailReport(id: number): Promise<{ ok: boolean }> {
  return fetch(`/api/reports/${id}/email`, { method: "POST" }).then((r) => json(r));
}
export function deleteReport(id: number): Promise<{ ok: boolean }> {
  return fetch(`/api/reports/${id}`, { method: "DELETE" }).then((r) => json(r));
}
```

- [ ] **Step 4: Create `Reports.tsx`** (month cards + generate + iframe view + refetch-after-action)

```tsx
import { createResource, createSignal, For, Show, type Component } from "solid-js";
import * as api from "../api";

const Reports: Component = () => {
  const [reports, { refetch }] = createResource(() => api.listReports().catch(() => [] as api.ReportCard[]));
  const [period, setPeriod] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const [openId, setOpenId] = createSignal<number | null>(null);
  const [html] = createResource(openId, (id) => (id != null ? api.reportHtml(id).catch(() => "") : ""));

  async function handleGenerate() {
    if (!period().trim()) return;
    setBusy(true);
    try { await api.generateReport(period().trim()); setPeriod(""); refetch(); } catch { /* keep field */ } finally { setBusy(false); }
  }
  async function handleDelete(id: number) { try { await api.deleteReport(id); if (openId() === id) setOpenId(null); refetch(); } catch { /* retry */ } }
  async function handleEmail(id: number) { try { await api.emailReport(id); refetch(); } catch { /* retry */ } }

  return (
    <div class="settings-view reports-view">
      <h2 class="settings-title">Reports</h2>
      <section class="form-section settings-section">
        <h3 class="form-section-title">Generate a report</h3>
        <div class="form-field">
          <label for="report-month">Month (YYYY-MM)</label>
          <input id="report-month" type="month" value={period()} onInput={(e) => setPeriod(e.currentTarget.value)} />
        </div>
        <div class="detail-actions">
          <button type="button" class="btn-accent" disabled={busy()} onClick={handleGenerate}>{busy() ? "Generating…" : "Generate report"}</button>
        </div>
      </section>
      <section class="form-section settings-section">
        <h3 class="form-section-title">Past reports</h3>
        <Show when={(reports() ?? []).length === 0}><p class="settings-note">No reports yet.</p></Show>
        <For each={reports() ?? []}>
          {(r) => (
            <div class="notif-row">
              <button type="button" class="btn-link" onClick={() => setOpenId(r.id)}>{r.label}</button>
              <span class="settings-note mono">{r.headline?.uptime_pct != null ? `${r.headline.uptime_pct}%` : "—"}</span>
              <span class="settings-note">{r.headline?.incidents ?? 0} incidents</span>
              <a class="btn-link" href={`/api/reports/${r.id}/html`} target="_blank" rel="noreferrer">Export HTML</a>
              <button type="button" class="btn-link" onClick={() => handleEmail(r.id)}>Email now</button>
              <button type="button" class="btn-link danger" onClick={() => handleDelete(r.id)}>Delete</button>
            </div>
          )}
        </For>
      </section>
      <Show when={openId() != null}>
        <section class="form-section settings-section">
          <iframe title="report" srcdoc={html() ?? ""} style="width:100%;height:70vh;border:1px solid var(--border-default);border-radius:10px;background:#fff" />
        </section>
      </Show>
    </div>
  );
};

export default Reports;
```

- [ ] **Step 5: Wire the rail + route** — `web/src/components/Rail.tsx`: extend `RailView` with `| "reports"`; add to `ICON_PATHS` a `reports` glyph:
```typescript
  // bar-chart — reports
  reports: "M3 3v18h18M8 17V9M13 17V5M18 17v-7",
```
add to `NAV_ITEMS` (before Settings): `{ icon: "reports", label: "Reports", key: "reports" },`.
`web/src/App.tsx`: `import Reports from "./components/Reports";`; extend the onNavigate whitelist with `|| key === "reports"`; add inside `<Switch>`:
```tsx
          <Match when={view() === "reports"}>
            <div class="app-content">
              <Reports />
            </div>
          </Match>
```

- [ ] **Step 6: Add the Settings block** — `web/src/components/Settings.tsx` (model on the digest block): signals `reportAutoGenerate`/`reportDayOfMonth`/`reportTime`/`reportRecipients` + saved/saving; load in `onMount`; `handleSaveReport()` PUTs `{ report_auto_generate, report_day_of_month, report_time, report_recipients }`; a "Monthly reports" `<section>` with a checkbox (auto-generate), a number input (day of month, min 1 max 31), a text input (time HH:MM UTC), and the `emailChannels()` checkbox-list bound to `reportRecipients()` — identical structure to the digest block quoted in the codebase.

- [ ] **Step 7: Run all frontend checks** — `cd web && npx vitest run && npx tsc --noEmit && npx vite build` (all clean).

- [ ] **Step 8: Commit**
```bash
git add web/src/components/Reports.tsx web/src/__tests__/reports.test.tsx
git commit -am "feat(p4.4): Reports screen (month cards + iframe view) + rail nav + settings block"
```

---

### Task 9: Full suite, live acceptance, finish

- [ ] **Step 1: Full backend suite** — `cargo test -- --test-threads=1` (0 failures) + `cargo tree -e normal,build,dev | grep -Ei "aws-lc|openssl"` returns nothing.
- [ ] **Step 2: Frontend** — `cd web && npx vitest run && npx tsc --noEmit && npx vite build` (all green).
- [ ] **Step 3: Live acceptance** (ephemeral container, host 8098, fresh DB, no channels → no real emails; production `vigil-data` untouched): container boots healthy with the scheduler spawned (no panic); `POST /api/reports/generate {period:"<a past month>"}` returns a report; `GET /api/reports` lists it with a headline; `GET /api/reports/:id/html` returns `text/html` containing the month label + `@media print`; `DELETE` removes it; with no recipients configured no email is sent. Tear down the ephemeral container + image afterward.
- [ ] **Step 4: Finish** — use superpowers:finishing-a-development-branch. Merge `feat/p4-monthly-reports` → master (local fast-forward), delete branch, do **not** push. Rebuild + redeploy production; confirm `healthz=200`, `/api/reports` returns `[]`, and the Reports rail item appears.
- [ ] **Step 5: Memory + ledger** — record P4.4 shipped in `.superpowers/sdd/progress.md` + the Vigil memory; note P4.5 (backup/export) is next.

---

## Self-Review

**Spec coverage:** §3 migration → Task 1 (+ db.rs wiring, the M2 must-fix); §4 compute (durable had_any M1, single-pass fleet, both-ends clip, up_count avg, whole-month p95, distinct alerts, delta-live, cert_outlook) → Task 2; §6 HTML → Task 3; §5 generate + §9 auto-email → Task 4; §7.1 API → Task 5; §8 scheduler (backfill S1 + retry S2 + clamp) → Task 6; §10 settings → Tasks 6/7; §7.2 frontend → Task 8; §11 (no event) honored; §13 tests distributed; §15 boundaries respected (UTC, HTML-only, p95/delta/outlook caveats). All covered.

**Placeholder scan:** no placeholders; the earlier `let _ = denom;` artifact and the double-compute were removed from the Task 2 code block (single-pass `uptime::compute`); all steps contain complete code.

**Type consistency:** `ReportSummary`/`FleetReport`/`MonitorReport`/`ReportIncident`/`ExpiryItem`/`LongestOutage` field names are identical across compute (Task 2), html (Task 3), generate (Task 4), and api (Task 5). `report::{generate, send_report_email, month_of, prior_month, next_month, month_bounds, month_label}` and `scheduler::{should_run_today, tick_once, run, seed_marker_if_absent}` names match across tasks. `SendOutcome` reused from `digest`. Settings keys (`report_auto_generate`/`report_day_of_month`/`report_time`/`report_recipients`/`report_tick_seconds`/`report.last_generated_period`) identical across Tasks 6/7.
