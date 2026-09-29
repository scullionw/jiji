//! The land engine: recognize what already merged, merge the bottom PR
//! when it is ready, then reconcile the local stack — jjpr's bottom-up
//! merge → fetch → reconcile loop as an explicit plan → confirm → execute
//! flow (see the jjpr inspiration note).
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
//! idempotence rule as re-running submit — and the M5 watch loop is that
//! re-run automated. Where the repo offers landing automation (auto-merge,
//! a merge queue), the plan prefers enabling and supervising it over
//! driving the merge locally, per the product spec's background-jobs note.
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
//! Deliberately deferred: multi-merge runs, merge-queue supervision
//! (enqueue is fire-and-report), and stack-comment fossilization during
//! land (the next submit heals comments).

use jiji_core::snapshot::{GraphNode, NodeKind, RepoSnapshot, SyncState};
use jiji_core::BackendError;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use ts_rs::TS;

use crate::error::ForgeError;
use crate::pr::{ChecksRollup, PrState, PrSummary, RepoPrState, ReviewDecision};
use crate::remote::ForgeRepo;
use crate::submit::{stack_segments, SubmitStepStatus};

/// How a PR merges into its base. GitHub's three; when several are allowed
/// the plan prefers squash (jjpr's default and the stacked-PR convention),
/// then rebase, then a merge commit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
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
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "camelCase")]
#[ts(export)]
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct LandSegment {
    pub bookmark: String,
    /// Change ids in the segment, bottom-first.
    pub change_ids: Vec<String>,
    /// The open PR GitHub has for this bookmark, when one exists.
    pub pr: Option<PrSummary>,
    pub status: LandSegmentStatus,
}

/// One landing action, in execution order.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "camelCase")]
#[ts(export)]
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
    #[serde(rename_all = "camelCase")]
    MergePr {
        number: u64,
        bookmark: String,
        method: MergeMethod,
        expected_head: String,
        commit_title: Option<String>,
        commit_message: Option<String>,
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
    /// segment, with everything above it — onto the new trunk.
    #[serde(rename_all = "camelCase")]
    RebaseOntoTrunk { root_change: String, moves: u32 },
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
    /// Delete the landed bookmark here and on the remote — skipped
    /// politely when GitHub's delete-branch-on-merge already did.
    #[serde(rename_all = "camelCase")]
    CleanupBookmark { bookmark: String },
    /// Abandon the landed segment's local changes, newest-first: a squash
    /// or rebase merge rewrites them into new trunk commits, leaving the
    /// originals as redundant mutable copies. Skipped when the refreshed
    /// snapshot shows them already in trunk's ancestry (a merge-commit
    /// landing) or already gone.
    #[serde(rename_all = "camelCase")]
    AbandonLanded {
        bookmark: String,
        change_ids: Vec<String>,
    },
}

/// One reason a land plan cannot act. `wait` says whether waiting alone
/// can clear it: true for remote conditions that settle by themselves
/// (checks running, an approval pending, GitHub still computing
/// mergeability) — what an auto-land job watches through quietly — and
/// false when the user has to act first (a draft, requested changes,
/// failing checks, an unpublished stack).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct LandBlocker {
    pub message: String,
    pub wait: bool,
}

impl LandBlocker {
    pub(crate) fn needs_user(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            wait: false,
        }
    }

    pub(crate) fn transient(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            wait: true,
        }
    }
}

/// What landing a stack will do this run, derived before anything runs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct LandStep {
    pub action: LandAction,
    pub status: SubmitStepStatus,
    /// Plain-language result: what happened, or why it was skipped.
    pub detail: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
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
    /// `jj rebase -s <root> -d <trunk>` — the host resolves the trunk's
    /// change from its own refreshed snapshot.
    fn rebase_onto_trunk(&self, root_change: &str) -> Result<String, BackendError>;
    fn push_bookmarks(&self, bookmarks: &[String], remote: &str) -> Result<String, BackendError>;
    fn delete_bookmark(&self, name: &str) -> Result<String, BackendError>;
    fn abandon_changes(&self, change_ids: &[String]) -> Result<String, BackendError>;
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

