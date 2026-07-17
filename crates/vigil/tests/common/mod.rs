#![allow(dead_code)]

use std::sync::{Arc, Mutex};
use vigil::app::{AppState, SchedCmd};
use vigil::models::*;
use vigil::notify::*;

pub struct TestEnv {
    pub state: AppState,
    pub sent: Arc<Mutex<Vec<(SmtpConfig, EmailMsg)>>>,
    _rx: tokio::sync::mpsc::UnboundedReceiver<SchedCmd>, // kept alive so sched_tx.send never errors
    _dir: tempfile::TempDir,
}

pub async fn fresh_pool() -> (sqlx::SqlitePool, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let pool = vigil::db::connect(dir.path().join("t.db").to_str().unwrap()).await.unwrap();
    (pool, dir)
}

pub async fn test_state() -> TestEnv {
    let (pool, dir) = fresh_pool().await;
    let sent = Arc::new(Mutex::new(Vec::new()));
    let (bus, _busrx) = tokio::sync::broadcast::channel(64);
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let anchor = Arc::new(vigil::anchor::AnchorGate::with_prober(bus.clone(), Box::new(|| true)));
    let state = AppState {
        db: pool,
        bus,
        transport: Arc::new(RecordingTransport { sent: sent.clone() }),
        sched_tx: tx,
        anchor,
    };
    TestEnv { state, sent, _rx: rx, _dir: dir }
}

pub fn test_http_monitor(url: &str, codes: &str) -> Monitor {
    let mut m = vigil::models::test_defaults_monitor();
    m.url = Some(url.into());
    m.expected_status_codes = codes.into();
    m
}

/// Seeds a monitor, an active `email` notification channel, and a
/// `monitor_notifications` attachment (triggers: down, recovered). Returns
/// the monitor id.
pub async fn seed_monitor_with_email_channel(pool: &sqlx::SqlitePool) -> i64 {
    let now = 1_700_000_000i64;
    let mid: i64 = sqlx::query_scalar(
        "INSERT INTO monitors (name, type, url, created_at, updated_at) VALUES (?, 'http', ?, ?, ?) RETURNING id",
    )
    .bind("seed")
    .bind("https://x")
    .bind(now)
    .bind(now)
    .fetch_one(pool)
    .await
    .unwrap();

    let config = r#"{"host":"h","port":25,"security":"none","from":"f@b","to":["a@b"]}"#;
    let cid: i64 = sqlx::query_scalar(
        "INSERT INTO notification_channels (name, type, config, is_active, created_at) VALUES (?, 'email', ?, 1, ?) RETURNING id",
    )
    .bind("seed-channel")
    .bind(config)
    .bind(now)
    .fetch_one(pool)
    .await
    .unwrap();

    sqlx::query(
        "INSERT INTO monitor_notifications (monitor_id, channel_id, triggers) VALUES (?, ?, ?)",
    )
    .bind(mid)
    .bind(cid)
    .bind(r#"["down","recovered"]"#)
    .execute(pool)
    .await
    .unwrap();

    mid
}

pub fn ctx_with(name: &str, url: &str, code: Option<i64>) -> TemplateCtx {
    TemplateCtx {
        monitor_name: name.into(),
        url: url.into(),
        status: "down".into(),
        status_code: code,
        error: None,
        response_time_ms: None,
        duration: None,
        checked_at: 0,
    }
}
