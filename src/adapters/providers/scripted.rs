//! Test double for `LLMClient`: returns queued replies and records calls.
//!
//! `ScriptedGenerator` is the in-process test double used by every legacy
//! stage module (`pm`, `techlead`, `qa`, `writer`, `dev`, `pipeline`) and by
//! the role-graph harness tests. It implements [`LLMClient`] and
//! [`Origin`](crate::generate::Origin) so it can be injected anywhere a real
//! provider client would go, without a network or a live Ollama instance.
//!
//! Moved here from `src/generate.rs` (issue #83) so the LLM-client test double
//! lives in the adapter layer alongside the real provider adapters, keeping
//! `generate.rs` focused on the shared `complete_text` call-site helper.

use crate::error::Error;
use crate::generate::Origin;
use llm_kernel::error::KernelError;
use llm_kernel::llm::{LLMClient, LLMRequest, LLMResponse};
use std::collections::VecDeque;
use std::sync::Mutex;

/// One recorded [`complete_text`](crate::generate::complete_text) invocation,
/// captured by [`ScriptedGenerator`].
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
///
/// Push replies with [`push_text`](Self::push_text) or
/// [`push_err`](Self::push_err); inspect recorded calls with
/// [`calls`](Self::calls). When the queue is exhausted the next
/// [`LLMClient::complete`] returns an `Ollama` error with the message
/// `"ScriptedGenerator: no remaining replies"`.
#[derive(Debug, Default)]
pub struct ScriptedGenerator {
    replies: Mutex<VecDeque<Result<String, Error>>>,
    calls: Mutex<Vec<GenerateCall>>,
}

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

impl Origin for ScriptedGenerator {}

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
    use crate::generate::{complete_text, ROLE_TEMPERATURE};

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

    #[tokio::test]
    async fn scripted_exhausted_replies_returns_ollama_error() {
        let gen = ScriptedGenerator::new();
        let err = gen
            .complete(LLMRequest::default())
            .await
            .expect_err("exhausted");
        match err {
            KernelError::LlmApi(msg) => {
                assert!(msg.contains("no remaining replies"), "{msg}")
            }
            other => panic!("expected LlmApi, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn scripted_stream_complete_is_unsupported() {
        let gen = ScriptedGenerator::new();
        let result = gen.stream_complete(LLMRequest::default()).await;
        match result {
            Err(KernelError::Config(msg)) => assert!(msg.contains("streaming"), "{msg}"),
            Err(other) => panic!("expected Config, got {other:?}"),
            Ok(_) => panic!("stream should error"),
        }
    }

    #[test]
    fn scripted_model_name_is_scripted() {
        let gen = ScriptedGenerator::new();
        assert_eq!(gen.model_name(), "scripted");
    }

    #[tokio::test]
    async fn scripted_records_temperature_from_request() {
        let gen = ScriptedGenerator::new();
        gen.push_text("ok");
        let request = LLMRequest {
            model: Some("m".into()),
            temperature: 0.7,
            ..LLMRequest::default()
        };
        let _ = gen.complete(request).await.expect("reply");
        let calls = gen.calls();
        assert_eq!(calls.len(), 1);
        assert!((calls[0].temperature - 0.7).abs() < f32::EPSILON);
    }

    #[tokio::test]
    async fn scripted_maps_model_not_found_to_kernel_http_404() {
        let gen = ScriptedGenerator::new();
        gen.push_err(Error::ModelNotFound {
            model: "nope".into(),
        });
        let err = gen
            .complete(LLMRequest::default())
            .await
            .expect_err("model not found");
        match err {
            KernelError::Http { status, message } => {
                assert_eq!(status, 404);
                assert_eq!(message, "nope");
            }
            other => panic!("expected Http 404, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn scripted_maps_other_error_to_llm_api() {
        let gen = ScriptedGenerator::new();
        gen.push_err(Error::Artifact("boom".into()));
        let err = gen
            .complete(LLMRequest::default())
            .await
            .expect_err("artifact");
        match err {
            KernelError::LlmApi(msg) => assert!(msg.contains("boom"), "{msg}"),
            other => panic!("expected LlmApi, got {other:?}"),
        }
    }

    #[test]
    fn scripted_default_origin_is_none() {
        let gen = ScriptedGenerator::new();
        assert!(gen.redacted_origin().is_none());
    }
}
