//! Stack discovery shared by the desktop and CLI. Graph lanes are layout,
//! not stack boundaries; branch identity also recognizes jjpr-created PRs.

use std::collections::{HashMap, HashSet};

use jiji_core::snapshot::{GraphNode, NodeKind, RepoSnapshot, TakenOver};
use serde::{Deserialize, Serialize};

use crate::{ChecksRollup, PrSummary, RepoPrState, ReviewDecision, SubmitHead};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StackSegment {
    pub bookmark: Option<String>,
    pub title: String,
    pub base: String,
    pub changes: Vec<GraphNode>,
    pub pr: Option<PrSummary>,
    /// Set when this segment's branch was someone else's and was taken
    /// over here ([`crate::takeover`]): for which pull request, and whose.
    #[serde(default)]
    pub taken_over: Option<TakenOver>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum StackStatus {
    Local,
    Conflicts,
    NeedsAttention,
    Draft,
    ChecksRunning,
    InReview,
    Published,
}

impl StackStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::Local => "Local",
            Self::Conflicts => "Conflicts",
            Self::NeedsAttention => "Needs attention",
            Self::Draft => "Draft",
            Self::ChecksRunning => "Checks running",
            Self::InReview => "In review",
            Self::Published => "Published",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PublishStack {
    pub key: String,
    pub title: String,
    pub head_bookmark: Option<String>,
    pub head_change: Option<String>,
    pub submit_head: Option<SubmitHead>,
    pub is_active: bool,
    /// Bottom first, in landing order. No Jiji PR-body metadata is required.
    pub segments: Vec<StackSegment>,
    pub base: String,
    pub change_count: u32,
    pub pr_count: u32,
    pub search_text: String,
    pub status: StackStatus,
    /// The GitHub stack these pull requests are linked into, when any are.
    #[serde(default)]
    pub github_stack: Option<GithubStack>,
    /// Open pull requests that rest on this stack and are someone else's:
    /// each is based on one of its branches, or on another of these, and
    /// its own branch is no bookmark here ([`crate::pr::others_on`]). A
    /// base comes before what rests on it. They are shown with the stack
    /// and are never its segments: nothing here pushes or merges them.
    #[serde(default)]
    pub others: Vec<PrSummary>,
}

/// How a stack's pull requests sit in one of GitHub's own stacks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GithubStack {
    /// The stack's number on GitHub.
    pub number: u64,
    /// How many of this stack's open pull requests are linked into it.
    /// Fewer than `pr_count` means the chain grew or changed since; the
    /// next publish brings the two back in step.
    pub linked: u32,
}

impl PublishStack {
    /// Select a prefix of a stack by its PR bookmark, also for remote-only
    /// stacks. This is what `jiji status <intermediate-bookmark>` describes.
    pub fn through_bookmark(&self, name: &str) -> Option<Self> {
        let index = self
            .segments
            .iter()
            .position(|s| s.bookmark.as_deref() == Some(name))?;
        let mut stack = self.clone();
        stack.segments.truncate(index + 1);
        let top = stack.segments.last()?;
        stack.key = name.to_owned();
        stack.title = top.title.clone();
        stack.head_bookmark = Some(name.to_owned());
        stack.head_change = top.changes.last().map(|c| c.id.clone());
        stack.submit_head = stack.head_change.as_ref().map(|_| SubmitHead::Bookmark {
            name: name.to_owned(),
        });
        stack.change_count = stack.segments.iter().map(|s| s.changes.len() as u32).sum();
        // Of someone else's pull requests, those that rest on what is
        // left: on a kept branch, or on one of them that does.
        let mut bases: Vec<String> = stack
            .segments
            .iter()
            .filter_map(|s| s.bookmark.clone())
            .collect();
        stack.others.retain(|other| {
            let rests = bases.contains(&other.base_branch);
            if rests {
                bases.push(other.head_branch.clone());
            }
            rests
        });
        Some(stack.finish())
    }

    /// Whether jj calls any of the stack's changes immutable because one
    /// of [`PublishStack::others`] (or another branch that is no bookmark
    /// here) builds on it. Publishing and landing go on as usual; editing
    /// such a change by hand does not, until that branch is tracked here
    /// or has moved off it.
    pub fn held(&self) -> bool {
        self.segments
            .iter()
            .any(|segment| segment.changes.iter().any(|change| change.held))
    }

