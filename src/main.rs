//! `nightshift` CLI entry point.

use clap::Parser;
use rs_nightshift::adapters::artifact_store::FsArtifactStore;
use rs_nightshift::adapters::capabilities::{CapabilityRunner, GraphContextProvider};
use rs_nightshift::adapters::clock::SystemClock;
use rs_nightshift::adapters::state::FsStateStore;
use rs_nightshift::adapters::ProviderFactory;
use rs_nightshift::application::executor::{execute, ExecuteParams, RoleContext};
use rs_nightshift::application::orchestrator::{run_graph, RunRequest};
use rs_nightshift::application::report::render_report;
use rs_nightshift::artifacts::{slugify, write_status, ArtifactStore};
use rs_nightshift::cli::{Cli, Command};
use rs_nightshift::context::PathProbe;
use rs_nightshift::doctor::{
    run_doctor, write_report, Check, DoctorReport, HttpModelCatalog, PathHost,
};
use rs_nightshift::domain::rolegraph::config::load_role_graph_config_from;
use rs_nightshift::domain::rolegraph::state::RunStatus;
use rs_nightshift::domain::rolegraph::verdict::Verdict;
use rs_nightshift::error::Error;
use rs_nightshift::ollama::{redact_ollama_url, validate_ollama_url, OllamaClient};
use rs_nightshift::pipeline::{local_date, run, RunRequest as PipelineRunRequest};
use rs_nightshift::ports::{
    ArtifactStore as ArtifactStorePort, Clock, ModelClientFactory, StateStore,
};
use rs_nightshift::testrun::ProcessTestRunner;
use std::io::{self, Write};
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
    match cli.command {
        Command::Doctor => {
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
            let catalog = HttpModelCatalog::new(validated_url.as_str())?;
            let mut report = run_doctor(&catalog, &PathHost).await?;
            report.checks.insert(
                0,
                Check {
                    name: "ollama-url".into(),
                    passed: true,
                    required: true,
                    detail: format!("using {}", redact_ollama_url(&validated_url)),
                },
            );
            write_report(&report, io::stdout())?;
            process::exit(report.exit_code());
        }
        Command::Status { out } => {
            let code = write_status(&ArtifactStore::new(out), io::stdout())?;
            process::exit(code);
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
            let cfg = load_role_graph_config_from(&config)?;
            let factory = ProviderFactory;
            let store = FsArtifactStore::new(&out);
            let state = FsStateStore::new();
            let clock = SystemClock;
            let tools = CapabilityRunner::new();
            let context_provider = GraphContextProvider;
            let slug = name.unwrap_or_else(|| slugify(&goal));
            let run_dir = store.create_run(&clock.today(), &slug)?;
            let result = run_graph(
                &factory,
                &store,
                &state,
                &clock,
                &tools,
                &context_provider,
                &RunRequest {
                    run: &run_dir,
                    repo: &repo,
                    config: &cfg,
                    goal: &goal,
                },
            )
            .await?;
            let snapshot = state.read_snapshot(&run_dir)?;
            let events = state.read_actions(&run_dir)?;
            writeln!(io::stdout(), "{}", render_report(&snapshot, &events))?;
            process::exit(match result.status {
                RunStatus::Done => 0,
                RunStatus::Failed => 1,
                RunStatus::Blocked | RunStatus::Running => 2,
            });
        }
        Command::Plan {
            goal,
            config,
            out,
            name,
            repo,
        } => {
            let cfg = load_role_graph_config_from(&config)?;
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
            let slug = name.unwrap_or_else(|| slugify(&goal));
            let run_dir = store.create_run(&clock.today(), &slug)?;

            let client = factory.build(
                &entry.provider,
                cfg.providers.get(&entry.provider),
                &entry.options,
            )?;
            let mut clarifications: Vec<String> = Vec::new();
            let outcome = loop {
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
                };
                let outcome =
                    execute(client.as_ref(), &store, &tools, &context_provider, &params).await?;
                if outcome.output.verdict == Verdict::Questions
                    && !outcome.output.questions.is_empty()
                {
                    writeln!(io::stdout(), "The entry role has clarifying questions:")?;
                    let mut answers = Vec::new();
                    for question in &outcome.output.questions {
                        writeln!(io::stdout(), "  Q: {}", question.text)?;
                        print!("  A: ");
                        io::stdout().flush()?;
                        let mut line = String::new();
                        io::stdin().read_line(&mut line)?;
                        answers.push(format!("Q: {}\nA: {}", question.text, line.trim()));
                    }
                    clarifications.extend(answers);
                    continue;
                }
                break outcome;
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
