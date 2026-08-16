//! Error types for rs-nightshift.

use thiserror::Error;

/// Recoverable library failure.
#[derive(Error, Debug)]
pub enum Error {
    /// An HTTP call to Ollama failed.
    #[error("Ollama request failed: {0}")]
    Ollama(String),

    /// Ollama has no such model installed.
    #[error("Ollama model not found: {model}")]
    ModelNotFound {
        /// Model tag that was requested.
        model: String,
    },

    /// The Ollama request exceeded the client timeout.
    #[error("Ollama request timed out")]
    Timeout,

    /// An I/O operation failed.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

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

    /// `codegraph` / `graphify` context gathering failed.
    #[error("context tools: {0}")]
    Context(String),

    /// Git inspect or apply failed (never commit/push/reset/clean).
    #[error("git: {0}")]
    Git(String),

    /// `nightshift.toml` could not be read or parsed.
    #[error("config error in {path}: {message}")]
    Config {
        /// Path to the config file that failed.
        path: String,
        /// Human-readable read or parse failure.
        message: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ollama_error_displays_message() {
        let error = Error::Ollama("connection refused".into());
        assert_eq!(
            error.to_string(),
            "Ollama request failed: connection refused"
        );
    }

    #[test]
    fn io_error_converts() {
        let error = Error::from(std::io::Error::other("disk"));
        assert!(error.to_string().contains("disk"));
    }

    #[test]
    fn model_not_found_and_timeout_are_distinct() {
        let missing = Error::ModelNotFound {
            model: "nope".into(),
        };
        assert_eq!(missing.to_string(), "Ollama model not found: nope");
        assert_eq!(Error::Timeout.to_string(), "Ollama request timed out");
    }

    #[test]
    fn artifact_error_displays_message() {
        let error = Error::Artifact("no latest run".into());
        assert_eq!(error.to_string(), "artifact error: no latest run");
    }

    #[test]
    fn invalid_artifact_displays_file_and_reason() {
        let error = Error::InvalidArtifact {
            artifact: "01_user_story.md",
            reason: "missing headings: Out of Scope".into(),
        };
        assert_eq!(
            error.to_string(),
            "invalid artifact 01_user_story.md: missing headings: Out of Scope"
        );
    }

    #[test]
    fn context_error_displays_message() {
        let error = Error::Context("codegraph is not on PATH".into());
        assert_eq!(error.to_string(), "context tools: codegraph is not on PATH");
    }

    #[test]
    fn git_error_displays_message() {
        let error = Error::Git("apply --check failed".into());
        assert_eq!(error.to_string(), "git: apply --check failed");
    }

    #[test]
    fn config_error_displays_path_and_message() {
        let error = Error::Config {
            path: "nightshift.toml".into(),
            message: "expected `=`".into(),
        };
        assert_eq!(
            error.to_string(),
            "config error in nightshift.toml: expected `=`"
        );
    }
}
