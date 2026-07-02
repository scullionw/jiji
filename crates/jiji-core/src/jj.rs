//! jj-lib-backed implementation of `RepoBackend`.
//!
//! Every `open` loads the workspace fresh, reads the repo at the current
//! operation head, and denormalizes it into the UI snapshot DTOs. All jj-lib
//! types stay inside this module; the boundary types in `snapshot` are the
//! only thing that escapes.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::Path;
use std::rc::Rc;
use std::sync::Arc;

use futures::StreamExt as _;
use jj_lib::backend::{CommitId, Timestamp};
use jj_lib::commit::Commit;
use jj_lib::conflicts::{
    materialized_diff_stream, ConflictMarkerStyle, ConflictMaterializeOptions,
    MaterializedTreeValue,
};
use jj_lib::copies::{CopiesTreeDiffEntryPath, CopyOperation, CopyRecords};
use jj_lib::diff_presentation::unified::{git_diff_part, unified_diff_hunks, DiffLineType};
use jj_lib::diff_presentation::{DiffTokenType, LineCompareMode};
use jj_lib::fileset::{self, FilesetAliasesMap, FilesetDiagnostics, FilesetParseContext};
use jj_lib::git::{self, REMOTE_NAME_FOR_LOCAL_GIT_REPO};
use jj_lib::gitignore::{GitIgnoreError, GitIgnoreFile};
use jj_lib::hex_util;
use jj_lib::matchers::{EverythingMatcher, NothingMatcher};
use jj_lib::merge::Diff as MergeDiff;
use jj_lib::object_id::{HexPrefix, ObjectId, PrefixResolution};
use jj_lib::op_store::{OperationId, RefTarget, RemoteRefState, ViewId};
use jj_lib::op_walk;
use jj_lib::operation::Operation;
use jj_lib::ref_name::{RefName, RemoteName, RemoteRefSymbol};
use jj_lib::repo::{ReadonlyRepo, Repo, StoreFactories};
use jj_lib::repo_path::{RepoPath, RepoPathUiConverter};
use jj_lib::revset::{
    self, ResolvedRevsetExpression, RevsetDiagnostics, RevsetExpression, RevsetExtensions,
    RevsetParseContext, RevsetWorkspaceContext, SymbolResolver,
};
use jj_lib::rewrite::{
    move_commits, MoveCommitsLocation, MoveCommitsTarget, RebaseOptions, RewriteRefsOptions,
};
use jj_lib::settings::HumanByteSize;
use jj_lib::store::Store;
use jj_lib::transaction::Transaction;
use jj_lib::working_copy::SnapshotOptions;
use jj_lib::tree_merge::MergeOptions;
use jj_lib::view::View;
use jj_lib::workspace::{default_working_copy_factories, Workspace};

use crate::backend::{validate_repo_path, BackendError, RepoBackend};
use crate::settings::{load_settings, revset_aliases, UserConfigSource};
use crate::snapshot::*;

