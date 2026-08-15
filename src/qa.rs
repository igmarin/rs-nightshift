//! QA stage: run tests, write `04_qa_report.json`, cap the Dev loop at 3.

use crate::artifacts::{QaReport, QaStatus, RunDir};
use crate::error::Error;
use crate::generate::{Generator, ROLE_TEMPERATURE};
use crate::models::{model_for, Role};
use crate::testrun::{format_command, TestOutcome};

/// Artifact written by QA.
pub const QA_REPORT_FILE: &str = "04_qa_report.json";

/// INV-2: Dev ↔ QA at most this many times.
pub const MAX_ITERATIONS: u8 = 3;

/// Truncate test logs before they reach the QA model or `run.log`.
pub const LOG_CAP: usize = 32 * 1024;

/// Truncate `text` to [`LOG_CAP`] bytes on a char boundary.
#[must_use]
pub fn truncate_log(text: &str) -> String {
    if text.len() <= LOG_CAP {
        return text.to_string();
    }
    let mut end = LOG_CAP;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    let mut out = text[..end].to_string();
    out.push_str("\n…[truncated]");
    out
}

/// Persist `04_qa_report.json`.
pub fn write_qa_report(run: &RunDir, report: &QaReport) -> Result<(), Error> {
    let bytes = serde_json::to_vec_pretty(report).map_err(|e| Error::Artifact(e.to_string()))?;
    std::fs::write(run.path.join(QA_REPORT_FILE), bytes)?;
    Ok(())
}

/// Read a previously written QA report.
pub fn read_qa_report(run: &RunDir) -> Result<QaReport, Error> {
    let bytes = std::fs::read(run.path.join(QA_REPORT_FILE))?;
    serde_json::from_slice(&bytes).map_err(|e| Error::Artifact(e.to_string()))
}

/// Build a report from a test outcome (no model).
#[must_use]
pub fn report_from_outcome(
    outcome: &TestOutcome,
    iteration: u8,
    status: QaStatus,
    fix_hints: String,
) -> QaReport {
    QaReport {
        status,
        iteration,
        command: format_command(&outcome.command),
        exit_code: outcome.exit_code,
        summary: if outcome.exit_code == 0 {
            "tests passed".into()
        } else {
            format!("tests failed (exit {})", outcome.exit_code)
        },
        fix_hints,
    }
}

/// Ask the QA model for fix hints. Test argv is not taken from the model (INV-9).
pub async fn fix_hints<G: Generator>(
    generator: &G,
    outcome: &TestOutcome,
) -> Result<String, Error> {
    let log = truncate_log(&outcome.output);
    let prompt = format!(
        "Tests failed with exit {}.\n\
         Command (do not change it): {}\n\
         Log (truncated):\n{log}\n\n\
         Write short fix hints for the developer. Do not invent a new test command.\n",
        outcome.exit_code,
        format_command(&outcome.command),
    );
    generator
        .generate(&model_for(Role::Qa), &prompt, ROLE_TEMPERATURE)
        .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::artifacts::ArtifactStore;
    use crate::generate::ScriptedGenerator;

    #[test]
    fn truncate_log_caps_at_32kib() {
        let big = "x".repeat(LOG_CAP + 50);
        let out = truncate_log(&big);
        assert!(out.len() < big.len());
        assert!(out.contains("truncated"));
    }

    #[test]
    fn write_and_read_round_trip() {
        let tmp = tempfile::tempdir().expect("tmp");
        let run = ArtifactStore::new(tmp.path())
            .create_run("2026-08-14", "qa")
            .expect("run");
        let report = QaReport {
            status: QaStatus::RequiresHumanReview,
            iteration: 3,
            command: "cargo test".into(),
            exit_code: 1,
            summary: "still failing".into(),
            fix_hints: "fix borrow".into(),
        };
        write_qa_report(&run, &report).expect("write");
        let got = read_qa_report(&run).expect("read");
        assert_eq!(got, report);
    }

    #[tokio::test]
    async fn fix_hints_use_qa_model() {
        let gen = ScriptedGenerator::new();
        gen.push_text("check the unwrap");
        let outcome = TestOutcome {
            command: vec!["cargo".into(), "test".into()],
            exit_code: 1,
            output: "panic at foo.rs".into(),
        };
        let hints = fix_hints(&gen, &outcome).await.expect("hints");
        assert_eq!(hints, "check the unwrap");
        let calls = gen.calls();
        assert_eq!(calls[0].model, model_for(Role::Qa));
        assert!(calls[0].prompt.contains("cargo test"));
        assert!(!calls[0].prompt.contains("pytest"));
    }
}
