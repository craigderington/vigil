# Vigil P3 (Certificates) — Design Spec

> **Status:** autonomous build (user "kick off P3") · **Date:** 2026-07-18 · **Revision:** v2 (spec-review-hardened) · **Base:** P1+P2 on `master` (666ee16)
> **Scope:** Phase 3 "Certificates" per [`CLAUDE.md`](../../../CLAUDE.md) §14. Inherits all P1/P2 decisions (containerized axum + SolidJS, rustls-only, SQLite/WAL, uptime-from-incidents, ports 8090/8099, version-ordered migrations, anchor gate).

Autonomous decisions grounded in `CLAUDE.md` §6/§7/§9/§11.6. v2 incorporates the multi-lens spec review (changelog §13). Where this doc and `CLAUDE.md` differ, this doc wins for P3.

---

## 1. P3 Definition of Done

**Expiry surprises are impossible.**

1. **SSL tracking:** an HTTPS `http`/`keyword` monitor with `ssl_check_enabled`, or the new `ssl` type, captures its certificate — issuer, subject, valid_from/until, days_remaining, chain_ok, hostname_match, self_signed, is_valid — via SNI-correct TLS. Tiered `ssl_expiring` alerts fire (most-urgent-first), plus an immediate `ssl_invalid` on a captured-but-invalid cert.
2. **Domain expiry:** a `domain_check_enabled` monitor resolves the **registrable domain** (eTLD+1) expiry via RDAP (WHOIS fallback), storing registrar/expiry/days/nameservers/status — cached ~24h. Tiered `domain_expiring`. Un-queryable registries show explicit "unknown", never false green; a transient lookup failure keeps the last-known value (not "unknown").
3. **New channels:** webhook, Discord, and ntfy work; the notify core routes by channel `type`; **each attached channel fires per trigger** (multi-channel), email unregressed.
4. **New triggers:** `ssl_expiring`, `ssl_invalid`, `domain_expiring` dispatchable per monitor per channel.
5. **UI:** detail-panel SSL card (issuer/subject/valid-until, days-remaining ring, chain/hostname/self-signed pills, Refresh) + domain card (registrar/expiry/ring/nameservers/lock, or "not queryable"), live-updating via SSE. Monitor form: SSL/domain toggles + alert-day chips + `ssl` type. Channel manager: webhook/Discord/ntfy types + optional SMTP username.
6. Migration `0003` applies on a P1/P2 DB. TDD-tested; `cargo test` + `vitest` green; Docker healthy; **deps stay ring-only (no openssl, no aws-lc)**.

---

## 2. Scope

### 2.1 In scope
- **SSL check** (rustls, SNI-correct, capturing verifier) → `x509-parser` leaf parse → validity/chain/hostname evaluated ourselves → `ssl_certs`.
- **Domain expiry** via RDAP (`rdap.org` redirector, transient-failure-aware) + WHOIS fallback (label-pinned parse) over the **eTLD+1** → `domain_info`.
- **Slow cadence** `cert_scheduler` (SSL 12h / domain 24h, from settings) + on-demand refresh.
- **Monitor add-ons** (`ssl_check_enabled`, `ssl_alert_days`, `domain_check_enabled`, `domain_alert_days`) + `ssl` monitor type (migration `0003`).
- **Notify refactor:** a shared delivery core routes by channel `type` → email / webhook / discord / ntfy; new triggers; per-`(monitor, channel, trigger)` cooldown; tiered fire-once. **Optional SMTP `username`** (falls back to `from`).
- **API:** `ssl`/`domain` get + refresh. **Frontend:** SSL/domain cards, form toggles, channel manager.

### 2.2 Out of scope (P4/later)
- Heartbeat/push monitors + `/ping` receiver logic, maintenance windows, monthly reports, re-notify/digest, Telegram/Pushover/Slack, DEGRADED, theme picker. **SSL add-on on `port`-type monitors** (P3 scopes SSL to `http`/`keyword`/`ssl`). Full public-suffix-list correctness, OCSP/CRL revocation, Certificate Transparency.

