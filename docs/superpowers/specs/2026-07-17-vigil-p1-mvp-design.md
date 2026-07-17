# Vigil P1 (MVP) — Design Spec

> **Status:** approved for planning · **Date:** 2026-07-17
> **Scope:** Phase 1 (MVP) only. P2–P4 get their own brainstorm → spec → build cycles.
> **Parent spec:** [`CLAUDE.md`](../../../CLAUDE.md) (full product blueprint) · **UI reference:** [`docs/vigil_dashboard_mock.html`](../../vigil_dashboard_mock.html)

This document is the build-ready design for Vigil's first shippable slice. It carves a clean P1 subset out of the full blueprint and records the architectural decisions that diverge from it. Where this document and `CLAUDE.md` disagree, **this document wins for P1**; `CLAUDE.md` remains the north star for later phases.

---

## 1. P1 Definition of Done

Point Vigil at a real website. When the site goes down, Vigil detects it — *confirmed*, not fooled by a single blip and not fooled by **your own** connection dropping — opens an incident, and sends you an **email**. When the site recovers, Vigil closes the incident and sends a recovery email. The whole thing runs as a background container you started with `docker compose up -d`, survives reboots, and shows a live navy dashboard in your browser.

Concretely, P1 is done when all of these are true:

1. `docker compose up -d` brings up a healthy container; the dashboard loads at `http://<host>:8080`.
2. You can add, edit, pause, resume, delete, and manually re-check an HTTP(S) monitor from the UI.
3. A monitor pointed at a reachable site reads **UP**; response time and last-checked update live (via SSE) without a page refresh.
4. Taking the target down drives the monitor to **DOWN** only after the confirmation threshold, opening an incident and sending one **down** email.
5. Killing *your* internet (not the target) drives affected monitors to **UNKNOWN** with a connectivity banner and **no** false down-emails; restoring it resumes normal evaluation.
6. Recovery sends one **recovered** email and closes the incident with a computed duration.
7. State survives `docker compose restart` — monitors, incidents, and schedules resume; past-due checks catch up staggered.

---

## 2. Scope

### 2.1 In scope (P1)

- **Monitor type:** HTTP(S) only — method (GET/POST/HEAD), request headers, body, basic/bearer/header auth, `expected_status_codes` (CSV of codes/ranges, default `200-299`), `timeout_seconds`, `follow_redirects`, `verify_ssl`.
- **Scheduler:** per-monitor interval (presets `30s,1m,2m,5m,10m,15m,30m,1h,6h,12h,24h` + custom seconds, **15s floor**), ±5% jitter on `next_run_at`, global concurrency semaphore (default 25), catch-up (staggered) on restart for past-due monitors.
- **State machine:** states `PENDING → UP | DOWN`, plus `PAUSED` and `UNKNOWN`. Confirmation threshold (default 3) with fast retry cadence (`retry_interval_seconds`, default 30) after first failure; recovery confirmation (default 1). **Anchor internet-sanity gate** before confirming any DOWN.
- **Anchor gate:** before a monitor is confirmed DOWN, probe configurable anchors (default `1.1.1.1:443`, `8.8.8.8:443`). If anchors are unreachable → affected monitors become `UNKNOWN`, alerting suppressed, `connectivity:changed{online:false}` emitted; resume when anchors return.
- **Incidents:** open on entering DOWN (`started_at`, `cause`, `status_code`/`error_message`), close on return to UP (`resolved_at`, computed `duration_seconds`).
- **Storage:** SQLite (WAL) on a mounted volume. Tables: `monitors`, `checks`, `incidents`, `notification_channels`, `monitor_notifications`, `notification_log`, `settings`, plus a `schema_migrations` bookkeeping table.
- **Notifications:** **Email (SMTP) only.** Triggers `down` and `recovered`. Per-`(monitor, trigger)` cooldown to prevent duplicate sends. "Send test email" action. Message templating with the P1 variable subset.
- **UI (SolidJS, served by axum):** left rail + top bar; **monitor grid** with status-dot cards (status, response ms, uptime badge, compact bar placeholder); **detail panel** (header + now-strip + 24h/7d uptime tiles + collapsed config summary); **Add/Edit monitor** panel with live **Test check**; **Settings** (SMTP config, anchor hosts, appearance/accent); empty/loading/error states; **connectivity-down banner**. Live updates via SSE.
- **Deployment:** multi-stage `Dockerfile`, `docker-compose.yml` (one service + named volume + Docker secret), `.env.example`, healthcheck, `restart: unless-stopped`.

