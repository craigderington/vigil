//! Self-contained HTML rendering of a ReportSummary: inline navy-theme CSS +
//! a print stylesheet (Ctrl-P → PDF). One renderer for both export and the
//! in-app iframe view. No templating crate.

use crate::digest::fmt_ts;
use crate::report::compute::{ExpiryItem, MonitorReport, ReportIncident, ReportSummary};

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}
fn pct(v: Option<f64>) -> String { v.map(|p| format!("{p:.2}%")).unwrap_or_else(|| "—".into()) }
fn ms(v: Option<i64>) -> String { v.map(|n| format!("{n}ms")).unwrap_or_else(|| "—".into()) }
fn secs(v: i64) -> String { format!("{}m {}s", v / 60, v % 60) }
fn delta(v: Option<f64>) -> String {
    match v { Some(d) if d >= 0.0 => format!("▲ {d:.2}"), Some(d) => format!("▼ {:.2}", d.abs()), None => "—".into() }
}

const STYLE: &str = "\
body{background:#0A1220;color:#EAEDF3;font-family:Inter,system-ui,sans-serif;margin:0;padding:24px}\
h1,h2{color:#EAEDF3} .band{display:flex;flex-wrap:wrap;gap:24px;margin:16px 0}\
.tile{background:#16233A;border:1px solid #2A3A56;border-radius:10px;padding:12px 16px}\
.tile .n{font-family:'JetBrains Mono',monospace;font-size:24px;font-weight:600}\
table{width:100%;border-collapse:collapse;margin:12px 0}th,td{text-align:left;padding:6px 10px;border-bottom:1px solid #1E2C44;font-size:14px}\
.mono{font-family:'JetBrains Mono',monospace}.flag-expiring{color:#F5A623}.flag-invalid,.flag-unknown{color:#F26D6D}\
@media print{body{background:#fff;color:#111}.tile{background:#f4f6fa;border-color:#ccc}th,td{border-color:#ddd}section{break-inside:avoid}}\
";

pub fn render_html(s: &ReportSummary) -> String {
    let f = &s.fleet;
    let mut h = String::new();
    h.push_str(&format!("<!doctype html><html><head><meta charset=\"utf-8\"><title>Vigil report — {}</title><style>{STYLE}</style></head><body>", esc(&s.label)));
    h.push_str(&format!("<h1>Vigil monthly report — {}</h1>", esc(&s.label)));
    h.push_str(&format!("<p class=\"mono\">{} · generated {} · Vigil {}</p>", esc(&s.period), fmt_ts(s.generated_at), env!("CARGO_PKG_VERSION")));
    // hero band
    h.push_str("<div class=\"band\">");
    for (label, val) in [
        ("Uptime", format!("{} <small>({})</small>", pct(f.uptime_pct), delta(f.uptime_delta))),
        ("Incidents", f.incidents.to_string()),
        ("Downtime", secs(f.downtime_seconds)),
        ("MTTR", f.mttr_seconds.map(secs).unwrap_or_else(|| "—".into())),
        ("Longest outage", f.longest_outage.as_ref().map(|l| format!("{} ({})", esc(&l.monitor), secs(l.seconds))).unwrap_or_else(|| "—".into())),
        ("Clean", format!("{} / {}", f.clean_monitors, f.monitors_total)),
        ("SSL/domain alerts", format!("{} / {}", f.ssl_alerts, f.domain_alerts)),
        ("Expiring ≤30d / ≤60d", format!("{} / {}", f.expiring_30d, f.expiring_60d)),
    ] {
        h.push_str(&format!("<div class=\"tile\"><div>{label}</div><div class=\"n\">{val}</div></div>"));
    }
    h.push_str("</div>");
    // per-monitor table
    h.push_str("<section><h2>Per-monitor</h2><table><tr><th>Monitor</th><th>Type</th><th>Uptime</th><th>Incidents</th><th>Downtime</th><th>MTTR</th><th>Avg</th><th>p95</th><th>End</th></tr>");
    for m in &s.monitors { h.push_str(&monitor_row(m)); }
    h.push_str("</table></section>");
    // incident log
    h.push_str("<section><h2>Incidents</h2><table><tr><th>Monitor</th><th>Started</th><th>Duration</th><th>Cause</th><th>Resolved</th></tr>");
    if s.incidents.is_empty() { h.push_str("<tr><td colspan=5>No incidents.</td></tr>"); }
    for i in &s.incidents { h.push_str(&incident_row(i)); }
    h.push_str("</table></section>");
    // cert outlook
    h.push_str("<section><h2>Certificate &amp; domain outlook</h2><table><tr><th>Monitor</th><th>Kind</th><th>Days remaining</th><th>Status</th></tr>");
    if s.cert_outlook.is_empty() { h.push_str("<tr><td colspan=4>Nothing tracked.</td></tr>"); }
    for e in &s.cert_outlook { h.push_str(&outlook_row(e)); }
    h.push_str("</table></section></body></html>");
    h
}

fn monitor_row(m: &MonitorReport) -> String {
    format!("<tr><td>{}</td><td>{}</td><td class=\"mono\">{}</td><td class=\"mono\">{}</td><td class=\"mono\">{}</td><td class=\"mono\">{}</td><td class=\"mono\">{}</td><td class=\"mono\">{}</td><td>{}</td></tr>",
        esc(&m.name), esc(&m.r#type), pct(m.uptime_pct), m.incidents, secs(m.downtime_seconds),
        m.mttr_seconds.map(secs).unwrap_or_else(|| "—".into()), ms(m.avg_ms), ms(m.p95_ms), esc(&m.end_status))
}
fn incident_row(i: &ReportIncident) -> String {
    format!("<tr><td>{}</td><td class=\"mono\">{}</td><td class=\"mono\">{}</td><td>{}</td><td class=\"mono\">{}</td></tr>",
        esc(&i.monitor_name), fmt_ts(i.started_at), i.duration_seconds.map(secs).unwrap_or_else(|| "—".into()),
        esc(i.cause.as_deref().unwrap_or("-")), i.resolved_at.map(fmt_ts).unwrap_or_else(|| "ongoing".into()))
}
fn outlook_row(e: &ExpiryItem) -> String {
    format!("<tr><td>{}</td><td>{}</td><td class=\"mono\">{}</td><td class=\"flag-{}\">{}</td></tr>",
        esc(&e.monitor), esc(&e.kind), e.days_remaining.map(|d| d.to_string()).unwrap_or_else(|| "—".into()), esc(&e.flag), esc(&e.flag))
}
