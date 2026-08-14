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
}
