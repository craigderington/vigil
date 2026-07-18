# Vigil P4.1 — Heartbeat / Push Monitors — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: superpowers:subagent-driven-development. Steps use `- [ ]`.
> **Autonomous build.** Builds on P1+P2+P3 (on `master`). Base: current `master` HEAD (`f6b6037`).
> Branch `feat/p4-heartbeat`.

**Goal:** Add the inverse "heartbeat/push" monitor — a job checks in at a unique `/ping/:token` URL; if
it doesn't within `interval + grace`, the monitor goes DOWN and fires `heartbeat_missed`; the next ping
recovers it.

**Architecture:** A new `heartbeat` monitor type driven NOT by the probe scheduler but by (a) an inbound
`GET|POST /ping/:token` receiver and (b) a slow reaper task, both reusing the existing incident /
notification / SSE machinery via race-safe atomic `UPDATE ... WHERE status=<expected>` gates. A new
`heartbeat.rs` module holds the heartbeat-specific logic; `engine.rs` gets a small refactor (anchor
param + two reusable notify helpers + fleet-UNKNOWN exclusion).

**Tech stack:** Rust (tokio/axum/sqlx-sqlite, rustls) + SolidJS/TS. No new crates (`rand` 0.8 already a
direct dep).

Spec: [`docs/superpowers/specs/2026-07-18-vigil-p4-heartbeat-monitors-design.md`](../specs/2026-07-18-vigil-p4-heartbeat-monitors-design.md).

## Global Constraints

- Inherit ALL P1–P3 constraints: rustls only (**no openssl, no aws-lc-rs**); i64 epoch seconds; SQLite
  FK/WAL; uptime from incidents; non-root container; app bind 8090 / host 8099; version-ordered
  migration runner; conventional commits; TDD. **After each backend task assert `cargo tree -p vigil
  -e normal,build,dev | grep -iE 'aws-lc|openssl'` is empty.**
- **axum 0.7** — path params use the colon form `/ping/:token` (NOT `{token}`).
- **`rand` 0.8** is already a direct dep (`scheduler.rs:39`) — use it for the token; add no new crate.
- **Heartbeats are NEVER probe-scheduled** (§7.5). **Heartbeats never enter `unknown`** (§7b).
- **Reaper DOWN and ping recovery are race-safe**: the transition decision is atomic with the status
  write via a conditional `UPDATE ... WHERE status = <expected>`; side-effects run only when
  `rows_affected == 1`.
- **`heartbeat_token` is a capability** — returned only from `GET /api/monitors/:id`, NEVER the list;
  never logged in full.
- Never-pinged heartbeats stay `pending` ("waiting"), never auto-DOWN, never alert. The switch arms on
  the first ping.

---

## Shared Types & Interfaces (the DRY backbone)

