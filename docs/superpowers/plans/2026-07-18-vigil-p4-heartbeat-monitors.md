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

// engine.rs — signature change (one prod caller worker::run_check + the run_once test helper):
pub async fn apply_result(state:&AppState, m:&Monitor, out:&ProbeOutcome, anchor: crate::models::Connectivity)
    -> anyhow::Result<ApplyOutcome>;
// engine.rs — two reusable post-commit notify helpers extracted from apply_result, called by
// apply_result AND heartbeat.rs. They emit IncidentOpened/Resolved + MonitorTransition + dispatch
// ONLY — NOT MonitorUpdated (apply_result keeps its single tail MonitorUpdated; heartbeat callers
// emit their own MonitorUpdated{response_time_ms:None} after commit):
pub(crate) async fn emit_opened(state:&AppState, m:&Monitor, incident_id:i64, from:Status, to:Status, down_trigger:Trigger);
pub(crate) async fn emit_resolved(state:&AppState, m:&Monitor, incident_id:i64, from:Status, to:Status, duration_seconds:i64);
// down_trigger is chosen by caller: Trigger::HeartbeatMissed for type=="heartbeat", else Trigger::Down.

// settings_store.rs
pub async fn heartbeat_tick_seconds(pool:&SqlitePool) -> i64;  // key "heartbeat.tick_seconds", default 20

// probe/http.rs already exposes `pub const DEFAULT_USER_AGENT` (unrelated P4 hotfix, already on master).
```

**Anchor** (`crate::models::Connectivity` — it's defined in `models.rs:281`; `anchor.rs` only
`use`s it privately, so `crate::anchor::Connectivity` does NOT resolve): `Online | Offline`, from
`state.anchor.current().await`. Tests: `vigil::models::Connectivity` (or bare `Connectivity` where
`use vigil::models::*;` is in scope, e.g. `engine_cycle.rs:1`).
**AppState**: `{ db, bus, transport, sched_tx, anchor, http_sender }`.
**SchedCmd** (app.rs): `Upsert(i64) | Remove(i64) | CheckNow(i64) | Complete(i64)`.

### Heartbeat transition pattern (the ONE design shared by record_ping + the reaper)

Every heartbeat state change is **one `BEGIN IMMEDIATE` transaction** — status flip + incident
open/close together (mirroring `apply_result`'s single-tx invariant at engine.rs:64-141) — then events
+ dispatch happen **after commit**. `BEGIN IMMEDIATE` takes the write lock up front, so a
read-then-write inside it cannot interleave with the other path (no `SQLITE_BUSY_SNAPSHOT`, no
double-recover). Use it explicitly: `let mut c = state.db.acquire().await?;
sqlx::query("BEGIN IMMEDIATE").execute(&mut *c).await?; … sqlx::query("COMMIT").execute(&mut *c).await?;`.

- **emit helpers do NOT emit `MonitorUpdated`.** `apply_result` already emits exactly one
  `MonitorUpdated` unconditionally at its tail (engine.rs:189-194) carrying `out.response_time_ms`.
  `emit_opened`/`emit_resolved` emit only `IncidentOpened`/`IncidentResolved` + `MonitorTransition` +
  the dispatch. **record_ping and the reaper each emit their OWN `MonitorUpdated{ response_time_ms:
  None }` after commit** (heartbeats have no response time).

### Token is `#[serde(skip_serializing)]` (never leaks via any Monitor serialization)

