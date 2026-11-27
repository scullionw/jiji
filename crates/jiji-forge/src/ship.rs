//! Ship directly to trunk, in the same plan → confirm → execute shape as
//! submit and land. Shipping moves the trunk bookmark to a stack's head and
//! pushes it — the PR-less way to land work when repo policy allows pushing
//! trunk.
//!
//! The host fetches the latest trunk *before* planning (so the plan
//! previews against current remote state and says whether a rebase is
//! needed), and the push's force-with-lease expectation is the
//! trunk-moved-again guard — a remote moved between the plan's fetch and
//! the push refuses cleanly, nothing is overwritten, and re-running Ship
//! (fetch → re-plan → rebase → push) *is* the retry path. Like submit, an
//! already-shipped stack plans zero actions instead of failing.
//!
//! Deliberately not asked: whether GitHub's branch protection allows the
//! push. There is no reliable read of push permission for the viewer
//! without admin scopes, and guessing would refuse pushes GitHub allows —
//! so GitHub's own refusal at push time is the policy answer, surfaced as
//! the failed step's story. Shipping itself is forge-agnostic (fetch,
//! rebase, bookmark move, push are all jj-side), so a plain-git-remote
//! host can ship too.
//!
//! Where the shipped work has open pull requests, the ship finishes them
//! as well. GitHub marks a pull request merged when its head turns up
//! inside its base branch, and only then: pushed to trunk, a stack's
//! bottom pull request is merged and the ones above it, each based on the
//! branch below, stay open, and afterwards GitHub refuses to point them
//! at trunk ("no new commits"; all of it seen on the sandbox). So each
//! is pointed at trunk before the push, with its branch at the commit that
//! ships, and the push merges them all. A push that is refused puts the
//! bases back. Pull requests that sit on the shipped work without being
//! part of it (the next one up, or someone else's) are pointed at trunk
//! after the push and stay open.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use jiji_core::snapshot::{GraphNode, RepoSnapshot, SyncState};
use jiji_core::BackendError;

use crate::error::ForgeError;
use crate::land::same_commit;
use crate::pr::{PrState, PrSummary, RepoPrState};
use crate::submit::SubmitStepStatus;

/// One shipping action, in execution order.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum ShipAction {
    /// The stack sits on an older trunk: rebase it (the root and all its
    /// descendants) onto the fetched trunk first, so what ships is what
    /// the user reviewed against current trunk.
    #[serde(rename_all = "camelCase")]
    RebaseOntoTrunk { root_change: String, moves: u32 },
    /// Push the branches of the shipped pull requests, so that each one's
    /// head on GitHub is the commit that ships: the rebase above gave the
    /// changes new commits, or a bookmark was ahead of its branch. A pull
    /// request whose head is some other commit is not marked merged by
    /// the push to trunk.
    #[serde(rename_all = "camelCase")]
    PushBranches { bookmarks: Vec<String>, remote: String },
    /// Take the shipped pull requests out of GitHub stack `stack`. GitHub
    /// refuses to retarget a pull request inside one of its stacks, and
    /// does not finish the stack by itself when trunk is pushed: the
    /// bottom pull request is marked merged and the rest stay open, still
    /// in the stack (seen on the sandbox). Planned only when every open
    /// pull request of the stack ships, so nothing is left out of a stack
    /// it was in. `members`, bottom first, is what the stack is linked
    /// from again if the push is refused.
    #[serde(rename_all = "camelCase")]
    Unstack { stack: u64, members: Vec<u64> },
    /// Point a shipped pull request at trunk ahead of the push, so that
    /// the push merges it. `from_base` is where it points again if the
    /// push is refused.
    #[serde(rename_all = "camelCase")]
    RetargetPr {
        number: u64,
        bookmark: String,
        from_base: String,
        to_base: String,
    },
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
    /// Wait for GitHub to mark the shipped pull requests merged (it does
    /// so a few seconds after the push), and say of each what it is.
    #[serde(rename_all = "camelCase")]
    ConfirmMerged { numbers: Vec<u64> },
    /// After the push: point at trunk a pull request that sits on the
    /// shipped work and is not part of it, the next one up in the user's
    /// own stack or someone else's (`theirs`, with `author` when GitHub
    /// names one). Its base branch has nothing left that trunk lacks. It
    /// stays open.
    #[serde(rename_all = "camelCase")]
    RetargetAbove {
        number: u64,
        #[serde(default)]
        author: Option<String>,
        #[serde(default)]
        theirs: bool,
        branch: String,
        from_base: String,
        to_base: String,
    },
    /// The working copy is inside the shipped stack and turns immutable
    /// with it: start a fresh empty change on the new trunk so the next
    /// edit has somewhere to land.
    #[serde(rename_all = "camelCase")]
    NewWorkingCopy { on: String },
}

/// What shipping a stack to trunk will do, derived before anything runs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShipStep {
    pub action: ShipAction,
    pub status: SubmitStepStatus,
    /// Plain-language result: what happened, or why it was skipped.
    pub detail: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
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

/// The GitHub side of a ship that has pull requests to finish. A ship of
/// work with no pull requests never asks it anything.
pub trait ShipForge {
    fn update_pr_base(&self, number: u64, base: &str) -> Result<(), ForgeError>;
    /// Take the open pull requests out of a GitHub stack; answers the
    /// ones GitHub kept in it.
    fn unstack(&self, stack: u64) -> Result<Vec<u64>, ForgeError>;
    /// Link pull requests, bottom first, as a GitHub stack; answers its
    /// number.
    fn create_stack(&self, pull_requests: &[u64]) -> Result<u64, ForgeError>;
    /// Where each of these pull requests stands, after giving GitHub a
    /// short while to mark as merged the ones whose commits a push just
    /// put on their base branch.
    fn await_merged(&self, numbers: &[u64]) -> Result<Vec<(u64, PrState)>, ForgeError>;
    /// The commit `branch` is at on GitHub right now. Asked after a push
    /// of trunk reported an error, to see whether it arrived all the
    /// same before anything is put back.
    fn branch_head(&self, branch: &str) -> Result<String, ForgeError>;
}

impl ShipForge for crate::submit::RepoForge<'_> {
    fn update_pr_base(&self, number: u64, base: &str) -> Result<(), ForgeError> {
        crate::submit::SubmitForge::update_pr_base(self, number, base)
    }

    fn unstack(&self, stack: u64) -> Result<Vec<u64>, ForgeError> {
        crate::submit::SubmitForge::unstack(self, stack)
    }

    fn create_stack(&self, pull_requests: &[u64]) -> Result<u64, ForgeError> {
        crate::submit::SubmitForge::create_stack(self, pull_requests)
    }

    fn await_merged(&self, numbers: &[u64]) -> Result<Vec<(u64, PrState)>, ForgeError> {
        // GitHub marks a pull request merged a few seconds after the push
        // that put its head inside its base (six, when it was timed on
        // the sandbox). Looked at every three seconds, for half a minute.
        const LOOKS: u32 = 10;
        let mut states: Vec<(u64, PrState)> = Vec::new();
        for look in 0..LOOKS {
            states.clear();
            for number in numbers {
                let pr = self
                    .client
                    .pr_by_number(&self.repo.owner, &self.repo.name, *number)?;
                states.push((*number, pr.state));
            }
            if states.iter().all(|(_, state)| *state != PrState::Open) || look + 1 == LOOKS {
                break;
            }
            std::thread::sleep(std::time::Duration::from_secs(3));
        }
        Ok(states)
    }

    fn branch_head(&self, branch: &str) -> Result<String, ForgeError> {
        self.client
            .branch_head(&self.repo.owner, &self.repo.name, branch)
    }
}

/// The note a host adds to a ship plan made without GitHub (no token, or
/// a remote that is not GitHub). The plan then finishes no pull request,
/// and GitHub goes by its own rule all the same.
pub const SHIPPED_WITHOUT_GITHUB: &str = "GitHub was not asked, so this plan knows of no pull \
     requests. GitHub still marks merged one whose commits this push puts on its base (as a \
     rule the one based on trunk); pull requests above it stay open, as they are";

