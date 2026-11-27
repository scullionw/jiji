//! Local host for the shared workflow executors. Each read uses a full,
//! fresh snapshot; every write goes through the same jj-lib backend as the app.

use jiji_core::snapshot::{LandedCheck, TipCheck};
use jiji_core::{BackendError, JjBackend, RepoBackend, RepoSnapshot};
use jiji_forge::{LandVcs, ShipVcs, SubmitVcs, TakeOverVcs};
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

impl TakeOverVcs for LocalVcs {
    fn git_fetch(&self, remote: &str) -> Result<String, BackendError> {
        self.backend
            .git_fetch(&self.path, Some(&[remote.to_owned()]))
            .map(|o| o.summary)
    }
    fn take_over(
        &self,
        bookmark: &str,
        remote: &str,
        number: u64,
        author: Option<&str>,
    ) -> Result<String, BackendError> {
        self.backend
            .take_over_branch(&self.path, bookmark, remote, number, author)
            .map(|o| o.summary)
    }
    fn give_back(&self, bookmark: &str, remote: &str) -> Result<String, BackendError> {
        self.backend
            .give_back_branch(&self.path, bookmark, remote)
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
    fn rebase_onto_trunk(
        &self,
        remote: &str,
        trunk: &str,
        root_change: &str,
        landed: &[String],
    ) -> Result<String, BackendError> {
        // Onto the plan's trunk branch as the remote has it after the
        // fetch, the commit the landing was checked against. Not onto
        // `trunk()` or the local trunk bookmark: a fetch can change what
        // the first names, and both can be ahead with commits nobody
        // pushed.
        self.backend
            .rebase_onto_trunk(&self.path, remote, trunk, root_change, landed)
            .map(|o| o.summary)
    }
    fn push_bookmarks(&self, bookmarks: &[String], remote: &str) -> Result<String, BackendError> {
        SubmitVcs::push_bookmarks(self, bookmarks, remote)
    }
    fn delete_bookmark_at(&self, name: &str, commit: &str) -> Result<String, BackendError> {
        self.backend
            .delete_bookmark_at(&self.path, name, commit)
            .map(|o| o.summary)
    }
    fn abandon_changes(&self, commits: &[String]) -> Result<String, BackendError> {
        // The sweep after a landing: refused while anything else sits on
        // the landed commits.
        self.backend
            .abandon_landed(&self.path, commits)
            .map(|o| o.summary)
    }
    fn landed(
        &self,
        remote: &str,
        trunk: &str,
        landing: &str,
        head: &str,
        bottom: &str,
        top: &str,
    ) -> Result<LandedCheck, BackendError> {
        self.backend
            .landed_check(&self.path, remote, trunk, landing, head, bottom, top)
    }
    fn lands_as_tip(
        &self,
        remote: &str,
        trunk: &str,
        bottom: &str,
        first_top: &str,
        tip: &str,
    ) -> Result<TipCheck, BackendError> {
        self.backend
            .lands_as_tip(&self.path, remote, trunk, bottom, first_top, tip)
    }
    fn resting_on(&self, commits: &[String]) -> Result<Vec<String>, BackendError> {
        self.backend.branches_resting_on(&self.path, commits)
    }
}

impl ShipVcs for LocalVcs {
    fn snapshot(&self) -> Result<RepoSnapshot, BackendError> {
        self.snapshot()
    }
    fn rebase_onto_trunk(&self, root_change: &str) -> Result<String, BackendError> {
        // Ship moves the local trunk bookmark onto the stack and pushes
        // it, so the stack goes onto that bookmark, unpushed commits and
        // all: they ship with it.
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
