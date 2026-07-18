# Vigil P2 (Signal) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: superpowers:subagent-driven-development. Steps use `- [ ]`.
> **Autonomous build.** Builds on P1 (merged to `master`). Base: `2d1977c`. Branch `feat/p2-signal`.

**Goal:** Make Vigil's dashboard tell the full story — new monitor types (keyword/TCP-port/DNS/TCP-ping), daily rollups, a response-time chart, 90-day uptime bars, incident history + acknowledge, and a dense list view.

**Architecture:** Extends the P1 Rust/axum + SolidJS app. `probe::run` dispatches by monitor type; a version-ordered migration runner applies `0002`; a nightly+startup rollup catch-up populates `check_aggregates_daily`; new read endpoints (series/bars/incidents); frontend adds uPlot chart, uptime-bar, list view, incident timeline.

**Tech stack additions:** `hickory-resolver` (DNS), `uPlot` (frontend, bundled). Everything else inherited from P1 (rustls, sqlx-sqlite, tokio, axum, SolidJS/Vite).

Spec: [`docs/superpowers/specs/2026-07-17-vigil-p2-signal-design.md`](../specs/2026-07-17-vigil-p2-signal-design.md).

## Global Constraints

- Inherit ALL P1 constraints (rustls only — no openssl; i64 epoch seconds; SQLite `foreign_keys=ON`/WAL/`auto_vacuum`; uptime from incidents; non-root container; app bind `0.0.0.0:8090`, host `8099`; SMTP password only from Docker secret).
- **Migration runner is version-ordered** — a `const MIGRATIONS: &[(i64,&str)]` list; apply each version absent from `schema_migrations`, each in its own tx, recording `applied_at = now` (real epoch). Statement splitting strips `--` line comments before `split(';')`.
- **New monitor types:** `keyword`, `port`, `ping`, `dns`. `Cause` enum = `Timeout, Status, Connection, Dns, Keyword`.
- **Ping** = TCP-ping (P1 §15): explicit port → single attempt; null port → 443 then 80 sequential, each bounded by full `timeout_seconds`, report successful attempt latency.
- **Keyword** reads ≤ **2 MiB** of body, lossy UTF-8, matches within the read prefix; `keyword_case_sensitive` default 0 (insensitive).
- **DNS** via hickory-resolver; expected-value = case-insensitive substring on canonical record string form (A/AAAA→IP; CNAME/NS→target minus trailing dot; MX→`"<pref> <host>"`; TXT→concatenated).
- **Rollups:** completed UTC days only; today computed live (never stored). `avg_ms` 30d/90d = count-weighted `sum(avg*sample_count)/sum(sample_count)`. Rollup uptime uses **overlap** incident spans clipped to the day.
- **DEGRADED deferred** — `degraded_count` stays 0. Bars are still uptime-graded (up/amber/down).
- **uPlot bundled** (npm dep), no external CDN. Self-contained.
- Commit after every task. TDD. Conventional commits.

---

## Shared Types & Interfaces (the DRY backbone)

New/changed canonical definitions (existing P1 types unchanged unless noted).

```rust
// models.rs — Cause gains Keyword
pub enum Cause { Timeout, Status, Connection, Dns, Keyword }  // #[serde(rename_all="lowercase")]

// models.rs — Monitor gains 7 fields (after the existing ones, before runtime fields):
//   pub host: Option<String>, pub port: Option<i64>,
//   pub keyword: Option<String>, pub keyword_mode: Option<String>,
//   pub keyword_case_sensitive: bool, pub dns_record_type: Option<String>,
//   pub dns_expected_value: Option<String>,
// The manual FromRow reads these columns (keyword_case_sensitive: i64->bool).
// test_defaults_monitor() sets them all None/false.

// models.rs — Incident gains: pub acknowledged: bool
// (define an Incident struct + FromRow if not already present; add `acknowledged`.)

// models.rs — CreateMonitorDto: add `#[serde(default="d_http")] pub r#type: String`,
//   change `pub url: String` -> `pub url: Option<String>`, add the 7 optional monitor fields.
//   d_http() -> "http". UpdateMonitorDto: add the same as Option<>.

// probe/mod.rs — the dispatcher
pub async fn run(m: &crate::models::Monitor) -> crate::models::ProbeOutcome;
// dispatches on m.r#type: "http"|"keyword" -> http::probe; "port"|"ping" -> tcp::probe; "dns" -> dns::probe.

// probe/tcp.rs
pub async fn probe(m: &Monitor) -> ProbeOutcome;   // port + ping (ping = m.r#type=="ping")
// probe/dns.rs
pub async fn probe(m: &Monitor) -> ProbeOutcome;   // uses a resolver; injectable for tests

// rollup.rs (or in maintenance.rs)
pub async fn rollup_day(pool: &SqlitePool, day: &str) -> anyhow::Result<()>;  // completed UTC day
pub async fn rollup_catch_up(pool: &SqlitePool, retention_days: i64) -> anyhow::Result<()>;
pub async fn ensure_aggregates(pool: &SqlitePool, monitor_id: i64, retention_days: i64) -> anyhow::Result<()>;

