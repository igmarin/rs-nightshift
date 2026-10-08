//! Git-adapter failures: inspect/apply primitives that never commit/push.
//!
//! Extracted from the former monolithic `Error` enum so [`crate::adapters::git`]
//! owns a typed error surface. The top-level [`enum@crate::error::Error`] wraps this
//! type via `#[from]`, so `Result<T, Error>` call sites lift a `GitError`
//! automatically with `?`. The display attribute is preserved verbatim,
//! keeping operator-facing messages stable.

use thiserror::Error;

/// Git inspect or apply failed (never commit/push/reset/clean).
#[derive(Error, Debug)]
#[error("git: {0}")]
pub struct GitError(pub String);

impl GitError {
    /// Build a git error from a message.
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn git_error_displays_message() {
        let error = GitError::new("apply --check failed");
        assert_eq!(error.to_string(), "git: apply --check failed");
    }

    #[test]
    fn git_error_lifts_into_top_level_error() {
        let top: crate::error::Error = GitError::new("dirty tree").into();
        assert_eq!(top.to_string(), "git: dirty tree");
    }
}
