//! Ship directly to trunk: the spec's "Ship Now" workflow as the same
//! plan → confirm → execute shape as submit and land. Shipping moves the
//! trunk bookmark to a stack's head and pushes it — the PR-less way to
//! land work when repo policy allows pushing trunk.
//!
//! The shape mirrors the spec's ship flow exactly: the host fetches the
//! latest trunk *before* planning (so the plan previews against current
//! remote state and says whether a rebase is needed), and the push's
//! force-with-lease expectation is the trunk-moved-again guard — a remote
//! moved between the plan's fetch and the push refuses cleanly, nothing
//! is overwritten, and re-running Ship (fetch → re-plan → rebase → push)
//! *is* the retry path. Like submit, an already-shipped stack plans zero
//! actions instead of failing.
//!
//! Deliberately not asked: whether GitHub's branch protection allows the
//! push. There is no reliable read of push permission for the viewer
//! without admin scopes, and guessing would refuse pushes GitHub allows —
//! so GitHub's own refusal at push time is the policy answer, surfaced as
//! the failed step's story. The engine itself is forge-agnostic (fetch,
//! rebase, bookmark move, push are all jj-side); open-PR facts only feed
//! an honesty warning, so a plain-git-remote host could ship too.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use jiji_core::snapshot::{GraphNode, NodeKind, RepoSnapshot, SyncState};
use jiji_core::BackendError;

use crate::error::ForgeError;
use crate::pr::RepoPrState;
use crate::submit::SubmitStepStatus;

/// One shipping action, in execution order.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "camelCase")]
#[ts(export)]
pub enum ShipAction {
    /// The stack sits on an older trunk: rebase it (the root and all its
    /// descendants) onto the fetched trunk first, so what ships is what
    /// the user reviewed against current trunk.
    #[serde(rename_all = "camelCase")]
    RebaseOntoTrunk { root_change: String, moves: u32 },
    /// Point the trunk bookmark at the stack's head — the local half of
    /// shipping. Nothing is rewritten; the shipped changes become
    /// immutable trunk ancestry the moment the bookmark moves.
    #[serde(rename_all = "camelCase")]
    MoveTrunk { bookmark: String, to: String },
    /// Push the trunk bookmark. Force-with-lease inside: the remote is
    /// expected where the plan's fetch recorded it, so trunk moved by
    /// someone else refuses — nothing recorded — instead of overwriting.
    #[serde(rename_all = "camelCase")]
    PushTrunk { bookmark: String, remote: String },
    /// The working copy is inside the shipped stack and turns immutable
    /// with it: start a fresh empty change on the new trunk so the next
    /// edit has somewhere to land.
    #[serde(rename_all = "camelCase")]
    NewWorkingCopy { on: String },
}

