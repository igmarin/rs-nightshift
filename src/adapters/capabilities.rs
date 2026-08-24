//! Capability adapters: `run-tests`, `apply-patch`, `write-file`,
//! `search-replace`, and `gather-context`.
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
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Runs declared capabilities against a target repo.
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

    /// Apply exact `old:` / `new:` replacements from `input` to files in `repo`.
    ///
    /// The payload is one or more blocks:
    ///
    /// ```text
    /// file: path/relative.txt
    /// old: unique snippet
    /// new: replacement
    /// ```
    ///
    /// Each `old` must match exactly once. All replacements are applied in
    /// memory first; files are written only if every block succeeds.
    async fn search_replace(&self, repo: &Path, input: &str) -> Result<String, Error> {
        let repo = repo.to_path_buf();
        let content = input.to_string();
        tokio::task::spawn_blocking(move || apply_search_replace(&repo, &content))
            .await
            .map_err(|error| {
                Error::from(ArtifactError::artifact(format!(
                    "search-replace join: {error}"
                )))
            })?
    }
}

/// One exact-text replacement against a repo-relative path.
#[derive(Debug, Clone, PartialEq, Eq)]
struct SearchReplaceBlock {
    path: String,
    old: String,
    new: String,
}

/// Parse `file:` / `old:` / `new:` blocks from a role's `content` payload.
fn parse_search_replace(input: &str) -> Result<Vec<SearchReplaceBlock>, Error> {
    let mut replacements = Vec::new();
    let mut file: Option<String> = None;
    let mut old: Option<String> = None;
    let mut new: Option<String> = None;
    let mut field: Option<&'static str> = None;

    let flush = |file: &Option<String>,
                 old: &mut Option<String>,
                 new: &mut Option<String>,
                 replacements: &mut Vec<SearchReplaceBlock>|
     -> Result<(), Error> {
        match (file.as_ref(), old.take(), new.take()) {
            (None, None, None) | (Some(_), None, None) => Ok(()),
            (Some(_), Some(_), None) => Err(Error::from(ArtifactError::artifact(
                "search-replace: block is missing a new: field",
            ))),
            (Some(_), None, Some(_)) => Err(Error::from(ArtifactError::artifact(
                "search-replace: block is missing an old: field",
            ))),
            (None, _, _) => Err(Error::from(ArtifactError::artifact(
                "search-replace: block is missing a file: header",
            ))),
            (Some(path), Some(old_text), Some(new_text)) => {
                if old_text.is_empty() {
                    return Err(Error::from(ArtifactError::artifact(
                        "search-replace: old_text must not be empty",
                    )));
                }
                if path.is_empty() {
                    return Err(Error::from(ArtifactError::artifact(
                        "search-replace: file path must not be empty",
                    )));
                }
                replacements.push(SearchReplaceBlock {
                    path: path.clone(),
                    old: old_text,
                    new: new_text,
                });
                Ok(())
            }
        }
    };

    for line in input.lines() {
        if let Some((kind, rest)) = header_line(line) {
            match kind {
                "file" => {
                    flush(&file, &mut old, &mut new, &mut replacements)?;
                    file = Some(rest.to_string());
                    field = None;
                }
                "old" => {
                    if old.is_some() {
                        flush(&file, &mut old, &mut new, &mut replacements)?;
                    }
                    old = Some(rest.to_string());
                    new = None;
                    field = Some("old");
                }
                "new" => {
                    if old.is_none() {
                        return Err(Error::from(ArtifactError::artifact(
                            "search-replace: new: before old:",
                        )));
                    }
                    if new.is_some() {
                        return Err(Error::from(ArtifactError::artifact(
                            "search-replace: duplicate new: in one block",
                        )));
                    }
                    new = Some(rest.to_string());
                    field = Some("new");
                }
                _ => unreachable!("header_line only returns file/old/new"),
            }
            continue;
        }
        match field {
            Some("old") => {
                let buf = old.get_or_insert_with(String::new);
                append_body_line(buf, line);
            }
            Some("new") => {
                let buf = new.get_or_insert_with(String::new);
                append_body_line(buf, line);
            }
            _ if line.trim().is_empty() => {}
            _ => {
                return Err(Error::from(ArtifactError::artifact(
                    "search-replace: stray content outside old:/new: blocks",
                )));
            }
        }
    }
    flush(&file, &mut old, &mut new, &mut replacements)?;
    if replacements.is_empty() {
        return Err(Error::from(ArtifactError::artifact(
            "search-replace: no replacements in input",
        )));
    }
    Ok(replacements)
}

