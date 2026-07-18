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
pub struct NotifyMsg { pub subject:String, pub body:String, pub body_html:Option<String> }
// deliver()'s email arm maps NotifyMsg.body_html -> EmailMsg.body_html.
// SmtpConfig gains `username: Option<String>`; auth username = auth_user(&cfg.username, &msg.from)
//   where `fn auth_user(username:&Option<String>, from:&str)->String { username.clone().unwrap_or_else(|| from.to_string()) }`
//   (SmtpConfig has NO `from` — `from` lives on EmailMsg; the helper takes both).
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
//   Constructed in THREE places — main::serve (real), tests/common `test_state` AND
//   `test_state_offline` (RecordingHttpSender). All three must be updated (Task 2).

// events.rs — Event enum gains ONE variant (serde tag = snake_case, matching P2):
//   CertUpdated { id: i64 }  → serializes {"event":"cert_updated","data":{"id":..}}
//   Reused for BOTH ssl and domain refreshes; SslCard AND DomainCard refetch on it.
//   (There is NO "monitor:" prefix — the frontend switches on the snake_case tag.)

// certcheck::persist_ssl(pool, monitor_id, &SslResult) — the ONLY writer of cert DATA columns.
//   Column-scoped upsert: INSERT(monitor_id, issuer..self_signed, error) ON CONFLICT(monitor_id)
//   DO UPDATE SET issuer=excluded.issuer, ... , error=excluded.error — touching ONLY data columns.
//   It NEVER writes last_checked, alerted_days, or invalid_alerted (those are the fire-once
//   bookkeeping owned solely by cert_scheduler::refresh_ssl). Same discipline for domain.
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
- [ ] **Step 4: Implement.** Append `(3, include_str!("../migrations/0003_certs.sql"))` to `MIGRATIONS` in db.rs. `models.rs`: add 4 `Monitor` fields + FromRow (`ssl_check_enabled`/`domain_check_enabled` i64→bool), extend `test_defaults_monitor`; add `Cause::Ssl` (serde lowercase); add `SslCert`+`DomainInfo` structs + manual FromRow; `CreateMonitorDto`/`UpdateMonitorDto` gain the 4 optional fields; `create()`/`update()` INSERT/UPDATE them. **`engine.rs`: add `Some(Cause::Ssl) => "ssl"` to the cause→str match.** `validate_monitor_dto` (add an `ssl_check_enabled: bool` param — it currently takes none; update its call site in `create`/`update`): `ssl_check_enabled` may be true ONLY on `http`/`keyword` (with `url` starting `https://`) or `ssl` type; on `port`/`ping`/`dns` it is a 422 (else such a monitor is selected by cert_scheduler and produces a false "errored" row). `ssl` type requires `host` (422 otherwise). Add a test asserting `ssl_check_enabled=true` on a `port` monitor → 422.
- [ ] **Step 5: Run → PASS** + `cargo test -p vigil` + `cargo clippy --all-targets -- -D warnings` (fix any Monitor/DTO construction sites). **Step 6: Commit** `git commit -am "feat: migration 0003 (cert add-ons + ssl_certs/domain_info), Cause::Ssl, validation"`

---

## Task 2: Notify refactor — deliver() core, HttpSender, senders, Trigger variants, SMTP username

**Files:** Modify `crates/vigil/src/notify/{mod,dispatch,email,templates}.rs`, `src/models.rs` (Trigger), `src/app.rs` (AppState), `src/api/channels.rs` (test routing), `src/main.rs` (construct http_sender), `tests/common/mod.rs` (RecordingHttpSender). Create `src/notify/http.rs` (webhook/discord/ntfy). Test: `tests/notify_multi.rs`.

**Interfaces:** `Trigger`+3; `NotifyMsg`; `AlertCtx`; `HttpSender`+`RecordingHttpSender`; `notify::{deliver, on_transition, send_alert}`; AppState.http_sender.

