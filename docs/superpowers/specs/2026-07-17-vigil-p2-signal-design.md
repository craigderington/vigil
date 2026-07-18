# Vigil P2 (Signal) — Design Spec

> **Status:** autonomous build (user delegated approval) · **Date:** 2026-07-17 · **Revision:** v2 (spec-review-hardened) · **Base:** P1 on `master` (2d1977c)
> **Scope:** Phase 2 "Signal" per [`CLAUDE.md`](../../../CLAUDE.md) §14. Builds on P1; inherits all P1 decisions (containerized axum + SolidJS, rustls, SQLite/WAL, uptime-from-incidents, ports 8090/8099).

Design decisions made autonomously (explicit "auto mode" mandate), grounded in `CLAUDE.md` §3/§9/§11 and the P1 code. v2 incorporates the multi-lens spec review (changelog §12). Where this doc and `CLAUDE.md` differ, this doc wins for P2.

---

## 1. P2 Definition of Done

The dashboard tells the full story at a glance:

1. **New monitor types work end-to-end:** Keyword, TCP-Port, DNS, Ping (TCP-ping) — created, probed correctly, transitioning UP/DOWN through the same state machine + anchor gate as HTTP.
2. **Response-time chart** in the detail panel over a selectable range, incident spans shaded.
3. **90-day uptime bars** (color-graded per-day) on cards and the detail panel, with hover tooltips.
4. **Daily rollups:** a nightly job (with multi-day catch-up) aggregates completed days' raw checks into `check_aggregates_daily`; 30d/90d avg-response reads from durable aggregates so it survives raw-check pruning.
5. **Incident history:** detail-panel incident timeline + **acknowledge**; a global Incidents screen.
6. **List view:** dense sortable table, toggled from the top bar.
7. All new logic TDD-tested; `cargo test` + `vitest` green; Docker healthy; **migration `0002` applies on top of a P1 database** via a restructured version-ordered runner.

---

## 2. Scope

### 2.1 In scope
- **Monitor types:** `keyword`, `port`, `ping`, `dns` (+ existing `http`). Ping = **TCP-ping only** (P1 §15).
- **Probe dispatch:** `probe::run(&Monitor) -> ProbeOutcome` dispatching on `monitor.type`.
- **Migration runner restructure** (see §3.1) + migration `0002` (§4).
- **Daily rollups** with catch-up (§6); shared aggregate-ensure for read endpoints.
- **Stats/series/bars** endpoints (§7).
- **Incidents** list + acknowledge (§7); detail-panel timeline; global Incidents screen.
- **Frontend:** response chart (uPlot, bundled), 90-day bar, list view, incident timeline, monitor-form type fields, detail-panel tiles extended to 24h/7d/30d/90d.

### 2.2 Out of scope (deferred)
- **DEGRADED** state + `degraded_threshold_ms` — not in §14 P2; `degraded_count` column exists, stays 0.
- SSL/domain (P3), heartbeat (P4), non-email channels (P3), maintenance windows/reports (P4), theme picker.
- **Acknowledge is a flag only** in P2 (sets `acknowledged=1`, shown in UI); its functional consumer (re-notify suppression) is P4.

---

## 3. Architecture deltas

### 3.1 Migration runner restructure (load-bearing)
The P1 runner (`db.rs`) is hardcoded single-shot (checks `version=1`, `include_str!("0001_init.sql")`, inserts 1). **Restructure** it to an ordered list and apply every not-yet-applied version:
```rust
const MIGRATIONS: &[(i64, &str)] = &[
    (1, include_str!("../migrations/0001_init.sql")),
    (2, include_str!("../migrations/0002_signal.sql")),
];
// run_migrations: ensure schema_migrations exists; for each (v, sql) in MIGRATIONS,
//   if no row WHERE version=v: in a tx, apply sql (split into statements), INSERT (v, now).
```
On a P1 database, version 1 is already recorded → only `0002` applies. On a fresh DB, both apply in order. **Statement splitting is hardened**: strip `--` line comments per line before joining and `split(';')` (defensive; also lets `0002` carry normal comments). Record `applied_at = now` (real epoch, fixing the P1 `applied_at=0` minor). A test asserts `0002` applies on a fresh DB **and** on a DB already at version 1, and that a comment-bearing statement applies.

