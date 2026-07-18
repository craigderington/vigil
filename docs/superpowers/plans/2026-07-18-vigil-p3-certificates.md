# Vigil P3 (Certificates) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: superpowers:subagent-driven-development. Steps use `- [ ]`.
> **Autonomous build.** Builds on P1+P2 (on `master`). Base: `666ee16`. Branch `feat/p3-certificates`.

**Goal:** Make cert/domain expiry surprises impossible — SSL certificate tracking + tiered alerts, domain-registration expiry (RDAP/WHOIS), and webhook/Discord/ntfy notification channels (plus an optional SMTP username).

**Architecture:** Extends the P1/P2 Rust/axum + SolidJS app. A `certcheck` module (SNI-correct rustls capture + x509 parse; RDAP/WHOIS over eTLD+1); a `cert_scheduler` slow-cadence task (anchor-gated); a refactored **multi-channel-type** notify core (`deliver()` routing by channel type, per-`(monitor,channel,trigger)` cooldown); migration `0003`; SSL/domain UI cards.

**Tech stack additions (ALL ring-only — no openssl, no aws-lc):** `tokio-rustls` (pinned ring), `x509-parser`, `webpki-roots`. Reuse transitive `rustls-webpki 0.103`. No `regex`.

Spec: [`docs/superpowers/specs/2026-07-18-vigil-p3-certificates-design.md`](../specs/2026-07-18-vigil-p3-certificates-design.md).

## Global Constraints

- Inherit ALL P1/P2 constraints (rustls only — **no openssl, no aws-lc-rs**; i64 epoch seconds; SQLite FK/WAL/auto_vacuum; uptime from incidents; non-root container; app bind 8090 / host 8099; SMTP password only from Docker secret; version-ordered migration runner).
- **`tokio-rustls` MUST be `{ version="0.26", default-features=false, features=["ring","tls12"] }`** and the TLS `ClientConfig` built with an explicit provider `rustls::crypto::ring::default_provider()` — a bare `cargo add tokio-rustls` enables `aws-lc-rs`, giving rustls two providers → runtime panic + a C/cmake dep. **After each cert task, assert `cargo tree -p vigil | grep -iE 'aws-lc|openssl'` is empty.**
- **Cooldown key is `(monitor_id, channel_id, trigger)`** — never `(monitor_id, trigger)` (that limits every trigger to one channel).
- **Tiered alerts fire most-urgent-first:** the smallest crossed-but-unalerted threshold; `alerted_days` tracks it; renewal (days jump above alerted_days) resets to NULL. `ssl_invalid` is a SEPARATE `invalid_alerted` boolean, reset on invalid→valid.
- `Cause` gains `Ssl`; **`engine.rs`'s exhaustive cause match gains `Some(Cause::Ssl) => "ssl"`** (else compile error).
- Cert alerts are **anchor-gated** (skip when `anchor.current()==Offline`). A transport/handshake error is NOT `ssl_invalid` (only a captured-but-invalid cert is).
- New `Trigger` variants: `SslExpiring, SslInvalid, DomainExpiring`. New channel types: `webhook, discord, ntfy`.
- Commit after every task. TDD. Conventional commits.

---

## Shared Types & Interfaces (the DRY backbone)

