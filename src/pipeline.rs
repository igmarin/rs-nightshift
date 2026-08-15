//! Overnight pipeline orchestration.

use crate::artifacts::{ArtifactStore, RunDir};
use crate::cli::Until;
use crate::context::{gather, ContextProbe};
use crate::dev::{read_tech_spec, working_tree_dirty, write_and_apply_patch};
use crate::error::Error;
use crate::generate::Generator;
use crate::techlead::{impacted_files, read_user_story, write_tech_spec};
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
    /// Stage to stop after (required until the full pipeline exists).
    pub until: Until,
}

/// Run implemented stages through `--until pm` or `--until tech-lead`.
pub async fn run<G, C>(
    generator: &G,
    store: &ArtifactStore,
    date: &str,
    request: &RunRequest,
    context: &C,
) -> Result<RunDir, Error>
where
    G: Generator,
    C: ContextProbe,
{
    if request.goal.trim().is_empty() {
        return Err(Error::Artifact("goal must not be empty".into()));
    }
    match request.until {
        Until::Pm | Until::TechLead | Until::Dev => {}
    }
    if !repo_exists(&request.repo) {
        return Err(Error::Artifact(format!(
            "repo is not a directory: {}",
            request.repo.display()
        )));
    }
    let _ = request.article;
    if matches!(request.until, Until::Dev)
        && working_tree_dirty(&request.repo)?
        && !request.allow_dirty
    {
        return Err(Error::Git(
            "working tree is dirty; pass --allow-dirty or commit/restore first".into(),
        ));
    }
    let slug = request.name.as_deref().unwrap_or(request.goal.as_str());
    let run = store.create_run(date, slug)?;
    run.append_log("stage=pm")?;
    if let Err(error) = crate::pm::write_user_story(generator, &run, &request.goal).await {
        let msg = error.to_string();
        run.write_state("pm", 0, Some(&msg))?;
        run.append_log(&format!("stage=pm failed: {msg}"))?;
        return Err(error);
    }
    run.write_state("pm", 0, None)?;
    run.append_log("stage=pm done")?;
    if request.until == Until::Pm {
        return Ok(run);
    }

    let story = read_user_story(&run)?;
    let bundle = match gather(context, &request.repo, &request.goal) {
        Ok(bundle) => bundle,
        Err(error) => {
            let msg = error.to_string();
            run.write_state("tech-lead", 0, Some(&msg))?;
            run.append_log(&format!("stage=tech-lead failed: {msg}"))?;
            return Err(error);
        }
    };
    for warning in &bundle.warnings {
        run.append_log(&format!("warn: {warning}"))?;
    }
    run.append_log("stage=tech-lead")?;
    if let Err(error) = write_tech_spec(generator, &run, &request.goal, &story, &bundle).await {
        let msg = error.to_string();
        run.write_state("tech-lead", 0, Some(&msg))?;
        run.append_log(&format!("stage=tech-lead failed: {msg}"))?;
        return Err(error);
    }
    run.write_state("tech-lead", 0, None)?;
    run.append_log("stage=tech-lead done")?;
    if request.until == Until::TechLead {
        return Ok(run);
    }

    let spec = read_tech_spec(&run)?;
    let files = impacted_files(&spec);
    run.append_log("stage=dev")?;
    match write_and_apply_patch(generator, &run, &request.repo, &request.goal, &spec, &files).await
    {
        Ok(()) => {
            run.write_state("dev", 0, None)?;
            run.append_log("stage=dev done")?;
            Ok(run)
        }
        Err(error) => {
            let msg = error.to_string();
            run.write_state("dev", 0, Some(&msg))?;
            run.append_log(&format!("stage=dev failed: {msg}"))?;
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
                until: Until::Pm,
            },
            &crate::context::PathProbe,
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
                until: Until::Pm,
            },
            &crate::context::PathProbe,
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
    async fn path_escaping_name_stays_inside_artifacts() {
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
                goal: "x".into(),
                repo,
                name: Some("../../outside".into()),
                allow_dirty: false,
                article: true,
                until: Until::Pm,
            },
            &crate::context::PathProbe,
        )
        .await
        .expect("run");
        assert!(
            run.path.starts_with(&out),
            "run dir escaped artifacts: {}",
            run.path.display()
        );
        assert!(!tmp.path().join("outside").exists());
        assert_eq!(
            run.path.file_name().and_then(|n| n.to_str()),
            Some("2026-08-14_outside")
        );
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
                until: Until::Pm,
            },
            &crate::context::PathProbe,
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
                until: Until::Pm,
            },
            &crate::context::PathProbe,
        )
        .await
        .expect_err("missing repo");
        match err {
            Error::Artifact(msg) => assert!(msg.contains("repo"), "{msg}"),
            other => panic!("expected Artifact, got {other:?}"),
        }
        assert!(!tmp.path().join("artifacts").exists());
    }

    struct TlProbe;

    impl crate::context::ContextProbe for TlProbe {
        fn codegraph_available(&self) -> bool {
            true
        }
        fn graphify_available(&self) -> bool {
            false
        }
        fn has_codegraph_index(&self, _repo: &Path) -> bool {
            true
        }
        fn has_graphify_graph(&self, _repo: &Path) -> bool {
            false
        }
        fn run_codegraph(&self, _repo: &Path, _args: &[&str]) -> Result<String, Error> {
            Ok("src/cli.rs src/pipeline.rs".into())
        }
        fn run_graphify(&self, _repo: &Path, _args: &[&str]) -> Result<String, Error> {
            Err(Error::Context("graphify unused".into()))
        }
    }

    #[tokio::test]
    async fn run_until_tech_lead_writes_spec() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let repo = tmp.path().join("repo");
        std::fs::create_dir(&repo).expect("repo");
        let out = tmp.path().join("artifacts");
        let store = ArtifactStore::new(&out);
        let gen = ScriptedGenerator::new();
        gen.push_text(complete_story());
        gen.push_text(
            r#"
## Impacted files
- src/cli.rs
- src/pipeline.rs

## Interfaces / signatures
run()

## TDD plan
write a failing test

## Out of scope
apply the patch
"#,
        );
        let run = run(
            &gen,
            &store,
            "2026-08-14",
            &RunRequest {
                goal: "add status".into(),
                repo,
                name: Some("tl".into()),
                allow_dirty: false,
                article: true,
                until: Until::TechLead,
            },
            &TlProbe,
        )
        .await
        .expect("run");
        assert!(run.path.join(crate::pm::USER_STORY_FILE).is_file());
        assert!(run.path.join(crate::techlead::TECH_SPEC_FILE).is_file());
        let state = std::fs::read_to_string(run.path.join("pipeline_state.json")).expect("state");
        assert!(state.contains("tech-lead"), "{state}");
        assert_eq!(gen.calls().len(), 2);
    }

    fn git_init(repo: &Path) {
        assert!(std::process::Command::new("git")
            .args(["init", "-b", "main"])
            .current_dir(repo)
            .status()
            .expect("init")
            .success());
        let _ = std::process::Command::new("git")
            .args(["config", "user.email", "dev@example.com"])
            .current_dir(repo)
            .status();
        let _ = std::process::Command::new("git")
            .args(["config", "user.name", "Dev"])
            .current_dir(repo)
            .status();
        std::fs::write(repo.join("hello.txt"), "hello\n").expect("hello");
        assert!(std::process::Command::new("git")
            .args(["add", "hello.txt"])
            .current_dir(repo)
            .status()
            .expect("add")
            .success());
        assert!(std::process::Command::new("git")
            .args(["commit", "-m", "init"])
            .current_dir(repo)
            .status()
            .expect("commit")
            .success());
    }

    struct HelloProbe;

    impl crate::context::ContextProbe for HelloProbe {
        fn codegraph_available(&self) -> bool {
            true
        }
        fn graphify_available(&self) -> bool {
            false
        }
        fn has_codegraph_index(&self, _repo: &Path) -> bool {
            true
        }
        fn has_graphify_graph(&self, _repo: &Path) -> bool {
            false
        }
        fn run_codegraph(&self, _repo: &Path, _args: &[&str]) -> Result<String, Error> {
            Ok("hello.txt".into())
        }
        fn run_graphify(&self, _repo: &Path, _args: &[&str]) -> Result<String, Error> {
            Err(Error::Context("unused".into()))
        }
    }

    #[tokio::test]
    async fn dirty_tree_without_allow_dirty_is_rejected() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let repo = tmp.path().join("repo");
        std::fs::create_dir(&repo).expect("repo");
        git_init(&repo);
        std::fs::write(repo.join("extra"), "x").expect("dirty");
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
                until: Until::Dev,
            },
            &HelloProbe,
        )
        .await
        .expect_err("dirty");
        match err {
            Error::Git(msg) => assert!(msg.contains("dirty"), "{msg}"),
            other => panic!("expected Git, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn run_until_dev_applies_patch_without_commit() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let repo = tmp.path().join("repo");
        std::fs::create_dir(&repo).expect("repo");
        git_init(&repo);
        let before = crate::dev::head_commit(&repo).expect("head");
        let store = ArtifactStore::new(tmp.path().join("artifacts"));
        let gen = ScriptedGenerator::new();
        gen.push_text(complete_story());
        gen.push_text(
            r#"
## Impacted files
- hello.txt

## Interfaces / signatures
hello

## TDD plan
n/a

## Out of scope
commit
"#,
        );
        gen.push_text(
            "\
diff --git a/hello.txt b/hello.txt
--- a/hello.txt
+++ b/hello.txt
@@ -1 +1 @@
-hello
+hello world
",
        );
        let run = run(
            &gen,
            &store,
            "2026-08-14",
            &RunRequest {
                goal: "greet".into(),
                repo: repo.clone(),
                name: Some("dev".into()),
                allow_dirty: false,
                article: true,
                until: Until::Dev,
            },
            &HelloProbe,
        )
        .await
        .expect("run");
        assert!(run.path.join(crate::dev::PATCH_FILE).is_file());
        assert!(crate::dev::working_tree_dirty(&repo).expect("dirty"));
        assert_eq!(crate::dev::head_commit(&repo).expect("head"), before);
    }

    #[test]
    fn local_date_is_yyyy_mm_dd() {
        let date = local_date().expect("date");
        assert_eq!(date.len(), 10, "{date}");
        assert_eq!(&date[4..5], "-");
        assert_eq!(&date[7..8], "-");
    }
}
