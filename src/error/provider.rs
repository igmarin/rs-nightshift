//! Provider-layer failures: Ollama/OpenAI HTTP, model lookup, timeout, URL.
//!
//! Extracted from the former monolithic `Error` enum so the LLM-provider
//! adapters (`adapters::ollama`, `adapters::providers::*`) own a typed error
//! surface instead of funnelling every failure through a single global enum.
//!
//! The top-level [`enum@crate::error::Error`] wraps this type via `#[from]`, so
//! `Result<T, Error>` call sites lift a `ProviderError` automatically with `?`.
//! [`thiserror::Error`] display attributes are preserved verbatim from the
//! original enum, keeping operator-facing messages stable.

use thiserror::Error;

/// LLM-provider failure (Ollama, OpenAI-compatible, or the kernel HTTP layer).
#[derive(Error, Debug)]
pub enum ProviderError {
    /// The configured Ollama URL is not a valid HTTP origin: it must be an
    /// `http://` or `https://` URL with a host and no path, query, fragment,
    /// or userinfo credentials.
    #[error("invalid Ollama URL {url:?}: expected an http:// or https:// URL with a host")]
    InvalidOllamaUrl {
        /// The operator-provided URL, with credentials redacted when possible.
        url: String,
    },

    /// An HTTP call to the model provider failed.
    #[error("Ollama request failed: {0}")]
    Ollama(String),

    /// The provider has no such model installed.
    #[error("Ollama model not found: {model}")]
    ModelNotFound {
        /// Model tag that was requested.
        model: String,
    },

    /// The provider request exceeded the client timeout.
    #[error("Ollama request timed out")]
    Timeout,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ollama_error_displays_message() {
        let error = ProviderError::Ollama("connection refused".into());
        assert_eq!(
            error.to_string(),
            "Ollama request failed: connection refused"
        );
    }

    #[test]
    fn model_not_found_and_timeout_are_distinct() {
        let missing = ProviderError::ModelNotFound {
            model: "nope".into(),
        };
        assert_eq!(missing.to_string(), "Ollama model not found: nope");
        assert_eq!(
            ProviderError::Timeout.to_string(),
            "Ollama request timed out"
        );
    }

    #[test]
    fn invalid_ollama_url_displays_redacted_url() {
        let error = ProviderError::InvalidOllamaUrl {
            url: "http://localhost:11434".into(),
        };
        assert!(error.to_string().contains("http://localhost:11434"));
        assert!(error.to_string().contains("invalid Ollama URL"));
    }

    #[test]
    fn provider_error_lifts_into_top_level_error() {
        let top: crate::error::Error = ProviderError::Timeout.into();
        assert_eq!(top.to_string(), "Ollama request timed out");
    }
}
