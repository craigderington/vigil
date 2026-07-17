# VIGIL — Self-Hosted Uptime & Certificate Monitor
### Complete Design & Technical Specification

> Working title: **Vigil**. A single-user desktop app that replaces UptimeRobot for personal use — HTTP/port/DNS/heartbeat monitoring, SSL + domain-expiry tracking, email + desktop alerts, and a genuinely beautiful right-side detail-panel UI.
> Target: one operator, self-hosted, no subscription. Everything runs locally.

---

## 1. Goals & Non-Goals

**Goals**
- Monitor an arbitrary set of endpoints on independent schedules and detect up/down/degraded transitions reliably (no false alarms from flapping or from *your* connection dropping).
- First-class **SSL certificate** and **domain-registration expiry** tracking with tiered advance warnings.
- Multiple check methods (HTTP/S, keyword, TCP port, ICMP/TCP ping, DNS, heartbeat/push).
- Notifications via **email (SMTP)** and **native desktop**, extensible to webhook/Discord/Slack/Telegram/ntfy.
- An **instrument-grade UI**: dark, calm, dense-when-needed, with slide-from-right detail panels and the signature 90-day uptime bar.

**Non-Goals (v1)**
- Multi-user / auth / RBAC. Single trusted operator.
- Public status pages (can be a later add-on).
- Distributed multi-region probing. Checks originate from the one host running the app.

---

## 2. Recommended Architecture

Stack is a recommendation; the data model, engine, and UI spec are framework-independent.

| Layer | Recommendation | Rationale |
|---|---|---|
| Shell | **Tauri 2** (Rust core + web frontend) | Tiny binary, native notifications, secure secret storage, Rust async for the check engine. Electron is a drop-in alternative if preferred. |
| Frontend | **SolidJS** or **Svelte** + Vite | Snappy, small, great for a live-updating dashboard. React works too. |
| Backend / engine | **Rust** (`tokio`, `reqwest`, `rustls`, `trust-dns`, `hickory-resolver`) | One async runtime for scheduling + probing. |
| Storage | **SQLite** (WAL mode) via `sqlx`/`rusqlite` | Zero-admin, single file, easy backup. |
| Heartbeat receiver | Embedded **`axum`** HTTP listener | Accepts push pings from your cron jobs. |
| Charts | `uPlot` (tiny, fast) or ECharts | Response-time + uptime visualizations. |

**Process model**

```
┌─────────────────────────────────────────────────────────────┐
│ Desktop App (single process)                                 │
│                                                              │
│  ┌────────────┐   IPC (Tauri commands / events)   ┌────────┐ │
│  │  Frontend  │ ◄────────────────────────────────►│  Core  │ │
│  │ (SolidJS)  │        live status via events      │ (Rust) │ │
│  └────────────┘                                    └───┬────┘ │
│                                                        │      │
│         ┌──────────────┬──────────────┬───────────────┤      │
│         ▼              ▼              ▼                ▼      │
│   ┌──────────┐  ┌────────────┐  ┌──────────┐   ┌──────────┐  │
│   │Scheduler │  │Check Worker│  │Notifier  │   │  axum    │  │
│   │(delay-q) │─►│ pool (N)   │─►│dispatcher│   │heartbeat │  │
│   └────┬─────┘  └─────┬──────┘  └────┬─────┘   └────┬─────┘  │
│        └──────────────┴─────────────┴──────────────┘         │
│                          ▼                                   │
│                   ┌────────────┐                             │
│                   │  SQLite    │                             │
│                   └────────────┘                             │
└─────────────────────────────────────────────────────────────┘
```

**Data flow per check:** Scheduler fires when `next_run_at` elapses → job enqueued → a worker (bounded concurrency) runs the probe with timeout → result written → **state machine** evaluates transition → if a transition warrants it, the notifier dispatches to that monitor's channels → frontend receives a `monitor:updated` event and re-renders. Config edits recompute the scheduler queue immediately.

---

## 3. Monitor Types (Check Methods)

Each monitor has a `type`. Fields not relevant to a type are ignored.

| Type | Probe | Success criteria | Key config |
|---|---|---|---|
| **HTTP(S)** | GET/POST/HEAD via reqwest | status in `expected_status_codes` (default `200–299`) **and** responded within timeout | method, headers, body, auth, follow_redirects, verify_ssl |
| **Keyword** | HTTP(S) fetch | body **contains** or **does not contain** `keyword` (case-sensitive toggle), plus status rule | keyword, keyword_mode (`present`/`absent`) |
| **TCP Port** | TCP connect to `host:port` | connection established within timeout | host, port |
| **Ping** | ICMP echo; **fallback to TCP-ping** on ports 443/80 where raw sockets need privilege | reply received within timeout | host, packet_count |
| **DNS** | Resolve `host` for a record type | resolved value matches `expected_value` (optional) and resolution succeeds | record_type (A/AAAA/CNAME/MX/TXT/NS), expected_value, resolver |
| **Heartbeat (push)** | *Inverse* — waits for an inbound ping to `/ping/{token}` | a ping arrived within `interval + grace` | heartbeat_token, grace_seconds |
| **SSL-only** | TLS handshake, read cert | cert valid, not expired, chain OK, hostname matches | (uses SSL add-on logic, §6) |