// api validation helper
pub fn validate_monitor_dto(ty: &str, url: &Option<String>, host: &Option<String>, port: &Option<i64>,
    keyword: &Option<String>, keyword_mode: &Option<String>, dns_record_type: &Option<String>) -> Result<(), String>;
```

**Endpoint response shapes** (JSON):
- series: `[{ "t": i64, "ms": Option<i64>, "status": "up"|"down" }]`
- bars: `[{ "day": "YYYY-MM-DD", "uptime_pct": Option<f64>, "incidents": i64, "down_seconds": i64, "has_data": bool }]`
- incidents: `[{ "id", "monitor_id", "monitor_name", "started_at", "resolved_at": Option, "duration_seconds": Option, "cause": Option, "status_code": Option, "error_message": Option, "acknowledged": bool }]`
- stats (extended): `{ "uptime_pct": Option<f64>, "downtime_seconds": i64, "avg_ms": Option<f64>, "incidents": i64 }`

**UTC day helpers** (`rollup.rs`): `day_str(epoch)->"YYYY-MM-DD"`, `day_bounds(day)->(start_epoch, end_epoch)`. **Add `chrono = { version="0.4", default-features=false, features=["std"] }`** (no openssl; a `YYYY-MM-DD` string→epoch needs civil-date math — flooring an epoch does NOT work). `day_bounds`: `NaiveDate::parse_from_str(day, "%Y-%m-%d")?.and_hms_opt(0,0,0)?.and_utc().timestamp()` for start, `start + 86400` for end. `day_str`: `DateTime::<Utc>::from_timestamp(epoch,0)?.format("%Y-%m-%d")`.

**`ensure_aggregates(pool, monitor_id, retention_days)`** — retention-bounded: internally finds the monitor's last stored aggregate day and rolls up completed days from there through yesterday, bounded by `retention_days`. (Single canonical signature; no `since_day` param.)

---

## File Structure (new/modified)

Backend: `crates/vigil/src/db.rs` (runner), `migrations/0002_signal.sql` (new), `src/models.rs`, `src/probe/{mod,http,tcp,dns}.rs` (tcp/dns new), `src/worker.rs`, `src/rollup.rs` (new), `src/maintenance.rs`, `src/api/{monitors,incidents,mod}.rs` (incidents new), `Cargo.toml` (hickory-resolver). Frontend: `web/src/components/{UptimeBar,ResponseChart,IncidentTimeline,Incidents,ListView}.tsx` (new), `MonitorForm.tsx`, `DetailPanel.tsx`, `MonitorCard.tsx`, `App.tsx`, `Rail.tsx`, `api.ts`, `store.ts`, `web/package.json` (uplot).

---

## Task 1: Version-ordered migration runner + `0002` + models/DTO fields

**Files:** Modify `crates/vigil/src/db.rs`, `crates/vigil/src/models.rs`, `crates/vigil/src/api/monitors.rs` (create/update column lists); Create `crates/vigil/migrations/0002_signal.sql`. Test: `crates/vigil/tests/migrate2.rs`, inline in db.rs.

**Interfaces:** Produces the version-ordered runner; `0002` schema (§4 of spec); `Monitor`/`Incident`/DTO new fields.

- [ ] **Step 1: Write `migrations/0002_signal.sql`** — verbatim from spec §4 (7 monitor ALTERs, `incidents.acknowledged` ALTER, `check_aggregates_daily` table + `idx_aggregates_day`). Normal `--` comments allowed.

- [ ] **Step 2: Failing test** — `tests/migrate2.rs`:
```rust
#[tokio::test] async fn migration_0002_applies_on_fresh_and_v1() {
    // fresh DB: connect() applies 1 then 2
    let d = tempfile::tempdir().unwrap();
    let pool = vigil::db::connect(d.path().join("f.db").to_str().unwrap()).await.unwrap();
    let v: i64 = sqlx::query_scalar("SELECT MAX(version) FROM schema_migrations").fetch_one(&pool).await.unwrap();
    assert_eq!(v, 2);
    // new column exists + aggregates table exists + incidents.acknowledged exists
    sqlx::query("SELECT host, keyword, dns_record_type FROM monitors").fetch_optional(&pool).await.unwrap();
    sqlx::query("SELECT acknowledged FROM incidents").fetch_optional(&pool).await.unwrap();
    sqlx::query("SELECT monitor_id, day, sample_count FROM check_aggregates_daily").fetch_optional(&pool).await.unwrap();
    let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM schema_migrations WHERE version=2").fetch_one(&pool).await.unwrap();
    assert_eq!(n, 1);
}
#[tokio::test] async fn comment_stripping_applies() {
    // a statement with a trailing -- comment applies (indirect: 0002 has comments; assert a col exists — covered above)
}
```
- [ ] **Step 3: Run → FAIL.** `cargo test -p vigil --test migrate2`.
- [ ] **Step 4: Implement runner** in `db.rs`:
```rust
const MIGRATIONS: &[(i64, &str)] = &[
    (1, include_str!("../migrations/0001_init.sql")),
    (2, include_str!("../migrations/0002_signal.sql")),
];
async fn run_migrations(pool: &sqlx::SqlitePool) -> anyhow::Result<()> {
    sqlx::query("CREATE TABLE IF NOT EXISTS schema_migrations (version INTEGER PRIMARY KEY, applied_at INTEGER NOT NULL)")
        .execute(pool).await?;
    let now = <epoch secs>;
    for (v, sql) in MIGRATIONS {
        let done: Option<i64> = sqlx::query_scalar("SELECT version FROM schema_migrations WHERE version=?")
            .bind(v).fetch_optional(pool).await?;
        if done.is_some() { continue; }
        let mut tx = pool.begin().await?;
        for stmt in split_statements(sql) { if !stmt.trim().is_empty() { sqlx::query(&stmt).execute(&mut *tx).await?; } }
        sqlx::query("INSERT INTO schema_migrations (version, applied_at) VALUES (?, ?)").bind(v).bind(now)
            .execute(&mut *tx).await?;
        tx.commit().await?;
    }
    Ok(())
}
// split_statements: for each line, drop from an unquoted `--` to EOL, join with \n, split on ';'.
```
(A simple `--`-strip that ignores `--` inside string literals is acceptable for our migrations, which have no `--` inside strings; note that in a comment.)
- [ ] **Step 5: Implement models/DTO** — add the 7 `Monitor` fields + `FromRow` reads (`keyword_case_sensitive` i64→bool), add `Incident.acknowledged` (add an `Incident` struct + manual `FromRow` if none exists; read `acknowledged` i64→bool), extend `test_defaults_monitor()` (new fields None/false, `r#type` stays "http"). `CreateMonitorDto`: add `#[serde(default="d_http")] pub r#type: String`, change `url` to `Option<String>`, add the 7 optional fields (`d_http()->"http".into()`, `keyword_case_sensitive` default false). `UpdateMonitorDto`: same as Option. Update `api/monitors.rs` `create()` to INSERT `type` from the dto (not hardcoded 'http') + the 7 new columns, and `url` as Option. Extend `update()`'s UPDATE SQL to SET the 7 new columns (coalescing dto-over-existing like the other fields); **`type` is NOT mutable on edit** (set once at create — the form disables the type selector in edit mode). **Also fix `test_check` to compile: change `m.url = Some(dto.url)` to `m.url = dto.url;`** (dto.url is now `Option`). (Per-type validation + full test_check type-copy land in Task 2 — for now keep create working with the dto's type.)
- [ ] **Step 6: Run → PASS** (migrate2 + full `cargo test -p vigil`) + `cargo clippy --all-targets -- -D warnings`. Note: existing tests that constructed a Monitor literal may need the new fields — update them (or rely on `test_defaults_monitor`).
- [ ] **Step 7: Commit** `git commit -am "feat: version-ordered migration runner + 0002 (types, acknowledged, aggregates); model/DTO fields"`

