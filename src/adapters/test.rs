//! Test-adapter: detect and run the target repo's test command (never from model output).

use crate::error::{ArtifactError, Error};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Default per-run test timeout.
pub const DEFAULT_TEST_TIMEOUT: Duration = Duration::from_secs(600);

/// Most bytes kept from one output stream. The rest is read and dropped, so
/// a command that prints without end cannot use up memory.
const MAX_STREAM_BYTES: usize = 1024 * 1024;

/// Added to a stream that was longer than [`MAX_STREAM_BYTES`].
const TRUNCATED_NOTE: &str = "\n[output truncated]\n";

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
            return Err(Error::from(ArtifactError::artifact(
                "test command is empty",
            )));
        }
        let mut command = Command::new(&argv[0]);
        command
            .args(&argv[1..])
            .current_dir(repo)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        // Its own process group, so a timeout can stop everything it started.
        #[cfg(unix)]
        std::os::unix::process::CommandExt::process_group(&mut command, 0);
        let mut child = command.spawn().map_err(|e| {
            Error::from(ArtifactError::artifact(format!(
                "failed to spawn test command: {e}"
            )))
        })?;
        // Read both pipes while the command runs. A pipe holds about 64 KB;
        // a command that prints more blocks until someone reads it.
        let stop = Arc::new(AtomicBool::new(false));
        let readers = drain(child.stdout.take(), &stop)
            .and_then(|stdout| Ok((stdout, drain(child.stderr.take(), &stop)?)));
        let (stdout, stderr) = match readers {
            Ok(readers) => readers,
            Err(error) => {
                stop.store(true, Ordering::Relaxed);
                stop_command(&mut child);
                return Err(Error::from(ArtifactError::artifact(format!(
                    "failed to read the test command's output: {error}"
                ))));
            }
        };
        let start = Instant::now();
        loop {
            match child.try_wait() {
                Ok(Some(status)) => {
                    let stdout = stdout.join().unwrap_or_default();
                    let stderr = stderr.join().unwrap_or_default();
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
                    // The readers stop at their next chunk and close the
                    // pipes. That covers a process that left the command's
                    // group: it fails on its next write.
                    stop.store(true, Ordering::Relaxed);
                    stop_command(&mut child);
                    return Err(Error::from(ArtifactError::artifact(
                        "test command timed out",
                    )));
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(20)),
                Err(error) => {
                    stop.store(true, Ordering::Relaxed);
                    return Err(error.into());
                }
            }
        }
    }
}

/// Stop the command and, on Unix, every process still in its group: what it
/// started, and what those started.
fn stop_command(child: &mut std::process::Child) {
    // `kill` with a negative id signals a process group. The command is not
    // reaped yet, so its id still names the group it leads.
    #[cfg(unix)]
    let _ = Command::new("kill")
        .args(["-KILL", "--", &format!("-{}", child.id())])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    let _ = child.kill();
    let _ = child.wait();
}

/// Read a child's pipe on its own thread, until its end or until `stop` is
/// set. Keeps at most [`MAX_STREAM_BYTES`].
fn drain<R>(
    pipe: Option<R>,
    stop: &Arc<AtomicBool>,
) -> std::io::Result<std::thread::JoinHandle<Vec<u8>>>
where
    R: std::io::Read + Send + 'static,
{
    let stop = Arc::clone(stop);
    std::thread::Builder::new().spawn(move || {
        let mut kept = Vec::new();
        let Some(mut pipe) = pipe else { return kept };
        let mut truncated = false;
        let mut chunk = [0_u8; 8192];
        while !stop.load(Ordering::Relaxed) {
            match pipe.read(&mut chunk) {
                Ok(0) => break,
                Ok(read) => {
                    let room = MAX_STREAM_BYTES - kept.len();
                    kept.extend_from_slice(&chunk[..read.min(room)]);
                    truncated |= read > room;
                }
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                Err(_) => break,
            }
        }
        if truncated {
            kept.extend_from_slice(TRUNCATED_NOTE.as_bytes());
        }
        kept
    })
}

