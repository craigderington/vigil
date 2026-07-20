# P4.3 Notification Throttling & Digest — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add re-notify (repeat down alerts while an outage is ongoing, silenced by acknowledge) and an optional daily digest email, on top of Vigil's existing notification subsystem.

**Architecture:** Two new background tasks (`renotify::run`, `digest::run`) spawned in `main.rs`, following the existing tick-loop pattern. Re-notify reuses the `deliver()` funnel (so maintenance-mute + per-channel cooldown + `notification_log` apply for free); the `notification_log` row is its clock. The digest bypasses `deliver()` (it's fleet-wide), computes yesterday's uptime **live from `incidents` + `uptime::compute` + maintenance intervals** (not the aggregate table), and sends via a shared email helper extracted from `dispatch.rs`. Everything new is `settings` rows — **no schema migration**.

**Tech Stack:** Rust (tokio, sqlx-sqlite, async-trait), SolidJS/TS frontend. No new crates.

## Global Constraints

- **rustls-only.** No `aws-lc-rs`/`openssl` anywhere, incl. dev-deps. This phase adds **no new crates**.
- **No schema migration.** All new persistence is `settings` key/value rows + `notification_log` rows (existing nullable columns). Do not create `migrations/0006_*`.
- **UTC everywhere.** The digest schedule (`digest_time`) and its "yesterday" window are UTC, via `rollup::day_str`/`day_bounds`. No local-timezone code.
- **Secrets discipline.** SMTP password never in DB/API/logs. Commit with `git commit -am` (never `git add -A`) so untracked secret files aren't staged.
- **Test isolation.** Run the full Rust suite with `--test-threads=1` (a pre-existing sqlx-sqlite flake appears only under cross-binary parallel runs).
- **Ports.** Never bind standard ports; app is `0.0.0.0:8090` internal / `8099` host.
- **Branch.** Work on `feat/p4-notification-throttling-digest`; finish = local fast-forward merge to `master`, branch deleted, not pushed to origin.
- **Re-notify reuses the `down`/`heartbeat_missed` subscription** — no new `Trigger` variant. Reminders are the 2nd+ `notification_log` rows sharing an `incident_id` (audit contract).

---

## File Structure

| File | Change | Responsibility |
|---|---|---|
| `crates/vigil/src/settings_store.rs` | modify | Add `renotify_hours`, `renotify_tick_seconds`, `digest_enabled`, `digest_time`, `digest_tick_seconds`, `digest_recipients` helpers + `DEFAULT_*` consts. |
| `crates/vigil/src/api/settings.rs` | modify | Surface the new keys on GET/PUT (recipients as a parsed array). |
| `crates/vigil/src/notify/dispatch.rs` | modify | Extract `pub send_email_via_channel(...)` (MUST be `pub`, not `pub(crate)` — the Task 3 integration test imports it across the crate boundary); route `send_to_channel`'s email arm through it. |
| `crates/vigil/src/renotify.rs` | create | `renotify_once` (scan/decide/fire) + reminder decoration + `run` loop. |
| `crates/vigil/src/digest.rs` | create | `DigestSummary` + `build` (compute) + `send`/audit + `should_send`/`parse_digest_time` + `run` scheduler. |
| `crates/vigil/src/lib.rs` | modify | `pub mod renotify;` + `pub mod digest;`. |
| `crates/vigil/src/main.rs` | modify | Spawn `renotify::run` + `digest::run`. |
| `web/src/api.ts` | modify | Extend `Settings` interface with the new fields. |
| `web/src/components/Settings.tsx` | modify | Re-notify + digest controls. |
| `web/src/components/Incidents.tsx` + `web/src/components/IncidentTimeline.tsx` | modify | Acknowledge "(silences reminders)" hint on BOTH Acknowledge buttons. |
| `crates/vigil/tests/settings_p43.rs` | create | settings_store helper tests. |
| `crates/vigil/tests/renotify.rs` | create | re-notify integration tests. |
| `crates/vigil/tests/digest.rs` | create | digest compute + send + scheduler tests. |
| `crates/vigil/tests/common/mod.rs` | modify | Add a `FailingTransport` for the all-failed digest test (Task 6). The offline case reuses the EXISTING `test_state_offline()`. |
| `web/src/__tests__/settings.test.tsx` | modify | Frontend settings PUT assertions. |

**Verified interfaces from the current tree (use these EXACTLY):**
- `settings_store::get(pool: &SqlitePool, key: &str, default: &str) -> String`; `set(pool, key, value) -> anyhow::Result<()>`.
- `dispatch::deliver(state: &AppState, m: &Monitor, trigger: Trigger, msg: &NotifyMsg, incident_id: Option<i64>) -> anyhow::Result<()>`.
- `templates::render(trigger: Trigger, ctx: &TemplateCtx) -> (String, String, Option<String>)` (subject, body_text, body_html; body_html always `None`).
- `TemplateCtx { monitor_name, url, status, status_code: Option<i64>, error: Option<String>, response_time_ms: Option<i64>, duration: Option<i64>, checked_at: Ts }`.
- `NotifyMsg { monitor_name, url, status, status_code: Option<i64>, error: Option<String>, response_time_ms: Option<i64>, duration: Option<i64>, ssl_days: Option<i64>, domain_days: Option<i64>, checked_at: Ts, incident_url: Option<String>, subject, body, body_html: Option<String> }`.
- `SmtpConfig { host: String, port: u16, security: String, username: Option<String> }`; `EmailMsg { to: Vec<String>, from: String, subject, body_text, body_html: Option<String> }`; `Transport::send(&self, &SmtpConfig, &EmailMsg) -> anyhow::Result<()>`.
- `EmailChannelConfig { host: String, port: u16, security: String, from: String, to: Vec<String>, username: Option<String> }` (private in dispatch.rs).
- `AppState { db: SqlitePool, bus, transport: Arc<dyn Transport>, http_sender: Arc<dyn HttpSender>, sched_tx, anchor: Arc<AnchorGate> }`.
- `state.anchor.current().await -> Connectivity` (`Connectivity::{Online, Offline}`; fail-open to `Online`).
- `uptime::compute(spans: &[Span], window_start: Ts, now: Ts, had_any_check: bool, maintenance: &[(Ts, Ts)]) -> Uptime { uptime_pct: Option<f64>, downtime_seconds: i64 }`; `Span { start: Ts, end: Option<Ts> }`.
- `maintenance_windows::active_windows(pool) -> Vec<MaintenanceWindow>`; `maintenance_windows::resolve::maintenance_intervals(&windows, id: i64, tags: &[String], from: Ts, to: Ts) -> Vec<(Ts,Ts)>`; `resolve::subtract_intervals(base: (Ts,Ts), cuts: &[(Ts,Ts)]) -> Vec<(Ts,Ts)>`; `resolve::parse_tags(raw: &str) -> Vec<String>`.
- `rollup::day_str(epoch: Ts) -> String`; `rollup::day_bounds(day: &str) -> (i64, i64)`.
- `Monitor` fields used: `id, name, r#type, url: Option<String>, tags: Option<String>, status: Status, is_paused: bool, last_ping_at: Option<Ts>, ssl_check_enabled: bool, ssl_alert_days: String, domain_check_enabled: bool, domain_alert_days: String`. `Status` renders `'down'` etc. as strings in SQL (`m.status = 'down'`).
- `SslCert { is_valid: Option<bool>, days_remaining: Option<i64>, .. }`; `DomainInfo { queryable: Option<bool>, days_remaining: Option<i64>, .. }` (both manual `FromRow`, `SELECT * FROM ssl_certs/domain_info WHERE monitor_id = ?`).
- Test harness `crates/vigil/tests/common/mod.rs`: `test_state() -> TestEnv { state, sent: Arc<Mutex<Vec<(SmtpConfig, EmailMsg)>>>, sent_http, .. }`; `fresh_pool() -> (SqlitePool, TempDir)`; `seed_monitor_with_email_channel(&db) -> i64` (attaches an email channel with default `["down","recovered"]` triggers). Tests assert via `env.sent.lock().unwrap()`.

---

### Task 1: Settings store helpers

**Files:**
- Modify: `crates/vigil/src/settings_store.rs`
- Test: `crates/vigil/tests/settings_p43.rs` (create)

**Interfaces:**
- Produces: `settings_store::renotify_hours(pool) -> i64`, `renotify_tick_seconds(pool) -> i64`, `digest_enabled(pool) -> bool`, `digest_time(pool) -> String`, `digest_tick_seconds(pool) -> i64`, `digest_recipients(pool) -> Vec<i64>`.

- [ ] **Step 1: Write the failing test** — `crates/vigil/tests/settings_p43.rs`

```rust
mod common;
use common::fresh_pool;
use vigil::settings_store as s;

#[tokio::test]
async fn renotify_and_digest_defaults_then_roundtrip() {
    let (pool, _dir) = fresh_pool().await;

    // defaults
    assert_eq!(s::renotify_hours(&pool).await, 6);
    assert_eq!(s::renotify_tick_seconds(&pool).await, 300);
    assert!(!s::digest_enabled(&pool).await);
    assert_eq!(s::digest_time(&pool).await, "08:00");
    assert_eq!(s::digest_tick_seconds(&pool).await, 60);
    assert_eq!(s::digest_recipients(&pool).await, Vec::<i64>::new());

    // round-trip
    s::set(&pool, "notify.renotify_hours", "12").await.unwrap();
    s::set(&pool, "notify.digest_enabled", "1").await.unwrap();
    s::set(&pool, "notify.digest_time", "07:30").await.unwrap();
    s::set(&pool, "notify.digest_recipients", "[3,5]").await.unwrap();

    assert_eq!(s::renotify_hours(&pool).await, 12);
    assert!(s::digest_enabled(&pool).await);
    assert_eq!(s::digest_time(&pool).await, "07:30");
    assert_eq!(s::digest_recipients(&pool).await, vec![3, 5]);
}

#[tokio::test]
async fn digest_enabled_is_false_for_any_non_one() {
    let (pool, _dir) = fresh_pool().await;
    s::set(&pool, "notify.digest_enabled", "0").await.unwrap();
    assert!(!s::digest_enabled(&pool).await);
    s::set(&pool, "notify.digest_enabled", "true").await.unwrap();
    assert!(!s::digest_enabled(&pool).await, "only \"1\" is true");
}
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test --test settings_p43 -- --test-threads=1`
Expected: FAIL to compile (`renotify_hours` etc. not found).

- [ ] **Step 3: Add the consts and helpers** — append to `crates/vigil/src/settings_store.rs`

Add consts alongside the existing `DEFAULT_*` block (near line 15):
```rust
const DEFAULT_RENOTIFY_HOURS: i64 = 6;
const DEFAULT_RENOTIFY_TICK_SECONDS: i64 = 300;
const DEFAULT_DIGEST_TIME: &str = "08:00";
const DEFAULT_DIGEST_TICK_SECONDS: i64 = 60;
```

Add the helpers (mirror the existing `cooldown_minutes` shape):
```rust
/// `notify.renotify_hours` — reminder cadence for an ongoing outage. 0 disables.
pub async fn renotify_hours(pool: &SqlitePool) -> i64 {
    get(pool, "notify.renotify_hours", &DEFAULT_RENOTIFY_HOURS.to_string())
        .await
        .parse()
        .unwrap_or(DEFAULT_RENOTIFY_HOURS)
}

/// `notify.renotify_tick_seconds` — how often the re-notify scan runs.
pub async fn renotify_tick_seconds(pool: &SqlitePool) -> i64 {
    get(pool, "notify.renotify_tick_seconds", &DEFAULT_RENOTIFY_TICK_SECONDS.to_string())
        .await
        .parse()
        .unwrap_or(DEFAULT_RENOTIFY_TICK_SECONDS)
}

/// `notify.digest_enabled` — daily digest master switch. Stored "1"/"0";
/// any value other than "1" is false. First boolean settings helper.
pub async fn digest_enabled(pool: &SqlitePool) -> bool {
    get(pool, "notify.digest_enabled", "0").await == "1"
}

/// `notify.digest_time` — "HH:MM" UTC offset into the day the digest fires.
pub async fn digest_time(pool: &SqlitePool) -> String {
    get(pool, "notify.digest_time", DEFAULT_DIGEST_TIME).await
}

/// `notify.digest_tick_seconds` — digest scheduler granularity.
pub async fn digest_tick_seconds(pool: &SqlitePool) -> i64 {
    get(pool, "notify.digest_tick_seconds", &DEFAULT_DIGEST_TICK_SECONDS.to_string())
        .await
        .parse()
        .unwrap_or(DEFAULT_DIGEST_TICK_SECONDS)
}

/// `notify.digest_recipients` — email channel ids, stored as a JSON array string.
pub async fn digest_recipients(pool: &SqlitePool) -> Vec<i64> {
    serde_json::from_str(&get(pool, "notify.digest_recipients", "[]").await).unwrap_or_default()
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --test settings_p43 -- --test-threads=1`
Expected: PASS (2 tests).

- [ ] **Step 5: Commit**

```bash
git commit -am "feat(p4.3): settings_store helpers for renotify + digest keys"
```

---

### Task 2: Settings API surface

**Files:**
- Modify: `crates/vigil/src/api/settings.rs`
- Test: `crates/vigil/tests/settings_p43.rs` (extend)

**Interfaces:**
- Consumes: the Task 1 `settings_store` helpers.
- Produces: `/api/settings` GET returns `renotify_hours`, `digest_enabled` (bool), `digest_time` (string), `digest_recipients` (JSON **array**); PUT accepts the same via `UpdateSettingsDto`.

- [ ] **Step 1: Write the failing test** — append to `crates/vigil/tests/settings_p43.rs`

```rust
use axum::extract::State;
use axum::Json;
use serde_json::json;
use vigil::api::settings::{get_settings, update_settings, UpdateSettingsDto};

#[tokio::test]
async fn settings_put_then_get_roundtrips_digest_recipients_as_array() {
    let env = common::test_state().await;
    let state = env.state.clone();

    let dto = UpdateSettingsDto {
        anchors: None,
        cooldown_minutes: None,
        retention_days: None,
        accent: None,
        renotify_hours: Some(9),
        digest_enabled: Some(true),
        digest_time: Some("07:15".into()),
        digest_recipients: Some(json!([2, 4])),
    };
    update_settings(State(state.clone()), Json(dto)).await.unwrap();

    let got = get_settings(State(state)).await.unwrap().0;
    assert_eq!(got["renotify_hours"], 9);
    assert_eq!(got["digest_enabled"], true);
    assert_eq!(got["digest_time"], "07:15");
    assert_eq!(got["digest_recipients"], json!([2, 4]), "recipients GET must be a JSON array, not a string");
}
```

Note: the test uses only `common::test_state().await` + `env.state` (both public) — no extra helper, no `AppState`/`TempDir` import needed.

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test --test settings_p43 settings_put_then_get -- --test-threads=1`
Expected: FAIL to compile (`UpdateSettingsDto` has no `renotify_hours` field; `api::settings` items not `pub`).

- [ ] **Step 3: Extend the API** — `crates/vigil/src/api/settings.rs`

Make the module items reachable from tests: ensure `pub mod settings;` in `api/mod.rs` (it already is) and that `UpdateSettingsDto`, `get_settings`, `update_settings` are `pub` (they are). Add fields to `UpdateSettingsDto`:
```rust
    pub renotify_hours: Option<i64>,
    pub digest_enabled: Option<bool>,
    pub digest_time: Option<String>,
    pub digest_recipients: Option<Value>,
```

Add to `current_settings`'s `json!` block:
```rust
        "renotify_hours": settings_store::renotify_hours(&state.db).await,
        "digest_enabled": settings_store::digest_enabled(&state.db).await,
        "digest_time": settings_store::digest_time(&state.db).await,
        "digest_recipients": settings_store::digest_recipients(&state.db).await,
```

Add to `update_settings` (before the final `Ok(Json(...))`):
```rust
    if let Some(h) = dto.renotify_hours {
        settings_store::set(&state.db, "notify.renotify_hours", &h.to_string())
            .await
            .map_err(set_err)?;
    }
    if let Some(e) = dto.digest_enabled {
        settings_store::set(&state.db, "notify.digest_enabled", if e { "1" } else { "0" })
            .await
            .map_err(set_err)?;
    }
    if let Some(t) = dto.digest_time {
        settings_store::set(&state.db, "notify.digest_time", &t)
            .await
            .map_err(set_err)?;
    }
    if let Some(r) = dto.digest_recipients {
        let ids: Vec<i64> = match &r {
            Value::Array(a) => a.iter().filter_map(|v| v.as_i64()).collect(),
            _ => Vec::new(),
        };
        let encoded = serde_json::to_string(&ids).unwrap_or_else(|_| "[]".to_string());
        settings_store::set(&state.db, "notify.digest_recipients", &encoded)
            .await
            .map_err(set_err)?;
    }
```

`TestEnv.state` is already `pub`, so the test reaches it directly.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --test settings_p43 -- --test-threads=1`
Expected: PASS (3 tests).

- [ ] **Step 5: Commit**

```bash
git commit -am "feat(p4.3): expose renotify + digest settings on /api/settings"
```

---

### Task 3: Extract the shared email-send helper (behavior-preserving)

**Files:**
- Modify: `crates/vigil/src/notify/dispatch.rs`
- Test: `crates/vigil/tests/notify_email_helper.rs` (create) + existing `tests/notify.rs`/`tests/notify_multi.rs` must still pass.

**Interfaces:**
- Produces: `pub async fn send_email_via_channel(transport: &dyn crate::notify::Transport, config_json: &str, subject: &str, body_text: &str, body_html: Option<String>) -> anyhow::Result<()>` — used by both `deliver()`'s email arm and (Task 6) `digest::send`. **`pub` (not `pub(crate)`):** integration tests under `crates/vigil/tests/` are separate crates and can only see `pub` items; the Task 3 test imports this directly, so `pub(crate)` would fail with E0603 (matches how `deliver` is already `pub`).

- [ ] **Step 1: Write the failing test** — `crates/vigil/tests/notify_email_helper.rs`

```rust
mod common;
use common::test_state;
use vigil::notify::dispatch::send_email_via_channel;

#[tokio::test]
async fn helper_parses_config_and_sends_email() {
    let env = test_state().await;
    let cfg = r#"{"host":"smtp.example.com","port":587,"security":"starttls","from":"a@b.com","to":["x@y.com","z@y.com"]}"#;

    send_email_via_channel(env.state.transport.as_ref(), cfg, "Subj", "Body", None)
        .await
        .unwrap();

    let sent = env.sent.lock().unwrap();
    assert_eq!(sent.len(), 1);
    let (smtp, msg) = &sent[0];
    assert_eq!(smtp.host, "smtp.example.com");
    assert_eq!(smtp.port, 587);
    assert_eq!(msg.from, "a@b.com");
    assert_eq!(msg.to, vec!["x@y.com".to_string(), "z@y.com".to_string()]);
    assert_eq!(msg.subject, "Subj");
    assert_eq!(msg.body_text, "Body");
}
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test --test notify_email_helper -- --test-threads=1`
Expected: FAIL to compile (`send_email_via_channel` not found).

- [ ] **Step 3: Extract the helper** — `crates/vigil/src/notify/dispatch.rs`

Add the helper (near `send_to_channel`):
```rust
/// Shared email-send: parse an `EmailChannelConfig` JSON, build the SMTP
/// config + message, and hand off to the transport. Used by both `deliver`'s
/// email arm and the daily digest (which bypasses `deliver`), so the two
/// never diverge (incl. the `username`/`from` handling).
pub async fn send_email_via_channel(
    transport: &dyn crate::notify::Transport,
    config_json: &str,
    subject: &str,
    body_text: &str,
    body_html: Option<String>,
) -> anyhow::Result<()> {
    let cfg: EmailChannelConfig = serde_json::from_str(config_json)?;
    let smtp_cfg = SmtpConfig {
        host: cfg.host,
        port: cfg.port,
        security: cfg.security,
        username: cfg.username,
    };
    let email_msg = EmailMsg {
        to: cfg.to,
        from: cfg.from,
        subject: subject.to_string(),
        body_text: body_text.to_string(),
        body_html,
    };
    transport.send(&smtp_cfg, &email_msg).await
}
```

Replace the email arm of `send_to_channel` (the `if ch.channel_type == "email"` block body) with:
```rust
    if ch.channel_type == "email" {
        send_email_via_channel(
            state.transport.as_ref(),
            &ch.config,
            &msg.subject,
            &msg.body,
            msg.body_html.clone(),
        )
        .await
    } else {
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --test notify_email_helper --test notify --test notify_multi -- --test-threads=1`
Expected: PASS (new helper test + all existing dispatch tests unchanged — proves the refactor is behavior-preserving).

- [ ] **Step 5: Commit**

```bash
git commit -am "refactor(p4.3): extract shared send_email_via_channel from dispatch"
```

---

### Task 4: Re-notify

**Files:**
- Create: `crates/vigil/src/renotify.rs`
- Modify: `crates/vigil/src/lib.rs` (add `pub mod renotify;`), `crates/vigil/src/main.rs` (spawn). (No `common/mod.rs` change — reuse existing `test_state_offline()`.)
- Test: `crates/vigil/tests/renotify.rs` (create)

**Interfaces:**
- Consumes: `dispatch::deliver`, `templates::render`, `settings_store::renotify_hours`/`renotify_tick_seconds`, `state.anchor.current()`.
- Produces: `renotify::renotify_once(state: &AppState) -> anyhow::Result<()>`, `renotify::run(state: AppState)`.

- [ ] **Step 1: No harness change needed — reuse the existing offline builder**

`crates/vigil/tests/common/mod.rs` **already** has `pub async fn test_state_offline() -> TestEnv` (line 43) — it builds an `AppState` with `AnchorGate::with_prober(.., || false)` and calls `anchor.probe_and_update().await` so `state.anchor.current()` reads `Offline`. The offline re-notify test (Step 2) uses it directly; do NOT add a new `test_state_prober` (that would duplicate it). `FailingTransport` for Task 6 does not exist yet and is added in Task 6.

- [ ] **Step 2: Write the failing tests** — `crates/vigil/tests/renotify.rs`

```rust
mod common;
use common::{seed_monitor_with_email_channel, test_state, test_state_offline};
use vigil::renotify::renotify_once;

fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64
}

// Seed a down monitor (email channel) + an open, unacked incident started `age` secs ago.
async fn seed_down_incident(db: &sqlx::SqlitePool, age: i64) -> (i64, i64) {
    let mid = seed_monitor_with_email_channel(db).await;
    sqlx::query("UPDATE monitors SET status = 'down' WHERE id = ?").bind(mid).execute(db).await.unwrap();
    let iid: i64 = sqlx::query_scalar(
        "INSERT INTO incidents (monitor_id, started_at, acknowledged) VALUES (?, ?, 0) RETURNING id",
    ).bind(mid).bind(now() - age).fetch_one(db).await.unwrap();
    (mid, iid)
}

#[tokio::test]
async fn fires_reminder_when_overdue() {
    let env = test_state().await; // renotify_hours default 6
    seed_down_incident(&env.state.db, 7 * 3600).await; // 7h > 6h
    renotify_once(&env.state).await.unwrap();
    let sent = env.sent.lock().unwrap();
    assert_eq!(sent.len(), 1, "an overdue open incident must fire one reminder");
    assert!(sent[0].1.subject.starts_with("Reminder:"), "subject must be prefixed Reminder:");
    assert!(sent[0].1.body_text.contains("Still down for"), "body must carry elapsed");
}

#[tokio::test]
async fn does_not_fire_within_interval() {
    let env = test_state().await;
    seed_down_incident(&env.state.db, 2 * 3600).await; // 2h < 6h
    renotify_once(&env.state).await.unwrap();
    assert_eq!(env.sent.lock().unwrap().len(), 0);
}

#[tokio::test]
async fn does_not_fire_when_acknowledged_resolved_paused_or_unknown() {
    // acknowledged
    let env = test_state().await;
    let (_m, iid) = seed_down_incident(&env.state.db, 7 * 3600).await;
    sqlx::query("UPDATE incidents SET acknowledged = 1 WHERE id = ?").bind(iid).execute(&env.state.db).await.unwrap();
    renotify_once(&env.state).await.unwrap();
    assert_eq!(env.sent.lock().unwrap().len(), 0, "acknowledged incident is silent");

    // paused
    let env = test_state().await;
    let (mid, _iid) = seed_down_incident(&env.state.db, 7 * 3600).await;
    sqlx::query("UPDATE monitors SET is_paused = 1 WHERE id = ?").bind(mid).execute(&env.state.db).await.unwrap();
    renotify_once(&env.state).await.unwrap();
    assert_eq!(env.sent.lock().unwrap().len(), 0, "paused monitor is silent");

    // status not down (unknown) — the post-reconnect window
    let env = test_state().await;
    let (mid, _iid) = seed_down_incident(&env.state.db, 7 * 3600).await;
    sqlx::query("UPDATE monitors SET status = 'unknown' WHERE id = ?").bind(mid).execute(&env.state.db).await.unwrap();
    renotify_once(&env.state).await.unwrap();
    assert_eq!(env.sent.lock().unwrap().len(), 0, "unknown-status monitor must not re-notify");

    // resolved
    let env = test_state().await;
    let (_mid, iid) = seed_down_incident(&env.state.db, 7 * 3600).await;
    sqlx::query("UPDATE incidents SET resolved_at = ? WHERE id = ?").bind(now()).bind(iid).execute(&env.state.db).await.unwrap();
    renotify_once(&env.state).await.unwrap();
    assert_eq!(env.sent.lock().unwrap().len(), 0, "resolved incident is silent");
}

#[tokio::test]
async fn disabled_when_renotify_hours_zero() {
    let env = test_state().await;
    vigil::settings_store::set(&env.state.db, "notify.renotify_hours", "0").await.unwrap();
    seed_down_incident(&env.state.db, 99 * 3600).await;
    renotify_once(&env.state).await.unwrap();
    assert_eq!(env.sent.lock().unwrap().len(), 0);
}

#[tokio::test]
async fn skips_pass_when_connectivity_offline() {
    let env = test_state_offline().await; // existing helper; anchor.current() == Offline
    seed_down_incident(&env.state.db, 7 * 3600).await;
    renotify_once(&env.state).await.unwrap();
    assert_eq!(env.sent.lock().unwrap().len(), 0, "offline: do not remind about outages");
}

#[tokio::test]
async fn baseline_is_incident_scoped_not_monitor_wide() {
    let env = test_state().await;
    // A PRIOR resolved incident with an old down send at now-8h.
    let mid = seed_monitor_with_email_channel(&env.state.db).await;
    sqlx::query("UPDATE monitors SET status = 'down' WHERE id = ?").bind(mid).execute(&env.state.db).await.unwrap();
    let old_iid: i64 = sqlx::query_scalar(
        "INSERT INTO incidents (monitor_id, started_at, resolved_at, acknowledged) VALUES (?, ?, ?, 0) RETURNING id",
    ).bind(mid).bind(now() - 9 * 3600).bind(now() - 8 * 3600).fetch_one(&env.state.db).await.unwrap();
    sqlx::query(
        "INSERT INTO notification_log (monitor_id, channel_id, incident_id, trigger, sent_at, success) VALUES (?, 1, ?, 'down', ?, 1)",
    ).bind(mid).bind(old_iid).bind(now() - 8 * 3600).execute(&env.state.db).await.unwrap();
    // A NEW incident started 1h ago with NO log row of its own.
    sqlx::query("INSERT INTO incidents (monitor_id, started_at, acknowledged) VALUES (?, ?, 0)")
        .bind(mid).bind(now() - 3600).execute(&env.state.db).await.unwrap();

    renotify_once(&env.state).await.unwrap();
    assert_eq!(env.sent.lock().unwrap().len(), 0,
        "new incident's baseline is its OWN start (1h), not the prior incident's 8h-old send");
}

#[tokio::test]
async fn baseline_advances_no_double_fire() {
    let env = test_state().await;
    // Disable deliver()'s 15-min cooldown so ONLY the re-notify baseline gates
    // the second pass — otherwise the cooldown would suppress it regardless and
    // the test could not distinguish a working baseline from a broken one.
    vigil::settings_store::set(&env.state.db, "notify.cooldown_minutes", "0").await.unwrap();
    seed_down_incident(&env.state.db, 7 * 3600).await;
    renotify_once(&env.state).await.unwrap();
    renotify_once(&env.state).await.unwrap();
    assert_eq!(env.sent.lock().unwrap().len(), 1, "baseline advanced → second immediate pass does not double-fire");
}

#[tokio::test]
async fn deleted_monitor_produces_no_reminder_and_no_panic() {
    let env = test_state().await;
    let (mid, _iid) = seed_down_incident(&env.state.db, 7 * 3600).await;
    // Deleting the monitor FK-cascades its incident (incidents.monitor_id
    // ON DELETE CASCADE), so the scan's JOIN drops it → no reminder, no panic.
    // (This verifies cascade cleanup. The `let Some(m) = m else { continue }`
    // in renotify_once is a defensive guard for a true mid-pass delete race,
    // near-unreachable given the JOIN — not exercised here.)
    sqlx::query("DELETE FROM monitors WHERE id = ?").bind(mid).execute(&env.state.db).await.unwrap();
    renotify_once(&env.state).await.unwrap(); // must not panic
    assert_eq!(env.sent.lock().unwrap().len(), 0);
}

#[tokio::test]
async fn heartbeat_monitor_renotifies_with_reminder() {
    let env = test_state().await;
    // A heartbeat monitor, status='down', with a channel subscribed to
    // heartbeat_missed (seed_monitor_with_email_channel only subscribes
    // ["down","recovered"], so it would wrongly send zero here).
    let mid: i64 = sqlx::query_scalar(
        "INSERT INTO monitors (name, type, url, status, created_at, updated_at) \
         VALUES ('cron','heartbeat',NULL,'down',0,0) RETURNING id",
    ).fetch_one(&env.state.db).await.unwrap();
    let cid: i64 = sqlx::query_scalar(
        "INSERT INTO notification_channels (name, type, config, is_active, created_at) \
         VALUES ('e','email','{\"host\":\"h\",\"port\":25,\"security\":\"none\",\"from\":\"f@b\",\"to\":[\"a@b\"]}',1,0) RETURNING id",
    ).fetch_one(&env.state.db).await.unwrap();
    sqlx::query("INSERT INTO monitor_notifications (monitor_id, channel_id, triggers) VALUES (?, ?, '[\"heartbeat_missed\"]')")
        .bind(mid).bind(cid).execute(&env.state.db).await.unwrap();
    sqlx::query("INSERT INTO incidents (monitor_id, started_at, acknowledged) VALUES (?, ?, 0)")
        .bind(mid).bind(now() - 7 * 3600).execute(&env.state.db).await.unwrap();

    renotify_once(&env.state).await.unwrap();
    let sent = env.sent.lock().unwrap();
    assert_eq!(sent.len(), 1, "heartbeat outage must re-notify via heartbeat_missed");
    assert!(sent[0].1.subject.starts_with("Reminder:"), "reminder prefix for heartbeat too");
    assert!(sent[0].1.body_text.contains("Still down for"));
}

#[tokio::test]
async fn first_down_alert_is_byte_identical_not_decorated() {
    // Guard the "decorate in renotify, DON'T touch templates" choice: the
    // initial (non-reminder) down alert must be exactly as it was pre-P4.3.
    let env = test_state().await;
    let mid = seed_monitor_with_email_channel(&env.state.db).await; // name 'seed'
    let m: vigil::models::Monitor = sqlx::query_as("SELECT * FROM monitors WHERE id = ?")
        .bind(mid).fetch_one(&env.state.db).await.unwrap();
    vigil::notify::dispatch::on_transition(&env.state, &m, vigil::models::Trigger::Down, Some(1)).await.unwrap();
    let sent = env.sent.lock().unwrap();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].1.subject, "🔴 seed is DOWN", "first-alert subject unchanged");
    assert!(!sent[0].1.body_text.contains("Still down for"), "first alert is not decorated");
}
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo test --test renotify -- --test-threads=1`
Expected: FAIL to compile (`vigil::renotify` missing).

- [ ] **Step 4: Implement `renotify.rs`** — `crates/vigil/src/renotify.rs`

```rust
//! P4.3 re-notify: re-fire the down alert for an ongoing, unacknowledged
//! outage on a global cadence (`notify.renotify_hours`, 0 = off) until it
//! resolves. Reuses the `dispatch::deliver` funnel (maintenance-mute,
//! per-channel cooldown, notification_log) — the log row is the clock.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::app::AppState;
use crate::models::{Connectivity, Monitor, Trigger};
use crate::notify::{dispatch, templates, NotifyMsg, TemplateCtx};
use crate::settings_store;

fn now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs() as i64
}

#[derive(sqlx::FromRow)]
struct OpenIncident {
    incident_id: i64,
    monitor_id: i64,
    started_at: i64,
}

/// Compact "6h 3m" style elapsed string.
fn format_elapsed(secs: i64) -> String {
    let s = secs.max(0);
    let h = s / 3600;
    let m = (s % 3600) / 60;
    if h > 0 {
        format!("{h}h {m}m")
    } else {
        format!("{m}m")
    }
}

/// Build a reminder message by rendering the base down/heartbeat alert, then
/// decorating it uniformly (works for BOTH triggers — no template change).
fn build_reminder_msg(m: &Monitor, trigger: Trigger, started_at: i64, now_ts: i64) -> NotifyMsg {
    let elapsed = now_ts - started_at;
    let ctx = TemplateCtx {
        monitor_name: m.name.clone(),
        url: m.url.clone().unwrap_or_default(),
        status: "down".to_string(),
        status_code: None,
        error: None,
        response_time_ms: None,
        duration: Some(elapsed),
        checked_at: now_ts,
    };
    let (subject, body_text, body_html) = templates::render(trigger, &ctx);
    NotifyMsg {
        monitor_name: ctx.monitor_name.clone(),
        url: ctx.url.clone(),
        status: ctx.status.clone(),
        status_code: None,
        error: None,
        response_time_ms: None,
        duration: Some(elapsed),
        ssl_days: None,
        domain_days: None,
        checked_at: now_ts,
        incident_url: None,
        subject: format!("Reminder: {subject}"),
        body: format!("{body_text}\n\nStill down for {}.", format_elapsed(elapsed)),
        body_html,
    }
}

/// One re-notify pass: fire an overdue reminder for every open, unacked,
/// confirmed-`down`, non-paused incident whose current-incident baseline is
/// older than `renotify_hours`.
pub async fn renotify_once(state: &AppState) -> anyhow::Result<()> {
    let hours = settings_store::renotify_hours(&state.db).await;
    if hours <= 0 {
        return Ok(());
    }
    if state.anchor.current().await == Connectivity::Offline {
        return Ok(());
    }
    let now_ts = now();
    let threshold = hours * 3600;

    let open: Vec<OpenIncident> = sqlx::query_as(
        "SELECT i.id AS incident_id, i.monitor_id, i.started_at \
         FROM incidents i JOIN monitors m ON m.id = i.monitor_id \
         WHERE i.resolved_at IS NULL AND i.acknowledged = 0 \
           AND m.is_paused = 0 AND m.status = 'down'",
    )
    .fetch_all(&state.db)
    .await?;

    for inc in open {
        let m: Option<Monitor> = sqlx::query_as("SELECT * FROM monitors WHERE id = ?")
            .bind(inc.monitor_id)
            .fetch_optional(&state.db)
            .await?;
        let Some(m) = m else { continue }; // deleted mid-pass → skip

        let last_reminder: Option<i64> = sqlx::query_scalar(
            "SELECT MAX(sent_at) FROM notification_log \
             WHERE incident_id = ? AND trigger IN ('down','heartbeat_missed')",
        )
        .bind(inc.incident_id)
        .fetch_one(&state.db)
        .await?;
        let baseline = last_reminder.unwrap_or(inc.started_at);
        if now_ts - baseline < threshold {
            continue;
        }

        // TOCTOU re-check: a recovery/ack may have landed since the batch scan.
        let still: Option<(Option<i64>, bool)> =
            sqlx::query_as("SELECT resolved_at, acknowledged FROM incidents WHERE id = ?")
                .bind(inc.incident_id)
                .fetch_optional(&state.db)
                .await?;
        if !matches!(still, Some((None, false))) {
            continue;
        }

        let trigger = if m.r#type == "heartbeat" {
            Trigger::HeartbeatMissed
        } else {
            Trigger::Down
        };
        let msg = build_reminder_msg(&m, trigger, inc.started_at, now_ts);
        dispatch::deliver(state, &m, trigger, &msg, Some(inc.incident_id)).await?;
    }
    Ok(())
}

/// The re-notify loop: scan every `notify.renotify_tick_seconds` (default 300).
pub async fn run(state: AppState) {
    loop {
        let tick = settings_store::renotify_tick_seconds(&state.db).await;
        if let Err(error) = renotify_once(&state).await {
            tracing::error!(%error, "renotify pass failed");
        }
        tokio::time::sleep(Duration::from_secs(tick.max(1) as u64)).await;
    }
}
```

Add to `crates/vigil/src/lib.rs`, alongside the other `pub mod` lines:
```rust
pub mod renotify;
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test --test renotify -- --test-threads=1`
Expected: PASS (all re-notify tests).

- [ ] **Step 6: Spawn the task** — `crates/vigil/src/main.rs`

Add `renotify` to the `use vigil::{...}` import, and after `tokio::spawn(maintenance_windows::run(state.clone()));` add:
```rust
    tokio::spawn(renotify::run(state.clone()));
```

- [ ] **Step 7: Build + commit**

Run: `cargo build`
Expected: compiles.
```bash
git commit -am "feat(p4.3): re-notify loop for ongoing outages (incident-scoped, ack-silenced)"
```

---

### Task 5: Digest compute (`DigestSummary` + `build`)

**Files:**
- Create: `crates/vigil/src/digest.rs`
- Modify: `crates/vigil/src/lib.rs` (add `pub mod digest;`)
- Test: `crates/vigil/tests/digest.rs` (create)

**Interfaces:**
- Consumes: `uptime::compute`, `maintenance_windows::active_windows`/`resolve::{maintenance_intervals, subtract_intervals, parse_tags}`, `rollup::day_bounds`, `SslCert`/`DomainInfo`.
- Produces: `digest::{DigestSummary, FleetSummary, DigestIncident, DigestDown, DigestExpiration}` (all `pub`, `Serialize`), `digest::build(state: &AppState, day: &str) -> anyhow::Result<DigestSummary>`.

- [ ] **Step 1: Write the failing tests** — `crates/vigil/tests/digest.rs`

```rust
mod common;
use common::test_state;
use vigil::digest::build;
use vigil::rollup::{day_bounds, day_str};

fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64
}

// yesterday's date string + its [ds,de) bounds
fn yesterday() -> (String, i64, i64) {
    let d = day_str(now() - 86_400);
    let (ds, de) = day_bounds(&d);
    (d, ds, de)
}

async fn seed_monitor(db: &sqlx::SqlitePool, name: &str, kind: &str) -> i64 {
    sqlx::query_scalar(
        "INSERT INTO monitors (name, type, url, created_at, updated_at) VALUES (?, ?, 'https://x', 0, 0) RETURNING id",
    ).bind(name).bind(kind).fetch_one(db).await.unwrap()
}

#[tokio::test]
async fn build_counts_yesterday_incident_downtime() {
    let env = test_state().await;
    let (day, ds, _de) = yesterday();
    let mid = seed_monitor(&env.state.db, "api", "http").await;
    // a 1-hour outage inside yesterday, and one check so had_any_check=true
    sqlx::query("INSERT INTO incidents (monitor_id, started_at, resolved_at, cause) VALUES (?, ?, ?, 'timeout')")
        .bind(mid).bind(ds + 3600).bind(ds + 7200).execute(&env.state.db).await.unwrap();
    sqlx::query("INSERT INTO checks (monitor_id, checked_at, status) VALUES (?, ?, 'up')")
        .bind(mid).bind(ds + 100).execute(&env.state.db).await.unwrap();

    let s = build(&env.state, &day).await.unwrap();
    assert_eq!(s.day, day);
    assert_eq!(s.fleet.monitors_total, 1);
    assert_eq!(s.fleet.incidents, 1);
    assert_eq!(s.fleet.downtime_seconds, 3600);
    assert_eq!(s.fleet.clean_monitors, 0);
    assert!(s.fleet.uptime_pct.unwrap() < 100.0 && s.fleet.uptime_pct.unwrap() > 95.0);
    assert_eq!(s.incidents.len(), 1);
    assert_eq!(s.incidents[0].monitor_name, "api");
    assert_eq!(s.incidents[0].duration_seconds, Some(3600));
}

#[tokio::test]
async fn maintenance_covered_outage_is_excluded_from_uptime() {
    let env = test_state().await;
    let (day, ds, de) = yesterday();
    let mid = seed_monitor(&env.state.db, "api", "http").await;
    sqlx::query("INSERT INTO checks (monitor_id, checked_at, status) VALUES (?, ?, 'up')")
        .bind(mid).bind(ds + 100).execute(&env.state.db).await.unwrap();
    // outage fully inside a maintenance window covering the whole day for this monitor
    sqlx::query("INSERT INTO incidents (monitor_id, started_at, resolved_at, cause) VALUES (?, ?, ?, 'timeout')")
        .bind(mid).bind(ds + 3600).bind(ds + 7200).execute(&env.state.db).await.unwrap();
    let target = format!("[{mid}]");
    // Window covers only the OUTAGE (ds+3000..ds+7800 ⊇ the ds+3600..ds+7200
    // incident), NOT the whole day — otherwise eff_denom=0 → uptime_pct=None
    // and the Some(100.0) assertion would fail (uptime.rs eff_denom<=0 branch).
    let _ = de;
    sqlx::query(
        "INSERT INTO maintenance_windows (name, scope, target_ref, starts_at, ends_at, recurrence, suppress, is_active, created_at) \
         VALUES ('w','monitors',?,?,?,NULL,'alerts',1,0)",
    ).bind(target).bind(ds + 3000).bind(ds + 7800).execute(&env.state.db).await.unwrap();

    let s = build(&env.state, &day).await.unwrap();
    assert_eq!(s.fleet.uptime_pct, Some(100.0), "outage fully inside maintenance → excluded, fleet 100%");
    assert_eq!(s.fleet.clean_monitors, 1);
    assert_eq!(s.fleet.downtime_seconds, 0);
}

#[tokio::test]
async fn armed_heartbeat_counts_as_having_data() {
    let env = test_state().await;
    let (day, _ds, _de) = yesterday();
    let mid = seed_monitor(&env.state.db, "cron", "heartbeat").await;
    sqlx::query("UPDATE monitors SET last_ping_at = ? WHERE id = ?").bind(now()).bind(mid).execute(&env.state.db).await.unwrap();

    let s = build(&env.state, &day).await.unwrap();
    // no incidents, armed → clean, fleet 100%
    assert_eq!(s.fleet.clean_monitors, 1);
    assert_eq!(s.fleet.uptime_pct, Some(100.0));
}

#[tokio::test]
async fn expirations_surface_invalid_cert_and_unqueryable_domain() {
    let env = test_state().await;
    let (day, _ds, _de) = yesterday();
    let mid = seed_monitor(&env.state.db, "site", "http").await;
    sqlx::query("UPDATE monitors SET ssl_check_enabled = 1, domain_check_enabled = 1 WHERE id = ?")
        .bind(mid).execute(&env.state.db).await.unwrap();
    sqlx::query("INSERT INTO ssl_certs (monitor_id, is_valid, days_remaining, invalid_alerted) VALUES (?, 0, -2, 0)")
        .bind(mid).execute(&env.state.db).await.unwrap();
    sqlx::query("INSERT INTO domain_info (monitor_id, queryable, days_remaining) VALUES (?, 0, NULL)")
        .bind(mid).execute(&env.state.db).await.unwrap();

    let s = build(&env.state, &day).await.unwrap();
    let ssl = s.expirations.iter().find(|e| e.kind == "ssl").unwrap();
    assert_eq!(ssl.flag, "invalid");
    let dom = s.expirations.iter().find(|e| e.kind == "domain").unwrap();
    assert_eq!(dom.flag, "unknown");
    assert_eq!(dom.days_remaining, None);
}

#[tokio::test]
async fn quiet_day_is_all_green_and_sendable() {
    let env = test_state().await;
    let (day, ds, _de) = yesterday();
    let mid = seed_monitor(&env.state.db, "ok", "http").await;
    sqlx::query("INSERT INTO checks (monitor_id, checked_at, status) VALUES (?, ?, 'up')")
        .bind(mid).bind(ds + 100).execute(&env.state.db).await.unwrap();

    let s = build(&env.state, &day).await.unwrap();
    assert_eq!(s.fleet.uptime_pct, Some(100.0));
    assert_eq!(s.fleet.clean_monitors, 1);
    assert!(s.incidents.is_empty());
    assert!(s.currently_down.is_empty());
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --test digest -- --test-threads=1`
Expected: FAIL to compile (`vigil::digest` missing).

- [ ] **Step 3: Implement the types + `build`** — `crates/vigil/src/digest.rs`

```rust
//! P4.3 daily digest. Computes yesterday's (UTC) fleet uptime, incidents and
//! upcoming SSL/domain expirations LIVE from `incidents` + `uptime::compute`
//! + maintenance intervals (NOT the aggregate table, which is untimely at
//! fire time and does not exclude maintenance). See §4.5 of the spec.

use serde::Serialize;

use crate::app::AppState;
use crate::maintenance_windows::{self, resolve};
use crate::models::{DomainInfo, Monitor, SslCert};
use crate::rollup;
use crate::uptime::{self, Span};

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct FleetSummary {
    pub uptime_pct: Option<f64>,
    pub monitors_total: i64,
    pub clean_monitors: i64,
    pub incidents: i64,
    pub downtime_seconds: i64,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct DigestIncident {
    pub monitor_name: String,
    pub started_at: i64,
    pub resolved_at: Option<i64>,
    pub duration_seconds: Option<i64>,
    pub cause: Option<String>,
    pub status_code: Option<i64>,
    pub error_message: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct DigestDown {
    pub monitor_name: String,
    pub since: i64,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct DigestExpiration {
    pub monitor_name: String,
    pub kind: String, // "ssl" | "domain"
    pub days_remaining: Option<i64>,
    pub flag: String, // "expiring" | "invalid" | "unknown"
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct DigestSummary {
    pub day: String,
    pub fleet: FleetSummary,
    pub incidents: Vec<DigestIncident>,
    pub currently_down: Vec<DigestDown>,
    pub expirations: Vec<DigestExpiration>,
}

fn round2(x: f64) -> f64 {
    (x * 100.0).round() / 100.0
}

/// Build the digest for a completed UTC `day` ("YYYY-MM-DD").
pub async fn build(state: &AppState, day: &str) -> anyhow::Result<DigestSummary> {
    let (ds, de) = rollup::day_bounds(day);
    let windows = maintenance_windows::active_windows(&state.db).await;

    let monitors: Vec<Monitor> = sqlx::query_as("SELECT * FROM monitors").fetch_all(&state.db).await?;
    let monitors_total = monitors.len() as i64;

    let mut total_down = 0i64;
    let mut total_denom = 0i64;
    let mut clean = 0i64;

    for m in &monitors {
        if m.is_paused {
            continue;
        }
        let raw: Vec<(i64, Option<i64>)> = sqlx::query_as(
            "SELECT started_at, resolved_at FROM incidents \
             WHERE monitor_id = ? AND started_at < ? AND (resolved_at IS NULL OR resolved_at > ?)",
        )
        .bind(m.id)
        .bind(de)
        .bind(ds)
        .fetch_all(&state.db)
        .await?;
        let spans: Vec<Span> = raw.into_iter().map(|(start, end)| Span { start, end }).collect();

        let has_checks: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM checks WHERE monitor_id = ? AND checked_at >= ? AND checked_at < ?)",
        )
        .bind(m.id)
        .bind(ds)
        .bind(de)
        .fetch_one(&state.db)
        .await?;
        let is_heartbeat = m.r#type == "heartbeat";
        let armed = m.last_ping_at.is_some();
        let had_any = has_checks || (is_heartbeat && armed);
        if !had_any {
            continue; // exclude no-data monitors from the fleet weighting
        }

        let tags = resolve::parse_tags(m.tags.as_deref().unwrap_or(""));
        let maint = resolve::maintenance_intervals(&windows, m.id, &tags, ds, de);
        let u = uptime::compute(&spans, ds, de, had_any, &maint);
        let eff_denom: i64 = resolve::subtract_intervals((ds, de), &maint)
            .iter()
            .map(|(s, e)| e - s)
            .sum();
        total_down += u.downtime_seconds;
        total_denom += eff_denom;
        if u.downtime_seconds == 0 {
            clean += 1;
        }
    }

    let fleet_uptime = if total_denom > 0 {
        Some(round2((1.0 - total_down as f64 / total_denom as f64) * 100.0))
    } else {
        None
    };

    // Incidents overlapping the day (started_at < de AND (unresolved OR resolved_at > ds)).
    let inc_rows: Vec<(i64, Option<i64>, Option<String>, Option<i64>, Option<String>, String)> = sqlx::query_as(
        "SELECT i.started_at, i.resolved_at, i.cause, i.status_code, i.error_message, m.name \
         FROM incidents i JOIN monitors m ON m.id = i.monitor_id \
         WHERE i.started_at < ? AND (i.resolved_at IS NULL OR i.resolved_at > ?) \
         ORDER BY i.started_at",
    )
    .bind(de)
    .bind(ds)
    .fetch_all(&state.db)
    .await?;
    let incidents: Vec<DigestIncident> = inc_rows
        .into_iter()
        .map(|(started_at, resolved_at, cause, status_code, error_message, name)| DigestIncident {
            monitor_name: name,
            started_at,
            resolved_at,
            duration_seconds: resolved_at.map(|r| r - started_at),
            cause,
            status_code,
            error_message,
        })
        .collect();

    // Currently down at send time.
    let down_rows: Vec<(String, i64)> = sqlx::query_as(
        "SELECT m.name, i.started_at FROM incidents i JOIN monitors m ON m.id = i.monitor_id \
         WHERE i.resolved_at IS NULL ORDER BY i.started_at",
    )
    .fetch_all(&state.db)
    .await?;
    let currently_down: Vec<DigestDown> = down_rows
        .into_iter()
        .map(|(monitor_name, since)| DigestDown { monitor_name, since })
        .collect();

    // Expirations (inside warning window OR invalid/unqueryable).
    let mut expirations = Vec::new();
    for m in &monitors {
        if m.ssl_check_enabled {
            let cert: Option<SslCert> = sqlx::query_as("SELECT * FROM ssl_certs WHERE monitor_id = ?")
                .bind(m.id)
                .fetch_optional(&state.db)
                .await?;
            if let Some(c) = cert {
                let max_t = serde_json::from_str::<Vec<i64>>(&m.ssl_alert_days)
                    .unwrap_or_default()
                    .into_iter()
                    .max()
                    .unwrap_or(0);
                let invalid = c.is_valid == Some(false);
                let expiring = c.days_remaining.map(|d| d <= max_t).unwrap_or(false);
                if invalid || expiring {
                    expirations.push(DigestExpiration {
                        monitor_name: m.name.clone(),
                        kind: "ssl".to_string(),
                        days_remaining: c.days_remaining,
                        flag: if invalid { "invalid" } else { "expiring" }.to_string(),
                    });
                }
            }
        }
        if m.domain_check_enabled {
            let dom: Option<DomainInfo> = sqlx::query_as("SELECT * FROM domain_info WHERE monitor_id = ?")
                .bind(m.id)
                .fetch_optional(&state.db)
                .await?;
            if let Some(d) = dom {
                let max_t = serde_json::from_str::<Vec<i64>>(&m.domain_alert_days)
                    .unwrap_or_default()
                    .into_iter()
                    .max()
                    .unwrap_or(0);
                let unknown = d.queryable == Some(false);
                let expiring = d.days_remaining.map(|dd| dd <= max_t).unwrap_or(false);
                if unknown || expiring {
                    expirations.push(DigestExpiration {
                        monitor_name: m.name.clone(),
                        kind: "domain".to_string(),
                        days_remaining: d.days_remaining,
                        flag: if unknown { "unknown" } else { "expiring" }.to_string(),
                    });
                }
            }
        }
    }

    Ok(DigestSummary {
        day: day.to_string(),
        fleet: FleetSummary {
            uptime_pct: fleet_uptime,
            monitors_total,
            clean_monitors: clean,
            incidents: incidents.len() as i64,
            downtime_seconds: total_down,
        },
        incidents,
        currently_down,
        expirations,
    })
}
```

Add to `crates/vigil/src/lib.rs`:
```rust
pub mod digest;
```

Verify `DomainInfo` and `SslCert` are exported from `crate::models` (they are — public structs). If `Monitor`/`SslCert`/`DomainInfo` need explicit import paths, use `crate::models::{DomainInfo, Monitor, SslCert}`.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --test digest -- --test-threads=1`
Expected: PASS (5 tests).

- [ ] **Step 5: Commit**

```bash
git commit -am "feat(p4.3): digest compute (build DigestSummary from incidents + uptime::compute)"
```

---

### Task 6: Digest delivery + scheduler

**Files:**
- Modify: `crates/vigil/src/digest.rs`, `crates/vigil/src/main.rs` (spawn), `crates/vigil/tests/common/mod.rs` (add `FailingTransport` + a builder)
- Test: `crates/vigil/tests/digest.rs` (extend)

**Interfaces:**
- Consumes: `dispatch::send_email_via_channel` (Task 3), `settings_store::{digest_recipients, digest_time, digest_tick_seconds, digest_enabled, get, set}`, `rollup::{day_str}`.
- Produces: `digest::parse_digest_time(&str) -> i64`, `digest::should_send(now_ts, today, last_sent_day, fire_offset) -> bool`, `digest::send(&AppState, &DigestSummary) -> SendOutcome`, `digest::seed_marker_if_absent(&AppState)`, `digest::tick_once(&AppState)`, `digest::run(AppState)`.

- [ ] **Step 1: Add a `FailingTransport` test double + builder** — `crates/vigil/tests/common/mod.rs`

```rust
pub struct FailingTransport;

#[async_trait::async_trait]
impl vigil::notify::Transport for FailingTransport {
    async fn send(&self, _cfg: &SmtpConfig, _msg: &EmailMsg) -> anyhow::Result<()> {
        anyhow::bail!("smtp down")
    }
}

/// A TestEnv whose transport ALWAYS errors (for the all-failed digest path).
pub async fn test_state_failing_transport() -> TestEnv {
    let (pool, dir) = fresh_pool().await;
    let sent = Arc::new(Mutex::new(Vec::new()));
    let sent_http = Arc::new(Mutex::new(Vec::new()));
    let (bus, _busrx) = tokio::sync::broadcast::channel(64);
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let anchor = Arc::new(vigil::anchor::AnchorGate::with_prober(bus.clone(), Box::new(|| true)));
    let state = AppState {
        db: pool,
        bus,
        transport: Arc::new(FailingTransport),
        http_sender: Arc::new(RecordingHttpSender { sent_http: sent_http.clone() }),
        sched_tx: tx,
        anchor,
    };
    TestEnv { state, sent, sent_http, _rx: rx, _dir: dir }
}
```
(`async_trait` is already a dependency; add `use async_trait::async_trait;` or the fully-qualified attribute as the file already does for the other doubles.)

- [ ] **Step 2: Write the failing tests** — append to `crates/vigil/tests/digest.rs`

```rust
use vigil::digest::{parse_digest_time, seed_marker_if_absent, send, should_send, tick_once, SendOutcome};

#[test]
fn parse_digest_time_and_should_send() {
    assert_eq!(parse_digest_time("08:00"), 8 * 3600);
    assert_eq!(parse_digest_time("07:30"), 7 * 3600 + 1800);
    assert_eq!(parse_digest_time("nonsense"), 8 * 3600); // fallback
    assert_eq!(parse_digest_time("99:99"), 8 * 3600); // out of range → fallback

    let (_d, ds, _de) = yesterday(); // reuse helper; ds is a day start
    // today at fire offset 0: any time in today >= today_start
    let today = day_str(now());
    let (today_start, _) = day_bounds(&today);
    assert!(should_send(today_start + 10, &today, "", 0));
    assert!(!should_send(today_start + 10, &today, &today, 0), "already sent today");
    assert!(!should_send(today_start - 5, &today, "", 10), "before fire time");
    let _ = ds;
}

#[tokio::test]
async fn send_fans_out_to_email_recipients_and_logs() {
    let env = test_state().await;
    // one active email channel
    let cid: i64 = sqlx::query_scalar(
        "INSERT INTO notification_channels (name, type, config, is_active, created_at) \
         VALUES ('e','email','{\"host\":\"h\",\"port\":25,\"security\":\"none\",\"from\":\"f@b\",\"to\":[\"a@b\"]}',1,0) RETURNING id",
    ).fetch_one(&env.state.db).await.unwrap();
    vigil::settings_store::set(&env.state.db, "notify.digest_recipients", &format!("[{cid}]")).await.unwrap();

    let summary = build(&env.state, &day_str(now() - 86_400)).await.unwrap();
    let outcome = send(&env.state, &summary).await;
    assert!(matches!(outcome, SendOutcome::Delivered));
    assert_eq!(env.sent.lock().unwrap().len(), 1);
    let logged: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM notification_log WHERE trigger = 'digest' AND success = 1")
        .fetch_one(&env.state.db).await.unwrap();
    assert_eq!(logged, 1);
}

#[tokio::test]
async fn send_with_no_recipients_audits_and_returns_nothing_to_send() {
    let env = test_state().await; // digest_recipients default []
    let summary = build(&env.state, &day_str(now() - 86_400)).await.unwrap();
    let outcome = send(&env.state, &summary).await;
    assert!(matches!(outcome, SendOutcome::NothingToSend));
    let logged: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM notification_log WHERE trigger = 'digest' AND success = 0 AND error = 'no deliverable email recipients'",
    ).fetch_one(&env.state.db).await.unwrap();
    assert_eq!(logged, 1, "the dead switch must leave an audit row");
}