> **Add-ons** (SSL check, domain-expiry check) attach to any HTTP/keyword/port monitor with an HTTPS/host target — they don't require a separate monitor unless you want SSL-only.

**Ping note (Linux):** ICMP typically requires `CAP_NET_RAW` or setting `net.ipv4.ping_group_range`. The app should detect lack of privilege and transparently fall back to TCP-ping, surfacing the mode in the detail panel so results aren't misread.

---

## 4. Scheduling Intervals

- Per-monitor interval, chosen from presets **or** custom seconds: `30s, 1m, 2m, 5m, 10m, 15m, 30m, 1h, 6h, 12h, 24h`. Minimum 15s (guarded).
- **SSL/domain checks run on their own slow cadence** (default once/12h), decoupled from the uptime interval — expensive WHOIS/TLS work shouldn't run every 30s.
- **Jitter:** add ±(0–5%) randomization to `next_run_at` so many monitors don't stampede on the same tick.
- **Concurrency cap:** global semaphore (default 25 simultaneous probes) to protect the host and the network.
- **Catch-up on restart:** on launch, any monitor whose `next_run_at` is in the past is scheduled immediately (staggered), not skipped.

---

## 5. Check Engine — State Machine & Flap Prevention

**States:** `PENDING → UP | DOWN`, plus `DEGRADED`, `PAUSED`, `MAINTENANCE`.

```
                 confirmation_threshold consecutive failures
        UP ─────────────────────────────────────────────► DOWN
         ▲                                                  │
         │            first success (or recovery_confirm)   │
         └──────────────────────────────────────────────────┘

  slow but reachable / SSL expiring soon  ──► DEGRADED (still "up", amber)
  manual pause                            ──► PAUSED (no probing)
  active maintenance window               ──► MAINTENANCE (probe optional, alerts suppressed)
```

- **Confirmation threshold** (default **3**): a single failed probe does *not* trip DOWN. After the first failure the monitor enters a fast **retry** cadence (`retry_interval`, default 30s) until it either recovers or hits the threshold, then transitions to DOWN and opens an incident.
- **Recovery confirmation** (default **1** success): fast to recover, slow to alarm.
- **Anchor check (internet-sanity):** before declaring any DOWN, probe a known-good anchor (configurable, default `1.1.1.1:443` + `8.8.8.8:443`). If the anchor is also unreachable, the app assumes *your* connectivity is down, **pauses alerting**, marks affected monitors `UNKNOWN` (rendered as a distinct muted state), and resumes normal evaluation once connectivity returns. This single feature kills most false-positive email storms.
- **Degraded:** `response_time_ms > degraded_threshold_ms` for K consecutive checks (default K=3) → DEGRADED; or SSL/domain within warning window. Degraded is a warning tint, not a full outage, and has its own (optional) notification trigger.

**Incident lifecycle:** opens on entering DOWN (`started_at`, `cause`, `status_code`/`error_message`), closes on return to UP (`resolved_at`, computed `duration_seconds`). Ongoing incidents can be **acknowledged** to silence re-notifications.

---

## 6. SSL Certificate & Domain Expiry Monitoring

**SSL (per monitor, when target is HTTPS or SSL-only):**
- On the slow cadence: TLS handshake, capture leaf + chain. Record `issuer`, `subject`, `valid_from`, `valid_until`, `days_remaining`, `chain_ok`, `hostname_match`, `self_signed`.
- **Alerts** fire once as each threshold in `ssl_alert_days` is crossed (default `[30, 14, 7, 3, 1]`), plus an immediate alert on **invalid / expired / chain-broken / hostname-mismatch**.
- Detail panel shows a cert card with a color-graded days-remaining ring (green > 30, amber 7–30, red < 7 / invalid).

**Domain expiry (per monitor):**
- Resolve registration expiry via **RDAP** (preferred, JSON, no rate-limit pain) with **WHOIS fallback** for TLDs lacking RDAP. Cache aggressively (default refresh once/24h) — registries dislike frequent WHOIS.
- Record `registrar`, `expiry_date`, `days_remaining`, `name_servers`, `status_codes` (e.g. clientTransferProhibited).
- **Alerts** at `domain_alert_days` thresholds (default `[45, 30, 14, 7]`).
- Some TLDs redact/rate-limit WHOIS; when expiry can't be determined, show an explicit "unknown — registry not queryable" state rather than a false green.

---

## 7. Notifications

**Channels** (each an independent, testable record):