---

## Task 2: `probe::run` dispatcher + `Cause::Keyword` + per-type validation

**Files:** Modify `crates/vigil/src/models.rs` (Cause), `crates/vigil/src/probe/mod.rs` (run), `crates/vigil/src/worker.rs` (call run), `crates/vigil/src/api/monitors.rs` (validate + test_check), `crates/vigil/src/engine.rs` (Cause match arm). Create a validation helper. Test: `tests/validate.rs`, inline.

**Interfaces:** `probe::run(&Monitor)->ProbeOutcome`; `validate_monitor_dto(...)`.

- [ ] **Step 1: Failing test** — `tests/validate.rs` (via the API): POST `{"name":"p","type":"port"}` (no host) → 422; `{"name":"d","type":"dns","host":"x"}` (no record type) → 422; `{"name":"h","type":"http"}` (no url) → 422; `{"name":"ok","type":"port","host":"h","port":80}` → 200.
- [ ] **Step 2: Run → FAIL.**
- [ ] **Step 3: Implement.** Add `Keyword` to `Cause` (lowercase serde). `probe/mod.rs`:
```rust
pub async fn run(m: &Monitor) -> ProbeOutcome {
    match m.r#type.as_str() {
        "http" | "keyword" => http::probe(m).await,
        "port" | "ping" => tcp::probe(m).await,
        "dns" => dns::probe(m).await,
        _ => http::probe(m).await, // default
    }
}
```
(`tcp`/`dns` modules are stubbed in later tasks — for THIS task, add `pub mod tcp; pub mod dns;` with a minimal `pub async fn probe(_m:&Monitor)->ProbeOutcome { ProbeOutcome{ok:false,response_time_ms:None,status_code:None,error_message:Some("not yet implemented".into()),resolved_ip:None,cause:Some(Cause::Connection)} }` placeholder so it compiles; Tasks 3-4 fill them.) `worker::run_check` calls `probe::run(&m)` instead of `probe::http::probe(&m)`. Add `validate_monitor_dto` (per spec §5) and call it in `create` + `update` (return 422 with the message).

