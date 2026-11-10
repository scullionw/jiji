//! Pure lookups that place one node in context: its children, the
//! workstream that owns it, what actions apply, and the contiguous
//! multi-selection (⇧-click along a lane). No UI types.

use std::collections::HashSet;

use jiji_core::snapshot::{
    BookmarkState, GraphNode, NodeKind, RepoSnapshot, SyncState, WorkstreamSummary,
};

pub fn find_node<'a>(snapshot: &'a RepoSnapshot, id: &str) -> Option<&'a GraphNode> {
    snapshot.nodes.iter().find(|node| node.id == id)
}

/// Children are derived rather than stored: the DTO only carries parent
/// links.
pub fn children_of<'a>(snapshot: &'a RepoSnapshot, id: &str) -> Vec<&'a GraphNode> {
    snapshot
        .nodes
        .iter()
        .filter(|node| node.parents.iter().any(|parent| parent == id))
        .collect()
}

/// Every transitive descendant within the snapshot, nearest first. Powers
/// the "N descendants will be rebased" line of mutation confirm panels.
pub fn descendants_of<'a>(snapshot: &'a RepoSnapshot, id: &str) -> Vec<&'a GraphNode> {
    let mut out = Vec::new();
    let mut seen: HashSet<&str> = HashSet::from([id]);
    let mut queue = std::collections::VecDeque::from([id]);
    while let Some(next) = queue.pop_front() {
        for child in children_of(snapshot, next) {
            if seen.insert(child.id.as_str()) {
                out.push(child);
                queue.push_back(child.id.as_str());
            }
        }
    }
    out
}

/// Preview of the description a squash produces, mirroring the backend's
/// combining: an empty side yields the other, two real descriptions
/// concatenate destination (parent) first.
pub fn combined_description(destination: &str, source: &str) -> String {
    let (dest, src) = (destination.trim(), source.trim());
    if dest.is_empty() {
        src.to_owned()
    } else if src.is_empty() {
        dest.to_owned()
    } else {
        format!("{dest}\n\n{src}")
    }
}

pub struct StackPosition<'a> {
    pub workstream: &'a WorkstreamSummary,
    /// 0-based position from the top of the stack.
    pub index: usize,
}

/// The workstream that claims this node, if any. Immutable bases belong to
/// no workstream.
pub fn stack_position<'a>(snapshot: &'a RepoSnapshot, id: &str) -> Option<StackPosition<'a>> {
    snapshot.workstreams.iter().find_map(|workstream| {
        workstream
            .node_ids
            .iter()
            .position(|node| node == id)
            .map(|index| StackPosition { workstream, index })
    })
}

/// Ancestry within the drawn snapshot, following parent and elided-parent
/// links up from the descendant.
pub fn is_ancestor(snapshot: &RepoSnapshot, ancestor_id: &str, descendant_id: &str) -> bool {
    let mut queue = vec![descendant_id];
    let mut seen = HashSet::new();
    while let Some(id) = queue.pop() {
        if id == ancestor_id {
            return true;
        }
        if !seen.insert(id) {
            continue;
        }
        if let Some(node) = find_node(snapshot, id) {
            queue.extend(node.parents.iter().map(String::as_str));
            queue.extend(node.elided_parents.iter().map(String::as_str));
        }
    }
    false
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MoveDirection {
    Forward,
    Backwards,
    Sideways,
}

/// The direction a bookmark move would take, mirroring how the backend
/// summarizes it. `None` when either end is outside the drawn snapshot.
pub fn move_direction(
    snapshot: &RepoSnapshot,
    from_id: &str,
    to_id: &str,
) -> Option<MoveDirection> {
    find_node(snapshot, from_id)?;
    find_node(snapshot, to_id)?;
    Some(if is_ancestor(snapshot, from_id, to_id) {
        MoveDirection::Forward
    } else if is_ancestor(snapshot, to_id, from_id) {
        MoveDirection::Backwards
    } else {
        MoveDirection::Sideways
    })
}

/// Which actions the workbench offers on one change — shared by the change
/// header's actions row, the context menu and the command palette so the
/// routes never disagree. The backend re-checks everything; this is the
/// affordance rule, not the enforcement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChangeActions {
    pub describe: bool,
    pub new_child: bool,
    pub edit: bool,
    pub bookmark: bool,
    pub rebase: bool,
    pub split: bool,
    pub squash: bool,
    pub abandon: bool,
}

pub fn action_availability(snapshot: &RepoSnapshot, node: &GraphNode) -> ChangeActions {
    let immutable = node.kind == NodeKind::Immutable;
    let parent = match node.parents.as_slice() {
        [only] => find_node(snapshot, only),
        _ => None,
    };
    ChangeActions {
        describe: !immutable,
        new_child: true,
        edit: node.kind == NodeKind::Mutable,
        bookmark: true,
        rebase: !immutable,
        // An empty change has no files to split apart.
        split: !immutable && !node.is_empty,
        squash: !immutable && parent.is_some_and(|p| p.kind != NodeKind::Immutable),
        abandon: !immutable,
    }
}

