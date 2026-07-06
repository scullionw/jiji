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
//! Deliberately deferred: multi-merge runs, draft→ready promotion,
//! merge-queue supervision (enqueue is fire-and-report), stack-comment
//! fossilization during land (the next submit heals comments), and a
//! per-repo merge-method setting (squash is preferred like jjpr, falling
//! back to whatever the repo allows).

use jiji_core::snapshot::{NodeKind, RepoSnapshot, SyncState};
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
    /// Merge the PR now. `expected_head` rides along as GitHub's own
    /// lease — the merge refuses if the PR's head moved since planning.
    #[serde(rename_all = "camelCase")]
    MergePr {
        number: u64,
        bookmark: String,
        method: MergeMethod,
        expected_head: String,
    },
    /// The PR is waiting only on conditions GitHub automates on (checks,
    /// required reviews): enable auto-merge and let GitHub finish. The
    /// reconcile happens on a later Land run, once the merge is real.
    #[serde(rename_all = "camelCase")]
    EnableAutoMerge {
        number: u64,
        bookmark: String,
        method: MergeMethod,
    },
    /// The base branch is protected by a merge queue — the only way to
    /// land is to enqueue and let GitHub drive.
    #[serde(rename_all = "camelCase")]
    EnqueuePr { number: u64, bookmark: String },
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
    /// Abandon the landed segment's local changes: a squash or rebase
    /// merge rewrites them into new trunk commits, leaving the originals
    /// as redundant mutable copies. Skipped when the refreshed snapshot
    /// shows them already in trunk's ancestry (a merge-commit landing) or
    /// already gone.
    #[serde(rename_all = "camelCase")]
    AbandonLanded {
        bookmark: String,
        /// Newest-first, the order the sweep is applied in.
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
    ) -> Result<(), ForgeError>;
    fn enable_auto_merge(&self, node_id: &str, method: MergeMethod) -> Result<(), ForgeError>;
    fn enqueue_pr(&self, node_id: &str) -> Result<(), ForgeError>;
    fn update_pr_base(&self, number: u64, base: &str) -> Result<(), ForgeError>;
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
    ) -> Result<(), ForgeError> {
        self.client
            .merge_pr(&self.repo.owner, &self.repo.name, number, method, expected_head)
    }

    fn enable_auto_merge(&self, node_id: &str, method: MergeMethod) -> Result<(), ForgeError> {
        self.client.enable_auto_merge(node_id, method)
    }

    fn enqueue_pr(&self, node_id: &str) -> Result<(), ForgeError> {
        self.client.enqueue_pr(node_id)
    }

    fn update_pr_base(&self, number: u64, base: &str) -> Result<(), ForgeError> {
        self.client
            .update_pr_base(&self.repo.owner, &self.repo.name, number, base)
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
pub(crate) fn classify_candidate(number: u64, land: &PrLandState, trunk: &str) -> Readiness {
    match land.state {
        PrState::Merged => return Readiness::AlreadyMerged,
        PrState::Closed => {
            return Readiness::Blocked(vec![LandBlocker::needs_user(format!(
                "#{number} was closed on GitHub without merging — reopen it or publish again"
            ))])
        }
        PrState::Open => {}
    }
    if land.in_merge_queue {
        return Readiness::AutomationRunning(format!(
            "#{number} is already in the merge queue; GitHub lands it from here — \
             run Land again afterwards to reconcile"
        ));
    }
    if land.auto_merge_enabled {
        return Readiness::AutomationRunning(format!(
            "auto-merge is already enabled on #{number}; GitHub merges it once its \
             requirements are met — run Land again afterwards to reconcile"
        ));
    }
    let mut blockers: Vec<LandBlocker> = Vec::new();
    if land.is_draft {
        blockers.push(LandBlocker::needs_user(format!(
            "#{number} is still a draft — mark it ready for review on GitHub first"
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
    if land.review == ReviewDecision::ChangesRequested {
        blockers.push(LandBlocker::needs_user(format!(
            "changes were requested on #{number}"
        )));
    }
    if land.checks == ChecksRollup::Failing {
        blockers.push(LandBlocker::needs_user(format!(
            "#{number}'s checks are failing"
        )));
    }
    if !blockers.is_empty() {
        return Readiness::Blocked(blockers);
    }
    // What remains are the wait-states GitHub's own automation exists for.
    let mut waits: Vec<String> = Vec::new();
    if land.checks == ChecksRollup::Pending {
        waits.push(format!("#{number}'s checks are still running"));
    }
    if land.review == ReviewDecision::ReviewRequired {
        waits.push(format!("#{number} still needs an approving review"));
    }
    if waits.is_empty() {
        Readiness::Ready
    } else {
        Readiness::WaitingOnAutomatable(waits)
    }
}

/// The merge method for a repo's capabilities: squash preferred (jjpr's
/// default and the stacked-PR convention), then rebase, then merge.
pub(crate) fn pick_method(land: &PrLandState) -> Option<MergeMethod> {
    if land.allows_squash {
        Some(MergeMethod::Squash)
    } else if land.allows_rebase {
        Some(MergeMethod::Rebase)
    } else if land.allows_merge {
        Some(MergeMethod::Merge)
    } else {
        None
    }
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
        actions.extend(reconcile_actions(
            snapshot,
            repo,
            &raw_segments,
            &placed,
            &landed,
            &trunk,
            None,
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
        let method = pick_method(land);
        if method.is_none() {
            blockers.push(LandBlocker::needs_user(format!(
                "{}/{} allows none of GitHub's merge methods — check the \
                 repository's merge settings",
                repo.owner, repo.name
            )));
        }
        let readiness = classify_candidate(pr.number, land, &trunk);
        match &readiness {
            Readiness::AlreadyMerged => unreachable!("handled during the walk"),
            Readiness::Blocked(reasons) => blockers.extend(reasons.iter().cloned()),
            Readiness::AutomationRunning(story) => warnings.push(story.clone()),
            Readiness::Ready | Readiness::WaitingOnAutomatable(_) => {}
        }
        if blockers.is_empty() {
            let method = method.expect("a missing method is a blocker");
            match &readiness {
                Readiness::Ready | Readiness::WaitingOnAutomatable(_)
                    if land.queue_on_base =>
                {
                    // A queue-protected trunk: enqueue and let GitHub
                    // drive, whatever the readiness.
                    actions.push(LandAction::EnqueuePr {
                        number: pr.number,
                        bookmark: raw.bookmark.to_owned(),
                    });
                    warnings.push(format!(
                        "\u{201c}{trunk}\u{201d} is protected by a merge queue; GitHub \
                         lands #{} from here — run Land again afterwards to reconcile",
                        pr.number
                    ));
                }
                Readiness::Ready => {
                    actions.push(LandAction::MergePr {
                        number: pr.number,
                        bookmark: raw.bookmark.to_owned(),
                        method,
                        expected_head: land.head_commit.clone(),
                    });
                    actions.extend(reconcile_actions(
                        snapshot,
                        repo,
                        &raw_segments,
                        &placed,
                        &[*index],
                        &trunk,
                        Some(method),
                        &mut warnings,
                    ));
                }
                Readiness::WaitingOnAutomatable(waits) => {
                    if land.auto_merge_allowed {
                        actions.push(LandAction::EnableAutoMerge {
                            number: pr.number,
                            bookmark: raw.bookmark.to_owned(),
                            method,
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
/// where the executor decides from the refreshed snapshot.
#[allow(clippy::too_many_arguments)]
fn reconcile_actions(
    snapshot: &RepoSnapshot,
    repo: &ForgeRepo,
    raw_segments: &[crate::submit::RawSegment<'_>],
    placed: &[PlacedSegment<'_>],
    landed: &[usize],
    trunk: &str,
    merge_method: Option<MergeMethod>,
    warnings: &mut Vec<String>,
) -> Vec<LandAction> {
    let mut actions: Vec<LandAction> = Vec::new();
    actions.push(LandAction::FetchRemote {
        remote: repo.remote.clone(),
    });

    // Chains to rebase: every drawn child of the topmost landed change.
    // Usually exactly one — the next segment's bottom change or loose
    // local work — but a fan-out above the landed head rebases per child.
    let top = *landed.iter().max().expect("landed is never empty");
    let landed_head = raw_segments[top]
        .nodes
        .last()
        .expect("segments are never empty");
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
    for child in snapshot
        .nodes
        .iter()
        .filter(|n| n.parents.contains(&landed_head.id))
    {
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
    if let Some(next) = placed
        .iter()
        .filter(|p| p.raw_index > top)
        .find_map(|p| p.pr.as_ref())
    {
        if next.base_branch != trunk {
            actions.push(LandAction::RetargetPr {
                number: next.number,
                bookmark: next.head_branch.clone(),
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
            if p.change_ids.iter().any(|id| *id == snapshot.working_copy) {
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
        LandAction::MergePr {
            number,
            bookmark,
            method,
            expected_head,
        } => {
            // Just-in-time re-check, jjpr's posture: never merge against
            // stale facts.
            let land = forge
                .pr_land_state(*number, trunk)
                .map_err(|err| err.to_string())?;
            match classify_candidate(*number, &land, trunk) {
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
                        return Err(format!(
                            "#{number}'s branch moved since this plan was made — \
                             review the fresh plan and land again"
                        ));
                    }
                    match forge.merge_pr(*number, *method, expected_head) {
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
        LandAction::EnableAutoMerge { number, method, .. } => {
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
            forge
                .enable_auto_merge(&land.node_id, *method)
                .map_err(|err| err.to_string())?;
            Ok(format!(
                "Auto-merge enabled on #{number}; GitHub merges it once its \
                 requirements are met — run Land again afterwards to reconcile"
            ))
        }
        LandAction::EnqueuePr { number, .. } => {
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
            forge
                .enqueue_pr(&land.node_id)
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
            review: ReviewDecision::Approved,
            checks: ChecksRollup::Passing,
            base_branch: "main".into(),
            head_commit: "feedface".into(),
            auto_merge_enabled: false,
            in_merge_queue: false,
            auto_merge_allowed: true,
            queue_on_base: false,
            allows_squash: true,
            allows_merge: true,
            allows_rebase: true,
        }
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

    #[derive(Default)]
    struct StubLandForge {
        merged: HashMap<String, PrSummary>,
        states: HashMap<u64, PrLandState>,
        merge_fails: bool,
        merged_despite_error: bool,
        calls: RefCell<Vec<String>>,
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
            Ok(state)
        }
        fn merge_pr(
            &self,
            number: u64,
            method: MergeMethod,
            _expected_head: &str,
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
        fn enable_auto_merge(&self, node_id: &str, method: MergeMethod) -> Result<(), ForgeError> {
            self.calls
                .borrow_mut()
                .push(format!("auto_merge:{node_id}:{}", method.rest_name()));
            Ok(())
        }
        fn enqueue_pr(&self, node_id: &str) -> Result<(), ForgeError> {
            self.calls.borrow_mut().push(format!("enqueue:{node_id}"));
            Ok(())
        }
        fn update_pr_base(&self, number: u64, base: &str) -> Result<(), ForgeError> {
            self.calls
                .borrow_mut()
                .push(format!("retarget:{number}:{base}"));
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

        let plan = plan_land(&snap, &prs, &forge_repo(), "profile", &forge).unwrap();
        assert!(plan.blockers.is_empty(), "{:?}", plan.blockers);
        assert_eq!(
            plan.actions,
            vec![
                LandAction::MergePr {
                    number: 1,
                    bookmark: "auth".into(),
                    method: MergeMethod::Squash,
                    expected_head: "feedface".into(),
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

        let plan = plan_land(&snap, &prs, &forge_repo(), "profile", &forge).unwrap();
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
        // stale pre-rebase diff.
        assert_eq!(plan.segments[1].status, LandSegmentStatus::Stacked);
        assert_eq!(forge.calls.borrow().as_slice(), ["find_merged:auth"]);
    }

    #[test]
    fn whole_stack_merged_plans_cleanup_only() {
        let snap = land_snapshot();
        let prs = pr_state(vec![], false);
        let mut forge = StubLandForge::default();
        forge.merged.insert("auth".into(), merged_pr(1, "auth"));
        forge.merged.insert("profile".into(), merged_pr(7, "profile"));

        let plan = plan_land(&snap, &prs, &forge_repo(), "profile", &forge).unwrap();
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

        let plan = plan_land(&snap, &prs, &forge_repo(), "profile", &forge).unwrap();
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

        let plan = plan_land(&snap, &prs, &forge_repo(), "profile", &forge).unwrap();
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

        let plan = plan_land(&snap, &prs, &forge_repo(), "profile", &forge).unwrap();
        assert!(plan.blockers.is_empty(), "{:?}", plan.blockers);
        assert_eq!(
            plan.actions,
            vec![LandAction::EnableAutoMerge {
                number: 1,
                bookmark: "auth".into(),
                method: MergeMethod::Squash,
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
        let plan = plan_land(&snap, &prs, &forge_repo(), "profile", &forge).unwrap();
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
                ..ready_state()
            },
        );

        let plan = plan_land(&snap, &prs, &forge_repo(), "profile", &forge).unwrap();
        assert_eq!(
            plan.actions,
            vec![LandAction::EnqueuePr { number: 1, bookmark: "auth".into() }]
        );
        assert!(
            plan.warnings.iter().any(|w| w.contains("merge queue")),
            "{:?}",
            plan.warnings
        );
    }

    #[test]
    fn blocked_states_plan_nothing_and_say_why() {
        let snap = land_snapshot();
        let prs = pr_state(
            vec![open_pr(1, "auth", "main"), open_pr(7, "profile", "auth")],
            false,
        );
        // The `wait` flag marks the one condition GitHub clears by itself
        // (mergeability still computing); everything else needs the user.
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
                PrLandState { is_draft: true, ..ready_state() },
                "still a draft",
                false,
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
        ] {
            let mut forge = StubLandForge::default();
            forge.states.insert(1, state);
            let plan = plan_land(&snap, &prs, &forge_repo(), "profile", &forge).unwrap();
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
    fn out_of_sync_bookmark_blocks_the_merge() {
        let mut snap = land_snapshot();
        snap.bookmarks[1].sync = SyncState::Ahead;
        let prs = pr_state(
            vec![open_pr(1, "auth", "main"), open_pr(7, "profile", "auth")],
            false,
        );
        let mut forge = StubLandForge::default();
        forge.states.insert(1, ready_state());

        let plan = plan_land(&snap, &prs, &forge_repo(), "profile", &forge).unwrap();
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

        let plan = plan_land(&snap, &prs, &forge_repo(), "profile", &forge).unwrap();
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

        let plan = plan_land(&snap, &prs, &forge_repo(), "profile", &forge).unwrap();
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
        snapshot(
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
        )
    }

    fn ready_plan(forge: &StubLandForge) -> LandPlan {
        let snap = land_snapshot();
        let prs = pr_state(
            vec![open_pr(1, "auth", "main"), open_pr(7, "profile", "auth")],
            false,
        );
        plan_land(&snap, &prs, &forge_repo(), "profile", forge).unwrap()
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
            ]
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
    fn execute_adapts_when_github_already_cleaned_up() {
        let mut forge = StubLandForge::default();
        forge.states.insert(1, ready_state());
        let plan = {
            // Plan from the merged-bottom flavor.
            let snap = land_snapshot();
            let prs = pr_state(vec![open_pr(7, "profile", "auth")], false);
            let mut planning = StubLandForge::default();
            planning.merged.insert("auth".into(), merged_pr(1, "auth"));
            plan_land(&snap, &prs, &forge_repo(), "profile", &planning).unwrap()
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
        let mut forge = forge;
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
        let plan = plan_land(&snap, &prs, &forge_repo(), "profile", &forge).unwrap();
        assert!(!plan.blockers.is_empty());
        let vcs = StubVcs::new(land_snapshot());
        let err = execute_land(&plan, &vcs, &forge).unwrap_err();
        assert!(err.to_string().contains("blockers"));
    }

    #[test]
    fn parse_pr_land_state_reads_the_full_shape() {
        let data = json!({
            "repository": {
                "autoMergeAllowed": true,
                "squashMergeAllowed": true,
                "mergeCommitAllowed": false,
                "rebaseMergeAllowed": false,
                "mergeQueue": { "id": "Q_1" },
                "pullRequest": {
                    "id": "PR_node1",
                    "state": "OPEN",
                    "isDraft": false,
                    "mergeable": "MERGEABLE",
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
        assert_eq!(state.review, ReviewDecision::Approved);
        assert_eq!(state.checks, ChecksRollup::Passing);
        assert_eq!(state.base_branch, "main");
        assert_eq!(state.head_commit, "abc123");
        assert!(!state.auto_merge_enabled);
        assert!(!state.in_merge_queue);
        assert!(state.auto_merge_allowed);
        assert!(state.queue_on_base);
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
        assert_eq!(state.review, ReviewDecision::None);
        assert_eq!(state.checks, ChecksRollup::None);
        assert!(state.auto_merge_enabled);
        assert!(!state.queue_on_base);

        // A missing PR is the not-found story, not a parse error.
        let data = json!({ "repository": { "pullRequest": null } });
        assert!(matches!(
            parse_pr_land_state(&data, 9).unwrap_err(),
            ForgeError::NotFound(_)
        ));
    }
}
