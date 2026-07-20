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
```
ReportSummary {
  period: String,            // "2026-03"
  label: String,             // "March 2026"
  generated_at: i64,
  fleet: FleetReport {
    uptime_pct: Option<f64>,           // downtime-weighted over active monitors (§4.2)
    uptime_delta: Option<f64>,         // vs prior month, None if no prior data
    incidents: i64,
    downtime_seconds: i64,
    mttr_seconds: Option<i64>,         // mean resolved-incident duration
    longest_outage: Option<{ monitor: String, seconds: i64 }>,
    monitors_total: i64,
    clean_monitors: i64,               // had-data monitors with 0 counted downtime
    ssl_alerts: i64,                   // notification_log ssl_* in window
    domain_alerts: i64,                // notification_log domain_expiring in window
    expiring_soon: Vec<ExpiryItem>,    // outlook snapshot at generation (§4.5)
  },
  monitors: Vec<MonitorReport {
    id: i64, name: String, r#type: String,
    uptime_pct: Option<f64>, incidents: i64, downtime_seconds: i64,
    mttr_seconds: Option<i64>, avg_ms: Option<i64>, p95_ms: Option<i64>,
    end_status: String,                // "up" | "down" at period_end (§4.4)
  }>,
  incidents: Vec<ReportIncident {      // every incident overlapping the month
    monitor_name: String, started_at: i64, resolved_at: Option<i64>,
    duration_seconds: Option<i64>, cause: Option<String>,
    status_code: Option<i64>, error_message: Option<String>,
  }>,
}
ExpiryItem { monitor: String, kind: "ssl"|"domain", days_remaining: Option<i64>, flag: "expiring"|"invalid"|"unknown" }
```

### 4.2 Uptime / downtime / clean (durable, maintenance-excluding — mirrors the digest)
Per non-paused monitor, over `(ds, de) = month_bounds(period)`:
- Fetch incident overlap spans: `started_at < de AND (resolved_at IS NULL OR resolved_at > ds)`
  → `uptime::Span{start, end}` (open incidents clip to `de` inside `compute`).
- `had_any = has_checks_row(id, ds, de) || (is_heartbeat && last_ping_at.is_some())`; skip
  no-data monitors (excluded from fleet numerator **and** denominator).
- `maint = resolve::maintenance_intervals(&windows, id, &tags, ds, de)`;
  `u = uptime::compute(&spans, ds, de, had_any, &maint)`;
  `eff_denom = Σ subtract_intervals((ds,de), &maint)`.
- Per-monitor `uptime_pct = u.uptime_pct`, `downtime_seconds = u.downtime_seconds`.
- Fleet: accumulate `total_down += u.downtime_seconds`, `total_denom += eff_denom`,
  `clean += (u.downtime_seconds == 0)`; `fleet.uptime_pct = if total_denom>0
  { Some(round2((1 - total_down/total_denom)*100)) } else { None }`. Identical formula to the
  digest (proven correct there).

### 4.3 Per-monitor incidents / MTTR / longest outage
From the incidents overlap set (JOIN monitors for name):
- Per-monitor `incidents` = count overlapping the month; `mttr_seconds` = mean of
  `(resolved_at - started_at)` over **resolved** incidents (None if none resolved).
- `duration_seconds`: resolved → `resolved_at - started_at`; open → `de - started_at` (clip
  to window). Fleet `mttr_seconds` = mean over all resolved incidents; `longest_outage` = the
  single incident with max duration (monitor named), None if no incidents.

### 4.4 Response stats + end status
- `avg_ms`: sample-weighted mean of `check_aggregates_daily.avg_response_ms` over the month's
  days: `Σ(avg*sample_count)/Σ(sample_count)`, rounded; None if no aggregate rows.
- `p95_ms`: **best-effort** from raw `checks.response_time_ms` in the window; **None** when
  those checks are pruned (raw retention ~30d). Rendered `—`. **Documented boundary:** p95 is
  reliable only for months whose raw checks are within retention at generation time.
- `end_status`: `"down"` if an incident was open at `period_end` (`started_at < de AND
  (resolved_at IS NULL OR resolved_at >= de)`), else `"up"`.

### 4.5 Fleet extras
- `uptime_delta = fleet.uptime_pct - prior_month_uptime`. Prior month = `period − 1 month`.
  `prior_month_uptime`: read the prior month's cached `summary_json.fleet.uptime_pct` if a
  `reports` row exists, else compute it live via a **`fleet_uptime_for(state, prior) ->
  Option<f64>`** helper (fleet uptime only — no delta, no per-monitor, so **no recursion**),
  else None → `uptime_delta = None`. `compute` uses `fleet_uptime_for` for the current month's
  fleet uptime too (single source).