`Monitor.heartbeat_token` carries `#[serde(skip_serializing)]` so it is absent from **every** full-
Monitor payload — the list (`GET /api/monitors`), `get_one`, `update`, and critically the SSE
`Event::Snapshot { monitors }` (sse.rs:22-28 broadcasts every monitor to every `/events` client on
connect + Lagged resync). The token is fetched only via a dedicated endpoint:
`GET /api/monitors/:id/heartbeat` → `{ "token": "...", "ping_path": "/ping/..." }` (returns 404 for a
non-heartbeat monitor). The frontend calls it for the HeartbeatCard and right after create.

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
  via `try_get::<Option<_>>`, `heartbeat_grace_seconds` via `try_get::<i64>`). **`heartbeat_token`
  carries `#[serde(skip_serializing)]`** so it never leaks via any Monitor payload (list, SSE
  snapshot, update) — Task 2's dedicated endpoint returns it. `CreateMonitorDto` +
  `heartbeat_grace_seconds` with `#[serde(default = "default_grace")]` and a `fn default_grace() -> i64
  { 60 }`; `UpdateMonitorDto` + `heartbeat_grace_seconds: Option<i64>`; extend `test_defaults_monitor`.
  `engine.rs`: add `Some(Cause::Heartbeat) => "heartbeat"` to the cause→str match (~line 97).
  `templates.rs`: `render` (line 20) — add a `Trigger::HeartbeatMissed` arm to the SUBJECT match
  only (`format!("{} missed its heartbeat", ctx.monitor_name)`; the body is built from `ctx` lines
  after the match, so a subject arm suffices). `render_alert` (line 59) also builds its body AFTER
  the subject match, so you **cannot** early-return a full tuple — **fold `Trigger::HeartbeatMissed`
  into `render_alert`'s existing `Down | Recovered` (or catch-all) subject arm** (unreachable for
  heartbeat, but must compile). `dispatch.rs`: add `Trigger::HeartbeatMissed => "down"` to
  `trigger_status` (line 56).
- [ ] **Step 4: Run → PASS** + `cargo test -p vigil` + `cargo clippy --all-targets -- -D warnings` (fix
  every Monitor/DTO construction site the 3 new fields break — there are several across the codebase and
  tests). **Step 5: Commit** `git commit -am "feat: migration 0004 (heartbeat cols) + Cause::Heartbeat + Trigger::HeartbeatMissed"`

---

## Task 2: Validation + token gen + create()/update() + dedicated token endpoint + test_check guard

**Files:** Modify `crates/vigil/src/api/monitors.rs` (validate_monitor_dto, create, update,
test_check, + new `get_heartbeat` handler), `src/api/mod.rs` (route), `src/heartbeat.rs`
(`generate_token`), `src/lib.rs` (`pub mod heartbeat;`). Test: `tests/heartbeat_create.rs`.

**Interfaces:** `heartbeat::generate_token() -> String`; a heartbeat is created with a token,
`confirmation_threshold=1`, `recovery_threshold=1`, `status='pending'`, `next_run_at=NULL`; the token
is NEVER serialized on a Monitor (Task 1 `skip_serializing`) and is fetched only via
`GET /api/monitors/:id/heartbeat`.

- [ ] **Step 1: Failing tests** — `tests/heartbeat_create.rs` (`common::test_state` + app router):
  - `create_heartbeat_forces_thresholds`: POST `/api/monitors`
    `{"name":"cron","type":"heartbeat","interval_seconds":3600,"heartbeat_grace_seconds":120}` → 200;
    the returned row has `status=="pending"`, `confirmation_threshold==1`, `recovery_threshold==1`,
    `heartbeat_grace_seconds==120`, **and NO `heartbeat_token` field in the JSON** (skip_serializing).
  - `heartbeat_token_only_via_dedicated_endpoint`: `GET /api/monitors/:id/heartbeat` → 200 with a
    non-empty 32-char alphanumeric `token` + `ping_path` starting `/ping/`; `GET /api/monitors` (list)
    and `GET /api/monitors/:id` JSON contain NO `heartbeat_token`; `GET
    /api/monitors/:non_heartbeat_id/heartbeat` → 404.
  - `heartbeat_rejects_ssl`: POST `ssl_check_enabled:true` on a heartbeat → 422.
  - `heartbeat_rejects_domain`: POST `domain_check_enabled:true` (ssl false) on a heartbeat → 422
    (exercises the OTHER disjunct — S4).
  - `heartbeat_rejects_short_interval`: POST `interval_seconds:10` on a heartbeat → 422 (the ≥30 floor).
  - `two_heartbeats_get_distinct_tokens`: create two → their `/heartbeat` tokens differ.
  Run → FAIL.
