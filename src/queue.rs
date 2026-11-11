//! `nightshift queue`: run many harness jobs back to back, unattended.
//!
//! The queue is a loop *around* the single-run harness, so the role graph and
//! its invariants stay as they are: one job at a time (INV-1), and the harness
//! never commits (INV-3). That second rule is why a job is skipped when its
//! repo has uncommitted changes: an earlier job on the same checkout leaves
//! them behind for the operator to review.
//!
//! [`run_queue`] takes a [`JobHost`], so tests inject a fake and the CLI edge
//! supplies the real harness. Progress is saved to `queue_state.json` after
//! every job, so a restart skips the jobs that already finished `done`.

use crate::error::{ArtifactError, ConfigError, Error};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Lock file under the artifact root: one queue per `--out` at a time.
pub const QUEUE_LOCK_FILE: &str = "queue.lock";

/// Progress file under the artifact root.
pub const QUEUE_STATE_FILE: &str = "queue_state.json";

/// Human-readable summary under the artifact root.
pub const QUEUE_REPORT_FILE: &str = "queue_report.md";

/// `reason` of a job skipped because its repo has uncommitted changes.
pub const DIRTY_TREE: &str = "dirty_tree";

/// One `[[jobs]]` entry of the queue file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Job {
    /// Business goal for the run.
    pub goal: String,
    /// Target repo (a relative path resolves against the queue file's directory).
    pub repo: PathBuf,
    /// Directory slug (default: slugified goal).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Role-graph config for this job (default: the queue's `--config`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config: Option<PathBuf>,
    /// Profile for this job (default: the queue's `--profile`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
}

/// The parsed queue file.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QueueFile {
    /// Jobs, in the order they run.
    #[serde(default)]
    pub jobs: Vec<Job>,
}

/// Read and check the queue file at `path`.
pub fn read_queue_file(path: &Path) -> Result<QueueFile, Error> {
    let location = path.display().to_string();
    let raw = std::fs::read_to_string(path)
        .map_err(|error| ConfigError::new(location.clone(), error.to_string()))?;
    let mut queue: QueueFile = toml::from_str(&raw)
        .map_err(|error| ConfigError::new(location.clone(), error.to_string()))?;
    if queue.jobs.is_empty() {
        return Err(ConfigError::new(location, "no [[jobs]] defined").into());
    }
    if let Some(index) = queue.jobs.iter().position(|job| job.goal.trim().is_empty()) {
        return Err(
            ConfigError::new(location, format!("job {} has an empty goal", index + 1)).into(),
        );
    }
    if let Some(index) = queue
        .jobs
        .iter()
        .position(|job| job.repo.as_os_str().is_empty())
    {
        return Err(
            ConfigError::new(location, format!("job {} has an empty repo", index + 1)).into(),
        );
    }
    // Relative paths resolve against the queue file's directory, so the same
    // file names the same checkouts from any working directory (launchd
    // starts in `/`). Symlinks are resolved, so a link re-pointed at another
    // checkout is a different job; a path that does not exist yet stays
    // absolute and its job fails on its own.
    let base = path.parent().unwrap_or_else(|| Path::new(""));
    let absolute = |relative: &Path| {
        let joined = base.join(relative);
        std::fs::canonicalize(&joined)
            .or_else(|_| std::path::absolute(&joined))
            .map_err(|error| {
                ConfigError::new(location.clone(), format!("{}: {error}", relative.display()))
            })
    };
    for job in &mut queue.jobs {
        job.repo = absolute(&job.repo)?;
        if let Some(config) = &job.config {
            job.config = Some(absolute(config)?);
        }
    }
    Ok(queue)
}

/// Where a job stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobStatus {
    /// Not started yet.
    Pending,
    /// Started and not finished (after a crash, this job is run again).
    Running,
    /// The harness finished `done`.
    Done,
    /// Skipped, or the harness halted for human review.
    Blocked,
    /// The harness failed.
    Failed,
}

impl JobStatus {
    fn label(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Done => "done",
            Self::Blocked => "blocked",
            Self::Failed => "failed",
        }
    }
}

/// One job's record in `queue_state.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobState {
    /// The job as the queue file defined it. A restart skips a `done` job
    /// only while its whole definition is unchanged.
    #[serde(flatten)]
    pub job: Job,
    /// Where the job stands.
    pub status: JobStatus,
    /// Why the job is `blocked` or `failed`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// The job's run directory, once it has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_dir: Option<PathBuf>,
}