```rust
// models.rs — Cause gains Ssl
pub enum Cause { Timeout, Status, Connection, Dns, Keyword, Ssl }  // serde lowercase → "ssl"

// models.rs — Trigger gains 3
pub enum Trigger { Down, Recovered, SslExpiring, SslInvalid, DomainExpiring }
// as_str: down|recovered|ssl_expiring|ssl_invalid|domain_expiring

// models.rs — Monitor gains 4 fields (+FromRow, +DTOs, all optional):
//   ssl_check_enabled: bool, ssl_alert_days: String (JSON, default "[30,14,7,3,1]"),
//   domain_check_enabled: bool, domain_alert_days: String (JSON, default "[45,30,14,7]")
// test_defaults_monitor(): ssl/domain disabled, default alert-day strings.

// models.rs — new rows (+FromRow):
pub struct SslCert { pub monitor_id:i64, pub issuer:Option<String>, pub subject:Option<String>,
  pub valid_from:Option<i64>, pub valid_until:Option<i64>, pub days_remaining:Option<i64>,
  pub is_valid:Option<bool>, pub chain_ok:Option<bool>, pub hostname_match:Option<bool>,
  pub self_signed:Option<bool>, pub error:Option<String>, pub alerted_days:Option<i64>,
  pub invalid_alerted:bool, pub last_checked:Option<i64> }
pub struct DomainInfo { pub monitor_id:i64, pub registrar:Option<String>, pub expiry_date:Option<i64>,
  pub days_remaining:Option<i64>, pub name_servers:Option<String>, pub status_codes:Option<String>,
  pub queryable:Option<bool>, pub source:Option<String>, pub alerted_days:Option<i64>, pub last_checked:Option<i64> }

// certcheck/ssl.rs
pub struct SslResult { pub issuer:Option<String>, pub subject:Option<String>, pub valid_from:Option<i64>,
  pub valid_until:Option<i64>, pub days_remaining:Option<i64>, pub is_valid:bool, pub chain_ok:bool,
  pub hostname_match:bool, pub self_signed:bool, pub error:Option<String> }
pub async fn check(host:&str, port:u16, timeout_secs:u64) -> SslResult;   // SNI = host
pub fn hostname_matches(sans:&[String], cn:Option<&str>, host:&str) -> bool;  // RFC6125, pure

// certcheck/domain.rs
pub struct DomainResult { pub registrar:Option<String>, pub expiry_date:Option<i64>,
  pub name_servers:Vec<String>, pub status_codes:Vec<String>, pub queryable:bool, pub source:Option<String>,
  pub transient:bool /* true => leave last-known, do not persist queryable=0 */ }
pub fn registrable_domain(host:&str) -> String;   // eTLD+1, pure
pub fn parse_rdap(json:&serde_json::Value) -> Option<DomainResult>;  // pure
pub async fn check(host:&str, timeout_secs:u64) -> DomainResult;

// certcheck/alerts.rs — pure fire-once
pub struct TierDecision { pub fire:bool, pub new_alerted_days:Option<i64> }
pub fn tier(days_remaining:i64, alert_days:&[i64], alerted_days:Option<i64>) -> TierDecision; // most-urgent-first
// invalid: fire iff is_valid==false && !invalid_alerted; reset invalid_alerted when is_valid flips false->true.

// notify — the refactor
pub struct NotifyMsg { pub subject:String, pub body:String, pub subject_html:Option<String> }
pub struct AlertCtx { pub monitor_name:String, pub target:String,
  pub ssl_days:Option<i64>, pub ssl_valid_until:Option<i64>, pub ssl_issuer:Option<String>,
  pub domain_days:Option<i64>, pub domain_expiry:Option<i64>, pub registrar:Option<String> }
#[async_trait] pub trait HttpSender: Send+Sync {  // webhook/discord/ntfy
  async fn send(&self, channel_type:&str, config:&serde_json::Value, msg:&NotifyMsg) -> anyhow::Result<()>;
}
// notify::deliver(state,&Monitor, trigger:Trigger, msg:&NotifyMsg, incident_id:Option<i64>) -> Result<()>
//   loops attached active channels whose triggers include trigger; cooldown per (monitor,channel_id,trigger);
//   routes by channel.type: "email"=>state.transport (existing), else=>state.http_sender; logs notification_log.
// notify::on_transition(...) renders Down/Recovered NotifyMsg -> deliver.
// notify::send_alert(state,&Monitor, trigger, &AlertCtx) renders ssl/domain NotifyMsg -> deliver.

// app.rs — AppState gains: pub http_sender: Arc<dyn crate::notify::HttpSender>
```

**Cert-scheduler settings** (settings_store keys, defaults): `cert.ssl_interval_seconds`=43200, `cert.domain_interval_seconds`=86400, `cert.tick_seconds`=60, `cert.concurrency`=5.

**Endpoint shapes:** `/ssl` → SslCert|null; `/domain` → DomainInfo|null; refresh endpoints return the persisted row.

**Test helpers** (extend tests/common): `test_state` gains an `http_sender: RecordingHttpSender` (records `(channel_type, config, msg)`); expose `sent_http` alongside `sent` (email). `RecordingHttpSender` is a P3 addition.

---

## Task 1: Migration 0003 + models + Cause::Ssl + validation

**Files:** Create `crates/vigil/migrations/0003_certs.sql`; Modify `crates/vigil/src/db.rs` (MIGRATIONS), `src/models.rs`, `src/engine.rs` (Cause arm), `src/api/monitors.rs` (validation). Test: `tests/migrate3.rs`.

**Interfaces:** the §4 schema; `Cause::Ssl`; `Monitor`/DTO add-on fields; `SslCert`/`DomainInfo` structs + FromRow.

