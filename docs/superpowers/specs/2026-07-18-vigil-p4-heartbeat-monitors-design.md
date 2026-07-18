# Vigil P4.1 — Heartbeat / Push Monitors — Design Spec (v2, hardened from adversarial review)

> First sub-project of P4 (Complete). The *inverse* monitor: instead of Vigil probing an endpoint, a
> heartbeat monitor waits for an inbound "ping" from the user's job (cron, backup, ETL). No check-in
> within the expected window → DOWN + alert. The "dead man's switch" (healthchecks.io / UptimeRobot
> push) pattern.

Builds on P1–P3 (on `master`). Single Rust/axum binary + SolidJS SPA, SQLite (WAL) on a mounted
volume, Docker (host `8099` → container `8090`), rustls-only, uptime derived from incidents. **axum
0.7** (path params use the colon form `:token`).

---

## 1. Goal & scope

Create a `heartbeat` monitor; Vigil returns a unique **ping URL**. The job hits that URL on a
schedule. Each ping records `last_ping_at`. If a pinged monitor then goes silent longer than
`interval + grace`, it goes DOWN (opens an incident, fires `heartbeat_missed`); the next ping recovers
it. A monitor that has **never** been pinged stays in a non-alerting "waiting" state — the switch
*arms* on the first ping.

**In scope:** the `heartbeat` type, token generation, `GET|POST /ping/:token`, a reaper, the
state-machine integration, `Cause::Heartbeat` + `Trigger::HeartbeatMissed`, stats/bars correctness,
and the UI. **Out of scope (later P4 sub-projects):** maintenance windows, reports, digest, theming,
backup.

---

## 2. Data model — migration `0004_heartbeat.sql`

```sql
ALTER TABLE monitors ADD COLUMN heartbeat_token TEXT;               -- URL-safe token; NULL for non-heartbeat
ALTER TABLE monitors ADD COLUMN heartbeat_grace_seconds INTEGER NOT NULL DEFAULT 60;
ALTER TABLE monitors ADD COLUMN last_ping_at INTEGER;              -- epoch of the most recent ping; NULL = never pinged
CREATE UNIQUE INDEX idx_monitors_heartbeat_token ON monitors(heartbeat_token) WHERE heartbeat_token IS NOT NULL;
```

