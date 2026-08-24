//! Ollama [`ModelClient`] adapter preserving `keep_alive: 0` unload.

use crate::adapters::kernel_error::map_kernel_error;
use crate::adapters::ollama::OllamaClient;
use crate::error::Error;
use crate::generate::Origin;
use crate::ports::{GenerateRequest, ModelClient};
use async_trait::async_trait;
use llm_kernel::llm::LLMRequest;
use std::time::Duration;

use super::openai::to_llm_request;

/// [`ModelClient`] adapter over [`OllamaClient`], preserving its
/// `keep_alive: 0` VRAM-unload behavior.
///
/// Each [`generate`](ModelClient::generate) call delegates to
/// [`OllamaClient::complete`], which sends the completion through Ollama's
/// OpenAI-compatible endpoint and then posts a best-effort `keep_alive: 0`
/// unload to the native `/api/generate` endpoint so the model is released from
/// VRAM between stages. When `think` is set, the `:think` tag suffix is
/// appended to the requested model (see the module docs).
pub struct OllamaAdapter {
    /// The underlying Ollama client (owns the unload behavior).
    inner: OllamaClient,
    /// Whether the `:think` model-tag suffix should be applied.
    think: bool,
    /// Overrides the request temperature when set.
    temperature: Option<f32>,
    /// Requested max output tokens.
    max_tokens: Option<u32>,
    /// Per-completion timeout used when the request does not set its own.
    timeout: Duration,
}

impl OllamaAdapter {
    /// Adapter over `inner` with no `:think` suffix and no per-role options.
    #[must_use]
    pub fn new(inner: OllamaClient, think: bool) -> Self {
        Self::with_options(inner, think, None, None)
    }

    /// Adapter over `inner` with an explicit think flag and per-role options.
    #[must_use]
    pub fn with_options(
        inner: OllamaClient,
        think: bool,
        temperature: Option<f32>,
        max_tokens: Option<u32>,
    ) -> Self {
        let timeout = inner.timeout();
        Self {
            inner,
            think,
            temperature,
            max_tokens,
            timeout,
        }
    }
}

#[async_trait]
impl ModelClient for OllamaAdapter {
    async fn generate(&self, request: &GenerateRequest) -> Result<String, Error> {
        let timeout = request.timeout.unwrap_or(self.timeout);
        let model = resolve_ollama_model(&request.model, self.think);
        let llm_request = LLMRequest {
            model: Some(model),
            ..to_llm_request(request, self.temperature, self.max_tokens)
        };
        let response = self
            .inner
            .complete_with_timeout(llm_request, timeout)
            .await
            .map_err(|error| map_kernel_error(error, &request.model))?;
        Ok(response.content)
    }

    fn redacted_origin(&self) -> Option<String> {
        Origin::redacted_origin(&self.inner)
    }
}

/// Apply Ollama's `:think` model-tag suffix when `think` is enabled.
///
/// The suffix is only appended when the tag does not already carry it, so a
/// configured `model = "qwen3:think"` stays untouched.
#[must_use]
fn resolve_ollama_model(model: &str, think: bool) -> String {
    if think && !model.ends_with(":think") {
        format!("{model}:think")
    } else {
        model.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::providers::test_support::{chat_response, mount_chat, request};
    use serde_json::Value;
    use wiremock::matchers::{body_partial_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn ollama_generate_returns_content_and_unloads_keep_alive_zero() {
        let server = MockServer::start().await;
        mount_chat(&server, &chat_response("hello")).await;
        Mock::given(method("POST"))
            .and(path("/api/generate"))
            .and(body_partial_json(serde_json::json!({"keep_alive": 0})))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;

        let inner = OllamaClient::new(server.uri()).expect("client");
        let client = OllamaAdapter::new(inner, false);
        let text = client.generate(&request("phi4")).await.expect("reply");
        assert_eq!(text, "hello");

        let generate_requests: Vec<_> = server
            .received_requests()
            .await
            .expect("requests")
            .into_iter()
            .filter(|r| r.url.path() == "/api/generate")
            .collect();
        assert_eq!(generate_requests.len(), 1, "expected one unload request");
        let body: Value =
            serde_json::from_slice(&generate_requests[0].body).expect("unload body JSON");
        assert_eq!(body["keep_alive"], 0);
        assert_eq!(body["model"], "phi4");
    }

    #[tokio::test]
    async fn ollama_generate_applies_think_suffix() {
        let server = MockServer::start().await;
        mount_chat(&server, &chat_response("ok")).await;
        Mock::given(method("POST"))
            .and(path("/api/generate"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;

        let inner = OllamaClient::new(server.uri()).expect("client");
        let client = OllamaAdapter::new(inner, true);
        client.generate(&request("phi4")).await.expect("reply");

        let chat_requests: Vec<_> = server
            .received_requests()
            .await
            .expect("requests")
            .into_iter()
            .filter(|r| r.url.path() == "/v1/chat/completions")
            .collect();
        assert_eq!(chat_requests.len(), 1);
        let body: Value = serde_json::from_slice(&chat_requests[0].body).expect("body");
        assert_eq!(body["model"], "phi4:think");
    }

    #[tokio::test]
    async fn ollama_generate_without_think_keeps_model_verbatim() {
        let server = MockServer::start().await;
        mount_chat(&server, &chat_response("ok")).await;
        Mock::given(method("POST"))
            .and(path("/api/generate"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;

        let inner = OllamaClient::new(server.uri()).expect("client");
        let client = OllamaAdapter::with_options(inner, false, Some(0.3), Some(256));
        client
            .generate(&request("qwen2.5-coder:7b"))
            .await
            .expect("reply");

        let chat_requests: Vec<_> = server
            .received_requests()
            .await
            .expect("requests")
            .into_iter()
            .filter(|r| r.url.path() == "/v1/chat/completions")
            .collect();
        assert_eq!(chat_requests.len(), 1);
        let body: Value = serde_json::from_slice(&chat_requests[0].body).expect("body");
        assert_eq!(body["model"], "qwen2.5-coder:7b");
        assert_eq!(body["temperature"], 0.3);
        assert_eq!(body["max_tokens"], 256);
    }

    #[tokio::test]
    async fn ollama_redacted_origin_delegates() {
        let client = OllamaAdapter::new(
            OllamaClient::new("http://127.0.0.1:11434").expect("client"),
            false,
        );
        assert_eq!(
            client.redacted_origin().as_deref(),
            Some("http://127.0.0.1:11434")
        );
    }

    #[tokio::test]
    async fn ollama_generate_uses_request_timeout_override() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_delay(Duration::from_millis(200))
                    .set_body_raw(chat_response("late").as_bytes(), "application/json"),
            )
            .mount(&server)
            .await;

        let inner =
            OllamaClient::with_timeout(server.uri(), Duration::from_secs(60)).expect("client");
        let client = OllamaAdapter::new(inner, false);
        let mut request = request("m");
        request.timeout = Some(Duration::from_millis(40));
        let err = client.generate(&request).await.expect_err("timeout");
        match err {
            Error::Provider(crate::error::ProviderError::Timeout) => {}
            other => panic!("expected Timeout, got {other:?}"),
        }
    }
}
