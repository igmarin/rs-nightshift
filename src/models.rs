//! Role to local Ollama model mapping.

use crate::error::Error;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

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
///
/// The TOML shape is a top-level `role_models` table mapping role names to
/// model tags:
///
/// ```toml
/// role_models = { Dev = "qwen2.5-coder:14b", Qa = "deepseek-r1:14b" }
/// ```
#[derive(Debug, Default, serde::Deserialize)]
pub struct ModelsConfig {
    /// Role-to-model mappings that override the defaults.
    /// Format: role_name = "model_tag"
    #[serde(default)]
    pub role_models: BTreeMap<String, String>,
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

/// Resolve the config file path from `NIGHTSHIFT_CONFIG` or the default.
#[must_use]
pub fn config_path() -> PathBuf {
    std::env::var("NIGHTSHIFT_CONFIG")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("nightshift.toml"))
}

/// Whether `NIGHTSHIFT_CONFIG` was explicitly set by the operator.
fn config_path_is_explicit() -> bool {
    std::env::var_os("NIGHTSHIFT_CONFIG").is_some()
}

/// Load models configuration from a specific file path.
///
/// A missing file is treated as an empty configuration (all defaults)
/// when it is the ambient default path. When `NIGHTSHIFT_CONFIG` was
/// explicitly set and the file is missing, an error is returned so the
/// operator learns about the mistake instead of silently falling back.
/// Read and parse errors are always returned.
pub fn load_models_config_from(path: &Path) -> Result<ModelsConfig, Error> {
    load_models_config_from_inner(path, config_path_is_explicit())
}

fn load_models_config_from_inner(path: &Path, explicit_path: bool) -> Result<ModelsConfig, Error> {
    let content = match std::fs::read_to_string(path) {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            if explicit_path {
                return Err(Error::Config {
                    path: path.display().to_string(),
                    message: "file not found (NIGHTSHIFT_CONFIG was set explicitly)".into(),
                });
            }
            return Ok(ModelsConfig::default());
        }
        Err(error) => {
            return Err(Error::Config {
                path: path.display().to_string(),
                message: error.to_string(),
            });
        }
    };
    toml::from_str(&content).map_err(|error| Error::Config {
        path: path.display().to_string(),
        message: error.to_string(),
    })
}

/// Load the models configuration from the ambient config path.
///
/// Errors are logged to stderr so the operator sees them in `run.log`
/// even in the unattended SSH/tmux workflow, then defaults are used so
/// the pipeline stays infallible. `doctor` surfaces the same errors via
/// [`load_models_config_from`].
fn load_models_config() -> ModelsConfig {
    match load_models_config_from(&config_path()) {
        Ok(config) => config,
        Err(error) => {
            eprintln!("warning: {error}; using default models");
            ModelsConfig::default()
        }
    }
}

/// Resolve the model tag for a role using the given config, falling back to defaults.
#[must_use]
pub fn model_for_with_config(role: Role, config: &ModelsConfig) -> String {
    let role_name = role_name(role);
    config
        .role_models
        .get(role_name)
        .cloned()
        .unwrap_or_else(|| default_model_for(role).to_string())
}

/// Model tag assigned to a role, reading from nightshift.toml with fallback to defaults.
pub fn model_for(role: Role) -> String {
    model_for_with_config(role, &load_models_config())
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
    fn load_config_from_missing_default_file_uses_defaults() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("nightshift.toml");
        // explicit=false: ambient default path missing is Ok (all defaults).
        let config = load_models_config_from_inner(&path, false).expect("missing default is ok");
        assert!(config.role_models.is_empty());
    }

    #[test]
    fn load_config_from_missing_explicit_file_returns_error() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("nightshift.toml");
        // explicit=true: NIGHTSHIFT_CONFIG was set, so a missing file is an error.
        let error =
            load_models_config_from_inner(&path, true).expect_err("missing explicit must error");
        let text = error.to_string();
        assert!(text.contains("config error"), "{text}");
        assert!(text.contains("NIGHTSHIFT_CONFIG"), "{text}");
    }

    #[test]
    fn load_config_from_valid_overrides_applies_them() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("nightshift.toml");
        std::fs::write(
            &path,
            "role_models = { Dev = \"qwen2.5-coder:14b\", Router = \"llama3.2:3b\" }\n",
        )
        .expect("write");
        let config = load_models_config_from_inner(&path, false).expect("valid config");
        assert_eq!(
            config.role_models.get("Dev"),
            Some(&"qwen2.5-coder:14b".to_string())
        );
    }

    #[test]
    fn load_config_from_malformed_returns_error() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("nightshift.toml");
        std::fs::write(&path, "not = valid = toml\n").expect("write");
        let error = load_models_config_from_inner(&path, false).expect_err("malformed must error");
        let text = error.to_string();
        assert!(text.contains("config error"), "{text}");
        assert!(text.contains("nightshift.toml"), "{text}");
    }

    #[test]
    fn model_for_with_config_uses_override_when_present() {
        let mut role_models = BTreeMap::new();
        role_models.insert("Router".to_string(), "custom-model:latest".to_string());
        let config = ModelsConfig { role_models };
        assert_eq!(
            model_for_with_config(Role::Router, &config),
            "custom-model:latest"
        );
    }

    #[test]
    fn model_for_with_config_falls_back_to_default() {
        let config = ModelsConfig::default();
        assert_eq!(model_for_with_config(Role::Router, &config), "llama3.2:3b");
    }

    #[test]
    fn model_for_with_config_ignores_unknown_role_keys() {
        let mut role_models = BTreeMap::new();
        role_models.insert("Typo".to_string(), "whatever".to_string());
        let config = ModelsConfig { role_models };
        assert_eq!(
            model_for_with_config(Role::Dev, &config),
            "qwen2.5-coder:7b"
        );
    }

    #[test]
    fn example_config_parses_successfully() {
        let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_else(|_| ".".into());
        let example = Path::new(&manifest_dir).join("nightshift.toml.example");
        let config = load_models_config_from(&example).expect("example must parse");
        // The example file has all overrides commented out, so it should
        // produce an empty role_models map (all defaults).
        assert!(
            config.role_models.is_empty(),
            "example file should have no active overrides: {:?}",
            config.role_models
        );
    }
}
