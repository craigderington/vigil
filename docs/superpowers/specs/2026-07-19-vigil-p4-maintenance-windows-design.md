# Vigil P4.2 — Maintenance Windows — Design Spec (v2, hardened from adversarial review)

> Second sub-project of P4 (Complete). Schedule planned work so alerts are suppressed and uptime %
> isn't dinged during known downtime. One-off or recurring (cron) windows, scoped to all / a tag / a
> set of monitors, suppressing either alerts (keep probing — preserves uptime data) or checks (pause
> probing). Per CLAUDE.md §8.

Builds on P1–P3 + P4.1 (on `master`). Single Rust/axum binary + SolidJS SPA, SQLite (WAL) on a mounted
volume, Docker (host 8099 → container 8090), rustls-only, uptime derived from incidents.

**Naming:** the existing `crates/vigil/src/maintenance.rs` is the nightly **DB** rollup/prune/vacuum
job — UNRELATED. This feature lives in a new `crates/vigil/src/maintenance_windows/` module and
`crates/vigil/src/api/maintenance.rs`. Do not touch `maintenance.rs`.

---

## 1. Goal & scope

A window has a name, a **scope** (all / a tag / a set of monitor ids), a **schedule** (one-off
`[starts_at, ends_at]` or recurring via a cron expression), and a **suppress mode** (`alerts` or
`checks`). While a window is active and a monitor is in scope, that monitor is *in maintenance*: its
alerts are suppressed, its uptime denominator excludes the maintenance time, and the UI shows a
`MAINTENANCE` overlay. `checks` additionally pauses probing.

**In scope:** the `maintenance_windows` table + CRUD/preview API; the pure resolve logic (scope +
one-off/cron matching); the three effects (alert suppression, check suppression, uptime exclusion on
the **live incident-derived stats/bars**, not a retroactive rollup rewrite); an evaluator task that
drives the live `MAINTENANCE` display via a dedicated SSE signal; the Maintenance UI. **Out of scope:**
throttling/digest, reports, backup, theming (later P4 sub-projects).

---

## 2. Data model — migration `0005_maintenance_windows.sql`

```sql
CREATE TABLE maintenance_windows (
  id          INTEGER PRIMARY KEY,
  name        TEXT NOT NULL,
  scope       TEXT NOT NULL DEFAULT 'all',       -- all | tag | monitors
  target_ref  TEXT,                              -- JSON string: NULL (all) | "\"prod\"" (tag) | "[1,2,3]" (monitors)
  starts_at   INTEGER NOT NULL,                  -- epoch; one-off start, or the >= lower bound + duration anchor for cron
  ends_at     INTEGER NOT NULL,                  -- epoch; one-off end; for cron, only ends_at - starts_at (the duration) matters
  recurrence  TEXT,                              -- NULL (one-off) | a 5-field cron expression (UTC)
  suppress    TEXT NOT NULL DEFAULT 'alerts',    -- alerts | checks
  is_active   INTEGER NOT NULL DEFAULT 1,        -- disable without deleting
  created_at  INTEGER NOT NULL
);
```

- Append `(5, include_str!("../migrations/0005_maintenance_windows.sql"))` to `MIGRATIONS` in `db.rs`.
  Additive (a single `CREATE TABLE`, one statement — no `;`-split hazard). No index (a single-operator
  table with a handful of rows; `is_active` is cardinality-2 — an index is worthless).
- **`target_ref` is a JSON STRING**, written with `serde_json::to_string` and read with `from_str`, so
  writer and reader agree on encoding: `all` → NULL; `tag` → a JSON string (`"\"prod\""`); `monitors`
  → a JSON array of ints (`"[1,2,3]"`). Mismatched encoding = the window silently matches nobody, so
  this contract is load-bearing and tested.
- **Recurring duration = `ends_at - starts_at`**; the cron generates occurrence START times; `starts_at`
  is a `>=` lower bound (the schedule does not fire before it), NOT required to itself be a cron match.
  Validation: `ends_at > starts_at`.

---

## 3. Models — NO `Status::Maintenance` (maintenance is a client-side overlay)

