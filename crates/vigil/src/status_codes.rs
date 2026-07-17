//! Parser for HTTP status code specifications.
//! Accepts formats like "200-299,301,418" where ranges are inclusive on both ends.

/// Parse a status code specification into ranges.
///
/// # Arguments
/// * `spec` - A comma-separated list of status codes or ranges (e.g., "200-299,301,418")
///
/// # Returns
/// * `Ok(Vec<(u16,u16)>)` - Vector of (start, end) ranges, inclusive
/// * `Err(String)` - On parse error
pub fn parse_expected(spec: &str) -> Result<Vec<(u16, u16)>, String> {
    spec.split(',')
        .map(|token| {
            let token = token.trim();
            if token.contains('-') {
                let parts: Vec<&str> = token.split('-').collect();
                if parts.len() != 2 {
                    return Err(format!("Invalid range: {}", token));
                }
                let start = parts[0]
                    .parse::<u16>()
                    .map_err(|_| format!("Invalid start in range: {}", token))?;
                let end = parts[1]
                    .parse::<u16>()
                    .map_err(|_| format!("Invalid end in range: {}", token))?;
                if start > end {
                    return Err(format!("Invalid range (start > end): {}", token));
                }
                Ok((start, end))
            } else {
                let code = token
                    .parse::<u16>()
                    .map_err(|_| format!("Invalid status code: {}", token))?;
                Ok((code, code))
            }
        })
        .collect()
}

/// Check if a status code matches any of the expected ranges.
///
/// # Arguments
/// * `ranges` - Vector of (start, end) ranges, inclusive
/// * `code` - Status code to check
///
/// # Returns
/// * `true` if code is within any range, `false` otherwise
pub fn matches(ranges: &[(u16, u16)], code: u16) -> bool {
    ranges.iter().any(|(start, end)| code >= *start && code <= *end)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ranges_and_singles() {
        let r = parse_expected("200-299,301,418").unwrap();
        assert!(matches(&r, 200) && matches(&r, 299) && matches(&r, 301) && matches(&r, 418));
        assert!(!matches(&r, 300) && !matches(&r, 500));
    }

    #[test]
    fn rejects_garbage() {
        assert!(parse_expected("abc").is_err());
    }

    #[test]
    fn default_2xx() {
        let r = parse_expected("200-299").unwrap();
        assert!(matches(&r, 204) && !matches(&r, 199));
    }

    #[test]
    fn rejects_reversed_range() {
        assert!(parse_expected("299-200").is_err());
        assert!(parse_expected("200-299").is_ok()); // forward range still fine
    }
}
