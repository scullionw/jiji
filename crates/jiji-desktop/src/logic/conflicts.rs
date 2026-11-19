//! How conflicts group and which recoveries apply, shared by the conflict
//! inbox and the diff surface's Resolve affordances.

use std::collections::HashMap;

use jiji_core::snapshot::{ConflictItem, ConflictKind, NodeKind, RepoSnapshot};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GroupKey {
    WorkingCopy,
    Commits,
    Bookmarks,
    Workspaces,
}

impl GroupKey {
    pub fn title(self) -> &'static str {
        match self {
            GroupKey::WorkingCopy => "In your working copy",
            GroupKey::Commits => "Recorded in commits",
            GroupKey::Bookmarks => "Conflicted bookmarks",
            GroupKey::Workspaces => "Stale workspaces",
        }
    }

    pub fn blurb(self) -> &'static str {
        match self {
            GroupKey::WorkingCopy => {
                "These conflicts are written into the files on disk right now as marker \
                 lines. Edit the files to resolve them — or keep working elsewhere; the \
                 conflict stays safely recorded either way."
            }
            GroupKey::Commits => {
                "A rewrite didn't apply cleanly, so jj recorded the conflict inside the \
                 commit instead of stopping halfway. Nothing is blocked — open a change to \
                 see the conflicted content, and edit it to resolve."
            }
            GroupKey::Bookmarks => {
                "The bookmark was moved to different targets by operations that don't \
                 agree — usually concurrent commands or a fetch racing local work. Point \
                 it at the right change to resolve."
            }
            GroupKey::Workspaces => {
                "The repository moved since this workspace's working copy was last \
                 updated. Update the workspace before working in it."
            }
        }
    }
}

pub struct ConflictGroup<'a> {
    pub key: GroupKey,
    pub items: Vec<&'a ConflictItem>,
}

fn group_of(item: &ConflictItem, working_copy: &str) -> GroupKey {
    match item.kind {
        ConflictKind::File if item.node_id.as_deref() == Some(working_copy) => {
            GroupKey::WorkingCopy
        }
        ConflictKind::File => GroupKey::Commits,
        ConflictKind::Bookmark => GroupKey::Bookmarks,
        ConflictKind::StaleWorkspace => GroupKey::Workspaces,
    }
}

/// The inbox's groups, most urgent first; commit conflicts follow the
/// graph's row order so the list reads like the tree.
pub fn group_conflicts(snapshot: &RepoSnapshot) -> Vec<ConflictGroup<'_>> {
    let order = [
        GroupKey::WorkingCopy,
        GroupKey::Commits,
        GroupKey::Bookmarks,
        GroupKey::Workspaces,
    ];
    let mut buckets: HashMap<GroupKey, Vec<&ConflictItem>> = HashMap::new();
    for item in &snapshot.conflicts {
        buckets
            .entry(group_of(item, &snapshot.working_copy))
            .or_default()
            .push(item);
    }
    if let Some(commits) = buckets.get_mut(&GroupKey::Commits) {
        let row: HashMap<&str, usize> = snapshot
            .nodes
            .iter()
            .enumerate()
            .map(|(ix, node)| (node.id.as_str(), ix))
            .collect();
        commits.sort_by_key(|item| {
            item.node_id
                .as_deref()
                .and_then(|id| row.get(id).copied())
                .unwrap_or(usize::MAX)
        });
    }
    order
        .into_iter()
        .filter_map(|key| {
            buckets
                .remove(&key)
                .map(|items| ConflictGroup { key, items })
        })
        .collect()
}

/// The merge tool's display name for its configured name.
pub fn merge_tool_label(tool: &str) -> &str {
    match tool {
        "smerge" => "Sublime Merge",
        "meld" => "Meld",
        "kdiff3" => "KDiff3",
        "mergiraf" => "Mergiraf",
        "vimdiff" => "Vim",
        "vscode" => "VS Code",
        "vscodium" => "VSCodium",
        other => other,
    }
}

