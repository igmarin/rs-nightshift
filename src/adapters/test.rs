//! Test-adapter: detect and run the target repo's test command (never from model output).

use crate::error::{ArtifactError, Error};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Default per-run test timeout.
pub const DEFAULT_TEST_TIMEOUT: Duration = Duration::from_secs(600);

/// Most bytes kept from one output stream. The rest is read and dropped, so
/// a command that prints without end cannot use up memory.
const MAX_STREAM_BYTES: usize = 1024 * 1024;

/// How long to wait for the end of the output after the command has exited.
/// The output of the command itself is read well within this time. Only a
/// process that the command left behind, with the pipes still open, takes
/// longer; the runner does not wait for it.
const LEFTOVER_GRACE: Duration = Duration::from_secs(2);

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
        let (stdout, stderr) = (Arc::default(), Arc::default());
        let readers = drain(child.stdout.take(), &stdout, &stop)
            .and_then(|first| Ok([first, drain(child.stderr.take(), &stderr, &stop)?]));
        let readers = match readers {
            Ok(readers) => readers,
            Err(error) => {
                stop.store(true, Ordering::Relaxed);
                let _ = stop_command(&mut child);
                return Err(Error::from(ArtifactError::artifact(format!(
                    "failed to read the test command's output: {error}"
                ))));
            }
        };
        let start = Instant::now();
        loop {
            match child.try_wait() {
                Ok(Some(status)) => {
                    // The pipes end when the last process that holds them
                    // exits. That can be a process the command left behind,
                    // so wait a short time only, then take what was read.
                    let exited = Instant::now();
                    while !readers.iter().all(std::thread::JoinHandle::is_finished)
                        && exited.elapsed() < LEFTOVER_GRACE
                    {
                        std::thread::sleep(Duration::from_millis(20));
                    }
                    stop.store(true, Ordering::Relaxed);
                    let (stdout, stderr) = (take(&stdout), take(&stderr));
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
                    let message = match stop_command(&mut child) {
                        None => "test command timed out".to_string(),
                        Some(problem) => format!(
                            "test command timed out; processes it started may still run ({problem})"
                        ),
                    };
                    return Err(Error::from(ArtifactError::artifact(message)));
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
/// started, and what those started. Returns the reason when the group could
/// not be signalled; then only the command itself was stopped.
fn stop_command(child: &mut std::process::Child) -> Option<String> {
    // The command is not reaped yet, so its id still names the group it leads.
    #[cfg(unix)]
    let problem = kill_group("kill", child.id());
    #[cfg(not(unix))]
    let problem = None;
    let _ = child.kill();
    let _ = child.wait();
    problem
}

/// Signal process group `group` with the `kill` program: a negative id names
/// a group. Returns the reason when that did not work.
#[cfg(unix)]
fn kill_group(kill: &str, group: u32) -> Option<String> {
    let status = Command::new(kill)
        .args(["-KILL", "--", &format!("-{group}")])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    match status {
        Ok(status) if status.success() => None,
        Ok(status) => Some(format!("`{kill}` ended with {status}")),
        Err(error) => Some(format!("could not run `{kill}`: {error}")),
    }
}

/// Read a child's pipe on its own thread, until its end or until `stop` is
/// set. Appends to `kept`, at most [`MAX_STREAM_BYTES`] and then
/// [`TRUNCATED_NOTE`], so `kept` can be taken while the thread still reads.
fn drain<R>(
    pipe: Option<R>,
    kept: &Arc<Mutex<Vec<u8>>>,
    stop: &Arc<AtomicBool>,
) -> std::io::Result<std::thread::JoinHandle<()>>
where
    R: std::io::Read + Send + 'static,
{
    let (kept, stop) = (Arc::clone(kept), Arc::clone(stop));
    std::thread::Builder::new().spawn(move || {
        let Some(mut pipe) = pipe else { return };
        let mut room = MAX_STREAM_BYTES;
        let mut truncated = false;
        let mut chunk = [0_u8; 8192];
        while !stop.load(Ordering::Relaxed) {
            match pipe.read(&mut chunk) {
                Ok(0) => break,
                Ok(read) => {
                    let keep = read.min(room);
                    room -= keep;
                    // Past the limit the data is read and dropped, so the
                    // command is not blocked on a full pipe.
                    if keep == 0 && truncated {
                        continue;
                    }
                    let Ok(mut kept) = kept.lock() else { break };
                    kept.extend_from_slice(&chunk[..keep]);
                    if keep < read {
                        kept.extend_from_slice(TRUNCATED_NOTE.as_bytes());
                        truncated = true;
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                Err(_) => break,
            }
        }
    })
}

/// What a reader has kept so far.
fn take(kept: &Mutex<Vec<u8>>) -> Vec<u8> {
    kept.lock()
        .map(|mut kept| std::mem::take(&mut *kept))
        .unwrap_or_default()
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

    #[cfg(unix)]
    #[test]
    fn kill_group_reports_why_it_could_not_signal() {
        // No such program: nothing is signalled, and the caller is told.
        let problem = kill_group("no-such-kill-program", 1).expect("problem");
        assert!(problem.contains("could not run"), "{problem}");
    }

    #[test]
    fn process_runner_does_not_wait_for_a_process_the_command_left_behind() {
        // The command exits at once. The `sleep` it started keeps both pipes
        // open; waiting for their end would take as long as the `sleep`.
        let tmp = tempfile::tempdir().expect("tmp");
        let script = "sleep 8 & echo $! > pid; echo done; exit 3";
        let runner = ProcessTestRunner::new(Duration::from_secs(60));
        let start = Instant::now();
        let out = runner
            .run(tmp.path(), &["sh".into(), "-c".into(), script.into()])
            .expect("run");
        let waited = start.elapsed();
        let pid = std::fs::read_to_string(tmp.path().join("pid")).expect("pid");
        let _ = Command::new("kill").arg(pid.trim()).status();
        assert!(waited < Duration::from_secs(5), "waited {waited:?}");
        assert_eq!(out.exit_code, 3);
        assert_eq!(out.output, "done\n");
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
