# Vigil P4.1 — Heartbeat / Push Monitors — Design Spec

> First sub-project of P4 (Complete). Adds the *inverse* monitor: instead of Vigil probing an
> endpoint, a heartbeat monitor waits for an inbound "ping" from the user's job (cron, backup
> script, ETL). No check-in within the expected window → DOWN + alert. The "dead man's switch"
> pattern (healthchecks.io / UptimeRobot push monitors).

Builds on P1–P3 (on `master`). Same stack: single Rust/axum binary + SolidJS SPA, SQLite (WAL) on a
mounted volume, Docker (host `8099` → container `8090`), rustls-only, uptime derived from incidents.

---

## 1. Goal & scope

A user creates a `heartbeat` monitor; Vigil hands back a unique **ping URL**. The user's job hits that
URL on a schedule. Vigil records each ping; if a ping doesn't arrive within `interval + grace`, the
monitor goes DOWN (opens an incident, fires the `heartbeat_missed` trigger). The next ping recovers it.

**In scope:** the `heartbeat` monitor type, token generation, the `GET|POST /ping/{token}` receiver, a
reaper task, the state-machine integration, `Cause::Heartbeat` + `Trigger::HeartbeatMissed`, and the
UI (form + detail card + list/grid rendering). **Out of scope (later P4 sub-projects):** maintenance
windows, reports, digest, theming, backup.

---

## 2. Data model — migration `0004_heartbeat.sql`

```sql
ALTER TABLE monitors ADD COLUMN heartbeat_token TEXT;               -- URL-safe token; NULL for non-heartbeat
ALTER TABLE monitors ADD COLUMN heartbeat_grace_seconds INTEGER NOT NULL DEFAULT 60;
ALTER TABLE monitors ADD COLUMN last_ping_at INTEGER;              -- epoch of the most recent ping; NULL = never pinged
CREATE UNIQUE INDEX idx_monitors_heartbeat_token ON monitors(heartbeat_token) WHERE heartbeat_token IS NOT NULL;
```

- A **partial** unique index (not a `UNIQUE` column constraint) so the many non-heartbeat monitors
  with `NULL` tokens don't collide (SQLite treats multiple NULLs as distinct, but a partial index is
  explicit and index-only over real tokens).
- Version-ordered runner: append `(4, include_str!("../migrations/0004_heartbeat.sql"))` to
  `MIGRATIONS` in `db.rs`. Additive only; a P1/P2/P3 DB upgrades cleanly.
- `Monitor` + FromRow + DTOs gain `heartbeat_token: Option<String>`, `heartbeat_grace_seconds: i64`,
  `last_ping_at: Option<i64>`. `test_defaults_monitor()` sets grace 60, token/last_ping None.

---

## 3. Enums

- `Cause::Heartbeat` (serde lowercase → `"heartbeat"`). **`engine.rs`'s exhaustive `Option<Cause>` →
  &str match (engine.rs:91-99) MUST gain `Some(Cause::Heartbeat) => "heartbeat"`** or the crate won't
  compile.
- `Trigger::HeartbeatMissed` (`as_str` → `"heartbeat_missed"`; snake_case serde). Add to
  `trigger_status()` (returns e.g. `"down"`, since a missed heartbeat is a down-state) and to the
  templates `render` match if it is exhaustive.

---

## 4. Token generation

- On **create** of a `heartbeat` monitor (and only then), generate a URL-safe token: 32 chars from
  `[A-Za-z0-9]` (or hex of 16 random bytes). Reuse an existing RNG in the tree if present; otherwise
  a small helper. Insert with retry on the (astronomically unlikely) unique-index collision.
- The token is the **capability**: whoever holds the URL can ping. No other auth. This is the
  established heartbeat pattern (healthchecks.io, UptimeRobot). The token lives in the URL path;
  exposure is governed by how the operator publishes port `8099` (localhost for same-host crons, LAN
  IP, or a tunnel like cloudflared/tailscale for remote jobs).
- Non-heartbeat monitors never get a token. Changing a monitor's type to/from heartbeat is **not**
  supported in edit mode (the type selector is already disabled on edit, per P2) — a heartbeat is
  created as such.

---

## 5. The receiver — `GET|POST /ping/{token}`

- Registered on the **main axum router, outside `/api`** (alongside `/events` and the static-asset
  fallback). Accepts both GET and POST (crons/curl use either). Returns `200 OK` with a tiny body
  (`"ok"`); unknown token → `404` (no information leak about which tokens exist).