**Two required compile fixes when adding `Cause::Keyword`:**
1. `crates/vigil/src/engine.rs` has an EXHAUSTIVE `match out.cause { Some(Cause::Timeout)=>"timeout", Some(Cause::Status)=>"status", Some(Cause::Connection)=>"connection", Some(Cause::Dns)=>"dns", None=>"connection" }` (~line 92) — adding the variant makes it non-exhaustive (E0004). Add an explicit arm `Some(Cause::Keyword) => "keyword",` (NOT a `_` wildcard — that would mislabel keyword incidents). `incidents.cause` has no CHECK, so "keyword" inserts fine.
2. Make **`test_check` work per type**: after building the fixture `Monitor` from the dto, also set `m.r#type = dto.r#type` and copy `host/port/keyword/keyword_mode/keyword_case_sensitive/dns_record_type/dns_expected_value` from the dto, and call `probe::run(&m).await` (not `probe::http::probe(&m)`). Add a test asserting a `type:"port"` test-check against a bound port returns `ok:true` (exercises the dispatcher through the API).
- [ ] **Step 4: Run → PASS** + clippy. **Step 5: Commit** `git commit -am "feat: probe::run dispatcher, Cause::Keyword, per-type monitor validation"`

---

## Task 3: TCP prober (port + ping fallback)

**Files:** Modify `crates/vigil/src/probe/tcp.rs`. Test: `tests/prober_tcp.rs`.

**Interfaces:** `tcp::probe(&Monitor)->ProbeOutcome` — `port` type: connect `host:port`; `ping` type: explicit port single attempt, null port 443→80 sequential.

- [ ] **Step 1: Failing tests** — `tests/prober_tcp.rs`:
```rust
use vigil::models::*;
fn m_port(host:&str, port:i64)->Monitor { let mut m=vigil::models::test_defaults_monitor();
    m.r#type="port".into(); m.host=Some(host.into()); m.port=Some(port); m.url=None; m }
#[tokio::test] async fn connects_to_open_port() {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let p = l.local_addr().unwrap().port() as i64;
    let o = vigil::probe::tcp::probe(&m_port("127.0.0.1", p)).await;
    assert!(o.ok);
}
#[tokio::test] async fn refused_port_is_down() {
    let o = vigil::probe::tcp::probe(&m_port("127.0.0.1", 1)).await;
    assert!(!o.ok); assert!(matches!(o.cause, Some(Cause::Connection)|Some(Cause::Timeout)));
}
#[tokio::test] async fn ping_explicit_port() {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let p = l.local_addr().unwrap().port() as i64;
    let mut m = m_port("127.0.0.1", p); m.r#type="ping".into();
    let o = vigil::probe::tcp::probe(&m).await; assert!(o.ok);
}
```
- [ ] **Step 2: Run → FAIL.** **Step 3: Implement** `tcp::probe`: resolve `host:port` target(s); for `ping` with null port, try 443 then 80; each attempt `tokio::time::timeout(Duration::from_secs(timeout_seconds), TcpStream::connect(addr))`. On connect → `ok=true, response_time_ms=elapsed`. On timeout → `Cause::Timeout`; on connect error → `Cause::Connection`. Fill `ProbeOutcome`. **Step 4: Run → PASS** + clippy. **Step 5: Commit** `git commit -am "feat: TCP port + ping prober"`

---

## Task 4: DNS prober (hickory-resolver)

**Files:** Modify `crates/vigil/src/probe/dns.rs`, `crates/vigil/Cargo.toml`. Test: `tests/prober_dns.rs`.

**Interfaces:** `dns::probe(&Monitor)->ProbeOutcome`. Add `hickory-resolver` dep (features for tokio + system config; NO openssl — it uses rustls/ring or none for plain DNS).

