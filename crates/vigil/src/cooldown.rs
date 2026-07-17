use crate::models::Ts;

/// true => allowed to send (no same-(monitor,trigger) send within the cooldown window)
pub fn allowed(last_sent_at: Option<Ts>, now: Ts, cooldown_minutes: i64) -> bool {
    last_sent_at.is_none_or(|t| now - t >= cooldown_minutes * 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_allowed() {
        assert!(allowed(None, 1000, 15));
    }

    #[test]
    fn within_blocked() {
        assert!(!allowed(Some(1000), 1000 + 14 * 60, 15));
    }

    #[test]
    fn after_allowed() {
        assert!(allowed(Some(1000), 1000 + 15 * 60, 15));
    }
}
