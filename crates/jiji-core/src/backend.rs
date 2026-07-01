//! The adapter boundary between Jiji and repository state.
//!
//! Everything UI-facing goes through `RepoBackend` so the jj-lib integration
//! can land later without touching the Tauri command surface or the frontend.

use std::path::Path;

use crate::snapshot::{ChangeDetail, ChangeDiff, MutationOutcome, RepoSnapshot};

#[derive(Debug, thiserror::Error)]
pub enum BackendError {
    #[error("Path does not exist: {0}")]
    PathMissing(String),
    #[error("Not a directory: {0}")]
    NotADirectory(String),
    #[error("Not a JJ repository (no .jj directory found): {0}")]
    NotAJjRepo(String),
    #[error("Could not open repository: {0}")]
    OpenFailed(String),
    #[error("Could not read repository state: {0}")]
    SnapshotFailed(String),
    #[error("Change {0} is not in the repository anymore")]
    ChangeMissing(String),
    #[error("Your jj configuration could not be loaded: {0}")]
    ConfigInvalid(String),
    #[error("Change {0} is immutable and cannot be modified")]
    ImmutableChange(String),
    #[error("There is no local bookmark named \u{201c}{0}\u{201d}")]
    BookmarkMissing(String),
    #[error("Operation {0} is not in the repository's operation log")]
    OperationMissing(String),
    #[error("The working copy is stale: {0}")]
    StaleWorkspace(String),
    #[error("Could not apply the change: {0}")]
    MutationFailed(String),
}

impl BackendError {
    /// Stable machine-readable code for the UI.
    pub fn code(&self) -> &'static str {
        match self {
            BackendError::PathMissing(_) => "path_missing",
            BackendError::NotADirectory(_) => "not_a_directory",
            BackendError::NotAJjRepo(_) => "not_a_jj_repo",
            BackendError::OpenFailed(_) => "open_failed",
            BackendError::SnapshotFailed(_) => "snapshot_failed",
            BackendError::ChangeMissing(_) => "change_missing",
            BackendError::ConfigInvalid(_) => "config_invalid",
            BackendError::ImmutableChange(_) => "immutable_change",
            BackendError::BookmarkMissing(_) => "bookmark_missing",
            BackendError::OperationMissing(_) => "operation_missing",
            BackendError::StaleWorkspace(_) => "stale_workspace",
            BackendError::MutationFailed(_) => "mutation_failed",
        }
    }
}

/// Shared path validation so every backend reports the same errors for
/// obviously-bad paths before touching repo internals.
pub(crate) fn validate_repo_path(path: &Path) -> Result<(), BackendError> {
    let display = path.display().to_string();
    if !path.exists() {
        return Err(BackendError::PathMissing(display));
    }
    if !path.is_dir() {
        return Err(BackendError::NotADirectory(display));
    }
    if !path.join(".jj").is_dir() {
        return Err(BackendError::NotAJjRepo(display));
    }
    Ok(())
}

pub trait RepoBackend: Send + Sync {
    /// Open the repository at `path` and produce an immutable UI snapshot.
    /// Re-invoking on an already-open path acts as a refresh.
    fn open(&self, path: &Path) -> Result<RepoSnapshot, BackendError>;

    /// Like `open`, but first brings the repo up to date the way running
    /// any jj CLI command would: snapshot on-disk working-copy edits into
    /// `@`, and in a colocated repo import an externally-moved git HEAD and
    /// git refs. Each step records its own operation; an already-current
    /// repo records nothing. This is what the app calls on open, manual
    /// refresh, and every watcher tick, so `@` and its diff track the disk.
    fn refresh(&self, path: &Path) -> Result<RepoSnapshot, BackendError> {
        self.open(path)
    }

    /// Watch the repository for out-of-band changes — working-copy file
    /// edits (gitignored paths excluded), operations from another client
    /// (the CLI in a terminal), and externally-moved git refs. `on_change`
    /// fires debounced on a background thread; the caller is expected to
    /// `refresh` and republish. Dropping the returned watcher stops it.
    /// Backends with nothing real to watch (the mock) return `None`.
    fn watch(
        &self,
        path: &Path,
        on_change: Box<dyn Fn() + Send + 'static>,
    ) -> Result<Option<crate::watch::RepoWatcher>, BackendError> {
        let _ = (path, on_change);
        Ok(None)
    }

