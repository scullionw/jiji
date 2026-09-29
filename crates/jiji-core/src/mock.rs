//! Deterministic mock snapshot used while the jj-lib backend is pending.
//!
//! The data is intentionally shaped like a real session: one active stack
//! with a working copy on top, a compact sibling workstream, trunk context,
//! bookmarks in different sync states, and a short operation log.

use std::path::Path;

use crate::backend::BackendError;
use crate::snapshot::*;

#[allow(clippy::too_many_arguments)]
fn node(
    id: &str,
    commit_id: &str,
    description: &str,
    author: &str,
    timestamp: &str,
    kind: NodeKind,
    parents: &[&str],
    bookmarks: &[&str],
) -> GraphNode {
    GraphNode {
        id: id.into(),
        change_id: id.into(),
        commit_id: commit_id.into(),
        description: description.into(),
        author: author.into(),
        timestamp: timestamp.into(),
        kind,
        parents: parents.iter().map(|p| p.to_string()).collect(),
        elided_parents: Vec::new(),
        bookmarks: bookmarks.iter().map(|b| b.to_string()).collect(),
        tags: Vec::new(),
        is_empty: false,
        has_conflict: false,
        is_divergent: false,
    }
}

/// One visible commit of a divergent change: keyed by its commit id (like
/// the real backend), sharing `change_id` with its sibling copies.
fn divergent_node(
    change_id: &str,
    commit_id: &str,
    description: &str,
    timestamp: &str,
    parents: &[&str],
) -> GraphNode {
    GraphNode {
        id: commit_id.into(),
        change_id: change_id.into(),
        is_divergent: true,
        ..node(
            change_id,
            commit_id,
            description,
            "lauf",
            timestamp,
            NodeKind::Mutable,
            parents,
            &[],
        )
    }
}

/// A mutation `MockBackend` remembers, replayed over the fabricated
/// snapshot on every open so refreshes reproduce the same state.
#[derive(Debug, Clone)]
pub enum MockMutation {
    Describe { id: String, text: String },
    New { parent: String },
    Edit { id: String },
    Abandon { id: String },
    AbandonMany { ids: Vec<String> },
    Squash { id: String },
    Split { id: String, selection: Vec<SplitSelection>, description: String },
    SquashInto { id: String, selection: Vec<SplitSelection>, destination: String },
    Rebase { id: String, destination: String, with_descendants: bool },
    CreateBookmark { name: String, target: String },
    MoveBookmark { name: String, target: String },
    RenameBookmark { old: String, new: String },
    DeleteBookmark { name: String },
    TrackBookmark { name: String, remote: String },
    UntrackBookmark { name: String, remote: String },
    DeleteRemoteBookmark { name: String, remote: String },
    Push { names: Vec<String>, remote: String },
    FetchPr { number: u64, bookmark: String },
    RevertOp { op_id: String },
    RestoreOp { op_id: String },
    Resolve { id: String, path: String, side: Option<ConflictSide> },
    RestoreFile { id: String, path: String, hunks: Option<Vec<SplitHunk>> },
    RestoreVersion { id: String, commit: String },
    Duplicate { id: String },
    Revert { id: String },
    Absorb { id: String },
}

impl MockMutation {
    /// Revert/restore entries time-travel other entries instead of touching
    /// the graph themselves.
    pub(crate) fn is_time_travel(&self) -> bool {
        matches!(
            self,
            MockMutation::RevertOp { .. } | MockMutation::RestoreOp { .. }
        )
    }
}

/// Deterministic ids per mutation index, so refreshes reproduce them.
fn mock_operation_id(index: usize) -> String {
    format!("ad{index:02}e5c4be00")
}

/// The inverse of `mock_operation_id`: which remembered mutation recorded
/// this operation, if any (fixture operations parse as `None`).
fn mock_mutation_index(op_id: &str) -> Option<usize> {
    let digits = op_id.strip_prefix("ad")?.strip_suffix("e5c4be00")?;
    if digits.len() != 2 {
        return None;
    }
    digits.parse().ok()
}

/// Which remembered mutations still apply after replaying revert/restore
/// entries — the mock's model of jj's op-level time travel: a revert
/// disables one entry, a restore rolls the whole set back to how it stood
/// right after the target entry (fixture ops roll back to "none").
pub fn active_effects(mutations: &[MockMutation]) -> Vec<bool> {
    fn state_after(mutations: &[MockMutation], upto: usize) -> Vec<bool> {
        let mut active = vec![false; mutations.len()];
        for i in 0..upto {
            match &mutations[i] {
                MockMutation::RevertOp { op_id } => {
                    if let Some(j) = mock_mutation_index(op_id) {
                        if j < i {
                            active[j] = false;
                        }
                    }
                }
                MockMutation::RestoreOp { op_id } => {
                    active = match mock_mutation_index(op_id) {
                        Some(j) if j < i => state_after(mutations, j + 1),
                        _ => vec![false; mutations.len()],
                    };
                }
                _ => active[i] = true,
            }
        }
        active
    }
    state_after(mutations, mutations.len())
}

/// How revert/restore name an operation in summaries: its description when
/// it has one, the id otherwise — matching the real backend.
fn op_label(op: &OperationItem) -> String {
    if op.description.is_empty() {
        format!("operation {}", op.id)
    } else {
        format!("\u{201c}{}\u{201d}", op.description)
    }
}

fn mock_new_change_id(index: usize) -> String {
    format!("wn{index:02}pqzu")
}

/// Commit id of the version a replayed restore fabricates (forward hex,
/// like every mock commit id; deterministic per mutation index).
pub(crate) fn mock_version_commit_id(index: usize) -> String {
    format!("e5{index:02}c0a4")
}

/// Resolve a restore-fabricated version commit id back to the fixture
/// version it restored, so version comparisons keep answering after a
/// restore grows the chain (the fabricated version's tree IS the source
/// version's). Ids no restore fabricated pass through untouched.
pub(crate) fn resolve_version_commit(mutations: &[MockMutation], id: &str) -> String {
    for (index, mutation) in mutations.iter().enumerate() {
        if let MockMutation::RestoreVersion { commit, .. } = mutation {
            if mock_version_commit_id(index) == id {
                return commit.clone();
            }
        }
    }
    id.to_owned()
}

fn mock_op_timestamp(index: usize) -> String {
    format!("2026-06-10T13:{:02}:00Z", (index + 1).min(59))
}