```rust
// models.rs
pub enum Cause { Timeout, Status, Connection, Dns, Keyword, Ssl, Heartbeat }  // serde lowercase → "heartbeat"
pub enum Trigger { Down, Recovered, SslExpiring, SslInvalid, DomainExpiring, HeartbeatMissed }
// Trigger::HeartbeatMissed as_str → "heartbeat_missed" (snake_case serde already set in P3)

// Monitor + manual FromRow gain (ALL three; runtime state, not all on input DTOs):
//   heartbeat_token: Option<String>          (col heartbeat_token TEXT)
//   heartbeat_grace_seconds: i64             (col heartbeat_grace_seconds INTEGER NOT NULL DEFAULT 60)
//   last_ping_at: Option<i64>                (col last_ping_at INTEGER)
// CreateMonitorDto gains ONLY: heartbeat_grace_seconds: i64  (#[serde(default = "default_grace")] → 60)
// UpdateMonitorDto gains:      heartbeat_grace_seconds: Option<i64>
// test_defaults_monitor(): heartbeat_grace_seconds=60, heartbeat_token=None, last_ping_at=None.

// heartbeat.rs (NEW module; add `pub mod heartbeat;` to lib.rs in alpha order after `events`)
pub fn generate_token() -> String;                       // 32-char [A-Za-z0-9] via rand Alphanumeric
pub async fn record_ping(state: &AppState, m: &Monitor) -> anyhow::Result<()>;  // atomic ping (§5)
pub async fn reap_once(state: &AppState) -> anyhow::Result<()>;                 // one reaper pass (§6)
pub async fn run_reaper(state: AppState);                                        // loop { sleep(tick); reap_once }

// engine.rs — signature change (one prod caller worker::run_check + two test call sites):
pub async fn apply_result(state:&AppState, m:&Monitor, out:&ProbeOutcome, anchor: crate::anchor::Connectivity)
    -> anyhow::Result<ApplyOutcome>;
// engine.rs — two reusable post-commit notify helpers extracted from apply_result, called by
// apply_result AND heartbeat.rs (so the incident/event/dispatch logic lives once):
pub(crate) async fn emit_opened(state:&AppState, m:&Monitor, incident_id:i64, from:Status, to:Status, down_trigger:Trigger);
pub(crate) async fn emit_resolved(state:&AppState, m:&Monitor, incident_id:i64, from:Status, to:Status, duration_seconds:i64);
// down_trigger is chosen by caller: Trigger::HeartbeatMissed for type=="heartbeat", else Trigger::Down.

// settings_store.rs
pub async fn heartbeat_tick_seconds(pool:&SqlitePool) -> i64;  // key "heartbeat.tick_seconds", default 20

// probe/http.rs already exposes `pub const DEFAULT_USER_AGENT` (unrelated P4 hotfix, already on master).
```

**Anchor** (`crate::anchor::Connectivity`): `Online | Offline`, from `state.anchor.current().await`.
**AppState**: `{ db, bus, transport, sched_tx, anchor, http_sender }`.
**SchedCmd** (app.rs): `Upsert(i64) | Remove(i64) | CheckNow(i64) | Complete(i64)`.

---

## Task 1: Migration 0004 + models + enums + exhaustive-match arms (compile foundation)

**Files:** Create `crates/vigil/migrations/0004_heartbeat.sql`; Modify `src/db.rs` (MIGRATIONS),
`src/models.rs` (Cause, Trigger, Monitor+FromRow+DTOs+test_defaults), `src/engine.rs` (Cause arm),
`src/notify/templates.rs` (render + render_alert arms), `src/notify/dispatch.rs` (trigger_status arm).
Test: `tests/migrate4.rs`.

**Interfaces produced:** `Cause::Heartbeat`, `Trigger::HeartbeatMissed`, the 3 new Monitor fields + DTOs.

- [ ] **Step 1: `0004_heartbeat.sql`** — verbatim:
```sql
ALTER TABLE monitors ADD COLUMN heartbeat_token TEXT;
ALTER TABLE monitors ADD COLUMN heartbeat_grace_seconds INTEGER NOT NULL DEFAULT 60;
ALTER TABLE monitors ADD COLUMN last_ping_at INTEGER;
CREATE UNIQUE INDEX idx_monitors_heartbeat_token ON monitors(heartbeat_token) WHERE heartbeat_token IS NOT NULL;
```
- [ ] **Step 2: Failing test** — `tests/migrate4.rs`: fresh DB → `MAX(version)=4`; `SELECT
  heartbeat_token, heartbeat_grace_seconds, last_ping_at FROM monitors` succeeds; a v3-DB upgrade test
  (apply 0001+0002+0003, record versions 1-3, insert a monitor, connect → only 0004 applies, monitor
  preserved, its `heartbeat_grace_seconds` backfilled to 60). Run `cargo test -p vigil --test migrate4`
  → FAIL.
