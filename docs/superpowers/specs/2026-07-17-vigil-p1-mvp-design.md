# Vigil P1 (MVP) — Design Spec

> **Status:** approved for planning · **Date:** 2026-07-17 · **Revision:** v2 (spec-review-hardened)
> **Scope:** Phase 1 (MVP) only. P2–P4 get their own brainstorm → spec → build cycles.
> **Parent spec:** [`CLAUDE.md`](../../../CLAUDE.md) (full product blueprint) · **UI reference:** [`docs/vigil_dashboard_mock.html`](../../vigil_dashboard_mock.html)

This document is the build-ready design for Vigil's first shippable slice. It carves a clean P1 subset out of the full blueprint and records the architectural decisions that diverge from it. Where this document and `CLAUDE.md` disagree, **this document wins for P1**; `CLAUDE.md` remains the north star for later phases. v2 incorporates the fixes from the multi-lens spec review — see the changelog in §15.

---

## 1. P1 Definition of Done

Point Vigil at a real website. When the site goes down, Vigil detects it — *confirmed*, not fooled by a single blip and not fooled by **your own** connection dropping — opens an incident, and sends you an **email**. When the site recovers, Vigil closes the incident and sends a recovery email. The whole thing runs as a background container you started with `docker compose up -d`, survives reboots, and shows a live navy dashboard in your browser.

Concretely, P1 is done when all of these are true:

1. `docker compose up -d` brings up a container that reports **healthy** (via the in-binary healthcheck, §4.1); the dashboard loads at `http://<host>:8080`.
2. You can add, edit, pause, resume, delete, and manually re-check an HTTP(S) monitor from the UI.
3. A monitor pointed at a reachable site reads **UP**; response time and last-checked update live (via SSE) without a page refresh, and the dashboard **re-syncs correctly** after a dropped/reconnected SSE stream (§9.3).
4. Taking the target down drives the monitor to **DOWN** only after the confirmation threshold, opening an incident and sending one **down** email.
5. Killing *your* internet (not the target) drives affected monitors to **UNKNOWN** with a connectivity banner and **no** false down-emails; restoring it resumes normal evaluation and closes any incident correctly (§7).
6. Recovery — including recovery via a DOWN→UNKNOWN→UP path — sends one **recovered** email and closes the incident with a computed duration.
7. State survives `docker compose restart` — monitors, incidents, and schedules resume; past-due checks catch up staggered.

---

## 2. Scope

### 2.1 In scope (P1)

- **Monitor type:** HTTP(S) only — method (GET/POST/HEAD), request headers, body, basic/bearer/header auth, `expected_status_codes` (CSV of codes/ranges, default `200-299`), `timeout_seconds`, `follow_redirects`, `verify_ssl`.
- **Scheduler:** per-monitor interval (presets `30s,1m,2m,5m,10m,15m,30m,1h,6h,12h,24h` + custom seconds, **15s floor**), ±5% jitter on `next_run_at`, global concurrency semaphore (default 25), catch-up (staggered) on restart for past-due monitors.
- **State machine:** states `PENDING → UP | DOWN`, plus `PAUSED` and `UNKNOWN`. Confirmation threshold (default 3) with fast retry cadence (`retry_interval_seconds`, default 30) after first failure; recovery confirmation (default 1). **Anchor internet-sanity gate** before confirming any DOWN.
- **Anchor gate:** before a monitor is confirmed DOWN, probe configurable anchors (default `1.1.1.1:443`, `8.8.8.8:443`). If anchors are unreachable → **fleet-wide UNKNOWN**, alerting suppressed, `connectivity:changed{online:false}` emitted; a background poll restores normal evaluation when connectivity returns (§6.3).
- **Incidents:** open on entering DOWN (`started_at`, `cause`, `status_code`/`error_message`), close on return to UP (`resolved_at`, computed `duration_seconds`), including the DOWN→UNKNOWN→UP recovery path.
- **Storage:** SQLite (WAL) on a mounted volume. Tables: `monitors`, `checks`, `incidents`, `notification_channels`, `monitor_notifications`, `notification_log`, `settings`, plus a `schema_migrations` bookkeeping table.
- **Notifications:** **Email (SMTP) only.** Triggers `down` and `recovered`. Per-`(monitor, trigger)` cooldown (default 15 min) to prevent duplicate sends. "Send test email" action. Message templating with the P1 variable subset.
- **UI (SolidJS, served by axum):** left rail + top bar; **monitor grid** with status-dot cards; **detail panel** (header + now-strip + 24h/7d uptime tiles + collapsed config summary); **Add/Edit monitor** panel with live **Test check**; **Settings** (SMTP config, anchor hosts, retention); empty/loading/error states; **connectivity-down banner**. Live updates via SSE with snapshot-on-connect.
- **Deployment:** multi-stage `Dockerfile` (non-root, pre-owned `/data`), `docker-compose.yml` (one service + named volume + Docker secret), `.env.example`, in-binary healthcheck, `restart: unless-stopped`.

### 2.2 Out of scope (deferred to later phases)

