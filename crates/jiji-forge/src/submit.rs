//! The submit engine: analyze a bookmark's stack from the repo snapshot,
//! plan the minimum set of remote actions, then execute the plan.
//!
//! The shape is jjpr's (see the jjpr inspiration note): every submission
//! derives an explicit plan first — which bookmarks push, which PRs open
//! and against which bases, which existing PRs retarget — and execution
//! walks that plan. That is what makes submit idempotent: re-running
//! against an already-consistent stack plans nothing, and "the stack is up
//! to date" is an empty action list, not a special case. The plan is a
//! ts-rs DTO so the Publish section renders exactly what will run, and a
//! future CLI can print the same object.
//!
//! What this slice deliberately leaves to later slices: PR body
//! reconciliation against hand edits (jjpr's fingerprinting), the
//! stack-info comment, draft handling, foreign bases (a coworker's branch
//! in the stack's ancestry — the snapshot does not carry remote-only
//! bookmarks on nodes yet), and recognizing already-merged PRs (needs a
//! per-bookmark merged-PR query; the land flow owns it).

use jiji_core::snapshot::{GraphNode, NodeKind, RepoSnapshot, SyncState};
use jiji_core::BackendError;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::error::ForgeError;
use crate::github::GitHubClient;
use crate::pr::{PrSummary, RepoPrState};
use crate::remote::ForgeRepo;

/// One publishable run of changes under a bookmark, listed bottom-up in
/// [`SubmitPlan::segments`]. Mirrors jjpr's segment: the commits between
/// the bookmark below (or trunk) and this segment's bookmark.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SubmitSegment {
    pub bookmark: String,
    /// Branch this segment's PR merges into: the trunk branch for the
    /// bottom segment, the bookmark below otherwise.
    pub base: String,
    /// Change ids in the segment, bottom-first.
    pub change_ids: Vec<String>,
    /// PR title the segment would get (or has): the bottom change's
    /// description first line.
    pub title: String,
    /// The open PR GitHub already has for this bookmark, when one exists.
    pub pr: Option<PrSummary>,
}

/// One remote action the plan will run, in execution order.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "camelCase")]
#[ts(export)]
pub enum SubmitAction {
    /// Update the remote branch behind a bookmark (`jj git push`); all
    /// pushes in a plan run as one batched operation.
    #[serde(rename_all = "camelCase")]
    Push { bookmark: String, create: bool },
    /// Open a pull request for a segment.
    #[serde(rename_all = "camelCase")]
    CreatePr {
        bookmark: String,
        base: String,
        title: String,
        body: String,
    },
    /// Point an existing PR at the base the stack shape expects.
    #[serde(rename_all = "camelCase")]
    RetargetPr {
        number: u64,
        bookmark: String,
        from_base: String,
        to_base: String,
    },
}

/// What submitting a stack will do, derived before anything runs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SubmitPlan {
    /// The bookmark the plan publishes up to (the stack's top bookmark).
    pub head_bookmark: String,
    /// Git remote the pushes go to — the detected forge repo's remote.
    pub remote: String,
    /// The trunk branch the bottom segment's PR targets.
    pub base_branch: String,
    /// The stack's publishable segments, bottom-up.
    pub segments: Vec<SubmitSegment>,
    /// Everything that will run, in order. Empty means up to date.
    pub actions: Vec<SubmitAction>,
    /// Problems that stop the plan from running at all (undescribed or
    /// conflicted commits in a pushed segment, a conflicted bookmark).
    pub blockers: Vec<String>,
    /// Worth knowing, but the plan still runs.
    pub warnings: Vec<String>,
}

