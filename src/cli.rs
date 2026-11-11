//! Command-line interface.

use clap::{Parser, Subcommand, ValueEnum};
use std::path::PathBuf;

/// Overnight local multi-agent engineering harness.
#[derive(Debug, Parser)]
#[command(name = "nightshift", version, about)]
pub struct Cli {
    /// Ollama HTTP origin used by `doctor` and `run`.
    #[arg(
        long,
        env = "NIGHTSHIFT_OLLAMA_URL",
        default_value = crate::models::DEFAULT_OLLAMA_URL,
        global = true
    )]
    pub ollama_url: String,
    /// Subcommand to run.
    #[command(subcommand)]
    pub command: Command,
}

/// Stop the pipeline after this stage (debug / tests).
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Until {
    /// Product-manager user story only.
    Pm,
    /// User story plus tech spec.
    TechLead,
    /// Spec plus applied working-tree patch (no commit).
    Dev,
    /// Dev plus test loop (max 3) and `04_qa_report.json`.
    Qa,
}

/// Supported commands.
#[derive(Debug, Subcommand, PartialEq, Eq)]
pub enum Command {
    /// Check that the server can run a nightshift job.
    Doctor {
        /// Role-graph config file (default: `nightshift.toml`).
        #[arg(long, default_value = "nightshift.toml")]
        config: PathBuf,
    },
    /// Print the latest QA verdict from the artifact store.
    Status {
        /// Artifact root (default: `./artifacts`).
        #[arg(long, default_value = crate::artifacts::DEFAULT_OUT_DIR)]
        out: PathBuf,
    },
    /// Run one overnight job (or stop early with `--until`).
    Run {
        /// Business goal for the job.
        #[arg(long)]
        goal: String,
        /// Path to the target git checkout.
        #[arg(long)]
        repo: PathBuf,
        /// Directory slug (default: slugified goal).
        #[arg(long)]
        name: Option<String>,
        /// Artifact root (default: `./artifacts`).
        #[arg(long, default_value = crate::artifacts::DEFAULT_OUT_DIR)]
        out: PathBuf,
        /// Allow a dirty target working tree.
        #[arg(long)]
        allow_dirty: bool,
        /// Write `05_article_draft.md` after a passing run (default).
        #[arg(long = "article", default_value_t = true, overrides_with = "article")]
        #[arg(long = "no-article", action = clap::ArgAction::SetFalse)]
        article: bool,
        /// Stop after this stage. Omit to run QA and optionally Writer.
        #[arg(long, value_enum)]
        until: Option<Until>,
    },
    /// Run a role-graph job from a config file (the new harness).
    Harness {
        /// Business goal for the run.
        #[arg(long)]
        goal: String,
        /// Role-graph config file (default: `nightshift.toml`).
        #[arg(long, default_value = "nightshift.toml")]
        config: PathBuf,
        /// Artifact root (default: `./artifacts`).
        #[arg(long, default_value = crate::artifacts::DEFAULT_OUT_DIR)]
        out: PathBuf,
        /// Directory slug (default: slugified goal).
        #[arg(long)]
        name: Option<String>,
        /// Target repo for capabilities (e.g. apply-patch).
        #[arg(long)]
        repo: PathBuf,
    },
    /// Check a model works with the harness: three short tasks, repeated.
    ///
    /// Builds the client the way a run does, so `--provider` can be `ollama`
    /// or any `[providers.*]` entry in the config (rapid-mlx, llama.cpp, LM
    /// Studio). Exits 0 only when every run of every task passes.
    Bench {
        /// Model tag to probe (for example `qwen3.6-35b-4bit`).
        #[arg(long)]
        model: String,
        /// Provider: `ollama`, `deepseek`, `kimi`, or a `[providers.*]` name.
        #[arg(long, default_value = "ollama")]
        provider: String,
        /// Config holding `[providers.*]` (default: `nightshift.toml` if it exists).
        #[arg(long)]
        config: Option<PathBuf>,
        /// Times each task runs.
        #[arg(long, default_value_t = crate::bench::DEFAULT_BENCH_RUNS,
              value_parser = clap::value_parser!(u32).range(1..))]
        runs: u32,
        /// Per-call timeout in seconds.
        #[arg(long, default_value_t = crate::bench::DEFAULT_BENCH_TIMEOUT_SECS,
              value_parser = clap::value_parser!(u64).range(1..))]
        timeout: u64,
    },
    /// Pre-flight: run the entry role and resolve its questions interactively.
    Plan {
        /// Business goal for the run.
        #[arg(long)]
        goal: String,
        /// Role-graph config file (default: `nightshift.toml`).
        #[arg(long, default_value = "nightshift.toml")]
        config: PathBuf,
        /// Artifact root (default: `./artifacts`).
        #[arg(long, default_value = crate::artifacts::DEFAULT_OUT_DIR)]
        out: PathBuf,
        /// Directory slug (default: slugified goal).
        #[arg(long)]
        name: Option<String>,
        /// Target repo for capabilities.
        #[arg(long)]
        repo: PathBuf,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn parses_doctor() {
        let cli = Cli::try_parse_from(["nightshift", "doctor"]).expect("parse");
        match cli.command {
            Command::Doctor { config } => assert_eq!(config, PathBuf::from("nightshift.toml")),
            other => panic!("expected Doctor, got {other:?}"),
        }
    }

    #[test]
    fn parses_ollama_url_after_subcommand() {
        let doctor = Cli::try_parse_from([
            "nightshift",
            "doctor",
            "--ollama-url",
            "http://example.test",
        ])
        .expect("parse URL after doctor");
        assert_eq!(doctor.ollama_url, "http://example.test");
        let run = Cli::try_parse_from([
            "nightshift",
            "run",
            "--goal",
            "x",
            "--repo",
            ".",
            "--ollama-url",
            "http://run.example",
        ])
        .expect("parse URL after run");
        assert_eq!(run.ollama_url, "http://run.example");
    }

    #[test]
    fn parses_status_default_and_out() {
        let cli = Cli::try_parse_from(["nightshift", "status"]).expect("parse");
        match cli.command {
            Command::Status { out } => {
                assert_eq!(out, PathBuf::from("artifacts"));
            }
            other => panic!("expected Status, got {other:?}"),
        }
        let cli = Cli::try_parse_from(["nightshift", "status", "--out", "/tmp/ns"]).expect("parse");
        match cli.command {
            Command::Status { out } => assert_eq!(out, PathBuf::from("/tmp/ns")),
            other => panic!("expected Status, got {other:?}"),
        }
    }

    #[test]
    fn rejects_unknown_command() {
        let err = Cli::try_parse_from(["nightshift", "fly"]).expect_err("unknown");
        let text = err.to_string();
        assert!(text.contains("unrecognized subcommand"), "{text}");
    }

    #[test]
    fn parses_run_until_qa() {
        let cli = Cli::try_parse_from([
            "nightshift",
            "run",
            "--goal",
            "x",
            "--repo",
            ".",
            "--until",
            "qa",
        ])
        .expect("parse");
        match cli.command {
            Command::Run { until, .. } => assert_eq!(until, Some(Until::Qa)),
            other => panic!("expected Run, got {other:?}"),
        }
    }

    #[test]
    fn parses_run_until_dev() {
        let cli = Cli::try_parse_from([
            "nightshift",
            "run",
            "--goal",
            "x",
            "--repo",
            ".",
            "--until",
            "dev",
        ])
        .expect("parse");
        match cli.command {
            Command::Run { until, .. } => assert_eq!(until, Some(Until::Dev)),
            other => panic!("expected Run, got {other:?}"),
        }
    }

    #[test]
    fn parses_run_until_tech_lead() {
        let cli = Cli::try_parse_from([
            "nightshift",
            "run",
            "--goal",
            "x",
            "--repo",
            ".",
            "--until",
            "tech-lead",
        ])
        .expect("parse");
        match cli.command {
            Command::Run { until, .. } => assert_eq!(until, Some(Until::TechLead)),
            other => panic!("expected Run, got {other:?}"),
        }
    }

    #[test]
    fn parses_run_until_pm() {
        let cli = Cli::try_parse_from([
            "nightshift",
            "run",
            "--goal",
            "add status",
            "--repo",
            "/tmp/app",
            "--until",
            "pm",
        ])
        .expect("parse");
        match cli.command {
            Command::Run {
                goal,
                repo,
                name,
                out,
                allow_dirty,
                article,
                until,
            } => {
                assert_eq!(goal, "add status");
                assert_eq!(repo, PathBuf::from("/tmp/app"));
                assert_eq!(name, None);
                assert_eq!(out, PathBuf::from("artifacts"));
                assert!(!allow_dirty);
                assert!(article);
                assert_eq!(until, Some(Until::Pm));
            }
            other => panic!("expected Run, got {other:?}"),
        }
    }

    #[test]
    fn parses_run_no_article_and_name() {
        let cli = Cli::try_parse_from([
            "nightshift",
            "run",
            "--goal",
            "x",
            "--repo",
            ".",
            "--name",
            "my-job",
            "--out",
            "/tmp/ns",
            "--allow-dirty",
            "--no-article",
        ])
        .expect("parse");
        match cli.command {
            Command::Run {
                name,
                out,
                allow_dirty,
                article,
                until,
                ..
            } => {
                assert_eq!(name.as_deref(), Some("my-job"));
                assert_eq!(out, PathBuf::from("/tmp/ns"));
                assert!(allow_dirty);
                assert!(!article);
                assert_eq!(until, None);
            }
            other => panic!("expected Run, got {other:?}"),
        }
    }

    #[test]
    fn run_requires_goal_and_repo() {
        let err = Cli::try_parse_from(["nightshift", "run"]).expect_err("required");
        let text = err.to_string();
        assert!(
            text.contains("required") || text.contains("--goal") || text.contains("--repo"),
            "{text}"
        );
    }

    #[test]
    fn parses_harness() {
        let cli = Cli::try_parse_from([
            "nightshift",
            "harness",
            "--goal",
            "add /health",
            "--repo",
            ".",
        ])
        .expect("parse");
        match cli.command {
            Command::Harness {
                goal,
                config,
                out,
                name,
                repo,
            } => {
                assert_eq!(goal, "add /health");
                assert_eq!(config, PathBuf::from("nightshift.toml"));
                assert_eq!(out, PathBuf::from("artifacts"));
                assert_eq!(name, None);
                assert_eq!(repo, PathBuf::from("."));
            }
            other => panic!("expected Harness, got {other:?}"),
        }
    }

    #[test]
    fn parses_bench_with_defaults() {
        let cli =
            Cli::try_parse_from(["nightshift", "bench", "--model", "llama3.1:8b"]).expect("parse");
        match cli.command {
            Command::Bench {
                model,
                provider,
                config,
                runs,
                timeout,
            } => {
                assert_eq!(model, "llama3.1:8b");
                assert_eq!(provider, "ollama");
                assert_eq!(config, None);
                assert_eq!(runs, 3);
                assert_eq!(timeout, 120);
            }
            other => panic!("expected Bench, got {other:?}"),
        }
    }

    #[test]
    fn parses_bench_provider_runs_and_timeout() {
        let cli = Cli::try_parse_from([
            "nightshift",
            "bench",
            "--model",
            "gemma-4-31b-4bit",
            "--provider",
            "mlx",
            "--config",
            "mac.toml",
            "--runs",
            "5",
            "--timeout",
            "300",
        ])
        .expect("parse");
        match cli.command {
            Command::Bench {
                provider,
                config,
                runs,
                timeout,
                ..
            } => {
                assert_eq!(provider, "mlx");
                assert_eq!(config, Some(PathBuf::from("mac.toml")));
                assert_eq!((runs, timeout), (5, 300));
            }
            other => panic!("expected Bench, got {other:?}"),
        }
    }

    #[test]
    fn bench_rejects_missing_model_and_zero_runs_or_timeout() {
        assert!(Cli::try_parse_from(["nightshift", "bench"]).is_err());
        for flag in ["--runs", "--timeout"] {
            assert!(
                Cli::try_parse_from(["nightshift", "bench", "--model", "m", flag, "0"]).is_err(),
                "{flag} 0 must be rejected"
            );
        }
    }

    #[test]
    fn parses_plan() {
        let cli =
            Cli::try_parse_from(["nightshift", "plan", "--goal", "add /health", "--repo", "."])
                .expect("parse");
        match cli.command {
            Command::Plan {
                goal,
                config,
                out,
                name,
                repo,
            } => {
                assert_eq!(goal, "add /health");
                assert_eq!(config, PathBuf::from("nightshift.toml"));
                assert_eq!(out, PathBuf::from("artifacts"));
                assert_eq!(name, None);
                assert_eq!(repo, PathBuf::from("."));
            }
            other => panic!("expected Plan, got {other:?}"),
        }
    }
}
