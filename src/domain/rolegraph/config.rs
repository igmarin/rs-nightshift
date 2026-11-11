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

/// Default per-role generate timeout in seconds when `timeout_secs` is omitted.
pub const DEFAULT_GENERATE_TIMEOUT_SECS: u64 = 3600;

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
    /// Default `[profiles.<name>]` for `@slot` models. `--profile` and
    /// `NIGHTSHIFT_PROFILE` override it. After
    /// [`NightshiftConfig::resolve_profile`], holds the profile in use.
    #[serde(default)]
    pub profile: Option<String>,
}

/// What a model slot resolves to in one profile:
/// `coder = { provider = "mlx", model = "qwen3.6-35b-4bit" }`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelSlot {
    /// Provider name; `ollama` is built in, others must be in `providers`.
    pub provider: String,
    /// Model tag/variant passed through to the provider verbatim.
    pub model: String,
    /// Model options; a role's own `options` override these key by key.
    #[serde(default)]
    pub options: BTreeMap<String, toml::Value>,
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
    /// Omitted when `model` is an `@slot` (the profile supplies it).
    #[serde(default)]
    pub provider: String,
    /// Model tag/variant passed through to the provider verbatim, or
    /// `@slot` to take provider, model and options from the selected profile.
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
    /// Code-side tools the role declares (`apply-patch`, `search-replace`, …).
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
    /// Per-role generate timeout in seconds.
    ///
    /// When omitted, the generate path falls back to
    /// [`DEFAULT_GENERATE_TIMEOUT_SECS`]. `timeout_secs = 0` is rejected at
    /// validation.
    #[serde(default)]
    pub timeout_secs: Option<u64>,
}

/// The parsed `nightshift.toml` role graph.
#[derive(Debug, Clone, Deserialize)]
pub struct NightshiftConfig {
    /// Run-level options.
    pub run: RunOptions,
    /// Named providers beyond the built-in `ollama`.
    #[serde(default)]
    pub providers: BTreeMap<String, ProviderSpec>,
    /// Named profiles mapping model slots to concrete models, one per
    /// machine (`[profiles.mac]`, `[profiles.linux]`).
    #[serde(default)]
    pub profiles: BTreeMap<String, BTreeMap<String, ModelSlot>>,
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
    /// Replace every `@slot` model with the provider, model and options the
    /// selected profile maps it to.
    ///
    /// `selected` (from `--profile` / `NIGHTSHIFT_PROFILE`) wins over
    /// `[run] profile`. On success `run.profile` holds the profile in use.
    /// Configs without `@slot` models are unchanged and ignore the selection
    /// (a machine-wide `NIGHTSHIFT_PROFILE` must not break them). On error
    /// `self` is left untouched.
    pub fn resolve_profile(&mut self, selected: Option<&str>) -> Result<(), Error> {
        if !self.roles.iter().any(|role| role.model.starts_with('@')) {
            self.run.profile = None;
            return Ok(());
        }
        let name = self.profile_name(selected);
        let profile = self.selected_profile(selected)?;
        let mut roles = self.roles.clone();
        for role in &mut roles {
            let Some(slot_name) = role.model.strip_prefix('@') else {
                continue;
            };
            let Some(profile) = profile else {
                return Err(Error::RoleGraph(format!(
                    "role {:?} uses model slot \"@{slot_name}\" but no profile is selected; \
                     pass --profile, set NIGHTSHIFT_PROFILE, or set [run] profile",
                    role.id
                )));
            };
            let Some(slot) = profile.get(slot_name) else {
                return Err(Error::RoleGraph(format!(
                    "role {:?} uses model slot \"@{slot_name}\", which profile {:?} does not define",
                    role.id,
                    name.as_deref().unwrap_or_default()
                )));
            };
            if !role.provider.is_empty() {
                return Err(Error::RoleGraph(format!(
                    "role {:?} sets provider {:?} and model slot \"@{slot_name}\"; \
                     remove provider, the profile supplies it",
                    role.id, role.provider
                )));
            }
            let mut options = slot.options.clone();
            options.append(&mut role.options);
            role.provider = slot.provider.clone();
            role.model = slot.model.clone();
            role.options = options;
        }
        self.roles = roles;
        self.run.profile = name;
        Ok(())
    }

    /// The profile in use: `selected` (flag/env) over `[run] profile`.
    #[must_use]
    pub fn profile_name(&self, selected: Option<&str>) -> Option<String> {
        selected
            .map(str::to_owned)
            .or_else(|| self.run.profile.clone())
    }

    fn selected_profile(
        &self,
        selected: Option<&str>,
    ) -> Result<Option<&BTreeMap<String, ModelSlot>>, Error> {
        let Some(name) = self.profile_name(selected) else {
            return Ok(None);
        };
        self.profiles.get(&name).map(Some).ok_or_else(|| {
            let known: Vec<&str> = self.profiles.keys().map(String::as_str).collect();
            Error::RoleGraph(format!(
                "unknown profile {name:?}; defined profiles: [{}]",
                known.join(", ")
            ))
        })
    }

