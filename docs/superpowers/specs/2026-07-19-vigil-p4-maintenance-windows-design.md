# Vigil P4.2 — Maintenance Windows — Design Spec

> Second sub-project of P4 (Complete). Schedule planned work so alerts are suppressed and uptime %
> isn't dinged during known downtime. One-off or recurring (cron) windows, scoped to all / a tag / a
> set of monitors, suppressing either alerts (keep probing — preserves uptime data) or checks (pause
> probing). Per CLAUDE.md §8.

Builds on P1–P3 + P4.1 (on `master`). Single Rust/axum binary + SolidJS SPA, SQLite (WAL) on a mounted
volume, Docker (host 8099 → container 8090), rustls-only, uptime derived from incidents.

---

## 1. Goal & scope

A window has a name, a **scope** (all monitors / a tag / a specific set), a **schedule** (one-off
`[starts_at, ends_at]` or recurring via a cron expression), and a **suppress mode** (`alerts` or
`checks`). While a window is active and a monitor is in its scope, that monitor is *in maintenance*:
its alerts are suppressed, its uptime denominator excludes the maintenance time, and the UI shows a
`MAINTENANCE` state. `suppress='checks'` additionally pauses probing.

**In scope:** the `maintenance_windows` table + CRUD API + preview; the pure resolve logic (scope +
one-off/cron time matching); the three effects (alert suppression, check suppression, uptime
exclusion — the last on the **live incident-derived stats/bars**, not a retroactive rewrite of the
durable daily rollups); an evaluator task that drives live `MAINTENANCE` display via SSE; and the
Maintenance UI screen. **Out of scope (later P4 sub-projects):** notification throttling/digest,
reports, backup, theming.

**Naming:** the existing `crates/vigil/src/maintenance.rs` is the nightly **DB** rollup/prune/vacuum
job — UNRELATED. This feature lives in a new `crates/vigil/src/maintenance_windows/` module and
`crates/vigil/src/api/maintenance.rs`. Do not touch `maintenance.rs`.

---

## 2. Data model — migration `0005_maintenance_windows.sql`

```sql
CREATE TABLE maintenance_windows (
  id          INTEGER PRIMARY KEY,
  name        TEXT NOT NULL,
  scope       TEXT NOT NULL DEFAULT 'all',       -- all | tag | monitors
  target_ref  TEXT,                              -- JSON: null (all) | "tagname" (tag) | [1,2,3] (monitors)
  starts_at   INTEGER NOT NULL,                  -- epoch; the first occurrence's start
  ends_at     INTEGER NOT NULL,                  -- epoch; the first occurrence's end (duration = ends_at - starts_at)
  recurrence  TEXT,                              -- null (one-off) | a 5-field cron expression (UTC)
  suppress    TEXT NOT NULL DEFAULT 'alerts',    -- alerts | checks
  is_active   INTEGER NOT NULL DEFAULT 1,        -- disable without deleting
  created_at  INTEGER NOT NULL
);
CREATE INDEX idx_mw_active ON maintenance_windows(is_active);
```

- Append `(5, include_str!("../migrations/0005_maintenance_windows.sql"))` to `MIGRATIONS` in `db.rs`.
  Additive; a P1–P4.1 DB upgrades cleanly (a pure `CREATE TABLE`, no monitor changes).
