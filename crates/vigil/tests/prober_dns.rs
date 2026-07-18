use vigil::models::*;

fn m_dns(record_type: &str, expected: Option<&str>) -> Monitor {
    let mut m = vigil::models::test_defaults_monitor();
    m.r#type = "dns".into();
    m.url = None;
    m.host = Some("x".into());
    m.dns_record_type = Some(record_type.into());
    m.dns_expected_value = expected.map(|s| s.to_string());
    m
}

/// Injected fake resolver matches: ok + resolved_ip set when the expected
/// substring is found (case-insensitively) among the returned records;
/// !ok when it isn't.
#[tokio::test]
async fn dns_expected_value_match() {
    let mut m = m_dns("A", Some("93.184"));
    let o = vigil::probe::dns::probe_with(&m, |_h, _rt| async {
        Ok(vec!["93.184.216.34".to_string()])
    })
    .await;
    assert!(o.ok);
    assert_eq!(o.resolved_ip.as_deref(), Some("93.184.216.34"));

    m.dns_expected_value = Some("10.0.0.1".into());
    let o2 = vigil::probe::dns::probe_with(&m, |_h, _rt| async {
        Ok(vec!["93.184.216.34".to_string()])
    })
    .await;
    assert!(!o2.ok); // expected value not found
    assert!(matches!(o2.cause, Some(Cause::Dns)));
}

/// A resolver call that succeeds but returns zero records is a failure —
/// there's nothing to check against, and an empty answer usually means
/// NXDOMAIN/NODATA rather than a clean success.
#[tokio::test]
async fn empty_records_is_not_ok() {
    let m = m_dns("A", None);
    let o = vigil::probe::dns::probe_with(&m, |_h, _rt| async { Ok(vec![]) }).await;
    assert!(!o.ok);
    assert!(matches!(o.cause, Some(Cause::Dns)));
}

/// With no `dns_expected_value` configured, any resolved record is a pass.
#[tokio::test]
async fn no_expected_value_any_record_is_ok() {
    let m = m_dns("A", None);
    let o = vigil::probe::dns::probe_with(&m, |_h, _rt| async {
        Ok(vec!["203.0.113.9".to_string()])
    })
    .await;
    assert!(o.ok);
    assert_eq!(o.resolved_ip.as_deref(), Some("203.0.113.9"));
}

/// Non-A/AAAA record types (e.g. CNAME) still run the substring match, but
/// `resolved_ip` stays `None` — it's only meaningful for address records.
#[tokio::test]
async fn cname_expected_substring_match() {
    let m = m_dns("CNAME", Some("example"));
    let o = vigil::probe::dns::probe_with(&m, |_h, _rt| async {
        Ok(vec!["example.com".to_string()])
    })
    .await;
    assert!(o.ok);
    assert_eq!(o.resolved_ip, None);
}

/// A resolver error (e.g. NXDOMAIN, network failure) is a failed outcome
/// with `Cause::Dns` and the error message preserved.
#[tokio::test]
async fn resolver_error_is_down() {
    let m = m_dns("A", None);
    let o = vigil::probe::dns::probe_with(&m, |_h, _rt| async {
        Err::<Vec<String>, String>("no such host".to_string())
    })
    .await;
    assert!(!o.ok);
    assert!(matches!(o.cause, Some(Cause::Dns)));
    assert_eq!(o.error_message.as_deref(), Some("no such host"));
}

/// Missing `host` or `dns_record_type` is a config error, not a network
/// probe — the resolver closure must never be invoked.
#[tokio::test]
async fn missing_host_is_config_error() {
    let mut m = m_dns("A", None);
    m.host = None;
    let o = vigil::probe::dns::probe_with(&m, |_h, _rt| async {
        panic!("resolver should not be called");
        #[allow(unreachable_code)]
        Ok(vec![])
    })
    .await;
    assert!(!o.ok);
    assert!(matches!(o.cause, Some(Cause::Dns)));
}

#[tokio::test]
async fn missing_record_type_is_config_error() {
    let mut m = m_dns("A", None);
    m.dns_record_type = None;
    let o = vigil::probe::dns::probe_with(&m, |_h, _rt| async {
        panic!("resolver should not be called");
        #[allow(unreachable_code)]
        Ok(vec![])
    })
    .await;
    assert!(!o.ok);
    assert!(matches!(o.cause, Some(Cause::Dns)));
}

/// A resolver that hangs longer than `timeout_seconds` must be bounded by
/// the outer timeout rather than left to run indefinitely — mirrors
/// `probe::tcp`/`probe::http`'s timeout behavior.
#[tokio::test]
async fn dns_resolve_timeout_is_bounded() {
    let mut m = vigil::models::test_defaults_monitor();
    m.r#type = "dns".into();
    m.host = Some("slow".into());
    m.url = None;
    m.dns_record_type = Some("A".into());
    m.timeout_seconds = 1;
    let o = vigil::probe::dns::probe_with(&m, |_h, _rt| async {
        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
        Ok::<Vec<String>, String>(vec!["1.2.3.4".into()])
    })
    .await;
    assert!(!o.ok, "must not succeed after timeout");
    assert!(matches!(o.cause, Some(Cause::Timeout)));
}

/// End-to-end wiring: `probe()` (the real-resolver entry point used by
/// `probe::run`) must compile and run against a live resolver. Network
/// access may not be available in CI/sandbox, so this only asserts the
/// outcome is well-formed (ok iff cause is absent), not that it succeeds.
#[tokio::test]
async fn probe_real_resolver_is_well_formed() {
    let m = m_dns("A", None);
    let o = vigil::probe::dns::probe(&m).await;
    assert_eq!(o.ok, o.cause.is_none());
}