### 3.2 Other deltas
- **`probe::run`** dispatches: `http`/`keyword`→`probe::http`; `port`/`ping`→`probe::tcp`; `dns`→`probe::dns`. `worker::run_check` calls `probe::run` (was `probe::http::probe`). New `Cause::Keyword`.
- **`rollup` module** (in `maintenance.rs` or `rollup.rs`): `rollup_day(pool, day_utc)` writes one `check_aggregates_daily` row per monitor for a **completed** day; `rollup_catch_up(pool)` rolls up every completed day since the last stored aggregate, bounded by raw-check retention. Nightly task + a one-shot at startup call `rollup_catch_up`.
- **New endpoints** (§7): series, bars, incidents list/ack; stats extended.
- **Frontend:** `uPlot` dep (bundled, self-contained). New components (§8).
- **New Rust deps:** `hickory-resolver` (DNS, tokio+rustls). TCP via `tokio::net`. No openssl.

---

## 4. Data model — migration `0002_signal.sql`

```sql
ALTER TABLE monitors ADD COLUMN host TEXT;
ALTER TABLE monitors ADD COLUMN port INTEGER;
ALTER TABLE monitors ADD COLUMN keyword TEXT;
ALTER TABLE monitors ADD COLUMN keyword_mode TEXT;
ALTER TABLE monitors ADD COLUMN keyword_case_sensitive INTEGER NOT NULL DEFAULT 0;
ALTER TABLE monitors ADD COLUMN dns_record_type TEXT;
ALTER TABLE monitors ADD COLUMN dns_expected_value TEXT;

ALTER TABLE incidents ADD COLUMN acknowledged INTEGER NOT NULL DEFAULT 0;

CREATE TABLE check_aggregates_daily (
  monitor_id      INTEGER NOT NULL REFERENCES monitors(id) ON DELETE CASCADE,
  day             TEXT NOT NULL,                 -- YYYY-MM-DD (UTC)
  up_count        INTEGER NOT NULL DEFAULT 0,
  down_count      INTEGER NOT NULL DEFAULT 0,
  degraded_count  INTEGER NOT NULL DEFAULT 0,    -- P2: always 0
  avg_response_ms REAL,
  min_response_ms INTEGER,
  max_response_ms INTEGER,
  uptime_pct      REAL,                          -- stored for P4 durable reports (completed days)
  incident_count  INTEGER NOT NULL DEFAULT 0,
  sample_count    INTEGER NOT NULL DEFAULT 0,    -- up_count+down_count, for count-weighted 30d/90d avg
  PRIMARY KEY (monitor_id, day)
);
CREATE INDEX idx_aggregates_day ON check_aggregates_daily(monitor_id, day);
```

All `ALTER TABLE ADD COLUMN` are SQLite-safe (nullable or defaulted). `Monitor` struct + manual `FromRow` gain the 7 monitor fields; `Incident` gains `acknowledged`. `CreateMonitorDto`/`UpdateMonitorDto` gain them **and** a `type` field (see §5).

---

## 5. Monitor types, DTO, & probers

**DTO changes (`models.rs`) — required, per review:** `CreateMonitorDto` gains `#[serde(default = d_http)] r#type: String` and its `url` becomes `Option<String>`; it gains `host: Option<String>, port: Option<i64>, keyword, keyword_mode, keyword_case_sensitive (default 0), dns_record_type, dns_expected_value`. The `create()` handler threads `type` into the INSERT column list (no longer hardcoded `'http'`) and replaces the unconditional "url required" check with the **per-type validation** below. `UpdateMonitorDto` gains the same optional fields.

**Per-type validation (422 on failure):**
- `http`/`keyword`: `url` required. `keyword`: `keyword` + `keyword_mode` (present|absent) required.
- `port`: `host` + `port` required. `ping`: `host` required (`port` optional). `dns`: `host` + `dns_record_type` (A|AAAA|CNAME|MX|TXT|NS) required.
- Interval floor 15s applies to all.