- [ ] **Step 1: `0003_certs.sql`** — verbatim from spec §4 (4 monitor ALTERs, `ssl_certs`, `domain_info`).
- [ ] **Step 2: Failing test** — `tests/migrate3.rs`: connect a fresh DB → `MAX(version)=3`; `SELECT ssl_check_enabled, ssl_alert_days FROM monitors`, `SELECT * FROM ssl_certs`, `SELECT * FROM domain_info` all succeed. Plus an on-a-v2-DB upgrade test (apply 0001+0002+record versions 1,2 + insert a monitor, connect → only 0003 applies, monitor preserved).
- [ ] **Step 3: Run → FAIL.** `cargo test -p vigil --test migrate3`.
- [ ] **Step 4: Implement.** Append `(3, include_str!("../migrations/0003_certs.sql"))` to `MIGRATIONS` in db.rs. `models.rs`: add 4 `Monitor` fields + FromRow (`ssl_check_enabled`/`domain_check_enabled` i64→bool), extend `test_defaults_monitor`; add `Cause::Ssl` (serde lowercase); add `SslCert`+`DomainInfo` structs + manual FromRow; `CreateMonitorDto`/`UpdateMonitorDto` gain the 4 optional fields; `create()`/`update()` INSERT/UPDATE them. **`engine.rs`: add `Some(Cause::Ssl) => "ssl"` to the cause→str match.** `validate_monitor_dto`: enabling `ssl_check_enabled` on http/keyword requires `url` starting `https://`; `ssl` type requires `host` (422 otherwise).
- [ ] **Step 5: Run → PASS** + `cargo test -p vigil` + `cargo clippy --all-targets -- -D warnings` (fix any Monitor/DTO construction sites). **Step 6: Commit** `git commit -am "feat: migration 0003 (cert add-ons + ssl_certs/domain_info), Cause::Ssl, validation"`

---

## Task 2: Notify refactor — deliver() core, HttpSender, senders, Trigger variants, SMTP username

**Files:** Modify `crates/vigil/src/notify/{mod,dispatch,email,templates}.rs`, `src/models.rs` (Trigger), `src/app.rs` (AppState), `src/api/channels.rs` (test routing), `src/main.rs` (construct http_sender), `tests/common/mod.rs` (RecordingHttpSender). Create `src/notify/http.rs` (webhook/discord/ntfy). Test: `tests/notify_multi.rs`.

**Interfaces:** `Trigger`+3; `NotifyMsg`; `AlertCtx`; `HttpSender`+`RecordingHttpSender`; `notify::{deliver, on_transition, send_alert}`; AppState.http_sender.

- [ ] **Step 1: Failing tests** — `tests/notify_multi.rs`:
  - `two_channels_on_down_both_fire`: seed a monitor + an email channel + a webhook channel, both with trigger `down`; call `notify::on_transition(down)`; assert the email double recorded 1 send AND the http double recorded 1 webhook send (proves per-`(monitor,channel,trigger)` cooldown, not the old single-channel bug).
  - `webhook_payload_shape`: a webhook channel test → the recorded body is JSON with the monitor name.
  - `smtp_username_used_when_set`: an email channel config with `username:"apikey"` → the SmtpTransport builds credentials with username `apikey` (assert via a Transport double capturing the SmtpConfig — extend SmtpConfig/RecordingTransport to carry username, OR unit-test the credential-selection helper `auth_user(cfg) = cfg.username.clone().unwrap_or(cfg.from.clone())`).
