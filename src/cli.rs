//! Command-line interface.

use clap::{Parser, Subcommand};
use std::path::PathBuf;

/// Overnight local multi-agent engineering harness.
#[derive(Debug, Parser)]
#[command(name = "nightshift", version, about)]
pub struct Cli {
    /// Subcommand to run.
    #[command(subcommand)]
    pub command: Command,
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
}