### 2.2 Out of scope (deferred to later phases)

Response-time charts and the 90-day uptime bar with daily rollups (`check_aggregates_daily`); keyword / TCP-port / DNS / ping / heartbeat / SSL-only monitor types; SSL certificate and domain-expiry tracking (`ssl_certs`, `domain_info`); webhook/Discord/Slack/Telegram/ntfy/Pushover channels and desktop/browser push; maintenance windows; monthly incident reports; list view; incident acknowledge + re-notify + daily digest; `DEGRADED` and `MAINTENANCE` states; multi-user/auth. These remain specified in `CLAUDE.md` for P2–P4.

> **Note on desktop notifications:** the full spec's P1 lists "desktop + email." Native desktop notifications require a logged-in desktop session and do not exist inside a container; they are intentionally dropped from this containerized P1. Email is the reliable 24/7 backbone. Push channels (ntfy/Discord/webhook) arrive in P3.

---

## 3. Architecture

### 3.1 Container model

Vigil P1 is a **single Rust binary** run as **one container**. `axum` serves four surfaces from one process: the REST API, an SSE event stream, the static SolidJS UI, and the `/ping/{token}` heartbeat endpoint (endpoint stubbed/reserved in P1 — heartbeat *monitors* are P4, but the route exists so the port contract is stable). The check engine (scheduler + worker pool + state machine + notifier) runs as `tokio` tasks in the same process. SQLite lives on a mounted named volume.

```
docker compose up -d
 └─ service: vigil  (one Rust binary, one container)
     ├─ axum ────────── REST API  +  SSE /events  +  static SolidJS UI  +  /ping/{token} (reserved)
     ├─ scheduler ───── delay-queue, ±5% jitter, concurrency semaphore (default 25)
     ├─ worker pool ─── reqwest HTTP probes with per-monitor timeout
     ├─ state machine ─ PENDING→UP|DOWN + PAUSED + UNKNOWN; confirmation/recovery; anchor gate
     ├─ notifier ────── email (SMTP via lettre)
     └─ SQLite (WAL) ── /data/vigil.db on a mounted named volume
     bind 0.0.0.0:8080  → compose maps host 8080 (LAN-accessible)
     restart: unless-stopped ; secret smtp_password → /run/secrets/smtp_password
```

**Why a container instead of the spec's Tauri desktop app:** a container runs 24/7 regardless of desktop-session state and restarts on boot — strictly better for an always-on monitor. The trade-offs (no native notifications, no OS keychain) are handled by email-only alerting and Docker-secret-based secret storage, below.

### 3.2 Rust module layout

Each module has one job and a well-defined interface. Pure-logic modules (`state`, `anchor` decision, `cooldown`, status-code parsing, template rendering) are unit-tested in isolation.

