# Vigil P3 (Certificates) — Design Spec

> **Status:** autonomous build (user "kick off P3") · **Date:** 2026-07-18 · **Base:** P1+P2 on `master` (666ee16)
> **Scope:** Phase 3 "Certificates" per [`CLAUDE.md`](../../../CLAUDE.md) §14. Builds on P1/P2; inherits all prior decisions (containerized axum + SolidJS, rustls, SQLite/WAL, uptime-from-incidents, ports 8090/8099, version-ordered migrations, no-openssl).

Design decisions made autonomously, grounded in `CLAUDE.md` §6 (SSL/domain), §7 (channels), §9 (schema), §11.6 (SSL/domain cards). Where this doc and `CLAUDE.md` differ, this doc wins for P3.

---

## 1. P3 Definition of Done

**Expiry surprises are impossible.**

1. **SSL tracking:** a monitor with an HTTPS target (or the new `ssl` type) captures its certificate on a slow cadence — issuer, subject, valid_from/until, days_remaining, chain_ok, hostname_match, self_signed — and stores it. Tiered alerts fire once as each `ssl_alert_days` threshold is crossed, plus an immediate alert on invalid/expired/chain-broken/hostname-mismatch.
2. **Domain expiry:** a domain-tracking monitor resolves registration expiry via **RDAP** (WHOIS fallback), storing registrar, expiry_date, days_remaining, name_servers, status_codes — cached ~24h. Tiered alerts at `domain_alert_days`. Un-queryable TLDs show an explicit "unknown" state, never a false green.
3. **New channels:** webhook (generic JSON), Discord, and ntfy notification channels work — the notify layer routes by channel `type`, and the existing email path is unregressed.
4. **New triggers:** `ssl_expiring`, `ssl_invalid`, `domain_expiring` are dispatchable per monitor per channel (alongside `down`/`recovered`).
5. **UI:** the detail panel shows an SSL card (issuer/subject/valid-until, a color-graded days-remaining ring, chain/hostname pills, Refresh) and a domain card (registrar/expiry/ring/nameservers/lock, or "not queryable"). The monitor form has SSL/domain toggles + alert-day chips; Settings' channel manager creates webhook/Discord/ntfy channels with type-specific config + test.
6. Migration `0003` applies on a P1/P2 database. All new logic TDD-tested; `cargo test` + `vitest` green; Docker healthy; deps stay rustls-clean (no openssl).

---

## 2. Scope

### 2.1 In scope
- **SSL certificate check:** rustls TLS connect that **captures the peer chain even for invalid/expired certs** (a capturing verifier that accepts anything), parse the leaf with `x509-parser`, evaluate validity/chain/hostname ourselves. Store in `ssl_certs`.
- **Domain expiry:** RDAP via `https://rdap.org/domain/{domain}` (redirect-following JSON), WHOIS fallback (TCP :43), "unknown/not-queryable" state. Store in `domain_info`.
- **Slow cadence:** a dedicated `cert_scheduler` task; SSL default every 12h, domain every 24h; on-demand `refresh_ssl(id)`/`refresh_domain(id)`.
- **Monitor add-ons:** `ssl_check_enabled`, `ssl_alert_days`, `domain_check_enabled`, `domain_alert_days` columns (migration `0003`). Plus a new `ssl` monitor type (SSL-only: up ⇔ cert valid).
- **Notify refactor:** dispatch routes by channel `type` → email (existing), webhook, discord, ntfy senders. New triggers `ssl_expiring`/`ssl_invalid`/`domain_expiring`. Tiered-threshold "fire once per crossing" logic.
- **API:** `get_ssl(id)`, `get_domain(id)`, `refresh_ssl(id)`, `refresh_domain(id)`; channel CRUD already generic (type in config).
- **Frontend:** SSL card, domain card, monitor-form SSL/domain toggles + alert chips + `ssl` type, channel manager for webhook/discord/ntfy.

### 2.2 Out of scope (deferred to P4)
- Heartbeat/push monitors + axum `/ping` receiver logic (route reserved since P1), maintenance windows, monthly reports, re-notify/cooldown-beyond-P1, daily digest, Telegram/Pushover/Slack channels (webhook/discord/ntfy are the P3 §14 set; Slack is a trivial webhook variant — include only if free), DEGRADED state, the accent/theme picker.

---

## 3. Architecture deltas