| Channel | Config | Notes |
|---|---|---|
| **Email (SMTP)** | host, port, security (`none`/`starttls`/`tls`), username, password*, from, to[] | Password stored via OS keychain. "Send test" button. |
| **Desktop** | — | Native Tauri notification; click opens the monitor's detail panel. |
| **Webhook** | URL, method, custom headers, JSON template | Generic; base for the rest. |
| **Discord / Slack** | webhook URL | Preset payload formatters. |
| **Telegram** | bot token, chat id | |
| **ntfy / Pushover** | topic/URL or user+app token | Mobile push without email. |

`*` secrets encrypted at rest (§12).

**Triggers** (per monitor, each toggleable per channel):
`down` · `recovered` · `degraded` · `ssl_expiring` · `ssl_invalid` · `domain_expiring` · `heartbeat_missed`.

**Throttling & digest**
- **Confirmation-gated:** down alerts only fire after the state machine confirms DOWN.
- **Re-notify interval** for ongoing outages (default: remind every 6h until resolved; disabled once acknowledged).
- **Cooldown** per (monitor, trigger) to prevent duplicate sends within N minutes.
- Optional **daily digest** email summarizing uptime %, incidents, and upcoming expirations.

**Message template variables:** `{{monitor_name}} {{url}} {{status}} {{status_code}} {{error}} {{response_time_ms}} {{duration}} {{ssl_days}} {{domain_days}} {{checked_at}} {{incident_url}}`.

---

## 8. Maintenance Windows

