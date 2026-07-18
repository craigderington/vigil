# Vigil P2 (Signal) — End-to-End Acceptance Results

**Date:** 2026-07-18 · **Branch:** `feat/p2-signal`

Driven against the **real `vigil` binary** with local targets (an HTTP server / TCP listener on :8123)
and a live DNS name, exercising every P2 Definition-of-Done item. All checks **PASS**.
Harness: `scratchpad/p2_accept.sh`.

| DoD item (spec §1) | Result | Evidence |
|---|---|---|
| 7. `0002` applies on a P1 database (version-ordered runner) | ✅ | `schema_migrations` MAX version = 2 after boot; the `migration upgrades_v1_db_applies_only_0002_and_preserves_data` unit test proves data is preserved on upgrade |
| 1. New monitor types work end-to-end | ✅ | Created http, keyword, port, ping, dns monitors; http/keyword/port/ping all read **UP** against the local target, **dns UP** (resolved `one.one.one.one` A via hickory). **keyword monitor DOWN when the keyword is absent** (the `Cause::Keyword` path). |
| 4. Daily rollups | ✅ | Injected backdated checks + a resolved incident for yesterday; `GET /bars` lazily rolled up (via `ensure_aggregates`) → **2 days `has_data`, 1 day with downtime** reflecting the incident (overlap-clipped). |
| 3. 90-day uptime bars | ✅ | `/bars?days=90` returns per-day `{uptime_pct, incidents, down_seconds, has_data}`; frontend `UptimeBar` color-bands them (has_data-gated). |
| 2. Response-time chart | ✅ | `/series?range=7d` returns bucketed points; `ResponseChart` (uPlot, jsdom-guarded) renders them with incident shading. |
| — 30d/90d stats | ✅ | `/stats?range=30d` → `avg_ms = 90.625` (count-weighted from aggregates, no double-count — proven by the exact-value unit test). |
| 5. Incident history + acknowledge | ✅ | `/api/incidents?monitor_id=` lists the incident with `monitor_name`; `POST /incidents/:id/acknowledge` flips `acknowledged` to true. Frontend timeline + global Incidents screen. |
| 6. List view | ✅ | `ListView` dense table + grid⇄list toggle (unit-tested sort); the compact 90-day bar renders per row. |

## Automated test suite

- Rust: all 24 test binaries pass (`cargo test -p vigil`), `cargo clippy --all-targets -- -D warnings` clean. New coverage: version-ordered migration + P1→P2 upgrade, TCP/DNS/keyword probers, rollup overlap-uptime + idempotency, stats/series/bars, incidents API.
- Web: 15 tests pass (`vitest`), `tsc --noEmit` + `vite build` clean. New: UptimeBar color-bands, ResponseChart jsdom guard, IncidentTimeline acknowledge-refetch, Incidents screen, ListView sort, MonitorForm type selector.

## New dependencies (both rustls-clean, no openssl)
- `hickory-resolver` (DNS), `chrono` (std-only, UTC day math), `uplot` (bundled frontend chart).

**P2 is complete and verified.**
