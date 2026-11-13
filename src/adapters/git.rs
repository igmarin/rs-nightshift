//! Git primitives used by adapters and legacy stages.
//!
//! These functions inspect and mutate a repo working tree, but never add,
//! commit, push, reset, or clean.

use crate::error::{ArtifactError, Error, GitError};
use std::path::{Path, PathBuf};
use std::process::Command;

/// Artifact file name written by the Dev stage.
pub const PATCH_FILE: &str = "03_diff.patch";

/// Every path `patch` touches, read the way `git apply` reads it
/// (`git apply --numstat -z`, which applies nothing): changed, new, deleted
/// and binary files, with quoted names decoded, and the new name of a renamed
/// or copied file. Git leaves out the old name, so the `rename from` and
/// `copy from` header lines add it. A patch git cannot parse is an error.
fn touched_paths(repo: &Path, patch: &str) -> Result<Vec<PathBuf>, Error> {
    let tmp = tempfile::NamedTempFile::new().map_err(|e| GitError::new(e.to_string()))?;
    std::fs::write(tmp.path(), patch)
        .map_err(|e| GitError::new(format!("failed to write patch: {e}")))?;
    let tmp_path = tmp
        .path()
        .to_str()
        .ok_or_else(|| GitError::new("patch path".to_string()))?;
    let mut paths = numstat_paths(&git(repo, &["apply", "--numstat", "-z", tmp_path])?);
    for line in patch.lines() {
        // A hunk line starts with ' ', '+', '-' or '\', never with these words.
        let Some(source) = line
            .strip_prefix("rename from ")
            .or_else(|| line.strip_prefix("copy from "))
        else {
            continue;
        };
        // Git decodes a quoted name, and ends an unquoted one at a control
        // character such as '\x0b', so either would check a different path
        // than the one git renames. A name git writes as it is has neither.
        if source.starts_with('"') || source.chars().any(char::is_control) {
            return Err(ArtifactError::invalid(
                PATCH_FILE,
                format!("patch renames or copies a quoted name or one with a control character: {source:?}"),
            )
            .into());
        }
        // Some git versions list the old name themselves.
        let source = PathBuf::from(source);
        if !paths.contains(&source) {
            paths.push(source);
        }
    }
    Ok(paths)
}

/// The paths `patch` touches ([`touched_paths`]) pass
/// [`validate_patch_paths`], and none of them goes through a symlink in the
/// working tree, including the file itself.
fn check_patch_paths(repo: &Path, patch: &str) -> Result<(), Error> {
    let paths = touched_paths(repo, patch)?;
    validate_patch_paths(&paths)?;
    // In a repo, git reads the paths from the top of the working tree, also
    // when `repo` is a folder inside it; outside a repo, from `repo`.
    let root = git(repo, &["rev-parse", "--show-toplevel"])
        .map(|top| PathBuf::from(top.trim_end_matches('\n')))
        .unwrap_or_else(|_| repo.to_path_buf());
    for path in &paths {
        // Git refuses a path "beyond a symbolic link" itself, except with
        // `--3way` when it does not track the link: it then writes through it.
        let mut current = root.clone();
        for component in path.components() {
            current.push(component);
            if current
                .symlink_metadata()
                .is_ok_and(|meta| meta.file_type().is_symlink())
            {
                return Err(ArtifactError::invalid(
                    PATCH_FILE,
                    format!("patch path goes through a symlink: {}", path.display()),
                )
                .into());
            }
        }
    }
    Ok(())
}

/// Paths in `git apply --numstat -z` output: `added\tdeleted\tpath\0` per
/// file (`-` counts for a binary file), or for a rename an empty path
/// followed by the old and the new path as two more fields.
fn numstat_paths(listing: &str) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    let mut fields = listing.split('\0');
    while let Some(field) = fields.next() {
        match field.splitn(3, '\t').nth(2) {
            Some("") => paths.extend(fields.by_ref().take(2).map(PathBuf::from)),
            Some(path) => paths.push(PathBuf::from(path)),
            None => {} // the empty field after the last NUL
        }
    }
    paths
}

