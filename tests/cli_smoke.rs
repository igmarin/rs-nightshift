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
    assert!(
        text.contains("--ollama-url"),
        "missing Ollama URL flag: {text}"
    );
    assert!(
        text.contains("NIGHTSHIFT_OLLAMA_URL"),
        "missing Ollama URL environment variable: {text}"
    );
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

#[test]
fn ollama_url_env_and_flag_precedence_are_consistent() {
    let env_output = nightshift_with_env(&["doctor"], &[("NIGHTSHIFT_OLLAMA_URL", "env invalid")]);
    let env_report = stdout(&env_output);
    assert_eq!(
        env_output.status.code(),
        Some(2),
        "exit: {:?}",
        env_output.status
    );
    assert!(env_report.contains("[FAIL] ollama-url"), "{env_report}");
    assert!(env_report.contains("env invalid"), "{env_report}");

    let flag_output = nightshift_with_env(
        &["doctor", "--ollama-url", "flag invalid"],
        &[("NIGHTSHIFT_OLLAMA_URL", "env invalid")],
    );
    let flag_report = stdout(&flag_output);
    assert_eq!(
        flag_output.status.code(),
        Some(2),
        "exit: {:?}",
        flag_output.status
    );
    assert!(flag_report.contains("[FAIL] ollama-url"), "{flag_report}");
    assert!(flag_report.contains("flag invalid"), "{flag_report}");
    assert!(!flag_report.contains("env invalid"), "{flag_report}");
}

#[test]
fn doctor_rejects_ollama_credentials_without_echoing_userinfo() {
    let output = nightshift(&[
        "doctor",
        "--ollama-url",
        "http://user:secret@127.0.0.1:11434",
    ]);
    let combined = format!(
        "{}{}",
        stdout(&output),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.status.code(), Some(2), "exit: {:?}", output.status);
    assert!(combined.contains("[FAIL] ollama-url"), "{combined}");
    assert!(!combined.contains("user"), "{combined}");
    assert!(!combined.contains("secret"), "{combined}");
}

fn path_arg(path: &Path) -> &str {
    path.to_str().expect("utf-8 temp path")
}

fn nightshift_with_env(args: &[&str], vars: &[(&str, &str)]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_nightshift"));
    command.args(args).env_clear();
    for (name, value) in vars {
        command.env(name, value);
    }
    command.output().expect("spawn nightshift")
}