/// Validates a mutation against the overlaid snapshot — mirroring the real
/// backend's refusals — and produces the breadcrumb it will record.
/// `operation_id: None` marks a no-op that should not be remembered.
/// `prior` is the already-remembered list (the entries `snapshot` replays);
/// revert/restore validate against it.
pub fn mutation_outcome(
    snapshot: &RepoSnapshot,
    prior: &[MockMutation],
    mutation: &MockMutation,
    index: usize,
) -> Result<MutationOutcome, BackendError> {
    let find = |id: &str| {
        snapshot
            .nodes
            .iter()
            .find(|n| n.id == id)
            .ok_or_else(|| BackendError::ChangeMissing(id.to_owned()))
    };
    let require_mutable = |node: &GraphNode| {
        if node.kind == NodeKind::Immutable {
            Err(BackendError::ImmutableChange(node.id.clone()))
        } else {
            Ok(())
        }
    };
    let recorded = |summary: String, target: &str| MutationOutcome {
        operation_id: Some(mock_operation_id(index)),
        summary,
        target_change: Some(target.to_owned()),
    };
    match mutation {
        MockMutation::Describe { id, text } => {
            let node = find(id)?;
            require_mutable(node)?;
            if node.description == *text {
                return Ok(MutationOutcome {
                    operation_id: None,
                    summary: format!("{id} already has this description"),
                    target_change: Some(id.clone()),
                });
            }
            Ok(recorded(format!("Described {id}"), id))
        }
        MockMutation::New { parent } => {
            find(parent)?;
            let new_id = mock_new_change_id(index);
            Ok(recorded(format!("Started {new_id} on {parent}"), &new_id))
        }
        MockMutation::Edit { id } => {
            let node = find(id)?;
            if snapshot.working_copy == *id {
                return Ok(MutationOutcome {
                    operation_id: None,
                    summary: format!("{id} is already the working copy"),
                    target_change: Some(id.clone()),
                });
            }
            require_mutable(node)?;
            Ok(recorded(format!("Editing {id}"), id))
        }
        MockMutation::Abandon { id } => {
            let node = find(id)?;
            require_mutable(node)?;
            let parent = node.parents.first().cloned().unwrap_or_default();
            Ok(recorded(format!("Abandoned {id}"), &parent))
        }
        MockMutation::AbandonMany { ids } => {
            let mut parent = String::new();
            for id in ids {
                let node = find(id)?;
                require_mutable(node)?;
                // Like the real backend: the selection lands on the first
                // parent outside the abandoned set (the last id's parent
                // when the ids walk a chain top-down).
                if let Some(p) = node.parents.iter().find(|p| !ids.contains(p)) {
                    parent = p.clone();
                }
            }
            Ok(recorded(format!("Abandoned {}", ids.join(", ")), &parent))
        }
        MockMutation::Squash { id } => {
            let node = find(id)?;
            require_mutable(node)?;
            let [parent_id] = node.parents.as_slice() else {
                return Err(BackendError::MutationFailed(format!(
                    "{id} is a merge; squashing into multiple parents is ambiguous"
                )));
            };
            let parent = find(parent_id)?;
            require_mutable(parent)?;
            Ok(recorded(format!("Squashed {id} into {parent_id}"), parent_id))
        }
        MockMutation::Split { id, selection, .. } => {
            let node = find(id)?;
            require_mutable(node)?;
            // The change's own diff decides what a selection can cover.
            // Mutation-created nodes have no fixture files, so they refuse
            // as "nothing selected" — same as the real backend on an empty
            // change. Hunk entries count as partially kept: their file
            // still leaves its unchosen hunks with the remainder, so only
            // an all-whole-files selection can cover everything (the hunk
            // coordinates themselves are not validated — fixture diffs are
            // static, a documented mock approximation).
            let files: Vec<String> = mock_change_detail(id)
                .map(|detail| detail.files.into_iter().map(|f| f.path).collect())
                .unwrap_or_default();
            let whole = files
                .iter()
                .filter(|f| {
                    selection
                        .iter()
                        .any(|s| s.path == **f && s.hunks.is_none())
                })
                .count();
            let partial = files
                .iter()
                .filter(|f| {
                    selection
                        .iter()
                        .any(|s| s.path == **f && s.hunks.is_some())
                })
                .count();
            if whole + partial == 0 {
                return Err(BackendError::MutationFailed(format!(
                    "none of the selected files change anything in {id}"
                )));
            }
            if whole == files.len() {
                return Err(BackendError::MutationFailed(format!(
                    "the selection covers every change in {id}; \
                     there would be nothing left to split off"
                )));
            }
            let kept = match (whole, partial) {
                (w, 0) => format!("{w} file{}", if w == 1 { "" } else { "s" }),
                (0, p) => format!("parts of {p} file{}", if p == 1 { "" } else { "s" }),
                (w, p) => {
                    format!("{w} file{} and parts of {p} more", if w == 1 { "" } else { "s" })
                }
            };
            let new_id = mock_new_change_id(index);
            Ok(recorded(
                format!("Split {id}: kept {kept}, the rest moved to {new_id}"),
                id,
            ))
        }
        MockMutation::SquashInto { id, selection, destination } => {
            let node = find(id)?;
            require_mutable(node)?;
            if id == destination {
                return Err(BackendError::MutationFailed(format!(
                    "cannot move changes from {id} into itself"
                )));
            }
            let dest = find(destination)?;
            require_mutable(dest)?;
            // The counting mirrors Split's: fixture files decide what a
            // selection can cover, hunk entries count as partially moved
            // (their unchosen hunks stay behind), and hunk coordinates go
            // unvalidated — fixture diffs are static, the documented mock
            // approximation.
            let files: Vec<String> = mock_change_detail(id)
                .map(|detail| detail.files.into_iter().map(|f| f.path).collect())
                .unwrap_or_default();
            let whole = files
                .iter()
                .filter(|f| {
                    selection
                        .iter()
                        .any(|s| s.path == **f && s.hunks.is_none())
                })
                .count();
            let partial = files
                .iter()
                .filter(|f| {
                    selection
                        .iter()
                        .any(|s| s.path == **f && s.hunks.is_some())
                })
                .count();
            if whole + partial == 0 {
                return Err(BackendError::MutationFailed(format!(
                    "none of the selected files change anything in {id}"
                )));
            }
            if whole == files.len() {
                // The full squash: the emptied source is abandoned and the
                // selection follows the destination.
                return Ok(recorded(
                    format!(
                        "Moved everything in {id} into {destination}; \
                         the emptied change was abandoned"
                    ),
                    destination,
                ));
            }
            let moved = match (whole, partial) {
                (w, 0) => format!("{w} file{}", if w == 1 { "" } else { "s" }),
                (0, p) => format!("parts of {p} file{}", if p == 1 { "" } else { "s" }),
                (w, p) => {
                    format!("{w} file{} and parts of {p} more", if w == 1 { "" } else { "s" })
                }
            };
            Ok(recorded(format!("Moved {moved} from {id} into {destination}"), id))
        }
        MockMutation::Rebase {
            id,
            destination,
            with_descendants,
        } => {
            let node = find(id)?;
            require_mutable(node)?;
            find(destination)?;
            if id == destination {
                return Err(BackendError::MutationFailed(format!(
                    "cannot rebase {id} onto itself"
                )));
            }
            if *with_descendants && is_mock_ancestor(snapshot, id, destination) {
                return Err(BackendError::MutationFailed(format!(
                    "cannot rebase {id} onto its own descendant {destination}"
                )));
            }
            let already_in_place = node.parents.as_slice() == [destination.clone()];
            let descendants = mock_descendant_count(snapshot, id);
            if already_in_place && (*with_descendants || descendants == 0) {
                return Ok(MutationOutcome {
                    operation_id: None,
                    summary: format!("{id} is already on {destination}"),
                    target_change: Some(id.clone()),
                });
            }
            let summary = if *with_descendants {
                if descendants == 0 {
                    format!("Rebased {id} onto {destination}")
                } else {
                    format!(
                        "Rebased {id} and {descendants} descendant{} onto {destination}",
                        if descendants == 1 { "" } else { "s" }
                    )
                }
            } else if already_in_place {
                format!(
                    "Moved {descendants} descendant{} of {id} onto {destination}",
                    if descendants == 1 { "" } else { "s" }
                )
            } else {
                format!("Moved {id} onto {destination}")
            };
            Ok(recorded(summary, id))
        }
        MockMutation::CreateBookmark { name, target } => {
            validate_mock_bookmark_name(name)?;
            if snapshot.bookmarks.iter().any(|b| b.name == *name) {
                return Err(BackendError::MutationFailed(format!(
                    "bookmark \u{201c}{name}\u{201d} already exists"
                )));
            }
            find(target)?;
            Ok(recorded(format!("Created {name} on {target}"), target))
        }
        MockMutation::MoveBookmark { name, target } => {
            let bookmark = find_bookmark(snapshot, name)?;
            find(target)?;
            if bookmark.target == *target {
                return Ok(MutationOutcome {
                    operation_id: None,
                    summary: format!("{name} already points at {target}"),
                    target_change: Some(target.clone()),
                });
            }
            let direction = if is_mock_ancestor(snapshot, &bookmark.target, target) {
                ""
            } else if is_mock_ancestor(snapshot, target, &bookmark.target) {
                " backwards"
            } else {
                " sideways"
            };
            Ok(recorded(format!("Moved {name}{direction} to {target}"), target))
        }
        MockMutation::RenameBookmark { old, new } => {
            validate_mock_bookmark_name(new)?;
            let bookmark = find_bookmark(snapshot, old)?;
            let target = bookmark.target.clone();
            if old == new {
                return Ok(MutationOutcome {
                    operation_id: None,
                    summary: format!("{old} is already the name"),
                    target_change: Some(target),
                });
            }
            if snapshot.bookmarks.iter().any(|b| b.name == *new) {
                return Err(BackendError::MutationFailed(format!(
                    "bookmark \u{201c}{new}\u{201d} already exists"
                )));
            }
            Ok(recorded(format!("Renamed {old} to {new}"), &target))
        }
        MockMutation::DeleteBookmark { name } => {
            let bookmark = find_bookmark(snapshot, name)?;
            let target = bookmark.target.clone();
            Ok(recorded(format!("Deleted {name}"), &target))
        }
        MockMutation::TrackBookmark { name, remote } => {
            let entry = snapshot
                .bookmarks
                .iter()
                .find(|b| b.name == *name && b.remote.as_deref() == Some(remote.as_str()))
                .ok_or_else(|| {
                    BackendError::MutationFailed(format!(
                        "there is no remote bookmark {name}@{remote}"
                    ))
                })?;
            if entry.is_local || entry.tracked {
                return Err(BackendError::MutationFailed(format!(
                    "{name}@{remote} is already tracked"
                )));
            }
            let target = entry.target.clone();
            Ok(recorded(format!("Tracking {name}@{remote}"), &target))
        }
        MockMutation::UntrackBookmark { name, remote } => {
            let tracked = snapshot.bookmarks.iter().any(|b| {
                b.name == *name
                    && b.is_local
                    && b.tracked
                    && b.remote.as_deref() == Some(remote.as_str())
            });
            if !tracked {
                return Err(BackendError::MutationFailed(format!(
                    "{name}@{remote} is not tracked"
                )));
            }
            Ok(MutationOutcome {
                operation_id: Some(mock_operation_id(index)),
                summary: format!("Stopped tracking {name}@{remote}"),
                target_change: None,
            })
        }
        MockMutation::DeleteRemoteBookmark { name, remote } => {
            let exists = snapshot
                .bookmarks
                .iter()
                .any(|b| b.name == *name && b.remote.as_deref() == Some(remote.as_str()));
            if !exists {
                return Err(BackendError::MutationFailed(format!(
                    "there is no remote bookmark {name}@{remote}"
                )));
            }
            Ok(MutationOutcome {
                operation_id: Some(mock_operation_id(index)),
                summary: format!("Deleted {name} on {remote}"),
                target_change: None,
            })
        }
        MockMutation::Push { names, remote } => {
            // Mirrors the real backend's refusals in its order: unknown
            // names, conflicted bookmarks, unpresentable commits, then the
            // all-synced no-op. The mock has no remote to move underneath
            // it, so lease refusals never replay.
            let mut to_push = Vec::new();
            for name in names {
                let bookmark = find_bookmark(snapshot, name)?;
                if !bookmark.is_local {
                    return Err(BackendError::BookmarkMissing(name.clone()));
                }
                if snapshot
                    .conflicts
                    .iter()
                    .any(|c| c.kind == ConflictKind::Bookmark && c.id == format!("bookmark-{name}"))
                {
                    return Err(BackendError::MutationFailed(format!(
                        "Bookmark \u{201c}{name}\u{201d} is conflicted; repoint it \
                         before pushing"
                    )));
                }
                let node = find(&bookmark.target)?;
                if node.description.is_empty() {
                    return Err(BackendError::MutationFailed(format!(
                        "Won't push commit {} since it has no description",
                        node.commit_id
                    )));
                }
                if node.has_conflict {
                    return Err(BackendError::MutationFailed(format!(
                        "Won't push commit {} since it has conflicts",
                        node.commit_id
                    )));
                }
                if bookmark.sync != SyncState::Synced || bookmark.remote.is_none() {
                    to_push.push(name.clone());
                }
            }
            if to_push.is_empty() {
                return Ok(MutationOutcome {
                    operation_id: None,
                    summary: format!("Everything is already pushed to {remote}"),
                    target_change: None,
                });
            }
            let summary = match &to_push[..] {
                [name] => format!("Pushed {name} to {remote}"),
                names => format!("Pushed {} bookmarks to {remote}", names.len()),
            };
            Ok(MutationOutcome {
                operation_id: Some(mock_operation_id(index)),
                summary,
                target_change: None,
            })
        }
        MockMutation::FetchPr { number, bookmark } => {
            validate_mock_bookmark_name(bookmark)?;
            if snapshot.bookmarks.iter().any(|b| b.name == *bookmark) {
                return Err(BackendError::MutationFailed(format!(
                    "bookmark \u{201c}{bookmark}\u{201d} already exists"
                )));
            }
            Ok(recorded(
                format!("Fetched PR #{number} into {bookmark}"),
                &mock_new_change_id(index),
            ))
        }
        // The two time-travel entries leave `target_change` empty: the
        // selection should follow the working copy of the replayed result,
        // which only the caller can see.
        MockMutation::RevertOp { op_id } => {
            let op = find_operation(snapshot, op_id)?;
            let label = op_label(op);
            let Some(j) = mock_mutation_index(op_id) else {
                // Real jj reverts any single-parent operation; the mock can
                // only unpick what it remembered. A clear refusal beats a
                // wrong replay.
                return Err(BackendError::MutationFailed(
                    "the mock backend can only revert operations it recorded".into(),
                ));
            };
            if prior.get(j).is_some_and(MockMutation::is_time_travel) {
                return Err(BackendError::MutationFailed(
                    "the mock backend cannot revert a revert or restore operation".into(),
                ));
            }
            if !active_effects(prior)[j] {
                // Like the CLI's "Nothing changed.": the inverse is already
                // part of the current state.
                return Ok(MutationOutcome {
                    operation_id: None,
                    summary: format!("{label} is already undone"),
                    target_change: None,
                });
            }
            Ok(MutationOutcome {
                operation_id: Some(mock_operation_id(index)),
                summary: format!("Reverted {label}"),
                target_change: None,
            })
        }
        MockMutation::RestoreOp { op_id } => {
            let op = find_operation(snapshot, op_id)?;
            if op.is_current {
                return Ok(MutationOutcome {
                    operation_id: None,
                    summary: "The repo is already in this state".into(),
                    target_change: None,
                });
            }
            Ok(MutationOutcome {
                operation_id: Some(mock_operation_id(index)),
                summary: format!("Restored to {}", op_label(op)),
                target_change: None,
            })
        }
        MockMutation::Resolve { id, path, side } => {
            let node = find(id)?;
            require_mutable(node)?;
            let listed = snapshot.conflicts.iter().any(|item| {
                item.kind == ConflictKind::File
                    && item.node_id.as_deref() == Some(id.as_str())
                    && item.paths.iter().any(|p| p == path)
            });
            if !listed {
                return Err(BackendError::MutationFailed(format!(
                    "{path} has no conflict in {id}"
                )));
            }
            let summary = match side {
                None => format!("Resolved {path} in {id}"),
                Some(ConflictSide::Ours) => format!("Resolved {path} in {id} by taking ours"),
                Some(ConflictSide::Theirs) => {
                    format!("Resolved {path} in {id} by taking theirs")
                }
            };
            Ok(recorded(summary, id))
        }
        MockMutation::RestoreFile { id, path, hunks } => {
            let node = find(id)?;
            require_mutable(node)?;
            // The change's own diff decides what can be discarded —
            // mutation-created nodes have no fixture files, matching the
            // real backend's "already unchanged" answer.
            let touches = mock_change_detail(id)
                .map(|detail| detail.files.iter().any(|f| f.path == *path))
                .unwrap_or(false);
            if !touches {
                return Ok(MutationOutcome {
                    operation_id: None,
                    summary: format!("{path} is already unchanged in {id}"),
                    target_change: Some(id.clone()),
                });
            }
            match hunks {
                Some(hunks) if hunks.is_empty() => Err(BackendError::MutationFailed(format!(
                    "no hunks selected in {path}"
                ))),
                Some(hunks) => Ok(recorded(
                    format!("Discarded {} of {path} in {id}", hunks_phrase(hunks.len())),
                    id,
                )),
                None => Ok(recorded(format!("Discarded changes to {path} in {id}"), id)),
            }
        }
        MockMutation::RestoreVersion { id, commit } => {
            let node = find(id)?;
            require_mutable(node)?;
            // The chain as the panel saw it — fixture entries plus any
            // already-replayed restores — decides what is restorable,
            // mirroring the real backend's predecessor-membership check.
            let chain = mock_change_evolog(snapshot, prior, &active_effects(prior), id)?;
            if !chain.entries.iter().any(|e| e.commit_id == *commit) {
                return Err(BackendError::MutationFailed(format!(
                    "{commit} is not an earlier version of {id} — refresh and try again"
                )));
            }
            if chain.entries.first().is_some_and(|head| head.commit_id == *commit) {
                return Ok(MutationOutcome {
                    operation_id: None,
                    summary: format!("{id} is already at version {commit}"),
                    target_change: Some(id.clone()),
                });
            }
            Ok(recorded(format!("Restored {id} to version {commit}"), id))
        }
        MockMutation::Duplicate { id } => {
            find(id)?;
            let new_id = mock_new_change_id(index);
            Ok(recorded(format!("Duplicated {id} as {new_id}"), &new_id))
        }
        MockMutation::Revert { id } => {
            // Like Duplicate, no mutability gate: the back-out is a new
            // change; reverting landed work is the point.
            find(id)?;
            let new_id = mock_new_change_id(index);
            Ok(recorded(format!("Reverted {id} as {new_id}"), &new_id))
        }
        MockMutation::Absorb { id } => {
            let node = find(id)?;
            require_mutable(node)?;
            if node.is_empty {
                return Err(BackendError::MutationFailed(format!(
                    "nothing in {id} could be absorbed — the change is empty"
                )));
            }
            Ok(recorded(
                format!("Absorbed changes from {id} into 1 ancestor change"),
                id,
            ))
        }
    }
}

fn find_operation<'a>(
    snapshot: &'a RepoSnapshot,
    op_id: &str,
) -> Result<&'a OperationItem, BackendError> {
    snapshot
        .operations
        .iter()
        .find(|o| o.id == op_id)
        .ok_or_else(|| BackendError::OperationMissing(op_id.to_owned()))
}

fn find_bookmark<'a>(
    snapshot: &'a RepoSnapshot,
    name: &str,
) -> Result<&'a BookmarkState, BackendError> {
    snapshot
        .bookmarks
        .iter()
        .find(|b| b.name == name)
        .ok_or_else(|| BackendError::BookmarkMissing(name.to_owned()))
}