| Module | Responsibility | Key dependencies |
|---|---|---|
| `main` | Wire everything: load config, open DB, run migrations, spawn engine tasks, start axum. | all |
| `config` | Load runtime config from env + Docker secrets at startup (bind addr, DB path, anchors default, SMTP secret). | — |
| `db` | `sqlx` SQLite pool (WAL), migration runner, typed queries. | `sqlx` |
| `models` | Domain types + DTOs (Monitor, Check, Incident, Channel, Settings) + serde. | `serde` |
| `secrets` | Read `/run/secrets/*` (fallback env) at startup; expose SMTP password to notifier. Never persisted to DB. | — |
| `scheduler` | Delay-queue of `next_run_at`; jitter; catch-up; recompute on config change; hand jobs to workers under the semaphore. | `tokio` |
| `worker` | Execute one probe with timeout; write `checks` row; hand result to `state`. | `tokio` |
| `probe::http` | The HTTP prober: build request (method/headers/body/auth), send via `reqwest`, classify success vs failure + cause. | `reqwest`, `rustls` |
| `state` | **Pure** transition function: `(current, streaks, result, anchor_ok) → (new_state, transition?)`. No I/O. | — |
| `anchor` | Probe anchor hosts (TCP connect); cache online/offline with hysteresis; gate DOWN confirmation. | `tokio` |
| `incidents` | Open/close incidents on transitions; compute duration. | `db` |
| `notify::email` | Compose + send SMTP mail via `lettre`; render templates. | `lettre` |
| `notify::dispatch` | Map a transition → eligible channels + triggers; apply cooldown; write `notification_log`. | `db` |
| `api` | axum routers: REST handlers (thin — validate DTO, call service, return JSON) + SSE. | `axum` |
| `events` | In-process broadcast bus (`tokio::sync::broadcast`) fanned out to SSE subscribers. | `tokio` |
| `static_assets` | Serve embedded/built SolidJS bundle. | `axum` |

### 3.3 Data flow per check

`scheduler` fires when `next_run_at ≤ now` → job enqueued, acquires a semaphore permit → `worker` runs `probe::http` with timeout → result row written to `checks` → `state` evaluates the transition from `(status, consecutive_failures, consecutive_successes, anchor_ok)`:

- **Success** → increment success streak; if currently DOWN and streak ≥ recovery threshold → transition **UP** (close incident, dispatch `recovered`). Reset failure streak. Schedule next run at normal interval.
- **Failure** → increment failure streak. If streak < confirmation threshold → stay UP/PENDING, schedule next run at `retry_interval` (fast retry). If streak ≥ threshold → **consult anchor gate**: anchors up → transition **DOWN** (open incident, dispatch `down`); anchors down → set **UNKNOWN**, suppress alerts, emit `connectivity:changed{online:false}`. Reset success streak.

Every write emits `monitor:updated`; every state change also emits `monitor:transition` (and `incident:opened`/`incident:resolved`). SSE fans these to the browser, which re-renders only the affected card. A config edit recomputes that monitor's queue entry immediately.

---

## 4. Deployment & configuration

### 4.1 Files

- **`Dockerfile`** — multi-stage: (1) build the SolidJS UI with Node; (2) build the Rust binary with the UI embedded (or copied to a served dir); (3) slim runtime image (Debian-slim or distroless) running the binary as a non-root user. CA certificates included for TLS probing/SMTP.
- **`docker-compose.yml`** — one `vigil` service: builds/pulls the image, maps `8080:8080` on `0.0.0.0`, mounts a named volume `vigil-data:/data`, declares the `smtp_password` secret, sets `restart: unless-stopped`, and a `healthcheck` hitting `GET /healthz`.
- **`.env.example`** — documents env vars (below) with safe defaults.
- **`secrets/smtp_password.example`** — placeholder documenting the Docker secret file.

### 4.2 Configuration surface

Read once at startup (`config` module). Non-secret operational config via env with defaults; the SMTP password only via the Docker secret file.

