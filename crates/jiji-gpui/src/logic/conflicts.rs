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
}
