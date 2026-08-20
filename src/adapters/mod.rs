//! Adapters (hexagonal) implementing the [`crate::ports::ModelClient`] port.
//!
//! This is the only layer allowed to import `llm-kernel` and `reqwest`
//! (ADR-007; `docs/role-graph.md` §Hexagonal). It provides an
//! OpenAI-compatible adapter (Deepseek, Kimi, and any custom
//! `openai-compatible` provider) and an Ollama adapter that preserves the
//! `keep_alive: 0` VRAM-unload behavior, plus the [`build_model_client`]
//! factory that wires provider names + [`ProviderSpec`] + role options into a
//! concrete client.
//!
//! # Provider rules
//!
//! - `ollama` — local, default base URL [`DEFAULT_OLLAMA_BASE_URL`]; no API
//!   key (a placeholder is passed to `llm-kernel`'s OpenAI client, mirroring
//!   `OllamaClient`). The `think` option appends the `:think` model-tag
//!   suffix (Ollama's convention for thinking variants, e.g. `qwen3:think`),
//!   so `options.think = true` on role `model = "phi4"` calls `phi4:think`.
//!   `num_ctx` is validated but deliberately not forwarded: llm-kernel's
//!   OpenAI-compatible request has no `num_ctx` field, so the value cannot
//!   reach Ollama through this path.
//! - `deepseek` — OpenAI-compatible, default base URL
//!   [`DEFAULT_DEEPSEEK_BASE_URL`], key from [`DEFAULT_DEEPSEEK_API_KEY_ENV`].
//!   The model tag is passed verbatim (e.g. `deepseek-v4-pro`, or
//!   `deepseek-reasoner` for high-thinking — the operator owns the tag).
//! - `kimi` — OpenAI-compatible, default base URL [`DEFAULT_KIMI_BASE_URL`],
//!   key from [`DEFAULT_KIMI_API_KEY_ENV`].
//! - any other provider name — resolved through `ProviderSpec.backend`;
//!   `openai-compatible` is honored, anything else is a config error.
//!
//! # Options (`RoleSpec.options`)
//!
//! `temperature` (float, or integer) overrides the request temperature when
//! present; `max_tokens` (non-negative integer) caps the completion length;
//! `think` (boolean) is honored by the Ollama adapter (see above). Unknown
//! option keys are ignored (the config schema declares options a
//! provider/model-specific passthrough). Invalid option *types* are config
//! errors so typos surface at build time rather than silently.

use crate::domain::rolegraph::config::ProviderSpec;
use crate::error::Error;
use crate::generate::{map_kernel_error, Origin};
use crate::ollama::{OllamaClient, DEFAULT_GENERATE_TIMEOUT};
use crate::ports::{GenerateRequest, ModelClient, ModelClientFactory};
use async_trait::async_trait;
use llm_kernel::llm::{ChatMessage, LLMClient, LLMRequest, OpenAIClient};
use std::collections::BTreeMap;
use std::time::Duration;

/// Default base URL for the built-in `ollama` provider.
pub const DEFAULT_OLLAMA_BASE_URL: &str = "http://127.0.0.1:11434";

/// Default base URL for the built-in `deepseek` provider.
pub const DEFAULT_DEEPSEEK_BASE_URL: &str = "https://api.deepseek.com";

/// Default base URL for the built-in `kimi` provider.
pub const DEFAULT_KIMI_BASE_URL: &str = "https://api.moonshot.cn/v1";

/// Default env var holding the Deepseek API key.
pub const DEFAULT_DEEPSEEK_API_KEY_ENV: &str = "DEEPSEEK_API_KEY";

/// Default env var holding the Kimi/Moonshot API key.
pub const DEFAULT_KIMI_API_KEY_ENV: &str = "MOONSHOT_API_KEY";

/// Resolved per-role options that the adapters act on.
///
/// `temperature` and `max_tokens` apply to every provider; `think` is only
/// acted on by the Ollama adapter (`:think` tag suffix). `num_ctx` is parsed
/// and validated but deliberately not stored — see the module docs.
#[derive(Debug, Clone, Copy, Default)]
struct ModelOptions {
    /// Overrides the request temperature when set.
    temperature: Option<f32>,
    /// Requested max output tokens.
    max_tokens: Option<u32>,
    /// Whether the Ollama `:think` tag suffix should be applied.
    think: bool,
}