Response-time charts and the 90-day uptime bar with daily rollups (`check_aggregates_daily`); keyword / TCP-port / DNS / ping / heartbeat / SSL-only monitor types; SSL certificate and domain-expiry tracking (`ssl_certs`, `domain_info`); webhook/Discord/Slack/Telegram/ntfy/Pushover channels and desktop/browser push; maintenance windows; monthly incident reports; list view; incident acknowledge + re-notify + daily digest; `DEGRADED` and `MAINTENANCE` states; tag filtering + drag reorder; the interactive accent/theme picker; multi-user/auth. These remain specified in `CLAUDE.md` for P2–P4.

> **Note on desktop notifications:** the full spec's P1 lists "desktop + email." Native desktop notifications require a logged-in desktop session and do not exist inside a container; they are intentionally dropped from this containerized P1. Email is the reliable 24/7 backbone. Push channels (ntfy/Discord/webhook) arrive in P3.

---

## 3. Architecture

### 3.1 Container model

Vigil P1 is a **single Rust binary** run as **one container**. `axum` serves four surfaces from one process: the REST API, an SSE event stream, the static SolidJS UI, and the `/ping/{token}` heartbeat endpoint (route reserved in P1 — heartbeat *monitors* are P4, but the route exists so the port contract is stable). The check engine (scheduler + worker pool + state machine + notifier + anchor poller) runs as `tokio` tasks in the same process. SQLite lives on a mounted named volume at `/data`.

```
docker compose up -d
 └─ service: vigil  (one Rust binary, one container, runs as non-root)
     ├─ axum ────────── REST API  +  SSE /events  +  static SolidJS UI (SPA fallback)  +  /ping/{token} (reserved)
     ├─ scheduler ───── delay-queue, ±5% jitter, concurrency semaphore (default 25)
     ├─ worker pool ─── reqwest HTTP probes with per-monitor timeout
     ├─ state machine ─ PENDING→UP|DOWN + PAUSED + UNKNOWN; confirmation/recovery; anchor gate
     ├─ anchor poller ─ background 15s poll while offline; lifts UNKNOWN on reconnect
     ├─ notifier ────── email (SMTP via lettre, rustls-tls)
     └─ SQLite (WAL) ── /data/vigil.db on a mounted named volume (dir owned by app user)
     bind 0.0.0.0:8080  → compose maps host 8080 (LAN-accessible)
     restart: unless-stopped ; secret smtp_password → /run/secrets/smtp_password
     healthcheck: ["CMD","/usr/local/bin/vigil","healthcheck"]
```

**Why a container instead of the spec's Tauri desktop app:** a container runs 24/7 regardless of desktop-session state and restarts on boot — strictly better for an always-on monitor. The trade-offs (no native notifications, no OS keychain) are handled by email-only alerting and Docker-secret-based secret storage, below.

### 3.2 Rust module layout

Each module has one job and a well-defined interface. Pure-logic modules (`state`, the anchor decision, `cooldown`, status-code parsing, template rendering, uptime computation) are unit-tested in isolation.

| Module | Responsibility | Key dependencies |
|---|---|---|
| `main` | Wire everything: load config, open DB, run migrations, spawn engine tasks, start axum. Also dispatches the `healthcheck` subcommand. | all |
| `config` | Load **startup-only** config from env + Docker secrets (bind addr, DB path, secret, concurrency). Distinct from DB-backed live settings (§4.2). | — |
| `db` | `sqlx` SQLite pool. On **every** pooled connection set `PRAGMA foreign_keys=ON` and `journal_mode=WAL` (via `SqliteConnectOptions`); enable `auto_vacuum=INCREMENTAL` at first DB creation. Migration runner, typed queries. | `sqlx` |
| `models` | Domain types + DTOs (Monitor, Check, Incident, Channel, Settings) + serde. | `serde` |
| `secrets` | Read `/run/secrets/*` (fallback env) at startup; expose SMTP password to notifier. Never persisted to DB. Logs a clear error if a secret file exists but is unreadable. | — |
| `settings_store` | Read/write DB-backed live settings; **read at point-of-use** (or cache-with-invalidation on write) so UI edits to anchors/SMTP/cooldown/retention take effect without a restart. | `db` |
| `scheduler` | Delay-queue of `next_run_at`; jitter; catch-up; recompute on config change; hand jobs to workers under the semaphore. | `tokio` |
| `worker` | Execute one probe with timeout; write `checks` row; hand result to `state`. | `tokio` |
| `probe::http` | The HTTP prober: build request (method/headers/body/auth via `auth_ref` grammar §6.2), send via `reqwest` (rustls), classify success vs failure + cause. | `reqwest`, `rustls` |
| `state` | **Pure** transition function: `(current, prev_confirmed_state, streaks, result, anchor_verdict, thresholds) → (new_state, transition?)`. No I/O. | — |
| `anchor` | Probe anchor hosts (TCP connect); cache verdict (10s TTL) with an explicit flip rule; background 15s poll while offline; gate DOWN confirmation and drive fleet-wide UNKNOWN. | `tokio` |
| `incidents` | Open/close incidents on transitions; compute duration. Source of truth for uptime/downtime. | `db` |
| `notify::email` | Compose + send SMTP mail via `lettre` (built with `rustls-tls`); render templates. | `lettre` |
| `notify::dispatch` | Map a transition → eligible channels + triggers; apply cooldown; write `notification_log`. Suppressed entirely while connectivity is offline. | `db` |
| `uptime` | **Pure** computation of uptime% + downtime for a window from incident spans (§10.3). | — |
| `api` | axum routers: REST handlers (thin — validate DTO, call service, return JSON) + SSE. | `axum` |
| `events` | In-process broadcast bus (`tokio::sync::broadcast`). Each `/events` connection gets a **full state snapshot first**, then deltas with incrementing event IDs; on a broadcast `Lagged` error, push a fresh snapshot rather than dropping. | `tokio` |
| `static_assets` | Serve the built SolidJS bundle, with an **SPA fallback** to `index.html` for any GET not matching `/api`, `/events`, `/ping`, or a real asset (so deep links / refreshes work). | `axum` |