- [ ] **Step 3: Implement.** Append `(4, include_str!("../migrations/0004_heartbeat.sql"))` to
  `MIGRATIONS`. `models.rs`: add `Cause::Heartbeat`; add `Trigger::HeartbeatMissed` (+ `as_str` arm
  `"heartbeat_missed"`); add the 3 Monitor fields + manual FromRow rows (`heartbeat_token`/`last_ping_at`
  via `try_get::<Option<_>>`, `heartbeat_grace_seconds` via `try_get::<i64>`); `CreateMonitorDto` +
  `heartbeat_grace_seconds` with `#[serde(default = "default_grace")]` and a `fn default_grace() -> i64
  { 60 }`; `UpdateMonitorDto` + `heartbeat_grace_seconds: Option<i64>`; extend `test_defaults_monitor`.
  `engine.rs`: add `Some(Cause::Heartbeat) => "heartbeat"` to the cause→str match (~line 97).
  `templates.rs`: add a `Trigger::HeartbeatMissed` arm to **both** `render` (line 20 — subject
  `format!("🔴 {} missed its heartbeat", ctx.monitor_name)`, body reuse the down body) and
  `render_alert` (line 59 — unreachable for heartbeat but must compile; return a generic
  `(format!("Heartbeat: {}", ctx.monitor_name), String::new(), None)`). `dispatch.rs`: add
  `Trigger::HeartbeatMissed => "down"` to `trigger_status` (line 56).
- [ ] **Step 4: Run → PASS** + `cargo test -p vigil` + `cargo clippy --all-targets -- -D warnings` (fix
  every Monitor/DTO construction site the 3 new fields break — there are several across the codebase and
  tests). **Step 5: Commit** `git commit -am "feat: migration 0004 (heartbeat cols) + Cause::Heartbeat + Trigger::HeartbeatMissed"`

---

## Task 2: Validation + token generation + create()/update() persistence + list-token strip

**Files:** Modify `crates/vigil/src/api/monitors.rs` (validate_monitor_dto, create, update, list),
`src/heartbeat.rs` (Create — `generate_token`), `src/lib.rs` (`pub mod heartbeat;`). Test:
`tests/heartbeat_create.rs`.

**Interfaces:** `heartbeat::generate_token() -> String`; a heartbeat monitor is created with a token,
`confirmation_threshold=1`, `recovery_threshold=1`, `status='pending'`, `next_run_at=NULL`; the list
endpoint never returns `heartbeat_token`.

- [ ] **Step 1: Failing tests** — `tests/heartbeat_create.rs` (use `common::test_state` + the app
  router):
  - `create_heartbeat_generates_token_and_forces_thresholds`: POST `/api/monitors`
    `{"name":"cron","type":"heartbeat","interval_seconds":3600,"heartbeat_grace_seconds":120}` → 200;
    the returned row has a non-empty `heartbeat_token` (32 chars, alphanumeric), `status=="pending"`,
    `confirmation_threshold==1`, `recovery_threshold==1`, `heartbeat_grace_seconds==120`.
  - `heartbeat_rejects_ssl_and_domain`: POST with `ssl_check_enabled:true` on a heartbeat → 422.
  - `list_endpoint_hides_heartbeat_token`: after creating a heartbeat, `GET /api/monitors` → the
    heartbeat row's `heartbeat_token` is `null`; `GET /api/monitors/:id` → the token is present.
  - `two_heartbeats_get_distinct_tokens`: create two → tokens differ.
  Run → FAIL.
- [ ] **Step 2: Implement.**
  - `heartbeat.rs`: `pub fn generate_token() -> String { use rand::{distributions::Alphanumeric, Rng};
    rand::thread_rng().sample_iter(&Alphanumeric).take(32).map(char::from).collect() }`.
  - `validate_monitor_dto` (extend signature with `heartbeat_grace_seconds: i64` and `domain_check_enabled:
    bool`; update the create + update call sites): add a `"heartbeat" =>` arm that requires neither
    `url` nor `host`, requires `heartbeat_grace_seconds >= 1` and `interval_seconds >= 30`, and 422s if
    `ssl_check_enabled || domain_check_enabled`. (The existing http/keyword/port/ping/dns/ssl arms
    unchanged.)
  - `create()` (the fixed-column INSERT, ~monitors.rs:156-196): when `dto.r#type == "heartbeat"`,
    generate the token (retry the INSERT on a unique-index error, max ~3 tries) and set
    `confirmation_threshold = 1`, `recovery_threshold = 1`, `status = "pending"`, `next_run_at = NULL`.
    **Grow the INSERT column list + VALUES + bind chain** to include `heartbeat_token`,
    `heartbeat_grace_seconds` (from DTO), `last_ping_at` (NULL). For non-heartbeat types, token stays
    NULL and thresholds come from the DTO as today.
  - `update()` (the fixed-column UPDATE, ~monitors.rs:259-297): add `heartbeat_grace_seconds` to the
    column list + bind (grace editable). When the existing row `type == "heartbeat"`, force
    `confirmation_threshold = 1`, `recovery_threshold = 1` regardless of the DTO.
  - `list()` (the `/api/monitors` handler): after fetching the rows, set `heartbeat_token = None` on each
    before returning (the detail `get_one` handler leaves it intact). Simplest: `for m in &mut rows {
    m.heartbeat_token = None; }`.
