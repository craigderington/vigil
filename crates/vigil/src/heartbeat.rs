//! Heartbeat (push) monitor support (§3 "Heartbeat (push)", §9
//! `heartbeat_token`). This module currently holds just the capability-token
//! generator used by `api::monitors::create` — the `/ping/:token` receiver
//! and reaper land in a later P4.1 task.

/// A 32-char alphanumeric capability token for a heartbeat monitor's
/// `/ping/:token` push-URL. Not a guessable sequence — this is the sole
/// secret gating who can post a ping for a given monitor, so it's drawn
/// from `rand`'s CSPRNG-backed thread-local generator, not `id`-derived.
pub fn generate_token() -> String {
    use rand::{distributions::Alphanumeric, Rng};
    rand::thread_rng().sample_iter(&Alphanumeric).take(32).map(char::from).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generates_32_char_alphanumeric_token() {
        let t = generate_token();
        assert_eq!(t.len(), 32);
        assert!(t.chars().all(|c| c.is_ascii_alphanumeric()));
    }

    #[test]
    fn two_calls_differ() {
        assert_ne!(generate_token(), generate_token());
    }
}
