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
   ```
4. For each row, load the `Monitor` (needed by `deliver`), then compute the reminder
   baseline:
   ```sql
   SELECT MAX(sent_at) FROM notification_log
   WHERE monitor_id = ? AND trigger IN ('down','heartbeat_missed')
   ```
   `baseline = last_reminder ?? incident.started_at` (fall back to the incident's start
   if the initial alert never produced a log row — e.g. no channel was attached yet).
5. **Fire if due:** `if now - baseline >= renotify_hours * 3600` → build the reminder
   `NotifyMsg` (§3.6) and call `deliver(&state, &m, down_trigger, &msg, Some(incident_id))`,
   where `down_trigger = if m.r#type == "heartbeat" { HeartbeatMissed } else { Down }`
   (same selection as `engine.rs:155-156`).
6. `deliver()` writes fresh `notification_log` row(s) at `now`, advancing the baseline so
   the next reminder is one interval out. No extra bookkeeping table.

### 3.4 Stop conditions — all fall out of the query + existing guards

| Condition | How it stops re-notify |
|---|---|
| Incident resolved | `resolved_at IS NULL` excludes it; `emit_resolved` already closed it. |
| **Acknowledged** | `acknowledged = 0` excludes it. **This is the new teeth on the ack flag.** |
| Monitor paused | `m.is_paused = 0` excludes it. |
| Under maintenance | `deliver()`'s maintenance guard short-circuits (no send, no log row → baseline doesn't advance → retried after window ends). |
| Your connection down | pass-level connectivity gate (§3.3 step 2). |
| Re-notify disabled | `renotify_hours == 0` short-circuit. |

### 3.5 The reminder message (`NotifyMsg` / template)
- The reminder should read as a reminder, not a fresh outage: subject prefixed
  `Reminder:` and body noting elapsed time (`now - incident.started_at`).
- Mechanism: extend `TemplateCtx` (in `notify/mod.rs`) with an optional reminder marker
  — e.g. `reminder_elapsed: Option<i64>` (seconds). `templates::render()` for the `down`
  trigger, when the marker is `Some`, renders "Still DOWN — {{duration}}" and the
  `Reminder:` subject prefix; when `None`, renders the existing first-alert copy
  unchanged (so the initial down alert is byte-identical to today).
- `renotify_once` constructs the reminder `NotifyMsg` from the incident (elapsed from
  `started_at`), reusing the same `NotifyMsg`/`TemplateCtx` fields `on_transition` uses.

### 3.6 Settings (re-notify)
| Key | Default | Meaning |
|---|---|---|
| `notify.renotify_hours` | `6` | Reminder cadence in hours; `0` = disabled. |
| `notify.renotify_tick_seconds` | `300` | Loop granularity (how often the scan runs). |

Both via `settings_store` helpers + `DEFAULT_*` consts, mirroring `cooldown_minutes` /
`maintenance_tick_seconds` (`settings_store.rs`).

### 3.7 Edge cases & documented v1 boundaries
- **Effective interval = `max(renotify_hours, cooldown_minutes/60)`** — `deliver()`'s
  per-channel cooldown is a backstop. With defaults (6h vs 15m) it never interferes; if
  an operator sets `renotify_hours` below `cooldown_minutes`, the cooldown wins.
  Documented, not guarded.
- **Failed sends restart the clock** — baseline keys off `MAX(sent_at)` regardless of
  `success` (mirrors cooldown). A failed *initial* alert won't retry for a full interval.
  Documented v1 boundary.
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
- **Startup seed (once, before the loop):** if `notify.digest_last_sent_day` is *absent*
  (brand-new instance), set it to today's UTC date so a fresh install never fires a digest
  for a day it wasn't monitoring. A *present* marker (a normal restart) is left untouched,
  which is what preserves same-day catch-up (§4.4).
- Then each loop iteration:
  1. read `digest_enabled`; if false, sleep the tick and continue.
  2. compute today's date string (UTC) and today's fire instant = `today 00:00 UTC +
     digest_time`.
  3. read the persisted marker `notify.digest_last_sent_day` (always present after the seed).
  4. **if `now >= today_fire_instant` AND `digest_last_sent_day < today`** → build +
     send the digest for **yesterday**, then `settings_store::set("notify.digest_last_sent_day", today)`.
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
- The digest summarizes **yesterday UTC** (`day_str(now) - 1 day`), read straight from the
  completed `check_aggregates_daily` rows for that day.
- **Documented boundary:** an operator who wants the mail in their local morning sets
  `digest_time` to the UTC equivalent. A future `notify.digest_tz_offset` is a clean
  additive enhancement. *(Flagged for user confirmation at the spec-review gate.)*

### 4.4 Idempotency & restart catch-up
- The `notify.digest_last_sent_day` marker (a `settings` row, so durable across restarts)
  makes the send exactly-once per UTC day: the send only proceeds when the marker is
  behind today, and advances it on success.
- **Restart catch-up:** if the app was down at `digest_time` and starts later the same day
  with the marker still behind today, the first tick sends immediately (matching the app's
  catch-up-on-restart philosophy). If it starts *before* `digest_time`, it waits.
- **First-run guard:** on a brand-new instance the marker is absent → treat as "sent" for
  today's date at startup (seed it to today) so a fresh install at 09:00 doesn't fire a
  digest for a day it wasn't monitoring. *(Alternatively seed to yesterday; the seed-to-today
  choice is the conservative one and is what this spec specifies.)*

### 4.5 Content — `DigestSummary` (computed from durable tables)
Period = yesterday UTC. Sources: `check_aggregates_daily`, `incidents`, `ssl_certs`,
`domain_info`, live monitor status.

