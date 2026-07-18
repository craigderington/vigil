//! DNS prober.
//!
//! Placeholder for Task 4, which fills in the real resolver-based logic
//! (resolve `host` for `dns_record_type`, compare against
//! `dns_expected_value` when set). Always reports failure so
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