    fn finish(mut self) -> Self {
        let prs: Vec<_> = self.segments.iter().filter_map(|s| s.pr.as_ref()).collect();
        self.pr_count = prs.len() as u32;
        // The stack the bottom-most linked pull request sits in; a chain
        // spread over two GitHub stacks reads as the lower one, partly
        // linked.
        self.github_stack = prs
            .iter()
            .find_map(|pr| pr.stack.as_ref())
            .map(|seat| GithubStack {
                number: seat.number,
                linked: prs
                    .iter()
                    .filter(|pr| pr.stack.as_ref().is_some_and(|s| s.number == seat.number))
                    .count() as u32,
            });
        self.status = if self
            .segments
            .iter()
            .any(|s| s.changes.iter().any(|c| c.has_conflict))
        {
            StackStatus::Conflicts
        } else if prs.is_empty() {
            StackStatus::Local
        } else if prs.iter().any(|p| {
            p.checks == ChecksRollup::Failing || p.review == ReviewDecision::ChangesRequested
        }) {
            StackStatus::NeedsAttention
        } else if prs.iter().any(|p| p.is_draft) {
            StackStatus::Draft
        } else if prs.iter().any(|p| p.checks == ChecksRollup::Pending) {
            StackStatus::ChecksRunning
        } else if prs
            .iter()
            .any(|p| p.review == ReviewDecision::ReviewRequired)
        {
            StackStatus::InReview
        } else {
            StackStatus::Published
        };
        self.search_text = std::iter::once(self.title.clone())
            .chain(self.head_bookmark.iter().cloned())
            .chain(self.segments.iter().flat_map(|s| {
                [
                    s.bookmark.clone().unwrap_or_default(),
                    s.title.clone(),
                    s.pr.as_ref()
                        .map(|p| format!("#{}", p.number))
                        .unwrap_or_default(),
                ]
            }))
            .collect::<Vec<_>>()
            .join(" ")
            .to_lowercase();
        self
    }
}

