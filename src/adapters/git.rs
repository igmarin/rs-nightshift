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
///
/// If the initial check fails due to incorrect hunk-header line counts (a
/// common model output defect), [`repair_hunk_headers`] is tried as a
/// fallback before the apply. If that also fails, `git apply --3way` is
/// tried as a last resort — it uses the index for 3-way merge, which
/// tolerates context-line mismatches that trip up the strict check.
pub fn apply_checked(repo: &Path, patch: &str) -> Result<(), Error> {
    match apply_check(repo, patch) {
        Ok(()) => {}
        Err(original) => {
            let repaired = repair_hunk_headers(patch);
            if repaired != patch && apply_check(repo, &repaired).is_ok() {
                return apply_raw(repo, &repaired);
            }
            // Last resort: 3-way merge uses the index to resolve context
            // mismatches. This handles the common case where the model got
            // the line numbers and context lines wrong but the actual
            // changes (removed/added lines) are correct.
            match apply_3way(repo, patch) {
                Ok(()) => return Ok(()),
                Err(_) => return Err(original),
            }
        }
    }
    apply_raw(repo, patch)
}

/// Write `patch` to a temp file and run `git apply` (no `--check`).
fn apply_raw(repo: &Path, patch: &str) -> Result<(), Error> {
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

/// Write `patch` to a temp file and run `git apply --3way` (3-way merge).
///
/// Unlike `apply_raw`, this uses the index as a base for 3-way merge,
/// which tolerates context-line mismatches that cause strict `--check` to
/// fail. This is the last-resort fallback in [`apply_checked`].
fn apply_3way(repo: &Path, patch: &str) -> Result<(), Error> {
    let tmp = tempfile::NamedTempFile::new().map_err(|e| GitError::new(e.to_string()))?;
    std::fs::write(tmp.path(), patch)
        .map_err(|e| GitError::new(format!("failed to write patch: {e}")))?;
    git(
        repo,
        &[
            "apply",
            "--3way",
            tmp.path()
                .to_str()
                .ok_or_else(|| GitError::new("patch path".to_string()))?,
        ],
    )?;
    Ok(())
}

/// Recompute hunk-header line counts from the actual hunk content.
///
/// Models frequently write `@@ -10,7 +10,7 @@` with wrong counts. This
/// function parses each hunk, counts context + removed lines for the old
/// count and context + added lines for the new count, and rewrites the
/// header. Start line numbers are preserved (git apply tolerates off-by-one
/// starts but not wrong counts).
#[must_use]
pub fn repair_hunk_headers(patch: &str) -> String {
    let lines: Vec<&str> = patch.lines().collect();
    let mut out = String::with_capacity(patch.len());
    let mut i = 0;

    while i < lines.len() {
        let line = lines[i];

        if line.starts_with("@@ ") {
            let (old_start, new_start) = parse_hunk_starts(line);
            let mut old_count: u32 = 0;
            let mut new_count: u32 = 0;
            let mut j = i + 1;

            while j < lines.len() {
                let body = lines[j];
                if body.starts_with("@@ ") || body.starts_with("diff --git") {
                    break;
                }
                if body.starts_with(' ') {
                    old_count += 1;
                    new_count += 1;
                } else if body.starts_with('-') {
                    old_count += 1;
                } else if body.starts_with('+') {
                    new_count += 1;
                } else if body.starts_with('\\') {
                    // "\ No newline at end of file" — not counted.
                } else if body.is_empty() {
                    // Empty lines in a diff are context lines that lost
                    // their space prefix. Treat as context.
                    old_count += 1;
                    new_count += 1;
                } else {
                    break;
                }
                j += 1;
            }

            out.push_str(&format_hunk_header(
                old_start, old_count, new_start, new_count,
            ));
            out.push('\n');
            for body in lines.iter().take(j).skip(i + 1) {
                out.push_str(body);
                out.push('\n');
            }
            i = j;
        } else {
            out.push_str(line);
            out.push('\n');
            i += 1;
        }
    }

    // git apply requires a trailing newline; ensure one is present.
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out
}

/// Extract `(old_start, new_start)` from a `@@ -OLD,COUNT +NEW,COUNT @@` line.
fn parse_hunk_starts(header: &str) -> (Option<i64>, Option<i64>) {
    // Format: @@ -START[,COUNT] +START[,COUNT] @@
    let parts: Vec<&str> = header.split_whitespace().collect();
    let old = parts.iter().find(|p| p.starts_with('-')).and_then(|p| {
        let s = &p[1..]; // strip leading '-'
        let start_str = s.split(',').next().unwrap_or(s);
        start_str.parse::<i64>().ok()
    });
    let new = parts.iter().find(|p| p.starts_with('+')).and_then(|p| {
        let s = &p[1..]; // strip leading '+'
        let start_str = s.split(',').next().unwrap_or(s);
        start_str.parse::<i64>().ok()
    });
    (old, new)
}

/// Format a hunk header with corrected counts.
fn format_hunk_header(
    old_start: Option<i64>,
    old_count: u32,
    new_start: Option<i64>,
    new_count: u32,
) -> String {
    let old = match old_start {
        Some(s) => format!("-{}", s),
        None => "-1".to_string(),
    };
    let new = match new_start {
        Some(s) => format!("+{}", s),
        None => "+1".to_string(),
    };
    format!("@@ {old},{old_count} {new},{new_count} @@")
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repair_fixes_wrong_hunk_counts() {
        let patch = "\
diff --git a/file.txt b/file.txt
--- a/file.txt
+++ b/file.txt
@@ -10,7 +10,7 @@
 context
-old
+new
 context
";
        let repaired = repair_hunk_headers(patch);
        assert!(
            repaired.contains("@@ -10,3 +10,3 @@"),
            "expected corrected counts, got: {repaired}"
        );
    }

    #[test]
    fn repair_handles_multiple_hunks() {
        let patch = "\
diff --git a/file.txt b/file.txt
--- a/file.txt
+++ b/file.txt
@@ -1,7 +1,7 @@
 ctx
-a
+b
 ctx
@@ -20,7 +20,8 @@
 ctx
-c
+d
+e
 ctx
";
        let repaired = repair_hunk_headers(patch);
        assert!(
            repaired.contains("@@ -1,3 +1,3 @@"),
            "first hunk wrong: {repaired}"
        );
        assert!(
            repaired.contains("@@ -20,3 +20,4 @@"),
            "second hunk wrong: {repaired}"
        );
    }

    #[test]
    fn repair_preserves_correct_counts() {
        let patch = "\
diff --git a/file.txt b/file.txt
--- a/file.txt
+++ b/file.txt
@@ -1,3 +1,3 @@
 ctx
-a
+b
 ctx
";
        let repaired = repair_hunk_headers(patch);
        assert_eq!(repaired, patch, "already-correct patch should be unchanged");
    }

    #[test]
    fn repair_handles_addition_only_hunk() {
        let patch = "\
diff --git a/file.txt b/file.txt
--- a/file.txt
+++ b/file.txt
@@ -1,1 +1,1 @@
 ctx
+new
";
        let repaired = repair_hunk_headers(patch);
        assert!(
            repaired.contains("@@ -1,1 +1,2 @@"),
            "addition-only hunk wrong: {repaired}"
        );
    }

    #[test]
    fn repair_handles_removal_only_hunk() {
        let patch = "\
diff --git a/file.txt b/file.txt
--- a/file.txt
+++ b/file.txt
@@ -1,1 +1,1 @@
-old
+new
";
        let repaired = repair_hunk_headers(patch);
        assert!(
            repaired.contains("@@ -1,1 +1,1 @@"),
            "removal-only hunk wrong: {repaired}"
        );
    }

    #[test]
    fn repair_preserves_start_line_numbers() {
        let patch = "\
diff --git a/file.txt b/file.txt
--- a/file.txt
+++ b/file.txt
@@ -42,7 +42,7 @@
 ctx
-a
+b
";
        let repaired = repair_hunk_headers(patch);
        assert!(
            repaired.contains("@@ -42,2 +42,2 @@"),
            "start numbers should be preserved: {repaired}"
        );
    }

    #[test]
    fn repair_skips_no_newline_marker() {
        let patch = "\
diff --git a/file.txt b/file.txt
--- a/file.txt
+++ b/file.txt
@@ -1,7 +1,7 @@
 ctx
-a
+b
\\ No newline at end of file
";
        let repaired = repair_hunk_headers(patch);
        assert!(
            repaired.contains("@@ -1,2 +1,2 @@"),
            "no-newline marker should not be counted: {repaired}"
        );
        assert!(
            repaired.contains("\\ No newline at end of file"),
            "marker should be preserved: {repaired}"
        );
    }
}
