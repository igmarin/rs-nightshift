//! Role-graph configuration: the `nightshift.toml` schema and validation.

use crate::domain::rolegraph::routing::{Routing, Target};
use crate::error::{ConfigError, Error};
use serde::Deserialize;
use std::collections::{BTreeMap, HashSet};
use std::path::Path;

/// Default global role-execution cap (a runaway backstop, not a target).
pub const DEFAULT_MAX_STEPS: u32 = 30;

/// Default per-role cap on how many times a back-edge may fire.
pub const DEFAULT_MAX_LOOP: u32 = 3;

/// How the harness treats clarifying questions it cannot resolve.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OnUnclear {
    /// Halt and write a report (the unattended-overnight default).
    #[default]
    Halt,
    /// Record the questions as assumptions and continue.
    Proceed,
}

/// Run-level options under the `[run]` table.
#[derive(Debug, Clone, Deserialize)]
pub struct RunOptions {
    /// The id of the entry role.
    pub start: String,
    /// How unresolved clarifying questions are handled.
    #[serde(default)]
    pub on_unclear: OnUnclear,
    /// Global cap on total role executions.
    #[serde(default = "default_max_steps")]
    pub max_steps: u32,
}

/// One provider definition under `[providers.<name>]`.
///
/// The built-in `ollama` provider needs no entry (it defaults to
/// `http://127.0.0.1:11434`); override it here to move it.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ProviderSpec {
    /// Provider backend id (e.g. `openai-compatible`); interpreted by the
    /// client factory.
    #[serde(default)]
    pub backend: Option<String>,
    /// Base URL override.
    #[serde(default)]
    pub base_url: Option<String>,
    /// Environment variable holding the API key.
    #[serde(default)]
    pub api_key_env: Option<String>,
}

/// One role definition under `[[roles]]`.
#[derive(Debug, Clone, Deserialize)]
pub struct RoleSpec {
    /// Unique role id referenced by routing targets and `start`.
    pub id: String,
    /// Provider name; `ollama` is built in, others must be in `providers`.
    pub provider: String,
    /// Model tag/variant passed through to the provider verbatim.
    pub model: String,
    /// Model-specific options (temperature, think, max_tokens, …).
    #[serde(default)]
    pub options: BTreeMap<String, toml::Value>,
    /// The role's job prompt.
    #[serde(default)]
    pub prompt: String,
    /// Artifact file the role writes (relative to the run dir).
    #[serde(default)]
    pub output: Option<String>,
    /// Code-side tools the role declares (`apply-patch`, `run-tests`, …).
    #[serde(default)]
    pub tools: Vec<String>,
    /// Repo-relative files to read and inject as raw context (for non-code
    /// files that `codegraph`/`graphify` don't index, e.g. HTML, CSS).
    #[serde(default)]
    pub context_files: Vec<String>,
    /// Verdict → target routing map.
    #[serde(default)]
    pub on: Routing,
    /// Cap on how many times a back-edge from this role may fire.
    #[serde(default = "default_max_loop")]
    pub max_loop: u32,
}

/// The parsed `nightshift.toml` role graph.
#[derive(Debug, Clone, Deserialize)]
pub struct NightshiftConfig {
    /// Run-level options.
    pub run: RunOptions,
    /// Named providers beyond the built-in `ollama`.
    #[serde(default)]
    pub providers: BTreeMap<String, ProviderSpec>,
    /// The ordered list of roles.
    ///
    /// Defaults to empty so a config with no roles parses and fails with a
    /// clear "no roles defined" validation error instead of a raw "missing
    /// field `roles`" parse error.
    #[serde(default)]
    pub roles: Vec<RoleSpec>,
}

fn default_max_steps() -> u32 {
    DEFAULT_MAX_STEPS
}

fn default_max_loop() -> u32 {
    DEFAULT_MAX_LOOP
}

