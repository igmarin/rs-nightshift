//! Overnight pipeline orchestration.

use crate::artifacts::{ArtifactStore, QaStatus, RunDir};
use crate::cli::Until;
use crate::context::{gather, ContextProbe};
use crate::dev::{read_tech_spec, working_tree_dirty, write_and_apply_patch};
use crate::error::Error;
use crate::generate::Generator;
use crate::qa::{fix_hints, report_from_outcome, truncate_log, write_qa_report, MAX_ITERATIONS};
use crate::techlead::{impacted_files, read_user_story, write_tech_spec};
use crate::testrun::{detect_test_command, TestRunner};
use crate::writer::write_article;
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
    /// Optional early stop. `None` runs QA and Writer (when `--article`).
    pub until: Option<Until>,
}

/// Run implemented stages. Omit `until` for QA plus Writer when `--article`.
pub async fn run<G, C, T>(
    generator: &G,
    store: &ArtifactStore,
    date: &str,
    request: &RunRequest,
    context: &C,
    tests: &T,
) -> Result<RunDir, Error>
where
    G: Generator,
    C: ContextProbe,
    T: TestRunner,
{
    if request.goal.trim().is_empty() {
        return Err(Error::Artifact("goal must not be empty".into()));
    }
    match request.until {
        None | Some(Until::Pm | Until::TechLead | Until::Dev | Until::Qa) => {}
    }
    if !repo_exists(&request.repo) {
        return Err(Error::Artifact(format!(
            "repo is not a directory: {}",
            request.repo.display()
        )));
    }
    if matches!(request.until, None | Some(Until::Dev | Until::Qa))
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
    if request.until == Some(Until::Pm) {
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
    if request.until == Some(Until::TechLead) {
        return Ok(run);
    }

    let spec = read_tech_spec(&run)?;
    let files = impacted_files(&spec);
    let test_argv = if matches!(request.until, None | Some(Until::Qa)) {
        Some(detect_test_command(&request.repo)?)
    } else {
        None
    };

    let mut hints = String::new();
    for iteration in 1..=MAX_ITERATIONS {
        run.append_log(&format!("stage=dev iteration={iteration}"))?;
        if let Err(error) = write_and_apply_patch(
            generator,
            &run,
            &request.repo,
            &request.goal,
            &spec,
            &files,
            &hints,
        )
        .await
        {
            let msg = error.to_string();
            run.write_state("dev", iteration, Some(&msg))?;
            run.append_log(&format!("stage=dev failed: {msg}"))?;
            if request.until == Some(Until::Dev) || iteration == MAX_ITERATIONS {
                if matches!(request.until, None | Some(Until::Qa)) {
                    let report = crate::artifacts::QaReport {
                        status: QaStatus::RequiresHumanReview,
                        iteration,
                        command: test_argv
                            .as_ref()
                            .map(|a| crate::testrun::format_command(a))
                            .unwrap_or_default(),
                        exit_code: -1,
                        summary: "dev apply failed".into(),
                        fix_hints: msg.clone(),
                    };
                    write_qa_report(&run, &report)?;
                }
                return Err(error);
            }
            hints = msg;
            continue;
        }
        run.write_state("dev", iteration, None)?;
        run.append_log("stage=dev done")?;
        if request.until == Some(Until::Dev) {
            return Ok(run);
        }

        let argv = test_argv.as_ref().expect("qa requires argv");
        run.append_log(&format!("stage=qa iteration={iteration}"))?;
        let outcome = tests.run(&request.repo, argv)?;
        let log = truncate_log(&outcome.output);
        run.append_log(&format!(
            "tests exit={} bytes={}",
            outcome.exit_code,
            log.len()
        ))?;
        if outcome.exit_code == 0 {
            let report = report_from_outcome(&outcome, iteration, QaStatus::Passed, String::new());
            write_qa_report(&run, &report)?;
            run.write_state("qa", iteration, None)?;
            run.append_log("stage=qa PASSED")?;
            if request.until == Some(Until::Qa) {
                return Ok(run);
            }
            if !request.article {
                return Ok(run);
            }
            run.append_log("stage=writer")?;
            match write_article(generator, &run, &request.goal).await {
                Ok(()) => {
                    run.write_state("writer", iteration, None)?;
                    run.append_log("stage=writer done")?;
                    return Ok(run);
                }
                Err(error) => {
                    let msg = error.to_string();
                    run.write_state("writer", iteration, Some(&msg))?;
                    run.append_log(&format!("stage=writer failed: {msg}"))?;
                    return Err(error);
                }
            }
        }
        if iteration == MAX_ITERATIONS {
            let report = report_from_outcome(
                &outcome,
                iteration,
                QaStatus::RequiresHumanReview,
                hints.clone(),
            );
            write_qa_report(&run, &report)?;
            run.write_state("qa", iteration, Some("REQUIRES_HUMAN_REVIEW"))?;
            run.append_log("stage=qa REQUIRES_HUMAN_REVIEW")?;
            return Err(Error::Artifact("REQUIRES_HUMAN_REVIEW".into()));
        }
        hints = match fix_hints(generator, &outcome).await {
            Ok(text) => text,
            Err(error) => {
                run.append_log(&format!("qa hints failed: {error}"))?;
                log
            }
        };
        let failed = report_from_outcome(&outcome, iteration, QaStatus::Failed, hints.clone());
        write_qa_report(&run, &failed)?;
        run.write_state("qa", iteration, Some("FAILED"))?;
    }
    Err(Error::Artifact("REQUIRES_HUMAN_REVIEW".into()))
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
            &crate::context::PathProbe,
            &crate::testrun::ProcessTestRunner::default(),
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
            &crate::context::PathProbe,
            &crate::testrun::ProcessTestRunner::default(),
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
                until: Some(Until::Pm),
            },
            &crate::context::PathProbe,
            &crate::testrun::ProcessTestRunner::default(),
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
                until: Some(Until::Pm),
            },
            &crate::context::PathProbe,
            &crate::testrun::ProcessTestRunner::default(),
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
            &crate::context::PathProbe,
            &crate::testrun::ProcessTestRunner::default(),
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
                until: Some(Until::TechLead),
            },
            &TlProbe,
            &crate::testrun::ProcessTestRunner::default(),
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
            .args(["add", "."])
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
                until: Some(Until::Dev),
            },
            &HelloProbe,
            &crate::testrun::ProcessTestRunner::default(),
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
                until: Some(Until::Dev),
            },
            &HelloProbe,
            &crate::testrun::ProcessTestRunner::default(),
        )
        .await
        .expect("run");
        assert!(run.path.join(crate::dev::PATCH_FILE).is_file());
        assert!(crate::dev::working_tree_dirty(&repo).expect("dirty"));
        assert_eq!(crate::dev::head_commit(&repo).expect("head"), before);
    }

    fn qa_story_spec_and_patches(gen: &ScriptedGenerator, patches: &[&str]) {
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
        for patch in patches {
            gen.push_text(*patch);
        }
    }

    const PATCH_V1: &str = "\
diff --git a/hello.txt b/hello.txt
--- a/hello.txt
+++ b/hello.txt
@@ -1 +1 @@
-hello
+hello world
";
    const PATCH_V2: &str = "\
diff --git a/hello.txt b/hello.txt
--- a/hello.txt
+++ b/hello.txt
@@ -1 +1 @@
-hello world
+hello world!
";
    const PATCH_V3: &str = "\
diff --git a/hello.txt b/hello.txt
--- a/hello.txt
+++ b/hello.txt
@@ -1 +1 @@
-hello world!
+still broken
";

    #[tokio::test]
    async fn qa_passing_tests_write_passed_report() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let repo = tmp.path().join("repo");
        std::fs::create_dir(&repo).expect("repo");
        std::fs::write(repo.join("nightshift.toml"), "[test]\ncommand = \"true\"\n").expect("toml");
        git_init(&repo);
        let store = ArtifactStore::new(tmp.path().join("artifacts"));
        let gen = ScriptedGenerator::new();
        qa_story_spec_and_patches(&gen, &[PATCH_V1]);
        let runner = crate::testrun::ScriptedRunner::new();
        runner.push_outcome(0, "ok", &["true".into()]);
        let run = run(
            &gen,
            &store,
            "2026-08-14",
            &RunRequest {
                goal: "greet".into(),
                repo,
                name: Some("qa-pass".into()),
                allow_dirty: false,
                article: true,
                until: Some(Until::Qa),
            },
            &HelloProbe,
            &runner,
        )
        .await
        .expect("run");
        let report = crate::qa::read_qa_report(&run).expect("report");
        assert_eq!(report.status, crate::artifacts::QaStatus::Passed);
        assert_eq!(report.iteration, 1);
        assert_eq!(report.command, "true");
        assert_eq!(report.exit_code, 0);
        let calls = runner.calls.lock().expect("calls");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].1, ["true"]);
        assert!(!run.path.join(crate::writer::ARTICLE_FILE).exists());
    }

    #[tokio::test]
    async fn full_run_writes_article_when_tests_pass() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let repo = tmp.path().join("repo");
        std::fs::create_dir(&repo).expect("repo");
        std::fs::write(repo.join("nightshift.toml"), "[test]\ncommand = \"true\"\n").expect("toml");
        git_init(&repo);
        let store = ArtifactStore::new(tmp.path().join("artifacts"));
        let gen = ScriptedGenerator::new();
        qa_story_spec_and_patches(&gen, &[PATCH_V1]);
        gen.push_text("# Article\nNothing was committed.\n");
        let runner = crate::testrun::ScriptedRunner::new();
        runner.push_outcome(0, "ok", &["true".into()]);
        let run = run(
            &gen,
            &store,
            "2026-08-14",
            &RunRequest {
                goal: "greet".into(),
                repo,
                name: Some("full".into()),
                allow_dirty: false,
                article: true,
                until: None,
            },
            &HelloProbe,
            &runner,
        )
        .await
        .expect("run");
        assert!(run.path.join(crate::pm::USER_STORY_FILE).is_file());
        assert!(run.path.join(crate::techlead::TECH_SPEC_FILE).is_file());
        assert!(run.path.join(crate::dev::PATCH_FILE).is_file());
        assert!(run.path.join(crate::qa::QA_REPORT_FILE).is_file());
        assert!(run.path.join(crate::writer::ARTICLE_FILE).is_file());
        assert!(run.path.join("run.log").is_file());
        let log = std::fs::read_to_string(run.path.join("run.log")).expect("log");
        assert!(log.contains("stage=writer done"), "{log}");
    }

    #[tokio::test]
    async fn qa_failing_tests_freeze_after_three_dev_attempts() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let repo = tmp.path().join("repo");
        std::fs::create_dir(&repo).expect("repo");
        std::fs::write(
            repo.join("nightshift.toml"),
            "[test]\ncommand = \"false\"\n",
        )
        .expect("toml");
        git_init(&repo);
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
        gen.push_text(PATCH_V1);
        gen.push_text("hint one");
        gen.push_text(PATCH_V2);
        gen.push_text("hint two");
        gen.push_text(PATCH_V3);
        let runner = crate::testrun::ScriptedRunner::new();
        runner.push_outcome(1, "fail 1", &["false".into()]);
        runner.push_outcome(1, "fail 2", &["false".into()]);
        runner.push_outcome(1, "fail 3", &["false".into()]);
        let err = run(
            &gen,
            &store,
            "2026-08-14",
            &RunRequest {
                goal: "greet".into(),
                repo,
                name: Some("qa-fail".into()),
                allow_dirty: false,
                article: true,
                until: Some(Until::Qa),
            },
            &HelloProbe,
            &runner,
        )
        .await
        .expect_err("freeze");
        match err {
            Error::Artifact(msg) => assert!(msg.contains("REQUIRES_HUMAN_REVIEW"), "{msg}"),
            other => panic!("expected Artifact, got {other:?}"),
        }
        let latest = store.root().join("latest");
        let report =
            crate::qa::read_qa_report(&crate::artifacts::RunDir { path: latest }).expect("report");
        assert_eq!(
            report.status,
            crate::artifacts::QaStatus::RequiresHumanReview
        );
        assert_eq!(report.iteration, 3);
        assert_eq!(report.command, "false");
        let test_calls = runner.calls.lock().expect("calls");
        assert_eq!(test_calls.len(), 3);
        let dev_calls = gen
            .calls()
            .into_iter()
            .filter(|c| c.model == crate::models::model_for(crate::models::Role::Dev))
            .count();
        assert_eq!(dev_calls, 3);
    }

    #[test]
    fn local_date_is_yyyy_mm_dd() {
        let date = local_date().expect("date");
        assert_eq!(date.len(), 10, "{date}");
        assert_eq!(&date[4..5], "-");
        assert_eq!(&date[7..8], "-");
    }
}
