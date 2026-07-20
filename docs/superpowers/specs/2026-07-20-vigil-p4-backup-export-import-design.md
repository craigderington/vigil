# Vigil P4.5 — Backup Export / Import — Design Spec

> Sub-project 5 of the P4 series (CLAUDE.md §12 "Backup/portability", §10 IPC
> `export_backup()` / `import_backup(file)`, §11.8 Settings "backup export/import"). The last
> unbuilt P4 line item. One-click **full-DB export** (download) + **atomic live import** (upload
> & replace, no restart), surfaced as a **"Backup & restore" section in the Settings screen**.
> Same rigor as the P4.2/P4.3/P4.4 specs.

---

## 1. Goals & Non-Goals

**Goals**
- **One-click export** → the operator downloads a single, self-consistent SQLite file that is a
  **full snapshot** of everything (monitors, channels, settings, maintenance windows AND all
  history: `checks`, `check_aggregates_daily`, `incidents`, `reports`, certs, domain info). A
  restore reproduces the install exactly.
- **Import** → upload a previously-exported file and **atomically replace** all live data in one
  transaction, **without a container restart**, with an automatic pre-import safety snapshot so a
  bad import is always recoverable.
- Fit existing conventions exactly (new `api/backup.rs`, `SchedCmd`, `settings_store`-free — no
  new settings, no new migration).

**Non-Goals (v1)** (all user-confirmed via brainstorming)
- **No scheduled/automatic backups** — export is a manual, on-demand download; import is manual.
  (No new `settings` keys, no scheduler task, no `report_*`-style config.)
- **No config-only / partial export** — export is always the full DB. (Config-only was offered
  and declined.)
- **No encryption, no zip/tar wrapping, no PDF** — the artifact is a bare `.db` SQLite file.
- **No merge import** — import is a full destructive replace (offered and declined).
- **No secret scrubbing** — DB-stored secrets (webhook `Authorization` headers, Discord/Slack
  `webhook_url`, ntfy `token`, `monitors.auth_ref` `inline:` values) are **included** so a
  restore is turnkey. The SMTP password is never in the DB (Docker secret / env) so it is
  excluded either way. The download is flagged **sensitive** in the UI.
- **No cross-tab live refresh** — the importing browser tab reloads; other open tabs stay stale
  until manually refreshed (single-operator app, rare op).

---

## 2. Context — what exists / what's reused

Verified against the tree. **No backup/export/import code exists** — genuinely new (a `grep` for
`backup|export|import|vacuum|snapshot|dump|zip|tar|archive` over `crates/vigil/src` returns only
unrelated hits). Reused / relevant:

- **Storage:** single SQLite file at `cfg.db_path` (default `/data/vigil.db`), **WAL mode**,
  `foreign_keys=ON`, `auto_vacuum=Incremental` (`db.rs:18-27`). The `/data` volume is the only
  writable, app-owned (uid 10001) path — export/import temp files and the pre-import snapshot all
  live **next to the DB file** (same parent dir).
- **README manual-backup runbook** (`README.md:44-62`) explicitly warns a hot `cp` of
  `vigil.db` alone can be **torn/stale** because of WAL. This is precisely why the in-app export
  uses `VACUUM INTO` (a transactionally-consistent, checkpointed, standalone single-file copy)
  rather than a file copy.
- **PRAGMA-on-pool precedent:** `maintenance.rs:59-68` issues `PRAGMA incremental_vacuum` against
  the pool — the same mechanism the export/import use for `VACUUM INTO` / `ATTACH`.
- **Migration runner** (`db.rs`): a hand-rolled `MIGRATIONS: &[(i64,&str)]` array + a
  `schema_migrations(version, applied_at)` table. `db::connect(path)` opens **and migrates** any
  DB at `path` up to the current version — reused to upgrade an older imported backup.
- **Reports "Export HTML" anchor-download** (`Reports.tsx:95`,
  `<a href="/api/reports/:id/html" target="_blank">`) — the closest existing download pattern;
  the export button mirrors it with a `download` attribute + `Content-Disposition`.
- **Scheduler** (`scheduler.rs`): an **in-memory** min-heap `SchedState` seeded once at boot by
  `catch_up(db, sched)` and thereafter mutated only by `SchedCmd` messages over
  `state.sched_tx`. It does **NOT** re-read all monitors per tick — the core reason import needs
  an explicit re-arm (§6).
