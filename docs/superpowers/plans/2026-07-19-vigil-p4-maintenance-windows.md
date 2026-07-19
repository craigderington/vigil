# Vigil P4.2 — Maintenance Windows — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: superpowers:subagent-driven-development. Steps use `- [ ]`.
> **Autonomous build.** Builds on P1–P4.1 (on `master`). Base: current `master` HEAD. Branch `feat/p4-maintenance`.

**Goal:** Schedule planned-work windows (one-off or cron, scoped to all/tag/monitors) that suppress
alerts (or pause checks) and exclude the maintenance time from uptime %, showing a MAINTENANCE overlay.

**Architecture:** A `maintenance_windows` table + a pure `resolve` module (scope + one-off/cron time
matching) feeding three suppression effects (alert suppression in `deliver()`, check suppression in
`worker`/`reap_once`, uptime exclusion in `uptime::compute`) plus a client-side display overlay
(`MaintenanceChanged` SSE + a `maintenance_ids` snapshot field → a frontend `maintenanceIds` set).
**Maintenance is NOT a monitor status** — `monitors.status` stays truthful; the overlay is orthogonal.

**Tech stack:** Rust (tokio/axum/sqlx-sqlite, rustls) + SolidJS/TS. One new crate: `croner` (pure).

Spec: [`docs/superpowers/specs/2026-07-19-vigil-p4-maintenance-windows-design.md`](../specs/2026-07-19-vigil-p4-maintenance-windows-design.md).

## Global Constraints

- Inherit ALL P1–P4.1 constraints: rustls only (**no openssl/aws-lc**); i64 epoch seconds; SQLite
  FK/WAL; uptime from incidents; version-ordered migration runner; conventional commits; TDD. **After
  each backend task assert `cargo tree -p vigil -e normal,build,dev | grep -iE 'aws-lc|openssl'` empty.**
- **`croner` is forward-only** — no `find_previous_occurrence`. Use a bounded forward scan.
- **`maintenance` is NEVER written to `monitors.status`** and is NOT a `Status` enum variant — it's a
  client-side overlay via `Event::MaintenanceChanged` + `Snapshot.maintenance_ids`.
- **Check-suppression MUST advance `next_run_at`** before returning (else the scheduler hot-loops).
- **The reaper maintenance filter is `Suppress::Checks`-ONLY** and computed OUTSIDE `reap_one`'s tx.
- **`target_ref`** is a serde_json STRING (`to_string` write / `from_str` read); per-scope validated.
- Run the full Rust suite with `--test-threads=1` (a pre-existing sqlx flake appears under parallel runs).

---

## Shared Types & Interfaces (the DRY backbone)

```rust
// models.rs — MaintenanceWindow (derive FromRow, like Channel) + DTOs. NO Status change.
#[derive(Clone, Debug, sqlx::FromRow, serde::Serialize)]
pub struct MaintenanceWindow {
  pub id:i64, pub name:String, pub scope:String, pub target_ref:Option<String>,   // target_ref = JSON string
  pub starts_at:i64, pub ends_at:i64, pub recurrence:Option<String>, pub suppress:String,
  pub is_active:bool, pub created_at:i64,
}
pub struct CreateMaintenanceWindowDto { name:String, scope:String,
  #[serde(default)] target_ref:Option<serde_json::Value>, starts_at:i64, ends_at:i64,
  #[serde(default)] recurrence:Option<String>, #[serde(default="default_suppress")] suppress:String }
pub fn default_suppress()->String { "alerts".into() }
pub struct UpdateMaintenanceWindowDto { /* all Option incl is_active:Option<bool> */ }

// maintenance_windows/resolve.rs — pure core
pub enum Suppress { Alerts, Checks }
pub fn parse_tags(raw:&Option<String>) -> Vec<String>;
pub fn window_active_at(w:&MaintenanceWindow, now:i64) -> bool;                       // one-off or cron (forward scan)
pub fn monitor_in_scope(w:&MaintenanceWindow, monitor_id:i64, tags:&[String]) -> bool;
pub fn maintenance_for(monitor_id:i64, tags:&[String], windows:&[MaintenanceWindow], now:i64) -> Option<Suppress>; // Checks>Alerts>None
pub fn occurrences_overlapping(w:&MaintenanceWindow, from:i64, to:i64) -> Vec<(i64,i64)>;    // scan from (from - dur)
pub fn maintenance_intervals(monitor_id:i64, tags:&[String], windows:&[MaintenanceWindow], from:i64, to:i64) -> Vec<(i64,i64)>; // merged
pub fn subtract_intervals(base:(i64,i64), cuts:&[(i64,i64)]) -> Vec<(i64,i64)>;
// maintenance_windows/mod.rs
pub async fn active_windows(pool:&SqlitePool) -> Vec<MaintenanceWindow>;              // SELECT * WHERE is_active=1
pub async fn run(state:AppState);                                                     // evaluator task

// engine/events
// Event::MaintenanceChanged { id:i64, in_maintenance:bool }  (serde tag "maintenance_changed")
// Event::Snapshot gains: maintenance_ids: Vec<i64>

// uptime.rs — new trailing param (existing callers pass &[])
pub fn compute(spans:&[Span], window_start:Ts, now:Ts, had_any_check:bool, maintenance:&[(Ts,Ts)]) -> Uptime;

// scheduler.rs (existing): pub fn next_run_with_jitter(now:Ts, base_secs:i64) -> Ts;   // used by check-suppression
```

