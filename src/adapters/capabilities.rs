//! Capability adapters: `run-tests`, `apply-patch`, and `gather-context`.
//!
//! These wrap the existing deterministic primitives (test-command detection +
//! process runner, `git apply --check` then apply, and the codegraph/graphify
//! probe) behind the [`ToolRunner`] and [`ContextProvider`] ports. Blocking
//! process I/O runs on `tokio` blocking threads so the executor never holds a
//! blocking primitive across `.await`.

use crate::adapters::context::{gather, PathProbe};
use crate::adapters::git::apply_checked;
use crate::adapters::test::{
    detect_test_command, ProcessTestRunner, TestRunner, DEFAULT_TEST_TIMEOUT,
};
use crate::error::{ArtifactError, Error};
use crate::ports::{ContextProvider, ToolRunner};
use std::path::Path;
use std::time::Duration;

/// Runs `run-tests` and `apply-patch` capabilities against a target repo.
#[derive(Debug, Clone)]
pub struct CapabilityRunner {
    /// Per-run test timeout.
    test_timeout: Duration,
}

impl CapabilityRunner {
    /// A runner with the default test timeout.
    #[must_use]
    pub fn new() -> Self {
        Self {
            test_timeout: DEFAULT_TEST_TIMEOUT,
        }
    }

    /// A runner with an explicit test timeout (used in tests).
    #[must_use]
    pub fn with_test_timeout(test_timeout: Duration) -> Self {
        Self { test_timeout }
    }

    /// Detect, run, and format the test command for `repo`.
    async fn run_tests(&self, repo: &Path) -> Result<String, Error> {
        let argv = detect_test_command(repo)?;
        let repo = repo.to_path_buf();
        let runner = ProcessTestRunner::new(self.test_timeout);
        let outcome = tokio::task::spawn_blocking(move || runner.run(&repo, &argv))
            .await
            .map_err(|error| {
                Error::from(ArtifactError::artifact(format!(
                    "test runner join: {error}"
                )))
            })??;
        Ok(format!(
            "exit code: {}\n{}",
            outcome.exit_code, outcome.output
        ))
    }

    /// Apply `input` as a unified diff to `repo` after validation.
    async fn apply_patch(&self, repo: &Path, input: &str) -> Result<String, Error> {
        let repo = repo.to_path_buf();
        let patch = input.to_string();
        tokio::task::spawn_blocking(move || apply_checked(&repo, &patch))
            .await
            .map_err(|error| {
                Error::from(ArtifactError::artifact(format!("git apply join: {error}")))
            })??;
        Ok("patch applied".to_string())
    }

    /// Write `input` as full file content to `repo`. The input must start
    /// with a header line `file: <path>` followed by the file content. The
    /// content is considered to end at the first sentinel line that looks
    /// like another context marker (`<!-- file:`, `<!-- end file content -->`,
    /// or `file:`) because small models sometimes echo the injected context
    /// markers.
    async fn write_file(&self, repo: &Path, input: &str) -> Result<String, Error> {
        let repo = repo.to_path_buf();
        let content = input.to_string();
        tokio::task::spawn_blocking(move || {
            // Parse the "file: <path>" header line
            let mut lines = content.lines();
            let header = lines
                .next()
                .ok_or_else(|| ArtifactError::artifact("write-file: empty input"))?;
            let path = header
                .strip_prefix("file: ")
                .ok_or_else(|| {
                    ArtifactError::artifact("write-file: input must start with 'file: <path>'")
                })?
                .trim();
            // Validate the path is repo-relative and safe
            let file_path = repo.join(path);
            let canonical = file_path
                .canonicalize()
                .map_err(|e| ArtifactError::artifact(format!("write-file: invalid path: {e}")))?;
            let repo_canonical = repo
                .canonicalize()
                .map_err(|e| ArtifactError::artifact(format!("write-file: invalid repo: {e}")))?;
            if !canonical.starts_with(&repo_canonical) {
                return Err(Error::from(ArtifactError::artifact(
                    "write-file: path escapes repo root",
                )));
            }
            // Collect body lines, stopping at context-marker sentinels that
            // small models often echo from the prompt. Also strip a common
            // JSON artifact: a lone trailing `"` line that the model inserts
            // when it confuses the JSON string closing quote with content.
            let mut body_lines: Vec<&str> = Vec::new();
            for line in lines {
                if line.starts_with("<!-- file:")
                    || line.starts_with("<!-- end file content -->")
                    || line.starts_with("file: ")
                {
                    break;
                }
                body_lines.push(line);
            }
            // Trim trailing blank or lone-quote lines that are JSON artifacts.
            while body_lines.last().is_some_and(|l| {
                l.trim().is_empty() || (l.trim() == "\"" && l.trim().len() == l.len())
            }) {
                body_lines.pop();
            }
            let body = body_lines.join("\n");
            let body_len = body.len();
            std::fs::write(&file_path, body)
                .map_err(|e| ArtifactError::artifact(format!("write-file: {e}")))?;
            Ok(format!("wrote {path} ({body_len} bytes)"))
        })
        .await
        .map_err(|error| {
            Error::from(ArtifactError::artifact(format!("write-file join: {error}")))
        })?
    }
}

