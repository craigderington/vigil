//! Domain registration expiry check (§6 of the spec).
//!
//! Resolves a monitor's `host` down to its registrable domain (eTLD+1),
//! queries RDAP (`rdap.org`, which redirects to the right registry) for
//! registration expiry, and falls back to WHOIS (port 43, via the IANA
//! bootstrap server) for TLDs that don't publish RDAP data.
//!
//! The one invariant every code path here must respect: **`transient` and
//! a definitive answer are mutually exclusive.** A 429/5xx/timeout/
//! connection failure means "unknown, ask again later" (`transient:
//! true`) — `queryable` is meaningless in that case and callers must not
//! persist field data over a last-known-good value. A non-transient
//! `queryable: false` means the registry gave us a real, final answer and
//! that answer has no expiry (a redacted TLD, RDAP 404 + no parseable
//! WHOIS expiry, etc.) — that IS a durable state worth persisting. A
//! network hiccup must never be reported as the latter, and a genuinely
//! unqueryable domain must never be reported as the former.

use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

/// Outcome of a single domain-expiry check.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DomainResult {
    pub registrar: Option<String>,
    pub expiry_date: Option<i64>,
    pub name_servers: Vec<String>,
    pub status_codes: Vec<String>,
    /// `true` => expiry known (definitive). `false` => registry not
    /// queryable (also definitive — only meaningful when `transient` is
    /// `false`; see the module doc for the invariant).
    pub queryable: bool,
    /// `"rdap"` | `"whois"` | `None` (network failure before any registry
    /// answered).
    pub source: Option<String>,
    /// `true` => transient failure (429/5xx/timeout/net error). Callers
    /// must not persist `queryable`/field data when this is set — only
    /// advance the last-checked clock and try again later.
    pub transient: bool,
}

impl DomainResult {
    fn transient() -> Self {
        DomainResult { transient: true, ..Default::default() }
    }

    /// A definitive "registry answered, no expiry available" result.
    fn not_queryable(source: &str) -> Self {
        DomainResult { source: Some(source.to_string()), queryable: false, transient: false, ..Default::default() }
    }
}

/// Two-label public suffixes common enough to be worth hardcoding. This is
/// a pragmatic subset, **not** the full Public Suffix List (no PSL crate
/// dependency for this task) — good enough to turn `x.example.co.uk` into
/// `example.co.uk` instead of the wrong `co.uk`.
const MULTI_SUFFIX: &[&str] = &[
    "co.uk", "org.uk", "gov.uk", "ac.uk", "me.uk", "ltd.uk", "plc.uk", "net.uk", "sch.uk",
    "com.au", "net.au", "org.au", "edu.au", "gov.au", "id.au",
    "co.jp", "or.jp", "ne.jp", "ac.jp", "ad.jp", "ed.jp", "go.jp", "gr.jp",
    "co.nz", "org.nz", "net.nz", "govt.nz", "ac.nz",
    "com.br", "net.br", "org.br",
    "co.in", "org.in", "net.in", "gov.in", "ac.in", "firm.in",
    "co.za", "org.za", "net.za", "gov.za",
    "co.kr", "or.kr", "ne.kr",
    "com.cn", "net.cn", "org.cn", "gov.cn",
    "com.tw", "org.tw", "net.tw",
    "com.mx", "org.mx", "net.mx",
    "com.sg", "org.sg", "net.sg", "edu.sg", "gov.sg",
    "co.id", "or.id", "net.id",
    "com.hk", "org.hk", "net.hk", "edu.hk", "gov.hk",
    "co.il", "org.il", "net.il",
    "co.th", "or.th", "ac.th", "in.th",
    "com.ar", "net.ar", "org.ar",
];

/// Reduces `host` to its registrable domain (eTLD+1): the public suffix
/// plus one label. Pure, no I/O. Hosts with two or fewer labels (already
/// an apex, or something degenerate like `localhost`) are returned as-is
/// (lowercased).
pub fn registrable_domain(host: &str) -> String {
    let host = host.trim().trim_end_matches('.').to_ascii_lowercase();
    let labels: Vec<&str> = host.split('.').filter(|s| !s.is_empty()).collect();
    if labels.len() <= 2 {
        return labels.join(".");
    }
    // `labels.len() > 2` here (the `<= 2` case already returned above), so
    // indexing 3 back from the end is always in bounds.
    let last_two = format!("{}.{}", labels[labels.len() - 2], labels[labels.len() - 1]);
    if MULTI_SUFFIX.contains(&last_two.as_str()) {
        labels[labels.len() - 3..].join(".")
    } else {
        labels[labels.len() - 2..].join(".")
    }
}

