# Vigil P4.3 — Notification Throttling & Digest — Design Spec

> Sub-project 3 of the P4 series. Adds **re-notify** (repeat alerts while an outage
> is ongoing) and a **daily digest** email. Builds on the notification subsystem
> shipped in P1/P3. Spec follows the same rigor as the P4.2 (maintenance windows) spec.

---

## 1. Goals & Non-Goals

**Goals**
- **Re-notify:** for an ongoing, unacknowledged outage, re-send the down alert on a
  configurable cadence (default every 6h) until it resolves or is acknowledged — so a
  long outage isn't forgotten after the single initial email.
- Wire the currently-**inert** `incidents.acknowledged` flag to *mean* something:
  acknowledging an incident silences its re-notify reminders.
- **Daily digest:** an optional once-per-day email summarizing the prior day's uptime,
  incidents, and upcoming SSL/domain expirations. Sent even on quiet days (dead-man's
  switch), to a dedicated recipient list.

**Non-Goals (v1)**
- No per-monitor re-notify override (global cadence only, mirroring `cooldown_minutes`).
- No new `Trigger` variant for re-notify — reminders reuse the existing `down` /
  `heartbeat_missed` subscription (user decision).
- No rich HTML digest — plaintext only (rich HTML + PDF is P4.4 monthly reports' job).
- No local-timezone infrastructure — digest scheduling and its "yesterday" window are
  **UTC**, consistent with the rest of the app (see §4.3). A local-tz offset is a
  documented future enhancement.
- No re-notify for cert/domain expiry alerts — those are already fire-once/tiered
  (`cert_scheduler`), not "ongoing outages."

---

## 2. Context — what already exists (must NOT be duplicated)

From the P1/P3 notification subsystem (verified against the tree):

- **Cooldown** — per `(monitor, channel, trigger)`, driven by settings key
  `notify.cooldown_minutes` (default 15). `cooldown::allowed(last_sent, now, mins)`
  compares `now` vs `MAX(notification_log.sent_at)` for that triple
  (`notify/dispatch.rs:195-207`, `cooldown.rs`). CLAUDE.md §7's "cooldown within N
  minutes" is **already done**.
- **Maintenance mute** — `deliver()` short-circuits (no send, no log row) if the monitor
  is under any active maintenance window (`notify/dispatch.rs:162-167`).
- **Per-trigger opt-in** — `monitor_notifications.triggers` JSON array filters which
  triggers each channel receives (`dispatch.rs:186-189`).
- **Cert/domain fire-once tiered alerts** — `cert_scheduler.rs` owns its own
  `alerts::tier` + `alerted_days` bookkeeping, independent of cooldown.
- **`deliver(state, m, trigger, msg, incident_id)`** — the single delivery funnel
  (`dispatch.rs:146`). Two wrappers: `on_transition()` (down/recovered) and
  `send_alert()` (ssl/domain). Every alert routes through `deliver()`, which applies the
  maintenance + cooldown guards and writes `notification_log`.

**Green field for P4.3:** no re-notify loop exists (all alerts are edge-triggered); no
digest of any kind; `incidents.acknowledged` is read only by the incidents API and never
by any notification path.

---

## 3. Feature A — Re-notify (repeat-while-down)

### 3.1 Decisions
- Reminders **reuse the `down` / `heartbeat_missed` subscription** — any channel already
  opted into those triggers gets reminders. One global cadence `notify.renotify_hours`
  (default 6; `0` disables). No new `Trigger` variant, no per-monitor opt-in, **no
  migration**.
- The reminder re-fires through the existing `deliver()` path, so the maintenance mute,
  per-channel cooldown, and `notification_log` write all apply for free — and that log
  row **is** the re-notify clock.

### 3.2 Architecture
- New module `crates/vigil/src/renotify.rs`, task `pub async fn run(state: AppState)`.
- **Tick-based loop**, mirroring `cert_scheduler::run` / `maintenance_windows::run`:
  each iteration reads `settings_store::renotify_tick_seconds` (default **300**), calls
  the testable seam `renotify_once(&state)`, then `sleep(tick.max(1))`.
- Spawned in `main.rs` alongside the other tasks: `tokio::spawn(renotify::run(state.clone()))`
  (module added to the `use vigil::{…}` line and `main.rs:81-92` spawn block).

### 3.3 The scan & fire decision (`renotify_once`)
1. **Disabled short-circuit:** read `renotify_hours`; if `0`, return immediately.
2. **Connectivity gate:** `if state.anchor.current().await == Connectivity::Offline { return }`
   — do not remind about outages while *your own* link is down. Exact precedent:
   `cert_scheduler.rs:216`. (`state.anchor.current()` is fail-open → `Online` if never
   probed; kept warm by the 15s anchor poller.)
3. **Query open reminder-eligible incidents** (a single read):
   ```sql
   SELECT i.id AS incident_id, i.monitor_id, i.started_at
   FROM incidents i
   JOIN monitors m ON m.id = i.monitor_id
   WHERE i.resolved_at IS NULL
     AND i.acknowledged = 0
     AND m.is_paused = 0
     AND m.status = 'down'
   ```
   **`m.status = 'down'` is load-bearing (review must-fix):** when connectivity drops,
   `bulk_set_unknown` flips down monitors to `status='unknown'` but leaves their incidents
   open, and the anchor flips back `Online` (10s TTL / 15s poll) *before* slow monitors are
   re-probed on their own interval. Without this filter, an incident whose monitor is still
   `unknown` in that reconnect window would get a spurious reminder — violating CLAUDE.md §5
   (UNKNOWN suppresses alerting). Heartbeat-missed monitors are `status='down'` too
   (`heartbeat.rs:250`), so both outage kinds are covered; `is_paused=0` stays as
   belt-and-braces.
4. For each row, load the `Monitor` (needed by `deliver`). **If the monitor is gone**
   (deleted between the scan and the load — `fetch_optional` → `None`), **skip it silently**
   (`continue`); never unwrap/panic. (Deleting a monitor FK-cascades its incidents
   (`ON DELETE CASCADE`), so a deleted monitor naturally drops out of the scan on the next
   pass — this handles "monitor deleted mid-outage" with no extra code.) Then compute the
   reminder baseline **scoped to THIS incident** (review must-fix — see §3.7):
   ```sql
   SELECT MAX(sent_at) FROM notification_log
   WHERE incident_id = ? AND trigger IN ('down','heartbeat_missed')
   ```
   `baseline = last_reminder ?? incident.started_at`. Incident-scoping makes the
   `?? started_at` fallback correct-by-construction: a channelless / cooldown-suppressed /
   maintenance-suppressed incident open wrote no row for *this* incident, so the fallback to
   `started_at` fires — instead of silently inheriting a *prior* resolved incident's send
   time. `notification_log.incident_id` is already populated (`emit_opened` and the reminder
   both pass `Some(incident_id)`).
5. **Fire if due:** `if now - baseline >= renotify_hours * 3600`: **re-read the incident**
   and skip if it is now `resolved_at IS NOT NULL` or `acknowledged = 1` (closes the TOCTOU
   between the batch scan and the per-incident SMTP send — a recovery/ack can land mid-pass).
   Otherwise build the reminder `NotifyMsg` (§3.5) and call
   `deliver(&state, &m, down_trigger, &msg, Some(incident_id))`, where
   `down_trigger = if m.r#type == "heartbeat" { HeartbeatMissed } else { Down }`
   (same selection as `engine.rs:155-156`).
6. `deliver()` writes fresh `notification_log` row(s) at `now`, advancing the (now
   incident-scoped) baseline so the next reminder is one interval out. No extra bookkeeping
   table. **Audit contract:** reminders share the initial alert's `incident_id` and log the
   same `trigger` (`down`/`heartbeat_missed`); the 2nd-and-later rows for a given
   `incident_id` are the reminders. This is the documented answer to "why did I get N emails
   for one outage" (§8 asserts it).

### 3.4 Stop conditions — all fall out of the query + existing guards

| Condition | How it stops re-notify |
|---|---|
| Incident resolved | `resolved_at IS NULL` excludes it; `emit_resolved` already closed it. |
| **Acknowledged** | `acknowledged = 0` excludes it. **This is the new teeth on the ack flag.** |
| Monitor paused | `m.is_paused = 0` excludes it. |
| Under maintenance | `deliver()`'s maintenance guard short-circuits (no send, no log row → baseline doesn't advance → retried after window ends). |
| Your connection down | pass-level connectivity gate (§3.3 step 2). |
| Re-notify disabled | `renotify_hours == 0` short-circuit. |
| Monitor not confirmed down | `m.status = 'down'` filter (excludes `unknown`/`pending`/`up`). |