### 3.3 Data flow per check

`scheduler` fires when `next_run_at ≤ now` → job enqueued, acquires a semaphore permit → `worker` runs `probe::http` with timeout → result row written to `checks` (raw probe outcome, kept for future response-time history; **not** the source of uptime) → `state` evaluates the transition from `(status, prev_confirmed_state, consecutive_failures, consecutive_successes, anchor_verdict)`:

- **Success** → increment success streak; if currently DOWN and streak ≥ recovery threshold → transition **UP** (close incident, dispatch `recovered`). Reset failure streak. Schedule next run at normal interval.
- **Failure** → increment failure streak. If streak < confirmation threshold → stay UP/PENDING, schedule next run at `retry_interval` (fast retry). If streak ≥ threshold → **consult anchor gate**: anchors up → transition **DOWN** (open incident, dispatch `down`); anchors down → the anchor gate has flipped the whole fleet to **UNKNOWN** (see §6.3), no incident opens and no alert fires. Reset success streak.

**UNKNOWN incident handling:** entering UNKNOWN does **not** close an already-open incident — a monitor that was DOWN keeps its incident open (suspended) while connectivity is out. On leaving UNKNOWN: the monitor re-evaluates on its next check; if its `prev_confirmed_state` was DOWN and it now succeeds, the state machine closes the still-open incident and fires `recovered` (this is the DOWN→UNKNOWN→UP path in DoD item 6). If it was UP and still succeeds, it simply returns to UP with no incident churn.

**Uptime is derived from incidents, never from counting raw `checks`.** Failed probes recorded during an UNKNOWN window (your-own-outage) never open an incident, so they never count as downtime. This sidesteps count-based corruption and matches the time-weighted formula in §10.3.

Every write emits `monitor:updated`; every state change also emits `monitor:transition` (and `incident:opened`/`incident:resolved`); connectivity changes emit `connectivity:changed`. SSE fans these to the browser, which patches only the affected card/panel. A config edit recomputes that monitor's queue entry immediately.

---

## 4. Deployment & configuration

### 4.1 Files

- **`Dockerfile`** — multi-stage: (1) build the SolidJS UI with Node; (2) build the Rust binary (`reqwest` + `lettre` both on **rustls**, no OpenSSL) with the UI bundle copied into the served dir; (3) slim runtime image (`debian:bookworm-slim`) with `ca-certificates` installed (for TLS probing + SMTP). The runtime stage **creates a non-root app user, `mkdir -p /data && chown app:app /data`, and sets `USER app` before the volume is mounted** — so the named volume inherits app-user ownership and the process can create `vigil.db` plus its `-wal`/`-shm` sidecars (WAL needs the *directory* writable, not just the file).
- **`docker-compose.yml`** — one `vigil` service: builds/pulls the image, maps `8080:8080` on `0.0.0.0`, mounts named volume `vigil-data:/data`, declares the `smtp_password` secret, sets `restart: unless-stopped`, and a healthcheck using the **in-binary subcommand**: `healthcheck: {test: ["CMD","/usr/local/bin/vigil","healthcheck"], interval: 30s, timeout: 3s, retries: 3}`. The subcommand does an in-process HTTP GET to `http://127.0.0.1:8080/healthz` and exits 0/1 — no `curl`/shell needed in the image (neither slim nor distroless ships one).
- **`.env.example`** — documents env vars (§4.2) with safe defaults.
- **`secrets/smtp_password.example`** — placeholder documenting the Docker secret file, including the **readability requirement** (§4.3).

### 4.2 Configuration surface

Config splits into two tiers:

**Startup-only (env + secret, read once by `config`)** — changing these needs a restart:

| Setting | Source | Default | Notes |
|---|---|---|---|
| Bind address | env `VIGIL_BIND` | `0.0.0.0:8080` | LAN-accessible per decision. |
| DB path | env `VIGIL_DB` | `/data/vigil.db` | On the mounted volume. |
| Global probe concurrency | env `VIGIL_MAX_CONCURRENCY` | `25` | Semaphore size. |
| SMTP password | file `/run/secrets/smtp_password` (fallback env `VIGIL_SMTP_PASSWORD` for dev) | — | Never written to DB. |

**Live (DB-backed `settings`/channel rows, read at point-of-use by `settings_store`)** — editable in the UI, effective without restart:

| Setting | Home | Default | Notes |
|---|---|---|---|
| Anchor hosts | `settings` key `anchors` | `1.1.1.1:443,8.8.8.8:443` | Anchor task re-reads per poll. |
| Notify cooldown | `settings` key `notify.cooldown_minutes` | `15` | Per-(monitor, trigger); see §8.2. |
| Raw-check retention | `settings` key `retention.raw_days` | `30` | Nightly maintenance (§5). |
| Accent | `settings` key `appearance.accent` | `#3FC8E4` | P1 ships fixed cyan; interactive picker deferred (§10.2). |
| SMTP host/port/security/from/to | `notification_channels.config` (JSON) of the email channel | — | **Single source of truth for SMTP config.** UI-managed. Notifier re-reads on send. |

