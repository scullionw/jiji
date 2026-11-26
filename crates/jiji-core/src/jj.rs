//! jj-lib-backed implementation of `RepoBackend`.
//!
//! Every `open` loads the workspace fresh, reads the repo at the current
//! operation head, and denormalizes it into the UI snapshot DTOs. All jj-lib
//! types stay inside this module; hosts consume snapshot and local workflow
//! DTOs.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::Path;
use std::rc::Rc;
use std::sync::Arc;

use base64::Engine as _;
use futures::StreamExt as _;
use jj_lib::backend::{CommitId, CopyId, MergedTreeValue, MergedTreeValueExt as _, Timestamp, TreeValue};
use jj_lib::commit::Commit;
use jj_lib::dag_walk_async;
use jj_lib::conflicts::{
    choose_materialized_conflict_marker_len, materialize_merge_result_to_bytes,
    materialized_diff_stream, try_materialize_file_conflict_value, update_from_content,
    ConflictMarkerStyle, ConflictMaterializeOptions, MaterializedFileConflictValue,
    MaterializedTreeValue, MIN_CONFLICT_MARKER_LEN,
};
use jj_lib::copies::{CopiesTreeDiffEntryPath, CopyOperation, CopyRecords};
use jj_lib::diff::{ContentDiff, DiffHunkKind};
use jj_lib::diff_presentation::unified::{
    git_diff_part, unified_diff_hunks, DiffLineType, GitDiffPart,
};
use jj_lib::diff_presentation::LineCompareMode;
use jj_lib::evolution;
use jj_lib::fileset::{self, FilesetAliasesMap, FilesetDiagnostics, FilesetParseContext};
use jj_lib::config::ConfigGetError;
use jj_lib::git::{
    self, expand_fetch_refspecs, load_default_fetch_bookmarks, GitFetch, GitFetchRefExpression,
    GitProgress, GitSidebandLineTerminator, GitSubprocessCallback, REMOTE_NAME_FOR_LOCAL_GIT_REPO,
};
use jj_lib::gitignore::{GitIgnoreError, GitIgnoreFile};
use jj_lib::hex_util;
use jj_lib::matchers::{EverythingMatcher, FilesMatcher, Matcher, NothingMatcher};
use jj_lib::merge::{Diff as MergeDiff, Merge};
use jj_lib::merged_tree::MergedTree;
use jj_lib::merged_tree_builder::MergedTreeBuilder;
use jj_lib::object_id::{HexPrefix, ObjectId, PrefixResolution};
use jj_lib::op_store::{OpStoreError, OperationId, RefTarget, RemoteRefState, ViewId};
use jj_lib::op_walk;
use jj_lib::operation::Operation;
use jj_lib::ref_name::{RefName, RemoteName, RemoteRefSymbol};
use jj_lib::refs::{classify_ref_push_action, LocalAndRemoteRef, RefPushAction};
use jj_lib::repo::{ReadonlyRepo, Repo};
use jj_lib::repo_path::{RepoPath, RepoPathBuf};
use jj_lib::ui_path::RepoPathUiConverter;
use jj_lib::revset::{
    self, ResolvedRevsetExpression, RevsetAliasesMap, RevsetDiagnostics, RevsetExpression,
    RevsetExtensions, RevsetParseContext, RevsetWorkspaceContext, SymbolResolver,
};
use jj_lib::absorb::{absorb_hunks, split_hunks_to_trees, AbsorbSource};
use jj_lib::annotate::FileAnnotator;
use jj_lib::rewrite::{
    duplicate_commits_onto_parents, move_commits, restore_tree, squash_commits,
    CommitWithSelection, MoveCommitsLocation, MoveCommitsTarget, RebaseOptions, RebasedCommit,
    RewriteRefsOptions,
};
use jj_lib::settings::{HumanByteSize, UserSettings};
use jj_lib::store::Store;
use jj_lib::str_util::StringExpression;
use jj_lib::transaction::Transaction;
use jj_lib::working_copy::{SnapshotOptions, WorkingCopyFreshness};
use jj_lib::tree_merge::MergeOptions;
use jj_lib::view::View;
use jj_lib::default_backend_factories::{default_backend_factories, default_working_copy_factories};
use jj_lib::workspace::Workspace;

use crate::backend::{validate_repo_path, BackendError, RepoBackend};
use crate::settings::{load_settings, load_user_settings, revset_aliases, UserConfigSource};
use crate::snapshot::*;

pub mod hidden;

/// Synchronous iteration over a jj-lib stream. Revsets, operation walks and
/// evolog walks are streams since jj-lib 0.40; every backend call already
/// runs on its own blocking thread, so they are drained in place.
fn blocking_iter<S: futures::Stream>(stream: S) -> impl Iterator<Item = S::Item> {
    futures::executor::block_on_stream(Box::pin(stream))
}

trait RevsetBlockingExt<'a> {
    /// The revset's commit ids, children before parents.
    fn iter_blocking(
        &self,
    ) -> impl Iterator<Item = Result<CommitId, jj_lib::revset::RevsetEvaluationError>> + 'a;
}

impl<'a> RevsetBlockingExt<'a> for Box<dyn revset::Revset + 'a> {
    fn iter_blocking(
        &self,
    ) -> impl Iterator<Item = Result<CommitId, jj_lib::revset::RevsetEvaluationError>> + 'a {
        futures::executor::block_on_stream(self.stream())
    }
}

/// Safety valve for pathological repos: mutable history this deep is almost
/// certainly a misconfigured trunk, and a graph that long is unusable anyway.
const MAX_MUTABLE_NODES: usize = 500;
/// Commits a view lens (Team, All, a custom revset, …) may add to a
/// snapshot. Past this the snapshot reports itself truncated; the graph is
/// unreadable at this depth anyway and the UI stays scrollable.
const MAX_VIEW_NODES: usize = 1000;
/// Operation log entries included in a snapshot (newest first).
const MAX_OPERATIONS: usize = 50;
/// Effect chips per operation; past this only a count is reported.
const MAX_OP_EFFECTS: usize = 6;
/// `behind_trunk` is informational; stop counting past this.
const MAX_BEHIND_COUNT: usize = 999;
/// Changed-file lists are for inspection, not bulk processing; cap them so a
/// vendored-dependency commit cannot flood the UI.
const MAX_CHANGED_FILES: usize = 1000;
/// `trunk_text_file` reads repo docs (a PR template) destined for PR
/// bodies; anything bigger than this is not a document worth inlining.
const MAX_TRUNK_TEXT_FILE_BYTES: u64 = 100 * 1024;

/// How many paths a failed landing check names; the rest are counted.
const MAX_LANDED_CHECK_PATHS: usize = 12;
/// Per-side file size limit for content diffs; larger files render as a
/// "too large" placeholder instead of hunks.
const MAX_DIFF_FILE_BYTES: usize = 1 << 20;
/// Total rendered diff lines per change across all files; past this the
/// remaining files keep their list entry but skip content ("omitted").
const MAX_DIFF_TOTAL_LINES: isize = 20_000;
/// Per-side cap for inlining image bytes into a diff response; a larger
/// image still gets an entry with its metadata, just no preview.
pub const MAX_IMAGE_INLINE_BYTES: usize = 8 << 20;
/// Total inlined image bytes per change across all files — the image
/// counterpart of `MAX_DIFF_TOTAL_LINES`, keeping one response (and any
/// cache holding it) bounded when a change carries many images. Later
/// images keep their metadata and drop the preview.
pub const MAX_IMAGE_TOTAL_BYTES: usize = 16 << 20;
/// Unified-diff context lines, matching jj's default.
const DIFF_CONTEXT_LINES: usize = 3;
/// Linking bases through elided history costs ancestry checks quadratic in
/// the base count; past this many bases the spine is unreadable anyway.
const MAX_LINKED_BASES: usize = 64;
/// History search walks at most this many commits before reporting itself
/// truncated — interactive latency beats exhaustiveness, and the caps are
/// visible in the result so the surface can say how far it got.
const MAX_SEARCH_COMMITS: usize = 1000;
/// Matching changes per search; a query this common needs narrowing, not
/// more scrolling.
const MAX_SEARCH_HITS: usize = 100;
/// Matching files reported per hit; past this only a count.
const MAX_SEARCH_FILES: usize = 8;
/// Wall-clock budget per search. Materializing file contents dominates,
/// and a huge repo should degrade to a truncated answer, not a hang.
const SEARCH_TIME_BUDGET: std::time::Duration = std::time::Duration::from_millis(3500);

/// Versions per evolog answer. A chain deeper than this is pathological
/// churn; the walk stops and reports itself truncated. Version-restore
/// verifies membership with the same bound, so everything the panel can
/// show stays restorable.
const MAX_EVOLOG_ENTRIES: usize = 100;

/// Lines per annotate answer; a longer file reports itself truncated. The
/// attribution walk still runs over the whole file (jj-lib computes it in
/// one pass) — only the rendered payload is bounded.
const MAX_ANNOTATE_LINES: usize = 10_000;

const TRUNK_CANDIDATES: [&str; 3] = ["main", "master", "trunk"];

pub struct JjBackend {
    /// Where user-level jj config comes from. The default discovers it like
    /// the jj CLI; tests pin it so they never read the developer's machine.
    user_config: UserConfigSource,
}

impl Default for JjBackend {
    fn default() -> Self {
        Self {
            user_config: UserConfigSource::Discover,
        }
    }
}

impl JjBackend {
    pub fn with_user_config(user_config: UserConfigSource) -> Self {
        Self { user_config }
    }

    fn load_workspace(&self, path: &Path) -> Result<Workspace, BackendError> {
        validate_repo_path(path)?;
        let settings = load_settings(path, &self.user_config)?;
        Workspace::load(
            &settings,
            path,
            &default_backend_factories(),
            &default_working_copy_factories(),
        )
        .map_err(|err| BackendError::OpenFailed(err.to_string()))
    }

    /// Loads the repo for reading. Reads never record an operation, so they
    /// take no write lock and stay responsive while a fetch runs.
    fn load_repo_at_head(&self, path: &Path) -> Result<(Workspace, Arc<ReadonlyRepo>), BackendError> {
        let workspace = self.load_workspace(path)?;
        let repo = load_head(&workspace)?;
        Ok((workspace, repo))
    }

    /// Loads the repo for a call that may record operations (every sync,
    /// mutation, fetch and push), holding the repo's write lock from before
    /// the head is read until the guard drops. See `RepoWriteLock`.
    fn load_repo_for_write(
        &self,
        path: &Path,
    ) -> Result<(RepoWriteGuard, Workspace, Arc<ReadonlyRepo>), BackendError> {
        let workspace = self.load_workspace(path)?;
        let guard = RepoWriteLock::acquire(workspace.repo_path());
        let repo = load_head(&workspace)?;
        Ok((guard, workspace, repo))
    }

    /// Shared body of `delete_bookmark` and `delete_bookmark_at`. With
    /// `expected`, the bookmark is deleted only while it points at exactly
    /// that commit; the test runs under the write lock, after the working
    /// copy is snapshotted, so nothing can move it in between.
    fn delete_bookmark_if(
        &self,
        path: &Path,
        name: &str,
        expected: Option<&str>,
    ) -> Result<MutationOutcome, BackendError> {
        let (_write, mut workspace, repo) = self.load_repo_for_write(path)?;
        let repo = sync_workspace(&mut workspace, repo)?;
        let name = name.trim();
        let target = repo.view().get_local_bookmark(RefName::new(name)).clone();
        if target.is_absent() {
            return Err(BackendError::BookmarkMissing(name.to_owned()));
        }
        if let Some(expected) = expected {
            // `expected` has to name one commit, and the bookmark has to
            // point at that commit and no other.
            let still_there = match (resolve_commit_id(&repo, expected)?, target.as_normal()) {
                (Some(expected), Some(at)) => expected == *at,
                _ => false,
            };
            if !still_there {
                return Err(BackendError::MutationFailed(format!(
                    "bookmark \u{201c}{name}\u{201d} no longer points at {expected}; it was \
                     left in place"
                )));
            }
        }
        let target_change = target
            .added_ids()
            .next()
            .map(|id| display_id_of(repo.as_ref(), id))
            .transpose()?;
        let mut tx = repo.start_transaction();
        tx.repo_mut()
            .set_local_bookmark_target(RefName::new(name), RefTarget::absent());
        let new_repo = finish_mutation(
            &mut workspace,
            &repo,
            tx,
            format!("delete bookmark {name}"),
        )?;
        Ok(MutationOutcome {
            operation_id: Some(short_operation_id(new_repo.op_id())),
            summary: format!("Deleted {name}"),
            target_change,
        })
    }

    /// Shared body of `abandon_changes` and `abandon_landed`. With
    /// `leaves_only`, nothing is abandoned while a change that is not in
    /// the set sits on it. The look runs under the write lock, after the
    /// working copy is snapshotted, so a change started a moment ago
    /// counts.
    fn abandon_set(
        &self,
        path: &Path,
        change_ids: &[String],
        leaves_only: bool,
    ) -> Result<MutationOutcome, BackendError> {
        // Curated deviation: the CLI prints "No revisions to abandon." and
        // exits cleanly; an empty list from a confirmed plan is a stale
        // plan, so it refuses instead.
        if change_ids.is_empty() {
            return Err(BackendError::MutationFailed(
                "there are no changes to abandon".to_owned(),
            ));
        }
        let (_write, mut workspace, repo) = self.load_repo_for_write(path)?;
        let repo = sync_workspace(&mut workspace, repo)?;
        let mut commits: Vec<Commit> = Vec::new();
        let mut display_ids: Vec<String> = Vec::new();
        for change_id in change_ids {
            let commit = resolve_change_commit_for_mutation(&repo, change_id)?;
            if leaves_only {
                // A landed commit that someone else's branch still has
                // in its history is immutable to jj and is swept all the
                // same: it landed, and hiding it here rewrites nothing.
                check_not_settled(&workspace, repo.as_ref(), &commit, change_id)?;
            } else {
                check_mutable(&workspace, repo.as_ref(), &commit, change_id)?;
            }
            // Duplicates collapse like the CLI's revset union.
            if !commits.iter().any(|c| c.id() == commit.id()) {
                commits.push(commit);
                display_ids.push(change_id.clone());
            }
        }
        // The CLI resolves its arguments as one revset, which iterates
        // newest-first, and quotes whichever commit comes first in that
        // order in the op description — match it by evaluating the same
        // set (probed empirically: input order does not matter).
        let expr = RevsetExpression::commits(commits.iter().map(|c| c.id().clone()).collect());
        if leaves_only {
            // What sits on the set, among what is visible here. Asked by
            // itself: taken together with `immutable_heads()`, which can
            // name commits hidden here, the hidden would count too.
            let outside = collect_revset(
                expr.children().minus(&expr),
                repo.as_ref(),
                MAX_MUTABLE_NODES,
            )?;
            // Someone else's commits sitting on the landed ones: a pull
            // request that was based on the landed branch and has not
            // been rebased yet. Abandoning under them would rewrite them.
            // The landed commits stay, as part of that branch's history,
            // and that is not a failure of the sweep. Someone else's are
            // the commits of a branch that is no bookmark here and under
            // no bookmark of the user's; anything else that sits there
            // (a change of the user's that such a branch holds, a tagged
            // commit) stops the sweep below and is named.
            let theirs = collect_revset(
                RevsetExpression::commits(outside.clone())
                    .intersection(&someone_elses_expression(&workspace, repo.as_ref())?),
                repo.as_ref(),
                MAX_MUTABLE_NODES,
            )?;
            let theirs_sit = !theirs.is_empty();
            let mut sitting: Vec<String> = Vec::new();
            for commit_id in outside.iter().filter(|id| !theirs.contains(id)) {
                sitting.push(display_id_of(repo.as_ref(), commit_id)?);
            }
            if sitting.is_empty() && theirs_sit {
                let set: Vec<CommitId> = commits.iter().map(|c| c.id().clone()).collect();
                let resting = branches_resting_on(repo.as_ref(), &set)?;
                return Ok(MutationOutcome {
                    operation_id: None,
                    summary: format!(
                        "{}{} in place: {} still {} on {}",
                        crate::backend::LANDED_LEFT_IN_PLACE,
                        display_ids.join(", "),
                        if resting.is_empty() {
                            "a branch that is not a bookmark here".to_owned()
                        } else {
                            name_list(&resting)
                        },
                        if resting.len() > 1 { "build" } else { "builds" },
                        if display_ids.len() == 1 { "it" } else { "them" },
                    ),
                    target_change: None,
                });
            }
            if !sitting.is_empty() {
                return Err(BackendError::MutationFailed(format!(
                    "{} {} on the landed commits and {} not one of them: abandoning them \
                     would move it onto what was below them",
                    sitting.join(", "),
                    if sitting.len() == 1 { "sits" } else { "sit" },
                    if sitting.len() == 1 { "is" } else { "are" },
                )));
            }
        }
        let revset = expr.evaluate(repo.as_ref()).map_err(snapshot_err)?;
        let mut ordered: Vec<Commit> = Vec::new();
        for commit_id in revset.iter_blocking() {
            let commit_id = commit_id.map_err(snapshot_err)?;
            let commit = repo.store().get_commit(&commit_id).map_err(snapshot_err)?;
            ordered.push(commit);
        }
        let newest = ordered.first().expect("the set is non-empty");
        let oldest = ordered.last().expect("the set is non-empty");
        // The selection ends up where the graph collapses to: the first
        // parent of the oldest abandoned change (outside the set by
        // topological order).
        let parent = repo
            .store()
            .get_commit(&oldest.parent_ids()[0])
            .map_err(snapshot_err)?;
        let parent_display = display_id(repo.as_ref(), &parent);

        let description = if ordered.len() == 1 {
            format!("abandon commit {}", newest.id().hex())
        } else {
            format!(
                "abandon commit {} and {} more",
                newest.id().hex(),
                ordered.len() - 1
            )
        };
        let mut tx = repo.start_transaction();
        for commit in &ordered {
            tx.repo_mut().record_abandoned_commit(commit);
        }
        // Same semantics as the single abandon: bookmarks on abandoned
        // changes are deleted, an abandoned working copy respawns inside
        // the rebase, and chains inside the set reparent transitively.
        let options = RebaseOptions {
            rewrite_refs: RewriteRefsOptions {
                delete_abandoned_bookmarks: true,
            },
            ..Default::default()
        };
        pollster::block_on(tx.repo_mut().rebase_descendants_with_options(&RevsetExpression::none(), &options, |_, _| {}))
            .map_err(mutation_err)?;
        let new_repo = finish_mutation(&mut workspace, &repo, tx, description)?;
        Ok(MutationOutcome {
            operation_id: Some(short_operation_id(new_repo.op_id())),
            summary: format!("Abandoned {}", display_ids.join(", ")),
            target_change: Some(parent_display),
        })
    }

    /// Shared body of `rebase_change` (`jj rebase -s`) and `move_change`
    /// (`jj rebase -r`): both are one `move_commits` call with a different
    /// target shape, and jj-lib works out which commits actually need
    /// rewriting (commits already in place are skipped, exactly like the
    /// CLI's "already in place" handling).
    fn rebase_onto(
        &self,
        path: &Path,
        change_id: &str,
        destination_id: &str,
        scope: RebaseScope,
    ) -> Result<MutationOutcome, BackendError> {
        let (_write, mut workspace, repo) = self.load_repo_for_write(path)?;
        let repo = sync_workspace(&mut workspace, repo)?;
        let commit = resolve_change_commit_for_mutation(&repo, change_id)?;
        check_mutable(&workspace, repo.as_ref(), &commit, change_id)?;
        // No immutability gate on the destination: it is only pointed at,
        // never rewritten — rebasing onto trunk is the canonical case.
        let destination = resolve_change_commit_for_mutation(&repo, destination_id)?;
        if destination.id() == commit.id() {
            return Err(BackendError::MutationFailed(format!(
                "cannot rebase {change_id} onto itself"
            )));
        }
        // A descendant destination is a cycle when descendants come along;
        // moving a lone change onto its descendant is fine (that is how two
        // adjacent changes swap order).
        if scope == RebaseScope::WithDescendants
            && is_ancestor(repo.as_ref(), commit.id(), destination.id())
        {
            return Err(BackendError::MutationFailed(format!(
                "cannot rebase {change_id} onto its own descendant {destination_id}"
            )));
        }

        let (target, op_description) = match scope {
            RebaseScope::WithDescendants => (
                MoveCommitsTarget::Roots(vec![commit.id().clone()]),
                format!("rebase commit {} and descendants", commit.id().hex()),
            ),
            RebaseScope::Alone => (
                MoveCommitsTarget::Commits(vec![commit.id().clone()]),
                format!("rebase commit {}", commit.id().hex()),
            ),
        };
        let mut tx = repo.start_transaction();
        let stats = pollster::block_on(move_commits(
            tx.repo_mut(),
            &MoveCommitsLocation {
                new_parent_ids: vec![destination.id().clone()],
                new_child_ids: vec![],
                target,
            },
            &RebaseOptions::default(),
        ))
        .map_err(mutation_err)?;

        if stats.num_rebased_targets == 0 && stats.num_rebased_descendants == 0 {
            // Everything was already in place; like the CLI, record nothing.
            return Ok(MutationOutcome {
                operation_id: None,
                summary: format!("{change_id} is already on {destination_id}"),
                target_change: Some(change_id.to_owned()),
            });
        }
        // The change follows its rewritten commit: for a divergent change
        // the node id is the commit id, which the rebase just replaced.
        let target_change = match stats.rebased_commits.get(commit.id()) {
            Some(jj_lib::rewrite::RebasedCommit::Rewritten(new_commit)) => {
                Some(new_commit.clone())
            }
            _ => None,
        };
        let new_repo = finish_mutation(&mut workspace, &repo, tx, op_description)?;
        let target_change = match target_change {
            Some(new_commit) => display_id(new_repo.as_ref(), &new_commit),
            None => change_id.to_owned(),
        };

        // In `Roots` scope every moved commit counts as a target, so the
        // descendant count is whatever moved beyond the change itself. In
        // `Alone` scope a skipped target with rebased descendants means the
        // change already sat on the destination and only its descendants
        // were reparented off it — say that instead of claiming a move.
        let summary = match scope {
            RebaseScope::WithDescendants => {
                let descendants = stats.num_rebased_targets.saturating_sub(1);
                if descendants == 0 {
                    format!("Rebased {change_id} onto {destination_id}")
                } else {
                    format!(
                        "Rebased {change_id} and {descendants} descendant{} onto {destination_id}",
                        if descendants == 1 { "" } else { "s" }
                    )
                }
            }
            RebaseScope::Alone if stats.num_rebased_targets == 0 => {
                let n = stats.num_rebased_descendants;
                format!(
                    "Moved {n} descendant{} of {change_id} onto {destination_id}",
                    if n == 1 { "" } else { "s" }
                )
            }
            RebaseScope::Alone => format!("Moved {change_id} onto {destination_id}"),
        };
        Ok(MutationOutcome {
            operation_id: Some(short_operation_id(new_repo.op_id())),
            summary,
            target_change: Some(target_change),
        })
    }
}

fn load_head(workspace: &Workspace) -> Result<Arc<ReadonlyRepo>, BackendError> {
    pollster::block_on(workspace.repo_loader().load_at_head())
        .map_err(|err| BackendError::OpenFailed(err.to_string()))
}

/// One jj writer per repo at a time within this process. jj itself is
/// lock-free: two transactions started from the same operation both commit,
/// and the next load merges the divergent operations. That merge is not
/// always benign. A watcher refresh that imports git refs while a fetch is
/// still rewriting them sees a torn state (`--prune` has deleted a merged
/// PR's branch, trunk has not moved yet), abandons the PR's commits, and
/// the merge with the fetch's operation then rebases every trunk commit
/// built on them — local trunk diverges from its remote. Jiji runs its own
/// work concurrently (the upstream fetch on open, the watcher, auto-land,
/// user actions), so it serializes that work instead: the waiting writer
/// loads the head only once the previous one committed.
///
/// Keyed by the canonical `.jj/repo` directory, which every workspace of a
/// repo shares along with its operation log. Process-wide, so every
/// `JjBackend` instance agrees. Another process (the jj CLI) is not covered;
/// that is jj's own concurrency model.
struct RepoWriteLock;

type RepoWriteGuard = std::sync::MutexGuard<'static, ()>;

impl RepoWriteLock {
    fn acquire(repo_path: &Path) -> RepoWriteGuard {
        static LOCKS: std::sync::OnceLock<
            std::sync::Mutex<HashMap<std::path::PathBuf, &'static std::sync::Mutex<()>>>,
        > = std::sync::OnceLock::new();
        let key = repo_path
            .canonicalize()
            .unwrap_or_else(|_| repo_path.to_owned());
        let lock: &'static std::sync::Mutex<()> = {
            let mut locks = LOCKS
                .get_or_init(Default::default)
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            // One small mutex per repo ever opened, alive for the process.
            locks
                .entry(key)
                .or_insert_with(|| Box::leak(Box::new(std::sync::Mutex::new(()))))
        };
        // The lock guards no data, so a writer that panicked leaves nothing
        // inconsistent behind; jj's own transaction was never committed.
        lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// Whether a rebase brings the change's descendants along (`jj rebase -s`)
/// or moves the change alone (`jj rebase -r`).
#[derive(Clone, Copy, PartialEq, Eq)]
enum RebaseScope {
    WithDescendants,
    Alone,
}

impl RepoBackend for JjBackend {
    fn open(&self, path: &Path) -> Result<RepoSnapshot, BackendError> {
        self.open_with(path, &SnapshotView::default())
    }

    fn open_with(&self, path: &Path, view: &SnapshotView) -> Result<RepoSnapshot, BackendError> {
        let (workspace, repo) = self.load_repo_at_head(path)?;
        build_snapshot(&workspace, &repo, view)
    }

    fn refresh(&self, path: &Path) -> Result<RepoSnapshot, BackendError> {
        self.refresh_with(path, &SnapshotView::default())
    }

    fn init_colocated(&self, root: &Path) -> Result<(), BackendError> {
        let display = root.display().to_string();
        if !root.is_dir() {
            return Err(BackendError::NotADirectory(display));
        }
        if root.join(".jj").exists() {
            return Err(BackendError::MutationFailed(format!(
                "{display} already has a .jj folder"
            )));
        }
        let git_dir = root.join(".git");
        if !git_dir.is_dir() {
            return Err(BackendError::MutationFailed(format!(
                "{display} is not the root of a Git repository (linked worktrees and \
                 submodules cannot be colocated)"
            )));
        }
        // The CLI's `jj git init --colocate` in an existing Git repo: an
        // external-git workspace whose Git repo is the colocated `.git`.
        let settings = load_user_settings(&self.user_config)?;
        let (workspace, repo) =
            pollster::block_on(Workspace::init_external_git(&settings, root, &git_dir))
                .map_err(|err| BackendError::OpenFailed(err.to_string()))?;
        // Git must not see jj's own files.
        std::fs::write(root.join(".jj").join(".gitignore"), "/*\n").map_err(|err| {
            BackendError::MutationFailed(format!("could not write .jj/.gitignore: {err}"))
        })?;
        // Import every branch and tag first, so all reachable commits are
        // indexed in chronological order — without abandoning anything or
        // recording synthetic predecessors for a whole history, like the CLI.
        let options = git::GitImportOptions {
            abandon_unreachable_commits: false,
            record_synthetic_predecessors: false,
            ..git_import_options(workspace.settings())?
        };
        let mut tx = repo.start_transaction();
        pollster::block_on(git::import_refs(tx.repo_mut(), &options)).map_err(mutation_err)?;
        if tx.repo().has_changes() {
            // Auto-tracked remote bookmarks can create local ones; a
            // colocated Git repo sees them as branches right away.
            git::export_refs(tx.repo_mut()).map_err(mutation_err)?;
            pollster::block_on(tx.commit("import git refs")).map_err(mutation_err)?;
        }
        // One deviation: the CLI also writes a repo-level `trunk()` alias
        // for the remote's default branch. Jiji's default `trunk()` already
        // covers main/master/trunk, and a repo-scoped config file lives in
        // jj's own user config directory, which Jiji only reads.
        Ok(())
    }

    fn refresh_with(&self, path: &Path, view: &SnapshotView) -> Result<RepoSnapshot, BackendError> {
        let (write, mut workspace, repo) = self.load_repo_for_write(path)?;
        // A stale workspace cannot sync (mutations keep refusing it), but
        // it must still open read-only — a viewer that errors out is worse
        // than a snapshot that lags the disk. `update_stale_workspace` is
        // the recovery.
        let repo = match sync_workspace(&mut workspace, repo.clone()) {
            Ok(repo) => repo,
            Err(BackendError::StaleWorkspace(reason)) => {
                tracing::warn!(reason, "workspace is stale; serving read-only snapshot");
                repo
            }
            Err(err) => return Err(err),
        };
        // Building reads only the repo already loaded; other writers need
        // not wait for it.
        drop(write);
        build_snapshot(&workspace, &repo, view)
    }

    fn watch(
        &self,
        path: &Path,
        on_change: Box<dyn Fn() + Send + 'static>,
    ) -> Result<Option<crate::watch::RepoWatcher>, BackendError> {
        // Loading the workspace resolves the real `.jj/repo` location (it
        // lives outside the root in non-default workspaces) and gives the
        // store for the same base gitignores snapshots use.
        let (workspace, repo) = self.load_repo_at_head(path)?;
        let base_ignores = base_ignores(repo.store()).map_err(snapshot_err)?;
        crate::watch::RepoWatcher::start(
            workspace.workspace_root(),
            workspace.repo_path(),
            base_ignores,
            on_change,
        )
        .map(Some)
    }

    fn change_detail(&self, path: &Path, change_id: &str) -> Result<ChangeDetail, BackendError> {
        let (_workspace, repo) = self.load_repo_at_head(path)?;
        build_change_detail(&repo, change_id)
    }

    fn change_diff(&self, path: &Path, change_id: &str) -> Result<ChangeDiff, BackendError> {
        let (_workspace, repo) = self.load_repo_at_head(path)?;
        build_change_diff(&repo, change_id)
    }

    fn compare_diff(
        &self,
        path: &Path,
        from_change_id: &str,
        to_change_id: &str,
    ) -> Result<ChangeDiff, BackendError> {
        let (_workspace, repo) = self.load_repo_at_head(path)?;
        build_compare_diff(&repo, from_change_id, to_change_id)
    }

    fn search_history(&self, path: &Path, query: &str) -> Result<HistorySearch, BackendError> {
        let (_workspace, repo) = self.load_repo_at_head(path)?;
        build_history_search(&repo, query)
    }

    fn change_evolog(&self, path: &Path, change_id: &str) -> Result<ChangeEvolog, BackendError> {
        let (_workspace, repo) = self.load_repo_at_head(path)?;
        build_change_evolog(&repo, change_id)
    }

    fn file_annotate(
        &self,
        path: &Path,
        change_id: &str,
        file_path: &str,
    ) -> Result<FileAnnotation, BackendError> {
        let (_workspace, repo) = self.load_repo_at_head(path)?;
        build_file_annotation(&repo, change_id, file_path)
    }

    fn describe(
        &self,
        path: &Path,
        change_id: &str,
        description: &str,
    ) -> Result<MutationOutcome, BackendError> {
        let (_write, mut workspace, repo) = self.load_repo_for_write(path)?;
        let repo = sync_workspace(&mut workspace, repo)?;
        let commit = resolve_change_commit_for_mutation(&repo, change_id)?;
        check_mutable(&workspace, repo.as_ref(), &commit, change_id)?;

        let new_description = complete_newline(description.trim_end());
        if new_description == commit.description() {
            return Ok(MutationOutcome {
                operation_id: None,
                summary: format!("{change_id} already has this description"),
                target_change: Some(change_id.to_owned()),
            });
        }

        let mut tx = repo.start_transaction();
        let new_commit = pollster::block_on(
            tx.repo_mut()
                .rewrite_commit(&commit)
                .set_description(&new_description)
                .write(),
        )
        .map_err(mutation_err)?;
        let new_repo = finish_mutation(
            &mut workspace,
            &repo,
            tx,
            format!("describe commit {}", commit.id().hex()),
        )?;
        Ok(MutationOutcome {
            operation_id: Some(short_operation_id(new_repo.op_id())),
            summary: format!("Described {change_id}"),
            // Recomputed rather than echoed: rewriting one side of a
            // divergent change gives that node a fresh commit-id-based id.
            target_change: Some(display_id(new_repo.as_ref(), &new_commit)),
        })
    }

    fn new_change(
        &self,
        path: &Path,
        parent_change_id: &str,
    ) -> Result<MutationOutcome, BackendError> {
        let (_write, mut workspace, repo) = self.load_repo_for_write(path)?;
        let repo = sync_workspace(&mut workspace, repo)?;
        let parent = resolve_change_commit_for_mutation(&repo, parent_change_id)?;
        // Deliberately no immutability gate: a new child rewrites nothing,
        // and starting work on top of trunk is the canonical jj workflow.
        let ws_name = workspace.workspace_name().to_owned();
        let mut tx = repo.start_transaction();
        let new_wc = pollster::block_on(tx.repo_mut().check_out(ws_name, &parent))
            .map_err(mutation_err)?;
        let new_repo = finish_mutation(&mut workspace, &repo, tx, "new empty commit".to_owned())?;
        let new_id = short_change_id(new_repo.as_ref(), &new_wc);
        Ok(MutationOutcome {
            operation_id: Some(short_operation_id(new_repo.op_id())),
            summary: format!("Started {new_id} on {parent_change_id}"),
            target_change: Some(new_id),
        })
    }

    fn edit_change(&self, path: &Path, change_id: &str) -> Result<MutationOutcome, BackendError> {
        let (_write, mut workspace, repo) = self.load_repo_for_write(path)?;
        let repo = sync_workspace(&mut workspace, repo)?;
        let commit = resolve_change_commit_for_mutation(&repo, change_id)?;
        let ws_name = workspace.workspace_name().to_owned();
        if repo.view().get_wc_commit_id(&ws_name) == Some(commit.id()) {
            return Ok(MutationOutcome {
                operation_id: None,
                summary: format!("{change_id} is already the working copy"),
                target_change: Some(change_id.to_owned()),
            });
        }
        check_mutable(&workspace, repo.as_ref(), &commit, change_id)?;
        let mut tx = repo.start_transaction();
        // `edit` also abandons the old working-copy commit when it is
        // discardable (empty, undescribed, childless), like the CLI.
        pollster::block_on(tx.repo_mut().edit(ws_name, &commit)).map_err(mutation_err)?;
        let new_repo = finish_mutation(
            &mut workspace,
            &repo,
            tx,
            format!("edit commit {}", commit.id().hex()),
        )?;
        Ok(MutationOutcome {
            operation_id: Some(short_operation_id(new_repo.op_id())),
            summary: format!("Editing {change_id}"),
            target_change: Some(change_id.to_owned()),
        })
    }

    fn abandon_change(
        &self,
        path: &Path,
        change_id: &str,
    ) -> Result<MutationOutcome, BackendError> {
        let (_write, mut workspace, repo) = self.load_repo_for_write(path)?;
        let repo = sync_workspace(&mut workspace, repo)?;
        let commit = resolve_change_commit_for_mutation(&repo, change_id)?;
        check_mutable(&workspace, repo.as_ref(), &commit, change_id)?;
        let parent = repo
            .store()
            .get_commit(&commit.parent_ids()[0])
            .map_err(snapshot_err)?;
        let parent_id = display_id(repo.as_ref(), &parent);

        let mut tx = repo.start_transaction();
        tx.repo_mut().record_abandoned_commit(&commit);
        // Like the CLI, bookmarks pointing at the abandoned change are
        // deleted rather than silently retargeted at its parent. An
        // abandoned working copy is replaced by a new empty change on the
        // parent (handled inside the rebase).
        let options = RebaseOptions {
            rewrite_refs: RewriteRefsOptions {
                delete_abandoned_bookmarks: true,
            },
            ..Default::default()
        };
        pollster::block_on(tx.repo_mut().rebase_descendants_with_options(&RevsetExpression::none(), &options, |_, _| {}))
            .map_err(mutation_err)?;
        let new_repo = finish_mutation(
            &mut workspace,
            &repo,
            tx,
            format!("abandon commit {}", commit.id().hex()),
        )?;
        Ok(MutationOutcome {
            operation_id: Some(short_operation_id(new_repo.op_id())),
            summary: format!("Abandoned {change_id}"),
            target_change: Some(parent_id),
        })
    }

    fn abandon_changes(
        &self,
        path: &Path,
        change_ids: &[String],
    ) -> Result<MutationOutcome, BackendError> {
        self.abandon_set(path, change_ids, false)
    }

    fn abandon_landed(
        &self,
        path: &Path,
        commits: &[String],
    ) -> Result<MutationOutcome, BackendError> {
        self.abandon_set(path, commits, true)
    }

    fn squash_change(&self, path: &Path, change_id: &str) -> Result<MutationOutcome, BackendError> {
        let (_write, mut workspace, repo) = self.load_repo_for_write(path)?;
        let repo = sync_workspace(&mut workspace, repo)?;
        let commit = resolve_change_commit_for_mutation(&repo, change_id)?;
        check_mutable(&workspace, repo.as_ref(), &commit, change_id)?;
        let parent_id = match commit.parent_ids() {
            [parent_id] => parent_id.clone(),
            _ => {
                return Err(BackendError::MutationFailed(format!(
                    "{change_id} is a merge; squashing into multiple parents is ambiguous"
                )))
            }
        };
        let parent = repo.store().get_commit(&parent_id).map_err(snapshot_err)?;
        let parent_display = display_id(repo.as_ref(), &parent);
        check_mutable(&workspace, repo.as_ref(), &parent, &parent_display)?;

        let combined = combined_description(parent.description(), commit.description());
        let mut tx = repo.start_transaction();
        // For a single parent the child's tree is exactly "parent content
        // plus the change", so the parent takes the child's tree and the
        // combined description; the child is abandoned, and descendants and
        // bookmarks follow through the shared rebase (bookmarks on the
        // squashed change land on the rewritten parent).
        let new_parent = pollster::block_on(
            tx.repo_mut()
                .rewrite_commit(&parent)
                .set_tree(commit.tree())
                .set_description(combined)
                .write(),
        )
        .map_err(mutation_err)?;
        tx.repo_mut().record_abandoned_commit(&commit);
        let new_repo = finish_mutation(
            &mut workspace,
            &repo,
            tx,
            format!("squash commits into {}", parent.id().hex()),
        )?;
        Ok(MutationOutcome {
            operation_id: Some(short_operation_id(new_repo.op_id())),
            summary: format!("Squashed {change_id} into {parent_display}"),
            // The parent was rewritten; recompute its id in case its change
            // is divergent (commit-id-keyed nodes move with the rewrite).
            target_change: Some(display_id(new_repo.as_ref(), &new_parent)),
        })
    }

    fn restore_file(
        &self,
        path: &Path,
        change_id: &str,
        file_path: &str,
        hunks: Option<&[SplitHunk]>,
    ) -> Result<MutationOutcome, BackendError> {
        let (_write, mut workspace, repo) = self.load_repo_for_write(path)?;
        let repo = sync_workspace(&mut workspace, repo)?;
        let commit = resolve_change_commit_for_mutation(&repo, change_id)?;
        check_mutable(&workspace, repo.as_ref(), &commit, change_id)?;

        let repo_path = RepoPathBuf::from_internal_string(file_path).map_err(|err| {
            BackendError::MutationFailed(format!("invalid path {file_path}: {err}"))
        })?;
        let matcher = FilesMatcher::new([&repo_path]);
        let parent_tree =
            pollster::block_on(commit.parent_tree(repo.as_ref())).map_err(snapshot_err)?;
        let commit_tree = commit.tree();

        // With a hunk selection the file rebuilds in place: its current
        // content with just those hunks reverted — the parent side with
        // the unchosen hunks still applied. Coordinates verify against a
        // freshly derived diff exactly like a hunk split. A selection
        // covering every hunk stays on the whole-file path below, which
        // also reverts a mode flip and deletes a file the change added.
        let mut rebuilt: Option<(Vec<u8>, bool, CopyId)> = None;
        if let Some(wanted) = hunks {
            if wanted.is_empty() {
                return Err(BackendError::MutationFailed(format!(
                    "no hunks selected in {file_path}"
                )));
            }
            let copies = CopyRecords::default();
            let tree_diff = parent_tree.diff_stream_with_copies(&commit_tree, &matcher, &copies);
            let labels = MergeDiff::new(parent_tree.labels(), commit_tree.labels());
            let store = repo.store();
            let mut stream = std::pin::pin!(materialized_diff_stream(store, tree_diff, labels));
            let spliced = pollster::block_on(async {
                while let Some(entry) = stream.next().await {
                    let values = entry.values.map_err(snapshot_err)?;
                    if entry.path.target().to_owned() != repo_path {
                        continue;
                    }
                    let (before_file, before_content) =
                        split_file_side(values.before, &repo_path).await?;
                    let (after_file, after_content) =
                        split_file_side(values.after, &repo_path).await?;
                    let splice =
                        splice_hunks(&before_content, &after_content, wanted, file_path, false)?;
                    // The rebuilt file keeps the commit side's mode — only
                    // lines revert — falling back to the parent's when the
                    // change removed the file.
                    let (executable, copy_id) = match (&after_file, &before_file) {
                        (Some((exec, copy)), _) | (None, Some((exec, copy))) => {
                            (*exec, copy.clone())
                        }
                        (None, None) => {
                            return Err(BackendError::MutationFailed(format!(
                                "{file_path} does not change anything in {change_id}"
                            )));
                        }
                    };
                    return Ok(Some((splice, executable, copy_id)));
                }
                Ok::<_, BackendError>(None)
            })?;
            // No diff entry: the file no longer changes anything here, so
            // whatever diff the selection came from is gone.
            let Some((splice, executable, copy_id)) = spliced else {
                return Err(BackendError::MutationFailed(format!(
                    "the diff of {file_path} no longer matches the selected \
                     hunks — the change moved; refresh and try again"
                )));
            };
            if !splice.all {
                rebuilt = Some((splice.content, executable, copy_id));
            }
        }
        // How the outcome reads: the hunk phrase only when a true subset
        // was discarded — an all-hunks selection is the whole file.
        let what = match (&rebuilt, hunks) {
            (Some(_), Some(wanted)) => Some(hunks_phrase(wanted.len())),
            _ => None,
        };

        let new_tree = match rebuilt {
            // The parent tree's version of the path replaces the commit's —
            // the file's diff disappears from the change (removed entirely
            // when the change added it).
            None => pollster::block_on(restore_tree(
                &parent_tree,
                &commit_tree,
                "parents".to_owned(),
                format!("commit {}", commit.id().hex()),
                &matcher,
            ))
            .map_err(mutation_err)?,
            Some((content, executable, copy_id)) => {
                let id = pollster::block_on(
                    repo.store().write_file(&repo_path, &mut content.as_slice()),
                )
                .map_err(mutation_err)?;
                let mut builder = MergedTreeBuilder::new(commit_tree.clone());
                builder.set_or_remove(
                    repo_path.clone(),
                    Merge::resolved(Some(TreeValue::File {
                        id,
                        executable,
                        copy_id,
                    })),
                );
                pollster::block_on(builder.write_tree()).map_err(mutation_err)?
            }
        };
        if new_tree.tree_ids() == commit_tree.tree_ids() {
            return Ok(MutationOutcome {
                operation_id: None,
                summary: format!("{file_path} is already unchanged in {change_id}"),
                target_change: Some(change_id.to_owned()),
            });
        }
        let mut tx = repo.start_transaction();
        let new_commit = pollster::block_on(
            tx.repo_mut()
                .rewrite_commit(&commit)
                .set_tree(new_tree)
                .write(),
        )
        .map_err(mutation_err)?;
        let op_description = match &what {
            Some(phrase) => {
                format!("restore {phrase} of {file_path} in commit {}", commit.id().hex())
            }
            None => format!("restore {file_path} in commit {}", commit.id().hex()),
        };
        let new_repo = finish_mutation(&mut workspace, &repo, tx, op_description)?;
        Ok(MutationOutcome {
            operation_id: Some(short_operation_id(new_repo.op_id())),
            summary: match &what {
                Some(phrase) => format!("Discarded {phrase} of {file_path} in {change_id}"),
                None => format!("Discarded changes to {file_path} in {change_id}"),
            },
            target_change: Some(display_id(new_repo.as_ref(), &new_commit)),
        })
    }

    fn restore_change_version(
        &self,
        path: &Path,
        change_id: &str,
        commit_id: &str,
    ) -> Result<MutationOutcome, BackendError> {
        let (_write, mut workspace, repo) = self.load_repo_for_write(path)?;
        let repo = sync_workspace(&mut workspace, repo)?;
        let commit = resolve_change_commit_for_mutation(&repo, change_id)?;
        check_mutable(&workspace, repo.as_ref(), &commit, change_id)?;

        // The version must come from this change's own predecessor chain —
        // anything else is a stale panel (the change was rewritten again)
        // or a mixup, and writing an unrelated tree would silently clobber
        // the change. Same bound as the evolog answer, so every version a
        // panel can show verifies.
        let version_id = resolve_commit_id(&repo, commit_id)?.ok_or_else(|| {
            BackendError::MutationFailed(format!(
                "there is no version {commit_id} of {change_id} — refresh and try again"
            ))
        })?;
        let mut in_chain = version_id == *commit.id();
        if !in_chain {
            let walk = evolution::walk_predecessors(&repo, std::slice::from_ref(commit.id()));
            for entry in blocking_iter(walk).take(MAX_EVOLOG_ENTRIES) {
                let entry = entry.map_err(snapshot_err)?;
                if *entry.commit.id() == version_id {
                    in_chain = true;
                    break;
                }
            }
        }
        if !in_chain {
            return Err(BackendError::MutationFailed(format!(
                "{commit_id} is not an earlier version of {change_id} — refresh and try again"
            )));
        }

        let version = repo.store().get_commit(&version_id).map_err(snapshot_err)?;
        // Only tree and description restore; parents stay (a version from
        // before a rebase restores content without undoing the rebase) and
        // so does the author.
        let version_tree = version.tree();
        let new_description = version.description().to_owned();
        if version_tree.tree_ids() == commit.tree().tree_ids()
            && new_description == commit.description()
        {
            return Ok(MutationOutcome {
                operation_id: None,
                summary: format!("{change_id} is already at version {commit_id}"),
                target_change: Some(change_id.to_owned()),
            });
        }

        let mut tx = repo.start_transaction();
        let new_commit = pollster::block_on(
            tx.repo_mut()
                .rewrite_commit(&commit)
                .set_tree(version_tree)
                .set_description(new_description)
                .write(),
        )
        .map_err(mutation_err)?;
        // No jj CLI verb to pin against (`jj evolog` is read-only), so the
        // op description is Jiji's own.
        let new_repo = finish_mutation(
            &mut workspace,
            &repo,
            tx,
            format!(
                "restore commit {} to version {}",
                commit.id().hex(),
                version.id().hex()
            ),
        )?;
        Ok(MutationOutcome {
            operation_id: Some(short_operation_id(new_repo.op_id())),
            summary: format!(
                "Restored {change_id} to version {}",
                short_commit_id(new_repo.as_ref(), version.id())
            ),
            target_change: Some(display_id(new_repo.as_ref(), &new_commit)),
        })
    }

    fn duplicate_change(
        &self,
        path: &Path,
        change_id: &str,
    ) -> Result<MutationOutcome, BackendError> {
        let (_write, mut workspace, repo) = self.load_repo_for_write(path)?;
        let repo = sync_workspace(&mut workspace, repo)?;
        let commit = resolve_change_commit_for_mutation(&repo, change_id)?;
        // Deliberately no immutability gate: the copy is a new mutable
        // change beside the original; nothing is rewritten.
        let mut tx = repo.start_transaction();
        let stats = pollster::block_on(duplicate_commits_onto_parents(
            tx.repo_mut(),
            &[commit.id().clone()],
            &HashMap::new(),
        ))
        .map_err(mutation_err)?;
        let duplicate = stats
            .duplicated_commits
            .values()
            .next()
            .cloned()
            .ok_or_else(|| {
                BackendError::MutationFailed(format!("nothing was duplicated for {change_id}"))
            })?;
        let new_repo = finish_mutation(
            &mut workspace,
            &repo,
            tx,
            format!("duplicate commit {}", commit.id().hex()),
        )?;
        let new_id = display_id_of(new_repo.as_ref(), duplicate.id())?;
        Ok(MutationOutcome {
            operation_id: Some(short_operation_id(new_repo.op_id())),
            summary: format!("Duplicated {change_id} as {new_id}"),
            // Selection moves to the copy — the thing the user just made.
            target_change: Some(new_id),
        })
    }

    fn revert_change(&self, path: &Path, change_id: &str) -> Result<MutationOutcome, BackendError> {
        let (_write, mut workspace, repo) = self.load_repo_for_write(path)?;
        let repo = sync_workspace(&mut workspace, repo)?;
        let commit = resolve_change_commit_for_mutation(&repo, change_id)?;
        // Deliberately no immutability gate: the back-out is a new change
        // on `@`; nothing is rewritten, and reverting landed work is the
        // point.
        let wc_commit_id = repo
            .view()
            .get_wc_commit_id(workspace.workspace_name())
            .cloned()
            .ok_or_else(|| {
                BackendError::MutationFailed(
                    "this workspace has no working copy to revert onto".to_owned(),
                )
            })?;
        let wc_commit = repo.store().get_commit(&wc_commit_id).map_err(snapshot_err)?;

        // `jj revert -r <rev> -o @`: the back-out tree is a three-way merge
        // that applies the reverse of the target's diff (its tree as base,
        // its parents' tree as the side) onto the working copy; the labels
        // name the sides in conflict markers when that merge conflicts.
        let old_base_tree =
            pollster::block_on(commit.parent_tree(repo.as_ref())).map_err(snapshot_err)?;
        let parents_label =
            pollster::block_on(commit.parents_conflict_label()).map_err(snapshot_err)?;
        let new_tree = pollster::block_on(MergedTree::merge(Merge::from_vec(vec![
            (
                wc_commit.tree(),
                format!("{} (revert destination)", wc_commit.conflict_label()),
            ),
            (
                commit.tree(),
                format!("{} (reverted revision)", commit.conflict_label()),
            ),
            (
                old_base_tree,
                format!("{parents_label} (parents of reverted revision)"),
            ),
        ])))
        .map_err(mutation_err)?;

        // The CLI's default `templates.revert_description`, expanded.
        let description = format!(
            "Revert \"{}\"\n\nThis reverts commit {}.\n",
            commit.description().lines().next().unwrap_or(""),
            commit.id().hex()
        );
        let mut tx = repo.start_transaction();
        let new_commit = pollster::block_on(
            tx.repo_mut()
                .new_commit(vec![wc_commit_id], new_tree)
                .set_description(description)
                .write(),
        )
        .map_err(mutation_err)?;
        let new_repo = finish_mutation(
            &mut workspace,
            &repo,
            tx,
            format!("revert commit {}", commit.id().hex()),
        )?;
        let new_id = display_id_of(new_repo.as_ref(), new_commit.id())?;
        Ok(MutationOutcome {
            operation_id: Some(short_operation_id(new_repo.op_id())),
            summary: format!("Reverted {change_id} as {new_id}"),
            // Selection moves to the back-out — the thing the user just made.
            target_change: Some(new_id),
        })
    }

    fn absorb_change(
        &self,
        path: &Path,
        change_id: &str,
    ) -> Result<MutationOutcome, BackendError> {
        let (_write, mut workspace, repo) = self.load_repo_for_write(path)?;
        let repo = sync_workspace(&mut workspace, repo)?;
        let commit = resolve_change_commit_for_mutation(&repo, change_id)?;
        check_mutable(&workspace, repo.as_ref(), &commit, change_id)?;

        let source = pollster::block_on(AbsorbSource::from_commit(repo.as_ref(), commit.clone()))
            .map_err(snapshot_err)?;
        // Destinations: the source's mutable ancestors — the CLI's default
        // (`mutable() & ::<source>-`). The file annotation decides which
        // of them each hunk actually lands in.
        let immutable_heads = immutable_heads_expression(&workspace, repo.as_ref())?;
        let immutable = immutable_heads.union(&RevsetExpression::root()).ancestors();
        let this = RevsetExpression::commits(vec![commit.id().clone()]);
        let destinations = this.ancestors().minus(&immutable).minus(&this);
        let selected = pollster::block_on(split_hunks_to_trees(
            repo.as_ref(),
            &source,
            &destinations,
            &EverythingMatcher,
        ))
        .map_err(mutation_err)?;
        if selected.target_commits.is_empty() {
            return Err(BackendError::MutationFailed(format!(
                "nothing in {change_id} could be absorbed — no mutable ancestor's diff \
                 touches the same lines"
            )));
        }

        let destination_count = selected.target_commits.len();
        let mut tx = repo.start_transaction();
        let stats =
            pollster::block_on(absorb_hunks(tx.repo_mut(), &source, selected.target_commits))
                .map_err(mutation_err)?;
        let new_repo = finish_mutation(
            &mut workspace,
            &repo,
            tx,
            format!("absorb changes into {destination_count} commits"),
        )?;
        let leftovers = stats.rewritten_source.is_some();
        Ok(MutationOutcome {
            operation_id: Some(short_operation_id(new_repo.op_id())),
            summary: format!(
                "Absorbed changes from {change_id} into {destination_count} ancestor \
                 change{}{}",
                if destination_count == 1 { "" } else { "s" },
                if leftovers { " — unclaimed hunks stayed put" } else { "" },
            ),
            // The rewritten source keeps the selection when it survives;
            // a fully absorbed source is abandoned and the selection
            // falls back like any removed node's.
            target_change: stats
                .rewritten_source
                .as_ref()
                .map(|c| display_id(new_repo.as_ref(), c)),
        })
    }

    fn split_change(
        &self,
        path: &Path,
        change_id: &str,
        selection: &[SplitSelection],
        description: &str,
    ) -> Result<MutationOutcome, BackendError> {
        let (_write, mut workspace, repo) = self.load_repo_for_write(path)?;
        let repo = sync_workspace(&mut workspace, repo)?;
        let commit = resolve_change_commit_for_mutation(&repo, change_id)?;
        check_mutable(&workspace, repo.as_ref(), &commit, change_id)?;

        let selected = select_changes(&repo, &commit, change_id, selection)?;
        // The curated deviation documented on the trait: a split may not
        // cover every change — the CLI warns and leaves an empty half
        // behind, but from a plan panel that is always a mistake.
        if selected.remaining_files == 0 {
            return Err(BackendError::MutationFailed(format!(
                "the selection covers every change in {change_id}; \
                 there would be nothing left to split off"
            )));
        }
        let commit_tree = commit.tree();

        let mut tx = repo.start_transaction();
        // The selected files stay in the change itself: same change id, the
        // new description.
        let first = pollster::block_on(
            tx.repo_mut()
                .rewrite_commit(&commit)
                .set_tree(selected.tree)
                .set_description(complete_newline(description.trim_end()))
                .write(),
        )
        .map_err(mutation_err)?;
        // The remainder is a new change directly on top with the original
        // tree, description, and author. Recording it as *the* rewrite of
        // the split commit is what routes bookmarks, descendants, and the
        // working copy to it — jj's split rule.
        let second = pollster::block_on(
            tx.repo_mut()
                .rewrite_commit(&commit)
                .set_parents(vec![first.id().clone()])
                .set_tree(commit_tree)
                .generate_new_change_id()
                .write(),
        )
        .map_err(mutation_err)?;
        tx.repo_mut()
            .set_rewritten_commit(commit.id().clone(), second.id().clone());
        let new_repo = finish_mutation(
            &mut workspace,
            &repo,
            tx,
            format!("split commit {}", commit.id().hex()),
        )?;
        let first_id = display_id_of(new_repo.as_ref(), first.id())?;
        let second_id = display_id_of(new_repo.as_ref(), second.id())?;
        let kept = files_phrase(selected.whole_files, selected.partial_files);
        Ok(MutationOutcome {
            operation_id: Some(short_operation_id(new_repo.op_id())),
            summary: format!("Split {change_id}: kept {kept}, the rest moved to {second_id}"),
            // Selection stays on the carved-out half — the change the user
            // just described, which keeps the change id (recomputed in case
            // the rewrite left it divergent).
            target_change: Some(first_id),
        })
    }

    fn squash_into(
        &self,
        path: &Path,
        change_id: &str,
        selection: &[SplitSelection],
        destination_id: &str,
    ) -> Result<MutationOutcome, BackendError> {
        let (_write, mut workspace, repo) = self.load_repo_for_write(path)?;
        let repo = sync_workspace(&mut workspace, repo)?;
        let commit = resolve_change_commit_for_mutation(&repo, change_id)?;
        check_mutable(&workspace, repo.as_ref(), &commit, change_id)?;
        let destination = resolve_change_commit_for_mutation(&repo, destination_id)?;
        if destination.id() == commit.id() {
            return Err(BackendError::MutationFailed(format!(
                "cannot move changes from {change_id} into itself"
            )));
        }
        let destination_display = display_id(repo.as_ref(), &destination);
        check_mutable(&workspace, repo.as_ref(), &destination, &destination_display)?;

        let selected = select_changes(&repo, &commit, change_id, selection)?;
        let moved = files_phrase(selected.whole_files, selected.partial_files);
        let source = CommitWithSelection {
            commit: commit.clone(),
            selected_tree: selected.tree,
            parent_tree: selected.parent_tree,
        };

        let mut tx = repo.start_transaction();
        // jj's own squash engine: a partial selection rewrites the source
        // with the inverse of the selected diff, a full one abandons it
        // (bookmarks land on its parent, an emptied working copy respawns —
        // handled by the descendant rebase below), a destination sitting
        // above the source is rebased first, and the content arrives as a
        // labeled tree merge so a non-applying move records a first-class
        // conflict.
        let squashed = pollster::block_on(squash_commits(
            tx.repo_mut(),
            std::slice::from_ref(&source),
            &destination,
            false,
        ))
        .map_err(mutation_err)?;
        let Some(squashed) = squashed else {
            // Unreachable after the changes-nothing refusal; mirror the
            // CLI's "Nothing changed" just in case.
            return Ok(MutationOutcome {
                operation_id: None,
                summary: format!("Nothing to move from {change_id}"),
                target_change: Some(change_id.to_owned()),
            });
        };
        let abandoned = !squashed.abandoned_commits.is_empty();
        let mut builder = squashed.commit_builder;
        if abandoned {
            // The CLI prompts with the combined text in an editor; Jiji's
            // curated combining (destination first, empty sides yield the
            // other) is the same rule squash_change already applies.
            builder = builder.set_description(combined_description(
                destination.description(),
                commit.description(),
            ));
        }
        let new_destination = pollster::block_on(builder.write()).map_err(mutation_err)?;

        // Rebase descendants here (finish_mutation's own pass then has
        // nothing left) to learn where the rewritten source lands: a source
        // above its destination is rewritten a second time by this pass.
        let mut source_now = if abandoned {
            None
        } else {
            tx.repo().new_parents(std::slice::from_ref(commit.id())).into_iter().next()
        };
        pollster::block_on(tx.repo_mut().rebase_descendants_with_options(
            &RevsetExpression::none(),
            &RebaseOptions::default(),
            |old_commit, rebased| {
                if Some(old_commit.id()) == source_now.as_ref() {
                    if let RebasedCommit::Rewritten(new_commit) = rebased {
                        source_now = Some(new_commit.id().clone());
                    }
                }
            },
        ))
        .map_err(mutation_err)?;

        let new_repo = finish_mutation(
            &mut workspace,
            &repo,
            tx,
            format!("squash commits into {}", destination.id().hex()),
        )?;
        // Selection follows the destination when the source dissolved into
        // it; a partial move stays on the rewritten source so peeling more
        // pieces off continues where the user was.
        let target = match source_now {
            Some(source_id) => display_id_of(new_repo.as_ref(), &source_id)?,
            None => display_id_of(new_repo.as_ref(), new_destination.id())?,
        };
        let summary = if abandoned {
            format!(
                "Moved everything in {change_id} into {destination_display}; \
                 the emptied change was abandoned"
            )
        } else {
            format!("Moved {moved} from {change_id} into {destination_display}")
        };
        Ok(MutationOutcome {
            operation_id: Some(short_operation_id(new_repo.op_id())),
            summary,
            target_change: Some(target),
        })
    }

    fn rebase_change(
        &self,
        path: &Path,
        change_id: &str,
        destination_id: &str,
    ) -> Result<MutationOutcome, BackendError> {
        self.rebase_onto(path, change_id, destination_id, RebaseScope::WithDescendants)
    }

    fn rebase_onto_trunk(
        &self,
        path: &Path,
        remote: &str,
        trunk: &str,
        change_id: &str,
        landed: &[String],
    ) -> Result<MutationOutcome, BackendError> {
        let (_write, mut workspace, repo) = self.load_repo_for_write(path)?;
        let repo = sync_workspace(&mut workspace, repo)?;
        let commit = resolve_change_commit_for_mutation(&repo, change_id)?;
        // What is left of a stack moves onto trunk although someone
        // else's branch holds it: the part below it has landed, and its
        // own pull requests have to show what is left.
        check_not_settled(&workspace, repo.as_ref(), &commit, change_id)?;
        // The commit `landed_check` judges landings against, so a stack
        // is rebuilt on what was checked.
        let trunk = landing_trunk(repo.as_ref(), remote, trunk)
            .map_err(BackendError::MutationFailed)?;
        let trunk_label = display_id_of(repo.as_ref(), &trunk)?;
        let landed: Vec<CommitId> = landed
            .iter()
            .filter_map(|id| resolve_commit_id(&repo, id).transpose())
            .collect::<Result<_, _>>()?;
        // Trunk takes the place of every parent that landed: the ones the
        // caller names, and any that are part of trunk already (a segment
        // a merge commit landed, or an older trunk commit). Every other
        // parent stays. `jj rebase -s` would put the change on the
        // destination alone, and a change that merges a side branch in
        // would lose that branch's content from its tree.
        let mut parents: Vec<CommitId> = Vec::new();
        let mut moved = false;
        for parent in commit.parent_ids() {
            let off = landed.contains(parent) || is_ancestor(repo.as_ref(), parent, &trunk);
            let onto = if off { &trunk } else { parent };
            moved |= off;
            if !parents.contains(onto) {
                parents.push(onto.clone());
            }
        }
        if !moved {
            return Ok(MutationOutcome {
                operation_id: None,
                summary: format!("{change_id} no longer sits on a landed change; it was left where it is"),
                target_change: Some(change_id.to_owned()),
            });
        }
        // Someone else's commits above the change: those of a branch
        // that is no bookmark here, under no local bookmark. (A commit
        // above the change that is immutable for a reason of its own, a
        // tag for one, makes the change itself settled, and the check
        // above has refused.) A rebase carries every descendant along, and
        // theirs are not this repository's to rewrite. Those that nothing
        // of the user's sits on are hidden here first, so the rebase does
        // not reach them; their branch on the remote stays where it is,
        // on the commits this rebase replaces. (One the user built on
        // moves with that work: it could not stay behind.)
        //
        // Each set is asked for by the ids of the one before. A revset
        // that names a hidden commit is evaluated as if that commit were
        // visible, and `immutable_heads()` names one whenever a remote
        // bookmark points at a commit hidden here (their branch, after an
        // earlier round of this). Descendants taken in the same
        // expression would then include what is hidden.
        let above = collect_revset(
            RevsetExpression::commits(vec![commit.id().clone()]).descendants(),
            repo.as_ref(),
            MAX_MUTABLE_NODES,
        )?;
        let theirs = collect_revset(
            RevsetExpression::commits(above.clone())
                .intersection(&someone_elses_expression(&workspace, repo.as_ref())?),
            repo.as_ref(),
            MAX_MUTABLE_NODES,
        )?;
        let ours: Vec<CommitId> = above
            .iter()
            .filter(|id| !theirs.contains(id))
            .cloned()
            .collect();
        let hidden = collect_revset(
            RevsetExpression::commits(theirs).minus(&RevsetExpression::commits(ours).ancestors()),
            repo.as_ref(),
            MAX_MUTABLE_NODES,
        )?;
        let resting = if hidden.is_empty() {
            Vec::new()
        } else {
            branches_resting_on(repo.as_ref(), &hidden)?
        };
        let mut tx = repo.start_transaction();
        if !hidden.is_empty() {
            for id in &hidden {
                let theirs = tx.repo().store().get_commit(id).map_err(snapshot_err)?;
                tx.repo_mut().record_abandoned_commit(&theirs);
            }
            pollster::block_on(tx.repo_mut().rebase_descendants()).map_err(mutation_err)?;
        }
        let stats = pollster::block_on(move_commits(
            tx.repo_mut(),
            &MoveCommitsLocation {
                new_parent_ids: parents,
                new_child_ids: vec![],
                target: MoveCommitsTarget::Roots(vec![commit.id().clone()]),
            },
            &RebaseOptions::default(),
        ))
        .map_err(mutation_err)?;
        if stats.num_rebased_targets == 0 && stats.num_rebased_descendants == 0 {
            // Everything was already in place; like the CLI, record nothing.
            return Ok(MutationOutcome {
                operation_id: None,
                summary: format!("{change_id} is already on {trunk_label}"),
                target_change: Some(change_id.to_owned()),
            });
        }
        let rewritten = match stats.rebased_commits.get(commit.id()) {
            Some(jj_lib::rewrite::RebasedCommit::Rewritten(new_commit)) => Some(new_commit.clone()),
            _ => None,
        };
        let new_repo = finish_mutation(
            &mut workspace,
            &repo,
            tx,
            format!("rebase commit {} and descendants onto trunk", commit.id().hex()),
        )?;
        let target_change = match rewritten {
            Some(new_commit) => display_id(new_repo.as_ref(), &new_commit),
            None => change_id.to_owned(),
        };
        let descendants = stats.num_rebased_targets.saturating_sub(1);
        let mut summary = if descendants == 0 {
            format!("Rebased {change_id} onto {trunk_label}")
        } else {
            format!(
                "Rebased {change_id} and {descendants} descendant{} onto {trunk_label}",
                if descendants == 1 { "" } else { "s" }
            )
        };
        if !resting.is_empty() {
            summary.push_str(&format!(
                "; {} still {} on the commits this replaced and {} left as {}",
                name_list(&resting),
                if resting.len() == 1 { "builds" } else { "build" },
                if resting.len() == 1 { "was" } else { "were" },
                if resting.len() == 1 { "it is" } else { "they are" },
            ));
        }
        Ok(MutationOutcome {
            operation_id: Some(short_operation_id(new_repo.op_id())),
            summary,
            target_change: Some(target_change),
        })
    }

    fn branches_resting_on(
        &self,
        path: &Path,
        commits: &[String],
    ) -> Result<Vec<String>, BackendError> {
        let (workspace, repo) = self.load_repo_at_head(path)?;
        // Ids that name nothing here any more rest under nothing.
        let mut known: Vec<CommitId> = commits
            .iter()
            .filter_map(|id| resolve_commit_id(&repo, id).transpose())
            .collect::<Result<_, _>>()?;
        if known.is_empty() {
            return Ok(Vec::new());
        }
        // What is part of trunk's history is under every branch made
        // since, and none of them rests on it in this sense. (A commit
        // that landed with its own id, and trunk's copy of a change that
        // landed by a rebase merge, which keeps the change id.)
        let trunk_history = resolve_revset_text(&workspace, repo.as_ref(), "::present(trunk())")
            .map_err(BackendError::RevsetInvalid)?;
        let in_trunk: HashSet<CommitId> = RevsetExpression::commits(known.clone())
            .intersection(&trunk_history)
            .evaluate(repo.as_ref())
            .map_err(snapshot_err)?
            .iter_blocking()
            .collect::<Result<_, _>>()
            .map_err(snapshot_err)?;
        known.retain(|id| !in_trunk.contains(id));
        if known.is_empty() {
            return Ok(Vec::new());
        }
        branches_resting_on(repo.as_ref(), &known)
    }

    fn move_change(
        &self,
        path: &Path,
        change_id: &str,
        destination_id: &str,
    ) -> Result<MutationOutcome, BackendError> {
        self.rebase_onto(path, change_id, destination_id, RebaseScope::Alone)
    }

    fn create_bookmark(
        &self,
        path: &Path,
        name: &str,
        change_id: &str,
    ) -> Result<MutationOutcome, BackendError> {
        let (_write, mut workspace, repo) = self.load_repo_for_write(path)?;
        let repo = sync_workspace(&mut workspace, repo)?;
        let name = name.trim();
        validate_bookmark_name(name)?;
        if repo.view().get_local_bookmark(RefName::new(name)).is_present() {
            return Err(BackendError::MutationFailed(format!(
                "bookmark \u{201c}{name}\u{201d} already exists"
            )));
        }
        // No immutability gate: a bookmark is a ref, nothing is rewritten —
        // bookmarking trunk or another immutable change is legitimate.
        let commit = resolve_change_commit_for_mutation(&repo, change_id)?;
        let mut tx = repo.start_transaction();
        tx.repo_mut().set_local_bookmark_target(
            RefName::new(name),
            RefTarget::normal(commit.id().clone()),
        );
        let new_repo = finish_mutation(
            &mut workspace,
            &repo,
            tx,
            format!("create bookmark {name} pointing to commit {}", commit.id().hex()),
        )?;
        Ok(MutationOutcome {
            operation_id: Some(short_operation_id(new_repo.op_id())),
            summary: format!("Created {name} on {change_id}"),
            target_change: Some(change_id.to_owned()),
        })
    }

    fn move_bookmark(
        &self,
        path: &Path,
        name: &str,
        change_id: &str,
    ) -> Result<MutationOutcome, BackendError> {
        let (_write, mut workspace, repo) = self.load_repo_for_write(path)?;
        let repo = sync_workspace(&mut workspace, repo)?;
        let name = name.trim();
        let target = repo.view().get_local_bookmark(RefName::new(name)).clone();
        if target.is_absent() {
            return Err(BackendError::BookmarkMissing(name.to_owned()));
        }
        let commit = resolve_change_commit_for_mutation(&repo, change_id)?;
        if target.as_normal() == Some(commit.id()) {
            return Ok(MutationOutcome {
                operation_id: None,
                summary: format!("{name} already points at {change_id}"),
                target_change: Some(change_id.to_owned()),
            });
        }
        // The breadcrumb names the direction; a conflicted bookmark compares
        // against all its targets (and moving it resolves the conflict).
        let old_ids: Vec<CommitId> = target.added_ids().cloned().collect();
        let direction = if old_ids
            .iter()
            .all(|old| is_ancestor(repo.as_ref(), old, commit.id()))
        {
            ""
        } else if old_ids
            .iter()
            .all(|old| is_ancestor(repo.as_ref(), commit.id(), old))
        {
            " backwards"
        } else {
            " sideways"
        };
        let mut tx = repo.start_transaction();
        tx.repo_mut().set_local_bookmark_target(
            RefName::new(name),
            RefTarget::normal(commit.id().clone()),
        );
        let new_repo = finish_mutation(
            &mut workspace,
            &repo,
            tx,
            format!("point bookmark {name} to commit {}", commit.id().hex()),
        )?;
        Ok(MutationOutcome {
            operation_id: Some(short_operation_id(new_repo.op_id())),
            summary: format!("Moved {name}{direction} to {change_id}"),
            target_change: Some(change_id.to_owned()),
        })
    }

    fn rename_bookmark(
        &self,
        path: &Path,
        old_name: &str,
        new_name: &str,
    ) -> Result<MutationOutcome, BackendError> {
        let (_write, mut workspace, repo) = self.load_repo_for_write(path)?;
        let repo = sync_workspace(&mut workspace, repo)?;
        let old_name = old_name.trim();
        let new_name = new_name.trim();
        validate_bookmark_name(new_name)?;
        let target = repo
            .view()
            .get_local_bookmark(RefName::new(old_name))
            .clone();
        if target.is_absent() {
            return Err(BackendError::BookmarkMissing(old_name.to_owned()));
        }
        let target_change = target
            .added_ids()
            .next()
            .map(|id| display_id_of(repo.as_ref(), id))
            .transpose()?;
        if new_name == old_name {
            return Ok(MutationOutcome {
                operation_id: None,
                summary: format!("{old_name} is already the name"),
                target_change,
            });
        }
        if repo
            .view()
            .get_local_bookmark(RefName::new(new_name))
            .is_present()
        {
            return Err(BackendError::MutationFailed(format!(
                "bookmark \u{201c}{new_name}\u{201d} already exists"
            )));
        }
        let mut tx = repo.start_transaction();
        tx.repo_mut()
            .set_local_bookmark_target(RefName::new(new_name), target);
        tx.repo_mut()
            .set_local_bookmark_target(RefName::new(old_name), RefTarget::absent());
        let new_repo = finish_mutation(
            &mut workspace,
            &repo,
            tx,
            format!("rename bookmark {old_name} to {new_name}"),
        )?;
        Ok(MutationOutcome {
            operation_id: Some(short_operation_id(new_repo.op_id())),
            summary: format!("Renamed {old_name} to {new_name}"),
            target_change,
        })
    }

    fn delete_bookmark(&self, path: &Path, name: &str) -> Result<MutationOutcome, BackendError> {
        self.delete_bookmark_if(path, name, None)
    }

    fn delete_bookmark_at(
        &self,
        path: &Path,
        name: &str,
        commit: &str,
    ) -> Result<MutationOutcome, BackendError> {
        self.delete_bookmark_if(path, name, Some(commit))
    }

    fn track_bookmark(
        &self,
        path: &Path,
        name: &str,
        remote: &str,
    ) -> Result<MutationOutcome, BackendError> {
        let (_write, mut workspace, repo) = self.load_repo_for_write(path)?;
        let repo = sync_workspace(&mut workspace, repo)?;
        let (name, remote) = (name.trim(), remote.trim());
        let symbol = validated_remote_symbol(repo.as_ref(), name, remote)?;
        let remote_ref = repo.view().get_remote_bookmark(symbol);
        if remote_ref.state == RemoteRefState::Tracked {
            return Err(BackendError::MutationFailed(format!(
                "{name}@{remote} is already tracked"
            )));
        }
        let target_change = remote_ref
            .target
            .added_ids()
            .next()
            .map(|id| display_id_of(repo.as_ref(), id))
            .transpose()?;
        let mut tx = repo.start_transaction();
        pollster::block_on(tx.repo_mut().track_remote_bookmark(symbol))
            .map_err(mutation_err)?;
        let new_repo = finish_mutation(
            &mut workspace,
            &repo,
            tx,
            format!("track remote bookmark {name}@{remote}"),
        )?;
        Ok(MutationOutcome {
            operation_id: Some(short_operation_id(new_repo.op_id())),
            summary: format!("Tracking {name}@{remote}"),
            target_change,
        })
    }

    fn untrack_bookmark(
        &self,
        path: &Path,
        name: &str,
        remote: &str,
    ) -> Result<MutationOutcome, BackendError> {
        let (_write, mut workspace, repo) = self.load_repo_for_write(path)?;
        let repo = sync_workspace(&mut workspace, repo)?;
        let (name, remote) = (name.trim(), remote.trim());
        let symbol = validated_remote_symbol(repo.as_ref(), name, remote)?;
        if repo.view().get_remote_bookmark(symbol).state != RemoteRefState::Tracked {
            return Err(BackendError::MutationFailed(format!(
                "{name}@{remote} is not tracked"
            )));
        }
        let mut tx = repo.start_transaction();
        tx.repo_mut().untrack_remote_bookmark(symbol);
        let new_repo = finish_mutation(
            &mut workspace,
            &repo,
            tx,
            format!("untrack remote bookmark {name}@{remote}"),
        )?;
        Ok(MutationOutcome {
            operation_id: Some(short_operation_id(new_repo.op_id())),
            summary: format!("Stopped tracking {name}@{remote}"),
            target_change: None,
        })
    }

    fn delete_remote_bookmark(
        &self,
        path: &Path,
        name: &str,
        remote: &str,
    ) -> Result<MutationOutcome, BackendError> {
        let (write, mut workspace, repo) = self.load_repo_for_write(path)?;
        let repo = sync_workspace(&mut workspace, repo)?;
        let (name, remote) = (name.trim(), remote.trim());
        let symbol = validated_remote_symbol(repo.as_ref(), name, remote)?;
        let tracked =
            repo.view().get_remote_bookmark(symbol).state == RemoteRefState::Tracked;
        let local_present = repo
            .view()
            .get_local_bookmark(RefName::new(name))
            .is_present();

        // The CLI dance as one action: an untracked branch tracks first
        // (deletions only propagate through tracked refs), the local
        // bookmark goes away, and the push carries the deletion out. The
        // prep records one operation; a tracked bookmark whose local is
        // already deleted skips straight to the push.
        if !tracked || local_present {
            let mut tx = repo.start_transaction();
            if !tracked {
                pollster::block_on(tx.repo_mut().track_remote_bookmark(symbol))
                    .map_err(mutation_err)?;
            }
            tx.repo_mut()
                .set_local_bookmark_target(RefName::new(name), RefTarget::absent());
            finish_mutation(
                &mut workspace,
                &repo,
                tx,
                format!("delete bookmark {name}"),
            )?;
        }
        // The push takes the write lock itself and reloads at the head.
        drop(write);
        self.push_bookmarks(path, &[name.to_owned()], Some(remote))
    }

    fn push_bookmarks(
        &self,
        path: &Path,
        names: &[String],
        remote: Option<&str>,
    ) -> Result<MutationOutcome, BackendError> {
        let (_write, mut workspace, repo) = self.load_repo_for_write(path)?;
        let repo = sync_workspace(&mut workspace, repo)?;
        if names.is_empty() {
            return Err(BackendError::MutationFailed(
                "there are no bookmarks to push".to_owned(),
            ));
        }
        let remote_name = match remote {
            Some(name) => name.trim().to_owned(),
            None => default_push_remote(workspace.settings(), repo.store())?,
        };
        if RemoteName::new(&remote_name) == REMOTE_NAME_FOR_LOCAL_GIT_REPO {
            return Err(BackendError::MutationFailed(
                "the \u{201c}git\u{201d} remote is jj's internal name for the backing \
                 git repository and cannot be pushed to"
                    .to_owned(),
            ));
        }
        if !git_remotes(repo.store()).iter().any(|r| r.name == remote_name) {
            return Err(BackendError::MutationFailed(format!(
                "there is no git remote named \u{201c}{remote_name}\u{201d}"
            )));
        }
        let remote_ref_name = RemoteName::new(&remote_name);

        // Classify every requested bookmark against its tracked remote
        // counterpart — jj-lib's own push classification, the same rules
        // `jj git push` applies.
        let view = repo.view();
        let mut updates: Vec<(&str, MergeDiff<Option<CommitId>>)> = Vec::new();
        let mut seen: HashSet<&str> = HashSet::new();
        for raw in names {
            let name = raw.trim();
            if name.is_empty() || !seen.insert(name) {
                continue;
            }
            let ref_name = RefName::new(name);
            let local_target = view.get_local_bookmark(ref_name);
            let remote_ref = view.get_remote_bookmark(RemoteRefSymbol {
                name: ref_name,
                remote: remote_ref_name,
            });
            // Curated deviation: a name with no local bookmark and no
            // tracked remote counterpart has nothing to push or delete. The
            // CLI warns "No matching bookmarks" and exits cleanly; from an
            // explicit UI action that is a stale plan, so it refuses.
            if local_target.is_absent() && remote_ref.tracked_target().is_absent() {
                return Err(BackendError::BookmarkMissing(name.to_owned()));
            }
            match classify_ref_push_action(LocalAndRemoteRef {
                local_target,
                remote_ref,
            }) {
                RefPushAction::AlreadyMatches => {}
                RefPushAction::LocalConflicted => {
                    return Err(BackendError::MutationFailed(format!(
                        "Bookmark \u{201c}{name}\u{201d} is conflicted; repoint it \
                         before pushing"
                    )));
                }
                RefPushAction::RemoteConflicted => {
                    return Err(BackendError::MutationFailed(format!(
                        "Bookmark \u{201c}{name}@{remote_name}\u{201d} is conflicted; \
                         fetch to update it first"
                    )));
                }
                RefPushAction::RemoteUntracked => {
                    return Err(BackendError::MutationFailed(format!(
                        "A non-tracking remote bookmark \u{201c}{name}@{remote_name}\u{201d} \
                         already exists; track it first (jj bookmark track \
                         {name} --remote={remote_name})"
                    )));
                }
                RefPushAction::Update(update) => updates.push((name, update)),
            }
        }
        if updates.is_empty() {
            return Ok(MutationOutcome {
                operation_id: None,
                summary: format!("Everything is already pushed to {remote_name}"),
                target_change: None,
            });
        }

        // CLI parity: every commit that would land on the remote — the
        // range from the remote's current positions up to the pushed
        // targets — must be presentable before anything runs.
        check_commits_ready_to_push(repo.as_ref(), remote_ref_name, &updates)?;

        let op_description = match &updates[..] {
            [(name, _)] => format!("push bookmark {name} to git remote {remote_name}"),
            _ => format!(
                "push bookmarks {} to git remote {remote_name}",
                updates.iter().map(|(name, _)| *name).collect::<Vec<_>>().join(", ")
            ),
        };

        let subprocess_options = git::GitSubprocessOptions::from_settings(workspace.settings())
            .map_err(|err| BackendError::ConfigInvalid(format!("git.executable-path: {err}")))?;
        let targets = git::GitPushRefTargets {
            bookmarks: updates
                .iter()
                .map(|(name, update)| (RefName::new(name).to_owned(), update.clone()))
                .collect(),
            tags: Vec::new(),
        };
        let mut tx = repo.start_transaction();
        let stats = git::push_refs(
            tx.repo_mut(),
            subprocess_options,
            remote_ref_name,
            &targets,
            &mut QuietGitCallback,
            &git::GitPushOptions::default(),
        )
        .map_err(|err| {
            BackendError::MutationFailed(format!("could not push to {remote_name}: {err}"))
        })?;
        if !stats.unexported_bookmarks.is_empty() {
            tracing::warn!(
                count = stats.unexported_bookmarks.len(),
                "some pushed bookmarks failed to export to the backing git repo"
            );
        }

        // Force-with-lease refusals: the remote moved past what the last
        // fetch recorded. Nothing pushed → record nothing, like the CLI; a
        // partial push keeps its operation (the remote really moved) and
        // the error tells the rest of the story.
        let rejected: Vec<String> = stats
            .rejected
            .iter()
            .chain(stats.remote_rejected.iter())
            .map(|(ref_name, _)| bookmark_of_git_ref(ref_name.as_str()))
            .collect();
        if !rejected.is_empty() && stats.pushed.is_empty() {
            return Err(remote_moved_error(&rejected, &remote_name, None));
        }
        let new_repo = finish_mutation(&mut workspace, &repo, tx, op_description)?;
        if !rejected.is_empty() {
            let pushed: Vec<String> = stats
                .pushed
                .iter()
                .map(|ref_name| bookmark_of_git_ref(ref_name.as_str()))
                .collect();
            return Err(remote_moved_error(&rejected, &remote_name, Some(&pushed)));
        }

        let (pushed, deleted): (Vec<&str>, Vec<&str>) = {
            let mut pushed = Vec::new();
            let mut deleted = Vec::new();
            for (name, update) in &updates {
                if update.after.is_some() {
                    pushed.push(*name);
                } else {
                    deleted.push(*name);
                }
            }
            (pushed, deleted)
        };
        let summary = match (&pushed[..], &deleted[..]) {
            ([name], []) => format!("Pushed {name} to {remote_name}"),
            (names, []) => format!("Pushed {} bookmarks to {remote_name}", names.len()),
            ([], [name]) => format!("Deleted {name} on {remote_name}"),
            ([], names) => format!("Deleted {} bookmarks on {remote_name}", names.len()),
            (names, gone) => format!(
                "Pushed {} bookmark{} and deleted {} on {remote_name}",
                names.len(),
                if names.len() == 1 { "" } else { "s" },
                gone.len()
            ),
        };
        Ok(MutationOutcome {
            operation_id: Some(short_operation_id(new_repo.op_id())),
            summary,
            target_change: None,
        })
    }

    fn git_fetch(
        &self,
        path: &Path,
        remotes: Option<&[String]>,
    ) -> Result<MutationOutcome, BackendError> {
        let (_write, mut workspace, repo) = self.load_repo_for_write(path)?;
        let repo = sync_workspace(&mut workspace, repo)?;
        let settings = workspace.settings().clone();

        let all_remotes = git::get_all_remote_names(repo.store()).map_err(|err| {
            BackendError::MutationFailed(format!("could not list git remotes: {err}"))
        })?;

        // Which remotes to fetch, kept in remote-listing order — the CLI's
        // op description follows `get_all_remote_names`, not the order the
        // names were asked for in (pinned empirically: `git.fetch =
        // ["upstream", "mirror"]` records `…remote(s) mirror,upstream`).
        let matching: Vec<&RemoteName> = match remotes {
            Some(names) => {
                let mut wanted: Vec<&str> = Vec::new();
                for raw in names {
                    let name = raw.trim();
                    if name.is_empty() || wanted.contains(&name) {
                        continue;
                    }
                    // Curated deviation: an explicitly-named remote that
                    // does not exist refuses (the CLI warns "No matching
                    // remotes" and continues) — from an explicit action
                    // that is a stale plan.
                    if !all_remotes.iter().any(|r| r.as_str() == name) {
                        return Err(BackendError::MutationFailed(format!(
                            "there is no git remote named \u{201c}{name}\u{201d}"
                        )));
                    }
                    wanted.push(name);
                }
                all_remotes
                    .iter()
                    .map(AsRef::as_ref)
                    .filter(|r: &&RemoteName| wanted.contains(&r.as_str()))
                    .collect()
            }
            None => {
                let expr = default_fetch_remotes(&settings, &all_remotes)?;
                let matcher = expr.to_matcher();
                // CLI parity: a configured name with no matching remote
                // only warns — a `git.fetch` entry pointing at a removed
                // remote must not break the background cadence.
                for name in expr.exact_strings() {
                    if all_remotes.iter().all(|r| r.as_str() != name) {
                        tracing::warn!(remote = name, "git.fetch names a remote that does not exist");
                    }
                }
                all_remotes
                    .iter()
                    .map(AsRef::as_ref)
                    .filter(|r: &&RemoteName| matcher.is_match(r.as_str()))
                    .collect()
            }
        };
        if matching.is_empty() {
            return Err(BackendError::MutationFailed(
                "there are no git remotes to fetch from".to_owned(),
            ));
        }

        // Per-remote branch/tag selection, like the CLI: the
        // `remotes.<name>.fetch-bookmarks`/`fetch-tags` settings when set,
        // else the remote's own fetch refspecs from git config. With no
        // explicit tag selection git's implicit tag-following stays on.
        let git_repo = git::get_git_backend(repo.store())
            .map_err(mutation_err)?
            .git_repo();
        let mut expansions = Vec::with_capacity(matching.len());
        for remote in &matching {
            let bookmark = match remote_fetch_expression(&settings, remote, "fetch-bookmarks")? {
                Some(expr) => expr,
                None => {
                    let (ignored, expr) =
                        load_default_fetch_bookmarks(remote, &git_repo).map_err(|err| {
                            BackendError::MutationFailed(format!(
                                "could not read fetch refspecs for {}: {err}",
                                remote.as_str()
                            ))
                        })?;
                    for spec in ignored.0 {
                        tracing::warn!(
                            refspec = %spec.refspec,
                            reason = spec.reason,
                            "ignored a fetch refspec"
                        );
                    }
                    expr
                }
            };
            // Tags fetch like bookmarks since jj 0.44 (as `<name>@<remote>`,
            // tracked by local tags); `remotes.<name>.fetch-tags` narrows
            // them, exactly like the CLI.
            let tag = remote_fetch_expression(&settings, remote, "fetch-tags")?
                .unwrap_or_else(StringExpression::all);
            let expanded = expand_fetch_refspecs(remote, GitFetchRefExpression { bookmark, tag })
                .map_err(|err| {
                BackendError::MutationFailed(format!("could not build fetch refspecs: {err}"))
            })?;
            expansions.push((*remote, expanded));
        }

        let op_description = format!(
            "fetch from git remote(s) {}",
            matching
                .iter()
                .map(|n| n.as_symbol().to_string())
                .collect::<Vec<_>>()
                .join(",")
        );
        let remotes_label = match &matching[..] {
            [one] => one.as_str().to_owned(),
            [a, b] => format!("{} and {}", a.as_str(), b.as_str()),
            many => format!("{} remotes", many.len()),
        };

        let subprocess_options = git::GitSubprocessOptions::from_settings(&settings)
            .map_err(|err| BackendError::ConfigInvalid(format!("git.executable-path: {err}")))?;
        let import_options = git_import_options(&settings)?;
        let mut tx = repo.start_transaction();
        let stats = {
            let mut fetch = GitFetch::new(tx.repo_mut(), subprocess_options, &import_options)
                .map_err(mutation_err)?;
            for (remote, expanded) in expansions {
                fetch
                    .fetch(remote, expanded, &mut QuietGitCallback, None)
                    .map_err(|err| {
                        BackendError::MutationFailed(format!(
                            "could not fetch from {}: {err}",
                            remote.as_str()
                        ))
                    })?;
            }
            pollster::block_on(fetch.import_refs()).map_err(mutation_err)?
        };
        if !stats.failed_ref_names.is_empty() {
            tracing::warn!(
                count = stats.failed_ref_names.len(),
                "some fetched git refs failed to import"
            );
        }
        if !tx.repo().has_changes() {
            return Ok(MutationOutcome {
                operation_id: None,
                summary: format!("Nothing new on {remotes_label}"),
                target_change: None,
            });
        }
        let moved = stats.changed_remote_bookmarks.len();
        let new_repo = finish_mutation(&mut workspace, &repo, tx, op_description)?;
        let summary = match moved {
            0 => format!("Fetched from {remotes_label}"),
            1 => format!("Fetched 1 bookmark update from {remotes_label}"),
            n => format!("Fetched {n} bookmark updates from {remotes_label}"),
        };
        Ok(MutationOutcome {
            operation_id: Some(short_operation_id(new_repo.op_id())),
            summary,
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
        let (_write, mut workspace, repo) = self.load_repo_for_write(path)?;
        let repo = sync_workspace(&mut workspace, repo)?;
        let settings = workspace.settings().clone();

        let name = bookmark.trim();
        validate_bookmark_name(name)?;
        if repo.view().get_local_bookmark(RefName::new(name)).is_present() {
            return Err(BackendError::MutationFailed(format!(
                "bookmark \u{201c}{name}\u{201d} already exists"
            )));
        }
        let remote = remote.trim();
        let all_remotes = git::get_all_remote_names(repo.store()).map_err(|err| {
            BackendError::MutationFailed(format!("could not list git remotes: {err}"))
        })?;
        if !all_remotes.iter().any(|r| r.as_str() == remote) {
            return Err(BackendError::MutationFailed(format!(
                "there is no git remote named \u{201c}{remote}\u{201d}"
            )));
        }

        // The raw fetch. jj-lib's fetch engine only expands branch and tag
        // refspecs, and jj has no way to fetch `refs/pull/*` at all
        // (jj-vcs/jj#4388) — so run git the way jj's own subprocess layer
        // does (same executable setting, same git dir) into a temporary
        // ref jj's importer never reads, then hand the arrived commit to
        // jj-lib. `--no-write-fetch-head` keeps the fetch invisible to
        // other git tooling, like jj's own fetches.
        let subprocess_options = git::GitSubprocessOptions::from_settings(&settings)
            .map_err(|err| BackendError::ConfigInvalid(format!("git.executable-path: {err}")))?;
        let git_dir = git::get_git_backend(repo.store())
            .map_err(mutation_err)?
            .git_repo_path()
            .to_owned();
        let pr_ref = format!("refs/pull/{number}/head");
        let temp_ref = format!("refs/jiji/pr-{number}-head");
        run_git(
            &subprocess_options,
            &git_dir,
            &[
                "fetch",
                "--quiet",
                "--no-tags",
                "--no-write-fetch-head",
                "--",
                remote,
                &format!("+{pr_ref}:{temp_ref}"),
            ],
        )
        .map_err(|stderr| {
            if stderr.contains("couldn't find remote ref") {
                BackendError::MutationFailed(format!(
                    "{remote} has no pull request #{number} (it serves no {pr_ref})"
                ))
            } else {
                BackendError::MutationFailed(format!(
                    "could not fetch {pr_ref} from {remote}: {stderr}"
                ))
            }
        })?;
        let oid = run_git(&subprocess_options, &git_dir, &["rev-parse", "--verify", &temp_ref])
            .map_err(|stderr| {
                BackendError::MutationFailed(format!(
                    "the fetched PR ref did not arrive: {stderr}"
                ))
            })?;
        // Best-effort cleanup; a leftover temp ref is invisible to jj.
        let _ = run_git(&subprocess_options, &git_dir, &["update-ref", "-d", &temp_ref]);
        let commit_id = CommitId::try_from_hex(oid.trim()).ok_or_else(|| {
            BackendError::MutationFailed(format!(
                "git answered a malformed commit id for {pr_ref}: {oid}"
            ))
        })?;
        let commit = repo.store().get_commit(&commit_id).map_err(mutation_err)?;

        let mut tx = repo.start_transaction();
        pollster::block_on(tx.repo_mut().add_head(&commit)).map_err(mutation_err)?;
        tx.repo_mut().set_local_bookmark_target(
            RefName::new(name),
            RefTarget::normal(commit_id.clone()),
        );
        // No CLI command exists for this, so the op description is Jiji's
        // own — kept in jj's lowercase op-log voice.
        let new_repo = finish_mutation(
            &mut workspace,
            &repo,
            tx,
            format!("fetch pull request #{number} into bookmark {name}"),
        )?;
        let target = display_id_of(new_repo.as_ref(), &commit_id)?;
        Ok(MutationOutcome {
            operation_id: Some(short_operation_id(new_repo.op_id())),
            summary: format!("Fetched PR #{number} into {name}"),
            target_change: Some(target),
        })
    }

    fn trunk_text_file(
        &self,
        path: &Path,
        candidates: &[String],
    ) -> Result<Option<(String, String)>, BackendError> {
        let (workspace, repo) = self.load_repo_at_head(path)?;
        let Some(trunk) = resolve_trunk(&workspace, repo.as_ref())? else {
            return Ok(None);
        };
        let commit = repo.store().get_commit(&trunk.target).map_err(snapshot_err)?;
        let tree = commit.tree();
        for candidate in candidates {
            let Ok(repo_path) = RepoPath::from_internal_string(candidate) else {
                continue;
            };
            // Only a plain, non-conflicted file answers; anything else
            // (missing, conflicted, a symlink) skips to the next candidate.
            let value = pollster::block_on(tree.path_value(repo_path)).map_err(snapshot_err)?;
            let materialized = pollster::block_on(jj_lib::conflicts::materialize_tree_value(
                repo.store(),
                repo_path,
                value,
                tree.labels(),
            ))
            .map_err(snapshot_err)?;
            let MaterializedTreeValue::File(mut file) = materialized else {
                continue;
            };
            let bytes =
                pollster::block_on(file.read_all(repo_path)).map_err(snapshot_err)?;
            if bytes.len() as u64 > MAX_TRUNK_TEXT_FILE_BYTES {
                continue;
            }
            match String::from_utf8(bytes) {
                Ok(text) => return Ok(Some((candidate.clone(), text))),
                Err(_) => continue,
            }
        }
        Ok(None)
    }

    fn landed_check(
        &self,
        path: &Path,
        remote: &str,
        trunk: &str,
        landing: &str,
        head: &str,
        bottom: &str,
        top: &str,
    ) -> Result<LandedCheck, BackendError> {
        let (_workspace, repo) = self.load_repo_at_head(path)?;
        let trunk = landing_trunk(repo.as_ref(), remote, trunk)
            .map_err(BackendError::SnapshotFailed)?;
        let bottom = resolve_change_commit(&repo, bottom)?;
        let top = resolve_change_commit(&repo, top)?;
        // The run's diff starts at the commit below it in the stack, which
        // the stack walk reaches through first parents. A merge of all of
        // `bottom`'s parents would already contain whatever a side branch
        // brings in, and the diff would come out without it.
        let below = bottom.parent_ids().first().ok_or_else(|| {
            BackendError::MutationFailed("the root commit is not part of any stack".to_owned())
        })?;
        let base_tree = repo.store().get_commit(below).map_err(snapshot_err)?.tree();
        let top_tree = top.tree();
        // What applying the run's whole diff on top of `onto` would still
        // change: the paths where the tree a rebase of the run onto it
        // ends with differs from `onto` itself. Nothing means `onto` holds
        // the run.
        let lacking = |onto: &Commit,
                       base_tree: &MergedTree|
         -> Result<(Vec<String>, u32), BackendError> {
            let onto_tree = onto.tree();
            let reapplied = reapply(&onto_tree, base_tree, &top_tree)?;
            Ok(differing_paths(&onto_tree, &reapplied))
        };
        let tip = repo.store().get_commit(&trunk).map_err(snapshot_err)?;

        // The commits themselves are part of trunk's history (a merge
        // commit, or a branch trunk was moved onto). That alone proves
        // nothing: a merge can take a branch in and keep none of its
        // content (seen on GitHub, which then names that merge as the pull
        // request's). So the commit where they entered trunk's line is
        // asked, whatever commit the forge names. A later revert does not
        // count against them; content dropped at that merge does.
        if is_ancestor(repo.as_ref(), top.id(), &trunk) {
            let entry = entry_into(repo.as_ref(), top.id(), &tip)?;
            // And that commit is asked about everything it brought in,
            // not about the span the caller happens to know. A caller
            // that has only the pull request's head (a landed bookmark
            // found again, with nothing mutable left to walk) names one
            // commit; the merge brought in the whole branch, and it can
            // keep the last commit's change and drop an earlier one's.
            // A history where trunk and the branch merged each other
            // before has more than one such place. Then each of them is
            // asked about, and the span the caller named as well (it is
            // what was asked before there was more than one answer, and
            // nothing that was kept on its account is deleted now). One
            // tree has to hold them all: the commit where the branch came
            // in, or else trunk's tip. A branch part of which is at the
            // one and part at the other never was on trunk whole, and its
            // bookmark stays.
            let left_at = branch_points(repo.as_ref(), &entry, top.id())?;
            let mut spans: Vec<MergedTree> = left_at.iter().map(Commit::tree).collect();
            if spans.len() != 1 {
                spans.push(base_tree.clone());
            }
            let mut missing: Option<(Vec<String>, u32)> = None;
            for span_base in &spans {
                let (paths, more) = lacking(&entry, span_base)?;
                if !paths.is_empty() {
                    missing.get_or_insert((paths, more));
                }
            }
            let Some((paths, more)) = missing else {
                return Ok(LandedCheck::Landed);
            };
            for span_base in &spans {
                if !lacking(&tip, span_base)?.0.is_empty() {
                    return Ok(LandedCheck::Missing { paths, more });
                }
            }
            return Ok(LandedCheck::Held);
        }

        let Some(landing_id) = resolve_commit_id(&repo, landing)? else {
            return Ok(LandedCheck::Unfetched);
        };
        let on_trunk = is_ancestor(repo.as_ref(), &landing_id, &trunk);
        // The pull request's merge speaks for these changes only if it
        // was merged from exactly these commits.
        let same_head = resolve_commit_id(&repo, head)?.is_some_and(|id| id == *top.id());
        if same_head && on_trunk {
            let landing = repo.store().get_commit(&landing_id).map_err(snapshot_err)?;
            let (paths, more) = lacking(&landing, &base_tree)?;
            if paths.is_empty() {
                return Ok(LandedCheck::Landed);
            }
            // The named commit lacks something. Trunk's tip may still
            // hold the changes: they can arrive by another road.
            if lacking(&tip, &base_tree)?.0.is_empty() {
                return Ok(LandedCheck::Held);
            }
            return Ok(LandedCheck::Missing { paths, more });
        }
        let (paths, more) = lacking(&tip, &base_tree)?;
        Ok(if paths.is_empty() {
            LandedCheck::Held
        } else if on_trunk {
            LandedCheck::Missing { paths, more }
        } else {
            LandedCheck::Elsewhere
        })
    }

    fn lands_as_tip(
        &self,
        path: &Path,
        remote: &str,
        trunk: &str,
        bottom: &str,
        first_top: &str,
        tip: &str,
    ) -> Result<TipCheck, BackendError> {
        let (_workspace, repo) = self.load_repo_at_head(path)?;
        let trunk = landing_trunk(repo.as_ref(), remote, trunk)
            .map_err(BackendError::SnapshotFailed)?;
        let bottom = resolve_change_commit(&repo, bottom)?;
        let first_top = resolve_change_commit(&repo, first_top)?;
        let tip = resolve_change_commit(&repo, tip)?;
        // The stack's own line, from the tip down to `bottom`. A merge on
        // it is rebased by other rules than a plain commit, and a line
        // that does not pass through the commits named is not the stack
        // the caller means: neither is judged.
        let mut line: Vec<Commit> = Vec::new();
        let mut at = tip;
        loop {
            let [parent] = at.parent_ids() else {
                return Ok(TipCheck::Unknown);
            };
            let parent = parent.clone();
            let reached = at.id() == bottom.id();
            line.push(at);
            if reached {
                break;
            }
            if line.len() >= MAX_TIP_CHECK_COMMITS {
                return Ok(TipCheck::Unknown);
            }
            at = repo.store().get_commit(&parent).map_err(snapshot_err)?;
        }
        line.reverse();
        let Some(first) = line.iter().position(|c| c.id() == first_top.id()) else {
            return Ok(TipCheck::Unknown);
        };
        let below = &bottom.parent_ids()[0];
        if *below == trunk {
            // Trunk has not moved under the stack: one pull request at a
            // time and the tip in one go are the same commits.
            return Ok(TipCheck::Same);
        }
        let trunk_tree = repo.store().get_commit(&trunk).map_err(snapshot_err)?.tree();
        let base_tree = repo.store().get_commit(below).map_err(snapshot_err)?.tree();
        if !is_ancestor(repo.as_ref(), below, &trunk) {
            // What the stack sits on is not part of trunk's history. It
            // can be on trunk all the same: a pull request that landed as
            // a squash leaves its own commits here until they are swept,
            // with the rest of the stack still on top of them. That is
            // judged too, when trunk holds everything those commits hold,
            // measured from where they left trunk's history. A stack on
            // anything else is not.
            let forks = pollster::block_on(
                repo.index()
                    .common_ancestors(std::slice::from_ref(below), std::slice::from_ref(&trunk)),
            )
            .map_err(snapshot_err)?;
            let [fork] = forks.as_slice() else {
                return Ok(TipCheck::Unknown);
            };
            let fork_tree = repo.store().get_commit(fork).map_err(snapshot_err)?.tree();
            let with_base = reapply(&trunk_tree, &fork_tree, &base_tree)?;
            if !differing_paths(&trunk_tree, &with_base).0.is_empty() {
                return Ok(TipCheck::Unknown);
            }
        }
        // The tip in one go: the stack's net change, merged into trunk.
        let whole = reapply(&trunk_tree, &base_tree, &line[line.len() - 1].tree())?;
        // One pull request at a time: the first in one piece, as the
        // forge merges it, then each commit above it in turn, as the
        // rebase that follows a landing replays them.
        let mut stepwise = reapply(&trunk_tree, &base_tree, &line[first].tree())?;
        for pair in line[first..].windows(2) {
            stepwise = reapply(&stepwise, &pair[0].tree(), &pair[1].tree())?;
        }
        let (paths, more) = differing_paths(&whole, &stepwise);
        Ok(if paths.is_empty() {
            TipCheck::Same
        } else {
            TipCheck::Differs { paths, more }
        })
    }

    fn revert_operation(&self, path: &Path, op_id: &str) -> Result<MutationOutcome, BackendError> {
        let (_write, mut workspace, repo) = self.load_repo_for_write(path)?;
        let repo = sync_workspace(&mut workspace, repo)?;
        let bad_op = resolve_operation(&repo, op_id)?;
        let mut parents = pollster::block_on(bad_op.parents())
            .map_err(mutation_err)?
            .into_iter();
        let Some(parent_op) = parents.next() else {
            return Err(BackendError::MutationFailed(
                "the root operation cannot be reverted".into(),
            ));
        };
        if parents.next().is_some() {
            return Err(BackendError::MutationFailed(format!(
                "operation {op_id} merges concurrent operations and cannot be reverted on its own"
            )));
        }
        let label = op_label(&bad_op);

        // The CLI's `jj op revert`: three-way-merge the bad operation's
        // parent state into the current state using the bad state as base —
        // current + (parent − bad), i.e. just that operation backed out,
        // everything after it kept.
        let loader = repo.loader();
        let bad_repo = pollster::block_on(loader.load_at(&bad_op)).map_err(mutation_err)?;
        let parent_repo = pollster::block_on(loader.load_at(&parent_op)).map_err(mutation_err)?;
        let mut tx = repo.start_transaction();
        pollster::block_on(tx.repo_mut().merge(&bad_repo, &parent_repo)).map_err(mutation_err)?;
        // The merge records heads the bad operation added (duplicate,
        // revert) as abandoned commits instead of editing the view; settle
        // them now so the no-op comparison below sees the real resulting
        // state instead of answering "already undone".
        pollster::block_on(tx.repo_mut().rebase_descendants()).map_err(mutation_err)?;
        let reverted_view =
            view_with_git_state_kept(tx.repo().view().store_view(), repo.view().store_view());
        // One curated deviation: when the inverse is already part of the
        // current state (the Undo button clicked twice), record nothing.
        // The CLI records a second revert operation here whose op diff is
        // empty — internal rewrite bookkeeping, not a state change
        // (verified against jj 0.41).
        if reverted_view == *repo.view().store_view() {
            return Ok(MutationOutcome {
                operation_id: None,
                summary: format!("{label} is already undone"),
                target_change: None,
            });
        }
        tx.repo_mut().set_view(reverted_view);
        let new_repo = finish_mutation(
            &mut workspace,
            &repo,
            tx,
            format!("revert operation {}", bad_op.id().hex()),
        )?;
        Ok(MutationOutcome {
            operation_id: Some(short_operation_id(new_repo.op_id())),
            summary: format!("Reverted {label}"),
            target_change: working_copy_change(&workspace, &new_repo)?,
        })
    }

    fn restore_operation(
        &self,
        path: &Path,
        op_id: &str,
    ) -> Result<MutationOutcome, BackendError> {
        let (_write, mut workspace, repo) = self.load_repo_for_write(path)?;
        let repo = sync_workspace(&mut workspace, repo)?;
        let target_op = resolve_operation(&repo, op_id)?;
        let label = op_label(&target_op);

        // The CLI's `jj op restore`: the target operation's view becomes the
        // current view wholesale, unwinding everything after it.
        let target_view = pollster::block_on(target_op.view()).map_err(mutation_err)?;
        let restored_view =
            view_with_git_state_kept(target_view.store_view(), repo.view().store_view());
        if restored_view == *repo.view().store_view() {
            // Restoring to where the repo already stands; like the CLI's
            // "Nothing changed.", record nothing.
            return Ok(MutationOutcome {
                operation_id: None,
                summary: "The repo is already in this state".into(),
                target_change: None,
            });
        }
        let mut tx = repo.start_transaction();
        tx.repo_mut().set_view(restored_view);
        let new_repo = finish_mutation(
            &mut workspace,
            &repo,
            tx,
            format!("restore to operation {}", target_op.id().hex()),
        )?;
        Ok(MutationOutcome {
            operation_id: Some(short_operation_id(new_repo.op_id())),
            summary: format!("Restored to {label}"),
            target_change: working_copy_change(&workspace, &new_repo)?,
        })
    }

    fn resolve_conflict(
        &self,
        path: &Path,
        change_id: &str,
        file_path: &str,
    ) -> Result<MutationOutcome, BackendError> {
        // Phase 1 — materialize the conflict, exactly like `jj resolve`
        // starting up: sync, resolve the change, refuse immutable targets,
        // and extract the conflict's sides.
        let (write, mut workspace, repo) = self.load_repo_for_write(path)?;
        let repo = sync_workspace(&mut workspace, repo)?;
        let commit = resolve_change_commit_for_mutation(&repo, change_id)?;
        check_mutable(&workspace, repo.as_ref(), &commit, change_id)?;
        let tool = crate::merge_tool::resolve_merge_tool(workspace.settings())?;

        let tree = commit.tree();
        let (repo_path, conflict, file, executable) =
            materialized_file_conflict(repo.store(), &tree, change_id, file_path)?;

        // Marker length and the pre-populated output, mirroring the CLI:
        // markers only for tools that edit them in place; the tool's marker
        // style overrides `ui.conflict-marker-style`.
        let uses_marker_length = tool.merge_args.iter().any(|arg| arg.contains("$marker_length"));
        let marker_len = if tool.edits_conflict_markers || uses_marker_length {
            choose_materialized_conflict_marker_len(&file.contents)
        } else {
            MIN_CONFLICT_MARKER_LEN
        };
        let initial_output = if tool.edits_conflict_markers {
            let style = match tool.marker_style {
                Some(style) => style,
                None => workspace
                    .settings()
                    .get::<ConflictMarkerStyle>("ui.conflict-marker-style")
                    .map_err(|err| {
                        BackendError::ConfigInvalid(format!("ui.conflict-marker-style: {err}"))
                    })?,
            };
            let options = ConflictMaterializeOptions {
                marker_style: style,
                marker_len: Some(marker_len),
                merge: repo.store().merge_options().clone(),
            };
            materialize_merge_result_to_bytes(&file.contents, &file.labels, &options).into()
        } else {
            Vec::new()
        };

        // Phase 2 — hand off to the tool and wait. No transaction is open
        // and the write lock is released; the merge window can stay up for
        // as long as the user needs while refreshes and other actions run.
        drop(write);
        let file_name = repo_path
            .components()
            .next_back()
            .map(|name| name.as_internal_str().to_owned())
            .unwrap_or_default();
        let output = crate::merge_tool::run_merge_tool(
            &tool,
            &crate::merge_tool::MergeInput {
                base: file.contents.get_remove(0).map(|c| c.as_slice()).unwrap_or_default(),
                left: file.contents.get_add(0).map(|c| c.as_slice()).unwrap_or_default(),
                right: file.contents.get_add(1).map(|c| c.as_slice()).unwrap_or_default(),
                initial_output: &initial_output,
                file_name: &file_name,
                repo_path: file_path,
                marker_len,
            },
        )?;

        // Phase 3 — record the result. The repo may have moved while the
        // merge window was open (the CLI blindly rewrites; Jiji re-reads):
        // reload at the current head, follow the change id to its current
        // commit, and refuse if the file itself changed underneath the tool.
        let (_write, mut workspace, repo) = self.load_repo_for_write(path)?;
        let repo = sync_workspace(&mut workspace, repo)?;
        let commit = resolve_change_commit_for_mutation(&repo, change_id).map_err(|err| match err {
            BackendError::ChangeMissing(id) => BackendError::MutationFailed(format!(
                "{id} disappeared while the merge tool was open; nothing was recorded"
            )),
            err => err,
        })?;
        check_mutable(&workspace, repo.as_ref(), &commit, change_id)?;
        let tree = commit.tree();
        let current_value = pollster::block_on(tree.path_value(repo_path)).map_err(snapshot_err)?;
        if current_value != conflict {
            return Err(BackendError::MutationFailed(format!(
                "{file_path} in {change_id} changed while the merge tool was open; \
                 nothing was recorded — resolve it again"
            )));
        }

        let store = repo.store().clone();
        let new_file_ids = if tool.edits_conflict_markers || output.exit_implies_conflict {
            // Remaining conflict markers parse back into a (possibly
            // partially resolved) conflict.
            pollster::block_on(update_from_content(
                &file.unsimplified_ids,
                &store,
                repo_path,
                &output.content,
                marker_len,
            ))
            .map_err(mutation_err)?
        } else {
            let file_id =
                pollster::block_on(store.write_file(repo_path, &mut output.content.as_slice()))
                    .map_err(mutation_err)?;
            Merge::normal(file_id)
        };
        // An exit code that promised conflict markers, but none parsed:
        // most likely broken markers — treating it as resolved would lie.
        if output.exit_implies_conflict && new_file_ids.is_resolved() {
            return Err(BackendError::MutationFailed(format!(
                "\u{201c}{}\u{201d} reported unresolved conflicts but left no valid conflict \
                 markers in {file_path}; nothing was recorded",
                tool.name
            )));
        }
        let fully_resolved = new_file_ids.is_resolved();

        let new_value = match new_file_ids.into_resolved() {
            Ok(file_id) => Merge::resolved(file_id.map(|id| jj_lib::backend::TreeValue::File {
                id,
                executable,
                copy_id: jj_lib::backend::CopyId::placeholder(),
            })),
            // Still conflicted: update the file ids, keep the rest of the
            // conflict's shape (executable bits and such) as it was.
            Err(file_ids) => conflict.with_new_file_ids(&file_ids),
        };
        let mut tree_builder = MergedTreeBuilder::new(tree);
        tree_builder.set_or_remove(repo_path.to_owned(), new_value);
        let new_tree = pollster::block_on(tree_builder.write_tree()).map_err(mutation_err)?;

        let mut tx = repo.start_transaction();
        let new_commit = pollster::block_on(
            tx.repo_mut().rewrite_commit(&commit).set_tree(new_tree).write(),
        )
        .map_err(mutation_err)?;
        let new_repo = finish_mutation(
            &mut workspace,
            &repo,
            tx,
            format!("Resolve conflicts in commit {}", commit.id().hex()),
        )?;
        let summary = if fully_resolved {
            format!("Resolved {file_path} in {change_id}")
        } else {
            format!("Partially resolved {file_path} in {change_id} — conflicts remain")
        };
        Ok(MutationOutcome {
            operation_id: Some(short_operation_id(new_repo.op_id())),
            summary,
            target_change: Some(display_id(new_repo.as_ref(), &new_commit)),
        })
    }

    fn resolve_conflict_take(
        &self,
        path: &Path,
        change_id: &str,
        file_path: &str,
        side: ConflictSide,
    ) -> Result<MutationOutcome, BackendError> {
        let (_write, mut workspace, repo) = self.load_repo_for_write(path)?;
        let repo = sync_workspace(&mut workspace, repo)?;
        let commit = resolve_change_commit_for_mutation(&repo, change_id)?;
        check_mutable(&workspace, repo.as_ref(), &commit, change_id)?;
        let tree = commit.tree();
        let (repo_path, _conflict, file, executable) =
            materialized_file_conflict(repo.store(), &tree, change_id, file_path)?;

        // The CLI's built-in `:ours`/`:theirs` tools: keep the picked side's
        // file id wholesale — including its absence, so a side that deleted
        // the file deletes it.
        let add_index = match side {
            ConflictSide::Ours => 0,
            ConflictSide::Theirs => 1,
        };
        let picked = file
            .ids
            .get_add(add_index)
            .cloned()
            .expect("a 2-sided conflict has both sides");
        let new_value = Merge::resolved(picked.map(|id| TreeValue::File {
            id,
            executable,
            copy_id: CopyId::placeholder(),
        }));
        let mut tree_builder = MergedTreeBuilder::new(tree);
        tree_builder.set_or_remove(repo_path.to_owned(), new_value);
        let new_tree = pollster::block_on(tree_builder.write_tree()).map_err(mutation_err)?;

        let mut tx = repo.start_transaction();
        let new_commit = pollster::block_on(
            tx.repo_mut().rewrite_commit(&commit).set_tree(new_tree).write(),
        )
        .map_err(mutation_err)?;
        let new_repo = finish_mutation(
            &mut workspace,
            &repo,
            tx,
            format!("Resolve conflicts in commit {}", commit.id().hex()),
        )?;
        let side_word = match side {
            ConflictSide::Ours => "ours",
            ConflictSide::Theirs => "theirs",
        };
        Ok(MutationOutcome {
            operation_id: Some(short_operation_id(new_repo.op_id())),
            summary: format!("Resolved {file_path} in {change_id} by taking {side_word}"),
            target_change: Some(display_id(new_repo.as_ref(), &new_commit)),
        })
    }

    fn update_stale_workspace(&self, path: &Path) -> Result<MutationOutcome, BackendError> {
        let (_write, mut workspace, repo) = self.load_repo_for_write(path)?;
        let ws_name = workspace.workspace_name().to_owned();
        let wc_op_id = workspace.working_copy().operation_id().clone();

        let wc_op = match pollster::block_on(repo.loader().load_operation(&wc_op_id)) {
            Ok(op) => op,
            Err(OpStoreError::ObjectNotFound { .. }) => {
                // The operation the working copy was last updated at is gone
                // (GC'd op store): nothing to replay the disk state against.
                // Like the CLI, park a recovery commit on the workspace's
                // recorded position — its tree, so the next snapshot records
                // whatever is on disk as changes *in that commit* instead of
                // guessing parents.
                let mut locked_ws =
                    pollster::block_on(workspace.start_working_copy_mutation()).map_err(mutation_err)?;
                let (new_repo, new_commit) = pollster::block_on(
                    jj_lib::working_copy::create_and_check_out_recovery_commit(
                        locked_ws.locked_wc(),
                        &repo,
                        ws_name,
                        RECOVERY_COMMIT_DESCRIPTION,
                    ),
                )
                .map_err(mutation_err)?;
                pollster::block_on(locked_ws.finish(new_repo.op_id().clone())).map_err(mutation_err)?;
                let synced = sync_workspace(&mut workspace, new_repo.clone())?;
                return Ok(MutationOutcome {
                    operation_id: Some(short_operation_id(new_repo.op_id())),
                    summary: format!(
                        "Recovered the workspace: its files were saved into new commit {}",
                        display_id(synced.as_ref(), &new_commit)
                    ),
                    target_change: Some(display_id(synced.as_ref(), &new_commit)),
                });
            }
            Err(err) => return Err(mutation_err(err)),
        };

        // Record what is on disk on top of the working copy's *own*
        // operation first, exactly like the CLI: unsaved edits belong to the
        // stale working-copy commit, not to whatever the repo moved to.
        // Reloading at head afterwards merges that operation branch in (the
        // fresh working-copy position wins the view merge).
        let stale_repo =
            pollster::block_on(workspace.repo_loader().load_at(&wc_op)).map_err(mutation_err)?;
        let stale_repo = snapshot_working_copy(&mut workspace, stale_repo)?;
        let stale_wc_commit = stale_repo
            .view()
            .get_wc_commit_id(&ws_name)
            .map(|id| stale_repo.store().get_commit(id))
            .transpose()
            .map_err(mutation_err)?;
        let repo = pollster::block_on(workspace.repo_loader().load_at_head())
            .map_err(mutation_err)?;
        let Some(desired_id) = repo.view().get_wc_commit_id(&ws_name).cloned() else {
            return Err(BackendError::MutationFailed(
                "this workspace is no longer listed in the repo".into(),
            ));
        };
        let desired = repo.store().get_commit(&desired_id).map_err(mutation_err)?;

        let mut locked_ws = pollster::block_on(workspace.start_working_copy_mutation()).map_err(mutation_err)?;
        match pollster::block_on(WorkingCopyFreshness::check_stale(
            locked_ws.locked_wc(),
            &desired,
            &repo,
        ))
        .map_err(mutation_err)?
        {
            WorkingCopyFreshness::Fresh | WorkingCopyFreshness::Updated(_) => {
                drop(locked_ws);
                Ok(MutationOutcome {
                    operation_id: None,
                    summary: "The workspace is not stale".to_owned(),
                    target_change: Some(display_id(repo.as_ref(), &desired)),
                })
            }
            WorkingCopyFreshness::WorkingCopyStale | WorkingCopyFreshness::SiblingOperation => {
                // The same guard the CLI applies: the on-disk state must
                // still be the one the snapshot above recorded.
                let stale_tree_ids = stale_wc_commit.as_ref().map(|commit| commit.tree_ids());
                if stale_tree_ids != Some(locked_ws.locked_wc().old_tree().tree_ids()) {
                    return Err(BackendError::MutationFailed(
                        "another client moved the working copy while it was being \
                         recovered; try again"
                            .into(),
                    ));
                }
                pollster::block_on(locked_ws.locked_wc().check_out(&desired))
                    .map_err(mutation_err)?;
                pollster::block_on(locked_ws.finish(repo.op_id().clone())).map_err(mutation_err)?;
                // Now unstale: finish like the start of any command (git
                // imports + snapshot), exactly what the CLI does after
                // recovery.
                let synced = sync_workspace(&mut workspace, repo)?;
                Ok(MutationOutcome {
                    // The checkout records no operation of its own — jj's
                    // model too ('jj workspace update-stale' only records
                    // the working-copy snapshot, and there is nothing an
                    // Undo could meaningfully revert).
                    operation_id: None,
                    summary: format!(
                        "Updated the workspace to {}",
                        display_id(synced.as_ref(), &desired)
                    ),
                    target_change: Some(display_id(synced.as_ref(), &desired)),
                })
            }
        }
    }
}

/// The commit description `update_stale_workspace` parks on a workspace
/// whose last-updated operation is lost — verbatim the CLI's, so anything
/// that recognizes `jj workspace update-stale` recovery commits recognizes
/// Jiji's too.
const RECOVERY_COMMIT_DESCRIPTION: &str = "RECOVERY COMMIT FROM `jj workspace update-stale`

This commit contains changes that were written to the working copy by an
operation that was subsequently lost (or was at least unavailable when you ran
`jj workspace update-stale`). Because the operation was lost, we don't know
what the parent commits are supposed to be. That means that the diff compared
to the current parents may contain changes from multiple commits.
";

/// Resolves an operation-id hex prefix — what `OperationItem.id` and
/// `MutationOutcome.operation_id` carry — against the loaded repo.
fn resolve_operation(
    repo: &Arc<ReadonlyRepo>,
    op_id: &str,
) -> Result<Operation, BackendError> {
    pollster::block_on(op_walk::resolve_op_with_repo(repo, op_id)).map_err(|err| match err {
        op_walk::OpsetEvaluationError::OpsetResolution(
            op_walk::OpsetResolutionError::NoSuchOperation(_)
            | op_walk::OpsetResolutionError::EmptyOperations(_),
        ) => BackendError::OperationMissing(op_id.to_owned()),
        err => BackendError::MutationFailed(err.to_string()),
    })
}

/// How revert/restore summaries name an operation: its description when it
/// has one (the timeline shows the same text), the short id otherwise.
fn op_label(op: &Operation) -> String {
    let description = op.metadata().description.lines().next().unwrap_or_default();
    if description.is_empty() {
        format!("operation {}", short_operation_id(op.id()))
    } else {
        format!("\u{201c}{description}\u{201d}")
    }
}

/// The portions of an earlier view revert/restore bring back. Git-tracking
/// state (imported git refs and HEAD) always stays current — it mirrors
/// what the backing git repo physically contains, which time travel does
/// not change; `finish_mutation`'s ref export then moves the real git
/// branches to match the restored bookmarks. Same as the CLI's default
/// `view_with_desired_portions_restored`.
fn view_with_git_state_kept(
    restored: &jj_lib::op_store::View,
    current: &jj_lib::op_store::View,
) -> jj_lib::op_store::View {
    jj_lib::op_store::View {
        head_ids: restored.head_ids.clone(),
        local_bookmarks: restored.local_bookmarks.clone(),
        local_tags: restored.local_tags.clone(),
        remote_views: restored.remote_views.clone(),
        git_refs: current.git_refs.clone(),
        git_heads: current.git_heads.clone(),
        wc_commit_ids: restored.wc_commit_ids.clone(),
    }
}

/// The change the workbench selection should follow after time travel: the
/// (possibly moved) working copy, the one stable anchor across arbitrary
/// view changes.
fn working_copy_change(
    workspace: &Workspace,
    repo: &Arc<ReadonlyRepo>,
) -> Result<Option<String>, BackendError> {
    repo.view()
        .get_wc_commit_id(workspace.workspace_name())
        .map(|id| display_id_of(repo.as_ref(), id))
        .transpose()
}

fn snapshot_err(err: impl std::fmt::Display) -> BackendError {
    BackendError::SnapshotFailed(err.to_string())
}

fn mutation_err(err: impl std::fmt::Display) -> BackendError {
    BackendError::MutationFailed(err.to_string())
}

/// jj stores non-empty descriptions newline-terminated; normalize before
/// comparing or writing so Jiji round-trips exactly like `jj describe`.
fn complete_newline(text: &str) -> String {
    let mut text = text.to_owned();
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    text
}

fn short_operation_id(op_id: &OperationId) -> String {
    op_id.hex().chars().take(12).collect()
}

struct TrunkRef {
    name: String,
    remote: Option<String>,
    target: CommitId,
}

fn build_snapshot(
    workspace: &Workspace,
    repo: &Arc<ReadonlyRepo>,
    view_choice: &SnapshotView,
) -> Result<RepoSnapshot, BackendError> {
    let repo_ref: &dyn Repo = repo.as_ref();
    let view = repo.view();
    let store = repo.store();

    let wc_commit_id = view.get_wc_commit_id(workspace.workspace_name()).cloned();
    let trunk = resolve_trunk(workspace, repo_ref)?;

    // jj's mutable set: everything visible that is not an ancestor of
    // `immutable_heads()` (or the root). The expression comes from the
    // user's revset aliases, so custom immutability config behaves exactly
    // like the CLI — and mutations are gated by the same expression. Every
    // view computes it: it decides node kinds and workstreams even when the
    // lens draws something else.
    let immutable_heads = immutable_heads_expression(workspace, repo_ref)?;
    let mutable_expr: Arc<ResolvedRevsetExpression> = RevsetExpression::visible_heads()
        .ancestors()
        .minus(&immutable_heads.union(&RevsetExpression::root()).ancestors());
    // The user's own changes that someone else's branch holds: immutable
    // to jj, and still work in progress here. They are drawn and grouped
    // with the mutable set, so a stack does not vanish from the default
    // view the moment a fetch brings a branch that was pushed on top of
    // it; their kind stays immutable.
    let held_ids = held_commit_ids(workspace, repo_ref)?;
    let held_set: HashSet<CommitId> = held_ids.iter().cloned().collect();
    let own_expr = if held_ids.is_empty() {
        mutable_expr
    } else {
        mutable_expr.union(&RevsetExpression::commits(held_ids))
    };
    // Topological order, children before parents.
    let own_ids = collect_revset(own_expr, repo_ref, MAX_MUTABLE_NODES)?;
    let own_set: HashSet<CommitId> = own_ids.iter().cloned().collect();
    let mutable_ids: Vec<CommitId> = own_ids
        .iter()
        .filter(|id| !held_set.contains(*id))
        .cloned()
        .collect();
    let mutable_set: HashSet<CommitId> = mutable_ids.iter().cloned().collect();

    let own_commits: Vec<Commit> = own_ids
        .iter()
        .map(|id| store.get_commit(id).map_err(snapshot_err))
        .collect::<Result<_, _>>()?;
    let mutable_commits: Vec<Commit> = own_commits
        .iter()
        .filter(|commit| mutable_set.contains(commit.id()))
        .cloned()
        .collect();

    // The view lens: commits drawn beyond (or instead of) the mutable set.
    // Presets are ordinary revsets parsed with the user's alias context, so
    // `trunk()` and `mine()` mean exactly what they mean in the CLI.
    let mut truncated = false;
    let mut extra_ids: Vec<CommitId> = Vec::new();
    if let Some(text) = view_revset_text(view_choice) {
        let expr = resolve_revset_text(workspace, repo_ref, &text)
            .map_err(BackendError::RevsetInvalid)?;
        extra_ids = collect_revset(expr, repo_ref, MAX_VIEW_NODES)?;
        truncated = extra_ids.len() == MAX_VIEW_NODES;
    }

    // Commits the graph draws as first-class rows. Lens views that replace
    // the default set still pin the working copy — the "where am I" anchor
    // never disappears.
    let mut drawn_ids: Vec<CommitId> = Vec::new();
    let mut drawn_set: HashSet<CommitId> = HashSet::new();
    {
        let mut draw = |id: &CommitId| {
            if drawn_set.insert(id.clone()) {
                drawn_ids.push(id.clone());
            }
        };
        if view_keeps_default_set(view_choice) {
            own_ids.iter().for_each(&mut draw);
        } else if let Some(wc) = &wc_commit_id {
            draw(wc);
        }
        extra_ids.iter().for_each(&mut draw);
    }
    let drawn_commits: Vec<Commit> = drawn_ids
        .iter()
        .map(|id| store.get_commit(id).map_err(snapshot_err))
        .collect::<Result<_, _>>()?;

    // Immutable commits the drawn work sits on (usually trunk). They give
    // the stack a visual base without dragging in deep history.
    let mut base_ids: Vec<CommitId> = Vec::new();
    let mut seen_bases: HashSet<CommitId> = HashSet::new();
    let add_base = |id: &CommitId, seen: &mut HashSet<CommitId>, out: &mut Vec<CommitId>| {
        if !drawn_set.contains(id) && seen.insert(id.clone()) {
            out.push(id.clone());
        }
    };
    for commit in &drawn_commits {
        for parent in commit.parent_ids() {
            add_base(parent, &mut seen_bases, &mut base_ids);
        }
    }
    if let Some(trunk) = &trunk {
        add_base(&trunk.target, &mut seen_bases, &mut base_ids);
    }
    let base_commits: Vec<Commit> = base_ids
        .iter()
        .map(|id| store.get_commit(id).map_err(snapshot_err))
        .collect::<Result<_, _>>()?;

    // Short display ids, computed once per commit. Commits of a divergent
    // change key by commit id so every node id stays unique (`GraphNode::id`).
    // Undrawn mutable commits are included: workstream summaries and
    // conflict items still name them under lens views that hide them.
    let mut change_ids: HashMap<CommitId, String> = HashMap::new();
    let mut divergent: HashSet<CommitId> = HashSet::new();
    for commit in drawn_commits
        .iter()
        .chain(&base_commits)
        .chain(own_commits.iter().filter(|c| !drawn_set.contains(c.id())))
    {
        if is_divergent(repo_ref, commit) {
            divergent.insert(commit.id().clone());
        }
        change_ids.insert(commit.id().clone(), node_id(repo_ref, commit));
    }

    // Divergence the user cannot resolve: the other commits of the
    // change are all someone else's to keep (see `old_copies_held`).
    let mut old_copies_held: HashSet<CommitId> = HashSet::new();
    for commit in own_commits.iter().filter(|c| divergent.contains(c.id())) {
        let others: Vec<CommitId> =
            pollster::block_on(repo_ref.resolve_change_id(commit.change_id()))
                .map_err(snapshot_err)?
                .map(|targets| {
                    targets
                        .visible_with_offsets()
                        .map(|(_, id)| id.clone())
                        .filter(|id| id != commit.id())
                        .collect()
                })
                .unwrap_or_default();
        if others.is_empty() || others.iter().any(|id| own_set.contains(id)) {
            continue;
        }
        // Not trunk's copy either (a rebase merge lands commits that keep
        // their change ids), nor one a tag pins.
        let settled = settled_expression(workspace, repo_ref)?;
        let any_settled = RevsetExpression::commits(others)
            .intersection(&settled)
            .evaluate(repo_ref)
            .map_err(snapshot_err)?
            .iter_blocking()
            .next()
            .is_some();
        if !any_settled {
            old_copies_held.insert(commit.id().clone());
        }
    }

    let elided_links = elided_base_links(repo_ref, &base_commits, &change_ids);

    // Git tags keyed by target commit (colocated repos import them as local
    // tags). Read-only markers; a conflicted tag (concurrent imports moved
    // it two ways) surfaces on every candidate commit rather than hiding.
    let mut commit_tags: HashMap<CommitId, Vec<String>> = HashMap::new();
    for (name, target) in view.local_tags() {
        for id in target.added_ids() {
            commit_tags
                .entry(id.clone())
                .or_default()
                .push(name.as_str().to_owned());
        }
    }

    // Node kinds stay honest under every lens: commits outside the mutable
    // set (landed work, teammates' branches) render — and gate actions — as
    // the immutable context they are, while a mutable commit a lens only
    // pulls in as a base (the pinned working copy's parent) keeps reading
    // as mutable.
    let nodes: Vec<GraphNode> = drawn_commits
        .iter()
        .chain(&base_commits)
        .map(|commit| {
            let kind = match wc_node_kind(commit.id(), &wc_commit_id) {
                NodeKind::WorkingCopy => NodeKind::WorkingCopy,
                _ if mutable_set.contains(commit.id()) => NodeKind::Mutable,
                _ => NodeKind::Immutable,
            };
            graph_node(
                repo_ref,
                view,
                commit,
                kind,
                held_set.contains(commit.id()),
                old_copies_held.contains(commit.id()),
                &trunk,
                &change_ids,
                &divergent,
                &elided_links,
                &commit_tags,
            )
        })
        .collect();

    // Held changes group like mutable ones: a stack is one workstream
    // whether or not someone else's branch sits on part of it.
    let workstreams = build_workstreams(
        repo_ref,
        view,
        &own_commits,
        &own_set,
        &wc_commit_id,
        &trunk,
        &change_ids,
    );

    let mut conflicts = file_conflicts(&mutable_commits, &wc_commit_id, &change_ids);
    let bookmarks = build_bookmarks(repo_ref, &trunk, &mut conflicts, &mut change_ids)?;
    let operations = build_operations(repo)?;

    let current_name = workspace.workspace_name().as_str().to_owned();
    let staleness = current_workspace_staleness(workspace, repo)?;
    if staleness != Staleness::Fresh {
        let summary = match staleness {
            Staleness::OperationLost => {
                "This workspace's working copy was last updated by an operation the repo \
                 no longer has; recovering saves its files in a new commit"
                    .to_owned()
            }
            _ => "This workspace is stale: the repo has moved since its working copy \
                  was last updated"
                .to_owned(),
        };
        conflicts.push(ConflictItem {
            id: format!("workspace-{current_name}"),
            kind: ConflictKind::StaleWorkspace,
            summary,
            node_id: wc_commit_id.as_ref().and_then(|id| change_ids.get(id).cloned()),
            paths: Vec::new(),
            more_paths: 0,
            targets: Vec::new(),
            workspace: Some(current_name.clone()),
        });
    }

    let workspaces = view
        .wc_commit_ids()
        .iter()
        .map(|(name, commit_id)| WorkspaceSummary {
            name: name.as_str().to_owned(),
            is_default: name.as_str() == "default",
            is_current: name.as_str() == current_name,
            // Sibling workspaces' staleness is undetectable from here (see
            // `current_workspace_staleness`); theirs stays `false`.
            is_stale: name.as_str() == current_name && staleness != Staleness::Fresh,
            working_copy_node: change_ids.get(commit_id).cloned(),
        })
        .collect();

    let repo_path = workspace.workspace_root().display().to_string();
    let repo_name = workspace
        .workspace_root()
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| repo_path.clone());

    Ok(RepoSnapshot {
        repo_path,
        repo_name,
        backend: "jj-lib".to_owned(),
        view: view_choice.clone(),
        truncated,
        trunk_bookmark: trunk.as_ref().map(|t| t.name.clone()).unwrap_or_default(),
        working_copy: wc_commit_id
            .as_ref()
            .and_then(|id| change_ids.get(id).cloned())
            .unwrap_or_default(),
        workspaces,
        workstreams,
        nodes,
        bookmarks,
        git_remotes: git_remotes(store),
        conflicts,
        operations,
        resolve_tool: crate::merge_tool::available_tool_name(workspace.settings()),
    })
}

/// The repo's configured git remotes, read from the git backend's config —
/// colocated and internal git backends alike. jj's hidden `git` remote for
/// the colocated repo is excluded, like everywhere else it is hidden.
/// Repos without a git backend have none.
fn git_remotes(store: &Arc<Store>) -> Vec<GitRemote> {
    let Ok(git_backend) = git::get_git_backend(store) else {
        return Vec::new();
    };
    let git_repo = git_backend.git_repo();
    let config = git_repo.config_snapshot();
    git_repo
        .remote_names()
        .iter()
        .map(|name| name.to_string())
        .filter(|name| name != REMOTE_NAME_FOR_LOCAL_GIT_REPO.as_str())
        .filter_map(|name| {
            let url = config.string(format!("remote.{name}.url").as_str())?;
            Some(GitRemote {
                name,
                url: url.to_string(),
            })
        })
        .collect()
}

/// Snapshot node ids are shortened reverse-hex change ids — except for
/// divergent changes, whose nodes key by forward-hex commit ids (see
/// `GraphNode::id`). The alphabets are disjoint (k–z vs 0–9a–f), so an id
/// string is unambiguously one or the other. The read path serves either:
/// a divergent change id resolves to its first visible commit, which is
/// fine for inspection the UI no longer relies on (divergent nodes always
/// arrive here as commit ids).
fn resolve_change_commit(
    repo: &Arc<ReadonlyRepo>,
    change_id: &str,
) -> Result<Commit, BackendError> {
    let missing = || BackendError::ChangeMissing(change_id.to_owned());
    if let Some(prefix) = change_id_prefix(change_id) {
        let commit_id = match pollster::block_on(repo.as_ref().resolve_change_id_prefix(&prefix))
            .map_err(snapshot_err)?
        {
            PrefixResolution::SingleMatch(targets) => targets
                .visible_with_offsets()
                .next()
                .map(|(_, id)| id.clone()),
            _ => None,
        }
        .ok_or_else(missing)?;
        return repo.store().get_commit(&commit_id).map_err(snapshot_err);
    }
    let commit_id = resolve_commit_id(repo, change_id)?.ok_or_else(missing)?;
    repo.store().get_commit(&commit_id).map_err(snapshot_err)
}

/// Like `resolve_change_commit`, but with the mutation rules: a change id
/// that is divergent is refused (rewriting one arbitrary side would
/// silently pick a winner), while an explicit commit id picks one side of
/// a divergence deliberately — exactly how the CLI operates on divergent
/// changes, and how they are resolved (abandon or rewrite one copy). One
/// curated deviation: a commit id that is no longer visible (a stale
/// snapshot's node after a rewrite) is refused, where the CLI would rewrite
/// the hidden commit and *create* divergence — surprising from a GUI click.
fn resolve_change_commit_for_mutation(
    repo: &Arc<ReadonlyRepo>,
    change_id: &str,
) -> Result<Commit, BackendError> {
    let missing = || BackendError::ChangeMissing(change_id.to_owned());
    if let Some(prefix) = change_id_prefix(change_id) {
        let targets = match pollster::block_on(repo.as_ref().resolve_change_id_prefix(&prefix))
            .map_err(snapshot_err)?
        {
            PrefixResolution::SingleMatch(targets) => targets,
            _ => return Err(missing()),
        };
        let mut visible = targets.visible_with_offsets();
        let commit_id = visible.next().map(|(_, id)| id.clone()).ok_or_else(missing)?;
        if visible.next().is_some() {
            return Err(BackendError::MutationFailed(format!(
                "change {change_id} is divergent; pick one of its commits"
            )));
        }
        return repo.store().get_commit(&commit_id).map_err(snapshot_err);
    }
    let commit_id = resolve_commit_id(repo, change_id)?.ok_or_else(missing)?;
    let commit = repo.store().get_commit(&commit_id).map_err(snapshot_err)?;
    let visible = pollster::block_on(repo.as_ref().resolve_change_id(commit.change_id()))
        .map_err(snapshot_err)?
        .is_some_and(|targets| targets.has_visible(commit.id()));
    if !visible {
        return Err(BackendError::MutationFailed(format!(
            "commit {change_id} has been rewritten; refresh and try again"
        )));
    }
    Ok(commit)
}

/// Parses an id string as a reverse-hex change-id prefix. Returns `None`
/// for forward-hex commit ids (and anything else), which resolve through
/// `resolve_commit_id` instead. Empty strings parse as an empty prefix and
/// resolve as ambiguous, so they fail cleanly either way.
fn change_id_prefix(id: &str) -> Option<HexPrefix> {
    if id.is_empty() {
        return None;
    }
    HexPrefix::try_from_reverse_hex(id)
}

/// Resolves a forward-hex commit-id prefix, hidden commits included (like
/// jj, which lets you inspect rewritten-away commits by id).
fn resolve_commit_id(
    repo: &Arc<ReadonlyRepo>,
    id: &str,
) -> Result<Option<CommitId>, BackendError> {
    let Some(prefix) = (!id.is_empty()).then(|| HexPrefix::try_from_hex(id)).flatten() else {
        return Ok(None);
    };
    match pollster::block_on(repo.index().resolve_commit_id_prefix(&prefix))
        .map_err(snapshot_err)?
    {
        PrefixResolution::SingleMatch(commit_id) => Ok(Some(commit_id)),
        _ => Ok(None),
    }
}

/// Parses and resolves one revset expression with the user's alias context —
/// the same parsing the CLI does for a `-r` argument. Errors come back as
/// the full cause chain: revset errors carry their useful detail ("no such
/// function", the offending span) in `source()`, not `Display`.
fn resolve_revset_text(
    workspace: &Workspace,
    repo: &dyn Repo,
    text: &str,
) -> Result<Arc<ResolvedRevsetExpression>, String> {
    resolve_revset_with(workspace, repo, text, &revset_aliases(workspace.settings()))
}

/// [`resolve_revset_text`] with the aliases given, in place of the user's:
/// the user's with one of the caller's own laid over them, or none at all,
/// so that every name in `text` is jj's own function whatever the user's
/// configuration calls by that name.
fn resolve_revset_with(
    workspace: &Workspace,
    repo: &dyn Repo,
    text: &str,
    aliases: &RevsetAliasesMap,
) -> Result<Arc<ResolvedRevsetExpression>, String> {
    let settings = workspace.settings();
    let path_converter = RepoPathUiConverter::Fs {
        cwd: workspace.workspace_root().to_owned(),
        base: workspace.workspace_root().to_owned(),
    };
    let fileset_aliases = FilesetAliasesMap::new();
    let extensions = RevsetExtensions::default();
    let context = RevsetParseContext {
        aliases_map: aliases,
        local_variables: HashMap::new(),
        user_email: settings.user_email(),
        date_pattern_context: chrono::Local::now().into(),
        default_ignored_remote: Some(REMOTE_NAME_FOR_LOCAL_GIT_REPO),
        fileset_aliases_map: &fileset_aliases,
        extensions: &extensions,
        workspace: Some(RevsetWorkspaceContext {
            path_converter: &path_converter,
            workspace_name: workspace.workspace_name(),
        }),
    };
    let parsed = revset::parse(&mut RevsetDiagnostics::new(), text, &context)
        .map_err(|err| error_chain(&err))?;
    let symbol_resolver = SymbolResolver::new(repo, extensions.symbol_resolvers());
    parsed
        .resolve_user_expression(repo, &symbol_resolver)
        .map_err(|err| error_chain(&err))
}

/// An error and its `source()` chain as one line, for surfaces that render
/// a single message.
fn error_chain(err: &dyn std::error::Error) -> String {
    let mut text = err.to_string();
    let mut source = err.source();
    while let Some(cause) = source {
        text.push_str(&format!(": {cause}"));
        source = cause.source();
    }
    text
}

/// Commits under a local bookmark that are not part of trunk and that jj
/// calls immutable. Nearly always none, and cheap to ask: a range near the
/// tips. Only when there are some is [`settled_expression`] worked out.
const IMMUTABLE_UNDER_BOOKMARKS: &str =
    "(present(trunk())..bookmarks()) & ::immutable_heads()";

/// jj's function for the remote branches that are no bookmarks here. Their
/// commits are the ones that may be someone else's.
const UNTRACKED_REMOTE_BOOKMARKS: &str = "untracked_remote_bookmarks";

/// The untracked remote bookmarks that are someone else's work the user
/// built on. An untracked remote bookmark strictly below a local bookmark,
/// and not at one, is of that kind. One that is at a local bookmark, above
/// one, or off to the side rests on the user's own work and settles
/// nothing by itself.
const BASES_OF_OTHERS: &str = "untracked_remote_bookmarks() & (::(bookmarks()-) ~ bookmarks())";

/// Trunk, as the user's configuration names it, and the root.
const TRUNK_AND_ROOT: &str = "present(trunk()) | root()";

/// What is immutable for good, as opposed to held: trunk, someone else's
/// work that the user built on, and everything the user's
/// `immutable_heads()` names for a reason of its own. A tag keeps its
/// commit immutable whoever's branch also rests there, and so does
/// anything a configuration adds.
///
/// The last is that very alias, asked with jj's
/// `untracked_remote_bookmarks()` counting for nothing: an alias of that
/// name, defined as `none()`, is laid over the user's for the one
/// evaluation, and the user's definition is otherwise evaluated as
/// written. That can be asked only where it is certain what it sets
/// aside. It is not when the user's configuration defines a helper of
/// that name itself: what it names is then the user's own reason, and it
/// would be replaced whole. And it is not when the expression calls the
/// function with an argument: an alias shadows a function by name,
/// whatever its arguments, and takes none by keyword, so the evaluation
/// fails. In both cases nothing is taken apart. Settled is then exactly
/// what jj calls immutable: nothing is held, nothing a landing does
/// rewrites under it, and nothing is refused that jj would allow. A
/// configuration that names remote branches some other way
/// (`remote_bookmarks()`) ends the same: the override does not touch them.
fn settled_expression(
    workspace: &Workspace,
    repo: &dyn Repo,
) -> Result<Arc<ResolvedRevsetExpression>, BackendError> {
    let mut aliases = revset_aliases(workspace.settings());
    let own_helper = aliases
        .function_names()
        .any(|name| name == UNTRACKED_REMOTE_BOOKMARKS);
    let set_aside = if own_helper {
        None
    } else {
        aliases
            .insert(format!("{UNTRACKED_REMOTE_BOOKMARKS}()"), "none()", None)
            .ok()
            .and_then(|()| resolve_revset_with(workspace, repo, "immutable_heads()", &aliases).ok())
    };
    let heads = match set_aside {
        // Their branches are out. Back in come the ones that are someone
        // else's work the user built on.
        Some(for_its_own_reasons) => {
            let bases = resolve_revset_text(workspace, repo, BASES_OF_OTHERS)
                .map_err(BackendError::RevsetInvalid)?;
            for_its_own_reasons.union(&bases)
        }
        // As written, and as jj reads it. A configuration that is broken
        // altogether fails here as it does everywhere else.
        None => immutable_heads_expression(workspace, repo)?,
    };
    let trunk = resolve_revset_text(workspace, repo, TRUNK_AND_ROOT)
        .map_err(BackendError::RevsetInvalid)?;
    Ok(heads.union(&trunk).ancestors())
}

/// The user's own changes that someone else's branch holds: under a local
/// bookmark, not part of trunk, and immutable only because an untracked
/// remote bookmark has them in its history (jj's default
/// `immutable_heads()` counts every untracked remote bookmark). That is
/// what a fetch leaves when someone pushed a branch on top of a stack.
/// Children before parents.
fn held_commit_ids(workspace: &Workspace, repo: &dyn Repo) -> Result<Vec<CommitId>, BackendError> {
    let candidates = resolve_revset_text(workspace, repo, IMMUTABLE_UNDER_BOOKMARKS)
        .map_err(BackendError::RevsetInvalid)?;
    let any = candidates.clone().evaluate(repo).map_err(snapshot_err)?;
    if any.iter_blocking().next().is_none() {
        return Ok(Vec::new());
    }
    collect_revset(
        candidates.minus(&settled_expression(workspace, repo)?),
        repo,
        MAX_MUTABLE_NODES,
    )
}

/// Someone else's commits: in the history of a remote branch that is no
/// bookmark here, and under no local bookmark. Said with
/// `untracked_remote_bookmarks()` and not with `immutable_heads()`: a
/// commit that is immutable for another reason (a tag, the user's own
/// configuration) is not someone else's on that account, and is never
/// hidden or skipped as if it were. And said in jj's own words, with no
/// alias of the user's: a helper of that name in a configuration says
/// what the user wants immutable, not whose a commit is.
fn someone_elses_expression(
    workspace: &Workspace,
    repo: &dyn Repo,
) -> Result<Arc<ResolvedRevsetExpression>, BackendError> {
    resolve_revset_with(
        workspace,
        repo,
        "::untracked_remote_bookmarks() ~ ::bookmarks()",
        &RevsetAliasesMap::new(),
    )
    .map_err(BackendError::RevsetInvalid)
}

/// Parses and resolves the user's `immutable_heads()` revset alias. This is
/// the immutability boundary: the snapshot's mutable set and every mutation
/// gate evaluate the same expression, so broken config is a hard error
/// rather than a silent fall back to defaults the user overrode.
fn immutable_heads_expression(
    workspace: &Workspace,
    repo: &dyn Repo,
) -> Result<Arc<ResolvedRevsetExpression>, BackendError> {
    resolve_revset_text(workspace, repo, "immutable_heads()").map_err(|err| {
        BackendError::ConfigInvalid(format!("revset-aliases.\"immutable_heads()\": {err}"))
    })
}

/// The revset a view lens draws, or `None` for the default pipeline. The
/// expressions are parsed with the user's alias context, so `trunk()` and
/// `immutable_heads()` overrides apply exactly as in the CLI.
fn view_revset_text(view: &SnapshotView) -> Option<String> {
    match view {
        SnapshotView::Workstreams => None,
        // Every bookmarked line of work, local or remote, back to where it
        // left trunk. The trunk guard is `present()` so bookmarkless repos
        // still answer.
        SnapshotView::Team => {
            Some("::(remote_bookmarks() | bookmarks()) ~ ::present(trunk())".to_owned())
        }
        SnapshotView::Recent => Some(r#"committer_date(after:"2 weeks ago")"#.to_owned()),
        SnapshotView::Mine => Some("mine()".to_owned()),
        SnapshotView::Trunk => Some("::present(trunk())".to_owned()),
        SnapshotView::All => Some("all()".to_owned()),
        SnapshotView::Custom { revset } => Some(revset.clone()),
    }
}

/// Whether the view draws the default mutable set alongside its own commits
/// (an additive lens) or replaces it, keeping only the working copy pinned.
fn view_keeps_default_set(view: &SnapshotView) -> bool {
    matches!(
        view,
        SnapshotView::Workstreams | SnapshotView::Team | SnapshotView::Recent
    )
}

/// Refuses to rewrite commits jj considers immutable, using the same
/// `immutable_heads()` expression the snapshot renders.
/// The opening move both resolve verbs share (external tool and
/// take-a-side): parse the path, find the conflict in the commit's tree,
/// and materialize its file content. Refuses paths that are not conflicted
/// or not present, conflicts involving something other than normal files,
/// conflicts with more than 2 sides (both surfaces present exactly one
/// base and two sides, like the CLI), and executable-bit conflicts.
fn materialized_file_conflict<'a>(
    store: &Store,
    tree: &MergedTree,
    change_id: &str,
    file_path: &'a str,
) -> Result<(&'a RepoPath, MergedTreeValue, MaterializedFileConflictValue, bool), BackendError> {
    let repo_path = RepoPath::from_internal_string(file_path).map_err(|err| {
        BackendError::MutationFailed(format!("not a valid repository path: {err}"))
    })?;
    let conflict = match pollster::block_on(tree.path_value(repo_path)).map_err(snapshot_err)?.into_resolved() {
        Err(conflict) => conflict,
        Ok(Some(_)) => {
            return Err(BackendError::MutationFailed(format!(
                "{file_path} has no conflict in {change_id}"
            )))
        }
        Ok(None) => {
            return Err(BackendError::MutationFailed(format!(
                "{file_path} does not exist in {change_id}"
            )))
        }
    };
    let file = pollster::block_on(try_materialize_file_conflict_value(
        store,
        repo_path,
        &conflict,
        tree.labels(),
    ))
    .map_err(snapshot_err)?
    .ok_or_else(|| {
        BackendError::MutationFailed(format!(
            "the conflict in {file_path} involves something other than normal files \
             (a symlink or a deleted directory); resolve it from the CLI"
        ))
    })?;
    if file.ids.num_sides() > 2 {
        return Err(BackendError::MutationFailed(format!(
            "the conflict in {file_path} has {} sides; only 2-sided conflicts can be \
             resolved here — resolve it in steps from the CLI",
            file.ids.num_sides()
        )));
    }
    let Some(executable) = file.executable else {
        return Err(BackendError::MutationFailed(format!(
            "{file_path} has a conflict in its executable bit; resolve it from the CLI"
        )));
    };
    Ok((repo_path, conflict, file, executable))
}

fn check_mutable(
    workspace: &Workspace,
    repo: &dyn Repo,
    commit: &Commit,
    display_id: &str,
) -> Result<(), BackendError> {
    let immutable_heads = immutable_heads_expression(workspace, repo)?;
    let immutable = immutable_heads.union(&RevsetExpression::root()).ancestors();
    let expr = RevsetExpression::commits(vec![commit.id().clone()]).intersection(&immutable);
    let revset = expr.evaluate(repo).map_err(snapshot_err)?;
    match revset.iter_blocking().next() {
        None => Ok(()),
        Some(Ok(_)) => Err(BackendError::ImmutableChange(display_id.to_owned())),
        Some(Err(err)) => Err(snapshot_err(err)),
    }
}

/// The check landing uses in place of [`check_mutable`]: a commit jj calls
/// immutable may still be moved or swept when it is immutable only on
/// account of someone else's branch (see [`held_commit_ids`]). Landed
/// history, tagged commits and other people's work that the user built on
/// stay refused. The caller names commits of the stack it lands, so this
/// never lets through a commit that is someone else's to rewrite.
fn check_not_settled(
    workspace: &Workspace,
    repo: &dyn Repo,
    commit: &Commit,
    display_id: &str,
) -> Result<(), BackendError> {
    let settled = settled_expression(workspace, repo)?;
    let expr = RevsetExpression::commits(vec![commit.id().clone()]).intersection(&settled);
    let revset = expr.evaluate(repo).map_err(snapshot_err)?;
    match revset.iter_blocking().next() {
        None => Ok(()),
        Some(Ok(_)) => Err(BackendError::ImmutableChange(display_id.to_owned())),
        Some(Err(err)) => Err(snapshot_err(err)),
    }
}

/// The untracked remote bookmarks that have any of `commits` in their
/// history, as `name@remote`, in name order: whose branches still build
/// on those commits.
fn branches_resting_on(repo: &dyn Repo, commits: &[CommitId]) -> Result<Vec<String>, BackendError> {
    let mut branches: Vec<(String, Vec<CommitId>)> = Vec::new();
    for (symbol, reference) in repo.view().all_remote_bookmarks() {
        if symbol.remote == REMOTE_NAME_FOR_LOCAL_GIT_REPO
            || reference.state == RemoteRefState::Tracked
        {
            continue;
        }
        let targets: Vec<CommitId> = reference.target.added_ids().cloned().collect();
        if !targets.is_empty() {
            let name = format!("{}@{}", symbol.name.as_str(), symbol.remote.as_str());
            branches.push((name, targets));
        }
    }
    if branches.is_empty() || commits.is_empty() {
        return Ok(Vec::new());
    }
    // One question for all of them, however many branches the remote
    // has: which of the commits they point at have any of `commits` at or
    // below them. (A branch may point at a commit hidden here; named in
    // the expression, it is looked at as if it were visible.)
    let heads: Vec<CommitId> = branches
        .iter()
        .flat_map(|(_, targets)| targets.iter().cloned())
        .collect();
    let resting: HashSet<CommitId> = RevsetExpression::commits(heads)
        .intersection(&RevsetExpression::commits(commits.to_vec()).descendants())
        .evaluate(repo)
        .map_err(snapshot_err)?
        .iter_blocking()
        .collect::<Result<_, _>>()
        .map_err(snapshot_err)?;
    let mut names: Vec<String> = branches
        .into_iter()
        .filter(|(_, targets)| targets.iter().any(|id| resting.contains(id)))
        .map(|(name, _)| name)
        .collect();
    names.sort();
    Ok(names)
}

fn name_list(names: &[String]) -> String {
    match names {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

/// The tail every mutation shares: rebase descendants, keep the backing git
/// repo in sync (colocated HEAD, exported refs), commit the transaction as
/// one operation, then update the on-disk working-copy state so CLI
/// workspaces do not go stale when the working-copy commit was rewritten.
fn finish_mutation(
    workspace: &mut Workspace,
    repo: &Arc<ReadonlyRepo>,
    mut tx: Transaction,
    op_description: String,
) -> Result<Arc<ReadonlyRepo>, BackendError> {
    pollster::block_on(tx.repo_mut().rebase_descendants()).map_err(mutation_err)?;

    let ws_name = workspace.workspace_name().to_owned();
    let old_wc_id = repo.view().get_wc_commit_id(&ws_name).cloned();
    let new_wc_id = tx.repo().view().get_wc_commit_id(&ws_name).cloned();
    let wc_moved = new_wc_id.is_some() && old_wc_id != new_wc_id;

    if git::get_git_backend(repo.store()).is_ok() {
        // In a colocated repo git HEAD tracks the working copy's parent;
        // leaving it behind would make plain git see a stale checkout.
        if is_colocated(workspace, repo.store()) && wc_moved {
            let new_wc = tx
                .repo()
                .store()
                .get_commit(new_wc_id.as_ref().expect("wc_moved implies a working copy"))
                .map_err(mutation_err)?;
            pollster::block_on(git::reset_head(
                tx.repo_mut(),
                &ws_name,
                workspace.workspace_root(),
                &new_wc,
            ))
            .map_err(mutation_err)?;
        }
        let stats = git::export_refs(tx.repo_mut()).map_err(mutation_err)?;
        if !stats.failed_bookmarks.is_empty() {
            tracing::warn!(failed = stats.failed_bookmarks.len(), "some git refs failed to export");
        }
    }

    let new_repo = pollster::block_on(tx.commit(op_description)).map_err(mutation_err)?;

    if wc_moved {
        let new_wc_id = new_repo
            .view()
            .get_wc_commit_id(&ws_name)
            .expect("working copy still present after commit")
            .clone();
        let new_commit = new_repo.store().get_commit(&new_wc_id).map_err(mutation_err)?;
        // Passing the old tree makes the checkout refuse to proceed if some
        // other client moved the working copy underneath us.
        let old_tree = old_wc_id
            .map(|id| repo.store().get_commit(&id))
            .transpose()
            .map_err(mutation_err)?
            .map(|commit| commit.tree());
        pollster::block_on(workspace.check_out(
            new_repo.op_id().clone(),
            old_tree.as_ref(),
            &new_commit,
        ))
        .map_err(mutation_err)?;
    }
    Ok(new_repo)
}

/// Why the current workspace's on-disk working copy no longer matches the
/// repo: another client (usually a jj command run from a different
/// workspace) moved the repo forward without this working copy being
/// updated, or the operation the working copy was last updated at is gone
/// from the op store entirely (garbage-collected or corrupted).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Staleness {
    Fresh,
    Behind,
    OperationLost,
}

/// Unlocked staleness check for the loaded workspace, following the CLI's
/// `WorkingCopyFreshness::check_stale` rules: same operation (or same
/// checked-out tree) is fresh; a working copy whose recorded operation is
/// an ancestor of the repo's — with a different tree checked out — is
/// stale, and so is one on a sibling operation. Reads only the recorded
/// working-copy state, no lock, so every snapshot can afford it. Sibling
/// workspaces keep this state in their own roots, invisible from here —
/// only the current workspace's staleness is detectable.
fn current_workspace_staleness(
    workspace: &Workspace,
    repo: &Arc<ReadonlyRepo>,
) -> Result<Staleness, BackendError> {
    let wc = workspace.working_copy();
    if wc.operation_id() == repo.op_id() {
        return Ok(Staleness::Fresh);
    }
    let wc_op = match pollster::block_on(repo.loader().load_operation(wc.operation_id())) {
        Ok(op) => op,
        Err(OpStoreError::ObjectNotFound { .. }) => return Ok(Staleness::OperationLost),
        Err(err) => return Err(snapshot_err(err)),
    };
    let repo_op = repo.operation();
    let ancestor = pollster::block_on(dag_walk_async::closest_common_nodes(
        [wc_op.clone()],
        [repo_op.clone()],
        |op: &Operation| op.id().clone(),
        async |op: &Operation| op.parents().await,
    ))
    .map_err(snapshot_err)?
    .into_iter()
    .next();
    match ancestor {
        // The working copy is *newer* than the repo we loaded; the next
        // load-at-head catches up. Not stale.
        Some(ancestor) if ancestor.id() == repo_op.id() => Ok(Staleness::Fresh),
        Some(ancestor) if ancestor.id() == wc_op.id() => {
            // The repo moved ahead of the working copy. Only an actual tree
            // difference makes that stale — mutations that never touch `@`
            // leave the recorded operation behind on purpose.
            let Some(wc_commit_id) = repo.view().get_wc_commit_id(workspace.workspace_name())
            else {
                return Ok(Staleness::Fresh); // no longer listed; nothing to update
            };
            let wc_commit = repo.store().get_commit(wc_commit_id).map_err(snapshot_err)?;
            let wc_tree = wc.tree().map_err(snapshot_err)?;
            if wc_tree.tree_ids() == wc_commit.tree_ids() {
                Ok(Staleness::Fresh)
            } else {
                Ok(Staleness::Behind)
            }
        }
        // A sibling operation (or unrelated history): recovery works the
        // same as for a working copy left behind.
        _ => Ok(Staleness::Behind),
    }
}

/// CLI parity for the start of every command: bring the loaded repo up to
/// date with the world outside jj's own view. In a colocated workspace an
/// externally-moved git HEAD imports first (so on-disk edits attribute to a
/// working copy on the right parent), then the working copy snapshots, then
/// externally-moved git refs import. Each step records its own operation,
/// exactly like the CLI; when nothing moved, nothing is recorded.
fn sync_workspace(
    workspace: &mut Workspace,
    repo: Arc<ReadonlyRepo>,
) -> Result<Arc<ReadonlyRepo>, BackendError> {
    let repo = import_git_head(workspace, repo)?;
    let repo = snapshot_working_copy(workspace, repo)?;
    import_git_refs(workspace, repo)
}

/// Whether the workspace shares its working copy with a git checkout (the
/// repo is git-backed and git's workdir is the workspace root). Only then
/// does the CLI auto-import git state on every command.
fn is_colocated(workspace: &Workspace, store: &Arc<Store>) -> bool {
    git::get_git_backend(store).is_ok_and(|backend| {
        backend
            .open_git_repo_at_workdir(workspace.workspace_root())
            .is_ok()
    })
}

/// Imports an externally-moved git HEAD (someone ran `git checkout` or
/// `git commit` in the colocated checkout), mirroring the CLI: check out a
/// new working-copy commit on the new HEAD and reset the recorded
/// working-copy state to it without touching files — git already updated
/// them, and the snapshot that follows records any leftover edits onto the
/// right parent. A discardable old working copy is abandoned.
fn import_git_head(
    workspace: &mut Workspace,
    repo: Arc<ReadonlyRepo>,
) -> Result<Arc<ReadonlyRepo>, BackendError> {
    if !is_colocated(workspace, repo.store()) {
        return Ok(repo);
    }
    let ws_name = workspace.workspace_name().to_owned();
    let mut tx = repo.start_transaction();
    pollster::block_on(git::import_head(
        tx.repo_mut(),
        &ws_name,
        workspace.workspace_root(),
    ))
    .map_err(mutation_err)?;
    if !tx.repo().has_changes() {
        return Ok(repo);
    }
    let new_git_head = tx.repo().view().git_head(&ws_name).clone();
    let Some(new_git_head_id) = new_git_head.as_normal().cloned() else {
        // HEAD became unborn or conflicted; record the import as-is.
        return pollster::block_on(tx.commit("import git head")).map_err(mutation_err);
    };
    let new_git_head_commit = tx
        .repo()
        .store()
        .get_commit(&new_git_head_id)
        .map_err(mutation_err)?;
    pollster::block_on(tx.repo_mut().check_out(ws_name, &new_git_head_commit))
        .map_err(mutation_err)?;
    let mut locked_ws = pollster::block_on(workspace.start_working_copy_mutation())
        .map_err(mutation_err)?;
    pollster::block_on(locked_ws.locked_wc().reset(&new_git_head_commit))
        .map_err(mutation_err)?;
    pollster::block_on(tx.repo_mut().rebase_descendants()).map_err(mutation_err)?;
    let new_repo = pollster::block_on(tx.commit("import git head")).map_err(mutation_err)?;
    pollster::block_on(locked_ws.finish(new_repo.op_id().clone()))
        .map_err(mutation_err)?;
    Ok(new_repo)
}

/// Imports externally-moved git refs (branches moved by `git commit`, new
/// branches, an external `git fetch`'s remote-tracking refs), with the
/// CLI's options: `git.auto-local-bookmark` and
/// `git.abandon-unreachable-commits` from settings. Newly-abandoned commits
/// rebase descendants through the shared mutation tail, which also keeps
/// the working copy checked out if it was rewritten.
fn import_git_refs(
    workspace: &mut Workspace,
    repo: Arc<ReadonlyRepo>,
) -> Result<Arc<ReadonlyRepo>, BackendError> {
    if !is_colocated(workspace, repo.store()) {
        return Ok(repo);
    }
    let options = git_import_options(workspace.settings())?;
    let mut tx = repo.start_transaction();
    let stats = pollster::block_on(git::import_refs(tx.repo_mut(), &options)).map_err(mutation_err)?;
    if !stats.failed_ref_names.is_empty() {
        tracing::warn!(
            count = stats.failed_ref_names.len(),
            "some git refs failed to import"
        );
    }
    if !tx.repo().has_changes() {
        return Ok(repo);
    }
    finish_mutation(workspace, &repo, tx, "import git refs".to_owned())
}

/// The CLI's git-import options, shared by ref import and fetch:
/// `git.abandon-unreachable-commits`, `git.record-synthetic-predecessors`
/// and each remote's `remotes.<name>.auto-track-bookmarks`.
fn git_import_options(settings: &UserSettings) -> Result<git::GitImportOptions, BackendError> {
    let config_err = |name: &str, err: &dyn std::fmt::Display| {
        BackendError::ConfigInvalid(format!("{name}: {err}"))
    };
    let git_settings =
        git::GitSettings::from_settings(settings).map_err(|err| config_err("git", &err))?;
    let mut remote_auto_track_bookmarks = HashMap::new();
    for remote in settings.table_keys("remotes") {
        let remote = RemoteName::new(remote);
        if let Some(expr) = remote_fetch_expression(settings, remote, "auto-track-bookmarks")? {
            remote_auto_track_bookmarks.insert(remote.to_owned(), expr.to_matcher());
        }
    }
    Ok(git::GitImportOptions {
        abandon_unreachable_commits: git_settings.abandon_unreachable_commits,
        record_synthetic_predecessors: git_settings.record_synthetic_predecessors,
        remote_auto_track_bookmarks,
    })
}

/// CLI parity for the start of every mutation: record what is on disk into
/// the working-copy commit first, as its own "snapshot working copy"
/// operation, exactly like running any jj command would. Without this,
/// mutations that move `@` (edit, abandon, new) could overwrite or
/// misattribute edits the user made since the last jj command ran.
fn snapshot_working_copy(
    workspace: &mut Workspace,
    repo: Arc<ReadonlyRepo>,
) -> Result<Arc<ReadonlyRepo>, BackendError> {
    let ws_name = workspace.workspace_name().to_owned();
    let Some(wc_commit_id) = repo.view().get_wc_commit_id(&ws_name).cloned() else {
        return Ok(repo); // nothing checked out in this workspace
    };
    let wc_commit = repo.store().get_commit(&wc_commit_id).map_err(snapshot_err)?;

    let settings = workspace.settings().clone();
    let workspace_root = workspace.workspace_root().to_owned();
    let base_ignores = base_ignores(repo.store()).map_err(mutation_err)?;
    let config_err = |name: &str, err: &dyn std::fmt::Display| {
        BackendError::ConfigInvalid(format!("{name}: {err}"))
    };
    let auto_track = settings
        .get_string("snapshot.auto-track")
        .map_err(|err| config_err("snapshot.auto-track", &err))?;
    let path_converter = RepoPathUiConverter::Fs {
        cwd: workspace_root.clone(),
        base: workspace_root,
    };
    let fileset_aliases = FilesetAliasesMap::new();
    let fileset_ctx = FilesetParseContext {
        aliases_map: &fileset_aliases,
        path_converter: &path_converter,
    };
    let start_tracking = fileset::parse(&mut FilesetDiagnostics::new(), &auto_track, &fileset_ctx)
        .map_err(|err| config_err("snapshot.auto-track", &err))?
        .to_matcher();
    let HumanByteSize(max_new_file_size) = settings
        .get_value_with("snapshot.max-new-file-size", TryInto::try_into)
        .map_err(|err| config_err("snapshot.max-new-file-size", &err))?;

    let mut locked_ws = pollster::block_on(workspace.start_working_copy_mutation())
        .map_err(mutation_err)?;
    // The on-disk working copy must agree with the repo we loaded: same
    // operation, or at least the same tree (the recorded operation lags
    // behind after mutations that never touched `@`). Anything else means
    // another client moved the checkout — bail instead of mixing states.
    if locked_ws.locked_wc().old_operation_id() != repo.op_id()
        && locked_ws.locked_wc().old_tree().tree_ids() != wc_commit.tree_ids()
    {
        return Err(BackendError::StaleWorkspace(
            "the repo has moved since this workspace was last updated; \
             update the workspace from the Conflicts section first"
                .into(),
        ));
    }

    let options = SnapshotOptions {
        base_ignores,
        progress: None,
        start_tracking_matcher: start_tracking.as_ref(),
        force_tracking_matcher: &NothingMatcher,
        max_new_file_size,
    };
    let (new_tree, stats) =
        pollster::block_on(locked_ws.locked_wc().snapshot(&options)).map_err(mutation_err)?;
    if !stats.untracked_paths.is_empty() {
        tracing::warn!(
            count = stats.untracked_paths.len(),
            "files left untracked by working-copy snapshot"
        );
    }

    let repo = if new_tree.tree_ids() != wc_commit.tree_ids() {
        let mut tx = repo.start_transaction();
        tx.set_is_snapshot(true);
        pollster::block_on(
            tx.repo_mut()
                .rewrite_commit(&wc_commit)
                .set_tree(new_tree)
                .write(),
        )
        .map_err(mutation_err)?;
        pollster::block_on(tx.repo_mut().rebase_descendants()).map_err(mutation_err)?;
        pollster::block_on(tx.commit("snapshot working copy")).map_err(mutation_err)?
    } else {
        repo
    };
    pollster::block_on(locked_ws.finish(repo.op_id().clone()))
        .map_err(mutation_err)?;
    Ok(repo)
}

/// Global and repo-level gitignores, mirroring the CLI's base ignores:
/// `core.excludesFile` (defaulting to `~/.config/git/ignore`) plus the git
/// repo's `info/exclude`. Repos without a git backend get none.
fn base_ignores(store: &Arc<Store>) -> Result<Arc<GitIgnoreFile>, GitIgnoreError> {
    let mut ignores = GitIgnoreFile::empty();
    let Ok(git_backend) = git::get_git_backend(store) else {
        return Ok(ignores);
    };
    let excludes_file = git_backend
        .git_repo()
        .config_snapshot()
        .string("core.excludesFile")
        .and_then(|value| {
            std::str::from_utf8(&value)
                .ok()
                .map(jj_lib::file_util::expand_home_path)
        })
        .or_else(|| {
            let base = std::env::var_os("XDG_CONFIG_HOME")
                .map(std::path::PathBuf::from)
                .filter(|p| p.is_absolute())
                .or_else(|| std::env::home_dir().map(|home| home.join(".config")))?;
            Some(base.join("git").join("ignore"))
        });
    if let Some(path) = excludes_file {
        ignores = ignores.chain_with_file(RepoPath::root(), path)?;
    }
    ignores.chain_with_file(
        RepoPath::root(),
        git_backend.git_repo_path().join("info").join("exclude"),
    )
}

/// Shared by create and rename. jj itself accepts almost any string as a
/// bookmark name, but names with whitespace or remote/revset syntax could
/// not be typed back into the CLI or exported as git refs; refusing them
/// up front beats a confusing failure later.
fn validate_bookmark_name(name: &str) -> Result<(), BackendError> {
    if name.is_empty() {
        return Err(BackendError::MutationFailed(
            "bookmark name cannot be empty".into(),
        ));
    }
    if name.chars().any(char::is_whitespace) || name.contains('@') || name.contains(':') {
        return Err(BackendError::MutationFailed(format!(
            "\u{201c}{name}\u{201d} is not a usable bookmark name \
             (no spaces, \u{201c}@\u{201d}, or \u{201c}:\u{201d})"
        )));
    }
    Ok(())
}

/// The `name@remote` symbol remote-bookmark mutations act on, validated:
/// jj's hidden colocated `git` remote refuses, and the remote bookmark
/// must exist in the view (which is what the fetch left there — an unknown
/// symbol is a stale plan).
fn validated_remote_symbol<'a>(
    repo: &dyn Repo,
    name: &'a str,
    remote: &'a str,
) -> Result<RemoteRefSymbol<'a>, BackendError> {
    if RemoteName::new(remote) == REMOTE_NAME_FOR_LOCAL_GIT_REPO {
        return Err(BackendError::MutationFailed(
            "the \u{201c}git\u{201d} remote is jj's internal name for the backing \
             git repository"
                .to_owned(),
        ));
    }
    let symbol = RemoteRefSymbol {
        name: RefName::new(name),
        remote: RemoteName::new(remote),
    };
    if repo.view().get_remote_bookmark(symbol).target.is_absent() {
        return Err(BackendError::MutationFailed(format!(
            "there is no remote bookmark {name}@{remote}"
        )));
    }
    Ok(symbol)
}

/// The CLI's default push remote: the `git.push` setting when configured,
/// else the repo's sole remote, else literally "origin" — which may not
/// exist; the caller's existence check reports that the same way the CLI
/// errors with "No git remote named 'origin'".
/// CLI parity for which remotes plain `jj git fetch` reads: the `git.fetch`
/// setting as a list or single string (entries parse as string patterns,
/// glob by default), else the repo's sole remote, else literally "origin".
fn default_fetch_remotes(
    settings: &UserSettings,
    all_remotes: &[jj_lib::ref_name::RemoteNameBuf],
) -> Result<StringExpression, BackendError> {
    const KEY: &str = "git.fetch";
    if let Ok(names) = settings.get::<Vec<String>>(KEY) {
        return parse_name_patterns(&names, KEY);
    }
    match settings.get_string(KEY) {
        Ok(name) => return parse_name_patterns(std::slice::from_ref(&name), KEY),
        Err(ConfigGetError::NotFound { .. }) => {}
        Err(err) => return Err(BackendError::ConfigInvalid(format!("{KEY}: {err}"))),
    }
    if let [only] = all_remotes {
        return Ok(StringExpression::exact(only.as_str()));
    }
    Ok(StringExpression::exact("origin"))
}

/// Union of string patterns, the CLI's `parse_union_name_patterns`: each
/// text parses with the revset string-expression syntax (`glob:` by
/// default, `exact:`/negation supported).
fn parse_name_patterns(texts: &[String], key: &str) -> Result<StringExpression, BackendError> {
    let mut diagnostics = RevsetDiagnostics::new();
    let expressions = texts
        .iter()
        .map(|text| revset::parse_string_expression(&mut diagnostics, text))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|err| BackendError::ConfigInvalid(format!("{key}: {err}")))?;
    Ok(StringExpression::union_all(expressions))
}

/// The `remotes.<name>.fetch-bookmarks` / `fetch-tags` settings (jj ≥
/// 0.41's per-remote fetch selection), parsed with the CLI's string
/// expression syntax. The jj-lib we build against predates the typed
/// fields, so they are read straight from config.
fn remote_fetch_expression(
    settings: &UserSettings,
    remote: &RemoteName,
    key: &str,
) -> Result<Option<StringExpression>, BackendError> {
    let text = match settings.get_string(["remotes", remote.as_str(), key]) {
        Ok(text) => text,
        Err(ConfigGetError::NotFound { .. }) => return Ok(None),
        Err(err) => {
            return Err(BackendError::ConfigInvalid(format!(
                "remotes.{}.{key}: {err}",
                remote.as_str()
            )))
        }
    };
    let mut diagnostics = RevsetDiagnostics::new();
    revset::parse_string_expression(&mut diagnostics, &text)
        .map(Some)
        .map_err(|err| {
            BackendError::ConfigInvalid(format!("remotes.{}.{key}: {err}", remote.as_str()))
        })
}

fn default_push_remote(
    settings: &UserSettings,
    store: &Arc<Store>,
) -> Result<String, BackendError> {
    match settings.get_string("git.push") {
        Ok(name) => return Ok(name),
        Err(ConfigGetError::NotFound { .. }) => {}
        Err(err) => return Err(BackendError::ConfigInvalid(format!("git.push: {err}"))),
    }
    let remotes = git_remotes(store);
    if let [only] = &remotes[..] {
        return Ok(only.name.clone());
    }
    Ok("origin".to_owned())
}

/// CLI parity for `jj git push`'s commit sanity checks: every commit the
/// push would put on the remote — the range from the remote's current
/// positions up to the pushed targets — must have a description, an author
/// and committer, and no conflicts. Wordings match the CLI's "Won't push
/// commit … since it …". (`git.private-commits` defaults to `none()` and is
/// not modeled.)
fn check_commits_ready_to_push(
    repo: &dyn Repo,
    remote: &RemoteName,
    updates: &[(&str, MergeDiff<Option<CommitId>>)],
) -> Result<(), BackendError> {
    let new_heads: Vec<CommitId> = updates
        .iter()
        .filter_map(|(_, update)| update.after.clone())
        .collect();
    if new_heads.is_empty() {
        return Ok(()); // pure deletions put nothing new on the remote
    }
    let old_heads: Vec<CommitId> = repo
        .view()
        .remote_bookmarks(remote)
        .flat_map(|(_, remote_ref)| remote_ref.target.added_ids().cloned().collect::<Vec<_>>())
        .collect();
    let expr = RevsetExpression::commits(old_heads)
        .union(&RevsetExpression::root())
        .range(&RevsetExpression::commits(new_heads));
    let revset = expr.evaluate(repo).map_err(snapshot_err)?;
    for commit_id in revset.iter_blocking() {
        let commit_id = commit_id.map_err(snapshot_err)?;
        let commit = repo.store().get_commit(&commit_id).map_err(snapshot_err)?;
        let reason = if commit.description().trim().is_empty() {
            "it has no description"
        } else if commit.author().name.is_empty()
            || commit.author().email.is_empty()
            || commit.committer().name.is_empty()
            || commit.committer().email.is_empty()
        {
            "it has no author and/or committer set"
        } else if commit.has_conflict() {
            "it has conflicts"
        } else {
            continue;
        };
        return Err(BackendError::MutationFailed(format!(
            "Won't push commit {} since {reason}",
            &commit.id().hex()[..12]
        )));
    }
    Ok(())
}

/// "refs/heads/feature" → "feature", for reporting push results by the
/// bookmark name the user knows.
fn bookmark_of_git_ref(qualified: &str) -> String {
    qualified
        .strip_prefix("refs/heads/")
        .unwrap_or(qualified)
        .to_owned()
}

/// The force-with-lease refusal: the remote no longer sits where the last
/// fetch recorded it, so pushing would overwrite someone else's move.
fn remote_moved_error(
    rejected: &[String],
    remote_name: &str,
    pushed: Option<&[String]>,
) -> BackendError {
    let refused = rejected.join(", ");
    let message = match pushed {
        Some(pushed) if !pushed.is_empty() => format!(
            "Pushed {} to {remote_name}, but {refused} moved on the remote since the \
             last fetch; fetch, reconcile, and push again",
            pushed.join(", ")
        ),
        _ => format!(
            "{refused} moved on the remote since the last fetch; fetch, reconcile, \
             and push again"
        ),
    };
    BackendError::MutationFailed(message)
}

/// Push progress has no terminal to stream to; sideband messages (which can
/// include credential-helper chatter) go to the log.
struct QuietGitCallback;

impl GitSubprocessCallback for QuietGitCallback {
    fn needs_progress(&self) -> bool {
        false
    }

    fn progress(&mut self, _progress: &GitProgress) -> std::io::Result<()> {
        Ok(())
    }

    fn local_sideband(
        &mut self,
        message: &[u8],
        _term: Option<GitSidebandLineTerminator>,
    ) -> std::io::Result<()> {
        tracing::debug!(message = %String::from_utf8_lossy(message), "git push");
        Ok(())
    }

    fn remote_sideband(
        &mut self,
        message: &[u8],
        _term: Option<GitSidebandLineTerminator>,
    ) -> std::io::Result<()> {
        tracing::debug!(message = %String::from_utf8_lossy(message), "git push (remote)");
        Ok(())
    }
}

/// Run one git subcommand against the repo's backing git store, built the
/// way jj-lib's own subprocess layer builds its commands (same executable
/// setting, `LC_ALL=C` for parseable output, fsmonitor and submodule
/// recursion forced off). Answers stdout; a failed run answers the trimmed
/// stderr. Only for the operations jj-lib's fetch/push engines cannot
/// express — today that is fetching a PR's `refs/pull/N/head`.
fn run_git(
    options: &git::GitSubprocessOptions,
    git_dir: &Path,
    args: &[&str],
) -> Result<String, String> {
    let mut cmd = std::process::Command::new(&options.executable_path);
    cmd.args(["-c", "core.fsmonitor=false", "-c", "submodule.recurse=false"])
        .arg("--git-dir")
        .arg(git_dir)
        .args(args)
        .env("LC_ALL", "C")
        .stdin(std::process::Stdio::null());
    cmd.envs(&options.environment);
    let output = cmd
        .output()
        .map_err(|err| format!("could not run git ({}): {err}", options.executable_path.display()))?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_owned())
    }
}

/// How `jj squash` combines descriptions without an editor: an empty side
/// yields the other, two real descriptions concatenate destination-first.
fn combined_description(destination: &str, source: &str) -> String {
    let destination = destination.trim();
    let source = source.trim();
    let combined = if destination.is_empty() {
        source.to_owned()
    } else if source.is_empty() {
        destination.to_owned()
    } else {
        format!("{destination}\n\n{source}")
    };
    complete_newline(&combined)
}

/// Copy/rename records from each parent to this commit, so a moved file
/// diffs source-against-target (like `jj diff`) instead of bloating into
/// a full remove + add pair.
fn change_copy_records(
    repo: &Arc<ReadonlyRepo>,
    commit: &Commit,
) -> Result<CopyRecords, BackendError> {
    let mut records = CopyRecords::default();
    for parent_id in commit.parent_ids() {
        let stream = repo
            .store()
            .get_copy_records(None, parent_id, commit.id())
            .map_err(snapshot_err)?;
        let collected: Vec<_> = pollster::block_on(stream.collect());
        let collected: Vec<_> = collected.into_iter().collect::<Result<_, _>>().map_err(snapshot_err)?;
        records.add_records(collected);
    }
    Ok(records)
}

/// Status (and rename/copy source) for one entry of a copy-aware diff
/// stream. `before` is the source side for renames and copies, so absence
/// checks still classify adds and removes correctly.
fn entry_status(
    path: &CopiesTreeDiffEntryPath,
    before_absent: bool,
    after_absent: bool,
) -> (FileStatus, Option<String>) {
    if before_absent {
        return (FileStatus::Added, None);
    }
    if after_absent {
        return (FileStatus::Removed, None);
    }
    match &path.source {
        Some((source, operation)) => {
            let status = match operation {
                CopyOperation::Rename => FileStatus::Renamed,
                CopyOperation::Copy => FileStatus::Copied,
            };
            (status, Some(source.as_internal_file_string().to_owned()))
        }
        None => (FileStatus::Modified, None),
    }
}

fn build_change_detail(
    repo: &Arc<ReadonlyRepo>,
    change_id: &str,
) -> Result<ChangeDetail, BackendError> {
    let repo_ref: &dyn Repo = repo.as_ref();
    let commit = resolve_change_commit(repo, change_id)?;

    let parent_tree = pollster::block_on(commit.parent_tree(repo_ref)).map_err(snapshot_err)?;
    let tree = commit.tree();
    let copy_records = change_copy_records(repo, &commit)?;
    let mut stream = parent_tree.diff_stream_with_copies(&tree, &EverythingMatcher, &copy_records);

    let mut files = Vec::new();
    let mut diffed_paths: Vec<RepoPathBuf> = Vec::new();
    let mut truncated = false;
    pollster::block_on(async {
        while let Some(entry) = stream.next().await {
            if files.len() >= MAX_CHANGED_FILES {
                truncated = true;
                break;
            }
            let values = entry.values.map_err(snapshot_err)?;
            let (status, renamed_from) = entry_status(
                &entry.path,
                values.before.is_absent(),
                values.after.is_absent(),
            );
            diffed_paths.push(entry.path.target().to_owned());
            files.push(ChangedFile {
                path: entry.path.target().as_internal_file_string().to_owned(),
                status,
                renamed_from,
                has_conflict: !values.after.is_resolved(),
            });
        }
        Ok::<_, BackendError>(())
    })?;

    // Conflicted paths the parent-relative diff never mentions (inherited
    // or merge-created conflicts) still belong in the list — the same
    // appending `diff_trees` performs, so the file tree and the diff cards
    // agree on what a conflicted change shows.
    let diffed: HashSet<RepoPathBuf> = diffed_paths.iter().cloned().collect();
    let mut extra: Vec<(RepoPathBuf, ChangedFile)> = Vec::new();
    for (path, value) in tree.conflicts() {
        let value = value.map_err(snapshot_err)?;
        if diffed.contains(&path) || !file_only_conflict(&value) {
            continue;
        }
        if files.len() + extra.len() >= MAX_CHANGED_FILES {
            truncated = true;
            break;
        }
        let file = ChangedFile {
            path: path.as_internal_file_string().to_owned(),
            status: FileStatus::Modified,
            renamed_from: None,
            has_conflict: true,
        };
        extra.push((path, file));
    }
    let files = merge_by_path(files, &diffed_paths, extra);
    if truncated {
        tracing::warn!(change_id, limit = MAX_CHANGED_FILES, "changed-file list truncated");
    }

    Ok(ChangeDetail {
        id: change_id.to_owned(),
        files,
        truncated,
    })
}

fn build_change_diff(
    repo: &Arc<ReadonlyRepo>,
    change_id: &str,
) -> Result<ChangeDiff, BackendError> {
    let repo_ref: &dyn Repo = repo.as_ref();
    let commit = resolve_change_commit(repo, change_id)?;
    let parent_tree = pollster::block_on(commit.parent_tree(repo_ref)).map_err(snapshot_err)?;
    let copy_records = change_copy_records(repo, &commit)?;
    diff_trees(repo, change_id.to_owned(), None, parent_tree, commit.tree(), copy_records)
}

/// `jj diff --from <rev> --to <rev>`: the two changes' trees compared
/// directly, with copy records traced between those two commits.
fn build_compare_diff(
    repo: &Arc<ReadonlyRepo>,
    from_change_id: &str,
    to_change_id: &str,
) -> Result<ChangeDiff, BackendError> {
    let from = resolve_change_commit(repo, from_change_id)?;
    let to = resolve_change_commit(repo, to_change_id)?;
    let mut copy_records = CopyRecords::default();
    let stream = repo
        .store()
        .get_copy_records(None, from.id(), to.id())
        .map_err(snapshot_err)?;
    let collected: Vec<_> = pollster::block_on(stream.collect());
    let collected: Vec<_> = collected.into_iter().collect::<Result<_, _>>().map_err(snapshot_err)?;
    copy_records.add_records(collected);
    diff_trees(
        repo,
        to_change_id.to_owned(),
        Some(from_change_id.to_owned()),
        from.tree(),
        to.tree(),
        copy_records,
    )
}

/// `jj evolog`: the change's current commit followed by the predecessors
/// it was rewritten from, newest first, each annotated with the operation
/// that recorded it when the op walk still knows it (entries reconstructed
/// from legacy commit metadata carry no operation).
fn build_change_evolog(
    repo: &Arc<ReadonlyRepo>,
    change_id: &str,
) -> Result<ChangeEvolog, BackendError> {
    let commit = resolve_change_commit(repo, change_id)?;
    let mut entries = Vec::new();
    let mut truncated = false;
    for entry in blocking_iter(evolution::walk_predecessors(repo, std::slice::from_ref(commit.id()))) {
        if entries.len() >= MAX_EVOLOG_ENTRIES {
            truncated = true;
            break;
        }
        let entry = entry.map_err(snapshot_err)?;
        let (op_id, op_description, op_timestamp) = match &entry.operation {
            Some(op) => {
                let metadata = op.metadata();
                (
                    Some(short_operation_id(op.id())),
                    Some(metadata.description.clone()),
                    Some(format_timestamp(&metadata.time.end)),
                )
            }
            None => (None, None, None),
        };
        entries.push(EvologEntry {
            commit_id: short_commit_id(repo.as_ref(), entry.commit.id()),
            description: entry.commit.description().trim().to_owned(),
            author: author_display(&entry.commit),
            timestamp: format_timestamp(&entry.commit.committer().timestamp),
            op_id,
            op_description,
            op_timestamp,
        });
    }
    Ok(ChangeEvolog {
        id: change_id.to_owned(),
        entries,
        truncated,
    })
}

/// `jj file annotate`: which change introduced each line of one file as it
/// stands in `change_id`'s tree. The attribution walk (jj-lib's annotator)
/// runs over the whole visible graph, so every line resolves to a visible
/// commit — addressed like `GraphNode::id` so the pane can jump to it.
/// Refusals are curated for a GUI pane rather than CLI parity: binary and
/// oversized files have nothing line-shaped to attribute, and a conflicted
/// file is refused instead of annotating materialized conflict markers the
/// diff surface never shows (the CLI does the latter).
fn build_file_annotation(
    repo: &Arc<ReadonlyRepo>,
    change_id: &str,
    file_path: &str,
) -> Result<FileAnnotation, BackendError> {
    let commit = resolve_change_commit(repo, change_id)?;
    let repo_path = RepoPath::from_internal_string(file_path).map_err(|err| {
        BackendError::SnapshotFailed(format!("not a valid repository path: {err}"))
    })?;
    let tree = commit.tree();
    let value = pollster::block_on(tree.path_value(repo_path)).map_err(snapshot_err)?;
    let materialized = pollster::block_on(jj_lib::conflicts::materialize_tree_value(
        repo.store(),
        repo_path,
        value,
        tree.labels(),
    ))
    .map_err(snapshot_err)?;
    let mut file = match materialized {
        MaterializedTreeValue::File(file) => file,
        MaterializedTreeValue::Absent => {
            return Err(BackendError::SnapshotFailed(format!(
                "{file_path} does not exist in {change_id}"
            )))
        }
        MaterializedTreeValue::FileConflict(_) | MaterializedTreeValue::OtherConflict { .. } => {
            return Err(BackendError::SnapshotFailed(format!(
                "{file_path} is conflicted in {change_id}; conflict markers have no \
                 single origin — resolve the conflict before annotating"
            )))
        }
        _ => {
            return Err(BackendError::SnapshotFailed(format!(
                "{file_path} is not a regular file in {change_id}"
            )))
        }
    };
    let content = pollster::block_on(file.read_all(repo_path)).map_err(snapshot_err)?;
    if content.contains(&0) {
        return Err(BackendError::SnapshotFailed(format!(
            "{file_path} is binary; there are no lines to annotate"
        )));
    }
    if content.len() > MAX_DIFF_FILE_BYTES {
        return Err(BackendError::SnapshotFailed(format!(
            "{file_path} is too large to annotate"
        )));
    }

    let mut annotator = FileAnnotator::with_file_content(commit.id(), repo_path, content);
    let domain: Arc<ResolvedRevsetExpression> = RevsetExpression::visible_heads().ancestors();
    pollster::block_on(annotator.compute(repo.as_ref(), &domain)).map_err(snapshot_err)?;
    let annotation = annotator.to_annotation();

    let mut origin_index: HashMap<CommitId, u32> = HashMap::new();
    let mut origins: Vec<AnnotateOrigin> = Vec::new();
    let mut lines: Vec<AnnotateLine> = Vec::new();
    let mut truncated = false;
    for (origin, line) in annotation.line_origins() {
        if lines.len() >= MAX_ANNOTATE_LINES {
            truncated = true;
            break;
        }
        // Ok: introduced within the domain. Err: the walk stopped at this
        // commit — unreachable with the whole visible graph as the domain,
        // but attributing to the boundary commit is still the honest answer.
        let commit_id = match origin {
            Ok(origin) => &origin.commit_id,
            Err(origin) => &origin.commit_id,
        };
        let index = match origin_index.get(commit_id) {
            Some(index) => *index,
            None => {
                let origin_commit =
                    repo.store().get_commit(commit_id).map_err(snapshot_err)?;
                let index = origins.len() as u32;
                origins.push(AnnotateOrigin {
                    id: node_id(repo.as_ref(), &origin_commit),
                    change_id: short_change_id(repo.as_ref(), &origin_commit),
                    commit_id: short_commit_id(repo.as_ref(), origin_commit.id()),
                    summary: origin_commit
                        .description()
                        .lines()
                        .next()
                        .unwrap_or_default()
                        .trim()
                        .to_owned(),
                    author: author_display(&origin_commit),
                    timestamp: format_timestamp(&origin_commit.committer().timestamp),
                });
                origin_index.insert(commit_id.clone(), index);
                index
            }
        };
        // Like `content_lines`: the trailing newline is line shape, not
        // line content ('\r' stays, matching the diff surface).
        let raw: &[u8] = line.as_ref();
        let text = raw.strip_suffix(b"\n").unwrap_or(raw);
        lines.push(AnnotateLine {
            origin: index,
            text: String::from_utf8_lossy(text).into_owned(),
        });
    }
    Ok(FileAnnotation {
        id: change_id.to_owned(),
        path: file_path.to_owned(),
        origins,
        lines,
        truncated,
    })
}

/// The pickaxe (`jj log -S` semantics): walk visible history newest first
/// and report the changes where some file's occurrence count of `query`
/// differs between the two sides of the commit's parent diff — the changes
/// that introduced or removed the string, skipping ones that only moved it
/// within a file. A path containing the query also matches, so the same
/// walk answers "which change touched this file". Bounded three ways
/// (commits, hits, wall clock); the result reports how far it got.
fn build_history_search(
    repo: &Arc<ReadonlyRepo>,
    query: &str,
) -> Result<HistorySearch, BackendError> {
    let repo_ref: &dyn Repo = repo.as_ref();
    let needle: Vec<u8> = query.trim().bytes().map(|b| b.to_ascii_lowercase()).collect();
    let mut result = HistorySearch {
        query: query.to_owned(),
        hits: Vec::new(),
        scanned: 0,
        truncated: false,
    };
    if needle.is_empty() {
        return Ok(result);
    }

    // Conflicted files materialize with markers, same as the diff surface,
    // so a search finds what the reader of that diff would see.
    let materialize_options = ConflictMaterializeOptions {
        marker_style: ConflictMarkerStyle::Diff,
        marker_len: None,
        merge: MergeOptions::from_settings(repo.settings()).map_err(snapshot_err)?,
    };

    let expr: Arc<ResolvedRevsetExpression> = RevsetExpression::visible_heads().ancestors();
    let revset = expr.evaluate(repo_ref).map_err(snapshot_err)?;
    let deadline = std::time::Instant::now() + SEARCH_TIME_BUDGET;
    for id in revset.iter_blocking() {
        if result.scanned as usize >= MAX_SEARCH_COMMITS
            || result.hits.len() >= MAX_SEARCH_HITS
            || std::time::Instant::now() >= deadline
        {
            // The iterator still had this commit to offer, so the walk is
            // reporting a prefix of history, not all of it.
            result.truncated = true;
            break;
        }
        let id = id.map_err(snapshot_err)?;
        let commit = repo.store().get_commit(&id).map_err(snapshot_err)?;
        result.scanned += 1;
        let (hit, timed_out) =
            search_commit(repo, &commit, &needle, &materialize_options, deadline)?;
        if let Some(hit) = hit {
            result.hits.push(hit);
        }
        if timed_out {
            result.truncated = true;
            break;
        }
    }
    Ok(result)
}

/// One commit's part of the pickaxe: diff it against its parent tree(s)
/// (copy-aware, so a pure rename pairs up and nets out) and count the
/// needle on both sides of every entry. Answers the hit (if any) plus
/// whether the deadline expired mid-commit — a giant commit must not hold
/// the whole search past its budget.
fn search_commit(
    repo: &Arc<ReadonlyRepo>,
    commit: &Commit,
    needle: &[u8],
    materialize_options: &ConflictMaterializeOptions,
    deadline: std::time::Instant,
) -> Result<(Option<SearchHit>, bool), BackendError> {
    let repo_ref: &dyn Repo = repo.as_ref();
    let parent_tree = pollster::block_on(commit.parent_tree(repo_ref)).map_err(snapshot_err)?;
    let tree = commit.tree();
    let copy_records = change_copy_records(repo, commit)?;
    let tree_diff = parent_tree.diff_stream_with_copies(&tree, &EverythingMatcher, &copy_records);
    let labels = MergeDiff::new(parent_tree.labels(), tree.labels());
    let mut stream = std::pin::pin!(materialized_diff_stream(repo.store(), tree_diff, labels));

    let mut files: Vec<SearchFileHit> = Vec::new();
    let mut more_files = 0u32;
    let mut examined = 0usize;
    let mut timed_out = false;
    pollster::block_on(async {
        while let Some(entry) = stream.next().await {
            if examined >= MAX_CHANGED_FILES || std::time::Instant::now() >= deadline {
                timed_out = std::time::Instant::now() >= deadline;
                break;
            }
            examined += 1;
            let values = entry.values.map_err(snapshot_err)?;
            let path = entry.path.target().as_internal_file_string().to_owned();
            let path_match = count_occurrences_ci(path.as_bytes(), needle) > 0;
            let left = pollster::block_on(git_diff_part(entry.path.target(), values.before, materialize_options))
                .map_err(snapshot_err)?;
            let right = pollster::block_on(git_diff_part(entry.path.target(), values.after, materialize_options))
                .map_err(snapshot_err)?;
            // Binary and oversized files count by path only — like the diff
            // surface, their contents are not text to search.
            let countable = |part: &GitDiffPart| {
                !part.content.is_binary && part.content.contents.len() <= MAX_DIFF_FILE_BYTES
            };
            let (added, removed) = if countable(&left) && countable(&right) {
                (
                    count_occurrences_ci(right.content.contents.as_ref(), needle),
                    count_occurrences_ci(left.content.contents.as_ref(), needle),
                )
            } else {
                (0, 0)
            };
            if path_match || added != removed {
                if files.len() < MAX_SEARCH_FILES {
                    files.push(SearchFileHit {
                        path,
                        added,
                        removed,
                        path_match,
                    });
                } else {
                    more_files += 1;
                }
            }
        }
        Ok::<_, BackendError>(())
    })?;
    if files.is_empty() {
        return Ok((None, timed_out));
    }
    let hit = SearchHit {
        id: node_id(repo_ref, commit),
        change_id: short_change_id(repo_ref, commit),
        commit_id: short_commit_id(repo_ref, commit.id()),
        description: commit.description().trim().to_owned(),
        author: author_display(commit),
        timestamp: format_timestamp(&commit.committer().timestamp),
        files,
        more_files,
    };
    Ok((Some(hit), timed_out))
}

/// Non-overlapping occurrences of `needle` in `haystack`, matched with
/// ASCII case folded (the needle arrives pre-lowercased). Non-ASCII bytes
/// compare exactly, so a UTF-8 needle still matches its literal bytes.
fn count_occurrences_ci(haystack: &[u8], needle: &[u8]) -> u32 {
    if needle.is_empty() || haystack.len() < needle.len() {
        return 0;
    }
    let mut count = 0u32;
    let mut i = 0;
    while i + needle.len() <= haystack.len() {
        if haystack[i].to_ascii_lowercase() == needle[0]
            && haystack[i..i + needle.len()]
                .iter()
                .zip(needle)
                .all(|(h, n)| h.to_ascii_lowercase() == *n)
        {
            count += 1;
            i += needle.len();
        } else {
            i += 1;
        }
    }
    count
}

/// Streams one tree-to-tree diff into renderable per-file content, under
/// the same file and line budgets regardless of which comparison asked.
fn diff_trees(
    repo: &Arc<ReadonlyRepo>,
    id: String,
    from: Option<String>,
    from_tree: jj_lib::merged_tree::MergedTree,
    to_tree: jj_lib::merged_tree::MergedTree,
    copy_records: CopyRecords,
) -> Result<ChangeDiff, BackendError> {
    let tree_diff = from_tree.diff_stream_with_copies(&to_tree, &EverythingMatcher, &copy_records);
    let labels = MergeDiff::new(from_tree.labels(), to_tree.labels());

    // Conflicted files materialize with jj's standard markers so the diff
    // shows what the CLI user would see in the working copy.
    let materialize_options = ConflictMaterializeOptions {
        marker_style: ConflictMarkerStyle::Diff,
        marker_len: None,
        merge: MergeOptions::from_settings(repo.settings()).map_err(snapshot_err)?,
    };

    let mut stream = std::pin::pin!(materialized_diff_stream(
        repo.store(),
        tree_diff,
        labels
    ));

    let mut files = Vec::new();
    let mut diffed_paths: Vec<RepoPathBuf> = Vec::new();
    let mut truncated = false;
    let mut line_budget = MAX_DIFF_TOTAL_LINES;
    let mut image_budget = MAX_IMAGE_TOTAL_BYTES;
    pollster::block_on(async {
        while let Some(entry) = stream.next().await {
            if files.len() >= MAX_CHANGED_FILES {
                truncated = true;
                break;
            }
            let values = entry.values.map_err(snapshot_err)?;
            let (status, renamed_from) = entry_status(
                &entry.path,
                values.before.is_absent(),
                values.after.is_absent(),
            );
            let has_conflict = matches!(
                values.after,
                MaterializedTreeValue::FileConflict(_) | MaterializedTreeValue::OtherConflict { .. }
            );
            let path = entry.path.target();
            let content = if line_budget <= 0 {
                FileDiffContent::Omitted
            } else {
                file_diff_content(
                    path,
                    renamed_from.as_deref(),
                    values,
                    &materialize_options,
                    &mut line_budget,
                    &mut image_budget,
                )?
            };
            diffed_paths.push(path.to_owned());
            files.push(FileDiff {
                path: path.as_internal_file_string().to_owned(),
                status,
                renamed_from,
                has_conflict,
                content,
            });
        }
        Ok::<_, BackendError>(())
    })?;

    // Conflicted paths the tree diff never mentions — a conflict inherited
    // from the parent, or a merge change whose auto-merge IS the conflict
    // (both trees hold the identical conflicted entry) — still need to be
    // seen: append them so opening a conflicted change always shows its
    // conflicts. Non-file conflicts (symlinks, deleted directories) stay
    // inbox-only, like the resolve verbs refuse them.
    let diffed: HashSet<RepoPathBuf> = diffed_paths.iter().cloned().collect();
    let mut extra: Vec<(RepoPathBuf, FileDiff)> = Vec::new();
    for (path, value) in to_tree.conflicts() {
        let value = value.map_err(snapshot_err)?;
        if diffed.contains(&path) || !file_only_conflict(&value) {
            continue;
        }
        if files.len() + extra.len() >= MAX_CHANGED_FILES {
            truncated = true;
            break;
        }
        let content = if line_budget <= 0 {
            FileDiffContent::Omitted
        } else {
            let Some(file) = pollster::block_on(try_materialize_file_conflict_value(
                repo.store(),
                &path,
                &value,
                to_tree.labels(),
            ))
            .map_err(snapshot_err)?
            else {
                continue;
            };
            conflict_diff_content(&file, &materialize_options, &mut line_budget)
                .unwrap_or_else(|| unrenderable_conflict_content(&file))
        };
        let file = FileDiff {
            path: path.as_internal_file_string().to_owned(),
            status: FileStatus::Modified,
            renamed_from: None,
            has_conflict: true,
            content,
        };
        extra.push((path, file));
    }
    let files = merge_by_path(files, &diffed_paths, extra);
    if truncated {
        tracing::warn!(change_id = id, limit = MAX_CHANGED_FILES, "diff file list truncated");
    }

    Ok(ChangeDiff {
        id,
        from,
        files,
        truncated,
    })
}

/// MIME type for paths whose extension marks an image the diff shows
/// inline; None for everything else.
fn image_mime(path: &str) -> Option<&'static str> {
    let ext = std::path::Path::new(path).extension()?.to_str()?;
    match ext.to_ascii_lowercase().as_str() {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "gif" => Some("image/gif"),
        "webp" => Some("image/webp"),
        "bmp" => Some("image/bmp"),
        "ico" => Some("image/x-icon"),
        "avif" => Some("image/avif"),
        "tif" | "tiff" => Some("image/tiff"),
        "heic" => Some("image/heic"),
        _ => None,
    }
}

/// One side of an image comparison: metadata always, bytes inlined while
/// they fit the per-side cap and the change-wide budget. None for an
/// absent side (added/removed file).
fn image_side(part: &GitDiffPart, mime: &str, image_budget: &mut usize) -> Option<ImageContent> {
    part.mode?;
    let bytes: &[u8] = part.content.contents.as_ref();
    let inline = bytes.len() <= MAX_IMAGE_INLINE_BYTES && bytes.len() <= *image_budget;
    if inline {
        *image_budget -= bytes.len();
    }
    Some(ImageContent {
        mime: mime.to_owned(),
        size: bytes.len() as u64,
        data: inline.then(|| base64::engine::general_purpose::STANDARD.encode(bytes)),
    })
}

/// Diff one file's materialized sides into renderable content, charging
/// emitted lines against the change-wide budget.
fn file_diff_content(
    path: &RepoPath,
    renamed_from: Option<&str>,
    values: MergeDiff<MaterializedTreeValue>,
    options: &ConflictMaterializeOptions,
    line_budget: &mut isize,
    image_budget: &mut usize,
) -> Result<FileDiffContent, BackendError> {
    // A conflicted new side renders as its merge structure so the resolve
    // verbs' choice is informed; binary or oversized conflicts keep the
    // marker-text pipeline below, like any other content.
    if let MaterializedTreeValue::FileConflict(conflict) = &values.after {
        if let Some(content) = conflict_diff_content(conflict, options, line_budget) {
            return Ok(content);
        }
    }
    let left = pollster::block_on(git_diff_part(path, values.before, options)).map_err(snapshot_err)?;
    let right = pollster::block_on(git_diff_part(path, values.after, options)).map_err(snapshot_err)?;
    if left.content.is_binary || right.content.is_binary {
        // Compare as pictures when every present side sits at an image
        // path. Gated on binary content so image-named text (an LFS
        // pointer, say) still diffs as the text it is; each side's MIME
        // follows its own path, so a rename across formats keeps both
        // previews honest.
        let old_mime = image_mime(renamed_from.unwrap_or(path.as_internal_file_string()));
        let new_mime = image_mime(path.as_internal_file_string());
        let old_ok = left.mode.is_none() || old_mime.is_some();
        let new_ok = right.mode.is_none() || new_mime.is_some();
        if old_ok && new_ok {
            return Ok(FileDiffContent::Image {
                old: old_mime.and_then(|mime| image_side(&left, mime, image_budget)),
                new: new_mime.and_then(|mime| image_side(&right, mime, image_budget)),
            });
        }
        return Ok(FileDiffContent::Binary {
            old_size: left.mode.map(|_| left.content.contents.len() as u64),
            new_size: right.mode.map(|_| right.content.contents.len() as u64),
        });
    }
    if left.content.contents.len() > MAX_DIFF_FILE_BYTES
        || right.content.contents.len() > MAX_DIFF_FILE_BYTES
    {
        return Ok(FileDiffContent::TooLarge);
    }

    let raw_hunks = unified_diff_hunks(
        MergeDiff::new(
            left.content.contents.as_ref(),
            right.content.contents.as_ref(),
        ),
        DIFF_CONTEXT_LINES,
        LineCompareMode::Exact,
    );

    // Hunk labels come from the side that still has content: the new
    // side, except for deletions.
    let family = crate::funcname::FuncFamily::of(path.as_internal_file_string());
    let label_old_side =
        right.content.contents.is_empty() && !left.content.contents.is_empty();
    let label_lines: Vec<&[u8]> = if family.is_some() {
        let side: &[u8] = if label_old_side {
            left.content.contents.as_ref()
        } else {
            right.content.contents.as_ref()
        };
        side.split(|&b| b == b'\n').collect()
    } else {
        Vec::new()
    };

    let mut hunks = Vec::new();
    let mut hunks_truncated = false;
    for hunk in &raw_hunks {
        if *line_budget <= 0 {
            hunks_truncated = true;
            break;
        }
        // A single hunk can dwarf the remaining budget (one giant rewrite);
        // trim its tail rather than letting it overshoot the change-wide
        // cap by thousands of lines. Numbering survives a tail trim.
        let take = (*line_budget as usize).min(hunk.lines.len());
        if take < hunk.lines.len() {
            hunks_truncated = true;
        }
        *line_budget -= take as isize;
        let context = family.and_then(|f| {
            let start = if label_old_side {
                hunk.left_line_range.start
            } else {
                hunk.right_line_range.start
            };
            crate::funcname::enclosing_symbol(&label_lines, start, f)
        });
        // Only the line structure comes from jj-lib. Its intraline tokens
        // word-diff a changed region's whole removed run against its whole
        // added run, letting matches jump across line boundaries; the
        // refinement below re-derives emphasis per paired line instead.
        let lines: Vec<(DiffLineKind, String)> = hunk
            .lines
            .iter()
            .take(take)
            .map(|(line_type, tokens)| {
                let kind = match line_type {
                    DiffLineType::Context => DiffLineKind::Context,
                    DiffLineType::Removed => DiffLineKind::Removed,
                    DiffLineType::Added => DiffLineKind::Added,
                };
                let mut text = String::new();
                for (_, chunk) in tokens {
                    text.push_str(&String::from_utf8_lossy(chunk));
                }
                if text.ends_with('\n') {
                    text.pop();
                }
                (kind, text)
            })
            .collect();
        hunks.push(DiffHunk {
            old_start: hunk.left_line_range.start as u32 + 1,
            new_start: hunk.right_line_range.start as u32 + 1,
            context,
            lines: crate::intraline::refine_lines(&lines),
        });
    }
    Ok(FileDiffContent::Text {
        hunks,
        truncated: hunks_truncated,
    })
}

/// One conflicted file value as its merge structure: resolved runs
/// interleaved with conflict regions, every side of a region shown as its
/// diff from the merge base — jj's diff-style marker semantics made
/// structural. None when the structure cannot speak for the file (a
/// binary or oversized side, or contents that merge cleanly — an
/// executable-bit-only conflict); those keep the marker-text pipeline.
fn conflict_diff_content(
    file: &MaterializedFileConflictValue,
    options: &ConflictMaterializeOptions,
    line_budget: &mut isize,
) -> Option<FileDiffContent> {
    if file
        .contents
        .iter()
        .any(|side| side.len() > MAX_DIFF_FILE_BYTES || side.contains(&0u8))
    {
        return None;
    }
    let jj_lib::files::MergeResult::Conflict(hunks) =
        jj_lib::files::merge_hunks(&file.contents, &options.merge)
    else {
        return None;
    };

    // Raw sections first: resolved runs at full length, regions with every
    // side diffed from its base. A region pairs side i with base i while
    // bases last (jj's own marker pairing), so the 2-sided case — the one
    // the take verbs act on — reads both sides against the one base.
    let mut sections = Vec::new();
    for hunk in &hunks {
        if let Some(content) = hunk.as_resolved() {
            let lines: Vec<DiffLine> = content_lines(content)
                .into_iter()
                .map(|text| DiffLine {
                    kind: DiffLineKind::Context,
                    segments: vec![DiffSegment { text, changed: false }],
                    pair: None,
                })
                .collect();
            if !lines.is_empty() {
                sections.push(ConflictSection::Resolved { lines });
            }
        } else {
            let bases: Vec<_> = hunk.removes().collect();
            let sides = hunk
                .adds()
                .enumerate()
                .map(|(index, side)| {
                    let base = bases[index.min(bases.len() - 1)];
                    let mut lines: Vec<(DiffLineKind, String)> = Vec::new();
                    for part in ContentDiff::by_line([base, side]).hunks() {
                        match part.kind {
                            DiffHunkKind::Matching => {
                                for text in content_lines(&part.contents[0]) {
                                    lines.push((DiffLineKind::Context, text));
                                }
                            }
                            DiffHunkKind::Different => {
                                for text in content_lines(&part.contents[0]) {
                                    lines.push((DiffLineKind::Removed, text));
                                }
                                for text in content_lines(&part.contents[1]) {
                                    lines.push((DiffLineKind::Added, text));
                                }
                            }
                        }
                    }
                    ConflictSideDiff {
                        label: file.labels.get_add(index).map(str::to_owned),
                        lines: crate::intraline::refine_lines(&lines),
                    }
                })
                .collect();
            sections.push(ConflictSection::Region { sides });
        }
    }
    let sections = trim_resolved_sections(sections);

    // Charge rendered line rows against the change-wide budget, dropping
    // whole sections once it runs out — a region is never cut mid-side,
    // so one giant region can overshoot like one giant hunk does.
    let mut kept = Vec::new();
    let mut truncated = false;
    for section in sections {
        if *line_budget <= 0 {
            truncated = true;
            break;
        }
        let cost = match &section {
            ConflictSection::Resolved { lines } => lines.len(),
            ConflictSection::Elided { .. } => 0,
            ConflictSection::Region { sides } => sides.iter().map(|s| s.lines.len()).sum(),
        };
        *line_budget -= cost as isize;
        kept.push(section);
    }
    Some(FileDiffContent::Conflict {
        sections: kept,
        truncated,
    })
}

/// Lines of one side's byte content; the empty piece a trailing newline
/// leaves behind is not a line.
fn content_lines(content: &[u8]) -> Vec<String> {
    if content.is_empty() {
        return Vec::new();
    }
    let mut lines: Vec<String> = content
        .split(|&b| b == b'\n')
        .map(|line| String::from_utf8_lossy(line).into_owned())
        .collect();
    if content.ends_with(b"\n") {
        lines.pop();
    }
    lines
}

/// Diff-style context trimming over a conflicted file's raw sections: a
/// resolved run keeps `DIFF_CONTEXT_LINES` on each region-facing flank
/// (the file's head and tail keep only the inner one) and an `Elided`
/// section stands in for the skipped middle.
fn trim_resolved_sections(sections: Vec<ConflictSection>) -> Vec<ConflictSection> {
    let last = sections.len().saturating_sub(1);
    let mut out = Vec::with_capacity(sections.len());
    for (index, section) in sections.into_iter().enumerate() {
        match section {
            ConflictSection::Resolved { mut lines } => {
                let head = if index == 0 { 0 } else { DIFF_CONTEXT_LINES };
                let tail = if index == last { 0 } else { DIFF_CONTEXT_LINES };
                if lines.len() <= head + tail {
                    out.push(ConflictSection::Resolved { lines });
                    continue;
                }
                let elided = (lines.len() - head - tail) as u32;
                let tail_lines = lines.split_off(lines.len() - tail);
                lines.truncate(head);
                if !lines.is_empty() {
                    out.push(ConflictSection::Resolved { lines });
                }
                out.push(ConflictSection::Elided { lines: elided });
                if !tail_lines.is_empty() {
                    out.push(ConflictSection::Resolved { lines: tail_lines });
                }
            }
            other => out.push(other),
        }
    }
    out
}

/// True when every term of a conflicted tree value is a normal file (or
/// absent) — the conflicts the diff surface can render and the resolve
/// verbs can act on. Symlink and tree conflicts stay inbox-only.
fn file_only_conflict(value: &MergedTreeValue) -> bool {
    value
        .iter()
        .all(|term| matches!(term, None | Some(TreeValue::File { .. })))
}

/// Content for an appended conflicted file the structural renderer cannot
/// speak for. Both trees hold the identical entry (that is why the diff
/// stream skipped it), so there is no old/new story to tell: an oversized
/// side reports itself, a binary side compares as nothing but bytes, and
/// contents that merge cleanly (an executable-bit-only conflict) are a
/// contentless change.
fn unrenderable_conflict_content(file: &MaterializedFileConflictValue) -> FileDiffContent {
    if file.contents.iter().any(|side| side.len() > MAX_DIFF_FILE_BYTES) {
        return FileDiffContent::TooLarge;
    }
    if file.contents.iter().any(|side| side.contains(&0u8)) {
        return FileDiffContent::Binary {
            old_size: None,
            new_size: None,
        };
    }
    FileDiffContent::Text {
        hunks: Vec::new(),
        truncated: false,
    }
}

/// Merges path-ordered appended entries into a path-ordered list.
/// `item_paths` are the existing items' repo paths, index-aligned.
fn merge_by_path<T>(
    items: Vec<T>,
    item_paths: &[RepoPathBuf],
    extra: Vec<(RepoPathBuf, T)>,
) -> Vec<T> {
    if extra.is_empty() {
        return items;
    }
    let mut merged = Vec::with_capacity(items.len() + extra.len());
    let mut extra_iter = extra.into_iter().peekable();
    for (path, item) in item_paths.iter().zip(items) {
        while extra_iter.peek().is_some_and(|(p, _)| p < path) {
            merged.push(extra_iter.next().unwrap().1);
        }
        merged.push(item);
    }
    merged.extend(extra_iter.map(|(_, item)| item));
    merged
}

/// How a `SplitSelection` partitions one change's diff, plus the tree the
/// selected side builds. Shared by `split_change` (the selection becomes
/// the carved-off half) and `squash_into` (the selection moves into
/// another change).
struct SelectedChanges {
    /// The parent tree with the selected content applied: whole selected
    /// files restored from the change's tree, then the chosen hunks of
    /// partially selected files spliced in.
    tree: MergedTree,
    parent_tree: MergedTree,
    /// Files selected whole.
    whole_files: usize,
    /// Files with a subset of their hunks selected.
    partial_files: usize,
    /// Diff entries the selection leaves behind — unselected files plus
    /// partially selected files with unchosen hunks. Zero means the
    /// selection covers every change the commit makes.
    remaining_files: usize,
}

/// Builds the selected side of a file/hunk selection over one change's
/// diff. Whole files are the `restore_tree` fast path; a hunk entry
/// rebuilds its file as the parent side's content with just the chosen
/// hunks applied, where the hunks are re-derived from the trees here and
/// matched against the coordinates the panel rendered — a mismatch means
/// the change moved since then, and rewriting it anyway could carve
/// content the user never saw. Selections that change nothing are refused.
fn select_changes(
    repo: &Arc<ReadonlyRepo>,
    commit: &Commit,
    change_id: &str,
    selection: &[SplitSelection],
) -> Result<SelectedChanges, BackendError> {
    // Partition the selection: whole files stay the restore_tree fast
    // path; hunk entries rebuild their file's content below.
    let mut whole_paths = Vec::new();
    let mut partial: Vec<(RepoPathBuf, &[SplitHunk])> = Vec::new();
    for entry in selection {
        let repo_path = RepoPathBuf::from_internal_string(entry.path.as_str()).map_err(|err| {
            BackendError::MutationFailed(format!("not a valid repository path: {err}"))
        })?;
        match entry.hunks.as_deref() {
            None => whole_paths.push(repo_path),
            Some([]) => {
                return Err(BackendError::MutationFailed(format!(
                    "no hunks selected in {}",
                    entry.path
                )));
            }
            Some(hunks) => partial.push((repo_path, hunks)),
        }
    }
    let whole_matcher = FilesMatcher::new(&whole_paths);

    // Partition the change's own diff by the selection before building
    // trees: degenerate selections are refused up front and the counts
    // feed the callers' summaries and policies. Hunk-selected paths are
    // counted below, where their hunks are matched.
    let commit_tree = commit.tree();
    let parent_tree = pollster::block_on(commit.parent_tree(repo.as_ref())).map_err(snapshot_err)?;
    let partial_paths: HashSet<RepoPathBuf> = partial.iter().map(|(p, _)| p.clone()).collect();
    let (mut whole_files, mut remaining_files) = (0usize, 0usize);
    pollster::block_on(async {
        let mut stream = parent_tree.diff_stream(&commit_tree, &EverythingMatcher);
        while let Some(entry) = stream.next().await {
            if partial_paths.contains(&entry.path) {
                continue;
            }
            if whole_matcher.matches(&entry.path) {
                whole_files += 1;
            } else {
                remaining_files += 1;
            }
        }
    });

    // Rebuild each hunk-selected file.
    let mut partial_files = 0usize;
    let mut matched: HashSet<RepoPathBuf> = HashSet::new();
    let mut partial_overrides: Vec<(RepoPathBuf, MergedTreeValue)> = Vec::new();
    if !partial.is_empty() {
        let partial_repo_paths: Vec<RepoPathBuf> = partial.iter().map(|(p, _)| p.clone()).collect();
        let partial_matcher = FilesMatcher::new(&partial_repo_paths);
        let copies = CopyRecords::default();
        let tree_diff =
            parent_tree.diff_stream_with_copies(&commit_tree, &partial_matcher, &copies);
        let labels = MergeDiff::new(parent_tree.labels(), commit_tree.labels());
        let store = repo.store();
        let mut stream = std::pin::pin!(materialized_diff_stream(store, tree_diff, labels));
        pollster::block_on(async {
            while let Some(entry) = stream.next().await {
                let values = entry.values.map_err(snapshot_err)?;
                let file_path = entry.path.target().to_owned();
                let Some((_, wanted)) = partial.iter().find(|(p, _)| *p == file_path) else {
                    continue;
                };
                let display = file_path.as_internal_file_string();
                let (before_file, before_content) =
                    split_file_side(values.before, &file_path).await?;
                let (after_file, after_content) =
                    split_file_side(values.after, &file_path).await?;
                let splice =
                    splice_hunks(&before_content, &after_content, wanted, display, true)?;
                partial_files += 1;
                matched.insert(file_path.clone());
                if !splice.all {
                    // Unchosen hunks of this file stay behind; an
                    // all-hunks selection leaves nothing of it there.
                    remaining_files += 1;
                }
                let content = splice.content;
                // The selected side keeps the parent side's file mode; a
                // mode-only flip stays behind (documented on the trait).
                let (executable, copy_id) = match (&before_file, &after_file) {
                    (Some((exec, copy)), _) | (None, Some((exec, copy))) => (*exec, copy.clone()),
                    (None, None) => {
                        return Err(BackendError::MutationFailed(format!(
                            "{display} does not change anything in this change"
                        )));
                    }
                };
                let id = store
                    .write_file(&file_path, &mut content.as_slice())
                    .await
                    .map_err(mutation_err)?;
                partial_overrides.push((
                    file_path,
                    Merge::resolved(Some(TreeValue::File {
                        id,
                        executable,
                        copy_id,
                    })),
                ));
            }
            Ok::<_, BackendError>(())
        })?;
    }
    for (p, _) in &partial {
        if !matched.contains(p) {
            return Err(BackendError::MutationFailed(format!(
                "{} does not change anything in {change_id}",
                p.as_internal_file_string()
            )));
        }
    }
    if whole_files == 0 && partial_files == 0 {
        return Err(BackendError::MutationFailed(format!(
            "none of the selected files change anything in {change_id}"
        )));
    }

    // The selected side's tree: the parent tree with the whole selected
    // paths restored from the change's tree, then the hunk-selected
    // files' rebuilt content layered on top.
    let selected_tree = pollster::block_on(restore_tree(
        &commit_tree,
        &parent_tree,
        format!("commit {}", commit.id().hex()),
        "parents".to_owned(),
        &whole_matcher,
    ))
    .map_err(mutation_err)?;
    let selected_tree = if partial_overrides.is_empty() {
        selected_tree
    } else {
        let mut builder = MergedTreeBuilder::new(selected_tree);
        for (file_path, value) in partial_overrides {
            builder.set_or_remove(file_path, value);
        }
        pollster::block_on(builder.write_tree()).map_err(mutation_err)?
    };
    Ok(SelectedChanges {
        tree: selected_tree,
        parent_tree,
        whole_files,
        partial_files,
        remaining_files,
    })
}

/// One hunk-selected file rebuilt from its freshly derived diff: the
/// parent side's content with a verified subset of the hunks applied.
struct HunkSplice {
    content: Vec<u8>,
    /// The wanted set covered every change the diff has — every hunk
    /// selected whole, or with line picks naming all its changed lines.
    all: bool,
}

/// What one freshly derived hunk contributes to the splice: nothing, its
/// whole diff, or just the picked line indices.
#[derive(Clone, Copy)]
enum HunkPick<'a> {
    Unselected,
    Whole,
    Lines(&'a [u32]),
}

/// Derives `before → after`'s unified hunks exactly as the diff surface
/// rendered them, matches `wanted` against their coordinates, and splices
/// a subset onto the parent side's content: the wanted hunks — or just
/// their picked lines — themselves (`keep_wanted`; a split carves them
/// out) or their complement (a hunk restore discards them). A coordinate
/// or line pick that no longer matches means the change moved underneath
/// the surface — refused, because rewriting anyway could touch content
/// the user never saw. Binary and oversized files are refused too: their
/// hunks were never rendered, so there are no coordinates to trust.
fn splice_hunks(
    before_content: &[u8],
    after_content: &[u8],
    wanted: &[SplitHunk],
    display: &str,
    keep_wanted: bool,
) -> Result<HunkSplice, BackendError> {
    if before_content.contains(&0) || after_content.contains(&0) {
        return Err(BackendError::MutationFailed(format!(
            "{display} is binary; hunks cannot be selected — \
             check the whole file instead"
        )));
    }
    if before_content.len() > MAX_DIFF_FILE_BYTES || after_content.len() > MAX_DIFF_FILE_BYTES {
        return Err(BackendError::MutationFailed(format!(
            "{display} is too large to split by hunk — \
             check the whole file instead"
        )));
    }
    let raw_hunks = unified_diff_hunks(
        MergeDiff::new(before_content.into(), after_content.into()),
        DIFF_CONTEXT_LINES,
        LineCompareMode::Exact,
    );
    let mut chosen = vec![HunkPick::Unselected; raw_hunks.len()];
    for want in wanted {
        let found = raw_hunks.iter().position(|hunk| {
            hunk.left_line_range.start + 1 == want.old_start as usize
                && hunk.right_line_range.start + 1 == want.new_start as usize
                && hunk.left_line_range.len() == want.old_lines as usize
                && hunk.right_line_range.len() == want.new_lines as usize
        });
        let Some(found) = found else {
            return Err(BackendError::MutationFailed(format!(
                "the diff of {display} no longer matches the selected \
                 hunks — the change moved; refresh and try again"
            )));
        };
        chosen[found] = match want.lines.as_deref() {
            None => HunkPick::Whole,
            Some([]) => {
                return Err(BackendError::MutationFailed(format!(
                    "no lines selected in a hunk of {display}"
                )));
            }
            Some(picks) => {
                for &pick in picks {
                    match raw_hunks[found].lines.get(pick as usize) {
                        None => {
                            return Err(BackendError::MutationFailed(format!(
                                "the diff of {display} no longer matches the selected \
                                 hunks — the change moved; refresh and try again"
                            )));
                        }
                        Some((DiffLineType::Context, _)) => {
                            return Err(BackendError::MutationFailed(format!(
                                "a context line of {display} was selected — only \
                                 added or removed lines carry a change to pick"
                            )));
                        }
                        Some(_) => {}
                    }
                }
                HunkPick::Lines(picks)
            }
        };
    }
    let all = chosen.iter().zip(&raw_hunks).all(|(pick, hunk)| match pick {
        HunkPick::Unselected => false,
        HunkPick::Whole => true,
        HunkPick::Lines(picks) => hunk.lines.iter().enumerate().all(|(index, (line_type, _))| {
            matches!(line_type, DiffLineType::Context) || picks.contains(&(index as u32))
        }),
    });
    // Splice the kept hunks into the parent content. Left lines split
    // \n-inclusive, matching the diff's own line ranges, so unchanged
    // stretches (including skipped hunks) copy through byte-exact.
    let left_lines: Vec<&[u8]> = before_content.split_inclusive(|b| *b == b'\n').collect();
    let mut content: Vec<u8> = Vec::with_capacity(after_content.len());
    let mut cursor = 0usize;
    for (hunk, pick) in raw_hunks.iter().zip(&chosen) {
        // Whether any of this hunk's changes make it into the output;
        // hunks contributing nothing copy through the cursor path.
        let enters = match pick {
            HunkPick::Unselected => !keep_wanted,
            HunkPick::Whole => keep_wanted,
            HunkPick::Lines(_) => true,
        };
        if !enters {
            continue;
        }
        for line in &left_lines[cursor..hunk.left_line_range.start] {
            content.extend_from_slice(line);
        }
        for (index, (line_type, tokens)) in hunk.lines.iter().enumerate() {
            // A picked line's change applies on the `keep_wanted` side and
            // stays behind on the other; unpicked lines the reverse. An
            // applied removal drops the old line, an applied addition
            // emits the new one; context always copies through.
            let applies = match pick {
                HunkPick::Unselected => !keep_wanted,
                HunkPick::Whole => keep_wanted,
                HunkPick::Lines(picks) => picks.contains(&(index as u32)) == keep_wanted,
            };
            let emit = match line_type {
                DiffLineType::Context => true,
                DiffLineType::Removed => !applies,
                DiffLineType::Added => applies,
            };
            if emit {
                for (_, text) in tokens {
                    content.extend_from_slice(text);
                }
            }
        }
        cursor = hunk.left_line_range.end;
    }
    for line in &left_lines[cursor..] {
        content.extend_from_slice(line);
    }
    Ok(HunkSplice { content, all })
}

/// How selections read in outcome summaries: "2 files", "parts of 1 file",
/// "1 file and parts of 2 more".
fn files_phrase(whole: usize, partial: usize) -> String {
    match (whole, partial) {
        (w, 0) => format!("{w} file{}", if w == 1 { "" } else { "s" }),
        (0, p) => format!("parts of {p} file{}", if p == 1 { "" } else { "s" }),
        (w, p) => format!("{w} file{} and parts of {p} more", if w == 1 { "" } else { "s" }),
    }
}

/// One side of a hunk-selected file in a split: its raw bytes plus the
/// parts a rebuilt tree entry needs. Only regular files and absent sides
/// qualify — conflicts, symlinks, and other exotic entries must be split
/// whole, because a partial hunk application over them has no meaning.
async fn split_file_side(
    value: MaterializedTreeValue,
    path: &RepoPath,
) -> Result<(Option<(bool, CopyId)>, Vec<u8>), BackendError> {
    match value {
        MaterializedTreeValue::Absent => Ok((None, Vec::new())),
        MaterializedTreeValue::File(mut file) => {
            let content = file.read_all(path).await.map_err(snapshot_err)?;
            Ok((Some((file.executable, file.copy_id)), content))
        }
        _ => Err(BackendError::MutationFailed(format!(
            "{} is not a regular file here (or is conflicted); hunks cannot \
             be selected — check the whole file instead",
            path.as_internal_file_string()
        ))),
    }
}

/// Use the same configured `trunk()` as jj's immutability boundary. Picking
/// a conventional branch independently can send a develop-based stack to
/// main, despite correctly treating develop's ancestors as immutable.
fn resolve_trunk(workspace: &Workspace, repo: &dyn Repo) -> Result<Option<TrunkRef>, BackendError> {
    let expr = resolve_revset_text(workspace, repo, "trunk()")
        .map_err(|err| BackendError::ConfigInvalid(format!("revset-aliases.\"trunk()\": {err}")))?;
    let ids = collect_revset(expr, repo, 2)?;
    let [target] = ids.as_slice() else {
        // No unambiguous destination: never fall back to an unrelated main.
        return Ok(None);
    };
    let definition = workspace
        .settings()
        .get_string(["revset-aliases", "trunk()"])
        .unwrap_or_default();
    let direct = definition.trim().trim_matches(['\'', '"']);
    let (direct_name, direct_remote) = direct.split_once('@').unwrap_or((direct, ""));
    // Definitions commonly wrap their reference in present()/latest() or
    // remote_bookmarks(exact:"develop"). Preserve that name even when a
    // conventional branch happens to point at the very same commit.
    let mentioned: HashSet<&str> = definition
        .split(|c: char| !c.is_alphanumeric() && !matches!(c, '-' | '_' | '/' | '.'))
        .collect();
    let mut candidates = Vec::new();
    for (symbol, reference) in repo.view().all_remote_bookmarks() {
        if symbol.remote != REMOTE_NAME_FOR_LOCAL_GIT_REPO
            && reference.target.as_normal() == Some(target)
        {
            candidates.push(TrunkRef {
                name: symbol.name.as_str().to_owned(),
                remote: Some(symbol.remote.as_str().to_owned()),
                target: target.clone(),
            });
        }
    }
    for (name, reference) in repo.view().local_bookmarks() {
        if reference.as_normal() == Some(target) {
            candidates.push(TrunkRef {
                name: name.as_str().to_owned(),
                remote: None,
                target: target.clone(),
            });
        }
    }
    candidates.sort_by_key(|candidate| {
        (
            candidate.name != direct_name,
            !mentioned.contains(candidate.name.as_str()),
            !direct_remote.is_empty() && candidate.remote.as_deref() != Some(direct_remote),
            !TRUNK_CANDIDATES.contains(&candidate.name.as_str()),
            candidate.remote.as_deref().map(remote_rank).unwrap_or(3),
            candidate.name.clone(),
        )
    });
    Ok(candidates.into_iter().next())
}

/// The commit landings are judged against and stacks are rebuilt on: the
/// branch `branch` as `remote` has it after the last fetch. The caller
/// names the branch, the one its plan was made for and a person
/// confirmed. `trunk()` is not asked again here. A fetch can change what
/// it answers: Jiji's default takes the latest of `main`, `master` and
/// `trunk`, so another of those moving ahead on the remote would switch
/// the destination in the middle of a run (the reason this takes a name).
/// And it can name a commit the remote does not have: a commit made on
/// the local `main` and not pushed becomes `trunk()`, while what lands,
/// lands on the remote's branch.
///
/// Without that branch from that remote there is nothing to judge
/// against, and the answer is the reason, for the caller to stop on. The
/// one exception is a repository that has nothing from `remote` at all
/// (this crate's test fixtures): it has no remote branch to be behind
/// of, and the local bookmark of that name answers.
fn landing_trunk(repo: &dyn Repo, remote: &str, branch: &str) -> Result<CommitId, String> {
    let view = repo.view();
    let symbol = RemoteRefSymbol {
        name: RefName::new(branch),
        remote: RemoteName::new(remote),
    };
    if let Some(target) = view.get_remote_bookmark(symbol).target.as_normal() {
        return Ok(target.clone());
    }
    let knows_remote = view.remote_bookmarks(RemoteName::new(remote)).next().is_some();
    match view.get_local_bookmark(RefName::new(branch)).as_normal() {
        Some(target) if !knows_remote => Ok(target.clone()),
        _ => Err(format!(
            "there is no \u{201c}{branch}\u{201d} from {remote} here to land on \u{2014} fetch, \
             and plan again"
        )),
    }
}

/// What applying the change from `from` to `to` on top of `onto` ends
/// with: the tree a rebase of those changes onto it would have.
fn reapply(
    onto: &MergedTree,
    from: &MergedTree,
    to: &MergedTree,
) -> Result<MergedTree, BackendError> {
    pollster::block_on(MergedTree::merge(Merge::from_vec(vec![
        (onto.clone(), "trunk".to_owned()),
        (from.clone(), "below the changes".to_owned()),
        (to.clone(), "the changes".to_owned()),
    ])))
    .map_err(snapshot_err)
}

/// The paths at which two trees differ, capped, and how many more there
/// are. Nothing when they are the same tree.
fn differing_paths(a: &MergedTree, b: &MergedTree) -> (Vec<String>, u32) {
    let mut paths: Vec<String> = Vec::new();
    let mut more = 0u32;
    if a.tree_ids() != b.tree_ids() {
        let mut stream = a.diff_stream(b, &EverythingMatcher);
        pollster::block_on(async {
            while let Some(entry) = stream.next().await {
                if paths.len() < MAX_LANDED_CHECK_PATHS {
                    paths.push(entry.path.as_internal_file_string().to_owned());
                } else {
                    more += 1;
                }
            }
        });
    }
    (paths, more)
}

/// How many commits of a stack `lands_as_tip` walks before it gives up.
const MAX_TIP_CHECK_COMMITS: usize = 500;

/// Where the branch that `entry` brought into trunk's line left that
/// line: the merge bases of `entry`'s first parent and `top`, the
/// branch's tip. One, usually; several when trunk and the branch merged
/// each other before. None when `entry` is `top` itself (trunk was moved
/// onto the branch and nothing was merged), or when `entry`'s first
/// parent has `top` in its history already. Then `entry` is not where the
/// branch came in (`entry_into` ran out of commits to walk), the merge
/// base would be `top` itself, and a span measured from there is empty
/// and holds whatever is asked of it.
fn branch_points(
    repo: &dyn Repo,
    entry: &Commit,
    top: &CommitId,
) -> Result<Vec<Commit>, BackendError> {
    if entry.id() == top {
        return Ok(Vec::new());
    }
    let Some(first) = entry.parent_ids().first() else {
        return Ok(Vec::new());
    };
    if is_ancestor(repo, top, first) {
        return Ok(Vec::new());
    }
    let bases = pollster::block_on(
        repo.index()
            .common_ancestors(std::slice::from_ref(first), std::slice::from_ref(top)),
    )
    .map_err(snapshot_err)?;
    bases
        .iter()
        .map(|base| repo.store().get_commit(base).map_err(snapshot_err))
        .collect()
}

/// How far back along trunk's first parents `entry_into` looks.
const MAX_ENTRY_WALK: usize = 10_000;

/// The commit on trunk's first-parent line where `commit`, an ancestor of
/// `tip`, entered it: the oldest one on that line that still has it as an
/// ancestor. For a merge commit that is the merge; for a branch trunk was
/// moved onto it is `commit` itself. A walk that runs out answers the
/// oldest commit it reached, which still has `commit` in its history.
fn entry_into(repo: &dyn Repo, commit: &CommitId, tip: &Commit) -> Result<Commit, BackendError> {
    let mut at = tip.clone();
    for _ in 0..MAX_ENTRY_WALK {
        let Some(first) = at.parent_ids().first() else {
            break;
        };
        if !is_ancestor(repo, commit, first) {
            break;
        }
        at = repo.store().get_commit(first).map_err(snapshot_err)?;
    }
    Ok(at)
}

fn remote_rank(name: &str) -> usize {
    match name {
        "origin" => 0,
        "upstream" => 1,
        _ => 2,
    }
}

fn collect_revset(
    expr: Arc<ResolvedRevsetExpression>,
    repo: &dyn Repo,
    limit: usize,
) -> Result<Vec<CommitId>, BackendError> {
    let revset = expr.evaluate(repo).map_err(snapshot_err)?;
    let mut ids = Vec::new();
    for id in revset.iter_blocking().take(limit) {
        ids.push(id.map_err(snapshot_err)?);
    }
    if ids.len() == limit {
        tracing::warn!(limit, "mutable commit set truncated for snapshot");
    }
    Ok(ids)
}

fn wc_node_kind(commit_id: &CommitId, wc_commit_id: &Option<CommitId>) -> NodeKind {
    if Some(commit_id) == wc_commit_id.as_ref() {
        NodeKind::WorkingCopy
    } else {
        NodeKind::Mutable
    }
}

/// The closest visible ancestor(s) for bases whose direct parents fall
/// outside the snapshot: these become jj's `~` elided-history links, so the
/// trunk line renders as one connected spine instead of disconnected
/// islands.
fn elided_base_links(
    repo: &dyn Repo,
    base_commits: &[Commit],
    change_ids: &HashMap<CommitId, String>,
) -> HashMap<CommitId, Vec<String>> {
    let mut links = HashMap::new();
    if base_commits.len() > MAX_LINKED_BASES {
        tracing::warn!(
            bases = base_commits.len(),
            limit = MAX_LINKED_BASES,
            "too many bases; skipping elided-history links"
        );
        return links;
    }
    for base in base_commits {
        if base
            .parent_ids()
            .iter()
            .any(|id| change_ids.contains_key(id))
        {
            continue; // a drawn parent already connects this base
        }
        let ancestors: Vec<&Commit> = base_commits
            .iter()
            .filter(|c| c.id() != base.id() && is_ancestor(repo, c.id(), base.id()))
            .collect();
        // Keep only the maximal ancestors: drop any that another candidate
        // sits above on the way to this base.
        let closest: Vec<String> = ancestors
            .iter()
            .filter(|c| {
                !ancestors
                    .iter()
                    .any(|d| d.id() != c.id() && is_ancestor(repo, c.id(), d.id()))
            })
            .filter_map(|c| change_ids.get(c.id()).cloned())
            .collect();
        if !closest.is_empty() {
            links.insert(base.id().clone(), closest);
        }
    }
    links
}

#[allow(clippy::too_many_arguments)]
fn graph_node(
    repo: &dyn Repo,
    view: &View,
    commit: &Commit,
    kind: NodeKind,
    held: bool,
    old_copies_held: bool,
    trunk: &Option<TrunkRef>,
    change_ids: &HashMap<CommitId, String>,
    divergent: &HashSet<CommitId>,
    elided_links: &HashMap<CommitId, Vec<String>>,
    commit_tags: &HashMap<CommitId, Vec<String>>,
) -> GraphNode {
    let mut bookmarks: Vec<String> = view
        .local_bookmarks_for_commit(commit.id())
        .map(|(name, _)| name.as_str().to_owned())
        .collect();
    if let Some(trunk) = trunk {
        if &trunk.target == commit.id() && !bookmarks.iter().any(|b| b == &trunk.name) {
            bookmarks.insert(0, trunk.name.clone());
        }
    }

    // Parents are limited to commits the snapshot draws; emptiness only
    // matters for the part of the graph that is still worked on, so bases
    // skip the extra tree loads.
    let is_base = kind == NodeKind::Immutable && !held;
    let parents = commit
        .parent_ids()
        .iter()
        .filter_map(|id| change_ids.get(id).cloned())
        .collect();
    let is_empty = !is_base && commit_is_empty(repo, commit);
    let author_name = author_display(commit);

    GraphNode {
        id: change_ids
            .get(commit.id())
            .cloned()
            .unwrap_or_else(|| node_id(repo, commit)),
        change_id: short_change_id(repo, commit),
        commit_id: short_commit_id(repo, commit.id()),
        description: commit.description().trim().to_owned(),
        author: author_name,
        timestamp: format_timestamp(&commit.committer().timestamp),
        kind,
        parents,
        elided_parents: elided_links.get(commit.id()).cloned().unwrap_or_default(),
        bookmarks,
        tags: commit_tags.get(commit.id()).cloned().unwrap_or_default(),
        is_empty,
        has_conflict: commit.has_conflict(),
        is_divergent: divergent.contains(commit.id()),
        held,
        old_copies_held,
    }
}

/// `Commit::is_empty`, memoized for the process by commit id. Emptiness is
/// fixed by what the id already covers (tree and parents), but answering
/// it for a merge commit means merging the parents' trees: hundreds of
/// merges in mutable history made that most of every snapshot on a big
/// repo, and each refresh repeated it. Errors are not cached; they read as
/// not empty, as before.
fn commit_is_empty(repo: &dyn Repo, commit: &Commit) -> bool {
    /// Past this the memo starts over rather than grow without bound.
    const MAX_ENTRIES: usize = 100_000;
    static EMPTY: std::sync::OnceLock<std::sync::Mutex<HashMap<CommitId, bool>>> =
        std::sync::OnceLock::new();
    let memo = EMPTY.get_or_init(Default::default);
    let lock = || memo.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(&empty) = lock().get(commit.id()) {
        return empty;
    }
    let Ok(empty) = pollster::block_on(commit.is_empty(repo)) else {
        return false;
    };
    let mut memo = lock();
    if memo.len() >= MAX_ENTRIES {
        memo.clear();
    }
    memo.insert(commit.id().clone(), empty);
    empty
}

/// The author name the UI shows: the name, or the email's local part when
/// the name is empty.
fn author_display(commit: &Commit) -> String {
    let author = commit.author();
    if !author.name.is_empty() {
        author.name.clone()
    } else {
        author.email.split('@').next().unwrap_or_default().to_owned()
    }
}

#[allow(clippy::too_many_arguments)]
fn build_workstreams(
    repo: &dyn Repo,
    view: &View,
    mutable_commits: &[Commit],
    mutable_set: &HashSet<CommitId>,
    wc_commit_id: &Option<CommitId>,
    trunk: &Option<TrunkRef>,
    change_ids: &HashMap<CommitId, String>,
) -> Vec<WorkstreamSummary> {
    let by_id: HashMap<&CommitId, &Commit> =
        mutable_commits.iter().map(|c| (c.id(), c)).collect();
    let mut has_mutable_child: HashSet<&CommitId> = HashSet::new();
    for commit in mutable_commits {
        for parent in commit.parent_ids() {
            if mutable_set.contains(parent) {
                has_mutable_child.insert(parent);
            }
        }
    }

    // `mutable_commits` is child-before-parent, so heads come out newest-ish
    // first, matching how the sidebar should rank sibling stacks.
    let heads = mutable_commits
        .iter()
        .filter(|c| !has_mutable_child.contains(c.id()));

    let mut claimed: HashSet<CommitId> = HashSet::new();
    let mut workstreams = Vec::new();
    for head in heads {
        if claimed.contains(head.id()) {
            continue;
        }
        let mut chain: Vec<&Commit> = Vec::new();
        let mut cursor = head;
        loop {
            chain.push(cursor);
            claimed.insert(cursor.id().clone());
            let next = cursor
                .parent_ids()
                .iter()
                .find(|id| mutable_set.contains(*id) && !claimed.contains(*id))
                .and_then(|id| by_id.get(id));
            match next {
                Some(commit) => cursor = commit,
                None => break,
            }
        }

        let bookmark = chain.iter().find_map(|commit| {
            view.local_bookmarks_for_commit(commit.id())
                .map(|(name, _)| name.as_str().to_owned())
                .next()
        });
        let title = bookmark
            .as_deref()
            .map(title_from_bookmark)
            .or_else(|| {
                chain.iter().find_map(|commit| {
                    let line = commit.description().lines().next().unwrap_or("").trim();
                    (!line.is_empty()).then(|| line.to_owned())
                })
            })
            .unwrap_or_else(|| "Anonymous work".to_owned());

        let is_active = wc_commit_id
            .as_ref()
            .is_some_and(|wc| chain.iter().any(|c| c.id() == wc));
        let head_short = change_ids
            .get(head.id())
            .cloned()
            .unwrap_or_else(|| short_change_id(repo, head));

        workstreams.push(WorkstreamSummary {
            id: format!("ws-{head_short}"),
            title,
            node_ids: chain
                .iter()
                .filter_map(|c| change_ids.get(c.id()).cloned())
                .collect(),
            bookmark,
            is_active,
            behind_trunk: trunk
                .as_ref()
                .map(|t| count_behind(repo, head.id(), &t.target))
                .unwrap_or(0),
        });
    }

    workstreams.sort_by_key(|ws| !ws.is_active);
    workstreams
}

fn count_behind(repo: &dyn Repo, head: &CommitId, trunk: &CommitId) -> u32 {
    let expr: Arc<ResolvedRevsetExpression> = RevsetExpression::commits(vec![head.clone()])
        .range(&RevsetExpression::commits(vec![trunk.clone()]));
    match expr.evaluate(repo) {
        Ok(revset) => revset
            .iter_blocking()
            .take(MAX_BEHIND_COUNT)
            .filter(|id| id.is_ok())
            .count() as u32,
        Err(_) => 0,
    }
}

fn title_from_bookmark(name: &str) -> String {
    let spaced = name.replace(['-', '_'], " ");
    let mut chars = spaced.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => spaced,
    }
}

/// Conflicted paths listed per item, capped like `OperationItem` effects.
const MAX_CONFLICT_PATHS: usize = 20;

fn file_conflicts(
    mutable_commits: &[Commit],
    wc_commit_id: &Option<CommitId>,
    change_ids: &HashMap<CommitId, String>,
) -> Vec<ConflictItem> {
    mutable_commits
        .iter()
        .filter(|commit| commit.has_conflict())
        .map(|commit| {
            let short_id = change_ids.get(commit.id()).cloned();
            let is_wc = Some(commit.id()) == wc_commit_id.as_ref();
            let summary = if is_wc {
                "The working copy has unresolved file conflicts".to_owned()
            } else {
                let label = commit.description().lines().next().unwrap_or("").trim();
                if label.is_empty() {
                    format!(
                        "Change {} has unresolved file conflicts",
                        short_id.as_deref().unwrap_or("?")
                    )
                } else {
                    format!("\u{201c}{label}\u{201d} has unresolved file conflicts")
                }
            };
            // The tree's own unresolved entries (`jj resolve --list`), not
            // the parent-relative diff: a child inherits its parent's
            // conflict in files the child never touched.
            let mut paths = Vec::new();
            let mut more_paths = 0u32;
            for (path, _value) in commit.tree().conflicts() {
                if paths.len() < MAX_CONFLICT_PATHS {
                    paths.push(path.as_internal_file_string().to_owned());
                } else {
                    more_paths += 1;
                }
            }
            ConflictItem {
                id: format!("file-{}", short_id.as_deref().unwrap_or("?")),
                kind: ConflictKind::File,
                summary,
                node_id: short_id,
                paths,
                more_paths,
                targets: Vec::new(),
                workspace: None,
            }
        })
        .collect()
}

fn build_bookmarks(
    repo: &dyn Repo,
    trunk: &Option<TrunkRef>,
    conflicts: &mut Vec<ConflictItem>,
    change_ids: &mut HashMap<CommitId, String>,
) -> Result<Vec<BookmarkState>, BackendError> {
    let view = repo.view();
    let trunk_name = trunk.as_ref().map(|t| t.name.as_str());
    let mut bookmarks = Vec::new();

    // Which targets trunk already contains, for every bookmark in one walk
    // over trunk's ancestry: an ancestry check per bookmark walks history
    // from trunk each time, which a repo with thousands of remote branches
    // cannot afford on every refresh.
    let merged: HashSet<CommitId> = match trunk {
        Some(trunk) => {
            let targets: Vec<CommitId> = view
                .bookmarks()
                .flat_map(|(_, targets)| {
                    let remote_ids = targets
                        .remote_refs
                        .into_iter()
                        .filter(|(remote, _)| *remote != REMOTE_NAME_FOR_LOCAL_GIT_REPO)
                        .flat_map(|(_, remote_ref)| remote_ref.target.added_ids());
                    targets.local_target.added_ids().chain(remote_ids)
                })
                .cloned()
                .collect();
            let expr: Arc<ResolvedRevsetExpression> = RevsetExpression::commits(targets)
                .intersection(&RevsetExpression::commit(trunk.target.clone()).ancestors());
            let revset = expr.evaluate(repo).map_err(snapshot_err)?;
            revset
                .iter_blocking()
                .collect::<Result<_, _>>()
                .map_err(snapshot_err)?
        }
        None => HashSet::new(),
    };

    // Target-commit facts every entry carries: merged-into-trunk (deleting
    // is cleanup, not loss) and display metadata for surfaces that render
    // bookmarks without the commit drawn (the bookmarks manager). Trunk's
    // own bookmark never reads as merged — callers mask it by `is_trunk`.
    let target_facts =
        |id: &CommitId| -> Result<(bool, String, String, String), BackendError> {
            let is_merged = merged.contains(id);
            let commit = repo.store().get_commit(id).map_err(snapshot_err)?;
            Ok((
                is_merged,
                commit
                    .description()
                    .lines()
                    .next()
                    .unwrap_or("")
                    .trim()
                    .to_owned(),
                author_display(&commit),
                format_timestamp(&commit.committer().timestamp),
            ))
        };

    for (name, targets) in view.bookmarks() {
        let local_target = targets.local_target;

        // Remote refs the local entry does not summarize: every remote ref
        // when no local bookmark exists (a teammate's work, or leftovers to
        // clean up), and *untracked* refs even beside a local one — jj
        // treats those as independent refs, and they may point anywhere.
        // One entry per remote; jj's hidden colocated `git` remote is
        // excluded like everywhere else.
        for (remote, remote_ref) in &targets.remote_refs {
            if *remote == REMOTE_NAME_FOR_LOCAL_GIT_REPO {
                continue;
            }
            let tracked = remote_ref.state == RemoteRefState::Tracked;
            if local_target.is_present() && tracked {
                continue;
            }
            let Some(remote_id) = remote_ref.target.added_ids().next() else {
                continue;
            };
            let (is_merged, target_description, target_author, target_timestamp) =
                target_facts(remote_id)?;
            // A same-name local entry claims trunk; only a truly
            // remote-only ref can be the trunk entry.
            let is_trunk = local_target.is_absent()
                && Some(name.as_str()) == trunk_name
                && trunk
                    .as_ref()
                    .is_some_and(|t| t.remote.as_deref() == Some(remote.as_str()));
            bookmarks.push(BookmarkState {
                name: name.as_str().to_owned(),
                target: node_id_for(repo, remote_id, change_ids)?,
                remote: Some(remote.as_str().to_owned()),
                sync: SyncState::RemoteOnly,
                is_trunk,
                is_local: false,
                tracked,
                is_merged: is_merged && !is_trunk,
                target_description,
                target_author,
                target_timestamp,
            });
        }
        if local_target.is_absent() {
            continue;
        }
        if local_target.has_conflict() {
            let targets = local_target
                .added_ids()
                .map(|id| node_id_for(repo, id, change_ids))
                .collect::<Result<Vec<_>, _>>()?;
            conflicts.push(ConflictItem {
                id: format!("bookmark-{}", name.as_str()),
                kind: ConflictKind::Bookmark,
                summary: format!(
                    "Bookmark \u{201c}{}\u{201d} resolved to multiple targets and needs to be repointed",
                    name.as_str()
                ),
                node_id: None,
                paths: Vec::new(),
                more_paths: 0,
                targets,
                workspace: None,
            });
        }
        let Some(local_id) = local_target.added_ids().next() else {
            continue;
        };

        let tracked_remote = targets
            .remote_refs
            .iter()
            .filter(|(remote, remote_ref)| {
                *remote != REMOTE_NAME_FOR_LOCAL_GIT_REPO
                    && remote_ref.state == RemoteRefState::Tracked
                    && remote_ref.target.is_present()
            })
            .min_by_key(|(remote, _)| (remote_rank(remote.as_str()), remote.as_str().to_owned()));

        let (remote, sync) = match tracked_remote {
            None => (None, SyncState::LocalOnly),
            Some((remote, remote_ref)) => {
                let sync = classify_sync(repo, local_id, remote_ref.target.added_ids().next());
                (Some(remote.as_str().to_owned()), sync)
            }
        };

        let (is_merged, target_description, target_author, target_timestamp) =
            target_facts(local_id)?;
        let is_trunk = Some(name.as_str()) == trunk_name;
        bookmarks.push(BookmarkState {
            name: name.as_str().to_owned(),
            target: node_id_for(repo, local_id, change_ids)?,
            tracked: remote.is_some(),
            remote,
            sync,
            is_trunk,
            is_local: true,
            is_merged: is_merged && !is_trunk,
            target_description,
            target_author,
            target_timestamp,
        });
    }

    // A trunk no bookmark entry claimed (e.g. its remote ref sits behind a
    // conflicted local target) still needs an entry: the UI finds the trunk
    // node through it.
    if let Some(trunk) = trunk {
        if !bookmarks.iter().any(|b| b.is_trunk) {
            let (_, target_description, target_author, target_timestamp) =
                target_facts(&trunk.target)?;
            bookmarks.push(BookmarkState {
                name: trunk.name.clone(),
                target: node_id_for(repo, &trunk.target, change_ids)?,
                remote: trunk.remote.clone(),
                sync: SyncState::Synced,
                is_trunk: true,
                is_local: false,
                tracked: true,
                is_merged: false,
                target_description,
                target_author,
                target_timestamp,
            });
        }
    }

    Ok(bookmarks)
}

fn classify_sync(repo: &dyn Repo, local_id: &CommitId, remote_id: Option<&CommitId>) -> SyncState {
    let Some(remote_id) = remote_id else {
        return SyncState::Ahead;
    };
    if remote_id == local_id {
        SyncState::Synced
    } else if is_ancestor(repo, remote_id, local_id) {
        SyncState::Ahead
    } else if is_ancestor(repo, local_id, remote_id) {
        SyncState::Behind
    } else {
        SyncState::Diverged
    }
}

fn is_ancestor(repo: &dyn Repo, ancestor: &CommitId, descendant: &CommitId) -> bool {
    pollster::block_on(repo.index().is_ancestor(ancestor, descendant)).unwrap_or(false)
}

fn build_operations(repo: &Arc<ReadonlyRepo>) -> Result<Vec<OperationItem>, BackendError> {
    let store = repo.store();
    let head_op = repo.operation().clone();
    // Adjacent ops in a linear history share one view between them; memoize
    // the extracted summaries so each view loads once.
    let mut summaries: HashMap<ViewId, Rc<OpViewSummary>> = HashMap::new();
    let mut operations = Vec::new();
    for op in blocking_iter(op_walk::walk_ancestors(&[head_op])).take(MAX_OPERATIONS) {
        let op = op.map_err(snapshot_err)?;
        // Effects only make sense against a single parent; the root op and
        // merges of divergent operations report none.
        let (effects, more_effects) = if op.parent_ids().len() == 1 {
            let parent = pollster::block_on(op.parents())
                .map_err(snapshot_err)?
                .into_iter()
                .next()
                .expect("single-parent op");
            let before = op_view_summary(&mut summaries, &parent)?;
            let after = op_view_summary(&mut summaries, &op)?;
            op_effects(store, &before, &after)
        } else {
            (Vec::new(), 0)
        };
        let metadata = op.metadata();
        operations.push(OperationItem {
            id: op.id().hex().chars().take(12).collect(),
            description: metadata.description.clone(),
            timestamp: format_timestamp(&metadata.time.end),
            is_current: op.id() == repo.op_id(),
            user: format!("{}@{}", metadata.username, metadata.hostname),
            is_snapshot: metadata.is_snapshot,
            effects,
            more_effects,
        });
    }
    Ok(operations)
}

/// The slice of an operation's view that the timeline diffs: ref targets
/// keyed by display name, and working-copy commits keyed by workspace.
struct OpViewSummary {
    local_bookmarks: BTreeMap<String, Vec<CommitId>>,
    /// Keyed `name@remote`; the hidden colocated `git` remote is excluded.
    remote_bookmarks: BTreeMap<String, Vec<CommitId>>,
    wc_commits: BTreeMap<String, CommitId>,
}

fn op_view_summary(
    cache: &mut HashMap<ViewId, Rc<OpViewSummary>>,
    op: &Operation,
) -> Result<Rc<OpViewSummary>, BackendError> {
    if let Some(summary) = cache.get(op.view_id()) {
        return Ok(summary.clone());
    }
    let view = pollster::block_on(op.view()).map_err(snapshot_err)?;
    let summary = Rc::new(OpViewSummary {
        local_bookmarks: view
            .local_bookmarks()
            .map(|(name, target)| {
                (name.as_str().to_owned(), target.added_ids().cloned().collect())
            })
            .collect(),
        remote_bookmarks: view
            .all_remote_bookmarks()
            .filter(|(symbol, _)| symbol.remote != REMOTE_NAME_FOR_LOCAL_GIT_REPO)
            .map(|(symbol, remote_ref)| {
                (
                    format!("{}@{}", symbol.name.as_str(), symbol.remote.as_str()),
                    remote_ref.target.added_ids().cloned().collect(),
                )
            })
            .collect(),
        wc_commits: view
            .wc_commit_ids()
            .iter()
            .map(|(name, id)| (name.as_str().to_owned(), id.clone()))
            .collect(),
    });
    cache.insert(op.view_id().clone(), summary.clone());
    Ok(summary)
}

fn op_effects(
    store: &Arc<Store>,
    before: &OpViewSummary,
    after: &OpViewSummary,
) -> (Vec<OpEffect>, u32) {
    let mut effects = Vec::new();
    diff_ref_maps(
        store,
        &before.local_bookmarks,
        &after.local_bookmarks,
        OpEffectKind::Bookmark,
        "moved",
        &mut effects,
    );
    diff_ref_maps(
        store,
        &before.remote_bookmarks,
        &after.remote_bookmarks,
        OpEffectKind::RemoteBookmark,
        "updated",
        &mut effects,
    );

    let wc_label = |name: &str, what: &str| OpEffect {
        kind: OpEffectKind::WorkingCopy,
        label: if name == "default" {
            format!("working copy {what}")
        } else {
            format!("{name} working copy {what}")
        },
    };
    for (name, after_id) in &after.wc_commits {
        match before.wc_commits.get(name) {
            None => effects.push(OpEffect {
                kind: OpEffectKind::WorkingCopy,
                label: format!("workspace {name} added"),
            }),
            // A rewrite that keeps the working copy on the same change
            // (snapshot, describe, rebase) is not a move.
            Some(before_id) if before_id != after_id
                && change_keys(store, std::slice::from_ref(before_id))
                    != change_keys(store, std::slice::from_ref(after_id))
                => {
                    effects.push(wc_label(name, "moved"));
                }
            _ => {}
        }
    }
    for name in before.wc_commits.keys() {
        if !after.wc_commits.contains_key(name) {
            effects.push(OpEffect {
                kind: OpEffectKind::WorkingCopy,
                label: format!("workspace {name} forgotten"),
            });
        }
    }

    let more = effects.len().saturating_sub(MAX_OP_EFFECTS) as u32;
    effects.truncate(MAX_OP_EFFECTS);
    (effects, more)
}

fn diff_ref_maps(
    store: &Arc<Store>,
    before: &BTreeMap<String, Vec<CommitId>>,
    after: &BTreeMap<String, Vec<CommitId>>,
    kind: OpEffectKind,
    moved_verb: &str,
    out: &mut Vec<OpEffect>,
) {
    let effect = |name: &str, verb: &str| OpEffect {
        kind,
        label: format!("{name} {verb}"),
    };
    for (name, after_ids) in after {
        match before.get(name) {
            None => out.push(effect(name, "created")),
            Some(before_ids) if before_ids != after_ids => {
                if after_ids.len() > 1 {
                    out.push(effect(name, "conflicted"));
                } else if change_keys(store, before_ids) != change_keys(store, after_ids) {
                    // Same change rewritten in place is not a move.
                    out.push(effect(name, moved_verb));
                }
            }
            _ => {}
        }
    }
    for name in before.keys() {
        if !after.contains_key(name) {
            out.push(effect(name, "deleted"));
        }
    }
}

/// Change-id identities for a set of commits, so ref moves can be compared
/// at change granularity. Commits an old operation references may have been
/// garbage-collected; fall back to the commit id itself.
fn change_keys(store: &Arc<Store>, ids: &[CommitId]) -> BTreeSet<String> {
    ids.iter()
        .map(|id| {
            store
                .get_commit(id)
                .map(|commit| commit.change_id().hex())
                .unwrap_or_else(|_| id.hex())
        })
        .collect()
}

/// jj renders change ids in reverse hex ("z-k" digits). Eight characters is
/// jj's display default; extend only if needed for uniqueness.
fn short_change_id(repo: &dyn Repo, commit: &Commit) -> String {
    let change_id = commit.change_id();
    let len = pollster::block_on(repo.shortest_unique_change_id_prefix_len(change_id))
        .unwrap_or(8)
        .max(8);
    let full = hex_util::encode_reverse_hex(change_id.as_bytes());
    full[..len.min(full.len())].to_owned()
}

/// jj's `??` state: several visible commits share this commit's change id.
/// Index errors report as not-divergent — anything genuinely broken fails
/// the surrounding revset evaluation first.
fn is_divergent(repo: &dyn Repo, commit: &Commit) -> bool {
    pollster::block_on(repo.resolve_change_id(commit.change_id()))
        .ok()
        .flatten()
        .is_some_and(|targets| targets.is_divergent())
}

/// The id a snapshot addresses this commit by (`GraphNode::id`): its short
/// change id when that names exactly this commit, otherwise its short
/// commit id — like the CLI, which falls back to commit ids for the copies
/// of a divergent change and for hidden commits. A hidden commit shares its
/// change id with the change's visible commit (a remote bookmark still on
/// the pre-rewrite commit), and both can be drawn: keyed by change id they
/// would collide in the snapshot, and the id would resolve to the visible
/// commit instead.
fn node_id(repo: &dyn Repo, commit: &Commit) -> String {
    if change_id_names_commit(repo, commit) {
        short_change_id(repo, commit)
    } else {
        short_commit_id(repo, commit.id())
    }
}

/// Whether the change id resolves to this commit alone: it is the change's
/// only visible commit. Index errors answer no, so the commit goes by its
/// commit id, which always resolves.
fn change_id_names_commit(repo: &dyn Repo, commit: &Commit) -> bool {
    pollster::block_on(repo.resolve_change_id(commit.change_id()))
        .ok()
        .flatten()
        .is_some_and(|targets| !targets.is_divergent() && targets.has_visible(commit.id()))
}

/// The id a mutation's selection follows (`MutationOutcome::target_change`):
/// the short change id — which resolves to the change's current commit
/// even when `commit` is a version the mutation has since rewritten again —
/// or the short commit id for a copy of a divergent change.
fn display_id(repo: &dyn Repo, commit: &Commit) -> String {
    if is_divergent(repo, commit) {
        short_commit_id(repo, commit.id())
    } else {
        short_change_id(repo, commit)
    }
}

fn node_id_for(
    repo: &dyn Repo,
    commit_id: &CommitId,
    change_ids: &mut HashMap<CommitId, String>,
) -> Result<String, BackendError> {
    if let Some(existing) = change_ids.get(commit_id) {
        return Ok(existing.clone());
    }
    let commit = repo.store().get_commit(commit_id).map_err(snapshot_err)?;
    let short = node_id(repo, &commit);
    change_ids.insert(commit_id.clone(), short.clone());
    Ok(short)
}

/// Display id for a commit id outside the snapshot's cache (bookmark
/// targets or the working copy resolved during a mutation).
fn display_id_of(repo: &dyn Repo, commit_id: &CommitId) -> Result<String, BackendError> {
    let commit = repo.store().get_commit(commit_id).map_err(snapshot_err)?;
    Ok(display_id(repo, &commit))
}

fn short_commit_id(repo: &dyn Repo, commit_id: &CommitId) -> String {
    let len = pollster::block_on(repo.index().shortest_unique_commit_id_prefix_len(commit_id))
        .unwrap_or(8)
        .max(8);
    let full = commit_id.hex();
    full[..len.min(full.len())].to_owned()
}

fn format_timestamp(timestamp: &Timestamp) -> String {
    timestamp
        .to_datetime()
        .map(|dt| dt.to_rfc3339())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use jj_lib::backend::{CopyId, TreeValue};
    use jj_lib::config::{ConfigLayer, ConfigSource, StackedConfig};
    use jj_lib::merge::Merge;
    use jj_lib::merged_tree::MergedTree;
    use jj_lib::merged_tree_builder::MergedTreeBuilder;
    use jj_lib::op_store::RefTarget;
    use jj_lib::ref_name::WorkspaceName;
    use jj_lib::repo_path::RepoPathBuf;
    use jj_lib::settings::UserSettings;
    use jj_lib::store::Store;
    use jj_lib::transaction::Transaction;

    use super::*;

    /// A backend that ignores this machine's `~/.config/jj`, so tests only
    /// see jj-lib defaults, Jiji's default aliases, and per-repo config.
    fn test_backend() -> JjBackend {
        JjBackend::with_user_config(UserConfigSource::None)
    }

    /// Writes repo-scoped config the legacy way (`.jj/repo/config.toml`),
    /// which the loader resolves without consulting platform directories.
    fn write_repo_config(root: &Path, text: &str) {
        std::fs::write(root.join(".jj/repo/config.toml"), text).unwrap();
    }

    fn test_settings() -> UserSettings {
        let mut config = StackedConfig::with_defaults();
        config.add_layer(
            ConfigLayer::parse(
                ConfigSource::User,
                "user.name = \"Test User\"\nuser.email = \"test@example.com\"\n\
                 operation.username = \"test-user\"\noperation.hostname = \"test-host\"\n",
            )
            .unwrap(),
        );
        UserSettings::from_config(config).unwrap()
    }

    fn write_commit(tx: &mut Transaction, parents: Vec<CommitId>, description: &str) -> Commit {
        let tree = tx.repo_mut().store().empty_merged_tree();
        write_commit_with_tree(tx, parents, description, tree)
    }

    fn write_commit_with_tree(
        tx: &mut Transaction,
        parents: Vec<CommitId>,
        description: &str,
        tree: MergedTree,
    ) -> Commit {
        let builder = tx.repo_mut().new_commit(parents, tree).set_description(description);
        pollster::block_on(builder.write()).unwrap()
    }

    /// A resolved tree containing exactly `files` (path, contents).
    fn file_tree<T: AsRef<[u8]>>(store: &Arc<Store>, files: &[(&str, T)]) -> MergedTree {
        let mut builder = MergedTreeBuilder::new(store.empty_merged_tree());
        for (path, contents) in files {
            let repo_path = RepoPathBuf::from_internal_string(*path).unwrap();
            let mut reader = contents.as_ref();
            let id = pollster::block_on(store.write_file(&repo_path, &mut reader)).unwrap();
            builder.set_or_remove(
                repo_path,
                Merge::normal(TreeValue::File {
                    id,
                    executable: false,
                    copy_id: CopyId::placeholder(),
                }),
            );
        }
        pollster::block_on(builder.write_tree()).unwrap()
    }

    /// root ── trunk(main) ── feature(feature-a) ── wc (undescribed, edited)
    ///     └── side (anonymous head, behind trunk by one)
    fn build_test_repo(root: &Path) {
        let settings = test_settings();
        let (_workspace, repo) =
            pollster::block_on(Workspace::init_simple(&settings, root)).unwrap();
        let root_commit_id = repo.store().root_commit_id().clone();
        let mut tx = repo.start_transaction();

        let trunk = write_commit(&mut tx, vec![root_commit_id.clone()], "release: cut 1.0");
        tx.repo_mut()
            .set_local_bookmark_target(RefName::new("main"), RefTarget::normal(trunk.id().clone()));

        let feature = write_commit(&mut tx, vec![trunk.id().clone()], "feat: first change");
        tx.repo_mut().set_local_bookmark_target(
            RefName::new("feature-a"),
            RefTarget::normal(feature.id().clone()),
        );

        let wc = write_commit(&mut tx, vec![feature.id().clone()], "");
        pollster::block_on(tx.repo_mut().edit(WorkspaceName::DEFAULT.to_owned(), &wc)).unwrap();

        write_commit(&mut tx, vec![root_commit_id], "wip: side experiment");

        pollster::block_on(tx.repo_mut().rebase_descendants()).unwrap();
        pollster::block_on(tx.commit("set up test stack")).unwrap();
    }

    #[test]
    fn open_rejects_non_jj_directories() {
        let dir = tempfile::tempdir().unwrap();
        let err = test_backend().open(dir.path()).unwrap_err();
        assert!(matches!(err, BackendError::NotAJjRepo(_)));
    }

    #[test]
    fn configured_trunk_drives_snapshot_and_publish_destination() {
        let dir = tempfile::tempdir().unwrap();
        build_test_repo(dir.path());
        let backend = test_backend();
        let (_, repo) = backend.load_repo_at_head(dir.path()).unwrap();
        let main = repo.view().get_local_bookmark(RefName::new("main")).clone();
        let mut tx = repo.start_transaction();
        // Even when both names point at the same commit, the configured
        // develop name is the destination, not the conventional main.
        tx.repo_mut()
            .set_local_bookmark_target(RefName::new("develop"), main);
        pollster::block_on(tx.commit("add develop")).unwrap();
        write_repo_config(dir.path(), "[revset-aliases]\n 'trunk()' = 'develop'\n");
        let snapshot = backend.open(dir.path()).unwrap();
        assert_eq!(snapshot.trunk_bookmark, "develop");
        assert!(snapshot
            .bookmarks
            .iter()
            .any(|b| b.name == "develop" && b.is_trunk));
        assert!(!snapshot
            .bookmarks
            .iter()
            .any(|b| b.name == "main" && b.is_trunk));
        write_repo_config(
            dir.path(),
            "[revset-aliases]\n 'trunk()' = 'latest(bookmarks(exact:\"develop\"))'\n",
        );
        assert_eq!(backend.open(dir.path()).unwrap().trunk_bookmark, "develop");

        // A different tip must also drive the immutability boundary.
        let (_, repo) = backend.load_repo_at_head(dir.path()).unwrap();
        let feature = repo
            .view()
            .get_local_bookmark(RefName::new("feature-a"))
            .clone();
        let mut tx = repo.start_transaction();
        tx.repo_mut()
            .set_local_bookmark_target(RefName::new("develop"), feature);
        pollster::block_on(tx.commit("advance develop")).unwrap();
        let snapshot = backend.open(dir.path()).unwrap();
        let develop = snapshot.bookmarks.iter().find(|b| b.is_trunk).unwrap();
        assert_eq!(develop.name, "develop");
        assert_eq!(
            snapshot
                .nodes
                .iter()
                .find(|n| n.id == develop.target)
                .unwrap()
                .kind,
            NodeKind::Immutable
        );
    }

    #[test]
    fn trunk_alias_errors_never_fall_back_to_main() {
        let dir = tempfile::tempdir().unwrap();
        build_test_repo(dir.path());
        write_repo_config(
            dir.path(),
            "[revset-aliases]\n 'trunk()' = 'unknown-branch'\n",
        );
        assert!(matches!(
            test_backend().open(dir.path()),
            Err(BackendError::ConfigInvalid(_))
        ));
    }

    #[test]
    fn open_builds_snapshot_from_real_repo() {
        let dir = tempfile::tempdir().unwrap();
        build_test_repo(dir.path());

        let snapshot = test_backend().open(dir.path()).unwrap();

        assert_eq!(snapshot.backend, "jj-lib");
        assert_eq!(snapshot.trunk_bookmark, "main");
        assert!(!snapshot.working_copy.is_empty());

        // The working copy renders as an empty, undescribed draft node.
        let wc_node = snapshot
            .nodes
            .iter()
            .find(|n| n.id == snapshot.working_copy)
            .expect("working copy node present");
        assert_eq!(wc_node.kind, NodeKind::WorkingCopy);
        assert_eq!(wc_node.description, "");
        assert!(wc_node.is_empty);
        assert!(!wc_node.has_conflict);
        assert_eq!(wc_node.author, "Test User");

        // Trunk appears as an immutable base carrying the main bookmark, and
        // the trunk bookmark points at it.
        let trunk_bookmark = snapshot
            .bookmarks
            .iter()
            .find(|b| b.is_trunk)
            .expect("trunk bookmark present");
        assert_eq!(trunk_bookmark.name, "main");
        assert_eq!(trunk_bookmark.sync, SyncState::LocalOnly);
        let trunk_node = snapshot
            .nodes
            .iter()
            .find(|n| n.id == trunk_bookmark.target)
            .expect("trunk node present");
        assert_eq!(trunk_node.kind, NodeKind::Immutable);
        assert!(trunk_node.bookmarks.contains(&"main".to_owned()));

        // The root commit is in the snapshot (the side stack sits on it), so
        // trunk links to it as a drawn parent and needs no elided link.
        let root_node = snapshot
            .nodes
            .iter()
            .find(|n| n.kind == NodeKind::Immutable && n.id != trunk_node.id)
            .expect("root base present");
        assert_eq!(trunk_node.parents, vec![root_node.id.clone()]);
        assert!(trunk_node.elided_parents.is_empty());

        // The active workstream is the bookmark stack under the working copy.
        let active = snapshot
            .workstreams
            .iter()
            .find(|ws| ws.is_active)
            .expect("active workstream present");
        assert_eq!(active.node_ids.len(), 2);
        assert_eq!(active.node_ids[0], snapshot.working_copy);
        assert_eq!(active.bookmark.as_deref(), Some("feature-a"));
        assert_eq!(active.title, "Feature a");
        assert_eq!(active.behind_trunk, 0);
        assert_eq!(snapshot.workstreams[0].id, active.id);

        // Parent links stay within the snapshot's change-id namespace.
        let feature_id = &active.node_ids[1];
        assert_eq!(&wc_node.parents, std::slice::from_ref(feature_id));

        // The anonymous side commit forms its own workstream, one behind trunk.
        let side = snapshot
            .workstreams
            .iter()
            .find(|ws| !ws.is_active)
            .expect("sibling workstream present");
        assert_eq!(side.title, "wip: side experiment");
        assert_eq!(side.node_ids.len(), 1);
        assert!(side.bookmark.is_none());
        assert_eq!(side.behind_trunk, 1);

        // Local bookmarks without remotes are local-only.
        let feature_bookmark = snapshot
            .bookmarks
            .iter()
            .find(|b| b.name == "feature-a")
            .expect("feature bookmark present");
        assert_eq!(feature_bookmark.sync, SyncState::LocalOnly);
        assert!(feature_bookmark.remote.is_none());
        assert_eq!(&feature_bookmark.target, feature_id);

        // Operation log is newest-first with the head op marked current.
        assert!(snapshot.operations.len() >= 2);
        assert_eq!(snapshot.operations[0].description, "set up test stack");
        assert!(snapshot.operations[0].is_current);
        assert!(snapshot.operations.iter().skip(1).all(|op| !op.is_current));

        // The default workspace is reported with its working-copy node.
        assert_eq!(snapshot.workspaces.len(), 1);
        assert!(snapshot.workspaces[0].is_default);
        assert_eq!(
            snapshot.workspaces[0].working_copy_node.as_deref(),
            Some(snapshot.working_copy.as_str())
        );

        assert!(snapshot.conflicts.is_empty());
    }

    #[test]
    fn snapshot_surfaces_tags_on_their_commits() {
        let dir = tempfile::tempdir().unwrap();
        let settings = test_settings();
        let (_workspace, repo) =
            pollster::block_on(Workspace::init_simple(&settings, dir.path())).unwrap();
        let root_commit_id = repo.store().root_commit_id().clone();
        let mut tx = repo.start_transaction();

        let tagged = write_commit(&mut tx, vec![root_commit_id], "release: cut 1.0");
        tx.repo_mut()
            .set_local_tag_target(RefName::new("v1.0"), RefTarget::normal(tagged.id().clone()));
        tx.repo_mut()
            .set_local_tag_target(RefName::new("v1.0-rc1"), RefTarget::normal(tagged.id().clone()));

        let wc = write_commit(&mut tx, vec![tagged.id().clone()], "");
        pollster::block_on(tx.repo_mut().edit(WorkspaceName::DEFAULT.to_owned(), &wc)).unwrap();
        pollster::block_on(tx.repo_mut().rebase_descendants()).unwrap();
        pollster::block_on(tx.commit("set up tagged history")).unwrap();

        let snapshot = test_backend().open(dir.path()).unwrap();

        // Both tags land on the tagged commit, in name order.
        let tagged_node = node_by_description(&snapshot, "release: cut 1.0");
        assert_eq!(tagged_node.tags, vec!["v1.0".to_owned(), "v1.0-rc1".to_owned()]);

        // Untagged commits carry none.
        let wc_node = snapshot
            .nodes
            .iter()
            .find(|n| n.id == snapshot.working_copy)
            .expect("working copy node present");
        assert!(wc_node.tags.is_empty());
    }

    #[test]
    fn view_lenses_draw_beyond_the_default_set() {
        let dir = tempfile::tempdir().unwrap();
        build_test_repo(dir.path());
        let backend = test_backend();

        // The default view is what plain `open` serves, echoing its lens.
        let default = backend.open(dir.path()).unwrap();
        assert_eq!(default.view, SnapshotView::Workstreams);
        assert!(!default.truncated);

        // `All` covers everything the default drew (this fixture's history
        // is shallow enough to be all-mutable anyway).
        let all = backend.open_with(dir.path(), &SnapshotView::All).unwrap();
        assert_eq!(all.view, SnapshotView::All);
        assert!(!all.truncated);
        for node in &default.nodes {
            assert!(
                all.nodes.iter().any(|n| n.id == node.id),
                "all() lost node {}",
                node.id
            );
        }

        // `Trunk` replaces the default set: trunk history plus the pinned
        // working copy. The side stack disappears; the feature change stops
        // being a row but grounds the working copy as its base — and keeps
        // reading as mutable, since the lens changes nothing about what jj
        // would let you rewrite.
        let trunk = backend.open_with(dir.path(), &SnapshotView::Trunk).unwrap();
        assert_eq!(trunk.view, SnapshotView::Trunk);
        assert!(
            !trunk.nodes.iter().any(|n| n.description == "wip: side experiment"),
            "side stack is not trunk history"
        );
        let wc_node = trunk
            .nodes
            .iter()
            .find(|n| n.id == trunk.working_copy)
            .expect("working copy stays pinned");
        assert_eq!(wc_node.kind, NodeKind::WorkingCopy);
        let feature = trunk
            .nodes
            .iter()
            .find(|n| n.description == "feat: first change")
            .expect("feature grounds the pinned working copy");
        assert_eq!(feature.kind, NodeKind::Mutable);
        assert!(
            trunk.nodes.iter().any(|n| n.description == "release: cut 1.0"),
            "trunk history drawn"
        );

        // Workstream summaries survive lens views (the sidebar still knows
        // the stacks), even when their rows are not drawn.
        assert_eq!(trunk.workstreams.len(), default.workstreams.len());
    }

    #[test]
    fn custom_view_answers_revsets_and_refuses_garbage() {
        let dir = tempfile::tempdir().unwrap();
        build_test_repo(dir.path());
        let backend = test_backend();

        let lens = SnapshotView::Custom {
            revset: "bookmarks(exact:\"feature-a\")".into(),
        };
        let custom = backend.open_with(dir.path(), &lens).unwrap();
        assert_eq!(custom.view, lens);
        let feature = custom
            .nodes
            .iter()
            .find(|n| n.description == "feat: first change")
            .expect("revset target drawn");
        assert_eq!(feature.kind, NodeKind::Mutable);
        assert!(
            custom.nodes.iter().any(|n| n.id == custom.working_copy),
            "working copy stays pinned"
        );
        assert!(
            !custom.nodes.iter().any(|n| n.description == "wip: side experiment"),
            "commits outside the revset stay out"
        );

        // Unparseable text refuses with the dedicated error so the UI can
        // render it inline; so does a symbol that fails to resolve.
        let err = backend
            .open_with(
                dir.path(),
                &SnapshotView::Custom { revset: "oops((".into() },
            )
            .unwrap_err();
        assert!(matches!(err, BackendError::RevsetInvalid(_)), "got {err:?}");
        let err = backend
            .open_with(
                dir.path(),
                &SnapshotView::Custom { revset: "no-such-bookmark".into() },
            )
            .unwrap_err();
        assert!(matches!(err, BackendError::RevsetInvalid(_)), "got {err:?}");
    }

    #[test]
    fn bases_link_through_elided_history() {
        // trunk1 ── trunk2 ── trunk3(main) ── wc; old stack on trunk1.
        // trunk2 has no mutable child, so it stays out of the snapshot and
        // trunk3 must reach trunk1 through an elided link.
        let dir = tempfile::tempdir().unwrap();
        let settings = test_settings();
        let (_workspace, repo) =
            pollster::block_on(Workspace::init_simple(&settings, dir.path())).unwrap();
        let root_commit_id = repo.store().root_commit_id().clone();
        let mut tx = repo.start_transaction();

        let trunk1 = write_commit(&mut tx, vec![root_commit_id], "trunk one");
        let trunk2 = write_commit(&mut tx, vec![trunk1.id().clone()], "trunk two");
        let trunk3 = write_commit(&mut tx, vec![trunk2.id().clone()], "trunk three");
        tx.repo_mut().set_local_bookmark_target(
            RefName::new("main"),
            RefTarget::normal(trunk3.id().clone()),
        );
        let wc = write_commit(&mut tx, vec![trunk3.id().clone()], "");
        pollster::block_on(tx.repo_mut().edit(WorkspaceName::DEFAULT.to_owned(), &wc)).unwrap();
        write_commit(&mut tx, vec![trunk1.id().clone()], "wip: old stack");
        pollster::block_on(tx.repo_mut().rebase_descendants()).unwrap();
        pollster::block_on(tx.commit("set up elided history")).unwrap();

        let snapshot = test_backend().open(dir.path()).unwrap();

        let find = |desc: &str| {
            snapshot
                .nodes
                .iter()
                .find(|n| n.description == desc)
                .unwrap_or_else(|| panic!("{desc} present"))
        };
        let t3 = find("trunk three");
        let t1 = find("trunk one");
        assert_eq!(t3.kind, NodeKind::Immutable);
        assert_eq!(t1.kind, NodeKind::Immutable);
        // trunk2 is not drawn anywhere.
        assert!(!snapshot.nodes.iter().any(|n| n.description == "trunk two"));

        // trunk3's direct parent is outside the snapshot, so it carries an
        // elided link to trunk1 instead of a drawn parent.
        assert!(t3.parents.is_empty());
        assert_eq!(t3.elided_parents, vec![t1.id.clone()]);
        // trunk1's parent (the root) is not drawn and nothing visible sits
        // below it: a true terminal elision.
        assert!(t1.parents.is_empty());
        assert!(t1.elided_parents.is_empty());
    }

    #[test]
    fn refresh_sees_new_operations() {
        let dir = tempfile::tempdir().unwrap();
        build_test_repo(dir.path());
        let first = test_backend().open(dir.path()).unwrap();

        // Mutate the repo out-of-band, as the CLI would.
        let settings = test_settings();
        let workspace = Workspace::load(
            &settings,
            dir.path(),
            &default_backend_factories(),
            &default_working_copy_factories(),
        )
        .unwrap();
        let repo = pollster::block_on(workspace.repo_loader().load_at_head()).unwrap();
        let mut tx = repo.start_transaction();
        let wc_id = repo
            .view()
            .get_wc_commit_id(WorkspaceName::DEFAULT)
            .unwrap()
            .clone();
        write_commit(&mut tx, vec![wc_id], "feat: stacked on top");
        pollster::block_on(tx.commit("write stacked change")).unwrap();

        let second = test_backend().open(dir.path()).unwrap();
        assert_eq!(second.operations[0].description, "write stacked change");
        assert!(second.nodes.len() > first.nodes.len());
    }

    #[test]
    fn operations_carry_user_and_effects() {
        let dir = tempfile::tempdir().unwrap();
        let settings = test_settings();
        let (_workspace, repo) =
            pollster::block_on(Workspace::init_simple(&settings, dir.path())).unwrap();
        let root_commit_id = repo.store().root_commit_id().clone();

        // Op 1: two commits, two bookmarks, working copy onto one of them.
        let mut tx = repo.start_transaction();
        let first = write_commit(&mut tx, vec![root_commit_id.clone()], "first change");
        let second = write_commit(&mut tx, vec![root_commit_id], "second change");
        tx.repo_mut()
            .set_local_bookmark_target(RefName::new("main"), RefTarget::normal(first.id().clone()));
        tx.repo_mut()
            .set_local_bookmark_target(RefName::new("temp"), RefTarget::normal(first.id().clone()));
        pollster::block_on(tx.repo_mut().edit(WorkspaceName::DEFAULT.to_owned(), &first)).unwrap();
        pollster::block_on(tx.repo_mut().rebase_descendants()).unwrap();
        let repo = pollster::block_on(tx.commit("set up bookmarks")).unwrap();

        // Op 2: move a bookmark to a different change, delete one, and move
        // the working copy to a different change.
        let mut tx = repo.start_transaction();
        tx.repo_mut().set_local_bookmark_target(
            RefName::new("main"),
            RefTarget::normal(second.id().clone()),
        );
        tx.repo_mut()
            .set_local_bookmark_target(RefName::new("temp"), RefTarget::absent());
        pollster::block_on(tx.repo_mut().edit(WorkspaceName::DEFAULT.to_owned(), &second)).unwrap();
        pollster::block_on(tx.repo_mut().rebase_descendants()).unwrap();
        let repo = pollster::block_on(tx.commit("rearrange bookmarks")).unwrap();

        // Op 3: rewrite the bookmarked commit in place (same change id, like
        // a describe). The bookmark and working copy follow the rewrite, but
        // at change granularity nothing moved.
        let mut tx = repo.start_transaction();
        let rewritten = tx
            .repo_mut()
            .rewrite_commit(&second)
            .set_description("second change, described")
            .write();
        pollster::block_on(rewritten).unwrap();
        pollster::block_on(tx.repo_mut().rebase_descendants()).unwrap();
        pollster::block_on(tx.commit("describe the change")).unwrap();

        let snapshot = test_backend().open(dir.path()).unwrap();
        let find = |desc: &str| {
            snapshot
                .operations
                .iter()
                .find(|op| op.description == desc)
                .unwrap_or_else(|| panic!("operation '{desc}' present"))
        };

        let setup = find("set up bookmarks");
        assert_eq!(setup.user, "test-user@test-host");
        assert!(!setup.is_snapshot);
        let setup_labels: Vec<&str> =
            setup.effects.iter().map(|e| e.label.as_str()).collect();
        assert_eq!(setup_labels, ["main created", "temp created", "working copy moved"]);
        assert_eq!(setup.effects[0].kind, OpEffectKind::Bookmark);
        assert_eq!(setup.effects[2].kind, OpEffectKind::WorkingCopy);
        assert_eq!(setup.more_effects, 0);

        let rearrange = find("rearrange bookmarks");
        let rearrange_labels: Vec<&str> =
            rearrange.effects.iter().map(|e| e.label.as_str()).collect();
        assert_eq!(rearrange_labels, ["main moved", "temp deleted", "working copy moved"]);

        // The in-place rewrite reports no effects: the bookmark and working
        // copy still point at the same change.
        let describe = find("describe the change");
        assert!(describe.effects.is_empty(), "got {:?}", describe.effects);
    }

    #[test]
    fn change_detail_classifies_files_against_parent() {
        let dir = tempfile::tempdir().unwrap();
        let settings = test_settings();
        let (_workspace, repo) =
            pollster::block_on(Workspace::init_simple(&settings, dir.path())).unwrap();
        let root_commit_id = repo.store().root_commit_id().clone();
        let store = repo.store().clone();
        let mut tx = repo.start_transaction();

        // base: adds two files; child: modifies one, adds one, removes one.
        let base_tree = file_tree(&store, &[("README.md", "hello\n"), ("src/main.rs", "fn main() {}\n")]);
        let base = write_commit_with_tree(&mut tx, vec![root_commit_id], "base", base_tree);
        let child_tree = file_tree(&store, &[("src/lib.rs", "pub fn lib() {}\n"), ("src/main.rs", "fn main() { lib(); }\n")]);
        let child = write_commit_with_tree(&mut tx, vec![base.id().clone()], "child", child_tree);
        pollster::block_on(tx.repo_mut().edit(WorkspaceName::DEFAULT.to_owned(), &child)).unwrap();
        pollster::block_on(tx.repo_mut().rebase_descendants()).unwrap();
        pollster::block_on(tx.commit("set up file history")).unwrap();

        let snapshot = test_backend().open(dir.path()).unwrap();
        let child_id = snapshot.working_copy.clone();
        let base_id = snapshot
            .nodes
            .iter()
            .find(|n| n.description == "base")
            .unwrap()
            .id
            .clone();

        let detail = test_backend().change_detail(dir.path(), &child_id).unwrap();
        assert_eq!(detail.id, child_id);
        assert!(!detail.truncated);
        let by_path: Vec<(&str, FileStatus)> = detail
            .files
            .iter()
            .map(|f| (f.path.as_str(), f.status))
            .collect();
        assert_eq!(
            by_path,
            vec![
                ("README.md", FileStatus::Removed),
                ("src/lib.rs", FileStatus::Added),
                ("src/main.rs", FileStatus::Modified),
            ]
        );
        assert!(detail.files.iter().all(|f| !f.has_conflict));

        // The base commit sits on the empty root tree: everything is an add.
        let base_detail = test_backend().change_detail(dir.path(), &base_id).unwrap();
        assert!(base_detail
            .files
            .iter()
            .all(|f| f.status == FileStatus::Added));
        assert_eq!(base_detail.files.len(), 2);

        // Unknown change ids surface as ChangeMissing, not a generic
        // failure — both unparseable ids and valid prefixes with no match.
        let err = test_backend().change_detail(dir.path(), "not-a-change-id").unwrap_err();
        assert!(matches!(err, BackendError::ChangeMissing(_)));
        let err = test_backend()
            .change_detail(dir.path(), "kkkkkkkkkkkkkkkk")
            .unwrap_err();
        assert!(matches!(err, BackendError::ChangeMissing(_)));
    }

    #[test]
    fn change_diff_builds_unified_hunks() {
        let dir = tempfile::tempdir().unwrap();
        let settings = test_settings();
        let (_workspace, repo) =
            pollster::block_on(Workspace::init_simple(&settings, dir.path())).unwrap();
        let root_commit_id = repo.store().root_commit_id().clone();
        let store = repo.store().clone();
        let mut tx = repo.start_transaction();

        let base_main = "fn main() {\n    let total = sum(1, 2);\n    println!(\"{total}\");\n}\n\nfn sum(a: i32, b: i32) -> i32 {\n    a + b\n}\n";
        let child_main = "fn main() {\n    let total = sum(3, 4);\n    println!(\"{total}\");\n}\n\nfn sum(a: i32, b: i32) -> i32 {\n    a + b\n}\n";

        let base_tree = file_tree(
            &store,
            &[("README.md", "hello\nworld\n"), ("src/main.rs", base_main)],
        );
        let base = write_commit_with_tree(&mut tx, vec![root_commit_id], "base", base_tree);
        let child_tree = file_tree(
            &store,
            &[
                ("empty.txt", b"" as &[u8]),
                ("firmware.bin", b"\x00\x01\x02\x03"),
                ("logo.png", b"\x89PNG\x00\x01binary"),
                ("src/main.rs", child_main.as_bytes()),
            ],
        );
        let child = write_commit_with_tree(&mut tx, vec![base.id().clone()], "child", child_tree);
        pollster::block_on(tx.repo_mut().edit(WorkspaceName::DEFAULT.to_owned(), &child)).unwrap();
        pollster::block_on(tx.repo_mut().rebase_descendants()).unwrap();
        pollster::block_on(tx.commit("set up diff history")).unwrap();

        let snapshot = test_backend().open(dir.path()).unwrap();
        let child_id = snapshot.working_copy.clone();

        let diff = test_backend().change_diff(dir.path(), &child_id).unwrap();
        assert_eq!(diff.id, child_id);
        assert!(!diff.truncated);
        let by_path: Vec<(&str, FileStatus)> = diff
            .files
            .iter()
            .map(|f| (f.path.as_str(), f.status))
            .collect();
        assert_eq!(
            by_path,
            vec![
                ("README.md", FileStatus::Removed),
                ("empty.txt", FileStatus::Added),
                ("firmware.bin", FileStatus::Added),
                ("logo.png", FileStatus::Added),
                ("src/main.rs", FileStatus::Modified),
            ]
        );
        let by_path: std::collections::HashMap<&str, &FileDiff> =
            diff.files.iter().map(|f| (f.path.as_str(), f)).collect();

        // A removed text file is one hunk of removed lines from line 1.
        let readme = by_path["README.md"];
        let FileDiffContent::Text { hunks, truncated } = &readme.content else {
            panic!("README.md should diff as text");
        };
        assert!(!truncated);
        assert_eq!(hunks.len(), 1);
        assert_eq!((hunks[0].old_start, hunks[0].new_start), (1, 1));
        assert!(hunks[0]
            .lines
            .iter()
            .all(|l| l.kind == DiffLineKind::Removed));
        let texts: Vec<String> = hunks[0]
            .lines
            .iter()
            .map(|l| l.segments.iter().map(|s| s.text.as_str()).collect())
            .collect();
        assert_eq!(texts, vec!["hello", "world"]);

        // An empty added file has no hunks but still diffs as text.
        assert_eq!(
            by_path["empty.txt"].content,
            FileDiffContent::Text {
                hunks: vec![],
                truncated: false
            }
        );

        // Binary bytes at an image path compare as pictures: the absent
        // old side stays None, the new side inlines its bytes as base64.
        assert_eq!(
            by_path["logo.png"].content,
            FileDiffContent::Image {
                old: None,
                new: Some(ImageContent {
                    mime: "image/png".to_owned(),
                    size: 12,
                    data: Some(
                        base64::engine::general_purpose::STANDARD
                            .encode(b"\x89PNG\x00\x01binary")
                    ),
                }),
            }
        );

        // Null bytes at a non-image path stay binary, reported by size.
        assert_eq!(
            by_path["firmware.bin"].content,
            FileDiffContent::Binary {
                old_size: None,
                new_size: Some(4),
            }
        );

        // The modified file gets context lines around the change and
        // word-level intraline segments on the changed pair.
        let main_rs = by_path["src/main.rs"];
        let FileDiffContent::Text { hunks, .. } = &main_rs.content else {
            panic!("src/main.rs should diff as text");
        };
        assert_eq!(hunks.len(), 1);
        assert_eq!((hunks[0].old_start, hunks[0].new_start), (1, 1));
        let kinds: Vec<DiffLineKind> = hunks[0].lines.iter().map(|l| l.kind).collect();
        assert_eq!(
            kinds,
            vec![
                DiffLineKind::Context,
                DiffLineKind::Removed,
                DiffLineKind::Added,
                DiffLineKind::Context,
                DiffLineKind::Context,
                DiffLineKind::Context,
            ]
        );
        let removed = &hunks[0].lines[1];
        assert!(removed.segments.iter().any(|s| s.changed));
        assert!(removed.segments.iter().any(|s| !s.changed));
        let removed_text: String = removed.segments.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(removed_text, "    let total = sum(1, 2);");
        // The changed pair links to each other; context lines don't pair.
        assert_eq!(hunks[0].lines[1].pair, Some(2));
        assert_eq!(hunks[0].lines[2].pair, Some(1));
        assert!(hunks[0]
            .lines
            .iter()
            .enumerate()
            .all(|(k, l)| l.pair.is_some() == (k == 1 || k == 2)));
        // No segment carries the layout newline.
        assert!(diff.files.iter().all(|f| match &f.content {
            FileDiffContent::Text { hunks, .. } => hunks
                .iter()
                .flat_map(|h| &h.lines)
                .flat_map(|l| &l.segments)
                .all(|s| !s.text.contains('\n')),
            _ => true,
        }));

        let err = test_backend().change_diff(dir.path(), "not-a-change-id").unwrap_err();
        assert!(matches!(err, BackendError::ChangeMissing(_)));
    }

    #[test]
    fn compare_diff_spans_the_changes_between_two_revisions() {
        let dir = tempfile::tempdir().unwrap();
        let settings = test_settings();
        let (_workspace, repo) =
            pollster::block_on(Workspace::init_simple(&settings, dir.path())).unwrap();
        let root_commit_id = repo.store().root_commit_id().clone();
        let store = repo.store().clone();
        let mut tx = repo.start_transaction();

        // base → mid → top: a file added mid-span then edited again, one
        // removed late, one untouched throughout.
        let base_tree = file_tree(&store, &[("keep.txt", "kept\n"), ("notes.md", "alpha\n")]);
        let base = write_commit_with_tree(&mut tx, vec![root_commit_id], "base", base_tree);
        let mid_tree = file_tree(
            &store,
            &[
                ("keep.txt", "kept\n"),
                ("notes.md", "alpha\nbeta\n"),
                ("new.rs", "fn new() {}\n"),
            ],
        );
        let mid = write_commit_with_tree(&mut tx, vec![base.id().clone()], "mid", mid_tree);
        let top_tree = file_tree(&store, &[("keep.txt", "kept\n"), ("new.rs", "fn newer() {}\n")]);
        let top = write_commit_with_tree(&mut tx, vec![mid.id().clone()], "top", top_tree);
        pollster::block_on(tx.repo_mut().edit(WorkspaceName::DEFAULT.to_owned(), &top)).unwrap();
        pollster::block_on(tx.repo_mut().rebase_descendants()).unwrap();
        pollster::block_on(tx.commit("set up compare history")).unwrap();

        let backend = test_backend();
        let snapshot = backend.open(dir.path()).unwrap();
        let base_id = node_by_description(&snapshot, "base").id.clone();
        let top_id = node_by_description(&snapshot, "top").id.clone();

        // Forward: one diff covering everything between the two trees. The
        // file added mid-span arrives as an add with its final content; the
        // late removal is a removal; the untouched file does not appear.
        let diff = backend.compare_diff(dir.path(), &base_id, &top_id).unwrap();
        assert_eq!(diff.id, top_id);
        assert_eq!(diff.from.as_deref(), Some(base_id.as_str()));
        assert!(!diff.truncated);
        let by_path: Vec<(&str, FileStatus)> =
            diff.files.iter().map(|f| (f.path.as_str(), f.status)).collect();
        assert_eq!(
            by_path,
            vec![("new.rs", FileStatus::Added), ("notes.md", FileStatus::Removed)]
        );
        let FileDiffContent::Text { hunks, .. } = &diff.files[0].content else {
            panic!("new.rs should diff as text");
        };
        let added: String = hunks
            .iter()
            .flat_map(|h| &h.lines)
            .flat_map(|l| &l.segments)
            .map(|s| s.text.as_str())
            .collect();
        assert_eq!(added, "fn newer() {}", "final tree state, not one change's edit");

        // From a descendant the same diff simply reads in reverse.
        let reversed = backend.compare_diff(dir.path(), &top_id, &base_id).unwrap();
        let by_path: Vec<(&str, FileStatus)> = reversed
            .files
            .iter()
            .map(|f| (f.path.as_str(), f.status))
            .collect();
        assert_eq!(
            by_path,
            vec![("new.rs", FileStatus::Removed), ("notes.md", FileStatus::Added)]
        );

        // A change against itself has nothing to show, and unknown ends
        // surface as the usual ChangeMissing.
        let same = backend.compare_diff(dir.path(), &top_id, &top_id).unwrap();
        assert!(same.files.is_empty());
        let err = backend
            .compare_diff(dir.path(), "kkkkkkkkkkkkkkkk", &top_id)
            .unwrap_err();
        assert!(matches!(err, BackendError::ChangeMissing(_)));
    }

    fn node_by_description<'a>(snapshot: &'a RepoSnapshot, description: &str) -> &'a GraphNode {
        snapshot
            .nodes
            .iter()
            .find(|n| n.description == description)
            .unwrap_or_else(|| panic!("node \u{201c}{description}\u{201d} present"))
    }

    #[test]
    fn search_history_reports_introductions_not_moves() {
        let dir = tempfile::tempdir().unwrap();
        let settings = test_settings();
        let (_workspace, repo) =
            pollster::block_on(Workspace::init_simple(&settings, dir.path())).unwrap();
        let root_commit_id = repo.store().root_commit_id().clone();
        let store = repo.store().clone();
        let mut tx = repo.start_transaction();

        // intro adds the token once; shuffle only moves it within the file
        // (same count — the pickaxe must skip it); dup adds a second copy
        // and a file whose *name* carries the token.
        let intro_tree = file_tree(&store, &[("a.rs", "fn alpha() {}\nmagic_token\n")]);
        let intro =
            write_commit_with_tree(&mut tx, vec![root_commit_id], "add the token", intro_tree);
        let shuffle_tree = file_tree(&store, &[("a.rs", "magic_token\nfn alpha() {}\n")]);
        let shuffle =
            write_commit_with_tree(&mut tx, vec![intro.id().clone()], "shuffle lines", shuffle_tree);
        let dup_tree = file_tree(
            &store,
            &[
                ("a.rs", "magic_token\nfn alpha() {}\nmagic_token\n"),
                ("docs/magic_token_notes.md", "notes\n"),
            ],
        );
        let dup =
            write_commit_with_tree(&mut tx, vec![shuffle.id().clone()], "duplicate the token", dup_tree);
        pollster::block_on(tx.repo_mut().edit(WorkspaceName::DEFAULT.to_owned(), &dup)).unwrap();
        pollster::block_on(tx.repo_mut().rebase_descendants()).unwrap();
        pollster::block_on(tx.commit("set up search history")).unwrap();

        let backend = test_backend();
        // Matching folds ASCII case both ways.
        let result = backend.search_history(dir.path(), "MAGIC_token").unwrap();
        assert_eq!(result.query, "MAGIC_token");
        assert!(!result.truncated);
        assert!(result.scanned >= 3, "walked the whole history: {}", result.scanned);
        let descriptions: Vec<&str> =
            result.hits.iter().map(|h| h.description.as_str()).collect();
        assert_eq!(
            descriptions,
            vec!["duplicate the token", "add the token"],
            "newest first, pure moves skipped"
        );

        // Hits address changes exactly like snapshot nodes, so jumping works.
        let snapshot = backend.open(dir.path()).unwrap();
        assert_eq!(result.hits[1].id, node_by_description(&snapshot, "add the token").id);

        let dup_hit = &result.hits[0];
        let a = dup_hit.files.iter().find(|f| f.path == "a.rs").unwrap();
        assert_eq!((a.added, a.removed, a.path_match), (2, 1, false));
        let notes = dup_hit
            .files
            .iter()
            .find(|f| f.path == "docs/magic_token_notes.md")
            .unwrap();
        assert!(notes.path_match, "filename matches count as hits");
        let intro_hit = &result.hits[1];
        let a = intro_hit.files.iter().find(|f| f.path == "a.rs").unwrap();
        assert_eq!((a.added, a.removed), (1, 0));

        // A path-only query finds every change touching that file.
        let by_path = backend.search_history(dir.path(), "a.rs").unwrap();
        let descriptions: Vec<&str> =
            by_path.hits.iter().map(|h| h.description.as_str()).collect();
        assert_eq!(
            descriptions,
            vec!["duplicate the token", "shuffle lines", "add the token"]
        );

        // Empty and blank queries answer empty without walking.
        let empty = backend.search_history(dir.path(), "   ").unwrap();
        assert!(empty.hits.is_empty());
        assert_eq!(empty.scanned, 0);
    }

    #[test]
    fn count_occurrences_folds_ascii_case_without_overlap() {
        assert_eq!(count_occurrences_ci(b"aaaa", b"aa"), 2, "non-overlapping");
        assert_eq!(count_occurrences_ci(b"FooBar foobar FOOBAR", b"foobar"), 3);
        assert_eq!(count_occurrences_ci(b"", b"x"), 0);
        assert_eq!(count_occurrences_ci(b"x", b""), 0);
        // Non-ASCII bytes compare exactly; ASCII around them still folds.
        assert_eq!(count_occurrences_ci("Grüße GRüSSE".as_bytes(), "grüße".as_bytes()), 1);
    }

    /// The on-disk working-copy state must track the operation a mutation
    /// created, or the next CLI command would report a stale workspace.
    fn assert_workspace_fresh(root: &Path) {
        let settings = test_settings();
        let workspace = Workspace::load(
            &settings,
            root,
            &default_backend_factories(),
            &default_working_copy_factories(),
        )
        .unwrap();
        let repo = pollster::block_on(workspace.repo_loader().load_at_head()).unwrap();
        assert_eq!(workspace.working_copy().operation_id(), repo.op_id());
    }

    #[test]
    fn describe_sets_description_and_keeps_workspace_fresh() {
        let dir = tempfile::tempdir().unwrap();
        build_test_repo(dir.path());
        let backend = test_backend();
        let before = backend.open(dir.path()).unwrap();
        let wc_id = before.working_copy.clone();

        let outcome = backend.describe(dir.path(), &wc_id, "wip: now described").unwrap();
        assert!(outcome.operation_id.is_some());
        assert_eq!(outcome.summary, format!("Described {wc_id}"));

        let after = backend.open(dir.path()).unwrap();
        // The change id is stable across the rewrite and stays the working copy.
        assert_eq!(after.working_copy, wc_id);
        let wc_node = node_by_description(&after, "wip: now described");
        assert_eq!(wc_node.id, wc_id);
        assert_eq!(wc_node.kind, NodeKind::WorkingCopy);

        // The mutation is one operation, current, named like the CLI's, and
        // an in-place rewrite reports no bookmark/working-copy effects.
        let op = &after.operations[0];
        assert!(op.description.starts_with("describe commit "));
        assert!(op.is_current);
        assert!(op.effects.is_empty(), "got {:?}", op.effects);
        assert_eq!(
            op.id,
            outcome.operation_id.unwrap(),
            "breadcrumb names the recorded operation"
        );

        assert_workspace_fresh(dir.path());
    }

    #[test]
    fn describe_mid_stack_rebases_descendants() {
        let dir = tempfile::tempdir().unwrap();
        build_test_repo(dir.path());
        let backend = test_backend();
        let before = backend.open(dir.path()).unwrap();
        let feature_id = node_by_description(&before, "feat: first change").id.clone();
        let wc_commit_before = node_by_description(&before, "").commit_id.clone();

        backend.describe(dir.path(), &feature_id, "feat: renamed").unwrap();

        let after = backend.open(dir.path()).unwrap();
        let feature = node_by_description(&after, "feat: renamed");
        assert_eq!(feature.id, feature_id);

        // The working copy rebased onto the rewritten parent: same change,
        // new commit, parent link intact.
        let wc_node = after
            .nodes
            .iter()
            .find(|n| n.id == after.working_copy)
            .expect("working copy present");
        assert_ne!(wc_node.commit_id, wc_commit_before);
        assert_eq!(wc_node.parents, vec![feature_id.clone()]);

        // The bookmark followed the rewrite (same change → no move effect).
        let bookmark = after.bookmarks.iter().find(|b| b.name == "feature-a").unwrap();
        assert_eq!(bookmark.target, feature_id);
        assert!(after.operations[0].effects.is_empty());

        assert_workspace_fresh(dir.path());
    }

    #[test]
    fn describe_refuses_immutable_and_unknown_changes() {
        let dir = tempfile::tempdir().unwrap();
        build_test_repo(dir.path());
        let backend = test_backend();
        let snapshot = backend.open(dir.path()).unwrap();

        // Trunk is immutable under the default aliases.
        let trunk_id = node_by_description(&snapshot, "release: cut 1.0").id.clone();
        let err = backend.describe(dir.path(), &trunk_id, "rewrite trunk").unwrap_err();
        assert!(matches!(err, BackendError::ImmutableChange(_)));

        // The root change id resolves but is always immutable.
        let err = backend.describe(dir.path(), "zzzzzzzz", "ghost").unwrap_err();
        assert!(matches!(err, BackendError::ImmutableChange(_)));

        let err = backend
            .describe(dir.path(), "kkkkkkkkkkkkkkkk", "ghost")
            .unwrap_err();
        assert!(matches!(err, BackendError::ChangeMissing(_)));

        // Nothing was recorded.
        let after = backend.open(dir.path()).unwrap();
        assert_eq!(after.operations[0].description, "set up test stack");
    }

    #[test]
    fn describe_with_same_description_is_a_noop() {
        let dir = tempfile::tempdir().unwrap();
        build_test_repo(dir.path());
        let backend = test_backend();
        let snapshot = backend.open(dir.path()).unwrap();
        let wc_id = snapshot.working_copy.clone();

        backend.describe(dir.path(), &wc_id, "wip: stable text").unwrap();
        let described = backend.open(dir.path()).unwrap();

        let outcome = backend.describe(dir.path(), &wc_id, "wip: stable text").unwrap();
        assert!(outcome.operation_id.is_none());
        let after = backend.open(dir.path()).unwrap();
        assert_eq!(after.operations.len(), described.operations.len());
    }

    /// Materializes the current working-copy commit on disk so the
    /// workspace state matches the repo head, like a CLI checkout would.
    fn check_out_working_copy(root: &Path) {
        let settings = test_settings();
        let mut workspace = Workspace::load(
            &settings,
            root,
            &default_backend_factories(),
            &default_working_copy_factories(),
        )
        .unwrap();
        let repo = pollster::block_on(workspace.repo_loader().load_at_head()).unwrap();
        let wc_id = repo
            .view()
            .get_wc_commit_id(WorkspaceName::DEFAULT)
            .unwrap()
            .clone();
        let commit = repo.store().get_commit(&wc_id).unwrap();
        pollster::block_on(workspace.check_out(repo.op_id().clone(), None, &commit)).unwrap();
    }

    #[test]
    fn new_change_starts_empty_working_copy_even_on_immutable_parents() {
        let dir = tempfile::tempdir().unwrap();
        build_test_repo(dir.path());
        let backend = test_backend();
        let before = backend.open(dir.path()).unwrap();
        let old_wc = before.working_copy.clone();
        let trunk_id = node_by_description(&before, "release: cut 1.0").id.clone();

        // Trunk is immutable; starting a new change on it is still allowed —
        // nothing is rewritten.
        let outcome = backend.new_change(dir.path(), &trunk_id).unwrap();
        assert!(outcome.operation_id.is_some());

        let after = backend.open(dir.path()).unwrap();
        let new_wc = after.working_copy.clone();
        assert_ne!(new_wc, old_wc);
        assert_eq!(outcome.target_change.as_deref(), Some(new_wc.as_str()));
        assert_eq!(outcome.summary, format!("Started {new_wc} on {trunk_id}"));

        let wc_node = after.nodes.iter().find(|n| n.id == new_wc).unwrap();
        assert_eq!(wc_node.kind, NodeKind::WorkingCopy);
        assert_eq!(wc_node.parents, vec![trunk_id]);
        assert!(wc_node.is_empty);
        assert!(wc_node.description.is_empty());

        // The old working copy was empty and undescribed — discardable, so
        // moving away abandoned it, like the CLI.
        assert!(!after.nodes.iter().any(|n| n.id == old_wc));

        assert_eq!(after.operations[0].description, "new empty commit");
        assert!(after.operations[0].is_current);
        assert_workspace_fresh(dir.path());
    }

    #[test]
    fn edit_moves_the_working_copy() {
        let dir = tempfile::tempdir().unwrap();
        build_test_repo(dir.path());
        let backend = test_backend();
        let before = backend.open(dir.path()).unwrap();
        let old_wc = before.working_copy.clone();
        let feature_id = node_by_description(&before, "feat: first change").id.clone();

        let outcome = backend.edit_change(dir.path(), &feature_id).unwrap();
        assert!(outcome.operation_id.is_some());
        assert_eq!(outcome.summary, format!("Editing {feature_id}"));
        assert_eq!(outcome.target_change.as_deref(), Some(feature_id.as_str()));

        let after = backend.open(dir.path()).unwrap();
        assert_eq!(after.working_copy, feature_id);
        let feature = node_by_description(&after, "feat: first change");
        assert_eq!(feature.kind, NodeKind::WorkingCopy);
        // The discardable old working copy was abandoned on the way out.
        assert!(!after.nodes.iter().any(|n| n.id == old_wc));
        assert!(after.operations[0]
            .description
            .starts_with("edit commit "));
        assert_workspace_fresh(dir.path());

        // Editing the change you are already on is a no-op.
        let outcome = backend.edit_change(dir.path(), &feature_id).unwrap();
        assert!(outcome.operation_id.is_none());

        // Immutable changes refuse, like the CLI.
        let trunk_id = node_by_description(&after, "release: cut 1.0").id.clone();
        let err = backend.edit_change(dir.path(), &trunk_id).unwrap_err();
        assert!(matches!(err, BackendError::ImmutableChange(_)));
    }

    #[test]
    fn abandon_mid_stack_rebases_descendants_and_deletes_bookmarks() {
        let dir = tempfile::tempdir().unwrap();
        build_test_repo(dir.path());
        let backend = test_backend();
        let before = backend.open(dir.path()).unwrap();
        let wc_id = before.working_copy.clone();
        let feature_id = node_by_description(&before, "feat: first change").id.clone();
        let trunk_id = node_by_description(&before, "release: cut 1.0").id.clone();

        let outcome = backend.abandon_change(dir.path(), &feature_id).unwrap();
        assert_eq!(outcome.summary, format!("Abandoned {feature_id}"));
        assert_eq!(outcome.target_change.as_deref(), Some(trunk_id.as_str()));

        let after = backend.open(dir.path()).unwrap();
        // The change is gone; its child (the working copy) rebased onto trunk.
        assert!(!after.nodes.iter().any(|n| n.id == feature_id));
        let wc_node = after.nodes.iter().find(|n| n.id == wc_id).unwrap();
        assert_eq!(wc_node.parents, vec![trunk_id]);
        // Its bookmark is deleted, not silently retargeted.
        assert!(!after.bookmarks.iter().any(|b| b.name == "feature-a"));
        assert!(after.operations[0]
            .description
            .starts_with("abandon commit "));
        let labels: Vec<&str> = after.operations[0]
            .effects
            .iter()
            .map(|e| e.label.as_str())
            .collect();
        assert!(labels.contains(&"feature-a deleted"), "got {labels:?}");
        assert_workspace_fresh(dir.path());
    }

    #[test]
    fn abandon_of_working_copy_respawns_on_parent() {
        let dir = tempfile::tempdir().unwrap();
        build_test_repo(dir.path());
        let backend = test_backend();
        let before = backend.open(dir.path()).unwrap();
        let wc_id = before.working_copy.clone();
        let feature_id = node_by_description(&before, "feat: first change").id.clone();

        let outcome = backend.abandon_change(dir.path(), &wc_id).unwrap();
        assert_eq!(outcome.target_change.as_deref(), Some(feature_id.as_str()));

        let after = backend.open(dir.path()).unwrap();
        assert_ne!(after.working_copy, wc_id);
        let wc_node = after.nodes.iter().find(|n| n.id == after.working_copy).unwrap();
        assert_eq!(wc_node.parents, vec![feature_id]);
        assert!(wc_node.is_empty);
        assert_workspace_fresh(dir.path());
    }

    #[test]
    fn abandon_changes_sweeps_a_chain_as_one_operation() {
        let dir = tempfile::tempdir().unwrap();
        build_test_repo(dir.path());
        let backend = test_backend();
        let before = backend.open(dir.path()).unwrap();
        let wc_id = before.working_copy.clone();
        let feature_id = node_by_description(&before, "feat: first change").id.clone();
        let trunk_id = node_by_description(&before, "release: cut 1.0").id.clone();
        let wc_commit = before
            .nodes
            .iter()
            .find(|n| n.id == wc_id)
            .unwrap()
            .commit_id
            .clone();
        let op_count = before.operations.len();

        // Oldest-first on purpose: the op description quotes the
        // topologically newest commit regardless of call order (probed
        // against jj 0.41).
        let outcome = backend
            .abandon_changes(dir.path(), &[feature_id.clone(), wc_id.clone()])
            .unwrap();
        assert_eq!(outcome.summary, format!("Abandoned {feature_id}, {wc_id}"));
        assert_eq!(outcome.target_change.as_deref(), Some(trunk_id.as_str()));

        let after = backend.open(dir.path()).unwrap();
        assert!(!after.nodes.iter().any(|n| n.id == feature_id));
        // One operation swept the chain, described like the CLI.
        assert_eq!(after.operations.len(), op_count + 1);
        let description = &after.operations[0].description;
        let quoted = description
            .strip_prefix("abandon commit ")
            .unwrap_or_else(|| panic!("unexpected op description {description:?}"));
        assert!(quoted.starts_with(&wc_commit), "got {description:?}");
        assert!(description.ends_with(" and 1 more"), "got {description:?}");
        // The bookmark on the abandoned change is deleted, and a fresh
        // working copy respawned on the nearest surviving ancestor.
        assert!(!after.bookmarks.iter().any(|b| b.name == "feature-a"));
        assert_ne!(after.working_copy, wc_id);
        let wc_node = after
            .nodes
            .iter()
            .find(|n| n.id == after.working_copy)
            .unwrap();
        assert_eq!(wc_node.parents, vec![trunk_id.clone()]);
        assert_workspace_fresh(dir.path());

        // Refusals record nothing: an empty list is a stale plan, and an
        // immutable member refuses the whole sweep.
        let err = backend.abandon_changes(dir.path(), &[]).unwrap_err();
        assert!(matches!(err, BackendError::MutationFailed(_)));
        let err = backend
            .abandon_changes(dir.path(), std::slice::from_ref(&trunk_id))
            .unwrap_err();
        assert!(matches!(err, BackendError::ImmutableChange(_)));
        let unchanged = backend.open(dir.path()).unwrap();
        assert_eq!(unchanged.operations.len(), op_count + 1);

        // Duplicates collapse like the CLI's revset union: the same change
        // twice is one abandon with the single-form description.
        let side_id = node_by_description(&unchanged, "wip: side experiment")
            .id
            .clone();
        let outcome = backend
            .abandon_changes(dir.path(), &[side_id.clone(), side_id.clone()])
            .unwrap();
        assert_eq!(outcome.summary, format!("Abandoned {side_id}"));
        let final_snapshot = backend.open(dir.path()).unwrap();
        assert!(final_snapshot.operations[0]
            .description
            .starts_with("abandon commit "));
        assert!(!final_snapshot.operations[0].description.contains(" more"));
    }

    #[test]
    fn squash_folds_change_into_parent() {
        let dir = tempfile::tempdir().unwrap();
        let settings = test_settings();
        let (_workspace, repo) =
            pollster::block_on(Workspace::init_simple(&settings, dir.path())).unwrap();
        let root_commit_id = repo.store().root_commit_id().clone();
        let store = repo.store().clone();
        let mut tx = repo.start_transaction();

        let base_tree = file_tree(&store, &[("a.txt", "one\n")]);
        let base = write_commit_with_tree(&mut tx, vec![root_commit_id], "feat: base work", base_tree);
        let child_tree = file_tree(&store, &[("a.txt", "two\n"), ("b.txt", "bee\n")]);
        let child =
            write_commit_with_tree(&mut tx, vec![base.id().clone()], "fix: tweak", child_tree);
        tx.repo_mut().set_local_bookmark_target(
            RefName::new("topic"),
            RefTarget::normal(child.id().clone()),
        );
        pollster::block_on(tx.repo_mut().edit(WorkspaceName::DEFAULT.to_owned(), &child)).unwrap();
        pollster::block_on(tx.repo_mut().rebase_descendants()).unwrap();
        pollster::block_on(tx.commit("set up squash stack")).unwrap();
        check_out_working_copy(dir.path());

        let backend = test_backend();
        let before = backend.open(dir.path()).unwrap();
        let child_id = before.working_copy.clone();
        let base_id = node_by_description(&before, "feat: base work").id.clone();

        let outcome = backend.squash_change(dir.path(), &child_id).unwrap();
        assert_eq!(outcome.summary, format!("Squashed {child_id} into {base_id}"));
        assert_eq!(outcome.target_change.as_deref(), Some(base_id.as_str()));

        let after = backend.open(dir.path()).unwrap();
        // The parent now carries both descriptions and the combined content.
        let combined = node_by_description(&after, "feat: base work\n\nfix: tweak");
        assert_eq!(combined.id, base_id);
        let detail = backend.change_detail(dir.path(), &base_id).unwrap();
        let paths: Vec<&str> = detail.files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, vec!["a.txt", "b.txt"]);
        // The squashed change is gone; the bookmark moved to the parent; the
        // working copy respawned as a new empty change on it.
        assert!(!after.nodes.iter().any(|n| n.id == child_id));
        let topic = after.bookmarks.iter().find(|b| b.name == "topic").unwrap();
        assert_eq!(topic.target, base_id);
        assert_ne!(after.working_copy, child_id);
        let wc_node = after.nodes.iter().find(|n| n.id == after.working_copy).unwrap();
        assert_eq!(wc_node.parents, vec![base_id]);
        assert!(wc_node.is_empty);
        assert!(after.operations[0]
            .description
            .starts_with("squash commits into "));
        assert_workspace_fresh(dir.path());
    }

    #[test]
    fn restore_file_discards_one_files_changes() {
        let dir = tempfile::tempdir().unwrap();
        let settings = test_settings();
        let (_workspace, repo) =
            pollster::block_on(Workspace::init_simple(&settings, dir.path())).unwrap();
        let root_commit_id = repo.store().root_commit_id().clone();
        let store = repo.store().clone();
        let mut tx = repo.start_transaction();

        let base_tree = file_tree(&store, &[("a.txt", "one\n")]);
        let base = write_commit_with_tree(&mut tx, vec![root_commit_id], "feat: base work", base_tree);
        let child_tree = file_tree(&store, &[("a.txt", "two\n"), ("b.txt", "bee\n")]);
        let child =
            write_commit_with_tree(&mut tx, vec![base.id().clone()], "fix: tweak", child_tree);
        pollster::block_on(tx.repo_mut().edit(WorkspaceName::DEFAULT.to_owned(), &child)).unwrap();
        pollster::block_on(tx.repo_mut().rebase_descendants()).unwrap();
        pollster::block_on(tx.commit("set up restore stack")).unwrap();
        check_out_working_copy(dir.path());

        let backend = test_backend();
        let before = backend.open(dir.path()).unwrap();
        let child_id = before.working_copy.clone();

        let outcome = backend.restore_file(dir.path(), &child_id, "a.txt", None).unwrap();
        assert_eq!(
            outcome.summary,
            format!("Discarded changes to a.txt in {child_id}")
        );
        assert!(outcome.operation_id.is_some());

        // Only b.txt still differs from the parent; a.txt went back to the
        // parent's content.
        let detail = backend.change_detail(dir.path(), &child_id).unwrap();
        let paths: Vec<&str> = detail.files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, vec!["b.txt"]);

        // Restoring an untouched file records nothing.
        let noop = backend.restore_file(dir.path(), &child_id, "a.txt", None).unwrap();
        assert!(noop.operation_id.is_none());
        assert_workspace_fresh(dir.path());
    }

    #[test]
    fn change_evolog_reports_the_predecessor_chain() {
        let dir = tempfile::tempdir().unwrap();
        build_test_repo(dir.path());
        let backend = test_backend();
        let before = backend.open(dir.path()).unwrap();
        let feature_id = before
            .nodes
            .iter()
            .find(|n| n.description == "feat: first change")
            .unwrap()
            .id
            .clone();

        // A never-rewritten change answers just its current version, wearing
        // the operation that created it.
        let evolog = backend.change_evolog(dir.path(), &feature_id).unwrap();
        assert_eq!(evolog.id, feature_id);
        assert_eq!(evolog.entries.len(), 1);
        assert_eq!(evolog.entries[0].description, "feat: first change");
        assert_eq!(
            evolog.entries[0].op_description.as_deref(),
            Some("set up test stack")
        );
        assert!(!evolog.truncated);

        // Each rewrite stacks a version on top with its op metadata.
        backend.describe(dir.path(), &feature_id, "feat: better title").unwrap();
        let evolog = backend.change_evolog(dir.path(), &feature_id).unwrap();
        assert_eq!(evolog.entries.len(), 2);
        assert_eq!(evolog.entries[0].description, "feat: better title");
        assert!(evolog.entries[0]
            .op_description
            .as_deref()
            .unwrap()
            .starts_with("describe commit"));
        assert!(evolog.entries[0].op_id.is_some());
        assert!(!evolog.entries[0].op_timestamp.as_deref().unwrap().is_empty());
        assert_eq!(evolog.entries[1].description, "feat: first change");
        assert_ne!(evolog.entries[0].commit_id, evolog.entries[1].commit_id);

        // ("zzzzzzzz" would be the root change; pick a prefix that cannot
        // resolve in this repo instead.)
        let err = backend.change_evolog(dir.path(), "qponmlkz").unwrap_err();
        assert!(matches!(err, BackendError::ChangeMissing(_)));
    }

    #[test]
    fn restore_change_version_round_trips_and_undoes() {
        let dir = tempfile::tempdir().unwrap();
        let settings = test_settings();
        let (_workspace, repo) =
            pollster::block_on(Workspace::init_simple(&settings, dir.path())).unwrap();
        let root_commit_id = repo.store().root_commit_id().clone();
        let store = repo.store().clone();
        let mut tx = repo.start_transaction();

        let tree = file_tree(&store, &[("notes.txt", "hello\n")]);
        let feature = write_commit_with_tree(&mut tx, vec![root_commit_id], "feat: v1", tree);
        pollster::block_on(tx.repo_mut().edit(WorkspaceName::DEFAULT.to_owned(), &feature))
            .unwrap();
        pollster::block_on(tx.repo_mut().rebase_descendants()).unwrap();
        pollster::block_on(tx.commit("set up evolog stack")).unwrap();
        check_out_working_copy(dir.path());

        let backend = test_backend();
        let before = backend.open(dir.path()).unwrap();
        let change_id = before.working_copy.clone();
        let original_commit = before
            .nodes
            .iter()
            .find(|n| n.id == change_id)
            .unwrap()
            .commit_id
            .clone();

        // Two rewrites: a new description, then the file's diff discarded.
        backend.describe(dir.path(), &change_id, "feat: v2").unwrap();
        backend.restore_file(dir.path(), &change_id, "notes.txt", None).unwrap();
        let evolog = backend.change_evolog(dir.path(), &change_id).unwrap();
        assert_eq!(evolog.entries.len(), 3);
        assert!(backend.change_detail(dir.path(), &change_id).unwrap().files.is_empty());

        // Restore to the original version: tree and description both return.
        let outcome = backend
            .restore_change_version(dir.path(), &change_id, &original_commit)
            .unwrap();
        assert_eq!(
            outcome.summary,
            format!("Restored {change_id} to version {original_commit}")
        );
        let op = outcome.operation_id.clone().unwrap();
        let after = backend.open(dir.path()).unwrap();
        let node = after.nodes.iter().find(|n| n.id == change_id).unwrap();
        assert_eq!(node.description, "feat: v1");
        let paths: Vec<String> = backend
            .change_detail(dir.path(), &change_id)
            .unwrap()
            .files
            .into_iter()
            .map(|f| f.path)
            .collect();
        assert_eq!(paths, vec!["notes.txt"]);
        assert!(after.operations[0].description.starts_with("restore commit"));
        let evolog = backend.change_evolog(dir.path(), &change_id).unwrap();
        assert_eq!(evolog.entries.len(), 4);
        assert_eq!(evolog.entries[0].description, "feat: v1");

        // Restoring to what the change already carries records nothing.
        let noop = backend
            .restore_change_version(dir.path(), &change_id, &original_commit)
            .unwrap();
        assert!(noop.operation_id.is_none());

        // The op log undoes it like any other mutation.
        backend.revert_operation(dir.path(), &op).unwrap();
        let reverted = backend.open(dir.path()).unwrap();
        let node = reverted.nodes.iter().find(|n| n.id == change_id).unwrap();
        assert_eq!(node.description, "feat: v2");
        assert!(backend.change_detail(dir.path(), &change_id).unwrap().files.is_empty());
        assert_workspace_fresh(dir.path());
    }

    #[test]
    fn restore_change_version_refuses_foreign_versions() {
        let dir = tempfile::tempdir().unwrap();
        build_test_repo(dir.path());
        let backend = test_backend();
        let before = backend.open(dir.path()).unwrap();
        let feature_id = before
            .nodes
            .iter()
            .find(|n| n.description == "feat: first change")
            .unwrap()
            .id
            .clone();
        let trunk_node = before
            .nodes
            .iter()
            .find(|n| n.description == "release: cut 1.0")
            .unwrap()
            .clone();

        // Another change's commit is not a version of this one.
        let err = backend
            .restore_change_version(dir.path(), &feature_id, &trunk_node.commit_id)
            .unwrap_err();
        assert!(err.to_string().contains("not an earlier version"), "got {err}");

        // A commit id resolving to nothing refuses with the stale story.
        let err = backend
            .restore_change_version(dir.path(), &feature_id, "feedfacefeed")
            .unwrap_err();
        assert!(err.to_string().contains("no version"), "got {err}");

        // Immutable changes refuse before any version is looked up.
        let err = backend
            .restore_change_version(dir.path(), &trunk_node.id, &trunk_node.commit_id)
            .unwrap_err();
        assert!(matches!(err, BackendError::ImmutableChange(_)), "got {err:?}");

        // Nothing was recorded by any refusal.
        assert_eq!(
            backend.open(dir.path()).unwrap().operations[0].description,
            "set up test stack"
        );
    }

    #[test]
    fn file_annotate_attributes_lines_to_their_changes() {
        let dir = tempfile::tempdir().unwrap();
        let settings = test_settings();
        let (_workspace, repo) =
            pollster::block_on(Workspace::init_simple(&settings, dir.path())).unwrap();
        let root_commit_id = repo.store().root_commit_id().clone();
        let store = repo.store().clone();
        let mut tx = repo.start_transaction();

        let base = write_commit_with_tree(
            &mut tx,
            vec![root_commit_id],
            "feat: base lines",
            file_tree(&store, &[("src/notes.txt", b"alpha\nbeta\ngamma\n" as &[u8])]),
        );
        let edit = write_commit_with_tree(
            &mut tx,
            vec![base.id().clone()],
            "feat: rework beta\n\nwhy: beta needed work\n",
            file_tree(
                &store,
                &[
                    ("src/notes.txt", b"alpha\nbeta reworked\ngamma\ndelta\n" as &[u8]),
                    ("logo.bin", b"\x00\x01\x02" as &[u8]),
                ],
            ),
        );
        pollster::block_on(tx.repo_mut().edit(WorkspaceName::DEFAULT.to_owned(), &edit)).unwrap();
        pollster::block_on(tx.repo_mut().rebase_descendants()).unwrap();
        pollster::block_on(tx.commit("set up annotate stack")).unwrap();
        check_out_working_copy(dir.path());

        let backend = test_backend();
        let snapshot = backend.open(dir.path()).unwrap();
        let base_node = node_by_description(&snapshot, "feat: base lines").clone();
        let edit_node = snapshot
            .nodes
            .iter()
            .find(|n| n.description.starts_with("feat: rework beta"))
            .unwrap()
            .clone();

        // At the edited change, each line wears the change that introduced
        // it; origins list in first-appearance order and summaries are the
        // description's first line.
        let annotation = backend
            .file_annotate(dir.path(), &edit_node.id, "src/notes.txt")
            .unwrap();
        assert_eq!(annotation.id, edit_node.id);
        assert_eq!(annotation.path, "src/notes.txt");
        assert!(!annotation.truncated);
        let texts: Vec<&str> = annotation.lines.iter().map(|l| l.text.as_str()).collect();
        assert_eq!(texts, vec!["alpha", "beta reworked", "gamma", "delta"]);
        let origins: Vec<u32> = annotation.lines.iter().map(|l| l.origin).collect();
        assert_eq!(origins, vec![0, 1, 0, 1]);
        assert_eq!(annotation.origins.len(), 2);
        assert_eq!(annotation.origins[0].id, base_node.id);
        assert_eq!(annotation.origins[0].change_id, base_node.id);
        assert_eq!(annotation.origins[0].commit_id, base_node.commit_id);
        assert_eq!(annotation.origins[0].summary, "feat: base lines");
        assert_eq!(annotation.origins[0].author, "Test User");
        assert!(!annotation.origins[0].timestamp.is_empty());
        assert_eq!(annotation.origins[1].id, edit_node.id);
        assert_eq!(annotation.origins[1].summary, "feat: rework beta");

        // At the base change the file is all its own.
        let annotation = backend
            .file_annotate(dir.path(), &base_node.id, "src/notes.txt")
            .unwrap();
        assert_eq!(annotation.lines.len(), 3);
        assert_eq!(annotation.origins.len(), 1);
        assert_eq!(annotation.origins[0].id, base_node.id);
        assert!(annotation.lines.iter().all(|l| l.origin == 0));

        // Refusals: a binary file, a path absent from the tree, an invalid
        // path, and an unknown change.
        let err = backend
            .file_annotate(dir.path(), &edit_node.id, "logo.bin")
            .unwrap_err();
        assert!(err.to_string().contains("binary"), "got {err}");
        let err = backend
            .file_annotate(dir.path(), &base_node.id, "logo.bin")
            .unwrap_err();
        assert!(err.to_string().contains("does not exist"), "got {err}");
        // ".." is just an (absent) tree key — no traversal happens; an
        // absolute path fails jj-lib's validation outright.
        let err = backend
            .file_annotate(dir.path(), &edit_node.id, "../outside.txt")
            .unwrap_err();
        assert!(err.to_string().contains("does not exist"), "got {err}");
        let err = backend
            .file_annotate(dir.path(), &edit_node.id, "/etc/passwd")
            .unwrap_err();
        assert!(err.to_string().contains("not a valid repository path"), "got {err}");
        let err = backend
            .file_annotate(dir.path(), "qponmlkz", "src/notes.txt")
            .unwrap_err();
        assert!(matches!(err, BackendError::ChangeMissing(_)), "got {err:?}");
    }

    #[test]
    fn file_annotate_refuses_conflicted_files() {
        let dir = tempfile::tempdir().unwrap();
        let backend = test_backend();
        let (_ours_id, theirs_id, _child_id) = build_conflicted_repo(dir.path(), &backend);
        let err = backend
            .file_annotate(dir.path(), &theirs_id, "notes.txt")
            .unwrap_err();
        assert!(err.to_string().contains("conflicted"), "got {err}");
    }

    #[test]
    fn restore_hunks_discards_only_the_selected_hunks() {
        let dir = tempfile::tempdir().unwrap();
        let settings = test_settings();
        let (_workspace, repo) =
            pollster::block_on(Workspace::init_simple(&settings, dir.path())).unwrap();
        let root_commit_id = repo.store().root_commit_id().clone();
        let store = repo.store().clone();
        let mut tx = repo.start_transaction();

        // Two edits far enough apart for two hunks at 3 context lines.
        let old_notes = "n1\nn2\nn3\nn4\nn5\nn6\nn7\nn8\nn9\nn10\nn11\nn12\n";
        let new_notes =
            "n1\nsecond changed\nn3\nn4\nn5\nn6\nn7\nn8\nn9\nn10\neleventh changed\nn12\n";
        let base_tree = file_tree(&store, &[("notes.txt", old_notes)]);
        let base = write_commit_with_tree(&mut tx, vec![root_commit_id], "feat: base", base_tree);
        let child_tree = file_tree(&store, &[("notes.txt", new_notes)]);
        let child =
            write_commit_with_tree(&mut tx, vec![base.id().clone()], "two edits", child_tree);
        pollster::block_on(tx.repo_mut().edit(WorkspaceName::DEFAULT.to_owned(), &child)).unwrap();
        pollster::block_on(tx.repo_mut().rebase_descendants()).unwrap();
        pollster::block_on(tx.commit("set up hunk restore")).unwrap();
        check_out_working_copy(dir.path());

        let backend = test_backend();
        let before = backend.open(dir.path()).unwrap();
        let child_id = before.working_copy.clone();
        let diff = backend.change_diff(dir.path(), &child_id).unwrap();
        let hunks = text_hunks(&diff, "notes.txt").to_vec();
        assert_eq!(hunks.len(), 2);

        // Refusals record nothing: stale coordinates (the change moved
        // under the surface), an empty hunk list, and a file the change
        // never touches.
        let stale = || SplitHunk {
            old_start: 999,
            new_start: 999,
            old_lines: 1,
            new_lines: 1,
            lines: None,
        };
        let err = backend
            .restore_file(dir.path(), &child_id, "notes.txt", Some(&[stale()]))
            .unwrap_err();
        assert!(err.to_string().contains("no longer matches"), "got {err}");
        let err = backend
            .restore_file(dir.path(), &child_id, "notes.txt", Some(&[]))
            .unwrap_err();
        assert!(err.to_string().contains("no hunks selected"), "got {err}");
        let err = backend
            .restore_file(dir.path(), &child_id, "elsewhere.txt", Some(&[stale()]))
            .unwrap_err();
        assert!(err.to_string().contains("no longer matches"), "got {err}");
        assert_eq!(
            backend.open(dir.path()).unwrap().operations[0].description,
            "set up hunk restore"
        );

        // Discard only the second edit: the first stays, and the working
        // copy file on disk (this is @) loses just that hunk.
        let outcome = backend
            .restore_file(dir.path(), &child_id, "notes.txt", Some(&[hunk_coords(&hunks[1])]))
            .unwrap();
        assert_eq!(
            outcome.summary,
            format!("Discarded 1 hunk of notes.txt in {child_id}")
        );
        assert!(outcome.operation_id.is_some());
        let after = backend.open(dir.path()).unwrap();
        assert!(after.operations[0]
            .description
            .starts_with("restore 1 hunk of notes.txt in commit "));
        let after_diff = backend.change_diff(dir.path(), &child_id).unwrap();
        let after_hunks = text_hunks(&after_diff, "notes.txt");
        assert_eq!(after_hunks.len(), 1);
        assert_eq!(
            line_texts(&after_hunks[0], DiffLineKind::Added),
            vec!["second changed"]
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("notes.txt")).unwrap(),
            "n1\nsecond changed\nn3\nn4\nn5\nn6\nn7\nn8\nn9\nn10\nn11\nn12\n"
        );

        // Selecting every remaining hunk is the whole-file restore: the
        // file drops out of the change and the phrasing says so.
        let remaining = text_hunks(&after_diff, "notes.txt").to_vec();
        let coords: Vec<SplitHunk> = remaining.iter().map(hunk_coords).collect();
        let outcome = backend
            .restore_file(dir.path(), &child_id, "notes.txt", Some(&coords))
            .unwrap();
        assert_eq!(
            outcome.summary,
            format!("Discarded changes to notes.txt in {child_id}")
        );
        let detail = backend.change_detail(dir.path(), &child_id).unwrap();
        assert!(detail.files.is_empty());
        assert_eq!(
            std::fs::read_to_string(dir.path().join("notes.txt")).unwrap(),
            old_notes
        );
        assert_workspace_fresh(dir.path());
    }

    #[test]
    fn duplicate_copies_a_change_in_place() {
        let dir = tempfile::tempdir().unwrap();
        let settings = test_settings();
        let (_workspace, repo) =
            pollster::block_on(Workspace::init_simple(&settings, dir.path())).unwrap();
        let root_commit_id = repo.store().root_commit_id().clone();
        let store = repo.store().clone();
        let mut tx = repo.start_transaction();

        let base_tree = file_tree(&store, &[("a.txt", "one\n")]);
        let base = write_commit_with_tree(&mut tx, vec![root_commit_id], "feat: base work", base_tree);
        tx.repo_mut().set_local_bookmark_target(
            RefName::new("topic"),
            RefTarget::normal(base.id().clone()),
        );
        let wc = write_commit(&mut tx, vec![base.id().clone()], "");
        pollster::block_on(tx.repo_mut().edit(WorkspaceName::DEFAULT.to_owned(), &wc)).unwrap();
        pollster::block_on(tx.repo_mut().rebase_descendants()).unwrap();
        pollster::block_on(tx.commit("set up duplicate stack")).unwrap();
        check_out_working_copy(dir.path());

        let backend = test_backend();
        let before = backend.open(dir.path()).unwrap();
        let original_id = node_by_description(&before, "feat: base work").id.clone();

        let outcome = backend.duplicate_change(dir.path(), &original_id).unwrap();
        let copy_id = outcome.target_change.clone().expect("copy id");
        assert_ne!(copy_id, original_id);
        assert_eq!(
            outcome.summary,
            format!("Duplicated {original_id} as {copy_id}")
        );

        // Both changes exist with the same description and parents; the
        // bookmark stays on the original.
        let after = backend.open(dir.path()).unwrap();
        let original = after.nodes.iter().find(|n| n.id == original_id).unwrap();
        let copy = after.nodes.iter().find(|n| n.id == copy_id).unwrap();
        assert_eq!(copy.description, original.description);
        assert_eq!(copy.parents, original.parents);
        assert!(copy.bookmarks.is_empty());
        assert!(original.bookmarks.contains(&"topic".to_owned()));

        // The op log undoes a duplicate: an operation that only adds a
        // head must not read as "already undone" (the merge records the
        // added head as an abandoned commit, not a view edit).
        let undone = backend
            .revert_operation(dir.path(), &outcome.operation_id.unwrap())
            .unwrap();
        assert!(undone.operation_id.is_some());
        let restored = backend.open(dir.path()).unwrap();
        assert!(!restored.nodes.iter().any(|n| n.id == copy_id));
        // No freshness assert: duplicating moves no working copy, so —
        // like a bookmark op — the recorded wc operation legitimately
        // stays one op behind (same tree ⇒ fresh at runtime).
    }

    #[test]
    fn revert_creates_a_back_out_change_on_the_working_copy() {
        let dir = tempfile::tempdir().unwrap();
        let settings = test_settings();
        let (_workspace, repo) =
            pollster::block_on(Workspace::init_simple(&settings, dir.path())).unwrap();
        let root_commit_id = repo.store().root_commit_id().clone();
        let store = repo.store().clone();
        let mut tx = repo.start_transaction();

        let base_tree = file_tree(&store, &[("a.txt", "one\n")]);
        let base = write_commit_with_tree(&mut tx, vec![root_commit_id], "feat: base work", base_tree);
        let target_tree = file_tree(&store, &[("a.txt", "one\ntwo\n")]);
        let target =
            write_commit_with_tree(&mut tx, vec![base.id().clone()], "feat: add two", target_tree);
        // A clean `jj new` on the target: the working copy carries the
        // target's tree, so the reverse diff applies without conflict.
        let wc_tree = file_tree(&store, &[("a.txt", "one\ntwo\n")]);
        let wc = write_commit_with_tree(&mut tx, vec![target.id().clone()], "", wc_tree);
        pollster::block_on(tx.repo_mut().edit(WorkspaceName::DEFAULT.to_owned(), &wc)).unwrap();
        pollster::block_on(tx.repo_mut().rebase_descendants()).unwrap();
        pollster::block_on(tx.commit("set up revert stack")).unwrap();
        check_out_working_copy(dir.path());

        let backend = test_backend();
        let before = backend.open(dir.path()).unwrap();
        let wc_id = before.working_copy.clone();
        let target_id = node_by_description(&before, "feat: add two").id.clone();

        let outcome = backend.revert_change(dir.path(), &target_id).unwrap();
        let new_id = outcome.target_change.clone().expect("back-out id");
        assert_ne!(new_id, target_id);
        assert_eq!(outcome.summary, format!("Reverted {target_id} as {new_id}"));

        // The back-out sits on `@` (which did not move), described like the
        // CLI's revert template, and its diff undoes exactly the target's.
        let after = backend.open(dir.path()).unwrap();
        assert_eq!(after.working_copy, wc_id);
        let back_out = after.nodes.iter().find(|n| n.id == new_id).unwrap();
        assert_eq!(back_out.parents, vec![wc_id.clone()]);
        assert!(
            back_out.description.starts_with("Revert \"feat: add two\""),
            "got {}",
            back_out.description
        );
        assert!(back_out.description.contains("This reverts commit "));
        assert!(!back_out.has_conflict);
        let detail = backend.change_detail(dir.path(), &new_id).unwrap();
        let paths: Vec<&str> = detail.files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, vec!["a.txt"]);

        // The op log undoes it: reverting the recorded operation removes
        // the back-out again.
        let undone = backend
            .revert_operation(dir.path(), &outcome.operation_id.unwrap())
            .unwrap();
        assert!(undone.operation_id.is_some());
        let restored = backend.open(dir.path()).unwrap();
        assert!(!restored.nodes.iter().any(|n| n.id == new_id));
        // No freshness assert: like duplicate, the working copy never moves,
        // so the recorded wc operation legitimately stays behind.
    }

    #[test]
    fn revert_backs_out_what_a_merge_itself_introduced() {
        let dir = tempfile::tempdir().unwrap();
        let settings = test_settings();
        let (_workspace, repo) =
            pollster::block_on(Workspace::init_simple(&settings, dir.path())).unwrap();
        let root_commit_id = repo.store().root_commit_id().clone();
        let store = repo.store().clone();
        let mut tx = repo.start_transaction();

        let base_tree = file_tree(&store, &[("a.txt", "one\n")]);
        let base = write_commit_with_tree(&mut tx, vec![root_commit_id], "feat: base work", base_tree);
        let left_tree = file_tree(&store, &[("a.txt", "one\n"), ("left.txt", "l\n")]);
        let left = write_commit_with_tree(&mut tx, vec![base.id().clone()], "feat: left", left_tree);
        let right_tree = file_tree(&store, &[("a.txt", "one\n"), ("right.txt", "r\n")]);
        let right =
            write_commit_with_tree(&mut tx, vec![base.id().clone()], "feat: right", right_tree);
        // The merge carries extra.txt beyond what its parents auto-merge
        // to, so reverting the merge must back out exactly that.
        let merge_tree = file_tree(
            &store,
            &[
                ("a.txt", "one\n"),
                ("left.txt", "l\n"),
                ("right.txt", "r\n"),
                ("extra.txt", "x\n"),
            ],
        );
        let merge = write_commit_with_tree(
            &mut tx,
            vec![left.id().clone(), right.id().clone()],
            "merge: join sides",
            merge_tree.clone(),
        );
        let wc = write_commit_with_tree(&mut tx, vec![merge.id().clone()], "", merge_tree);
        pollster::block_on(tx.repo_mut().edit(WorkspaceName::DEFAULT.to_owned(), &wc)).unwrap();
        pollster::block_on(tx.repo_mut().rebase_descendants()).unwrap();
        pollster::block_on(tx.commit("set up merge revert stack")).unwrap();
        check_out_working_copy(dir.path());

        let backend = test_backend();
        let before = backend.open(dir.path()).unwrap();
        let merge_id = node_by_description(&before, "merge: join sides").id.clone();

        let outcome = backend.revert_change(dir.path(), &merge_id).unwrap();
        let new_id = outcome.target_change.clone().expect("back-out id");

        // Only the merge's own contribution is backed out; what its
        // parents brought in stays.
        let back_out_detail = backend.change_detail(dir.path(), &new_id).unwrap();
        let paths: Vec<&str> = back_out_detail.files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, vec!["extra.txt"]);
        let after = backend.open(dir.path()).unwrap();
        let back_out = after.nodes.iter().find(|n| n.id == new_id).unwrap();
        assert!(
            back_out.description.starts_with("Revert \"merge: join sides\""),
            "got {}",
            back_out.description
        );
        assert!(!back_out.has_conflict);
    }

    #[test]
    fn absorb_sinks_edits_into_the_ancestor_that_owns_the_lines() {
        let dir = tempfile::tempdir().unwrap();
        let settings = test_settings();
        let (_workspace, repo) =
            pollster::block_on(Workspace::init_simple(&settings, dir.path())).unwrap();
        let root_commit_id = repo.store().root_commit_id().clone();
        let store = repo.store().clone();
        let mut tx = repo.start_transaction();

        // The base introduces every line, so an edit to line two annotates
        // back to it; the working copy carries that edit.
        let base_tree = file_tree(&store, &[("a.txt", "one\ntwo\nthree\n")]);
        let base = write_commit_with_tree(&mut tx, vec![root_commit_id], "feat: base work", base_tree);
        let wc_tree = file_tree(&store, &[("a.txt", "one\nTWO\nthree\n")]);
        let wc = write_commit_with_tree(&mut tx, vec![base.id().clone()], "", wc_tree);
        pollster::block_on(tx.repo_mut().edit(WorkspaceName::DEFAULT.to_owned(), &wc)).unwrap();
        pollster::block_on(tx.repo_mut().rebase_descendants()).unwrap();
        pollster::block_on(tx.commit("set up absorb stack")).unwrap();
        check_out_working_copy(dir.path());

        let backend = test_backend();
        let before = backend.open(dir.path()).unwrap();
        let wc_id = before.working_copy.clone();
        let base_id = node_by_description(&before, "feat: base work").id.clone();

        let outcome = backend.absorb_change(dir.path(), &wc_id).unwrap();
        assert!(outcome.summary.contains("Absorbed changes"));
        assert!(outcome.operation_id.is_some());

        // The edit lives in the base now; the fully-absorbed working copy
        // was discarded and respawned empty.
        let after = backend.open(dir.path()).unwrap();
        let detail = backend.change_detail(dir.path(), &base_id).unwrap();
        let paths: Vec<&str> = detail.files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, vec!["a.txt"]);
        let wc_node = after.nodes.iter().find(|n| n.id == after.working_copy).unwrap();
        assert!(wc_node.is_empty);
        assert_workspace_fresh(dir.path());
    }

    #[test]
    fn absorb_refuses_when_no_mutable_ancestor_claims_the_lines() {
        let dir = tempfile::tempdir().unwrap();
        build_test_repo(dir.path());
        write_repo_config(
            dir.path(),
            "revset-aliases.\"immutable_heads()\" = \"main\"\n",
        );
        check_out_working_copy(dir.path());

        let backend = test_backend();
        let snapshot = backend.open(dir.path()).unwrap();
        // The side experiment sits directly on the root: no mutable
        // ancestor exists to absorb into.
        let side = node_by_description(&snapshot, "wip: side experiment").id.clone();
        let err = backend.absorb_change(dir.path(), &side).unwrap_err();
        assert!(matches!(err, BackendError::MutationFailed(_)));
    }

    #[test]
    fn squash_refuses_immutable_parents_and_merges() {
        let dir = tempfile::tempdir().unwrap();
        build_test_repo(dir.path());
        let backend = test_backend();
        let snapshot = backend.open(dir.path()).unwrap();

        // "feat: first change" sits directly on trunk, which is immutable.
        let feature_id = node_by_description(&snapshot, "feat: first change").id.clone();
        let err = backend.squash_change(dir.path(), &feature_id).unwrap_err();
        assert!(matches!(err, BackendError::ImmutableChange(_)));

        // Merge commits refuse: squashing into multiple parents is ambiguous.
        let merge_dir = tempfile::tempdir().unwrap();
        let settings = test_settings();
        let (_workspace, repo) =
            pollster::block_on(Workspace::init_simple(&settings, merge_dir.path())).unwrap();
        let root_commit_id = repo.store().root_commit_id().clone();
        let mut tx = repo.start_transaction();
        let a = write_commit(&mut tx, vec![root_commit_id.clone()], "side a");
        let b = write_commit(&mut tx, vec![root_commit_id], "side b");
        let merge = write_commit(&mut tx, vec![a.id().clone(), b.id().clone()], "merge");
        pollster::block_on(tx.repo_mut().edit(WorkspaceName::DEFAULT.to_owned(), &merge)).unwrap();
        pollster::block_on(tx.repo_mut().rebase_descendants()).unwrap();
        pollster::block_on(tx.commit("set up merge")).unwrap();

        let backend = test_backend();
        let snapshot = backend.open(merge_dir.path()).unwrap();
        let merge_id = node_by_description(&snapshot, "merge").id.clone();
        let err = backend.squash_change(merge_dir.path(), &merge_id).unwrap_err();
        assert!(matches!(err, BackendError::MutationFailed(_)), "got {err:?}");

        // Nothing was recorded in either repo.
        let after = backend.open(merge_dir.path()).unwrap();
        assert_eq!(after.operations[0].description, "set up merge");
    }

    #[test]
    fn split_working_copy_carves_selected_files_off_the_bottom() {
        let dir = tempfile::tempdir().unwrap();
        let settings = test_settings();
        let (_workspace, repo) =
            pollster::block_on(Workspace::init_simple(&settings, dir.path())).unwrap();
        let root_commit_id = repo.store().root_commit_id().clone();
        let store = repo.store().clone();
        let mut tx = repo.start_transaction();

        let base_tree = file_tree(&store, &[("a.txt", "one\n")]);
        let base = write_commit_with_tree(&mut tx, vec![root_commit_id], "feat: base", base_tree);
        let mixed_tree = file_tree(
            &store,
            &[("a.txt", "two\n"), ("b.txt", "bee\n"), ("c.txt", "sea\n")],
        );
        let mixed =
            write_commit_with_tree(&mut tx, vec![base.id().clone()], "mixed work", mixed_tree);
        tx.repo_mut().set_local_bookmark_target(
            RefName::new("topic"),
            RefTarget::normal(mixed.id().clone()),
        );
        pollster::block_on(tx.repo_mut().edit(WorkspaceName::DEFAULT.to_owned(), &mixed)).unwrap();
        pollster::block_on(tx.repo_mut().rebase_descendants()).unwrap();
        pollster::block_on(tx.commit("set up split fixture")).unwrap();
        check_out_working_copy(dir.path());

        let backend = test_backend();
        let before = backend.open(dir.path()).unwrap();
        let mixed_id = before.working_copy.clone();
        let base_id = node_by_description(&before, "feat: base").id.clone();

        let outcome = backend
            .split_change(
                dir.path(),
                &mixed_id,
                &[
                    SplitSelection::whole("a.txt"),
                    SplitSelection::whole("c.txt"),
                ],
                "refactor: carve a and c",
            )
            .unwrap();
        // Selection stays on the carved half, which keeps the change id.
        assert_eq!(outcome.target_change.as_deref(), Some(mixed_id.as_str()));

        let after = backend.open(dir.path()).unwrap();
        // The carved half: same change id, the new description, only the
        // selected files, sitting on the original parent.
        let carved = after.nodes.iter().find(|n| n.id == mixed_id).unwrap();
        assert_eq!(carved.description, "refactor: carve a and c");
        assert_eq!(carved.parents, vec![base_id]);
        assert_eq!(carved.kind, NodeKind::Mutable);
        let carved_files = backend.change_detail(dir.path(), &mixed_id).unwrap();
        let paths: Vec<&str> = carved_files.files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, vec!["a.txt", "c.txt"]);

        // The remainder: a new change directly on top with the original
        // description, the unselected file, the bookmark, and the working
        // copy — jj's split rule.
        let remainder = node_by_description(&after, "mixed work");
        assert_ne!(remainder.id, mixed_id);
        assert_eq!(remainder.parents, vec![mixed_id.clone()]);
        assert_eq!(remainder.kind, NodeKind::WorkingCopy);
        assert_eq!(after.working_copy, remainder.id);
        assert!(remainder.bookmarks.contains(&"topic".to_owned()));
        let remainder_files = backend.change_detail(dir.path(), &remainder.id).unwrap();
        let paths: Vec<&str> = remainder_files.files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, vec!["b.txt"]);

        assert_eq!(
            outcome.summary,
            format!("Split {mixed_id}: kept 2 files, the rest moved to {}", remainder.id)
        );
        assert!(after.operations[0].description.starts_with("split commit "));
        // Splitting @ leaves the same files on disk (the remainder's tree is
        // the original tree), recorded fresh at the new operation.
        assert!(dir.path().join("a.txt").exists());
        assert!(dir.path().join("b.txt").exists());
        assert_workspace_fresh(dir.path());
    }

    #[test]
    fn split_mid_stack_rebases_descendants_onto_the_remainder() {
        let dir = tempfile::tempdir().unwrap();
        let settings = test_settings();
        let (_workspace, repo) =
            pollster::block_on(Workspace::init_simple(&settings, dir.path())).unwrap();
        let root_commit_id = repo.store().root_commit_id().clone();
        let store = repo.store().clone();
        let mut tx = repo.start_transaction();

        let middle_tree = file_tree(&store, &[("a.txt", "aaa\n"), ("b.txt", "bbb\n")]);
        let middle =
            write_commit_with_tree(&mut tx, vec![root_commit_id], "mixed middle", middle_tree);
        let child_tree = file_tree(
            &store,
            &[("a.txt", "aaa\n"), ("b.txt", "bbb\n"), ("d.txt", "ddd\n")],
        );
        let child =
            write_commit_with_tree(&mut tx, vec![middle.id().clone()], "child work", child_tree);
        pollster::block_on(tx.repo_mut().edit(WorkspaceName::DEFAULT.to_owned(), &child)).unwrap();
        pollster::block_on(tx.repo_mut().rebase_descendants()).unwrap();
        pollster::block_on(tx.commit("set up mid-stack split")).unwrap();
        check_out_working_copy(dir.path());

        let backend = test_backend();
        let before = backend.open(dir.path()).unwrap();
        let middle_id = node_by_description(&before, "mixed middle").id.clone();
        let child_id = before.working_copy.clone();

        let outcome = backend
            .split_change(dir.path(), &middle_id, &[SplitSelection::whole("b.txt")], "fix: only b")
            .unwrap();
        assert_eq!(
            outcome.summary,
            format!(
                "Split {middle_id}: kept 1 file, the rest moved to {}",
                node_by_description(&backend.open(dir.path()).unwrap(), "mixed middle").id
            )
        );

        let after = backend.open(dir.path()).unwrap();
        let carved = after.nodes.iter().find(|n| n.id == middle_id).unwrap();
        assert_eq!(carved.description, "fix: only b");
        let remainder = node_by_description(&after, "mixed middle");
        assert_eq!(remainder.parents, vec![middle_id.clone()]);
        // The descendant rebased onto the remainder, and the working copy
        // followed it; its content is untouched.
        let child_node = after.nodes.iter().find(|n| n.id == child_id).unwrap();
        assert_eq!(child_node.parents, vec![remainder.id.clone()]);
        assert_eq!(after.working_copy, child_id);
        let child_files = backend.change_detail(dir.path(), &child_id).unwrap();
        let paths: Vec<&str> = child_files.files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, vec!["d.txt"]);
        assert_workspace_fresh(dir.path());
    }

    #[test]
    fn split_refusals_record_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let settings = test_settings();
        let (_workspace, repo) =
            pollster::block_on(Workspace::init_simple(&settings, dir.path())).unwrap();
        let root_commit_id = repo.store().root_commit_id().clone();
        let store = repo.store().clone();
        let mut tx = repo.start_transaction();
        let tree = file_tree(&store, &[("a.txt", "aaa\n"), ("b.txt", "bbb\n")]);
        let mixed = write_commit_with_tree(&mut tx, vec![root_commit_id], "mixed", tree);
        pollster::block_on(tx.repo_mut().edit(WorkspaceName::DEFAULT.to_owned(), &mixed)).unwrap();
        pollster::block_on(tx.repo_mut().rebase_descendants()).unwrap();
        pollster::block_on(tx.commit("set up split refusals")).unwrap();
        check_out_working_copy(dir.path());

        let backend = test_backend();
        let snapshot = backend.open(dir.path()).unwrap();
        let mixed_id = snapshot.working_copy.clone();

        // A selection that changes nothing (unknown path, or no paths at
        // all) and one that covers every change are both refused — the
        // curated deviation from the CLI's warn-and-proceed.
        let err = backend
            .split_change(dir.path(), &mixed_id, &[SplitSelection::whole("zz.txt")], "x")
            .unwrap_err();
        assert!(
            err.to_string().contains("none of the selected files"),
            "got {err}"
        );
        let err = backend.split_change(dir.path(), &mixed_id, &[], "x").unwrap_err();
        assert!(
            err.to_string().contains("none of the selected files"),
            "got {err}"
        );
        let err = backend
            .split_change(
                dir.path(),
                &mixed_id,
                &[
                    SplitSelection::whole("a.txt"),
                    SplitSelection::whole("b.txt"),
                ],
                "x",
            )
            .unwrap_err();
        assert!(err.to_string().contains("covers every change"), "got {err}");

        // Immutable targets refuse like every mutation.
        let root_id = snapshot
            .nodes
            .iter()
            .find(|n| n.kind == NodeKind::Immutable)
            .unwrap()
            .id
            .clone();
        let err = backend
            .split_change(dir.path(), &root_id, &[SplitSelection::whole("a.txt")], "x")
            .unwrap_err();
        assert!(matches!(
            err,
            BackendError::ImmutableChange(_) | BackendError::MutationFailed(_)
        ));

        let after = backend.open(dir.path()).unwrap();
        assert_eq!(after.operations[0].description, "set up split refusals");
    }

    /// Build the coordinates the split panel would send for a rendered
    /// hunk — the same derivation the UI uses.
    fn hunk_coords(hunk: &DiffHunk) -> SplitHunk {
        SplitHunk {
            old_start: hunk.old_start,
            new_start: hunk.new_start,
            old_lines: hunk.lines.iter().filter(|l| l.kind != DiffLineKind::Added).count() as u32,
            new_lines: hunk.lines.iter().filter(|l| l.kind != DiffLineKind::Removed).count()
                as u32,
            lines: None,
        }
    }

    /// The same coordinates narrowed to a line pick — what the split
    /// panel's per-line checkboxes would send.
    fn hunk_lines(hunk: &DiffHunk, picks: &[u32]) -> SplitHunk {
        SplitHunk {
            lines: Some(picks.to_vec()),
            ..hunk_coords(hunk)
        }
    }

    fn text_hunks<'a>(diff: &'a ChangeDiff, path: &str) -> &'a [DiffHunk] {
        let file = diff.files.iter().find(|f| f.path == path).unwrap();
        match &file.content {
            FileDiffContent::Text { hunks, .. } => hunks,
            other => panic!("expected text content for {path}, got {other:?}"),
        }
    }

    fn line_texts(hunk: &DiffHunk, kind: DiffLineKind) -> Vec<String> {
        hunk.lines
            .iter()
            .filter(|l| l.kind == kind)
            .map(|l| l.segments.iter().map(|s| s.text.as_str()).collect())
            .collect()
    }

    #[test]
    fn split_by_hunks_carves_selected_hunks_of_a_file() {
        let dir = tempfile::tempdir().unwrap();
        let settings = test_settings();
        let (_workspace, repo) =
            pollster::block_on(Workspace::init_simple(&settings, dir.path())).unwrap();
        let root_commit_id = repo.store().root_commit_id().clone();
        let store = repo.store().clone();
        let mut tx = repo.start_transaction();

        // Two edits far enough apart for two hunks at 3 context lines,
        // plus a whole extra file and a binary file that stays behind.
        let old_src = "l1\nl2\nl3\nl4\nl5\nl6\nl7\nl8\nl9\nl10\nl11\nl12\n";
        let new_src = "l1\ntwo changed\nl3\nl4\nl5\nl6\nl7\nl8\nl9\nl10\neleven changed\nl12\n";
        let base_tree = file_tree(
            &store,
            &[("src.txt", old_src.as_bytes()), ("bin.dat", b"\x00one".as_slice())],
        );
        let base = write_commit_with_tree(&mut tx, vec![root_commit_id], "feat: base", base_tree);
        let mixed_tree = file_tree(
            &store,
            &[
                ("src.txt", new_src.as_bytes()),
                ("bin.dat", b"\x00two".as_slice()),
                ("other.txt", b"whole\n".as_slice()),
            ],
        );
        let mixed =
            write_commit_with_tree(&mut tx, vec![base.id().clone()], "mixed work", mixed_tree);
        tx.repo_mut().set_local_bookmark_target(
            RefName::new("topic"),
            RefTarget::normal(mixed.id().clone()),
        );
        pollster::block_on(tx.repo_mut().edit(WorkspaceName::DEFAULT.to_owned(), &mixed)).unwrap();
        pollster::block_on(tx.repo_mut().rebase_descendants()).unwrap();
        pollster::block_on(tx.commit("set up hunk split fixture")).unwrap();
        check_out_working_copy(dir.path());

        let backend = test_backend();
        let before = backend.open(dir.path()).unwrap();
        let mixed_id = before.working_copy.clone();
        let diff = backend.change_diff(dir.path(), &mixed_id).unwrap();
        let hunks = text_hunks(&diff, "src.txt");
        assert_eq!(hunks.len(), 2, "fixture edits should render as two hunks");

        // Hunk selection on a binary file refuses before anything runs.
        let err = backend
            .split_change(
                dir.path(),
                &mixed_id,
                &[SplitSelection {
                    path: "bin.dat".to_owned(),
                    hunks: Some(vec![hunk_coords(&hunks[0])]),
                }],
                "x",
            )
            .unwrap_err();
        assert!(err.to_string().contains("binary"), "got {err}");
        assert_eq!(
            backend.open(dir.path()).unwrap().operations[0].description,
            "set up hunk split fixture"
        );

        // Carve the whole extra file plus only the first hunk of src.txt.
        let outcome = backend
            .split_change(
                dir.path(),
                &mixed_id,
                &[
                    SplitSelection::whole("other.txt"),
                    SplitSelection {
                        path: "src.txt".to_owned(),
                        hunks: Some(vec![hunk_coords(&hunks[0])]),
                    },
                ],
                "carve: other plus the first hunk",
            )
            .unwrap();
        assert_eq!(outcome.target_change.as_deref(), Some(mixed_id.as_str()));

        let after = backend.open(dir.path()).unwrap();
        let remainder = node_by_description(&after, "mixed work");
        assert_eq!(
            outcome.summary,
            format!(
                "Split {mixed_id}: kept 1 file and parts of 1 more, the rest moved to {}",
                remainder.id
            )
        );

        // The carved half: the whole file plus src.txt with only the first
        // edit applied over the parent content.
        let carved = after.nodes.iter().find(|n| n.id == mixed_id).unwrap();
        assert_eq!(carved.description, "carve: other plus the first hunk");
        let carved_detail = backend.change_detail(dir.path(), &mixed_id).unwrap();
        let paths: Vec<&str> = carved_detail.files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, vec!["other.txt", "src.txt"]);
        let carved_diff = backend.change_diff(dir.path(), &mixed_id).unwrap();
        let carved_hunks = text_hunks(&carved_diff, "src.txt");
        assert_eq!(carved_hunks.len(), 1);
        assert_eq!(line_texts(&carved_hunks[0], DiffLineKind::Removed), vec!["l2"]);
        assert_eq!(line_texts(&carved_hunks[0], DiffLineKind::Added), vec!["two changed"]);

        // The remainder: the second edit and the binary change, on top of
        // the carved half, with the bookmark and working copy following.
        assert_eq!(remainder.parents, vec![mixed_id.clone()]);
        assert_eq!(after.working_copy, remainder.id);
        let topic = after.bookmarks.iter().find(|b| b.name == "topic").unwrap();
        assert_eq!(topic.target, remainder.id);
        let rem_detail = backend.change_detail(dir.path(), &remainder.id).unwrap();
        let rem_paths: Vec<&str> = rem_detail.files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(rem_paths, vec!["bin.dat", "src.txt"]);
        let rem_diff = backend.change_diff(dir.path(), &remainder.id).unwrap();
        let rem_hunks = text_hunks(&rem_diff, "src.txt");
        assert_eq!(rem_hunks.len(), 1);
        assert_eq!(line_texts(&rem_hunks[0], DiffLineKind::Removed), vec!["l11"]);
        assert_eq!(line_texts(&rem_hunks[0], DiffLineKind::Added), vec!["eleven changed"]);

        // Nothing moved on disk: the working copy still holds every edit.
        assert_eq!(
            std::fs::read_to_string(dir.path().join("src.txt")).unwrap(),
            new_src
        );
        assert!(after.operations[0].description.starts_with("split commit "));
        assert_workspace_fresh(dir.path());
    }

    #[test]
    fn split_single_file_change_by_hunks() {
        let dir = tempfile::tempdir().unwrap();
        let settings = test_settings();
        let (_workspace, repo) =
            pollster::block_on(Workspace::init_simple(&settings, dir.path())).unwrap();
        let root_commit_id = repo.store().root_commit_id().clone();
        let store = repo.store().clone();
        let mut tx = repo.start_transaction();

        let old_notes = "n1\nn2\nn3\nn4\nn5\nn6\nn7\nn8\nn9\nn10\nn11\nn12\n";
        let new_notes = "n1\nsecond changed\nn3\nn4\nn5\nn6\nn7\nn8\nn9\nn10\neleventh changed\nn12\n";
        let base_tree = file_tree(&store, &[("notes.txt", old_notes)]);
        let base = write_commit_with_tree(&mut tx, vec![root_commit_id], "feat: base", base_tree);
        let child_tree = file_tree(&store, &[("notes.txt", new_notes)]);
        let child =
            write_commit_with_tree(&mut tx, vec![base.id().clone()], "two edits", child_tree);
        pollster::block_on(tx.repo_mut().edit(WorkspaceName::DEFAULT.to_owned(), &child)).unwrap();
        pollster::block_on(tx.repo_mut().rebase_descendants()).unwrap();
        pollster::block_on(tx.commit("set up single-file hunk split")).unwrap();
        check_out_working_copy(dir.path());

        let backend = test_backend();
        let before = backend.open(dir.path()).unwrap();
        let child_id = before.working_copy.clone();
        let diff = backend.change_diff(dir.path(), &child_id).unwrap();
        let hunks = text_hunks(&diff, "notes.txt");
        assert_eq!(hunks.len(), 2);

        // Refusals record nothing: stale coordinates (the change moved
        // under the panel), an empty hunk list, and a selection of every
        // hunk of the only changed file (nothing left to split off).
        let err = backend
            .split_change(
                dir.path(),
                &child_id,
                &[SplitSelection {
                    path: "notes.txt".to_owned(),
                    hunks: Some(vec![SplitHunk {
                        old_start: 999,
                        new_start: 999,
                        old_lines: 1,
                        new_lines: 1,
                        lines: None,
                    }]),
                }],
                "x",
            )
            .unwrap_err();
        assert!(err.to_string().contains("no longer matches"), "got {err}");
        let err = backend
            .split_change(
                dir.path(),
                &child_id,
                &[SplitSelection {
                    path: "notes.txt".to_owned(),
                    hunks: Some(Vec::new()),
                }],
                "x",
            )
            .unwrap_err();
        assert!(err.to_string().contains("no hunks selected"), "got {err}");
        let err = backend
            .split_change(
                dir.path(),
                &child_id,
                &[SplitSelection {
                    path: "notes.txt".to_owned(),
                    hunks: Some(hunks.iter().map(hunk_coords).collect()),
                }],
                "x",
            )
            .unwrap_err();
        assert!(err.to_string().contains("covers every change"), "got {err}");
        assert_eq!(
            backend.open(dir.path()).unwrap().operations[0].description,
            "set up single-file hunk split"
        );

        // A one-file change is still splittable at hunk granularity —
        // the case file-level selection could never serve.
        let outcome = backend
            .split_change(
                dir.path(),
                &child_id,
                &[SplitSelection {
                    path: "notes.txt".to_owned(),
                    hunks: Some(vec![hunk_coords(&hunks[1])]),
                }],
                "carve: the second edit",
            )
            .unwrap();
        let after = backend.open(dir.path()).unwrap();
        let remainder = node_by_description(&after, "two edits");
        assert_eq!(
            outcome.summary,
            format!(
                "Split {child_id}: kept parts of 1 file, the rest moved to {}",
                remainder.id
            )
        );
        let carved_diff = backend.change_diff(dir.path(), &child_id).unwrap();
        let carved_hunks = text_hunks(&carved_diff, "notes.txt");
        assert_eq!(carved_hunks.len(), 1);
        assert_eq!(
            line_texts(&carved_hunks[0], DiffLineKind::Added),
            vec!["eleventh changed"]
        );
        let rem_diff = backend.change_diff(dir.path(), &remainder.id).unwrap();
        let rem_hunks = text_hunks(&rem_diff, "notes.txt");
        assert_eq!(rem_hunks.len(), 1);
        assert_eq!(
            line_texts(&rem_hunks[0], DiffLineKind::Added),
            vec!["second changed"]
        );
        assert_eq!(after.working_copy, remainder.id);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("notes.txt")).unwrap(),
            new_notes
        );
        assert_workspace_fresh(dir.path());
    }

    #[test]
    fn split_by_lines_carves_single_lines_and_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let settings = test_settings();
        let (_workspace, repo) =
            pollster::block_on(Workspace::init_simple(&settings, dir.path())).unwrap();
        let root_commit_id = repo.store().root_commit_id().clone();
        let store = repo.store().clone();
        let mut tx = repo.start_transaction();

        // Two edits close enough to share one hunk at 3 context lines —
        // the case hunk granularity can never separate.
        let old_notes = "m1\nm2\nm3\nm4\nm5\nm6\nm7\nm8\n";
        let new_notes = "m1\ntwo changed\nm3\nfour changed\nm5\nm6\nm7\nm8\n";
        let base_tree = file_tree(&store, &[("notes.txt", old_notes)]);
        let base = write_commit_with_tree(&mut tx, vec![root_commit_id], "feat: base", base_tree);
        let child_tree = file_tree(&store, &[("notes.txt", new_notes)]);
        let child =
            write_commit_with_tree(&mut tx, vec![base.id().clone()], "two close edits", child_tree);
        pollster::block_on(tx.repo_mut().edit(WorkspaceName::DEFAULT.to_owned(), &child)).unwrap();
        pollster::block_on(tx.repo_mut().rebase_descendants()).unwrap();
        pollster::block_on(tx.commit("set up line split")).unwrap();
        check_out_working_copy(dir.path());

        let backend = test_backend();
        let before = backend.open(dir.path()).unwrap();
        let child_id = before.working_copy.clone();
        let diff = backend.change_diff(dir.path(), &child_id).unwrap();
        let hunks = text_hunks(&diff, "notes.txt").to_vec();
        assert_eq!(hunks.len(), 1, "both edits should share one hunk");
        let hunk = &hunks[0];
        let index_of = |kind: DiffLineKind, text: &str| {
            hunk.lines
                .iter()
                .position(|l| {
                    l.kind == kind
                        && l.segments.iter().map(|s| s.text.as_str()).collect::<String>() == text
                })
                .unwrap() as u32
        };
        let added_two = index_of(DiffLineKind::Added, "two changed");
        let context_m1 = index_of(DiffLineKind::Context, "m1");
        let all_changed: Vec<u32> = hunk
            .lines
            .iter()
            .enumerate()
            .filter(|(_, l)| l.kind != DiffLineKind::Context)
            .map(|(i, _)| i as u32)
            .collect();

        // Refusals record nothing: a context-line pick, an out-of-range
        // pick (the change moved), an empty pick list, and picks that
        // cover every changed line of the only hunk (nothing left for
        // the remainder).
        let refusal = |picks: &[u32]| {
            backend
                .split_change(
                    dir.path(),
                    &child_id,
                    &[SplitSelection {
                        path: "notes.txt".to_owned(),
                        hunks: Some(vec![hunk_lines(hunk, picks)]),
                    }],
                    "x",
                )
                .unwrap_err()
                .to_string()
        };
        assert!(refusal(&[context_m1]).contains("context line"), "{}", refusal(&[context_m1]));
        assert!(refusal(&[99]).contains("no longer matches"), "{}", refusal(&[99]));
        assert!(refusal(&[]).contains("no lines selected"), "{}", refusal(&[]));
        assert!(
            refusal(&all_changed).contains("covers every change"),
            "{}",
            refusal(&all_changed)
        );
        assert_eq!(
            backend.open(dir.path()).unwrap().operations[0].description,
            "set up line split"
        );

        // Carve a single added line out — its removed partner and the
        // whole second edit stay behind.
        let outcome = backend
            .split_change(
                dir.path(),
                &child_id,
                &[SplitSelection {
                    path: "notes.txt".to_owned(),
                    hunks: Some(vec![hunk_lines(hunk, &[added_two])]),
                }],
                "carve: one added line",
            )
            .unwrap();
        assert_eq!(outcome.target_change.as_deref(), Some(child_id.as_str()));
        let carved_diff = backend.change_diff(dir.path(), &child_id).unwrap();
        let carved_hunks = text_hunks(&carved_diff, "notes.txt");
        assert_eq!(carved_hunks.len(), 1);
        assert_eq!(
            line_texts(&carved_hunks[0], DiffLineKind::Added),
            vec!["two changed"]
        );
        assert_eq!(line_texts(&carved_hunks[0], DiffLineKind::Removed), Vec::<String>::new());

        let after = backend.open(dir.path()).unwrap();
        let remainder = node_by_description(&after, "two close edits");
        let rem_diff = backend.change_diff(dir.path(), &remainder.id).unwrap();
        let rem_hunks = text_hunks(&rem_diff, "notes.txt");
        assert_eq!(
            line_texts(&rem_hunks[0], DiffLineKind::Removed),
            vec!["m2", "m4"]
        );
        assert_eq!(
            line_texts(&rem_hunks[0], DiffLineKind::Added),
            vec!["four changed"]
        );

        // Round trip: squashing the remainder back reunites the change —
        // the original two edits, byte-identical file on disk.
        backend
            .squash_into(
                dir.path(),
                &remainder.id,
                &[SplitSelection::whole("notes.txt")],
                &child_id,
            )
            .unwrap();
        let merged_diff = backend.change_diff(dir.path(), &child_id).unwrap();
        let merged_hunks = text_hunks(&merged_diff, "notes.txt");
        assert_eq!(merged_hunks.len(), 1);
        assert_eq!(
            line_texts(&merged_hunks[0], DiffLineKind::Removed),
            vec!["m2", "m4"]
        );
        assert_eq!(
            line_texts(&merged_hunks[0], DiffLineKind::Added),
            vec!["two changed", "four changed"]
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("notes.txt")).unwrap(),
            new_notes
        );
        assert_workspace_fresh(dir.path());
    }

    #[test]
    fn squash_into_moves_selection_down_into_an_ancestor() {
        let dir = tempfile::tempdir().unwrap();
        let settings = test_settings();
        let (_workspace, repo) =
            pollster::block_on(Workspace::init_simple(&settings, dir.path())).unwrap();
        let root_commit_id = repo.store().root_commit_id().clone();
        let store = repo.store().clone();
        let mut tx = repo.start_transaction();

        let old_notes = "n1\nn2\nn3\nn4\nn5\nn6\nn7\nn8\nn9\nn10\nn11\nn12\n";
        let new_notes = "n1\nsecond changed\nn3\nn4\nn5\nn6\nn7\nn8\nn9\nn10\neleventh changed\nn12\n";
        let base_tree = file_tree(&store, &[("notes.txt", old_notes)]);
        let base = write_commit_with_tree(&mut tx, vec![root_commit_id], "feat: base", base_tree);
        let mixed_tree = file_tree(
            &store,
            &[("notes.txt", new_notes), ("b.txt", "bee\n"), ("c.txt", "sea\n")],
        );
        let mixed =
            write_commit_with_tree(&mut tx, vec![base.id().clone()], "mixed work", mixed_tree);
        tx.repo_mut().set_local_bookmark_target(
            RefName::new("topic"),
            RefTarget::normal(mixed.id().clone()),
        );
        pollster::block_on(tx.repo_mut().edit(WorkspaceName::DEFAULT.to_owned(), &mixed)).unwrap();
        pollster::block_on(tx.repo_mut().rebase_descendants()).unwrap();
        pollster::block_on(tx.commit("set up squash-into fixture")).unwrap();
        check_out_working_copy(dir.path());

        let backend = test_backend();
        let before = backend.open(dir.path()).unwrap();
        let mixed_id = before.working_copy.clone();
        let base_id = node_by_description(&before, "feat: base").id.clone();
        let diff = backend.change_diff(dir.path(), &mixed_id).unwrap();
        let hunks = text_hunks(&diff, "notes.txt");
        assert_eq!(hunks.len(), 2);

        // Move b.txt whole and only the first notes.txt hunk from the
        // working copy down into its ancestor.
        let outcome = backend
            .squash_into(
                dir.path(),
                &mixed_id,
                &[
                    SplitSelection::whole("b.txt"),
                    SplitSelection {
                        path: "notes.txt".to_owned(),
                        hunks: Some(vec![hunk_coords(&hunks[0])]),
                    },
                ],
                &base_id,
            )
            .unwrap();
        assert_eq!(
            outcome.summary,
            format!("Moved 1 file and parts of 1 more from {mixed_id} into {base_id}")
        );
        // A partial move keeps the selection on the source, where the next
        // piece gets peeled off.
        assert_eq!(outcome.target_change.as_deref(), Some(mixed_id.as_str()));

        let after = backend.open(dir.path()).unwrap();
        assert!(after.operations[0].description.starts_with("squash commits into "));
        // Both descriptions and the bookmark stay put on a partial move.
        assert_eq!(node_by_description(&after, "feat: base").id, base_id);
        assert_eq!(node_by_description(&after, "mixed work").id, mixed_id);
        let topic = after.bookmarks.iter().find(|b| b.name == "topic").unwrap();
        assert_eq!(topic.target, mixed_id);

        // The destination gained the file and the chosen hunk's edit.
        let detail = backend.change_detail(dir.path(), &base_id).unwrap();
        let paths: Vec<&str> = detail.files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, vec!["b.txt", "notes.txt"]);
        let base_diff = backend.change_diff(dir.path(), &base_id).unwrap();
        let base_added: Vec<String> = text_hunks(&base_diff, "notes.txt")
            .iter()
            .flat_map(|h| line_texts(h, DiffLineKind::Added))
            .collect();
        assert!(base_added.contains(&"second changed".to_owned()));
        assert!(!base_added.contains(&"eleventh changed".to_owned()));

        // The source keeps the rest; the working copy stayed on it and the
        // files on disk are untouched — the moved content still sits below.
        let detail = backend.change_detail(dir.path(), &mixed_id).unwrap();
        let paths: Vec<&str> = detail.files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, vec!["c.txt", "notes.txt"]);
        let mixed_diff = backend.change_diff(dir.path(), &mixed_id).unwrap();
        let mixed_hunks = text_hunks(&mixed_diff, "notes.txt");
        assert_eq!(mixed_hunks.len(), 1);
        assert_eq!(
            line_texts(&mixed_hunks[0], DiffLineKind::Added),
            vec!["eleventh changed"]
        );
        assert_eq!(after.working_copy, mixed_id);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("notes.txt")).unwrap(),
            new_notes
        );
        assert!(dir.path().join("b.txt").exists());
        assert_workspace_fresh(dir.path());
    }

    #[test]
    fn squash_into_pulls_changes_forward_into_a_descendant() {
        let dir = tempfile::tempdir().unwrap();
        let settings = test_settings();
        let (_workspace, repo) =
            pollster::block_on(Workspace::init_simple(&settings, dir.path())).unwrap();
        let root_commit_id = repo.store().root_commit_id().clone();
        let store = repo.store().clone();
        let mut tx = repo.start_transaction();

        let base_tree = file_tree(&store, &[("f.txt", "eff\n"), ("g.txt", "gee\n")]);
        let base = write_commit_with_tree(&mut tx, vec![root_commit_id], "base pair", base_tree);
        let top_tree = file_tree(
            &store,
            &[("f.txt", "eff\n"), ("g.txt", "gee\n"), ("t.txt", "tee\n")],
        );
        let top = write_commit_with_tree(&mut tx, vec![base.id().clone()], "top work", top_tree);
        pollster::block_on(tx.repo_mut().edit(WorkspaceName::DEFAULT.to_owned(), &top)).unwrap();
        pollster::block_on(tx.repo_mut().rebase_descendants()).unwrap();
        pollster::block_on(tx.commit("set up forward move")).unwrap();
        check_out_working_copy(dir.path());

        let backend = test_backend();
        let before = backend.open(dir.path()).unwrap();
        let base_id = node_by_description(&before, "base pair").id.clone();
        let top_id = before.working_copy.clone();

        // Move f.txt up into the descendant: the destination is rebased
        // onto the rewritten source first, then takes the content.
        let outcome = backend
            .squash_into(dir.path(), &base_id, &[SplitSelection::whole("f.txt")], &top_id)
            .unwrap();
        assert_eq!(
            outcome.summary,
            format!("Moved 1 file from {base_id} into {top_id}")
        );
        assert_eq!(outcome.target_change.as_deref(), Some(base_id.as_str()));

        let after = backend.open(dir.path()).unwrap();
        let detail = backend.change_detail(dir.path(), &base_id).unwrap();
        let paths: Vec<&str> = detail.files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, vec!["g.txt"]);
        let detail = backend.change_detail(dir.path(), &top_id).unwrap();
        let paths: Vec<&str> = detail.files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, vec!["f.txt", "t.txt"]);
        assert_eq!(after.working_copy, top_id);
        assert_eq!(std::fs::read_to_string(dir.path().join("f.txt")).unwrap(), "eff\n");
        assert_workspace_fresh(dir.path());
    }

    #[test]
    fn squash_into_full_selection_abandons_the_source() {
        let dir = tempfile::tempdir().unwrap();
        let settings = test_settings();
        let (_workspace, repo) =
            pollster::block_on(Workspace::init_simple(&settings, dir.path())).unwrap();
        let root_commit_id = repo.store().root_commit_id().clone();
        let store = repo.store().clone();
        let mut tx = repo.start_transaction();

        // A sibling destination, so "bookmarks land on the parent" is
        // distinguishable from "bookmarks follow the destination".
        let base_tree = file_tree(&store, &[("x.txt", "x\n")]);
        let base = write_commit_with_tree(&mut tx, vec![root_commit_id.clone()], "feat: base", base_tree);
        let src_tree = file_tree(&store, &[("x.txt", "x\n"), ("y.txt", "y\n")]);
        let src = write_commit_with_tree(&mut tx, vec![base.id().clone()], "src: adds y", src_tree);
        tx.repo_mut().set_local_bookmark_target(
            RefName::new("topic"),
            RefTarget::normal(src.id().clone()),
        );
        let wc = write_commit_with_tree(
            &mut tx,
            vec![src.id().clone()],
            "",
            file_tree(&store, &[("x.txt", "x\n"), ("y.txt", "y\n"), ("w.txt", "w\n")]),
        );
        pollster::block_on(tx.repo_mut().edit(WorkspaceName::DEFAULT.to_owned(), &wc)).unwrap();
        let side_tree = file_tree(&store, &[("s.txt", "s\n")]);
        write_commit_with_tree(&mut tx, vec![root_commit_id], "side: dest", side_tree);
        pollster::block_on(tx.repo_mut().rebase_descendants()).unwrap();
        pollster::block_on(tx.commit("set up full squash-into")).unwrap();
        check_out_working_copy(dir.path());

        let backend = test_backend();
        let before = backend.open(dir.path()).unwrap();
        let wc_id = before.working_copy.clone();
        let base_id = node_by_description(&before, "feat: base").id.clone();
        let src_id = node_by_description(&before, "src: adds y").id.clone();
        let side_id = node_by_description(&before, "side: dest").id.clone();

        let outcome = backend
            .squash_into(dir.path(), &src_id, &[SplitSelection::whole("y.txt")], &side_id)
            .unwrap();
        assert_eq!(
            outcome.summary,
            format!(
                "Moved everything in {src_id} into {side_id}; \
                 the emptied change was abandoned"
            )
        );
        // The source is gone, so the selection follows the destination.
        assert_eq!(outcome.target_change.as_deref(), Some(side_id.as_str()));

        let after = backend.open(dir.path()).unwrap();
        // The emptied source is abandoned: its child reparents onto the
        // parent, and — unlike an explicit abandon, which deletes them —
        // its bookmark moves to the parent, like the CLI.
        assert!(!after.nodes.iter().any(|n| n.id == src_id));
        let topic = after.bookmarks.iter().find(|b| b.name == "topic").unwrap();
        assert_eq!(topic.target, base_id);
        let wc_node = after.nodes.iter().find(|n| n.id == wc_id).unwrap();
        assert_eq!(wc_node.parents, vec![base_id]);
        assert_eq!(after.working_copy, wc_id);

        // The destination took the content and the folded description,
        // destination first — the same combining as squash_change.
        let side = node_by_description(&after, "side: dest\n\nsrc: adds y");
        assert_eq!(side.id, side_id);
        let detail = backend.change_detail(dir.path(), &side_id).unwrap();
        let paths: Vec<&str> = detail.files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, vec!["s.txt", "y.txt"]);

        // The moved content left the working copy's line of history, so
        // its files updated on disk: y.txt is gone, the wc's own file stays.
        assert!(!dir.path().join("y.txt").exists());
        assert!(dir.path().join("w.txt").exists());
        assert_workspace_fresh(dir.path());
    }

    #[test]
    fn squash_into_refusals_record_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let settings = test_settings();
        let (_workspace, repo) =
            pollster::block_on(Workspace::init_simple(&settings, dir.path())).unwrap();
        let root_commit_id = repo.store().root_commit_id().clone();
        let store = repo.store().clone();
        let mut tx = repo.start_transaction();
        let base_tree = file_tree(&store, &[("a.txt", "aaa\n")]);
        let base = write_commit_with_tree(&mut tx, vec![root_commit_id], "base", base_tree);
        let tree = file_tree(&store, &[("a.txt", "aaa\n"), ("b.txt", "bbb\n")]);
        let mixed = write_commit_with_tree(&mut tx, vec![base.id().clone()], "mixed", tree);
        pollster::block_on(tx.repo_mut().edit(WorkspaceName::DEFAULT.to_owned(), &mixed)).unwrap();
        pollster::block_on(tx.repo_mut().rebase_descendants()).unwrap();
        pollster::block_on(tx.commit("set up squash-into refusals")).unwrap();
        check_out_working_copy(dir.path());

        let backend = test_backend();
        let snapshot = backend.open(dir.path()).unwrap();
        let mixed_id = snapshot.working_copy.clone();
        let base_id = node_by_description(&snapshot, "base").id.clone();
        let root_id = snapshot
            .nodes
            .iter()
            .find(|n| n.kind == NodeKind::Immutable)
            .unwrap()
            .id
            .clone();

        let err = backend
            .squash_into(dir.path(), &mixed_id, &[SplitSelection::whole("b.txt")], &mixed_id)
            .unwrap_err();
        assert!(err.to_string().contains("into itself"), "got {err}");
        let err = backend
            .squash_into(dir.path(), &mixed_id, &[SplitSelection::whole("zz.txt")], &base_id)
            .unwrap_err();
        assert!(err.to_string().contains("none of the selected files"), "got {err}");
        let err = backend
            .squash_into(dir.path(), &mixed_id, &[], &base_id)
            .unwrap_err();
        assert!(err.to_string().contains("none of the selected files"), "got {err}");
        // Both ends must be mutable: the destination is rewritten too.
        let err = backend
            .squash_into(dir.path(), &mixed_id, &[SplitSelection::whole("b.txt")], &root_id)
            .unwrap_err();
        assert!(matches!(err, BackendError::ImmutableChange(_)), "got {err:?}");
        let err = backend
            .squash_into(dir.path(), &root_id, &[SplitSelection::whole("a.txt")], &mixed_id)
            .unwrap_err();
        assert!(matches!(err, BackendError::ImmutableChange(_)), "got {err:?}");
        // Stale hunk coordinates mean the change moved under the panel.
        let err = backend
            .squash_into(
                dir.path(),
                &mixed_id,
                &[SplitSelection {
                    path: "b.txt".to_owned(),
                    hunks: Some(vec![SplitHunk {
                        old_start: 999,
                        new_start: 999,
                        old_lines: 1,
                        new_lines: 1,
                        lines: None,
                    }]),
                }],
                &base_id,
            )
            .unwrap_err();
        assert!(err.to_string().contains("no longer matches"), "got {err}");

        assert_eq!(
            backend.open(dir.path()).unwrap().operations[0].description,
            "set up squash-into refusals"
        );
    }

    #[test]
    fn rebase_change_moves_stack_onto_destination() {
        let dir = tempfile::tempdir().unwrap();
        build_test_repo(dir.path());
        let backend = test_backend();
        let before = backend.open(dir.path()).unwrap();
        let wc_id = before.working_copy.clone();
        let feature_id = node_by_description(&before, "feat: first change").id.clone();
        let side_id = node_by_description(&before, "wip: side experiment").id.clone();

        let outcome = backend.rebase_change(dir.path(), &feature_id, &side_id).unwrap();
        assert_eq!(
            outcome.summary,
            format!("Rebased {feature_id} and 1 descendant onto {side_id}")
        );
        assert_eq!(outcome.target_change.as_deref(), Some(feature_id.as_str()));

        let after = backend.open(dir.path()).unwrap();
        let feature = node_by_description(&after, "feat: first change");
        assert_eq!(feature.parents, vec![side_id.clone()]);
        // The working copy followed its parent; same change, new commit.
        let wc_node = after.nodes.iter().find(|n| n.id == wc_id).unwrap();
        assert_eq!(wc_node.parents, vec![feature_id.clone()]);
        assert_eq!(after.working_copy, wc_id);
        // The bookmark rides the rewrite: same change, so no move effect.
        let bookmark = after.bookmarks.iter().find(|b| b.name == "feature-a").unwrap();
        assert_eq!(bookmark.target, feature_id);
        let op = &after.operations[0];
        assert!(op.description.starts_with("rebase commit "));
        assert!(op.description.ends_with(" and descendants"));
        assert!(op.effects.is_empty(), "got {:?}", op.effects);
        assert_eq!(op.id, outcome.operation_id.unwrap());
        assert_workspace_fresh(dir.path());
    }

    #[test]
    fn rebase_allows_immutable_destinations_and_skips_in_place() {
        let dir = tempfile::tempdir().unwrap();
        build_test_repo(dir.path());
        let backend = test_backend();
        let before = backend.open(dir.path()).unwrap();
        let trunk_id = node_by_description(&before, "release: cut 1.0").id.clone();
        let side_id = node_by_description(&before, "wip: side experiment").id.clone();
        let feature_id = node_by_description(&before, "feat: first change").id.clone();

        // Rebasing onto trunk — an immutable destination — is the canonical
        // way to restack work; only the source must be mutable.
        let outcome = backend.rebase_change(dir.path(), &side_id, &trunk_id).unwrap();
        assert_eq!(outcome.summary, format!("Rebased {side_id} onto {trunk_id}"));
        let after = backend.open(dir.path()).unwrap();
        let side = node_by_description(&after, "wip: side experiment");
        assert_eq!(side.parents, vec![trunk_id.clone()]);

        // Already on the destination: like the CLI, nothing is recorded.
        let ops_before = after.operations.len();
        let outcome = backend.rebase_change(dir.path(), &feature_id, &trunk_id).unwrap();
        assert!(outcome.operation_id.is_none());
        assert_eq!(outcome.summary, format!("{feature_id} is already on {trunk_id}"));
        let unchanged = backend.open(dir.path()).unwrap();
        assert_eq!(unchanged.operations.len(), ops_before);
    }

    #[test]
    fn rebase_refuses_immutable_sources_self_and_cycles() {
        let dir = tempfile::tempdir().unwrap();
        build_test_repo(dir.path());
        let backend = test_backend();
        let snapshot = backend.open(dir.path()).unwrap();
        let wc_id = snapshot.working_copy.clone();
        let trunk_id = node_by_description(&snapshot, "release: cut 1.0").id.clone();
        let side_id = node_by_description(&snapshot, "wip: side experiment").id.clone();
        let feature_id = node_by_description(&snapshot, "feat: first change").id.clone();

        let err = backend.rebase_change(dir.path(), &trunk_id, &side_id).unwrap_err();
        assert!(matches!(err, BackendError::ImmutableChange(_)), "got {err:?}");
        let err = backend
            .rebase_change(dir.path(), &feature_id, &feature_id)
            .unwrap_err();
        assert!(matches!(err, BackendError::MutationFailed(_)), "got {err:?}");
        // The working copy descends from the feature change: a cycle.
        let err = backend.rebase_change(dir.path(), &feature_id, &wc_id).unwrap_err();
        assert!(matches!(err, BackendError::MutationFailed(_)), "got {err:?}");
        let err = backend
            .rebase_change(dir.path(), &feature_id, "kkkkkkkkkkkkkkkk")
            .unwrap_err();
        assert!(matches!(err, BackendError::ChangeMissing(_)), "got {err:?}");

        // Nothing was recorded.
        let after = backend.open(dir.path()).unwrap();
        assert_eq!(after.operations[0].description, "set up test stack");
    }

    #[test]
    fn move_change_extracts_single_change() {
        let dir = tempfile::tempdir().unwrap();
        build_test_repo(dir.path());
        let backend = test_backend();
        let before = backend.open(dir.path()).unwrap();
        let wc_id = before.working_copy.clone();
        let trunk_id = node_by_description(&before, "release: cut 1.0").id.clone();
        let feature_id = node_by_description(&before, "feat: first change").id.clone();
        let side_id = node_by_description(&before, "wip: side experiment").id.clone();

        // Move only the feature change onto the side stack: its descendant
        // (the working copy) stays behind, reparented onto trunk.
        let outcome = backend.move_change(dir.path(), &feature_id, &side_id).unwrap();
        assert_eq!(outcome.summary, format!("Moved {feature_id} onto {side_id}"));
        assert_eq!(outcome.target_change.as_deref(), Some(feature_id.as_str()));

        let after = backend.open(dir.path()).unwrap();
        let feature = node_by_description(&after, "feat: first change");
        assert_eq!(feature.parents, vec![side_id]);
        let wc_node = after.nodes.iter().find(|n| n.id == wc_id).unwrap();
        assert_eq!(wc_node.parents, vec![trunk_id]);
        let bookmark = after.bookmarks.iter().find(|b| b.name == "feature-a").unwrap();
        assert_eq!(bookmark.target, feature_id);
        let op = &after.operations[0];
        assert!(op.description.starts_with("rebase commit "));
        assert!(!op.description.contains("descendants"));
        assert_workspace_fresh(dir.path());
    }

    #[test]
    fn move_change_onto_descendant_swaps_order() {
        let dir = tempfile::tempdir().unwrap();
        build_test_repo(dir.path());
        let backend = test_backend();
        let before = backend.open(dir.path()).unwrap();
        let wc_id = before.working_copy.clone();
        let trunk_id = node_by_description(&before, "release: cut 1.0").id.clone();
        let feature_id = node_by_description(&before, "feat: first change").id.clone();

        // trunk ── feature ── wc becomes trunk ── wc ── feature: moving a
        // change onto its own descendant is how adjacent changes swap.
        let outcome = backend.move_change(dir.path(), &feature_id, &wc_id).unwrap();
        assert_eq!(outcome.summary, format!("Moved {feature_id} onto {wc_id}"));

        let after = backend.open(dir.path()).unwrap();
        let feature = node_by_description(&after, "feat: first change");
        assert_eq!(feature.parents, vec![wc_id.clone()]);
        let wc_node = after.nodes.iter().find(|n| n.id == wc_id).unwrap();
        assert_eq!(wc_node.parents, vec![trunk_id]);
        assert_eq!(after.working_copy, wc_id);
        assert_workspace_fresh(dir.path());
    }

    #[test]
    fn move_change_already_in_place_still_extracts_descendants() {
        let dir = tempfile::tempdir().unwrap();
        build_test_repo(dir.path());
        let backend = test_backend();
        let before = backend.open(dir.path()).unwrap();
        let wc_id = before.working_copy.clone();
        let trunk_id = node_by_description(&before, "release: cut 1.0").id.clone();
        let feature_id = node_by_description(&before, "feat: first change").id.clone();

        // The feature change already sits on trunk, but a lone move still
        // reparents its descendants off it — exactly the CLI's behavior.
        let outcome = backend.move_change(dir.path(), &feature_id, &trunk_id).unwrap();
        assert!(outcome.operation_id.is_some());
        assert_eq!(
            outcome.summary,
            format!("Moved 1 descendant of {feature_id} onto {trunk_id}")
        );
        let after = backend.open(dir.path()).unwrap();
        let wc_node = after.nodes.iter().find(|n| n.id == wc_id).unwrap();
        assert_eq!(wc_node.parents, vec![trunk_id.clone()]);

        // With nothing left to extract, the same move records nothing.
        let outcome = backend.move_change(dir.path(), &feature_id, &trunk_id).unwrap();
        assert!(outcome.operation_id.is_none());
        assert_workspace_fresh(dir.path());
    }

    #[test]
    fn mutations_snapshot_the_dirty_working_copy_first() {
        let dir = tempfile::tempdir().unwrap();
        let settings = test_settings();
        let (_workspace, _repo) =
            pollster::block_on(Workspace::init_simple(&settings, dir.path())).unwrap();
        std::fs::write(dir.path().join(".gitignore"), "ignored.txt\n").unwrap();
        std::fs::write(dir.path().join("notes.txt"), "remember this\n").unwrap();
        std::fs::write(dir.path().join("ignored.txt"), "scratch\n").unwrap();

        let backend = test_backend();
        let before = backend.open(dir.path()).unwrap();
        let old_wc = before.working_copy.clone();

        backend.new_change(dir.path(), &old_wc).unwrap();

        let after = backend.open(dir.path()).unwrap();
        // The on-disk edits were snapshotted into the old working copy as a
        // separate operation before the new change was created on top — they
        // belong to the change the user was on, not the new empty one.
        assert_ne!(after.working_copy, old_wc);
        let old_node = after.nodes.iter().find(|n| n.id == old_wc).expect("old wc kept");
        assert!(!old_node.is_empty);
        let detail = backend.change_detail(dir.path(), &old_wc).unwrap();
        let paths: Vec<&str> = detail.files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, vec![".gitignore", "notes.txt"], "ignored file stays untracked");
        let new_detail = backend.change_detail(dir.path(), &after.working_copy).unwrap();
        assert!(new_detail.files.is_empty());

        assert_eq!(after.operations[0].description, "new empty commit");
        assert_eq!(after.operations[1].description, "snapshot working copy");
        assert!(after.operations[1].is_snapshot);
        assert_workspace_fresh(dir.path());
    }

    #[test]
    fn create_bookmark_on_mutable_and_immutable_changes() {
        let dir = tempfile::tempdir().unwrap();
        build_test_repo(dir.path());
        let backend = test_backend();
        let before = backend.open(dir.path()).unwrap();
        let wc_id = before.working_copy.clone();
        let trunk_id = node_by_description(&before, "release: cut 1.0").id.clone();

        let outcome = backend.create_bookmark(dir.path(), "topic", &wc_id).unwrap();
        assert!(outcome.operation_id.is_some());
        assert_eq!(outcome.summary, format!("Created topic on {wc_id}"));
        assert_eq!(outcome.target_change.as_deref(), Some(wc_id.as_str()));

        let after = backend.open(dir.path()).unwrap();
        let topic = after.bookmarks.iter().find(|b| b.name == "topic").unwrap();
        assert_eq!(topic.target, wc_id);
        assert_eq!(topic.sync, SyncState::LocalOnly);
        assert!(!topic.is_trunk);
        let wc_node = after.nodes.iter().find(|n| n.id == wc_id).unwrap();
        assert!(wc_node.bookmarks.contains(&"topic".to_owned()));
        let op = &after.operations[0];
        assert!(op.description.starts_with("create bookmark topic pointing to commit "));
        let labels: Vec<&str> = op.effects.iter().map(|e| e.label.as_str()).collect();
        assert_eq!(labels, ["topic created"]);

        // Bookmarking an immutable change is allowed — nothing is rewritten.
        backend.create_bookmark(dir.path(), "release-1.0", &trunk_id).unwrap();
        let after = backend.open(dir.path()).unwrap();
        let release = after.bookmarks.iter().find(|b| b.name == "release-1.0").unwrap();
        assert_eq!(release.target, trunk_id);

        // Refusals: taken names, unusable names, unknown targets.
        let err = backend.create_bookmark(dir.path(), "topic", &wc_id).unwrap_err();
        assert!(matches!(err, BackendError::MutationFailed(_)), "got {err:?}");
        for bad in ["", "my topic", "feature@origin", "ref:name"] {
            let err = backend.create_bookmark(dir.path(), bad, &wc_id).unwrap_err();
            assert!(matches!(err, BackendError::MutationFailed(_)), "{bad:?} got {err:?}");
        }
        let err = backend
            .create_bookmark(dir.path(), "ghost", "kkkkkkkkkkkkkkkk")
            .unwrap_err();
        assert!(matches!(err, BackendError::ChangeMissing(_)));
    }

    #[test]
    fn move_bookmark_reports_direction_and_noop() {
        let dir = tempfile::tempdir().unwrap();
        build_test_repo(dir.path());
        let backend = test_backend();
        let before = backend.open(dir.path()).unwrap();
        let wc_id = before.working_copy.clone();
        let trunk_id = node_by_description(&before, "release: cut 1.0").id.clone();
        let side_id = node_by_description(&before, "wip: side experiment").id.clone();

        // feature-a starts on the feature change; the working copy is its
        // child, so this is a plain forward move.
        let outcome = backend.move_bookmark(dir.path(), "feature-a", &wc_id).unwrap();
        assert_eq!(outcome.summary, format!("Moved feature-a to {wc_id}"));
        let after = backend.open(dir.path()).unwrap();
        let bookmark = after.bookmarks.iter().find(|b| b.name == "feature-a").unwrap();
        assert_eq!(bookmark.target, wc_id);
        assert!(after.operations[0]
            .description
            .starts_with("point bookmark feature-a to commit "));
        let labels: Vec<&str> = after.operations[0]
            .effects
            .iter()
            .map(|e| e.label.as_str())
            .collect();
        assert_eq!(labels, ["feature-a moved"]);

        // Backwards (to an ancestor) and sideways (to an unrelated stack)
        // are allowed; the summary names the direction.
        let outcome = backend.move_bookmark(dir.path(), "feature-a", &trunk_id).unwrap();
        assert_eq!(outcome.summary, format!("Moved feature-a backwards to {trunk_id}"));
        let outcome = backend.move_bookmark(dir.path(), "feature-a", &side_id).unwrap();
        assert_eq!(outcome.summary, format!("Moved feature-a sideways to {side_id}"));

        // Moving to where it already points records nothing.
        let outcome = backend.move_bookmark(dir.path(), "feature-a", &side_id).unwrap();
        assert!(outcome.operation_id.is_none());

        let err = backend.move_bookmark(dir.path(), "missing", &wc_id).unwrap_err();
        assert!(matches!(err, BackendError::BookmarkMissing(_)), "got {err:?}");
    }

    #[test]
    fn rename_bookmark_keeps_target() {
        let dir = tempfile::tempdir().unwrap();
        build_test_repo(dir.path());
        let backend = test_backend();
        let before = backend.open(dir.path()).unwrap();
        let feature_id = node_by_description(&before, "feat: first change").id.clone();

        let outcome = backend
            .rename_bookmark(dir.path(), "feature-a", "feature-b")
            .unwrap();
        assert_eq!(outcome.summary, "Renamed feature-a to feature-b");
        assert_eq!(outcome.target_change.as_deref(), Some(feature_id.as_str()));

        let after = backend.open(dir.path()).unwrap();
        assert!(!after.bookmarks.iter().any(|b| b.name == "feature-a"));
        let renamed = after.bookmarks.iter().find(|b| b.name == "feature-b").unwrap();
        assert_eq!(renamed.target, feature_id);
        assert_eq!(
            after.operations[0].description,
            "rename bookmark feature-a to feature-b"
        );
        // At change granularity a rename is new-name-created, old-name-deleted.
        let labels: Vec<&str> = after.operations[0]
            .effects
            .iter()
            .map(|e| e.label.as_str())
            .collect();
        assert_eq!(labels, ["feature-b created", "feature-a deleted"]);

        // Renaming over an existing name refuses; same name records nothing.
        let err = backend
            .rename_bookmark(dir.path(), "feature-b", "main")
            .unwrap_err();
        assert!(matches!(err, BackendError::MutationFailed(_)), "got {err:?}");
        let outcome = backend
            .rename_bookmark(dir.path(), "feature-b", "feature-b")
            .unwrap();
        assert!(outcome.operation_id.is_none());
        let err = backend
            .rename_bookmark(dir.path(), "feature-a", "feature-c")
            .unwrap_err();
        assert!(matches!(err, BackendError::BookmarkMissing(_)));
    }

    #[test]
    fn delete_bookmark_removes_local() {
        let dir = tempfile::tempdir().unwrap();
        build_test_repo(dir.path());
        let backend = test_backend();
        let before = backend.open(dir.path()).unwrap();
        let feature_id = node_by_description(&before, "feat: first change").id.clone();

        let outcome = backend.delete_bookmark(dir.path(), "feature-a").unwrap();
        assert_eq!(outcome.summary, "Deleted feature-a");
        assert_eq!(outcome.target_change.as_deref(), Some(feature_id.as_str()));

        let after = backend.open(dir.path()).unwrap();
        assert!(!after.bookmarks.iter().any(|b| b.name == "feature-a"));
        let feature = after.nodes.iter().find(|n| n.id == feature_id).unwrap();
        assert!(!feature.bookmarks.contains(&"feature-a".to_owned()));
        assert_eq!(after.operations[0].description, "delete bookmark feature-a");
        let labels: Vec<&str> = after.operations[0]
            .effects
            .iter()
            .map(|e| e.label.as_str())
            .collect();
        assert_eq!(labels, ["feature-a deleted"]);

        let err = backend.delete_bookmark(dir.path(), "feature-a").unwrap_err();
        assert!(matches!(err, BackendError::BookmarkMissing(_)));
    }

    #[test]
    fn revert_backs_out_one_operation_and_reverting_the_revert_redoes() {
        let dir = tempfile::tempdir().unwrap();
        build_test_repo(dir.path());
        let backend = test_backend();
        let before = backend.open(dir.path()).unwrap();
        let wc_id = before.working_copy.clone();
        let feature_id = node_by_description(&before, "feat: first change").id.clone();

        let described = backend.describe(dir.path(), &wc_id, "wip: described").unwrap();
        let described_op = described.operation_id.unwrap();
        // A later operation on top proves revert keeps what came after.
        backend.create_bookmark(dir.path(), "topic", &feature_id).unwrap();

        let reverted = backend.revert_operation(dir.path(), &described_op).unwrap();
        // The summary quotes the reverted operation's description verbatim —
        // the same text its timeline row shows (full commit hex, like the
        // CLI's op log).
        assert!(
            reverted.summary.starts_with("Reverted \u{201c}describe commit "),
            "got {}",
            reverted.summary
        );
        assert_eq!(reverted.target_change.as_deref(), Some(wc_id.as_str()));

        let after = backend.open(dir.path()).unwrap();
        let wc_node = after.nodes.iter().find(|n| n.id == wc_id).unwrap();
        assert_eq!(wc_node.description, "", "describe backed out");
        assert!(
            after.bookmarks.iter().any(|b| b.name == "topic"),
            "the later operation stays"
        );
        assert!(after.operations[0].description.starts_with("revert operation "));
        assert_eq!(after.operations[0].id, reverted.operation_id.clone().unwrap());

        // Reverting it again: the inverse is already in, nothing to record.
        let again = backend.revert_operation(dir.path(), &described_op).unwrap();
        assert!(again.operation_id.is_none());
        assert!(again.summary.ends_with("is already undone"), "got {}", again.summary);

        // Reverting the revert is redo.
        let redone = backend
            .revert_operation(dir.path(), &reverted.operation_id.unwrap())
            .unwrap();
        assert!(redone.operation_id.is_some());
        let after = backend.open(dir.path()).unwrap();
        assert_eq!(
            after.nodes.iter().find(|n| n.id == wc_id).unwrap().description,
            "wip: described"
        );

        assert_workspace_fresh(dir.path());
    }

    #[test]
    fn restore_rewinds_every_later_operation_and_checks_out_the_working_copy() {
        let dir = tempfile::tempdir().unwrap();
        build_test_repo(dir.path());
        let backend = test_backend();
        let before = backend.open(dir.path()).unwrap();
        let baseline_op = before.operations[0].id.clone();
        assert_eq!(before.operations[0].description, "set up test stack");
        let wc_id = before.working_copy.clone();

        // Three later operations: a working-copy snapshot of an on-disk
        // edit, a describe, and a bookmark.
        std::fs::write(dir.path().join("notes.txt"), "draft\n").unwrap();
        backend.describe(dir.path(), &wc_id, "wip: described").unwrap();
        backend.create_bookmark(dir.path(), "topic", &wc_id).unwrap();

        let restored = backend.restore_operation(dir.path(), &baseline_op).unwrap();
        assert_eq!(restored.summary, "Restored to \u{201c}set up test stack\u{201d}");
        assert_eq!(restored.target_change.as_deref(), Some(wc_id.as_str()));

        let after = backend.open(dir.path()).unwrap();
        let wc_node = after.nodes.iter().find(|n| n.id == wc_id).unwrap();
        assert_eq!(wc_node.description, "");
        assert!(wc_node.is_empty, "the snapshotted edit unwound too");
        assert!(!after.bookmarks.iter().any(|b| b.name == "topic"));
        assert!(after.operations[0].description.starts_with("restore to operation "));
        // The restored working copy is checked out: the tracked file left
        // the tree, so it leaves the disk.
        assert!(!dir.path().join("notes.txt").exists());
        assert_workspace_fresh(dir.path());

        // Restoring to where the repo already stands records nothing.
        let current_op = after.operations[0].id.clone();
        let noop = backend.restore_operation(dir.path(), &current_op).unwrap();
        assert!(noop.operation_id.is_none());
        assert_eq!(noop.summary, "The repo is already in this state");

        let err = backend
            .restore_operation(dir.path(), "feedfacefeed")
            .unwrap_err();
        assert!(matches!(err, BackendError::OperationMissing(_)), "got {err:?}");
    }

    #[test]
    fn revert_refuses_root_and_merge_operations() {
        let dir = tempfile::tempdir().unwrap();
        build_test_repo(dir.path());
        let backend = test_backend();
        let snapshot = backend.open(dir.path()).unwrap();

        let root_op = snapshot
            .operations
            .iter()
            .find(|op| op.id.chars().all(|c| c == '0'))
            .expect("root operation within the op cap");
        let err = backend.revert_operation(dir.path(), &root_op.id).unwrap_err();
        assert!(
            err.to_string().contains("root operation"),
            "got {err:?}"
        );

        // Two transactions committed off the same operation leave two op
        // heads; the next load merges them into a multi-parent operation,
        // which has no single inverse.
        let settings = test_settings();
        let workspace = Workspace::load(
            &settings,
            dir.path(),
            &default_backend_factories(),
            &default_working_copy_factories(),
        )
        .unwrap();
        let repo = pollster::block_on(workspace.repo_loader().load_at_head()).unwrap();
        let root_commit_id = repo.store().root_commit_id().clone();
        let mut tx1 = repo.start_transaction();
        write_commit(&mut tx1, vec![root_commit_id.clone()], "concurrent a");
        pollster::block_on(tx1.commit("concurrent op a")).unwrap();
        let mut tx2 = repo.start_transaction();
        write_commit(&mut tx2, vec![root_commit_id], "concurrent b");
        pollster::block_on(tx2.commit("concurrent op b")).unwrap();

        let merged = backend.open(dir.path()).unwrap();
        let head_op = &merged.operations[0];
        let err = backend.revert_operation(dir.path(), &head_op.id).unwrap_err();
        assert!(
            err.to_string().contains("merges concurrent operations"),
            "got {err:?}"
        );
    }

    /// Rewrites the side commit in two concurrent operations — the classic
    /// way divergence happens (two clients rewriting the same change) — and
    /// returns the shared change id the copies display.
    fn diverge_side_commit(root: &Path) -> String {
        let settings = test_settings();
        let workspace = Workspace::load(
            &settings,
            root,
            &default_backend_factories(),
            &default_working_copy_factories(),
        )
        .unwrap();
        let repo = pollster::block_on(workspace.repo_loader().load_at_head()).unwrap();
        let snapshot = test_backend().open(root).unwrap();
        let side_id = node_by_description(&snapshot, "wip: side experiment").id.clone();
        let side = resolve_change_commit(&repo, &side_id).unwrap();

        for (text, op) in [
            ("wip: side experiment (tuned)", "describe side one way"),
            ("wip: side experiment (rewritten)", "describe side the other way"),
        ] {
            let mut tx = repo.start_transaction();
            pollster::block_on(
                tx.repo_mut().rewrite_commit(&side).set_description(text).write(),
            )
            .unwrap();
            pollster::block_on(tx.repo_mut().rebase_descendants()).unwrap();
            pollster::block_on(tx.commit(op)).unwrap();
        }
        side_id
    }

    #[test]
    fn divergent_change_surfaces_and_is_addressed_by_commit_id() {
        let dir = tempfile::tempdir().unwrap();
        build_test_repo(dir.path());
        let backend = test_backend();
        let change_id = diverge_side_commit(dir.path());

        // Both visible copies render, sharing the change id but each keyed
        // by its own forward-hex commit id, so every node id stays unique.
        let snapshot = backend.open(dir.path()).unwrap();
        let copies: Vec<&GraphNode> = snapshot.nodes.iter().filter(|n| n.is_divergent).collect();
        assert_eq!(copies.len(), 2, "both copies drawn");
        let (a, b) = (copies[0], copies[1]);
        assert_eq!(a.change_id, change_id);
        assert_eq!(b.change_id, change_id);
        assert_ne!(a.id, b.id);
        for copy in &copies {
            assert_eq!(copy.id, copy.commit_id, "divergent nodes key by commit id");
            assert!(copy.id.chars().all(|c| c.is_ascii_hexdigit()));
            // Both copies are the user's own: this divergence is theirs
            // to resolve, unlike a copy someone else's branch keeps.
            assert!(!copy.old_copies_held && copy.diverged_here());
        }
        // Everything else keeps the change-id key.
        let wc = snapshot
            .nodes
            .iter()
            .find(|n| n.id == snapshot.working_copy)
            .unwrap();
        assert!(!wc.is_divergent);
        assert_eq!(wc.id, wc.change_id);

        // Each copy is separately inspectable through its commit id.
        let diff = backend.change_diff(dir.path(), &a.id).unwrap();
        assert_eq!(diff.id, a.id);

        // Mutating by change id is ambiguous and refused; an explicit
        // commit id picks one side, and the outcome follows the rewritten
        // copy (its node id changed with the rewrite).
        let err = backend.describe(dir.path(), &change_id, "pick me").unwrap_err();
        assert!(err.to_string().contains("divergent"), "got {err:?}");
        let outcome = backend
            .describe(dir.path(), &a.id, "wip: side experiment (kept)")
            .unwrap();
        let after = backend.open(dir.path()).unwrap();
        let target = outcome.target_change.unwrap();
        let kept = after
            .nodes
            .iter()
            .find(|n| n.id == target)
            .expect("selection follows the rewritten copy");
        assert!(kept.is_divergent, "still two visible copies");
        assert_eq!(kept.description, "wip: side experiment (kept)");

        // The rewritten-away commit id refuses mutations (a stale click
        // must not resurrect a hidden commit and widen the divergence),
        // while the read path still serves it, like `jj show`.
        let err = backend.describe(dir.path(), &a.id, "too late").unwrap_err();
        assert!(err.to_string().contains("rewritten"), "got {err:?}");
        assert!(backend.change_diff(dir.path(), &a.id).is_ok());

        // Abandoning the copy not wanted resolves the divergence: one
        // visible commit again, keyed by its change id.
        backend.abandon_change(dir.path(), &b.id).unwrap();
        let resolved = backend.open(dir.path()).unwrap();
        assert!(!resolved.nodes.iter().any(|n| n.is_divergent));
        let survivor = node_by_description(&resolved, "wip: side experiment (kept)");
        assert_eq!(survivor.id, change_id);
        assert_eq!(survivor.change_id, change_id);
    }

    /// Conflicts carry their file paths: the conflicted change lists the
    /// tree's unresolved entries, and a child that never touched the file
    /// inherits the same path even though its own diff has nothing to show.
    #[test]
    fn conflicted_changes_list_their_paths() {
        let dir = tempfile::tempdir().unwrap();
        let settings = test_settings();
        let (_workspace, repo) =
            pollster::block_on(Workspace::init_simple(&settings, dir.path())).unwrap();
        let store = repo.store().clone();
        let root_commit_id = store.root_commit_id().clone();
        let mut tx = repo.start_transaction();
        let base = write_commit_with_tree(
            &mut tx,
            vec![root_commit_id],
            "base: shared notes",
            file_tree(&store, &[("notes.txt", "base\n")]),
        );
        tx.repo_mut()
            .set_local_bookmark_target(RefName::new("main"), RefTarget::normal(base.id().clone()));
        let _ours = write_commit_with_tree(
            &mut tx,
            vec![base.id().clone()],
            "feat: ours",
            file_tree(&store, &[("notes.txt", "ours\n")]),
        );
        let theirs = write_commit_with_tree(
            &mut tx,
            vec![base.id().clone()],
            "feat: theirs",
            file_tree(&store, &[("notes.txt", "theirs\n")]),
        );
        write_commit_with_tree(
            &mut tx,
            vec![theirs.id().clone()],
            "docs: readme only",
            file_tree(&store, &[("notes.txt", "theirs\n"), ("README.md", "hi\n")]),
        );
        pollster::block_on(tx.commit("set up conflicting siblings")).unwrap();

        let backend = test_backend();
        let before = backend.open(dir.path()).unwrap();
        assert!(before.conflicts.is_empty());
        let ours_id = node_by_description(&before, "feat: ours").id.clone();
        let theirs_id = node_by_description(&before, "feat: theirs").id.clone();
        let child_id = node_by_description(&before, "docs: readme only").id.clone();

        // Both sides rewrote notes.txt, so rebasing one onto the other
        // records the conflict first-class instead of blocking.
        backend.rebase_change(dir.path(), &theirs_id, &ours_id).unwrap();

        let after = backend.open(dir.path()).unwrap();
        let by_node = |id: &str| {
            after
                .conflicts
                .iter()
                .find(|c| c.node_id.as_deref() == Some(id))
                .unwrap_or_else(|| panic!("conflict item for {id}"))
        };
        let theirs_item = by_node(&theirs_id);
        assert_eq!(theirs_item.kind, ConflictKind::File);
        assert_eq!(theirs_item.paths, vec!["notes.txt"]);
        assert_eq!(theirs_item.more_paths, 0);
        assert!(theirs_item.summary.contains("feat: theirs"), "got {}", theirs_item.summary);

        // The child never touched notes.txt — its parent-relative diff has
        // no entry for it — but its tree carries the unresolved conflict,
        // so the diff appends the file as a conflict-section entry.
        let child_item = by_node(&child_id);
        assert_eq!(child_item.paths, vec!["notes.txt"]);
        let child_diff = backend.change_diff(dir.path(), &child_id).unwrap();
        let appended = child_diff.files.iter().find(|f| f.path == "notes.txt").unwrap();
        assert!(appended.has_conflict);
        assert!(matches!(appended.content, FileDiffContent::Conflict { .. }));
        // The file list agrees with the diff, and the file the child
        // actually changes keeps its ordinary text diff.
        let child_detail = backend.change_detail(dir.path(), &child_id).unwrap();
        let listed = child_detail.files.iter().find(|f| f.path == "notes.txt").unwrap();
        assert!(listed.has_conflict);
        assert_eq!(listed.status, FileStatus::Modified);
        let readme = child_diff.files.iter().find(|f| f.path == "README.md").unwrap();
        assert!(matches!(readme.content, FileDiffContent::Text { .. }));
        for id in [&theirs_id, &child_id] {
            assert!(after.nodes.iter().find(|n| &n.id == id).unwrap().has_conflict);
        }
        assert!(!after.nodes.iter().find(|n| n.id == ours_id).unwrap().has_conflict);
    }

    /// The diff pane's conflict rendering: a conflicted file arrives as
    /// structural sections, each region showing every side as its diff
    /// from the base — what the take verbs call ours (side #1) and theirs
    /// (side #2).
    #[test]
    fn change_diff_renders_conflict_sections_for_both_sides() {
        let dir = tempfile::tempdir().unwrap();
        let backend = test_backend();
        let (_ours_id, theirs_id, _child_id) = build_conflicted_repo(dir.path(), &backend);

        let diff = backend.change_diff(dir.path(), &theirs_id).unwrap();
        let file = diff.files.iter().find(|f| f.path == "notes.txt").unwrap();
        assert!(file.has_conflict);
        let FileDiffContent::Conflict { sections, truncated } = &file.content else {
            panic!("expected conflict content, got {:?}", file.content);
        };
        assert!(!truncated);
        assert_eq!(sections.len(), 1, "one region, no resolved runs: {sections:?}");
        let ConflictSection::Region { sides } = &sections[0] else {
            panic!("expected a region, got {:?}", sections[0]);
        };
        assert_eq!(sides.len(), 2);
        let texts = |side: &ConflictSideDiff| -> Vec<(DiffLineKind, String)> {
            side.lines
                .iter()
                .map(|l| (l.kind, l.segments.iter().map(|s| s.text.as_str()).collect()))
                .collect()
        };
        // Side #1 is the rebase destination ("ours"), side #2 the rebased
        // revision ("theirs") — both read against the one base.
        assert_eq!(
            texts(&sides[0]),
            vec![
                (DiffLineKind::Removed, "base".to_owned()),
                (DiffLineKind::Added, "ours".to_owned())
            ]
        );
        assert_eq!(
            texts(&sides[1]),
            vec![
                (DiffLineKind::Removed, "base".to_owned()),
                (DiffLineKind::Added, "theirs".to_owned())
            ]
        );
    }

    /// Long resolved runs around a conflict region trim to diff context
    /// with elision bands, so a big file with one conflict stays readable.
    #[test]
    fn conflict_sections_trim_resolved_context() {
        let dir = tempfile::tempdir().unwrap();
        let settings = test_settings();
        let (_workspace, repo) =
            pollster::block_on(Workspace::init_simple(&settings, dir.path())).unwrap();
        let store = repo.store().clone();
        let root_commit_id = store.root_commit_id().clone();
        let mut tx = repo.start_transaction();
        // Twelve lines with one contested line in the middle: five resolved
        // above the region, six below.
        let numbered = |mid: &str| -> String {
            let mut s = String::new();
            for i in 1..=5 {
                s.push_str(&format!("l{i:02}\n"));
            }
            s.push_str(mid);
            s.push('\n');
            for i in 7..=12 {
                s.push_str(&format!("l{i:02}\n"));
            }
            s
        };
        let base = write_commit_with_tree(
            &mut tx,
            vec![root_commit_id],
            "base: long notes",
            file_tree(&store, &[("notes.txt", &numbered("mid"))]),
        );
        tx.repo_mut()
            .set_local_bookmark_target(RefName::new("main"), RefTarget::normal(base.id().clone()));
        let _ours = write_commit_with_tree(
            &mut tx,
            vec![base.id().clone()],
            "feat: ours",
            file_tree(&store, &[("notes.txt", &numbered("ours-mid"))]),
        );
        let theirs = write_commit_with_tree(
            &mut tx,
            vec![base.id().clone()],
            "feat: theirs",
            file_tree(&store, &[("notes.txt", &numbered("theirs-mid"))]),
        );
        let _ = theirs;
        pollster::block_on(tx.commit("set up conflicting siblings")).unwrap();
        let backend = test_backend();
        let before = backend.open(dir.path()).unwrap();
        let ours_id = node_by_description(&before, "feat: ours").id.clone();
        let theirs_id = node_by_description(&before, "feat: theirs").id.clone();
        backend.rebase_change(dir.path(), &theirs_id, &ours_id).unwrap();

        let diff = backend.change_diff(dir.path(), &theirs_id).unwrap();
        let file = diff.files.iter().find(|f| f.path == "notes.txt").unwrap();
        let FileDiffContent::Conflict { sections, .. } = &file.content else {
            panic!("expected conflict content, got {:?}", file.content);
        };
        let shape: Vec<String> = sections
            .iter()
            .map(|s| match s {
                ConflictSection::Resolved { lines } => format!("resolved:{}", lines.len()),
                ConflictSection::Elided { lines } => format!("elided:{lines}"),
                ConflictSection::Region { sides } => format!("region:{}", sides.len()),
            })
            .collect();
        assert_eq!(
            shape,
            vec!["elided:2", "resolved:3", "region:2", "resolved:3", "elided:3"],
        );
        // The kept context is the run's region-facing flank.
        let ConflictSection::Resolved { lines } = &sections[1] else { unreachable!() };
        let texts: Vec<String> = lines
            .iter()
            .map(|l| l.segments.iter().map(|s| s.text.as_str()).collect())
            .collect();
        assert_eq!(texts, vec!["l03", "l04", "l05"]);
    }

    /// Conflicts the take verbs refuse (more than two sides) still render:
    /// the view is read-only and shows every side.
    #[test]
    fn conflict_sections_render_every_side_of_many_sided_conflicts() {
        let dir = tempfile::tempdir().unwrap();
        let backend = test_backend();
        let settings = test_settings();
        let (_workspace, repo) =
            pollster::block_on(Workspace::init_simple(&settings, dir.path())).unwrap();
        let store = repo.store().clone();
        let root_commit_id = store.root_commit_id().clone();
        let mut tx = repo.start_transaction();
        let base = write_commit_with_tree(
            &mut tx,
            vec![root_commit_id],
            "base: shared notes",
            file_tree(&store, &[("notes.txt", "base\n")]),
        );
        tx.repo_mut()
            .set_local_bookmark_target(RefName::new("main"), RefTarget::normal(base.id().clone()));
        for (name, content) in [("a", "a\n"), ("b", "b\n"), ("c", "c\n")] {
            write_commit_with_tree(
                &mut tx,
                vec![base.id().clone()],
                &format!("feat: {name}"),
                file_tree(&store, &[("notes.txt", content)]),
            );
        }
        pollster::block_on(tx.commit("set up three siblings")).unwrap();
        let snap = backend.open(dir.path()).unwrap();
        let a_id = node_by_description(&snap, "feat: a").id.clone();
        let b_id = node_by_description(&snap, "feat: b").id.clone();
        let c_id = node_by_description(&snap, "feat: c").id.clone();
        backend.rebase_change(dir.path(), &b_id, &a_id).unwrap();
        backend.rebase_change(dir.path(), &c_id, &b_id).unwrap();

        let diff = backend.change_diff(dir.path(), &c_id).unwrap();
        let file = diff.files.iter().find(|f| f.path == "notes.txt").unwrap();
        let FileDiffContent::Conflict { sections, .. } = &file.content else {
            panic!("expected conflict content, got {:?}", file.content);
        };
        let regions: Vec<_> = sections
            .iter()
            .filter_map(|s| match s {
                ConflictSection::Region { sides } => Some(sides),
                _ => None,
            })
            .collect();
        assert_eq!(regions.len(), 1);
        assert_eq!(regions[0].len(), 3, "all three sides render: {:?}", regions[0]);
    }

    /// A bookmark moved to different targets in concurrent operations
    /// resolves to multiple candidates; the inbox item names them all and
    /// the bookmark list keeps an entry parked on the first.
    #[test]
    fn conflicted_bookmark_lists_its_candidate_targets() {
        let dir = tempfile::tempdir().unwrap();
        build_test_repo(dir.path());
        let backend = test_backend();
        let before = backend.open(dir.path()).unwrap();
        let feature_id = node_by_description(&before, "feat: first change").id.clone();
        let side_id = node_by_description(&before, "wip: side experiment").id.clone();

        let settings = test_settings();
        let workspace = Workspace::load(
            &settings,
            dir.path(),
            &default_backend_factories(),
            &default_working_copy_factories(),
        )
        .unwrap();
        let repo = pollster::block_on(workspace.repo_loader().load_at_head()).unwrap();
        let feature = resolve_change_commit(&repo, &feature_id).unwrap();
        let side = resolve_change_commit(&repo, &side_id).unwrap();
        for (commit, op) in [(&feature, "point release at feature"), (&side, "point release at side")]
        {
            let mut tx = repo.start_transaction();
            tx.repo_mut().set_local_bookmark_target(
                RefName::new("release"),
                RefTarget::normal(commit.id().clone()),
            );
            pollster::block_on(tx.commit(op)).unwrap();
        }

        let merged = backend.open(dir.path()).unwrap();
        let item = merged
            .conflicts
            .iter()
            .find(|c| c.kind == ConflictKind::Bookmark)
            .expect("bookmark conflict surfaced");
        assert_eq!(item.id, "bookmark-release");
        assert!(item.summary.contains("release"), "got {}", item.summary);
        let mut targets = item.targets.clone();
        targets.sort();
        let mut expected = vec![feature_id, side_id];
        expected.sort();
        assert_eq!(targets, expected);
        let bookmark = merged.bookmarks.iter().find(|b| b.name == "release").unwrap();
        assert!(item.targets.contains(&bookmark.target));
    }

    #[test]
    fn repo_config_overrides_immutable_heads() {
        let dir = tempfile::tempdir().unwrap();
        build_test_repo(dir.path());
        // Pin the feature bookmark immutable, like a team config would for
        // release branches. Its ancestors (trunk) become immutable with it.
        write_repo_config(
            dir.path(),
            "[revset-aliases]\n'immutable_heads()' = 'bookmarks(exact:\"feature-a\")'\n",
        );
        let backend = test_backend();
        let snapshot = backend.open(dir.path()).unwrap();

        assert_eq!(
            node_by_description(&snapshot, "feat: first change").kind,
            NodeKind::Immutable
        );
        assert_eq!(
            node_by_description(&snapshot, "release: cut 1.0").kind,
            NodeKind::Immutable
        );
        // The side stack and working copy stay mutable.
        assert_eq!(
            node_by_description(&snapshot, "wip: side experiment").kind,
            NodeKind::Mutable
        );
        let active = snapshot.workstreams.iter().find(|ws| ws.is_active).unwrap();
        assert_eq!(active.node_ids, vec![snapshot.working_copy.clone()]);

        // The same expression gates mutations.
        let feature_id = node_by_description(&snapshot, "feat: first change").id.clone();
        let err = backend.describe(dir.path(), &feature_id, "nope").unwrap_err();
        assert!(matches!(err, BackendError::ImmutableChange(_)));
        backend
            .describe(dir.path(), &snapshot.working_copy, "wip: still allowed")
            .unwrap();
    }

    #[test]
    fn user_config_supplies_identity() {
        let dir = tempfile::tempdir().unwrap();
        build_test_repo(dir.path());
        let config_dir = tempfile::tempdir().unwrap();
        let config_path = config_dir.path().join("config.toml");
        std::fs::write(
            &config_path,
            "user.name = \"Config User\"\nuser.email = \"config@example.com\"\n\
             operation.username = \"config-user\"\noperation.hostname = \"config-host\"\n",
        )
        .unwrap();
        let backend =
            JjBackend::with_user_config(UserConfigSource::Paths(vec![config_path]));

        let snapshot = backend.open(dir.path()).unwrap();
        backend
            .describe(dir.path(), &snapshot.working_copy, "wip: signed work")
            .unwrap();

        // The operation records the configured identity...
        let after = backend.open(dir.path()).unwrap();
        assert_eq!(after.operations[0].user, "config-user@config-host");

        // ...and the rewritten commit's committer comes from user config
        // (the original author is preserved by the rewrite).
        let settings = test_settings();
        let workspace = Workspace::load(
            &settings,
            dir.path(),
            &default_backend_factories(),
            &default_working_copy_factories(),
        )
        .unwrap();
        let repo = pollster::block_on(workspace.repo_loader().load_at_head()).unwrap();
        let wc_commit_id = repo
            .view()
            .get_wc_commit_id(WorkspaceName::DEFAULT)
            .unwrap()
            .clone();
        let commit = repo.store().get_commit(&wc_commit_id).unwrap();
        assert_eq!(commit.committer().name, "Config User");
        assert_eq!(commit.committer().email, "config@example.com");
        assert_eq!(commit.author().name, "Test User");
    }

    #[test]
    fn broken_immutable_heads_alias_is_a_hard_error() {
        let dir = tempfile::tempdir().unwrap();
        build_test_repo(dir.path());
        let backend = test_backend();
        let wc_id = backend.open(dir.path()).unwrap().working_copy;

        write_repo_config(
            dir.path(),
            "[revset-aliases]\n'immutable_heads()' = 'bookmarks(exact:'\n",
        );
        let err = backend.open(dir.path()).unwrap_err();
        assert!(matches!(err, BackendError::ConfigInvalid(_)), "got {err:?}");

        // Mutations refuse too: gating safety on a fallback the user
        // overrode would let Jiji rewrite what jj considers immutable.
        let err = backend.describe(dir.path(), &wc_id, "text").unwrap_err();
        assert!(matches!(err, BackendError::ConfigInvalid(_)), "got {err:?}");
    }

    #[test]
    fn refresh_records_working_copy_edits_then_converges() {
        let dir = tempfile::tempdir().unwrap();
        build_test_repo(dir.path());
        let backend = test_backend();

        std::fs::write(dir.path().join("notes.txt"), "draft\n").unwrap();
        let snapshot = backend.refresh(dir.path()).unwrap();

        // The edit landed in `@` as the CLI's own snapshot operation...
        assert_eq!(snapshot.operations[0].description, "snapshot working copy");
        assert!(snapshot.operations[0].is_snapshot);
        let wc_node = snapshot
            .nodes
            .iter()
            .find(|n| n.id == snapshot.working_copy)
            .unwrap();
        assert!(!wc_node.is_empty);
        let detail = backend.change_detail(dir.path(), &snapshot.working_copy).unwrap();
        assert!(detail.files.iter().any(|f| f.path == "notes.txt"));

        // ...and an already-current repo records nothing and produces an
        // identical snapshot (what lets auto-refresh suppress republishing).
        let again = backend.refresh(dir.path()).unwrap();
        assert_eq!(again, snapshot);
    }

    /// Rewrites `@`'s tree out-of-band without updating the on-disk
    /// working-copy state — what a mutation from another workspace's client
    /// looks like to this one.
    fn make_workspace_stale(root: &Path) {
        let settings = test_settings();
        let workspace = Workspace::load(
            &settings,
            root,
            &default_backend_factories(),
            &default_working_copy_factories(),
        )
        .unwrap();
        let repo = pollster::block_on(workspace.repo_loader().load_at_head()).unwrap();
        let wc_id = repo
            .view()
            .get_wc_commit_id(WorkspaceName::DEFAULT)
            .unwrap()
            .clone();
        let wc_commit = repo.store().get_commit(&wc_id).unwrap();
        let tree = file_tree(repo.store(), &[("other.txt", "rewritten\n")]);
        let mut tx = repo.start_transaction();
        pollster::block_on(tx.repo_mut().rewrite_commit(&wc_commit).set_tree(tree).write())
            .unwrap();
        pollster::block_on(tx.repo_mut().rebase_descendants()).unwrap();
        pollster::block_on(tx.commit("rewrite from elsewhere")).unwrap();
    }

    #[test]
    fn refresh_degrades_to_read_only_when_workspace_is_stale() {
        let dir = tempfile::tempdir().unwrap();
        build_test_repo(dir.path());
        let backend = test_backend();
        make_workspace_stale(dir.path());

        // Refresh still answers — read-only, without syncing — while
        // mutations keep refusing until the workspace is updated.
        let snapshot = backend.refresh(dir.path()).unwrap();
        assert_eq!(snapshot.operations[0].description, "rewrite from elsewhere");
        let err = backend
            .describe(dir.path(), &snapshot.working_copy, "nope")
            .unwrap_err();
        assert!(matches!(err, BackendError::StaleWorkspace(_)), "got {err:?}");
    }

    #[test]
    fn stale_workspace_surfaces_in_snapshot_and_update_recovers_it() {
        let dir = tempfile::tempdir().unwrap();
        build_test_repo(dir.path());
        let backend = test_backend();

        // A fresh repo reports a fresh current workspace and no stale item.
        let fresh = backend.open(dir.path()).unwrap();
        let current = fresh.workspaces.iter().find(|w| w.is_current).unwrap();
        assert!(current.is_default);
        assert!(!current.is_stale);
        assert!(fresh.conflicts.iter().all(|c| c.kind != ConflictKind::StaleWorkspace));

        check_out_working_copy(dir.path());
        make_workspace_stale(dir.path());

        // Detection: the summary flips and the inbox item names the current
        // workspace, pointing at the drawn working-copy row.
        let stale = backend.open(dir.path()).unwrap();
        assert!(stale.workspaces.iter().find(|w| w.is_current).unwrap().is_stale);
        let item = stale
            .conflicts
            .iter()
            .find(|c| c.kind == ConflictKind::StaleWorkspace)
            .unwrap();
        assert_eq!(item.workspace.as_deref(), Some("default"));
        assert_eq!(item.node_id.as_deref(), Some(stale.working_copy.as_str()));

        // An edit made while stale must survive the recovery.
        std::fs::write(dir.path().join("sneaky.txt"), "do not lose me\n").unwrap();

        let outcome = backend.update_stale_workspace(dir.path()).unwrap();
        assert_eq!(outcome.operation_id, None, "the checkout records no operation");
        assert!(
            outcome.summary.starts_with("Updated the workspace to"),
            "got {}",
            outcome.summary
        );

        // The repo's position is on disk now; the sneaky edit left the disk
        // (it is not part of the fresh tree) but was snapshotted into a
        // visible commit first, exactly like the CLI.
        assert_eq!(
            std::fs::read_to_string(dir.path().join("other.txt")).unwrap(),
            "rewritten\n"
        );
        assert!(!dir.path().join("sneaky.txt").exists());
        let settings = test_settings();
        let workspace = Workspace::load(
            &settings,
            dir.path(),
            &default_backend_factories(),
            &default_working_copy_factories(),
        )
        .unwrap();
        let repo = pollster::block_on(workspace.repo_loader().load_at_head()).unwrap();
        let sneaky = RepoPathBuf::from_internal_string("sneaky.txt").unwrap();
        let preserved = repo.view().heads().iter().any(|id| {
            let commit = repo.store().get_commit(id).unwrap();
            !pollster::block_on(commit.tree().path_value(&sneaky)).unwrap().is_absent()
        });
        assert!(preserved, "the stale working copy's edit was snapshotted");
        assert_workspace_fresh(dir.path());

        // The stale state cleared, and mutations work again. The rescued
        // stale copy is a visible sibling of the same change, so the change
        // is divergent and the outcome follows the checked-out copy by
        // commit id, the way a divergent copy is always addressed.
        let after = backend.open(dir.path()).unwrap();
        assert_eq!(outcome.target_change.as_deref(), Some(after.working_copy.as_str()));
        assert!(!after.workspaces.iter().find(|w| w.is_current).unwrap().is_stale);
        assert!(after.conflicts.iter().all(|c| c.kind != ConflictKind::StaleWorkspace));
        backend.describe(dir.path(), &after.working_copy, "back to life").unwrap();

        // Running the recovery on a fresh workspace records nothing.
        let noop = backend.update_stale_workspace(dir.path()).unwrap();
        assert_eq!(noop.operation_id, None);
        assert_eq!(noop.summary, "The workspace is not stale");
    }

    #[test]
    fn update_stale_workspace_recovers_when_the_working_copy_operation_is_lost() {
        let dir = tempfile::tempdir().unwrap();
        build_test_repo(dir.path());
        let backend = test_backend();
        check_out_working_copy(dir.path());
        make_workspace_stale(dir.path());

        // Simulate `jj op abandon` + `jj util gc` of the operation the
        // working copy was last updated at: reparent the op chain past it,
        // then delete its object.
        let settings = test_settings();
        let workspace = Workspace::load(
            &settings,
            dir.path(),
            &default_backend_factories(),
            &default_working_copy_factories(),
        )
        .unwrap();
        let repo = pollster::block_on(workspace.repo_loader().load_at_head()).unwrap();
        let lost_id = workspace.working_copy().operation_id().clone();
        let lost_op = pollster::block_on(repo.loader().load_operation(&lost_id)).unwrap();
        let dest = pollster::block_on(lost_op.parents()).unwrap().remove(0);
        let head = repo.operation().clone();
        let stats = pollster::block_on(op_walk::reparent_range(
            repo.op_store().as_ref(),
            std::slice::from_ref(&lost_op),
            std::slice::from_ref(&head),
            &dest,
        ))
        .unwrap();
        pollster::block_on(
            repo.op_heads_store()
                .update_op_heads(&[head.id().clone()], &stats.new_head_ids[0]),
        )
        .unwrap();
        std::fs::remove_file(
            dir.path().join(".jj/repo/op_store/operations").join(lost_id.hex()),
        )
        .unwrap();
        drop(repo);
        drop(workspace);

        // Detection reports the lost-operation flavor...
        let snapshot = backend.open(dir.path()).unwrap();
        let item = snapshot
            .conflicts
            .iter()
            .find(|c| c.kind == ConflictKind::StaleWorkspace)
            .unwrap();
        assert!(item.summary.contains("no longer has"), "got {}", item.summary);

        // ...and recovery parks the workspace's files in a recovery commit,
        // which is a real, undoable operation.
        let outcome = backend.update_stale_workspace(dir.path()).unwrap();
        assert!(outcome.operation_id.is_some());
        assert!(
            outcome.summary.starts_with("Recovered the workspace"),
            "got {}",
            outcome.summary
        );

        let after = backend.open(dir.path()).unwrap();
        assert_eq!(outcome.target_change.as_deref(), Some(after.working_copy.as_str()));
        let wc_node = after.nodes.iter().find(|n| n.id == after.working_copy).unwrap();
        assert!(
            wc_node.description.starts_with("RECOVERY COMMIT"),
            "got {}",
            wc_node.description
        );
        assert!(after.conflicts.iter().all(|c| c.kind != ConflictKind::StaleWorkspace));
        assert_workspace_fresh(dir.path());
    }

    #[test]
    fn init_colocated_takes_over_a_plain_git_repo() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let backend = test_backend();
        git(&root, &["init", "-q", "-b", "main"]);
        git(&root, &["config", "user.name", "Git User"]);
        git(&root, &["config", "user.email", "git@example.com"]);
        std::fs::write(root.join("readme.md"), "hello\n").unwrap();
        git(&root, &["add", "readme.md"]);
        git(&root, &["commit", "-q", "-m", "Initial commit"]);
        let head = git(&root, &["rev-parse", "HEAD"]);
        std::fs::create_dir(root.join("src")).unwrap();

        // Opening the repo, or a folder inside it, names the Git root.
        let err = backend.open(&root.join("src")).unwrap_err();
        match err {
            BackendError::GitRepoWithoutJj(git_root) => {
                assert_eq!(Path::new(&git_root), root.as_path())
            }
            other => panic!("expected GitRepoWithoutJj, got {other:?}"),
        }

        backend.init_colocated(&root).unwrap();
        assert_eq!(
            std::fs::read_to_string(root.join(".jj/.gitignore")).unwrap(),
            "/*\n"
        );
        // Git ignores jj's folder and still sees a clean checkout.
        assert_eq!(git(&root, &["status", "--porcelain"]), "");

        let snapshot = backend.refresh(&root).unwrap();
        let wc = snapshot
            .nodes
            .iter()
            .find(|n| n.id == snapshot.working_copy)
            .unwrap();
        assert!(wc.is_empty);
        let parent = snapshot
            .nodes
            .iter()
            .find(|n| wc.parents.contains(&n.id))
            .expect("the working copy sits on git HEAD");
        assert!(head.starts_with(&parent.commit_id), "{} vs {head}", parent.commit_id);
        assert_eq!(parent.description, "Initial commit");
        assert!(parent.bookmarks.iter().any(|b| b == "main"));

        let again = backend.init_colocated(&root).unwrap_err();
        assert!(again.to_string().contains("already has a .jj folder"));
    }

    #[test]
    fn init_colocated_refuses_folders_that_are_not_git_roots() {
        let dir = tempfile::tempdir().unwrap();
        let backend = test_backend();
        let err = backend.init_colocated(dir.path()).unwrap_err();
        assert!(err.to_string().contains("not the root of a Git repository"));
        assert!(!dir.path().join(".jj").exists());
    }

    /// Runs git against the colocated repo, like a user in a terminal.
    fn git(root: &Path, args: &[&str]) -> String {
        let output = std::process::Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .output()
            .expect("git binary available");
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    }

    /// A colocated repo built through real backend mutations (so git HEAD
    /// and refs are exported the way the app leaves them): `main` bookmark
    /// on a described "first change", with an empty working copy on top.
    /// Returns the first change's snapshot node id.
    fn build_colocated_repo(root: &Path, backend: &JjBackend) -> String {
        let settings = test_settings();
        pollster::block_on(Workspace::init_colocated_git(&settings, root, gix::hash::Kind::Sha1)).unwrap();
        // The backend loads its own settings (`UserConfigSource::None`), so
        // author identity has to come from repo config — like any real repo
        // has. Push refuses authorless commits, exactly like the CLI.
        write_repo_config(
            root,
            "user.name = \"Test User\"\nuser.email = \"test@example.com\"\n",
        );
        std::fs::write(root.join("file.txt"), "one\n").unwrap();
        backend.refresh(root).unwrap();
        let first = backend.refresh(root).unwrap().working_copy;
        backend.describe(root, &first, "first change").unwrap();
        backend.create_bookmark(root, "main", &first).unwrap();
        backend.new_change(root, &first).unwrap();
        first
    }

    /// Jiji's writers to one repo take turns. A refresh (the watcher's)
    /// that starts while another writer holds the repo — here the test,
    /// standing in for a fetch that has moved git refs but not committed
    /// its operation yet — waits, then loads the head that writer
    /// committed. Racing instead, it would import the half-fetched refs as
    /// a sibling operation, and jj's reconcile of the two could rebase
    /// trunk.
    #[test]
    fn writers_to_one_repo_take_turns() {
        let dir = tempfile::tempdir().unwrap();
        let backend = test_backend();
        let first = build_colocated_repo(dir.path(), &backend);
        let first_commit = {
            let snapshot = backend.open(dir.path()).unwrap();
            let short = &snapshot.nodes.iter().find(|n| n.id == first).unwrap().commit_id;
            git(dir.path(), &["rev-parse", short])
        };

        let (write, workspace, repo) = backend.load_repo_for_write(dir.path()).unwrap();
        git(dir.path(), &["branch", "topic", &first_commit]);
        // Its own backend instance: the lock is process-wide.
        let refresher = std::thread::spawn({
            let path = dir.path().to_owned();
            move || test_backend().refresh(&path).unwrap()
        });
        std::thread::sleep(std::time::Duration::from_millis(300));
        assert!(!refresher.is_finished(), "the refresh waits for the writer");

        let mut tx = repo.start_transaction();
        let options = git_import_options(workspace.settings()).unwrap();
        pollster::block_on(git::import_refs(tx.repo_mut(), &options)).unwrap();
        pollster::block_on(tx.commit("fetch from git remote(s) origin")).unwrap();
        drop(write);

        let snapshot = refresher.join().unwrap();
        assert_eq!(
            snapshot.operations[0].description, "fetch from git remote(s) origin",
            "the refresh found the refs already imported and recorded nothing"
        );
        assert!(snapshot.bookmarks.iter().any(|b| b.name == "topic"));
    }

    #[test]
    fn refresh_imports_externally_moved_git_refs() {
        let dir = tempfile::tempdir().unwrap();
        let backend = test_backend();
        let first = build_colocated_repo(dir.path(), &backend);
        let before = backend.open(dir.path()).unwrap();
        let first_commit = before
            .nodes
            .iter()
            .find(|n| n.id == first)
            .unwrap()
            .commit_id
            .clone();

        // A branch created by plain git appears as a bookmark on refresh.
        git(dir.path(), &["branch", "topic", &first_commit]);
        let snapshot = backend.refresh(dir.path()).unwrap();

        assert_eq!(snapshot.operations[0].description, "import git refs");
        let topic = snapshot
            .bookmarks
            .iter()
            .find(|b| b.name == "topic")
            .expect("externally-created branch imported");
        assert_eq!(topic.target, first);
        assert!(topic.is_local);
    }

    #[test]
    fn refresh_imports_externally_moved_git_head() {
        let dir = tempfile::tempdir().unwrap();
        let backend = test_backend();
        let first = build_colocated_repo(dir.path(), &backend);

        // Stack a second described change so HEAD has somewhere to move
        // back from (git HEAD tracks the working copy's parent).
        std::fs::write(dir.path().join("two.txt"), "two\n").unwrap();
        let second = backend.refresh(dir.path()).unwrap().working_copy;
        backend.describe(dir.path(), &second, "second change").unwrap();
        backend.new_change(dir.path(), &second).unwrap();

        let before = backend.open(dir.path()).unwrap();
        let first_commit = before
            .nodes
            .iter()
            .find(|n| n.id == first)
            .unwrap()
            .commit_id
            .clone();

        // `git checkout` in a terminal moves HEAD and the on-disk files;
        // refresh imports it as a fresh working copy on the new HEAD.
        git(dir.path(), &["checkout", "--detach", &first_commit]);
        let snapshot = backend.refresh(dir.path()).unwrap();

        assert!(snapshot
            .operations
            .iter()
            .any(|op| op.description == "import git head"));
        let wc_node = snapshot
            .nodes
            .iter()
            .find(|n| n.id == snapshot.working_copy)
            .unwrap();
        assert_eq!(wc_node.parents, vec![first.clone()]);
        assert!(wc_node.is_empty);
        // git updated the files; nothing bogus was snapshotted into `@`.
        assert!(!dir.path().join("two.txt").exists());

        // The discardable old working copy was abandoned, not left behind
        // as a stray sibling of the second change.
        let second_node = snapshot.nodes.iter().find(|n| n.id == second).unwrap();
        let children: Vec<_> = snapshot
            .nodes
            .iter()
            .filter(|n| n.parents.contains(&second_node.id))
            .collect();
        assert!(children.is_empty(), "old empty wc abandoned: {children:?}");
    }

    #[test]
    fn mutations_import_externally_moved_git_refs_first() {
        let dir = tempfile::tempdir().unwrap();
        let backend = test_backend();
        let first = build_colocated_repo(dir.path(), &backend);
        let before = backend.open(dir.path()).unwrap();
        let first_commit = before
            .nodes
            .iter()
            .find(|n| n.id == first)
            .unwrap()
            .commit_id
            .clone();

        git(dir.path(), &["branch", "topic", &first_commit]);
        backend
            .describe(dir.path(), &before.working_copy, "wip: described")
            .unwrap();

        // The import ran as its own operation before the describe, exactly
        // like the CLI does at the start of every command.
        let snapshot = backend.open(dir.path()).unwrap();
        assert!(snapshot.operations[0].description.starts_with("describe commit"));
        assert_eq!(snapshot.operations[1].description, "import git refs");
        assert!(snapshot.bookmarks.iter().any(|b| b.name == "topic"));
    }

    /// A bare git repo in its own directory, wired into the colocated
    /// fixture as the `origin` remote — pushes run against it over the
    /// file transport, no network or auth involved.
    fn add_bare_origin(root: &Path, bare: &Path) {
        add_bare_remote(root, bare, "origin");
    }

    /// Same, under any remote name — how the fetch tests build multi-remote
    /// shapes.
    fn add_bare_remote(root: &Path, bare: &Path, name: &str) {
        git(bare, &["init", "--bare", "--quiet"]);
        git(root, &["remote", "add", name, bare.to_str().unwrap()]);
    }

    /// Writes a new commit (empty tree) directly into a bare repo and
    /// points `branch` at it — how tests move a remote out-of-band without
    /// a second clone. Returns the full commit hex.
    fn bare_commit(bare: &Path, branch: &str, parent: &str, message: &str) -> String {
        let tree = git(bare, &["hash-object", "-t", "tree", "-w", "/dev/null"]);
        let commit = git(
            bare,
            &[
                "-c",
                "user.name=Upstream",
                "-c",
                "user.email=upstream@example.com",
                "commit-tree",
                &tree,
                "-p",
                parent,
                "-m",
                message,
            ],
        );
        git(bare, &["update-ref", &format!("refs/heads/{branch}"), &commit]);
        commit
    }

    /// Full commit hex a bare repo's branch points at, `None` when the
    /// branch does not exist.
    fn bare_branch(bare: &Path, name: &str) -> Option<String> {
        let output = std::process::Command::new("git")
            .arg("-C")
            .arg(bare)
            .args(["rev-parse", "--verify", "--quiet", &format!("refs/heads/{name}")])
            .output()
            .expect("git binary available");
        output
            .status
            .success()
            .then(|| String::from_utf8(output.stdout).unwrap().trim().to_owned())
    }

    #[test]
    fn push_creates_moves_and_deletes_remote_bookmarks() {
        let dir = tempfile::tempdir().unwrap();
        let bare = tempfile::tempdir().unwrap();
        let backend = test_backend();
        let _first = build_colocated_repo(dir.path(), &backend);
        add_bare_origin(dir.path(), bare.path());

        // Pushing a new bookmark creates the remote branch and starts
        // tracking it; an immutable target (main is trunk) is fine — a
        // push rewrites nothing.
        let outcome = backend
            .push_bookmarks(dir.path(), &["main".into()], None)
            .unwrap();
        assert!(outcome.operation_id.is_some());
        assert_eq!(outcome.summary, "Pushed main to origin");
        assert!(outcome.target_change.is_none());
        let snapshot = backend.open(dir.path()).unwrap();
        assert_eq!(
            snapshot.operations[0].description,
            "push bookmark main to git remote origin"
        );
        let main = snapshot.bookmarks.iter().find(|b| b.name == "main").unwrap();
        assert_eq!(main.remote.as_deref(), Some("origin"));
        assert_eq!(main.sync, SyncState::Synced);
        let first_commit = node_by_description(&snapshot, "first change").commit_id.clone();
        assert!(bare_branch(bare.path(), "main").unwrap().starts_with(&first_commit));

        // Everything in place records nothing, like the CLI's "Nothing
        // changed."; requested names already matching are just skipped.
        let noop = backend
            .push_bookmarks(dir.path(), &["main".into()], None)
            .unwrap();
        assert!(noop.operation_id.is_none());
        assert_eq!(noop.summary, "Everything is already pushed to origin");

        // Stack two described bookmarks on top; a mixed request pushes
        // only what moved and the op description names exactly that.
        let feature_change = backend.open(dir.path()).unwrap().working_copy;
        backend.describe(dir.path(), &feature_change, "feat: work").unwrap();
        backend.create_bookmark(dir.path(), "feature", &feature_change).unwrap();
        backend.new_change(dir.path(), &feature_change).unwrap();
        let stacked_change = backend.open(dir.path()).unwrap().working_copy;
        backend.describe(dir.path(), &stacked_change, "feat: stacked").unwrap();
        backend.create_bookmark(dir.path(), "stacked", &stacked_change).unwrap();
        backend.new_change(dir.path(), &stacked_change).unwrap();
        let outcome = backend
            .push_bookmarks(dir.path(), &["main".into(), "feature".into()], None)
            .unwrap();
        assert_eq!(outcome.summary, "Pushed feature to origin");
        let snapshot = backend.open(dir.path()).unwrap();
        assert_eq!(
            snapshot.operations[0].description,
            "push bookmark feature to git remote origin"
        );

        // Rewriting the stack moves both bookmarks; one call pushes both
        // in one operation, with the CLI's plural op description.
        backend
            .describe(dir.path(), &feature_change, "feat: work, amended")
            .unwrap();
        let outcome = backend
            .push_bookmarks(dir.path(), &["feature".into(), "stacked".into()], None)
            .unwrap();
        assert_eq!(outcome.summary, "Pushed 2 bookmarks to origin");
        let snapshot = backend.open(dir.path()).unwrap();
        assert_eq!(
            snapshot.operations[0].description,
            "push bookmarks feature, stacked to git remote origin"
        );
        for name in ["feature", "stacked"] {
            let bookmark = snapshot.bookmarks.iter().find(|b| b.name == name).unwrap();
            assert_eq!(bookmark.sync, SyncState::Synced, "{name} synced after push");
            let target_commit = snapshot
                .nodes
                .iter()
                .find(|n| n.id == bookmark.target)
                .unwrap()
                .commit_id
                .clone();
            assert!(
                bare_branch(bare.path(), name).unwrap().starts_with(&target_commit),
                "{name} on the remote where jj says it is"
            );
        }

        // Deleting locally and pushing the name propagates the deletion:
        // the remote branch goes away and the tombstone with it.
        backend.delete_bookmark(dir.path(), "stacked").unwrap();
        let outcome = backend
            .push_bookmarks(dir.path(), &["stacked".into()], None)
            .unwrap();
        assert_eq!(outcome.summary, "Deleted stacked on origin");
        let snapshot = backend.open(dir.path()).unwrap();
        assert_eq!(
            snapshot.operations[0].description,
            "push bookmark stacked to git remote origin"
        );
        assert!(bare_branch(bare.path(), "stacked").is_none());
        assert!(!snapshot.bookmarks.iter().any(|b| b.name == "stacked"));
    }

    #[test]
    fn push_refuses_unpresentable_commits_and_bad_requests() {
        let dir = tempfile::tempdir().unwrap();
        let bare = tempfile::tempdir().unwrap();
        let backend = test_backend();
        let first = build_colocated_repo(dir.path(), &backend);
        add_bare_origin(dir.path(), bare.path());
        let before = backend.open(dir.path()).unwrap();
        let first_commit = node_by_description(&before, "first change").commit_id.clone();

        // A bookmark on the undescribed working copy refuses with the
        // CLI's wording, recording nothing.
        let wc = before.working_copy.clone();
        backend.create_bookmark(dir.path(), "wip", &wc).unwrap();
        let top_op = backend.open(dir.path()).unwrap().operations[0].id.clone();
        let err = backend
            .push_bookmarks(dir.path(), &["wip".into()], None)
            .unwrap_err();
        assert!(matches!(err, BackendError::MutationFailed(_)), "got {err:?}");
        assert!(err.to_string().contains("has no description"), "{err}");
        assert!(bare_branch(bare.path(), "wip").is_none());

        // A conflicted commit refuses the same way.
        backend.describe(dir.path(), &wc, "feat: ours").unwrap();
        std::fs::write(dir.path().join("notes.txt"), "ours\n").unwrap();
        backend.refresh(dir.path()).unwrap();
        backend.new_change(dir.path(), &first).unwrap();
        let theirs = backend.open(dir.path()).unwrap().working_copy;
        std::fs::write(dir.path().join("notes.txt"), "theirs\n").unwrap();
        backend.describe(dir.path(), &theirs, "feat: theirs").unwrap();
        backend.move_change(dir.path(), &theirs, &wc).unwrap();
        backend.create_bookmark(dir.path(), "cbm", &theirs).unwrap();
        let err = backend
            .push_bookmarks(dir.path(), &["cbm".into()], None)
            .unwrap_err();
        assert!(err.to_string().contains("has conflicts"), "{err}");

        // Unknown names refuse (the CLI warns and exits cleanly — from an
        // explicit UI action that is a stale plan); unknown remotes and
        // empty requests refuse too.
        let err = backend
            .push_bookmarks(dir.path(), &["nope".into()], None)
            .unwrap_err();
        assert!(matches!(err, BackendError::BookmarkMissing(_)), "got {err:?}");
        let err = backend
            .push_bookmarks(dir.path(), &["wip".into()], Some("upstream"))
            .unwrap_err();
        assert!(err.to_string().contains("no git remote named"), "{err}");
        let err = backend.push_bookmarks(dir.path(), &[], None).unwrap_err();
        assert!(err.to_string().contains("no bookmarks"), "{err}");

        // A remote branch jj never tracked blocks a same-name local push,
        // like the CLI's "Non-tracking remote bookmark exists". Plain git
        // creates it (and the local remote-tracking ref); the refresh
        // imports it untracked.
        let full_first = git(dir.path(), &["rev-parse", &first_commit]);
        git(
            dir.path(),
            &["push", "--quiet", "origin", &format!("{full_first}:refs/heads/standalone")],
        );
        backend.refresh(dir.path()).unwrap();
        backend.create_bookmark(dir.path(), "standalone", &first).unwrap();
        let err = backend
            .push_bookmarks(dir.path(), &["standalone".into()], None)
            .unwrap_err();
        assert!(err.to_string().contains("non-tracking remote bookmark"), "{err}");

        // A conflicted bookmark refuses before anything runs. The two
        // concurrent moves need *sibling* targets — ancestor/descendant
        // adds fast-forward instead of conflicting.
        backend.new_change(dir.path(), &first).unwrap();
        let sibling = backend.open(dir.path()).unwrap().working_copy;
        let settings = test_settings();
        let workspace = Workspace::load(
            &settings,
            dir.path(),
            &default_backend_factories(),
            &default_working_copy_factories(),
        )
        .unwrap();
        let repo = pollster::block_on(workspace.repo_loader().load_at_head()).unwrap();
        let ours_commit = resolve_change_commit(&repo, &wc).unwrap();
        let sibling_commit = resolve_change_commit(&repo, &sibling).unwrap();
        for (commit, op) in [(&ours_commit, "release at ours"), (&sibling_commit, "release at sibling")] {
            let mut tx = repo.start_transaction();
            tx.repo_mut().set_local_bookmark_target(
                RefName::new("release"),
                RefTarget::normal(commit.id().clone()),
            );
            pollster::block_on(tx.commit(op)).unwrap();
        }
        let err = backend
            .push_bookmarks(dir.path(), &["release".into()], None)
            .unwrap_err();
        assert!(err.to_string().contains("is conflicted"), "{err}");

        // None of the refusals recorded a push operation.
        let ops = backend.open(dir.path()).unwrap().operations;
        assert!(
            ops.iter().all(|op| !op.description.starts_with("push bookmark")),
            "refusals recorded nothing"
        );
        let _ = top_op;
    }

    #[test]
    fn push_refuses_when_the_remote_moved_underneath() {
        let dir = tempfile::tempdir().unwrap();
        let bare = tempfile::tempdir().unwrap();
        let backend = test_backend();
        let first = build_colocated_repo(dir.path(), &backend);
        add_bare_origin(dir.path(), bare.path());
        let feature_change = backend.open(dir.path()).unwrap().working_copy;
        backend.describe(dir.path(), &feature_change, "feat: work").unwrap();
        backend
            .create_bookmark(dir.path(), "feature", &feature_change)
            .unwrap();
        backend
            .push_bookmarks(dir.path(), &["main".into(), "feature".into()], None)
            .unwrap();

        // Someone else moves the remote branch (straight in the bare repo,
        // so this workspace has no idea). The next push must not overwrite
        // their move: the force-with-lease expectation fails and nothing
        // records — the CLI's exact posture.
        let first_snapshot = backend.open(dir.path()).unwrap();
        let first_commit_full = bare_branch(bare.path(), "main").unwrap();
        git(bare.path(), &["update-ref", "refs/heads/feature", &first_commit_full]);
        backend
            .describe(dir.path(), &feature_change, "feat: work, amended")
            .unwrap();
        let err = backend
            .push_bookmarks(dir.path(), &["feature".into()], None)
            .unwrap_err();
        assert!(matches!(err, BackendError::MutationFailed(_)), "got {err:?}");
        assert!(err.to_string().contains("moved on the remote"), "{err}");
        assert!(err.to_string().contains("fetch"), "{err}");
        let ops = backend.open(dir.path()).unwrap().operations;
        assert!(
            ops[0].description.starts_with("describe commit"),
            "the failed push recorded nothing: {}",
            ops[0].description
        );
        // The remote still holds the other client's position.
        assert_eq!(bare_branch(bare.path(), "feature").unwrap(), first_commit_full);
        let _ = (first, first_snapshot);
    }
    #[test]
    fn fetch_imports_remote_moves_and_prunes() {
        let dir = tempfile::tempdir().unwrap();
        let bare = tempfile::tempdir().unwrap();
        let backend = test_backend();
        let first = build_colocated_repo(dir.path(), &backend);
        add_bare_origin(dir.path(), bare.path());

        // Local work published: `feature` on the described change under the
        // working copy, `main` as trunk, both tracked after the push.
        let feature_change = backend.open(dir.path()).unwrap().working_copy;
        backend
            .describe(dir.path(), &feature_change, "feat: local work")
            .unwrap();
        backend
            .create_bookmark(dir.path(), "feature", &feature_change)
            .unwrap();
        backend.new_change(dir.path(), &feature_change).unwrap();
        backend
            .push_bookmarks(dir.path(), &["main".into(), "feature".into()], None)
            .unwrap();

        // Nothing new on the remote: records nothing, like the CLI's
        // "Nothing changed.".
        let noop = backend.git_fetch(dir.path(), None).unwrap();
        assert!(noop.operation_id.is_none());
        assert_eq!(noop.summary, "Nothing new on origin");

        // Someone lands a commit on the remote's main (straight in the
        // bare repo, so this workspace has no idea). A dirty on-disk edit
        // snapshots first, like every mutation. Fetch imports the move:
        // the tracked local main fast-forwards, and the feature stack now
        // reads behind trunk — "the remote moved under you" made visible.
        std::fs::write(dir.path().join("dirty.txt"), "dirty\n").unwrap();
        let old_main = bare_branch(bare.path(), "main").unwrap();
        let new_main = bare_commit(bare.path(), "main", &old_main, "upstream: lands work");
        let outcome = backend.git_fetch(dir.path(), None).unwrap();
        assert!(outcome.operation_id.is_some());
        assert_eq!(outcome.summary, "Fetched 1 bookmark update from origin");
        assert!(outcome.target_change.is_none());
        let snapshot = backend.open(dir.path()).unwrap();
        assert_eq!(
            snapshot.operations[0].description,
            "fetch from git remote(s) origin"
        );
        assert_eq!(snapshot.operations[1].description, "snapshot working copy");
        let main = snapshot.bookmarks.iter().find(|b| b.name == "main").unwrap();
        assert_eq!(main.sync, SyncState::Synced, "tracked main followed the remote");
        let main_node = snapshot.nodes.iter().find(|n| n.id == main.target).unwrap();
        assert!(new_main.starts_with(&main_node.commit_id));
        let feature_stream = snapshot
            .workstreams
            .iter()
            .find(|w| w.bookmark.as_deref() == Some("feature"))
            .expect("feature stack still a workstream");
        assert_eq!(feature_stream.behind_trunk, 1, "stack reads behind the moved trunk");

        // Deleting a branch on the remote prunes on fetch: the tracked
        // local bookmark follows the deletion, the now-unreachable commit
        // is abandoned (`git.abandon-unreachable-commits`), and its
        // descendant working copy rebases onto the surviving parent.
        git(bare.path(), &["update-ref", "-d", "refs/heads/feature"]);
        let outcome = backend.git_fetch(dir.path(), None).unwrap();
        assert!(outcome.operation_id.is_some());
        assert_eq!(outcome.summary, "Fetched 1 bookmark update from origin");
        let snapshot = backend.open(dir.path()).unwrap();
        assert!(
            !snapshot.bookmarks.iter().any(|b| b.name == "feature"),
            "tracked bookmark followed the remote deletion"
        );
        assert!(
            !snapshot
                .nodes
                .iter()
                .any(|n| n.description.starts_with("feat: local work")),
            "unreachable commit abandoned"
        );
        let wc_node = snapshot
            .nodes
            .iter()
            .find(|n| n.id == snapshot.working_copy)
            .unwrap();
        assert_eq!(wc_node.parents, vec![first.clone()], "working copy rebased onto the parent");
    }

    /// trunk ─ lower ─ upper, both bookmarked and pushed, in `root`
    /// with `bare` as origin. Answers the two changes' ids.
    fn pushed_pair(root: &Path, bare: &Path, backend: &JjBackend) -> (String, String) {
        let first = build_colocated_repo(root, backend);
        add_bare_origin(root, bare);
        let (lower, _) = record_change(root, backend, &first, "lower", &[("lower.txt", "l\n")]);
        let (upper, _) = record_change(root, backend, &lower, "upper", &[("upper.txt", "u\n")]);
        backend.create_bookmark(root, "lower", &lower).unwrap();
        backend.create_bookmark(root, "upper", &upper).unwrap();
        let names = ["main".to_owned(), "lower".to_owned(), "upper".to_owned()];
        backend.push_bookmarks(root, &names, None).unwrap();
        (lower, upper)
    }

    /// Someone pushes a branch on top of a published stack: a teammate,
    /// or the same person from another clone. jj calls everything an
    /// untracked remote bookmark contains immutable, so after the fetch
    /// the whole stack is. It is still the user's stack: drawn in the
    /// default view, one workstream, each change marked as held.
    #[test]
    fn a_stack_someone_else_built_on_is_held_not_gone() {
        let dir = tempfile::tempdir().unwrap();
        let bare = tempfile::tempdir().unwrap();
        let backend = test_backend();
        let root = dir.path();
        let (lower, upper) = pushed_pair(root, bare.path(), &backend);

        let before = backend.open(root).unwrap();
        assert!(before.nodes.iter().all(|n| !n.held));

        // Their branch arrives, on top of `upper`.
        let upper_commit = bare_branch(bare.path(), "upper").unwrap();
        bare_commit(bare.path(), "theirs", &upper_commit, "theirs: on top");
        backend.git_fetch(root, None).unwrap();

        let snapshot = backend.open(root).unwrap();
        for id in [&lower, &upper] {
            let node = snapshot.nodes.iter().find(|n| n.id == *id).expect("still drawn");
            // jj's answer stands: nothing here may rewrite it by hand.
            assert_eq!(node.kind, NodeKind::Immutable, "{id}");
            assert!(node.held, "{id}");
            assert!(!node.is_settled(), "{id}");
        }
        assert!(matches!(
            backend.describe(root, &upper, "edited").unwrap_err(),
            BackendError::ImmutableChange(_)
        ));
        // Their commit is theirs: not drawn here, and not held.
        assert!(!snapshot.nodes.iter().any(|n| n.description == "theirs: on top"));
        // The stack is one workstream still, named by its bookmark.
        let stream = snapshot
            .workstreams
            .iter()
            .find(|ws| ws.node_ids.contains(&upper))
            .expect("the stack is a workstream");
        assert!(stream.node_ids.contains(&lower), "{:?}", stream.node_ids);
        // Trunk is settled history, as ever.
        let trunk = snapshot.nodes.iter().find(|n| n.bookmarks.contains(&"main".to_owned())).unwrap();
        assert!(trunk.is_settled() && !trunk.held);

        // In the Team view their commit is drawn too, as the immutable
        // context it is.
        let team = backend.open_with(root, &SnapshotView::Team).unwrap();
        let theirs = team.nodes.iter().find(|n| n.description == "theirs: on top").unwrap();
        assert!(theirs.is_settled());
        assert!(team.nodes.iter().find(|n| n.id == upper).unwrap().held);
    }

    /// Only a branch that rests on the user's work makes it held. A
    /// teammate's branch the user built on is the other way round: their
    /// commits are below the user's bookmark, and stay plain immutable
    /// history. A second remote branch at the very commit of a local
    /// bookmark holds it like one above.
    #[test]
    fn held_is_for_work_under_someone_elses_branch_not_over_it() {
        let dir = tempfile::tempdir().unwrap();
        let bare = tempfile::tempdir().unwrap();
        let backend = test_backend();
        let root = dir.path();
        build_colocated_repo(root, &backend);
        add_bare_origin(root, bare.path());
        backend.push_bookmarks(root, &["main".into()], None).unwrap();

        // A teammate's branch on trunk, and the user's change on top of it.
        let main_commit = bare_branch(bare.path(), "main").unwrap();
        bare_commit(bare.path(), "their-base", &main_commit, "theirs: a base");
        backend.git_fetch(root, None).unwrap();
        let team = backend.open_with(root, &SnapshotView::Team).unwrap();
        let base = team.nodes.iter().find(|n| n.description == "theirs: a base").unwrap().id.clone();
        let (mine, _) = record_change(root, &backend, &base, "mine", &[("mine.txt", "m\n")]);
        backend.create_bookmark(root, "mine", &mine).unwrap();
        backend.push_bookmarks(root, &["mine".into()], None).unwrap();

        let snapshot = backend.open(root).unwrap();
        let base_node = snapshot.nodes.iter().find(|n| n.id == base).expect("drawn as the base");
        assert!(base_node.is_settled() && !base_node.held);
        let mine_node = snapshot.nodes.iter().find(|n| n.id == mine).unwrap();
        assert_eq!(mine_node.kind, NodeKind::Mutable);

        // Another branch appears on the remote at the commit `mine` is on.
        let mine_commit = bare_branch(bare.path(), "mine").unwrap();
        git(bare.path(), &["update-ref", "refs/heads/copy-of-mine", &mine_commit]);
        backend.git_fetch(root, None).unwrap();
        let snapshot = backend.open(root).unwrap();
        let mine_node = snapshot.nodes.iter().find(|n| n.id == mine).unwrap();
        assert_eq!(mine_node.kind, NodeKind::Immutable);
        assert!(mine_node.held);
        // The teammate's base under it is still theirs.
        assert!(!snapshot.nodes.iter().find(|n| n.id == base).unwrap().held);
    }

    /// What GitHub's squash merge leaves on the remote: one commit on
    /// main with `commit`'s tree. `commit` must sit directly on main.
    fn bare_squash(bare: &Path, commit: &str, message: &str) -> String {
        let tree = git(bare, &["rev-parse", &format!("{commit}^{{tree}}")]);
        let parent = bare_branch(bare, "main").unwrap();
        let squash = git(
            bare,
            &[
                "-c",
                "user.name=Upstream",
                "-c",
                "user.email=upstream@example.com",
                "commit-tree",
                &tree,
                "-p",
                &parent,
                "-m",
                message,
            ],
        );
        git(bare, &["update-ref", "refs/heads/main", &squash]);
        squash
    }

    /// Landing the bottom of a stack while someone else's pull request
    /// sits on its top. The fetch that brings the squash brings their
    /// branch too, and jj then calls what is left of the stack immutable.
    /// It still has to move onto trunk; their commit must not be
    /// rewritten on the way, and no copy of it may appear here.
    #[test]
    fn landing_moves_a_held_stack_and_leaves_someone_elses_commits_alone() {
        let dir = tempfile::tempdir().unwrap();
        let bare = tempfile::tempdir().unwrap();
        let backend = test_backend();
        let root = dir.path();
        let (lower, upper) = pushed_pair(root, bare.path(), &backend);
        let lower_commit = bare_branch(bare.path(), "lower").unwrap();
        let upper_commit = bare_branch(bare.path(), "upper").unwrap();
        let theirs = bare_commit(bare.path(), "theirs", &upper_commit, "theirs: on top");
        let squash = bare_squash(bare.path(), &lower_commit, "lower (#1)");
        backend.git_fetch(root, None).unwrap();
        assert!(backend.open(root).unwrap().nodes.iter().find(|n| n.id == upper).unwrap().held);

        // A hand edit is refused as before; the landing's rebase is not.
        assert!(matches!(
            backend.rebase_change(root, &upper, &lower).unwrap_err(),
            BackendError::ImmutableChange(_)
        ));
        // Before that rebase, `upper` still sits on the landed commit. It
        // is the user's own, though their branch holds it: the sweep
        // stops for it as for any change of the user's, and does not take
        // it for someone else's commit that may stay behind.
        let err = backend
            .abandon_landed(root, std::slice::from_ref(&lower_commit))
            .unwrap_err();
        assert!(err.to_string().contains("on the landed commits"), "{err}");
        let outcome = backend
            .rebase_onto_trunk(root, "origin", "main", &upper, std::slice::from_ref(&lower_commit))
            .unwrap();
        assert!(
            outcome.summary.ends_with(
                "; theirs@origin still builds on the commits this replaced and was left as it is"
            ),
            "{}",
            outcome.summary
        );

        let snapshot = backend.open(root).unwrap();
        let moved = snapshot.nodes.iter().find(|n| n.id == upper).unwrap();
        let parent = snapshot.nodes.iter().find(|n| n.id == moved.parents[0]).unwrap();
        assert!(squash.starts_with(&parent.commit_id), "upper sits on the squash now");
        // Nothing holds it any more: their branch is on the old commits.
        assert_eq!(moved.kind, NodeKind::Mutable);
        assert!(!moved.held && !moved.is_divergent);
        // Their branch is where it was, on the commit they pushed, and
        // no visible commit here is a copy of theirs.
        assert_eq!(git(root, &["rev-parse", "refs/remotes/origin/theirs"]), theirs);
        let all = backend.open_with(root, &SnapshotView::All).unwrap();
        assert!(
            !all.nodes.iter().any(|n| n.description == "theirs: on top"),
            "their commit is neither rewritten nor shown"
        );

        // The landed commit is swept although their branch still has it
        // in its history (which is all that makes it immutable now).
        let outcome = backend.abandon_landed(root, std::slice::from_ref(&lower_commit)).unwrap();
        assert!(outcome.summary.starts_with("Abandoned"), "{}", outcome.summary);
        let all = backend.open_with(root, &SnapshotView::All).unwrap();
        assert!(!all.nodes.iter().any(|n| n.id == lower));
    }

    /// Their commit moves only when something of the user's sits on it:
    /// that work cannot stay behind, and it needs what it was built on.
    #[test]
    fn someone_elses_commit_the_user_built_on_moves_with_that_work() {
        let dir = tempfile::tempdir().unwrap();
        let bare = tempfile::tempdir().unwrap();
        let backend = test_backend();
        let root = dir.path();
        let (_lower, upper) = pushed_pair(root, bare.path(), &backend);
        let lower_commit = bare_branch(bare.path(), "lower").unwrap();
        let upper_commit = bare_branch(bare.path(), "upper").unwrap();
        bare_commit(bare.path(), "theirs", &upper_commit, "theirs: on top");
        backend.git_fetch(root, None).unwrap();
        let team = backend.open_with(root, &SnapshotView::Team).unwrap();
        let theirs = team.nodes.iter().find(|n| n.description == "theirs: on top").unwrap().id.clone();
        let (mine, _) = record_change(root, &backend, &theirs, "mine on theirs", &[("m.txt", "m\n")]);
        bare_squash(bare.path(), &lower_commit, "lower (#1)");
        backend.git_fetch(root, None).unwrap();

        let outcome = backend
            .rebase_onto_trunk(root, "origin", "main", &upper, std::slice::from_ref(&lower_commit))
            .unwrap();
        assert!(!outcome.summary.contains("left as"), "{}", outcome.summary);
        let snapshot = backend.open(root).unwrap();
        let mine_node = snapshot.nodes.iter().find(|n| n.id == mine).unwrap();
        let below = snapshot.nodes.iter().find(|n| n.id == mine_node.parents[0]).unwrap();
        assert_eq!(below.description, "theirs: on top");
        assert_eq!(below.parents.as_slice(), std::slice::from_ref(&upper));
    }

    /// Their pull request was based on the branch that landed, beside
    /// what is left of the stack. The sweep would abandon the landed
    /// commit under their commit, and jj would rebase theirs onto what
    /// was below it. The landed commit stays instead, as part of their
    /// branch's history, and the sweep says so without failing.
    #[test]
    fn the_sweep_leaves_a_landed_commit_someone_elses_branch_sits_on() {
        let dir = tempfile::tempdir().unwrap();
        let bare = tempfile::tempdir().unwrap();
        let backend = test_backend();
        let root = dir.path();
        let (lower, upper) = pushed_pair(root, bare.path(), &backend);
        let lower_commit = bare_branch(bare.path(), "lower").unwrap();
        let theirs = bare_commit(bare.path(), "theirs", &lower_commit, "theirs: beside upper");
        bare_squash(bare.path(), &lower_commit, "lower (#1)");
        backend.git_fetch(root, None).unwrap();
        backend
            .rebase_onto_trunk(root, "origin", "main", &upper, std::slice::from_ref(&lower_commit))
            .unwrap();
        backend.delete_bookmark(root, "lower").unwrap();

        let outcome = backend.abandon_landed(root, std::slice::from_ref(&lower_commit)).unwrap();
        assert_eq!(outcome.operation_id, None, "nothing was recorded");
        assert!(
            outcome.summary.starts_with(crate::backend::LANDED_LEFT_IN_PLACE)
                && outcome.summary.ends_with("in place: theirs@origin still builds on it"),
            "{}",
            outcome.summary
        );
        // Asked by itself, the same answer: whose branch keeps the landed
        // commit. A commit that is part of trunk rests under none, and
        // neither does an id that names nothing here.
        assert_eq!(
            backend.branches_resting_on(root, std::slice::from_ref(&lower_commit)).unwrap(),
            ["theirs@origin"]
        );
        // Nor does anything rest on a commit of trunk's history, however
        // many branches were made on top of it since.
        let trunk_commit = bare_branch(bare.path(), "main").unwrap();
        bare_commit(bare.path(), "later", &trunk_commit, "later: started on trunk");
        backend.git_fetch(root, None).unwrap();
        assert!(backend.branches_resting_on(root, &[trunk_commit]).unwrap().is_empty());
        assert_eq!(
            backend.branches_resting_on(root, std::slice::from_ref(&lower_commit)).unwrap(),
            ["theirs@origin"]
        );
        assert!(backend
            .branches_resting_on(root, &["0123456789abcdef0123456789abcdef01234567".into()])
            .unwrap()
            .is_empty());
        // Their commit is as they pushed it, on the landed commit.
        assert_eq!(git(root, &["rev-parse", "refs/remotes/origin/theirs"]), theirs);
        let team = backend.open_with(root, &SnapshotView::Team).unwrap();
        let theirs_node = team.nodes.iter().find(|n| n.description == "theirs: beside upper").unwrap();
        assert_eq!(theirs_node.parents.as_slice(), std::slice::from_ref(&lower));

        // A change of the user's own on a landed commit still stops the
        // sweep, as before: that one would lose what landed.
        let (_late, _) = record_change(root, &backend, &lower, "late", &[("late.txt", "x\n")]);
        let err = backend.abandon_landed(root, std::slice::from_ref(&lower_commit)).unwrap_err();
        assert!(err.to_string().contains("on the landed commits"), "{err}");
    }

    /// A commit made on the remote by someone else: `parent`'s tree with
    /// `file` set to `text`, on `branch`. Returns the full commit hex.
    fn bare_file_commit(
        bare: &Path,
        branch: &str,
        parent: &str,
        message: &str,
        file: &str,
        text: &str,
    ) -> String {
        let piped = |args: &[&str], input: &str| -> String {
            use std::io::Write as _;
            let mut child = std::process::Command::new("git")
                .arg("-C")
                .arg(bare)
                .args(args)
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .spawn()
                .expect("git binary available");
            child.stdin.take().unwrap().write_all(input.as_bytes()).unwrap();
            let output = child.wait_with_output().unwrap();
            assert!(output.status.success(), "git {args:?}");
            String::from_utf8(output.stdout).unwrap().trim().to_owned()
        };
        let blob = piped(&["hash-object", "-w", "--stdin"], text);
        let mut entries: Vec<String> = git(bare, &["ls-tree", parent])
            .lines()
            .filter(|line| !line.ends_with(&format!("\t{file}")))
            .map(str::to_owned)
            .collect();
        entries.push(format!("100644 blob {blob}\t{file}"));
        let tree = piped(&["mktree"], &(entries.join("\n") + "\n"));
        let commit = git(
            bare,
            &[
                "-c",
                "user.name=Upstream",
                "-c",
                "user.email=upstream@example.com",
                "commit-tree",
                &tree,
                "-p",
                parent,
                "-m",
                message,
            ],
        );
        git(bare, &["update-ref", &format!("refs/heads/{branch}"), &commit]);
        commit
    }

    /// The user rewrote a change after someone else had built on it. The
    /// fetch that brings their branch brings the old commit back into
    /// view: the change now has two commits, and the old one is not the
    /// user's to abandon. The snapshot says which kind of divergence
    /// this is, and a landing's rebase goes on with the user's commit and
    /// leaves their branch, and the old commit it keeps, where they are.
    #[test]
    fn an_old_copy_someone_elses_branch_keeps_is_not_the_users_divergence() {
        let dir = tempfile::tempdir().unwrap();
        let bare = tempfile::tempdir().unwrap();
        let backend = test_backend();
        let root = dir.path();
        let (lower, upper) = pushed_pair(root, bare.path(), &backend);
        let lower_commit = bare_branch(bare.path(), "lower").unwrap();
        let old_upper = bare_branch(bare.path(), "upper").unwrap();
        // They build on `upper` as it is. The user amends it before
        // fetching, and publishes the amended one.
        let theirs = bare_file_commit(
            bare.path(),
            "theirs",
            &old_upper,
            "theirs: on top",
            "theirs.txt",
            "t\n",
        );
        backend.describe(root, &upper, "upper, amended").unwrap();
        backend.push_bookmarks(root, &["upper".into()], None).unwrap();
        backend.git_fetch(root, None).unwrap();

        let amended = |snapshot: &RepoSnapshot| {
            snapshot
                .nodes
                .iter()
                .find(|n| n.description == "upper, amended")
                .cloned()
                .expect("the amended change is drawn")
        };
        let snapshot = backend.open(root).unwrap();
        let node = amended(&snapshot);
        // Two commits of one change, and the other is theirs to keep.
        assert!(node.is_divergent && node.old_copies_held && !node.diverged_here());
        assert_eq!(node.kind, NodeKind::Mutable);
        assert!(snapshot.nodes.iter().find(|n| n.id == lower).unwrap().held);
        // Their branch rests on the commit it was built on, and on that
        // one only: the amended commit is not in its history.
        let new_upper = bare_branch(bare.path(), "upper").unwrap();
        assert_ne!(new_upper, old_upper);
        assert_eq!(
            backend.branches_resting_on(root, std::slice::from_ref(&old_upper)).unwrap(),
            ["theirs@origin"]
        );
        assert!(backend
            .branches_resting_on(root, std::slice::from_ref(&new_upper))
            .unwrap()
            .is_empty());

        // `lower` lands and what is left moves onto trunk. Nothing here
        // pushes to their branch: it stays where its author left it, the
        // change still has two commits here, and ending that is still
        // theirs to do (by rebasing their branch, or deleting it).
        bare_squash(bare.path(), &lower_commit, "lower (#1)");
        backend.git_fetch(root, None).unwrap();
        let landed = std::slice::from_ref(&lower_commit);
        backend.rebase_onto_trunk(root, "origin", "main", &node.id, landed).unwrap();
        assert_eq!(bare_branch(bare.path(), "theirs").unwrap(), theirs);
        let snapshot = backend.open(root).unwrap();
        let node = amended(&snapshot);
        assert!(node.is_divergent && node.old_copies_held && !node.diverged_here());
        assert_eq!(node.kind, NodeKind::Mutable);
    }

    /// A commit can be immutable twice over: someone else's branch rests
    /// on it, and it carries a tag. Leaving their branch out of the
    /// reckoning must leave the tag standing: the change is settled, not
    /// held, and a landing rewrites nothing under it. What is immutable
    /// on account of their branch alone, above the tag, is still held.
    /// (Found in review: a tagged change with someone's branch at the
    /// same commit read as held, and the landing's rebase rewrote it.)
    #[test]
    fn a_tag_keeps_a_change_settled_whoever_else_rests_on_it() {
        let dir = tempfile::tempdir().unwrap();
        let bare = tempfile::tempdir().unwrap();
        let backend = test_backend();
        let root = dir.path();
        let (lower, upper) = pushed_pair(root, bare.path(), &backend);
        let (top, _) = record_change(root, &backend, &upper, "top", &[("top.txt", "t\n")]);
        backend.create_bookmark(root, "top", &top).unwrap();
        backend.push_bookmarks(root, &["top".into()], None).unwrap();
        let lower_commit = bare_branch(bare.path(), "lower").unwrap();
        let upper_commit = bare_branch(bare.path(), "upper").unwrap();
        let top_commit = bare_branch(bare.path(), "top").unwrap();
        // On the remote: a tag on `upper`, someone's branch at that very
        // commit, and another at `top`'s.
        git(bare.path(), &["tag", "v1", &upper_commit]);
        git(bare.path(), &["update-ref", "refs/heads/copy-of-upper", &upper_commit]);
        git(bare.path(), &["update-ref", "refs/heads/copy-of-top", &top_commit]);
        backend.git_fetch(root, None).unwrap();

        let snapshot = backend.open(root).unwrap();
        let node = |id: &str| snapshot.nodes.iter().find(|n| n.id == id).cloned();
        let team = backend.open_with(root, &SnapshotView::Team).unwrap();
        for id in [&lower, &upper] {
            // Settled history is not drawn with the user's own changes.
            let found = node(id).or_else(|| team.nodes.iter().find(|n| n.id == *id).cloned());
            let found = found.expect("the tagged change is in view somewhere");
            assert_eq!(found.kind, NodeKind::Immutable, "{id}");
            assert!(!found.held && found.is_settled(), "{id}");
        }
        let top_node = node(&top).expect("the change above the tag is still the user's");
        assert!(top_node.held && !top_node.is_settled());

        // The landing's two mutations refuse the tagged changes.
        assert!(matches!(
            backend
                .rebase_onto_trunk(root, "origin", "main", &upper, std::slice::from_ref(&lower_commit))
                .unwrap_err(),
            BackendError::ImmutableChange(_)
        ));
        assert!(matches!(
            backend.abandon_landed(root, std::slice::from_ref(&upper_commit)).unwrap_err(),
            BackendError::ImmutableChange(_)
        ));
        // Nothing was rewritten: the tag and the branch are where they were.
        assert_eq!(bare_branch(bare.path(), "upper").unwrap(), upper_commit);
        let after = backend.open_with(root, &SnapshotView::Team).unwrap();
        let upper_after = after.nodes.iter().find(|n| n.id == upper).unwrap();
        assert!(upper_commit.starts_with(&upper_after.commit_id));
    }

    /// The same for whatever the user's own `immutable_heads()` names: a
    /// change it makes immutable stays settled when someone else's branch
    /// also rests on it. And a configuration that names every remote
    /// branch some other way than jj's `untracked_remote_bookmarks()`
    /// cannot be taken apart: under it nothing is held.
    #[test]
    fn what_the_users_immutable_heads_names_for_its_own_reasons_stays_settled() {
        let dir = tempfile::tempdir().unwrap();
        let bare = tempfile::tempdir().unwrap();
        let backend = test_backend();
        let root = dir.path();
        let (lower, upper) = pushed_pair(root, bare.path(), &backend);
        let upper_commit = bare_branch(bare.path(), "upper").unwrap();
        bare_commit(bare.path(), "theirs", &upper_commit, "theirs: on top");
        backend.git_fetch(root, None).unwrap();
        let held = |id: &str| {
            let snapshot = backend.open_with(root, &SnapshotView::Team).unwrap();
            snapshot.nodes.iter().find(|n| n.id == id).map(|n| n.held)
        };
        // jj's default: both are held by their branch.
        assert_eq!((held(&lower), held(&upper)), (Some(true), Some(true)));

        // The user's configuration protects `lower` as well.
        write_repo_config(
            root,
            "[revset-aliases]\n'immutable_heads()' = \
             'builtin_immutable_heads() | bookmarks(exact:\"lower\")'\n",
        );
        assert_eq!((held(&lower), held(&upper)), (Some(false), Some(true)));

        // Every remote branch, by another name: their branch cannot be
        // told from the user's reasons, and stays one of them.
        write_repo_config(
            root,
            "[revset-aliases]\n'immutable_heads()' = \
             'present(trunk()) | tags() | remote_bookmarks()'\n",
        );
        assert_eq!((held(&lower), held(&upper)), (Some(false), Some(false)));
    }

    /// A configuration can call jj's `untracked_remote_bookmarks` with an
    /// argument: a remote, a name pattern. An alias cannot stand in for a
    /// call with arguments, so their branches cannot be set aside in such
    /// an `immutable_heads()`. Then nothing is taken apart: everything it
    /// names is settled, nothing is held, and the repository opens as it
    /// does for jj. (Found in review: once someone's branch had been
    /// fetched onto a stack, every snapshot failed with a configuration
    /// error.)
    #[test]
    fn untracked_remote_bookmarks_called_with_an_argument_leaves_everything_settled() {
        let dir = tempfile::tempdir().unwrap();
        let bare = tempfile::tempdir().unwrap();
        let backend = test_backend();
        let root = dir.path();
        let (lower, upper) = pushed_pair(root, bare.path(), &backend);
        let lower_commit = bare_branch(bare.path(), "lower").unwrap();
        let upper_commit = bare_branch(bare.path(), "upper").unwrap();
        bare_commit(bare.path(), "theirs", &upper_commit, "theirs: on top");
        backend.git_fetch(root, None).unwrap();

        for call in [
            "untracked_remote_bookmarks(remote=\"origin\")",
            "untracked_remote_bookmarks(\"theirs\")",
        ] {
            write_repo_config(
                root,
                &format!(
                    "[revset-aliases]\n'immutable_heads()' = 'present(trunk()) | tags() | {call}'\n"
                ),
            );
            // Both views open, and the stack under their branch reads as
            // it does for jj: immutable, and not held.
            backend.open(root).unwrap_or_else(|err| panic!("{call}: {err}"));
            let team = backend
                .open_with(root, &SnapshotView::Team)
                .unwrap_or_else(|err| panic!("{call}: {err}"));
            for id in [&lower, &upper] {
                let node = team.nodes.iter().find(|n| n.id == *id).expect("in view");
                assert_eq!(node.kind, NodeKind::Immutable, "{call}: {id}");
                assert!(!node.held && node.is_settled(), "{call}: {id}");
            }
            // The landing's two mutations rewrite nothing under it.
            assert!(
                matches!(
                    backend
                        .rebase_onto_trunk(
                            root,
                            "origin",
                            "main",
                            &upper,
                            std::slice::from_ref(&lower_commit)
                        )
                        .unwrap_err(),
                    BackendError::ImmutableChange(_)
                ),
                "{call}"
            );
            assert!(
                matches!(
                    backend.abandon_landed(root, std::slice::from_ref(&upper_commit)).unwrap_err(),
                    BackendError::ImmutableChange(_)
                ),
                "{call}"
            );
        }
    }

    /// `untracked_remote_bookmarks()` can be an alias of the user's own,
    /// standing in for jj's function. What it names is then the user's
    /// reason like any other, and it is not set aside: a change it makes
    /// immutable is settled, with or without someone's branch on it, and
    /// a landing rewrites nothing under it. (Found in review: the user's
    /// helper was replaced whole, the change read as held, and the
    /// landing's rebase was let through.)
    #[test]
    fn a_helper_of_the_users_own_named_untracked_remote_bookmarks_is_not_set_aside() {
        let dir = tempfile::tempdir().unwrap();
        let bare = tempfile::tempdir().unwrap();
        let backend = test_backend();
        let root = dir.path();
        let (lower, upper) = pushed_pair(root, bare.path(), &backend);
        let lower_commit = bare_branch(bare.path(), "lower").unwrap();
        let upper_commit = bare_branch(bare.path(), "upper").unwrap();
        write_repo_config(
            root,
            "[revset-aliases]\n'untracked_remote_bookmarks()' = 'bookmarks(exact:\"upper\")'\n",
        );
        let settled = |when: &str| {
            let team = backend.open_with(root, &SnapshotView::Team).unwrap();
            for id in [&lower, &upper] {
                let node = team.nodes.iter().find(|n| n.id == *id).expect("in view");
                assert_eq!(node.kind, NodeKind::Immutable, "{when}: {id}");
                assert!(!node.held && node.is_settled(), "{when}: {id}");
            }
        };
        settled("before anyone else's branch");
        bare_commit(bare.path(), "theirs", &upper_commit, "theirs: on top");
        backend.git_fetch(root, None).unwrap();
        settled("with their branch on it");

        assert!(matches!(
            backend
                .rebase_onto_trunk(root, "origin", "main", &upper, std::slice::from_ref(&lower_commit))
                .unwrap_err(),
            BackendError::ImmutableChange(_)
        ));
        assert!(matches!(
            backend.abandon_landed(root, std::slice::from_ref(&upper_commit)).unwrap_err(),
            BackendError::ImmutableChange(_)
        ));
    }

    /// Where nothing is taken apart, settled is what jj calls immutable
    /// and no more. Here the user's own helper names nothing, so their
    /// commit, which the user built on and bookmarked above, is mutable
    /// for jj. The landing's rebase then moves the stack as jj would, their
    /// commit with the work that sits on it, and does not stop half-way
    /// over a commit that only jj's default would have protected.
    #[test]
    fn under_a_helper_of_the_users_own_nothing_is_refused_that_jj_allows() {
        let dir = tempfile::tempdir().unwrap();
        let bare = tempfile::tempdir().unwrap();
        let backend = test_backend();
        let root = dir.path();
        let (_lower, upper) = pushed_pair(root, bare.path(), &backend);
        let lower_commit = bare_branch(bare.path(), "lower").unwrap();
        let upper_commit = bare_branch(bare.path(), "upper").unwrap();
        bare_commit(bare.path(), "theirs", &upper_commit, "theirs: on top");
        write_repo_config(
            root,
            "[revset-aliases]\n'untracked_remote_bookmarks()' = 'none()'\n",
        );
        backend.git_fetch(root, None).unwrap();
        let team = backend.open_with(root, &SnapshotView::Team).unwrap();
        let theirs = team.nodes.iter().find(|n| n.description == "theirs: on top").unwrap().id.clone();
        let (mine, _) = record_change(root, &backend, &theirs, "mine on theirs", &[("m.txt", "m\n")]);
        backend.create_bookmark(root, "mine", &mine).unwrap();
        bare_squash(bare.path(), &lower_commit, "lower (#1)");
        backend.git_fetch(root, None).unwrap();

        backend
            .rebase_onto_trunk(root, "origin", "main", &upper, std::slice::from_ref(&lower_commit))
            .unwrap();
        let snapshot = backend.open(root).unwrap();
        let mine_node = snapshot.nodes.iter().find(|n| n.id == mine).unwrap();
        let below = snapshot.nodes.iter().find(|n| n.id == mine_node.parents[0]).unwrap();
        assert_eq!(below.description, "theirs: on top");
        assert_eq!(below.parents.as_slice(), std::slice::from_ref(&upper));
    }

    /// Whose commits are someone else's is read from the repository, not
    /// through a helper the user may have redefined. Here the user's own
    /// `untracked_remote_bookmarks()` names nothing, so their branch makes
    /// nothing immutable and the stack lands as plain mutable work. Their
    /// commit is still theirs: the landing's rebase does not rewrite it.
    #[test]
    fn someone_elses_commits_are_told_by_the_repository_not_by_the_users_helper() {
        let dir = tempfile::tempdir().unwrap();
        let bare = tempfile::tempdir().unwrap();
        let backend = test_backend();
        let root = dir.path();
        let (_lower, upper) = pushed_pair(root, bare.path(), &backend);
        let lower_commit = bare_branch(bare.path(), "lower").unwrap();
        let upper_commit = bare_branch(bare.path(), "upper").unwrap();
        let theirs = bare_commit(bare.path(), "theirs", &upper_commit, "theirs: on top");
        bare_squash(bare.path(), &lower_commit, "lower (#1)");
        write_repo_config(
            root,
            "[revset-aliases]\n'untracked_remote_bookmarks()' = 'none()'\n",
        );
        backend.git_fetch(root, None).unwrap();
        let node = backend.open(root).unwrap().nodes.into_iter().find(|n| n.id == upper).unwrap();
        assert_eq!(node.kind, NodeKind::Mutable);

        let outcome = backend
            .rebase_onto_trunk(root, "origin", "main", &upper, std::slice::from_ref(&lower_commit))
            .unwrap();
        assert!(
            outcome.summary.ends_with(
                "; theirs@origin still builds on the commits this replaced and was left as it is"
            ),
            "{}",
            outcome.summary
        );
        assert_eq!(git(root, &["rev-parse", "refs/remotes/origin/theirs"]), theirs);
        let all = backend.open_with(root, &SnapshotView::All).unwrap();
        assert!(
            !all.nodes.iter().any(|n| n.description == "theirs: on top"),
            "their commit is neither rewritten nor shown"
        );
    }

    #[test]
    fn team_view_draws_untracked_remote_branches() {
        let dir = tempfile::tempdir().unwrap();
        let bare = tempfile::tempdir().unwrap();
        let backend = test_backend();
        build_colocated_repo(dir.path(), &backend);
        add_bare_origin(dir.path(), bare.path());

        // Publish trunk, then a teammate grows a branch on it out-of-band.
        // The fetch imports it as an untracked remote bookmark.
        backend
            .push_bookmarks(dir.path(), &["main".into()], None)
            .unwrap();
        let main_commit = bare_branch(bare.path(), "main").unwrap();
        bare_commit(bare.path(), "teammate-topic", &main_commit, "teammate: new API");
        backend.git_fetch(dir.path(), None).unwrap();

        // The default view hides it: untracked remote bookmarks are
        // immutable, off every workstream.
        let default = backend.open(dir.path()).unwrap();
        assert!(
            !default.nodes.iter().any(|n| n.description == "teammate: new API"),
            "default view stays workstreams-only"
        );

        // The Team lens draws it as the immutable context it is, and the
        // default set still rides along.
        let team = backend.open_with(dir.path(), &SnapshotView::Team).unwrap();
        assert_eq!(team.view, SnapshotView::Team);
        let teammate = team
            .nodes
            .iter()
            .find(|n| n.description == "teammate: new API")
            .expect("teammate branch drawn");
        assert_eq!(teammate.kind, NodeKind::Immutable);
        assert!(
            team.nodes.iter().any(|n| n.id == team.working_copy),
            "own work still drawn"
        );

        // Mine draws only this user's commits — the teammate's stays out.
        let mine = backend.open_with(dir.path(), &SnapshotView::Mine).unwrap();
        assert!(
            !mine.nodes.iter().any(|n| n.description == "teammate: new API"),
            "mine() excludes the teammate's work"
        );
        assert!(
            mine.nodes.iter().any(|n| n.description == "first change"),
            "mine() includes own landed work"
        );
    }

    fn assert_unique_node_ids(snapshot: &RepoSnapshot) {
        let mut seen = HashSet::new();
        for node in &snapshot.nodes {
            assert!(seen.insert(node.id.as_str()), "node id {} drawn twice", node.id);
        }
    }

    /// The commit a node id resolves to, as every backend request sees it.
    fn resolved_commit_id(root: &Path, node_id: &str) -> String {
        let (_, repo) = test_backend().load_repo_at_head(root).unwrap();
        resolve_change_commit(&repo, node_id).unwrap().id().hex()
    }

    /// Amending a pushed branch leaves its remote bookmark on the hidden
    /// pre-rewrite commit, and the Team lens draws both under one change
    /// id. The hidden one keys by commit id, as the CLI addresses it: node
    /// ids stay unique (the graph's keyed rows refuse duplicates, which
    /// blanked the workbench) and each id reads back its own commit.
    #[test]
    fn hidden_remote_targets_key_by_commit_id() {
        let dir = tempfile::tempdir().unwrap();
        let bare = tempfile::tempdir().unwrap();
        let backend = test_backend();
        build_colocated_repo(dir.path(), &backend);
        add_bare_origin(dir.path(), bare.path());
        std::fs::write(dir.path().join("feature.txt"), "one\n").unwrap();
        let feature = backend.refresh(dir.path()).unwrap().working_copy;
        backend.describe(dir.path(), &feature, "feature: first cut").unwrap();
        backend.create_bookmark(dir.path(), "feature", &feature).unwrap();
        backend.new_change(dir.path(), &feature).unwrap();
        backend
            .push_bookmarks(dir.path(), &["main".into(), "feature".into()], None)
            .unwrap();
        backend.describe(dir.path(), &feature, "feature: amended").unwrap();

        let team = backend.open_with(dir.path(), &SnapshotView::Team).unwrap();
        assert_unique_node_ids(&team);
        let amended = node_by_description(&team, "feature: amended");
        let pushed = node_by_description(&team, "feature: first cut");
        assert_eq!(amended.id, feature, "the visible commit keeps its change id");
        assert_eq!(pushed.change_id, feature);
        assert_eq!(pushed.id, pushed.commit_id, "the hidden commit keys by commit id");
        assert!(!pushed.is_divergent, "a hidden copy is not divergence");
        for node in [amended, pushed] {
            assert!(resolved_commit_id(dir.path(), &node.id).starts_with(&node.commit_id));
        }
    }

    /// A trunk commit rewritten locally (how a reconciled concurrent
    /// operation once rebased trunk) leaves a `trunk()` pinned to the
    /// remote on the hidden original while the rewrite is drawn as mutable
    /// work. The default view draws both; they must not collide.
    #[test]
    fn hidden_trunk_target_keys_by_commit_id() {
        let dir = tempfile::tempdir().unwrap();
        let bare = tempfile::tempdir().unwrap();
        let backend = test_backend();
        let first = build_colocated_repo(dir.path(), &backend);
        write_repo_config(
            dir.path(),
            "user.name = \"Test User\"\nuser.email = \"test@example.com\"\n\
             [revset-aliases]\n'trunk()' = 'main@origin'\n",
        );
        add_bare_origin(dir.path(), bare.path());
        backend
            .push_bookmarks(dir.path(), &["main".into()], None)
            .unwrap();
        {
            let (_write, _workspace, repo) = backend.load_repo_for_write(dir.path()).unwrap();
            let commit = resolve_change_commit(&repo, &first).unwrap();
            let mut tx = repo.start_transaction();
            pollster::block_on(
                tx.repo_mut()
                    .rewrite_commit(&commit)
                    .set_description("first change (rewritten)")
                    .write(),
            )
            .unwrap();
            pollster::block_on(tx.repo_mut().rebase_descendants()).unwrap();
            pollster::block_on(tx.commit("rewrite trunk underneath its remote")).unwrap();
        }

        let snapshot = backend.open(dir.path()).unwrap();
        assert_unique_node_ids(&snapshot);
        let rewritten = node_by_description(&snapshot, "first change (rewritten)");
        let remote = node_by_description(&snapshot, "first change");
        assert_eq!(rewritten.id, first);
        assert_eq!(rewritten.kind, NodeKind::Mutable);
        assert_eq!(remote.id, remote.commit_id);
        assert_eq!(remote.kind, NodeKind::Immutable);
        assert!(resolved_commit_id(dir.path(), &remote.id).starts_with(&remote.commit_id));
    }

    #[test]
    fn bookmarks_report_remote_only_tracked_and_merged() {
        let dir = tempfile::tempdir().unwrap();
        let bare = tempfile::tempdir().unwrap();
        let backend = test_backend();
        let first = build_colocated_repo(dir.path(), &backend);
        add_bare_origin(dir.path(), bare.path());
        backend
            .push_bookmarks(dir.path(), &["main".into()], None)
            .unwrap();

        // A local bookmark pointing at trunk itself: its work has landed,
        // so it reads merged — cleanup, not loss. Trunk never does.
        backend.create_bookmark(dir.path(), "landed", &first).unwrap();

        // A branch pushed by plain git that jj never tracked: the refresh
        // imports it as a remote-only, untracked entry.
        let first_commit = {
            let snapshot = backend.open(dir.path()).unwrap();
            snapshot.nodes.iter().find(|n| n.id == first).unwrap().commit_id.clone()
        };
        let full_first = git(dir.path(), &["rev-parse", &first_commit]);
        git(
            dir.path(),
            &["push", "--quiet", "origin", &format!("{full_first}:refs/heads/alice-topic")],
        );
        let snapshot = backend.refresh(dir.path()).unwrap();

        let main = snapshot.bookmarks.iter().find(|b| b.name == "main").unwrap();
        assert!(main.is_local && main.tracked && main.is_trunk);
        assert!(!main.is_merged, "trunk itself never reads merged");
        assert_eq!(main.target_description, "first change");
        assert_eq!(main.target_author, "Test User");
        assert!(!main.target_timestamp.is_empty());

        let landed = snapshot.bookmarks.iter().find(|b| b.name == "landed").unwrap();
        assert!(landed.is_local && !landed.tracked);
        assert_eq!(landed.sync, SyncState::LocalOnly);
        assert!(landed.is_merged, "a bookmark on trunk has landed");

        let alice = snapshot
            .bookmarks
            .iter()
            .find(|b| b.name == "alice-topic")
            .expect("remote-only bookmark reported");
        assert!(!alice.is_local && !alice.tracked);
        assert_eq!(alice.remote.as_deref(), Some("origin"));
        assert_eq!(alice.sync, SyncState::RemoteOnly);
        assert!(alice.is_merged, "it points at trunk's commit");
        assert_eq!(alice.target_description, "first change");

        // The working copy's undescribed draft has no bookmark; nothing
        // else invented entries.
        assert_eq!(snapshot.bookmarks.len(), 3);
    }

    #[test]
    fn track_untrack_and_delete_remote_bookmarks() {
        let dir = tempfile::tempdir().unwrap();
        let bare = tempfile::tempdir().unwrap();
        let backend = test_backend();
        let first = build_colocated_repo(dir.path(), &backend);
        add_bare_origin(dir.path(), bare.path());
        backend
            .push_bookmarks(dir.path(), &["main".into()], None)
            .unwrap();

        // A teammate's branch arrives untracked via plain git + refresh.
        let first_commit = {
            let snapshot = backend.open(dir.path()).unwrap();
            snapshot.nodes.iter().find(|n| n.id == first).unwrap().commit_id.clone()
        };
        let full_first = git(dir.path(), &["rev-parse", &first_commit]);
        for branch in ["shared", "bob-old"] {
            git(
                dir.path(),
                &["push", "--quiet", "origin", &format!("{full_first}:refs/heads/{branch}")],
            );
        }
        backend.refresh(dir.path()).unwrap();

        // Track: the entry becomes an ordinary tracked local bookmark.
        let outcome = backend.track_bookmark(dir.path(), "shared", "origin").unwrap();
        assert!(outcome.operation_id.is_some());
        assert_eq!(outcome.summary, "Tracking shared@origin");
        let snapshot = backend.open(dir.path()).unwrap();
        assert_eq!(
            snapshot.operations[0].description,
            "track remote bookmark shared@origin"
        );
        let shared = snapshot.bookmarks.iter().find(|b| b.name == "shared").unwrap();
        assert!(shared.is_local && shared.tracked);
        assert_eq!(shared.sync, SyncState::Synced);

        // Tracking again, or a bookmark that does not exist, refuses.
        let err = backend.track_bookmark(dir.path(), "shared", "origin").unwrap_err();
        assert!(err.to_string().contains("already tracked"), "{err}");
        let err = backend.track_bookmark(dir.path(), "nope", "origin").unwrap_err();
        assert!(err.to_string().contains("no remote bookmark"), "{err}");

        // Untrack: the local bookmark stays, the remote ref reads
        // remote-only again.
        let outcome = backend.untrack_bookmark(dir.path(), "shared", "origin").unwrap();
        assert_eq!(outcome.summary, "Stopped tracking shared@origin");
        let snapshot = backend.open(dir.path()).unwrap();
        let (locals, remotes): (Vec<_>, Vec<_>) = snapshot
            .bookmarks
            .iter()
            .filter(|b| b.name == "shared")
            .partition(|b| b.is_local);
        assert_eq!(locals.len(), 1);
        assert_eq!(locals[0].sync, SyncState::LocalOnly);
        assert!(!locals[0].tracked);
        assert_eq!(remotes.len(), 1);
        assert_eq!(remotes[0].sync, SyncState::RemoteOnly);
        let err = backend.untrack_bookmark(dir.path(), "shared", "origin").unwrap_err();
        assert!(err.to_string().contains("is not tracked"), "{err}");

        // Delete on the remote: works from the untracked state (the CLI
        // dance — track, delete local, push the deletion).
        let outcome = backend
            .delete_remote_bookmark(dir.path(), "shared", "origin")
            .unwrap();
        assert!(outcome.operation_id.is_some());
        assert_eq!(outcome.summary, "Deleted shared on origin");
        assert!(bare_branch(bare.path(), "shared").is_none(), "remote branch gone");
        let snapshot = backend.open(dir.path()).unwrap();
        assert!(
            !snapshot.bookmarks.iter().any(|b| b.name == "shared"),
            "no local or remote entry survives"
        );

        // And from the pure remote-only state, with no local bookmark ever.
        backend
            .delete_remote_bookmark(dir.path(), "bob-old", "origin")
            .unwrap();
        assert!(bare_branch(bare.path(), "bob-old").is_none());
        let snapshot = backend.open(dir.path()).unwrap();
        assert!(!snapshot.bookmarks.iter().any(|b| b.name == "bob-old"));
        let err = backend
            .delete_remote_bookmark(dir.path(), "bob-old", "origin")
            .unwrap_err();
        assert!(err.to_string().contains("no remote bookmark"), "{err}");
    }

    #[test]
    fn fetch_resolves_remotes_like_the_cli() {
        let dir = tempfile::tempdir().unwrap();
        let bare_upstream = tempfile::tempdir().unwrap();
        let bare_mirror = tempfile::tempdir().unwrap();
        let backend = test_backend();
        build_colocated_repo(dir.path(), &backend);

        // A sole remote is fetched whatever its name (the CLI's
        // fetch-from-the-only-remote rule).
        add_bare_remote(dir.path(), bare_upstream.path(), "upstream");
        backend
            .push_bookmarks(dir.path(), &["main".into()], Some("upstream"))
            .unwrap();
        let seed = bare_branch(bare_upstream.path(), "main").unwrap();
        bare_commit(bare_upstream.path(), "main", &seed, "upstream: one");
        let outcome = backend.git_fetch(dir.path(), None).unwrap();
        assert_eq!(outcome.summary, "Fetched 1 bookmark update from upstream");
        let snapshot = backend.open(dir.path()).unwrap();
        assert_eq!(
            snapshot.operations[0].description,
            "fetch from git remote(s) upstream"
        );

        // With several remotes and no `git.fetch`, the CLI falls back to
        // the literal name "origin" — which does not exist here, so there
        // is nothing to fetch from.
        add_bare_remote(dir.path(), bare_mirror.path(), "mirror");
        git(dir.path(), &["push", "--quiet", "mirror", "refs/heads/main:refs/heads/main"]);
        backend.refresh(dir.path()).unwrap();
        let err = backend.git_fetch(dir.path(), None).unwrap_err();
        assert!(err.to_string().contains("no git remotes to fetch from"), "{err}");

        // The `git.fetch` setting picks the defaults instead.
        write_repo_config(
            dir.path(),
            "user.name = \"Test User\"\nuser.email = \"test@example.com\"\n\
             git.fetch = [\"mirror\"]\n",
        );
        let mirror_main = bare_branch(bare_mirror.path(), "main").unwrap();
        bare_commit(bare_mirror.path(), "main", &mirror_main, "mirror: one");
        let outcome = backend.git_fetch(dir.path(), None).unwrap();
        assert!(outcome.operation_id.is_some());
        let snapshot = backend.open(dir.path()).unwrap();
        assert_eq!(
            snapshot.operations[0].description,
            "fetch from git remote(s) mirror"
        );

        // An explicit multi-remote fetch runs as one operation, with the
        // remotes named in listing order — not request order (pinned
        // against the CLI, which does the same).
        let up = bare_branch(bare_upstream.path(), "main").unwrap();
        bare_commit(bare_upstream.path(), "main", &up, "upstream: two");
        let mi = bare_branch(bare_mirror.path(), "main").unwrap();
        bare_commit(bare_mirror.path(), "main", &mi, "mirror: two");
        let outcome = backend
            .git_fetch(dir.path(), Some(&["upstream".into(), "mirror".into()]))
            .unwrap();
        assert_eq!(outcome.summary, "Fetched 2 bookmark updates from mirror and upstream");
        let snapshot = backend.open(dir.path()).unwrap();
        assert_eq!(
            snapshot.operations[0].description,
            "fetch from git remote(s) mirror,upstream"
        );

        // An explicitly-named unknown remote refuses (curated deviation —
        // the CLI warns and continues), recording nothing.
        let err = backend
            .git_fetch(dir.path(), Some(&["upstream".into(), "nosuch".into()]))
            .unwrap_err();
        assert!(err.to_string().contains("no git remote named"), "{err}");
        let after = backend.open(dir.path()).unwrap();
        assert_eq!(after.operations[0].id, snapshot.operations[0].id, "refusal recorded nothing");
    }

    /// Writes a commit into a bare repo and publishes it as a PR head ref
    /// (`refs/pull/<N>/head`) with no `refs/heads/*` counterpart — exactly
    /// how GitHub serves a cross-fork PR on the base repository.
    fn bare_pull_ref(bare: &Path, number: u64, parent: &str, message: &str) -> String {
        let tree = git(bare, &["hash-object", "-t", "tree", "-w", "/dev/null"]);
        let commit = git(
            bare,
            &[
                "-c",
                "user.name=Contributor",
                "-c",
                "user.email=contributor@example.com",
                "commit-tree",
                &tree,
                "-p",
                parent,
                "-m",
                message,
            ],
        );
        git(
            bare,
            &["update-ref", &format!("refs/pull/{number}/head"), &commit],
        );
        commit
    }

    #[test]
    fn fetch_pr_head_creates_a_review_bookmark() {
        let dir = tempfile::tempdir().unwrap();
        let bare = tempfile::tempdir().unwrap();
        let backend = test_backend();
        build_colocated_repo(dir.path(), &backend);
        add_bare_origin(dir.path(), bare.path());
        backend
            .push_bookmarks(dir.path(), &["main".into()], None)
            .unwrap();

        // The "PR": a contributor's commit the base repo serves only
        // through refs/pull/7/head — jj alone cannot fetch it
        // (jj-vcs/jj#4388), which is the whole point of this method.
        let main_commit = bare_branch(bare.path(), "main").unwrap();
        let pr_commit = bare_pull_ref(bare.path(), 7, &main_commit, "contributor: fix the thing");

        let outcome = backend
            .fetch_pr_head(dir.path(), "origin", 7, "pr/7")
            .unwrap();
        assert!(outcome.operation_id.is_some());
        assert_eq!(outcome.summary, "Fetched PR #7 into pr/7");
        let target = outcome.target_change.expect("selection follows the PR head");

        let snapshot = backend.open(dir.path()).unwrap();
        assert_eq!(
            snapshot.operations[0].description,
            "fetch pull request #7 into bookmark pr/7"
        );
        let bookmark = snapshot
            .bookmarks
            .iter()
            .find(|b| b.name == "pr/7")
            .expect("review bookmark exists");
        assert!(bookmark.is_local);
        assert_eq!(bookmark.sync, SyncState::LocalOnly, "tracks nothing remote");
        assert_eq!(bookmark.target, target);
        let node = snapshot
            .nodes
            .iter()
            .find(|n| n.id == target)
            .expect("the PR head is drawn");
        assert_eq!(node.description, "contributor: fix the thing");
        assert!(pr_commit.starts_with(&node.commit_id));

        // A taken bookmark name refuses before anything runs.
        let err = backend
            .fetch_pr_head(dir.path(), "origin", 7, "pr/7")
            .unwrap_err();
        assert!(err.to_string().contains("already exists"), "{err}");

        // An unknown remote is a stale plan, refused like git_fetch's.
        let err = backend
            .fetch_pr_head(dir.path(), "nosuch", 7, "pr/7-again")
            .unwrap_err();
        assert!(err.to_string().contains("no git remote named"), "{err}");

        // A PR the remote does not serve gets a plain story, and the
        // refusal records nothing.
        let before = backend.open(dir.path()).unwrap();
        let err = backend
            .fetch_pr_head(dir.path(), "origin", 99, "pr/99")
            .unwrap_err();
        assert!(err.to_string().contains("no pull request #99"), "{err}");
        let after = backend.open(dir.path()).unwrap();
        assert_eq!(after.operations[0].id, before.operations[0].id);
    }

    #[test]
    fn trunk_text_file_reads_docs_from_the_trunk_tree() {
        let dir = tempfile::tempdir().unwrap();
        let backend = test_backend();
        build_colocated_repo(dir.path(), &backend);

        // Put a PR template (and a root-level decoy) on the change trunk
        // points at: write the files, snapshot them into the working copy,
        // and move main there.
        std::fs::create_dir_all(dir.path().join(".github")).unwrap();
        std::fs::write(
            dir.path().join(".github/PULL_REQUEST_TEMPLATE.md"),
            "## Checklist\n- [ ] tests\n",
        )
        .unwrap();
        std::fs::write(dir.path().join("PULL_REQUEST_TEMPLATE.md"), "root decoy\n").unwrap();
        let wc = backend.refresh(dir.path()).unwrap().working_copy;
        backend
            .move_bookmark(dir.path(), "main", &wc)
            .unwrap();

        // The first candidate that exists wins — candidate order is the
        // caller's precedence.
        let found = backend
            .trunk_text_file(
                dir.path(),
                &[
                    ".github/PULL_REQUEST_TEMPLATE.md".to_owned(),
                    "PULL_REQUEST_TEMPLATE.md".to_owned(),
                ],
            )
            .unwrap()
            .expect("template found on trunk");
        assert_eq!(found.0, ".github/PULL_REQUEST_TEMPLATE.md");
        assert_eq!(found.1, "## Checklist\n- [ ] tests\n");

        // Missing candidates skip to the next; all-missing answers None.
        let found = backend
            .trunk_text_file(
                dir.path(),
                &[
                    "docs/PULL_REQUEST_TEMPLATE.md".to_owned(),
                    "PULL_REQUEST_TEMPLATE.md".to_owned(),
                ],
            )
            .unwrap()
            .expect("the root file answers when .github is not asked for");
        assert_eq!(found.0, "PULL_REQUEST_TEMPLATE.md");
        assert_eq!(
            backend
                .trunk_text_file(dir.path(), &["nope.md".to_owned()])
                .unwrap(),
            None
        );
    }

    /// Records one described change on top of `parent` that writes
    /// `files`, and answers its node id and commit id. The working copy
    /// ends up on a fresh change above it.
    fn record_change(
        root: &Path,
        backend: &JjBackend,
        parent: &str,
        description: &str,
        files: &[(&str, &str)],
    ) -> (String, String) {
        backend.new_change(root, parent).unwrap();
        for (name, text) in files {
            std::fs::write(root.join(name), text).unwrap();
        }
        let id = backend.refresh(root).unwrap().working_copy;
        backend.describe(root, &id, description).unwrap();
        backend.new_change(root, &id).unwrap();
        let snapshot = backend.open(root).unwrap();
        let commit = snapshot.nodes.iter().find(|n| n.id == id).unwrap().commit_id.clone();
        (id, commit)
    }

    #[test]
    fn landed_check_asks_whether_a_trunk_commit_holds_the_changes() {
        let dir = tempfile::tempdir().unwrap();
        let backend = test_backend();
        let root = dir.path();
        let first = build_colocated_repo(root, &backend);

        // A stack of two on trunk: `a` rewrites file.txt's only line and
        // adds a.txt, `b` rewrites the same line again and adds b.txt.
        let (a, a_commit) =
            record_change(root, &backend, &first, "a", &[("file.txt", "two\n"), ("a.txt", "a\n")]);
        let (b, b_commit) =
            record_change(root, &backend, &a, "b", &[("file.txt", "three\n"), ("b.txt", "b\n")]);

        // Nothing has landed: a commit that is not on trunk is elsewhere,
        // one this repository has never seen is unfetched.
        assert_eq!(
            backend.landed_check(root, "origin", "main", &a_commit, &a_commit, &a, &a).unwrap(),
            LandedCheck::Elsewhere
        );
        assert_eq!(
            backend
                .landed_check(root, "origin", "main", "0123456789abcdef0123456789abcdef01234567", &a_commit, &a, &a)
                .unwrap(),
            LandedCheck::Unfetched
        );

        // Trunk gains someone else's commit, then a commit that carries
        // only half of `a`: the line, without a.txt. The loss is named.
        let (other, _) = record_change(root, &backend, &first, "other", &[("other.txt", "o\n")]);
        let (lossy, lossy_commit) =
            record_change(root, &backend, &other, "a (#1)", &[("file.txt", "two\n")]);
        backend.move_bookmark(root, "main", &lossy).unwrap();
        assert_eq!(
            backend.landed_check(root, "origin", "main", &lossy_commit, &a_commit, &a, &a).unwrap(),
            LandedCheck::Missing { paths: vec!["a.txt".to_owned()], more: 0 }
        );

        // A squash of all of `a`, as GitHub makes one: a new commit with
        // `a`'s content on a trunk that moved underneath it. It holds `a`
        // and does not hold `b`: both of b's paths would still change.
        let (squash, squash_commit) =
            record_change(root, &backend, &lossy, "a (#2)", &[("a.txt", "a\n")]);
        backend.move_bookmark(root, "main", &squash).unwrap();
        assert_eq!(
            backend.landed_check(root, "origin", "main", &squash_commit, &a_commit, &a, &a).unwrap(),
            LandedCheck::Landed
        );
        let b_missing = LandedCheck::Missing {
            paths: vec!["b.txt".to_owned(), "file.txt".to_owned()],
            more: 0,
        };
        assert_eq!(
            backend.landed_check(root, "origin", "main", &squash_commit, &b_commit, &b, &b).unwrap(),
            b_missing
        );
        // The run `a..=b` as one question: `a` is there, `b` is not.
        assert_eq!(
            backend.landed_check(root, "origin", "main", &squash_commit, &b_commit, &a, &b).unwrap(),
            b_missing
        );

        // A later trunk commit that rewrites the same line does not turn
        // the landing into a missing one: the question is about the
        // landing commit, not about trunk's tip.
        let (later, _) =
            record_change(root, &backend, &squash, "later", &[("file.txt", "rewritten\n")]);
        backend.move_bookmark(root, "main", &later).unwrap();
        assert_eq!(
            backend.landed_check(root, "origin", "main", &squash_commit, &a_commit, &a, &a).unwrap(),
            LandedCheck::Landed
        );
        // Asked about the earlier, lossy commit: it does not hold `a`,
        // and trunk's tip no longer does either (the line was rewritten),
        // so the loss stands.
        assert_eq!(
            backend.landed_check(root, "origin", "main", &lossy_commit, &a_commit, &a, &a).unwrap(),
            LandedCheck::Missing { paths: vec!["a.txt".to_owned()], more: 0 }
        );
    }

    #[test]
    fn landed_check_finds_changes_that_reached_trunk_another_way() {
        let dir = tempfile::tempdir().unwrap();
        let backend = test_backend();
        let root = dir.path();
        let first = build_colocated_repo(root, &backend);
        // `b` sits on `a`. Its pull request merged into a's branch, as the
        // commit `into_a` (never on trunk); then a's branch landed whole,
        // as one squash holding both.
        let (a, _) = record_change(root, &backend, &first, "a", &[("a.txt", "a\n")]);
        let (b, b_commit) = record_change(root, &backend, &a, "b", &[("b.txt", "b\n")]);
        let (_, into_a) = record_change(root, &backend, &a, "b (#2)", &[("b.txt", "b\n")]);

        // Before anything landed, b's merge is elsewhere and that is all.
        assert_eq!(
            backend.landed_check(root, "origin", "main", &into_a, &b_commit, &b, &b).unwrap(),
            LandedCheck::Elsewhere
        );

        let (squash, _) = record_change(
            root,
            &backend,
            &first,
            "a (#1)",
            &[("a.txt", "a\n"), ("b.txt", "b\n")],
        );
        backend.move_bookmark(root, "main", &squash).unwrap();
        // The commit GitHub names for b's merge is still not on trunk, but
        // trunk holds b's changes: they arrived inside a's squash.
        assert_eq!(
            backend.landed_check(root, "origin", "main", &into_a, &b_commit, &b, &b).unwrap(),
            LandedCheck::Held
        );
    }

    /// A segment whose bottom change is a merge: the branch it merges in
    /// is part of what its pull request carries, so the diff is measured
    /// from the commit below it in the stack (its first parent), never
    /// from all its parents merged.
    #[test]
    fn landed_check_measures_a_merge_from_the_commit_below_it() {
        let dir = tempfile::tempdir().unwrap();
        let backend = test_backend();
        let root = dir.path();
        let first = build_colocated_repo(root, &backend);
        // `topic` merges a side branch (which adds side.txt) into a branch
        // at trunk. Its pull request was "merged" at its own head, into
        // some integration branch: never on trunk.
        let (topic, landing) = {
            let (_write, _workspace, repo) = backend.load_repo_for_write(root).unwrap();
            let base = resolve_change_commit(&repo, &first).unwrap();
            let store = repo.store().clone();
            let with_side = file_tree(&store, &[("file.txt", "one\n"), ("side.txt", "side\n")]);
            let mut tx = repo.start_transaction();
            let side =
                write_commit_with_tree(&mut tx, vec![base.id().clone()], "side", with_side.clone());
            let topic = write_commit_with_tree(
                &mut tx,
                vec![base.id().clone(), side.id().clone()],
                "topic: merge side",
                with_side.clone(),
            );
            // The same content landed properly, as a squash on trunk,
            // kept aside until the bookmark moves to it.
            let landing = write_commit_with_tree(
                &mut tx,
                vec![base.id().clone()],
                "topic (#1)",
                with_side,
            );
            pollster::block_on(tx.commit("a merge and its squash")).unwrap();
            (topic.id().hex(), landing.id().hex())
        };

        // Trunk has no side.txt, and the pull request's merge is off
        // trunk: the changes have not landed.
        assert_eq!(
            backend.landed_check(root, "origin", "main", &topic, &topic, &topic, &topic).unwrap(),
            LandedCheck::Elsewhere
        );
        // Asked about a trunk commit that lacks the file, the loss is named.
        let first_commit = {
            let snapshot = backend.open(root).unwrap();
            snapshot.nodes.iter().find(|n| n.id == first).unwrap().commit_id.clone()
        };
        assert_eq!(
            backend.landed_check(root, "origin", "main", &first_commit, &topic, &topic, &topic).unwrap(),
            LandedCheck::Missing { paths: vec!["side.txt".to_owned()], more: 0 }
        );
        // Once the squash is trunk, it holds the merge's content.
        backend.move_bookmark(root, "main", &landing).unwrap();
        assert_eq!(
            backend.landed_check(root, "origin", "main", &landing, &topic, &topic, &topic).unwrap(),
            LandedCheck::Landed
        );
    }

    /// A branch name can outlive its pull request. The earlier pull
    /// request's merge vouches for the commits it was merged from and for
    /// nothing else: new work under the same name is judged by trunk's
    /// tip, even when the old merge happens to contain the same patch
    /// (it was reverted since).
    #[test]
    fn landed_check_asks_trunks_tip_when_the_pull_request_had_another_head() {
        let dir = tempfile::tempdir().unwrap();
        let backend = test_backend();
        let root = dir.path();
        let first = build_colocated_repo(root, &backend);
        // The flag goes on in a pull request that lands as a squash, and
        // is turned off again on trunk.
        let (old, old_commit) =
            record_change(root, &backend, &first, "flag on", &[("file.txt", "on\n")]);
        let (squash, squash_commit) =
            record_change(root, &backend, &first, "flag on (#1)", &[("file.txt", "on\n")]);
        let (revert, _) =
            record_change(root, &backend, &squash, "revert", &[("file.txt", "one\n")]);
        backend.move_bookmark(root, "main", &revert).unwrap();
        // The pull request's own change did land, revert or no revert.
        assert_eq!(
            backend.landed_check(root, "origin", "main", &squash_commit, &old_commit, &old, &old).unwrap(),
            LandedCheck::Landed
        );

        // A fresh change turns the flag on again, under the old branch
        // name. The old merge holds the very same patch, and it says
        // nothing about this change: trunk's tip has the flag off.
        let (again, again_commit) =
            record_change(root, &backend, &revert, "flag on again", &[("file.txt", "on\n")]);
        assert_eq!(
            backend.landed_check(root, "origin", "main", &squash_commit, &old_commit, &again, &again).unwrap(),
            LandedCheck::Missing { paths: vec!["file.txt".to_owned()], more: 0 }
        );
        // The same holds when the forge names a head this repository has
        // never seen.
        assert_eq!(
            backend
                .landed_check(
                    root, "origin", "main",
                    &squash_commit,
                    "0123456789abcdef0123456789abcdef01234567",
                    &again,
                    &again,
                )
                .unwrap(),
            LandedCheck::Missing { paths: vec!["file.txt".to_owned()], more: 0 }
        );
        // When trunk's tip does hold it, a pull request with another head
        // is no obstacle: a change reworded after it merged still counts.
        let (relanded, _) =
            record_change(root, &backend, &revert, "flag on again (#2)", &[("file.txt", "on\n")]);
        backend.move_bookmark(root, "main", &relanded).unwrap();
        assert_eq!(
            backend.landed_check(root, "origin", "main", &squash_commit, &old_commit, &again, &again).unwrap(),
            LandedCheck::Held
        );
        let _ = again_commit;
    }

    /// Commits that are part of trunk's history are judged where they
    /// entered it. A merge commit that kept their content landed them,
    /// whatever commit the forge names; a merge that took the branch in
    /// and kept none of its content did not (GitHub marks that pull
    /// request merged all the same, and names the merge).
    #[test]
    fn landed_check_asks_the_commit_where_a_branch_entered_trunk() {
        let dir = tempfile::tempdir().unwrap();
        let backend = test_backend();
        let root = dir.path();
        let first = build_colocated_repo(root, &backend);
        let (topic, topic_commit) =
            record_change(root, &backend, &first, "topic", &[("topic.txt", "t\n")]);
        let (other, _) = record_change(root, &backend, &first, "other", &[("other.txt", "o\n")]);
        // Two merges of the topic into trunk's line: one keeps its file,
        // one keeps trunk's content only (`git merge -s ours`).
        let merge_of = |files: &[(&str, &str)], description: &str| {
            let (_write, _workspace, repo) = backend.load_repo_for_write(root).unwrap();
            let line = resolve_change_commit(&repo, &other).unwrap();
            let head = resolve_change_commit(&repo, &topic).unwrap();
            let store = repo.store().clone();
            let tree = file_tree(&store, files);
            let mut tx = repo.start_transaction();
            let merge = write_commit_with_tree(
                &mut tx,
                vec![line.id().clone(), head.id().clone()],
                description,
                tree,
            );
            pollster::block_on(tx.commit("a merge commit")).unwrap();
            merge.id().hex()
        };
        let kept = merge_of(
            &[("file.txt", "one\n"), ("other.txt", "o\n"), ("topic.txt", "t\n")],
            "Merge pull request #1",
        );
        let dropped = merge_of(
            &[("file.txt", "one\n"), ("other.txt", "o\n")],
            "Merge topic, keeping ours",
        );
        let unknown = "0123456789abcdef0123456789abcdef01234567";
        let check = |landing: &str| {
            backend
                .landed_check(root, "origin", "main", landing, &topic_commit, &topic_commit, &topic_commit)
                .unwrap()
        };
        // Not on trunk yet.
        assert_eq!(check(&kept), LandedCheck::Elsewhere);

        // The merge that kept the file: landed, also when the commit the
        // forge names has not been seen here, and also when it names the
        // pull request's own head.
        backend.move_bookmark(root, "main", &kept).unwrap();
        for landing in [kept.as_str(), unknown, topic_commit.as_str()] {
            assert_eq!(check(landing), LandedCheck::Landed, "{landing}");
        }
        // A later commit on trunk that removes the file again does not
        // undo that: the question is about the merge.
        backend.new_change(root, &kept).unwrap();
        std::fs::remove_file(root.join("topic.txt")).unwrap();
        let removal = backend.refresh(root).unwrap().working_copy;
        backend.describe(root, &removal, "remove topic.txt again").unwrap();
        backend.new_change(root, &removal).unwrap();
        backend.move_bookmark(root, "main", &removal).unwrap();
        assert_eq!(check(&kept), LandedCheck::Landed);

        // The merge that dropped it: the commits are in trunk's history
        // and the file is not on trunk. Whatever the forge names.
        backend.move_bookmark(root, "main", &dropped).unwrap();
        for landing in [dropped.as_str(), unknown, topic_commit.as_str()] {
            assert_eq!(
                check(landing),
                LandedCheck::Missing { paths: vec!["topic.txt".to_owned()], more: 0 },
                "{landing}"
            );
        }
        // Trunk moved straight onto the branch (a push of trunk to the
        // topic): the commit entered trunk as itself.
        backend.move_bookmark(root, "main", &topic).unwrap();
        assert_eq!(check(&topic_commit), LandedCheck::Landed);
    }

    /// After a landing the rest of a stack is rebuilt on trunk as the
    /// remote has it after the fetch. The local bookmark of the same name
    /// can be ahead of it with commits nobody pushed, and is the one the
    /// snapshot marks as trunk.
    #[test]
    fn rebase_onto_trunk_follows_the_remote_past_an_unpushed_local_bookmark() {
        let dir = tempfile::tempdir().unwrap();
        let bare = tempfile::tempdir().unwrap();
        let backend = test_backend();
        let root = dir.path();
        let first = build_colocated_repo(root, &backend);
        write_repo_config(
            root,
            "user.name = \"Test User\"\nuser.email = \"test@example.com\"\n\
             [revset-aliases]\n'trunk()' = 'main@origin'\n",
        );
        add_bare_origin(root, bare.path());
        backend.push_bookmarks(root, &["main".into()], None).unwrap();

        // A change waiting to land, and work committed on the local main
        // that was never pushed.
        let (topic, _) = record_change(root, &backend, &first, "topic", &[("topic.txt", "t\n")]);
        let (unpushed, _) =
            record_change(root, &backend, &first, "unpushed", &[("local.txt", "l\n")]);
        backend.move_bookmark(root, "main", &unpushed).unwrap();
        // The remote's main moves on, and a fetch brings it.
        let remote_first = bare_branch(bare.path(), "main").unwrap();
        let landed = bare_commit(bare.path(), "main", &remote_first, "landed on the remote");
        backend.git_fetch(root, None).unwrap();

        let snapshot = backend.open(root).unwrap();
        let marked = snapshot.bookmarks.iter().find(|b| b.is_trunk).unwrap();
        assert!(marked.is_local, "the local main is the bookmark marked as trunk");
        assert_eq!(marked.target, unpushed, "and it still sits on the unpushed commit");

        backend.rebase_onto_trunk(root, "origin", "main", &topic, &[]).unwrap();
        let snapshot = backend.open(root).unwrap();
        let moved = snapshot.nodes.iter().find(|n| n.id == topic).unwrap();
        let parent = snapshot.nodes.iter().find(|n| n.id == moved.parents[0]).unwrap();
        assert_eq!(parent.description, "landed on the remote");
        assert!(landed.starts_with(&parent.commit_id));
        // Already there: nothing more is recorded.
        assert!(backend
            .rebase_onto_trunk(root, "origin", "main", &topic, &[])
            .unwrap()
            .operation_id
            .is_none());
    }

    /// With no `trunk()` of the user's own, Jiji's default takes the
    /// latest of the remote's main and the local one. A commit made on
    /// the local main after the last fetch is then `trunk()`, and it is
    /// not what a pull request lands on. Landing goes by the remote's
    /// branch: for the check and for the rebase.
    #[test]
    fn landing_goes_by_the_remotes_trunk_when_the_default_trunk_is_a_local_commit() {
        let dir = tempfile::tempdir().unwrap();
        let bare = tempfile::tempdir().unwrap();
        let backend = test_backend();
        let root = dir.path();
        let first = build_colocated_repo(root, &backend);
        add_bare_origin(root, bare.path());
        backend.push_bookmarks(root, &["main".into()], None).unwrap();
        let (lower, lower_commit) =
            record_change(root, &backend, &first, "lower", &[("lower.txt", "l\n")]);
        let (upper, _) = record_change(root, &backend, &lower, "upper", &[("upper.txt", "u\n")]);

        // The lower change's pull request is squashed on the remote, and
        // a fetch brings the squash; the local main follows it.
        let squash = {
            // `lower` sits directly on trunk, so its tree is the squash's.
            // The bare repository needs that tree before it can commit it.
            let lower_tree = git(root, &["rev-parse", &format!("{lower_commit}^{{tree}}")]);
            let scratch = format!("{lower_commit}:refs/heads/scratch-lower");
            git(root, &["push", "--quiet", bare.path().to_str().unwrap(), &scratch]);
            let parent = bare_branch(bare.path(), "main").unwrap();
            let commit = git(
                bare.path(),
                &[
                    "-c",
                    "user.name=Upstream",
                    "-c",
                    "user.email=upstream@example.com",
                    "commit-tree",
                    &lower_tree,
                    "-p",
                    &parent,
                    "-m",
                    "lower (#1)",
                ],
            );
            git(bare.path(), &["update-ref", "refs/heads/main", &commit]);
            git(bare.path(), &["update-ref", "-d", "refs/heads/scratch-lower"]);
            commit
        };
        backend.git_fetch(root, None).unwrap();
        // Then a commit on the local main, not pushed. It is newer than
        // the squash, so the default `trunk()` now names it.
        let squash_node = {
            let snapshot = backend.open(root).unwrap();
            snapshot.nodes.iter().find(|n| squash.starts_with(&n.commit_id)).unwrap().id.clone()
        };
        std::thread::sleep(std::time::Duration::from_millis(1100));
        let (unpushed, _) = record_change(
            root,
            &backend,
            &squash_node,
            "unpushed",
            &[("unpublished.txt", "x\n")],
        );
        backend.move_bookmark(root, "main", &unpushed).unwrap();
        let snapshot = backend.open(root).unwrap();
        let unpushed_node = snapshot.nodes.iter().find(|n| n.id == unpushed).unwrap();
        assert_eq!(
            unpushed_node.kind,
            NodeKind::Immutable,
            "the default trunk() has moved onto the unpushed commit"
        );

        // The check judges the squash against the remote's main.
        assert_eq!(
            backend
                .landed_check(root, "origin", "main", &squash, &lower_commit, &lower, &lower)
                .unwrap(),
            LandedCheck::Landed
        );
        // And the rest of the stack goes onto the squash, not onto the
        // unpushed commit.
        backend
            .rebase_onto_trunk(root, "origin", "main", &upper, std::slice::from_ref(&lower_commit))
            .unwrap();
        let snapshot = backend.open(root).unwrap();
        let moved = snapshot.nodes.iter().find(|n| n.id == upper).unwrap();
        let parent = snapshot.nodes.iter().find(|n| n.id == moved.parents[0]).unwrap();
        assert!(squash.starts_with(&parent.commit_id), "{}", parent.description);
        // Its tree has nothing of the unpushed commit.
        let tree = git(root, &["ls-tree", "-r", "--name-only", &resolved_commit_id(root, &upper)]);
        assert!(tree.contains("upper.txt") && tree.contains("lower.txt"), "{tree}");
        assert!(!tree.contains("unpublished.txt"), "{tree}");

        // A fetch can also change which branch `trunk()` names. The
        // default alias takes the latest of `main`, `master` and `trunk`:
        // someone moves `master` ahead of `main` on the remote.
        std::thread::sleep(std::time::Duration::from_millis(1100));
        let squash_tree = git(bare.path(), &["rev-parse", &format!("{squash}^{{tree}}")]);
        let legacy = git(
            bare.path(),
            &[
                "-c",
                "user.name=Upstream",
                "-c",
                "user.email=upstream@example.com",
                "commit-tree",
                &squash_tree,
                "-p",
                &squash,
                "-m",
                "legacy",
            ],
        );
        git(bare.path(), &["update-ref", "refs/heads/master", &legacy]);
        backend.git_fetch(root, None).unwrap();
        assert_eq!(
            backend.open(root).unwrap().trunk_bookmark,
            "master",
            "trunk() names the other branch now"
        );
        // A landing that was planned for `main` stays on `main`: the
        // stack is where it should be, and is not moved onto `master`.
        backend.rebase_onto_trunk(root, "origin", "main", &upper, &[]).unwrap();
        let snapshot = backend.open(root).unwrap();
        let moved = snapshot.nodes.iter().find(|n| n.id == upper).unwrap();
        let parent = snapshot.nodes.iter().find(|n| n.id == moved.parents[0]).unwrap();
        assert!(squash.starts_with(&parent.commit_id), "{}", parent.description);

        // And a branch the remote does not have is refused, not replaced
        // by a local bookmark of that name.
        backend.create_bookmark(root, "develop", &upper).unwrap();
        let err = backend
            .landed_check(root, "origin", "develop", &squash, &lower_commit, &lower, &lower)
            .unwrap_err();
        assert!(err.to_string().contains("from origin here to land on"), "{err}");
        let err = backend
            .rebase_onto_trunk(root, "origin", "develop", &upper, &[])
            .unwrap_err();
        assert!(err.to_string().contains("from origin here to land on"), "{err}");
    }

    /// A change that merges a side branch in keeps that branch when the
    /// stack moves onto trunk. Only the parent that landed is replaced:
    /// with the destination as its one parent, the change would lose
    /// everything the side branch brings.
    #[test]
    fn rebase_onto_trunk_keeps_the_other_parent_of_a_merge() {
        let dir = tempfile::tempdir().unwrap();
        let backend = test_backend();
        let root = dir.path();
        let first = build_colocated_repo(root, &backend);
        // `lower` lands as a squash. `upper` sits on it and merges `side`.
        let (lower, lower_commit) =
            record_change(root, &backend, &first, "lower", &[("lower.txt", "l\n")]);
        let (side, _) = record_change(root, &backend, &first, "side", &[("side.txt", "s\n")]);
        let upper = {
            let (_write, _workspace, repo) = backend.load_repo_for_write(root).unwrap();
            let below = resolve_change_commit(&repo, &lower).unwrap();
            let other = resolve_change_commit(&repo, &side).unwrap();
            let store = repo.store().clone();
            let tree = file_tree(
                &store,
                &[("file.txt", "one\n"), ("lower.txt", "l\n"), ("side.txt", "s\n"), ("upper.txt", "u\n")],
            );
            let mut tx = repo.start_transaction();
            let upper = write_commit_with_tree(
                &mut tx,
                vec![below.id().clone(), other.id().clone()],
                "upper: merge side",
                tree,
            );
            pollster::block_on(tx.commit("a merge on the stack")).unwrap();
            upper.id().hex()
        };
        let (squash, _) =
            record_change(root, &backend, &first, "lower (#1)", &[("lower.txt", "l\n")]);
        backend.move_bookmark(root, "main", &squash).unwrap();

        let files_of = |commit: &str| {
            let mut files: Vec<String> = git(root, &["ls-tree", "-r", "--name-only", commit])
                .lines()
                .map(str::to_owned)
                .collect();
            files.sort();
            files
        };
        // What landing the original tip would put on trunk.
        let expected = vec!["file.txt", "lower.txt", "side.txt", "upper.txt"];
        assert_eq!(files_of(&upper), expected);

        let outcome = backend
            .rebase_onto_trunk(root, "origin", "main", &upper, std::slice::from_ref(&lower_commit))
            .unwrap();
        let moved = outcome.target_change.unwrap();
        let snapshot = backend.open(root).unwrap();
        let node = snapshot.nodes.iter().find(|n| n.id == moved).unwrap();
        assert_eq!(node.parents, vec![squash.clone(), side.clone()], "trunk, and the side branch");
        let rebased = resolved_commit_id(root, &moved);
        assert_eq!(files_of(&rebased), expected);
        // Trunk is the squash of what was under the tip and nothing else,
        // so the rebased change has the very tree the tip had: landing it
        // leaves trunk as landing the tip would have.
        let tree_of = |commit: &str| git(root, &["rev-parse", &format!("{commit}^{{tree}}")]);
        assert_eq!(tree_of(&rebased), tree_of(&upper));

        // A change none of whose parents landed is left where it is.
        let (loose, _) = record_change(root, &backend, &side, "loose", &[("loose.txt", "x\n")]);
        let outcome = backend
            .rebase_onto_trunk(root, "origin", "main", &loose, std::slice::from_ref(&lower_commit))
            .unwrap();
        assert!(outcome.operation_id.is_none());
        assert!(outcome.summary.contains("left where it is"), "{}", outcome.summary);
    }

    /// "Update branch" on GitHub adds a merge of trunk to a pull request's
    /// branch, and the pull request is then merged there. A Land planned
    /// from the commit that was here checks that commit, sweeps it, and
    /// leaves the bookmark, which the fetch moved onto GitHub's merge. A
    /// second Land plans from what is left and finishes: this pins the
    /// two answers that makes true.
    #[test]
    fn a_branch_updated_on_github_before_its_merge_is_cleaned_up_in_two_lands() {
        let dir = tempfile::tempdir().unwrap();
        let backend = test_backend();
        let root = dir.path();
        let first = build_colocated_repo(root, &backend);
        let (topic, topic_commit) =
            record_change(root, &backend, &first, "topic", &[("topic.txt", "t\n")]);
        backend.create_bookmark(root, "topic", &topic).unwrap();
        let (other, _) = record_change(root, &backend, &first, "other", &[("other.txt", "o\n")]);
        backend.move_bookmark(root, "main", &other).unwrap();
        // GitHub's "Update branch": a merge of main into the topic.
        let updated = {
            let (_write, _workspace, repo) = backend.load_repo_for_write(root).unwrap();
            let head = resolve_change_commit(&repo, &topic).unwrap();
            let trunk = resolve_change_commit(&repo, &other).unwrap();
            let store = repo.store().clone();
            let tree = file_tree(
                &store,
                &[("file.txt", "one\n"), ("other.txt", "o\n"), ("topic.txt", "t\n")],
            );
            let mut tx = repo.start_transaction();
            let merge = write_commit_with_tree(
                &mut tx,
                vec![head.id().clone(), trunk.id().clone()],
                "Merge branch 'main' into topic",
                tree,
            );
            pollster::block_on(tx.commit("update branch")).unwrap();
            merge.id().hex()
        };
        // GitHub squashes that head onto main, and the fetch moves the
        // bookmark to it.
        let (squash, squash_commit) =
            record_change(root, &backend, &other, "topic (#1)", &[("topic.txt", "t\n")]);
        backend.move_bookmark(root, "main", &squash).unwrap();
        backend.move_bookmark(root, "topic", &updated).unwrap();

        // First Land: planned from the topic's own commit. The pull
        // request had another head, so trunk's tip vouches for it.
        assert_eq!(
            backend
                .landed_check(root, "origin", "main", &squash_commit, &updated, &topic_commit, &topic_commit)
                .unwrap(),
            LandedCheck::Held
        );
        // The bookmark is no longer there, so it stays; the checked
        // commit is abandoned, which rewrites the merge above it.
        assert!(backend.delete_bookmark_at(root, "topic", &topic_commit).is_err());
        backend.abandon_changes(root, &[topic_commit]).unwrap();

        // Second Land: the stack under "topic" is what is left of the
        // merge, and trunk holds all of it.
        let snapshot = backend.open(root).unwrap();
        let bookmark = snapshot.bookmarks.iter().find(|b| b.name == "topic").unwrap();
        let left = snapshot.nodes.iter().find(|n| n.id == bookmark.target).unwrap();
        assert_eq!(left.kind, NodeKind::Mutable);
        let left_commit = left.commit_id.clone();
        assert_eq!(
            backend
                .landed_check(root, "origin", "main", &squash_commit, &updated, &left_commit, &left_commit)
                .unwrap(),
            LandedCheck::Held
        );
        backend.delete_bookmark_at(root, "topic", &left_commit).unwrap();
        backend.abandon_changes(root, &[left_commit]).unwrap();
        let snapshot = backend.open(root).unwrap();
        assert!(!snapshot.bookmarks.iter().any(|b| b.name == "topic"));
        assert!(
            snapshot
                .nodes
                .iter()
                .all(|n| n.kind != NodeKind::Mutable || n.description.is_empty()),
            "nothing of the topic is left"
        );
    }

    /// A pull request of two commits, taken into trunk by a merge that
    /// keeps the second commit's file and drops the first one's change. A
    /// caller that has only the pull request's head (a landed bookmark
    /// found again, with nothing mutable left to walk) names that one
    /// commit as the whole span. The merge is asked about everything it
    /// brought in all the same.
    #[test]
    fn landed_check_asks_a_merge_about_the_whole_branch_it_brought_in() {
        let dir = tempfile::tempdir().unwrap();
        let backend = test_backend();
        let root = dir.path();
        let first = build_colocated_repo(root, &backend);
        let (flag, _) = record_change(root, &backend, &first, "topic: flag on", &[("flag.txt", "on\n")]);
        let (topic, head) = record_change(root, &backend, &flag, "topic: a file", &[("new.txt", "n\n")]);
        let (other, _) = record_change(root, &backend, &first, "other", &[("other.txt", "o\n")]);
        let merge_of = |files: &[(&str, &str)], description: &str| {
            let (_write, _workspace, repo) = backend.load_repo_for_write(root).unwrap();
            let line = resolve_change_commit(&repo, &other).unwrap();
            let branch = resolve_change_commit(&repo, &topic).unwrap();
            let store = repo.store().clone();
            let tree = file_tree(&store, files);
            let mut tx = repo.start_transaction();
            let merge = write_commit_with_tree(
                &mut tx,
                vec![line.id().clone(), branch.id().clone()],
                description,
                tree,
            );
            pollster::block_on(tx.commit("a merge commit")).unwrap();
            merge.id().hex()
        };
        let partial = merge_of(
            &[("file.txt", "one\n"), ("new.txt", "n\n"), ("other.txt", "o\n")],
            "Merge topic, without its flag",
        );
        let whole = merge_of(
            &[("file.txt", "one\n"), ("flag.txt", "on\n"), ("new.txt", "n\n"), ("other.txt", "o\n")],
            "Merge pull request #1",
        );
        // Head, head, head: all a caller without the commits can say.
        let by_head = |landing: &str| {
            backend.landed_check(root, "origin", "main", landing, &head, &head, &head).unwrap()
        };
        backend.move_bookmark(root, "main", &partial).unwrap();
        assert_eq!(
            by_head(&partial),
            LandedCheck::Missing { paths: vec!["flag.txt".to_owned()], more: 0 }
        );
        backend.move_bookmark(root, "main", &whole).unwrap();
        assert_eq!(by_head(&whole), LandedCheck::Landed);
    }

    /// Seen on GitHub. A flag is off. The lower pull request turns it on,
    /// the upper one turns it off again, and trunk turns it on by itself.
    /// The tip, merged in one go, leaves trunk's "on" alone: the stack as
    /// a whole does not touch the flag. One pull request at a time, the
    /// lower one merges cleanly (trunk already has its change) and the
    /// upper one then turns the flag off. Both ways are computed and
    /// compared before anything merges.
    #[test]
    fn landing_one_at_a_time_is_compared_with_landing_the_tip() {
        let dir = tempfile::tempdir().unwrap();
        let backend = test_backend();
        let root = dir.path();
        let first = build_colocated_repo(root, &backend);
        let (base, _) = record_change(root, &backend, &first, "flag off", &[("flag.txt", "off\n")]);
        backend.move_bookmark(root, "main", &base).unwrap();
        let (lower, lower_commit) = record_change(
            root,
            &backend,
            &base,
            "lower: flag on",
            &[("flag.txt", "on\n"), ("lower.txt", "l\n")],
        );
        let (_, upper_commit) = record_change(
            root,
            &backend,
            &lower,
            "upper: flag off again",
            &[("flag.txt", "off\n"), ("upper.txt", "u\n")],
        );
        let (flag_on, _) =
            record_change(root, &backend, &base, "someone turns the flag on", &[("flag.txt", "on\n")]);
        let (elsewhere, _) =
            record_change(root, &backend, &base, "someone adds a file", &[("other.txt", "o\n")]);
        let two_pull_requests = || {
            backend
                .lands_as_tip(root, "origin", "main", &lower_commit, &lower_commit, &upper_commit)
                .unwrap()
        };

        // Trunk where the stack was based: nothing to compare.
        assert_eq!(two_pull_requests(), TipCheck::Same);
        // Trunk moved somewhere the stack does not go: both ways agree.
        backend.move_bookmark(root, "main", &elsewhere).unwrap();
        assert_eq!(two_pull_requests(), TipCheck::Same);
        // Trunk made the lower pull request's change itself.
        backend.move_bookmark(root, "main", &flag_on).unwrap();
        assert_eq!(
            two_pull_requests(),
            TipCheck::Differs { paths: vec!["flag.txt".to_owned()], more: 0 }
        );
        // The same two commits as one pull request are the tip, merged in
        // one piece: there is nothing to differ from.
        assert_eq!(
            backend
                .lands_as_tip(root, "origin", "main", &lower_commit, &upper_commit, &upper_commit)
                .unwrap(),
            TipCheck::Same
        );
        // A stack that does not sit on trunk's history is not judged.
        assert_eq!(
            backend
                .lands_as_tip(root, "origin", "main", &upper_commit, &upper_commit, &upper_commit)
                .unwrap(),
            TipCheck::Unknown
        );
    }

    /// The same comparison once the bottom of a stack has landed, which
    /// is when what is left gets rebased onto trunk one change at a time.
    /// A squash leaves the landed commits here, off trunk's history, with
    /// the rest of the stack on them: that base counts when trunk holds
    /// its content. (Seen on GitHub: the lower pull request merged, trunk
    /// turned a flag on by itself, the middle one turns it on and the
    /// upper one off again. Resumed, the stack landed with the flag off.)
    #[test]
    fn the_rest_of_a_stack_is_compared_with_its_tip_once_what_is_below_has_landed() {
        let dir = tempfile::tempdir().unwrap();
        let backend = test_backend();
        let root = dir.path();
        let first = build_colocated_repo(root, &backend);
        let (base, _) = record_change(root, &backend, &first, "flag off", &[("flag.txt", "off\n")]);
        backend.move_bookmark(root, "main", &base).unwrap();
        let (lower, _) = record_change(root, &backend, &base, "lower", &[("lower.txt", "l\n")]);
        let (middle, middle_commit) =
            record_change(root, &backend, &lower, "middle: flag on", &[("flag.txt", "on\n")]);
        let (_, upper_commit) = record_change(
            root,
            &backend,
            &middle,
            "upper: flag off again",
            &[("flag.txt", "off\n"), ("upper.txt", "u\n")],
        );
        let (squash, _) =
            record_change(root, &backend, &base, "lower (#1)", &[("lower.txt", "l\n")]);
        let (flag_on, _) =
            record_change(root, &backend, &squash, "someone turns the flag on", &[("flag.txt", "on\n")]);
        // Every commit of the rest in turn: the first one stands for itself.
        let rest = || {
            backend
                .lands_as_tip(root, "origin", "main", &middle_commit, &middle_commit, &upper_commit)
                .unwrap()
        };

        // Trunk without the landing: what the rest sits on is not on
        // trunk, and nothing can be said.
        assert_eq!(rest(), TipCheck::Unknown);
        // Landed, and nothing else moved: both ways agree.
        backend.move_bookmark(root, "main", &squash).unwrap();
        assert_eq!(rest(), TipCheck::Same);
        // Landed, and trunk has turned the flag on by itself.
        backend.move_bookmark(root, "main", &flag_on).unwrap();
        assert_eq!(
            rest(),
            TipCheck::Differs { paths: vec!["flag.txt".to_owned()], more: 0 }
        );
    }

    /// Trunk and a branch that merged each other before have two merge
    /// bases, and "where the branch left trunk" has two answers. A merge
    /// that then takes the branch in, keeps its last file and drops an
    /// earlier change has to hold what came in from each of them: with
    /// one answer picked, or none, the check looked at the last commit
    /// only and the bookmark went.
    #[test]
    fn landed_check_asks_about_every_place_a_branch_left_trunk() {
        let dir = tempfile::tempdir().unwrap();
        let backend = test_backend();
        let root = dir.path();
        let first = build_colocated_repo(root, &backend);
        let (a, _) = record_change(root, &backend, &first, "trunk side", &[("a.txt", "a\n")]);
        let (b, _) = record_change(root, &backend, &first, "topic side", &[("b.txt", "b\n")]);
        let merge = |parents: [&str; 2], files: &[(&str, &str)], description: &str| -> String {
            let (_write, _workspace, repo) = backend.load_repo_for_write(root).unwrap();
            let ids: Vec<CommitId> = parents
                .iter()
                .map(|p| resolve_change_commit(&repo, p).unwrap().id().clone())
                .collect();
            let store = repo.store().clone();
            let tree = file_tree(&store, files);
            let mut tx = repo.start_transaction();
            let commit = write_commit_with_tree(&mut tx, ids, description, tree);
            pollster::block_on(tx.commit("a merge commit")).unwrap();
            commit.id().hex()
        };
        // Each side takes the other in: from here on there are two.
        let both = [("a.txt", "a\n"), ("b.txt", "b\n"), ("file.txt", "one\n")];
        let on_trunk = merge([&a, &b], &both, "trunk takes the topic side in");
        let on_topic = merge([&b, &a], &both, "the topic takes trunk in");
        let (flag, _) =
            record_change(root, &backend, &on_topic, "topic: flag on", &[("flag.txt", "on\n")]);
        let (topic, head) =
            record_change(root, &backend, &flag, "topic: a file", &[("new.txt", "n\n")]);
        let (line, _) = record_change(root, &backend, &on_trunk, "other", &[("other.txt", "o\n")]);
        let partial = merge(
            [&line, &topic],
            &[
                ("a.txt", "a\n"),
                ("b.txt", "b\n"),
                ("file.txt", "one\n"),
                ("new.txt", "n\n"),
                ("other.txt", "o\n"),
            ],
            "Merge topic, without its flag",
        );
        let whole = merge(
            [&line, &topic],
            &[
                ("a.txt", "a\n"),
                ("b.txt", "b\n"),
                ("file.txt", "one\n"),
                ("flag.txt", "on\n"),
                ("new.txt", "n\n"),
                ("other.txt", "o\n"),
            ],
            "Merge pull request #1",
        );
        let by_head = |landing: &str| {
            backend.landed_check(root, "origin", "main", landing, &head, &head, &head).unwrap()
        };
        backend.move_bookmark(root, "main", &partial).unwrap();
        assert_eq!(
            by_head(&partial),
            LandedCheck::Missing { paths: vec!["flag.txt".to_owned()], more: 0 }
        );
        backend.move_bookmark(root, "main", &whole).unwrap();
        assert_eq!(by_head(&whole), LandedCheck::Landed);
    }

    /// Two flags make a feature, on top of a file it starts with. The
    /// flags were written on two lines that trunk merged one by one before
    /// it merged the feature, so the branch left trunk in two places, one
    /// holding each flag. One tree has to hold everything the feature
    /// brought, whichever span the caller names:
    ///
    /// - The merge of the feature kept the first flag only, and a later
    ///   commit on trunk turned the first off and the second on. Each
    ///   place is accounted for somewhere, one at the merge and one at
    ///   trunk's tip, and at no time did trunk have both flags on.
    /// - The merge kept both flags and dropped the file. Both places are
    ///   accounted for at the merge; the span the caller names, from below
    ///   the file, is not, and that span is what was asked before there
    ///   was more than one place to ask about.
    #[test]
    fn landed_check_wants_one_tree_to_hold_every_place_a_branch_left_trunk() {
        let dir = tempfile::tempdir().unwrap();
        let backend = test_backend();
        let root = dir.path();
        let first = build_colocated_repo(root, &backend);
        let (off, _) = record_change(
            root,
            &backend,
            &first,
            "flags off",
            &[("flag1.txt", "off\n"), ("flag2.txt", "off\n")],
        );
        let (seed, seed_commit) =
            record_change(root, &backend, &off, "feature: its first file", &[("seed.txt", "s\n")]);
        let (one, _) =
            record_change(root, &backend, &seed, "feature: flag one", &[("flag1.txt", "on\n")]);
        let (two, _) =
            record_change(root, &backend, &seed, "feature: flag two", &[("flag2.txt", "on\n")]);
        let (other, _) = record_change(root, &backend, &off, "other", &[("other.txt", "o\n")]);
        // A merge of two commits with exactly these files.
        let merge = |parents: [&str; 2], files: &[(&str, &str)], description: &str| -> String {
            let (_write, _workspace, repo) = backend.load_repo_for_write(root).unwrap();
            let ids: Vec<CommitId> = parents
                .iter()
                .map(|p| resolve_change_commit(&repo, p).unwrap().id().clone())
                .collect();
            let store = repo.store().clone();
            let mut all = vec![("file.txt", "one\n")];
            all.extend_from_slice(files);
            all.sort();
            let tree = file_tree(&store, &all);
            let mut tx = repo.start_transaction();
            let commit = write_commit_with_tree(&mut tx, ids, description, tree);
            pollster::block_on(tx.commit("a merge commit")).unwrap();
            commit.id().hex()
        };
        let (on, no) = ("on\n", "off\n");
        // The feature's head: both lines, both flags on.
        let head = merge(
            [&one, &two],
            &[("flag1.txt", on), ("flag2.txt", on), ("seed.txt", "s\n")],
            "feature: both flags",
        );
        // Trunk takes each line in by itself, and leaves the second flag off.
        let trunk_files = [("flag1.txt", on), ("flag2.txt", no), ("other.txt", "o\n"), ("seed.txt", "s\n")];
        let with_one = merge([&other, &one], &trunk_files, "trunk takes flag one in");
        let with_two = merge([&with_one, &two], &trunk_files, "trunk takes flag two in, off");
        // Asked with the feature's whole span, and with its head alone.
        let whole_span = |landing: &str| {
            backend
                .landed_check(root, "origin", "main", landing, &head, &seed_commit, &head)
                .unwrap()
        };
        let by_head = |landing: &str| {
            backend.landed_check(root, "origin", "main", landing, &head, &head, &head).unwrap()
        };

        // The merge of the feature keeps the first flag only.
        let partial = merge([&with_two, &head], &trunk_files, "Merge feature, first flag only");
        let (swapped, _) = record_change(
            root,
            &backend,
            &partial,
            "swap the flags",
            &[("flag1.txt", "off\n"), ("flag2.txt", "on\n")],
        );
        let no_second = LandedCheck::Missing { paths: vec!["flag2.txt".to_owned()], more: 0 };
        backend.move_bookmark(root, "main", &partial).unwrap();
        assert_eq!(whole_span(&partial), no_second);
        assert_eq!(by_head(&partial), no_second);
        // With the flags swapped on trunk, each half is somewhere and the
        // whole is nowhere.
        backend.move_bookmark(root, "main", &swapped).unwrap();
        assert_eq!(whole_span(&partial), no_second);
        assert_eq!(by_head(&partial), no_second);

        // The merge keeps both flags and drops the file.
        let without_seed = merge(
            [&with_two, &head],
            &[("flag1.txt", on), ("flag2.txt", on), ("other.txt", "o\n")],
            "Merge feature, without its first file",
        );
        backend.move_bookmark(root, "main", &without_seed).unwrap();
        assert_eq!(
            whole_span(&without_seed),
            LandedCheck::Missing { paths: vec!["seed.txt".to_owned()], more: 0 }
        );
        // And the merge that keeps everything is a landing.
        let everything = merge(
            [&with_two, &head],
            &[("flag1.txt", on), ("flag2.txt", on), ("other.txt", "o\n"), ("seed.txt", "s\n")],
            "Merge pull request #1",
        );
        backend.move_bookmark(root, "main", &everything).unwrap();
        assert_eq!(whole_span(&everything), LandedCheck::Landed);
        assert_eq!(by_head(&everything), LandedCheck::Landed);
    }

    /// The sweep after a landing abandons the landed commits and nothing
    /// may sit on them then: jj would rebase a child onto what was below
    /// them, and its tree would lose what they hold. A change started on
    /// one after the caller looked stops the sweep, with its tree as it
    /// was.
    #[test]
    fn abandon_landed_refuses_while_a_change_sits_on_the_landed_commits() {
        let dir = tempfile::tempdir().unwrap();
        let backend = test_backend();
        let root = dir.path();
        let first = build_colocated_repo(root, &backend);
        let (landed, landed_commit) =
            record_change(root, &backend, &first, "landed: flag on", &[("flag.txt", "on\n")]);
        let (follow, follow_commit) =
            record_change(root, &backend, &landed, "follow-up", &[("uses_flag.txt", "x\n")]);

        let err = backend
            .abandon_landed(root, std::slice::from_ref(&landed_commit))
            .unwrap_err();
        assert!(err.to_string().contains("on the landed commits"), "{err}");
        let snapshot = backend.open(root).unwrap();
        assert!(snapshot.nodes.iter().any(|n| n.id == landed), "nothing was abandoned");
        assert!(
            resolved_commit_id(root, &follow).starts_with(&follow_commit),
            "the follow-up is untouched"
        );
        assert_eq!(git(root, &["show", &format!("{follow_commit}:flag.txt")]), "on");

        // With nothing on them, they go.
        backend.rebase_change(root, &follow, &first).unwrap();
        backend
            .abandon_landed(root, std::slice::from_ref(&landed_commit))
            .unwrap();
        let snapshot = backend.open(root).unwrap();
        assert!(!snapshot.nodes.iter().any(|n| n.id == landed));
    }

    /// Cleanup after a landing deletes a bookmark only while it points at
    /// the commit that landed. A bookmark that moved, by a rewrite or by
    /// an edit the working copy has not recorded yet, stays.
    #[test]
    fn delete_bookmark_at_spares_a_bookmark_that_moved() {
        let dir = tempfile::tempdir().unwrap();
        let backend = test_backend();
        let root = dir.path();
        let first = build_colocated_repo(root, &backend);
        let commit_of = |id: &str| {
            let snapshot = backend.open(root).unwrap();
            snapshot.nodes.iter().find(|n| n.id == id).unwrap().commit_id.clone()
        };
        let (topic, planned) = record_change(root, &backend, &first, "topic", &[("t.txt", "t\n")]);
        backend.create_bookmark(root, "topic", &topic).unwrap();

        // Reworded since: the same change, another commit.
        backend.describe(root, &topic, "topic, reworded").unwrap();
        let err = backend.delete_bookmark_at(root, "topic", &planned).unwrap_err();
        assert!(err.to_string().contains("no longer points at"), "{err}");
        assert!(backend.open(root).unwrap().bookmarks.iter().any(|b| b.name == "topic"));

        // Edited in the working copy, with nothing having recorded the
        // edit yet: the mutation records it first, and then refuses.
        backend.edit_change(root, &topic).unwrap();
        let planned = commit_of(&topic);
        std::fs::write(root.join("late.txt"), "late\n").unwrap();
        let err = backend.delete_bookmark_at(root, "topic", &planned).unwrap_err();
        assert!(err.to_string().contains("no longer points at"), "{err}");
        assert!(backend.open(root).unwrap().bookmarks.iter().any(|b| b.name == "topic"));

        // Unmoved, it goes.
        let planned = commit_of(&topic);
        backend.delete_bookmark_at(root, "topic", &planned).unwrap();
        assert!(!backend.open(root).unwrap().bookmarks.iter().any(|b| b.name == "topic"));
    }

    /// Cleanup abandons landed changes by commit id, so a change edited
    /// since (here in the working copy, not yet recorded) is refused
    /// whole and nothing is lost.
    #[test]
    fn abandoning_by_commit_id_refuses_a_change_edited_since() {
        let dir = tempfile::tempdir().unwrap();
        let backend = test_backend();
        let root = dir.path();
        let first = build_colocated_repo(root, &backend);
        let (lower, lower_commit) =
            record_change(root, &backend, &first, "lower", &[("l.txt", "l\n")]);
        let (upper, upper_commit) =
            record_change(root, &backend, &lower, "upper", &[("u.txt", "u\n")]);
        backend.edit_change(root, &upper).unwrap();
        std::fs::write(root.join("late.txt"), "late\n").unwrap();

        let err = backend
            .abandon_changes(root, &[upper_commit, lower_commit.clone()])
            .unwrap_err();
        assert!(err.to_string().contains("has been rewritten"), "{err}");
        let snapshot = backend.open(root).unwrap();
        assert!(snapshot.nodes.iter().any(|n| n.id == lower));
        assert!(snapshot.nodes.iter().any(|n| n.id == upper));
        assert!(root.join("late.txt").exists());
        // The untouched one alone still goes.
        backend.abandon_changes(root, &[lower_commit]).unwrap();
        assert!(!backend.open(root).unwrap().nodes.iter().any(|n| n.id == lower));
        assert!(root.join("late.txt").exists());
    }

    fn build_conflicted_repo(root: &Path, backend: &JjBackend) -> (String, String, String) {
        let settings = test_settings();
        let (_workspace, repo) =
            pollster::block_on(Workspace::init_simple(&settings, root)).unwrap();
        let store = repo.store().clone();
        let root_commit_id = store.root_commit_id().clone();
        let mut tx = repo.start_transaction();
        let base = write_commit_with_tree(
            &mut tx,
            vec![root_commit_id],
            "base: shared notes",
            file_tree(&store, &[("notes.txt", "base\n")]),
        );
        tx.repo_mut()
            .set_local_bookmark_target(RefName::new("main"), RefTarget::normal(base.id().clone()));
        let _ours = write_commit_with_tree(
            &mut tx,
            vec![base.id().clone()],
            "feat: ours",
            file_tree(&store, &[("notes.txt", "ours\n")]),
        );
        let theirs = write_commit_with_tree(
            &mut tx,
            vec![base.id().clone()],
            "feat: theirs",
            file_tree(&store, &[("notes.txt", "theirs\n")]),
        );
        write_commit_with_tree(
            &mut tx,
            vec![theirs.id().clone()],
            "docs: readme only",
            file_tree(&store, &[("notes.txt", "theirs\n"), ("README.md", "hi\n")]),
        );
        pollster::block_on(tx.commit("set up conflicting siblings")).unwrap();

        let before = backend.open(root).unwrap();
        let ours_id = node_by_description(&before, "feat: ours").id.clone();
        let theirs_id = node_by_description(&before, "feat: theirs").id.clone();
        let child_id = node_by_description(&before, "docs: readme only").id.clone();
        backend.rebase_change(root, &theirs_id, &ours_id).unwrap();
        (ours_id, theirs_id, child_id)
    }

    /// Points the repo's `ui.merge-editor` at a fake tool: a shell script
    /// with `body` (temp files arrive as `$1`=base `$2`=left `$3`=right
    /// `$4`=output). The script lives outside the workspace so running it
    /// never dirties the working copy. `tool_config` appends extra
    /// `[merge-tools.fake]` keys.
    fn install_fake_merge_tool(root: &Path, scratch: &Path, body: &str, tool_config: &str) {
        let script = scratch.join("fake-merge-tool.sh");
        std::fs::write(&script, format!("#!/bin/sh\n{body}\n")).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        write_repo_config(
            root,
            &format!(
                "ui.merge-editor = \"fake\"\n[merge-tools.fake]\nprogram = \"{}\"\n\
                 merge-args = [\"$base\", \"$left\", \"$right\", \"$output\"]\n{tool_config}",
                script.display()
            ),
        );
    }

    #[test]
    #[cfg(unix)]
    fn resolve_conflict_records_the_tools_output_and_rebases_descendants() {
        let dir = tempfile::tempdir().unwrap();
        let scratch = tempfile::tempdir().unwrap();
        let backend = test_backend();
        let (_ours_id, theirs_id, child_id) = build_conflicted_repo(dir.path(), &backend);
        // The tool sees all three sides read-only and saves a merge; the
        // capture files pin what the temp files actually contained.
        install_fake_merge_tool(
            dir.path(),
            scratch.path(),
            &format!(
                "cp \"$1\" {s}/got_base; cp \"$2\" {s}/got_left; cp \"$3\" {s}/got_right\n\
                 printf 'merged\\n' > \"$4\"",
                s = scratch.path().display()
            ),
            "",
        );
        let before = backend.open(dir.path()).unwrap();
        assert_eq!(before.resolve_tool.as_deref(), Some("fake"));
        assert_eq!(before.conflicts.len(), 2, "theirs and its child are conflicted");
        let theirs_commit = node_by_description(&before, "feat: theirs").commit_id.clone();

        let outcome = backend
            .resolve_conflict(dir.path(), &theirs_id, "notes.txt")
            .unwrap();
        assert!(outcome.operation_id.is_some());
        assert_eq!(outcome.summary, format!("Resolved notes.txt in {theirs_id}"));
        assert_eq!(outcome.target_change.as_deref(), Some(theirs_id.as_str()));

        // The tool got the real sides: the shared base and the two rewrites.
        let read = |name: &str| std::fs::read_to_string(scratch.path().join(name)).unwrap();
        assert_eq!(read("got_base"), "base\n");
        let sides = [read("got_left"), read("got_right")];
        assert!(sides.contains(&"ours\n".to_owned()), "sides: {sides:?}");
        assert!(sides.contains(&"theirs\n".to_owned()), "sides: {sides:?}");

        // The conflict is gone — and the child, which carried the inherited
        // conflict in a file it never touched, auto-resolved through the
        // descendant rebase (its side never diverged from the parent's).
        let after = backend.open(dir.path()).unwrap();
        assert!(after.conflicts.is_empty(), "got {:?}", after.conflicts);
        for id in [&theirs_id, &child_id] {
            assert!(!after.nodes.iter().find(|n| &n.id == id).unwrap().has_conflict);
        }
        assert_eq!(
            after.operations[0].description,
            format!("Resolve conflicts in commit {}", full_commit_hex(&after, &theirs_commit)),
        );
        // The rewritten change now diffs as plain resolved content.
        let diff = backend.change_diff(dir.path(), &theirs_id).unwrap();
        let file = diff.files.iter().find(|f| f.path == "notes.txt").unwrap();
        assert!(!file.has_conflict);
        let FileDiffContent::Text { hunks, .. } = &file.content else {
            panic!("expected text content");
        };
        let added: Vec<String> = hunks
            .iter()
            .flat_map(|h| &h.lines)
            .filter(|l| l.kind == DiffLineKind::Added)
            .map(|l| l.segments.iter().map(|s| s.text.as_str()).collect())
            .collect();
        assert_eq!(added, vec!["merged"]);
    }

    /// The full commit hex for an operation-description assertion: the
    /// snapshot only carries the short id, so match by prefix against the
    /// recorded description.
    fn full_commit_hex(snapshot: &RepoSnapshot, short_commit_id: &str) -> String {
        let op = &snapshot.operations[0];
        let hex = op
            .description
            .rsplit(' ')
            .next()
            .unwrap_or_default()
            .to_owned();
        assert!(
            hex.starts_with(short_commit_id),
            "operation \u{201c}{}\u{201d} does not name commit {short_commit_id}",
            op.description
        );
        hex
    }

    #[test]
    #[cfg(unix)]
    fn resolve_conflict_on_the_working_copy_updates_the_disk() {
        let dir = tempfile::tempdir().unwrap();
        let scratch = tempfile::tempdir().unwrap();
        let backend = test_backend();
        let (_ours_id, theirs_id, _child_id) = build_conflicted_repo(dir.path(), &backend);
        install_fake_merge_tool(dir.path(), scratch.path(), "printf 'merged\\n' > \"$4\"", "");

        // Make the conflicted change the working copy: the conflict
        // materializes into the file on disk as marker lines.
        backend.edit_change(dir.path(), &theirs_id).unwrap();
        let on_disk = std::fs::read_to_string(dir.path().join("notes.txt")).unwrap();
        assert!(on_disk.contains("<<<<<<<"), "markers materialized, got: {on_disk}");

        let outcome = backend
            .resolve_conflict(dir.path(), &theirs_id, "notes.txt")
            .unwrap();
        assert!(outcome.operation_id.is_some());

        // The rewritten working copy checked out: the disk has the merge.
        let on_disk = std::fs::read_to_string(dir.path().join("notes.txt")).unwrap();
        assert_eq!(on_disk, "merged\n");
        let after = backend.open(dir.path()).unwrap();
        assert!(after.conflicts.is_empty());
    }

    #[test]
    #[cfg(unix)]
    fn resolve_conflict_parses_markers_left_by_editing_tools() {
        let dir = tempfile::tempdir().unwrap();
        let scratch = tempfile::tempdir().unwrap();
        let backend = test_backend();
        let (_ours_id, theirs_id, _child_id) = build_conflicted_repo(dir.path(), &backend);
        // A tool that edits conflict markers in place: `$output` arrives
        // pre-populated with the materialized conflict; the tool appends a
        // line and leaves the markers, i.e. resolves nothing but changes
        // the file — a partial resolution.
        install_fake_merge_tool(
            dir.path(),
            scratch.path(),
            &format!(
                "cp \"$4\" {s}/got_output; printf 'trailer\\n' >> \"$4\"",
                s = scratch.path().display()
            ),
            "merge-tool-edits-conflict-markers = true\n",
        );

        let outcome = backend
            .resolve_conflict(dir.path(), &theirs_id, "notes.txt")
            .unwrap();
        assert!(outcome.operation_id.is_some());
        assert_eq!(
            outcome.summary,
            format!("Partially resolved notes.txt in {theirs_id} — conflicts remain")
        );

        // The output really was pre-populated with markers, and the change
        // still renders as conflicted afterwards.
        let got = std::fs::read_to_string(scratch.path().join("got_output")).unwrap();
        assert!(got.contains("<<<<<<<"), "pre-populated output, got: {got}");
        let after = backend.open(dir.path()).unwrap();
        assert!(after
            .conflicts
            .iter()
            .any(|c| c.node_id.as_deref() == Some(theirs_id.as_str())
                && c.paths == vec!["notes.txt"]));
    }

    #[test]
    #[cfg(unix)]
    fn resolve_conflict_refusals_record_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let scratch = tempfile::tempdir().unwrap();
        let backend = test_backend();
        let (ours_id, theirs_id, _child_id) = build_conflicted_repo(dir.path(), &backend);
        let ops_before = backend.open(dir.path()).unwrap().operations.len();

        // The tool was closed without completing (nonzero exit).
        install_fake_merge_tool(dir.path(), scratch.path(), "exit 1", "");
        let err = backend
            .resolve_conflict(dir.path(), &theirs_id, "notes.txt")
            .unwrap_err();
        assert!(err.to_string().contains("closed without completing"), "got {err}");

        // The tool exited fine but never wrote the output file.
        install_fake_merge_tool(dir.path(), scratch.path(), "exit 0", "");
        let err = backend
            .resolve_conflict(dir.path(), &theirs_id, "notes.txt")
            .unwrap_err();
        assert!(err.to_string().contains("without saving"), "got {err}");

        // Not a conflict / not a file / immutable target — refused before
        // any tool launches.
        install_fake_merge_tool(dir.path(), scratch.path(), "printf 'x\\n' > \"$4\"", "");
        let err = backend
            .resolve_conflict(dir.path(), &ours_id, "notes.txt")
            .unwrap_err();
        assert!(err.to_string().contains("has no conflict"), "got {err}");
        let err = backend
            .resolve_conflict(dir.path(), &theirs_id, "missing.txt")
            .unwrap_err();
        assert!(err.to_string().contains("does not exist"), "got {err}");
        let snapshot = backend.open(dir.path()).unwrap();
        let base_id = node_by_description(&snapshot, "base: shared notes").id.clone();
        let err = backend
            .resolve_conflict(dir.path(), &base_id, "notes.txt")
            .unwrap_err();
        assert!(matches!(err, BackendError::ImmutableChange(_)), "got {err:?}");

        // Config that cannot run: a bare unknown tool name, and the CLI's
        // builtin TUI editor Jiji cannot host. Both refuse without spawning.
        write_repo_config(dir.path(), "ui.merge-editor = \"sometool\"\n");
        let err = backend
            .resolve_conflict(dir.path(), &theirs_id, "notes.txt")
            .unwrap_err();
        assert!(err.to_string().contains("merge-args"), "got {err}");
        write_repo_config(dir.path(), "ui.merge-editor = \":builtin\"\n");
        let err = backend
            .resolve_conflict(dir.path(), &theirs_id, "notes.txt")
            .unwrap_err();
        assert!(err.to_string().contains("Jiji cannot run"), "got {err}");
        // A broken tool config also hides the snapshot's Resolve affordance
        // instead of failing the snapshot.
        let snapshot = backend.open(dir.path()).unwrap();
        assert_eq!(snapshot.resolve_tool, None);

        let after = backend.open(dir.path()).unwrap();
        assert_eq!(after.operations.len(), ops_before, "no operation recorded");
    }

    #[test]
    #[cfg(unix)]
    fn resolve_conflict_refuses_when_the_file_changed_under_the_tool() {
        let dir = tempfile::tempdir().unwrap();
        let scratch = tempfile::tempdir().unwrap();
        let backend = test_backend();
        let (_ours_id, theirs_id, _child_id) = build_conflicted_repo(dir.path(), &backend);
        backend.edit_change(dir.path(), &theirs_id).unwrap();

        // While the merge window is "open", the user also edits the
        // conflicted file on disk. The post-tool sync snapshots that edit,
        // so the conflict the tool resolved is no longer what the change
        // holds — recording the tool's output would silently discard the
        // manual edit. The CLI would clobber it; Jiji refuses.
        install_fake_merge_tool(
            dir.path(),
            scratch.path(),
            &format!(
                "printf 'merged\\n' > \"$4\"\nprintf 'sneaky edit\\n' >> \"{}\"",
                dir.path().join("notes.txt").display()
            ),
            "",
        );
        let err = backend
            .resolve_conflict(dir.path(), &theirs_id, "notes.txt")
            .unwrap_err();
        assert!(
            err.to_string().contains("changed while the merge tool was open"),
            "got {err}"
        );

        // The manual edit survived — on disk and snapshotted into `@`.
        let on_disk = std::fs::read_to_string(dir.path().join("notes.txt")).unwrap();
        assert!(on_disk.contains("sneaky edit"));
        let after = backend.open(dir.path()).unwrap();
        assert!(after.operations[0].is_snapshot, "the sneaky edit was snapshotted");
        assert!(
            after.conflicts.iter().any(|c| c.node_id.as_deref() == Some(theirs_id.as_str())),
            "the conflict is still recorded"
        );
    }

    #[test]
    fn resolve_conflict_take_theirs_keeps_the_rebased_side() {
        let dir = tempfile::tempdir().unwrap();
        let backend = test_backend();
        let (_ours_id, theirs_id, child_id) = build_conflicted_repo(dir.path(), &backend);
        let before = backend.open(dir.path()).unwrap();
        assert_eq!(before.conflicts.len(), 2, "theirs and its child are conflicted");
        let theirs_commit = node_by_description(&before, "feat: theirs").commit_id.clone();

        // No merge tool needed: taking a side works with nothing configured.
        let outcome = backend
            .resolve_conflict_take(dir.path(), &theirs_id, "notes.txt", ConflictSide::Theirs)
            .unwrap();
        assert!(outcome.operation_id.is_some());
        assert_eq!(
            outcome.summary,
            format!("Resolved notes.txt in {theirs_id} by taking theirs")
        );
        assert_eq!(outcome.target_change.as_deref(), Some(theirs_id.as_str()));

        // Side #2 is the rebased revision's content; the conflict is gone
        // and the child auto-resolved through the descendant rebase.
        let after = backend.open(dir.path()).unwrap();
        assert!(after.conflicts.is_empty(), "got {:?}", after.conflicts);
        for id in [&theirs_id, &child_id] {
            assert!(!after.nodes.iter().find(|n| &n.id == id).unwrap().has_conflict);
        }
        assert_eq!(
            after.operations[0].description,
            format!("Resolve conflicts in commit {}", full_commit_hex(&after, &theirs_commit)),
        );
        let diff = backend.change_diff(dir.path(), &theirs_id).unwrap();
        let file = diff.files.iter().find(|f| f.path == "notes.txt").unwrap();
        assert!(!file.has_conflict);
        let FileDiffContent::Text { hunks, .. } = &file.content else {
            panic!("expected text content");
        };
        let added: Vec<String> = hunks
            .iter()
            .flat_map(|h| &h.lines)
            .filter(|l| l.kind == DiffLineKind::Added)
            .map(|l| l.segments.iter().map(|s| s.text.as_str()).collect())
            .collect();
        assert_eq!(added, vec!["theirs"]);
    }

    #[test]
    fn resolve_conflict_take_ours_updates_the_disk_and_undoes() {
        let dir = tempfile::tempdir().unwrap();
        let backend = test_backend();
        let (_ours_id, theirs_id, _child_id) = build_conflicted_repo(dir.path(), &backend);

        // Make the conflicted change the working copy so the take's effect
        // is visible on disk.
        backend.edit_change(dir.path(), &theirs_id).unwrap();
        let on_disk = std::fs::read_to_string(dir.path().join("notes.txt")).unwrap();
        assert!(on_disk.contains("<<<<<<<"), "markers materialized, got: {on_disk}");

        // Side #1 is the rebase destination's content — the change's own
        // version is discarded, so the file drops out of its diff entirely.
        let outcome = backend
            .resolve_conflict_take(dir.path(), &theirs_id, "notes.txt", ConflictSide::Ours)
            .unwrap();
        assert_eq!(
            outcome.summary,
            format!("Resolved notes.txt in {theirs_id} by taking ours")
        );
        let on_disk = std::fs::read_to_string(dir.path().join("notes.txt")).unwrap();
        assert_eq!(on_disk, "ours\n");
        let after = backend.open(dir.path()).unwrap();
        assert!(after.conflicts.is_empty());
        let diff = backend.change_diff(dir.path(), &theirs_id).unwrap();
        assert!(
            diff.files.iter().all(|f| f.path != "notes.txt"),
            "taking the parent side leaves no diff for the file"
        );

        // The op log undoes the take: the conflict (and its markers on
        // disk) come back.
        let op_id = outcome.operation_id.unwrap();
        backend.revert_operation(dir.path(), &op_id).unwrap();
        let after = backend.open(dir.path()).unwrap();
        assert_eq!(after.conflicts.len(), 2, "got {:?}", after.conflicts);
        assert!(after.nodes.iter().find(|n| n.id == theirs_id).unwrap().has_conflict);
        let on_disk = std::fs::read_to_string(dir.path().join("notes.txt")).unwrap();
        assert!(on_disk.contains("<<<<<<<"), "markers rematerialized, got: {on_disk}");
    }

    #[test]
    fn resolve_conflict_take_refuses_many_sided_conflicts() {
        let dir = tempfile::tempdir().unwrap();
        let backend = test_backend();

        // Three siblings all rewriting the same file, chained by rebases:
        // C ends up carrying a conflict with three sides (A, B, and C).
        let settings = test_settings();
        let (_workspace, repo) =
            pollster::block_on(Workspace::init_simple(&settings, dir.path())).unwrap();
        let store = repo.store().clone();
        let root_commit_id = store.root_commit_id().clone();
        let mut tx = repo.start_transaction();
        let base = write_commit_with_tree(
            &mut tx,
            vec![root_commit_id],
            "base: shared notes",
            file_tree(&store, &[("notes.txt", "base\n")]),
        );
        tx.repo_mut()
            .set_local_bookmark_target(RefName::new("main"), RefTarget::normal(base.id().clone()));
        for (name, content) in [("a", "a\n"), ("b", "b\n"), ("c", "c\n")] {
            write_commit_with_tree(
                &mut tx,
                vec![base.id().clone()],
                &format!("feat: {name}"),
                file_tree(&store, &[("notes.txt", content)]),
            );
        }
        pollster::block_on(tx.commit("set up three siblings")).unwrap();
        let snap = backend.open(dir.path()).unwrap();
        let a_id = node_by_description(&snap, "feat: a").id.clone();
        let b_id = node_by_description(&snap, "feat: b").id.clone();
        let c_id = node_by_description(&snap, "feat: c").id.clone();
        backend.rebase_change(dir.path(), &b_id, &a_id).unwrap();
        backend.rebase_change(dir.path(), &c_id, &b_id).unwrap();
        let before = backend.open(dir.path()).unwrap();
        assert!(
            before.conflicts.iter().any(|c| c.node_id.as_deref() == Some(c_id.as_str())),
            "C is conflicted, got {:?}",
            before.conflicts
        );
        let ops_before = before.operations.len();

        let err = backend
            .resolve_conflict_take(dir.path(), &c_id, "notes.txt", ConflictSide::Ours)
            .unwrap_err();
        assert!(err.to_string().contains("has 3 sides"), "got {err}");

        // The refusals shared with the external-tool path answer the same
        // way without a tool configured: clean changes and immutable ones.
        let err = backend
            .resolve_conflict_take(dir.path(), &a_id, "notes.txt", ConflictSide::Ours)
            .unwrap_err();
        assert!(err.to_string().contains("has no conflict"), "got {err}");
        let base_id = node_by_description(&before, "base: shared notes").id.clone();
        let err = backend
            .resolve_conflict_take(dir.path(), &base_id, "notes.txt", ConflictSide::Theirs)
            .unwrap_err();
        assert!(matches!(err, BackendError::ImmutableChange(_)), "got {err:?}");

        let after = backend.open(dir.path()).unwrap();
        assert_eq!(after.operations.len(), ops_before, "no operation recorded");
    }
}