impl ModelOptions {
    /// Parse and type-check the role's `options` map.
    fn parse(options: &BTreeMap<String, toml::Value>) -> Result<Self, Error> {
        let temperature = match options.get("temperature") {
            None => None,
            Some(toml::Value::Float(value)) => Some(*value as f32),
            Some(toml::Value::Integer(value)) => Some(*value as f32),
            Some(_) => return Err(bad_option("temperature", "a number")),
        };
        let max_tokens = match options.get("max_tokens") {
            None => None,
            Some(value) => {
                let value = value
                    .as_integer()
                    .ok_or_else(|| bad_option("max_tokens", "an integer"))?;
                Some(
                    u32::try_from(value)
                        .map_err(|_| bad_option("max_tokens", "a non-negative integer"))?,
                )
            }
        };
        let think = match options.get("think") {
            None => false,
            Some(value) => value
                .as_bool()
                .ok_or_else(|| bad_option("think", "a boolean"))?,
        };
        if let Some(value) = options.get("num_ctx") {
            let value = value
                .as_integer()
                .ok_or_else(|| bad_option("num_ctx", "an integer"))?;
            // Validated but not forwarded: see the module docs.
            u32::try_from(value).map_err(|_| bad_option("num_ctx", "a non-negative integer"))?;
        }
        Ok(Self {
            temperature,
            max_tokens,
            think,
        })
    }
}

/// Build a [`Error::Config`] for a malformed role option.
fn bad_option(name: &str, expected: &str) -> Error {
    Error::Config {
        path: format!("option {name:?}"),
        message: format!("expected {expected}"),
    }
}

/// Validate an OpenAI-compatible base URL for the remote adapters.
///
/// Accepts `http://` / `https://` URLs with a host (a path such as `/v1` is
/// allowed) and rejects userinfo so credentials can never be embedded in the
/// URL. Rejected values are reported redacted.
fn validate_chat_base_url(value: &str) -> Result<String, Error> {
    let value = value.trim();
    let redacted = crate::ollama::redact_ollama_url(value);
    let parsed = reqwest::Url::parse(value).map_err(|_| Error::Config {
        path: "provider base_url".into(),
        message: format!(
            "invalid base URL {redacted:?}: expected an http:// or https:// URL with a host"
        ),
    })?;
    if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
        return Err(Error::Config {
            path: "provider base_url".into(),
            message: format!(
                "invalid base URL {redacted:?}: expected an http:// or https:// URL with a host"
            ),
        });
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(Error::Config {
            path: "provider base_url".into(),
            message: format!(
                "base URL {redacted:?} must not embed credentials; use the API key header instead"
            ),
        });
    }
    Ok(parsed.to_string().trim_end_matches('/').to_owned())
}

/// Map a port request to an [`LLMRequest`] for the OpenAI-compatible path.
fn to_llm_request(
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

/// OpenAI-compatible [`ModelClient`] used for Deepseek, Kimi, and any custom
/// `openai-compatible` provider.
///
/// Wraps [`OpenAIClient`] and maps [`GenerateRequest`] to an [`LLMRequest`]
/// (model, system, a single user message, temperature, optional max tokens).
/// Kernel errors are mapped via [`map_kernel_error`]. Completions are bounded
/// by `tokio::time::timeout` so a hanging provider surfaces as
/// [`Error::Timeout`] (the kernel's own client would otherwise surface
/// reqwest timeouts as `LlmApi`, losing the structured info).
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
        Self::with_timeout(base_url, api_key, DEFAULT_GENERATE_TIMEOUT, None, None)
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
        let redacted_base_url = crate::ollama::redact_ollama_url(&base_url);
        // The per-request model always overrides the placeholder below, so
        // the client-level model name is never sent to the provider.
        let http_client = reqwest::Client::builder()
            .build()
            .map_err(|error| Error::Config {
                path: "provider base_url".into(),
                message: format!("failed to build HTTP client: {error}"),
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
        let response = tokio::time::timeout(
            self.timeout,
            self.inner
                .complete(to_llm_request(request, self.temperature, self.max_tokens)),
        )
        .await
        .map_err(|_| Error::Timeout)
        .and_then(|result| result.map_err(|error| map_kernel_error(error, &request.model)))?;
        Ok(response.content)
    }

    fn redacted_origin(&self) -> Option<String> {
        Some(self.redacted_base_url.clone())
    }
}

/// [`ModelClient`] adapter over [`OllamaClient`], preserving its
/// `keep_alive: 0` VRAM-unload behavior.
///
/// Each [`generate`](ModelClient::generate) call delegates to
/// [`OllamaClient::complete`], which sends the completion through Ollama's
/// OpenAI-compatible endpoint and then posts a best-effort `keep_alive: 0`
/// unload to the native `/api/generate` endpoint so the model is released
/// from VRAM between stages. When `think` is set, the `:think` tag suffix is
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
}

