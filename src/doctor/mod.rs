//! Environment readiness checks (`nightshift doctor`).

mod catalog;
mod host;
mod report;

pub use catalog::{HttpModelCatalog, HttpProviderCatalog, ModelCatalog, ProviderCatalog};
pub use host::{HostCommands, PathHost};
pub use report::{write_report, Check, DoctorReport};

use crate::domain::rolegraph::config::{NightshiftConfig, ProviderSpec};
use crate::error::Error;
#[cfg(test)]
use crate::error::ProviderError;
use std::collections::{BTreeMap, BTreeSet};

/// Run readiness checks against the role graph, injected catalogs, and host.
///
/// `catalog` lists Ollama models; `providers` probes every other (OpenAI-
/// compatible) provider a role uses.
pub async fn run_doctor<C, P, H>(
    config: &NightshiftConfig,
    catalog: &C,
    providers: &P,
    host: &H,
) -> Result<DoctorReport, Error>
where
    C: ModelCatalog,
    P: ProviderCatalog,
    H: HostCommands,
{
    let mut checks = Vec::new();

    push_toolchain_checks(&mut checks, host);

    // codegraph/graphify: required only when a role gathers context.
    let uses_context = config
        .roles
        .iter()
        .any(|role| role.tools.iter().any(|tool| tool == "gather-context"));
    for cmd in ["codegraph", "graphify"] {
        let ok = host.command_on_path(cmd);
        checks.push(Check {
            name: cmd.into(),
            passed: ok,
            required: uses_context,
            detail: if ok {
                format!("{cmd} is on PATH")
            } else {
                format!("{cmd} not found on PATH")
            },
        });
    }

    // Ollama reachability + per-model presence (only when a role uses Ollama).
    let ollama_models: BTreeSet<&str> = config
        .roles
        .iter()
        .filter(|role| role.provider == "ollama")
        .map(|role| role.model.as_str())
        .collect();
    if !ollama_models.is_empty() {
        match catalog.list_models().await {
            Ok(installed) => {
                checks.push(Check {
                    name: "ollama".into(),
                    passed: true,
                    required: true,
                    detail: format!("reachable ({} models)", installed.len()),
                });
                for model in &ollama_models {
                    let present = installed.iter().any(|m| model_matches(m, model));
                    checks.push(Check {
                        name: format!("model:{model}"),
                        passed: present,
                        required: true,
                        detail: if present {
                            format!("{model} is installed")
                        } else {
                            format!("missing {model}; run `ollama pull {model}`")
                        },
                    });
                }
            }
            Err(error) => {
                checks.push(Check {
                    name: "ollama".into(),
                    passed: false,
                    required: true,
                    detail: format!("not reachable: {error}"),
                });
                for model in &ollama_models {
                    checks.push(Check {
                        name: format!("model:{model}"),
                        passed: false,
                        required: true,
                        detail: "skipped; Ollama is not reachable".into(),
                    });
                }
            }
        }
    }

    // OpenAI-compatible providers: API key (when one is configured), then
    // reachability and per-model presence via `GET {base_url}/models`.
    let mut remote: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for role in config.roles.iter().filter(|role| role.provider != "ollama") {
        remote
            .entry(role.provider.as_str())
            .or_default()
            .insert(role.model.as_str());
    }
    for (provider, models) in remote {
        let spec = config.providers.get(provider);
        push_provider_checks(&mut checks, provider, spec, &models, providers).await;
    }

    Ok(DoctorReport { checks })
}

fn push_toolchain_checks(checks: &mut Vec<Check>, host: &impl HostCommands) {
    let rustc_ok = host.rustc_available();
    checks.push(Check {
        name: "rustc".into(),
        passed: rustc_ok,
        required: true,
        detail: if rustc_ok {
            "rustc is on PATH".into()
        } else {
            "rustc not found; install via mise or rustup".into()
        },
    });

    let mise_ok = host.mise_available();
    checks.push(Check {
        name: "mise".into(),
        passed: mise_ok,
        required: false,
        detail: if mise_ok {
            "mise is on PATH".into()
        } else {
            "mise not found; rustc via rustup is accepted".into()
        },
    });
}

