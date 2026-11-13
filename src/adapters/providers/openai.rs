//! OpenAI-compatible [`ModelClient`] adapter (Deepseek, Kimi, custom).

use crate::adapters::kernel_error::{complete_text, map_kernel_error, usage};
use crate::error::{ConfigError, Error, ProviderError};
use crate::ports::{GenerateRequest, ModelClient};
use async_trait::async_trait;
use llm_kernel::llm::{ChatMessage, LLMClient, LLMRequest, OpenAIClient};
use std::time::Duration;

/// Map a port request to an [`LLMRequest`] for the OpenAI-compatible path.
pub(crate) fn to_llm_request(
    request: &GenerateRequest,
    temperature: Option<f32>,
    max_tokens: Option<u32>,
) -> LLMRequest {
    LLMRequest {
        model: Some(request.model.clone()),
        system: request.system.clone(),
        messages: vec![ChatMessage::user(request.prompt.clone())],
        temperature: temperature.unwrap_or(request.temperature),
        max_tokens,
        ..LLMRequest::default()
    }
}

/// Validate an OpenAI-compatible base URL for the remote adapters.
///
/// Accepts `http://` / `https://` URLs with a host (a path such as `/v1` is
/// allowed) and rejects userinfo so credentials can never be embedded in the
/// URL, a query or fragment (they would break the appended endpoint paths), and
/// plain `http://` to a non-loopback host (the API key would travel in
/// cleartext). Rejected values are reported redacted.
///
/// `http://` is allowed for local OpenAI-compatible proxies (vLLM, Ollama);
/// for remote keyed providers (Deepseek/Kimi/custom) prefer `https://` so the
/// API key is never sent in cleartext.
pub(crate) fn validate_chat_base_url(value: &str) -> Result<String, Error> {
    let value = value.trim();
    // Scheme, host and port only: a path segment could carry a token, and these
    // errors reach the run log and the doctor report.
    let redacted = reqwest::Url::parse(value)
        .ok()
        .filter(|url| url.has_host())
        .map(|url| url.origin().ascii_serialization())
        .unwrap_or_else(|| crate::adapters::ollama::redact_ollama_url(value));
    let parsed = reqwest::Url::parse(value).map_err(|_| {
        Error::from(ConfigError {
            path: "provider base_url".into(),
            message: format!(
                "invalid base URL {redacted:?}: expected an http:// or https:// URL with a host"
            ),
        })
    })?;
    if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
        return Err(Error::from(ConfigError {
            path: "provider base_url".into(),
            message: format!(
                "invalid base URL {redacted:?}: expected an http:// or https:// URL with a host"
            ),
        }));
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(Error::from(ConfigError {
            path: "provider base_url".into(),
            message: format!(
                "base URL {redacted:?} must not embed credentials; use the API key header instead"
            ),
        }));
    }
    // Plain http is only for this machine, so an API key never crosses a network
    // in cleartext.
    if parsed.scheme() == "http" && !is_loopback(&parsed) {
        return Err(Error::from(ConfigError {
            path: "provider base_url".into(),
            message: format!(
                "base URL {redacted:?} uses http:// on a non-loopback host; use https://"
            ),
        }));
    }
    // Endpoints are built by appending (`/chat/completions`, `/models`), which a
    // query or fragment would swallow, so the request would go to the wrong path.
    if parsed.query().is_some() || parsed.fragment().is_some() {
        return Err(Error::from(ConfigError {
            path: "provider base_url".into(),
            message: format!("base URL {redacted:?} must not have a query string or fragment"),
        }));
    }
    Ok(parsed.to_string().trim_end_matches('/').to_owned())
}

/// `localhost`, `127.0.0.0/8` or `::1`.
fn is_loopback(url: &reqwest::Url) -> bool {
    match url.host_str() {
        Some("localhost") => true,
        Some(host) => host
            .trim_start_matches('[')
            .trim_end_matches(']')
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback()),
        None => false,
    }
}

/// True when `value` parses as a URL whose host is this machine.
pub(crate) fn is_loopback_base_url(value: &str) -> bool {
    reqwest::Url::parse(value.trim()).is_ok_and(|url| is_loopback(&url))
}