/// The whole queue's progress.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueueState {
    /// One record per job, in queue order.
    pub jobs: Vec<JobState>,
}

impl QueueState {
    /// There is at least one job and every job finished `done`.
    #[must_use]
    pub fn all_done(&self) -> bool {
        !self.jobs.is_empty() && self.jobs.iter().all(|job| job.status == JobStatus::Done)
    }
}

/// How one job ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobOutcome {
    /// `Done`, `Blocked` or `Failed`.
    pub status: JobStatus,
    /// Why, when the job did not finish `done`.
    pub reason: Option<String>,
    /// The run directory, when one was created.
    pub run_dir: Option<PathBuf>,
}

/// What the queue needs from the outside world.
#[async_trait::async_trait(?Send)]
pub trait JobHost {
    /// Whether `repo` has uncommitted changes.
    fn is_dirty(&self, repo: &Path) -> Result<bool, Error>;

    /// Run one job to its end. An error inside the run is a `Failed` outcome,
    /// so one bad job cannot stop the queue.
    async fn run(&self, job: &Job) -> JobOutcome;
}

/// Run `jobs` one at a time, saving progress under `out` after each.
///
/// Jobs already `done` in `out/queue_state.json` (same position, same
/// definition) are skipped. A job whose repo is dirty is `blocked:
/// dirty_tree`. A failed job does not stop the queue unless `stop_on_failure`
/// is set. Only one queue may use `out` at a time.
pub async fn run_queue<H: JobHost>(
    host: &H,
    jobs: &[Job],
    out: &Path,
    stop_on_failure: bool,
) -> Result<QueueState, Error> {
    std::fs::create_dir_all(out).map_err(|error| io_error("create", out, &error))?;
    // Held until this function returns; the OS drops it if the process dies.
    let _lock = lock(out).await?;
    // Resolved like the job repos, so the two can be compared.
    let out_absolute = std::fs::canonicalize(out).unwrap_or_else(|_| out.to_path_buf());
    let previous = load_state(out)?;
    let mut state = QueueState {
        jobs: jobs
            .iter()
            .enumerate()
            .map(|(index, job)| match previous.jobs.get(index) {
                Some(done) if done.status == JobStatus::Done && done.job == *job => done.clone(),
                _ => JobState {
                    job: job.clone(),
                    status: JobStatus::Pending,
                    reason: None,
                    run_dir: None,
                },
            })
            .collect(),
    };
    save(out, &state)?;

    for (index, job) in jobs.iter().enumerate() {
        let position = format!("[{}/{}]", index + 1, jobs.len());
        if status_of(&state, index) == Some(JobStatus::Done) {
            eprintln!("{position} already done, skipping: {}", job.goal);
            continue;
        }
        let outcome = match host.is_dirty(&job.repo) {
            Ok(true) => JobOutcome {
                status: JobStatus::Blocked,
                // The queue's own files dirty a repo that contains `--out`.
                reason: Some(if out_absolute.starts_with(&job.repo) {
                    format!(
                        "{DIRTY_TREE} (--out is inside this repo: add it to the repo's \
                         .gitignore or use another --out)"
                    )
                } else {
                    DIRTY_TREE.into()
                }),
                run_dir: None,
            },
            Err(error) => JobOutcome {
                status: JobStatus::Failed,
                reason: Some(error.to_string()),
                run_dir: None,
            },
            Ok(false) => {
                eprintln!("{position} running: {}", job.goal);
                if let Some(record) = state.jobs.get_mut(index) {
                    record.status = JobStatus::Running;
                }
                save(out, &state)?;
                host.run(job).await
            }
        };
        eprintln!(
            "{position} {}{}",
            outcome.status.label(),
            outcome
                .reason
                .as_deref()
                .map(|reason| format!(": {reason}"))
                .unwrap_or_default()
        );
        let failed = outcome.status == JobStatus::Failed;
        if let Some(record) = state.jobs.get_mut(index) {
            record.status = outcome.status;
            record.reason = outcome.reason;
            record.run_dir = outcome.run_dir;
        }
        save(out, &state)?;
        if failed && stop_on_failure {
            break;
        }
    }
    Ok(state)
}

fn status_of(state: &QueueState, index: usize) -> Option<JobStatus> {
    state.jobs.get(index).map(|job| job.status)
}

