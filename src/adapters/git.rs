//! Git primitives used by adapters and legacy stages.
//!
//! These functions inspect and mutate a repo working tree, but never add,
//! commit, push, reset, or clean.

use crate::error::{ArtifactError, Error, GitError};
use std::path::{Path, PathBuf};
use std::process::Command;

/// Artifact file name written by the Dev stage.
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
            return Err(ArtifactError::invalid(
                PATCH_FILE,
                format!("patch path escapes the repo: {raw}"),
            )
            .into());
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

/// Validate patch paths and run `git apply --check` in `repo`.
///
/// This is the safe preflight used by `apply_checked` and
/// `crate::dev::write_and_apply_patch`. It never modifies the working tree.
pub(crate) fn apply_check(repo: &Path, patch: &str) -> Result<(), Error> {
    validate_patch_paths(&patch_paths(patch))?;
    let tmp = tempfile::NamedTempFile::new().map_err(|e| GitError::new(e.to_string()))?;
    std::fs::write(tmp.path(), patch)
        .map_err(|e| GitError::new(format!("failed to write patch: {e}")))?;
    git(
        repo,
        &[
            "apply",
            "--check",
            tmp.path()
                .to_str()
                .ok_or_else(|| GitError::new("patch path".to_string()))?,
        ],
    )?;
    Ok(())
}

/// `git apply --check` then `git apply`. Never add/commit/push/reset/clean.
pub fn apply_checked(repo: &Path, patch: &str) -> Result<(), Error> {
    apply_check(repo, patch)?;
    let tmp = tempfile::NamedTempFile::new().map_err(|e| GitError::new(e.to_string()))?;
    std::fs::write(tmp.path(), patch)
        .map_err(|e| GitError::new(format!("failed to write patch: {e}")))?;
    git(
        repo,
        &[
            "apply",
            tmp.path()
                .to_str()
                .ok_or_else(|| GitError::new("patch path".to_string()))?,
        ],
    )?;
    Ok(())
}

/// Run a git command in `repo` and return its stdout as UTF-8.
fn git(repo: &Path, args: &[&str]) -> Result<String, Error> {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .map_err(|e| GitError::new(format!("failed to spawn git: {e}")))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(
            GitError::new(format!("git {} failed: {}", args.join(" "), stderr.trim())).into(),
        );
    }
    String::from_utf8(output.stdout).map_err(|e| GitError::new(e.to_string()).into())
}
