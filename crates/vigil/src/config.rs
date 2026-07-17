//! Runtime configuration, loaded from environment variables with
//! Global-Constraints defaults (see docs/superpowers/plans/2026-07-17-vigil-p1-mvp.md).

/// Default bind address: `0.0.0.0:8080`.
pub const DEFAULT_BIND: &str = "0.0.0.0:8080";
/// Default SQLite database path inside the container's `/data` volume.
pub const DEFAULT_DB_PATH: &str = "/data/vigil.db";
/// Default global probe-concurrency cap.
pub const DEFAULT_MAX_CONCURRENCY: usize = 25;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
    pub bind: String,
    pub db_path: String,
    pub max_concurrency: usize,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            bind: DEFAULT_BIND.to_string(),
            db_path: DEFAULT_DB_PATH.to_string(),
            max_concurrency: DEFAULT_MAX_CONCURRENCY,
        }
    }
}

impl Config {
    /// Build a `Config` from environment variables, falling back to defaults
    /// for anything unset or unparsable.
    ///
    /// - `VIGIL_BIND` (default `0.0.0.0:8080`)
    /// - `VIGIL_DB` (default `/data/vigil.db`)
    /// - `VIGIL_MAX_CONCURRENCY` (default `25`)
    pub fn from_env() -> Self {
        let defaults = Self::default();
        let bind = std::env::var("VIGIL_BIND").unwrap_or(defaults.bind);
        let db_path = std::env::var("VIGIL_DB").unwrap_or(defaults.db_path);
        let max_concurrency = std::env::var("VIGIL_MAX_CONCURRENCY")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(defaults.max_concurrency);
        Self {
            bind,
            db_path,
            max_concurrency,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_global_constraints() {
        let cfg = Config::default();
        assert_eq!(cfg.bind, "0.0.0.0:8080");
        assert_eq!(cfg.db_path, "/data/vigil.db");
        assert_eq!(cfg.max_concurrency, 25);
    }
}