- [ ] **Step 1: Failing tests** — `tests/notify_multi.rs`:
  - `two_channels_on_down_both_fire`: seed a monitor + an email channel + a webhook channel, both with trigger `down`; call `notify::on_transition(down)`; assert the email double recorded 1 send AND the http double recorded 1 webhook send (proves per-`(monitor,channel,trigger)` cooldown, not the old single-channel bug).
  - `webhook_payload_shape`: a webhook channel test → the recorded body is JSON with the monitor name.
  - `smtp_username_used_when_set`: unit-test the credential-selection helper directly — `auth_user(&Some("apikey".into()), "no-reply@x.com") == "apikey"` and `auth_user(&None, "no-reply@x.com") == "no-reply@x.com"` (username falls back to the From address). `SmtpConfig` has NO `from` field, so the helper takes both args; do not assert on a Transport double's internal SmtpConfig.
- [ ] **Step 2: Run → FAIL.**
- [ ] **Step 3: Implement.**
  - `models.rs`: add `Trigger::{SslExpiring, SslInvalid, DomainExpiring}` (+ `as_str`: `ssl_expiring|ssl_invalid|domain_expiring`). **Change the `Trigger` serde attr from `rename_all="lowercase"` to `rename_all="snake_case"`** so any serde output matches `as_str()` (else the multi-word variants serialize as `sslexpiring`, diverging from the load-bearing `ssl_expiring` in `monitor_notifications.triggers`).
  - `notify/mod.rs`: add `NotifyMsg`, `AlertCtx`, `#[async_trait] trait HttpSender`, `RecordingHttpSender` (Arc<Mutex<Vec<(String, Value, NotifyMsg)>>>). Add `username: Option<String>` to `SmtpConfig` (which is `{host,port,security}` today — it has NO `from`). Add a free helper `fn auth_user(username:&Option<String>, from:&str)->String { username.clone().unwrap_or_else(|| from.to_string()) }`. `SmtpTransport::send` uses `Credentials::new(auth_user(&cfg.username, &msg.from), password)` (the From address lives on `EmailMsg`, passed alongside the SmtpConfig).
  - **Plumb `username` through BOTH `EmailChannelConfig` copies** — `dispatch.rs:35` AND `api/channels.rs:116` — each gains `#[serde(default)] username: Option<String>`, and each site that builds a `SmtpConfig` from an `EmailChannelConfig` (dispatch's deliver + `channels.rs:132`, the `/channels/:id/test` path the Mailgun acceptance drives) sets `username: cfg.username.clone()`.
  - `notify/http.rs`: `struct ReqwestHttpSender` impl `HttpSender` — match channel_type: `webhook` (POST config.url, method default POST, headers, JSON body from config.template with {{vars}} substituted+escaped, default template per spec §6), `discord` (POST config.webhook_url `{content,embeds:[{title,description,color}]}`), `ntfy` (POST `{server||https://ntfy.sh}/{topic}` body=msg.body, headers Title/Priority/Tags, Authorization Bearer if token). Uses a shared reqwest client (rustls). Errors → anyhow.
  - `notify/dispatch.rs`: **extract `deliver(state, m, trigger, msg, incident_id)`** — query attached active channels (JOIN monitor_notifications) whose `triggers` JSON includes `trigger.as_str()` — **DROP the `AND nc.type='email'` filter**; per channel, cooldown `SELECT MAX(sent_at) ... WHERE monitor_id=? AND channel_id=? AND trigger=?`; if allowed, `match ch.type { "email" => build SmtpConfig+EmailMsg from config, state.transport.send; _ => state.http_sender.send(ch.type, config, msg) }`; insert notification_log. `on_transition` renders the down/recovered NotifyMsg via templates and calls `deliver`. Add `send_alert(state, m, trigger, ctx)` rendering the ssl/domain NotifyMsg and calling `deliver`. **`trigger_status()` (dispatch.rs:44) is an exhaustive `match Trigger { Down|Recovered }` — adding 3 variants breaks it: add arms for the cert triggers (they have no up/down status → return `None`/skip) or a `_ =>` arm.**
  - `templates.rs`: `render(trigger, ctx)` handles all 5 triggers (ssl/domain subjects/bodies use AlertCtx fields; add the §6 variables). (Two ctx types OR a merged ctx — keep it simple with an enum or two render fns; the tests pin down/recovered + one ssl body.)
  - `app.rs`: `AppState` gains `http_sender: Arc<dyn HttpSender>`. `main::serve`: `Arc::new(ReqwestHttpSender::new())`. `tests/common`: **BOTH `test_state` AND `test_state_offline`** build a `RecordingHttpSender` and expose `sent_http` (both construct `AppState` — miss either and the crate won't compile; the offline one is what Task 7's anchor-gate test exercises).
  - `api/channels.rs::test`: route by channel.type (email → transport, else → http_sender) building a sample NotifyMsg.
- [ ] **Step 4: Run → PASS** (multi-channel + payload + username) + full suite + clippy + **no aws-lc/openssl** (no new tls dep yet, but confirm). **Step 5: Commit** `git commit -am "feat: notify refactor — multi-channel deliver(), HttpSender (webhook/discord/ntfy), per-channel cooldown, SMTP username, ssl/domain triggers"`

---

## Task 3: SSL cert check (rustls capture, SNI, x509, RFC6125, chain)

**Files:** Create `crates/vigil/src/certcheck/{mod,ssl}.rs`; Modify `src/lib.rs`, `crates/vigil/Cargo.toml`. Test: `tests/certcheck_ssl.rs`.

**Interfaces:** `ssl::check(host,port,timeout)->SslResult`; `ssl::hostname_matches(...)` (pure, RFC6125).

- [ ] **Step 1: Add deps** — `tokio-rustls = { version="0.26", default-features=false, features=["ring","tls12"] }`, `x509-parser="0.16"`, `webpki-roots="1"`, and the **dev-dependency** (under `[dev-dependencies]`) `rcgen = { version="0.13", default-features=false, features=["ring","pem"] }` — a bare `rcgen` pulls its default `aws_lc_rs`/`crypto` features and `cargo tree` (which includes dev-deps) would then trip the no-aws-lc gate. `cargo build` then `cargo tree -p vigil | grep -iE 'aws-lc|openssl'` MUST be empty (if aws-lc appears, a feature leaked — fix the pin). Note rcgen 0.13's `CertifiedKey` API: `cert.der()` and `key_pair.serialize_der()` (differs from 0.12).
- [ ] **Step 2: Failing tests** — `tests/certcheck_ssl.rs`:
  - `hostname_matches` RFC6125 vectors (pure): `["example.com"] vs "example.com"`→true; `["*.example.com"] vs "a.example.com"`→true, `vs "example.com"`→false, `vs "a.b.example.com"`→false; case-insensitive; CN fallback only when SANs empty.
  - integration `local_self_signed_cert`: start a `tokio-rustls` TLS server on 127.0.0.1:0 with a generated self-signed cert (use `rcgen` as a **dev-dependency** to mint the cert), then `ssl::check("127.0.0.1", port, 5)` → `self_signed==true`, `chain_ok==false` (not in Mozilla roots), a parsed `valid_until` in the future, `error==None` (handshake completed via the capturing verifier).
  - (optional, network-gated) a live `one.one.one.one:443` smoke asserting `is_valid && days_remaining>0` — guard so CI without network still passes (skip/log if it errors).
- [ ] **Step 3: Run → FAIL.**
- [ ] **Step 4: Implement.** `certcheck/mod.rs` (`pub mod ssl;` + `pub mod domain;` later; add `pub mod certcheck;` to lib.rs). `ssl.rs`:
  - A `Capturing` struct impl `rustls::client::danger::ServerCertVerifier` with **all four** methods: `verify_server_cert` stores `end_entity`+`intermediates` (clone the DER) into an `Arc<Mutex<Option<Vec<CertificateDer>>>>` and returns `Ok(ServerCertVerified::assertion())`; `verify_tls12_signature`/`verify_tls13_signature` delegate to `rustls::crypto::{verify_tls12_signature, verify_tls13_signature}(message, cert, dss, &ring_provider.signature_verification_algorithms)`; `supported_verify_schemes()` returns `ring_provider.signature_verification_algorithms.supported_schemes()`.
  - Build `ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider())).with_safe_default_protocol_versions()?.dangerous().with_custom_certificate_verifier(Arc::new(capturing)).with_no_client_auth()`.
  - `TcpStream::connect((host,port))` (timeout-wrapped), `tokio_rustls::TlsConnector::from(config).connect(ServerName::try_from(host.to_owned())?, tcp)` — **ServerName = host (SNI)**; `try_from(&str)` yields a borrowed name that fails the connector's `ServerName<'static>` bound, so pass the OWNED `host.to_owned()`. For a bare IP use `ServerName::IpAddress(ip.into())` (already 'static).
  - After handshake, take the captured chain; parse `chain[0]` with `x509_parser::parse_x509_certificate`: issuer/subject (`.to_string()`), `validity().not_before/not_after` → epoch, SAN dNSNames. `days_remaining`. `hostname_matches(sans, cn, host)`. `chain_ok`: build a `WebPkiServerVerifier` over `webpki_roots::TLS_SERVER_ROOTS` **via `builder_with_provider(roots, Arc::new(rustls::crypto::ring::default_provider()))` (NOT the bare `builder()`, whose provider resolves from crate features and would panic if a second crypto provider ever leaked in)** and verify the chain at `now` (Ok→true). `self_signed`: issuer==subject or len==1. `is_valid = now∈[from,until] && chain_ok && hostname_match`.
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

## Task 6: `ssl` monitor type (cert check drives up/down + persists cert data)

**Files:** Modify `crates/vigil/src/worker.rs`, `src/certcheck/ssl.rs` (add `ssl_probe`), `src/probe/mod.rs` (keep the `_ => http` fallback; `ssl` no longer routes here), `src/api/monitors.rs` (`test_check` for ssl). Test: `tests/prober_ssl.rs`.

**Interfaces:** `certcheck::ssl::ssl_probe(&AppState, &Monitor) -> ProbeOutcome` (checks + persists cert data, returns up/down); `worker::run_check` branches on `m.type=="ssl"`.

**Chosen wiring (was ambiguous — this is now decided):** `probe::run(&Monitor)` has NO pool, so it CANNOT persist. It stays as-is (its `_ => http::probe` fallback is fine — `ssl` never reaches it). Instead **`worker::run_check` branches at the top: `if m.type=="ssl" { certcheck::ssl::ssl_probe(state, m).await } else { probe::run(m).await }`.** `ssl_probe` calls `ssl::check`, then `certcheck::persist_ssl(&state.db, m.id, &r)` (the column-scoped data-only upsert from Shared Types — it writes cert columns + `error`, and **NEVER `last_checked`/`alerted_days`/`invalid_alerted`**; leaving `last_checked` to cert_scheduler is what lets the 12h `ssl_expiring` eval still see an ssl-type monitor as due — otherwise the fast probe would keep `last_checked` fresh and cert_scheduler would never run the expiry eval for it), and returns `ProbeOutcome{ ok:r.is_valid, cause: if r.is_valid {None} else {Some(Cause::Ssl)}, error_message:r.error, response_time_ms:handshake_ms, status_code:None, resolved_ip:None }`.

**`test_check` (monitors.rs:320) currently calls `probe::run(&m)` with no state** → an unsaved `ssl` DTO would fall to `http::probe`. Fix: in `test_check`, branch `if dto.type=="ssl" { let r = certcheck::ssl::check(host, port.unwrap_or(443), timeout).await; return the SslResult summary as the probe result (is_valid→ok, error, no persist) }` — the live "Test check" button must not write `ssl_certs` for an unsaved monitor.

- [ ] **Step 1: Failing test** — with a local self-signed TLS server (reuse Task 3's harness), create an `ssl` monitor (host/port), call `worker::run_check` (needs a persisted monitor + `test_state`), assert: a `ssl_certs` row was written for the monitor (cert data present, `last_checked IS NULL` — proving the fast probe left the cadence marker for cert_scheduler), and the monitor's status went `down` (self-signed → is_valid=false → Cause::Ssl). (confirmation_threshold=1.)
- [ ] **Step 2: Run → FAIL.** **Step 3: Implement** per the chosen wiring above (`ssl_probe`, `persist_ssl`, `worker::run_check` branch, `test_check` branch). **Step 4: Run → PASS** + suite + clippy + no aws-lc. **Step 5: Commit** `git commit -am "feat: ssl monitor type — cert check drives up/down, persists ssl_certs (last_checked owned by cert_scheduler)"`

---

## Task 7: cert_scheduler + send_alert wiring + refresh-on-demand

**Files:** Create `crates/vigil/src/cert_scheduler.rs`; Modify `src/main.rs` (spawn), `src/settings_store.rs` (cadence keys), `src/events.rs` (add `CertUpdated { id }` variant), `src/notify/dispatch.rs` (send_alert already there). Test: `tests/cert_scheduler.rs`.

**Interfaces:** `cert_scheduler::run(AppState)`; `refresh_ssl(&AppState,id)`, `refresh_domain(&AppState,id)` (also used by the API task).

- [ ] **Step 1: Failing test** — `refresh_ssl_persists_and_alerts`: with a local self-signed TLS server + an http/keyword monitor with `ssl_check_enabled` (https url pointing at the server) and one channel on `ssl_invalid`, call `cert_scheduler::refresh_ssl(&state, id)`; assert an `ssl_certs` row is written (is_valid=false) AND (since invalid + anchor Online in test_state) an `ssl_invalid` alert was delivered (email or http double recorded 1). Also a tiered test: set `ssl_certs` with days_remaining low + a channel on `ssl_expiring`, run the eval, assert `ssl_expiring` fired once (and not twice on a second run — alerted_days set).
- [ ] **Step 2: Run → FAIL.** **Step 3: Implement.** `refresh_ssl(&AppState, id)`:
  1. **Read the OLD `alerted_days` + `invalid_alerted` FIRST** (`SELECT alerted_days, invalid_alerted FROM ssl_certs WHERE monitor_id=?`, default None/false if no row).
  2. derive target (https url host+port / ssl host+port), `certcheck::ssl::check`.
  3. `certcheck::persist_ssl(&state.db, id, &r)` — the **column-scoped data-only upsert** (writes cert columns + `error`; **NEVER touches `alerted_days`/`invalid_alerted`** — a full-row `INSERT OR REPLACE` here would reset the fire-once state every 12h and re-alert forever).
  4. evaluate (anchor-gate: skip if `state.anchor.current()==Offline`): `ssl_invalid` fires iff `!r.is_valid && r.error.is_none()` (captured-but-invalid, NOT a transport error) `&& !old_invalid_alerted` (NOT for type=='ssl'); tiered `ssl_expiring` via `alerts::tier(days, ssl_alert_days, old_alerted_days)`.
  5. on fire: `notify::send_alert`, then a **separate bookkeeping `UPDATE ssl_certs SET last_checked=?, alerted_days=?, invalid_alerted=? WHERE monitor_id=?`** (this is the ONLY writer of those three columns). On no fire, still `UPDATE ssl_certs SET last_checked=?` (advance the cadence clock; carry alerted forward, or set to `tier`'s `new_alerted_days` on a renewal reset).
  6. emit `Event::CertUpdated{id}` on the bus.
  `refresh_domain` analogous over `domain_info`: on a **`transient`** result, do NOT overwrite known fields or `queryable` — but **DO `UPDATE domain_info SET last_checked=now`** (advance the retry clock; else the due-query keeps re-selecting it every tick and hammers rdap.org, worsening the 429). `cert_scheduler::run`: loop `sleep(tick)`; select SSL-due (`ssl_check_enabled OR type='ssl'`, `last_checked` NULL or stale by `cert.ssl_interval_seconds`) + domain-due monitors; refresh under a `Semaphore(cert.concurrency)`. Startup catch-up (refresh never-checked). `main::serve` spawns it. `settings_store`: `cert_*` getters with defaults. `events.rs`: add `CertUpdated { id: i64 }`. **Step 4: Run → PASS** + suite + clippy + no aws-lc. **Step 5: Commit** `git commit -am "feat: cert_scheduler (slow cadence, anchor-gated), refresh + tiered alerts (fire-once column-scoped), SSE cert event"`

---

## Task 8: API — ssl/domain get + refresh endpoints

**Files:** Modify `crates/vigil/src/api/monitors.rs`, `src/api/mod.rs` (routes). Test: `tests/api_certs.rs`.

- [ ] **Step 1: Failing test** — create an ssl monitor; POST `/api/monitors/:id/refresh-ssl` (against a local TLS server or accept the error row) → 200 with a row; GET `/api/monitors/:id/ssl` → the row; GET `/domain` on a domain-enabled monitor → row or null. Register routes in api/mod.rs.
- [ ] **Step 2: Run → FAIL.** **Step 3: Implement** `get_ssl`/`get_domain` (SELECT the row → JSON or null), `refresh_ssl`/`refresh_domain` handlers (call `cert_scheduler::refresh_*` then return the row). **Register** `.route("/monitors/:id/ssl", get(...))`, `/domain`, `.route("/monitors/:id/refresh-ssl", post(...))`, `/refresh-domain` in `api::routes()`. **Step 4: Run → PASS** + suite + clippy. **Step 5: Commit** `git commit -am "feat: ssl/domain get + refresh API"`

---

## Task 9: Frontend — SslCard + SSE live-update

**Files:** Create `web/src/components/SslCard.tsx`; Modify `web/src/api.ts` (getSsl, refreshSsl), `DetailPanel.tsx`, `store.ts` (cert_updated event). Test: `web/src/__tests__/sslcard.test.tsx`.

- [ ] **Step 1: Failing test** — stub fetch: getSsl returns `{issuer:"Let's Encrypt", subject:"CN=x", valid_until:<future>, days_remaining:12, is_valid:true, chain_ok:true, hostname_match:true, self_signed:false, error:null}`; render `<SslCard monitorId={1} />`; assert issuer renders, the ring shows amber (7–30) via a `data-tier="amber"` attr, and "12" days shows. A second case: `error:"handshake failed"` → the error is shown.
- [ ] **Step 2: Run → FAIL.** **Step 3: Implement** `getSsl(id)`/`refreshSsl(id)` in api.ts; `SslCard`: fetch getSsl; days-remaining ring (inline SVG, `data-tier` green>30/amber7-30/red<7-or-invalid), issuer/subject/valid-until, chain/hostname/self-signed pills, Refresh button (refreshSsl→refetch), error display. Add a `cert_updated` case to `applyEvent` (the frontend switches on the snake_case serde tag — the backend variant `CertUpdated{id}` serializes to `{"event":"cert_updated","data":{"id"}}`, matching the P2 event convention; there is no `monitor:` prefix). Since `applyEvent` reduces the monitor LIST (not card-local cert state), expose the last cert-updated monitor id (e.g. a `certVersion`/`lastCertUpdate` signal in the store keyed by id) that an open `SslCard`/`DomainCard` reads to trigger a refetch. Wire `<SslCard>` into DetailPanel (gated on `ssl_check_enabled || type=='ssl'`). **Step 4: Run → PASS** + build + tsc. **Step 5: Commit** `git commit -am "feat(web): SSL card with days-remaining ring + live update"`

---

## Task 10: Frontend — DomainCard

**Files:** Create `web/src/components/DomainCard.tsx`; Modify `api.ts` (getDomain, refreshDomain), `DetailPanel.tsx`. Test: `web/src/__tests__/domaincard.test.tsx`.

> **SSE:** `DomainCard` reuses the same `cert_updated` signal wired in Task 9 (refresh_domain also emits `Event::CertUpdated{id}`) — subscribe so a scheduled domain refresh live-updates an open card, exactly as `SslCard` does.

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