    /// Per-change detail (the changed-file list) for one change from the
    /// latest snapshot. Lazy because tree diffs are too expensive to compute
    /// for every node up front.
    fn change_detail(&self, path: &Path, change_id: &str) -> Result<ChangeDetail, BackendError>;

    /// The full content diff for one change: per-file unified hunks with
    /// intraline segments. Heavier than `change_detail` because file
    /// contents are materialized; fetched only when the diff surface needs
    /// to render a selection.
    fn change_diff(&self, path: &Path, change_id: &str) -> Result<ChangeDiff, BackendError>;

    /// The diff between two changes' trees (`jj diff --from <rev> --to
    /// <rev>`): everything that differs between those two repo states,
    /// however many changes sit between them. Powers commit-to-commit and
    /// stack-relative comparison on the diff surface. Read-only, so either
    /// end may be immutable; `from` may even be a descendant of `to`, which
    /// simply renders the reversed diff.
    fn compare_diff(
        &self,
        path: &Path,
        from_change_id: &str,
        to_change_id: &str,
    ) -> Result<ChangeDiff, BackendError>;

    /// Set one change's description. The first mutation: every write action
    /// shares its shape — refuse immutable targets, run one jj transaction,
    /// and report the resulting operation as a breadcrumb. The caller
    /// refreshes the snapshot afterwards.
    fn describe(
        &self,
        path: &Path,
        change_id: &str,
        description: &str,
    ) -> Result<MutationOutcome, BackendError>;

    /// Start a new empty change on top of `parent_change_id` and make it the
    /// working copy (`jj new <rev>`). The parent may be immutable — that is
    /// how work starts on trunk; nothing is rewritten.
    fn new_change(&self, path: &Path, parent_change_id: &str)
        -> Result<MutationOutcome, BackendError>;

    /// Move the working copy (`@`) onto an existing mutable change
    /// (`jj edit <rev>`).
    fn edit_change(&self, path: &Path, change_id: &str) -> Result<MutationOutcome, BackendError>;

    /// Abandon a mutable change (`jj abandon`): descendants rebase onto its
    /// parents, bookmarks pointing at it are deleted, and an abandoned
    /// working copy is replaced by a new empty change on its parent.
    fn abandon_change(&self, path: &Path, change_id: &str)
        -> Result<MutationOutcome, BackendError>;

    /// Squash a change into its single parent (`jj squash -r <rev>`): the
    /// parent takes the change's content and combined description, the
    /// change itself is abandoned, and bookmarks on it move to the parent.
    /// Both the change and its parent must be mutable.
    fn squash_change(&self, path: &Path, change_id: &str)
        -> Result<MutationOutcome, BackendError>;

    /// Rebase a change and all its descendants onto a new parent
    /// (`jj rebase -s <rev> -d <dest>`). The change must be mutable; the
    /// destination may be immutable — rebasing onto trunk is the canonical
    /// case. Refuses the change itself and its descendants as destinations
    /// (a cycle). Like the CLI, a change already on the destination records
    /// nothing.
    fn rebase_change(
        &self,
        path: &Path,
        change_id: &str,
        destination_id: &str,
    ) -> Result<MutationOutcome, BackendError>;

    /// Move only this change onto a new parent (`jj rebase -r <rev> -d
    /// <dest>`): its descendants stay behind, reparented onto the change's
    /// current parents. The destination may be a descendant of the change —
    /// that is how two adjacent changes swap order.
    fn move_change(
        &self,
        path: &Path,
        change_id: &str,
        destination_id: &str,
    ) -> Result<MutationOutcome, BackendError>;

    /// Create a local bookmark pointing at a change (`jj bookmark create
    /// <name> -r <rev>`). The target may be immutable — a bookmark is a
    /// ref, nothing is rewritten. Refuses names that already exist.
    fn create_bookmark(
        &self,
        path: &Path,
        name: &str,
        change_id: &str,
    ) -> Result<MutationOutcome, BackendError>;