#[tokio::test]
async fn send_all_failed_returns_all_failed() {
    let env = test_state_failing_transport().await;
    let cid: i64 = sqlx::query_scalar(
        "INSERT INTO notification_channels (name, type, config, is_active, created_at) \
         VALUES ('e','email','{\"host\":\"h\",\"port\":25,\"security\":\"none\",\"from\":\"f@b\",\"to\":[\"a@b\"]}',1,0) RETURNING id",
    ).fetch_one(&env.state.db).await.unwrap();
    vigil::settings_store::set(&env.state.db, "notify.digest_recipients", &format!("[{cid}]")).await.unwrap();
    let summary = build(&env.state, &day_str(now() - 86_400)).await.unwrap();
    assert!(matches!(send(&env.state, &summary).await, SendOutcome::AllFailed));
}

#[tokio::test]
async fn tick_advances_marker_on_success_but_not_on_all_failed() {
    // success path
    let env = test_state().await;
    let cid: i64 = sqlx::query_scalar(
        "INSERT INTO notification_channels (name, type, config, is_active, created_at) \
         VALUES ('e','email','{\"host\":\"h\",\"port\":25,\"security\":\"none\",\"from\":\"f@b\",\"to\":[\"a@b\"]}',1,0) RETURNING id",
    ).fetch_one(&env.state.db).await.unwrap();
    let s = &env.state;
    vigil::settings_store::set(&s.db, "notify.digest_enabled", "1").await.unwrap();
    vigil::settings_store::set(&s.db, "notify.digest_time", "00:00").await.unwrap(); // always past
    vigil::settings_store::set(&s.db, "notify.digest_recipients", &format!("[{cid}]")).await.unwrap();
    tick_once(s).await.unwrap();
    let today = day_str(now());
    assert_eq!(vigil::settings_store::get(&s.db, "notify.digest_last_sent_day", "").await, today);

    // all-failed path: marker must NOT advance
    let env = test_state_failing_transport().await;
    let cid: i64 = sqlx::query_scalar(
        "INSERT INTO notification_channels (name, type, config, is_active, created_at) \
         VALUES ('e','email','{\"host\":\"h\",\"port\":25,\"security\":\"none\",\"from\":\"f@b\",\"to\":[\"a@b\"]}',1,0) RETURNING id",
    ).fetch_one(&env.state.db).await.unwrap();
    let s = &env.state;
    vigil::settings_store::set(&s.db, "notify.digest_enabled", "1").await.unwrap();
    vigil::settings_store::set(&s.db, "notify.digest_time", "00:00").await.unwrap();
    vigil::settings_store::set(&s.db, "notify.digest_recipients", &format!("[{cid}]")).await.unwrap();
    tick_once(s).await.unwrap();
    assert_eq!(vigil::settings_store::get(&s.db, "notify.digest_last_sent_day", "").await, "",
        "a total send failure must NOT advance the marker (retry next tick)");
}