| Setting | Source | Default | Notes |
|---|---|---|---|
| Bind address | env `VIGIL_BIND` | `0.0.0.0:8080` | LAN-accessible per decision. |
| DB path | env `VIGIL_DB` | `/data/vigil.db` | On the mounted volume. |
| Anchor hosts | env `VIGIL_ANCHORS` (also editable in Settings, DB-backed) | `1.1.1.1:443,8.8.8.8:443` | Settings value overrides env once set. |
| Global probe concurrency | env `VIGIL_MAX_CONCURRENCY` | `25` | Semaphore size. |
| SMTP password | file `/run/secrets/smtp_password` (fallback env `VIGIL_SMTP_PASSWORD` for dev) | — | Never written to DB. |
| SMTP host/port/security/from/to | DB `settings` (edited in UI) | — | Non-secret; UI-managed. |

**SMTP password lifecycle:** supplied only as a Docker secret read at boot. The Settings UI shows SMTP host/port/security/from/recipients (editable) and displays *"Password managed via Docker secret"* with no password field. Rotating the password means editing `secrets/smtp_password` and re-running `docker compose up -d`. This is documented in the Settings UI and README.

### 4.3 Network & security posture (P1)

- Binds `0.0.0.0:8080`; compose maps host `8080`. Reachable from the LAN (dashboard + reserved heartbeat route).
- **No authentication** in P1 (spec non-goal §1: single trusted operator on a trusted network). Documented as such; a simple access token is a candidate for a later phase if the user exposes it beyond LAN.
- `verify_ssl` defaults on for probes.
- Runs as non-root in the container; SQLite file permissions restricted to the app user.
- Secret never logged, never returned by any API, never stored in DB.

---

## 5. Data model (P1 subset)

SQLite, WAL mode. DDL below is the P1 slice; columns not used by P1 features are omitted from `CLAUDE.md`'s definitions to keep the MVP tight but are **forward-compatible** (later phases add columns/tables via migrations). Timestamps are UNIX epoch seconds (INTEGER) unless noted.

```sql
CREATE TABLE schema_migrations (version INTEGER PRIMARY KEY, applied_at INTEGER NOT NULL);

CREATE TABLE monitors (
  id                     INTEGER PRIMARY KEY,
  name                   TEXT NOT NULL,
  type                   TEXT NOT NULL DEFAULT 'http',     -- P1: always 'http'
  url                    TEXT NOT NULL,
  method                 TEXT NOT NULL DEFAULT 'GET',
  headers                TEXT,                             -- JSON object or null
  body                   TEXT,
  auth_type              TEXT,                             -- none|basic|bearer|header
  auth_ref               TEXT,                             -- reference, never the secret
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
  tags                   TEXT,                             -- JSON array or null
  sort_order             INTEGER NOT NULL DEFAULT 0,
  created_at             INTEGER NOT NULL,
  updated_at             INTEGER NOT NULL
);

CREATE TABLE checks (
  id               INTEGER PRIMARY KEY,
  monitor_id       INTEGER NOT NULL REFERENCES monitors(id) ON DELETE CASCADE,
  checked_at       INTEGER NOT NULL,
  status           TEXT NOT NULL,                          -- up|down  (P1: no degraded)
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
  config     TEXT NOT NULL,                                -- JSON: host,port,security,from,to[]; password NOT stored
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

CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT);   -- smtp.*, anchors, theme, accent, retention
```

**Retention (P1):** a lightweight nightly task deletes `checks` older than `raw_retention_days` (default 30). Daily rollups (`check_aggregates_daily`) are a P2 concern; P1 uptime tiles compute directly from raw `checks` over the requested window. `incidents` are kept indefinitely.

---

## 6. Check engine

### 6.1 Scheduler

An async delay-queue keyed by `next_run_at`. On startup, load all non-paused monitors; any with `next_run_at` in the past are scheduled immediately but **staggered** (spread over a few seconds) to avoid a thundering herd. Each scheduled run adds **±5% jitter**. A global `tokio::sync::Semaphore` (default 25 permits) bounds simultaneous probes. Creating/updating/pausing/deleting a monitor, or `check_now`, recomputes that monitor's queue entry immediately (via a control channel to the scheduler task).

### 6.2 HTTP prober (`probe::http`)