- [ ] **Step 2: Run → FAIL.**
- [ ] **Step 3: Implement.**
  - `models.rs`: add `Trigger::{SslExpiring, SslInvalid, DomainExpiring}` (+ `as_str`).
  - `notify/mod.rs`: add `NotifyMsg`, `AlertCtx`, `#[async_trait] trait HttpSender`, `RecordingHttpSender` (Arc<Mutex<Vec<(String, Value, NotifyMsg)>>>). Add `username: Option<String>` to `SmtpConfig` and a helper `auth_user(&SmtpConfig)->String = username.clone().unwrap_or(from.clone())`. `SmtpTransport::send` uses `Credentials::new(auth_user(cfg), password)`.
  - `notify/http.rs`: `struct ReqwestHttpSender` impl `HttpSender` — match channel_type: `webhook` (POST config.url, method default POST, headers, JSON body from config.template with {{vars}} substituted+escaped, default template per spec §6), `discord` (POST config.webhook_url `{content,embeds:[{title,description,color}]}`), `ntfy` (POST `{server||https://ntfy.sh}/{topic}` body=msg.body, headers Title/Priority/Tags, Authorization Bearer if token). Uses a shared reqwest client (rustls). Errors → anyhow.
  - `notify/dispatch.rs`: **extract `deliver(state, m, trigger, msg, incident_id)`** — query attached active channels (JOIN monitor_notifications) whose `triggers` JSON includes `trigger.as_str()` — **DROP the `AND nc.type='email'` filter**; per channel, cooldown `SELECT MAX(sent_at) ... WHERE monitor_id=? AND channel_id=? AND trigger=?`; if allowed, `match ch.type { "email" => build SmtpConfig+EmailMsg from config, state.transport.send; _ => state.http_sender.send(ch.type, config, msg) }`; insert notification_log. `on_transition` renders the down/recovered NotifyMsg via templates and calls `deliver`. Add `send_alert(state, m, trigger, ctx)` rendering the ssl/domain NotifyMsg and calling `deliver`.
  - `templates.rs`: `render(trigger, ctx)` handles all 5 triggers (ssl/domain subjects/bodies use AlertCtx fields; add the §6 variables). (Two ctx types OR a merged ctx — keep it simple with an enum or two render fns; the tests pin down/recovered + one ssl body.)
  - `app.rs`: `AppState` gains `http_sender: Arc<dyn HttpSender>`. `main::serve`: `Arc::new(ReqwestHttpSender::new())`. `tests/common`: `test_state` builds a `RecordingHttpSender` and exposes `sent_http`.
  - `api/channels.rs::test`: route by channel.type (email → transport, else → http_sender) building a sample NotifyMsg.
- [ ] **Step 4: Run → PASS** (multi-channel + payload + username) + full suite + clippy + **no aws-lc/openssl** (no new tls dep yet, but confirm). **Step 5: Commit** `git commit -am "feat: notify refactor — multi-channel deliver(), HttpSender (webhook/discord/ntfy), per-channel cooldown, SMTP username, ssl/domain triggers"`

---

## Task 3: SSL cert check (rustls capture, SNI, x509, RFC6125, chain)

**Files:** Create `crates/vigil/src/certcheck/{mod,ssl}.rs`; Modify `src/lib.rs`, `crates/vigil/Cargo.toml`. Test: `tests/certcheck_ssl.rs`.

**Interfaces:** `ssl::check(host,port,timeout)->SslResult`; `ssl::hostname_matches(...)` (pure, RFC6125).

- [ ] **Step 1: Add deps** — `tokio-rustls = { version="0.26", default-features=false, features=["ring","tls12"] }`, `x509-parser="0.16"`, `webpki-roots="1"`. `cargo build` then `cargo tree -p vigil | grep -iE 'aws-lc|openssl'` MUST be empty (if aws-lc appears, a feature leaked — fix the pin).
- [ ] **Step 2: Failing tests** — `tests/certcheck_ssl.rs`:
  - `hostname_matches` RFC6125 vectors (pure): `["example.com"] vs "example.com"`→true; `["*.example.com"] vs "a.example.com"`→true, `vs "example.com"`→false, `vs "a.b.example.com"`→false; case-insensitive; CN fallback only when SANs empty.
  - integration `local_self_signed_cert`: start a `tokio-rustls` TLS server on 127.0.0.1:0 with a generated self-signed cert (use `rcgen` as a **dev-dependency** to mint the cert), then `ssl::check("127.0.0.1", port, 5)` → `self_signed==true`, `chain_ok==false` (not in Mozilla roots), a parsed `valid_until` in the future, `error==None` (handshake completed via the capturing verifier).
  - (optional, network-gated) a live `one.one.one.one:443` smoke asserting `is_valid && days_remaining>0` — guard so CI without network still passes (skip/log if it errors).
