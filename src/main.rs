//! `nightshift` CLI entry point.

use clap::Parser;
use rs_nightshift::artifacts::{write_status, ArtifactStore};
use rs_nightshift::cli::{Cli, Command};
use rs_nightshift::doctor::{run_doctor, write_report, HttpModelCatalog, PathHost};
use rs_nightshift::models::DEFAULT_OLLAMA_URL;
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
    match Cli::parse().command {
        Command::Doctor => {
            let catalog = HttpModelCatalog::new(DEFAULT_OLLAMA_URL)?;
            let report = run_doctor(&catalog, &PathHost).await?;
            write_report(&report, io::stdout())?;
            process::exit(report.exit_code());
        }
        Command::Status { out } => {
            let code = write_status(&ArtifactStore::new(out), io::stdout())?;
            process::exit(code);
        }
    }
}
