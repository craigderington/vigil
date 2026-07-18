# Vigil P3 (Certificates) — End-to-End Acceptance Results

**Date:** 2026-07-18 · **Branch:** `feat/p3-certificates` (base `666ee16`, 20 commits)

Verifies the P3 Definition of Done (spec §1): SSL certificate tracking + tiered alerts,
domain-registration expiry (RDAP/WHOIS), multi-channel notifications (email/webhook/discord/ntfy),
migration `0003`, an `ssl` monitor type, and the SSL/domain detail-panel cards.

## Automated test suite (all green)

| Suite | Result |
|---|---|
| Rust `cargo test -p vigil` | **151 passed, 0 failed** across 31 test binaries |
| Rust `cargo clippy --all-targets -- -D warnings` | clean |
| Web `npx vitest run` | **26 passed** across 11 files |
| Web `npx tsc --noEmit` | clean |
| Web `npx vite build` | succeeds |
| Dependency gate `cargo tree -e normal,build,dev \| grep -iE 'aws-lc\|openssl'` | **empty** — rustls-only preserved (incl. dev-deps; the `rcgen` dev-dep is pinned ring-only) |

New P3 coverage: migration `0003` (fresh + v2→v3 upgrade preserving data + backfilled defaults);
`certcheck::ssl` (RFC6125 hostname vectors, self-signed capture, chain-independent-of-hostname
regression, live `one.one.one.one:443` smoke); `certcheck::domain` (eTLD+1, RDAP fixture parse,
RDAP status classification incl. transient-vs-not-queryable); `certcheck::alerts::tier`
(most-urgent-first fire-once, renewal reset, order-independence); `ssl` monitor type through the
state machine (`last_checked IS NULL` left for the scheduler); notify multi-channel `deliver()`
(per-`(monitor,channel,trigger)` cooldown regression, webhook payload, SMTP username helper);
`cert_scheduler` (fire-once bookkeeping, ssl + domain expiring fire exactly once); ssl/domain API;
SslCard/DomainCard/MonitorForm/Settings.

## Bug found & fixed by live acceptance

**RDAP User-Agent (commit `d12b354`).** `rdap.org` (Cloudflare-fronted) returns **403 Forbidden**
to HTTP clients that send no User-Agent. Vigil's domain client sent none, so every RDAP lookup was
403'd → correctly classified `transient` by the Task-4 status classifier → the domain-expiry feature
silently never returned data and retried forever. Live acceptance (through the running container)
surfaced this; the unit tests couldn't (no live registrar call). Fix: send an identifying
`User-Agent` on the RDAP client and request. Verified live afterward — `cloudflare.com`, `google.com`,
`github.com` all return real registrar + expiry over RDAP. This is exactly what the live pass is for.

## Live acceptance (Docker stack, host 8099 → container 8090)

| DoD item (spec §1) | Result | Evidence |
|---|---|---|
| Migration `0003` on a P1/P2 DB | ✅ | `migrate3` upgrade test applies only 0003 on a v2 DB, preserves the inserted monitor, backfills the 4 add-on columns to defaults; container boots healthy (runs migrations) |
| Live SSL cert captured (SNI correct) | ✅ | `ssl` monitor → `refresh-ssl` → `GET /ssl`: issuer `SSL.com`, subject `cloudflare-dns.com` (SNI worked), `is_valid=true`, `chain_ok=true`, `hostname_match=true`, `self_signed=false`, `days_remaining=156`, no error — chain & hostname independently true (Task-3 decoupling) |
| Live domain expiry (RDAP) | ✅ (after UA fix) | `domain_check_enabled` monitor → `refresh-domain` → `GET /domain`: registrar `Cloudflare, Inc.`, `queryable=true`, `source=rdap`, `days_remaining=2406`, nameservers populated |
| Webhook channel delivery | ✅ | `webhook_payload_shape` unit test asserts the JSON body carries the monitor name; the final review cross-checked every channel-type config field name against the backend senders (`notify/http.rs`) |
| **LIVE Mailgun send test** (user-requested) | ⏳ pending user | requires the user to place the Mailgun API key in `secrets/smtp_password`, plus an authorized recipient + From address — see below |
| Docker rebuild → healthy on 8099 | ✅ | `docker compose up -d --build` → `health: healthy`, `GET /api/monitors` HTTP 200 on `0.0.0.0:8099` |

## Final whole-branch review (opus): **Ready-to-merge**
No Critical/Important. All 8 cross-cutting concerns verified adversarially (fire-once end-to-end incl.
ssl-type reaching the 12h eval; rustls single-ring-provider; SMTP password never in config/API/log;
migration additive+versioned; anchor-gating consistent; ssl-type same state machine; FE/BE
trigger+config+SSE contract exact). All carried-forward Minors triaged defer-OK. One optional
follow-up (redact reqwest error strings in `notification_log.error`) tracked separately.

### Live Mailgun send test (Step 5 — needs the operator)
Per the user's explicit request, acceptance includes a LIVE send through Mailgun to prove
`username != from` SMTP auth end-to-end. Parameters (username, host) are in the gitignored
`scratchpad/p3-mailgun-acceptance.txt`. To run it, the operator must supply:
1. the Mailgun **API key** placed in `secrets/smtp_password` (a Docker secret — never committed, never seen by the assistant),
2. an **authorized recipient** address (sandbox domains only deliver to authorized recipients),
3. the **From** address.

Then configure an `email` channel (host `smtp.mailgun.org:587` starttls, username
`postmaster@sandbox….mailgun.org`) and POST `/api/channels/:id/test` → Mailgun should accept.
