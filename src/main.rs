//! `nightshift` CLI entry point.

use clap::Parser;
use rs_nightshift::adapters::artifact_store::FsArtifactStore;
use rs_nightshift::adapters::capabilities::{CapabilityRunner, GraphContextProvider};
use rs_nightshift::adapters::clock::SystemClock;
use rs_nightshift::adapters::context::PathProbe;
use rs_nightshift::adapters::git::working_tree_dirty;
use rs_nightshift::adapters::ollama::{redact_ollama_url, validate_ollama_url, OllamaClient};
use rs_nightshift::adapters::state::FsStateStore;
use rs_nightshift::adapters::test::ProcessTestRunner;
use rs_nightshift::adapters::ProviderFactory;
use rs_nightshift::application::executor::{execute, ExecuteParams, RoleContext};
use rs_nightshift::application::orchestrator::{run_graph, RunRequest, RunResult};
use rs_nightshift::application::report::{block_reason_description, render_report};
use rs_nightshift::artifacts::{slugify, write_status, ArtifactStore};
use rs_nightshift::bench::{render_report as render_bench_report, run_bench, BenchParams};
use rs_nightshift::cli::{Cli, Command};
use rs_nightshift::doctor::{
    run_doctor, write_report, Check, DoctorReport, HttpModelCatalog, HttpProviderCatalog, PathHost,
};
use rs_nightshift::domain::rolegraph::config::{
    load_role_graph_config_from, read_config, NightshiftConfig,
};
use rs_nightshift::domain::rolegraph::state::RunStatus;
use rs_nightshift::domain::rolegraph::verdict::Verdict;
use rs_nightshift::error::Error;
use rs_nightshift::pipeline::{local_date, run, RunRequest as PipelineRunRequest};
use rs_nightshift::ports::{
    ArtifactStore as ArtifactStorePort, Clock, ModelClientFactory, StateStore,
};
use rs_nightshift::queue::{
    read_queue_file, render_report as render_queue_report, run_queue, Job, JobHost, JobOutcome,
    JobStatus,
};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process;

#[tokio::main]
async fn main() {
    if let Err(error) = real_main().await {
        let _ = writeln!(io::stderr(), "{error:#}");
        process::exit(1);
    }
}