/// Classify a fresh [`PrLandState`] for a PR expected to land on `trunk`.
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
    if land.base_branch != trunk {
        blockers.push(LandBlocker::needs_user(format!(
            "#{number} still targets \u{201c}{}\u{201d}, not \u{201c}{trunk}\u{201d} — \
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
pub fn plan_land(
    snapshot: &RepoSnapshot,
    prs: &RepoPrState,
    repo: &ForgeRepo,
    head_bookmark: &str,
    forge: &dyn LandForge,
    options: &LandOptions,
) -> Result<LandPlan, ForgeError> {
    let (raw_segments, mut warnings) =
        stack_segments(snapshot, prs, head_bookmark).map_err(ForgeError::Land)?;
    let trunk = snapshot.trunk_bookmark.clone();
    let mut blockers: Vec<LandBlocker> = Vec::new();
    if prs.report.truncated {
        warnings.push(
            "GitHub answered only the 100 most recently updated open PRs; a stack \
             PR past that may read as missing"
                .to_owned(),
        );
    }

    // Walk bottom-up: already-merged segments, then the landing candidate,
    // then the stacked rest.
    let mut placed: Vec<PlacedSegment> = Vec::new();
    let mut merged_top: Option<usize> = None;
    let mut candidate: Option<(usize, PrSummary, Option<PrLandState>)> = None;
    let mut no_pr_bookmark: Option<&str> = None;
    let mut bottom_resolved = true;
    for (index, raw) in raw_segments.iter().enumerate() {
        let change_ids: Vec<String> = raw.nodes.iter().map(|n| n.id.clone()).collect();
        let open_pr = prs.by_branch.get(raw.bookmark).cloned();
        if !bottom_resolved {
            placed.push(PlacedSegment {
                raw_index: index,
                bookmark: raw.bookmark,
                change_ids,
                pr: open_pr,
                status: LandSegmentStatus::Stacked,
            });
            continue;
        }
        match open_pr {
            Some(pr) => {
                bottom_resolved = false;
                if merged_top.is_some() {
                    // Reconcile-only run: this PR stays put until the
                    // stack below it is rebased, pushed, and re-checked.
                    candidate = Some((index, pr.clone(), None));
                    placed.push(PlacedSegment {
                        raw_index: index,
                        bookmark: raw.bookmark,
                        change_ids,
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
                    placed.push(PlacedSegment {
                        raw_index: index,
                        bookmark: raw.bookmark,
                        change_ids,
                        pr: None,
                        status: LandSegmentStatus::Merged {
                            number: pr.number,
                            url: pr.url.clone(),
                        },
                    });
                    merged_top = Some(index);
                    bottom_resolved = true;
                    continue;
                }
                candidate = Some((index, pr.clone(), Some(land)));
                placed.push(PlacedSegment {
                    raw_index: index,
                    bookmark: raw.bookmark,
                    change_ids,
                    pr: Some(pr),
                    // Settled below once the candidate is classified.
                    status: LandSegmentStatus::Waiting,
                });
            }
            None => match forge.find_merged_pr(raw.bookmark)? {
                Some(merged) => {
                    placed.push(PlacedSegment {
                        raw_index: index,
                        bookmark: raw.bookmark,
                        change_ids,
                        pr: None,
                        status: LandSegmentStatus::Merged {
                            number: merged.number,
                            url: merged.url.clone(),
                        },
                    });
                    merged_top = Some(index);
                }
                None => {
                    no_pr_bookmark = Some(raw.bookmark);
                    bottom_resolved = false;
                    placed.push(PlacedSegment {
                        raw_index: index,
                        bookmark: raw.bookmark,
                        change_ids,
                        pr: None,
                        status: LandSegmentStatus::Waiting,
                    });
                }
            },
        }
    }

    let mut actions: Vec<LandAction> = Vec::new();
    if merged_top.is_some() {
        // Reconcile-only run. A missing PR above the merged set does not
        // block the reconcile itself — publishing continues afterwards.
        if let Some(bookmark) = no_pr_bookmark {
            warnings.push(format!(
                "\u{201c}{bookmark}\u{201d} has no pull request yet — publish the \
                 stack once this reconcile lands"
            ));
            if let Some(p) = placed.iter_mut().find(|p| p.bookmark == bookmark) {
                p.status = LandSegmentStatus::Stacked;
            }
        }
        let landed: Vec<usize> = placed
            .iter()
            .filter(|p| matches!(p.status, LandSegmentStatus::Merged { .. }))
            .map(|p| p.raw_index)
            .collect();
        // The next PR up (the candidate this run leaves stacked) can arm
        // auto-merge once the reconcile has rebased and retargeted it —
        // GitHub then lands it as its fresh checks pass. Asked against
        // fresh land state so a repo without auto-merge, a queue-protected
        // trunk, or a draft skips the hand-off; a failed lookup just means
        // no chain, never a failed plan.
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
                })
                .and_then(|next| pick_method(&next, options.method)),
            _ => None,
        };
        actions.extend(reconcile_actions(
            snapshot,
            repo,
            &raw_segments,
            &placed,
            &landed,
            &trunk,
            None,
            chain_method,
            &mut warnings,
        ));
    } else if let Some(bookmark) = no_pr_bookmark {
        blockers.push(LandBlocker::needs_user(format!(
            "\u{201c}{bookmark}\u{201d} has no pull request — publish the stack first"
        )));
    } else if let Some((index, pr, Some(land))) = &candidate {
        // Merge run: nothing below is pending — evaluate the candidate.
        let raw = &raw_segments[*index];
        let bookmark_state = snapshot
            .bookmarks
            .iter()
            .find(|b| b.name == raw.bookmark)
            .expect("segment bookmarks come from the snapshot");
        if bookmark_state.sync != SyncState::Synced {
            blockers.push(LandBlocker::needs_user(format!(
                "\u{201c}{}\u{201d} and its GitHub branch differ — publish the stack \
                 first, so what merges is what you see here",
                raw.bookmark
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
        let readiness = classify_candidate(pr.number, &effective, &trunk, options);
        match &readiness {
            Readiness::AlreadyMerged => unreachable!("handled during the walk"),
            Readiness::Blocked(reasons) => blockers.extend(reasons.iter().cloned()),
            Readiness::AutomationRunning(story) => warnings.push(story.clone()),
            Readiness::Ready | Readiness::WaitingOnAutomatable(_) => {}
        }
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
                }
                Readiness::Ready => {
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
                    actions.push(LandAction::MergePr {
                        number: pr.number,
                        bookmark: raw.bookmark.to_owned(),
                        method,
                        expected_head: land.head_commit.clone(),
                        commit_title,
                        commit_message,
                    });
                    // The next PR up can arm auto-merge once this merge's
                    // reconcile has rebased and retargeted it.
                    let chain_method = (!options.bypass
                        && land.auto_merge_allowed
                        && !land.queue_on_base)
                        .then_some(method);
                    actions.extend(reconcile_actions(
                        snapshot,
                        repo,
                        &raw_segments,
                        &placed,
                        &[*index],
                        &trunk,
                        Some(method),
                        chain_method,
                        &mut warnings,
                    ));
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
        if let Some(p) = placed.iter_mut().find(|p| p.raw_index == *index) {
            p.status = if blockers.is_empty() {
                LandSegmentStatus::Landing
            } else {
                LandSegmentStatus::Waiting
            };
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

/// The reconcile tail shared by both plan flavors: fetch, rebase whatever
/// still sits on the landed segments onto the new trunk, push the rebased
/// live bookmarks, retarget the next PR, then clean up each landed
/// segment's bookmark and changes. `landed` holds the raw indices of the
/// landed segments; `merge_method` is known when this run performs the
/// merge — a merge-commit landing skips the abandon, since the local
/// changes become trunk ancestry — and `None` for GitHub-side merges,
/// where the executor decides from the refreshed snapshot. A
/// `chain_method` arms auto-merge on the next PR as the run's coda, so
/// GitHub lands it once its rebased checks pass.
#[allow(clippy::too_many_arguments)]
fn reconcile_actions(
    snapshot: &RepoSnapshot,
    repo: &ForgeRepo,
    raw_segments: &[crate::submit::RawSegment<'_>],
    placed: &[PlacedSegment<'_>],
    landed: &[usize],
    trunk: &str,
    merge_method: Option<MergeMethod>,
    chain_method: Option<MergeMethod>,
    warnings: &mut Vec<String>,
) -> Vec<LandAction> {
    let mut actions: Vec<LandAction> = Vec::new();
    actions.push(LandAction::FetchRemote {
        remote: repo.remote.clone(),
    });

    // Chains to rebase: every drawn child of any landed change that is
    // not itself landed. Usually exactly one — the next segment's bottom
    // change — but loose work off a mid-segment landed change gets its
    // own chain too: the abandon would otherwise reparent it onto stale
    // trunk history instead of the new trunk.
    let top = *landed.iter().max().expect("landed is never empty");
    let landed_ids: std::collections::HashSet<&str> = landed
        .iter()
        .flat_map(|i| raw_segments[*i].nodes.iter().map(|n| n.id.as_str()))
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
    for child in snapshot.nodes.iter().filter(|n| {
        !landed_ids.contains(n.id.as_str())
            && n.parents.iter().any(|p| landed_ids.contains(p.as_str()))
    }) {
        actions.push(LandAction::RebaseOntoTrunk {
            root_change: child.id.clone(),
            moves: subtree_sizes.get(child.id.as_str()).copied().unwrap_or(1),
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

    // The next open PR above the landed set points at trunk now. GitHub
    // retargets it automatically when the old base branch is deleted; the
    // executor re-checks and skips politely when that already happened.
    let next = placed
        .iter()
        .filter(|p| p.raw_index > top)
        .find(|p| p.pr.is_some());
    if let Some(next) = next {
        let pr = next.pr.as_ref().expect("next was selected for having a PR");
        if pr.base_branch != trunk {
            actions.push(LandAction::RetargetPr {
                number: pr.number,
                bookmark: pr.head_branch.clone(),
                to_base: trunk.to_owned(),
            });
        }
    }

    // Clean up each landed segment, bottom-up: the bookmark first (here
    // and on the remote), then the now-redundant local changes.
    let landed_placed: Vec<&PlacedSegment> = placed
        .iter()
        .filter(|p| landed.contains(&p.raw_index))
        .collect();
    for p in &landed_placed {
        actions.push(LandAction::CleanupBookmark {
            bookmark: p.bookmark.to_owned(),
        });
    }
    if merge_method != Some(MergeMethod::Merge) {
        for p in &landed_placed {
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
            });
        }
    }

    // The run's coda: arm auto-merge on the next PR — one level up only,
    // higher PRs still point at branches this stack rewrites — so GitHub
    // lands it as its rebased checks pass. Last on purpose: a refused
    // hand-off must not skip the cleanup above. Skipped for drafts; the
    // run that lands them promotes them deliberately.
    if let (Some(method), Some(next)) = (chain_method, next) {
        let pr = next.pr.as_ref().expect("next was selected for having a PR");
        if !pr.is_draft {
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
        if node.kind == NodeKind::Immutable {
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

/// Resolve the lease for a GitHub automation mutation. Candidate actions
/// carry the full head read at plan time. A chained auto-merge happens
/// after this same plan rewrites and pushes the next bookmark, so it
/// validates GitHub's fresh full oid against jj's resulting short commit
/// id and leases that oid in the mutation.
fn automation_head_lease(
    number: u64,
    bookmark: &str,
    planned: Option<&str>,
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
    Ok(land.head_commit.clone())
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
        } => {
            // Just-in-time re-check, jjpr's posture: never merge against
            // stale facts — judged by the same options the plan was.
            let land = forge
                .pr_land_state(*number, trunk)
                .map_err(|err| err.to_string())?;
            match classify_candidate(*number, &land, trunk, &plan.options) {
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
                    match forge.merge_pr(
                        *number,
                        *method,
                        expected_head,
                        commit_title.as_deref(),
                        commit_message.as_deref(),
                    ) {
                        Ok(()) => Ok(format!(
                            "Merged #{number} ({bookmark}) into {trunk}"
                        )),
                        Err(err) => {
                            // GitHub occasionally errors on a merge that
                            // went through; believe the PR over the error.
                            if let Ok(after) = forge.pr_land_state(*number, trunk) {
                                if after.state == PrState::Merged {
                                    return Ok(format!(
                                        "Merged #{number} ({bookmark}) into {trunk} \
                                         (GitHub reported an error, but the merge \
                                         went through)"
                                    ));
                                }
                            }
                            Err(err.to_string())
                        }
                    }
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
            if land.is_draft {
                return Ok(format!(
                    "#{number} is still a draft — auto-merge cannot arm until it is \
                     marked ready for review"
                ));
            }
            // GitHub refuses to arm auto-merge on a PR it would merge
            // outright; skip politely — the next Land run merges it.
            if land.merge_state.merges_now() {
                return Ok(format!(
                    "#{number} is already ready to merge — run Land again to merge \
                     it directly"
                ));
            }
            let expected_head = automation_head_lease(
                *number,
                bookmark,
                expected_head.as_deref(),
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
            forge
                .enqueue_pr(&land.node_id, expected_head)
                .map_err(|err| err.to_string())?;
            Ok(format!(
                "#{number} added to the merge queue — run Land again once it lands"
            ))
        }
        LandAction::FetchRemote { remote } => {
            vcs.git_fetch(remote).map_err(|err| err.to_string())
        }
        LandAction::RebaseOntoTrunk { root_change, .. } => {
            let snapshot = vcs.snapshot().map_err(|err| err.to_string())?;
            let Some(root) = snapshot.nodes.iter().find(|n| n.id == *root_change) else {
                return Ok(format!(
                    "{root_change} is already gone — nothing left to rebase"
                ));
            };
            let trunk_target = snapshot
                .bookmarks
                .iter()
                .find(|b| b.is_trunk)
                .map(|b| b.target.clone());
            if let Some(target) = trunk_target {
                if root.parents.len() == 1 && root.parents[0] == target {
                    return Ok(format!("{root_change} is already on the new trunk"));
                }
            }
            vcs.rebase_onto_trunk(root_change)
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
        } => {
            let land = forge
                .pr_land_state(*number, trunk)
                .map_err(|err| err.to_string())?;
            if land.state != PrState::Open {
                return Ok(format!(
                    "#{number} is no longer open; leaving its base alone"
                ));
            }
            if land.base_branch == *to_base {
                return Ok(format!(
                    "GitHub already retargeted #{number} onto {to_base}"
                ));
            }
            forge
                .update_pr_base(*number, to_base)
                .map_err(|err| err.to_string())?;
            Ok(format!("Retargeted #{number} onto {to_base}"))
        }
        LandAction::CleanupBookmark { bookmark } => {
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
            if was_local {
                vcs.delete_bookmark(bookmark).map_err(|err| err.to_string())?;
            }
            if had_remote {
                vcs.push_bookmarks(std::slice::from_ref(bookmark), &plan.remote)
                    .map_err(|err| err.to_string())?;
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
        } => {
            let snapshot = vcs.snapshot().map_err(|err| err.to_string())?;
            let sweep: Vec<String> = change_ids
                .iter()
                .filter(|id| {
                    snapshot
                        .nodes
                        .iter()
                        .any(|n| n.id == **id && n.kind != NodeKind::Immutable)
                })
                .cloned()
                .collect();
            if sweep.is_empty() {
                return Ok(format!(
                    "\u{201c}{bookmark}\u{201d}'s landed changes are already part of \
                     trunk's history"
                ));
            }
            vcs.abandon_changes(&sweep).map_err(|err| err.to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pr::PrStateReport;
    use crate::remote::ForgeProvider;
    use jiji_core::snapshot::{BookmarkState, GraphNode};
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
            head_commit: "feedface".into(),
            head_owner: Some("o".into()),
            base_branch: base.into(),
            body: None,
            review: ReviewDecision::None,
            checks: ChecksRollup::None,
        }
    }

    fn merged_pr(number: u64, head: &str) -> PrSummary {
        PrSummary {
            state: PrState::Merged,
            ..open_pr(number, head, "main")
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
            head_commit: "feedface".into(),
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
    }

    impl LandForge for StubLandForge {
        fn find_merged_pr(&self, branch: &str) -> Result<Option<PrSummary>, ForgeError> {
            self.calls.borrow_mut().push(format!("find_merged:{branch}"));
            Ok(self.merged.get(branch).cloned())
        }
        fn pr_land_state(&self, number: u64, _base: &str) -> Result<PrLandState, ForgeError> {
            self.calls.borrow_mut().push(format!("land_state:{number}"));
            let mut state = self
                .states
                .get(&number)
                .cloned()
                .ok_or_else(|| ForgeError::Api(format!("no stub state for #{number}")))?;
            if self.merged_despite_error && self.calls.borrow().len() > 2 {
                state.state = PrState::Merged;
            }
            if *self.promoted.borrow() {
                state.is_draft = false;
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
            self.calls
                .borrow_mut()
                .push(format!("retarget:{number}:{base}"));
            Ok(())
        }
        fn mark_ready(&self, node_id: &str) -> Result<(), ForgeError> {
            self.calls.borrow_mut().push(format!("mark_ready:{node_id}"));
            *self.promoted.borrow_mut() = true;
            Ok(())
        }
    }

    struct StubVcs {
        post: RepoSnapshot,
        calls: RefCell<Vec<String>>,
    }

    impl StubVcs {
        fn new(post: RepoSnapshot) -> Self {
            Self {
                post,
                calls: RefCell::new(vec![]),
            }
        }
    }

    impl LandVcs for StubVcs {
        fn git_fetch(&self, remote: &str) -> Result<String, BackendError> {
            self.calls.borrow_mut().push(format!("fetch:{remote}"));
            Ok("Fetched from origin".into())
        }
        fn snapshot(&self) -> Result<RepoSnapshot, BackendError> {
            Ok(self.post.clone())
        }
        fn rebase_onto_trunk(&self, root_change: &str) -> Result<String, BackendError> {
            self.calls.borrow_mut().push(format!("rebase:{root_change}"));
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
            Ok("Pushed".into())
        }
        fn delete_bookmark(&self, name: &str) -> Result<String, BackendError> {
            self.calls.borrow_mut().push(format!("delete:{name}"));
            Ok(format!("Deleted {name}"))
        }
        fn abandon_changes(&self, change_ids: &[String]) -> Result<String, BackendError> {
            self.calls
                .borrow_mut()
                .push(format!("abandon:{}", change_ids.join(",")));
            Ok("Abandoned".into())
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

        let plan = plan_land(&snap, &prs, &forge_repo(), "profile", &forge, &opts()).unwrap();
        assert!(plan.blockers.is_empty(), "{:?}", plan.blockers);
        assert_eq!(
            plan.actions,
            vec![
                LandAction::MergePr {
                    number: 1,
                    bookmark: "auth".into(),
                    method: MergeMethod::Squash,
                    expected_head: "feedface".into(),
                    // The squash commit: PR-title convention on top, the
                    // segment's change descriptions as the body.
                    commit_title: Some("PR 1 (#1)".into()),
                    commit_message: Some("auth: login flow\n\nauth: sessions".into()),
                },
                LandAction::FetchRemote { remote: "origin".into() },
                LandAction::RebaseOntoTrunk { root_change: "b1".into(), moves: 1 },
                LandAction::PushStack { bookmarks: vec!["profile".into()] },
                LandAction::RetargetPr {
                    number: 7,
                    bookmark: "profile".into(),
                    to_base: "main".into(),
                },
                LandAction::CleanupBookmark { bookmark: "auth".into() },
                LandAction::AbandonLanded {
                    bookmark: "auth".into(),
                    change_ids: vec!["a2".into(), "a1".into()],
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

    #[test]
    fn merged_bottom_plans_reconcile_only() {
        let snap = land_snapshot();
        // auth has no open PR — it merged on GitHub.
        let prs = pr_state(vec![open_pr(7, "profile", "auth")], false);
        let mut forge = StubLandForge::default();
        forge.merged.insert("auth".into(), merged_pr(1, "auth"));

        let plan = plan_land(&snap, &prs, &forge_repo(), "profile", &forge, &opts()).unwrap();
        assert!(plan.blockers.is_empty(), "{:?}", plan.blockers);
        assert_eq!(
            plan.actions,
            vec![
                LandAction::FetchRemote { remote: "origin".into() },
                LandAction::RebaseOntoTrunk { root_change: "b1".into(), moves: 1 },
                LandAction::PushStack { bookmarks: vec!["profile".into()] },
                LandAction::RetargetPr {
                    number: 7,
                    bookmark: "profile".into(),
                    to_base: "main".into(),
                },
                LandAction::CleanupBookmark { bookmark: "auth".into() },
                LandAction::AbandonLanded {
                    bookmark: "auth".into(),
                    change_ids: vec!["a2".into(), "a1".into()],
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

        let plan = plan_land(&snap, &prs, &forge_repo(), "profile", &forge, &opts()).unwrap();
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
        let plan = plan_land(&snap, &prs, &forge_repo(), "profile", &forge, &opts()).unwrap();
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

        let plan = plan_land(&snap, &prs, &forge_repo(), "profile", &forge, &opts()).unwrap();
        assert!(plan.blockers.is_empty(), "{:?}", plan.blockers);
        assert_eq!(
            plan.actions,
            vec![
                LandAction::FetchRemote { remote: "origin".into() },
                LandAction::CleanupBookmark { bookmark: "auth".into() },
                LandAction::CleanupBookmark { bookmark: "profile".into() },
                LandAction::AbandonLanded {
                    bookmark: "auth".into(),
                    change_ids: vec!["a2".into(), "a1".into()],
                },
                LandAction::AbandonLanded {
                    bookmark: "profile".into(),
                    change_ids: vec!["b1".into()],
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

        let plan = plan_land(&snap, &prs, &forge_repo(), "profile", &forge, &opts()).unwrap();
        assert!(plan.blockers.is_empty(), "{:?}", plan.blockers);
        // Reconcile still runs; no push (profile was never pushed), no
        // retarget (no PR above).
        assert_eq!(
            plan.actions,
            vec![
                LandAction::FetchRemote { remote: "origin".into() },
                LandAction::RebaseOntoTrunk { root_change: "b1".into(), moves: 1 },
                LandAction::CleanupBookmark { bookmark: "auth".into() },
                LandAction::AbandonLanded {
                    bookmark: "auth".into(),
                    change_ids: vec!["a2".into(), "a1".into()],
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

        let plan = plan_land(&snap, &prs, &forge_repo(), "profile", &forge, &opts()).unwrap();
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

        let plan = plan_land(&snap, &prs, &forge_repo(), "profile", &forge, &opts()).unwrap();
        assert!(plan.blockers.is_empty(), "{:?}", plan.blockers);
        assert_eq!(
            plan.actions,
            vec![LandAction::EnableAutoMerge {
                number: 1,
                bookmark: "auth".into(),
                method: MergeMethod::Squash,
                expected_head: Some("feedface".into()),
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
        let plan = plan_land(&snap, &prs, &forge_repo(), "profile", &forge, &opts()).unwrap();
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

        let plan = plan_land(&snap, &prs, &forge_repo(), "profile", &forge, &opts()).unwrap();
        assert_eq!(
            plan.actions,
            vec![LandAction::EnqueuePr {
                number: 1,
                bookmark: "auth".into(),
                method: MergeMethod::Squash,
                expected_head: "feedface".into(),
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
            [("NODE1".into(), "feedface".into())]
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
            let plan = plan_land(&snap, &prs, &forge_repo(), "profile", &forge, &opts()).unwrap();
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

        let plan = plan_land(&snap, &prs, &forge_repo(), "profile", &forge, &opts()).unwrap();
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
        let plan = plan_land(&snap, &prs, &forge_repo(), "profile", &forge, &opts()).unwrap();
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
                    expected_head: Some("feedface".into()),
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
        let plan = plan_land(&snap, &prs, &forge_repo(), "profile", &forge, &rebase).unwrap();
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
        let plan = plan_land(&snap, &prs, &forge_repo(), "profile", &forge, &rebase).unwrap();
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
        let plan = plan_land(&snap, &prs, &forge_repo(), "profile", &forge, &bypass).unwrap();
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
        let plan = plan_land(&snap, &prs, &forge_repo(), "profile", &forge, &bypass).unwrap();
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
                plan_land(&snap, &prs, &forge_repo(), "profile", &forge, &bypass).unwrap();
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
        let plan = plan_land(&snap, &prs, &forge_repo(), "profile", &forge, &bypass).unwrap();
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

        let plan = plan_land(&snap, &prs, &forge_repo(), "profile", &forge, &opts()).unwrap();
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
        let plan = plan_land(&snap, &prs, &forge_repo(), "profile", &forge, &opts()).unwrap();
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

        let plan = plan_land(&snap, &prs, &forge_repo(), "profile", &forge, &opts()).unwrap();
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

        let plan = plan_land(&snap, &prs, &forge_repo(), "profile", &forge, &opts()).unwrap();
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

        let plan = plan_land(&snap, &prs, &forge_repo(), "profile", &forge, &opts()).unwrap();
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

        let plan = plan_land(&snap, &prs, &forge_repo(), "profile", &forge, &opts()).unwrap();
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
        answer.nodes[0].commit_id = "feedface".into();
        answer
    }

    fn ready_plan(forge: &StubLandForge) -> LandPlan {
        let snap = land_snapshot();
        let prs = pr_state(
            vec![open_pr(1, "auth", "main"), open_pr(7, "profile", "auth")],
            false,
        );
        plan_land(&snap, &prs, &forge_repo(), "profile", forge, &opts()).unwrap()
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
                "land_state:7",
                "retarget:7:main",
                "land_state:7",
                "auto_merge:NODE1:squash",
            ]
        );
        assert_eq!(
            forge.automation_calls.borrow().as_slice(),
            [(
                "NODE1".into(),
                MergeMethod::Squash,
                "feedface".into(),
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
                "abandon:a2,a1",
            ]
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
            plan_land(&snap, &prs, &forge_repo(), "profile", &forge, &opts()).unwrap()
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
        forge.states.insert(1, ready_state());
        let plan = {
            // Plan from the merged-bottom flavor.
            let snap = land_snapshot();
            let prs = pr_state(vec![open_pr(7, "profile", "auth")], false);
            let mut planning = StubLandForge::default();
            planning.merged.insert("auth".into(), merged_pr(1, "auth"));
            plan_land(&snap, &prs, &forge_repo(), "profile", &planning, &opts()).unwrap()
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
        assert!(details.iter().any(|d| d.contains("already on the new trunk")), "{details:?}");
        assert!(details.iter().any(|d| d.contains("already retargeted")), "{details:?}");
        assert!(details.iter().any(|d| d.contains("already gone")), "{details:?}");
        assert!(
            details.iter().any(|d| d.contains("already part of trunk")),
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
        let plan = plan_land(&snap, &prs, &forge_repo(), "profile", &forge, &opts()).unwrap();
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
}
