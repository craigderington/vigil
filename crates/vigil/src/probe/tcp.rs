//! TCP Port / Ping prober.
//!
//! Placeholder for Task 3, which fills in the real TCP-connect (`port`) and
//! ICMP-with-TCP-fallback (`ping`) logic. Always reports failure so
//! `probe::run`'s dispatch can be exercised (and compile) before the real
//! implementation lands.

use crate::models::{Cause, Monitor, ProbeOutcome};

pub async fn probe(_m: &Monitor) -> ProbeOutcome {
    ProbeOutcome {
        ok: false,
        response_time_ms: None,
        status_code: None,
        error_message: Some("not yet implemented".into()),
        resolved_ip: None,
        cause: Some(Cause::Connection),
    }
}