- [ ] **Step 3: Run → PASS** + full suite + clippy + no aws-lc. **Step 4: Commit** `git commit -am "feat: heartbeat create/validate — token gen, forced thresholds, grace, list-token strip"`

---

## Task 3: engine.rs — anchor param + reusable notify helpers + fleet-UNKNOWN exclusion + type-trigger

**Files:** Modify `crates/vigil/src/engine.rs`, `src/worker.rs` (call site),
`tests/engine_cycle.rs` (2 call sites). Test: `tests/heartbeat_engine.rs`.

**Interfaces produced:** `apply_result(.., anchor)`; `engine::emit_opened`/`emit_resolved`;
`bulk_set_unknown` excludes heartbeat; type-based down-trigger.

- [ ] **Step 1: Failing tests** — `tests/heartbeat_engine.rs`:
  - `bulk_set_unknown_skips_heartbeats`: seed an `up` http monitor + an `up` heartbeat monitor;
    `engine::bulk_set_unknown(&state)`; assert the http monitor → `unknown`, the heartbeat monitor
    stays `up`.
  - `apply_result_takes_anchor`: a compile-level test — call `apply_result(&state, &m, &out,
    Connectivity::Online)` and assert it still opens an incident on `ok:false` (proves the param
    threaded through; anchor Online ⇒ not routed to Unknown).
  Run → FAIL (signature/behavior).