/// Candidate destinations for rebasing one change, in the snapshot's graph
/// order. Excluded: the change itself; its descendants when they come
/// along (a cycle — but a lone move onto a descendant is how adjacent
/// changes swap order, so they stay); and the sole current parent when
/// landing there would change nothing.
pub fn rebase_destinations<'a>(
    snapshot: &'a RepoSnapshot,
    id: &str,
    with_descendants: bool,
) -> Vec<&'a GraphNode> {
    let Some(node) = find_node(snapshot, id) else {
        return Vec::new();
    };
    let mut excluded: HashSet<&str> = HashSet::from([id]);
    let descendants = descendants_of(snapshot, id);
    if with_descendants {
        excluded.extend(descendants.iter().map(|d| d.id.as_str()));
    }
    if node.parents.len() == 1 && (with_descendants || descendants.is_empty()) {
        excluded.insert(node.parents[0].as_str());
    }
    snapshot
        .nodes
        .iter()
        .filter(|n| !excluded.contains(n.id.as_str()))
        .collect()
}

/// Candidate destinations for moving files out of one change (`jj squash
/// --from --into`): any other mutable drawn change.
pub fn squash_destinations<'a>(snapshot: &'a RepoSnapshot, id: &str) -> Vec<&'a GraphNode> {
    snapshot
        .nodes
        .iter()
        .filter(|n| n.id != id && n.kind != NodeKind::Immutable)
        .collect()
}

/// What the diff surface measures a selection against. The presets are
/// relative, not pinned change ids, so walking the stack with "vs trunk"
/// keeps comparing each selection against trunk.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum CompareMode {
    #[default]
    Parent,
    Trunk,
    Base,
    Change(String),
}

/// The change id a node's workstream grew from: the first (possibly
/// elided) parent under the stack's bottom node.
pub fn stack_base_of(snapshot: &RepoSnapshot, id: &str) -> Option<String> {
    let workstream = stack_position(snapshot, id)?.workstream;
    let bottom = find_node(snapshot, workstream.node_ids.last()?)?;
    bottom
        .parents
        .first()
        .or(bottom.elided_parents.first())
        .cloned()
}

/// The from-change a compare mode resolves to for one selection, or `None`
/// when the comparison cannot apply — the surface then falls back to the
/// plain parent diff.
pub fn resolve_compare_from(
    snapshot: &RepoSnapshot,
    node_id: &str,
    mode: &CompareMode,
) -> Option<String> {
    let usable =
        |id: Option<String>| id.filter(|id| id != node_id && find_node(snapshot, id).is_some());
    match mode {
        CompareMode::Parent => None,
        CompareMode::Trunk => usable(
            snapshot
                .bookmarks
                .iter()
                .find(|b| b.is_trunk)
                .map(|b| b.target.clone()),
        ),
        CompareMode::Base => usable(stack_base_of(snapshot, node_id)),
        CompareMode::Change(id) => usable(Some(id.clone())),
    }
}

/// The other visible commit(s) of a divergent change, drawn-snapshot only.
pub fn divergent_siblings<'a>(snapshot: &'a RepoSnapshot, node: &GraphNode) -> Vec<&'a GraphNode> {
    if !node.is_divergent {
        return Vec::new();
    }
    snapshot
        .nodes
        .iter()
        .filter(|n| n.change_id == node.change_id && n.id != node.id)
        .collect()
}

/// Bookmark states pointing at this node, trunk first.
pub fn bookmarks_at<'a>(snapshot: &'a RepoSnapshot, id: &str) -> Vec<&'a BookmarkState> {
    let mut found: Vec<&BookmarkState> = snapshot
        .bookmarks
        .iter()
        .filter(|bookmark| bookmark.target == id)
        .collect();
    found.sort_by_key(|bookmark| !bookmark.is_trunk);
    found
}