/// Reject `..`, absolute paths, empty paths, and secret-bearing paths (INV-4).
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
        if crate::domain::rolegraph::config::is_secret_path(&raw) {
            return Err(ArtifactError::invalid(
                PATCH_FILE,
                format!("patch path is secret-bearing: {raw}"),
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
    check_patch_paths(repo, patch)?;
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
/// Every attempt checks the paths first ([`validate_patch_paths`]).
pub fn apply_checked(repo: &Path, patch: &str) -> Result<(), Error> {
    match apply_check(repo, patch) {
        Ok(()) => {}
        Err(original) => {
            let repaired = repair_hunk_headers(patch);
            if repaired != patch && apply_check(repo, &repaired).is_ok() {
                return apply_raw(repo, &repaired);
            }
            // `original` may be the path check itself, and `--3way` checks
            // nothing, so check the paths again. A patch git cannot parse
            // cannot be applied with `--3way` either.
            if check_patch_paths(repo, patch).is_err() {
                return Err(original);
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

    #[test]
    fn validate_patch_paths_rejects_secret_paths() {
        for secret in [".env", "config/id_rsa", "certs/server.pem"] {
            let err = validate_patch_paths(&[PathBuf::from(secret)]).expect_err(secret);
            assert!(err.to_string().contains("secret-bearing"), "{err}");
        }
        validate_patch_paths(&[PathBuf::from("src/main.rs")]).expect("a normal path passes");
    }

    #[test]
    fn numstat_paths_reads_files_binaries_and_split_renames() {
        let listing = concat!(
            "1\t0\t.env\0",
            "-\t-\tkey.pem\0",
            "0\t0\t\0old.txt\0new.txt\0"
        );
        let expected = [".env", "key.pem", "old.txt", "new.txt"].map(PathBuf::from);
        assert_eq!(numstat_paths(listing), expected);
    }

    /// `touched_paths` for `patch`, in a directory that is not a repo
    /// (`git apply --numstat` reads only the patch).
    fn touched(patch: &str) -> Result<Vec<PathBuf>, Error> {
        let dir = tempfile::tempdir().expect("tempdir");
        touched_paths(dir.path(), patch)
    }

    #[test]
    fn touched_paths_follow_git_not_the_hunk_text() {
        let cases = [
            // a delete
            (
                "diff --git a/.env b/.env\ndeleted file mode 100644\n--- a/.env\n+++ /dev/null\n@@ -1 +0,0 @@\n-S=1\n",
                vec![".env"],
            ),
            // a new empty file: no ---/+++ lines at all
            (
                "diff --git a/.env b/.env\nnew file mode 100644\nindex 0000000..e69de29\n",
                vec![".env"],
            ),
            // a quoted name (a tab in a directory name), decoded by git
            (
                "diff --git \"a/x\\tdir/.env\" \"b/x\\tdir/.env\"\nnew file mode 100644\n--- /dev/null\n+++ \"b/x\\tdir/.env\"\n@@ -0,0 +1 @@\n+T\n",
                vec!["x\tdir/.env"],
            ),
            // a rename: git lists the new name, the header gives the old one
            (
                "diff --git a/.env b/plain.txt\nsimilarity index 100%\nrename from .env\nrename to plain.txt\n",
                vec!["plain.txt", ".env"],
            ),
            // deleting the line `-- .env` is an edit to q.sql only
            (
                "diff --git a/q.sql b/q.sql\n--- a/q.sql\n+++ b/q.sql\n@@ -1,2 +1 @@\n SELECT 1;\n--- .env\n",
                vec!["q.sql"],
            ),
        ];
        for (patch, expected) in cases {
            let mut expected: Vec<PathBuf> = expected.into_iter().map(PathBuf::from).collect();
            let mut paths = touched(patch).expect(patch);
            // The order depends on whether git lists a rename's old name.
            expected.sort();
            paths.sort();
            assert_eq!(paths, expected, "{patch}");
        }
    }

    #[test]
    fn touched_paths_reject_a_quoted_or_control_char_rename_source() {
        let patches = [
            "diff --git \"a/x\\t.env\" b/plain.txt\nsimilarity index 100%\nrename from \"x\\t.env\"\nrename to plain.txt\n",
            // git would end this name at the vertical tab and rename `.env`
            "diff --git a/.env b/plain.txt\nsimilarity index 100%\nrename from .env\x0bignored\nrename to plain.txt\n",
        ];
        for patch in patches {
            let err = touched(patch).expect_err(patch);
            assert!(err.to_string().contains("control character"), "{err}");
        }
    }

    #[test]
    fn touched_paths_reject_a_name_that_is_not_utf8() {
        // A patch is UTF-8 text, so only git's octal escape can name such a
        // file; the listing is then not UTF-8 and the check fails closed.
        let patch = "diff --git \"a/\\377.txt\" \"b/\\377.txt\"\nnew file mode 100644\n--- /dev/null\n+++ \"b/\\377.txt\"\n@@ -0,0 +1 @@\n+x\n";
        touched(patch).expect_err("non-UTF-8 name");
    }

    #[cfg(unix)]
    #[test]
    fn apply_checked_does_not_write_through_a_symlink() {
        // `git apply --check` refuses this path, but the `--3way` fallback
        // wrote through a symlink git does not track, outside the repo. The
        // second case runs in a folder of the repo: git still reads the
        // paths from the top of the working tree.
        for (workdir, link) in [("", "link"), ("sub", "sub/link")] {
            let dir = tempfile::tempdir().expect("tempdir");
            let (repo, outside) = (dir.path().join("repo"), dir.path().join("outside"));
            std::fs::create_dir_all(repo.join("sub")).expect("repo dirs");
            std::fs::create_dir_all(&outside).expect("outside dir");
            git(&repo, &["init", "-q", "-b", "main"]).expect("init");
            std::os::unix::fs::symlink(&outside, repo.join(link)).expect("symlink");
            let patch = format!(
                "diff --git a/{link}/evil.txt b/{link}/evil.txt\nnew file mode 100644\n--- /dev/null\n+++ b/{link}/evil.txt\n@@ -0,0 +1 @@\n+pwned\n"
            );
            let err = apply_checked(&repo.join(workdir), &patch).expect_err(link);
            assert!(err.to_string().contains("symlink"), "{link}: {err}");
            assert!(!outside.join("evil.txt").exists(), "{link}");
        }
    }

    #[test]
    fn apply_checked_still_repairs_wrong_hunk_counts() {
        // Git cannot parse the original (its counts are wrong), so the first
        // path check fails; the repaired patch must still be checked and applied.
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = dir.path();
        git(repo, &["init", "-q", "-b", "main"]).expect("init");
        std::fs::write(repo.join("notes.txt"), "a\nb\nc\n").expect("write");
        let patch = "--- a/notes.txt\n+++ b/notes.txt\n@@ -1,9 +1,9 @@\n a\n-b\n+B\n c\n";
        apply_checked(repo, patch).expect("repaired patch applies");
        let text = std::fs::read_to_string(repo.join("notes.txt")).expect("read");
        assert_eq!(text, "a\nB\nc\n");
    }

    #[test]
    fn apply_checked_rejects_secret_paths_before_any_apply() {
        // The path check runs before git applies anything, so no repo is
        // needed. Before, the `--3way` fallback applied such a patch.
        let patches = [
            "--- /dev/null\n+++ b/.env\n@@ -0,0 +1 @@\n+TOKEN=x\n",
            "diff --git \"a/x\\tdir/.env\" \"b/x\\tdir/.env\"\nnew file mode 100644\n--- /dev/null\n+++ \"b/x\\tdir/.env\"\n@@ -0,0 +1 @@\n+T\n",
        ];
        for patch in patches {
            let dir = tempfile::tempdir().expect("tempdir");
            let err = apply_checked(dir.path(), patch).expect_err(patch);
            assert!(err.to_string().contains("secret-bearing"), "{err}");
            assert!(!dir.path().join(".env").exists());
            assert!(!dir.path().join("x\tdir").exists());
        }
    }
}