- [ ] **Step 2: Implement.**
  - `heartbeat.rs`: `pub fn generate_token() -> String { use rand::{distributions::Alphanumeric, Rng};
    rand::thread_rng().sample_iter(&Alphanumeric).take(32).map(char::from).collect() }`.
  - `validate_monitor_dto` (extend the signature with `interval_seconds: i64`, `heartbeat_grace_seconds:
    i64`, and `domain_check_enabled: bool`; update BOTH the create and update call sites): add a
    `"heartbeat" =>` arm requiring neither `url` nor `host`, requiring `heartbeat_grace_seconds >= 1`
    and `interval_seconds >= 30`, and 422 if `ssl_check_enabled || domain_check_enabled`. (Existing
    http/keyword/port/ping/dns/ssl arms unchanged.)
  - `create()` (INSERT ~monitors.rs:156-196): for `dto.r#type == "heartbeat"`, generate the token
    (retry INSERT on a unique-index error, ≤3 tries) and set `confirmation_threshold = 1`,
    `recovery_threshold = 1`, `status = "pending"`, `next_run_at = NULL`. **Grow the INSERT column
    list + VALUES + bind chain** to include `heartbeat_token`, `heartbeat_grace_seconds` (from DTO),
    `last_ping_at` (NULL). Non-heartbeat types: token NULL, thresholds from the DTO as today.
  - `update()` (UPDATE ~monitors.rs:259-297): add `heartbeat_grace_seconds` to the column list + bind
    (editable). When the existing row `type == "heartbeat"`, force `confirmation_threshold = 1`,
    `recovery_threshold = 1` regardless of the DTO. (Token is safe in the returned Monitor — Task 1
    `skip_serializing` — so no strip needed here.)
  - `test_check` (monitors.rs:344 — S2 defense): early-return for `dto.r#type == "heartbeat"` with a
    `ProbeOutcome{ ok:false, error_message:Some("n/a — heartbeat is a push monitor; it has no probe"),
    .. }` so a direct POST doesn't fall through to `http::probe` on a null URL.
  - **New `get_heartbeat` handler + route**: `GET /api/monitors/:id/heartbeat` → load the monitor; if
    `type != "heartbeat"` or no token → 404; else `Json(json!({"token": token, "ping_path":
    format!("/ping/{token}")}))`. Register `.route("/monitors/:id/heartbeat", get(monitors::get_heartbeat))`
    in `api/mod.rs`.