- [ ] **Step 1: Add dep** `hickory-resolver = "0.24"` (or current; tokio runtime). Verify it builds without openssl.
- [ ] **Step 2: Failing test** — resolve a name that always works locally. Prefer an **injectable** design so tests don't depend on network: `dns::probe_with(resolver_fn, m)` where the default `probe` uses a real resolver. For the test, resolve `localhost` A record (returns 127.0.0.1 on virtually all systems) and assert ok + resolved_ip contains "127.0.0.1". If `localhost` resolution is unreliable in the sandbox, make the resolver injectable and inject a fake returning one A record; assert the expected-value match logic. Prefer the injectable approach:
```rust
#[tokio::test] async fn dns_expected_value_match() {
    // inject a fake resolver returning A 93.184.216.34 for record type A
    let mut m = vigil::models::test_defaults_monitor(); m.r#type="dns".into(); m.host=Some("x".into());
    m.url=None; m.dns_record_type=Some("A".into()); m.dns_expected_value=Some("93.184".into());
    let o = vigil::probe::dns::probe_with(&m, |_h,_rt| Ok(vec!["93.184.216.34".to_string()])).await;
    assert!(o.ok); assert_eq!(o.resolved_ip.as_deref(), Some("93.184.216.34"));
    m.dns_expected_value=Some("10.0.0.1".into());
    let o2 = vigil::probe::dns::probe_with(&m, |_h,_rt| Ok(vec!["93.184.216.34".to_string()])).await;
    assert!(!o2.ok);  // expected value not found
}
```
- [ ] **Step 3: Run → FAIL.** **Step 4: Implement** `dns::probe_with(m, resolver_fn)` where `resolver_fn(host, record_type) -> Result<Vec<String>>` returns canonical string forms; the match logic (≥1 record AND, if expected set, case-insensitive substring); `resolved_ip`←first record for A/AAAA. `dns::probe(m)` wraps a real hickory resolver (`TokioAsyncResolver::tokio_from_system_conf()` or a default) mapping the record type + canonicalizing (A/AAAA→ip, CNAME/NS→trim trailing dot, MX→"pref host", TXT→concat). **Step 5: Run → PASS** + clippy. **Step 6: Commit** `git commit -am "feat: DNS prober (hickory) with expected-value match"`

---

## Task 5: Keyword mode in the HTTP prober (bounded body)

**Files:** Modify `crates/vigil/src/probe/http.rs`. Test: `tests/prober_keyword.rs`.

**Interfaces:** `http::probe` honors `m.r#type=="keyword"` — after the status check, read ≤2 MiB body, match `keyword`/`keyword_mode`/`keyword_case_sensitive`.

- [ ] **Step 1: Failing tests** — wiremock serving a body "hello WORLD":
  - present + found → ok; present + missing keyword → `!ok, cause=Keyword`; absent + present keyword → `!ok, cause=Keyword`; absent + missing → ok; case-insensitive default matches "world"; case-sensitive requires exact.
- [ ] **Step 2: Run → FAIL.** **Step 3: Implement** in `http::probe`: after computing the status-code success, if `m.r#type=="keyword"` and `m.keyword` is Some and status was ok: read the body with a **bounded chunk loop** (NOT `resp.bytes()`, which buffers the whole body → OOM risk): `let mut buf=Vec::new(); while let Some(ch)=resp.chunk().await? { buf.extend_from_slice(&ch); if buf.len() >= 2*1024*1024 { buf.truncate(2*1024*1024); break; } }`, then `String::from_utf8_lossy(&buf)`, apply case folding per flag, check `keyword_mode`; on mismatch set `ok=false, cause=Some(Cause::Keyword), error_message=Some("keyword <present|absent> check failed")`. Keep a shared `http::probe` used by both `http` and `keyword` types. **Step 4: Run → PASS** + clippy. **Step 5: Commit** `git commit -am "feat: keyword monitoring (bounded body match)"`

---

## Task 6: Daily rollups + catch-up + ensure_aggregates

**Files:** Create `crates/vigil/src/rollup.rs`; Modify `src/maintenance.rs` (call catch-up), `src/main.rs` (startup catch-up), `src/lib.rs`. Test: `tests/rollup.rs`.

**Interfaces:** `rollup::{rollup_day, rollup_catch_up, ensure_aggregates, day_str, day_bounds}`.

