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
    Ssl,
    Heartbeat,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Trigger {
    Down,
    Recovered,
    SslExpiring,
    SslInvalid,
    DomainExpiring,
    HeartbeatMissed,
}
impl Trigger {
    pub fn as_str(&self) -> &'static str {
        match self {
            Trigger::Down => "down",
            Trigger::Recovered => "recovered",
            Trigger::SslExpiring => "ssl_expiring",
            Trigger::SslInvalid => "ssl_invalid",
            Trigger::DomainExpiring => "domain_expiring",
            Trigger::HeartbeatMissed => "heartbeat_missed",
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
    pub ssl_check_enabled: bool,
    pub ssl_alert_days: String,
    pub domain_check_enabled: bool,
    pub domain_alert_days: String,
    /// Never serialized — a security requirement (§9/§10): the push-ping
    /// secret must never leak via any `Monitor` payload (list, get, SSE
    /// snapshot, create/update response). The dedicated heartbeat endpoint
    /// (Task 2) returns it explicitly instead.
    #[serde(skip_serializing)]
    pub heartbeat_token: Option<String>,
    pub heartbeat_grace_seconds: i64,
    pub last_ping_at: Option<Ts>,
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
        let ssl_check_enabled_raw: i64 = row.try_get("ssl_check_enabled")?;
        let domain_check_enabled_raw: i64 = row.try_get("domain_check_enabled")?;
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
            ssl_check_enabled: ssl_check_enabled_raw != 0,
            ssl_alert_days: row.try_get("ssl_alert_days")?,
            domain_check_enabled: domain_check_enabled_raw != 0,
            domain_alert_days: row.try_get("domain_alert_days")?,
            heartbeat_token: row.try_get::<Option<String>, _>("heartbeat_token")?,
            heartbeat_grace_seconds: row.try_get::<i64, _>("heartbeat_grace_seconds")?,
            last_ping_at: row.try_get::<Option<i64>, _>("last_ping_at")?,
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

#[derive(Clone, Debug, Serialize)]
pub struct SslCert {
    pub monitor_id: i64,
    pub issuer: Option<String>,
    pub subject: Option<String>,
    pub valid_from: Option<i64>,
    pub valid_until: Option<i64>,
    pub days_remaining: Option<i64>,
    pub is_valid: Option<bool>,
    pub chain_ok: Option<bool>,
    pub hostname_match: Option<bool>,
    pub self_signed: Option<bool>,
    pub error: Option<String>,
    pub alerted_days: Option<i64>,
    pub invalid_alerted: bool,
    pub last_checked: Option<i64>,
}

impl<'r> sqlx::FromRow<'r, sqlx::sqlite::SqliteRow> for SslCert {
    fn from_row(row: &'r sqlx::sqlite::SqliteRow) -> sqlx::Result<Self> {
        use sqlx::Row;
        let is_valid_raw: Option<i64> = row.try_get("is_valid")?;
        let chain_ok_raw: Option<i64> = row.try_get("chain_ok")?;
        let hostname_match_raw: Option<i64> = row.try_get("hostname_match")?;
        let self_signed_raw: Option<i64> = row.try_get("self_signed")?;
        let invalid_alerted_raw: i64 = row.try_get("invalid_alerted")?;
        Ok(SslCert {
            monitor_id: row.try_get("monitor_id")?,
            issuer: row.try_get("issuer")?,
            subject: row.try_get("subject")?,
            valid_from: row.try_get("valid_from")?,
            valid_until: row.try_get("valid_until")?,
            days_remaining: row.try_get("days_remaining")?,
            is_valid: is_valid_raw.map(|v| v != 0),
            chain_ok: chain_ok_raw.map(|v| v != 0),
            hostname_match: hostname_match_raw.map(|v| v != 0),
            self_signed: self_signed_raw.map(|v| v != 0),
            error: row.try_get("error")?,
            alerted_days: row.try_get("alerted_days")?,
            invalid_alerted: invalid_alerted_raw != 0,
            last_checked: row.try_get("last_checked")?,
        })
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct DomainInfo {
    pub monitor_id: i64,
    pub registrar: Option<String>,
    pub expiry_date: Option<i64>,
    pub days_remaining: Option<i64>,
    pub name_servers: Option<String>,
    pub status_codes: Option<String>,
    pub queryable: Option<bool>,
    pub source: Option<String>,
    pub alerted_days: Option<i64>,
    pub last_checked: Option<i64>,
}

impl<'r> sqlx::FromRow<'r, sqlx::sqlite::SqliteRow> for DomainInfo {
    fn from_row(row: &'r sqlx::sqlite::SqliteRow) -> sqlx::Result<Self> {
        use sqlx::Row;
        let queryable_raw: Option<i64> = row.try_get("queryable")?;
        Ok(DomainInfo {
            monitor_id: row.try_get("monitor_id")?,
            registrar: row.try_get("registrar")?,
            expiry_date: row.try_get("expiry_date")?,
            days_remaining: row.try_get("days_remaining")?,
            name_servers: row.try_get("name_servers")?,
            status_codes: row.try_get("status_codes")?,
            queryable: queryable_raw.map(|v| v != 0),
            source: row.try_get("source")?,
            alerted_days: row.try_get("alerted_days")?,
            last_checked: row.try_get("last_checked")?,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Connectivity {
    Online,
    Offline,
}

/// A maintenance window (P4.2): scheduled downtime that suppresses alerts
/// and/or checks for in-scope monitors. `target_ref` is the raw JSON
/// string as stored (NULL for `all`, a JSON string for `tag`, a JSON array
/// of ints for `monitors`) — the resolve module (a later task) parses it.
/// `#[derive(sqlx::FromRow)]` per the `Channel` precedent (api/channels.rs):
/// `is_active: bool` auto-decodes from the INTEGER column, no manual
/// `FromRow` impl needed.
///
/// Deliberately NOT a `Status` variant — maintenance is a client-side
/// display overlay (see the P4.2 design spec), so this struct has no
/// bearing on `monitors.status`.
#[derive(Clone, Debug, sqlx::FromRow, Serialize)]
pub struct MaintenanceWindow {
    pub id: i64,
    pub name: String,
    pub scope: String, // all|tag|monitors
    pub target_ref: Option<String>,
    pub starts_at: Ts,
    pub ends_at: Ts,
    pub recurrence: Option<String>,
    pub suppress: String, // alerts|checks
    pub is_active: bool,
    pub created_at: Ts,
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
fn d_ssl_alert_days() -> String {
    "[30,14,7,3,1]".to_string()
}
fn d_domain_alert_days() -> String {
    "[45,30,14,7]".to_string()
}
fn default_grace() -> i64 {
    60
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
    #[serde(default)]
    pub ssl_check_enabled: bool,
    #[serde(default = "d_ssl_alert_days")]
    pub ssl_alert_days: String,
    #[serde(default)]
    pub domain_check_enabled: bool,
    #[serde(default = "d_domain_alert_days")]
    pub domain_alert_days: String,
    #[serde(default = "default_grace")]
    pub heartbeat_grace_seconds: i64,
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
    pub ssl_check_enabled: Option<bool>,
    pub ssl_alert_days: Option<String>,
    pub domain_check_enabled: Option<bool>,
    pub domain_alert_days: Option<String>,
    pub heartbeat_grace_seconds: Option<i64>,
}

pub fn default_suppress() -> String {
    "alerts".into()
}

#[derive(Clone, Debug, Deserialize)]
pub struct CreateMaintenanceWindowDto {
    pub name: String,
    pub scope: String,
    #[serde(default)]
    pub target_ref: Option<serde_json::Value>,
    pub starts_at: i64,
    pub ends_at: i64,
    #[serde(default)]
    pub recurrence: Option<String>,
    #[serde(default = "default_suppress")]
    pub suppress: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct UpdateMaintenanceWindowDto {
    pub name: Option<String>,
    pub scope: Option<String>,
    #[serde(default)]
    pub target_ref: Option<serde_json::Value>,
    pub starts_at: Option<i64>,
    pub ends_at: Option<i64>,
    #[serde(default)]
    pub recurrence: Option<String>,
    pub suppress: Option<String>,
    pub is_active: Option<bool>,
}

/// Validates a maintenance-window create/update payload (P4.2 §7):
/// - `name` non-empty.
/// - `scope` is one of `all`/`tag`/`monitors`.
/// - `target_ref` shape matches `scope`: `monitors` ⇒ a non-empty JSON
///   array of integers; `tag` ⇒ a non-empty JSON string; `all` ⇒ ignored
///   (any value, including `None`, is accepted — the API layer forces it
///   to `NULL` before storage).
/// - `ends_at > starts_at`, computed via `checked_sub` so `starts_at`/
///   `ends_at` at the i64 extremes are rejected as out-of-range rather than
///   overflowing the subtraction, and the resulting duration is capped at
///   366 days (a longer maintenance window is nonsensical, and this bounds
///   the `dur` every downstream consumer — e.g. `resolve.rs`'s cron
///   math — ever sees for an API-created window).
/// - a non-null `recurrence` must split into EXACTLY 5 whitespace-separated
///   fields (rejecting 6/7-field seconds/year forms and `@`-macros, which
///   never split into 5) AND parse successfully under `croner::Cron`.
pub fn validate_window_dto(
    name: &str,
    scope: &str,
    target_ref: &Option<serde_json::Value>,
    starts_at: i64,
    ends_at: i64,
    recurrence: &Option<String>,
) -> Result<(), String> {
    if name.trim().is_empty() {
        return Err("name must not be empty".into());
    }
    match scope {
        "all" => {}
        "tag" => match target_ref {
            Some(serde_json::Value::String(s)) if !s.is_empty() => {}
            _ => return Err("scope 'tag' requires a non-empty string target_ref".into()),
        },
        "monitors" => match target_ref {
            Some(serde_json::Value::Array(arr)) if !arr.is_empty() && arr.iter().all(|v| v.is_i64() || v.is_u64()) => {}
            _ => return Err("scope 'monitors' requires a non-empty array of monitor ids as target_ref".into()),
        },
        _ => return Err("scope must be one of all|tag|monitors".into()),
    }
    let dur = ends_at.checked_sub(starts_at).ok_or_else(|| "window duration is out of range".to_string())?;
    if dur <= 0 {
        return Err("ends_at must be after starts_at".into());
    }
    const MAX_WINDOW_DURATION_SECONDS: i64 = 366 * 24 * 3600;
    if dur > MAX_WINDOW_DURATION_SECONDS {
        return Err("window duration exceeds 366 days".into());
    }
    if let Some(expr) = recurrence {
        let fields: Vec<&str> = expr.split_whitespace().collect();
        if fields.len() != 5 {
            return Err("recurrence must be exactly 5 whitespace-separated fields".into());
        }
        if croner::Cron::new(expr).parse().is_err() {
            return Err("recurrence is not a valid 5-field cron expression".into());
        }
    }
    Ok(())
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
        ssl_check_enabled: false,
        ssl_alert_days: "[30,14,7,3,1]".to_string(),
        domain_check_enabled: false,
        domain_alert_days: "[45,30,14,7]".to_string(),
        heartbeat_token: None,
        heartbeat_grace_seconds: 60,
        last_ping_at: None,
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

#[cfg(test)]
mod maintenance_window_validation_tests {
    use super::validate_window_dto;
    use serde_json::json;

    #[test]
    fn valid_create_all_scope() {
        assert!(validate_window_dto("nightly", "all", &None, 1000, 2000, &None).is_ok());
    }

    #[test]
    fn valid_create_monitors_scope_with_cron() {
        let target = Some(json!([1, 2, 3]));
        assert!(validate_window_dto(
            "db upgrade",
            "monitors",
            &target,
            1000,
            2000,
            &Some("0 2 * * 0".to_string())
        )
        .is_ok());
    }

    #[test]
    fn valid_create_tag_scope() {
        let target = Some(json!("prod"));
        assert!(validate_window_dto("tag window", "tag", &target, 1000, 2000, &None).is_ok());
    }

    #[test]
    fn empty_name_rejected() {
        assert!(validate_window_dto("", "all", &None, 1000, 2000, &None).is_err());
    }

    #[test]
    fn bad_scope_rejected() {
        assert!(validate_window_dto("w", "bogus", &None, 1000, 2000, &None).is_err());
    }

    #[test]
    fn monitors_scope_non_array_target_ref_rejected() {
        let target = Some(json!("not-an-array"));
        assert!(validate_window_dto("w", "monitors", &target, 1000, 2000, &None).is_err());
    }

    #[test]
    fn monitors_scope_empty_array_target_ref_rejected() {
        let target = Some(json!([]));
        assert!(validate_window_dto("w", "monitors", &target, 1000, 2000, &None).is_err());
    }

    #[test]
    fn monitors_scope_missing_target_ref_rejected() {
        assert!(validate_window_dto("w", "monitors", &None, 1000, 2000, &None).is_err());
    }

    #[test]
    fn tag_scope_non_string_target_ref_rejected() {
        let target = Some(json!(["prod"]));
        assert!(validate_window_dto("w", "tag", &target, 1000, 2000, &None).is_err());
    }

    #[test]
    fn tag_scope_empty_string_target_ref_rejected() {
        let target = Some(json!(""));
        assert!(validate_window_dto("w", "tag", &target, 1000, 2000, &None).is_err());
    }

    #[test]
    fn ends_at_equal_starts_at_rejected() {
        assert!(validate_window_dto("w", "all", &None, 2000, 2000, &None).is_err());
    }

    #[test]
    fn ends_at_before_starts_at_rejected() {
        assert!(validate_window_dto("w", "all", &None, 2000, 1000, &None).is_err());
    }

    #[test]
    fn extreme_bounds_overflow_rejected_not_panics() {
        // starts_at/ends_at at the i64 extremes must be rejected via a
        // checked_sub, not panic (debug/CI overflow-checks) or wrap
        // (release) inside the validator itself.
        assert!(validate_window_dto("w", "all", &None, i64::MIN, i64::MAX, &None).is_err());
    }

    #[test]
    fn duration_over_366_days_rejected() {
        let too_long = 366 * 24 * 3600 + 1;
        assert!(validate_window_dto("w", "all", &None, 0, too_long, &None).is_err());
    }

    #[test]
    fn duration_exactly_366_days_accepted() {
        let max_ok = 366 * 24 * 3600;
        assert!(validate_window_dto("w", "all", &None, 0, max_ok, &None).is_ok());
    }

    #[test]
    fn six_field_cron_rejected() {
        // seconds-prefixed 6-field form must be rejected — the contract is
        // strictly 5 fields.
        let rec = Some("0 18 * * * 5".to_string());
        assert!(validate_window_dto("w", "all", &None, 1000, 2000, &rec).is_err());
    }

    #[test]
    fn macro_recurrence_rejected() {
        // "@daily" splits into a single whitespace field, not 5.
        let rec = Some("@daily".to_string());
        assert!(validate_window_dto("w", "all", &None, 1000, 2000, &rec).is_err());
    }

    #[test]
    fn malformed_five_field_cron_rejected() {
        let rec = Some("99 * * * *".to_string());
        assert!(validate_window_dto("w", "all", &None, 1000, 2000, &rec).is_err());
    }

    #[test]
    fn valid_five_field_cron_accepted() {
        let rec = Some("0 2 * * 0".to_string());
        assert!(validate_window_dto("w", "all", &None, 1000, 2000, &rec).is_ok());
    }
}
