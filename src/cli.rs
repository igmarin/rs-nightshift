//! Command-line interface.

use clap::{Parser, Subcommand};

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
    fn rejects_unknown_command() {
        let err = Cli::try_parse_from(["nightshift", "fly"]).expect_err("unknown");
        let text = err.to_string();
        assert!(text.contains("unrecognized subcommand"), "{text}");
    }
}