- [ ] **Step 1: Failing test** — `tests/rollup.rs`:
```rust
mod common; use common::*;
#[tokio::test] async fn rollup_day_aggregates_checks_and_overlap_incident() {
    let (pool,_d)=fresh_pool().await;
    // monitor id 1
    sqlx::query("INSERT INTO monitors (name,type,url,expected_status_codes,interval_seconds,timeout_seconds,confirmation_threshold,recovery_threshold,retry_interval_seconds,status,created_at,updated_at) VALUES ('m','http','https://x','200-299',300,30,3,1,30,'up',0,0)").execute(&pool).await.unwrap();
    // day 2000-01-01 UTC bounds:
    let (ds, de) = vigil::rollup::day_bounds("2000-01-01");
    // 3 up checks + 1 down check within the day
    for t in [ds+10, ds+20, ds+30] { sqlx::query("INSERT INTO checks (monitor_id,checked_at,status,response_time_ms) VALUES (1,?,'up',100)").bind(t).execute(&pool).await.unwrap(); }
    sqlx::query("INSERT INTO checks (monitor_id,checked_at,status,response_time_ms) VALUES (1,?,'down',null)").bind(ds+40).execute(&pool).await.unwrap();
    // an incident spanning from the previous day into this day (overlap): started ds-100, resolved ds+200 => 200s in-day
    sqlx::query("INSERT INTO incidents (monitor_id,started_at,resolved_at,duration_seconds,cause) VALUES (1,?,?,?, 'status')").bind(ds-100).bind(ds+200).bind(300).execute(&pool).await.unwrap();
    vigil::rollup::rollup_day(&pool, "2000-01-01").await.unwrap();
    let (up,down,samp): (i64,i64,i64) = sqlx::query_as("SELECT up_count,down_count,sample_count FROM check_aggregates_daily WHERE monitor_id=1 AND day='2000-01-01'").fetch_one(&pool).await.unwrap();
    assert_eq!((up,down,samp),(3,1,4));
    let down_secs: f64 = sqlx::query_scalar("SELECT (100.0 - uptime_pct)/100.0*86400 FROM check_aggregates_daily WHERE monitor_id=1 AND day='2000-01-01'").fetch_one(&pool).await.unwrap();
    assert!((down_secs-200.0).abs() < 10.0, "overlap incident contributes ~200s downtime (uptime_pct is 2-dp rounded, ~8.6s granularity), got {down_secs}");
}
```
- [ ] **Step 2: Run → FAIL.** **Step 3: Implement.** Add the `chrono` dep (per Shared Types: `default-features=false, features=["std"]`, no openssl). `day_bounds("YYYY-MM-DD")->(i64,i64)`: `NaiveDate::parse_from_str(day,"%Y-%m-%d")?.and_hms_opt(0,0,0)?.and_utc().timestamp()` for start, `start+86400` for end; `day_str(epoch)`: `DateTime::<Utc>::from_timestamp(epoch,0)?.format("%Y-%m-%d").to_string()`. `rollup_day`: for each monitor with a check in `[ds,de)`, compute up/down/sample counts, avg/min/max response, incident_count (started in-day), and uptime_pct via `uptime::compute` fed the overlap incident spans (`started_at < de AND (resolved_at IS NULL OR resolved_at > ds)`) clipped to `[ds,de]` with `now=de` (completed day). Upsert. `rollup_catch_up`: from each monitor's last aggregate day (or oldest retained check) through yesterday, bounded by retention, call `rollup_day`. `ensure_aggregates`: bounded catch-up for one monitor (completed days since its last aggregate, within retention). Wire `rollup_catch_up` into the nightly maintenance loop (before prune) and a one-shot at startup (`main::serve`). **Step 4: Run → PASS** + clippy. **Step 5: Commit** `git commit -am "feat: daily rollups (overlap uptime), catch-up, ensure_aggregates"`

---

## Task 7: Stats 30d/90d + series + bars endpoints

**Files:** Modify `crates/vigil/src/api/monitors.rs`, `crates/vigil/src/api/mod.rs` (register routes). Test: `tests/api_signal.rs`.

**Interfaces:** stats accepts 30d/90d (count-weighted avg from aggregates); `GET /series`; `GET /bars`. **Register the new routes in `api::routes()` in `api/mod.rs`** (next to the existing stats route): `.route("/monitors/:id/series", get(monitors::series))` and `.route("/monitors/:id/bars", get(monitors::bars))` — without this both 404 and the tests fail.

