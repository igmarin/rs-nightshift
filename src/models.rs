//! Role to local Ollama model mapping.

/// Default Ollama HTTP origin.
pub const DEFAULT_OLLAMA_URL: &str = "http://127.0.0.1:11434";

/// Pipeline role that maps to one local model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// Fast schema repair and payload checks.
    Router,
    /// Product owner / user-story writer.
    Pm,
    /// Tech lead / architect.
    TechLead,
    /// Implementation / patch author.
    Dev,
    /// Test-failure reasoner.
    Qa,
    /// Changelog and article writer.
    Writer,
    /// Lightweight auxiliary sanity check.
    Aux,
}

/// Configuration for role-to-model mapping from nightshift.toml.
#[derive(Debug, serde::Deserialize)]
pub struct ModelsConfig {
    /// Role-to-model mappings that override the defaults.
    /// Format: role_name = "model_tag"
    #[serde(default)]
    pub role_models: std::collections::BTreeMap<String, String>,
}

/// Get the string name of a Role for config lookups.
fn role_name(role: Role) -> &'static str {
    match role {
        Role::Router => "Router",
        Role::Pm => "Pm",
        Role::TechLead => "TechLead",
        Role::Dev => "Dev",
        Role::Qa => "Qa",
        Role::Writer => "Writer",
        Role::Aux => "Aux",
    }
}

/// Load the models configuration from nightshift.toml.
fn load_models_config() -> ModelsConfig {
    let config_path =
        std::env::var("NIGHTSHIFT_CONFIG").unwrap_or_else(|_| "nightshift.toml".to_string());
    let config_content = std::fs::read_to_string(config_path).unwrap_or_default();
    toml::from_str(&config_content).unwrap_or_else(|_| ModelsConfig {
        role_models: std::collections::BTreeMap::new(),
    })
}

/// Model tag assigned to a role, reading from nightshift.toml with fallback to defaults.
pub fn model_for(role: Role) -> String {
    let config = load_models_config();
    let role_name = role_name(role);
    config
        .role_models
        .get(role_name)
        .cloned()
        .unwrap_or_else(|| default_model_for(role).to_string())
}

/// Map a role to its default model tag.
#[must_use]
pub fn default_model_for(role: Role) -> &'static str {
    match role {
        Role::Router => "llama3.2:3b",
        Role::Pm => "llama3.1:8b",
        Role::TechLead => "mistral-nemo:12b",
        Role::Dev => "qwen2.5-coder:7b",
        Role::Qa => "deepseek-r1:7b",
        Role::Writer => "gemma2:9b",
        Role::Aux => "phi3.5:latest",
    }
}

/// Every model the overnight pipeline expects to be present.
#[must_use]
pub fn required_models() -> &'static [Role] {
    &[
        Role::Router,
        Role::Pm,
        Role::TechLead,
        Role::Dev,
        Role::Qa,
        Role::Writer,
        Role::Aux,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_role_has_a_distinct_default_model() {
        let tags: Vec<&str> = required_models()
            .iter()
            .map(|role| default_model_for(*role))
            .collect();
        let mut sorted = tags.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), required_models().len());
    }

    #[test]
    fn default_models_are_stable() {
        assert_eq!(default_model_for(Role::Pm), "llama3.1:8b");
        assert_eq!(default_model_for(Role::TechLead), "mistral-nemo:12b");
        assert_eq!(default_model_for(Role::Dev), "qwen2.5-coder:7b");
        assert_eq!(default_model_for(Role::Qa), "deepseek-r1:7b");
        assert_eq!(default_model_for(Role::Writer), "gemma2:9b");
        assert_eq!(default_model_for(Role::Router), "llama3.2:3b");
        assert_eq!(default_model_for(Role::Aux), "phi3.5:latest");
    }

    #[test]
    fn default_ollama_url_is_loopback() {
        assert_eq!(DEFAULT_OLLAMA_URL, "http://127.0.0.1:11434");
    }

    #[test]
    fn config_overrides_are_expected() {
        assert_eq!(
            model_for(Role::Router),
            "llama3.2:3b",
            "no config present, so router keeps its default"
        );
    }
}