- Partial unique index (index-only over real tokens; non-heartbeat NULLs don't participate). Valid
  SQLite; no `;`-splitting hazard for the version-ordered runner.
- Append `(4, include_str!("../migrations/0004_heartbeat.sql"))` to `MIGRATIONS` in `db.rs`. Additive;
  a P1/P2/P3 DB upgrades cleanly. No existing heartbeat rows exist, so no `unknown`-normalization
  backfill is needed (see §7b).
- **`Monitor` + its FromRow gain all three fields**: `heartbeat_token: Option<String>`,
  `heartbeat_grace_seconds: i64`, `last_ping_at: Option<i64>`. `test_defaults_monitor()` → grace 60,
  token/last_ping None.
- **DTO split (O3):** `CreateMonitorDto` gains **only** `heartbeat_grace_seconds` (`#[serde(default =
  "default_grace")]` → 60); it does **not** accept `heartbeat_token` (server-generated) or
  `last_ping_at` (runtime state). `UpdateMonitorDto` gains `heartbeat_grace_seconds: Option<i64>`
  (grace is editable, §9).

---

## 3. Enums

- `Cause::Heartbeat` (serde lowercase → `"heartbeat"`). **`engine.rs`'s exhaustive `Option<Cause>` →
  &str match (engine.rs:91-99) gains `Some(Cause::Heartbeat) => "heartbeat"`** or the crate won't
  compile.
- `Trigger::HeartbeatMissed` (`as_str` → `"heartbeat_missed"`; snake_case serde). **Three exhaustive
  `Trigger` matches must gain an arm** (all verified): `trigger_status` (dispatch.rs — returns
  `"down"`, a missed heartbeat is a down-state), `render` (templates.rs:21-27 — a DOWN-style subject),
  and `render_alert` (templates.rs:59-65 — will not compile without the arm even though heartbeat
  won't route there).

---

## 4. Token generation (the sole auth → must be a CSPRNG)

- On **create** of a `heartbeat` monitor (only), generate a 32-char `[A-Za-z0-9]` token from a
  CSPRNG: `rand::distributions::Alphanumeric` over `rand::thread_rng()` (**`rand` 0.8 is already a
  direct dep**, used at `scheduler.rs:39` — ring-clean, no new crate; never `Math`-style or the
  non-crypto path). Insert with retry on the (astronomically unlikely) unique-index collision.
- The token **is** the capability (healthchecks.io/UptimeRobot pattern), carried in the URL path.
- **Token exposure hardening (security):** `Monitor` derives `Serialize` with no field skip, so a
  naive list endpoint would hand every ping token to any client that can reach the *unauthenticated*
  `GET /api/monitors`. **The list endpoint must NOT return `heartbeat_token`** — only the
  single-monitor detail endpoint (`GET /api/monitors/:id`) returns it. Implement by nulling
  `heartbeat_token` in the list rows (map before serialize) or a list-specific response shape. This
  keeps a LAN peer from harvesting tokens and forging pings to hold a dead job falsely UP.

---

## 5. The receiver — `GET|POST /ping/:token`

- Registered on the **main axum router, outside `/api`**, and **before the SPA static-asset
  fallback** (`app.rs:46` already has the `.get(ping).post(ping)` placeholder at `/ping/:token` — keep
  the colon form; axum 0.7). axum auto-routes HEAD to the GET handler.
- Handler (must be race-safe — see §7a):
  1. Resolve the monitor by token: `SELECT ... WHERE heartbeat_token = ?` → **404** if none (no
     info leak).
  2. **Atomic recovery gate** (single statement, no stale-read): `UPDATE monitors SET last_ping_at =
     ?, status = CASE WHEN status IN ('down','pending') THEN 'up' ELSE status END, updated_at = ?
     WHERE id = ?` and capture the pre-update status (`RETURNING` the old status, or read-in-txn).
     - If the old status was `down` or `pending` → the row just transitioned to `up`: **close the
       open incident** (resolved_at, duration) and dispatch `Trigger::Recovered`, emit
       `IncidentResolved` + `MonitorTransition` + `MonitorUpdated`. Reuse the incident-close +
       notify code path (a small shared helper, or `apply_result` restricted to the close side — see
       §7a).
     - If old status was `up` → only `last_ping_at` moved: emit a lightweight `MonitorUpdated` so the
       UI "last ping" refreshes; **no** transaction-heavy transition, no incident, no double emit.
     - If `paused`/`maintenance` → `last_ping_at` is still recorded but status is untouched (a paused
       monitor stays paused); emit `MonitorUpdated`.
  3. Return `200` with body `"ok"`.
- The write and the transition decision are **the same atomic UPDATE**, so a concurrent reaper pass
  cannot see a stale `up` and open a DOWN while this ping recovers (O1 + M6).
- **GET side-effect note (S9):** a GET ping mutates state, and HEAD executes the GET handler. A link
  scanner / unfurler / prefetcher (SafeLinks, Proofpoint, Mimecast, browser prefetch) that fetches
  the ping URL will register a **false ping** (masking a real outage). Documented risk: the ping URL
  must not be posted where such scanners reach it (chat, email, issue trackers). This is inherent to
  the URL-as-capability pattern; we accept it and warn, matching healthchecks.io.

---

## 6. The reaper — `heartbeat_reaper` (tokio task)

- Spawned in `main::serve` beside the other background tasks. Loop `sleep(tick)`,
  `tick = settings_store::heartbeat_tick_seconds(&db)` (default **20s**, key `heartbeat.tick_seconds`).
- **Single, explicitly-parenthesized due-query** (only pinged-then-silent monitors; a never-pinged
  monitor stays PENDING and is *not* reaped — see below):

  ```sql
  SELECT * FROM monitors
  WHERE type = 'heartbeat'
    AND is_paused = 0
    AND status = 'up'
    AND last_ping_at IS NOT NULL
    AND ? > last_ping_at + interval_seconds + heartbeat_grace_seconds   -- bind: now
  ```
  (Precedence matters: without the parentheses/single-arm design, an `OR` disjunct could drop the
  `type='heartbeat'` guard and select every non-heartbeat pending monitor — `last_ping_at IS NULL` is
  permanently true for them — driving a **fleet-wide false DOWN**. This single-arm form avoids it.)

- For each due monitor, drive DOWN via an **atomic conditional gate**: `UPDATE monitors SET
  status='down', updated_at=? WHERE id=? AND status='up'`; only if `rows_affected == 1` proceed to
  **open one incident** (`cause='heartbeat'`, `error='no ping within interval + grace'`) and dispatch
  `Trigger::HeartbeatMissed`, emit `IncidentOpened` + `MonitorTransition` + `MonitorUpdated`. The
  `WHERE status='up'` makes it idempotent and race-safe against a concurrent recovering ping.
- **Never-pinged monitors are NOT reaped and NOT alerted.** A heartbeat with `last_ping_at IS NULL`
  stays `pending` ("waiting for first ping") indefinitely. The dead-man's-switch **arms on the first
  ping** — this removes the setup race (no DOWN+alert 2 minutes after Create, before the cron is even
  installed) and matches healthchecks.io. (`confirmation_threshold` is therefore moot for the DOWN
  edge; the grace window is the only tolerance.)
- Skips `down` (already open), `pending` (unarmed), `paused`, `maintenance`, `unknown` (the
  `status='up'` filter handles all). Per-monitor errors logged and skipped. No semaphore (pure DB).

---

## 7. State-machine integration (the crux)

Heartbeats reuse the incident/notification/event machinery, but with race-safe atomic gates and three
correctness changes:

**(a) Race-safety + anchor independence.** `apply_result` (engine.rs:32) currently reads
`state.anchor.current()` internally and decides the transition from the **passed-in `m.status`**
(potentially stale) — a read-then-write hazard the scheduler's in-flight guard normally prevents, but
heartbeats bypass that guard (§7.5). Two changes:
  1. `apply_result` gains an explicit `anchor: Connectivity` parameter. `worker::run_check` passes
     `state.anchor.current().await` (unchanged). **Both `tests/engine_cycle.rs:5` and `:57` call
     sites must be updated to pass `Connectivity::Online`** (compile-blocking otherwise).
  2. The heartbeat reaper and ping handler do **not** rely on the passed-in status to decide the
     transition. They make the decision atomic with the write via the conditional
     `UPDATE ... WHERE status = <expected>` gates (§5, §6): the side-effects (incident open/close,
     notify, events) run only when `rows_affected == 1`. This can be a thin heartbeat-specific
     transition helper that shares `apply_result`'s incident-open/close + notify blocks but takes the
     already-decided transition rather than re-reading status. Heartbeats always pass
     `Connectivity::Online` so a missed ping is never routed to `ToUnknown`.

**(b) Heartbeats excluded from the fleet-wide UNKNOWN reaction.** `bulk_set_unknown` (engine.rs:203)
flips every non-paused monitor to UNKNOWN on connectivity loss — wrong for heartbeats (not outbound
probed). Add `AND type != 'heartbeat'` to its SELECT and UPDATE. **Invariant + test:** no code path
leaves a heartbeat in `unknown` (so the reaper's `status='up'` filter can't strand it). A heartbeat's
status is only ever up / down / pending / paused / maintenance.

**(c) The trigger.** In the incident-opened path, choose the down-side trigger by type: `if m.type ==
"heartbeat" { Trigger::HeartbeatMissed } else { Trigger::Down }`. Recovery fires `Trigger::Recovered`
for both.

**(d) Default notification triggers (M1 — else the switch is silently defeated).** `deliver()` filters
channels by exact string match against `monitor_notifications.triggers`, whose form/default is
`["down","recovered"]`. `heartbeat_missed` is absent → the DOWN alert is **dropped**, while
`recovered` still fires — silent on death, chatty on recovery. Fix: when a channel is attached to a
**heartbeat** monitor, the default trigger set is `["heartbeat_missed","recovered"]`, and the form
exposes a `heartbeat_missed` checkbox (§10). (The generic `down` checkbox is hidden for heartbeats.)

### 7.5 Scheduler exclusion (heartbeats are NEVER probe-scheduled)

If the probe scheduler enqueues a heartbeat, `worker::run_check` → `probe::run`'s `_ => http::probe`
runs against `url = NULL` → connection error → **false DOWN** fighting the ping-driven state. **All**
of these touch-points must exclude heartbeats (points are jointly required):

- **Catch-up on restart** (`scheduler.rs:161`): `SELECT ... WHERE is_paused = 0` schedules a NULL
  `next_run_at` as `0` (due now) → add `AND type != 'heartbeat'`.
- **`reschedule_from_db`** (`scheduler.rs:125-136`): reads `next_run_at, is_paused` for one monitor
  after a check/update → also read `type` and skip scheduling when `type='heartbeat'`.
- **`SchedCmd::Upsert` call sites** — `create()`, `update()`, and `resume()` (`api/monitors.rs`) all
  hand a monitor to the scheduler. Skip the Upsert for heartbeat monitors; a heartbeat is created
  with `next_run_at = NULL` and `status = 'pending'` (not `0`/due).
- **`check_now`** (`api/monitors.rs:335-338`): sends `SchedCmd::CheckNow` with no type check →
  `scheduler` pushes `schedule(id, 0)` (due now), bypassing `reschedule_from_db`. **Make `check_now`
  a no-op/reject for heartbeat monitors in the handler**, AND hide the "Check now" action in the UI
  for heartbeats (§10). Do not rely on the worker guard alone.
- **`worker::run_check` guard** (belt-and-suspenders): if a heartbeat ever reaches it, `signal_complete`
  and return without probing.

Heartbeat liveness is owned entirely by the reaper (§6) + the ping receiver (§5).

---

## 8. Validation & create/update

`validate_monitor_dto` (extend the signature to also receive `domain_check_enabled` and
`heartbeat_grace_seconds`; update **both** call sites — create and update):
- Add a **`"heartbeat"` arm requiring neither `url` nor `host`** (else the DTO falls through the
  catch-all `_ => "url is required"` and is 422'd — heartbeats couldn't be created at all).
- `ssl_check_enabled` and `domain_check_enabled` must be `false` for heartbeat (422 — no TLS/domain
  target). `heartbeat_grace_seconds ≥ 1`. **Heartbeats are exempt from the `interval ≥ 15s` floor**
  (an interval can be hours/days for a nightly job); enforce `interval ≥ 30s` for heartbeats or reuse
  the existing floor exemption — pick one and state it: **heartbeat `interval_seconds ≥ 30`**.

`create()` (the fixed-column INSERT at `api/monitors.rs:156-196` — **it currently omits all three
heartbeat columns**): for a heartbeat, generate the token (needs the pool, §4), and **grow the INSERT
column list + VALUES + bind chain** to write `heartbeat_token`, `heartbeat_grace_seconds` (from DTO),
`last_ping_at` (NULL). **Force `confirmation_threshold = 1` and `recovery_threshold = 1` in `create()`**
(the pure validator can't mutate). Leave `next_run_at` NULL and `status = 'pending'`.

`update()` (fixed-column UPDATE at `api/monitors.rs:259-297`): add `heartbeat_grace_seconds` to the
UPDATE column list + bind (grace is editable); **re-force `confirmation_threshold = 1`/`recovery = 1`
for heartbeat rows** so a PATCH can't set them > 1 (which would make the reaper need multiple ticks to
trip). Type cannot change on edit (selector disabled since P2); the token is never regenerated in v1.

---

## 9. API / IPC surface

- `POST /api/monitors {type:"heartbeat", ...}` → creates it; the **create response returns the full
  row including `heartbeat_token`** so the UI can render the ping URL.
- `GET /api/monitors/:id` returns `heartbeat_token` (detail). **`GET /api/monitors` (list) strips it**
  (§4 security).
- `pause`/`resume`/`delete` reused. `update` can edit name/interval/grace/notifications (not type,
  not token). `check_now` is a no-op for heartbeats (§7.5).
- `GET|POST /ping/:token` — the receiver (§5), not under `/api`.
- No new SSE event — reuse `MonitorUpdated` / `MonitorTransition` / `IncidentOpened` /
  `IncidentResolved`.
- **Ping URL in the UI** is `{window.location.origin}/ping/{token}` — reflects however the operator
  reached the app (localhost / LAN / tunnel). The backend needs no self-URL.

---

## 10. UI

- **`MonitorForm`:** add `heartbeat` to `MONITOR_TYPES`. When selected: hide URL/host/method/keyword/
  DNS **and the Certificate & Domain section and the confirmation/recovery threshold inputs** (all
  currently ungated — wrap in `Show when={type()!=='heartbeat'}`); show **interval** (expected ping
  period) + **grace seconds**. `buildDto` must **not** send `ssl/domain` flags for heartbeat. Hide the
  **"Test check"** button for heartbeat (it would call `probe::run` on a null URL and show a
  meaningless failure — also short-circuit `test_check` server-side to return an "n/a — push monitor"
  outcome as defense). On **save of a new** heartbeat, reveal a panel with the **copyable ping URL**
  (`{origin}/ping/{token}`) + copy button + `curl -fsS {url}` one-liner + a "call this after your job
  succeeds" hint. Notifications section: show a **`heartbeat_missed`** checkbox (default on), hide
  `down`.
- **Detail panel:** a **Heartbeat card** (gated `type==='heartbeat'`) with the ping URL (copy), **last
  ping** (relative + absolute), **next expected by** (`last_ping_at + interval + grace`, or "—"), and
  a distinct **"Waiting for first ping"** state when `last_ping_at` is null. **The "Now strip"
  (status / response-time / last-checked tiles) needs a heartbeat variant**: response-time is always
  "—" and `last_checked_at` is never written for heartbeats (only `last_ping_at`), so show **last-ping
  / next-expected-by** in place of the response-time/last-checked tiles. Hide "Check now" for
  heartbeats.
- **Grid card / list row:** show **last ping** ("2m ago") in place of the response-time/sparkline
  slot; a heartbeat/pulse type glyph.

---

## 11. Stats / bars correctness (heartbeats have no `checks` rows)

Heartbeats never write a `checks` row (the reaper/ping don't go through `worker::run_check`). Two
places assume checks exist and must special-case heartbeats:
- **`stats()`** (`api/monitors.rs:499-508`) passes `had_any_check = EXISTS(checks for monitor)` to
  `uptime::compute`; `uptime.rs:24-29` early-returns `{uptime_pct: None, downtime_seconds: 0}` when
  `!had_any_check`, **ignoring incident spans**. A heartbeat with an open 2h incident would show
  `uptime_pct: null` and `0` downtime while actively DOWN. Fix: for `type='heartbeat'`, pass
  `had_any_check = true` (or derive "has signal" from `last_ping_at IS NOT NULL OR an incident
  exists`), so `compute` honors the incident spans.
- **90-day bar builder** (`api/monitors.rs:744-746`): `has_data` is true only for a day with a rollup,
  a raw check, or an overlapping incident — so a cleanly-pinging heartbeat's healthy days render as
  muted "no data". Fix: for heartbeats, treat a day with signal and no open incident (monitor armed,
  not pending) as `has_data = true` so up-days render green.
- **Rollups (O2, known limitation, note for the Reports sub-project):** `check_aggregates_daily` is
  keyed off `DISTINCT monitor_id FROM checks`; heartbeats get zero rollup rows ever. Heartbeat
  uptime/incidents for reports must be read from the `incidents` table, not the aggregates. Out of
  scope here; documented so the Reports sub-project accounts for it.

---

## 12. Non-functional / security

- `/ping/:token` is unauthenticated by design (token = capability); logged by monitor id, **never the
  full token**.
- **Exposure (S10):** `docker-compose.yml` publishes `${VIGIL_HOST_PORT:-8099}:8090` and `VIGIL_BIND`
  defaults to `0.0.0.0:8090` — i.e. out of the box the unauthenticated `/api` (create/update/delete)
  **and** the mutating `/ping` are LAN-reachable, and `/api/monitors/:id` distributes ping tokens.
  The spec does **not** silently claim "localhost only". Recommended hardening (operator's choice,
  documented — not force-changed since remote crons may need reach): publish
  `127.0.0.1:${VIGIL_HOST_PORT}:8090` for a same-host deployment, or front remote pings with an
  authenticated tunnel (cloudflared/tailscale). The token-stripped list endpoint (§4) limits harvest
  even on a trusted LAN.
- rustls-only preserved (no new TLS deps; the receiver is plain HTTP on the existing axum server). No
  new crates (`rand` already in-tree).

---

## 13. Build phases (for the plan)

1. Migration `0004` + `Monitor`/FromRow + DTO split + `Cause::Heartbeat` + `Trigger::HeartbeatMissed`
   (+ the 3 template/dispatch arms) + validation heartbeat arm + token generation & the grown
   `create()` INSERT + forced thresholds + `update()` grace.
2. Engine: `apply_result` `anchor` param (+ the two `engine_cycle.rs` call sites) + `bulk_set_unknown`
   heartbeat exclusion + type-based down-trigger + the race-safe heartbeat transition helper +
   **scheduler exclusion** (all §7.5 touch-points incl. `check_now`) — regression tests proving a
   heartbeat is never probe-scheduled and never left `unknown`.
3. `GET|POST /ping/:token` receiver (atomic recovery gate; 404; paused/up/down/pending paths; token
   never logged; list endpoint token strip).
4. `heartbeat_reaper` (single-arm due-query; atomic DOWN gate; never-pinged not reaped; idempotent) +
   `heartbeat.tick_seconds` + spawn.
5. Stats/bars heartbeat special-cases (§11) — tests that a DOWN heartbeat reports real downtime and a
   healthy heartbeat renders green days.
6. Frontend: form heartbeat type (fields gated, ping-URL reveal, heartbeat_missed trigger, no
   Test-check/Check-now); HeartbeatCard + Now-strip variant; list/grid last-ping.
7. Acceptance (live via Docker: create a heartbeat, curl its ping URL, watch DOWN past the window and
   recover on the next ping; confirm the list endpoint hides the token) + final review + merge.

---

## 14. Decisions log (resolved)

1. Exposure — `/ping` on the main server; operator controls reach; token stripped from list. ✅
2. Auth — token-as-capability-URL (CSPRNG), no extra auth. ✅
3. Anchor — heartbeat-missed **not** anchor-gated; heartbeats excluded from fleet UNKNOWN. ✅
4. Thresholds — forced `confirmation=1`/`recovery=1` in create() **and** re-forced in update(); grace
   is the buffer; interval floor for heartbeats is `≥ 30s`. ✅
5. Reaper tick — 20s default, `heartbeat.tick_seconds`. ✅
6. **Never-pinged** — stays PENDING/"waiting", never auto-DOWN, never alerts; the switch arms on the
   first ping (single-arm reaper). ✅
7. **Race-safety** — reaper DOWN and ping recovery use atomic conditional `UPDATE ... WHERE status`
   gates; side-effects only on `rows_affected == 1`. ✅
8. Grace editable via `update()`; token not rotatable and type not changeable in v1 (deferred). ✅
9. Stats/bars — heartbeats special-cased so incident-derived uptime shows (not `had_any_check`-gated);
   rollups remain checks-only (Reports sub-project reads incidents for heartbeats). ✅