    /// Point an existing local bookmark at a change (`jj bookmark move
    /// <name> --to <rev>`). One curated deviation from the CLI: backwards
    /// and sideways moves are allowed without a flag — Jiji's plan/confirm
    /// step is the explicit acknowledgment `--allow-backwards` provides —
    /// and the outcome summary names the direction instead.
    fn move_bookmark(
        &self,
        path: &Path,
        name: &str,
        change_id: &str,
    ) -> Result<MutationOutcome, BackendError>;

    /// Rename a local bookmark (`jj bookmark rename <old> <new>`). Like the
    /// CLI, tracked remote bookmarks keep the old name until the next push;
    /// the renamed bookmark starts out local-only.
    fn rename_bookmark(
        &self,
        path: &Path,
        old_name: &str,
        new_name: &str,
    ) -> Result<MutationOutcome, BackendError>;

    /// Delete a local bookmark (`jj bookmark delete <name>`). A tracked
    /// remote counterpart is deleted on the next push, like the CLI.
    fn delete_bookmark(&self, path: &Path, name: &str) -> Result<MutationOutcome, BackendError>;

    /// Revert one earlier operation by applying its inverse on top of the
    /// current state (`jj op revert <op>`); everything recorded after it
    /// stays. Reverting the operation a mutation just recorded is the
    /// breadcrumb's Undo, and reverting a revert is redo. `op_id` is any
    /// unambiguous operation-id hex prefix — what `OperationItem.id` and
    /// `MutationOutcome.operation_id` carry. The root operation and merges
    /// of concurrent operations are refused. Like the CLI, git-tracking
    /// state (imported git refs/HEAD) is never time-traveled — it mirrors
    /// what the backing git repo physically contains. One curated
    /// deviation: a revert that leaves the repo state exactly as it is
    /// (Undo clicked twice) records nothing, where the CLI records a
    /// bookkeeping-only operation with an empty op diff.
    fn revert_operation(&self, path: &Path, op_id: &str) -> Result<MutationOutcome, BackendError>;

    /// Restore the repo to the state an operation recorded (`jj op restore
    /// <op>`): every later operation is unwound by one new operation, which
    /// is itself revertible — nothing is lost. Restoring to the current
    /// operation records nothing. Git-tracking state stays current, as in
    /// `revert_operation`.
    fn restore_operation(&self, path: &Path, op_id: &str)
        -> Result<MutationOutcome, BackendError>;
}

/// Deterministic mock backend, kept for UI development against stable data
/// (run the app with `JIJI_MOCK_BACKEND=1`). It validates the path like the
/// real backend, then fabricates a plausible repo snapshot. Mutations are
/// remembered in memory and replayed over the fabricated snapshot, so the
/// full action → refresh → breadcrumb loop works against fabricated data.
#[derive(Default)]
pub struct MockBackend {
    mutations: std::sync::Mutex<Vec<crate::mock::MockMutation>>,
}

impl MockBackend {
    /// The snapshot with all remembered mutations replayed; mutations
    /// validate against this so e.g. a node added by `new` can be described.
    /// Mutations undone by a remembered revert/restore keep their operation
    /// row (jj's op log keeps everything) but skip their effect.
    fn overlaid_snapshot(&self, path: &Path) -> Result<RepoSnapshot, BackendError> {
        validate_repo_path(path)?;
        let mut snapshot = crate::mock::mock_snapshot(path);
        let mutations = self.mutations.lock().expect("mock state lock poisoned");
        let active = crate::mock::active_effects(&mutations);
        for (index, mutation) in mutations.iter().enumerate() {
            crate::mock::apply_mutation(&mut snapshot, mutation, index, active[index]);
        }
        Ok(snapshot)
    }

    /// Validates a mutation against the current overlay (mirroring the real
    /// backend's refusals), records it, and answers with its breadcrumb.
    fn mutate(
        &self,
        path: &Path,
        mutation: crate::mock::MockMutation,
    ) -> Result<MutationOutcome, BackendError> {
        let snapshot = self.overlaid_snapshot(path)?;
        let mut mutations = self.mutations.lock().expect("mock state lock poisoned");
        let index = mutations.len();
        let mut outcome = crate::mock::mutation_outcome(&snapshot, &mutations, &mutation, index)?;
        if outcome.operation_id.is_some() {
            mutations.push(mutation);
            drop(mutations);
            // Revert/restore outcomes follow the working copy, which only
            // the replayed snapshot knows.
            if outcome.target_change.is_none() {
                outcome.target_change = Some(self.overlaid_snapshot(path)?.working_copy);
            }
        }
        Ok(outcome)
    }
}

