//! Dev stage: validated unified diff, `git apply --check`, then apply. Never commit.

use crate::artifacts::RunDir;
use crate::error::Error;
use crate::generate::{complete_text, LLMClient, ROLE_TEMPERATURE};
use crate::models::{model_for, Role};
use crate::techlead::TECH_SPEC_FILE;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Artifact written by Dev.
pub const PATCH_FILE: &str = "03_diff.patch";

/// Paths named by a unified diff (`+++ b/foo`).
#[must_use]
pub fn patch_paths(patch: &str) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    for line in patch.lines() {
        let Some(rest) = line.strip_prefix("+++ ") else {
            continue;
        };
        let rest = rest.trim();
        if rest == "/dev/null" {
            continue;
        }
        let path = rest.strip_prefix("b/").unwrap_or(rest);
        paths.push(PathBuf::from(path));
    }
    paths
}

/// Reject `..`, absolute paths, and empty paths (INV-4).
pub fn validate_patch_paths(paths: &[PathBuf]) -> Result<(), Error> {
    for path in paths {
        let raw = path.to_string_lossy();
        if raw.is_empty()
            || path.is_absolute()
            || path
                .components()
                .any(|c| matches!(c, std::path::Component::ParentDir))
        {
            return Err(Error::InvalidArtifact {
                artifact: PATCH_FILE,
                reason: format!("patch path escapes the repo: {raw}"),
            });
        }
    }
    Ok(())
}

/// `git status --porcelain` is non-empty.
pub fn working_tree_dirty(repo: &Path) -> Result<bool, Error> {
    let out = git(repo, &["status", "--porcelain"])?;
    Ok(!out.trim().is_empty())
}

/// Current `HEAD` commit hash. Used to prove we never commit.
pub fn head_commit(repo: &Path) -> Result<String, Error> {
    git(repo, &["rev-parse", "HEAD"]).map(|s| s.trim().to_string())
}

/// `git apply --check` then `git apply`. Never add/commit/push/reset/clean.
pub fn apply_checked(repo: &Path, patch: &str) -> Result<(), Error> {
    validate_patch_paths(&patch_paths(patch))?;
    let tmp = tempfile::NamedTempFile::new().map_err(|e| Error::Git(e.to_string()))?;
    std::fs::write(tmp.path(), patch)?;
    git(
        repo,
        &[
            "apply",
            "--check",
            tmp.path()
                .to_str()
                .ok_or_else(|| Error::Git("patch path".into()))?,
        ],
    )?;
    git(
        repo,
        &[
            "apply",
            tmp.path()
                .to_str()
                .ok_or_else(|| Error::Git("patch path".into()))?,
        ],
    )?;
    Ok(())
}

fn git(repo: &Path, args: &[&str]) -> Result<String, Error> {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .map_err(|e| Error::Git(format!("failed to spawn git: {e}")))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(Error::Git(format!(
            "git {} failed: {}",
            args.join(" "),
            stderr.trim()
        )));
    }
    String::from_utf8(output.stdout).map_err(|e| Error::Git(e.to_string()))
}

fn file_slices(repo: &Path, files: &[PathBuf], max_bytes: usize) -> String {
    let mut out = String::new();
    let mut used = 0;
    for path in files {
        if path.is_absolute()
            || path
                .components()
                .any(|c| matches!(c, std::path::Component::ParentDir))
        {
            continue;
        }
        let full = repo.join(path);
        let Ok(bytes) = std::fs::read(&full) else {
            continue;
        };
        let take = (max_bytes.saturating_sub(used)).min(bytes.len());
        if take == 0 {
            break;
        }
        let chunk = String::from_utf8_lossy(&bytes[..take]);
        out.push_str(&format!("### {}\n{chunk}\n", path.display()));
        used += take;
    }
    out
}

fn dev_prompt(goal: &str, spec: &str, slices: &str, hints: &str) -> String {
    let hints_block = if hints.is_empty() {
        String::new()
    } else {
        format!("\nQA fix hints from the last failing run:\n{hints}\n")
    };
    format!(
        "You are the developer for one overnight job.\n\
         Goal:\n{goal}\n\n\
         Tech spec:\n{spec}\n\n\
         File slices (only these files):\n{slices}\n\
         {hints_block}\n\
         Reply with a unified diff only (`diff --git` / `--- a/` / `+++ b/`).\n\
         Do not include files outside the spec. Do not use absolute paths or `..`.\n"
    )
}

/// Generate a patch, validate paths, write `03_diff.patch`, apply to the repo.
pub async fn write_and_apply_patch<G: LLMClient>(
    generator: &G,
    run: &RunDir,
    repo: &Path,
    goal: &str,
    spec: &str,
    files: &[PathBuf],
    hints: &str,
) -> Result<(), Error> {
    let slices = file_slices(repo, files, 16_384);
    let draft = complete_text(
        generator,
        &model_for(Role::Dev),
        &dev_prompt(goal, spec, &slices, hints),
        ROLE_TEMPERATURE,
    )
    .await?;
    let patch = match validate_and_check(repo, &draft) {
        Ok(()) => draft,
        Err(error) => {
            let repaired = complete_text(
                generator,
                &model_for(Role::Router),
                &format!(
                    "Rewrite as a valid unified diff. Problem: {error}.\nOriginal:\n{draft}\n"
                ),
                ROLE_TEMPERATURE,
            )
            .await?;
            validate_and_check(repo, &repaired)?;
            repaired
        }
    };
    std::fs::write(run.path.join(PATCH_FILE), &patch)?;
    apply_checked(repo, &patch)?;
    Ok(())
}

