//! Configuration failures: `nightshift.toml` read/parse and role-graph config.
//!
//! Extracted from the former monolithic `Error` enum so the config-reading
//! sites (`models.rs`, `domain::rolegraph::config`, `adapters::providers::*`)
//! share one typed error. The top-level [`enum@crate::error::Error`] wraps this type
//! via `#[from]`, so `Result<T, Error>` call sites lift a `ConfigError`
//! automatically with `?`. The display attribute is preserved verbatim,
//! keeping operator-facing messages stable.

use thiserror::Error;

/// `nightshift.toml` (or a role-graph config file) could not be read or parsed.
#[derive(Error, Debug)]
#[error("config error in {path}: {message}")]
pub struct ConfigError {
    /// Path to the config file that failed.
    pub path: String,
    /// Human-readable read or parse failure.
    pub message: String,
}

impl ConfigError {
    /// Build a config error from a path and a message.
    #[must_use]
    pub fn new(path: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            message: message.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_error_displays_path_and_message() {
        let error = ConfigError::new("nightshift.toml", "expected `=`");
        assert_eq!(
            error.to_string(),
            "config error in nightshift.toml: expected `=`"
        );
    }

    #[test]
    fn config_error_lifts_into_top_level_error() {
        let top: crate::error::Error = ConfigError::new("nightshift.toml", "missing roles").into();
        assert_eq!(
            top.to_string(),
            "config error in nightshift.toml: missing roles"
        );
    }
}