/// Per-action result of executing a plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum SubmitStepStatus {
    Done,
    Failed,
    /// Not attempted because an earlier step failed (or a blocker stopped
    /// the plan).
    Skipped,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SubmitStep {
    pub action: SubmitAction,
    pub status: SubmitStepStatus,
    /// Plain-language result: the push summary, or the failure message.
    pub detail: Option<String>,
    /// The PR a `CreatePr` step opened, for linking out.
    pub pr: Option<PrSummary>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SubmitOutcome {
    pub steps: Vec<SubmitStep>,
    pub failed: bool,
}

/// The jj side of executing a plan, host-implemented over `jiji-core`'s
/// `RepoBackend::push_bookmarks` (a stub in tests). One call pushes all of
/// a plan's bookmarks as one operation, exactly like `jj git push -b a -b
/// b`; the returned string is the outcome summary.
pub trait SubmitVcs {
    fn push_bookmarks(&self, bookmarks: &[String], remote: &str) -> Result<String, BackendError>;
}

/// The forge side of executing a plan; implemented by [`RepoForge`] over
/// the real client, a stub in tests.
pub trait SubmitForge {
    fn create_pr(
        &self,
        title: &str,
        body: &str,
        head: &str,
        base: &str,
    ) -> Result<PrSummary, ForgeError>;
    fn update_pr_base(&self, number: u64, base: &str) -> Result<(), ForgeError>;
}

/// [`SubmitForge`] over the real GitHub client, bound to a detected repo.
pub struct RepoForge<'a> {
    pub client: &'a GitHubClient,
    pub repo: &'a ForgeRepo,
}

impl SubmitForge for RepoForge<'_> {
    fn create_pr(
        &self,
        title: &str,
        body: &str,
        head: &str,
        base: &str,
    ) -> Result<PrSummary, ForgeError> {
        self.client
            .create_pr(&self.repo.owner, &self.repo.name, title, body, head, base)
    }

    fn update_pr_base(&self, number: u64, base: &str) -> Result<(), ForgeError> {
        self.client
            .update_pr_base(&self.repo.owner, &self.repo.name, number, base)
    }
}