- Handler:
  1. `SELECT * FROM monitors WHERE heartbeat_token = ?` → 404 if none.
  2. `UPDATE monitors SET last_ping_at = ? WHERE id = ?` (the hot path — a single indexed write).
  3. **Only if the monitor is currently `down` or `pending`**, drive a recovery/first-up transition:
     call `engine::apply_result(state, &m, &ProbeOutcome{ ok:true, .. }, Connectivity::Online)`
     (see §7). If it's already `up`, skip the transition (just the `last_ping_at` write) so a
     frequently-pinging healthy monitor doesn't run a transaction + emit an event on every ping.
     If it's `paused` or `maintenance`, record `last_ping_at` but do **not** transition (a paused
     monitor stays paused).
  4. Emit a lightweight `Event::MonitorUpdated { id, status, response_time_ms: None, checked_at: now }`
     **only when step 3 did not run a transition** (i.e. the already-`up`/`paused`/`maintenance`
     cases) — so the UI's "last ping" refreshes without a double emit. When step 3 *did* transition
     (down/pending → up), `apply_result` already emits `MonitorTransition` + `MonitorUpdated`, so the
     handler does not emit again.
- The route must be cheap and non-blocking; a ping is not a probe.

---

## 6. The reaper — `heartbeat_reaper` (tokio task)

- Spawned in `main::serve` alongside the other background tasks. Loop: `sleep(tick)` where
  `tick = settings_store::heartbeat_tick_seconds(&db)` (default **20s**, settings key
  `heartbeat.tick_seconds`).
- Each pass: `SELECT * FROM monitors WHERE type='heartbeat' AND is_paused=0 AND status IN
  ('up','pending') AND last_ping_at IS NOT NULL AND ? > last_ping_at + interval_seconds +
  heartbeat_grace_seconds` (bind `now`). For each, drive DOWN:
  `engine::apply_result(state, &m, &ProbeOutcome{ ok:false, cause:Some(Cause::Heartbeat),
  error_message:Some("no ping within interval + grace"), response_time_ms:None, status_code:None,
  resolved_ip:None }, Connectivity::Online)`.
- **Never-pinged monitors** (`last_ping_at IS NULL`): they sit `pending` until the first window
  elapses *from creation*. Treat `created_at` as the reference when `last_ping_at IS NULL`, i.e. the
  reaper query's second arm also selects `type='heartbeat' AND is_paused=0 AND status='pending' AND
  last_ping_at IS NULL AND ? > created_at + interval_seconds + heartbeat_grace_seconds`. (A job that
  never checks in even once is DOWN, after one grace window.)
- Idempotent: an already-`down` heartbeat is excluded by the `status IN ('up','pending')` filter, so
  the reaper opens exactly one incident per outage.
- Skip `maintenance`/`paused`/`unknown` (the `status IN ('up','pending')` filter handles all three).
- Errors per-monitor are logged and skipped (one bad row doesn't kill the loop). No semaphore needed
  (these are pure DB transitions, not network probes).

---

## 7. State-machine integration (the crux)

Heartbeats reuse the existing incident/notification/event machinery in `engine::apply_result`, with
two changes so heartbeat semantics are correct:

**(a) Anchor independence.** `apply_result` currently reads `state.anchor.current()` internally
(engine.rs:34) and `state::evaluate` turns a failing probe into `ToUnknown` when the anchor is
Offline. That is right for *outbound* probes (suppress false DOWN when Vigil's own link is down) but
wrong for heartbeats (an inbound ping's absence is about the user's job, and the reaper/ping paths
must be deterministic). **Refactor:** `apply_result` gains an explicit `anchor: Connectivity`
parameter; `worker::run_check` passes `state.anchor.current().await` (unchanged behavior); the
heartbeat reaper and ping handler pass `Connectivity::Online` (so `evaluate` never routes a heartbeat
to `ToUnknown`). Low blast radius — `apply_result` has one existing caller (`worker::run_check`).

**(b) Heartbeat monitors are excluded from the fleet-wide UNKNOWN reaction.** `bulk_set_unknown`
(engine.rs:203) flips every non-paused monitor to UNKNOWN when connectivity drops. Heartbeats aren't
outbound-probed, so a lost outbound anchor says nothing about them — add `AND type != 'heartbeat'` to
both its SELECT and UPDATE. Heartbeats therefore never enter UNKNOWN; their status is purely
ping-driven (up / down / pending / paused / maintenance).

**(c) The trigger.** In `apply_result`'s incident-opened branch, choose the down-side trigger by type:
`let down_trigger = if m.r#type == "heartbeat" { Trigger::HeartbeatMissed } else { Trigger::Down };`
and dispatch that. Recovery fires `Trigger::Recovered` for both (a heartbeat coming back is
"recovered"). This keeps `heartbeat_missed` a first-class, per-channel-toggleable trigger (§7 of
CLAUDE.md) without a second dispatch path.