/// Mirrors the real backend's pragmatic name validation.
fn validate_mock_bookmark_name(name: &str) -> Result<(), BackendError> {
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

/// Ancestry within the fabricated graph (parent and elided-parent links),
/// for the move direction the real backend reads from the index.
fn is_mock_ancestor(snapshot: &RepoSnapshot, ancestor: &str, descendant: &str) -> bool {
    let mut queue = vec![descendant.to_owned()];
    let mut seen = std::collections::HashSet::new();
    while let Some(id) = queue.pop() {
        if id == ancestor {
            return true;
        }
        if !seen.insert(id.clone()) {
            continue;
        }
        if let Some(node) = snapshot.nodes.iter().find(|n| n.id == id) {
            queue.extend(node.parents.iter().cloned());
            queue.extend(node.elided_parents.iter().cloned());
        }
    }
    false
}

/// Transitive descendants of one node within the fabricated graph, for the
/// rebase summary's "and N descendants".
fn mock_descendant_count(snapshot: &RepoSnapshot, id: &str) -> usize {
    let mut seen = std::collections::HashSet::from([id.to_owned()]);
    let mut queue = vec![id.to_owned()];
    let mut count = 0;
    while let Some(current) = queue.pop() {
        for node in &snapshot.nodes {
            if node.parents.contains(&current) && seen.insert(node.id.clone()) {
                count += 1;
                queue.push(node.id.clone());
            }
        }
    }
    count
}

/// Replays one remembered mutation onto the fabricated snapshot. Mutations
/// arrive pre-validated by `mutation_outcome`. An inactive mutation (undone
/// by a later revert/restore) keeps its operation row — jj's op log keeps
/// everything — but skips its effect; its row loses the effect chips, a
/// documented mock approximation (the real backend re-derives them from op
/// views).
pub fn apply_mutation(
    snapshot: &mut RepoSnapshot,
    mutation: &MockMutation,
    index: usize,
    active: bool,
) {
    if !active && !mutation.is_time_travel() {
        let description = inactive_op_description(snapshot, mutation);
        push_op(snapshot, index, description, vec![]);
        return;
    }
    match mutation {
        MockMutation::RevertOp { op_id } => {
            push_op(snapshot, index, format!("revert operation {op_id}"), vec![]);
        }
        MockMutation::RestoreOp { op_id } => {
            push_op(
                snapshot,
                index,
                format!("restore to operation {op_id}"),
                vec![],
            );
        }
        MockMutation::Describe { id, text } => {
            let Some(node) = snapshot.nodes.iter_mut().find(|n| n.id == *id) else {
                return;
            };
            node.description = text.clone();
            let commit = node.commit_id.clone();
            push_op(snapshot, index, format!("describe commit {commit}"), vec![]);
        }
        MockMutation::New { parent } => {
            let new_id = spawn_working_copy(snapshot, parent, index);
            push_op(
                snapshot,
                index,
                "new empty commit".to_owned(),
                vec![wc_moved_effect()],
            );
            debug_assert_eq!(new_id, mock_new_change_id(index));
        }
        MockMutation::Edit { id } => {
            retire_working_copy(snapshot);
            let Some(node) = snapshot.nodes.iter_mut().find(|n| n.id == *id) else {
                return;
            };
            node.kind = NodeKind::WorkingCopy;
            let commit = node.commit_id.clone();
            set_working_copy(snapshot, id);
            for ws in &mut snapshot.workstreams {
                ws.is_active = ws.node_ids.iter().any(|n| n == id);
            }
            push_op(
                snapshot,
                index,
                format!("edit commit {commit}"),
                vec![wc_moved_effect()],
            );
        }
        MockMutation::Abandon { id } => {
            let Some(removed) = remove_node(snapshot, id) else {
                return;
            };
            reparent_children(snapshot, id, &removed.parents);
            // Like the real backend: bookmarks on the abandoned change are
            // deleted, and an abandoned working copy respawns on the parent.
            let mut effects: Vec<OpEffect> = Vec::new();
            snapshot.bookmarks.retain(|b| {
                if b.target == *id {
                    effects.push(OpEffect {
                        kind: OpEffectKind::Bookmark,
                        label: format!("{} deleted", b.name),
                    });
                    false
                } else {
                    true
                }
            });
            prune_workstreams(snapshot, id);
            if snapshot.working_copy == *id {
                if let Some(parent) = removed.parents.first() {
                    spawn_working_copy(snapshot, &parent.clone(), index);
                }
                effects.push(wc_moved_effect());
            }
            push_op(
                snapshot,
                index,
                format!("abandon commit {}", removed.commit_id),
                effects,
            );
        }
        MockMutation::AbandonMany { ids } => {
            // One op sweeping the set, like the real backend: each removal
            // reparents children onto the removed node's parents, so chains
            // inside the set collapse transitively.
            let mut effects: Vec<OpEffect> = Vec::new();
            let mut newest_commit = String::new();
            let mut removed_count = 0usize;
            for id in ids {
                let Some(removed) = remove_node(snapshot, id) else {
                    continue;
                };
                if newest_commit.is_empty() {
                    // Snapshot nodes render newest-first, and the replay
                    // removes in list order — the first removed id whose
                    // node exists stands in for the CLI's topologically
                    // newest (the land cleanup passes ids newest-first).
                    newest_commit = removed.commit_id.clone();
                }
                removed_count += 1;
                reparent_children(snapshot, id, &removed.parents);
                snapshot.bookmarks.retain(|b| {
                    if b.target == *id {
                        effects.push(OpEffect {
                            kind: OpEffectKind::Bookmark,
                            label: format!("{} deleted", b.name),
                        });
                        false
                    } else {
                        true
                    }
                });
                prune_workstreams(snapshot, id);
                if snapshot.working_copy == *id {
                    if let Some(parent) = removed.parents.first() {
                        spawn_working_copy(snapshot, &parent.clone(), index);
                    }
                    effects.push(wc_moved_effect());
                }
            }
            let description = if removed_count <= 1 {
                format!("abandon commit {newest_commit}")
            } else {
                format!(
                    "abandon commit {newest_commit} and {} more",
                    removed_count - 1
                )
            };
            push_op(snapshot, index, description, effects);
        }
        MockMutation::Squash { id } => {
            let Some(removed) = remove_node(snapshot, id) else {
                return;
            };
            let Some(parent_id) = removed.parents.first().cloned() else {
                return;
            };
            if let Some(parent) = snapshot.nodes.iter_mut().find(|n| n.id == parent_id) {
                parent.description =
                    combined_mock_description(&parent.description, &removed.description);
                parent.bookmarks.extend(removed.bookmarks.iter().cloned());
                parent.is_empty = false;
            }
            // Bookmarks move from the squashed change to its parent — a
            // change-granularity move the timeline reports.
            let mut effects: Vec<OpEffect> = Vec::new();
            for bookmark in &mut snapshot.bookmarks {
                if bookmark.target == *id {
                    bookmark.target = parent_id.clone();
                    effects.push(OpEffect {
                        kind: OpEffectKind::Bookmark,
                        label: format!("{} moved", bookmark.name),
                    });
                }
            }
            reparent_children(snapshot, id, std::slice::from_ref(&parent_id));
            prune_workstreams(snapshot, id);
            if snapshot.working_copy == *id {
                spawn_working_copy(snapshot, &parent_id, index);
                effects.push(wc_moved_effect());
            }
            let parent_commit = snapshot
                .nodes
                .iter()
                .find(|n| n.id == parent_id)
                .map(|n| n.commit_id.clone())
                .unwrap_or_default();
            push_op(
                snapshot,
                index,
                format!("squash commits into {parent_commit}"),
                effects,
            );
        }
        MockMutation::Split { id, description, .. } => {
            // The split change keeps the selected files and takes the new
            // description; a new change directly on top inherits the
            // original description, bookmarks, children, and working-copy
            // status — jj's split rule. Fixture diffs stay static, so both
            // halves keep showing the original file list (a documented mock
            // approximation, like resolve's marker content).
            let Some(position) = snapshot.nodes.iter().position(|n| n.id == *id) else {
                return;
            };
            let new_id = mock_new_change_id(index);
            let (commit, remainder) = {
                let node = &mut snapshot.nodes[position];
                let remainder = GraphNode {
                    id: new_id.clone(),
                    change_id: new_id.clone(),
                    commit_id: format!("e5{index:02}9b2c"),
                    description: std::mem::replace(&mut node.description, description.clone()),
                    author: node.author.clone(),
                    timestamp: node.timestamp.clone(),
                    kind: node.kind,
                    parents: vec![id.clone()],
                    elided_parents: Vec::new(),
                    bookmarks: std::mem::take(&mut node.bookmarks),
                    tags: Vec::new(),
                    is_empty: false,
                    has_conflict: false,
                    is_divergent: false,
                };
                if node.kind == NodeKind::WorkingCopy {
                    node.kind = NodeKind::Mutable;
                }
                (node.commit_id.clone(), remainder)
            };
            reparent_children(snapshot, id, std::slice::from_ref(&new_id));
            let mut effects: Vec<OpEffect> = Vec::new();
            for bookmark in &mut snapshot.bookmarks {
                if bookmark.target == *id {
                    bookmark.target = new_id.clone();
                    effects.push(OpEffect {
                        kind: OpEffectKind::Bookmark,
                        label: format!("{} moved", bookmark.name),
                    });
                }
            }
            let was_working_copy = snapshot.working_copy == *id;
            snapshot.nodes.insert(position, remainder);
            if was_working_copy {
                set_working_copy(snapshot, &new_id);
                effects.push(wc_moved_effect());
            }
            // The remainder slots into the same workstream, directly above
            // the split change (node_ids are top-first).
            for ws in &mut snapshot.workstreams {
                if let Some(at) = ws.node_ids.iter().position(|n| n == id) {
                    ws.node_ids.insert(at, new_id.clone());
                }
            }
            push_op(snapshot, index, format!("split commit {commit}"), effects);
        }
        MockMutation::SquashInto { id, selection, destination } => {
            // A partial move changes only file contents, which the static
            // fixture diffs cannot follow (the documented mock
            // approximation); the destination stops reading as empty either
            // way. A full move abandons the emptied source like the real
            // backend: children reparent onto its parents, bookmarks land
            // on its first parent, and an emptied working copy respawns.
            let full = mock_change_detail(id).is_some_and(|detail| {
                !detail.files.is_empty()
                    && detail.files.iter().all(|f| {
                        selection
                            .iter()
                            .any(|s| s.path == f.path && s.hunks.is_none())
                    })
            });
            let dest_commit = match snapshot.nodes.iter_mut().find(|n| n.id == *destination) {
                Some(dest) => {
                    dest.is_empty = false;
                    dest.commit_id.clone()
                }
                None => return,
            };
            let mut effects: Vec<OpEffect> = Vec::new();
            if full {
                let Some(removed) = remove_node(snapshot, id) else {
                    return;
                };
                if let Some(dest) = snapshot.nodes.iter_mut().find(|n| n.id == *destination) {
                    dest.description =
                        combined_mock_description(&dest.description, &removed.description);
                }
                reparent_children(snapshot, id, &removed.parents);
                if let Some(parent_id) = removed.parents.first() {
                    for bookmark in &mut snapshot.bookmarks {
                        if bookmark.target == *id {
                            bookmark.target = parent_id.clone();
                            effects.push(OpEffect {
                                kind: OpEffectKind::Bookmark,
                                label: format!("{} moved", bookmark.name),
                            });
                        }
                    }
                    if let Some(parent) =
                        snapshot.nodes.iter_mut().find(|n| n.id == *parent_id)
                    {
                        parent.bookmarks.extend(removed.bookmarks.iter().cloned());
                    }
                }
                prune_workstreams(snapshot, id);
                if snapshot.working_copy == *id {
                    if let Some(parent) = removed.parents.first() {
                        spawn_working_copy(snapshot, &parent.clone(), index);
                    }
                    effects.push(wc_moved_effect());
                }
            }
            push_op(
                snapshot,
                index,
                format!("squash commits into {dest_commit}"),
                effects,
            );
        }
        MockMutation::Rebase {
            id,
            destination,
            with_descendants,
        } => {
            // A rebase rewrites commits in place at change granularity:
            // bookmarks and the working copy follow the same change ids, so
            // the operation reports no effects — matching the real backend.
            let old_parents = match snapshot.nodes.iter().find(|n| n.id == *id) {
                Some(node) => node.parents.clone(),
                None => return,
            };
            if !with_descendants {
                reparent_children(snapshot, id, &old_parents);
            }
            let commit = {
                let Some(node) = snapshot.nodes.iter_mut().find(|n| n.id == *id) else {
                    return;
                };
                node.parents = vec![destination.clone()];
                node.commit_id.clone()
            };
            let suffix = if *with_descendants { " and descendants" } else { "" };
            push_op(
                snapshot,
                index,
                format!("rebase commit {commit}{suffix}"),
                vec![],
            );
        }
        MockMutation::CreateBookmark { name, target } => {
            let entry = local_bookmark_entry(snapshot, name, target);
            snapshot.bookmarks.push(entry);
            let mut commit = String::new();
            if let Some(node) = snapshot.nodes.iter_mut().find(|n| n.id == *target) {
                node.bookmarks.push(name.clone());
                commit = node.commit_id.clone();
            }
            // The workstream containing the target adopts the bookmark when
            // it had none, like the real backend's chain walk would.
            if let Some(ws) = snapshot
                .workstreams
                .iter_mut()
                .find(|ws| ws.bookmark.is_none() && ws.node_ids.iter().any(|n| n == target))
            {
                ws.bookmark = Some(name.clone());
            }
            push_op(
                snapshot,
                index,
                format!("create bookmark {name} pointing to commit {commit}"),
                vec![OpEffect {
                    kind: OpEffectKind::Bookmark,
                    label: format!("{name} created"),
                }],
            );
        }
        MockMutation::MoveBookmark { name, target } => {
            let mut commit = String::new();
            for node in &mut snapshot.nodes {
                node.bookmarks.retain(|b| b != name);
                if node.id == *target {
                    node.bookmarks.push(name.clone());
                    commit = node.commit_id.clone();
                }
            }
            for bookmark in &mut snapshot.bookmarks {
                if bookmark.name == *name {
                    bookmark.target = target.clone();
                    // A tracked bookmark that moved is no longer in sync.
                    if bookmark.remote.is_some() && bookmark.sync == SyncState::Synced {
                        bookmark.sync = SyncState::Ahead;
                    }
                }
            }
            // Workstream bookmark labels follow the move, like the real
            // backend's chain walk would find them.
            for ws in &mut snapshot.workstreams {
                if ws.bookmark.as_deref() == Some(name.as_str())
                    && !ws.node_ids.iter().any(|n| n == target)
                {
                    ws.bookmark = None;
                }
            }
            if let Some(ws) = snapshot
                .workstreams
                .iter_mut()
                .find(|ws| ws.bookmark.is_none() && ws.node_ids.iter().any(|n| n == target))
            {
                ws.bookmark = Some(name.clone());
            }
            push_op(
                snapshot,
                index,
                format!("point bookmark {name} to commit {commit}"),
                vec![OpEffect {
                    kind: OpEffectKind::Bookmark,
                    label: format!("{name} moved"),
                }],
            );
        }
        MockMutation::RenameBookmark { old, new } => {
            for bookmark in &mut snapshot.bookmarks {
                if bookmark.name == *old {
                    bookmark.name = new.clone();
                    // Like the CLI: tracked remote bookmarks keep the old
                    // name until push, so the renamed one starts local-only.
                    bookmark.remote = None;
                    bookmark.sync = SyncState::LocalOnly;
                }
            }
            for node in &mut snapshot.nodes {
                for b in &mut node.bookmarks {
                    if b == old {
                        *b = new.clone();
                    }
                }
            }
            for ws in &mut snapshot.workstreams {
                if ws.bookmark.as_deref() == Some(old.as_str()) {
                    ws.bookmark = Some(new.clone());
                }
            }
            push_op(
                snapshot,
                index,
                format!("rename bookmark {old} to {new}"),
                // Same order the real backend's view diff reports: the new
                // name created, then the old one deleted.
                vec![
                    OpEffect {
                        kind: OpEffectKind::Bookmark,
                        label: format!("{new} created"),
                    },
                    OpEffect {
                        kind: OpEffectKind::Bookmark,
                        label: format!("{old} deleted"),
                    },
                ],
            );
        }
        MockMutation::DeleteBookmark { name } => {
            snapshot.bookmarks.retain(|b| b.name != *name);
            for node in &mut snapshot.nodes {
                node.bookmarks.retain(|b| b != name);
            }
            for ws in &mut snapshot.workstreams {
                if ws.bookmark.as_deref() == Some(name.as_str()) {
                    ws.bookmark = None;
                }
            }
            push_op(
                snapshot,
                index,
                format!("delete bookmark {name}"),
                vec![OpEffect {
                    kind: OpEffectKind::Bookmark,
                    label: format!("{name} deleted"),
                }],
            );
        }
        MockMutation::TrackBookmark { name, remote } => {
            // The remote-only entry becomes an ordinary tracked local
            // bookmark at the remote position, like the real backend.
            let mut target = String::new();
            for bookmark in &mut snapshot.bookmarks {
                if bookmark.name == *name
                    && !bookmark.is_local
                    && bookmark.remote.as_deref() == Some(remote.as_str())
                {
                    bookmark.is_local = true;
                    bookmark.tracked = true;
                    bookmark.sync = SyncState::Synced;
                    target = bookmark.target.clone();
                }
            }
            if let Some(node) = snapshot.nodes.iter_mut().find(|n| n.id == target) {
                if !node.bookmarks.contains(name) {
                    node.bookmarks.push(name.clone());
                }
            }
            push_op(
                snapshot,
                index,
                format!("track remote bookmark {name}@{remote}"),
                vec![OpEffect {
                    kind: OpEffectKind::Bookmark,
                    label: format!("{name} created"),
                }],
            );
        }
        MockMutation::UntrackBookmark { name, remote } => {
            // The local bookmark stays where it is but stops following the
            // remote; the remote ref itself survives, so it reappears as a
            // remote-only entry — like the real backend.
            let mut ghost: Option<BookmarkState> = None;
            for bookmark in &mut snapshot.bookmarks {
                if bookmark.name == *name
                    && bookmark.is_local
                    && bookmark.remote.as_deref() == Some(remote.as_str())
                {
                    ghost = Some(BookmarkState {
                        remote: Some(remote.clone()),
                        sync: SyncState::RemoteOnly,
                        is_local: false,
                        tracked: false,
                        is_trunk: false,
                        ..bookmark.clone()
                    });
                    bookmark.remote = None;
                    bookmark.sync = SyncState::LocalOnly;
                    bookmark.tracked = false;
                }
            }
            if let Some(entry) = ghost {
                snapshot.bookmarks.push(entry);
            }
            push_op(
                snapshot,
                index,
                format!("untrack remote bookmark {name}@{remote}"),
                vec![],
            );
        }
        MockMutation::DeleteRemoteBookmark { name, remote } => {
            // The real backend records the local delete and the push as
            // separate operations; the mock collapses them into the push —
            // a documented approximation, the visible result matches.
            snapshot.bookmarks.retain(|b| {
                !(b.name == *name
                    && (b.is_local || b.remote.as_deref() == Some(remote.as_str())))
            });
            for node in &mut snapshot.nodes {
                node.bookmarks.retain(|b| b != name);
            }
            for ws in &mut snapshot.workstreams {
                if ws.bookmark.as_deref() == Some(name.as_str()) {
                    ws.bookmark = None;
                }
            }
            push_op(
                snapshot,
                index,
                format!("push bookmark {name} to git remote {remote}"),
                vec![
                    OpEffect {
                        kind: OpEffectKind::Bookmark,
                        label: format!("{name} deleted"),
                    },
                    OpEffect {
                        kind: OpEffectKind::RemoteBookmark,
                        label: format!("{name}@{remote} deleted"),
                    },
                ],
            );
        }
        MockMutation::Push { names, remote } => {
            // Pushed bookmarks read as tracked and synced afterwards; the
            // op description matches the CLI's, like the real backend.
            let mut pushed = Vec::new();
            let mut effects = Vec::new();
            for name in names {
                let Some(bookmark) = snapshot.bookmarks.iter_mut().find(|b| b.name == *name)
                else {
                    continue;
                };
                if bookmark.sync == SyncState::Synced && bookmark.remote.is_some() {
                    continue;
                }
                bookmark.remote = Some(remote.clone());
                bookmark.sync = SyncState::Synced;
                bookmark.tracked = true;
                pushed.push(name.clone());
                effects.push(OpEffect {
                    kind: OpEffectKind::RemoteBookmark,
                    label: format!("{name}@{remote} updated"),
                });
            }
            let description = match &pushed[..] {
                [name] => format!("push bookmark {name} to git remote {remote}"),
                names => format!(
                    "push bookmarks {} to git remote {remote}",
                    names.join(", ")
                ),
            };
            push_op(snapshot, index, description, effects);
        }
        MockMutation::FetchPr { number, bookmark } => {
            // Documented approximation: the fabricated remote serves every
            // PR head as one fresh change directly on trunk (the real
            // backend fetches whatever ancestry the PR really carries).
            let new_id = mock_new_change_id(index);
            let trunk = snapshot
                .bookmarks
                .iter()
                .find(|b| b.is_trunk)
                .map(|b| b.target.clone())
                .unwrap_or_default();
            snapshot.nodes.insert(
                0,
                GraphNode {
                    id: new_id.clone(),
                    change_id: new_id.clone(),
                    commit_id: format!("9c{index:02}7de2"),
                    description: format!("PR #{number} head (fetched for review)"),
                    author: "them".into(),
                    timestamp: mock_op_timestamp(index),
                    kind: NodeKind::Mutable,
                    parents: vec![trunk],
                    elided_parents: Vec::new(),
                    bookmarks: vec![bookmark.clone()],
                    tags: Vec::new(),
                    is_empty: false,
                    has_conflict: false,
                    is_divergent: false,
                },
            );
            let entry = local_bookmark_entry(snapshot, bookmark, &new_id);
            snapshot.bookmarks.push(entry);
            snapshot.workstreams.push(WorkstreamSummary {
                id: format!("ws-{new_id}"),
                title: format!("PR #{number} head (fetched for review)"),
                node_ids: vec![new_id.clone()],
                bookmark: Some(bookmark.clone()),
                is_active: false,
                behind_trunk: 0,
            });
            push_op(
                snapshot,
                index,
                format!("fetch pull request #{number} into bookmark {bookmark}"),
                vec![OpEffect {
                    kind: OpEffectKind::Bookmark,
                    label: format!("{bookmark} created"),
                }],
            );
        }
        MockMutation::Resolve { id, path, .. } => {
            // The resolved path leaves its inbox item; an item with no paths
            // left is done, and its node stops rendering as conflicted. Only
            // the targeted item settles — an ancestor's copy of the same
            // conflict stays until resolved itself, like the real backend.
            // (Fixture diffs still show the old marker content — a documented
            // mock approximation, they are static fixtures.)
            let commit = snapshot
                .nodes
                .iter()
                .find(|n| n.id == *id)
                .map(|n| n.commit_id.clone())
                .unwrap_or_default();
            let mut settled = false;
            snapshot.conflicts.retain_mut(|item| {
                if item.kind != ConflictKind::File || item.node_id.as_deref() != Some(id.as_str())
                {
                    return true;
                }
                item.paths.retain(|p| p != path);
                if item.paths.is_empty() && item.more_paths == 0 {
                    settled = true;
                    return false;
                }
                true
            });
            if settled {
                if let Some(node) = snapshot.nodes.iter_mut().find(|n| n.id == *id) {
                    node.has_conflict = false;
                }
            }
            push_op(
                snapshot,
                index,
                format!("Resolve conflicts in commit {commit}"),
                vec![],
            );
        }
        MockMutation::RestoreFile { id, path, hunks } => {
            // Fixture diffs are static, so the discarded file's hunks keep
            // rendering — a documented mock approximation. The op row is
            // the visible effect.
            let commit = commit_of_node(snapshot, id);
            push_op(
                snapshot,
                index,
                match hunks {
                    Some(hunks) => format!(
                        "restore {} of {path} in commit {commit}",
                        hunks_phrase(hunks.len())
                    ),
                    None => format!("restore {path} in commit {commit}"),
                },
                vec![],
            );
        }
        MockMutation::RestoreVersion { id, commit } => {
            // Fixture diffs are static, so a restored version shows as the
            // description coming back and a fresh commit id — a documented
            // mock approximation (the evolog itself grows the new version
            // through `mock_change_evolog`).
            let restored_description = fixture_evolog_entries(id)
                .into_iter()
                .find(|e| e.commit_id == *commit)
                .map(|e| e.description);
            let Some(node) = snapshot.nodes.iter_mut().find(|n| n.id == *id) else {
                return;
            };
            let old_commit = node.commit_id.clone();
            if let Some(description) = restored_description {
                node.description = description;
            }
            node.commit_id = mock_version_commit_id(index);
            push_op(
                snapshot,
                index,
                format!("restore commit {old_commit} to version {commit}"),
                vec![],
            );
        }
        MockMutation::Duplicate { id } => {
            // The copy appears beside the original: same description and
            // parents, a fresh id, no bookmarks, outside any workstream
            // (like a stray change a custom revset would reveal).
            let new_id = mock_new_change_id(index);
            let Some(position) = snapshot.nodes.iter().position(|n| n.id == *id) else {
                return;
            };
            let original = &snapshot.nodes[position];
            let copy = GraphNode {
                id: new_id.clone(),
                change_id: new_id.clone(),
                commit_id: format!("d0{index:02}4af9"),
                description: original.description.clone(),
                author: original.author.clone(),
                timestamp: mock_op_timestamp(index),
                kind: NodeKind::Mutable,
                parents: original.parents.clone(),
                elided_parents: original.elided_parents.clone(),
                bookmarks: Vec::new(),
                tags: Vec::new(),
                is_empty: original.is_empty,
                has_conflict: original.has_conflict,
                is_divergent: false,
            };
            let commit = original.commit_id.clone();
            snapshot.nodes.insert(position, copy);
            push_op(
                snapshot,
                index,
                format!("duplicate commit {commit}"),
                vec![],
            );
        }
        MockMutation::Revert { id } => {
            // The back-out lands as a new child of the working copy (`jj
            // revert -o @`); `@` itself does not move. Fixture diffs are
            // static, so the node renders contentless — a documented mock
            // approximation, like Duplicate's.
            let new_id = mock_new_change_id(index);
            let Some(original) = snapshot.nodes.iter().find(|n| n.id == *id) else {
                return;
            };
            let commit = original.commit_id.clone();
            let first_line = original.description.lines().next().unwrap_or("").to_owned();
            let wc_id = snapshot.working_copy.clone();
            let Some(position) = snapshot.nodes.iter().position(|n| n.id == wc_id) else {
                return;
            };
            snapshot.nodes.insert(
                position,
                GraphNode {
                    id: new_id.clone(),
                    change_id: new_id.clone(),
                    commit_id: format!("ba{index:02}4af9"),
                    description: format!(
                        "Revert \"{first_line}\"\n\nThis reverts commit {commit}.\n"
                    ),
                    author: "lauf".into(),
                    timestamp: mock_op_timestamp(index),
                    kind: NodeKind::Mutable,
                    parents: vec![wc_id.clone()],
                    elided_parents: Vec::new(),
                    bookmarks: Vec::new(),
                    tags: Vec::new(),
                    is_empty: false,
                    has_conflict: false,
                    is_divergent: false,
                },
            );
            // The back-out heads the workstream the working copy headed.
            if let Some(ws) = snapshot
                .workstreams
                .iter_mut()
                .find(|w| w.node_ids.first() == Some(&wc_id))
            {
                ws.node_ids.insert(0, new_id.clone());
            }
            push_op(snapshot, index, format!("revert commit {commit}"), vec![]);
        }
        MockMutation::Absorb { id } => {
            // Fixture diffs are static, so the absorbed hunks keep
            // rendering in both places — a documented mock approximation.
            let commit = commit_of_node(snapshot, id);
            let _ = commit;
            push_op(
                snapshot,
                index,
                "absorb changes into 1 commits".to_owned(),
                vec![],
            );
        }
    }
}

/// The commit id a change id currently renders as, for op descriptions.
fn commit_of_node(snapshot: &RepoSnapshot, id: &str) -> String {
    snapshot
        .nodes
        .iter()
        .find(|n| n.id == id)
        .map(|n| n.commit_id.clone())
        .unwrap_or_else(|| id.to_owned())
}

/// The operation row text for a mutation whose effect a later revert or
/// restore undid. Mirrors the descriptions `apply_mutation` pushes, falling
/// back to change ids when the nodes the descriptions name no longer exist
/// in the replayed snapshot.
fn inactive_op_description(snapshot: &RepoSnapshot, mutation: &MockMutation) -> String {
    let commit_of = |id: &str| {
        snapshot
            .nodes
            .iter()
            .find(|n| n.id == id)
            .map(|n| n.commit_id.clone())
            .unwrap_or_else(|| id.to_owned())
    };
    match mutation {
        MockMutation::Describe { id, .. } => format!("describe commit {}", commit_of(id)),
        MockMutation::New { .. } => "new empty commit".into(),
        MockMutation::Edit { id } => format!("edit commit {}", commit_of(id)),
        MockMutation::Abandon { id } => format!("abandon commit {}", commit_of(id)),
        MockMutation::AbandonMany { ids } => match &ids[..] {
            [id] => format!("abandon commit {}", commit_of(id)),
            ids => format!(
                "abandon commit {} and {} more",
                commit_of(&ids[0]),
                ids.len() - 1
            ),
        },
        MockMutation::Squash { id } => {
            let parent = snapshot
                .nodes
                .iter()
                .find(|n| n.id == *id)
                .and_then(|n| n.parents.first().cloned())
                .unwrap_or_default();
            format!("squash commits into {}", commit_of(&parent))
        }
        MockMutation::Split { id, .. } => format!("split commit {}", commit_of(id)),
        MockMutation::SquashInto { destination, .. } => {
            format!("squash commits into {}", commit_of(destination))
        }
        MockMutation::Rebase {
            id,
            with_descendants,
            ..
        } => format!(
            "rebase commit {}{}",
            commit_of(id),
            if *with_descendants { " and descendants" } else { "" }
        ),
        MockMutation::CreateBookmark { name, target } => {
            format!("create bookmark {name} pointing to commit {}", commit_of(target))
        }
        MockMutation::MoveBookmark { name, target } => {
            format!("point bookmark {name} to commit {}", commit_of(target))
        }
        MockMutation::RenameBookmark { old, new } => format!("rename bookmark {old} to {new}"),
        MockMutation::DeleteBookmark { name } => format!("delete bookmark {name}"),
        MockMutation::TrackBookmark { name, remote } => {
            format!("track remote bookmark {name}@{remote}")
        }
        MockMutation::UntrackBookmark { name, remote } => {
            format!("untrack remote bookmark {name}@{remote}")
        }
        MockMutation::DeleteRemoteBookmark { name, remote } => {
            format!("push bookmark {name} to git remote {remote}")
        }
        MockMutation::Push { names, remote } => match &names[..] {
            [name] => format!("push bookmark {name} to git remote {remote}"),
            names => format!("push bookmarks {} to git remote {remote}", names.join(", ")),
        },
        MockMutation::FetchPr { number, bookmark } => {
            format!("fetch pull request #{number} into bookmark {bookmark}")
        }
        MockMutation::RevertOp { op_id } => format!("revert operation {op_id}"),
        MockMutation::RestoreOp { op_id } => format!("restore to operation {op_id}"),
        MockMutation::Resolve { id, .. } => {
            format!("Resolve conflicts in commit {}", commit_of(id))
        }
        MockMutation::RestoreFile { id, path, hunks } => match hunks {
            Some(hunks) => format!(
                "restore {} of {path} in commit {}",
                hunks_phrase(hunks.len()),
                commit_of(id)
            ),
            None => format!("restore {path} in commit {}", commit_of(id)),
        },
        MockMutation::RestoreVersion { id, commit } => {
            format!("restore commit {} to version {commit}", commit_of(id))
        }
        MockMutation::Duplicate { id } => {
            format!("duplicate commit {}", commit_of(id))
        }
        MockMutation::Revert { id } => {
            format!("revert commit {}", commit_of(id))
        }
        MockMutation::Absorb { .. } => "absorb changes into 1 commits".to_owned(),
    }
}

fn wc_moved_effect() -> OpEffect {
    OpEffect {
        kind: OpEffectKind::WorkingCopy,
        label: "working copy moved".into(),
    }
}

/// A fresh local-only bookmark entry, its target metadata filled from the
/// node when the graph draws it (replayed bookmarks always point at drawn
/// nodes; the fallback keeps a missing one harmless).
fn local_bookmark_entry(snapshot: &RepoSnapshot, name: &str, target: &str) -> BookmarkState {
    let node = snapshot.nodes.iter().find(|n| n.id == target);
    BookmarkState {
        name: name.to_owned(),
        target: target.to_owned(),
        remote: None,
        sync: SyncState::LocalOnly,
        is_trunk: false,
        is_local: true,
        tracked: false,
        is_merged: false,
        target_description: node.map(|n| n.description.clone()).unwrap_or_default(),
        target_author: node.map(|n| n.author.clone()).unwrap_or_default(),
        target_timestamp: node.map(|n| n.timestamp.clone()).unwrap_or_default(),
    }
}

fn push_op(snapshot: &mut RepoSnapshot, index: usize, description: String, effects: Vec<OpEffect>) {
    for op in &mut snapshot.operations {
        op.is_current = false;
    }
    snapshot.operations.insert(
        0,
        OperationItem {
            id: mock_operation_id(index),
            description,
            timestamp: mock_op_timestamp(index),
            is_current: true,
            user: "lauf@mbp".into(),
            is_snapshot: false,
            effects,
            more_effects: 0,
        },
    );
}

/// The previous working-copy node becomes a plain mutable change.
fn retire_working_copy(snapshot: &mut RepoSnapshot) {
    let wc = snapshot.working_copy.clone();
    if let Some(node) = snapshot.nodes.iter_mut().find(|n| n.id == wc) {
        if node.kind == NodeKind::WorkingCopy {
            node.kind = NodeKind::Mutable;
        }
    }
}

fn set_working_copy(snapshot: &mut RepoSnapshot, id: &str) {
    snapshot.working_copy = id.to_owned();
    for workspace in &mut snapshot.workspaces {
        if workspace.is_default {
            workspace.working_copy_node = Some(id.to_owned());
        }
    }
}

/// Creates the new empty working-copy node `jj new` (or abandoning `@`)
/// produces, joining the workstream headed by `parent` when there is one.
fn spawn_working_copy(snapshot: &mut RepoSnapshot, parent: &str, index: usize) -> String {
    let new_id = mock_new_change_id(index);
    retire_working_copy(snapshot);
    snapshot.nodes.insert(
        0,
        GraphNode {
            id: new_id.clone(),
            change_id: new_id.clone(),
            commit_id: format!("0e{index:02}4af9"),
            description: String::new(),
            author: "lauf".into(),
            timestamp: mock_op_timestamp(index),
            kind: NodeKind::WorkingCopy,
            parents: vec![parent.to_owned()],
            elided_parents: Vec::new(),
            bookmarks: Vec::new(),
            tags: Vec::new(),
            is_empty: true,
            has_conflict: false,
            is_divergent: false,
        },
    );
    set_working_copy(snapshot, &new_id);
    for ws in &mut snapshot.workstreams {
        ws.is_active = false;
    }
    if let Some(ws) = snapshot
        .workstreams
        .iter_mut()
        .find(|w| w.node_ids.first().map(String::as_str) == Some(parent))
    {
        ws.node_ids.insert(0, new_id.clone());
        ws.is_active = true;
    } else {
        snapshot.workstreams.insert(
            0,
            WorkstreamSummary {
                id: format!("ws-{new_id}"),
                title: "Anonymous work".into(),
                node_ids: vec![new_id.clone()],
                bookmark: None,
                is_active: true,
                behind_trunk: 0,
            },
        );
    }
    new_id
}

fn remove_node(snapshot: &mut RepoSnapshot, id: &str) -> Option<GraphNode> {
    let position = snapshot.nodes.iter().position(|n| n.id == id)?;
    let removed = snapshot.nodes.remove(position);
    // Removing one copy of a divergent change can resolve the divergence.
    // Mock approximation: the surviving copy keeps its commit-id-based `id`
    // (the real backend re-keys it by change id on the next snapshot).
    let mut siblings = snapshot
        .nodes
        .iter_mut()
        .filter(|n| n.change_id == removed.change_id);
    if let (Some(last), None) = (siblings.next(), siblings.next()) {
        last.is_divergent = false;
    }
    // Conflict items follow the graph: a removed change takes its inbox
    // entries along, and a removed bookmark-conflict candidate leaves the
    // list (mock approximation: with one candidate left the conflict reads
    // as resolved, where the real backend re-derives ref state).
    snapshot.conflicts.retain(|c| c.node_id.as_deref() != Some(id));
    for conflict in &mut snapshot.conflicts {
        conflict.targets.retain(|t| t != id);
    }
    snapshot
        .conflicts
        .retain(|c| c.kind != ConflictKind::Bookmark || c.targets.len() > 1);
    Some(removed)
}

/// Children of a removed node adopt its parents in place.
fn reparent_children(snapshot: &mut RepoSnapshot, removed: &str, new_parents: &[String]) {
    for node in &mut snapshot.nodes {
        if let Some(position) = node.parents.iter().position(|p| p == removed) {
            node.parents.remove(position);
            for parent in new_parents.iter().rev() {
                if !node.parents.contains(parent) {
                    node.parents.insert(position, parent.clone());
                }
            }
        }
    }
}

fn prune_workstreams(snapshot: &mut RepoSnapshot, id: &str) {
    for ws in &mut snapshot.workstreams {
        ws.node_ids.retain(|n| n != id);
    }
    snapshot.workstreams.retain(|ws| !ws.node_ids.is_empty());
}

/// Mirrors the real backend's squash description combining (sans the
/// trailing-newline normalization snapshots never render).
fn combined_mock_description(destination: &str, source: &str) -> String {
    let destination = destination.trim();
    let source = source.trim();
    if destination.is_empty() {
        source.to_owned()
    } else if source.is_empty() {
        destination.to_owned()
    } else {
        format!("{destination}\n\n{source}")
    }
}

pub fn mock_snapshot(repo_path: &Path) -> RepoSnapshot {
    let repo_name = repo_path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| repo_path.display().to_string());

    let nodes = vec![
        node(
            "ktpqsmxw",
            "9a37be51",
            "",
            "lauf",
            "2026-06-10T12:21:00Z",
            NodeKind::WorkingCopy,
            &["lwnouzpy"],
            &[],
        ),
        node(
            "lwnouzpy",
            "c5e208d4",
            "feat: virtualize multi-file diff rendering",
            "lauf",
            "2026-06-10T11:47:00Z",
            NodeKind::Mutable,
            &["mvkortsq"],
            &["diff-virtualization"],
        ),
        node(
            "mvkortsq",
            "41d7aa90",
            "refactor: extract diff row measurement",
            "lauf",
            "2026-06-10T09:05:00Z",
            NodeKind::Mutable,
            &["nzpwlxvr"],
            &[],
        ),
        node(
            "nzpwlxvr",
            "8b19f3c2",
            "feat: sticky file headers in the diff surface",
            "lauf",
            "2026-06-09T19:12:00Z",
            NodeKind::Mutable,
            &["uvkmrtpz"],
            &["diff-model"],
        ),
        // The rebase onto 0.41 (see the ops list) left this stack
        // conflicted: the wip change collided in ConflictList.svelte, and
        // its child carries the unresolved conflict until someone fixes it.
        GraphNode {
            has_conflict: true,
            ..node(
                "qvlxnsry",
                "d2a6f4e8",
                "feat: conflict inbox empty states",
                "lauf",
                "2026-06-08T17:30:00Z",
                NodeKind::Mutable,
                &["pmwzqkvt"],
                &["conflict-inbox"],
            )
        },
        GraphNode {
            has_conflict: true,
            ..node(
                "pmwzqkvt",
                "77be0c13",
                "wip: conflict inbox list skeleton",
                "lauf",
                "2026-06-08T15:02:00Z",
                NodeKind::Mutable,
                &["uvkmrtpz"],
                &[],
            )
        },
        // One change rewritten two ways (jj's `??` state): the same fix
        // described from two terminals, both copies still visible. Nodes
        // key by commit id; the change id is shared.
        // Each terminal also pointed `watcher-fix` at its own copy, so the
        // bookmark resolved to multiple targets (the chip renders on the
        // first candidate, like the real backend's bookmark state).
        GraphNode {
            bookmarks: vec!["watcher-fix".into()],
            ..divergent_node(
                "rzvqnkom",
                "b41c77d0",
                "fix: debounce watcher restarts",
                "2026-06-09T18:26:00Z",
                &["uvkmrtpz"],
            )
        },
        divergent_node(
            "rzvqnkom",
            "e93d5a12",
            "fix: debounce watcher restarts (simpler timer)",
            "2026-06-09T18:31:00Z",
            &["uvkmrtpz"],
        ),
        // A clean two-change stack with no bookmark anywhere: the publish
        // flow's derived-branch-name path (name invented from the bottom
        // change, bookmark created on confirm) needs one to demo on.
        node(
            "toznkvpq",
            "f31c09ab",
            "feat: fuzzy revset filters in the palette",
            "lauf",
            "2026-06-10T10:15:00Z",
            NodeKind::Mutable,
            &["swkmlnxu"],
            &[],
        ),
        node(
            "swkmlnxu",
            "27d4e6b0",
            "feat: palette matcher scores revset atoms",
            "lauf",
            "2026-06-10T08:40:00Z",
            NodeKind::Mutable,
            &["uvkmrtpz"],
            &[],
        ),
        // Trunk carries the release tag the changelog commit was cut as,
        // so the tag chip has a fixture to render on.
        GraphNode {
            tags: vec!["v0.41.0".into()],
            ..node(
                "uvkmrtpz",
                "e7c41a9f",
                "release: cut 0.41 changelog",
                "maintainers",
                "2026-06-09T16:40:00Z",
                NodeKind::Immutable,
                &[],
                &["main"],
            )
        },
    ];

    let bookmarks = vec![
        BookmarkState {
            name: "main".into(),
            target: "uvkmrtpz".into(),
            remote: Some("origin".into()),
            sync: SyncState::Synced,
            is_trunk: true,
            is_local: true,
            tracked: true,
            is_merged: false,
            target_description: "release: cut 0.41 changelog".into(),
            target_author: "maintainers".into(),
            target_timestamp: "2026-06-09T16:40:00Z".into(),
        },
        BookmarkState {
            name: "diff-virtualization".into(),
            target: "lwnouzpy".into(),
            remote: None,
            sync: SyncState::LocalOnly,
            is_trunk: false,
            is_local: true,
            tracked: false,
            is_merged: false,
            target_description: "feat: virtualize multi-file diff rendering".into(),
            target_author: "lauf".into(),
            target_timestamp: "2026-06-10T11:47:00Z".into(),
        },
        BookmarkState {
            name: "conflict-inbox".into(),
            target: "qvlxnsry".into(),
            remote: Some("origin".into()),
            sync: SyncState::Ahead,
            is_trunk: false,
            is_local: true,
            tracked: true,
            is_merged: false,
            target_description: "feat: conflict inbox empty states".into(),
            target_author: "lauf".into(),
            target_timestamp: "2026-06-08T17:30:00Z".into(),
        },
        // Mid-stack bookmark under diff-virtualization, so the publish
        // flow has a two-segment stack to plan (push, PR-per-segment,
        // base retargeting) on fabricated data.
        BookmarkState {
            name: "diff-model".into(),
            target: "nzpwlxvr".into(),
            remote: Some("origin".into()),
            sync: SyncState::Ahead,
            is_trunk: false,
            is_local: true,
            tracked: true,
            is_merged: false,
            target_description: "feat: sticky file headers in the diff surface".into(),
            target_author: "lauf".into(),
            target_timestamp: "2026-06-09T19:12:00Z".into(),
        },
        // Conflicted: points at the first candidate, like the real
        // backend's state for a bookmark whose ref resolved to multiple
        // targets (the conflict item below carries all candidates).
        BookmarkState {
            name: "watcher-fix".into(),
            target: "b41c77d0".into(),
            remote: None,
            sync: SyncState::LocalOnly,
            is_trunk: false,
            is_local: true,
            tracked: false,
            is_merged: false,
            target_description: "fix: debounce watcher restarts".into(),
            target_author: "lauf".into(),
            target_timestamp: "2026-06-09T18:26:00Z".into(),
        },
        // A local bookmark whose work already landed — it sits on trunk
        // itself, so the bookmarks manager offers it as cleanup.
        BookmarkState {
            name: "swatch-hover-fix".into(),
            target: "uvkmrtpz".into(),
            remote: Some("origin".into()),
            sync: SyncState::Synced,
            is_trunk: false,
            is_local: true,
            tracked: true,
            is_merged: true,
            target_description: "release: cut 0.41 changelog".into(),
            target_author: "maintainers".into(),
            target_timestamp: "2026-06-09T16:40:00Z".into(),
        },
        // Remote branches with no local bookmark — what a fetch leaves
        // behind for work jj never tracked: two teammates mid-flight and
        // one long-landed leftover ripe for cleanup. The teammates'
        // targets are deliberately not in the graph (the default view
        // hides other people's work; the Team lens draws it).
        BookmarkState {
            name: "alice/palette-icons".into(),
            target: "yqosuwnv".into(),
            remote: Some("origin".into()),
            sync: SyncState::RemoteOnly,
            is_trunk: false,
            is_local: false,
            tracked: false,
            is_merged: false,
            target_description: "feat: icon set for the palette rows".into(),
            target_author: "alice".into(),
            target_timestamp: "2026-06-10T07:52:00Z".into(),
        },
        BookmarkState {
            name: "bob/watcher-bench".into(),
            target: "wmxvpkte".into(),
            remote: Some("origin".into()),
            sync: SyncState::RemoteOnly,
            is_trunk: false,
            is_local: false,
            tracked: false,
            is_merged: false,
            target_description: "perf: benchmark harness for watcher restarts".into(),
            target_author: "bob".into(),
            target_timestamp: "2026-05-28T13:10:00Z".into(),
        },
        BookmarkState {
            name: "graph-spike".into(),
            target: "uvkmrtpz".into(),
            remote: Some("origin".into()),
            sync: SyncState::RemoteOnly,
            is_trunk: false,
            is_local: false,
            tracked: false,
            is_merged: true,
            target_description: "release: cut 0.41 changelog".into(),
            target_author: "maintainers".into(),
            target_timestamp: "2026-06-09T16:40:00Z".into(),
        },
    ];

    let workstreams = vec![
        WorkstreamSummary {
            id: "ws-diff-virtualization".into(),
            title: "Diff virtualization".into(),
            node_ids: vec![
                "ktpqsmxw".into(),
                "lwnouzpy".into(),
                "mvkortsq".into(),
                "nzpwlxvr".into(),
            ],
            bookmark: Some("diff-virtualization".into()),
            is_active: true,
            behind_trunk: 0,
        },
        WorkstreamSummary {
            id: "ws-conflict-inbox".into(),
            title: "Conflict inbox".into(),
            node_ids: vec!["qvlxnsry".into(), "pmwzqkvt".into()],
            bookmark: Some("conflict-inbox".into()),
            is_active: false,
            behind_trunk: 4,
        },
        // Each copy of the divergent change is its own head, so the
        // first-mutable-parent walk yields one workstream per copy.
        WorkstreamSummary {
            id: "ws-e93d5a12".into(),
            title: "fix: debounce watcher restarts (simpler timer)".into(),
            node_ids: vec!["e93d5a12".into()],
            bookmark: None,
            is_active: false,
            behind_trunk: 0,
        },
        WorkstreamSummary {
            id: "ws-b41c77d0".into(),
            title: "fix: debounce watcher restarts".into(),
            node_ids: vec!["b41c77d0".into()],
            bookmark: None,
            is_active: false,
            behind_trunk: 0,
        },
        WorkstreamSummary {
            id: "ws-palette-fuzzy".into(),
            title: "feat: fuzzy revset filters in the palette".into(),
            node_ids: vec!["toznkvpq".into(), "swkmlnxu".into()],
            bookmark: None,
            is_active: false,
            behind_trunk: 0,
        },
    ];

    let op = |id: &str, description: &str, timestamp: &str| OperationItem {
        id: id.into(),
        description: description.into(),
        timestamp: timestamp.into(),
        is_current: false,
        user: "lauf@mbp".into(),
        is_snapshot: false,
        effects: vec![],
        more_effects: 0,
    };
    let effect = |kind: OpEffectKind, label: &str| OpEffect {
        kind,
        label: label.into(),
    };

    // Shaped like a real few days: snapshot runs between commands, bookmark
    // and remote effects, and the current op on top.
    let operations = vec![
        OperationItem {
            is_current: true,
            is_snapshot: true,
            ..op("f3c19ad42b71", "snapshot working copy", "2026-06-10T12:21:00Z")
        },
        OperationItem {
            effects: vec![effect(OpEffectKind::WorkingCopy, "working copy moved")],
            ..op(
                "8e02b6d97c44",
                "new empty change on top of lwnouzpy",
                "2026-06-10T11:48:00Z",
            )
        },
        op("5b77e1f00a93", "describe commit lwnouzpy", "2026-06-10T11:47:00Z"),
        OperationItem {
            is_snapshot: true,
            ..op("9c40d18e2f55", "snapshot working copy", "2026-06-10T11:31:00Z")
        },
        OperationItem {
            is_snapshot: true,
            ..op("7d92ce04ab16", "snapshot working copy", "2026-06-10T11:02:00Z")
        },
        OperationItem {
            is_snapshot: true,
            ..op("6e83bf15dc27", "snapshot working copy", "2026-06-10T10:48:00Z")
        },
        op("2d4c90ab38e6", "rebase 3 commits onto uvkmrtpz", "2026-06-10T09:02:00Z"),
        OperationItem {
            effects: vec![
                effect(OpEffectKind::RemoteBookmark, "main@origin updated"),
                effect(OpEffectKind::RemoteBookmark, "release-0.40@origin created"),
            ],
            ..op("0a1f6e83d527", "fetch from origin", "2026-06-09T16:41:00Z")
        },
        OperationItem {
            effects: vec![effect(
                OpEffectKind::RemoteBookmark,
                "conflict-inbox@origin updated",
            )],
            ..op(
                "4b65da72e908",
                "push bookmark conflict-inbox to origin",
                "2026-06-09T16:38:00Z",
            )
        },
        OperationItem {
            effects: vec![effect(OpEffectKind::Bookmark, "conflict-inbox created")],
            ..op(
                "3f51cb89a674",
                "create bookmark conflict-inbox pointing to qvlxnsry",
                "2026-06-08T17:31:00Z",
            )
        },
        OperationItem {
            effects: vec![effect(OpEffectKind::WorkingCopy, "working copy moved")],
            ..op("1c30ba56f493", "edit commit pmwzqkvt", "2026-06-08T14:02:00Z")
        },
    ];

    RepoSnapshot {
        repo_path: repo_path.display().to_string(),
        repo_name,
        backend: "mock".into(),
        view: SnapshotView::default(),
        truncated: false,
        trunk_bookmark: "main".into(),
        working_copy: "ktpqsmxw".into(),
        workspaces: vec![
            WorkspaceSummary {
                name: "default".into(),
                is_default: true,
                is_current: true,
                is_stale: false,
                working_copy_node: Some("ktpqsmxw".into()),
            },
            // A sibling workspace parked on the conflict-inbox head went
            // stale when the rebase moved the repo underneath it. Its card
            // gets no recovery action — like the real backend, only the
            // current workspace can be updated from here.
            WorkspaceSummary {
                name: "review".into(),
                is_default: false,
                is_current: false,
                is_stale: true,
                working_copy_node: Some("qvlxnsry".into()),
            },
        ],
        workstreams,
        nodes,
        bookmarks,
        // A GitHub-shaped remote so forge detection has something to find
        // on fabricated data (the owner/name are obviously not real).
        git_remotes: vec![GitRemote {
            name: "origin".into(),
            url: "https://github.com/jiji-mock/workbench.git".into(),
        }],
        conflicts: mock_conflicts(),
        operations,
        // The fabricated machine has Sublime Merge installed, so every
        // Resolve affordance renders (the stub/harness replay the mutation).
        resolve_tool: Some("smerge".into()),
    }
}