- One-off (`starts_at`/`ends_at`) or **recurring** (cron expression).
- Scope: all monitors, a tag, or a specific set.
- Behavior: `suppress_alerts` (keep probing, don't notify — recommended, preserves uptime data) or `suppress_checks` (pause probing entirely).
- Active windows render monitors in the `MAINTENANCE` state and are excluded from uptime-% denominators.

---

## 9. Data Model (SQLite)

```sql
-- Core monitor definition
CREATE TABLE monitors (
  id                    INTEGER PRIMARY KEY,
  name                  TEXT NOT NULL,
  type                  TEXT NOT NULL,            -- http|keyword|port|ping|dns|heartbeat|ssl
  url                   TEXT,                     -- for http/keyword/ssl
  host                  TEXT,                     -- for port/ping/dns
  port                  INTEGER,
  method                TEXT DEFAULT 'GET',
  headers               TEXT,                     -- JSON object
  body                  TEXT,
  auth_type             TEXT,                     -- none|basic|bearer|header
  auth_ref              TEXT,                     -- keychain reference (never the secret)
  expected_status_codes TEXT DEFAULT '200-299',  -- CSV of codes/ranges
  keyword               TEXT,
  keyword_mode          TEXT,                     -- present|absent
  keyword_case_sensitive INTEGER DEFAULT 0,
  dns_record_type       TEXT,
  dns_expected_value    TEXT,
  interval_seconds      INTEGER NOT NULL DEFAULT 300,
  timeout_seconds       INTEGER NOT NULL DEFAULT 30,
  follow_redirects      INTEGER DEFAULT 1,
  verify_ssl            INTEGER DEFAULT 1,
  confirmation_threshold INTEGER DEFAULT 3,
  recovery_threshold    INTEGER DEFAULT 1,
  retry_interval_seconds INTEGER DEFAULT 30,
  degraded_threshold_ms INTEGER,                  -- null = disabled
  -- add-ons
  ssl_check_enabled     INTEGER DEFAULT 0,
  ssl_alert_days        TEXT DEFAULT '[30,14,7,3,1]',
  domain_check_enabled  INTEGER DEFAULT 0,
  domain_alert_days     TEXT DEFAULT '[45,30,14,7]',
  -- heartbeat
  heartbeat_token       TEXT UNIQUE,
  heartbeat_grace_seconds INTEGER DEFAULT 60,
  last_ping_at          INTEGER,
  -- runtime
  status                TEXT DEFAULT 'pending',   -- pending|up|down|degraded|paused|maintenance|unknown
  is_paused             INTEGER DEFAULT 0,
  last_checked_at       INTEGER,
  next_run_at           INTEGER,
  consecutive_failures  INTEGER DEFAULT 0,
  consecutive_successes INTEGER DEFAULT 0,
  tags                  TEXT,                     -- JSON array
  sort_order            INTEGER DEFAULT 0,
  created_at            INTEGER NOT NULL,
  updated_at            INTEGER NOT NULL
);

-- Raw check results (retained short-term, then rolled up)
CREATE TABLE checks (
  id             INTEGER PRIMARY KEY,
  monitor_id     INTEGER NOT NULL REFERENCES monitors(id) ON DELETE CASCADE,
  checked_at     INTEGER NOT NULL,
  status         TEXT NOT NULL,                   -- up|down|degraded
  response_time_ms INTEGER,
  status_code    INTEGER,
  error_message  TEXT,
  resolved_ip    TEXT,
  ssl_days_remaining INTEGER
);
CREATE INDEX idx_checks_monitor_time ON checks(monitor_id, checked_at DESC);

-- Daily rollups (uptime bars & long-range charts)
CREATE TABLE check_aggregates_daily (
  monitor_id     INTEGER NOT NULL REFERENCES monitors(id) ON DELETE CASCADE,
  day            TEXT NOT NULL,                   -- YYYY-MM-DD (local)
  up_count       INTEGER DEFAULT 0,
  down_count     INTEGER DEFAULT 0,
  degraded_count INTEGER DEFAULT 0,
  avg_response_ms REAL,
  min_response_ms INTEGER,
  max_response_ms INTEGER,
  uptime_pct     REAL,
  incident_count INTEGER DEFAULT 0,
  PRIMARY KEY (monitor_id, day)
);

CREATE TABLE incidents (
  id             INTEGER PRIMARY KEY,
  monitor_id     INTEGER NOT NULL REFERENCES monitors(id) ON DELETE CASCADE,
  started_at     INTEGER NOT NULL,
  resolved_at    INTEGER,
  duration_seconds INTEGER,
  cause          TEXT,                            -- timeout|status|keyword|dns|ssl|connection|heartbeat
  status_code    INTEGER,
  error_message  TEXT,
  acknowledged   INTEGER DEFAULT 0
);
CREATE INDEX idx_incidents_monitor ON incidents(monitor_id, started_at DESC);

CREATE TABLE notification_channels (
  id          INTEGER PRIMARY KEY,
  name        TEXT NOT NULL,
  type        TEXT NOT NULL,                      -- email|desktop|webhook|discord|slack|telegram|ntfy|pushover
  config      TEXT NOT NULL,                      -- JSON; secret fields hold keychain refs
  is_active   INTEGER DEFAULT 1,
  created_at  INTEGER NOT NULL
);

CREATE TABLE monitor_notifications (
  monitor_id  INTEGER NOT NULL REFERENCES monitors(id) ON DELETE CASCADE,
  channel_id  INTEGER NOT NULL REFERENCES notification_channels(id) ON DELETE CASCADE,
  triggers    TEXT NOT NULL DEFAULT '["down","recovered"]',  -- JSON array
  PRIMARY KEY (monitor_id, channel_id)
);

CREATE TABLE notification_log (
  id          INTEGER PRIMARY KEY,
  monitor_id  INTEGER, channel_id INTEGER, incident_id INTEGER,
  trigger     TEXT, sent_at INTEGER, success INTEGER, error TEXT
);

CREATE TABLE maintenance_windows (
  id          INTEGER PRIMARY KEY,
  name        TEXT, scope TEXT, target_ref TEXT,  -- all|tag|monitors + JSON
  starts_at   INTEGER, ends_at INTEGER,
  recurrence  TEXT,                               -- null | cron expr
  suppress    TEXT DEFAULT 'alerts',              -- alerts|checks
  created_at  INTEGER
);

CREATE TABLE ssl_certs (
  monitor_id  INTEGER PRIMARY KEY REFERENCES monitors(id) ON DELETE CASCADE,
  issuer TEXT, subject TEXT, valid_from INTEGER, valid_until INTEGER,
  days_remaining INTEGER, is_valid INTEGER, chain_ok INTEGER,
  hostname_match INTEGER, self_signed INTEGER, last_checked INTEGER
);

CREATE TABLE domain_info (
  monitor_id  INTEGER PRIMARY KEY REFERENCES monitors(id) ON DELETE CASCADE,
  registrar TEXT, expiry_date INTEGER, days_remaining INTEGER,
  name_servers TEXT, status_codes TEXT, queryable INTEGER, last_checked INTEGER
);

CREATE TABLE reports (
  id            INTEGER PRIMARY KEY,
  period_start  INTEGER NOT NULL,               -- first day of month (local, 00:00)
  period_end    INTEGER NOT NULL,               -- last moment of month
  label         TEXT NOT NULL,                  -- e.g. "March 2026"
  generated_at  INTEGER NOT NULL,
  summary_json  TEXT NOT NULL,                  -- cached computed metrics (see §13)
  html_path     TEXT,                           -- exported self-contained HTML
  pdf_path      TEXT,                           -- exported PDF (if requested)
  emailed_at    INTEGER,
  UNIQUE(period_start)
);

CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT);  -- SMTP defaults, retention, theme, accent, anchors, report schedule...
```

**Retention & rollup:** a nightly job aggregates the previous day's `checks` into `check_aggregates_daily`, then deletes raw `checks` older than `raw_retention_days` (default 30). Daily aggregates and `incidents` are kept **indefinitely** (tiny) — this is deliberate: monthly reports (§13) are computed from those durable tables, so a report for any past month stays accurate long after its raw checks are pruned. `VACUUM` weekly. Uptime bars read from aggregates; the live response-time chart reads raw `checks`.

---

## 10. IPC / Command Surface

Tauri commands (or a localhost REST layer if you prefer). Frontend also subscribes to push **events**.

```
Monitors     list_monitors() · get_monitor(id) · create_monitor(dto) · update_monitor(id,dto)
             delete_monitor(id) · pause(id) · resume(id) · check_now(id) · reorder(ids[])
Stats        get_stats(id, range)            -> {uptime_24h,7d,30d,90d, avg_ms, incidents}
             get_response_series(id, range)  -> [{t, ms, status}]
             get_uptime_bars(id, days=90)    -> [{day, uptime_pct, incidents, down_secs}]
Incidents    list_incidents(monitor_id?, range?) · acknowledge_incident(id)
Certs/Domain get_ssl(id) · get_domain(id) · refresh_ssl(id) · refresh_domain(id)
Channels     list_channels() · create/update/delete_channel · test_channel(id)
Maintenance  list_windows() · create/update/delete_window
Reports      list_reports() · get_report(id) · generate_report(period=YYYY-MM) · export_report(id, "html"|"pdf")
             email_report(id, channel_ids[]) · delete_report(id)
Settings     get_settings() · update_settings(dto) · test_smtp(dto) · export_backup() · import_backup(file)

Events (backend → frontend)
  monitor:updated {id, status, response_time_ms, checked_at}
  monitor:transition {id, from, to, incident_id}
  incident:opened / incident:resolved
  cert:warning {id, days} · domain:warning {id, days}
  report:generated {id, label}
  connectivity:changed {online: bool}
```

**Heartbeat endpoint (axum):** `GET|POST /ping/{token}` → 200, updates `last_ping_at`. Bind address configurable: `127.0.0.1` (local crons), LAN IP, or front it with `cloudflared`/tailscale for jobs running elsewhere. A separate reaper task marks a heartbeat monitor DOWN when `now > last_ping_at + interval + grace`.

---

## 11. UI / UX Specification  ★ (the headline)

Design intent: **an instrument, not a SaaS dashboard.** A deep, calm navy canvas (lifted from The Open's footer) that lets *status* be the only thing that glows — red/amber/green alerts pop hard against navy in a way they never would on grey. Restrained chrome, tabular mono numerals for anything measured, physical spring-eased slide-out panels. Nothing decorative competes with signal.

### 11.1 Design tokens

```
/* Surfaces — deep navy (The Open footer), layered by elevation.
   ↓ REPLACE --bg-base with your exact hyprpicker value; the rest is derived from it. */
--bg-sunken:      #0A1220   /* deepest, behind cards            */
--bg-base:        #101B2E   /* app background — The Open navy    */
--bg-surface:     #16233A   /* cards, panels                    */
--bg-surface-2:   #1C2C46   /* hover / elevated                 */
--bg-surface-3:   #243654   /* active / popover / panel         */

/* Borders — hairline, mostly felt not seen */
--border-subtle:  #1E2C44
--border-default: #2A3A56
--border-strong:  #3A4E70

/* Text — cool light on navy */
--text-primary:   #EAEDF3
--text-secondary: #9BA6BC
--text-tertiary:  #5F6C86
--text-disabled:  #3D4862

/* Status — these are the "pop". Each has a 12% tint bg + 28% border variant. */
--up:          #35D07F
--down:        #F26D6D
--degraded:    #F5A623
--paused:      #6B7688
--pending:     #5B8DEF
--maintenance: #B58BF5
--unknown:     #4A5670   /* your-connection-down state        */

/* Accent — cool cyan, deliberately NOT green so status green reads as status.
   Alt "signature" option: The Open yellow #FFBA00 (verified from their brand
   metadata) — swap --accent to it if you want the golf-major identity. */
--accent:      #3FC8E4
--accent-weak: rgba(63,200,228,0.14)
--focus-ring:  #3FC8E4

/* Radii */  --r-sm:6  --r-md:10  --r-lg:14  --r-pill:999
/* Space  */  4·8·12·16·20·24·32·40·48·64  (4-based scale)
/* Shadow (dark-tuned) */
--shadow-card:  0 1px 0 rgba(255,255,255,0.03) inset, 0 2px 8px rgba(0,0,0,0.4)
--shadow-panel: -24px 0 60px rgba(0,0,0,0.55)
```

**Typography** — UI: `Inter` (or `Geist`). Metrics: `JetBrains Mono` with `font-feature-settings:'tnum','zero'` for tabular, slashed numerals.

| Role | Size / line-height | Weight |
|---|---|---|
| Hero metric (mono) | 30 / 34 | 600 |
| Display | 24 / 30 | 650 |
| Section H | 17 / 24 | 600 |
| Body | 14 / 20 | 450 |
| Small | 13 / 18 | 450 |
| Micro-label (uppercase, `letter-spacing:0.08em`) | 11 / 14 | 600 |

**Motion** — panel slide `260ms cubic-bezier(0.32,0.72,0,1)`; backdrop fade `200ms`; card hover `120ms ease-out`; number count-up `320ms`. Honor `prefers-reduced-motion` (disable slides/pulses, keep instant state).

### 11.2 App layout

```
┌──────┬──────────────────────────────────────────────┬─────────────────┐
│ Rail │  Top bar: search · filter · grid/list · +Add  │                 │
│ 72px │──────────────────────────────────────────────│  Detail panel   │
│      │                                              │  (slides in from│
│ ▣ Dash│   Monitor grid / list (main content)         │   right, 440px, │
│ ⚠ Inci│                                              │   over a dimmed │
│ 🔔 Notf│   [card] [card] [card] [card]                │   backdrop)     │
│ 🛠 Maint│  [card] [card] [card] [card]                │                 │
│ ⚙ Set │                                              │                 │
│      │                                              │                 │
│ ──── │                                              │                 │
│ 12▲ 1▼│  ← global status summary pinned at rail base │                 │
└──────┴──────────────────────────────────────────────┴─────────────────┘
```

- **Left rail** (72px, expandable to 240px with labels): Dashboard, Incidents, Reports, Notifications, Maintenance, Settings. App mark at top; a live **global summary** at the base (`12 up · 1 down · 2 paused`), the down count in `--down` and gently pulsing when > 0.
- **Top bar:** fuzzy search, tag/status filter chips, **grid ⇄ list** toggle, range selector (24h/7d/30d/90d affecting cards), primary **+ Add monitor** button (accent).
- **Detail panel:** overlays the grid with a `rgba(0,0,0,0.5)` backdrop by default; on ultra-wide it can **push** the grid instead (setting). Closes on ✕, `Esc`, or backdrop click.

### 11.3 Monitor card (grid view)

```
┌────────────────────────────────────────────┐
│ ● api.myapp.com                        ⋯    │  ● = status dot (glow if up)
│   https://api.myapp.com/health   ⓗ http     │
│                                             │
│   142ms  ▁▂▁▃▂▁▂▁▄▂▁  (30-check sparkline)   │  mono response time
│                                             │
│   99.98% · 30d          SSL 24d  ⬤          │  uptime badge + cert ring
│   ▍▍▍▍▍▍▍▍▍▍▍▍▍▍▍▍▍▍▍▍▍▎▍▍▍▍▍▍▍▍▍▍▍▍          │  compact uptime bar (~45 seg)
└────────────────────────────────────────────┘
```
- Entire card is the click target → opens detail panel. `⋯` opens a quick menu (Check now · Pause · Edit · Delete) without opening the panel.
- **Status dot** carries a soft 2s "breathing" glow when UP (liveness signal); steady/urgent when DOWN; hollow when PAUSED.
- Hover: lift to `--bg-surface-2`, border → `--border-strong`, sparkline sharpens.

### 11.4 List view (dense, for many monitors)

Columns: `● | Name | Type | Last check | Response | 24h | 7d | 30d | ▍uptime bar | ⋯`. Numbers right-aligned, mono, tabular. Sortable headers. Row click → detail panel. This is the mode you'll live in past ~20 monitors.

### 11.5 The signature 90-day uptime bar

- One thin rounded segment per day, 2px gaps. Color = that day's rollup: full `--up`, `--degraded` if partial/slow, `--down` if any outage (intensity ∝ downtime), `--border-default` if no data.
- **Hover tooltip:** date · uptime % · incident count · total downtime. Click a segment → filters the incident list below to that day.
- Left/right end labels: "90 days ago" / "Today". A faint legend row beneath.

### 11.6 Detail panel (slide-from-right) — content order

1. **Header** — status pill + monitor name; secondary line = URL/host + type icon. Right: quick actions (`Check now`, `Pause`, `Edit`, `⋯ → Delete`). ✕ to close.
2. **Now strip** — three large tiles: current **status**, current **response time** (mono, count-up), and **last checked** (relative + absolute on hover).
3. **Uptime tiles** — `24h · 7d · 30d · 90d` percentages in a row (mono), each with its period downtime beneath in `--text-tertiary`.
4. **Response-time chart** — area/line, selectable range, hover crosshair reading exact ms + timestamp; incident spans shaded red beneath the curve.
5. **90-day uptime bar** (§11.5).
6. **SSL card** (if enabled) — issuer, subject, valid-until, a **days-remaining ring** (green/amber/red), chain/hostname status pills. `Refresh` button.
7. **Domain card** (if enabled) — registrar, expiry date, days-remaining ring, nameservers, registry lock status. Or an explicit "not queryable" note.
8. **Incident history** — reverse-chronological timeline: cause icon, started, duration, resolved, status code/error. Ongoing incidents show a live-ticking duration and an **Acknowledge** button.
9. **Request details** (HTTP) — method, resolved IP, final URL after redirects, response headers (collapsible), body snippet if keyword monitoring.
10. **Notifications** — chips of attached channels with per-trigger toggles; quick "add channel".
11. **Configuration** (collapsed) — inline summary with an **Edit** that opens the editor panel.

Panel scrolls independently; header stays pinned. Deep-linkable so a desktop notification click lands directly here.

### 11.7 Add / Edit monitor (also a right-side panel — consistent)

Sectioned form (accordion or stepper):
- **Basics** — name, type selector (icon segmented control), URL/host/port.
- **Schedule** — interval (preset chips + custom), timeout, confirmation threshold, retry interval.
- **Validation** — expected status codes, keyword + mode, follow redirects, verify SSL, degraded threshold (ms).
- **Certificate & Domain** — toggles + alert-day chips.
- **Notifications** — pick channels + per-trigger checkboxes.
- **Advanced** — method, request headers (key/value rows), body, auth (basic/bearer/header).
- **Live "Test check"** button runs a real probe immediately and renders the result inline (status, code, ms, cert peek) *before* you save — so misconfigurations never make it to production.

### 11.8 Other screens

- **Incidents** — global timeline across all monitors; filter by monitor/tag/range; header stats: **open incidents**, MTTR, incidents (30d). Acknowledge inline.
- **Reports** — grid of month cards (most recent first), each showing that month's headline uptime %, incident count, and total downtime. Click opens the full report in the main area (§13). Top-right: **Generate report** (month picker, for back-filling any past month) and per-report **Export HTML / Export PDF / Email now**. Auto-generated reports for the prior month appear here on the 1st.
- **Notifications** — channel manager (add/edit/test) + notification log (what was sent, when, success/failure).
- **Maintenance** — list + create windows (one-off / cron), scope picker, alerts-vs-checks suppression.
- **Settings** — SMTP (with Send-test), default check params, **anchor hosts** for the internet-sanity check, data retention, **monthly report schedule** (auto-generate on/off, day-of-month, time, recipient channels, formats), **appearance** (theme + accent-color picker — the accent token is user-swappable, e.g. cyan ↔ The Open yellow), backup export/import, about.

### 11.9 States & a11y

- **Empty:** quiet centered mark + "Add your first monitor" (accent CTA). **Loading:** shimmer skeletons matching card/panel shape. **Error:** inline with a Retry affordance — never a dead panel.
- **Connectivity-down banner:** a slim top strip ("Your connection appears offline — alerting paused") while monitors sit in `UNKNOWN`.
- Full keyboard nav; visible `--focus-ring`; ARIA live region announces status transitions; AA contrast throughout; reduced-motion respected; every icon-only control has an accessible label and tooltip.

---

## 12. Non-Functional Requirements

- **Performance:** comfortably handle a few hundred monitors on a desktop; bounded probe concurrency; incremental frontend updates via events (never full re-fetch on a single monitor's tick).
- **Reliability:** state persisted so restarts resume mid-schedule; catch-up for missed runs; heartbeat reaper independent of the scheduler.
- **Security:** all secrets (SMTP password, bearer tokens, webhook URLs with embedded tokens) stored in the **OS keychain** (Tauri `keyring` / `stronghold`); DB holds only references. Heartbeat listener defaults to `127.0.0.1`. `verify_ssl` on by default.
- **Storage:** SQLite WAL; raw-check retention + daily rollups keep the file small indefinitely; weekly `VACUUM`.
- **Backup/portability:** one-click export (DB snapshot + settings JSON, secrets excluded/optionally re-entered) and import.
- **Observability:** internal app log with a viewer in Settings; every notification and state transition is auditable via `notification_log` / `incidents`.

---

## 13. Monthly Incident Reports

A first-class output, not an afterthought. On the **1st of each month** the app auto-generates a report for the month just ended, computed entirely from the durable `check_aggregates_daily`, `incidents`, `ssl_certs`, and `domain_info` tables — so it's cheap, reproducible, and works for any historical month even after raw checks are pruned. Reports are also generable **on demand** for any past month.

### 13.1 Contents

**Cover / period header** — month label, date range, generation timestamp, app version.

**Fleet summary (hero band)** — the month at a glance, in mono numerals:
- Overall uptime % (downtime-weighted across all active monitors) and **delta vs previous month** (▲/▼).
- Total incidents · total accumulated downtime · **MTTR** · longest single outage (with the monitor named).
- Count of monitors, and how many had a "clean" month (100% up).
- SSL/domain alerts raised this month; expirations coming due in the next 30/60 days.

**Per-monitor table** — one row each: name · type · uptime % · incidents · total downtime · MTTR · avg / p95 response time · month-end status. Sortable in-app; sorted worst-uptime-first in exports so problems surface at the top.

**Incident log** — every incident opened or active during the month: monitor, started, duration, cause (timeout/status/keyword/dns/ssl/connection/heartbeat), status code / error, resolved-at (or "ongoing"). Grouped by monitor, chronological within.

**Certificate & domain outlook** — snapshot at month-end of every tracked cert/domain and its days-remaining, flagging anything inside its warning window. This is the section that stops a silent expiry from ever surprising you.

**Response-time trend** (optional) — small per-monitor sparklines or a combined chart of daily avg response times across the month.

### 13.2 Computed summary (`summary_json`)

Cached on the `reports` row so re-opening is instant and exports are deterministic:

```json
{
  "period": "2026-03",
  "fleet": {
    "uptime_pct": 99.94, "uptime_delta": +0.07,
    "incidents": 11, "downtime_seconds": 5220,
    "mttr_seconds": 474, "longest_outage": {"monitor":"api.myapp.com","seconds":1980},
    "monitors_total": 23, "clean_monitors": 18,
    "ssl_alerts": 2, "domain_alerts": 0,
    "expiring_soon": [{"monitor":"myapp.com","kind":"ssl","days":19}]
  },
  "monitors": [
    {"id":4,"name":"api.myapp.com","uptime_pct":99.7,"incidents":3,
     "downtime_seconds":2610,"mttr_seconds":870,"avg_ms":142,"p95_ms":310,"end_status":"up"}
  ],
  "incidents": [
    {"monitor":"api.myapp.com","started":"2026-03-08T02:14Z","duration_seconds":1980,
     "cause":"timeout","status_code":null,"resolved":"2026-03-08T02:47Z"}
  ]
}
```

### 13.3 Output & delivery

- **In-app view** — full report rendered in the main area, styled in the navy theme (§11.1). This is the default surface; the Reports screen (§11.8) lists past months as cards.
- **Self-contained HTML export** — single file, inline CSS + inline chart SVGs, no external assets. Uses the same navy palette so it looks like the app, and carries a **print stylesheet** (light background, page breaks between sections) so `Ctrl-P → Save as PDF` yields a clean document.
- **PDF export** — render the HTML headless (Tauri's webview print-to-PDF, or a bundled renderer) → `pdf_path`.
- **Auto-email** — if a report recipient channel is configured, on generation the report is emailed (HTML inline + PDF attached) to those addresses. Reuses the SMTP channel from §7. Pairs naturally with the daily digest as its monthly counterpart.

### 13.4 Scheduling & config (in Settings)

`report_auto_generate` (bool, default on) · `report_day_of_month` (default 1) · `report_time` (default 08:00 local) · `report_recipients` (channel ids) · `report_formats` (`["html"]` default, add `"pdf"`). A scheduler task mirrors the check scheduler: at the configured moment it computes the prior month, writes the `reports` row + exports, emits `report:generated`, and emails if configured. Manual `generate_report("2026-03")` is idempotent per `period_start` (regenerate overwrites).

---

## 14. Build Phases

| Phase | Scope | Definition of done |
|---|---|---|
| **P1 — MVP** | HTTP monitors · intervals · confirmation/anchor state machine · SQLite · dashboard grid + basic detail panel · desktop + email alerts | You can watch a real site and get a real email when it drops and recovers. |
| **P2 — Signal** | response-time chart · 90-day bars + daily rollups · incident history · keyword + TCP port + DNS + ping · list view | The dashboard tells the full story at a glance. |
| **P3 — Certificates** | SSL cert tracking + tiered alerts · domain RDAP/WHOIS + alerts · webhook/Discord/ntfy channels | Expiry surprises are impossible. |
| **P4 — Complete** | heartbeat/push monitors + axum receiver · maintenance windows · **monthly incident reports (in-app + HTML/PDF + auto-email)** · re-notify/cooldown/digest · full motion + polish + accent theming · backup/export | Feature-parity with (and nicer than) UptimeRobot for one user. |

---

## 15. Open Decisions (pick before P1)

1. **Frontend framework** — SolidJS (leanest) vs Svelte vs React.
2. **Heartbeat exposure** — localhost-only vs LAN vs tunnel; affects whether external cron jobs can reach it.
3. **Ping strategy** — grant `CAP_NET_RAW` for true ICMP, or ship TCP-ping only.
4. **Panel behavior on ultrawide** — overlay (default) vs push-content; expose as a setting either way.
5. **Accent color** — cyan `#3FC8E4` default, or The Open yellow `#FFBA00` for the golf-major identity (user-swappable in Settings).
6. **Report defaults** — HTML-only vs HTML+PDF; whether monthly reports auto-email or stay in-app until you export.

---

*End of specification. This is a build-ready blueprint; §3–§8 define behavior, §9–§11 define data / API / UI, §13 the monthly reporting subsystem — all in enough detail to implement directly.*
