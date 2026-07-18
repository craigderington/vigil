# Vigil P1 — End-to-End Acceptance Results

**Date:** 2026-07-17 · **Branch:** `feat/p1-mvp`

Driven against the **real `vigil` binary** with a real local SMTP catcher (127.0.0.1:2525) and a
local HTTP target (127.0.0.1:8123), exercising every P1 Definition-of-Done item from spec §1.
All checks **PASS**. Harness: `scratchpad/e2e.sh` + `scratchpad/smtp_catcher.py`.

| DoD item (spec §1) | Result | Evidence |
|---|---|---|
| 1. `docker compose up -d` → healthy; dashboard loads | ✅ | Docker: `docker compose ps` → `(healthy)` (Task 20, real build); `/healthz` → `ok` |
| 2. Add/edit/pause/resume/delete/re-check a monitor | ✅ | CRUD via API + UI; e2e created + attached a monitor |
| 3. Reachable site reads UP; response/last-checked update live (SSE) | ✅ | monitor → `up` after a scheduled check; SSE snapshot/deltas verified |
| 4. Target down → **DOWN only after confirmation** → incident + one DOWN email | ✅ | stopped target → `down` after threshold; incident count 1; **DOWN email** captured (subj "🔴 Target is DOWN") |
| 5. **Your** internet down → **UNKNOWN**, no false down-email; resumes on return | ✅ | anchors set unreachable → monitor `unknown` (fleet-wide via reactor); failing check during offline did **not** go DOWN; **no false email** (count 3==3); restored anchors |
| 6. Recovery (incl. DOWN→UNKNOWN→UP) → recovered email; incident closed | ✅ | target restored → `up`; **recovered email** captured (subj "✅ Target recovered") |
| 7. State survives `docker compose restart` | ✅ | killed + restarted vigil (same DB): monitors persisted (1→1), incidents persisted (count 1) |

**Emails captured (real lettre SMTP send):** 3 — `Vigil test email`, `🔴 Target is DOWN`, `✅ Target recovered`.

## Bug found & fixed during acceptance

- **Channel config double-encoding** (`crates/vigil/src/api/channels.rs`): the create/update handlers
  re-encoded the channel `config` (`Value::to_string()` on an already-JSON-string field), so
  `channels::test` and `notify::dispatch` failed to parse it (`expected struct EmailChannelConfig`) —
  **email alerting was silently broken through the UI.** Fixed to store `config` verbatim; the Settings UI
  now sends a JSON string consistently; added the `channel_config_stored_verbatim_not_double_encoded`
  regression test. Commit `f164541`. Unit tests had missed it because the notify test seeded config
  straight into the DB, bypassing the create endpoint — exactly what an end-to-end pass is for.

## Automated test suite

- Rust: **61 tests** pass (`cargo test -p vigil`), `cargo clippy --all-targets -- -D warnings` clean.
- Web: **6 tests** pass (`vitest`), `tsc --noEmit` + `vite build` clean.
- Docker: multi-stage build → **healthy** container, verified twice (Task 20 + its fix).

**P1 is complete and verified.**