**Decision (review M3/M6/S7):** maintenance is NOT a monitor status. `monitors.status` stays truthful
(pending/up/down/paused/unknown) everywhere on the backend — no overlay is written or emitted onto the
status field. This avoids the fatal flicker where a probe (which keeps running in `alerts` mode) emits
`MonitorUpdated{real status}` and clobbers a status-overlay. The `Status` enum is **unchanged** (no new
variant, no exhaustive-match churn). Instead the backend publishes a separate "who is in maintenance"
signal and the **frontend** renders the `MAINTENANCE` pill from it (§5.1).

- **`MaintenanceWindow`** struct with `#[derive(sqlx::FromRow)]` (the `channels.rs` precedent —
  `is_active: bool` decodes from the INTEGER column). `target_ref: Option<String>` (raw JSON; the
  resolve module parses it). `MaintenanceScope`/`Suppress` are `&str`-compared, not enums, to match the
  existing string-column style.
- **DTOs:** `CreateMaintenanceWindowDto { name, scope, #[serde(default)] target_ref: Option<serde_json::Value>,
  starts_at, ends_at, #[serde(default)] recurrence: Option<String>, #[serde(default = "default_suppress")] suppress }`
  (`serde(default)` so an `all`-scope create with no `target_ref` deserializes). `UpdateMaintenanceWindowDto`
  — all `Option`, including `is_active: Option<bool>`.

---

## 4. Cron — the `croner` crate (forward-only)

Add `croner = "2"` (a pure cron parser; **not** rustls-relevant). **Note (review S2):** croner pulls
`chrono` with default features (incl. `clock`) and `iana-time-zone` — so this repo's chrono is no longer
std-only after this task. That is fine and stays rustls-clean (`iana-time-zone` is pure); the
`cargo tree | grep -iE 'aws-lc|openssl'` gate still passes — assert it.

Cron is evaluated in **UTC** (the app stores UTC epochs). **croner 2.x is FORWARD-ONLY** — there is no
`find_previous_occurrence`. The two primitives (`resolve.rs`, pinned here so the implementer doesn't
reach for a nonexistent method):

