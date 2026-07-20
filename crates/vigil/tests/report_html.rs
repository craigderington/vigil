use vigil::report::compute::*;
use vigil::report::html::render_html;

fn sample() -> ReportSummary {
    ReportSummary {
        period: "2026-03".into(), label: "March 2026".into(), generated_at: 1_772_323_200,
        fleet: FleetReport {
            uptime_pct: Some(99.94), uptime_delta: Some(0.07), incidents: 2, downtime_seconds: 5220,
            mttr_seconds: Some(474), longest_outage: Some(LongestOutage { monitor: "api<x>".into(), seconds: 1980 }),
            monitors_total: 3, clean_monitors: 2, ssl_alerts: 1, domain_alerts: 0, expiring_30d: 1, expiring_60d: 1,
        },
        cert_outlook: vec![ExpiryItem { monitor: "api<x>".into(), kind: "ssl".into(), days_remaining: Some(12), flag: "expiring".into() }],
        monitors: vec![MonitorReport { id: 1, name: "api<x>".into(), r#type: "http".into(), uptime_pct: Some(99.7), incidents: 2, downtime_seconds: 5220, mttr_seconds: Some(474), avg_ms: Some(142), p95_ms: None, end_status: "up".into() }],
        incidents: vec![ReportIncident { monitor_name: "api<x>".into(), started_at: 1_772_323_200, resolved_at: Some(1_772_325_180), duration_seconds: Some(1980), cause: Some("timeout".into()), status_code: None, error_message: None }],
    }
}

#[test]
fn render_html_is_self_contained_and_escaped() {
    let h = render_html(&sample());
    assert!(h.contains("March 2026"));
    assert!(h.contains("99.94"));
    assert!(h.contains("@media print"), "carries a print stylesheet");
    assert!(h.contains("<style"), "inline CSS, self-contained");
    assert!(!h.contains("http://") && !h.contains("https://fonts"), "no external assets");
    // HTML-escaping: the monitor name 'api<x>' must not appear as a raw tag
    assert!(h.contains("api&lt;x&gt;"));
    assert!(!h.contains("api<x>"));
    // p95 None renders as a dash
    assert!(h.contains("—"));
}
