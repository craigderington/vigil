//! Pure maintenance-window resolution: scope matching, one-off/cron
//! active-at checks, occurrence enumeration over a time range, and interval
//! math for excluding maintenance from uptime denominators. No I/O — see
//! `super::active_windows` for the one DB read callers need before handing
//! rows to these functions.
//!
//! ## croner 2.x is forward-only
//!
//! `croner::Cron::find_next_occurrence(&self, start: &DateTime<Tz>, inclusive:
//! bool) -> Result<DateTime<Tz>, CronError>` has no backward/previous-
//! occurrence counterpart. Both [`window_active_at`]'s cron path and
//! [`occurrences_overlapping`]'s cron path therefore do a bounded FORWARD
//! scan from an anchor that is deliberately backed up by the window's
//! duration (`dur = ends_at - starts_at`), so an occurrence that started
//! before the point of interest but still covers it is never missed:
//!
//! - the *first* call is `inclusive = true`, so an occurrence starting
//!   exactly at the anchor is not skipped (otherwise a window active at the
//!   exact instant its occurrence starts would wrongly resolve "not
//!   active");
//! - each subsequent call advances `t = s + 1` (one second past the
//!   previously found start) and again passes `inclusive = true` — simpler
//!   than flipping to `inclusive = false` from `s`, and behaviorally
//!   identical, since no cron occurrence can recur 1 second after another
//!   at the granularities this app supports (minimum monitor interval is
//!   15s; cron fields here have no seconds component).
//!
//! Both scans are bounded by [`scan_cap`] and log `tracing::warn!` (never
//! silently truncating) if the cap is hit — a pathological cron expression
//! must not hang a probe or alert evaluation, both of which call into this
//! module on every tick.

use crate::models::{MaintenanceWindow, Ts};

/// The strongest suppression an in-scope, currently-active maintenance
/// window applies to a monitor. Ordering (weakest to strongest):
/// `None < Alerts < Checks`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Suppression {
    None,
    Alerts,
    Checks,
}

/// Parses a monitor's `tags` column (a JSON array of strings) into a plain
/// `Vec<String>`. Never panics: an empty string, malformed JSON, or a
/// validly-parsed-but-wrong-shaped value (e.g. `"{}"`, `"[1,2,3]"`) all
/// yield an empty vec, treating the monitor as tagless rather than failing.
pub fn parse_tags(raw: &str) -> Vec<String> {
    serde_json::from_str::<Vec<String>>(raw).unwrap_or_default()
}

/// Bounded iteration cap for the forward cron scans below: at least
/// 10,000 steps, or roughly one step per minute of the scanned range,
/// whichever is larger. `range_seconds` is clamped to non-negative before
/// dividing so a degenerate/inverted range can't underflow.
fn scan_cap(range_seconds: i64) -> u64 {
    let per_minute = range_seconds.max(0) / 60 + 2;
    (per_minute as u64).max(10_000)
}

/// Converts a `Ts` (unix epoch seconds) to a UTC `DateTime` for croner, or
/// `None` if the epoch is out of `chrono`'s representable range (never
/// panics/unwraps).
fn to_utc(t: Ts) -> Option<chrono::DateTime<chrono::Utc>> {
    chrono::DateTime::<chrono::Utc>::from_timestamp(t, 0)
}

/// Is `window` active at instant `now`?
///
/// - **One-off** (`recurrence.is_none()`): `starts_at <= now <= ends_at`.
/// - **Cron** (`recurrence = Some(expr)`): the window recurs with duration
///   `dur = ends_at - starts_at` starting at each cron occurrence, floored
///   by `starts_at` (a cron window has no occurrences before its
///   configured start — matches the migration's "the >= lower bound ...
///   for cron" comment on the `starts_at` column). Since croner is
///   forward-only, we scan forward from
///   `anchor = max(starts_at, now - dur)` — far enough back that an
///   occurrence starting before `now` but still covering it isn't missed —
///   collecting the LAST occurrence start `s <= now`. Active iff a
///   qualifying `s` was found and `now < s + dur` (the `s >= starts_at`
///   check is implied by the anchor but kept explicit for clarity/safety).
///   An unparseable `recurrence`, a non-positive `dur`, or an epoch
///   `chrono` can't represent all resolve to `false`.
pub fn window_active_at(window: &MaintenanceWindow, now: Ts) -> bool {
    match &window.recurrence {
        None => window.starts_at <= now && now <= window.ends_at,
        Some(expr) => {
            let dur = window.ends_at - window.starts_at;
            if dur <= 0 {
                return false;
            }
            let Ok(cron) = croner::Cron::new(expr).parse() else {
                return false;
            };
            let anchor = window.starts_at.max(now - dur);
            match last_occurrence_start(&cron, anchor, now, window.id, "window_active_at") {
                Some(s) => s >= window.starts_at && now < s + dur,
                None => false,
            }
        }
    }
}

