//! REST handlers for whole-database backup export/import (CLAUDE.md §12, §10).
//! Export = a WAL-safe `VACUUM INTO` snapshot streamed as an attachment.
//! Import = validate → migrate-if-older → pre-import snapshot → atomic
//! deferred-FK replace over the live pool → re-seed the scheduler (Task 4).

use std::path::{Path, PathBuf};

use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::extract::State;
use axum::Json;
use serde::Serialize;

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

    // helper to remove the temp import file + its WAL sidecars on every exit
    // (hoisted above the write itself so a failed/partial write is also
    // cleaned up, not just the post-write validation/replace failures)
    let cleanup = |p: &Path| {
        let p = p.to_path_buf();
        async move {
            let _ = tokio::fs::remove_file(&p).await;
            let _ = tokio::fs::remove_file(p.with_extension("db-wal")).await;
            let _ = tokio::fs::remove_file(p.with_extension("db-shm")).await;
        }
    };
    if let Err(e) = tokio::fs::write(&import_path, &body).await {
        cleanup(&import_path).await;
        return Err(ise(format!("write upload: {e}")));
    }

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
        let import_str = match import_path.to_str() {
            Some(s) => s,
            None => { cleanup(&import_path).await; return Err(ise("non-UTF-8 import path".into())); }
        };
        match crate::db::connect(import_str).await {
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
