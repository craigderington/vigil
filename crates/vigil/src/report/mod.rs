//! P4.4 monthly incident reports (CLAUDE.md §13). UTC calendar months,
//! computed from durable tables. See `compute` for the metrics, `html` for
//! rendering, `scheduler` for the monthly auto-generate loop.

pub mod compute;
pub mod html;
pub mod scheduler;

use serde::Serialize;

/// A stored report row (`reports` table).
#[derive(Clone, Debug, Serialize, sqlx::FromRow)]
pub struct Report {
    pub id: i64,
    pub period_start: i64,
    pub period_end: i64,
    pub label: String,
    pub generated_at: i64,
    pub summary_json: String,
    pub html_path: Option<String>,
    pub pdf_path: Option<String>,
    pub emailed_at: Option<i64>,
}

/// `"YYYY-MM"` → the 1st of that month as a `NaiveDate` (fallback 1970-01-01).
fn parse_month_first(period: &str) -> chrono::NaiveDate {
    let mut it = period.split('-');
    let y = it.next().and_then(|s| s.parse::<i32>().ok()).unwrap_or(1970);
    let m = it.next().and_then(|s| s.parse::<u32>().ok()).filter(|m| (1..=12).contains(m)).unwrap_or(1);
    chrono::NaiveDate::from_ymd_opt(y, m, 1).unwrap_or_else(|| chrono::NaiveDate::from_ymd_opt(1970, 1, 1).unwrap())
}

/// UTC `"YYYY-MM"` for the month containing `epoch`.
pub fn month_of(epoch: i64) -> String {
    chrono::DateTime::<chrono::Utc>::from_timestamp(epoch, 0).unwrap_or_default().format("%Y-%m").to_string()
}

/// `(start, end)` UTC epoch bounds for `"YYYY-MM"`: first-of-month 00:00 → first-of-next-month 00:00 (exclusive).
pub fn month_bounds(period: &str) -> (i64, i64) {
    let first = parse_month_first(period);
    let start = first.and_hms_opt(0, 0, 0).map(|dt| dt.and_utc().timestamp()).unwrap_or(0);
    let next = first.checked_add_months(chrono::Months::new(1)).unwrap_or(first);
    let end = next.and_hms_opt(0, 0, 0).map(|dt| dt.and_utc().timestamp()).unwrap_or(start + 2_678_400);
    (start, end)
}

/// `"March 2026"` for `"2026-03"`.
pub fn month_label(period: &str) -> String {
    parse_month_first(period).format("%B %Y").to_string()
}

/// The month before `period` (`"2026-01"` → `"2025-12"`).
pub fn prior_month(period: &str) -> String {
    let f = parse_month_first(period);
    f.checked_sub_months(chrono::Months::new(1)).unwrap_or(f).format("%Y-%m").to_string()
}

/// The month after `period`.
pub fn next_month(period: &str) -> String {
    let f = parse_month_first(period);
    f.checked_add_months(chrono::Months::new(1)).unwrap_or(f).format("%Y-%m").to_string()
}

// (`generate` + `send_report_email` are added in Task 4.)
pub use compute::{compute, fleet_uptime_for, ExpiryItem, FleetReport, LongestOutage, MonitorReport, ReportIncident, ReportSummary};

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn month_bounds_and_labels_utc() {
        // 2026-03: 2026-03-01T00:00Z .. 2026-04-01T00:00Z
        let (s, e) = month_bounds("2026-03");
        assert_eq!(s, 1_772_323_200); // 2026-03-01T00:00:00Z
        assert_eq!(e, 1_775_001_600); // 2026-04-01T00:00:00Z (March has 31 days)
        assert_eq!(month_label("2026-03"), "March 2026");
        assert_eq!(month_of(s), "2026-03");
        assert_eq!(month_of(e - 1), "2026-03");
    }
    #[test]
    fn prior_and_next_month_year_rollover() {
        assert_eq!(prior_month("2026-01"), "2025-12");
        assert_eq!(next_month("2025-12"), "2026-01");
        assert_eq!(prior_month("2026-03"), "2026-02");
        assert_eq!(next_month("2026-03"), "2026-04");
    }
    #[test]
    fn month_bounds_bad_input_is_safe() {
        let (s, e) = month_bounds("nonsense");
        assert!(e > s);
    }
}
