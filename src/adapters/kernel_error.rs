//! `llm-kernel` error mapping shared by the provider adapters.

use crate::error::Error;
use llm_kernel::error::KernelError;

/// Convert an `llm-kernel` error into the rs-nightshift `Error` enum.
///
/// `model` is the requested model tag, used for [`Error::ModelNotFound`] so
/// the error reports the model the caller asked for rather than the HTTP
/// response body (which may contain a full API error JSON object).
///
/// `KernelError::Timeout` maps to [`Error::Timeout`];
/// `KernelError::Http { status: 404, .. }` maps to [`Error::ModelNotFound`]
/// using `model`;
/// `KernelError::LlmApi` whose message contains "timed out" also maps to
/// [`Error::Timeout`] (the kernel's `OpenAIClient` surfaces reqwest timeouts
/// as `LlmApi` rather than `Timeout`); everything else maps to
/// [`Error::Ollama`] with the kernel's message.
pub fn map_kernel_error(error: KernelError, model: &str) -> Error {
    match error {
        KernelError::Timeout(_) => Error::Timeout,
        KernelError::LlmApi(ref msg) if msg.contains("timed out") => Error::Timeout,
        KernelError::Http { status: 404, .. } => Error::ModelNotFound {
            model: model.to_string(),
        },
        other => Error::Ollama(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn map_kernel_error_timeout_maps_to_timeout() {
        let err = map_kernel_error(KernelError::Timeout(120), "m");
        assert!(matches!(err, Error::Timeout));
    }

    #[test]
    fn map_kernel_error_http_404_maps_to_model_not_found() {
        let err = map_kernel_error(
            KernelError::Http {
                status: 404,
                message: "some API error body".into(),
            },
            "qwen2.5-coder:7b",
        );
        match err {
            Error::ModelNotFound { model } => assert_eq!(model, "qwen2.5-coder:7b"),
            other => panic!("expected ModelNotFound, got {other:?}"),
        }
    }

    #[test]
    fn map_kernel_error_other_maps_to_ollama() {
        let err = map_kernel_error(KernelError::LlmApi("boom".into()), "m");
        match err {
            Error::Ollama(msg) => assert!(msg.contains("boom"), "{msg}"),
            other => panic!("expected Ollama, got {other:?}"),
        }
    }
}
