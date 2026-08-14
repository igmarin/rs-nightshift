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

/// Model tag assigned to a role.
#[must_use]
pub fn model_for(role: Role) -> &'static str {
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
    fn every_role_has_a_distinct_model() {
        let mut tags = required_models()
            .iter()
            .map(|role| model_for(*role))
            .collect::<Vec<_>>();
        tags.sort_unstable();
        tags.dedup();
        assert_eq!(tags.len(), required_models().len());
        assert_eq!(model_for(Role::Pm), "llama3.1:8b");
        assert_eq!(model_for(Role::TechLead), "mistral-nemo:12b");
        assert_eq!(model_for(Role::Dev), "qwen2.5-coder:7b");
        assert_eq!(model_for(Role::Qa), "deepseek-r1:7b");
        assert_eq!(model_for(Role::Writer), "gemma2:9b");
        assert_eq!(model_for(Role::Router), "llama3.2:3b");
        assert_eq!(model_for(Role::Aux), "phi3.5:latest");
    }

    #[test]
    fn default_ollama_url_is_loopback() {
        assert_eq!(DEFAULT_OLLAMA_URL, "http://127.0.0.1:11434");
    }
}