- [ ] **Step 3: Run → FAIL.**
- [ ] **Step 4: Implement.** `certcheck/mod.rs` (`pub mod ssl;` + `pub mod domain;` later; add `pub mod certcheck;` to lib.rs). `ssl.rs`:
  - A `Capturing` struct impl `rustls::client::danger::ServerCertVerifier` with **all four** methods: `verify_server_cert` stores `end_entity`+`intermediates` (clone the DER) into an `Arc<Mutex<Option<Vec<CertificateDer>>>>` and returns `Ok(ServerCertVerified::assertion())`; `verify_tls12_signature`/`verify_tls13_signature` delegate to `rustls::crypto::{verify_tls12_signature, verify_tls13_signature}(message, cert, dss, &ring_provider.signature_verification_algorithms)`; `supported_verify_schemes()` returns `ring_provider.signature_verification_algorithms.supported_schemes()`.
  - Build `ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider())).with_safe_default_protocol_versions()?.dangerous().with_custom_certificate_verifier(Arc::new(capturing)).with_no_client_auth()`.
  - `TcpStream::connect((host,port))` (timeout-wrapped), `tokio_rustls::TlsConnector::from(config).connect(ServerName::try_from(host)?, tcp)` — **ServerName = host (SNI)**; for a bare IP use `ServerName::IpAddress`.
  - After handshake, take the captured chain; parse `chain[0]` with `x509_parser::parse_x509_certificate`: issuer/subject (`.to_string()`), `validity().not_before/not_after` → epoch, SAN dNSNames. `days_remaining`. `hostname_matches(sans, cn, host)`. `chain_ok`: build a `WebPkiServerVerifier` over `webpki_roots::TLS_SERVER_ROOTS` and verify the chain at `now` (Ok→true). `self_signed`: issuer==subject or len==1. `is_valid = now∈[from,until] && chain_ok && hostname_match`.
  - Any connect/handshake/parse error → `SslResult{ error:Some(e.to_string()), is_valid:false, .. }`.
- [ ] **Step 5: Run → PASS** + suite + clippy + **no aws-lc/openssl**. **Step 6: Commit** `git commit -am "feat: SSL cert check (rustls capture, SNI, x509 parse, RFC6125 hostname, chain verify)"`

---

## Task 4: Domain check (eTLD+1, RDAP, WHOIS fallback)

**Files:** Create `crates/vigil/src/certcheck/domain.rs`; Modify `certcheck/mod.rs`. Test: `tests/certcheck_domain.rs`.

**Interfaces:** `domain::{check, registrable_domain, parse_rdap}`.

- [ ] **Step 1: Failing tests:**
  - `registrable_domain` (pure): `"api.myapp.com"`→`"myapp.com"`, `"www.example.com"`→`"example.com"`, `"x.example.co.uk"`→`"example.co.uk"`, `"example.com"`→`"example.com"`.
  - `parse_rdap` (pure, fixture): a captured RDAP JSON (an `events` entry `eventAction:"expiration", eventDate:"2027-03-01T00:00:00Z"`, an `entities` registrar, `nameservers`) → `expiry_date` epoch matches, `registrar` set, `queryable==true`.
  - (network-gated smoke) `check("one.one.one.one", 10)` → doesn't panic; `transient` or `queryable` set sanely (tolerant).
- [ ] **Step 2: Run → FAIL.**
- [ ] **Step 3: Implement.** `registrable_domain`: split labels; a hardcoded `MULTI_SUFFIX` set (`co.uk, org.uk, gov.uk, com.au, net.au, org.au, co.jp, co.nz, com.br, co.in, ...`) — if the last two labels form a multi-suffix, keep three, else two. `parse_rdap(&Value)`: dig `events[].eventAction=="expiration"` → `eventDate` (chrono RFC3339 → epoch); registrar from `entities` (role contains "registrar", vcardArray `fn`); nameservers `nameservers[].ldhName`; status `status[]`. `check`: `let d = registrable_domain(host)`; reqwest GET `https://rdap.org/domain/{d}` (Accept rdap+json, redirects, timeout): 200 → parse_rdap (if it has an expiry → return queryable); 404 → WHOIS fallback; 429/5xx/timeout/net-err → `DomainResult{ transient:true, .. }`. WHOIS: TCP `whois.iana.org:43` send `{d}\r\n`, read, find `refer:`/`whois:` server; connect it, send `{d}`, read; label-pinned expiry parse (priority: `Registry Expiry Date:`, `Registrar Registration Expiration Date:`, `Expiry Date:`, `Expiration Date:`, `paid-till:`) + multi-format date parse. If no expiry → `queryable:false`.
- [ ] **Step 4: Run → PASS** + suite + clippy. **Step 5: Commit** `git commit -am "feat: domain expiry check (eTLD+1, RDAP, WHOIS fallback)"`

---

## Task 5: Tiered alert logic (pure, fire-once, most-urgent-first)

**Files:** Create `crates/vigil/src/certcheck/alerts.rs`; Modify `certcheck/mod.rs`. Tests inline.

**Interfaces:** `alerts::tier(days_remaining, alert_days, alerted_days) -> TierDecision`.