- **Settings/API/handler conventions:** `api/mod.rs` route table + `pub mod`; `ApiResult<T>` =
  `Result<Json<T>, (StatusCode, String)>`, `db_err`, `now()` (`api/mod.rs:24-30`); DTOs
  `#[derive(Deserialize)]`. Tests: `tests/common/mod.rs::test_state()` (tempdir DB via the real
  `db::connect`) + `tests/api.rs::serve()` (real axum app on an ephemeral port, hit with
  `reqwest`).

**Full table inventory (export scope, migrations 0001–0006):** `monitors`, `checks`,
`incidents`, `notification_channels`, `monitor_notifications`, `notification_log`, `settings`,
`check_aggregates_daily`, `ssl_certs`, `domain_info`, `maintenance_windows`, `reports`, plus the
internal `schema_migrations`. Schema uses `INTEGER PRIMARY KEY` (rowid aliases, **not**
`AUTOINCREMENT`) → **no `sqlite_sequence` table** to worry about; explicit ids round-trip
verbatim.

---

## 3. Locked decisions (from brainstorming)

| Decision | Choice |
|---|---|
| Export scope | **Full snapshot** — every table incl. history + `schema_migrations` (version pin). |
| Export mechanism | **`VACUUM INTO`** (WAL-safe, no process stop). |
| Import mode | **Atomic live replace** — wipe + reload in one transaction (deferred FK), no restart, pre-import safety snapshot first. |
| Secrets in export | **Included** (DB plaintext secrets travel; SMTP password stays external). |
| Surface | **Settings section** "Backup & restore" (not a rail screen). |
| Post-import UI | **`location.reload()`** in the importing tab. |
| Pre-import snapshots | **Kept on disk** (no auto-pruning in v1); operator prunes `/data` manually. |

---

## 4. Export — `GET /api/backup/export`

Handler `backup::export(State(state)) -> impl IntoResponse`:
1. Derive `data_dir` = parent of `state.db_path` (§7). Choose a unique temp path
   `data_dir/.vigil-export-<epoch>-<rand>.db` (`rand` already in-tree; `VACUUM INTO` **fails if
   the target exists**, so the name must be unique).
2. `sqlx::query("VACUUM INTO ?").bind(&temp_path).execute(&state.db).await` — bound parameter
   (avoids path-quoting; the path is app-generated regardless). Produces a clean, checkpointed,
   standalone DB containing **all** tables incl. `schema_migrations`.
3. `let bytes = tokio::fs::read(&temp_path).await?`; `tokio::fs::remove_file(&temp_path)` (log &
   ignore cleanup errors).
4. Return `200` with:
   - `Content-Type: application/octet-stream`
   - `Content-Disposition: attachment; filename="vigil-backup-<epoch>.db"`
   - body = `bytes` (via `([(header, val)…], Vec<u8>).into_response()`).
   On any error (VACUUM / read) → `(StatusCode::INTERNAL_SERVER_ERROR, msg)` and best-effort temp
   cleanup.

**Notes**
- `VACUUM INTO` reads a consistent snapshot and does **not** need an exclusive lock (unlike plain
  `VACUUM`), so concurrent probes/writes are unaffected.
- The file is **full & secret-bearing** by decision (§1). The UI labels the download sensitive.
- Filename uses the raw epoch (dependency-free, unambiguous); the operator may rename. The
  browser honors `Content-Disposition` regardless.
- Frontend: a plain `<a href="/api/backup/export" download="vigil-backup.db">` styled as a
  button — no `fetch`/Blob needed (mirrors the Reports HTML-export anchor).

---

## 5. Import — `POST /api/backup/import` (raw `.db` request body)

Body is the raw SQLite file bytes (`Content-Type: application/octet-stream`), extracted as
`body: axum::body::Bytes`. **The route carries `DefaultBodyLimit::max(1 GiB)`** — axum's global
2 MB default would truncate any real backup; this is a per-route override (§8), a **must-not-miss**.

Handler `backup::import(State(state), body: Bytes) -> ApiResult<ImportResult>`, all steps
reversible until the final `COMMIT`:

### 5.1 Cheap header guard (before touching disk)
- Reject if `body.len() < 16` or `&body[..16] != b"SQLite format 3\0"` (the 16-byte SQLite magic,
  NUL-terminated) → `400 "not a SQLite database"`. Kills garbage, truncated, and empty uploads
  before any file write or DB open (defends against the "valid-but-empty DB silently wipes
  everything" footgun — an empty/garbage upload never reaches the replace).

### 5.2 Persist + validate (read-only, no auto-migrate)
- `data_dir = parent(state.db_path)`; write bytes to `import_path =
  data_dir/.vigil-import-<epoch>-<rand>.db` (`tokio::fs::write`).
- Open a **read-only** validation connection (NOT the migrating `db::connect`):
  `SqliteConnectOptions::from_str("sqlite://<import_path>")?.create_if_missing(false).read_only(true)`,
  `SqlitePoolOptions::new().max_connections(1).connect_with(...)`. Open failure → `400 "not a
  valid database"` (+ temp cleanup).
- `let backup_version: i64 = sqlx::query_scalar("SELECT COALESCE(MAX(version),0) FROM
  schema_migrations").fetch_one(&vconn).await` — if the query **errors** (no such table) → `400
  "not a Vigil backup (no schema_migrations)"`. If `backup_version == 0` → `400 "empty or invalid
  backup"`. If `backup_version > db::current_schema_version()` → `400 "backup is from a newer
  Vigil version (schema vN); upgrade Vigil first"`. **Close/drop the validation pool** before any
  write to the file (release its lock / WAL handles).

### 5.3 Upgrade older backups
- If `backup_version < current`: `db::connect(&import_path).await` (opens writable + runs the
  hand-rolled migration runner → brings the file to current) then **drop that pool** (checkpoints
  its WAL, releases handles) before ATTACH. If `== current`: skip. This makes backups durable
  across Vigil upgrades — the whole point of a backup.

### 5.4 Pre-import safety snapshot
- Acquire a `state.db` connection; `VACUUM INTO 'data_dir/pre-import-<epoch>.db'` of the **current
  live** data. Kept on disk (the undo). If this fails → abort import (`500`), live data untouched.

### 5.5 Atomic replace — one pooled connection, deferred FK
- `let mut conn = state.db.acquire().await?;` (hold one connection for the whole sequence — ATTACH
  + the transaction must share it).
- Enumerate target tables from the **live** schema:
  `SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' AND name !=
  'schema_migrations'` (schema_migrations is deliberately **left untouched** — the live version
  record stays authoritative; the imported file is already at current, so its data is identical).
- Run, **all on `conn`**, in this exact order (ATTACH cannot run inside a transaction; deferred FK
  must be set inside it):
  ```
  ATTACH DATABASE ? AS backup;              -- bind import_path
  BEGIN;
  PRAGMA defer_foreign_keys = ON;           -- FK checked once at COMMIT → order-independent
  -- for each enumerated table t:
  DELETE FROM main."t";
  INSERT INTO main."t" SELECT * FROM backup."t";
  COMMIT;
  DETACH DATABASE backup;
  ```
  Table names are interpolated (can't be bound) but come from `sqlite_master` (our own trusted
  schema) and are double-quoted. `SELECT *` relies on identical column order, guaranteed because
  both DBs are at the **same** schema version (§5.3).
- **On any error** during the transaction: `ROLLBACK; DETACH DATABASE backup;` → live data intact
  (rollback restores it), return `500`. The pre-import snapshot is the second safety net.
- **WAL/ATTACH robustness note:** if §5.3 migrated the file, dropping that pool checkpoints its
  WAL back into the file before ATTACH, so a fresh ATTACH sees complete data (we only ever **read**
  `backup.*` and **write** `main.*`, so main's atomicity is unaffected by the attached DB's journal
  mode). The round-trip test (§12) is the guard. If in-process ATTACH of a WAL file ever proves
  troublesome, the equivalent fallback preserving the same atomic-replace intent is: open the
  import file as a second read pool and, within the single `main` transaction, `SELECT`-then-batch-
  `INSERT` each table's rows instead of `INSERT … SELECT FROM backup`. Prefer ATTACH; fall back
  only if a test forces it.

### 5.6 Finalize
- `tokio::fs::remove_file(import_path)` (+ its `-wal`/`-shm` if present); **keep** the pre-import
  snapshot. Log & ignore cleanup errors.
- **Re-arm the scheduler:** `state.sched_tx.send(SchedCmd::Reseed)` (§6). Ignore a send error
  (only fails if the scheduler is gone, i.e. shutdown).
- Return `200 ImportResult { ok: true, schema_version: current, backup_version, migrated:
  backup_version < current, pre_import_snapshot: "pre-import-<epoch>.db", tables: {name: count} }`
  (per-table row counts read after the replace, for the UI summary).

---

## 6. Scheduler re-arm — `SchedCmd::Reseed`

The probe scheduler's `SchedState` is an in-memory heap seeded once at boot; after a full DB
replace it still references the **old** monitor set (stale ids; because ids are reused
`INTEGER PRIMARY KEY`s, an old heap entry could fire a *different* monitor now occupying that id).
Fix, matching the existing command pattern:

- **`app.rs`:** add `Reseed` to `enum SchedCmd` (`#[derive(Clone, Copy, Debug)]`).
- **`scheduler.rs`:** add `SchedState::clear_schedule(&mut self)` that **empties the heap but
  preserves `in_flight`** (so a worker mid-probe when the reseed lands cannot double-fire — the
  re-seeded entry for its id is popped-and-discarded by the existing `take_due` in-flight guard).
  Implement as `self.heap.clear();` (leaves `in_flight` intact). Handle the command in
  `run_scheduler`'s `select!`:
  ```
  Some(SchedCmd::Reseed) => { sched.clear_schedule(); catch_up(&state.db, &mut sched).await; }
  ```
  `catch_up` re-seeds every non-paused, non-heartbeat monitor from the new DB (heartbeat monitors
  stay ping/reaper-driven, exactly as at boot).
- **Other background loops** (`cert_scheduler`, `heartbeat::run_reaper`, `maintenance::run`,
  `maintenance_windows::run`, `renotify::run`, `digest::run`, `report::scheduler::run`,
  `rollup`) re-query the DB each iteration → **self-adapt** after the replace, no signal needed.
- **Known limitation (documented in-UI):** `AnchorGate` reads the `anchors` setting **once at
  boot** (`main.rs:69-70`) — an imported `anchors` value takes effect only after a restart. Minor;
  noted next to the import control.
- **Frontend:** after a `200` import, `location.reload()` — guarantees fresh UI everywhere in the
  importing tab (the SPA re-runs every `onMount` loader / re-opens SSE).

---

## 7. Plumbing — `AppState.db_path`

Handlers need the data directory to place temp/snapshot files. `AppState` does not currently carry
the path.

- **`app.rs`:** add `pub db_path: std::sync::Arc<str>` to `AppState`.
- **`main.rs::serve`:** `db_path: cfg.db_path.clone().into()` when constructing `AppState`.
- **`tests/common/mod.rs`:** thread the tempdir DB path into the 3 constructors (`test_state`,
  `test_state_offline`, `test_state_failing_transport`). Cleanest: have `fresh_pool()` also return
  the path string (`(pool, dir, String)`) and set `db_path: path.into()` in each. (All call sites
  are in this one file.)
- Handlers derive `let data_dir = std::path::Path::new(&*state.db_path).parent()` (fallback to
  `.` if none). Temp/snapshot files are siblings of `vigil.db`.
- **`db.rs`:** add `pub fn current_schema_version() -> i64 { MIGRATIONS.last().map(|(v,_)| *v).unwrap_or(0) }`.

---

## 8. API surface (`crates/vigil/src/api/backup.rs`, mounted under `/api`)

| Route | Handler | Returns |
|---|---|---|
| `GET /backup/export` | `export` | `application/octet-stream` attachment (the full snapshot). |
| `POST /backup/import` | `import` | `ImportResult` JSON (§5.6); `400` on validation failure, `500` on replace error (live data intact). |
| `GET /backup/info` | `info` | `{ schema_version, db_size_bytes, counts: {monitors, incidents, checks, reports, channels}, generated_at }` — powers the Settings section (current-state readout before export/import). |

- Registered in `api/mod.rs`: `pub mod backup;` + three `.route(...)` lines. The **import route
  carries `.layer(axum::extract::DefaultBodyLimit::max(1024*1024*1024))`** (1 GiB) — applied to
  that route only (the export/info routes keep the default). Confirm placement so the limit layer
  wraps only `/backup/import`.
- `info.db_size_bytes` from `tokio::fs::metadata(&state.db_path)` (best-effort; the live file, not
  incl. WAL sidecars — labeled "approx"). `counts` from `SELECT COUNT(*)` per table.
- Handlers follow `ApiResult`/`db_err`/`now` conventions. Import validation errors are
  `(StatusCode::BAD_REQUEST, msg)` (plain-text body; the frontend surfaces the message).

---

## 9. Frontend — "Backup & restore" Settings section

A new `<section class="form-section settings-section">` in `web/src/components/Settings.tsx`
(loaded in the existing `onMount` alongside channels/settings), modeled on the Data-retention /
Monthly-reports blocks:

- **Readout** (from `api.getBackupInfo()`): schema version, approx DB size, quick counts
  (monitors · incidents · checks · reports · channels).
- **Export:** an `<a class="btn-accent" href="/api/backup/export" download="vigil-backup.db">
  Download backup</a>` + a one-line note: *"Full snapshot including channel secrets — store it
  securely. SMTP password is not included (Docker secret)."*
- **Import:** `<input type="file" accept=".db,.sqlite">` + a selected-filename display + an
  **"Import & replace"** destructive button. Clicking arms an inline `<Show>`-gated confirm row —
  *"This ERASES all current data and replaces it with the backup. A safety snapshot is saved to
  /data first. Anchor-host changes need a restart. Continue?"* — with **"Yes, replace everything"**
  / **"Cancel"**. Confirm → `POST` the `File` as the raw body via `api.importBackup(file)`; on
  success show a summary (`tables` counts + `pre_import_snapshot` name) then `location.reload()`;
  on failure show the thrown message inline (the shared `json()` helper surfaces `"400 …: msg"`).
- **`web/src/api.ts`:** `getBackupInfo()`; `importBackup(file: File)` =
  `fetch("/api/backup/import", { method:"POST", body: file })` → parse JSON on 2xx, throw a clean
  message on non-2xx; a `BackupInfo` / `ImportResult` TS interface. Export needs no `api.ts` fn
  (it's a plain anchor).

---

## 10. Concurrency & safety

- **Writer serialization:** while the §5.5 transaction holds SQLite's single writer, other pool
  writers **block** briefly then resume against the new data. Reads proceed (WAL).
- **In-flight probe race (documented, accepted):** a probe that read a monitor *before* the
  replace could try to write a `checks`/`incidents` row for a `monitor_id` that no longer exists
  post-replace → **its own INSERT errors and is logged; no corruption** (FK on, its txn rolls
  back). Or the id now maps to a different monitor → one mislabeled check, self-healing on the
  next tick. Given import is a rare manual op for one operator, this is documented rather than
  guarded with a global quiesce (YAGNI). The pre-import snapshot bounds worst-case blast radius.
- **Atomicity:** the replace is a single transaction with deferred FK — it either fully applies or
  fully rolls back; there is no partially-replaced state.
- **Recoverability:** every import writes `pre-import-<epoch>.db` first; a bad restore is undone by
  importing that snapshot (or the README manual-restore runbook).

---

## 11. Module / file structure

- **New:** `crates/vigil/src/api/backup.rs` (`export` / `import` / `info` handlers, `ImportResult`
  / `BackupInfo` structs); `crates/vigil/tests/api_backup.rs`.
- **Edits:**
  - `api/mod.rs` — `pub mod backup;` + 3 routes + the 1 GiB `DefaultBodyLimit` layer on import.
  - `app.rs` — `AppState.db_path: Arc<str>`; `SchedCmd::Reseed`.
  - `scheduler.rs` — `SchedState::clear_schedule()`; handle `SchedCmd::Reseed` in `run_scheduler`.
  - `db.rs` — `pub fn current_schema_version()`.
  - `main.rs` — set `db_path` on `AppState`.
  - `tests/common/mod.rs` — thread the DB path into the 3 `AppState` constructors.
  - `web/src/components/Settings.tsx` — the Backup & restore section.
  - `web/src/api.ts` — `getBackupInfo`, `importBackup`, `BackupInfo`/`ImportResult` interfaces.
- **No new migration** (operates over existing tables); **no new settings keys**; **no new crates**
  (`rand`, `tokio::fs`, `sqlx` all in-tree); **no `events.rs` change** (post-import refresh is a
  client-side `location.reload()`, not an SSE event).

---

## 12. Testing (`tests/api_backup.rs` via `serve()` + `reqwest`; unit tests inline)

- **export is valid SQLite:** `GET /api/backup/export` → `200`, `Content-Disposition` contains
  `attachment; filename="vigil-backup-`, and the body's first 16 bytes == `b"SQLite format 3\0"`.
- **round-trip restore:** seed a monitor + email channel + a non-default setting → export bytes →
  mutate live DB (delete the monitor via `DELETE /api/monitors/:id`; change the setting via `PUT
  /api/settings`) → `POST /api/backup/import` with the saved bytes → assert the monitor is back
  (`GET /api/monitors`), the setting reverted (`GET /api/settings`), the channel restored.
- **secret survives round-trip:** a `webhook` channel whose `config` holds an `Authorization`
  header (a secret) is byte-identical after export→import (proves "secrets included").
- **reject non-SQLite:** `POST` a garbage/`<16`-byte body → `400`, and assert **live data is
  unchanged** (a pre-existing monitor still present) — the header guard fires before any write.
- **reject newer schema:** craft a temp DB (via `db::connect`) and `INSERT INTO schema_migrations
  (version, applied_at) VALUES (current+1, …)`; `POST` its bytes → `400` mentioning "newer" and
  live data unchanged.
- **pre-import snapshot created:** after a successful import, a `pre-import-*.db` file exists in
  the test DB's directory (assert via the tempdir path).
- **info readout:** `GET /api/backup/info` → the current `schema_version` and plausible counts.
- **older-backup upgrade (optional/if cheap):** a v5 DB imported into a v6 app is migrated to v6
  then replaced (asserts `migrated: true`); or documented as covered by the migration runner.
- **unit — `clear_schedule`:** heap emptied, `in_flight` preserved; a re-seed after an in-flight
  `take_due` does not hand the id out again until `complete` (mirrors the existing
  `remove_does_not_clear_in_flight_no_double_fire` test).
- **unit — `current_schema_version`** == 6 (updates with each future migration).
- **frontend (`web/src/__tests__/`):** the Backup section renders the info readout; the import
  confirm gate arms and cancels; `importBackup` posts the file body. (Export is a static anchor.)
- Backend suite `--test-threads=1`, rustls-only, **no new crates**; tsc + vite build clean.

---

## 13. Task decomposition preview (~6 tasks; writing-plans finalizes)

1. **Plumbing:** `AppState.db_path` (+ `main.rs` + 3 test constructors), `SchedCmd::Reseed` +
   `SchedState::clear_schedule()` + `run_scheduler` handling + `db::current_schema_version()` +
   the `clear_schedule` / version unit tests.
2. **Export:** `backup::export` (`VACUUM INTO` temp → stream attachment → cleanup) + route + the
   valid-SQLite + attachment-header test.
3. **Info:** `backup::info` + route + test.
4. **Import:** `backup::import` (header guard → validate read-only → upgrade-if-older →
   pre-import snapshot → atomic ATTACH replace → cleanup → `Reseed`) + the 1 GiB body-limit layer
   + round-trip / secret / reject-garbage / reject-newer / snapshot-created tests.
5. **Frontend:** Backup & restore Settings section + `api.ts` (`getBackupInfo`, `importBackup`,
   interfaces) + the confirm-gate/reload flow + tests.
6. **Live acceptance & merge:** real container export→mutate→import→reload smoke, README backup
   runbook updated to mention the in-app path, then merge.

---

## 14. Documented boundaries (recap)

- **Full snapshot, secrets included** — the download bears webhook/discord/ntfy tokens and
  `inline:` auth in plaintext; UI flags it sensitive. SMTP password is external (never in the DB).
- **Atomic live replace, no restart** — one deferred-FK transaction; pre-import snapshot kept on
  disk (`pre-import-<epoch>.db`, not auto-pruned in v1).
- **Scheduler re-armed** via `SchedCmd::Reseed`; other loops self-adapt. **Anchor host list needs
  a restart** to pick up an imported `anchors` change (read once at boot).
- **In-flight probe race** during replace is possible but non-corrupting (FK-guarded), documented,
  not quiesced.
- **Schema version gate:** older backups are migrated up on import; a **newer**-than-current
  backup is rejected (can't downgrade). Version pinned by the exported `schema_migrations`.
- **Body limit:** import route raised to 1 GiB (vs axum's 2 MB default) — required for real DBs.
- **Cross-tab staleness:** only the importing tab reloads; others refresh manually.
- **DB size in `info`** excludes WAL sidecars (labeled approx).

---

*End of P4.5 spec. §4–§6 define behavior, §7–§9 plumbing/API/UI, §10 safety, §12 testing —
build-ready for the implementation plan.*