1. **`certcheck` module** (`crates/vigil/src/certcheck/{mod,ssl,domain}.rs`):
   - `ssl::check(host, port) -> SslResult` — `tokio-rustls` connect with a **capturing, non-verifying** cert verifier (a `ServerCertVerifier` that stores the presented chain and returns Ok); parse leaf via `x509-parser`; compute `valid_from/until` (from cert validity), `days_remaining`, `hostname_match` (SAN/CN vs host), `chain_ok` (verify the chain against Mozilla roots via `webpki`/`webpki-roots`), `self_signed` (issuer==subject or 1-cert chain), `is_valid` (not expired && chain_ok && hostname_match). Bounded by a timeout.
   - `domain::check(domain) -> DomainResult` — RDAP first (`reqwest` GET `https://rdap.org/domain/{domain}`, follow redirects, JSON: parse `events[].eventAction=="expiration"` → expiry, `entities` registrar, `nameservers`, `status`); on RDAP failure/no-expiry, WHOIS fallback (TCP :43 to `whois.iana.org` for the referral server, then that server, text-parse "Registry Expiry Date"/"Expiry Date"). `queryable=false` when neither yields an expiry.
2. **Migration `0003`** adds the 4 monitor add-on columns + `ssl_certs` + `domain_info` tables (§4). Version-ordered runner (P2) appends `(3, 0003_certs.sql)`.
3. **`cert_scheduler`** background task (spawned in `serve`): every N minutes, find SSL-due monitors (`ssl_check_enabled` and `ssl_certs.last_checked` older than the SSL cadence, or never) and domain-due monitors; refresh under a small semaphore; after each refresh, evaluate + dispatch tiered alerts. Also a startup catch-up (refresh anything never checked).
4. **Notify refactor** (`notify::dispatch`): loop attached channels; for each, `match channel.type { "email" => email::send, "webhook" => webhook::send, "discord" => discord::send, "ntfy" => ntfy::send }`. All senders take a common `NotifyMsg { subject, body, monitor, trigger, ... }`. Email path unchanged in behavior. New senders use `reqwest` (rustls).
5. **`probe::run`** gains `"ssl" => certcheck::ssl_probe(m)` — an `ssl` monitor's ProbeOutcome is up ⇔ cert `is_valid`, cause `Cause::Ssl` (new) on failure.
6. **New deps** (rustls-clean): `tokio-rustls`, `x509-parser`, `webpki-roots` (Mozilla roots for chain verify). `rustls`/`webpki` already transitive. RDAP/WHOIS use existing `reqwest`/`tokio`.

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
  error TEXT,                          -- populated when the check itself failed
  alerted_days INTEGER,                -- lowest ssl_alert_days threshold already alerted (fire-once)
  last_checked INTEGER
);

CREATE TABLE domain_info (
  monitor_id  INTEGER PRIMARY KEY REFERENCES monitors(id) ON DELETE CASCADE,
  registrar TEXT, expiry_date INTEGER, days_remaining INTEGER,
  name_servers TEXT, status_codes TEXT,
  queryable INTEGER,                   -- 0 = registry not queryable (explicit unknown)
  source TEXT,                         -- 'rdap' | 'whois' | null
  alerted_days INTEGER,
  last_checked INTEGER
);
```

`Cause` gains `Ssl`. `Monitor` + FromRow + DTOs gain the 4 add-on fields. `ssl_alert_days`/`domain_alert_days` are JSON arrays parsed at eval time.

---

## 5. SSL & domain checks

**SSL (`certcheck::ssl`):**
- Target host/port: for `http`/`keyword`, derive from the `url` (host + port, default 443) — only if scheme is https; for `ssl` type, from `host`+`port` (default 443).
- rustls `ClientConfig` with a custom `ServerCertVerifier` (`Capturing`) that: records the presented cert DER chain, and returns `Ok(ServerCertVerified::assertion())` unconditionally (so the handshake completes even for bad certs). Wrap the connect in `timeout(timeout_seconds)`.
- Parse the leaf (chain[0]) with `x509-parser`: `issuer`, `subject`, `not_before`/`not_after` (→ `valid_from`/`valid_until` epoch), SANs.
- `days_remaining = (valid_until - now)/86400`.
- `hostname_match`: any SAN dNSName (or CN fallback) matches the target host (wildcard-aware).
- `chain_ok`: verify the captured chain against `webpki-roots` (Mozilla) with `webpki` at `now`; ok/err.
- `self_signed`: `issuer == subject` (leaf) OR chain length 1.
- `is_valid`: `now` within [valid_from, valid_until] AND `chain_ok` AND `hostname_match`.
- On any handshake/parse failure → `SslResult { error: Some(...), is_valid: false, ... }`.
- Alerts (evaluated after a check): if not valid (expired/chain-broken/hostname-mismatch/self-signed-and-verify-failed) → `ssl_invalid` (once per distinct invalid state, gated by `alerted_days = -1` sentinel). Else tiered: for each threshold T in `ssl_alert_days` (desc), if `days_remaining <= T` and `T < alerted_days` (or alerted_days null) → fire `ssl_expiring` for the largest such T, set `alerted_days = T`. Reset `alerted_days` to null on renewal (days_remaining jumps back up).

**Domain (`certcheck::domain`):**
- `domain`: the registrable domain derived from the monitor's url/host (naive: last two labels, or the host itself — P3 uses the host/url host as-is; a public-suffix list is out of scope, note it).
- RDAP: `reqwest` GET `https://rdap.org/domain/{domain}` (follow redirects, Accept: application/rdap+json), 200 → parse JSON: `events` where `eventAction == "expiration"` → `eventDate` (RFC3339 → epoch); registrar from `entities` (role registrar, vcard fn); `name_servers` from `nameservers[].ldhName`; `status` array. `source="rdap"`.
- Fallback WHOIS: TCP connect `whois.iana.org:43`, send `{domain}\r\n`, read referral (`refer:` / `whois:` line) → connect that server:43, send domain, text-parse a line matching `(?i)(registry )?expir` with a date; registrar/nameservers best-effort. `source="whois"`.
- If neither yields an expiry → `queryable=0` (explicit unknown; no alert, UI shows "not queryable").
- Alerts: tiered like SSL over `domain_alert_days` → `domain_expiring`.
- Cache: refresh no more than once/24h (registries dislike frequent WHOIS).