#[tokio::test]
async fn seed_marker_only_when_absent() {
    let env = test_state().await;
    seed_marker_if_absent(&env.state).await.unwrap();
    let seeded = vigil::settings_store::get(&env.state.db, "notify.digest_last_sent_day", "").await;
    assert_eq!(seeded, day_str(now()), "fresh instance seeds today");
    // a present marker is left untouched
    vigil::settings_store::set(&env.state.db, "notify.digest_last_sent_day", "2020-01-01").await.unwrap();
    seed_marker_if_absent(&env.state).await.unwrap();
    assert_eq!(vigil::settings_store::get(&env.state.db, "notify.digest_last_sent_day", "").await, "2020-01-01");
}
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo test --test digest -- --test-threads=1`
Expected: FAIL to compile (`send`, `should_send`, etc. missing).

- [ ] **Step 4: Implement delivery + scheduler** — append to `crates/vigil/src/digest.rs`

Add imports at the top (extend the existing `use` block):
```rust
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::notify::dispatch;
use crate::settings_store;
```

Add:
```rust
fn now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs() as i64
}

#[derive(Debug, PartialEq)]
pub enum SendOutcome {
    Delivered,
    NothingToSend,
    AllFailed,
}

/// "HH:MM" (UTC) → seconds into the day. Falls back to 08:00 on any parse error.
pub fn parse_digest_time(s: &str) -> i64 {
    let parts: Vec<&str> = s.split(':').collect();
    if parts.len() == 2 {
        if let (Ok(h), Ok(m)) = (parts[0].parse::<i64>(), parts[1].parse::<i64>()) {
            if (0..24).contains(&h) && (0..60).contains(&m) {
                return h * 3600 + m * 60;
            }
        }
    }
    tracing::warn!(input = %s, "invalid digest_time; falling back to 08:00");
    8 * 3600
}