/// Build the submission plan for the stack under `head_bookmark`: walk the
/// snapshot graph from the bookmark down first parents to the immutable
/// base, segment the mutable chain at local non-trunk bookmarks, and
/// compare each segment against the forge's open-PR state.
pub fn plan_submit(
    snapshot: &RepoSnapshot,
    prs: &RepoPrState,
    repo: &ForgeRepo,
    head_bookmark: &str,
) -> Result<SubmitPlan, ForgeError> {
    let bookmark = snapshot
        .bookmarks
        .iter()
        .find(|b| b.name == head_bookmark && b.is_local)
        .ok_or_else(|| {
            ForgeError::Plan(format!(
                "there is no local bookmark named \u{201c}{head_bookmark}\u{201d}"
            ))
        })?;
    if bookmark.is_trunk {
        return Err(ForgeError::Plan(format!(
            "\u{201c}{head_bookmark}\u{201d} is the trunk — it is what stacks land on, \
             not something to submit"
        )));
    }

    let node_by_id: std::collections::HashMap<&str, &GraphNode> =
        snapshot.nodes.iter().map(|n| (n.id.as_str(), n)).collect();
    let mut chain: Vec<&GraphNode> = Vec::new();
    let mut cursor = node_by_id.get(bookmark.target.as_str()).copied();
    while let Some(node) = cursor {
        if node.kind == NodeKind::Immutable {
            break;
        }
        chain.push(node);
        cursor = node
            .parents
            .first()
            .and_then(|id| node_by_id.get(id.as_str()).copied());
    }
    if chain.is_empty() {
        return Err(ForgeError::Plan(format!(
            "\u{201c}{head_bookmark}\u{201d} points at immutable history — there is \
             nothing to publish"
        )));
    }
    chain.reverse(); // bottom-up, ending at the head bookmark's change

    // Segment the chain at bookmarked changes. Every local non-trunk
    // bookmark ends a segment; several on one change collapse into one
    // segment published through one of them (jjpr collapses the same way).
    let mut warnings: Vec<String> = Vec::new();
    let mut blockers: Vec<String> = Vec::new();
    let local_bookmarks: std::collections::HashSet<&str> = snapshot
        .bookmarks
        .iter()
        .filter(|b| b.is_local && !b.is_trunk)
        .map(|b| b.name.as_str())
        .collect();
    struct RawSegment<'a> {
        bookmark: &'a str,
        nodes: Vec<&'a GraphNode>,
    }
    let mut raw_segments: Vec<RawSegment> = Vec::new();
    let mut pending: Vec<&GraphNode> = Vec::new();
    for node in &chain {
        pending.push(node);
        let mut names: Vec<&str> = node
            .bookmarks
            .iter()
            .map(String::as_str)
            .filter(|name| local_bookmarks.contains(name))
            .collect();
        if names.is_empty() {
            continue;
        }
        // Prefer the name GitHub already knows; the head bookmark always
        // publishes through itself.
        names.sort_by_key(|name| {
            (
                *name != head_bookmark,
                prs.by_branch.get(*name).is_none(),
                name.to_owned(),
            )
        });
        if names.len() > 1 {
            warnings.push(format!(
                "{} point at the same change; publishing through \u{201c}{}\u{201d}",
                names.join(" and "),
                names[0]
            ));
        }
        raw_segments.push(RawSegment {
            bookmark: names[0],
            nodes: pending.drain(..).collect(),
        });
    }
    // The walk ends at the head bookmark's change, which closes the last
    // segment; anything left over would mean the bookmark vanished.
    debug_assert!(pending.is_empty());

    // Compare each segment with the forge state, bottom-up, tracking the
    // effective base like jjpr: trunk first, then each live segment's
    // bookmark.
    let mut segments: Vec<SubmitSegment> = Vec::new();
    let mut actions: Vec<SubmitAction> = Vec::new();
    let mut pr_actions: Vec<SubmitAction> = Vec::new();
    let mut effective_base = snapshot.trunk_bookmark.clone();
    if prs.report.truncated {
        warnings.push(
            "GitHub answered only the 100 most recently updated open PRs; an existing \
             PR past that may be missed and recreated"
                .to_owned(),
        );
    }
    for raw in &raw_segments {
        let name = raw.bookmark;
        let state = snapshot
            .bookmarks
            .iter()
            .find(|b| b.name == name)
            .expect("segment bookmarks come from the snapshot");
        let bottom = raw.nodes.first().expect("segments are never empty");
        let (title, body) = derive_title_body(&bottom.description, name);
        let pr = prs.by_branch.get(name).cloned();
        let segment = SubmitSegment {
            bookmark: name.to_owned(),
            base: effective_base.clone(),
            change_ids: raw.nodes.iter().map(|n| n.id.clone()).collect(),
            title: title.clone(),
            pr: pr.clone(),
        };

        // An all-empty segment pushes nothing: an empty diff would make
        // GitHub auto-close the PR (jjpr's rule). It still becomes the
        // base below the next segment.
        if raw.nodes.iter().all(|n| n.is_empty) {
            warnings.push(format!(
                "every change under \u{201c}{name}\u{201d} is empty; skipping its push \
                 and PR"
            ));
            effective_base = name.to_owned();
            segments.push(segment);
            continue;
        }

        let needs_push = state.sync != SyncState::Synced || state.remote.is_none();
        if needs_push {
            if snapshot
                .conflicts
                .iter()
                .any(|c| c.id == format!("bookmark-{name}"))
            {
                blockers.push(format!(
                    "bookmark \u{201c}{name}\u{201d} is conflicted; repoint it before \
                     publishing"
                ));
            }
            // Only commits that would land on the remote need to be
            // presentable — the same checks the push itself enforces,
            // surfaced at plan time so the panel can say so.
            for node in &raw.nodes {
                let change = &node.change_id;
                if node.description.is_empty() {
                    blockers.push(format!("{change} has no description; describe it first"));
                }
                if node.has_conflict {
                    blockers.push(format!("{change} has conflicts; resolve them first"));
                }
                if node.is_divergent {
                    blockers.push(format!(
                        "{change} is divergent; resolve the divergence first"
                    ));
                }
            }
            match state.sync {
                SyncState::Behind => warnings.push(format!(
                    "\u{201c}{name}\u{201d} is behind its remote; the push moves the \
                     remote branch backwards"
                )),
                SyncState::Diverged => warnings.push(format!(
                    "\u{201c}{name}\u{201d} and its remote have diverged; the push \
                     replaces the remote position"
                )),
                _ => {}
            }
            actions.push(SubmitAction::Push {
                bookmark: name.to_owned(),
                create: state.sync == SyncState::LocalOnly,
            });
        }

        match &pr {
            Some(pr) => {
                if pr.base_branch != effective_base {
                    pr_actions.push(SubmitAction::RetargetPr {
                        number: pr.number,
                        bookmark: name.to_owned(),
                        from_base: pr.base_branch.clone(),
                        to_base: effective_base.clone(),
                    });
                }
            }
            None => {
                pr_actions.push(SubmitAction::CreatePr {
                    bookmark: name.to_owned(),
                    base: effective_base.clone(),
                    title,
                    body,
                });
            }
        }

        effective_base = name.to_owned();
        segments.push(segment);
    }
    // Pushes first (a new PR's head and base branches must exist), then PR
    // creations bottom-up, then retargets.
    let (creates, retargets): (Vec<_>, Vec<_>) = pr_actions
        .into_iter()
        .partition(|a| matches!(a, SubmitAction::CreatePr { .. }));
    actions.extend(creates);
    actions.extend(retargets);

    Ok(SubmitPlan {
        head_bookmark: head_bookmark.to_owned(),
        remote: repo.remote.clone(),
        base_branch: snapshot.trunk_bookmark.clone(),
        segments,
        actions,
        blockers,
        warnings,
    })
}