---

## 6. Notification channels (refactor to multi-type)

`notification_channels.type` ∈ `email | webhook | discord | ntfy`. `config` JSON per type:
- email: `{host,port,security,from,to[]}` (unchanged, password via Docker secret).
- webhook: `{url, method, headers?, template?}` — POST a JSON body (default template with `{{...}}` variables); custom headers.
- discord: `{webhook_url}` — POST Discord's `{content, embeds}` preset.
- ntfy: `{server?, topic, priority?}` — POST to `{server||https://ntfy.sh}/{topic}` with title/message headers.

`notify::dispatch::on_transition(state, monitor, trigger, incident_id?)` (extended to also handle cert/domain triggers via a similar `notify::send_alert(state, monitor, trigger, ctx)` entry) loops the monitor's attached active channels whose `triggers` include the event, applies the P1 cooldown, then routes by `channel.type` to the matching sender, building a `NotifyMsg` from a shared template (`notify::templates::render(trigger, ctx)` extended for the new triggers). Each sender maps `NotifyMsg` → its wire format. The email `Transport` seam stays; webhook/discord/ntfy get their own `reqwest`-based senders (injectable for tests via a `HttpSender` trait double, mirroring the email `Transport` double). `notification_log` records all sends.

**Message variables** extend §7: `{{ssl_days}} {{ssl_valid_until}} {{ssl_issuer}} {{domain_days}} {{domain_expiry}} {{registrar}}` for the cert triggers.

---

## 7. API additions

```
SSL/Domain  GET  /api/monitors/:id/ssl      -> ssl_certs row (or null) as JSON
            GET  /api/monitors/:id/domain   -> domain_info row (or null) as JSON
            POST /api/monitors/:id/refresh-ssl     -> run check now, return the row
            POST /api/monitors/:id/refresh-domain  -> run check now, return the row
Channels    (existing CRUD; config now type-specific; test routes to the type's sender)
```
Monitor create/update accept `ssl_check_enabled`, `ssl_alert_days`, `domain_check_enabled`, `domain_alert_days`, and `type:"ssl"`.

---

## 8. Frontend

- **SSL card** (`SslCard.tsx`, detail panel §11.6.6): issuer, subject, valid-until, a **days-remaining ring** (green >30, amber 7–30, red <7/invalid), chain/hostname/self-signed status pills, a **Refresh** button (`refresh-ssl`). Shows the `error` string if the check failed. Only rendered when `ssl_check_enabled`.
- **Domain card** (`DomainCard.tsx`, §11.6.7): registrar, expiry date, days-remaining ring, nameservers, registry-lock (status_codes) — or an explicit **"not queryable — registry not queryable"** note when `queryable=0`. Refresh button. Only when `domain_check_enabled`.
- **Monitor form:** SSL/domain toggle checkboxes + alert-day chip editors (comma list → JSON array); the type selector gains `ssl` (host + port fields, like port). For `http`/`keyword` with an https url, the toggles enable the add-ons.
- **Channel manager (Settings):** the channel-create form gains a **type** selector (email/webhook/discord/ntfy); type-specific config fields (webhook: url/method/headers/template; discord: webhook url; ntfy: server/topic/priority); the trigger checkboxes gain `ssl_expiring`/`ssl_invalid`/`domain_expiring`; **Send test** routes to the channel's sender.
- Detail panel wires SslCard/DomainCard below the existing cards.

Frontend keeps the pure `applyEvent` reducer + navy tokens. Rings are inline SVG (no new dep).

---

## 9. Testing