async fn real_main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let ollama_url = cli.ollama_url;
    let profile = cli.profile;
    match cli.command {
        Command::Doctor { config } => {
            let validated_url = match validate_ollama_url(&ollama_url) {
                Ok(url) => url,
                Err(error) => {
                    let report = DoctorReport {
                        checks: vec![Check {
                            name: "ollama-url".into(),
                            passed: false,
                            required: true,
                            detail: error.to_string(),
                        }],
                    };
                    write_report(&report, io::stdout())?;
                    process::exit(report.exit_code());
                }
            };
            let role_graph = match load_role_graph_config_from(&config, profile.as_deref()) {
                Ok(config) => config,
                Err(error) => {
                    let report = DoctorReport {
                        checks: vec![Check {
                            name: "config".into(),
                            passed: false,
                            required: true,
                            detail: error.to_string(),
                        }],
                    };
                    write_report(&report, io::stdout())?;
                    process::exit(report.exit_code());
                }
            };
            let catalog = HttpModelCatalog::new(validated_url.as_str())?;
            let providers = HttpProviderCatalog::new()?;
            let mut report = run_doctor(&role_graph, &catalog, &providers, &PathHost).await?;
            report.checks.insert(
                0,
                Check {
                    name: "ollama-url".into(),
                    passed: true,
                    required: true,
                    detail: format!("using {}", redact_ollama_url(&validated_url)),
                },
            );
            report.checks.insert(
                1,
                Check {
                    name: "roles".into(),
                    passed: true,
                    required: false,
                    detail: describe_models(&role_graph),
                },
            );
            write_report(&report, io::stdout())?;
            process::exit(report.exit_code());
        }
        Command::Status { out } => {
            let code = write_status(&ArtifactStore::new(out), io::stdout())?;
            process::exit(code);
        }
        Command::Bench {
            model,
            provider,
            config,
            runs,
            timeout,
        } => {
            let config = config.or_else(|| {
                let default = std::path::PathBuf::from("nightshift.toml");
                default.is_file().then_some(default)
            });
            let cfg = config.as_deref().map(read_config).transpose()?;
            let (provider, model, options) = match model.strip_prefix('@') {
                Some(slot_name) => {
                    if provider.is_some() {
                        anyhow::bail!("--provider cannot be combined with an @slot model");
                    }
                    let cfg = cfg.as_ref().ok_or_else(|| {
                        anyhow::anyhow!("an @slot model needs a config; pass --config")
                    })?;
                    let slot = cfg.model_slot(profile.as_deref(), slot_name)?;
                    (
                        slot.provider.clone(),
                        slot.model.clone(),
                        slot.options.clone(),
                    )
                }
                None => (
                    provider.unwrap_or_else(|| "ollama".into()),
                    model,
                    std::collections::BTreeMap::new(),
                ),
            };
            let providers = cfg.map(|cfg| cfg.providers).unwrap_or_default();
            let client = ProviderFactory.build(&provider, providers.get(&provider), &options)?;
            let params = BenchParams {
                model,
                runs,
                timeout: std::time::Duration::from_secs(timeout),
            };
            let report = run_bench(client.as_ref(), &params).await;
            let origin = client.redacted_origin();
            writeln!(
                io::stdout(),
                "{}",
                render_bench_report(&report, origin.as_deref())
            )?;
            process::exit(if report.all_passed() { 0 } else { 1 });
        }
        Command::Run {
            goal,
            repo,
            name,
            out,
            allow_dirty,
            article,
            until,
        } => {
            let client = OllamaClient::new(ollama_url.as_str())?;
            let run_dir = run(
                &client,
                &ArtifactStore::new(out),
                &local_date()?,
                &PipelineRunRequest {
                    goal,
                    repo,
                    name,
                    allow_dirty,
                    article,
                    until,
                },
                &PathProbe,
                &ProcessTestRunner::default(),
            )
            .await?;
            writeln!(io::stdout(), "{}", run_dir.path.display())?;
        }
        Command::Harness {
            goal,
            config,
            out,
            name,
            repo,
        } => {
            let (cfg, run_dir) =
                prepare_run(&config, profile.as_deref(), &goal, &out, name.as_deref())?;
            let (result, report) = execute_run(&cfg, &run_dir, &repo, &goal, &out).await?;
            writeln!(io::stdout(), "{report}")?;
            process::exit(match result.status {
                RunStatus::Done => 0,
                RunStatus::Failed => 1,
                RunStatus::Blocked | RunStatus::Running => 2,
            });
        }
        Command::Queue {
            file,
            config,
            out,
            stop_on_failure,
        } => {
            let queue = read_queue_file(&file)?;
            let host = HarnessHost {
                config,
                profile,
                out: out.clone(),
            };
            let state = run_queue(&host, &queue.jobs, &out, stop_on_failure).await?;
            writeln!(io::stdout(), "{}", render_queue_report(&state))?;
            process::exit(i32::from(!state.all_done()));
        }
        Command::Plan {
            goal,
            config,
            out,
            name,
            repo,
        } => {
            let cfg = load_role_graph_config_from(&config, profile.as_deref())?;
            writeln!(io::stderr(), "{}", describe_models(&cfg))?;
            let entry = cfg
                .roles
                .iter()
                .find(|role| role.id == cfg.run.start)
                .ok_or_else(|| {
                    Error::RoleGraph(format!("start role {:?} not found", cfg.run.start))
                })?;
            let factory = ProviderFactory;
            let store = FsArtifactStore::new(&out);
            let clock = SystemClock;
            let tools = CapabilityRunner::new();
            let context_provider = GraphContextProvider;
            let slug = run_slug(&clock, name.as_deref(), &goal, "plan");
            let run_dir = store.create_run(&clock.today(), &slug)?;

            let client = factory.build(
                &entry.provider,
                cfg.providers.get(&entry.provider),
                &entry.options,
            )?;
            let mut clarifications: Vec<String> = Vec::new();
            let mut rounds = 0;
            let outcome = loop {
                rounds += 1;
                let ctx = RoleContext {
                    goal: goal.clone(),
                    findings: Vec::new(),
                    questions: Vec::new(),
                    clarifications: clarifications.clone(),
                };
                let params = ExecuteParams {
                    run: &run_dir,
                    repo: &repo,
                    role: entry,
                    context: &ctx,
                    artifacts: &[],
                    // `plan` is interactive and keeps no action log, so a
                    // retry is reported on stderr.
                    on_retry: &|attempt, error| {
                        eprintln!("attempt {attempt} rejected, retrying: {error}");
                        Ok(())
                    },
                };
                let outcome =
                    execute(client.as_ref(), &store, &tools, &context_provider, &params).await?;
                let more = outcome.output.verdict == Verdict::Questions
                    && !outcome.output.questions.is_empty()
                    && rounds < 5;
                if !more {
                    break outcome;
                }
                writeln!(io::stdout(), "The entry role has clarifying questions:")?;
                let mut answers = Vec::new();
                let mut eof = false;
                for question in &outcome.output.questions {
                    writeln!(io::stdout(), "  Q: {}", question.text)?;
                    print!("  A: ");
                    io::stdout().flush()?;
                    let mut line = String::new();
                    if io::stdin().read_line(&mut line)? == 0 {
                        eof = true;
                        break;
                    }
                    answers.push(format!("Q: {}\nA: {}", question.text, line.trim()));
                }
                clarifications.extend(answers);
                if eof {
                    break outcome;
                }
            };
            writeln!(io::stdout(), "{}", outcome.output.summary)?;
            if let Some(artifact) = &outcome.artifact {
                writeln!(io::stdout(), "Wrote {}", artifact)?;
            }
            process::exit(0);
        }
    }
    Ok(())
}