/// Pure scheduler decision: fire iff now has passed today's fire instant and
/// we have not already sent for `today` (lexicographic "YYYY-MM-DD" compare).
pub fn should_send(now_ts: i64, today: &str, last_sent_day: &str, fire_offset: i64) -> bool {
    let (today_start, _) = rollup::day_bounds(today);
    now_ts >= today_start + fire_offset && last_sent_day < today
}

async fn log_digest(state: &AppState, channel_id: Option<i64>, success: bool, error: Option<&str>) -> anyhow::Result<()> {
    sqlx::query(
        "INSERT INTO notification_log (monitor_id, channel_id, incident_id, trigger, sent_at, success, error) \
         VALUES (NULL, ?, NULL, 'digest', ?, ?, ?)",
    )
    .bind(channel_id)
    .bind(now())
    .bind(success)
    .bind(error)
    .execute(&state.db)
    .await?;
    Ok(())
}

/// Render a plaintext digest email. UTC throughout.
fn render_digest(s: &DigestSummary) -> (String, String) {
    let up = s.fleet.uptime_pct.map(|p| format!("{p:.2}%")).unwrap_or_else(|| "n/a".to_string());
    let subject = format!("Vigil daily digest — {} — {} uptime", s.day, up);
    let mut body = String::new();
    body.push_str(&format!("Vigil daily digest for {} (UTC)\n\n", s.day));
    body.push_str(&format!(
        "Fleet uptime: {up}\nMonitors: {} ({} clean)\nIncidents: {}\nTotal downtime: {}s\n\n",
        s.fleet.monitors_total, s.fleet.clean_monitors, s.fleet.incidents, s.fleet.downtime_seconds
    ));
    if s.incidents.is_empty() {
        body.push_str("No incidents.\n");
    } else {
        body.push_str("Incidents:\n");
        for i in &s.incidents {
            let dur = i.duration_seconds.map(|d| format!("{d}s")).unwrap_or_else(|| "ongoing".to_string());
            body.push_str(&format!(
                "  - {} | started {} | {} | {}\n",
                i.monitor_name, i.started_at, dur, i.cause.as_deref().unwrap_or("-")
            ));
        }
    }
    if !s.currently_down.is_empty() {
        body.push_str("\nCurrently down:\n");
        for d in &s.currently_down {
            body.push_str(&format!("  - {} (since {})\n", d.monitor_name, d.since));
        }
    }
    if !s.expirations.is_empty() {
        body.push_str("\nUpcoming expirations:\n");
        for e in &s.expirations {
            let days = e.days_remaining.map(|d| format!("{d}d")).unwrap_or_else(|| "unknown".to_string());
            body.push_str(&format!("  - {} {} [{}] {}\n", e.monitor_name, e.kind, e.flag, days));
        }
    }
    (subject, body)
}