fn header_line(line: &str) -> Option<(&'static str, &str)> {
    for (kind, prefix) in [("file", "file:"), ("old", "old:"), ("new", "new:")] {
        if let Some(rest) = line.strip_prefix(prefix) {
            let rest = rest.strip_prefix(' ').unwrap_or(rest);
            return Some((kind, rest));
        }
    }
    None
}

fn append_body_line(buf: &mut String, line: &str) {
    if buf.is_empty() {
        buf.push_str(line);
    } else {
        buf.push('\n');
        buf.push_str(line);
    }
}

/// Resolve `rel` inside `repo`. Rejects empty, absolute, and `..` paths.
fn resolve_repo_file(repo: &Path, rel: &str) -> Result<PathBuf, Error> {
    let path = Path::new(rel);
    if rel.is_empty()
        || path.is_absolute()
        || path
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return Err(Error::from(ArtifactError::artifact(format!(
            "search-replace: path escapes repo root: {rel}"
        ))));
    }
    let file_path = repo.join(path);
    let canonical = file_path.canonicalize().map_err(|error| {
        ArtifactError::artifact(format!("search-replace: invalid path {rel}: {error}"))
    })?;
    let repo_canonical = repo.canonicalize().map_err(|error| {
        ArtifactError::artifact(format!("search-replace: invalid repo: {error}"))
    })?;
    if !canonical.starts_with(&repo_canonical) {
        return Err(Error::from(ArtifactError::artifact(
            "search-replace: path escapes repo root",
        )));
    }
    if !canonical.is_file() {
        return Err(Error::from(ArtifactError::artifact(format!(
            "search-replace: not a file: {rel}"
        ))));
    }
    Ok(canonical)
}

fn apply_search_replace(repo: &Path, input: &str) -> Result<String, Error> {
    let replacements = parse_search_replace(input)?;
    let mut abs_by_rel: HashMap<String, PathBuf> = HashMap::new();
    for block in &replacements {
        if abs_by_rel.contains_key(&block.path) {
            continue;
        }
        abs_by_rel.insert(block.path.clone(), resolve_repo_file(repo, &block.path)?);
    }

    let mut contents: HashMap<String, String> = HashMap::new();
    for (rel, abs) in &abs_by_rel {
        let body = std::fs::read_to_string(abs).map_err(|error| {
            ArtifactError::artifact(format!("search-replace: failed to read {rel}: {error}"))
        })?;
        contents.insert(rel.clone(), body);
    }

    for block in &replacements {
        let body = contents.get_mut(&block.path).expect("loaded");
        let count = body.matches(block.old.as_str()).count();
        match count {
            0 => {
                return Err(Error::from(ArtifactError::artifact(format!(
                    "search-replace: old_text not found in {}",
                    block.path
                ))));
            }
            1 => {
                *body = body.replacen(&block.old, &block.new, 1);
            }
            n => {
                return Err(Error::from(ArtifactError::artifact(format!(
                    "search-replace: old_text matches {n} locations in {} (ambiguous)",
                    block.path
                ))));
            }
        }
    }

    for (rel, abs) in &abs_by_rel {
        let body = contents.get(rel).expect("applied");
        std::fs::write(abs, body).map_err(|error| {
            ArtifactError::artifact(format!("search-replace: failed to write {rel}: {error}"))
        })?;
    }

    let n = replacements.len();
    let mut paths: Vec<&str> = replacements.iter().map(|b| b.path.as_str()).collect();
    paths.sort();
    paths.dedup();
    Ok(format!("replaced {n} block(s) in {}", paths.join(", ")))
}