impl RepoBackend for MockBackend {
    fn open(&self, path: &Path) -> Result<RepoSnapshot, BackendError> {
        self.overlaid_snapshot(path)
    }

    fn change_detail(&self, path: &Path, change_id: &str) -> Result<ChangeDetail, BackendError> {
        validate_repo_path(path)?;
        if let Some(detail) = crate::mock::mock_change_detail(change_id) {
            return Ok(detail);
        }
        // Nodes added by mock mutations have no fixture diffs; render them
        // as contentless changes instead of failing.
        let snapshot = self.overlaid_snapshot(path)?;
        if snapshot.nodes.iter().any(|n| n.id == change_id) {
            return Ok(ChangeDetail {
                id: change_id.to_owned(),
                files: vec![],
                truncated: false,
            });
        }
        Err(BackendError::ChangeMissing(change_id.to_owned()))
    }

    fn change_diff(&self, path: &Path, change_id: &str) -> Result<ChangeDiff, BackendError> {
        validate_repo_path(path)?;
        if let Some(diff) = crate::mock::mock_change_diff(change_id) {
            return Ok(diff);
        }
        let snapshot = self.overlaid_snapshot(path)?;
        if snapshot.nodes.iter().any(|n| n.id == change_id) {
            return Ok(ChangeDiff {
                id: change_id.to_owned(),
                from: None,
                files: vec![],
                truncated: false,
            });
        }
        Err(BackendError::ChangeMissing(change_id.to_owned()))
    }

    fn compare_diff(
        &self,
        path: &Path,
        from_change_id: &str,
        to_change_id: &str,
    ) -> Result<ChangeDiff, BackendError> {
        let snapshot = self.overlaid_snapshot(path)?;
        crate::mock::mock_compare_diff(&snapshot, from_change_id, to_change_id)
    }

    fn describe(
        &self,
        path: &Path,
        change_id: &str,
        description: &str,
    ) -> Result<MutationOutcome, BackendError> {
        self.mutate(
            path,
            crate::mock::MockMutation::Describe {
                id: change_id.to_owned(),
                text: description.trim().to_owned(),
            },
        )
    }

    fn new_change(
        &self,
        path: &Path,
        parent_change_id: &str,
    ) -> Result<MutationOutcome, BackendError> {
        self.mutate(
            path,
            crate::mock::MockMutation::New {
                parent: parent_change_id.to_owned(),
            },
        )
    }

    fn edit_change(&self, path: &Path, change_id: &str) -> Result<MutationOutcome, BackendError> {
        self.mutate(
            path,
            crate::mock::MockMutation::Edit {
                id: change_id.to_owned(),
            },
        )
    }

    fn abandon_change(
        &self,
        path: &Path,
        change_id: &str,
    ) -> Result<MutationOutcome, BackendError> {
        self.mutate(
            path,
            crate::mock::MockMutation::Abandon {
                id: change_id.to_owned(),
            },
        )
    }

    fn squash_change(&self, path: &Path, change_id: &str) -> Result<MutationOutcome, BackendError> {
        self.mutate(
            path,
            crate::mock::MockMutation::Squash {
                id: change_id.to_owned(),
            },
        )
    }

    fn rebase_change(
        &self,
        path: &Path,
        change_id: &str,
        destination_id: &str,
    ) -> Result<MutationOutcome, BackendError> {
        self.mutate(
            path,
            crate::mock::MockMutation::Rebase {
                id: change_id.to_owned(),
                destination: destination_id.to_owned(),
                with_descendants: true,
            },
        )
    }

    fn move_change(
        &self,
        path: &Path,
        change_id: &str,
        destination_id: &str,
    ) -> Result<MutationOutcome, BackendError> {
        self.mutate(
            path,
            crate::mock::MockMutation::Rebase {
                id: change_id.to_owned(),
                destination: destination_id.to_owned(),
                with_descendants: false,
            },
        )
    }

    fn create_bookmark(
        &self,
        path: &Path,
        name: &str,
        change_id: &str,
    ) -> Result<MutationOutcome, BackendError> {
        self.mutate(
            path,
            crate::mock::MockMutation::CreateBookmark {
                name: name.trim().to_owned(),
                target: change_id.to_owned(),
            },
        )
    }