- [ ] **Step 1: Failing tests:**
  - fresh cert 40 days, `[30,14,7,3,1]`, alerted None → no fire (40>30).
  - 25 days, alerted None → fire, new_alerted_days=Some(14)? NO — **most-urgent-first: the smallest crossed-unalerted T with days<=T**. days=25 crosses only 30 (25<=30) not 14 (25>14) → fire T=30? Wait — smallest crossed T means smallest T with 25<=T → T=30 (25<=30 true, 25<=14 false) → the only crossed threshold is 30 → fire 30, alerted=30.
  - 10 days, alerted Some(30) → crossed unalerted = smallest T with 10<=T and T<30 → T∈{14,7,3,1} with 10<=T → 14 → fire 14, alerted=14. (Then next eval at 10 days alerted=14 → smallest T<14 with 10<=T → 7? 10<=7 false → none → no re-fire. Good.)
  - 5 days, alerted Some(14) → smallest T<14 with 5<=T → 7 → fire 7, alerted=7.
  - renewal: 40 days, alerted Some(7) → days>alerted → reset alerted=None, no fire.
  Write these as exact asserts.
- [ ] **Step 2: Run → FAIL.** **Step 3: Implement** `tier`: if `alerted_days.map_or(false,|a| days_remaining > a)` → `TierDecision{fire:false, new_alerted_days:None}` (renewal reset). Else find `t = alert_days.iter().filter(|&&t| days_remaining <= t && alerted_days.map_or(true,|a| t < a)).min()`; if Some(t) → `{fire:true, new_alerted_days:Some(t)}` else `{fire:false, new_alerted_days:alerted_days}`. **Step 4: Run → PASS.** **Step 5: Commit** `git commit -am "feat: tiered cert/domain alert logic (most-urgent-first fire-once)"`

---

## Task 6: probe::run "ssl" arm (persist + up/down)

**Files:** Modify `crates/vigil/src/probe/mod.rs`, `src/worker.rs` (or add `certcheck::ssl_probe`). Test: `tests/prober_ssl.rs`.

**Interfaces:** `probe::run` handles `"ssl"`; an ssl monitor persists `ssl_certs` and returns `ok=is_valid`.