/// What shipping a stack to trunk will do, derived before anything runs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ShipPlan {
    /// The stack head being shipped (a snapshot node id).
    pub head_change: String,
    /// First line of the head's description, for the panel.
    pub head_title: String,
    pub trunk_bookmark: String,
    pub remote: String,
    /// The shipped chain, bottom-up (first parents from trunk to head).
    pub change_ids: Vec<String>,
    /// Everything this run will do, in order. Empty with no blockers
    /// means the stack is already shipped — submit's idempotence.
    pub actions: Vec<ShipAction>,
    /// Problems that stop the plan from running at all.
    pub blockers: Vec<String>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ShipStep {
    pub action: ShipAction,
    pub status: SubmitStepStatus,
    /// Plain-language result: what happened, or why it was skipped.
    pub detail: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ShipOutcome {
    pub steps: Vec<ShipStep>,
    pub failed: bool,
}

/// The jj side of executing a ship plan, host-implemented over
/// `jiji-core`'s backend (a stub in tests). Every mutating method also
/// republishes the snapshot; `snapshot` answers the latest one so steps
/// can skip politely when a re-run finds their work already done.
pub trait ShipVcs {
    fn snapshot(&self) -> Result<RepoSnapshot, BackendError>;
    /// `jj rebase -s <root> -d <trunk>` — the host resolves the trunk's
    /// change from its own refreshed snapshot.
    fn rebase_onto_trunk(&self, root_change: &str) -> Result<String, BackendError>;
    fn move_bookmark(&self, name: &str, to_change: &str) -> Result<String, BackendError>;
    fn push_bookmarks(&self, bookmarks: &[String], remote: &str) -> Result<String, BackendError>;
    /// `jj new <rev>` — the respawn for a shipped working copy.
    fn new_change(&self, parent_change: &str) -> Result<String, BackendError>;
}

/// Build the ship plan for the stack under `head_id` (a snapshot node id —
/// any mutable change, bookmarked or not; shipping needs no PR and no
/// bookmark other than trunk's own).
///
/// The caller fetches before planning, so trunk state here is as fresh as
/// it gets: rebase-needed falls out of comparing the stack's immutable
/// base against the trunk bookmark's target, and trunk sync states that
/// survived the fetch (diverged, behind) are real problems, not staleness.
/// `prs` only feeds the open-PR honesty warning — pushing a PR's commits
/// to trunk makes GitHub mark it merged, and the user should hear that
/// before confirming, not after.
pub fn plan_ship(
    snapshot: &RepoSnapshot,
    prs: Option<&RepoPrState>,
    remote: &str,
    head_id: &str,
) -> Result<ShipPlan, ForgeError> {
    let node_by_id: HashMap<&str, &GraphNode> =
        snapshot.nodes.iter().map(|n| (n.id.as_str(), n)).collect();
    let head = node_by_id.get(head_id).copied().ok_or_else(|| {
        ForgeError::Ship(format!(
            "no change {head_id} in this snapshot — refresh and pick again"
        ))
    })?;
    if head.kind == NodeKind::Immutable {
        return Err(ForgeError::Ship(format!(
            "{head_id} is already immutable history — there is nothing to ship"
        )));
    }
    let trunk = snapshot
        .bookmarks
        .iter()
        .find(|b| b.is_trunk)
        .ok_or_else(|| {
            ForgeError::Ship(
                "this repository has no trunk bookmark — shipping needs somewhere to land".into(),
            )
        })?;

    let mut blockers: Vec<String> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();

    // The trunk bookmark itself must be movable and honestly positioned.
    if !trunk.is_local {
        blockers.push(format!(
            "\u{201c}{}\u{201d} exists only on the remote here — create a local \
             \u{201c}{}\u{201d} bookmark first",
            trunk.name, trunk.name
        ));
    }
    if snapshot
        .conflicts
        .iter()
        .any(|c| c.id == format!("bookmark-{}", trunk.name))
    {
        blockers.push(format!(
            "bookmark \u{201c}{}\u{201d} is conflicted; repoint it before shipping",
            trunk.name
        ));
    }
    match trunk.sync {
        SyncState::Behind => blockers.push(format!(
            "\u{201c}{}\u{201d} is behind its remote — shipping now would push a trunk \
             missing those commits",
            trunk.name
        )),
        SyncState::Diverged => blockers.push(format!(
            "\u{201c}{}\u{201d} and its remote have diverged — reconcile them before \
             shipping",
            trunk.name
        )),
        SyncState::LocalOnly => warnings.push(format!(
            "{remote} has no \u{201c}{}\u{201d} yet — this push creates it",
            trunk.name
        )),
        SyncState::Ahead => warnings.push(format!(
            "\u{201c}{}\u{201d} is already ahead of its remote — the push includes that \
             work too",
            trunk.name
        )),
        SyncState::Synced => {}
    }

    // Walk first parents from the head down to the immutable base — the
    // chain that ships. Side branches reachable through merge changes ride
    // along on the push (jj's own range checks still guard them); the
    // plan's counts and per-change checks cover the first-parent chain,
    // like submit and land.
    let mut chain: Vec<&GraphNode> = Vec::new();
    let mut base: Option<&GraphNode> = None;
    let mut cursor = head;
    loop {
        chain.push(cursor);
        match cursor
            .parents
            .first()
            .and_then(|id| node_by_id.get(id.as_str()).copied())
        {
            None => break,
            Some(parent) if parent.kind == NodeKind::Immutable => {
                base = Some(parent);
                break;
            }
            Some(parent) => cursor = parent,
        }
    }
    chain.reverse(); // bottom-up, ending at the head
    if base.is_none() {
        blockers.push(
            "the stack's base is outside this snapshot's view — refresh and try again".into(),
        );
    }

    // The same presentability checks the push itself enforces, surfaced at
    // plan time so the panel can say so.
    for node in &chain {
        let change = &node.change_id;
        if node.description.is_empty() {
            blockers.push(format!("{change} has no description; describe it first"));
        }
        if node.has_conflict {
            blockers.push(format!("{change} has conflicts; resolve them first"));
        }
        if node.is_divergent {
            blockers.push(format!("{change} is divergent; resolve the divergence first"));
        }
    }
    if head.is_empty {
        warnings.push(
            "the head change has no file changes — shipping records an empty commit on \
             trunk"
                .into(),
        );
    }

    // Open PRs whose branches sit inside the shipped chain: pushing the
    // same commits to trunk makes GitHub mark them merged.
    if let Some(prs) = prs {
        for node in &chain {
            for name in &node.bookmarks {
                if let Some(pr) = prs.by_branch.get(name.as_str()) {
                    warnings.push(format!(
                        "#{} ({name}) is open for this work — pushing the same commits \
                         to \u{201c}{}\u{201d} marks it merged on GitHub",
                        pr.number, trunk.name
                    ));
                }
            }
        }
    }

    let mut actions: Vec<ShipAction> = Vec::new();
    let already_at_head = trunk.target == head.id;
    // On trunk already when the chain grows out of the trunk bookmark's
    // target — or contains it (a partially-shipped stack whose snapshot
    // has not caught up yet reads that way).
    let on_trunk = base.is_some_and(|b| b.id == trunk.target)
        || chain.iter().any(|n| n.id == trunk.target);
    if base.is_some() && !on_trunk {
        actions.push(ShipAction::RebaseOntoTrunk {
            root_change: chain[0].id.clone(),
            moves: chain.len() as u32,
        });
    }
    if !already_at_head {
        actions.push(ShipAction::MoveTrunk {
            bookmark: trunk.name.clone(),
            to: head.id.clone(),
        });
    }
    if !already_at_head || trunk.sync != SyncState::Synced {
        actions.push(ShipAction::PushTrunk {
            bookmark: trunk.name.clone(),
            remote: remote.to_owned(),
        });
    }
    if chain.iter().any(|n| n.id == snapshot.working_copy) {
        actions.push(ShipAction::NewWorkingCopy {
            on: head.id.clone(),
        });
    }

    Ok(ShipPlan {
        head_change: head.id.clone(),
        head_title: head
            .description
            .lines()
            .next()
            .unwrap_or_default()
            .to_owned(),
        trunk_bookmark: trunk.name.clone(),
        remote: remote.to_owned(),
        change_ids: chain.iter().map(|n| n.id.clone()).collect(),
        actions,
        blockers,
        warnings,
    })
}

/// Run a confirmed ship plan. The first failure stops execution — later
/// steps report as skipped, and a lease-refused push names the retry path
/// (fetch, review the fresh plan, ship again). Steps skip politely when a
/// re-run finds their work already done.
pub fn execute_ship(plan: &ShipPlan, vcs: &dyn ShipVcs) -> Result<ShipOutcome, ForgeError> {
    if !plan.blockers.is_empty() {
        return Err(ForgeError::Ship(format!(
            "the plan has blockers: {}",
            plan.blockers.join("; ")
        )));
    }
    let mut steps: Vec<ShipStep> = plan
        .actions
        .iter()
        .map(|action| ShipStep {
            action: action.clone(),
            status: SubmitStepStatus::Skipped,
            detail: None,
        })
        .collect();
    let mut failed = false;
    for step in &mut steps {
        match run_ship_step(&step.action, vcs) {
            Ok(detail) => {
                step.status = SubmitStepStatus::Done;
                step.detail = Some(detail);
            }
            Err(message) => {
                step.status = SubmitStepStatus::Failed;
                step.detail = Some(message);
                failed = true;
                break;
            }
        }
    }
    Ok(ShipOutcome { steps, failed })
}

fn run_ship_step(action: &ShipAction, vcs: &dyn ShipVcs) -> Result<String, String> {
    match action {
        ShipAction::RebaseOntoTrunk { root_change, .. } => vcs
            .rebase_onto_trunk(root_change)
            .map_err(|err| err.to_string()),
        ShipAction::MoveTrunk { bookmark, to } => {
            let snap = vcs.snapshot().map_err(|err| err.to_string())?;
            if snap
                .bookmarks
                .iter()
                .any(|b| b.name == *bookmark && b.is_local && b.target == *to)
            {
                return Ok(format!(
                    "\u{201c}{bookmark}\u{201d} already points at {to}"
                ));
            }
            vcs.move_bookmark(bookmark, to).map_err(|err| err.to_string())
        }
        ShipAction::PushTrunk { bookmark, remote } => vcs
            .push_bookmarks(std::slice::from_ref(bookmark), remote)
            .map_err(|err| err.to_string()),
        ShipAction::NewWorkingCopy { on } => {
            vcs.new_change(on).map_err(|err| err.to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pr::{PrState, PrStateReport, PrSummary};
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
            working_copy: String::new(),
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

    /// trunk ─ b1 ─ b2(head); trunk bookmark on trunk, synced.
    fn stack_on_trunk() -> RepoSnapshot {
        snapshot(
            vec![
                node("b2", "feat: top", NodeKind::Mutable, &["b1"], &[]),
                node("b1", "feat: bottom", NodeKind::Mutable, &["t0"], &[]),
                node("t0", "trunk tip", NodeKind::Immutable, &[], &["main"]),
            ],
            vec![bookmark("main", "t0", SyncState::Synced, true)],
        )
    }

    fn prs_with(open: Vec<PrSummary>) -> RepoPrState {
        RepoPrState::new(
            PrStateReport {
                prs: open,
                truncated: false,
            },
            "o",
        )
    }

    fn open_pr(number: u64, head: &str) -> PrSummary {
        PrSummary {
            number,
            title: format!("PR {number}"),
            url: format!("https://github.com/o/r/pull/{number}"),
            state: PrState::Open,
            is_draft: false,
            head_branch: head.into(),
            head_commit: "feedface".into(),
            head_owner: Some("o".into()),
            base_branch: "main".into(),
            body: None,
            review: crate::pr::ReviewDecision::None,
            checks: crate::pr::ChecksRollup::None,
        }
    }

    #[test]
    fn plans_move_and_push_for_a_stack_on_current_trunk() {
        let plan = plan_ship(&stack_on_trunk(), None, "origin", "b2").unwrap();
        assert!(plan.blockers.is_empty(), "{:?}", plan.blockers);
        assert_eq!(plan.change_ids, vec!["b1", "b2"]);
        assert_eq!(
            plan.actions,
            vec![
                ShipAction::MoveTrunk {
                    bookmark: "main".into(),
                    to: "b2".into()
                },
                ShipAction::PushTrunk {
                    bookmark: "main".into(),
                    remote: "origin".into()
                },
            ]
        );
        assert_eq!(plan.head_title, "feat: top");
    }

    #[test]
    fn plans_a_rebase_when_the_stack_sits_on_an_older_trunk() {
        // t1 is the fetched trunk tip; the stack still sits on t0.
        let snap = snapshot(
            vec![
                node("b2", "feat: top", NodeKind::Mutable, &["b1"], &[]),
                node("b1", "feat: bottom", NodeKind::Mutable, &["t0"], &[]),
                node("t1", "newer trunk", NodeKind::Immutable, &["t0"], &["main"]),
                node("t0", "old trunk", NodeKind::Immutable, &[], &[]),
            ],
            vec![bookmark("main", "t1", SyncState::Synced, true)],
        );
        let plan = plan_ship(&snap, None, "origin", "b2").unwrap();
        assert!(plan.blockers.is_empty(), "{:?}", plan.blockers);
        assert_eq!(
            plan.actions[0],
            ShipAction::RebaseOntoTrunk {
                root_change: "b1".into(),
                moves: 2
            }
        );
        assert!(matches!(plan.actions[1], ShipAction::MoveTrunk { .. }));
        assert!(matches!(plan.actions[2], ShipAction::PushTrunk { .. }));
    }

    #[test]
    fn an_already_shipped_stack_plans_nothing() {
        // Trunk bookmark already at the head and synced: submit's
        // idempotence — zero actions, no blockers.
        let snap = snapshot(
            vec![
                node("b1", "feat: shipped", NodeKind::Mutable, &["t0"], &["main"]),
                node("t0", "old trunk", NodeKind::Immutable, &[], &[]),
            ],
            vec![bookmark("main", "b1", SyncState::Synced, true)],
        );
        let plan = plan_ship(&snap, None, "origin", "b1").unwrap();
        assert!(plan.blockers.is_empty(), "{:?}", plan.blockers);
        assert!(plan.actions.is_empty(), "{:?}", plan.actions);
    }

    #[test]
    fn a_failed_push_retries_as_push_only() {
        // Local main already moved to the head (the failed run's move
        // step), remote still behind: the re-plan is just the push.
        let snap = snapshot(
            vec![
                node("b1", "feat: retry", NodeKind::Mutable, &["t0"], &["main"]),
                node("t0", "old trunk", NodeKind::Immutable, &[], &[]),
            ],
            vec![bookmark("main", "b1", SyncState::Ahead, true)],
        );
        let plan = plan_ship(&snap, None, "origin", "b1").unwrap();
        assert!(plan.blockers.is_empty(), "{:?}", plan.blockers);
        assert_eq!(
            plan.actions,
            vec![ShipAction::PushTrunk {
                bookmark: "main".into(),
                remote: "origin".into()
            }]
        );
        assert!(
            plan.warnings.iter().any(|w| w.contains("already ahead")),
            "{:?}",
            plan.warnings
        );
    }

    #[test]
    fn unpresentable_changes_and_a_broken_trunk_block() {
        let mut snap = stack_on_trunk();
        snap.nodes[0].description = String::new();
        snap.nodes[1].has_conflict = true;
        snap.bookmarks[0].sync = SyncState::Diverged;
        let plan = plan_ship(&snap, None, "origin", "b2").unwrap();
        assert!(
            plan.blockers.iter().any(|b| b.contains("diverged")),
            "{:?}",
            plan.blockers
        );
        assert!(
            plan.blockers.iter().any(|b| b.contains("no description")),
            "{:?}",
            plan.blockers
        );
        assert!(
            plan.blockers.iter().any(|b| b.contains("has conflicts")),
            "{:?}",
            plan.blockers
        );
    }

    #[test]
    fn divergent_changes_and_remote_only_trunk_block() {
        let mut snap = stack_on_trunk();
        snap.nodes[1].is_divergent = true;
        snap.bookmarks[0].is_local = false;
        let plan = plan_ship(&snap, None, "origin", "b2").unwrap();
        assert!(
            plan.blockers.iter().any(|b| b.contains("divergent")),
            "{:?}",
            plan.blockers
        );
        assert!(
            plan.blockers.iter().any(|b| b.contains("only on the remote")),
            "{:?}",
            plan.blockers
        );
    }

    #[test]
    fn a_conflicted_trunk_bookmark_blocks() {
        let mut snap = stack_on_trunk();
        snap.conflicts.push(ConflictItem {
            id: "bookmark-main".into(),
            kind: ConflictKind::Bookmark,
            summary: "main is conflicted".into(),
            node_id: None,
            paths: vec![],
            more_paths: 0,
            targets: vec![],
            workspace: None,
        });
        let plan = plan_ship(&snap, None, "origin", "b2").unwrap();
        assert!(
            plan.blockers.iter().any(|b| b.contains("conflicted")),
            "{:?}",
            plan.blockers
        );
    }

    #[test]
    fn refuses_immutable_and_unknown_heads() {
        let snap = stack_on_trunk();
        assert!(plan_ship(&snap, None, "origin", "t0").is_err());
        assert!(plan_ship(&snap, None, "origin", "zzzz").is_err());
    }

    #[test]
    fn a_shipped_working_copy_respawns_and_open_prs_warn() {
        let mut snap = stack_on_trunk();
        snap.working_copy = "b2".into();
        snap.nodes[0].bookmarks = vec!["feature".into()];
        snap.bookmarks.push(bookmark(
            "feature",
            "b2",
            SyncState::Synced,
            false,
        ));
        let prs = prs_with(vec![open_pr(7, "feature")]);
        let plan = plan_ship(&snap, Some(&prs), "origin", "b2").unwrap();
        assert_eq!(
            plan.actions.last(),
            Some(&ShipAction::NewWorkingCopy { on: "b2".into() })
        );
        assert!(
            plan.warnings
                .iter()
                .any(|w| w.contains("#7") && w.contains("marks it merged")),
            "{:?}",
            plan.warnings
        );
    }

    #[test]
    fn local_only_trunk_warns_and_still_pushes() {
        let mut snap = stack_on_trunk();
        snap.bookmarks[0] = bookmark("main", "t0", SyncState::LocalOnly, true);
        let plan = plan_ship(&snap, None, "origin", "b2").unwrap();
        assert!(plan.blockers.is_empty(), "{:?}", plan.blockers);
        assert!(
            plan.warnings.iter().any(|w| w.contains("this push creates it")),
            "{:?}",
            plan.warnings
        );
        assert!(plan
            .actions
            .iter()
            .any(|a| matches!(a, ShipAction::PushTrunk { .. })));
    }

    // ── execute ──

    /// Scripted vcs: answers each call from a log, optionally failing the
    /// push like a lease refusal.
    struct Vcs {
        snap: RepoSnapshot,
        push_fails: bool,
        calls: RefCell<Vec<String>>,
    }

    impl ShipVcs for Vcs {
        fn snapshot(&self) -> Result<RepoSnapshot, BackendError> {
            Ok(self.snap.clone())
        }
        fn rebase_onto_trunk(&self, root_change: &str) -> Result<String, BackendError> {
            self.calls.borrow_mut().push(format!("rebase:{root_change}"));
            Ok(format!("Rebased {root_change} onto trunk"))
        }
        fn move_bookmark(&self, name: &str, to_change: &str) -> Result<String, BackendError> {
            self.calls
                .borrow_mut()
                .push(format!("move:{name}:{to_change}"));
            Ok(format!("Moved {name} to {to_change}"))
        }
        fn push_bookmarks(
            &self,
            bookmarks: &[String],
            remote: &str,
        ) -> Result<String, BackendError> {
            self.calls
                .borrow_mut()
                .push(format!("push:{}:{remote}", bookmarks.join(",")));
            if self.push_fails {
                Err(BackendError::MutationFailed(
                    "Refusing to push: origin's main moved since the last fetch".into(),
                ))
            } else {
                Ok("Pushed main to origin".into())
            }
        }
        fn new_change(&self, parent_change: &str) -> Result<String, BackendError> {
            self.calls.borrow_mut().push(format!("new:{parent_change}"));
            Ok(format!("Started a new change on {parent_change}"))
        }
    }

    fn full_plan() -> ShipPlan {
        ShipPlan {
            head_change: "b2".into(),
            head_title: "feat: top".into(),
            trunk_bookmark: "main".into(),
            remote: "origin".into(),
            change_ids: vec!["b1".into(), "b2".into()],
            actions: vec![
                ShipAction::RebaseOntoTrunk {
                    root_change: "b1".into(),
                    moves: 2,
                },
                ShipAction::MoveTrunk {
                    bookmark: "main".into(),
                    to: "b2".into(),
                },
                ShipAction::PushTrunk {
                    bookmark: "main".into(),
                    remote: "origin".into(),
                },
                ShipAction::NewWorkingCopy { on: "b2".into() },
            ],
            blockers: vec![],
            warnings: vec![],
        }
    }

    #[test]
    fn executes_in_order_and_reports_each_step() {
        let vcs = Vcs {
            snap: stack_on_trunk(),
            push_fails: false,
            calls: RefCell::new(vec![]),
        };
        let outcome = execute_ship(&full_plan(), &vcs).unwrap();
        assert!(!outcome.failed);
        assert!(outcome
            .steps
            .iter()
            .all(|s| s.status == SubmitStepStatus::Done));
        assert_eq!(
            *vcs.calls.borrow(),
            vec!["rebase:b1", "move:main:b2", "push:main:origin", "new:b2"]
        );
    }

    #[test]
    fn a_refused_push_fails_the_step_and_skips_the_rest() {
        let vcs = Vcs {
            snap: stack_on_trunk(),
            push_fails: true,
            calls: RefCell::new(vec![]),
        };
        let outcome = execute_ship(&full_plan(), &vcs).unwrap();
        assert!(outcome.failed);
        assert_eq!(outcome.steps[2].status, SubmitStepStatus::Failed);
        assert!(outcome.steps[2]
            .detail
            .as_deref()
            .unwrap()
            .contains("moved since the last fetch"));
        assert_eq!(outcome.steps[3].status, SubmitStepStatus::Skipped);
    }

    #[test]
    fn move_trunk_skips_politely_when_already_done() {
        // The vcs's snapshot already has main at the head — a re-run after
        // a push failure, say.
        let mut snap = stack_on_trunk();
        snap.bookmarks[0].target = "b2".into();
        let vcs = Vcs {
            snap,
            push_fails: false,
            calls: RefCell::new(vec![]),
        };
        let plan = ShipPlan {
            actions: vec![
                ShipAction::MoveTrunk {
                    bookmark: "main".into(),
                    to: "b2".into(),
                },
                ShipAction::PushTrunk {
                    bookmark: "main".into(),
                    remote: "origin".into(),
                },
            ],
            ..full_plan()
        };
        let outcome = execute_ship(&plan, &vcs).unwrap();
        assert!(!outcome.failed);
        assert!(outcome.steps[0]
            .detail
            .as_deref()
            .unwrap()
            .contains("already points at"));
        assert_eq!(*vcs.calls.borrow(), vec!["push:main:origin"]);
    }

    #[test]
    fn blocked_plans_refuse_to_execute() {
        let vcs = Vcs {
            snap: stack_on_trunk(),
            push_fails: false,
            calls: RefCell::new(vec![]),
        };
        let plan = ShipPlan {
            blockers: vec!["nope".into()],
            ..full_plan()
        };
        assert!(execute_ship(&plan, &vcs).is_err());
        assert!(vcs.calls.borrow().is_empty());
    }
}