fn validate_and_check(repo: &Path, patch: &str) -> Result<(), Error> {
    validate_patch_paths(&patch_paths(patch))?;
    let tmp = tempfile::NamedTempFile::new().map_err(|e| Error::Git(e.to_string()))?;
    std::fs::write(tmp.path(), patch)?;
    git(
        repo,
        &[
            "apply",
            "--check",
            tmp.path()
                .to_str()
                .ok_or_else(|| Error::Git("patch path".into()))?,
        ],
    )?;
    Ok(())
}

/// Read the tech spec from the run directory.
pub fn read_tech_spec(run: &RunDir) -> Result<String, Error> {
    std::fs::read_to_string(run.path.join(TECH_SPEC_FILE))
        .map_err(|e| Error::Artifact(format!("missing {TECH_SPEC_FILE}: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::artifacts::ArtifactStore;
    use crate::generate::ScriptedGenerator;
    use crate::techlead::impacted_files;

    fn init_repo() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().expect("tempdir");
        let repo = tmp.path();
        assert!(Command::new("git")
            .args(["init", "-b", "main"])
            .current_dir(repo)
            .status()
            .expect("init")
            .success());
        let _ = Command::new("git")
            .args(["config", "user.email", "dev@example.com"])
            .current_dir(repo)
            .status();
        let _ = Command::new("git")
            .args(["config", "user.name", "Dev"])
            .current_dir(repo)
            .status();
        std::fs::write(repo.join("hello.txt"), "hello\n").expect("write");
        assert!(Command::new("git")
            .args(["add", "hello.txt"])
            .current_dir(repo)
            .status()
            .expect("add")
            .success());
        assert!(Command::new("git")
            .args(["commit", "-m", "init"])
            .current_dir(repo)
            .status()
            .expect("commit")
            .success());
        tmp
    }

    fn hello_patch() -> String {
        "\
diff --git a/hello.txt b/hello.txt
--- a/hello.txt
+++ b/hello.txt
@@ -1 +1 @@
-hello
+hello world
"
        .into()
    }

    #[test]
    fn escaping_patch_is_rejected() {
        let err = validate_patch_paths(&[PathBuf::from("../secret")]).expect_err("escape");
        match err {
            Error::InvalidArtifact { reason, .. } => {
                assert!(reason.contains("escapes"), "{reason}");
            }
            other => panic!("expected InvalidArtifact, got {other:?}"),
        }
        let err = validate_patch_paths(&[PathBuf::from("/etc/passwd")]).expect_err("abs");
        assert!(matches!(err, Error::InvalidArtifact { .. }));
    }

    #[test]
    fn apply_dirties_tree_and_does_not_commit() {
        let repo = init_repo();
        let before = head_commit(repo.path()).expect("head");
        apply_checked(repo.path(), &hello_patch()).expect("apply");
        assert!(working_tree_dirty(repo.path()).expect("dirty"));
        let after = head_commit(repo.path()).expect("head");
        assert_eq!(before, after, "pipeline must not commit");
        let body = std::fs::read_to_string(repo.path().join("hello.txt")).expect("read");
        assert_eq!(body, "hello world\n");
    }

    #[test]
    fn failed_apply_check_is_an_error() {
        let repo = init_repo();
        let bad = "\
diff --git a/missing.txt b/missing.txt
--- a/missing.txt
+++ b/missing.txt
@@ -1 +1 @@
-nope
+still
";
        let err = apply_checked(repo.path(), bad).expect_err("check");
        assert!(matches!(err, Error::Git(_)), "{err:?}");
        assert!(!working_tree_dirty(repo.path()).expect("clean"));
    }

    #[tokio::test]
    async fn writes_patch_artifact_then_applies() {
        let repo = init_repo();
        let artifacts = tempfile::tempdir().expect("art");
        let run = ArtifactStore::new(artifacts.path())
            .create_run("2026-08-14", "dev")
            .expect("run");
        let gen = ScriptedGenerator::new();
        gen.push_text(hello_patch());
        write_and_apply_patch(
            &gen,
            &run,
            repo.path(),
            "greet",
            "## Impacted files\n- hello.txt\n",
            &[PathBuf::from("hello.txt")],
            "",
        )
        .await
        .expect("dev");
        assert!(run.path.join(PATCH_FILE).is_file());
        assert_eq!(gen.calls()[0].model, model_for(Role::Dev));
        assert!(working_tree_dirty(repo.path()).expect("dirty"));
    }

    #[test]
    fn impacted_files_feed_slices() {
        let files = impacted_files("## Impacted files\n- src/cli.rs\n## Out of scope\n");
        assert_eq!(files, [PathBuf::from("src/cli.rs")]);
    }
}