/// Render `queue_report.md`: one table row per job.
#[must_use]
pub fn render_report(state: &QueueState) -> String {
    let done = state
        .jobs
        .iter()
        .filter(|job| job.status == JobStatus::Done)
        .count();
    let mut lines = vec![
        "# Queue report".to_string(),
        String::new(),
        format!("{done} of {} jobs done.", state.jobs.len()),
        String::new(),
        "| # | Status | Goal | Repo | Artifacts |".to_string(),
        "| :-- | :-- | :-- | :-- | :-- |".to_string(),
    ];
    for (index, job) in state.jobs.iter().enumerate() {
        let status = match &job.reason {
            Some(reason) => format!("{}: {reason}", job.status.label()),
            None => job.status.label().to_string(),
        };
        let artifacts = job
            .run_dir
            .as_ref()
            .map(|dir| dir.display().to_string())
            .unwrap_or_default();
        lines.push(format!(
            "| {} | {} | {} | {} | {} |",
            index + 1,
            cell(&status),
            cell(&job.job.goal),
            cell(&job.job.repo.display().to_string()),
            cell(&artifacts),
        ));
    }
    lines.push(String::new());
    lines.join("\n")
}

/// One table cell: a single line with pipes escaped.
fn cell(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .replace('|', "\\|")
}

/// Take the exclusive queue lock for `out`, or say why not.
///
/// A lock that was just released can still look held for a moment (a queue
/// that is exiting, or a child process between fork and exec that still has
/// the descriptor), so a busy lock is retried for a quarter of a second.
async fn lock(out: &Path) -> Result<std::fs::File, Error> {
    const ATTEMPTS: u32 = 5;
    const PAUSE: std::time::Duration = std::time::Duration::from_millis(50);
    let path = out.join(QUEUE_LOCK_FILE);
    // Never truncate: the lock file holds nothing, and an existing path is
    // left exactly as it is.
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
        .map_err(|error| io_error("open", &path, &error))?;
    for attempt in 1..=ATTEMPTS {
        match file.try_lock() {
            Ok(()) => return Ok(file),
            Err(std::fs::TryLockError::WouldBlock) if attempt < ATTEMPTS => {
                tokio::time::sleep(PAUSE).await;
            }
            Err(std::fs::TryLockError::WouldBlock) => break,
            Err(std::fs::TryLockError::Error(error)) => {
                return Err(io_error("lock", &path, &error))
            }
        }
    }
    Err(Error::from(ArtifactError::artifact(format!(
        "another nightshift queue is already using {}; wait for it or use another --out",
        out.display()
    ))))
}

fn load_state(out: &Path) -> Result<QueueState, Error> {
    let path = out.join(QUEUE_STATE_FILE);
    if !path.exists() {
        return Ok(QueueState::default());
    }
    let raw = std::fs::read_to_string(&path).map_err(|error| io_error("read", &path, &error))?;
    serde_json::from_str(&raw).map_err(|error| {
        Error::from(ArtifactError::artifact(format!(
            "{}: {error} (delete the file to start the queue over)",
            path.display()
        )))
    })
}

/// Write the state file and the report.
fn save(out: &Path, state: &QueueState) -> Result<(), Error> {
    let json = serde_json::to_string_pretty(state)
        .map_err(|error| Error::from(ArtifactError::artifact(error.to_string())))?;
    replace_file(&out.join(QUEUE_STATE_FILE), &json)?;
    replace_file(&out.join(QUEUE_REPORT_FILE), &render_report(state))
}

/// Write to a new, uniquely named temp file, then rename, so a reader never
/// sees a partial file and no existing path is written through.
fn replace_file(path: &Path, content: &str) -> Result<(), Error> {
    use std::io::Write;
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let mut tmp =
        tempfile::NamedTempFile::new_in(dir).map_err(|error| io_error("create in", dir, &error))?;
    tmp.write_all(content.as_bytes())
        .map_err(|error| io_error("write", tmp.path(), &error))?;
    tmp.persist(path)
        .map_err(|error| io_error("rename", path, &error.error))?;
    Ok(())
}

