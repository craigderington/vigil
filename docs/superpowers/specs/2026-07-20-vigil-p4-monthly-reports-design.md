# Vigil P4.4 — Monthly Incident Reports — Design Spec

> Sub-project 4 of the P4 series (CLAUDE.md §13). Auto-generates a monthly report
> for the prior month (fleet + per-monitor uptime, incident log, cert/domain outlook),
> viewable in-app, exportable as self-contained HTML (print → PDF), and auto-emailed.
> Largest P4 sub-project but one cohesive subsystem. Same rigor as the P4.2/P4.3 specs.

---

## 1. Goals & Non-Goals

**Goals**
- On the 1st of each month (configurable), auto-generate a report for the month just
  ended, computed from **durable** tables (`incidents`, `check_aggregates_daily`,
  `ssl_certs`, `domain_info`, `notification_log`) so it is cheap, reproducible, and works
  for any historical month even after raw checks are pruned.
- On-demand generation for **any past month** (back-fill), idempotent per month.
- Cache the computed metrics as `summary_json` on a `reports` row so re-opening and export
  are instant and deterministic.
- **In-app view** (a Reports screen: month-card grid + full report), **self-contained HTML
  export** (inline CSS + inline SVG, print stylesheet for Ctrl-P → PDF), and **auto-email**
  of the inline HTML to configured recipients.

**Non-Goals (v1)** (all user-confirmed)
- **No server-side PDF rendering** — Vigil is a headless container with no browser. PDF is
  produced by the operator via browser print (the HTML carries a print stylesheet). No PDF
  file, no email attachment.
- **No local timezone** — months and the generation time are **UTC** (consistent with the
  rest of the app; same call as the P4.3 digest). CLAUDE.md §13.4's "local" is superseded.
- No `report_formats` setting — with HTML-only there is nothing to choose.
- No stored HTML/PDF files — the HTML is rendered on demand from `summary_json` (the
  `html_path`/`pdf_path` columns exist per the §9 schema but stay `NULL` in v1).
- No per-monitor report scoping — reports are always fleet-wide for a month.

---

## 2. Context — what exists / what's reused

Verified against the tree. **No reports code or table exists** — genuinely new. Reused:
- **Digest compute pattern** (`digest::build`, P4.3): per-monitor uptime from the durable
  `incidents` table via `uptime::compute(spans, ds, de, had_any_check, &maintenance)` +
  `resolve::{maintenance_intervals, subtract_intervals, parse_tags}` + `active_windows`; the
  incidents overlap query; the expirations snapshot logic; `round2`; `fmt_ts`. A report does
  this over a **month** window with **per-monitor rows** (the digest only aggregates fleet-wide).
- **`dispatch::send_email_via_channel(transport, config_json, subject, body_text, body_html)`**
  (P4.3-extracted) — the auto-email reuses it with `Some(html)`.
- **Digest scheduler shape** (`digest::run`/`tick_once`/`should_send` + a settings marker) —
  mirrored monthly.
- **Settings/API/screen conventions** — `settings_store` typed helpers + `api/settings.rs`
  DTO; `api/mod.rs` route table; the SolidJS screen pattern (`Incidents.tsx`/`Maintenance.tsx`
  + `api.ts` + `App.tsx`/`Rail.tsx` routing).
- **Durable tables:** `incidents` (kept indefinitely) → uptime/downtime/MTTR/incident-log for
  any month; `check_aggregates_daily` (kept indefinitely, `avg_response_ms`+`sample_count`) →
  monthly avg response; `notification_log` → cert/domain alert counts; `ssl_certs`/`domain_info`
  (one latest row per monitor) → current expiry outlook.

---

## 3. Data model — migration `0006_reports.sql`

First migration since P4.2's `0005`. The §9 `reports` table with **UTC** period bounds:

```sql
CREATE TABLE reports (
  id            INTEGER PRIMARY KEY,
  period_start  INTEGER NOT NULL,   -- first day of month, 00:00:00 UTC (epoch secs)
  period_end    INTEGER NOT NULL,   -- first day of NEXT month, 00:00:00 UTC (exclusive)
  label         TEXT NOT NULL,      -- "March 2026"
  generated_at  INTEGER NOT NULL,
  summary_json  TEXT NOT NULL,      -- cached ReportSummary (§4)
  html_path     TEXT,               -- reserved (v1: NULL, HTML rendered on demand)
  pdf_path      TEXT,               -- reserved (v1: NULL, no server-side PDF)
  emailed_at    INTEGER,            -- set when auto-emailed / emailed on demand
  UNIQUE(period_start)
);
```

`UNIQUE(period_start)` makes generation idempotent per month (upsert overwrites). Rust model
`Report` (manual or derived `FromRow`) mirrors the columns; `Ts = i64`.

---

## 4. Compute — `report::compute(state, period: &str) -> anyhow::Result<ReportSummary>`

`period` is `"YYYY-MM"`. **UTC month window** via a new helper
`month_bounds(period) -> (Ts, Ts)` = (first-of-month 00:00 UTC, first-of-next-month 00:00
UTC, exclusive), computed with `chrono` (already in-tree via `rollup`). `month_label(period)`
→ `"March 2026"`.

### 4.1 `ReportSummary` (cached as `summary_json`; all `Serialize`/`Deserialize`)
(CLAUDE.md §13.2's JSON is *illustrative*; this Rust/epoch shape is canonical — the TS
interface mirrors it.)
```
ReportSummary {
  period: String,            // "2026-03"
  label: String,             // "March 2026"
  generated_at: i64,
  fleet: FleetReport {
    uptime_pct: Option<f64>,           // downtime-weighted over reporting monitors (§4.2)
    uptime_delta: Option<f64>,         // round2; None if either month's uptime is None
    incidents: i64,
    downtime_seconds: i64,
    mttr_seconds: Option<i64>,         // mean duration of incidents RESOLVED in-window
    longest_outage: Option<{ monitor: String, seconds: i64 }>,  // max in-month clipped duration
    monitors_total: i64,               // = reporting monitors (non-paused, had-data) — clean's denom
    clean_monitors: i64,               // reporting monitors with uptime_pct.is_some() AND downtime 0
    ssl_alerts: i64,                   // DISTINCT ssl alert events in window (§4.5)
    domain_alerts: i64,                // DISTINCT domain alert events in window
    expiring_30d: i64, expiring_60d: i64,  // fleet counts of certs/domains due within horizon
  },
  cert_outlook: Vec<ExpiryItem>,       // FULL snapshot: one row per ssl/domain-enabled monitor (§4.5)
  monitors: Vec<MonitorReport {        // ONE ROW PER MONITOR (incl paused/no-data, tagged)
    id: i64, name: String, r#type: String,
    uptime_pct: Option<f64>, incidents: i64, downtime_seconds: i64,
    mttr_seconds: Option<i64>, avg_ms: Option<i64>, p95_ms: Option<i64>,
    end_status: String,                // "up" | "down" | "paused" | "no data" (§4.4)
  }>,
  incidents: Vec<ReportIncident {      // every incident overlapping the month
    monitor_name: String, started_at: i64, resolved_at: Option<i64>,
    duration_seconds: Option<i64>,     // IN-MONTH clipped duration (§4.3)
    cause: Option<String>, status_code: Option<i64>, error_message: Option<String>,
  }>,
}
ExpiryItem { monitor: String, kind: "ssl"|"domain", days_remaining: Option<i64>, flag: "ok"|"expiring"|"invalid"|"unknown" }
```

