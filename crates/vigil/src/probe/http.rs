//! HTTP(S) prober: performs the actual request for `http`/`keyword` monitors,
//! classifies failures into a `Cause`, and resolves `auth_ref` secrets.

use crate::models::{Cause, Monitor, ProbeOutcome};
use crate::status_codes;
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// Default `User-Agent` for probe requests. Many sites (Cloudflare,
/// ModSecurity, and other WAF/bot rules) return 4xx to requests that send no
/// User-Agent, which would make Vigil report a false DOWN for a site that is
/// actually up. This identifying, browser-compatible UA clears those rules
/// while honestly naming the tool. A monitor may override it with its own
/// `User-Agent` request header (reqwest merges client-default headers with
/// `or_insert` semantics, so a per-request header wins — see the
/// `custom_user_agent_overrides_default` test).
pub const DEFAULT_USER_AGENT: &str =
    "Mozilla/5.0 (compatible; Vigil/1.0; +https://github.com/craigderington/vigil)";

/// Cached `reqwest::Client`s keyed on `(verify_ssl, follow_redirects)` —
/// both are `ClientBuilder`-level settings, so we build one client per
/// combination and reuse it (a fresh client per probe would defeat
/// connection pooling).
static CLIENTS: OnceLock<Mutex<HashMap<(bool, bool), reqwest::Client>>> = OnceLock::new();

fn client_for(verify_ssl: bool, follow_redirects: bool) -> reqwest::Client {
    let cache = CLIENTS.get_or_init(|| Mutex::new(HashMap::new()));
    let key = (verify_ssl, follow_redirects);

    // Lock, clone an existing client out (or build+insert+clone), then drop
    // the guard before returning — callers must never hold this across an
    // `.await`.
    let mut guard = cache.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(existing) = guard.get(&key) {
        return existing.clone();
    }
    let built = reqwest::Client::builder()
        .user_agent(DEFAULT_USER_AGENT)
        .danger_accept_invalid_certs(!verify_ssl)
        .redirect(if follow_redirects {
            reqwest::redirect::Policy::limited(10)
        } else {
            reqwest::redirect::Policy::none()
        })
        .build()
        .unwrap_or_else(|_| reqwest::Client::new());
    guard.insert(key, built.clone());
    built
}

/// Resolves an `auth_ref` string into the secret value it points at.
/// `env:VAR` reads the environment variable `VAR`; `inline:x` yields `x`
/// literally. Anything else (including `None`) resolves to `None`.
pub fn resolve_auth(auth_ref: &Option<String>) -> Option<String> {
    let raw = auth_ref.as_deref()?;
    if let Some(var) = raw.strip_prefix("env:") {
        std::env::var(var).ok()
    } else {
        raw.strip_prefix("inline:").map(|val| val.to_string())
    }
}

fn classify_send_error(e: &reqwest::Error) -> Cause {
    if e.is_timeout() {
        Cause::Timeout
    } else if e.is_connect() {
        Cause::Connection
    } else if e.to_string().to_lowercase().contains("dns") {
        Cause::Dns
    } else {
        Cause::Connection
    }
}

