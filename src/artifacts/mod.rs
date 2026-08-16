//! Dated run directories and morning `status` lookup.

mod qa;
mod state;
mod util;

pub use qa::{write_status, QaReport, QaStatus};
pub use util::slugify;

use crate::error::Error;
use std::path::PathBuf;

/// Default artifact root relative to the process CWD.
pub const DEFAULT_OUT_DIR: &str = "artifacts";

/// One overnight run folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunDir {
    /// Absolute path to `YYYY-MM-DD_<slug>/`.
    pub path: PathBuf,
}

/// Root `artifacts/` directory.
#[derive(Debug, Clone)]
pub struct ArtifactStore {
    root: PathBuf,
}

impl ArtifactStore {
    /// Store under `root` (created on first run).
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// Artifact root path.
    #[must_use]
    pub fn root(&self) -> &std::path::Path {
        &self.root
    }

    /// Create `YYYY-MM-DD_<slug>/`, seed state files, and point `latest` at it.
    pub fn create_run(&self, date: &str, slug: &str) -> Result<RunDir, Error> {
        state::validate_date_format(date)?;
        std::fs::create_dir_all(&self.root)?;
        let slug = slugify(slug);
        let mut dir_name = format!("{date}_{slug}");
        let mut path = self.root.join(&dir_name);
        let mut suffix = 2;
        while path.exists() {
            if suffix > 10_000 {
                return Err(Error::Artifact(
                    "too many runs with same date and slug; clean up old runs".into(),
                ));
            }
            dir_name = format!("{date}_{slug}-{suffix}");
            path = self.root.join(&dir_name);
            suffix += 1;
        }
        std::fs::create_dir_all(&path)?;
        let run = RunDir { path };
        run.write_state("created", 0, None)?;
        std::fs::write(run.path.join("run.log"), b"")?;
        state::update_latest(&self.root, &dir_name)?;
        Ok(run)
    }
}

impl RunDir {
    /// Overwrite `pipeline_state.json`.
    pub fn write_state(
        &self,
        stage: &str,
        iteration: u8,
        last_error: Option<&str>,
    ) -> Result<(), Error> {
        state::write_pipeline_state(&self.path, stage, iteration, last_error)
    }

    /// Append a line to `run.log`.
    pub fn append_log(&self, line: &str) -> Result<(), Error> {
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.path.join("run.log"))?;
        writeln!(file, "{line}")?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn store() -> (tempfile::TempDir, ArtifactStore) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let store = ArtifactStore::new(tmp.path());
        (tmp, store)
    }

    #[test]
    fn create_run_rejects_path_escaping_date() {
        let (tmp, store) = store();
        let err = store.create_run("../oops", "x").expect_err("escaped date");
        match err {
            Error::Artifact(msg) => assert!(msg.contains("YYYY-MM-DD"), "{msg}"),
            other => panic!("expected Artifact, got {other:?}"),
        }
        assert!(!tmp.path().join("oops").exists());
        assert!(!store.root().exists() || store.root().read_dir().expect("read").next().is_none());
    }

    #[test]
    #[cfg(unix)]
    fn create_run_writes_dated_dir_state_and_latest() {
        let (_tmp, store) = store();
        let run = store
            .create_run("2026-08-14", "rate-limit")
            .expect("create");
        assert_eq!(
            run.path.file_name().and_then(|n| n.to_str()),
            Some("2026-08-14_rate-limit")
        );
        assert!(run.path.join("pipeline_state.json").is_file());
        assert!(run.path.join("run.log").is_file());
        let latest = store.root.join("latest");
        let meta = fs::symlink_metadata(&latest).expect("latest");
        assert!(meta.file_type().is_symlink(), "latest must be a symlink");
        let target = fs::read_link(&latest).expect("readlink");
        assert_eq!(target, PathBuf::from("2026-08-14_rate-limit"));
    }

    #[test]
    #[cfg(unix)]
    fn second_run_moves_latest_symlink() {
        let (_tmp, store) = store();
        store.create_run("2026-08-14", "one").expect("first");
        store.create_run("2026-08-14", "two").expect("second");
        let target = fs::read_link(store.root.join("latest")).expect("readlink");
        assert_eq!(target, PathBuf::from("2026-08-14_two"));
        assert!(store.root.join("2026-08-14_one").is_dir());
        assert!(store.root.join("2026-08-14_two").is_dir());
    }
}
