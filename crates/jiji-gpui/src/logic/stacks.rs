//! Which workstreams can publish or ship, and from where.

use std::collections::{HashMap, HashSet};

use jiji_core::snapshot::{GraphNode, NodeKind, RepoSnapshot};

use super::inspect::summary_of;

/// What a stack submits from: its head bookmark, or — when it has none —
/// the head change a branch name will be derived for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StackHead {
    Bookmark(String),
    Change { id: String, title: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishableStack {
    pub workstream_id: String,
    pub title: String,
    pub is_active: bool,
    pub change_count: usize,
    pub head: StackHead,
}

impl PublishableStack {
    pub fn head_bookmark(&self) -> Option<&str> {
        match &self.head {
            StackHead::Bookmark(name) => Some(name),
            StackHead::Change { .. } => None,
        }
    }
}

fn head_title(node: &GraphNode) -> String {
    let title = summary_of(&node.description);
    if title.is_empty() {
        "no description".to_owned()
    } else {
        title.to_owned()
    }
}

/// A fresh, undescribed working copy on top is a scratch pad, not part of
/// what gets published: the stack starts under it.
fn skip_draft(snapshot_nodes: &HashMap<&str, &GraphNode>, top: Option<&String>) -> usize {
    match top.and_then(|id| snapshot_nodes.get(id.as_str())) {
        Some(node) if node.kind == NodeKind::WorkingCopy && node.description.trim().is_empty() => 1,
        _ => 0,
    }
}

/// Every workstream that can submit: from its topmost locally bookmarked
/// change (preferring a bookmark that already has a PR), or from its head
/// change when nothing is bookmarked yet. The active stack first.
pub fn publishable_stacks(
    snapshot: &RepoSnapshot,
    pr_branches: &HashSet<&str>,
) -> Vec<PublishableStack> {
    let local: HashSet<&str> = snapshot
        .bookmarks
        .iter()
        .filter(|b| b.is_local && !b.is_trunk)
        .map(|b| b.name.as_str())
        .collect();
    let nodes: HashMap<&str, &GraphNode> =
        snapshot.nodes.iter().map(|n| (n.id.as_str(), n)).collect();
    let mut stacks = Vec::new();
    for stream in &snapshot.workstreams {
        let head_index = stream.node_ids.iter().position(|id| {
            nodes.get(id.as_str()).is_some_and(|node| {
                node.bookmarks
                    .iter()
                    .any(|name| local.contains(name.as_str()))
            })
        });
        let Some(head_index) = head_index else {
            let start = skip_draft(&nodes, stream.node_ids.first());
            // Only a fresh draft: nothing to publish.
            let Some(head) = stream
                .node_ids
                .get(start)
                .and_then(|id| nodes.get(id.as_str()))
            else {
                continue;
            };
            stacks.push(PublishableStack {
                workstream_id: stream.id.clone(),
                title: stream.title.clone(),
                is_active: stream.is_active,
                change_count: stream.node_ids.len() - start,
                head: StackHead::Change {
                    id: head.id.clone(),
                    title: head_title(head),
                },
            });
            continue;
        };
        let mut names: Vec<&str> = nodes[stream.node_ids[head_index].as_str()]
            .bookmarks
            .iter()
            .map(String::as_str)
            .filter(|name| local.contains(name))
            .collect();
        names.sort_by_key(|name| (!pr_branches.contains(name), *name));
        stacks.push(PublishableStack {
            workstream_id: stream.id.clone(),
            title: stream.title.clone(),
            is_active: stream.is_active,
            change_count: stream.node_ids.len() - head_index,
            head: StackHead::Bookmark(names[0].to_owned()),
        });
    }
    stacks.sort_by_key(|stack| !stack.is_active);
    stacks
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShippableStack {
    pub workstream_id: String,
    pub title: String,
    pub head_change: String,
    pub head_title: String,
    pub is_active: bool,
    pub change_count: usize,
}

/// Every workstream that can ship straight to trunk: shipping needs no
/// bookmark, only a head change.
pub fn shippable_stacks(snapshot: &RepoSnapshot) -> Vec<ShippableStack> {
    let nodes: HashMap<&str, &GraphNode> =
        snapshot.nodes.iter().map(|n| (n.id.as_str(), n)).collect();
    let mut stacks = Vec::new();
    for stream in &snapshot.workstreams {
        let start = skip_draft(&nodes, stream.node_ids.first());
        let Some(head) = stream
            .node_ids
            .get(start)
            .and_then(|id| nodes.get(id.as_str()))
        else {
            continue;
        };
        stacks.push(ShippableStack {
            workstream_id: stream.id.clone(),
            title: stream.title.clone(),
            head_change: head.id.clone(),
            head_title: head_title(head),
            is_active: stream.is_active,
            change_count: stream.node_ids.len() - start,
        });
    }
    stacks.sort_by_key(|stack| !stack.is_active);
    stacks
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiji_core::{MockBackend, RepoBackend};

    fn snapshot() -> RepoSnapshot {
        let dir = std::env::temp_dir().join(format!("jiji-stacks-{}", std::process::id()));
        let _ = std::fs::create_dir_all(dir.join(".jj"));
        MockBackend::default().open(&dir).unwrap()
    }

    #[test]
    fn the_active_stack_leads_and_every_stack_has_a_head() {
        let snapshot = snapshot();
        let stacks = publishable_stacks(&snapshot, &HashSet::new());
        assert!(!stacks.is_empty());
        assert!(stacks
            .windows(2)
            .all(|pair| pair[0].is_active >= pair[1].is_active));
        for stack in &stacks {
            assert!(stack.change_count >= 1);
            match &stack.head {
                StackHead::Bookmark(name) => assert!(snapshot
                    .bookmarks
                    .iter()
                    .any(|b| b.is_local && !b.is_trunk && &b.name == name)),
                StackHead::Change { id, .. } => {
                    assert!(snapshot.nodes.iter().any(|n| &n.id == id))
                }
            }
        }
    }

    #[test]
    fn an_undescribed_working_copy_on_top_is_left_out() {
        let snapshot = snapshot();
        let active = snapshot.workstreams.iter().find(|w| w.is_active).unwrap();
        let top = snapshot
            .nodes
            .iter()
            .find(|n| n.id == active.node_ids[0])
            .unwrap();
        let shippable = shippable_stacks(&snapshot);
        let stack = shippable
            .iter()
            .find(|stack| stack.workstream_id == active.id)
            .unwrap();
        if top.kind == NodeKind::WorkingCopy && top.description.trim().is_empty() {
            assert_eq!(stack.head_change, active.node_ids[1]);
            assert_eq!(stack.change_count, active.node_ids.len() - 1);
        } else {
            assert_eq!(stack.head_change, active.node_ids[0]);
        }
    }

    #[test]
    fn a_bookmark_with_a_pull_request_is_the_preferred_head() {
        let mut snapshot = snapshot();
        // Give one bookmarked head a second local bookmark that sorts first.
        let stream = snapshot
            .workstreams
            .iter()
            .find(|w| w.bookmark.is_some())
            .cloned()
            .unwrap();
        let existing = stream.bookmark.clone().unwrap();
        let target = snapshot
            .bookmarks
            .iter()
            .find(|b| b.name == existing)
            .unwrap()
            .target
            .clone();
        let mut extra = snapshot
            .bookmarks
            .iter()
            .find(|b| b.name == existing)
            .unwrap()
            .clone();
        extra.name = "aaa-first".into();
        snapshot.bookmarks.push(extra);
        snapshot
            .nodes
            .iter_mut()
            .find(|n| n.id == target)
            .unwrap()
            .bookmarks
            .push("aaa-first".into());
        let by_name = publishable_stacks(&snapshot, &HashSet::new());
        let found = by_name
            .iter()
            .find(|s| s.workstream_id == stream.id)
            .unwrap();
        assert_eq!(found.head_bookmark(), Some("aaa-first"));
        let with_pr = publishable_stacks(&snapshot, &HashSet::from([existing.as_str()]));
        let found = with_pr
            .iter()
            .find(|s| s.workstream_id == stream.id)
            .unwrap();
        assert_eq!(found.head_bookmark(), Some(existing.as_str()));
    }
}
