//! Smoke tests for the packaged `nightshift` binary.
//!
//! These run the real executable that release artifacts ship, so they guard the
//! entry point without needing Ollama, `codegraph`, or `graphify`.

use std::path::Path;
use std::process::{Command, Output};
use tempfile::tempdir;

fn nightshift(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_nightshift"))
        .args(args)
        .output()
        .expect("spawn nightshift")
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[test]
fn version_prints_a_version() {
    let output = nightshift(&["--version"]);
    assert!(output.status.success(), "exit: {:?}", output.status);
    let text = stdout(&output);
    assert!(text.contains(env!("CARGO_PKG_VERSION")), "{text}");
}

#[test]
fn help_lists_commands() {
    let output = nightshift(&["--help"]);
    assert!(output.status.success(), "exit: {:?}", output.status);
    let text = stdout(&output);
    for command in ["doctor", "status", "run"] {
        assert!(text.contains(command), "missing {command} in: {text}");
    }
}

#[test]
fn run_without_goal_and_repo_fails() {
    let output = nightshift(&["run"]);
    assert!(!output.status.success(), "expected failure");
    let text = String::from_utf8_lossy(&output.stderr);
    assert!(text.contains("--goal") || text.contains("--repo"), "{text}");
}

#[test]
fn status_on_empty_out_dir_reports_no_run() {
    let dir = tempdir().expect("tempdir");
    let output = nightshift(&["status", "--out", path_arg(dir.path())]);
    assert_eq!(output.status.code(), Some(2), "exit: {:?}", output.status);
    let text = stdout(&output);
    assert!(text.contains("QA has not run yet"), "{text}");
}

fn path_arg(path: &Path) -> &str {
    path.to_str().expect("utf-8 temp path")
}