**Probers** (all return the existing `ProbeOutcome`; timing via `Instant`; feed the unchanged state machine + anchor gate):
- **Keyword** (`probe::http`): the HTTP fetch as for `http`; status-code rule first. If it passes and `keyword` is set, read **at most 2 MiB** of the body (`resp.bytes()` bounded; a larger body is truncated and matched only within the read prefix), decode **lossy UTF-8**, honor `keyword_case_sensitive` (default insensitive), and check `keyword_mode`: `present`→must contain; `absent`→must not. Keyword failure → `ok=false, cause=Cause::Keyword` (status was fine).
- **TCP Port** (`probe::tcp::connect`): `tokio::time::timeout(timeout, TcpStream::connect((host, port)))`. Connected→ok. Cause `Timeout` on elapse, else `Connection`.
- **Ping** (`probe::tcp::ping`): if `port` is set → single attempt to `host:port`. If `port` is null → try **443 then 80 sequentially**, each bounded by full `timeout_seconds`; success if either connects; any 443 failure (refused or timeout) proceeds to 80. `response_time_ms` = the successful attempt's latency. Cause `Connection`/`Timeout`. UI labels this "TCP-ping".
- **DNS** (`probe::dns`, hickory-resolver from system config): resolve `host` for `dns_record_type`. Success if ≥1 record returned **and**, if `dns_expected_value` set, at least one record's canonical string form **case-insensitively contains** it. Canonical forms: A/AAAA→IP string; CNAME/NS→target host (trailing dot trimmed); MX→`"<pref> <host>"`; TXT→concatenated segments. `resolved_ip`←first A/AAAA. Failure cause `Cause::Dns`.

`Cause` enum becomes `Timeout, Status, Connection, Dns, Keyword`. `test-check` supports all types.

---

## 6. Daily rollups

`rollup_day(pool, day: &str /* YYYY-MM-DD UTC, a COMPLETED day */) -> Result<()>` — per monitor with any check that day (checks in `[day_start, day_end)` UTC):
- `up_count`/`down_count` from `checks.status`; `degraded_count`=0; `sample_count`=up+down.
- `avg/min/max_response_ms` from non-null `response_time_ms`.
- `incident_count` = incidents whose `started_at` ∈ `[day_start, day_end)` (**started-that-day**).
- `uptime_pct` = time-weighted via `uptime::compute` over the day window, fed **overlap-based** incident spans: `SELECT started_at, resolved_at FROM incidents WHERE monitor_id=? AND started_at < day_end AND (resolved_at IS NULL OR resolved_at > day_start)`, clipped to `[day_start, day_end]`. (A completed day uses the full `day_end` as clip end.)
- Upsert into `check_aggregates_daily` (`ON CONFLICT(monitor_id,day) DO UPDATE`).

`rollup_catch_up(pool)`: find `MAX(day)` per monitor in aggregates (or the oldest raw check within retention if none), and `rollup_day` **every completed day** from there through **yesterday** (UTC), bounded to days still within `raw_retention_days` (older days have no raw checks to source). Called nightly **and** once at startup — so multi-day downtime doesn't permanently lose days.