/// The inbox items matching the fabricated graph: the rebase-conflicted
/// stack (the child inherits the parent's unresolved file), the bookmark
/// that resolved to both copies of the divergent change, and the stale
/// `review` workspace (mock-only until the real backend detects staleness).
fn mock_conflicts() -> Vec<ConflictItem> {
    let file_item = |id: &str, summary: &str, node: &str, paths: &[&str]| ConflictItem {
        id: format!("file-{id}"),
        kind: ConflictKind::File,
        summary: summary.into(),
        node_id: Some(node.into()),
        paths: paths.iter().map(|p| p.to_string()).collect(),
        more_paths: 0,
        targets: Vec::new(),
        workspace: None,
    };
    vec![
        file_item(
            "qvlxnsry",
            "\u{201c}feat: conflict inbox empty states\u{201d} has unresolved file conflicts",
            "qvlxnsry",
            &["src/lib/components/conflicts/ConflictList.svelte"],
        ),
        file_item(
            "pmwzqkvt",
            "\u{201c}wip: conflict inbox list skeleton\u{201d} has unresolved file conflicts",
            "pmwzqkvt",
            &["src/lib/components/conflicts/ConflictList.svelte"],
        ),
        ConflictItem {
            id: "bookmark-watcher-fix".into(),
            kind: ConflictKind::Bookmark,
            summary: "Bookmark \u{201c}watcher-fix\u{201d} resolved to multiple targets and needs to be repointed".into(),
            node_id: None,
            paths: Vec::new(),
            more_paths: 0,
            targets: vec!["b41c77d0".into(), "e93d5a12".into()],
            workspace: None,
        },
        ConflictItem {
            id: "workspace-review".into(),
            kind: ConflictKind::StaleWorkspace,
            summary: "Workspace \u{201c}review\u{201d} is stale: the repo has moved since its working copy was last updated".into(),
            node_id: Some("qvlxnsry".into()),
            paths: Vec::new(),
            more_paths: 0,
            targets: Vec::new(),
            workspace: Some("review".into()),
        },
    ]
}