### 4.2 Uptime / downtime / clean (durable, maintenance-excluding — single pass)
Iterate ALL monitors, over `(ds, de) = month_bounds(period)`; build one `MonitorReport` per
monitor and accumulate the fleet numbers in the **same pass** (no second pass — mirrors the
digest's proven approach, `digest.rs`):
- **A `MonitorReport` row is emitted for EVERY monitor** (CLAUDE.md §13.1 "one row each"). A
  paused monitor → `end_status:"paused"`, its metrics computed but it does **not** enter the
  fleet weighting. A monitor with no month data → `end_status:"no data"`, null metrics.
- **`had_any` MUST use a DURABLE signal (review must-fix M1):** raw `checks` are pruned at
  `retention.raw_days` (~30d), so gating on them would blank every back-filled month AND the
  first pruned days of even the on-time report. Use:
  `had_any = EXISTS(SELECT 1 FROM check_aggregates_daily WHERE monitor_id=? AND day >= '<YYYY-MM-01>' AND day < '<next-YYYY-MM-01>')  OR  has_incident_overlap(id, ds, de)`.
  (`check_aggregates_daily` + `incidents` are kept indefinitely; a heartbeat that missed shows
  via its incident. A heartbeat with zero incidents and no checks has no durable trace → "no
  data" — acceptable.) Do NOT gate on raw `checks` or `last_ping_at`; reserve raw `checks`
  solely for best-effort p95 (§4.4).
- For each **non-paused, had-data** monitor: fetch incident overlap spans
  (`started_at < de AND (resolved_at IS NULL OR resolved_at > ds)` → `uptime::Span{start,end}`,
  open clips to `de` in `compute`); `maint = resolve::maintenance_intervals(&windows, id, &tags, ds, de)`;
  `u = uptime::compute(&spans, ds, de, true, &maint)` (`had_any` already established);
  `eff_denom = Σ subtract_intervals((ds,de), &maint)`. Per-monitor `uptime_pct = u.uptime_pct`,
  `downtime_seconds = u.downtime_seconds`.
- **Fleet from these same accumulators** (single source): `total_down += u.downtime_seconds`;
  `total_denom += eff_denom`; `reporting += 1`; `clean += (u.uptime_pct.is_some() && u.downtime_seconds == 0)`
  (a wholly-maintenance month yields `uptime_pct None, downtime 0` — **not** clean, review O5).
  `fleet.uptime_pct = if total_denom>0 { Some(round2((1 - total_down/total_denom)*100)) } else { None }`;
  `fleet.monitors_total = reporting` (shares `clean_monitors`'s denominator).

### 4.3 Incidents / MTTR / longest outage (clip BOTH ends — review S4)
From the incidents overlap set (JOIN monitors for name). For every counted interval, clip to
the window on **both** ends: `dur_in_month = min(resolved_at.unwrap_or(de), de) - max(started_at, ds)`.
- Per-monitor & fleet `incidents` = count overlapping the month; `ReportIncident.duration_seconds`
  = `dur_in_month` (the incident-log entry keeps the true `started_at`/`resolved_at`).
- `longest_outage` = the incident with the max `dur_in_month` (monitor named); None if none.
- **`mttr_seconds` = mean of `(resolved_at - started_at)` over incidents RESOLVED WITHIN the
  window** (`ds <= resolved_at < de`) — excludes still-open and next-month spill-overs, so it
  can't cross-attribute or exceed the window. Per-monitor MTTR likewise; None if none resolved
  in-window.

### 4.4 Response stats + end status
- **`avg_ms` (review S5):** weight daily `avg_response_ms` by **`up_count`** (the count of
  non-null-response samples that produced that day's average — `sample_count` = up+down
  over-counts because down/timeout probes write `response_time_ms=NULL`), over rows with a
  non-null `avg_response_ms`: `round(Σ(avg_response_ms*up_count) / Σ(up_count))`; None if no
  such rows. **Documented residual bias:** `up_count` counts *successful* probes; without a
  durable non-null-response count this is the closest durable weighting (exact reconstruction
  isn't possible retroactively).
- **`p95_ms` (review S6):** computed from raw `checks.response_time_ms` in the window **only
  when the ENTIRE month is within retention at generation** (`ds >= now - retention_days*86400`),
  else `None` → `—`. **Documented boundary:** retention prunes by age, so the default
  auto-generated prior month (retention 30, generated on the 1st) is *partially* pruned →
  `p95 = None`; p95 is therefore excluded from the determinism guarantee (avg/uptime are not).
- **`end_status`:** `"paused"` if `is_paused`; else `"no data"` if `!had_any`; else `"down"` if
  an incident was open at `period_end` (`started_at < de AND (resolved_at IS NULL OR resolved_at >= de)`),
  else `"up"`.

### 4.5 Fleet extras
- **`uptime_delta` (review S7/O3/O4):** always recompute the prior month **live** (deterministic
  — never read a possibly-deleted cached report): `prior = prior_month(period)`;
  `prior_uptime = fleet_uptime_for(state, prior)`. `fleet_uptime_for(state, "YYYY-MM") ->
  Option<f64>` runs the §4.2 fleet loop for that month and returns only `fleet.uptime_pct` (calls
  neither `compute` nor itself → **no recursion**). `compute` derives the *current* month's fleet
  uptime from its own §4.2 accumulators — **not** from `fleet_uptime_for` (which is for the prior
  month only). `uptime_delta = match (current, prior) { (Some(c), Some(p)) => Some(round2(c - p)), _ => None }`.
- **`ssl_alerts` / `domain_alerts` (review S3):** `deliver()` writes one `notification_log` row
  **per channel per send** (success and failure both), all sharing one `sent_at`, so a plain
  `COUNT(*)` inflates by the channel count. Count DISTINCT alert *events*:
  `COUNT(DISTINCT monitor_id || '|' || trigger || '|' || sent_at) WHERE sent_at >= ds AND sent_at < de AND trigger IN (...)`
  — ssl: `('ssl_expiring','ssl_invalid')`; domain: `('domain_expiring')`. (Trigger strings verified
  against `Trigger::as_str`.) These are "alert notifications raised"; a monitor with no attached
  channel raises none — documented.
- **`cert_outlook` (full snapshot — review S9):** one `ExpiryItem` per monitor with
  `ssl_check_enabled` / `domain_check_enabled`, from the latest `ssl_certs`/`domain_info` row —
  `flag`: SSL `invalid` if `is_valid==Some(false)`, else `expiring` if `days_remaining <= max(ssl_alert_days)`,
  else `ok`; domain `unknown` if `queryable==Some(false)`, else `expiring`/`ok` vs `max(domain_alert_days)`.
  (Every tracked cert/domain appears, per §13.1, not just the flagged subset.) `fleet.expiring_30d`
  / `expiring_60d` = count of items with `days_remaining <= 30` / `<= 60`. **Documented boundary:**
  cert/domain tables hold only the *latest* snapshot (no history), so for a back-filled old month
  this reflects **current** cert state at generation, not month-end.

---

## 5. Generation & idempotency — `report::generate(state, period) -> anyhow::Result<Report>`
1. `summary = compute(state, period)`.
2. UPSERT the `reports` row keyed by `period_start` (`INSERT ... ON CONFLICT(period_start) DO
   UPDATE SET label, generated_at, summary_json, emailed_at=NULL`) — regenerate overwrites and
   clears the emailed marker.
3. Return the `Report`. Auto-email (§9) is invoked by the **scheduler** after generation, not
   inside `generate` (so on-demand generation doesn't surprise-email); the on-demand API path
   generates without emailing, and the operator uses "Email now" explicitly.

No SSE event is emitted (review O10 — an inaccurate precedent claim + YAGNI for a once-a-month
event; the P4.3 digest added no event either). The Reports screen refetches its list on mount
and after each local Generate / Delete / Email now (§7.2).

---

## 6. HTML rendering — `report::render_html(&ReportSummary) -> String` (one renderer)
A **self-contained** HTML string (no external assets), built by a small hand-rolled string
builder (the codebase has no templating crate; report content is tables + numbers):
- Inline `<style>`: the navy theme (§11.1 tokens), plus a **`@media print`** block (light
  background, page breaks between sections) so `Ctrl-P → Save as PDF` yields a clean document.
- Cover/period header (label, date range, generation timestamp, app version).
- Fleet hero band (mono numerals): uptime % + delta (▲/▼), incidents, downtime, MTTR, longest
  outage, monitors total/clean, ssl/domain alerts, `expiring_30d`/`expiring_60d`.
- Per-monitor table (all rows; **pre-sorted worst-uptime-first** in the export, per §13.1).
- Incident log (grouped by monitor, chronological; in-month clipped durations).
- **Cert/domain outlook** — the full `cert_outlook` list as a table (every tracked cert/domain),
  flagged rows (`expiring`/`invalid`/`unknown`) highlighted.
- **Inline SVG sparklines** are OPTIONAL for v1 (tables + hero band are the required core); if
  added, base a per-monitor sparkline on the durable daily **avg response time** (§13.1's
  optional trend), not uptime.
- All values HTML-escaped (a name with `<` must not break the layout). **This single renderer is
  the source for both the export file and the in-app view** — guaranteed identical.
- **Documented deviation (review O11):** because the in-app view embeds this static HTML
  (§7.2), the per-monitor table is **not interactively sortable in-app** in v1 — it ships
  pre-sorted worst-uptime-first (matching the export). Interactive sort is a future enhancement.

---

## 7. In-app view + API surface

### 7.1 API (`crates/vigil/src/api/reports.rs`, mounted under `/api`)
| Route | Handler | Returns |
|---|---|---|
| `GET /reports` | `list` | `[{id, label, period_start, period_end, generated_at, emailed_at, headline:{uptime_pct, incidents, downtime_seconds}}]`, newest first (headline from `summary_json.fleet`). |
| `GET /reports/:id` | `get_one` | full `{…row, summary: ReportSummary}` (parsed). |
| `POST /reports/generate` | `generate` | body `{period:"YYYY-MM"}` → the report (idempotent; validates format + not-future). |
| `GET /reports/:id/html` | `html` | `text/html` self-contained report (serves both the in-app iframe and "Export HTML" download). |
| `POST /reports/:id/email` | `email` | emails the HTML to `report_recipients` (or body `channel_ids[]` override); sets `emailed_at`. |
| `DELETE /reports/:id` | `delete` | `{ok:true}`. |

Handlers use the existing `ApiResult<T>`/`db_err`/`now` conventions. `generate` rejects a
malformed or **future** period (422).

### 7.2 Frontend — new Reports screen
- **Rail:** add `"reports"` to `RailView` (`Rail.tsx` + `App.tsx`), a `NAV_ITEMS` entry + a
  document/chart `ICON_PATHS` glyph, whitelist it in `App.tsx`'s `onNavigate`, and a
  `<Match when={view()==="reports"}>` → `<Reports/>`. (Reports is currently missing from the nav.)
- **`components/Reports.tsx`:** month-card grid (headline uptime % / incidents / downtime,
  newest first) via `createResource(api.listReports)`; a **Generate** month-picker (back-fill
  any past month); per-card **Export HTML** (opens `/api/reports/:id/html`) · **Email now** ·
  **Delete**. The list `refetch()`es after each local Generate / Delete / Email now (no SSE —
  §5). Selecting a card opens the full report in the main area, **embedded via
  `<iframe srcdoc={html}>`** (fetched from `/api/reports/:id/html`) so the report's inline CSS
  is style-isolated from the app.
- **`api.ts`:** `listReports()`, `getReport(id)`, `generateReport(period)`,
  `reportHtml(id)` (returns HTML text), `emailReport(id)`, `deleteReport(id)` + a `ReportCard`
  / `ReportSummary` TS interface.

---

## 8. Scheduler — `report::run(state)` (monthly, UTC)
Mirrors `digest::run`: a loop that wakes every `report.tick_seconds` (default 300) and, if
`report_auto_generate`, calls `tick_once`. **Month helpers** (pure, string-based, one source of
Dec→prev-year rollover — review O6): `month_of(epoch)->"YYYY-MM"`, `prior_month(&str)->String`,
`next_month(&str)->String`, `month_bounds(&str)->(Ts,Ts)`, `month_label(&str)->String`.

**Due gate** (pure, unit-tested):
```
should_run_today(now, day_of_month, time_offset) ->
   let today = utc date(now)
   let eff_day = min(day_of_month, days_in_month(today))   // clamp: 31 in April → 30
   today.day() >= eff_day && now >= start_of_today_utc + time_offset
```

**`tick_once` — ordered backfill with email-retry (review S1 + S2):**
```
if !report_auto_generate: return
let now = now();  let target = prior_month(month_of(now))    // the month that just ended
if !should_run_today(now, day_of_month, time_offset): return
let mut cursor = report.last_generated_period                // seeded to `target` on fresh install
while next_month(cursor) <= target {                         // "YYYY-MM" string compare
    let p = next_month(cursor);
    generate(state, p);                                      // idempotent (§5)
    let outcome = if report_recipients non-empty { auto_email(state, p) } else { NothingToSend };
    match outcome {
        Delivered | NothingToSend => { set report.last_generated_period = p; cursor = p; }
        AllFailed => break,   // leave marker at cursor; the next tick re-generates + re-emails p
    }
}
```
This **backfills every missing month in order** (fixes the S1 cross-month gap: a month the app
was down for is caught on the next day-N tick, not silently lost), and **advances the marker
only on a delivered / nothing-to-send outcome** — a transient SMTP failure leaves the marker so
the next tick retries `generate`+`auto_email` for that month (matches the P4.3 digest's
`SendOutcome` policy; empty recipients = `NothingToSend` = advance). `generate`'s idempotency
(§5) makes re-running a month safe.

**Fresh-instance seed:** `report.last_generated_period` is seeded to `target = prior_month(month_of(now))`
on a fresh install (raw `fetch_optional` absence check, like the digest marker) so a new install
doesn't back-fire reports for months it wasn't monitoring. Spawned in `main.rs` alongside
`digest::run`. The month helpers + `should_run_today` are pure and unit-tested (incl. year
rollover, short-month clamp, and a multi-month backfill).

---

## 9. Auto-email — reuse `send_email_via_channel`
On scheduler-triggered generation (and on the explicit `POST /reports/:id/email`): resolve
`report_recipients` → active email channels (same query as `digest::send`), render
`render_html(summary)`, and `send_email_via_channel(transport, cfg, subject, body_text,
Some(html))` per recipient. `subject = "Vigil monthly report — {label} — {uptime}% uptime"`;
`body_text` = a short plaintext fallback. Each send writes a `notification_log` row with
`trigger='report'`, `monitor_id=NULL`. On ≥1 success, set `reports.emailed_at = now`. No
recipients / all-fail is logged (audit), non-fatal. Auto-email only fires when
`report_recipients` is non-empty (generate-and-store still happens regardless).

---

## 10. Settings (`report_*` keys)
Threaded through `settings_store.rs` + `api/settings.rs` + the TS `Settings` interface, same
as the digest keys:
| Key | Default | Meaning |
|---|---|---|
| `report_auto_generate` | `true` ("1") | Master switch for the monthly scheduler. |
| `report_day_of_month` | `1` | Day-of-month to generate the prior month. |
| `report_time` | `"08:00"` | Fire time as HH:MM **UTC** offset. |
| `report_recipients` | `[]` | JSON array of email-channel ids for auto-email. |
| `report_tick_seconds` | `300` | Scheduler granularity (not on the DTO). |
| `report.last_generated_period` | *(internal)* | `"YYYY-MM"` fire-once marker (not on the DTO). |

Settings UI: a "Monthly reports" block in `Settings.tsx` (auto-generate toggle, day-of-month,
time labeled UTC, recipient channel checkboxes) modeled on the P4.3 digest block.

---

## 11. Events — none (review O10)
**No `Event` variant is added.** A once-a-month generation doesn't warrant SSE plumbing, and the
P4.3 digest added none either. The Reports screen stays current by refetching its list on mount
and after each local Generate / Delete / Email now (§7.2). (This removes any `events.rs` /
`store.ts` change from the scope.)

---

## 12. Module / file structure
- **New:** `crates/vigil/migrations/0006_reports.sql`; `crates/vigil/src/report/mod.rs`
  (Report model, ReportSummary + sub-structs, the month helpers `month_of`/`prior_month`/
  `next_month`/`month_bounds`/`month_label`, `generate`), `report/compute.rs` (`compute`,
  `fleet_uptime_for`), `report/html.rs` (`render_html`), `report/scheduler.rs`
  (`run`/`tick_once`/`should_run_today` + backfill); `crates/vigil/src/api/reports.rs`;
  `web/src/components/Reports.tsx`.
- **Edits:** **`db.rs` — MUST append `(6, include_str!("../migrations/0006_reports.sql"))` to
  the hardcoded `MIGRATIONS` const (review must-fix M2 — this repo does NOT use `sqlx::migrate!`;
  a bare `.sql` file is otherwise never applied and every handler fails with "no such table:
  reports").** `lib.rs` (`pub mod report;`), `main.rs` (spawn `report::scheduler::run`),
  `api/mod.rs` (`pub mod reports;` + routes), `settings_store.rs` + `api/settings.rs` (report_*
  keys), `web/src/api.ts` (report fns + interfaces + `Settings`), `web/src/App.tsx` +
  `web/src/components/Rail.tsx` (RailView + nav + Switch), `web/src/components/Settings.tsx`
  (report block). **No `events.rs` / `store.ts` change** (no SSE event — §11). If `round2`/
  `fmt_ts` are reused they must be made `pub` (or moved to a shared util) — they are currently
  private in `digest.rs` (review O7).

---

## 13. Testing
- **migration wiring:** after `db::connect()` on a fresh DB, `SELECT ... FROM reports` succeeds
  (proves 0006 is registered in `MIGRATIONS`, M2); a v5→v6 preserving test.
- **durable-gate (M1):** seed ONLY `check_aggregates_daily` + `incidents` for an old month with
  **no** raw `checks` rows → the monitor still appears in `monitors[]` with correct uptime and
  is counted in the fleet (the exact back-fill case raw-checks-gating would blank).
- **compute:** fleet uptime (downtime-weighted, maintenance-excluded, single-pass), per-monitor
  rows incl. paused ("paused") and no-data ("no data") monitors, `monitors_total` = reporting
  count sharing `clean_monitors`'s denom, a wholly-maintenance monitor **not** counted clean
  (O5), MTTR over in-window-resolved only, longest-outage from **both-ends-clipped** duration,
  an incident starting before `ds` clipped (S4), `uptime_delta` (always-live prior, round2, None
  when either None), DISTINCT ssl/domain alert counts (a 2-channel monitor's one alert counts
  once — S3), full `cert_outlook` incl. a healthy "ok" cert + `expiring_30d/60d`, `end_status`
  (open-at-period_end → down), whole-month-in-retention → p95 value vs partially-pruned → p95
  None (S6).
- **month math:** `month_of`/`prior_month`/`next_month`/`month_bounds`/`month_label` incl. **year
  rollover** (Jan↔Dec) and short-month, UTC.
- **generate:** idempotent overwrite (regenerate updates the one row, clears `emailed_at`); no
  event emitted.
- **scheduler:** `should_run_today` decision table (day clamp, time, not-before); **multi-month
  backfill** (marker two months behind → both generated in order — S1); **email-retry** (first
  tick `AllFailed` → marker held; second tick `Delivered` → advances — S2); empty recipients →
  `NothingToSend` advances; fresh-instance seed to `target` (no back-fire).
- **email:** fan-out via `RecordingTransport` to `report_recipients`, `trigger='report'` audit
  row, `emailed_at` set on ≥1 success; no-recipients audit + non-fatal.
- **html:** `render_html` output contains the label, fleet uptime, a `@media print` block, a
  per-monitor row, and the cert-outlook table; HTML-escaping of a name with `<`.
- **api:** generate (valid + future-rejected 422), list headline, get_one parses summary, html
  `content-type: text/html`, email path.
- **frontend:** Reports screen renders cards from `listReports` + refetches after Generate/
  Delete/Email; generate posts the period; the iframe view fetches `/reports/:id/html`; settings
  report block PUTs the keys.
- Full suite `--test-threads=1`; **one migration (0006)**; rustls-only; **no new crates**
  (`chrono` already in-tree); tsc + vite build clean.

---

## 14. Task decomposition preview (~9 tasks; writing-plans finalizes)
1. Migration 0006 + **wire it into `db.rs`'s `MIGRATIONS`** + `Report` model + the month helpers
   (`month_of`/`prior_month`/`next_month`/`month_bounds`/`month_label`) + tests.
2. `report/compute.rs` — `compute` (durable `had_any`, single-pass fleet, both-ends clip,
   corrected avg, gated p95, distinct alerts, full cert_outlook) + `fleet_uptime_for` + tests
   (the big one).
3. `report/html.rs` — `render_html` + tests.
4. `report/mod.rs` — `generate` (upsert, no event) + tests.
5. `api/reports.rs` — list/get/generate/html/email/delete + tests.
6. `report/scheduler.rs` — `should_run_today` + `tick_once` (ordered backfill, email-retry) +
   `run` + seed + spawn + tests.
7. Auto-email wiring (reuse `send_email_via_channel`, `report` trigger, `emailed_at`) + tests.
8. Settings (`report_*` keys: store + API + TS) + tests.
9. Frontend: Reports screen (cards + iframe view + refetch-after-actions) + rail nav + api.ts +
   settings block + tests. (+ live acceptance & merge as the final task.)

---

## 15. Documented boundaries (recap)
- **UTC** months + generation time (no local-tz).
- **HTML-only**; PDF via browser print; auto-email sends inline HTML (no attachment). HTML
  rendered on demand from `summary_json` (no stored files; `html_path`/`pdf_path` NULL). The
  in-app report view embeds that static HTML in an iframe, so the per-monitor table is **not
  interactively sortable in v1** (pre-sorted worst-uptime-first).
- **Durability sources:** uptime/downtime/incidents/MTTR from `incidents`; `had_any` inclusion +
  monthly `avg` from `check_aggregates_daily` — both kept indefinitely. **`p95`** only when the
  *entire* month is within raw-check retention (`—` otherwise; the default auto prior month is
  partially pruned → `p95 = None`); p95 is excluded from the determinism guarantee. **`avg`** is
  `up_count`-weighted (documented residual bias — no durable non-null-response count).
- **Alert counts** come from `notification_log` (DISTINCT events). `notification_log` is not in
  CLAUDE.md §9's "kept indefinitely" set (only aggregates + incidents are); a future log-rotation
  feature would under-count historical alerts — flagged, acceptable for v1.
- **`cert_outlook`** reflects current cert/domain state at generation (tables hold only the
  latest snapshot, no history), so a back-filled old month shows today's outlook, not month-end.
- **Maintenance exclusion** for a back-filled month uses **today's** `active_windows` — windows
  since deleted/edited/toggled won't reflect what was actually suppressed then (parallels the
  P4.2 "current is_active" boundary).
- **Cascade delete:** `incidents`/`check_aggregates_daily` are `ON DELETE CASCADE`, so deleting a
  monitor erases it from any **not-yet-generated** past-month report (already-cached reports
  survive).
- **delta** needs a prior month with data (`—` otherwise), always recomputed live (deterministic).
- Reports are fleet-wide, UTC calendar months; on-demand generation never auto-emails (explicit
  "Email now" only).

---

*End of P4.4 spec. §3–§9 define behavior, §10–§12 settings/events/structure, §13 testing —
build-ready for the implementation plan.*
