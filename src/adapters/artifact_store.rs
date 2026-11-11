//! Filesystem [`ArtifactStore`] adapter: run directories under a root.

use crate::error::{ArtifactError, Error};
use crate::ports::ArtifactStore;
use std::path::{Path, PathBuf};

/// Filesystem artifact store rooted at a directory (e.g. `./artifacts`).
///
/// [`create_run`](ArtifactStore::create_run) creates a `{date}_{slug}`
/// subdirectory and returns its path; reads and writes address files inside
/// that run directory by name.
#[derive(Debug, Clone)]
pub struct FsArtifactStore {
    /// The root directory runs are created under.
    root: PathBuf,
}

impl FsArtifactStore {
    /// A store rooted at `root`.
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }
}

impl ArtifactStore for FsArtifactStore {
    /// Create `{date}_{slug}` under the root. A run never reuses another
    /// run's directory: when the name is taken, `-2`, `-3`, … is appended.
    fn create_run(&self, date: &str, slug: &str) -> Result<PathBuf, Error> {
        let failed = |dir: &Path, error: std::io::Error| {
            Error::from(ArtifactError::artifact(format!(
                "create {}: {error}",
                dir.display()
            )))
        };
        std::fs::create_dir_all(&self.root).map_err(|error| failed(&self.root, error))?;
        let mut dir = self.root.join(format!("{date}_{slug}"));
        let mut copy = 2u32;
        loop {
            // `create_dir` fails when the directory exists, so two runs
            // started in the same second cannot share one.
            match std::fs::create_dir(&dir) {
                Ok(()) => return Ok(dir),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    dir = self.root.join(format!("{date}_{slug}-{copy}"));
                    copy += 1;
                }
                Err(error) => return Err(failed(&dir, error)),
            }
        }
    }

    fn read_artifact(&self, run: &Path, name: &str) -> Result<String, Error> {
        let path = run.join(name);
        std::fs::read_to_string(&path).map_err(|error| {
            Error::from(ArtifactError::artifact(format!(
                "read {}: {error}",
                path.display()
            )))
        })
    }

    fn write_artifact(&self, run: &Path, name: &str, content: &str) -> Result<(), Error> {
        let path = run.join(name);
        std::fs::write(&path, content).map_err(|error| {
            Error::from(ArtifactError::artifact(format!(
                "write {}: {error}",
                path.display()
            )))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_read_write_round_trip() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let store = FsArtifactStore::new(tmp.path());
        let run = store
            .create_run("2026-08-20", "add-health")
            .expect("create");
        assert!(run.is_dir());
        store
            .write_artifact(&run, "01_brief.md", "the brief")
            .expect("write");
        assert_eq!(
            store.read_artifact(&run, "01_brief.md").expect("read"),
            "the brief"
        );
    }

    #[test]
    fn create_run_never_reuses_a_directory() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let store = FsArtifactStore::new(tmp.path().join("nested/out"));
        let first = store.create_run("2026-08-20", "same").expect("create");
        let second = store.create_run("2026-08-20", "same").expect("create");
        let third = store.create_run("2026-08-20", "same").expect("create");
        assert!(first.ends_with("2026-08-20_same"));
        assert!(second.ends_with("2026-08-20_same-2"));
        assert!(third.ends_with("2026-08-20_same-3"));
        assert!(first.is_dir() && second.is_dir() && third.is_dir());
    }

    #[test]
    fn read_missing_artifact_is_an_error() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let store = FsArtifactStore::new(tmp.path());
        let run = store.create_run("2026-08-20", "x").expect("create");
        let err = store.read_artifact(&run, "nope.md").expect_err("missing");
        assert!(err.to_string().contains("nope.md"), "{err}");
    }
}