impl NightshiftConfig {
    /// Validate the graph's internal consistency.
    ///
    /// Semantic errors use [`Error::RoleGraph`] so the operator gets a clear,
    /// actionable message rather than a generic parse failure.
    pub fn validate(&self) -> Result<(), Error> {
        if self.roles.is_empty() {
            return Err(Error::RoleGraph("no roles defined".into()));
        }
        let ids: HashSet<&str> = self.roles.iter().map(|role| role.id.as_str()).collect();
        if ids.len() != self.roles.len() {
            return Err(Error::RoleGraph("duplicate role id".into()));
        }
        if !ids.contains(self.run.start.as_str()) {
            return Err(Error::RoleGraph(format!(
                "start role {:?} is not defined",
                self.run.start
            )));
        }
        if self.run.max_steps == 0 {
            return Err(Error::RoleGraph("max_steps must be at least 1".into()));
        }
        for role in &self.roles {
            if role.provider != "ollama" && !self.providers.contains_key(&role.provider) {
                return Err(Error::RoleGraph(format!(
                    "role {:?} references unknown provider {:?}",
                    role.id, role.provider
                )));
            }
        }
        for role in &self.roles {
            for target in [
                role.on.next.as_ref(),
                role.on.issues.as_ref(),
                role.on.questions.as_ref(),
            ]
            .into_iter()
            .flatten()
            {
                if let Target::Role(role_id) = target {
                    if !ids.contains(role_id.as_str()) {
                        return Err(Error::RoleGraph(format!(
                            "role {:?} routes to unknown role {:?}",
                            role.id, role_id
                        )));
                    }
                }
            }
        }
        const KNOWN_TOOLS: [&str; 3] = ["gather-context", "run-tests", "apply-patch"];
        for role in &self.roles {
            for tool in &role.tools {
                if !KNOWN_TOOLS.contains(&tool.as_str()) {
                    return Err(Error::RoleGraph(format!(
                        "role {:?} declares unknown tool {:?}",
                        role.id, tool
                    )));
                }
            }
        }
        const MAX_CONTEXT_FILES: usize = 10;
        for role in &self.roles {
            if role.context_files.len() > MAX_CONTEXT_FILES {
                return Err(Error::RoleGraph(format!(
                    "role {:?} declares {} context_files entries; limit is {MAX_CONTEXT_FILES}",
                    role.id,
                    role.context_files.len()
                )));
            }
            for file in &role.context_files {
                let path = std::path::Path::new(file);
                if file.is_empty() {
                    return Err(Error::RoleGraph(format!(
                        "role {:?} has an empty context_files entry",
                        role.id
                    )));
                }
                if path.is_absolute() || path.has_root() {
                    return Err(Error::RoleGraph(format!(
                        "role {:?} context_files entry {:?} must be repo-relative, not absolute",
                        role.id, file
                    )));
                }
                if path
                    .components()
                    .any(|c| matches!(c, std::path::Component::ParentDir))
                {
                    return Err(Error::RoleGraph(format!(
                        "role {:?} context_files entry {:?} must not contain '..'",
                        role.id, file
                    )));
                }
                if is_secret_path(file) {
                    return Err(Error::RoleGraph(format!(
                        "role {:?} context_files entry {:?} looks like a secret-bearing \
                         file (.env, *.pem, *.key, .git/config, etc.) — refusing to \
                         inject potentially sensitive content into the model prompt",
                        role.id, file
                    )));
                }
            }
            if !role.context_files.is_empty() && !role.tools.iter().any(|t| t == "gather-context") {
                return Err(Error::RoleGraph(format!(
                    "role {:?} declares context_files but does not include \
                     the \"gather-context\" tool — context_files are only \
                     injected during gather-context",
                    role.id
                )));
            }
        }
        Ok(())
    }
}

