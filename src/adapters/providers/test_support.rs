//! Shared test helpers for the provider adapters.

use crate::domain::rolegraph::config::ProviderSpec;
use crate::ports::GenerateRequest;
use std::collections::BTreeMap;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// A minimal generate request for the given model.
pub(crate) fn request(model: &str) -> GenerateRequest {
    GenerateRequest {
        model: model.into(),
        system: Some("You are QA.".into()),
        prompt: "review the patch".into(),
        temperature: 0.2,
    }
}

/// Mount an OpenAI-compatible `/v1/chat/completions` 200 handler.
pub(crate) async fn mount_chat(server: &MockServer, body: &str) {
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(body.to_string(), "application/json"))
        .mount(server)
        .await;
}

/// Standard OpenAI chat completion response with the given content.
pub(crate) fn chat_response(content: &str) -> String {
    format!(
        r#"{{"id":"chatcmpl-x","created":1,"model":"ollama","choices":[{{"index":0,"message":{{"role":"assistant","content":{}}},"finish_reason":"stop"}}],"usage":{{"prompt_tokens":1,"completion_tokens":1,"total_tokens":2}}}}"#,
        serde_json::to_string(content).expect("content JSON")
    )
}

/// Build a role-options map from key/value pairs.
pub(crate) fn options(entries: &[(&str, toml::Value)]) -> BTreeMap<String, toml::Value> {
    entries
        .iter()
        .map(|(key, value)| ((*key).to_string(), value.clone()))
        .collect()
}

/// A `ProviderSpec` with the given base URL and API-key env var.
pub(crate) fn spec_with(base_url: &str, api_key_env: &str) -> ProviderSpec {
    ProviderSpec {
        backend: None,
        base_url: Some(base_url.into()),
        api_key_env: Some(api_key_env.into()),
    }
}

/// Sets an env var for the lifetime of the test and removes it afterwards.
pub(crate) struct EnvGuard(&'static str);

impl EnvGuard {
    /// Set `name` to `value`; removes `name` on drop.
    pub(crate) fn set(name: &'static str, value: &str) -> Self {
        std::env::set_var(name, value);
        Self(name)
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        std::env::remove_var(self.0);
    }
}