- **`window_active_at(w, now)`** (boolean): one-off → `starts_at <= now <= ends_at`. Cron → let
  `dur = ends_at - starts_at`; **bounded forward scan** from `anchor = max(starts_at, now - dur)`:
  iterate `cron.find_next_occurrence(t, inclusive)` collecting the last occurrence start `s <= now`;
  active iff `s >= starts_at && now < s + dur`. Checking only the latest occurrence `<= now` is
  sufficient even when `dur > cron period` (a later occurrence covering `now` would have a start
  `<= now`, so it's the one found) — no overlap-merge needed for the boolean.
- **`occurrences_overlapping(w, from, to)`** (for uptime, §5.4): one-off → the single `[starts_at, ends_at]`
  if it overlaps. Cron → **scan from `from - dur`** (NOT `from` — an occurrence starting before the
  range can still extend into it; failing to back up = under-excluded maintenance = a real outage
  wrongly counted, review M2), iterate `find_next_occurrence` emitting `[s, s + dur]` while `s <= to`,
  stop past `to`. **Cap the scan at `max(10_000, (to - from) / 60 + 2)`** (scales with the range so a
  90-day bars call isn't truncated) and `tracing::warn!` if the cap is hit — never silently truncate
  the interval set (review S1).

**Validation (create/update, review S4/S8):** a non-null `recurrence` must be **exactly 5
whitespace-separated fields** and parse under croner (reject 6/7-field seconds/year forms and
`@`-macros — the contract is 5-field). `ends_at > starts_at`. If `dur > ` the cron's minimum period is
detectable, it's allowed (occurrences may overlap; the boolean/interval logic handles it) but the form
should warn. **UTC foot-gun (review S3):** the create-form preview of "next few fire times" (§8) is
rendered in the operator's **local** time so a US-Eastern user entering `0 2 * * 0` sees it fire at
their local Saturday 21:00/22:00, catching the mistake at create time. (Storing a per-window operator
timezone is a deferred nicety; UTC storage + local-time preview is v1.)

---

## 5. The resolve module (`maintenance_windows/resolve.rs` — pure)

```rust
pub enum Suppress { Alerts, Checks }
pub fn parse_tags(raw: &Option<String>) -> Vec<String>;                            // monitors.tags JSON-array-string → Vec
pub fn window_active_at(w: &MaintenanceWindow, now: Ts) -> bool;                   // §4
pub fn monitor_in_scope(w: &MaintenanceWindow, monitor_id: i64, tags: &[String]) -> bool;
//   all → true; tag → tags contains the parsed target_ref string; monitors → the parsed id-array contains monitor_id;
//   a malformed/empty target_ref → false (never panics)
pub fn maintenance_for(monitor_id: i64, tags: &[String], active_windows: &[MaintenanceWindow], now: Ts) -> Option<Suppress>;
//   over the windows that are is_active AND window_active_at AND monitor_in_scope: strongest wins (Checks > Alerts > None)
pub fn maintenance_intervals(monitor_id: i64, tags: &[String], windows: &[MaintenanceWindow], from: Ts, to: Ts) -> Vec<(Ts,Ts)>;
//   union (merged, sorted, disjoint) of occurrences_overlapping(..) clipped to [from,to] for every in-scope active window
pub fn subtract_intervals(base: (Ts,Ts), cuts: &[(Ts,Ts)]) -> Vec<(Ts,Ts)>;       // base minus a merged cut-set
```
Callers load `is_active=1` windows once (`SELECT * FROM maintenance_windows WHERE is_active = 1`) and
pass them in; the pure functions re-check `window_active_at`/scope, so a caller can pass the full active
set.

### 5.1 Effect A — Maintenance display (client-side, NOT a status overlay)

- **`Event::MaintenanceChanged { id: i64, in_maintenance: bool }`** — a new SSE event (events.rs), serde
  tag `maintenance_changed`. The evaluator (§6) emits it when a monitor enters/exits maintenance.
- **`Event::Snapshot`** gains `maintenance_ids: Vec<i64>` (built in `sse.rs::build_snapshot` by resolving
  the active windows against the current monitors) so a freshly-connected client knows the initial set.
- **Frontend:** the store holds a `maintenanceIds: Set<number>` (seeded from the snapshot, updated on
  `maintenance_changed`). The status pill / dot / list render **`is_paused ? "paused" : (maintenanceIds
  .has(id) ? "maintenance" : statusClass(monitor.status))`** — an explicit precedence: **paused beats
  maintenance beats the real status** (review S6). `monitor.status` stays the truthful up/down value;
  the Rail global-summary and TopBar filter chips read the real status but also surface a maintenance
  count (review S7). No `MonitorUpdated` ever carries "maintenance" — apply_result/heartbeat/
  bulk_set_unknown all keep emitting the real status, and the maintenance overlay is orthogonal, so the
  card never flickers.

### 5.2 Effect B — Alert suppression (`notify::dispatch::deliver`)

At the very top of `deliver()` (dispatch.rs:142), which is the single funnel for ALL alert paths
(on_transition down/recovered, send_alert ssl/domain, the reaper's heartbeat_missed): load the active
windows and compute `maintenance_for(m.id, parse_tags(&m.tags), &windows, now)`. If `Some(_)` (either
suppress kind suppresses alerts), `tracing::debug!` and `return Ok(())` — no channel contacted, nothing
logged to `notification_log`. `deliver` already receives the full `&Monitor` (so `m.tags` is available).
The incident STILL opens (only the alert is muted); its downtime is netted out of uptime by Effect D, so
the operator isn't dinged.

### 5.3 Effect C — Check suppression (`suppress='checks'` only)

- **Probe path (`worker::run_check`):** beside the `is_paused` / heartbeat guards, load active windows +
  compute `maintenance_for(...)`. If `Some(Suppress::Checks)`: **advance `next_run_at` to
  `scheduler::next_run_with_jitter(now, interval_seconds)` and persist it, THEN `signal_complete` and
  return** — do NOT skip the reschedule. **Critical (review M4):** unlike `is_paused`/heartbeat (which
  `reschedule_from_db` special-cases and does not re-heap), a maintenance-suppressed monitor IS re-heaped
  by `reschedule_from_db`; returning without advancing `next_run_at` leaves it at its stale past
  `next_run_at`, so `take_due` fires it immediately and the guard re-suppresses in a tight busy-loop for
  the whole window (a pegged core + WAL hammering). Advancing `next_run_at` makes the next fire land one
  interval out, so a checks-suppressed monitor idles quietly.
- **Heartbeat reaper (`heartbeat::reap_once`):** a Rust-side filter BETWEEN the due-SELECT
  (heartbeat.rs:182) and the per-monitor `reap_one` (heartbeat.rs:192) — compute `maintenance_for` per
  candidate OUTSIDE any transaction and skip it iff `Some(Suppress::Checks)`. **Checks-only (review
  M5):** an `alerts`-window heartbeat MUST still reap (open the incident) — its alert is muted by Effect
  B and its downtime netted by Effect D; wrongly excluding it with a `.is_some()` predicate would write
  no incident, erasing the outage from history AND leaving Effect D nothing to exclude (a false 100%).
  Do NOT push the windows query inside `reap_one`'s `BEGIN IMMEDIATE` transaction (it would extend the
  held write lock, undermining P4.1's atomicity).

Note: Effect C does not attempt to "freeze" the status against `bulk_set_unknown` (the connectivity
reactor may still flip a checks-suppressed monitor to `unknown` in the DB); the maintenance DISPLAY
(§5.1) shows MAINTENANCE regardless, and after the window ends the next probe corrects the status. This
is acceptable and documented (not a silent guarantee).

### 5.4 Effect D — Uptime exclusion (`uptime::compute`)

```rust
pub fn compute(spans: &[Span], window_start: Ts, now: Ts, had_any_check: bool, maintenance: &[(Ts,Ts)]) -> Uptime;
```
- **effective denominator** = `(now - window_start)` minus total maintenance overlap in `[window_start, now]`.
- **effective downtime** = each down-span clipped to `[window_start, now]`, then `subtract_intervals`
  the maintenance cut-set, summed.
- `uptime_pct = Some(1 - effective_downtime/effective_denominator)`; `effective_denominator <= 0` (the
  whole window was maintenance) → `{ uptime_pct: None, downtime_seconds: 0 }`.
- **All existing callers pass `&[]`** (no behavior change) EXCEPT **`stats` (monitors.rs:~599-626)** and
  the **90-day `bars` builder (monitors.rs:~767-)** — both must first fetch the monitor's `tags` +
  `is_active=1` windows and pass `maintenance_intervals(m.id, tags, &windows, window_start, now)` (per
  day for bars). Pin this wiring in the plan (review S5).

Scope note: exclusion is on the **live** stats/bars only; `check_aggregates_daily` rollups are NOT
retroactively rewritten (same v1 boundary as heartbeats — documented).

---

## 6. Evaluator task (`maintenance_windows::run`)

Spawned in `main::serve`. Loop `sleep(tick)` (`maintenance.tick_seconds`, default 30). Each pass:
1. `SELECT * FROM maintenance_windows WHERE is_active = 1`; `SELECT id, tags FROM monitors`.
2. Compute the current in-maintenance set `{id}` via `maintenance_for`.
3. Diff vs the previous tick's set (task-local): **entered** → `MaintenanceChanged{id, in_maintenance:true}`;
   **exited** → `MaintenanceChanged{id, in_maintenance:false}`. No DB writes (pure display).

On app restart the local set is empty, so the first tick emits `in_maintenance:true` for every
currently-in-maintenance monitor — harmless (the snapshot already carried the same set; the frontend Set
is idempotent).

---

## 7. API (`api/maintenance.rs`, under `/api`)

- `GET /api/maintenance-windows` → list all.
- `POST /api/maintenance-windows` → create. Validate (422 otherwise): `name` non-empty; `scope` ∈
  {all,tag,monitors}; **`target_ref` shape matches scope** — `monitors` ⇒ a NON-EMPTY array of integers,
  `tag` ⇒ a non-empty string, `all` ⇒ ignored/forced NULL (review M8/S9); `ends_at > starts_at`; a
  non-null `recurrence` is exactly 5 fields + parses as cron. `target_ref` is stored via
  `serde_json::to_string`. (Dangling monitor ids in a `monitors` window are allowed and skipped at
  resolve time — no FK is possible with JSON storage; documented.)
- `PUT /api/maintenance-windows/:id` → update (same validation; toggles `is_active`).
- `DELETE /api/maintenance-windows/:id`.
- **`POST /api/maintenance-windows/preview`** — **body-driven** (NOT id-keyed, review S11): body
  `{scope, target_ref, recurrence?, starts_at?, ends_at?}` → `{affected_monitor_ids: [...], active_now:
  bool}`, resolving scope against ALL current monitors (incl. tags) so the **create form** can show a
  live "affects N monitors" before the window is saved. `active_now` = `window_active_at(..)` for the
  posted schedule (a create-form has no `is_active` yet → treat as active). A separate
  `GET /api/maintenance-windows/:id/preview` may exist for an existing window, but its `active_now` must
  be `is_active && window_active_at(..)` (review S10).

---

## 8. UI

- **Rail:** add a "Maintenance" nav entry → the Maintenance screen. The Rail global summary
  (Rail.tsx:29-33, currently up/down/paused) and TopBar filter chips (TopBar.tsx STATUS_CHIPS) gain a
  **maintenance** count/chip, reading `maintenanceIds` (review S7).
- **Maintenance screen:** list of windows (name; scope summary; schedule summary — "one-off Mar 3
  02:00–04:00" or "cron `0 2 * * 0` (2h)"; suppress badge; active/disabled toggle) + a **Create window**
  form: name; **scope** picker (All / Tag [tag input] / Monitors [multi-select]); **schedule** (one-off:
  start+end datetime; recurring: a 5-field cron field + duration, with a **local-time preview** of the
  next few fire times); **suppress** radio (Alerts / Checks); a live **"affects N monitors"** via the
  POST preview. Edit / delete / enable-disable per row.
- **Status rendering:** the pill/dot render the maintenance overlay per §5.1's precedence
  (`is_paused ? paused : in_maintenance ? maintenance : status`). **theme.css:** `.status-pill.maintenance`
  already exists; **ADD `.status-dot.maintenance { background: var(--maintenance) }`** — the grid card
  (MonitorCard.tsx) and list view (ListView.tsx) use `.status-dot`, which has no maintenance rule today,
  so a maintenance monitor would show an invisible dot (review M7). The `--maintenance` token
  (`#B58BF5`) already exists in theme.css.
- api.ts: `listMaintenanceWindows`, `createMaintenanceWindow`, `updateMaintenanceWindow`,
  `deleteMaintenanceWindow`, `previewMaintenanceWindow`.

---

## 9. Testing

**Rust — pure resolve (priority; the logic is subtle):**
- `window_active_at`: one-off before/during/after; cron active during an occurrence, inactive between,
  inactive before `starts_at`; a cron with `dur > period` (overlapping occurrences) still correct.
- `occurrences_overlapping`: **an occurrence starting BEFORE the range but extending into it IS emitted**
  (the `from - dur` back-up — the M2 regression); clipping; the runaway cap logs and doesn't truncate a
  reasonable (daily-cron over 90d) set.
- `monitor_in_scope`: all→true; tag match/no-match; monitors match/no-match; malformed/empty target_ref
  → false (no panic). The tag encoding round-trips (`to_string` write ↔ `from_str` read).
- `maintenance_for`: strongest-suppression (checks > alerts > none) over overlapping windows.
- `subtract_intervals` + `compute` with maintenance: downtime fully inside a window → 100% (excluded);
  partial overlap → only the outside part counts; whole window maintenance → `uptime_pct: None`.

**Rust — integration:**
- alert suppression: monitor in an active window + a channel on `down`; `apply_result` DOWN; assert NO
  delivery AND the incident DID open (suppresses the alert, not the incident).
- check suppression: monitor in a `checks` window; `worker::run_check`; assert NO `checks` row, status
  unchanged, AND `next_run_at` was advanced (no hot-loop).
- reaper suppression: an overdue heartbeat in a `checks` window → NOT reaped (no incident); an overdue
  heartbeat in an `alerts` window → IS reaped (incident opens) but no alert delivered.
- migration 0005 (fresh→v5 + table; v4-DB upgrade preserves data).
- API: create valid + each 422 (bad scope/target_ref shape, empty monitors array, ends<=starts, 6-field
  cron); list; POST preview (correct affected ids + active_now); `:id/preview` active_now honors
  is_active; delete.
- evaluator: a window active now over a monitor → one pass emits `MaintenanceChanged{true}`; deactivate →
  next pass emits `{false}`.
- **invariant test:** no code path writes `"maintenance"` to `monitors.status` (review O3).

**Web (vitest):** the Maintenance screen renders the list + form; scope picker toggles tag/monitors
inputs; one-off↔recurring toggle swaps datetime vs cron field; save calls `createMaintenanceWindow` with
the right DTO; a monitor in `maintenanceIds` renders the maintenance pill AND dot; a `paused` monitor
that's also in `maintenanceIds` renders PAUSED (precedence).

---

## 10. Non-functional

- rustls-only preserved. New crate: `croner` (pure) + its transitive `chrono`(clock)/`iana-time-zone`
  (pure) — assert `cargo tree -e normal,build,dev | grep -iE 'aws-lc|openssl'` stays empty. This repo's
  chrono gains the `clock` feature via croner's default features — update any doc that called chrono
  "std-only".
- Migration 0005 additive (new table only); no monitor-row or existing-data changes.
- Single-operator: on-demand active-windows queries in `deliver`/`worker`/`reaper` are cheap (few
  windows). The evaluator is the only periodic cost (one query pair per 30s).
- No writer ever persists `"maintenance"` to `monitors.status` — it isn't a `Status` variant; the real
  status is always authoritative, so disabling/deleting a window instantly reverts the display.

---

## 11. Build phases (for the plan)

1. Migration 0005 + `MaintenanceWindow` model (`#[derive(FromRow)]`) + DTOs + `croner` dep + the
   create/update validation helpers (scope/target_ref shape, 5-field cron, ends>starts). (No `Status`
   change.)
2. `maintenance_windows/resolve.rs` — the pure core (`parse_tags`, `window_active_at` forward-scan,
   `monitor_in_scope`, `maintenance_for`, `occurrences_overlapping` with the `from-dur` back-up + cap,
   `maintenance_intervals`, `subtract_intervals`) with heavy unit tests.
3. `uptime::compute` maintenance param + the `stats`/`bars` callers fetching tags+windows and passing
   intervals.
4. Alert suppression in `deliver()`; check suppression in `worker::run_check` (advance next_run_at!) +
   the heartbeat `reap_once` Checks-only filter.
5. `Event::MaintenanceChanged` + `maintenance_ids` in the Snapshot + the evaluator task + spawn.
6. API (`api/maintenance.rs` CRUD + POST preview) + routes.
7. Frontend: Rail entry + Maintenance screen + create form (local-time cron preview) + the
   `maintenanceIds` store signal + pill/dot precedence + `.status-dot.maintenance` CSS + Rail/TopBar
   maintenance count.
8. Acceptance (live via Docker: a one-off `alerts` window flips a monitor to MAINTENANCE, suppresses its
   down-alert, and excludes its uptime; a `checks` window skips probing without hot-looping; a cron
   window; the token/no-flicker holds) + final review + merge.

---

## 12. Decisions log (resolved from the review)

1. **Maintenance = a client-side display overlay** via `MaintenanceChanged` + snapshot `maintenance_ids`;
   `monitors.status` stays truthful; **no `Status::Maintenance` in Rust** (kills the probe-vs-overlay
   flicker). ✅
2. **Recurring = 5-field UTC cron via `croner` (forward-only)**; `window_active_at`/interval use a
   bounded forward scan (croner has no previous-occurrence); the form preview shows LOCAL fire times. ✅
3. **`suppress='alerts'` default** (keeps probing → preserves uptime data). ✅
4. **Check-suppression advances `next_run_at`** (no scheduler hot-loop); the **reaper filter is
   Checks-only** and outside the atomic gate. ✅
5. **Uptime exclusion on live stats/bars** (not a rollup rewrite); `occurrences_overlapping` backs up by
   `dur`; the cap scales with range and logs. ✅
6. **Display precedence:** paused > maintenance > real status. **Strongest suppression** (checks >
   alerts) wins across overlapping windows. **Recovered alerts also suppressed** during maintenance. ✅
7. **`target_ref` = a serde_json STRING** (to_string/from_str round-trip), per-scope-validated
   (non-empty monitors array / non-empty tag string / null for all). ✅