/// Send the digest to every active email channel in `notify.digest_recipients`.
pub async fn send(state: &AppState, summary: &DigestSummary) -> SendOutcome {
    let ids = settings_store::digest_recipients(&state.db).await;
    let mut channels: Vec<(i64, String)> = Vec::new();
    for id in &ids {
        let cfg: Option<String> = sqlx::query_scalar(
            "SELECT config FROM notification_channels WHERE id = ? AND type = 'email' AND is_active = 1",
        )
        .bind(id)
        .fetch_optional(&state.db)
        .await
        .ok()
        .flatten();
        if let Some(cfg) = cfg {
            channels.push((*id, cfg));
        }
    }

    if channels.is_empty() {
        let _ = log_digest(state, None, false, Some("no deliverable email recipients")).await;
        tracing::warn!("digest enabled but no deliverable email recipients");
        return SendOutcome::NothingToSend;
    }

    let (subject, body) = render_digest(summary);
    let mut any_ok = false;
    for (id, cfg) in channels {
        let r = dispatch::send_email_via_channel(state.transport.as_ref(), &cfg, &subject, &body, None).await;
        let (ok, err) = match &r {
            Ok(()) => (true, None),
            Err(e) => (false, Some(e.to_string())),
        };
        any_ok |= ok;
        let _ = log_digest(state, Some(id), ok, err.as_deref()).await;
    }
    if any_ok {
        SendOutcome::Delivered
    } else {
        SendOutcome::AllFailed
    }
}