/// Forward-scans `cron` from `anchor`, returning the LAST occurrence start
/// `<= limit` (or `None` if none is found within the scan cap). Shared
/// engine for `window_active_at`'s "is now covered by the most recent
/// occurrence" question.
fn last_occurrence_start(cron: &croner::Cron, anchor: Ts, limit: Ts, window_id: i64, caller: &str) -> Option<Ts> {
    let cap = scan_cap(limit - anchor);
    let mut t = anchor;
    let mut last: Option<Ts> = None;
    let mut iterations: u64 = 0;
    loop {
        if iterations >= cap {
            tracing::warn!(window_id, cap, caller, "maintenance_windows: cron forward scan hit iteration cap");
            break;
        }
        iterations += 1;
        let Some(dt) = to_utc(t) else { break };
        // inclusive=true on every call: on the first iteration this is
        // required (see module docs); on later iterations `t` is already
        // one second past the previous start, so inclusive vs exclusive
        // is a distinction without a difference here.
        let Ok(next) = cron.find_next_occurrence(&dt, true) else {
            break;
        };
        let s = next.timestamp();
        if s > limit {
            break;
        }
        last = Some(s);
        t = s + 1;
    }
    last
}

/// All occurrences of `window` that overlap `[from, to]`, as `(start, end)`
/// pairs — UNCLIPPED to the range (callers that need clipping, e.g.
/// [`maintenance_intervals`], clip themselves). Empty if `to < from`.
///
/// - **One-off**: the single `(starts_at, ends_at)` interval, if it
///   overlaps `[from, to]` at all.
/// - **Cron**: the window recurs with duration `dur = ends_at - starts_at`.
///   Scans forward from `anchor = max(from - dur, starts_at)` — NOT from
///   `from` — because an occurrence starting before `from` can still
///   extend into `[from, to]` (e.g. an hourly cron with a 1h duration: the
///   occurrence starting at 02:00 covers `[02:00, 03:00]`, which overlaps
///   a queried range starting at 02:30 — scanning from `from` alone would
///   miss it and under-report maintenance, wrongly counting real downtime
///   as an outage). Every occurrence found this way overlaps `[from, to]`
///   by construction (`s >= anchor >= from - dur` ⇒ `s + dur >= from`, and
///   the loop stops once `s > to`), so no separate overlap filter is
///   needed. Floored by `starts_at` for the same reason as
///   `window_active_at`: a cron window has no occurrences before its
///   configured start.
pub fn occurrences_overlapping(window: &MaintenanceWindow, from: Ts, to: Ts) -> Vec<(Ts, Ts)> {
    if to < from {
        return vec![];
    }
    match &window.recurrence {
        None => {
            if window.starts_at <= to && window.ends_at >= from {
                vec![(window.starts_at, window.ends_at)]
            } else {
                vec![]
            }
        }
        Some(expr) => {
            let dur = window.ends_at - window.starts_at;
            if dur <= 0 {
                return vec![];
            }
            let Ok(cron) = croner::Cron::new(expr).parse() else {
                return vec![];
            };
            let anchor = (from - dur).max(window.starts_at);
            let cap = scan_cap(to - from);
            let mut out = Vec::new();
            let mut t = anchor;
            let mut iterations: u64 = 0;
            loop {
                if iterations >= cap {
                    tracing::warn!(window_id = window.id, cap, "maintenance_windows: occurrences_overlapping cron scan hit iteration cap");
                    break;
                }
                iterations += 1;
                let Some(dt) = to_utc(t) else { break };
                let Ok(next) = cron.find_next_occurrence(&dt, true) else {
                    break;
                };
                let s = next.timestamp();
                if s > to {
                    break;
                }
                out.push((s, s + dur));
                t = s + 1;
            }
            out
        }
    }
}

