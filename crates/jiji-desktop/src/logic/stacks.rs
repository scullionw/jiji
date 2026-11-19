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

    // ── Ported from the Tauri app's submit.test.ts and ship.test.ts ──

    use jiji_core::snapshot::{BookmarkState, SyncState, WorkstreamSummary};

    fn change(id: &str, kind: NodeKind, description: &str, bookmarks: &[&str]) -> GraphNode {
        GraphNode {
            id: id.into(),
            change_id: id.into(),
            commit_id: format!("c{id}"),
            description: description.into(),
            author: "test".into(),
            timestamp: "2026-07-01T12:00:00Z".into(),
            kind,
            parents: vec![],
            elided_parents: vec![],
            bookmarks: bookmarks.iter().map(|b| (*b).to_owned()).collect(),
            tags: vec![],
            is_empty: false,
            has_conflict: false,
            is_divergent: false,
        }
    }

    fn mutable(id: &str, bookmarks: &[&str]) -> GraphNode {
        change(id, NodeKind::Mutable, &format!("change {id}"), bookmarks)
    }

    fn local(name: &str, is_trunk: bool) -> BookmarkState {
        BookmarkState {
            name: name.into(),
            target: String::new(),
            remote: None,
            sync: SyncState::LocalOnly,
            is_trunk,
            is_local: true,
            tracked: false,
            is_merged: false,
            target_description: String::new(),
            target_author: String::new(),
            target_timestamp: String::new(),
        }
    }

    fn stream(id: &str, nodes: &[&str], active: bool) -> WorkstreamSummary {
        WorkstreamSummary {
            id: id.into(),
            title: format!("Stream {id}"),
            node_ids: nodes.iter().map(|n| (*n).to_owned()).collect(),
            bookmark: None,
            is_active: active,
            behind_trunk: 0,
        }
    }

    fn repo(
        nodes: Vec<GraphNode>,
        bookmarks: Vec<BookmarkState>,
        streams: Vec<WorkstreamSummary>,
    ) -> RepoSnapshot {
        RepoSnapshot {
            nodes,
            bookmarks,
            workstreams: streams,
            ..snapshot()
        }
    }

    #[test]
    fn a_stack_publishes_from_its_topmost_bookmark_and_counts_from_there() {
        let snapshot = repo(
            vec![
                mutable("wc", &[]),
                mutable("b", &["feat"]),
                mutable("c", &[]),
                mutable("d", &[]),
            ],
            vec![local("feat", false), local("main", true)],
            vec![stream("s1", &["wc", "b", "c", "d"], true)],
        );
        assert_eq!(
            publishable_stacks(&snapshot, &HashSet::new()),
            [PublishableStack {
                workstream_id: "s1".into(),
                title: "Stream s1".into(),
                is_active: true,
                change_count: 3,
                head: StackHead::Bookmark("feat".into()),
            }]
        );
    }

    #[test]
    fn a_stack_with_no_bookmark_publishes_from_its_head_change_active_first() {
        let snapshot = repo(
            vec![
                mutable("a", &["one"]),
                mutable("b", &[]),
                mutable("c", &["two"]),
            ],
            vec![local("one", false), local("two", false)],
            vec![
                stream("anon", &["b"], false),
                stream("other", &["a"], false),
                stream("active", &["c"], true),
            ],
        );
        let stacks = publishable_stacks(&snapshot, &HashSet::new());
        let order: Vec<&str> = stacks.iter().map(|s| s.workstream_id.as_str()).collect();
        assert_eq!(order, ["active", "anon", "other"]);
        assert_eq!(
            stacks[1],
            PublishableStack {
                workstream_id: "anon".into(),
                title: "Stream anon".into(),
                is_active: false,
                change_count: 1,
                head: StackHead::Change {
                    id: "b".into(),
                    title: "change b".into()
                },
            }
        );
        assert_eq!(stacks[2].head_bookmark(), Some("one"));
    }

    #[test]
    fn an_undescribed_draft_on_top_is_not_published_and_alone_offers_nothing() {
        let draft = change("wc", NodeKind::WorkingCopy, "", &[]);
        let snapshot = repo(
            vec![draft, mutable("b", &[]), mutable("c", &[])],
            vec![local("main", true)],
            vec![
                stream("s1", &["wc", "b", "c"], true),
                stream("only-draft", &["wc"], false),
            ],
        );
        assert_eq!(
            publishable_stacks(&snapshot, &HashSet::new()),
            [PublishableStack {
                workstream_id: "s1".into(),
                title: "Stream s1".into(),
                is_active: true,
                change_count: 2,
                head: StackHead::Change {
                    id: "b".into(),
                    title: "change b".into()
                },
            }]
        );
    }

    #[test]
    fn trunk_is_never_a_head_and_a_name_github_knows_wins_a_shared_change() {
        let snapshot = repo(
            vec![mutable("a", &["main", "zeta", "beta"])],
            vec![
                local("main", true),
                local("zeta", false),
                local("beta", false),
            ],
            vec![stream("s1", &["a"], true)],
        );
        // By name without PR knowledge…
        let named = publishable_stacks(&snapshot, &HashSet::new());
        assert_eq!(named[0].head_bookmark(), Some("beta"));
        // …but a branch with a pull request wins.
        let known = publishable_stacks(&snapshot, &HashSet::from(["zeta"]));
        assert_eq!(known[0].head_bookmark(), Some("zeta"));
    }

    #[test]
    fn every_stream_ships_bookmark_or_not_the_active_one_first() {
        let snapshot = repo(
            vec![
                mutable("a1", &[]),
                change("b1", NodeKind::WorkingCopy, "change b1", &[]),
                mutable("b2", &[]),
            ],
            vec![],
            vec![
                stream("a", &["a1"], false),
                stream("b", &["b1", "b2"], true),
            ],
        );
        let stacks = shippable_stacks(&snapshot);
        let order: Vec<&str> = stacks.iter().map(|s| s.workstream_id.as_str()).collect();
        assert_eq!(order, ["b", "a"]);
        assert_eq!(
            (stacks[0].head_change.as_str(), stacks[0].change_count),
            ("b1", 2)
        );
    }

    #[test]
    fn an_undescribed_working_copy_never_ships_but_a_described_one_does() {
        // Edits or not, undescribed is a draft, not what ships.
        let snapshot = repo(
            vec![
                change("wc", NodeKind::WorkingCopy, "", &[]),
                change("b1", NodeKind::Mutable, "feat: real work", &[]),
            ],
            vec![],
            vec![stream("b", &["wc", "b1"], true)],
        );
        let stacks = shippable_stacks(&snapshot);
        assert_eq!(stacks.len(), 1);
        assert_eq!(
            (
                stacks[0].head_change.as_str(),
                stacks[0].head_title.as_str(),
                stacks[0].change_count
            ),
            ("b1", "feat: real work", 1)
        );
        let mut lone = change("lone", NodeKind::WorkingCopy, "", &[]);
        lone.is_empty = true;
        let draft_only = repo(vec![lone], vec![], vec![stream("l", &["lone"], true)]);
        assert!(shippable_stacks(&draft_only).is_empty());
        let described = repo(
            vec![change("wc", NodeKind::WorkingCopy, "fix: on top", &[])],
            vec![],
            vec![stream("w", &["wc"], true)],
        );
        assert_eq!(shippable_stacks(&described)[0].head_change, "wc");
    }
}