/// Discover local and remote-only stacks. `RepoPrState::new` filters fork
/// PRs out of `by_branch`, so a fork's same-named branch cannot be adopted.
pub fn discover_stacks(snapshot: &RepoSnapshot, prs: Option<&RepoPrState>) -> Vec<PublishStack> {
    let empty = HashMap::new();
    let by_branch = prs.map(|p| &p.by_branch).unwrap_or(&empty);
    let bookmarks: HashMap<_, _> = snapshot
        .bookmarks
        .iter()
        .filter(|b| b.is_local && !b.is_trunk)
        .map(|b| (b.name.as_str(), b))
        .collect();
    let nodes: HashMap<_, _> = snapshot.nodes.iter().map(|n| (n.id.as_str(), n)).collect();
    let mut result = Vec::new();
    let mut seen = HashSet::new();
    // Active first also makes a duplicated head inherit the active lane.
    let mut streams: Vec<_> = snapshot.workstreams.iter().collect();
    streams.sort_by_key(|s| !s.is_active);
    for stream in streams {
        let named_head = stream
            .node_ids
            .iter()
            .filter_map(|id| nodes.get(id.as_str()))
            .find(|n| {
                n.bookmarks
                    .iter()
                    .any(|name| bookmarks.contains_key(name.as_str()))
            });
        let (head, head_bookmark) = if let Some(node) = named_head {
            let mut names: Vec<_> = node
                .bookmarks
                .iter()
                .filter(|n| bookmarks.contains_key(n.as_str()))
                .collect();
            names.sort_by_key(|name| (!by_branch.contains_key(*name), name.as_str()));
            let name = names[0];
            (bookmarks[name.as_str()].target.as_str(), Some(name.clone()))
        } else {
            let mut ids = stream.node_ids.iter();
            let first = ids.next().and_then(|id| nodes.get(id.as_str()));
            let node = match first {
                Some(n) if n.kind == NodeKind::WorkingCopy && n.description.trim().is_empty() => {
                    ids.next().and_then(|id| nodes.get(id.as_str()))
                }
                other => other,
            };
            let Some(node) = node else { continue };
            (node.id.as_str(), None)
        };
        let key = head_bookmark.clone().unwrap_or_else(|| head.to_owned());
        if !seen.insert(key.clone()) {
            continue;
        }
        let mut chain = Vec::new();
        let mut visited = HashSet::new();
        let mut node = nodes.get(head).copied();
        while let Some(current) = node {
            if current.is_settled() || !visited.insert(&current.id) {
                break;
            }
            chain.push(current);
            node = current
                .parents
                .first()
                .and_then(|id| nodes.get(id.as_str()))
                .copied();
        }
        chain.reverse();
        if chain.is_empty()
            || (head_bookmark.is_none()
                && chain
                    .iter()
                    .all(|n| n.is_empty && n.description.trim().is_empty()))
        {
            continue;
        }
        let mut segments = Vec::new();
        let mut pending = Vec::new();
        let mut base = snapshot.trunk_bookmark.clone();
        for (index, node) in chain.iter().enumerate() {
            pending.push((*node).clone());
            let mut names: Vec<_> = node
                .bookmarks
                .iter()
                .filter(|n| bookmarks.contains_key(n.as_str()))
                .collect();
            names.sort_by_key(|name| {
                (
                    Some(name.as_str()) != head_bookmark.as_deref(),
                    !by_branch.contains_key(*name),
                    name.as_str(),
                )
            });
            let bookmark = names.first().map(|name| (*name).clone());
            if bookmark.is_none() && index + 1 != chain.len() {
                continue;
            }
            let pr = bookmark
                .as_ref()
                .and_then(|name| by_branch.get(name))
                .cloned();
            let title = pr.as_ref().map(|p| p.title.clone()).unwrap_or_else(|| {
                pending[0]
                    .description
                    .lines()
                    .next()
                    .filter(|s| !s.is_empty())
                    .unwrap_or("Untitled change")
                    .to_owned()
            });
            let taken_over = bookmark
                .as_deref()
                .and_then(|name| crate::takeover::taken_over(snapshot, name, pr.as_ref()));
            segments.push(StackSegment {
                bookmark: bookmark.clone(),
                title,
                base: base.clone(),
                changes: std::mem::take(&mut pending),
                pr,
                taken_over,
            });
            if let Some(name) = bookmark {
                base = name;
            }
        }
        let submit_head = Some(match &head_bookmark {
            Some(name) => SubmitHead::Bookmark { name: name.clone() },
            None => SubmitHead::Change {
                change: head.to_owned(),
            },
        });
        result.push(
            PublishStack {
                key,
                title: segments.last().unwrap().title.clone(),
                head_bookmark,
                head_change: Some(head.to_owned()),
                submit_head,
                is_active: stream.is_active,
                base: segments[0]
                    .pr
                    .as_ref()
                    .map(|p| p.base_branch.clone())
                    .unwrap_or_else(|| snapshot.trunk_bookmark.clone()),
                segments,
                change_count: chain.len() as u32,
                pr_count: 0,
                search_text: String::new(),
                status: StackStatus::Local,
                github_stack: None,
                others: Vec::new(),
            }
            .finish(),
        );
    }
    // Someone else's pull requests on each local stack.
    if let Some(prs) = prs {
        for stack in &mut result {
            let branches: Vec<&str> = stack
                .segments
                .iter()
                .filter_map(|segment| segment.bookmark.as_deref())
                .collect();
            stack.others = crate::pr::others_on(snapshot, prs, &branches)
                .into_iter()
                .cloned()
                .collect();
        }
    }
    let represented: HashSet<_> = result
        .iter()
        .flat_map(|s| s.segments.iter().filter_map(|s| s.bookmark.clone()))
        .collect();
    let parents: HashSet<_> = by_branch.values().map(|p| &p.base_branch).collect();
    let mut remote_heads: Vec<_> = by_branch.values().collect();
    remote_heads.sort_by_key(|p| std::cmp::Reverse(p.number));
    for pr in remote_heads {
        if parents.contains(&pr.head_branch) || represented.contains(&pr.head_branch) {
            continue;
        }
        let mut segments = Vec::new();
        let mut visited = HashSet::new();
        let mut current = Some(pr);
        while let Some(pr) = current {
            if !visited.insert(&pr.head_branch) {
                break;
            }
            segments.push(StackSegment {
                bookmark: Some(pr.head_branch.clone()),
                title: pr.title.clone(),
                base: pr.base_branch.clone(),
                changes: vec![],
                pr: Some(pr.clone()),
                taken_over: None,
            });
            // Down to where a stack that is here begins, and no further:
            // a pull request someone else opened on top of a local stack
            // is a stack of its own, based on that one's branch. Walked
            // on, it would list the local stack a second time as
            // something that is on GitHub only.
            current = by_branch
                .get(&pr.base_branch)
                .filter(|below| !represented.contains(&below.head_branch));
        }
        segments.reverse();
        result.push(
            PublishStack {
                key: format!("remote:{}", pr.head_branch),
                title: pr.title.clone(),
                head_bookmark: Some(pr.head_branch.clone()),
                head_change: None,
                submit_head: None,
                is_active: false,
                base: segments[0].base.clone(),
                segments,
                change_count: 0,
                pr_count: 0,
                search_text: String::new(),
                status: StackStatus::Published,
                github_stack: None,
                others: Vec::new(),
            }
            .finish(),
        );
    }
    result.sort_by_key(|s| (!s.is_active, s.pr_count == 0));
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (RepoSnapshot, RepoPrState, Vec<PublishStack>) {
        #[derive(Deserialize)]
        struct Fixture {
            snapshot: RepoSnapshot,
            prs: RepoPrState,
            stacks: Vec<PublishStack>,
        }
        let f: Fixture =
            serde_json::from_str(include_str!("../tests/fixtures/publish-stack.json")).unwrap();
        (f.snapshot, f.prs, f.stacks)
    }

    #[test]
    fn jjpr_stack_matches_the_desktop_fixture_without_jiji_metadata() {
        let (snapshot, prs, mut expected) = fixture();
        let actual = discover_stacks(&snapshot, Some(&prs));
        // Search whitespace isn't a UI contract; search terms and all the
        // structure, identities, statuses, and counts are shared verbatim.
        for (actual, expected) in actual.iter().zip(&mut expected) {
            assert_eq!(
                actual.search_text.split_whitespace().collect::<Vec<_>>(),
                expected.search_text.split_whitespace().collect::<Vec<_>>()
            );
            expected.search_text = actual.search_text.clone();
        }
        assert_eq!(actual, expected);
        assert_eq!(
            actual[0]
                .segments
                .iter()
                .map(|s| s.pr.as_ref().unwrap().number)
                .collect::<Vec<_>>(),
            [753, 754, 755, 756, 757]
        );
        assert_eq!(
            actual[0]
                .segments
                .iter()
                .map(|s| s.changes.len())
                .collect::<Vec<_>>(),
            [1, 1, 1, 3, 1]
        );
    }

    #[test]
    fn walks_shared_ancestors_outside_the_display_lane() {
        let (mut snapshot, prs, _) = fixture();
        snapshot.workstreams[0].node_ids = ["wc", "c6", "c5"].map(str::to_owned).to_vec();
        assert_eq!(discover_stacks(&snapshot, Some(&prs))[0].pr_count, 5);
    }

    #[test]
    fn remote_only_stacks_follow_pr_bases_and_cannot_execute_locally() {
        let (mut snapshot, prs, _) = fixture();
        snapshot.nodes.clear();
        snapshot.workstreams.clear();
        let stacks = discover_stacks(&snapshot, Some(&prs));
        assert_eq!(stacks.len(), 1);
        assert_eq!(stacks[0].pr_count, 5);
        assert_eq!(stacks[0].base, "develop");
        assert!(stacks[0].submit_head.is_none());
        assert!(stacks[0]
            .through_bookmark("ws/tu2")
            .unwrap()
            .submit_head
            .is_none());
    }

    /// Pull requests other people opened on a local stack: one on its
    /// top, one on a branch in the middle, one on top of theirs.
    fn with_others(prs: &RepoPrState) -> RepoPrState {
        let theirs = |number: u64, head: &str, base: &str, author: &str| PrSummary {
            number,
            title: format!("{head} by {author}"),
            head_branch: head.into(),
            base_branch: base.into(),
            author: Some(author.into()),
            ..prs.report.prs[0].clone()
        };
        let mut report = prs.report.clone();
        report.prs.push(theirs(900, "their-top", "ws/tu3-emission", "kim"));
        report.prs.push(theirs(901, "their-mid", "ws/tu2", "sam"));
        report.prs.push(theirs(902, "their-next", "their-top", "kim"));
        RepoPrState::new(report, "org")
    }

    #[test]
    fn someone_elses_pull_requests_show_with_the_stack_and_are_not_its_segments() {
        let (snapshot, prs, _) = fixture();
        let prs = with_others(&prs);
        let stacks = discover_stacks(&snapshot, Some(&prs));

        // The local stack is what it was: five segments, five pull
        // requests of its own.
        let own = &stacks[0];
        assert_eq!((own.segments.len(), own.pr_count), (5, 5));
        assert!(own.submit_head.is_some());
        // And it says who else is on it: bases before what rests on them.
        assert_eq!(
            own.others
                .iter()
                .map(|pr| (pr.number, pr.base_branch.as_str()))
                .collect::<Vec<_>>(),
            [(901, "ws/tu2"), (900, "ws/tu3-emission"), (902, "their-top")]
        );
        assert!(!own.held());

        // Theirs are stacks of their own, on GitHub only, and each starts
        // where the local stack ends: the local one is not listed twice.
        let remote: Vec<(&str, Vec<&str>, &str)> = stacks
            .iter()
            .filter(|stack| stack.submit_head.is_none())
            .map(|stack| {
                (
                    stack.key.as_str(),
                    stack
                        .segments
                        .iter()
                        .filter_map(|s| s.bookmark.as_deref())
                        .collect(),
                    stack.base.as_str(),
                )
            })
            .collect();
        assert_eq!(
            remote,
            [
                ("remote:their-next", vec!["their-top", "their-next"], "ws/tu3-emission"),
                ("remote:their-mid", vec!["their-mid"], "ws/tu2"),
            ]
        );

        // A prefix of the stack keeps only what rests on that prefix.
        let lower = own.through_bookmark("ws/tu2").unwrap();
        assert_eq!(lower.others.iter().map(|pr| pr.number).collect::<Vec<_>>(), [901]);
        let bottom = own.through_bookmark("ws/tu1").unwrap();
        assert!(bottom.others.is_empty());
    }

    #[test]
    fn a_stack_whose_changes_someone_elses_branch_holds_is_still_a_stack() {
        // What a fetch leaves once their branch is here: jj calls the
        // stack's changes immutable, and the snapshot marks them as held.
        let (mut snapshot, prs, _) = fixture();
        let prs = with_others(&prs);
        let stack_ids: Vec<String> = discover_stacks(&snapshot, Some(&prs))[0]
            .segments
            .iter()
            .flat_map(|segment| segment.changes.iter().map(|change| change.id.clone()))
            .collect();
        for node in snapshot.nodes.iter_mut().filter(|n| stack_ids.contains(&n.id)) {
            node.kind = NodeKind::Immutable;
            node.held = true;
        }
        let stacks = discover_stacks(&snapshot, Some(&prs));
        let own = &stacks[0];
        assert_eq!((own.segments.len(), own.pr_count), (5, 5));
        assert_eq!(own.change_count as usize, stack_ids.len());
        assert!(own.held());

        // Without the mark they would be landed history, and no stack.
        for node in snapshot.nodes.iter_mut() {
            node.held = false;
        }
        let stacks = discover_stacks(&snapshot, Some(&prs));
        assert!(stacks.iter().all(|stack| stack.submit_head.is_none() || stack.pr_count == 0));
    }

    /// Taken over, their pull request is a segment of the stack that says
    /// whose it is, and nothing lists it as someone else's any more.
    #[test]
    fn a_taken_over_pull_request_is_a_segment_that_says_whose_it_is() {
        let (mut snapshot, prs, _) = fixture();
        let prs = with_others(&prs);
        // Their branch "their-top" (#900 by kim) is a bookmark here now,
        // on the working copy's change above the stack's top.
        let mut theirs = snapshot.bookmarks[5].clone();
        theirs.name = "their-top".into();
        theirs.target = "wc".into();
        snapshot.bookmarks.push(theirs);
        let top = snapshot.nodes.iter_mut().find(|n| n.id == "wc").unwrap();
        top.bookmarks = vec!["their-top".into()];
        top.description = "their fix".into();
        snapshot.workstreams[0].bookmark = Some("their-top".into());
        snapshot.taken_over = vec![TakenOver {
            bookmark: "their-top".into(),
            remote: "origin".into(),
            number: 900,
            author: Some("kim".into()),
        }];

        let stacks = discover_stacks(&snapshot, Some(&prs));
        let own = &stacks[0];
        assert_eq!((own.segments.len(), own.pr_count), (6, 6));
        let marks: Vec<Option<String>> = own
            .segments
            .iter()
            .map(|s| s.taken_over.as_ref().map(|t| t.by_author()))
            .collect();
        assert_eq!(marks, [None, None, None, None, None, Some("#900 by @kim".into())]);
        // #900 is the stack's own now. What rests on it, and what rests
        // elsewhere on the stack, are still someone else's.
        assert_eq!(
            own.others.iter().map(|pr| pr.number).collect::<Vec<_>>(),
            [901, 902]
        );
        // And it is no stack of its own on GitHub any more.
        assert!(
            !stacks
                .iter()
                .any(|stack| stack.submit_head.is_none()
                    && stack.segments.iter().any(|s| s.bookmark.as_deref() == Some("their-top"))),
            "{:?}",
            stacks.iter().map(|s| s.key.as_str()).collect::<Vec<_>>()
        );

        // A prefix that ends below it carries no mark.
        let lower = own.through_bookmark("ws/tu3-emission").unwrap();
        assert!(lower.segments.iter().all(|s| s.taken_over.is_none()));

        // A record that names another pull request is a name used again.
        snapshot.taken_over[0].number = 12;
        let stacks = discover_stacks(&snapshot, Some(&prs));
        assert!(stacks[0].segments.iter().all(|s| s.taken_over.is_none()));
    }

    #[test]
    fn fork_branches_do_not_attach_to_same_named_local_bookmarks() {
        let (snapshot, mut prs, _) = fixture();
        prs.report.prs[4].head_owner = Some("a-fork".into());
        let prs = RepoPrState::new(prs.report, "org");
        let stacks = discover_stacks(&snapshot, Some(&prs));
        assert_eq!(stacks[0].pr_count, 4);
        assert!(stacks[0].segments[4].pr.is_none());
    }

    #[test]
    fn deduplicates_heads_and_prefers_the_active_lane() {
        let (mut snapshot, prs, _) = fixture();
        let mut duplicate = snapshot.workstreams[0].clone();
        duplicate.id = "another-lane".into();
        duplicate.is_active = false;
        snapshot.workstreams.insert(0, duplicate);
        let stacks = discover_stacks(&snapshot, Some(&prs));
        assert_eq!(stacks.len(), 2);
        assert!(stacks[0].is_active);
        assert_eq!(stacks[1].status, StackStatus::Local);
    }

    #[test]
    fn intermediate_bookmarks_select_only_the_requested_prefix() {
        let (snapshot, prs, _) = fixture();
        let stack = discover_stacks(&snapshot, Some(&prs))[0]
            .through_bookmark("ws/tu2")
            .unwrap();
        assert_eq!(stack.pr_count, 2);
        assert_eq!(stack.change_count, 2);
        assert_eq!(stack.head_change.as_deref(), Some("c1"));
        assert_eq!(
            stack.submit_head,
            Some(SubmitHead::Bookmark {
                name: "ws/tu2".into()
            })
        );
    }

    #[test]
    fn status_reports_failures_before_drafts_and_never_calls_green_ci_mergeable() {
        let (snapshot, mut prs, _) = fixture();
        prs.by_branch.get_mut("ws/tu1").unwrap().checks = ChecksRollup::Failing;
        assert_eq!(
            discover_stacks(&snapshot, Some(&prs))[0].status,
            StackStatus::NeedsAttention
        );
        for pr in prs.by_branch.values_mut() {
            pr.is_draft = false;
            pr.checks = ChecksRollup::Passing;
        }
        assert_eq!(
            discover_stacks(&snapshot, Some(&prs))[0].status,
            StackStatus::Published
        );
    }
}
