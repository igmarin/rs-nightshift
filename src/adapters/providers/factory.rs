//! Provider factory: wire provider names + spec + options into a client.

use crate::domain::rolegraph::config::ProviderSpec;
use crate::error::Error;
use crate::ollama::{OllamaClient, DEFAULT_GENERATE_TIMEOUT};
use crate::ports::{ModelClient, ModelClientFactory};
use std::collections::BTreeMap;

use super::ollama::OllamaAdapter;
use super::openai::OpenAICompatibleAdapter;
use super::options::ModelOptions;

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
    use crate::adapters::providers::test_support::{
        chat_response, mount_chat, options, request, spec_with, EnvGuard,
    };
    use serde_json::Value;
    use wiremock::matchers::{body_partial_json, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

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