**Acknowledge is terminal-until-resolve (v1):** the acknowledge endpoint is one-way
(`UPDATE incidents SET acknowledged = 1`, `api/incidents.rs:117`); there is no un-acknowledge
path, so an accidental ack silences reminders until the incident resolves. Documented as-is;
if an un-ack endpoint is added later it must reset the re-notify clock to `now()` (not the
stale baseline) to avoid an immediate reminder.

### 3.5 The reminder message (post-render decoration)
The reminder should read as a reminder, not a fresh outage: `Reminder:` subject prefix +
an elapsed line. **Mechanism (revised per review — simpler and covers both triggers):**
do **not** touch the shared `TemplateCtx` / `templates::render` (that would force edits to
the first-alert construction site and every `TemplateCtx`-building test, and its down-arm
branch would miss `heartbeat_missed` reminders). Instead, `renotify_once` renders the base
message via the existing path, then decorates it uniformly, trigger-agnostically:
- `subject = format!("Reminder: {base_subject}")`
- append a line to the body: `"Still down for {elapsed}"` where `elapsed = now - started_at`.

This keeps the first-alert path **byte-identical for free** (§8 has a regression test) and
decorates `down` and `heartbeat_missed` reminders identically. `renotify_once` builds the
reminder `NotifyMsg` from the incident (elapsed from `started_at`), reusing the same
`NotifyMsg` fields `on_transition` uses.