- [ ] **Step 1: Failing test** — with a local self-signed TLS server (reuse Task 3's harness), create an `ssl` monitor (host/port), call `worker::run_check` (needs a persisted monitor + `test_state`), assert: a `ssl_certs` row was written for the monitor, and the monitor's status went `down` (self-signed → is_valid=false → Cause::Ssl). (confirmation_threshold=1.)
- [ ] **Step 2: Run → FAIL.** **Step 3: Implement.** `probe::run`: `"ssl" => { let r = certcheck::ssl::check(host, port||443, timeout).await; persist r into ssl_certs (upsert); ProbeOutcome{ ok:r.is_valid, cause: if r.is_valid {None} else {Some(Cause::Ssl)}, error_message:r.error, response_time_ms:.., status_code:None, resolved_ip:None } }`. (Persisting needs the pool — pass it, or have the worker persist; simplest: `certcheck::ssl_probe(pool, m)->ProbeOutcome` that checks+persists, called from `probe::run` — but `probe::run` has no pool. Resolve: add the ssl persist in `worker::run_check` after `probe::run` when `m.type=="ssl"`, OR give `probe::run` the pool. Choose: `worker::run_check` special-cases nothing — instead `probe::run` stays pure-ish and the SSL persist happens in a small `certcheck::ssl_probe(&AppState,&Monitor)` the worker calls for ssl type. State the chosen wiring in the report.) **Step 4: Run → PASS** + suite + clippy. **Step 5: Commit** `git commit -am "feat: ssl monitor type — cert check drives up/down, persists ssl_certs"`

---

## Task 7: cert_scheduler + send_alert wiring + refresh-on-demand

**Files:** Create `crates/vigil/src/cert_scheduler.rs`; Modify `src/main.rs` (spawn), `src/settings_store.rs` (cadence keys), `src/notify/dispatch.rs` (send_alert already there). Test: `tests/cert_scheduler.rs`.

**Interfaces:** `cert_scheduler::run(AppState)`; `refresh_ssl(&AppState,id)`, `refresh_domain(&AppState,id)` (also used by the API task).

- [ ] **Step 1: Failing test** — `refresh_ssl_persists_and_alerts`: with a local self-signed TLS server + an http/keyword monitor with `ssl_check_enabled` (https url pointing at the server) and one channel on `ssl_invalid`, call `cert_scheduler::refresh_ssl(&state, id)`; assert an `ssl_certs` row is written (is_valid=false) AND (since invalid + anchor Online in test_state) an `ssl_invalid` alert was delivered (email or http double recorded 1). Also a tiered test: set `ssl_certs` with days_remaining low + a channel on `ssl_expiring`, run the eval, assert `ssl_expiring` fired once (and not twice on a second run — alerted_days set).
- [ ] **Step 2: Run → FAIL.** **Step 3: Implement.** `refresh_ssl`: derive target (https url host+port / ssl host+port), `certcheck::ssl::check`, upsert `ssl_certs`, then evaluate: anchor-gate (skip if Offline); `ssl_invalid` if captured-but-invalid && !invalid_alerted (NOT for type=='ssl'); tiered `ssl_expiring` via `alerts::tier` over `ssl_alert_days`; on fire `notify::send_alert` + update `alerted_days`/`invalid_alerted`; emit SSE `monitor:cert_updated{id}`. `refresh_domain` analogous (skip persist on `transient`). `cert_scheduler::run`: loop `sleep(tick)`; select SSL-due (`ssl_check_enabled OR type='ssl'`, last_checked stale) + domain-due monitors; refresh under a semaphore(concurrency). Startup catch-up (refresh never-checked). `main::serve` spawns it. `settings_store`: `cert_*` getters with defaults. **Step 4: Run → PASS** + suite + clippy + no aws-lc. **Step 5: Commit** `git commit -am "feat: cert_scheduler (slow cadence, anchor-gated), refresh + tiered alerts, SSE cert event"`

---

## Task 8: API — ssl/domain get + refresh endpoints

**Files:** Modify `crates/vigil/src/api/monitors.rs`, `src/api/mod.rs` (routes). Test: `tests/api_certs.rs`.

- [ ] **Step 1: Failing test** — create an ssl monitor; POST `/api/monitors/:id/refresh-ssl` (against a local TLS server or accept the error row) → 200 with a row; GET `/api/monitors/:id/ssl` → the row; GET `/domain` on a domain-enabled monitor → row or null. Register routes in api/mod.rs.
- [ ] **Step 2: Run → FAIL.** **Step 3: Implement** `get_ssl`/`get_domain` (SELECT the row → JSON or null), `refresh_ssl`/`refresh_domain` handlers (call `cert_scheduler::refresh_*` then return the row). **Register** `.route("/monitors/:id/ssl", get(...))`, `/domain`, `.route("/monitors/:id/refresh-ssl", post(...))`, `/refresh-domain` in `api::routes()`. **Step 4: Run → PASS** + suite + clippy. **Step 5: Commit** `git commit -am "feat: ssl/domain get + refresh API"`

---

## Task 9: Frontend — SslCard + SSE live-update

**Files:** Create `web/src/components/SslCard.tsx`; Modify `web/src/api.ts` (getSsl, refreshSsl), `DetailPanel.tsx`, `store.ts` (cert_updated event). Test: `web/src/__tests__/sslcard.test.tsx`.

- [ ] **Step 1: Failing test** — stub fetch: getSsl returns `{issuer:"Let's Encrypt", subject:"CN=x", valid_until:<future>, days_remaining:12, is_valid:true, chain_ok:true, hostname_match:true, self_signed:false, error:null}`; render `<SslCard monitorId={1} />`; assert issuer renders, the ring shows amber (7–30) via a `data-tier="amber"` attr, and "12" days shows. A second case: `error:"handshake failed"` → the error is shown.
- [ ] **Step 2: Run → FAIL.** **Step 3: Implement** `getSsl(id)`/`refreshSsl(id)` in api.ts; `SslCard`: fetch getSsl; days-remaining ring (inline SVG, `data-tier` green>30/amber7-30/red<7-or-invalid), issuer/subject/valid-until, chain/hostname/self-signed pills, Refresh button (refreshSsl→refetch), error display. Add `cert_updated` handling to `applyEvent`/store so an open card refetches. Wire `<SslCard>` into DetailPanel (gated on `ssl_check_enabled || type=='ssl'`). **Step 4: Run → PASS** + build + tsc. **Step 5: Commit** `git commit -am "feat(web): SSL card with days-remaining ring + live update"`

---

## Task 10: Frontend — DomainCard

**Files:** Create `web/src/components/DomainCard.tsx`; Modify `api.ts` (getDomain, refreshDomain), `DetailPanel.tsx`. Test: `web/src/__tests__/domaincard.test.tsx`.

- [ ] **Step 1: Failing test** — getDomain returns `{registrar:"NameCheap", expiry_date:<future>, days_remaining:60, name_servers:"ns1,ns2", status_codes:"clientTransferProhibited", queryable:true, ...}` → registrar + ring (green>45) render; a second case `queryable:false` → "not queryable" note shows.
- [ ] **Step 2: Run → FAIL.** **Step 3: Implement** getDomain/refreshDomain; `DomainCard`: ring (green>45/amber7-45/red<7), registrar/expiry/nameservers/status, "not queryable — registry not queryable" when `queryable===false`, Refresh. Wire into DetailPanel (gated on `domain_check_enabled`). **Step 4: Run → PASS** + build + tsc. **Step 5: Commit** `git commit -am "feat(web): domain card with expiry ring + not-queryable state"`

---

## Task 11: Frontend — MonitorForm SSL/domain toggles + ssl type + alert chips

**Files:** Modify `web/src/components/MonitorForm.tsx`. Test: extend `web/src/__tests__/form.test.tsx`.

- [ ] **Step 1: Failing test** — enable the SSL toggle on an https monitor → an `ssl_alert_days` chip editor appears; save calls createMonitor with `ssl_check_enabled:true`. Add `ssl` to the type selector → host+port fields (like port); the SSL toggle is auto-on/forced for ssl type.
- [ ] **Step 2: Run → FAIL.** **Step 3: Implement** SSL/domain toggle checkboxes (SSL enabled only for https url or ssl type), alert-day chip editors (comma text ↔ JSON array), add `ssl` to the type selector (host+port fields). buildDto includes `ssl_check_enabled`/`ssl_alert_days`/`domain_check_enabled`/`domain_alert_days`. **Step 4: Run → PASS** + build + tsc. **Step 5: Commit** `git commit -am "feat(web): monitor form SSL/domain toggles + alert chips + ssl type"`

---

## Task 12: Frontend — Channel manager types (webhook/discord/ntfy) + SMTP username + new triggers

**Files:** Modify `web/src/components/Settings.tsx`, `api.ts`. Test: extend `web/src/__tests__/settings.test.tsx`.

- [ ] **Step 1: Failing test** — the channel-create form has a type selector; selecting "webhook" shows a URL field (and hides SMTP fields); selecting "email" shows an optional "SMTP username" field; the trigger checkboxes include "ssl_expiring". Saving a webhook channel calls createChannel with `type:"webhook"` + a config containing the url.
- [ ] **Step 2: Run → FAIL.** **Step 3: Implement** a channel **type** selector (email/webhook/discord/ntfy); type-specific config fields (email + optional username; webhook url/method/headers/template; discord webhook url; ntfy server/topic/priority/token); trigger checkboxes gain ssl_expiring/ssl_invalid/domain_expiring; Send-test routes to `/channels/:id/test` (backend already type-aware). **Step 4: Run → PASS** + build + tsc. **Step 5: Commit** `git commit -am "feat(web): channel manager webhook/discord/ntfy + SMTP username + cert triggers"`

---

## Task 13: Acceptance + final review

**Files:** Create `docs/superpowers/plans/P3-acceptance.md`. No product code unless a DoD item fails.

- [ ] **Step 1** — `0003` on a real P1/P2 DB copy: version=3, data preserved.
- [ ] **Step 2** — enable SSL on a live https monitor (`one.one.one.one`, type ssl or http+ssl_check_enabled) → `refresh-ssl` → `ssl_certs` captured, days_remaining>0, is_valid true (SNI correct).
- [ ] **Step 3** — a domain monitor (`myapp.com` or similar) → `refresh-domain` → RDAP expiry (or explicit `queryable:false` for a redacted TLD — either is a pass as long as it's not a crash/false-green).
- [ ] **Step 4** — a **webhook** channel pointed at a local catcher → `/channels/:id/test` → the catcher receives the JSON.
- [ ] **Step 5 (user-requested LIVE Mailgun)** — configure an email channel with `username=postmaster@sandbox….mailgun.org`, host `smtp.mailgun.org:587` starttls, from + an **authorized** recipient (prompt the user for the API-key-in-secret + recipient + from at acceptance time); `/channels/:id/test` → Mailgun accepts (proves `username != from` SMTP auth). See `scratchpad/p3-mailgun-acceptance.txt`.
- [ ] **Step 6** — Docker rebuild → healthy on 8099; confirm `cargo tree | grep -iE 'aws-lc|openssl'` empty.
- [ ] **Step 7: Commit** the acceptance checklist. Then final whole-branch review (opus) + merge.

---

## Definition of Done
All §1 spec items verified; `cargo test` + `vitest` green; `0003` on a P1/P2 DB; **no aws-lc/openssl in the lock**; Docker healthy; live SSL cert captured + a channel delivered; every task committed.
