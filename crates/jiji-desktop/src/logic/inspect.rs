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

    // ── Ported from the Tauri app's inspect.test.ts and selection.test.ts ──

    fn node(id: &str, kind: NodeKind, parents: &[&str]) -> GraphNode {
        GraphNode {
            id: id.into(),
            change_id: id.into(),
            commit_id: format!("c-{id}"),
            description: format!("change {id}"),
            author: "test".into(),
            timestamp: "2026-06-10T12:00:00Z".into(),
            kind,
            parents: parents.iter().map(|p| (*p).to_owned()).collect(),
            elided_parents: vec![],
            bookmarks: vec![],
            tags: vec![],
            is_empty: false,
            has_conflict: false,
            is_divergent: false,
            held: false,
            old_copies_held: false,
        }
    }

    fn stream(id: &str, nodes: &[&str]) -> WorkstreamSummary {
        WorkstreamSummary {
            id: id.into(),
            title: id.into(),
            node_ids: nodes.iter().map(|n| (*n).to_owned()).collect(),
            bookmark: None,
            is_active: false,
            behind_trunk: 0,
        }
    }

    fn bookmark(name: &str, target: &str, trunk: bool) -> BookmarkState {
        BookmarkState {
            name: name.into(),
            target: target.into(),
            remote: trunk.then(|| "origin".into()),
            sync: if trunk {
                SyncState::Synced
            } else {
                SyncState::LocalOnly
            },
            is_trunk: trunk,
            is_local: true,
            tracked: trunk,
            is_merged: false,
            target_description: String::new(),
            target_author: String::new(),
            target_timestamp: String::new(),
        }
    }

    fn graph(
        nodes: Vec<GraphNode>,
        streams: Vec<WorkstreamSummary>,
        bookmarks: Vec<BookmarkState>,
    ) -> RepoSnapshot {
        RepoSnapshot {
            nodes,
            workstreams: streams,
            bookmarks,
            ..snapshot()
        }
    }

    fn ids(nodes: Vec<&GraphNode>) -> Vec<&str> {
        nodes.into_iter().map(|node| node.id.as_str()).collect()
    }

    fn owned(ids: &[&str]) -> Vec<String> {
        ids.iter().map(|id| (*id).to_owned()).collect()
    }

    #[test]
    fn children_are_every_node_naming_the_change_as_a_parent() {
        let snapshot = graph(
            vec![
                node("a", NodeKind::WorkingCopy, &["b"]),
                node("merge", NodeKind::Mutable, &["b", "side"]),
                node("b", NodeKind::Mutable, &["base"]),
                node("side", NodeKind::Mutable, &["base"]),
                node("base", NodeKind::Immutable, &[]),
            ],
            vec![],
            vec![],
        );
        assert_eq!(ids(children_of(&snapshot, "b")), ["a", "merge"]);
        assert!(children_of(&snapshot, "a").is_empty());
    }

    #[test]
    fn descendants_walk_nearest_first_across_forks_and_count_once() {
        let snapshot = graph(
            vec![
                node("top", NodeKind::WorkingCopy, &["mid"]),
                node("merge", NodeKind::Mutable, &["mid", "side"]),
                node("mid", NodeKind::Mutable, &["base"]),
                node("side", NodeKind::Mutable, &["base"]),
                node("base", NodeKind::Immutable, &[]),
            ],
            vec![],
            vec![],
        );
        assert_eq!(
            ids(descendants_of(&snapshot, "base")),
            ["mid", "side", "top", "merge"]
        );
        // Reachable two ways, reported once.
        assert_eq!(ids(descendants_of(&snapshot, "mid")), ["top", "merge"]);
        assert!(descendants_of(&snapshot, "top").is_empty());
    }

    #[test]
    fn a_squash_s_description_is_either_side_or_both_destination_first() {
        assert_eq!(combined_description("", "child text"), "child text");
        assert_eq!(combined_description("parent text", ""), "parent text");
        assert_eq!(
            combined_description("parent text\n", "child text"),
            "parent text\n\nchild text"
        );
    }

    #[test]
    fn a_change_s_place_is_its_stream_and_index_or_nothing() {
        let snapshot = graph(
            vec![
                node("top", NodeKind::WorkingCopy, &["low"]),
                node("low", NodeKind::Mutable, &[]),
                node("base", NodeKind::Immutable, &[]),
            ],
            vec![stream("ws-a", &["top", "low"]), stream("ws-b", &["other"])],
            vec![],
        );
        let place = stack_position(&snapshot, "low").unwrap();
        assert_eq!((place.workstream.id.as_str(), place.index), ("ws-a", 1));
        assert!(stack_position(&snapshot, "base").is_none());
    }

    #[test]
    fn a_change_s_bookmarks_list_trunk_first() {
        let snapshot = graph(
            vec![node("n1", NodeKind::Immutable, &[])],
            vec![],
            vec![
                bookmark("feature", "n1", false),
                bookmark("main", "n1", true),
                bookmark("other", "n2", false),
            ],
        );
        let names: Vec<&str> = bookmarks_at(&snapshot, "n1")
            .iter()
            .map(|b| b.name.as_str())
            .collect();
        assert_eq!(names, ["main", "feature"]);
    }

    fn stack_with_elided_history() -> RepoSnapshot {
        let mut mid = node("mid", NodeKind::Mutable, &[]);
        mid.elided_parents = vec!["base".into()];
        graph(
            vec![
                node("top", NodeKind::WorkingCopy, &["mid"]),
                mid,
                node("side", NodeKind::Mutable, &["base"]),
                node("base", NodeKind::Immutable, &[]),
            ],
            vec![],
            vec![],
        )
    }

    #[test]
    fn ancestry_follows_parents_and_elided_links() {
        let snapshot = stack_with_elided_history();
        assert!(is_ancestor(&snapshot, "base", "top"));
        assert!(is_ancestor(&snapshot, "mid", "top"));
        assert!(!is_ancestor(&snapshot, "top", "base"));
        assert!(!is_ancestor(&snapshot, "side", "top"));
    }

    #[test]
    fn a_bookmark_move_is_named_as_the_backend_names_it() {
        let snapshot = stack_with_elided_history();
        let way = |from, to| move_direction(&snapshot, from, to);
        assert_eq!(way("base", "top"), Some(MoveDirection::Forward));
        assert_eq!(way("top", "base"), Some(MoveDirection::Backwards));
        assert_eq!(way("side", "top"), Some(MoveDirection::Sideways));
        // An end outside the snapshot has no direction to state.
        assert_eq!(way("gone", "top"), None);
    }

    fn stack_and_sibling() -> RepoSnapshot {
        graph(
            vec![
                node("top", NodeKind::WorkingCopy, &["mid"]),
                node("mid", NodeKind::Mutable, &["base"]),
                node("side", NodeKind::Mutable, &["base"]),
                node("base", NodeKind::Immutable, &[]),
            ],
            vec![],
            vec![],
        )
    }

    #[test]
    fn rebase_destinations_leave_out_the_change_its_stack_and_its_parent() {
        let snapshot = stack_and_sibling();
        let to = |id, descendants| ids(rebase_destinations(&snapshot, id, descendants));
        // Taking its descendants along: not itself, not them, not the
        // parent it already sits on.
        assert_eq!(to("mid", true), ["side"]);
        // Alone: its descendants stay destinations (how neighbours swap),
        // and the parent too, since there is a descendant to extract.
        assert_eq!(to("mid", false), ["top", "side", "base"]);
        // With nothing to extract the sole parent is no move at all.
        assert_eq!(to("top", false), ["side", "base"]);
        assert_eq!(to("top", true), ["side", "base"]);
        assert!(to("gone", true).is_empty());
    }

    #[test]
    fn squash_destinations_are_every_other_mutable_change() {
        let snapshot = stack_and_sibling();
        assert_eq!(ids(squash_destinations(&snapshot, "top")), ["mid", "side"]);
        // Unlike a rebase the parent stays: moving files there is how work
        // amends down the stack.
        assert_eq!(ids(squash_destinations(&snapshot, "mid")), ["top", "side"]);
    }

    fn stacks_on_trunk_and_on_old_history() -> RepoSnapshot {
        let mut side = node("side", NodeKind::Mutable, &[]);
        side.elided_parents = vec!["old".into()];
        graph(
            vec![
                node("top", NodeKind::WorkingCopy, &["mid"]),
                node("mid", NodeKind::Mutable, &["trunk"]),
                side,
                node("trunk", NodeKind::Immutable, &[]),
                node("old", NodeKind::Immutable, &[]),
            ],
            vec![stream("ws-a", &["top", "mid"]), stream("ws-b", &["side"])],
            vec![bookmark("main", "trunk", true)],
        )
    }

    #[test]
    fn a_stack_s_base_is_the_parent_under_its_bottom_change() {
        let snapshot = stacks_on_trunk_and_on_old_history();
        assert_eq!(stack_base_of(&snapshot, "top").as_deref(), Some("trunk"));
        assert_eq!(stack_base_of(&snapshot, "mid").as_deref(), Some("trunk"));
        assert_eq!(stack_base_of(&snapshot, "side").as_deref(), Some("old"));
        // Immutable bases belong to no stack.
        assert_eq!(stack_base_of(&snapshot, "trunk"), None);
    }

    #[test]
    fn each_comparison_resolves_for_a_selection_or_not_at_all() {
        let snapshot = stacks_on_trunk_and_on_old_history();
        let from = |id, mode: CompareMode| resolve_compare_from(&snapshot, id, &mode);
        assert_eq!(from("top", CompareMode::Parent), None);
        assert_eq!(from("top", CompareMode::Trunk).as_deref(), Some("trunk"));
        assert_eq!(from("top", CompareMode::Base).as_deref(), Some("trunk"));
        assert_eq!(from("side", CompareMode::Base).as_deref(), Some("old"));
        assert_eq!(
            from("top", CompareMode::Change("side".into())).as_deref(),
            Some("side")
        );
        // Trunk against itself, a node with no stack, a picked change that
        // left the snapshot or is the selection.
        assert_eq!(from("trunk", CompareMode::Trunk), None);
        assert_eq!(from("trunk", CompareMode::Base), None);
        assert_eq!(from("top", CompareMode::Change("gone".into())), None);
        assert_eq!(from("top", CompareMode::Change("top".into())), None);
        let trunkless = graph(
            vec![node("top", NodeKind::WorkingCopy, &[])],
            vec![],
            vec![],
        );
        assert_eq!(
            resolve_compare_from(&trunkless, "top", &CompareMode::Trunk),
            None
        );
    }

    #[test]
    fn a_divergent_change_s_siblings_are_its_other_copies() {
        let copy = |id: &str| GraphNode {
            change_id: "rzvqnkom".into(),
            is_divergent: true,
            held: false,
            old_copies_held: false,
            ..node(id, NodeKind::Mutable, &["trunk"])
        };
        let snapshot = graph(
            vec![
                copy("b41c77d0"),
                copy("e93d5a12"),
                node("side", NodeKind::Mutable, &["trunk"]),
                node("trunk", NodeKind::Immutable, &[]),
            ],
            vec![],
            vec![],
        );
        let of = |ix: usize| ids(divergent_siblings(&snapshot, &snapshot.nodes[ix]));
        assert_eq!(of(0), ["e93d5a12"]);
        assert_eq!(of(1), ["b41c77d0"]);
        assert!(of(2).is_empty());
    }

    fn mixed_stack() -> RepoSnapshot {
        graph(
            vec![
                node("wc", NodeKind::WorkingCopy, &["mid"]),
                node("mid", NodeKind::Mutable, &["bottom"]),
                node("bottom", NodeKind::Mutable, &["base"]),
                node("merge", NodeKind::Mutable, &["mid", "bottom"]),
                node("base", NodeKind::Immutable, &[]),
            ],
            vec![],
            vec![],
        )
    }

    fn actions_at(snapshot: &RepoSnapshot, id: &str) -> ChangeActions {
        action_availability(snapshot, find_node(snapshot, id).unwrap())
    }

    #[test]
    fn a_mutable_change_on_a_mutable_parent_offers_every_action() {
        let snapshot = mixed_stack();
        assert_eq!(
            actions_at(&snapshot, "mid"),
            ChangeActions {
                describe: true,
                new_child: true,
                edit: true,
                bookmark: true,
                rebase: true,
                split: true,
                squash: true,
                abandon: true,
            }
        );
        assert_eq!(
            actions_at(&snapshot, "base"),
            ChangeActions {
                describe: false,
                new_child: true,
                edit: false,
                bookmark: true,
                rebase: false,
                split: false,
                squash: false,
                abandon: false,
            }
        );
    }

    #[test]
    fn split_squash_and_edit_are_withheld_where_they_mean_nothing() {
        let mut empty = node("wc", NodeKind::WorkingCopy, &["base"]);
        empty.is_empty = true;
        let snapshot = graph(
            vec![empty, node("base", NodeKind::Immutable, &[])],
            vec![],
            vec![],
        );
        assert!(!actions_at(&snapshot, "wc").split);
        let snapshot = mixed_stack();
        // An immutable parent, or two parents: nothing single to squash into.
        assert!(!actions_at(&snapshot, "bottom").squash);
        assert!(!actions_at(&snapshot, "merge").squash);
        // The working copy is edited already, and still described.
        let wc = actions_at(&snapshot, "wc");
        assert!(!wc.edit && wc.describe);
    }

    /// wc → b → a on trunk, and a one-change sibling on trunk.
    fn three_and_one() -> RepoSnapshot {
        let mut active = stream("ws-a", &["wc", "b", "a"]);
        active.is_active = true;
        graph(
            vec![
                node("wc", NodeKind::WorkingCopy, &["b"]),
                node("b", NodeKind::Mutable, &["a"]),
                node("a", NodeKind::Mutable, &["trunk"]),
                node("s1", NodeKind::Mutable, &["trunk"]),
                node("trunk", NodeKind::Immutable, &[]),
            ],
            vec![active, stream("ws-s", &["s1"])],
            vec![],
        )
    }

    #[test]
    fn a_range_runs_head_first_whichever_end_was_picked_first() {
        let snapshot = three_and_one();
        let range = |anchor, target| extend_range(&snapshot, anchor, target);
        assert_eq!(range("wc", "a"), Some(owned(&["wc", "b", "a"])));
        assert_eq!(range("a", "wc"), Some(owned(&["wc", "b", "a"])));
        // Middle to end leaves the rest of the lane alone; one end is one id.
        assert_eq!(range("b", "a"), Some(owned(&["b", "a"])));
        assert_eq!(range("b", "b"), Some(owned(&["b"])));
    }

    #[test]
    fn a_range_stays_in_one_stream_and_needs_a_base_to_diff_from() {
        let snapshot = three_and_one();
        assert_eq!(extend_range(&snapshot, "wc", "s1"), None);
        assert_eq!(extend_range(&snapshot, "wc", "trunk"), None);
        assert_eq!(extend_range(&snapshot, "trunk", "wc"), None);
        let rootless = graph(
            vec![
                node("wc", NodeKind::WorkingCopy, &["a"]),
                node("a", NodeKind::Mutable, &[]),
            ],
            vec![stream("ws", &["wc", "a"])],
            vec![],
        );
        assert_eq!(extend_range(&rootless, "wc", "a"), None);
    }

    #[test]
    fn a_range_diffs_from_its_root_s_first_parent() {
        let snapshot = three_and_one();
        let from = |run: &[&str]| range_compare_from(&snapshot, &owned(run));
        assert_eq!(from(&["wc", "b", "a"]).as_deref(), Some("trunk"));
        assert_eq!(from(&["wc", "b"]).as_deref(), Some("a"));
        assert_eq!(from(&["ghost", "a"]), None);
        assert_eq!(from(&["wc", "ghost"]), None);
        assert_eq!(from(&[]), None);
        let merge = graph(
            vec![
                node("wc", NodeKind::WorkingCopy, &["m"]),
                node("m", NodeKind::Mutable, &["a", "other"]),
                node("a", NodeKind::Immutable, &[]),
                node("other", NodeKind::Immutable, &[]),
            ],
            vec![stream("ws", &["wc", "m"])],
            vec![],
        );
        assert_eq!(
            range_compare_from(&merge, &owned(&["wc", "m"])).as_deref(),
            Some("a")
        );
    }

    #[test]
    fn a_kept_range_holds_only_while_its_run_is_unchanged() {
        let snapshot = three_and_one();
        let holds = |run: &[&str]| valid_range(&snapshot, &owned(run));
        assert!(holds(&["wc", "b", "a"]) && holds(&["b", "a"]));
        assert!(!holds(&["wc"]) && !holds(&[]));
        // The head left every stream.
        assert!(!holds(&["trunk", "a"]));
        // A member was abandoned mid-run.
        let abandoned = graph(
            vec![
                node("wc", NodeKind::WorkingCopy, &["a"]),
                node("a", NodeKind::Mutable, &["trunk"]),
                node("trunk", NodeKind::Immutable, &[]),
            ],
            vec![stream("ws-a", &["wc", "a"])],
            vec![],
        );
        assert!(!valid_range(&abandoned, &owned(&["wc", "b", "a"])));
        // The lane was reordered underneath it.
        let reordered = graph(
            vec![
                node("wc", NodeKind::WorkingCopy, &["a"]),
                node("a", NodeKind::Mutable, &["b"]),
                node("b", NodeKind::Mutable, &["trunk"]),
                node("trunk", NodeKind::Immutable, &[]),
            ],
            vec![stream("ws-a", &["wc", "a", "b"])],
            vec![],
        );
        assert!(!valid_range(&reordered, &owned(&["wc", "b", "a"])));
    }
}
