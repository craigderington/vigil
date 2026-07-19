use crate::maintenance_windows::resolve::subtract_intervals;
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

/// Compute uptime from a set of incident spans, excluding maintenance time
/// from both the denominator and any downtime (maintenance windows must not
/// count against uptime %).
///
/// Rules:
/// - If `!had_any_check` → `Uptime { uptime_pct: None, downtime_seconds: 0 }`.
/// - Else: `maintenance` (arbitrary order, possibly overlapping/unmerged —
///   callers are not required to pre-clip or pre-merge) is applied via
///   [`subtract_intervals`], which clips each interval to the base range and
///   sorts/coalesces them before subtracting, so a messy caller-supplied set
///   can never corrupt the result. `eff_denom` is the length of
///   `[window_start, now]` remaining after subtracting `maintenance`; if
///   `eff_denom <= 0` (maintenance covers the whole window) →
///   `Uptime { uptime_pct: None, downtime_seconds: 0 }` (no meaningful
///   window left to measure).
/// - Else: for each span, clip `[start, end_or_now]` to `[window_start,
///   now]`, subtract `maintenance` from the clipped span, and sum what's
///   left into `downtime_seconds`.
/// - `uptime_pct = Some(((1.0 - downtime_seconds as f64 / eff_denom as f64) * 100.0)`
///   rounded to 2 decimal places `)`.
pub fn compute(spans: &[Span], window_start: Ts, now: Ts, had_any_check: bool, maintenance: &[(Ts, Ts)]) -> Uptime {
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

    // Effective denominator: the window minus any maintenance overlap.
    // `subtract_intervals` clips `maintenance` to `[window_start, now]` and
    // merges it before subtracting, so this is correct even if `maintenance`
    // is unmerged/overlapping/out-of-window.
    let eff_denom: i64 = subtract_intervals((window_start, now), maintenance)
        .iter()
        .map(|&(s, e)| e - s)
        .sum();

    if eff_denom <= 0 {
        return Uptime {
            uptime_pct: None,
            downtime_seconds: 0,
        };
    }

    let mut total_downtime: i64 = 0;

    for span in spans {
        // Clip span to window: [window_start, now]
        let span_start = span.start.max(window_start);
        let span_end = span.end.unwrap_or(now).min(now);

        // Only count positive overlaps, minus any maintenance overlap.
        if span_start < span_end {
            total_downtime += subtract_intervals((span_start, span_end), maintenance)
                .iter()
                .map(|&(s, e)| e - s)
                .sum::<i64>();
        }
    }

    let uptime_pct = (1.0 - total_downtime as f64 / eff_denom as f64) * 100.0;

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
        let u = compute(&[], 0, 1000, false, &[]);
        assert_eq!(u.uptime_pct, None);
        assert_eq!(u.downtime_seconds, 0);
    }

    #[test]
    fn no_incidents_100() {
        let u = compute(&[], 0, 1000, true, &[]);
        assert_eq!(u.uptime_pct, Some(100.0));
    }

    #[test]
    fn open_incident_to_now() {
        let u = compute(&[Span { start: 800, end: None }], 0, 1000, true, &[]);
        assert_eq!(u.downtime_seconds, 200);
        assert_eq!(u.uptime_pct, Some(80.0));
    }

    #[test]
    fn clips_to_window() {
        let u = compute(&[Span { start: -100, end: Some(100) }], 0, 1000, true, &[]);
        assert_eq!(u.downtime_seconds, 100);
    }

    // ---- maintenance exclusion ----

    #[test]
    fn downtime_fully_inside_maintenance_is_excluded() {
        // A 200s outage entirely covered by a 300s maintenance window ->
        // zero counted downtime and 100% uptime (with a shrunk denominator).
        let u = compute(
            &[Span { start: 400, end: Some(600) }],
            0,
            1000,
            true,
            &[(300, 600)],
        );
        assert_eq!(u.downtime_seconds, 0);
        assert_eq!(u.uptime_pct, Some(100.0));
    }

    #[test]
    fn downtime_partial_overlap_counts_only_outside_part() {
        // Outage [400,600); maintenance only covers [300,500) -> the
        // [500,600) tail (100s) still counts as real downtime, out of an
        // eff_denom shrunk by the 200s maintenance slice (window=1000,
        // maintenance=200 -> eff_denom=800).
        let u = compute(
            &[Span { start: 400, end: Some(600) }],
            0,
            1000,
            true,
            &[(300, 500)],
        );
        assert_eq!(u.downtime_seconds, 100);
        // 100 - 100*100/800
        assert_eq!(u.uptime_pct, Some(87.5));
    }

    #[test]
    fn whole_window_under_maintenance_is_none() {
        // Maintenance covers [window_start, now] entirely -> no meaningful
        // denominator left, so uptime_pct is None (not a divide-by-zero
        // panic, not a misleading 100%).
        let u = compute(&[Span { start: 400, end: Some(600) }], 0, 1000, true, &[(0, 1000)]);
        assert_eq!(u.uptime_pct, None);
        assert_eq!(u.downtime_seconds, 0);
    }

    #[test]
    fn maintenance_unmerged_and_out_of_window_still_correct() {
        // Two overlapping maintenance intervals (unmerged) plus one
        // entirely outside [window_start, now] -> must not corrupt the
        // denominator (S4 defensive clip+merge inside compute).
        let u = compute(
            &[Span { start: 400, end: Some(600) }],
            0,
            1000,
            true,
            &[(300, 500), (450, 600), (2000, 3000)],
        );
        assert_eq!(u.downtime_seconds, 0);
        assert_eq!(u.uptime_pct, Some(100.0));
    }
}