/// Push `provider:<name>` plus one `model:<name>:<model>` check per model the roles
/// use. Every check is required: a role cannot run without its server.
async fn push_provider_checks<P: ProviderCatalog>(
    checks: &mut Vec<Check>,
    provider: &str,
    spec: Option<&ProviderSpec>,
    models: &BTreeSet<&str>,
    catalog: &P,
) {
    let check = |name: String, passed: bool, detail: String| Check {
        name,
        passed,
        required: true,
        detail,
    };
    let fail = |checks: &mut Vec<Check>, detail: String| {
        checks.push(check(format!("provider:{provider}"), false, detail));
        for model in models {
            checks.push(check(
                format!("model:{provider}:{model}"),
                false,
                format!("skipped; provider {provider:?} is not ready"),
            ));
        }
    };
    // Build the client exactly as a run would, so doctor never passes a
    // provider the factory rejects (backend, base_url, http/keyless rules, key).
    if let Err(error) =
        crate::adapters::providers::build_model_client(provider, spec, &BTreeMap::new())
    {
        return fail(checks, error.to_string());
    }
    let api_key = match api_key_env_for(provider, spec) {
        Some(env) => match std::env::var(&env) {
            Ok(key) => Some(key),
            Err(_) => return fail(checks, format!("missing {env}")),
        },
        None => None,
    };
    let Some(base_url) = base_url_for(provider, spec) else {
        return fail(
            checks,
            format!("define base_url for provider {provider:?} in [providers.{provider}]"),
        );
    };
    // Scheme, host and port only: a path segment could carry a token.
    let origin = reqwest::Url::parse(&base_url)
        .map(|url| url.origin().ascii_serialization())
        .unwrap_or_else(|_| "<invalid base_url>".into());
    // Probe with the same bearer token the run client sends.
    let token = api_key
        .as_deref()
        .unwrap_or(crate::adapters::providers::NO_API_KEY);
    match catalog.list_models(&base_url, Some(token)).await {
        Ok(served) => {
            checks.push(check(
                format!("provider:{provider}"),
                true,
                format!("reachable at {origin} ({} models)", served.len()),
            ));
            for model in models {
                let present = served.iter().any(|m| model_matches(m, model));
                let detail = if present {
                    format!("{model} is served by {provider}")
                } else {
                    format!(
                        "{model} is not served by {provider}; available: {}",
                        served.join(", ")
                    )
                };
                checks.push(check(format!("model:{provider}:{model}"), present, detail));
            }
        }
        Err(error) => fail(checks, format!("not reachable at {origin}: {error}")),
    }
}

/// Base URL for an OpenAI-compatible provider: the spec override, then the
/// built-in `deepseek` / `kimi` defaults.
fn base_url_for(provider: &str, spec: Option<&ProviderSpec>) -> Option<String> {
    if let Some(url) = spec.and_then(|s| s.base_url.clone()) {
        return Some(url);
    }
    match provider {
        "deepseek" => Some(crate::adapters::DEFAULT_DEEPSEEK_BASE_URL.to_string()),
        "kimi" => Some(crate::adapters::DEFAULT_KIMI_BASE_URL.to_string()),
        _ => None,
    }
}

/// Resolve the API-key env var for a provider: an explicit spec override wins,
/// then the built-in defaults for `deepseek` and `kimi`, then `None` (a keyless
/// custom provider).
fn api_key_env_for(provider: &str, spec: Option<&ProviderSpec>) -> Option<String> {
    if let Some(env) = spec.and_then(|s| s.api_key_env.clone()) {
        return Some(env);
    }
    match provider {
        "deepseek" => Some(crate::adapters::DEFAULT_DEEPSEEK_API_KEY_ENV.to_string()),
        "kimi" => Some(crate::adapters::DEFAULT_KIMI_API_KEY_ENV.to_string()),
        _ => None,
    }
}