/// The one conflicted path in the fabricated graph (see `mock_conflicts`):
/// the wip change collided here when the stack was rebased onto 0.41. Its
/// child inherits the conflict without touching the file; the real backend
/// appends such paths to the child's file list and diff anyway, so both
/// changes list it — exactly like the real backend.
fn is_mock_conflicted(change_id: &str, path: &str) -> bool {
    (change_id == "pmwzqkvt" || change_id == "qvlxnsry")
        && path == "src/lib/components/conflicts/ConflictList.svelte"
}

/// Changed-file lists matching the mock graph, keyed by change id. `None`
/// for ids the mock snapshot never produced.
/// The fixture evolution chain for one change, newest first — the full
/// list including the current version. Only `lwnouzpy` carries a story
/// (rebase → two working-copy snapshots → final describe, with op ids
/// matching the fixture operation log); every other change answers a
/// single-entry chain built from its live node.
fn fixture_evolog_entries(change_id: &str) -> Vec<EvologEntry> {
    if change_id != "lwnouzpy" {
        return Vec::new();
    }
    let entry = |commit_id: &str,
                 description: &str,
                 timestamp: &str,
                 op: (&str, &str, &str)| EvologEntry {
        commit_id: commit_id.into(),
        description: description.into(),
        author: "lauf".into(),
        timestamp: timestamp.into(),
        op_id: Some(op.0.into()),
        op_description: Some(op.1.into()),
        op_timestamp: Some(op.2.into()),
    };
    vec![
        entry(
            "c5e208d4",
            "feat: virtualize multi-file diff rendering",
            "2026-06-10T11:47:00Z",
            (
                "5b77e1f00a93",
                "describe commit lwnouzpy",
                "2026-06-10T11:47:00Z",
            ),
        ),
        entry(
            "b7f2c9d1",
            "wip: virtualize diff rows",
            "2026-06-10T11:31:00Z",
            (
                "9c40d18e2f55",
                "snapshot working copy",
                "2026-06-10T11:31:00Z",
            ),
        ),
        entry(
            "9e12ff30",
            "wip: virtualize diff rows",
            "2026-06-10T11:02:00Z",
            (
                "7d92ce04ab16",
                "snapshot working copy",
                "2026-06-10T11:02:00Z",
            ),
        ),
        entry(
            "4a81d0f2",
            "wip: virtualize diff rows",
            "2026-06-10T09:02:00Z",
            (
                "2d4c90ab38e6",
                "rebase 3 commits onto uvkmrtpz",
                "2026-06-10T09:02:00Z",
            ),
        ),
    ]
}