/// Does `window`'s `scope` cover `monitor_id` (given its parsed `tags`)?
///
/// - `all` → always `true`.
/// - `tag` → `target_ref` must parse as a JSON string; `true` iff `tags`
///   contains it.
/// - `monitors` → `target_ref` must parse as a JSON array of `i64`; `true`
///   iff it contains `monitor_id`.
/// - anything else (missing `target_ref`, malformed/wrong-shaped JSON, or
///   an unrecognized `scope` value) → `false`. Never panics — this runs
///   against DB rows that `validate_window_dto` should have already
///   constrained, but must degrade safely if that invariant is ever
///   violated (e.g. a manually-edited DB).
pub fn monitor_in_scope(window: &MaintenanceWindow, monitor_id: i64, tags: &[String]) -> bool {
    match window.scope.as_str() {
        "all" => true,
        "tag" => window
            .target_ref
            .as_deref()
            .and_then(|raw| serde_json::from_str::<String>(raw).ok())
            .is_some_and(|tag| tags.contains(&tag)),
        "monitors" => window
            .target_ref
            .as_deref()
            .and_then(|raw| serde_json::from_str::<Vec<i64>>(raw).ok())
            .is_some_and(|ids| ids.contains(&monitor_id)),
        _ => false,
    }
}

/// The strongest suppression currently applying to `monitor_id` (given its
/// `tags`) at instant `now`, folded over every window in `windows` that is
/// `is_active`, [`window_active_at`] `now`, and [`monitor_in_scope`] for
/// this monitor. `Suppression::Checks` wins over `Suppression::Alerts`
/// (any matching window suppressing checks takes precedence, regardless of
/// how many alerts-only windows also match); `Suppression::None` if no
/// window matches.
pub fn maintenance_for(windows: &[MaintenanceWindow], monitor_id: i64, tags: &[String], now: Ts) -> Suppression {
    let mut any_alerts = false;
    for w in windows
        .iter()
        .filter(|w| w.is_active && window_active_at(w, now) && monitor_in_scope(w, monitor_id, tags))
    {
        if w.suppress == "checks" {
            return Suppression::Checks;
        }
        any_alerts = true;
    }
    if any_alerts {
        Suppression::Alerts
    } else {
        Suppression::None
    }
}

/// Union of occurrence intervals — clipped to `[from, to]` — across every
/// `is_active` window in `windows` that [`monitor_in_scope`] puts in scope
/// for `(monitor_id, tags)`, regardless of `suppress` mode: both
/// alerts-only and checks-suppressing windows represent "this monitor was
/// under maintenance" time and should be excluded from an uptime-%
/// denominator (§8 of the design spec — active windows are excluded from
/// the denominator independent of which alerts they suppress).
/// Overlapping/adjacent intervals (from the same or different windows) are
/// merged; the result is sorted, non-overlapping, and every `(start, end)`
/// satisfies `start < end`. Empty if `to <= from`.
pub fn maintenance_intervals(windows: &[MaintenanceWindow], monitor_id: i64, tags: &[String], from: Ts, to: Ts) -> Vec<(Ts, Ts)> {
    if to <= from {
        return vec![];
    }
    let mut raw: Vec<(Ts, Ts)> = windows
        .iter()
        .filter(|w| w.is_active && monitor_in_scope(w, monitor_id, tags))
        .flat_map(|w| occurrences_overlapping(w, from, to))
        .filter_map(|(s, e)| {
            let cs = s.max(from);
            let ce = e.min(to);
            (cs < ce).then_some((cs, ce))
        })
        .collect();

    raw.sort_by_key(|iv| iv.0);

    let mut merged: Vec<(Ts, Ts)> = Vec::new();
    for (s, e) in raw.drain(..) {
        match merged.last_mut() {
            Some(last) if s <= last.1 => last.1 = last.1.max(e),
            _ => merged.push((s, e)),
        }
    }
    merged
}