/// Runs a single HTTP(S) probe for `m` and returns the outcome. Never
/// panics on network failure — errors are classified into a `Cause` and
/// returned as a failed `ProbeOutcome`.
pub async fn probe(m: &Monitor) -> ProbeOutcome {
    let Some(url) = m.url.as_deref() else {
        return ProbeOutcome {
            ok: false,
            response_time_ms: None,
            status_code: None,
            error_message: Some("monitor has no url configured".to_string()),
            resolved_ip: None,
            cause: Some(Cause::Connection),
        };
    };

    let client = client_for(m.verify_ssl, m.follow_redirects);

    let method =
        reqwest::Method::from_bytes(m.method.as_bytes()).unwrap_or(reqwest::Method::GET);

    let mut req = client
        .request(method, url)
        .timeout(Duration::from_secs(m.timeout_seconds as u64));

    if let Some(headers) = &m.headers {
        if let Ok(map) = serde_json::from_str::<HashMap<String, String>>(headers) {
            for (k, v) in map {
                req = req.header(k, v);
            }
        }
    }

    if let Some(body) = &m.body {
        req = req.body(body.clone());
    }

    match m.auth_type.as_deref() {
        Some("basic") => {
            if let Some(value) = resolve_auth(&m.auth_ref) {
                let (user, pass) = match value.split_once(':') {
                    Some((u, p)) => (u.to_string(), p.to_string()),
                    None => (value, String::new()),
                };
                req = req.basic_auth(user, Some(pass));
            }
        }
        Some("bearer") => {
            if let Some(value) = resolve_auth(&m.auth_ref) {
                req = req.bearer_auth(value);
            }
        }
        Some("header") => {
            if let Some(value) = resolve_auth(&m.auth_ref) {
                let (name, val) = match value.split_once(':') {
                    Some((n, v)) => (n.trim().to_string(), v.trim().to_string()),
                    None => (value, String::new()),
                };
                req = req.header(name, val);
            }
        }
        _ => {}
    }

    let start = Instant::now();
    let result = req.send().await;
    let mut elapsed = start.elapsed().as_millis() as i64;

    match result {
        Err(e) => {
            let cause = classify_send_error(&e);
            ProbeOutcome {
                ok: false,
                response_time_ms: Some(elapsed),
                status_code: None,
                error_message: Some(e.to_string()),
                resolved_ip: None,
                cause: Some(cause),
            }
        }
        Ok(mut resp) => {
            let code = resp.status().as_u16();
            let expected_ok = status_codes::parse_expected(&m.expected_status_codes)
                .map(|ranges| status_codes::matches(&ranges, code))
                .unwrap_or(false);

            if !expected_ok {
                return ProbeOutcome {
                    ok: false,
                    response_time_ms: Some(elapsed),
                    status_code: Some(code as i64),
                    error_message: Some(format!("unexpected status {code}")),
                    resolved_ip: None,
                    cause: Some(Cause::Status),
                };
            }

            if m.r#type == "keyword" {
                if let Some(keyword) = m.keyword.as_deref() {
                    // Bounded chunk loop — never buffer the whole body
                    // (`resp.bytes()` would), cap at 2 MiB. Distinguish a
                    // genuine end-of-stream (`Ok(None)`) from a transport
                    // error mid-body (`Err`) — collapsing both to "stop
                    // reading" would misreport a dropped connection as a
                    // keyword mismatch on whatever partial bytes arrived.
                    let mut buf = Vec::new();
                    loop {
                        match resp.chunk().await {
                            Ok(Some(chunk)) => {
                                buf.extend_from_slice(&chunk);
                                if buf.len() >= 2 * 1024 * 1024 {
                                    buf.truncate(2 * 1024 * 1024);
                                    break;
                                }
                            }
                            Ok(None) => break,
                            Err(e) => {
                                elapsed = start.elapsed().as_millis() as i64;
                                let cause = if e.is_timeout() {
                                    Cause::Timeout
                                } else {
                                    Cause::Connection
                                };
                                return ProbeOutcome {
                                    ok: false,
                                    response_time_ms: Some(elapsed),
                                    status_code: Some(code as i64),
                                    error_message: Some(e.to_string()),
                                    resolved_ip: None,
                                    cause: Some(cause),
                                };
                            }
                        }
                    }
                    elapsed = start.elapsed().as_millis() as i64;
                    let body = String::from_utf8_lossy(&buf);

                    let (haystack, needle) = if m.keyword_case_sensitive {
                        (body.to_string(), keyword.to_string())
                    } else {
                        (body.to_lowercase(), keyword.to_lowercase())
                    };
                    let contains = haystack.contains(&needle);
                    let mode = m.keyword_mode.as_deref().unwrap_or("present");
                    let keyword_ok = match mode {
                        "absent" => !contains,
                        _ => contains,
                    };

                    if !keyword_ok {
                        return ProbeOutcome {
                            ok: false,
                            response_time_ms: Some(elapsed),
                            status_code: Some(code as i64),
                            error_message: Some(format!("keyword {mode} check failed")),
                            resolved_ip: None,
                            cause: Some(Cause::Keyword),
                        };
                    }
                }
            }

            ProbeOutcome {
                ok: true,
                response_time_ms: Some(elapsed),
                status_code: Some(code as i64),
                error_message: None,
                resolved_ip: None,
                cause: None,
            }
        }
    }
}