/// One change's evolution over the fabricated data: the fixture chain (or
/// a single live-node entry for changes without one), with every active
/// replayed restore stacked on top as the new current version — so the
/// panel sees the version a restore just recorded, like the real backend.
pub fn mock_change_evolog(
    snapshot: &RepoSnapshot,
    mutations: &[MockMutation],
    active: &[bool],
    change_id: &str,
) -> Result<ChangeEvolog, BackendError> {
    let node = snapshot
        .nodes
        .iter()
        .find(|n| n.id == change_id)
        .ok_or_else(|| BackendError::ChangeMissing(change_id.to_owned()))?;
    let mut entries = fixture_evolog_entries(change_id);
    if entries.is_empty() {
        // Every change has at least its current version. Mutation-created
        // nodes and the rest of the fixture graph show just that.
        entries.push(EvologEntry {
            commit_id: node.commit_id.clone(),
            description: node.description.clone(),
            author: node.author.clone(),
            timestamp: node.timestamp.clone(),
            op_id: None,
            op_description: None,
            op_timestamp: None,
        });
    }
    for (index, mutation) in mutations.iter().enumerate() {
        if !active[index] {
            continue;
        }
        let MockMutation::RestoreVersion { id, commit } = mutation else {
            continue;
        };
        if id != change_id {
            continue;
        }
        let description = entries
            .iter()
            .find(|e| e.commit_id == *commit)
            .map(|e| e.description.clone())
            .unwrap_or_default();
        let prior_head = entries.first().map(|e| e.commit_id.clone()).unwrap_or_default();
        entries.insert(
            0,
            EvologEntry {
                commit_id: mock_version_commit_id(index),
                description,
                author: node.author.clone(),
                timestamp: mock_op_timestamp(index),
                op_id: Some(mock_operation_id(index)),
                op_description: Some(format!(
                    "restore commit {prior_head} to version {commit}"
                )),
                op_timestamp: Some(mock_op_timestamp(index)),
            },
        );
    }
    Ok(ChangeEvolog {
        id: change_id.to_owned(),
        entries,
        truncated: false,
    })
}