```
DigestSummary {
  day: String,                     // "YYYY-MM-DD" (UTC)
  fleet: {
    uptime_pct: Option<f64>,       // downtime-weighted across active monitors
    monitors_total: i64,
    clean_monitors: i64,           // 100% up that day
    incidents: i64,                // incidents active during the day
    downtime_seconds: i64,
  },
  incidents: Vec<{                 // incidents open at any point during the day
    monitor_name, started_at, resolved_at: Option, duration_seconds: Option,
    cause, status_code: Option, error_message: Option,
  }>,
  currently_down: Vec<{ monitor_name, since }>,   // open incidents at send time
  expirations: Vec<{               // certs/domains inside their warning window
    monitor_name, kind: "ssl"|"domain", days_remaining,
  }>,
}
```
Uptime uses `uptime::compute(...)` with maintenance intervals excluded (same as live
stats). A quiet day yields `incidents: []`, `currently_down: []`, and an all-green fleet
line — still sent.

### 4.6 Delivery & audit
- For each channel id in `notify.digest_recipients` that resolves to an **active email
  channel**, render the plaintext digest and send via `state.transport.send(...)`.
- Per-recipient failures are logged and do not abort the others.
- **Audit:** each send writes a `notification_log` row with `monitor_id = NULL`,
  `channel_id`, `incident_id = NULL`, `trigger = 'digest'`, `sent_at`, `success`, `error`.
  (`trigger` is a free TEXT column; `'digest'` needs no `Trigger` enum variant — the enum
  governs per-monitor opt-in, which the digest doesn't use.) A dedicated
  `log_digest_result` insert mirrors `dispatch::log_result`.

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
- **No recipients / no active email channel** → nothing to send; the marker still advances
  so the loop doesn't retry all day. (An enabled digest with zero recipients is a no-op.)
- **Clock granularity** — fires within one `digest_tick_seconds` (≤60s) of `digest_time`.
- **Missed day while down > 24h** — on restart only the most recent day's digest is sent
  (marker jumps to today); intermediate days are not back-filled (monthly reports, P4.4,
  own historical back-fill). Documented.

---

## 5. Data model

**No schema migration.** Everything new is either:
- `settings` rows (generic key/value table already exists) — all keys in §3.6 + §4.7.
- `notification_log` rows for digest sends (`trigger='digest'`, `monitor_id=NULL`) — all
  columns already nullable; the existing index `(monitor_id, trigger, sent_at DESC)`
  covers the re-notify baseline query.

This keeps P4.3 migration-free (next migration file remains unused; `0006` not created).

---

## 6. API surface (settings extension)

Extend the existing `/api/settings` GET/PUT (`api/settings.rs`) — the fixed-typed-DTO
pattern:
1. Add to `UpdateSettingsDto`: `renotify_hours: Option<i64>`, `digest_enabled: Option<bool>`,
   `digest_time: Option<String>`, `digest_recipients: Option<Value>` (JSON array). Each
   gets an `if let Some(_)` block calling `settings_store::set` (recipients: store as the
   JSON string; `digest_enabled`: store `"1"`/`"0"`).
2. Add the four keys to the `current_settings` `json!` block (GET response).
3. Add `settings_store` helpers + `DEFAULT_*` consts for each (renotify_hours,
   renotify_tick_seconds, digest_enabled, digest_time, digest_recipients,
   digest_tick_seconds).
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
- fires a reminder when an incident is open, unacked, and `now - baseline >= interval`;
- does **not** fire when: acknowledged / resolved / paused / within interval / disabled
  (`renotify_hours=0`) / under an active maintenance window / connectivity offline;
- baseline advances — a second immediate pass does **not** double-fire;
- baseline falls back to `started_at` when no prior `down` log row exists;
- heartbeat monitors re-notify with `heartbeat_missed`, http with `down`.

**Digest:**
- `should_send` decision table: before time / after time not-yet-sent / already-sent-today
  / disabled;
- `build` over seeded aggregates+incidents+certs produces correct fleet/incidents/expiry
  fields; a **quiet day still yields a sendable all-green summary**;
- recipient fan-out sends one email per active email channel and logs `trigger='digest'`;
- idempotency: marker prevents a second send the same UTC day; restart catch-up sends once
  when started after `digest_time` with a stale marker;
- invalid `digest_time` falls back to 08:00 without panicking.

**Cross-cutting:** full suite green at `--test-threads=1`; rustls-only (no new
aws-lc/openssl deps — this phase adds no crates); tsc + vite build + vitest clean.

---

## 9. Task decomposition preview (for the plan)

Roughly (writing-plans will finalize):
1. Settings store helpers + `DEFAULT_*` consts (renotify + digest keys).
2. Settings API DTO/GET/PUT extension + tests.
3. `TemplateCtx` reminder marker + `down` template reminder rendering + tests.
4. `renotify.rs` — `renotify_once` scan/decision/fire + tests; wire tick loop + spawn.
5. `digest.rs` — `should_send` + `build` + `send`/audit + tests; wire scheduler + spawn.
6. Frontend Settings controls (re-notify + digest) + Acknowledge hint + tests.
7. Live acceptance + finish/merge.

## 10. Documented boundaries (recap)
- Effective re-notify interval = `max(renotify_hours, cooldown_minutes)`.
- Failed sends restart the re-notify clock (mirrors cooldown).
- Digest schedule + window are **UTC**; local-tz offset is a future enhancement.
- Restart back-fills only the most recent day's digest.
- Digest is plaintext (rich HTML/PDF is P4.4).

---

*End of P4.3 spec. §3–§4 define behavior, §5–§7 data/API/UI, §8 testing — build-ready for
the implementation plan.*