/// Seed the once-per-day marker to today on a brand-new instance (absent
/// marker), so a fresh install does not fire for a day it wasn't monitoring.
pub async fn seed_marker_if_absent(state: &AppState) -> anyhow::Result<()> {
    let existing: Option<String> =
        sqlx::query_scalar("SELECT value FROM settings WHERE key = 'notify.digest_last_sent_day'")
            .fetch_optional(&state.db)
            .await?;
    if existing.is_none() {
        settings_store::set(&state.db, "notify.digest_last_sent_day", &rollup::day_str(now())).await?;
    }
    Ok(())
}

/// One scheduler evaluation: if due, build yesterday's digest, send it, and
/// advance the marker ONLY on a delivered / nothing-to-send outcome (a total
/// send failure leaves the marker so the next tick retries within the day).
pub async fn tick_once(state: &AppState) -> anyhow::Result<()> {
    let now_ts = now();
    let today = rollup::day_str(now_ts);
    let last = settings_store::get(&state.db, "notify.digest_last_sent_day", "").await;
    let offset = parse_digest_time(&settings_store::digest_time(&state.db).await);
    if !should_send(now_ts, &today, &last, offset) {
        return Ok(());
    }
    let yesterday = rollup::day_str(now_ts - 86_400);
    let summary = build(state, &yesterday).await?;
    match send(state, &summary).await {
        SendOutcome::Delivered | SendOutcome::NothingToSend => {
            settings_store::set(&state.db, "notify.digest_last_sent_day", &today).await?;
        }
        SendOutcome::AllFailed => {
            tracing::warn!("digest send failed for all recipients; will retry next tick");
        }
    }
    Ok(())
}

