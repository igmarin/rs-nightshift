//! Environment readiness checks (`nightshift doctor`).

mod catalog;
mod host;
mod report;

pub use catalog::{HttpModelCatalog, ModelCatalog};
pub use host::{HostCommands, PathHost};
pub use report::{write_report, Check, DoctorReport};

use crate::error::Error;

/// Run readiness checks against an injected catalog and host.
pub async fn run_doctor<C, H>(catalog: &C, host: &H) -> Result<DoctorReport, Error>
where
    C: ModelCatalog,
    H: HostCommands,
{
    let mut checks = Vec::new();

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

    let config_path = crate::models::config_path();
    match crate::models::load_models_config_from(&config_path) {
        Ok(config) => {
            let overrides = config.role_models.len();
            checks.push(Check {
                name: "config".into(),
                passed: true,
                required: false,
                detail: if overrides == 0 {
                    format!(
                        "{} (no overrides; using default models)",
                        config_path.display()
                    )
                } else {
                    format!(
                        "{} ({} override{})",
                        config_path.display(),
                        overrides,
                        if overrides == 1 { "" } else { "s" }
                    )
                },
            });
        }
        Err(error) => {
            checks.push(Check {
                name: "config".into(),
                passed: false,
                required: false,
                detail: format!("{error}; using default models"),
            });
        }
    }

    match catalog.list_models().await {
        Ok(models) => {
            checks.push(Check {
                name: "ollama".into(),
                passed: true,
                required: true,
                detail: format!("reachable ({} models)", models.len()),
            });
            push_model_checks(&mut checks, &models);
        }
        Err(error) => {
            checks.push(Check {
                name: "ollama".into(),
                passed: false,
                required: true,
                detail: format!("not reachable: {error}"),
            });
            for role in crate::models::required_models() {
                let tag = crate::models::model_for(*role);
                checks.push(Check {
                    name: format!("model:{tag}"),
                    passed: false,
                    required: true,
                    detail: "skipped; Ollama is not reachable".into(),
                });
            }
        }
    }

    for cmd in ["codegraph", "graphify"] {
        let ok = host.command_on_path(cmd);
        checks.push(Check {
            name: cmd.into(),
            passed: ok,
            required: true,
            detail: if ok {
                format!("{cmd} is on PATH")
            } else {
                format!("{cmd} not found on PATH")
            },
        });
    }

    Ok(DoctorReport { checks })
}

fn push_model_checks(checks: &mut Vec<Check>, models: &[String]) {
    for role in crate::models::required_models() {
        let tag = crate::models::model_for(*role);
        let present = models
            .iter()
            .any(|installed| model_matches(installed, &tag));
        checks.push(Check {
            name: format!("model:{tag}"),
            passed: present,
            required: true,
            detail: if present {
                format!("{tag} is installed")
            } else {
                format!("missing {tag}; run `ollama pull {tag}`")
            },
        });
    }
}