impl Default for CapabilityRunner {
    /// Default runner with the standard test timeout.
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl ToolRunner for CapabilityRunner {
    /// Dispatch `run-tests`, `apply-patch`, or `write-file` for `repo`.
    async fn run(&self, tool: &str, repo: &Path, input: &str) -> Result<String, Error> {
        match tool {
            "run-tests" => self.run_tests(repo).await,
            "apply-patch" => self.apply_patch(repo, input).await,
            "write-file" => self.write_file(repo, input).await,
            other => Err(Error::from(ArtifactError::artifact(format!(
                "unknown tool {other:?}"
            )))),
        }
    }
}

/// Gathers codegraph/graphify context via [`PathProbe`].
#[derive(Debug, Default)]
pub struct GraphContextProvider;

#[async_trait::async_trait]
impl ContextProvider for GraphContextProvider {
    /// Gather codegraph/graphify context for `repo` and `goal`.
    async fn gather(&self, repo: &Path, goal: &str) -> Result<String, Error> {
        let repo = repo.to_path_buf();
        let goal = goal.to_string();
        tokio::task::spawn_blocking(move || {
            gather(&PathProbe, &repo, &goal).map(|bundle| bundle.text)
        })
        .await
        .map_err(|error| Error::Context(format!("context join: {error}")))?
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    /// Create a temporary git repo with an initial `hello.txt` commit.
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

    /// A valid unified diff that updates `hello.txt`.
    const HELLO_PATCH: &str = "\
diff --git a/hello.txt b/hello.txt
--- a/hello.txt
+++ b/hello.txt
@@ -1 +1 @@
-hello
+hello world
";

    #[tokio::test]
    async fn apply_patch_applies_a_valid_patch() {
        let repo = init_repo();
        let runner = CapabilityRunner::new();
        let out = runner
            .run("apply-patch", repo.path(), HELLO_PATCH)
            .await
            .expect("apply");
        assert_eq!(out, "patch applied");
        let body = std::fs::read_to_string(repo.path().join("hello.txt")).expect("read");
        assert_eq!(body, "hello world\n");
    }

    #[tokio::test]
    async fn run_tests_runs_the_configured_command() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            tmp.path().join("nightshift.toml"),
            "[test]\ncommand = \"true\"\n",
        )
        .expect("write");
        let runner = CapabilityRunner::new();
        let out = runner.run("run-tests", tmp.path(), "").await.expect("run");
        assert!(out.contains("exit code: 0"), "{out}");
    }

    #[tokio::test]
    async fn unknown_tool_is_an_error() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let runner = CapabilityRunner::new();
        let err = runner
            .run("fly", tmp.path(), "")
            .await
            .expect_err("unknown");
        assert!(err.to_string().contains("unknown tool"), "{err}");
    }
}