/// PR title and body from a change description: first line titles, the
/// rest bodies, with a trailing block of git trailers (`Co-authored-by:`
/// and friends) dropped — commit attribution is not a PR description
/// (jjpr strips the same set).
fn derive_title_body(description: &str, fallback: &str) -> (String, String) {
    let description = description.trim();
    if description.is_empty() {
        return (fallback.to_owned(), String::new());
    }
    let title = description.lines().next().unwrap_or(fallback).to_owned();
    let body = strip_trailers(description[title.len()..].trim());
    (title, body)
}

const TRAILER_KEYS: &[&str] = &[
    "co-authored-by",
    "co-developed-by",
    "signed-off-by",
    "helped-by",
    "reviewed-by",
    "acked-by",
    "tested-by",
    "reported-by",
    "suggested-by",
    "change-id",
];

/// Drop a trailing block of recognized git trailers (and the blank lines
/// around it). A trailer mid-body, or any non-trailer line, stops the scan.
fn strip_trailers(body: &str) -> String {
    let lines: Vec<&str> = body.lines().collect();
    let mut end = lines.len();
    while end > 0 {
        let line = lines[end - 1].trim();
        if line.is_empty() {
            end -= 1;
            continue;
        }
        let is_trailer = line.split_once(':').is_some_and(|(key, value)| {
            !value.trim().is_empty()
                && TRAILER_KEYS.contains(&key.trim().to_ascii_lowercase().as_str())
        });
        if is_trailer {
            end -= 1;
        } else {
            break;
        }
    }
    lines[..end].join("\n").trim_end().to_owned()
}