/// Leading directories (with a trailing slash, or "" at the root) and the
/// file name.
pub fn split_path(path: &str) -> (&str, &str) {
    match path.rfind('/') {
        Some(cut) => path.split_at(cut + 1),
        None => ("", path),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Ok,
    Warn,
    Danger,
    Muted,
}

pub fn sync_label(sync: SyncState) -> (&'static str, Tone) {
    match sync {
        SyncState::Synced => ("synced", Tone::Ok),
        SyncState::Ahead => ("ahead", Tone::Warn),
        SyncState::Behind => ("behind", Tone::Warn),
        SyncState::Diverged => ("diverged", Tone::Danger),
        SyncState::LocalOnly => ("local", Tone::Muted),
        SyncState::RemoteOnly => ("remote", Tone::Muted),
    }
}

/// The first line of a description, or the placeholder for none.
pub fn summary_of(description: &str) -> &str {
    description.lines().next().unwrap_or("").trim()
}

// ── The graph's contiguous multi-selection ──

/// The run a ⇧-click extends to: anchor and target must sit in the same
/// workstream, and the range is the slice between them, inclusive
/// (head-first). `None` when the two share no lane or the run has no diff
/// base; the caller falls back to a plain select.
pub fn extend_range(
    snapshot: &RepoSnapshot,
    anchor_id: &str,
    target_id: &str,
) -> Option<Vec<String>> {
    let anchor = stack_position(snapshot, anchor_id)?;
    let target = stack_position(snapshot, target_id)?;
    if anchor.workstream.id != target.workstream.id {
        return None;
    }
    let lo = anchor.index.min(target.index);
    let hi = anchor.index.max(target.index);
    let ids = anchor.workstream.node_ids[lo..=hi].to_vec();
    range_compare_from(snapshot, &ids)?;
    Some(ids)
}

/// What a range's combined diff is measured from: the first parent of the
/// run's root (its oldest change).
pub fn range_compare_from(snapshot: &RepoSnapshot, ids: &[String]) -> Option<String> {
    find_node(snapshot, ids.first()?)?;
    find_node(snapshot, ids.last()?)?.parents.first().cloned()
}

/// Whether a stored range still holds against a (possibly refreshed)
/// snapshot: still one contiguous run of a workstream, unchanged, with a
/// diff base.
pub fn valid_range(snapshot: &RepoSnapshot, ids: &[String]) -> bool {
    if ids.len() < 2 {
        return false;
    }
    let Some(head) = stack_position(snapshot, &ids[0]) else {
        return false;
    };
    let current = head
        .workstream
        .node_ids
        .get(head.index..head.index + ids.len());
    current == Some(ids) && range_compare_from(snapshot, ids).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiji_core::{MockBackend, RepoBackend};

    fn snapshot() -> RepoSnapshot {
        let dir = std::env::temp_dir().join(format!("jiji-inspect-{}", std::process::id()));
        let _ = std::fs::create_dir_all(dir.join(".jj"));
        MockBackend::default().open(&dir).unwrap()
    }

    #[test]
    fn ranges_stay_inside_one_workstream() {
        let snapshot = snapshot();
        let stream = snapshot
            .workstreams
            .iter()
            .find(|w| w.node_ids.len() >= 2)
            .expect("the mock has a multi-change stack");
        let (head, root) = (&stream.node_ids[0], stream.node_ids.last().unwrap());
        let range = extend_range(&snapshot, head, root).unwrap();
        assert_eq!(range, stream.node_ids);
        assert!(valid_range(&snapshot, &range));
        // Anchoring from the other end yields the same head-first run.
        assert_eq!(extend_range(&snapshot, root, head).unwrap(), range);
        assert!(!valid_range(&snapshot, &range[..1]));
        if let Some(other) = snapshot.workstreams.iter().find(|w| w.id != stream.id) {
            assert!(extend_range(&snapshot, head, &other.node_ids[0]).is_none());
        }
    }

    #[test]
    fn immutable_changes_only_offer_non_rewriting_actions() {
        let snapshot = snapshot();
        let base = snapshot
            .nodes
            .iter()
            .find(|n| n.kind == NodeKind::Immutable)
            .unwrap();
        let actions = action_availability(&snapshot, base);
        assert!(actions.new_child && actions.bookmark);
        assert!(!actions.describe && !actions.rebase && !actions.abandon && !actions.squash);
    }

    #[test]
    fn rebase_destinations_exclude_self_and_cycles() {
        let snapshot = snapshot();
        let stream = snapshot
            .workstreams
            .iter()
            .find(|w| w.node_ids.len() >= 2)
            .unwrap();
        let root = stream.node_ids.last().unwrap();
        let with = rebase_destinations(&snapshot, root, true);
        assert!(with.iter().all(|n| &n.id != root));
        for descendant in descendants_of(&snapshot, root) {
            assert!(with.iter().all(|n| n.id != descendant.id));
        }
        // A lone move may land on a descendant: that is how neighbors swap.
        let alone = rebase_destinations(&snapshot, root, false);
        assert!(alone.len() > with.len());
    }

    #[test]
    fn paths_split_into_directory_and_name() {
        assert_eq!(split_path("src/lib/api.ts"), ("src/lib/", "api.ts"));
        assert_eq!(split_path("README.md"), ("", "README.md"));
        assert_eq!(combined_description("a\n", ""), "a");
        assert_eq!(combined_description("a", "b"), "a\n\nb");
    }
}
