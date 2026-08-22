//! Ollama client adapter.
//!
//! Sequential Ollama client backed by `llm-kernel`'s `OpenAIClient`.
//! Completions go through Ollama's OpenAI-compatible `/v1/chat/completions`
//! endpoint (via `llm_kernel::llm::OpenAIClient`). After each call, an
//! unload request is sent to Ollama's native `/api/generate` endpoint with
//! `keep_alive: 0` so the model is released from VRAM between stages —
//! preserving the memory behavior of the original hand-rolled client.

use crate::error::{Error, ProviderError};
use crate::generate::Origin;
use async_trait::async_trait;
use llm_kernel::error::KernelError;
use llm_kernel::llm::{LLMClient, LLMRequest, LLMResponse, OpenAIClient};
use serde::Serialize;
use std::time::Duration;
use tokio::sync::Mutex;

/// Default generate timeout (matches the original hand-rolled client).
pub const DEFAULT_GENERATE_TIMEOUT: Duration = Duration::from_secs(600);

/// Timeout for the best-effort `keep_alive: 0` unload request.
///
/// Kept short so a hanging Ollama server cannot block the serialization lock
/// (and thus all subsequent stages) for an extended period.
const UNLOAD_TIMEOUT: Duration = Duration::from_secs(30);

/// Validate an Ollama HTTP(S) origin and return its normalized form.
pub fn validate_ollama_url(value: &str) -> Result<String, Error> {
    let value = value.trim();
    let mut parsed = reqwest::Url::parse(value).map_err(|_| {
        Error::from(ProviderError::InvalidOllamaUrl {
            url: redact_ollama_url(value),
        })
    })?;
    if !matches!(parsed.scheme(), "http" | "https")
        || parsed.host_str().is_none()
        || !matches!(parsed.path(), "" | "/")
        || parsed.query().is_some()
        || parsed.fragment().is_some()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
    {
        return Err(Error::from(ProviderError::InvalidOllamaUrl {
            url: redact_ollama_url(value),
        }));
    }
    parsed.set_path("");
    Ok(parsed.to_string().trim_end_matches('/').to_owned())
}

/// Redact userinfo credentials from an Ollama URL before reporting it.
///
/// Both the username and password are stripped so that neither credential
/// form (username-only tokens or username/password pairs) leaks into
/// operator-facing logs or error messages. Only the scheme, host, and port
/// are preserved.
#[must_use]
pub fn redact_ollama_url(value: &str) -> String {
    let Ok(mut parsed) = reqwest::Url::parse(value) else {
        return redact_unparsed_url(value);
    };
    // Clear the entire userinfo so username-only and username-password
    // credentials are both removed before the URL is reported.
    if parsed.set_username("").is_err() || parsed.set_password(None).is_err() {
        return redact_unparsed_url(value);
    }
    parsed.set_query(None);
    parsed.set_fragment(None);
    let result = parsed.to_string().trim_end_matches('/').to_owned();
    // If the parsed URL still contains @, the original value may have
    // credentials in an unexpected position (e.g. a non-http scheme where
    // Url::parse treats userinfo as part of the path). Fall back to the
    // conservative unparsed redactor so credentials never leak.
    if result.contains('@') {
        redact_unparsed_url(value)
    } else {
        result
    }
}

fn redact_unparsed_url(value: &str) -> String {
    let end = value.find(['?', '#']).unwrap_or(value.len());
    let value = &value[..end];
    let authority_start = match value.find("://") {
        Some(index) => index + 3,
        None => {
            // No scheme separator; conservatively redact anything before
            // the first @ (up to the first /) so malformed URLs without a
            // scheme cannot leak userinfo-like credentials.
            let slash = value.find('/').unwrap_or(value.len());
            return match value[..slash].rfind('@') {
                Some(at) => format!("[REDACTED]@{}", &value[at + 1..]),
                None => value.to_owned(),
            };
        }
    };
    let authority_end = value[authority_start..]
        .find('/')
        .map(|index| authority_start + index)
        .unwrap_or(value.len());
    let authority = &value[authority_start..authority_end];
    let Some(userinfo_end) = authority.rfind('@') else {
        return value.to_owned();
    };
    let userinfo_end = authority_start + userinfo_end;
    format!(
        "{}[REDACTED]@{}",
        &value[..authority_start],
        &value[userinfo_end + 1..]
    )
}

