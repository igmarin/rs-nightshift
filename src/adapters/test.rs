//! Test-adapter: detect and run the target repo's test command (never from model output).

use crate::error::Error;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Default per-run test timeout.
pub const DEFAULT_TEST_TIMEOUT: Duration = Duration::from_secs(600);

/// Captured test invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestOutcome {
    /// Argv that was executed (from config or detector).
    pub command: Vec<String>,
    /// Process exit code (`-1` if the process was killed).
    pub exit_code: i32,
    /// Combined stdout+stderr, truncated to [`crate::qa::LOG_CAP`] by the caller.
    pub output: String,
}

/// Runs a fixed argv in the target repo.
pub trait TestRunner: Send + Sync {
    /// Execute `argv` with cwd `repo`.
    fn run(&self, repo: &Path, argv: &[String]) -> Result<TestOutcome, Error>;
}

/// Blocking process runner with a wall-clock timeout.
pub struct ProcessTestRunner {
    timeout: Duration,
}

impl ProcessTestRunner {
    /// Runner that kills the test process after `timeout`.
    #[must_use]
    pub fn new(timeout: Duration) -> Self {
        Self { timeout }
    }
}

impl Default for ProcessTestRunner {
    fn default() -> Self {
        Self::new(DEFAULT_TEST_TIMEOUT)
    }
}

impl TestRunner for ProcessTestRunner {
    fn run(&self, repo: &Path, argv: &[String]) -> Result<TestOutcome, Error> {
        if argv.is_empty() {
            return Err(Error::Artifact("test command is empty".into()));
        }
        let mut child = Command::new(&argv[0])
            .args(&argv[1..])
            .current_dir(repo)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| Error::Artifact(format!("failed to spawn test command: {e}")))?;
        let start = Instant::now();
        loop {
            match child.try_wait() {
                Ok(Some(status)) => {
                    let stdout = child
                        .stdout
                        .take()
                        .map(|mut s| {
                            let mut buf = Vec::new();
                            let _ = std::io::Read::read_to_end(&mut s, &mut buf);
                            buf
                        })
                        .unwrap_or_default();
                    let stderr = child
                        .stderr
                        .take()
                        .map(|mut s| {
                            let mut buf = Vec::new();
                            let _ = std::io::Read::read_to_end(&mut s, &mut buf);
                            buf
                        })
                        .unwrap_or_default();
                    let mut output = String::from_utf8_lossy(&stdout).into_owned();
                    if !stderr.is_empty() {
                        if !output.is_empty() {
                            output.push('\n');
                        }
                        output.push_str(&String::from_utf8_lossy(&stderr));
                    }
                    return Ok(TestOutcome {
                        command: argv.to_vec(),
                        exit_code: status.code().unwrap_or(-1),
                        output,
                    });
                }
                Ok(None) if start.elapsed() >= self.timeout => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(Error::Timeout);
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(20)),
                Err(error) => return Err(error.into()),
            }
        }
    }
}

/// Resolve argv from `nightshift.toml` or a repo heuristic. Never from a model.
pub fn detect_test_command(repo: &Path) -> Result<Vec<String>, Error> {
    let config = repo.join("nightshift.toml");
    if config.is_file() {
        let text = std::fs::read_to_string(&config)?;
        if let Some(argv) = parse_test_command(&text) {
            if argv.is_empty() {
                return Err(Error::Artifact(
                    "nightshift.toml [test] command is empty".into(),
                ));
            }
            return Ok(argv);
        }
    }
    if repo.join("Cargo.toml").is_file() {
        return Ok(vec!["cargo".into(), "test".into()]);
    }
    if repo.join("Gemfile").is_file() {
        return Ok(vec!["bundle".into(), "exec".into(), "rspec".into()]);
    }
    if repo.join("mix.exs").is_file() {
        return Ok(vec!["mix".into(), "test".into()]);
    }
    if repo.join("pyproject.toml").is_file()
        || repo.join("pytest.ini").is_file()
        || repo.join("setup.py").is_file()
    {
        return Ok(vec!["pytest".into()]);
    }
    Err(Error::Artifact(
        "no test command: add nightshift.toml [test] command = \"...\"".into(),
    ))
}