/// The guided stale-workspace recovery applies only to the workspace the
/// app has open.
pub fn can_recover_workspace(snapshot: &RepoSnapshot, item: &ConflictItem) -> bool {
    if item.kind != ConflictKind::StaleWorkspace {
        return false;
    }
    let Some(workspace) = &item.workspace else {
        return false;
    };
    snapshot
        .workspaces
        .iter()
        .any(|w| w.is_current && &w.name == workspace)
}

/// Taking a side rewrites the change, so it needs a drawn, mutable one.
pub fn can_take_side(snapshot: &RepoSnapshot, node_id: Option<&str>) -> bool {
    node_id
        .and_then(|id| snapshot.nodes.iter().find(|n| n.id == id))
        .is_some_and(|node| node.kind != NodeKind::Immutable)
}

pub fn can_resolve(snapshot: &RepoSnapshot, node_id: Option<&str>) -> bool {
    snapshot.resolve_tool.is_some() && can_take_side(snapshot, node_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiji_core::{MockBackend, RepoBackend};

    #[test]
    fn the_mock_inbox_groups_in_urgency_order() {
        let dir = std::env::temp_dir().join(format!("jiji-conflicts-{}", std::process::id()));
        let _ = std::fs::create_dir_all(dir.join(".jj"));
        let snapshot = MockBackend::default().open(&dir).unwrap();
        let groups = group_conflicts(&snapshot);
        let total: usize = groups.iter().map(|group| group.items.len()).sum();
        assert_eq!(total, snapshot.conflicts.len());
        let keys: Vec<GroupKey> = groups.iter().map(|group| group.key).collect();
        let mut sorted = keys.clone();
        sorted.sort_by_key(|key| match key {
            GroupKey::WorkingCopy => 0,
            GroupKey::Commits => 1,
            GroupKey::Bookmarks => 2,
            GroupKey::Workspaces => 3,
        });
        assert_eq!(keys, sorted);
        assert_eq!(merge_tool_label("smerge"), "Sublime Merge");
        assert_eq!(merge_tool_label("custom-tool"), "custom-tool");
        assert!(!can_take_side(&snapshot, None));
    }

    // ── Ported from the Tauri app's conflicts.test.ts ──

    use jiji_core::snapshot::{GraphNode, WorkspaceSummary};

    fn base() -> RepoSnapshot {
        let dir = std::env::temp_dir().join(format!("jiji-conflicts-{}", std::process::id()));
        let _ = std::fs::create_dir_all(dir.join(".jj"));
        MockBackend::default().open(&dir).unwrap()
    }

    fn node(id: &str, kind: NodeKind) -> GraphNode {
        GraphNode {
            id: id.into(),
            change_id: id.into(),
            commit_id: format!("c-{id}"),
            description: format!("change {id}"),
            author: "test".into(),
            timestamp: "2026-06-10T12:00:00Z".into(),
            kind,
            parents: vec![],
            elided_parents: vec![],
            bookmarks: vec![],
            tags: vec![],
            is_empty: false,
            has_conflict: false,
            is_divergent: false,
        }
    }

    fn item(id: &str, kind: ConflictKind, node_id: Option<&str>) -> ConflictItem {
        ConflictItem {
            id: id.into(),
            kind,
            summary: format!("conflict {id}"),
            node_id: node_id.map(str::to_owned),
            paths: vec![],
            more_paths: 0,
            targets: vec![],
            workspace: None,
        }
    }

    fn inbox(conflicts: Vec<ConflictItem>, nodes: Vec<GraphNode>) -> RepoSnapshot {
        RepoSnapshot {
            conflicts,
            nodes,
            working_copy: "wc".into(),
            resolve_tool: Some("smerge".into()),
            ..base()
        }
    }

    fn grouped(snapshot: &RepoSnapshot) -> Vec<(GroupKey, Vec<&str>)> {
        group_conflicts(snapshot)
            .into_iter()
            .map(|group| {
                (
                    group.key,
                    group.items.iter().map(|i| i.id.as_str()).collect(),
                )
            })
            .collect()
    }

    #[test]
    fn the_working_copy_s_files_group_apart_from_commits() {
        let snapshot = inbox(
            vec![
                item("file-a", ConflictKind::File, Some("a")),
                item("file-wc", ConflictKind::File, Some("wc")),
            ],
            vec![
                node("wc", NodeKind::WorkingCopy),
                node("a", NodeKind::Mutable),
            ],
        );
        assert_eq!(
            grouped(&snapshot),
            [
                (GroupKey::WorkingCopy, vec!["file-wc"]),
                (GroupKey::Commits, vec!["file-a"])
            ]
        );
    }

    #[test]
    fn groups_keep_their_order_and_empty_ones_are_left_out() {
        let snapshot = inbox(
            vec![
                item("workspace-review", ConflictKind::StaleWorkspace, None),
                item("bookmark-x", ConflictKind::Bookmark, None),
            ],
            vec![],
        );
        let keys: Vec<GroupKey> = grouped(&snapshot).into_iter().map(|(key, _)| key).collect();
        assert_eq!(keys, [GroupKey::Bookmarks, GroupKey::Workspaces]);
        assert!(group_conflicts(&inbox(vec![], vec![])).is_empty());
    }

    #[test]
    fn commit_conflicts_read_in_graph_order_with_undrawn_changes_last() {
        let snapshot = inbox(
            vec![
                item("file-hidden", ConflictKind::File, Some("gone")),
                item("file-low", ConflictKind::File, Some("low")),
                item("file-high", ConflictKind::File, Some("high")),
            ],
            vec![
                node("high", NodeKind::Mutable),
                node("low", NodeKind::Mutable),
            ],
        );
        assert_eq!(
            grouped(&snapshot),
            [(
                GroupKey::Commits,
                vec!["file-high", "file-low", "file-hidden"]
            )]
        );
    }

    #[test]
    fn known_merge_tools_go_by_their_names() {
        assert_eq!(merge_tool_label("smerge"), "Sublime Merge");
        assert_eq!(merge_tool_label("vscode"), "VS Code");
        assert_eq!(merge_tool_label("meld"), "Meld");
        assert_eq!(merge_tool_label("my-house-tool"), "my-house-tool");
    }

    #[test]
    fn only_the_open_workspace_s_staleness_can_be_recovered_here() {
        let workspace = |name: &str, current: bool| WorkspaceSummary {
            name: name.into(),
            is_default: current,
            is_current: current,
            is_stale: true,
            working_copy_node: None,
        };
        let stale = |name: Option<&str>| ConflictItem {
            workspace: name.map(str::to_owned),
            ..item("workspace", ConflictKind::StaleWorkspace, None)
        };
        let mut snapshot = inbox(vec![], vec![]);
        snapshot.workspaces = vec![workspace("default", true), workspace("review", false)];
        assert!(can_recover_workspace(&snapshot, &stale(Some("default"))));
        assert!(!can_recover_workspace(&snapshot, &stale(Some("review"))));
        assert!(!can_recover_workspace(
            &snapshot,
            &item("file-a", ConflictKind::File, None)
        ));
        assert!(!can_recover_workspace(&snapshot, &stale(None)));
        snapshot.workspaces.clear();
        assert!(!can_recover_workspace(&snapshot, &stale(Some("default"))));
    }

    #[test]
    fn resolving_needs_a_tool_and_taking_a_side_a_drawn_mutable_change() {
        let nodes = vec![
            node("wc", NodeKind::WorkingCopy),
            node("mut", NodeKind::Mutable),
            node("trunk", NodeKind::Immutable),
        ];
        let tooled = inbox(vec![], nodes.clone());
        let untooled = RepoSnapshot {
            resolve_tool: None,
            ..inbox(vec![], nodes)
        };
        for (id, ok) in [
            (Some("wc"), true),
            (Some("mut"), true),
            (Some("trunk"), false),
            (Some("gone"), false),
            (None, false),
        ] {
            assert_eq!(can_resolve(&tooled, id), ok, "{id:?}");
            assert_eq!(can_take_side(&untooled, id), ok, "{id:?}");
        }
        assert!(!can_resolve(&untooled, Some("mut")));
    }
}
