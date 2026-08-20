//! Injectable text generation built on `llm-kernel`'s `LLMClient` trait.
//!
//! Production uses [`crate::ollama::OllamaClient`] (an `LLMClient` impl that
//! talks to a local Ollama origin and unloads the model after each call).
//! Tests use [`ScriptedGenerator`], another `LLMClient` impl that returns
//! queued replies.

use crate::adapters::kernel_error::map_kernel_error;
use crate::error::Error;
#[cfg(test)]
use llm_kernel::error::KernelError;
pub use llm_kernel::llm::LLMClient;
use llm_kernel::llm::{ChatMessage, LLMRequest, LLMResponse};
#[cfg(test)]
use std::collections::VecDeque;
#[cfg(test)]
use std::sync::Mutex;

/// Default sampling temperature for PM, Tech Lead, Dev, and QA.
pub const ROLE_TEMPERATURE: f32 = 0.2;

/// Sampling temperature for the Writer article draft.
pub const WRITER_TEMPERATURE: f32 = 0.5;

/// Origin label for operator-facing run logs, when the client can report one.
///
/// Implemented by [`crate::ollama::OllamaClient`] (redacted Ollama URL) and
/// trivially by any test double that wants to surface an origin line.
pub trait Origin: LLMClient {
    /// Redacted origin for run-log context, or `None` to omit the line.
    fn redacted_origin(&self) -> Option<String> {
        None
    }
}

/// Run a single-prompt completion against `client` and return the text.
///
/// This is the shared call-site helper used by every stage: it builds an
/// [`LLMRequest`] from a user prompt + temperature, calls
/// [`LLMClient::complete`], and unwraps the response content. Kernel errors
/// are mapped via [`map_kernel_error`] with the requested model tag.
pub async fn complete_text(
    client: &dyn LLMClient,
    model: &str,
    prompt: &str,
    temperature: f32,
) -> Result<String, Error> {
    let request = LLMRequest {
        model: Some(model.to_string()),
        messages: vec![ChatMessage::user(prompt)],
        temperature,
        ..LLMRequest::default()
    };
    let response: LLMResponse = client
        .complete(request)
        .await
        .map_err(|e| map_kernel_error(e, model))?;
    Ok(response.content)
}

/// One recorded [`complete_text`] invocation, captured by [`ScriptedGenerator`].
#[cfg(test)]
#[derive(Debug, Clone, PartialEq)]
pub struct GenerateCall {
    /// Model tag passed to the call.
    pub model: String,
    /// Prompt text.
    pub prompt: String,
    /// Sampling temperature.
    pub temperature: f32,
}

/// Test double for any `LLMClient`: returns queued replies, records calls.
#[cfg(test)]
#[derive(Debug, Default)]
pub struct ScriptedGenerator {
    replies: Mutex<VecDeque<Result<String, Error>>>,
    calls: Mutex<Vec<GenerateCall>>,
}

#[cfg(test)]
impl ScriptedGenerator {
    /// Empty script.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Push a successful reply.
    pub fn push_text(&self, text: impl Into<String>) {
        self.replies
            .lock()
            .expect("script mutex")
            .push_back(Ok(text.into()));
    }

    /// Push a failed generate.
    pub fn push_err(&self, error: Error) {
        self.replies
            .lock()
            .expect("script mutex")
            .push_back(Err(error));
    }

    /// Recorded calls, in order.
    #[must_use]
    pub fn calls(&self) -> Vec<GenerateCall> {
        self.calls.lock().expect("script mutex").clone()
    }
}

#[cfg(test)]
impl Origin for ScriptedGenerator {}

#[cfg(test)]
#[async_trait::async_trait]
impl LLMClient for ScriptedGenerator {
    async fn complete(&self, request: LLMRequest) -> std::result::Result<LLMResponse, KernelError> {
        let model = request
            .model
            .clone()
            .unwrap_or_else(|| "scripted".to_string());
        let prompt = request
            .messages
            .first()
            .map(|m| m.text_content())
            .unwrap_or_default();
        let temperature = request.temperature;
        self.calls.lock().expect("script mutex").push(GenerateCall {
            model: model.clone(),
            prompt: prompt.clone(),
            temperature,
        });
        let result = self
            .replies
            .lock()
            .expect("script mutex")
            .pop_front()
            .unwrap_or_else(|| {
                Err(Error::Ollama(
                    "ScriptedGenerator: no remaining replies".into(),
                ))
            });
        result
            .map(|content| LLMResponse {
                content,
                ..LLMResponse::default()
            })
            .map_err(|e| match e {
                Error::Timeout => KernelError::Timeout(0),
                Error::ModelNotFound { model } => KernelError::Http {
                    status: 404,
                    message: model,
                },
                other => KernelError::LlmApi(other.to_string()),
            })
    }

    fn model_name(&self) -> &str {
        "scripted"
    }

    async fn stream_complete(
        &self,
        _request: LLMRequest,
    ) -> std::result::Result<llm_kernel::llm::LLMStream, KernelError> {
        Err(KernelError::Config(
            "ScriptedGenerator does not support streaming".into(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn scripted_returns_queued_text_and_records_call() {
        let gen = ScriptedGenerator::new();
        gen.push_text("hello");
        let text = complete_text(&gen, "llama3.1:8b", "goal", ROLE_TEMPERATURE)
            .await
            .expect("reply");
        assert_eq!(text, "hello");
        let calls = gen.calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].model, "llama3.1:8b");
        assert_eq!(calls[0].prompt, "goal");
        assert!((calls[0].temperature - ROLE_TEMPERATURE).abs() < f32::EPSILON);
    }

    #[tokio::test]
    async fn scripted_push_err_is_returned() {
        let gen = ScriptedGenerator::new();
        gen.push_err(Error::Timeout);
        let err = complete_text(&gen, "m", "p", ROLE_TEMPERATURE)
            .await
            .expect_err("scripted error");
        assert!(matches!(err, Error::Timeout));
    }
}