/// Safety valve for pathological repos: mutable history this deep is almost
/// certainly a misconfigured trunk, and the M0 UI cannot render it anyway.
const MAX_MUTABLE_NODES: usize = 500;
/// Operation log entries included in a snapshot (newest first).
const MAX_OPERATIONS: usize = 50;
/// Effect chips per operation; past this only a count is reported.
const MAX_OP_EFFECTS: usize = 6;
/// `behind_trunk` is informational; stop counting past this.
const MAX_BEHIND_COUNT: usize = 999;
/// Changed-file lists are for inspection, not bulk processing; cap them so a
/// vendored-dependency commit cannot flood the UI.
const MAX_CHANGED_FILES: usize = 1000;
/// Per-side file size limit for content diffs; larger files render as a
/// "too large" placeholder instead of hunks.
const MAX_DIFF_FILE_BYTES: usize = 1 << 20;
/// Total rendered diff lines per change across all files; past this the
/// remaining files keep their list entry but skip content ("omitted").
const MAX_DIFF_TOTAL_LINES: isize = 20_000;
/// Unified-diff context lines, matching jj's default.
const DIFF_CONTEXT_LINES: usize = 3;
/// Linking bases through elided history costs ancestry checks quadratic in
/// the base count; past this many bases the spine is unreadable anyway.
const MAX_LINKED_BASES: usize = 64;

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

    fn load_repo_at_head(&self, path: &Path) -> Result<(Workspace, Arc<ReadonlyRepo>), BackendError> {
        validate_repo_path(path)?;
        let settings = load_settings(path, &self.user_config)?;
        let workspace = Workspace::load(
            &settings,
            path,
            &StoreFactories::default(),
            &default_working_copy_factories(),
        )
        .map_err(|err| BackendError::OpenFailed(err.to_string()))?;
        let repo = pollster::block_on(workspace.repo_loader().load_at_head())
            .map_err(|err| BackendError::OpenFailed(err.to_string()))?;
        Ok((workspace, repo))
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
        let (mut workspace, repo) = self.load_repo_at_head(path)?;
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

/// Whether a rebase brings the change's descendants along (`jj rebase -s`)
/// or moves the change alone (`jj rebase -r`).
#[derive(Clone, Copy, PartialEq, Eq)]
enum RebaseScope {
    WithDescendants,
    Alone,
}

impl RepoBackend for JjBackend {
    fn open(&self, path: &Path) -> Result<RepoSnapshot, BackendError> {
        let (workspace, repo) = self.load_repo_at_head(path)?;
        build_snapshot(&workspace, &repo)
    }

    fn refresh(&self, path: &Path) -> Result<RepoSnapshot, BackendError> {
        let (mut workspace, repo) = self.load_repo_at_head(path)?;
        // A stale workspace cannot sync (mutations keep refusing it), but
        // it must still open read-only — a viewer that errors out is worse
        // than a snapshot that lags the disk. M3 owns the guided recovery.
        let repo = match sync_workspace(&mut workspace, repo.clone()) {
            Ok(repo) => repo,
            Err(BackendError::StaleWorkspace(reason)) => {
                tracing::warn!(reason, "workspace is stale; serving read-only snapshot");
                repo
            }
            Err(err) => return Err(err),
        };
        build_snapshot(&workspace, &repo)
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

    fn describe(
        &self,
        path: &Path,
        change_id: &str,
        description: &str,
    ) -> Result<MutationOutcome, BackendError> {
        let (mut workspace, repo) = self.load_repo_at_head(path)?;
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
        let (mut workspace, repo) = self.load_repo_at_head(path)?;
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
        let (mut workspace, repo) = self.load_repo_at_head(path)?;
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
        let (mut workspace, repo) = self.load_repo_at_head(path)?;
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
        pollster::block_on(tx.repo_mut().rebase_descendants_with_options(&options, |_, _| {}))
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

    fn squash_change(&self, path: &Path, change_id: &str) -> Result<MutationOutcome, BackendError> {
        let (mut workspace, repo) = self.load_repo_at_head(path)?;
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

    fn rebase_change(
        &self,
        path: &Path,
        change_id: &str,
        destination_id: &str,
    ) -> Result<MutationOutcome, BackendError> {
        self.rebase_onto(path, change_id, destination_id, RebaseScope::WithDescendants)
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
        let (mut workspace, repo) = self.load_repo_at_head(path)?;
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
        let (mut workspace, repo) = self.load_repo_at_head(path)?;
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
        let (mut workspace, repo) = self.load_repo_at_head(path)?;
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
        let (mut workspace, repo) = self.load_repo_at_head(path)?;
        let repo = sync_workspace(&mut workspace, repo)?;
        let name = name.trim();
        let target = repo.view().get_local_bookmark(RefName::new(name)).clone();
        if target.is_absent() {
            return Err(BackendError::BookmarkMissing(name.to_owned()));
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

    fn revert_operation(&self, path: &Path, op_id: &str) -> Result<MutationOutcome, BackendError> {
        let (mut workspace, repo) = self.load_repo_at_head(path)?;
        let repo = sync_workspace(&mut workspace, repo)?;
        let bad_op = resolve_operation(&repo, op_id)?;
        let mut parents = bad_op.parents();
        let Some(parent_op) = parents.next().transpose().map_err(mutation_err)? else {
            return Err(BackendError::MutationFailed(
                "the root operation cannot be reverted".into(),
            ));
        };
        if parents.next().is_some() {
            return Err(BackendError::MutationFailed(format!(
                "operation {op_id} merges concurrent operations and cannot be reverted on its own"
            )));
        }
        drop(parents);
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
        let (mut workspace, repo) = self.load_repo_at_head(path)?;
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
}

/// Resolves an operation-id hex prefix — what `OperationItem.id` and
/// `MutationOutcome.operation_id` carry — against the loaded repo.
fn resolve_operation(
    repo: &Arc<ReadonlyRepo>,
    op_id: &str,
) -> Result<Operation, BackendError> {
    op_walk::resolve_op_with_repo(repo, op_id).map_err(|err| match err {
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
        git_head: current.git_head.clone(),
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
) -> Result<RepoSnapshot, BackendError> {
    let repo_ref: &dyn Repo = repo.as_ref();
    let view = repo.view();
    let store = repo.store();

    let wc_commit_id = view.get_wc_commit_id(workspace.workspace_name()).cloned();
    let trunk = resolve_trunk(view);

    // jj's mutable set: everything visible that is not an ancestor of
    // `immutable_heads()` (or the root). The expression comes from the
    // user's revset aliases, so custom immutability config behaves exactly
    // like the CLI — and mutations are gated by the same expression.
    let immutable_heads = immutable_heads_expression(workspace, repo_ref)?;
    let mutable_expr: Arc<ResolvedRevsetExpression> = RevsetExpression::visible_heads()
        .ancestors()
        .minus(&immutable_heads.union(&RevsetExpression::root()).ancestors());
    let mutable_ids = collect_revset(mutable_expr, repo_ref, MAX_MUTABLE_NODES)?;
    let mutable_set: HashSet<CommitId> = mutable_ids.iter().cloned().collect();

    // Topological order, children before parents.
    let mutable_commits: Vec<Commit> = mutable_ids
        .iter()
        .map(|id| store.get_commit(id).map_err(snapshot_err))
        .collect::<Result<_, _>>()?;

    // Immutable commits the mutable work sits on (usually trunk). They give
    // the stack a visual base without dragging in deep history.
    let mut base_ids: Vec<CommitId> = Vec::new();
    let mut seen_bases: HashSet<CommitId> = HashSet::new();
    let add_base = |id: &CommitId, seen: &mut HashSet<CommitId>, out: &mut Vec<CommitId>| {
        if !mutable_set.contains(id) && seen.insert(id.clone()) {
            out.push(id.clone());
        }
    };
    for commit in &mutable_commits {
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
    let mut change_ids: HashMap<CommitId, String> = HashMap::new();
    let mut divergent: HashSet<CommitId> = HashSet::new();
    for commit in mutable_commits.iter().chain(&base_commits) {
        let id = if is_divergent(repo_ref, commit) {
            divergent.insert(commit.id().clone());
            short_commit_id(repo_ref, commit.id())
        } else {
            short_change_id(repo_ref, commit)
        };
        change_ids.insert(commit.id().clone(), id);
    }

    let elided_links = elided_base_links(repo_ref, &base_commits, &change_ids);

    let nodes: Vec<GraphNode> = mutable_commits
        .iter()
        .map(|commit| (commit, wc_node_kind(commit.id(), &wc_commit_id)))
        .chain(base_commits.iter().map(|commit| {
            let kind = match wc_node_kind(commit.id(), &wc_commit_id) {
                NodeKind::WorkingCopy => NodeKind::WorkingCopy,
                _ => NodeKind::Immutable,
            };
            (commit, kind)
        }))
        .map(|(commit, kind)| {
            graph_node(
                repo_ref,
                view,
                commit,
                kind,
                &trunk,
                &change_ids,
                &divergent,
                &elided_links,
            )
        })
        .collect();

    let workstreams = build_workstreams(
        repo_ref,
        view,
        &mutable_commits,
        &mutable_set,
        &wc_commit_id,
        &trunk,
        &change_ids,
    );

    let mut conflicts = file_conflicts(&mutable_commits, &wc_commit_id, &change_ids);
    let bookmarks = build_bookmarks(repo_ref, &trunk, &mut conflicts, &mut change_ids)?;
    let operations = build_operations(repo)?;

    let workspaces = view
        .wc_commit_ids()
        .iter()
        .map(|(name, commit_id)| WorkspaceSummary {
            name: name.as_str().to_owned(),
            is_default: name.as_str() == "default",
            // Staleness needs per-workspace working-copy inspection; deferred.
            is_stale: false,
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
        trunk_bookmark: trunk.as_ref().map(|t| t.name.clone()).unwrap_or_default(),
        working_copy: wc_commit_id
            .as_ref()
            .and_then(|id| change_ids.get(id).cloned())
            .unwrap_or_default(),
        workspaces,
        workstreams,
        nodes,
        bookmarks,
        conflicts,
        operations,
    })
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
        let commit_id = match repo
            .as_ref()
            .resolve_change_id_prefix(&prefix)
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
        let targets = match repo
            .as_ref()
            .resolve_change_id_prefix(&prefix)
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
    let visible = repo
        .as_ref()
        .resolve_change_id(commit.change_id())
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
    match repo
        .index()
        .resolve_commit_id_prefix(&prefix)
        .map_err(snapshot_err)?
    {
        PrefixResolution::SingleMatch(commit_id) => Ok(Some(commit_id)),
        _ => Ok(None),
    }
}

/// Parses and resolves the user's `immutable_heads()` revset alias. This is
/// the immutability boundary: the snapshot's mutable set and every mutation
/// gate evaluate the same expression, so broken config is a hard error
/// rather than a silent fall back to defaults the user overrode.
fn immutable_heads_expression(
    workspace: &Workspace,
    repo: &dyn Repo,
) -> Result<Arc<ResolvedRevsetExpression>, BackendError> {
    let settings = workspace.settings();
    let aliases = revset_aliases(settings);
    let path_converter = RepoPathUiConverter::Fs {
        cwd: workspace.workspace_root().to_owned(),
        base: workspace.workspace_root().to_owned(),
    };
    let fileset_aliases = FilesetAliasesMap::new();
    let extensions = RevsetExtensions::default();
    let context = RevsetParseContext {
        aliases_map: &aliases,
        local_variables: HashMap::new(),
        user_email: settings.user_email(),
        date_pattern_context: chrono::Local::now().into(),
        default_ignored_remote: Some(REMOTE_NAME_FOR_LOCAL_GIT_REPO),
        fileset_aliases_map: &fileset_aliases,
        use_glob_by_default: true,
        extensions: &extensions,
        workspace: Some(RevsetWorkspaceContext {
            path_converter: &path_converter,
            workspace_name: workspace.workspace_name(),
        }),
    };
    let config_err = |err: &dyn std::fmt::Display| {
        BackendError::ConfigInvalid(format!("revset-aliases.\"immutable_heads()\": {err}"))
    };
    let parsed = revset::parse(&mut RevsetDiagnostics::new(), "immutable_heads()", &context)
        .map_err(|err| config_err(&err))?;
    let symbol_resolver = SymbolResolver::new(repo, extensions.symbol_resolvers());
    parsed
        .resolve_user_expression(repo, &symbol_resolver)
        .map_err(|err| config_err(&err))
}

/// Refuses to rewrite commits jj considers immutable, using the same
/// `immutable_heads()` expression the snapshot renders.
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
    match revset.iter().next() {
        None => Ok(()),
        Some(Ok(_)) => Err(BackendError::ImmutableChange(display_id.to_owned())),
        Some(Err(err)) => Err(snapshot_err(err)),
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
            git::reset_head(tx.repo_mut(), &new_wc).map_err(mutation_err)?;
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
    git::get_git_backend(store)
        .ok()
        .and_then(|backend| backend.git_workdir())
        .is_some_and(|workdir| workdir == workspace.workspace_root())
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
    let mut tx = repo.start_transaction();
    git::import_head(tx.repo_mut()).map_err(mutation_err)?;
    if !tx.repo().has_changes() {
        return Ok(repo);
    }
    let new_git_head = tx.repo().view().git_head().clone();
    let Some(new_git_head_id) = new_git_head.as_normal().cloned() else {
        // HEAD became unborn or conflicted; record the import as-is.
        return pollster::block_on(tx.commit("import git head")).map_err(mutation_err);
    };
    let ws_name = workspace.workspace_name().to_owned();
    let new_git_head_commit = tx
        .repo()
        .store()
        .get_commit(&new_git_head_id)
        .map_err(mutation_err)?;
    pollster::block_on(tx.repo_mut().check_out(ws_name, &new_git_head_commit))
        .map_err(mutation_err)?;
    let mut locked_ws = workspace
        .start_working_copy_mutation()
        .map_err(mutation_err)?;
    pollster::block_on(locked_ws.locked_wc().reset(&new_git_head_commit))
        .map_err(mutation_err)?;
    pollster::block_on(tx.repo_mut().rebase_descendants()).map_err(mutation_err)?;
    let new_repo = pollster::block_on(tx.commit("import git head")).map_err(mutation_err)?;
    locked_ws
        .finish(new_repo.op_id().clone())
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
    let settings = workspace.settings();
    let config_err = |name: &str, err: &dyn std::fmt::Display| {
        BackendError::ConfigInvalid(format!("{name}: {err}"))
    };
    let options = git::GitImportOptions {
        auto_local_bookmark: settings
            .get_bool("git.auto-local-bookmark")
            .map_err(|err| config_err("git.auto-local-bookmark", &err))?,
        abandon_unreachable_commits: settings
            .get_bool("git.abandon-unreachable-commits")
            .map_err(|err| config_err("git.abandon-unreachable-commits", &err))?,
        // Per-remote auto-tracking is newer than the jj-lib we build
        // against; bookmarks new to jj follow `git.auto-local-bookmark`.
        remote_auto_track_bookmarks: HashMap::new(),
    };
    let mut tx = repo.start_transaction();
    let stats = git::import_refs(tx.repo_mut(), &options).map_err(mutation_err)?;
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

    let mut locked_ws = workspace
        .start_working_copy_mutation()
        .map_err(mutation_err)?;
    // The on-disk working copy must agree with the repo we loaded: same
    // operation, or at least the same tree (the recorded operation lags
    // behind after mutations that never touched `@`). Anything else means
    // another client moved the checkout — bail instead of mixing states.
    if locked_ws.locked_wc().old_operation_id() != repo.op_id()
        && locked_ws.locked_wc().old_tree().tree_ids() != wc_commit.tree_ids()
    {
        return Err(BackendError::StaleWorkspace(
            "the working copy is checked out at a different operation; \
             run a jj command in this workspace to update it first"
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
    locked_ws
        .finish(repo.op_id().clone())
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
        .trusted_path("core.excludesFile")
        .and_then(|path| path.ok())
        .map(|path| path.into_owned())
        .or_else(|| {
            let base = std::env::var_os("XDG_CONFIG_HOME")
                .map(std::path::PathBuf::from)
                .filter(|p| p.is_absolute())
                .or_else(|| std::env::home_dir().map(|home| home.join(".config")))?;
            Some(base.join("git").join("ignore"))
        });
    if let Some(path) = excludes_file {
        ignores = ignores.chain_with_file("", path)?;
    }
    ignores.chain_with_file(
        "",
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
        records.add_records(collected).map_err(snapshot_err)?;
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
            files.push(ChangedFile {
                path: entry.path.target().as_internal_file_string().to_owned(),
                status,
                renamed_from,
                has_conflict: !values.after.is_resolved(),
            });
        }
        Ok::<_, BackendError>(())
    })?;
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
    copy_records.add_records(collected).map_err(snapshot_err)?;
    diff_trees(
        repo,
        to_change_id.to_owned(),
        Some(from_change_id.to_owned()),
        from.tree(),
        to.tree(),
        copy_records,
    )
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
    let mut truncated = false;
    let mut line_budget = MAX_DIFF_TOTAL_LINES;
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
                file_diff_content(path, values, &materialize_options, &mut line_budget)?
            };
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

/// Diff one file's materialized sides into renderable content, charging
/// emitted lines against the change-wide budget.
fn file_diff_content(
    path: &RepoPath,
    values: MergeDiff<MaterializedTreeValue>,
    options: &ConflictMaterializeOptions,
    line_budget: &mut isize,
) -> Result<FileDiffContent, BackendError> {
    let left = git_diff_part(path, values.before, options).map_err(snapshot_err)?;
    let right = git_diff_part(path, values.after, options).map_err(snapshot_err)?;
    if left.content.is_binary || right.content.is_binary {
        return Ok(FileDiffContent::Binary);
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
        hunks.push(DiffHunk {
            old_start: hunk.left_line_range.start as u32 + 1,
            new_start: hunk.right_line_range.start as u32 + 1,
            lines: hunk
                .lines
                .iter()
                .take(take)
                .map(|(line_type, tokens)| DiffLine {
                    kind: match line_type {
                        DiffLineType::Context => DiffLineKind::Context,
                        DiffLineType::Removed => DiffLineKind::Removed,
                        DiffLineType::Added => DiffLineKind::Added,
                    },
                    segments: to_segments(tokens),
                })
                .collect(),
        });
    }
    Ok(FileDiffContent::Text {
        hunks,
        truncated: hunks_truncated,
    })
}

/// Collapse a line's diff tokens into contiguous changed/unchanged segments
/// of lossy UTF-8 text, without the trailing newline.
fn to_segments(tokens: &[(DiffTokenType, &[u8])]) -> Vec<DiffSegment> {
    let mut segments: Vec<DiffSegment> = Vec::new();
    for (token_type, text) in tokens {
        let changed = matches!(token_type, DiffTokenType::Different);
        let text = String::from_utf8_lossy(text);
        match segments.last_mut() {
            Some(last) if last.changed == changed => last.text.push_str(&text),
            _ => segments.push(DiffSegment {
                text: text.into_owned(),
                changed,
            }),
        }
    }
    if let Some(last) = segments.last_mut() {
        if last.text.ends_with('\n') {
            last.text.pop();
        }
        if last.text.is_empty() {
            segments.pop();
        }
    }
    segments
}

/// Mirrors jj's default `trunk()` alias: a main/master/trunk bookmark on the
/// preferred remote, falling back to a local bookmark of the same names.
fn resolve_trunk(view: &View) -> Option<TrunkRef> {
    let mut remotes: Vec<&RemoteName> = view
        .remote_views()
        .map(|(name, _)| name)
        .filter(|name| *name != REMOTE_NAME_FOR_LOCAL_GIT_REPO)
        .collect();
    remotes.sort_by_key(|name| (remote_rank(name.as_str()), name.as_str().to_owned()));
    for remote in remotes {
        for candidate in TRUNK_CANDIDATES {
            let symbol = RemoteRefSymbol {
                name: RefName::new(candidate),
                remote,
            };
            if let Some(id) = view.get_remote_bookmark(symbol).target.added_ids().next() {
                return Some(TrunkRef {
                    name: candidate.to_owned(),
                    remote: Some(remote.as_str().to_owned()),
                    target: id.clone(),
                });
            }
        }
    }
    for candidate in TRUNK_CANDIDATES {
        let target = view.get_local_bookmark(RefName::new(candidate));
        if let Some(id) = target.added_ids().next() {
            return Some(TrunkRef {
                name: candidate.to_owned(),
                remote: None,
                target: id.clone(),
            });
        }
    }
    None
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
    for id in revset.iter().take(limit) {
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
    trunk: &Option<TrunkRef>,
    change_ids: &HashMap<CommitId, String>,
    divergent: &HashSet<CommitId>,
    elided_links: &HashMap<CommitId, Vec<String>>,
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
    // matters for the mutable part of the graph, so bases skip the extra
    // tree loads.
    let is_base = kind == NodeKind::Immutable;
    let parents = commit
        .parent_ids()
        .iter()
        .filter_map(|id| change_ids.get(id).cloned())
        .collect();
    let is_empty = !is_base && commit.is_empty(repo).unwrap_or(false);

    let author = commit.author();
    let author_name = if !author.name.is_empty() {
        author.name.clone()
    } else {
        author.email.split('@').next().unwrap_or_default().to_owned()
    };

    GraphNode {
        id: change_ids
            .get(commit.id())
            .cloned()
            .unwrap_or_else(|| display_id(repo, commit)),
        change_id: short_change_id(repo, commit),
        commit_id: short_commit_id(repo, commit.id()),
        description: commit.description().trim().to_owned(),
        author: author_name,
        timestamp: format_timestamp(&commit.committer().timestamp),
        kind,
        parents,
        elided_parents: elided_links.get(commit.id()).cloned().unwrap_or_default(),
        bookmarks,
        is_empty,
        has_conflict: commit.has_conflict(),
        is_divergent: divergent.contains(commit.id()),
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
            .iter()
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

    for (name, targets) in view.bookmarks() {
        let local_target = targets.local_target;
        if local_target.is_absent() {
            continue;
        }
        if local_target.has_conflict() {
            let targets = local_target
                .added_ids()
                .map(|id| display_id_for(repo, id, change_ids))
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
            });
        }
        let Some(local_id) = local_target.added_ids().next() else {
            continue;
        };

        let tracked = targets
            .remote_refs
            .iter()
            .filter(|(remote, remote_ref)| {
                *remote != REMOTE_NAME_FOR_LOCAL_GIT_REPO
                    && remote_ref.state == RemoteRefState::Tracked
                    && remote_ref.target.is_present()
            })
            .min_by_key(|(remote, _)| (remote_rank(remote.as_str()), remote.as_str().to_owned()));

        let (remote, sync) = match tracked {
            None => (None, SyncState::LocalOnly),
            Some((remote, remote_ref)) => {
                let sync = classify_sync(repo, local_id, remote_ref.target.added_ids().next());
                (Some(remote.as_str().to_owned()), sync)
            }
        };

        bookmarks.push(BookmarkState {
            name: name.as_str().to_owned(),
            target: display_id_for(repo, local_id, change_ids)?,
            remote,
            sync,
            is_trunk: Some(name.as_str()) == trunk_name,
            is_local: true,
        });
    }

    // A trunk that only exists as a remote bookmark (no local counterpart)
    // still needs an entry: the UI finds the trunk node through it.
    if let Some(trunk) = trunk {
        if !bookmarks.iter().any(|b| b.is_trunk) {
            bookmarks.push(BookmarkState {
                name: trunk.name.clone(),
                target: display_id_for(repo, &trunk.target, change_ids)?,
                remote: trunk.remote.clone(),
                sync: SyncState::Synced,
                is_trunk: true,
                is_local: false,
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
    repo.index().is_ancestor(ancestor, descendant).unwrap_or(false)
}

fn build_operations(repo: &Arc<ReadonlyRepo>) -> Result<Vec<OperationItem>, BackendError> {
    let store = repo.store();
    let head_op = repo.operation().clone();
    // Adjacent ops in a linear history share one view between them; memoize
    // the extracted summaries so each view loads once.
    let mut summaries: HashMap<ViewId, Rc<OpViewSummary>> = HashMap::new();
    let mut operations = Vec::new();
    for op in op_walk::walk_ancestors(&[head_op]).take(MAX_OPERATIONS) {
        let op = op.map_err(snapshot_err)?;
        // Effects only make sense against a single parent; the root op and
        // merges of divergent operations report none.
        let (effects, more_effects) = if op.parent_ids().len() == 1 {
            let parent = op
                .parents()
                .next()
                .expect("single-parent op")
                .map_err(snapshot_err)?;
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
            Some(before_id) if before_id != after_id => {
                if change_keys(store, std::slice::from_ref(before_id))
                    != change_keys(store, std::slice::from_ref(after_id))
                {
                    effects.push(wc_label(name, "moved"));
                }
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
    let len = repo
        .shortest_unique_change_id_prefix_len(change_id)
        .unwrap_or(8)
        .max(8);
    let full = hex_util::encode_reverse_hex(change_id.as_bytes());
    full[..len.min(full.len())].to_owned()
}

/// jj's `??` state: several visible commits share this commit's change id.
/// Index errors report as not-divergent — anything genuinely broken fails
/// the surrounding revset evaluation first.
fn is_divergent(repo: &dyn Repo, commit: &Commit) -> bool {
    repo.resolve_change_id(commit.change_id())
        .ok()
        .flatten()
        .is_some_and(|targets| targets.is_divergent())
}

/// The id the UI addresses this commit by (`GraphNode::id`): its short
/// change id, or its short commit id when the change is divergent — a
/// divergent change id names several commits, so like the CLI Jiji falls
/// back to commit ids for the individual copies.
fn display_id(repo: &dyn Repo, commit: &Commit) -> String {
    if is_divergent(repo, commit) {
        short_commit_id(repo, commit.id())
    } else {
        short_change_id(repo, commit)
    }
}

fn display_id_for(
    repo: &dyn Repo,
    commit_id: &CommitId,
    change_ids: &mut HashMap<CommitId, String>,
) -> Result<String, BackendError> {
    if let Some(existing) = change_ids.get(commit_id) {
        return Ok(existing.clone());
    }
    let commit = repo.store().get_commit(commit_id).map_err(snapshot_err)?;
    let short = display_id(repo, &commit);
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
    let len = repo
        .index()
        .shortest_unique_commit_id_prefix_len(commit_id)
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
        assert_eq!(&wc_node.parents, &[feature_id.clone()]);

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
            &StoreFactories::default(),
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

        // Null bytes mark a file binary.
        assert_eq!(by_path["logo.png"].content, FileDiffContent::Binary);

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

    /// The on-disk working-copy state must track the operation a mutation
    /// created, or the next CLI command would report a stale workspace.
    fn assert_workspace_fresh(root: &Path) {
        let settings = test_settings();
        let workspace = Workspace::load(
            &settings,
            root,
            &StoreFactories::default(),
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
            &StoreFactories::default(),
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
            &StoreFactories::default(),
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
            &StoreFactories::default(),
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
        let ours = write_commit_with_tree(
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
        // no entry for it — but its tree carries the unresolved conflict.
        let child_item = by_node(&child_id);
        assert_eq!(child_item.paths, vec!["notes.txt"]);
        let child_diff = backend.change_diff(dir.path(), &child_id).unwrap();
        assert!(child_diff.files.iter().all(|f| f.path != "notes.txt"));
        for id in [&theirs_id, &child_id] {
            assert!(after.nodes.iter().find(|n| &n.id == id).unwrap().has_conflict);
        }
        assert!(!after.nodes.iter().find(|n| n.id == ours_id).unwrap().has_conflict);
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
            &StoreFactories::default(),
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
            &StoreFactories::default(),
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

    #[test]
    fn refresh_degrades_to_read_only_when_workspace_is_stale() {
        let dir = tempfile::tempdir().unwrap();
        build_test_repo(dir.path());
        let backend = test_backend();

        // Rewrite `@`'s tree out-of-band without updating the on-disk
        // working-copy state — what a mutation from another workspace's
        // client looks like to this one.
        let settings = test_settings();
        let workspace = Workspace::load(
            &settings,
            dir.path(),
            &StoreFactories::default(),
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

        // Refresh still answers — read-only, without syncing — while
        // mutations keep refusing until the workspace is updated.
        let snapshot = backend.refresh(dir.path()).unwrap();
        assert_eq!(snapshot.operations[0].description, "rewrite from elsewhere");
        let err = backend
            .describe(dir.path(), &snapshot.working_copy, "nope")
            .unwrap_err();
        assert!(matches!(err, BackendError::StaleWorkspace(_)), "got {err:?}");
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
        pollster::block_on(Workspace::init_colocated_git(&settings, root)).unwrap();
        std::fs::write(root.join("file.txt"), "one\n").unwrap();
        backend.refresh(root).unwrap();
        let first = backend.refresh(root).unwrap().working_copy;
        backend.describe(root, &first, "first change").unwrap();
        backend.create_bookmark(root, "main", &first).unwrap();
        backend.new_change(root, &first).unwrap();
        first
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
}