Build the request from the monitor: method, headers (JSON), body, auth (basic/bearer/custom header — auth material resolved via `auth_ref`; in P1 basic/bearer values may be inline non-secret or env-provided, no per-monitor keychain). Send via a shared `reqwest` client honoring `follow_redirects`, `verify_ssl`, and a hard `timeout_seconds`. Classify:

- **Success:** response received within timeout **and** status ∈ `expected_status_codes`.
- **Failure + cause:** `timeout` (elapsed), `connection` (connect/TLS/reset), `dns` (resolution), `status` (responded but code not expected). Record `status_code`, `error_message`, `response_time_ms`, `resolved_ip` where available.

`expected_status_codes` parsing (e.g. `200-299,301,418`) is a pure, unit-tested function.

### 6.3 Anchor gate (`anchor`)

Maintains a cached connectivity verdict. When a monitor reaches the confirmation threshold of consecutive failures, the engine asks the anchor gate: are the anchors reachable? The gate TCP-connects to each anchor host (default `1.1.1.1:443`, `8.8.8.8:443`) with a short timeout; **online** if any anchor answers. To avoid flapping the whole fleet on a single blip, the gate applies light hysteresis (a fresh probe on demand, cached briefly). Verdict:

- **Anchors online** → the monitor's own failure is real → allow **DOWN**.
- **Anchors offline** → *our* connectivity is down → set affected monitors to **UNKNOWN**, suppress all alerting, emit `connectivity:changed{online:false}`. On the next successful anchor probe, emit `online:true`; monitors re-evaluate on their next check.

---

## 7. State machine (`state`) — pure & TDD-first

A pure function with no I/O, exhaustively unit-tested. Inputs: current status, `consecutive_failures`, `consecutive_successes`, the latest probe outcome, the anchor verdict, and thresholds. Output: next status + an optional `Transition` describing what the caller must persist/notify.

```
States: PENDING, UP, DOWN, PAUSED, UNKNOWN

PENDING --first success--> UP
PENDING --failures ≥ threshold & anchors up--> DOWN (open incident, notify: down)
UP      --failures ≥ threshold & anchors up--> DOWN (open incident, notify: down)
UP/DOWN --failures ≥ threshold & anchors DOWN--> UNKNOWN (no incident, no notify)
DOWN    --successes ≥ recovery--> UP (close incident, notify: recovered)
UNKNOWN --anchors back & success--> UP  |  --anchors back & failure(confirmed)--> DOWN
any     --pause--> PAUSED (no probing)   PAUSED --resume--> PENDING
```

Between first failure and the confirmation threshold the monitor **stays UP/PENDING** but reschedules at `retry_interval` (fast retry). This is the flap-prevention core; its tests are the anchor of P1 correctness.

---

## 8. Notifications (email only)

### 8.1 Channel & triggers

One channel type in P1: **email**. A `notification_channels` row holds `{host, port, security: none|starttls|tls, from, to:[...]}` in `config` JSON; the **password is not stored** — the notifier reads it from the Docker secret at startup. Monitors attach to the channel via `monitor_notifications` with `triggers` (default `["down","recovered"]`).

### 8.2 Dispatch, cooldown, logging

On a `down`/`recovered` transition, `notify::dispatch` finds attached active channels whose triggers include the event, checks the per-`(monitor, trigger)` **cooldown** (default N minutes, from settings) against `notification_log`, and if clear, renders and sends via `notify::email`, then writes a `notification_log` row (success/failure + error). Cooldown prevents duplicate sends (e.g. a rapid down→up→down). Alerts are **fully suppressed** while connectivity is offline (UNKNOWN).

### 8.3 Templates

P1 message variables: `{{monitor_name}} {{url}} {{status}} {{status_code}} {{error}} {{response_time_ms}} {{duration}} {{checked_at}}`. Two default templates (down, recovered) with a subject + plaintext/HTML body; rendering is a pure, tested function. "Send test email" composes a sample message to verify SMTP end-to-end.