    fn move_bookmark(
        &self,
        path: &Path,
        name: &str,
        change_id: &str,
    ) -> Result<MutationOutcome, BackendError> {
        self.mutate(
            path,
            crate::mock::MockMutation::MoveBookmark {
                name: name.trim().to_owned(),
                target: change_id.to_owned(),
            },
        )
    }

    fn rename_bookmark(
        &self,
        path: &Path,
        old_name: &str,
        new_name: &str,
    ) -> Result<MutationOutcome, BackendError> {
        self.mutate(
            path,
            crate::mock::MockMutation::RenameBookmark {
                old: old_name.trim().to_owned(),
                new: new_name.trim().to_owned(),
            },
        )
    }

    fn delete_bookmark(&self, path: &Path, name: &str) -> Result<MutationOutcome, BackendError> {
        self.mutate(
            path,
            crate::mock::MockMutation::DeleteBookmark {
                name: name.trim().to_owned(),
            },
        )
    }

    fn revert_operation(&self, path: &Path, op_id: &str) -> Result<MutationOutcome, BackendError> {
        self.mutate(
            path,
            crate::mock::MockMutation::RevertOp {
                op_id: op_id.to_owned(),
            },
        )
    }

    fn restore_operation(
        &self,
        path: &Path,
        op_id: &str,
    ) -> Result<MutationOutcome, BackendError> {
        self.mutate(
            path,
            crate::mock::MockMutation::RestoreOp {
                op_id: op_id.to_owned(),
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_rejects_missing_paths() {
        let err = MockBackend::default().open(Path::new("/definitely/not/here")).unwrap_err();
        assert!(matches!(err, BackendError::PathMissing(_)));
    }

    #[test]
    fn open_rejects_non_jj_directories() {
        let dir = tempfile::tempdir().unwrap();
        let err = MockBackend::default().open(dir.path()).unwrap_err();
        assert!(matches!(err, BackendError::NotAJjRepo(_)));
    }

    #[test]
    fn open_returns_snapshot_for_jj_repo() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join(".jj")).unwrap();
        let snapshot = MockBackend::default().open(dir.path()).unwrap();
        assert_eq!(snapshot.backend, "mock");
        assert!(snapshot.nodes.iter().any(|n| n.id == snapshot.working_copy));
        let active = snapshot.workstreams.iter().find(|w| w.is_active).unwrap();
        assert!(active.node_ids.contains(&snapshot.working_copy));
    }

    #[test]
    fn change_detail_covers_every_mock_node() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join(".jj")).unwrap();
        let snapshot = MockBackend::default().open(dir.path()).unwrap();
        for node in &snapshot.nodes {
            let detail = MockBackend::default().change_detail(dir.path(), &node.id).unwrap();
            assert_eq!(detail.id, node.id);
            assert!(!detail.truncated);
        }
        let err = MockBackend::default().change_detail(dir.path(), "zzzzzzzz").unwrap_err();
        assert!(matches!(err, BackendError::ChangeMissing(_)));
    }

    #[test]
    fn mock_revert_and_restore_replay_time_travel() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join(".jj")).unwrap();
        let backend = MockBackend::default();
        let before = backend.open(dir.path()).unwrap();
        let wc_id = before.working_copy.clone();
        let fixture_op_count = before.operations.len();

        // Describe, then revert the describe: the description comes back,
        // but both operation rows stay in the timeline plus the revert's.
        let described = backend.describe(dir.path(), &wc_id, "wip: temp").unwrap();
        let described_op = described.operation_id.unwrap();
        let reverted = backend.revert_operation(dir.path(), &described_op).unwrap();
        assert!(reverted.summary.starts_with("Reverted \u{201c}"));
        assert_eq!(reverted.target_change.as_deref(), Some(wc_id.as_str()));
        let snapshot = backend.open(dir.path()).unwrap();
        let wc_node = snapshot.nodes.iter().find(|n| n.id == wc_id).unwrap();
        assert_eq!(wc_node.description, "", "describe undone");
        assert_eq!(snapshot.operations.len(), fixture_op_count + 2);
        assert!(snapshot.operations[0].description.starts_with("revert operation"));

        // Reverting it again is a no-op, like the real backend.
        let again = backend.revert_operation(dir.path(), &described_op).unwrap();
        assert!(again.operation_id.is_none());

        // Restore to a fixture operation: every recorded effect unwinds,
        // every operation row stays.
        backend.describe(dir.path(), &wc_id, "wip: again").unwrap();
        let target_op = before.operations[1].id.clone();
        let restored = backend.restore_operation(dir.path(), &target_op).unwrap();
        assert!(restored.summary.starts_with("Restored to \u{201c}"));
        let snapshot = backend.open(dir.path()).unwrap();
        let wc_node = snapshot.nodes.iter().find(|n| n.id == wc_id).unwrap();
        assert_eq!(wc_node.description, "");
        assert!(snapshot.operations[0].description.starts_with("restore to operation"));

        // Fixture operations cannot be reverted (mock limitation); unknown
        // ids are missing; restoring to the current op records nothing.
        let err = backend
            .revert_operation(dir.path(), &snapshot.operations[2].id)
            .unwrap_err();
        assert!(matches!(err, BackendError::MutationFailed(_)), "got {err:?}");
        let err = backend.revert_operation(dir.path(), "feedfacefeed").unwrap_err();
        assert!(matches!(err, BackendError::OperationMissing(_)), "got {err:?}");
        let noop = backend
            .restore_operation(dir.path(), &snapshot.operations[0].id)
            .unwrap();
        assert!(noop.operation_id.is_none());
    }

    #[test]
    fn mock_divergent_pair_renders_and_resolves_by_abandon() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join(".jj")).unwrap();
        let backend = MockBackend::default();

        let snapshot = backend.open(dir.path()).unwrap();
        let copies: Vec<_> = snapshot.nodes.iter().filter(|n| n.is_divergent).collect();
        assert_eq!(copies.len(), 2);
        assert_eq!(copies[0].change_id, copies[1].change_id);
        assert_ne!(copies[0].id, copies[1].id);
        for copy in &copies {
            assert_eq!(copy.id, copy.commit_id, "divergent nodes key by commit id");
            assert!(
                backend.change_diff(dir.path(), &copy.id).is_ok(),
                "each copy is separately inspectable"
            );
        }

        // Abandoning one copy settles the survivor's flag (its id stays
        // commit-keyed — a documented mock approximation).
        let (kept, dropped) = (copies[0].id.clone(), copies[1].id.clone());
        backend.abandon_change(dir.path(), &dropped).unwrap();
        let after = backend.open(dir.path()).unwrap();
        assert!(!after.nodes.iter().any(|n| n.id == dropped));
        let survivor = after.nodes.iter().find(|n| n.id == kept).unwrap();
        assert!(!survivor.is_divergent);
    }

    #[test]
    fn compare_diff_combines_the_mock_span() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join(".jj")).unwrap();
        let backend = MockBackend::default();
        let snapshot = backend.open(dir.path()).unwrap();

        // Trunk → working copy spans the whole working stack: every file any
        // change in the span touches appears exactly once.
        let combined = backend
            .compare_diff(dir.path(), "uvkmrtpz", &snapshot.working_copy)
            .unwrap();
        assert_eq!(combined.id, snapshot.working_copy);
        assert_eq!(combined.from.as_deref(), Some("uvkmrtpz"));
        for id in ["lwnouzpy", "mvkortsq", "nzpwlxvr"] {
            for file in backend.change_diff(dir.path(), id).unwrap().files {
                assert!(
                    combined.files.iter().any(|f| f.path == file.path),
                    "missing {} from {id}",
                    file.path
                );
            }
        }
        let mut paths: Vec<&str> = combined.files.iter().map(|f| f.path.as_str()).collect();
        paths.dedup();
        assert_eq!(paths.len(), combined.files.len(), "one entry per path");

        let same = backend
            .compare_diff(dir.path(), &snapshot.working_copy, &snapshot.working_copy)
            .unwrap();
        assert!(same.files.is_empty());

        let err = backend
            .compare_diff(dir.path(), "zzzzzzzz", &snapshot.working_copy)
            .unwrap_err();
        assert!(matches!(err, BackendError::ChangeMissing(_)));
    }
}