/// Parses an RDAP domain JSON object into a [`DomainResult`]. Pure, no
/// I/O. Returns `None` when the document has no usable `events` array at
/// all (callers turn that into a definitive non-transient "not queryable"
/// result, since RDAP itself answered — see [`check`]).
pub fn parse_rdap(json: &serde_json::Value) -> Option<DomainResult> {
    let events = json.get("events")?.as_array()?;

    let expiry_date = events.iter().find_map(|event| {
        let action = event.get("eventAction")?.as_str()?;
        if action != "expiration" {
            return None;
        }
        let date_str = event.get("eventDate")?.as_str()?;
        chrono::DateTime::parse_from_rfc3339(date_str).ok().map(|dt| dt.timestamp())
    });

    let registrar = json.get("entities").and_then(|v| v.as_array()).and_then(|entities| {
        entities.iter().find_map(|entity| {
            let roles = entity.get("roles")?.as_array()?;
            let is_registrar = roles.iter().any(|r| r.as_str() == Some("registrar"));
            if !is_registrar {
                return None;
            }
            let vcard = entity.get("vcardArray")?.as_array()?;
            let props = vcard.get(1)?.as_array()?;
            props.iter().find_map(|prop| {
                let entry = prop.as_array()?;
                if entry.first()?.as_str()? != "fn" {
                    return None;
                }
                entry.last()?.as_str().map(|s| s.to_string())
            })
        })
    });

    let name_servers = json
        .get("nameservers")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|ns| ns.get("ldhName").and_then(|v| v.as_str()).map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();

    let status_codes = json
        .get("status")
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().filter_map(|s| s.as_str().map(|s| s.to_string())).collect())
        .unwrap_or_default();

    let queryable = expiry_date.is_some();

    Some(DomainResult {
        registrar,
        expiry_date,
        name_servers,
        status_codes,
        queryable,
        source: Some("rdap".to_string()),
        transient: false,
    })
}

/// Runs a domain-expiry check for `host`. Reduces to the registrable
/// domain, queries RDAP first, then falls back to WHOIS for domains RDAP
/// doesn't know about. Never panics; every network/parse failure is
/// classified into either `transient: true` (try again later) or a
/// definitive `queryable: false` (see the module doc for the invariant).
pub async fn check(host: &str, timeout_secs: u64) -> DomainResult {
    let d = registrable_domain(host);
    let timeout = Duration::from_secs(timeout_secs);

    let client = reqwest::Client::builder().timeout(timeout).build().unwrap_or_else(|_| reqwest::Client::new());

    let url = format!("https://rdap.org/domain/{d}");
    let response = client.get(&url).header(reqwest::header::ACCEPT, "application/rdap+json").send().await;

    match response {
        Ok(resp) => {
            let status = resp.status();
            if status.is_success() {
                match resp.json::<serde_json::Value>().await {
                    Ok(json) => match parse_rdap(&json) {
                        Some(result) if result.expiry_date.is_some() => result,
                        // RDAP answered but had no expiration event (or no
                        // events at all) — a real, final answer: not
                        // transient, not queryable.
                        _ => DomainResult::not_queryable("rdap"),
                    },
                    // A 200 with an unparseable body is still a real
                    // response from the RDAP aggregator, not a network
                    // hiccup — treat as a definitive non-answer rather
                    // than transient.
                    Err(_) => DomainResult::not_queryable("rdap"),
                }
            } else if status.as_u16() == 404 {
                whois_fallback(&d, timeout_secs).await
            } else if status.as_u16() == 429 || status.is_server_error() {
                DomainResult::transient()
            } else {
                // Any other 4xx: RDAP definitively declined to answer.
                DomainResult::not_queryable("rdap")
            }
        }
        // Connection refused, DNS failure, TLS error, or a timeout that
        // fired before/while sending — all transient by nature.
        Err(_) => DomainResult::transient(),
    }
}

/// Sends `{domain}\r\n` to `server:43` and reads the full response,
/// wrapped in `timeout`. Returns `None` on any connect/write/read/timeout
/// failure — callers treat that uniformly as transient, since a WHOIS
/// server refusing to talk is a connectivity problem, not an answer.
async fn whois_query(server: &str, domain: &str, timeout: Duration) -> Option<String> {
    let fut = async {
        let mut stream = TcpStream::connect((server, 43)).await.ok()?;
        stream.write_all(format!("{domain}\r\n").as_bytes()).await.ok()?;
        let mut buf = Vec::new();
        stream.read_to_end(&mut buf).await.ok()?;
        Some(String::from_utf8_lossy(&buf).into_owned())
    };
    tokio::time::timeout(timeout, fut).await.ok().flatten()
}

