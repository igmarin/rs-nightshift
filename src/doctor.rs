//! Environment readiness checks (`nightshift doctor`).

use crate::error::Error;
use async_trait::async_trait;

/// One named check in a doctor report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Check {
    /// Stable check identifier (for example `ollama` or `model:llama3.1:8b`).
    pub name: String,
    /// Whether the check passed.
    pub passed: bool,
    /// When true, a failure makes the environment not ready.
    pub required: bool,
    /// Operator-facing detail.
    pub detail: String,
}

/// Full doctor report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DoctorReport {
    /// Ordered checks.
    pub checks: Vec<Check>,
}

impl DoctorReport {
    /// Required checks all passed.
    #[must_use]
    pub fn is_ready(&self) -> bool {
        self.checks.iter().filter(|c| c.required).all(|c| c.passed)
    }

    /// Process exit code: `0` ready, `2` not ready.
    #[must_use]
    pub fn exit_code(&self) -> i32 {
        if self.is_ready() {
            0
        } else {
            2
        }
    }
}

/// Lists models known to an Ollama server.
#[async_trait]
pub trait ModelCatalog: Send + Sync {
    /// Installed model tags (for example `llama3.1:8b`).
    async fn list_models(&self) -> Result<Vec<String>, Error>;
}

/// Host-side command and toolchain probes.
pub trait HostCommands: Send + Sync {
    /// `rustc` is on PATH.
    fn rustc_available(&self) -> bool;

    /// `mise` is on PATH. Missing mise is a warning when `rustc` exists.
    fn mise_available(&self) -> bool;

    /// Named executable is on PATH.
    fn command_on_path(&self, name: &str) -> bool;
}

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

/// Write a human-readable report.
pub fn write_report(report: &DoctorReport, mut out: impl std::io::Write) -> std::io::Result<()> {
    for check in &report.checks {
        let mark = if check.passed {
            "ok"
        } else if check.required {
            "FAIL"
        } else {
            "warn"
        };
        writeln!(out, "[{mark}] {} - {}", check.name, check.detail)?;
    }
    if report.is_ready() {
        writeln!(out, "environment is ready")?;
    } else {
        writeln!(out, "environment is not ready")?;
    }
    Ok(())
}

/// Ollama `/api/tags` client.
pub struct HttpModelCatalog {
    client: reqwest::Client,
    base_url: String,
}

impl HttpModelCatalog {
    /// Build a catalog for `base_url` (no trailing path).
    pub fn new(base_url: impl Into<String>) -> Result<Self, Error> {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .no_proxy()
            .build()
            .map_err(|error| Error::Ollama(error.to_string()))?;
        Ok(Self {
            client,
            base_url: base_url.into(),
        })
    }
}

#[derive(serde::Deserialize)]
struct TagsResponse {
    models: Vec<TagModel>,
}

#[derive(serde::Deserialize)]
struct TagModel {
    name: String,
}

#[async_trait]
impl ModelCatalog for HttpModelCatalog {
    async fn list_models(&self) -> Result<Vec<String>, Error> {
        let url = format!("{}/api/tags", self.base_url.trim_end_matches('/'));
        let response = self
            .client
            .get(url)
            .send()
            .await
            .map_err(|error| Error::Ollama(error.to_string()))?;
        if !response.status().is_success() {
            return Err(Error::Ollama(format!("status {}", response.status())));
        }
        let body: TagsResponse = response
            .json()
            .await
            .map_err(|error| Error::Ollama(error.to_string()))?;
        Ok(body.models.into_iter().map(|model| model.name).collect())
    }
}

/// PATH lookups via the `which` crate.
pub struct PathHost;

impl HostCommands for PathHost {
    fn rustc_available(&self) -> bool {
        which::which("rustc").is_ok()
    }

    fn mise_available(&self) -> bool {
        which::which("mise").is_ok()
    }