- **Pure/unit (TDD):** SSL hostname-match (SAN/CN/wildcard), days-remaining + tiered-threshold "fire-once" crossing logic (ssl + domain), RDAP JSON expiry/registrar parse, WHOIS text expiry parse, `Cause::Ssl`, channel-type routing, webhook/discord/ntfy payload formatting.
- **certcheck integration:** `ssl::check` against a locally-served TLS endpoint with a known (self-signed) cert → asserts self_signed + hostname logic + parsed fields; against `one.one.one.one:443` (real, valid) → is_valid true, days_remaining>0 (network-gated, tolerant). domain RDAP parse tested with a captured JSON fixture (network-free); a live smoke optional.
- **Notify:** each sender formats correctly and posts to an injected HTTP double; dispatch routes by channel type; email path still green.
- **API:** ssl/domain get + refresh shapes; create an ssl monitor; monitor add-on fields persist; channel CRUD with webhook/discord/ntfy config.
- **DB:** `0003` applies on a P1/P2 DB (version-ordered) + fresh; cascade.
- **Frontend:** SslCard ring/pills from data; DomainCard not-queryable state; channel-manager type-specific fields; form ssl/domain toggles.
- **Acceptance:** enable SSL on an https monitor (e.g. one.one.one.one) → cert captured, days_remaining shown; a domain monitor → RDAP expiry (or unknown); a webhook channel → test hits a local catcher; Docker healthy; `0003` on a real P1/P2 DB.

---

## 10. Decisions log (autonomous)

| # | Decision | Choice |
|---|---|---|
| 1 | SSL cert capture | rustls capturing/non-verifying verifier → parse with `x509-parser`; evaluate validity/chain/hostname ourselves (so expired/invalid certs are still reported). |
| 2 | Chain verify roots | `webpki-roots` (Mozilla, bundled, no openssl). |
| 3 | Domain expiry | RDAP-first via `rdap.org` redirector; WHOIS (:43) fallback; explicit "not queryable" when neither. Public-suffix derivation is naive (host as-is) — noted. |
| 4 | Cadence | dedicated `cert_scheduler`; SSL 12h, domain 24h; on-demand refresh endpoints; startup catch-up. |
| 5 | New channels | webhook / discord / ntfy (§14 P3 set). Slack = a webhook variant, include only if trivial. Telegram/Pushover deferred to P4. |
| 6 | Notify refactor | dispatch routes by `channel.type`; senders share `NotifyMsg`; email seam unchanged; new senders reqwest+injectable double. |
| 7 | ssl monitor type | added (`ssl`): up ⇔ cert `is_valid`; `Cause::Ssl`. |
| 8 | Tiered alerts | fire-once per crossing via `alerted_days` (lowest threshold alerted); reset on renewal; `ssl_invalid`/`domain_expiring` immediate/tiered. |
| 9 | New deps | `tokio-rustls`, `x509-parser`, `webpki-roots` — all rustls-clean. |
| 10 | New triggers | `ssl_expiring`, `ssl_invalid`, `domain_expiring`. |

---

## 11. Build order

1. Migration `0003` + `models` add-on fields/FromRow/DTOs + `Cause::Ssl` + validation for `ssl` type. **(TDD** 0003-on-P2-DB.)
2. `certcheck::ssl` — capturing verifier + x509 parse + hostname/chain/validity **(TDD** hostname-match, days; integration vs local self-signed + live).
3. `certcheck::domain` — RDAP parse + WHOIS fallback + not-queryable **(TDD** RDAP/WHOIS parse fixtures).
4. Tiered-alert crossing logic (`certcheck::alerts` pure) + `alerted_days` fire-once **(TDD).**
5. Notify refactor: `NotifyMsg`, channel-type routing, webhook/discord/ntfy senders (injectable `HttpSender`), templates for new triggers **(TDD** routing + payloads; email regression).
6. `probe::run` `"ssl"` arm + worker (ssl monitor up/down from cert) **(TDD).**
7. `cert_scheduler` (slow cadence + startup catch-up) + refresh-on-demand; wire alerts.
8. API: ssl/domain get + refresh endpoints **(TDD).**
9. Frontend: SslCard (ring/pills/refresh) + `/ssl` wiring.
10. Frontend: DomainCard (ring/nameservers/not-queryable) + `/domain`.
11. Frontend: MonitorForm SSL/domain toggles + alert chips + `ssl` type.
12. Frontend: Channel manager type selector + webhook/discord/ntfy config + new triggers + test.
13. Acceptance (ssl on a live https monitor, domain RDAP, webhook to a local catcher; `0003` on a real P1/P2 DB) + final review.

---

## 12. Non-goals reminder
Heartbeat receiver, maintenance windows, monthly reports (P4). Public-suffix-correct domain extraction, OCSP/CRL revocation checking, and Certificate Transparency are out of scope for P3.

*End of P3 design spec.*
