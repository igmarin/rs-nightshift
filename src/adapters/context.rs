//! Context adapter (`adapters/context.rs`): AST / knowledge-graph context via
//! `codegraph` and optional `graphify`.

use crate::error::Error;
use std::path::{Path, PathBuf};

/// Gathered slices for the Tech Lead prompt. Paths are repo-relative.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextBundle {
    /// Combined tool output (never a recursive tree dump).
    pub text: String,
    /// File paths mentioned by the tools.
    pub files: Vec<PathBuf>,
    /// Non-fatal notes (missing graphify, missing graph).
    pub warnings: Vec<String>,
}

/// Subprocess seam for context CLIs.
pub trait ContextProbe: Send + Sync {
    /// `codegraph` is on PATH.
    fn codegraph_available(&self) -> bool;
    /// `graphify` is on PATH.
    fn graphify_available(&self) -> bool;
    /// Target repo already has `.codegraph/`.
    fn has_codegraph_index(&self, repo: &Path) -> bool;
    /// Target repo already has `graphify-out/graph.json`.
    fn has_graphify_graph(&self, repo: &Path) -> bool;
    /// Run `codegraph` with `args` in `repo`.
    fn run_codegraph(&self, repo: &Path, args: &[&str]) -> Result<String, Error>;
    /// Run `graphify` with `args` in `repo`.
    fn run_graphify(&self, repo: &Path, args: &[&str]) -> Result<String, Error>;
}

/// Live PATH + filesystem probe.
pub struct PathProbe;

impl ContextProbe for PathProbe {
    fn codegraph_available(&self) -> bool {
        which::which("codegraph").is_ok()
    }

    fn graphify_available(&self) -> bool {
        which::which("graphify").is_ok()
    }

    fn has_codegraph_index(&self, repo: &Path) -> bool {
        repo.join(".codegraph").is_dir()
    }

    fn has_graphify_graph(&self, repo: &Path) -> bool {
        repo.join("graphify-out").join("graph.json").is_file()
    }

    fn run_codegraph(&self, repo: &Path, args: &[&str]) -> Result<String, Error> {
        run_tool("codegraph", repo, args)
    }

    fn run_graphify(&self, repo: &Path, args: &[&str]) -> Result<String, Error> {
        run_tool("graphify", repo, args)
    }
}

fn run_tool(bin: &str, repo: &Path, args: &[&str]) -> Result<String, Error> {
    let output = std::process::Command::new(bin)
        .args(args)
        .current_dir(repo)
        .output()
        .map_err(|e| Error::Context(format!("failed to spawn {bin}: {e}")))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(Error::Context(format!(
            "{bin} {} failed: {}",
            args.join(" "),
            stderr.trim()
        )));
    }
    String::from_utf8(output.stdout).map_err(|e| Error::Context(e.to_string()))
}

/// Collect AST slices. Never dumps the tree. Never rebuilds graphify.
pub fn gather(probe: &impl ContextProbe, repo: &Path, query: &str) -> Result<ContextBundle, Error> {
    if !probe.codegraph_available() {
        return Err(Error::Context("codegraph is not on PATH".into()));
    }
    let mut warnings = Vec::new();
    let mut chunks = Vec::new();
    if !probe.has_codegraph_index(repo) {
        chunks.push(probe.run_codegraph(repo, &["init", "."])?);
    }
    chunks.push(probe.run_codegraph(repo, &["status", "."])?);
    chunks.push(probe.run_codegraph(repo, &["explore", "-p", ".", query])?);
    chunks.push(probe.run_codegraph(repo, &["impact", "-p", ".", query])?);
    if probe.graphify_available() && probe.has_graphify_graph(repo) {
        chunks.push(probe.run_graphify(repo, &["query", query])?);
    } else if !probe.graphify_available() {
        warnings.push("graphify is not on PATH; continuing without it".into());
    } else {
        warnings.push("graphify-out/graph.json missing; skipping graphify query".into());
    }
    let text = chunks.join("\n");
    let files = extract_paths(&text);
    Ok(ContextBundle {
        text,
        files,
        warnings,
    })
}

/// File-like tokens in tool output (`src/foo.rs`, `Cargo.toml`).
#[must_use]
pub fn extract_paths(text: &str) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for raw in text.split(|c: char| {
        c.is_whitespace()
            || matches!(
                c,
                '`' | '"' | '\'' | ',' | '[' | ']' | '(' | ')' | '{' | '}' | '='
            )
    }) {
        let token = raw.trim_matches(|c: char| matches!(c, ':' | ';' | '.'));
        if looks_like_repo_file(token) {
            files.push(PathBuf::from(token));
        }
    }
    files.sort();
    files.dedup();
    files
}

fn looks_like_repo_file(token: &str) -> bool {
    if token.is_empty() || token.contains("..") || token.starts_with('/') {
        return false;
    }
    let lower = token.to_ascii_lowercase();
    const EXTS: &[&str] = &[".rs", ".toml", ".md", ".yml", ".yaml", ".json", ".txt"];
    EXTS.iter().any(|ext| lower.ends_with(ext))
}

