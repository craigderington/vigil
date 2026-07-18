use serde::{Deserialize, Serialize};
pub type Ts = i64; // unix epoch seconds

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Pending,
    Up,
    Down,
    Paused,
    Unknown,
}
impl Status {
    pub fn as_str(&self) -> &'static str {
        match self {
            Status::Pending => "pending",
            Status::Up => "up",
            Status::Down => "down",
            Status::Paused => "paused",
            Status::Unknown => "unknown",
        }
    }
    pub fn from_db(s: &str) -> Status {
        match s {
            "up" => Status::Up,
            "down" => Status::Down,
            "paused" => Status::Paused,
            "unknown" => Status::Unknown,
            _ => Status::Pending,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Cause {
    Timeout,
    Status,
    Connection,
    Dns,
    Keyword,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Trigger {
    Down,
    Recovered,
}
impl Trigger {
    pub fn as_str(&self) -> &'static str {
        match self {
            Trigger::Down => "down",
            Trigger::Recovered => "recovered",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ProbeOutcome {
    pub ok: bool,
    pub response_time_ms: Option<i64>,
    pub status_code: Option<i64>,
    pub error_message: Option<String>,
    pub resolved_ip: Option<String>,
    pub cause: Option<Cause>, // Some iff !ok
}

#[derive(Clone, Debug, Serialize)]
pub struct Monitor {
    pub id: i64,
    pub name: String,
    pub r#type: String,
    pub url: Option<String>,
    pub method: String,
    pub headers: Option<String>,
    pub body: Option<String>,
    pub auth_type: Option<String>,
    pub auth_ref: Option<String>,
    pub expected_status_codes: String,
    pub interval_seconds: i64,
    pub timeout_seconds: i64,
    pub follow_redirects: bool,
    pub verify_ssl: bool,
    pub confirmation_threshold: i64,
    pub recovery_threshold: i64,
    pub retry_interval_seconds: i64,
    pub host: Option<String>,
    pub port: Option<i64>,
    pub keyword: Option<String>,
    pub keyword_mode: Option<String>,
    pub keyword_case_sensitive: bool,
    pub dns_record_type: Option<String>,
    pub dns_expected_value: Option<String>,
    pub status: Status,
    pub is_paused: bool,
    pub last_checked_at: Option<Ts>,
    pub next_run_at: Option<Ts>,
    pub consecutive_failures: i64,
    pub consecutive_successes: i64,
    pub tags: Option<String>,
    pub sort_order: i64,
    pub created_at: Ts,
    pub updated_at: Ts,
}

impl<'r> sqlx::FromRow<'r, sqlx::sqlite::SqliteRow> for Monitor {
    fn from_row(row: &'r sqlx::sqlite::SqliteRow) -> sqlx::Result<Self> {
        use sqlx::Row;
        let status_raw: String = row.try_get("status")?;
        let follow_redirects_raw: i64 = row.try_get("follow_redirects")?;
        let verify_ssl_raw: i64 = row.try_get("verify_ssl")?;
        let is_paused_raw: i64 = row.try_get("is_paused")?;
        let keyword_case_sensitive_raw: i64 = row.try_get("keyword_case_sensitive")?;
        Ok(Monitor {
            id: row.try_get("id")?,
            name: row.try_get("name")?,
            r#type: row.try_get("type")?,
            url: row.try_get("url")?,
            method: row.try_get("method")?,
            headers: row.try_get("headers")?,
            body: row.try_get("body")?,
            auth_type: row.try_get("auth_type")?,
            auth_ref: row.try_get("auth_ref")?,
            expected_status_codes: row.try_get("expected_status_codes")?,
            interval_seconds: row.try_get("interval_seconds")?,
            timeout_seconds: row.try_get("timeout_seconds")?,
            follow_redirects: follow_redirects_raw != 0,
            verify_ssl: verify_ssl_raw != 0,
            confirmation_threshold: row.try_get("confirmation_threshold")?,
            recovery_threshold: row.try_get("recovery_threshold")?,
            retry_interval_seconds: row.try_get("retry_interval_seconds")?,
            host: row.try_get("host")?,
            port: row.try_get("port")?,
            keyword: row.try_get("keyword")?,
            keyword_mode: row.try_get("keyword_mode")?,
            keyword_case_sensitive: keyword_case_sensitive_raw != 0,
            dns_record_type: row.try_get("dns_record_type")?,
            dns_expected_value: row.try_get("dns_expected_value")?,
            status: Status::from_db(&status_raw),
            is_paused: is_paused_raw != 0,
            last_checked_at: row.try_get("last_checked_at")?,
            next_run_at: row.try_get("next_run_at")?,
            consecutive_failures: row.try_get("consecutive_failures")?,
            consecutive_successes: row.try_get("consecutive_successes")?,
            tags: row.try_get("tags")?,
            sort_order: row.try_get("sort_order")?,
            created_at: row.try_get("created_at")?,
            updated_at: row.try_get("updated_at")?,
        })
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct Incident {
    pub id: i64,
    pub monitor_id: i64,
    pub started_at: Ts,
    pub resolved_at: Option<Ts>,
    pub duration_seconds: Option<i64>,
    pub cause: Option<String>,
    pub status_code: Option<i64>,
    pub error_message: Option<String>,
    pub acknowledged: bool,
}

impl<'r> sqlx::FromRow<'r, sqlx::sqlite::SqliteRow> for Incident {
    fn from_row(row: &'r sqlx::sqlite::SqliteRow) -> sqlx::Result<Self> {
        use sqlx::Row;
        let acknowledged_raw: i64 = row.try_get("acknowledged")?;
        Ok(Incident {
            id: row.try_get("id")?,
            monitor_id: row.try_get("monitor_id")?,
            started_at: row.try_get("started_at")?,
            resolved_at: row.try_get("resolved_at")?,
            duration_seconds: row.try_get("duration_seconds")?,
            cause: row.try_get("cause")?,
            status_code: row.try_get("status_code")?,
            error_message: row.try_get("error_message")?,
            acknowledged: acknowledged_raw != 0,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Connectivity {
    Online,
    Offline,
}

// ---- DTOs ----

fn d_method() -> String {
    "GET".to_string()
}
fn d_codes() -> String {
    "200-299".to_string()
}
fn d_interval() -> i64 {
    300
}
fn d_timeout() -> i64 {
    30
}
fn d_true() -> bool {
    true
}
fn d_conf() -> i64 {
    3
}
fn d_rec() -> i64 {
    1
}
fn d_retry() -> i64 {
    30
}
fn d_http() -> String {
    "http".to_string()
}

#[derive(Clone, Debug, Deserialize)]
pub struct CreateMonitorDto {
    pub name: String,
    #[serde(default = "d_http")]
    pub r#type: String,
    pub url: Option<String>,
    #[serde(default = "d_method")]
    pub method: String,
    pub headers: Option<String>,
    pub body: Option<String>,
    pub auth_type: Option<String>,
    pub auth_ref: Option<String>,
    #[serde(default = "d_codes")]
    pub expected_status_codes: String,
    #[serde(default = "d_interval")]
    pub interval_seconds: i64,
    #[serde(default = "d_timeout")]
    pub timeout_seconds: i64,
    #[serde(default = "d_true")]
    pub follow_redirects: bool,
    #[serde(default = "d_true")]
    pub verify_ssl: bool,
    #[serde(default = "d_conf")]
    pub confirmation_threshold: i64,
    #[serde(default = "d_rec")]
    pub recovery_threshold: i64,
    #[serde(default = "d_retry")]
    pub retry_interval_seconds: i64,
    pub host: Option<String>,
    pub port: Option<i64>,
    pub keyword: Option<String>,
    pub keyword_mode: Option<String>,
    #[serde(default)]
    pub keyword_case_sensitive: bool,
    pub dns_record_type: Option<String>,
    pub dns_expected_value: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct UpdateMonitorDto {
    pub name: Option<String>,
    pub url: Option<String>,
    pub method: Option<String>,
    pub headers: Option<String>,
    pub body: Option<String>,
    pub auth_type: Option<String>,
    pub auth_ref: Option<String>,
    pub expected_status_codes: Option<String>,
    pub interval_seconds: Option<i64>,
    pub timeout_seconds: Option<i64>,
    pub follow_redirects: Option<bool>,
    pub verify_ssl: Option<bool>,
    pub confirmation_threshold: Option<i64>,
    pub recovery_threshold: Option<i64>,
    pub retry_interval_seconds: Option<i64>,
    pub host: Option<String>,
    pub port: Option<i64>,
    pub keyword: Option<String>,
    pub keyword_mode: Option<String>,
    pub keyword_case_sensitive: Option<bool>,
    pub dns_record_type: Option<String>,
    pub dns_expected_value: Option<String>,
}

/// Fully-defaulted http Monitor fixture for tests in later tasks.
pub fn test_defaults_monitor() -> Monitor {
    Monitor {
        id: 0,
        name: "test".to_string(),
        r#type: "http".to_string(),
        url: Some("https://example.com".to_string()),
        method: "GET".to_string(),
        headers: None,
        body: None,
        auth_type: None,
        auth_ref: None,
        expected_status_codes: "200-299".to_string(),
        interval_seconds: 300,
        timeout_seconds: 30,
        follow_redirects: true,
        verify_ssl: true,
        confirmation_threshold: 3,
        recovery_threshold: 1,
        retry_interval_seconds: 30,
        host: None,
        port: None,
        keyword: None,
        keyword_mode: None,
        keyword_case_sensitive: false,
        dns_record_type: None,
        dns_expected_value: None,
        status: Status::Pending,
        is_paused: false,
        last_checked_at: None,
        next_run_at: None,
        consecutive_failures: 0,
        consecutive_successes: 0,
        tags: None,
        sort_order: 0,
        created_at: 0,
        updated_at: 0,
    }
}
