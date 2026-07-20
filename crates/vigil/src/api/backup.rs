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