**AppState** `{ db, bus, transport, sched_tx, anchor, http_sender }`. **`deliver`** already gets `&Monitor`
(so `m.tags` is available). **`Event`** enum (events.rs) uses `#[serde(tag="event", content="data",
rename_all="snake_case")]`. **`next_run_with_jitter`** at scheduler.rs:37.

---

## Task 1: Migration 0005 + MaintenanceWindow model + DTOs + croner + validation

**Files:** Create `crates/vigil/migrations/0005_maintenance_windows.sql`; Modify `src/db.rs`,
`src/models.rs`, `crates/vigil/Cargo.toml`, `src/api/maintenance.rs` (new — validation helper here or in
models). Test: `tests/migrate5.rs`, unit tests for validation.

**Interfaces produced:** `MaintenanceWindow`, the DTOs, `validate_window_dto`, `croner` available.

- [ ] **Step 1: `0005_maintenance_windows.sql`** — the §2 `CREATE TABLE` verbatim (no index).
- [ ] **Step 2: Add dep** — **`croner = "2"`** in Cargo.toml (the `"2"` pin is load-bearing: croner
  3.x removed `Cron::new(expr).parse()` — every snippet here uses the 2.x API). `cargo build`, then
  `cargo tree -p vigil -e normal,build,dev | grep -iE 'aws-lc|openssl'` MUST be empty (croner pulls
  chrono(clock) + iana-time-zone — both pure; if aws-lc appears, stop and report).
- [ ] **Step 3: Failing test** — `tests/migrate5.rs`: fresh DB → `MAX(version)=5` + `SELECT * FROM
  maintenance_windows` succeeds; a v4-DB upgrade test (apply 0001-0004, record versions, connect →
  only 0005 applies, prior data preserved). Plus validation unit tests: valid create; 422 on bad scope,
  on `scope='monitors'` with a non-array/empty-array target_ref, on `scope='tag'` with a non-string,
  on `ends_at<=starts_at`, on a 6-field cron. Run → FAIL.
- [ ] **Step 4: Implement.** Append `(5, include_str!("../migrations/0005_maintenance_windows.sql"))` to
  `MIGRATIONS`. `models.rs`: add `MaintenanceWindow` (`#[derive(sqlx::FromRow, Serialize)]`) + the two
  DTOs + `default_suppress()`. `validate_window_dto(dto) -> Result<(), String>`: `name` non-empty;
  `scope ∈ {all,tag,monitors}`; target_ref shape per scope (`monitors`→`Value::Array` of ints, non-empty;
  `tag`→`Value::String` non-empty; `all`→ignored); `ends_at > starts_at`; a non-null `recurrence` splits
  into EXACTLY 5 whitespace fields AND `croner::Cron::new(expr).parse()` succeeds (reject seconds/macros).
- [ ] **Step 5: Run → PASS** + `cargo test -p vigil -- --test-threads=1` + clippy. **Step 6: Commit**
  `git commit -am "feat: migration 0005 maintenance_windows + model + DTOs + croner + validation"`

---

## Task 2: `maintenance_windows/resolve.rs` — the pure core

**Files:** Create `crates/vigil/src/maintenance_windows/mod.rs`, `.../resolve.rs`; Modify `src/lib.rs`
(`pub mod maintenance_windows;`). Test: inline `#[cfg(test)]` in resolve.rs.