impl Default for CapabilityRunner {
    /// Default runner with the standard test timeout.
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl ToolRunner for CapabilityRunner {
    /// Dispatch a declared capability for `repo`.
    async fn run(&self, tool: &str, repo: &Path, input: &str) -> Result<String, Error> {
        match tool {
            "run-tests" => self.run_tests(repo).await,
            "apply-patch" => self.apply_patch(repo, input).await,
            "write-file" => self.write_file(repo, input).await,
            "search-replace" => self.search_replace(repo, input).await,
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

    fn search_replace_input(path: &str, old: &str, new: &str) -> String {
        format!("file: {path}\nold: {old}\nnew: {new}\n")
    }

    #[tokio::test]
    async fn search_replace_applies_a_single_replacement() {
        let repo = init_repo();
        let runner = CapabilityRunner::new();
        let out = runner
            .run(
                "search-replace",
                repo.path(),
                &search_replace_input("hello.txt", "hello", "hello world"),
            )
            .await
            .expect("replace");
        assert!(out.contains("replaced"), "{out}");
        let body = std::fs::read_to_string(repo.path().join("hello.txt")).expect("read");
        assert_eq!(body, "hello world\n");
    }

    #[tokio::test]
    async fn search_replace_applies_a_multiline_snippet() {
        let repo = init_repo();
        std::fs::write(repo.path().join("hello.txt"), "alpha\nbeta\ngamma\nkeep\n").expect("write");
        let runner = CapabilityRunner::new();
        let input = "\
file: hello.txt
old:
beta
gamma
new:
BETA
GAMMA
";
        runner
            .run("search-replace", repo.path(), input)
            .await
            .expect("replace");
        let body = std::fs::read_to_string(repo.path().join("hello.txt")).expect("read");
        assert_eq!(body, "alpha\nBETA\nGAMMA\nkeep\n");
    }

    #[tokio::test]
    async fn search_replace_errors_on_ambiguous_match() {
        let repo = init_repo();
        std::fs::write(repo.path().join("hello.txt"), "hello\nhello\n").expect("write");
        let runner = CapabilityRunner::new();
        let err = runner
            .run(
                "search-replace",
                repo.path(),
                &search_replace_input("hello.txt", "hello", "hi"),
            )
            .await
            .expect_err("ambiguous");
        assert!(err.to_string().contains("ambiguous"), "{err}");
        let body = std::fs::read_to_string(repo.path().join("hello.txt")).expect("read");
        assert_eq!(body, "hello\nhello\n");
    }

    #[tokio::test]
    async fn search_replace_errors_when_old_text_is_missing() {
        let repo = init_repo();
        let runner = CapabilityRunner::new();
        let err = runner
            .run(
                "search-replace",
                repo.path(),
                &search_replace_input("hello.txt", "nope", "hi"),
            )
            .await
            .expect_err("not found");
        assert!(err.to_string().contains("not found"), "{err}");
        let body = std::fs::read_to_string(repo.path().join("hello.txt")).expect("read");
        assert_eq!(body, "hello\n");
    }

    #[tokio::test]
    async fn search_replace_rejects_path_escape() {
        let repo = init_repo();
        let runner = CapabilityRunner::new();
        let err = runner
            .run(
                "search-replace",
                repo.path(),
                &search_replace_input("../hello.txt", "hello", "hi"),
            )
            .await
            .expect_err("escape");
        let msg = err.to_string();
        assert!(
            msg.contains("escapes") || msg.contains("..") || msg.contains("invalid"),
            "{msg}"
        );
        let body = std::fs::read_to_string(repo.path().join("hello.txt")).expect("read");
        assert_eq!(body, "hello\n");
    }

    #[tokio::test]
    async fn search_replace_is_all_or_nothing() {
        let repo = init_repo();
        std::fs::write(repo.path().join("other.txt"), "keep\n").expect("write");
        let runner = CapabilityRunner::new();
        let input = "\
file: hello.txt
old: hello
new: changed
file: other.txt
old: missing
new: nope
";
        let err = runner
            .run("search-replace", repo.path(), input)
            .await
            .expect_err("second block missing");
        assert!(err.to_string().contains("not found"), "{err}");
        let hello = std::fs::read_to_string(repo.path().join("hello.txt")).expect("read");
        let other = std::fs::read_to_string(repo.path().join("other.txt")).expect("read");
        assert_eq!(hello, "hello\n");
        assert_eq!(other, "keep\n");
    }
}