- [ ] **Step 3: Run → PASS** + full suite + clippy + no aws-lc. **Step 4: Commit** `git commit -am "feat: heartbeat create/validate — token gen (skip_serializing + dedicated endpoint), forced thresholds, grace, interval floor, test_check guard"`

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
    with an `anchor: crate::models::Connectivity` parameter. Extract the two post-commit branches into
    `pub(crate) async fn emit_opened(state, m, incident_id, from, to, down_trigger: Trigger)` (emits
    `IncidentOpened` + `MonitorTransition`, dispatches `down_trigger`) and
    `pub(crate) async fn emit_resolved(state, m, incident_id, from, to, duration_seconds)` (emits
    `IncidentResolved` + `MonitorTransition`, dispatches `Trigger::Recovered`). **Neither helper emits
    `MonitorUpdated`** — `apply_result` keeps its single unconditional tail `MonitorUpdated`
    (engine.rs:189-194, carrying `out.response_time_ms`), and the heartbeat callers (Tasks 5/6) emit
    their own. `apply_result`'s Opened branch chooses `down_trigger = if m.r#type == "heartbeat" {
    Trigger::HeartbeatMissed } else { Trigger::Down }` and calls `emit_opened`.
  - `worker::run_check`: pass `state.anchor.current().await` into `apply_result`.
  - `tests/engine_cycle.rs`: the `run_once` helper at **line 5 is called by BOTH the online AND the
    offline test** — it must pass `state.anchor.current().await` (mirroring `worker.rs:83`), NOT a
    literal, or the offline test's `status==Unknown`/`sent==0`/`incidents==0` assertions break. Only
    the standalone `:57` call site (online `test_state`) may pass `Connectivity::Online`. Tests import
    `Connectivity` via the existing `use vigil::models::*;` (engine_cycle.rs:1).
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
(route wiring — **delete the local `async fn ping()` placeholder at app.rs:52-57**, else it is
dead_code and `clippy -D warnings` fails). Test: `tests/heartbeat_ping.rs`.

**Interfaces:** `record_ping(state, m)`; the route returns 200 on a known token, 404 on unknown.

- [ ] **Step 1: Failing tests** — `tests/heartbeat_ping.rs` (app router + `common::test_state`; for
  the recovery-notify assertion attach a channel whose triggers include `"recovered"`):
  - `ping_unknown_token_404`: `GET /ping/doesnotexist` → 404.
  - `ping_updates_last_ping_at`: create a heartbeat; `POST /ping/:token` → 200; `last_ping_at` set.
  - `ping_recovers_down_heartbeat`: create a heartbeat, set status='down' + insert an open incident;
    `POST /ping/:token` → 200; status → `up`, incident resolved (`resolved_at` set), a `recovered`
    notification delivered (assert via `env.sent`).
  - `first_ping_arms_pending_no_false_recovery`: a never-pinged `pending` heartbeat (NO open incident)
    with a channel on `recovered`; `POST /ping/:token` → 200; status → `up`, **and NO `recovered`
    notification delivered** and NO phantom IncidentResolved (the switch merely arms).
  - `ping_up_heartbeat_no_new_incident`: an `up` heartbeat pinged again → still `up`, no incident,
    `last_ping_at` advanced.
  - `ping_paused_heartbeat_stays_paused`: a `paused` heartbeat pinged → `last_ping_at` set, stays `paused`.
  Run → FAIL.
- [ ] **Step 2: Implement `record_ping(state, m)` as ONE `BEGIN IMMEDIATE` transaction** (see the
  "Heartbeat transition pattern" in Shared Types — read the pre-status under the write lock so a
  concurrent reaper cannot interleave; a plain deferred `begin()` + SELECT is unsafe, and SQLite
  forbids a subquery in `RETURNING`):
  ```
  let mut c = state.db.acquire().await?;
  sqlx::query("BEGIN IMMEDIATE").execute(&mut *c).await?;
  let old: String = SELECT status FROM monitors WHERE id=?;                       // under the write lock
  UPDATE monitors SET last_ping_at=?1, updated_at=?1,
      status = CASE WHEN status IN ('down','pending') THEN 'up' ELSE status END WHERE id=?2;
  let mut closed: Option<(i64,i64)> = None;                                        // (incident_id, duration)
  if old == "down" {                                                              // ONLY down has an open incident
      if let Some((iid, started)) = SELECT id, started_at FROM incidents
             WHERE monitor_id=? AND resolved_at IS NULL ORDER BY started_at DESC LIMIT 1 {
          let dur = now - started;
          UPDATE incidents SET resolved_at=?, duration_seconds=? WHERE id=?;
          closed = Some((iid, dur));
      }
  }
  sqlx::query("COMMIT").execute(&mut *c).await?;
  // emit AFTER commit:
  match old.as_str() {
    "down"    => { if let Some((iid,dur)) = closed { engine::emit_resolved(state, m, iid, Status::Down, Status::Up, dur).await; }
                   state.bus.send(MonitorUpdated{ id:m.id, status:Status::Up, response_time_ms:None, checked_at:now }); }
    "pending" => { state.bus.send(MonitorTransition{ id:m.id, from:Status::Pending, to:Status::Up, incident_id:None });   // ARMING — no recovery notify
                   state.bus.send(MonitorUpdated{ id:m.id, status:Status::Up, response_time_ms:None, checked_at:now }); }
    _         => { state.bus.send(MonitorUpdated{ id:m.id, status:Status::from_db(&old), response_time_ms:None, checked_at:now }); } // up/paused/maintenance
  }
  ```
  **Key:** `pending → up` is *arming*, NOT recovery — no incident to close, no `recovered` dispatch
  (only `old == "down"` recovers). Only `emit_resolved` fires the `Recovered` notification.
  - The axum handler `async fn ping(State(state), Path(token): Path<String>) -> impl IntoResponse`:
    `SELECT * FROM monitors WHERE heartbeat_token = ?` → `StatusCode::NOT_FOUND` if none; else
    `record_ping(&state, &m).await`; `(StatusCode::OK, "ok")`. **Log `monitor_id`, never the token.**
  - `app.rs`: **delete the placeholder `ping()` (app.rs:52-57)** and register
    `.route("/ping/:token", get(heartbeat::ping).post(heartbeat::ping))` BEFORE the SPA static-asset
    fallback (colon form; axum 0.7).
- [ ] **Step 3: Run → PASS** + full suite + clippy. **Step 4: Commit** `git commit -am "feat: GET|POST /ping/:token receiver — atomic ping + conditional recover, 404 on unknown"`

---

## Task 6: `heartbeat_reaper` (single-arm due-query, atomic DOWN gate)

**Files:** Modify `crates/vigil/src/heartbeat.rs` (`reap_once`, `run_reaper`), `src/settings_store.rs`
(`heartbeat_tick_seconds`), `src/main.rs` (spawn). Test: `tests/heartbeat_reaper.rs`.

**Interfaces:** `reap_once(state)`, `run_reaper(state)`, `settings_store::heartbeat_tick_seconds`.

- [ ] **Step 1: Failing tests** — `tests/heartbeat_reaper.rs` (`common::test_state`):
  - `overdue_heartbeat_goes_down`: create a heartbeat (interval 60, grace 60), set `status='up'`,
    `last_ping_at = now - 200`; **attach a channel whose triggers JSON contains `"heartbeat_missed"`**
    (the default seed helper hardcodes `["down","recovered"]`, which `deliver()` filters out — S3);
    `heartbeat::reap_once(&state)`; assert status → `down`, an incident `cause='heartbeat'` opened, and
    a `heartbeat_missed` notification delivered (via `env.sent`).
  - `within_grace_not_reaped`: `status='up'`, `last_ping_at = now - 30` (< 60+60); `reap_once`; assert
    still `up`, no incident.
  - `never_pinged_not_reaped`: `status='pending'`, `last_ping_at=NULL`, `created_at = now - 10000`;
    `reap_once`; assert still `pending`, no incident, no alert (the switch is unarmed).
  - `fresh_ping_mid_reap_not_downed`: `status='up'` but `last_ping_at = now` (just pinged, NOT stale);
    `reap_once`; assert still `up`, no incident (the UPDATE's staleness predicate makes rows_affected 0).
  - `already_down_not_reopened`: `status='down'` + an open incident; `reap_once`; assert still exactly
    one open incident (idempotent).
  - `reaper_ignores_non_heartbeat`: seed an `up` http monitor (with a stale `last_checked`) + an overdue
    heartbeat; `reap_once`; assert ONLY the heartbeat flipped to `down` (the http monitor is untouched —
    guards the type filter against an accidental OR-loosening; S5).
  - `offline_anchor_still_reaps`: use `common::test_state_offline`; an overdue heartbeat still goes
    `down` (heartbeats aren't anchor-gated).
  Run → FAIL.
- [ ] **Step 2: Implement.**
  - `settings_store::heartbeat_tick_seconds`: `get(pool, "heartbeat.tick_seconds", "20").await.parse().unwrap_or(20)`.
  - `reap_once`: `SELECT * FROM monitors WHERE type='heartbeat' AND is_paused=0 AND status='up' AND
    last_ping_at IS NOT NULL AND ? > last_ping_at + interval_seconds + heartbeat_grace_seconds` (bind
    `now`). For each, ONE `BEGIN IMMEDIATE` transaction (Shared Types pattern — status flip + incident
    insert together): the DOWN gate **re-asserts the staleness predicate** so a ping that refreshed
    `last_ping_at` between the SELECT and the UPDATE makes rows_affected 0 and opens NO incident:
    `UPDATE monitors SET status='down', updated_at=?1 WHERE id=?2 AND status='up' AND ?1 > last_ping_at
    + interval_seconds + heartbeat_grace_seconds`; if `rows_affected == 1`, `INSERT INTO incidents
    (monitor_id, started_at, cause, error_message) VALUES (?, ?, 'heartbeat', 'no ping within interval
    + grace') RETURNING id` (same tx); `COMMIT`. **After commit** (only if it transitioned):
    `engine::emit_opened(state, m, incident_id, Status::Up, Status::Down, Trigger::HeartbeatMissed)`
    then `state.bus.send(MonitorUpdated{ id, status:Status::Down, response_time_ms:None, checked_at:now })`.
    Log + skip per-monitor errors.
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
  - `stats` (monitors.rs:499-508): the handler must load `type, last_ping_at` for the monitor (add
    `SELECT type, last_ping_at FROM monitors WHERE id=?` — `stats` currently only has the id). Let
    `is_heartbeat = type == "heartbeat"` and `armed = last_ping_at.is_some()`. Pass `had_any_check ||
    (is_heartbeat && armed)` into `uptime::compute` — so a **never-pinged** heartbeat still reports
    `uptime_pct: None` (matching the "waiting" UI), while an armed one gets incident-derived uptime (O1).
  - bar builder (monitors.rs:720-758): **`bars()` does NOT currently load the Monitor** — add `SELECT
    type, last_ping_at, created_at FROM monitors WHERE id=?` at its top. Compute `first_active_day =
    rollup::day_str(min(created_at, last_ping_at.unwrap_or(created_at)))` — **a UTC day String** (the
    whole bars pipeline is UTC via `rollup::day_str`/`day_bounds`; keep it a String so `day >=
    first_active_day` typechecks — String vs i64 would not compile — O2). Then at line 744:
    `let has_data = if is_heartbeat { armed && day >= first_active_day } else { <existing expr> };`
    (`armed = last_ping_at.is_some()`). Keep the existing expression for non-heartbeats. (Inline
    comment: heartbeat uptime is incident-derived, not rollup-derived — spec §11 / O2.)
- [ ] **Step 3: Run → PASS** + full suite + clippy. **Step 4: Commit** `git commit -am "feat: heartbeat stats/bars — incident-derived uptime (not had_any_check-gated), armed days render green"`

---

## Task 8: Frontend — heartbeat type, ping-URL, HeartbeatCard, Now-strip variant, triggers

**Files:** Modify `web/src/components/MonitorForm.tsx`, `web/src/components/DetailPanel.tsx`,
`web/src/components/MonitorCard.tsx`, `web/src/components/ListView.tsx`, `web/src/api.ts`. Create
`web/src/components/HeartbeatCard.tsx`. Tests: extend `web/src/__tests__/form.test.tsx`, create
`web/src/__tests__/heartbeatcard.test.tsx`.

- [ ] **Step 1: Failing tests:**
  - `form.test.tsx`: selecting type `heartbeat` → URL/host/cert-domain/threshold fields hidden, a
    **grace-seconds** field shown, and the **Test check** button hidden; saving a new heartbeat fetches
    the token from `/api/monitors/:id/heartbeat` and renders a **ping URL** panel containing `/ping/`;
    the notifications section shows a `heartbeat_missed` checkbox (checked) and no `down` checkbox;
    `buildDto` emits `triggers` for the attached channel equal to EXACTLY `["heartbeat_missed",
    "recovered"]` (no stray `"down"` — O4); `buildDto` sends `type:"heartbeat"`,
    `heartbeat_grace_seconds`, and NO `ssl_check_enabled`/`domain_check_enabled`.
  - `heartbeatcard.test.tsx`: stub `getHeartbeat` → `{token, ping_path}` and `getMonitor` → a heartbeat
    with `last_ping_at` set → HeartbeatCard renders the ping URL + "last ping" + "next expected by";
    with `last_ping_at:null` → the **"Waiting for first ping"** state.
  Run `cd web && npx vitest run` → FAIL.
- [ ] **Step 2: Implement.**
  - `api.ts`: add `getHeartbeat(id): Promise<{token:string, ping_path:string}>` → `fetch('/api/monitors/'+id+'/heartbeat').then(json)`;
    and a `pingUrl(path)` helper returning `${window.location.origin}${path}`. (The token is NOT on the
    Monitor object — it comes only from this endpoint.)
  - `MonitorForm.tsx`: add `{label:"Heartbeat", value:"heartbeat"}` to `MONITOR_TYPES`. Wrap URL/host/
    method/keyword/DNS/**Certificate&Domain section**/**confirmation+recovery inputs**/**ResponseChart**
    (S6 — heartbeats have no `/series`) in `Show when={type()!=='heartbeat'}`. Add a
    `heartbeat_grace_seconds` numeric input shown `when={type()==='heartbeat'}` (signal `graceSeconds`,
    default 60). Hide the **Test check** button for heartbeat. `buildDto`: for heartbeat, include
    `heartbeat_grace_seconds`, do NOT append `ssl_check_enabled`/`domain_check_enabled`. On successful
    save of a NEW heartbeat, call `getHeartbeat(saved.id)` and show a panel with the copyable ping URL
    (`pingUrl(hb.ping_path)`), a copy button, and `curl -fsS <url>`. In the notifications `NotifRow`,
    for a heartbeat monitor render a `heartbeat_missed` checkbox **instead of** `down`: extend
    `NotifRow` with `heartbeat_missed:boolean`; a heartbeat row defaults to `{attached, down:false,
    heartbeat_missed:true, recovered:true}` and `selectedNotifications` emits `"heartbeat_missed"` (and
    NOT `"down"`) for heartbeat monitors — strings must match `Trigger::as_str` exactly.
  - `HeartbeatCard.tsx` (new): props `{monitor}`; on mount `getHeartbeat(monitor.id)` for the URL; shows
    the ping URL (copy), **last ping** (relative + absolute), **next expected by** (`last_ping_at +
    interval_seconds + heartbeat_grace_seconds`, or "—"), and a **"Waiting for first ping"** state when
    `last_ping_at == null`.
  - `DetailPanel.tsx`: mount `<HeartbeatCard>` gated `type==='heartbeat'`; gate the existing
    `<ResponseChart>` on `type!=='heartbeat'` (S6); give the **Now strip** a heartbeat variant — replace
    the response-time + last-checked tiles with **last-ping** + **next-expected-by** when
    `type==='heartbeat'`; hide the **Check now** action for heartbeat.
  - `MonitorCard.tsx` / `ListView.tsx`: for heartbeat monitors, render **last ping** ("2m ago") in place
    of the response-time/sparkline slot.
- [ ] **Step 3: Run → PASS** + `npx tsc --noEmit` + `npx vite build`. **Step 4: Commit** `git commit -am "feat(web): heartbeat monitor type — form + ping URL + HeartbeatCard + Now-strip/list last-ping + heartbeat_missed trigger"`

---

## Task 9: Acceptance + final review

**Files:** Create `docs/superpowers/plans/P4.1-acceptance.md`. No product code unless a DoD item fails.

- [ ] **Step 1** — `0004` on a real P1/P2/P3 DB copy: version=4, data preserved.
- [ ] **Step 2 (live via Docker)** — create a heartbeat monitor (interval 60, grace 30); confirm
  `GET /api/monitors` and `GET /api/monitors/:id` JSON contain NO `heartbeat_token`, and `GET
  /api/monitors/:id/heartbeat` returns `{token, ping_path}`; `POST /ping/<token>` → 200, `last_ping_at`
  set, status → `up`.
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
