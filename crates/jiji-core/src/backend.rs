//! The adapter boundary between Jiji and repository state.
//!
//! Everything UI-facing goes through `RepoBackend`, so the interface never
//! depends on jj-lib directly and the mock backend can stand in for it.

use std::path::Path;

use crate::snapshot::{
    ChangeDetail, ChangeDiff, ChangeEvolog, ConflictSide, FileAnnotation,
    HistorySearch, LandedCheck, MutationOutcome, RepoSnapshot, SnapshotView, SplitHunk,
    SplitSelection, TipCheck,
};

#[derive(Debug, thiserror::Error)]
pub enum BackendError {
    #[error("Path does not exist: {0}")]
    PathMissing(String),
    #[error("Not a directory: {0}")]
    NotADirectory(String),
    #[error("Not a JJ repository (no .jj directory found): {0}")]
    NotAJjRepo(String),
    /// A Git repository jj has not been set up in; carries the Git root,
    /// which is where `init_colocated` would put `.jj`.
    #[error("{0} is a Git repository that jj is not set up in yet")]
    GitRepoWithoutJj(String),
    #[error("Could not open repository: {0}")]
    OpenFailed(String),
    #[error("Could not read repository state: {0}")]
    SnapshotFailed(String),
    #[error("Change {0} is not in the repository anymore")]
    ChangeMissing(String),
    #[error("Your jj configuration could not be loaded: {0}")]
    ConfigInvalid(String),
    #[error("The revset could not be used: {0}")]
    RevsetInvalid(String),
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
            BackendError::GitRepoWithoutJj(_) => "git_repo_without_jj",
            BackendError::OpenFailed(_) => "open_failed",
            BackendError::SnapshotFailed(_) => "snapshot_failed",
            BackendError::ChangeMissing(_) => "change_missing",
            BackendError::ConfigInvalid(_) => "config_invalid",
            BackendError::RevsetInvalid(_) => "revset_invalid",
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
        if let Some(git_root) = git_root_for(path) {
            return Err(BackendError::GitRepoWithoutJj(git_root.display().to_string()));
        }
        return Err(BackendError::NotAJjRepo(display));
    }
    Ok(())
}

/// The root of the plain Git repository enclosing `path`, if any: the
/// nearest ancestor holding a `.git` directory. Linked worktrees and
/// submodules (a `.git` file) don't count — jj cannot colocate there.
pub fn git_root_for(path: &Path) -> Option<std::path::PathBuf> {
    path.ancestors()
        .find(|dir| dir.join(".git").exists())
        .filter(|dir| dir.join(".git").is_dir())
        .map(Path::to_path_buf)
}

/// How [`RepoBackend::abandon_landed`] starts its answer when it left the
/// landed commits where they are because someone else's branch still
/// builds on them.
pub const LANDED_LEFT_IN_PLACE: &str = "Left ";

pub trait RepoBackend: Send + Sync {
    /// Open the repository at `path` and produce an immutable UI snapshot.
    /// Re-invoking on an already-open path acts as a refresh.
    fn open(&self, path: &Path) -> Result<RepoSnapshot, BackendError>;

    /// Like `open`, but rendering the given view instead of the default
    /// workstreams lens. Backends that render only the default view (the
    /// base mock) echo the request so the UI still knows what was asked.
    /// A `Custom` view whose revset does not parse or resolve is refused
    /// with `RevsetInvalid`.
    fn open_with(&self, path: &Path, view: &SnapshotView) -> Result<RepoSnapshot, BackendError> {
        let mut snapshot = self.open(path)?;
        snapshot.view = view.clone();
        Ok(snapshot)
    }

    /// Like `open`, but first brings the repo up to date the way running
    /// any jj CLI command would: snapshot on-disk working-copy edits into
    /// `@`, and in a colocated repo import an externally-moved git HEAD and
    /// git refs. Each step records its own operation; an already-current
    /// repo records nothing. This is what the app calls on open, manual
    /// refresh, and every watcher tick, so `@` and its diff track the disk.
    fn refresh(&self, path: &Path) -> Result<RepoSnapshot, BackendError> {
        self.open(path)
    }

    /// `refresh` rendering the given view; see `open_with`.
    fn refresh_with(&self, path: &Path, view: &SnapshotView) -> Result<RepoSnapshot, BackendError> {
        let mut snapshot = self.refresh(path)?;
        snapshot.view = view.clone();
        Ok(snapshot)
    }