/// The digest scheduler loop.
pub async fn run(state: AppState) {
    if let Err(error) = seed_marker_if_absent(&state).await {
        tracing::error!(%error, "digest marker seed failed");
    }
    loop {
        let tick = settings_store::digest_tick_seconds(&state.db).await;
        if settings_store::digest_enabled(&state.db).await {
            if let Err(error) = tick_once(&state).await {
                tracing::error!(%error, "digest tick failed");
            }
        }
        tokio::time::sleep(Duration::from_secs(tick.max(1) as u64)).await;
    }
}
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test --test digest -- --test-threads=1`
Expected: PASS (all digest tests).

- [ ] **Step 6: Spawn the task** — `crates/vigil/src/main.rs`

Add `digest` to the `use vigil::{...}` import, and after the `renotify::run` spawn add:
```rust
    tokio::spawn(digest::run(state.clone()));
```

- [ ] **Step 7: Build + commit**

Run: `cargo build`
```bash
git commit -am "feat(p4.3): daily digest delivery + UTC scheduler (dead-man's switch, marker-on-success)"
```

---

### Task 7: Frontend — settings controls + acknowledge hint

**Files:**
- Modify: `web/src/api.ts`, `web/src/components/Settings.tsx`, `web/src/components/Incidents.tsx`, `web/src/components/IncidentTimeline.tsx`
- Test: `web/src/__tests__/settings.test.tsx`

**Interfaces:**
- Consumes: `GET/PUT /api/settings` fields from Task 2.

- [ ] **Step 1: Write the failing test** — append to `web/src/__tests__/settings.test.tsx`

```tsx
test("saving re-notify hours PUTs renotify_hours", async () => {
  const puts: any[] = [];
  vi.stubGlobal(
    "fetch",
    vi.fn(async (url: any, opts?: any) => {
      if (url === "/api/settings" && opts?.method === "PUT") {
        puts.push(JSON.parse(opts.body));
        return { ok: true, json: async () => ({}) };
      }
      // GET /api/settings and GET /api/channels
      if (url === "/api/settings") {
        return { ok: true, json: async () => ({ anchors: [], retention_days: 30, renotify_hours: 6, digest_enabled: false, digest_time: "08:00", digest_recipients: [] }) };
      }
      return { ok: true, json: async () => [] };
    }) as any,
  );

  render(() => <Settings />);
  const input = await screen.findByLabelText(/re-notify interval/i);
  fireEvent.input(input, { target: { value: "12" } });
  fireEvent.click(screen.getByRole("button", { name: /save re-notify/i }));

  await screen.findByText(/saved/i);
  const put = puts.find((p) => "renotify_hours" in p);
  expect(put).toBeTruthy();
  expect(put.renotify_hours).toBe(12);
});
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cd web && npx vitest run settings`
Expected: FAIL (no "re-notify interval" control).

- [ ] **Step 3: Extend the `Settings` type + add controls**

`web/src/api.ts` — extend the interface:
```ts
export interface Settings {
  anchors: string[];
  cooldown_minutes: number;
  retention_days: number;
  accent: string;
  renotify_hours: number;
  digest_enabled: boolean;
  digest_time: string;
  digest_recipients: number[];
}
```

`web/src/components/Settings.tsx` — add signals (near the retention signals):
```tsx
  const [renotifyHours, setRenotifyHours] = createSignal<number>(6);
  const [renotifySaving, setRenotifySaving] = createSignal(false);
  const [renotifySaved, setRenotifySaved] = createSignal(false);

  const [digestEnabled, setDigestEnabled] = createSignal<boolean>(false);
  const [digestTime, setDigestTime] = createSignal<string>("08:00");
  const [digestRecipients, setDigestRecipients] = createSignal<number[]>([]);
  const [digestSaving, setDigestSaving] = createSignal(false);
  const [digestSaved, setDigestSaved] = createSignal(false);
