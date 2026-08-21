//! Environment readiness checks (`nightshift doctor`).

mod catalog;
mod host;
mod report;

pub use catalog::{HttpModelCatalog, ModelCatalog};
pub use host::{HostCommands, PathHost};
pub use report::{write_report, Check, DoctorReport};

use crate::domain::rolegraph::config::{NightshiftConfig, ProviderSpec};
use crate::error::Error;
#[cfg(test)]
use crate::error::ProviderError;
use std::collections::BTreeSet;

/// Run readiness checks against the role graph, an injected catalog, and host.
pub async fn run_doctor<C, H>(
    config: &NightshiftConfig,
    catalog: &C,
    host: &H,
) -> Result<DoctorReport, Error>
where
    C: ModelCatalog,
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

    // API-key checks for remote providers (one check per provider).
    let mut seen = BTreeSet::new();
    for role in &config.roles {
        if role.provider == "ollama" || !seen.insert(role.provider.clone()) {
            continue;
        }
        let spec = config.providers.get(&role.provider);
        match api_key_env_for(&role.provider, spec) {
            Some(env) => {
                let set = std::env::var(&env).is_ok();
                checks.push(Check {
                    name: format!("provider:{}", role.provider),
                    passed: set,
                    required: true,
                    detail: if set {
                        format!("{env} is set")
                    } else {
                        format!("missing {env}")
                    },
                });
            }
            None => checks.push(Check {
                name: format!("provider:{}", role.provider),
                passed: false,
                required: true,
                detail: format!("define api_key_env for provider {:?}", role.provider),
            }),
        }
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

/// Resolve the API-key env var for a provider: an explicit spec override wins,
/// then the built-in defaults for `deepseek` and `kimi`, then `None` for custom
/// providers that must declare `api_key_env`.
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
        let report = run_doctor(&config(OLLAMA_QA), &catalog, &healthy_host())
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
        let report = run_doctor(&config(OLLAMA_QA), &catalog, &healthy_host())
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
        let report = run_doctor(&config(OLLAMA_QA), &catalog, &host)
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
        let report = run_doctor(&config(OLLAMA_QA), &catalog, &host)
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
        let report = run_doctor(&cfg, &catalog, &host).await.expect("report");
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
        let report = run_doctor(&cfg, &catalog, &healthy_host())
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

    #[tokio::test]
    async fn healthy_environment_is_ready() {
        let catalog = FakeCatalog {
            result: Ok(vec!["phi4".into()]),
        };
        let report = run_doctor(&config(OLLAMA_QA), &catalog, &healthy_host())
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