/// Resolve argv from `nightshift.toml` or a repo heuristic. Never from a model.
pub fn detect_test_command(repo: &Path) -> Result<Vec<String>, Error> {
    let config = repo.join("nightshift.toml");
    if config.is_file() {
        let text = std::fs::read_to_string(&config)?;
        if let Some(argv) = parse_test_command(&text) {
            if argv.is_empty() {
                return Err(Error::from(ArtifactError::artifact(
                    "nightshift.toml [test] command is empty",
                )));
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
    Err(Error::from(ArtifactError::artifact(
        "no test command: add nightshift.toml [test] command = \"...\"",
    )))
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
            .unwrap_or_else(|| {
                Err(Error::from(ArtifactError::artifact(
                    "no scripted test outcome",
                )))
            })
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
            Error::Artifact(ArtifactError::Artifact(msg)) => {
                assert!(msg.contains("nightshift.toml"), "{msg}")
            }
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
    fn process_runner_reads_output_larger_than_a_pipe() {
        // A pipe holds about 64 KB. A command that prints more must not block.
        let tmp = tempfile::tempdir().expect("tmp");
        let runner = ProcessTestRunner::new(Duration::from_secs(10));
        let script = "head -c 200000 /dev/zero; head -c 200000 /dev/zero >&2";
        let out = runner
            .run(tmp.path(), &["sh".into(), "-c".into(), script.into()])
            .expect("run");
        assert_eq!(out.exit_code, 0);
        // 200 000 bytes from each stream, plus the newline between them.
        assert_eq!(out.output.len(), 400_001);
    }

    #[test]
    fn process_runner_keeps_a_bounded_part_of_endless_output() {
        let tmp = tempfile::tempdir().expect("tmp");
        let runner = ProcessTestRunner::new(Duration::from_secs(10));
        let out = runner
            .run(
                tmp.path(),
                &[
                    "head".into(),
                    "-c".into(),
                    "3000000".into(),
                    "/dev/zero".into(),
                ],
            )
            .expect("run");
        assert_eq!(out.exit_code, 0);
        assert_eq!(out.output.len(), MAX_STREAM_BYTES + TRUNCATED_NOTE.len());
        assert!(out.output.ends_with(TRUNCATED_NOTE));
    }

    #[test]
    fn process_runner_stops_reading_a_leftover_process_after_a_timeout() {
        // The command leaves a background loop that prints forever. After the
        // timeout nobody reads the pipe, so the loop fails on a write and ends.
        let tmp = tempfile::tempdir().expect("tmp");
        let script = "(while :; do echo x; sleep 0.05; done) & echo $! > pid; wait";
        let runner = ProcessTestRunner::new(Duration::from_millis(300));
        runner
            .run(tmp.path(), &["sh".into(), "-c".into(), script.into()])
            .expect_err("timeout");
        let pid = std::fs::read_to_string(tmp.path().join("pid")).expect("pid");
        let alive = || {
            Command::new("kill")
                .args(["-0", pid.trim()])
                .stderr(Stdio::null())
                .status()
                .expect("kill -0")
                .success()
        };
        let deadline = Instant::now() + Duration::from_secs(5);
        while alive() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(!alive(), "the leftover process {} still runs", pid.trim());
    }

    #[cfg(unix)]
    #[test]
    fn process_runner_stops_what_the_command_started_after_a_timeout() {
        // The command starts a process that prints nothing and outlives the
        // timeout. Stopping only the command would leave it running.
        let tmp = tempfile::tempdir().expect("tmp");
        let script = "sleep 30 & echo $! > pid; wait";
        let runner = ProcessTestRunner::new(Duration::from_millis(300));
        runner
            .run(tmp.path(), &["sh".into(), "-c".into(), script.into()])
            .expect_err("timeout");
        let pid = std::fs::read_to_string(tmp.path().join("pid")).expect("pid");
        let alive = || {
            Command::new("kill")
                .args(["-0", pid.trim()])
                .stderr(Stdio::null())
                .status()
                .expect("kill -0")
                .success()
        };
        let deadline = Instant::now() + Duration::from_secs(5);
        while alive() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(50));
        }
        let still_alive = alive();
        if still_alive {
            let _ = Command::new("kill").arg(pid.trim()).status();
        }
        assert!(!still_alive, "process {} outlived the timeout", pid.trim());
    }

    #[test]
    fn process_runner_times_out() {
        let tmp = tempfile::tempdir().expect("tmp");
        let runner = ProcessTestRunner::new(Duration::from_millis(50));
        let err = runner
            .run(tmp.path(), &["sleep".into(), "5".into()])
            .expect_err("timeout");
        assert!(
            matches!(err, Error::Artifact(ArtifactError::Artifact(ref msg)) if msg.contains("timed out")),
            "{err:?}"
        );
    }
}
