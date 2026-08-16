//! QA report data model and `nightshift status` lookup.

use crate::error::Error;
use serde::{Deserialize, Serialize};
use std::io::Write;

/// QA verdict written as `04_qa_report.json`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum QaStatus {
    /// Tests passed.
    Passed,
    /// Latest test run failed (may still retry).
    Failed,
    /// Loop exhausted; operator must review.
    RequiresHumanReview,
}

/// Contents of `04_qa_report.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QaReport {
    /// Overall verdict.
    pub status: QaStatus,
    /// Dev/QA iteration that produced this report (`1..=3`).
    pub iteration: u8,
    /// Test command that was run.
    pub command: String,
    /// Process exit code.
    pub exit_code: i32,
    /// Short operator-facing summary.
    pub summary: String,
    /// Fix hints for Dev (empty on pass).
    pub fix_hints: String,
}

/// Read `latest/04_qa_report.json` and write a morning summary.
///
/// Returns process exit code `0` when a report is printed, `2` when QA has not run.
pub fn write_status(store: &super::ArtifactStore, mut out: impl Write) -> Result<i32, Error> {
    let latest = store.root().join("latest");
    if !latest.exists() {
        writeln!(out, "QA has not run yet: no artifact run found")?;
        return Ok(2);
    }
    let report_path = latest.join("04_qa_report.json");
    if !report_path.is_file() {
        writeln!(out, "QA has not run yet")?;
        return Ok(2);
    }
    let bytes = std::fs::read(&report_path)?;
    let report: QaReport =
        serde_json::from_slice(&bytes).map_err(|e| Error::Artifact(e.to_string()))?;
    writeln!(out, "{}", qa_label(report.status))?;
    if !report.summary.is_empty() {
        writeln!(out, "{}", report.summary)?;
    }
    Ok(0)
}

fn qa_label(status: QaStatus) -> &'static str {
    match status {
        QaStatus::Passed => "PASSED",
        QaStatus::Failed => "FAILED",
        QaStatus::RequiresHumanReview => "REQUIRES_HUMAN_REVIEW",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::artifacts::ArtifactStore;

    fn store() -> (tempfile::TempDir, ArtifactStore) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let store = ArtifactStore::new(tmp.path());
        (tmp, store)
    }

    #[test]
    fn status_prints_verdict_when_qa_report_exists() {
        let (_tmp, store) = store();
        let run = store.create_run("2026-08-14", "done").expect("create");
        let report = QaReport {
            status: QaStatus::Passed,
            iteration: 1,
            command: "cargo test".into(),
            exit_code: 0,
            summary: "ok".into(),
            fix_hints: String::new(),
        };
        std::fs::write(
            run.path.join("04_qa_report.json"),
            serde_json::to_vec_pretty(&report).expect("json"),
        )
        .expect("write report");

        let mut buf = Vec::new();
        let code = write_status(&store, &mut buf).expect("status");
        assert_eq!(code, 0);
        let text = String::from_utf8(buf).expect("utf8");
        assert!(text.contains("PASSED"), "{text}");
    }

    #[test]
    fn status_prints_requires_human_review() {
        let (_tmp, store) = store();
        let run = store.create_run("2026-08-14", "stuck").expect("create");
        let report = QaReport {
            status: QaStatus::RequiresHumanReview,
            iteration: 3,
            command: "cargo test".into(),
            exit_code: 1,
            summary: "still failing".into(),
            fix_hints: "check borrow".into(),
        };
        std::fs::write(
            run.path.join("04_qa_report.json"),
            serde_json::to_vec_pretty(&report).expect("json"),
        )
        .expect("write report");

        let mut buf = Vec::new();
        let code = write_status(&store, &mut buf).expect("status");
        assert_eq!(code, 0);
        let text = String::from_utf8(buf).expect("utf8");
        assert!(text.contains("REQUIRES_HUMAN_REVIEW"), "{text}");
    }

    #[test]
    fn status_missing_report_is_not_ready() {
        let (_tmp, store) = store();
        store.create_run("2026-08-14", "early").expect("create");
        let mut buf = Vec::new();
        let code = write_status(&store, &mut buf).expect("status");
        assert_eq!(code, 2);
        let text = String::from_utf8(buf).expect("utf8");
        assert!(
            text.to_lowercase().contains("qa") || text.contains("not"),
            "{text}"
        );
    }

    #[test]
    fn status_without_runs_is_not_ready() {
        let (_tmp, store) = store();
        let mut buf = Vec::new();
        let code = write_status(&store, &mut buf).expect("status");
        assert_eq!(code, 2);
    }
}
