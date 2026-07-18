# Vigil P2 (Signal) — Design Spec

> **Status:** autonomous build (user delegated approval) · **Date:** 2026-07-17 · **Base:** P1 on `master` (2d1977c)
> **Scope:** Phase 2 "Signal" per [`CLAUDE.md`](../../../CLAUDE.md) §14. Builds on the P1 architecture; inherits all P1 decisions (containerized axum + SolidJS, rustls, SQLite/WAL, uptime-from-incidents, ports 8090/8099).

Design decisions here were made autonomously (user asleep, explicit "auto mode" mandate), grounded in `CLAUDE.md` §3 (monitor types), §9 (schema), §11.4–11.6 (list view / 90-day bar / detail panel), and the P1 conventions. Where this doc and `CLAUDE.md` differ, this doc wins for P2.

---

## 1. P2 Definition of Done

The dashboard tells the full story at a glance:

1. **New monitor types work end-to-end:** Keyword, TCP-Port, DNS, and Ping (TCP-ping) monitors can be created, are probed correctly, and transition UP/DOWN through the same state machine + anchor gate as HTTP.
2. **Response-time chart:** the detail panel shows a real response-time line/area chart over a selectable range, with incident spans shaded.
3. **90-day uptime bars:** the signature per-day bar (color-graded by that day's rollup) renders on cards and the detail panel, with hover tooltips.
4. **Daily rollups:** a nightly job aggregates the previous day's raw checks into `check_aggregates_daily`; 30d/90d stats and bars read from durable aggregates, so they stay correct after raw checks are pruned.
5. **Incident history:** the detail panel shows a reverse-chronological incident timeline (cause, started, duration, resolved, status/error), and incidents can be **acknowledged**; a global Incidents screen lists incidents across monitors.
6. **List view:** a dense sortable table alternative to the grid, toggled from the top bar.
7. All new logic is TDD-tested; `cargo test` + `vitest` green; Docker container still healthy; migration `0002` applies cleanly on top of a P1 database (and the migration runner is hardened to tolerate SQL comments).

---

## 2. Scope

### 2.1 In scope (P2)
- **Monitor types:** `keyword`, `port`, `ping`, `dns` (plus existing `http`). Ping is **TCP-ping only** (P1 §15 decision — no raw ICMP).
- **Probe dispatch:** refactor the P1 HTTP prober into a `probe::run(&Monitor) -> ProbeOutcome` dispatcher by `monitor.type`.
- **Schema (migration `0002`):** add monitor columns (`host, port, keyword, keyword_mode, keyword_case_sensitive, dns_record_type, dns_expected_value`) and the `check_aggregates_daily` table. **Harden the migration runner** to strip `--` line comments before splitting on `;` (the P1 carry-forward, now that a 2nd migration lands).
- **Daily rollups:** extend the nightly maintenance task to roll up the previous day's `checks` into `check_aggregates_daily` (up/down counts, avg/min/max response, uptime%, incident count) **before** pruning raw checks.
- **Stats & series:** extend `get_stats` to 30d/90d; add `get_response_series(id, range)` (raw checks) and `get_uptime_bars(id, days=90)` (per-day from incidents + aggregates).
- **Incidents:** `list_incidents(monitor_id?, range?)` + `acknowledge_incident(id)` endpoints; detail-panel incident timeline; global Incidents screen.
- **Frontend:** response-time chart (uPlot), 90-day uptime bar component (cards + panel), list view toggle, incident timeline, and monitor-form fields for the new types.

### 2.2 Out of scope (deferred)
- **DEGRADED** state + response-time thresholds (`degraded_threshold_ms`) — not in the §14 P2 list; the `degraded_count` aggregate column exists but stays 0 in P2. Deferred.
- SSL/domain tracking (P3), heartbeat monitors (P4), non-email channels (P3), maintenance windows / reports (P4), the accent/theme picker.
- MX/TXT/NS DNS records beyond basic resolution matching are supported but not deeply validated; A/AAAA/CNAME are the primary tested paths.

---

## 3. Architecture deltas (from P1)

1. **`probe` module** gains `run(&Monitor) -> ProbeOutcome` that dispatches on `monitor.r#type`:
   - `http` / `keyword` → `probe::http` (keyword adds a body-content assertion).
   - `port` → `probe::tcp` (TCP connect to `host:port`).
   - `ping` → `probe::tcp` in ping mode (connect to `host:port`, default port 443 then 80).
   - `dns` → `probe::dns` (hickory-resolver).
   `worker::run_check` calls `probe::run` instead of `probe::http::probe`.
2. **Migration `0002`** adds columns + the aggregates table. The **migration runner** (`db.rs`) is hardened: strip `-- …` line comments per line before joining + `split(';')`, so migration files can carry normal SQL comments (fixes the P1 carry-forward). A test asserts a comment-bearing migration applies.
3. **`rollup` module** (`maintenance.rs` extension): `rollup_day(pool, day)` computes a `check_aggregates_daily` row per monitor for a given local day from raw `checks` + `incidents`; the nightly task rolls up yesterday, then prunes.
4. **New read endpoints** in `api/monitors.rs` (series, bars) + `api/incidents.rs` (list, acknowledge). Stats extended.
5. **Frontend:** `uPlot` dependency (bundled, self-contained) for the response chart; a `UptimeBar` component; a `ListView`; an `IncidentTimeline`; monitor-form type-specific fields.
6. **New Rust deps:** `hickory-resolver` (DNS). TCP uses `tokio::net`. No openssl (rustls stays).

---

## 4. Data model — migration `0002`

```sql
-- 0002_signal.sql (runner strips -- comments, splits on ;)
ALTER TABLE monitors ADD COLUMN host TEXT;
ALTER TABLE monitors ADD COLUMN port INTEGER;
ALTER TABLE monitors ADD COLUMN keyword TEXT;
ALTER TABLE monitors ADD COLUMN keyword_mode TEXT;            -- present|absent
ALTER TABLE monitors ADD COLUMN keyword_case_sensitive INTEGER NOT NULL DEFAULT 0;
ALTER TABLE monitors ADD COLUMN dns_record_type TEXT;        -- A|AAAA|CNAME|MX|TXT|NS
ALTER TABLE monitors ADD COLUMN dns_expected_value TEXT;

CREATE TABLE check_aggregates_daily (
  monitor_id      INTEGER NOT NULL REFERENCES monitors(id) ON DELETE CASCADE,
  day             TEXT NOT NULL,                 -- YYYY-MM-DD (UTC)
  up_count        INTEGER NOT NULL DEFAULT 0,
  down_count      INTEGER NOT NULL DEFAULT 0,
  degraded_count  INTEGER NOT NULL DEFAULT 0,    -- P2: always 0 (DEGRADED deferred)
  avg_response_ms REAL,
  min_response_ms INTEGER,
  max_response_ms INTEGER,
  uptime_pct      REAL,
  incident_count  INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (monitor_id, day)
);
```

Migration `0002` is recorded as version 2 by the runner. `ALTER TABLE ADD COLUMN` is safe on SQLite (the columns are nullable or have defaults). `monitors.type` CHECK is not enforced by DDL (app validates); the new types are `keyword|port|ping|dns`.

The `Monitor` struct + `FromRow` (`models.rs`) gain the new fields; `CreateMonitorDto`/`UpdateMonitorDto` gain them (all optional).

---

## 5. Monitor types & probers

All probers return the existing `ProbeOutcome { ok, response_time_ms, status_code, error_message, resolved_ip, cause }` and feed the **unchanged** state machine + anchor gate. Timing via `Instant`.

- **Keyword** (`probe::http`, keyword mode): perform the HTTP fetch as for `http`, then if `keyword` is set, read the body and check `keyword_mode`: `present` → body must contain the keyword; `absent` → body must NOT contain it. Respect `keyword_case_sensitive` (default case-insensitive). On keyword failure → `ok=false, cause=Cause::Keyword` (new cause variant), even if the status code matched. Status-code rule still applies first.
- **TCP Port** (`probe::tcp::connect`): `tokio::net::TcpStream::connect((host, port))` within `timeout_seconds`. Success if connected. Failure cause `Cause::Connection` (or `Cause::Timeout`). Requires `host` + `port`.
- **Ping** (TCP-ping, `probe::tcp::ping`): connect to `host:port` where port defaults to 443 then 80 if `port` is null; success if either connects. Cause `Connection`/`Timeout`. Surfaced in the UI as "TCP-ping" so results aren't misread (per `CLAUDE.md` §3 note).
- **DNS** (`probe::dns`): use `hickory-resolver` (system config) to resolve `host` for `dns_record_type` (A/AAAA/CNAME/MX/TXT/NS). Success if resolution returns ≥1 record **and**, if `dns_expected_value` is set, at least one record's string form contains/equals it. `resolved_ip` set to the first A/AAAA answer where applicable. Failure cause `Cause::Dns`.

**New `Cause` variant:** `Keyword`. (`Cause` enum: `Timeout, Status, Connection, Dns, Keyword`.) Incident `cause` TEXT accordingly.

**Validation (API):** `http`/`keyword` require `url`; `port`/`ping`/`dns` require `host` (`port` required for `port`); `dns` requires `dns_record_type`. `keyword` requires `keyword` + `keyword_mode`. Interval floor 15s still applies.

---

## 6. Daily rollups

`rollup::rollup_day(pool, day: &str /* YYYY-MM-DD UTC */) -> Result<()>`: for each monitor with checks on that day, compute from raw `checks` where `checked_at` ∈ [day 00:00, next day 00:00) UTC:
- `up_count` = count(status='up'), `down_count` = count(status='down'), `degraded_count` = 0.
- `avg/min/max_response_ms` from non-null `response_time_ms`.
- `incident_count` = incidents that **started** that day.
- `uptime_pct` = time-weighted from incident spans clipped to the day (reuse `uptime::compute` over the day window) — falls back to count-based (`up/(up+down)`) only if no incident data; store the % .
- Upsert into `check_aggregates_daily` (`ON CONFLICT(monitor_id,day) DO UPDATE`).

The nightly maintenance loop: on each daily pass, `rollup_day(yesterday_utc)` (idempotent), **then** prune raw checks older than retention, **then** weekly `incremental_vacuum`. Rollups are also computed on-demand for any day missing an aggregate when the 90-day-bars endpoint runs (lazy backfill), so a fresh install / recently-restarted instance still shows bars.

---

## 7. API additions

```
Stats     GET /api/monitors/:id/stats?range=24h|7d|30d|90d
              -> {uptime_pct|null, downtime_seconds, avg_ms|null, incidents}
              (uptime/downtime still from incidents; avg from raw checks for ≤7d, from aggregates for 30d/90d)
Series    GET /api/monitors/:id/series?range=24h|7d
              -> [{t: epoch, ms: number|null, status: "up"|"down"}]  (raw checks, capped ~500 points)
Bars      GET /api/monitors/:id/bars?days=90
              -> [{day:"YYYY-MM-DD", uptime_pct|null, incidents, down_seconds, has_data}]
              (per-day from incidents for uptime/down, aggregates for has_data; lazy-backfills missing days)
Incidents GET /api/incidents?monitor_id=&range=30d   -> [{id, monitor_id, monitor_name, started_at, resolved_at,
              duration_seconds, cause, status_code, error_message, acknowledged}]
          POST /api/incidents/:id/acknowledge         -> 200 (sets acknowledged=1)
```

Monitor create/update accept the new type fields; validation per §5. `test-check` supports all types.

---

## 8. Frontend

- **Response-time chart** (`ResponseChart.tsx`, detail panel §11.6.4): **uPlot** (bundled). Fetch `/series?range`; render an area/line of ms over time; range selector (24h/7d); shade incident spans (from `/incidents`) beneath. Respect reduced-motion (no animation). Theme via CSS vars.
- **90-day uptime bar** (`UptimeBar.tsx`, §11.5): thin rounded segment per day from `/bars`; color = `--up` (100%), `--degraded` (partial/slow), `--down` (outage, intensity ∝ downtime), `--border-default` (no data). Hover tooltip: date · uptime% · incidents · downtime. Used on cards (compact ~45-seg) and the detail panel (full 90). Click a segment → filter incident timeline to that day (panel).
- **Incident timeline** (`IncidentTimeline.tsx`, §11.6.8): reverse-chron list in the detail panel — cause icon, started, duration (live-ticking if ongoing), resolved, status/error, **Acknowledge** button (ongoing).
- **Incidents screen** (`Incidents.tsx`, §11.8): global timeline via `/api/incidents`; filter by monitor/range; header stats (open incidents, MTTR, 30d count); acknowledge inline. Rail gains an Incidents nav item.
- **List view** (`ListView.tsx`, §11.4): dense sortable table — `● | Name | Type | Last check | Response | 24h | 7d | 30d | ▍bar | ⋯`. Top-bar grid⇄list toggle (a `view` signal, persisted in the store).
- **Monitor form** (`MonitorForm.tsx`): type selector (http/keyword/port/ping/dns) reveals type-specific fields — url (http/keyword), keyword+mode+case (keyword), host+port (port/ping), host+record-type+expected (dns). Test-check works per type.
- **Cards** gain the compact 90-day bar (replacing the P1 placeholder strip).

Frontend keeps the pure-`applyEvent` reducer + navy tokens. New charts must be self-contained (uPlot bundled, no external CDN).

---

## 9. Testing

- **Pure/unit (TDD):** keyword match logic (present/absent × case), DNS expected-value match, rollup aggregation math, `Cause::Keyword` classification, bars per-day computation, the hardened migration comment-stripper.
- **Prober integration:** keyword (wiremock body), TCP port (a bound `TcpListener`), DNS (resolve `localhost`/a known name, or an injectable resolver), ping (bound listener). Each asserts the classified `ProbeOutcome`.
- **API:** create each monitor type (validation), series/bars/stats-30d/90d shapes, incident list + acknowledge, rollup-then-stats.
- **DB:** migration `0002` applies on a fresh DB **and** on a P1 DB (idempotent version check); comment-bearing migration applies.
- **Frontend:** `applyEvent` unchanged; component sanity for UptimeBar (colors from bars data), ListView, the form's type-field reveal; ResponseChart mounts without crashing on empty series.
- **Acceptance:** create one monitor of each type against local targets; verify probe results + a rollup + bars + series + an incident timeline; Docker still healthy.

---

## 10. Decisions log (autonomous)

| # | Decision | Choice |
|---|---|---|
| 1 | Migration runner hardening (P1 carry-forward) | **Harden now** (0002 lands): strip `--` line comments before `split(';')`; keeps the no-build-time-DB runner. |
| 2 | Ping | TCP-ping only (P1 §15), connect to `host:port` (default 443→80). |
| 3 | DNS resolver | `hickory-resolver` (new dep). |
| 4 | Response chart lib | **uPlot** (tiny, bundled, self-contained). |
| 5 | DEGRADED state | **Deferred** (not in §14 P2 list); `degraded_count` column exists, stays 0. |
| 6 | Uptime source | Still **incidents** (P1); aggregates provide response-time history + bar has-data + 30d/90d avg. |
| 7 | Bars lazy-backfill | The bars endpoint computes/upserts missing daily aggregates on demand, so fresh/restarted instances show history. |
| 8 | New `Cause` | `Keyword` (keyword assertion failed despite status match). |
| 9 | Probe dispatch | `probe::run(&Monitor)` matches on type → http/tcp/dns; worker calls it. |
| 10 | Global Incidents screen | Included (basic) — Rail nav + list + acknowledge. |

---

## 11. Build order

1. Migration `0002` + **runner hardening** (comment-strip) + `models` new fields/FromRow/DTOs + validation helpers. (TDD: comment-strip, migration-on-P1-DB.)
2. `Cause::Keyword`; `probe::run` dispatcher skeleton (http passthrough) + worker switch to `probe::run`.
3. `probe::tcp` (port + ping) **(TDD, bound listener)**.
4. `probe::dns` (hickory) **(TDD)**.
5. Keyword mode in `probe::http` + keyword match helper **(TDD)**.
6. `rollup::rollup_day` + nightly integration + `uptime` reuse **(TDD)**.
7. API: stats 30d/90d + `series` + `bars` (lazy backfill) endpoints **(TDD)**.
8. API: `incidents` list + acknowledge **(TDD)**; `incidents.acknowledged` already in schema.
9. Frontend: `UptimeBar` (cards + panel) + `/bars` wiring.
10. Frontend: `ResponseChart` (uPlot) + incident-span shading.
11. Frontend: `IncidentTimeline` (panel) + acknowledge.
12. Frontend: `Incidents` screen + Rail nav.
13. Frontend: `ListView` + grid⇄list toggle.
14. Frontend: `MonitorForm` type selector + type-specific fields; cards show real bar.
15. Acceptance (one monitor per type + rollup/bars/series/incident) + final review.

*End of P2 design spec.*
