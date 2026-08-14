//! Error types for rs-nightshift.

use thiserror::Error;

/// Recoverable library failure.
#[derive(Error, Debug)]
pub enum Error {
    /// An HTTP call to Ollama failed.
    #[error("Ollama request failed: {0}")]
    Ollama(String),

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
}