- **For recurring windows, the duration is `ends_at - starts_at`** and the cron expression generates
  occurrence START times ≥ `starts_at`; each occurrence lasts that duration. (The absolute `ends_at`
  is just the first occurrence's end / the duration anchor.) Validation: `ends_at > starts_at`.

---

## 3. Enums & models

- **`Status::Maintenance`** — add to the `Status` enum (`as_str` → `"maintenance"`; `from_db`
  `"maintenance" => Maintenance`). **DISPLAY-ONLY: it is NEVER written to the `monitors.status`
  column.** It is the *effective* status computed at serialize/emit time by overlaying the real
  status when a monitor is in maintenance (see §5.1). The monitor's true up/down/pending status is
  always preserved in the DB — this is why maintenance is an overlay, not a stored state (no
  status-restore problem when a window ends, unlike the anchor-UNKNOWN path).
- **`MaintenanceWindow`** struct + FromRow (mirrors the table). Fields parsed lazily: `target_ref`
  stays a raw `Option<String>` (JSON) on the struct; the resolve module parses it.
- **DTOs:** `CreateMaintenanceWindowDto { name, scope, target_ref (JSON value), starts_at, ends_at,
  recurrence, suppress }` and `UpdateMaintenanceWindowDto` (all optional). `is_active` toggled via a
  dedicated field on update.

---

## 4. Cron — the `croner` crate

Recurring windows use a standard 5-field cron expression, parsed/evaluated by the pure **`croner`**
crate (no TLS, rustls-clean; add `croner = "2"` — the implementer confirms the exact version/API).
Cron times are evaluated in **UTC** (the app stores UTC epochs; document that a window's cron fires on
UTC wall-clock). The two operations needed (the plan pins the exact `croner` method names):
- **"is `now` inside an occurrence"** — find the latest occurrence start ≤ `now` (croner's
  previous-occurrence lookup from `now`), and the window is active iff that start ≥ `starts_at` AND
  `now < occ_start + duration`.
- **"enumerate occurrences overlapping `[a, b]`"** (for uptime exclusion) — iterate occurrence starts
  from `a` forward (croner's next-occurrence), each interval `[occ_start, occ_start + duration]`,
  stop once `occ_start > b`. **Cap the iteration** (e.g. 10_000 occurrences) as a runaway guard and
  `log` if the cap is hit (a per-minute cron over 90 days is nonsensical for maintenance; the cap
  protects the stats endpoint).

An invalid cron expression is rejected at window create/update (422), so the resolve path never sees
a malformed expression.

---

## 5. The resolve module (`maintenance_windows/resolve.rs` — pure, the DRY core)

```rust
pub enum Suppress { Alerts, Checks }

pub struct ActiveWindow<'a> { pub window: &'a MaintenanceWindow, pub suppress: Suppress }

// One window, one instant:
pub fn window_active_at(w: &MaintenanceWindow, now: Ts) -> bool;          // one-off or cron (§4)
pub fn monitor_in_scope(w: &MaintenanceWindow, monitor_id: i64, tags: &[String]) -> bool;
//   all → true; tag → tags contains target_ref (the tag string); monitors → target_ref (id array) contains monitor_id

// The decision for one monitor across ALL active windows it's in scope for:
//   returns the STRONGEST suppression: Checks if ANY active in-scope window is 'checks', else
//   Alerts if any is 'alerts', else None. (Checks implies alerts.)
pub fn maintenance_for(monitor_id: i64, tags: &[String], windows: &[MaintenanceWindow], now: Ts) -> Option<Suppress>;

// Union of maintenance intervals overlapping [from, to] for a monitor (for uptime, §5.3):
pub fn maintenance_intervals(monitor_id: i64, tags: &[String], windows: &[MaintenanceWindow], from: Ts, to: Ts) -> Vec<(Ts, Ts)>;
//   enumerates one-off + cron occurrences of every in-scope active window overlapping [from,to],
//   clips each to [from,to], then MERGES overlapping intervals into a sorted disjoint set.
```

`tags` is parsed from `monitor.tags` (the existing JSON-array-string column, unused since P1) via a
small helper `parse_tags(&Option<String>) -> Vec<String>`.

### 5.1 Effect A — Display overlay (`Status::Maintenance`)

A monitor's **effective status** = `Maintenance` when `maintenance_for(..) == Some(_)`, else its real
`status`. Applied at every point that serializes/emits a monitor's status to the UI, via one helper
`overlay_maintenance(&mut [Monitor], &[MaintenanceWindow], now)` that sets `m.status =
Status::Maintenance.as_str()` for in-maintenance monitors:
- `api::monitors::list` (the `/api/monitors` list),
- `api::monitors::get_one`,
- the SSE `build_snapshot` (sse.rs) `Event::Snapshot { monitors }`.

The **evaluator task** (§6) emits an `Event::MonitorUpdated { status: effective }` when a monitor
*enters* or *exits* maintenance, so an open dashboard flips to/from `MAINTENANCE` live. The frontend
needs no maintenance logic — it just renders `monitor.status`, which is already `"maintenance"` when
applicable. (The real status still lives in the DB for all engine logic.)

### 5.2 Effect B — Alert suppression (`notify::dispatch::deliver`)

At the top of `deliver()` (dispatch.rs:142), before loading channels: load the active windows and
compute `maintenance_for(m.id, tags, &windows, now)`. If `Some(_)` (either suppress kind suppresses
alerts), **log one debug line and return `Ok(())`** — no channel is contacted, nothing is written to
`notification_log`. This covers ALL alert triggers (down / recovered / ssl / domain / heartbeat_missed)
because every alert path funnels through `deliver()`. (Recovered is also suppressed during
maintenance — consistent: the operator doesn't want a "recovered" ping for planned work either.)

### 5.3 Effect C — Check suppression (`worker::run_check`, `suppress='checks'` only)

At the top of `worker::run_check` (worker.rs, beside the `is_paused` / heartbeat guards): if
`maintenance_for(m.id, tags, &windows, now) == Some(Suppress::Checks)`, `signal_complete` and return
WITHOUT probing (no probe, no `checks` row, no state change — the real status freezes at its last
value). `suppress='alerts'` windows do NOT skip the probe (they keep probing so uptime data
continues; only alerts are suppressed by Effect B).

**Heartbeats:** the probe path doesn't touch heartbeat monitors (they're ping/reaper-driven), so the
`checks`-suppression equivalent for a heartbeat is the **reaper**. In `heartbeat::reap_once`, exclude
monitors in an active `Suppress::Checks` window from the due-set (or skip the DOWN transition for
them), so a missed check-in during planned maintenance does NOT open a DOWN incident. An
`alerts`-window heartbeat still reaps (opens the incident) but its `heartbeat_missed` alert is
suppressed by Effect B — consistent with the probe model. (The reaper computes `maintenance_for`
per candidate monitor, same helper.)

### 5.4 Effect D — Uptime exclusion (`uptime::compute`)

Extend `compute` to accept the monitor's maintenance intervals within the window and exclude them:
```rust
pub fn compute(spans: &[Span], window_start: Ts, now: Ts, had_any_check: bool, maintenance: &[(Ts,Ts)]) -> Uptime;
```
- **effective denominator** = `(now - window_start)` minus total maintenance overlap within
  `[window_start, now]`.
- **effective downtime** = for each down-span clipped to `[window_start, now]`, subtract the parts
  overlapping `maintenance` (a pure `interval-minus-intervals` helper), then sum.
- `uptime_pct = Some(1 - effective_downtime/effective_denominator)`; if `effective_denominator <= 0`
  (the whole window was maintenance) → `uptime_pct: None, downtime_seconds: 0` (nothing to report).
- Existing callers pass `&[]` (no change in behavior) EXCEPT `stats` and the 90-day `bars` builder,
  which pass `maintenance_intervals(...)` for the range/day. **This is the one intricate part** — the
  interval arithmetic (subtract a merged interval set from both the denominator and each down-span)
  must be a small, unit-tested pure helper `subtract_intervals(base:(Ts,Ts), cuts:&[(Ts,Ts)]) ->
  Vec<(Ts,Ts)>`.

Scope note: exclusion applies to the **live** incident-derived `stats`/`bars`. The durable
`check_aggregates_daily` rollups are NOT retroactively rewritten (same v1 boundary as heartbeats —
documented, not silent).

---

## 6. Evaluator task (`maintenance_windows::run`)

Spawned in `main::serve`. Loop `sleep(tick)` (`maintenance.tick_seconds`, default 30). Each pass:
1. Load `is_active=1` windows + all monitors (id, status, tags).
2. Compute the current in-maintenance set `{monitor_id}` via `maintenance_for`.
3. Diff against the previous tick's set (held in the task's local state):
   - **entered** maintenance → `Event::MonitorUpdated { id, status: "maintenance", .. }`.
   - **exited** maintenance → `Event::MonitorUpdated { id, status: <real status from DB>, .. }`.
4. (No DB writes — the evaluator is pure display; it never mutates `monitors.status`.)

This is the single live-update driver. Alert/check suppression (§5.2/§5.3) recompute on-demand at
their own decision points (cheap active-windows query — few windows, single operator), so there's no
shared-mutable-set to keep in sync.

---

## 7. API (`api/maintenance.rs`, mounted under `/api`)

- `GET /api/maintenance-windows` → list (all windows).
- `POST /api/maintenance-windows` → create (validate: name non-empty; scope ∈ {all,tag,monitors} with
  a matching `target_ref` shape; `ends_at > starts_at`; a non-null `recurrence` must parse as cron;
  `suppress` ∈ {alerts,checks} — else 422).
- `PUT /api/maintenance-windows/:id` → update (same validation; can toggle `is_active`).
- `DELETE /api/maintenance-windows/:id`.
- `GET /api/maintenance-windows/:id/preview` → `{ affected_monitor_ids: [...], active_now: bool }`
  (resolve the scope against the current monitors + whether it's active at `now`) — powers the form's
  "this affects N monitors" hint.
- No secrets involved; standard `AppState` handlers.

---

## 8. UI

- **Rail:** a "Maintenance" nav entry (the Rail already has Dashboard/Incidents/Notifications/
  Settings — add Maintenance) → a Maintenance screen.
- **Maintenance screen:** a list of windows (name, scope summary, schedule summary — "one-off Mar 3
  02:00–04:00" or "cron `0 2 * * 0`", suppress badge, active/disabled toggle) + a **Create window**
  form: name; **scope** picker (All / Tag [tag input] / Monitors [multi-select of monitor names]);
  **schedule** (one-off: start + end datetime pickers; recurring: a cron field + duration, with a
  plain-language preview of the next few fire times); **suppress** radio (Alerts / Checks); a live
  "affects N monitors" count via the preview endpoint. Edit/delete/enable-disable per row.
- **Monitor cards / detail:** when `status === 'maintenance'`, render the existing status pill in the
  `--maintenance` accent (already a design token in CLAUDE.md §11.1: `--maintenance: #B58BF5`); the
  detail panel shows a small "In maintenance: <window name>" note. No new state-management approach —
  `status` already carries `"maintenance"` from the backend overlay.
- api.ts: `listMaintenanceWindows`, `createMaintenanceWindow`, `updateMaintenanceWindow`,
  `deleteMaintenanceWindow`, `previewMaintenanceWindow`.

---

## 9. Testing

**Rust (pure resolve — the priority, since the logic is subtle):**
- `window_active_at`: one-off before/during/after; a cron window active during an occurrence and
  inactive between occurrences; a cron window inactive before `starts_at`.
- `monitor_in_scope`: all→true; tag match/no-match; monitors-array match/no-match; empty/malformed
  `target_ref` → false (never panics).
- `maintenance_for`: strongest-suppression (checks beats alerts beats none) across overlapping windows.
- `maintenance_intervals` + `subtract_intervals`: overlap clipping, MERGING overlapping windows,
  subtracting a cut-set from a down-span (partial overlap, full cover, no overlap, multiple cuts).
- `compute` with maintenance: downtime fully inside a window → uptime 100% (excluded); partial
  overlap → only the non-maintenance part counts; whole window is maintenance → `uptime_pct: None`.

**Rust (integration):**
- alert suppression: a monitor in an active window + a channel on `down`; drive it DOWN via
  `apply_result`; assert NO notification delivered (the double recorded 0) AND the incident still
  opened (maintenance suppresses the *alert*, not the incident/state).
- check suppression: a monitor in an active `checks` window; `worker::run_check`; assert NO `checks`
  row written and status unchanged.
- migration 0005 (fresh → v5 + table selectable; v4-DB upgrade preserves data).
- API: create (valid + each 422 case) / list / preview (correct affected ids + active_now) / delete.
- evaluator: seed a window active now over a monitor; run one evaluator pass; assert a
  `MonitorUpdated{status:"maintenance"}` was emitted; deactivate → next pass emits the real status.

**Web (vitest):** the Maintenance screen renders a window list + create form; the scope picker toggles
tag/monitors inputs; the one-off/recurring toggle swaps date-pickers vs cron field; saving calls
`createMaintenanceWindow` with the right DTO; a `status:"maintenance"` monitor renders the maintenance
pill.

---

## 10. Non-functional

- rustls-only preserved; the only new crate is `croner` (pure cron parser, no TLS). Assert
  `cargo tree | grep -iE 'aws-lc|openssl'` stays empty.
- Migration 0005 is additive (a new table only); no monitor-row or existing-data changes.
- Single-operator: on-demand active-windows queries in `deliver`/`worker` are cheap (few windows); no
  new index needed beyond `idx_mw_active`.
- The evaluator never writes `monitors.status` — the real status is authoritative; maintenance is a
  pure overlay, so disabling/deleting a window instantly reverts the display with zero cleanup.

---

## 11. Build phases (for the plan)

1. Migration 0005 + `MaintenanceWindow` model/DTOs + `Status::Maintenance` (+ the exhaustive `Status`
   matches it forces) + `croner` dep + validation helpers.
2. `maintenance_windows/resolve.rs` — the pure core (`window_active_at`, `monitor_in_scope`,
   `maintenance_for`, `maintenance_intervals`, `subtract_intervals`, `parse_tags`) with heavy unit
   tests.
3. `uptime::compute` maintenance param + the `stats`/`bars` callers passing intervals.
4. Alert suppression in `deliver()`; check suppression in `worker::run_check` AND the heartbeat
   `reap_once` (a `Suppress::Checks` window skips the reaper's DOWN for heartbeats).
5. The display overlay (`overlay_maintenance` at list/get_one/snapshot) + the evaluator task + spawn.
6. API (`api/maintenance.rs` CRUD + preview) + routes.
7. Frontend (Rail entry + Maintenance screen + create form + maintenance pill) + api.ts.
8. Acceptance (live via Docker: create a one-off window over a monitor, watch it flip to MAINTENANCE +
   suppress a down-alert + exclude uptime; a `checks` window skips probing; a cron window) + final
   review + merge.

---

## 12. Decisions log (resolved)

1. **MAINTENANCE = derived display overlay** (`Status::Maintenance` never stored in `monitors.status`)
   — preserves real health, no restore problem. ✅
2. **Recurring = cron via the pure `croner` crate** (UTC). ✅
3. **`suppress='alerts'` default** (keeps probing → preserves uptime data). ✅
4. **Uptime exclusion on live stats/bars**, not a retroactive rollup rewrite (documented v1 boundary). ✅
5. **Strongest-suppression wins** when a monitor is in multiple overlapping windows (checks > alerts). ✅
6. **Recovered alerts are also suppressed** during maintenance (no "recovered" ping for planned work). ✅
