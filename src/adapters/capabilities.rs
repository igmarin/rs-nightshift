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
use crate::domain::rolegraph::config::is_secret_path;
use crate::error::{ArtifactError, Error};
use crate::ports::{ContextProvider, PendingChunks, ToolReport, ToolRunner};
use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Runs declared capabilities against a target repo.
#[derive(Debug, Clone)]
pub struct CapabilityRunner {
    /// Per-run test timeout.
    test_timeout: Duration,
    /// The run directory that holds `write-file` chunk buffers.
    run_dir: Option<PathBuf>,
}

impl CapabilityRunner {
    /// A runner with the default test timeout.
    #[must_use]
    pub fn new() -> Self {
        Self {
            test_timeout: DEFAULT_TEST_TIMEOUT,
            run_dir: None,
        }
    }

    /// A runner with an explicit test timeout (used in tests).
    #[must_use]
    pub fn with_test_timeout(test_timeout: Duration) -> Self {
        Self {
            test_timeout,
            run_dir: None,
        }
    }

    /// A runner that keeps `write-file` chunk buffers under `run`, the run
    /// directory, so they survive between model calls of the same run.
    #[must_use]
    pub fn with_run_dir(mut self, run: &Path) -> Self {
        self.run_dir = Some(run.to_path_buf());
        self
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

    /// Write `input` as full file content to `repo`, creating the file (and
    /// its parent directories) when it does not exist. The input must start
    /// with a header line `file: <path>` followed by the file content. The
    /// content is considered to end at the first sentinel line that looks
    /// like another context marker (`<!-- file:`, `<!-- end file content -->`,
    /// or `file:`) because small models sometimes echo the injected context
    /// markers.
    ///
    /// A chunked input (`file: <path> chunk: N/M` sections) is buffered in the
    /// run directory and written only once every chunk of the file is present;
    /// see [`write_chunk_section`].
    async fn write_file(&self, repo: &Path, input: &str) -> Result<ToolReport, Error> {
        let repo = repo.to_path_buf();
        let content = input.to_string();
        let run_dir = self.run_dir.clone();
        tokio::task::spawn_blocking(move || {
            if is_chunked(&content) {
                return write_chunk_section(&repo, run_dir.as_deref(), &content);
            }
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
            let file_path = resolve_writable_repo_file(&repo, path)?;
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
            // A text file ends with a newline; without it formatters and
            // `git diff` flag every file this tool writes.
            let mut body = body_lines.join("\n");
            if !body.is_empty() {
                body.push('\n');
            }
            let body_len = body.len();
            if let Some(dir) = file_path.parent() {
                std::fs::create_dir_all(dir).map_err(|error| {
                    ArtifactError::artifact(format!("write-file: create directory: {error}"))
                })?;
            }
            write_atomically(&file_path, &body, "write-file")?;
            Ok(ToolReport::plain(format!(
                "wrote {path} ({body_len} bytes)"
            )))
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
pub(crate) struct SearchReplaceBlock {
    pub(crate) path: String,
    pub(crate) old: String,
    pub(crate) new: String,
}

#[derive(Clone, Copy)]
enum HeaderKind {
    File,
    Old,
    New,
}

#[derive(Clone, Copy)]
enum BodyField {
    Old,
    New,
}

/// Parse `file:` / `old:` / `new:` blocks from a role's `content` payload.
pub(crate) fn parse_search_replace(input: &str) -> Result<Vec<SearchReplaceBlock>, Error> {
    let mut replacements = Vec::new();
    let mut file: Option<String> = None;
    let mut old: Option<String> = None;
    let mut new: Option<String> = None;
    let mut field: Option<BodyField> = None;
    let mut unused_file = false;

    let flush = |file: &Option<String>,
                 old: &mut Option<String>,
                 new: &mut Option<String>,
                 unused_file: &mut bool,
                 replacements: &mut Vec<SearchReplaceBlock>|
     -> Result<(), Error> {
        match (file.as_ref(), old.take(), new.take()) {
            (None | Some(_), None, None) => Ok(()),
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
                *unused_file = false;
                Ok(())
            }
        }
    };

    for line in input.lines() {
        if let Some((kind, rest)) = header_line(line) {
            match kind {
                HeaderKind::File => {
                    flush(
                        &file,
                        &mut old,
                        &mut new,
                        &mut unused_file,
                        &mut replacements,
                    )?;
                    if unused_file {
                        return Err(Error::from(ArtifactError::artifact(
                            "search-replace: file: header has no old:/new: block",
                        )));
                    }
                    file = Some(rest.to_string());
                    field = None;
                    unused_file = true;
                }
                HeaderKind::Old => {
                    if old.is_some() {
                        flush(
                            &file,
                            &mut old,
                            &mut new,
                            &mut unused_file,
                            &mut replacements,
                        )?;
                    }
                    old = Some(rest.to_string());
                    new = None;
                    field = Some(BodyField::Old);
                }
                HeaderKind::New => {
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
                    field = Some(BodyField::New);
                }
            }
            continue;
        }
        match field {
            Some(BodyField::Old) => {
                let buf = old.get_or_insert_with(String::new);
                append_body_line(buf, line);
            }
            Some(BodyField::New) => {
                let buf = new.get_or_insert_with(String::new);
                append_body_line(buf, line);
            }
            None if line.trim().is_empty() => {}
            None => {
                return Err(Error::from(ArtifactError::artifact(
                    "search-replace: stray content outside old:/new: blocks",
                )));
            }
        }
    }
    flush(
        &file,
        &mut old,
        &mut new,
        &mut unused_file,
        &mut replacements,
    )?;
    if unused_file {
        return Err(Error::from(ArtifactError::artifact(
            "search-replace: file: header has no old:/new: block",
        )));
    }
    if replacements.is_empty() {
        return Err(Error::from(ArtifactError::artifact(
            "search-replace: no replacements in input",
        )));
    }
    Ok(replacements)
}

fn header_line(line: &str) -> Option<(HeaderKind, &str)> {
    for (kind, prefix) in [
        (HeaderKind::File, "file:"),
        (HeaderKind::Old, "old:"),
        (HeaderKind::New, "new:"),
    ] {
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

/// Reject a repo-relative path that is empty, absolute, contains `..`, or
/// names a secret-bearing path. Looks at the text only, not the filesystem.
fn check_relative_path(rel: &str, tool: &str) -> Result<(), Error> {
    let path = Path::new(rel);
    if rel.is_empty()
        || path.is_absolute()
        || path
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return Err(Error::from(ArtifactError::artifact(format!(
            "{tool}: path escapes repo root: {rel}"
        ))));
    }
    if is_secret_path(rel) {
        return Err(Error::from(ArtifactError::artifact(format!(
            "{tool}: path is a secret-bearing path: {rel}"
        ))));
    }
    Ok(())
}

/// Resolve `rel` for `write-file`: an existing file (the rules of
/// [`resolve_repo_file`]) or a file that does not exist yet.
///
/// For a new file the deepest existing ancestor is canonicalized, so a
/// symlinked directory cannot move the write outside the repo or into a
/// secret-bearing path. The missing directories are not created here.
fn resolve_writable_repo_file(repo: &Path, rel: &str) -> Result<PathBuf, Error> {
    const TOOL: &str = "write-file";
    check_relative_path(rel, TOOL)?;
    let file_path = repo.join(rel);
    // `symlink_metadata` also sees a dangling symlink, which must go through
    // the existing-file rules instead of being replaced.
    if file_path.symlink_metadata().is_ok() {
        return resolve_repo_file(repo, rel, TOOL);
    }
    let repo_canonical = repo
        .canonicalize()
        .map_err(|error| ArtifactError::artifact(format!("{TOOL}: invalid repo: {error}")))?;
    // `rel` is relative with no `..`, so the walk ends at `repo` at the latest.
    let ancestor = file_path
        .ancestors()
        .skip(1)
        .find(|dir| dir.symlink_metadata().is_ok())
        .unwrap_or(repo);
    let ancestor_canonical = ancestor
        .canonicalize()
        .map_err(|error| ArtifactError::artifact(format!("{TOOL}: invalid path {rel}: {error}")))?;
    if !ancestor_canonical.starts_with(&repo_canonical) {
        return Err(Error::from(ArtifactError::artifact(format!(
            "{TOOL}: path escapes repo root"
        ))));
    }
    if !ancestor_canonical.is_dir() {
        return Err(Error::from(ArtifactError::artifact(format!(
            "{TOOL}: not a directory: {}",
            ancestor.strip_prefix(repo).unwrap_or(ancestor).display()
        ))));
    }
    let rest = file_path.strip_prefix(ancestor).unwrap_or(Path::new(rel));
    let target = ancestor_canonical.join(rest);
    let target_rel = target.strip_prefix(&repo_canonical).unwrap_or(&target);
    if is_secret_path(&target_rel.to_string_lossy()) {
        return Err(Error::from(ArtifactError::artifact(format!(
            "{TOOL}: path resolves to secret-bearing path {}",
            target_rel.display()
        ))));
    }
    Ok(target)
}

/// Resolve `rel` to an existing file inside `repo`. Rejects empty, absolute,
/// `..`, and secret paths.
fn resolve_repo_file(repo: &Path, rel: &str, tool: &str) -> Result<PathBuf, Error> {
    check_relative_path(rel, tool)?;
    let file_path = repo.join(rel);
    let canonical = file_path
        .canonicalize()
        .map_err(|error| ArtifactError::artifact(format!("{tool}: invalid path {rel}: {error}")))?;
    let repo_canonical = repo
        .canonicalize()
        .map_err(|error| ArtifactError::artifact(format!("{tool}: invalid repo: {error}")))?;
    if !canonical.starts_with(&repo_canonical) {
        return Err(Error::from(ArtifactError::artifact(format!(
            "{tool}: path escapes repo root"
        ))));
    }
    let rel_canonical = canonical
        .strip_prefix(&repo_canonical)
        .unwrap_or(&canonical);
    let rel_canonical = rel_canonical.to_string_lossy();
    if is_secret_path(&rel_canonical) {
        return Err(Error::from(ArtifactError::artifact(format!(
            "{tool}: path resolves to secret-bearing path {rel_canonical}"
        ))));
    }
    if !canonical.is_file() {
        return Err(Error::from(ArtifactError::artifact(format!(
            "{tool}: not a file: {rel}"
        ))));
    }
    Ok(canonical)
}

/// How many match lines an "ambiguous" error lists.
const SHOWN_MATCHES: usize = 5;

/// In how many places `needle` starts in `haystack`, overlapping places
/// included, and the 1-based line of the first [`SHOWN_MATCHES`] of them.
///
/// `"aaa"` contains `"aa"` twice; `str::matches` would report one and miss
/// the ambiguity.
fn match_lines(haystack: &str, needle: &str) -> (usize, Vec<usize>) {
    let mut lines = Vec::new();
    if needle.is_empty() {
        return (0, lines);
    }
    // `line` is the line of `counted`: newlines are counted once, not again
    // from the start for each match.
    let (mut count, mut from, mut counted, mut line) = (0, 0, 0, 1);
    while let Some(offset) = haystack[from..].find(needle) {
        let at = from + offset;
        if lines.len() < SHOWN_MATCHES {
            line += haystack[counted..at].matches('\n').count();
            counted = at;
            lines.push(line);
        }
        count += 1;
        from = at + haystack[at..].chars().next().map_or(1, char::len_utf8);
    }
    (count, lines)
}

/// Why `old` is not in `body`, written for the role's next attempt.
///
/// Lines `old` up with the place in the file that it matches for the most
/// lines (leading and trailing whitespace ignored), then names the first
/// line that is not exactly equal.
fn where_old_differs(body: &str, old: &str) -> String {
    // Long lines are cut: the text goes back into a prompt.
    let shown = |line: &str| line.chars().take(160).collect::<String>();
    let same = |a: &str, b: &str| a.trim() == b.trim();
    let file: Vec<&str> = body.lines().collect();
    let old: Vec<&str> = old.lines().collect();
    let Some(first) = old.first() else {
        return String::new();
    };
    let run = |start: usize| {
        old.iter()
            .zip(&file[start..])
            .take_while(|(old, file)| same(old, file))
            .count()
    };
    // shortcut: only the first MAX_STARTS places where the first line of
    // `old` occurs are tried, so a file of repeated lines cannot make this
    // slow. A place after those gives a less exact message, nothing worse.
    const MAX_STARTS: usize = 1000;
    let best = (0..file.len())
        .filter(|&start| same(file[start], first))
        .take(MAX_STARTS)
        .max_by_key(|&start| (run(start), std::cmp::Reverse(start)));
    let Some(start) = best else {
        return format!(
            "no line of the file matches the first line of old: {}",
            shown(first)
        );
    };
    let equal = old
        .iter()
        .zip(&file[start..])
        .take_while(|(old, file)| old == file)
        .count();
    let from = start + 1;
    let Some(wanted) = old.get(equal) else {
        return format!(
            "old matches the file from line {from} line by line, but not as one text; \
             check the line endings"
        );
    };
    let Some(actual) = file.get(start + equal) else {
        return format!(
            "old matches the file from line {from} for {equal} line(s), but the file ends \
             after line {}.\n  old line {}: {}",
            file.len(),
            equal + 1,
            shown(wanted)
        );
    };
    let note = if same(wanted, actual) {
        " (only the whitespace differs)"
    } else {
        ""
    };
    format!(
        "old matches the file from line {from} for {equal} line(s), then differs{note}.\n  \
         old line {}: {}\n  file line {}: {}",
        equal + 1,
        shown(wanted),
        from + equal,
        shown(actual)
    )
}

fn write_atomically(path: &Path, body: &str, tool: &str) -> Result<(), Error> {
    let dir = path.parent().ok_or_else(|| {
        ArtifactError::artifact(format!("{tool}: missing parent for {}", path.display()))
    })?;
    let mut tmp = tempfile::NamedTempFile::new_in(dir)
        .map_err(|error| ArtifactError::artifact(format!("{tool}: temp file: {error}")))?;
    tmp.write_all(body.as_bytes())
        .map_err(|error| ArtifactError::artifact(format!("{tool}: {error}")))?;
    tmp.flush()
        .map_err(|error| ArtifactError::artifact(format!("{tool}: {error}")))?;
    tmp.persist(path)
        .map_err(|error| ArtifactError::artifact(format!("{tool}: persist: {error}")))?;
    Ok(())
}

/// The most chunks one file may be split into.
const MAX_CHUNKS: usize = 1000;

/// True when `input` opens with a chunked `file: <path> chunk: N/M` header.
fn is_chunked(input: &str) -> bool {
    input
        .lines()
        .next()
        .and_then(|line| line.strip_prefix("file: "))
        .is_some_and(|rest| rest.contains(" chunk: "))
}

/// One reply's chunks of a single file. Each body is the raw bytes the reply
/// sent after its header line, kept exactly: nothing is trimmed or re-joined.
struct ChunkedWrite {
    /// The repo-relative path, as the model wrote it.
    path: String,
    /// The total chunk count `M` every section declares.
    total: usize,
    /// `(N, body)` for each section, in the order the reply gave them.
    chunks: Vec<(usize, String)>,
}

/// Parse a chunked `write-file` input: one `file: <path> chunk: N/M` header
/// per section, each followed by that chunk's content. All sections must
/// name the same path and total, and no number may repeat within the reply.
fn parse_chunked_write(input: &str) -> Result<ChunkedWrite, Error> {
    // (path, number, total, raw body) per section.
    let mut sections: Vec<(&str, usize, usize, String)> = Vec::new();
    // Split keeping each line's terminator, so bodies are copied byte for byte.
    for raw in input.split_inclusive('\n') {
        let line = raw.trim_end_matches(['\n', '\r']);
        // Same sentinels as a plain write: the model echoed a context marker.
        if line.starts_with("<!-- file:") || line.starts_with("<!-- end file content -->") {
            break;
        }
        if let Some(rest) = line.strip_prefix("file: ") {
            let (path, number, total) = parse_chunk_header(rest)?;
            sections.push((path, number, total, String::new()));
        } else if let Some(section) = sections.last_mut() {
            section.3.push_str(raw);
        } else {
            return Err(Error::from(ArtifactError::artifact(
                "write-file: input must start with 'file: <path> chunk: N/M'",
            )));
        }
    }
    let Some(&(path, _, total, _)) = sections.first() else {
        return Err(Error::from(ArtifactError::artifact(
            "write-file: input must start with 'file: <path> chunk: N/M'",
        )));
    };
    let mut chunks: Vec<(usize, String)> = Vec::with_capacity(sections.len());
    for (section_path, number, section_total, body) in sections {
        if section_path != path || section_total != total {
            return Err(Error::from(ArtifactError::artifact(format!(
                "write-file: a reply carries chunks of one file and one total; \
                 got {path} chunk N/{total} and {section_path} chunk N/{section_total}"
            ))));
        }
        if chunks.iter().any(|(seen, _)| *seen == number) {
            return Err(Error::from(ArtifactError::artifact(format!(
                "write-file: chunk {number}/{total} of {path} appears twice in one reply"
            ))));
        }
        // Blank content is refused, but not trimmed: a chunk that is only
        // whitespace is an empty chunk, and anything else is kept as sent.
        if body.trim().is_empty() {
            return Err(Error::from(ArtifactError::artifact(format!(
                "write-file: chunk {number}/{total} of {path} is empty"
            ))));
        }
        chunks.push((number, body));
    }
    Ok(ChunkedWrite {
        path: path.to_string(),
        total,
        chunks,
    })
}

/// Parse the text after `file: ` in a chunk header: `<path> chunk: N/M`.
fn parse_chunk_header(rest: &str) -> Result<(&str, usize, usize), Error> {
    let bad = || {
        Error::from(ArtifactError::artifact(format!(
            "write-file: chunk header must be 'file: <path> chunk: N/M' with whole numbers, got: {rest}"
        )))
    };
    let (path, spec) = rest.rsplit_once(" chunk: ").ok_or_else(bad)?;
    let (number, total) = spec.trim().split_once('/').ok_or_else(bad)?;
    let number: usize = number.trim().parse().map_err(|_| bad())?;
    let total: usize = total.trim().parse().map_err(|_| bad())?;
    let path = path.trim();
    if number == 0 || number > total || total == 0 || total > MAX_CHUNKS {
        return Err(Error::from(ArtifactError::artifact(format!(
            "write-file: chunk {number}/{total} of {path} is out of range: \
             the number runs from 1 to the total, and the total from 1 to {MAX_CHUNKS}"
        ))));
    }
    Ok((path, number, total))
}

/// Buffer one reply's chunks under the run directory and write the file once
/// every chunk of its total is present. Until then nothing is written to the
/// repo and the tool succeeds, reporting which chunks are still missing.
fn write_chunk_section(repo: &Path, run: Option<&Path>, input: &str) -> Result<ToolReport, Error> {
    let run = run.ok_or_else(|| {
        Error::from(ArtifactError::artifact(
            "write-file: chunked input needs a run directory; this runner has none",
        ))
    })?;
    let write = parse_chunked_write(input)?;
    // The path rules run before anything is buffered, so a rejected path
    // leaves no chunks behind.
    let file_path = resolve_writable_repo_file(repo, &write.path)?;

    let buffer = run.join("chunks").join(buffer_key(&write.path));
    let dir = buffer.join(write.total.to_string());
    std::fs::create_dir_all(&dir)
        .map_err(|error| ArtifactError::artifact(format!("write-file: chunk buffer: {error}")))?;
    for (number, body) in &write.chunks {
        write_atomically(&dir.join(number.to_string()), body, "write-file")?;
    }

    let missing: Vec<usize> = (1..=write.total)
        .filter(|number| !dir.join(number.to_string()).is_file())
        .collect();
    if !missing.is_empty() {
        let pending = PendingChunks {
            path: write.path.clone(),
            missing,
            total: write.total,
        };
        return Ok(ToolReport {
            text: format!(
                "stored chunks of {}; file not written yet, missing {} of {}",
                write.path,
                pending.missing_list(),
                write.total
            ),
            pending: Some(pending),
        });
    }

    let mut body = String::new();
    for number in 1..=write.total {
        let part = std::fs::read_to_string(dir.join(number.to_string())).map_err(|error| {
            ArtifactError::artifact(format!("write-file: read chunk {number}: {error}"))
        })?;
        body.push_str(&part);
    }
    if let Some(parent) = file_path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| {
            ArtifactError::artifact(format!("write-file: create directory: {error}"))
        })?;
    }
    write_atomically(&file_path, &body, "write-file")?;
    std::fs::remove_dir_all(&buffer).map_err(|error| {
        ArtifactError::artifact(format!("write-file: clear chunk buffer: {error}"))
    })?;
    Ok(ToolReport::plain(format!(
        "wrote {} ({} bytes) from {} chunks",
        write.path,
        body.len(),
        write.total
    )))
}

/// A buffer directory name for `path`: its bytes in hex, so no path can
/// name a directory outside the run's `chunks` folder.
fn buffer_key(path: &str) -> String {
    path.bytes().map(|byte| format!("{byte:02x}")).collect()
}

fn apply_search_replace(repo: &Path, input: &str) -> Result<String, Error> {
    let replacements = parse_search_replace(input)?;
    let mut abs_by_rel: HashMap<String, PathBuf> = HashMap::new();
    for block in &replacements {
        if abs_by_rel.contains_key(&block.path) {
            continue;
        }
        abs_by_rel.insert(
            block.path.clone(),
            resolve_repo_file(repo, &block.path, "search-replace")?,
        );
    }

    let mut contents: HashMap<String, String> = HashMap::new();
    for (rel, abs) in &abs_by_rel {
        let body = std::fs::read_to_string(abs).map_err(|error| {
            ArtifactError::artifact(format!("search-replace: failed to read {rel}: {error}"))
        })?;
        contents.insert(rel.clone(), body);
    }

    // Kept so a failed write can put back the files already written.
    let originals = contents.clone();
    for block in &replacements {
        let body = contents.get_mut(&block.path).ok_or_else(|| {
            ArtifactError::artifact(format!(
                "search-replace: missing content for {}",
                block.path
            ))
        })?;
        // A failure says where to look: the message is the role's only
        // guide for its next attempt.
        let (count, lines) = match_lines(body, &block.old);
        match count {
            0 => {
                return Err(Error::from(ArtifactError::artifact(format!(
                    "search-replace: old_text not found in {}. {}\nQuote the file as it is now.",
                    block.path,
                    where_old_differs(body, &block.old)
                ))));
            }
            1 => {
                *body = body.replacen(&block.old, &block.new, 1);
            }
            n => {
                let shown: Vec<String> = lines.iter().map(usize::to_string).collect();
                return Err(Error::from(ArtifactError::artifact(format!(
                    "search-replace: old_text matches {n} locations in {} (ambiguous), at lines {}. \
                     Add lines before or after it to old, so that it matches one place.",
                    block.path,
                    shown.join(", ")
                ))));
            }
        }
    }

    // Write in path order. If one write fails, restore the files already
    // written (best effort), so a failed call leaves the repo as it found it
    // and the role can safely try again.
    let mut files: Vec<(&PathBuf, &String, &String)> = Vec::new();
    for (rel, abs) in &abs_by_rel {
        let (Some(body), Some(original)) = (contents.get(rel), originals.get(rel)) else {
            return Err(Error::from(ArtifactError::artifact(format!(
                "search-replace: missing content for {rel}"
            ))));
        };
        files.push((abs, body, original));
    }
    files.sort_by_key(|(abs, _, _)| *abs);
    for (index, (abs, body, _)) in files.iter().enumerate() {
        if let Err(error) = write_atomically(abs, body, "search-replace") {
            for (done, _, original) in files.iter().take(index) {
                let _ = write_atomically(done, original, "search-replace");
            }
            return Err(error);
        }
    }

    let n = replacements.len();
    let mut paths: Vec<&str> = replacements.iter().map(|b| b.path.as_str()).collect();
    paths.sort_unstable();
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
    /// Dispatch a declared capability for `repo`, keeping only its text.
    async fn run(&self, tool: &str, repo: &Path, input: &str) -> Result<String, Error> {
        match tool {
            "run-tests" => self.run_tests(repo).await,
            "apply-patch" => self.apply_patch(repo, input).await,
            "write-file" => self.write_file(repo, input).await.map(|report| report.text),
            "search-replace" => self.search_replace(repo, input).await,
            other => Err(Error::from(ArtifactError::artifact(format!(
                "unknown tool {other:?}"
            )))),
        }
    }

    /// Dispatch a declared capability for `repo`, with what it left pending.
    async fn run_report(&self, tool: &str, repo: &Path, input: &str) -> Result<ToolReport, Error> {
        match tool {
            "write-file" => self.write_file(repo, input).await,
            _ => self.run(tool, repo, input).await.map(ToolReport::plain),
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

    #[cfg(unix)]
    #[tokio::test]
    async fn search_replace_restores_written_files_when_a_later_write_fails() {
        use std::os::unix::fs::PermissionsExt;
        let repo = init_repo();
        let locked = repo.path().join("locked");
        std::fs::create_dir(&locked).expect("mkdir");
        std::fs::write(locked.join("b.txt"), "keep\n").expect("write");
        // A read-only directory: b.txt can be read but not replaced.
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o555)).expect("chmod");
        let input = "\
file: hello.txt
old: hello
new: changed
file: locked/b.txt
old: keep
new: changed
";
        let result = CapabilityRunner::new()
            .run("search-replace", repo.path(), input)
            .await;
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        // Root ignores directory permissions, so the write cannot be made to fail there.
        let Err(err) = result else { return };
        assert!(err.to_string().contains("search-replace"), "{err}");
        // hello.txt sorts first, so it was written and then restored.
        let hello = std::fs::read_to_string(repo.path().join("hello.txt")).expect("read");
        assert_eq!(hello, "hello\n");
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

    #[tokio::test]
    async fn search_replace_overlapping_match_is_ambiguous() {
        let repo = init_repo();
        std::fs::write(repo.path().join("hello.txt"), "aaa\n").expect("write");
        let runner = CapabilityRunner::new();
        let err = runner
            .run(
                "search-replace",
                repo.path(),
                &search_replace_input("hello.txt", "aa", "b"),
            )
            .await
            .expect_err("overlapping");
        assert!(err.to_string().contains("ambiguous"), "{err}");
        let body = std::fs::read_to_string(repo.path().join("hello.txt")).expect("read");
        assert_eq!(body, "aaa\n");
    }

    #[tokio::test]
    async fn search_replace_rejects_secret_path() {
        let repo = init_repo();
        let runner = CapabilityRunner::new();
        let err = runner
            .run(
                "search-replace",
                repo.path(),
                &search_replace_input(".git/config", "[core]", "[pwned]"),
            )
            .await
            .expect_err("secret");
        assert!(err.to_string().contains("secret"), "{err}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn search_replace_rejects_symlink_to_secret() {
        let repo = init_repo();
        std::fs::write(repo.path().join(".env"), "SECRET=1\n").expect("write");
        std::os::unix::fs::symlink(repo.path().join(".env"), repo.path().join("link.txt"))
            .expect("symlink");
        let runner = CapabilityRunner::new();
        let err = runner
            .run(
                "search-replace",
                repo.path(),
                &search_replace_input("link.txt", "SECRET=1", "SECRET=2"),
            )
            .await
            .expect_err("symlink secret");
        assert!(err.to_string().contains("secret"), "{err}");
        let body = std::fs::read_to_string(repo.path().join(".env")).expect("read");
        assert_eq!(body, "SECRET=1\n");
    }

    #[tokio::test]
    async fn search_replace_trailing_file_header_is_an_error() {
        let repo = init_repo();
        let runner = CapabilityRunner::new();
        let input = "\
file: hello.txt
old: hello
new: hi
file: leftover.txt
";
        let err = runner
            .run("search-replace", repo.path(), input)
            .await
            .expect_err("trailing file");
        assert!(err.to_string().contains("file:"), "{err}");
        let body = std::fs::read_to_string(repo.path().join("hello.txt")).expect("read");
        assert_eq!(body, "hello\n");
    }

    #[tokio::test]
    async fn write_file_rejects_secret_path() {
        let repo = init_repo();
        let runner = CapabilityRunner::new();
        let err = runner
            .run("write-file", repo.path(), "file: .git/config\npwned\n")
            .await
            .expect_err("secret");
        assert!(err.to_string().contains("secret"), "{err}");
    }

    #[tokio::test]
    async fn write_file_creates_a_new_file_and_its_directories() {
        let repo = init_repo();
        let runner = CapabilityRunner::new();
        let out = runner
            .run(
                "write-file",
                repo.path(),
                "file: .github/ISSUE_TEMPLATE/task.md\n## Goal\n\nText\n",
            )
            .await
            .expect("create");
        assert!(out.contains(".github/ISSUE_TEMPLATE/task.md"), "{out}");
        let body = std::fs::read_to_string(repo.path().join(".github/ISSUE_TEMPLATE/task.md"))
            .expect("read");
        assert_eq!(body, "## Goal\n\nText\n", "one final newline");

        // An existing file is still replaced as a whole.
        runner
            .run("write-file", repo.path(), "file: hello.txt\nbye\n")
            .await
            .expect("overwrite");
        let body = std::fs::read_to_string(repo.path().join("hello.txt")).expect("read");
        assert_eq!(body, "bye\n");
    }

    #[tokio::test]
    async fn write_file_rejects_a_new_file_below_a_file() {
        let repo = init_repo();
        let err = CapabilityRunner::new()
            .run("write-file", repo.path(), "file: hello.txt/new.md\nx\n")
            .await
            .expect_err("parent is a file");
        assert!(err.to_string().contains("not a directory"), "{err}");
    }

    #[tokio::test]
    async fn write_file_rejects_a_new_secret_file() {
        let repo = init_repo();
        let err = CapabilityRunner::new()
            .run("write-file", repo.path(), "file: config/.env\nKEY=1\n")
            .await
            .expect_err("secret");
        assert!(err.to_string().contains("secret"), "{err}");
        assert!(!repo.path().join("config").exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn write_file_rejects_a_new_file_through_an_escaping_symlink() {
        let repo = init_repo();
        let outside = tempfile::tempdir().expect("outside");
        std::os::unix::fs::symlink(outside.path(), repo.path().join("out")).expect("symlink");
        std::os::unix::fs::symlink(repo.path().join(".git"), repo.path().join("meta"))
            .expect("symlink");
        std::os::unix::fs::symlink(outside.path().join("missing"), repo.path().join("dangling"))
            .expect("symlink");
        let runner = CapabilityRunner::new();

        let err = runner
            .run("write-file", repo.path(), "file: out/sub/new.md\nx\n")
            .await
            .expect_err("escapes");
        assert!(err.to_string().contains("escapes repo root"), "{err}");
        assert!(!outside.path().join("sub").exists());

        let err = runner
            .run("write-file", repo.path(), "file: meta/hooks/pre-push\nx\n")
            .await
            .expect_err("secret through symlink");
        assert!(err.to_string().contains("secret"), "{err}");
        assert!(!repo.path().join(".git/hooks/pre-push").exists());

        // A dangling symlink is not treated as a free name.
        runner
            .run("write-file", repo.path(), "file: dangling\nx\n")
            .await
            .expect_err("dangling symlink");
        assert!(!outside.path().join("missing").exists());
    }

    #[test]
    fn match_lines_finds_sliding_windows_and_their_lines() {
        assert_eq!(match_lines("aaa", "aa"), (2, vec![1, 1]));
        assert_eq!(match_lines("aaaa", "aa").0, 3);
        assert_eq!(match_lines("hello", "hello"), (1, vec![1]));
        assert_eq!(match_lines("hello", "nope"), (0, vec![]));
        assert_eq!(
            match_lines("x = 1\ny = 2\nx = 1\n", "x = 1"),
            (2, vec![1, 3])
        );
        assert_eq!(match_lines("a\n\na a\nb\na", "a"), (4, vec![1, 3, 3, 5]));
        // Every match is counted; only the first few lines are kept.
        let big = "x\n".repeat(200_000);
        assert_eq!(match_lines(&big, "x"), (200_000, vec![1, 2, 3, 4, 5]));
    }

    #[test]
    fn where_old_differs_names_the_first_line_that_differs() {
        // The file changed after the model last read it: `old` is stale.
        let body = "fn a() {\n    one();\n    added();\n    two();\n}\n";
        let old = "fn a() {\n    one();\n    two();\n}";
        let why = where_old_differs(body, old);
        assert!(why.contains("from line 1 for 2 line(s)"), "{why}");
        assert!(why.contains("old line 3:     two();"), "{why}");
        assert!(why.contains("file line 3:     added();"), "{why}");
        assert!(!why.contains("whitespace"), "{why}");
    }

    #[test]
    fn where_old_differs_says_when_only_whitespace_differs() {
        let body = "fn a() {\n    one();\n}\n";
        let why = where_old_differs(body, "fn a() {\n  one();\n}");
        assert!(why.contains("from line 1 for 1 line(s)"), "{why}");
        assert!(why.contains("old line 2:   one();"), "{why}");
        assert!(why.contains("file line 2:     one();"), "{why}");
        assert!(why.contains("only the whitespace differs"), "{why}");
    }

    #[test]
    fn where_old_differs_takes_the_longest_run_and_handles_the_edges() {
        // The second `a` starts the longer run.
        let why = where_old_differs("a\nx\na\nb\nq\n", "a\nb\nc");
        assert!(why.contains("from line 3 for 2 line(s)"), "{why}");
        assert!(why.contains("file line 5: q"), "{why}");
        // `old` runs past the end of the file.
        let why = where_old_differs("a\nb\n", "a\nb\nc");
        assert!(why.contains("the file ends after line 2"), "{why}");
        // Nothing to line up with.
        let why = where_old_differs("hello\n", "nope");
        assert!(
            why.contains("no line of the file matches the first line of old: nope"),
            "{why}"
        );
        // A very long line is cut.
        let long = "x".repeat(1000);
        let why = where_old_differs(&format!("a\n{long}\n"), "a\nb");
        assert!(why.len() < 600, "{}", why.len());
        // A file of repeated lines: the search is bounded, and still answers.
        let body = "}\n".repeat(20_000);
        let why = where_old_differs(&body, &format!("{}differs", "}\n".repeat(2_000)));
        assert!(why.contains("for 2000 line(s), then differs"), "{why}");
    }

    #[tokio::test]
    async fn search_replace_errors_say_where_to_look() {
        let repo = init_repo();
        std::fs::write(
            repo.path().join("f.rs"),
            "let x = 1;\nlet y = 2;\nlet x = 1;\n",
        )
        .expect("write");
        let runner = CapabilityRunner::new();
        let err = runner
            .run(
                "search-replace",
                repo.path(),
                &search_replace_input("f.rs", "let x = 1;", "let x = 3;"),
            )
            .await
            .expect_err("ambiguous");
        assert!(
            err.to_string()
                .contains("matches 2 locations in f.rs (ambiguous), at lines 1, 3"),
            "{err}"
        );
        let err = runner
            .run(
                "search-replace",
                repo.path(),
                "file: f.rs\nold:\nlet y = 2;\nlet x = 9;\nnew:\nlet y = 5;\n",
            )
            .await
            .expect_err("not found");
        let text = err.to_string();
        assert!(text.contains("old_text not found in f.rs"), "{text}");
        assert!(text.contains("from line 2 for 1 line(s)"), "{text}");
        assert!(text.contains("file line 3: let x = 1;"), "{text}");
    }

    /// One chunk section: the header line plus its body.
    fn chunk(path: &str, n: usize, m: usize, body: &str) -> String {
        format!("file: {path} chunk: {n}/{m}\n{body}")
    }

    /// A runner whose chunk buffers live under `run`.
    fn chunk_runner(run: &Path) -> CapabilityRunner {
        CapabilityRunner::new().with_run_dir(run)
    }

    #[tokio::test]
    async fn write_file_assembles_two_chunks_in_one_call() {
        let repo = init_repo();
        let run = tempfile::tempdir().expect("run");
        let runner = chunk_runner(run.path());
        let input = format!(
            "{}{}",
            chunk("site/index.html", 1, 2, "<html>\n"),
            chunk("site/index.html", 2, 2, "</html>\n")
        );
        let out = runner
            .run("write-file", repo.path(), &input)
            .await
            .expect("assemble");
        assert!(out.contains("site/index.html"), "{out}");
        let body = std::fs::read_to_string(repo.path().join("site/index.html")).expect("read");
        assert_eq!(body, "<html>\n</html>\n");
    }

    #[tokio::test]
    async fn write_file_assembles_three_chunks_across_three_calls() {
        let repo = init_repo();
        let run = tempfile::tempdir().expect("run");
        let runner = chunk_runner(run.path());
        runner
            .run("write-file", repo.path(), &chunk("big.md", 1, 3, "one\n"))
            .await
            .expect("first call");
        assert!(!repo.path().join("big.md").exists(), "after chunk 1/3");
        runner
            .run("write-file", repo.path(), &chunk("big.md", 2, 3, "two\n"))
            .await
            .expect("second call");
        assert!(!repo.path().join("big.md").exists(), "after chunk 2/3");
        runner
            .run("write-file", repo.path(), &chunk("big.md", 3, 3, "three\n"))
            .await
            .expect("third call");
        let body = std::fs::read_to_string(repo.path().join("big.md")).expect("read");
        assert_eq!(body, "one\ntwo\nthree\n");
    }

    #[tokio::test]
    async fn write_file_does_not_write_while_a_chunk_is_missing() {
        let repo = init_repo();
        let run = tempfile::tempdir().expect("run");
        let runner = chunk_runner(run.path());
        let input = format!(
            "{}{}",
            chunk("gap.md", 1, 3, "first\n"),
            chunk("gap.md", 3, 3, "third\n")
        );
        let out = runner
            .run("write-file", repo.path(), &input)
            .await
            .expect("buffer");
        assert!(!repo.path().join("gap.md").exists(), "must not write");
        assert!(out.contains("missing 2"), "{out}");
    }

    #[tokio::test]
    async fn write_file_assembles_out_of_order_chunks() {
        let repo = init_repo();
        let run = tempfile::tempdir().expect("run");
        let runner = chunk_runner(run.path());
        // Last chunk first, in its own call.
        runner
            .run("write-file", repo.path(), &chunk("ooo.md", 3, 3, "c\n"))
            .await
            .expect("chunk 3");
        runner
            .run("write-file", repo.path(), &chunk("ooo.md", 1, 3, "a\n"))
            .await
            .expect("chunk 1");
        assert!(
            !repo.path().join("ooo.md").exists(),
            "chunk 2 still missing"
        );
        runner
            .run("write-file", repo.path(), &chunk("ooo.md", 2, 3, "b\n"))
            .await
            .expect("chunk 2");
        let body = std::fs::read_to_string(repo.path().join("ooo.md")).expect("read");
        assert_eq!(body, "a\nb\nc\n", "assembled by number, not arrival");

        // Reversed inside one reply.
        let input = format!(
            "{}{}",
            chunk("rev.md", 2, 2, "second\n"),
            chunk("rev.md", 1, 2, "first\n")
        );
        runner
            .run("write-file", repo.path(), &input)
            .await
            .expect("reversed");
        let body = std::fs::read_to_string(repo.path().join("rev.md")).expect("read");
        assert_eq!(body, "first\nsecond\n");
    }

    #[tokio::test]
    async fn write_file_later_chunk_replaces_an_earlier_one() {
        let repo = init_repo();
        let run = tempfile::tempdir().expect("run");
        let runner = chunk_runner(run.path());
        runner
            .run("write-file", repo.path(), &chunk("re.md", 1, 2, "old\n"))
            .await
            .expect("first try");
        runner
            .run("write-file", repo.path(), &chunk("re.md", 1, 2, "new\n"))
            .await
            .expect("resent");
        runner
            .run("write-file", repo.path(), &chunk("re.md", 2, 2, "end\n"))
            .await
            .expect("last");
        let body = std::fs::read_to_string(repo.path().join("re.md")).expect("read");
        assert_eq!(body, "new\nend\n");
    }

    #[tokio::test]
    async fn write_file_clears_the_chunk_buffer_after_assembly() {
        let repo = init_repo();
        let run = tempfile::tempdir().expect("run");
        let runner = chunk_runner(run.path());
        runner
            .run("write-file", repo.path(), &chunk("gone.md", 1, 2, "a\n"))
            .await
            .expect("buffer");
        runner
            .run("write-file", repo.path(), &chunk("gone.md", 2, 2, "b\n"))
            .await
            .expect("assemble");
        let left = std::fs::read_dir(run.path().join("chunks"))
            .map(|entries| entries.count())
            .unwrap_or(0);
        assert_eq!(left, 0, "buffer must be empty once the file is written");
    }

    #[tokio::test]
    async fn write_file_keeps_chunk_bytes_exactly_including_trailing_blank_lines() {
        let repo = init_repo();
        let run = tempfile::tempdir().expect("run");
        let runner = chunk_runner(run.path());
        let input = format!(
            "{}{}",
            chunk("blank.md", 1, 2, "a\n\n\n"),
            chunk("blank.md", 2, 2, "b\r\n\n")
        );
        runner
            .run("write-file", repo.path(), &input)
            .await
            .expect("assemble");
        let bytes = std::fs::read(repo.path().join("blank.md")).expect("read");
        assert_eq!(bytes, b"a\n\n\nb\r\n\n", "bytes must be kept as sent");
    }

    #[tokio::test]
    async fn write_file_report_names_the_missing_chunks_until_assembly() {
        let repo = init_repo();
        let run = tempfile::tempdir().expect("run");
        let runner = chunk_runner(run.path());
        let input = format!(
            "{}{}",
            chunk("gap.md", 1, 3, "first\n"),
            chunk("gap.md", 3, 3, "third\n")
        );
        let report = runner
            .run_report("write-file", repo.path(), &input)
            .await
            .expect("buffer");
        assert_eq!(
            report.pending,
            Some(PendingChunks {
                path: "gap.md".into(),
                missing: vec![2],
                total: 3,
            })
        );
        let report = runner
            .run_report(
                "write-file",
                repo.path(),
                &chunk("gap.md", 2, 3, "second\n"),
            )
            .await
            .expect("assemble");
        assert_eq!(report.pending, None, "nothing pending once written");
    }

    #[tokio::test]
    async fn write_file_rejects_bad_chunk_headers() {
        let repo = init_repo();
        let run = tempfile::tempdir().expect("run");
        let runner = chunk_runner(run.path());
        for (input, expected) in [
            (chunk("x.md", 0, 2, "a\n"), "chunk 0/2"),
            (chunk("x.md", 3, 2, "a\n"), "chunk 3/2"),
            (chunk("x.md", 1, 0, "a\n"), "chunk 1/0"),
            ("file: x.md chunk: one/2\na\n".to_string(), "chunk"),
            ("file: x.md chunk: 1\na\n".to_string(), "chunk"),
            (chunk("x.md", 1, 2, ""), "empty"),
        ] {
            let err = runner
                .run("write-file", repo.path(), &input)
                .await
                .expect_err(&input);
            assert!(err.to_string().contains(expected), "{input:?}: {err}");
        }
        assert!(!repo.path().join("x.md").exists());
    }

    #[tokio::test]
    async fn write_file_rejects_mixed_or_duplicate_chunks_in_one_call() {
        let repo = init_repo();
        let run = tempfile::tempdir().expect("run");
        let runner = chunk_runner(run.path());
        let mixed_totals = format!(
            "{}{}",
            chunk("m.md", 1, 2, "a\n"),
            chunk("m.md", 2, 3, "b\n")
        );
        let err = runner
            .run("write-file", repo.path(), &mixed_totals)
            .await
            .expect_err("mixed totals");
        assert!(err.to_string().contains("one file and one total"), "{err}");

        let mixed_paths = format!(
            "{}{}",
            chunk("m.md", 1, 2, "a\n"),
            chunk("n.md", 2, 2, "b\n")
        );
        let err = runner
            .run("write-file", repo.path(), &mixed_paths)
            .await
            .expect_err("mixed paths");
        assert!(err.to_string().contains("one file and one total"), "{err}");

        let duplicate = format!(
            "{}{}",
            chunk("d.md", 1, 2, "a\n"),
            chunk("d.md", 1, 2, "b\n")
        );
        let err = runner
            .run("write-file", repo.path(), &duplicate)
            .await
            .expect_err("duplicate");
        assert!(err.to_string().contains("twice in one reply"), "{err}");
        assert!(!repo.path().join("d.md").exists());
    }

    #[tokio::test]
    async fn write_file_chunks_keep_the_path_rules() {
        let repo = init_repo();
        let run = tempfile::tempdir().expect("run");
        let runner = chunk_runner(run.path());
        for path in [".git/config", "config/.env", "../outside.md"] {
            let err = runner
                .run("write-file", repo.path(), &chunk(path, 1, 2, "x\n"))
                .await
                .expect_err(path);
            let text = err.to_string();
            assert!(
                text.contains("secret") || text.contains("escapes"),
                "{path}: {text}"
            );
        }
        // A rejected path never reaches the buffer.
        assert!(!run.path().join("chunks").exists(), "no buffer");
    }

    #[tokio::test]
    async fn write_file_chunks_need_a_run_directory() {
        let repo = init_repo();
        let runner = CapabilityRunner::new();
        let err = runner
            .run("write-file", repo.path(), &chunk("n.md", 1, 2, "x\n"))
            .await
            .expect_err("no run dir");
        assert!(err.to_string().contains("run directory"), "{err}");
    }
}
