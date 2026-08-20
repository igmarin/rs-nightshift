//! Filesystem [`ArtifactStore`] adapter: run directories under a root.

use crate::error::Error;
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
    fn create_run(&self, date: &str, slug: &str) -> Result<PathBuf, Error> {
        let dir = self.root.join(format!("{date}_{slug}"));
        std::fs::create_dir_all(&dir)
            .map_err(|error| Error::Artifact(format!("create {}: {error}", dir.display())))?;
        Ok(dir)
    }

    fn read_artifact(&self, run: &Path, name: &str) -> Result<String, Error> {
        let path = run.join(name);
        std::fs::read_to_string(&path)
            .map_err(|error| Error::Artifact(format!("read {}: {error}", path.display())))
    }

    fn write_artifact(&self, run: &Path, name: &str, content: &str) -> Result<(), Error> {
        let path = run.join(name);
        std::fs::write(&path, content)
            .map_err(|error| Error::Artifact(format!("write {}: {error}", path.display())))
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
    fn read_missing_artifact_is_an_error() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let store = FsArtifactStore::new(tmp.path());
        let run = store.create_run("2026-08-20", "x").expect("create");
        let err = store.read_artifact(&run, "nope.md").expect_err("missing");
        assert!(err.to_string().contains("nope.md"), "{err}");
    }
}
