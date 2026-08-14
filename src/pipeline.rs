//! Overnight pipeline orchestration.

use crate::artifacts::{ArtifactStore, RunDir};
use crate::cli::Until;
use crate::error::Error;
use crate::generate::Generator;
use std::path::{Path, PathBuf};

/// Arguments for one `nightshift run`.
#[derive(Debug, Clone)]
pub struct RunRequest {
    /// Operator-written business goal.
    pub goal: String,
    /// Target git checkout.
    pub repo: PathBuf,
    /// Optional artifact slug (defaults to the goal).
    pub name: Option<String>,
    /// Allow a dirty target tree (enforced in a later task).
    pub allow_dirty: bool,
    /// Write the Writer article after PASSED (later task).
    pub article: bool,
    /// Optional early stop.
    pub until: Option<Until>,
}

/// Run implemented stages. This slice supports `--until pm` only.
pub async fn run<G: Generator>(
    generator: &G,
    store: &ArtifactStore,
    date: &str,
    request: &RunRequest,
) -> Result<RunDir, Error> {
    if request.goal.trim().is_empty() {
        return Err(Error::Artifact("goal must not be empty".into()));
    }
    match request.until {
        Some(Until::Pm) => {}
        None => {
            return Err(Error::Artifact(
                "full pipeline is not implemented yet; pass --until pm".into(),
            ));
        }
    }
    if !repo_exists(&request.repo) {
        return Err(Error::Artifact(format!(
            "repo is not a directory: {}",
            request.repo.display()
        )));
    }
    let _ = (request.allow_dirty, request.article);
    let slug = request.name.as_deref().unwrap_or(request.goal.as_str());
    let run = store.create_run(date, slug)?;
    run.append_log("stage=pm")?;
    match crate::pm::write_user_story(generator, &run, &request.goal).await {
        Ok(()) => {
            run.write_state("pm", 0, None)?;
            run.append_log("stage=pm done")?;
            Ok(run)
        }
        Err(error) => {
            let msg = error.to_string();
            run.write_state("pm", 0, Some(&msg))?;
            run.append_log(&format!("stage=pm failed: {msg}"))?;
            Err(error)
        }
    }
}

/// Local calendar date as `YYYY-MM-DD` (`date +%Y-%m-%d` on Unix).
pub fn local_date() -> Result<String, Error> {
    let output = std::process::Command::new("date")
        .arg("+%Y-%m-%d")
        .output()?;
    if !output.status.success() {
        return Err(Error::Artifact("failed to read local date".into()));
    }
    let text = String::from_utf8(output.stdout)
        .map_err(|e| Error::Artifact(e.to_string()))?
        .trim()
        .to_string();
    Ok(text)
}

/// True when `repo` is an existing directory.
pub fn repo_exists(repo: &Path) -> bool {
    repo.is_dir()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::generate::ScriptedGenerator;
    use crate::pm::{USER_STORY_FILE, USER_STORY_HEADINGS};

    fn complete_story() -> String {
        let mut body = String::from("# Story\n");
        for heading in USER_STORY_HEADINGS {
            body.push_str(&format!("\n## {heading}\nbody\n"));
        }
        body
    }

    #[tokio::test]
    async fn run_until_pm_writes_story_and_latest() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let repo = tmp.path().join("repo");
        std::fs::create_dir(&repo).expect("repo");
        let out = tmp.path().join("artifacts");
        let store = ArtifactStore::new(&out);
        let gen = ScriptedGenerator::new();
        gen.push_text(complete_story());
        let run = run(
            &gen,
            &store,
            "2026-08-14",
            &RunRequest {
                goal: "add status command".into(),
                repo,
                name: Some("status".into()),
                allow_dirty: false,
                article: true,
                until: Some(Until::Pm),
            },
        )
        .await
        .expect("run");
        assert!(run.path.join(USER_STORY_FILE).is_file());
        assert!(out.join("latest").exists());
        let state = std::fs::read_to_string(run.path.join("pipeline_state.json")).expect("state");
        assert!(state.contains("\"pm\"") || state.contains("pm"), "{state}");
        assert_eq!(gen.calls().len(), 1);
    }

    #[tokio::test]
    async fn run_rejects_empty_goal() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let repo = tmp.path().join("repo");
        std::fs::create_dir(&repo).expect("repo");
        let store = ArtifactStore::new(tmp.path().join("artifacts"));
        let gen = ScriptedGenerator::new();
        let err = run(
            &gen,
            &store,
            "2026-08-14",
            &RunRequest {
                goal: "   ".into(),
                repo,
                name: None,
                allow_dirty: false,
                article: true,
                until: Some(Until::Pm),
            },
        )
        .await
        .expect_err("empty goal");
        match err {
            Error::Artifact(msg) => assert!(msg.contains("empty"), "{msg}"),
            other => panic!("expected Artifact, got {other:?}"),
        }
        assert!(!tmp.path().join("artifacts").exists());
    }

    #[tokio::test]
    async fn run_rejects_missing_until() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let repo = tmp.path().join("repo");
        std::fs::create_dir(&repo).expect("repo");
        let store = ArtifactStore::new(tmp.path().join("artifacts"));
        let gen = ScriptedGenerator::new();
        let err = run(
            &gen,
            &store,
            "2026-08-14",
            &RunRequest {
                goal: "x".into(),
                repo,
                name: None,
                allow_dirty: false,
                article: true,
                until: None,
            },
        )
        .await
        .expect_err("full run");
        match err {
            Error::Artifact(msg) => {
                assert!(msg.contains("--until pm"), "{msg}");
            }
            other => panic!("expected Artifact, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn run_records_pm_failure_in_state() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let repo = tmp.path().join("repo");
        std::fs::create_dir(&repo).expect("repo");
        let store = ArtifactStore::new(tmp.path().join("artifacts"));
        let gen = ScriptedGenerator::new();
        gen.push_text("no headings");
        gen.push_text("still no headings");
        let err = run(
            &gen,
            &store,
            "2026-08-14",
            &RunRequest {
                goal: "x".into(),
                repo,
                name: Some("fail".into()),
                allow_dirty: false,
                article: true,
                until: Some(Until::Pm),
            },
        )
        .await
        .expect_err("invalid story");
        assert!(matches!(err, Error::InvalidArtifact { .. }));
        let latest = store.root().join("latest").join("pipeline_state.json");
        let state = std::fs::read_to_string(latest).expect("state");
        assert!(state.contains("\"pm\""), "{state}");
        assert!(state.contains("last_error"), "{state}");
        assert!(state.contains("01_user_story.md"), "{state}");
    }

    #[tokio::test]
    async fn run_rejects_missing_repo() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let store = ArtifactStore::new(tmp.path().join("artifacts"));
        let gen = ScriptedGenerator::new();
        let err = run(
            &gen,
            &store,
            "2026-08-14",
            &RunRequest {
                goal: "x".into(),
                repo: tmp.path().join("no-such-repo"),
                name: None,
                allow_dirty: false,
                article: true,
                until: Some(Until::Pm),
            },
        )
        .await
        .expect_err("missing repo");
        match err {
            Error::Artifact(msg) => assert!(msg.contains("repo"), "{msg}"),
            other => panic!("expected Artifact, got {other:?}"),
        }
        assert!(!tmp.path().join("artifacts").exists());
    }

    #[test]
    fn local_date_is_yyyy_mm_dd() {
        let date = local_date().expect("date");
        assert_eq!(date.len(), 10, "{date}");
        assert_eq!(&date[4..5], "-");
        assert_eq!(&date[7..8], "-");
    }
}