- [ ] **Step 1: Failing tests** — create a monitor, insert checks + an incident; assert: `/stats?range=30d` returns a numeric `avg_ms`; `/series?range=24h` returns an array of `{t,ms,status}` bucketed ≤300; `/bars?days=90` returns 90 (or ≤90) day rows with `has_data` true for days with a check/incident.
- [ ] **Step 2: Run → FAIL.** **Step 3: Implement.** Extend the stats range parser (24h/7d/30d/90d). For 30d/90d avg: `ensure_aggregates`, then `SELECT SUM(avg_response_ms*sample_count)/SUM(sample_count) FROM check_aggregates_daily WHERE monitor_id=? AND day>=? AND avg_response_ms IS NOT NULL` (+ blend today's raw-check avg, count-weighted). uptime/downtime from incidents (extend the P1 window logic to 30d/90d). `series`: SELECT checks in the window, bucket into ≤300 equal time-slots, per slot emit avg(ms)+worst status; return JSON. `bars`: `ensure_aggregates`; for each of the last `days` UTC days compute `uptime_pct`/`down_seconds` from incidents (overlap, clip end=min(day_end, now)), `incidents`=started-that-day count, `has_data`= aggregate exists OR (within retention AND a check exists) OR an incident overlaps. Return the array oldest→newest. **Step 4: Run → PASS** + clippy. **Step 5: Commit** `git commit -am "feat: stats 30d/90d, series (bucketed), bars endpoints"`

---

## Task 8: Incidents list + acknowledge endpoints

**Files:** Create `crates/vigil/src/api/incidents.rs`; Modify `src/api/mod.rs` (add `pub mod incidents;` there — NOT lib.rs; it's an `/api` submodule — and register its routes in `routes()`). Test: `tests/api_incidents.rs`.

**Interfaces:** `GET /api/incidents?monitor_id=&range=`; `POST /api/incidents/:id/acknowledge`.

- [ ] **Step 1: Failing test** — open an incident (insert), GET `/api/incidents` returns it with `acknowledged:false` + `monitor_name`; POST `/api/incidents/:id/acknowledge` → 200; GET again shows `acknowledged:true`.
- [ ] **Step 2: Run → FAIL.** **Step 3: Implement.** `list`: `SELECT i.*, m.name AS monitor_name FROM incidents i JOIN monitors m ON m.id=i.monitor_id WHERE (?1 IS NULL OR i.monitor_id=?1) AND i.started_at >= ? ORDER BY i.started_at DESC` (range default 30d). `acknowledge`: `UPDATE incidents SET acknowledged=1 WHERE id=?`. Register routes in `api/mod.rs`. **Step 4: Run → PASS** + clippy. **Step 5: Commit** `git commit -am "feat: incidents list + acknowledge API"`

---

## Task 9: Frontend — 90-day UptimeBar (cards + panel)

**Files:** Create `web/src/components/UptimeBar.tsx`; Modify `web/src/api.ts` (getBars), `MonitorCard.tsx`, `DetailPanel.tsx`. Test: `web/src/__tests__/uptimebar.test.tsx`.

**Interfaces:** `UptimeBar` props `{ monitorId, days, compact? }` fetches `/bars`, renders segments with color bands + hover tooltip.

- [ ] **Step 1: Failing test** — render `UptimeBar` with stubbed fetch returning bars `[{day,uptime_pct:100,has_data:true,...},{uptime_pct:40,has_data:true,...},{has_data:false,...}]`; assert 3 segments render and their classes/colors reflect up / down / no-data (e.g. `data-tier` attr = "up"/"down"/"nodata").
- [ ] **Step 2: Run → FAIL.** **Step 3: Implement.** `getBars(id,days)` in api.ts. `UptimeBar`: fetch bars; render a flex row of thin segments; color band per spec §8 (100→up, 50–99.99→degraded amber, <50→down, !has_data→border); a `data-tier` attr for testability; hover title tooltip (date · uptime% · incidents · downtime). `compact` → ~45 segs, else 90 with end labels ("90 days ago"/"Today") + a faint legend. Use it on the card (compact, replacing the placeholder strip) and the detail panel (full). **Step 4: Run → PASS** + `npm run build` + `tsc`. **Step 5: Commit** `git commit -am "feat(web): 90-day uptime bar with color bands"`

---

## Task 10: Frontend — ResponseChart (uPlot) + incident shading

**Files:** Create `web/src/components/ResponseChart.tsx`; Modify `web/package.json` (uplot), `api.ts` (getSeries, getIncidents), `DetailPanel.tsx`. Test: `web/src/__tests__/responsechart.test.tsx`.

- [ ] **Step 1: Add `uplot` dep** (`npm i uplot`). Import its CSS.
- [ ] **Step 2: Failing test** — render `ResponseChart` with stubbed getSeries returning `[]` (empty) and assert it mounts without throwing (renders an empty-state, no crash). (uPlot in jsdom may need a guard — render a placeholder when the container has no size / no data.)
- [ ] **Step 3: Run → FAIL.** **Step 4: Implement.** `getSeries(id,range)` and `getIncidents(range?, monitorId?)` in api.ts (omit the `monitor_id` query param when `monitorId` is undefined, so Task 12's global Incidents screen reuses it for all monitors). `ResponseChart`: a range selector (24h/7d); on data, mount uPlot in an effect (guard for jsdom/no-size: only init uPlot if `clientWidth>0` and data present, else render an empty-state div); plot ms over time; shade incident spans (from getIncidents, clipped to range, open→now) as background rects/bands. Respect reduced-motion. Place in the detail panel. **Step 5: Run → PASS** + build + tsc. **Step 6: Commit** `git commit -am "feat(web): response-time chart (uPlot) with incident shading"`

---

## Task 11: Frontend — IncidentTimeline + panel tiles 24h/7d/30d/90d

**Files:** Create `web/src/components/IncidentTimeline.tsx`; Modify `DetailPanel.tsx`, `api.ts` (acknowledgeIncident). Test: `web/src/__tests__/timeline.test.tsx`.

- [ ] **Step 1: Failing test** — render `IncidentTimeline` with stubbed getIncidents returning one ongoing (resolved_at:null) + one resolved incident; assert both render, the ongoing shows an Acknowledge button, clicking it calls `acknowledgeIncident`.
- [ ] **Step 2: Run → FAIL.** **Step 3: Implement.** `acknowledgeIncident(id)` in api.ts. `IncidentTimeline` props `{ monitorId, dayFilter? }`: fetch `/api/incidents?monitor_id`, reverse-chron list — cause icon, started (relative), duration (live tick if ongoing), resolved, status/error, Acknowledge button (ongoing/!acknowledged). Add it to the detail panel. **Extend the panel's uptime tiles to 24h/7d/30d/90d** (add 30d + 90d tiles wired to getStats). **First widen `StatsRange` in `web/src/api.ts`** from `"24h"|"7d"` to `"24h"|"7d"|"30d"|"90d"` (else `tsc` fails), and confirm the `Stats` type exposes `downtime_seconds` for the per-tile downtime line. **Step 4: Run → PASS** + build + tsc. **Step 5: Commit** `git commit -am "feat(web): incident timeline + acknowledge + 30d/90d tiles"`

---

## Task 12: Frontend — global Incidents screen + Rail nav

**Files:** Create `web/src/components/Incidents.tsx`; Modify `App.tsx`, `Rail.tsx`. Test: `web/src/__tests__/incidents_screen.test.tsx`.

- [ ] **Step 1: Failing test** — render `Incidents` with stubbed getIncidents returning 2 incidents (1 open); assert both render and a header stat shows open count = 1.
- [ ] **Step 2: Run → FAIL.** **Step 3: Implement.** `Incidents` screen: global list via getIncidents (range filter), header stats (open incidents = resolved_at null count; MTTR = avg duration of resolved; 30d count), acknowledge inline. Add an "Incidents" nav item to the Rail + a `view` value in App ("dashboard"|"settings"|"incidents"). **Step 4: Run → PASS** + build + tsc. **Step 5: Commit** `git commit -am "feat(web): global incidents screen + rail nav"`

---

## Task 13: Frontend — ListView + grid⇄list toggle + sort

**Files:** Create `web/src/components/ListView.tsx`; Modify `App.tsx`, `TopBar.tsx`, `store.ts`. Test: `web/src/__tests__/listview.test.tsx`.

- [ ] **Step 1: Failing test** — render `ListView` with 3 monitors; assert a table with rows; clicking the Name header sorts (assert order changes). Default sort = sort_order then name.
- [ ] **Step 2: Run → FAIL.** **Step 3: Implement.** `ListView`: dense table `● | Name | Type | Last check | Response | 24h | 7d | 30d | ▍bar | ⋯` (reuse UptimeBar compact + per-row stats). Sortable headers (client-side); default `sort_order` then name; active sort persists in the store (a signal). Top-bar grid⇄list toggle (a `view` signal in App/store, persisted in-memory). Row click → detail panel; `⋯` quick actions (reuse card's). **Step 4: Run → PASS** + build + tsc. **Step 5: Commit** `git commit -am "feat(web): dense list view with sort + grid/list toggle"`

---

## Task 14: Frontend — MonitorForm type selector + type-specific fields

**Files:** Modify `web/src/components/MonitorForm.tsx`. Test: extend `web/src/__tests__/form.test.tsx`.

- [ ] **Step 1: Failing test** — select type "port" in the form; assert host + port fields appear (and url hides); filling them + save calls createMonitor with `{type:"port", host, port}`.
- [ ] **Step 2: Run → FAIL.** **Step 3: Implement.** Add a type selector (http/keyword/port/ping/dns). Conditionally render fields: url (http/keyword); keyword+mode(select)+case(checkbox) (keyword); host+port (port); host+optional port (ping); host+record-type(select)+expected (dns). `buildDto` includes `type` + the relevant fields (omit irrelevant ones / send null). Test-check sends the full dto (works per type via the backend). Keep the existing http path working. **Step 4: Run → PASS** + build + tsc. **Step 5: Commit** `git commit -am "feat(web): monitor form type selector + type-specific fields"`

---

## Task 15: P2 acceptance + final review

**Files:** Create `docs/superpowers/plans/P2-acceptance.md`. No product code unless a DoD item fails.

- [ ] **Step 1** — Migration: point the binary at a **copy of a P1-populated DB** (or a fresh one) and confirm `0002` applies (`SELECT MAX(version) FROM schema_migrations` = 2), no data lost.
- [ ] **Step 2** — Create one monitor of EACH type (http, keyword against a local body, port against a bound port, ping against a bound port, dns) via the API; confirm each probes and reports a sane status. For the **dns** monitor use a reliably-resolvable public name (e.g. `one.one.one.one` A) rather than `localhost` (localhost resolution can be non-deterministic in the container); accept either up/down for dns as long as it doesn't error.
- [ ] **Step 3** — Backdate some checks + an incident; run a rollup (or wait/trigger); confirm `/bars` shows graded days, `/series` returns points, `/stats?range=30d` returns avg_ms, `/api/incidents` lists it, acknowledge flips the flag.
- [ ] **Step 4** — Docker rebuild + boot on host 8099 → **healthy**; the dashboard shows bars + list view toggle.
- [ ] **Step 5: Commit** the acceptance checklist.

Then: final whole-branch review (opus), fix any blocker/important, merge to master.

---

## Definition of Done

All §1 spec items verified; `cargo test` + `vitest` green; `0002` applies on a P1 DB; Docker healthy; every task committed.