/// OpenAI-compatible [`ModelClient`] used for Deepseek, Kimi, and any custom
/// `openai-compatible` provider.
///
/// Wraps [`OpenAIClient`] and maps [`GenerateRequest`] to an [`LLMRequest`]
/// (model, system, a single user message, temperature, optional max tokens).
/// Kernel errors are mapped via [`map_kernel_error`]. Completions are bounded
/// by `tokio::time::timeout` so a hanging provider surfaces as
/// [`ProviderError::Timeout`] (the kernel's own client would otherwise surface reqwest
/// timeouts as `LlmApi`, losing the structured info).
pub struct OpenAICompatibleAdapter {
    /// `llm-kernel` OpenAI-compatible client; the model is set per-request.
    inner: OpenAIClient,
    /// Redacted base URL for run-log context.
    redacted_base_url: String,
    /// Overrides the request temperature when set.
    temperature: Option<f32>,
    /// Requested max output tokens.
    max_tokens: Option<u32>,
    /// Per-completion timeout (used for `tokio::time::timeout` wrapping).
    timeout: Duration,
}

impl OpenAICompatibleAdapter {
    /// Client for `base_url` with the default generate timeout and no options.
    pub fn new(base_url: impl Into<String>, api_key: impl Into<String>) -> Result<Self, Error> {
        Self::with_timeout(
            base_url,
            api_key,
            crate::adapters::ollama::DEFAULT_GENERATE_TIMEOUT,
            None,
            None,
        )
    }

    /// Client with an explicit request timeout and per-role options.
    ///
    /// `temperature` overrides the request temperature when set; `max_tokens`
    /// caps the completion length. The base URL must be `http(s)://` with a
    /// host and no userinfo credentials.
    pub fn with_timeout(
        base_url: impl Into<String>,
        api_key: impl Into<String>,
        timeout: Duration,
        temperature: Option<f32>,
        max_tokens: Option<u32>,
    ) -> Result<Self, Error> {
        let base_url = validate_chat_base_url(&base_url.into())?;
        let redacted_base_url = crate::adapters::ollama::redact_ollama_url(&base_url);
        // The per-request model always overrides the placeholder below, so the
        // client-level model name is never sent to the provider.
        // No redirects: a redirect could move the bearer key to plain http or
        // another host, bypassing the base URL checks above.
        let http_client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|error| {
                Error::from(ConfigError {
                    path: "provider base_url".into(),
                    message: format!("failed to build HTTP client: {error}"),
                })
            })?;
        let inner = OpenAIClient::from_key_with_base_url("unset", api_key, base_url, http_client);
        Ok(Self {
            inner,
            redacted_base_url,
            temperature,
            max_tokens,
            timeout,
        })
    }
}

#[async_trait]
impl ModelClient for OpenAICompatibleAdapter {
    async fn generate(&self, request: &GenerateRequest) -> Result<String, Error> {
        Ok(self.generate_with_usage(request).await?.0)
    }

    async fn generate_with_usage(
        &self,
        request: &GenerateRequest,
    ) -> Result<(String, Option<crate::ports::Usage>), Error> {
        let timeout = request.timeout.unwrap_or(self.timeout);
        let response = tokio::time::timeout(
            timeout,
            self.inner
                .complete(to_llm_request(request, self.temperature, self.max_tokens)),
        )
        .await
        .map_err(|_| Error::from(ProviderError::Timeout))
        .and_then(|result| {
            result.map_err(|error| Error::from(map_kernel_error(error, &request.model)))
        })?;
        let usage = usage(&response);
        let text = complete_text(response)?;
        Ok((text, usage))
    }