/// `base` minus every interval in `cuts` — standard interval subtraction.
/// `cuts` need not be pre-sorted, pre-merged, or clipped to `base`; this
/// function sorts, clips, and coalesces them itself before walking left to
/// right, emitting the gap before each cut and finally whatever remains of
/// `base` after the last one. Returns `[]` if `base` is empty/inverted
/// (`base.0 >= base.1`) or if `cuts` fully covers `base`.
pub fn subtract_intervals(base: (Ts, Ts), cuts: &[(Ts, Ts)]) -> Vec<(Ts, Ts)> {
    let (base_start, base_end) = base;
    if base_start >= base_end {
        return vec![];
    }

    let mut clipped: Vec<(Ts, Ts)> = cuts
        .iter()
        .map(|&(s, e)| (s.max(base_start), e.min(base_end)))
        .filter(|&(s, e)| s < e)
        .collect();
    clipped.sort_by_key(|iv| iv.0);

    let mut merged: Vec<(Ts, Ts)> = Vec::new();
    for (s, e) in clipped {
        match merged.last_mut() {
            Some(last) if s <= last.1 => last.1 = last.1.max(e),
            _ => merged.push((s, e)),
        }
    }

    let mut result = Vec::new();
    let mut cursor = base_start;
    for (s, e) in merged {
        if cursor < s {
            result.push((cursor, s));
        }
        cursor = cursor.max(e);
    }
    if cursor < base_end {
        result.push((cursor, base_end));
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal `MaintenanceWindow` fixture builder — every field the pure
    /// resolve fns care about is a parameter; irrelevant fields (`name`,
    /// `created_at`) get fixed filler values.
    #[allow(clippy::too_many_arguments)]
    fn window(
        id: i64,
        scope: &str,
        target_ref: Option<&str>,
        starts_at: Ts,
        ends_at: Ts,
        recurrence: Option<&str>,
        suppress: &str,
        is_active: bool,
    ) -> MaintenanceWindow {
        MaintenanceWindow {
            id,
            name: format!("w{id}"),
            scope: scope.to_string(),
            target_ref: target_ref.map(str::to_string),
            starts_at,
            ends_at,
            recurrence: recurrence.map(str::to_string),
            suppress: suppress.to_string(),
            is_active,
            created_at: 0,
        }
    }

    // ---- window_active_at: one-off ----

    #[test]
    fn one_off_before_during_after() {
        let w = window(1, "all", None, 1_000, 2_000, None, "alerts", true);
        assert!(!window_active_at(&w, 999));
        assert!(window_active_at(&w, 1_000)); // inclusive start
        assert!(window_active_at(&w, 1_500));
        assert!(window_active_at(&w, 2_000)); // inclusive end
        assert!(!window_active_at(&w, 2_001));
    }

    // ---- window_active_at: cron ----
    // Fixed calendar epochs so `0 * * * *` (hourly, on the hour) lines up
    // with real UTC boundaries croner understands.
    const JAN1_00: Ts = 1_704_067_200; // 2024-01-01T00:00:00Z
    const JAN1_0130: Ts = 1_704_072_600; // 2024-01-01T01:30:00Z
    const JAN1_02: Ts = 1_704_074_400; // 2024-01-01T02:00:00Z
    const JAN1_0230: Ts = 1_704_076_200; // 2024-01-01T02:30:00Z
    const JAN1_0245: Ts = 1_704_077_100; // 2024-01-01T02:45:00Z
    const JAN1_03: Ts = 1_704_078_000; // 2024-01-01T03:00:00Z
    const JAN1_04: Ts = 1_704_081_600; // 2024-01-01T04:00:00Z
    const JAN1_05: Ts = 1_704_085_200; // 2024-01-01T05:00:00Z
    const JAN1_06: Ts = 1_704_088_800; // 2024-01-01T06:00:00Z
    const JAN2_00: Ts = 1_704_153_600; // 2024-01-02T00:00:00Z
    const DEC31_00: Ts = 1_703_980_800; // 2023-12-31T00:00:00Z

    #[test]
    fn cron_active_in_occurrence() {
        // hourly cron, 1h duration: now falls inside the 02:00-03:00 occurrence.
        let w = window(1, "all", None, DEC31_00, DEC31_00 + 3_600, Some("0 * * * *"), "alerts", true);
        assert!(window_active_at(&w, JAN1_0230));
    }

    #[test]
    fn cron_inactive_between_occurrences() {
        // hourly cron, but only a 30-minute duration per occurrence, so
        // there's a real gap between 02:30 (end of the 02:00 occurrence)
        // and 03:00 (start of the next).
        let w = window(1, "all", None, DEC31_00, DEC31_00 + 1_800, Some("0 * * * *"), "alerts", true);
        assert!(window_active_at(&w, JAN1_02)); // right at the occurrence start
        assert!(!window_active_at(&w, JAN1_0245)); // in the gap
        assert!(window_active_at(&w, JAN1_03)); // next occurrence starts
    }

    #[test]
    fn cron_inactive_before_starts_at() {
        // schedule doesn't begin until Jan 2 — querying Jan 1 (before the
        // configured start) must be inactive even though the bare cron
        // pattern would otherwise "occur" hourly there too.
        let w = window(1, "all", None, JAN2_00, JAN2_00 + 3_600, Some("0 * * * *"), "alerts", true);
        assert!(!window_active_at(&w, JAN1_0230));
    }

    #[test]
    fn cron_duration_exceeds_period_still_resolves_latest_occurrence() {
        // hourly cron (period 1h) but a 2h duration, so occurrences
        // overlap each other. now=01:30 sits inside BOTH the 00:00 and the
        // 01:00 occurrence's span; the forward scan must walk through
        // multiple overlapping occurrences and keep the LATEST start
        // (01:00), not stop at the first one found (00:00).
        let w = window(1, "all", None, JAN1_00, JAN1_00 + 7_200, Some("0 * * * *"), "alerts", true);
        assert!(window_active_at(&w, JAN1_0130));
    }

    // ---- occurrences_overlapping ----

    #[test]
    fn occurrences_overlapping_one_off() {
        let w = window(1, "all", None, 1_000, 2_000, None, "alerts", true);
        assert_eq!(occurrences_overlapping(&w, 500, 1_500), vec![(1_000, 2_000)]);
        assert_eq!(occurrences_overlapping(&w, 2_000, 3_000), vec![(1_000, 2_000)]); // touches at boundary
        assert_eq!(occurrences_overlapping(&w, 2_001, 3_000), vec![]);
        assert_eq!(occurrences_overlapping(&w, 0, 999), vec![]);
    }

    #[test]
    fn occurrences_overlapping_backs_up_from_minus_dur() {
        // The from-dur regression: hourly cron, 1h duration, range
        // [02:30, 05:00]. The 02:00 occurrence starts BEFORE `from` but
        // still extends into the range ([02:00,03:00] overlaps
        // [02:30,05:00] in exactly [02:30,03:00]) — scanning from `from`
        // alone (instead of `from - dur`) would miss it and wrongly count
        // that real maintenance slice as an outage.
        let w = window(1, "all", None, DEC31_00, DEC31_00 + 3_600, Some("0 * * * *"), "alerts", true);
        let occ = occurrences_overlapping(&w, JAN1_0230, JAN1_05);
        assert_eq!(
            occ,
            vec![
                (JAN1_02, JAN1_03), // starts before `from`, extends into range
                (JAN1_03, JAN1_04),
                (JAN1_04, JAN1_05),
                (JAN1_05, JAN1_06), // starts exactly at `to`, still included
            ]
        );
        // The 02:00 occurrence's overlap with [from,to] is exactly
        // [02:30,03:00] — the slice this regression guards.
        assert_eq!((occ[0].0.max(JAN1_0230), occ[0].1.min(JAN1_05)), (JAN1_0230, JAN1_03));
    }

    #[test]
    fn occurrences_overlapping_floored_by_starts_at() {
        let w = window(1, "all", None, JAN2_00, JAN2_00 + 3_600, Some("0 * * * *"), "alerts", true);
        assert_eq!(occurrences_overlapping(&w, JAN1_00, JAN1_02), vec![]);
    }

    #[test]
    fn scan_cap_floor_and_growth() {
        assert_eq!(scan_cap(0), 10_000);
        assert_eq!(scan_cap(-100), 10_000); // clamped, never underflows
        assert_eq!(scan_cap(600_000), 600_000 / 60 + 2);
    }

    // ---- monitor_in_scope ----

    #[test]
    fn monitor_in_scope_all() {
        let w = window(1, "all", None, 0, 0, None, "alerts", true);
        assert!(monitor_in_scope(&w, 42, &[]));
        assert!(monitor_in_scope(&w, 42, &["prod".to_string()]));
    }

    #[test]
    fn monitor_in_scope_tag_match_and_mismatch() {
        let w = window(1, "tag", Some("\"prod\""), 0, 0, None, "alerts", true);
        assert!(monitor_in_scope(&w, 1, &["prod".to_string(), "web".to_string()]));
        assert!(!monitor_in_scope(&w, 1, &["staging".to_string()]));
        assert!(!monitor_in_scope(&w, 1, &[]));
    }

    #[test]
    fn monitor_in_scope_tag_malformed_or_missing_target_ref_never_panics() {
        let missing = window(1, "tag", None, 0, 0, None, "alerts", true);
        assert!(!monitor_in_scope(&missing, 1, &["prod".to_string()]));
        let malformed = window(2, "tag", Some("not json"), 0, 0, None, "alerts", true);
        assert!(!monitor_in_scope(&malformed, 1, &["prod".to_string()]));
        let wrong_shape = window(3, "tag", Some("[1,2,3]"), 0, 0, None, "alerts", true);
        assert!(!monitor_in_scope(&wrong_shape, 1, &["prod".to_string()]));
        let empty = window(4, "tag", Some(""), 0, 0, None, "alerts", true);
        assert!(!monitor_in_scope(&empty, 1, &["prod".to_string()]));
    }

    #[test]
    fn monitor_in_scope_monitors_match_and_mismatch() {
        let w = window(1, "monitors", Some("[1,2,3]"), 0, 0, None, "alerts", true);
        assert!(monitor_in_scope(&w, 2, &[]));
        assert!(!monitor_in_scope(&w, 99, &[]));
    }

    #[test]
    fn monitor_in_scope_monitors_malformed_or_missing_target_ref_never_panics() {
        let missing = window(1, "monitors", None, 0, 0, None, "alerts", true);
        assert!(!monitor_in_scope(&missing, 2, &[]));
        let malformed = window(2, "monitors", Some("nope"), 0, 0, None, "alerts", true);
        assert!(!monitor_in_scope(&malformed, 2, &[]));
        let empty_array = window(3, "monitors", Some("[]"), 0, 0, None, "alerts", true);
        assert!(!monitor_in_scope(&empty_array, 2, &[]));
    }

    #[test]
    fn monitor_in_scope_unknown_scope_defaults_false() {
        let w = window(1, "bogus", None, 0, 0, None, "alerts", true);
        assert!(!monitor_in_scope(&w, 1, &[]));
    }

    // ---- maintenance_for ----

    #[test]
    fn maintenance_for_strongest_wins() {
        let now = 1_500;
        let alerts_w = window(1, "all", None, 1_000, 2_000, None, "alerts", true);
        let checks_w = window(2, "all", None, 1_000, 2_000, None, "checks", true);
        assert_eq!(maintenance_for(std::slice::from_ref(&alerts_w), 1, &[], now), Suppression::Alerts);
        assert_eq!(maintenance_for(&[alerts_w.clone(), checks_w.clone()], 1, &[], now), Suppression::Checks);
        // order-independent: checks wins even listed first
        assert_eq!(maintenance_for(&[checks_w, alerts_w], 1, &[], now), Suppression::Checks);
    }

    #[test]
    fn maintenance_for_none_when_no_match() {
        let now = 1_500;
        let inactive = window(1, "all", None, 1_000, 2_000, None, "checks", false);
        let out_of_time = window(2, "all", None, 3_000, 4_000, None, "checks", true);
        let out_of_scope = window(3, "monitors", Some("[99]"), 1_000, 2_000, None, "checks", true);
        assert_eq!(maintenance_for(&[inactive], 1, &[], now), Suppression::None);
        assert_eq!(maintenance_for(&[out_of_time], 1, &[], now), Suppression::None);
        assert_eq!(maintenance_for(&[out_of_scope], 1, &[], now), Suppression::None);
        assert_eq!(maintenance_for(&[], 1, &[], now), Suppression::None);
    }

    // ---- maintenance_intervals ----

    #[test]
    fn maintenance_intervals_clips_to_range() {
        let w = window(1, "all", None, 1_000, 3_000, None, "alerts", true);
        assert_eq!(maintenance_intervals(&[w], 1, &[], 1_500, 2_500), vec![(1_500, 2_500)]);
    }

    #[test]
    fn maintenance_intervals_merges_overlapping_windows() {
        let a = window(1, "all", None, 1_000, 2_000, None, "alerts", true);
        let b = window(2, "all", None, 1_500, 2_500, None, "checks", true);
        assert_eq!(maintenance_intervals(&[a, b], 1, &[], 0, 5_000), vec![(1_000, 2_500)]);
    }

    #[test]
    fn maintenance_intervals_skips_inactive_and_out_of_scope() {
        let inactive = window(1, "all", None, 1_000, 2_000, None, "alerts", false);
        let out_of_scope = window(2, "monitors", Some("[99]"), 1_000, 2_000, None, "alerts", true);
        assert_eq!(maintenance_intervals(&[inactive, out_of_scope], 1, &[], 0, 5_000), vec![]);
    }

    // ---- subtract_intervals ----

    #[test]
    fn subtract_intervals_no_cuts() {
        assert_eq!(subtract_intervals((0, 100), &[]), vec![(0, 100)]);
    }

    #[test]
    fn subtract_intervals_partial_cut() {
        assert_eq!(subtract_intervals((0, 100), &[(20, 40)]), vec![(0, 20), (40, 100)]);
        assert_eq!(subtract_intervals((0, 100), &[(-10, 10)]), vec![(10, 100)]);
        assert_eq!(subtract_intervals((0, 100), &[(90, 200)]), vec![(0, 90)]);
    }

    #[test]
    fn subtract_intervals_full_cut() {
        assert_eq!(subtract_intervals((0, 100), &[(0, 100)]), vec![]);
        assert_eq!(subtract_intervals((0, 100), &[(-10, 200)]), vec![]);
    }

    #[test]
    fn subtract_intervals_multiple_unsorted_overlapping_cuts() {
        let cuts = vec![(60, 80), (10, 20), (15, 25)]; // unsorted; (15,25) overlaps (10,20)
        assert_eq!(subtract_intervals((0, 100), &cuts), vec![(0, 10), (25, 60), (80, 100)]);
    }

    #[test]
    fn subtract_intervals_empty_or_inverted_base() {
        assert_eq!(subtract_intervals((50, 50), &[(0, 100)]), vec![]);
        assert_eq!(subtract_intervals((100, 50), &[]), vec![]);
    }

    // ---- parse_tags ----

    #[test]
    fn parse_tags_valid() {
        assert_eq!(parse_tags(r#"["prod","web"]"#), vec!["prod".to_string(), "web".to_string()]);
        assert_eq!(parse_tags("[]"), Vec::<String>::new());
    }

    #[test]
    fn parse_tags_malformed_or_wrong_shape_or_empty_never_panics() {
        assert_eq!(parse_tags(""), Vec::<String>::new());
        assert_eq!(parse_tags("not json"), Vec::<String>::new());
        assert_eq!(parse_tags("{}"), Vec::<String>::new());
        assert_eq!(parse_tags("[1,2,3]"), Vec::<String>::new());
    }
}