```

Load-on-mount (inside the existing `onMount` try, after `setRetentionDays(...)`):
```tsx
      setRenotifyHours(typeof s?.renotify_hours === "number" ? s.renotify_hours : 6);
      setDigestEnabled(!!s?.digest_enabled);
      setDigestTime(typeof s?.digest_time === "string" ? s.digest_time : "08:00");
      setDigestRecipients(Array.isArray(s?.digest_recipients) ? s.digest_recipients : []);
```

Save handlers:
```tsx
  async function handleSaveRenotify() {
    setRenotifySaving(true);
    setRenotifySaved(false);
    try {
      await api.updateSettings({ renotify_hours: Math.max(0, Number(renotifyHours()) || 0) });
      setRenotifySaved(true);
    } catch {
      // leave as typed
    } finally {
      setRenotifySaving(false);
    }
  }

  async function handleSaveDigest() {
    setDigestSaving(true);
    setDigestSaved(false);
    try {
      await api.updateSettings({
        digest_enabled: digestEnabled(),
        digest_time: digestTime(),
        digest_recipients: digestRecipients(),
      });
      setDigestSaved(true);
    } catch {
      // leave as typed
    } finally {
      setDigestSaving(false);
    }
  }
```

Markup (add two `<section>`s mirroring the retention field). Re-notify:
```tsx
      <section class="form-section settings-section">
        <h3 class="form-section-title">Re-notify</h3>
        <div class="form-field">
          <label for="set-renotify">Re-notify interval (hours, 0 = off)</label>
          <input
            id="set-renotify"
            type="number"
            min={0}
            value={renotifyHours()}
            onInput={(e) => { setRenotifyHours(Number(e.currentTarget.value) || 0); setRenotifySaved(false); }}
          />
        </div>
        <div class="detail-actions">
          <button type="button" class="btn-accent" disabled={renotifySaving()} onClick={handleSaveRenotify}>
            {renotifySaving() ? "Saving…" : "Save re-notify"}
          </button>
        </div>
        <Show when={renotifySaved()}><div class="test-result mono">Saved.</div></Show>
      </section>
```

Digest (recipients: reuse the component's existing channels list if one is already loaded for the channel manager; otherwise load email channels into a local signal via `fetch("/api/channels")` in `onMount`, filter `type === "email"`, and render checkboxes bound to `digestRecipients()`):
```tsx
      <section class="form-section settings-section">
        <h3 class="form-section-title">Daily digest</h3>
        <div class="form-field">
          <label><input type="checkbox" checked={digestEnabled()} onInput={(e) => { setDigestEnabled(e.currentTarget.checked); setDigestSaved(false); }} /> Enable daily digest</label>
        </div>
        <div class="form-field">
          <label for="set-digest-time">Send time (HH:MM, UTC)</label>
          <input id="set-digest-time" type="text" value={digestTime()} onInput={(e) => { setDigestTime(e.currentTarget.value); setDigestSaved(false); }} />
        </div>
        <div class="form-field">
          <label>Recipient email channels</label>
          <For each={emailChannels()}>{(ch) => (
            <label class="check-row">
              <input
                type="checkbox"
                checked={digestRecipients().includes(ch.id)}
                onInput={(e) => {
                  const on = e.currentTarget.checked;
                  setDigestRecipients((prev) => on ? [...prev, ch.id] : prev.filter((x) => x !== ch.id));
                  setDigestSaved(false);
                }}
              /> {ch.name}
            </label>
          )}</For>
        </div>
        <div class="detail-actions">
          <button type="button" class="btn-accent" disabled={digestSaving()} onClick={handleSaveDigest}>
            {digestSaving() ? "Saving…" : "Save digest"}
          </button>
        </div>
        <Show when={digestSaved()}><div class="test-result mono">Saved.</div></Show>
      </section>
```

For `emailChannels()`: if the component already loads channels (it manages the channel list), derive `const emailChannels = () => channels().filter((c) => c.type === "email")`. If no channels signal exists yet, add `const [emailChannels, setEmailChannels] = createSignal<any[]>([]);` and, in `onMount`, `const chs = await fetch("/api/channels").then((r) => r.json()); setEmailChannels((chs || []).filter((c: any) => c.type === "email"));`. Reuse whichever already exists — do not add a duplicate channels fetch.

- [ ] **Step 4: Add the acknowledge hint** — `web/src/components/Incidents.tsx` AND `web/src/components/IncidentTimeline.tsx`

The Acknowledge controls live in BOTH `Incidents.tsx` (the global Incidents screen, button ~line 160) and `IncidentTimeline.tsx` (rendered inside the detail panel, button ~line 184) — NOT in `DetailPanel.tsx`. Add `title="Acknowledge (silences re-notify reminders)"` to the Acknowledge button in each, keeping the visible label "Acknowledge".

- [ ] **Step 5: Run tests + typecheck + build**

Run: `cd web && npx vitest run && npx tsc --noEmit && npx vite build`
Expected: all pass, tsc clean, build clean.

- [ ] **Step 6: Commit**

```bash
git commit -am "feat(p4.3): settings UI for re-notify + digest; acknowledge silences-reminders hint"
```

---

### Task 8: Full suite, live acceptance, finish

**Files:** none (verification + merge).

- [ ] **Step 1: Full backend suite (single-threaded)**

Run: `cd /home/cd/Work/vigil && cargo test -- --test-threads=1`
Expected: 0 failures. Also confirm rustls-only: `cargo tree -e normal,build,dev | grep -Ei "aws-lc|openssl"` returns nothing.

- [ ] **Step 2: Full frontend checks**

Run: `cd web && npx vitest run && npx tsc --noEmit && npx vite build`
Expected: all green.

- [ ] **Step 3: Live acceptance (ephemeral container, NO real emails)**

Build an ephemeral image + run on host `8098` with a fresh empty DB and **no channels** (so no emails are possible). Do NOT touch the production `vigil-data` volume. Drive via API/DB:
- **Re-notify:** create an http monitor; force `status='down'` + insert an open incident `started_at = now-7h` (via a direct DB write into the container, or a monitor that is genuinely down with a short interval); set `notify.renotify_hours=0.001`-equivalent by leaving default 6 and back-dating the incident; observe (with an email channel pointing at a local sink OR simply asserting a `notification_log` row with `trigger='down'` appears for the incident after the reaper interval). Acknowledge the incident and confirm no further `down` rows accrue.
- **Digest:** set `digest_enabled=1`, `digest_time=00:00`, add an email channel to `digest_recipients` (email host pointing at a black-hole/local sink), wait one `digest_tick_seconds`, and confirm a `notification_log` row with `trigger='digest'` and `notify.digest_last_sent_day` advanced. With zero recipients, confirm the `success=0, error='no deliverable email recipients'` audit row.
- Observe strictly via `GET /api/...` and direct DB reads inside the ephemeral container. Tear down the container + image + anonymous volume afterward.

- [ ] **Step 4: Finish the branch** — use superpowers:finishing-a-development-branch

Merge `feat/p4-notification-throttling-digest` to `master` as a **local fast-forward**, delete the branch, do **not** push to origin. Then rebuild + redeploy the production container (`docker compose up -d --build`) and confirm `healthz=200` and `/api/settings` returns the new keys.

- [ ] **Step 5: Update memory + SDD ledger**

Record P4.3 shipped in `.superpowers/sdd/progress.md` and the Vigil memory file; note P4.4 (monthly reports) is next.

---

## Self-Review

**Spec coverage:** §3 re-notify → Task 4; §3.6 settings → Task 1; §3.5 reminder decoration → Task 4 `build_reminder_msg`; §4 digest compute → Task 5; §4.2/4.4/4.6/4.8 scheduler/marker/delivery/audit → Task 6; §4.5 formula/expirations → Task 5; §6 API → Task 2; §4.6 shared email helper → Task 3; §7 frontend → Task 7; §8 tests → distributed across Tasks 4-7; §5 no-migration → honored (no migration task). All covered.

**Type consistency:** `renotify_once`/`build`/`send`/`tick_once`/`should_send`/`parse_digest_time`/`seed_marker_if_absent` names are used identically in tests and impl. `DigestSummary`/`FleetSummary`/`SendOutcome` field/variant names match between Task 5/6 impl and tests. `send_email_via_channel(&dyn Transport, &str, &str, &str, Option<String>)` signature matches its two call sites (Task 3 deliver arm, Task 6 send). Settings keys (`notify.renotify_hours`, `notify.digest_*`) are identical across Tasks 1/2/6.

**Placeholder scan:** every code step contains complete code; the only "match existing pattern" flex is the Task 7 recipients-channels list, which gives concrete fallback code plus a one-line reuse note (not a TODO).