impl OllamaAdapter {
    /// Adapter over `inner` with no `:think` suffix and no per-role options.
    #[must_use]
    pub fn new(inner: OllamaClient, think: bool) -> Self {
        Self {
            inner,
            think,
            temperature: None,
            max_tokens: None,
        }
    }

    /// Adapter over `inner` with an explicit think flag and per-role options.
    #[must_use]
    pub fn with_options(
        inner: OllamaClient,
        think: bool,
        temperature: Option<f32>,
        max_tokens: Option<u32>,
    ) -> Self {
        Self {
            inner,
            think,
            temperature,
            max_tokens,
        }
    }
}

#[async_trait]
impl ModelClient for OllamaAdapter {
    async fn generate(&self, request: &GenerateRequest) -> Result<String, Error> {
        let model = resolve_ollama_model(&request.model, self.think);
        let llm_request = LLMRequest {
            model: Some(model),
            ..to_llm_request(request, self.temperature, self.max_tokens)
        };
        let response = self
            .inner
            .complete(llm_request)
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

/// Build a [`ModelClient`] for `provider` from its [`ProviderSpec`] and role
/// options.
///
/// Provider rules and option semantics are documented in the module docs.
/// Build-time configuration failures (unknown provider/backend, missing API
/// key env var, malformed base URL or option types) are reported as
/// [`Error::Config`].
pub fn build_model_client(
    provider: &str,
    spec: Option<&ProviderSpec>,
    options: &BTreeMap<String, toml::Value>,
) -> Result<Box<dyn ModelClient>, Error> {
    let parsed = ModelOptions::parse(options)?;
    match provider {
        "ollama" => build_ollama(spec, parsed),
        "deepseek" => build_openai_compatible(
            provider,
            Some(DEFAULT_DEEPSEEK_BASE_URL),
            Some(DEFAULT_DEEPSEEK_API_KEY_ENV),
            spec,
            parsed,
        ),
        "kimi" => build_openai_compatible(
            provider,
            Some(DEFAULT_KIMI_BASE_URL),
            Some(DEFAULT_KIMI_API_KEY_ENV),
            spec,
            parsed,
        ),
        other => match spec.and_then(|s| s.backend.as_deref()) {
            Some("openai-compatible") => build_openai_compatible(other, None, None, spec, parsed),
            Some(backend) => Err(Error::Config {
                path: format!("provider {other:?}"),
                message: format!("unknown backend {backend:?}; expected \"openai-compatible\""),
            }),
            None => Err(Error::Config {
                path: format!("provider {other:?}"),
                message: format!(
                    "unknown provider; define a [providers.{other}] block with \
                     backend = \"openai-compatible\""
                ),
            }),
        },
    }
}

/// [`ModelClientFactory`] implementation that wires provider names to concrete
/// adapters via [`build_model_client`].
///
/// The CLI edge constructs this and hands it to the orchestrator, so the
/// application never imports an adapter directly.
pub struct ProviderFactory;

impl ModelClientFactory for ProviderFactory {
    fn build(
        &self,
        provider: &str,
        spec: Option<&ProviderSpec>,
        options: &BTreeMap<String, toml::Value>,
    ) -> Result<Box<dyn ModelClient>, Error> {
        build_model_client(provider, spec, options)
    }
}

/// Build the built-in Ollama adapter, honoring `think`, `temperature`, and
/// `max_tokens` options.
fn build_ollama(
    spec: Option<&ProviderSpec>,
    options: ModelOptions,
) -> Result<Box<dyn ModelClient>, Error> {
    let base_url = spec
        .and_then(|s| s.base_url.clone())
        .unwrap_or_else(|| DEFAULT_OLLAMA_BASE_URL.to_string());
    let inner = OllamaClient::new(base_url)?;
    Ok(Box::new(OllamaAdapter::with_options(
        inner,
        options.think,
        options.temperature,
        options.max_tokens,
    )))
}

/// Build an OpenAI-compatible adapter for `provider`.
///
/// `default_base_url` / `default_api_key_env` supply the built-in values for
/// `deepseek` / `kimi`; a `ProviderSpec` override wins when present. Custom
/// providers must supply both via their spec.
fn build_openai_compatible(
    provider: &str,
    default_base_url: Option<&str>,
    default_api_key_env: Option<&str>,
    spec: Option<&ProviderSpec>,
    options: ModelOptions,
) -> Result<Box<dyn ModelClient>, Error> {
    let base_url = spec
        .and_then(|s| s.base_url.clone())
        .or_else(|| default_base_url.map(str::to_owned))
        .ok_or_else(|| Error::Config {
            path: format!("provider {provider:?}"),
            message: "no base_url configured and no built-in default".into(),
        })?;
    let api_key_env = spec
        .and_then(|s| s.api_key_env.clone())
        .or_else(|| default_api_key_env.map(str::to_owned))
        .ok_or_else(|| Error::Config {
            path: format!("provider {provider:?}"),
            message: "no api_key_env configured and no built-in default".into(),
        })?;
    let api_key = std::env::var(&api_key_env).map_err(|_| Error::Config {
        path: api_key_env.clone(),
        message: "environment variable not set".into(),
    })?;
    let client = OpenAICompatibleAdapter::with_timeout(
        base_url,
        api_key,
        DEFAULT_GENERATE_TIMEOUT,
        options.temperature,
        options.max_tokens,
    )?;
    Ok(Box::new(client))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ollama::OllamaClient;
    use serde_json::Value;
    use std::time::Duration;
    use wiremock::matchers::{body_partial_json, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn request(model: &str) -> GenerateRequest {
        GenerateRequest {
            model: model.into(),
            system: Some("You are QA.".into()),
            prompt: "review the patch".into(),
            temperature: 0.2,
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

    fn options(entries: &[(&str, toml::Value)]) -> BTreeMap<String, toml::Value> {
        entries
            .iter()
            .map(|(key, value)| ((*key).to_string(), value.clone()))
            .collect()
    }

    fn spec_with(base_url: &str, api_key_env: &str) -> ProviderSpec {
        ProviderSpec {
            backend: None,
            base_url: Some(base_url.into()),
            api_key_env: Some(api_key_env.into()),
        }
    }

    /// Sets an env var for the lifetime of the test and removes it afterwards.
    struct EnvGuard(&'static str);

    impl EnvGuard {
        fn set(name: &'static str, value: &str) -> Self {
            std::env::set_var(name, value);
            Self(name)
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            std::env::remove_var(self.0);
        }
    }

    // ---- OpenAI-compatible adapter -------------------------------------

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
        // The role's option overrides the request temperature.
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
            Error::ModelNotFound { model } => assert_eq!(model, "nope"),
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
            Error::Ollama(msg) => assert!(msg.contains("500"), "{msg}"),
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
            matches!(err, Error::Timeout),
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
    fn openai_redacted_origin_reports_base_url() {
        let client = OpenAICompatibleAdapter::new("http://host:8080/v1/", "k").expect("client");
        assert_eq!(
            client.redacted_origin().as_deref(),
            Some("http://host:8080/v1")
        );
    }

    // ---- Ollama adapter -------------------------------------------------

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

    // ---- Factory ---------------------------------------------------------

    #[tokio::test]
    async fn factory_deepseek_uses_spec_base_url_env_and_auth_header() {
        let _guard = EnvGuard::set("RS_NIGHTSHIFT_TEST_DEEPSEEK_KEY", "sk-test-deepseek-123");
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .and(header("Authorization", "Bearer sk-test-deepseek-123"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_raw(chat_response("deepseek answer"), "application/json"),
            )
            .mount(&server)
            .await;

        let client = build_model_client(
            "deepseek",
            Some(&spec_with(
                &format!("{}/v1", server.uri()),
                "RS_NIGHTSHIFT_TEST_DEEPSEEK_KEY",
            )),
            &options(&[]),
        )
        .expect("client");
        let text = client
            .generate(&request("deepseek-v4-pro"))
            .await
            .expect("reply");
        assert_eq!(text, "deepseek answer");

        let chat_requests: Vec<_> = server
            .received_requests()
            .await
            .expect("requests")
            .into_iter()
            .filter(|r| r.url.path() == "/v1/chat/completions")
            .collect();
        assert_eq!(chat_requests.len(), 1);
        let body: Value = serde_json::from_slice(&chat_requests[0].body).expect("body");
        assert_eq!(body["model"], "deepseek-v4-pro");
    }

    #[tokio::test]
    async fn factory_kimi_defaults_to_moonshot_env() {
        let _guard = EnvGuard::set("MOONSHOT_API_KEY", "sk-kimi-test");
        let server = MockServer::start().await;
        mount_chat(&server, &chat_response("kimi answer")).await;

        let spec = ProviderSpec {
            backend: None,
            base_url: Some(format!("{}/v1", server.uri())),
            api_key_env: None,
        };
        let client = build_model_client("kimi", Some(&spec), &options(&[])).expect("client");
        let text = client.generate(&request("kimi3")).await.expect("reply");
        assert_eq!(text, "kimi answer");

        let chat_requests: Vec<_> = server
            .received_requests()
            .await
            .expect("requests")
            .into_iter()
            .filter(|r| r.url.path() == "/v1/chat/completions")
            .collect();
        assert_eq!(chat_requests.len(), 1);
        let body: Value = serde_json::from_slice(&chat_requests[0].body).expect("body");
        assert_eq!(body["model"], "kimi3");
    }

    #[test]
    fn factory_ollama_defaults_to_localhost() {
        let client = build_model_client("ollama", None, &options(&[])).expect("client");
        assert_eq!(
            client.redacted_origin().as_deref(),
            Some("http://127.0.0.1:11434")
        );
    }

    #[tokio::test]
    async fn factory_ollama_spec_base_url_and_options() {
        let server = MockServer::start().await;
        mount_chat(&server, &chat_response("local answer")).await;
        Mock::given(method("POST"))
            .and(path("/api/generate"))
            .and(body_partial_json(serde_json::json!({"keep_alive": 0})))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;

        let client = build_model_client(
            "ollama",
            Some(&spec_with(&server.uri(), "unused")),
            &options(&[
                ("think", toml::Value::Boolean(true)),
                ("temperature", toml::Value::Float(0.1)),
                ("max_tokens", toml::Value::Integer(512)),
                ("num_ctx", toml::Value::Integer(8192)),
            ]),
        )
        .expect("client");
        let text = client.generate(&request("phi4")).await.expect("reply");
        assert_eq!(text, "local answer");

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
        assert_eq!(body["temperature"], 0.1);
        assert_eq!(body["max_tokens"], 512);

        // The memory-critical unload still fires through the factory.
        let generate_requests: Vec<_> = server
            .received_requests()
            .await
            .expect("requests")
            .into_iter()
            .filter(|r| r.url.path() == "/api/generate")
            .collect();
        assert_eq!(generate_requests.len(), 1, "expected one unload request");
        let unload: Value =
            serde_json::from_slice(&generate_requests[0].body).expect("unload body JSON");
        assert_eq!(unload["keep_alive"], 0);
        assert_eq!(unload["model"], "phi4:think");
    }

    #[tokio::test]
    async fn factory_custom_openai_compatible_backend() {
        let _guard = EnvGuard::set("RS_NIGHTSHIFT_TEST_ACME_KEY", "sk-acme");
        let server = MockServer::start().await;
        mount_chat(&server, &chat_response("acme answer")).await;

        let spec = ProviderSpec {
            backend: Some("openai-compatible".into()),
            base_url: Some(format!("{}/v1", server.uri())),
            api_key_env: Some("RS_NIGHTSHIFT_TEST_ACME_KEY".into()),
        };
        let client = build_model_client("acme", Some(&spec), &options(&[])).expect("client");
        let text = client
            .generate(&request("acme-model"))
            .await
            .expect("reply");
        assert_eq!(text, "acme answer");
    }

    #[test]
    fn factory_missing_api_key_is_config_error() {
        let err = match build_model_client(
            "deepseek",
            Some(&spec_with(
                "http://127.0.0.1:9/v1",
                "RS_NIGHTSHIFT_TEST_NO_SUCH_KEY_42",
            )),
            &options(&[]),
        ) {
            Ok(_) => panic!("missing env must fail"),
            Err(error) => error,
        };
        let text = err.to_string();
        assert!(text.contains("RS_NIGHTSHIFT_TEST_NO_SUCH_KEY_42"), "{text}");
    }

    #[test]
    fn factory_unknown_provider_is_config_error() {
        let err = match build_model_client("ghost", None, &options(&[])) {
            Ok(_) => panic!("must fail"),
            Err(error) => error,
        };
        assert!(err.to_string().contains("ghost"), "{err}");
    }

    #[test]
    fn factory_unknown_backend_is_config_error() {
        let spec = ProviderSpec {
            backend: Some("anthropic".into()),
            base_url: Some("http://127.0.0.1:9".into()),
            api_key_env: Some("X".into()),
        };
        let err = match build_model_client("acme", Some(&spec), &options(&[])) {
            Ok(_) => panic!("must fail"),
            Err(error) => error,
        };
        assert!(err.to_string().contains("anthropic"), "{err}");
    }

    #[test]
    fn factory_invalid_option_types_are_config_errors() {
        for (name, value) in [
            ("temperature", toml::Value::String("hot".into())),
            ("max_tokens", toml::Value::String("many".into())),
            ("max_tokens", toml::Value::Integer(-5)),
            ("think", toml::Value::String("yes".into())),
            ("num_ctx", toml::Value::String("big".into())),
        ] {
            let err = match build_model_client("ollama", None, &options(&[(name, value)])) {
                Ok(_) => panic!("invalid option must fail"),
                Err(error) => error,
            };
            assert!(err.to_string().contains(name), "{err}");
        }
    }

    #[test]
    fn factory_deepseek_default_base_url() {
        // `api_key_env` from the spec; no `base_url`, so the built-in
        // default base URL is used. Unique env var so parallel tests can
        // never race on the real `DEEPSEEK_API_KEY`.
        let _guard = EnvGuard::set("RS_NIGHTSHIFT_TEST_DEEPSEEK_DEFAULT_KEY", "sk-default-env");
        let spec = ProviderSpec {
            backend: None,
            base_url: None,
            api_key_env: Some("RS_NIGHTSHIFT_TEST_DEEPSEEK_DEFAULT_KEY".into()),
        };
        let client = build_model_client("deepseek", Some(&spec), &options(&[])).expect("client");
        assert_eq!(
            client.redacted_origin().as_deref(),
            Some("https://api.deepseek.com")
        );
    }

    #[test]
    fn factory_kimi_default_base_url() {
        // `api_key_env` from the spec; no `base_url`, so the built-in
        // default base URL is used. Unique env var so parallel tests can
        // never race on the real `MOONSHOT_API_KEY`.
        let _guard = EnvGuard::set("RS_NIGHTSHIFT_TEST_KIMI_KEY", "sk-default-env");
        let spec = ProviderSpec {
            backend: None,
            base_url: None,
            api_key_env: Some("RS_NIGHTSHIFT_TEST_KIMI_KEY".into()),
        };
        let client = build_model_client("kimi", Some(&spec), &options(&[])).expect("client");
        assert_eq!(
            client.redacted_origin().as_deref(),
            Some("https://api.moonshot.cn/v1")
        );
    }
}