/// Parse `[test] command = "..."` from a tiny TOML subset.
#[must_use]
pub fn parse_test_command(toml: &str) -> Option<Vec<String>> {
    let mut in_test = false;
    for line in toml.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_test = line == "[test]";
            continue;
        }
        if !in_test {
            continue;
        }
        let Some(rest) = line
            .strip_prefix("command")
            .map(str::trim)
            .and_then(|s| s.strip_prefix('='))
            .map(str::trim)
        else {
            continue;
        };
        let quoted = rest
            .strip_prefix('"')
            .and_then(|s| s.strip_suffix('"'))
            .or_else(|| rest.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')))
            .unwrap_or(rest);
        let argv = split_argv(quoted);
        return Some(argv);
    }
    None
}

fn split_argv(command: &str) -> Vec<String> {
    command.split_whitespace().map(ToOwned::to_owned).collect()
}

/// Display argv the way the QA report stores it.
#[must_use]
pub fn format_command(argv: &[String]) -> String {
    argv.join(" ")
}

/// Queue of scripted test outcomes.
#[cfg(test)]
#[derive(Default)]
pub struct ScriptedRunner {
    replies: std::sync::Mutex<std::collections::VecDeque<Result<TestOutcome, Error>>>,
    /// Recorded `(repo, argv)` pairs.
    pub calls: std::sync::Mutex<Vec<(std::path::PathBuf, Vec<String>)>>,
}

#[cfg(test)]
impl ScriptedRunner {
    /// Empty script.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Queue a successful process result.
    pub fn push_outcome(&self, exit_code: i32, output: impl Into<String>, argv: &[String]) {
        self.replies
            .lock()
            .expect("runner")
            .push_back(Ok(TestOutcome {
                command: argv.to_vec(),
                exit_code,
                output: output.into(),
            }));
    }
}

#[cfg(test)]
impl TestRunner for ScriptedRunner {
    fn run(&self, repo: &Path, argv: &[String]) -> Result<TestOutcome, Error> {
        self.calls
            .lock()
            .expect("runner")
            .push((repo.to_path_buf(), argv.to_vec()));
        self.replies
            .lock()
            .expect("runner")
            .pop_front()
            .unwrap_or_else(|| Err(Error::Artifact("no scripted test outcome".into())))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn nightshift_toml_wins_over_cargo() {
        let tmp = tempfile::tempdir().expect("tmp");
        fs::write(tmp.path().join("Cargo.toml"), "[package]\n").expect("cargo");
        fs::write(
            tmp.path().join("nightshift.toml"),
            "[test]\ncommand = \"bundle exec rspec\"\n",
        )
        .expect("toml");
        assert_eq!(
            detect_test_command(tmp.path()).expect("detect"),
            ["bundle", "exec", "rspec"]
        );
    }

    #[test]
    fn cargo_toml_detects_cargo_test() {
        let tmp = tempfile::tempdir().expect("tmp");
        fs::write(tmp.path().join("Cargo.toml"), "[package]\n").expect("cargo");
        assert_eq!(
            detect_test_command(tmp.path()).expect("detect"),
            ["cargo", "test"]
        );
    }

    #[test]
    fn missing_command_is_an_error() {
        let tmp = tempfile::tempdir().expect("tmp");
        let err = detect_test_command(tmp.path()).expect_err("none");
        match err {
            Error::Artifact(msg) => assert!(msg.contains("nightshift.toml"), "{msg}"),
            other => panic!("expected Artifact, got {other:?}"),
        }
    }

    #[test]
    fn parse_test_command_ignores_other_tables() {
        let toml = "[other]\ncommand = \"nope\"\n\n[test]\ncommand = \"mix test\"\n";
        assert_eq!(
            parse_test_command(toml).as_deref(),
            Some(&["mix".into(), "test".into()][..])
        );
    }

    #[test]
    fn process_runner_captures_exit_code() {
        let tmp = tempfile::tempdir().expect("tmp");
        let runner = ProcessTestRunner::new(Duration::from_secs(5));
        let out = runner.run(tmp.path(), &["false".into()]).expect("run");
        assert_ne!(out.exit_code, 0);
        assert_eq!(out.command, ["false"]);
    }

    #[test]
    fn process_runner_times_out() {
        let tmp = tempfile::tempdir().expect("tmp");
        let runner = ProcessTestRunner::new(Duration::from_millis(50));
        let err = runner
            .run(tmp.path(), &["sleep".into(), "5".into()])
            .expect_err("timeout");
        assert!(matches!(err, Error::Timeout), "{err:?}");
    }
}