fn model_matches(installed: &str, required: &str) -> bool {
    installed == required || installed.starts_with(&format!("{required}-"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::Error;
    use catalog::tests::FakeCatalog;
    use host::tests::{healthy_host, FakeHost};

    fn all_models() -> Vec<String> {
        crate::models::required_models()
            .iter()
            .map(|role| crate::models::model_for(*role).to_string())
            .collect()
    }

    #[tokio::test]
    async fn ollama_unreachable_is_not_ready() {
        let catalog = FakeCatalog {
            result: Err(Error::Ollama("connection refused".into())),
        };
        let report = run_doctor(&catalog, &healthy_host())
            .await
            .expect("doctor should return a report");
        let ollama = report
            .checks
            .iter()
            .find(|c| c.name == "ollama")
            .expect("ollama check");
        assert!(
            !ollama.passed,
            "unreachable Ollama must fail the ollama check"
        );
        assert!(ollama.required);
        assert!(!report.is_ready());
        assert_eq!(report.exit_code(), 2);
    }

    #[tokio::test]
    async fn missing_required_model_is_not_ready() {
        let catalog = FakeCatalog {
            result: Ok(vec!["llama3.2:3b".into()]),
        };
        let report = run_doctor(&catalog, &healthy_host())
            .await
            .expect("doctor should return a report");
        let missing = report
            .checks
            .iter()
            .find(|c| c.name == "model:llama3.1:8b")
            .expect("per-model check for llama3.1:8b");
        assert!(!missing.passed);
        assert!(missing.required);
        assert!(!report.is_ready());
    }

    #[tokio::test]
    async fn missing_rustc_is_not_ready() {
        let catalog = FakeCatalog {
            result: Ok(all_models()),
        };
        let host = FakeHost {
            rustc: false,
            mise: false,
            commands: vec!["codegraph", "graphify"],
        };
        let report = run_doctor(&catalog, &host).await.expect("report");
        let rustc = report
            .checks
            .iter()
            .find(|c| c.name == "rustc")
            .expect("rustc check");
        assert!(!rustc.passed);
        assert!(!report.is_ready());
    }

    #[tokio::test]
    async fn missing_codegraph_is_not_ready() {
        let catalog = FakeCatalog {
            result: Ok(all_models()),
        };
        let host = FakeHost {
            rustc: true,
            mise: true,
            commands: vec!["graphify"],
        };
        let report = run_doctor(&catalog, &host).await.expect("report");
        let check = report
            .checks
            .iter()
            .find(|c| c.name == "codegraph")
            .expect("codegraph check");
        assert!(!check.passed);
        assert!(!report.is_ready());
    }

    #[tokio::test]
    async fn missing_graphify_is_not_ready() {
        let catalog = FakeCatalog {
            result: Ok(all_models()),
        };
        let host = FakeHost {
            rustc: true,
            mise: true,
            commands: vec!["codegraph"],
        };
        let report = run_doctor(&catalog, &host).await.expect("report");
        let check = report
            .checks
            .iter()
            .find(|c| c.name == "graphify")
            .expect("graphify check");
        assert!(!check.passed);
        assert!(!report.is_ready());
    }

    #[tokio::test]
    async fn missing_mise_is_warning_when_rustc_exists() {
        let catalog = FakeCatalog {
            result: Ok(all_models()),
        };
        let host = FakeHost {
            rustc: true,
            mise: false,
            commands: vec!["codegraph", "graphify"],
        };
        let report = run_doctor(&catalog, &host).await.expect("report");
        let mise = report
            .checks
            .iter()
            .find(|c| c.name == "mise")
            .expect("mise check");
        assert!(!mise.passed);
        assert!(!mise.required);
        assert!(report.is_ready());
        assert_eq!(report.exit_code(), 0);
    }

    #[tokio::test]
    async fn healthy_environment_is_ready() {
        let catalog = FakeCatalog {
            result: Ok(all_models()),
        };
        let report = run_doctor(&catalog, &healthy_host()).await.expect("report");
        assert!(report.is_ready(), "{report:?}");
        assert_eq!(report.exit_code(), 0);
        for name in [
            "rustc",
            "mise",
            "ollama",
            "codegraph",
            "graphify",
            "model:llama3.2:3b",
            "model:llama3.1:8b",
            "model:mistral-nemo:12b",
            "model:qwen2.5-coder:7b",
            "model:deepseek-r1:7b",
            "model:gemma2:9b",
            "model:phi3.5:latest",
        ] {
            assert!(
                report.checks.iter().any(|c| c.name == name && c.passed),
                "missing passing check {name} in {:?}",
                report.checks
            );
        }
    }

    #[test]
    fn model_matches_exact_and_quantized_suffix() {
        assert!(model_matches("llama3.1:8b", "llama3.1:8b"));
        assert!(model_matches("llama3.1:8b-q4_0", "llama3.1:8b"));
        assert!(!model_matches("llama3.2:3b", "llama3.1:8b"));
    }
}
