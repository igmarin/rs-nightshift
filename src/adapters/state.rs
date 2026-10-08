//! Filesystem [`StateStore`] adapter: append-only JSONL action log + JSON snapshot.

use crate::domain::rolegraph::state::{ActionEvent, StatusSnapshot};
use crate::error::{ArtifactError, Error};
use crate::ports::StateStore;
use std::io::Write;
use std::path::Path;

/// Action-log file name under the run directory.
pub const ACTIONS_FILE: &str = "actions.jsonl";

/// Status-snapshot file name under the run directory.
pub const SNAPSHOT_FILE: &str = "state.json";

/// Append-only JSONL action log plus a JSON status snapshot, under a run dir.
///
/// Each [`append_action`](StateStore::append_action) call appends one line of
/// JSON to [`ACTIONS_FILE`]; [`write_snapshot`](StateStore::write_snapshot)
/// overwrites [`SNAPSHOT_FILE`]; [`read_snapshot`](StateStore::read_snapshot)
/// reads it back. The caller (the orchestrator) is responsible for creating the
/// run directory first via the artifact store.
#[derive(Debug, Default)]
pub struct FsStateStore;

impl FsStateStore {
    /// A new filesystem state store.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl StateStore for FsStateStore {
    fn append_action(&self, run: &Path, event: &ActionEvent) -> Result<(), Error> {
        let line = serde_json::to_string(event)
            .map_err(|error| Error::from(ArtifactError::artifact(error.to_string())))?;
        let path = run.join(ACTIONS_FILE);
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(|error| {
                Error::from(ArtifactError::artifact(format!(
                    "open {}: {error}",
                    path.display()
                )))
            })?;
        writeln!(file, "{line}").map_err(|error| {
            Error::from(ArtifactError::artifact(format!(
                "write {}: {error}",
                path.display()
            )))
        })
    }

    fn read_actions(&self, run: &Path) -> Result<Vec<ActionEvent>, Error> {
        let path = run.join(ACTIONS_FILE);
        let content = match std::fs::read_to_string(&path) {
            Ok(content) => content,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => {
                return Err(Error::from(ArtifactError::artifact(format!(
                    "read {}: {error}",
                    path.display()
                ))))
            }
        };
        // Skip blank lines and lines that fail to parse (e.g. a truncated final
        // line after a crash) so a complete run still yields a readable report.
        Ok(content
            .lines()
            .filter(|line| !line.trim().is_empty())
            .filter_map(|line| serde_json::from_str::<ActionEvent>(line).ok())
            .collect())
    }

    fn write_snapshot(&self, run: &Path, snapshot: &StatusSnapshot) -> Result<(), Error> {
        let json = serde_json::to_string_pretty(snapshot)
            .map_err(|error| Error::from(ArtifactError::artifact(error.to_string())))?;
        let path = run.join(SNAPSHOT_FILE);
        // Write to a temp file then rename, so an interruption never leaves a
        // truncated `state.json`.
        let tmp = run.join(format!("{SNAPSHOT_FILE}.tmp"));
        std::fs::write(&tmp, json).map_err(|error| {
            Error::from(ArtifactError::artifact(format!(
                "write {}: {error}",
                tmp.display()
            )))
        })?;
        std::fs::rename(&tmp, &path).map_err(|error| {
            Error::from(ArtifactError::artifact(format!(
                "rename {}: {error}",
                path.display()
            )))
        })
    }

    fn read_snapshot(&self, run: &Path) -> Result<StatusSnapshot, Error> {
        let path = run.join(SNAPSHOT_FILE);
        let bytes = std::fs::read(&path).map_err(|error| {
            Error::from(ArtifactError::artifact(format!(
                "read {}: {error}",
                path.display()
            )))
        })?;
        serde_json::from_slice(&bytes).map_err(|error| {
            Error::from(ArtifactError::artifact(format!(
                "parse {}: {error}",
                path.display()
            )))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::rolegraph::state::{EventKind, RunStatus};
    use crate::domain::rolegraph::verdict::{BlockReason, Verdict};

    fn event(ts: &str, event: EventKind, role: &str) -> ActionEvent {
        ActionEvent {
            ts: ts.into(),
            event,
            role: role.into(),
            provider: String::new(),
            model: String::new(),
            verdict: None,
            artifact: None,
            block_reason: BlockReason::None,
        }
    }

    #[test]
    fn append_action_writes_one_json_line_per_call() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let store = FsStateStore::new();
        store
            .append_action(tmp.path(), &event("t1", EventKind::RoleStart, "qa"))
            .expect("append");
        store
            .append_action(tmp.path(), &event("t2", EventKind::RoleEnd, "qa"))
            .expect("append");
        let content = std::fs::read_to_string(tmp.path().join(ACTIONS_FILE)).expect("read");
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 2, "{content}");
        for line in lines {
            serde_json::from_str::<ActionEvent>(line).expect("valid json line");
        }
    }

    #[test]
    fn read_actions_returns_appended_events_in_order() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let store = FsStateStore::new();
        store
            .append_action(tmp.path(), &event("t1", EventKind::RoleStart, "po"))
            .expect("append");
        store
            .append_action(tmp.path(), &event("t2", EventKind::RoleEnd, "po"))
            .expect("append");
        let events = store.read_actions(tmp.path()).expect("read");
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].role, "po");
        assert_eq!(events[1].role, "po");
    }

    #[test]
    fn read_actions_missing_file_is_empty() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let store = FsStateStore::new();
        assert!(store.read_actions(tmp.path()).expect("read").is_empty());
    }

    #[test]
    fn write_and_read_snapshot_round_trips() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let store = FsStateStore::new();
        let snap = StatusSnapshot {
            current_role: Some("dev".into()),
            steps: 3,
            last_verdict: Some(Verdict::Continue),
            status: RunStatus::Running,
            block_reason: BlockReason::None,
            loop_counters: std::collections::BTreeMap::new(),
        };
        store.write_snapshot(tmp.path(), &snap).expect("write");
        assert_eq!(store.read_snapshot(tmp.path()).expect("read"), snap);
    }

    #[test]
    fn read_snapshot_missing_is_an_error() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let store = FsStateStore::new();
        let err = store.read_snapshot(tmp.path()).expect_err("missing");
        assert!(err.to_string().contains("state.json"), "{err}");
    }
}