**SMTP password lifecycle:** supplied only as a Docker secret read at boot. The Settings UI edits the email channel's `config` (host/port/security/from/recipients) and displays *"Password managed via Docker secret"* with no password field. Rotating the password means editing `secrets/smtp_password` and re-running `docker compose up -d`.

### 4.3 Network & security posture (P1)

- Binds `0.0.0.0:8080`; compose maps host `8080`. Reachable from the LAN (dashboard + reserved heartbeat route).
- **No authentication** in P1 (spec non-goal §1: single trusted operator on a trusted network). Documented as such; a simple access token is a candidate for a later phase if exposed beyond LAN.
- `verify_ssl` defaults on for probes; both `reqwest` and `lettre` use rustls with the system CA bundle (`ca-certificates`).
- Runs as **non-root**; `/data` and the DB file are owned/writable only by the app user.
- **Docker secret readability:** compose file-secrets are mounted preserving the host file's uid/gid/mode. `secrets/smtp_password` must be readable by the container's app user — created world-readable (`0644`) *or* owned by a uid matching the app user. The README and `.example` state this; the `secrets` module logs a clear, actionable error if the file exists but can't be read (so email failures aren't silent).
- The secret is never logged, never returned by any API, never stored in DB.

---

## 5. Data model (P1 subset)

SQLite, WAL mode, `foreign_keys=ON` per connection (§3.2), `auto_vacuum=INCREMENTAL`. DDL below is the P1 slice; columns are chosen for forward-compatibility with `CLAUDE.md` §9 so later phases add via `ALTER TABLE ADD COLUMN`/new tables without rebuilds. Timestamps are UNIX epoch seconds (INTEGER) unless noted.

```sql
CREATE TABLE schema_migrations (version INTEGER PRIMARY KEY, applied_at INTEGER NOT NULL);

CREATE TABLE monitors (
  id                     INTEGER PRIMARY KEY,
  name                   TEXT NOT NULL,
  type                   TEXT NOT NULL DEFAULT 'http',     -- P1: always 'http'
  url                    TEXT,                             -- nullable per blueprint; app enforces presence for type='http'
  method                 TEXT NOT NULL DEFAULT 'GET',
  headers                TEXT,                             -- JSON object or null
  body                   TEXT,
  auth_type              TEXT,                             -- none|basic|bearer|header
  auth_ref               TEXT,                             -- grammar in §6.2 (env:VAR | inline:<value>); never a keychain secret
  expected_status_codes  TEXT NOT NULL DEFAULT '200-299',
  interval_seconds       INTEGER NOT NULL DEFAULT 300,
  timeout_seconds        INTEGER NOT NULL DEFAULT 30,
  follow_redirects       INTEGER NOT NULL DEFAULT 1,
  verify_ssl             INTEGER NOT NULL DEFAULT 1,
  confirmation_threshold INTEGER NOT NULL DEFAULT 3,
  recovery_threshold     INTEGER NOT NULL DEFAULT 1,
  retry_interval_seconds INTEGER NOT NULL DEFAULT 30,
  status                 TEXT NOT NULL DEFAULT 'pending',  -- pending|up|down|paused|unknown
  is_paused              INTEGER NOT NULL DEFAULT 0,
  last_checked_at        INTEGER,
  next_run_at            INTEGER,
  consecutive_failures   INTEGER NOT NULL DEFAULT 0,
  consecutive_successes  INTEGER NOT NULL DEFAULT 0,
  tags                   TEXT,                             -- forward-compat: no tag filtering UI in P1
  sort_order             INTEGER NOT NULL DEFAULT 0,       -- forward-compat: reorder endpoint deferred; no P1 write path
  created_at             INTEGER NOT NULL,
  updated_at             INTEGER NOT NULL
);

CREATE TABLE checks (
  id               INTEGER PRIMARY KEY,
  monitor_id       INTEGER NOT NULL REFERENCES monitors(id) ON DELETE CASCADE,
  checked_at       INTEGER NOT NULL,
  status           TEXT NOT NULL,                          -- up|down  (raw probe outcome; not the uptime source)
  response_time_ms INTEGER,
  status_code      INTEGER,
  error_message    TEXT,
  resolved_ip      TEXT
);
CREATE INDEX idx_checks_monitor_time ON checks(monitor_id, checked_at DESC);

CREATE TABLE incidents (
  id               INTEGER PRIMARY KEY,
  monitor_id       INTEGER NOT NULL REFERENCES monitors(id) ON DELETE CASCADE,
  started_at       INTEGER NOT NULL,
  resolved_at      INTEGER,
  duration_seconds INTEGER,
  cause            TEXT,                                   -- timeout|status|connection|dns
  status_code      INTEGER,
  error_message    TEXT
);
CREATE INDEX idx_incidents_monitor ON incidents(monitor_id, started_at DESC);

CREATE TABLE notification_channels (
  id         INTEGER PRIMARY KEY,
  name       TEXT NOT NULL,
  type       TEXT NOT NULL,                                -- P1: 'email'
  config     TEXT NOT NULL,                                -- JSON: {host,port,security,from,to[]} — SOLE home of SMTP config; password NOT stored
  is_active  INTEGER NOT NULL DEFAULT 1,
  created_at INTEGER NOT NULL
);

CREATE TABLE monitor_notifications (
  monitor_id INTEGER NOT NULL REFERENCES monitors(id) ON DELETE CASCADE,
  channel_id INTEGER NOT NULL REFERENCES notification_channels(id) ON DELETE CASCADE,
  triggers   TEXT NOT NULL DEFAULT '["down","recovered"]',
  PRIMARY KEY (monitor_id, channel_id)
);

CREATE TABLE notification_log (
  id          INTEGER PRIMARY KEY,
  monitor_id  INTEGER,
  channel_id  INTEGER,
  incident_id INTEGER,
  trigger     TEXT,
  sent_at     INTEGER,
  success     INTEGER,
  error       TEXT
);
CREATE INDEX idx_notif_log_monitor_trigger ON notification_log(monitor_id, trigger, sent_at DESC);

CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT);   -- anchors, notify.cooldown_minutes, retention.raw_days, appearance.accent
```