---

## 9. API & events

REST under `/api` (thin handlers → services → JSON). SSE at `/events`.

```
Health      GET  /healthz                      -> 200 (compose healthcheck)
Monitors    GET  /api/monitors                 -> [Monitor]
            GET  /api/monitors/:id             -> Monitor
            POST /api/monitors                 (dto) -> Monitor
            PUT  /api/monitors/:id             (dto) -> Monitor
            DEL  /api/monitors/:id
            POST /api/monitors/:id/pause | /resume | /check-now
            POST /api/monitors/test-check      (dto) -> probe result WITHOUT saving
Stats       GET  /api/monitors/:id/stats?range=24h|7d  -> {uptime_pct, avg_ms, incidents}
Channels    GET  /api/channels · POST · PUT/:id · DEL/:id · POST /api/channels/:id/test
Settings    GET  /api/settings · PUT /api/settings · POST /api/settings/test-smtp
Events      GET  /events (SSE): monitor:updated, monitor:transition,
                                 incident:opened, incident:resolved, connectivity:changed
Heartbeat   GET|POST /ping/:token  -> 200 (route reserved; heartbeat monitors are P4)
```

**SSE payloads** mirror `CLAUDE.md` §10: `monitor:updated{id,status,response_time_ms,checked_at}`, `monitor:transition{id,from,to,incident_id}`, `connectivity:changed{online}`. The browser keeps an `EventSource` open and patches only the affected card/panel — never a full refetch on a single tick.

---

## 10. Frontend (SolidJS)

Built with Vite to static assets served by axum. Rich navy instrument aesthetic per `CLAUDE.md` §11, scoped to P1 screens.

### 10.1 Design tokens

