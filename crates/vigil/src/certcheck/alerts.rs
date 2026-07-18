/// Fire-once, most-urgent-first tiered alert logic for SSL and domain expiry.
/// Used by both `ssl_expiring` and `domain_expiring` notifications.

#[derive(Debug, Clone, PartialEq)]
pub struct TierDecision {
    pub fire: bool,
    pub new_alerted_days: Option<i64>,
}

/// Determine whether to fire an expiry alert and update the alerted threshold.
///
/// # Arguments
/// - `days_remaining`: days until expiry (e.g., SSL cert or domain registration)
/// - `alert_days`: thresholds at which to fire alerts (e.g., [30, 14, 7, 3, 1])
/// - `alerted_days`: the threshold at which we last fired (None = never fired)
///
/// # Logic
/// 1. **Renewal detection:** if `alerted_days` is Some and `days_remaining > alerted_days`,
///    the cert/domain has been renewed → reset to no alert.
/// 2. **Most-urgent-first fire-once:** find the smallest threshold T in `alert_days` where:
///    - `days_remaining <= T` (threshold crossed)
///    - `alerted_days.is_none() OR T < alerted_days` (not yet fired at this tier)
/// 3. If such a T exists, fire and set `new_alerted_days = Some(T)`.
/// 4. Otherwise, do not fire and preserve `alerted_days`.
pub fn tier(
    days_remaining: i64,
    alert_days: &[i64],
    alerted_days: Option<i64>,
) -> TierDecision {
    // Renewal case: if we've alerted before and days_remaining is now greater,
    // the certificate has been renewed → reset alerted state.
    if let Some(prev_alerted) = alerted_days {
        if days_remaining > prev_alerted {
            return TierDecision {
                fire: false,
                new_alerted_days: None,
            };
        }
    }

    // Find the smallest threshold T where:
    // - days_remaining <= T (threshold is crossed)
    // - alerted_days is None OR T < alerted_days (we haven't fired at this tier yet)
    let next_tier = alert_days
        .iter()
        .filter(|&&t| {
            days_remaining <= t
                && alerted_days.is_none_or(|a| t < a)
        })
        .min();

    match next_tier {
        Some(&t) => TierDecision {
            fire: true,
            new_alerted_days: Some(t),
        },
        None => TierDecision {
            fire: false,
            new_alerted_days: alerted_days,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fresh_cert_no_fire() {
        // Fresh cert, 40 days, [30,14,7,3,1], never alerted
        // 40 > 30, so no threshold is crossed → no fire
        let result = tier(40, &[30, 14, 7, 3, 1], None);
        assert_eq!(result, TierDecision {
            fire: false,
            new_alerted_days: None,
        });
    }

    #[test]
    fn test_25_days_fire_30() {
        // 25 days, [30,14,7,3,1], never alerted
        // Thresholds crossed: 30 (25<=30 ✓), not 14 (25>14)
        // Smallest crossed = 30 → fire 30
        let result = tier(25, &[30, 14, 7, 3, 1], None);
        assert_eq!(result, TierDecision {
            fire: true,
            new_alerted_days: Some(30),
        });
    }

    #[test]
    fn test_10_days_alerted_30_fire_14() {
        // 10 days, [30,14,7,3,1], already alerted at 30
        // Smallest T with 10<=T and T<30: candidates {14,7,3,1}, 10<=T gives {14}
        // Fire 14
        let result = tier(10, &[30, 14, 7, 3, 1], Some(30));
        assert_eq!(result, TierDecision {
            fire: true,
            new_alerted_days: Some(14),
        });
    }

    #[test]
    fn test_5_days_alerted_14_fire_7() {
        // 5 days, [30,14,7,3,1], already alerted at 14
        // Smallest T with 5<=T and T<14: candidates {7,3,1}, 5<=T gives {7}
        // Fire 7
        let result = tier(5, &[30, 14, 7, 3, 1], Some(14));
        assert_eq!(result, TierDecision {
            fire: true,
            new_alerted_days: Some(7),
        });
    }

    #[test]
    fn test_renewal_reset() {
        // Renewal: 40 days, [30,14,7,3,1], was alerted at 7
        // days_remaining (40) > alerted_days (7) → renewal, reset
        let result = tier(40, &[30, 14, 7, 3, 1], Some(7));
        assert_eq!(result, TierDecision {
            fire: false,
            new_alerted_days: None,
        });
    }

    #[test]
    fn test_order_independence() {
        // Same input (25 days, never alerted) with shuffled thresholds
        // [7,30,1,14,3] is the same set as [30,14,7,3,1]
        // Should fire at 30 just like the sorted case
        let shuffled = &[7, 30, 1, 14, 3];
        let result = tier(25, shuffled, None);
        assert_eq!(result, TierDecision {
            fire: true,
            new_alerted_days: Some(30),
        });
    }
}
