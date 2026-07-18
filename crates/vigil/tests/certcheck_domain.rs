//! Tests for `vigil::certcheck::domain`:
//! - `registrable_domain` — pure eTLD+1 vectors (hardcoded MULTI_SUFFIX
//!   subset, not a full Public Suffix List).
//! - `parse_rdap` — pure parse of an inline RDAP JSON fixture.
//! - `check` — a network-gated smoke test against a real public host,
//!   tolerant of network absence.

use serde_json::json;

use vigil::certcheck::domain;

// ---- registrable_domain (pure, eTLD+1) ----

#[test]
fn registrable_domain_strips_subdomain() {
    assert_eq!(domain::registrable_domain("api.myapp.com"), "myapp.com");
}

#[test]
fn registrable_domain_strips_www() {
    assert_eq!(domain::registrable_domain("www.example.com"), "example.com");
}

#[test]
fn registrable_domain_keeps_multi_suffix() {
    assert_eq!(domain::registrable_domain("x.example.co.uk"), "example.co.uk");
}

#[test]
fn registrable_domain_apex_unchanged() {
    assert_eq!(domain::registrable_domain("example.com"), "example.com");
}

// ---- parse_rdap (pure, inline fixture) ----

/// 2027-03-01T00:00:00Z as a Unix epoch second. Computed independently of
/// the implementation under test (verified via `date -u -d
/// "2027-03-01T00:00:00Z" +%s` and cross-checked in Python) — NOT the
/// 1803168000 figure floated in the task brief, which is off by 8 days
/// (that value is actually 2027-02-21T00:00:00Z). See task-4-report.md for
/// the discrepancy note.
const EXPECTED_EXPIRY_EPOCH: i64 = 1_803_859_200;

#[test]
fn parse_rdap_extracts_expiry_registrar_queryable() {
    let fixture = json!({
        "objectClassName": "domain",
        "ldhName": "EXAMPLE.COM",
        "status": ["client transfer prohibited"],
        "entities": [
            {
                "objectClassName": "entity",
                "roles": ["registrar"],
                "vcardArray": [
                    "vcard",
                    [
                        ["version", {}, "text", "4.0"],
                        ["fn", {}, "text", "Example Registrar, Inc."]
                    ]
                ]
            }
        ],
        "nameservers": [
            {"objectClassName": "nameserver", "ldhName": "ns1.example.com"},
            {"objectClassName": "nameserver", "ldhName": "ns2.example.com"}
        ],
        "events": [
            {"eventAction": "registration", "eventDate": "2010-03-01T00:00:00Z"},
            {"eventAction": "expiration", "eventDate": "2027-03-01T00:00:00Z"}
        ]
    });

    let result = domain::parse_rdap(&fixture).expect("fixture should parse");

    assert_eq!(
        result.expiry_date,
        Some(EXPECTED_EXPIRY_EPOCH),
        "expiry epoch must match 2027-03-01T00:00:00Z exactly, not just be present"
    );
    assert_eq!(result.registrar.as_deref(), Some("Example Registrar, Inc."));
    assert!(result.queryable, "an fixture with a parsed expiry must be queryable");
    assert!(!result.transient, "a successful parse is never transient");
    assert_eq!(result.source.as_deref(), Some("rdap"));
    assert_eq!(result.name_servers, vec!["ns1.example.com", "ns2.example.com"]);
    assert_eq!(result.status_codes, vec!["client transfer prohibited"]);
}

#[test]
fn parse_rdap_no_events_key_is_none() {
    // No "events" array at all — parse_rdap can't find an expiry, so it
    // must not fabricate one. `check()` (not exercised here) turns a `None`
    // return from a 200 RDAP response into a definitive non-transient
    // "not queryable" result; this test only proves parse_rdap itself
    // doesn't return a bogus Some.
    let fixture = json!({"objectClassName": "domain", "ldhName": "example.com"});
    assert!(domain::parse_rdap(&fixture).is_none());
}

// ---- check() live smoke test (network-gated, tolerant) ----

/// Best-effort smoke test against a real public host. Must never fail the
/// suite when the network/RDAP/WHOIS infrastructure is unavailable
/// (sandboxed/offline CI) — it only asserts internal coherence:
/// - `check` never panics.
/// - `transient` and a definitive answer are mutually exclusive by
///   construction (the invariant this task hardens): if the result is NOT
///   transient AND is queryable, it MUST carry an actual expiry date.
#[tokio::test]
async fn live_smoke_one_dot_one() {
    let result = domain::check("one.one.one.one", 10).await;

    if result.transient {
        eprintln!("live_smoke_one_dot_one: transient result (network/registry unavailable), skipping further assertions");
        return;
    }

    if result.queryable {
        assert!(
            result.expiry_date.is_some(),
            "a non-transient, queryable result must carry an expiry_date"
        );
    }
}
