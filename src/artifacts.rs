//! Dated run directories and morning `status` lookup.

use crate::error::Error;
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::PathBuf;

/// Default artifact root relative to the process CWD.
pub const DEFAULT_OUT_DIR: &str = "artifacts";

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

/// One overnight run folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunDir {
    /// Absolute path to `YYYY-MM-DD_<slug>/`.
    pub path: PathBuf,
}

/// Root `artifacts/` directory.
#[derive(Debug, Clone)]
pub struct ArtifactStore {
    root: PathBuf,
}

impl ArtifactStore {
    /// Store under `root` (created on first run).
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// Artifact root path.
    #[must_use]
    pub fn root(&self) -> &std::path::Path {
        &self.root
    }

    /// Create `YYYY-MM-DD_<slug>/`, seed state files, and point `latest` at it.
    pub fn create_run(&self, date: &str, slug: &str) -> Result<RunDir, Error> {
        std::fs::create_dir_all(&self.root)?;
        let slug = slugify(slug);
        let mut dir_name = format!("{date}_{slug}");
        let mut path = self.root.join(&dir_name);
        let mut suffix = 2;
        while path.exists() {
            dir_name = format!("{date}_{slug}-{suffix}");
            path = self.root.join(&dir_name);
            suffix += 1;
        }
        std::fs::create_dir_all(&path)?;
        write_pipeline_state(&path)?;
        std::fs::write(path.join("run.log"), b"")?;
        update_latest(&self.root, &dir_name)?;
        Ok(RunDir { path })
    }
}

#[derive(Serialize)]
struct PipelineState<'a> {
    stage: &'a str,
    iteration: u8,
    last_error: Option<&'a str>,
}

fn write_pipeline_state(run: &std::path::Path) -> Result<(), Error> {
    let state = PipelineState {
        stage: "created",
        iteration: 0,
        last_error: None,
    };
    let bytes = serde_json::to_vec_pretty(&state).map_err(|e| Error::Artifact(e.to_string()))?;
    std::fs::write(run.join("pipeline_state.json"), bytes)?;
    Ok(())
}

fn update_latest(root: &std::path::Path, dir_name: &str) -> Result<(), Error> {
    let latest = root.join("latest");
    match std::fs::remove_file(&latest) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    std::os::unix::fs::symlink(dir_name, latest)?;
    Ok(())
}

/// Sanitize a `--name` or goal into a directory slug.
#[must_use]
pub fn slugify(raw: &str) -> String {
    let mut out = String::new();
    let mut prev_dash = false;
    for ch in raw.trim().chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            prev_dash = false;
        } else if !prev_dash {
            out.push('-');
            prev_dash = true;
        }
    }
    let trimmed = out.trim_matches('-');
    if trimmed.is_empty() {
        return "task".into();
    }
    let mut slug = trimmed.chars().take(40).collect::<String>();
    while slug.ends_with('-') {
        slug.pop();
    }
    if slug.is_empty() {
        "task".into()
    } else {
        slug
    }
}

/// Read `latest/04_qa_report.json` and write a morning summary.
///
/// Returns process exit code `0` when a report is printed, `2` when QA has not run.
pub fn write_status(store: &ArtifactStore, mut out: impl Write) -> Result<i32, Error> {
    let latest = store.root.join("latest");
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
    use std::fs;

    fn store() -> (tempfile::TempDir, ArtifactStore) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let store = ArtifactStore::new(tmp.path());
        (tmp, store)
    }

    #[test]
    fn slugify_sanitizes_goal_text() {
        assert_eq!(
            slugify("Implement rate limiting!"),
            "implement-rate-limiting"
        );
        assert_eq!(slugify("  Hello   World  "), "hello-world");
        assert_eq!(slugify(""), "task");
        assert!(slugify(&"x".repeat(80)).len() <= 40);
    }

    #[test]
    fn create_run_writes_dated_dir_state_and_latest() {
        let (_tmp, store) = store();
        let run = store
            .create_run("2026-08-14", "rate-limit")
            .expect("create");
        assert_eq!(
            run.path.file_name().and_then(|n| n.to_str()),
            Some("2026-08-14_rate-limit")
        );
        assert!(run.path.join("pipeline_state.json").is_file());
        assert!(run.path.join("run.log").is_file());
        let latest = store.root.join("latest");
        let meta = fs::symlink_metadata(&latest).expect("latest");
        assert!(meta.file_type().is_symlink(), "latest must be a symlink");
        let target = fs::read_link(&latest).expect("readlink");
        assert_eq!(target, PathBuf::from("2026-08-14_rate-limit"));
    }

    #[test]
    fn second_run_moves_latest_symlink() {
        let (_tmp, store) = store();
        store.create_run("2026-08-14", "one").expect("first");
        store.create_run("2026-08-14", "two").expect("second");
        let target = fs::read_link(store.root.join("latest")).expect("readlink");
        assert_eq!(target, PathBuf::from("2026-08-14_two"));
        assert!(store.root.join("2026-08-14_one").is_dir());
        assert!(store.root.join("2026-08-14_two").is_dir());
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
        fs::write(
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
        fs::write(
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