---

## 3. Architecture deltas

### 3.1 New deps (all ring-only — verified against Cargo.lock: rustls 0.23.42, rustls-webpki 0.103.13, webpki-roots 1.0.8)
- `tokio-rustls = { version = "0.26", default-features = false, features = ["ring", "tls12"] }` — **default features include `aws-lc-rs`; a bare add would give rustls two providers and panic at runtime + pull aws-lc-sys (C/cmake).** Build the config with an **explicit provider**: `ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider())).with_safe_default_protocol_versions()?`.
- `x509-parser = "0.16"` (pure-Rust leaf parse). `webpki-roots = "1"` (Mozilla roots for chain verify). Reuse the transitive `rustls-webpki 0.103` (imported as `webpki`). No `regex` (WHOIS uses manual matching). `chrono` (already) for RFC3339/date parse.
- **CI/build check:** assert `aws-lc` / `openssl` stay absent from `Cargo.lock`.

### 3.2 `certcheck` module (`crates/vigil/src/certcheck/{mod,ssl,domain,alerts}.rs`) — see §5.

### 3.3 `cert_scheduler` background task (spawned in `serve`, + startup catch-up)
- Ticks every `cert.tick_seconds` (default **60s**). Finds monitors due for SSL (`ssl_check_enabled=1 OR type='ssl'`, and `ssl_certs.last_checked` older than `cert.ssl_interval_seconds` default **43200**/12h, or never) and for domain (`domain_check_enabled=1`, older than `cert.domain_interval_seconds` default **86400**/24h). Refreshes under a semaphore of `cert.concurrency` (default **5**). Cadence/concurrency read from `settings_store` with those defaults.
- **Anchor-gated:** before evaluating/alerting, `state.anchor.current()`; if `Offline`, skip (leave last-known). Alerts only fire when connectivity is up.
- After each refresh: persist the row, emit an SSE `monitor:cert_updated{id}` event (§8), and run the alert evaluation (§5.3).

### 3.4 Notify refactor (the tricky integration — the cert triggers don't fit the P1 shape)
- `Trigger` gains `SslExpiring, SslInvalid, DomainExpiring` (with `as_str` + a status/label + `templates::render` arms). Existing `Down/Recovered` unchanged.
- Extract the P1 channel-loop/cooldown/log core out of `on_transition` into **`notify::deliver(state, monitor, trigger, msg: NotifyMsg, incident_id: Option<i64>) -> Result<()>`**: loops the monitor's attached active channels whose `triggers` include `trigger`, applies the **per-`(monitor, channel_id, trigger)`** cooldown, routes by `channel.type` to the right sender, writes `notification_log`.
- `on_transition(...)` renders the down/recovered `NotifyMsg` and calls `deliver`. `send_alert(state, monitor, trigger, ctx: AlertCtx)` (new, called by `cert_scheduler`) renders the ssl/domain `NotifyMsg` and calls `deliver`.
- **`AlertCtx`** (new, or widen `TemplateCtx`) carries: `monitor_name, url_or_host, ssl_days, ssl_valid_until, ssl_issuer, domain_days, domain_expiry, registrar` (all `Option`). `templates::render(trigger, &ctx)` produces `(subject, body)` per trigger.
- **Channel routing seam:** `dispatch`'s channel query drops the `AND nc.type='email'` filter and routes on `ch.type`. `AppState` gains `http_sender: Arc<dyn HttpSender>` (a reqwest-backed sender; a `RecordingHttpSender` double for tests, mirroring the email `Transport`/`RecordingTransport`). `api::channels::test` routes by `channel.type` too. Email keeps its `Transport` seam.
- **Cooldown fix (also fixes a latent P1 bug):** the cooldown query becomes `SELECT MAX(sent_at) FROM notification_log WHERE monitor_id=? AND channel_id=? AND trigger=?` — otherwise the first channel's just-inserted log row blocks every other channel in the same transition (only one channel would ever fire).