**(d) Thresholds.** Heartbeat monitors are created with `confirmation_threshold = 1` and
`recovery_threshold = 1` (the **grace window is the confirmation buffer**; a second reaper tick
shouldn't add latency). These aren't exposed for editing on the heartbeat form. With threshold 1, one
reaper detection → DOWN, one ping → UP.

No `checks` row is written for heartbeats (they have no response time; the reaper/ping call
`apply_result` directly, not through `worker::run_check` which is what inserts the `checks` row).
Uptime still works — it's derived from incidents, which `apply_result` opens/closes.

### 7.5 Scheduler exclusion (CRITICAL — heartbeats are never probe-scheduled)

The P1 probe scheduler must **never** enqueue a heartbeat monitor. If it did, `worker::run_check`
would call `probe::run`, whose `_ => http::probe` fallback would HTTP-probe a monitor with no URL →
connection error → a **false DOWN** that fights the ping-driven state. Verified touch-points (the plan
must cover all of them):

- **Catch-up on restart** (`scheduler.rs:161`): the `SELECT id, next_run_at FROM monitors WHERE
  is_paused = 0` schedules a NULL `next_run_at` as `0` (due now). Add `AND type != 'heartbeat'`.
- **`reschedule_from_db`** (`scheduler.rs:125-136`): re-heaps a single monitor after a check/update.
  Its `SELECT next_run_at, is_paused` must also read `type` and skip scheduling when
  `type = 'heartbeat'` (else an update/resume re-heaps it).
- **Create / resume** (`api/monitors.rs`): wherever a newly-created or resumed monitor is handed to
  the scheduler (`SchedCmd`), skip that for heartbeat monitors; a heartbeat is created with
  `next_run_at = NULL` and `status = 'pending'` (not `0`/due).
- **`worker::run_check` guard** (belt-and-suspenders): if a heartbeat monitor ever reaches it, return
  after `signal_complete` without probing (never fall through to `probe::run`).

Heartbeat liveness is owned entirely by the **reaper** (§6) + the **ping receiver** (§5), not the
scheduler.

---

## 8. Validation

`validate_monitor_dto` (already parameterized in P3): a `heartbeat`-type monitor requires neither
`url` nor `host` (it has no target). `interval_seconds` and `heartbeat_grace_seconds` must be ≥ 1.
`ssl_check_enabled`/`domain_check_enabled` must be false for heartbeat (422 otherwise — a heartbeat
has no TLS/domain target). On create, force `confirmation_threshold = 1`, `recovery_threshold = 1`,
and generate the token server-side (ignore any client-supplied token).

---

## 9. API / IPC surface

- `POST /api/monitors` with `type:"heartbeat"` → creates it, returns the row **including
  `heartbeat_token`** so the UI can render the ping URL.
- `GET /api/monitors/:id` returns the token too (so re-opening the detail panel shows the URL).
- Reuse existing `pause`/`resume`/`delete`/`update` (update cannot change type or regenerate the
  token in v1 — keep it simple; a "rotate token" action is a deferred nicety).
- `GET|POST /ping/{token}` — the receiver (§5), not under `/api`.
- No new SSE event type — reuse `MonitorUpdated` / `MonitorTransition` / `IncidentOpened` /
  `IncidentResolved`, which the frontend already handles.

**The ping URL shown in the UI** is `{origin}/ping/{token}` where `{origin}` is the browser's
`window.location.origin` (so it reflects however the operator reached the app — localhost, LAN IP,
or tunnel hostname). The backend doesn't need to know its own external URL.

---

## 10. UI

- **Monitor form:** add `heartbeat` to the type selector. When selected: hide URL/host/method/keyword/
  DNS/SSL/domain fields; show **interval** (the expected ping period, reuse the interval chips) and
  **grace seconds**. Hide confirmation/recovery threshold (forced to 1). On **save of a new**
  heartbeat monitor, reveal a success panel with the **copyable ping URL** (`{origin}/ping/{token}`),
  a copy button, and a curl one-liner (`curl -fsS {url}`) plus a one-line "call this from your cron
  after the job succeeds" hint.
- **Detail panel:** a **Heartbeat card** (gated on `type==='heartbeat'`) showing: the ping URL (with
  copy), **last ping** (relative + absolute on hover), **next expected by** (`last_ping_at + interval
  + grace`, or "—" if never pinged), and a distinct **"Waiting for first ping"** state when
  `last_ping_at` is null. Status pill/incident timeline reuse the existing components.
- **Grid card / list row:** for heartbeat monitors, show **last ping** ("2m ago") in place of the
  response-time/sparkline slot; the type icon is a heartbeat/pulse glyph.
- Respect the existing status colors (down pulses, up breathes).

---

## 11. Testing

**Rust:**
- migration `0004`: fresh DB → version 4 + the 3 columns + partial index selectable; v3-DB upgrade
  applies only 0004, preserves data.
- token generation: two heartbeat monitors get distinct tokens; token is URL-safe; non-heartbeat
  monitors get NULL.
- `/ping/{token}`: valid token updates `last_ping_at` and returns 200; a DOWN heartbeat recovers
  (incident closed, `recovered` dispatched); a healthy (up) heartbeat's ping does NOT open a
  transaction-heavy transition (assert no spurious incident/transition); unknown token → 404;
  paused monitor's ping updates `last_ping_at` but stays paused.
- reaper: an overdue heartbeat (last ping older than interval+grace) → DOWN, incident opened with
  `cause='heartbeat'`, `heartbeat_missed` dispatched (assert via the notify double); a heartbeat
  pinged **within** grace is NOT reaped; a never-pinged heartbeat past its first window from
  `created_at` → DOWN; an already-DOWN heartbeat is not re-opened (one incident per outage).
- anchor independence: with `test_state_offline` (anchor Offline), the reaper STILL drives a heartbeat
  DOWN (not UNKNOWN); `bulk_set_unknown` leaves heartbeat monitors' status unchanged.
- trigger selection: a heartbeat DOWN dispatches `heartbeat_missed`, an http DOWN still dispatches
  `down` (guards the type-branch).
- `apply_result` anchor-param refactor: existing worker tests still pass (regression).

**Web (vitest):** form shows heartbeat fields + the ping URL on save; HeartbeatCard renders last-ping
+ waiting state; list/grid render last-ping for heartbeat type.

---

## 12. Non-functional / security

- The `/ping` route is unauthenticated by design (token = capability). Document that exposing port
  `8099` to an untrusted network exposes both the unauthenticated `/api` and `/ping` — the app is
  single-operator and meant for localhost / trusted LAN / an authenticated tunnel (unchanged from
  P1–P3).
- Token is not a secret in the keychain sense (it's a URL); it lives in the DB `monitors` row like
  any config. It is **not** logged in full on ping (log the monitor id, not the token).
- rustls-only preserved (no new TLS deps; the receiver is plain HTTP served by the existing axum
  server). No new crates beyond an RNG if one isn't already present.

---

## 13. Build phases (for the plan)

1. Migration `0004` + models/DTOs + `Cause::Heartbeat` + `Trigger::HeartbeatMissed` + validation +
   token generation on create.
2. Engine changes: `apply_result` anchor-param refactor + `bulk_set_unknown` heartbeat exclusion +
   type-based down-trigger selection + **scheduler exclusion** (§7.5: catch-up query,
   `reschedule_from_db`, create/resume scheduling, `worker::run_check` guard) — with regression tests
   proving a heartbeat monitor is never probe-scheduled.
3. `GET|POST /ping/{token}` receiver (route + handler + tests).
4. `heartbeat_reaper` task + `heartbeat.tick_seconds` setting + spawn in `main` (tests).
5. Frontend: form heartbeat type + ping-URL reveal; HeartbeatCard; list/grid last-ping.
6. Acceptance (live: create a heartbeat via the API, curl its ping URL, watch it go DOWN past the
   window and recover on the next ping — through the running Docker stack) + final review + merge.

---

## 14. Open decisions (resolved)

1. **Exposure** — `/ping` on the main `8099` server; operator controls reach. ✅
2. **Auth** — token-as-capability-URL, no extra auth. ✅
3. **Anchor** — heartbeat-missed is **not** anchor-gated; heartbeats excluded from fleet UNKNOWN. ✅
4. **Thresholds** — forced `confirmation=1`/`recovery=1`; grace is the buffer. ✅
5. **Reaper tick** — 20s default, `heartbeat.tick_seconds` setting. ✅
6. **Token rotation / type change on edit** — out of scope for v1 (deferred nicety). ✅