/// HTTP client that talks to one Ollama origin via `llm-kernel`'s
/// `OpenAIClient`, unloading the model after each completion.
///
/// Completions are serialized by an internal mutex (matching the original
/// hand-rolled client's behavior) so two `complete` calls on a shared
/// `OllamaClient` never overlap.
pub struct OllamaClient {
    /// `llm-kernel` OpenAI-compatible client pointed at `{origin}/v1`.
    /// The model is set per-request via `LLMRequest::model`, so the client's
    /// own model name is a placeholder.
    inner: OpenAIClient,
    /// `reqwest::Client` used for the post-completion unload call.
    unload_client: reqwest::Client,
    /// Normalized origin (no trailing slash), e.g. `http://127.0.0.1:11434`.
    base_url: String,
    /// Redacted origin for run-log context.
    redacted_base_url: String,
    /// Serializes completions (preserves the original client's behavior).
    generate_lock: Mutex<()>,
    /// Per-completion timeout (used for `tokio::time::timeout` wrapping).
    timeout: Duration,
}

#[derive(Serialize)]
struct UnloadRequest<'a> {
    model: &'a str,
    prompt: &'a str,
    stream: bool,
    keep_alive: i32,
}

impl OllamaClient {
    /// Client for `base_url` with the default 10 minute generate timeout.
    pub fn new(base_url: impl Into<String>) -> Result<Self, Error> {
        Self::with_timeout(base_url, DEFAULT_GENERATE_TIMEOUT)
    }

    /// Client with an explicit request timeout (used in tests).
    pub fn with_timeout(base_url: impl Into<String>, timeout: Duration) -> Result<Self, Error> {
        let base_url = validate_ollama_url(&base_url.into())?;
        let redacted_base_url = redact_ollama_url(&base_url);
        // Ollama's OpenAI-compatible endpoint lives at `{origin}/v1`.
        let chat_base_url = format!("{}/v1", base_url);
        // Ollama needs no API key; pass a non-empty placeholder so the
        // OpenAIClient's `Authorization: Bearer <key>` header is well-formed
        // (Ollama ignores it). No reqwest timeout — we wrap completions with
        // `tokio::time::timeout` in `complete` so timeouts map to
        // `KernelError::Timeout` (the kernel's OpenAIClient would otherwise
        // surface reqwest timeouts as `LlmApi`, losing the structured info).
        let http_client = reqwest::Client::builder()
            .no_proxy()
            .build()
            .map_err(|error| Error::from(ProviderError::Ollama(error.to_string())))?;
        let inner = OpenAIClient::from_key_with_base_url(
            "ollama",
            "ollama",
            chat_base_url,
            http_client.clone(),
        );
        Ok(Self {
            inner,
            unload_client: http_client,
            base_url,
            redacted_base_url,
            generate_lock: Mutex::new(()),
            timeout,
        })
    }

    /// Redacted origin used for operator-facing run logs.
    #[must_use]
    pub fn redacted_origin(&self) -> &str {
        &self.redacted_base_url
    }

    /// Send Ollama a `keep_alive: 0` unload request for `model`.
    ///
    /// Errors are best-effort: a failed unload does not fail the pipeline,
    /// since the completion already succeeded. The request is bounded by
    /// [`UNLOAD_TIMEOUT`] so a hanging Ollama server cannot block the
    /// serialization lock (and thus all subsequent stages) indefinitely.
    async fn unload(&self, model: &str) -> Result<(), Error> {
        let url = format!("{}/api/generate", self.base_url.trim_end_matches('/'));
        let send = self
            .unload_client
            .post(url)
            .json(&UnloadRequest {
                model,
                prompt: "",
                stream: false,
                keep_alive: 0,
            })
            .send();
        let response = tokio::time::timeout(UNLOAD_TIMEOUT, send)
            .await
            .map_err(|_| {
                Error::from(ProviderError::Ollama(format!(
                    "unload timed out after {}s",
                    UNLOAD_TIMEOUT.as_secs()
                )))
            })?
            .map_err(|e| Error::from(ProviderError::Ollama(e.to_string())))?;
        if !response.status().is_success() {
            return Err(Error::from(ProviderError::Ollama(format!(
                "unload status {}",
                response.status()
            ))));
        }
        Ok(())
    }
}