/// One annotated file over the fabricated data: the diff-virtualization
/// stack's `DiffViewport.svelte`, its lines attributed across the stack's
/// changes plus trunk (a multi-author fixture — trunk is "maintainers").
/// Any other path answers the same does-not-exist refusal as the real
/// backend, and an unknown change refuses like every other per-change read.
pub fn mock_file_annotation(
    snapshot: &RepoSnapshot,
    change_id: &str,
    file_path: &str,
) -> Result<FileAnnotation, BackendError> {
    if !snapshot.nodes.iter().any(|n| n.id == change_id) {
        return Err(BackendError::ChangeMissing(change_id.to_owned()));
    }
    if file_path != "src/lib/components/diff/DiffViewport.svelte" {
        return Err(BackendError::SnapshotFailed(format!(
            "{file_path} does not exist in {change_id}"
        )));
    }
    let origin = |id: &str, commit: &str, summary: &str, author: &str, ts: &str| AnnotateOrigin {
        id: id.to_owned(),
        change_id: id.to_owned(),
        commit_id: commit.to_owned(),
        summary: summary.to_owned(),
        author: author.to_owned(),
        timestamp: ts.to_owned(),
    };
    let origins = vec![
        origin(
            "nzpwlxvr",
            "8b19f3c2",
            "feat: sticky file headers in the diff surface",
            "lauf",
            "2026-06-09T19:12:00Z",
        ),
        origin(
            "lwnouzpy",
            "c5e208d4",
            "feat: virtualize multi-file diff rendering",
            "lauf",
            "2026-06-10T11:47:00Z",
        ),
        origin("ktpqsmxw", "9a37be51", "", "lauf", "2026-06-10T12:21:00Z"),
        origin(
            "uvkmrtpz",
            "e7c41a9f",
            "release: cut 0.41 changelog",
            "maintainers",
            "2026-06-09T16:40:00Z",
        ),
    ];
    let lines: &[(u32, &str)] = &[
        (0, "<script lang=\"ts\">"),
        (0, "  import { onMount } from \"svelte\";"),
        (1, "  import VirtualDiffList from \"./VirtualDiffList.svelte\";"),
        (0, "  import FileHeader from \"./FileHeader.svelte\";"),
        (0, ""),
        (0, "  let { files } = $props();"),
        (0, "  let viewport = $state(null);"),
        (2, "  const visible = $derived("),
        (2, "    rows.slice(range.first, range.last + 1)"),
        (2, "  );"),
        (0, ""),
        (0, "  function onScroll(event: Event) {"),
        (2, "    if (!viewport) return;"),
        (0, "    schedule(measure);"),
        (0, "  }"),
        (0, "</script>"),
        (3, ""),
        (3, "<div class=\"viewport\" bind:this={viewport}>"),
        (1, "  <VirtualDiffList {files} {viewport} />"),
        (3, "</div>"),
    ];
    Ok(FileAnnotation {
        id: change_id.to_owned(),
        path: file_path.to_owned(),
        origins,
        lines: lines
            .iter()
            .map(|(origin, text)| AnnotateLine {
                origin: *origin,
                text: (*text).to_owned(),
            })
            .collect(),
        truncated: false,
    })
}

pub fn mock_change_detail(change_id: &str) -> Option<ChangeDetail> {
    use FileStatus::*;
    let files: &[(&str, FileStatus)] = match change_id {
        "ktpqsmxw" => &[
            ("src/lib/components/diff/DiffViewport.svelte", Modified),
            ("src/lib/styles/tokens.css", Modified),
        ],
        "lwnouzpy" => &[
            ("src/lib/components/diff/DiffViewport.svelte", Modified),
            ("src/lib/components/diff/VirtualDiffList.svelte", Added),
            ("src/lib/components/diff/virtual.test.ts", Added),
            ("src/lib/components/views/DiffView.svelte", Modified),
        ],
        "mvkortsq" => &[
            ("src/lib/components/diff/DiffRow.svelte", Modified),
            ("src/lib/components/diff/measure.ts", Renamed),
            ("src/lib/components/diff/rowHeightCache.ts", Removed),
        ],
        "nzpwlxvr" => &[
            ("src/lib/components/diff/FileHeader.svelte", Added),
            ("src/lib/components/diff/DiffViewport.svelte", Modified),
            ("src/lib/styles/global.css", Modified),
            ("static/fonts/jiji-mono.woff2", Modified),
            ("static/textures/header-grain.png", Modified),
        ],
        "qvlxnsry" => &[
            ("src/lib/components/conflicts/ConflictList.svelte", Modified),
            ("src/lib/components/conflicts/EmptyInbox.svelte", Added),
            ("src/lib/components/views/ConflictsView.svelte", Modified),
        ],
        "pmwzqkvt" => &[
            ("src/lib/components/conflicts/ConflictList.svelte", Added),
            ("src/lib/components/conflicts/ConflictRow.svelte", Added),
        ],
        "b41c77d0" | "e93d5a12" => &[("src/lib/state/watcher.ts", Modified)],
        "uvkmrtpz" => &[("CHANGELOG.md", Modified), ("package.json", Modified)],
        _ => return None,
    };
    Some(ChangeDetail {
        id: change_id.to_owned(),
        files: files
            .iter()
            .map(|(path, status)| ChangedFile {
                path: (*path).to_owned(),
                status: *status,
                renamed_from: None,
                has_conflict: is_mock_conflicted(change_id, path),
            })
            .collect(),
        truncated: false,
    })
}

fn seg(text: &str, changed: bool) -> DiffSegment {
    DiffSegment {
        text: text.into(),
        changed,
    }
}

/// A whole line of one kind: context lines are one unchanged segment,
/// added/removed lines one changed segment (no intraline counterpart).
fn line(kind: DiffLineKind, text: &str) -> DiffLine {
    DiffLine {
        kind,
        segments: vec![seg(text, kind != DiffLineKind::Context)],
        pair: None,
    }
}

fn intraline(kind: DiffLineKind, parts: &[(&str, bool)]) -> DiffLine {
    DiffLine {
        kind,
        segments: parts.iter().map(|(text, changed)| seg(text, *changed)).collect(),
        pair: None,
    }
}

/// Mirror the real backend's line pairing on hand-authored mock lines:
/// within each changed block, the k-th removed line carrying intraline
/// segments pairs with the k-th such added line.
fn pair_intraline_lines(lines: &mut [DiffLine]) {
    fn mixed(line: &DiffLine) -> bool {
        line.segments.iter().any(|s| s.changed) && line.segments.iter().any(|s| !s.changed)
    }
    let mut i = 0;
    while i < lines.len() {
        if lines[i].kind == DiffLineKind::Context {
            i += 1;
            continue;
        }
        let start = i;
        while i < lines.len() && lines[i].kind != DiffLineKind::Context {
            i += 1;
        }
        let removed: Vec<usize> = (start..i)
            .filter(|&k| lines[k].kind == DiffLineKind::Removed && mixed(&lines[k]))
            .collect();
        let added: Vec<usize> = (start..i)
            .filter(|&k| lines[k].kind == DiffLineKind::Added && mixed(&lines[k]))
            .collect();
        for (&r, &a) in removed.iter().zip(&added) {
            lines[r].pair = Some(a as u32);
            lines[a].pair = Some(r as u32);
        }
    }
}

fn hunk(old_start: u32, new_start: u32, mut lines: Vec<DiffLine>) -> DiffHunk {
    pair_intraline_lines(&mut lines);
    DiffHunk {
        old_start,
        new_start,
        context: None,
        lines,
    }
}

fn text(hunks: Vec<DiffHunk>) -> FileDiffContent {
    FileDiffContent::Text {
        hunks,
        truncated: false,
    }
}

/// Every line of a freshly added file, as one hunk.
fn added_file(lines: &[&str]) -> FileDiffContent {
    text(vec![hunk(
        1,
        1,
        lines.iter().map(|l| line(DiffLineKind::Added, l)).collect(),
    )])
}

fn removed_file(lines: &[&str]) -> FileDiffContent {
    text(vec![hunk(
        1,
        1,
        lines.iter().map(|l| line(DiffLineKind::Removed, l)).collect(),
    )])
}

/// The conflicted component both sides of the 0.41 rebase added, as the
/// structural conflict content the real backend ships: one region, each
/// side its diff from the (absent) base — so both sides read as all-added.
/// Served for the conflicted change itself and for its child, which
/// inherits the identical conflicted entry (the real backend appends such
/// paths to the diff even though the parent-relative diff skips them).
fn conflict_list_content() -> FileDiffContent {
    use DiffLineKind::Added;
    let side = |lines: &[&str]| ConflictSideDiff {
        label: None,
        lines: lines.iter().map(|l| line(Added, l)).collect(),
    };
    FileDiffContent::Conflict {
        sections: vec![ConflictSection::Region {
            sides: vec![
                side(&[
                    "<script lang=\"ts\">",
                    "  import ConflictRow from \"./ConflictRow.svelte\";",
                    "  let { items } = $props();",
                    "</script>",
                    "",
                    "{#each items as item (item.id)}",
                    "  <ConflictRow {item} />",
                    "{/each}",
                ]),
                side(&[
                    "<script lang=\"ts\">",
                    "  let { conflicts } = $props();",
                    "</script>",
                    "",
                    "{#each conflicts as conflict}",
                    "  <p class=\"row\">{conflict.summary}</p>",
                    "{/each}",
                ]),
            ],
        }],
        truncated: false,
    }
}

// Tiny real PNGs (grain textures, 48x48 and 64x64) so the mock repo
// exercises the image-diff surface with data the webview can render.
const GRAIN_OLD_PNG_B64: &str = "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\
     QmCC";

const GRAIN_NEW_PNG_B64: &str = "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\
     LfHLCOQAAAAASUVORK5CYII=";