    /// Look up one model slot (without the `@`) in the selected profile.
    pub fn model_slot(&self, selected: Option<&str>, slot: &str) -> Result<&ModelSlot, Error> {
        let profile = self.selected_profile(selected)?.ok_or_else(|| {
            Error::RoleGraph(format!(
                "model slot \"@{slot}\" needs a profile; pass --profile, set \
                 NIGHTSHIFT_PROFILE, or set [run] profile"
            ))
        })?;
        profile.get(slot).ok_or_else(|| {
            Error::RoleGraph(format!(
                "profile {:?} does not define model slot \"@{slot}\"",
                self.profile_name(selected).unwrap_or_default()
            ))
        })
    }

    /// One line per role: `id: provider/model`, for logs and `doctor`.
    #[must_use]
    pub fn role_models(&self) -> Vec<String> {
        self.roles
            .iter()
            .map(|role| format!("{}: {}/{}", role.id, role.provider, role.model))
            .collect()
    }

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
            if role.timeout_secs == Some(0) {
                return Err(Error::RoleGraph(format!(
                    "role {:?} timeout_secs must be at least 1",
                    role.id
                )));
            }
        }
        for role in &self.roles {
            if role.model.starts_with('@') {
                return Err(Error::RoleGraph(format!(
                    "role {:?} model slot {:?} was not resolved against a profile",
                    role.id, role.model
                )));
            }
            if role.provider.is_empty() {
                return Err(Error::RoleGraph(format!(
                    "role {:?} has no provider; set provider or use an @slot model",
                    role.id
                )));
            }
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
        const KNOWN_TOOLS: [&str; 5] = [
            "gather-context",
            "run-tests",
            "apply-patch",
            "write-file",
            "search-replace",
        ];
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
/// Sensitive directory components include `.git`, `.ssh`, `.env`, `.aws`,
/// `.azure`, and `.kube`.
pub(crate) fn is_secret_path(file: &str) -> bool {
    let path = std::path::Path::new(file);
    // Reject any entry containing a sensitive directory component.
    let has_sensitive_dir = path.components().any(|c| {
        matches!(c, std::path::Component::Normal(name)
            if matches!(name.to_str(), Some(n)
                if [".git", ".ssh", ".env", ".aws", ".azure", ".kube"]
                    .contains(&n.to_ascii_lowercase().as_str())))
    });
    if has_sensitive_dir {
        return true;
    }
    // Check the file name for secret-bearing patterns.
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    let lower = name.to_ascii_lowercase();
    // Exact-match credential files.
    const SECRET_NAMES: &[&str] = &[
        ".env",
        ".npmrc",
        ".pypirc",
        ".netrc",
        "credentials.json",
        "service-account.json",
    ];
    if SECRET_NAMES.contains(&lower.as_str()) || lower.starts_with(".env.") {
        return true;
    }
    // SSH private key filenames.
    const SSH_KEYS: &[&str] = &["id_rsa", "id_ed25519", "id_ecdsa", "id_dsa"];
    if SSH_KEYS.contains(&lower.as_str()) {
        return true;
    }
    // Secret-bearing file extensions.
    lower.ends_with(".pem")
        || lower.ends_with(".key")
        || lower.ends_with(".p12")
        || lower.ends_with(".pfx")
        || lower.ends_with(".keystore")
}

/// Read and parse a config file without resolving profiles or validating.
pub fn read_config(path: &Path) -> Result<NightshiftConfig, Error> {
    let content = std::fs::read_to_string(path).map_err(|error| {
        Error::from(ConfigError {
            path: path.display().to_string(),
            message: error.to_string(),
        })
    })?;
    toml::from_str(&content).map_err(|error| {
        Error::from(ConfigError {
            path: path.display().to_string(),
            message: error.to_string(),
        })
    })
}

/// Load, parse, resolve `@slot` models against the selected profile (see
/// [`NightshiftConfig::resolve_profile`]), and validate the role graph.
pub fn load_role_graph_config_from(
    path: &Path,
    profile: Option<&str>,
) -> Result<NightshiftConfig, Error> {
    let mut config = read_config(path)?;
    config.resolve_profile(profile)?;
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
    fn search_replace_is_a_known_tool() {
        let config: NightshiftConfig = toml::from_str(
            r#"
[run]
start = "dev"
[[roles]]
id = "dev"
provider = "ollama"
model = "phi4"
tools = ["search-replace"]
"#,
        )
        .expect("parse");
        config.validate().expect("search-replace is a known tool");
    }

    #[test]
    fn unknown_tool_is_an_error() {
        let config: NightshiftConfig = toml::from_str(
            r#"
[run]
start = "dev"
[[roles]]
id = "dev"
provider = "ollama"
model = "phi4"
tools = ["fly"]
"#,
        )
        .expect("parse");
        let err = config.validate().expect_err("unknown tool must fail");
        assert!(err.to_string().contains("unknown tool"), "{err}");
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
        let config = load_role_graph_config_from(&path, None).expect("valid config");
        assert_eq!(config.roles.len(), 3);
    }

    #[test]
    fn load_role_graph_config_from_reports_parse_errors() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("nightshift.toml");
        std::fs::write(&path, "not = valid = toml\n").expect("write");
        let err = load_role_graph_config_from(&path, None).expect_err("malformed must fail");
        assert!(err.to_string().contains("config error"), "{err}");
    }

    #[test]
    fn shipped_example_config_parses_and_validates() {
        let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_else(|_| ".".into());
        let example = Path::new(&manifest_dir).join("nightshift.toml.example");
        let config =
            load_role_graph_config_from(&example, None).expect("example must parse + validate");
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
            ".npmrc",
            ".netrc",
            "credentials.json",
            "service-account.json",
            "keys/id_ecdsa",
            "cert.pfx",
            "app.keystore",
            "foo/.env/bar",
            "config/.aws/credentials",
            "deploy/.kube/config",
            "ci/.azure/credentials",
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
        // "./.git/config" — has_root() is false on Unix, but the .git
        // component is caught by is_secret_path. On Windows, has_root()
        // catches leading ./ paths. Either way, this must be rejected.
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
        // Either caught as absolute (has_root on Windows) or as secret path
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

    #[test]
    fn timeout_secs_parses_and_validates() {
        let config: NightshiftConfig = toml::from_str(
            r#"
[run]
start = "qa"
[[roles]]
id = "qa"
provider = "ollama"
model = "phi4"
timeout_secs = 1200
"#,
        )
        .expect("parse");
        config.validate().expect("valid timeout_secs");
        assert_eq!(config.roles[0].timeout_secs, Some(1200));
    }

    #[test]
    fn timeout_secs_omitted_is_none() {
        let config = parse_example();
        config.validate().expect("valid graph");
        assert!(config.roles.iter().all(|role| role.timeout_secs.is_none()));
    }

    #[test]
    fn zero_timeout_secs_is_an_error() {
        let config: NightshiftConfig = toml::from_str(
            r#"
[run]
start = "qa"
[[roles]]
id = "qa"
provider = "ollama"
model = "phi4"
timeout_secs = 0
"#,
        )
        .expect("parse");
        let err = config.validate().expect_err("zero timeout_secs must fail");
        assert!(err.to_string().contains("timeout_secs"), "{err}");
    }

    const PROFILES: &str = r#"
[run]
start = "po"

[providers.mlx]
backend = "openai-compatible"
base_url = "http://127.0.0.1:8000/v1"

[profiles.mac]
planner = { provider = "mlx", model = "qwen3.8-27b-4bit", options = { temperature = 0.3, max_tokens = 4096 } }
coder = { provider = "mlx", model = "gemma-4-31b-4bit" }

[profiles.linux]
planner = { provider = "ollama", model = "llama3.1:8b" }
coder = { provider = "ollama", model = "qwen2.5-coder:7b" }

[[roles]]
id = "po"
model = "@planner"
options = { temperature = 0.1 }
on = { next = "dev" }

[[roles]]
id = "dev"
model = "@coder"
on = { next = "qa" }

[[roles]]
id = "qa"
provider = "ollama"
model = "phi4"
"#;

    fn profiles(extra_run: &str) -> NightshiftConfig {
        let text = PROFILES.replace("start = \"po\"", &format!("start = \"po\"\n{extra_run}"));
        toml::from_str(&text).expect("parse")
    }

    fn models(config: &NightshiftConfig) -> Vec<(&str, &str)> {
        config
            .roles
            .iter()
            .map(|r| (r.provider.as_str(), r.model.as_str()))
            .collect()
    }

    #[test]
    fn profile_resolves_every_slot_and_leaves_literal_models() {
        let mut config = profiles("");
        config.resolve_profile(Some("linux")).expect("resolve");
        config.validate().expect("valid");
        assert_eq!(
            models(&config),
            [
                ("ollama", "llama3.1:8b"),
                ("ollama", "qwen2.5-coder:7b"),
                ("ollama", "phi4")
            ]
        );
        assert_eq!(config.run.profile.as_deref(), Some("linux"));
        assert_eq!(
            config.role_models(),
            [
                "po: ollama/llama3.1:8b",
                "dev: ollama/qwen2.5-coder:7b",
                "qa: ollama/phi4"
            ]
        );
    }

    #[test]
    fn selected_profile_beats_run_profile() {
        let mut config = profiles("profile = \"mac\"");
        config.resolve_profile(Some("linux")).expect("resolve");
        assert_eq!(config.roles[0].model, "llama3.1:8b");

        let mut config = profiles("profile = \"mac\"");
        config.resolve_profile(None).expect("resolve");
        assert_eq!(config.roles[0].provider, "mlx");
        assert_eq!(config.run.profile.as_deref(), Some("mac"));
    }

    #[test]
    fn role_options_override_slot_options_key_by_key() {
        let mut config = profiles("");
        config.resolve_profile(Some("mac")).expect("resolve");
        let options = &config.roles[0].options;
        assert_eq!(options["temperature"], toml::Value::Float(0.1));
        assert_eq!(options["max_tokens"], toml::Value::Integer(4096));
    }

    #[test]
    fn literal_config_ignores_an_undefined_selected_profile() {
        let mut config = profiles("profile = \"mac\"");
        for role in &mut config.roles {
            role.provider = "ollama".into();
            role.model = "phi4".into();
        }
        config
            .resolve_profile(Some("not-defined"))
            .expect("no slots");
        assert_eq!(config.run.profile, None);
        assert_eq!(config.roles[0].model, "phi4");
    }

    #[test]
    fn failed_resolution_leaves_roles_untouched() {
        let mut config = profiles("");
        config.roles[1].model = "@missing".into();
        config
            .resolve_profile(Some("mac"))
            .expect_err("missing slot");
        assert_eq!(config.roles[0].model, "@planner");
        assert!(config.roles[0].provider.is_empty());
        assert_eq!(config.run.profile, None);
    }

    #[test]
    fn missing_slot_names_the_role_and_slot() {
        let mut config = profiles("");
        config.roles[1].model = "@missing".into();
        let err = config
            .resolve_profile(Some("mac"))
            .expect_err("missing slot")
            .to_string();
        assert!(
            err.contains("\"dev\"") && err.contains("@missing") && err.contains("mac"),
            "{err}"
        );
    }

    #[test]
    fn slot_without_a_profile_is_an_error() {
        let err = profiles("")
            .resolve_profile(None)
            .expect_err("no profile")
            .to_string();
        assert!(err.contains("\"po\"") && err.contains("--profile"), "{err}");
    }

    #[test]
    fn unknown_profile_lists_the_defined_ones() {
        let err = profiles("")
            .resolve_profile(Some("windows"))
            .expect_err("unknown")
            .to_string();
        assert!(
            err.contains("\"windows\"") && err.contains("[linux, mac]"),
            "{err}"
        );
    }

    #[test]
    fn provider_plus_slot_is_an_error() {
        let mut config = profiles("");
        config.roles[1].provider = "ollama".into();
        let err = config
            .resolve_profile(Some("mac"))
            .expect_err("both")
            .to_string();
        assert!(err.contains("remove provider"), "{err}");
    }

    #[test]
    fn unresolved_slot_or_missing_provider_fails_validation() {
        let config = profiles("");
        let err = config.validate().expect_err("unresolved").to_string();
        assert!(err.contains("not resolved"), "{err}");

        let mut config = profiles("");
        config.resolve_profile(Some("mac")).expect("resolve");
        config.roles[2].provider = String::new();
        let err = config.validate().expect_err("no provider").to_string();
        assert!(err.contains("has no provider"), "{err}");
    }

    #[test]
    fn unknown_slot_field_is_a_parse_error() {
        let text = PROFILES.replace(
            "coder = { provider = \"mlx\", model = \"gemma-4-31b-4bit\" }",
            "coder = { provider = \"mlx\", modle = \"gemma-4-31b-4bit\" }",
        );
        assert!(toml::from_str::<NightshiftConfig>(&text).is_err());
    }

    #[test]
    fn model_slot_looks_up_the_selected_profile() {
        let config = profiles("profile = \"mac\"");
        assert_eq!(
            config.model_slot(None, "coder").expect("slot").model,
            "gemma-4-31b-4bit"
        );
        assert_eq!(
            config
                .model_slot(Some("linux"), "coder")
                .expect("slot")
                .provider,
            "ollama"
        );
        assert!(config.model_slot(None, "nope").is_err());
        assert!(profiles("").model_slot(None, "coder").is_err());
    }

    #[test]
    fn load_resolves_the_profile_from_the_file() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("nightshift.toml");
        std::fs::write(&path, PROFILES).expect("write");
        let config = load_role_graph_config_from(&path, Some("mac")).expect("load");
        assert_eq!(config.roles[1].model, "gemma-4-31b-4bit");
        assert!(load_role_graph_config_from(&path, None).is_err());
        assert_eq!(read_config(&path).expect("read").roles[1].model, "@coder");
    }
}