impl Origin for OllamaClient {
    fn redacted_origin(&self) -> Option<String> {
        Some(self.redacted_base_url.clone())
    }
}

#[async_trait]
impl LLMClient for OllamaClient {
    async fn complete(&self, request: LLMRequest) -> Result<LLMResponse, KernelError> {
        let _guard = self.generate_lock.lock().await;
        // The per-request model overrides the client's placeholder model.
        let model_for_unload = request
            .model
            .clone()
            .unwrap_or_else(|| self.inner.model_name().to_string());
        // Wrap with our own tokio timeout so we can reliably detect timeouts
        // (the kernel's OpenAIClient maps reqwest timeouts to LlmApi, losing
        // the structured timeout info).
        let response = tokio::time::timeout(self.timeout, self.inner.complete(request))
            .await
            .map_err(|_| KernelError::Timeout(self.timeout.as_secs()))??;
        // Best-effort unload: a failure here does not fail the completion.
        let _ = self.unload(&model_for_unload).await;
        Ok(response)
    }

    fn model_name(&self) -> &str {
        self.inner.model_name()
    }

    async fn stream_complete(
        &self,
        request: LLMRequest,
    ) -> Result<llm_kernel::llm::LLMStream, KernelError> {
        // Delegate directly. Streaming does NOT take `generate_lock`, so a
        // stream can overlap a `complete` call on a shared `OllamaClient`.
        // Streaming also does not unload (the model is still producing
        // tokens). Callers that need serialization or unload after a stream
        // should call `complete` instead.
        self.inner.stream_complete(request).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::kernel_error::map_kernel_error;
    use serde_json::Value;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;
    use wiremock::matchers::{body_partial_json, method, path};
    use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

    struct CountingDelay {
        in_flight: Arc<AtomicU32>,
        max: Arc<AtomicU32>,
    }

    impl Respond for CountingDelay {
        fn respond(&self, _request: &Request) -> ResponseTemplate {
            let current = self.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
            self.max.fetch_max(current, Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(80));
            self.in_flight.fetch_sub(1, Ordering::SeqCst);
            ResponseTemplate::new(200).set_body_raw(
                r#"{"id":"chatcmpl-x","created":1,"model":"ollama","choices":[{"index":0,"message":{"role":"assistant","content":"ok"},"finish_reason":"stop"}],"usage":{"prompt_tokens":1,"completion_tokens":1,"total_tokens":2}}"#,
                "application/json",
            )
        }
    }

    /// Mount an OpenAI-compatible `/v1/chat/completions` 200 handler.
    async fn mount_chat(server: &MockServer, body: &str) {
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(
                ResponseTemplate::new(200).set_body_raw(body.to_string(), "application/json"),
            )
            .mount(server)
            .await;
    }

    /// Standard OpenAI chat completion response with the given content.
    fn chat_response(content: &str) -> String {
        format!(
            r#"{{"id":"chatcmpl-x","created":1,"model":"ollama","choices":[{{"index":0,"message":{{"role":"assistant","content":{}}},"finish_reason":"stop"}}],"usage":{{"prompt_tokens":1,"completion_tokens":1,"total_tokens":2}}}}"#,
            serde_json::to_string(content).expect("content JSON")
        )
    }

    #[tokio::test]
    async fn complete_returns_content_and_unloads_with_keep_alive_zero() {
        let server = MockServer::start().await;
        mount_chat(&server, &chat_response("hello")).await;
        // Capture the unload request body.
        Mock::given(method("POST"))
            .and(path("/api/generate"))
            .and(body_partial_json(serde_json::json!({"keep_alive": 0})))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;

        let client = OllamaClient::new(server.uri()).expect("client");
        let request = LLMRequest {
            model: Some("qwen2.5-coder:7b".to_string()),
            messages: vec![llm_kernel::llm::ChatMessage::user("say hi")],
            temperature: 0.2,
            ..LLMRequest::default()
        };
        let response = client.complete(request).await.expect("complete");
        assert_eq!(response.content, "hello");

        // Verify the unload request carried keep_alive: 0.
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
        assert_eq!(body["model"], "qwen2.5-coder:7b");
    }

    #[tokio::test]
    async fn complete_passes_temperature_through() {
        let server = MockServer::start().await;
        mount_chat(&server, &chat_response("ok")).await;
        Mock::given(method("POST"))
            .and(path("/api/generate"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;

        let client = OllamaClient::new(server.uri()).expect("client");
        let request = LLMRequest {
            model: Some("gemma2:9b".to_string()),
            messages: vec![llm_kernel::llm::ChatMessage::user("draft")],
            temperature: 0.5,
            ..LLMRequest::default()
        };
        client.complete(request).await.expect("complete");

        let chat_requests: Vec<_> = server
            .received_requests()
            .await
            .expect("requests")
            .into_iter()
            .filter(|r| r.url.path() == "/v1/chat/completions")
            .collect();
        assert_eq!(chat_requests.len(), 1);
        let body: Value = serde_json::from_slice(&chat_requests[0].body).expect("chat body JSON");
        assert_eq!(body["temperature"], 0.5);
    }

    #[tokio::test]
    async fn complete_maps_404_to_model_not_found() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(404).set_body_raw(
                r#"{"error":{"message":"model 'nope' not found"}}"#,
                "application/json",
            ))
            .mount(&server)
            .await;

        let client = OllamaClient::new(server.uri()).expect("client");
        let request = LLMRequest {
            model: Some("nope".to_string()),
            messages: vec![llm_kernel::llm::ChatMessage::user("x")],
            temperature: 0.2,
            ..LLMRequest::default()
        };
        let err = client.complete(request).await.expect_err("missing model");
        let mapped = map_kernel_error(err, "nope");
        match mapped {
            ProviderError::ModelNotFound { model } => assert_eq!(model, "nope"),
            other => panic!("expected ModelNotFound, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn complete_maps_non_404_http_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;

        let client = OllamaClient::new(server.uri()).expect("client");
        let request = LLMRequest {
            model: Some("m".to_string()),
            messages: vec![llm_kernel::llm::ChatMessage::user("p")],
            temperature: 0.2,
            ..LLMRequest::default()
        };
        let err = client.complete(request).await.expect_err("status");
        let mapped = map_kernel_error(err, "m");
        match mapped {
            ProviderError::Ollama(msg) => assert!(msg.contains("500"), "{msg}"),
            other => panic!("expected Ollama status error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn complete_maps_timeout() {
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

        let client =
            OllamaClient::with_timeout(server.uri(), Duration::from_millis(40)).expect("client");
        let request = LLMRequest {
            model: Some("m".to_string()),
            messages: vec![llm_kernel::llm::ChatMessage::user("p")],
            temperature: 0.2,
            ..LLMRequest::default()
        };
        let err = client.complete(request).await.expect_err("timeout");
        let mapped = map_kernel_error(err, "m");
        match mapped {
            ProviderError::Timeout => {}
            other => panic!("expected Timeout, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn complete_is_serialized() {
        let server = MockServer::start().await;
        let in_flight = Arc::new(AtomicU32::new(0));
        let max = Arc::new(AtomicU32::new(0));
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(CountingDelay {
                in_flight: Arc::clone(&in_flight),
                max: Arc::clone(&max),
            })
            .expect(2)
            .mount(&server)
            .await;
        // Unload requests can be 404s — they're best-effort.
        Mock::given(method("POST"))
            .and(path("/api/generate"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;

        let client = OllamaClient::new(server.uri()).expect("client");
        let req = || LLMRequest {
            model: Some("m".to_string()),
            messages: vec![llm_kernel::llm::ChatMessage::user("p")],
            temperature: 0.2,
            ..LLMRequest::default()
        };
        let first = client.complete(req());
        let second = client.complete(req());
        let (a, b) = tokio::join!(first, second);
        a.expect("first");
        b.expect("second");
        assert_eq!(
            max.load(Ordering::SeqCst),
            1,
            "two completes must not overlap"
        );
    }

    #[test]
    fn rejects_invalid_ollama_url() {
        let error = match OllamaClient::new("not a URL") {
            Ok(_) => panic!("invalid URL must fail"),
            Err(error) => error,
        };
        let text = error.to_string();
        assert!(text.contains("not a URL"), "{text}");
        assert!(text.contains("http://"), "{text}");
    }

    #[test]
    fn rejects_non_origin_ollama_urls() {
        for value in [
            "http://example.test/path",
            "http://example.test?token=secret",
            "http://example.test#fragment",
            "http://user:secret@example.test",
            "http://api-token@example.test",
        ] {
            assert!(
                validate_ollama_url(value).is_err(),
                "expected origin rejection for {value}"
            );
        }
    }

    #[test]
    fn rejects_userinfo_without_leaking_credentials() {
        let error =
            validate_ollama_url("http://user:secret@example.test").expect_err("must reject");
        let text = error.to_string();
        assert!(!text.contains("secret"), "{text}");
        assert!(!text.contains("user"), "{text}");
        // The redacted URL must not contain the credentials.
        assert!(text.contains("example.test"), "{text}");
    }

    #[test]
    fn normalizes_trailing_slash() {
        assert_eq!(
            validate_ollama_url("http://example.test:11434/").expect("valid origin"),
            "http://example.test:11434"
        );
    }

    #[test]
    fn redacts_ollama_userinfo_username_and_password() {
        let redacted = redact_ollama_url("http://user:secret@example.test:11434");
        assert!(!redacted.contains("user"), "{redacted}");
        assert!(!redacted.contains("secret"), "{redacted}");
        assert!(!redacted.contains('@'), "{redacted}");
        assert_eq!(redacted, "http://example.test:11434");
    }

    #[test]
    fn redacts_ollama_userinfo_username_only() {
        let redacted = redact_ollama_url("http://api-token@example.test:11434");
        assert!(!redacted.contains("api-token"), "{redacted}");
        assert!(!redacted.contains('@'), "{redacted}");
        assert_eq!(redacted, "http://example.test:11434");
    }

    #[test]
    fn redacts_ollama_userinfo_preserves_scheme_host_port() {
        let redacted = redact_ollama_url("https://token:pass@host.local:8080");
        assert_eq!(redacted, "https://host.local:8080");
    }

    #[test]
    fn redacts_ollama_userinfo_no_credentials() {
        assert_eq!(
            redact_ollama_url("http://example.test:11434"),
            "http://example.test:11434"
        );
    }

    #[test]
    fn redacts_malformed_credentials() {
        let error = match OllamaClient::new("http://user:secret@[invalid") {
            Ok(_) => panic!("invalid URL must fail"),
            Err(error) => error,
        };
        let text = error.to_string();
        assert!(!text.contains("secret"), "{text}");
        assert!(text.contains("[REDACTED]"), "{text}");
    }

    #[test]
    fn redacts_unparsed_url_without_scheme() {
        // A malformed URL without :// must still redact userinfo-like content
        // so credentials never leak into error messages.
        let redacted = redact_ollama_url("user:secret@host");
        assert!(!redacted.contains("secret"), "{redacted}");
        assert!(redacted.contains("[REDACTED]"), "{redacted}");
    }

    #[tokio::test]
    async fn error_path_does_not_leak_credentials() {
        // Since validate_ollama_url rejects userinfo, credentials can never
        // reach the stored base_url. This test verifies the error path
        // defensively: a failed request's error message must not contain
        // any credential-like content even if the URL were to leak.
        let client = OllamaClient::with_timeout("http://127.0.0.1:1", Duration::from_millis(100))
            .expect("valid origin");
        let request = LLMRequest {
            model: Some("m".to_string()),
            messages: vec![llm_kernel::llm::ChatMessage::user("p")],
            temperature: 0.2,
            ..LLMRequest::default()
        };
        let error = client.complete(request).await.expect_err("must fail");
        let text = map_kernel_error(error, "m").to_string();
        // The error may contain the URL (reqwest formats it), but it must
        // never contain credential separators since validation strips them.
        assert!(
            !text.contains("@"),
            "error must not contain userinfo: {text}"
        );
    }
}