**Interfaces produced:** all the resolve fns (see Shared Types) **AND `pub async fn active_windows(pool:
&sqlx::SqlitePool) -> Vec<MaintenanceWindow>`** (in `mod.rs`: `sqlx::query_as("SELECT * FROM
maintenance_windows WHERE is_active = 1").fetch_all(pool).await.unwrap_or_default()`) — Tasks 3/4/5 all
consume it, so it MUST be created here (GAP A).

**croner 2.x API (pin — the plan snippets below use these exact shapes):** `croner::Cron::new(expr)
.parse()? -> Cron`; `cron.find_next_occurrence(&dt, inclusive: bool) -> Result<DateTime<Tz>, CronError>`
(takes a **reference**, returns a **Result**). Epoch↔chrono: `chrono::DateTime::<chrono::Utc>::
from_timestamp(t, 0)` returns an **`Option`** (handle `None` → treat as "no occurrence", never
`unwrap`); `.timestamp()` back to i64.

- [ ] **Step 1: Failing tests** (inline) — the §9 pure-resolve cases: `window_active_at` one-off
  before/during/after + cron active-in-occurrence / inactive-between / inactive-before-starts_at +
  dur>period overlap; `occurrences_overlapping` **emits an occurrence starting before the range that
  extends into it** (the `from-dur` back-up — a `0 * * * *` hourly cron, dur 2h, range [02:30,05:00]
  MUST include the 02:00 occurrence's [02:30,03:00] slice) + clipping; `monitor_in_scope` all/tag/
  monitors match+no-match + malformed/empty target_ref→false; `maintenance_for` strongest-wins;
  `subtract_intervals` partial/full/none/multiple cuts; `parse_tags`. Run → FAIL.
- [ ] **Step 2: Implement.** `parse_tags`: `serde_json::from_str::<Vec<String>>(raw).unwrap_or_default()`.
  `window_active_at`: one-off `starts_at<=now<=ends_at`; cron — `dur=ends_at-starts_at`, scan from
  `anchor=max(starts_at, now-dur)`, iterate `cron.find_next_occurrence(&dt, /*inclusive=*/true)` on the
  FIRST call (**inclusive=true so an occurrence starting exactly at the anchor is not skipped** — else a
  window active at its occurrence start reports false-inactive), then advance `t = s + 1s` (inclusive
  false or +1s) each step, collecting the last start `s<=now`; active iff `s>=starts_at && now<s+dur`.
  Convert epochs via `from_timestamp(t,0)` (bail to `false` on `None`); handle the `Result` from
  `find_next_occurrence` (`Ok` continue, `Err`/None-return → stop). **Cap the scan** the same as
  `occurrences_overlapping` (below) with a `tracing::warn!` on hit — it runs on every alert/probe. `monitor_in_scope`: match
  on `scope` — `all`→true; `tag`→`parse target_ref as JSON String`, `tags.contains`; `monitors`→`parse
  target_ref as Vec<i64>`, `.contains(&monitor_id)`; parse failure→false. `maintenance_for`: fold over
  `windows.iter().filter(|w| w.is_active && window_active_at(w,now) && monitor_in_scope(w,id,tags))`,
  return `Checks` if any `suppress=="checks"` else `Alerts` if any else `None`. `occurrences_overlapping`:
  one-off→`[(starts_at,ends_at)]` if overlaps `[from,to]`; cron→scan from `from-dur`, emit `[(s,s+dur)]`
  while `s<=to`, **cap `max(10_000,(to-from)/60+2)`**, `tracing::warn!` if hit. `maintenance_intervals`:
  union+merge (sort by start, coalesce overlaps) of in-scope active windows' occurrences clipped to
  `[from,to]`. `subtract_intervals`: standard base-minus-cuts (sorted cuts).
- [ ] **Step 3: Run → PASS** + suite + clippy + no aws-lc. **Step 4: Commit** `git commit -am "feat: maintenance_windows resolve core (scope + cron/interval math, forward-scan)"`

---

## Task 3: `uptime::compute` maintenance param + stats/bars callers

**Files:** Modify `crates/vigil/src/uptime.rs` (compute + its in-file unit tests at uptime.rs:70/77/83/90),
`src/api/monitors.rs` (stats, bars), **`src/rollup.rs` (the `uptime::compute(&spans, ds, de, true)` call
at rollup.rs:132 — a THIRD production caller; add the arg here too, GAP B)**. Test:
`tests/maintenance_uptime.rs`, extend uptime unit tests.

**Interfaces:** `compute(.., maintenance:&[(Ts,Ts)])`.

- [ ] **Step 1: Failing tests** — uptime unit: downtime fully inside a maintenance interval → 100%
  (`downtime_seconds` excluded); partial overlap → only the outside part; whole `[window_start,now]` is
  maintenance → `uptime_pct:None`. Integration `maintenance_uptime.rs`: a monitor with a resolved
  incident (1h) that FULLY overlaps an active one-off window → `GET /stats?range=24h` → `downtime_seconds
  ~0` and `uptime_pct ~100`. Run → FAIL.
- [ ] **Step 2: Implement.** `compute` gains `maintenance:&[(Ts,Ts)]`. **First, inside compute, clip
  each maintenance interval to `[window_start,now]` and MERGE into disjoint intervals** (so the
  denominator subtraction stays correct even if a caller passes an unmerged set — S4). `eff_denom =
  (now-window_start) - sum(len of merged maintenance)`; per down-span clipped to the window,
  `subtract_intervals(span, merged)` then sum gives `eff_downtime`; if `eff_denom<=0` then `{None,0}`;
  else `uptime_pct = round2(100*(1 - eff_downtime/eff_denom))`. **Update EVERY caller to pass `&[]`** —
  grep `uptime::compute(` finds the in-file unit tests at **uptime.rs:70/77/83/90**, the 90-day-bar
  per-day call, and **rollup.rs:132** (all `&[]`) EXCEPT:
  `stats` (monitors.rs:~599-626) fetches the monitor `tags` + `active_windows(&db)` and passes
  `maintenance_intervals(id, tags, &windows, window_start, now)`; the `bars` builder (monitors.rs:~767-)
  fetches tags+windows once and passes per-day `maintenance_intervals(id,tags,&windows, day_start,
  clipped_end)`.
- [ ] **Step 3: Run → PASS** + suite + clippy. **Step 4: Commit** `git commit -am "feat: uptime excludes maintenance intervals (live stats + bars)"`

---

## Task 4: Alert suppression (deliver) + check suppression (worker + reaper)

**Files:** Modify `crates/vigil/src/notify/dispatch.rs` (deliver), `src/worker.rs`, `src/heartbeat.rs`
(reap_once). Test: `tests/maintenance_suppression.rs`.

**Interfaces:** consumes `maintenance_for`, `active_windows`, `next_run_with_jitter`.

- [ ] **Step 1: Failing tests** — `maintenance_suppression.rs` (`common::test_state`):
  - `alert_suppressed_but_incident_opens`: monitor in an active `alerts` window + a channel on `down`;
    `engine::apply_result` DOWN; assert `env.sent` recorded 0 AND an incident row opened.
  - `checks_window_skips_probe_and_advances_next_run`: monitor in an active `checks` window; capture its
    `next_run_at`; `worker::run_check`; assert NO `checks` row, status unchanged, AND `next_run_at`
    advanced (not equal to the stale value / roughly now+interval — no hot-loop).
  - `checks_window_skips_reaper_but_alerts_window_reaps`: an overdue heartbeat in a `checks` window →
    `reap_once` → NO incident; the same overdue heartbeat in an `alerts` window → `reap_once` → incident
    opens (but a channel on `heartbeat_missed` records 0 sends via deliver's suppression).
  Run → FAIL.
- [ ] **Step 2: Implement.**
  - `deliver` (dispatch.rs:142, top): `let windows = maintenance_windows::active_windows(&state.db).await;
    if maintenance_windows::resolve::maintenance_for(m.id, &parse_tags(&m.tags), &windows, now).is_some()
    { tracing::debug!(monitor_id=m.id, "alert suppressed by maintenance window"); return Ok(()); }`.
  - `worker::run_check` (after the heartbeat guard, ~worker.rs:60): **`let now = now();`** FIRST (the
    existing `let now = now()` isn't bound until ~worker.rs:69, so at this insertion point `now` would
    resolve to the imported fn item — bind it locally here), then load active windows; `if let
    Some(Suppress::Checks) = maintenance_for(m.id, &parse_tags(&m.tags), &windows, now) { let next =
    scheduler::next_run_with_jitter(now, m.interval_seconds); if let Err(e) = sqlx::query("UPDATE
    monitors SET next_run_at=? WHERE id=?").bind(next).bind(m.id).execute(&state.db).await {
    tracing::error!(monitor_id=m.id, error=%e, "maintenance checks-window: failed to advance next_run_at");
    } signal_complete(state, monitor_id); return; }`. (Log, don't `.ok()`-swallow — a silent write
    failure here would leave the stale `next_run_at` and re-introduce the hot-loop — S2.)
  - `heartbeat::reap_once`: after the due-SELECT (change `let due` at heartbeat.rs:182 to **`let mut
    due`** — it's immutable today), load active windows once; filter — `due.retain(|m|
    !matches!(maintenance_for(m.id, &parse_tags(&m.tags), &windows, now), Some(Suppress::Checks)))`
    (Checks-ONLY) — BEFORE calling `reap_one` (OUTSIDE the `BEGIN IMMEDIATE` tx).
- [ ] **Step 3: Run → PASS** + suite + clippy. **Step 4: Commit** `git commit -am "feat: maintenance suppression — alerts in deliver(), checks in worker (advance next_run) + heartbeat reaper (checks-only)"`

---

## Task 5: `MaintenanceChanged` event + `maintenance_ids` snapshot + evaluator task

**Files:** Modify `crates/vigil/src/events.rs` (Event), `src/api/sse.rs` (build_snapshot),
`src/maintenance_windows/mod.rs` (run + a `monitors_in_maintenance` helper), `src/main.rs` (spawn),
`src/settings_store.rs` (`maintenance_tick_seconds`). Test: `tests/maintenance_evaluator.rs`.

- [ ] **Step 1: Failing test** — `maintenance_evaluator.rs`: seed a monitor + an active-now one-off
  window over it; call one evaluator pass (`maintenance_windows::eval_once(&state, &mut prev_set)`);
  assert a `MaintenanceChanged{id, in_maintenance:true}` was published on the bus; deactivate the window;
  next pass → `MaintenanceChanged{id, in_maintenance:false}`. Also: `build_snapshot` includes the
  monitor's id in `maintenance_ids`. Run → FAIL.
- [ ] **Step 2: Implement.** `events.rs`: add `MaintenanceChanged { id:i64, in_maintenance:bool }` (serde
  tag `maintenance_changed`) and `maintenance_ids: Vec<i64>` to `Snapshot`. `maintenance_windows`:
  **`pub async fn monitors_in_maintenance(pool) -> Vec<i64>`** (load active windows + `SELECT id, tags
  FROM monitors`, return ids where `maintenance_for(..).is_some()`); **`pub async fn eval_once(state:
  &AppState, prev:&mut std::collections::HashSet<i64>)`** (both `pub` — `tests/maintenance_evaluator.rs`
  calls `eval_once` directly as an integration test) computes the current set, diffs vs `prev`, emits
  `let _ = state.bus.send(Event::MaintenanceChanged{ id, in_maintenance })` per entered/exited (the
  `let _` matters — a tokio broadcast `send` returns `Err` when zero SSE clients are connected, which is
  the common case — S9), updates `prev`; `run(state)` loops `sleep(maintenance_tick_seconds)` with a
  task-local `HashSet` calling `eval_once`. `sse::build_snapshot`:
  `let maintenance_ids = maintenance_windows::monitors_in_maintenance(&state.db).await;` into the Snapshot.
  `settings_store::maintenance_tick_seconds` (key `maintenance.tick_seconds`, default 30). `main::serve`:
  `tokio::spawn(vigil::maintenance_windows::run(state.clone()));`.
- [ ] **Step 3: Run → PASS** + suite + clippy + no aws-lc. **Step 4: Commit** `git commit -am "feat: MaintenanceChanged SSE + snapshot maintenance_ids + evaluator task"`

---

## Task 6: API — CRUD + POST preview

**Files:** Modify `crates/vigil/src/api/maintenance.rs` (handlers), `src/api/mod.rs` (routes). Test:
`tests/api_maintenance.rs`.

- [ ] **Step 1: Failing test** — `api_maintenance.rs` (app router): POST create (valid → 200 + row);
  each 422 case (bad scope / monitors-with-empty-array / tag-with-non-string / ends<=starts / 6-field
  cron); GET list; PUT update (toggle is_active); POST `/maintenance-windows/preview` with
  `{scope:"tag", target_ref:"prod"}` → `affected_monitor_ids` = the monitors tagged prod + `active_now`;
  DELETE. Run → FAIL.
- [ ] **Step 2: Implement.** `list` (`SELECT * ORDER BY id`), `create` (validate → `serde_json::to_string`
  the target_ref → INSERT → return row). `update`: **merge-then-validate** — fetch the existing row,
  apply the `Option` DTO fields over it, `validate_window_dto` the MERGED window, then UPDATE (S10 — the
  validator needs a complete window; a partial PATCH validated alone would reject/misfire). `delete`.
  `preview` (POST, body `{scope, target_ref, recurrence?, starts_at?, ends_at?}`): `SELECT id, tags FROM
  monitors`, return `{ affected_monitor_ids: monitors where monitor_in_scope(scope,target_ref,..),
  active_now: <bool> }` where `active_now` is `window_active_at` of a transient window **only when both
  `starts_at` and `ends_at` are supplied** (a create-form has them); if either is omitted, return
  `active_now: null` (undefined without a duration — the missed-gap note). Register routes in
  `api/mod.rs` (**`put` is NOT in the `use axum::routing::{get, post}` import at mod.rs:15 — fully-
  qualify it like the channels route does**): `.route("/maintenance-windows",
  get(maintenance::list).post(maintenance::create))`, `.route("/maintenance-windows/preview",
  post(maintenance::preview))`, `.route("/maintenance-windows/:id",
  axum::routing::put(maintenance::update).delete(maintenance::delete))`.
- [ ] **Step 3: Run → PASS** + suite + clippy. **Step 4: Commit** `git commit -am "feat: maintenance-windows CRUD + body-driven preview API"`

---

## Task 7: Frontend — Maintenance screen + maintenanceIds overlay + Rail/TopBar

**Files:** Create `web/src/components/Maintenance.tsx`, `web/src/maintenance_ids.ts` (the module-level
signal); Modify `web/src/api.ts`, `web/src/store.ts`, `web/src/components/Rail.tsx`,
`web/src/components/TopBar.tsx`, `web/src/components/MonitorCard.tsx`, `web/src/components/ListView.tsx`,
`web/src/components/DetailPanel.tsx`, `web/src/App.tsx` (view type + routing + filter), `web/src/theme.css`.
Tests: `web/src/__tests__/maintenance.test.tsx`, extend `store.test.ts`.

- [ ] **Step 1: Failing tests** — `maintenance.test.tsx`: the screen renders a window list + a create
  form; the scope picker toggles tag/monitors inputs; the one-off↔recurring toggle swaps datetime vs a
  cron field; saving calls `createMaintenanceWindow` with the right DTO. `store.test.ts`: a `snapshot`
  frame with `maintenance_ids:[1]` REPLACES the set (a resync snapshot must reset, not accumulate — S6);
  a `maintenance_changed{id:1,in_maintenance:false}` frame removes it; the PURE
  `displayStatus(monitor, ids)` returns `"maintenance"` for an in-set monitor and `"paused"` for a
  paused one even if in-set (precedence). Run → FAIL.
- [ ] **Step 2: Implement.**
  - **`web/src/maintenance_ids.ts` (MODULE-LEVEL, so leaf components can read it without the store
    instance — M7):** `const [ids, setIds] = createSignal<Set<number>>(new Set());
    export const inMaintenance = (id:number) => ids().has(id); export function setMaintenanceIds(list:
    number[]) { setIds(new Set(list)); } export function patchMaintenance(id:number, on:boolean) { ... };
    export function displayStatus(m:{id:number,is_paused?:boolean,status:string}) { return m.is_paused ?
    "paused" : (inMaintenance(m.id) ? "maintenance" : m.status); }` (PURE-ish — `displayStatus` reads the
    module signal; the store.test asserts a param-taking variant `displayStatusWith(m, idsSet)` for pure
    testability, and `displayStatus` calls it with `ids()`).
  - `store.ts` SSE handler: on `snapshot` → `setMaintenanceIds(frame.data.maintenance_ids ?? [])`
    (REPLACE); on `maintenance_changed` → `patchMaintenance(frame.data.id, frame.data.in_maintenance)`.
  - `MonitorCard.tsx` (local `statusClass` at :21), `ListView.tsx` (:46), `DetailPanel.tsx`: render the
    pill+dot from `displayStatus(monitor)` (import from `maintenance_ids.ts`) instead of
    `monitor.status`.
  - `App.tsx`: **add `"maintenance"` to the `RailView` type (Rail.tsx:3) and the `view` signal; add a
    `<Match when={view()==="maintenance"}><Maintenance/></Match>` branch; fix the nav mapping at
    App.tsx:70 so `"maintenance"` is not collapsed to `"dashboard"` (M8).** In `filtered()` (App.tsx:25-34)
    **special-case the maintenance chip: `status === "maintenance" ? inMaintenance(m.id) : m.status ===
    status`** — since `monitor.status` is never `"maintenance"`, a plain equality chip would filter to
    zero (M9).
  - `Rail.tsx`: the summary (Rail.tsx:25-35) adds a maintenance tally using `inMaintenance(m.id)` with
    the same `is_paused > maintenance > real` precedence (import `inMaintenance` — no prop-drilling
    needed since it's module-level — S7); add a "Maintenance" nav entry.
  - `TopBar.tsx`: add `"maintenance"` to `STATUS_CHIPS` (TopBar.tsx:15).
  - `Maintenance.tsx`: list + create form (scope picker, one-off/recurring toggle with a cron field + a
    **local-time** next-fire preview, suppress radio, live "affects N" via `previewMaintenanceWindow`).
  - `api.ts`: the 5 CRUD/preview fns. `theme.css`: **add `.status-dot.maintenance { background:
    var(--maintenance) }`** (the grid/list dots have no maintenance rule today — M7 dot half).
- [ ] **Step 3: Run → PASS** + `npx tsc --noEmit` + `npx vite build`. **Step 4: Commit** `git commit -am "feat(web): maintenance screen + maintenanceIds overlay (pill/dot precedence) + Rail/TopBar + CSS"`

---

## Task 8: Acceptance + final review

**Files:** Create `docs/superpowers/plans/P4.2-acceptance.md`. No product code unless a DoD item fails.

- [ ] **Step 1** — `0005` on a real P1-P4.1 DB copy: version=5, data preserved.
- [ ] **Step 2 (live via Docker)** — create a one-off `alerts` window (scope=monitors, starts now, ends
  +1h) over a monitor; confirm `GET /events` snapshot lists it in `maintenance_ids`, the card shows
  MAINTENANCE; force the monitor DOWN (a bad-url test monitor) → NO alert delivered (check
  notification_log empty for it) + the incident opened + `stats` shows the downtime excluded.
- [ ] **Step 3** — a `checks` window over a monitor → its `next_run_at` advances and it stops probing
  (no new `checks` rows), CPU stays idle (no hot-loop); delete the window → probing resumes.
- [ ] **Step 4** — a cron window (`* * * * *` every-minute, dur 30s or a near-future minute) → flips to
  MAINTENANCE during the occurrence, reverts after; the form's local-time preview matches.
- [ ] **Step 5** — Docker rebuild → healthy on 8099; `cargo tree | grep -iE 'aws-lc|openssl'` empty;
  full `cargo test -p vigil -- --test-threads=1` + `vitest` green.
- [ ] **Step 6: Commit** the acceptance doc. Then final whole-branch review (opus) + merge.

---

## Definition of Done
Windows creatable (one-off + cron, all/tag/monitors scope); alerts suppressed + checks paused (with
`next_run_at` advanced) during active windows; uptime excludes maintenance on live stats/bars; the
MAINTENANCE overlay shows live via `maintenance_ids`/`MaintenanceChanged` with paused>maintenance>real
precedence and no flicker; the Maintenance UI (reachable via the Rail) works; `cargo test` + `vitest`
green; `0005` on a P4.1 DB; no aws-lc/openssl; Docker healthy; every task committed.

**Known v1 boundaries (documented, not silent):** (1) uptime exclusion is computed from the CURRENT
`is_active=1` window set — toggling a window inactive or deleting it stops excluding its past downtime,
so historical uptime % can shift retroactively when an operator edits a window; and it excludes nothing
for a window that was active during an incident but is inactive now. (2) The durable
`check_aggregates_daily` rollups are not retroactively rewritten (§5.4). Add a Task-2/Task-5 **invariant
test** asserting no code path ever writes `"maintenance"` into the `monitors.status` column (O2).