### 3.6 Settings (re-notify)
| Key | Default | Meaning |
|---|---|---|
| `notify.renotify_hours` | `6` | Reminder cadence in hours; `0` = disabled. |
| `notify.renotify_tick_seconds` | `300` | Loop granularity (how often the scan runs). |

Both via `settings_store` helpers + `DEFAULT_*` consts, mirroring `cooldown_minutes` /
`maintenance_tick_seconds` (`settings_store.rs`).

### 3.7 Edge cases & documented v1 boundaries
- **Incident-scoped baseline** — the reminder clock is `MAX(sent_at)` for
  `incident_id = ?` (not monitor-wide), so a flapping monitor's *next* incident never
  inherits a *prior* incident's send time (which caused a spurious immediate reminder in
  the first draft). The existing index `(monitor_id, trigger, sent_at DESC)` no longer
  covers this query, but re-notify volume is tiny (a handful of open incidents scanned
  every ~5 min) — no new index needed.
- **Effective interval = `max(renotify_hours, cooldown_minutes/60)`** — `deliver()`'s
  per-channel cooldown is a backstop. With defaults (6h vs 15m) it never interferes; if
  an operator sets `renotify_hours` below `cooldown_minutes`, the cooldown wins.
  Documented, not guarded.
- **Failed sends restart the clock** — the baseline keys off `MAX(sent_at)` for the
  incident regardless of `success` (mirrors cooldown; `deliver` logs failures too). A
  failed *reminder* won't retry for a full interval. Documented v1 boundary.
- **Channelless open incident** — if no channel subscribes to `down`, `deliver` writes no
  row, baseline stays at `started_at`, and each tick re-attempts a no-op `deliver`. Cheap
  (one SELECT + early return per few minutes); acceptable at single-operator scale.
- **Granularity** — reminders fire within one tick (≤5 min) of the exact interval mark.

---

## 4. Feature B — Daily Digest

