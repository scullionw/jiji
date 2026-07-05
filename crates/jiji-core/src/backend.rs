//! The adapter boundary between Jiji and repository state.
//!
//! Everything UI-facing goes through `RepoBackend` so the jj-lib integration
//! can land later without touching the Tauri command surface or the frontend.

use std::path::Path;

use crate::snapshot::{ChangeDetail, ChangeDiff, MutationOutcome, RepoSnapshot, SplitSelection};

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

    /// Abandon several changes as one operation (`jj abandon <rev> <rev>…`),
    /// pinned against jj 0.41: descendants outside the set rebase onto the
    /// nearest surviving ancestors, bookmarks pointing at any abandoned
    /// change are deleted, and an abandoned working copy respawns as a new
    /// empty change on the nearest surviving ancestor. The op description
    /// matches the CLI's — `abandon commit <hex>` for one change, `abandon
    /// commit <hex> and N more` past that, quoting the topologically newest
    /// target regardless of call order (probed empirically). Duplicate ids
    /// collapse like the CLI's revset union. The post-land cleanup sweeps a
    /// squash-landed segment through this so the whole sweep is one
    /// undoable operation.
    ///
    /// One curated deviation, documented on the implementation: an empty
    /// list is refused where the CLI prints "No revisions to abandon." and
    /// exits cleanly — coming from a confirmed plan, it is a stale plan.
    fn abandon_changes(
        &self,
        path: &Path,
        change_ids: &[String],
    ) -> Result<MutationOutcome, BackendError>;

    /// Squash a change into its single parent (`jj squash -r <rev>`): the
    /// parent takes the change's content and combined description, the
    /// change itself is abandoned, and bookmarks on it move to the parent.
    /// Both the change and its parent must be mutable.
    fn squash_change(&self, path: &Path, change_id: &str)
        -> Result<MutationOutcome, BackendError>;

    /// Split a change in two (`jj split <paths> -r <rev>`, or `jj split -i`
    /// when a selection names hunks): the selected content stays in the
    /// change itself — same change id, new `description` — and everything
    /// else moves into a new change inserted directly on top, which keeps
    /// the original description and author. Like the CLI, bookmarks,
    /// descendants, and (when splitting `@`) the working copy all follow
    /// the remainder, so "split" reads as carving the selection off the
    /// bottom while work continues on top.
    ///
    /// Whole-file entries (`hunks: None`) are the fast path. An entry with
    /// hunks takes only those hunks of the file's diff — the carved half
    /// gets the parent's file content with the selected hunks applied, the
    /// rest of the file stays with the remainder. Hunk-level entries must
    /// be regular non-conflicted text files; the hunk coordinates are
    /// re-derived from the trees at mutation time and refused when they no
    /// longer match what the panel rendered (the change moved underneath —
    /// refresh and try again), the same never-clobber posture as
    /// `resolve_conflict`. A partial file keeps the parent side's
    /// executable bit in the carved half; a mode-only flip rides the
    /// remainder.
    ///
    /// One curated deviation, both ways the same: a selection that changes
    /// nothing or covers every change is refused — the CLI warns and
    /// proceeds, leaving an empty half behind, but from a plan panel that
    /// is always a mistake.
    fn split_change(
        &self,
        path: &Path,
        change_id: &str,
        selection: &[SplitSelection],
        description: &str,
    ) -> Result<MutationOutcome, BackendError>;

    /// Move part of a change into another existing change (`jj squash
    /// --from <rev> --into <dest> <paths>`, or `jj squash -i` semantics when
    /// a selection names hunks): the selected content leaves the change and
    /// lands in the destination, wherever it sits — an ancestor amends work
    /// down the stack, a descendant pulls it forward, a sibling moves it
    /// across stacks. Both changes must be mutable and distinct; descendants
    /// of both rebase, and content that no longer applies cleanly records a
    /// first-class conflict rather than blocking. Selection entries follow
    /// `split_change`'s rules exactly (whole files, or verified hunk
    /// coordinates re-derived at mutation time and refused when the change
    /// moved underneath the panel).
    ///
    /// A selection covering every change in the source is the CLI's full
    /// squash: the emptied change is abandoned, its description folds into
    /// the destination's (destination first, no editor — the same curated
    /// combining as `squash_change`), bookmarks on it move to its parent,
    /// and an emptied working copy respawns as a new empty change. One
    /// curated deviation, same as `split_change`: a selection that changes
    /// nothing is refused, where the CLI warns and records nothing.
    fn squash_into(
        &self,
        path: &Path,
        change_id: &str,
        selection: &[SplitSelection],
        destination_id: &str,
    ) -> Result<MutationOutcome, BackendError>;

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

    /// Push bookmarks to a git remote (`jj git push --bookmark <name>...`),
    /// pinned against jj 0.41. Each named bookmark's remote counterpart is
    /// updated to the local position with force-with-lease semantics: the
    /// remote is expected to sit where the last fetch recorded it, so a
    /// remote moved by someone else refuses instead of overwriting their
    /// work. A bookmark with no remote counterpart is created and starts
    /// tracking it (the CLI's auto-track on push); a locally-deleted
    /// bookmark whose tracked remote still exists propagates the deletion.
    /// Backwards and sideways moves push fine — rewriting is jj's normal
    /// state of affairs and the lease is the safety.
    ///
    /// `remote` picks the git remote by name; `None` resolves like the CLI:
    /// the `git.push` setting, else the repo's sole remote, else "origin".
    /// Like the CLI, commits that would land on the remote are checked
    /// first — no description, conflicts, or a missing author/committer
    /// refuse the push before anything runs. Conflicted bookmarks and
    /// non-tracking remote counterparts refuse like the CLI. Pushing
    /// everything already matching records nothing.
    ///
    /// Curated deviations, both documented on the implementation: a name
    /// with nothing to push anywhere is refused (the CLI warns "No matching
    /// bookmarks" and exits cleanly — from an explicit UI action that is a
    /// bug), and when the remote rejects only part of a multi-bookmark push
    /// the accepted subset still records its operation while the refusal
    /// lists what moved on the remote (the CLI prints the same story but
    /// buries the partial success in warnings).
    fn push_bookmarks(
        &self,
        path: &Path,
        names: &[String],
        remote: Option<&str>,
    ) -> Result<MutationOutcome, BackendError>;

    /// Fetch from git remotes (`jj git fetch`), pinned against jj 0.41:
    /// update the remote-tracking refs from each remote and import what
    /// moved — tracked local bookmarks follow, new remote bookmarks arrive
    /// tracked or not per `git.auto-local-bookmark`, deleted remote
    /// branches prune (a tracked local bookmark follows the deletion, and
    /// newly-unreachable commits honor `git.abandon-unreachable-commits`).
    /// This is how "the remote moved under you" becomes visible: sync
    /// glyphs, behind-trunk counts, and conflicted bookmarks all read the
    /// refreshed remote state.
    ///
    /// `remotes` names exactly which remotes to fetch; `None` resolves like
    /// the CLI: the `git.fetch` setting (string or list, glob patterns
    /// honored), else the repo's sole remote, else literally "origin".
    /// Per-remote branch selection follows the CLI too:
    /// `remotes.<name>.fetch-bookmarks`/`fetch-tags` when set, else the
    /// remote's fetch refspecs from git config. The op description matches
    /// the CLI's (`fetch from git remote(s) a,b` in remote-listing order),
    /// and a fetch that changes nothing records nothing.
    ///
    /// One curated deviation, documented on the implementation: an
    /// explicitly-passed remote name that does not exist is refused, where
    /// the CLI warns and continues — from a UI action that is a stale plan.
    /// Config-driven names keep the CLI's forgiving behavior (a `git.fetch`
    /// entry pointing at a removed remote should not break the background
    /// cadence); only zero matching remotes is an error.
    fn git_fetch(
        &self,
        path: &Path,
        remotes: Option<&[String]>,
    ) -> Result<MutationOutcome, BackendError>;

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

    /// Resolve one conflicted file by launching the configured external
    /// 3-way merge tool and waiting for it to exit (`jj resolve <path> -r
    /// <rev>` with `ui.merge-editor` semantics; Sublime Merge is the
    /// default when nothing is configured and it is installed —
    /// `RepoSnapshot::resolve_tool` names what will run). The tool's saved
    /// output rewrites the change through the shared mutation plumbing;
    /// descendants rebase and a rewritten working copy is checked out, so
    /// resolving `@` updates the file on disk. Blocks for as long as the
    /// merge window stays open. One curated deviation from the CLI: the
    /// conflict is re-read after the tool exits, and if the change moved or
    /// the file changed underneath the open tool the resolution is refused
    /// ("resolve again") instead of rewriting stale state.
    fn resolve_conflict(
        &self,
        path: &Path,
        change_id: &str,
        file_path: &str,
    ) -> Result<MutationOutcome, BackendError>;

    /// Update a stale working copy to the repo's current state (`jj
    /// workspace update-stale`) — the guided recovery for the one state
    /// where every other mutation refuses to run. Acts on the current
    /// workspace only: sibling workspaces keep their working-copy state in
    /// their own roots, out of reach from here. Like the CLI, on-disk edits
    /// are first recorded on top of the working copy's *own* last
    /// operation (so nothing is lost or misattributed), then the working
    /// copy checks out the position the repo's view holds for it; when the
    /// working copy's last operation is missing from the op store entirely,
    /// its files are parked in a recovery commit instead. A workspace that
    /// is not stale records nothing. The checkout itself records no
    /// operation — jj's model too — so the outcome carries an operation id
    /// only on the recovery-commit path.
    fn update_stale_workspace(&self, path: &Path) -> Result<MutationOutcome, BackendError>;
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
            let follows_working_copy = mutation.is_time_travel();
            mutations.push(mutation);
            drop(mutations);
            // Revert/restore outcomes follow the working copy, which only
            // the replayed snapshot knows; other mutations answering no
            // target (a push moves nothing local) keep it that way.
            if follows_working_copy && outcome.target_change.is_none() {
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

    fn abandon_changes(
        &self,
        path: &Path,
        change_ids: &[String],
    ) -> Result<MutationOutcome, BackendError> {
        if change_ids.is_empty() {
            return Err(BackendError::MutationFailed(
                "there are no changes to abandon".to_owned(),
            ));
        }
        self.mutate(
            path,
            crate::mock::MockMutation::AbandonMany {
                ids: change_ids.to_vec(),
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

    fn split_change(
        &self,
        path: &Path,
        change_id: &str,
        selection: &[SplitSelection],
        description: &str,
    ) -> Result<MutationOutcome, BackendError> {
        self.mutate(
            path,
            crate::mock::MockMutation::Split {
                id: change_id.to_owned(),
                selection: selection.to_vec(),
                description: description.trim().to_owned(),
            },
        )
    }

    fn squash_into(
        &self,
        path: &Path,
        change_id: &str,
        selection: &[SplitSelection],
        destination_id: &str,
    ) -> Result<MutationOutcome, BackendError> {
        self.mutate(
            path,
            crate::mock::MockMutation::SquashInto {
                id: change_id.to_owned(),
                selection: selection.to_vec(),
                destination: destination_id.to_owned(),
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

    fn push_bookmarks(
        &self,
        path: &Path,
        names: &[String],
        remote: Option<&str>,
    ) -> Result<MutationOutcome, BackendError> {
        self.mutate(
            path,
            crate::mock::MockMutation::Push {
                names: names.iter().map(|n| n.trim().to_owned()).collect(),
                remote: remote.unwrap_or("origin").to_owned(),
            },
        )
    }

    fn git_fetch(
        &self,
        path: &Path,
        remotes: Option<&[String]>,
    ) -> Result<MutationOutcome, BackendError> {
        // The fabricated remote never moves, so a mock fetch is always the
        // real backend's nothing-new no-op (recording nothing) — after the
        // same explicit-unknown-remote refusal. Documented approximation:
        // fabricated remote state is static, there is nothing to import.
        let snapshot = self.overlaid_snapshot(path)?;
        if let Some(names) = remotes {
            for raw in names {
                let name = raw.trim();
                if !name.is_empty() && !snapshot.git_remotes.iter().any(|r| r.name == name) {
                    return Err(BackendError::MutationFailed(format!(
                        "there is no git remote named \u{201c}{name}\u{201d}"
                    )));
                }
            }
        }
        let label = match remotes {
            Some([one]) => one.trim().to_owned(),
            _ => snapshot
                .git_remotes
                .first()
                .map(|r| r.name.clone())
                .unwrap_or_else(|| "origin".to_owned()),
        };
        Ok(MutationOutcome {
            operation_id: None,
            summary: format!("Nothing new on {label}"),
            target_change: None,
        })
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

    fn resolve_conflict(
        &self,
        path: &Path,
        change_id: &str,
        file_path: &str,
    ) -> Result<MutationOutcome, BackendError> {
        self.mutate(
            path,
            crate::mock::MockMutation::Resolve {
                id: change_id.to_owned(),
                path: file_path.to_owned(),
            },
        )
    }

    fn update_stale_workspace(&self, path: &Path) -> Result<MutationOutcome, BackendError> {
        // The mock's stale workspace (`review`) is a sibling, and recovery
        // acts on the current workspace only — which the mock always
        // fabricates fresh. Answer with the real backend's no-op.
        validate_repo_path(path)?;
        Ok(MutationOutcome {
            operation_id: None,
            summary: "The workspace is not stale".to_owned(),
            target_change: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snapshot::SplitHunk;

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
    fn mock_git_fetch_answers_the_no_op() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join(".jj")).unwrap();
        let backend = MockBackend::default();
        let before = backend.open(dir.path()).unwrap();

        // The fabricated remote never moves, so a mock fetch is always the
        // real backend's nothing-new answer, recording nothing.
        let outcome = backend.git_fetch(dir.path(), None).unwrap();
        assert!(outcome.operation_id.is_none());
        assert_eq!(outcome.summary, "Nothing new on origin");
        let outcome = backend
            .git_fetch(dir.path(), Some(&["origin".into()]))
            .unwrap();
        assert_eq!(outcome.summary, "Nothing new on origin");
        let err = backend
            .git_fetch(dir.path(), Some(&["nosuch".into()]))
            .unwrap_err();
        assert!(err.to_string().contains("no git remote named"), "{err}");
        let after = backend.open(dir.path()).unwrap();
        assert_eq!(after.operations.len(), before.operations.len());
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
    fn mock_resolve_settles_one_conflict_item_at_a_time() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join(".jj")).unwrap();
        let backend = MockBackend::default();
        let before = backend.open(dir.path()).unwrap();
        assert_eq!(before.resolve_tool.as_deref(), Some("smerge"));
        let path = "src/lib/components/conflicts/ConflictList.svelte";

        // Wrong paths and clean changes refuse like the real backend.
        let err = backend
            .resolve_conflict(dir.path(), "pmwzqkvt", "nope.txt")
            .unwrap_err();
        assert!(matches!(err, BackendError::MutationFailed(_)), "got {err:?}");

        let outcome = backend.resolve_conflict(dir.path(), "pmwzqkvt", path).unwrap();
        assert!(outcome.operation_id.is_some());
        assert_eq!(outcome.summary, format!("Resolved {path} in pmwzqkvt"));

        // The parent's item settled and its node reads clean; the child
        // still carries its inherited copy until resolved itself.
        let after = backend.open(dir.path()).unwrap();
        assert!(!after
            .conflicts
            .iter()
            .any(|c| c.node_id.as_deref() == Some("pmwzqkvt")));
        assert!(!after.nodes.iter().find(|n| n.id == "pmwzqkvt").unwrap().has_conflict);
        assert!(after
            .conflicts
            .iter()
            .any(|c| c.node_id.as_deref() == Some("qvlxnsry") && c.kind == crate::snapshot::ConflictKind::File));
        assert!(after.operations[0].description.starts_with("Resolve conflicts in commit"));

        // Resolving the already-settled path now refuses.
        let err = backend.resolve_conflict(dir.path(), "pmwzqkvt", path).unwrap_err();
        assert!(matches!(err, BackendError::MutationFailed(_)), "got {err:?}");
    }

    #[test]
    fn mock_split_carves_a_new_change_above() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join(".jj")).unwrap();
        let backend = MockBackend::default();
        let before = backend.open(dir.path()).unwrap();
        let wc_id = before.working_copy.clone();
        let wc_parents = before
            .nodes
            .iter()
            .find(|n| n.id == wc_id)
            .unwrap()
            .parents
            .clone();
        let files = backend.change_detail(dir.path(), &wc_id).unwrap().files;
        assert!(files.len() >= 2, "fixture working copy has files to split");

        // Degenerate selections refuse like the real backend.
        let err = backend
            .split_change(dir.path(), &wc_id, &[SplitSelection::whole("nope.txt")], "x")
            .unwrap_err();
        assert!(matches!(err, BackendError::MutationFailed(_)), "got {err:?}");
        let all: Vec<SplitSelection> = files
            .iter()
            .map(|f| SplitSelection::whole(f.path.clone()))
            .collect();
        let err = backend.split_change(dir.path(), &wc_id, &all, "x").unwrap_err();
        assert!(matches!(err, BackendError::MutationFailed(_)), "got {err:?}");

        // A hunk entry counts as partially kept, so covering every file is
        // fine as long as one of them is partial (its unchosen hunks stay
        // with the remainder; the mock does not validate coordinates —
        // fixture diffs are static).
        let mut selected: Vec<SplitSelection> = files
            .iter()
            .take(files.len() - 1)
            .map(|f| SplitSelection::whole(f.path.clone()))
            .collect();
        selected.push(SplitSelection {
            path: files[files.len() - 1].path.clone(),
            hunks: Some(vec![SplitHunk {
                old_start: 1,
                new_start: 1,
                old_lines: 1,
                new_lines: 1,
            }]),
        });
        let outcome = backend
            .split_change(dir.path(), &wc_id, &selected, "carved: first file")
            .unwrap();
        assert!(outcome.operation_id.is_some());
        assert_eq!(outcome.target_change.as_deref(), Some(wc_id.as_str()));
        assert!(
            outcome.summary.contains(&format!(
                "kept {} file{} and parts of 1 more",
                files.len() - 1,
                if files.len() - 1 == 1 { "" } else { "s" }
            )),
            "got {}",
            outcome.summary
        );

        // The carved half keeps the change id, its original parents, and the
        // new description; the remainder sits directly on top with the old
        // description and the working-copy status.
        let after = backend.open(dir.path()).unwrap();
        let carved = after.nodes.iter().find(|n| n.id == wc_id).unwrap();
        assert_eq!(carved.description, "carved: first file");
        assert_eq!(carved.parents, wc_parents);
        assert_ne!(after.working_copy, wc_id);
        let remainder = after
            .nodes
            .iter()
            .find(|n| n.id == after.working_copy)
            .unwrap();
        assert_eq!(remainder.parents, vec![wc_id.clone()]);
        assert_eq!(
            remainder.description,
            before.nodes.iter().find(|n| n.id == wc_id).unwrap().description
        );
        assert!(after.operations[0].description.starts_with("split commit "));
        // The active workstream gained the remainder right above the carved
        // change.
        let active = after.workstreams.iter().find(|w| w.is_active).unwrap();
        let remainder_at = active.node_ids.iter().position(|n| *n == remainder.id).unwrap();
        assert_eq!(active.node_ids.get(remainder_at + 1), Some(&wc_id));
    }

    #[test]
    fn mock_squash_into_replays_partial_and_full_moves() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join(".jj")).unwrap();
        let backend = MockBackend::default();
        let before = backend.open(dir.path()).unwrap();
        let wc_id = before.working_copy.clone();
        let wc_parent = before
            .nodes
            .iter()
            .find(|n| n.id == wc_id)
            .unwrap()
            .parents[0]
            .clone();
        let files = backend.change_detail(dir.path(), &wc_id).unwrap().files;
        assert!(files.len() >= 2, "fixture working copy has files to move");
        let dest_id = wc_parent.clone();

        // Refusals mirror the real backend: self destination, selections
        // that change nothing.
        let err = backend
            .squash_into(dir.path(), &wc_id, &[SplitSelection::whole(files[0].path.clone())], &wc_id)
            .unwrap_err();
        assert!(matches!(err, BackendError::MutationFailed(_)), "got {err:?}");
        let err = backend
            .squash_into(dir.path(), &wc_id, &[SplitSelection::whole("nope.txt")], &dest_id)
            .unwrap_err();
        assert!(matches!(err, BackendError::MutationFailed(_)), "got {err:?}");

        // A partial move records but leaves the graph shape alone (fixture
        // diffs are static — the documented approximation).
        let outcome = backend
            .squash_into(
                dir.path(),
                &wc_id,
                &[SplitSelection::whole(files[0].path.clone())],
                &dest_id,
            )
            .unwrap();
        assert_eq!(outcome.summary, format!("Moved 1 file from {wc_id} into {dest_id}"));
        assert_eq!(outcome.target_change.as_deref(), Some(wc_id.as_str()));
        let after = backend.open(dir.path()).unwrap();
        assert!(after.nodes.iter().any(|n| n.id == wc_id));
        assert!(after.operations[0].description.starts_with("squash commits into "));

        // A full selection abandons the emptied source: the working copy
        // respawns on its parent and the selection follows the destination.
        backend.create_bookmark(dir.path(), "tmp", &wc_id).unwrap();
        let all: Vec<SplitSelection> = files
            .iter()
            .map(|f| SplitSelection::whole(f.path.clone()))
            .collect();
        let outcome = backend.squash_into(dir.path(), &wc_id, &all, &dest_id).unwrap();
        assert_eq!(
            outcome.summary,
            format!(
                "Moved everything in {wc_id} into {dest_id}; \
                 the emptied change was abandoned"
            )
        );
        assert_eq!(outcome.target_change.as_deref(), Some(dest_id.as_str()));
        let after = backend.open(dir.path()).unwrap();
        assert!(!after.nodes.iter().any(|n| n.id == wc_id));
        assert_ne!(after.working_copy, wc_id);
        let wc_node = after.nodes.iter().find(|n| n.id == after.working_copy).unwrap();
        assert_eq!(wc_node.parents, vec![wc_parent.clone()]);
        // The bookmark on the abandoned source moved to its parent — the
        // parent happens to be the destination here, like `jj squash -r`.
        let tmp = after.bookmarks.iter().find(|b| b.name == "tmp").unwrap();
        assert_eq!(tmp.target, wc_parent);
    }

    #[test]
    fn mock_push_replays_tracking_and_sync_state() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join(".jj")).unwrap();
        let backend = MockBackend::default();
        let before = backend.open(dir.path()).unwrap();
        let local_only = before
            .bookmarks
            .iter()
            .find(|b| b.name == "diff-virtualization")
            .unwrap();
        assert_eq!(local_only.sync, crate::snapshot::SyncState::LocalOnly);

        // Pushing the local-only bookmark (already-synced main just skips)
        // records one op with the CLI's description; it reads tracked and
        // synced after.
        let outcome = backend
            .push_bookmarks(
                dir.path(),
                &["main".into(), "diff-virtualization".into()],
                None,
            )
            .unwrap();
        assert_eq!(outcome.summary, "Pushed diff-virtualization to origin");
        assert!(outcome.target_change.is_none());
        let after = backend.open(dir.path()).unwrap();
        let pushed = after
            .bookmarks
            .iter()
            .find(|b| b.name == "diff-virtualization")
            .unwrap();
        assert_eq!(pushed.sync, crate::snapshot::SyncState::Synced);
        assert_eq!(pushed.remote.as_deref(), Some("origin"));
        assert_eq!(
            after.operations[0].description,
            "push bookmark diff-virtualization to git remote origin"
        );

        // Re-pushing records nothing; conflicted bookmarks, bookmarks on
        // conflicted changes, and unknown names refuse like the real
        // backend.
        let noop = backend
            .push_bookmarks(dir.path(), &["diff-virtualization".into()], None)
            .unwrap();
        assert!(noop.operation_id.is_none());
        let err = backend
            .push_bookmarks(dir.path(), &["watcher-fix".into()], None)
            .unwrap_err();
        assert!(err.to_string().contains("is conflicted"), "{err}");
        let err = backend
            .push_bookmarks(dir.path(), &["conflict-inbox".into()], None)
            .unwrap_err();
        assert!(err.to_string().contains("has conflicts"), "{err}");
        let err = backend
            .push_bookmarks(dir.path(), &["nope".into()], None)
            .unwrap_err();
        assert!(matches!(err, BackendError::BookmarkMissing(_)), "got {err:?}");
    }

    #[test]
    fn mock_stale_workspace_is_a_sibling_preview_only() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join(".jj")).unwrap();
        let backend = MockBackend::default();
        let snapshot = backend.open(dir.path()).unwrap();

        let current = snapshot.workspaces.iter().find(|w| w.is_current).unwrap();
        assert!(current.is_default);
        assert!(!current.is_stale);
        let review = snapshot.workspaces.iter().find(|w| w.name == "review").unwrap();
        assert!(review.is_stale);
        assert!(!review.is_current);
        let item = snapshot
            .conflicts
            .iter()
            .find(|c| c.kind == crate::snapshot::ConflictKind::StaleWorkspace)
            .unwrap();
        assert_eq!(item.workspace.as_deref(), Some("review"));

        // Recovery acts on the current workspace, which the mock keeps
        // fresh — the real backend's no-op, with nothing recorded.
        let outcome = backend.update_stale_workspace(dir.path()).unwrap();
        assert!(outcome.operation_id.is_none());
        assert_eq!(outcome.summary, "The workspace is not stale");
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