### 3.5 Probe + engine
- `Cause` gains `Ssl`. **`engine.rs` cause match gains `Some(Cause::Ssl) => "ssl"`** (the match is exhaustive — omitting it fails to compile). `probe::run` gains `"ssl" => certcheck::ssl_probe(m)`: runs `ssl::check`, **persists the `SslResult` to `ssl_certs`**, returns `ok = is_valid` (cause `Ssl` on failure) so the state machine + anchor gate drive up/down normally.

---

## 4. Data model — migration `0003_certs.sql`

```sql
ALTER TABLE monitors ADD COLUMN ssl_check_enabled INTEGER NOT NULL DEFAULT 0;
ALTER TABLE monitors ADD COLUMN ssl_alert_days TEXT NOT NULL DEFAULT '[30,14,7,3,1]';
ALTER TABLE monitors ADD COLUMN domain_check_enabled INTEGER NOT NULL DEFAULT 0;
ALTER TABLE monitors ADD COLUMN domain_alert_days TEXT NOT NULL DEFAULT '[45,30,14,7]';

CREATE TABLE ssl_certs (
  monitor_id     INTEGER PRIMARY KEY REFERENCES monitors(id) ON DELETE CASCADE,
  issuer TEXT, subject TEXT, valid_from INTEGER, valid_until INTEGER,
  days_remaining INTEGER, is_valid INTEGER, chain_ok INTEGER,
  hostname_match INTEGER, self_signed INTEGER,
  error TEXT,                          -- transport/handshake/parse failure (distinct from a captured-but-invalid cert)
  alerted_days INTEGER,                -- SMALLEST ssl_alert_days threshold already alerted; NULL = none; reset on renewal
  invalid_alerted INTEGER NOT NULL DEFAULT 0,  -- ssl_invalid fired for the current invalid state; reset on invalid->valid
  last_checked INTEGER
);

CREATE TABLE domain_info (
  monitor_id  INTEGER PRIMARY KEY REFERENCES monitors(id) ON DELETE CASCADE,
  registrar TEXT, expiry_date INTEGER, days_remaining INTEGER,
  name_servers TEXT, status_codes TEXT,
  queryable INTEGER,                   -- 1 = expiry known; 0 = registry not queryable (definitive unknown). A transient failure updates nothing.
  source TEXT,                         -- 'rdap' | 'whois' | null
  alerted_days INTEGER,                -- smallest domain_alert_days threshold alerted; NULL; reset on renewal
  last_checked INTEGER
);
```

`Cause` gains `Ssl`. `Monitor` + FromRow + DTOs gain the 4 add-on fields (`ssl_alert_days`/`domain_alert_days` are JSON arrays). Version-ordered runner appends `(3, "0003_certs.sql")`.

---

## 5. SSL & domain checks

### 5.1 SSL (`certcheck::ssl`)
**Target derivation** (P3 scopes to http/keyword/ssl):
- `http`/`keyword`: only if the `url` scheme is **https** — host + explicit URL port else **443**. If scheme is not https, enabling `ssl_check_enabled` is rejected at API validation (not an errored row).
- `ssl` type: `host` + `port` (default 443).