**Today is never stored as an aggregate** (it's incomplete). Live per-day values for today come from incidents + recent checks at read time (§7).

**Shared read-path ensure:** `ensure_aggregates(pool, monitor_id, since_day)` runs a **bounded** catch-up (completed days since the monitor's last aggregate, still within retention) before a read endpoint uses aggregates. After the nightly/startup catch-up this is cheap (usually a no-op); it caps work at the retention window (≤30 completed days), never 90 inline rollups.

---

## 7. API additions

```
Stats     GET /api/monitors/:id/stats?range=24h|7d|30d|90d
              -> {uptime_pct|null, downtime_seconds, avg_ms|null, incidents}
              uptime/downtime: incidents (overlap, clip to now) — unchanged from P1, extended windows.
              avg_ms: ≤7d from raw checks; 30d/90d COUNT-WEIGHTED from aggregates
                = sum(avg_response_ms*sample_count)/sum(sample_count) over days with non-null avg,
                after ensure_aggregates. Plus today's raw-check avg blended in (count-weighted).
Series    GET /api/monitors/:id/series?range=24h|7d
              -> [{t, ms|null, status}]  bucketed: divide the window into <=300 equal time-slots;
                 per slot emit avg(response_time_ms) + worst status (down if any down). Empty slots omitted.
Bars      GET /api/monitors/:id/bars?days=90
              -> [{day, uptime_pct|null, incidents, down_seconds, has_data}]
                 uptime/down per day computed from INCIDENTS (overlap, clip end = min(day_end, now)),
                 so today is live & correct. has_data = aggregate exists for the day OR (day within
                 retention AND a check exists) OR any incident overlaps the day. calls ensure_aggregates.
Incidents GET /api/incidents?monitor_id=&range=30d
              -> [{id, monitor_id, monitor_name, started_at, resolved_at, duration_seconds,
                   cause, status_code, error_message, acknowledged}]
          POST /api/incidents/:id/acknowledge   -> 200 (UPDATE incidents SET acknowledged=1)
```

---

## 8. Frontend

- **Detail-panel uptime tiles** extended to **24h · 7d · 30d · 90d** (each with period downtime beneath), wired to the extended stats endpoint (blueprint §11.6.3).
- **Response-time chart** (`ResponseChart.tsx`, uPlot, bundled): fetch `/series?range` (24h/7d selector); area/line of ms; shade incident spans fetched from `/api/incidents?monitor_id=:id`, clipped to the selected range (open incidents extended to now). Reduced-motion respected; theme via CSS vars.
- **90-day uptime bar** (`UptimeBar.tsx`): a segment per day from `/bars`. **Color bands** (P2, no DEGRADED state but the bar itself is graded by day uptime%): `uptime_pct == 100` (or has_data with no downtime) → `--up`; `50 ≤ uptime% < 100` → `--degraded` (amber, shade darker as uptime falls); `uptime% < 50` → `--down`; `!has_data` → `--border-default`. Hover tooltip: date · uptime% · incidents · downtime. Compact ~45-seg variant on cards; full 90-seg on the panel with **"90 days ago"/"Today" end labels + a faint legend row**. Click a panel segment → filter the incident timeline to that day.
- **Incident timeline** (`IncidentTimeline.tsx`, panel): reverse-chron; cause icon, started, duration (live-ticking if ongoing), resolved, status/error, **Acknowledge** button (ongoing/unacked → `POST /incidents/:id/acknowledge`, then refresh).
- **Incidents screen** (`Incidents.tsx`): global list via `/api/incidents`; filter by monitor/range; header stats (open incidents, MTTR, 30d count); acknowledge inline. Rail gains an Incidents nav item.
- **List view** (`ListView.tsx`): dense table `● | Name | Type | Last check | Response | 24h | 7d | 30d | ▍bar | ⋯`. Sortable headers; **default sort = `sort_order` then name**; the active sort column+direction and the grid⇄list `view` choice **persist in the store** (in-memory; survives view toggles this session). Top-bar grid⇄list toggle.
- **Monitor form** (`MonitorForm.tsx`): a **type selector** (http/keyword/port/ping/dns) reveals type-specific fields — url (http/keyword); keyword+mode+case (keyword); host+port (port); host+optional-port (ping); host+record-type+expected (dns). Sends `type` + the relevant fields. Test-check works per type.
- **Cards** show the real compact 90-day bar (replacing the P1 placeholder strip).

Frontend keeps the pure `applyEvent` reducer + navy tokens; uPlot is bundled (no CDN).

---

## 9. Testing

- **Pure/unit (TDD):** keyword match (present/absent × case, prefix-truncation), DNS expected-value canonical-form match, rollup aggregation math (incl. overlap span → correct uptime), count-weighted avg, series bucketing, bar color-band mapping, the migration comment-strip + multi-version apply, `Cause::Keyword`.
- **Prober integration:** keyword (wiremock body, present/absent/large-body), TCP port (bound `TcpListener` up + refused down), ping (bound listener; null-port fallback), DNS (injectable resolver or resolve `localhost`).
- **API:** create each type (validation 422 paths incl. missing host/url/type), series/bars/stats-30d/90d shapes, incident list + acknowledge (flag flips), rollup_day + ensure_aggregates then stats.
- **DB:** `0002` applies on a fresh DB **and** on a version-1 DB (idempotent, only 2 applies); comment-bearing statement applies; cascade still works.
- **Frontend:** `applyEvent` unchanged; UptimeBar color-band from bars data; ListView sort; form type-field reveal; ResponseChart mounts on empty series without crashing.
- **Acceptance:** one monitor of each type against local targets; verify probe results, a rollup, bars, series, an incident timeline + acknowledge; Docker healthy; `0002` on a real P1 DB.

---

## 10. Decisions log (autonomous)

| # | Decision | Choice |
|---|---|---|
| 1 | Migration runner | **Restructure to a version-ordered list** applying each unapplied version; comment-strip is defensive layering. Real `applied_at`. |
| 2 | Ping | TCP-ping; explicit port = single attempt; null port = 443→80 sequential, each full timeout; report successful latency. |
| 3 | DNS | hickory-resolver; case-insensitive substring on canonical record string form. |
| 4 | Response chart | uPlot, bundled. |
| 5 | DEGRADED | Deferred; `degraded_count`=0. Bars are still uptime-graded (up/amber/down bands). |
| 6 | Uptime source | Incidents (overlap-clipped). `/bars` & `/stats` uptime from incidents (live today); aggregate `uptime_pct` stored nightly for P4 reports. avg_ms 30d/90d = count-weighted from aggregates. |
| 7 | Rollups | Completed days only; nightly + startup **catch-up** since last aggregate, bounded by retention; today computed live. Read endpoints call a bounded `ensure_aggregates`. |
| 8 | Acknowledge | `acknowledged` column added in 0002; P2 = flag + UI only (re-notify suppression is P4). |
| 9 | Keyword body | Read ≤2 MiB, lossy UTF-8, match within prefix. |
| 10 | Series | Bucket window into ≤300 slots (avg ms + worst status). |
| 11 | New Cause | `Keyword`. |
| 12 | Detail tiles | Extended to 24h/7d/30d/90d. |

---

## 11. Build order

1. **Migration runner restructure** (version-ordered + comment-strip + real applied_at) + `0002_signal.sql` + `models` new fields/FromRow (Monitor + Incident) + DTO `type`/`url`-optional/new fields. **(TDD:** comment-strip, multi-version apply on fresh + v1 DB.)
2. `Cause::Keyword`; `probe::run` dispatcher (http passthrough) + `worker` switch to `probe::run`; per-type API validation in create/update. **(TDD** validation.)
3. `probe::tcp` (port + ping w/ fallback) **(TDD, bound listener).**
4. `probe::dns` (hickory, injectable resolver for tests) **(TDD).**
5. Keyword mode in `probe::http` (bounded body) + match helper **(TDD).**
6. `rollup_day` + `rollup_catch_up` + `ensure_aggregates` + nightly/startup wiring **(TDD** overlap uptime, catch-up).
7. API: stats 30d/90d (count-weighted avg) + `series` (bucketed) + `bars` (incidents+ensure) **(TDD).**
8. API: `incidents` list + acknowledge **(TDD).**
9. Frontend: `UptimeBar` (cards + panel, color bands, labels/legend) + `/bars`.
10. Frontend: `ResponseChart` (uPlot) + incident-span shading.
11. Frontend: `IncidentTimeline` (panel) + acknowledge; detail-panel tiles → 24h/7d/30d/90d.
12. Frontend: `Incidents` screen + Rail nav.
13. Frontend: `ListView` + grid⇄list toggle + sort persistence.
14. Frontend: `MonitorForm` type selector + type-specific fields; cards show real bar.
15. Acceptance (one monitor per type + rollup/bars/series/incident/ack; `0002` on real P1 DB) + final review.

---

## 12. Changelog — v2 (spec-review hardening)

**Must-fix:** migration runner restructured to version-ordered (comment-strip alone couldn't apply 0002) — §3.1/§4/§11; `incidents.acknowledged` column added in 0002 (didn't exist) — §4/§7; `CreateMonitorDto` gains `type`, `url`→Optional, per-type validation replaces url-required — §5; rollup uptime uses overlap-based span query (not started-that-day) so midnight-spanning/open incidents count — §6. **Should-fix:** shared `ensure_aggregates` for /bars AND /stats — §6/§7; today never stored / computed live (no future-window miscount) — §6/§7; has_data/retention interaction defined — §7; keyword 2 MiB body cap + lossy UTF-8 — §5; count-weighted 30d/90d avg — §7; bounded read-path backfill + startup catch-up — §6; UptimeBar color bands defined — §8; series bucketing (≤300 slots) — §7; ping fallback semantics — §5. **Completeness (missed):** multi-day rollup catch-up since last aggregate — §6. **Optional:** aggregate uptime_pct kept for P4 (bars use incidents) — §6; detail tiles 24h/7d/30d/90d — §8; DNS canonical string forms — §5; list sort default+persistence — §8; incident-span shading source clipped to range — §8; bar end labels + legend — §8.

*End of P2 design spec.*
