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
    Squash { id: String },
    Rebase { id: String, destination: String, with_descendants: bool },
    CreateBookmark { name: String, target: String },
    MoveBookmark { name: String, target: String },
    RenameBookmark { old: String, new: String },
    DeleteBookmark { name: String },
    RevertOp { op_id: String },
    RestoreOp { op_id: String },
}

impl MockMutation {
    /// Revert/restore entries time-travel other entries instead of touching
    /// the graph themselves.
    fn is_time_travel(&self) -> bool {
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
            if node.parents.iter().any(|p| *p == current) && seen.insert(node.id.clone()) {
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
            snapshot.bookmarks.push(BookmarkState {
                name: name.clone(),
                target: target.clone(),
                remote: None,
                sync: SyncState::LocalOnly,
                is_trunk: false,
                is_local: true,
            });
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
    }
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
        MockMutation::Squash { id } => {
            let parent = snapshot
                .nodes
                .iter()
                .find(|n| n.id == *id)
                .and_then(|n| n.parents.first().cloned())
                .unwrap_or_default();
            format!("squash commits into {}", commit_of(&parent))
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
        MockMutation::RevertOp { op_id } => format!("revert operation {op_id}"),
        MockMutation::RestoreOp { op_id } => format!("restore to operation {op_id}"),
    }
}

fn wc_moved_effect() -> OpEffect {
    OpEffect {
        kind: OpEffectKind::WorkingCopy,
        label: "working copy moved".into(),
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
            &[],
        ),
        node(
            "qvlxnsry",
            "d2a6f4e8",
            "feat: conflict inbox empty states",
            "lauf",
            "2026-06-08T17:30:00Z",
            NodeKind::Mutable,
            &["pmwzqkvt"],
            &["conflict-inbox"],
        ),
        node(
            "pmwzqkvt",
            "77be0c13",
            "wip: conflict inbox list skeleton",
            "lauf",
            "2026-06-08T15:02:00Z",
            NodeKind::Mutable,
            &["uvkmrtpz"],
            &[],
        ),
        // One change rewritten two ways (jj's `??` state): the same fix
        // described from two terminals, both copies still visible. Nodes
        // key by commit id; the change id is shared.
        divergent_node(
            "rzvqnkom",
            "b41c77d0",
            "fix: debounce watcher restarts",
            "2026-06-09T18:26:00Z",
            &["uvkmrtpz"],
        ),
        divergent_node(
            "rzvqnkom",
            "e93d5a12",
            "fix: debounce watcher restarts (simpler timer)",
            "2026-06-09T18:31:00Z",
            &["uvkmrtpz"],
        ),
        node(
            "uvkmrtpz",
            "e7c41a9f",
            "release: cut 0.41 changelog",
            "maintainers",
            "2026-06-09T16:40:00Z",
            NodeKind::Immutable,
            &[],
            &["main"],
        ),
    ];

    let bookmarks = vec![
        BookmarkState {
            name: "main".into(),
            target: "uvkmrtpz".into(),
            remote: Some("origin".into()),
            sync: SyncState::Synced,
            is_trunk: true,
            is_local: true,
        },
        BookmarkState {
            name: "diff-virtualization".into(),
            target: "lwnouzpy".into(),
            remote: None,
            sync: SyncState::LocalOnly,
            is_trunk: false,
            is_local: true,
        },
        BookmarkState {
            name: "conflict-inbox".into(),
            target: "qvlxnsry".into(),
            remote: Some("origin".into()),
            sync: SyncState::Ahead,
            is_trunk: false,
            is_local: true,
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
        trunk_bookmark: "main".into(),
        working_copy: "ktpqsmxw".into(),
        workspaces: vec![WorkspaceSummary {
            name: "default".into(),
            is_default: true,
            is_stale: false,
            working_copy_node: Some("ktpqsmxw".into()),
        }],
        workstreams,
        nodes,
        bookmarks,
        conflicts: vec![],
        operations,
    }
}

/// Changed-file lists matching the mock graph, keyed by change id. `None`
/// for ids the mock snapshot never produced.
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
            ("static/textures/header-grain.png", Added),
        ],
        "qvlxnsry" => &[
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
                has_conflict: false,
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
    }
}

fn intraline(kind: DiffLineKind, parts: &[(&str, bool)]) -> DiffLine {
    DiffLine {
        kind,
        segments: parts.iter().map(|(text, changed)| seg(text, *changed)).collect(),
    }
}

fn hunk(old_start: u32, new_start: u32, lines: Vec<DiffLine>) -> DiffHunk {
    DiffHunk {
        old_start,
        new_start,
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
                "static/textures/header-grain.png",
                FileStatus::Added,
                FileDiffContent::Binary,
            ),
        ],
        "qvlxnsry" => vec![
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
            file(
                "src/lib/components/conflicts/ConflictList.svelte",
                FileStatus::Added,
                added_file(&[
                    "<script lang=\"ts\">",
                    "  import ConflictRow from \"./ConflictRow.svelte\";",
                    "  let { items } = $props();",
                    "</script>",
                    "",
                    "{#each items as item (item.id)}",
                    "  <ConflictRow {item} />",
                    "{/each}",
                ]),
            ),
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
pub fn mock_compare_diff(
    snapshot: &RepoSnapshot,
    from_id: &str,
    to_id: &str,
) -> Result<ChangeDiff, BackendError> {
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
