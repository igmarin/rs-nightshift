//! `nightshift` CLI entry point.

use clap::Parser;
use rs_nightshift::artifacts::{write_status, ArtifactStore};
use rs_nightshift::cli::{Cli, Command};
use rs_nightshift::context::PathProbe;
use rs_nightshift::doctor::{
    run_doctor, write_report, Check, DoctorReport, HttpModelCatalog, PathHost,
};
use rs_nightshift::ollama::{redact_ollama_url, validate_ollama_url, OllamaClient};
use rs_nightshift::pipeline::{local_date, run, RunRequest};
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
            let catalog = HttpModelCatalog::new(&validated_url)?;
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
            let client = OllamaClient::new(&ollama_url)?;
            let run_dir = run(
                &client,
                &ArtifactStore::new(out),
                &local_date()?,
                &RunRequest {
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
    }
    Ok(())
}
