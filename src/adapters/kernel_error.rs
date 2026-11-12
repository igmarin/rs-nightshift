//! `llm-kernel` error mapping shared by the provider adapters.

use crate::error::ProviderError;
use llm_kernel::error::KernelError;

/// Convert an `llm-kernel` error into the rs-nightshift `ProviderError` enum.
///
/// `model` is the requested model tag, used for [`ProviderError::ModelNotFound`]
/// so the error reports the model the caller asked for rather than the HTTP
/// response body (which may contain a full API error JSON object).
///
/// `KernelError::Timeout` maps to [`ProviderError::Timeout`];
/// `KernelError::Http { status: 404, .. }` maps to
/// [`ProviderError::ModelNotFound`] using `model`;
/// `KernelError::LlmApi` whose message contains "timed out" also maps to
/// [`ProviderError::Timeout`] (the kernel's `OpenAIClient` surfaces reqwest
/// timeouts as `LlmApi` rather than `Timeout`); everything else maps to
/// [`ProviderError::Ollama`] with the kernel's message.
///
/// Callers in `Result<_, Error>` contexts lift the returned `ProviderError`
/// into the top-level [`crate::error::Error`] automatically via `?` (the
/// `#[from]` impl on `enum@Error`'s `Provider` variant).
pub fn map_kernel_error(error: KernelError, model: &str) -> ProviderError {
    match error {
        KernelError::Timeout(_) => ProviderError::Timeout,
        KernelError::LlmApi(ref msg) if msg.contains("timed out") => ProviderError::Timeout,
        KernelError::Http { status: 404, .. } => ProviderError::ModelNotFound {
            model: model.to_string(),
        },
        other => ProviderError::Ollama(other.to_string()),
    }
}

/// The text of a reply that the model finished, or [`ProviderError::CutOff`]
/// when it stopped at its output limit (`finish_reason` is `length`).
///
/// A cut-off reply must not be used: repairing it would apply half an edit,
/// or write a file that ends in the middle.
pub fn complete_text(response: llm_kernel::llm::LLMResponse) -> Result<String, ProviderError> {
    match response.finish_reason.as_deref() {
        Some("length" | "max_tokens") => Err(ProviderError::CutOff {
            text: response.content,
        }),
        _ => Ok(response.content),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn map_kernel_error_timeout_maps_to_timeout() {
        let err = map_kernel_error(KernelError::Timeout(120), "m");
        assert!(matches!(err, ProviderError::Timeout));
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
            ProviderError::ModelNotFound { model } => assert_eq!(model, "qwen2.5-coder:7b"),
            other => panic!("expected ModelNotFound, got {other:?}"),
        }
    }

    #[test]
    fn map_kernel_error_other_maps_to_ollama() {
        let err = map_kernel_error(KernelError::LlmApi("boom".into()), "m");
        match err {
            ProviderError::Ollama(msg) => assert!(msg.contains("boom"), "{msg}"),
            other => panic!("expected Ollama, got {other:?}"),
        }
    }

    #[test]
    fn complete_text_rejects_a_reply_that_stopped_at_the_output_limit() {
        let reply = |finish_reason: Option<&str>| llm_kernel::llm::LLMResponse {
            content: "half a rep".into(),
            finish_reason: finish_reason.map(str::to_string),
            ..Default::default()
        };
        assert_eq!(
            complete_text(reply(Some("stop"))).expect("stop"),
            "half a rep"
        );
        assert_eq!(complete_text(reply(None)).expect("none"), "half a rep");
        for reason in ["length", "max_tokens"] {
            let error = complete_text(reply(Some(reason))).expect_err(reason);
            assert!(
                matches!(&error, ProviderError::CutOff { text } if text == "half a rep"),
                "{error:?}"
            );
            assert!(error.to_string().contains("cut off"), "{error}");
            assert!(!error.to_string().contains("half a rep"), "{error}");
        }
    }
}