- `ssl_alerts` / `domain_alerts`: `COUNT(*) FROM notification_log WHERE sent_at >= ds AND
  sent_at < de AND trigger IN (...)` — ssl: `('ssl_expiring','ssl_invalid')`, domain:
  `('domain_expiring')`.
- `expiring_soon`: snapshot at generation of `ssl_certs`/`domain_info` for monitors with the
  add-on enabled, filtered like the digest (SSL: `is_valid==Some(false)`→"invalid" or
  `days_remaining <= max(ssl_alert_days)`→"expiring"; domain: `queryable==Some(false)`→
  "unknown" or `<= max(domain_alert_days)`→"expiring"). **Documented boundary:** cert/domain
  tables hold only the latest snapshot (no history), so for a back-filled old month this
  reflects **current** cert state, not month-end.

---

## 5. Generation & idempotency — `report::generate(state, period) -> anyhow::Result<Report>`
1. `summary = compute(state, period)`.
2. UPSERT the `reports` row keyed by `period_start` (`INSERT ... ON CONFLICT(period_start) DO
   UPDATE SET label, generated_at, summary_json, emailed_at=NULL`) — regenerate overwrites and
   clears the emailed marker.
3. Emit `Event::ReportGenerated { id, label }` on `state.bus` (§11).
4. Return the `Report`. Auto-email (§9) is invoked by the **scheduler** after generation, not
   inside `generate` (so on-demand generation doesn't surprise-email); the on-demand API path
   generates without emailing, and the operator uses "Email now" explicitly.

---

## 6. HTML rendering — `report::render_html(&ReportSummary) -> String` (one renderer)
A **self-contained** HTML string (no external assets), built by a small hand-rolled string
builder (the codebase has no templating crate; report content is tables + numbers):
- Inline `<style>`: the navy theme (§11.1 tokens), plus a **`@media print`** block (light
  background, page breaks between sections) so `Ctrl-P → Save as PDF` yields a clean document.
- Cover/period header (label, date range, generation timestamp, app version).
- Fleet hero band (mono numerals): uptime % + delta (▲/▼), incidents, downtime, MTTR, longest
  outage, monitors total/clean, ssl/domain alerts, expiring-soon.
- Per-monitor table (sorted worst-uptime-first, per §13.1).
- Incident log (grouped by monitor, chronological).
- Cert/domain outlook (the `expiring_soon` list, flagging warning-window items).
- **Inline SVG sparklines** are optional/deferred for v1 if they add risk — the tables are the
  core; a small per-fleet or per-monitor daily-uptime sparkline may be added. (Plan decides;
  spec requires at least the tables + hero band.)
- All values HTML-escaped. **This single renderer is the source for both the export file and
  the in-app view** — they are guaranteed identical.

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
  **Delete**. Selecting a card opens the full report in the main area, **embedded via
  `<iframe srcdoc={html}>`** (fetched from `/api/reports/:id/html`) so the report's inline CSS
  is style-isolated from the app.
- **`api.ts`:** `listReports()`, `getReport(id)`, `generateReport(period)`,
  `reportHtml(id)` (returns HTML text), `emailReport(id)`, `deleteReport(id)` + a `ReportCard`
  / `ReportSummary` TS interface.

---

## 8. Scheduler — `report::run(state)` (monthly, UTC)
Mirrors `digest::run`: a loop that wakes every `report.tick_seconds` (default 300) and, if
`report_auto_generate`, evaluates `should_generate`:
```
should_generate(now, day_of_month, time_offset, last_period) ->
   let today = utc date(now)
   let eff_day = min(day_of_month, days_in_month(today))   // clamp: 31 in April → 30
   today.day() >= eff_day                                  // >= not ==, so a passed/short day still fires
   && now >= start_of_today_utc + time_offset
   && last_period < prior_month(now)                       // "YYYY-MM" string compare; fire-once guard
```
The `>=` + clamp + `last_period` guard together guarantee the prior month is generated exactly
once, even when `report_day_of_month` exceeds the current month's length or the target day was
missed (app down) — the first qualifying tick fires, then the marker suppresses repeats.
On fire: `let period = prior_month(now)` (the month that just ended); `generate(state, period)`;
then auto-email (§9); then advance the marker `report.last_generated_period = period` (the
monthly analog of the digest's date marker). Catch-up on restart: if the app was down on
day-of-month N and starts later that day with the marker behind the prior month, it fires.
`report.last_generated_period` is **seeded to the prior month on a fresh instance** (raw
`fetch_optional` absence check, like the digest marker) so a fresh install doesn't back-fire.
Spawned in `main.rs` alongside `digest::run`. `should_generate`/`prior_month`/`month_bounds`
are pure and unit-tested.

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

## 11. Events
Add `Event::ReportGenerated { id: i64, label: String }` to `events.rs` (tag
`report_generated`), broadcast from `generate`. The frontend Reports screen refreshes its list
on this SSE event (store subscription), matching how `MaintenanceChanged`/`cert_updated` drive
live updates.

---

## 12. Module / file structure
- **New:** `crates/vigil/migrations/0006_reports.sql`; `crates/vigil/src/report/mod.rs`
  (Report model, ReportSummary + sub-structs, `month_bounds`/`month_label`/`prior_month`,
  `generate`), `report/compute.rs` (`compute`, `fleet_uptime_for`), `report/html.rs`
  (`render_html`), `report/scheduler.rs` (`run`/`tick_once`/`should_generate`);
  `crates/vigil/src/api/reports.rs`; `web/src/components/Reports.tsx`.
- **Edits:** `lib.rs` (`pub mod report;`), `main.rs` (spawn `report::scheduler::run`),
  `api/mod.rs` (`pub mod reports;` + routes), `events.rs` (`ReportGenerated`),
  `settings_store.rs` + `api/settings.rs` (report_* keys), `web/src/api.ts` (report fns +
  interfaces + `Settings`), `web/src/App.tsx` + `web/src/components/Rail.tsx` (RailView + nav +
  Switch), `web/src/components/Settings.tsx` (report block), `web/src/store.ts` (subscribe to
  `report_generated`).

---

## 13. Testing
- **compute:** over seeded incidents/aggregates/certs/notification_log — fleet uptime
  (downtime-weighted, maintenance-excluded), per-monitor rows, MTTR, longest outage,
  `uptime_delta` (with and without a prior month), ssl/domain alert counts, month-end
  `expiring_soon`, `end_status` (open incident at period_end → down); a **pruned-month → p95
  None** case; a monitor with no data excluded from fleet weighting.
- **month math:** `month_bounds`/`prior_month`/`month_label` incl. year rollover (Jan→Dec
  prior), UTC.
- **generate:** idempotent overwrite (regenerate same period updates the one row, clears
  `emailed_at`); emits `ReportGenerated`.
- **scheduler:** `should_generate` decision table (right day/time/not-yet-generated), marker
  advance, fresh-instance seed (no back-fire).
- **email:** fan-out via `RecordingTransport` to `report_recipients`, `trigger='report'` audit
  row, `emailed_at` set on success; no-recipients audit + non-fatal.
- **html:** `render_html` output contains the label, fleet uptime, a `@media print` block, and
  a per-monitor row; HTML-escaping of a name with `<`.
- **api:** generate (valid + future-rejected 422), list headline, get_one parses summary, html
  content-type `text/html`, email path.
- **frontend:** Reports screen renders cards from `listReports`; generate posts the period;
  the iframe view fetches `/reports/:id/html`; settings report block PUTs the keys.
- Full suite `--test-threads=1`; **one migration (0006)**; rustls-only; **no new crates**
  (`chrono` already in-tree); tsc + vite build clean.

---

## 14. Task decomposition preview (~9 tasks; writing-plans finalizes)
1. Migration 0006 + `Report` model + `month_bounds`/`month_label`/`prior_month` + tests.
2. `report/compute.rs` — `compute` + `fleet_uptime_for` + tests (the big one).
3. `report/html.rs` — `render_html` + tests.
4. `report/mod.rs` — `generate` (upsert + event) + `ReportGenerated` event + tests.
5. `api/reports.rs` — list/get/generate/html/email/delete + tests.
6. `report/scheduler.rs` — `should_generate`/`tick_once`/`run` + seed + spawn + tests.
7. Auto-email wiring (reuse send_email_via_channel, `report` trigger, `emailed_at`) + tests.
8. Settings (`report_*` keys: store + API + TS) + tests.
9. Frontend: Reports screen + rail nav + api.ts + settings block + SSE refresh + tests.
   (+ live acceptance & merge as the final task.)

---

## 15. Documented boundaries (recap)
- **UTC** months + generation time (no local-tz).
- **HTML-only**; PDF via browser print; auto-email sends inline HTML (no attachment). HTML
  rendered on demand from `summary_json` (no stored files; `html_path`/`pdf_path` NULL).
- **p95** only for months whose raw checks are within retention (`—` otherwise); **avg** is
  durable from aggregates.
- **`expiring_soon`** reflects current cert/domain state at generation (no cert history), so a
  back-filled old month shows today's outlook.
- **delta** needs a prior month with data (`—` otherwise).
- Reports are fleet-wide, UTC calendar months; on-demand generation never auto-emails (explicit
  "Email now" only).

---

*End of P4.4 spec. §3–§9 define behavior, §10–§12 settings/events/structure, §13 testing —
build-ready for the implementation plan.*