    fn redacted_origin(&self) -> Option<String> {
        Some(self.redacted_base_url.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::providers::test_support::{chat_response, mount_chat, request};
    use serde_json::Value;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn openai_generate_returns_content_and_sends_request() {
        let server = MockServer::start().await;
        mount_chat(&server, &chat_response("hello")).await;

        let client = OpenAICompatibleAdapter::new(format!("{}/v1", server.uri()), "sk-test")
            .expect("client");
        let text = client
            .generate(&request("deepseek-v4-pro"))
            .await
            .expect("reply");
        assert_eq!(text, "hello");

        let chat_requests: Vec<_> = server
            .received_requests()
            .await
            .expect("requests")
            .into_iter()
            .filter(|r| r.url.path() == "/v1/chat/completions")
            .collect();
        assert_eq!(chat_requests.len(), 1);
        let body: Value = serde_json::from_slice(&chat_requests[0].body).expect("chat body JSON");
        assert_eq!(body["model"], "deepseek-v4-pro");
        assert_eq!(body["messages"][0]["role"], "system");
        assert_eq!(body["messages"][0]["content"], "You are QA.");
        assert_eq!(body["messages"][1]["role"], "user");
        assert_eq!(body["messages"][1]["content"], "review the patch");
        assert_eq!(body["temperature"], 0.2);
    }

    #[tokio::test]
    async fn openai_generate_with_usage_returns_token_counts() {
        let server = MockServer::start().await;
        mount_chat(&server, &chat_response("ok")).await;

        let client = OpenAICompatibleAdapter::new(format!("{}/v1", server.uri()), "sk-test")
            .expect("client");
        let (text, usage) = client
            .generate_with_usage(&request("m"))
            .await
            .expect("reply");
        assert_eq!(text, "ok");
        let u = usage.expect("usage");
        assert_eq!(u.prompt_tokens, 1);
        assert_eq!(u.completion_tokens, 1);
    }

    #[tokio::test]
    async fn openai_generate_with_usage_returns_none_when_no_usage_field() {
        let server = MockServer::start().await;
        let body = chat_response("ok").replace(
            r#","usage":{"prompt_tokens":1,"completion_tokens":1,"total_tokens":2}"#,
            "",
        );
        assert!(!body.contains("usage"), "{body}");
        mount_chat(&server, &body).await;

        let client = OpenAICompatibleAdapter::new(format!("{}/v1", server.uri()), "sk-test")
            .expect("client");
        let (text, usage) = client
            .generate_with_usage(&request("m"))
            .await
            .expect("reply");
        assert_eq!(text, "ok");
        assert!(usage.is_none());
    }

    #[tokio::test]
    async fn openai_generate_rejects_a_reply_cut_off_at_the_output_limit() {
        let server = MockServer::start().await;
        let body = chat_response("{\"verdict\": \"cont")
            .replace(r#""finish_reason":"stop""#, r#""finish_reason":"length""#);
        assert!(body.contains("length"), "{body}");
        mount_chat(&server, &body).await;

        let client = OpenAICompatibleAdapter::new(format!("{}/v1", server.uri()), "sk-test")
            .expect("client");
        let error = client.generate(&request("m")).await.expect_err("cut off");
        assert!(
            matches!(&error, Error::Provider(ProviderError::CutOff { text }) if text == "{\"verdict\": \"cont"),
            "{error:?}"
        );
    }

    #[tokio::test]
    async fn openai_generate_applies_temperature_override_and_max_tokens() {
        let server = MockServer::start().await;
        mount_chat(&server, &chat_response("ok")).await;

        let client = OpenAICompatibleAdapter::with_timeout(
            format!("{}/v1", server.uri()),
            "sk-test",
            Duration::from_secs(60),
            Some(0.5),
            Some(128),
        )
        .expect("client");
        client.generate(&request("m")).await.expect("reply");

        let chat_requests: Vec<_> = server
            .received_requests()
            .await
            .expect("requests")
            .into_iter()
            .filter(|r| r.url.path() == "/v1/chat/completions")
            .collect();
        assert_eq!(chat_requests.len(), 1);
        let body: Value = serde_json::from_slice(&chat_requests[0].body).expect("body");
        assert_eq!(body["temperature"], 0.5);
        assert_eq!(body["max_tokens"], 128);
    }

    #[tokio::test]
    async fn openai_generate_maps_404_to_model_not_found() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(404).set_body_raw(
                r#"{"error":{"message":"model 'nope' not found"}}"#,
                "application/json",
            ))
            .mount(&server)
            .await;

        let client = OpenAICompatibleAdapter::new(format!("{}/v1", server.uri()), "sk-test")
            .expect("client");
        let err = client
            .generate(&request("nope"))
            .await
            .expect_err("missing model");
        match err {
            Error::Provider(ProviderError::ModelNotFound { model }) => assert_eq!(model, "nope"),
            other => panic!("expected ModelNotFound, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn openai_generate_maps_http_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;

        let client = OpenAICompatibleAdapter::new(format!("{}/v1", server.uri()), "sk-test")
            .expect("client");
        let err = client.generate(&request("m")).await.expect_err("status");
        match err {
            Error::Provider(ProviderError::Ollama(msg)) => assert!(msg.contains("500"), "{msg}"),
            other => panic!("expected Ollama status error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn openai_generate_maps_timeout() {
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

        let client = OpenAICompatibleAdapter::with_timeout(
            format!("{}/v1", server.uri()),
            "sk-test",
            Duration::from_millis(40),
            None,
            None,
        )
        .expect("client");
        let err = client.generate(&request("m")).await.expect_err("timeout");
        assert!(
            matches!(err, Error::Provider(ProviderError::Timeout)),
            "expected Timeout, got {err:?}"
        );
    }

    #[tokio::test]
    async fn openai_generate_uses_request_timeout_override() {
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

        let client = OpenAICompatibleAdapter::with_timeout(
            format!("{}/v1", server.uri()),
            "sk-test",
            Duration::from_secs(60),
            None,
            None,
        )
        .expect("client");
        let mut request = request("m");
        request.timeout = Some(Duration::from_millis(40));
        let err = client.generate(&request).await.expect_err("timeout");
        assert!(
            matches!(err, Error::Provider(ProviderError::Timeout)),
            "expected Timeout, got {err:?}"
        );
    }

    #[test]
    fn openai_new_rejects_invalid_base_url() {
        let error = match OpenAICompatibleAdapter::new("not a URL", "k") {
            Ok(_) => panic!("must fail"),
            Err(error) => error,
        };
        let text = error.to_string();
        assert!(text.contains("not a URL"), "{text}");
        assert!(text.contains("http://"), "{text}");
    }

    #[test]
    fn openai_new_rejects_non_http_scheme() {
        let error = match OpenAICompatibleAdapter::new("ftp://example.test/v1", "k") {
            Ok(_) => panic!("must fail"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("http"), "{error}");
    }

    #[test]
    fn openai_new_rejects_userinfo_without_leaking_credentials() {
        let error = match OpenAICompatibleAdapter::new("http://user:secret@host:8080/v1", "k") {
            Ok(_) => panic!("must fail"),
            Err(error) => error,
        };
        let text = error.to_string();
        assert!(!text.contains("secret"), "{text}");
        assert!(!text.contains("user"), "{text}");
    }

    #[test]
    fn openai_new_rejects_query_and_fragment() {
        for url in [
            "http://127.0.0.1:8000/v1?tenant=x",
            "http://127.0.0.1:8000/v1#frag",
        ] {
            let error = match OpenAICompatibleAdapter::new(url, "k") {
                Ok(_) => panic!("{url} must fail"),
                Err(error) => error,
            };
            assert!(
                error.to_string().contains("query string or fragment"),
                "{error}"
            );
        }
    }

    #[test]
    fn openai_url_errors_never_echo_the_path() {
        let error = match OpenAICompatibleAdapter::new("https://host/v1/sk-secret?tenant=x", "k") {
            Ok(_) => panic!("query must fail"),
            Err(error) => error,
        };
        let text = error.to_string();
        assert!(!text.contains("sk-secret"), "{text}");
        assert!(text.contains("https://host"), "{text}");
    }

    #[tokio::test]
    async fn openai_does_not_follow_redirects() {
        use crate::adapters::providers::test_support::{chat_response, request};
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::path("/v1/chat/completions"))
            .respond_with(
                wiremock::ResponseTemplate::new(307)
                    .insert_header("Location", format!("{}/elsewhere", server.uri()).as_str()),
            )
            .mount(&server)
            .await;
        // If the redirect were followed, this would answer and the call would succeed.
        wiremock::Mock::given(wiremock::matchers::path("/elsewhere"))
            .respond_with(
                wiremock::ResponseTemplate::new(200)
                    .set_body_raw(chat_response("leaked"), "application/json"),
            )
            .mount(&server)
            .await;
        let client =
            OpenAICompatibleAdapter::new(format!("{}/v1", server.uri()), "sk-x").expect("client");
        assert!(client.generate(&request("m")).await.is_err());
        let paths: Vec<_> = server
            .received_requests()
            .await
            .expect("requests")
            .into_iter()
            .map(|r| r.url.path().to_owned())
            .collect();
        assert_eq!(paths, vec!["/v1/chat/completions".to_string()]);
    }

    #[test]
    fn openai_new_allows_http_only_on_loopback() {
        for url in [
            "http://127.0.0.1:8000/v1",
            "http://localhost:8000/v1",
            "http://[::1]:8000/v1",
            "https://api.example.test/v1",
        ] {
            assert!(
                OpenAICompatibleAdapter::new(url, "k").is_ok(),
                "{url} must pass"
            );
        }
        let error = match OpenAICompatibleAdapter::new("http://10.0.0.5:8000/v1", "k") {
            Ok(_) => panic!("plain http to a LAN host must fail"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("https://"), "{error}");
    }

    #[test]
    fn openai_redacted_origin_reports_base_url() {
        let client = OpenAICompatibleAdapter::new("https://host:8080/v1/", "k").expect("client");
        assert_eq!(
            client.redacted_origin().as_deref(),
            Some("https://host:8080/v1")
        );
    }
}