fn io_error(action: &str, path: &Path, error: &std::io::Error) -> Error {
    Error::from(ArtifactError::artifact(format!(
        "{action} {}: {error}",
        path.display()
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::BTreeSet;

    /// Scripted host: `fail` and `dirty` name goals; a run dirties its repo.
    #[derive(Default)]
    struct FakeHost {
        fail: BTreeSet<&'static str>,
        dirty: RefCell<BTreeSet<PathBuf>>,
        ran: RefCell<Vec<String>>,
        /// States seen on disk at the start of each run.
        seen_on_disk: RefCell<Vec<QueueState>>,
        out: PathBuf,
    }

    #[async_trait::async_trait(?Send)]
    impl JobHost for FakeHost {
        fn is_dirty(&self, repo: &Path) -> Result<bool, Error> {
            if repo == Path::new("/not-a-repo") {
                return Err(Error::from(ArtifactError::artifact("not a git repository")));
            }
            Ok(self.dirty.borrow().contains(repo))
        }

        async fn run(&self, job: &Job) -> JobOutcome {
            self.ran.borrow_mut().push(job.goal.clone());
            self.dirty.borrow_mut().insert(job.repo.clone());
            self.seen_on_disk
                .borrow_mut()
                .push(load_state(&self.out).expect("state on disk"));
            if self.fail.contains(job.goal.as_str()) {
                return JobOutcome {
                    status: JobStatus::Failed,
                    reason: Some("boom".into()),
                    run_dir: Some(PathBuf::from("/runs/failed")),
                };
            }
            JobOutcome {
                status: JobStatus::Done,
                reason: None,
                run_dir: Some(PathBuf::from(format!("/runs/{}", job.goal))),
            }
        }
    }

    fn job(goal: &str, repo: &str) -> Job {
        Job {
            goal: goal.into(),
            repo: repo.into(),
            name: None,
            config: None,
            profile: None,
        }
    }

    fn host(out: &Path, fail: &[&'static str]) -> FakeHost {
        FakeHost {
            fail: fail.iter().copied().collect(),
            out: out.to_path_buf(),
            ..FakeHost::default()
        }
    }

    fn statuses(state: &QueueState) -> Vec<JobStatus> {
        state.jobs.iter().map(|job| job.status).collect()
    }

    #[tokio::test]
    async fn a_failed_job_does_not_stop_the_queue() {
        let out = tempfile::tempdir().expect("tempdir");
        let jobs = [job("one", "/a"), job("two", "/b"), job("three", "/c")];
        let host = host(out.path(), &["two"]);
        let state = run_queue(&host, &jobs, out.path(), false)
            .await
            .expect("queue");
        assert_eq!(
            statuses(&state),
            [JobStatus::Done, JobStatus::Failed, JobStatus::Done]
        );
        assert!(!state.all_done());
        assert_eq!(state.jobs[1].reason.as_deref(), Some("boom"));
        assert_eq!(*host.ran.borrow(), ["one", "two", "three"]);
        // The state on disk matches what was returned, and the report exists.
        assert_eq!(load_state(out.path()).expect("state"), state);
        let report = std::fs::read_to_string(out.path().join(QUEUE_REPORT_FILE)).expect("report");
        assert!(report.contains("2 of 3 jobs done."), "{report}");
        assert!(
            report.contains("| 2 | failed: boom | two | /b | /runs/failed |"),
            "{report}"
        );
    }

    #[tokio::test]
    async fn stop_on_failure_leaves_later_jobs_pending() {
        let out = tempfile::tempdir().expect("tempdir");
        let jobs = [job("one", "/a"), job("two", "/b"), job("three", "/c")];
        let host = host(out.path(), &["two"]);
        let state = run_queue(&host, &jobs, out.path(), true)
            .await
            .expect("queue");
        assert_eq!(
            statuses(&state),
            [JobStatus::Done, JobStatus::Failed, JobStatus::Pending]
        );
        assert_eq!(*host.ran.borrow(), ["one", "two"]);
    }

    #[tokio::test]
    async fn restart_skips_done_jobs_and_reruns_the_rest() {
        let out = tempfile::tempdir().expect("tempdir");
        let jobs = [job("one", "/a"), job("two", "/b"), job("three", "/c")];
        // A killed process leaves job 2 `running` on disk.
        let killed = QueueState {
            jobs: vec![
                JobState {
                    job: job("one", "/a"),
                    status: JobStatus::Done,
                    reason: None,
                    run_dir: Some("/runs/one".into()),
                },
                JobState {
                    job: job("two", "/b"),
                    status: JobStatus::Running,
                    reason: None,
                    run_dir: None,
                },
            ],
        };
        save(out.path(), &killed).expect("save");
        let host = host(out.path(), &[]);
        let state = run_queue(&host, &jobs, out.path(), false)
            .await
            .expect("queue");
        assert!(state.all_done());
        assert_eq!(*host.ran.borrow(), ["two", "three"]);
        assert_eq!(state.jobs[0].run_dir, Some(PathBuf::from("/runs/one")));
        // While job 2 ran, the file on disk said so.
        assert_eq!(
            statuses(&host.seen_on_disk.borrow()[0]),
            [JobStatus::Done, JobStatus::Running, JobStatus::Pending]
        );
    }

    #[tokio::test]
    async fn a_changed_job_is_not_treated_as_done() {
        let out = tempfile::tempdir().expect("tempdir");
        let first = host(out.path(), &[]);
        run_queue(&first, &[job("one", "/a")], out.path(), false)
            .await
            .expect("queue");
        // Same position, different goal: it must run.
        let second = host(out.path(), &[]);
        let state = run_queue(&second, &[job("other", "/b")], out.path(), false)
            .await
            .expect("queue");
        assert_eq!(*second.ran.borrow(), ["other"]);
        assert!(state.all_done());
        // Same goal and repo, different profile: it must run too.
        let mut changed = job("other", "/c");
        let third = host(out.path(), &[]);
        run_queue(&third, std::slice::from_ref(&changed), out.path(), false)
            .await
            .expect("queue");
        changed.profile = Some("linux".into());
        third.dirty.borrow_mut().clear();
        run_queue(&third, std::slice::from_ref(&changed), out.path(), false)
            .await
            .expect("queue");
        assert_eq!(*third.ran.borrow(), ["other", "other"]);
        // Unchanged: skipped.
        run_queue(&third, std::slice::from_ref(&changed), out.path(), false)
            .await
            .expect("queue");
        assert_eq!(third.ran.borrow().len(), 2);
    }

    #[tokio::test]
    async fn a_second_queue_on_the_same_out_is_refused() {
        let out = tempfile::tempdir().expect("tempdir");
        let held = lock(out.path()).await.expect("first lock");
        let host = host(out.path(), &[]);
        let err = run_queue(&host, &[job("one", "/a")], out.path(), false)
            .await
            .expect_err("locked");
        assert!(err.to_string().contains("already using"), "{err}");
        assert!(host.ran.borrow().is_empty());
        drop(held);
        run_queue(&host, &[job("one", "/a")], out.path(), false)
            .await
            .expect("lock released");
    }

    #[tokio::test]
    async fn second_job_on_a_dirty_repo_is_blocked() {
        let out = tempfile::tempdir().expect("tempdir");
        let jobs = [job("one", "/same"), job("two", "/same"), job("three", "/c")];
        let host = host(out.path(), &[]);
        let state = run_queue(&host, &jobs, out.path(), false)
            .await
            .expect("queue");
        assert_eq!(
            statuses(&state),
            [JobStatus::Done, JobStatus::Blocked, JobStatus::Done]
        );
        assert_eq!(state.jobs[1].reason.as_deref(), Some(DIRTY_TREE));
        assert_eq!(*host.ran.borrow(), ["one", "three"]);
    }

    #[tokio::test]
    async fn dirty_repo_that_contains_out_says_so() {
        let repo = tempfile::tempdir().expect("tempdir");
        // Job repos arrive with symlinks resolved (see `read_queue_file`).
        let repo_path = repo.path().canonicalize().expect("canonical");
        let out = repo.path().join("artifacts");
        let host = host(&out, &[]);
        host.dirty.borrow_mut().insert(repo_path.clone());
        let jobs = [job("one", &repo_path.display().to_string())];
        let state = run_queue(&host, &jobs, &out, false).await.expect("queue");
        let reason = state.jobs[0].reason.as_deref().expect("reason");
        assert!(reason.starts_with(DIRTY_TREE), "{reason}");
        assert!(reason.contains("--out is inside this repo"), "{reason}");
        assert!(host.ran.borrow().is_empty());
    }

    #[tokio::test]
    async fn a_repo_that_cannot_be_checked_fails_the_job_only() {
        let out = tempfile::tempdir().expect("tempdir");
        let jobs = [job("one", "/not-a-repo"), job("two", "/b")];
        let host = host(out.path(), &[]);
        let state = run_queue(&host, &jobs, out.path(), false)
            .await
            .expect("queue");
        assert_eq!(statuses(&state), [JobStatus::Failed, JobStatus::Done]);
        assert!(state.jobs[0]
            .reason
            .as_deref()
            .is_some_and(|reason| reason.contains("not a git repository")));
    }

    #[test]
    fn queue_file_parses_jobs_and_rejects_bad_input() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("goals.toml");
        std::fs::write(
            &path,
            "[[jobs]]\ngoal = \"one\"\nrepo = \"/a\"\n\n[[jobs]]\ngoal = \"two\"\nrepo = \"b\"\n\
             name = \"second\"\nconfig = \"mac.toml\"\nprofile = \"mac\"\n",
        )
        .expect("write");
        let queue = read_queue_file(&path).expect("parses");
        assert_eq!(queue.jobs.len(), 2);
        assert_eq!(queue.jobs[0], job("one", "/a"));
        assert_eq!(queue.jobs[1].name.as_deref(), Some("second"));
        // Relative paths are stored absolute, against the queue file's directory.
        assert_eq!(queue.jobs[1].repo, dir.path().join("b"));
        assert_eq!(queue.jobs[1].config, Some(dir.path().join("mac.toml")));
        assert_eq!(queue.jobs[1].profile.as_deref(), Some("mac"));

        for (body, want) in [
            ("", "no [[jobs]]"),
            (
                "[[jobs]]\ngoal = \" \"\nrepo = \"/a\"\n",
                "job 1 has an empty goal",
            ),
            ("[[jobs]]\ngoal = \"one\"\n", "repo"),
            (
                "[[jobs]]\ngoal = \"one\"\nrepo = \"\"\n",
                "job 1 has an empty repo",
            ),
            (
                "[[jobs]]\ngoal = \"one\"\nrepo = \"/a\"\ntypo = 1\n",
                "typo",
            ),
        ] {
            std::fs::write(&path, body).expect("write");
            let err = read_queue_file(&path).expect_err(body);
            assert!(err.to_string().contains(want), "{body:?} -> {err}");
        }
        let missing = read_queue_file(&dir.path().join("nope.toml")).expect_err("missing file");
        assert!(missing.to_string().contains("nope.toml"), "{missing}");
    }

    #[cfg(unix)]
    #[test]
    fn a_repointed_symlink_is_a_different_job() {
        let dir = tempfile::tempdir().expect("tempdir");
        for name in ["a", "b"] {
            std::fs::create_dir(dir.path().join(name)).expect("mkdir");
        }
        let link = dir.path().join("current");
        let path = dir.path().join("goals.toml");
        std::fs::write(&path, "[[jobs]]\ngoal = \"one\"\nrepo = \"current\"\n").expect("write");

        std::os::unix::fs::symlink(dir.path().join("a"), &link).expect("symlink");
        let first = read_queue_file(&path).expect("parses").jobs.remove(0);
        std::fs::remove_file(&link).expect("unlink");
        std::os::unix::fs::symlink(dir.path().join("b"), &link).expect("symlink");
        let second = read_queue_file(&path).expect("parses").jobs.remove(0);

        assert!(first.repo.ends_with("a"), "{:?}", first.repo);
        assert!(second.repo.ends_with("b"), "{:?}", second.repo);
        // `run_queue` skips a done job only when the whole job is equal.
        assert_ne!(first, second);
    }

    #[test]
    fn report_keeps_each_job_on_one_row() {
        let state = QueueState {
            jobs: vec![JobState {
                job: job("fix a | b\nand c", "/a"),
                status: JobStatus::Blocked,
                reason: Some(DIRTY_TREE.into()),
                run_dir: None,
            }],
        };
        let report = render_report(&state);
        assert!(report.contains("0 of 1 jobs done."), "{report}");
        assert!(
            report.contains("| 1 | blocked: dirty_tree | fix a \\| b and c | /a |  |"),
            "{report}"
        );
    }

    #[tokio::test]
    async fn an_empty_queue_is_not_all_done() {
        let out = tempfile::tempdir().expect("tempdir");
        let state = run_queue(&host(out.path(), &[]), &[], out.path(), false)
            .await
            .expect("queue");
        assert!(state.jobs.is_empty());
        assert!(!state.all_done());
    }

    #[test]
    fn corrupt_state_file_is_an_error_that_says_what_to_do() {
        let out = tempfile::tempdir().expect("tempdir");
        std::fs::write(out.path().join(QUEUE_STATE_FILE), "{not json").expect("write");
        let err = load_state(out.path()).expect_err("corrupt");
        assert!(err.to_string().contains("delete the file"), "{err}");
    }
}
