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
    Doctor,
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
        /// Target repo for capabilities (default: current directory).
        #[arg(long, default_value = ".")]
        repo: PathBuf,
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
        /// Target repo (default: current directory).
        #[arg(long, default_value = ".")]
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
        assert_eq!(cli.command, Command::Doctor);
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
            "--config",
            "nightshift.toml",
        ])
        .expect("parse");
        match cli.command {
            Command::Harness { goal, config, .. } => {
                assert_eq!(goal, "add /health");
                assert_eq!(config, PathBuf::from("nightshift.toml"));
            }
            other => panic!("expected Harness, got {other:?}"),
        }
    }

    #[test]
    fn parses_plan() {
        let cli =
            Cli::try_parse_from(["nightshift", "plan", "--goal", "add /health"]).expect("parse");
        match cli.command {
            Command::Plan { goal, .. } => assert_eq!(goal, "add /health"),
            other => panic!("expected Plan, got {other:?}"),
        }
    }
}