### 4.1 Decisions
- **Always sends** on a quiet day (dead-man's switch) — an "all green" email also proves
  the alert pipeline is alive.
- **Dedicated recipients** — `notify.digest_recipients` (JSON array of email-channel ids),
  default `[]`. Digest is effectively off until both `digest_enabled` is true and at
  least one recipient is set.
- **Plaintext** email; **UTC** period + schedule (§4.3).
- Digest **bypasses `deliver()`** — it's fleet-wide, not a per-monitor trigger, so it has
  no cooldown / per-monitor maintenance semantics. It renders + sends directly via the
  SMTP transport and logs its own audit rows.

### 4.2 Architecture
- New module `crates/vigil/src/digest.rs`, task `pub async fn run(state: AppState)`.
- **Tick-based daily scheduler** (there is no midnight-aligned scheduler to reuse; the
  nightly rollup job is a drifting fixed-24h sleep — `maintenance.rs:37` — not a template
  for a time-of-day send).
- **Startup seed (once, before the loop):** if `notify.digest_last_sent_day` is *absent*,
  set it to today's UTC date so a fresh install never fires a digest for a day it wasn't
  monitoring; a *present* marker (a normal restart) is left untouched, preserving same-day
  catch-up (§4.4). **Absence detection (review must-fix):** `settings_store::get` returns
  its `default` for a missing row, so it *cannot* tell absent from present — the seed must
  read the marker with a raw `fetch_optional` (→ `Option<String>`) and branch on `None`.
  (Everywhere else the marker is read via `get(pool, key, "")` with `""` treated as "before
  any date".)
- Then each loop iteration:
  1. read `digest_enabled`; if false, sleep the tick and continue.
  2. compute today's date string (UTC) and today's fire instant = `today 00:00 UTC +
     digest_time`.
  3. read the persisted marker `notify.digest_last_sent_day`.
  4. **if `now >= today_fire_instant` AND `digest_last_sent_day < today`** → build + send
     the digest for **yesterday**. **Advance the marker (`set(..., today)`) only if the
     send actually delivered** (≥1 recipient succeeded) OR there was genuinely nothing to
     deliver (no resolvable active-email recipients — §4.6 still writes an audit row). On a
     total send failure (recipients exist, every send errored) **leave the marker** so the
     next tick retries within the same UTC day — see §4.4/§4.8. This is what keeps the
     dead-man's switch honest: a flaky-SMTP morning is exactly when a missing digest matters.
  5. sleep `digest_tick_seconds` (default **60**).
- Spawned in `main.rs`: `tokio::spawn(digest::run(state.clone()))`.
- Testable seams: `digest::should_send(now, digest_time, last_sent_day) -> Option<today>`
  (pure scheduler decision) and `digest::build(&state, day) -> DigestSummary` (pure-ish
  compute) and `digest::send(&state, &summary) -> Result<()>` (fan-out).

### 4.3 Timezone & period — **UTC** (changed from the brainstorm's "local")
The entire backend computes in UTC: daily rollups are "one row per monitor per completed
**UTC** day" (`rollup.rs:1-2`, `day_str`/`day_bounds` at `rollup.rs:21-36`), and there is
no timezone setting or local-tz helper anywhere. To keep the digest's numbers reconcilable
with the dashboard bars and avoid inventing tz infrastructure for one feature:
- `digest_time` is an **offset into the UTC day** (default `"08:00"` = 08:00 UTC).
- The digest summarizes **yesterday UTC** — the window `day_bounds(day_str(now) - 1 day)`.
  It is computed **live from durable `incidents` + `uptime::compute` + maintenance
  intervals** (§4.5), **not** read from `check_aggregates_daily` (see the review must-fix in
  §4.5 for why the aggregate table is both untimely and maintenance-inconsistent here).
- **Documented boundary:** *both* the send time *and* the content window are UTC. An
  operator at UTC−8 who wants the mail at 08:00 local sets `digest_time` to the UTC
  equivalent (16:00) — and note the figures then cover "yesterday UTC," a window shifted
  from their local calendar day (it ended ~8h before they read it). A future
  `notify.digest_tz_offset` covering both send time and window is a clean additive
  enhancement. *(Flagged for user confirmation at the spec-review gate.)*

### 4.4 Idempotency & restart catch-up
- The `notify.digest_last_sent_day` marker (a `settings` row, durable across restarts) gives
  **at-least-once per UTC day** delivery — not exactly-once. Within a running process it is
  exactly-once (single task, marker advanced right after a successful send). The only
  double-send window is a **crash between `send()` and the marker write**: on restart the
  marker is still behind today, so the digest re-sends once. Digest re-sends are low-harm
  (a duplicate summary email), so this spec accepts at-least-once rather than adding
  per-recipient sent-markers. (Do **not** claim exactly-once.)
- **Restart catch-up:** if the app was down at `digest_time` and starts later the same day
  with the marker still behind today, the first tick sends immediately (matching the app's
  catch-up-on-restart philosophy). If it starts *before* `digest_time`, it waits.
- **First-run guard:** on a brand-new instance the marker is absent → seed it to today's UTC
  date at startup (§4.2) so a fresh install at 09:00 doesn't fire a digest for a day it
  wasn't monitoring. Absence is detected with a raw `fetch_optional`, since
  `settings_store::get` can't distinguish absent from a stored value.

### 4.5 Content — `DigestSummary` (computed live from durable tables)

**Review must-fix (all four lenses): do NOT read `check_aggregates_daily`.** Yesterday's
aggregate row is written by the nightly rollup, a **drifting fixed-24h sleep anchored to
process start** (`maintenance.rs:37-71`) that only rolls up days strictly before today
(`rollup.rs:210`) — so at an 08:00 UTC fire, yesterday's row usually **does not exist yet**
(it's written whenever the app's start-time-of-day next comes around). And even when it
exists, its stored `uptime_pct` is deliberately computed with **maintenance NOT excluded**
(`rollup.rs:137` passes `&[]`), which contradicts "same as live stats." Instead, compute
everything the way the live stats/bars path does (`api/monitors.rs:629-633`, `:883-896`):
from the durable `incidents` table + `uptime::compute` + per-monitor
`maintenance_windows::resolve::maintenance_intervals(&windows, id, &tags, ds, de)`. This
removes the rollup-ordering race and satisfies CLAUDE.md §8's maintenance exclusion.

Period = yesterday UTC, `(ds, de) = day_bounds(yesterday)`. Sources: `incidents`,
`maintenance_windows`, live `monitors`, `ssl_certs`, `domain_info` (+ `checks` only for
`had_any_check`).

```
DigestSummary {
  day: String,                     // "YYYY-MM-DD" (UTC)
  fleet: {
    uptime_pct: Option<f64>,       // downtime-weighted (formula below); None if no denom
    monitors_total: i64,           // live monitor count
    clean_monitors: i64,           // >=1 check yesterday AND zero counted downtime
    incidents: i64,                // = incidents[].len() (overlap query below)
    downtime_seconds: i64,         // sum of clipped, maintenance-excluded downtime
  },
  incidents: Vec<{                 // incidents open at any point during the day
    monitor_name, started_at, resolved_at: Option, duration_seconds: Option,
    cause, status_code: Option, error_message: Option,
  }>,
  currently_down: Vec<{ monitor_name, since }>,   // open incidents at send time
  expirations: Vec<{
    monitor_name, kind: "ssl"|"domain",
    days_remaining: Option,        // None => "unknown" (invalid cert / unqueryable domain)
    flag: "expiring"|"invalid"|"unknown",
  }>,
}
```

**Per-monitor uptime (loop over live monitors, mirroring the stats handler):**
- fetch that monitor's incident spans overlapping `[ds, de)`, clipped to the window;
- `day_maint = maintenance_intervals(&windows, id, &tags, ds, de)` (load `active_windows`
  once before the loop);
- `had_any_check = has_checks_row(id, ds, de) || (is_heartbeat && last_ping_at.is_some())`
  — the **armed-heartbeat special-case** (`api/monitors.rs:614-633`); heartbeats never write
  `checks` rows, so without this every heartbeat monitor reads as "no data";
- `u = uptime::compute(&spans, ds, de, had_any_check, &day_maint)` → `u.downtime_seconds`,
  `u.uptime_pct`.

**Fleet uptime (downtime-weighted — `uptime::compute` does not expose the denominator, so
compute it explicitly):**
- per monitor, `eff_denom = Σ (e−s) over subtract_intervals((ds,de), &day_maint)`
  (= 86400 − maintenance overlap);
- accumulate only over **active monitors** = not `is_paused` **and** `had_any_check` (a
  monitor with no data yesterday is excluded from both numerator and denominator, so it
  can't drag the fleet number);
- `total_down += u.downtime_seconds; total_denom += eff_denom;`
- `fleet.uptime_pct = if total_denom > 0 { Some((1 − total_down/total_denom) * 100) } else { None }`.
This makes the headline number reconcile with the per-monitor rows the same digest prints.

**Incidents (overlap query — the aggregate `incident_count` is "started that day" and
under-counts carry-overs, `rollup.rs:102-109`):**
```sql
SELECT i.started_at, i.resolved_at, i.cause, i.status_code, i.error_message, m.name
FROM incidents i JOIN monitors m ON m.id = i.monitor_id
WHERE i.started_at < ?/*de*/ AND (i.resolved_at IS NULL OR i.resolved_at > ?/*ds*/)
ORDER BY i.started_at
```
`fleet.incidents = this set's length`. `currently_down` = the subset with
`resolved_at IS NULL` at send time.

**Expirations (define the window + surface invalid/unqueryable — CLAUDE.md §6):**
- SSL entry when `days_remaining <= max(monitor.ssl_alert_days)` **OR** `is_valid = 0`
  (invalid/expired → `flag:"invalid"`, `days_remaining` may be `None`/negative);
- Domain entry when `days_remaining <= max(monitor.domain_alert_days)` **OR**
  `queryable = 0` (`flag:"unknown"`, `days_remaining: None`);
- otherwise `flag:"expiring"`.

A quiet day yields `incidents: []`, `currently_down: []`, an all-green fleet line, and
possibly some `expirations` — **still sent** (dead-man's switch).

### 4.6 Delivery & audit
- **Reuse, don't re-implement, deliver()'s email internals (review should-fix).** Bypassing
  `deliver()` is correct (it's monitor-centric — takes `&Monitor`, applies per-monitor
  maintenance + per-`(monitor,channel,trigger)` cooldown, loads channels via
  `monitor_notifications` — none of which a fleet-wide digest wants). But the email plumbing
  (`EmailChannelConfig` parse → `SmtpConfig`/`EmailMsg` build → `transport.send`, incl. the
  `auth_user` username-fallback at `notify/mod.rs:47`) is private in `dispatch.rs`. Extract a
  `pub(crate)` helper — `send_email_via_channel(transport, channel_config_json, subject,
  body_text, body_html) -> Result<()>` — and call it from **both** `deliver()`'s email arm
  and `digest::send`, so the two never diverge.
- For each id in `notify.digest_recipients` that resolves to an **active email channel**,
  render the plaintext digest and send via the shared helper. Per-recipient failures are
  logged and do not abort the others.
- **Audit (CLAUDE.md §12 — every send auditable):** each send writes a `notification_log`
  row with `monitor_id = NULL`, `channel_id`, `incident_id = NULL`, `trigger = 'digest'`,
  `sent_at`, `success`, `error`. (`trigger` is a free TEXT column; `'digest'` needs no
  `Trigger` enum variant — the enum governs per-monitor opt-in, which the digest doesn't
  use.) A dedicated `log_digest_result` insert mirrors `dispatch::log_result`.
- **Silently-dead digest must leave a trace (review must-fix):** if `digest_enabled` but the
  resolvable active-email recipient set is **empty** (empty list, all ids non-email/inactive/
  deleted), write **one** `notification_log` row (`trigger='digest'`, `channel_id=NULL`,
  `success=0`, `error='no deliverable email recipients'`) + a `tracing::warn`, then advance
  the marker (§4.2). Without this, an operator can't distinguish "digest ran, nothing to
  deliver" from "digest never ran" — the exact silent failure the dead-man's switch exists to
  kill. (Optional: validate at `PUT /api/settings` that each `digest_recipients` id is an
  existing active email channel and warn/reject otherwise.)

### 4.7 Settings (digest)
| Key | Default | Meaning |
|---|---|---|
| `notify.digest_enabled` | `false` | Master on/off. |
| `notify.digest_time` | `"08:00"` | Fire time as HH:MM **UTC** offset into the day. |
| `notify.digest_recipients` | `[]` | JSON array of email-channel ids. |
| `notify.digest_tick_seconds` | `60` | Scheduler granularity. |
| `notify.digest_last_sent_day` | *(internal)* | `YYYY-MM-DD` UTC marker; not user-facing. |

### 4.8 Edge cases & v1 boundaries
- **Invalid `digest_time`** (unparseable) → treat as `08:00` and log a warning (never
  crash the loop).
- **No deliverable recipients** (empty list / all non-email / inactive / deleted) → nothing
  to send, but write the audit row (§4.6) and advance the marker so the loop doesn't retry
  all day. An enabled digest with zero recipients is an audited no-op.
- **Total send failure** (recipients exist, every send errored — SMTP down, auth error, or
  the transport was built with no password) → **do NOT advance the marker**; the next tick
  retries within the same UTC day until a send succeeds or the day rolls over. This is the
  dead-man's switch working: one late digest once SMTP recovers, rather than a silently
  dropped day. Failure rows are logged either way.
- **Clock granularity** — fires within one `digest_tick_seconds` (≤60s) of `digest_time`.
- **Missed day while down > 24h** — on restart only the most recent day's digest is sent
  (marker jumps to today); intermediate days are not back-filled (monthly reports, P4.4,
  own historical back-fill). Documented.

---

## 5. Data model

**No schema migration.** Everything new is either:
- `settings` rows (generic key/value table already exists) — all keys in §3.6 + §4.7.
- `notification_log` rows for digest sends (`trigger='digest'`, `monitor_id=NULL`,
  `channel_id=NULL` for the no-recipients audit row) — all columns already nullable.

**Index note:** the re-notify baseline query is now scoped by `incident_id` (§3.3/§3.7), so
the existing `idx_notif_log_monitor_trigger (monitor_id, trigger, sent_at DESC)` does not
cover it. Re-notify volume is tiny (a handful of open incidents scanned every ~5 min), so a
full scan of the small `notification_log` is fine and **no new index is added** (adding one
would be a schema migration for negligible benefit — YAGNI). Revisit only if
`notification_log` grows large.

This keeps P4.3 migration-free (next migration file remains unused; `0006` not created).

---

## 6. API surface (settings extension)

Extend the existing `/api/settings` GET/PUT (`api/settings.rs`) — the fixed-typed-DTO
pattern:
1. Add to `UpdateSettingsDto`: `renotify_hours: Option<i64>`, `digest_enabled: Option<bool>`,
   `digest_time: Option<String>`, `digest_recipients: Option<Value>` (JSON array). Each
   gets an `if let Some(_)` block calling `settings_store::set` (recipients: store as the
   JSON string via `serde_json::to_string`; `digest_enabled`: store `"1"`/`"0"`).
2. Add the four keys to the `current_settings` `json!` block (GET response). **Round-trip
   correctness (review should-fix):** `digest_recipients` is stored as a JSON *string*, so
   GET must emit the **parsed array** (via the `settings_store::digest_recipients ->
   Vec<i64>` helper below), not the raw string — otherwise the frontend multi-select
   receives `"[1,2]"` and can't bind it. (Mirror how `anchors` is emitted as a parsed
   `Vec`.) Emit `digest_enabled` as a JSON bool.
3. Add `settings_store` helpers + `DEFAULT_*` consts for each. **Two new helper shapes**
   beyond the existing i64/String helpers:
   - `digest_enabled(pool) -> bool` — reads `"1"`/`"0"` (any non-`"1"` = false); the first
     boolean helper, so define the `"1"`/`"0"` convention here.
   - `digest_recipients(pool) -> Vec<i64>` — `serde_json::from_str(&get(...,"[]")).unwrap_or_default()`.
   Plus i64/String helpers for `renotify_hours`, `renotify_tick_seconds`, `digest_time`,
   `digest_tick_seconds`.
- `digest_last_sent_day` and the two `*_tick_seconds` keys are **not** exposed on the DTO
  (internal / advanced) — read via `settings_store` only.
- No new routes.

---

## 7. Frontend

`web/src/components/Settings.tsx` + `web/src/api.ts` `Settings` interface, following the
existing "signal + Save button + saving/saved pair" pattern (as `anchors`/`retention`):
- **Re-notify** field: `renotify_hours` number input (`0` = off, with helper text).
- **Digest** section: `digest_enabled` toggle, `digest_time` HH:MM input (labeled **UTC**),
  and a `digest_recipients` multi-select of the existing email channels.
- **Incidents screen:** the existing "Acknowledge" button gains a hint —
  "Acknowledge (silences reminders)" — since ack now suppresses re-notify.
- Extend `Settings` TS interface with the new fields; `updateSettings(patch)` already
  sends partial patches.
- Tests in `web/src/__tests__/settings.test.tsx`.

---

## 8. Testing strategy

**Re-notify (`renotify_once`, via `RecordingTransport`):**
- fires a reminder when an incident is open, unacked, `status='down'`, and
  `now - baseline >= interval`;
- does **not** fire when: acknowledged / resolved / paused / within interval / disabled
  (`renotify_hours=0`) / under an active maintenance window / connectivity offline /
  **monitor `status='unknown'`** (post-reconnect window);
- baseline advances — a second immediate pass does **not** double-fire;
- **incident-scoped baseline:** two sequential incidents on one monitor — the second's
  first reminder is timed from *its own* start, not the first incident's last send
  (regression for the cross-incident-contamination must-fix);
- baseline falls back to `started_at` when this incident has no prior `down` log row;
- **cascade-deleted monitor:** an incident whose monitor was deleted mid-pass is skipped —
  no reminder, no panic;
- heartbeat monitors re-notify with `heartbeat_missed`, http with `down`; **the reminder
  subject is `Reminder: …` and body has the elapsed line for BOTH triggers**;
- **first-alert regression:** the initial (non-reminder) down alert subject/body is
  byte-identical to today (guards the decorate-don't-modify-templates choice).

**Digest:**
- `should_send` decision table: before time / after time not-yet-sent / already-sent-today
  / disabled; **first-run** absent-marker seed makes a fresh instance NOT fire for today;
- `build` computes fleet uptime/clean/incidents from **`incidents` + `uptime::compute`**
  (not aggregates) over seeded data; a **quiet day still yields a sendable all-green
  summary**;
- **maintenance exclusion:** a yesterday outage fully inside a maintenance window → fleet
  100% / monitor clean (guards §4.5 against the aggregate path);
- **armed-heartbeat** monitor with a ping but no `checks` rows is treated as having data
  (not null uptime);
- **expirations:** an invalid cert (`is_valid=0`) and an unqueryable domain
  (`queryable=0`) both appear with `flag:"invalid"/"unknown"`, not silently dropped;
- recipient fan-out sends one email per active email channel and logs `trigger='digest'`;
- **no deliverable recipients** (empty / non-email / inactive id) → an audit row
  (`success=0`, `error='no deliverable email recipients'`) is written and the marker
  advances exactly once;
- **total send failure** → marker does **not** advance; the next tick retries;
- idempotency: marker prevents a second send the same UTC day; restart catch-up sends once
  when started after `digest_time` with a stale marker;
- invalid `digest_time` falls back to 08:00 without panicking.

**Cross-cutting:** full suite green at `--test-threads=1`; rustls-only (no new
aws-lc/openssl deps — this phase adds no crates); tsc + vite build + vitest clean.

---

## 9. Task decomposition preview (for the plan)

Roughly (writing-plans will finalize):
1. Settings store helpers + `DEFAULT_*` consts (renotify + digest keys, incl. the
   `digest_enabled -> bool` and `digest_recipients -> Vec<i64>` helpers) + tests.
2. Settings API DTO/GET/PUT extension (parsed-array GET for recipients) + tests.
3. Extract `pub(crate) send_email_via_channel(...)` from `dispatch.rs`; route `deliver()`'s
   email arm through it (behavior-preserving refactor) + tests.
4. `renotify.rs` — `renotify_once` (incident-scoped baseline, `status='down'` filter,
   deleted-monitor skip, TOCTOU re-check, post-render `Reminder:` decoration) + tests; wire
   tick loop + spawn. (No `TemplateCtx`/`templates::render` change.)
5. `digest.rs` — `should_send` + `build` (from `incidents`/`uptime::compute`/maintenance)
   + `send` (shared email helper) + audit rows + marker policy + tests; wire scheduler +
   spawn.
6. Frontend Settings controls (re-notify + digest) + Acknowledge "(silences reminders)"
   hint + tests.
7. Live acceptance + finish/merge.

## 10. Documented boundaries (recap)
- Effective re-notify interval = `max(renotify_hours, cooldown_minutes/60)` (both in hours).
- Re-notify baseline is **incident-scoped**; reminders reuse the `down`/`heartbeat_missed`
  trigger (2nd+ rows per `incident_id` = reminders — audit contract, §3.3).
- Failed *reminder* sends restart the clock (mirrors cooldown).
- Acknowledge is terminal-until-resolve (no un-ack in v1).
- Digest schedule **and** content window are **UTC**; a `digest_tz_offset` is a future add.
- Digest computes from `incidents`/`uptime::compute` (not `check_aggregates_daily`), so it
  is timely and maintenance-excluding.
- Digest delivery is **at-least-once** per UTC day (crash between send and marker → one
  re-send); marker advances only on a successful/nothing-to-send outcome, not on failure.
- Restart back-fills only the most recent day's digest.
- Digest is plaintext (rich HTML/PDF is P4.4).

---

*End of P4.3 spec. §3–§4 define behavior, §5–§7 data/API/UI, §8 testing — build-ready for
the implementation plan.*