/// Content diffs matching the `mock_change_detail` file lists, keyed by
/// change id. Shaped to exercise the diff surface: multi-hunk files,
/// intraline edits, added/removed files, and a binary entry.
pub fn mock_change_diff(change_id: &str) -> Option<ChangeDiff> {
    use DiffLineKind::{Added, Context, Removed};
    let file = |path: &str, status: FileStatus, content: FileDiffContent| FileDiff {
        path: path.to_owned(),
        status,
        renamed_from: None,
        has_conflict: false,
        content,
    };

    let files: Vec<FileDiff> = match change_id {
        "ktpqsmxw" => vec![
            file(
                "src/lib/components/diff/DiffViewport.svelte",
                FileStatus::Modified,
                text(vec![
                    hunk(
                        38,
                        38,
                        vec![
                            line(Context, "  const visible = $derived("),
                            intraline(
                                Removed,
                                &[("    rows.slice(range.", false), ("start", true), (", range.", false), ("end", true), (")", false)],
                            ),
                            intraline(
                                Added,
                                &[("    rows.slice(range.", false), ("first", true), (", range.", false), ("last + 1", true), (")", false)],
                            ),
                            line(Context, "  );"),
                        ],
                    ),
                    hunk(
                        71,
                        71,
                        vec![
                            line(Context, "  function onScroll(event: Event) {"),
                            line(Added, "    if (!viewport) return;"),
                            line(Context, "    schedule(measure);"),
                            line(Context, "  }"),
                        ],
                    ),
                ]),
            ),
            file(
                "src/lib/styles/tokens.css",
                FileStatus::Modified,
                text(vec![hunk(
                    52,
                    52,
                    vec![
                        line(Context, "  --radius-l: 10px;"),
                        line(Added, "  --diff-row-h: 20px;"),
                        line(Context, "  --radius-xl: 14px;"),
                    ],
                )]),
            ),
        ],
        "lwnouzpy" => vec![
            file(
                "src/lib/components/diff/DiffViewport.svelte",
                FileStatus::Modified,
                text(vec![
                    hunk(
                        4,
                        4,
                        vec![
                            line(Context, "  import { onMount } from \"svelte\";"),
                            line(Added, "  import VirtualDiffList from \"./VirtualDiffList.svelte\";"),
                            line(Context, "  import FileHeader from \"./FileHeader.svelte\";"),
                        ],
                    ),
                    hunk(
                        59,
                        60,
                        vec![
                            line(Context, "<div class=\"viewport\" bind:this={viewport}>"),
                            line(Removed, "  {#each files as file (file.path)}"),
                            line(Removed, "    <FileSection {file} />"),
                            line(Removed, "  {/each}"),
                            line(Added, "  <VirtualDiffList {files} {viewport} />"),
                            line(Context, "</div>"),
                        ],
                    ),
                ]),
            ),
            file(
                "src/lib/components/diff/VirtualDiffList.svelte",
                FileStatus::Added,
                added_file(&[
                    "<script lang=\"ts\">",
                    "  import { measureRows } from \"./measure\";",
                    "",
                    "  let { files, viewport } = $props();",
                    "  const rows = $derived(measureRows(files));",
                    "</script>",
                    "",
                    "{#each rows as row (row.key)}",
                    "  <div class=\"row\" style:height=\"{row.height}px\">{row.render()}</div>",
                    "{/each}",
                ]),
            ),
            file(
                "src/lib/components/diff/virtual.test.ts",
                FileStatus::Added,
                added_file(&[
                    "import { describe, expect, it } from \"vitest\";",
                    "import { measureRows } from \"./measure\";",
                    "",
                    "describe(\"measureRows\", () => {",
                    "  it(\"accounts for sticky headers\", () => {",
                    "    expect(measureRows([]).length).toBe(0);",
                    "  });",
                    "});",
                ]),
            ),
            file(
                "src/lib/components/views/DiffView.svelte",
                FileStatus::Modified,
                text(vec![hunk(
                    18,
                    18,
                    vec![
                        line(Context, "  const stats = $derived(totalStats(files));"),
                        intraline(
                            Removed,
                            &[("  const overscan = ", false), ("4", true), (";", false)],
                        ),
                        intraline(
                            Added,
                            &[("  const overscan = ", false), ("12", true), (";", false)],
                        ),
                        line(Context, "</script>"),
                    ],
                )]),
            ),
        ],
        "mvkortsq" => vec![
            file(
                "src/lib/components/diff/DiffRow.svelte",
                FileStatus::Modified,
                text(vec![hunk(
                    9,
                    9,
                    vec![
                        line(Context, "  let { row } = $props();"),
                        intraline(
                            Removed,
                            &[("  const height = ", false), ("row.lines * LINE_H", true), (";", false)],
                        ),
                        intraline(
                            Added,
                            &[("  const height = ", false), ("measure(row)", true), (";", false)],
                        ),
                        line(Context, ""),
                    ],
                )]),
            ),
            // A rename with a small edit, as copy tracing reports it: the
            // moved file diffs against its source instead of re-adding
            // every line.
            FileDiff {
                path: "src/lib/components/diff/measure.ts".to_owned(),
                status: FileStatus::Renamed,
                renamed_from: Some("src/lib/components/diff/rowHeight.ts".to_owned()),
                has_conflict: false,
                content: text(vec![hunk(
                    1,
                    1,
                    vec![
                        line(Context, "export const LINE_H = 20;"),
                        intraline(
                            Removed,
                            &[("export function ", false), ("rowHeight", true), ("(lines: number): number {", false)],
                        ),
                        intraline(
                            Added,
                            &[("export function ", false), ("measure", true), ("(lines: number): number {", false)],
                        ),
                        line(Context, "  return lines * LINE_H;"),
                        line(Context, "}"),
                    ],
                )]),
            },
            file(
                "src/lib/components/diff/rowHeightCache.ts",
                FileStatus::Removed,
                removed_file(&[
                    "const cache = new Map<string, number>();",
                    "export function cachedRowHeight(key: string): number | undefined {",
                    "  return cache.get(key);",
                    "}",
                ]),
            ),
        ],
        "nzpwlxvr" => vec![
            file(
                "src/lib/components/diff/FileHeader.svelte",
                FileStatus::Added,
                added_file(&[
                    "<script lang=\"ts\">",
                    "  let { file } = $props();",
                    "</script>",
                    "",
                    "<header class=\"file-header\">",
                    "  <span class=\"path mono\">{file.path}</span>",
                    "</header>",
                    "",
                    "<style>",
                    "  .file-header {",
                    "    position: sticky;",
                    "    top: 0;",
                    "  }",
                    "</style>",
                ]),
            ),
            file(
                "src/lib/components/diff/DiffViewport.svelte",
                FileStatus::Modified,
                text(vec![hunk(
                    44,
                    44,
                    vec![
                        line(Context, "  {#each files as file (file.path)}"),
                        line(Added, "    <FileHeader {file} />"),
                        line(Context, "    <FileSection {file} />"),
                        line(Context, "  {/each}"),
                    ],
                )]),
            ),
            file(
                "src/lib/styles/global.css",
                FileStatus::Modified,
                text(vec![hunk(
                    88,
                    88,
                    vec![
                        line(Context, ".kbd {"),
                        line(Context, "  font-family: var(--font-mono);"),
                        line(Added, "  font-variant-numeric: tabular-nums;"),
                        line(Context, "  font-size: var(--text-xs);"),
                    ],
                )]),
            ),
            file(
                "static/fonts/jiji-mono.woff2",
                FileStatus::Modified,
                FileDiffContent::Binary {
                    old_size: Some(48_120),
                    new_size: Some(49_384),
                },
            ),
            file(
                "static/textures/header-grain.png",
                FileStatus::Modified,
                FileDiffContent::Image {
                    old: Some(ImageContent {
                        mime: "image/png".to_owned(),
                        size: 1947,
                        data: Some(GRAIN_OLD_PNG_B64.to_owned()),
                    }),
                    new: Some(ImageContent {
                        mime: "image/png".to_owned(),
                        size: 3527,
                        data: Some(GRAIN_NEW_PNG_B64.to_owned()),
                    }),
                },
            ),
        ],
        "qvlxnsry" => vec![
            // Inherited from the conflicted parent: the child never touched
            // the file, so the parent-relative diff skips it — the real
            // backend appends such conflicted paths so the change still
            // shows what is unresolved in its tree.
            FileDiff {
                has_conflict: true,
                ..file(
                    "src/lib/components/conflicts/ConflictList.svelte",
                    FileStatus::Modified,
                    conflict_list_content(),
                )
            },
            file(
                "src/lib/components/conflicts/EmptyInbox.svelte",
                FileStatus::Added,
                added_file(&[
                    "<div class=\"empty\">",
                    "  <h3>No conflicts</h3>",
                    "  <p>Everything merges cleanly. Carry on.</p>",
                    "</div>",
                ]),
            ),
            file(
                "src/lib/components/views/ConflictsView.svelte",
                FileStatus::Modified,
                text(vec![hunk(
                    12,
                    12,
                    vec![
                        line(Context, "{#if conflicts.length === 0}"),
                        intraline(
                            Removed,
                            &[("  <p>", false), ("Nothing here yet", true), ("</p>", false)],
                        ),
                        intraline(Added, &[("  <", false), ("EmptyInbox /", true), (">", false)]),
                        line(Context, "{/if}"),
                    ],
                )]),
            ),
        ],
        "pmwzqkvt" => vec![
            // Conflicted: both sides of the rebase added this component;
            // the content arrives as the structural conflict sections the
            // real backend ships for an unresolved file.
            FileDiff {
                has_conflict: true,
                ..file(
                    "src/lib/components/conflicts/ConflictList.svelte",
                    FileStatus::Added,
                    conflict_list_content(),
                )
            },
            file(
                "src/lib/components/conflicts/ConflictRow.svelte",
                FileStatus::Added,
                added_file(&[
                    "<script lang=\"ts\">",
                    "  let { item } = $props();",
                    "</script>",
                    "",
                    "<div class=\"conflict-row\">{item.summary}</div>",
                ]),
            ),
        ],
        // The two copies of the divergent change: the same fix, written two
        // ways — comparing them side by side is how a user picks a winner.
        "b41c77d0" => vec![file(
            "src/lib/state/watcher.ts",
            FileStatus::Modified,
            text(vec![hunk(
                21,
                21,
                vec![
                    line(Context, "export function scheduleRefresh(repo: string) {"),
                    intraline(
                        Removed,
                        &[("  refresh(repo)", true), (";", false)],
                    ),
                    intraline(
                        Added,
                        &[("  debounce(() => refresh(repo), 400)", true), (";", false)],
                    ),
                    line(Context, "}"),
                ],
            )]),
        )],
        "e93d5a12" => vec![file(
            "src/lib/state/watcher.ts",
            FileStatus::Modified,
            text(vec![hunk(
                21,
                21,
                vec![
                    line(Context, "export function scheduleRefresh(repo: string) {"),
                    intraline(
                        Removed,
                        &[("  refresh(repo)", true), (";", false)],
                    ),
                    line(Added, "  clearTimeout(timer);"),
                    line(Added, "  timer = setTimeout(() => refresh(repo), 400);"),
                    line(Context, "}"),
                ],
            )]),
        )],
        "uvkmrtpz" => vec![
            file(
                "CHANGELOG.md",
                FileStatus::Modified,
                text(vec![hunk(
                    1,
                    1,
                    vec![
                        line(Context, "# Changelog"),
                        line(Context, ""),
                        line(Added, "## 0.41"),
                        line(Added, ""),
                        line(Added, "- sticky file headers in the diff surface"),
                        line(Added, "- conflict inbox skeleton"),
                        line(Added, ""),
                        line(Context, "## 0.40"),
                    ],
                )]),
            ),
            file(
                "package.json",
                FileStatus::Modified,
                text(vec![hunk(
                    2,
                    2,
                    vec![
                        line(Context, "  \"name\": \"jiji\","),
                        intraline(
                            Removed,
                            &[("  \"version\": \"0.", false), ("40", true), (".0\",", false)],
                        ),
                        intraline(
                            Added,
                            &[("  \"version\": \"0.", false), ("41", true), (".0\",", false)],
                        ),
                        line(Context, "  \"private\": true,"),
                    ],
                )]),
            ),
        ],
        _ => return None,
    };

    Some(ChangeDiff {
        id: change_id.to_owned(),
        from: None,
        files,
        truncated: false,
    })
}

/// Fabricated commit-to-commit comparison: when `from` is an ancestor of
/// `to`, the per-change diffs along the first-parent chain combine
/// (last-touch-wins per path, with adds staying adds), which reads
/// plausibly as "everything between the two". Any other direction falls
/// back to the target's own diff — good enough for UI work; the real
/// backend diffs the trees directly.
/// Version-transition diffs for the fixture evolog chain (`lwnouzpy`):
/// what arriving at each version changed, keyed by the newer version's
/// commit id, with wider spans unioned along the chain under the same
/// merge rule as `mock_compare_diff` below (newest touch wins per path,
/// adds stay adds). The graph carries no nodes for these hidden commit
/// ids, so the node-based comparison cannot answer them.
fn fixture_version_compare(from_id: &str, to_id: &str) -> Option<ChangeDiff> {
    use DiffLineKind::{Added, Context, Removed};
    const CHAIN: [&str; 4] = ["4a81d0f2", "9e12ff30", "b7f2c9d1", "c5e208d4"];
    let from = CHAIN.iter().position(|id| *id == from_id)?;
    let to = CHAIN.iter().position(|id| *id == to_id)?;

    let file = |path: &str, status: FileStatus, content: FileDiffContent| FileDiff {
        path: path.to_owned(),
        status,
        renamed_from: None,
        has_conflict: false,
        content,
    };
    // The oldest version is the chain's floor; the newest step (the
    // describe) touched no files — the surface renders its description
    // move instead.
    let step = |commit: &str| -> Vec<FileDiff> {
        match commit {
            "9e12ff30" => vec![
                file(
                    "src/lib/components/diff/DiffViewport.svelte",
                    FileStatus::Modified,
                    text(vec![hunk(
                        4,
                        4,
                        vec![
                            line(Context, "  import { onMount } from \"svelte\";"),
                            line(
                                Added,
                                "  import VirtualDiffList from \"./VirtualDiffList.svelte\";",
                            ),
                            line(Context, "  import FileHeader from \"./FileHeader.svelte\";"),
                        ],
                    )]),
                ),
                file(
                    "src/lib/components/diff/VirtualDiffList.svelte",
                    FileStatus::Added,
                    added_file(&[
                        "<script lang=\"ts\">",
                        "  let { files, viewport } = $props();",
                        "  const rows = $derived(files.flatMap(fileRows));",
                        "</script>",
                        "",
                        "{#each rows as row (row.key)}",
                        "  <DiffRow {row} />",
                        "{/each}",
                    ]),
                ),
            ],
            "b7f2c9d1" => vec![
                file(
                    "src/lib/components/diff/VirtualDiffList.svelte",
                    FileStatus::Modified,
                    text(vec![hunk(
                        2,
                        2,
                        vec![
                            line(Context, "  let { files, viewport } = $props();"),
                            intraline(
                                Removed,
                                &[
                                    ("  const rows = $derived(files.flatMap(fileRows)", false),
                                    (");", true),
                                ],
                            ),
                            intraline(
                                Added,
                                &[
                                    ("  const rows = $derived(files.flatMap(fileRows)", false),
                                    (".slice(range.first, range.last + 1));", true),
                                ],
                            ),
                            line(
                                Added,
                                "  const range = $derived(visibleRange(viewport, rows.length));",
                            ),
                        ],
                    )]),
                ),
                file(
                    "src/lib/components/diff/virtual.test.ts",
                    FileStatus::Added,
                    added_file(&[
                        "import { describe, expect, it } from \"vitest\";",
                        "import { visibleRange } from \"./virtual\";",
                        "",
                        "describe(\"visibleRange\", () => {",
                        "  it(\"windows rows to the viewport\", () => {",
                        "    expect(visibleRange(vp(600), 5000).last).toBeLessThan(80);",
                        "  });",
                        "});",
                    ]),
                ),
                file(
                    "src/lib/components/views/DiffView.svelte",
                    FileStatus::Modified,
                    text(vec![hunk(
                        112,
                        112,
                        vec![
                            line(Context, "  <div class=\"scroller\">"),
                            line(Removed, "    <FileList {files} />"),
                            line(Added, "    <DiffViewport {files} />"),
                            line(Context, "  </div>"),
                        ],
                    )]),
                ),
            ],
            _ => Vec::new(),
        }
    };

    let mut files: Vec<FileDiff> = Vec::new();
    for commit in CHAIN.iter().take(to + 1).skip(from + 1) {
        for incoming in step(commit) {
            match files.iter_mut().find(|f| f.path == incoming.path) {
                Some(existing) => {
                    let added_in_span = existing.status == FileStatus::Added
                        && incoming.status != FileStatus::Removed;
                    *existing = incoming;
                    if added_in_span {
                        existing.status = FileStatus::Added;
                    }
                }
                None => files.push(incoming),
            }
        }
    }
    files.sort_by(|a, b| a.path.cmp(&b.path));
    Some(ChangeDiff {
        id: to_id.to_owned(),
        from: Some(from_id.to_owned()),
        files,
        truncated: false,
    })
}

pub fn mock_compare_diff(
    snapshot: &RepoSnapshot,
    from_id: &str,
    to_id: &str,
) -> Result<ChangeDiff, BackendError> {
    // The evolution surface compares hidden versions of one change by
    // commit id; the fixture chain answers those before the node lookup
    // gets a say.
    if let Some(diff) = fixture_version_compare(from_id, to_id) {
        return Ok(diff);
    }
    for id in [from_id, to_id] {
        if !snapshot.nodes.iter().any(|n| n.id == id) {
            return Err(BackendError::ChangeMissing(id.to_owned()));
        }
    }

    // The first-parent chain from `to` down to (but excluding) `from`,
    // oldest first so later changes overwrite earlier ones per path.
    let mut chain = Vec::new();
    let mut cursor = to_id.to_owned();
    let reached_from = loop {
        if cursor == from_id {
            break true;
        }
        chain.push(cursor.clone());
        let Some(node) = snapshot.nodes.iter().find(|n| n.id == cursor) else {
            break false;
        };
        match node.parents.first().or_else(|| node.elided_parents.first()) {
            Some(parent) => cursor = parent.clone(),
            None => break false,
        }
    };
    if !reached_from {
        chain = vec![to_id.to_owned()];
    }

    let mut files: Vec<FileDiff> = Vec::new();
    for id in chain.iter().rev() {
        for file in mock_change_diff(id).map(|d| d.files).unwrap_or_default() {
            match files.iter_mut().find(|f| f.path == file.path) {
                Some(existing) => {
                    // A file added earlier in the span is still an add for
                    // the comparison as a whole, whatever touched it later.
                    let added_in_span = existing.status == FileStatus::Added
                        && file.status != FileStatus::Removed;
                    *existing = file;
                    if added_in_span {
                        existing.status = FileStatus::Added;
                    }
                }
                None => files.push(file),
            }
        }
    }
    files.sort_by(|a, b| a.path.cmp(&b.path));

    Ok(ChangeDiff {
        id: to_id.to_owned(),
        from: Some(from_id.to_owned()),
        files,
        truncated: false,
    })
}