/// Run a plan: one batched push, then PR creations bottom-up, then base
/// retargets. The first failure stops execution — later steps report as
/// skipped rather than running against a half-updated remote. A plan with
/// blockers refuses to run at all.
pub fn execute_submit(
    plan: &SubmitPlan,
    vcs: &dyn SubmitVcs,
    forge: &dyn SubmitForge,
) -> Result<SubmitOutcome, ForgeError> {
    if !plan.blockers.is_empty() {
        return Err(ForgeError::Plan(format!(
            "the plan has blockers: {}",
            plan.blockers.join("; ")
        )));
    }
    let mut steps: Vec<SubmitStep> = plan
        .actions
        .iter()
        .map(|action| SubmitStep {
            action: action.clone(),
            status: SubmitStepStatus::Skipped,
            detail: None,
            pr: None,
        })
        .collect();
    let mut failed = false;

    // The batched push: every push action succeeds or fails as one
    // operation, exactly like `jj git push -b a -b b`.
    let push_indices: Vec<usize> = steps
        .iter()
        .enumerate()
        .filter(|(_, s)| matches!(s.action, SubmitAction::Push { .. }))
        .map(|(i, _)| i)
        .collect();
    if !push_indices.is_empty() {
        let names: Vec<String> = push_indices
            .iter()
            .map(|&i| match &steps[i].action {
                SubmitAction::Push { bookmark, .. } => bookmark.clone(),
                _ => unreachable!(),
            })
            .collect();
        match vcs.push_bookmarks(&names, &plan.remote) {
            Ok(summary) => {
                for &i in &push_indices {
                    steps[i].status = SubmitStepStatus::Done;
                    steps[i].detail = Some(summary.clone());
                }
            }
            Err(err) => {
                failed = true;
                for &i in &push_indices {
                    steps[i].status = SubmitStepStatus::Failed;
                    steps[i].detail = Some(err.to_string());
                }
            }
        }
    }

    if !failed {
        for step in &mut steps {
            match &step.action {
                SubmitAction::Push { .. } => {}
                SubmitAction::CreatePr {
                    bookmark,
                    base,
                    title,
                    body,
                } => match forge.create_pr(title, body, bookmark, base) {
                    Ok(pr) => {
                        step.status = SubmitStepStatus::Done;
                        step.detail = Some(format!("Opened #{} for {bookmark}", pr.number));
                        step.pr = Some(pr);
                    }
                    Err(err) => {
                        step.status = SubmitStepStatus::Failed;
                        step.detail = Some(err.to_string());
                        failed = true;
                        break;
                    }
                },
                SubmitAction::RetargetPr {
                    number,
                    bookmark,
                    from_base,
                    to_base,
                } => match forge.update_pr_base(*number, to_base) {
                    Ok(()) => {
                        step.status = SubmitStepStatus::Done;
                        step.detail = Some(format!(
                            "Retargeted #{number} ({bookmark}) from {from_base} to {to_base}"
                        ));
                    }
                    Err(err) => {
                        step.status = SubmitStepStatus::Failed;
                        step.detail = Some(err.to_string());
                        failed = true;
                        break;
                    }
                },
            }
        }
    }

    Ok(SubmitOutcome { steps, failed })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pr::{ChecksRollup, PrState, PrStateReport, ReviewDecision};
    use crate::remote::ForgeProvider;
    use jiji_core::snapshot::{BookmarkState, ConflictItem, ConflictKind};
    use std::cell::RefCell;

    fn node(
        id: &str,
        description: &str,
        kind: NodeKind,
        parents: &[&str],
        bookmarks: &[&str],
    ) -> GraphNode {
        GraphNode {
            id: id.into(),
            change_id: id.into(),
            commit_id: format!("c{id}"),
            description: description.into(),
            author: "Test <t@example.com>".into(),
            timestamp: "2026-07-01T12:00:00Z".into(),
            kind,
            parents: parents.iter().map(|p| p.to_string()).collect(),
            elided_parents: vec![],
            bookmarks: bookmarks.iter().map(|b| b.to_string()).collect(),
            is_empty: false,
            has_conflict: false,
            is_divergent: false,
        }
    }

    fn bookmark(name: &str, target: &str, sync: SyncState, is_trunk: bool) -> BookmarkState {
        BookmarkState {
            name: name.into(),
            target: target.into(),
            remote: (sync != SyncState::LocalOnly).then(|| "origin".into()),
            sync,
            is_trunk,
            is_local: true,
        }
    }

    fn snapshot(nodes: Vec<GraphNode>, bookmarks: Vec<BookmarkState>) -> RepoSnapshot {
        RepoSnapshot {
            repo_path: "/tmp/repo".into(),
            repo_name: "repo".into(),
            backend: "test".into(),
            trunk_bookmark: "main".into(),
            working_copy: nodes.first().map(|n| n.id.clone()).unwrap_or_default(),
            workspaces: vec![],
            workstreams: vec![],
            nodes,
            bookmarks,
            git_remotes: vec![],
            conflicts: vec![],
            operations: vec![],
            resolve_tool: None,
        }
    }

    fn open_pr(number: u64, head: &str, base: &str) -> PrSummary {
        PrSummary {
            number,
            title: format!("PR {number}"),
            url: format!("https://github.com/o/r/pull/{number}"),
            state: PrState::Open,
            is_draft: false,
            head_branch: head.into(),
            head_commit: "feedface".into(),
            head_owner: Some("o".into()),
            base_branch: base.into(),
            review: ReviewDecision::None,
            checks: ChecksRollup::None,
        }
    }

    fn pr_state(prs: Vec<PrSummary>, truncated: bool) -> RepoPrState {
        RepoPrState::new(PrStateReport { prs, truncated }, "o")
    }

    fn forge_repo() -> ForgeRepo {
        ForgeRepo {
            provider: ForgeProvider::GitHub,
            remote: "origin".into(),
            host: "github.com".into(),
            owner: "o".into(),
            name: "r".into(),
        }
    }

    /// trunk ── a1(auth) ── a2 ── b1(profile): two segments, the upper one
    /// two changes deep.
    fn stack_snapshot() -> RepoSnapshot {
        snapshot(
            vec![
                node("b1", "profile: avatars\n\nWith uploads.", NodeKind::WorkingCopy, &["a2"], &["profile"]),
                node("a2", "auth: sessions", NodeKind::Mutable, &["a1"], &[]),
                node(
                    "a1",
                    "auth: login flow\n\nThe form.\n\nCo-authored-by: X <x@e.c>",
                    NodeKind::Mutable,
                    &["m"],
                    &["auth"],
                ),
                node("m", "release", NodeKind::Immutable, &[], &["main"]),
            ],
            vec![
                bookmark("main", "m", SyncState::Synced, true),
                bookmark("auth", "a2", SyncState::LocalOnly, false),
                bookmark("profile", "b1", SyncState::Ahead, false),
            ],
        )
    }

    #[test]
    fn plans_pushes_creations_and_retargets_bottom_up() {
        let mut snap = stack_snapshot();
        // `auth` segments at a2 (its bookmark target), profile above it.
        snap.bookmarks[1].target = "a2".into();
        snap.nodes[1].bookmarks = vec!["auth".into()];
        snap.nodes[2].bookmarks = vec![];
        // profile already has a PR, parked on the wrong base.
        let prs = pr_state(vec![open_pr(7, "profile", "main")], false);

        let plan = plan_submit(&snap, &prs, &forge_repo(), "profile").unwrap();
        assert_eq!(plan.base_branch, "main");
        assert_eq!(plan.remote, "origin");
        assert!(plan.blockers.is_empty(), "{:?}", plan.blockers);

        let segs: Vec<(&str, &str, usize)> = plan
            .segments
            .iter()
            .map(|s| (s.bookmark.as_str(), s.base.as_str(), s.change_ids.len()))
            .collect();
        assert_eq!(segs, vec![("auth", "main", 2), ("profile", "auth", 1)]);
        assert_eq!(plan.segments[0].change_ids, vec!["a1", "a2"], "bottom-first");
        assert_eq!(plan.segments[0].title, "auth: login flow");
        assert_eq!(plan.segments[1].pr.as_ref().unwrap().number, 7);

        assert_eq!(
            plan.actions,
            vec![
                SubmitAction::Push { bookmark: "auth".into(), create: true },
                SubmitAction::Push { bookmark: "profile".into(), create: false },
                SubmitAction::CreatePr {
                    bookmark: "auth".into(),
                    base: "main".into(),
                    title: "auth: login flow".into(),
                    // Body keeps the prose, drops the trailer block.
                    body: "The form.".into(),
                },
                SubmitAction::RetargetPr {
                    number: 7,
                    bookmark: "profile".into(),
                    from_base: "main".into(),
                    to_base: "auth".into(),
                },
            ]
        );
    }

    #[test]
    fn consistent_stacks_plan_nothing() {
        let mut snap = stack_snapshot();
        snap.bookmarks[1].sync = SyncState::Synced;
        snap.bookmarks[1].remote = Some("origin".into());
        snap.bookmarks[2].sync = SyncState::Synced;
        let prs = pr_state(
            vec![open_pr(6, "auth", "main"), open_pr(7, "profile", "auth")],
            false,
        );

        let plan = plan_submit(&snap, &prs, &forge_repo(), "profile").unwrap();
        assert!(plan.actions.is_empty(), "{:?}", plan.actions);
        assert!(plan.blockers.is_empty());
        assert_eq!(plan.segments.len(), 2);
    }

    #[test]
    fn unpresentable_changes_block_only_pushing_segments() {
        let mut snap = stack_snapshot();
        snap.nodes[1].description = String::new(); // a2, in auth's segment
        snap.nodes[0].has_conflict = true; // b1, profile's segment
        let prs = pr_state(vec![], false);
        let plan = plan_submit(&snap, &prs, &forge_repo(), "profile").unwrap();
        assert_eq!(plan.blockers.len(), 2, "{:?}", plan.blockers);
        assert!(plan.blockers[0].contains("no description"), "{:?}", plan.blockers);
        assert!(plan.blockers[1].contains("has conflicts"), "{:?}", plan.blockers);

        // The same problems under an already-synced bookmark are already
        // on the remote — nothing new pushes, so nothing blocks.
        let mut synced = stack_snapshot();
        synced.nodes[2].description = String::new(); // a1 under synced auth
        synced.bookmarks[1].sync = SyncState::Synced;
        synced.bookmarks[1].remote = Some("origin".into());
        synced.bookmarks[2].sync = SyncState::Synced;
        let plan = plan_submit(&synced, &prs, &forge_repo(), "profile").unwrap();
        assert!(plan.blockers.is_empty(), "{:?}", plan.blockers);
    }

    #[test]
    fn empty_segments_skip_but_still_base_the_stack() {
        let mut snap = stack_snapshot();
        snap.nodes[1].is_empty = true; // a2
        snap.nodes[2].is_empty = true; // a1 — auth's whole segment empty
        let prs = pr_state(vec![], true);
        let plan = plan_submit(&snap, &prs, &forge_repo(), "profile").unwrap();

        // auth neither pushes nor gets a PR, but profile still bases on it.
        assert!(plan
            .actions
            .iter()
            .all(|a| !matches!(a, SubmitAction::Push { bookmark, .. } if bookmark == "auth")));
        assert_eq!(
            plan.actions
                .iter()
                .find_map(|a| match a {
                    SubmitAction::CreatePr { bookmark, base, .. } if bookmark == "profile" =>
                        Some(base.clone()),
                    _ => None,
                })
                .unwrap(),
            "auth"
        );
        assert!(plan.warnings.iter().any(|w| w.contains("empty")), "{:?}", plan.warnings);
        assert!(
            plan.warnings.iter().any(|w| w.contains("100 most recently")),
            "truncation warned: {:?}",
            plan.warnings
        );
    }

    #[test]
    fn conflicted_bookmarks_block_and_bad_targets_refuse() {
        let mut snap = stack_snapshot();
        snap.conflicts.push(ConflictItem {
            id: "bookmark-profile".into(),
            kind: ConflictKind::Bookmark,
            summary: "conflicted".into(),
            node_id: None,
            paths: vec![],
            more_paths: 0,
            targets: vec![],
            workspace: None,
        });
        let prs = pr_state(vec![], false);
        let plan = plan_submit(&snap, &prs, &forge_repo(), "profile").unwrap();
        assert!(
            plan.blockers.iter().any(|b| b.contains("is conflicted")),
            "{:?}",
            plan.blockers
        );

        let snap = stack_snapshot();
        let err = plan_submit(&snap, &prs, &forge_repo(), "nope").unwrap_err();
        assert_eq!(err.code(), "plan_failed");
        let err = plan_submit(&snap, &prs, &forge_repo(), "main").unwrap_err();
        assert!(err.to_string().contains("trunk"), "{err}");
        // A bookmark parked on immutable history has nothing to publish.
        let mut on_trunk = stack_snapshot();
        on_trunk.bookmarks.push(bookmark("old", "m", SyncState::LocalOnly, false));
        on_trunk.nodes[3].bookmarks.push("old".into());
        let err = plan_submit(&on_trunk, &prs, &forge_repo(), "old").unwrap_err();
        assert!(err.to_string().contains("immutable"), "{err}");
    }

    #[test]
    fn shared_change_bookmarks_collapse_into_one_segment() {
        let mut snap = stack_snapshot();
        // A second name on auth's change; the one GitHub knows wins.
        snap.bookmarks.push(bookmark("auth-alias", "a2", SyncState::LocalOnly, false));
        snap.bookmarks[1].target = "a2".into();
        snap.nodes[1].bookmarks = vec!["auth".into(), "auth-alias".into()];
        snap.nodes[2].bookmarks = vec![];
        let prs = pr_state(vec![open_pr(9, "auth-alias", "main")], false);

        let plan = plan_submit(&snap, &prs, &forge_repo(), "profile").unwrap();
        assert_eq!(plan.segments.len(), 2);
        assert_eq!(plan.segments[0].bookmark, "auth-alias");
        assert!(
            plan.warnings.iter().any(|w| w.contains("same change")),
            "{:?}",
            plan.warnings
        );
        // The upper segment bases on the published name.
        assert_eq!(plan.segments[1].base, "auth-alias");
    }

    #[derive(Default)]
    struct StubVcs {
        calls: RefCell<Vec<(Vec<String>, String)>>,
        fail: bool,
    }

    impl SubmitVcs for StubVcs {
        fn push_bookmarks(
            &self,
            bookmarks: &[String],
            remote: &str,
        ) -> Result<String, BackendError> {
            self.calls
                .borrow_mut()
                .push((bookmarks.to_vec(), remote.to_owned()));
            if self.fail {
                Err(BackendError::MutationFailed("remote moved".into()))
            } else {
                Ok(format!("Pushed {} bookmarks to {remote}", bookmarks.len()))
            }
        }
    }

    #[derive(Default)]
    struct StubForge {
        log: RefCell<Vec<String>>,
        fail_create: Option<String>,
    }

    impl SubmitForge for StubForge {
        fn create_pr(
            &self,
            title: &str,
            _body: &str,
            head: &str,
            base: &str,
        ) -> Result<PrSummary, ForgeError> {
            if self.fail_create.as_deref() == Some(head) {
                return Err(ForgeError::Api("boom".into()));
            }
            self.log.borrow_mut().push(format!("create {head}->{base}: {title}"));
            Ok(open_pr(42, head, base))
        }

        fn update_pr_base(&self, number: u64, base: &str) -> Result<(), ForgeError> {
            self.log.borrow_mut().push(format!("retarget #{number}->{base}"));
            Ok(())
        }
    }

    fn plan_with_all_action_kinds() -> SubmitPlan {
        let mut snap = stack_snapshot();
        snap.bookmarks[1].target = "a2".into();
        snap.nodes[1].bookmarks = vec!["auth".into()];
        snap.nodes[2].bookmarks = vec![];
        let prs = pr_state(vec![open_pr(7, "profile", "main")], false);
        plan_submit(&snap, &prs, &forge_repo(), "profile").unwrap()
    }

    #[test]
    fn execute_batches_pushes_then_walks_pr_actions() {
        let plan = plan_with_all_action_kinds();
        let vcs = StubVcs::default();
        let forge = StubForge::default();
        let outcome = execute_submit(&plan, &vcs, &forge).unwrap();

        assert!(!outcome.failed);
        assert!(outcome.steps.iter().all(|s| s.status == SubmitStepStatus::Done));
        // One batched push for both bookmarks, to the plan's remote.
        assert_eq!(
            vcs.calls.borrow().as_slice(),
            &[(vec!["auth".to_owned(), "profile".to_owned()], "origin".to_owned())]
        );
        assert_eq!(
            forge.log.borrow().as_slice(),
            &[
                "create auth->main: auth: login flow".to_owned(),
                "retarget #7->auth".to_owned(),
            ]
        );
        let created = outcome
            .steps
            .iter()
            .find(|s| matches!(s.action, SubmitAction::CreatePr { .. }))
            .unwrap();
        assert_eq!(created.pr.as_ref().unwrap().number, 42);
    }

    #[test]
    fn execute_stops_at_the_first_failure() {
        let plan = plan_with_all_action_kinds();
        // The push fails: nothing else runs.
        let vcs = StubVcs { fail: true, ..Default::default() };
        let forge = StubForge::default();
        let outcome = execute_submit(&plan, &vcs, &forge).unwrap();
        assert!(outcome.failed);
        for step in &outcome.steps {
            match step.action {
                SubmitAction::Push { .. } => {
                    assert_eq!(step.status, SubmitStepStatus::Failed);
                    assert!(step.detail.as_deref().unwrap().contains("remote moved"));
                }
                _ => assert_eq!(step.status, SubmitStepStatus::Skipped),
            }
        }
        assert!(forge.log.borrow().is_empty());

        // A PR creation fails: the push stays done, the retarget skips.
        let vcs = StubVcs::default();
        let forge = StubForge { fail_create: Some("auth".into()), ..Default::default() };
        let outcome = execute_submit(&plan, &vcs, &forge).unwrap();
        assert!(outcome.failed);
        let statuses: Vec<SubmitStepStatus> =
            outcome.steps.iter().map(|s| s.status).collect();
        assert_eq!(
            statuses,
            vec![
                SubmitStepStatus::Done,
                SubmitStepStatus::Done,
                SubmitStepStatus::Failed,
                SubmitStepStatus::Skipped,
            ]
        );

        // A plan with blockers refuses outright.
        let mut blocked = plan_with_all_action_kinds();
        blocked.blockers.push("something is wrong".into());
        let err = execute_submit(&blocked, &vcs, &forge).unwrap_err();
        assert_eq!(err.code(), "plan_failed");
    }

    #[test]
    fn titles_and_bodies_derive_from_the_bottom_change() {
        let (title, body) = derive_title_body(
            "feat: thing\n\nBody line.\n\nCo-authored-by: A <a@e.c>\nSigned-off-by: B <b@e.c>",
            "fallback",
        );
        assert_eq!(title, "feat: thing");
        assert_eq!(body, "Body line.");
        // A trailer mid-body survives; only the trailing block drops.
        let (_, body) = derive_title_body(
            "t\n\nReviewed-by: A <a@e.c>\n\nMore prose.",
            "fallback",
        );
        assert_eq!(body, "Reviewed-by: A <a@e.c>\n\nMore prose.");
        let (title, body) = derive_title_body("", "fallback");
        assert_eq!(title, "fallback");
        assert_eq!(body, "");
    }
}
