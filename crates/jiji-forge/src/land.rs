//! The land engine: recognize what already merged, merge the bottom PR
//! when it is ready, then reconcile the local stack — jjpr's bottom-up
//! merge → fetch → reconcile loop as an explicit plan → confirm → execute
//! flow.
//!
//! The shape mirrors [`crate::submit`]: `plan_land` derives everything the
//! run will do before anything runs — the merge (or the hand-off to
//! GitHub's own automation), the fetch that brings the new trunk home, the
//! descendant rebases, the batched push of the rebased stack, the base
//! retarget of the next PR, and the cleanup of the landed bookmark and its
//! now-redundant local changes. `execute_land` walks that plan, re-checking
//! GitHub just-in-time before the merge and adapting the cleanup steps to
//! what the refreshed snapshot actually shows (GitHub may have deleted the
//! branch on merge; a merge-commit landing turns the local changes into
//! trunk ancestry that must not be abandoned).
//!
//! Landing is deliberately one merge round per run: after the bottom PR
//! merges and the stack above rebases, the next PR's checks are freshly
//! re-running, so a second merge in the same run would almost always be
//! against stale CI. Re-running Land *is* the continue flow — the same
//! idempotence rule as re-running submit — and [`crate::autoland`] is that
//! re-run automated. Where the repo offers landing automation (auto-merge,
//! a merge queue), the plan prefers enabling and supervising it over
//! driving the merge locally.
//!
//! The user steers a run through [`LandOptions`]: a merge method to
//! insist on (the default picks squash, then rebase, then a merge
//! commit, from what the repo allows) and a bypass switch that merges
//! right through waits and soft blockers — pending or failing checks, a
//! required review, requested changes, the merge queue — which GitHub
//! honors only when the token's user may bypass the branch's
//! protections. A draft candidate no longer blocks: the plan marks it
//! ready for review as its own confirmable step. After a merge
//! reconciles, the plan arms auto-merge on the next PR up (one level
//! only — higher PRs still point at branches about to be rewritten), so
//! GitHub continues the stack while the checks re-run.
//!
//! Nothing is taken on GitHub's word that a merge landed. GitHub marks a
//! pull request merged whenever its head turns up inside its base branch,
//! whatever that branch is, so "merged" can mean "pushed under another
//! branch of the same stack" or "merged into the branch below"; and a
//! branch name can have had an earlier pull request. So before a run
//! rebases, pushes, deletes or abandons anything on the strength of a
//! merge, it asks the repository ([`LandVcs::landed`]): is the commit
//! GitHub names for that merge part of trunk here, and does it hold the
//! segment's changes as they are here? A no stops the run at that step
//! with the reason, and the plan says the same before anything runs when
//! the repository already knows the answer.
//!
//! A stack GitHub itself knows as one (its stacked pull requests, linked
//! by [`crate::submit`] or by anyone on github.com) lands through the
//! asynchronous merge endpoint, the only one GitHub accepts for it.
//! There a merge takes every open pull request below the one asked for
//! in the same request, all or none, so a round on such a stack merges
//! every pull request that is ready from the bottom up in one go, and
//! GitHub rebases and pushes what is left. The reconcile is the same one:
//! its rebase and push find nothing to do when GitHub got there first.
//! GitHub offers no auto-merge on stacked pull requests, so waits are
//! reported as waits and [`crate::autoland`] is what sits through them.
//!
//! Deliberately deferred: merge-queue supervision (enqueue is
//! fire-and-report), and stack-comment fossilization during land (the
//! next submit heals comments).

use jiji_core::snapshot::{
    GraphNode, LandedCheck, RepoSnapshot, SyncState, TipCheck,
};
use jiji_core::BackendError;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::ForgeError;
use crate::github::{AsyncMerge, Restack};
use crate::pr::{
    parse_graphql_seat, ChecksRollup, PrState, PrSummary, RepoPrState, ReviewDecision, StackSeat,
};
use crate::remote::ForgeRepo;
use crate::submit::{stack_segments, SubmitStepStatus};

/// How a PR merges into its base. GitHub's three; when several are allowed
/// the plan prefers squash (jjpr's default and the stacked-PR convention),
/// then rebase, then a merge commit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum MergeMethod {
    Squash,
    Rebase,
    Merge,
}

impl MergeMethod {
    pub fn rest_name(self) -> &'static str {
        match self {
            MergeMethod::Squash => "squash",
            MergeMethod::Rebase => "rebase",
            MergeMethod::Merge => "merge",
        }
    }

    pub fn graphql_name(self) -> &'static str {
        match self {
            MergeMethod::Squash => "SQUASH",
            MergeMethod::Rebase => "REBASE",
            MergeMethod::Merge => "MERGE",
        }
    }

    /// How the method reads in a sentence.
    pub fn label(self) -> &'static str {
        match self {
            MergeMethod::Squash => "squash",
            MergeMethod::Rebase => "rebase",
            MergeMethod::Merge => "merge-commit",
        }
    }
}

fn default_merge_method() -> MergeMethod {
    MergeMethod::Squash
}

/// How the user asked this land to run. `method` insists on one of
/// GitHub's merge methods (`None` picks squash, then rebase, then a merge
/// commit, from what the repo allows). `bypass` merges right through
/// waits and soft blockers — pending or failing checks, a required
/// review, requested changes, the merge queue — which GitHub honors only
/// when the token's user may bypass the branch's protections; hard
/// impossibilities (a closed PR, merge conflicts, a wrong base) still
/// block. The options ride on the plan and on an auto-land job's record,
/// so every re-derivation and just-in-time re-check judges by the same
/// rules the user confirmed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LandOptions {
    #[serde(default)]
    pub method: Option<MergeMethod>,
    #[serde(default)]
    pub bypass: bool,
}

/// GitHub's `mergeStateStatus` — the merge button's own state, which is
/// the only rollup that distinguishes *required* conditions from optional
/// ones. `Unknown` also stands in for answers that omit the field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MergeStateStatus {
    Clean,
    /// Mergeable with non-required checks failing.
    Unstable,
    HasHooks,
    Behind,
    Blocked,
    Dirty,
    Draft,
    #[default]
    Unknown,
}

impl MergeStateStatus {
    /// GitHub would merge this PR right now: every *required* condition
    /// is met, whatever optional checks are doing.
    pub fn merges_now(self) -> bool {
        matches!(
            self,
            MergeStateStatus::Clean | MergeStateStatus::Unstable | MergeStateStatus::HasHooks
        )
    }
}

/// GitHub's per-PR mergeability: whether the PR applies cleanly to its
/// base. `Unknown` means GitHub is still computing it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mergeable {
    Mergeable,
    Conflicting,
    Unknown,
}

/// One PR's land readiness plus the repo's landing capabilities — the
/// answer to [`crate::github::GitHubClient::pr_land_state`], asked for the
/// landing candidate only (mergeability is computed lazily by GitHub).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrLandState {
    /// GraphQL node id — what the auto-merge and enqueue mutations take.
    pub node_id: String,
    pub state: PrState,
    pub is_draft: bool,
    pub mergeable: Mergeable,
    /// The merge button's own state — how required-vs-optional checks are
    /// told apart (the checks rollup counts every check).
    pub merge_state: MergeStateStatus,
    pub review: ReviewDecision,
    pub checks: ChecksRollup,
    pub base_branch: String,
    pub head_commit: String,
    /// The commit GitHub recorded as the merge, once merged. See
    /// [`PrSummary::merge_commit`] for what it can be.
    pub merge_commit: Option<String>,
    /// The PR's seat in a GitHub stack right now. Read fresh because
    /// anyone can link or dissolve a stack on github.com at any moment,
    /// and it decides which merge endpoint GitHub accepts.
    pub stack: Option<StackSeat>,
    /// Auto-merge is already enabled on this PR.
    pub auto_merge_enabled: bool,
    pub in_merge_queue: bool,
    /// The repo allows enabling auto-merge at all.
    pub auto_merge_allowed: bool,
    /// The queried base branch is protected by a merge queue.
    pub queue_on_base: bool,
    /// The merge method owned by that queue's configuration. Unlike a
    /// direct or auto-merge landing, enqueueing cannot override it.
    pub queue_method: Option<MergeMethod>,
    pub allows_squash: bool,
    pub allows_merge: bool,
    pub allows_rebase: bool,
}

impl PrLandState {
    /// The branch this PR lands on; see [`PrSummary::landing_branch`].
    pub fn landing_branch(&self) -> &str {
        self.stack
            .as_ref()
            .map_or(self.base_branch.as_str(), |seat| seat.base.as_str())
    }

    /// The branch a merged PR's commits went into; see
    /// [`PrSummary::merged_into`].
    pub fn merged_into(&self) -> &str {
        if self.merge_commit.as_deref() == Some(self.head_commit.as_str()) {
            &self.base_branch
        } else {
            self.landing_branch()
        }
    }
}

/// Reshape the [`crate::github::PR_LAND_QUERY`] answer.
pub fn parse_pr_land_state(data: &Value, number: u64) -> Result<PrLandState, ForgeError> {
    let malformed = |what: &str| ForgeError::Api(format!("unexpected GitHub answer: {what}"));
    let repository = data
        .get("repository")
        .filter(|v| !v.is_null())
        .ok_or_else(|| {
            ForgeError::NotFound("the repository is not visible to this token".to_owned())
        })?;
    let pr = repository
        .get("pullRequest")
        .filter(|v| !v.is_null())
        .ok_or_else(|| ForgeError::NotFound(format!("pull request #{number} was not found")))?;
    let state = match pr["state"].as_str() {
        Some("OPEN") => PrState::Open,
        Some("MERGED") => PrState::Merged,
        Some("CLOSED") => PrState::Closed,
        other => {
            return Err(malformed(&format!(
                "pull request #{number} has unexpected state {other:?}"
            )))
        }
    };
    let mergeable = match pr["mergeable"].as_str() {
        Some("MERGEABLE") => Mergeable::Mergeable,
        Some("CONFLICTING") => Mergeable::Conflicting,
        _ => Mergeable::Unknown,
    };
    let merge_state = match pr["mergeStateStatus"].as_str() {
        Some("CLEAN") => MergeStateStatus::Clean,
        Some("UNSTABLE") => MergeStateStatus::Unstable,
        Some("HAS_HOOKS") => MergeStateStatus::HasHooks,
        Some("BEHIND") => MergeStateStatus::Behind,
        Some("BLOCKED") => MergeStateStatus::Blocked,
        Some("DIRTY") => MergeStateStatus::Dirty,
        Some("DRAFT") => MergeStateStatus::Draft,
        _ => MergeStateStatus::Unknown,
    };
    let review = match pr["reviewDecision"].as_str() {
        Some("APPROVED") => ReviewDecision::Approved,
        Some("CHANGES_REQUESTED") => ReviewDecision::ChangesRequested,
        Some("REVIEW_REQUIRED") => ReviewDecision::ReviewRequired,
        _ => ReviewDecision::None,
    };
    let checks = match pr["commits"]["nodes"][0]["commit"]["statusCheckRollup"]["state"].as_str() {
        Some("SUCCESS") => ChecksRollup::Passing,
        Some("FAILURE") | Some("ERROR") => ChecksRollup::Failing,
        Some("PENDING") | Some("EXPECTED") => ChecksRollup::Pending,
        Some(_) => ChecksRollup::Pending,
        None => ChecksRollup::None,
    };
    Ok(PrLandState {
        node_id: pr["id"]
            .as_str()
            .ok_or_else(|| malformed(&format!("pull request #{number} missing id")))?
            .to_owned(),
        state,
        is_draft: pr["isDraft"].as_bool().unwrap_or(false),
        mergeable,
        merge_state,
        review,
        checks,
        base_branch: pr["baseRefName"]
            .as_str()
            .ok_or_else(|| malformed(&format!("pull request #{number} missing baseRefName")))?
            .to_owned(),
        head_commit: pr["headRefOid"]
            .as_str()
            .ok_or_else(|| malformed(&format!("pull request #{number} missing headRefOid")))?
            .to_owned(),
        merge_commit: pr["mergeCommit"]["oid"].as_str().map(str::to_owned),
        stack: parse_graphql_seat(pr),
        auto_merge_enabled: !pr["autoMergeRequest"].is_null(),
        in_merge_queue: pr["isInMergeQueue"].as_bool().unwrap_or(false),
        auto_merge_allowed: repository["autoMergeAllowed"].as_bool().unwrap_or(false),
        queue_on_base: !repository["mergeQueue"].is_null(),
        queue_method: match repository["mergeQueue"]["configuration"]["mergeMethod"].as_str() {
            Some("SQUASH") => Some(MergeMethod::Squash),
            Some("REBASE") => Some(MergeMethod::Rebase),
            Some("MERGE") => Some(MergeMethod::Merge),
            _ => None,
        },
        allows_squash: repository["squashMergeAllowed"].as_bool().unwrap_or(true),
        allows_merge: repository["mergeCommitAllowed"].as_bool().unwrap_or(true),
        allows_rebase: repository["rebaseMergeAllowed"].as_bool().unwrap_or(true),
    })
}

/// Where a segment stands in the landing story.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum LandSegmentStatus {
    /// GitHub already merged this segment's PR; this run reconciles it.
    #[serde(rename_all = "camelCase")]
    Merged { number: u64, url: String },
    /// The PR this run merges — or hands to GitHub's automation.
    Landing,
    /// The bottom-most unmerged segment, and it cannot land yet; the
    /// plan's blockers carry the reasons.
    Waiting,
    /// Above the landing point: lands on a later run, once the stack below
    /// it is in and its own checks have re-run.
    Stacked,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LandSegment {
    pub bookmark: String,
    /// Change ids in the segment, bottom-first.
    pub change_ids: Vec<String>,
    /// The open PR GitHub has for this bookmark, when one exists.
    pub pr: Option<PrSummary>,
    pub status: LandSegmentStatus,
}

/// A pull request that lands in the same request as the one above it in a
/// GitHub stack.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LandsWith {
    pub number: u64,
    pub bookmark: String,
    /// The head the plan read. Execution refuses a different one.
    pub expected_head: String,
}

/// One landing action, in execution order.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum LandAction {
    /// Mark a draft PR ready for review — a draft cannot merge, enqueue,
    /// or arm auto-merge, and landing it means publishing it.
    #[serde(rename_all = "camelCase")]
    MarkReadyPr { number: u64, bookmark: String },
    /// Merge the PR now. `expected_head` rides along as GitHub's own
    /// lease — the merge refuses if the PR's head moved since planning.
    /// A squash merge carries its trunk commit's title and body, composed
    /// from the PR and the segment's change descriptions, so carefully
    /// described local work lands as carefully described history instead
    /// of GitHub's commit-list default.
    ///
    /// In a GitHub stack the merge takes the pull requests below with it:
    /// `with` lists them, bottom first, each with the head it must still
    /// have. They land in the same request, all or none, each as its own
    /// commit; the title and message here shape `number`'s commit only.
    /// `stack` is the stack's number as planned, so execution can tell a
    /// stack that was linked, dissolved or rebuilt in between.
    #[serde(rename_all = "camelCase")]
    MergePr {
        number: u64,
        bookmark: String,
        method: MergeMethod,
        expected_head: String,
        commit_title: Option<String>,
        commit_message: Option<String>,
        #[serde(default)]
        with: Vec<LandsWith>,
        #[serde(default)]
        stack: Option<u64>,
    },
    /// After a merge in a GitHub stack, GitHub rebases the pull requests
    /// left above and pushes their branches itself, a moment after it
    /// reports the merge. Wait for `number`'s head to move off `from_head`
    /// before fetching, so the fetch brings GitHub's commits instead of
    /// racing them. When GitHub says the rebase would conflict, it leaves
    /// the branch alone and the rebase happens here, where the conflict
    /// can be resolved.
    ///
    /// `merged_here` says this run made the merge. Then a branch GitHub
    /// has neither moved nor called conflicting by the end of the wait
    /// stops the run: GitHub is about to move it, and a rebase and push
    /// from here would leave two copies of the change. A later run that
    /// finds the merge already made and the branch still unmoved takes
    /// that as GitHub leaving it alone, and rebases here.
    #[serde(rename_all = "camelCase")]
    AwaitRestack {
        number: u64,
        bookmark: String,
        from_head: String,
        #[serde(default)]
        merged_here: bool,
    },
    /// The gate behind every merge: ask the repository whether the commit
    /// GitHub names as `number`'s merge is part of trunk and holds the
    /// segment's changes, from `bottom_commit` up to `top_commit` as they
    /// were when the plan was made. Anything else stops the run here,
    /// before a rebase, a push, or a cleanup can build on a merge that
    /// did not land.
    #[serde(rename_all = "camelCase")]
    CheckLanded {
        number: u64,
        bookmark: String,
        bottom_commit: String,
        top_commit: String,
    },
    /// The PR is waiting only on conditions GitHub automates on (checks,
    /// required reviews): enable auto-merge and let GitHub finish. The
    /// reconcile happens on a later Land run, once the merge is real.
    /// `expected_head: Some` is the candidate head read at plan time;
    /// `None` is reserved for a chained hand-off after this plan rebases
    /// and pushes the bookmark, so the executor resolves and leases that
    /// resulting commit just before the mutation.
    #[serde(rename_all = "camelCase")]
    EnableAutoMerge {
        number: u64,
        bookmark: String,
        method: MergeMethod,
        #[serde(default)]
        expected_head: Option<String>,
        #[serde(default)]
        commit_title: Option<String>,
        #[serde(default)]
        commit_message: Option<String>,
    },
    /// The base branch is protected by a merge queue — the only way to
    /// land is to enqueue and let GitHub drive. The queue owns `method`;
    /// carrying it makes the actual semantics visible and lets execution
    /// detect a configuration change before enqueueing.
    #[serde(rename_all = "camelCase")]
    EnqueuePr {
        number: u64,
        bookmark: String,
        // The default keeps older persisted job outcomes readable.
        #[serde(default = "default_merge_method")]
        method: MergeMethod,
        #[serde(default)]
        expected_head: String,
    },
    /// Fetch from the remote so the merged trunk (and any branch GitHub
    /// auto-deleted on merge) arrives locally.
    #[serde(rename_all = "camelCase")]
    FetchRemote { remote: String },
    /// Rebase a stale chain — a change still sitting on the landed
    /// segment, with everything above it — onto the new trunk. `off` are
    /// the landed commits it sits on: trunk takes their place among its
    /// parents and every other parent stays, so a change that merges a
    /// side branch in keeps what the branch brings.
    #[serde(rename_all = "camelCase")]
    RebaseOntoTrunk {
        root_change: String,
        moves: u32,
        #[serde(default)]
        off: Vec<String>,
    },
    /// One batched push of the rebased stack's tracked bookmarks, so the
    /// remaining PRs show the rebased commits.
    #[serde(rename_all = "camelCase")]
    PushStack { bookmarks: Vec<String> },
    /// Point the next PR at the trunk now that everything below it is in.
    /// GitHub retargets automatically when the old base branch is deleted;
    /// the executor checks first and skips politely.
    #[serde(rename_all = "camelCase")]
    RetargetPr {
        number: u64,
        bookmark: String,
        to_base: String,
    },
    /// Point someone else's pull request at the trunk: it is based on a
    /// branch of this stack that lands in this run, and that branch is
    /// deleted afterwards. GitHub closes a pull request whose base branch
    /// is deleted; it moves one to the merged pull request's base only
    /// when its own "delete branch" does the deleting (seen on the
    /// sandbox, with a branch deleted through Git). `author` is who
    /// opened the pull request, when GitHub says.
    ///
    /// This is all a landing does for someone else's pull request. Its
    /// branch is never pushed to from here: after a squash or a rebase it
    /// still carries the commits that landed, and the pull request shows
    /// them as its own until its author rebases it. The plan and the
    /// step both say so.
    #[serde(rename_all = "camelCase")]
    RetargetOther {
        number: u64,
        #[serde(default)]
        author: Option<String>,
        branch: String,
        from_base: String,
        to_base: String,
    },
    /// Delete the landed bookmark here and on the remote — skipped
    /// politely when GitHub's delete-branch-on-merge already did. `commit`
    /// is the commit the bookmark pointed at when the plan was made, the
    /// top of what the landing check verified; a bookmark that points
    /// anywhere else by now (edited here, or pushed to on GitHub and
    /// fetched) is left in place, here and on the remote.
    #[serde(rename_all = "camelCase")]
    CleanupBookmark {
        bookmark: String,
        #[serde(default)]
        commit: String,
    },
    /// Abandon the landed segment's local changes, newest-first: a squash
    /// or rebase merge rewrites them into new trunk commits, leaving the
    /// originals as redundant mutable copies. `commits` are exactly the
    /// commits the landing check verified, and only those are abandoned:
    /// a change edited since has another commit and stays. Skipped when
    /// the refreshed snapshot shows them already in trunk's ancestry (a
    /// merge-commit landing) or already gone. `change_ids` name the same
    /// changes for display.
    #[serde(rename_all = "camelCase")]
    AbandonLanded {
        bookmark: String,
        change_ids: Vec<String>,
        #[serde(default)]
        commits: Vec<String>,
    },
}

/// One reason a land plan cannot act. `wait` says whether waiting alone
/// can clear it: true for remote conditions that settle by themselves
/// (checks running, an approval pending, GitHub still computing
/// mergeability) — what an auto-land job watches through quietly — and
/// false when the user has to act first (a draft, requested changes,
/// failing checks, an unpublished stack).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LandBlocker {
    pub message: String,
    pub wait: bool,
    /// What stands in the way is that a branch moved on GitHub past what
    /// was last fetched here. Fetching brings it: an auto-land job does
    /// that once and looks again, a person is told to fetch and look.
    #[serde(default)]
    pub fetch: bool,
}

impl LandBlocker {
    pub(crate) fn needs_user(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            wait: false,
            fetch: false,
        }
    }

    pub(crate) fn transient(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            wait: true,
            fetch: false,
        }
    }

    pub(crate) fn behind_remote(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            wait: false,
            fetch: true,
        }
    }
}

/// What landing a stack will do this run, derived before anything runs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LandPlan {
    pub head_bookmark: String,
    pub remote: String,
    /// The trunk branch the stack lands on.
    pub base_branch: String,
    /// The options the plan was derived under — the executor's
    /// just-in-time re-checks judge by the same rules.
    #[serde(default)]
    pub options: LandOptions,
    /// The stack, bottom-up, each segment placed in the landing story.
    pub segments: Vec<LandSegment>,
    /// Everything this run will do, in order. Empty with a note in
    /// `warnings` means GitHub is already driving (auto-merge enabled, in
    /// the merge queue) and there is nothing to do but wait.
    pub actions: Vec<LandAction>,
    /// Problems that stop this run from doing anything.
    pub blockers: Vec<LandBlocker>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LandStep {
    pub action: LandAction,
    pub status: SubmitStepStatus,
    /// Plain-language result: what happened, or why it was skipped.
    pub detail: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LandOutcome {
    pub steps: Vec<LandStep>,
    pub failed: bool,
}

/// The jj side of executing a land plan, host-implemented over
/// `jiji-core`'s backend (a stub in tests). Every method that mutates also
/// republishes the snapshot, and `snapshot` answers the latest one — the
/// executor's cleanup steps adapt to what the repo actually shows after
/// the fetch.
pub trait LandVcs {
    fn git_fetch(&self, remote: &str) -> Result<String, BackendError>;
    /// The latest snapshot after the steps so far.
    fn snapshot(&self) -> Result<RepoSnapshot, BackendError>;
    /// Move `root_change` and everything above it off the landed commits
    /// `landed` and onto the branch `trunk` as `remote` has it after the
    /// fetch, the commit [`LandVcs::landed`] judged the landing against
    /// (`RepoBackend::rebase_onto_trunk`). `trunk` is the plan's own
    /// branch name: what `trunk()` answers can change with a fetch, and
    /// it or the local bookmark of that name can be ahead with commits
    /// nobody pushed, which the stack would carry into its pull requests.
    /// Only the parents that landed are replaced; a change that merges a
    /// side branch in keeps the branch.
    fn rebase_onto_trunk(
        &self,
        remote: &str,
        trunk: &str,
        root_change: &str,
        landed: &[String],
    ) -> Result<String, BackendError>;
    fn push_bookmarks(&self, bookmarks: &[String], remote: &str) -> Result<String, BackendError>;
    /// Delete a local bookmark while it still points at `commit`
    /// (`RepoBackend::delete_bookmark_at`). One that moved is refused.
    fn delete_bookmark_at(&self, name: &str, commit: &str) -> Result<String, BackendError>;
    /// Abandon exactly these commits, named by commit id
    /// (`RepoBackend::abandon_landed`). None is abandoned when one of
    /// them has been rewritten since, or when a change that is not one of
    /// them sits on them: the abandon would move that change onto what
    /// was below the landed commits. When what sits on them is someone
    /// else's (a branch that is no bookmark here), that is no failure:
    /// the answer then starts with
    /// [`jiji_core::backend::LANDED_LEFT_IN_PLACE`] and says whose.
    fn abandon_changes(&self, commits: &[String]) -> Result<String, BackendError>;
    /// The remote branches that are no bookmarks here and still have any
    /// of `commits` in their history, as `name@remote`
    /// (`RepoBackend::branches_resting_on`). Read-only.
    fn resting_on(&self, commits: &[String]) -> Result<Vec<String>, BackendError>;
    /// Whether the branch `trunk`, as `remote` has it here, holds the
    /// changes from `bottom` up to `top`, on the strength of a pull
    /// request merged as the commit `landing` from the head `head`
    /// (`jiji-core`'s `RepoBackend::landed_check`). Read-only; planning
    /// asks it too.
    fn landed(
        &self,
        remote: &str,
        trunk: &str,
        landing: &str,
        head: &str,
        bottom: &str,
        top: &str,
    ) -> Result<LandedCheck, BackendError>;
    /// Whether landing the pull request that spans `bottom` to
    /// `first_top`, and then everything above it up to `tip`, one pull
    /// request at a time, would leave `trunk` as one merge of `tip` would
    /// (`RepoBackend::lands_as_tip`). Read-only, and against trunk as the
    /// last fetch left it.
    fn lands_as_tip(
        &self,
        remote: &str,
        trunk: &str,
        bottom: &str,
        first_top: &str,
        tip: &str,
    ) -> Result<TipCheck, BackendError>;
}

/// The forge side of planning and executing a land, implemented by
/// [`LandRepoForge`] over the real client, a stub in tests.
pub trait LandForge {
    /// The merged PR a branch once headed, if any — how already-merged
    /// segments are recognized without an open PR to go by.
    fn find_merged_pr(&self, branch: &str) -> Result<Option<PrSummary>, ForgeError>;
    /// Fresh land readiness for one PR, asked at plan time for the
    /// landing candidate and re-asked just before the merge.
    fn pr_land_state(&self, number: u64, base: &str) -> Result<PrLandState, ForgeError>;
    fn merge_pr(
        &self,
        number: u64,
        method: MergeMethod,
        expected_head: &str,
        commit_title: Option<&str>,
        commit_message: Option<&str>,
    ) -> Result<(), ForgeError>;
    fn enable_auto_merge(
        &self,
        node_id: &str,
        method: MergeMethod,
        expected_head: &str,
        commit_title: Option<&str>,
        commit_message: Option<&str>,
    ) -> Result<(), ForgeError>;
    fn enqueue_pr(&self, node_id: &str, expected_head: &str) -> Result<(), ForgeError>;
    fn update_pr_base(&self, number: u64, base: &str) -> Result<(), ForgeError>;
    fn mark_ready(&self, node_id: &str) -> Result<(), ForgeError>;
    /// Every open pull request whose base is `base`, asked of GitHub at
    /// the moment a landed branch is about to be deleted: GitHub closes
    /// the pull requests based on a branch that is deleted, so none may
    /// be missed, and the batched list can be cut short or out of date.
    fn open_prs_based_on(&self, base: &str) -> Result<Vec<PrSummary>, ForgeError>;
    /// Merge a pull request that sits in a GitHub stack, with every open
    /// pull request below it, and wait for GitHub to finish: the one
    /// endpoint GitHub accepts for stacked pull requests. `bypass` asks
    /// GitHub to skip the rules this user may bypass, which it allows for
    /// the bottom pull request alone.
    ///
    /// The answer is where the request ended. GitHub answers "enqueued",
    /// not "merged", for a pull request that was already in a merge
    /// queue, whatever was asked.
    fn merge_stacked(
        &self,
        number: u64,
        method: MergeMethod,
        expected_head: &str,
        commit_title: Option<&str>,
        commit_message: Option<&str>,
        bypass: bool,
    ) -> Result<AsyncMerge, ForgeError>;
    /// Add a stacked pull request, and the ones below it, to the base
    /// branch's merge queue.
    fn enqueue_stacked(&self, number: u64, expected_head: &str)
        -> Result<AsyncMerge, ForgeError>;
    /// Wait for GitHub to rebase the rest of a stack after a merge:
    /// whether `number`'s head moved off `from_head`, GitHub reported the
    /// rebase would conflict, or neither happened for the whole wait.
    fn await_restack(&self, number: u64, from_head: &str) -> Result<Restack, ForgeError>;
}

/// [`LandForge`] over the real GitHub client, bound to a detected repo.
pub struct LandRepoForge<'a> {
    pub client: &'a crate::github::GitHubClient,
    pub repo: &'a ForgeRepo,
}

impl LandForge for LandRepoForge<'_> {
    fn find_merged_pr(&self, branch: &str) -> Result<Option<PrSummary>, ForgeError> {
        self.client
            .find_merged_pr(&self.repo.owner, &self.repo.name, branch)
    }

    fn pr_land_state(&self, number: u64, base: &str) -> Result<PrLandState, ForgeError> {
        self.client
            .pr_land_state(&self.repo.owner, &self.repo.name, number, base)
    }

    fn open_prs_based_on(&self, base: &str) -> Result<Vec<PrSummary>, ForgeError> {
        self.client
            .open_prs_based_on(&self.repo.owner, &self.repo.name, base)
    }

    fn merge_pr(
        &self,
        number: u64,
        method: MergeMethod,
        expected_head: &str,
        commit_title: Option<&str>,
        commit_message: Option<&str>,
    ) -> Result<(), ForgeError> {
        self.client.merge_pr(
            &self.repo.owner,
            &self.repo.name,
            number,
            method,
            expected_head,
            commit_title,
            commit_message,
        )
    }

    fn enable_auto_merge(
        &self,
        node_id: &str,
        method: MergeMethod,
        expected_head: &str,
        commit_title: Option<&str>,
        commit_message: Option<&str>,
    ) -> Result<(), ForgeError> {
        self.client.enable_auto_merge(
            node_id,
            method,
            expected_head,
            commit_title,
            commit_message,
        )
    }

    fn enqueue_pr(&self, node_id: &str, expected_head: &str) -> Result<(), ForgeError> {
        self.client.enqueue_pr(node_id, expected_head)
    }

    fn update_pr_base(&self, number: u64, base: &str) -> Result<(), ForgeError> {
        self.client
            .update_pr_base(&self.repo.owner, &self.repo.name, number, base)
    }

    fn mark_ready(&self, node_id: &str) -> Result<(), ForgeError> {
        self.client.mark_ready(node_id)
    }

    fn merge_stacked(
        &self,
        number: u64,
        method: MergeMethod,
        expected_head: &str,
        commit_title: Option<&str>,
        commit_message: Option<&str>,
        bypass: bool,
    ) -> Result<AsyncMerge, ForgeError> {
        self.client.merge_async(
            &self.repo.owner,
            &self.repo.name,
            number,
            method,
            expected_head,
            commit_title,
            commit_message,
            bypass,
            false,
        )
    }

    fn enqueue_stacked(
        &self,
        number: u64,
        expected_head: &str,
    ) -> Result<AsyncMerge, ForgeError> {
        self.client.merge_async(
            &self.repo.owner,
            &self.repo.name,
            number,
            MergeMethod::Squash,
            expected_head,
            None,
            None,
            false,
            true,
        )
    }

    fn await_restack(&self, number: u64, from_head: &str) -> Result<Restack, ForgeError> {
        self.client
            .await_restack(&self.repo.owner, &self.repo.name, number, from_head)
    }
}

/// How the landing candidate reads right now — shared by plan (to shape
/// the actions) and execute (the just-in-time re-check before merging).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Readiness {
    /// Mergeable right now.
    Ready,
    /// Merged since the state was last read.
    AlreadyMerged,
    /// GitHub's automation is already driving this PR.
    AutomationRunning(String),
    /// Waiting only on conditions GitHub automates on.
    WaitingOnAutomatable(Vec<String>),
    /// Cannot land right now; each blocker says whether waiting can clear
    /// it on its own.
    Blocked(Vec<LandBlocker>),
}

/// Classify a fresh [`PrLandState`] for a PR expected to land on `trunk`
/// and to target `base`: trunk itself, or in a GitHub stack the branch of
/// the pull request below.
///
/// The options shape the judgment: `bypass` waves through the soft
/// blockers and waits (requested changes, failing or pending checks, a
/// required review) — GitHub itself enforces whether the user may — while
/// hard impossibilities (closed, conflicting, wrong base, a draft) block
/// regardless. A merge-state that says the button is green right now
/// (CLEAN/UNSTABLE/HAS_HOOKS) also clears the failing-checks blocker and
/// the waits: whatever the full rollup counts, nothing *required* is
/// outstanding. Requested changes still block without bypass even when
/// mergeable — a human asked for changes, and landing over them silently
/// is not this tool's call.
pub(crate) fn classify_candidate(
    number: u64,
    land: &PrLandState,
    base: &str,
    trunk: &str,
    options: &LandOptions,
) -> Readiness {
    match land.state {
        PrState::Merged => return Readiness::AlreadyMerged,
        PrState::Closed => {
            return Readiness::Blocked(vec![LandBlocker::needs_user(format!(
                "#{number} was closed on GitHub without merging — reopen it or publish again"
            ))])
        }
        PrState::Open => {}
    }
    // Bypass is an explicit request to try a direct admin merge even when
    // GitHub automation is already armed. Without it, the existing
    // hand-off owns the PR and another mutation would only fight it.
    if land.in_merge_queue && !options.bypass {
        return Readiness::AutomationRunning(format!(
            "#{number} is already in the merge queue; GitHub lands it from here — \
             run Land again afterwards to reconcile"
        ));
    }
    if land.auto_merge_enabled && !options.bypass {
        return Readiness::AutomationRunning(format!(
            "auto-merge is already enabled on #{number}; GitHub merges it once its \
             requirements are met — run Land again afterwards to reconcile"
        ));
    }
    let merges_now = land.merge_state.merges_now();
    let mut blockers: Vec<LandBlocker> = Vec::new();
    if land.is_draft {
        blockers.push(LandBlocker::needs_user(format!(
            "#{number} is still a draft — mark it ready for review first"
        )));
    }
    if land.base_branch != base {
        blockers.push(LandBlocker::needs_user(format!(
            "#{number} still targets \u{201c}{}\u{201d}, not \u{201c}{base}\u{201d} — \
             publish the stack to retarget it first",
            land.base_branch
        )));
    }
    match land.mergeable {
        Mergeable::Conflicting => blockers.push(LandBlocker::needs_user(format!(
            "#{number} has merge conflicts with \u{201c}{trunk}\u{201d} — rebase onto \
             the fetched trunk, publish, and land again"
        ))),
        Mergeable::Unknown => blockers.push(LandBlocker::transient(format!(
            "GitHub is still working out whether #{number} merges cleanly — plan \
             again in a moment"
        ))),
        Mergeable::Mergeable => {}
    }
    if land.review == ReviewDecision::ChangesRequested && !options.bypass {
        blockers.push(LandBlocker::needs_user(format!(
            "changes were requested on #{number}"
        )));
    }
    if land.checks == ChecksRollup::Failing && !merges_now && !options.bypass {
        blockers.push(LandBlocker::needs_user(format!(
            "#{number}'s checks are failing"
        )));
    }
    if land.merge_state == MergeStateStatus::Dirty
        && land.mergeable != Mergeable::Conflicting
    {
        blockers.push(LandBlocker::needs_user(format!(
            "#{number} has merge conflicts with \u{201c}{trunk}\u{201d} — rebase onto \
             the fetched trunk, publish, and land again"
        )));
    }
    if land.merge_state == MergeStateStatus::Behind
        && !land.queue_on_base
        && !options.bypass
    {
        blockers.push(LandBlocker::needs_user(format!(
            "#{number} is behind \u{201c}{trunk}\u{201d} — rebase onto the fetched \
             trunk, publish, and land again"
        )));
    }
    if !blockers.is_empty() {
        return Readiness::Blocked(blockers);
    }
    if options.bypass || merges_now {
        return Readiness::Ready;
    }
    // What remains are the wait-states GitHub's own automation exists for.
    let mut waits: Vec<String> = Vec::new();
    if land.checks == ChecksRollup::Pending {
        waits.push(format!("#{number}'s checks are still running"));
    }
    if land.review == ReviewDecision::ReviewRequired {
        waits.push(format!("#{number} still needs an approving review"));
    }
    if land.merge_state == MergeStateStatus::Behind {
        waits.push(format!(
            "#{number} is behind \u{201c}{trunk}\u{201d}; the merge queue updates it"
        ));
    }
    if land.merge_state == MergeStateStatus::Blocked && waits.is_empty() {
        waits.push(format!(
            "GitHub reports #{number} is blocked by a repository requirement"
        ));
    }
    if waits.is_empty() {
        Readiness::Ready
    } else {
        Readiness::WaitingOnAutomatable(waits)
    }
}

/// The merge method for a repo's capabilities. A requested method is
/// honored only if the repo allows it (`None` answered otherwise — the
/// caller phrases the blocker); with nothing requested, squash is
/// preferred (jjpr's default and the stacked-PR convention), then rebase,
/// then merge.
pub(crate) fn pick_method(
    land: &PrLandState,
    requested: Option<MergeMethod>,
) -> Option<MergeMethod> {
    let allows = |method: MergeMethod| match method {
        MergeMethod::Squash => land.allows_squash,
        MergeMethod::Rebase => land.allows_rebase,
        MergeMethod::Merge => land.allows_merge,
    };
    match requested {
        Some(method) => allows(method).then_some(method),
        None => [MergeMethod::Squash, MergeMethod::Rebase, MergeMethod::Merge]
            .into_iter()
            .find(|method| allows(*method)),
    }
}

/// The squash commit's title and body: `PR title (#N)` — GitHub's own
/// squash-title convention — with the segment's change descriptions as
/// the body. A single change whose summary line repeats the PR title
/// contributes only its body, so nothing is said twice.
fn squash_commit(pr: &PrSummary, nodes: &[&GraphNode]) -> (String, String) {
    let title = format!("{} (#{})", pr.title.trim(), pr.number);
    let descriptions: Vec<&str> = nodes
        .iter()
        .map(|n| n.description.trim())
        .filter(|d| !d.is_empty())
        .collect();
    let message = match descriptions.as_slice() {
        [only] => {
            let (summary, body) = only.split_once('\n').unwrap_or((only, ""));
            if summary.trim() == pr.title.trim() {
                body.trim().to_owned()
            } else {
                (*only).to_owned()
            }
        }
        many => many.join("\n\n"),
    };
    (title, message)
}


/// One segment placed in the landing story, before DTO conversion.
struct PlacedSegment<'a> {
    raw_index: usize,
    bookmark: &'a str,
    change_ids: Vec<String>,
    pr: Option<PrSummary>,
    status: LandSegmentStatus,
    /// The segment's pull request's seat in a GitHub stack, merged or not.
    seat: Option<StackSeat>,
}

/// Whether GitHub's full oid names the commit a snapshot node carries
/// (snapshot commit ids are unique prefixes).
pub(crate) fn same_commit(full: &str, short: &str) -> bool {
    !short.is_empty() && full.starts_with(short)
}

/// The commits a segment spans as the snapshot has them, bottom and top:
/// what a landing check is asked about. Commit ids, not change ids, so
/// the answer stays about exactly these changes whatever a fetch brings.
fn segment_span(raw: &crate::submit::RawSegment<'_>) -> (String, String) {
    let bottom = raw.nodes.first().expect("segments are never empty");
    let top = raw.nodes.last().expect("segments are never empty");
    (bottom.commit_id.clone(), top.commit_id.clone())
}

fn bookmark_synced(snapshot: &RepoSnapshot, name: &str) -> bool {
    snapshot
        .bookmarks
        .iter()
        .any(|b| b.name == name && b.sync == SyncState::Synced)
}

/// "a.txt, b.txt and 3 more".
fn path_list(paths: &[String], more: u32) -> String {
    let mut list = paths.join(", ");
    if more > 0 {
        list.push_str(&format!(" and {more} more"));
    }
    list
}

fn merged_elsewhere(number: u64, bookmark: &str, into: &str, trunk: &str) -> String {
    if into == trunk {
        // GitHub names trunk and a commit trunk does not have: the branch
        // was rewritten, or the commit never reached it.
        return format!(
            "#{number} ({bookmark}) is closed as merged on GitHub, but the commit it \
             merged as is not part of \u{201c}{trunk}\u{201d} and \u{201c}{trunk}\u{201d} does \
             not hold its changes — publish the stack to open a new pull request for them"
        );
    }
    format!(
        "#{number} ({bookmark}) is closed as merged on GitHub, but it merged into \
         \u{201c}{into}\u{201d}, not \u{201c}{trunk}\u{201d}, so its changes have not \
         landed — publish the stack to open a new pull request for them"
    )
}

/// The commits are in trunk's history and their content is not: the merge
/// that took the branch in dropped it. There is nothing to rebase or
/// publish again from commits trunk already has, so the bookmark stays.
fn kept_nothing(number: u64, bookmark: &str, trunk: &str, paths: &[String], more: u32) -> String {
    format!(
        "#{number} is merged on GitHub and the commits under \u{201c}{bookmark}\u{201d} are \
         part of \u{201c}{trunk}\u{201d}'s history, but the merge that brought them in did not \
         keep their changes ({}). \u{201c}{bookmark}\u{201d} was left alone: it is what still \
         names that work",
        path_list(paths, more)
    )
}

/// `same_head` says the pull request was merged from the commit the
/// bookmark has here. When it was not, that is the likelier story than a
/// merge that lost something: the branch name had an earlier pull request,
/// or the changes were edited after it merged.
fn missing_changes(
    number: u64,
    bookmark: &str,
    trunk: &str,
    same_head: bool,
    paths: &[String],
    more: u32,
) -> String {
    let paths = path_list(paths, more);
    if same_head {
        format!(
            "#{number} is merged on GitHub, but \u{201c}{trunk}\u{201d} does not hold \
             everything under \u{201c}{bookmark}\u{201d} ({paths}) — rebase onto the fetched \
             trunk and publish to open a pull request for what is left"
        )
    } else {
        format!(
            "#{number} is merged on GitHub, but it was merged from another commit than \
             \u{201c}{bookmark}\u{201d} has here, and \u{201c}{trunk}\u{201d} does not hold \
             everything under \u{201c}{bookmark}\u{201d} ({paths}) — rebase onto the fetched \
             trunk and publish to open a pull request for what is left"
        )
    }
}

/// Whether the merge GitHub reports for a segment's branch is that
/// segment's landing, as far as this repository can tell right now.
/// `None` says it is, or that only a fetch can tell; the run's own
/// [`LandAction::CheckLanded`] settles it after fetching either way.
/// `Some` is the reason it certainly is not: a pull request merged into
/// another branch, or one whose merge does not hold what is here (an
/// earlier pull request under a reused branch name, or work added since).
#[allow(clippy::too_many_arguments)]
fn not_landed(
    number: u64,
    merge_commit: Option<&str>,
    head: &str,
    merged_into: &str,
    raw: &crate::submit::RawSegment<'_>,
    trunk: &str,
    remote: &str,
    vcs: &dyn LandVcs,
) -> Result<Option<String>, ForgeError> {
    let Some(landing) = merge_commit else {
        return Ok(None);
    };
    let (bottom, top) = segment_span(raw);
    let check = vcs
        .landed(remote, trunk, landing, head, &bottom, &top)
        .map_err(|err| ForgeError::Land(err.to_string()))?;
    // A segment that is trunk history already (see `landed_under`).
    let in_trunk = raw.nodes.iter().all(|n| n.is_settled());
    Ok(match check {
        LandedCheck::Landed | LandedCheck::Held | LandedCheck::Unfetched => None,
        // It merged into trunk, and trunk here does not have the commit:
        // trunk here is behind. The run fetches, then checks.
        LandedCheck::Elsewhere if merged_into == trunk => None,
        LandedCheck::Elsewhere => Some(merged_elsewhere(
            number,
            raw.bookmark,
            merged_into,
            trunk,
        )),
        LandedCheck::Missing { paths, more } if in_trunk => {
            Some(kept_nothing(number, raw.bookmark, trunk, &paths, more))
        }
        LandedCheck::Missing { paths, more } => Some(missing_changes(
            number,
            raw.bookmark,
            trunk,
            same_commit(head, &top),
            &paths,
            more,
        )),
    })
}

/// The bookmark `base` on the commit a stack sits on, when that commit is
/// trunk history and a pull request merged from exactly it: a segment
/// that landed with its own commits (a merge commit, or trunk moved onto
/// the branch), met again after a fetch made those commits immutable. The
/// walk from the head stops above it, so it is put back as a segment of
/// one: its landing still wants checking, its bookmark cleaning up, and
/// what sits on it rebasing. A run interrupted after such a merge, or an
/// auto-land job's next look, finds its way back through here.
///
/// `own` is the stack's bottom pull request and `base` the branch it
/// still targets. That is what makes the leftover this stack's business:
/// without it the pull request cannot land. A landed bookmark nothing
/// here is based on is left for `land` on that bookmark by name.
///
/// And it has to be this stack's alone. A branch other open pull requests
/// are based on is not a leftover of one stack: it may be a long-lived
/// branch (a `develop` merged into trunk with a merge commit looks exactly
/// like a landed segment), and removing it would move or close pull
/// requests that are none of this stack's business. Then nothing is done
/// about it, and the plan says what it always said: the bottom pull
/// request targets a branch that is not trunk.
fn landed_under<'a>(
    snapshot: &'a RepoSnapshot,
    prs: &RepoPrState,
    node: &'a GraphNode,
    own: u64,
    base: &str,
    forge: &dyn LandForge,
) -> Result<Option<crate::submit::RawSegment<'a>>, ForgeError> {
    if !node.is_settled() {
        return Ok(None);
    }
    let shared = prs.report.truncated
        || prs
            .report
            .prs
            .iter()
            .any(|pr| pr.base_branch == base && pr.number != own);
    if shared {
        return Ok(None);
    }
    let candidates = snapshot.bookmarks.iter().filter(|b| {
        b.name == base
            && b.is_local
            && !b.is_trunk
            && b.target == node.id
            && !prs.by_branch.contains_key(&b.name)
    });
    for bookmark in candidates {
        let merged = forge.find_merged_pr(&bookmark.name)?;
        if merged.is_some_and(|pr| same_commit(&pr.head_commit, &node.commit_id)) {
            return Ok(Some(crate::submit::RawSegment {
                bookmark: &bookmark.name,
                nodes: vec![node],
            }));
        }
    }
    Ok(None)
}

/// The plan for a head bookmark that has nothing mutable under it
/// because its pull request landed with its own commits: the same
/// leftover as [`landed_under`], asked for by name. There is no segment
/// to walk and often no commit drawn (the snapshot draws nothing inside
/// trunk's history that no mutable change sits on), so the plan goes by
/// the commit the pull request was merged from: check that it is part of
/// trunk and that trunk kept its content, then delete the bookmark, which
/// the backend does only while the bookmark points at exactly that commit.
///
/// That is all it does. It retargets nothing, and it does not run at all
/// while any open pull request is based on the branch: deleting a branch
/// on GitHub moves or closes the pull requests based on it, and a branch
/// with pull requests based on it may be a long-lived one that was merged
/// into trunk and goes on (see [`landed_under`]). A stack of the user's
/// own that still sits on it reconciles it through that stack's Land.
fn landed_head_plan(
    snapshot: &RepoSnapshot,
    prs: &RepoPrState,
    repo: &ForgeRepo,
    head_bookmark: &str,
    forge: &dyn LandForge,
    vcs: &dyn LandVcs,
    options: &LandOptions,
) -> Result<Option<LandPlan>, ForgeError> {
    let Some(bookmark) = snapshot
        .bookmarks
        .iter()
        .find(|b| b.name == head_bookmark && b.is_local && !b.is_trunk)
    else {
        return Ok(None);
    };
    if prs.by_branch.contains_key(head_bookmark) {
        return Ok(None);
    }
    let Some(merged) = forge.find_merged_pr(head_bookmark)? else {
        return Ok(None);
    };
    let Some(landing) = merged.merge_commit.as_deref() else {
        return Ok(None);
    };
    let head = merged.head_commit.as_str();
    // The bookmark has to sit on the commit that pull request was merged
    // from, as far as the snapshot can tell: on that very commit when it
    // is drawn, somewhere inside trunk's history when it is not. (The
    // delete at the end is exact either way, and nothing else is changed
    // on the strength of this look.)
    let on_head = match snapshot.nodes.iter().find(|n| n.id == bookmark.target) {
        Some(node) => node.is_settled() && same_commit(head, &node.commit_id),
        None => bookmark.is_merged,
    };
    if !on_head {
        return Ok(None);
    }
    let trunk = snapshot.trunk_bookmark.clone();
    let mut blockers: Vec<LandBlocker> = Vec::new();
    // And that commit has to be part of trunk here, content included. A
    // merge can take a branch in and keep none of its changes; GitHub
    // calls that merged too (seen on the sandbox). Then the bookmark is
    // all that still names the work, and it stays.
    // (The head stands for the whole pull request here: the backend asks
    // the merge about everything it brought in, not about one commit.)
    match vcs.landed(&repo.remote, &trunk, landing, head, head, head) {
        Ok(LandedCheck::Landed | LandedCheck::Held) => {}
        Ok(LandedCheck::Missing { paths, more }) => {
            blockers.push(LandBlocker::needs_user(kept_nothing(
                merged.number,
                head_bookmark,
                &trunk,
                &paths,
                more,
            )));
        }
        _ => return Ok(None),
    }
    let based_on_it: Vec<u64> = prs
        .report
        .prs
        .iter()
        .filter(|pr| pr.base_branch == head_bookmark)
        .map(|pr| pr.number)
        .collect();
    if !blockers.is_empty() {
        // Already not going to be deleted.
    } else if !based_on_it.is_empty() {
        blockers.push(LandBlocker::needs_user(format!(
            "\u{201c}{head_bookmark}\u{201d} landed (#{}), and {} still based on it. Jiji does \
             not delete a branch that open pull requests are based on: GitHub would move or \
             close them. If they are a stack of yours here, land that stack and it takes \
             \u{201c}{head_bookmark}\u{201d} with it; otherwise leave the branch",
            merged.number,
            match based_on_it.as_slice() {
                [one] => format!("#{one} is"),
                many => format!("{} are", number_list(many.iter().copied())),
            }
        )));
    } else if prs.report.truncated {
        blockers.push(LandBlocker::needs_user(format!(
            "\u{201c}{head_bookmark}\u{201d} landed (#{}), but GitHub answered only the 100 \
             most recently updated open pull requests, so Jiji cannot tell whether one is \
             based on it, and does not delete a branch that may have one",
            merged.number
        )));
    }
    let actions = if blockers.is_empty() {
        vec![
            LandAction::FetchRemote {
                remote: repo.remote.clone(),
            },
            LandAction::CheckLanded {
                number: merged.number,
                bookmark: head_bookmark.to_owned(),
                bottom_commit: head.to_owned(),
                top_commit: head.to_owned(),
            },
            LandAction::CleanupBookmark {
                bookmark: head_bookmark.to_owned(),
                commit: head.to_owned(),
            },
        ]
    } else {
        Vec::new()
    };
    Ok(Some(LandPlan {
        head_bookmark: head_bookmark.to_owned(),
        remote: repo.remote.clone(),
        base_branch: trunk,
        options: *options,
        segments: vec![LandSegment {
            bookmark: head_bookmark.to_owned(),
            change_ids: Vec::new(),
            pr: None,
            status: LandSegmentStatus::Merged {
                number: merged.number,
                url: merged.url.clone(),
            },
        }],
        actions,
        blockers,
        warnings: Vec::new(),
    }))
}

/// The first open pull request above `seat` in the same GitHub stack
/// whose bookmark here is not what GitHub has, told as the reason a merge
/// below it has to wait. GitHub rebases its own copy of every branch left
/// above a merge; a copy here that differs then exists twice (seen on the
/// sandbox: the change divergent, the bookmark conflicted, the push
/// refused).
fn unpublished_above(
    snapshot: &RepoSnapshot,
    prs: &RepoPrState,
    seat: &StackSeat,
) -> Option<String> {
    let mut above: Vec<&PrSummary> = prs
        .report
        .prs
        .iter()
        .filter(|pr| {
            pr.stack
                .as_ref()
                .is_some_and(|s| s.number == seat.number && s.position > seat.position)
        })
        .collect();
    above.sort_by_key(|pr| pr.stack.as_ref().map(|s| s.position));
    above
        .into_iter()
        .find(|pr| {
            prs.of_this_repository(pr)
                && snapshot.bookmarks.iter().any(|b| {
                    b.name == pr.head_branch && b.is_local && b.sync != SyncState::Synced
                })
        })
        .map(|pr| {
            format!(
                "\u{201c}{}\u{201d} (#{}) differs here from its branch on GitHub. It is in \
                 the same GitHub stack, and after a merge below it GitHub rebases its own \
                 copy of that branch — publish first, so there is one copy to rebase",
                pr.head_branch, pr.number
            )
        })
}

/// The pull requests above the candidate that land with it. In a GitHub
/// stack a merge takes everything below the pull request asked for, so a
/// round asks for the highest one that is ready: each next segment joins
/// while its pull request holds the next seat in the same stack, its
/// branch on GitHub is the commit here, and it reads ready (a draft
/// counts as ready; the plan promotes it). The first one that does not
/// ends the run and waits for a later round.
#[allow(clippy::too_many_arguments)]
fn stack_run(
    snapshot: &RepoSnapshot,
    raw_segments: &[crate::submit::RawSegment<'_>],
    placed: &[PlacedSegment<'_>],
    candidate: usize,
    seat: &StackSeat,
    trunk: &str,
    forge: &dyn LandForge,
    options: &LandOptions,
) -> Vec<(usize, PrSummary, PrLandState)> {
    let mut run: Vec<(usize, PrSummary, PrLandState)> = Vec::new();
    let mut base = raw_segments[candidate].bookmark;
    let above = placed.iter().filter(|p| p.raw_index > candidate);
    // Each next segment must hold the next seat up.
    for (p, next_seat) in above.zip(seat.position + 1..) {
        let Some(pr) = &p.pr else { break };
        // A failed read ends the run here; it never fails the plan.
        let Ok(fresh) = forge.pr_land_state(pr.number, trunk) else {
            break;
        };
        let seated = fresh
            .stack
            .as_ref()
            .is_some_and(|s| s.number == seat.number && s.position == next_seat);
        let raw = &raw_segments[p.raw_index];
        let (_, top) = segment_span(raw);
        if !seated
            || !bookmark_synced(snapshot, raw.bookmark)
            || !same_commit(&fresh.head_commit, &top)
            || raw.nodes.iter().any(|n| n.has_conflict)
        {
            break;
        }
        let mut promoted = fresh.clone();
        promoted.is_draft = false;
        if classify_candidate(pr.number, &promoted, base, trunk, options) != Readiness::Ready {
            break;
        }
        base = raw.bookmark;
        run.push((p.raw_index, pr.clone(), fresh));
    }
    run
}

/// Build the landing plan for the stack under `head_bookmark`.
///
/// Bottom-up, jjpr's walk: segments whose PR GitHub already merged are
/// recognized (the per-bookmark merged query — the batched open-PR state
/// cannot see merges) and get their reconcile planned; the first unmerged
/// segment is the landing candidate, evaluated against fresh per-PR state;
/// everything above waits for a later run. When a segment below the
/// candidate merged, this run is reconcile-only — the candidate's branch
/// still carries the merged segment's commits until the rebase and push go
/// through, so merging it in the same run would land a stale diff.
///
/// A merge GitHub reports counts only as far as the repository agrees
/// (`vcs`, see [`not_landed`]): one that certainly did not land this
/// segment leaves the segment unlanded, with the reason as a blocker.
pub fn plan_land(
    snapshot: &RepoSnapshot,
    prs: &RepoPrState,
    repo: &ForgeRepo,
    head_bookmark: &str,
    forge: &dyn LandForge,
    vcs: &dyn LandVcs,
    options: &LandOptions,
) -> Result<LandPlan, ForgeError> {
    let (mut raw_segments, mut warnings) = match stack_segments(snapshot, prs, head_bookmark) {
        Ok(found) => found,
        Err(message) => {
            // Nothing mutable under the head. It may be a landed
            // bookmark left on trunk's history.
            return landed_head_plan(snapshot, prs, repo, head_bookmark, forge, vcs, options)?
                .ok_or(ForgeError::Land(message));
        }
    };
    // A landed bookmark can also be what the stack sits on, with the
    // stack's bottom pull request still pointing at its branch.
    let below = raw_segments
        .first()
        .and_then(|raw| raw.nodes.first())
        .and_then(|bottom| bottom.parents.first())
        .and_then(|id| snapshot.nodes.iter().find(|n| n.id == *id));
    let still_based_on = raw_segments
        .first()
        .and_then(|raw| prs.by_branch.get(raw.bookmark))
        .map(|pr| (pr.number, pr.base_branch.clone()));
    if let (Some(below), Some((own, base))) = (below, still_based_on) {
        if let Some(raw) = landed_under(snapshot, prs, below, own, &base, forge)? {
            raw_segments.insert(0, raw);
        }
    }
    let trunk = snapshot.trunk_bookmark.clone();
    let mut blockers: Vec<LandBlocker> = Vec::new();
    if prs.report.truncated {
        warnings.push(
            "GitHub answered only the 100 most recently updated open PRs; a stack \
             PR past that may read as missing"
                .to_owned(),
        );
    }
    // Pull requests that rest on this stack and are someone else's: based
    // on one of its branches, with a branch that is no bookmark here.
    // They are never merged from here, and their branches never pushed
    // to. What a landing does to the branches under them is said for
    // each, by number and author, before anything runs.
    let own_branches: Vec<&str> = raw_segments.iter().map(|raw| raw.bookmark).collect();
    let others = crate::pr::others_on(snapshot, prs, &own_branches);

    // Walk bottom-up: already-merged segments, then the landing candidate,
    // then the stacked rest.
    let mut placed: Vec<PlacedSegment> = Vec::new();
    let mut merged_top: Option<usize> = None;
    let mut candidate: Option<(usize, PrSummary, Option<PrLandState>)> = None;
    // The bottom-most segment with nothing to land through: no pull
    // request at all, or (with the reason) a merged one that did not land
    // it.
    let mut unpublished: Option<(&str, Option<String>)> = None;
    let mut bottom_resolved = true;
    for (index, raw) in raw_segments.iter().enumerate() {
        let change_ids: Vec<String> = raw.nodes.iter().map(|n| n.id.clone()).collect();
        let open_pr = prs.by_branch.get(raw.bookmark).cloned();
        if !bottom_resolved {
            placed.push(PlacedSegment {
                raw_index: index,
                bookmark: raw.bookmark,
                change_ids,
                seat: open_pr.as_ref().and_then(|pr| pr.stack.clone()),
                pr: open_pr,
                status: LandSegmentStatus::Stacked,
            });
            continue;
        }
        match open_pr {
            Some(pr) => {
                if merged_top.is_some() {
                    // Reconcile-only run: this PR stays put until the
                    // stack below it is rebased, pushed, and re-checked.
                    bottom_resolved = false;
                    candidate = Some((index, pr.clone(), None));
                    placed.push(PlacedSegment {
                        raw_index: index,
                        bookmark: raw.bookmark,
                        change_ids,
                        seat: pr.stack.clone(),
                        pr: Some(pr),
                        status: LandSegmentStatus::Stacked,
                    });
                    continue;
                }
                // The landing candidate: ask GitHub for fresh land state.
                let land = forge.pr_land_state(pr.number, &trunk)?;
                if land.state == PrState::Merged {
                    // Merged since the batched open-PR fetch — the
                    // recognition path catches the race.
                    let stray = not_landed(
                        pr.number,
                        land.merge_commit.as_deref(),
                        &land.head_commit,
                        land.merged_into(),
                        raw,
                        &trunk,
                        &repo.remote,
                        vcs,
                    )?;
                    if let Some(reason) = stray {
                        bottom_resolved = false;
                        unpublished = Some((raw.bookmark, Some(reason)));
                        placed.push(PlacedSegment {
                            raw_index: index,
                            bookmark: raw.bookmark,
                            change_ids,
                            pr: None,
                            status: LandSegmentStatus::Waiting,
                            seat: land.stack.clone(),
                        });
                        continue;
                    }
                    placed.push(PlacedSegment {
                        raw_index: index,
                        bookmark: raw.bookmark,
                        change_ids,
                        pr: None,
                        status: LandSegmentStatus::Merged {
                            number: pr.number,
                            url: pr.url.clone(),
                        },
                        seat: land.stack.clone(),
                    });
                    merged_top = Some(index);
                    continue;
                }
                bottom_resolved = false;
                placed.push(PlacedSegment {
                    raw_index: index,
                    bookmark: raw.bookmark,
                    change_ids,
                    seat: land.stack.clone(),
                    pr: Some(pr.clone()),
                    // Settled below once the candidate is classified.
                    status: LandSegmentStatus::Waiting,
                });
                candidate = Some((index, pr, Some(land)));
            }
            None => {
                let merged = forge.find_merged_pr(raw.bookmark)?;
                let stray = match &merged {
                    Some(merged) => not_landed(
                        merged.number,
                        merged.merge_commit.as_deref(),
                        &merged.head_commit,
                        merged.merged_into(),
                        raw,
                        &trunk,
                        &repo.remote,
                        vcs,
                    )?,
                    None => None,
                };
                match (merged, stray) {
                    (Some(merged), None) => {
                        placed.push(PlacedSegment {
                            raw_index: index,
                            bookmark: raw.bookmark,
                            change_ids,
                            pr: None,
                            status: LandSegmentStatus::Merged {
                                number: merged.number,
                                url: merged.url.clone(),
                            },
                            seat: merged.stack.clone(),
                        });
                        merged_top = Some(index);
                    }
                    (_, stray) => {
                        unpublished = Some((raw.bookmark, stray));
                        bottom_resolved = false;
                        placed.push(PlacedSegment {
                            raw_index: index,
                            bookmark: raw.bookmark,
                            change_ids,
                            pr: None,
                            status: LandSegmentStatus::Waiting,
                            seat: None,
                        });
                    }
                }
            }
        }
    }

    let mut actions: Vec<LandAction> = Vec::new();
    if merged_top.is_some() {
        // Reconcile-only run. A segment above the merged set with nothing
        // to land through does not block the reconcile itself — publishing
        // continues afterwards.
        if let Some((bookmark, stray)) = &unpublished {
            warnings.push(stray.clone().unwrap_or_else(|| {
                format!(
                    "\u{201c}{bookmark}\u{201d} has no pull request yet — publish the \
                     stack once this reconcile lands"
                )
            }));
            if let Some(p) = placed.iter_mut().find(|p| p.bookmark == *bookmark) {
                p.status = LandSegmentStatus::Stacked;
            }
        }
        let landed: Vec<(usize, u64)> = placed
            .iter()
            .filter_map(|p| match p.status {
                LandSegmentStatus::Merged { number, .. } => Some((p.raw_index, number)),
                _ => None,
            })
            .collect();
        // The next PR up (the candidate this run leaves stacked) can arm
        // auto-merge once the reconcile has rebased and retargeted it —
        // GitHub then lands it as its fresh checks pass. Asked against
        // fresh land state so a repo without auto-merge, a queue-protected
        // trunk, a draft, or a pull request in a GitHub stack (which has
        // no auto-merge) skips the hand-off; a failed lookup just means no
        // chain, never a failed plan.
        let chain_method = match (&candidate, options.bypass) {
            (Some((_, pr, None)), false) => forge
                .pr_land_state(pr.number, &trunk)
                .ok()
                .filter(|next| {
                    next.state == PrState::Open
                        && !next.is_draft
                        && !next.auto_merge_enabled
                        && !next.in_merge_queue
                        && next.auto_merge_allowed
                        && !next.queue_on_base
                        && next.stack.is_none()
                })
                .and_then(|next| pick_method(&next, options.method)),
            _ => None,
        };
        actions.extend(reconcile_actions(
            snapshot,
            prs,
            repo,
            &raw_segments,
            &placed,
            &landed,
            &others,
            &trunk,
            None,
            chain_method,
            &mut warnings,
            &mut blockers,
        ));
    } else if let Some((bookmark, stray)) = unpublished {
        blockers.push(LandBlocker::needs_user(stray.unwrap_or_else(|| {
            format!("\u{201c}{bookmark}\u{201d} has no pull request — publish the stack first")
        })));
    } else if let Some((index, pr, Some(land))) = &candidate {
        // Merge run: nothing below is pending — evaluate the candidate.
        let raw = &raw_segments[*index];
        let (_, top_commit) = segment_span(raw);
        if !bookmark_synced(snapshot, raw.bookmark) && raw.nodes.iter().all(|n| n.is_empty) {
            // Publish does not push a segment whose changes are all
            // empty, so "publish first" would send the two commands back
            // and forth. A rebase makes a change empty when trunk already
            // has what it did.
            blockers.push(LandBlocker::needs_user(format!(
                "every change under \u{201c}{}\u{201d} is empty here, so nothing is left to \
                 land through #{}, and Publish does not push an empty segment. Close #{} \
                 and abandon the change, or give it content; then publish, and land the \
                 rest",
                raw.bookmark, pr.number, pr.number
            )));
        } else if !bookmark_synced(snapshot, raw.bookmark) {
            blockers.push(LandBlocker::needs_user(format!(
                "\u{201c}{}\u{201d} and its GitHub branch differ — publish the stack \
                 first, so what merges is what you see here",
                raw.bookmark
            )));
        } else if !same_commit(&land.head_commit, &top_commit) {
            // The bookmark reads in sync against the last fetch, yet
            // GitHub's branch is somewhere else: it moved since.
            blockers.push(LandBlocker::behind_remote(format!(
                "#{}'s branch on GitHub is at {}, not at {top_commit} where \
                 \u{201c}{}\u{201d} is here — something pushed to it; fetch and look at \
                 what arrived before landing",
                pr.number,
                &land.head_commit[..land.head_commit.len().min(top_commit.len().max(8))],
                raw.bookmark
            )));
        }
        // A change with a jj conflict has the conflict's sides in its Git
        // tree, as files. jj does not push one, but a branch pushed some
        // other way can hold one, in sync and with a ready pull request;
        // GitHub sees ordinary files and would merge them.
        if let Some(node) = raw.nodes.iter().find(|n| n.has_conflict) {
            blockers.push(LandBlocker::needs_user(format!(
                "{} under \u{201c}{}\u{201d} has conflicts — resolve them and publish before \
                 landing; merged as it is, the conflict itself would land on \
                 \u{201c}{trunk}\u{201d} as files",
                node.id, raw.bookmark
            )));
        }
        // A GitHub stack lands on its own base, and a merge takes every
        // open pull request below the one asked for. A stack that lands
        // somewhere else would take this pull request there, along with
        // pull requests this plan never looked at.
        if let Some(seat) = land.stack.as_ref().filter(|seat| seat.base != trunk) {
            blockers.push(LandBlocker::needs_user(format!(
                "#{} is part of GitHub stack {}, which lands on \u{201c}{}\u{201d}, not on \
                 \u{201c}{trunk}\u{201d} — a merge would take it there; take it out of that \
                 stack on GitHub first",
                pr.number, seat.number, seat.base
            )));
        }
        // A merge queue owns the method; enqueuePullRequest has no method
        // input. Follow that configuration by default, and refuse an
        // explicitly different request instead of silently changing the
        // landing semantics the user selected.
        let queue_driven = land.queue_on_base && !options.bypass;
        let method = if queue_driven {
            match land.queue_method {
                Some(queue_method) => {
                    if let Some(requested) = options.method {
                        if requested != queue_method {
                            blockers.push(LandBlocker::needs_user(format!(
                                "\u{201c}{trunk}\u{201d}'s merge queue uses {} merges, \
                                 not the requested {} method — choose Auto or change \
                                 the queue configuration",
                                queue_method.label(),
                                requested.label()
                            )));
                        }
                    }
                    Some(queue_method)
                }
                None => {
                    blockers.push(LandBlocker::transient(format!(
                        "GitHub did not report \u{201c}{trunk}\u{201d}'s merge-queue \
                         method — plan again before enqueueing"
                    )));
                    None
                }
            }
        } else {
            pick_method(land, options.method)
        };
        if method.is_none() && !queue_driven {
            blockers.push(LandBlocker::needs_user(match options.method {
                Some(requested) => format!(
                    "{}/{} does not allow {} merges — pick another method",
                    repo.owner,
                    repo.name,
                    requested.label()
                ),
                None => format!(
                    "{}/{} allows none of GitHub's merge methods — check the \
                     repository's merge settings",
                    repo.owner, repo.name
                ),
            }));
        }
        // A draft lands too: the plan marks it ready for review as its own
        // confirmable step instead of sending the user to GitHub, and the
        // candidate is judged as if already promoted.
        let promote = land.is_draft;
        let effective = if promote {
            let mut promoted = land.clone();
            promoted.is_draft = false;
            promoted
        } else {
            land.clone()
        };
        let readiness = classify_candidate(pr.number, &effective, &trunk, &trunk, options);
        match &readiness {
            Readiness::AlreadyMerged => unreachable!("handled during the walk"),
            Readiness::Blocked(reasons) => blockers.extend(reasons.iter().cloned()),
            Readiness::AutomationRunning(story) => warnings.push(story.clone()),
            Readiness::Ready | Readiness::WaitingOnAutomatable(_) => {
                // The pull requests that ride along in the same request
                // are in sync by construction; the first one that is not
                // ends the run and is the one named here.
                let stranded = land
                    .stack
                    .as_ref()
                    .filter(|_| blockers.is_empty())
                    .and_then(|seat| unpublished_above(snapshot, prs, seat));
                if let Some(reason) = stranded {
                    blockers.push(LandBlocker::needs_user(reason));
                }
            }
        }
        // What follows starts landing a stack that has more above this
        // pull request, one pull request at a time. That has to end where
        // one merge of the stack's tip would. It always does while trunk
        // is where the stack was based, and nearly always when trunk has
        // moved. It does not when trunk made a change that a lower pull
        // request makes too and a higher one changes again: the tip as a
        // whole leaves trunk's version alone, and the pull requests in
        // turn replace it (seen on GitHub). So both results are worked
        // out before the first merge, against trunk as the last fetch
        // left it, and a difference stops the plan. GitHub's rebase
        // method replays every commit either way, so there is nothing to
        // compare for it.
        let more_above = raw_segments.len() > *index + 1;
        let starts_landing = matches!(
            readiness,
            Readiness::Ready | Readiness::WaitingOnAutomatable(_)
        );
        if blockers.is_empty()
            && more_above
            && starts_landing
            && method.is_some_and(|m| m != MergeMethod::Rebase)
        {
            let (bottom, first_top) = segment_span(raw);
            let (_, tip) = segment_span(raw_segments.last().expect("the candidate is one of them"));
            match vcs
                .lands_as_tip(&repo.remote, &trunk, &bottom, &first_top, &tip)
                .map_err(|err| ForgeError::Land(err.to_string()))?
            {
                TipCheck::Differs { paths, more } => {
                    blockers.push(LandBlocker::needs_user(format!(
                        "Landing this stack one pull request at a time would not end where \
                         landing its tip in one go would: \u{201c}{trunk}\u{201d} would differ \
                         in {}. \u{201c}{trunk}\u{201d} changed there after the stack was based, \
                         in a way that meets the stack's own changes. Rebase the stack onto \
                         \u{201c}{trunk}\u{201d} and publish first; the pull requests then show \
                         exactly what lands",
                        path_list(&paths, more)
                    )));
                }
                TipCheck::Same | TipCheck::Unknown => {}
            }
        }
        // Every segment this run merges: the candidate, and in a GitHub
        // stack whatever lands with it.
        let mut landing: Vec<usize> = Vec::new();
        if blockers.is_empty() {
            let method = method.expect("a missing method is a blocker");
            if land.checks == ChecksRollup::Failing && land.merge_state.merges_now() {
                warnings.push(format!(
                    "#{} has failing checks GitHub does not require for merging — \
                     landing proceeds past them",
                    pr.number
                ));
            }
            if options.bypass {
                let mut past: Vec<&str> = Vec::new();
                if effective.review == ReviewDecision::ChangesRequested {
                    past.push("requested changes");
                }
                if effective.review == ReviewDecision::ReviewRequired {
                    past.push("the required review");
                }
                if effective.checks == ChecksRollup::Failing
                    && !effective.merge_state.merges_now()
                {
                    past.push("failing checks");
                }
                if effective.checks == ChecksRollup::Pending {
                    past.push("running checks");
                }
                if land.queue_on_base {
                    past.push("the merge queue");
                }
                if !past.is_empty() {
                    warnings.push(format!(
                        "Bypassing {} on #{} — GitHub refuses the merge unless you \
                         may bypass this repository's protections",
                        past.join(", "),
                        pr.number
                    ));
                }
            }
            let mark_ready = LandAction::MarkReadyPr {
                number: pr.number,
                bookmark: raw.bookmark.to_owned(),
            };
            match &readiness {
                Readiness::Ready | Readiness::WaitingOnAutomatable(_)
                    if land.queue_on_base && !options.bypass =>
                {
                    // A queue-protected trunk: enqueue and let GitHub
                    // drive, whatever the readiness.
                    if promote {
                        actions.push(mark_ready);
                    }
                    actions.push(LandAction::EnqueuePr {
                        number: pr.number,
                        bookmark: raw.bookmark.to_owned(),
                        method,
                        expected_head: land.head_commit.clone(),
                    });
                    warnings.push(format!(
                        "\u{201c}{trunk}\u{201d} is protected by a merge queue; GitHub \
                         lands #{} with its configured {} method — run Land again \
                         afterwards to reconcile",
                        pr.number,
                        method.label()
                    ));
                    landing.push(*index);
                }
                Readiness::Ready => {
                    // In a GitHub stack, everything ready above the
                    // candidate lands in the same request. Bypass is the
                    // exception: GitHub allows it for the bottom pull
                    // request alone.
                    let run = match &land.stack {
                        Some(seat) if !options.bypass => stack_run(
                            snapshot,
                            &raw_segments,
                            &placed,
                            *index,
                            seat,
                            &trunk,
                            forge,
                            options,
                        ),
                        _ => Vec::new(),
                    };
                    if promote {
                        actions.push(mark_ready);
                    }
                    for (_, run_pr, run_land) in &run {
                        if run_land.is_draft {
                            actions.push(LandAction::MarkReadyPr {
                                number: run_pr.number,
                                bookmark: run_pr.head_branch.clone(),
                            });
                        }
                    }
                    // The pull request the merge asks for is the top of
                    // the run; the rest ride along, bottom first.
                    let mut members: Vec<(usize, &PrSummary, &PrLandState)> =
                        vec![(*index, pr, land)];
                    members.extend(run.iter().map(|(i, p, l)| (*i, p, l)));
                    let (top_index, top_pr, top_land) =
                        *members.last().expect("the candidate is always a member");
                    let with: Vec<LandsWith> = members[..members.len() - 1]
                        .iter()
                        .map(|(_, p, l)| LandsWith {
                            number: p.number,
                            bookmark: p.head_branch.clone(),
                            expected_head: l.head_commit.clone(),
                        })
                        .collect();
                    if !with.is_empty() && method == MergeMethod::Squash {
                        warnings.push(format!(
                            "GitHub lands {} in one request and writes the squash \
                             commits below #{} itself, from each pull request's title \
                             and commits",
                            number_list(members.iter().map(|(_, p, _)| p.number)),
                            top_pr.number
                        ));
                    }
                    let (commit_title, commit_message) = match method {
                        MergeMethod::Squash => {
                            let (title, message) =
                                squash_commit(top_pr, &raw_segments[top_index].nodes);
                            (Some(title), Some(message))
                        }
                        _ => (None, None),
                    };
                    actions.push(LandAction::MergePr {
                        number: top_pr.number,
                        bookmark: top_pr.head_branch.clone(),
                        method,
                        expected_head: top_land.head_commit.clone(),
                        commit_title,
                        commit_message,
                        with,
                        stack: land.stack.as_ref().map(|seat| seat.number),
                    });
                    landing.extend(members.iter().map(|(i, _, _)| *i));
                    // The next PR up can arm auto-merge once this merge's
                    // reconcile has rebased and retargeted it.
                    let chain_method = (!options.bypass
                        && land.auto_merge_allowed
                        && !land.queue_on_base)
                        .then_some(method);
                    let landed: Vec<(usize, u64)> =
                        members.iter().map(|(i, p, _)| (*i, p.number)).collect();
                    actions.extend(reconcile_actions(
                        snapshot,
                        prs,
                        repo,
                        &raw_segments,
                        &placed,
                        &landed,
                        &others,
                        &trunk,
                        Some(method),
                        chain_method,
                        &mut warnings,
                        &mut blockers,
                    ));
                }
                Readiness::WaitingOnAutomatable(waits) if land.stack.is_some() => {
                    blockers.extend(waits.iter().map(|wait| {
                        LandBlocker::transient(format!(
                            "{wait} — GitHub has no auto-merge for stacked pull \
                             requests, so land again when it settles"
                        ))
                    }));
                }
                Readiness::WaitingOnAutomatable(waits) => {
                    if land.auto_merge_allowed {
                        if promote {
                            actions.push(mark_ready);
                        }
                        let (commit_title, commit_message) = match method {
                            MergeMethod::Squash => {
                                let (title, message) = squash_commit(pr, &raw.nodes);
                                (Some(title), Some(message))
                            }
                            _ => (None, None),
                        };
                        actions.push(LandAction::EnableAutoMerge {
                            number: pr.number,
                            bookmark: raw.bookmark.to_owned(),
                            method,
                            expected_head: Some(land.head_commit.clone()),
                            commit_title,
                            commit_message,
                        });
                        warnings.push(format!(
                            "{} — auto-merge hands the wait to GitHub",
                            waits.join("; ")
                        ));
                        landing.push(*index);
                    } else {
                        blockers.extend(waits.iter().map(|wait| {
                            LandBlocker::transient(format!(
                                "{wait} — this repository has auto-merge disabled, \
                                 so land again when it settles"
                            ))
                        }));
                    }
                }
                _ => {}
            }
        }
        for p in placed.iter_mut() {
            if blockers.is_empty() && (landing.contains(&p.raw_index) || p.raw_index == *index) {
                p.status = LandSegmentStatus::Landing;
            }
        }
    }

    let segments: Vec<LandSegment> = placed
        .into_iter()
        .map(|p| LandSegment {
            bookmark: p.bookmark.to_owned(),
            change_ids: p.change_ids,
            pr: p.pr,
            status: p.status,
        })
        .collect();

    Ok(LandPlan {
        head_bookmark: head_bookmark.to_owned(),
        remote: repo.remote.clone(),
        base_branch: trunk,
        options: *options,
        segments,
        actions,
        blockers,
        warnings,
    })
}

/// "#12, #13 and #14".
pub(crate) fn number_list(numbers: impl Iterator<Item = u64>) -> String {
    let numbers: Vec<String> = numbers.map(|n| format!("#{n}")).collect();
    match numbers.as_slice() {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

/// The reconcile tail shared by both plan flavors: fetch, check that
/// trunk really holds each landed segment, retarget the next PR, rebase
/// whatever still sits on the landed segments onto the new trunk, push
/// the rebased live bookmarks, then clean up each landed segment's
/// bookmark and changes. `landed` holds the landed segments' raw indices
/// with their pull request numbers, bottom first; `merge_method` is known
/// when this run performs the merge — a merge-commit landing skips the
/// abandon, since the local changes become trunk ancestry — and `None`
/// for GitHub-side merges, where the executor decides from the refreshed
/// snapshot. A `chain_method` arms auto-merge on the next PR as the run's
/// coda, so GitHub lands it once its rebased checks pass.
///
/// The check comes straight after the fetch and before everything else:
/// a merge that did not put the segment's changes on trunk must stop the
/// run while nothing here has been rebased, pushed, deleted or abandoned
/// on its account. The retarget comes next, ahead of the rebase and
/// push, so the next PR points at trunk for as short a time as possible
/// after its old base landed; merged there, it would land in a branch
/// nobody keeps.
#[allow(clippy::too_many_arguments)]
fn reconcile_actions(
    snapshot: &RepoSnapshot,
    prs: &RepoPrState,
    repo: &ForgeRepo,
    raw_segments: &[crate::submit::RawSegment<'_>],
    placed: &[PlacedSegment<'_>],
    landed: &[(usize, u64)],
    others: &[&PrSummary],
    trunk: &str,
    merge_method: Option<MergeMethod>,
    chain_method: Option<MergeMethod>,
    warnings: &mut Vec<String>,
    blockers: &mut Vec<LandBlocker>,
) -> Vec<LandAction> {
    let mut actions: Vec<LandAction> = Vec::new();
    let top = landed
        .iter()
        .map(|(index, _)| *index)
        .max()
        .expect("landed is never empty");

    // In a GitHub stack, GitHub rebases what is left above a merge and
    // pushes those branches itself, a moment after reporting the merge.
    // The rebase and push planned below would race it. So when the
    // lowest pull request left in the stack has a bookmark here (whether
    // or not this plan reaches that far up), in sync, and GitHub's branch
    // is still that bookmark's commit, wait for the branch to move before
    // fetching. A branch that already differs has moved, and the fetch
    // brings it; one with no bookmark here is not rebased or pushed from
    // here at all.
    let top_seat = placed
        .iter()
        .find(|p| p.raw_index == top)
        .and_then(|p| p.seat.as_ref());
    if let Some(seat) = top_seat {
        let above = prs
            .report
            .prs
            .iter()
            .filter(|pr| {
                pr.stack
                    .as_ref()
                    .is_some_and(|s| s.number == seat.number && s.position > seat.position)
            })
            .min_by_key(|pr| pr.stack.as_ref().map(|s| s.position));
        if let Some(above) = above {
            let bookmark = snapshot
                .bookmarks
                .iter()
                .find(|b| b.name == above.head_branch && b.is_local)
                .filter(|_| prs.of_this_repository(above));
            let here = bookmark.and_then(|b| snapshot.nodes.iter().find(|n| n.id == b.target));
            if bookmark.is_some_and(|b| b.sync != SyncState::Synced) {
                // A plan that merges is blocked before it gets here. This
                // is a merge someone already made: GitHub has rebased its
                // copy or is about to, and no order of steps prevents the
                // two copies. Say so before the fetch shows them.
                warnings.push(format!(
                    "\u{201c}{}\u{201d} (#{}) differs here from its branch on GitHub, and \
                     GitHub rebases its own copy of that branch once the pull request below \
                     it has merged. If the fetch brings that copy, the change exists twice: \
                     abandon the copy you do not want, point \u{201c}{}\u{201d} at the other, \
                     and land again",
                    above.head_branch, above.number, above.head_branch
                ));
            } else if here.is_some_and(|node| same_commit(&above.head_commit, &node.commit_id)) {
                actions.push(LandAction::AwaitRestack {
                    number: above.number,
                    bookmark: above.head_branch.clone(),
                    from_head: above.head_commit.clone(),
                    merged_here: merge_method.is_some(),
                });
            }
        }
    }

    actions.push(LandAction::FetchRemote {
        remote: repo.remote.clone(),
    });

    for (index, number) in landed {
        let raw = &raw_segments[*index];
        let (bottom_commit, top_commit) = segment_span(raw);
        actions.push(LandAction::CheckLanded {
            number: *number,
            bookmark: raw.bookmark.to_owned(),
            bottom_commit,
            top_commit,
        });
    }

    // The next open PR above the landed set points at trunk now. GitHub
    // retargets it automatically when the old base branch is deleted, and
    // always in a GitHub stack (where it refuses anyone else's retarget);
    // the executor re-checks and skips politely when that already
    // happened.
    //
    // Only the segment directly above qualifies. One further up, past a
    // segment that has no pull request, still contains that segment's
    // commits: pointed at trunk, its pull request would show and land
    // work nobody opened a pull request for.
    let next = placed
        .iter()
        .find(|p| p.raw_index == top + 1)
        .filter(|p| p.pr.is_some());
    let next_stacked = next.is_some_and(|p| p.seat.is_some());
    if let Some(next) = next {
        let pr = next.pr.as_ref().expect("next was selected for having a PR");
        if pr.base_branch != trunk && !next_stacked {
            actions.push(LandAction::RetargetPr {
                number: pr.number,
                bookmark: pr.head_branch.clone(),
                to_base: trunk.to_owned(),
            });
        }
    }

    // Someone else's pull requests on this stack. Land never merges one
    // and never pushes to its branch. One based on a branch this run
    // lands is pointed at trunk before the branch is deleted (GitHub
    // closes a pull request with its base branch), ahead of the rebase
    // and push like the stack's own next pull request.
    //
    // One in a GitHub stack is GitHub's to move and nobody else's: it
    // refuses a retarget there. In the stack this landing is part of,
    // GitHub moves and rebases what is left above a merge itself. A
    // GitHub stack of someone else's that starts on a branch this run
    // lands is another matter: nothing would move it, the branch could
    // not be deleted without closing its bottom pull request, and a
    // landed bookmark that stays is reconciled again by every later run.
    // That is stopped here, before anything merges.
    let landed_branches: Vec<&str> = landed
        .iter()
        .map(|(index, _)| raw_segments[*index].bookmark)
        .collect();
    let starts_on_what_lands = |other: &PrSummary| {
        other
            .stack
            .as_ref()
            .is_some_and(|seat| landed_branches.contains(&seat.base.as_str()))
    };
    let mut stacks_of_others: Vec<(u64, &str)> = others
        .iter()
        .filter(|other| starts_on_what_lands(other))
        .filter_map(|other| other.stack.as_ref())
        .map(|seat| (seat.number, seat.base.as_str()))
        .collect();
    stacks_of_others.sort_unstable();
    stacks_of_others.dedup();
    for (stack, base) in stacks_of_others {
        let members: Vec<String> = others
            .iter()
            .filter(|other| other.stack.as_ref().is_some_and(|seat| seat.number == stack))
            .map(|other| other.by_author())
            .collect();
        let one = members.len() == 1;
        blockers.push(LandBlocker::needs_user(format!(
            "{} {} in GitHub stack {stack}, a stack of {} own that starts on \
             \u{201c}{base}\u{201d}. GitHub lets no one else change a base inside a stack, so \
             {} cannot be pointed at \u{201c}{trunk}\u{201d}, and \u{201c}{base}\u{201d} could \
             not be deleted after landing without GitHub closing a pull request \u{2014} ask \
             {} to move that stack off \u{201c}{base}\u{201d}, or to dissolve it, and land then",
            match members.as_slice() {
                [one] => one.clone(),
                [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
                [] => unreachable!("a stack listed here has a member"),
            },
            if one { "is" } else { "are" },
            if one { "its" } else { "their" },
            if one { "it" } else { "they" },
            if one { "its author" } else { "their authors" },
        )));
    }
    for other in others.iter().filter(|other| other.stack.is_none()) {
        if landed_branches.contains(&other.base_branch.as_str()) {
            actions.push(LandAction::RetargetOther {
                number: other.number,
                author: other.author.clone(),
                branch: other.head_branch.clone(),
                from_base: other.base_branch.clone(),
                to_base: trunk.to_owned(),
            });
        }
    }

    // Chains to rebase: every drawn child of any landed change that is
    // not itself landed. Usually exactly one — the next segment's bottom
    // change — but loose work off a mid-segment landed change gets its
    // own chain too: the abandon would otherwise reparent it onto stale
    // trunk history instead of the new trunk.
    let landed_ids: std::collections::HashSet<&str> = landed
        .iter()
        .flat_map(|(i, _)| raw_segments[*i].nodes.iter().map(|n| n.id.as_str()))
        .collect();
    // Nodes render children before parents, so one forward pass has every
    // node's children already counted when it is reached.
    let mut subtree_sizes: std::collections::HashMap<&str, u32> =
        std::collections::HashMap::new();
    for node in &snapshot.nodes {
        let own: u32 = 1 + snapshot
            .nodes
            .iter()
            .filter(|c| c.parents.contains(&node.id))
            .map(|c| subtree_sizes.get(c.id.as_str()).copied().unwrap_or(1))
            .sum::<u32>();
        subtree_sizes.insert(node.id.as_str(), own);
    }
    // Never settled history: a landed segment that is trunk history
    // already (see `landed_under`) has trunk's own commits above it too.
    // A change someone else's branch holds is the user's and moves like
    // any other.
    for child in snapshot.nodes.iter().filter(|n| {
        !n.is_settled()
            && !landed_ids.contains(n.id.as_str())
            && n.parents.iter().any(|p| landed_ids.contains(p.as_str()))
    }) {
        // The landed commits it sits on, which trunk replaces. Its other
        // parents stay.
        let off: Vec<String> = landed
            .iter()
            .flat_map(|(i, _)| raw_segments[*i].nodes.iter())
            .filter(|n| child.parents.contains(&n.id))
            .map(|n| n.commit_id.clone())
            .collect();
        actions.push(LandAction::RebaseOntoTrunk {
            root_change: child.id.clone(),
            moves: subtree_sizes.get(child.id.as_str()).copied().unwrap_or(1),
            off,
        });
    }

    // Push the rebased stack's tracked bookmarks so the remaining PRs show
    // the rebased commits. Untracked (never-pushed) bookmarks stay local —
    // publishing them is Publish's job.
    let live_bookmarks: Vec<String> = placed
        .iter()
        .filter(|p| p.raw_index > top)
        .filter(|p| {
            snapshot
                .bookmarks
                .iter()
                .any(|b| b.name == p.bookmark && b.remote.is_some())
        })
        .map(|p| p.bookmark.to_owned())
        .collect();
    if !live_bookmarks.is_empty() {
        actions.push(LandAction::PushStack {
            bookmarks: live_bookmarks,
        });
    }

    // What each of their pull requests will show afterwards, said before
    // anything runs. A pull request shows what its branch has that its
    // base does not, by commit; their branches keep this stack's commits
    // as they were. So one whose base landed by a squash or a rebase
    // shows the landed commits as its own, and one whose base this run
    // rebases and pushes shows the replaced ones, until its author
    // rebases it. (A merge commit lands the very commits, and changes
    // nothing for a pull request that sat on them.)
    if !others.is_empty() {
        let names = |prs: &[&PrSummary]| -> String {
            let listed: Vec<String> = prs.iter().map(|pr| pr.by_author()).collect();
            match listed.as_slice() {
                [] => String::new(),
                [one] => one.clone(),
                [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
            }
        };
        let pushed: Vec<&str> = actions
            .iter()
            .filter_map(|action| match action {
                LandAction::PushStack { bookmarks } => Some(bookmarks),
                _ => None,
            })
            .flatten()
            .map(String::as_str)
            .collect();
        // Whether a landed branch reaches trunk with its own commits.
        let lands_as_it_is = |branch: &str| {
            merge_method == Some(MergeMethod::Merge)
                || (merge_method.is_none()
                    && landed.iter().any(|(index, _)| {
                        raw_segments[*index].bookmark == branch
                            && raw_segments[*index].nodes.iter().all(|n| n.is_settled())
                    }))
        };
        let in_this_stack: Vec<&PrSummary> = others
            .iter()
            .copied()
            .filter(|other| other.stack.is_some() && !starts_on_what_lands(other))
            .collect();
        let plain: Vec<&PrSummary> = others
            .iter()
            .copied()
            .filter(|other| other.stack.is_none())
            .collect();
        if !plain.is_empty() {
            let one = plain.len() == 1;
            let mut note = format!(
                "{} {} on this stack and {} someone else's: Land never merges {} and never \
                 pushes to {}",
                names(&plain),
                if one { "rests" } else { "rest" },
                if one { "is" } else { "are" },
                if one { "it" } else { "them" },
                if one { "its branch" } else { "their branches" },
            );
            for other in &plain {
                let (who, base, branch) = (other.by_author(), &other.base_branch, &other.head_branch);
                if landed_branches.contains(&base.as_str()) {
                    if lands_as_it_is(base) {
                        note.push_str(&format!(
                            ". {who} is pointed at {trunk} before \u{201c}{base}\u{201d} is \
                             deleted, and goes on showing what it shows now: \
                             \u{201c}{base}\u{201d} lands with its own commits"
                        ));
                    } else {
                        note.push_str(&format!(
                            ". {who} is pointed at {trunk} before \u{201c}{base}\u{201d} is \
                             deleted, and then shows the landed commits its branch still \
                             carries as its own until its author rebases \
                             \u{201c}{branch}\u{201d}"
                        ));
                    }
                } else if pushed.contains(&base.as_str()) {
                    note.push_str(&format!(
                        ". {who} stays on \u{201c}{base}\u{201d}, and shows the commits this \
                         run replaces there as its own until its author rebases \
                         \u{201c}{branch}\u{201d}"
                    ));
                }
            }
            warnings.push(note);
        }
        if !in_this_stack.is_empty() {
            warnings.push(format!(
                "{} {} in the same GitHub stack and {} someone else's: Land never merges \
                 {}, and GitHub moves and rebases what is left of a stack itself",
                names(&in_this_stack),
                if in_this_stack.len() == 1 { "is" } else { "are" },
                if in_this_stack.len() == 1 { "is" } else { "are" },
                if in_this_stack.len() == 1 { "it" } else { "them" },
            ));
        }
    }

    // Clean up each landed segment: the bookmarks first, bottom-up (here
    // and on the remote), then the now-redundant local changes, top-down.
    let landed_placed: Vec<&PlacedSegment> = placed
        .iter()
        .filter(|p| landed.iter().any(|(index, _)| *index == p.raw_index))
        .collect();
    for p in &landed_placed {
        actions.push(LandAction::CleanupBookmark {
            bookmark: p.bookmark.to_owned(),
            commit: segment_span(&raw_segments[p.raw_index]).1,
        });
    }
    if merge_method != Some(MergeMethod::Merge) {
        // The top segment first. Abandoning a change rewrites everything
        // above it (jj rebases the descendants onto its parent), and the
        // commits above would then no longer be the ones this plan names:
        // they would be left behind as if someone had edited them (seen
        // on the sandbox with three pull requests landing in one request).
        for p in landed_placed.iter().rev() {
            if p.change_ids.contains(&snapshot.working_copy) {
                warnings.push(format!(
                    "the working copy sits on \u{201c}{}\u{201d}'s landed changes; it \
                     respawns as a fresh empty change when they are swept",
                    p.bookmark
                ));
            }
            let mut newest_first = p.change_ids.clone();
            newest_first.reverse();
            actions.push(LandAction::AbandonLanded {
                bookmark: p.bookmark.to_owned(),
                change_ids: newest_first,
                commits: raw_segments[p.raw_index]
                    .nodes
                    .iter()
                    .rev()
                    .map(|n| n.commit_id.clone())
                    .collect(),
            });
        }
    }

    // The run's coda: arm auto-merge on the next PR — one level up only,
    // higher PRs still point at branches this stack rewrites — so GitHub
    // lands it as its rebased checks pass. Last on purpose: a refused
    // hand-off must not skip the cleanup above. Skipped for drafts; the
    // run that lands them promotes them deliberately. Skipped in a GitHub
    // stack, which has no auto-merge.
    if let (Some(method), Some(next), false) = (chain_method, next, next_stacked) {
        let pr = next.pr.as_ref().expect("next was selected for having a PR");
        // Not for a draft, and not for a segment that carries a conflict
        // (see the candidate's own check).
        let conflicted = raw_segments[next.raw_index]
            .nodes
            .iter()
            .any(|n| n.has_conflict);
        if !pr.is_draft && !conflicted {
            let (commit_title, commit_message) = match method {
                MergeMethod::Squash => {
                    let (title, message) = squash_commit(pr, &raw_segments[next.raw_index].nodes);
                    (Some(title), Some(message))
                }
                _ => (None, None),
            };
            actions.push(LandAction::EnableAutoMerge {
                number: pr.number,
                bookmark: pr.head_branch.clone(),
                method,
                expected_head: None,
                commit_title,
                commit_message,
            });
            warnings.push(format!(
                "#{} auto-merges once its rebased checks pass — run Land again \
                 afterwards to reconcile (an auto-land job does this itself)",
                pr.number
            ));
        }
    }
    actions
}

/// Run a confirmed land plan. The first failure stops execution — later
/// steps report as skipped. The merge step re-checks GitHub just-in-time
/// (never merge against stale facts), and every cleanup step adapts to the
/// refreshed snapshot: GitHub may have deleted the branch on merge, the
/// fetch may have pruned the bookmark, a merge-commit landing turns the
/// local changes into immutable trunk ancestry that must not be abandoned.
pub fn execute_land(
    plan: &LandPlan,
    vcs: &dyn LandVcs,
    forge: &dyn LandForge,
) -> Result<LandOutcome, ForgeError> {
    if !plan.blockers.is_empty() {
        return Err(ForgeError::Land(format!(
            "the plan has blockers: {}",
            plan.blockers
                .iter()
                .map(|b| b.message.as_str())
                .collect::<Vec<_>>()
                .join("; ")
        )));
    }
    let mut steps: Vec<LandStep> = plan
        .actions
        .iter()
        .map(|action| LandStep {
            action: action.clone(),
            status: SubmitStepStatus::Skipped,
            detail: None,
        })
        .collect();
    let mut failed = false;
    for step in &mut steps {
        match run_land_step(&step.action, plan, vcs, forge) {
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
    Ok(LandOutcome { steps, failed })
}

/// Point an open pull request at `to_base`, unless GitHub already did or
/// keeps that to itself: it moves the bases in its own stacks, and refuses
/// anyone else's retarget there.
fn retarget(
    number: u64,
    to_base: &str,
    trunk: &str,
    forge: &dyn LandForge,
) -> Result<String, String> {
    let land = forge
        .pr_land_state(number, trunk)
        .map_err(|err| err.to_string())?;
    if land.state != PrState::Open {
        return Ok(format!(
            "#{number} is no longer open; leaving its base alone"
        ));
    }
    if land.base_branch == to_base {
        return Ok(format!(
            "GitHub already retargeted #{number} onto {to_base}"
        ));
    }
    if land.stack.is_some() {
        return Ok(format!(
            "#{number} is in a GitHub stack; GitHub moves its base itself"
        ));
    }
    forge
        .update_pr_base(number, to_base)
        .map_err(|err| err.to_string())?;
    Ok(format!("Retargeted #{number} onto {to_base}"))
}

/// Point someone else's pull request at `to_base`, the trunk: its base
/// `from_base` has landed and is about to be deleted. Left as it is when
/// it is no longer open, when GitHub already moved it, when it is in a
/// GitHub stack, and when its base is no longer the one the plan saw: its
/// author pointed it somewhere else in the meantime, and that stands.
/// The answer says what the pull request shows from here on. Its branch
/// is not pushed to: it keeps the commits of `from_base` as they were
/// when it was built. `method` is the plan's merge method, when the plan
/// says.
#[allow(clippy::too_many_arguments)]
fn retarget_other(
    number: u64,
    who: &str,
    branch: &str,
    from_base: &str,
    to_base: &str,
    trunk: &str,
    method: Option<MergeMethod>,
    forge: &dyn LandForge,
    vcs: &dyn LandVcs,
) -> Result<String, String> {
    let land = forge
        .pr_land_state(number, trunk)
        .map_err(|err| err.to_string())?;
    if land.state != PrState::Open {
        return Ok(format!("{who} is no longer open; its base was left alone"));
    }
    // Whether the branch landed with its own commits (a merge commit).
    // The plan's method says, when the plan merged it. A run that only
    // reconciles asks the repository: such a branch is trunk's history
    // after the fetch, its bookmark's commit settled or no longer drawn.
    // Everything else landed as other commits.
    //
    // With its own commits on trunk, nothing under their pull request is
    // replaced by this landing: it shows what it showed before. That is
    // only its own changes, unless something replaced commits under it
    // earlier (a published rewrite, or the rebase of an earlier round).
    let landed_as_it_is = match method {
        Some(method) => method == MergeMethod::Merge,
        None => vcs.snapshot().is_ok_and(|snapshot| {
            snapshot
                .bookmarks
                .iter()
                .find(|b| b.name == from_base && b.is_local)
                .is_some_and(|b| {
                    snapshot
                        .nodes
                        .iter()
                        .find(|n| n.id == b.target)
                        .is_none_or(GraphNode::is_settled)
                })
        }),
    };
    let shows = if landed_as_it_is {
        format!(
            "It shows what it showed before: \u{201c}{from_base}\u{201d} landed with its \
             own commits"
        )
    } else {
        format!(
            "It shows the landed commits its branch still carries as its own until its \
             author rebases \u{201c}{branch}\u{201d}"
        )
    };
    if land.base_branch == to_base {
        return Ok(format!("GitHub already pointed {who} at {to_base}. {shows}"));
    }
    if land.base_branch != from_base {
        return Ok(format!(
            "{who} targets \u{201c}{}\u{201d} now, not \u{201c}{from_base}\u{201d}: its base \
             was changed since this plan was made, and was left as it is",
            land.base_branch
        ));
    }
    if land.stack.is_some() {
        return Ok(format!(
            "{who} is in a GitHub stack; GitHub moves its base itself"
        ));
    }
    forge
        .update_pr_base(number, to_base)
        .map_err(|err| err.to_string())?;
    Ok(format!("Pointed {who} at {to_base}. {shows}"))
}

/// The merge method a plan lands with, when the plan says: the one that
/// was asked for, or else the one its own merge or hand-off step carries.
/// A plan that only reconciles, under the automatic method, does not say.
fn plan_method(plan: &LandPlan) -> Option<MergeMethod> {
    plan.options.method.or_else(|| {
        plan.actions.iter().find_map(|action| match action {
            LandAction::MergePr { method, .. }
            | LandAction::EnableAutoMerge { method, .. }
            | LandAction::EnqueuePr { method, .. } => Some(*method),
            _ => None,
        })
    })
}

/// The commits of `root` and of the stack's head, when `root` is below
/// the head on its first-parent line and is not the head itself: what a
/// rebase of `root` carries onto trunk that pull requests will land, and
/// more than one change of it. Nothing for a chain that is not the
/// stack's own (work started off to the side, or the working copy).
fn rest_of_stack(
    snapshot: &RepoSnapshot,
    head_bookmark: &str,
    root: &str,
) -> Option<(String, String)> {
    let head = snapshot
        .bookmarks
        .iter()
        .find(|b| b.name == head_bookmark && b.is_local)?;
    let top = snapshot.nodes.iter().find(|n| n.id == head.target)?;
    let mut at = top;
    while at.id != root {
        if at.is_settled() {
            return None;
        }
        let below = at.parents.first()?;
        at = snapshot.nodes.iter().find(|n| n.id == *below)?;
    }
    (at.id != top.id).then(|| (at.commit_id.clone(), top.commit_id.clone()))
}

/// Whether any change in the mutable history under `bookmark` carries a
/// conflict — the push gate's question, walked over the refreshed
/// snapshot from the bookmark's target down to the immutable base.
fn conflicted_under(snapshot: &RepoSnapshot, bookmark: &str) -> bool {
    let Some(state) = snapshot.bookmarks.iter().find(|b| b.name == bookmark) else {
        return false;
    };
    let by_id: std::collections::HashMap<&str, &GraphNode> =
        snapshot.nodes.iter().map(|n| (n.id.as_str(), n)).collect();
    let mut queue: Vec<&str> = vec![state.target.as_str()];
    let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
    while let Some(id) = queue.pop() {
        if !seen.insert(id) {
            continue;
        }
        let Some(node) = by_id.get(id) else { continue };
        if node.is_settled() {
            continue;
        }
        if node.has_conflict {
            return true;
        }
        queue.extend(node.parents.iter().map(String::as_str));
    }
    false
}

fn stale_head(number: u64) -> String {
    format!(
        "#{number}'s branch moved since this plan was made — review the fresh plan \
         and land again"
    )
}

fn stale_stack(number: u64) -> String {
    format!(
        "the GitHub stack around #{number} changed since this plan was made — review \
         the fresh plan and land again"
    )
}

fn stale_base(number: u64, base: &str, trunk: &str) -> String {
    format!(
        "#{number} now targets \u{201c}{base}\u{201d}, not \u{201c}{trunk}\u{201d} — review \
         the fresh plan and land again"
    )
}

/// Merge one pull request outside any GitHub stack, through the endpoint
/// that has always done it.
#[allow(clippy::too_many_arguments)]
fn merge_plain(
    forge: &dyn LandForge,
    number: u64,
    bookmark: &str,
    method: MergeMethod,
    expected_head: &str,
    commit_title: Option<&str>,
    commit_message: Option<&str>,
    trunk: &str,
) -> Result<String, String> {
    match forge.merge_pr(number, method, expected_head, commit_title, commit_message) {
        Ok(()) => Ok(format!("Merged #{number} ({bookmark}) into {trunk}")),
        Err(err) => {
            // GitHub occasionally errors on a merge that went through;
            // believe the PR over the error.
            if let Ok(after) = forge.pr_land_state(number, trunk) {
                if after.state == PrState::Merged {
                    return Ok(format!(
                        "Merged #{number} ({bookmark}) into {trunk} (GitHub reported an \
                         error, but the merge went through)"
                    ));
                }
            }
            Err(err.to_string())
        }
    }
}

/// Resolve the lease for a GitHub automation mutation. Candidate actions
/// carry the full head read at plan time. A chained auto-merge happens
/// after this same plan rewrites and pushes the next bookmark, so it
/// validates GitHub's fresh full oid against jj's resulting short commit
/// id and leases that oid in the mutation.
fn automation_head_lease(
    number: u64,
    bookmark: &str,
    planned: Option<&str>,
    changes: Option<&[String]>,
    land: &PrLandState,
    vcs: &dyn LandVcs,
) -> Result<String, String> {
    if let Some(expected) = planned {
        return (land.head_commit == expected)
            .then(|| expected.to_owned())
            .ok_or_else(|| stale_head(number));
    }

    let snapshot = vcs.snapshot().map_err(|err| err.to_string())?;
    let state = snapshot
        .bookmarks
        .iter()
        .find(|state| state.name == bookmark && state.is_local)
        .ok_or_else(|| {
            format!(
                "\u{201c}{bookmark}\u{201d} disappeared before auto-merge could be armed — \
                 review the fresh plan and land again"
            )
        })?;
    let node = snapshot
        .nodes
        .iter()
        .find(|node| node.id == state.target)
        .ok_or_else(|| {
            format!(
                "could not resolve \u{201c}{bookmark}\u{201d}'s pushed commit before arming \
                 auto-merge — review the fresh plan and land again"
            )
        })?;
    if node.commit_id.is_empty() || !land.head_commit.starts_with(&node.commit_id) {
        return Err(stale_head(number));
    }
    // The bookmark and GitHub agree. That is not yet the commit this plan
    // was made for: the run's own fetch can bring another one under the
    // same name, when someone pushed to the branch or replaced it while
    // the pull request below it merged, and the bookmark follows. So the
    // bookmark has to hold the changes the plan listed for it, those and
    // no others, down to trunk.
    if let Some(changes) = changes {
        let mut here: Vec<&GraphNode> = Vec::new();
        let mut at = Some(node);
        while let Some(n) = at.filter(|n| !n.is_settled()) {
            here.push(n);
            at = n
                .parents
                .first()
                .and_then(|id| snapshot.nodes.iter().find(|p| p.id == *id));
        }
        here.reverse();
        let same = here.len() == changes.len()
            && here
                .iter()
                .zip(changes)
                .all(|(n, id)| n.id == *id || n.change_id == *id);
        if !same {
            return Err(format!(
                "\u{201c}{bookmark}\u{201d} does not hold what this plan was made for any more: \
                 it has {} now, and the plan listed {}. The fetch can bring that, when the \
                 branch was pushed to or replaced on GitHub in the meantime. Auto-merge was \
                 not armed on #{number} \u{2014} review the fresh plan and land again",
                id_list(here.iter().map(|n| n.id.as_str())),
                id_list(changes.iter().map(String::as_str)),
            ));
        }
    }
    Ok(land.head_commit.clone())
}

/// "a1, a2", or "nothing".
fn id_list<'a>(ids: impl Iterator<Item = &'a str>) -> String {
    let ids: Vec<&str> = ids.collect();
    if ids.is_empty() {
        "nothing".to_owned()
    } else {
        ids.join(", ")
    }
}

/// One step; the answer is the step's plain-language detail, the error the
/// failure story.
fn run_land_step(
    action: &LandAction,
    plan: &LandPlan,
    vcs: &dyn LandVcs,
    forge: &dyn LandForge,
) -> Result<String, String> {
    let trunk = &plan.base_branch;
    match action {
        LandAction::MarkReadyPr { number, .. } => {
            let land = forge
                .pr_land_state(*number, trunk)
                .map_err(|err| err.to_string())?;
            match land.state {
                PrState::Merged => {
                    return Ok(format!(
                        "#{number} already merged on GitHub — run Land again to reconcile"
                    ))
                }
                PrState::Closed => {
                    return Err(format!(
                        "#{number} was closed on GitHub without merging — reopen it \
                         or publish again"
                    ))
                }
                PrState::Open => {}
            }
            if !land.is_draft {
                return Ok(format!("#{number} is already ready for review"));
            }
            forge
                .mark_ready(&land.node_id)
                .map_err(|err| err.to_string())?;
            Ok(format!("Marked #{number} ready for review"))
        }
        LandAction::MergePr {
            number,
            bookmark,
            method,
            expected_head,
            commit_title,
            commit_message,
            with,
            stack,
        } => {
            // Just-in-time re-check, jjpr's posture: never merge against
            // stale facts — judged by the same options the plan was.
            let land = forge
                .pr_land_state(*number, trunk)
                .map_err(|err| err.to_string())?;
            // With pull requests riding along, this one sits above them
            // in a GitHub stack and targets the branch below it.
            let base = with.last().map_or(trunk.as_str(), |below| below.bookmark.as_str());
            match classify_candidate(*number, &land, base, trunk, &plan.options) {
                Readiness::AlreadyMerged => Ok(format!(
                    "#{number} was already merged on GitHub; reconciling"
                )),
                Readiness::AutomationRunning(story) => Err(story),
                Readiness::WaitingOnAutomatable(reasons) => Err(format!(
                    "#{number} is no longer ready to merge: {}",
                    reasons.join("; ")
                )),
                Readiness::Blocked(blockers) => Err(format!(
                    "#{number} is no longer ready to merge: {}",
                    blockers
                        .iter()
                        .map(|b| b.message.as_str())
                        .collect::<Vec<_>>()
                        .join("; ")
                )),
                Readiness::Ready => {
                    if land.head_commit != *expected_head {
                        return Err(stale_head(*number));
                    }
                    // Which endpoint GitHub accepts, and what a merge
                    // takes with it, both hang on the stack. One that was
                    // linked, dissolved or rebuilt since planning is a
                    // different merge from the one that was confirmed.
                    let seat = land.stack.as_ref();
                    if seat.map(|seat| seat.number) != *stack
                        || seat.is_some_and(|seat| seat.base != *trunk)
                    {
                        return Err(stale_stack(*number));
                    }
                    let Some(seat) = seat else {
                        return merge_plain(
                            forge,
                            *number,
                            bookmark,
                            *method,
                            expected_head,
                            commit_title.as_deref(),
                            commit_message.as_deref(),
                            trunk,
                        );
                    };
                    // Everything below lands in the same request: each
                    // must still be the pull request the plan read.
                    let mut below_base = trunk.as_str();
                    for (offset, lower) in with.iter().enumerate() {
                        let fresh = forge
                            .pr_land_state(lower.number, trunk)
                            .map_err(|err| err.to_string())?;
                        // The seats run up to the one asked for.
                        let expected_seat =
                            seat.position.checked_sub((with.len() - offset) as u32);
                        if !fresh.stack.as_ref().is_some_and(|s| {
                            s.number == seat.number && Some(s.position) == expected_seat
                        }) {
                            return Err(stale_stack(lower.number));
                        }
                        if fresh.head_commit != lower.expected_head {
                            return Err(stale_head(lower.number));
                        }
                        match classify_candidate(
                            lower.number,
                            &fresh,
                            below_base,
                            trunk,
                            &plan.options,
                        ) {
                            Readiness::Ready => {}
                            _ => {
                                return Err(format!(
                                    "#{} is no longer ready to merge — review the fresh \
                                     plan and land again",
                                    lower.number
                                ))
                            }
                        }
                        below_base = lower.bookmark.as_str();
                    }
                    // One more look at the pull request asked for, as late
                    // as it can be. GitHub leases its head and nothing
                    // else: if the stack was dissolved since the first
                    // look, the request would merge it into the branch
                    // below. This narrows that to the time one request
                    // takes; GitHub offers nothing that closes it.
                    if !with.is_empty() {
                        let last = forge
                            .pr_land_state(*number, trunk)
                            .map_err(|err| err.to_string())?;
                        let same = last.state == PrState::Open
                            && last.head_commit == *expected_head
                            && last.base_branch == base
                            && last.stack.as_ref().is_some_and(|s| {
                                s.number == seat.number
                                    && s.position == seat.position
                                    && s.base == *trunk
                            });
                        if !same {
                            return Err(stale_stack(*number));
                        }
                    }
                    let together = number_list(
                        with.iter().map(|lower| lower.number).chain([*number]),
                    );
                    match forge.merge_stacked(
                        *number,
                        *method,
                        expected_head,
                        commit_title.as_deref(),
                        commit_message.as_deref(),
                        plan.options.bypass,
                    ) {
                        Ok(AsyncMerge::Merged { .. }) => Ok(format!(
                            "Merged {together} into {trunk} (GitHub stack {})",
                            seat.number
                        )),
                        // Nothing has merged: GitHub lands it from the
                        // queue, and the steps after this one are for a
                        // merge that happened.
                        Ok(AsyncMerge::Enqueued) => Err(format!(
                            "GitHub answered that #{number} is in \u{201c}{trunk}\u{201d}'s \
                             merge queue, so it has not merged yet — it lands from there; \
                             run Land again afterwards to reconcile"
                        )),
                        Err(err) => {
                            if let Ok(after) = forge.pr_land_state(*number, trunk) {
                                if after.state == PrState::Merged {
                                    return Ok(format!(
                                        "Merged {together} into {trunk} (GitHub reported \
                                         an error, but the merge went through)"
                                    ));
                                }
                            }
                            Err(err.to_string())
                        }
                    }
                }
            }
        }
        LandAction::AwaitRestack {
            number,
            from_head,
            merged_here,
            ..
        } => match forge
            .await_restack(*number, from_head)
            .map_err(|err| err.to_string())?
        {
            Restack::Rebased => Ok(format!(
                "GitHub rebased the rest of the stack onto {trunk}"
            )),
            Restack::Conflicting => Ok(format!(
                "GitHub cannot rebase #{number}: it conflicts with \u{201c}{trunk}\u{201d}. \
                 The rest of the stack is rebased here, where the conflict can be resolved"
            )),
            Restack::Unmoved if *merged_here => Err(format!(
                "GitHub has not rebased #{number} yet, and a rebase from here could cross \
                 its own — nothing was fetched, rebased or pushed; land again in a moment"
            )),
            Restack::Unmoved => Ok(format!(
                "GitHub has left #{number} as it was; the rest of the stack is rebased \
                 here instead"
            )),
        },
        LandAction::CheckLanded {
            number,
            bookmark,
            bottom_commit,
            top_commit,
        } => {
            let untouched = "nothing was rebased, pushed or cleaned up";
            let land = forge
                .pr_land_state(*number, trunk)
                .map_err(|err| err.to_string())?;
            if land.state != PrState::Merged {
                return Err(format!(
                    "#{number} does not read as merged on GitHub after all — {untouched}; \
                     land again"
                ));
            }
            let Some(landing) = land.merge_commit.as_deref() else {
                return Err(format!(
                    "GitHub reports #{number} merged without naming the commit, so \
                     there is nothing to check \u{201c}{trunk}\u{201d} against — {untouched}"
                ));
            };
            let short = &landing[..landing.len().min(8)];
            // Whether GitHub merged the commit that is here. A merge in a
            // GitHub stack leases only the head of the pull request asked
            // for, so a push to a lower one in that instant lands with it.
            let same_head = same_commit(&land.head_commit, top_commit);
            match vcs
                .landed(&plan.remote, trunk, landing, &land.head_commit, bottom_commit, top_commit)
                .map_err(|err| err.to_string())?
            {
                LandedCheck::Landed => Ok(format!(
                    "\u{201c}{trunk}\u{201d} holds everything under \u{201c}{bookmark}\u{201d} \
                     (#{number} landed as {short})"
                )),
                LandedCheck::Held if same_head => Ok(format!(
                    "\u{201c}{trunk}\u{201d} holds everything under \u{201c}{bookmark}\u{201d}: \
                     #{number}'s own merge {short} is not what put it there, the changes \
                     arrived another way"
                )),
                LandedCheck::Held => Ok(format!(
                    "\u{201c}{trunk}\u{201d} holds everything under \u{201c}{bookmark}\u{201d}. \
                     #{number} was merged from {}, not from {top_commit} as it is here, so \
                     its merge {short} may hold commits that were pushed from elsewhere",
                    &land.head_commit[..land.head_commit.len().min(top_commit.len().max(8))]
                )),
                LandedCheck::Unfetched => Err(format!(
                    "#{number} merged as {short}, but the fetch did not bring that \
                     commit — {untouched}; land again"
                )),
                LandedCheck::Elsewhere => Err(format!(
                    "{} ({untouched})",
                    merged_elsewhere(*number, bookmark, land.merged_into(), trunk)
                )),
                LandedCheck::Missing { paths, more } => {
                    // Commits trunk already has cannot be rebased onto it
                    // and published again: say what happened instead.
                    // (A leftover asked for by name has no changes
                    // listed; one under a stack is drawn, as history.)
                    let in_trunk = plan
                        .segments
                        .iter()
                        .any(|s| s.bookmark == *bookmark && s.change_ids.is_empty())
                        || vcs.snapshot().is_ok_and(|snapshot| {
                            snapshot.nodes.iter().any(|n| {
                                n.is_settled()
                                    && (same_commit(top_commit, &n.commit_id)
                                        || same_commit(&n.commit_id, top_commit))
                            })
                        });
                    Err(format!(
                        "{} ({untouched})",
                        if in_trunk {
                            kept_nothing(*number, bookmark, trunk, &paths, more)
                        } else {
                            missing_changes(*number, bookmark, trunk, same_head, &paths, more)
                        }
                    ))
                }
            }
        }
        LandAction::EnableAutoMerge {
            number,
            bookmark,
            method,
            expected_head,
            commit_title,
            commit_message,
        } => {
            let land = forge
                .pr_land_state(*number, trunk)
                .map_err(|err| err.to_string())?;
            if land.state == PrState::Merged {
                return Ok(format!(
                    "#{number} already merged on GitHub — run Land again to reconcile"
                ));
            }
            if land.auto_merge_enabled {
                return Ok(format!("auto-merge is already enabled on #{number}"));
            }
            if land.in_merge_queue {
                return Ok(format!("#{number} is already in the merge queue"));
            }
            if land.stack.is_some() {
                return Ok(format!(
                    "#{number} is in a GitHub stack, which has no auto-merge — run \
                     Land again when its checks pass"
                ));
            }
            if land.is_draft {
                return Ok(format!(
                    "#{number} is still a draft — auto-merge cannot arm until it is \
                     marked ready for review"
                ));
            }
            // Auto-merge merges into whatever the base is when the checks
            // pass. Arm it only on a pull request that points at trunk.
            if land.base_branch != *trunk {
                return Err(stale_base(*number, &land.base_branch, trunk));
            }
            // GitHub refuses to arm auto-merge on a PR it would merge
            // outright; skip politely — the next Land run merges it.
            if land.merge_state.merges_now() {
                return Ok(format!(
                    "#{number} is already ready to merge — run Land again to merge \
                     it directly"
                ));
            }
            let changes = plan
                .segments
                .iter()
                .find(|s| s.bookmark == *bookmark)
                .map(|s| s.change_ids.as_slice());
            let expected_head = automation_head_lease(
                *number,
                bookmark,
                expected_head.as_deref(),
                changes,
                &land,
                vcs,
            )?;
            forge
                .enable_auto_merge(
                    &land.node_id,
                    *method,
                    &expected_head,
                    commit_title.as_deref(),
                    commit_message.as_deref(),
                )
                .map_err(|err| err.to_string())?;
            Ok(format!(
                "Auto-merge enabled on #{number}; GitHub merges it once its \
                 requirements are met — run Land again afterwards to reconcile"
            ))
        }
        LandAction::EnqueuePr {
            number,
            method,
            expected_head,
            ..
        } => {
            let land = forge
                .pr_land_state(*number, trunk)
                .map_err(|err| err.to_string())?;
            if land.state == PrState::Merged {
                return Ok(format!(
                    "#{number} already merged on GitHub — run Land again to reconcile"
                ));
            }
            if land.in_merge_queue {
                return Ok(format!("#{number} is already in the merge queue"));
            }
            if land.queue_method != Some(*method) {
                return Err(format!(
                    "\u{201c}{trunk}\u{201d}'s merge-queue method changed since this \
                     plan was made — review the fresh plan and land again"
                ));
            }
            if land.head_commit != *expected_head {
                return Err(stale_head(*number));
            }
            // The queue asked about is trunk's. A pull request that
            // points elsewhere by now would join another branch's queue.
            if land.base_branch != *trunk {
                return Err(stale_base(*number, &land.base_branch, trunk));
            }
            if let Some(seat) = &land.stack {
                if seat.base != *trunk {
                    return Err(stale_stack(*number));
                }
                let answer = forge
                    .enqueue_stacked(*number, expected_head)
                    .map_err(|err| err.to_string())?;
                if matches!(answer, AsyncMerge::Merged { .. }) {
                    return Ok(format!(
                        "#{number} already merged on GitHub — run Land again to reconcile"
                    ));
                }
            } else {
                forge
                    .enqueue_pr(&land.node_id, expected_head)
                    .map_err(|err| err.to_string())?;
            }
            Ok(format!(
                "#{number} added to the merge queue — run Land again once it lands"
            ))
        }
        LandAction::FetchRemote { remote } => {
            vcs.git_fetch(remote).map_err(|err| err.to_string())
        }
        LandAction::RebaseOntoTrunk {
            root_change, off, ..
        } => {
            let snapshot = vcs.snapshot().map_err(|err| err.to_string())?;
            // The root by the id it answers to now. A change that
            // answers to its commit ids has more than one commit since
            // the plan was made: the fetch brought a second copy.
            let root_now: String;
            let root_change: &String = if snapshot.nodes.iter().any(|n| n.id == *root_change) {
                root_change
            } else {
                let copies: Vec<&GraphNode> = snapshot
                    .nodes
                    .iter()
                    .filter(|n| n.change_id == *root_change)
                    .collect();
                match copies.as_slice() {
                    [] => {
                        return Ok(format!(
                            "{root_change} is already gone — nothing left to rebase"
                        ))
                    }
                    // One commit of it is here, and every other is what
                    // someone else's branch keeps of an earlier version:
                    // the fetch brought their branch, built before this
                    // change was rewritten and published. Those copies
                    // are not the user's to abandon, and this one is the
                    // change. (The old copies stay in view for as long as
                    // their branch keeps them: nothing here moves it.)
                    [only] if !only.diverged_here() => {
                        root_now = only.id.clone();
                        &root_now
                    }
                    // GitHub rebased its own copy after a merge in its
                    // stack, while the one here had changes GitHub did
                    // not have.
                    _ => {
                        return Err(format!(
                            "{root_change} exists more than once here now ({}): the fetch \
                             brought a copy GitHub rebased, and the one here had changes \
                             GitHub did not have. Nothing was rebased or pushed — abandon the \
                             copy you do not want, point its bookmark at the other, and land \
                             again",
                            copies
                                .iter()
                                .map(|n| n.commit_id.as_str())
                                .collect::<Vec<_>>()
                                .join(", ")
                        ))
                    }
                }
            };
            // This rebase replays what is left of the stack onto trunk
            // one change at a time, and the pull requests then land what
            // it made. That has to end where one merge of the stack's tip
            // would, the question the plan asks before a first merge. It
            // is asked again here, where the bottom of the stack has
            // landed already and the plan had nothing to ask it about,
            // and against trunk as the fetch just left it. A difference
            // stops the run before this part of the stack is rewritten,
            // and before anything is pushed. Not for the rebase method,
            // as in the plan: GitHub then replays every commit of the tip
            // too, and there is nothing for the two to differ in.
            let replayed_either_way = plan_method(plan) == Some(MergeMethod::Rebase);
            let rest = rest_of_stack(&snapshot, &plan.head_bookmark, root_change)
                .filter(|_| !replayed_either_way);
            if let Some((bottom, tip)) = rest {
                let check = vcs
                    .lands_as_tip(&plan.remote, trunk, &bottom, &bottom, &tip)
                    .map_err(|err| err.to_string())?;
                if let TipCheck::Differs { paths, more } = check {
                    return Err(format!(
                        "What is left of this stack would not end on \u{201c}{trunk}\u{201d} as \
                         its tip would in one go: rebased one change at a time, \
                         \u{201c}{trunk}\u{201d} would differ in {}. \u{201c}{trunk}\u{201d} changed \
                         there in a way that meets the stack's own changes. {root_change} and \
                         what is above it were not rebased, and nothing was pushed \u{2014} \
                         rebase {root_change} onto \u{201c}{trunk}\u{201d} yourself, look at \
                         what it does there, publish, and land again",
                        path_list(&paths, more)
                    ));
                }
            }
            // The backend names the destination itself and answers
            // plainly when the chain is already on it.
            vcs.rebase_onto_trunk(&plan.remote, trunk, root_change, off)
                .map_err(|err| err.to_string())
        }
        LandAction::PushStack { bookmarks } => {
            let snapshot = vcs.snapshot().map_err(|err| err.to_string())?;
            let live: Vec<String> = bookmarks
                .iter()
                .filter(|name| {
                    snapshot
                        .bookmarks
                        .iter()
                        .any(|b| b.name == **name && b.is_local && b.remote.is_some())
                })
                .cloned()
                .collect();
            if live.is_empty() {
                return Ok("nothing left to push".to_owned());
            }
            // The rebase records conflicts inside changes instead of
            // failing — and those markers must never reach the open PRs.
            // Stopping here leaves the cleanup steps skipped; resolving
            // and re-running Land plans the same reconcile again.
            let conflicted: Vec<&String> = live
                .iter()
                .filter(|name| conflicted_under(&snapshot, name))
                .collect();
            if !conflicted.is_empty() {
                return Err(format!(
                    "the rebase left conflicts under {} — resolve them locally, \
                     then run Land again to push and continue",
                    conflicted
                        .iter()
                        .map(|name| format!("\u{201c}{name}\u{201d}"))
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
            vcs.push_bookmarks(&live, &plan.remote)
                .map_err(|err| err.to_string())
        }
        LandAction::RetargetPr {
            number, to_base, ..
        } => retarget(*number, to_base, trunk, forge),
        LandAction::RetargetOther {
            number,
            author,
            branch,
            from_base,
            to_base,
        } => {
            let who = match author {
                Some(login) => format!("#{number} by @{login}"),
                None => format!("#{number}"),
            };
            // Someone else's pull request that cannot be pointed away is
            // not a reason to stop the landing half-way: the stack's own
            // steps go on, and the cleanup leaves the branch it is based
            // on in place (it asks GitHub before it deletes one).
            let method = plan_method(plan);
            Ok(retarget_other(
                *number, &who, branch, from_base, to_base, trunk, method, forge, vcs,
            )
            .unwrap_or_else(|err| {
                format!(
                    "{who} could not be pointed at {to_base} ({err}). \
                     \u{201c}{from_base}\u{201d} stays in place for it"
                )
            }))
        }
        LandAction::CleanupBookmark { bookmark, commit } => {
            let snapshot = vcs.snapshot().map_err(|err| err.to_string())?;
            let Some(state) = snapshot.bookmarks.iter().find(|b| b.name == *bookmark)
            else {
                return Ok(format!(
                    "\u{201c}{bookmark}\u{201d} is already gone — GitHub deleted it \
                     on merge and the fetch pruned it here"
                ));
            };
            let had_remote = state.remote.is_some();
            let was_local = state.is_local;
            // What landed is the commit the plan names. A bookmark that
            // points anywhere else was moved since, by an edit here or by
            // a push to its branch that the fetch brought: deleting it
            // would delete that, here and on the remote.
            //
            // The snapshot draws a bookmark's commit only while it is
            // outside trunk's history. One it does not draw (a segment a
            // merge commit landed) is left to the backend, which compares
            // inside the mutation either way.
            let at = snapshot
                .nodes
                .iter()
                .find(|n| n.id == state.target)
                .map_or("", |n| n.commit_id.as_str());
            // (Either id may be the longer one: the plan names a
            // snapshot's unique prefix, or the full id GitHub gave. This
            // look only words the answer; the backend's is exact.)
            let moved = !same_commit(at, commit) && !same_commit(commit, at);
            if !commit.is_empty() && !at.is_empty() && moved {
                // The usual reason: the branch got commits on GitHub that
                // were not here ("Update branch") before it was merged
                // there, and this run's fetch brought them. The bookmark
                // then sits on the commit GitHub merged, which the check
                // did not look at this time. The next Land plans from that
                // commit and cleans up.
                let merged_from = plan.actions.iter().find_map(|a| match a {
                    LandAction::CheckLanded {
                        number, bookmark: b, ..
                    } if b == bookmark => forge.pr_land_state(*number, trunk).ok().map(|l| (*number, l)),
                    _ => None,
                });
                if let Some((number, land)) = merged_from {
                    if same_commit(&land.head_commit, at) {
                        return Ok(format!(
                            "\u{201c}{bookmark}\u{201d} moved to {at} with the fetch: the commit \
                             #{number} was merged from on GitHub, which had more than was \
                             here. It was left in place this time, here and on {} — land \
                             again to clean it up",
                            plan.remote
                        ));
                    }
                }
                return Ok(format!(
                    "\u{201c}{bookmark}\u{201d} points at {at} now, not at {commit}, which is \
                     what landed — left in place, here and on {}; look at what it holds",
                    plan.remote
                ));
            }
            if had_remote {
                // GitHub closes every open pull request based on a branch
                // the moment the branch is deleted, merged or not (seen
                // on the sandbox). The plan pointed the ones it knew of
                // at trunk. Asked once more here, of GitHub and by base:
                // one opened since, one the batched list left out, or one
                // whose retarget did not go through must not be closed by
                // this cleanup.
                let based = forge.open_prs_based_on(bookmark).map_err(|err| {
                    format!(
                        "could not ask GitHub which pull requests are based on \
                         \u{201c}{bookmark}\u{201d} ({err}); nothing was deleted, here or on \
                         {} \u{2014} land again",
                        plan.remote
                    )
                })?;
                if !based.is_empty() {
                    // By author when it is someone else's: one whose
                    // branch is a bookmark here needs no name.
                    let listed: Vec<String> = based
                        .iter()
                        .map(|pr| {
                            let own = snapshot
                                .bookmarks
                                .iter()
                                .any(|b| b.is_local && b.name == pr.head_branch);
                            if own {
                                format!("#{}", pr.number)
                            } else {
                                pr.by_author()
                            }
                        })
                        .collect();
                    let (verb, them) = match listed.len() {
                        1 => ("is", "it"),
                        _ => ("are", "them"),
                    };
                    // In a GitHub stack only GitHub changes a base. It
                    // does after merging through the stack, sometimes a
                    // little late; it does not after a push that put the
                    // commits on trunk (seen on the sandbox), and then
                    // the pull request stays on this branch until the
                    // stack is dissolved.
                    let stacked = based.iter().any(|pr| {
                        pr.stack.is_some()
                            || forge
                                .pr_land_state(pr.number, trunk)
                                .is_ok_and(|land| land.stack.is_some())
                    });
                    let way_out = if stacked {
                        format!(
                            "In a GitHub stack only GitHub changes a base: land again in a \
                             moment, and if {them} {verb} still based on \
                             \u{201c}{bookmark}\u{201d} then, dissolve the stack (publish \
                             --no-stack) so that {them} can be pointed elsewhere"
                        )
                    } else {
                        format!("Point {them} at another branch, and land again to clean up")
                    };
                    return Ok(format!(
                        "\u{201c}{bookmark}\u{201d} was left in place, here and on {}: {} {verb} \
                         still based on it, and GitHub closes a pull request when its base \
                         branch is deleted. {way_out}",
                        plan.remote,
                        match listed.as_slice() {
                            [one] => one.clone(),
                            [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
                            [] => unreachable!("checked non-empty"),
                        }
                    ));
                }
            }
            if was_local {
                // The backend compares once more, inside the mutation.
                vcs.delete_bookmark_at(bookmark, if commit.is_empty() { at } else { commit })
                    .map_err(|err| format!("{err} (nothing was deleted on {})", plan.remote))?;
            }
            if had_remote {
                if let Err(err) = vcs.push_bookmarks(std::slice::from_ref(bookmark), &plan.remote) {
                    // Where the repository deletes the branch of a merged
                    // pull request by itself, GitHub does so a moment
                    // after the merge: after this run's fetch, every time
                    // it was tried on the sandbox. The push of the
                    // deletion is then refused, the branch not being
                    // where the fetch left it. A fetch tells whether that
                    // is all there is to it: with the branch gone from
                    // the remote, nothing is left to delete.
                    let gone = vcs.git_fetch(&plan.remote).is_ok()
                        && vcs.snapshot().is_ok_and(|after| {
                            !after
                                .bookmarks
                                .iter()
                                .any(|b| b.name == *bookmark && b.remote.is_some())
                        });
                    if !gone {
                        return Err(err.to_string());
                    }
                    return Ok(if was_local {
                        format!(
                            "Deleted \u{201c}{bookmark}\u{201d} here; GitHub had deleted its \
                             branch on {} already",
                            plan.remote
                        )
                    } else {
                        format!(
                            "\u{201c}{bookmark}\u{201d}'s branch on {} is gone already: GitHub \
                             deleted it",
                            plan.remote
                        )
                    });
                }
            }
            Ok(match (was_local, had_remote) {
                (true, true) => format!(
                    "Deleted \u{201c}{bookmark}\u{201d} here and on {}",
                    plan.remote
                ),
                (true, false) => format!("Deleted \u{201c}{bookmark}\u{201d}"),
                (false, _) => format!(
                    "Removed \u{201c}{bookmark}\u{201d}'s leftover branch on {}",
                    plan.remote
                ),
            })
        }
        LandAction::AbandonLanded {
            bookmark,
            change_ids,
            commits,
        } => {
            let snapshot = vcs.snapshot().map_err(|err| err.to_string())?;
            // Exactly the commits the landing check verified, and only
            // while they are still here and mutable. That is by commit
            // id on purpose. A change id names whatever the change has
            // become, so a change edited after the plan was made would be
            // abandoned with its new work. And a rebase merge lands
            // commits that keep jj's change ids, so after the fetch each
            // landed change exists twice, on trunk and here; the commit
            // id still names the one here.
            //
            // The ids handed on are the planned ones, not the snapshot's:
            // the backend resolves each to the one commit it names and
            // refuses an id that has become ambiguous.
            let planned = |n: &GraphNode| commits.iter().any(|c| same_commit(&n.commit_id, c));
            // The cleanup before this left the bookmark where it was (a
            // pull request is still based on its branch). Abandoning the
            // changes would delete the bookmark with them, and the next
            // push of deletions would delete the branch after all.
            let still_named = snapshot.bookmarks.iter().any(|b| {
                b.name == *bookmark
                    && b.is_local
                    && snapshot.nodes.iter().any(|n| n.id == b.target && planned(n))
            });
            if still_named {
                return Ok(format!(
                    "\u{201c}{bookmark}\u{201d} was left in place, and its changes with it"
                ));
            }
            let sweep: Vec<String> = commits
                .iter()
                .filter(|c| {
                    snapshot
                        .nodes
                        .iter()
                        .any(|n| !n.is_settled() && same_commit(&n.commit_id, c))
                })
                .cloned()
                .collect();
            // A planned change that is here under another commit was
            // edited since. It stays, and is named.
            let edited: Vec<&str> = change_ids
                .iter()
                .filter(|id| {
                    snapshot.nodes.iter().any(|n| {
                        (n.id == **id || n.change_id == **id)
                            && !n.is_settled()
                            && !planned(n)
                    })
                })
                .map(String::as_str)
                .collect();
            let kept = (!edited.is_empty()).then(|| {
                format!(
                    "{} changed here after this plan was made and was left in place",
                    edited.join(", ")
                )
            });
            if sweep.is_empty() {
                // Nothing of it is a change of this stack any more. That
                // is a landing that put these very commits on trunk (a
                // merge commit), or commits that are gone already (a
                // fetch that finds their branch deleted on the remote
                // takes them with it), or
                // commits someone else's branch still builds on, which
                // read as immutable here once their bookmark is deleted.
                // The last is not "part of trunk", and is said as it is.
                let resting = vcs.resting_on(commits).unwrap_or_default();
                if !resting.is_empty() && kept.is_none() {
                    return Ok(format!(
                        "\u{201c}{bookmark}\u{201d}'s landed changes stay in place: {} still \
                         {} on them",
                        match resting.as_slice() {
                            [one] => one.clone(),
                            [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
                            [] => unreachable!("checked non-empty"),
                        },
                        if resting.len() == 1 { "builds" } else { "build" },
                    ));
                }
                return Ok(kept.unwrap_or_else(|| {
                    format!(
                        "\u{201c}{bookmark}\u{201d}'s landed changes need no sweeping: they \
                         are part of trunk's history, or gone from here already"
                    )
                }));
            }
            // Whatever sits on these commits and is not one of them goes
            // onto trunk first. The plan moved the children it knew of. A
            // change started on a landed one since then (or one of the
            // planned changes, edited since) would be rebased by the
            // abandon onto what was below the landed commits: trunk as it
            // was before they landed. Its diff would survive and its tree
            // would lose everything that just landed.
            let late: Vec<String> = snapshot
                .nodes
                .iter()
                .filter(|n| {
                    !n.is_settled()
                        && !planned(n)
                        && n.parents.iter().any(|p| {
                            snapshot.nodes.iter().any(|parent| {
                                parent.id == *p
                                    && !parent.is_settled()
                                    && planned(parent)
                            })
                        })
                })
                .map(|n| n.id.clone())
                .collect();
            for child in &late {
                vcs.rebase_onto_trunk(&plan.remote, trunk, child, &sweep).map_err(|err| {
                    format!(
                        "{err} — {child} sits on \u{201c}{bookmark}\u{201d}'s landed changes \
                         and could not be moved onto \u{201c}{trunk}\u{201d}, so nothing was \
                         abandoned; land again to see what is left"
                    )
                })?;
            }
            // The backend refuses the whole set when one of the commits
            // was rewritten in the meantime (an edit in the working copy
            // is recorded as the mutation starts), and when a change that
            // is not in the set sits on it: one started after the look
            // above. Nothing is abandoned then, and its tree stays whole.
            let answer = vcs.abandon_changes(&sweep).map_err(|err| {
                // The landed bookmark is deleted before its changes are
                // swept. With it gone, no later Land finds these commits:
                // what is left is done by hand, and said so.
                let named = snapshot
                    .bookmarks
                    .iter()
                    .any(|b| b.name == *bookmark && b.is_local);
                if named {
                    format!(
                        "{err} — nothing under \u{201c}{bookmark}\u{201d} was abandoned; land \
                         again to see what is left"
                    )
                } else {
                    format!(
                        "{err} — nothing was abandoned. \u{201c}{bookmark}\u{201d} itself is \
                         cleaned up already, so Land will not come back for {}. What landed \
                         of them is on \u{201c}{trunk}\u{201d}: move anything that sits on \
                         them, or was added to them since, onto \u{201c}{trunk}\u{201d}, and \
                         abandon what is left",
                        change_ids.join(", ")
                    )
                }
            })?;
            // Someone else's branch still builds on the landed commits:
            // they stay as part of that branch's history, and that is the
            // whole story of this step.
            if answer.starts_with(jiji_core::backend::LANDED_LEFT_IN_PLACE) {
                return Ok(match kept {
                    Some(kept) => format!("{answer}; {kept}"),
                    None => answer,
                });
            }
            // Told by change id, as the plan listed them.
            let swept: Vec<&str> = change_ids
                .iter()
                .zip(commits)
                .filter(|(_, commit)| sweep.contains(commit))
                .map(|(change, _)| change.as_str())
                .collect();
            let mut story = format!("Abandoned {}", swept.join(", "));
            if !late.is_empty() {
                story.push_str(&format!(
                    "; {} sat on them and went onto \u{201c}{trunk}\u{201d} first",
                    late.join(", ")
                ));
            }
            Ok(match kept {
                Some(kept) => format!("{story}; {kept}"),
                None => story,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pr::PrStateReport;
    use crate::remote::ForgeProvider;
    use jiji_core::snapshot::{BookmarkState, GraphNode, NodeKind};
    use serde_json::json;
    use std::cell::RefCell;
    use std::collections::HashMap;

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
            head_commit: "ca2feedface".into(),
            head_owner: Some("o".into()),
            base_branch: base.into(),
            body: None,
            review: ReviewDecision::None,
            checks: ChecksRollup::None,
            merge_commit: None,
            stack: None,
            author: None,
        }
    }

    /// A PR GitHub merged into trunk, as commit `landed<number>`.
    fn merged_pr(number: u64, head: &str) -> PrSummary {
        PrSummary {
            state: PrState::Merged,
            merge_commit: Some(format!("landed{number}")),
            ..open_pr(number, head, "main")
        }
    }

    /// Seat `position` of `size` in GitHub stack 90 on main.
    fn seat(position: u32, size: u32) -> Option<StackSeat> {
        Some(StackSeat {
            number: 90,
            position,
            size,
            base: "main".into(),
        })
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

    fn ready_state() -> PrLandState {
        PrLandState {
            node_id: "NODE1".into(),
            state: PrState::Open,
            is_draft: false,
            mergeable: Mergeable::Mergeable,
            merge_state: MergeStateStatus::Unknown,
            review: ReviewDecision::Approved,
            checks: ChecksRollup::Passing,
            base_branch: "main".into(),
            head_commit: "ca2feedface".into(),
            merge_commit: None,
            stack: None,
            auto_merge_enabled: false,
            in_merge_queue: false,
            auto_merge_allowed: true,
            queue_on_base: false,
            queue_method: None,
            allows_squash: true,
            allows_merge: true,
            allows_rebase: true,
        }
    }

    fn opts() -> LandOptions {
        LandOptions::default()
    }

    /// main(m) ── a1 ── a2 (auth, synced) ── b1 (profile, ahead, @).
    fn land_snapshot() -> RepoSnapshot {
        snapshot(
            vec![
                node("b1", "profile: avatars", NodeKind::WorkingCopy, &["a2"], &["profile"]),
                node("a2", "auth: sessions", NodeKind::Mutable, &["a1"], &["auth"]),
                node("a1", "auth: login flow", NodeKind::Mutable, &["m"], &[]),
                node("m", "release", NodeKind::Immutable, &[], &["main"]),
            ],
            vec![
                bookmark("main", "m", SyncState::Synced, true),
                bookmark("auth", "a2", SyncState::Synced, false),
                bookmark("profile", "b1", SyncState::Ahead, false),
            ],
        )
    }

    type AutomationCall = (String, MergeMethod, String, Option<String>, Option<String>);

    #[derive(Default)]
    struct StubLandForge {
        merged: HashMap<String, PrSummary>,
        states: HashMap<u64, PrLandState>,
        merge_fails: bool,
        merged_despite_error: bool,
        /// Set by `mark_ready`: later state answers drop the draft flag,
        /// the way GitHub reflects the promotion.
        promoted: RefCell<bool>,
        calls: RefCell<Vec<String>>,
        automation_calls: RefCell<Vec<AutomationCall>>,
        enqueue_calls: RefCell<Vec<(String, String)>>,
        /// PRs a merge call landed in this run, with the commit GitHub
        /// names for each: later state answers read merged, as GitHub's do.
        landed: RefCell<HashMap<u64, String>>,
        /// What a stacked merge takes with it: the PR asked for, then the
        /// ones below it.
        stack_below: HashMap<u64, Vec<u64>>,
        /// `await_restack` answers that GitHub never moved the branch.
        restack_stalls: bool,
        /// `await_restack` answers that GitHub called the branch
        /// conflicting and left it.
        restack_conflicts: bool,
        /// A stacked merge answers that the pull request sits in the
        /// merge queue, and merges nothing.
        answers_enqueued: bool,
        /// Bases changed by `update_pr_base`: later state answers carry
        /// them, as GitHub's do.
        retargeted: RefCell<HashMap<u64, String>>,
        /// Pull requests whose stack is dissolved after that many looks
        /// at them: later answers have no seat, and nothing else differs.
        dissolved_after: HashMap<u64, usize>,
        /// Open pull requests as GitHub lists them by base branch: what
        /// the cleanup asks before it deletes a branch. A retarget made
        /// through this stub moves one, as on GitHub.
        open_by_base: RefCell<Vec<PrSummary>>,
        /// Retargets GitHub refuses, by pull request number.
        retarget_refused: Vec<u64>,
    }

    impl LandForge for StubLandForge {
        fn find_merged_pr(&self, branch: &str) -> Result<Option<PrSummary>, ForgeError> {
            self.calls.borrow_mut().push(format!("find_merged:{branch}"));
            Ok(self.merged.get(branch).cloned())
        }
        fn pr_land_state(&self, number: u64, _base: &str) -> Result<PrLandState, ForgeError> {
            self.calls.borrow_mut().push(format!("land_state:{number}"));
            // A PR that merged before the run answers as GitHub does for
            // one: merged, with the commit it landed as.
            let recognized = self.merged.values().find(|pr| pr.number == number).map(|pr| {
                PrLandState {
                    state: PrState::Merged,
                    base_branch: pr.base_branch.clone(),
                    head_commit: pr.head_commit.clone(),
                    merge_commit: pr.merge_commit.clone(),
                    stack: pr.stack.clone(),
                    ..ready_state()
                }
            });
            let mut state = self
                .states
                .get(&number)
                .cloned()
                .or(recognized)
                .ok_or_else(|| ForgeError::Api(format!("no stub state for #{number}")))?;
            if self.merged_despite_error && self.calls.borrow().len() > 2 {
                state.state = PrState::Merged;
                state.merge_commit = Some(format!("landed{number}"));
            }
            if let Some(commit) = self.landed.borrow().get(&number) {
                state.state = PrState::Merged;
                state.merge_commit = Some(commit.clone());
            }
            if *self.promoted.borrow() {
                state.is_draft = false;
            }
            if let Some(base) = self.retargeted.borrow().get(&number) {
                state.base_branch = base.clone();
            }
            if let Some(looks) = self.dissolved_after.get(&number) {
                let looked = self
                    .calls
                    .borrow()
                    .iter()
                    .filter(|c| **c == format!("land_state:{number}"))
                    .count();
                if looked > *looks {
                    state.stack = None;
                }
            }
            Ok(state)
        }
        fn merge_pr(
            &self,
            number: u64,
            method: MergeMethod,
            _expected_head: &str,
            _commit_title: Option<&str>,
            _commit_message: Option<&str>,
        ) -> Result<(), ForgeError> {
            self.calls
                .borrow_mut()
                .push(format!("merge:{number}:{}", method.rest_name()));
            if self.merge_fails || self.merged_despite_error {
                Err(ForgeError::Api("HTTP 405: merge refused".into()))
            } else {
                self.landed
                    .borrow_mut()
                    .insert(number, format!("landed{number}"));
                Ok(())
            }
        }
        fn enable_auto_merge(
            &self,
            node_id: &str,
            method: MergeMethod,
            expected_head: &str,
            commit_title: Option<&str>,
            commit_message: Option<&str>,
        ) -> Result<(), ForgeError> {
            self.calls
                .borrow_mut()
                .push(format!("auto_merge:{node_id}:{}", method.rest_name()));
            self.automation_calls.borrow_mut().push((
                node_id.to_owned(),
                method,
                expected_head.to_owned(),
                commit_title.map(str::to_owned),
                commit_message.map(str::to_owned),
            ));
            Ok(())
        }
        fn enqueue_pr(&self, node_id: &str, expected_head: &str) -> Result<(), ForgeError> {
            self.calls.borrow_mut().push(format!("enqueue:{node_id}"));
            self.enqueue_calls
                .borrow_mut()
                .push((node_id.to_owned(), expected_head.to_owned()));
            Ok(())
        }
        fn update_pr_base(&self, number: u64, base: &str) -> Result<(), ForgeError> {
            if self.retarget_refused.contains(&number) {
                return Err(ForgeError::Api("HTTP 422: the base cannot be changed".into()));
            }
            self.calls
                .borrow_mut()
                .push(format!("retarget:{number}:{base}"));
            self.retargeted.borrow_mut().insert(number, base.to_owned());
            for pr in self.open_by_base.borrow_mut().iter_mut() {
                if pr.number == number {
                    pr.base_branch = base.to_owned();
                }
            }
            Ok(())
        }
        fn open_prs_based_on(&self, base: &str) -> Result<Vec<PrSummary>, ForgeError> {
            // Not put in `calls`: it is a question, and the tests that
            // read `calls` read what was done.
            Ok(self
                .open_by_base
                .borrow()
                .iter()
                .filter(|pr| pr.base_branch == base)
                .cloned()
                .collect())
        }
        fn mark_ready(&self, node_id: &str) -> Result<(), ForgeError> {
            self.calls.borrow_mut().push(format!("mark_ready:{node_id}"));
            *self.promoted.borrow_mut() = true;
            Ok(())
        }
        fn merge_stacked(
            &self,
            number: u64,
            method: MergeMethod,
            _expected_head: &str,
            _commit_title: Option<&str>,
            _commit_message: Option<&str>,
            bypass: bool,
        ) -> Result<AsyncMerge, ForgeError> {
            self.calls.borrow_mut().push(format!(
                "merge_stacked:{number}:{}{}",
                method.rest_name(),
                if bypass { ":bypass" } else { "" }
            ));
            if self.merge_fails {
                return Err(ForgeError::Api("GitHub did not merge: refused".into()));
            }
            if self.answers_enqueued {
                return Ok(AsyncMerge::Enqueued);
            }
            // GitHub lands everything below the one asked for with it.
            let mut landed = self.landed.borrow_mut();
            landed.insert(number, format!("landed{number}"));
            for below in self.stack_below.get(&number).into_iter().flatten() {
                landed.insert(*below, format!("landed{below}"));
            }
            Ok(AsyncMerge::Merged {
                commit: format!("landed{number}"),
            })
        }
        fn enqueue_stacked(
            &self,
            number: u64,
            _expected_head: &str,
        ) -> Result<AsyncMerge, ForgeError> {
            self.calls.borrow_mut().push(format!("enqueue_stacked:{number}"));
            Ok(AsyncMerge::Enqueued)
        }
        fn await_restack(&self, number: u64, from_head: &str) -> Result<Restack, ForgeError> {
            self.calls
                .borrow_mut()
                .push(format!("await_restack:{number}:{from_head}"));
            Ok(if self.restack_conflicts {
                Restack::Conflicting
            } else if self.restack_stalls {
                Restack::Unmoved
            } else {
                Restack::Rebased
            })
        }
    }

    struct StubVcs {
        /// The repository after the fetch. An abandon changes it the way
        /// jj does: the commits go, and everything above them is
        /// rewritten onto what was below.
        post: RefCell<RepoSnapshot>,
        calls: RefCell<Vec<String>>,
        /// What the repository says of a landing commit; one it has no
        /// answer for is on trunk and holds the changes.
        landings: HashMap<String, LandedCheck>,
        /// Every landing check asked: `landing:bottom..top`.
        asked: RefCell<Vec<String>>,
        /// The pull request head each of those checks was given.
        heads: RefCell<Vec<String>>,
        /// The abandon refuses, as the backend does when one of the
        /// commits was rewritten since.
        abandon_refuses: bool,
        /// Commits of bookmark targets the snapshot does not draw (it
        /// draws nothing inside trunk's history that no mutable change
        /// sits on), by node id. The backend knows them all the same.
        undrawn: HashMap<String, String>,
        /// Every trunk rebase asked for: `root@remote off commit,commit`.
        rebased_off: RefCell<Vec<String>>,
        /// The remote each landing check named.
        remotes: RefCell<Vec<String>>,
        /// The trunk branch every check and trunk rebase named.
        trunks: RefCell<Vec<String>>,
        /// What the repository says of landing one at a time against
        /// the tip, and every question asked: `bottom..first_top..tip`.
        tip: TipCheck,
        tip_asked: RefCell<Vec<String>>,
        /// The sweep answers this and abandons nothing, as the backend
        /// does when someone else's branch still sits on the commits.
        abandon_leaves: Option<String>,
        /// The branches that are no bookmarks here and still have the
        /// landed commits in their history.
        resting: Vec<String>,
        /// Branches whose deletion the remote refuses to take from here,
        /// as it does when the branch is no longer where the last fetch
        /// left it.
        refuses_deletion: Vec<String>,
        /// Branches a fetch finds gone from the remote: GitHub deleted
        /// them itself after the merge.
        gone_at_fetch: Vec<String>,
    }

    impl StubVcs {
        fn new(post: RepoSnapshot) -> Self {
            Self {
                post: RefCell::new(post),
                calls: RefCell::new(vec![]),
                landings: HashMap::new(),
                asked: RefCell::new(vec![]),
                heads: RefCell::new(vec![]),
                abandon_refuses: false,
                undrawn: HashMap::new(),
                rebased_off: RefCell::new(vec![]),
                remotes: RefCell::new(vec![]),
                trunks: RefCell::new(vec![]),
                tip: TipCheck::Same,
                tip_asked: RefCell::new(vec![]),
                abandon_leaves: None,
                resting: Vec::new(),
                refuses_deletion: Vec::new(),
                gone_at_fetch: Vec::new(),
            }
        }

        fn knowing(mut self, target: &str, commit: &str) -> Self {
            self.undrawn.insert(target.to_owned(), commit.to_owned());
            self
        }

        fn saying(mut self, landing: &str, check: LandedCheck) -> Self {
            self.landings.insert(landing.to_owned(), check);
            self
        }
    }

    /// The repository as planning asks it: every merge GitHub reports is
    /// on trunk and holds its changes.
    fn at_plan() -> StubVcs {
        StubVcs::new(land_snapshot())
    }

    impl LandVcs for StubVcs {
        fn git_fetch(&self, remote: &str) -> Result<String, BackendError> {
            self.calls.borrow_mut().push(format!("fetch:{remote}"));
            // A branch that is gone from the remote and is no bookmark
            // here any more is pruned.
            self.post
                .borrow_mut()
                .bookmarks
                .retain(|b| b.is_local || !self.gone_at_fetch.contains(&b.name));
            Ok("Fetched from origin".into())
        }
        fn snapshot(&self) -> Result<RepoSnapshot, BackendError> {
            Ok(self.post.borrow().clone())
        }
        fn rebase_onto_trunk(
            &self,
            remote: &str,
            trunk: &str,
            root_change: &str,
            landed: &[String],
        ) -> Result<String, BackendError> {
            self.trunks.borrow_mut().push(trunk.to_owned());
            self.rebased_off
                .borrow_mut()
                .push(format!("{root_change}@{remote} off {}", landed.join(",")));
            // As the backend answers a chain that already sits on trunk:
            // plainly, and without recording anything.
            let mut post = self.post.borrow_mut();
            let trunk = post.bookmarks.iter().find(|b| b.is_trunk).map(|b| b.target.clone());
            let on_trunk = post
                .nodes
                .iter()
                .find(|n| n.id == root_change)
                .is_some_and(|n| n.parents.len() == 1 && trunk.as_ref() == Some(&n.parents[0]));
            if on_trunk {
                return Ok(format!("{root_change} is already on main"));
            }
            self.calls.borrow_mut().push(format!("rebase:{root_change}"));
            // The chain leaves what it sat on. (Its commit ids stay as
            // the fixture has them: the ids a push of the rebase gave.)
            if let (Some(trunk), Some(root)) =
                (trunk, post.nodes.iter_mut().find(|n| n.id == root_change))
            {
                root.parents = vec![trunk];
            }
            Ok(format!("Rebased {root_change} onto main"))
        }
        fn push_bookmarks(
            &self,
            bookmarks: &[String],
            remote: &str,
        ) -> Result<String, BackendError> {
            self.calls
                .borrow_mut()
                .push(format!("push:{}:{remote}", bookmarks.join(",")));
            if let [only] = bookmarks {
                if self.refuses_deletion.contains(only) {
                    return Err(BackendError::MutationFailed(format!(
                        "{only} moved on the remote since the last fetch; fetch, reconcile, \
                         and push again"
                    )));
                }
            }
            Ok("Pushed".into())
        }
        fn delete_bookmark_at(&self, name: &str, commit: &str) -> Result<String, BackendError> {
            // The backend's own comparison, against the snapshot this
            // stub stands for.
            let post = self.post.borrow();
            let at = post
                .bookmarks
                .iter()
                .find(|b| b.name == name && b.is_local)
                .and_then(|b| {
                    post.nodes
                        .iter()
                        .find(|n| n.id == b.target)
                        .map(|n| n.commit_id.clone())
                        .or_else(|| self.undrawn.get(&b.target).cloned())
                })
                .unwrap_or_default();
            // (The backend resolves a prefix to its one commit; here one
            // id being a prefix of the other stands for that.)
            if at.is_empty() || !(at.starts_with(commit) || commit.starts_with(&at)) {
                return Err(BackendError::MutationFailed(format!(
                    "bookmark \u{201c}{name}\u{201d} no longer points at {commit}; it was left in place"
                )));
            }
            drop(post);
            self.calls.borrow_mut().push(format!("delete:{name}"));
            let mut post = self.post.borrow_mut();
            if self.refuses_deletion.iter().any(|refused| refused == name) {
                // Still a branch of the remote, as far as this
                // repository knows, until a push deletes it or a fetch
                // finds it gone.
                for bookmark in post.bookmarks.iter_mut().filter(|b| b.name == name) {
                    bookmark.is_local = false;
                }
            } else {
                post.bookmarks.retain(|b| !(b.name == name && b.is_local));
            }
            for node in post.nodes.iter_mut() {
                node.bookmarks.retain(|b| b != name);
            }
            Ok(format!("Deleted {name}"))
        }
        fn resting_on(&self, _commits: &[String]) -> Result<Vec<String>, BackendError> {
            Ok(self.resting.clone())
        }
        fn abandon_changes(&self, commits: &[String]) -> Result<String, BackendError> {
            if self.abandon_refuses {
                return Err(BackendError::MutationFailed(format!(
                    "commit {} has been rewritten; refresh and try again",
                    commits[0]
                )));
            }
            if let Some(left) = &self.abandon_leaves {
                return Ok(left.clone());
            }
            self.calls
                .borrow_mut()
                .push(format!("abandon:{}", commits.join(",")));
            // The commits go; every mutable change above one of them is
            // rebased onto what was below, which makes it another commit.
            let mut post = self.post.borrow_mut();
            let gone: Vec<(String, Vec<String>)> = post
                .nodes
                .iter()
                .filter(|n| commits.contains(&n.commit_id))
                .map(|n| (n.id.clone(), n.parents.clone()))
                .collect();
            post.nodes.retain(|n| !gone.iter().any(|(id, _)| *id == n.id));
            let mut moved: Vec<String> = gone.iter().map(|(id, _)| id.clone()).collect();
            // Nodes are listed children first: walk up from the bottom.
            for at in (0..post.nodes.len()).rev() {
                let node = &mut post.nodes[at];
                if node.kind == NodeKind::Immutable
                    || !node.parents.iter().any(|p| moved.contains(p))
                {
                    continue;
                }
                for parent in node.parents.iter_mut() {
                    // Past every abandoned parent, to what was below it.
                    while let Some((_, below)) = gone.iter().find(|(id, _)| id == parent) {
                        *parent = below.first().cloned().unwrap_or_default();
                    }
                }
                node.commit_id = format!("rewritten-{}", node.commit_id);
                moved.push(node.id.clone());
            }
            Ok("Abandoned".into())
        }
        fn lands_as_tip(
            &self,
            _remote: &str,
            trunk: &str,
            bottom: &str,
            first_top: &str,
            tip: &str,
        ) -> Result<TipCheck, BackendError> {
            self.trunks.borrow_mut().push(trunk.to_owned());
            self.tip_asked
                .borrow_mut()
                .push(format!("{bottom}..{first_top}..{tip}"));
            Ok(self.tip.clone())
        }
        fn landed(
            &self,
            remote: &str,
            trunk: &str,
            landing: &str,
            head: &str,
            bottom: &str,
            top: &str,
        ) -> Result<LandedCheck, BackendError> {
            self.trunks.borrow_mut().push(trunk.to_owned());
            self.remotes.borrow_mut().push(remote.to_owned());
            self.heads.borrow_mut().push(head.to_owned());
            self.asked
                .borrow_mut()
                .push(format!("{landing}:{bottom}..{top}"));
            Ok(self
                .landings
                .get(landing)
                .cloned()
                .unwrap_or(LandedCheck::Landed))
        }
    }

    #[test]
    fn ready_candidate_plans_merge_and_full_reconcile() {
        let snap = land_snapshot();
        let prs = pr_state(
            vec![open_pr(1, "auth", "main"), open_pr(7, "profile", "auth")],
            false,
        );
        let mut forge = StubLandForge::default();
        forge.states.insert(1, ready_state());

        let plan = plan_land(&snap, &prs, &forge_repo(), "profile", &forge, &at_plan(), &opts()).unwrap();
        assert!(plan.blockers.is_empty(), "{:?}", plan.blockers);
        assert_eq!(
            plan.actions,
            vec![
                LandAction::MergePr {
                    number: 1,
                    bookmark: "auth".into(),
                    method: MergeMethod::Squash,
                    expected_head: "ca2feedface".into(),
                    // The squash commit: PR-title convention on top, the
                    // segment's change descriptions as the body.
                    commit_title: Some("PR 1 (#1)".into()),
                    commit_message: Some("auth: login flow\n\nauth: sessions".into()),
                    with: vec![],
                    stack: None,
                },
                LandAction::FetchRemote { remote: "origin".into() },
                // The gate: nothing below runs unless main holds a1..a2.
                LandAction::CheckLanded {
                    number: 1,
                    bookmark: "auth".into(),
                    bottom_commit: "ca1".into(),
                    top_commit: "ca2".into(),
                },
                // Retargeted before the rebase and push: the less time #7
                // spends pointing at a branch that has landed, the better.
                LandAction::RetargetPr {
                    number: 7,
                    bookmark: "profile".into(),
                    to_base: "main".into(),
                },
                LandAction::RebaseOntoTrunk {
                    root_change: "b1".into(),
                    moves: 1,
                    off: vec!["ca2".into()],
                },
                LandAction::PushStack { bookmarks: vec!["profile".into()] },
                LandAction::CleanupBookmark {
                    bookmark: "auth".into(),
                    commit: "ca2".into(),
                },
                LandAction::AbandonLanded {
                    bookmark: "auth".into(),
                    change_ids: vec!["a2".into(), "a1".into()],
                    commits: vec!["ca2".into(), "ca1".into()],
                },
                // The coda: the next PR up auto-merges once its rebased
                // checks pass.
                LandAction::EnableAutoMerge {
                    number: 7,
                    bookmark: "profile".into(),
                    method: MergeMethod::Squash,
                    expected_head: None,
                    commit_title: Some("PR 7 (#7)".into()),
                    commit_message: Some("profile: avatars".into()),
                },
            ]
        );
        assert_eq!(plan.segments.len(), 2);
        assert_eq!(plan.segments[0].status, LandSegmentStatus::Landing);
        assert_eq!(plan.segments[1].status, LandSegmentStatus::Stacked);
        // Only the candidate was asked for fresh state.
        assert_eq!(forge.calls.borrow().as_slice(), ["land_state:1"]);
    }

    /// Someone else's open pull request: `head` into `base`, opened by
    /// `author`, with `commit` as its head on GitHub.
    fn their_pr(number: u64, head: &str, base: &str, author: &str, commit: &str) -> PrSummary {
        PrSummary {
            author: Some(author.into()),
            head_commit: commit.into(),
            ..open_pr(number, head, base)
        }
    }

    /// The two-segment stack with two pull requests by other people on
    /// it: #8 on the branch that lands, #9 on the top.
    fn with_others() -> RepoPrState {
        pr_state(
            vec![
                open_pr(1, "auth", "main"),
                open_pr(7, "profile", "auth"),
                their_pr(9, "their-top", "profile", "kim", "c0ffee09"),
                their_pr(8, "their-mid", "auth", "sam", "c0ffee08"),
            ],
            false,
        )
    }

    fn their_states(forge: &mut StubLandForge) {
        forge.states.insert(1, ready_state());
        forge.states.insert(7, PrLandState { base_branch: "auth".into(), ..ready_state() });
        forge.states.insert(
            8,
            PrLandState {
                base_branch: "auth".into(),
                head_commit: "c0ffee08".into(),
                ..ready_state()
            },
        );
        forge.states.insert(
            9,
            PrLandState {
                base_branch: "profile".into(),
                head_commit: "c0ffee09".into(),
                ..ready_state()
            },
        );
        *forge.open_by_base.borrow_mut() = with_others().report.prs;
    }

    /// A pull request someone else opened on the stack is seen through a
    /// landing: pointed at trunk before the branch under it is deleted.
    /// It is never merged, it is no segment of the plan, and its branch
    /// is never pushed to: the plan and the step say what it will show
    /// until its author rebases it.
    #[test]
    fn someone_elses_pull_requests_are_pointed_at_trunk_and_their_branches_left_alone() {
        let snap = land_snapshot();
        let prs = with_others();
        let mut forge = StubLandForge::default();
        their_states(&mut forge);
        let story = |plan: &LandPlan| -> Vec<String> {
            plan.actions
                .iter()
                .map(|action| match action {
                    LandAction::MergePr { number, .. } => format!("merge #{number}"),
                    LandAction::FetchRemote { .. } => "fetch".into(),
                    LandAction::CheckLanded { number, .. } => format!("check #{number}"),
                    LandAction::RetargetPr { number, to_base, .. } => {
                        format!("retarget #{number} -> {to_base}")
                    }
                    LandAction::RetargetOther {
                        number,
                        author,
                        branch,
                        from_base,
                        to_base,
                    } => format!(
                        "retarget #{number} by {} ({branch}) {from_base} -> {to_base}",
                        author.as_deref().unwrap_or("?")
                    ),
                    LandAction::RebaseOntoTrunk { root_change, .. } => {
                        format!("rebase {root_change}")
                    }
                    LandAction::PushStack { bookmarks } => format!("push {}", bookmarks.join(",")),
                    LandAction::CleanupBookmark { bookmark, .. } => format!("clean up {bookmark}"),
                    LandAction::AbandonLanded { bookmark, .. } => format!("abandon {bookmark}"),
                    LandAction::EnableAutoMerge { number, .. } => format!("auto-merge #{number}"),
                    other => format!("{other:?}"),
                })
                .collect()
        };

        let plan = plan_land(&snap, &prs, &forge_repo(), "profile", &forge, &at_plan(), &opts()).unwrap();
        assert!(plan.blockers.is_empty(), "{:?}", plan.blockers);
        // Theirs are no segments: nothing here lands them.
        assert_eq!(
            plan.segments.iter().map(|s| s.bookmark.as_str()).collect::<Vec<_>>(),
            ["auth", "profile"]
        );
        assert_eq!(
            story(&plan),
            [
                "merge #1",
                "fetch",
                "check #1",
                "retarget #7 -> main",
                // #8 is based on the branch that lands and is deleted:
                // pointed at trunk first, like the stack's own next one.
                // #9 keeps its base, which is still there. Neither branch
                // gets a step: nothing here pushes to them.
                "retarget #8 by sam (their-mid) auth -> main",
                "rebase b1",
                "push profile",
                "clean up auth",
                "abandon auth",
                "auto-merge #7",
            ]
        );
        assert_eq!(
            plan.warnings
                .iter()
                .filter(|w| w.contains("someone else's"))
                .collect::<Vec<_>>(),
            ["#8 by @sam and #9 by @kim rest on this stack and are someone else's: Land never \
              merges them and never pushes to their branches. #8 by @sam is pointed at main \
              before \u{201c}auth\u{201d} is deleted, and then shows the landed commits its \
              branch still carries as its own until its author rebases \
              \u{201c}their-mid\u{201d}. #9 by @kim stays on \u{201c}profile\u{201d}, and shows \
              the commits this run replaces there as its own until its author rebases \
              \u{201c}their-top\u{201d}"]
        );

        forge.calls.borrow_mut().clear();
        let vcs = StubVcs::new(post_fetch_snapshot());
        let outcome = execute_land(&plan, &vcs, &forge).unwrap();
        assert!(!outcome.failed, "{:?}", outcome.steps);
        let did = forge.calls.borrow().clone();
        assert!(did.contains(&"retarget:8:main".to_owned()), "{did:?}");
        // The only merge is the stack's own bottom pull request.
        assert_eq!(
            did.iter().filter(|c| c.starts_with("merge")).collect::<Vec<_>>(),
            ["merge:1:squash"]
        );
        assert!(!did.iter().any(|c| c.starts_with("retarget:9")), "{did:?}");
        // The repository is asked for the stack's own steps and no other:
        // no branch but the stack's own is pushed.
        assert_eq!(
            vcs.calls.borrow().as_slice(),
            [
                "fetch:origin",
                "rebase:b1",
                "push:profile:origin",
                "delete:auth",
                "push:auth:origin",
                "abandon:ca2,ca1"
            ]
        );
        let told = outcome
            .steps
            .iter()
            .find(|step| matches!(step.action, LandAction::RetargetOther { .. }))
            .and_then(|step| step.detail.clone())
            .unwrap();
        assert_eq!(
            told,
            "Pointed #8 by @sam at main. It shows the landed commits its branch still carries as \
             its own until its author rebases \u{201c}their-mid\u{201d}"
        );

        // A merge commit lands the very commits their branch sits on:
        // nothing changes in what their pull request shows, and the plan
        // says that instead.
        let by_merge = LandOptions { method: Some(MergeMethod::Merge), ..opts() };
        let mut forge = StubLandForge::default();
        their_states(&mut forge);
        let plan =
            plan_land(&snap, &prs, &forge_repo(), "profile", &forge, &at_plan(), &by_merge).unwrap();
        assert!(matches!(
            plan.actions[0],
            LandAction::MergePr { method: MergeMethod::Merge, .. }
        ));
        assert!(
            plan.warnings.iter().any(|w| w.contains(
                "#8 by @sam is pointed at main before \u{201c}auth\u{201d} is deleted, and goes on \
                 showing what it shows now: \u{201c}auth\u{201d} lands with its own commits."
            )),
            "{:?}",
            plan.warnings
        );
    }

    /// A fetch that brings someone else's branch makes every change of
    /// the stack under it immutable to jj. Those changes are held, not
    /// landed: the plan and the run are the ones the same stack gets
    /// without that branch. (Seen on the sandbox before this: Land
    /// answered that the bookmark "points at immutable history —
    /// everything under it is already part of trunk".)
    #[test]
    fn a_stack_someone_elses_branch_holds_lands_like_any_other() {
        // Their branch sits on the stack's top: every change of the
        // stack is under it.
        let held = |mut snap: RepoSnapshot| {
            for node in snap.nodes.iter_mut().filter(|n| ["a1", "a2", "b1"].contains(&n.id.as_str())) {
                node.kind = NodeKind::Immutable;
                node.held = true;
            }
            snap
        };
        let forge = || {
            let mut forge = StubLandForge::default();
            forge.states.insert(1, ready_state());
            forge.states.insert(7, PrLandState { base_branch: "auth".into(), ..ready_state() });
            forge
        };
        let prs = pr_state(
            vec![open_pr(1, "auth", "main"), open_pr(7, "profile", "auth")],
            false,
        );
        let plain = ready_plan(&forge());
        let plan = plan_land(
            &held(land_snapshot()),
            &prs,
            &forge_repo(),
            "profile",
            &forge(),
            &at_plan(),
            &opts(),
        )
        .unwrap();
        assert!(plan.blockers.is_empty(), "{:?}", plan.blockers);
        assert_eq!(
            plan.segments.iter().map(|s| s.bookmark.as_str()).collect::<Vec<_>>(),
            ["auth", "profile"]
        );
        assert_eq!(plan.actions, plain.actions);

        // And the run: the same steps reach the repository, the sweep of
        // the landed changes among them.
        let run = |after: RepoSnapshot| {
            let vcs = StubVcs::new(after);
            let outcome = execute_land(&plan, &vcs, &forge()).unwrap();
            assert!(!outcome.failed, "{:?}", outcome.steps);
            let calls = vcs.calls.borrow().clone();
            calls
        };
        let here = run(held(post_fetch_snapshot()));
        assert_eq!(here, run(post_fetch_snapshot()));
        assert!(here.iter().any(|c| c.starts_with("abandon")), "{here:?}");
        assert!(here.iter().any(|c| c.starts_with("rebase")), "{here:?}");
    }

    /// GitHub closes every open pull request based on a branch when the
    /// branch is deleted (seen on the sandbox, merged or not). So the
    /// cleanup asks GitHub by base at the moment it would delete, and
    /// leaves the branch, here and on the remote, while one is based on
    /// it: one opened since the plan was made, or one whose retarget did
    /// not go through.
    #[test]
    fn a_landed_branch_is_not_deleted_under_a_pull_request_still_based_on_it() {
        // Opened after the plan was made: the plan knows nothing of #8.
        let mut forge = StubLandForge::default();
        forge.states.insert(1, ready_state());
        forge.states.insert(7, PrLandState { base_branch: "auth".into(), ..ready_state() });
        let plan = ready_plan(&forge);
        *forge.open_by_base.borrow_mut() =
            vec![their_pr(8, "their-mid", "auth", "sam", "c0ffee08")];
        let vcs = StubVcs::new(post_fetch_snapshot());
        let outcome = execute_land(&plan, &vcs, &forge).unwrap();
        assert!(!outcome.failed, "{:?}", outcome.steps);
        let detail = |wanted: fn(&LandAction) -> bool| {
            outcome
                .steps
                .iter()
                .find(|step| wanted(&step.action))
                .and_then(|step| step.detail.clone())
                .unwrap()
        };
        assert_eq!(
            detail(|a| matches!(a, LandAction::CleanupBookmark { .. })),
            "\u{201c}auth\u{201d} was left in place, here and on origin: #8 by @sam is still \
             based on it, and GitHub closes a pull request when its base branch is deleted. \
             Point it at another branch, and land again to clean up"
        );
        // Its changes stay with it: abandoning them would delete the
        // bookmark here, and the next push of deletions the branch.
        assert_eq!(
            detail(|a| matches!(a, LandAction::AbandonLanded { .. })),
            "\u{201c}auth\u{201d} was left in place, and its changes with it"
        );
        let here = vcs.calls.borrow().clone();
        assert!(!here.iter().any(|c| c.starts_with("delete") || c.starts_with("abandon")), "{here:?}");

        // The user's own next pull request, in a GitHub stack, which
        // GitHub left on the landed branch: after a push put the commits
        // on trunk it does not move a stack's bases, and lets no one
        // else. (On the sandbox the build before this deleted the branch
        // and GitHub closed the pull request.) The branch stays, and the
        // step says the way out.
        *forge.open_by_base.borrow_mut() = vec![PrSummary {
            author: Some("me".into()),
            ..open_pr(7, "profile", "auth")
        }];
        forge.states.get_mut(&7).unwrap().stack = Some(crate::pr::StackSeat {
            number: 90,
            position: 2,
            size: 2,
            base: "main".into(),
        });
        let vcs = StubVcs::new(post_fetch_snapshot());
        let outcome = execute_land(&plan, &vcs, &forge).unwrap();
        let cleanup = outcome
            .steps
            .iter()
            .find(|step| matches!(step.action, LandAction::CleanupBookmark { .. }))
            .and_then(|step| step.detail.clone())
            .unwrap();
        assert_eq!(
            cleanup,
            "\u{201c}auth\u{201d} was left in place, here and on origin: #7 is still based on it, \
             and GitHub closes a pull request when its base branch is deleted. In a GitHub stack \
             only GitHub changes a base: land again in a moment, and if it is still based on \
             \u{201c}auth\u{201d} then, dissolve the stack (publish --no-stack) so that it can be \
             pointed elsewhere"
        );
        let here = vcs.calls.borrow().clone();
        assert!(!here.iter().any(|c| c.starts_with("delete") || c.starts_with("abandon")), "{here:?}");

        // Known to the plan, and GitHub refuses the retarget. The run
        // goes on, the branch stays, and the step says why.
        let prs = with_others();
        let mut forge = StubLandForge::default();
        their_states(&mut forge);
        forge.retarget_refused = vec![8];
        let plan =
            plan_land(&land_snapshot(), &prs, &forge_repo(), "profile", &forge, &at_plan(), &opts())
                .unwrap();
        let vcs = StubVcs::new(post_fetch_snapshot());
        let outcome = execute_land(&plan, &vcs, &forge).unwrap();
        assert!(!outcome.failed, "{:?}", outcome.steps);
        let retarget = outcome
            .steps
            .iter()
            .find(|step| matches!(step.action, LandAction::RetargetOther { .. }))
            .and_then(|step| step.detail.clone())
            .unwrap();
        assert!(
            retarget.starts_with("#8 by @sam could not be pointed at main")
                && retarget.ends_with("\u{201c}auth\u{201d} stays in place for it"),
            "{retarget}"
        );
        let here = vcs.calls.borrow().clone();
        assert!(here.contains(&"push:profile:origin".to_owned()), "{here:?}");
        assert!(!here.iter().any(|c| c.starts_with("delete") || c.starts_with("abandon")), "{here:?}");

        // Asking GitHub failed: nothing is deleted on a guess.
        struct Unreachable(StubLandForge);
        impl LandForge for Unreachable {
            fn find_merged_pr(&self, branch: &str) -> Result<Option<PrSummary>, ForgeError> {
                self.0.find_merged_pr(branch)
            }
            fn pr_land_state(&self, number: u64, base: &str) -> Result<PrLandState, ForgeError> {
                self.0.pr_land_state(number, base)
            }
            fn merge_pr(
                &self,
                number: u64,
                method: MergeMethod,
                expected_head: &str,
                commit_title: Option<&str>,
                commit_message: Option<&str>,
            ) -> Result<(), ForgeError> {
                self.0.merge_pr(number, method, expected_head, commit_title, commit_message)
            }
            fn enable_auto_merge(
                &self,
                node_id: &str,
                method: MergeMethod,
                expected_head: &str,
                commit_title: Option<&str>,
                commit_message: Option<&str>,
            ) -> Result<(), ForgeError> {
                self.0.enable_auto_merge(node_id, method, expected_head, commit_title, commit_message)
            }
            fn enqueue_pr(&self, node_id: &str, expected_head: &str) -> Result<(), ForgeError> {
                self.0.enqueue_pr(node_id, expected_head)
            }
            fn update_pr_base(&self, number: u64, base: &str) -> Result<(), ForgeError> {
                self.0.update_pr_base(number, base)
            }
            fn open_prs_based_on(&self, _base: &str) -> Result<Vec<PrSummary>, ForgeError> {
                Err(ForgeError::Network("GitHub is unreachable".into()))
            }
            fn mark_ready(&self, node_id: &str) -> Result<(), ForgeError> {
                self.0.mark_ready(node_id)
            }
            fn merge_stacked(
                &self,
                number: u64,
                method: MergeMethod,
                expected_head: &str,
                commit_title: Option<&str>,
                commit_message: Option<&str>,
                bypass: bool,
            ) -> Result<AsyncMerge, ForgeError> {
                self.0.merge_stacked(number, method, expected_head, commit_title, commit_message, bypass)
            }
            fn enqueue_stacked(
                &self,
                number: u64,
                expected_head: &str,
            ) -> Result<AsyncMerge, ForgeError> {
                self.0.enqueue_stacked(number, expected_head)
            }
            fn await_restack(&self, number: u64, from_head: &str) -> Result<Restack, ForgeError> {
                self.0.await_restack(number, from_head)
            }
        }
        let mut inner = StubLandForge::default();
        inner.states.insert(1, ready_state());
        inner.states.insert(7, PrLandState { base_branch: "auth".into(), ..ready_state() });
        let plan = ready_plan(&inner);
        let vcs = StubVcs::new(post_fetch_snapshot());
        let outcome = execute_land(&plan, &vcs, &Unreachable(inner)).unwrap();
        assert!(outcome.failed);
        let failed = outcome.steps.iter().find(|s| s.status == SubmitStepStatus::Failed).unwrap();
        assert!(matches!(failed.action, LandAction::CleanupBookmark { .. }));
        assert!(
            failed.detail.as_deref().unwrap().contains("nothing was deleted, here or on origin"),
            "{:?}",
            failed.detail
        );
        assert!(!vcs.calls.borrow().iter().any(|c| c.starts_with("delete")));
    }

    /// The retarget of their pull request looks at it first, and leaves
    /// what its author or GitHub did in the meantime: a pull request that
    /// was closed, one that was pointed somewhere else since the plan was
    /// made (found in review: it was pointed at trunk over the author's
    /// choice), one GitHub already moved, and one in a GitHub stack.
    #[test]
    fn a_pull_request_its_author_moved_or_closed_is_left_as_it_is() {
        let told_by = |method: Option<MergeMethod>,
                       edit: &dyn Fn(&mut PrLandState),
                       post: RepoSnapshot|
         -> (String, Vec<String>) {
            let mut forge = StubLandForge::default();
            their_states(&mut forge);
            edit(forge.states.get_mut(&8).unwrap());
            forge.calls.borrow_mut().clear();
            let vcs = StubVcs::new(post);
            let answer = retarget_other(
                8,
                "#8 by @sam",
                "their-mid",
                "auth",
                "main",
                "main",
                method,
                &forge,
                &vcs,
            )
            .unwrap();
            let asked: Vec<String> = forge
                .calls
                .borrow()
                .iter()
                .filter(|c| c.starts_with("retarget"))
                .cloned()
                .collect();
            (answer, asked)
        };
        // A run that only reconciles does not say how the branch landed.
        let told = |edit: &dyn Fn(&mut PrLandState), post: RepoSnapshot| told_by(None, edit, post);
        let untouched: Vec<String> = Vec::new();

        // Pointed at another branch by its author since the plan.
        let (answer, retargets) =
            told(&|land| land.base_branch = "release".into(), post_fetch_snapshot());
        assert_eq!(
            answer,
            "#8 by @sam targets \u{201c}release\u{201d} now, not \u{201c}auth\u{201d}: its base was \
             changed since this plan was made, and was left as it is"
        );
        assert_eq!(retargets, untouched);

        // Closed.
        let (answer, retargets) = told(&|land| land.state = PrState::Closed, post_fetch_snapshot());
        assert_eq!(answer, "#8 by @sam is no longer open; its base was left alone");
        assert_eq!(retargets, untouched);

        // On trunk already (GitHub moves it itself where the repository
        // deletes head branches on merge).
        let (answer, retargets) = told(&|land| land.base_branch = "main".into(), post_fetch_snapshot());
        assert!(answer.starts_with("GitHub already pointed #8 by @sam at main. It shows "), "{answer}");
        assert_eq!(retargets, untouched);

        // In a GitHub stack by now: only GitHub changes a base there.
        let (answer, retargets) = told(
            &|land| {
                land.stack = Some(crate::pr::StackSeat {
                    number: 90,
                    position: 3,
                    size: 3,
                    base: "main".into(),
                })
            },
            post_fetch_snapshot(),
        );
        assert_eq!(answer, "#8 by @sam is in a GitHub stack; GitHub moves its base itself");
        assert_eq!(retargets, untouched);

        // As the plan saw it: pointed at trunk, and told what it shows.
        // After a squash the landed commits are not trunk's own.
        let (answer, retargets) = told(&|_| {}, post_fetch_snapshot());
        assert_eq!(
            answer,
            "Pointed #8 by @sam at main. It shows the landed commits its branch still carries as \
             its own until its author rebases \u{201c}their-mid\u{201d}"
        );
        assert_eq!(retargets, ["retarget:8:main"]);
        // After a merge commit they are, and nothing changes for it: it
        // shows what it showed before. (Not "only its own changes": an
        // earlier round's rebase, or a published rewrite, may have
        // replaced commits under it, and those it goes on showing.)
        let as_it_was = "Pointed #8 by @sam at main. It shows what it showed before: \
                         \u{201c}auth\u{201d} landed with its own commits";
        let (answer, _) = told(&|_| {}, merge_landed_snapshot());
        assert_eq!(answer, as_it_was);
        // The plan's method decides where the plan says: the repository
        // is not asked (the fetch may have taken the bookmark with it,
        // where GitHub deletes head branches on merge).
        let (answer, _) = told_by(Some(MergeMethod::Merge), &|_| {}, post_fetch_snapshot());
        assert_eq!(answer, as_it_was);
        let (answer, _) = told_by(Some(MergeMethod::Squash), &|_| {}, merge_landed_snapshot());
        assert!(answer.ends_with("until its author rebases \u{201c}their-mid\u{201d}"), "{answer}");
    }

    /// GitHub lets no one else change a base inside one of its stacks.
    /// In the stack this landing is part of it moves what is left itself.
    /// A stack of someone else's that starts on a branch this run lands
    /// would be left on a branch that cannot be deleted, and the plan
    /// stops before anything merges. (Found in review: such a stack was
    /// called "the same GitHub stack" and skipped.) A pull request from
    /// a fork is pointed at trunk like any other.
    #[test]
    fn a_github_stack_of_someone_elses_on_a_branch_that_lands_stops_the_plan() {
        let snap = land_snapshot();
        let seat = |number: u64, position: u32, size: u32, base: &str| {
            Some(crate::pr::StackSeat { number, position, size, base: base.into() })
        };
        let mut forge = StubLandForge::default();
        their_states(&mut forge);
        let plan_with = |theirs: Vec<PrSummary>| {
            let mut all = vec![open_pr(1, "auth", "main"), open_pr(7, "profile", "auth")];
            all.extend(theirs);
            plan_land(
                &snap,
                &pr_state(all, false),
                &forge_repo(),
                "profile",
                &forge,
                &at_plan(),
                &opts(),
            )
            .unwrap()
        };
        let theirs = |plan: &LandPlan| -> Vec<u64> {
            plan.actions
                .iter()
                .filter_map(|action| match action {
                    LandAction::RetargetOther { number, .. } => Some(*number),
                    _ => None,
                })
                .collect()
        };

        // A stack of their own, rooted on the branch that lands.
        let plan = plan_with(vec![
            PrSummary { stack: seat(91, 1, 2, "auth"), ..their_pr(8, "their-mid", "auth", "sam", "c0ffee08") },
            PrSummary { stack: seat(91, 2, 2, "auth"), ..their_pr(10, "their-next", "their-mid", "kim", "c0ffee10") },
        ]);
        assert_eq!(plan.blockers.len(), 1, "{:?}", plan.blockers);
        assert_eq!(
            plan.blockers[0].message,
            "#8 by @sam and #10 by @kim are in GitHub stack 91, a stack of their own that starts \
             on \u{201c}auth\u{201d}. GitHub lets no one else change a base inside a stack, so \
             they cannot be pointed at \u{201c}main\u{201d}, and \u{201c}auth\u{201d} could not be \
             deleted after landing without GitHub closing a pull request \u{2014} ask their \
             authors to move that stack off \u{201c}auth\u{201d}, or to dissolve it, and land then"
        );
        assert!(!plan.blockers[0].wait);
        assert!(theirs(&plan).is_empty());

        // Their stack starts on a branch that stays: nothing stops.
        let plan = plan_with(vec![PrSummary {
            stack: seat(92, 1, 2, "profile"),
            ..their_pr(9, "their-top", "profile", "kim", "c0ffee09")
        }]);
        assert!(plan.blockers.is_empty(), "{:?}", plan.blockers);

        // In the stack the landing is part of (its base is trunk): GitHub
        // moves it, and the plan says so.
        let plan = plan_with(vec![PrSummary {
            stack: seat(90, 3, 3, "main"),
            ..their_pr(9, "their-top", "profile", "kim", "c0ffee09")
        }]);
        assert!(plan.blockers.is_empty(), "{:?}", plan.blockers);
        assert!(theirs(&plan).is_empty());
        assert!(
            plan.warnings.iter().any(|w| w.starts_with(
                "#9 by @kim is in the same GitHub stack and is someone else's: Land never merges it"
            )),
            "{:?}",
            plan.warnings
        );

        // From a fork: its base is this repository's branch all the same.
        let plan = plan_with(vec![PrSummary {
            head_owner: Some("a-fork".into()),
            ..their_pr(8, "auth", "auth", "sam", "c0ffee08")
        }]);
        assert!(plan.blockers.is_empty(), "{:?}", plan.blockers);
        assert_eq!(theirs(&plan), [8]);
    }

    /// Their pull request was added to our GitHub stack, on top, and the
    /// whole stack lands in one request. Its base branch lands and is
    /// deleted, and it still gets no step: in a stack only GitHub changes
    /// a base, and it moves what is left above a merge itself. The plan
    /// says so.
    #[test]
    fn someone_elses_pull_request_in_our_github_stack_gets_no_step() {
        let snap = stacked_snapshot();
        let forge = stacked_forge();
        let mut all = stacked_prs().report.prs;
        all.push(PrSummary {
            stack: seat(3, 3),
            ..their_pr(9, "their-top", "profile", "kim", "c0ffee09")
        });
        let plan =
            plan_land(&snap, &pr_state(all, false), &forge_repo(), "profile", &forge, &at_plan(), &opts())
                .unwrap();
        assert!(plan.blockers.is_empty(), "{:?}", plan.blockers);
        assert!(
            matches!(&plan.actions[0], LandAction::MergePr { number: 7, with, .. } if with.len() == 1),
            "{:?}",
            plan.actions
        );
        assert!(
            !plan.actions.iter().any(|a| matches!(a, LandAction::RetargetOther { .. })),
            "{:?}",
            plan.actions
        );
        assert!(
            plan.warnings.iter().any(|w| w.starts_with(
                "#9 by @kim is in the same GitHub stack and is someone else's: Land never merges it"
            )),
            "{:?}",
            plan.warnings
        );
    }

    /// The sweep finds someone else's branch still sitting on the landed
    /// commits (nothing here moves their branch): they stay, and that is
    /// told as what happened, not as a failure.
    #[test]
    fn landed_changes_someone_elses_branch_still_sits_on_are_left_without_failing() {
        let mut forge = StubLandForge::default();
        forge.states.insert(1, ready_state());
        forge.states.insert(7, PrLandState { base_branch: "auth".into(), ..ready_state() });
        let plan = ready_plan(&forge);
        let mut vcs = StubVcs::new(post_fetch_snapshot());
        vcs.abandon_leaves =
            Some("Left ca2, ca1 in place: their-mid@origin still builds on them".into());
        let outcome = execute_land(&plan, &vcs, &forge).unwrap();
        assert!(!outcome.failed, "{:?}", outcome.steps);
        let sweep = outcome
            .steps
            .iter()
            .find(|step| matches!(step.action, LandAction::AbandonLanded { .. }))
            .unwrap();
        assert_eq!(sweep.status, SubmitStepStatus::Done);
        assert_eq!(
            sweep.detail.as_deref(),
            Some("Left ca2, ca1 in place: their-mid@origin still builds on them")
        );

        // With the landed bookmark deleted, commits their branch holds
        // are no longer drawn as the stack's: there is nothing to sweep.
        // That is not "part of trunk's history", which is what the step
        // says of a merge-commit landing.
        let sweep_of = |resting: &[&str]| {
            let mut after = post_fetch_snapshot();
            after.nodes.retain(|n| n.id != "a1" && n.id != "a2");
            after.nodes[0].parents = vec!["s".into()];
            // (The fixture gives `b1` a commit id that starts like a2's,
            // for the hand-off that ends the run. Here it must not read
            // as one of the landed commits, and the hand-off, which then
            // finds another head than GitHub's, is not what is looked at.)
            after.nodes[0].commit_id = "cb1rebased".into();
            let mut vcs = StubVcs::new(after).knowing("a2", "ca2");
            vcs.resting = resting.iter().map(|name| (*name).to_owned()).collect();
            let outcome = execute_land(&plan, &vcs, &forge).unwrap();
            outcome
                .steps
                .iter()
                .find(|step| matches!(step.action, LandAction::AbandonLanded { .. }))
                .and_then(|step| step.detail.clone())
                .unwrap()
        };
        assert_eq!(
            sweep_of(&["their-mid@origin"]),
            "\u{201c}auth\u{201d}'s landed changes stay in place: their-mid@origin still builds \
             on them"
        );
        assert_eq!(
            sweep_of(&[]),
            "\u{201c}auth\u{201d}'s landed changes need no sweeping: they are part of trunk's \
             history, or gone from here already"
        );
    }

    #[test]
    fn merged_bottom_plans_reconcile_only() {
        let snap = land_snapshot();
        // auth has no open PR — it merged on GitHub.
        let prs = pr_state(vec![open_pr(7, "profile", "auth")], false);
        let mut forge = StubLandForge::default();
        forge.merged.insert("auth".into(), merged_pr(1, "auth"));

        let plan = plan_land(&snap, &prs, &forge_repo(), "profile", &forge, &at_plan(), &opts()).unwrap();
        assert!(plan.blockers.is_empty(), "{:?}", plan.blockers);
        assert_eq!(
            plan.actions,
            vec![
                LandAction::FetchRemote { remote: "origin".into() },
                LandAction::CheckLanded {
                    number: 1,
                    bookmark: "auth".into(),
                    bottom_commit: "ca1".into(),
                    top_commit: "ca2".into(),
                },
                LandAction::RetargetPr {
                    number: 7,
                    bookmark: "profile".into(),
                    to_base: "main".into(),
                },
                LandAction::RebaseOntoTrunk {
                    root_change: "b1".into(),
                    moves: 1,
                    off: vec!["ca2".into()],
                },
                LandAction::PushStack { bookmarks: vec!["profile".into()] },
                LandAction::CleanupBookmark {
                    bookmark: "auth".into(),
                    commit: "ca2".into(),
                },
                LandAction::AbandonLanded {
                    bookmark: "auth".into(),
                    change_ids: vec!["a2".into(), "a1".into()],
                    commits: vec!["ca2".into(), "ca1".into()],
                },
            ]
        );
        assert_eq!(
            plan.segments[0].status,
            LandSegmentStatus::Merged { number: 1, url: "https://github.com/o/r/pull/1".into() }
        );
        // The PR above stays put this run — merging it now would land the
        // stale pre-rebase diff. Its land state was asked once for the
        // auto-merge hand-off (the stub has none, so no chain planned).
        assert_eq!(plan.segments[1].status, LandSegmentStatus::Stacked);
        assert_eq!(
            forge.calls.borrow().as_slice(),
            ["find_merged:auth", "land_state:7"]
        );
    }

    #[test]
    fn reconcile_only_run_chains_auto_merge_on_the_next_pr() {
        let snap = land_snapshot();
        let prs = pr_state(vec![open_pr(7, "profile", "auth")], false);
        let mut forge = StubLandForge::default();
        forge.merged.insert("auth".into(), merged_pr(1, "auth"));
        forge.states.insert(7, PrLandState { base_branch: "auth".into(), ..ready_state() });

        let plan = plan_land(&snap, &prs, &forge_repo(), "profile", &forge, &at_plan(), &opts()).unwrap();
        assert!(plan.blockers.is_empty(), "{:?}", plan.blockers);
        assert_eq!(
            plan.actions.last(),
            Some(&LandAction::EnableAutoMerge {
                number: 7,
                bookmark: "profile".into(),
                method: MergeMethod::Squash,
                expected_head: None,
                commit_title: Some("PR 7 (#7)".into()),
                commit_message: Some("profile: avatars".into()),
            }),
            "{:?}",
            plan.actions
        );
        assert!(
            plan.warnings.iter().any(|w| w.contains("auto-merges once")),
            "{:?}",
            plan.warnings
        );

        // A repo without auto-merge skips the hand-off quietly.
        let mut forge = StubLandForge::default();
        forge.merged.insert("auth".into(), merged_pr(1, "auth"));
        forge.states.insert(
            7,
            PrLandState {
                base_branch: "auth".into(),
                auto_merge_allowed: false,
                ..ready_state()
            },
        );
        let plan = plan_land(&snap, &prs, &forge_repo(), "profile", &forge, &at_plan(), &opts()).unwrap();
        assert!(
            !plan
                .actions
                .iter()
                .any(|a| matches!(a, LandAction::EnableAutoMerge { .. })),
            "{:?}",
            plan.actions
        );
    }

    #[test]
    fn whole_stack_merged_plans_cleanup_only() {
        let snap = land_snapshot();
        let prs = pr_state(vec![], false);
        let mut forge = StubLandForge::default();
        forge.merged.insert("auth".into(), merged_pr(1, "auth"));
        forge.merged.insert("profile".into(), merged_pr(7, "profile"));

        let plan = plan_land(&snap, &prs, &forge_repo(), "profile", &forge, &at_plan(), &opts()).unwrap();
        assert!(plan.blockers.is_empty(), "{:?}", plan.blockers);
        assert_eq!(
            plan.actions,
            vec![
                LandAction::FetchRemote { remote: "origin".into() },
                LandAction::CheckLanded {
                    number: 1,
                    bookmark: "auth".into(),
                    bottom_commit: "ca1".into(),
                    top_commit: "ca2".into(),
                },
                LandAction::CheckLanded {
                    number: 7,
                    bookmark: "profile".into(),
                    bottom_commit: "cb1".into(),
                    top_commit: "cb1".into(),
                },
                LandAction::CleanupBookmark {
                    bookmark: "auth".into(),
                    commit: "ca2".into(),
                },
                LandAction::CleanupBookmark {
                    bookmark: "profile".into(),
                    commit: "cb1".into(),
                },
                LandAction::AbandonLanded {
                    bookmark: "profile".into(),
                    change_ids: vec!["b1".into()],
                    commits: vec!["cb1".into()],
                },
                LandAction::AbandonLanded {
                    bookmark: "auth".into(),
                    change_ids: vec!["a2".into(), "a1".into()],
                    commits: vec!["ca2".into(), "ca1".into()],
                },
            ]
        );
        // The working copy sits on the landed head; the plan says so.
        assert!(
            plan.warnings.iter().any(|w| w.contains("working copy")),
            "{:?}",
            plan.warnings
        );
    }

    #[test]
    fn missing_pr_above_merged_bottom_reconciles_with_a_warning() {
        let mut snap = land_snapshot();
        // profile never got a PR.
        snap.bookmarks[2].sync = SyncState::LocalOnly;
        snap.bookmarks[2].remote = None;
        let prs = pr_state(vec![], false);
        let mut forge = StubLandForge::default();
        forge.merged.insert("auth".into(), merged_pr(1, "auth"));

        let plan = plan_land(&snap, &prs, &forge_repo(), "profile", &forge, &at_plan(), &opts()).unwrap();
        assert!(plan.blockers.is_empty(), "{:?}", plan.blockers);
        // Reconcile still runs; no push (profile was never pushed), no
        // retarget (no PR above).
        assert_eq!(
            plan.actions,
            vec![
                LandAction::FetchRemote { remote: "origin".into() },
                LandAction::CheckLanded {
                    number: 1,
                    bookmark: "auth".into(),
                    bottom_commit: "ca1".into(),
                    top_commit: "ca2".into(),
                },
                LandAction::RebaseOntoTrunk {
                    root_change: "b1".into(),
                    moves: 1,
                    off: vec!["ca2".into()],
                },
                LandAction::CleanupBookmark {
                    bookmark: "auth".into(),
                    commit: "ca2".into(),
                },
                LandAction::AbandonLanded {
                    bookmark: "auth".into(),
                    change_ids: vec!["a2".into(), "a1".into()],
                    commits: vec!["ca2".into(), "ca1".into()],
                },
            ]
        );
        assert!(
            plan.warnings.iter().any(|w| w.contains("no pull request yet")),
            "{:?}",
            plan.warnings
        );
        assert_eq!(plan.segments[1].status, LandSegmentStatus::Stacked);
    }

    #[test]
    fn no_pr_at_the_bottom_blocks() {
        let snap = land_snapshot();
        let prs = pr_state(vec![], false);
        let forge = StubLandForge::default();

        let plan = plan_land(&snap, &prs, &forge_repo(), "profile", &forge, &at_plan(), &opts()).unwrap();
        assert!(plan.actions.is_empty());
        assert!(
            plan.blockers
                .iter()
                .any(|b| b.message.contains("has no pull request") && !b.wait),
            "{:?}",
            plan.blockers
        );
        assert_eq!(plan.segments[0].status, LandSegmentStatus::Waiting);
    }

    #[test]
    fn pending_checks_prefer_auto_merge_when_the_repo_allows() {
        let snap = land_snapshot();
        let prs = pr_state(
            vec![open_pr(1, "auth", "main"), open_pr(7, "profile", "auth")],
            false,
        );
        let mut forge = StubLandForge::default();
        forge.states.insert(
            1,
            PrLandState {
                checks: ChecksRollup::Pending,
                ..ready_state()
            },
        );

        let plan = plan_land(&snap, &prs, &forge_repo(), "profile", &forge, &at_plan(), &opts()).unwrap();
        assert!(plan.blockers.is_empty(), "{:?}", plan.blockers);
        assert_eq!(
            plan.actions,
            vec![LandAction::EnableAutoMerge {
                number: 1,
                bookmark: "auth".into(),
                method: MergeMethod::Squash,
                expected_head: Some("ca2feedface".into()),
                commit_title: Some("PR 1 (#1)".into()),
                commit_message: Some("auth: login flow\n\nauth: sessions".into()),
            }]
        );
        assert!(
            plan.warnings.iter().any(|w| w.contains("auto-merge")),
            "{:?}",
            plan.warnings
        );

        // With auto-merge disabled on the repo, the same state blocks.
        let mut forge = StubLandForge::default();
        forge.states.insert(
            1,
            PrLandState {
                checks: ChecksRollup::Pending,
                auto_merge_allowed: false,
                ..ready_state()
            },
        );
        let plan = plan_land(&snap, &prs, &forge_repo(), "profile", &forge, &at_plan(), &opts()).unwrap();
        assert!(plan.actions.is_empty());
        // A pending-checks wait is transient — an auto-land job may watch
        // through it even though the manual flow is blocked.
        assert!(
            plan.blockers
                .iter()
                .any(|b| b.message.contains("checks are still running") && b.wait),
            "{:?}",
            plan.blockers
        );
    }

    #[test]
    fn queue_protected_trunk_enqueues() {
        let snap = land_snapshot();
        let prs = pr_state(
            vec![open_pr(1, "auth", "main"), open_pr(7, "profile", "auth")],
            false,
        );
        let mut forge = StubLandForge::default();
        forge.states.insert(
            1,
            PrLandState {
                queue_on_base: true,
                queue_method: Some(MergeMethod::Squash),
                ..ready_state()
            },
        );

        let plan = plan_land(&snap, &prs, &forge_repo(), "profile", &forge, &at_plan(), &opts()).unwrap();
        assert_eq!(
            plan.actions,
            vec![LandAction::EnqueuePr {
                number: 1,
                bookmark: "auth".into(),
                method: MergeMethod::Squash,
                expected_head: "ca2feedface".into(),
            }]
        );
        assert!(
            plan.warnings.iter().any(|w| w.contains("merge queue")),
            "{:?}",
            plan.warnings
        );

        let outcome = execute_land(&plan, &StubVcs::new(land_snapshot()), &forge).unwrap();
        assert!(!outcome.failed, "{:?}", outcome.steps);
        assert_eq!(
            forge.enqueue_calls.borrow().as_slice(),
            [("NODE1".into(), "ca2feedface".into())]
        );

        // enqueuePullRequest cannot override the queue's method. An
        // explicit mismatch must be visible as a blocker, never silently
        // accepted as the queue's configured method.
        let requested = LandOptions {
            method: Some(MergeMethod::Rebase),
            bypass: false,
        };
        let mismatch = plan_land(
            &snap,
            &prs,
            &forge_repo(),
            "profile",
            &forge,
            &at_plan(),
            &requested,
        )
        .unwrap();
        assert!(mismatch.actions.is_empty(), "{:?}", mismatch.actions);
        assert!(
            mismatch
                .blockers
                .iter()
                .any(|b| b.message.contains("merge queue uses squash")
                    && b.message.contains("requested rebase")),
            "{:?}",
            mismatch.blockers
        );
    }

    #[test]
    fn blocked_states_plan_nothing_and_say_why() {
        let snap = land_snapshot();
        let prs = pr_state(
            vec![open_pr(1, "auth", "main"), open_pr(7, "profile", "auth")],
            false,
        );
        // The `wait` flag separates conditions GitHub may clear by itself
        // from states that require a local rebase or another user action.
        for (state, needle, wait) in [
            (
                PrLandState { mergeable: Mergeable::Conflicting, ..ready_state() },
                "merge conflicts",
                false,
            ),
            (
                PrLandState { mergeable: Mergeable::Unknown, ..ready_state() },
                "still working out",
                true,
            ),
            (
                PrLandState { review: ReviewDecision::ChangesRequested, ..ready_state() },
                "changes were requested",
                false,
            ),
            (
                PrLandState { checks: ChecksRollup::Failing, ..ready_state() },
                "checks are failing",
                false,
            ),
            (
                PrLandState { base_branch: "old-base".into(), ..ready_state() },
                "still targets",
                false,
            ),
            (
                PrLandState {
                    merge_state: MergeStateStatus::Behind,
                    ..ready_state()
                },
                "is behind",
                false,
            ),
            (
                PrLandState {
                    merge_state: MergeStateStatus::Blocked,
                    auto_merge_allowed: false,
                    ..ready_state()
                },
                "blocked by a repository requirement",
                true,
            ),
        ] {
            let mut forge = StubLandForge::default();
            forge.states.insert(1, state);
            let plan = plan_land(&snap, &prs, &forge_repo(), "profile", &forge, &at_plan(), &opts()).unwrap();
            assert!(plan.actions.is_empty(), "{needle}: {:?}", plan.actions);
            assert!(
                plan.blockers
                    .iter()
                    .any(|b| b.message.contains(needle) && b.wait == wait),
                "{needle}: {:?}",
                plan.blockers
            );
            assert_eq!(plan.segments[0].status, LandSegmentStatus::Waiting);
        }
    }

    #[test]
    fn draft_candidate_is_promoted_then_merged() {
        let snap = land_snapshot();
        let prs = pr_state(
            vec![open_pr(1, "auth", "main"), open_pr(7, "profile", "auth")],
            false,
        );
        let mut forge = StubLandForge::default();
        forge.states.insert(1, PrLandState { is_draft: true, ..ready_state() });

        let plan = plan_land(&snap, &prs, &forge_repo(), "profile", &forge, &at_plan(), &opts()).unwrap();
        assert!(plan.blockers.is_empty(), "{:?}", plan.blockers);
        assert_eq!(
            plan.actions[0],
            LandAction::MarkReadyPr {
                number: 1,
                bookmark: "auth".into(),
            }
        );
        assert!(
            matches!(plan.actions[1], LandAction::MergePr { number: 1, .. }),
            "{:?}",
            plan.actions
        );

        // A draft waiting on checks promotes, then hands to auto-merge.
        let mut forge = StubLandForge::default();
        forge.states.insert(
            1,
            PrLandState {
                is_draft: true,
                checks: ChecksRollup::Pending,
                ..ready_state()
            },
        );
        let plan = plan_land(&snap, &prs, &forge_repo(), "profile", &forge, &at_plan(), &opts()).unwrap();
        assert_eq!(
            plan.actions,
            vec![
                LandAction::MarkReadyPr {
                    number: 1,
                    bookmark: "auth".into(),
                },
                LandAction::EnableAutoMerge {
                    number: 1,
                    bookmark: "auth".into(),
                    method: MergeMethod::Squash,
                    expected_head: Some("ca2feedface".into()),
                    commit_title: Some("PR 1 (#1)".into()),
                    commit_message: Some("auth: login flow\n\nauth: sessions".into()),
                },
            ]
        );
    }

    #[test]
    fn requested_method_is_honored_or_blocked() {
        let snap = land_snapshot();
        let prs = pr_state(
            vec![open_pr(1, "auth", "main"), open_pr(7, "profile", "auth")],
            false,
        );
        let mut forge = StubLandForge::default();
        forge.states.insert(1, ready_state());

        let rebase = LandOptions {
            method: Some(MergeMethod::Rebase),
            bypass: false,
        };
        let plan = plan_land(&snap, &prs, &forge_repo(), "profile", &forge, &at_plan(), &rebase).unwrap();
        assert!(plan.blockers.is_empty(), "{:?}", plan.blockers);
        assert!(
            matches!(
                plan.actions[0],
                LandAction::MergePr { method: MergeMethod::Rebase, .. }
            ),
            "{:?}",
            plan.actions
        );
        assert_eq!(plan.options, rebase);

        // A method the repo forbids blocks with its name, not a fallback.
        let mut forge = StubLandForge::default();
        forge
            .states
            .insert(1, PrLandState { allows_rebase: false, ..ready_state() });
        let plan = plan_land(&snap, &prs, &forge_repo(), "profile", &forge, &at_plan(), &rebase).unwrap();
        assert!(plan.actions.is_empty());
        assert!(
            plan.blockers
                .iter()
                .any(|b| b.message.contains("does not allow rebase merges") && !b.wait),
            "{:?}",
            plan.blockers
        );
    }

    #[test]
    fn bypass_merges_through_waits_and_soft_blockers() {
        let snap = land_snapshot();
        let prs = pr_state(
            vec![open_pr(1, "auth", "main"), open_pr(7, "profile", "auth")],
            false,
        );
        let mut forge = StubLandForge::default();
        forge.states.insert(
            1,
            PrLandState {
                checks: ChecksRollup::Failing,
                review: ReviewDecision::ChangesRequested,
                ..ready_state()
            },
        );

        let bypass = LandOptions {
            method: None,
            bypass: true,
        };
        let plan = plan_land(&snap, &prs, &forge_repo(), "profile", &forge, &at_plan(), &bypass).unwrap();
        assert!(plan.blockers.is_empty(), "{:?}", plan.blockers);
        assert!(
            matches!(plan.actions[0], LandAction::MergePr { number: 1, .. }),
            "{:?}",
            plan.actions
        );
        assert!(
            plan.warnings
                .iter()
                .any(|w| w.contains("Bypassing") && w.contains("requested changes")),
            "{:?}",
            plan.warnings
        );
        // No auto-merge hand-off in a bypass run — the next run bypasses
        // right through as well.
        assert!(
            !plan
                .actions
                .iter()
                .any(|a| matches!(a, LandAction::EnableAutoMerge { .. })),
            "{:?}",
            plan.actions
        );

        // Bypass also merges straight past a queue-protected trunk.
        let mut forge = StubLandForge::default();
        forge
            .states
            .insert(1, PrLandState { queue_on_base: true, ..ready_state() });
        let plan = plan_land(&snap, &prs, &forge_repo(), "profile", &forge, &at_plan(), &bypass).unwrap();
        assert!(
            matches!(plan.actions[0], LandAction::MergePr { .. }),
            "{:?}",
            plan.actions
        );

        // The same explicit bypass still means a direct attempt when a
        // previous run already armed GitHub automation.
        for state in [
            PrLandState {
                auto_merge_enabled: true,
                ..ready_state()
            },
            PrLandState {
                in_merge_queue: true,
                queue_on_base: true,
                queue_method: Some(MergeMethod::Squash),
                ..ready_state()
            },
        ] {
            let mut forge = StubLandForge::default();
            forge.states.insert(1, state);
            let plan =
                plan_land(&snap, &prs, &forge_repo(), "profile", &forge, &at_plan(), &bypass).unwrap();
            assert!(
                matches!(plan.actions[0], LandAction::MergePr { .. }),
                "{:?}",
                plan.actions
            );
        }

        // Hard impossibilities still block: conflicts are not a protection.
        let mut forge = StubLandForge::default();
        forge.states.insert(
            1,
            PrLandState { mergeable: Mergeable::Conflicting, ..ready_state() },
        );
        let plan = plan_land(&snap, &prs, &forge_repo(), "profile", &forge, &at_plan(), &bypass).unwrap();
        assert!(plan.actions.is_empty());
        assert!(
            plan.blockers.iter().any(|b| b.message.contains("conflicts")),
            "{:?}",
            plan.blockers
        );
    }

    #[test]
    fn unstable_merge_state_treats_failing_checks_as_optional() {
        let snap = land_snapshot();
        let prs = pr_state(
            vec![open_pr(1, "auth", "main"), open_pr(7, "profile", "auth")],
            false,
        );
        let mut forge = StubLandForge::default();
        forge.states.insert(
            1,
            PrLandState {
                checks: ChecksRollup::Failing,
                merge_state: MergeStateStatus::Unstable,
                ..ready_state()
            },
        );

        let plan = plan_land(&snap, &prs, &forge_repo(), "profile", &forge, &at_plan(), &opts()).unwrap();
        assert!(plan.blockers.is_empty(), "{:?}", plan.blockers);
        assert!(
            matches!(plan.actions[0], LandAction::MergePr { number: 1, .. }),
            "{:?}",
            plan.actions
        );
        assert!(
            plan.warnings
                .iter()
                .any(|w| w.contains("does not require")),
            "{:?}",
            plan.warnings
        );

        // A clean merge state also skips the pending-checks hand-off:
        // nothing required is outstanding, so the merge runs now.
        let mut forge = StubLandForge::default();
        forge.states.insert(
            1,
            PrLandState {
                checks: ChecksRollup::Pending,
                merge_state: MergeStateStatus::Clean,
                ..ready_state()
            },
        );
        let plan = plan_land(&snap, &prs, &forge_repo(), "profile", &forge, &at_plan(), &opts()).unwrap();
        assert!(
            matches!(plan.actions[0], LandAction::MergePr { .. }),
            "{:?}",
            plan.actions
        );
    }

    #[test]
    fn loose_work_off_a_mid_segment_landed_change_rebases_too() {
        // main(m) ── a1 ── a2 (auth) ── b1 (profile, @), plus loose work x
        // hanging off a1 — landing auth must move x onto the new trunk,
        // not leave the abandon to reparent it onto stale history.
        let snap = snapshot(
            vec![
                node("b1", "profile: avatars", NodeKind::WorkingCopy, &["a2"], &["profile"]),
                node("x", "spike: retries", NodeKind::Mutable, &["a1"], &[]),
                node("a2", "auth: sessions", NodeKind::Mutable, &["a1"], &["auth"]),
                node("a1", "auth: login flow", NodeKind::Mutable, &["m"], &[]),
                node("m", "release", NodeKind::Immutable, &[], &["main"]),
            ],
            vec![
                bookmark("main", "m", SyncState::Synced, true),
                bookmark("auth", "a2", SyncState::Synced, false),
                bookmark("profile", "b1", SyncState::Ahead, false),
            ],
        );
        let prs = pr_state(
            vec![open_pr(1, "auth", "main"), open_pr(7, "profile", "auth")],
            false,
        );
        let mut forge = StubLandForge::default();
        forge.states.insert(1, ready_state());

        let plan = plan_land(&snap, &prs, &forge_repo(), "profile", &forge, &at_plan(), &opts()).unwrap();
        let rebased: Vec<&str> = plan
            .actions
            .iter()
            .filter_map(|a| match a {
                LandAction::RebaseOntoTrunk { root_change, .. } => Some(root_change.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(rebased, ["b1", "x"], "{:?}", plan.actions);
    }

    #[test]
    fn out_of_sync_bookmark_blocks_the_merge() {
        let mut snap = land_snapshot();
        snap.bookmarks[1].sync = SyncState::Ahead;
        let prs = pr_state(
            vec![open_pr(1, "auth", "main"), open_pr(7, "profile", "auth")],
            false,
        );
        let mut forge = StubLandForge::default();
        forge.states.insert(1, ready_state());

        let plan = plan_land(&snap, &prs, &forge_repo(), "profile", &forge, &at_plan(), &opts()).unwrap();
        assert!(plan.actions.is_empty());
        assert!(
            plan.blockers
                .iter()
                .any(|b| b.message.contains("GitHub branch differ") && !b.wait),
            "{:?}",
            plan.blockers
        );
    }

    #[test]
    fn automation_already_running_plans_nothing() {
        let snap = land_snapshot();
        let prs = pr_state(
            vec![open_pr(1, "auth", "main"), open_pr(7, "profile", "auth")],
            false,
        );
        let mut forge = StubLandForge::default();
        forge.states.insert(
            1,
            PrLandState {
                auto_merge_enabled: true,
                ..ready_state()
            },
        );

        let plan = plan_land(&snap, &prs, &forge_repo(), "profile", &forge, &at_plan(), &opts()).unwrap();
        assert!(plan.actions.is_empty());
        assert!(plan.blockers.is_empty(), "{:?}", plan.blockers);
        assert!(
            plan.warnings
                .iter()
                .any(|w| w.contains("auto-merge is already enabled")),
            "{:?}",
            plan.warnings
        );
        assert_eq!(plan.segments[0].status, LandSegmentStatus::Landing);
    }

    #[test]
    fn merge_commit_landing_skips_the_abandon() {
        let snap = land_snapshot();
        let prs = pr_state(
            vec![open_pr(1, "auth", "main"), open_pr(7, "profile", "auth")],
            false,
        );
        let mut forge = StubLandForge::default();
        forge.states.insert(
            1,
            PrLandState {
                allows_squash: false,
                allows_rebase: false,
                ..ready_state()
            },
        );

        let plan = plan_land(&snap, &prs, &forge_repo(), "profile", &forge, &at_plan(), &opts()).unwrap();
        assert!(matches!(
            plan.actions[0],
            LandAction::MergePr { method: MergeMethod::Merge, .. }
        ));
        assert!(
            !plan
                .actions
                .iter()
                .any(|a| matches!(a, LandAction::AbandonLanded { .. })),
            "a merge-commit landing keeps the local changes as trunk ancestry: {:?}",
            plan.actions
        );
        assert!(plan
            .actions
            .iter()
            .any(|a| matches!(a, LandAction::CleanupBookmark { .. })));
    }

    /// The world after a squash merge landed and the fetch imported it:
    /// trunk moved to the squash commit, the stack rebased locally is still
    /// pending, auth's branch survived (no delete-on-merge).
    fn post_fetch_snapshot() -> RepoSnapshot {
        let mut answer = snapshot(
            vec![
                node("b1", "profile: avatars", NodeKind::WorkingCopy, &["a2"], &["profile"]),
                node("a2", "auth: sessions", NodeKind::Mutable, &["a1"], &["auth"]),
                node("a1", "auth: login flow", NodeKind::Mutable, &["m"], &[]),
                node("s", "auth (squash)", NodeKind::Immutable, &["m"], &["main"]),
                node("m", "release", NodeKind::Immutable, &[], &[]),
            ],
            vec![
                bookmark("main", "s", SyncState::Synced, true),
                bookmark("auth", "a2", SyncState::Synced, false),
                bookmark("profile", "b1", SyncState::Ahead, false),
            ],
        );
        // Model the post-push Git head used by the chained auto-merge
        // lease. Real snapshots carry jj's unique short prefix.
        answer.nodes[0].commit_id = "ca2feedface".into();
        answer
    }

    fn ready_plan(forge: &StubLandForge) -> LandPlan {
        let snap = land_snapshot();
        let prs = pr_state(
            vec![open_pr(1, "auth", "main"), open_pr(7, "profile", "auth")],
            false,
        );
        plan_land(&snap, &prs, &forge_repo(), "profile", forge, &at_plan(), &opts()).unwrap()
    }

    /// Seen on the sandbox, with the repository set to delete head
    /// branches on merge: GitHub deletes the landed branch a moment after
    /// the merge, later than the run's fetch, and the push of the
    /// deletion is refused ("moved on the remote since the last fetch").
    /// Every landing failed at this step, and the landed change was left
    /// behind. A fetch settles it: with the branch gone, the step is
    /// done, and the sweep after it runs.
    #[test]
    fn a_branch_github_deleted_after_the_fetch_is_not_a_failed_cleanup() {
        let forge = || {
            let mut forge = StubLandForge::default();
            forge.states.insert(1, ready_state());
            forge.states.insert(7, PrLandState { base_branch: "auth".into(), ..ready_state() });
            forge
        };
        let plan = ready_plan(&forge());
        let cleanup = |outcome: &LandOutcome| {
            outcome
                .steps
                .iter()
                .find(|step| matches!(step.action, LandAction::CleanupBookmark { .. }))
                .map(|step| (step.status, step.detail.clone().unwrap_or_default()))
                .unwrap()
        };

        let mut vcs = StubVcs::new(post_fetch_snapshot());
        vcs.refuses_deletion = vec!["auth".into()];
        vcs.gone_at_fetch = vec!["auth".into()];
        let outcome = execute_land(&plan, &vcs, &forge()).unwrap();
        assert!(!outcome.failed, "{:?}", outcome.steps);
        assert_eq!(
            cleanup(&outcome),
            (
                SubmitStepStatus::Done,
                "Deleted \u{201c}auth\u{201d} here; GitHub had deleted its branch on origin already"
                    .to_owned()
            )
        );
        // The landed changes are swept after it, as in any other run.
        let calls = vcs.calls.borrow().clone();
        let at = |what: &str| calls.iter().position(|c| c == what);
        assert!(at("push:auth:origin") < at("abandon:ca2,ca1"), "{calls:?}");
        assert!(at("abandon:ca2,ca1").is_some(), "{calls:?}");

        // The push is refused and the branch is still there after the
        // fetch: something else moved it. That is a failure, as before.
        let mut vcs = StubVcs::new(post_fetch_snapshot());
        vcs.refuses_deletion = vec!["auth".into()];
        let outcome = execute_land(&plan, &vcs, &forge()).unwrap();
        assert!(outcome.failed);
        let (status, detail) = cleanup(&outcome);
        assert_eq!(status, SubmitStepStatus::Failed);
        assert!(detail.contains("auth moved on the remote since the last fetch"), "{detail}");
        assert!(!vcs.calls.borrow().iter().any(|c| c.starts_with("abandon")));
    }

    #[test]
    fn execute_runs_merge_and_reconcile_in_order() {
        let mut forge = StubLandForge::default();
        forge.states.insert(1, ready_state());
        forge.states.insert(7, PrLandState { base_branch: "auth".into(), ..ready_state() });
        let plan = ready_plan(&forge);
        forge.calls.borrow_mut().clear();
        let vcs = StubVcs::new(post_fetch_snapshot());

        let outcome = execute_land(&plan, &vcs, &forge).unwrap();
        assert!(!outcome.failed, "{:?}", outcome.steps);
        assert!(outcome
            .steps
            .iter()
            .all(|s| s.status == SubmitStepStatus::Done));
        assert_eq!(
            forge.calls.borrow().as_slice(),
            [
                "land_state:1",
                "merge:1:squash",
                // The landing check asks which commit #1 merged as.
                "land_state:1",
                "land_state:7",
                "retarget:7:main",
                "land_state:7",
                "auto_merge:NODE1:squash",
            ]
        );
        // The repository was asked about exactly the planned changes.
        assert_eq!(vcs.asked.borrow().as_slice(), ["landed1:ca1..ca2"]);
        assert_eq!(
            forge.automation_calls.borrow().as_slice(),
            [(
                "NODE1".into(),
                MergeMethod::Squash,
                "ca2feedface".into(),
                Some("PR 7 (#7)".into()),
                Some("profile: avatars".into()),
            )]
        );
        assert_eq!(
            vcs.calls.borrow().as_slice(),
            [
                "fetch:origin",
                "rebase:b1",
                "push:profile:origin",
                "delete:auth",
                "push:auth:origin",
                "abandon:ca2,ca1",
            ]
        );
    }

    /// Seen on GitHub: a flag trunk had turned on ended up off after a
    /// stack landed one pull request at a time, where its tip in one go
    /// would have left it on. Each merge passed its own check. The plan
    /// now asks the repository whether the two ways end the same before
    /// the first merge, and a difference stops it there.
    #[test]
    fn landing_that_would_not_end_as_the_tip_is_stopped_before_the_first_merge() {
        let snap = land_snapshot();
        let prs = pr_state(
            vec![open_pr(1, "auth", "main"), open_pr(7, "profile", "auth")],
            false,
        );
        let mut forge = StubLandForge::default();
        forge.states.insert(1, ready_state());
        let differing = || {
            let mut vcs = at_plan();
            vcs.tip = TipCheck::Differs { paths: vec!["flag.txt".into()], more: 0 };
            vcs
        };

        let vcs = differing();
        let plan = plan_land(&snap, &prs, &forge_repo(), "profile", &forge, &vcs, &opts()).unwrap();
        assert!(plan.actions.is_empty(), "{:?}", plan.actions);
        let [blocker] = plan.blockers.as_slice() else {
            panic!("{:?}", plan.blockers);
        };
        assert!(!blocker.wait && !blocker.fetch, "a person has to look");
        assert!(
            blocker.message.contains("would not end where landing its tip in one go would")
                && blocker.message.contains("flag.txt")
                && blocker.message.contains("Rebase the stack onto \u{201c}main\u{201d} and publish"),
            "{}",
            blocker.message
        );
        // Asked about the pull request that lands first and the top of
        // the stack, on the branch the plan names.
        assert_eq!(vcs.tip_asked.borrow().as_slice(), ["ca1..ca2..cb1"]);
        assert!(vcs.trunks.borrow().iter().all(|t| t == "main"));

        // The same before the first pull request is handed to GitHub's
        // auto-merge.
        let mut waiting = StubLandForge::default();
        waiting.states.insert(
            1,
            PrLandState {
                checks: ChecksRollup::Pending,
                merge_state: MergeStateStatus::Blocked,
                ..ready_state()
            },
        );
        let vcs = differing();
        let plan = plan_land(&snap, &prs, &forge_repo(), "profile", &waiting, &vcs, &opts()).unwrap();
        assert!(plan.actions.is_empty(), "{:?}", plan.actions);
        assert!(plan.blockers.iter().any(|b| b.message.contains("its tip in one go")));

        // One pull request is its own tip: there is nothing to ask.
        let vcs = differing();
        let plan = plan_land(&snap, &prs, &forge_repo(), "auth", &forge, &vcs, &opts()).unwrap();
        assert!(plan.blockers.is_empty(), "{:?}", plan.blockers);
        assert!(vcs.tip_asked.borrow().is_empty());

        // GitHub's rebase method replays every commit, whichever pull
        // request is merged: nothing to compare.
        let vcs = differing();
        let rebase = LandOptions { method: Some(MergeMethod::Rebase), ..opts() };
        let plan = plan_land(&snap, &prs, &forge_repo(), "profile", &forge, &vcs, &rebase).unwrap();
        assert!(plan.blockers.is_empty(), "{:?}", plan.blockers);
        assert!(vcs.tip_asked.borrow().is_empty());
        assert!(matches!(
            plan.actions[0],
            LandAction::MergePr { number: 1, method: MergeMethod::Rebase, .. }
        ));

        // A shape the repository does not judge lands as it always did.
        let mut vcs = at_plan();
        vcs.tip = TipCheck::Unknown;
        let plan = plan_land(&snap, &prs, &forge_repo(), "profile", &forge, &vcs, &opts()).unwrap();
        assert!(plan.blockers.is_empty(), "{:?}", plan.blockers);
        assert!(matches!(plan.actions[0], LandAction::MergePr { number: 1, .. }));
    }

    /// While #1 merges, someone replaces #7's branch on GitHub. The run's
    /// fetch follows it: the bookmark and GitHub agree again, about a
    /// commit this plan never saw. The change the plan listed is gone, so
    /// its rebase has nothing to do and says so. Auto-merge is not armed
    /// on what the fetch brought.
    #[test]
    fn auto_merge_is_not_armed_on_a_branch_the_fetch_replaced() {
        let mut forge = StubLandForge::default();
        forge.states.insert(1, ready_state());
        forge.states.insert(
            7,
            PrLandState {
                base_branch: "auth".into(),
                head_commit: "cx9feedface".into(),
                ..ready_state()
            },
        );
        let plan = ready_plan(&forge);
        assert!(
            matches!(plan.actions.last(), Some(LandAction::EnableAutoMerge { number: 7, .. })),
            "{:?}",
            plan.actions
        );
        forge.calls.borrow_mut().clear();
        // After the fetch: "profile" on the replacement, in sync, and b1
        // nowhere.
        let replaced = snapshot(
            vec![
                node("x9", "something else", NodeKind::Mutable, &["s"], &["profile"]),
                node("a2", "auth: sessions", NodeKind::WorkingCopy, &["a1"], &["auth"]),
                node("a1", "auth: login flow", NodeKind::Mutable, &["m"], &[]),
                node("s", "auth (squash)", NodeKind::Immutable, &["m"], &["main"]),
                node("m", "release", NodeKind::Immutable, &[], &[]),
            ],
            vec![
                bookmark("main", "s", SyncState::Synced, true),
                bookmark("auth", "a2", SyncState::Synced, false),
                bookmark("profile", "x9", SyncState::Synced, false),
            ],
        );
        let vcs = StubVcs::new(replaced);

        let outcome = execute_land(&plan, &vcs, &forge).unwrap();
        assert!(outcome.failed, "{:?}", outcome.steps);
        let failed = outcome
            .steps
            .iter()
            .find(|s| s.status == SubmitStepStatus::Failed)
            .expect("a step failed");
        assert!(matches!(failed.action, LandAction::EnableAutoMerge { number: 7, .. }));
        let story = failed.detail.as_deref().unwrap();
        assert!(
            story.contains("does not hold what this plan was made for any more")
                && story.contains("it has x9 now, and the plan listed b1")
                && story.contains("Auto-merge was not armed on #7"),
            "{story}"
        );
        assert!(forge.automation_calls.borrow().is_empty());
        // Everything before it ran: the landed pull request is cleaned up.
        assert!(vcs.calls.borrow().iter().any(|c| c == "abandon:ca2,ca1"));
    }

    /// Seen on GitHub: the lower pull request had merged, trunk had
    /// turned a flag on by itself, and of the two pull requests left one
    /// turned it on and the next off again. Resumed, Land rebased both
    /// onto trunk, which made the first a no-op and the second undo
    /// trunk's change; their tip in one go would have left it alone. The
    /// plan's comparison is for a first merge and had nothing to ask. So
    /// the rebase asks, and stops before it rewrites anything.
    #[test]
    fn the_rest_of_a_stack_is_not_rebased_when_that_would_not_end_as_its_tip() {
        // main(m) ── a1 (auth, #1 merged) ── b1 (profile, #7) ── c1 (cloud, #9, @).
        let three = |trunk: &str| {
            let mut nodes = vec![
                node("c1", "cloud: sync", NodeKind::WorkingCopy, &["b1"], &["cloud"]),
                node("b1", "profile: avatars", NodeKind::Mutable, &["a1"], &["profile"]),
                node("a1", "auth: login flow", NodeKind::Mutable, &["m"], &["auth"]),
            ];
            if trunk == "s" {
                nodes.push(node("s", "auth (squash)", NodeKind::Immutable, &["m"], &["main"]));
                nodes.push(node("m", "release", NodeKind::Immutable, &[], &[]));
            } else {
                nodes.push(node("m", "release", NodeKind::Immutable, &[], &["main"]));
            }
            snapshot(
                nodes,
                vec![
                    bookmark("main", trunk, SyncState::Synced, true),
                    bookmark("auth", "a1", SyncState::Synced, false),
                    bookmark("profile", "b1", SyncState::Synced, false),
                    bookmark("cloud", "c1", SyncState::Synced, false),
                ],
            )
        };
        let mut forge = StubLandForge::default();
        forge.merged.insert("auth".into(), merged_pr(1, "auth"));
        forge.states.insert(
            7,
            PrLandState {
                base_branch: "auth".into(),
                head_commit: "cb1feedface".into(),
                auto_merge_allowed: false,
                ..ready_state()
            },
        );
        let prs = pr_state(
            vec![
                PrSummary { head_commit: "cb1feedface".into(), ..open_pr(7, "profile", "auth") },
                PrSummary { head_commit: "cc1feedface".into(), ..open_pr(9, "cloud", "profile") },
            ],
            false,
        );
        let plan =
            plan_land(&three("m"), &prs, &forge_repo(), "cloud", &forge, &at_plan(), &opts())
                .unwrap();
        assert!(plan.blockers.is_empty(), "{:?}", plan.blockers);
        assert!(
            plan.actions.iter().any(
                |a| matches!(a, LandAction::RebaseOntoTrunk { root_change, .. } if root_change == "b1")
            ),
            "{:?}",
            plan.actions
        );

        let mut vcs = StubVcs::new(three("s"));
        vcs.tip = TipCheck::Differs { paths: vec!["flag.txt".into()], more: 0 };
        let outcome = execute_land(&plan, &vcs, &forge).unwrap();
        assert!(outcome.failed, "{:?}", outcome.steps);
        let failed = outcome
            .steps
            .iter()
            .find(|s| s.status == SubmitStepStatus::Failed)
            .expect("a step failed");
        assert!(matches!(failed.action, LandAction::RebaseOntoTrunk { .. }), "{:?}", failed.action);
        let story = failed.detail.as_deref().unwrap();
        assert!(
            story.contains("would not end on \u{201c}main\u{201d} as its tip would in one go")
                && story.contains("flag.txt")
                // What it says was left alone is what the stop is about: a
                // chain off to the side may have been rebased before it.
                && story.contains("b1 and what is above it were not rebased, and nothing was pushed")
                && !story.contains("Nothing was rebased")
                && story.contains("rebase b1 onto \u{201c}main\u{201d} yourself"),
            "{story}"
        );
        // Every change of what is left in turn, from its bottom to the
        // stack's head, on the plan's trunk.
        assert_eq!(vcs.tip_asked.borrow().as_slice(), ["cb1..cb1..cc1"]);
        assert!(vcs.trunks.borrow().iter().all(|t| t == "main"));
        let calls = vcs.calls.borrow();
        assert!(
            !calls.iter().any(|c| c.starts_with("rebase") || c.starts_with("push")
                || c.starts_with("delete") || c.starts_with("abandon")),
            "{calls:?}"
        );
        drop(calls);

        // Where the two agree, the reconcile goes through as before.
        let vcs = StubVcs::new(three("s"));
        let outcome = execute_land(&plan, &vcs, &forge).unwrap();
        assert!(!outcome.failed, "{:?}", outcome.steps);
        assert!(vcs.calls.borrow().iter().any(|c| c == "rebase:b1"));

        // The comparison is made for a stack someone else's branch holds
        // as well: its changes are immutable to jj and are still what the
        // pull requests land.
        let mut held = three("s");
        for node in held.nodes.iter_mut().filter(|n| n.id == "b1" || n.id == "c1") {
            node.kind = NodeKind::Immutable;
            node.held = true;
        }
        let mut vcs = StubVcs::new(held);
        vcs.tip = TipCheck::Differs { paths: vec!["flag.txt".into()], more: 0 };
        let outcome = execute_land(&plan, &vcs, &forge).unwrap();
        assert!(outcome.failed, "{:?}", outcome.steps);
        assert_eq!(vcs.tip_asked.borrow().as_slice(), ["cb1..cb1..cc1"]);

        // Landing with the rebase method, GitHub replays every commit of
        // the tip as well: nothing to compare, here as in the plan.
        let rebase = LandOptions { method: Some(MergeMethod::Rebase), ..opts() };
        let by_rebase =
            plan_land(&three("m"), &prs, &forge_repo(), "cloud", &forge, &at_plan(), &rebase)
                .unwrap();
        let mut vcs = StubVcs::new(three("s"));
        vcs.tip = TipCheck::Differs { paths: vec!["flag.txt".into()], more: 0 };
        let outcome = execute_land(&by_rebase, &vcs, &forge).unwrap();
        assert!(!outcome.failed, "{:?}", outcome.steps);
        assert!(vcs.tip_asked.borrow().is_empty());
        assert!(vcs.calls.borrow().iter().any(|c| c == "rebase:b1"));

        // One change left is its own tip: nothing is asked.
        let mut forge = StubLandForge::default();
        forge.merged.insert("auth".into(), merged_pr(1, "auth"));
        forge.states.insert(
            7,
            PrLandState { base_branch: "auth".into(), auto_merge_allowed: false, ..ready_state() },
        );
        let prs = pr_state(vec![open_pr(7, "profile", "auth")], false);
        let plan =
            plan_land(&land_snapshot(), &prs, &forge_repo(), "profile", &forge, &at_plan(), &opts())
                .unwrap();
        let mut vcs = StubVcs::new(post_fetch_snapshot());
        vcs.tip = TipCheck::Differs { paths: vec!["flag.txt".into()], more: 0 };
        let outcome = execute_land(&plan, &vcs, &forge).unwrap();
        assert!(!outcome.failed, "{:?}", outcome.steps);
        assert!(vcs.tip_asked.borrow().is_empty());
    }

    /// A rebase can leave a change empty: trunk already has what it did.
    /// Publish skips a segment like that, so telling the user to publish
    /// first would send the two commands back and forth (seen on the
    /// sandbox). Land says what is the case instead.
    #[test]
    fn a_segment_that_came_out_empty_is_not_sent_back_to_publish() {
        let mut snap = land_snapshot();
        // "auth" was rebased here: both changes empty, the bookmark ahead.
        snap.nodes[1].is_empty = true;
        snap.nodes[2].is_empty = true;
        snap.bookmarks[1].sync = SyncState::Ahead;
        let prs = pr_state(
            vec![open_pr(1, "auth", "main"), open_pr(7, "profile", "auth")],
            false,
        );
        let mut forge = StubLandForge::default();
        forge.states.insert(1, ready_state());
        let plan = plan_land(&snap, &prs, &forge_repo(), "profile", &forge, &at_plan(), &opts()).unwrap();
        assert!(plan.actions.is_empty(), "{:?}", plan.actions);
        assert!(
            plan.blockers.iter().any(|b| {
                b.message.contains("every change under \u{201c}auth\u{201d} is empty here")
                    && b.message.contains("Close #1 and abandon the change")
                    && !b.message.contains("publish the stack first")
            }),
            "{:?}",
            plan.blockers
        );
        // With content, the usual answer.
        snap.nodes[1].is_empty = false;
        let plan = plan_land(&snap, &prs, &forge_repo(), "profile", &forge, &at_plan(), &opts()).unwrap();
        assert!(
            plan.blockers.iter().any(|b| b.message.contains("publish the stack first")),
            "{:?}",
            plan.blockers
        );
    }

    #[test]
    fn execute_refuses_to_push_a_conflicted_rebase() {
        let mut forge = StubLandForge::default();
        forge.states.insert(1, ready_state());
        forge.states.insert(7, PrLandState { base_branch: "auth".into(), ..ready_state() });
        let plan = ready_plan(&forge);
        // The rebase left b1 conflicted against the squashed trunk.
        let mut post = post_fetch_snapshot();
        post.nodes[0].has_conflict = true;
        let vcs = StubVcs::new(post);

        let outcome = execute_land(&plan, &vcs, &forge).unwrap();
        assert!(outcome.failed, "{:?}", outcome.steps);
        let failed = outcome
            .steps
            .iter()
            .find(|s| s.status == SubmitStepStatus::Failed)
            .expect("the push step failed");
        assert!(matches!(failed.action, LandAction::PushStack { .. }));
        assert!(
            failed.detail.as_deref().unwrap().contains("conflicts"),
            "{:?}",
            failed.detail
        );
        // Nothing was pushed, deleted, or abandoned past the gate.
        assert_eq!(vcs.calls.borrow().as_slice(), ["fetch:origin", "rebase:b1"]);

        // The gate holds for a change someone else's branch holds too:
        // immutable to jj, and still what this push would publish.
        let mut forge = StubLandForge::default();
        forge.states.insert(1, ready_state());
        forge.states.insert(7, PrLandState { base_branch: "auth".into(), ..ready_state() });
        let mut post = post_fetch_snapshot();
        post.nodes[0].has_conflict = true;
        post.nodes[0].kind = NodeKind::Immutable;
        post.nodes[0].held = true;
        let vcs = StubVcs::new(post);
        let outcome = execute_land(&plan, &vcs, &forge).unwrap();
        assert!(outcome.failed, "{:?}", outcome.steps);
        assert!(!vcs.calls.borrow().iter().any(|c| c.starts_with("push")));
    }

    #[test]
    fn execute_marks_a_draft_ready_before_merging() {
        let mut forge = StubLandForge::default();
        forge.states.insert(1, PrLandState { is_draft: true, ..ready_state() });
        forge.states.insert(7, PrLandState { base_branch: "main".into(), ..ready_state() });
        let plan = {
            let snap = land_snapshot();
            let prs = pr_state(
                vec![open_pr(1, "auth", "main"), open_pr(7, "profile", "auth")],
                false,
            );
            plan_land(&snap, &prs, &forge_repo(), "profile", &forge, &at_plan(), &opts()).unwrap()
        };
        forge.calls.borrow_mut().clear();
        let vcs = StubVcs::new(post_fetch_snapshot());

        let outcome = execute_land(&plan, &vcs, &forge).unwrap();
        assert!(!outcome.failed, "{:?}", outcome.steps);
        let calls = forge.calls.borrow();
        assert_eq!(
            &calls[..4],
            ["land_state:1", "mark_ready:NODE1", "land_state:1", "merge:1:squash"]
        );
    }

    #[test]
    fn execute_adapts_when_github_already_cleaned_up() {
        let mut forge = StubLandForge::default();
        forge.merged.insert("auth".into(), merged_pr(1, "auth"));
        let plan = {
            // Plan from the merged-bottom flavor.
            let snap = land_snapshot();
            let prs = pr_state(vec![open_pr(7, "profile", "auth")], false);
            let mut planning = StubLandForge::default();
            planning.merged.insert("auth".into(), merged_pr(1, "auth"));
            plan_land(&snap, &prs, &forge_repo(), "profile", &planning, &at_plan(), &opts()).unwrap()
        };
        // After the fetch: delete-on-merge pruned auth and the abandon
        // machinery swept a1/a2; b1 already rebased onto the new trunk;
        // GitHub already retargeted #7.
        let post = snapshot(
            vec![
                node("b1", "profile: avatars", NodeKind::WorkingCopy, &["s"], &["profile"]),
                node("s", "auth (squash)", NodeKind::Immutable, &["m"], &["main"]),
                node("m", "release", NodeKind::Immutable, &[], &[]),
            ],
            vec![
                bookmark("main", "s", SyncState::Synced, true),
                bookmark("profile", "b1", SyncState::Ahead, false),
            ],
        );
        forge
            .states
            .insert(7, PrLandState { base_branch: "main".into(), ..ready_state() });
        let vcs = StubVcs::new(post);

        let outcome = execute_land(&plan, &vcs, &forge).unwrap();
        assert!(!outcome.failed, "{:?}", outcome.steps);
        assert!(outcome
            .steps
            .iter()
            .all(|s| s.status == SubmitStepStatus::Done));
        // Nothing to rewrite: only the fetch and the push of the (still
        // pending) rebased stack ran.
        assert_eq!(
            vcs.calls.borrow().as_slice(),
            ["fetch:origin", "push:profile:origin"]
        );
        // The polite details tell the story.
        let details: Vec<&str> = outcome
            .steps
            .iter()
            .filter_map(|s| s.detail.as_deref())
            .collect();
        assert!(details.iter().any(|d| d.contains("is already on main")), "{details:?}");
        assert!(details.iter().any(|d| d.contains("already retargeted")), "{details:?}");
        assert!(details.iter().any(|d| d.contains("already gone")), "{details:?}");
        // (The fetch that pruned the branch took its commits with it:
        // they are gone from here, not part of trunk's history.)
        assert!(
            details
                .iter()
                .any(|d| d.contains("need no sweeping") && d.contains("gone from here already")),
            "{details:?}"
        );
    }

    #[test]
    fn execute_stops_when_the_merge_refuses() {
        let mut forge = StubLandForge::default();
        forge.states.insert(1, ready_state());
        let plan = ready_plan(&forge);
        forge.calls.borrow_mut().clear();
        // The candidate's checks flipped to failing between confirm and run.
        forge.states.insert(
            1,
            PrLandState { checks: ChecksRollup::Failing, ..ready_state() },
        );
        let vcs = StubVcs::new(post_fetch_snapshot());

        let outcome = execute_land(&plan, &vcs, &forge).unwrap();
        assert!(outcome.failed);
        assert_eq!(outcome.steps[0].status, SubmitStepStatus::Failed);
        assert!(outcome.steps[0]
            .detail
            .as_deref()
            .unwrap()
            .contains("no longer ready"));
        assert!(outcome.steps[1..]
            .iter()
            .all(|s| s.status == SubmitStepStatus::Skipped));
        assert!(vcs.calls.borrow().is_empty(), "nothing local ran");
    }

    #[test]
    fn execute_believes_the_pr_over_a_merge_error() {
        let mut forge = StubLandForge::default();
        forge.states.insert(1, ready_state());
        forge.states.insert(7, PrLandState { base_branch: "main".into(), ..ready_state() });
        let plan = ready_plan(&forge);
        forge.calls.borrow_mut().clear();
        forge.merged_despite_error = true;
        let vcs = StubVcs::new(post_fetch_snapshot());

        let outcome = execute_land(&plan, &vcs, &forge).unwrap();
        assert!(!outcome.failed, "{:?}", outcome.steps);
        assert!(outcome.steps[0]
            .detail
            .as_deref()
            .unwrap()
            .contains("the merge went through"));
    }

    #[test]
    fn execute_refuses_plans_with_blockers() {
        let snap = land_snapshot();
        let prs = pr_state(vec![], false);
        let forge = StubLandForge::default();
        let plan = plan_land(&snap, &prs, &forge_repo(), "profile", &forge, &at_plan(), &opts()).unwrap();
        assert!(!plan.blockers.is_empty());
        let vcs = StubVcs::new(land_snapshot());
        let err = execute_land(&plan, &vcs, &forge).unwrap_err();
        assert!(err.to_string().contains("blockers"));
    }

    #[test]
    fn older_persisted_automation_actions_keep_deserializing() {
        let auto: LandAction = serde_json::from_value(json!({
            "kind": "enableAutoMerge",
            "number": 1,
            "bookmark": "auth",
            "method": "squash"
        }))
        .unwrap();
        assert!(matches!(
            auto,
            LandAction::EnableAutoMerge {
                expected_head: None,
                commit_title: None,
                commit_message: None,
                ..
            }
        ));

        let queue: LandAction = serde_json::from_value(json!({
            "kind": "enqueuePr",
            "number": 1,
            "bookmark": "auth"
        }))
        .unwrap();
        assert!(matches!(
            queue,
            LandAction::EnqueuePr {
                method: MergeMethod::Squash,
                ref expected_head,
                ..
            } if expected_head.is_empty()
        ));
    }

    #[test]
    fn parse_pr_land_state_reads_the_full_shape() {
        let data = json!({
            "repository": {
                "autoMergeAllowed": true,
                "squashMergeAllowed": true,
                "mergeCommitAllowed": false,
                "rebaseMergeAllowed": false,
                "mergeQueue": {
                    "id": "Q_1",
                    "configuration": { "mergeMethod": "REBASE" }
                },
                "pullRequest": {
                    "id": "PR_node1",
                    "state": "OPEN",
                    "isDraft": false,
                    "mergeable": "MERGEABLE",
                    "mergeStateStatus": "UNSTABLE",
                    "reviewDecision": "APPROVED",
                    "isInMergeQueue": false,
                    "autoMergeRequest": null,
                    "baseRefName": "main",
                    "headRefOid": "abc123",
                    "commits": { "nodes": [ { "commit": { "statusCheckRollup": { "state": "SUCCESS" } } } ] }
                }
            }
        });
        let state = parse_pr_land_state(&data, 5).unwrap();
        assert_eq!(state.node_id, "PR_node1");
        assert_eq!(state.state, PrState::Open);
        assert_eq!(state.mergeable, Mergeable::Mergeable);
        assert_eq!(state.merge_state, MergeStateStatus::Unstable);
        assert!(state.merge_state.merges_now());
        assert_eq!(state.review, ReviewDecision::Approved);
        assert_eq!(state.checks, ChecksRollup::Passing);
        assert_eq!(state.base_branch, "main");
        assert_eq!(state.head_commit, "abc123");
        assert!(!state.auto_merge_enabled);
        assert!(!state.in_merge_queue);
        assert!(state.auto_merge_allowed);
        assert!(state.queue_on_base);
        assert_eq!(state.queue_method, Some(MergeMethod::Rebase));
        assert!(state.allows_squash);
        assert!(!state.allows_merge);
        assert!(!state.allows_rebase);

        // No rollup, auto-merge on, no queue on the base branch.
        let data = json!({
            "repository": {
                "autoMergeAllowed": false,
                "squashMergeAllowed": true,
                "mergeCommitAllowed": true,
                "rebaseMergeAllowed": true,
                "mergeQueue": null,
                "pullRequest": {
                    "id": "PR_node2",
                    "state": "MERGED",
                    "isDraft": false,
                    "mergeable": "UNKNOWN",
                    "reviewDecision": null,
                    "isInMergeQueue": false,
                    "autoMergeRequest": { "enabledAt": "2026-07-01T00:00:00Z" },
                    "baseRefName": "main",
                    "headRefOid": "def456",
                    "commits": { "nodes": [ { "commit": { "statusCheckRollup": null } } ] }
                }
            }
        });
        let state = parse_pr_land_state(&data, 6).unwrap();
        assert_eq!(state.state, PrState::Merged);
        assert_eq!(state.mergeable, Mergeable::Unknown);
        // The field missing entirely reads as unknown, never a parse error.
        assert_eq!(state.merge_state, MergeStateStatus::Unknown);
        assert_eq!(state.review, ReviewDecision::None);
        assert_eq!(state.checks, ChecksRollup::None);
        assert!(state.auto_merge_enabled);
        assert!(!state.queue_on_base);
        assert_eq!(state.queue_method, None);

        // A missing PR is the not-found story, not a parse error.
        let data = json!({ "repository": { "pullRequest": null } });
        assert!(matches!(
            parse_pr_land_state(&data, 9).unwrap_err(),
            ForgeError::NotFound(_)
        ));
    }
    // ---- A merge is a landing only when the repository says so ----

    /// What GitHub reports after a reordered push closed #1: merged, into
    /// the branch that used to sit below it, "as" its own head commit.
    fn merged_by_reachability(number: u64, head: &str, into: &str) -> PrSummary {
        PrSummary {
            state: PrState::Merged,
            base_branch: into.into(),
            merge_commit: Some("ca2feedface".into()),
            ..open_pr(number, head, into)
        }
    }

    #[test]
    fn a_pull_request_merged_into_another_branch_has_not_landed() {
        // Reproduced on GitHub: a reorder pushed with the old bases in
        // place made GitHub close #1 as merged into "profile". Nothing
        // reached main. The old plan called the segment landed and
        // scheduled its bookmark and changes for deletion.
        let snap = land_snapshot();
        let prs = pr_state(vec![open_pr(7, "profile", "main")], false);
        let mut forge = StubLandForge::default();
        forge
            .merged
            .insert("auth".into(), merged_by_reachability(1, "auth", "profile"));
        // The repository knows that commit, and trunk does not have it.
        let vcs = at_plan().saying("ca2feedface", LandedCheck::Elsewhere);

        let plan = plan_land(&snap, &prs, &forge_repo(), "profile", &forge, &vcs, &opts()).unwrap();
        assert!(plan.actions.is_empty(), "{:?}", plan.actions);
        assert_eq!(plan.blockers.len(), 1, "{:?}", plan.blockers);
        let blocker = &plan.blockers[0];
        assert!(!blocker.wait);
        assert!(
            blocker.message.contains("#1 (auth) is closed as merged on GitHub")
                && blocker.message.contains("merged into \u{201c}profile\u{201d}, not \u{201c}main\u{201d}")
                && blocker.message.contains("publish the stack"),
            "{}",
            blocker.message
        );
        // Not shown as merged, and nothing above it lands first.
        assert_eq!(plan.segments[0].status, LandSegmentStatus::Waiting);
        assert_eq!(plan.segments[1].status, LandSegmentStatus::Stacked);
        // The question was about exactly this segment's commits.
        assert_eq!(vcs.asked.borrow().as_slice(), ["ca2feedface:ca1..ca2"]);
    }

    #[test]
    fn a_merge_that_does_not_hold_the_changes_has_not_landed() {
        // Reproduced on GitHub: a bookmark reusing the name of a branch
        // whose pull request merged long ago. The old merge is on trunk;
        // this work is not in it.
        let snap = land_snapshot();
        let prs = pr_state(vec![], false);
        let mut forge = StubLandForge::default();
        forge.merged.insert("auth".into(), merged_pr(1, "auth"));
        let vcs = at_plan().saying(
            "landed1",
            LandedCheck::Missing {
                paths: vec!["src/new.rs".into(), "src/other.rs".into()],
                more: 3,
            },
        );

        let plan = plan_land(&snap, &prs, &forge_repo(), "profile", &forge, &vcs, &opts()).unwrap();
        assert!(plan.actions.is_empty(), "{:?}", plan.actions);
        assert_eq!(plan.blockers.len(), 1, "{:?}", plan.blockers);
        assert!(
            plan.blockers[0].message.contains(
                "\u{201c}main\u{201d} does not hold everything under \u{201c}auth\u{201d} \
                 (src/new.rs, src/other.rs and 3 more)"
            ),
            "{}",
            plan.blockers[0].message
        );
        assert_eq!(plan.segments[0].status, LandSegmentStatus::Waiting);

        // The same with someone else's branch on "auth": its changes are
        // immutable to jj and are still not trunk's history, so the
        // answer is the same one, not the one for a merge that took a
        // branch in and kept nothing of it.
        let mut held = land_snapshot();
        for node in held.nodes.iter_mut().filter(|n| n.id == "a1" || n.id == "a2") {
            node.kind = NodeKind::Immutable;
            node.held = true;
        }
        let again =
            plan_land(&held, &prs, &forge_repo(), "profile", &forge, &vcs, &opts()).unwrap();
        assert_eq!(again.blockers.len(), 1, "{:?}", again.blockers);
        assert_eq!(again.blockers[0].message, plan.blockers[0].message);
    }

    #[test]
    fn a_stray_merge_above_real_ones_does_not_stop_their_reconcile() {
        // auth really landed; profile's "merge" went into another branch.
        let snap = land_snapshot();
        let prs = pr_state(vec![], false);
        let mut forge = StubLandForge::default();
        forge.merged.insert("auth".into(), merged_pr(1, "auth"));
        forge.merged.insert(
            "profile".into(),
            PrSummary {
                merge_commit: Some("cb1feedface".into()),
                ..merged_by_reachability(7, "profile", "auth")
            },
        );
        let vcs = at_plan().saying("cb1feedface", LandedCheck::Elsewhere);

        let plan = plan_land(&snap, &prs, &forge_repo(), "profile", &forge, &vcs, &opts()).unwrap();
        assert!(plan.blockers.is_empty(), "{:?}", plan.blockers);
        assert!(
            plan.warnings.iter().any(|w| w.contains("#7 (profile) is closed as merged")),
            "{:?}",
            plan.warnings
        );
        // Only auth is checked, cleaned up and abandoned; profile's
        // changes are rebased onto the new trunk and kept.
        assert_eq!(
            plan.actions,
            vec![
                LandAction::FetchRemote { remote: "origin".into() },
                LandAction::CheckLanded {
                    number: 1,
                    bookmark: "auth".into(),
                    bottom_commit: "ca1".into(),
                    top_commit: "ca2".into(),
                },
                LandAction::RebaseOntoTrunk {
                    root_change: "b1".into(),
                    moves: 1,
                    off: vec!["ca2".into()],
                },
                LandAction::PushStack { bookmarks: vec!["profile".into()] },
                LandAction::CleanupBookmark {
                    bookmark: "auth".into(),
                    commit: "ca2".into(),
                },
                LandAction::AbandonLanded {
                    bookmark: "auth".into(),
                    change_ids: vec!["a2".into(), "a1".into()],
                    commits: vec!["ca2".into(), "ca1".into()],
                },
            ]
        );
        assert_eq!(plan.segments[1].status, LandSegmentStatus::Stacked);
    }

    #[test]
    fn a_merge_into_trunk_that_is_not_fetched_yet_plans_the_check() {
        // GitHub says #1 merged into main and names a commit this
        // repository has not fetched, or has but not on its stale trunk:
        // only the fetch can tell, so the plan fetches and then checks.
        let snap = land_snapshot();
        let prs = pr_state(vec![open_pr(7, "profile", "auth")], false);
        let mut forge = StubLandForge::default();
        forge.merged.insert("auth".into(), merged_pr(1, "auth"));
        for unknown in [LandedCheck::Unfetched, LandedCheck::Elsewhere] {
            let vcs = at_plan().saying("landed1", unknown);
            let plan =
                plan_land(&snap, &prs, &forge_repo(), "profile", &forge, &vcs, &opts()).unwrap();
            assert!(plan.blockers.is_empty(), "{:?}", plan.blockers);
            assert_eq!(
                &plan.actions[..2],
                [
                    LandAction::FetchRemote { remote: "origin".into() },
                    LandAction::CheckLanded {
                        number: 1,
                        bookmark: "auth".into(),
                        bottom_commit: "ca1".into(),
                        top_commit: "ca2".into(),
                    },
                ]
            );
        }
    }

    #[test]
    fn a_failed_landing_check_stops_before_anything_is_rewritten() {
        let snap = land_snapshot();
        let prs = pr_state(vec![open_pr(7, "profile", "auth")], false);
        let mut forge = StubLandForge::default();
        forge.merged.insert("auth".into(), merged_pr(1, "auth"));
        forge.states.insert(7, PrLandState { base_branch: "auth".into(), ..ready_state() });
        let plan =
            plan_land(&snap, &prs, &forge_repo(), "profile", &forge, &at_plan(), &opts()).unwrap();
        forge.calls.borrow_mut().clear();

        let cases = [
            (LandedCheck::Elsewhere, "is not part of \u{201c}main\u{201d}"),
            (
                LandedCheck::Missing { paths: vec!["a.rs".into()], more: 0 },
                "does not hold everything under \u{201c}auth\u{201d} (a.rs)",
            ),
            (LandedCheck::Unfetched, "the fetch did not bring that commit"),
        ];
        for (answer, story) in cases {
            // After the fetch the repository gives a different answer
            // than planning could.
            let vcs = StubVcs::new(post_fetch_snapshot()).saying("landed1", answer);
            let outcome = execute_land(&plan, &vcs, &forge).unwrap();
            assert!(outcome.failed);
            let failed = outcome
                .steps
                .iter()
                .find(|s| s.status == SubmitStepStatus::Failed)
                .expect("a failed step");
            assert!(matches!(failed.action, LandAction::CheckLanded { number: 1, .. }));
            let detail = failed.detail.as_deref().unwrap();
            assert!(detail.contains(story), "{detail}");
            assert!(
                detail.contains("nothing was rebased, pushed or cleaned up"),
                "{detail}"
            );
            // The fetch ran. No rebase, push, delete or abandon did, and
            // GitHub was asked to change nothing.
            assert_eq!(vcs.calls.borrow().as_slice(), ["fetch:origin"]);
            assert!(
                forge
                    .calls
                    .borrow()
                    .iter()
                    .all(|call| call.starts_with("land_state:")),
                "{:?}",
                forge.calls
            );
            forge.calls.borrow_mut().clear();
        }
    }

    #[test]
    fn the_runs_own_merge_is_checked_too() {
        let mut forge = StubLandForge::default();
        forge.states.insert(1, ready_state());
        forge.states.insert(7, PrLandState { base_branch: "auth".into(), ..ready_state() });
        let plan = ready_plan(&forge);
        // GitHub merged #1, and trunk still lacks a file of it.
        let vcs = StubVcs::new(post_fetch_snapshot()).saying(
            "landed1",
            LandedCheck::Missing { paths: vec!["auth/session.rs".into()], more: 0 },
        );

        let outcome = execute_land(&plan, &vcs, &forge).unwrap();
        assert!(outcome.failed);
        assert_eq!(outcome.steps[0].status, SubmitStepStatus::Done); // the merge
        assert_eq!(outcome.steps[2].status, SubmitStepStatus::Failed); // the check
        assert!(outcome.steps[3..]
            .iter()
            .all(|s| s.status == SubmitStepStatus::Skipped));
        assert_eq!(vcs.calls.borrow().as_slice(), ["fetch:origin"]);
        assert!(!forge.calls.borrow().iter().any(|c| c.starts_with("retarget")));
    }

    #[test]
    fn a_branch_that_moved_on_github_blocks_the_merge() {
        // Reproduced on GitHub: someone merged the pull request above
        // into this one's branch. The bookmark still reads "synced" (no
        // fetch since), and GitHub's own head would have been the lease.
        let snap = land_snapshot();
        let prs = pr_state(vec![open_pr(1, "auth", "main")], false);
        let mut forge = StubLandForge::default();
        forge.states.insert(
            1,
            PrLandState {
                head_commit: "bd4aafaa792eafce7f2216938b97facbb08b1ecc".into(),
                ..ready_state()
            },
        );

        let plan = plan_land(&snap, &prs, &forge_repo(), "auth", &forge, &at_plan(), &opts()).unwrap();
        assert!(plan.actions.is_empty(), "{:?}", plan.actions);
        assert_eq!(plan.blockers.len(), 1, "{:?}", plan.blockers);
        assert!(
            plan.blockers[0]
                .message
                .contains("#1's branch on GitHub is at bd4aafaa, not at ca2 where \u{201c}auth\u{201d} is here"),
            "{}",
            plan.blockers[0].message
        );
    }

    #[test]
    fn a_rebase_merge_leaves_no_divergent_copies_behind() {
        // Reproduced on GitHub: a rebase merge keeps jj's change ids, so
        // after the fetch each landed change exists twice, and the copy
        // here answers to its commit id. The old sweep looked the planned
        // change ids up, found only trunk's twins, and abandoned nothing.
        let mut forge = StubLandForge::default();
        forge.merged.insert("auth".into(), merged_pr(1, "auth"));
        let plan = {
            let snap = land_snapshot();
            let prs = pr_state(vec![], false);
            let mut planning = StubLandForge::default();
            planning.merged.insert("auth".into(), merged_pr(1, "auth"));
            plan_land(&snap, &prs, &forge_repo(), "auth", &planning, &at_plan(), &opts()).unwrap()
        };
        let divergent = |id: &str, change: &str, kind: NodeKind, parent: &str| GraphNode {
            change_id: change.into(),
            commit_id: id.into(),
            is_divergent: true,
            held: false,
            old_copies_held: false,
            ..node(id, "auth", kind, &[parent], &[])
        };
        let post = snapshot(
            vec![
                // Trunk's replayed commits, immutable, same change ids.
                divergent("f00d2", "a2", NodeKind::Immutable, "f00d1"),
                divergent("f00d1", "a1", NodeKind::Immutable, "m"),
                // The copies here, mutable, keyed by commit id.
                divergent("ca2", "a2", NodeKind::Mutable, "ca1"),
                divergent("ca1", "a1", NodeKind::Mutable, "m"),
                node("m", "release", NodeKind::Immutable, &[], &[]),
            ],
            vec![bookmark("main", "f00d2", SyncState::Synced, true)],
        );
        let vcs = StubVcs::new(post);

        let outcome = execute_land(&plan, &vcs, &forge).unwrap();
        assert!(!outcome.failed, "{:?}", outcome.steps);
        assert_eq!(
            vcs.calls.borrow().as_slice(),
            ["fetch:origin", "abandon:ca2,ca1"]
        );
    }

    // ---- GitHub's own stacks ----

    /// The land snapshot with both bookmarks in sync with GitHub.
    fn stacked_snapshot() -> RepoSnapshot {
        let mut snap = land_snapshot();
        snap.bookmarks[2].sync = SyncState::Synced;
        snap
    }

    /// #1 (auth) and #7 (profile) linked as GitHub stack 90.
    fn stacked_prs() -> RepoPrState {
        pr_state(
            vec![
                PrSummary { stack: seat(1, 2), ..open_pr(1, "auth", "main") },
                PrSummary {
                    stack: seat(2, 2),
                    head_commit: "cb1feedface".into(),
                    ..open_pr(7, "profile", "auth")
                },
            ],
            false,
        )
    }

    fn stacked_forge() -> StubLandForge {
        let mut forge = StubLandForge::default();
        forge
            .states
            .insert(1, PrLandState { stack: seat(1, 2), ..ready_state() });
        forge.states.insert(
            7,
            PrLandState {
                stack: seat(2, 2),
                base_branch: "auth".into(),
                head_commit: "cb1feedface".into(),
                ..ready_state()
            },
        );
        forge.stack_below.insert(7, vec![1]);
        forge
    }

    #[test]
    fn a_github_stack_lands_everything_ready_in_one_request() {
        let snap = stacked_snapshot();
        let forge = stacked_forge();
        let plan =
            plan_land(&snap, &stacked_prs(), &forge_repo(), "profile", &forge, &at_plan(), &opts())
                .unwrap();
        assert!(plan.blockers.is_empty(), "{:?}", plan.blockers);
        assert_eq!(
            plan.actions,
            vec![
                // One request for the top; #1 rides along, with its lease.
                LandAction::MergePr {
                    number: 7,
                    bookmark: "profile".into(),
                    method: MergeMethod::Squash,
                    expected_head: "cb1feedface".into(),
                    commit_title: Some("PR 7 (#7)".into()),
                    commit_message: Some("profile: avatars".into()),
                    with: vec![LandsWith {
                        number: 1,
                        bookmark: "auth".into(),
                        expected_head: "ca2feedface".into(),
                    }],
                    stack: Some(90),
                },
                LandAction::FetchRemote { remote: "origin".into() },
                // Each landed pull request is checked on its own.
                LandAction::CheckLanded {
                    number: 1,
                    bookmark: "auth".into(),
                    bottom_commit: "ca1".into(),
                    top_commit: "ca2".into(),
                },
                LandAction::CheckLanded {
                    number: 7,
                    bookmark: "profile".into(),
                    bottom_commit: "cb1".into(),
                    top_commit: "cb1".into(),
                },
                LandAction::CleanupBookmark {
                    bookmark: "auth".into(),
                    commit: "ca2".into(),
                },
                LandAction::CleanupBookmark {
                    bookmark: "profile".into(),
                    commit: "cb1".into(),
                },
                LandAction::AbandonLanded {
                    bookmark: "profile".into(),
                    change_ids: vec!["b1".into()],
                    commits: vec!["cb1".into()],
                },
                LandAction::AbandonLanded {
                    bookmark: "auth".into(),
                    change_ids: vec!["a2".into(), "a1".into()],
                    commits: vec!["ca2".into(), "ca1".into()],
                },
            ]
        );
        assert!(plan
            .segments
            .iter()
            .all(|s| s.status == LandSegmentStatus::Landing));
        assert!(
            plan.warnings.iter().any(|w| w.contains("#1 and #7 in one request")),
            "{:?}",
            plan.warnings
        );

        forge.calls.borrow_mut().clear();
        let vcs = StubVcs::new(post_fetch_snapshot());
        let outcome = execute_land(&plan, &vcs, &forge).unwrap();
        assert!(!outcome.failed, "{:?}", outcome.steps);
        let calls = forge.calls.borrow();
        // The stacked endpoint, once, for the top; never the plain one.
        assert_eq!(
            calls.iter().filter(|c| c.starts_with("merge")).collect::<Vec<_>>(),
            ["merge_stacked:7:squash"]
        );
        assert_eq!(
            outcome.steps[0].detail.as_deref(),
            Some("Merged #1 and #7 into main (GitHub stack 90)")
        );
        assert_eq!(
            vcs.asked.borrow().as_slice(),
            ["landed1:ca1..ca2", "landed7:cb1..cb1"]
        );
    }

    #[test]
    fn a_github_stack_run_stops_at_the_first_pull_request_not_ready() {
        let snap = stacked_snapshot();
        let mut forge = stacked_forge();
        // #7's checks are still running: #1 lands alone this round.
        forge.states.get_mut(&7).unwrap().checks = ChecksRollup::Pending;
        let plan =
            plan_land(&snap, &stacked_prs(), &forge_repo(), "profile", &forge, &at_plan(), &opts())
                .unwrap();
        assert!(plan.blockers.is_empty(), "{:?}", plan.blockers);
        assert_eq!(
            plan.actions,
            vec![
                LandAction::MergePr {
                    number: 1,
                    bookmark: "auth".into(),
                    method: MergeMethod::Squash,
                    expected_head: "ca2feedface".into(),
                    commit_title: Some("PR 1 (#1)".into()),
                    commit_message: Some("auth: login flow\n\nauth: sessions".into()),
                    with: vec![],
                    stack: Some(90),
                },
                // GitHub rebases and pushes #7 itself; wait for that
                // before fetching.
                LandAction::AwaitRestack {
                    number: 7,
                    bookmark: "profile".into(),
                    from_head: "cb1feedface".into(),
                    merged_here: true,
                },
                LandAction::FetchRemote { remote: "origin".into() },
                LandAction::CheckLanded {
                    number: 1,
                    bookmark: "auth".into(),
                    bottom_commit: "ca1".into(),
                    top_commit: "ca2".into(),
                },
                // No retarget (GitHub refuses one inside a stack and does
                // it itself); the rebase and push stay, and find nothing
                // to do when GitHub got there first.
                LandAction::RebaseOntoTrunk {
                    root_change: "b1".into(),
                    moves: 1,
                    off: vec!["ca2".into()],
                },
                LandAction::PushStack { bookmarks: vec!["profile".into()] },
                LandAction::CleanupBookmark {
                    bookmark: "auth".into(),
                    commit: "ca2".into(),
                },
                LandAction::AbandonLanded {
                    bookmark: "auth".into(),
                    change_ids: vec!["a2".into(), "a1".into()],
                    commits: vec!["ca2".into(), "ca1".into()],
                },
                // And no auto-merge coda: stacked pull requests have none.
            ]
        );
        assert_eq!(plan.segments[0].status, LandSegmentStatus::Landing);
        assert_eq!(plan.segments[1].status, LandSegmentStatus::Stacked);

        forge.calls.borrow_mut().clear();
        let vcs = StubVcs::new(post_fetch_snapshot());
        let outcome = execute_land(&plan, &vcs, &forge).unwrap();
        assert!(!outcome.failed, "{:?}", outcome.steps);
        assert!(forge
            .calls
            .borrow()
            .contains(&"await_restack:7:cb1feedface".to_owned()));
        assert_eq!(
            outcome.steps[1].detail.as_deref(),
            Some("GitHub rebased the rest of the stack onto main")
        );
    }

    #[test]
    fn a_restack_github_has_not_made_yet_stops_the_run_that_merged() {
        // The merge is seconds old and GitHub has neither moved #7's
        // branch nor called it conflicting. It is about to move it: a
        // rebase and push from here would leave two copies.
        let snap = stacked_snapshot();
        let mut forge = stacked_forge();
        forge.states.get_mut(&7).unwrap().checks = ChecksRollup::Pending;
        forge.restack_stalls = true;
        let plan =
            plan_land(&snap, &stacked_prs(), &forge_repo(), "profile", &forge, &at_plan(), &opts())
                .unwrap();
        let vcs = StubVcs::new(post_fetch_snapshot());
        let outcome = execute_land(&plan, &vcs, &forge).unwrap();
        assert!(outcome.failed, "{:?}", outcome.steps);
        assert_eq!(outcome.steps[0].status, SubmitStepStatus::Done);
        assert_eq!(outcome.steps[1].status, SubmitStepStatus::Failed);
        let story = outcome.steps[1].detail.as_deref().unwrap();
        assert!(
            story.contains("GitHub has not rebased #7 yet") && story.contains("land again"),
            "{story}"
        );
        // Nothing was fetched, rebased or pushed.
        assert!(vcs.calls.borrow().is_empty(), "{:?}", vcs.calls);
    }

    #[test]
    fn a_restack_github_refuses_is_made_here() {
        // Seen on the sandbox (R4b): when the rebase would conflict,
        // GitHub leaves the branch and marks the pull request dirty.
        let snap = stacked_snapshot();
        let mut forge = stacked_forge();
        forge.states.get_mut(&7).unwrap().checks = ChecksRollup::Pending;
        forge.restack_conflicts = true;
        let plan =
            plan_land(&snap, &stacked_prs(), &forge_repo(), "profile", &forge, &at_plan(), &opts())
                .unwrap();
        let vcs = StubVcs::new(post_fetch_snapshot());
        let outcome = execute_land(&plan, &vcs, &forge).unwrap();
        assert!(!outcome.failed, "{:?}", outcome.steps);
        let story = outcome.steps[1].detail.as_deref().unwrap();
        assert!(story.contains("GitHub cannot rebase #7: it conflicts"), "{story}");
        assert!(vcs.calls.borrow().contains(&"rebase:b1".to_owned()));
    }

    #[test]
    fn a_restack_still_missing_on_a_later_run_is_made_here() {
        // #1 merged before this run began (an earlier run stopped at the
        // wait, or someone merged on github.com). A branch GitHub has
        // still not moved by the end of another wait is one it is leaving
        // alone.
        let snap = stacked_snapshot();
        let mut forge = StubLandForge::default();
        forge.merged.insert(
            "auth".into(),
            PrSummary { stack: seat(1, 2), ..merged_pr(1, "auth") },
        );
        forge.restack_stalls = true;
        let prs = pr_state(
            vec![PrSummary {
                stack: seat(2, 2),
                head_commit: "cb1feedface".into(),
                ..open_pr(7, "profile", "main")
            }],
            false,
        );
        forge.states.insert(
            7,
            PrLandState { stack: seat(2, 2), head_commit: "cb1feedface".into(), ..ready_state() },
        );
        let plan =
            plan_land(&snap, &prs, &forge_repo(), "profile", &forge, &at_plan(), &opts()).unwrap();
        let vcs = StubVcs::new(post_fetch_snapshot());
        let outcome = execute_land(&plan, &vcs, &forge).unwrap();
        assert!(!outcome.failed, "{:?}", outcome.steps);
        assert!(outcome.steps[0]
            .detail
            .as_deref()
            .unwrap()
            .contains("GitHub has left #7 as it was"));
        assert!(vcs.calls.borrow().contains(&"rebase:b1".to_owned()));
        assert!(vcs.calls.borrow().contains(&"push:profile:origin".to_owned()));
    }

    #[test]
    fn a_wait_in_a_github_stack_is_reported_not_handed_to_auto_merge() {
        let snap = stacked_snapshot();
        let mut forge = stacked_forge();
        forge.states.get_mut(&1).unwrap().checks = ChecksRollup::Pending;
        let plan =
            plan_land(&snap, &stacked_prs(), &forge_repo(), "profile", &forge, &at_plan(), &opts())
                .unwrap();
        assert!(plan.actions.is_empty(), "{:?}", plan.actions);
        assert_eq!(plan.blockers.len(), 1, "{:?}", plan.blockers);
        assert!(plan.blockers[0].wait);
        assert!(
            plan.blockers[0]
                .message
                .contains("GitHub has no auto-merge for stacked pull requests"),
            "{}",
            plan.blockers[0].message
        );
    }

    #[test]
    fn bypass_in_a_github_stack_merges_the_bottom_alone() {
        let snap = stacked_snapshot();
        let forge = stacked_forge();
        let options = LandOptions { bypass: true, ..opts() };
        let plan = plan_land(
            &snap,
            &stacked_prs(),
            &forge_repo(),
            "profile",
            &forge,
            &at_plan(),
            &options,
        )
        .unwrap();
        assert!(matches!(
            &plan.actions[0],
            LandAction::MergePr { number: 1, with, stack: Some(90), .. } if with.is_empty()
        ));
        forge.calls.borrow_mut().clear();
        let vcs = StubVcs::new(post_fetch_snapshot());
        execute_land(&plan, &vcs, &forge).unwrap();
        assert!(forge
            .calls
            .borrow()
            .contains(&"merge_stacked:1:squash:bypass".to_owned()));
    }

    #[test]
    fn a_stack_that_changed_since_the_plan_refuses_the_merge() {
        // Planned as a plain chain; linked on github.com before the run.
        // The plain endpoint would be refused, and a stacked merge of the
        // bottom is a different thing from what was confirmed.
        let mut forge = StubLandForge::default();
        forge.states.insert(1, ready_state());
        forge.states.insert(7, PrLandState { base_branch: "auth".into(), ..ready_state() });
        let plan = ready_plan(&forge);
        forge.states.get_mut(&1).unwrap().stack = seat(1, 2);
        forge.calls.borrow_mut().clear();
        let vcs = StubVcs::new(post_fetch_snapshot());
        let outcome = execute_land(&plan, &vcs, &forge).unwrap();
        assert!(outcome.failed);
        assert!(outcome.steps[0]
            .detail
            .as_deref()
            .unwrap()
            .contains("the GitHub stack around #1 changed"));
        assert!(!forge.calls.borrow().iter().any(|c| c.starts_with("merge")));

        // The other way round: planned as a stack of two, dissolved since.
        // Merging #7 through the plain endpoint would land it in "auth".
        let snap = stacked_snapshot();
        let mut forge = stacked_forge();
        let plan =
            plan_land(&snap, &stacked_prs(), &forge_repo(), "profile", &forge, &at_plan(), &opts())
                .unwrap();
        forge.states.get_mut(&7).unwrap().stack = None;
        forge.states.get_mut(&1).unwrap().stack = None;
        forge.calls.borrow_mut().clear();
        let outcome = execute_land(&plan, &StubVcs::new(post_fetch_snapshot()), &forge).unwrap();
        assert!(outcome.failed);
        assert!(!forge.calls.borrow().iter().any(|c| c.starts_with("merge")));
    }

    #[test]
    fn a_lower_pull_request_that_moved_refuses_the_stacked_merge() {
        let snap = stacked_snapshot();
        let mut forge = stacked_forge();
        let plan =
            plan_land(&snap, &stacked_prs(), &forge_repo(), "profile", &forge, &at_plan(), &opts())
                .unwrap();
        // Something pushed to #1's branch after the plan was read.
        forge.states.get_mut(&1).unwrap().head_commit = "0ddba11".into();
        forge.calls.borrow_mut().clear();
        let outcome = execute_land(&plan, &StubVcs::new(post_fetch_snapshot()), &forge).unwrap();
        assert!(outcome.failed);
        assert!(outcome.steps[0]
            .detail
            .as_deref()
            .unwrap()
            .contains("#1's branch moved"));
        assert!(!forge.calls.borrow().iter().any(|c| c.starts_with("merge")));
    }

    #[test]
    fn reconciling_a_github_stack_waits_for_its_restack_only_when_needed() {
        // #1 merged on github.com; #7 is next in the same stack.
        let snap = stacked_snapshot();
        let mut forge = StubLandForge::default();
        forge.merged.insert(
            "auth".into(),
            PrSummary { stack: seat(1, 2), ..merged_pr(1, "auth") },
        );
        let seated = |head: &str| {
            pr_state(
                vec![PrSummary {
                    stack: seat(2, 2),
                    head_commit: head.into(),
                    ..open_pr(7, "profile", "main")
                }],
                false,
            )
        };
        // GitHub still shows the commit that is here: it has not pushed
        // its rebase yet, so the run waits for it before fetching.
        let plan = plan_land(
            &snap,
            &seated("cb1feedface"),
            &forge_repo(),
            "profile",
            &forge,
            &at_plan(),
            &opts(),
        )
        .unwrap();
        assert_eq!(
            plan.actions[0],
            LandAction::AwaitRestack {
                number: 7,
                bookmark: "profile".into(),
                from_head: "cb1feedface".into(),
                merged_here: false,
            }
        );
        assert!(!plan
            .actions
            .iter()
            .any(|a| matches!(a, LandAction::RetargetPr { .. } | LandAction::EnableAutoMerge { .. })));

        // GitHub's branch already differs from the bookmark here: it has
        // moved, and the fetch brings it.
        let plan = plan_land(
            &snap,
            &seated("9a11ed0"),
            &forge_repo(),
            "profile",
            &forge,
            &at_plan(),
            &opts(),
        )
        .unwrap();
        assert_eq!(
            plan.actions[0],
            LandAction::FetchRemote { remote: "origin".into() }
        );

        // The next pull request in the stack has no bookmark here: nothing
        // of it is rebased or pushed from here, so there is nothing to
        // wait for.
        let prs = pr_state(
            vec![PrSummary {
                stack: seat(2, 2),
                head_commit: "cb1feedface".into(),
                ..open_pr(9, "someone-elses", "main")
            }],
            false,
        );
        let plan =
            plan_land(&snap, &prs, &forge_repo(), "auth", &forge, &at_plan(), &opts()).unwrap();
        assert_eq!(
            plan.actions[0],
            LandAction::FetchRemote { remote: "origin".into() }
        );
    }

    // ---- Round 2: what the first reviews found ----

    #[test]
    fn a_merge_in_a_github_stack_waits_for_unpublished_work_above_it() {
        // Reproduced on GitHub (R2): #1 merged while "profile" had an
        // edit here that was not on GitHub. GitHub rebased its own copy
        // of the branch; after the fetch the change was divergent, the
        // bookmark conflicted, and the push refused.
        let snap = land_snapshot(); // "profile" is ahead of its remote
        let forge = stacked_forge();
        let plan =
            plan_land(&snap, &stacked_prs(), &forge_repo(), "profile", &forge, &at_plan(), &opts())
                .unwrap();
        assert!(plan.actions.is_empty(), "{:?}", plan.actions);
        assert_eq!(plan.blockers.len(), 1, "{:?}", plan.blockers);
        let blocker = &plan.blockers[0];
        assert!(!blocker.wait && !blocker.fetch);
        assert!(
            blocker.message.contains("\u{201c}profile\u{201d} (#7) differs here from its branch")
                && blocker.message.contains("publish first"),
            "{}",
            blocker.message
        );
        assert_eq!(plan.segments[0].status, LandSegmentStatus::Waiting);

        // The same chain not linked as a GitHub stack: Jiji does the
        // rebase itself, from the one copy there is, and #1 lands.
        let mut plain = StubLandForge::default();
        plain.states.insert(1, ready_state());
        let prs = pr_state(
            vec![open_pr(1, "auth", "main"), open_pr(7, "profile", "auth")],
            false,
        );
        let plan =
            plan_land(&snap, &prs, &forge_repo(), "profile", &plain, &at_plan(), &opts()).unwrap();
        assert!(plan.blockers.is_empty(), "{:?}", plan.blockers);
        assert!(matches!(plan.actions[0], LandAction::MergePr { number: 1, .. }));
    }

    /// A pull request from a fork has a branch name too, and it may be
    /// the name of a bookmark here. It is never taken for that bookmark's:
    /// not as the unpublished work that makes a merge below it wait, and
    /// not as the branch whose rebase by GitHub is waited for.
    #[test]
    fn a_forks_pull_request_above_in_a_github_stack_is_not_a_bookmark_here() {
        let snap = land_snapshot(); // "profile" is ahead of its remote
        let from_a_fork = |base: &str| PrSummary {
            stack: seat(2, 2),
            head_owner: Some("a-fork".into()),
            author: Some("sam".into()),
            ..open_pr(19, "profile", base)
        };
        // Before a merge: nothing of "profile" here is in that stack.
        let mut forge = StubLandForge::default();
        forge.states.insert(
            1,
            PrLandState { stack: seat(1, 2), ..ready_state() },
        );
        let prs = pr_state(
            vec![
                PrSummary { stack: seat(1, 2), ..open_pr(1, "auth", "main") },
                from_a_fork("auth"),
            ],
            false,
        );
        let plan =
            plan_land(&snap, &prs, &forge_repo(), "auth", &forge, &at_plan(), &opts()).unwrap();
        assert!(
            !plan.blockers.iter().any(|b| b.message.contains("differs here from its branch")),
            "{:?}",
            plan.blockers
        );

        // After a merge made on github.com: no wait for a rebase of a
        // branch that is not here, and no word about two copies.
        let mut forge = StubLandForge::default();
        forge.merged.insert(
            "auth".into(),
            PrSummary { stack: seat(1, 2), ..merged_pr(1, "auth") },
        );
        let prs = pr_state(vec![from_a_fork("main")], false);
        let plan =
            plan_land(&snap, &prs, &forge_repo(), "auth", &forge, &at_plan(), &opts()).unwrap();
        assert!(
            !plan.warnings.iter().any(|w| w.contains("differs here from its branch")),
            "{:?}",
            plan.warnings
        );
        assert!(
            !plan.actions.iter().any(|a| matches!(a, LandAction::AwaitRestack { .. })),
            "{:?}",
            plan.actions
        );
    }

    #[test]
    fn a_merge_someone_else_made_below_unpublished_work_is_reconciled_with_a_warning() {
        // #1 merged on github.com. Nothing here can stop GitHub's rebase
        // of "profile" any more, and refusing to reconcile would leave no
        // way through (the second Land of R2 is what finished the job).
        let snap = land_snapshot();
        let mut forge = StubLandForge::default();
        forge.merged.insert(
            "auth".into(),
            PrSummary { stack: seat(1, 2), ..merged_pr(1, "auth") },
        );
        let prs = pr_state(
            vec![PrSummary {
                stack: seat(2, 2),
                head_commit: "9a11ed0".into(),
                ..open_pr(7, "profile", "main")
            }],
            false,
        );
        let plan =
            plan_land(&snap, &prs, &forge_repo(), "profile", &forge, &at_plan(), &opts()).unwrap();
        assert!(plan.blockers.is_empty(), "{:?}", plan.blockers);
        assert_eq!(plan.actions[0], LandAction::FetchRemote { remote: "origin".into() });
        assert!(
            plan.warnings.iter().any(|w| w.contains("\u{201c}profile\u{201d} (#7) differs here")
                && w.contains("the change exists twice")),
            "{:?}",
            plan.warnings
        );

        // After the fetch the change answers to two commit ids. The run
        // checks the landing, then stops before it rewrites anything.
        let twice = |id: &str, parent: &str, bookmarks: &[&str]| GraphNode {
            change_id: "b1".into(),
            commit_id: id.into(),
            is_divergent: true,
            held: false,
            old_copies_held: false,
            ..node(id, "profile: avatars", NodeKind::Mutable, &[parent], bookmarks)
        };
        let post = snapshot(
            vec![
                twice("9a11ed0", "s", &[]),
                twice("cb1", "a2", &["profile"]),
                node("a2", "auth: sessions", NodeKind::Mutable, &["a1"], &["auth"]),
                node("a1", "auth: login flow", NodeKind::Mutable, &["m"], &[]),
                node("s", "auth (squash)", NodeKind::Immutable, &["m"], &["main"]),
                node("m", "release", NodeKind::Immutable, &[], &[]),
            ],
            vec![
                bookmark("main", "s", SyncState::Synced, true),
                bookmark("auth", "a2", SyncState::Synced, false),
                bookmark("profile", "cb1", SyncState::Diverged, false),
            ],
        );
        let vcs = StubVcs::new(post);
        let outcome = execute_land(&plan, &vcs, &forge).unwrap();
        assert!(outcome.failed);
        let failed = outcome
            .steps
            .iter()
            .find(|s| s.status == SubmitStepStatus::Failed)
            .unwrap();
        assert!(matches!(failed.action, LandAction::RebaseOntoTrunk { .. }));
        let story = failed.detail.as_deref().unwrap();
        assert!(
            story.contains("b1 exists more than once here now (9a11ed0, cb1)")
                && story.contains("Nothing was rebased or pushed"),
            "{story}"
        );
        assert_eq!(vcs.calls.borrow().as_slice(), ["fetch:origin"]);
        assert!(outcome
            .steps
            .iter()
            .any(|s| matches!(s.action, LandAction::CheckLanded { .. })
                && s.status == SubmitStepStatus::Done));
    }

    /// Seen on the sandbox: the stack was reordered and published after
    /// someone else had built on it, and Land's own fetch then brought
    /// their branch, with the old commits of the stack's changes under
    /// it. Each change of the stack has two commits from then on, and
    /// the old one is theirs to keep: nothing here can abandon it. The
    /// rebase used to stop with "abandon the copy you do not want", after
    /// the merge and the retargets. It goes on with the commit that is
    /// the user's.
    #[test]
    fn a_fetch_that_brings_old_copies_someone_else_built_on_does_not_stop_the_rebase() {
        let mut forge = StubLandForge::default();
        forge.states.insert(1, ready_state());
        forge.states.insert(7, PrLandState { base_branch: "auth".into(), ..ready_state() });
        let plan = ready_plan(&forge);
        assert!(plan.actions.iter().any(
            |a| matches!(a, LandAction::RebaseOntoTrunk { root_change, .. } if root_change == "b1")
        ));
        // After the fetch: b1's change answers to its commit id, and the
        // other commit of it is not drawn here (it is their branch's).
        let after = |old_copies_held: bool| {
            let mut post = post_fetch_snapshot();
            post.nodes[0].id = "cb1".into();
            post.nodes[0].commit_id = "cb1".into();
            post.nodes[0].change_id = "b1".into();
            post.nodes[0].is_divergent = true;
            post.nodes[0].old_copies_held = old_copies_held;
            post.bookmarks[2].target = "cb1".into();
            post
        };
        let vcs = StubVcs::new(after(true));
        let outcome = execute_land(&plan, &vcs, &forge).unwrap();
        let rebase = outcome
            .steps
            .iter()
            .find(|s| matches!(s.action, LandAction::RebaseOntoTrunk { .. }))
            .unwrap();
        assert_eq!(rebase.status, SubmitStepStatus::Done, "{:?}", rebase.detail);
        // By the id it answers to now.
        assert!(vcs.calls.borrow().iter().any(|c| c == "rebase:cb1"), "{:?}", vcs.calls.borrow());

        // A second commit that is not someone else's to keep is still a
        // divergence to resolve first, and the run stops as before.
        let mut forge = StubLandForge::default();
        forge.states.insert(1, ready_state());
        forge.states.insert(7, PrLandState { base_branch: "auth".into(), ..ready_state() });
        let vcs = StubVcs::new(after(false));
        let outcome = execute_land(&plan, &vcs, &forge).unwrap();
        assert!(outcome.failed);
        let failed = outcome
            .steps
            .iter()
            .find(|s| s.status == SubmitStepStatus::Failed)
            .unwrap();
        assert!(
            failed.detail.as_deref().unwrap().contains("b1 exists more than once here now (cb1)"),
            "{:?}",
            failed.detail
        );
        assert!(!vcs.calls.borrow().iter().any(|c| c.starts_with("rebase")));
    }

    #[test]
    fn cleanup_leaves_what_changed_after_the_plan_was_made() {
        // A valid squash of "auth" is planned for cleanup. Before the run
        // gets there, a2 is amended (same change, another commit, a new
        // file) and "auth" follows it.
        let snap = land_snapshot();
        let mut forge = StubLandForge::default();
        forge.merged.insert("auth".into(), merged_pr(1, "auth"));
        let prs = pr_state(vec![], false);
        let plan =
            plan_land(&snap, &prs, &forge_repo(), "auth", &forge, &at_plan(), &opts()).unwrap();
        assert_eq!(
            &plan.actions[plan.actions.len() - 2..],
            [
                LandAction::CleanupBookmark {
                    bookmark: "auth".into(),
                    commit: "ca2".into(),
                },
                LandAction::AbandonLanded {
                    bookmark: "auth".into(),
                    change_ids: vec!["a2".into(), "a1".into()],
                    commits: vec!["ca2".into(), "ca1".into()],
                },
            ]
        );

        // (b1 keeps its own commit: the shared fixture gives it the id
        // of a pushed rebase, which a real snapshot could not shorten to
        // the same prefix as a2's.)
        let after = |a2_commit: &str| {
            let mut post = post_fetch_snapshot();
            post.nodes[0].commit_id = "cb1".into();
            post.nodes[1].commit_id = a2_commit.into();
            post
        };
        let vcs = StubVcs::new(after("edited99")); // a2, amended
        let outcome = execute_land(&plan, &vcs, &forge).unwrap();
        assert!(!outcome.failed, "{:?}", outcome.steps);
        let detail = |wanted: fn(&LandAction) -> bool| {
            outcome
                .steps
                .iter()
                .find(|s| wanted(&s.action))
                .and_then(|s| s.detail.clone())
                .unwrap()
        };
        let bookmark_story = detail(|a| matches!(a, LandAction::CleanupBookmark { .. }));
        assert!(
            bookmark_story.contains("points at edited99 now, not at ca2")
                && bookmark_story.contains("left in place"),
            "{bookmark_story}"
        );
        let abandon_story = detail(|a| matches!(a, LandAction::AbandonLanded { .. }));
        assert!(
            abandon_story.contains("a2 changed here after this plan was made"),
            "{abandon_story}"
        );
        // The bookmark was not deleted here or on the remote, and only
        // the commit that was checked and is still here was abandoned.
        // a2, edited, sat on a1: it goes onto trunk before a1 is
        // abandoned, so its tree keeps what landed.
        assert_eq!(
            vcs.calls.borrow().as_slice(),
            ["fetch:origin", "rebase:b1", "rebase:a2", "abandon:ca1"]
        );

        // Untouched, both go.
        let vcs = StubVcs::new(after("ca2"));
        let outcome = execute_land(&plan, &vcs, &forge).unwrap();
        assert!(!outcome.failed, "{:?}", outcome.steps);
        assert_eq!(
            vcs.calls.borrow().as_slice(),
            ["fetch:origin", "rebase:b1", "delete:auth", "push:auth:origin", "abandon:ca2,ca1"]
        );

        // An edit the working copy had not recorded when the run looked
        // is caught by the backend: it refuses, and the step says so.
        let mut vcs = StubVcs::new(after("ca2"));
        vcs.abandon_refuses = true;
        let outcome = execute_land(&plan, &vcs, &forge).unwrap();
        assert!(outcome.failed);
        let story = outcome.steps.last().unwrap().detail.as_deref().unwrap();
        // By then "auth" is deleted, here and on the remote: no later
        // Land can be pointed at these commits, and the step says what is
        // left to do by hand without telling anyone to abandon an edit.
        assert!(
            story.contains("has been rewritten")
                && story.contains("nothing was abandoned")
                && story.contains("Land will not come back for a2, a1")
                && story.contains("or was added to them since")
                && !story.contains("land again"),
            "{story}"
        );
    }

    #[test]
    fn a_stacked_pull_request_closed_into_the_branch_below_is_told_as_that() {
        // #7 sits in GitHub stack 90 on main, and GitHub closed it as
        // merged because its head turned up inside "auth", the branch
        // below it. The stack's base says main; the truth is "auth".
        let snap = land_snapshot();
        let prs = pr_state(vec![open_pr(1, "auth", "main")], false);
        let mut forge = StubLandForge::default();
        forge.states.insert(1, ready_state());
        forge.merged.insert(
            "profile".into(),
            PrSummary {
                stack: seat(2, 2),
                head_commit: "cb1feedface".into(),
                merge_commit: Some("cb1feedface".into()),
                ..merged_by_reachability(7, "profile", "auth")
            },
        );
        let vcs = at_plan().saying("cb1feedface", LandedCheck::Elsewhere);
        // Asked for "profile" alone would walk both; ask the walk to
        // reach it past a merged "auth".
        forge.merged.insert("auth".into(), merged_pr(1, "auth"));
        let prs_none = pr_state(vec![], false);
        let plan =
            plan_land(&snap, &prs_none, &forge_repo(), "profile", &forge, &vcs, &opts()).unwrap();
        let said: Vec<&str> = plan
            .warnings
            .iter()
            .map(String::as_str)
            .chain(plan.blockers.iter().map(|b| b.message.as_str()))
            .collect();
        assert!(
            said.iter().any(|m| m
                .contains("merged into \u{201c}auth\u{201d}, not \u{201c}main\u{201d}")),
            "{said:?}"
        );
        assert!(!said.iter().any(|m| m.contains("\u{201c}main\u{201d}, not \u{201c}main\u{201d}")));
        let _ = prs;
    }

    #[test]
    fn an_enqueued_answer_to_a_stacked_merge_is_not_a_merge() {
        // GitHub answers "enqueued" for a pull request that already sits
        // in a merge queue, whatever was asked (its documentation; the
        // sandbox has no queue to try it on).
        let snap = stacked_snapshot();
        let mut forge = stacked_forge();
        forge.answers_enqueued = true;
        let plan =
            plan_land(&snap, &stacked_prs(), &forge_repo(), "profile", &forge, &at_plan(), &opts())
                .unwrap();
        let vcs = StubVcs::new(post_fetch_snapshot());
        let outcome = execute_land(&plan, &vcs, &forge).unwrap();
        assert!(outcome.failed);
        assert_eq!(outcome.steps[0].status, SubmitStepStatus::Failed);
        let story = outcome.steps[0].detail.as_deref().unwrap();
        assert!(story.contains("merge queue") && story.contains("has not merged yet"), "{story}");
        // No fetch, no check, no cleanup on a merge that did not happen.
        assert!(vcs.calls.borrow().is_empty(), "{:?}", vcs.calls);
        assert!(outcome.steps[1..]
            .iter()
            .all(|s| s.status == SubmitStepStatus::Skipped));
    }

    #[test]
    fn a_github_stack_that_lands_elsewhere_is_not_merged_from_here() {
        // The stack's bottom pull request merges into "production"; #1
        // sits above it and its own base reads "main". A merge of #1
        // would take the pull request below along, into "production".
        let elsewhere = || {
            Some(StackSeat {
                number: 90,
                position: 2,
                size: 3,
                base: "production".into(),
            })
        };
        let snap = stacked_snapshot();
        let mut forge = StubLandForge::default();
        forge
            .states
            .insert(1, PrLandState { stack: elsewhere(), ..ready_state() });
        let prs = pr_state(
            vec![PrSummary { stack: elsewhere(), ..open_pr(1, "auth", "main") }],
            false,
        );
        let plan =
            plan_land(&snap, &prs, &forge_repo(), "auth", &forge, &at_plan(), &opts()).unwrap();
        assert!(plan.actions.is_empty(), "{:?}", plan.actions);
        assert!(
            plan.blockers.iter().any(|b| !b.wait
                && b.message.contains("GitHub stack 90, which lands on \u{201c}production\u{201d}")),
            "{:?}",
            plan.blockers
        );

        // Planned while the stack landed on main, moved under it since:
        // the merge is refused at the last look.
        let mut forge = stacked_forge();
        let plan =
            plan_land(&snap, &stacked_prs(), &forge_repo(), "profile", &forge, &at_plan(), &opts())
                .unwrap();
        forge.states.get_mut(&7).unwrap().stack = Some(StackSeat {
            base: "production".into(),
            ..seat(2, 2).unwrap()
        });
        forge.calls.borrow_mut().clear();
        let outcome = execute_land(&plan, &StubVcs::new(post_fetch_snapshot()), &forge).unwrap();
        assert!(outcome.failed);
        assert!(outcome.steps[0]
            .detail
            .as_deref()
            .unwrap()
            .contains("the GitHub stack around #7 changed"));
        assert!(!forge.calls.borrow().iter().any(|c| c.starts_with("merge")));

        // The same for a stacked pull request that lands alone, where
        // there is no second look to catch it.
        let mut forge = stacked_forge();
        let plan =
            plan_land(&snap, &stacked_prs(), &forge_repo(), "auth", &forge, &at_plan(), &opts())
                .unwrap();
        assert!(
            matches!(&plan.actions[0], LandAction::MergePr { number: 1, with, stack: Some(90), .. } if with.is_empty()),
            "{:?}",
            plan.actions[0]
        );
        forge.states.get_mut(&1).unwrap().stack = Some(StackSeat {
            base: "production".into(),
            ..seat(1, 2).unwrap()
        });
        forge.calls.borrow_mut().clear();
        let outcome = execute_land(&plan, &StubVcs::new(post_fetch_snapshot()), &forge).unwrap();
        assert!(outcome.failed);
        assert!(outcome.steps[0]
            .detail
            .as_deref()
            .unwrap()
            .contains("the GitHub stack around #1 changed"));
        assert!(!forge.calls.borrow().iter().any(|c| c.starts_with("merge")));
    }

    /// main(m) ── a1 (auth) ── b1 (bare, pushed, no pull request) ── c1
    /// (cloud, @): three segments, the middle one without a pull request.
    fn gapped_snapshot() -> RepoSnapshot {
        snapshot(
            vec![
                node("c1", "cloud: sync", NodeKind::WorkingCopy, &["b1"], &["cloud"]),
                node("b1", "bare: no pull request", NodeKind::Mutable, &["a1"], &["bare"]),
                node("a1", "auth: login flow", NodeKind::Mutable, &["m"], &["auth"]),
                node("m", "release", NodeKind::Immutable, &[], &["main"]),
            ],
            vec![
                bookmark("main", "m", SyncState::Synced, true),
                bookmark("auth", "a1", SyncState::Synced, false),
                bookmark("bare", "b1", SyncState::Synced, false),
                bookmark("cloud", "c1", SyncState::Synced, false),
            ],
        )
    }

    #[test]
    fn the_hand_off_after_a_merge_goes_to_the_segment_directly_above_only() {
        // #9 targets "bare", which has no pull request. Pointed at main
        // and armed for auto-merge, #9 would show and land bare's commits
        // with nobody having opened a pull request for them.
        let snap = gapped_snapshot();
        let prs = pr_state(
            vec![
                PrSummary { head_commit: "ca1feedface".into(), ..open_pr(1, "auth", "main") },
                PrSummary { head_commit: "cc1feedface".into(), ..open_pr(9, "cloud", "bare") },
            ],
            false,
        );
        let mut forge = StubLandForge::default();
        forge
            .states
            .insert(1, PrLandState { head_commit: "ca1feedface".into(), ..ready_state() });
        forge.states.insert(
            9,
            PrLandState {
                base_branch: "bare".into(),
                head_commit: "cc1feedface".into(),
                ..ready_state()
            },
        );
        let plan =
            plan_land(&snap, &prs, &forge_repo(), "cloud", &forge, &at_plan(), &opts()).unwrap();
        assert!(plan.blockers.is_empty(), "{:?}", plan.blockers);
        assert!(matches!(plan.actions[0], LandAction::MergePr { number: 1, .. }));
        assert!(
            !plan.actions.iter().any(|a| matches!(
                a,
                LandAction::RetargetPr { .. } | LandAction::EnableAutoMerge { .. }
            )),
            "{:?}",
            plan.actions
        );
        // The rest of the stack is still rebased and pushed.
        assert!(plan.actions.contains(&LandAction::PushStack {
            bookmarks: vec!["bare".into(), "cloud".into()]
        }));

        // The same when #1 merged before the run.
        let mut merged = StubLandForge::default();
        merged.merged.insert("auth".into(), merged_pr(1, "auth"));
        merged.states.insert(
            9,
            PrLandState {
                base_branch: "bare".into(),
                head_commit: "cc1feedface".into(),
                ..ready_state()
            },
        );
        let prs = pr_state(
            vec![PrSummary { head_commit: "cc1feedface".into(), ..open_pr(9, "cloud", "bare") }],
            false,
        );
        let plan =
            plan_land(&snap, &prs, &forge_repo(), "cloud", &merged, &at_plan(), &opts()).unwrap();
        assert!(
            !plan.actions.iter().any(|a| matches!(
                a,
                LandAction::RetargetPr { .. } | LandAction::EnableAutoMerge { .. }
            )),
            "{:?}",
            plan.actions
        );
    }

    #[test]
    fn automation_is_not_handed_a_pull_request_that_points_somewhere_else() {
        // Waiting on checks: the plan arms auto-merge on #1.
        let snap = stacked_snapshot();
        let prs = pr_state(vec![open_pr(1, "auth", "main")], false);
        let waiting = PrLandState {
            checks: ChecksRollup::Pending,
            merge_state: MergeStateStatus::Blocked,
            ..ready_state()
        };
        let mut forge = StubLandForge::default();
        forge.states.insert(1, waiting.clone());
        let plan =
            plan_land(&snap, &prs, &forge_repo(), "auth", &forge, &at_plan(), &opts()).unwrap();
        assert!(matches!(plan.actions[0], LandAction::EnableAutoMerge { number: 1, .. }));
        // Retargeted to "release" after the plan, head unchanged.
        forge
            .states
            .insert(1, PrLandState { base_branch: "release".into(), ..waiting });
        forge.calls.borrow_mut().clear();
        let outcome = execute_land(&plan, &StubVcs::new(post_fetch_snapshot()), &forge).unwrap();
        assert!(outcome.failed);
        assert!(outcome.steps[0]
            .detail
            .as_deref()
            .unwrap()
            .contains("#1 now targets \u{201c}release\u{201d}, not \u{201c}main\u{201d}"));
        assert!(forge.automation_calls.borrow().is_empty());

        // The same for the merge queue.
        let queued = PrLandState {
            queue_on_base: true,
            queue_method: Some(MergeMethod::Squash),
            ..ready_state()
        };
        let mut forge = StubLandForge::default();
        forge.states.insert(1, queued.clone());
        let plan =
            plan_land(&snap, &prs, &forge_repo(), "auth", &forge, &at_plan(), &opts()).unwrap();
        assert!(matches!(plan.actions[0], LandAction::EnqueuePr { number: 1, .. }));
        forge
            .states
            .insert(1, PrLandState { base_branch: "release".into(), ..queued });
        let outcome = execute_land(&plan, &StubVcs::new(post_fetch_snapshot()), &forge).unwrap();
        assert!(outcome.failed);
        assert!(outcome.steps[0]
            .detail
            .as_deref()
            .unwrap()
            .contains("#1 now targets \u{201c}release\u{201d}"));
        assert!(forge.enqueue_calls.borrow().is_empty());
    }

    #[test]
    fn a_merged_pull_request_is_judged_with_the_head_it_was_merged_from() {
        // "auth" had a pull request, #1, merged from commit 0ddba11 long
        // ago (and reverted since). The work under "auth" now is other
        // commits. The repository is told which head #1 had, and answers
        // by trunk's tip: these changes are not there.
        let snap = land_snapshot();
        let prs = pr_state(vec![], false);
        let mut forge = StubLandForge::default();
        forge.merged.insert(
            "auth".into(),
            PrSummary { head_commit: "0ddba11f00d".into(), ..merged_pr(1, "auth") },
        );
        let vcs = at_plan().saying(
            "landed1",
            LandedCheck::Missing { paths: vec!["flag.rs".into()], more: 0 },
        );
        let plan =
            plan_land(&snap, &prs, &forge_repo(), "auth", &forge, &vcs, &opts()).unwrap();
        assert_eq!(vcs.heads.borrow().as_slice(), ["0ddba11f00d"]);
        assert!(plan.actions.is_empty(), "{:?}", plan.actions);
        assert!(
            plan.blockers[0].message.contains(
                "it was merged from another commit than \u{201c}auth\u{201d} has here"
            ) && plan.blockers[0].message.contains("(flag.rs)"),
            "{}",
            plan.blockers[0].message
        );

        // At the check after a merge, the head GitHub reports for the
        // merged pull request is the one passed on. One that is not the
        // commit here (a push landed with it: GitHub's lease in a stack
        // covers only the pull request asked for) is said, even when
        // trunk holds everything.
        let mut forge = StubLandForge::default();
        forge.merged.insert("auth".into(), merged_pr(1, "auth"));
        let plan =
            plan_land(&snap, &prs, &forge_repo(), "auth", &forge, &at_plan(), &opts()).unwrap();
        forge.merged.insert(
            "auth".into(),
            PrSummary { head_commit: "0ddba11f00d".into(), ..merged_pr(1, "auth") },
        );
        let vcs = StubVcs::new(post_fetch_snapshot()).saying("landed1", LandedCheck::Held);
        let outcome = execute_land(&plan, &vcs, &forge).unwrap();
        assert!(!outcome.failed, "{:?}", outcome.steps);
        assert_eq!(vcs.heads.borrow().as_slice(), ["0ddba11f00d"]);
        let story = outcome
            .steps
            .iter()
            .find(|s| matches!(s.action, LandAction::CheckLanded { .. }))
            .and_then(|s| s.detail.as_deref())
            .unwrap();
        assert!(
            story.contains("#1 was merged from 0ddba11f, not from ca2 as it is here"),
            "{story}"
        );
    }

    /// After a merge-commit landing of "auth" and a fetch: trunk's merge
    /// commit has "auth"'s own commits as ancestors, so they are
    /// immutable, and "profile" still sits on them.
    fn merge_landed_snapshot() -> RepoSnapshot {
        snapshot(
            vec![
                node("b1", "profile: avatars", NodeKind::WorkingCopy, &["a2"], &["profile"]),
                node("mm", "Merge pull request #1", NodeKind::Immutable, &["m", "a2"], &["main"]),
                node("a2", "auth: sessions", NodeKind::Immutable, &["a1"], &["auth"]),
                node("a1", "auth: login flow", NodeKind::Immutable, &["m"], &[]),
                node("m", "release", NodeKind::Immutable, &[], &[]),
            ],
            vec![
                bookmark("main", "mm", SyncState::Synced, true),
                bookmark("auth", "a2", SyncState::Synced, false),
                bookmark("profile", "b1", SyncState::Synced, false),
            ],
        )
    }

    /// The same repository once "profile" has been rebased onto trunk:
    /// "auth"'s commits are no longer drawn, though its bookmark is there.
    fn undrawn_after() -> RepoSnapshot {
        snapshot(
            vec![
                node("b1", "profile: avatars", NodeKind::WorkingCopy, &["mm"], &["profile"]),
                node("mm", "Merge pull request #1", NodeKind::Immutable, &["m", "a2"], &["main"]),
                node("m", "release", NodeKind::Immutable, &[], &[]),
            ],
            vec![
                bookmark("main", "mm", SyncState::Synced, true),
                bookmark("auth", "a2", SyncState::Synced, false),
                bookmark("profile", "b1", SyncState::Synced, false),
            ],
        )
    }

    #[test]
    fn a_landed_bookmark_left_on_trunk_history_is_still_reconciled() {
        // A merge commit landed "auth" and a fetch brought it, but the
        // run stopped before it cleaned up (interrupted, or a later step
        // failed). The walk from "profile" ends at "auth"'s commits,
        // which are trunk now. The bookmark on them is what is left of
        // the segment: it is checked, cleaned up, and "profile" moves on.
        let snap = merge_landed_snapshot();
        let mut forge = StubLandForge::default();
        forge.merged.insert("auth".into(), merged_pr(1, "auth"));
        forge.states.insert(
            7,
            PrLandState {
                base_branch: "auth".into(),
                head_commit: "cb1feedface".into(),
                auto_merge_allowed: false,
                ..ready_state()
            },
        );
        let prs = pr_state(
            vec![PrSummary { head_commit: "cb1feedface".into(), ..open_pr(7, "profile", "auth") }],
            false,
        );
        let plan =
            plan_land(&snap, &prs, &forge_repo(), "profile", &forge, &at_plan(), &opts()).unwrap();
        assert!(plan.blockers.is_empty(), "{:?}", plan.blockers);
        assert_eq!(
            plan.segments[0].status,
            LandSegmentStatus::Merged { number: 1, url: "https://github.com/o/r/pull/1".into() }
        );
        assert_eq!(
            plan.actions,
            vec![
                LandAction::FetchRemote { remote: "origin".into() },
                LandAction::CheckLanded {
                    number: 1,
                    bookmark: "auth".into(),
                    bottom_commit: "ca2".into(),
                    top_commit: "ca2".into(),
                },
                LandAction::RetargetPr {
                    number: 7,
                    bookmark: "profile".into(),
                    to_base: "main".into(),
                },
                // Trunk's own merge commit sits on a2 as well; only the
                // mutable child is rebased.
                LandAction::RebaseOntoTrunk {
                    root_change: "b1".into(),
                    moves: 1,
                    off: vec!["ca2".into()],
                },
                LandAction::PushStack { bookmarks: vec!["profile".into()] },
                LandAction::CleanupBookmark {
                    bookmark: "auth".into(),
                    commit: "ca2".into(),
                },
                LandAction::AbandonLanded {
                    bookmark: "auth".into(),
                    change_ids: vec!["a2".into()],
                    commits: vec!["ca2".into()],
                },
            ]
        );
        let vcs = StubVcs::new(merge_landed_snapshot());
        let outcome = execute_land(&plan, &vcs, &forge).unwrap();
        assert!(!outcome.failed, "{:?}", outcome.steps);
        // Nothing immutable was abandoned; the bookmark went.
        assert_eq!(
            vcs.calls.borrow().as_slice(),
            ["fetch:origin", "rebase:b1", "push:profile:origin", "delete:auth", "push:auth:origin"]
        );

        // When the merge that took "auth" in dropped its content, there
        // is nothing to rebase and publish again: the commits are trunk's
        // already. The plan says what happened, in the words the by-name
        // plan uses, and leaves the bookmark.
        let dropped = at_plan().saying(
            "landed1",
            LandedCheck::Missing { paths: vec!["a.txt".into()], more: 0 },
        );
        let blocked =
            plan_land(&snap, &prs, &forge_repo(), "profile", &forge, &dropped, &opts()).unwrap();
        assert!(blocked.actions.is_empty(), "{:?}", blocked.actions);
        assert!(
            blocked.blockers.iter().any(|b| {
                b.message.contains("the merge that brought them in did not keep their changes (a.txt)")
                    && b.message.contains("\u{201c}auth\u{201d} was left alone")
                    && !b.message.contains("rebase onto the fetched")
            }),
            "{:?}",
            blocked.blockers
        );

        // Seen on the sandbox (W3): once "profile" has left them, the
        // snapshot no longer draws "auth"'s commits at all, so the run
        // cannot read where the bookmark points. The backend can, and
        // the bookmark still goes.
        let undrawn = undrawn_after();
        let vcs = StubVcs::new(undrawn.clone()).knowing("a2", "ca2");
        let outcome = execute_land(&plan, &vcs, &forge).unwrap();
        assert!(!outcome.failed, "{:?}", outcome.steps);
        assert_eq!(
            vcs.calls.borrow().as_slice(),
            ["fetch:origin", "push:profile:origin", "delete:auth", "push:auth:origin"]
        );
        // Moved in the meantime, to a commit the snapshot does not draw
        // either: the backend refuses, and nothing is deleted anywhere.
        let vcs = StubVcs::new(undrawn).knowing("a2", "0ddba11");
        let outcome = execute_land(&plan, &vcs, &forge).unwrap();
        assert!(outcome.failed);
        assert!(!vcs.calls.borrow().iter().any(|c| c.starts_with("delete")));
        assert!(!vcs.calls.borrow().contains(&"push:auth:origin".to_owned()));

        // A stack whose bottom pull request already points at trunk has no
        // business with the leftover under it: it lands as usual, and the
        // bookmark waits to be asked for by name.
        let retargeted = pr_state(
            vec![PrSummary { head_commit: "cb1feedface".into(), ..open_pr(7, "profile", "main") }],
            false,
        );
        let mut ready = StubLandForge::default();
        ready.merged.insert("auth".into(), merged_pr(1, "auth"));
        ready
            .states
            .insert(7, PrLandState { head_commit: "cb1feedface".into(), ..ready_state() });
        let plan =
            plan_land(&snap, &retargeted, &forge_repo(), "profile", &ready, &at_plan(), &opts())
                .unwrap();
        assert_eq!(plan.segments.len(), 1);
        assert!(matches!(plan.actions[0], LandAction::MergePr { number: 7, .. }));
        assert!(!ready.calls.borrow().contains(&"find_merged:auth".to_owned()));

        // Asked for by name, the bookmark is a plan of its own: nothing
        // to walk, so it goes by the commit #1 was merged from.
        let alone = pr_state(vec![], false);
        let plan =
            plan_land(&snap, &alone, &forge_repo(), "auth", &forge, &at_plan(), &opts()).unwrap();
        assert!(matches!(plan.segments[0].status, LandSegmentStatus::Merged { number: 1, .. }));
        assert_eq!(
            plan.actions,
            vec![
                LandAction::FetchRemote { remote: "origin".into() },
                LandAction::CheckLanded {
                    number: 1,
                    bookmark: "auth".into(),
                    bottom_commit: "ca2feedface".into(),
                    top_commit: "ca2feedface".into(),
                },
                LandAction::CleanupBookmark {
                    bookmark: "auth".into(),
                    commit: "ca2feedface".into(),
                },
            ]
        );
        let vcs = StubVcs::new(merge_landed_snapshot());
        let outcome = execute_land(&plan, &vcs, &forge).unwrap();
        assert!(!outcome.failed, "{:?}", outcome.steps);
        assert_eq!(
            vcs.calls.borrow().as_slice(),
            ["fetch:origin", "delete:auth", "push:auth:origin"]
        );
        // With #7 still based on "auth", the by-name plan does nothing:
        // that is the stack's Land to do, with #7 moved first.
        let plan =
            plan_land(&snap, &prs, &forge_repo(), "auth", &forge, &at_plan(), &opts()).unwrap();
        assert!(plan.actions.is_empty(), "{:?}", plan.actions);
        assert!(
            plan.blockers[0].message.contains("#7 is still based on it"),
            "{:?}",
            plan.blockers
        );
        // Not drawn at all (W3, once "profile" had left it): the snapshot
        // still says the bookmark is inside trunk.
        let mut bare = undrawn_after();
        bare.bookmarks[1].is_merged = true;
        let plan =
            plan_land(&bare, &alone, &forge_repo(), "auth", &forge, &at_plan(), &opts()).unwrap();
        assert!(plan
            .actions
            .iter()
            .any(|a| matches!(a, LandAction::CleanupBookmark { .. })));

        // A bookmark on trunk history is not a stack when no merged pull
        // request was made from its commit, or when the repository does
        // not find that commit in trunk. As before.
        let mut unrelated = StubLandForge::default();
        unrelated.merged.insert(
            "auth".into(),
            PrSummary { head_commit: "0ddba11f00d".into(), ..merged_pr(1, "auth") },
        );
        // In trunk's history and dropped by the merge that brought it in:
        // said plainly, and nothing is planned.
        let dropped = at_plan().saying(
            "landed1",
            LandedCheck::Missing { paths: vec!["flag.txt".into()], more: 0 },
        );
        let plan =
            plan_land(&snap, &alone, &forge_repo(), "auth", &forge, &dropped, &opts()).unwrap();
        assert!(plan.actions.is_empty(), "{:?}", plan.actions);
        assert!(
            plan.blockers[0]
                .message
                .contains("the merge that brought them in did not keep their changes (flag.txt)"),
            "{:?}",
            plan.blockers
        );

        let elsewhere = at_plan().saying("landed1", LandedCheck::Elsewhere);
        for (forge, vcs) in [
            (&unrelated, &at_plan()),
            (&StubLandForge::default(), &at_plan()),
            (&forge, &elsewhere),
        ] {
            let err = plan_land(&snap, &alone, &forge_repo(), "auth", forge, vcs, &opts())
                .unwrap_err();
            assert!(err.to_string().contains("points at immutable history"), "{err}");
        }
    }

    #[test]
    fn landed_segments_are_swept_from_the_top_down() {
        // Seen on the sandbox (W1b): three pull requests landed in one
        // request. Abandoning the bottom segment first made jj rewrite
        // the two above it, which then no longer were the commits the
        // plan named, and they were left behind.
        let snap = stacked_snapshot();
        let forge = stacked_forge();
        let plan =
            plan_land(&snap, &stacked_prs(), &forge_repo(), "profile", &forge, &at_plan(), &opts())
                .unwrap();
        // After the fetch everything planned is still here, on the old
        // trunk commit, next to the new trunk.
        let post = snapshot(
            vec![
                node("b1", "profile: avatars", NodeKind::WorkingCopy, &["a2"], &["profile"]),
                node("a2", "auth: sessions", NodeKind::Mutable, &["a1"], &["auth"]),
                node("a1", "auth: login flow", NodeKind::Mutable, &["m"], &[]),
                node("s", "profile (squash)", NodeKind::Immutable, &["m"], &["main"]),
                node("m", "release", NodeKind::Immutable, &[], &[]),
            ],
            vec![
                bookmark("main", "s", SyncState::Synced, true),
                bookmark("auth", "a2", SyncState::Synced, false),
                bookmark("profile", "b1", SyncState::Synced, false),
            ],
        );
        let vcs = StubVcs::new(post);
        let outcome = execute_land(&plan, &vcs, &forge).unwrap();
        assert!(!outcome.failed, "{:?}", outcome.steps);
        let abandons: Vec<String> = vcs
            .calls
            .borrow()
            .iter()
            .filter(|c| c.starts_with("abandon"))
            .cloned()
            .collect();
        assert_eq!(abandons, ["abandon:cb1", "abandon:ca2,ca1"]);
        assert!(
            !outcome
                .steps
                .iter()
                .any(|s| s.detail.as_deref().is_some_and(|d| d.contains("left in place"))),
            "{:?}",
            outcome.steps
        );
        // Nothing mutable is left of the stack.
        assert!(vcs
            .snapshot()
            .unwrap()
            .nodes
            .iter()
            .all(|n| n.kind == NodeKind::Immutable));
    }

    #[test]
    fn a_branch_that_moved_on_github_is_a_blocker_a_fetch_can_clear() {
        let snap = land_snapshot();
        let prs = pr_state(vec![open_pr(1, "auth", "main")], false);
        let mut forge = StubLandForge::default();
        forge.states.insert(
            1,
            PrLandState {
                head_commit: "bd4aafaa792eafce7f2216938b97facbb08b1ecc".into(),
                ..ready_state()
            },
        );
        let plan = plan_land(&snap, &prs, &forge_repo(), "auth", &forge, &at_plan(), &opts()).unwrap();
        assert_eq!(plan.blockers.len(), 1);
        // A person is told to fetch and look; an auto-land job reads the
        // flag (see the job's own test).
        assert!(plan.blockers[0].fetch && !plan.blockers[0].wait);
        // Older persisted blockers read as before.
        let old: LandBlocker =
            serde_json::from_value(json!({ "message": "x", "wait": true })).unwrap();
        assert!(!old.fetch);
    }

    // ---- Round 3: what the second reviews found ----

    #[test]
    fn a_change_with_a_conflict_is_not_merged_enqueued_or_armed() {
        // A jj conflict pushed with git: in sync, a ready pull request,
        // and a tree GitHub reads as ordinary files.
        let mut snap = land_snapshot();
        snap.nodes[2].has_conflict = true; // a1, under "auth"
        let prs = pr_state(vec![open_pr(1, "auth", "main")], false);
        for land in [
            ready_state(),
            PrLandState {
                checks: ChecksRollup::Pending,
                merge_state: MergeStateStatus::Blocked,
                ..ready_state()
            },
            PrLandState {
                queue_on_base: true,
                queue_method: Some(MergeMethod::Squash),
                ..ready_state()
            },
        ] {
            let mut forge = StubLandForge::default();
            forge.states.insert(1, land);
            let plan =
                plan_land(&snap, &prs, &forge_repo(), "auth", &forge, &at_plan(), &opts()).unwrap();
            assert!(plan.actions.is_empty(), "{:?}", plan.actions);
            assert!(
                plan.blockers.iter().any(|b| !b.wait
                    && b.message.contains("a1 under \u{201c}auth\u{201d} has conflicts")),
                "{:?}",
                plan.blockers
            );
        }
        // Nor when someone else's branch holds the conflicted change: it
        // is immutable to jj then, and still what this pull request lands.
        let mut held = snap.clone();
        for node in held.nodes.iter_mut().filter(|n| n.id == "a1" || n.id == "a2") {
            node.kind = NodeKind::Immutable;
            node.held = true;
        }
        let mut forge = StubLandForge::default();
        forge.states.insert(1, ready_state());
        let plan =
            plan_land(&held, &prs, &forge_repo(), "auth", &forge, &at_plan(), &opts()).unwrap();
        assert!(plan.actions.is_empty(), "{:?}", plan.actions);
        assert!(
            plan.blockers
                .iter()
                .any(|b| b.message.contains("a1 under \u{201c}auth\u{201d} has conflicts")),
            "{:?}",
            plan.blockers
        );

        // In a GitHub stack a conflicted segment does not ride along.
        let mut snap = stacked_snapshot();
        snap.nodes[0].has_conflict = true; // b1, under "profile"
        let forge = stacked_forge();
        let plan =
            plan_land(&snap, &stacked_prs(), &forge_repo(), "profile", &forge, &at_plan(), &opts())
                .unwrap();
        assert!(
            matches!(&plan.actions[0], LandAction::MergePr { number: 1, with, .. } if with.is_empty()),
            "{:?}",
            plan.actions[0]
        );

        // And the next pull request up is not armed for auto-merge.
        let mut forge = StubLandForge::default();
        forge.states.insert(1, ready_state());
        forge.states.insert(7, PrLandState { base_branch: "auth".into(), ..ready_state() });
        let mut snap = land_snapshot();
        snap.bookmarks[2].sync = SyncState::Synced;
        snap.nodes[0].has_conflict = true;
        let prs = pr_state(
            vec![open_pr(1, "auth", "main"), open_pr(7, "profile", "auth")],
            false,
        );
        let plan =
            plan_land(&snap, &prs, &forge_repo(), "profile", &forge, &at_plan(), &opts()).unwrap();
        assert!(matches!(plan.actions[0], LandAction::MergePr { number: 1, .. }));
        assert!(!plan
            .actions
            .iter()
            .any(|a| matches!(a, LandAction::EnableAutoMerge { .. })));
    }

    #[test]
    fn the_pull_request_asked_for_is_looked_at_once_more_before_a_stacked_merge() {
        // The stack is dissolved after the run's first look at #7 and
        // before the request. #7's head and base have not changed, so
        // GitHub's lease would pass, and the merge would go into "auth".
        let snap = stacked_snapshot();
        let mut forge = stacked_forge();
        let plan =
            plan_land(&snap, &stacked_prs(), &forge_repo(), "profile", &forge, &at_plan(), &opts())
                .unwrap();
        assert!(matches!(&plan.actions[0], LandAction::MergePr { number: 7, with, .. } if with.len() == 1));
        forge.calls.borrow_mut().clear();
        forge.dissolved_after.insert(7, 1);
        let vcs = StubVcs::new(post_fetch_snapshot());
        let outcome = execute_land(&plan, &vcs, &forge).unwrap();
        assert!(outcome.failed);
        assert!(outcome.steps[0]
            .detail
            .as_deref()
            .unwrap()
            .contains("the GitHub stack around #7 changed"));
        assert_eq!(
            forge.calls.borrow().as_slice(),
            ["land_state:7", "land_state:1", "land_state:7"],
            "three looks and no merge request"
        );
        assert!(vcs.calls.borrow().is_empty());
    }

    #[test]
    fn a_change_started_on_a_landed_one_during_the_run_goes_onto_trunk() {
        // After the plan was made, a new change was started on "auth"'s
        // top change. Abandoning that change would rebase the new one
        // onto what was below "auth": trunk as it was before "auth"
        // landed. Its tree would lose what just landed.
        let snap = land_snapshot();
        let mut forge = StubLandForge::default();
        forge.merged.insert("auth".into(), merged_pr(1, "auth"));
        let plan = plan_land(
            &snap,
            &pr_state(vec![], false),
            &forge_repo(),
            "auth",
            &forge,
            &at_plan(),
            &opts(),
        )
        .unwrap();
        let mut post = post_fetch_snapshot();
        post.nodes[0].commit_id = "cb1".into();
        post.nodes.insert(
            0,
            node("n1", "started during the run", NodeKind::Mutable, &["a2"], &["later"]),
        );
        post.bookmarks.push(bookmark("later", "n1", SyncState::LocalOnly, false));
        let vcs = StubVcs::new(post);
        let outcome = execute_land(&plan, &vcs, &forge).unwrap();
        assert!(!outcome.failed, "{:?}", outcome.steps);
        assert_eq!(
            vcs.calls.borrow().as_slice(),
            [
                "fetch:origin",
                "rebase:b1",
                "delete:auth",
                "push:auth:origin",
                "rebase:n1",
                "abandon:ca2,ca1"
            ]
        );
        // It was moved off exactly the commits being abandoned, onto the
        // remote's trunk.
        assert_eq!(
            vcs.rebased_off.borrow().last().map(String::as_str),
            Some("n1@origin off ca2,ca1")
        );
        let after = vcs.snapshot().unwrap();
        let late = after.nodes.iter().find(|n| n.id == "n1").unwrap();
        assert_eq!(late.parents, ["s"], "on trunk, not on what was below auth");
        assert_eq!(late.commit_id, "cn1", "and not rewritten by the abandon");
        let story = outcome.steps.last().unwrap().detail.as_deref().unwrap();
        assert!(story.contains("n1 sat on them and went onto \u{201c}main\u{201d} first"), "{story}");

        // The same when someone else's branch already sits on the new
        // change: immutable to jj, and still the user's to carry along.
        let mut post = post_fetch_snapshot();
        post.nodes[0].commit_id = "cb1".into();
        // Everything under their branch is held: the landed changes too.
        for node in post.nodes.iter_mut().filter(|n| n.id == "a1" || n.id == "a2") {
            node.kind = NodeKind::Immutable;
            node.held = true;
        }
        let mut late = node("n1", "started during the run", NodeKind::Immutable, &["a2"], &["later"]);
        late.held = true;
        post.nodes.insert(0, late);
        post.bookmarks.push(bookmark("later", "n1", SyncState::Synced, false));
        let vcs = StubVcs::new(post);
        let outcome = execute_land(&plan, &vcs, &forge).unwrap();
        assert!(!outcome.failed, "{:?}", outcome.steps);
        let calls = vcs.calls.borrow();
        let at = |what: &str| calls.iter().position(|c| c == what);
        assert!(at("rebase:n1").is_some() && at("rebase:n1") < at("abandon:ca2,ca1"), "{calls:?}");
    }

    #[test]
    fn a_chain_that_merges_a_side_branch_in_names_only_the_landed_parent() {
        // b1 merges a side branch: its parents are a2 (landed) and x1.
        let mut snap = land_snapshot();
        snap.nodes[0].parents = vec!["a2".into(), "x1".into()];
        snap.nodes.insert(
            1,
            node("x1", "side: more", NodeKind::Mutable, &["m"], &[]),
        );
        let mut forge = StubLandForge::default();
        forge.merged.insert("auth".into(), merged_pr(1, "auth"));
        let plan = plan_land(
            &snap,
            &pr_state(vec![], false),
            &forge_repo(),
            "auth",
            &forge,
            &at_plan(),
            &opts(),
        )
        .unwrap();
        assert!(plan.actions.contains(&LandAction::RebaseOntoTrunk {
            root_change: "b1".into(),
            moves: 1,
            off: vec!["ca2".into()],
        }));
        let mut post = post_fetch_snapshot();
        post.nodes[0].commit_id = "cb1".into();
        let vcs = StubVcs::new(post);
        execute_land(&plan, &vcs, &forge).unwrap();
        // The backend is told which parent landed and which remote's
        // trunk takes its place; it keeps the other parent.
        assert_eq!(vcs.rebased_off.borrow().as_slice(), ["b1@origin off ca2"]);
        assert!(vcs.remotes.borrow().iter().all(|r| r == "origin"));
        assert!(!vcs.remotes.borrow().is_empty());
    }

    #[test]
    fn a_branch_other_pull_requests_are_based_on_is_not_taken_for_a_leftover() {
        // "auth" stands for a long-lived branch here: merged into trunk
        // with a merge commit, unmoved since, and a teammate's #50 is
        // based on it besides #7.
        let snap = merge_landed_snapshot();
        let mut forge = StubLandForge::default();
        forge.merged.insert("auth".into(), merged_pr(1, "auth"));
        forge.states.insert(
            7,
            PrLandState {
                base_branch: "auth".into(),
                head_commit: "cb1feedface".into(),
                ..ready_state()
            },
        );
        let mine = PrSummary { head_commit: "cb1feedface".into(), ..open_pr(7, "profile", "auth") };
        let theirs = open_pr(50, "teammate", "auth");
        let prs = pr_state(vec![mine.clone(), theirs.clone()], false);

        // Landing the stack above it does nothing about the branch. The
        // plan says what it said before: #7 targets a branch that is not
        // trunk.
        let plan =
            plan_land(&snap, &prs, &forge_repo(), "profile", &forge, &at_plan(), &opts()).unwrap();
        assert!(plan.actions.is_empty(), "{:?}", plan.actions);
        assert_eq!(plan.segments.len(), 1);
        assert!(
            plan.blockers[0].message.contains("#7 still targets \u{201c}auth\u{201d}"),
            "{:?}",
            plan.blockers
        );

        // Asked for by name, it is not deleted, and nothing is retargeted.
        for (prs, wanted) in [
            (pr_state(vec![theirs.clone()], false), "#50 is still based on it"),
            (prs, "#7 and #50 are still based on it"),
            (pr_state(vec![], true), "cannot tell whether one is based on it"),
        ] {
            let plan =
                plan_land(&snap, &prs, &forge_repo(), "auth", &forge, &at_plan(), &opts()).unwrap();
            assert!(plan.actions.is_empty(), "{:?}", plan.actions);
            assert_eq!(plan.blockers.len(), 1);
            assert!(
                !plan.blockers[0].wait && plan.blockers[0].message.contains(wanted),
                "{:?}",
                plan.blockers
            );
            assert!(execute_land(&plan, &StubVcs::new(merge_landed_snapshot()), &forge).is_err());
        }
    }

    #[test]
    fn a_bookmark_the_fetch_moved_to_the_merged_head_is_said_to_be_that() {
        // "Update branch" on GitHub added a commit to #1's branch, and #1
        // was then merged there. The plan is made from the commit that is
        // here; the run's fetch moves the bookmark to the merged head.
        let snap = land_snapshot();
        let mut forge = StubLandForge::default();
        forge.merged.insert(
            "auth".into(),
            PrSummary { head_commit: "c0ffee11".into(), ..merged_pr(1, "auth") },
        );
        let plan = plan_land(
            &snap,
            &pr_state(vec![], false),
            &forge_repo(),
            "auth",
            &forge,
            &at_plan().saying("landed1", LandedCheck::Held),
            &opts(),
        )
        .unwrap();
        let mut post = post_fetch_snapshot();
        post.nodes[0].commit_id = "cb1".into();
        post.nodes.insert(
            0,
            GraphNode {
                commit_id: "c0ffee11".into(),
                ..node("u1", "Merge main into auth", NodeKind::Mutable, &["a2", "s"], &["auth"])
            },
        );
        post.bookmarks[1].target = "u1".into();
        let vcs = StubVcs::new(post).saying("landed1", LandedCheck::Held);
        let outcome = execute_land(&plan, &vcs, &forge).unwrap();
        assert!(!outcome.failed, "{:?}", outcome.steps);
        let story = outcome
            .steps
            .iter()
            .find(|s| matches!(s.action, LandAction::CleanupBookmark { .. }))
            .and_then(|s| s.detail.as_deref())
            .unwrap();
        assert!(
            story.contains("moved to c0ffee11 with the fetch: the commit #1 was merged from")
                && story.contains("land again to clean it up"),
            "{story}"
        );
        assert!(!vcs.calls.borrow().iter().any(|c| c.starts_with("delete")));
    }

    #[test]
    fn stacked_fields_parse_from_both_queries() {
        let data = json!({
            "repository": {
                "autoMergeAllowed": false,
                "mergeQueue": null,
                "pullRequest": {
                    "id": "PR_node3",
                    "state": "MERGED",
                    "isDraft": false,
                    "mergeable": "UNKNOWN",
                    "baseRefName": "b1-two",
                    "headRefOid": "b71a5797",
                    "mergeCommit": { "oid": "665c293e" },
                    "stack": { "number": 15, "size": 3, "baseRefName": "main" },
                    "stackEntry": { "position": 3 },
                    "commits": { "nodes": [] }
                }
            }
        });
        let state = parse_pr_land_state(&data, 14).unwrap();
        assert_eq!(state.merge_commit.as_deref(), Some("665c293e"));
        assert_eq!(
            state.stack,
            Some(StackSeat { number: 15, position: 3, size: 3, base: "main".into() })
        );
        // Its own base ref still names the branch below; it landed on the
        // stack's base.
        assert_eq!(state.base_branch, "b1-two");
        assert_eq!(state.landing_branch(), "main");
    }
}
