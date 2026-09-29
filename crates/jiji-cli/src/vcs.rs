//! Local host for the shared workflow executors. Each read uses a full,
//! fresh snapshot; every write goes through the same jj-lib backend as the app.

use jiji_core::{BackendError, JjBackend, RepoBackend, RepoSnapshot};
use jiji_forge::{LandVcs, ShipVcs, SubmitVcs};
use std::path::PathBuf;

pub struct LocalVcs {
    pub backend: JjBackend,
    pub path: PathBuf,
}

impl LocalVcs {
    pub fn snapshot(&self) -> Result<RepoSnapshot, BackendError> {
        self.backend.open(&self.path)
    }
}

impl SubmitVcs for LocalVcs {
    fn push_bookmarks(&self, bookmarks: &[String], remote: &str) -> Result<String, BackendError> {
        self.backend
            .push_bookmarks(&self.path, bookmarks, Some(remote))
            .map(|o| o.summary)
    }
    fn create_bookmark(&self, name: &str, change: &str) -> Result<String, BackendError> {
        self.backend
            .create_bookmark(&self.path, name, change)
            .map(|o| o.summary)
    }
}

impl LandVcs for LocalVcs {
    fn snapshot(&self) -> Result<RepoSnapshot, BackendError> {
        self.snapshot()
    }
    fn git_fetch(&self, remote: &str) -> Result<String, BackendError> {
        self.backend
            .git_fetch(&self.path, Some(&[remote.to_owned()]))
            .map(|o| o.summary)
    }
    fn rebase_onto_trunk(&self, root_change: &str) -> Result<String, BackendError> {
        let trunk = self
            .snapshot()?
            .bookmarks
            .into_iter()
            .find(|b| b.is_trunk)
            .ok_or_else(|| {
                BackendError::MutationFailed(
                    "The repository has no trunk bookmark to rebase onto".into(),
                )
            })?;
        self.backend
            .rebase_change(&self.path, root_change, &trunk.target)
            .map(|o| o.summary)
    }
    fn push_bookmarks(&self, bookmarks: &[String], remote: &str) -> Result<String, BackendError> {
        SubmitVcs::push_bookmarks(self, bookmarks, remote)
    }
    fn delete_bookmark(&self, name: &str) -> Result<String, BackendError> {
        self.backend
            .delete_bookmark(&self.path, name)
            .map(|o| o.summary)
    }
    fn abandon_changes(&self, ids: &[String]) -> Result<String, BackendError> {
        self.backend
            .abandon_changes(&self.path, ids)
            .map(|o| o.summary)
    }
}

impl ShipVcs for LocalVcs {
    fn snapshot(&self) -> Result<RepoSnapshot, BackendError> {
        self.snapshot()
    }
    fn rebase_onto_trunk(&self, root_change: &str) -> Result<String, BackendError> {
        LandVcs::rebase_onto_trunk(self, root_change)
    }
    fn push_bookmarks(&self, bookmarks: &[String], remote: &str) -> Result<String, BackendError> {
        SubmitVcs::push_bookmarks(self, bookmarks, remote)
    }
    fn move_bookmark(&self, name: &str, change: &str) -> Result<String, BackendError> {
        self.backend
            .move_bookmark(&self.path, name, change)
            .map(|o| o.summary)
    }
    fn new_change(&self, parent: &str) -> Result<String, BackendError> {
        self.backend
            .new_change(&self.path, parent)
            .map(|o| o.summary)
    }
}