- [ ] **Step 2: Implement.**
  - `apply_result`: replace the internal `let anchor = state.anchor.current().await;` (engine.rs:34)
    with an `anchor: Connectivity` parameter. Extract the two post-commit branches into
    `pub(crate) async fn emit_opened(state, m, incident_id, from, to, down_trigger: Trigger)` (emits
    `IncidentOpened` + `MonitorTransition` + `MonitorUpdated`, dispatches `down_trigger`) and
    `pub(crate) async fn emit_resolved(state, m, incident_id, from, to, duration_seconds)` (emits
    `IncidentResolved` + `MonitorTransition` + `MonitorUpdated`, dispatches `Trigger::Recovered`).
    `apply_result`'s Opened branch chooses `down_trigger = if m.r#type == "heartbeat" {
    Trigger::HeartbeatMissed } else { Trigger::Down }` and calls `emit_opened`.
  - `worker::run_check`: pass `state.anchor.current().await` into `apply_result`.
  - `tests/engine_cycle.rs:5` and `:57`: pass `vigil::anchor::Connectivity::Online`.
  - `bulk_set_unknown` (engine.rs:203): add `AND type != 'heartbeat'` to BOTH the SELECT (line 211) and
    the UPDATE (line 215).
- [ ] **Step 3: Run → PASS** + full suite + clippy + no aws-lc. **Step 4: Commit** `git commit -am "feat: apply_result anchor param + reusable notify helpers + heartbeat excluded from fleet UNKNOWN + type-based down-trigger"`

---

## Task 4: Scheduler exclusion (heartbeats never probe-scheduled)

**Files:** Modify `crates/vigil/src/scheduler.rs` (catch-up query, `reschedule_from_db`),
`src/api/monitors.rs` (`check_now` guard), `src/worker.rs` (guard). Test: `tests/heartbeat_sched.rs`.

**Interfaces:** a heartbeat monitor is never enqueued/probed by the scheduler or check_now.

- [ ] **Step 1: Failing tests** — `tests/heartbeat_sched.rs`:
  - `heartbeat_not_caught_up_on_start`: insert a heartbeat monitor with `next_run_at=NULL`,
    `status='pending'`; run the scheduler catch-up query directly (or the `run_scheduler` bootstrap) and
    assert the heartbeat id is NOT in the scheduled set (the http monitor IS).
  - `worker_run_check_heartbeat_is_noop`: persist a heartbeat monitor; call `worker::run_check(&state,
    id)`; assert NO `checks` row was written and status is unchanged (`pending`), i.e. it never probed.
  - `check_now_heartbeat_rejected`: `POST /api/monitors/:id/check-now` on a heartbeat → the handler does
    not enqueue it (assert via a no-checks-row + still-pending outcome, or a 4xx — pick one and assert).
  Run → FAIL.
- [ ] **Step 2: Implement.**
  - `scheduler.rs:161` catch-up: `SELECT id, next_run_at FROM monitors WHERE is_paused = 0 AND type != 'heartbeat'`.
  - `reschedule_from_db` (scheduler.rs:125-136): change the query to `SELECT next_run_at, is_paused, type
    FROM monitors WHERE id = ?`; if `type == "heartbeat"` return without scheduling (covers all
    `SchedCmd::Upsert` from create/update/resume, which all route here).
  - `check_now` handler (monitors.rs:335-338): before sending `SchedCmd::CheckNow`, load the monitor's
    type; if `heartbeat`, return `Ok(Json(json!({"ok":false,"error":"heartbeat monitors are driven by
    pings, not manual checks"})))` and do NOT send the command.
  - `worker::run_check`: after loading the monitor, `if m.r#type == "heartbeat" { signal_complete(state,
    monitor_id); return; }` (belt-and-suspenders; before the probe).
- [ ] **Step 3: Run → PASS** + full suite + clippy. **Step 4: Commit** `git commit -am "feat: exclude heartbeat monitors from the probe scheduler (catch-up, reschedule, check_now, worker guard)"`

---

## Task 5: `GET|POST /ping/:token` receiver (atomic recovery gate)

**Files:** Modify `crates/vigil/src/heartbeat.rs` (`record_ping` + the axum handler), `src/app.rs`
(route wiring — a `/ping/:token` placeholder may already exist at app.rs:46). Test: `tests/heartbeat_ping.rs`.

**Interfaces:** `record_ping(state, m)`; the route returns 200 on a known token, 404 on unknown.

- [ ] **Step 1: Failing tests** — `tests/heartbeat_ping.rs` (app router + `common::test_state`):
  - `ping_unknown_token_404`: `GET /ping/doesnotexist` → 404.
  - `ping_updates_last_ping_at`: create a heartbeat; `POST /ping/:token` → 200; the row's `last_ping_at`
    is set (non-null).
  - `ping_recovers_down_heartbeat`: create a heartbeat, drive it DOWN (set status='down' + insert an open
    incident); `POST /ping/:token` → 200; status → `up`, the incident is resolved (`resolved_at` set),
    and a `recovered` notification was delivered (assert via `env.sent`/`sent_http`).
  - `ping_up_heartbeat_no_new_incident`: an `up` heartbeat pinged again → still `up`, no incident opened,
    `last_ping_at` advanced.
  - `ping_paused_heartbeat_stays_paused`: a `paused` heartbeat pinged → `last_ping_at` set, status stays
    `paused`.
  Run → FAIL.
- [ ] **Step 2: Implement.**
  - `record_ping(state, m)`: a single atomic UPDATE that both records the ping and (conditionally)
    recovers: `UPDATE monitors SET last_ping_at = ?1, status = CASE WHEN status IN ('down','pending')
    THEN 'up' ELSE status END, updated_at = ?1 WHERE id = ?2 RETURNING (SELECT status FROM monitors
    WHERE id = ?2)` — or read the pre-status inside a transaction. Determine `was_down_or_pending`. If
    so: close the open incident (`UPDATE incidents SET resolved_at=?, duration_seconds=? WHERE
    monitor_id=? AND resolved_at IS NULL`), then `engine::emit_resolved(state, m, incident_id, from=<old>,
    to=Up, duration)`. Otherwise emit a lightweight `Event::MonitorUpdated { id, status:<current>,
    response_time_ms:None, checked_at:now }` (no transition, no double emit).
  - The axum handler `async fn ping(State(state), Path(token): Path<String>) -> impl IntoResponse`:
    `SELECT * FROM monitors WHERE heartbeat_token = ?` → `StatusCode::NOT_FOUND` if none; else
    `record_ping(&state, &m).await`; return `(StatusCode::OK, "ok")`. **Log `monitor_id`, never the
    token.**
  - `app.rs`: ensure `.route("/ping/:token", get(heartbeat::ping).post(heartbeat::ping))` is registered
    on the main router BEFORE the SPA static-asset fallback (keep the colon form).
- [ ] **Step 3: Run → PASS** + full suite + clippy. **Step 4: Commit** `git commit -am "feat: GET|POST /ping/:token receiver — atomic ping + conditional recover, 404 on unknown"`

---

## Task 6: `heartbeat_reaper` (single-arm due-query, atomic DOWN gate)

**Files:** Modify `crates/vigil/src/heartbeat.rs` (`reap_once`, `run_reaper`), `src/settings_store.rs`
(`heartbeat_tick_seconds`), `src/main.rs` (spawn). Test: `tests/heartbeat_reaper.rs`.

**Interfaces:** `reap_once(state)`, `run_reaper(state)`, `settings_store::heartbeat_tick_seconds`.

- [ ] **Step 1: Failing tests** — `tests/heartbeat_reaper.rs` (`common::test_state`):
  - `overdue_heartbeat_goes_down`: create a heartbeat (interval 60, grace 60), set `status='up'`,
    `last_ping_at = now - 200`; `heartbeat::reap_once(&state)`; assert status → `down`, an incident with
    `cause='heartbeat'` opened, and a `heartbeat_missed` notification delivered (via `env.sent`).
  - `within_grace_not_reaped`: `status='up'`, `last_ping_at = now - 30` (< 60+60); `reap_once`; assert
    still `up`, no incident.
  - `never_pinged_not_reaped`: `status='pending'`, `last_ping_at=NULL`, `created_at = now - 10000`;
    `reap_once`; assert still `pending`, no incident, no alert (the switch is unarmed).
  - `already_down_not_reopened`: `status='down'` + an open incident; `reap_once`; assert still exactly
    one open incident (idempotent).
  - `offline_anchor_still_reaps`: use `common::test_state_offline`; an overdue heartbeat still goes
    `down` (heartbeats aren't anchor-gated).
  Run → FAIL.
- [ ] **Step 2: Implement.**
  - `settings_store::heartbeat_tick_seconds`: `get(pool, "heartbeat.tick_seconds", "20").await.parse().unwrap_or(20)`.
  - `reap_once`: `SELECT * FROM monitors WHERE type='heartbeat' AND is_paused=0 AND status='up' AND
    last_ping_at IS NOT NULL AND ? > last_ping_at + interval_seconds + heartbeat_grace_seconds` (bind
    `now`). For each: atomic DOWN gate `UPDATE monitors SET status='down', updated_at=? WHERE id=? AND
    status='up'`; if `rows_affected == 1`, `INSERT INTO incidents (monitor_id, started_at, cause,
    error_message) VALUES (?, ?, 'heartbeat', 'no ping within interval + grace') RETURNING id`, then
    `engine::emit_opened(state, m, incident_id, from=Up, to=Down, Trigger::HeartbeatMissed)`. Log +
    skip per-monitor errors.
  - `run_reaper(state)`: `loop { let tick = settings_store::heartbeat_tick_seconds(&state.db).await;
    tokio::time::sleep(Duration::from_secs(tick as u64)).await; let _ = reap_once(&state).await; }`.
  - `main::serve`: `tokio::spawn(vigil::heartbeat::run_reaper(state.clone()));`.
- [ ] **Step 3: Run → PASS** + full suite + clippy + no aws-lc. **Step 4: Commit** `git commit -am "feat: heartbeat reaper (single-arm due-query, atomic DOWN gate, anchor-independent, never-pinged safe)"`

---

## Task 7: Stats & 90-day-bar heartbeat special-cases

**Files:** Modify `crates/vigil/src/api/monitors.rs` (`stats`, the bar builder). Test:
`tests/heartbeat_stats.rs`.

**Interfaces:** a DOWN heartbeat reports real downtime; a healthy heartbeat's days render green.

- [ ] **Step 1: Failing tests** — `tests/heartbeat_stats.rs`:
  - `heartbeat_downtime_counts`: create a heartbeat, insert a resolved incident spanning 1h within the
    24h window (and NO `checks` rows); `GET /api/monitors/:id/stats?range=24h` → `uptime_pct` is a real
    number (not null) and `downtime_seconds >= 3600` (proves the `had_any_check` special-case).
  - `heartbeat_bars_have_data`: create a heartbeat with `last_ping_at` set (armed) + a clean day (no
    incident); `GET /api/monitors/:id/bars?days=7` → the armed clean day has `has_data == true` (renders
    green, not muted).
  Run → FAIL.
- [ ] **Step 2: Implement.**
  - `stats` (monitors.rs:499-508): compute `is_heartbeat = m.r#type == "heartbeat"` (load the monitor's
    type; the handler already has the id — one `SELECT type FROM monitors WHERE id=?` or reuse a loaded
    row) and pass `had_any_check || is_heartbeat` into `uptime::compute`.
  - bar builder (monitors.rs:744): for a heartbeat monitor, treat a day as `has_data` when the monitor
    is armed (`last_ping_at IS NOT NULL`) and the day is on/after the first-ping day OR has an incident:
    `let has_data = if is_heartbeat { armed && day >= first_active_day } else { <existing expr> };`
    where `first_active_day` = the local day of `min(created_at, last_ping_at)`. Keep the existing
    expression for non-heartbeats. (Document the rollup limitation inline per spec §11 — heartbeat
    uptime is incident-derived, not rollup-derived.)
- [ ] **Step 3: Run → PASS** + full suite + clippy. **Step 4: Commit** `git commit -am "feat: heartbeat stats/bars — incident-derived uptime (not had_any_check-gated), armed days render green"`

---

## Task 8: Frontend — heartbeat type, ping-URL, HeartbeatCard, Now-strip variant, triggers

**Files:** Modify `web/src/components/MonitorForm.tsx`, `web/src/components/DetailPanel.tsx`,
`web/src/components/MonitorCard.tsx`, `web/src/components/ListView.tsx`, `web/src/api.ts`. Create
`web/src/components/HeartbeatCard.tsx`. Tests: extend `web/src/__tests__/form.test.tsx`, create
`web/src/__tests__/heartbeatcard.test.tsx`.

- [ ] **Step 1: Failing tests:**
  - `form.test.tsx`: selecting type `heartbeat` → URL/host/cert-domain/threshold fields hidden, a
    **grace-seconds** field shown, and the **Test check** button hidden; saving a new heartbeat renders a
    **ping URL** panel containing `/ping/` + the returned token; the notifications section shows a
    `heartbeat_missed` checkbox (checked) and no `down` checkbox; `buildDto` sends
    `type:"heartbeat"`, `heartbeat_grace_seconds`, and NO `ssl_check_enabled`/`domain_check_enabled`.
  - `heartbeatcard.test.tsx`: `getMonitor` returns a heartbeat with `last_ping_at` set → HeartbeatCard
    renders the ping URL + "last ping" relative time + "next expected by"; with `last_ping_at:null` →
    the **"Waiting for first ping"** state.
  Run `cd web && npx vitest run` → FAIL.
- [ ] **Step 2: Implement.**
  - `api.ts`: no new endpoint (reuse `getMonitor`/`createMonitor`); add a `pingUrl(token)` helper
    returning `${window.location.origin}/ping/${token}`.
  - `MonitorForm.tsx`: add `{label:"Heartbeat", value:"heartbeat"}` to `MONITOR_TYPES`. Wrap URL/host/
    method/keyword/DNS/**Certificate&Domain section**/**confirmation+recovery inputs** in `Show
    when={type()!=='heartbeat'}`. Add a `heartbeat_grace_seconds` numeric input shown `when={type()===
    'heartbeat'}` (signal `graceSeconds`, default 60). Hide the **Test check** button for heartbeat.
    `buildDto`: for heartbeat, include `heartbeat_grace_seconds`, and do NOT append
    `ssl_check_enabled`/`domain_check_enabled`. On successful save of a NEW heartbeat, show a panel with
    the copyable ping URL (`pingUrl(saved.heartbeat_token)`), a copy button, and `curl -fsS <url>`. In
    the notifications `NotifRow`, for a heartbeat monitor default the row to `{attached, heartbeat_missed:
    true, recovered:true}` and render a `heartbeat_missed` checkbox instead of `down` (extend `NotifRow`
    with `heartbeat_missed:boolean`, `selectedNotifications` emits `"heartbeat_missed"`; strings must
    match the backend exactly).
  - `HeartbeatCard.tsx` (new): props `{monitor}`; shows the ping URL (copy), **last ping** (relative +
    absolute), **next expected by** (`last_ping_at + interval_seconds + heartbeat_grace_seconds`, or
    "—"), and a **"Waiting for first ping"** state when `last_ping_at == null`.
  - `DetailPanel.tsx`: mount `<HeartbeatCard>` gated `type==='heartbeat'`; give the **Now strip** a
    heartbeat variant — replace the response-time + last-checked tiles with **last-ping** +
    **next-expected-by** when `type==='heartbeat'`; hide the **Check now** action for heartbeat.
  - `MonitorCard.tsx` / `ListView.tsx`: for heartbeat monitors, render **last ping** ("2m ago") in place
    of the response-time/sparkline slot.
- [ ] **Step 3: Run → PASS** + `npx tsc --noEmit` + `npx vite build`. **Step 4: Commit** `git commit -am "feat(web): heartbeat monitor type — form + ping URL + HeartbeatCard + Now-strip/list last-ping + heartbeat_missed trigger"`

---

## Task 9: Acceptance + final review

**Files:** Create `docs/superpowers/plans/P4.1-acceptance.md`. No product code unless a DoD item fails.

- [ ] **Step 1** — `0004` on a real P1/P2/P3 DB copy: version=4, data preserved.
- [ ] **Step 2 (live via Docker)** — create a heartbeat monitor (interval 60, grace 30); `GET
  /api/monitors` shows its token as `null`, `GET /api/monitors/:id` shows the token; `POST
  /ping/:token` → 200, `last_ping_at` set, status → `up`.
- [ ] **Step 3 (live)** — stop pinging; after `interval+grace+tick`, the reaper drives it `down` with a
  `heartbeat` incident; ping again → recovers to `up` (incident resolved). Watch the SSE/detail update.
- [ ] **Step 4** — a heartbeat with `ssl_check_enabled:true` → 422; `check-now` on a heartbeat → rejected/no-op.
- [ ] **Step 5** — Docker rebuild → healthy on 8099; `cargo tree | grep -iE 'aws-lc|openssl'` empty;
  full `cargo test -p vigil` + `vitest` green.
- [ ] **Step 6: Commit** the acceptance doc. Then final whole-branch review (opus) + merge.

---

## Definition of Done
Heartbeat monitor type creatable with a token; `/ping/:token` records + recovers; the reaper drives
overdue heartbeats DOWN and fires `heartbeat_missed`; never-pinged stays waiting; heartbeats are never
probe-scheduled and never `unknown`; the list endpoint hides the token; stats/bars show
incident-derived uptime; the UI has the form + card + last-ping rendering; `cargo test` + `vitest`
green; `0004` on a P1/P2/P3 DB; no aws-lc/openssl; Docker healthy; every task committed.