/// `src/cli.rs` is allowed if the tools mentioned that path or a suffix match.
#[must_use]
pub fn path_allowed(path: &Path, allowed: &[PathBuf]) -> bool {
    allowed
        .iter()
        .any(|known| known == path || known.ends_with(path) || path.ends_with(known))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[derive(Default)]
    pub(crate) struct ScriptedProbe {
        pub(crate) codegraph: bool,
        pub(crate) graphify: bool,
        pub(crate) index: bool,
        pub(crate) graph: bool,
        calls: Mutex<Vec<Vec<String>>>,
        replies: Mutex<std::collections::VecDeque<Result<String, Error>>>,
    }

    impl ScriptedProbe {
        pub(crate) fn push(&self, text: &str) {
            self.replies
                .lock()
                .expect("probe")
                .push_back(Ok(text.into()));
        }
    }

    impl ContextProbe for ScriptedProbe {
        fn codegraph_available(&self) -> bool {
            self.codegraph
        }
        fn graphify_available(&self) -> bool {
            self.graphify
        }
        fn has_codegraph_index(&self, _repo: &Path) -> bool {
            self.index
        }
        fn has_graphify_graph(&self, _repo: &Path) -> bool {
            self.graph
        }
        fn run_codegraph(&self, _repo: &Path, args: &[&str]) -> Result<String, Error> {
            self.calls
                .lock()
                .expect("probe")
                .push(args.iter().map(|s| (*s).to_string()).collect());
            self.replies
                .lock()
                .expect("probe")
                .pop_front()
                .unwrap_or_else(|| Err(Error::Context("no codegraph reply".into())))
        }
        fn run_graphify(&self, _repo: &Path, args: &[&str]) -> Result<String, Error> {
            self.calls.lock().expect("probe").push(
                std::iter::once("graphify".to_string())
                    .chain(args.iter().map(|s| (*s).to_string()))
                    .collect(),
            );
            self.replies
                .lock()
                .expect("probe")
                .pop_front()
                .unwrap_or_else(|| Err(Error::Context("no graphify reply".into())))
        }
    }

    #[test]
    fn extract_paths_finds_rust_files() {
        let text = "NODE Cli [src=src/cli.rs] and Cargo.toml plus /etc/passwd and ../escape.rs";
        let files = extract_paths(text);
        assert!(
            files.iter().any(|p| p == Path::new("src/cli.rs")),
            "{files:?}"
        );
        assert!(
            files.iter().any(|p| p == Path::new("Cargo.toml")),
            "{files:?}"
        );
        assert!(!files.iter().any(|p| p.to_string_lossy().contains("passwd")));
        assert!(!files.iter().any(|p| p.to_string_lossy().contains("escape")));
    }

    #[test]
    fn missing_codegraph_fails() {
        let probe = ScriptedProbe::default();
        let err = gather(&probe, Path::new("/tmp"), "goal").expect_err("missing");
        match err {
            Error::Context(msg) => assert!(msg.contains("codegraph"), "{msg}"),
            other => panic!("expected Context, got {other:?}"),
        }
    }

    #[test]
    fn inits_when_index_missing_then_explores() {
        let probe = ScriptedProbe {
            codegraph: true,
            index: false,
            graphify: false,
            ..ScriptedProbe::default()
        };
        probe.push("init ok");
        probe.push("status ok src/lib.rs");
        probe.push("explore src/cli.rs src/pm.rs");
        probe.push("impact src/cli.rs");
        let bundle = gather(&probe, Path::new("/tmp/repo"), "status command").expect("gather");
        let calls = probe.calls.lock().expect("calls").clone();
        assert_eq!(calls[0][0], "init");
        assert!(calls
            .iter()
            .any(|c| c.first().map(String::as_str) == Some("explore")));
        assert!(calls
            .iter()
            .any(|c| c.first().map(String::as_str) == Some("impact")));
        assert!(calls
            .iter()
            .any(|c| c.first().map(String::as_str) == Some("status")));
        assert!(bundle.files.iter().any(|p| p == Path::new("src/cli.rs")));
        assert!(bundle
            .warnings
            .iter()
            .any(|w| w.to_ascii_lowercase().contains("graphify")));
        assert!(!calls
            .iter()
            .any(|c| c.first().map(String::as_str) == Some("graphify")));
    }

    #[test]
    fn graphify_query_only_when_graph_exists() {
        let probe = ScriptedProbe {
            codegraph: true,
            index: true,
            graphify: true,
            graph: true,
            ..ScriptedProbe::default()
        };
        probe.push("status");
        probe.push("explore src/artifacts.rs");
        probe.push("impact src/artifacts.rs");
        probe.push("graphify src/artifacts.rs");
        let bundle = gather(&probe, Path::new("/tmp/repo"), "artifacts").expect("gather");
        let calls = probe.calls.lock().expect("calls").clone();
        assert!(calls
            .iter()
            .any(|c| c.first().map(String::as_str) == Some("graphify")));
        assert!(!calls
            .iter()
            .any(|c| c.iter().any(|a| a == "--update" || a == "build")));
        assert!(bundle
            .files
            .iter()
            .any(|p| p == Path::new("src/artifacts.rs")));
        assert!(bundle.warnings.is_empty());
    }
}