/// Finds a `refer:`/`whois:` referral line in an IANA bootstrap response
/// and returns the referred WHOIS server hostname (lowercased — WHOIS
/// hostnames are case-insensitive and this value is only ever used to
/// open a TCP connection).
fn find_referral(text: &str) -> Option<String> {
    text.lines().find_map(|line| {
        let lower = line.to_ascii_lowercase();
        lower
            .strip_prefix("refer:")
            .or_else(|| lower.strip_prefix("whois:"))
            .map(|rest| rest.trim().to_string())
            .filter(|s| !s.is_empty())
    })
}

/// Expiry-line labels to search for, in priority order (first match wins).
const EXPIRY_LABELS: &[&str] = &[
    "registry expiry date:",
    "registrar registration expiration date:",
    "expiry date:",
    "expiration date:",
    "paid-till:",
];

/// Parses a single date token against a small list of formats seen across
/// WHOIS registries (ISO-ish, `DD-Mon-YYYY`, dot- and slash-separated).
/// Only the first whitespace-delimited token is considered — expiry lines
/// occasionally carry trailing annotations after the date itself.
fn parse_whois_date(raw: &str) -> Option<i64> {
    let token = raw.split_whitespace().next()?;

    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(token) {
        return Some(dt.timestamp());
    }

    const FORMATS: &[&str] = &["%Y-%m-%dT%H:%M:%SZ", "%Y-%m-%d", "%d-%b-%Y", "%Y.%m.%d", "%Y/%m/%d"];
    for fmt in FORMATS {
        if let Ok(ndt) = chrono::NaiveDateTime::parse_from_str(token, fmt) {
            return Some(ndt.and_utc().timestamp());
        }
        if let Ok(d) = chrono::NaiveDate::parse_from_str(token, fmt) {
            return d.and_hms_opt(0, 0, 0).map(|dt| dt.and_utc().timestamp());
        }
    }
    None
}

/// Parses a raw WHOIS response body into a [`DomainResult`]. Always
/// `transient: false` — by the time this runs, the WHOIS server has
/// already answered with actual text, so whatever we extract (or fail to
/// extract) is a definitive result.
fn parse_whois(text: &str) -> DomainResult {
    let expiry_date = EXPIRY_LABELS.iter().find_map(|label| {
        text.lines().find_map(|line| {
            let lower = line.to_ascii_lowercase();
            lower.strip_prefix(label).and_then(parse_whois_date)
        })
    });

    let registrar = text.lines().find_map(|line| {
        let lower = line.to_ascii_lowercase();
        if lower.starts_with("registrar:") {
            line.split_once(':').map(|(_, v)| v.trim().to_string()).filter(|s| !s.is_empty())
        } else {
            None
        }
    });

    let name_servers: Vec<String> = text
        .lines()
        .filter_map(|line| {
            let lower = line.to_ascii_lowercase();
            if lower.starts_with("name server:") || lower.starts_with("nserver:") {
                line.split_once(':').map(|(_, v)| v.trim().to_ascii_lowercase())
            } else {
                None
            }
        })
        .filter(|s| !s.is_empty())
        .collect();

    let status_codes: Vec<String> = text
        .lines()
        .filter_map(|line| {
            let lower = line.to_ascii_lowercase();
            if lower.starts_with("domain status:") || lower.starts_with("status:") {
                line.split_once(':').and_then(|(_, v)| v.split_whitespace().next()).map(|s| s.to_string())
            } else {
                None
            }
        })
        .filter(|s| !s.is_empty())
        .collect();

    DomainResult {
        registrar,
        expiry_date,
        name_servers,
        status_codes,
        queryable: expiry_date.is_some(),
        source: Some("whois".to_string()),
        transient: false,
    }
}

/// WHOIS fallback for TLDs without RDAP data: queries the IANA bootstrap
/// server for `domain`'s referral, then queries the referred registry
/// WHOIS server. Any connectivity failure at either hop is `transient`;
/// only a server that actually answered (but has no referral, or no
/// parseable expiry) yields a definitive `queryable: false`.
async fn whois_fallback(domain: &str, timeout_secs: u64) -> DomainResult {
    let timeout = Duration::from_secs(timeout_secs);

    let Some(iana_resp) = whois_query("whois.iana.org", domain, timeout).await else {
        return DomainResult::transient();
    };

    let Some(referral) = find_referral(&iana_resp) else {
        // IANA answered but has no referral for this TLD/domain — a real,
        // final "we don't know", not a hiccup.
        return DomainResult::not_queryable("whois");
    };

    let Some(whois_resp) = whois_query(&referral, domain, timeout).await else {
        return DomainResult::transient();
    };

    parse_whois(&whois_resp)
}