    fn command_on_path(&self, name: &str) -> bool {
        which::which(name).is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeCatalog {
        result: Result<Vec<String>, Error>,
    }

    #[async_trait]
    impl ModelCatalog for FakeCatalog {
        async fn list_models(&self) -> Result<Vec<String>, Error> {
            match &self.result {
                Ok(models) => Ok(models.clone()),
                Err(Error::Ollama(msg)) => Err(Error::Ollama(msg.clone())),
                Err(Error::ModelNotFound { model }) => Err(Error::ModelNotFound {
                    model: model.clone(),
                }),
                Err(Error::Timeout) => Err(Error::Timeout),
                Err(Error::Artifact(msg)) => Err(Error::Artifact(msg.clone())),
                Err(Error::InvalidArtifact { artifact, reason }) => Err(Error::InvalidArtifact {
                    artifact,
                    reason: reason.clone(),
                }),
                Err(Error::Context(msg)) => Err(Error::Context(msg.clone())),
                Err(Error::Git(msg)) => Err(Error::Git(msg.clone())),
                Err(Error::Io(e)) => Err(Error::Io(std::io::Error::new(e.kind(), e.to_string()))),
                Err(Error::Config { path, message }) => Err(Error::Config {
                    path: path.clone(),
                    message: message.clone(),
                }),
            }
        }
    }

    struct FakeHost {
        rustc: bool,
        mise: bool,
        commands: Vec<&'static str>,
    }

    impl HostCommands for FakeHost {
        fn rustc_available(&self) -> bool {
            self.rustc
        }

        fn mise_available(&self) -> bool {
            self.mise
        }

        fn command_on_path(&self, name: &str) -> bool {
            self.commands.contains(&name)
        }
    }

    fn healthy_host() -> FakeHost {
        FakeHost {
            rustc: true,
            mise: true,
            commands: vec!["codegraph", "graphify"],
        }
    }

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
    fn write_report_marks_failures_and_warnings() {
        let report = DoctorReport {
            checks: vec![
                Check {
                    name: "rustc".into(),
                    passed: true,
                    required: true,
                    detail: "ok rustc".into(),
                },
                Check {
                    name: "mise".into(),
                    passed: false,
                    required: false,
                    detail: "no mise".into(),
                },
                Check {
                    name: "ollama".into(),
                    passed: false,
                    required: true,
                    detail: "down".into(),
                },
            ],
        };
        let mut buf = Vec::new();
        write_report(&report, &mut buf).expect("write");
        let text = String::from_utf8(buf).expect("utf8");
        assert!(text.contains("[ok] rustc"));
        assert!(text.contains("[warn] mise"));
        assert!(text.contains("[FAIL] ollama"));
        assert!(text.contains("environment is not ready"));
    }

    #[test]
    fn write_report_ready_message() {
        let report = DoctorReport {
            checks: vec![Check {
                name: "rustc".into(),
                passed: true,
                required: true,
                detail: "ok".into(),
            }],
        };
        let mut buf = Vec::new();
        write_report(&report, &mut buf).expect("write");
        let text = String::from_utf8(buf).expect("utf8");
        assert!(text.contains("environment is ready"));
    }

    #[test]
    fn model_matches_exact_and_quantized_suffix() {
        assert!(model_matches("llama3.1:8b", "llama3.1:8b"));
        assert!(model_matches("llama3.1:8b-q4_0", "llama3.1:8b"));
        assert!(!model_matches("llama3.2:3b", "llama3.1:8b"));
    }

    #[test]
    fn path_host_probes_common_binaries() {
        assert!(PathHost.rustc_available());
        assert!(!PathHost.command_on_path("definitely-not-a-nightshift-binary"));
        let _ = PathHost.mise_available();
    }

    #[tokio::test]
    async fn http_catalog_lists_tags() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/api/tags"))
            .respond_with(
                wiremock::ResponseTemplate::new(200)
                    .set_body_raw(r#"{"models":[{"name":"llama3.1:8b"}]}"#, "application/json"),
            )
            .mount(&server)
            .await;

        let catalog = HttpModelCatalog::new(server.uri()).expect("catalog");
        let models = catalog.list_models().await.expect("tags");
        assert_eq!(models, vec!["llama3.1:8b".to_string()]);
    }

    #[tokio::test]
    async fn http_catalog_maps_invalid_json() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/api/tags"))
            .respond_with(
                wiremock::ResponseTemplate::new(200).set_body_raw("not-json", "application/json"),
            )
            .mount(&server)
            .await;

        let catalog = HttpModelCatalog::new(server.uri()).expect("catalog");
        let err = catalog.list_models().await.expect_err("json");
        assert!(matches!(err, Error::Ollama(_)));
    }

    #[tokio::test]
    async fn http_catalog_maps_http_error() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/api/tags"))
            .respond_with(wiremock::ResponseTemplate::new(503))
            .mount(&server)
            .await;

        let catalog = HttpModelCatalog::new(server.uri()).expect("catalog");
        let err = catalog.list_models().await.expect_err("status");
        match err {
            Error::Ollama(msg) => assert!(msg.contains("503"), "{msg}"),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[tokio::test]
    async fn http_catalog_maps_connection_error() {
        let catalog = HttpModelCatalog::new("http://127.0.0.1:9").expect("catalog");
        let err = catalog.list_models().await.expect_err("connect");
        assert!(matches!(err, Error::Ollama(_)));
    }
}
