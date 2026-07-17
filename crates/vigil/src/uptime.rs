use crate::models::Ts;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Span {
    pub start: Ts,
    pub end: Option<Ts>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Uptime {
    pub uptime_pct: Option<f64>,
    pub downtime_seconds: i64,
}

/// Compute uptime from a set of incident spans.
///
/// Rules:
/// - If `!had_any_check` → `Uptime { uptime_pct: None, downtime_seconds: 0 }`.
/// - Else: for each span, clip `[start, end_or_now]` to `[window_start, now]`;
///   sum positive overlaps into `downtime_seconds`.
/// - `uptime_pct = Some(((1.0 - downtime_seconds as f64 / (now - window_start) as f64) * 100.0)`
///   rounded to 2 decimal places `)`.
pub fn compute(spans: &[Span], window_start: Ts, now: Ts, had_any_check: bool) -> Uptime {
    if !had_any_check {
        return Uptime {
            uptime_pct: None,
            downtime_seconds: 0,
        };
    }

    // Avoid divide-by-zero
    if now == window_start {
        return Uptime {
            uptime_pct: Some(100.0),
            downtime_seconds: 0,
        };
    }

    let mut total_downtime: i64 = 0;

    for span in spans {
        // Clip span to window: [window_start, now]
        let span_start = span.start.max(window_start);
        let span_end = span.end.unwrap_or(now).min(now);

        // Only count positive overlaps
        if span_start < span_end {
            total_downtime += span_end - span_start;
        }
    }

    let window_duration = (now - window_start) as f64;
    let uptime_pct = (1.0 - total_downtime as f64 / window_duration) * 100.0;

    // Round to 2 decimal places
    let uptime_pct_rounded = (uptime_pct * 100.0).round() / 100.0;

    Uptime {
        uptime_pct: Some(uptime_pct_rounded),
        downtime_seconds: total_downtime,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_checks_none() {
        let u = compute(&[], 0, 1000, false);
        assert_eq!(u.uptime_pct, None);
        assert_eq!(u.downtime_seconds, 0);
    }

    #[test]
    fn no_incidents_100() {
        let u = compute(&[], 0, 1000, true);
        assert_eq!(u.uptime_pct, Some(100.0));
    }

    #[test]
    fn open_incident_to_now() {
        let u = compute(&[Span { start: 800, end: None }], 0, 1000, true);
        assert_eq!(u.downtime_seconds, 200);
        assert_eq!(u.uptime_pct, Some(80.0));
    }

    #[test]
    fn clips_to_window() {
        let u = compute(&[Span { start: -100, end: Some(100) }], 0, 1000, true);
        assert_eq!(u.downtime_seconds, 100);
    }
}