Adopt §11.1 tokens with **`--bg-base: #06051e`** (the user's exact desktop navy); derive `--bg-sunken` darker and `--bg-surface`/`-2`/`-3` progressively lighter from it; keep hairline borders and the cool text ramp. **Accent stays cyan `--accent: #3FC8E4`** (deliberately not green so status-green reads as status), user-swappable in Settings. Status colors (`--up/--down/--paused/--pending/--unknown`) per spec. Fonts: **Inter** (UI) + **JetBrains Mono** (metrics, `tnum`/`zero`). Motion honors `prefers-reduced-motion`.

### 10.2 Screens (P1)

- **Shell:** 72px left rail (Dashboard, Settings for P1; Incidents/Notifications/etc. present but may be stubbed) with a live global summary at its base (`N up · N down · N paused`, down count in `--down`, gentle pulse when > 0). Top bar: search, status filter chips, range selector (24h/7d), **+ Add monitor** (accent).
- **Monitor grid:** cards per §11.3 — status dot (breathing glow when UP, steady when DOWN, hollow when PAUSED), name, URL + type icon, response ms (mono), `uptime% · 24h/7d` badge, and a compact bar area (rendered as a simple placeholder/last-N strip in P1; the true 90-day bar is P2). Whole card opens the detail panel; `⋯` quick menu (Check now · Pause · Edit · Delete).
- **Detail panel** (slide-from-right, 440px, dimmed backdrop; Esc/✕/backdrop to close): header (status pill, name, URL/type, quick actions), **now-strip** (status / response ms count-up / last-checked relative+absolute), **uptime tiles** (24h · 7d from raw checks with period downtime beneath), and a collapsed **configuration** summary with **Edit**. Deep-linkable.
- **Add/Edit monitor** (right-side panel, consistent): Basics (name, URL) · Schedule (interval chips + custom, timeout, confirmation threshold, retry interval) · Validation (expected status codes, follow redirects, verify SSL) · Advanced (method, headers rows, body, auth) · Notifications (attach email channel + down/recovered toggles) · **Live "Test check"** running a real probe and rendering status/code/ms inline before save.
- **Settings:** SMTP (host/port/security/from/recipients + **Send test**; password shown as "managed via Docker secret"), **anchor hosts** editor, **appearance** (accent picker: cyan default / The Open yellow / custom), data retention days.
- **States:** empty ("Add your first monitor" + accent CTA), loading skeletons, inline error+retry, and the **connectivity-down banner** ("Your connection appears offline — alerting paused") while monitors sit in UNKNOWN.
- **A11y:** full keyboard nav, visible focus ring, ARIA live region announcing status transitions, AA contrast, reduced-motion respected, labels/tooltips on icon-only controls.

---

## 11. Testing strategy

- **TDD for pure logic** (write tests first): the `state` transition function (every edge in §7), `expected_status_codes` parsing, cooldown eligibility, anchor verdict → decision mapping, and template rendering. These are the correctness core and have zero I/O.
- **Prober tests:** integration tests against a local test HTTP server (e.g. `wiremock`/`axum` test server) covering success, wrong status, timeout, connection refused, redirect handling, and `verify_ssl` behavior — asserting the classified cause.
- **Engine integration:** an end-to-end up → down (confirmed after threshold) → recover cycle against the local test server, asserting incident open/close, `notification_log` entries (with a stub SMTP transport), and emitted events. A separate test forces the anchor gate offline and asserts UNKNOWN + suppressed alerts.
- **Migrations:** a test that applies migrations to a fresh SQLite file and asserts the schema.
- **Frontend:** component sanity for the card/panel/form; the real acceptance is manual verification of the P1 Definition of Done against a live site (up → down → email → recovery) with `docker compose up -d`.
- **CI-friendly:** all Rust tests run without network by using the local test server and stub SMTP; anchor gate is injectable for tests.

---

## 12. Decisions log (this session)

| # | Decision | Choice |
|---|---|---|
| 1 | Build scope | **P1 only**; P2–P4 deferred to their own cycles. |
| 2 | Stack | **Containerized single Rust binary** (axum + SolidJS static UI + SSE), run via `docker compose up -d`. Tauri dropped. |
| 3 | Ping strategy | TCP-ping only (relevant P2; no raw-socket/CAP_NET_RAW). |
| 4 | Canvas color | `--bg-base: #06051e` (user's desktop navy); elevations derived. |
| 5 | Accent | Cyan `#3FC8E4` (default, user-swappable). |
| 6 | P1 detail panel | Now-strip + 24h/7d uptime tiles + collapsed config (from raw checks). |
| 7 | Alert channels (P1) | **Email only.** Desktop/ntfy/etc. deferred (no container desktop). |
| 8 | Secret storage | **Docker secret file** `/run/secrets/smtp_password` read at startup; DB stores no secret. Rotation needs redeploy. |
| 9 | Network bind | `0.0.0.0:8080`, **LAN-accessible**; no auth in P1 (single trusted operator). |
| 10 | Live updates | **SSE** (server→browser), one-way, patches affected cards only. |

---

## 13. Build order (for the implementation plan)

1. Repo scaffold: Rust workspace + Vite/SolidJS app; `Dockerfile`, `docker-compose.yml`, `.env.example`, secret placeholder; `/healthz`.
2. `db` + migrations + `models`; `schema_migrations` runner.
3. `state` machine + `expected_status_codes` parser **(TDD)**.
4. `probe::http` + prober tests (local test server).
5. `scheduler` + `worker` + concurrency; catch-up on restart.
6. `anchor` gate + integration into confirmation path.
7. `incidents` open/close on transitions.
8. `notify::email` + `dispatch` + cooldown + `notification_log`; templates.
9. `api` REST + `events` SSE + static asset serving.
10. Frontend: tokens/shell → grid → detail panel → Add/Edit + Test check → Settings.
11. End-to-end acceptance against a live site via `docker compose up -d`; verify the Definition of Done.

*End of P1 design spec.*
