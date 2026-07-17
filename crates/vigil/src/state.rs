//! Pure state machine for monitor status transitions.
//!
//! No I/O, no async, no DB access — a single function over inputs that
//! decides the next status, updated consecutive counters, and whether a
//! transition (incident open/close, unknown edge) occurred.

use crate::models::*;

pub struct Thresholds {
    pub confirmation: i64,
    pub recovery: i64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Decision {
    pub next_status: Status,
    pub consecutive_failures: i64,
    pub consecutive_successes: i64,
    pub transition: Option<Transition>,
    pub use_retry_interval: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Transition {
    ToDownOpenIncident,
    ToUpCloseIncident,
    ToUpNoIncident,
    ToUnknown,
}

pub struct Inputs {
    pub current: Status,
    pub prev_confirmed: Status,
    pub consecutive_failures: i64,
    pub consecutive_successes: i64,
    pub outcome_ok: bool,
    pub anchor: Connectivity,
    pub th: Thresholds,
}

pub fn evaluate(i: &Inputs) -> Decision {
    let (mut f, mut s) = (i.consecutive_failures, i.consecutive_successes);
    if i.outcome_ok {
        s += 1;
        f = 0;
        let recovered = matches!(i.current, Status::Down)
            || (matches!(i.current, Status::Unknown) && matches!(i.prev_confirmed, Status::Down));
        if recovered && s >= i.th.recovery {
            return Decision {
                next_status: Status::Up,
                consecutive_failures: f,
                consecutive_successes: s,
                transition: Some(Transition::ToUpCloseIncident),
                use_retry_interval: false,
            };
        }
        let to_up_fresh = matches!(i.current, Status::Pending)
            || (matches!(i.current, Status::Unknown) && !matches!(i.prev_confirmed, Status::Down));
        let trans = if to_up_fresh { Some(Transition::ToUpNoIncident) } else { None };
        return Decision {
            next_status: Status::Up,
            consecutive_failures: f,
            consecutive_successes: s,
            transition: trans,
            use_retry_interval: false,
        };
    }
    // failure
    f += 1;
    s = 0;
    if f < i.th.confirmation {
        return Decision {
            next_status: active_display(i.current),
            consecutive_failures: f,
            consecutive_successes: s,
            transition: None,
            use_retry_interval: true,
        };
    }
    match i.anchor {
        Connectivity::Offline => Decision {
            next_status: Status::Unknown,
            consecutive_failures: f,
            consecutive_successes: 0,
            transition: Some(Transition::ToUnknown),
            use_retry_interval: false,
        },
        Connectivity::Online => {
            let already_down = matches!(i.current, Status::Down)
                || (matches!(i.current, Status::Unknown) && matches!(i.prev_confirmed, Status::Down));
            let trans = if already_down { None } else { Some(Transition::ToDownOpenIncident) };
            Decision {
                next_status: Status::Down,
                consecutive_failures: f,
                consecutive_successes: 0,
                transition: trans,
                use_retry_interval: false,
            }
        }
    }
}

/// During pre-confirmation retry, a failing-but-unconfirmed monitor still
/// displays as Up (it hasn't crossed the confirmation threshold yet).
/// Down and Paused are preserved as-is.
fn active_display(current: Status) -> Status {
    match current {
        Status::Down => Status::Down,
        Status::Paused => Status::Paused,
        _ => Status::Up,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{Connectivity::*, Status::*};
    fn inp(cur: Status, prev: Status, f: i64, s: i64, ok: bool, a: crate::models::Connectivity) -> Inputs {
        Inputs { current: cur, prev_confirmed: prev, consecutive_failures: f, consecutive_successes: s,
                 outcome_ok: ok, anchor: a, th: Thresholds { confirmation: 3, recovery: 1 } } }
    #[test] fn pending_first_success_goes_up() { let d=evaluate(&inp(Pending,Pending,0,0,true,Online));
        assert_eq!(d.next_status,Up); assert_eq!(d.transition,Some(Transition::ToUpNoIncident)); }
    #[test] fn one_failure_stays_up_and_retries() { let d=evaluate(&inp(Up,Up,0,0,false,Online));
        assert_eq!(d.next_status,Up); assert_eq!(d.consecutive_failures,1); assert!(d.use_retry_interval);
        assert_eq!(d.transition,None); }
    #[test] fn third_failure_anchors_up_goes_down() { let d=evaluate(&inp(Up,Up,2,0,false,Online));
        assert_eq!(d.next_status,Down); assert_eq!(d.consecutive_failures,3);
        assert_eq!(d.transition,Some(Transition::ToDownOpenIncident)); }
    #[test] fn third_failure_anchors_down_goes_unknown() { let d=evaluate(&inp(Up,Up,2,0,false,Offline));
        assert_eq!(d.next_status,Unknown); assert_eq!(d.transition,Some(Transition::ToUnknown)); }
    #[test] fn down_recovers_on_first_success() { let d=evaluate(&inp(Down,Down,3,0,true,Online));
        assert_eq!(d.next_status,Up); assert_eq!(d.transition,Some(Transition::ToUpCloseIncident)); }
    #[test] fn unknown_to_up_prev_up() { let d=evaluate(&inp(Unknown,Up,0,0,true,Online));
        assert_eq!(d.next_status,Up); assert_eq!(d.transition,Some(Transition::ToUpNoIncident)); }
    #[test] fn unknown_to_up_prev_down_closes() { let d=evaluate(&inp(Unknown,Down,0,0,true,Online));
        assert_eq!(d.next_status,Up); assert_eq!(d.transition,Some(Transition::ToUpCloseIncident)); }
    #[test] fn unknown_confirmed_fail_prev_up_opens() { let d=evaluate(&inp(Unknown,Up,2,0,false,Online));
        assert_eq!(d.next_status,Down); assert_eq!(d.transition,Some(Transition::ToDownOpenIncident)); }
    #[test] fn unknown_confirmed_fail_prev_down_no_dup() { let d=evaluate(&inp(Unknown,Down,2,0,false,Online));
        assert_eq!(d.next_status,Down); assert_eq!(d.transition,None); }
    #[test] fn fail_while_offline_never_opens_incident() { let d=evaluate(&inp(Down,Down,2,0,false,Offline));
        assert_eq!(d.next_status,Unknown); assert_eq!(d.transition,Some(Transition::ToUnknown)); }
}