**Retention & maintenance (P1):** a nightly maintenance task (a) deletes `checks` older than `retention.raw_days` (default 30), and (b) runs `PRAGMA incremental_vacuum` weekly to return freed pages to the OS (paired with `auto_vacuum=INCREMENTAL` set at DB creation — SQLite doesn't reclaim pages on `DELETE` alone). Daily rollups (`check_aggregates_daily`) are a P2 concern; P1 uptime tiles compute from **incidents** (§10.3), not raw checks. `incidents` and `notification_log` are kept indefinitely in P1 (both tiny); a `notification_log` retention policy can land with the re-notify/digest work.

**Forward-compat note:** the blueprint's `monitors.heartbeat_token` is `TEXT UNIQUE`. SQLite's `ALTER TABLE ADD COLUMN` cannot add an inline `UNIQUE` constraint, so the P4 migration must add `heartbeat_token` as a plain column and create uniqueness with a separate `CREATE UNIQUE INDEX`.

---

## 6. Check engine

### 6.1 Scheduler

An async delay-queue keyed by `next_run_at`. On startup, load all non-paused monitors; any with `next_run_at` in the past are scheduled immediately but **staggered** (spread over a few seconds) to avoid a thundering herd. Each scheduled run adds **±5% jitter**. A global `tokio::sync::Semaphore` (default 25 permits) bounds simultaneous probes. Creating/updating/pausing/deleting a monitor, or `check_now`, recomputes that monitor's queue entry immediately (via a control channel to the scheduler task).

### 6.2 HTTP prober (`probe::http`)

Build the request from the monitor: method, headers (JSON), body, auth. **`auth_ref` grammar (P1):** a small tagged string — `env:VAR_NAME` (resolve from the process environment at send time) or `inline:<value>` (a literal, non-secret value stored as-is). `auth_type` selects how the resolved value is applied (`basic` → `Authorization: Basic base64(user:pass)`, `bearer` → `Authorization: Bearer <v>`, `header` → a custom header). The schema comment "never a keychain secret" means P1 does not integrate the OS keychain; genuinely secret bearer tokens should use `env:` so they live in the container env/secret, not the DB. `test-check` (§9) honors the `auth_ref` carried in the DTO for an unsaved monitor.

Send via a shared `reqwest` client honoring `follow_redirects`, `verify_ssl`, and a hard `timeout_seconds`. Classify:

- **Success:** response received within timeout **and** status ∈ `expected_status_codes`.
- **Failure + cause:** `timeout` (elapsed), `connection` (connect/TLS/reset), `dns` (resolution), `status` (responded but code not expected). Record `status_code`, `error_message`, `response_time_ms`, `resolved_ip` where available.

`expected_status_codes` parsing (e.g. `200-299,301,418`) is a pure, unit-tested function.

### 6.3 Anchor gate (`anchor`)

Maintains a cached connectivity **verdict** (`online`/`offline`) with a **10-second TTL**, plus a background poller.

- **Probe:** TCP-connect to each anchor host (default `1.1.1.1:443`, `8.8.8.8:443`) with a short timeout. Verdict is **online if any anchor answers**, **offline if none do**.
- **Flip rule (hysteresis, distinct from caching):** to flip **online→offline**, one on-demand probe with all anchors failing is sufficient (two independent well-known hosts both unreachable is already strong evidence). To flip **offline→online**, one successful background poll is sufficient. The 10s TTL only governs how often the verdict is recomputed on demand, so a burst of monitor failures within 10s reuses one verdict rather than hammering the anchors.
- **On-demand consultation:** when a monitor reaches its confirmation threshold of consecutive failures, the engine asks the gate. **Online** → the failure is real → allow **DOWN**. **Offline** → drive the fleet UNKNOWN (below).
- **Fleet-wide UNKNOWN:** the moment the verdict is confirmed offline, **all non-paused monitors** are set to `UNKNOWN`, alerting is suppressed, and `connectivity:changed{online:false}` is emitted (this matches the single global connectivity banner). Monitors **keep probing on their normal schedule** during UNKNOWN (so response history continues and recovery is detected promptly) but no incidents open and no alerts fire.
- **Background poller & recovery:** while offline, a dedicated task polls the anchors **every 15s**, independent of any monitor schedule — this is what guarantees recovery even when every monitor is UNKNOWN and none is hitting the confirmation path. On the first successful poll → verdict `online`, emit `connectivity:changed{online:true}`; monitors re-evaluate on their next check per §3.3 (UNKNOWN → UP/DOWN, closing suspended incidents as needed).

---

## 7. State machine (`state`) — pure & TDD-first

A pure function with no I/O, exhaustively unit-tested. Inputs: current status, `prev_confirmed_state` (the last UP/DOWN before any UNKNOWN), `consecutive_failures`, `consecutive_successes`, the latest probe outcome, the anchor verdict, and thresholds. Output: next status + an optional `Transition` describing what the caller must persist/notify.

```
States: PENDING, UP, DOWN, PAUSED, UNKNOWN

PENDING --first success--> UP
PENDING/UP --failures ≥ threshold & anchors up--> DOWN            (open incident, notify: down)
any active --anchors offline--> UNKNOWN                           (fleet-wide; incident, if open, left OPEN/suspended; no notify)
DOWN    --successes ≥ recovery--> UP                              (close incident, notify: recovered)
UNKNOWN --anchors back & success & prev=DOWN--> UP                (close the suspended incident, notify: recovered)
UNKNOWN --anchors back & success & prev=UP--> UP                  (no incident, no notify)
UNKNOWN --anchors back & failure(confirmed) & anchors up--> DOWN  (prev=UP: open incident+notify; prev=DOWN: keep incident, no dup notify)
any     --pause--> PAUSED (no probing)      PAUSED --resume--> PENDING
```

Between first failure and the confirmation threshold the monitor **stays UP/PENDING** but reschedules at `retry_interval` (fast retry). This is the flap-prevention core; its tests — including every UNKNOWN edge and the DOWN→UNKNOWN→UP incident-closing path — are the anchor of P1 correctness.

---

## 8. Notifications (email only)

### 8.1 Channel & triggers

One channel type in P1: **email**. A `notification_channels` row is the **sole home** of SMTP config: `config` JSON holds `{host, port, security: none|starttls|tls, from, to:[...]}`; the **password is not stored** — the notifier reads it from the Docker secret at startup. Monitors attach to the channel via `monitor_notifications` with `triggers` (default `["down","recovered"]`).

### 8.2 Dispatch, cooldown, logging

On a `down`/`recovered` transition, `notify::dispatch` finds attached active channels whose triggers include the event, then checks the per-`(monitor, trigger)` **cooldown** (default **15 minutes**, `settings` key `notify.cooldown_minutes`) against `notification_log` (indexed by `(monitor_id, trigger, sent_at)`). **Semantic:** a send is suppressed only if the *same* `(monitor, trigger)` was sent within the window — so a rapid re-`down` inside 15 min is dampened, but a `recovered` is never blocked by a `down` cooldown (different trigger), and vice-versa. If clear, render and send via `notify::email`, then write a `notification_log` row (success/failure + error). Alerts are **fully suppressed** while connectivity is offline (UNKNOWN), independent of cooldown.

### 8.3 Templates

P1 message variables: `{{monitor_name}} {{url}} {{status}} {{status_code}} {{error}} {{response_time_ms}} {{duration}} {{checked_at}}`. Two default templates (down, recovered) with a subject + plaintext/HTML body; rendering is a pure, tested function. "Send test email" composes a sample message to verify SMTP end-to-end.

---

## 9. API & events

REST under `/api` (thin handlers → services → JSON). SSE at `/events`.

### 9.1 REST

```
Health      GET  /healthz                      -> 200 (target of the in-binary healthcheck subcommand)
Monitors    GET  /api/monitors                 -> [Monitor]
            GET  /api/monitors/:id             -> Monitor
            POST /api/monitors                 (dto) -> Monitor
            PUT  /api/monitors/:id             (dto) -> Monitor
            DEL  /api/monitors/:id
            POST /api/monitors/:id/pause | /resume | /check-now
            POST /api/monitors/test-check      (dto) -> probe result WITHOUT saving (honors DTO auth_ref)
Stats       GET  /api/monitors/:id/stats?range=24h|7d  -> {uptime_pct|null, downtime_seconds, avg_ms, incidents}
Channels    GET  /api/channels · POST · PUT/:id · DEL/:id
            POST /api/channels/:id/test        -> send a test email via this channel (the Settings 'Send test' button)
Settings    GET  /api/settings · PUT /api/settings   (anchors, cooldown, retention, appearance — NOT SMTP)
Events      GET  /events (SSE)
Heartbeat   GET|POST /ping/:token  -> 200 (route reserved; heartbeat monitors are P4)
```

There is **one** SMTP test path — `POST /api/channels/:id/test` — because SMTP config lives on the channel; the earlier `/api/settings/test-smtp` is removed to avoid a second source of truth.

### 9.2 SSE payloads

Mirror `CLAUDE.md` §10: `monitor:updated{id,status,response_time_ms,checked_at}`, `monitor:transition{id,from,to,incident_id}`, `incident:opened`/`incident:resolved`, `connectivity:changed{online}`.

### 9.3 SSE resync contract

To keep "never a full refetch on a tick" from causing stale drift: on **each new `/events` connection**, the server first sends a **full snapshot** (all monitors' current state + connectivity) as an initial event, then streams deltas, each carrying an incrementing `id:`. The browser's `EventSource` auto-reconnects; on reconnect it gets a fresh snapshot (and may send `Last-Event-ID`, honored best-effort). If a subscriber lags and the `broadcast` channel returns `Lagged`, the server pushes a fresh snapshot to that subscriber instead of silently dropping. This satisfies DoD item 3's re-sync requirement.

---

## 10. Frontend (SolidJS)

Built with Vite to static assets served by axum (SPA fallback per §3.2). Rich navy instrument aesthetic per `CLAUDE.md` §11, scoped to P1 screens.

### 10.1 Design tokens

Adopt §11.1 tokens with **`--bg-base: #06051e`** (the user's exact desktop navy); derive `--bg-sunken` darker and `--bg-surface`/`-2`/`-3` progressively lighter from it; keep hairline borders and the cool text ramp. **Accent is cyan `--accent: #3FC8E4`**, fixed for P1 (deliberately not green so status-green reads as status). The token is swappable at the CSS/build level, but the interactive picker/theme-switching UI is deferred to the theming phase (P4) — it configures nothing the P1 DoD exercises. Status colors (`--up/--down/--paused/--pending/--unknown`) per spec. Fonts: **Inter** (UI) + **JetBrains Mono** (metrics, `tnum`/`zero`). Motion honors `prefers-reduced-motion`.

### 10.2 Screens (P1)

- **Shell:** 72px left rail (Dashboard, Settings for P1; other nav present but stubbed) with a live global summary at its base (`N up · N down · N paused`, down count in `--down`, gentle pulse when > 0). Top bar: search, status filter chips, range selector (24h/7d), **+ Add monitor** (accent).
- **Monitor grid:** cards per §11.3 — status dot (breathing glow when UP, steady when DOWN, hollow when PAUSED), name, URL + type icon, response ms (mono), `uptime% · 24h/7d` badge, and a compact bar area (a simple last-N strip placeholder in P1; the true 90-day bar is P2). Whole card opens the detail panel; `⋯` quick menu (Check now · Pause · Edit · Delete).
- **Detail panel** (slide-from-right, 440px, dimmed backdrop; Esc/✕/backdrop to close): header (status pill, name, URL/type, quick actions), **now-strip** (status / response ms count-up / last-checked relative+absolute), **uptime tiles** (24h · 7d, §10.3, with period downtime beneath), and a collapsed **configuration** summary with **Edit**. **Deep-linkable** (relies on the SPA fallback).
- **Add/Edit monitor** (right-side panel, consistent): Basics (name, URL) · Schedule (interval chips + custom, timeout, confirmation threshold, retry interval) · Validation (expected status codes, follow redirects, verify SSL) · Advanced (method, headers rows, body, auth) · Notifications (attach email channel + down/recovered toggles) · **Live "Test check"** running a real probe and rendering status/code/ms inline before save.
- **Settings:** SMTP (edits the email channel's host/port/security/from/recipients + **Send test**; password shown as "managed via Docker secret"), **anchor hosts** editor, data retention days. (No accent/theme picker in P1.)
- **States:** empty ("Add your first monitor" + accent CTA), loading skeletons, inline error+retry, and the **connectivity-down banner** ("Your connection appears offline — alerting paused") while monitors sit in UNKNOWN.
- **A11y:** full keyboard nav, visible focus ring, ARIA live region announcing status transitions, AA contrast, reduced-motion respected, labels/tooltips on icon-only controls.

### 10.3 Uptime computation (`uptime`, pure)

Uptime and downtime for a window `[start, now]` are **time-weighted from incident spans**, not counted from raw `checks`:

- `downtime_seconds` = sum over the monitor's incidents of `overlap(incident_interval, window)`, where an open incident extends to `now`.
- `uptime_pct` = `(1 − downtime_seconds / window_seconds) × 100`.
- **Empty/sparse window:** if the monitor has produced **no checks at all** within the window (e.g. just created, or a long interval with no tick yet), render **"—"** rather than a misleading `100%`/`0%`. A monitor with checks but no incidents in the window is genuinely `100%`.
- Because incidents never open during UNKNOWN, your-own-outage failures never depress uptime. The same incident spans yield the "period downtime" shown beneath each tile.

---

## 11. Testing strategy

- **TDD for pure logic** (write tests first): the `state` transition function (every edge in §7, including all UNKNOWN paths and DOWN→UNKNOWN→UP incident-closing), `expected_status_codes` parsing, cooldown eligibility (same-trigger suppression, cross-trigger allowance), the anchor verdict + flip rule, `uptime` computation (time-weighted, open-incident-to-now, empty-window "—"), and template rendering. These are the correctness core and have zero I/O.
- **Prober tests:** integration tests against a local test HTTP server (e.g. `wiremock`/`axum` test server) covering success, wrong status, timeout, connection refused, redirect handling, and `verify_ssl` behavior — asserting the classified cause.
- **Engine integration:** an end-to-end up → down (confirmed after threshold) → recover cycle against the local test server, asserting incident open/close, `notification_log` entries (with a stub SMTP transport), and emitted events. A separate test forces the anchor gate offline and asserts fleet-wide UNKNOWN, suppressed alerts, a suspended-but-open incident, and correct recovery when the gate returns online.
- **DB tests:** apply migrations to a fresh SQLite file and assert the schema; a cascade test that deletes a monitor and asserts child rows are gone (proving `foreign_keys=ON` is actually set on pooled connections).
- **Frontend:** component sanity for the card/panel/form; the real acceptance is manual verification of the P1 Definition of Done against a live site (up → down → email → recovery) with `docker compose up -d`, plus an SSE reconnect check (§9.3).
- **CI-friendly:** all Rust tests run without network by using the local test server and stub SMTP; the anchor gate is injectable for tests.

---

## 12. Decisions log (this session)

| # | Decision | Choice |
|---|---|---|
| 1 | Build scope | **P1 only**; P2–P4 deferred to their own cycles. |
| 2 | Stack | **Containerized single Rust binary** (axum + SolidJS static UI + SSE), run via `docker compose up -d`. Tauri dropped. |
| 3 | Ping strategy | TCP-ping only (relevant P2; no raw-socket/CAP_NET_RAW). |
| 4 | Canvas color | `--bg-base: #06051e` (user's desktop navy); elevations derived. |
| 5 | Accent | Cyan `#3FC8E4`, **fixed in P1**; interactive picker deferred to the theming phase. |
| 6 | P1 detail panel | Now-strip + 24h/7d uptime tiles (from incident spans) + collapsed config. |
| 7 | Alert channels (P1) | **Email only.** Desktop/ntfy/etc. deferred (no container desktop). |
| 8 | Secret storage | **Docker secret file** `/run/secrets/smtp_password` read at startup; DB stores no secret. Rotation needs redeploy. |
| 9 | Network bind | `0.0.0.0:8080`, **LAN-accessible**; no auth in P1 (single trusted operator). |
| 10 | Live updates | **SSE** with snapshot-on-connect + `Lagged`→snapshot resync (§9.3). |
| 11 | SMTP config home | `notification_channels.config` only (single source of truth); one test endpoint `/api/channels/:id/test`. |
| 12 | Notify cooldown | Default **15 min**, per-(monitor, trigger); same-trigger suppression only. |
| 13 | Uptime source | Time-weighted from **incidents**, not raw `checks`; empty window → "—". |
| 14 | Connectivity model | Anchor-offline ⇒ **fleet-wide UNKNOWN**, monitors keep probing, 15s background recovery poll. |

---

## 13. Build order (for the implementation plan)

1. Repo scaffold: Rust workspace + Vite/SolidJS app; `Dockerfile` (non-root, `/data` pre-owned, rustls, ca-certificates), `docker-compose.yml` (in-binary healthcheck, secret, volume), `.env.example`, secret placeholder; `/healthz` + the `vigil healthcheck` subcommand.
2. `db` + migrations + `models`; `schema_migrations` runner; pool with `foreign_keys=ON`/WAL/`auto_vacuum`.
3. `state` machine + `expected_status_codes` parser + `uptime` computation **(TDD)**.
4. `probe::http` (+ `auth_ref` grammar) + prober tests (local test server).
5. `scheduler` + `worker` + concurrency; catch-up on restart.
6. `anchor` gate (verdict/TTL/flip rule + 15s background poller) + fleet-wide UNKNOWN integration.
7. `incidents` open/close on transitions (incl. suspended-through-UNKNOWN).
8. `notify::email` (lettre rustls) + `dispatch` + cooldown + `notification_log`; templates.
9. `settings_store` (point-of-use reads); `api` REST + `events` SSE (snapshot-on-connect) + `static_assets` SPA fallback.
10. Frontend: tokens/shell → grid → detail panel → Add/Edit + Test check → Settings.
11. Nightly maintenance (retention delete + weekly incremental_vacuum).
12. End-to-end acceptance against a live site via `docker compose up -d`; verify the Definition of Done incl. SSE reconnect and the your-connection-down path.

---

## 14. (reserved)

---

## 15. Changelog — v2 (spec-review hardening)

Applied from the six-lens adversarial review. **Must-fix (10):** single SMTP config home (channel, not settings) + one test endpoint (§4.2/§5/§8.1/§9); UNKNOWN→UP closes suspended incident + fires recovered (§3.3/§7); in-binary healthcheck vs shell-less runtime (§4.1); non-root `/data` ownership for WAL (§4.1); SSE snapshot-on-connect + `Lagged` resync (§9.3); `url` nullable to avoid a P2 table rebuild (§5); `foreign_keys=ON` per connection for cascades (§3.2/§5); concrete cooldown default 15 min (§4.2/§8.2); anchor gate background poll + fleet-wide UNKNOWN semantics (§6.3); uptime from incident spans + empty-window rule (§10.3). **Should-fix (8):** lettre on rustls-tls (§4.1); weekly `incremental_vacuum` + `auto_vacuum` (§5); `notification_log` index (§5); SPA fallback for deep links (§3.2/§10.2); `auth_ref` grammar (§6.2); Docker-secret readability requirement + clear error (§4.3); anchor cache TTL + explicit flip rule (§6.3); `tags`/`sort_order` marked forward-compat (§5). **Optional (2):** accent picker deferred, fixed cyan in P1 (§10.1); `heartbeat_token` P4-migration note (§5). **Completeness note:** DB-backed settings read at point-of-use, not frozen at startup (§3.2/§4.2); raw `checks` during UNKNOWN don't corrupt uptime because uptime derives from incidents (§3.3/§10.3).

*End of P1 design spec.*