/// Reject file paths that commonly hold secrets or credentials.
///
/// Checks path components (not just the full string) so that nested paths
/// like `sub/.env`, `app/.git/config`, and `deploy/.ssh/id_rsa` are caught.
pub(crate) fn is_secret_path(file: &str) -> bool {
    let path = std::path::Path::new(file);
    // Reject any entry containing a `.git` or `.ssh` component.
    let has_sensitive_dir = path.components().any(|c| {
        matches!(c, std::path::Component::Normal(name)
            if name.eq_ignore_ascii_case(".git") || name.eq_ignore_ascii_case(".ssh"))
    });
    if has_sensitive_dir {
        return true;
    }
    // Check the file name for secret-bearing patterns.
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    let lower = name.to_ascii_lowercase();
    lower == ".env"
        || lower.starts_with(".env.")
        || lower.ends_with(".pem")
        || lower.ends_with(".key")
        || lower.ends_with(".p12")
}

/// Load, parse, and validate the role graph from a TOML file.
pub fn load_role_graph_config_from(path: &Path) -> Result<NightshiftConfig, Error> {
    let content = std::fs::read_to_string(path).map_err(|error| {
        Error::from(ConfigError {
            path: path.display().to_string(),
            message: error.to_string(),
        })
    })?;
    let config: NightshiftConfig = toml::from_str(&content).map_err(|error| {
        Error::from(ConfigError {
            path: path.display().to_string(),
            message: error.to_string(),
        })
    })?;
    config.validate()?;
    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXAMPLE: &str = r#"
[run]
start = "product-owner"

[providers.deepseek]
base_url = "https://api.deepseek.com"
api_key_env = "DEEPSEEK_API_KEY"

[providers.kimi]
base_url = "https://api.moonshot.cn/v1"
api_key_env = "MOONSHOT_API_KEY"

[[roles]]
id = "product-owner"
provider = "deepseek"
model = "deepseek-v4-pro"
options = { temperature = 0.2 }
output = "01_brief.md"
on = { continue = "developer", questions = "@halt" }

[[roles]]
id = "developer"
provider = "kimi"
model = "kimi3"
options = { think = true }
output = "02_patch.patch"
tools = ["apply-patch"]
on = { continue = "qa" }

[[roles]]
id = "qa"
provider = "ollama"
model = "phi4"
tools = ["run-tests"]
on = { issues = "developer" }
"#;

    fn parse_example() -> NightshiftConfig {
        toml::from_str(EXAMPLE).expect("example parses")
    }

    #[test]
    fn example_config_parses_and_validates() {
        let config = parse_example();
        config.validate().expect("valid graph");
        assert_eq!(config.run.start, "product-owner");
        assert_eq!(config.roles.len(), 3);
    }

    #[test]
    fn defaults_apply_when_omitted() {
        let config = parse_example();
        assert_eq!(config.run.max_steps, DEFAULT_MAX_STEPS);
        assert_eq!(config.run.on_unclear, OnUnclear::Halt);
        assert!(config.roles.iter().all(|r| r.max_loop == DEFAULT_MAX_LOOP));
    }

    #[test]
    fn options_map_captures_model_specific_values() {
        let config = parse_example();
        let dev = config
            .roles
            .iter()
            .find(|r| r.id == "developer")
            .expect("developer");
        assert_eq!(
            dev.options.get("think").and_then(|v| v.as_bool()),
            Some(true)
        );
        let po = config
            .roles
            .iter()
            .find(|r| r.id == "product-owner")
            .expect("product-owner");
        assert_eq!(
            po.options.get("temperature").and_then(|v| v.as_float()),
            Some(0.2)
        );
    }

    #[test]
    fn continue_defaults_to_done() {
        let config = parse_example();
        let qa = config.roles.iter().find(|r| r.id == "qa").expect("qa");
        assert_eq!(qa.on.continue_target(), Target::Done);
        assert_eq!(qa.on.issues_target(), Target::Role("developer".into()));
    }

    #[test]
    fn missing_start_role_is_an_error() {
        let config: NightshiftConfig = toml::from_str(
            r#"
[run]
start = "nope"
[[roles]]
id = "qa"
provider = "ollama"
model = "phi4"
"#,
        )
        .expect("parse");
        let err = config.validate().expect_err("missing start must fail");
        assert!(err.to_string().contains("start role"), "{err}");
    }

    #[test]
    fn duplicate_role_id_is_an_error() {
        let config: NightshiftConfig = toml::from_str(
            r#"
[run]
start = "qa"
[[roles]]
id = "qa"
provider = "ollama"
model = "phi4"
[[roles]]
id = "qa"
provider = "ollama"
model = "phi4"
"#,
        )
        .expect("parse");
        let err = config.validate().expect_err("duplicate id must fail");
        assert!(err.to_string().contains("duplicate"), "{err}");
    }

    #[test]
    fn unknown_provider_is_an_error() {
        let config: NightshiftConfig = toml::from_str(
            r#"
[run]
start = "qa"
[[roles]]
id = "qa"
provider = "deepseek"
model = "deepseek-chat"
"#,
        )
        .expect("parse");
        let err = config.validate().expect_err("unknown provider must fail");
        assert!(err.to_string().contains("unknown provider"), "{err}");
    }

    #[test]
    fn unknown_routing_target_is_an_error() {
        let config: NightshiftConfig = toml::from_str(
            r#"
[run]
start = "qa"
[[roles]]
id = "qa"
provider = "ollama"
model = "phi4"
on = { issues = "ghost" }
"#,
        )
        .expect("parse");
        let err = config.validate().expect_err("unknown target must fail");
        assert!(err.to_string().contains("unknown role"), "{err}");
    }

    #[test]
    fn zero_max_steps_is_an_error() {
        let config: NightshiftConfig = toml::from_str(
            r#"
[run]
start = "qa"
max_steps = 0
[[roles]]
id = "qa"
provider = "ollama"
model = "phi4"
"#,
        )
        .expect("parse");
        let err = config.validate().expect_err("zero max_steps must fail");
        assert!(err.to_string().contains("max_steps"), "{err}");
    }

    #[test]
    fn empty_roles_is_an_error() {
        let config: NightshiftConfig = toml::from_str("[run]\nstart = \"qa\"\n").expect("parse");
        let err = config.validate().expect_err("empty roles must fail");
        assert!(err.to_string().contains("no roles"), "{err}");
    }

    #[test]
    fn load_role_graph_config_from_reads_and_validates() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("nightshift.toml");
        std::fs::write(&path, EXAMPLE).expect("write");
        let config = load_role_graph_config_from(&path).expect("valid config");
        assert_eq!(config.roles.len(), 3);
    }

    #[test]
    fn load_role_graph_config_from_reports_parse_errors() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("nightshift.toml");
        std::fs::write(&path, "not = valid = toml\n").expect("write");
        let err = load_role_graph_config_from(&path).expect_err("malformed must fail");
        assert!(err.to_string().contains("config error"), "{err}");
    }

    #[test]
    fn shipped_example_config_parses_and_validates() {
        let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_else(|_| ".".into());
        let example = Path::new(&manifest_dir).join("nightshift.toml.example");
        let config = load_role_graph_config_from(&example).expect("example must parse + validate");
        assert_eq!(config.run.start, "product-owner");
        assert_eq!(config.roles.len(), 3);
    }

    #[test]
    fn context_files_parse_from_toml() {
        let config: NightshiftConfig = toml::from_str(
            r#"
[run]
start = "dev"
[[roles]]
id = "dev"
provider = "ollama"
model = "phi4"
context_files = ["public/index.html", "README.md"]
"#,
        )
        .expect("parse");
        let dev = &config.roles[0];
        assert_eq!(dev.context_files, vec!["public/index.html", "README.md"]);
    }

    #[test]
    fn context_files_default_to_empty() {
        let config: NightshiftConfig = toml::from_str(
            r#"
[run]
start = "dev"
[[roles]]
id = "dev"
provider = "ollama"
model = "phi4"
"#,
        )
        .expect("parse");
        assert!(config.roles[0].context_files.is_empty());
    }

    #[test]
    fn context_files_absolute_path_rejected() {
        let config: NightshiftConfig = toml::from_str(
            r#"
[run]
start = "dev"
[[roles]]
id = "dev"
provider = "ollama"
model = "phi4"
context_files = ["/etc/passwd"]
"#,
        )
        .expect("parse");
        let err = config.validate().expect_err("absolute path must fail");
        assert!(err.to_string().contains("absolute"), "{err}");
    }

    #[test]
    fn context_files_parent_dir_rejected() {
        let config: NightshiftConfig = toml::from_str(
            r#"
[run]
start = "dev"
[[roles]]
id = "dev"
provider = "ollama"
model = "phi4"
context_files = ["../escape.txt"]
"#,
        )
        .expect("parse");
        let err = config.validate().expect_err("parent dir must fail");
        assert!(err.to_string().contains("'..'"), "{err}");
    }

    #[test]
    fn context_files_empty_entry_rejected() {
        let config: NightshiftConfig = toml::from_str(
            r#"
[run]
start = "dev"
[[roles]]
id = "dev"
provider = "ollama"
model = "phi4"
context_files = [""]
"#,
        )
        .expect("parse");
        let err = config.validate().expect_err("empty entry must fail");
        assert!(err.to_string().contains("empty"), "{err}");
    }

    #[test]
    fn context_files_valid_relative_path_passes() {
        let config: NightshiftConfig = toml::from_str(
            r#"
[run]
start = "dev"
[[roles]]
id = "dev"
provider = "ollama"
model = "phi4"
context_files = ["public/index.html"]
tools = ["gather-context"]
"#,
        )
        .expect("parse");
        config.validate().expect("valid relative path should pass");
    }

    #[test]
    fn context_files_without_gather_context_rejected() {
        let config: NightshiftConfig = toml::from_str(
            r#"
[run]
start = "dev"
[[roles]]
id = "dev"
provider = "ollama"
model = "phi4"
context_files = ["public/index.html"]
tools = ["apply-patch"]
"#,
        )
        .expect("parse");
        let err = config
            .validate()
            .expect_err("context_files without gather-context must fail");
        assert!(err.to_string().contains("gather-context"), "{err}");
    }

    #[test]
    fn context_files_secret_paths_rejected() {
        for secret in [
            ".env",
            "config.pem",
            "secret.key",
            ".git/config",
            ".ssh/id_rsa",
            "sub/.env",
            "app/.git/config",
            "deploy/.ssh/id_rsa",
            "config/.env.local",
            "certs/server.pem",
        ] {
            let config: NightshiftConfig = toml::from_str(&format!(
                r#"
[run]
start = "dev"
[[roles]]
id = "dev"
provider = "ollama"
model = "phi4"
context_files = ["{secret}"]
tools = ["gather-context"]
"#
            ))
            .expect("parse");
            let err = config.validate().expect_err("secret path must fail");
            assert!(err.to_string().contains("secret"), "for {secret}: {err}");
        }
    }

    #[test]
    fn context_files_dot_prefix_absolute_rejected() {
        // "./.git/config" has no ParentDir but has_root() catches the leading ./
        let config: NightshiftConfig = toml::from_str(
            r#"
[run]
start = "dev"
[[roles]]
id = "dev"
provider = "ollama"
model = "phi4"
context_files = ["./.git/config"]
tools = ["gather-context"]
"#,
        )
        .expect("parse");
        let err = config.validate().expect_err("dot-prefix path must fail");
        // Either caught as absolute (has_root) or as secret path
        let msg = err.to_string();
        assert!(
            msg.contains("absolute") || msg.contains("secret"),
            "should reject ./.git/config: {msg}"
        );
    }

    #[test]
    fn context_files_too_many_entries_rejected() {
        let entries: Vec<String> = (0..11).map(|i| format!("file{i}.html")).collect();
        let entries_toml = entries
            .iter()
            .map(|e| format!("\"{e}\""))
            .collect::<Vec<_>>()
            .join(", ");
        let config: NightshiftConfig = toml::from_str(&format!(
            r#"
[run]
start = "dev"
[[roles]]
id = "dev"
provider = "ollama"
model = "phi4"
context_files = [{entries_toml}]
tools = ["gather-context"]
"#
        ))
        .expect("parse");
        let err = config.validate().expect_err("too many entries must fail");
        assert!(err.to_string().contains("limit"), "{err}");
    }
}
