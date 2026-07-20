# Vigil P4.5 — Backup Export / Import — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add one-click full-DB backup **export** (download) and **atomic live import** (upload & replace, no restart) to Vigil, surfaced as a "Backup & restore" section in Settings.

**Architecture:** Export runs SQLite `VACUUM INTO` to a temp file beside the live DB (WAL-safe, no process stop) and streams it as an `attachment`. Import validates the uploaded SQLite file, upgrades an older backup via the existing migration runner, writes a pre-import safety snapshot, then replaces every table in one deferred-FK transaction over the same pool — and signals the in-memory probe scheduler to re-seed via a new `SchedCmd::Reseed`. Other background loops re-query the DB each tick and self-adapt.

**Tech Stack:** Rust (axum 0.7, sqlx 0.8 SQLite/rustls, tokio) backend; SolidJS + Vite frontend. All deps already in-tree (`rand`, `tokio::fs`, `tempfile`).

**Spec:** `docs/superpowers/specs/2026-07-20-vigil-p4-backup-export-import-design.md` (read it first).

## Global Constraints

- **No new crates** — `rand 0.8`, `tokio` (full), `tempfile 3`, `chrono`, `sqlx 0.8` are all present. Do not add dependencies.
- **No new migration, no new `settings` keys, no new SSE `Event` variant** — backup operates over existing tables; post-import UI refresh is a client-side `location.reload()`.
- **rustls only** (no native-tls). SQLite **WAL** mode; `foreign_keys=ON`.
- **`/data` is the only app-writable dir** (container uid 10001). All temp/export/snapshot files go **beside the live DB** (`parent(state.db_path)`).
- **Secrets:** SMTP password is external (Docker secret / env), never in the DB. The export **includes** DB-plaintext secrets (webhook headers, `webhook_url`, ntfy token, `inline:` auth) by design; the UI flags the download sensitive.
- **Import route body limit = 1 GiB** (`DefaultBodyLimit::max`), overriding axum's 2 MB global default. Applied to the import route only.
- **Backend tests:** `cargo test -p vigil -- --test-threads=1`, rustls-only. **Frontend:** `cd web && npx vitest run`; `npm run build` (tsc) must stay clean.
- **Ports** unchanged (8090 internal / 8099 host). No auth (single trusted operator).
- Follow existing conventions: `ApiResult<T> = Result<Json<T>, (StatusCode, String)>`, `super::{now, db_err}`, DTOs `#[derive(Deserialize)]`, handlers in `src/api/<resource>.rs`, integration tests via `tests/api.rs`'s `serve()` on an ephemeral port hit with `reqwest`.

---

## File Structure

- **Create:** `crates/vigil/src/api/backup.rs` — `export` / `import` / `info` handlers + `ImportResult` / `BackupInfo` structs.
- **Create:** `crates/vigil/tests/api_backup.rs` — HTTP round-trip integration tests.
- **Create:** `web/src/__tests__/backup.test.tsx` — Backup Settings-section tests.
- **Modify:** `crates/vigil/src/app.rs` — add `AppState.db_path: Arc<str>`; add `SchedCmd::Reseed`.
- **Modify:** `crates/vigil/src/scheduler.rs` — `SchedState::clear_schedule()`; handle `SchedCmd::Reseed`.
- **Modify:** `crates/vigil/src/db.rs` — `pub fn current_schema_version()`.
- **Modify:** `crates/vigil/src/main.rs` — set `db_path` on `AppState`.
- **Modify:** `crates/vigil/tests/common/mod.rs` — thread the tempdir DB path into the 3 `AppState` constructors.
- **Modify:** `crates/vigil/src/api/mod.rs` — `pub mod backup;` + 3 routes + the import body-limit layer.
- **Modify:** `web/src/api.ts` — `getBackupInfo`, `importBackup`, `BackupInfo`/`ImportResult` interfaces.
- **Modify:** `web/src/components/Settings.tsx` — the "Backup & restore" section.
- **Modify:** `README.md` — note the in-app backup path (Task 6).

---

## Task 1: Backend plumbing — `db_path`, `SchedCmd::Reseed`, schema version

**Files:**
- Modify: `crates/vigil/src/app.rs`
- Modify: `crates/vigil/src/scheduler.rs`
- Modify: `crates/vigil/src/db.rs`
- Modify: `crates/vigil/src/main.rs`
- Modify: `crates/vigil/tests/common/mod.rs`

**Interfaces:**
- Produces: `AppState.db_path: std::sync::Arc<str>`; `SchedCmd::Reseed` (unit variant); `SchedState::clear_schedule(&mut self)`; `db::current_schema_version() -> i64`.

- [ ] **Step 1: Write the failing unit tests**

Append to the `#[cfg(test)] mod tests` block in `crates/vigil/src/scheduler.rs` (after `remove_does_not_clear_in_flight_no_double_fire`):

```rust
    #[test]
    fn clear_schedule_empties_heap_keeps_inflight() {
        let mut s = SchedState::new();
        s.schedule(1, 0);
        assert_eq!(s.take_due(10), Some(1)); // id 1 now in-flight
        s.schedule(2, 0); // a second, not-yet-fired entry
        s.clear_schedule(); // e.g. a DB import landed
        assert_eq!(s.take_due(10), None, "heap cleared: nothing left to fire");
        // in_flight preserved: re-seeding id 1 must NOT hand it out again until Complete
        s.schedule(1, 0);
        assert_eq!(s.take_due(10), None, "in-flight id 1 must not double-fire after clear+reseed");
        s.complete(1);
        s.schedule(1, 0);
        assert_eq!(s.take_due(10), Some(1), "after Complete it can run again");
    }
```

Add to `crates/vigil/src/db.rs` a new `#[cfg(test)] mod tests` block at the end of the file:

```rust
#[cfg(test)]
mod tests {
    #[test]
    fn current_schema_version_is_latest_migration() {
        assert_eq!(super::current_schema_version(), 6);
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p vigil clear_schedule_empties_heap_keeps_inflight current_schema_version_is_latest_migration -- --test-threads=1`
Expected: FAIL — `no function or associated item named clear_schedule` / `cannot find function current_schema_version`.

- [ ] **Step 3: Add `current_schema_version` to `db.rs`**

In `crates/vigil/src/db.rs`, after the `MIGRATIONS` const (line 16), add:

```rust
/// The highest migration version this build knows about — the schema version
/// a fresh `connect()` brings a DB to. Used by the backup import to gate
/// version-mismatched uploads and to report the current version.
pub fn current_schema_version() -> i64 {
    MIGRATIONS.last().map(|(v, _)| *v).unwrap_or(0)
}
```

- [ ] **Step 4: Add `SchedCmd::Reseed` and `AppState.db_path` in `app.rs`**

In `crates/vigil/src/app.rs`, add the field to `AppState` (after `anchor`, line 15):

```rust
    pub anchor: Arc<crate::anchor::AnchorGate>,
    /// Filesystem path of the live SQLite DB (e.g. `/data/vigil.db`). Backup
    /// export/import derive their temp + snapshot dir from this path's parent.
    pub db_path: Arc<str>,
```

Add the `Reseed` variant to `SchedCmd` (after `Complete(i64)`, line 29):

```rust
    Complete(i64),
    /// Full DB was replaced (backup import): drop the in-memory schedule and
    /// re-seed it from the new `monitors` rows. Preserves in-flight guards.
    Reseed,
```

- [ ] **Step 5: Add `clear_schedule` and handle `Reseed` in `scheduler.rs`**

In `crates/vigil/src/scheduler.rs`, add a method to the `impl SchedState` block (after `remove`, around line 123):

```rust
    /// Drop every scheduled heap entry but KEEP the `in_flight` set — used by
    /// `SchedCmd::Reseed` after a backup import replaces the DB. Preserving
    /// in-flight guards means a worker mid-probe when the import landed still
    /// can't double-fire: `catch_up` re-seeds its id, but `take_due` discards
    /// that entry while the id remains in-flight (cleared only by `Complete`).
    pub fn clear_schedule(&mut self) {
        self.heap.clear();
    }
```

Add a match arm in `run_scheduler`'s `rx.recv()` block (after the `SchedCmd::Remove` arm, around line 244):

```rust
                    Some(SchedCmd::Remove(id)) => {
                        sched.remove(id);
                    }
                    Some(SchedCmd::Reseed) => {
                        sched.clear_schedule();
                        catch_up(&state.db, &mut sched).await;
                    }
```

- [ ] **Step 6: Set `db_path` on `AppState` in `main.rs`**

In `crates/vigil/src/main.rs::serve`, in the `AppState` literal (after `anchor: anchor.clone(),`, line 78):

```rust
        anchor: anchor.clone(),
        db_path: cfg.db_path.clone().into(),
```

- [ ] **Step 7: Thread `db_path` through the test harness**

> **Do NOT change `fresh_pool`'s signature.** It returns `(pool, TempDir)` and is called by
> `let (pool, _d) = fresh_pool().await;` in **six** places across *other* test files
> (`tests/rollup.rs:6` & `:33`, `tests/maintenance.rs:6`, `tests/settings.rs:6`,
> `tests/settings_p43.rs:7` & `:31`). Widening it to a 3-tuple breaks all of them (E0308) and
> fails the workspace test build. Instead, derive `db_path` **inside** each constructor from the
> same path `fresh_pool` opened the pool on (`dir.path().join("t.db")`).

In `crates/vigil/tests/common/mod.rs`, leave `fresh_pool` **unchanged**. In **each** of the three
constructors (`test_state`, `test_state_offline`, `test_state_failing_transport`), add one line
after the `fresh_pool()` call and one field to the `AppState` literal. For `test_state`:

```rust
pub async fn test_state() -> TestEnv {
    let (pool, dir) = fresh_pool().await;
    let db_path = dir.path().join("t.db").to_str().unwrap().to_string();
    // ...unchanged: sent, sent_http, bus, tx/rx, anchor...
    let state = AppState {
        db: pool,
        bus,
        transport: Arc::new(RecordingTransport { sent: sent.clone() }),
        http_sender: Arc::new(RecordingHttpSender { sent_http: sent_http.clone() }),
        sched_tx: tx,
        anchor,
        db_path: db_path.into(),
    };
    TestEnv { state, sent, sent_http, _rx: rx, _dir: dir }
}
```

Apply the identical two additions (`let db_path = dir.path().join("t.db").to_str().unwrap().to_string();`
right after the `let (pool, dir) = fresh_pool().await;` line, and `db_path: db_path.into(),` in the
`AppState` literal) to `test_state_offline` and `test_state_failing_transport`. No other files change
— `main.rs:72` and these three constructors are the only `AppState { … }` literals in the crate.

- [ ] **Step 8: Run the tests to verify they pass + the workspace builds**

Run: `cargo test -p vigil clear_schedule_empties_heap_keeps_inflight current_schema_version_is_latest_migration -- --test-threads=1`
Expected: PASS (2 tests).

Run: `cargo build -p vigil --tests`
Expected: builds clean (proves the `AppState` field addition compiles across `main.rs` + all test constructors).

- [ ] **Step 9: Commit**

```bash
git add crates/vigil/src/app.rs crates/vigil/src/scheduler.rs crates/vigil/src/db.rs crates/vigil/src/main.rs crates/vigil/tests/common/mod.rs
git commit -m "feat(p4.5): AppState.db_path + SchedCmd::Reseed + current_schema_version

Plumbing for backup export/import: handlers derive their temp/snapshot
dir from db_path; Reseed re-arms the in-memory probe scheduler after a
full-DB replace (clear_schedule keeps in-flight guards).

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 2: Export endpoint — `GET /api/backup/export`

**Files:**
- Create: `crates/vigil/src/api/backup.rs`
- Modify: `crates/vigil/src/api/mod.rs`
- Create/Test: `crates/vigil/tests/api_backup.rs`

**Interfaces:**
- Consumes: `AppState.db_path` (Task 1), `super::{db_err}`.
- Produces: `backup::export` handler; the `/backup/export` route.

- [ ] **Step 1: Write the failing test**

Create `crates/vigil/tests/api_backup.rs`:

```rust
mod common;
use common::*;

async fn serve(state: vigil::app::AppState) -> std::net::SocketAddr {
    let app = vigil::app::router(state);
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let a = l.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(l, app).await.unwrap(); });
    a
}