    /// Set jj up in the plain Git repository rooted at `root`, the way
    /// `jj git init --colocate` does there: `.jj` lands next to `.git`, the
    /// Git refs are imported, and Git keeps working alongside. Refuses a
    /// folder that already has `.jj` or is not a Git root. `open` takes it
    /// from there (importing HEAD and snapshotting the working copy).
    fn init_colocated(&self, root: &Path) -> Result<(), BackendError> {
        Err(BackendError::MutationFailed(format!(
            "this backend cannot set up jj in {}",
            root.display()
        )))
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

    /// Abandon exactly these commits, as [`RepoBackend::abandon_changes`]
    /// does, unless something that is not one of them sits on them. Then
    /// nothing is abandoned and the answer names what sits there.
    ///
    /// This is the sweep after a landing. Abandoning a commit rebases its
    /// children onto what was below it, which for a landed commit is
    /// trunk as it was before the landing: a child's diff survives, and
    /// its tree loses what just landed. The caller moves the children it
    /// knows of onto trunk first. One started after it looked is caught
    /// here, under the write lock and after the working copy is
    /// snapshotted, so there is no moment in between.
    ///
    /// Someone else's commits can sit on the landed ones too: a pull
    /// request that was based on the landed branch and whose branch has
    /// not been rebased yet. Those are not this repository's to move.
    /// The landed commits then stay, nothing is recorded, and the answer
    /// says so, starting with [`LANDED_LEFT_IN_PLACE`] and naming the
    /// branch. A landed commit that is immutable only because such a
    /// branch has it in its history is swept like any other.
    fn abandon_landed(
        &self,
        path: &Path,
        commits: &[String],
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
    /// rest of the file stays with the remainder. A hunk entry may narrow
    /// further to individual lines (`SplitHunk::lines`): only the picked
    /// added/removed lines move, the hunk's other changes stay with the
    /// remainder. Hunk-level entries must be regular non-conflicted text
    /// files; the hunk coordinates (and any line picks) are re-derived
    /// from the trees at mutation time and refused when they no longer
    /// match what the panel rendered (the change moved underneath —
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

    /// Move a change and all its descendants off landed history and onto
    /// trunk as `remote` has it: the commit of the branch `trunk` on that
    /// remote after the last fetch. The caller names the branch, the one
    /// its plan was made for. `trunk()` is not asked again: a fetch can
    /// change what it answers, and it can sit ahead of the remote with
    /// commits nobody pushed, as the local bookmark of that name can; a
    /// stack rebased onto those would carry them into its pull requests.
    /// This is the commit [`RepoBackend::landed_check`] judges a landing
    /// against, so what was checked and what is built on are one commit.
    /// Refused when the remote has no such branch here.
    ///
    /// Only the parents that landed are replaced: those named in `landed`
    /// (commit ids), and any that are part of that trunk already. A change
    /// that merges a side branch in keeps that branch as its other parent,
    /// and with it everything the branch brings. A change none of whose
    /// parents landed is left where it is.
    fn rebase_onto_trunk(
        &self,
        path: &Path,
        remote: &str,
        trunk: &str,
        change_id: &str,
        landed: &[String],
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

    /// Delete a local bookmark only while it still points at `commit` (a
    /// commit id, or a unique prefix of one). A bookmark that moved since
    /// the caller looked is refused and left where it is: what it points
    /// at now is not what the caller decided about. The comparison runs
    /// inside the mutation, after the working copy is snapshotted, so an
    /// edit made in the meantime counts.
    fn delete_bookmark_at(
        &self,
        path: &Path,
        name: &str,
        commit: &str,
    ) -> Result<MutationOutcome, BackendError>;

    /// Start tracking a remote bookmark (`jj bookmark track name@remote`):
    /// the local bookmark takes (or merges in) the remote position and
    /// follows the remote on future fetches. Refuses a remote bookmark
    /// that does not exist or is already tracked.
    fn track_bookmark(
        &self,
        path: &Path,
        name: &str,
        remote: &str,
    ) -> Result<MutationOutcome, BackendError>;

    /// Stop tracking a remote bookmark (`jj bookmark untrack
    /// name@remote`): the local bookmark keeps its position but no longer
    /// follows the remote. Refuses a remote bookmark that does not exist
    /// or is not tracked.
    fn untrack_bookmark(
        &self,
        path: &Path,
        name: &str,
        remote: &str,
    ) -> Result<MutationOutcome, BackendError>;

    /// Take over a branch someone else made: `name@remote`, a remote
    /// bookmark that is no bookmark here. It becomes a local bookmark at
    /// the remote position and follows the remote from then on, exactly
    /// as `track_bookmark` would leave it, and the repository records for
    /// which pull request it was taken over and whose that is
    /// ([`crate::snapshot::TakenOver`]), so that every plan that pushes
    /// the branch can say whose it is. Nothing is fetched and nothing is
    /// pushed: the caller fetches first, and the first push is a later
    /// one, under `push_bookmarks`' lease like any other. Refuses a branch
    /// that is tracked already, and a name another local bookmark has.
    fn take_over_branch(
        &self,
        path: &Path,
        name: &str,
        remote: &str,
        number: u64,
        author: Option<&str>,
    ) -> Result<MutationOutcome, BackendError>;

    /// Give a taken-over branch back: the reverse, as one operation. The
    /// remote bookmark is untracked and the local bookmark removed; the
    /// branch on the remote is not touched and nothing is pushed. Refuses
    /// a branch that was not taken over, one that is not here what its
    /// remote has (what differs would be left without a bookmark), and
    /// one that another local bookmark sits on (that work would rest on
    /// someone else's branch).
    fn give_back_branch(
        &self,
        path: &Path,
        name: &str,
        remote: &str,
    ) -> Result<MutationOutcome, BackendError>;

    /// Delete a branch on the remote — and its local bookmark, when one
    /// exists. The composite of the CLI dance (track if untracked, delete
    /// the local bookmark, push): the local delete records one operation,
    /// the push another, and the push carries `push_bookmarks`' usual
    /// force-with-lease safety — a remote that moved since the last fetch
    /// refuses instead of deleting someone's new work (the local deletion
    /// then stands, unpushed, exactly as the CLI would leave it).
    fn delete_remote_bookmark(
        &self,
        path: &Path,
        name: &str,
        remote: &str,
    ) -> Result<MutationOutcome, BackendError>;

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

    /// Fetch a pull request's current head into a new local bookmark for
    /// local review. GitHub publishes every PR's head as
    /// `refs/pull/<N>/head` on the base repository — which is what makes
    /// cross-fork PRs fetchable without ever touching the fork — but jj
    /// itself cannot fetch non-branch refs (jj-vcs/jj#4388), so the fetch
    /// runs git directly against the repo's backing git store (the same
    /// subprocess and `git.executable-path` setting jj's own fetch uses)
    /// through a temporary ref, and the arrived commit becomes a visible
    /// head with `bookmark` pointing at it, all recorded as one operation.
    ///
    /// The bookmark is an ordinary local bookmark: it reviews, compares,
    /// and deletes like any other, and deleting it touches nothing remote
    /// (it tracks no remote branch). There is no jj CLI command to pin
    /// against, so the op description is Jiji's own (`fetch pull request
    /// #N into bookmark <name>`). Taken names refuse like
    /// `create_bookmark`; unknown remotes refuse like an explicitly-named
    /// `git_fetch`; a PR head that is a rewrite of local work arrives as a
    /// divergent copy, surfaced first-class like any other divergence.
    fn fetch_pr_head(
        &self,
        path: &Path,
        remote: &str,
        number: u64,
        bookmark: &str,
    ) -> Result<MutationOutcome, BackendError>;

    /// One change's evolution (`jj evolog`): its current commit followed
    /// by the predecessors it was rewritten from, newest first, each
    /// annotated with the operation that recorded it when the op log still
    /// carries it. Squash-shaped rewrites list absorbed source commits in
    /// the chain too, exactly like the CLI. Read-only; per-version diffs go
    /// through `compare_diff`, which resolves hidden commit ids.
    fn change_evolog(&self, path: &Path, change_id: &str) -> Result<ChangeEvolog, BackendError>;

    /// Restore a change to one of its earlier versions: its commit is
    /// rewritten to carry the version's tree and description. The change
    /// does not move — parents and author stay as they are, so a version
    /// from before a rebase restores content without undoing the rebase —
    /// and descendants rebase through the shared mutation plumbing. There
    /// is no jj CLI verb to pin against, so the op description is Jiji's
    /// own (`restore commit <hex> to version <hex>`); the operation is
    /// revertible like any other. Refuses a commit id that is not in the
    /// change's predecessor chain (a stale panel after the change moved);
    /// restoring to a version identical to the current state records
    /// nothing.
    fn restore_change_version(
        &self,
        path: &Path,
        change_id: &str,
        commit_id: &str,
    ) -> Result<MutationOutcome, BackendError>;

    /// Annotate one file line by line (`jj file annotate` semantics):
    /// which change introduced each line of `file_path` as the file stands
    /// in `change_id`'s tree. Read-only. Attribution follows jj's
    /// annotation walk over the whole visible graph, so a line's origin is
    /// the visible commit that introduced it, addressed like
    /// `GraphNode::id` for jumping. Binary and oversized files are
    /// refused (there is nothing line-shaped to attribute); a path absent
    /// from the change's tree is refused by name.
    fn file_annotate(
        &self,
        path: &Path,
        change_id: &str,
        file_path: &str,
    ) -> Result<FileAnnotation, BackendError>;

    /// Search visible history for a string (`jj log -S` semantics, the
    /// pickaxe): walk commits newest first, diff each against its parent
    /// tree(s), and report the changes where the query's occurrence count
    /// differs between the two sides of some file — the changes that
    /// introduced or removed the string, skipping ones that only moved it
    /// around. A file whose *path* contains the query also matches, so one
    /// box answers "which change touched this file" too. Matching is
    /// case-insensitive (ASCII); binary and oversized files count only by
    /// path. Read-only and bounded: the walk stops at internal commit/hit
    /// caps or a time budget and reports itself `truncated`, with
    /// `scanned` saying how far it got. An empty query answers empty.
    fn search_history(&self, path: &Path, query: &str) -> Result<HistorySearch, BackendError>;

    /// The first of `candidates` (repo-relative paths, matched exactly)
    /// that exists as a regular non-conflicted UTF-8 file in the trunk
    /// change's tree, answered as `(matched path, text)`. Read-only — how
    /// forge workflows read repo conventions (a PR template) from what
    /// trunk actually carries, which is where GitHub reads them from too.
    /// Files over 100 KiB, binary files, and conflicted entries are
    /// skipped rather than failing the probe.
    fn trunk_text_file(
        &self,
        path: &Path,
        candidates: &[String],
    ) -> Result<Option<(String, String)>, BackendError>;

    /// Whether trunk holds the run of changes from `bottom` up to `top`,
    /// on the strength of a pull request a forge reports merged. Trunk is
    /// the trunk branch as `remote` has it after the last fetch (see
    /// [`RepoBackend::rebase_onto_trunk`]). `landing` is the commit the
    /// forge names as that merge and `head` the commit it recorded as the
    /// pull request's head; `bottom` and `top` are commit or node ids,
    /// `top` reached from `bottom` along first parents. Read-only.
    ///
    /// "Holds" is the question a rebase would answer: applying the run's
    /// whole diff (from the commit below `bottom` in the stack, its first
    /// parent, up to `top`) on top of a commit must alter nothing. That
    /// is true of a squash, a rebase or a merge commit of exactly these
    /// changes, whatever else trunk gained underneath, and false as soon
    /// as one line of them is absent. The first parent and not all
    /// parents: when `bottom` merges a side branch in, that branch's
    /// content is part of what the pull request carries.
    ///
    /// Which commit is asked depends on how the changes relate to trunk.
    /// When the run's own commits are part of trunk's history (a merge
    /// commit, or trunk moved onto the branch), the commit asked is the
    /// one where they entered trunk's first-parent line. Being in the
    /// history is not enough: a merge can take a branch in and keep none
    /// of its content. And what is asked of that commit is everything it
    /// brought in, from where the branch left trunk's line up to `top`,
    /// whatever `bottom` the caller knows: a caller that has only the
    /// pull request's head still gets an answer about the whole pull
    /// request. (Where trunk and the branch merged each other before,
    /// there are several such places, and the commit has to hold what
    /// came in from each.) Otherwise, when the pull request was merged from
    /// exactly `top`, `landing` is asked first and not trunk's tip, so a
    /// later commit by someone else that edits the same lines, or a
    /// revert, does not turn a good landing into a missing one; trunk's
    /// tip is asked when `landing` is not the answer (it is off trunk, or
    /// lacks something), because changes can reach trunk by another road
    /// than their own pull request's merge. When the pull request was
    /// merged from some other commit, its merge says nothing about these
    /// changes (the branch name was used before, or the changes were
    /// edited since), and only trunk's tip can vouch for them.
    #[allow(clippy::too_many_arguments)]
    fn landed_check(
        &self,
        path: &Path,
        remote: &str,
        trunk: &str,
        landing: &str,
        head: &str,
        bottom: &str,
        top: &str,
    ) -> Result<LandedCheck, BackendError>;

    /// Whether landing a stack one pull request at a time, starting now,
    /// would leave trunk as one merge of the stack's tip would. Trunk is
    /// the branch `trunk` as `remote` has it after the last fetch.
    /// `bottom` and `first_top` are the ends of the pull request that
    /// lands first, and `tip` is the top of the stack; all three are
    /// commit or node ids on one first-parent line. Read-only.
    ///
    /// The two can differ when trunk has moved under the stack. One merge
    /// of the tip applies the stack's net change. One pull request at a
    /// time applies each commit's own change in turn: the first pull
    /// request as the forge merges it, in one piece, and then every
    /// commit above it rebased onto the result, which is what follows a
    /// landing. Where trunk made a change that a lower commit makes too,
    /// and a higher commit changes the same thing again, the net change
    /// leaves trunk's version alone and the commits in turn replace it
    /// (seen on GitHub: a flag trunk had turned on ended up off).
    ///
    /// Both results are computed here, with jj's merge and not the
    /// forge's, and compared. Trunk is as the last fetch left it, so a
    /// change that reached the remote since is not seen.
    ///
    /// The same question is asked after a landing, before what is left
    /// of the stack is rebased onto trunk, with `first_top` equal to
    /// `bottom`: every commit replayed in turn, as that rebase does it.
    /// The stack then sits on the landed commits, which are not part of
    /// trunk's history when the landing was a squash. That base counts as
    /// long as trunk holds everything it holds.
    fn lands_as_tip(
        &self,
        path: &Path,
        remote: &str,
        trunk: &str,
        bottom: &str,
        first_top: &str,
        tip: &str,
    ) -> Result<TipCheck, BackendError>;

    /// The remote branches that are no bookmarks here and still have any
    /// of `commits` in their history, as `name@remote`: whose pull
    /// requests keep those commits around. A commit that is part of
    /// trunk's history rests under none: every branch made since has it.
    /// Read-only. Landing asks it about landed commits it finds it has
    /// nothing to sweep of, to say which of two things happened: they
    /// became part of trunk's history, or someone else's branch still
    /// builds on them.
    fn branches_resting_on(
        &self,
        path: &Path,
        commits: &[String],
    ) -> Result<Vec<String>, BackendError>;

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

    /// Resolve one conflicted file by keeping one side wholesale — the
    /// CLI's built-in `:ours`/`:theirs` merge tools (`jj resolve --tool
    /// :ours`): "ours" keeps side #1 of the conflict, "theirs" side #2. No
    /// tool launches and nothing blocks. The picked side's content —
    /// including its absence, when that side deleted the file — rewrites
    /// the change through the shared mutation plumbing; descendants rebase
    /// and a rewritten working copy is checked out. Refuses the same shapes
    /// the external-tool path refuses: non-conflicted paths, non-file
    /// conflicts, conflicts with more than 2 sides, and executable-bit
    /// conflicts.
    fn resolve_conflict_take(
        &self,
        path: &Path,
        change_id: &str,
        file_path: &str,
        side: ConflictSide,
    ) -> Result<MutationOutcome, BackendError>;

    /// Revert one file inside a change to its parent-tree content — `jj
    /// restore` scoped to that change and path. The file's diff disappears
    /// from the change (deleted entirely when the change added it);
    /// descendants rebase through the shared mutation plumbing. With
    /// `hunks`, only those hunks of the file's diff are discarded: the
    /// coordinates are verified against a freshly computed diff under
    /// `split_change`'s rules (refused when the change moved underneath
    /// the surface), the file's other hunks stay put, and a selection
    /// covering every hunk behaves as the whole-file restore.
    fn restore_file(
        &self,
        path: &Path,
        change_id: &str,
        file_path: &str,
        hunks: Option<&[SplitHunk]>,
    ) -> Result<MutationOutcome, BackendError>;

    /// Copy a change in place (`jj duplicate`): a new change with the same
    /// tree, parents, description, and author beside the original. No
    /// immutability gate — duplicating rewrites nothing, and copying a
    /// landed change to rework it is the point.
    fn duplicate_change(
        &self,
        path: &Path,
        change_id: &str,
    ) -> Result<MutationOutcome, BackendError>;

    /// Back out a change (`jj revert -r <rev> -o @`): a new change on top of
    /// the working copy whose tree applies the reverse of the target's diff,
    /// described the way the CLI's default `templates.revert_description`
    /// would. The working copy does not move — the back-out sits above `@`
    /// as a new anonymous head, like the CLI's `--onto` form. No
    /// immutability gate — nothing is rewritten, and backing out landed
    /// work is the point. A reverse diff that no longer applies cleanly
    /// records a first-class conflict rather than blocking.
    fn revert_change(&self, path: &Path, change_id: &str)
        -> Result<MutationOutcome, BackendError>;

    /// Sink a change's hunks into the mutable ancestors whose diffs last
    /// touched those lines (`jj absorb`). Hunks no ancestor claims stay
    /// put; the source is abandoned when everything absorbs away and
    /// nothing else names it. Refuses (rather than records nothing) when
    /// no hunk has a destination, so the surface can explain why.
    fn absorb_change(
        &self,
        path: &Path,
        change_id: &str,
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
        // Restore-fabricated version ids answer as the version they
        // restored — their trees are the same, and it keeps the evolution
        // surface's comparisons working after a replayed restore.
        let (from, to) = {
            let mutations = self.mutations.lock().expect("mock state lock poisoned");
            (
                crate::mock::resolve_version_commit(&mutations, from_change_id),
                crate::mock::resolve_version_commit(&mutations, to_change_id),
            )
        };
        crate::mock::mock_compare_diff(&snapshot, &from, &to)
    }

    fn change_evolog(&self, path: &Path, change_id: &str) -> Result<ChangeEvolog, BackendError> {
        let snapshot = self.overlaid_snapshot(path)?;
        let mutations = self.mutations.lock().expect("mock state lock poisoned");
        let active = crate::mock::active_effects(&mutations);
        crate::mock::mock_change_evolog(&snapshot, &mutations, &active, change_id)
    }

    fn file_annotate(
        &self,
        path: &Path,
        change_id: &str,
        file_path: &str,
    ) -> Result<FileAnnotation, BackendError> {
        let snapshot = self.overlaid_snapshot(path)?;
        crate::mock::mock_file_annotation(&snapshot, change_id, file_path)
    }

    fn restore_change_version(
        &self,
        path: &Path,
        change_id: &str,
        commit_id: &str,
    ) -> Result<MutationOutcome, BackendError> {
        self.mutate(
            path,
            crate::mock::MockMutation::RestoreVersion {
                id: change_id.to_owned(),
                commit: commit_id.to_owned(),
            },
        )
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
        // Like the real backend, an id may name a commit instead of a
        // change: landing's cleanup abandons by commit id.
        let snapshot = self.overlaid_snapshot(path)?;
        let ids = change_ids
            .iter()
            .map(|id| {
                snapshot
                    .nodes
                    .iter()
                    .find(|node| node.id != *id && node.commit_id == *id)
                    .map_or_else(|| id.clone(), |node| node.id.clone())
            })
            .collect();
        self.mutate(path, crate::mock::MockMutation::AbandonMany { ids })
    }

    fn abandon_landed(
        &self,
        path: &Path,
        commits: &[String],
    ) -> Result<MutationOutcome, BackendError> {
        let snapshot = self.overlaid_snapshot(path)?;
        let named = |node: &crate::snapshot::GraphNode| {
            commits.iter().any(|id| node.id == *id || node.commit_id == *id)
        };
        let sitting: Vec<&str> = snapshot
            .nodes
            .iter()
            .filter(|node| !named(node))
            .filter(|node| {
                node.parents
                    .iter()
                    .any(|p| snapshot.nodes.iter().any(|parent| parent.id == *p && named(parent)))
            })
            .map(|node| node.id.as_str())
            .collect();
        if !sitting.is_empty() {
            return Err(BackendError::MutationFailed(format!(
                "{} sits on the landed commits and is not one of them",
                sitting.join(", ")
            )));
        }
        self.abandon_changes(path, commits)
    }

    fn squash_change(&self, path: &Path, change_id: &str) -> Result<MutationOutcome, BackendError> {
        self.mutate(
            path,
            crate::mock::MockMutation::Squash {
                id: change_id.to_owned(),
            },
        )
    }

    fn restore_file(
        &self,
        path: &Path,
        change_id: &str,
        file_path: &str,
        hunks: Option<&[SplitHunk]>,
    ) -> Result<MutationOutcome, BackendError> {
        self.mutate(
            path,
            crate::mock::MockMutation::RestoreFile {
                id: change_id.to_owned(),
                path: file_path.to_owned(),
                hunks: hunks.map(<[SplitHunk]>::to_vec),
            },
        )
    }

    fn duplicate_change(
        &self,
        path: &Path,
        change_id: &str,
    ) -> Result<MutationOutcome, BackendError> {
        self.mutate(
            path,
            crate::mock::MockMutation::Duplicate {
                id: change_id.to_owned(),
            },
        )
    }

    fn revert_change(&self, path: &Path, change_id: &str) -> Result<MutationOutcome, BackendError> {
        self.mutate(
            path,
            crate::mock::MockMutation::Revert {
                id: change_id.to_owned(),
            },
        )
    }

    fn absorb_change(&self, path: &Path, change_id: &str) -> Result<MutationOutcome, BackendError> {
        self.mutate(
            path,
            crate::mock::MockMutation::Absorb {
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

    fn rebase_onto_trunk(
        &self,
        path: &Path,
        _remote: &str,
        _trunk: &str,
        change_id: &str,
        _landed: &[String],
    ) -> Result<MutationOutcome, BackendError> {
        // The fabricated repository has one trunk bookmark and no remote
        // that moves, so its target is trunk. Its changes have one parent
        // each, so there is no other parent to keep.
        let destination = self
            .overlaid_snapshot(path)?
            .bookmarks
            .into_iter()
            .find(|b| b.is_trunk)
            .map(|b| b.target)
            .ok_or_else(|| {
                BackendError::MutationFailed(
                    "the repository has no trunk bookmark to rebase onto".to_owned(),
                )
            })?;
        self.rebase_change(path, change_id, &destination)
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

    fn delete_bookmark_at(
        &self,
        path: &Path,
        name: &str,
        commit: &str,
    ) -> Result<MutationOutcome, BackendError> {
        let snapshot = self.overlaid_snapshot(path)?;
        let name = name.trim();
        let at = snapshot
            .bookmarks
            .iter()
            .find(|b| b.name == name && b.is_local)
            .and_then(|b| snapshot.nodes.iter().find(|n| n.id == b.target))
            .map(|n| n.commit_id.clone())
            .ok_or_else(|| BackendError::BookmarkMissing(name.to_owned()))?;
        if commit.is_empty() || !(at.starts_with(commit) || commit.starts_with(&at)) {
            return Err(BackendError::MutationFailed(format!(
                "bookmark \u{201c}{name}\u{201d} no longer points at {commit}; it was left in place"
            )));
        }
        self.delete_bookmark(path, name)
    }

    fn track_bookmark(
        &self,
        path: &Path,
        name: &str,
        remote: &str,
    ) -> Result<MutationOutcome, BackendError> {
        self.mutate(
            path,
            crate::mock::MockMutation::TrackBookmark {
                name: name.trim().to_owned(),
                remote: remote.trim().to_owned(),
            },
        )
    }

    fn untrack_bookmark(
        &self,
        path: &Path,
        name: &str,
        remote: &str,
    ) -> Result<MutationOutcome, BackendError> {
        self.mutate(
            path,
            crate::mock::MockMutation::UntrackBookmark {
                name: name.trim().to_owned(),
                remote: remote.trim().to_owned(),
            },
        )
    }

    fn take_over_branch(
        &self,
        path: &Path,
        name: &str,
        remote: &str,
        number: u64,
        author: Option<&str>,
    ) -> Result<MutationOutcome, BackendError> {
        self.mutate(
            path,
            crate::mock::MockMutation::TakeOver {
                name: name.trim().to_owned(),
                remote: remote.trim().to_owned(),
                number,
                author: author.map(str::to_owned),
            },
        )
    }

    fn give_back_branch(
        &self,
        path: &Path,
        name: &str,
        remote: &str,
    ) -> Result<MutationOutcome, BackendError> {
        self.mutate(
            path,
            crate::mock::MockMutation::GiveBack {
                name: name.trim().to_owned(),
                remote: remote.trim().to_owned(),
            },
        )
    }

    fn delete_remote_bookmark(
        &self,
        path: &Path,
        name: &str,
        remote: &str,
    ) -> Result<MutationOutcome, BackendError> {
        self.mutate(
            path,
            crate::mock::MockMutation::DeleteRemoteBookmark {
                name: name.trim().to_owned(),
                remote: remote.trim().to_owned(),
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

    fn fetch_pr_head(
        &self,
        path: &Path,
        remote: &str,
        number: u64,
        bookmark: &str,
    ) -> Result<MutationOutcome, BackendError> {
        let snapshot = self.overlaid_snapshot(path)?;
        let remote = remote.trim();
        if !snapshot.git_remotes.iter().any(|r| r.name == remote) {
            return Err(BackendError::MutationFailed(format!(
                "there is no git remote named \u{201c}{remote}\u{201d}"
            )));
        }
        self.mutate(
            path,
            crate::mock::MockMutation::FetchPr {
                number,
                bookmark: bookmark.trim().to_owned(),
            },
        )
    }

    fn trunk_text_file(
        &self,
        path: &Path,
        _candidates: &[String],
    ) -> Result<Option<(String, String)>, BackendError> {
        // The fabricated trunk tree carries no repo docs — documented
        // approximation: mock submit plans never see a PR template.
        validate_repo_path(path)?;
        Ok(None)
    }

    fn lands_as_tip(
        &self,
        path: &Path,
        _remote: &str,
        _trunk: &str,
        _bottom: &str,
        _first_top: &str,
        _tip: &str,
    ) -> Result<TipCheck, BackendError> {
        // Documented approximation: the mock has no trees to merge, and
        // its trunk never moves under a stack.
        validate_repo_path(path)?;
        Ok(TipCheck::Same)
    }

    fn branches_resting_on(
        &self,
        path: &Path,
        _commits: &[String],
    ) -> Result<Vec<String>, BackendError> {
        // Documented approximation: the mock repository holds no remote
        // branch that is not also a bookmark in it.
        validate_repo_path(path)?;
        Ok(Vec::new())
    }

    fn landed_check(
        &self,
        path: &Path,
        _remote: &str,
        _trunk: &str,
        landing: &str,
        _head: &str,
        _bottom: &str,
        _top: &str,
    ) -> Result<LandedCheck, BackendError> {
        // Documented approximation: the fabricated remote never moves and
        // the mock has no trees, so it cannot receive a landing commit or
        // compare content. It takes the forge's word for a commit it has
        // never seen (the trunk commit a merge made), and knows better only
        // about its own: a pull request "merged" at one of the mock's
        // mutable commits did not reach trunk, which is what GitHub reports
        // when a branch's commits turn up inside its base branch.
        let snapshot = self.overlaid_snapshot(path)?;
        let mutable_here = !landing.is_empty()
            && snapshot.nodes.iter().any(|node| {
                node.kind != crate::snapshot::NodeKind::Immutable
                    && !node.commit_id.is_empty()
                    && (landing.starts_with(&node.commit_id)
                        || node.commit_id.starts_with(landing))
            });
        Ok(if mutable_here {
            LandedCheck::Elsewhere
        } else {
            LandedCheck::Landed
        })
    }

    fn search_history(&self, path: &Path, query: &str) -> Result<HistorySearch, BackendError> {
        // Documented approximation: fixture diffs carry only hunk lines,
        // not whole file sides, so occurrence counts come from the added
        // and removed lines. Context lines contribute equally to both
        // sides, so skipping them matches the pickaxe's "count changed"
        // rule for everything a fixture can express.
        let snapshot = self.overlaid_snapshot(path)?;
        let needle = query.trim().to_lowercase();
        let mut hits = Vec::new();
        let mut scanned = 0u32;
        if !needle.is_empty() {
            for node in &snapshot.nodes {
                scanned += 1;
                let Some(diff) = crate::mock::mock_change_diff(&node.id) else {
                    continue;
                };
                let mut files = Vec::new();
                for file in &diff.files {
                    let path_match = file.path.to_lowercase().contains(&needle);
                    let (mut added, mut removed) = (0u32, 0u32);
                    if let crate::snapshot::FileDiffContent::Text { hunks, .. } = &file.content {
                        for hunk in hunks {
                            for line in &hunk.lines {
                                let text = line
                                    .segments
                                    .iter()
                                    .map(|s| s.text.as_str())
                                    .collect::<String>()
                                    .to_lowercase();
                                let count = text.matches(needle.as_str()).count() as u32;
                                match line.kind {
                                    crate::snapshot::DiffLineKind::Added => added += count,
                                    crate::snapshot::DiffLineKind::Removed => removed += count,
                                    crate::snapshot::DiffLineKind::Context => {}
                                }
                            }
                        }
                    }
                    if path_match || added != removed {
                        files.push(crate::snapshot::SearchFileHit {
                            path: file.path.clone(),
                            added,
                            removed,
                            path_match,
                        });
                    }
                }
                if !files.is_empty() {
                    hits.push(crate::snapshot::SearchHit {
                        id: node.id.clone(),
                        change_id: node.change_id.clone(),
                        commit_id: node.commit_id.clone(),
                        description: node.description.clone(),
                        author: node.author.clone(),
                        timestamp: node.timestamp.clone(),
                        files,
                        more_files: 0,
                    });
                }
            }
        }
        Ok(HistorySearch {
            query: query.to_owned(),
            hits,
            scanned,
            truncated: false,
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
                side: None,
            },
        )
    }

    fn resolve_conflict_take(
        &self,
        path: &Path,
        change_id: &str,
        file_path: &str,
        side: ConflictSide,
    ) -> Result<MutationOutcome, BackendError> {
        self.mutate(
            path,
            crate::mock::MockMutation::Resolve {
                id: change_id.to_owned(),
                path: file_path.to_owned(),
                side: Some(side),
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

    /// The fabricated repository takes a teammate's branch over as the
    /// real one does: a tracked bookmark here and a record of whose it
    /// was, with the same refusals. Its one approximation is that the
    /// branch, which the fabricated graph does not draw, arrives as one
    /// change on top of the working copy's stack. Given back, it is as
    /// it was.
    #[test]
    fn mock_takes_a_teammates_branch_over_and_gives_it_back() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join(".jj")).unwrap();
        let backend = MockBackend::default();
        let before = backend.open(dir.path()).unwrap();
        assert!(before.taken_over.is_empty());
        assert!(!before.nodes.iter().any(|n| n.id == "yqosuwnv"));

        let outcome = backend
            .take_over_branch(dir.path(), "alice/palette-icons", "origin", 52, Some("alice"))
            .unwrap();
        assert_eq!(
            outcome.summary,
            "Took over \u{201c}alice/palette-icons\u{201d}: it is a bookmark here now, at \
             19b753c4, and nothing was pushed"
        );
        let after = backend.open(dir.path()).unwrap();
        assert_eq!(
            after.taken_over,
            [crate::snapshot::TakenOver {
                bookmark: "alice/palette-icons".into(),
                remote: "origin".into(),
                number: 52,
                author: Some("alice".into()),
            }]
        );
        let bookmark = after
            .bookmarks
            .iter()
            .find(|b| b.name == "alice/palette-icons")
            .unwrap();
        assert!(bookmark.is_local && bookmark.tracked);
        assert_eq!(bookmark.sync, crate::snapshot::SyncState::Synced);
        // Their change, on top of the working copy's stack, in a lane of
        // its own that runs down that stack.
        let theirs = after.nodes.iter().find(|n| n.id == "yqosuwnv").unwrap();
        assert_eq!(theirs.commit_id, "19b753c4");
        assert_eq!(theirs.author, "alice");
        assert_eq!(theirs.parents, ["lwnouzpy"]);
        assert_eq!(theirs.bookmarks, ["alice/palette-icons"]);
        let lane = after.workstreams.iter().find(|w| w.id == "ws-yqosuwnv").unwrap();
        assert_eq!(lane.node_ids, ["yqosuwnv", "lwnouzpy", "mvkortsq", "nzpwlxvr"]);
        assert!(backend.change_detail(dir.path(), "yqosuwnv").is_ok());
        assert_eq!(
            after.operations[0].description,
            "take over alice/palette-icons@origin (pull request #52)"
        );

        // Taken already; and a branch that is not on the remote.
        let err = backend
            .take_over_branch(dir.path(), "alice/palette-icons", "origin", 52, None)
            .unwrap_err();
        assert!(err.to_string().contains("is a bookmark here already"), "{err}");
        let err = backend
            .take_over_branch(dir.path(), "nowhere", "origin", 53, None)
            .unwrap_err();
        assert!(err.to_string().contains("there is no remote bookmark nowhere@origin"), "{err}");
        // Only what was taken over is given back.
        let err = backend
            .give_back_branch(dir.path(), "diff-model", "origin")
            .unwrap_err();
        assert!(err.to_string().contains("was not taken over here"), "{err}");

        backend
            .give_back_branch(dir.path(), "alice/palette-icons", "origin")
            .unwrap();
        let back = backend.open(dir.path()).unwrap();
        assert!(back.taken_over.is_empty());
        assert_eq!(back.nodes, before.nodes);
        assert_eq!(back.bookmarks, before.bookmarks);
        assert_eq!(back.workstreams, before.workstreams);
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
    fn mock_evolog_serves_fixture_chain_and_restores() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join(".jj")).unwrap();
        let backend = MockBackend::default();

        // The story change carries a fixture chain; others answer just
        // their current version.
        let evolog = backend.change_evolog(dir.path(), "lwnouzpy").unwrap();
        assert_eq!(evolog.id, "lwnouzpy");
        assert_eq!(evolog.entries.len(), 4);
        assert_eq!(evolog.entries[0].commit_id, "c5e208d4");
        assert_eq!(evolog.entries[1].description, "wip: virtualize diff rows");
        assert!(evolog.entries[0].op_id.is_some());
        assert!(!evolog.truncated);
        let single = backend.change_evolog(dir.path(), "mvkortsq").unwrap();
        assert_eq!(single.entries.len(), 1);
        let err = backend.change_evolog(dir.path(), "zzzzzzzz").unwrap_err();
        assert!(matches!(err, BackendError::ChangeMissing(_)));

        // Restoring an earlier version records an operation, stacks the
        // new version on the chain, and brings its description back.
        let outcome = backend
            .restore_change_version(dir.path(), "lwnouzpy", "b7f2c9d1")
            .unwrap();
        assert_eq!(outcome.summary, "Restored lwnouzpy to version b7f2c9d1");
        let op = outcome.operation_id.clone().unwrap();
        let snapshot = backend.open(dir.path()).unwrap();
        let node = snapshot.nodes.iter().find(|n| n.id == "lwnouzpy").unwrap();
        assert_eq!(node.description, "wip: virtualize diff rows");
        let evolog = backend.change_evolog(dir.path(), "lwnouzpy").unwrap();
        assert_eq!(evolog.entries.len(), 5);
        assert_eq!(evolog.entries[0].commit_id, node.commit_id);
        assert_eq!(
            evolog.entries[0].op_description.as_deref(),
            Some("restore commit c5e208d4 to version b7f2c9d1")
        );
        assert!(snapshot.operations[0]
            .description
            .starts_with("restore commit"));

        // Restoring to the (new) current version records nothing; foreign
        // versions and immutable changes refuse.
        let noop = backend
            .restore_change_version(dir.path(), "lwnouzpy", &evolog.entries[0].commit_id)
            .unwrap();
        assert!(noop.operation_id.is_none());
        let err = backend
            .restore_change_version(dir.path(), "lwnouzpy", "feedface")
            .unwrap_err();
        assert!(err.to_string().contains("not an earlier version"), "got {err}");
        let err = backend
            .restore_change_version(dir.path(), "uvkmrtpz", "e7c41a9f")
            .unwrap_err();
        assert!(matches!(err, BackendError::ImmutableChange(_)));

        // The op log undoes the restore like any other mutation.
        backend.revert_operation(dir.path(), &op).unwrap();
        let snapshot = backend.open(dir.path()).unwrap();
        let node = snapshot.nodes.iter().find(|n| n.id == "lwnouzpy").unwrap();
        assert_eq!(node.description, "feat: virtualize multi-file diff rendering");
        let evolog = backend.change_evolog(dir.path(), "lwnouzpy").unwrap();
        assert_eq!(evolog.entries.len(), 4);
    }

    #[test]
    fn mock_annotate_serves_the_fixture_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join(".jj")).unwrap();
        let backend = MockBackend::default();

        let annotation = backend
            .file_annotate(
                dir.path(),
                "lwnouzpy",
                "src/lib/components/diff/DiffViewport.svelte",
            )
            .unwrap();
        assert_eq!(annotation.id, "lwnouzpy");
        assert_eq!(annotation.lines.len(), 20);
        assert_eq!(annotation.origins.len(), 4);
        // Multi-author fixture: the stack is lauf's, trunk is maintainers'.
        assert!(annotation.origins.iter().any(|o| o.author == "maintainers"));
        assert!(annotation.origins.iter().any(|o| o.author == "lauf"));
        // Every line indexes a real origin.
        assert!(annotation
            .lines
            .iter()
            .all(|l| (l.origin as usize) < annotation.origins.len()));
        assert!(!annotation.truncated);

        let err = backend
            .file_annotate(dir.path(), "lwnouzpy", "src/other.ts")
            .unwrap_err();
        assert!(err.to_string().contains("does not exist"), "got {err}");
        let err = backend
            .file_annotate(dir.path(), "zzzzzzzz", "src/other.ts")
            .unwrap_err();
        assert!(matches!(err, BackendError::ChangeMissing(_)));
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
    fn mock_take_side_settles_the_item_like_resolve() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join(".jj")).unwrap();
        let backend = MockBackend::default();
        let path = "src/lib/components/conflicts/ConflictList.svelte";

        let err = backend
            .resolve_conflict_take(
                dir.path(),
                "pmwzqkvt",
                "nope.txt",
                crate::snapshot::ConflictSide::Ours,
            )
            .unwrap_err();
        assert!(matches!(err, BackendError::MutationFailed(_)), "got {err:?}");

        let outcome = backend
            .resolve_conflict_take(
                dir.path(),
                "pmwzqkvt",
                path,
                crate::snapshot::ConflictSide::Theirs,
            )
            .unwrap();
        assert!(outcome.operation_id.is_some());
        assert_eq!(
            outcome.summary,
            format!("Resolved {path} in pmwzqkvt by taking theirs")
        );
        let after = backend.open(dir.path()).unwrap();
        assert!(!after
            .conflicts
            .iter()
            .any(|c| c.node_id.as_deref() == Some("pmwzqkvt")));
        assert!(after.operations[0].description.starts_with("Resolve conflicts in commit"));
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
                lines: None,
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

    #[test]
    fn compare_diff_answers_fixture_version_transitions() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join(".jj")).unwrap();
        let backend = MockBackend::default();
        backend.open(dir.path()).unwrap();

        // The evolution surface asks for hidden version commit ids the
        // graph carries no nodes for; each fixture step answers.
        let step = backend
            .compare_diff(dir.path(), "9e12ff30", "b7f2c9d1")
            .unwrap();
        assert_eq!(step.from.as_deref(), Some("9e12ff30"));
        assert!(!step.files.is_empty());

        // The describe step changed no files.
        let describe = backend
            .compare_diff(dir.path(), "b7f2c9d1", "c5e208d4")
            .unwrap();
        assert!(describe.files.is_empty());

        // A wider span unions its steps: the file added in the first step
        // and touched in the second stays a single add.
        let span = backend
            .compare_diff(dir.path(), "4a81d0f2", "c5e208d4")
            .unwrap();
        let list = span
            .files
            .iter()
            .find(|f| f.path.ends_with("VirtualDiffList.svelte"))
            .expect("spanned file present");
        assert_eq!(list.status, crate::snapshot::FileStatus::Added);
        let mut paths: Vec<&str> = span.files.iter().map(|f| f.path.as_str()).collect();
        paths.dedup();
        assert_eq!(paths.len(), span.files.len(), "one entry per path");
    }
}