/// Build the ship plan for the stack under `head_id` (a snapshot node id —
/// any mutable change, bookmarked or not; shipping needs no PR and no
/// bookmark other than trunk's own).
///
/// The caller fetches before planning, so trunk state here is as fresh as
/// it gets: rebase-needed falls out of comparing the stack's immutable
/// base against the trunk bookmark's target, and trunk sync states that
/// survived the fetch (diverged, behind) are real problems, not staleness.
/// With `prs`, the plan finishes the pull requests of the shipped work
/// too, each by number: its branch pushed where the commit that ships is
/// not what GitHub has, its base pointed at trunk ahead of the push, and
/// the merge confirmed after it. Without (no token, or a failed read) the
/// stack ships as it always did and its pull requests are left as they
/// are.
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
    if head.is_settled() {
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
        // A remote-only trunk already collected the create-a-local-bookmark
        // blocker above; there is no local position to compare.
        SyncState::RemoteOnly => {}
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
            Some(parent) if parent.is_settled() => {
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
        // As in a publish: copies only someone else's branch keeps are
        // not the user's divergence to resolve.
        if node.diverged_here() {
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

    let already_at_head = trunk.target == head.id;
    // On trunk already when the chain grows out of the trunk bookmark's
    // target — or contains it (a partially-shipped stack whose snapshot
    // has not caught up yet reads that way).
    let on_trunk = base.is_some_and(|b| b.id == trunk.target)
        || chain.iter().any(|n| n.id == trunk.target);
    let rebases = base.is_some() && !on_trunk;
    let pushes_trunk = !already_at_head || trunk.sync != SyncState::Synced;

    // The pull requests of the shipped work: open, each for a local
    // bookmark on one of the shipped changes. Bottom first.
    let mut branch_pushes: Vec<String> = Vec::new();
    let mut unstacks: Vec<ShipAction> = Vec::new();
    let mut retargets: Vec<ShipAction> = Vec::new();
    let mut confirm: Vec<u64> = Vec::new();
    let mut above: Vec<ShipAction> = Vec::new();
    if let Some(prs) = prs.filter(|_| pushes_trunk) {
        let mut shipped: Vec<&PrSummary> = Vec::new();
        // Every branch of the shipped work, whether or not it has a pull
        // request of its own: someone else's can be based on any of them.
        let mut shipped_branches: Vec<&str> = Vec::new();
        for node in &chain {
            for name in &node.bookmarks {
                let Some(bookmark) = snapshot
                    .bookmarks
                    .iter()
                    .find(|b| b.name == *name && b.is_local && !b.is_trunk)
                else {
                    continue;
                };
                shipped_branches.push(name.as_str());
                let Some(pr) = prs.by_branch.get(name.as_str()) else {
                    continue;
                };
                // GitHub marks a pull request merged when its head is
                // inside its base. The head has to be the commit that
                // ships: the branch is pushed first where the rebase is
                // about to give the change another commit, or the
                // bookmark is ahead of its branch.
                if rebases || bookmark.sync != SyncState::Synced {
                    branch_pushes.push(name.clone());
                } else if !same_commit(&pr.head_commit, &node.commit_id) {
                    blockers.push(format!(
                        "#{}'s branch on GitHub is at {}, not at {} where \u{201c}{name}\u{201d} \
                         is here: something pushed to it since the last fetch. Shipped as it \
                         is here, GitHub would not mark #{} merged \u{2014} fetch, and look at \
                         what arrived",
                        pr.number,
                        &pr.head_commit[..pr.head_commit.len().min(node.commit_id.len().max(8))],
                        node.commit_id,
                        pr.number
                    ));
                }
                if pr.base_branch != trunk.name {
                    retargets.push(ShipAction::RetargetPr {
                        number: pr.number,
                        bookmark: name.clone(),
                        from_base: pr.base_branch.clone(),
                        to_base: trunk.name.clone(),
                    });
                }
                confirm.push(pr.number);
                shipped.push(pr);
            }
        }
        // A pull request of the work that was someone else's and was
        // taken over ships like the rest. Named, with what a push of its
        // branch can and cannot do.
        let taken: Vec<jiji_core::snapshot::TakenOver> = shipped
            .iter()
            .filter_map(|pr| crate::takeover::taken_over(snapshot, &pr.head_branch, Some(pr)))
            .collect();
        if !taken.is_empty() {
            let listed: Vec<String> = taken.iter().map(|entry| entry.by_author()).collect();
            let one = listed.len() == 1;
            warnings.push(format!(
                "{} {} taken over, and {} with this work like a pull request of your own",
                match listed.as_slice() {
                    [one] => one.clone(),
                    [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
                    [] => unreachable!("checked non-empty"),
                },
                if one { "was" } else { "were" },
                if one { "ships" } else { "ship" },
            ));
            let pushed: Vec<&jiji_core::snapshot::TakenOver> = taken
                .iter()
                .filter(|entry| branch_pushes.contains(&entry.bookmark))
                .collect();
            if let Some(note) = crate::takeover::pushes_their_branch(&pushed) {
                warnings.push(note);
            }
        }
        // GitHub's own stacks. A pull request in one cannot be
        // retargeted, so a stack whose pull requests need it is dissolved
        // first. That is done only when the whole stack ships: taking a
        // stack apart under pull requests that stay open (the rest of
        // the user's own, or someone else's added on top) is theirs to
        // decide, and Land takes a GitHub stack as it is.
        //
        // A stack with members that do not ship is refused even when
        // nothing that ships needs another base (only its bottom ships).
        // The member above would stay based on the shipped branch: GitHub
        // does not move it after a push, and lets no one else. The next
        // landing could then not delete that branch without GitHub
        // closing the pull request (both seen on the sandbox).
        let retargeted = |pr: &PrSummary| {
            retargets
                .iter()
                .any(|a| matches!(a, ShipAction::RetargetPr { number, .. } if *number == pr.number))
        };
        let mut stacks: Vec<u64> = shipped
            .iter()
            .filter_map(|pr| pr.stack.as_ref().map(|seat| seat.number))
            .collect();
        stacks.sort_unstable();
        stacks.dedup();
        // The stacks this plan refuses to dissolve: what stays in them is
        // told by the blocker.
        let mut refused: Vec<u64> = Vec::new();
        for stack in stacks {
            let mut members: Vec<&PrSummary> = prs
                .report
                .prs
                .iter()
                .filter(|pr| pr.stack.as_ref().is_some_and(|seat| seat.number == stack))
                .collect();
            members.sort_by_key(|pr| pr.stack.as_ref().map(|seat| seat.position));
            let left: Vec<&PrSummary> = members
                .iter()
                .copied()
                .filter(|member| !shipped.iter().any(|pr| pr.number == member.number))
                .collect();
            if prs.report.truncated && !crate::submit::sees_whole_stack(&prs.report.prs, stack) {
                // Not dissolved unseen. The refusal for a list that was
                // cut short, below, says so for the whole plan.
                refused.push(stack);
            } else if !left.is_empty() {
                refused.push(stack);
                // By author where the branch is no bookmark here: the
                // user's own need no name.
                let listed: Vec<String> = left
                    .iter()
                    .map(|pr| {
                        let own = prs.of_this_repository(pr)
                            && snapshot
                                .bookmarks
                                .iter()
                                .any(|b| b.is_local && b.name == pr.head_branch);
                        named(pr.number, pr.author.as_deref(), !own)
                    })
                    .collect();
                blockers.push(format!(
                    "the pull requests of this work are in GitHub stack {stack}, and GitHub \
                     does not let a pull request in a stack be pointed at \
                     \u{201c}{}\u{201d}. Shipping would have to dissolve the stack, and {} \
                     {} in it without being part of what ships \u{2014} land the stack \
                     instead (GitHub lands a stack in one request), or dissolve it first \
                     with publish --no-stack",
                    trunk.name,
                    match listed.as_slice() {
                        [one] => one.clone(),
                        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
                        [] => unreachable!("checked non-empty"),
                    },
                    if listed.len() == 1 { "is" } else { "are" },
                ));
            } else if shipped.iter().any(|pr| {
                retargeted(pr) && pr.stack.as_ref().is_some_and(|seat| seat.number == stack)
            }) {
                unstacks.push(ShipAction::Unstack {
                    stack,
                    members: members.iter().map(|pr| pr.number).collect(),
                });
            }
        }
        // Pull requests that sit on the shipped work and do not ship: the
        // next one up in the user's own stack, and someone else's (based
        // on a shipped branch, with a branch that is no bookmark here).
        // After the push each is pointed at trunk and stays open. Not one
        // in a GitHub stack: the block above refused, or GitHub moves it.
        let others = crate::pr::others_on(snapshot, prs, &shipped_branches);
        let is_other = |pr: &PrSummary| others.iter().any(|other| other.number == pr.number);
        let mut on_shipped: Vec<&PrSummary> = prs
            .report
            .prs
            .iter()
            .filter(|pr| {
                shipped_branches.contains(&pr.base_branch.as_str())
                    && !shipped.iter().any(|own| own.number == pr.number)
                    && pr.stack.is_none()
            })
            .collect();
        on_shipped.sort_by_key(|pr| pr.number);
        for pr in on_shipped {
            above.push(ShipAction::RetargetAbove {
                number: pr.number,
                author: pr.author.clone(),
                theirs: is_other(pr),
                branch: pr.head_branch.clone(),
                from_base: pr.base_branch.clone(),
                to_base: trunk.name.clone(),
            });
        }
        // One in a GitHub stack of its own (a stack that starts on a
        // shipped branch, with nothing of what ships in it) stays as it
        // is: GitHub does not let a member of a stack be pointed
        // elsewhere. Said, by number and author.
        let mut staying: Vec<&PrSummary> = prs
            .report
            .prs
            .iter()
            .filter(|pr| {
                shipped_branches.contains(&pr.base_branch.as_str())
                    && !shipped.iter().any(|own| own.number == pr.number)
                    && pr
                        .stack
                        .as_ref()
                        .is_some_and(|seat| !refused.contains(&seat.number))
            })
            .collect();
        staying.sort_by_key(|pr| pr.number);
        for pr in staying {
            let who = named(pr.number, pr.author.as_deref(), is_other(pr));
            warnings.push(format!(
                "{who} is in a GitHub stack above what ships and stays open on \
                 \u{201c}{}\u{201d}: GitHub does not let a pull request in a stack be pointed \
                 at \u{201c}{}\u{201d}, so its stack has to be dissolved (publish --no-stack) \
                 before it can be",
                pr.base_branch, trunk.name
            ));
        }
        // GitHub answered only part of the open pull requests. A pull
        // request of this work that the answer lacks would be left open
        // on a branch below it (that is how Ship used to leave them all),
        // and one of someone else's on a shipped branch would not be seen
        // at all. Ship does not go by a list it cannot call complete.
        let published = chain.iter().flat_map(|node| &node.bookmarks).any(|name| {
            snapshot
                .bookmarks
                .iter()
                .any(|b| b.name == *name && b.is_local && !b.is_trunk && b.remote.is_some())
        });
        if prs.report.truncated && published {
            blockers.push(
                "GitHub answered only the 100 most recently updated open pull requests, so \
                 Ship cannot tell which are open for this work or rest on it. It would leave \
                 the ones it does not see as they are, and it does not dissolve a GitHub stack \
                 it cannot see whole \u{2014} land the stack instead"
                    .to_owned(),
            );
        }
        // A pull request that is not one of this work's, whose head is
        // one of the commits that ship: a bookmark was moved onto their
        // commits. The push of that branch, or of trunk, would put their
        // head inside its base, and GitHub would mark it merged. A push
        // cannot be taken back, so this is refused before the first one,
        // as Publish refuses it.
        //
        // The push carries more than the chain: what a merge change in it
        // brings in through its other parents rides along. Someone else's
        // head can be there too, so for their pull requests the look goes
        // along every parent from there, as far as the snapshot draws, as
        // Publish's does. Their commit counts whatever jj calls it: once
        // their branch is fetched it is immutable here. That walk reaches
        // old history too, so a head down there counts only for a pull
        // request based on a branch this ship pushes, trunk or a shipped
        // one: a pull request from an old commit into some other branch
        // is none of this ship's. (A branch of the user's own that was
        // merged in is the user's to ship that way, as before.)
        let at = |pr: &PrSummary, node: &GraphNode| {
            same_commit(&pr.head_commit, &node.commit_id)
                || same_commit(&node.commit_id, &pr.head_commit)
        };
        let mut merged_in: Vec<&GraphNode> = Vec::new();
        let mut queue: Vec<&GraphNode> = chain
            .iter()
            .flat_map(|node| node.parents.iter().skip(1))
            .filter_map(|id| node_by_id.get(id.as_str()).copied())
            .collect();
        while let Some(node) = queue.pop() {
            if merged_in.iter().chain(&chain).any(|seen| seen.id == node.id) {
                continue;
            }
            merged_in.push(node);
            queue.extend(
                node.parents
                    .iter()
                    .filter_map(|id| node_by_id.get(id.as_str()).copied()),
            );
        }
        let pushed_base = |pr: &PrSummary| {
            pr.base_branch == trunk.name || shipped_branches.contains(&pr.base_branch.as_str())
        };
        let own_bookmark = |pr: &PrSummary| {
            prs.of_this_repository(pr)
                && snapshot
                    .bookmarks
                    .iter()
                    .any(|b| b.is_local && !b.is_trunk && b.name == pr.head_branch)
        };
        for pr in prs
            .report
            .prs
            .iter()
            .filter(|pr| !shipped.iter().any(|own| own.number == pr.number))
        {
            let (how, way_out) = if chain.iter().any(|node| at(pr, node)) {
                (
                    "a bookmark of this work was moved onto its commits",
                    "move the bookmark back off those commits",
                )
            } else if !own_bookmark(pr)
                && pushed_base(pr)
                && merged_in.iter().any(|node| at(pr, node))
            {
                (
                    "a merge in this work brings its commits in",
                    "take that merge out of this work",
                )
            } else {
                continue;
            };
            // Taken over, the pull request ships with the work. A fork's
            // cannot be taken over.
            let or_take_over = if prs.of_this_repository(pr) {
                format!(
                    ", or take the pull request over (take-over {}) if it is yours to ship",
                    pr.number
                )
            } else {
                String::new()
            };
            blockers.push(format!(
                "{} (\u{201c}{}\u{201d}) has its head among the changes this ships: {how}. \
                 Shipping would put that head inside \u{201c}{}\u{201d}, and GitHub would mark \
                 the pull request merged \u{2014} {way_out}{or_take_over}",
                named(pr.number, pr.author.as_deref(), true),
                pr.head_branch,
                pr.base_branch,
            ));
        }
        // Someone else's pull request on the shipped work ends open, on
        // trunk, showing only its own changes: that holds when trunk gets
        // the very commits their branch is built on. It does not hold
        // when this ship replaces those commits first, and nothing here
        // pushes to their branch to make it hold. So such a ship is
        // refused, by number and author, with what replaces them:
        // a rebase onto trunk; a branch that was rewritten here since it
        // was pushed (whether or not this ship pushes it: trunk gets the
        // new commits either way); a change that was rewritten after
        // their branch was built on it (their branch, once fetched,
        // keeps the old commit in view).
        //
        // `from` is the lowest change of the chain whose commit GitHub
        // does not have under them. Only what rests at or above it is
        // touched: an amendment to an upper branch replaces nothing
        // under a pull request that is based on a lower one.
        let replaced: Option<(usize, String)> = if others.is_empty() {
            None
        } else if rebases {
            Some((
                0,
                format!(
                    "this work has to be rebased onto \u{201c}{}\u{201d} before it can ship",
                    trunk.name
                ),
            ))
        } else {
            chain.iter().enumerate().find_map(|(index, node)| {
                let rewritten = node.bookmarks.iter().find(|name| {
                    snapshot.bookmarks.iter().any(|b| {
                        b.name == **name
                            && b.is_local
                            && !b.is_trunk
                            && matches!(b.sync, SyncState::Diverged | SyncState::Behind)
                    })
                });
                if let Some(name) = rewritten {
                    Some((
                        index,
                        format!(
                            "\u{201c}{name}\u{201d} was rewritten here since it was last pushed"
                        ),
                    ))
                } else if node.old_copies_held {
                    Some((
                        index,
                        format!(
                            "{} was rewritten after a branch that is no bookmark here was \
                             built on it",
                            node.change_id
                        ),
                    ))
                } else {
                    None
                }
            })
        };
        if let Some((from, why)) = replaced {
            // Where each of them rests in the chain: on a shipped branch,
            // or on another of them that does (a base comes first in
            // `others`). One that cannot be placed counts as touched.
            let at = |branch: &str| {
                chain
                    .iter()
                    .position(|node| node.bookmarks.iter().any(|name| name == branch))
            };
            let mut placed: Vec<(&PrSummary, usize)> = Vec::new();
            for other in &others {
                let rests_at = if shipped_branches.contains(&other.base_branch.as_str()) {
                    at(&other.base_branch)
                } else {
                    placed
                        .iter()
                        .find(|(below, _)| {
                            prs.of_this_repository(below) && below.head_branch == other.base_branch
                        })
                        .map(|(_, index)| *index)
                };
                placed.push((other, rests_at.unwrap_or(usize::MAX)));
            }
            let listed: Vec<String> = placed
                .iter()
                .filter(|(_, index)| *index >= from)
                .map(|(other, _)| other.by_author())
                .collect();
            if !listed.is_empty() {
                let one = listed.len() == 1;
                blockers.push(format!(
                    "{why}, and {} {} on it, someone else's. Pointed at \u{201c}{}\u{201d} \
                     after such a ship, {} would show the replaced commits as {} own, and \
                     nothing here pushes to {}. Ship goes ahead only when {} only {} own \
                     changes \u{2014} ask {} to rebase onto the new commits and ship then, or \
                     land the stack, which says what {} afterwards",
                    match listed.as_slice() {
                        [one] => one.clone(),
                        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
                        [] => unreachable!("checked non-empty"),
                    },
                    if one { "rests" } else { "rest" },
                    trunk.name,
                    if one { "it" } else { "they" },
                    if one { "its" } else { "their" },
                    if one { "its branch" } else { "their branches" },
                    if one { "it ends up showing" } else { "they end up showing" },
                    if one { "its" } else { "their" },
                    if one { "its author" } else { "their authors" },
                    if one { "it shows" } else { "they show" },
                ));
            }
        }
    }
    // The same, when the pull requests could not be read and only the
    // repository knows: a change someone else's branch holds cannot be
    // rebased here.
    if rebases && chain.iter().any(|node| node.held) && blockers.is_empty() {
        blockers.push(format!(
            "this work has to be rebased onto \u{201c}{}\u{201d} before it can ship, and a \
             branch that is not a bookmark here builds on it, which makes it immutable to \
             jj \u{2014} land the stack instead, or take over the pull request that branch \
             belongs to (take-over BRANCH) and ship then",
            trunk.name
        ));
    }

    // The rebase; then the pull requests made ready (their branches at
    // the commits that ship, out of a GitHub stack, pointed at trunk);
    // then trunk moved and pushed, which merges them; then the look at
    // GitHub, and what sits on the shipped work pointed at trunk.
    let mut actions: Vec<ShipAction> = Vec::new();
    if rebases {
        actions.push(ShipAction::RebaseOntoTrunk {
            root_change: chain[0].id.clone(),
            moves: chain.len() as u32,
        });
    }
    if !branch_pushes.is_empty() {
        actions.push(ShipAction::PushBranches {
            bookmarks: branch_pushes,
            remote: remote.to_owned(),
        });
    }
    actions.extend(unstacks);
    actions.extend(retargets);
    if !already_at_head {
        actions.push(ShipAction::MoveTrunk {
            bookmark: trunk.name.clone(),
            to: head.id.clone(),
        });
    }
    if pushes_trunk {
        actions.push(ShipAction::PushTrunk {
            bookmark: trunk.name.clone(),
            remote: remote.to_owned(),
        });
    }
    if !confirm.is_empty() {
        actions.push(ShipAction::ConfirmMerged { numbers: confirm });
    }
    actions.extend(above);
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
///
/// `forge` is GitHub, for a plan that finishes pull requests; a plan with
/// none never asks it. When a step fails before trunk has been pushed,
/// what the run did to pull requests is put back: each one that was
/// pointed at trunk points at its old base again, and a GitHub stack that
/// was dissolved is linked again. The failed step says what was put back
/// and what could not be. The step that failed counts too: a retarget or
/// a dissolve that reported an error may have gone through.
///
/// A push of trunk that reports an error may also have arrived. Putting
/// the pull requests back then would undo what the push just did for
/// them, so GitHub is asked where trunk is first: there, the run goes on;
/// not there, everything is put back; and when that cannot be seen,
/// nothing is touched and the step says how the pull requests were left.
pub fn execute_ship(
    plan: &ShipPlan,
    vcs: &dyn ShipVcs,
    forge: Option<&dyn ShipForge>,
) -> Result<ShipOutcome, ForgeError> {
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
    for at in 0..steps.len() {
        match run_ship_step(&steps[at].action, vcs, forge) {
            Ok(detail) => {
                steps[at].status = SubmitStepStatus::Done;
                steps[at].detail = Some(detail);
            }
            Err(message) => {
                let touched = steps[..at].iter().any(|step| {
                    step.status == SubmitStepStatus::Done
                        && matches!(
                            step.action,
                            ShipAction::RetargetPr { .. } | ShipAction::Unstack { .. }
                        )
                });
                let arrived = match (&steps[at].action, forge) {
                    (ShipAction::PushTrunk { bookmark, remote }, Some(forge)) if touched => {
                        Some((trunk_arrived(plan, vcs, forge, bookmark), bookmark, remote))
                    }
                    _ => None,
                };
                match arrived {
                    Some((Ok(true), bookmark, remote)) => {
                        steps[at].detail = Some(format!(
                            "Pushed {bookmark} to {remote}: the push reported an error \
                             ({message}), and GitHub has \u{201c}{bookmark}\u{201d} at the \
                             shipped commit all the same"
                        ));
                        steps[at].status = SubmitStepStatus::Done;
                        continue;
                    }
                    Some((Err(look), bookmark, _)) => {
                        let left = left_as_set(&steps[..at]);
                        steps[at].detail = Some(format!(
                            "{message} \u{2014} and whether \u{201c}{bookmark}\u{201d} arrived \
                             could not be seen ({look}). Nothing was put back: {left}. If the \
                             push did not arrive, ship again"
                        ));
                        steps[at].status = SubmitStepStatus::Failed;
                        failed = true;
                        break;
                    }
                    Some((Ok(false), ..)) | None => {}
                }
                let put_back = put_back(&steps[..at], &steps[at].action, forge);
                let step = &mut steps[at];
                step.status = SubmitStepStatus::Failed;
                step.detail = Some(match put_back {
                    Some(story) => format!("{message} \u{2014} {story}"),
                    None => message,
                });
                failed = true;
                break;
            }
        }
    }
    Ok(ShipOutcome { steps, failed })
}

/// Whether GitHub has `bookmark` at the commit this plan ships. An error
/// is "could not be seen".
fn trunk_arrived(
    plan: &ShipPlan,
    vcs: &dyn ShipVcs,
    forge: &dyn ShipForge,
    bookmark: &str,
) -> Result<bool, String> {
    let snapshot = vcs.snapshot().map_err(|err| err.to_string())?;
    let shipped = snapshot
        .nodes
        .iter()
        .find(|node| node.id == plan.head_change)
        .map(|node| node.commit_id.clone())
        .ok_or_else(|| format!("{} is no longer in view here", plan.head_change))?;
    let there = forge.branch_head(bookmark).map_err(|err| err.to_string())?;
    Ok(same_commit(&there, &shipped) || same_commit(&shipped, &there))
}

/// How the steps that ran left the pull requests, for a failure after
/// which nothing is put back.
fn left_as_set(done: &[ShipStep]) -> String {
    let mut left: Vec<String> = Vec::new();
    for step in done.iter().filter(|step| step.status == SubmitStepStatus::Done) {
        match &step.action {
            ShipAction::RetargetPr { number, to_base, .. } => {
                left.push(format!("#{number} points at {to_base}"));
            }
            ShipAction::Unstack { stack, .. } => {
                left.push(format!("GitHub stack {stack} is dissolved"));
            }
            _ => {}
        }
    }
    left.join("; ")
}

/// Undo what the steps before a failure did to pull requests, as long as
/// trunk has not been pushed: after that the pull requests are where they
/// belong. Answers what was put back and what could not be, or nothing
/// when there was nothing to undo. `failed` is the step that failed: when
/// it is a retarget or a dissolve, GitHub may have done it and lost the
/// answer, so it is undone like the others, and said to be unknown when
/// that fails too.
fn put_back(
    done: &[ShipStep],
    failed: &ShipAction,
    forge: Option<&dyn ShipForge>,
) -> Option<String> {
    let ran = |step: &&ShipStep| step.status == SubmitStepStatus::Done;
    if done
        .iter()
        .filter(ran)
        .any(|step| matches!(step.action, ShipAction::PushTrunk { .. }))
    {
        return None;
    }
    let forge = forge?;
    let mut back: Vec<String> = Vec::new();
    let mut stuck: Vec<String> = Vec::new();
    // Bases first, newest step first: a stack links only pull requests
    // whose bases chain.
    if let ShipAction::RetargetPr {
        number,
        from_base,
        to_base,
        ..
    } = failed
    {
        match forge.update_pr_base(*number, from_base) {
            Ok(()) => back.push(format!("#{number} points at {from_base}")),
            Err(err) => stuck.push(format!(
                "#{number} may point at {to_base}: pointing it there reported an error, and \
                 so did pointing it back at {from_base} ({err})"
            )),
        }
    }
    for step in done.iter().rev().filter(ran) {
        if let ShipAction::RetargetPr {
            number,
            from_base,
            to_base,
            ..
        } = &step.action
        {
            match forge.update_pr_base(*number, from_base) {
                Ok(()) => back.push(format!("#{number} points at {from_base} again")),
                Err(err) => stuck.push(format!(
                    "#{number} still points at {to_base} and could not be pointed back at \
                     {from_base} ({err})"
                )),
            }
        }
    }
    if let ShipAction::Unstack { stack, members } = failed {
        let listed = crate::land::number_list(members.iter().copied());
        match forge.create_stack(members) {
            Ok(new) => back.push(format!(
                "{listed} are linked again, as GitHub stack {new} (it was {stack})"
            )),
            Err(err) => stuck.push(format!(
                "GitHub stack {stack} may be dissolved in part, and {listed} could not be \
                 linked again ({err})"
            )),
        }
    }
    for step in done.iter().rev().filter(ran) {
        if let ShipAction::Unstack { stack, members } = &step.action {
            let listed = crate::land::number_list(members.iter().copied());
            match forge.create_stack(members) {
                Ok(new) => back.push(format!(
                    "{listed} are linked again, as GitHub stack {new} (it was {stack})"
                )),
                Err(err) => stuck.push(format!(
                    "GitHub stack {stack} was dissolved and {listed} could not be linked \
                     again ({err})"
                )),
            }
        }
    }
    let pushed_branches = done
        .iter()
        .filter(ran)
        .any(|step| matches!(step.action, ShipAction::PushBranches { .. }));
    if back.is_empty() && stuck.is_empty() && !pushed_branches {
        return None;
    }
    let mut story = String::from("trunk was not pushed, and no pull request was merged");
    if !back.is_empty() {
        story.push_str(&format!(". Put back: {}", back.join("; ")));
    }
    if !stuck.is_empty() {
        story.push_str(&format!(". Not put back: {}", stuck.join("; ")));
    }
    if pushed_branches {
        story.push_str(
            ". The branches pushed before that stay as pushed: each pull request shows the \
             commits that would have shipped",
        );
    }
    Some(story)
}

/// "#12 by @sam" for someone else's pull request, "#12" for one's own.
fn named(number: u64, author: Option<&str>, theirs: bool) -> String {
    match (theirs, author) {
        (true, Some(login)) => format!("#{number} by @{login}"),
        _ => format!("#{number}"),
    }
}

fn run_ship_step(
    action: &ShipAction,
    vcs: &dyn ShipVcs,
    forge: Option<&dyn ShipForge>,
) -> Result<String, String> {
    let github = || forge.ok_or_else(|| "GitHub is not connected".to_owned());
    match action {
        ShipAction::RebaseOntoTrunk { root_change, .. } => vcs
            .rebase_onto_trunk(root_change)
            .map_err(|err| err.to_string()),
        ShipAction::PushBranches { bookmarks, remote } => {
            vcs.push_bookmarks(bookmarks, remote).map_err(|err| {
                // The lease held: when the branch that had moved is one
                // that was taken over, its author pushed to it again.
                let refusal = err.to_string();
                let taken = vcs
                    .snapshot()
                    .map(|snapshot| snapshot.taken_over)
                    .unwrap_or_default();
                match crate::takeover::pushed_to_again(&refusal, &taken, "ship again") {
                    Some(story) => format!("{refusal}. {story}"),
                    None => refusal,
                }
            })
        }
        ShipAction::Unstack { stack, .. } => {
            let kept = github()?.unstack(*stack).map_err(|err| err.to_string())?;
            if kept.is_empty() {
                Ok(format!(
                    "Dissolved GitHub stack {stack}; its pull requests are a plain chain"
                ))
            } else {
                // One GitHub would not let go (it documents a pull request
                // queued to merge) cannot be pointed at trunk either.
                Err(format!(
                    "GitHub kept {} in stack {stack} and took the rest out: a pull request \
                     queued to merge cannot leave its stack",
                    crate::land::number_list(kept.into_iter())
                ))
            }
        }
        ShipAction::RetargetPr {
            number, to_base, ..
        } => {
            github()?
                .update_pr_base(*number, to_base)
                .map_err(|err| err.to_string())?;
            Ok(format!("Pointed #{number} at {to_base}"))
        }
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
        ShipAction::ConfirmMerged { numbers } => {
            // Trunk is pushed and the work is in. A pull request GitHub
            // has not marked yet, or a failed look, is told and is not a
            // failed ship.
            let states = match github().and_then(|forge| {
                forge.await_merged(numbers).map_err(|err| err.to_string())
            }) {
                Ok(states) => states,
                Err(err) => {
                    return Ok(format!(
                        "could not ask GitHub whether {} {} merged ({err}); look there",
                        crate::land::number_list(numbers.iter().copied()),
                        if numbers.len() == 1 { "is" } else { "are" },
                    ))
                }
            };
            let of = |wanted: PrState| -> Vec<u64> {
                states
                    .iter()
                    .filter(|(_, state)| *state == wanted)
                    .map(|(number, _)| *number)
                    .collect()
            };
            let (merged, open, closed) =
                (of(PrState::Merged), of(PrState::Open), of(PrState::Closed));
            let mut story: Vec<String> = Vec::new();
            if !merged.is_empty() {
                story.push(format!(
                    "GitHub marked {} merged",
                    crate::land::number_list(merged.into_iter())
                ));
            }
            if !open.is_empty() {
                story.push(format!(
                    "{} {} still open: GitHub has not marked {} merged yet; look again in a \
                     moment",
                    crate::land::number_list(open.iter().copied()),
                    if open.len() == 1 { "is" } else { "are" },
                    if open.len() == 1 { "it" } else { "them" },
                ));
            }
            if !closed.is_empty() {
                story.push(format!(
                    "{} {} closed without being merged",
                    crate::land::number_list(closed.iter().copied()),
                    if closed.len() == 1 { "is" } else { "are" },
                ));
            }
            Ok(story.join("; "))
        }
        ShipAction::RetargetAbove {
            number,
            author,
            theirs,
            from_base,
            to_base,
            ..
        } => {
            // After the push the work is shipped. A pull request that
            // could not be pointed at trunk stays on a branch that still
            // exists, and is told.
            let who = named(*number, author.as_deref(), *theirs);
            Ok(
                match github().and_then(|forge| {
                    forge
                        .update_pr_base(*number, to_base)
                        .map_err(|err| err.to_string())
                }) {
                    Ok(()) => format!("Pointed {who} at {to_base}; it stays open"),
                    Err(err) => format!(
                        "{who} could not be pointed at {to_base} ({err}); it stays open on \
                         {from_base}"
                    ),
                },
            )
        }
        ShipAction::NewWorkingCopy { on } => {
            vcs.new_change(on).map_err(|err| err.to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pr::{PrState, PrStateReport, PrSummary};
    use jiji_core::snapshot::{BookmarkState, ConflictItem, ConflictKind, NodeKind};
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
            tags: Vec::new(),
            is_empty: false,
            has_conflict: false,
            is_divergent: false,
            held: false,
            old_copies_held: false,
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
            tracked: sync != SyncState::LocalOnly,
            is_merged: false,
            target_description: String::new(),
            target_author: String::new(),
            target_timestamp: String::new(),
        }
    }

    fn snapshot(nodes: Vec<GraphNode>, bookmarks: Vec<BookmarkState>) -> RepoSnapshot {
        RepoSnapshot {
            repo_path: "/tmp/repo".into(),
            repo_name: "repo".into(),
            backend: "test".into(),
            view: jiji_core::snapshot::SnapshotView::default(),
            truncated: false,
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
            taken_over: Vec::new(),
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
            merge_commit: None,
            stack: None,
            author: None,
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

        // The other copy is one someone else's branch keeps of an earlier
        // version: not the user's to resolve, and not a reason to refuse.
        let mut snap = stack_on_trunk();
        snap.nodes[1].is_divergent = true;
        snap.nodes[1].old_copies_held = true;
        let plan = plan_ship(&snap, None, "origin", "b2").unwrap();
        assert!(plan.blockers.is_empty(), "{:?}", plan.blockers);
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

    /// trunk ─ b1 (bottom, #7 into main) ─ b2 (top, #8 into bottom), both
    /// branches where GitHub has them.
    fn published_pair() -> (RepoSnapshot, RepoPrState) {
        let mut snap = stack_on_trunk();
        snap.nodes[1].bookmarks = vec!["bottom".into()];
        snap.nodes[0].bookmarks = vec!["top".into()];
        snap.bookmarks
            .push(bookmark("bottom", "b1", SyncState::Synced, false));
        snap.bookmarks
            .push(bookmark("top", "b2", SyncState::Synced, false));
        let prs = prs_with(vec![
            PrSummary {
                head_commit: "cb1".into(),
                ..open_pr(7, "bottom")
            },
            PrSummary {
                head_commit: "cb2".into(),
                base_branch: "bottom".into(),
                ..open_pr(8, "top")
            },
        ]);
        (snap, prs)
    }

    fn told(plan: &ShipPlan) -> Vec<String> {
        plan.actions
            .iter()
            .map(|action| match action {
                ShipAction::RebaseOntoTrunk { root_change, .. } => format!("rebase {root_change}"),
                ShipAction::PushBranches { bookmarks, .. } => {
                    format!("push branches {}", bookmarks.join(","))
                }
                ShipAction::Unstack { stack, members } => format!("unstack {stack} {members:?}"),
                ShipAction::RetargetPr {
                    number,
                    from_base,
                    to_base,
                    ..
                } => format!("retarget #{number} {from_base}->{to_base}"),
                ShipAction::MoveTrunk { to, .. } => format!("move trunk to {to}"),
                ShipAction::PushTrunk { .. } => "push trunk".into(),
                ShipAction::ConfirmMerged { numbers } => format!("confirm {numbers:?}"),
                ShipAction::RetargetAbove {
                    number,
                    author,
                    theirs,
                    to_base,
                    ..
                } => format!(
                    "above #{number}{} -> {to_base}",
                    if *theirs {
                        format!(" by {}", author.as_deref().unwrap_or("?"))
                    } else {
                        String::new()
                    }
                ),
                ShipAction::NewWorkingCopy { on } => format!("new working copy on {on}"),
            })
            .collect()
    }

    #[test]
    fn a_shipped_working_copy_respawns_and_its_pull_request_is_confirmed() {
        let mut snap = stack_on_trunk();
        snap.working_copy = "b2".into();
        snap.nodes[0].bookmarks = vec!["feature".into()];
        snap.bookmarks.push(bookmark(
            "feature",
            "b2",
            SyncState::Synced,
            false,
        ));
        let prs = prs_with(vec![PrSummary {
            head_commit: "cb2".into(),
            ..open_pr(7, "feature")
        }]);
        let plan = plan_ship(&snap, Some(&prs), "origin", "b2").unwrap();
        assert!(plan.blockers.is_empty(), "{:?}", plan.blockers);
        // Based on trunk already and pushed where it is: the push merges
        // it, and the plan says so by number.
        assert_eq!(
            told(&plan),
            ["move trunk to b2", "push trunk", "confirm [7]", "new working copy on b2"]
        );
    }

    /// Once trunk is pushed the pull requests are where they belong. A
    /// step that fails after that (here the new working copy) puts
    /// nothing back: pointing a pull request at its old base again would
    /// undo what the push just did for it.
    #[test]
    fn nothing_is_put_back_once_trunk_is_pushed() {
        struct NoNewChange(Vcs);
        impl ShipVcs for NoNewChange {
            fn snapshot(&self) -> Result<RepoSnapshot, BackendError> {
                self.0.snapshot()
            }
            fn rebase_onto_trunk(&self, root_change: &str) -> Result<String, BackendError> {
                self.0.rebase_onto_trunk(root_change)
            }
            fn move_bookmark(&self, name: &str, to_change: &str) -> Result<String, BackendError> {
                self.0.move_bookmark(name, to_change)
            }
            fn push_bookmarks(
                &self,
                bookmarks: &[String],
                remote: &str,
            ) -> Result<String, BackendError> {
                self.0.push_bookmarks(bookmarks, remote)
            }
            fn new_change(&self, _parent_change: &str) -> Result<String, BackendError> {
                Err(BackendError::MutationFailed("the working copy is locked".into()))
            }
        }
        let (mut snap, prs) = published_pair();
        snap.working_copy = "b2".into();
        let plan = plan_ship(&snap, Some(&prs), "origin", "b2").unwrap();
        assert_eq!(
            told(&plan),
            [
                "retarget #8 bottom->main",
                "move trunk to b2",
                "push trunk",
                "confirm [7, 8]",
                "new working copy on b2",
            ]
        );
        let vcs = NoNewChange(Vcs {
            snap: snap.clone(),
            push_fails: false,
            calls: RefCell::new(vec![]),
        });
        let hub = Hub::default();
        let outcome = execute_ship(&plan, &vcs, Some(&hub)).unwrap();
        assert!(outcome.failed);
        let failed = outcome
            .steps
            .iter()
            .find(|step| step.status == SubmitStepStatus::Failed)
            .unwrap();
        assert!(matches!(failed.action, ShipAction::NewWorkingCopy { .. }));
        // The step's own words, and no account of pull requests put back.
        assert_eq!(
            failed.detail.as_deref(),
            Some("Could not apply the change: the working copy is locked")
        );
        assert_eq!(*hub.calls.borrow(), ["retarget:8:main", "await:[7, 8]"]);
    }

    /// Seen on GitHub: a shipped chain of three left the two pull requests
    /// above the bottom one open, because their bases are the branches
    /// below them and those never receive the commits; afterwards GitHub
    /// refused to point them at trunk ("no new commits"). So each is
    /// pointed at trunk before the push, and the push merges them all.
    #[test]
    fn the_upper_pull_requests_are_pointed_at_trunk_before_the_push() {
        let (snap, prs) = published_pair();
        let plan = plan_ship(&snap, Some(&prs), "origin", "b2").unwrap();
        assert!(plan.blockers.is_empty(), "{:?}", plan.blockers);
        assert_eq!(
            told(&plan),
            ["retarget #8 bottom->main", "move trunk to b2", "push trunk", "confirm [7, 8]"]
        );

        let vcs = Vcs {
            snap: snap.clone(),
            push_fails: false,
            calls: RefCell::new(vec![]),
        };
        let hub = Hub::default();
        let outcome = execute_ship(&plan, &vcs, Some(&hub)).unwrap();
        assert!(!outcome.failed, "{:?}", outcome.steps);
        // In that order: GitHub's side first, then the one push.
        assert_eq!(*hub.calls.borrow(), ["retarget:8:main", "await:[7, 8]"]);
        assert_eq!(*vcs.calls.borrow(), ["move:main:b2", "push:main:origin"]);
        assert_eq!(
            outcome.steps.last().unwrap().detail.as_deref(),
            Some("GitHub marked #7 and #8 merged")
        );

        // Without GitHub (no token, or a failed read) the stack ships as
        // it always did, and nothing is promised about pull requests.
        let plan = plan_ship(&snap, None, "origin", "b2").unwrap();
        assert_eq!(told(&plan), ["move trunk to b2", "push trunk"]);
        // A stack that has shipped already plans nothing, pull requests
        // or not.
        let mut shipped = snap.clone();
        shipped.bookmarks[0].target = "b2".into();
        let plan = plan_ship(&shipped, Some(&prs), "origin", "b2").unwrap();
        assert!(plan.actions.is_empty(), "{:?}", plan.actions);
    }

    /// One push, and it either moved trunk or did not. When it did not,
    /// the pull requests point where they pointed before the ship, and
    /// the failed step says what state each is in.
    #[test]
    fn a_refused_push_points_the_pull_requests_back() {
        let (snap, prs) = published_pair();
        let plan = plan_ship(&snap, Some(&prs), "origin", "b2").unwrap();
        let refused = || Vcs {
            snap: snap.clone(),
            push_fails: true,
            calls: RefCell::new(vec![]),
        };
        let hub = Hub::default();
        let outcome = execute_ship(&plan, &refused(), Some(&hub)).unwrap();
        assert!(outcome.failed);
        assert_eq!(*hub.calls.borrow(), ["retarget:8:main", "retarget:8:bottom"]);
        let failed = outcome
            .steps
            .iter()
            .find(|step| step.status == SubmitStepStatus::Failed)
            .unwrap();
        assert!(matches!(failed.action, ShipAction::PushTrunk { .. }));
        assert_eq!(
            failed.detail.as_deref(),
            Some(
                "Could not apply the change: Refusing to push: origin's main moved since the \
                 last fetch \u{2014} trunk was not pushed, and no pull request was merged. Put \
                 back: #8 points at bottom again"
            )
        );
        // Nothing after the push ran.
        assert_eq!(outcome.steps.last().unwrap().status, SubmitStepStatus::Skipped);

        // GitHub refuses the way back too: said, with what to do.
        let hub = Hub {
            refuse: vec![(8, "bottom".into())],
            ..Hub::default()
        };
        let outcome = execute_ship(&plan, &refused(), Some(&hub)).unwrap();
        let failed = outcome
            .steps
            .iter()
            .find(|step| step.status == SubmitStepStatus::Failed)
            .unwrap();
        assert!(
            failed.detail.as_deref().unwrap().ends_with(
                "Not put back: #8 still points at main and could not be pointed back at \
                 bottom (GitHub request failed: HTTP 422: refused)"
            ),
            "{:?}",
            failed.detail
        );
    }

    /// A push that reports an error has not always failed: the remote can
    /// have taken it before the answer was lost. Pointing the pull
    /// requests back then would leave them open on a branch below, with
    /// their commits on trunk, which is what Ship is there to prevent.
    /// So GitHub is asked where trunk is before anything is put back.
    /// (Found in review.)
    #[test]
    fn a_failed_push_is_looked_at_before_anything_is_put_back() {
        let (snap, prs) = published_pair();
        let plan = plan_ship(&snap, Some(&prs), "origin", "b2").unwrap();
        let refused = || Vcs {
            snap: snap.clone(),
            push_fails: true,
            calls: RefCell::new(vec![]),
        };
        let push = |outcome: &ShipOutcome| {
            outcome
                .steps
                .iter()
                .find(|step| matches!(step.action, ShipAction::PushTrunk { .. }))
                .map(|step| (step.status, step.detail.clone().unwrap_or_default()))
                .unwrap()
        };

        // GitHub has trunk at the shipped commit: the push arrived. The
        // run goes on, and nothing is pointed back.
        let hub = Hub {
            trunk_at: Some(Ok("cb2".into())),
            ..Hub::default()
        };
        let outcome = execute_ship(&plan, &refused(), Some(&hub)).unwrap();
        assert!(!outcome.failed, "{:?}", outcome.steps);
        let (status, detail) = push(&outcome);
        assert_eq!(status, SubmitStepStatus::Done);
        assert!(
            detail.starts_with("Pushed main to origin: the push reported an error (")
                && detail.ends_with(
                    "), and GitHub has \u{201c}main\u{201d} at the shipped commit all the same"
                ),
            "{detail}"
        );
        assert_eq!(*hub.calls.borrow(), ["retarget:8:main", "await:[7, 8]"]);

        // GitHub cannot be asked: nothing is touched, and the step says
        // how the pull requests were left.
        let hub = Hub {
            trunk_at: Some(Err("offline".into())),
            ..Hub::default()
        };
        let outcome = execute_ship(&plan, &refused(), Some(&hub)).unwrap();
        assert!(outcome.failed);
        let (status, detail) = push(&outcome);
        assert_eq!(status, SubmitStepStatus::Failed);
        assert!(
            detail.ends_with(
                "and whether \u{201c}main\u{201d} arrived could not be seen (Could not reach \
                 GitHub: offline). Nothing was put back: #8 points at main. If the push did \
                 not arrive, ship again"
            ),
            "{detail}"
        );
        assert_eq!(*hub.calls.borrow(), ["retarget:8:main"]);
    }

    /// The step that failed is undone with the ones before it: GitHub can
    /// apply a retarget, or dissolve part of a stack, and the answer be
    /// lost or be an error. (Found in review: the pull request stayed
    /// pointed at trunk, and a half-dissolved stack stayed that way.)
    #[test]
    fn the_step_that_failed_is_put_back_too() {
        let (snap, prs) = published_pair();
        let vcs = || Vcs {
            snap: snap.clone(),
            push_fails: false,
            calls: RefCell::new(vec![]),
        };
        let failed = |outcome: &ShipOutcome| {
            outcome
                .steps
                .iter()
                .find(|step| step.status == SubmitStepStatus::Failed)
                .map(|step| step.detail.clone().unwrap_or_default())
                .unwrap()
        };

        // The retarget went through and its answer did not come back.
        let plan = plan_ship(&snap, Some(&prs), "origin", "b2").unwrap();
        let hub = Hub {
            loses_answer: vec![(8, "main".into())],
            ..Hub::default()
        };
        let outcome = execute_ship(&plan, &vcs(), Some(&hub)).unwrap();
        assert!(outcome.failed);
        assert_eq!(*hub.calls.borrow(), ["retarget:8:main", "retarget:8:bottom"]);
        assert_eq!(hub.bases.borrow().get(&8).map(String::as_str), Some("bottom"));
        assert!(
            failed(&outcome).ends_with(
                "trunk was not pushed, and no pull request was merged. Put back: #8 points at \
                 bottom"
            ),
            "{}",
            failed(&outcome)
        );

        // GitHub kept one pull request in the stack and took the other
        // out: the stack is linked again.
        let seat = |position: u32| {
            Some(crate::pr::StackSeat {
                number: 90,
                position,
                size: 2,
                base: "main".into(),
            })
        };
        let mut report = prs.report.clone();
        report.prs[0].stack = seat(1);
        report.prs[1].stack = seat(2);
        let plan = plan_ship(&snap, Some(&prs_with(report.prs)), "origin", "b2").unwrap();
        let hub = Hub {
            keeps_stacked: vec![8],
            ..Hub::default()
        };
        let outcome = execute_ship(&plan, &vcs(), Some(&hub)).unwrap();
        assert!(outcome.failed);
        assert_eq!(*hub.calls.borrow(), ["unstack:90", "stack:[7, 8]"]);
        assert!(
            failed(&outcome).ends_with(
                "Put back: #7 and #8 are linked again, as GitHub stack 91 (it was 90)"
            ),
            "{}",
            failed(&outcome)
        );
    }

    /// GitHub marks a pull request merged when its head is inside its
    /// base, so the head has to be the commit that ships. A rebase gives
    /// the changes new commits, and a bookmark can be ahead of its
    /// branch: the branches are pushed first then.
    #[test]
    fn branches_are_pushed_first_when_the_commits_that_ship_are_not_on_github() {
        // t1 is the fetched trunk tip; the stack still sits on t0.
        let (mut snap, prs) = published_pair();
        snap.nodes.insert(
            2,
            node("t1", "newer trunk", NodeKind::Immutable, &["t0"], &["main"]),
        );
        snap.nodes[3].bookmarks.clear();
        snap.bookmarks[0].target = "t1".into();
        let plan = plan_ship(&snap, Some(&prs), "origin", "b2").unwrap();
        assert!(plan.blockers.is_empty(), "{:?}", plan.blockers);
        assert_eq!(
            told(&plan),
            [
                "rebase b1",
                "push branches bottom,top",
                "retarget #8 bottom->main",
                "move trunk to b2",
                "push trunk",
                "confirm [7, 8]",
            ]
        );
        // A refused trunk push after that says the branches stay pushed.
        let vcs = Vcs {
            snap: snap.clone(),
            push_fails: true,
            calls: RefCell::new(vec![]),
        };
        let outcome = execute_ship(&plan, &vcs, Some(&Hub::default())).unwrap();
        // (The scripted push refuses the first push it sees, the branches'.)
        let failed = outcome
            .steps
            .iter()
            .find(|step| step.status == SubmitStepStatus::Failed)
            .unwrap();
        assert!(matches!(failed.action, ShipAction::PushBranches { .. }));
        assert!(!failed.detail.as_deref().unwrap().contains("Put back"));

        // On current trunk, with one bookmark ahead of its branch.
        let (mut snap, prs) = published_pair();
        snap.bookmarks[2].sync = SyncState::Ahead;
        let plan = plan_ship(&snap, Some(&prs), "origin", "b2").unwrap();
        assert_eq!(told(&plan)[0], "push branches top");

        // In sync by the last fetch, and GitHub has another commit: the
        // push to trunk would not merge that pull request.
        let (snap, mut prs) = published_pair();
        prs.report.prs[1].head_commit = "0ddba11".into();
        let prs = prs_with(prs.report.prs);
        let plan = plan_ship(&snap, Some(&prs), "origin", "b2").unwrap();
        assert_eq!(plan.blockers.len(), 1, "{:?}", plan.blockers);
        assert!(
            plan.blockers[0].starts_with(
                "#8's branch on GitHub is at 0ddba11, not at cb2 where \u{201c}top\u{201d} is here"
            ),
            "{:?}",
            plan.blockers
        );
    }

    /// A pull request in a GitHub stack cannot be retargeted, and GitHub
    /// does not finish a shipped stack by itself (seen on the sandbox:
    /// the bottom one merged, the one above stayed open in the stack).
    /// So the stack is dissolved first, when all of it ships; a refused
    /// push links it again. A stack with a member that does not ship is
    /// not taken apart: the ship is refused.
    #[test]
    fn a_github_stack_is_dissolved_to_ship_only_when_all_of_it_ships() {
        let seat = |position: u32, size: u32| {
            Some(crate::pr::StackSeat {
                number: 90,
                position,
                size,
                base: "main".into(),
            })
        };
        let (snap, prs) = published_pair();
        let mut report = prs.report.clone();
        report.prs[0].stack = seat(1, 2);
        report.prs[1].stack = seat(2, 2);
        // The user's own, as GitHub answers them: with a login.
        report.prs[0].author = Some("me".into());
        report.prs[1].author = Some("me".into());
        let stacked = prs_with(report.prs.clone());
        let plan = plan_ship(&snap, Some(&stacked), "origin", "b2").unwrap();
        assert!(plan.blockers.is_empty(), "{:?}", plan.blockers);
        assert_eq!(
            told(&plan),
            [
                "unstack 90 [7, 8]",
                "retarget #8 bottom->main",
                "move trunk to b2",
                "push trunk",
                "confirm [7, 8]",
            ]
        );
        let vcs = Vcs {
            snap: snap.clone(),
            push_fails: true,
            calls: RefCell::new(vec![]),
        };
        let hub = Hub::default();
        let outcome = execute_ship(&plan, &vcs, Some(&hub)).unwrap();
        assert!(outcome.failed);
        // Bases first, then the link: a stack takes pull requests whose
        // bases chain.
        assert_eq!(
            *hub.calls.borrow(),
            ["unstack:90", "retarget:8:main", "retarget:8:bottom", "stack:[7, 8]"]
        );
        let failed = outcome
            .steps
            .iter()
            .find(|step| step.status == SubmitStepStatus::Failed)
            .unwrap();
        assert!(
            failed.detail.as_deref().unwrap().ends_with(
                "Put back: #8 points at bottom again; #7 and #8 are linked again, as GitHub \
                 stack 91 (it was 90)"
            ),
            "{:?}",
            failed.detail
        );

        // Someone else's pull request was added to the stack above.
        report.prs[0].stack = seat(1, 3);
        report.prs[1].stack = seat(2, 3);
        report.prs.push(PrSummary {
            base_branch: "top".into(),
            author: Some("kim".into()),
            stack: seat(3, 3),
            ..open_pr(9, "their-top")
        });
        let with_theirs = prs_with(report.prs);
        let plan = plan_ship(&snap, Some(&with_theirs), "origin", "b2").unwrap();
        assert_eq!(plan.blockers.len(), 1, "{:?}", plan.blockers);
        assert!(
            plan.blockers[0].contains(
                "Shipping would have to dissolve the stack, and #9 by @kim is in it without \
                 being part of what ships"
            ),
            "{:?}",
            plan.blockers
        );
        // The blocker tells it; no note repeats it.
        assert!(plan.warnings.is_empty(), "{:?}", plan.warnings);

        // Only the stack's bottom ships. Nothing that ships needs another
        // base, and the ship is refused all the same: the member above
        // would stay based on the shipped branch, where GitHub neither
        // moves it nor lets anyone else, and the landing after that could
        // not clean the branch up without GitHub closing it. (Seen on the
        // sandbox: the build before this closed the user's own pull
        // request that way.)
        let plan = plan_ship(&snap, Some(&stacked), "origin", "b1").unwrap();
        assert_eq!(plan.blockers.len(), 1, "{:?}", plan.blockers);
        assert!(
            plan.blockers[0].contains(
                "Shipping would have to dissolve the stack, and #8 is in it without being part \
                 of what ships"
            ),
            "{:?}",
            plan.blockers
        );
        assert!(!plan
            .actions
            .iter()
            .any(|a| matches!(a, ShipAction::Unstack { .. })));

        // GitHub answered only part of the open pull requests, and the
        // stack has a third seat the answer does not show: a stack that
        // cannot be seen whole is not dissolved.
        let mut partial = stacked.clone();
        partial.report.truncated = true;
        for pr in &mut partial.report.prs {
            pr.stack.as_mut().unwrap().size = 3;
        }
        let plan = plan_ship(&snap, Some(&partial), "origin", "b2").unwrap();
        assert_eq!(plan.blockers.len(), 1, "{:?}", plan.blockers);
        assert!(
            plan.blockers[0].contains("it does not dissolve a GitHub stack it cannot see whole"),
            "{:?}",
            plan.blockers
        );
        assert!(!plan
            .actions
            .iter()
            .any(|a| matches!(a, ShipAction::Unstack { .. })));

        // A stack that starts on the shipped work, with nothing of what
        // ships in it, is not this plan's to dissolve and does not stop
        // it. Its bottom cannot be pointed at trunk afterwards, and the
        // plan says so by number and author.
        let (snap, prs) = published_pair();
        let mut report = prs.report.clone();
        report.prs.push(PrSummary {
            base_branch: "top".into(),
            author: Some("kim".into()),
            stack: Some(crate::pr::StackSeat {
                number: 91,
                position: 1,
                size: 2,
                base: "top".into(),
            }),
            ..open_pr(9, "their-top")
        });
        let plan = plan_ship(&snap, Some(&prs_with(report.prs)), "origin", "b2").unwrap();
        assert!(plan.blockers.is_empty(), "{:?}", plan.blockers);
        assert_eq!(
            told(&plan),
            ["retarget #8 bottom->main", "move trunk to b2", "push trunk", "confirm [7, 8]"]
        );
        assert_eq!(
            plan.warnings,
            ["#9 by @kim is in a GitHub stack above what ships and stays open on \
              \u{201c}top\u{201d}: GitHub does not let a pull request in a stack be pointed at \
              \u{201c}main\u{201d}, so its stack has to be dissolved (publish --no-stack) before \
              it can be"]
        );
    }

    /// A pull request that sits on the shipped work without being part of
    /// it is not marked merged: someone else's on top, or the next one up
    /// in the user's own stack. After the push it is pointed at trunk,
    /// and stays open. The commits under it are the very ones that
    /// shipped, so it shows only its own changes.
    #[test]
    fn a_pull_request_on_the_shipped_work_stays_open_and_is_pointed_at_trunk() {
        let (mut snap, prs) = published_pair();
        // The user's own next segment, above the shipped head.
        snap.nodes
            .insert(0, node("b3", "feat: next", NodeKind::Mutable, &["b2"], &["next"]));
        snap.bookmarks
            .push(bookmark("next", "b3", SyncState::Synced, false));
        let mut report = prs.report.clone();
        report.prs.push(PrSummary {
            head_commit: "cb3".into(),
            base_branch: "top".into(),
            ..open_pr(10, "next")
        });
        // Someone else's, on the shipped top and on the shipped bottom.
        report.prs.push(PrSummary {
            base_branch: "top".into(),
            author: Some("kim".into()),
            ..open_pr(9, "their-top")
        });
        report.prs.push(PrSummary {
            base_branch: "bottom".into(),
            author: Some("sam".into()),
            ..open_pr(11, "their-mid")
        });
        let prs = prs_with(report.prs);
        let plan = plan_ship(&snap, Some(&prs), "origin", "b2").unwrap();
        assert!(plan.blockers.is_empty(), "{:?}", plan.blockers);
        assert_eq!(
            told(&plan),
            [
                "retarget #8 bottom->main",
                "move trunk to b2",
                "push trunk",
                // Only the shipped two are to be merged.
                "confirm [7, 8]",
                "above #9 by kim -> main",
                "above #10 -> main",
                "above #11 by sam -> main",
            ]
        );
        let vcs = Vcs {
            snap: snap.clone(),
            push_fails: false,
            calls: RefCell::new(vec![]),
        };
        // One of them cannot be pointed at trunk: the ship is done all
        // the same, and the step says where that pull request stayed.
        let hub = Hub {
            refuse: vec![(11, "main".into())],
            ..Hub::default()
        };
        let outcome = execute_ship(&plan, &vcs, Some(&hub)).unwrap();
        assert!(!outcome.failed, "{:?}", outcome.steps);
        let details: Vec<&str> = outcome
            .steps
            .iter()
            .filter(|step| matches!(step.action, ShipAction::RetargetAbove { .. }))
            .filter_map(|step| step.detail.as_deref())
            .collect();
        assert_eq!(
            details,
            [
                "Pointed #9 by @kim at main; it stays open",
                "Pointed #10 at main; it stays open",
                "#11 by @sam could not be pointed at main (GitHub request failed: HTTP 422: \
                 refused); it stays open on bottom",
            ]
        );
        // After the push, never before: until trunk has the commits, a
        // pull request pointed at trunk would show the shipped work too.
        let calls = hub.calls.borrow();
        let at = |call: &str| calls.iter().position(|c| c == call).unwrap();
        assert!(at("await:[7, 8]") < at("retarget:9:main"));
    }

    /// Someone else's pull request on the shipped work ends showing only
    /// its own changes when trunk gets the very commits its branch is
    /// built on. A ship that replaces those commits first cannot keep
    /// that promise, and nothing here pushes to their branch: it is
    /// refused, by number and author, whichever way the commits are
    /// replaced. (Found in review for the second and third: the plan
    /// went ahead and said "it stays open".)
    #[test]
    fn a_ship_that_would_rewrite_commits_under_someone_elses_pull_request_is_refused() {
        let with_theirs = |prs: &RepoPrState| {
            let mut report = prs.report.clone();
            report.prs.push(PrSummary {
                base_branch: "top".into(),
                author: Some("kim".into()),
                ..open_pr(9, "their-top")
            });
            prs_with(report.prs)
        };
        let ends = "ask its author to rebase onto the new commits and ship then, or land the \
                    stack, which says what it shows afterwards";

        // A rebase onto trunk.
        let (mut snap, prs) = published_pair();
        snap.nodes.insert(
            2,
            node("t1", "newer trunk", NodeKind::Immutable, &["t0"], &["main"]),
        );
        snap.nodes[3].bookmarks.clear();
        snap.bookmarks[0].target = "t1".into();
        let plan = plan_ship(&snap, Some(&with_theirs(&prs)), "origin", "b2").unwrap();
        assert_eq!(plan.blockers.len(), 1, "{:?}", plan.blockers);
        assert!(
            plan.blockers[0].starts_with(
                "this work has to be rebased onto \u{201c}main\u{201d} before it can ship, and \
                 #9 by @kim rests on it, someone else's. Pointed at \u{201c}main\u{201d} after \
                 such a ship, it would show the replaced commits as its own, and nothing here \
                 pushes to its branch."
            ) && plan.blockers[0].ends_with(ends),
            "{:?}",
            plan.blockers
        );

        // With no pull requests read, the repository still knows: their
        // branch, once fetched, makes the changes under it immutable.
        snap.nodes[0].kind = NodeKind::Immutable;
        snap.nodes[0].held = true;
        snap.nodes[1].kind = NodeKind::Immutable;
        snap.nodes[1].held = true;
        let plan = plan_ship(&snap, None, "origin", "b2").unwrap();
        assert_eq!(plan.blockers.len(), 1, "{:?}", plan.blockers);
        assert!(plan.blockers[0].contains("a branch that is not a bookmark here builds on it"));

        // Trunk has not moved, and "top" was amended here since it was
        // pushed: the ship would push the amended commit, and theirs is
        // built on the one it replaces.
        let (mut snap, prs) = published_pair();
        snap.bookmarks[2].sync = SyncState::Diverged;
        let plan = plan_ship(&snap, Some(&with_theirs(&prs)), "origin", "b2").unwrap();
        assert_eq!(plan.blockers.len(), 1, "{:?}", plan.blockers);
        assert!(
            plan.blockers[0].starts_with(
                "\u{201c}top\u{201d} was rewritten here since it was last pushed, and #9 by \
                 @kim rests on it"
            ) && plan.blockers[0].ends_with(ends),
            "{:?}",
            plan.blockers
        );
        // A pull request from a fork counts like anyone's while the
        // rewrite is here and not yet published: the bookmark says that
        // the branch was rewritten, whoever rests on it. (Once it is
        // published nothing here says so for a fork, whose branch is
        // never fetched; that is a stated limit.)
        let from_a_fork = {
            let mut report = prs.report.clone();
            report.prs.push(PrSummary {
                base_branch: "top".into(),
                author: Some("kim".into()),
                head_owner: Some("a-fork".into()),
                ..open_pr(9, "their-top")
            });
            prs_with(report.prs)
        };
        let plan = plan_ship(&snap, Some(&from_a_fork), "origin", "b2").unwrap();
        assert_eq!(plan.blockers.len(), 1, "{:?}", plan.blockers);
        assert!(
            plan.blockers[0].contains("and #9 by @kim rests on it, someone else's"),
            "{:?}",
            plan.blockers
        );
        // The same amendment with nobody else's pull request on it ships.
        let plan = plan_ship(&snap, Some(&prs), "origin", "b2").unwrap();
        assert!(plan.blockers.is_empty(), "{:?}", plan.blockers);
        // And with theirs on the branch below the amended one: nothing
        // under it is replaced, and it ends showing only its own.
        let on_bottom = |prs: &RepoPrState| {
            let mut report = prs.report.clone();
            report.prs.push(PrSummary {
                base_branch: "bottom".into(),
                author: Some("sam".into()),
                ..open_pr(10, "their-mid")
            });
            prs_with(report.prs)
        };
        let plan = plan_ship(&snap, Some(&on_bottom(&prs)), "origin", "b2").unwrap();
        assert!(plan.blockers.is_empty(), "{:?}", plan.blockers);
        // Both there: only the one on the amended branch is named.
        let plan = plan_ship(&snap, Some(&on_bottom(&with_theirs(&prs))), "origin", "b2").unwrap();
        assert_eq!(plan.blockers.len(), 1, "{:?}", plan.blockers);
        assert!(
            plan.blockers[0].contains("and #9 by @kim rests on it, someone else's")
                && !plan.blockers[0].contains("#10"),
            "{:?}",
            plan.blockers
        );
        // An amended "bottom" takes "top" with it: both are named, and
        // so is one that rests on theirs.
        snap.bookmarks[1].sync = SyncState::Diverged;
        let mut all = on_bottom(&with_theirs(&prs)).report.prs;
        all.push(PrSummary {
            base_branch: "their-top".into(),
            author: Some("lee".into()),
            ..open_pr(11, "on-theirs")
        });
        let plan = plan_ship(&snap, Some(&prs_with(all)), "origin", "b2").unwrap();
        assert_eq!(plan.blockers.len(), 1, "{:?}", plan.blockers);
        assert!(
            plan.blockers[0].starts_with(
                "\u{201c}bottom\u{201d} was rewritten here since it was last pushed, and #10 \
                 by @sam, #9 by @kim and #11 by @lee rest on it, someone else's. Pointed at \
                 \u{201c}main\u{201d} after such a ship, they would show the replaced commits \
                 as their own, and nothing here pushes to their branches."
            ),
            "{:?}",
            plan.blockers
        );
        snap.bookmarks[1].sync = SyncState::Synced;
        // A rewritten branch that has no pull request of its own is not
        // pushed by the ship, and trunk gets its new commits all the
        // same: theirs on it is named too.
        let alone = prs_with(vec![prs.report.prs[0].clone()]);
        let plan = plan_ship(&snap, Some(&with_theirs(&alone)), "origin", "b2").unwrap();
        assert_eq!(plan.blockers.len(), 1, "{:?}", plan.blockers);
        assert!(
            plan.blockers[0].starts_with(
                "\u{201c}top\u{201d} was rewritten here since it was last pushed, and #9 by \
                 @kim rests on it"
            ),
            "{:?}",
            plan.blockers
        );
        // And commits only added on top replace nothing under theirs.
        snap.bookmarks[2].sync = SyncState::Ahead;
        let plan = plan_ship(&snap, Some(&with_theirs(&prs)), "origin", "b2").unwrap();
        assert!(plan.blockers.is_empty(), "{:?}", plan.blockers);

        // The amendment was published first, so nothing is pushed now.
        // Their branch, fetched, still holds the commit it was built on:
        // the change reads as having an old copy someone else keeps.
        let (mut snap, prs) = published_pair();
        snap.nodes[0].is_divergent = true;
        snap.nodes[0].old_copies_held = true;
        let plan = plan_ship(&snap, Some(&with_theirs(&prs)), "origin", "b2").unwrap();
        assert_eq!(plan.blockers.len(), 1, "{:?}", plan.blockers);
        assert!(
            plan.blockers[0].starts_with(
                "b2 was rewritten after a branch that is no bookmark here was built on it, and \
                 #9 by @kim rests on it"
            ) && plan.blockers[0].ends_with(ends),
            "{:?}",
            plan.blockers
        );

        // Held, and on current trunk, with nothing replaced: it ships.
        let (mut snap, _) = published_pair();
        for node in snap.nodes.iter_mut().take(2) {
            node.kind = NodeKind::Immutable;
            node.held = true;
        }
        let plan = plan_ship(&snap, None, "origin", "b2").unwrap();
        assert!(plan.blockers.is_empty(), "{:?}", plan.blockers);
        assert_eq!(told(&plan), ["move trunk to b2", "push trunk"]);
    }

    /// A bookmark of the shipped work was moved onto someone else's
    /// commits. Their pull request's head is then among the changes that
    /// ship, and the first push that contains it makes GitHub mark their
    /// pull request merged: into the branch below it, before trunk has
    /// moved at all. A push cannot be put back, so the plan stops before
    /// it, as a publish does. (Found in review.)
    #[test]
    fn a_pull_request_whose_head_is_among_the_shipped_commits_stops_the_ship() {
        let (mut snap, prs) = published_pair();
        // "bottom" sits on #9's head now: their commit, with ours on top.
        snap.nodes[1].commit_id = "c0ffee09".into();
        snap.bookmarks[1].sync = SyncState::Ahead;
        let mut report = prs.report.clone();
        report.prs.push(PrSummary {
            base_branch: "bottom".into(),
            head_commit: "c0ffee09".into(),
            author: Some("kim".into()),
            ..open_pr(9, "their-fix")
        });
        let plan = plan_ship(&snap, Some(&prs_with(report.prs)), "origin", "b2").unwrap();
        assert!(
            plan.blockers.contains(
                &"#9 by @kim (\u{201c}their-fix\u{201d}) has its head among the changes this \
                  ships: a bookmark of this work was moved onto its commits. Shipping would put \
                  that head inside \u{201c}bottom\u{201d}, and GitHub would mark the pull request \
                  merged \u{2014} move the bookmark back off those commits, or take the pull \
                  request over (take-over 9) if it is yours to ship"
                    .to_owned()
            ),
            "{:?}",
            plan.blockers
        );

        // The same when their head comes in beside the chain, as the
        // second parent of a merge: the push of "top" carries it, and
        // puts it inside "bottom" all the same. (Found in review: the
        // look went along first parents only.)
        let (mut snap, prs) = published_pair();
        snap.nodes[0].parents.push("s1".into());
        snap.nodes.insert(
            2,
            node("s1", "theirs: a fix", NodeKind::Mutable, &["b1"], &[]),
        );
        snap.nodes[2].commit_id = "c0ffee09".into();
        let theirs = |prs: &RepoPrState| {
            let mut report = prs.report.clone();
            report.prs.push(PrSummary {
                base_branch: "bottom".into(),
                head_commit: "c0ffee09".into(),
                author: Some("kim".into()),
                ..open_pr(9, "their-fix")
            });
            prs_with(report.prs)
        };
        let plan = plan_ship(&snap, Some(&theirs(&prs)), "origin", "b2").unwrap();
        assert!(
            plan.blockers.contains(
                &"#9 by @kim (\u{201c}their-fix\u{201d}) has its head among the changes this \
                  ships: a merge in this work brings its commits in. Shipping would put that \
                  head inside \u{201c}bottom\u{201d}, and GitHub would mark the pull request \
                  merged \u{2014} take that merge out of this work, or take the pull request \
                  over (take-over 9) if it is yours to ship"
                    .to_owned()
            ),
            "{:?}",
            plan.blockers
        );
        // Their commit counts whatever jj calls it: once their branch has
        // been fetched, it is immutable here.
        snap.nodes[2].kind = NodeKind::Immutable;
        let plan = plan_ship(&snap, Some(&theirs(&prs)), "origin", "b2").unwrap();
        assert!(
            plan.blockers.iter().any(|b| b.starts_with("#9 by @kim")
                && b.contains("a merge in this work brings its commits in")),
            "{:?}",
            plan.blockers
        );
        // From a fork, the pull request cannot be taken over, and the
        // refusal does not offer that.
        let from_a_fork = {
            let mut report = theirs(&prs).report;
            report.prs.last_mut().unwrap().head_owner = Some("kim".into());
            prs_with(report.prs)
        };
        let plan = plan_ship(&snap, Some(&from_a_fork), "origin", "b2").unwrap();
        assert!(
            plan.blockers
                .iter()
                .any(|b| b.starts_with("#9 by @kim") && b.ends_with("take that merge out of this work")),
            "{:?}",
            plan.blockers
        );
        // A pull request from that commit into a branch this ship does not
        // push is none of its business: nothing here puts its head inside
        // its base.
        let elsewhere = {
            let mut report = prs.report.clone();
            report.prs.push(PrSummary {
                base_branch: "release".into(),
                head_commit: "c0ffee09".into(),
                author: Some("kim".into()),
                ..open_pr(9, "their-fix")
            });
            prs_with(report.prs)
        };
        let plan = plan_ship(&snap, Some(&elsewhere), "origin", "b2").unwrap();
        assert!(plan.blockers.is_empty(), "{:?}", plan.blockers);
        // A branch of the user's own that was merged in is theirs to ship:
        // with "their-fix" a bookmark here, nothing stops.
        snap.nodes[2].kind = NodeKind::Mutable;
        snap.nodes[2].bookmarks = vec!["their-fix".into()];
        snap.bookmarks
            .push(bookmark("their-fix", "s1", SyncState::Synced, false));
        let plan = plan_ship(&snap, Some(&theirs(&prs)), "origin", "b2").unwrap();
        assert!(plan.blockers.is_empty(), "{:?}", plan.blockers);
    }

    /// A shipped branch need not have a pull request of its own for
    /// someone else's to be based on it. (Found in review: theirs was
    /// looked for only on the branches of the shipped pull requests.)
    #[test]
    fn a_pull_request_on_a_shipped_branch_that_has_none_of_its_own_is_seen() {
        let (snap, prs) = published_pair();
        // "top" is pushed and has no pull request; #9 is based on it.
        let mut report = prs.report.clone();
        report.prs.truncate(1);
        report.prs.push(PrSummary {
            base_branch: "top".into(),
            author: Some("kim".into()),
            ..open_pr(9, "their-top")
        });
        let plan = plan_ship(&snap, Some(&prs_with(report.prs)), "origin", "b2").unwrap();
        assert!(plan.blockers.is_empty(), "{:?}", plan.blockers);
        assert_eq!(
            told(&plan),
            ["move trunk to b2", "push trunk", "confirm [7]", "above #9 by kim -> main"]
        );
    }

    /// GitHub answers a hundred open pull requests at most. Past that, a
    /// pull request of the shipped work can be missing from the answer
    /// (it would be left open, as every upper one used to be), and so can
    /// someone else's on a shipped branch. Ship does not guess: published
    /// work is refused, and work with no branch on GitHub still ships.
    #[test]
    fn a_list_of_pull_requests_that_is_cut_short_stops_a_ship_of_published_work() {
        let (snap, prs) = published_pair();
        // Only the lower pull request is in the answer.
        let mut report = prs.report.clone();
        report.prs.truncate(1);
        report.truncated = true;
        let cut = RepoPrState::new(report, "o");
        let plan = plan_ship(&snap, Some(&cut), "origin", "b2").unwrap();
        assert_eq!(plan.blockers.len(), 1, "{:?}", plan.blockers);
        assert!(
            plan.blockers[0].starts_with(
                "GitHub answered only the 100 most recently updated open pull requests, so \
                 Ship cannot tell which are open for this work or rest on it"
            ),
            "{:?}",
            plan.blockers
        );
        // Nothing of this work was ever pushed: there is no pull request
        // to miss.
        let unpublished = stack_on_trunk();
        let none = RepoPrState::new(PrStateReport { prs: vec![], truncated: true }, "o");
        let plan = plan_ship(&unpublished, Some(&none), "origin", "b2").unwrap();
        assert!(plan.blockers.is_empty(), "{:?}", plan.blockers);
    }

    /// After the push the work is in. What GitHub says of each pull
    /// request is told as it is, and is never a failed ship.
    #[test]
    fn the_look_after_the_push_tells_each_pull_request_as_github_has_it() {
        let (snap, prs) = published_pair();
        let plan = plan_ship(&snap, Some(&prs), "origin", "b2").unwrap();
        let confirmed = |hub: Hub| {
            let vcs = Vcs {
                snap: snap.clone(),
                push_fails: false,
                calls: RefCell::new(vec![]),
            };
            let outcome = execute_ship(&plan, &vcs, Some(&hub)).unwrap();
            assert!(!outcome.failed, "{:?}", outcome.steps);
            outcome.steps.last().unwrap().detail.clone().unwrap()
        };
        let mut slow = Hub::default();
        slow.states.insert(8, PrState::Open);
        assert_eq!(
            confirmed(slow),
            "GitHub marked #7 merged; #8 is still open: GitHub has not marked it merged yet; \
             look again in a moment"
        );
        let unreachable = Hub {
            await_fails: true,
            ..Hub::default()
        };
        assert_eq!(
            confirmed(unreachable),
            "could not ask GitHub whether #7 and #8 are merged (Could not reach GitHub: \
             offline); look there"
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

    /// GitHub as a ship asks it, scripted: what it was asked, in order,
    /// and what it refuses.
    #[derive(Default)]
    struct Hub {
        calls: RefCell<Vec<String>>,
        /// Retargets GitHub refuses: the pull request and the base.
        refuse: Vec<(u64, String)>,
        /// What the look after the push finds; merged unless said here.
        states: HashMap<u64, PrState>,
        await_fails: bool,
        stacks: RefCell<u64>,
        /// The commit GitHub has trunk at when asked after a failed push:
        /// `None` is the old trunk, `Some(Ok)` that commit, `Some(Err)`
        /// no answer.
        trunk_at: Option<Result<String, String>>,
        /// Retargets GitHub applies and then answers with an error.
        loses_answer: Vec<(u64, String)>,
        /// The dissolve takes the rest out and keeps these.
        keeps_stacked: Vec<u64>,
        /// The pull requests' bases as these calls leave them.
        bases: RefCell<HashMap<u64, String>>,
    }

    impl ShipForge for Hub {
        fn update_pr_base(&self, number: u64, base: &str) -> Result<(), ForgeError> {
            if self.refuse.iter().any(|(n, b)| *n == number && b == base) {
                return Err(ForgeError::Api("HTTP 422: refused".into()));
            }
            self.calls.borrow_mut().push(format!("retarget:{number}:{base}"));
            self.bases.borrow_mut().insert(number, base.to_owned());
            if self.loses_answer.iter().any(|(n, b)| *n == number && b == base) {
                return Err(ForgeError::Network("the connection dropped".into()));
            }
            Ok(())
        }
        fn unstack(&self, stack: u64) -> Result<Vec<u64>, ForgeError> {
            self.calls.borrow_mut().push(format!("unstack:{stack}"));
            Ok(self.keeps_stacked.clone())
        }
        fn branch_head(&self, _branch: &str) -> Result<String, ForgeError> {
            match &self.trunk_at {
                None => Ok("0ldtrunk".into()),
                Some(Ok(commit)) => Ok(commit.clone()),
                Some(Err(why)) => Err(ForgeError::Network(why.clone())),
            }
        }
        fn create_stack(&self, pull_requests: &[u64]) -> Result<u64, ForgeError> {
            self.calls.borrow_mut().push(format!("stack:{pull_requests:?}"));
            *self.stacks.borrow_mut() += 1;
            Ok(90 + *self.stacks.borrow())
        }
        fn await_merged(&self, numbers: &[u64]) -> Result<Vec<(u64, PrState)>, ForgeError> {
            if self.await_fails {
                return Err(ForgeError::Network("offline".into()));
            }
            self.calls.borrow_mut().push(format!("await:{numbers:?}"));
            Ok(numbers
                .iter()
                .map(|n| (*n, self.states.get(n).copied().unwrap_or(PrState::Merged)))
                .collect())
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
        let outcome = execute_ship(&full_plan(), &vcs, None).unwrap();
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
        let outcome = execute_ship(&full_plan(), &vcs, None).unwrap();
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
        let outcome = execute_ship(&plan, &vcs, None).unwrap();
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
        assert!(execute_ship(&plan, &vcs, None).is_err());
        assert!(vcs.calls.borrow().is_empty());
    }

    /// The published pair, with #8 on top by kim and taken over.
    fn pair_with_taken_over() -> (RepoSnapshot, RepoPrState) {
        let (mut snap, prs) = published_pair();
        snap.taken_over = vec![jiji_core::snapshot::TakenOver {
            bookmark: "top".into(),
            remote: "origin".into(),
            number: 8,
            author: Some("kim".into()),
        }];
        let mut open = prs.report.prs;
        open[1].author = Some("kim".into());
        (snap, prs_with(open))
    }

    /// The remote refuses the first push it sees, in the backend's words
    /// for a branch that moved since the last fetch.
    struct Leased(RepoSnapshot);

    impl ShipVcs for Leased {
        fn snapshot(&self) -> Result<RepoSnapshot, BackendError> {
            Ok(self.0.clone())
        }
        fn rebase_onto_trunk(&self, root_change: &str) -> Result<String, BackendError> {
            Ok(format!("Rebased {root_change} onto trunk"))
        }
        fn move_bookmark(&self, name: &str, to_change: &str) -> Result<String, BackendError> {
            Ok(format!("Moved {name} to {to_change}"))
        }
        fn push_bookmarks(&self, bookmarks: &[String], _: &str) -> Result<String, BackendError> {
            Err(BackendError::MutationFailed(format!(
                "{} moved on the remote since the last fetch; fetch, reconcile, and push again",
                bookmarks.join(", ")
            )))
        }
        fn new_change(&self, parent_change: &str) -> Result<String, BackendError> {
            Ok(format!("Started a new change on {parent_change}"))
        }
    }

    /// A pull request that was taken over ships with the work like one
    /// of the user's own, and the plan says whose it is. When the ship
    /// first has to push its branch, the plan names that push, and a
    /// push the remote refuses because its author pushed again says so.
    #[test]
    fn a_taken_over_pull_request_ships_with_the_work_and_says_whose_it_is() {
        let about = |plan: &ShipPlan| -> Vec<String> {
            plan.warnings.iter().filter(|w| w.contains("taken over")).cloned().collect()
        };
        // In step with GitHub: it ships, and nothing of its branch moves.
        let (snap, prs) = pair_with_taken_over();
        let plan = plan_ship(&snap, Some(&prs), "origin", "b2").unwrap();
        assert!(plan.blockers.is_empty(), "{:?}", plan.blockers);
        assert_eq!(told(&plan).last().map(String::as_str), Some("confirm [7, 8]"));
        assert_eq!(
            about(&plan),
            ["#8 by @kim was taken over, and ships with this work like a pull request of your own"]
        );

        // Changed here since: its branch is pushed first, and the plan
        // says whose branch that is.
        let (mut snap, prs) = pair_with_taken_over();
        snap.bookmarks[2].sync = SyncState::Ahead;
        let plan = plan_ship(&snap, Some(&prs), "origin", "b2").unwrap();
        assert_eq!(told(&plan)[0], "push branches top");
        assert_eq!(
            about(&plan),
            [
                "#8 by @kim was taken over, and ships with this work like a pull request of your own",
                "This pushes \u{201c}top\u{201d} (#8 by @kim, taken over): a branch someone else \
                 made. The push goes through only if the branch is where the last fetch saw it, \
                 so nothing pushed there since is overwritten",
            ]
        );
        // Refused, because its author pushed in the meantime.
        let outcome = execute_ship(&plan, &Leased(snap.clone()), Some(&Hub::default())).unwrap();
        assert!(outcome.failed);
        assert_eq!(
            outcome.steps[0].detail.as_deref(),
            Some(
                "Could not apply the change: top moved on the remote since the last fetch; \
                 fetch, reconcile, and push again. \u{201c}top\u{201d} is #8 by @kim's branch, \
                 taken over: it was pushed to again since the last fetch, and nothing there was \
                 overwritten. Fetch: the new commits arrive beside yours, and \u{201c}top\u{201d} \
                 reads as conflicted until you have brought the two together (rebase theirs onto \
                 yours, or take theirs) and pointed it at the result. Then ship again"
            )
        );

        // The user's own pair says none of it.
        let (mut snap, prs) = published_pair();
        snap.bookmarks[2].sync = SyncState::Ahead;
        let plan = plan_ship(&snap, Some(&prs), "origin", "b2").unwrap();
        assert!(about(&plan).is_empty(), "{:?}", plan.warnings);
        let outcome = execute_ship(&plan, &Leased(snap), Some(&Hub::default())).unwrap();
        assert_eq!(
            outcome.steps[0].detail.as_deref(),
            Some(
                "Could not apply the change: top moved on the remote since the last fetch; \
                 fetch, reconcile, and push again"
            )
        );
    }
}