/// Load the config and create the run directory for one harness job.
fn prepare_run(
    config: &Path,
    profile: Option<&str>,
    goal: &str,
    out: &Path,
    name: Option<&str>,
) -> Result<(NightshiftConfig, PathBuf), Error> {
    let cfg = load_role_graph_config_from(config, profile)?;
    writeln!(io::stderr(), "{}", describe_models(&cfg))?;
    let clock = SystemClock;
    let slug = run_slug(&clock, name, goal, "run");
    let run_dir = FsArtifactStore::new(out).create_run(&clock.today(), &slug)?;
    Ok((cfg, run_dir))
}

/// Walk the role graph in `run_dir`; returns the result and the morning report.
async fn execute_run(
    cfg: &NightshiftConfig,
    run_dir: &Path,
    repo: &Path,
    goal: &str,
    out: &Path,
) -> Result<(RunResult, String), Error> {
    let state = FsStateStore::new();
    let result = run_graph(
        &ProviderFactory,
        &FsArtifactStore::new(out),
        &state,
        &SystemClock,
        &CapabilityRunner::new(),
        &GraphContextProvider,
        &RunRequest {
            run: run_dir,
            repo,
            config: cfg,
            goal,
        },
    )
    .await?;
    let snapshot = state.read_snapshot(run_dir)?;
    let events = state.read_actions(run_dir)?;
    Ok((result, render_report(&snapshot, &events)))
}

/// Runs each queue job through the harness, exactly like `nightshift harness`.
struct HarnessHost {
    /// Config for jobs that do not set their own.
    config: PathBuf,
    /// Profile for jobs that do not set their own.
    profile: Option<String>,
    /// Artifact root.
    out: PathBuf,
}

#[async_trait::async_trait(?Send)]
impl JobHost for HarnessHost {
    fn is_dirty(&self, repo: &Path) -> Result<bool, Error> {
        working_tree_dirty(repo)
    }

    async fn run(&self, job: &Job) -> JobOutcome {
        let failed = |error: Error, run_dir: Option<PathBuf>| JobOutcome {
            status: JobStatus::Failed,
            reason: Some(error.to_string()),
            run_dir,
        };
        let config = job.config.as_deref().unwrap_or(&self.config);
        let profile = job.profile.as_deref().or(self.profile.as_deref());
        let (cfg, run_dir) =
            match prepare_run(config, profile, &job.goal, &self.out, job.name.as_deref()) {
                Ok(prepared) => prepared,
                Err(error) => return failed(error, None),
            };
        match execute_run(&cfg, &run_dir, &job.repo, &job.goal, &self.out).await {
            Ok((result, _report)) => {
                let status = match result.status {
                    RunStatus::Done => JobStatus::Done,
                    RunStatus::Blocked => JobStatus::Blocked,
                    RunStatus::Failed | RunStatus::Running => JobStatus::Failed,
                };
                let reason = (status != JobStatus::Done)
                    .then(|| block_reason_description(result.block_reason).to_string());
                JobOutcome {
                    status,
                    reason,
                    run_dir: Some(run_dir),
                }
            }
            Err(error) => failed(error, Some(run_dir)),
        }
    }
}

/// `profile mac: po: mlx/qwen3.8-27b-4bit, dev: mlx/qwen3.6-35b-4bit`.
fn describe_models(cfg: &NightshiftConfig) -> String {
    let profile = cfg
        .run
        .profile
        .as_deref()
        .map_or_else(|| "no profile".to_owned(), |name| format!("profile {name}"));
    format!("{profile}: {}", cfg.role_models().join(", "))
}

/// Build a unique run-directory slug: `{name-or-goal}-{kind}-{timestamp}`.
///
/// The timestamp keeps repeat invocations on the same day isolated; the `kind`
/// distinguishes `run` from `plan` so a plan never shares a directory with a
/// later harness run.
fn run_slug(clock: &SystemClock, name: Option<&str>, goal: &str, kind: &str) -> String {
    let base = name.map(str::to_string).unwrap_or_else(|| slugify(goal));
    let stamp = clock.now_iso().replace(':', "-");
    format!("{base}-{kind}-{stamp}")
}