#[tokio::test]
async fn export_returns_valid_sqlite_attachment() {
    let env = test_state().await;
    let a = serve(env.state.clone()).await;
    let c = reqwest::Client::new();

    let resp = c.get(format!("http://{a}/api/backup/export")).send().await.unwrap();
    assert!(resp.status().is_success(), "export status: {}", resp.status());
    let cd = resp.headers().get(reqwest::header::CONTENT_DISPOSITION).unwrap().to_str().unwrap().to_string();
    assert!(cd.contains("attachment; filename=\"vigil-backup-"), "content-disposition: {cd}");
    let bytes = resp.bytes().await.unwrap();
    assert!(bytes.len() >= 16, "body too short");
    assert_eq!(&bytes[..16], b"SQLite format 3\0", "export is not a SQLite database");
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p vigil --test api_backup export_returns_valid_sqlite_attachment -- --test-threads=1`
Expected: FAIL — 404 (route not mounted) so the `is_success()` assert fails.

- [ ] **Step 3: Create `backup.rs` with the export handler**

Create `crates/vigil/src/api/backup.rs`:

```rust
//! REST handlers for whole-database backup export/import (CLAUDE.md §12, §10).
//! Export = a WAL-safe `VACUUM INTO` snapshot streamed as an attachment.
//! Import = validate → migrate-if-older → pre-import snapshot → atomic
//! deferred-FK replace over the live pool → re-seed the scheduler (Task 4).

use std::path::{Path, PathBuf};

use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::extract::State;

use super::{db_err, now};
use crate::app::AppState;

/// The directory holding the live DB (and thus where temp/export/snapshot
/// files are written — the only app-writable path, `/data` in the container).
fn data_dir(state: &AppState) -> PathBuf {
    Path::new(&*state.db_path)
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Single-quote-escape a filesystem path for inline use in a SQLite
/// `VACUUM INTO '...'` / `ATTACH DATABASE '...'` statement. Paths we build are
/// app-generated (epoch + random suffix) and never contain quotes; this is
/// belt-and-suspenders, and avoids relying on parameter binding in these
/// statements.
fn sql_quote(p: &Path) -> String {
    p.display().to_string().replace('\'', "''")
}

pub async fn export(State(state): State<AppState>) -> Result<(HeaderMap, Vec<u8>), (StatusCode, String)> {
    let ts = now();
    let tmp = data_dir(&state).join(format!(".vigil-export-{ts}-{}.db", rand::random::<u32>()));

    // VACUUM INTO produces a transactionally-consistent, checkpointed,
    // standalone single-file DB — no process stop, no torn WAL. Fails if the
    // target exists, hence the unique name.
    let vac = format!("VACUUM INTO '{}'", sql_quote(&tmp));
    if let Err(e) = sqlx::query(&vac).execute(&state.db).await {
        let _ = tokio::fs::remove_file(&tmp).await;
        return Err(db_err(e));
    }

    let bytes = match tokio::fs::read(&tmp).await {
        Ok(b) => b,
        Err(e) => {
            let _ = tokio::fs::remove_file(&tmp).await;
            return Err((StatusCode::INTERNAL_SERVER_ERROR, format!("read export: {e}")));
        }
    };
    let _ = tokio::fs::remove_file(&tmp).await;

    let mut headers = HeaderMap::new();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("application/octet-stream"));
    headers.insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_str(&format!("attachment; filename=\"vigil-backup-{ts}.db\""))
            .unwrap_or_else(|_| HeaderValue::from_static("attachment; filename=\"vigil-backup.db\"")),
    );
    Ok((headers, bytes))
}
```

Register the module and route. In `crates/vigil/src/api/mod.rs`, insert a single new line
`pub mod backup;` **before** the existing `pub mod channels;` (line 5), keeping the list
alphabetical — do not duplicate `pub mod channels;`:

```rust
pub mod backup;
pub mod channels;   // existing — unchanged
```

And add the route inside `routes()` (after the `/reports/...` block, line 82):

```rust
        .route("/reports/:id/email", post(reports::email))
        .route("/backup/export", get(backup::export))
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p vigil --test api_backup export_returns_valid_sqlite_attachment -- --test-threads=1`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/vigil/src/api/backup.rs crates/vigil/src/api/mod.rs crates/vigil/tests/api_backup.rs
git commit -m "feat(p4.5): backup export endpoint (VACUUM INTO -> attachment)

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 3: Info endpoint — `GET /api/backup/info`

**Files:**
- Modify: `crates/vigil/src/api/backup.rs`
- Modify: `crates/vigil/src/api/mod.rs`
- Modify: `crates/vigil/tests/api_backup.rs`

**Interfaces:**
- Consumes: `db::current_schema_version` (Task 1), `AppState.db_path`.
- Produces: `backup::info` handler + `BackupInfo` struct; the `/backup/info` route.

- [ ] **Step 1: Write the failing test**

Append to `crates/vigil/tests/api_backup.rs`:

```rust
#[tokio::test]
async fn info_reports_schema_version_and_counts() {
    let env = test_state().await;
    let a = serve(env.state.clone()).await;
    let c = reqwest::Client::new();

    // seed one monitor so counts are non-trivial
    c.post(format!("http://{a}/api/monitors"))
        .json(&serde_json::json!({"name":"x","url":"https://example.com"}))
        .send().await.unwrap();

    let info: serde_json::Value = c.get(format!("http://{a}/api/backup/info")).send().await.unwrap().json().await.unwrap();
    assert_eq!(info["schema_version"].as_i64(), Some(6));
    assert_eq!(info["counts"]["monitors"].as_i64(), Some(1));
    assert!(info["db_size_bytes"].as_i64().unwrap() > 0);
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p vigil --test api_backup info_reports_schema_version_and_counts -- --test-threads=1`
Expected: FAIL — 404 → `json()` errors / assert fails.

- [ ] **Step 3: Implement `info`**

Append to `crates/vigil/src/api/backup.rs`:

```rust
use axum::Json;
use serde::Serialize;

#[derive(Serialize)]
pub struct BackupInfo {
    pub schema_version: i64,
    pub db_size_bytes: i64, // live vigil.db file only (excludes WAL sidecars) — approximate
    pub generated_at: i64,
    pub counts: BackupCounts,
}

#[derive(Serialize)]
pub struct BackupCounts {
    pub monitors: i64,
    pub incidents: i64,
    pub checks: i64,
    pub reports: i64,
    pub channels: i64,
}

async fn count(pool: &sqlx::SqlitePool, table: &str) -> i64 {
    // table names are hardcoded literals below (never user input)
    sqlx::query_scalar::<_, i64>(&format!("SELECT COUNT(*) FROM {table}"))
        .fetch_one(pool).await.unwrap_or(0)
}

pub async fn info(State(state): State<AppState>) -> Result<Json<BackupInfo>, (StatusCode, String)> {
    let db_size_bytes = tokio::fs::metadata(&*state.db_path).await.map(|m| m.len() as i64).unwrap_or(0);
    let counts = BackupCounts {
        monitors: count(&state.db, "monitors").await,
        incidents: count(&state.db, "incidents").await,
        checks: count(&state.db, "checks").await,
        reports: count(&state.db, "reports").await,
        channels: count(&state.db, "notification_channels").await,
    };
    Ok(Json(BackupInfo {
        schema_version: crate::db::current_schema_version(),
        db_size_bytes,
        generated_at: now(),
        counts,
    }))
}
```

Add the route in `crates/vigil/src/api/mod.rs` (after the export route):

```rust
        .route("/backup/export", get(backup::export))
        .route("/backup/info", get(backup::info))
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p vigil --test api_backup info_reports_schema_version_and_counts -- --test-threads=1`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/vigil/src/api/backup.rs crates/vigil/src/api/mod.rs crates/vigil/tests/api_backup.rs
git commit -m "feat(p4.5): backup info endpoint (schema version + counts + db size)

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 4: Import endpoint — `POST /api/backup/import` (atomic live replace)

**Files:**
- Modify: `crates/vigil/src/api/backup.rs`
- Modify: `crates/vigil/src/api/mod.rs`
- Modify: `crates/vigil/tests/api_backup.rs`

**Interfaces:**
- Consumes: `db::{connect, current_schema_version}`, `AppState.{db, db_path, sched_tx}`, `SchedCmd::Reseed`.
- Produces: `backup::import` handler + `ImportResult` struct; the `/backup/import` route with a 1 GiB `DefaultBodyLimit`.

- [ ] **Step 1: Write the failing tests**

Append to `crates/vigil/tests/api_backup.rs`:

```rust
// Full round-trip: state captured in an export is restored by importing it,
// including a monitor and a settings value — and a channel secret survives.
#[tokio::test]
async fn export_import_round_trip_restores_data_and_secrets() {
    let env = test_state().await;
    let a = serve(env.state.clone()).await;
    let c = reqwest::Client::new();

    // seed: a monitor + a webhook channel whose config holds a secret token
    let created: serde_json::Value = c.post(format!("http://{a}/api/monitors"))
        .json(&serde_json::json!({"name":"keep","url":"https://example.com"}))
        .send().await.unwrap().json().await.unwrap();
    let mid = created["id"].as_i64().unwrap();
    sqlx::query("INSERT INTO notification_channels (name, type, config, is_active, created_at) VALUES ('hook','webhook',?,1,0)")
        .bind(r#"{"url":"http://x","token":"SECRET123"}"#)
        .execute(&env.state.db).await.unwrap();

    // EXPORT the current state
    let backup = c.get(format!("http://{a}/api/backup/export")).send().await.unwrap().bytes().await.unwrap().to_vec();

    // MUTATE: delete the monitor and change a setting away from its default
    c.delete(format!("http://{a}/api/monitors/{mid}")).send().await.unwrap();
    c.put(format!("http://{a}/api/settings")).json(&serde_json::json!({"retention_days": 99})).send().await.unwrap();
    let list: serde_json::Value = c.get(format!("http://{a}/api/monitors")).send().await.unwrap().json().await.unwrap();
    assert_eq!(list.as_array().unwrap().len(), 0, "precondition: monitor deleted");

    // IMPORT the backup — atomic replace
    let resp = c.post(format!("http://{a}/api/backup/import")).body(backup).send().await.unwrap();
    assert!(resp.status().is_success(), "import status: {}", resp.status());
    let res: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(res["ok"].as_bool(), Some(true));

    // monitor restored, setting reverted to default (30), secret intact
    let list: serde_json::Value = c.get(format!("http://{a}/api/monitors")).send().await.unwrap().json().await.unwrap();
    assert_eq!(list.as_array().unwrap().len(), 1, "monitor restored by import");
    let settings: serde_json::Value = c.get(format!("http://{a}/api/settings")).send().await.unwrap().json().await.unwrap();
    assert_eq!(settings["retention_days"].as_i64(), Some(30), "setting reverted to backup value");
    let cfg: String = sqlx::query_scalar("SELECT config FROM notification_channels WHERE name='hook'")
        .fetch_one(&env.state.db).await.unwrap();
    assert!(cfg.contains("SECRET123"), "channel secret survived round-trip: {cfg}");
}

// A pre-import safety snapshot is written to the data dir before the replace.
#[tokio::test]
async fn import_writes_pre_import_snapshot() {
    let env = test_state().await;
    let a = serve(env.state.clone()).await;
    let c = reqwest::Client::new();
    let backup = c.get(format!("http://{a}/api/backup/export")).send().await.unwrap().bytes().await.unwrap().to_vec();
    let resp = c.post(format!("http://{a}/api/backup/import")).body(backup).send().await.unwrap();
    assert!(resp.status().is_success());

    let dir = std::path::Path::new(&*env.state.db_path).parent().unwrap();
    let has_snapshot = std::fs::read_dir(dir).unwrap()
        .filter_map(|e| e.ok())
        .any(|e| e.file_name().to_string_lossy().starts_with("pre-import-"));
    assert!(has_snapshot, "a pre-import-*.db snapshot must exist in {dir:?}");
}

// Garbage / too-short uploads are rejected before any write; live data intact.
#[tokio::test]
async fn import_rejects_non_sqlite_and_preserves_data() {
    let env = test_state().await;
    let a = serve(env.state.clone()).await;
    let c = reqwest::Client::new();
    c.post(format!("http://{a}/api/monitors"))
        .json(&serde_json::json!({"name":"stay","url":"https://example.com"})).send().await.unwrap();

    for body in [b"abc".to_vec(), b"not a sqlite database at all!!!".to_vec()] {
        let resp = c.post(format!("http://{a}/api/backup/import")).body(body).send().await.unwrap();
        assert_eq!(resp.status(), 400, "non-sqlite upload must be 400");
    }
    let list: serde_json::Value = c.get(format!("http://{a}/api/monitors")).send().await.unwrap().json().await.unwrap();
    assert_eq!(list.as_array().unwrap().len(), 1, "live data untouched by rejected imports");
}

// A backup from a newer schema version is rejected (can't downgrade).
#[tokio::test]
async fn import_rejects_newer_schema_version() {
    let env = test_state().await;
    let a = serve(env.state.clone()).await;
    let c = reqwest::Client::new();

    let dir = tempfile::tempdir().unwrap();
    let craft = dir.path().join("craft.db");
    let pool = vigil::db::connect(craft.to_str().unwrap()).await.unwrap();
    sqlx::query("INSERT INTO schema_migrations (version, applied_at) VALUES (7, 0)").execute(&pool).await.unwrap();
    let clean = dir.path().join("craft-clean.db");
    sqlx::query(&format!("VACUUM INTO '{}'", clean.display())).execute(&pool).await.unwrap();
    drop(pool);
    let bytes = std::fs::read(&clean).unwrap();

    let resp = c.post(format!("http://{a}/api/backup/import")).body(bytes).send().await.unwrap();
    assert_eq!(resp.status(), 400);
    let msg = resp.text().await.unwrap();
    assert!(msg.to_lowercase().contains("newer"), "expected a 'newer version' message, got: {msg}");
}

// An OLDER backup is migrated up before the replace. This is the ONLY test that
// exercises the §5.3 migrate step + the §5.5 ATTACH-of-a-freshly-migrated file
// (the path the spec flags as fragile) — so if the WAL/ATTACH fallback is ever
// needed, this test is what forces it.
#[tokio::test]
async fn import_upgrades_older_backup() {
    let env = test_state().await;
    let a = serve(env.state.clone()).await;
    let c = reqwest::Client::new();

    // Craft a "v5" backup: a full v6 DB minus the reports table and its v6
    // migration marker. Structurally identical to a real v5 DB, because
    // migration 0006 only ADDS the reports table (no ALTERs to other tables).
    let dir = tempfile::tempdir().unwrap();
    let older = dir.path().join("older.db");
    let pool = vigil::db::connect(older.to_str().unwrap()).await.unwrap();
    sqlx::query("INSERT INTO monitors (name, type, url, created_at, updated_at) VALUES ('older','http','https://e.com',0,0)")
        .execute(&pool).await.unwrap();
    sqlx::query("DROP TABLE reports").execute(&pool).await.unwrap();
    sqlx::query("DELETE FROM schema_migrations WHERE version = 6").execute(&pool).await.unwrap();
    let clean = dir.path().join("older-clean.db");
    sqlx::query(&format!("VACUUM INTO '{}'", clean.display())).execute(&pool).await.unwrap();
    drop(pool);
    let bytes = std::fs::read(&clean).unwrap();

    let resp = c.post(format!("http://{a}/api/backup/import")).body(bytes).send().await.unwrap();
    assert!(resp.status().is_success(), "import status: {}", resp.status());
    let res: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(res["migrated"].as_bool(), Some(true), "older backup should be migrated up");
    assert_eq!(res["backup_version"].as_i64(), Some(5));
    assert_eq!(res["schema_version"].as_i64(), Some(6));

    let list: serde_json::Value = c.get(format!("http://{a}/api/monitors")).send().await.unwrap().json().await.unwrap();
    assert_eq!(list.as_array().unwrap().len(), 1, "monitor from the older backup is present after migrate+replace");
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p vigil --test api_backup import_ export_import_round_trip -- --test-threads=1`
Expected: FAIL — 404 on `/api/backup/import` (route not mounted).

- [ ] **Step 3: Implement `import` + `ImportResult`**

Append to `crates/vigil/src/api/backup.rs`:

```rust
use std::str::FromStr;

use axum::body::Bytes;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::Executor;

use crate::app::SchedCmd;

const SQLITE_MAGIC: &[u8; 16] = b"SQLite format 3\0";

#[derive(Serialize)]
pub struct ImportResult {
    pub ok: bool,
    pub schema_version: i64,
    pub backup_version: i64,
    pub migrated: bool,
    pub pre_import_snapshot: String,
    pub tables: serde_json::Value, // { table_name: row_count }
}

fn bad(msg: &str) -> (StatusCode, String) { (StatusCode::BAD_REQUEST, msg.to_string()) }
fn ise(msg: String) -> (StatusCode, String) { (StatusCode::INTERNAL_SERVER_ERROR, msg) }

pub async fn import(State(state): State<AppState>, body: Bytes) -> Result<Json<ImportResult>, (StatusCode, String)> {
    // 1. cheap header guard — reject garbage/empty before touching disk
    if body.len() < 16 || &body[..16] != SQLITE_MAGIC {
        return Err(bad("not a SQLite database"));
    }

    let dir = data_dir(&state);
    let ts = now();
    let import_path = dir.join(format!(".vigil-import-{ts}-{}.db", rand::random::<u32>()));
    tokio::fs::write(&import_path, &body).await.map_err(|e| ise(format!("write upload: {e}")))?;

    // helper to remove the temp import file + its WAL sidecars on every exit
    let cleanup = |p: &Path| {
        let p = p.to_path_buf();
        async move {
            let _ = tokio::fs::remove_file(&p).await;
            let _ = tokio::fs::remove_file(p.with_extension("db-wal")).await;
            let _ = tokio::fs::remove_file(p.with_extension("db-shm")).await;
        }
    };

    // 2. validate read-only (do NOT auto-create / auto-migrate here)
    let current = crate::db::current_schema_version();
    let backup_version: i64 = {
        let vopts = match SqliteConnectOptions::from_str(&format!("sqlite://{}", import_path.display())) {
            Ok(o) => o.read_only(true).create_if_missing(false),
            Err(e) => { cleanup(&import_path).await; return Err(bad(&format!("open backup: {e}"))); }
        };
        let vpool = match SqlitePoolOptions::new().max_connections(1).connect_with(vopts).await {
            Ok(p) => p,
            Err(_) => { cleanup(&import_path).await; return Err(bad("not a valid database")); }
        };
        let v: Result<i64, _> = sqlx::query_scalar("SELECT COALESCE(MAX(version),0) FROM schema_migrations").fetch_one(&vpool).await;
        vpool.close().await;
        match v {
            Ok(v) => v,
            Err(_) => { cleanup(&import_path).await; return Err(bad("not a Vigil backup (no schema_migrations)")); }
        }
    };
    if backup_version == 0 { cleanup(&import_path).await; return Err(bad("empty or invalid backup")); }
    if backup_version > current {
        cleanup(&import_path).await;
        return Err(bad(&format!("backup is from a newer Vigil version (schema v{backup_version}); upgrade Vigil first")));
    }

    // 3. upgrade an older backup in place, then release its handles before ATTACH
    let migrated = backup_version < current;
    if migrated {
        match crate::db::connect(import_path.to_str().unwrap()).await {
            Ok(p) => p.close().await,
            Err(e) => { cleanup(&import_path).await; return Err(ise(format!("migrate backup: {e}"))); }
        }
    }

    // 4. pre-import safety snapshot of CURRENT data (kept on disk = the undo).
    // Random suffix (like the temp files) so two imports in the same second —
    // or a second import while an earlier snapshot is still on disk — don't
    // collide with VACUUM INTO's "target must not already exist" rule.
    let snapshot_name = format!("pre-import-{ts}-{}.db", rand::random::<u32>());
    let snapshot_path = dir.join(&snapshot_name);
    if let Err(e) = sqlx::query(&format!("VACUUM INTO '{}'", sql_quote(&snapshot_path))).execute(&state.db).await {
        cleanup(&import_path).await;
        return Err(ise(format!("pre-import snapshot: {e}")));
    }

    // 5. atomic replace on ONE connection (ATTACH + txn must share it)
    let mut conn = match state.db.acquire().await {
        Ok(c) => c,
        Err(e) => { cleanup(&import_path).await; return Err(db_err(e)); }
    };
    let tables: Vec<String> = match sqlx::query_scalar(
        "SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' AND name != 'schema_migrations'",
    ).fetch_all(&mut *conn).await {
        Ok(t) => t,
        Err(e) => { cleanup(&import_path).await; return Err(db_err(e)); }
    };

    if let Err(e) = conn.execute(format!("ATTACH DATABASE '{}' AS backup", sql_quote(&import_path)).as_str()).await {
        cleanup(&import_path).await;
        return Err(ise(format!("attach backup: {e}")));
    }
    let replaced = replace_all(&mut conn, &tables).await;
    let _ = conn.execute("DETACH DATABASE backup").await;
    if let Err(e) = replaced {
        // the transaction already rolled back inside replace_all; live data intact
        cleanup(&import_path).await;
        return Err(ise(format!("import failed, rolled back (no data changed): {e}")));
    }

    // 6. finalize: per-table counts (for the UI), re-arm scheduler, clean up
    let mut counts = serde_json::Map::new();
    for t in &tables {
        let n: i64 = sqlx::query_scalar(&format!("SELECT COUNT(*) FROM \"{t}\"")).fetch_one(&mut *conn).await.unwrap_or(0);
        counts.insert(t.clone(), serde_json::json!(n));
    }
    drop(conn);
    cleanup(&import_path).await;
    let _ = state.sched_tx.send(SchedCmd::Reseed);

    Ok(Json(ImportResult {
        ok: true,
        schema_version: current,
        backup_version,
        migrated,
        pre_import_snapshot: snapshot_name,
        tables: serde_json::Value::Object(counts),
    }))
}

/// The destructive part, isolated so a failure anywhere rolls the whole thing
/// back. Deferred FK = the delete/insert order across tables doesn't matter;
/// referential integrity is checked once at COMMIT against the complete,
/// self-consistent imported dataset.
async fn replace_all(conn: &mut sqlx::SqliteConnection, tables: &[String]) -> Result<(), sqlx::Error> {
    if let Err(e) = async {
        conn.execute("BEGIN").await?;
        conn.execute("PRAGMA defer_foreign_keys=ON").await?;
        for t in tables {
            conn.execute(format!("DELETE FROM main.\"{t}\"").as_str()).await?;
            conn.execute(format!("INSERT INTO main.\"{t}\" SELECT * FROM backup.\"{t}\"").as_str()).await?;
        }
        conn.execute("COMMIT").await?;
        Ok::<(), sqlx::Error>(())
    }.await {
        let _ = conn.execute("ROLLBACK").await;
        return Err(e);
    }
    Ok(())
}
```

Register the route **with the 1 GiB body limit** in `crates/vigil/src/api/mod.rs`. Add the import at the top of the file (with the other axum imports, near line 16):

```rust
use axum::extract::DefaultBodyLimit;
```

Add the route after `/backup/info` — the limit layer is on this route's `MethodRouter` only:

```rust
        .route("/backup/info", get(backup::info))
        .route(
            "/backup/import",
            post(backup::import).layer(DefaultBodyLimit::max(1024 * 1024 * 1024)),
        )
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p vigil --test api_backup -- --test-threads=1`
Expected: PASS — all 7 tests (export, info, round-trip, snapshot, reject-non-sqlite, reject-newer, upgrade-older-backup).

If `export_import_round_trip_restores_data_and_secrets` fails at the ATTACH/replace step with a WAL-related error on the attached (migrated) file, apply the spec §5.5 fallback: replace `INSERT … SELECT FROM backup."t"` with a second read-only pool over `import_path` and per-row batch inserts inside the same `main` transaction. Prefer ATTACH; only switch if a test forces it.

- [ ] **Step 5: Run the full backend suite (no regressions)**

Run: `cargo test -p vigil -- --test-threads=1`
Expected: PASS (all pre-existing tests + the new `api_backup` file).

- [ ] **Step 6: Commit**

```bash
git add crates/vigil/src/api/backup.rs crates/vigil/src/api/mod.rs crates/vigil/tests/api_backup.rs
git commit -m "feat(p4.5): backup import (validate -> snapshot -> atomic replace -> reseed)

Header-guard + read-only version check + migrate-older-backup + pre-import
safety snapshot + one deferred-FK transaction replacing every table, then
SchedCmd::Reseed. Import route carries a 1 GiB body limit. Rejects
non-SQLite and newer-schema uploads with live data intact.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 5: Frontend — "Backup & restore" Settings section

**Files:**
- Modify: `web/src/api.ts`
- Modify: `web/src/components/Settings.tsx`
- Create: `web/src/__tests__/backup.test.tsx`

**Interfaces:**
- Consumes: `GET /api/backup/info`, `GET /api/backup/export`, `POST /api/backup/import`.
- Produces: `api.getBackupInfo()`, `api.importBackup(file)`, `BackupInfo`/`ImportResult` TS interfaces; a Backup section in Settings.

- [ ] **Step 1: Write the failing test**

Create `web/src/__tests__/backup.test.tsx`:

```tsx
import { render, screen, fireEvent } from "@solidjs/testing-library";
import { test, expect, vi } from "vitest";
import Settings from "../components/Settings";

function stubFetch(posts: any[]) {
  vi.stubGlobal("fetch", vi.fn(async (url: any, opts?: any) => {
    if (url === "/api/backup/info") {
      return { ok: true, json: async () => ({ schema_version: 6, db_size_bytes: 4096, generated_at: 0, counts: { monitors: 2, incidents: 0, checks: 0, reports: 0, channels: 1 } }) };
    }
    if (url === "/api/backup/import" && opts?.method === "POST") {
      posts.push(url);
      // pending promise: keeps the component out of its post-success location.reload()
      return new Promise(() => {});
    }
    if (url === "/api/settings") return { ok: true, json: async () => ({ anchors: [], retention_days: 30 }) };
    return { ok: true, json: async () => [] };
  }) as any);
}

test("backup section shows the info readout and a download link", async () => {
  stubFetch([]);
  render(() => <Settings />);
  expect(await screen.findByText(/Backup & restore/i)).toBeTruthy();
  const link = await screen.findByRole("link", { name: /download backup/i });
  expect(link.getAttribute("href")).toBe("/api/backup/export");
  // schema readout renders only after the (3rd, sequential) getBackupInfo() in
  // onMount resolves — await it rather than reading synchronously.
  expect(await screen.findByText(/schema v6/i)).toBeTruthy();
});

test("import requires an explicit confirm before POSTing", async () => {
  const posts: any[] = [];
  stubFetch(posts);
  render(() => <Settings />);

  const fileInput = (await screen.findByLabelText(/choose backup file/i)) as HTMLInputElement;
  const file = new File([new Uint8Array([0x53, 0x51, 0x4c])], "b.db", { type: "application/octet-stream" });
  fireEvent.change(fileInput, { target: { files: [file] } });

  fireEvent.click(screen.getByRole("button", { name: /import & replace/i }));
  // nothing sent until the destructive confirm is clicked
  expect(posts.length).toBe(0);
  fireEvent.click(screen.getByRole("button", { name: /yes, replace everything/i }));
  await Promise.resolve();
  expect(posts).toContain("/api/backup/import");
});
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cd web && npx vitest run src/__tests__/backup.test.tsx`
Expected: FAIL — "Backup & restore" text / the download link / the file input don't exist yet.

- [ ] **Step 3: Add the api.ts functions + interfaces**

Append to `web/src/api.ts`:

```ts
export interface BackupInfo {
  schema_version: number;
  db_size_bytes: number;
  generated_at: number;
  counts: { monitors: number; incidents: number; checks: number; reports: number; channels: number };
}

export interface ImportResult {
  ok: boolean;
  schema_version: number;
  backup_version: number;
  migrated: boolean;
  pre_import_snapshot: string;
  tables: Record<string, number>;
}

export function getBackupInfo(): Promise<BackupInfo> {
  return fetch("/api/backup/info").then((r) => json(r));
}

/** POSTs the raw file bytes as the request body (matches the Bytes extractor
 *  on the backend). Throws the server's message on a non-2xx response. */
export function importBackup(file: File): Promise<ImportResult> {
  return fetch("/api/backup/import", { method: "POST", body: file }).then((r) => json(r));
}
```

- [ ] **Step 4: Add the Backup section to Settings.tsx**

In `web/src/components/Settings.tsx`, add these signals alongside the other `createSignal`s near the top of the component (e.g. after the retention signals):

```tsx
  const [backupInfo, setBackupInfo] = createSignal<api.BackupInfo | null>(null);
  const [importFile, setImportFile] = createSignal<File | null>(null);
  const [importConfirming, setImportConfirming] = createSignal(false);
  const [importBusy, setImportBusy] = createSignal(false);
  const [importError, setImportError] = createSignal<string | null>(null);
```

Extend the `onMount` (inside the existing settings `try`, after the report lines around line 178) to load backup info:

```tsx
      try {
        setBackupInfo(await api.getBackupInfo());
      } catch {
        // backup info is best-effort; the section still renders its actions
      }
```

Add the import handler alongside the other handlers (e.g. after `handleSaveRetention`):

```tsx
  async function handleImportConfirmed() {
    const file = importFile();
    if (!file) return;
    setImportBusy(true);
    setImportError(null);
    try {
      await api.importBackup(file);
      // Full DB was replaced — reload so every screen reflects the new data.
      window.location.reload();
    } catch (e: any) {
      setImportError(e?.message ?? "Import failed");
      setImportBusy(false);
      setImportConfirming(false);
    }
  }
```

Add the section JSX immediately after the Data-retention `</section>` (around line 809):

```tsx
      <section class="form-section settings-section">
        <h3 class="form-section-title">Backup &amp; restore</h3>
        <Show when={backupInfo()}>
          {(info) => (
            <p class="settings-note mono">
              schema v{info().schema_version} · {info().counts.monitors} monitors ·{" "}
              {info().counts.incidents} incidents · {info().counts.channels} channels ·{" "}
              ~{Math.round(info().db_size_bytes / 1024)} KB
            </p>
          )}
        </Show>

        <div class="form-field">
          <a class="btn-accent" href="/api/backup/export" download="vigil-backup.db">
            Download backup
          </a>
          <p class="settings-note">
            Full snapshot including channel secrets — store it securely. The SMTP password is not
            included (it lives in a Docker secret).
          </p>
        </div>

        <div class="form-field">
          <label for="backup-import-file">Choose backup file</label>
          <input
            id="backup-import-file"
            type="file"
            accept=".db,.sqlite"
            onChange={(e) => {
              setImportFile(e.currentTarget.files?.[0] ?? null);
              setImportConfirming(false);
              setImportError(null);
            }}
          />
        </div>

        <div class="detail-actions">
          <button
            type="button"
            class="btn-accent"
            disabled={!importFile() || importBusy()}
            onClick={() => setImportConfirming(true)}
          >
            Import &amp; replace
          </button>
        </div>

        <Show when={importConfirming()}>
          <div class="settings-note">
            This <strong>erases all current data</strong> and replaces it with the backup. A safety
            snapshot is saved to <code>/data</code> first. Anchor-host changes need a restart.
          </div>
          <div class="detail-actions">
            <button type="button" class="btn-ghost danger" disabled={importBusy()} onClick={handleImportConfirmed}>
              {importBusy() ? "Importing…" : "Yes, replace everything"}
            </button>
            <button type="button" class="btn-ghost" disabled={importBusy()} onClick={() => setImportConfirming(false)}>
              Cancel
            </button>
          </div>
        </Show>

        <Show when={importError()}>
          <div class="test-result mono">{importError()}</div>
        </Show>
      </section>
```

> Note (verified): `theme.css` defines `.btn-accent`, `.btn-ghost` (+ `.btn-ghost.danger`), and
> `.btn-link` (+ `.btn-link.danger`) — there is **no** `.btn-danger`. The app's destructive-button
> convention is `class="btn-ghost danger"` (see `DetailPanel.tsx`) or `class="btn-link danger"`
> (see `Settings.tsx`/`Maintenance.tsx`). Use `btn-ghost danger` for the confirm button as above.

- [ ] **Step 5: Run the test to verify it passes**

Run: `cd web && npx vitest run src/__tests__/backup.test.tsx`
Expected: PASS (2 tests).

- [ ] **Step 6: Typecheck + full frontend suite + build**

Run: `cd web && npx vitest run && npm run build`
Expected: all vitest tests PASS; `npm run build` (tsc + vite) completes with no type errors.

- [ ] **Step 7: Commit**

```bash
git add web/src/api.ts web/src/components/Settings.tsx web/src/__tests__/backup.test.tsx
git commit -m "feat(p4.5): Backup & restore settings section (download + confirm-gated import)

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 6: Live acceptance + docs + merge

**Files:**
- Modify: `README.md`

- [ ] **Step 1: Build and run the container**

Run:
```bash
docker compose build && docker compose up -d
sleep 5
curl -sS -o /dev/null -w "%{http_code}\n" http://127.0.0.1:8099/healthz
```
Expected: `200`.

- [ ] **Step 2: Seed, export, mutate, import — end-to-end smoke**

Run:
```bash
# create a monitor
curl -sS -X POST http://127.0.0.1:8099/api/monitors \
  -H 'content-type: application/json' \
  -d '{"name":"acceptance","url":"https://example.com"}'
# export a backup to disk
curl -sS http://127.0.0.1:8099/api/backup/export -o /tmp/vigil-backup.db
# verify it's a SQLite file
head -c 16 /tmp/vigil-backup.db | xxd | head -1   # expect "SQLite format 3."
# delete every monitor, then import the backup
curl -sS http://127.0.0.1:8099/api/monitors | python3 -c 'import sys,json;[print(m["id"]) for m in json.load(sys.stdin)]' \
  | xargs -I{} curl -sS -X DELETE http://127.0.0.1:8099/api/monitors/{}
curl -sS -X POST --data-binary @/tmp/vigil-backup.db http://127.0.0.1:8099/api/backup/import
# the monitor is back
curl -sS http://127.0.0.1:8099/api/monitors
# a pre-import snapshot was written into the data volume
docker compose exec vigil ls -1 /data | grep pre-import-
```
Expected: the SQLite magic line prints; import returns `{"ok":true,...}`; the monitor list is non-empty again; a `pre-import-*.db` file is listed under `/data`. Tear down with `docker compose down` when satisfied.

- [ ] **Step 3: Update the README backup section**

In `README.md`'s "Data & backups" section, add a paragraph noting the in-app path (keep the manual `cp` runbook as the cold-copy alternative):

```markdown
**In-app backup (recommended):** Settings → *Backup & restore* → **Download backup** exports a
consistent, WAL-safe snapshot of the whole database (via `VACUUM INTO`) as a single `.db` file —
no need to stop the container. It includes channel secrets (webhook/ntfy tokens, `inline:` auth)
but **not** the SMTP password (that lives in a Docker secret). To restore, choose the file under
*Import & replace*: Vigil validates it, writes a `pre-import-<epoch>.db` safety snapshot to `/data`,
then atomically replaces all data and reloads. Anchor-host changes from a restored backup take
effect after the next restart.
```

- [ ] **Step 4: Commit**

```bash
git add README.md
git commit -m "docs(p4.5): document in-app backup export/import path

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

- [ ] **Step 5: Finish the branch**

Use the `superpowers:finishing-a-development-branch` skill to choose merge/PR. This branch (`feat/p4-monthly-reports`) also carries the P4.4 reports work; confirm with the operator whether backup lands in the same PR or its own.

---

## Self-Review

**1. Spec coverage** — every spec section maps to a task:
- §4 Export → Task 2. §5 Import (header guard, read-only validate, migrate-older, pre-import snapshot, atomic ATTACH replace, cleanup, Reseed, body limit) → Task 4. §6 Scheduler re-arm (`SchedCmd::Reseed`, `clear_schedule`, `catch_up`) → Task 1 + 4. §7 `AppState.db_path` + `current_schema_version` → Task 1. §8 API (export/import/info + 1 GiB limit) → Tasks 2/3/4. §9 Frontend section + `api.ts` → Task 5. §10 safety (rollback, pre-import snapshot, in-flight race documented) → Task 4 tests + README. §12 tests (valid SQLite, round-trip, secret survives, reject non-SQLite, reject newer, **older-backup upgrade `migrated:true` — the migrate + ATTACH-of-WAL path**, snapshot, `clear_schedule`, `current_schema_version`, frontend) → Tasks 1/2/4/5. §13 tasks → this plan. §14 boundaries → README (Task 6).
- No new migration / settings / SSE event — honored (none added).

**2. Placeholder scan** — no TBD/TODO; every code step shows complete code; the only conditional guidance (WAL/ATTACH fallback in Task 4 Step 4, button-class fallback in Task 5 Step 4) is spec-sanctioned and fully specified.

**3. Type consistency** — `AppState.db_path: Arc<str>` set (Task 1) and read via `&*state.db_path` (Tasks 2–4). `SchedCmd::Reseed` defined (Task 1) and sent (Task 4). `clear_schedule` name consistent (Task 1 def, Task 1 test). `current_schema_version` consistent across db.rs, Task 3 info, Task 4 import. `BackupInfo`/`ImportResult` field names match between backend structs (Tasks 3/4) and TS interfaces (Task 5). Route paths (`/backup/export`, `/backup/info`, `/backup/import`) match between `api/mod.rs`, handlers, tests, and `api.ts`.
