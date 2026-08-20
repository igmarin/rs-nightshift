//! Shared test helpers for the provider adapters.

use crate::domain::rolegraph::config::ProviderSpec;
use crate::ports::GenerateRequest;
use std::collections::BTreeMap;
use std::sync::Mutex;
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

/// Serializes env-var mutation so parallel tests cannot race on a shared var.
static ENV_LOCK: Mutex<()> = Mutex::new(());

/// Sets an env var for the lifetime of the test and restores it afterwards.
///
/// The prior value (including absence) is captured and restored on drop, so a
/// real operator key (e.g. `MOONSHOT_API_KEY`) is never clobbered by a test.
pub(crate) struct EnvGuard {
    /// The variable name.
    name: &'static str,
    /// The value present before `set`, or `None` if the variable was unset.
    prior: Option<std::ffi::OsString>,
}

impl EnvGuard {
    /// Set `name` to `value`; restores the previous value (or absence) on drop.
    pub(crate) fn set(name: &'static str, value: &str) -> Self {
        let _lock = ENV_LOCK.lock().expect("env mutex");
        let prior = std::env::var_os(name);
        std::env::set_var(name, value);
        Self { name, prior }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        let _lock = ENV_LOCK.lock().expect("env mutex");
        match &self.prior {
            Some(value) => std::env::set_var(self.name, value),
            None => std::env::remove_var(self.name),
        }
    }
}
