//! Artifact failures: directory/report handling and schema validation.
//!
//! Extracted from the former monolithic `Error` enum so the artifact store,
//! state store, test runner, and legacy stages share one typed error surface.
//! The top-level [`enum@crate::error::Error`] wraps this type via `#[from]`, so
//! `Result<T, Error>` call sites lift an `ArtifactError` automatically with
//! `?`. The display attributes are preserved verbatim, keeping operator-facing
//! messages stable.

use thiserror::Error;

/// Artifact directory, report handling, or schema validation failure.
#[derive(Error, Debug)]
pub enum ArtifactError {
    /// Artifact directory or report handling failed.
    #[error("artifact error: {0}")]
    Artifact(String),

    /// Model output failed schema validation (after optional repair).
    #[error("invalid artifact {artifact}: {reason}")]
    InvalidArtifact {
        /// Artifact file that failed validation.
        artifact: &'static str,
        /// Human-readable validation failure.
        reason: String,
    },
}

impl ArtifactError {
    /// Build a generic artifact error from a message.
    #[must_use]
    pub fn artifact(message: impl Into<String>) -> Self {
        Self::Artifact(message.into())
    }

    /// Build an invalid-artifact error from a file name and a reason.
    #[must_use]
    pub fn invalid(artifact: &'static str, reason: impl Into<String>) -> Self {
        Self::InvalidArtifact {
            artifact,
            reason: reason.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn artifact_error_displays_message() {
        let error = ArtifactError::artifact("no latest run");
        assert_eq!(error.to_string(), "artifact error: no latest run");
    }

    #[test]
    fn invalid_artifact_displays_file_and_reason() {
        let error = ArtifactError::invalid("01_user_story.md", "missing headings: Out of Scope");
        assert_eq!(
            error.to_string(),
            "invalid artifact 01_user_story.md: missing headings: Out of Scope"
        );
    }

    #[test]
    fn artifact_error_lifts_into_top_level_error() {
        let top: crate::error::Error = ArtifactError::artifact("missing dir").into();
        assert_eq!(top.to_string(), "artifact error: missing dir");
    }

    #[test]
    fn invalid_artifact_lifts_into_top_level_error() {
        let top: crate::error::Error =
            ArtifactError::invalid("03_diff.patch", "escapes repo").into();
        assert_eq!(
            top.to_string(),
            "invalid artifact 03_diff.patch: escapes repo"
        );
    }
}