**Capture** (rustls 0.23 — `dangerous_configuration` cargo feature was REMOVED; use `.dangerous()` which is always available):
- A custom `Capturing` `ServerCertVerifier` implementing **all four** methods: `verify_server_cert` stores the presented DER chain and returns `Ok(ServerCertVerified::assertion())`; `verify_tls12_signature`/`verify_tls13_signature` delegate to `rustls::crypto::{verify_tls12_signature, verify_tls13_signature}` with `ring::default_provider().signature_verification_algorithms`; `supported_verify_schemes()` returns those. (A verifier that only Ok's `verify_server_cert` fails the handshake for real certs.)
- **SNI:** set the rustls `ServerName` to the **derived hostname** so shared-hosting/CDN vhosts return the correct cert. For a bare-IP target, use the IP `ServerName` and note `hostname_match` compares against the IP. Wrap the `tokio-rustls` connect in `timeout(timeout_seconds)`.

**Evaluate** the captured leaf (chain[0]) via `x509-parser`: `issuer`, `subject`, `not_before`/`not_after` → `valid_from`/`valid_until` epoch, `days_remaining=(valid_until-now)/86400`, SANs.
- `hostname_match` (**RFC6125**): case-insensitive; a SAN dNSName matches; wildcard is **leftmost-label only** (`*.example.com` matches `a.example.com`, NOT `example.com` nor `a.b.example.com`; no partial-label wildcards); CN fallback **only when no SANs**; trailing dots normalized.
- `chain_ok`: verify the captured chain against `webpki-roots` (Mozilla) using `WebPkiServerVerifier` (or `webpki` directly) at `now`.
- `self_signed`: leaf `issuer == subject` OR chain length 1.
- `is_valid`: `now ∈ [valid_from, valid_until]` AND `chain_ok` AND `hostname_match`.
- On a transport/handshake/parse failure: `SslResult { error: Some(...), is_valid: false, .. }` — this is **distinct** from a captured-but-invalid cert (§5.3 treats them differently).

### 5.2 Domain (`certcheck::domain`)
- **Registrable domain (eTLD+1):** strip subdomain labels to the last two labels, **except** for a small hardcoded multi-label public-suffix set (`co.uk, org.uk, com.au, net.au, co.jp, co.nz, com.br, co.in, …`) where the last three are kept. (Naive but covers `api.myapp.com`→`myapp.com` and `x.example.co.uk`→`example.co.uk`; full PSL is deferred.)
- **RDAP:** `reqwest` GET `https://rdap.org/domain/{regdomain}` (Accept `application/rdap+json`, follow redirects). **200** → parse: `events[].eventAction=="expiration"` → `eventDate` (RFC3339→epoch); registrar from `entities` (role `registrar`, vCard `fn`); `name_servers` from `nameservers[].ldhName`; `status[]`. `source="rdap"`, `queryable=1`.
  - **404** (no such RDAP object / redacted) → WHOIS fallback. **429/5xx/timeout/network** → **transient**: leave the existing row untouched (do NOT set queryable=0), retry next cadence.
- **WHOIS fallback:** TCP `whois.iana.org:43`, send `{regdomain}\r\n`, read the `refer:`/`whois:` referral server; connect it:43, send the domain; **label-pinned** expiry parse in priority order: `Registry Expiry Date:`, `Registrar Registration Expiration Date:`, `Expiry Date:`, `paid-till:`, `Expiration Date:` — parse several date formats (RFC3339, `YYYY-MM-DD`, `DD-MMM-YYYY`). registrar/nameservers best-effort. `source="whois"`.
- If neither yields an expiry (redacted/rate-limited TLD, or referral had no server) → `queryable=0` (definitive unknown; UI shows "not queryable"; no alert). Refresh no more than once/`domain_interval` (registries dislike frequent WHOIS).

### 5.3 Alerts (`certcheck::alerts`, pure + persisted state) — evaluated after each refresh
- **Tiered, most-urgent-first (fire-once):** given `days_remaining` and the sorted `alert_days`, the target threshold is the **smallest** `T` with `days_remaining <= T` and (`alerted_days IS NULL` OR `T < alerted_days`). If one exists → fire `ssl_expiring`/`domain_expiring`, set `alerted_days = T` (so smaller thresholds still fire later; larger are implied). **Renewal reset:** if `days_remaining > alerted_days` (cert/domain renewed) → `alerted_days = NULL`.
- **`ssl_invalid` (immediate, distinct state):** fires only when a cert was **captured but is invalid** (expired / hostname-mismatch / chain-broken / self-signed-failing) AND `invalid_alerted == 0` → fire, set `invalid_alerted = 1`. A pure **transport/handshake error** (`error` set, no cert) does **not** fire `ssl_invalid` (ambiguous with connectivity — the uptime monitor's `down` covers a truly-down host). Reset `invalid_alerted = 0` on `is_valid` false→true.
- **`ssl`-type monitors:** the probe already drives up/down (`down` incident, `Cause::Ssl`) which is the invalidity alert owner — so `cert_scheduler` fires **`ssl_expiring` (tiered) only** for `type='ssl'`, **not** `ssl_invalid` (avoids double-alerting the same bad cert). For `http`/`keyword` + `ssl_check_enabled`, `cert_scheduler` owns both `ssl_expiring` and `ssl_invalid`.
- All alerts are anchor-gated (§3.3) and go through `notify::send_alert` → `deliver`.

---

## 6. Notification channels (multi-type)

`notification_channels.type` ∈ `email | webhook | discord | ntfy`. `config` JSON per type:
- **email:** `{host,port,security,from,username?,to[]}`. Password via Docker secret. **SMTP auth uses `Credentials::new(username ?? from, password)`** (was hardcoded `from`) — fixes SendGrid (`apikey`), Mailgun (`postmaster@…`), Gmail-alias. Backward-compatible (absent username → from). `EmailChannelConfig` gains `username: Option<String>`; the channel-manager email form gains an optional "SMTP username" field.
- **webhook:** `{url, method?(POST), headers?, template?}`. `template` is a JSON string with `{{var}}` placeholders substituted (values JSON-escaped); default `{"monitor":"{{monitor_name}}","trigger":"{{trigger}}","status":"{{status}}","message":"{{body}}"}`. POST content-type application/json.
- **discord:** `{webhook_url}`. POST `{content: subject, embeds:[{title: subject, description: body, color: <status color int>}]}`.
- **ntfy:** `{server?(https://ntfy.sh), topic, priority?, token?}`. POST `{server}/{topic}` body=message, headers `Title`, `Priority`(1-5), `Tags`; `Authorization: Bearer {token}` when set.

The `HttpSender` trait (`send(channel_type, config, msg: &NotifyMsg) -> Result`) wraps reqwest for webhook/discord/ntfy; a `RecordingHttpSender` double records sends for tests. Cooldown per `(monitor, channel, trigger)` (§3.4). **Message variables** extend §7: `{{ssl_days}} {{ssl_valid_until}} {{ssl_issuer}} {{domain_days}} {{domain_expiry}} {{registrar}}` for the cert/domain triggers.

---

## 7. API additions

```
SSL/Domain  GET  /api/monitors/:id/ssl        -> ssl_certs row (or null)
            GET  /api/monitors/:id/domain     -> domain_info row (or null)
            POST /api/monitors/:id/refresh-ssl      -> run ssl::check now, persist + eval, return row
            POST /api/monitors/:id/refresh-domain   -> run domain::check now, persist + eval, return row
Channels    (existing CRUD; config type-specific; POST /channels/:id/test routes by channel.type)
```
Monitor create/update accept `ssl_check_enabled`, `ssl_alert_days`, `domain_check_enabled`, `domain_alert_days`, and `type:"ssl"`. **Validation:** enabling `ssl_check_enabled` on an `http`/`keyword` monitor requires an `https://` url; `ssl` type requires `host`.

---

## 8. Frontend

- **SSL card** (`SslCard.tsx`, gated on `ssl_check_enabled OR type=='ssl'`): issuer, subject, valid-until, a **days-remaining ring** (green >30, amber 7–30, red <7/invalid), chain/hostname/self-signed pills, Refresh (`refresh-ssl`). Shows the `error` string if the check failed. Inline SVG ring.
- **Domain card** (`DomainCard.tsx`, gated on `domain_check_enabled`): registrar, expiry, **days-remaining ring** (green >45, amber 7–45, red <7 — aligned to `domain_alert_days`), nameservers, registry-lock (status_codes) — or an explicit **"not queryable — registry not queryable"** note when `queryable=0`. Refresh.
- **SSE live-update:** `applyEvent` handles a new `cert_updated`/`domain_updated` event → refetch the affected card (so a scheduled refresh updates an open panel).
- **Monitor form:** SSL/domain toggle checkboxes + alert-day chip editors (comma list ↔ JSON array); type selector gains `ssl` (host+port fields); SSL toggle only enabled for https urls / ssl type.
- **Channel manager (Settings):** a channel **type** selector (email/webhook/discord/ntfy); type-specific config fields (email adds optional **SMTP username**; webhook: url/method/headers/template; discord: webhook url; ntfy: server/topic/priority/token); trigger checkboxes gain `ssl_expiring`/`ssl_invalid`/`domain_expiring`; **Send test** routes to the channel's sender.

Frontend keeps the pure `applyEvent` reducer + navy tokens; rings are inline SVG (no new dep).

---

## 9. Testing

- **Pure/unit (TDD):** RFC6125 hostname-match (SAN/CN/wildcard positive+negative vectors); tiered fire-once (most-urgent-first, renewal reset) for ssl+domain; `invalid_alerted` separation; RDAP JSON expiry/registrar parse (fixture); WHOIS label-pinned + multi-format date parse; eTLD+1 extraction (`api.myapp.com`, `x.example.co.uk`); channel-type routing; webhook/discord/ntfy payload formatting; `Cause::Ssl`; SMTP `username ?? from`.
- **certcheck integration:** `ssl::check` against a locally-served TLS endpoint (self-signed) → self_signed + hostname logic + parsed fields, chain_ok=false; against `one.one.one.one:443` (network-gated, tolerant) → is_valid, days>0, SNI correct. domain RDAP via captured fixture; live smoke optional.
- **Notify:** each sender formats + posts to the injected `RecordingHttpSender`; `deliver` routes by channel type; **two channels on one trigger both fire** (cooldown-per-channel); email path unregressed; SMTP username test.
- **API:** ssl/domain get+refresh shapes; create an `ssl` monitor; add-on fields persist; https-required validation; channel CRUD with each type.
- **DB:** `0003` on a P1/P2 DB (version-ordered, only 0003 runs, data preserved) + fresh; cascade.
- **Frontend:** SslCard ring/pills/error; DomainCard not-queryable; channel-manager type fields; form ssl/domain toggles; cert SSE live-update.
- **Acceptance:** enable SSL on a live https monitor (`one.one.one.one`) → cert captured + days shown; a domain monitor (`myapp.com`) → RDAP expiry (or explicit unknown); webhook channel → local catcher receives; **live Mailgun email** (username≠from) → provider accepts; Docker healthy; `0003` on a real P1/P2 DB.

---

## 10. Decisions log (autonomous + review-hardened)

| # | Decision | Choice |
|---|---|---|
| 1 | SSL capture | rustls `.dangerous()` (no `dangerous_configuration` feature) capturing verifier implementing ALL 4 methods; explicit `ring` provider; SNI = target host. |
| 2 | tokio-rustls | pinned `default-features=false, features=["ring","tls12"]` + `builder_with_provider(ring)` — avoids the aws-lc dual-provider panic. CI asserts no aws-lc/openssl in lock. |
| 3 | Chain roots | `webpki-roots="1"` + transitive `rustls-webpki 0.103`. |
| 4 | Domain | eTLD+1 (naive multi-label suffix set); RDAP-first; WHOIS label-pinned fallback; transient failure ≠ queryable=0. |
| 5 | Cadence | `cert_scheduler`; settings `cert.ssl_interval_seconds`=43200, `cert.domain_interval_seconds`=86400, `cert.tick_seconds`=60, `cert.concurrency`=5. **Anchor-gated.** |
| 6 | Channels | webhook/discord/ntfy (+ email username). Slack/Telegram/Pushover → P4. `HttpSender` seam + recording double. |
| 7 | Notify core | extract `deliver()`; cooldown per `(monitor, channel_id, trigger)` (fixes P1 single-channel latent bug); `send_alert` + `AlertCtx` for cert triggers. |
| 8 | Tiered alerts | **most-urgent-first** (smallest crossed-unalerted T); `alerted_days` for day-thresholds; separate `invalid_alerted` for `ssl_invalid`; renewal resets. |
| 9 | ssl type | implies SSL tracking (scheduler/card treat `type='ssl' OR ssl_check_enabled`); probe persists ssl_certs + drives up/down (`Cause::Ssl`); scheduler fires only `ssl_expiring` for ssl-type (not `ssl_invalid`). |
| 10 | New triggers | `ssl_expiring`, `ssl_invalid`, `domain_expiring`. |
| 11 | SMTP username (user) | optional `username` in email config; auth uses `username ?? from`. Live Mailgun acceptance test. |
| 12 | SSE | `cert_scheduler` emits a cert/domain-updated event; `applyEvent` refetches the card. |

---

## 11. Build order

1. Migration `0003` + `models` add-on fields/FromRow/DTOs + `Cause::Ssl` (+ **`engine.rs` match arm**) + validation (https-required for ssl add-on; `ssl` type). **(TDD** 0003-on-P2-DB, validation.)
2. **Notify refactor**: extract `deliver()` with per-`(monitor,channel,trigger)` cooldown + type routing; add `HttpSender` (+`RecordingHttpSender`) to AppState; webhook/discord/ntfy senders; `Trigger` variants + templates; SMTP `username`. **(TDD** two-channels-both-fire, per-type payloads, email+username regression.)
3. `certcheck::ssl` — pinned tokio-rustls, capturing 4-method verifier, SNI, x509 parse, RFC6125 hostname, chain_ok, is_valid **(TDD** hostname vectors, days; integration vs local self-signed + live).
4. `certcheck::domain` — eTLD+1 + RDAP parse + WHOIS label-pinned + transient-vs-unknown **(TDD** fixtures, eTLD+1).
5. `certcheck::alerts` — most-urgent-first tiered + `invalid_alerted` + renewal reset (ssl+domain) **(TDD).**
6. `probe::run` `"ssl"` arm (persist ssl_certs, up/down) + worker **(TDD).**
7. `cert_scheduler` (settings cadence, semaphore, anchor-gated, startup catch-up, SSE emit) + `send_alert` wiring + refresh-on-demand.
8. API: ssl/domain get + refresh endpoints (routes registered in api/mod.rs) **(TDD).**
9. Frontend: SslCard (ring/pills/refresh) + `/ssl` + SSE live-update.
10. Frontend: DomainCard (ring/nameservers/not-queryable) + `/domain`.
11. Frontend: MonitorForm SSL/domain toggles + alert chips + `ssl` type.
12. Frontend: Channel manager type selector + webhook/discord/ntfy config + SMTP username + new triggers + test.
13. Acceptance (ssl live, domain RDAP, webhook catcher, **live Mailgun username≠from**, `0003` on real P1/P2 DB) + final review.

---

## 12. Non-goals reminder
Heartbeat receiver, maintenance windows, monthly reports (P4). Full PSL, OCSP/CRL, CT, port-monitor SSL — out of scope for P3.

## 13. Changelog — v2 (spec-review hardening)
**Must-fix:** cooldown keyed per-`(monitor,channel,trigger)` (was single-channel) §3.4; `tokio-rustls` ring-pinned + explicit provider (aws-lc panic) §3.1; tiered alerts **most-urgent-first** §5.3; notify core extracted + Trigger/AlertCtx/templates for cert triggers §3.4; `ssl` type coherent (implies tracking, persists, single alert owner) §3.5/§5.3; domain **eTLD+1** (subdomains) §5.2; `alerted_days` vs `invalid_alerted` split §4/§5.3. **Should-fix:** `engine.rs` `Cause::Ssl` arm §3.5; `HttpSender` seam + drop `type='email'` filter §3.4; cert alerts anchor-gated §3.3; 4-method capturing verifier, no `dangerous_configuration`, webpki-roots="1" §5.1; cadence/concurrency constants+settings §3.3; WHOIS label-pinned+multi-format, no regex §5.2; RFC6125 wildcard §5.1. **Optional:** domain ring bands §8; cert SSE events §8; SSL scoped to http/keyword/ssl §2.2; non-https validation §7; RDAP transient≠unknown §5.2; sender wire formats §6. **Missed:** SNI ServerName set to target host (CDN/vhost correctness) §5.1. **Plus** the user-requested SMTP `username` §6.

*End of P3 design spec.*
