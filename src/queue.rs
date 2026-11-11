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

/// Progress file under the artifact root.
pub const QUEUE_STATE_FILE: &str = "queue_state.json";

/// Human-readable summary under the artifact root.
pub const QUEUE_REPORT_FILE: &str = "queue_report.md";

/// `reason` of a job skipped because its repo has uncommitted changes.
pub const DIRTY_TREE: &str = "dirty_tree";

/// One `[[jobs]]` entry of the queue file.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Job {
    /// Business goal for the run.
    pub goal: String,
    /// Target repo (relative paths resolve against the working directory).
    pub repo: PathBuf,
    /// Directory slug (default: slugified goal).
    #[serde(default)]
    pub name: Option<String>,
    /// Role-graph config for this job (default: the queue's `--config`).
    #[serde(default)]
    pub config: Option<PathBuf>,
    /// Profile for this job (default: the queue's `--profile`).
    #[serde(default)]
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
    let queue: QueueFile = toml::from_str(&raw)
        .map_err(|error| ConfigError::new(location.clone(), error.to_string()))?;
    if queue.jobs.is_empty() {
        return Err(ConfigError::new(location, "no [[jobs]] defined").into());
    }
    if let Some(index) = queue.jobs.iter().position(|job| job.goal.trim().is_empty()) {
        return Err(
            ConfigError::new(location, format!("job {} has an empty goal", index + 1)).into(),
        );
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
    /// The job's goal (with `repo`, it identifies the job across restarts).
    pub goal: String,
    /// The job's target repo.
    pub repo: PathBuf,
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
    /// Every job finished `done`.
    #[must_use]
    pub fn all_done(&self) -> bool {
        self.jobs.iter().all(|job| job.status == JobStatus::Done)
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
/// Jobs already `done` in `out/queue_state.json` (same position, goal and
/// repo) are skipped. A job whose repo is dirty is `blocked: dirty_tree`. A
/// failed job does not stop the queue unless `stop_on_failure` is set.
pub async fn run_queue<H: JobHost>(
    host: &H,
    jobs: &[Job],
    out: &Path,
    stop_on_failure: bool,
) -> Result<QueueState, Error> {
    std::fs::create_dir_all(out).map_err(|error| io_error("create", out, &error))?;
    let previous = load_state(out)?;
    let mut state = QueueState {
        jobs: jobs
            .iter()
            .enumerate()
            .map(|(index, job)| match previous.jobs.get(index) {
                Some(done)
                    if done.status == JobStatus::Done
                        && done.goal == job.goal
                        && done.repo == job.repo =>
                {
                    done.clone()
                }
                _ => JobState {
                    goal: job.goal.clone(),
                    repo: job.repo.clone(),
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
                reason: Some(DIRTY_TREE.into()),
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
            cell(&job.goal),
            cell(&job.repo.display().to_string()),
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

/// Write the state file (temp file, then rename) and the report.
fn save(out: &Path, state: &QueueState) -> Result<(), Error> {
    let json = serde_json::to_string_pretty(state)
        .map_err(|error| Error::from(ArtifactError::artifact(error.to_string())))?;
    let path = out.join(QUEUE_STATE_FILE);
    let tmp = out.join(format!("{QUEUE_STATE_FILE}.tmp"));
    std::fs::write(&tmp, json).map_err(|error| io_error("write", &tmp, &error))?;
    std::fs::rename(&tmp, &path).map_err(|error| io_error("rename", &path, &error))?;
    let report = out.join(QUEUE_REPORT_FILE);
    std::fs::write(&report, render_report(state))
        .map_err(|error| io_error("write", &report, &error))
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
                    goal: "one".into(),
                    repo: "/a".into(),
                    status: JobStatus::Done,
                    reason: None,
                    run_dir: Some("/runs/one".into()),
                },
                JobState {
                    goal: "two".into(),
                    repo: "/b".into(),
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
        let state = run_queue(&second, &[job("other", "/a")], out.path(), false)
            .await
            .expect("queue");
        assert_eq!(*second.ran.borrow(), ["other"]);
        assert!(state.all_done());
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
        assert_eq!(queue.jobs[1].config, Some(PathBuf::from("mac.toml")));
        assert_eq!(queue.jobs[1].profile.as_deref(), Some("mac"));

        for (body, want) in [
            ("", "no [[jobs]]"),
            (
                "[[jobs]]\ngoal = \" \"\nrepo = \"/a\"\n",
                "job 1 has an empty goal",
            ),
            ("[[jobs]]\ngoal = \"one\"\n", "repo"),
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

    #[test]
    fn report_keeps_each_job_on_one_row() {
        let state = QueueState {
            jobs: vec![JobState {
                goal: "fix a | b\nand c".into(),
                repo: "/a".into(),
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

    #[test]
    fn corrupt_state_file_is_an_error_that_says_what_to_do() {
        let out = tempfile::tempdir().expect("tempdir");
        std::fs::write(out.path().join(QUEUE_STATE_FILE), "{not json").expect("write");
        let err = load_state(out.path()).expect_err("corrupt");
        assert!(err.to_string().contains("delete the file"), "{err}");
    }
}