fn model_matches(installed: &str, required: &str) -> bool {
    // Exact tag match only: a `phi4-mini` install must not satisfy `phi4`.
    installed == required
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::Error;
    use catalog::tests::FakeCatalog;
    use host::tests::{healthy_host, FakeHost};

    fn config(toml: &str) -> NightshiftConfig {
        toml::from_str(toml).expect("config parses")
    }

    const OLLAMA_QA: &str = r#"
[run]
start = "qa"
[[roles]]
id = "qa"
provider = "ollama"
model = "phi4"
"#;

    #[tokio::test]
    async fn ollama_unreachable_is_not_ready() {
        let catalog = FakeCatalog {
            result: Err(Error::from(ProviderError::Ollama(
                "connection refused".into(),
            ))),
        };
        let report = run_doctor(&config(OLLAMA_QA), &catalog, &catalog, &healthy_host())
            .await
            .expect("report");
        let ollama = report
            .checks
            .iter()
            .find(|c| c.name == "ollama")
            .expect("ollama check");
        assert!(!ollama.passed);
        assert!(ollama.required);
        assert!(!report.is_ready());
        assert_eq!(report.exit_code(), 2);
    }

    #[tokio::test]
    async fn missing_required_model_is_not_ready() {
        let catalog = FakeCatalog {
            result: Ok(vec!["llama3.2:3b".into()]),
        };
        let report = run_doctor(&config(OLLAMA_QA), &catalog, &catalog, &healthy_host())
            .await
            .expect("report");
        let missing = report
            .checks
            .iter()
            .find(|c| c.name == "model:phi4")
            .expect("phi4 check");
        assert!(!missing.passed);
        assert!(missing.required);
        assert!(!report.is_ready());
    }

    #[tokio::test]
    async fn missing_rustc_is_not_ready() {
        let catalog = FakeCatalog {
            result: Ok(vec!["phi4".into()]),
        };
        let host = FakeHost {
            rustc: false,
            mise: false,
            commands: vec!["codegraph", "graphify"],
        };
        let report = run_doctor(&config(OLLAMA_QA), &catalog, &catalog, &host)
            .await
            .expect("report");
        assert!(
            !report
                .checks
                .iter()
                .find(|c| c.name == "rustc")
                .expect("rustc")
                .passed
        );
        assert!(!report.is_ready());
    }

    #[tokio::test]
    async fn codegraph_is_required_only_for_gather_context() {
        let catalog = FakeCatalog {
            result: Ok(vec!["phi4".into()]),
        };
        let host = FakeHost {
            rustc: true,
            mise: true,
            commands: vec!["graphify"],
        };
        // No gather-context → codegraph is not required.
        let report = run_doctor(&config(OLLAMA_QA), &catalog, &catalog, &host)
            .await
            .expect("report");
        let codegraph = report
            .checks
            .iter()
            .find(|c| c.name == "codegraph")
            .expect("codegraph");
        assert!(!codegraph.required);
        assert!(report.is_ready());

        // gather-context → codegraph required and failing.
        let cfg = config(
            r#"
[run]
start = "qa"
[[roles]]
id = "qa"
provider = "ollama"
model = "phi4"
tools = ["gather-context"]
"#,
        );
        let report = run_doctor(&cfg, &catalog, &catalog, &host)
            .await
            .expect("report");
        let codegraph = report
            .checks
            .iter()
            .find(|c| c.name == "codegraph")
            .expect("codegraph");
        assert!(codegraph.required);
        assert!(!report.is_ready());
    }

    #[tokio::test]
    async fn missing_api_key_is_not_ready() {
        let catalog = FakeCatalog {
            result: Ok(Vec::new()),
        };
        let cfg = config(
            r#"
[run]
start = "po"
[providers.deepseek]
api_key_env = "NIGHTSHIFT_DOCTOR_UNSET_KEY"
[[roles]]
id = "po"
provider = "deepseek"
model = "deepseek-v4-pro"
"#,
        );
        let report = run_doctor(&cfg, &catalog, &catalog, &healthy_host())
            .await
            .expect("report");
        let check = report
            .checks
            .iter()
            .find(|c| c.name == "provider:deepseek")
            .expect("provider check");
        assert!(!check.passed);
        assert!(!report.is_ready());
    }

    const MLX_DEV: &str = r#"
[run]
start = "dev"
[providers.mlx]
backend = "openai-compatible"
base_url = "http://127.0.0.1:8000/v1"
[[roles]]
id = "dev"
provider = "mlx"
model = "qwen3.6-35b-4bit"
"#;

    #[tokio::test]
    async fn keyless_local_provider_serving_the_model_is_ready() {
        let catalog = FakeCatalog {
            result: Ok(vec!["qwen3.6-35b-4bit".into(), "gemma-4-31b-4bit".into()]),
        };
        let report = run_doctor(&config(MLX_DEV), &catalog, &catalog, &healthy_host())
            .await
            .expect("report");
        let provider = report
            .checks
            .iter()
            .find(|c| c.name == "provider:mlx")
            .expect("provider check");
        assert!(provider.passed, "{provider:?}");
        assert!(provider.detail.contains("127.0.0.1:8000"), "{provider:?}");
        assert!(report.is_ready(), "{report:?}");
    }

    #[tokio::test]
    async fn local_provider_missing_model_lists_served_models() {
        let catalog = FakeCatalog {
            result: Ok(vec!["gemma-4-31b-4bit".into()]),
        };
        let report = run_doctor(&config(MLX_DEV), &catalog, &catalog, &healthy_host())
            .await
            .expect("report");
        let model = report
            .checks
            .iter()
            .find(|c| c.name == "model:mlx:qwen3.6-35b-4bit")
            .expect("model check");
        assert!(!model.passed);
        assert!(model.detail.contains("gemma-4-31b-4bit"), "{model:?}");
        assert!(!report.is_ready());
    }

    #[tokio::test]
    async fn unreachable_local_provider_is_not_ready() {
        let catalog = FakeCatalog {
            result: Err(Error::from(ProviderError::Ollama(
                "connection refused".into(),
            ))),
        };
        let report = run_doctor(&config(MLX_DEV), &catalog, &catalog, &healthy_host())
            .await
            .expect("report");
        let provider = report
            .checks
            .iter()
            .find(|c| c.name == "provider:mlx")
            .expect("provider check");
        assert!(!provider.passed);
        assert!(
            provider.detail.contains("connection refused"),
            "{provider:?}"
        );
        let model = report
            .checks
            .iter()
            .find(|c| c.name == "model:mlx:qwen3.6-35b-4bit")
            .expect("model check");
        assert!(model.detail.starts_with("skipped"), "{model:?}");
        assert_eq!(report.exit_code(), 2);
    }

    #[tokio::test]
    async fn custom_provider_without_base_url_is_not_ready() {
        let cfg = config(
            r#"
[run]
start = "dev"
[providers.mlx]
backend = "openai-compatible"
[[roles]]
id = "dev"
provider = "mlx"
model = "qwen3.6-35b-4bit"
"#,
        );
        let catalog = FakeCatalog {
            result: Ok(Vec::new()),
        };
        let report = run_doctor(&cfg, &catalog, &catalog, &healthy_host())
            .await
            .expect("report");
        let provider = report
            .checks
            .iter()
            .find(|c| c.name == "provider:mlx")
            .expect("provider check");
        assert!(!provider.passed);
        assert!(provider.detail.contains("base_url"), "{provider:?}");
    }

    #[tokio::test]
    async fn custom_provider_with_unsupported_backend_is_not_ready() {
        let catalog = FakeCatalog {
            result: Ok(vec!["qwen3.6-35b-4bit".into()]),
        };
        for cfg in [
            MLX_DEV.replace("backend = \"openai-compatible\"\n", ""),
            MLX_DEV.replace("openai-compatible", "anthropic"),
        ] {
            let report = run_doctor(&config(&cfg), &catalog, &catalog, &healthy_host())
                .await
                .expect("report");
            let provider = report
                .checks
                .iter()
                .find(|c| c.name == "provider:mlx")
                .expect("provider check");
            assert!(!provider.passed, "{cfg}");
            assert!(provider.detail.contains("backend"), "{provider:?}");
            assert!(!report.is_ready());
        }
    }

    /// Records the token each probe sends.
    struct RecordingCatalog(std::sync::Mutex<Vec<Option<String>>>);

    #[async_trait::async_trait]
    impl ProviderCatalog for RecordingCatalog {
        async fn list_models(
            &self,
            _base_url: &str,
            api_key: Option<&str>,
        ) -> Result<Vec<String>, Error> {
            self.0
                .lock()
                .expect("lock")
                .push(api_key.map(str::to_owned));
            Ok(vec!["qwen3.6-35b-4bit".into()])
        }
    }

    #[tokio::test]
    async fn keyless_probe_sends_the_run_placeholder_token() {
        let ollama = FakeCatalog {
            result: Ok(Vec::new()),
        };
        let probe = RecordingCatalog(std::sync::Mutex::new(Vec::new()));
        run_doctor(&config(MLX_DEV), &ollama, &probe, &healthy_host())
            .await
            .expect("report");
        let sent = probe.0.lock().expect("lock").clone();
        assert_eq!(sent, vec![Some("local".to_string())]);
    }

    #[tokio::test]
    async fn keyless_remote_provider_is_not_ready() {
        let cfg = config(
            r#"
[run]
start = "dev"
[providers.remote]
backend = "openai-compatible"
base_url = "https://llm.example.test/v1/secret-token"
[[roles]]
id = "dev"
provider = "remote"
model = "m"
"#,
        );
        let catalog = FakeCatalog {
            result: Ok(vec!["m".into()]),
        };
        let report = run_doctor(&cfg, &catalog, &catalog, &healthy_host())
            .await
            .expect("report");
        let provider = report
            .checks
            .iter()
            .find(|c| c.name == "provider:remote")
            .expect("provider check");
        assert!(!provider.passed);
        assert!(provider.detail.contains("api_key_env"), "{provider:?}");
    }

    #[tokio::test]
    async fn provider_detail_shows_origin_without_path() {
        let cfg = config(&MLX_DEV.replace("/v1", "/v1/secret-token"));
        let catalog = FakeCatalog {
            result: Ok(vec!["qwen3.6-35b-4bit".into()]),
        };
        let report = run_doctor(&cfg, &catalog, &catalog, &healthy_host())
            .await
            .expect("report");
        let provider = report
            .checks
            .iter()
            .find(|c| c.name == "provider:mlx")
            .expect("provider check");
        assert!(
            provider.detail.contains("http://127.0.0.1:8000"),
            "{provider:?}"
        );
        assert!(!provider.detail.contains("secret-token"), "{provider:?}");
    }

    #[tokio::test]
    async fn healthy_environment_is_ready() {
        let catalog = FakeCatalog {
            result: Ok(vec!["phi4".into()]),
        };
        let report = run_doctor(&config(OLLAMA_QA), &catalog, &catalog, &healthy_host())
            .await
            .expect("report");
        assert!(report.is_ready(), "{report:?}");
        assert_eq!(report.exit_code(), 0);
    }

    #[test]
    fn model_matches_is_exact() {
        assert!(model_matches("llama3.1:8b", "llama3.1:8b"));
        assert!(!model_matches("llama3.1:8b-q4_0", "llama3.1:8b"));
        assert!(!model_matches("phi4-mini", "phi4"));
        assert!(!model_matches("llama3.2:3b", "llama3.1:8b"));
    }
}
