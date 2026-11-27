//! The submit engine: analyze a bookmark's stack from the repo snapshot,
//! plan the minimum set of remote actions, then execute the plan.
//!
//! The shape is jjpr's: every submission
//! derives an explicit plan first — which bookmarks push, which PRs open
//! and against which bases, which existing PRs retarget — and execution
//! walks that plan. That is what makes submit idempotent: re-running
//! against an already-consistent stack plans nothing, and "the stack is up
//! to date" is an empty action list, not a special case. The plan is a
//! plain DTO, so the Publish section renders exactly what will run and
//! the CLI prints the same object.
//!
//! Re-submitting also reconciles what GitHub shows: existing PR titles and
//! descriptions update from the commits through the fingerprint machinery
//! in [`crate::reconcile`] (hand edits are detected and respected, never
//! clobbered), and every PR in a multi-PR stack carries the stack-info
//! comment from [`crate::comment`], edited in place on later submits.
//!
//! New PRs open ready for review unless the plan is switched to drafts
//! ([`SubmitPlan::open_new_prs_as_drafts`]); an existing PR's draft flag
//! is never changed here — [`crate::readiness`] owns that.
//!
//! Two things about GitHub shape the order a plan runs in. GitHub closes
//! a pull request as merged the moment its head is reachable from its
//! base branch, and a reorder pushed with the old bases in place does
//! exactly that to the pull request that moved down: the branch above it
//! is still its base and now contains it. So every pull request whose
//! base changes points at trunk before the push, and at its new base
//! after. And a pull request in a GitHub stack cannot be retargeted at
//! all, nor a stack reordered: the stack is dissolved first and linked
//! again at the end.
//!
//! A chain becomes a GitHub stack only when asked ([`StackLink::Link`]);
//! once it is one, by Jiji or by anyone on github.com, every later plan
//! reads that from GitHub and keeps the stack in step with the local
//! chain without being asked.
//!
//! Deliberately left out: foreign bases
//! (a coworker's branch in the stack's ancestry — the snapshot does not
//! carry remote-only bookmarks on nodes yet), and
//! recognizing already-merged PRs (needs a per-bookmark merged-PR query;
//! the land flow owns it).

use jiji_core::snapshot::{GraphNode, RepoSnapshot, SyncState, TakenOver};
use jiji_core::BackendError;
use serde::{Deserialize, Serialize};

use crate::comment::{inherit_fossils, parse_stack_data, render_stack_comment, StackEntry};
use crate::error::ForgeError;
use crate::github::GitHubClient;
use crate::pr::{PrSummary, RepoPrState};
use crate::reconcile::{plan_pr_text, TextWarning};
use crate::remote::ForgeRepo;
use crate::template::{new_pr_body, PrTemplate};

/// What a submit plan starts from: a stack's top bookmark — or, for a
/// stack whose head carries no bookmark yet, the head change itself, with
/// the branch name derived from the work ([`crate::naming`]) and the
/// bookmark created by the plan on confirm. jjpr has no second form (it
/// submits only up to the topmost existing bookmark); Jiji adds it so
/// inventing a bookmark by hand stops being part of the publish flow.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum SubmitHead {
    /// Publish the stack under this local non-trunk bookmark.
    #[serde(rename_all = "camelCase")]
    Bookmark { name: String },
    /// Publish the stack under this change (a snapshot node id). When the
    /// change turns out to carry a bookmark, this is the bookmark form;
    /// otherwise the top segment gets a derived name.
    #[serde(rename_all = "camelCase")]
    Change { change: String },
}

/// What to do about GitHub's own stacked pull requests when publishing.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum StackLink {
    /// Leave it as GitHub has it: a chain that is a GitHub stack stays
    /// one and follows the local order; a plain chain stays plain.
    #[default]
    Follow,
    /// Link the stack's pull requests as a GitHub stack.
    Link,
    /// Dissolve the GitHub stack; the pull requests stay, as a plain chain.
    Unlink,
    /// What [`StackLink::Follow`] becomes under the setting that links new
    /// stacks without being asked: a chain that is not a GitHub stack is
    /// linked by the publish that opens a pull request for it, and is
    /// otherwise left as it is. Where the repository has no GitHub
    /// stacks, the pull requests stay a plain chain and the publish does
    /// not fail for it.
    LinkNew,
}

/// How the user asked this publish to run. The options ride on the plan,
/// so the re-derivation before execution judges by the same ones.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubmitOptions {
    #[serde(default)]
    pub stack: StackLink,
}

impl SubmitOptions {
    /// These options under the setting that links new stacks as GitHub
    /// stacks. A choice made for this publish (to link, or to dissolve)
    /// stands either way; only a publish that was not told anything
    /// follows the setting.
    pub fn under_setting(mut self, link_new_stacks: bool) -> Self {
        if link_new_stacks && self.stack == StackLink::Follow {
            self.stack = StackLink::LinkNew;
        }
        self
    }
}

/// One publishable run of changes under a bookmark, listed bottom-up in
/// [`SubmitPlan::segments`]. Mirrors jjpr's segment: the commits between
/// the bookmark below (or trunk) and this segment's bookmark.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
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
    /// Set when this segment's branch was someone else's and was taken
    /// over here ([`crate::takeover`]): for which pull request, and whose.
    /// A plan that pushes it says so.
    #[serde(default)]
    pub taken_over: Option<TakenOver>,
}

/// One remote action the plan will run, in execution order.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum SubmitAction {
    /// Create a local bookmark at an unbookmarked stack head, its name
    /// derived from the segment's bottom change description
    /// ([`crate::naming`]). Runs before every push — the branch has to
    /// exist to push — through the same mutation plumbing as the
    /// workbench's own bookmark action.
    #[serde(rename_all = "camelCase")]
    CreateBookmark { bookmark: String, change: String },
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
        /// Open it as a draft, which requests no reviews until marked ready.
        draft: bool,
    },
    /// Point an existing PR at the base the stack shape expects.
    #[serde(rename_all = "camelCase")]
    RetargetPr {
        number: u64,
        bookmark: String,
        from_base: String,
        to_base: String,
    },
    /// Rewrite an existing PR's Jiji-managed text from the commits. Hand
    /// edits survive: the managed description section is fingerprinted
    /// (`crate::reconcile`), so only text Jiji provably wrote is replaced,
    /// and user prose outside the sentinels is preserved verbatim.
    #[serde(rename_all = "camelCase")]
    UpdatePrText {
        number: u64,
        bookmark: String,
        /// The title to set; `None` leaves the PR's title alone.
        title: Option<String>,
        /// The full replacement body, managed section and fingerprints
        /// rewritten, user text carried over.
        body: String,
        /// True when nothing visible changes — the write only records
        /// Jiji's fingerprints on a PR it recognizes as its own.
        seed: bool,
    },
    /// Take the unmerged pull requests out of GitHub stack `stack`. They
    /// keep their bases and go back to being a plain chain. GitHub has no
    /// way to reorder a stack, insert into one, or retarget a pull request
    /// inside one, so a chain that changed shape leaves its stack first
    /// and is linked again by a `LinkStack` at the end of the same plan.
    /// `members` are the open pull requests in the stack when the plan was
    /// made, bottom first: everyone the dissolve takes out, whoever opened
    /// them. A plan confirmed in the app is refused when they have changed
    /// by the time it runs.
    #[serde(rename_all = "camelCase")]
    Unstack {
        stack: u64,
        #[serde(default)]
        members: Vec<u64>,
    },
    /// Link pull requests as a GitHub stack: all of them as a new stack,
    /// or with `onto` the listed ones on top of that existing stack.
    /// Bottom first, by bookmark; the numbers of pull requests this plan
    /// creates are only known once it runs. `above` are pull requests
    /// that sat above the published head in a stack this plan dissolves
    /// and still chain on top of it: they go back into the new stack, on
    /// top, so a publish of part of a chain does not drop the rest of it
    /// from GitHub's stack.
    #[serde(rename_all = "camelCase")]
    LinkStack {
        bookmarks: Vec<String>,
        #[serde(default)]
        onto: Option<u64>,
        #[serde(default)]
        above: Vec<u64>,
    },
    /// Post or refresh the stack-info comment that explains the stack to
    /// GitHub readers.
    #[serde(rename_all = "camelCase")]
    SyncStackComment {
        bookmark: String,
        /// The PR's number; `None` when the PR is created earlier in this
        /// same plan and the number is not known yet.
        number: Option<u64>,
        /// True posts a new comment; false edits the existing one.
        create: bool,
    },
}

/// What submitting a stack will do, derived before anything runs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubmitPlan {
    /// The bookmark the plan publishes up to: the stack's top bookmark,
    /// or — planning from an unbookmarked head — the derived name the
    /// plan's `CreateBookmark` action brings into being.
    pub head_bookmark: String,
    /// Git remote the pushes go to — the detected forge repo's remote.
    pub remote: String,
    /// The trunk branch the bottom segment's PR targets.
    pub base_branch: String,
    /// The options the plan was derived under.
    #[serde(default)]
    pub options: SubmitOptions,
    /// The stack's publishable segments, bottom-up.
    pub segments: Vec<SubmitSegment>,
    /// Everything that will run, in order. Empty means up to date.
    pub actions: Vec<SubmitAction>,
    /// Problems that stop the plan from running at all (undescribed or
    /// conflicted commits in a pushed segment, a conflicted bookmark).
    pub blockers: Vec<String>,
    /// Worth knowing, but the plan still runs.
    pub warnings: Vec<String>,
    /// The stack-info comment as it will read, rendered for the panel
    /// when the plan syncs comments (PRs created by this plan appear as
    /// plain names — their links exist only after execution).
    pub stack_comment_preview: Option<String>,
    /// Where the repo's PR template lives, when trunk carries one and this
    /// plan creates a PR whose body folds it in — so the panel can say new
    /// descriptions start from it.
    pub pr_template_path: Option<String>,
}

impl SubmitPlan {
    /// Open the PRs this plan creates as drafts. PRs that already exist
    /// keep their state; the plan says so for any open for review.
    pub fn open_new_prs_as_drafts(&mut self) {
        for action in &mut self.actions {
            if let SubmitAction::CreatePr { draft, .. } = action {
                *draft = true;
            }
        }
        for segment in &self.segments {
            if let Some(pr) = segment.pr.as_ref().filter(|pr| !pr.is_draft) {
                self.warnings.push(format!(
                    "#{} ({}) is already open for review and stays that way",
                    pr.number, segment.bookmark
                ));
            }
        }
    }
}

/// Per-action result of executing a plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SubmitStepStatus {
    Done,
    Failed,
    /// Not attempted because an earlier step failed (or a blocker stopped
    /// the plan).
    Skipped,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubmitStep {
    pub action: SubmitAction,
    pub status: SubmitStepStatus,
    /// Plain-language result: the push summary, or the failure message.
    pub detail: Option<String>,
    /// The PR a `CreatePr` step opened, for linking out.
    pub pr: Option<PrSummary>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
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
    /// `jj bookmark create <name> -r <change>` for a derived head
    /// bookmark, run before the pushes so the branch exists to push; the
    /// returned string is the outcome summary.
    fn create_bookmark(&self, name: &str, change: &str) -> Result<String, BackendError>;
}

/// A PR's existing Jiji stack comment, found by its sentinel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExistingComment {
    pub id: u64,
    pub body: String,
}

/// Where planning reads existing stack comments from. Split out of
/// [`SubmitForge`] because planning only reads — a stub answering `None`
/// keeps plan tests network-free.
pub trait StackCommentSource {
    fn stack_comment(&self, number: u64) -> Result<Option<ExistingComment>, ForgeError>;

    /// Every open pull request whose base is `base`. The planner asks
    /// this only when the batched open-PR list was cut short, for each
    /// branch of the stack it publishes: a pull request based on a
    /// pushed branch can be closed by the push, so none of them may be
    /// missing from what the plan is made of. A source that knows no
    /// more than the list answers nothing.
    fn open_prs_based_on(&self, _base: &str) -> Result<Vec<PrSummary>, ForgeError> {
        Ok(Vec::new())
    }
}

/// The forge side of executing a plan; implemented by [`RepoForge`] over
/// the real client, a stub in tests.
pub trait SubmitForge: StackCommentSource {
    fn create_pr(
        &self,
        title: &str,
        body: &str,
        head: &str,
        base: &str,
        draft: bool,
    ) -> Result<PrSummary, ForgeError>;
    fn update_pr_base(&self, number: u64, base: &str) -> Result<(), ForgeError>;
    fn update_pr_text(
        &self,
        number: u64,
        title: Option<&str>,
        body: &str,
    ) -> Result<(), ForgeError>;
    fn create_comment(&self, number: u64, body: &str) -> Result<(), ForgeError>;
    fn update_comment(&self, comment_id: u64, body: &str) -> Result<(), ForgeError>;
    /// Link pull requests, bottom first, as a new GitHub stack; answers
    /// the stack's number.
    fn create_stack(&self, pull_requests: &[u64]) -> Result<u64, ForgeError>;
    /// Put pull requests, bottom first, on top of an existing GitHub stack.
    fn add_to_stack(&self, stack: u64, pull_requests: &[u64]) -> Result<(), ForgeError>;
    /// Take the unmerged pull requests out of a GitHub stack.
    /// Answers the open pull requests GitHub left in the stack (it
    /// documents keeping one that is queued to merge).
    fn unstack(&self, stack: u64) -> Result<Vec<u64>, ForgeError>;
}

/// [`SubmitForge`] over the real GitHub client, bound to a detected repo.
pub struct RepoForge<'a> {
    pub client: &'a GitHubClient,
    pub repo: &'a ForgeRepo,
}

impl StackCommentSource for RepoForge<'_> {
    fn open_prs_based_on(&self, base: &str) -> Result<Vec<PrSummary>, ForgeError> {
        self.client
            .open_prs_based_on(&self.repo.owner, &self.repo.name, base)
    }

    fn stack_comment(&self, number: u64) -> Result<Option<ExistingComment>, ForgeError> {
        let comments = self
            .client
            .list_comments(&self.repo.owner, &self.repo.name, number)?;
        Ok(comments
            .into_iter()
            .find(|(_, body)| crate::comment::is_stack_comment(body))
            .map(|(id, body)| ExistingComment { id, body }))
    }
}

impl SubmitForge for RepoForge<'_> {
    fn create_pr(
        &self,
        title: &str,
        body: &str,
        head: &str,
        base: &str,
        draft: bool,
    ) -> Result<PrSummary, ForgeError> {
        self.client.create_pr(
            &self.repo.owner,
            &self.repo.name,
            title,
            body,
            head,
            base,
            draft,
        )
    }

    fn update_pr_base(&self, number: u64, base: &str) -> Result<(), ForgeError> {
        self.client
            .update_pr_base(&self.repo.owner, &self.repo.name, number, base)
    }

    fn update_pr_text(
        &self,
        number: u64,
        title: Option<&str>,
        body: &str,
    ) -> Result<(), ForgeError> {
        self.client
            .update_pr_text(&self.repo.owner, &self.repo.name, number, title, body)
    }

    fn create_comment(&self, number: u64, body: &str) -> Result<(), ForgeError> {
        self.client
            .create_comment(&self.repo.owner, &self.repo.name, number, body)
    }

    fn update_comment(&self, comment_id: u64, body: &str) -> Result<(), ForgeError> {
        self.client
            .update_comment(&self.repo.owner, &self.repo.name, comment_id, body)
    }

    fn create_stack(&self, pull_requests: &[u64]) -> Result<u64, ForgeError> {
        self.client
            .create_stack(&self.repo.owner, &self.repo.name, pull_requests)
            .map_err(|err| no_stacks_here(err, self.repo))
    }

    fn add_to_stack(&self, stack: u64, pull_requests: &[u64]) -> Result<(), ForgeError> {
        self.client
            .add_to_stack(&self.repo.owner, &self.repo.name, stack, pull_requests)
            .map_err(|err| no_stacks_here(err, self.repo))
    }

    fn unstack(&self, stack: u64) -> Result<Vec<u64>, ForgeError> {
        self.client
            .unstack(&self.repo.owner, &self.repo.name, stack)
            .map_err(|err| no_stacks_here(err, self.repo))
    }
}

/// The stacks endpoints answer a bare 404 on a host or repository that
/// does not have GitHub's stacked pull requests; say that instead.
fn no_stacks_here(err: ForgeError, repo: &ForgeRepo) -> ForgeError {
    match err {
        ForgeError::NotFound(_) => ForgeError::NoStacks(format!(
            "{}/{} on {} does not offer GitHub's stacked pull requests",
            repo.owner, repo.name, repo.host
        )),
        other => other,
    }
}

/// One bookmark-bounded run of the stack, bottom-up — the shape submit and
/// land planning share.
pub(crate) struct RawSegment<'a> {
    pub bookmark: &'a str,
    pub nodes: Vec<&'a GraphNode>,
}

/// Walk down first parents from `top` to the settled base, answered
/// bottom-up ending at `top`. Empty when `top` itself is settled history.
/// A change someone else's branch holds is immutable to jj and still part
/// of the chain: it is the user's, published or to be published, and a
/// walk that stopped at it would take it for trunk.
fn mutable_chain<'a>(
    node_by_id: &std::collections::HashMap<&str, &'a GraphNode>,
    top: &'a GraphNode,
) -> Vec<&'a GraphNode> {
    let mut chain: Vec<&GraphNode> = Vec::new();
    let mut cursor = Some(top);
    while let Some(node) = cursor {
        if node.is_settled() {
            break;
        }
        chain.push(node);
        cursor = node
            .parents
            .first()
            .and_then(|id| node_by_id.get(id.as_str()).copied());
    }
    chain.reverse();
    chain
}

/// Cut a bottom-up chain at bookmarked changes. Every local non-trunk
/// bookmark ends a segment; several on one change collapse into one
/// segment published through one of them (jjpr collapses the same way),
/// reported in the returned warnings. Changes above the last bookmarked
/// one come back as the unbookmarked top run — the derived-name flow's
/// input; a walk that starts at a bookmark never has one.
fn cut_at_bookmarks<'a>(
    chain: &[&'a GraphNode],
    snapshot: &'a RepoSnapshot,
    prs: &RepoPrState,
    head_bookmark: Option<&str>,
) -> (Vec<RawSegment<'a>>, Vec<&'a GraphNode>, Vec<String>) {
    let mut warnings: Vec<String> = Vec::new();
    let local_bookmarks: std::collections::HashSet<&str> = snapshot
        .bookmarks
        .iter()
        .filter(|b| b.is_local && !b.is_trunk)
        .map(|b| b.name.as_str())
        .collect();
    let mut raw_segments: Vec<RawSegment> = Vec::new();
    let mut pending: Vec<&'a GraphNode> = Vec::new();
    for node in chain {
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
                Some(*name) != head_bookmark,
                !prs.by_branch.contains_key(*name),
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
            nodes: std::mem::take(&mut pending),
        });
    }
    (raw_segments, pending, warnings)
}

/// Walk the snapshot graph from `head_bookmark` down first parents to the
/// immutable base and cut the mutable chain at local non-trunk bookmarks.
/// Errors are plain messages for each caller to wrap in its own
/// `ForgeError` flavor.
pub(crate) fn stack_segments<'a>(
    snapshot: &'a RepoSnapshot,
    prs: &RepoPrState,
    head_bookmark: &str,
) -> Result<(Vec<RawSegment<'a>>, Vec<String>), String> {
    let bookmark = snapshot
        .bookmarks
        .iter()
        .find(|b| b.name == head_bookmark && b.is_local)
        .ok_or_else(|| format!("there is no local bookmark named \u{201c}{head_bookmark}\u{201d}"))?;
    if bookmark.is_trunk {
        return Err(format!(
            "\u{201c}{head_bookmark}\u{201d} is the trunk — it is what stacks land on, \
             not a stack of its own"
        ));
    }

    let node_by_id: std::collections::HashMap<&str, &GraphNode> =
        snapshot.nodes.iter().map(|n| (n.id.as_str(), n)).collect();
    let chain = node_by_id
        .get(bookmark.target.as_str())
        .map(|top| mutable_chain(&node_by_id, top))
        .unwrap_or_default();
    if chain.is_empty() {
        return Err(format!(
            "\u{201c}{head_bookmark}\u{201d} points at immutable history — everything \
             under it is already part of trunk"
        ));
    }

    let (raw_segments, pending, warnings) =
        cut_at_bookmarks(&chain, snapshot, prs, Some(head_bookmark));
    // The walk ends at the head bookmark's change, which closes the last
    // segment; anything left over would mean the bookmark vanished.
    debug_assert!(pending.is_empty());
    Ok((raw_segments, warnings))
}

/// One segment as the planner iterates it: a name to publish through,
/// the changes it covers, and — for the derived top segment of an
/// unbookmarked head — the change the new bookmark is created at.
struct PlannedSegment<'a> {
    name: String,
    nodes: Vec<&'a GraphNode>,
    create_at: Option<String>,
}

/// Resolve a submit head into bottom-up planned segments. A bookmark head
/// is the classic walk; a change head walks from that node, and when
/// changes remain above the last bookmarked one, they become the derived
/// top segment — named from its bottom change's description, stepping
/// around every name the repo already uses (local bookmarks, the
/// snapshot's remote bookmarks, open-PR head branches).
fn planned_segments<'a>(
    snapshot: &'a RepoSnapshot,
    prs: &RepoPrState,
    head: &SubmitHead,
) -> Result<(Vec<PlannedSegment<'a>>, Vec<String>), ForgeError> {
    let from_raw = |raw: Vec<RawSegment<'a>>| {
        raw.into_iter()
            .map(|r| PlannedSegment {
                name: r.bookmark.to_owned(),
                nodes: r.nodes,
                create_at: None,
            })
            .collect::<Vec<_>>()
    };
    match head {
        SubmitHead::Bookmark { name } => {
            let (raw, warnings) =
                stack_segments(snapshot, prs, name).map_err(ForgeError::Plan)?;
            Ok((from_raw(raw), warnings))
        }
        SubmitHead::Change { change } => {
            let node_by_id: std::collections::HashMap<&str, &GraphNode> =
                snapshot.nodes.iter().map(|n| (n.id.as_str(), n)).collect();
            let top = node_by_id.get(change.as_str()).copied().ok_or_else(|| {
                ForgeError::Plan(format!(
                    "no change {change} in this snapshot — refresh and pick again"
                ))
            })?;
            if top.is_settled() {
                return Err(ForgeError::Plan(format!(
                    "{change} is immutable history — it is already part of trunk"
                )));
            }
            let chain = mutable_chain(&node_by_id, top);
            let (raw, leftover, warnings) = cut_at_bookmarks(&chain, snapshot, prs, None);
            let mut planned = from_raw(raw);
            if !leftover.is_empty() {
                let bottom = leftover.first().expect("leftover is never empty here");
                let head_node = leftover.last().expect("leftover is never empty here");
                let derived = crate::naming::derive_branch_name(
                    &bottom.description,
                    &bottom.change_id,
                    snapshot
                        .bookmarks
                        .iter()
                        .map(|b| b.name.as_str())
                        .chain(prs.by_branch.keys().map(String::as_str)),
                );
                planned.push(PlannedSegment {
                    name: derived,
                    create_at: Some(head_node.id.clone()),
                    nodes: leftover,
                });
            }
            Ok((planned, warnings))
        }
    }
}

/// Build the submission plan for the stack under `head`: walk the
/// snapshot graph from the head down first parents to the immutable
/// base, segment the mutable chain at local non-trunk bookmarks, and
/// compare each segment against the forge's open-PR state — including each
/// existing PR's title/description text and its stack comment, so the plan
/// enumerates every remote write before anything runs and an up-to-date
/// stack still plans nothing.
///
/// A [`SubmitHead::Change`] head whose change carries no bookmark gets
/// one derived from the segment's bottom change description
/// ([`crate::naming`]) and a `CreateBookmark` action ahead of the pushes
/// — nothing is named until the plan is confirmed.
pub fn plan_submit(
    snapshot: &RepoSnapshot,
    prs: &RepoPrState,
    repo: &ForgeRepo,
    head: &SubmitHead,
    comments: &dyn StackCommentSource,
    pr_template: Option<&PrTemplate>,
    options: &SubmitOptions,
) -> Result<SubmitPlan, ForgeError> {
    // GitHub's batched list holds at most the hundred most recently
    // updated open pull requests. One it left out can still be closed by
    // this publish, and that takes its base being a branch the publish
    // pushes: a branch of this stack. So when the list was cut short,
    // GitHub is asked for the pull requests based on each of them, and
    // the plan is made of the list with those added. Added before
    // anything is planned: a pull request of the stack itself that the
    // list left out has to be known as that segment's, or the plan would
    // push around it and try to open it a second time.
    let completed = if prs.report.truncated {
        let (first_look, _) = planned_segments(snapshot, prs, head)?;
        let mut report = prs.report.clone();
        for seg in first_look.iter().filter(|seg| seg.create_at.is_none()) {
            for pr in comments.open_prs_based_on(&seg.name)? {
                if !report.prs.iter().any(|known| known.number == pr.number) {
                    report.prs.push(pr);
                }
            }
        }
        Some(RepoPrState::new(report, &repo.owner))
    } else {
        None
    };
    let prs = completed.as_ref().unwrap_or(prs);
    let (planned, mut warnings) = planned_segments(snapshot, prs, head)?;
    let head_bookmark = planned
        .last()
        .map(|seg| seg.name.clone())
        .expect("a resolved head always yields at least one segment");
    let mut blockers: Vec<String> = Vec::new();

    // Compare each segment with the forge state, bottom-up, tracking the
    // effective base like jjpr: trunk first, then each live segment's
    // bookmark.
    let mut segments: Vec<SubmitSegment> = Vec::new();
    let mut bookmark_creates: Vec<SubmitAction> = Vec::new();
    let mut pushes: Vec<SubmitAction> = Vec::new();
    let mut creates: Vec<SubmitAction> = Vec::new();
    // Each existing PR whose base must change: (number, bookmark, the
    // base GitHub has, the base the stack shape expects).
    let mut rebases: Vec<(u64, String, String, String)> = Vec::new();
    let mut text_actions: Vec<SubmitAction> = Vec::new();
    // Segments that will have a PR once the plan runs (existing or being
    // created) — the stack the comment describes. Bottom-up.
    let mut live: Vec<(String, Option<PrSummary>)> = Vec::new();
    let mut effective_base = snapshot.trunk_bookmark.clone();
    if prs.report.truncated {
        warnings.push(
            "GitHub answered only the 100 most recently updated open PRs; an existing \
             PR past that may be missed and recreated"
                .to_owned(),
        );
    }
    for seg in &planned {
        let name = seg.name.as_str();
        // The derived top segment has no bookmark yet — it reads as
        // local-only: always pushed, always a branch creation.
        let state = match &seg.create_at {
            Some(_) => None,
            None => Some(
                snapshot
                    .bookmarks
                    .iter()
                    .find(|b| b.name == name)
                    .expect("segment bookmarks come from the snapshot"),
            ),
        };
        let bottom = seg.nodes.first().expect("segments are never empty");
        let (title, body) = derive_title_body(&bottom.description, name);
        let pr = prs.by_branch.get(name).cloned();
        let taken_over = crate::takeover::taken_over(snapshot, name, pr.as_ref());
        let segment = SubmitSegment {
            bookmark: name.to_owned(),
            base: effective_base.clone(),
            change_ids: seg.nodes.iter().map(|n| n.id.clone()).collect(),
            title: title.clone(),
            pr: pr.clone(),
            taken_over: taken_over.clone(),
        };

        // An all-empty segment pushes nothing: an empty diff would make
        // GitHub auto-close the PR (jjpr's rule). It still becomes the
        // base below the next segment — and a derived bookmark for it is
        // never created (nothing would publish under it).
        if seg.nodes.iter().all(|n| n.is_empty) {
            warnings.push(if seg.create_at.is_some() {
                format!(
                    "every change under \u{201c}{name}\u{201d} is empty; skipping its \
                     bookmark, push, and PR"
                )
            } else {
                format!(
                    "every change under \u{201c}{name}\u{201d} is empty; skipping its \
                     push and PR"
                )
            });
            effective_base = name.to_owned();
            segments.push(segment);
            continue;
        }

        let needs_push = state.is_none_or(|s| s.sync != SyncState::Synced || s.remote.is_none());
        if needs_push {
            if state.is_some()
                && snapshot
                    .conflicts
                    .iter()
                    .any(|c| c.id == format!("bookmark-{name}"))
            {
                // For a branch that was taken over, a conflict is what a
                // fetch leaves when its author pushed to it again.
                blockers.push(match &taken_over {
                    Some(entry) => crate::takeover::conflicted_story(entry),
                    None => format!(
                        "bookmark \u{201c}{name}\u{201d} is conflicted; repoint it before \
                         publishing"
                    ),
                });
            }
            // Only commits that would land on the remote need to be
            // presentable — the same checks the push itself enforces,
            // surfaced at plan time so the panel can say so.
            for node in &seg.nodes {
                let change = &node.change_id;
                if node.description.is_empty() {
                    blockers.push(format!("{change} has no description; describe it first"));
                }
                if node.has_conflict {
                    blockers.push(format!("{change} has conflicts; resolve them first"));
                }
                // Not when the other copies are only what someone else's
                // branch still has of an earlier version: nothing here
                // can abandon those, and pushing this one is how the
                // change moves on. The warning about their pull request
                // says the rest.
                if node.diverged_here() {
                    blockers.push(format!(
                        "{change} is divergent; resolve the divergence first"
                    ));
                }
            }
            match state.map(|s| s.sync) {
                Some(SyncState::Behind) => warnings.push(format!(
                    "\u{201c}{name}\u{201d} is behind its remote; the push moves the \
                     remote branch backwards"
                )),
                Some(SyncState::Diverged) => warnings.push(format!(
                    "\u{201c}{name}\u{201d} and its remote have diverged; the push \
                     replaces the remote position"
                )),
                _ => {}
            }
            if let Some(change) = &seg.create_at {
                bookmark_creates.push(SubmitAction::CreateBookmark {
                    bookmark: name.to_owned(),
                    change: change.clone(),
                });
            }
            pushes.push(SubmitAction::Push {
                bookmark: name.to_owned(),
                create: state.is_none_or(|s| s.sync == SyncState::LocalOnly),
            });
        }

        match &pr {
            Some(pr) => {
                if pr.base_branch != effective_base {
                    rebases.push((
                        pr.number,
                        name.to_owned(),
                        pr.base_branch.clone(),
                        effective_base.clone(),
                    ));
                }

                // Reconcile the PR's title and description against the
                // commit. Skipped for an undescribed bottom change — its
                // expected title is only the bookmark-name fallback, not
                // something to correct the PR toward.
                if !bottom.description.is_empty() {
                    let text = plan_pr_text(
                        &pr.title,
                        pr.body.as_deref().unwrap_or(""),
                        &title,
                        &body,
                    );
                    if let Some(new_body) = text.body.clone() {
                        text_actions.push(SubmitAction::UpdatePrText {
                            number: pr.number,
                            bookmark: name.to_owned(),
                            title: text.title.clone(),
                            body: new_body,
                            seed: text.seed,
                        });
                    }
                    for warning in &text.warnings {
                        let story = match warning {
                            TextWarning::BodyConflict { unfingerprinted: false } => format!(
                                "#{} ({name}): the PR description and the commit description \
                                 both changed since Jiji last wrote it; leaving the PR text alone",
                                pr.number
                            ),
                            TextWarning::BodyConflict { unfingerprinted: true } => format!(
                                "#{} ({name}): the PR description was edited on GitHub (or \
                                 predates Jiji's tracking); leaving it alone",
                                pr.number
                            ),
                            TextWarning::TitleConflict => format!(
                                "#{} ({name}): the PR title and the commit's first line both \
                                 changed; leaving the title alone",
                                pr.number
                            ),
                            // Ownership unknown: warn only for one-change
                            // segments — multi-change PRs often carry a
                            // hand-curated title (jjpr's heuristic).
                            TextWarning::TitleDrift => {
                                if seg.nodes.len() != 1 {
                                    continue;
                                }
                                format!(
                                    "#{} ({name}): the PR title (\u{201c}{}\u{201d}) differs \
                                     from the commit (\u{201c}{title}\u{201d}) and Jiji does \
                                     not know which is intended",
                                    pr.number, pr.title
                                )
                            }
                        };
                        warnings.push(story);
                    }
                }
            }
            None => {
                creates.push(SubmitAction::CreatePr {
                    bookmark: name.to_owned(),
                    base: effective_base.clone(),
                    title: title.clone(),
                    // Sentinel-wrapped and fingerprinted from birth, so
                    // later submits can update it without guessing; the
                    // repo's PR template (when trunk carries one) rides
                    // below the managed section as user-space text.
                    body: new_pr_body(&body, &title, pr_template),
                    draft: false,
                });
            }
        }

        live.push((name.to_owned(), pr.clone()));
        effective_base = name.to_owned();
        segments.push(segment);
    }
    // Retargets, around the push. GitHub closes a pull request as merged
    // the moment its head is reachable from its base branch, whatever
    // that branch is. A reorder pushed with the old bases in place does
    // that to the pull request that moved down: the branch above it is
    // still its base, and after the push contains it. So a pull request
    // whose base changes points at trunk while branches move, where
    // nothing this plan pushes can ever be reachable, and at its new base
    // once they have moved.
    let trunk = snapshot.trunk_bookmark.as_str();
    let pushing = !pushes.is_empty();

    // The same rule catches a pull request whose base stays the same.
    // Slide a chain's bookmarks up by one commit and the lower branch is
    // pushed onto the commit the upper pull request still has as its head
    // on GitHub. Whether that pull request survives then hangs on its own
    // branch arriving in the same push, and jj's push is not atomic. So:
    // which pushed branch will contain which pull request's head?
    let pushed: std::collections::HashSet<&str> = pushes
        .iter()
        .filter_map(|action| match action {
            SubmitAction::Push { bookmark, .. } => Some(bookmark.as_str()),
            _ => None,
        })
        .collect();
    // A push of a branch that was taken over moves a branch someone else
    // made. Said by pull request and author, so that the user sees whose
    // branch is about to move.
    let theirs: Vec<&TakenOver> = segments
        .iter()
        .filter(|segment| pushed.contains(segment.bookmark.as_str()))
        .filter_map(|segment| segment.taken_over.as_ref())
        .collect();
    if let Some(note) = crate::takeover::pushes_their_branch(&theirs) {
        warnings.push(note);
    }
    // Where each pushed branch will stand, and what it will then contain:
    // everything reachable from that commit, along every parent. (A merge
    // can bring a pull request's head in as its second parent.)
    let by_id: std::collections::HashMap<&str, &GraphNode> =
        snapshot.nodes.iter().map(|n| (n.id.as_str(), n)).collect();
    let pushed_to = |branch: &str| -> Option<&GraphNode> {
        pushed.contains(branch).then(|| {
            planned
                .iter()
                .find(|seg| seg.name == branch)
                .and_then(|seg| seg.nodes.last().copied())
        })?
    };
    // Whether `commit`, as GitHub names it, is `from` or below it here,
    // as far as the snapshot draws. Immutable commits count like any
    // other: a pull request's head can be one (a tagged commit is).
    let contains = |from: &GraphNode, commit: &str| -> bool {
        let mut queue = vec![from];
        let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
        while let Some(node) = queue.pop() {
            if !seen.insert(node.id.as_str()) {
                continue;
            }
            if crate::land::same_commit(commit, &node.commit_id) {
                return true;
            }
            queue.extend(node.parents.iter().filter_map(|p| by_id.get(p.as_str()).copied()));
        }
        false
    };
    // Every open pull request a pushed branch can affect: the list, made
    // whole above when GitHub cut it short.
    let open: &[PrSummary] = &prs.report.prs;
    // Only a pull request of this repository's own branch is ever matched
    // to a segment or to a branch this publish pushes: a fork's branch
    // may have the name of a bookmark here, and names nothing here.
    let of_this_repository = |pr: &PrSummary| prs.of_this_repository(pr);
    // A push of several branches can go through for some and not for
    // others, so a pull request is at risk when any outcome puts its head
    // inside its base: its base pushed onto the head GitHub has for it
    // (bookmarks that slid up; its own branch may not follow), or its own
    // branch pushed to a commit its base already has on GitHub (bookmarks
    // that slid down; its base may not follow).
    let at_risk = |pr: &PrSummary| -> bool {
        let under_new_base = pushed_to(&pr.base_branch)
            .is_some_and(|base| contains(base, &pr.head_commit));
        // (Its own branch is pushed from here only when it is this
        // repository's: a fork's branch of the same name is not.)
        let own_head = of_this_repository(pr)
            .then(|| pushed_to(&pr.head_branch))
            .flatten();
        let in_old_base = own_head.is_some_and(|head| {
            match prs.by_branch.get(&pr.base_branch) {
                Some(below) => snapshot
                    .nodes
                    .iter()
                    .find(|n| crate::land::same_commit(&below.head_commit, &n.commit_id))
                    .is_some_and(|base| contains(base, &head.commit_id)),
                // Where a branch stands on GitHub is known here through
                // its own pull request. A base that has none, and that
                // this publish moves, could be anywhere: at risk.
                None => pushed.contains(pr.base_branch.as_str()),
            }
        });
        under_new_base || in_old_base
    };
    // A base that reads in sync stands, here, where the last fetch left
    // it; where it stands on GitHub is its own pull request's head. When
    // the two differ, the branch moved on GitHub since, to commits that
    // are not here, and nothing above can tell whether a branch pushed
    // against it ends up inside it. That is not parked around: it is
    // fetched and looked at first. (In sync, and not merely "not pushed":
    // a base whose changes all came out empty is not pushed either, and
    // has moved here.)
    for (name, pr) in live
        .iter()
        .filter_map(|(name, pr)| pr.as_ref().map(|pr| (name, pr)))
    {
        if !pushed.contains(name.as_str()) || pushed.contains(pr.base_branch.as_str()) {
            continue;
        }
        let here = snapshot
            .bookmarks
            .iter()
            .find(|b| b.name == pr.base_branch && b.is_local && b.sync == SyncState::Synced)
            .and_then(|b| by_id.get(b.target.as_str()));
        let (Some(below), Some(here)) = (prs.by_branch.get(&pr.base_branch), here) else {
            continue;
        };
        if !crate::land::same_commit(&below.head_commit, &here.commit_id) {
            blockers.push(format!(
                "\u{201c}{}\u{201d} is at {} on GitHub and at {} here: something pushed to it \
                 since the last fetch. Pushing \u{201c}{name}\u{201d} against a base that moved \
                 could put #{} inside it, and GitHub would close #{} as merged \u{2014} fetch, \
                 look at what arrived, and publish again",
                pr.base_branch,
                &below.head_commit[..below.head_commit.len().min(here.commit_id.len().max(8))],
                here.commit_id,
                pr.number,
                pr.number,
            ));
        }
    }
    // In the chain being published it is parked like a base change, and
    // put back on the same base afterwards.
    let slid: Vec<(u64, String, String)> = live
        .iter()
        .filter_map(|(name, pr)| pr.as_ref().map(|pr| (name, pr)))
        .filter(|(_, pr)| !rebases.iter().any(|r| r.0 == pr.number) && at_risk(pr))
        .map(|(name, pr)| (pr.number, name.clone(), pr.base_branch.clone()))
        .collect();
    // Outside it, nothing this plan does can move the pull request's own
    // branch, so the push would close it. Refuse, and say which publish
    // would not.
    // Someone else's pull requests on this stack: based on one of its
    // branches, with a branch that is no bookmark here. A publish never
    // pushes or retargets them. What it does under them is said, by
    // number and author, before it runs.
    let own_branches: Vec<&str> = planned.iter().map(|seg| seg.name.as_str()).collect();
    let others = crate::pr::others_on(snapshot, prs, &own_branches);
    let is_other = |pr: &PrSummary| others.iter().any(|other| other.number == pr.number);
    for pr in open {
        if of_this_repository(pr) && planned.iter().any(|seg| seg.name == pr.head_branch) {
            continue;
        }
        if at_risk(pr) && is_other(pr) {
            // Their branch is not here to be moved along, so there is no
            // publish that keeps the pull request open: the only way out
            // is not to put its commits under its base.
            // Taken over, its branch is a bookmark here and the publish
            // goes through it. A fork's branch cannot be taken over.
            let or_take_over = if prs.of_this_repository(pr) {
                format!(
                    ", or take that pull request over first (take-over {}) and publish \
                     through it",
                    pr.number
                )
            } else {
                String::new()
            };
            blockers.push(format!(
                "publishing through \u{201c}{head_bookmark}\u{201d} would push \
                 \u{201c}{}\u{201d} onto the commits of {} (\u{201c}{}\u{201d}), which is \
                 based on it, and GitHub would close {} as merged into \u{201c}{}\u{201d}. That \
                 pull request is someone else's: its branch is not a bookmark here, and no \
                 publish from here can move it out of the way. Move \u{201c}{}\u{201d} back off \
                 those commits{or_take_over}",
                pr.base_branch,
                pr.by_author(),
                pr.head_branch,
                pr.by_author(),
                pr.base_branch,
                pr.base_branch,
            ));
        } else if at_risk(pr) {
            blockers.push(format!(
                "publishing through \u{201c}{head_bookmark}\u{201d} would push \
                 \u{201c}{}\u{201d} onto the commits of #{} (\u{201c}{}\u{201d}), which is \
                 based on it, and GitHub would close #{} as merged — publish through \
                 \u{201c}{}\u{201d} instead, so its own branch moves too",
                pr.base_branch, pr.number, pr.head_branch, pr.number, pr.head_branch
            ));
        }
    }
    // A push that replaces commits of a branch (a rewrite, a reorder, a
    // bookmark moved back) leaves a pull request based on that branch
    // with the replaced commits in its own branch and nowhere else: it
    // shows them as its own until its branch is rebased. A push that
    // only adds commits on top does not.
    let replaces = |branch: &str| {
        pushed.contains(branch)
            && snapshot.bookmarks.iter().any(|b| {
                b.name == branch
                    && b.is_local
                    && matches!(b.sync, SyncState::Diverged | SyncState::Behind)
            })
    };
    for other in &others {
        // The branch of the stack it rests on, through any of the others
        // it is based on.
        // (Through this repository's branches only, and never twice
        // through the same one: a pull request from a fork can have the
        // branch name of its own base.)
        let mut base = other.base_branch.as_str();
        let mut through: Vec<&str> = Vec::new();
        while let Some(below) = others
            .iter()
            .find(|o| of_this_repository(o) && o.head_branch == base)
        {
            if through.contains(&base) {
                break;
            }
            through.push(base);
            base = below.base_branch.as_str();
        }
        if replaces(base) {
            warnings.push(format!(
                "{} (\u{201c}{}\u{201d}) is someone else's pull request on \u{201c}{base}\u{201d}, \
                 and this push replaces commits under it: it shows the replaced commits as \
                 its own until its branch is rebased. Nothing here pushes to \
                 \u{201c}{}\u{201d}: rebasing it is its author's to do",
                other.by_author(),
                other.head_branch,
                other.head_branch
            ));
        }
    }

    // GitHub's own stack, when this chain is (or is to become) one. It
    // refuses a retarget inside a stack, so every pull request that is
    // retargeted at all counts as moving.
    let moving: Vec<u64> = rebases
        .iter()
        .map(|r| r.0)
        .chain(slid.iter().map(|s| s.0))
        .collect();
    // What sits on top of the chain as it will stand: on its top segment
    // that has, or gets, a pull request.
    let on_top = live
        .last()
        .map(|(top, _)| on_top_of(snapshot, prs, top))
        .unwrap_or_default();
    let (unstacks, link) = plan_stack_link(
        &live,
        &moving,
        &prs.report.prs,
        &on_top,
        options.stack,
        !creates.is_empty(),
        &mut warnings,
    );
    // Dissolving a stack to link it again takes knowing all of it. With
    // the list cut short, a member can be missing even after the lookups
    // above (the bottom one is based on trunk, and one past the published
    // head is based on a branch this publish does not touch). The plan
    // would then dissolve the stack over a pull request it believes it
    // has to open, or link it again without one it never saw. Asked for
    // outright (`--no-stack`), a dissolve takes the same knowledge for
    // another reason: every member leaves the stack with it, and one the
    // list does not show may be someone else's pull request, which the
    // plan could not name.
    if prs.report.truncated {
        for action in &unstacks {
            let SubmitAction::Unstack { stack, .. } = action else {
                continue;
            };
            if !sees_whole_stack(&prs.report.prs, *stack) {
                let seen: Vec<u64> = prs
                    .report
                    .prs
                    .iter()
                    .filter(|pr| pr.stack.as_ref().is_some_and(|s| s.number == *stack))
                    .map(|pr| pr.number)
                    .collect();
                blockers.push(format!(
                    "GitHub answered only the 100 most recently updated open pull requests, \
                     and they are not all of GitHub stack {stack}: Jiji sees only {}. It does \
                     not dissolve a stack it cannot see whole, because it could not {}. A \
                     comment on the pull requests it does not see brings them into the list \
                     \u{2014} then publish again",
                    crate::land::number_list(seen.into_iter()),
                    if options.stack == StackLink::Unlink {
                        "say whose pull requests leave it"
                    } else {
                        "put it back together"
                    }
                ));
            }
        }
    }

    let mut parked: Vec<SubmitAction> = Vec::new();
    let mut retargets: Vec<SubmitAction> = Vec::new();
    for (number, bookmark, base) in slid {
        parked.push(SubmitAction::RetargetPr {
            number,
            bookmark: bookmark.clone(),
            from_base: base.clone(),
            to_base: trunk.to_owned(),
        });
        retargets.push(SubmitAction::RetargetPr {
            number,
            bookmark,
            from_base: trunk.to_owned(),
            to_base: base,
        });
    }
    for (number, bookmark, from_base, to_base) in rebases {
        if !pushing || from_base == trunk {
            retargets.push(SubmitAction::RetargetPr {
                number,
                bookmark,
                from_base,
                to_base,
            });
            continue;
        }
        parked.push(SubmitAction::RetargetPr {
            number,
            bookmark: bookmark.clone(),
            from_base,
            to_base: trunk.to_owned(),
        });
        if to_base != trunk {
            retargets.push(SubmitAction::RetargetPr {
                number,
                bookmark,
                from_base: trunk.to_owned(),
                to_base,
            });
        }
    }
    if parked
        .iter()
        .any(|a| retargets.iter().any(|b| retarget_number(a) == retarget_number(b)))
    {
        warnings.push(format!(
            "pull requests that this push would put inside their own base branch point \
             at \u{201c}{trunk}\u{201d} while the branches are pushed, and at their bases \
             again afterwards — GitHub closes a pull request as merged the moment its \
             branch turns up inside its base"
        ));
    }

    // Bookmark creations first (a derived branch must exist locally to
    // push); then the stack is dissolved if it has to change shape (GitHub
    // refuses to retarget inside one); then the parking retargets, the
    // pushes (a new PR's head and base branches must exist), PR creations
    // bottom-up, the final retargets, text updates, the stack comments
    // (they mention the PRs everything above sets up), and last the
    // linking as a GitHub stack, which needs every PR to exist with its
    // final base.
    let creating = !creates.is_empty();
    let mut actions: Vec<SubmitAction> = bookmark_creates;
    actions.extend(unstacks);
    actions.extend(parked);
    actions.extend(pushes);
    actions.extend(creates);
    actions.extend(retargets);
    actions.extend(text_actions);

    // The stack comment: every PR in a multi-PR stack carries one. When no
    // PR is being created every link is already known, so the exact bodies
    // compare against the existing comments and an unchanged stack plans
    // nothing; pending creations make the contents unknowable until
    // execution, so every live PR syncs. A lone PR never gets a first
    // comment ("part of a stack" on a single PR is noise) but an existing
    // one keeps updating — jjpr's rule.
    let entries: Vec<StackEntry> = live
        .iter()
        .map(|(name, pr)| StackEntry {
            bookmark: name.clone(),
            number: pr.as_ref().map(|pr| pr.number),
            url: pr.as_ref().map(|pr| pr.url.clone()),
        })
        .collect();
    let mut comment_actions: Vec<SubmitAction> = Vec::new();
    for (name, pr) in &live {
        match pr {
            Some(pr) => {
                let existing = comments.stack_comment(pr.number)?;
                if creating {
                    comment_actions.push(SubmitAction::SyncStackComment {
                        bookmark: name.clone(),
                        number: Some(pr.number),
                        create: existing.is_none(),
                    });
                    continue;
                }
                let previous = existing.as_ref().and_then(|c| parse_stack_data(&c.body));
                let fossils = inherit_fossils(previous.as_ref(), &entries);
                let body = render_stack_comment(&entries, &fossils, Some(name));
                match &existing {
                    Some(existing) if existing.body != body => {
                        comment_actions.push(SubmitAction::SyncStackComment {
                            bookmark: name.clone(),
                            number: Some(pr.number),
                            create: false,
                        });
                    }
                    None if live.len() >= 2 => {
                        comment_actions.push(SubmitAction::SyncStackComment {
                            bookmark: name.clone(),
                            number: Some(pr.number),
                            create: true,
                        });
                    }
                    _ => {}
                }
            }
            None if live.len() >= 2 => {
                comment_actions.push(SubmitAction::SyncStackComment {
                    bookmark: name.clone(),
                    number: None,
                    create: true,
                });
            }
            None => {}
        }
    }
    let stack_comment_preview = (!comment_actions.is_empty())
        .then(|| render_stack_comment(&entries, &[], None));
    actions.extend(comment_actions);
    actions.extend(link);
    let pr_template_path = pr_template
        .filter(|_| {
            actions
                .iter()
                .any(|a| matches!(a, SubmitAction::CreatePr { .. }))
        })
        .map(|t| t.path.clone());

    Ok(SubmitPlan {
        head_bookmark,
        remote: repo.remote.clone(),
        base_branch: snapshot.trunk_bookmark.clone(),
        options: *options,
        segments,
        actions,
        blockers,
        warnings,
        stack_comment_preview,
        pr_template_path,
    })
}

fn retarget_number(action: &SubmitAction) -> Option<u64> {
    match action {
        SubmitAction::RetargetPr { number, .. } => Some(*number),
        _ => None,
    }
}

/// Whether the open pull requests in `open` are every open member of
/// GitHub stack `stack`: the lowest of them is based on the stack's own
/// base (members below it have merged; a stack lands from the bottom),
/// the highest sits in the stack's last seat, and none is missing in
/// between.
pub(crate) fn sees_whole_stack(open: &[PrSummary], stack: u64) -> bool {
    let mut seats: Vec<(&PrSummary, &crate::pr::StackSeat)> = open
        .iter()
        .filter_map(|pr| pr.stack.as_ref().filter(|s| s.number == stack).map(|s| (pr, s)))
        .collect();
    seats.sort_by_key(|(_, seat)| seat.position);
    let (Some((bottom, low)), Some((_, high))) = (seats.first(), seats.last()) else {
        return false;
    };
    bottom.base_branch == low.base
        && high.position == high.size
        && seats
            .windows(2)
            .all(|pair| pair[1].1.position == pair[0].1.position + 1)
}

/// Someone else's pull requests that sit on top of `head`, the top branch
/// of a chain, as one line: the one based on `head`, the one based on
/// that one's branch, and so on up. These are the ones a GitHub stack of
/// the chain can take in, and in a stack GitHub rebases their branches
/// itself after each merge below (seen on the sandbox), so they go on
/// showing only their own changes.
///
/// Plain ones of this repository only: one that is in a stack already is
/// where it is, and a fork's branch is in another repository. The line
/// ends where two rest on the same branch, because a stack is one line.
/// And nothing is on top of a chain that goes on above `head` here: a
/// bookmark of the user's up there means theirs is beside the chain.
pub(crate) fn on_top_of<'a>(
    snapshot: &RepoSnapshot,
    prs: &'a RepoPrState,
    head: &str,
) -> Vec<&'a PrSummary> {
    let Some(at) = snapshot
        .bookmarks
        .iter()
        .find(|b| b.is_local && b.name == head)
        .map(|b| b.target.as_str())
    else {
        return Vec::new();
    };
    // Nodes come children before parents: one pass from the bottom up
    // finds everything that has `at` below it.
    let mut above: Vec<&str> = Vec::new();
    for node in snapshot.nodes.iter().rev() {
        if node
            .parents
            .iter()
            .any(|p| p == at || above.contains(&p.as_str()))
        {
            above.push(node.id.as_str());
        }
    }
    let goes_on = snapshot
        .bookmarks
        .iter()
        .any(|b| b.is_local && !b.is_trunk && above.contains(&b.target.as_str()));
    if goes_on {
        return Vec::new();
    }
    let others = crate::pr::others_on(snapshot, prs, &[head]);
    let mut line: Vec<&PrSummary> = Vec::new();
    let mut base = head;
    loop {
        let mut on_it = others.iter().filter(|pr| pr.base_branch == base);
        let (Some(next), None) = (on_it.next(), on_it.next()) else {
            break;
        };
        if next.stack.is_some() || !prs.of_this_repository(next) {
            break;
        }
        line.push(next);
        base = next.head_branch.as_str();
    }
    line
}

/// What a plan does about GitHub's own stack: the stacks to dissolve
/// before anything is retargeted, and the link to make at the end.
///
/// `live` is the chain as it will stand, bottom-up, each segment with its
/// existing pull request; `moving` the pull requests this plan retargets
/// at any point; `open` every open pull request GitHub answered, which is
/// how the stack's members outside this chain are known. An existing
/// stack is left alone when it already is the chain, extended when the
/// chain only grew on top of it (the one change the stacks API has), and
/// otherwise dissolved and linked again. Linking again takes back the
/// pull requests above the published head that still chain on top of it;
/// any other member the new stack cannot hold is named in a warning.
///
/// `on_top` are someone else's pull requests that sit on top of the chain
/// ([`on_top_of`]). A link that is asked for outright takes them in, on
/// top, and says so by number and author: that is how a pull request of
/// theirs is spared showing landed commits. A stack that is only kept in
/// step, or linked because of the setting, does not reach for them.
fn plan_stack_link(
    live: &[(String, Option<PrSummary>)],
    moving: &[u64],
    open: &[PrSummary],
    on_top: &[&PrSummary],
    wanted: StackLink,
    opens_a_pr: bool,
    warnings: &mut Vec<String>,
) -> (Vec<SubmitAction>, Option<SubmitAction>) {
    let take_in: Vec<u64> = match wanted {
        StackLink::Link => on_top.iter().map(|pr| pr.number).collect(),
        _ => Vec::new(),
    };
    let taking_in = |warnings: &mut Vec<String>| {
        let listed: Vec<String> = on_top.iter().map(|pr| pr.by_author()).collect();
        let one = listed.len() == 1;
        warnings.push(format!(
            "{} {} on top of this chain and {} someone else's: {} into the GitHub stack with \
             it. In a stack GitHub rebases {} itself after each merge below, so {} on \
             showing only {} own changes; and {} cannot be changed while {} in the stack",
            match listed.as_slice() {
                [one] => one.clone(),
                [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
                [] => unreachable!("only called with some to take in"),
            },
            if one { "sits" } else { "sit" },
            if one { "is" } else { "are" },
            if one { "it goes" } else { "they go" },
            if one { "its branch" } else { "their branches" },
            if one { "it goes" } else { "they go" },
            if one { "its" } else { "their" },
            if one { "its base" } else { "their bases" },
            if one { "it is" } else { "they are" },
        ));
    };
    let seat = |entry: &(String, Option<PrSummary>)| {
        entry.1.as_ref().and_then(|pr| pr.stack.clone())
    };
    let mut stacks: Vec<u64> = live.iter().filter_map(|e| seat(e).map(|s| s.number)).collect();
    stacks.sort_unstable();
    stacks.dedup();
    let dissolve = |stacks: &[u64]| -> Vec<SubmitAction> {
        stacks
            .iter()
            .map(|stack| {
                let mut members: Vec<(u32, u64)> = open
                    .iter()
                    .filter_map(|pr| {
                        let seat = pr.stack.as_ref().filter(|s| s.number == *stack)?;
                        Some((seat.position, pr.number))
                    })
                    .collect();
                members.sort_unstable();
                SubmitAction::Unstack {
                    stack: *stack,
                    members: members.into_iter().map(|(_, number)| number).collect(),
                }
            })
            .collect()
    };
    // Members of the stacks being dissolved that this chain does not
    // reach, lowest seat first. (By pull request, not by branch name: a
    // member is the chain's own when it is the pull request of one of
    // its segments.)
    let outside = || -> Vec<&PrSummary> {
        let mut outside: Vec<&PrSummary> = open
            .iter()
            .filter(|pr| {
                pr.stack.as_ref().is_some_and(|s| stacks.contains(&s.number))
                    && !live
                        .iter()
                        .any(|(_, own)| own.as_ref().is_some_and(|own| own.number == pr.number))
            })
            .collect();
        outside.sort_by_key(|pr| pr.stack.as_ref().map(|s| (s.number, s.position)));
        outside
    };
    // By number and author: a member this chain does not reach may be
    // someone else's pull request, added to the stack above this one.
    let named = |prs: &[&PrSummary]| -> String {
        let listed: Vec<String> = prs.iter().map(|pr| pr.by_author()).collect();
        match listed.as_slice() {
            [] => String::new(),
            [one] => one.clone(),
            [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
        }
    };
    let stack_names = || crate::land::number_list(stacks.iter().copied()).replace('#', "");
    let leaving = |left: &[&PrSummary], warnings: &mut Vec<String>| {
        if left.is_empty() {
            return;
        }
        warnings.push(format!(
            "{} {} in GitHub stack {} but not in this chain as it now stands, and \
             {} the stack when it is dissolved",
            named(left),
            if left.len() == 1 { "is" } else { "are" },
            stack_names(),
            if left.len() == 1 { "leaves" } else { "leave" },
        ));
    };

    match wanted {
        StackLink::Unlink => {
            // Asked for outright. The members this chain does not reach
            // go out with the rest; each stays open on the base it has.
            let also = outside();
            if !also.is_empty() {
                warnings.push(format!(
                    "dissolving GitHub stack {} takes {} out of it too: {} open, as a \
                     plain pull request on the base {} now",
                    stack_names(),
                    named(&also),
                    if also.len() == 1 { "it stays" } else { "they stay" },
                    if also.len() == 1 { "it has" } else { "they have" },
                ));
            }
            return (dissolve(&stacks), None);
        }
        StackLink::Follow if stacks.is_empty() => return (Vec::new(), None),
        // The setting is about new stacks: a plain chain that only
        // changes stays plain, however often it is published. One that
        // gains a pull request is linked then.
        StackLink::LinkNew if stacks.is_empty() && !opens_a_pr => return (Vec::new(), None),
        StackLink::Follow | StackLink::Link | StackLink::LinkNew => {}
    }
    // One pull request of the chain's own is enough for a stack when
    // someone else's sits on top of it and is taken in.
    if live.is_empty() || live.len() + take_in.len() < 2 {
        if wanted == StackLink::Link {
            warnings.push(
                "a GitHub stack needs at least two pull requests; this one stays a \
                 single pull request"
                    .to_owned(),
            );
        }
        // A lone pull request left in a stack that changes shape still
        // has to leave it to be retargeted.
        let moved = live.iter().any(|(_, pr)| {
            pr.as_ref()
                .is_some_and(|pr| pr.stack.is_some() && moving.contains(&pr.number))
        });
        if moved {
            leaving(&outside(), warnings);
        }
        return (if moved { dissolve(&stacks) } else { Vec::new() }, None);
    }
    if stacks.is_empty() {
        if !take_in.is_empty() {
            taking_in(warnings);
        }
        return (
            Vec::new(),
            Some(SubmitAction::LinkStack {
                bookmarks: live.iter().map(|(name, _)| name.clone()).collect(),
                onto: None,
                above: take_in,
            }),
        );
    }

    // The existing stack is still this chain when its pull requests are
    // the chain's bottom, in order, in consecutive seats, none of them
    // retargeted. Anything above them in the chain is then new and can be
    // added on top, provided the stack ends where they do.
    let seated = live.iter().take_while(|e| seat(e).is_some()).count();
    let rest_unseated = live[seated..].iter().all(|e| seat(e).is_none());
    let seats: Vec<_> = live[..seated].iter().filter_map(seat).collect();
    let in_order = stacks.len() == 1
        && rest_unseated
        && seats.windows(2).all(|pair| pair[1].position == pair[0].position + 1)
        && !live[..seated]
            .iter()
            .any(|(_, pr)| pr.as_ref().is_some_and(|pr| moving.contains(&pr.number)));
    let top = seats.last();
    let ends_here = top.is_some_and(|top| top.position == top.size);
    if in_order && seated == live.len() && !(ends_here && !take_in.is_empty()) {
        return (Vec::new(), None);
    }
    if in_order && ends_here {
        if !take_in.is_empty() {
            taking_in(warnings);
        }
        return (
            Vec::new(),
            Some(SubmitAction::LinkStack {
                bookmarks: live[seated..].iter().map(|(name, _)| name.clone()).collect(),
                onto: Some(stacks[0]),
                above: take_in,
            }),
        );
    }

    // Dissolve and link again. The stack may go on above the published
    // head, in pull requests this chain does not reach. Those that still
    // chain on top of the head (each based on the branch of the one
    // below, starting from the head's) go back in, in that order.
    let mut left = outside();
    let mut above: Vec<u64> = Vec::new();
    let mut base = live.last().map(|(name, _)| name.as_str()).unwrap_or_default();
    let mut back_on_top: Vec<&PrSummary> = Vec::new();
    while let Some(at) = left.iter().position(|pr| pr.base_branch == base) {
        let pr = left.remove(at);
        above.push(pr.number);
        back_on_top.push(pr);
        base = pr.head_branch.as_str();
    }
    if !back_on_top.is_empty() {
        warnings.push(format!(
            "{} {} above this chain in GitHub stack {}: the stack is dissolved to \
             change shape and linked again with {} back on top",
            named(&back_on_top),
            if back_on_top.len() == 1 { "sits" } else { "sit" },
            stack_names(),
            if back_on_top.len() == 1 { "it" } else { "them" },
        ));
    }
    leaving(&left, warnings);
    // Nothing of the old stack goes back on top: what sits there now and
    // was asked in comes instead.
    if above.is_empty() && !take_in.is_empty() {
        taking_in(warnings);
        above = take_in;
    }
    (
        dissolve(&stacks),
        Some(SubmitAction::LinkStack {
            bookmarks: live.iter().map(|(name, _)| name.clone()).collect(),
            onto: None,
            above,
        }),
    )
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

/// Run a plan, step by step in the order it lists them (see
/// [`plan_submit`] for why that order): derived bookmark creations, the
/// stack dissolved if it changes shape, parking retargets, one batched
/// push, PR creations bottom-up, the final retargets, text updates, stack
/// comments, and the link as a GitHub stack. The first failure stops
/// execution — later steps report as skipped rather than running against
/// a half-updated remote. A plan with blockers refuses to run at all.
///
/// Stack-comment steps re-read and re-render at execution time (the
/// read-merge-write jjpr does): PRs created moments earlier now have real
/// numbers and links, and a comment that already reads right is left
/// untouched rather than edited to an identical body.
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
    // PRs opened by this run, for the steps that follow them.
    let mut created: std::collections::HashMap<String, PrSummary> =
        std::collections::HashMap::new();

    let mut index = 0;
    while index < steps.len() && !failed {
        // The batched push: every push action succeeds or fails as one
        // operation, exactly like `jj git push -b a -b b`. The plan lists
        // its pushes together.
        if matches!(steps[index].action, SubmitAction::Push { .. }) {
            let run = steps[index..]
                .iter()
                .take_while(|step| matches!(step.action, SubmitAction::Push { .. }))
                .count();
            let names: Vec<String> = steps[index..index + run]
                .iter()
                .map(|step| match &step.action {
                    SubmitAction::Push { bookmark, .. } => bookmark.clone(),
                    _ => unreachable!(),
                })
                .collect();
            let (status, detail) = match vcs.push_bookmarks(&names, &plan.remote) {
                Ok(summary) => (SubmitStepStatus::Done, summary),
                Err(err) => {
                    failed = true;
                    // The lease held: when the branch that had moved is one
                    // that was taken over, its author pushed to it again.
                    let taken: Vec<TakenOver> = plan
                        .segments
                        .iter()
                        .filter_map(|segment| segment.taken_over.clone())
                        .collect();
                    let refusal = err.to_string();
                    let detail = match crate::takeover::pushed_to_again(
                        &refusal,
                        &taken,
                        "publish again",
                    ) {
                        Some(story) => format!("{refusal}. {story}"),
                        None => refusal,
                    };
                    (SubmitStepStatus::Failed, detail)
                }
            };
            for step in &mut steps[index..index + run] {
                step.status = status;
                step.detail = Some(detail.clone());
            }
            index += run;
            continue;
        }

        let step = &mut steps[index];
        let result: Result<(String, Option<PrSummary>), ForgeError> = match &step.action {
            SubmitAction::Push { .. } => unreachable!("pushes run as one batch above"),
            SubmitAction::CreateBookmark { bookmark, change } => vcs
                .create_bookmark(bookmark, change)
                .map(|summary| (summary, None))
                .map_err(|err| ForgeError::Plan(err.to_string())),
            SubmitAction::CreatePr {
                bookmark,
                base,
                title,
                body,
                draft,
            } => forge.create_pr(title, body, bookmark, base, *draft).map(|pr| {
                created.insert(bookmark.clone(), pr.clone());
                let kind = if *draft { "draft " } else { "" };
                (format!("Opened {kind}#{} for {bookmark}", pr.number), Some(pr))
            }),
            SubmitAction::RetargetPr {
                number,
                bookmark,
                from_base,
                to_base,
            } => forge.update_pr_base(*number, to_base).map(|()| {
                (
                    format!("Retargeted #{number} ({bookmark}) from {from_base} to {to_base}"),
                    None,
                )
            }),
            SubmitAction::UpdatePrText {
                number,
                bookmark,
                title,
                body,
                seed,
            } => forge.update_pr_text(*number, title.as_deref(), body).map(|()| {
                let story = if *seed {
                    format!("Recorded Jiji's description fingerprints on #{number}")
                } else if title.is_some() {
                    format!("Updated #{number}'s title and description from {bookmark}")
                } else {
                    format!("Updated #{number}'s description from {bookmark}")
                };
                (story, None)
            }),
            SubmitAction::SyncStackComment { bookmark, number, .. } => {
                sync_stack_comment(plan, forge, &created, bookmark, *number)
                    .map(|story| (story, None))
            }
            SubmitAction::Unstack { stack, .. } => forge.unstack(*stack).and_then(|kept| {
                if kept.is_empty() {
                    return Ok((
                        format!(
                            "Dissolved GitHub stack {stack}; its pull requests are a plain chain"
                        ),
                        None,
                    ));
                }
                // The steps after this one need every pull request out
                // of the stack. One GitHub would not let go (it documents
                // a pull request queued to merge) stops them.
                Err(ForgeError::Api(format!(
                    "GitHub kept {} in stack {stack} and took the rest out — a pull \
                     request queued to merge cannot leave its stack; publish again once \
                     it has merged or left the queue",
                    crate::land::number_list(kept.into_iter())
                )))
            }),
            SubmitAction::LinkStack {
                bookmarks,
                onto,
                above,
            } => match link_stack(plan, forge, &created, bookmarks, *onto, above) {
                // Linked because a setting says to, not because this
                // publish was asked: a repository without GitHub stacks
                // keeps its plain chain, and that is not a failure.
                Err(ForgeError::NoStacks(why)) if plan.options.stack == StackLink::LinkNew => {
                    Ok((format!("{why}; the pull requests stay a plain chain"), None))
                }
                other => other.map(|story| (story, None)),
            },
        };
        match result {
            Ok((detail, pr)) => {
                step.status = SubmitStepStatus::Done;
                step.detail = Some(detail);
                step.pr = pr;
            }
            Err(err) => {
                step.status = SubmitStepStatus::Failed;
                step.detail = Some(match err {
                    // A bookmark step's error is the backend's own story.
                    ForgeError::Plan(message)
                        if matches!(step.action, SubmitAction::CreateBookmark { .. }) =>
                    {
                        message
                    }
                    other => other.to_string(),
                });
                failed = true;
            }
        }
        index += 1;
    }

    if failed {
        // What a stop in the middle left behind on GitHub, told where the
        // failure is told.
        let notes = unfinished(plan, &steps);
        if let Some(step) = steps
            .iter_mut()
            .find(|step| step.status == SubmitStepStatus::Failed)
        {
            for note in notes {
                let detail = step.detail.get_or_insert_with(String::new);
                detail.push_str(" — ");
                detail.push_str(&note);
            }
        }
    }

    Ok(SubmitOutcome { steps, failed })
}

/// What a publish that stopped part-way left for the next one: pull
/// requests parked on trunk and not pointed back, and a GitHub stack
/// dissolved and not linked again. Both are finished by publishing again,
/// and each has something the user should know meanwhile.
fn unfinished(plan: &SubmitPlan, steps: &[SubmitStep]) -> Vec<String> {
    let trunk = plan.base_branch.as_str();
    let done = |step: &SubmitStep| step.status == SubmitStepStatus::Done;
    let mut notes: Vec<String> = Vec::new();

    let parked: Vec<u64> = steps
        .iter()
        .enumerate()
        .filter_map(|(at, step)| match &step.action {
            SubmitAction::RetargetPr {
                number,
                from_base,
                to_base,
                ..
            } if done(step) && to_base == trunk && from_base != trunk => {
                // Parked, with the retarget that points it back still to
                // run. One whose new base is trunk is where it belongs.
                steps[at + 1..]
                    .iter()
                    .any(|later| {
                        !done(later)
                            && matches!(
                                &later.action,
                                SubmitAction::RetargetPr { number: n, .. } if n == number
                            )
                    })
                    .then_some(*number)
            }
            _ => None,
        })
        .collect();
    if !parked.is_empty() {
        notes.push(format!(
            "{} {} left pointing at \u{201c}{trunk}\u{201d}, where {} everything below \
             as well: publish again to finish, and do not merge {} until then",
            crate::land::number_list(parked.iter().copied()),
            if parked.len() == 1 { "is" } else { "are" },
            if parked.len() == 1 { "it shows" } else { "they show" },
            if parked.len() == 1 { "it" } else { "them" },
        ));
    }

    let relinks = steps
        .iter()
        .any(|step| matches!(step.action, SubmitAction::LinkStack { .. }));
    let relinked = steps
        .iter()
        .any(|step| matches!(step.action, SubmitAction::LinkStack { .. }) && done(step));
    let dissolved: Vec<u64> = steps
        .iter()
        .filter_map(|step| match step.action {
            SubmitAction::Unstack { stack, .. } if done(step) => Some(stack),
            _ => None,
        })
        .collect();
    if relinks && !relinked && !dissolved.is_empty() {
        notes.push(format!(
            "GitHub stack {} was dissolved for this publish and is not linked again: \
             its pull requests are a plain chain until a publish that asks for the \
             GitHub stack (--stack) goes through",
            crate::land::number_list(dissolved.into_iter()).replace('#', "")
        ));
    }
    notes
}

/// One link step: the pull requests behind `bookmarks` (existing, or
/// opened earlier in this run), bottom first, then the ones `above` them,
/// as a new GitHub stack or on top of stack `onto`.
fn link_stack(
    plan: &SubmitPlan,
    forge: &dyn SubmitForge,
    created: &std::collections::HashMap<String, PrSummary>,
    bookmarks: &[String],
    onto: Option<u64>,
    above: &[u64],
) -> Result<String, ForgeError> {
    let mut numbers: Vec<u64> = bookmarks
        .iter()
        .map(|bookmark| {
            plan.segments
                .iter()
                .find(|segment| segment.bookmark == *bookmark)
                .and_then(|segment| segment.pr.as_ref())
                .or_else(|| created.get(bookmark))
                .map(|pr| pr.number)
                .ok_or_else(|| {
                    ForgeError::Plan(format!(
                        "no pull request exists for \u{201c}{bookmark}\u{201d} to link"
                    ))
                })
        })
        .collect::<Result<_, _>>()?;
    numbers.extend_from_slice(above);
    let list = crate::land::number_list(numbers.iter().copied());
    match onto {
        Some(stack) => {
            forge.add_to_stack(stack, &numbers)?;
            Ok(format!("Added {list} to GitHub stack {stack}"))
        }
        None => {
            let stack = forge.create_stack(&numbers)?;
            Ok(format!("Linked {list} as GitHub stack {stack}"))
        }
    }
}

/// One comment step: resolve the PR (planned number or just created),
/// re-read its existing comment, merge fossils, render, and write only
/// when the text actually changes.
fn sync_stack_comment(
    plan: &SubmitPlan,
    forge: &dyn SubmitForge,
    created: &std::collections::HashMap<String, PrSummary>,
    bookmark: &str,
    number: Option<u64>,
) -> Result<String, ForgeError> {
    let number = number
        .or_else(|| created.get(bookmark).map(|pr| pr.number))
        .ok_or_else(|| {
            ForgeError::Plan(format!(
                "no pull request exists for \u{201c}{bookmark}\u{201d} to comment on"
            ))
        })?;
    // The live stack as it stands after the steps above: the plan's
    // segments that have a PR, links filled in from this run's creations.
    let entries: Vec<StackEntry> = plan
        .segments
        .iter()
        .filter_map(|segment| {
            let pr = segment
                .pr
                .as_ref()
                .or_else(|| created.get(&segment.bookmark))?;
            Some(StackEntry {
                bookmark: segment.bookmark.clone(),
                number: Some(pr.number),
                url: Some(pr.url.clone()),
            })
        })
        .collect();
    let existing = forge.stack_comment(number)?;
    let previous = existing.as_ref().and_then(|c| parse_stack_data(&c.body));
    let fossils = inherit_fossils(previous.as_ref(), &entries);
    let body = render_stack_comment(&entries, &fossils, Some(bookmark));
    match existing {
        Some(existing) if existing.body == body => {
            Ok(format!("The stack comment on #{number} already reads right"))
        }
        Some(existing) => {
            forge.update_comment(existing.id, &body)?;
            Ok(format!("Updated the stack comment on #{number}"))
        }
        None => {
            forge.create_comment(number, &body)?;
            Ok(format!("Posted the stack comment on #{number}"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pr::{ChecksRollup, PrState, PrStateReport, ReviewDecision, StackSeat};
    use crate::reconcile::{extract_managed_body, fingerprint, stored_body_fp, wrap_managed_body};
    use crate::remote::ForgeProvider;
    use jiji_core::snapshot::{BookmarkState, ConflictItem, ConflictKind, NodeKind};
    use std::cell::RefCell;
    use std::collections::HashMap;

    /// Planning against a forge with no stack comments anywhere.
    struct NoComments;

    impl StackCommentSource for NoComments {
        fn stack_comment(&self, _number: u64) -> Result<Option<ExistingComment>, ForgeError> {
            Ok(None)
        }
    }

    /// Planning against known existing comments, keyed by PR number.
    struct StubComments(HashMap<u64, ExistingComment>);

    impl StackCommentSource for StubComments {
        fn stack_comment(&self, number: u64) -> Result<Option<ExistingComment>, ForgeError> {
            Ok(self.0.get(&number).cloned())
        }
    }

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
            taken_over: Vec::new(),
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
            merge_commit: None,
            stack: None,
            author: None,
        }
    }

    /// A PR whose title and body already read exactly as Jiji would write
    /// them for the given commit text — the in-sync state.
    fn text_pr(number: u64, head: &str, base: &str, title: &str, commit_body: &str) -> PrSummary {
        PrSummary {
            title: title.into(),
            body: Some(wrap_managed_body(commit_body, title)),
            ..open_pr(number, head, base)
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

    /// The classic head form; most tests publish through a bookmark.
    fn by_bookmark(name: &str) -> SubmitHead {
        SubmitHead::Bookmark { name: name.into() }
    }

    /// The derived-name head form: publish the stack under a change.
    fn by_change(change: &str) -> SubmitHead {
        SubmitHead::Change { change: change.into() }
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
    fn plans_pushes_creations_retargets_text_and_comments_in_order() {
        let mut snap = stack_snapshot();
        // `auth` segments at a2 (its bookmark target), profile above it.
        snap.bookmarks[1].target = "a2".into();
        snap.nodes[1].bookmarks = vec!["auth".into()];
        snap.nodes[2].bookmarks = vec![];
        // profile already has a PR, parked on the wrong base, with a title
        // Jiji never wrote and an empty body.
        let prs = pr_state(vec![open_pr(7, "profile", "main")], false);

        let plan = plan_submit(&snap, &prs, &forge_repo(), &by_bookmark("profile"), &NoComments, None, &SubmitOptions::default()).unwrap();
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

        assert_eq!(plan.actions.len(), 7, "{:?}", plan.actions);
        assert_eq!(
            plan.actions[..4].to_vec(),
            vec![
                SubmitAction::Push { bookmark: "auth".into(), create: true },
                SubmitAction::Push { bookmark: "profile".into(), create: false },
                SubmitAction::CreatePr {
                    bookmark: "auth".into(),
                    base: "main".into(),
                    title: "auth: login flow".into(),
                    // The prose survives, the trailer block drops, and the
                    // body is born sentinel-wrapped and fingerprinted.
                    body: wrap_managed_body("The form.", "auth: login flow"),
                    draft: false,
                },
                SubmitAction::RetargetPr {
                    number: 7,
                    bookmark: "profile".into(),
                    from_base: "main".into(),
                    to_base: "auth".into(),
                },
            ]
        );
        // #7's empty body fills in from the commit; the hand-written title
        // is not claimed (no title fingerprint), only warned about.
        let SubmitAction::UpdatePrText { number, title, body, seed, .. } = &plan.actions[4]
        else {
            panic!("expected UpdatePrText, got {:?}", plan.actions[4]);
        };
        assert_eq!(*number, 7);
        assert_eq!(*title, None);
        assert!(!seed);
        assert_eq!(extract_managed_body(body), Some("With uploads."));
        assert_eq!(stored_body_fp(body), Some(fingerprint("With uploads.").as_str()));
        assert!(
            plan.warnings.iter().any(|w| w.contains("PR 7") && w.contains("title")),
            "{:?}",
            plan.warnings
        );
        // A creation is pending, so every live PR syncs its stack comment;
        // the created PR's number is unknowable until execution.
        assert_eq!(
            plan.actions[5..].to_vec(),
            vec![
                SubmitAction::SyncStackComment {
                    bookmark: "auth".into(),
                    number: None,
                    create: true,
                },
                SubmitAction::SyncStackComment {
                    bookmark: "profile".into(),
                    number: Some(7),
                    create: true,
                },
            ]
        );
        let preview = plan.stack_comment_preview.as_deref().expect("preview renders");
        assert!(preview.contains("1. `auth`"), "created PRs render plain: {preview}");
        assert!(preview.contains("[`profile`]"), "existing PRs link: {preview}");
    }

    #[test]
    fn new_pr_bodies_fold_in_the_repo_template() {
        let snap = stack_snapshot();
        let prs = pr_state(vec![], false);
        let template = PrTemplate {
            path: ".github/PULL_REQUEST_TEMPLATE.md".into(),
            text: "## Checklist\n- [ ] tests\n".into(),
        };

        let plan = plan_submit(
            &snap,
            &prs,
            &forge_repo(),
            &by_bookmark("profile"),
            &NoComments,
            Some(&template),
            &SubmitOptions::default(),
        )
        .unwrap();
        assert_eq!(
            plan.pr_template_path.as_deref(),
            Some(".github/PULL_REQUEST_TEMPLATE.md")
        );
        let bodies: Vec<&str> = plan
            .actions
            .iter()
            .filter_map(|a| match a {
                SubmitAction::CreatePr { body, .. } => Some(body.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(bodies.len(), 2);
        for body in bodies {
            // Managed description first, template below as user-space text.
            assert!(body.starts_with(crate::reconcile::DESCRIPTION_START), "{body}");
            assert!(body.ends_with("## Checklist\n- [ ] tests"), "{body}");
        }
    }

    #[test]
    fn update_only_plans_leave_the_template_out() {
        // Both PRs exist with the right bases: the plan only pushes, so
        // existing descriptions are never templated and the panel note
        // would be noise.
        let mut snap = stack_snapshot();
        snap.bookmarks[1].target = "a2".into();
        snap.nodes[1].bookmarks = vec!["auth".into()];
        snap.nodes[2].bookmarks = vec![];
        let prs = pr_state(
            vec![
                text_pr(6, "auth", "main", "auth: login flow", "The form."),
                text_pr(7, "profile", "auth", "profile: avatars", "With uploads."),
            ],
            false,
        );
        let template = PrTemplate { path: "PULL_REQUEST_TEMPLATE.md".into(), text: "T".into() };
        let plan = plan_submit(
            &snap,
            &prs,
            &forge_repo(),
            &by_bookmark("profile"),
            &NoComments,
            Some(&template),
            &SubmitOptions::default(),
        )
        .unwrap();
        assert!(
            plan.actions.iter().all(|a| !matches!(a, SubmitAction::CreatePr { .. })),
            "{:?}",
            plan.actions
        );
        assert_eq!(plan.pr_template_path, None);
    }

    /// The fully-synced fixture: bookmarks synced, PR bases right, PR text
    /// exactly as Jiji writes it, and both stack comments current.
    fn consistent_stack() -> (RepoSnapshot, RepoPrState, StubComments) {
        let mut snap = stack_snapshot();
        snap.bookmarks[1].sync = SyncState::Synced;
        snap.bookmarks[1].remote = Some("origin".into());
        snap.bookmarks[2].sync = SyncState::Synced;
        let prs = pr_state(
            vec![
                text_pr(6, "auth", "main", "auth: login flow", "The form."),
                // The upper PR's commit has no body: an empty body with no
                // markers is also in-sync (nothing to manage yet).
                PrSummary {
                    title: "auth: sessions".into(),
                    ..open_pr(7, "profile", "auth")
                },
            ],
            false,
        );
        let entries = vec![
            crate::comment::StackEntry {
                bookmark: "auth".into(),
                number: Some(6),
                url: Some("https://github.com/o/r/pull/6".into()),
            },
            crate::comment::StackEntry {
                bookmark: "profile".into(),
                number: Some(7),
                url: Some("https://github.com/o/r/pull/7".into()),
            },
        ];
        let comments = StubComments(HashMap::from([
            (
                6,
                ExistingComment {
                    id: 60,
                    body: render_stack_comment(&entries, &[], Some("auth")),
                },
            ),
            (
                7,
                ExistingComment {
                    id: 70,
                    body: render_stack_comment(&entries, &[], Some("profile")),
                },
            ),
        ]));
        (snap, prs, comments)
    }

    #[test]
    fn consistent_stacks_plan_nothing() {
        let (snap, prs, comments) = consistent_stack();
        let plan = plan_submit(&snap, &prs, &forge_repo(), &by_bookmark("profile"), &comments, None, &SubmitOptions::default()).unwrap();
        assert!(plan.actions.is_empty(), "{:?}", plan.actions);
        assert!(plan.blockers.is_empty());
        assert!(plan.warnings.is_empty(), "{:?}", plan.warnings);
        assert_eq!(plan.stack_comment_preview, None);
        assert_eq!(plan.segments.len(), 2);
    }

    #[test]
    fn stale_pr_text_updates_and_hand_edits_are_respected() {
        // The auth commit's body moved since Jiji wrote #6.
        let (mut snap, prs, comments) = consistent_stack();
        snap.nodes[2].description =
            "auth: login flow\n\nThe form, now with validation.".into();
        let plan = plan_submit(&snap, &prs, &forge_repo(), &by_bookmark("profile"), &comments, None, &SubmitOptions::default()).unwrap();
        let updates: Vec<_> = plan
            .actions
            .iter()
            .filter_map(|a| match a {
                SubmitAction::UpdatePrText { number, body, seed, title, .. } => {
                    Some((*number, body.clone(), *seed, title.clone()))
                }
                _ => None,
            })
            .collect();
        assert_eq!(updates.len(), 1, "{:?}", plan.actions);
        let (number, body, seed, title) = &updates[0];
        assert_eq!(*number, 6);
        assert!(!seed);
        assert_eq!(*title, None, "the title did not move");
        assert_eq!(
            extract_managed_body(body),
            Some("The form, now with validation.")
        );

        // The same drift on GitHub's side instead: the user rewrote the
        // managed section, the commit is unchanged — nothing plans.
        let (snap, mut prs, _) = consistent_stack();
        let hand_edited = wrap_managed_body("The form.", "auth: login flow")
            .replace("The form.", "My hand-written description.");
        prs.report.prs[0].body = Some(hand_edited.clone());
        prs.by_branch.get_mut("auth").unwrap().body = Some(hand_edited);
        let (_, _, comments) = consistent_stack();
        let plan = plan_submit(&snap, &prs, &forge_repo(), &by_bookmark("profile"), &comments, None, &SubmitOptions::default()).unwrap();
        assert!(plan.actions.is_empty(), "{:?}", plan.actions);
        assert!(plan.warnings.is_empty(), "{:?}", plan.warnings);

        // Both sides moved: no action, a warning that names the conflict.
        let (mut snap, mut prs, _) = consistent_stack();
        snap.nodes[2].description = "auth: login flow\n\nRewritten in the commit.".into();
        let hand_edited = wrap_managed_body("The form.", "auth: login flow")
            .replace("The form.", "Rewritten on GitHub.");
        prs.report.prs[0].body = Some(hand_edited.clone());
        prs.by_branch.get_mut("auth").unwrap().body = Some(hand_edited);
        let (_, _, comments) = consistent_stack();
        let plan = plan_submit(&snap, &prs, &forge_repo(), &by_bookmark("profile"), &comments, None, &SubmitOptions::default()).unwrap();
        assert!(
            !plan.actions.iter().any(|a| matches!(a, SubmitAction::UpdatePrText { .. })),
            "{:?}",
            plan.actions
        );
        assert!(
            plan.warnings.iter().any(|w| w.contains("#6") && w.contains("both changed")),
            "{:?}",
            plan.warnings
        );
    }

    #[test]
    fn titles_update_through_their_fingerprint() {
        // Jiji wrote #6's title; the commit's first line moved.
        let (mut snap, prs, comments) = consistent_stack();
        snap.nodes[2].description = "auth: login and signup flow\n\nThe form.".into();
        let plan = plan_submit(&snap, &prs, &forge_repo(), &by_bookmark("profile"), &comments, None, &SubmitOptions::default()).unwrap();
        let update = plan
            .actions
            .iter()
            .find_map(|a| match a {
                SubmitAction::UpdatePrText { number: 6, title, body, seed, .. } => {
                    Some((title.clone(), body.clone(), *seed))
                }
                _ => None,
            })
            .expect("the title update plans");
        assert_eq!(update.0.as_deref(), Some("auth: login and signup flow"));
        assert!(!update.2);
        // The managed body text is untouched by a title-only update.
        assert_eq!(extract_managed_body(&update.1), Some("The form."));
    }

    #[test]
    fn identical_markerless_bodies_adopt_quietly() {
        // A PR created before fingerprinting: right text, no markers.
        let (snap, mut prs, comments) = consistent_stack();
        prs.report.prs[0].body = Some("The form.".into());
        prs.by_branch.get_mut("auth").unwrap().body = Some("The form.".into());
        let plan = plan_submit(&snap, &prs, &forge_repo(), &by_bookmark("profile"), &comments, None, &SubmitOptions::default()).unwrap();
        let seed = plan
            .actions
            .iter()
            .find_map(|a| match a {
                SubmitAction::UpdatePrText { number: 6, seed, body, .. } => {
                    Some((*seed, body.clone()))
                }
                _ => None,
            })
            .expect("adoption plans a seed write");
        assert!(seed.0, "nothing visible changes");
        assert_eq!(extract_managed_body(&seed.1), Some("The form."));

        // A markerless body that reads differently is the user's: silence.
        let (snap, mut prs, comments) = consistent_stack();
        prs.report.prs[0].body = Some("A description someone wrote by hand.".into());
        prs.by_branch.get_mut("auth").unwrap().body =
            Some("A description someone wrote by hand.".into());
        let plan = plan_submit(&snap, &prs, &forge_repo(), &by_bookmark("profile"), &comments, None, &SubmitOptions::default()).unwrap();
        assert!(plan.actions.is_empty(), "{:?}", plan.actions);
        assert!(plan.warnings.is_empty(), "{:?}", plan.warnings);
    }

    #[test]
    fn stack_comments_sync_only_when_stale() {
        // One comment drifted (say the user edited it): exactly one
        // update plans, aimed at that comment.
        let (snap, prs, mut comments) = consistent_stack();
        comments.0.get_mut(&7).unwrap().body = "someone touched this".into();
        let plan = plan_submit(&snap, &prs, &forge_repo(), &by_bookmark("profile"), &comments, None, &SubmitOptions::default()).unwrap();
        assert_eq!(
            plan.actions,
            vec![SubmitAction::SyncStackComment {
                bookmark: "profile".into(),
                number: Some(7),
                create: false,
            }]
        );
        assert!(plan.stack_comment_preview.is_some());

        // No comments exist yet on a two-PR stack: both get one.
        let (snap, prs, _) = consistent_stack();
        let plan = plan_submit(&snap, &prs, &forge_repo(), &by_bookmark("profile"), &NoComments, None, &SubmitOptions::default()).unwrap();
        let syncs: Vec<_> = plan
            .actions
            .iter()
            .filter(|a| matches!(a, SubmitAction::SyncStackComment { .. }))
            .collect();
        assert_eq!(syncs.len(), 2, "{:?}", plan.actions);
        assert!(matches!(
            *syncs[0],
            SubmitAction::SyncStackComment { number: Some(6), create: true, .. }
        ));

        // A single-PR stack never gets a first comment…
        let (mut snap, mut prs, _) = consistent_stack();
        snap.bookmarks.remove(1); // drop `auth`
        snap.nodes[2].bookmarks = vec![];
        prs.report.prs.remove(0);
        prs.by_branch.remove("auth");
        let plan = plan_submit(&snap, &prs, &forge_repo(), &by_bookmark("profile"), &NoComments, None, &SubmitOptions::default()).unwrap();
        assert!(
            !plan.actions.iter().any(|a| matches!(a, SubmitAction::SyncStackComment { .. })),
            "{:?}",
            plan.actions
        );

        // …but an existing one keeps updating as the stack shrinks: the
        // now-gone `auth` entry is inherited as a fossil.
        let entries = [crate::comment::StackEntry {
            bookmark: "profile".into(),
            number: Some(7),
            url: Some("https://github.com/o/r/pull/7".into()),
        }];
        let previous = render_stack_comment(
            &[
                crate::comment::StackEntry {
                    bookmark: "auth".into(),
                    number: Some(6),
                    url: Some("https://github.com/o/r/pull/6".into()),
                },
                entries[0].clone(),
            ],
            &[],
            Some("profile"),
        );
        let comments = StubComments(HashMap::from([(
            7,
            ExistingComment { id: 70, body: previous },
        )]));
        let plan = plan_submit(&snap, &prs, &forge_repo(), &by_bookmark("profile"), &comments, None, &SubmitOptions::default()).unwrap();
        let syncs: Vec<_> = plan
            .actions
            .iter()
            .filter(|a| matches!(a, SubmitAction::SyncStackComment { .. }))
            .collect();
        assert_eq!(
            syncs,
            vec![&SubmitAction::SyncStackComment {
                bookmark: "profile".into(),
                number: Some(7),
                create: false,
            }]
        );
    }

    #[test]
    fn unpresentable_changes_block_only_pushing_segments() {
        let mut snap = stack_snapshot();
        snap.nodes[1].description = String::new(); // a2, in auth's segment
        snap.nodes[0].has_conflict = true; // b1, profile's segment
        let prs = pr_state(vec![], false);
        let plan = plan_submit(&snap, &prs, &forge_repo(), &by_bookmark("profile"), &NoComments, None, &SubmitOptions::default()).unwrap();
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
        let plan = plan_submit(&synced, &prs, &forge_repo(), &by_bookmark("profile"), &NoComments, None, &SubmitOptions::default()).unwrap();
        assert!(plan.blockers.is_empty(), "{:?}", plan.blockers);
    }

    /// Someone else's open pull request: `head` into `base`, by
    /// `author`, with `commit` as its head on GitHub.
    fn their_pr(number: u64, head: &str, base: &str, author: &str, commit: &str) -> PrSummary {
        PrSummary {
            author: Some(author.into()),
            head_commit: commit.into(),
            ..open_pr(number, head, base)
        }
    }

    fn plan_through(snap: &RepoSnapshot, prs: &RepoPrState, head: &str, stack: StackLink) -> SubmitPlan {
        plan_submit(
            snap,
            prs,
            &forge_repo(),
            &by_bookmark(head),
            &NoComments,
            None,
            &SubmitOptions { stack },
        )
        .unwrap()
    }

    /// main(m) ── a1 (auth) ── b1 (profile, @), with #9 by someone else
    /// on profile and #10, theirs too, on top of #9. `profile` reads
    /// `sync` against its remote.
    fn pair_with_others(sync: SyncState) -> (RepoSnapshot, RepoPrState) {
        let snap = snapshot(
            vec![
                node("b1", "profile: avatars", NodeKind::WorkingCopy, &["a1"], &["profile"]),
                node("a1", "auth: login", NodeKind::Mutable, &["m"], &["auth"]),
                node("m", "release", NodeKind::Immutable, &[], &["main"]),
            ],
            vec![
                bookmark("main", "m", SyncState::Synced, true),
                bookmark("auth", "a1", SyncState::Synced, false),
                bookmark("profile", "b1", sync, false),
            ],
        );
        // Each of the stack's own pull requests has, on GitHub, the
        // commit its bookmark was last pushed at.
        let pushed_at = |pr: PrSummary, commit: &str| PrSummary {
            head_commit: commit.into(),
            ..pr
        };
        let prs = pr_state(
            vec![
                pushed_at(text_pr(1, "auth", "main", "auth: login", ""), "ca1"),
                pushed_at(
                    text_pr(7, "profile", "auth", "profile: avatars", ""),
                    if sync == SyncState::Synced { "cb1" } else { "0ldb1" },
                ),
                their_pr(9, "their-top", "profile", "kim", "c0ffee09"),
                their_pr(10, "their-next", "their-top", "kim", "c0ffee10"),
            ],
            false,
        );
        (snap, prs)
    }

    /// A publish never pushes someone else's branch. When its push
    /// replaces commits under their pull request, the plan says so by
    /// number and author before anything runs; a push that only adds
    /// commits on top of the branch disturbs nothing and says nothing.
    #[test]
    fn replacing_commits_under_someone_elses_pull_request_is_said_before_it_runs() {
        let about = |plan: &SubmitPlan| -> Vec<String> {
            plan.warnings.iter().filter(|w| w.contains("someone else's")).cloned().collect()
        };
        let (snap, prs) = pair_with_others(SyncState::Diverged);
        let plan = plan_through(&snap, &prs, "profile", StackLink::Follow);
        assert!(plan.blockers.is_empty(), "{:?}", plan.blockers);
        assert_eq!(
            about(&plan),
            [
                "#9 by @kim (\u{201c}their-top\u{201d}) is someone else's pull request on \
                 \u{201c}profile\u{201d}, and this push replaces commits under it: it shows the \
                 replaced commits as its own until its branch is rebased. Nothing here \
                 pushes to \u{201c}their-top\u{201d}: rebasing it is its author's to do",
                // The one on top of theirs rests on the same branch.
                "#10 by @kim (\u{201c}their-next\u{201d}) is someone else's pull request on \
                 \u{201c}profile\u{201d}, and this push replaces commits under it: it shows the \
                 replaced commits as its own until its branch is rebased. Nothing here \
                 pushes to \u{201c}their-next\u{201d}: rebasing it is its author's to do",
            ]
        );
        // Theirs are never pushed, retargeted, described or commented on.
        for action in &plan.actions {
            let touched = match action {
                SubmitAction::Push { bookmark, .. } | SubmitAction::CreateBookmark { bookmark, .. } => {
                    bookmark.starts_with("their")
                }
                SubmitAction::RetargetPr { number, .. } | SubmitAction::UpdatePrText { number, .. } => {
                    *number >= 9
                }
                SubmitAction::SyncStackComment { number, .. } => number.is_some_and(|n| n >= 9),
                _ => false,
            };
            assert!(!touched, "{action:?}");
        }

        // Commits added on top of `profile`: their pull request still
        // shows what it showed.
        let (snap, prs) = pair_with_others(SyncState::Ahead);
        let plan = plan_through(&snap, &prs, "profile", StackLink::Follow);
        assert!(plan.actions.iter().any(|a| matches!(a, SubmitAction::Push { .. })));
        assert!(about(&plan).is_empty(), "{:?}", plan.warnings);

        // The branch under them is not pushed at all: nothing to say.
        let (snap, prs) = pair_with_others(SyncState::Synced);
        let plan = plan_through(&snap, &prs, "profile", StackLink::Follow);
        assert!(about(&plan).is_empty(), "{:?}", plan.warnings);
    }

    /// Once a fetch has brought their branch, jj calls the stack under it
    /// immutable. It is still the stack: a new segment on top of it is
    /// published against the branch below it, not against trunk with the
    /// whole stack inside its pull request.
    #[test]
    fn a_new_segment_on_a_held_stack_is_based_on_the_stack_not_on_trunk() {
        let (mut snap, prs) = pair_with_others(SyncState::Synced);
        for node in snap.nodes.iter_mut().take(2) {
            node.kind = NodeKind::Immutable;
            node.held = true;
        }
        snap.nodes.insert(
            0,
            node("c1", "search: index", NodeKind::WorkingCopy, &["b1"], &["search"]),
        );
        snap.bookmarks.push(bookmark("search", "c1", SyncState::LocalOnly, false));
        let plan = plan_through(&snap, &prs, "search", StackLink::Follow);
        assert!(plan.blockers.is_empty(), "{:?}", plan.blockers);
        assert_eq!(
            plan.segments.iter().map(|s| (s.bookmark.as_str(), s.base.as_str())).collect::<Vec<_>>(),
            [("auth", "main"), ("profile", "auth"), ("search", "profile")]
        );
        assert_eq!(retargets_and_pushes(&plan), ["push search", "open search->profile"]);

        // Without the mark the two lower segments would read as landed
        // history, and the new pull request would target trunk.
        for node in snap.nodes.iter_mut() {
            node.held = false;
        }
        let plan = plan_through(&snap, &prs, "search", StackLink::Follow);
        assert_eq!(retargets_and_pushes(&plan), ["push search", "open search->main"]);
    }

    /// A bookmark of the stack moved onto their commits: pushed, their
    /// pull request's head would be inside its own base, and GitHub
    /// would close it as merged. Refused, with a way out that exists:
    /// their branch is no bookmark here, so "publish through it" is not
    /// one.
    #[test]
    fn a_bookmark_moved_onto_someone_elses_commits_is_refused_and_says_whose() {
        let (mut snap, prs) = pair_with_others(SyncState::Ahead);
        // Their commit, fetched, with `profile` moved up onto it. It is
        // under a local bookmark now, so it draws as part of the chain.
        let mut theirs = node("t1", "theirs: on top", NodeKind::Immutable, &["b1"], &["profile"]);
        theirs.held = true;
        theirs.commit_id = "c0ffee09".into();
        snap.nodes.insert(0, theirs);
        snap.nodes[1].bookmarks.clear();
        snap.bookmarks[2].target = "t1".into();
        let plan = plan_through(&snap, &prs, "profile", StackLink::Follow);
        assert_eq!(
            plan.blockers,
            ["publishing through \u{201c}profile\u{201d} would push \u{201c}profile\u{201d} onto \
              the commits of #9 by @kim (\u{201c}their-top\u{201d}), which is based on it, and \
              GitHub would close #9 by @kim as merged into \u{201c}profile\u{201d}. That pull \
              request is someone else's: its branch is not a bookmark here, and no publish \
              from here can move it out of the way. Move \u{201c}profile\u{201d} back off those \
              commits, or take that pull request over first (take-over 9) and publish through \
              it"]
        );
    }

    /// The same pull request from a fork whose branch has the name of a
    /// bookmark here. A fork's branch names nothing in this repository:
    /// the pull request is not the segment's own, and the push that
    /// would close it is refused all the same. (Found in review: it was
    /// skipped as the segment "auth"'s pull request.)
    #[test]
    fn a_pull_request_from_a_fork_is_never_taken_for_a_segments_by_its_branch_name() {
        let (mut snap, prs) = pair_with_others(SyncState::Ahead);
        let mut theirs = node("t1", "theirs: on top", NodeKind::Immutable, &["b1"], &["profile"]);
        theirs.held = true;
        theirs.commit_id = "c0ffee19".into();
        snap.nodes.insert(0, theirs);
        snap.nodes[1].bookmarks.clear();
        snap.bookmarks[2].target = "t1".into();
        let mut all = prs.report.prs.clone();
        all.retain(|pr| pr.number != 9 && pr.number != 10);
        all.push(PrSummary {
            head_owner: Some("a-fork".into()),
            ..their_pr(19, "auth", "profile", "kim", "c0ffee19")
        });
        let prs = pr_state(all, false);
        let plan = plan_through(&snap, &prs, "profile", StackLink::Follow);
        assert_eq!(plan.blockers.len(), 1, "{:?}", plan.blockers);
        assert!(
            plan.blockers[0].starts_with(
                "publishing through \u{201c}profile\u{201d} would push \u{201c}profile\u{201d} \
                 onto the commits of #19 by @kim (\u{201c}auth\u{201d}), which is based on it, \
                 and GitHub would close #19 by @kim as merged into \u{201c}profile\u{201d}."
            ),
            "{:?}",
            plan.blockers
        );
        // A fork's pull request cannot be taken over, so that way out is
        // not offered: the bookmark has to come back off its commits.
        assert!(
            plan.blockers[0].ends_with("Move \u{201c}profile\u{201d} back off those commits"),
            "{:?}",
            plan.blockers
        );

        // A fork can also call its branch what its base is called here.
        // Following bases through pull requests must not go round on
        // that (it did, and a publish that had nothing to do hung).
        let (snap, prs) = pair_with_others(SyncState::Diverged);
        let mut all = prs.report.prs.clone();
        all.push(PrSummary {
            head_owner: Some("a-fork".into()),
            ..their_pr(20, "profile", "profile", "sam", "c0ffee20")
        });
        let plan = plan_through(&snap, &pr_state(all, false), "profile", StackLink::Follow);
        assert!(plan.blockers.is_empty(), "{:?}", plan.blockers);
        assert!(
            plan.warnings.iter().any(|w| w.starts_with("#20 by @sam (\u{201c}profile\u{201d}) is someone else's")),
            "{:?}",
            plan.warnings
        );

        // Nor is a base followed through a fork's branch. #11 rests on
        // "auth", which this publish leaves alone. A fork's branch called
        // "auth", based on "profile", does not put #11 on "profile".
        let (snap, prs) = pair_with_others(SyncState::Diverged);
        let mut all = prs.report.prs.clone();
        all.retain(|pr| pr.number != 9 && pr.number != 10);
        all.push(their_pr(11, "their-mid", "auth", "lee", "c0ffee11"));
        all.push(PrSummary {
            head_owner: Some("a-fork".into()),
            ..their_pr(21, "auth", "profile", "sam", "c0ffee21")
        });
        let plan = plan_through(&snap, &pr_state(all, false), "profile", StackLink::Follow);
        assert!(plan.blockers.is_empty(), "{:?}", plan.blockers);
        let about: Vec<&String> =
            plan.warnings.iter().filter(|w| w.contains("someone else's")).collect();
        assert_eq!(about.len(), 1, "{:?}", plan.warnings);
        assert!(
            about[0].starts_with(
                "#21 by @sam (\u{201c}auth\u{201d}) is someone else's pull request on \
                 \u{201c}profile\u{201d}"
            ),
            "{:?}",
            plan.warnings
        );

        // And a fork's branch is not pushed from here, whatever it is
        // called. "auth" has no pull request and is pushed; a pull
        // request from a fork's "profile", based on it, is not at risk
        // because the "profile" of this repository is pushed too.
        let (mut snap, prs) = pair_with_others(SyncState::Diverged);
        snap.bookmarks[1].sync = SyncState::Ahead;
        let mut all = prs.report.prs.clone();
        all.retain(|pr| pr.number == 7);
        all.push(PrSummary {
            head_owner: Some("a-fork".into()),
            ..their_pr(22, "profile", "auth", "sam", "c0ffee22")
        });
        let plan = plan_through(&snap, &pr_state(all, false), "profile", StackLink::Follow);
        assert!(plan.blockers.is_empty(), "{:?}", plan.blockers);
    }

    /// The user rewrote a change someone else had built on, and a fetch
    /// brought the old commit back with their branch: the change has two
    /// commits, and the other one is theirs to keep. That must not stop
    /// the publish of the new one. A divergence between two commits of
    /// the user's own still does.
    #[test]
    fn an_old_copy_on_someone_elses_branch_does_not_block_the_publish() {
        let (mut snap, prs) = pair_with_others(SyncState::Diverged);
        snap.nodes[0].is_divergent = true;
        snap.nodes[0].old_copies_held = true;
        let plan = plan_through(&snap, &prs, "profile", StackLink::Follow);
        assert!(plan.blockers.is_empty(), "{:?}", plan.blockers);
        assert!(plan.actions.iter().any(
            |a| matches!(a, SubmitAction::Push { bookmark, .. } if bookmark == "profile")
        ));

        snap.nodes[0].old_copies_held = false;
        let plan = plan_through(&snap, &prs, "profile", StackLink::Follow);
        assert_eq!(plan.blockers, ["b1 is divergent; resolve the divergence first"]);
    }

    /// Their pull request was added to the GitHub stack above this chain.
    /// Dissolving the stack takes it out too, and the plan says so by
    /// number and author before it runs.
    #[test]
    fn dissolving_a_github_stack_names_someone_elses_pull_request_in_it() {
        let (snap, mut prs) = pair_with_others(SyncState::Synced);
        let seat = |position: u32| {
            Some(StackSeat {
                number: 90,
                position,
                size: 3,
                base: "main".into(),
            })
        };
        let mut report = prs.report.clone();
        report.prs.truncate(3);
        for (pr, position) in report.prs.iter_mut().zip(1..) {
            pr.stack = seat(position);
        }
        prs = RepoPrState::new(report, "o");
        let plan = plan_through(&snap, &prs, "profile", StackLink::Unlink);
        assert_eq!(retargets_and_pushes(&plan), ["unstack 90"]);
        assert!(
            plan.warnings.contains(
                &"dissolving GitHub stack 90 takes #9 by @kim out of it too: it stays open, \
                  as a plain pull request on the base it has now"
                    .to_owned()
            ),
            "{:?}",
            plan.warnings
        );
        // The step itself carries everyone it takes out, bottom first.
        let members = |plan: &SubmitPlan| -> Vec<u64> {
            plan.actions
                .iter()
                .find_map(|action| match action {
                    SubmitAction::Unstack { members, .. } => Some(members.clone()),
                    _ => None,
                })
                .unwrap()
        };
        assert_eq!(members(&plan), [1, 7, 9]);
        // So a plan confirmed while the stack had two members is not the
        // plan for the stack once someone has joined it: a host that
        // compares the two (the app does, before it runs one) refuses.
        // (Found in review: the two plans had the same steps, and only a
        // note told them apart.)
        let mut before = prs.report.clone();
        before.prs.truncate(2);
        for pr in &mut before.prs {
            pr.stack.as_mut().unwrap().size = 2;
        }
        let earlier = plan_through(&snap, &RepoPrState::new(before, "o"), "profile", StackLink::Unlink);
        assert_eq!(members(&earlier), [1, 7]);
        assert_ne!(earlier.actions, plan.actions);
        // A member from a fork is someone else's whatever its branch is
        // called. Called "auth", like the segment here, it is still said
        // to leave the stack: members are told apart by pull request.
        let mut forked = prs.report.clone();
        forked.prs[2].head_branch = "auth".into();
        forked.prs[2].head_owner = Some("a-fork".into());
        let from_a_fork =
            plan_through(&snap, &RepoPrState::new(forked, "o"), "profile", StackLink::Unlink);
        assert!(
            from_a_fork
                .warnings
                .iter()
                .any(|w| w.starts_with("dissolving GitHub stack 90 takes #9 by @kim out of it too")),
            "{:?}",
            from_a_fork.warnings
        );
        // Left as GitHub has it, nothing is planned and nothing said.
        let plan = plan_through(&snap, &prs, "profile", StackLink::Follow);
        assert!(retargets_and_pushes(&plan).is_empty(), "{:?}", plan.actions);
        assert!(!plan.warnings.iter().any(|w| w.contains("#9")), "{:?}", plan.warnings);
    }

    #[test]
    fn empty_segments_skip_but_still_base_the_stack() {
        let mut snap = stack_snapshot();
        snap.nodes[1].is_empty = true; // a2
        snap.nodes[2].is_empty = true; // a1 — auth's whole segment empty
        let prs = pr_state(vec![], true);
        let plan = plan_submit(&snap, &prs, &forge_repo(), &by_bookmark("profile"), &NoComments, None, &SubmitOptions::default()).unwrap();

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
        let plan = plan_submit(&snap, &prs, &forge_repo(), &by_bookmark("profile"), &NoComments, None, &SubmitOptions::default()).unwrap();
        assert!(
            plan.blockers.iter().any(|b| b.contains("is conflicted")),
            "{:?}",
            plan.blockers
        );

        let snap = stack_snapshot();
        let err = plan_submit(&snap, &prs, &forge_repo(), &by_bookmark("nope"), &NoComments, None, &SubmitOptions::default()).unwrap_err();
        assert_eq!(err.code(), "plan_failed");
        let err = plan_submit(&snap, &prs, &forge_repo(), &by_bookmark("main"), &NoComments, None, &SubmitOptions::default()).unwrap_err();
        assert!(err.to_string().contains("trunk"), "{err}");
        // A bookmark parked on immutable history has nothing to publish.
        let mut on_trunk = stack_snapshot();
        on_trunk.bookmarks.push(bookmark("old", "m", SyncState::LocalOnly, false));
        on_trunk.nodes[3].bookmarks.push("old".into());
        let err = plan_submit(&on_trunk, &prs, &forge_repo(), &by_bookmark("old"), &NoComments, None, &SubmitOptions::default()).unwrap_err();
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

        let plan = plan_submit(&snap, &prs, &forge_repo(), &by_bookmark("profile"), &NoComments, None, &SubmitOptions::default()).unwrap();
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

    /// trunk m ── c1 ── c2(@): an unbookmarked two-change stack — the
    /// derived-name flow's home turf.
    fn unbookmarked_snapshot() -> RepoSnapshot {
        snapshot(
            vec![
                node("c2", "Wire parser into loader", NodeKind::WorkingCopy, &["c1"], &[]),
                node("c1", "Add config parser\n\nReads TOML.", NodeKind::Mutable, &["m"], &[]),
                node("m", "release", NodeKind::Immutable, &[], &["main"]),
            ],
            vec![bookmark("main", "m", SyncState::Synced, true)],
        )
    }

    #[test]
    fn unbookmarked_head_derives_a_name_and_creates_the_bookmark() {
        let snap = unbookmarked_snapshot();
        let prs = pr_state(vec![], false);
        let plan =
            plan_submit(&snap, &prs, &forge_repo(), &by_change("c2"), &NoComments, None, &SubmitOptions::default()).unwrap();

        assert_eq!(plan.head_bookmark, "add-config-parser");
        assert!(plan.blockers.is_empty(), "{:?}", plan.blockers);
        let segs: Vec<(&str, &str, usize)> = plan
            .segments
            .iter()
            .map(|s| (s.bookmark.as_str(), s.base.as_str(), s.change_ids.len()))
            .collect();
        assert_eq!(segs, vec![("add-config-parser", "main", 2)]);
        // The name comes from the segment's bottom change — the same
        // change that titles the PR.
        assert_eq!(plan.segments[0].title, "Add config parser");
        assert_eq!(
            plan.actions[..2].to_vec(),
            vec![
                SubmitAction::CreateBookmark {
                    bookmark: "add-config-parser".into(),
                    change: "c2".into(),
                },
                SubmitAction::Push { bookmark: "add-config-parser".into(), create: true },
            ]
        );
        assert!(
            matches!(
                &plan.actions[2],
                SubmitAction::CreatePr { bookmark, base, .. }
                    if bookmark == "add-config-parser" && base == "main"
            ),
            "{:?}",
            plan.actions
        );
    }

    #[test]
    fn derived_names_step_around_taken_names() {
        let mut snap = unbookmarked_snapshot();
        // The bare name is a local bookmark elsewhere; the first suffix is
        // an open PR's head branch.
        snap.bookmarks
            .push(bookmark("add-config-parser", "m", SyncState::Synced, false));
        let prs = pr_state(vec![open_pr(9, "add-config-parser-2", "main")], false);
        let plan =
            plan_submit(&snap, &prs, &forge_repo(), &by_change("c2"), &NoComments, None, &SubmitOptions::default()).unwrap();
        assert_eq!(plan.head_bookmark, "add-config-parser-3");
    }

    #[test]
    fn change_head_with_a_bookmark_is_the_classic_plan() {
        let snap = stack_snapshot();
        let prs = pr_state(vec![open_pr(7, "profile", "auth")], false);
        let by_name =
            plan_submit(&snap, &prs, &forge_repo(), &by_bookmark("profile"), &NoComments, None, &SubmitOptions::default())
                .unwrap();
        let by_head =
            plan_submit(&snap, &prs, &forge_repo(), &by_change("b1"), &NoComments, None, &SubmitOptions::default()).unwrap();
        assert_eq!(by_name, by_head);
        assert!(
            by_head
                .actions
                .iter()
                .all(|a| !matches!(a, SubmitAction::CreateBookmark { .. })),
            "{:?}",
            by_head.actions
        );
    }

    #[test]
    fn unbookmarked_head_above_bookmarks_stacks_on_them() {
        let mut snap = stack_snapshot();
        // New work above the bookmarked head: publishing it derives a name
        // for just the top run and stacks its PR on “profile”.
        snap.nodes.insert(
            0,
            node("t1", "polish: avatar cropping", NodeKind::Mutable, &["b1"], &[]),
        );
        let prs = pr_state(vec![], false);
        let plan =
            plan_submit(&snap, &prs, &forge_repo(), &by_change("t1"), &NoComments, None, &SubmitOptions::default()).unwrap();
        let segs: Vec<(&str, &str)> = plan
            .segments
            .iter()
            .map(|s| (s.bookmark.as_str(), s.base.as_str()))
            .collect();
        assert_eq!(
            segs,
            vec![
                ("auth", "main"),
                ("profile", "auth"),
                ("polish-avatar-cropping", "profile"),
            ]
        );
        assert!(
            plan.actions.contains(&SubmitAction::CreateBookmark {
                bookmark: "polish-avatar-cropping".into(),
                change: "t1".into(),
            }),
            "{:?}",
            plan.actions
        );
    }

    #[test]
    fn unknown_and_immutable_change_heads_refuse() {
        let snap = unbookmarked_snapshot();
        let prs = pr_state(vec![], false);
        let err = plan_submit(&snap, &prs, &forge_repo(), &by_change("zz"), &NoComments, None, &SubmitOptions::default())
            .unwrap_err();
        assert!(err.to_string().contains("no change zz"), "{err}");
        let err = plan_submit(&snap, &prs, &forge_repo(), &by_change("m"), &NoComments, None, &SubmitOptions::default())
            .unwrap_err();
        assert!(err.to_string().contains("immutable"), "{err}");
    }

    #[test]
    fn undescribed_derived_head_blocks_and_names_by_change_id() {
        let mut snap = unbookmarked_snapshot();
        snap.nodes[1].description = String::new();
        let prs = pr_state(vec![], false);
        let plan =
            plan_submit(&snap, &prs, &forge_repo(), &by_change("c2"), &NoComments, None, &SubmitOptions::default()).unwrap();
        // Nothing slugs from an empty description: jj's own push-<change
        // id> convention names the branch, and the undescribed change is
        // already a blocker so nothing runs until it is described.
        assert_eq!(plan.head_bookmark, "push-c1");
        assert!(
            plan.blockers.iter().any(|b| b.contains("no description")),
            "{:?}",
            plan.blockers
        );
    }

    #[derive(Default)]
    struct StubVcs {
        calls: RefCell<Vec<(Vec<String>, String)>>,
        created: RefCell<Vec<(String, String)>>,
        fail: bool,
        fail_create_bookmark: bool,
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

        fn create_bookmark(&self, name: &str, change: &str) -> Result<String, BackendError> {
            if self.fail_create_bookmark {
                return Err(BackendError::MutationFailed("name taken".into()));
            }
            self.created
                .borrow_mut()
                .push((name.to_owned(), change.to_owned()));
            Ok(format!("Created bookmark {name} at {change}"))
        }
    }

    #[derive(Default)]
    struct StubForge {
        log: RefCell<Vec<String>>,
        fail_create: Option<String>,
        comments: RefCell<HashMap<u64, ExistingComment>>,
    }

    impl StackCommentSource for StubForge {
        fn stack_comment(&self, number: u64) -> Result<Option<ExistingComment>, ForgeError> {
            Ok(self.comments.borrow().get(&number).cloned())
        }
    }

    impl SubmitForge for StubForge {
        fn create_pr(
            &self,
            title: &str,
            _body: &str,
            head: &str,
            base: &str,
            draft: bool,
        ) -> Result<PrSummary, ForgeError> {
            if self.fail_create.as_deref() == Some(head) {
                return Err(ForgeError::Api("boom".into()));
            }
            let kind = if draft { "draft " } else { "" };
            self.log.borrow_mut().push(format!("create {kind}{head}->{base}: {title}"));
            Ok(PrSummary { is_draft: draft, ..open_pr(42, head, base) })
        }

        fn update_pr_base(&self, number: u64, base: &str) -> Result<(), ForgeError> {
            self.log.borrow_mut().push(format!("retarget #{number}->{base}"));
            Ok(())
        }

        fn update_pr_text(
            &self,
            number: u64,
            title: Option<&str>,
            _body: &str,
        ) -> Result<(), ForgeError> {
            self.log.borrow_mut().push(format!(
                "text #{number} title:{}",
                title.unwrap_or("unchanged")
            ));
            Ok(())
        }

        fn create_comment(&self, number: u64, body: &str) -> Result<(), ForgeError> {
            self.log.borrow_mut().push(format!("comment-create #{number}"));
            self.comments.borrow_mut().insert(
                number,
                ExistingComment { id: number * 10, body: body.to_owned() },
            );
            Ok(())
        }

        fn update_comment(&self, comment_id: u64, _body: &str) -> Result<(), ForgeError> {
            self.log
                .borrow_mut()
                .push(format!("comment-update id:{comment_id}"));
            Ok(())
        }

        fn create_stack(&self, pull_requests: &[u64]) -> Result<u64, ForgeError> {
            self.log.borrow_mut().push(format!("stack {pull_requests:?}"));
            Ok(90)
        }

        fn add_to_stack(&self, stack: u64, pull_requests: &[u64]) -> Result<(), ForgeError> {
            self.log
                .borrow_mut()
                .push(format!("stack {stack} += {pull_requests:?}"));
            Ok(())
        }

        fn unstack(&self, stack: u64) -> Result<Vec<u64>, ForgeError> {
            self.log.borrow_mut().push(format!("unstack {stack}"));
            Ok(Vec::new())
        }
    }

    fn plan_with_all_action_kinds() -> SubmitPlan {
        let mut snap = stack_snapshot();
        snap.bookmarks[1].target = "a2".into();
        snap.nodes[1].bookmarks = vec!["auth".into()];
        snap.nodes[2].bookmarks = vec![];
        let prs = pr_state(vec![open_pr(7, "profile", "main")], false);
        plan_submit(&snap, &prs, &forge_repo(), &by_bookmark("profile"), &NoComments, None, &SubmitOptions::default()).unwrap()
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
        // The comment on `auth` resolves the number from the PR the create
        // step just opened (#42).
        assert_eq!(
            forge.log.borrow().as_slice(),
            &[
                "create auth->main: auth: login flow".to_owned(),
                "retarget #7->auth".to_owned(),
                "text #7 title:unchanged".to_owned(),
                "comment-create #42".to_owned(),
                "comment-create #7".to_owned(),
            ]
        );
        let created = outcome
            .steps
            .iter()
            .find(|s| matches!(s.action, SubmitAction::CreatePr { .. }))
            .unwrap();
        assert_eq!(created.pr.as_ref().unwrap().number, 42);
        // Both comments list the whole stack, with their own PR bolded and
        // the created PR's real link filled in.
        let comments = forge.comments.borrow();
        let on_created = &comments[&42].body;
        assert!(on_created.contains("**`auth` ← this PR**"), "{on_created}");
        assert!(on_created.contains("[`profile`](https://github.com/o/r/pull/7)"));
        let on_existing = &comments[&7].body;
        assert!(on_existing.contains("[`auth`](https://github.com/o/r/pull/42)"));
        assert!(on_existing.contains("**`profile` ← this PR**"));
    }

    #[test]
    fn draft_plans_open_new_prs_as_drafts_and_leave_existing_ones_alone() {
        let mut plan = plan_with_all_action_kinds();
        plan.open_new_prs_as_drafts();
        // #7 already exists and is open for review; it is noted, not converted.
        assert!(
            plan.warnings.iter().any(|w| w.starts_with("#7 (profile)")),
            "{:?}",
            plan.warnings
        );
        let forge = StubForge::default();
        let outcome = execute_submit(&plan, &StubVcs::default(), &forge).unwrap();
        assert_eq!(forge.log.borrow()[0], "create draft auth->main: auth: login flow");
        let created = outcome
            .steps
            .iter()
            .find(|s| matches!(s.action, SubmitAction::CreatePr { .. }))
            .unwrap();
        assert_eq!(created.detail.as_deref(), Some("Opened draft #42 for auth"));
        assert!(created.pr.as_ref().unwrap().is_draft);
    }

    #[test]
    fn comment_steps_merge_fossils_and_skip_current_comments() {
        // The plan syncs one existing PR's comment (no creations pending).
        let (snap, prs, _) = consistent_stack();
        let previous_with_fossil = {
            let entries = vec![
                crate::comment::StackEntry {
                    bookmark: "landed".into(),
                    number: Some(4),
                    url: Some("https://github.com/o/r/pull/4".into()),
                },
                crate::comment::StackEntry {
                    bookmark: "auth".into(),
                    number: Some(6),
                    url: Some("https://github.com/o/r/pull/6".into()),
                },
            ];
            render_stack_comment(&entries, &[], Some("auth"))
        };
        let comments = StubComments(HashMap::from([(
            6,
            ExistingComment { id: 60, body: previous_with_fossil.clone() },
        )]));
        let plan = plan_submit(&snap, &prs, &forge_repo(), &by_bookmark("profile"), &comments, None, &SubmitOptions::default()).unwrap();

        let forge = StubForge::default();
        forge.comments.borrow_mut().insert(
            6,
            ExistingComment { id: 60, body: previous_with_fossil },
        );
        let vcs = StubVcs::default();
        let outcome = execute_submit(&plan, &vcs, &forge).unwrap();
        assert!(!outcome.failed);
        // `landed` left the local stack; it survives struck-through, and
        // #7's missing comment is posted alongside the #6 edit.
        let log = forge.log.borrow();
        assert!(log.iter().any(|l| l == "comment-update id:60"), "{log:?}");
        assert!(log.iter().any(|l| l == "comment-create #7"), "{log:?}");
        let update_step = outcome
            .steps
            .iter()
            .find(|s| {
                matches!(s.action, SubmitAction::SyncStackComment { number: Some(6), .. })
            })
            .unwrap();
        assert!(update_step.detail.as_deref().unwrap().contains("Updated"));

        // Re-running against the now-current comments plans nothing.
        let fresh = StubComments(forge.comments.borrow().clone());
        // The #6 update above logged but did not store; store it like the
        // real forge would so the replan sees the written body.
        let replan_comments = {
            let mut map = fresh.0;
            let entries = vec![
                crate::comment::StackEntry {
                    bookmark: "auth".into(),
                    number: Some(6),
                    url: Some("https://github.com/o/r/pull/6".into()),
                },
                crate::comment::StackEntry {
                    bookmark: "profile".into(),
                    number: Some(7),
                    url: Some("https://github.com/o/r/pull/7".into()),
                },
            ];
            let fossils = vec![crate::comment::StackDataItem {
                bookmark: "landed".into(),
                number: 4,
                url: "https://github.com/o/r/pull/4".into(),
            }];
            map.insert(
                6,
                ExistingComment {
                    id: 60,
                    body: render_stack_comment(&entries, &fossils, Some("auth")),
                },
            );
            StubComments(map)
        };
        let plan =
            plan_submit(&snap, &prs, &forge_repo(), &by_bookmark("profile"), &replan_comments, None, &SubmitOptions::default()).unwrap();
        assert!(plan.actions.is_empty(), "{:?}", plan.actions);
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

        // A PR creation fails: the pushes stay done, everything after —
        // retarget, text update, both comments — skips.
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
                SubmitStepStatus::Skipped,
                SubmitStepStatus::Skipped,
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
    fn execute_creates_the_derived_bookmark_before_the_push() {
        let snap = unbookmarked_snapshot();
        let prs = pr_state(vec![], false);
        let plan =
            plan_submit(&snap, &prs, &forge_repo(), &by_change("c2"), &NoComments, None, &SubmitOptions::default()).unwrap();
        let vcs = StubVcs::default();
        let forge = StubForge::default();
        let outcome = execute_submit(&plan, &vcs, &forge).unwrap();
        assert!(!outcome.failed);
        assert_eq!(
            *vcs.created.borrow(),
            vec![("add-config-parser".to_owned(), "c2".to_owned())]
        );
        assert_eq!(vcs.calls.borrow().len(), 1, "one batched push");
        assert!(
            outcome.steps.iter().all(|s| s.status == SubmitStepStatus::Done),
            "{:?}",
            outcome.steps
        );
    }

    #[test]
    fn a_failed_bookmark_creation_skips_everything_after() {
        let snap = unbookmarked_snapshot();
        let prs = pr_state(vec![], false);
        let plan =
            plan_submit(&snap, &prs, &forge_repo(), &by_change("c2"), &NoComments, None, &SubmitOptions::default()).unwrap();
        let vcs = StubVcs { fail_create_bookmark: true, ..Default::default() };
        let forge = StubForge::default();
        let outcome = execute_submit(&plan, &vcs, &forge).unwrap();
        assert!(outcome.failed);
        assert_eq!(outcome.steps[0].status, SubmitStepStatus::Failed);
        assert!(outcome.steps[0].detail.as_deref().unwrap().contains("name taken"));
        for step in &outcome.steps[1..] {
            assert_eq!(step.status, SubmitStepStatus::Skipped, "{:?}", step.action);
        }
        assert!(vcs.calls.borrow().is_empty(), "the push never ran");
        assert!(forge.log.borrow().is_empty(), "no forge writes ran");
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
    // ---- The order of a publish, against GitHub's own rules ----

    #[derive(Clone)]
    struct ModelPr {
        number: u64,
        head: String,
        base: String,
        open: bool,
        merged: bool,
        stack: Option<u64>,
    }

    /// GitHub and the branches on it, as far as a publish can tell, with
    /// the rules seen on the real thing: a pull request whose head is
    /// reachable from its base branch is closed as merged the moment a
    /// push makes it so; a pull request in a stack cannot be retargeted; a
    /// stack takes two or more open pull requests whose bases chain, and
    /// grows at the top only. It is the remote of `local` too: a push
    /// moves branches to where the local bookmarks are.
    struct ModelGitHub<'a> {
        local: &'a RepoSnapshot,
        branches: RefCell<HashMap<String, String>>,
        prs: RefCell<Vec<ModelPr>>,
        stacks: RefCell<u64>,
        log: RefCell<Vec<String>>,
        /// Pull requests GitHub will not take out of a stack (its
        /// documentation names one queued to merge). Not seen on the
        /// sandbox, which has no merge queue.
        queued: RefCell<Vec<u64>>,
        /// Branches the remote refuses to update. A push of several
        /// branches is not atomic: the others still move.
        refused: RefCell<Vec<String>>,
        /// A repository, or a host, without GitHub's stacked pull
        /// requests: the stacks endpoints answer a bare 404 there.
        no_stacks: std::cell::Cell<bool>,
    }

    impl<'a> ModelGitHub<'a> {
        /// `branches` are where the remote's branches stand before the
        /// publish (ids outside the local graph are commits that have
        /// since been rewritten); `prs` are `(number, head, base)`.
        fn new(local: &'a RepoSnapshot, branches: &[(&str, &str)], prs: &[(u64, &str, &str)]) -> Self {
            Self {
                local,
                branches: RefCell::new(
                    branches.iter().map(|(b, at)| (b.to_string(), at.to_string())).collect(),
                ),
                prs: RefCell::new(
                    prs.iter()
                        .map(|(number, head, base)| ModelPr {
                            number: *number,
                            head: head.to_string(),
                            base: base.to_string(),
                            open: true,
                            merged: false,
                            stack: None,
                        })
                        .collect(),
                ),
                stacks: RefCell::new(89),
                log: RefCell::new(vec![]),
                queued: RefCell::new(vec![]),
                refused: RefCell::new(vec![]),
                no_stacks: std::cell::Cell::new(false),
            }
        }

        /// What the real client makes of the stacks endpoints' 404.
        fn stacks_offered(&self) -> Result<(), ForgeError> {
            if self.no_stacks.get() {
                return Err(ForgeError::NoStacks(
                    "acme/widgets on github.example does not offer GitHub's stacked pull requests"
                        .into(),
                ));
            }
            Ok(())
        }

        fn stacked(self, numbers: &[u64]) -> Self {
            *self.stacks.borrow_mut() += 1;
            let stack = *self.stacks.borrow();
            for pr in self.prs.borrow_mut().iter_mut() {
                if numbers.contains(&pr.number) {
                    pr.stack = Some(stack);
                }
            }
            self
        }

        /// Whether commit `ancestor` is `descendant` or one of its
        /// ancestors, in the local graph.
        fn reachable(&self, ancestor: &str, descendant: &str) -> bool {
            let mut queue = vec![descendant.to_owned()];
            while let Some(id) = queue.pop() {
                if id == ancestor {
                    return true;
                }
                if let Some(node) = self.local.nodes.iter().find(|n| n.id == id) {
                    queue.extend(node.parents.iter().cloned());
                }
            }
            false
        }

        /// GitHub's rule, applied after every push.
        fn close_what_is_inside_its_base(&self) {
            let branches = self.branches.borrow();
            for pr in self.prs.borrow_mut().iter_mut().filter(|pr| pr.open) {
                let (Some(head), Some(base)) = (branches.get(&pr.head), branches.get(&pr.base))
                else {
                    continue;
                };
                if self.reachable(head, base) {
                    pr.open = false;
                    pr.merged = true;
                    self.log
                        .borrow_mut()
                        .push(format!("#{} closed as merged into {}", pr.number, pr.base));
                }
            }
        }

        fn pr(&self, number: u64) -> ModelPr {
            self.prs
                .borrow()
                .iter()
                .find(|pr| pr.number == number)
                .cloned()
                .expect("the model has this pull request")
        }

        fn stack_of(&self, stack: u64) -> Vec<u64> {
            self.prs
                .borrow()
                .iter()
                .filter(|pr| pr.stack == Some(stack))
                .map(|pr| pr.number)
                .collect()
        }

        fn refuse(message: &str) -> ForgeError {
            ForgeError::Api(format!("HTTP 422: {message}"))
        }
    }

    impl SubmitVcs for ModelGitHub<'_> {
        fn push_bookmarks(&self, bookmarks: &[String], _remote: &str) -> Result<String, BackendError> {
            let refused = self.refused.borrow();
            let moved: Vec<&str> = bookmarks
                .iter()
                .map(String::as_str)
                .filter(|name| !refused.iter().any(|r| r == name))
                .collect();
            for name in &moved {
                let target = self
                    .local
                    .bookmarks
                    .iter()
                    .find(|b| b.name == *name)
                    .map(|b| b.target.clone())
                    .expect("pushed bookmarks exist locally");
                self.branches.borrow_mut().insert((*name).to_owned(), target);
            }
            self.log.borrow_mut().push(format!("push {}", moved.join(",")));
            self.close_what_is_inside_its_base();
            if moved.len() < bookmarks.len() {
                return Err(BackendError::MutationFailed(format!(
                    "the remote refused {}",
                    refused.join(", ")
                )));
            }
            Ok("Pushed".into())
        }

        fn create_bookmark(&self, name: &str, _change: &str) -> Result<String, BackendError> {
            Ok(format!("Created bookmark {name}"))
        }
    }

    impl StackCommentSource for ModelGitHub<'_> {
        fn stack_comment(&self, _number: u64) -> Result<Option<ExistingComment>, ForgeError> {
            Ok(None)
        }

        fn open_prs_based_on(&self, base: &str) -> Result<Vec<PrSummary>, ForgeError> {
            self.log.borrow_mut().push(format!("list open on {base}"));
            Ok(model_prs(self)
                .report
                .prs
                .into_iter()
                .filter(|pr| pr.base_branch == base)
                .collect())
        }
    }

    impl SubmitForge for ModelGitHub<'_> {
        fn create_pr(
            &self,
            _title: &str,
            _body: &str,
            head: &str,
            base: &str,
            _draft: bool,
        ) -> Result<PrSummary, ForgeError> {
            let mut prs = self.prs.borrow_mut();
            let number = prs.iter().map(|pr| pr.number).max().unwrap_or(0) + 1;
            prs.push(ModelPr {
                number,
                head: head.into(),
                base: base.into(),
                open: true,
                merged: false,
                stack: None,
            });
            self.log.borrow_mut().push(format!("open #{number} {head}->{base}"));
            Ok(open_pr(number, head, base))
        }

        fn update_pr_base(&self, number: u64, base: &str) -> Result<(), ForgeError> {
            let mut prs = self.prs.borrow_mut();
            let pr = prs.iter_mut().find(|pr| pr.number == number).expect("a known PR");
            if !pr.open {
                return Err(Self::refuse("Cannot change the base branch of a closed pull request."));
            }
            if pr.stack.is_some() {
                return Err(Self::refuse(
                    "Cannot change the base branch because the pull request is part of a stack.",
                ));
            }
            pr.base = base.into();
            self.log.borrow_mut().push(format!("retarget #{number}->{base}"));
            Ok(())
        }

        fn update_pr_text(&self, _: u64, _: Option<&str>, _: &str) -> Result<(), ForgeError> {
            Ok(())
        }

        fn create_comment(&self, _: u64, _: &str) -> Result<(), ForgeError> {
            Ok(())
        }

        fn update_comment(&self, _: u64, _: &str) -> Result<(), ForgeError> {
            Ok(())
        }

        fn create_stack(&self, pull_requests: &[u64]) -> Result<u64, ForgeError> {
            self.stacks_offered()?;
            if pull_requests.len() < 2 {
                return Err(Self::refuse("2 items required"));
            }
            let members: Vec<ModelPr> = pull_requests.iter().map(|n| self.pr(*n)).collect();
            if members.iter().any(|pr| pr.stack.is_some()) {
                return Err(Self::refuse("Pull requests are already part of a stack"));
            }
            if members.iter().any(|pr| !pr.open)
                || members.windows(2).any(|pair| pair[1].base != pair[0].head)
            {
                return Err(Self::refuse(
                    "Pull requests must form a stack, where each PR's base ref is the previous PR's head ref",
                ));
            }
            *self.stacks.borrow_mut() += 1;
            let stack = *self.stacks.borrow();
            for pr in self.prs.borrow_mut().iter_mut() {
                if pull_requests.contains(&pr.number) {
                    pr.stack = Some(stack);
                }
            }
            self.log.borrow_mut().push(format!("stack {stack} = {pull_requests:?}"));
            Ok(stack)
        }

        fn add_to_stack(&self, stack: u64, pull_requests: &[u64]) -> Result<(), ForgeError> {
            let top = self
                .prs
                .borrow()
                .iter()
                .rfind(|pr| pr.stack == Some(stack))
                .cloned()
                .ok_or_else(|| ForgeError::NotFound("Not Found".into()))?;
            let mut below = top.head;
            for number in pull_requests {
                let pr = self.pr(*number);
                if pr.stack.is_some() || !pr.open || pr.base != below {
                    return Err(Self::refuse("the pull requests can't be appended to the stack"));
                }
                below = pr.head;
            }
            for pr in self.prs.borrow_mut().iter_mut() {
                if pull_requests.contains(&pr.number) {
                    pr.stack = Some(stack);
                }
            }
            self.log.borrow_mut().push(format!("stack {stack} += {pull_requests:?}"));
            Ok(())
        }

        fn unstack(&self, stack: u64) -> Result<Vec<u64>, ForgeError> {
            let queued = self.queued.borrow();
            let mut kept = Vec::new();
            for pr in self.prs.borrow_mut().iter_mut() {
                if pr.stack == Some(stack) && pr.open {
                    if queued.contains(&pr.number) {
                        kept.push(pr.number);
                    } else {
                        pr.stack = None;
                    }
                }
            }
            self.log.borrow_mut().push(format!("unstack {stack}"));
            Ok(kept)
        }
    }

    /// The model's pull requests as the open-PR query would answer them.
    fn model_prs(model: &ModelGitHub<'_>) -> RepoPrState {
        let prs = model.prs.borrow();
        let seats: Vec<Option<StackSeat>> = prs
            .iter()
            .map(|pr| {
                let stack = pr.stack?;
                let members: Vec<&ModelPr> =
                    prs.iter().filter(|other| other.stack == Some(stack)).collect();
                Some(StackSeat {
                    number: stack,
                    position: members.iter().position(|m| m.number == pr.number)? as u32 + 1,
                    size: members.len() as u32,
                    base: "main".into(),
                })
            })
            .collect();
        // Each pull request's head is the commit its branch stands on.
        let branches = model.branches.borrow();
        pr_state(
            prs.iter()
                .zip(seats)
                .filter(|(pr, _)| pr.open)
                .map(|(pr, stack)| PrSummary {
                    stack,
                    head_commit: branches
                        .get(&pr.head)
                        .map_or_else(|| "feedface".to_owned(), |at| format!("c{at}")),
                    ..open_pr(pr.number, &pr.head, &pr.base)
                })
                .collect(),
            false,
        )
    }

    /// main(m) ── b1 (profile) ── a1 (auth, @): the two-PR stack after
    /// its changes swapped places. Both branches have to be pushed.
    fn swapped_snapshot() -> RepoSnapshot {
        snapshot(
            vec![
                node("a1", "auth: login", NodeKind::WorkingCopy, &["b1"], &["auth"]),
                node("b1", "profile: avatars", NodeKind::Mutable, &["m"], &["profile"]),
                node("m", "release", NodeKind::Immutable, &[], &["main"]),
            ],
            vec![
                bookmark("main", "m", SyncState::Synced, true),
                bookmark("auth", "a1", SyncState::Diverged, false),
                bookmark("profile", "b1", SyncState::Diverged, false),
            ],
        )
    }

    fn retargets_and_pushes(plan: &SubmitPlan) -> Vec<String> {
        plan.actions
            .iter()
            .filter_map(|action| match action {
                SubmitAction::RetargetPr { number, to_base, .. } => {
                    Some(format!("retarget #{number}->{to_base}"))
                }
                SubmitAction::Push { bookmark, .. } => Some(format!("push {bookmark}")),
                SubmitAction::Unstack { stack, .. } => Some(format!("unstack {stack}")),
                SubmitAction::LinkStack {
                    bookmarks,
                    onto,
                    above,
                } if above.is_empty() => Some(format!("link {bookmarks:?} onto {onto:?}")),
                SubmitAction::LinkStack {
                    bookmarks,
                    onto,
                    above,
                } => Some(format!("link {bookmarks:?} onto {onto:?} with {above:?} above")),
                SubmitAction::CreatePr { bookmark, base, .. } => {
                    Some(format!("open {bookmark}->{base}"))
                }
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_reorder_never_leaves_a_pull_request_inside_its_base() {
        // Reproduced on GitHub: #1 auth -> main and #7 profile -> auth,
        // then profile moved below auth. Pushed with #7 still based on
        // auth, GitHub closed #7 as merged within two seconds: auth's new
        // commit contains profile's.
        let local = swapped_snapshot();
        let github = ModelGitHub::new(
            &local,
            &[("main", "m"), ("auth", "old-a"), ("profile", "old-b")],
            &[(1, "auth", "main"), (7, "profile", "auth")],
        );
        let plan = plan_submit(
            &local,
            &model_prs(&github),
            &forge_repo(),
            &by_bookmark("auth"),
            &github,
            None,
            &SubmitOptions::default(),
        )
        .unwrap();
        // #7 moves to trunk before anything is pushed (and stays there:
        // trunk is its new base); #1 moves onto profile afterwards.
        assert_eq!(
            retargets_and_pushes(&plan),
            ["retarget #7->main", "push profile", "push auth", "retarget #1->profile"]
        );

        let outcome = execute_submit(&plan, &github, &github).unwrap();
        assert!(!outcome.failed, "{:?}", outcome.steps);
        assert!(github.pr(7).open && github.pr(1).open, "{:?}", github.log);
        assert_eq!(github.pr(7).base, "main");
        assert_eq!(github.pr(1).base, "profile");

        // The order this replaced, on the same GitHub: push, then retarget.
        let github = ModelGitHub::new(
            &local,
            &[("main", "m"), ("auth", "old-a"), ("profile", "old-b")],
            &[(1, "auth", "main"), (7, "profile", "auth")],
        );
        github
            .push_bookmarks(&["profile".to_owned(), "auth".to_owned()], "origin")
            .unwrap();
        assert!(github.pr(7).merged, "the model closes it, as GitHub did");
        assert!(github.update_pr_base(7, "main").is_err());
    }

    #[test]
    fn a_base_that_moves_between_branches_parks_on_trunk_in_between() {
        // main ── a (#1) ── b (#2) ── c (#3) became main ── a ── c ── b.
        let local = snapshot(
            vec![
                node("b1", "b", NodeKind::WorkingCopy, &["c1"], &["b"]),
                node("c1", "c", NodeKind::Mutable, &["a1"], &["c"]),
                node("a1", "a", NodeKind::Mutable, &["m"], &["a"]),
                node("m", "release", NodeKind::Immutable, &[], &["main"]),
            ],
            vec![
                bookmark("main", "m", SyncState::Synced, true),
                bookmark("a", "a1", SyncState::Synced, false),
                bookmark("b", "b1", SyncState::Diverged, false),
                bookmark("c", "c1", SyncState::Diverged, false),
            ],
        );
        let github = ModelGitHub::new(
            &local,
            &[("main", "m"), ("a", "a1"), ("b", "old-b"), ("c", "old-c")],
            &[(1, "a", "main"), (2, "b", "a"), (3, "c", "b")],
        );
        let plan = plan_submit(
            &local,
            &model_prs(&github),
            &forge_repo(),
            &by_bookmark("b"),
            &github,
            None,
            &SubmitOptions::default(),
        )
        .unwrap();
        assert_eq!(
            retargets_and_pushes(&plan),
            [
                "retarget #3->main",
                "retarget #2->main",
                "push c",
                "push b",
                "retarget #3->a",
                "retarget #2->c",
            ]
        );
        assert!(
            plan.warnings.iter().any(|w| w.contains("while the branches are pushed")),
            "{:?}",
            plan.warnings
        );
        let outcome = execute_submit(&plan, &github, &github).unwrap();
        assert!(!outcome.failed, "{:?}", outcome.steps);
        assert!((1..=3).all(|n| github.pr(n).open), "{:?}", github.log);
        assert_eq!(
            (github.pr(2).base, github.pr(3).base),
            ("c".to_owned(), "a".to_owned())
        );
    }

    #[test]
    fn a_retarget_with_nothing_to_push_goes_straight_to_its_base() {
        // Same shape on both sides, one base wrong on GitHub: no branch
        // moves, so nothing can end up inside anything.
        let local = synced_pair();
        let prs = pr_state(
            vec![open_pr(1, "auth", "main"), open_pr(7, "profile", "stale")],
            false,
        );
        let plan = plan_submit(
            &local,
            &prs,
            &forge_repo(),
            &by_bookmark("profile"),
            &NoComments,
            None,
            &SubmitOptions::default(),
        )
        .unwrap();
        assert_eq!(retargets_and_pushes(&plan), ["retarget #7->auth"]);
    }

    // ---- GitHub's own stacks ----

    /// main(m) ── a1 (auth) ── b1 (profile, @), both pushed and in sync.
    fn synced_pair() -> RepoSnapshot {
        snapshot(
            vec![
                node("b1", "profile: avatars", NodeKind::WorkingCopy, &["a1"], &["profile"]),
                node("a1", "auth: login", NodeKind::Mutable, &["m"], &["auth"]),
                node("m", "release", NodeKind::Immutable, &[], &["main"]),
            ],
            vec![
                bookmark("main", "m", SyncState::Synced, true),
                bookmark("auth", "a1", SyncState::Synced, false),
                bookmark("profile", "b1", SyncState::Synced, false),
            ],
        )
    }

    fn plan_on(
        local: &RepoSnapshot,
        github: &ModelGitHub<'_>,
        head: &str,
        stack: StackLink,
    ) -> SubmitPlan {
        plan_submit(
            local,
            &model_prs(github),
            &forge_repo(),
            &by_bookmark(head),
            github,
            None,
            &SubmitOptions { stack },
        )
        .unwrap()
    }

    #[test]
    fn a_chain_becomes_a_github_stack_only_when_asked() {
        let local = synced_pair();
        let branches = [("main", "m"), ("auth", "a1"), ("profile", "b1")];
        let chain = [(1, "auth", "main"), (7, "profile", "auth")];

        // Left alone, a plain chain stays one.
        let github = ModelGitHub::new(&local, &branches, &chain);
        let plan = plan_on(&local, &github, "profile", StackLink::Follow);
        assert!(retargets_and_pushes(&plan).is_empty(), "{:?}", plan.actions);

        // Asked: linked, as the last step, bottom first.
        let plan = plan_on(&local, &github, "profile", StackLink::Link);
        assert_eq!(
            plan.actions.last(),
            Some(&SubmitAction::LinkStack {
                bookmarks: vec!["auth".into(), "profile".into()],
                onto: None,
                above: vec![],
            })
        );
        let outcome = execute_submit(&plan, &github, &github).unwrap();
        assert!(!outcome.failed, "{:?}", outcome.steps);
        assert_eq!(github.stack_of(90), [1, 7]);
        assert_eq!(
            outcome.steps.last().unwrap().detail.as_deref(),
            Some("Linked #1 and #7 as GitHub stack 90")
        );

        // From then on nothing needs asking, and nothing needs doing.
        let plan = plan_on(&local, &github, "profile", StackLink::Follow);
        assert!(retargets_and_pushes(&plan).is_empty(), "{:?}", plan.actions);
        let plan = plan_on(&local, &github, "profile", StackLink::Link);
        assert!(retargets_and_pushes(&plan).is_empty(), "{:?}", plan.actions);

        // Asked to let go: the stack is dissolved, the chain stays.
        let plan = plan_on(&local, &github, "profile", StackLink::Unlink);
        assert_eq!(retargets_and_pushes(&plan), ["unstack 90"]);
        execute_submit(&plan, &github, &github).unwrap();
        assert!(github.stack_of(90).is_empty());
        assert_eq!(github.pr(7).base, "auth");
    }

    /// main(m) ── a1 (auth) ── b1 (profile, @), neither pushed yet.
    fn unpublished_pair() -> RepoSnapshot {
        snapshot(
            vec![
                node("b1", "profile: avatars", NodeKind::WorkingCopy, &["a1"], &["profile"]),
                node("a1", "auth: login", NodeKind::Mutable, &["m"], &["auth"]),
                node("m", "release", NodeKind::Immutable, &[], &["main"]),
            ],
            vec![
                bookmark("main", "m", SyncState::Synced, true),
                bookmark("auth", "a1", SyncState::LocalOnly, false),
                bookmark("profile", "b1", SyncState::LocalOnly, false),
            ],
        )
    }

    fn link_steps(plan: &SubmitPlan) -> Vec<String> {
        retargets_and_pushes(plan)
            .into_iter()
            .filter(|step| step.starts_with("link") || step.starts_with("unstack"))
            .collect()
    }

    #[test]
    fn the_setting_stands_in_only_for_a_publish_that_was_not_told() {
        let told = |stack| SubmitOptions { stack };
        // Off, nothing changes.
        assert_eq!(told(StackLink::Follow).under_setting(false).stack, StackLink::Follow);
        // On, a publish that was told nothing links new stacks.
        assert_eq!(told(StackLink::Follow).under_setting(true).stack, StackLink::LinkNew);
        // What this publish was told wins, both ways.
        for (stack, on) in [
            (StackLink::Link, false),
            (StackLink::Link, true),
            (StackLink::Unlink, false),
            (StackLink::Unlink, true),
        ] {
            assert_eq!(told(stack).under_setting(on).stack, stack);
        }
    }

    #[test]
    fn under_the_setting_the_publish_that_opens_a_pull_request_links_the_chain() {
        // Nothing published yet. Not told and with no setting, the two
        // pull requests are opened as a plain chain.
        let local = unpublished_pair();
        let github = ModelGitHub::new(&local, &[("main", "m")], &[]);
        let plan = plan_on(&local, &github, "profile", StackLink::Follow);
        assert!(link_steps(&plan).is_empty(), "{:?}", plan.actions);

        // Under the setting the same publish links them, as its last step.
        let plan = plan_on(&local, &github, "profile", StackLink::LinkNew);
        assert_eq!(
            plan.actions.last(),
            Some(&SubmitAction::LinkStack {
                bookmarks: vec!["auth".into(), "profile".into()],
                onto: None,
                above: vec![],
            })
        );
        let outcome = execute_submit(&plan, &github, &github).unwrap();
        assert!(!outcome.failed, "{:?}", outcome.steps);
        assert_eq!(github.stack_of(90), [1, 2]);

        // It is a GitHub stack from then on, and follows the chain as one
        // does: a third segment joins from the top.
        let local = snapshot(
            vec![
                node("c1", "search: index", NodeKind::WorkingCopy, &["b1"], &["search"]),
                node("b1", "profile: avatars", NodeKind::Mutable, &["a1"], &["profile"]),
                node("a1", "auth: login", NodeKind::Mutable, &["m"], &["auth"]),
                node("m", "release", NodeKind::Immutable, &[], &["main"]),
            ],
            vec![
                bookmark("main", "m", SyncState::Synced, true),
                bookmark("auth", "a1", SyncState::Synced, false),
                bookmark("profile", "b1", SyncState::Synced, false),
                bookmark("search", "c1", SyncState::LocalOnly, false),
            ],
        );
        let github = ModelGitHub::new(
            &local,
            &[("main", "m"), ("auth", "a1"), ("profile", "b1")],
            &[(1, "auth", "main"), (2, "profile", "auth")],
        )
        .stacked(&[1, 2]);
        let plan = plan_on(&local, &github, "search", StackLink::LinkNew);
        assert_eq!(link_steps(&plan), ["link [\"search\"] onto Some(90)"]);
    }

    #[test]
    fn under_the_setting_a_plain_chain_that_only_changes_stays_plain() {
        // Both pull requests exist and were never linked: a chain from
        // before the setting, or one that was dissolved on purpose.
        let local = synced_pair();
        let branches = [("main", "m"), ("auth", "a1"), ("profile", "b1")];
        let github =
            ModelGitHub::new(&local, &branches, &[(1, "auth", "main"), (7, "profile", "auth")]);
        let plan = plan_on(&local, &github, "profile", StackLink::LinkNew);
        assert!(link_steps(&plan).is_empty(), "{:?}", plan.actions);

        // The same chain gains a segment: the publish that opens its pull
        // request links all three.
        let local = snapshot(
            vec![
                node("c1", "search: index", NodeKind::WorkingCopy, &["b1"], &["search"]),
                node("b1", "profile: avatars", NodeKind::Mutable, &["a1"], &["profile"]),
                node("a1", "auth: login", NodeKind::Mutable, &["m"], &["auth"]),
                node("m", "release", NodeKind::Immutable, &[], &["main"]),
            ],
            vec![
                bookmark("main", "m", SyncState::Synced, true),
                bookmark("auth", "a1", SyncState::Synced, false),
                bookmark("profile", "b1", SyncState::Synced, false),
                bookmark("search", "c1", SyncState::LocalOnly, false),
            ],
        );
        let github =
            ModelGitHub::new(&local, &branches, &[(1, "auth", "main"), (7, "profile", "auth")]);
        let plan = plan_on(&local, &github, "search", StackLink::LinkNew);
        assert_eq!(
            link_steps(&plan),
            ["link [\"auth\", \"profile\", \"search\"] onto None"]
        );
        // A publish told not to still wins over the setting.
        let plan = plan_on(&local, &github, "search", StackLink::Unlink);
        assert!(link_steps(&plan).is_empty(), "{:?}", plan.actions);

        // One pull request is not a stack, and the setting does not say
        // so each time one is opened.
        let local = snapshot(
            vec![
                node("a1", "auth: login", NodeKind::WorkingCopy, &["m"], &["auth"]),
                node("m", "release", NodeKind::Immutable, &[], &["main"]),
            ],
            vec![
                bookmark("main", "m", SyncState::Synced, true),
                bookmark("auth", "a1", SyncState::LocalOnly, false),
            ],
        );
        let github = ModelGitHub::new(&local, &[("main", "m")], &[]);
        let plan = plan_on(&local, &github, "auth", StackLink::LinkNew);
        assert!(link_steps(&plan).is_empty(), "{:?}", plan.actions);
        assert!(plan.warnings.is_empty(), "{:?}", plan.warnings);
    }

    #[test]
    fn a_repository_without_github_stacks_keeps_a_plain_chain_under_the_setting() {
        let local = unpublished_pair();

        // The setting is on, and this repository has no stacks. The
        // publish goes through; its last step says what was left out.
        let github = ModelGitHub::new(&local, &[("main", "m")], &[]);
        github.no_stacks.set(true);
        let plan = plan_on(&local, &github, "profile", StackLink::LinkNew);
        let outcome = execute_submit(&plan, &github, &github).unwrap();
        assert!(!outcome.failed, "{:?}", outcome.steps);
        let last = outcome.steps.last().unwrap();
        assert_eq!(last.status, SubmitStepStatus::Done);
        assert_eq!(
            last.detail.as_deref(),
            Some(
                "acme/widgets on github.example does not offer GitHub's stacked pull \
                 requests; the pull requests stay a plain chain"
            )
        );
        assert!(github.pr(1).open && github.pr(2).open);
        assert_eq!(github.pr(2).base, "auth");

        // Asked for on this publish, the same answer is a failure to do
        // what was asked.
        let github = ModelGitHub::new(&local, &[("main", "m")], &[]);
        github.no_stacks.set(true);
        let plan = plan_on(&local, &github, "profile", StackLink::Link);
        let outcome = execute_submit(&plan, &github, &github).unwrap();
        assert!(outcome.failed);
        assert_eq!(outcome.steps.last().unwrap().status, SubmitStepStatus::Failed);
    }

    #[test]
    fn new_pull_requests_join_the_stack_from_the_top() {
        // auth and profile are stack 90; a third segment is new.
        let local = snapshot(
            vec![
                node("c1", "search: index", NodeKind::WorkingCopy, &["b1"], &["search"]),
                node("b1", "profile: avatars", NodeKind::Mutable, &["a1"], &["profile"]),
                node("a1", "auth: login", NodeKind::Mutable, &["m"], &["auth"]),
                node("m", "release", NodeKind::Immutable, &[], &["main"]),
            ],
            vec![
                bookmark("main", "m", SyncState::Synced, true),
                bookmark("auth", "a1", SyncState::Synced, false),
                bookmark("profile", "b1", SyncState::Synced, false),
                bookmark("search", "c1", SyncState::LocalOnly, false),
            ],
        );
        let github = ModelGitHub::new(
            &local,
            &[("main", "m"), ("auth", "a1"), ("profile", "b1")],
            &[(1, "auth", "main"), (7, "profile", "auth")],
        )
        .stacked(&[1, 7]);

        let plan = plan_on(&local, &github, "search", StackLink::Follow);
        assert_eq!(
            retargets_and_pushes(&plan),
            ["push search", "open search->profile", "link [\"search\"] onto Some(90)"]
        );
        let outcome = execute_submit(&plan, &github, &github).unwrap();
        assert!(!outcome.failed, "{:?}", outcome.steps);
        assert_eq!(github.stack_of(90), [1, 7, 8]);
    }

    #[test]
    fn a_stack_that_changes_shape_is_dissolved_and_linked_again() {
        // Stack 90 is #1 auth, #7 profile; the two swapped places here.
        let local = swapped_snapshot();
        let before = || {
            ModelGitHub::new(
                &local,
                &[("main", "m"), ("auth", "old-a"), ("profile", "old-b")],
                &[(1, "auth", "main"), (7, "profile", "auth")],
            )
            .stacked(&[1, 7])
        };
        let github = before();
        let plan = plan_on(&local, &github, "auth", StackLink::Follow);
        assert_eq!(
            retargets_and_pushes(&plan),
            [
                // GitHub refuses a retarget inside a stack, and has no
                // reorder: out of the stack first.
                "unstack 90",
                "retarget #7->main",
                "push profile",
                "push auth",
                "retarget #1->profile",
                // And back in, in the new order, as the last step.
                "link [\"profile\", \"auth\"] onto None",
            ]
        );
        let outcome = execute_submit(&plan, &github, &github).unwrap();
        assert!(!outcome.failed, "{:?}", outcome.steps);
        assert!(github.pr(1).open && github.pr(7).open, "{:?}", github.log);
        assert_eq!(github.stack_of(91), [1, 7]);
        assert_eq!((github.pr(7).base, github.pr(1).base), ("main".into(), "profile".into()));

        // Without leaving the stack first, GitHub refuses the first
        // retarget and nothing is pushed.
        let github = before();
        let mut stuck = plan.clone();
        stuck.actions.retain(|a| !matches!(a, SubmitAction::Unstack { .. }));
        let outcome = execute_submit(&stuck, &github, &github).unwrap();
        assert!(outcome.failed);
        assert!(outcome.steps[0]
            .detail
            .as_deref()
            .unwrap()
            .contains("part of a stack"));
        assert!(!github.log.borrow().iter().any(|l| l.starts_with("push")));
    }

    #[test]
    fn linking_needs_two_pull_requests_and_says_so() {
        let local = snapshot(
            vec![
                node("a1", "auth: login", NodeKind::WorkingCopy, &["m"], &["auth"]),
                node("m", "release", NodeKind::Immutable, &[], &["main"]),
            ],
            vec![
                bookmark("main", "m", SyncState::Synced, true),
                bookmark("auth", "a1", SyncState::Synced, false),
            ],
        );
        let github = ModelGitHub::new(&local, &[("main", "m"), ("auth", "a1")], &[(1, "auth", "main")]);
        let plan = plan_on(&local, &github, "auth", StackLink::Link);
        assert!(retargets_and_pushes(&plan).is_empty(), "{:?}", plan.actions);
        assert!(
            plan.warnings.iter().any(|w| w.contains("at least two pull requests")),
            "{:?}",
            plan.warnings
        );
    }

    /// main(m) ── a1 (auth) ── x1 (extra, new) ── b1 (profile, @): a
    /// segment wedged in below profile.
    fn wedged_snapshot() -> RepoSnapshot {
        snapshot(
            vec![
                node("b1", "profile: avatars", NodeKind::WorkingCopy, &["x1"], &["profile"]),
                node("x1", "extra: wedge", NodeKind::Mutable, &["a1"], &["extra"]),
                node("a1", "auth: login", NodeKind::Mutable, &["m"], &["auth"]),
                node("m", "release", NodeKind::Immutable, &[], &["main"]),
            ],
            vec![
                bookmark("main", "m", SyncState::Synced, true),
                bookmark("auth", "a1", SyncState::Synced, false),
                bookmark("extra", "x1", SyncState::LocalOnly, false),
                bookmark("profile", "b1", SyncState::Diverged, false),
            ],
        )
    }

    #[test]
    fn a_stack_that_goes_on_above_the_published_head_keeps_what_is_above() {
        // Stack 90 is auth, profile, search on GitHub. Publishing through
        // "profile" with a new base for it has to rebuild the stack, and
        // search, which this publish does not reach, still sits on
        // profile: it goes back in on top.
        let local = wedged_snapshot();
        let github = ModelGitHub::new(
            &local,
            &[("main", "m"), ("auth", "a1"), ("profile", "old-b"), ("search", "old-c")],
            &[(1, "auth", "main"), (7, "profile", "auth"), (9, "search", "profile")],
        )
        .stacked(&[1, 7, 9]);
        let plan = plan_on(&local, &github, "profile", StackLink::Follow);
        assert_eq!(
            retargets_and_pushes(&plan),
            [
                "unstack 90",
                "retarget #7->main",
                "push extra",
                "push profile",
                "open extra->auth",
                "retarget #7->extra",
                "link [\"auth\", \"extra\", \"profile\"] onto None with [9] above",
            ]
        );
        // "search" is no bookmark here: #9 is someone else's, and the plan
        // says, by number, what this publish does around it. (The model's
        // pull requests have no author; with one the plan names it too.)
        let about_9: Vec<&String> = plan.warnings.iter().filter(|w| w.contains("#9")).collect();
        assert_eq!(
            about_9,
            [
                "#9 (\u{201c}search\u{201d}) is someone else's pull request on \u{201c}profile\u{201d}, \
                 and this push replaces commits under it: it shows the replaced commits as \
                 its own until its branch is rebased. Nothing here pushes to \
                 \u{201c}search\u{201d}: rebasing it is its author's to do",
                "#9 sits above this chain in GitHub stack 90: the stack is dissolved to \
                 change shape and linked again with it back on top",
            ]
        );
        assert!(!plan.warnings.iter().any(|w| w.contains("leaves the stack")));
        let outcome = execute_submit(&plan, &github, &github).unwrap();
        assert!(!outcome.failed, "{:?}", outcome.steps);
        assert_eq!(github.stack_of(91), [1, 7, 9, 10]);
        assert_eq!(
            outcome.steps.last().unwrap().detail.as_deref(),
            Some("Linked #1, #10, #7 and #9 as GitHub stack 91")
        );
    }

    #[test]
    fn a_stack_member_the_new_stack_cannot_hold_is_named_before_it_leaves() {
        // Stack 90 is auth, profile, search, and search sits on a branch
        // this chain no longer has under that name: GitHub's stack cannot
        // take it back, and the plan says so before anything runs.
        let local = wedged_snapshot();
        let github = ModelGitHub::new(
            &local,
            &[("main", "m"), ("auth", "a1"), ("profile", "old-b"), ("search", "old-c")],
            &[(1, "auth", "main"), (7, "profile", "auth"), (9, "search", "elsewhere")],
        )
        .stacked(&[1, 7, 9]);
        let plan = plan_on(&local, &github, "profile", StackLink::Follow);
        assert_eq!(
            retargets_and_pushes(&plan).last().map(String::as_str),
            Some("link [\"auth\", \"extra\", \"profile\"] onto None")
        );
        assert!(
            plan.warnings.iter().any(|w| w.contains("#9 is in GitHub stack 90")
                && w.contains("leaves the stack")),
            "{:?}",
            plan.warnings
        );
    }

    #[test]
    fn an_unstack_that_leaves_a_pull_request_behind_stops_the_publish() {
        // GitHub documents that a pull request queued to merge cannot
        // leave its stack: the answer then still lists it, open.
        let local = swapped_snapshot();
        let github = ModelGitHub::new(
            &local,
            &[("main", "m"), ("auth", "old-a"), ("profile", "old-b")],
            &[(1, "auth", "main"), (7, "profile", "auth")],
        )
        .stacked(&[1, 7]);
        github.queued.borrow_mut().push(1);
        let plan = plan_on(&local, &github, "auth", StackLink::Follow);
        let outcome = execute_submit(&plan, &github, &github).unwrap();
        assert!(outcome.failed);
        let story = outcome.steps[0].detail.as_deref().unwrap();
        assert!(story.contains("GitHub kept #1 in stack 90"), "{story}");
        // Nothing was retargeted or pushed on the strength of a stack
        // that is only half dissolved.
        assert_eq!(github.log.borrow().as_slice(), ["unstack 90"]);
    }

    /// main(m) ── p1 ── q1 (lower) ── r1 (upper, @): a published chain
    /// whose two bookmarks each moved up one commit. On the remote,
    /// lower is still p1 and upper still q1.
    fn slid_snapshot() -> RepoSnapshot {
        snapshot(
            vec![
                node("r1", "r", NodeKind::WorkingCopy, &["q1"], &["upper"]),
                node("q1", "q", NodeKind::Mutable, &["p1"], &["lower"]),
                node("p1", "p", NodeKind::Mutable, &["m"], &[]),
                node("m", "release", NodeKind::Immutable, &[], &["main"]),
            ],
            vec![
                bookmark("main", "m", SyncState::Synced, true),
                bookmark("lower", "q1", SyncState::Ahead, false),
                bookmark("upper", "r1", SyncState::Ahead, false),
            ],
        )
    }

    fn slid_github(local: &RepoSnapshot) -> ModelGitHub<'_> {
        ModelGitHub::new(
            local,
            &[("main", "m"), ("lower", "p1"), ("upper", "q1")],
            &[(1, "lower", "main"), (7, "upper", "lower")],
        )
    }

    #[test]
    fn publishing_part_of_a_chain_never_swallows_the_pull_request_above() {
        // Reproduced on GitHub (R7a): with only "lower" pushed, its branch
        // lands on the commit #7 still has as its head, and GitHub closes
        // #7 as merged into "lower".
        let local = slid_snapshot();
        let github = slid_github(&local);
        let plan = plan_on(&local, &github, "lower", StackLink::Follow);
        assert_eq!(plan.blockers.len(), 1, "{:?}", plan.blockers);
        assert!(
            plan.blockers[0].contains("GitHub would close #7 as merged")
                && plan.blockers[0].contains("publish through \u{201c}upper\u{201d}"),
            "{:?}",
            plan.blockers
        );
        assert!(execute_submit(&plan, &github, &github).is_err());
        assert!(github.log.borrow().is_empty());

        // What the refused push would have done, on the same GitHub.
        github.push_bookmarks(&["lower".to_owned()], "origin").unwrap();
        assert!(github.pr(7).merged, "{:?}", github.log);
    }

    #[test]
    fn a_slide_parks_the_pull_request_its_push_would_swallow() {
        // The whole chain is published, so #7's own branch moves too. On
        // GitHub that was enough when both branches arrived (R7b). It is
        // not when only one does, and a push of several branches is not
        // atomic. So #7 points at trunk while the branches move.
        let local = slid_snapshot();
        let github = slid_github(&local);
        let plan = plan_on(&local, &github, "upper", StackLink::Follow);
        assert!(plan.blockers.is_empty(), "{:?}", plan.blockers);
        assert_eq!(
            retargets_and_pushes(&plan),
            ["retarget #7->main", "push lower", "push upper", "retarget #7->lower"]
        );
        let outcome = execute_submit(&plan, &github, &github).unwrap();
        assert!(!outcome.failed, "{:?}", outcome.steps);
        assert!(github.pr(1).open && github.pr(7).open, "{:?}", github.log);
        assert_eq!(github.pr(7).base, "lower");

        // A push that goes through for "lower" and is refused for
        // "upper": #7 survives, and the failure says where it was left.
        let github = slid_github(&local);
        github.refused.borrow_mut().push("upper".to_owned());
        let outcome = execute_submit(&plan, &github, &github).unwrap();
        assert!(outcome.failed);
        assert!(github.pr(7).open, "{:?}", github.log);
        assert_eq!(github.pr(7).base, "main");
        let story = outcome
            .steps
            .iter()
            .find(|step| step.status == SubmitStepStatus::Failed)
            .and_then(|step| step.detail.as_deref())
            .unwrap();
        assert!(
            story.contains("#7 is left pointing at \u{201c}main\u{201d}")
                && story.contains("publish again to finish"),
            "{story}"
        );
        // Without the parking, the same partial push closes it.
        let github = slid_github(&local);
        github.refused.borrow_mut().push("upper".to_owned());
        let _ = github.push_bookmarks(&["lower".to_owned(), "upper".to_owned()], "origin");
        assert!(github.pr(7).merged, "{:?}", github.log);

        // Publishing again after the failure finishes the job: #7 goes
        // back onto "lower" once its branch has moved.
        let github = slid_github(&local);
        github.update_pr_base(7, "main").unwrap();
        github.branches.borrow_mut().insert("lower".into(), "q1".into());
        github.log.borrow_mut().clear();
        let mut after = local.clone();
        after.bookmarks[1].sync = SyncState::Synced;
        let plan = plan_on(&after, &github, "upper", StackLink::Follow);
        assert_eq!(retargets_and_pushes(&plan), ["push upper", "retarget #7->lower"]);
    }

    #[test]
    fn a_slide_inside_a_github_stack_leaves_the_stack_to_be_parked() {
        // GitHub refuses a retarget inside a stack, so the parking needs
        // the stack dissolved first and linked again afterwards.
        let local = slid_snapshot();
        let github = slid_github(&local).stacked(&[1, 7]);
        let plan = plan_on(&local, &github, "upper", StackLink::Follow);
        assert_eq!(
            retargets_and_pushes(&plan),
            [
                "unstack 90",
                "retarget #7->main",
                "push lower",
                "push upper",
                "retarget #7->lower",
                "link [\"lower\", \"upper\"] onto None",
            ]
        );
        let outcome = execute_submit(&plan, &github, &github).unwrap();
        assert!(!outcome.failed, "{:?}", outcome.steps);
        assert_eq!(github.stack_of(91), [1, 7]);
    }

    #[test]
    fn a_publish_that_stops_after_dissolving_a_stack_says_it_is_not_linked_again() {
        let local = swapped_snapshot();
        let github = ModelGitHub::new(
            &local,
            &[("main", "m"), ("auth", "old-a"), ("profile", "old-b")],
            &[(1, "auth", "main"), (7, "profile", "auth")],
        )
        .stacked(&[1, 7]);
        github.refused.borrow_mut().push("auth".to_owned());
        let plan = plan_on(&local, &github, "auth", StackLink::Follow);
        let outcome = execute_submit(&plan, &github, &github).unwrap();
        assert!(outcome.failed);
        let story = outcome
            .steps
            .iter()
            .find(|step| step.status == SubmitStepStatus::Failed)
            .and_then(|step| step.detail.as_deref())
            .unwrap();
        assert!(
            story.contains("GitHub stack 90 was dissolved for this publish and is not linked again")
                && story.contains("--stack"),
            "{story}"
        );
        // #7's new base is trunk, where the parking left it: nothing to
        // point back, and nothing said about it.
        assert!(!story.contains("left pointing at"), "{story}");
        assert!(github.stack_of(90).is_empty());
    }

    #[test]
    fn a_stack_is_seen_whole_only_with_its_bottom_its_top_and_all_between() {
        let member = |number: u64, base: &str, position: u32, size: u32| PrSummary {
            stack: Some(StackSeat { number: 90, position, size, base: "main".into() }),
            ..open_pr(number, "x", base)
        };
        // Both of two.
        assert!(sees_whole_stack(&[member(1, "main", 1, 2), member(7, "lower", 2, 2)], 90));
        // The upper one alone: its base is the branch of one not seen.
        assert!(!sees_whole_stack(&[member(7, "lower", 2, 2)], 90));
        // The bottom one merged: the next is based on trunk, and nothing
        // open is below it.
        assert!(sees_whole_stack(&[member(7, "main", 2, 3), member(9, "upper", 3, 3)], 90));
        // The top one not seen.
        assert!(!sees_whole_stack(
            &[member(1, "main", 1, 4), member(7, "lower", 2, 4), member(9, "upper", 3, 4)],
            90
        ));
        // One in the middle not seen.
        assert!(!sees_whole_stack(&[member(1, "main", 1, 3), member(9, "upper", 3, 3)], 90));
        // None of it.
        assert!(!sees_whole_stack(&[], 90));
    }

    /// A base this publish does not push is, here, where the last fetch
    /// left it. GitHub can have it somewhere else by now, at commits that
    /// are not here: whether the branch pushed against it ends up inside
    /// it cannot be told from this repository. (Seen with the real
    /// backend: "lower" had gained a commit holding the very commit
    /// "upper" was moved back to, and the push closed #7 as merged.)
    #[test]
    fn a_base_that_moved_on_github_since_the_fetch_stops_the_push_against_it() {
        // main(m) ── p1 (lower, in sync here) ── q1 (upper, moved back) ── r1.
        let local = snapshot(
            vec![
                node("r1", "r", NodeKind::WorkingCopy, &["q1"], &[]),
                node("q1", "q", NodeKind::Mutable, &["p1"], &["upper"]),
                node("p1", "p", NodeKind::Mutable, &["m"], &["lower"]),
                node("m", "release", NodeKind::Immutable, &[], &["main"]),
            ],
            vec![
                bookmark("main", "m", SyncState::Synced, true),
                bookmark("lower", "p1", SyncState::Synced, false),
                bookmark("upper", "q1", SyncState::Behind, false),
            ],
        );
        let github = ModelGitHub::new(
            &local,
            &[("main", "m"), ("lower", "p1"), ("upper", "r1")],
            &[(1, "lower", "main"), (7, "upper", "lower")],
        );
        // As far as the last fetch knows: fine, and "upper" is pushed.
        let plan = plan_on(&local, &github, "upper", StackLink::Follow);
        assert!(plan.blockers.is_empty(), "{:?}", plan.blockers);
        assert!(retargets_and_pushes(&plan).contains(&"push upper".to_owned()));

        // GitHub has "lower" at a commit that is not here.
        let mut prs = model_prs(&github);
        for pr in prs.report.prs.iter_mut().filter(|pr| pr.number == 1) {
            pr.head_commit = "0ddba11feedface".into();
        }
        let moved = RepoPrState::new(prs.report, "o");
        let plan = plan_submit(
            &local,
            &moved,
            &forge_repo(),
            &by_bookmark("upper"),
            &github,
            None,
            &SubmitOptions::default(),
        )
        .unwrap();
        assert!(
            plan.blockers.iter().any(|b| b.contains("\u{201c}lower\u{201d} is at 0ddba11f on GitHub and at cp1 here")
                && b.contains("GitHub would close #7 as merged")
                && b.contains("fetch")),
            "{:?}",
            plan.blockers
        );

        // "Not pushed" alone does not mean "where the fetch left it". A
        // base whose change came out empty (a rebase made it a no-op) is
        // not pushed either, and it has moved here: GitHub having it at
        // another commit says nothing then. (Seen on the sandbox, where
        // this stopped the publish that the rebase had been made for.)
        let mut emptied = local.clone();
        emptied.nodes[2].is_empty = true;
        emptied.bookmarks[1].sync = SyncState::Ahead;
        let plan = plan_submit(
            &emptied,
            &moved,
            &forge_repo(),
            &by_bookmark("upper"),
            &github,
            None,
            &SubmitOptions::default(),
        )
        .unwrap();
        assert!(
            !plan.blockers.iter().any(|b| b.contains("since the last fetch")),
            "{:?}",
            plan.blockers
        );
    }

    #[test]
    fn a_pull_request_head_brought_in_by_a_merge_is_seen_too() {
        // main(m) ── p1 ── l1 (lower, @), and l1 merges q1 in: the commit
        // #7 (upper, based on lower) has as its head. The stack walk goes
        // p1, l1 along first parents and never visits q1.
        let local = snapshot(
            vec![
                node("l1", "lower: merge q in", NodeKind::WorkingCopy, &["p1", "q1"], &["lower"]),
                node("q1", "q", NodeKind::Mutable, &["p1"], &["upper"]),
                node("p1", "p", NodeKind::Mutable, &["m"], &[]),
                node("m", "release", NodeKind::Immutable, &[], &["main"]),
            ],
            vec![
                bookmark("main", "m", SyncState::Synced, true),
                bookmark("lower", "l1", SyncState::Ahead, false),
                bookmark("upper", "q1", SyncState::Synced, false),
            ],
        );
        let github = ModelGitHub::new(
            &local,
            &[("main", "m"), ("lower", "p1"), ("upper", "q1")],
            &[(1, "lower", "main"), (7, "upper", "lower")],
        );
        let plan = plan_on(&local, &github, "lower", StackLink::Follow);
        assert_eq!(plan.blockers.len(), 1, "{:?}", plan.blockers);
        assert!(plan.blockers[0].contains("GitHub would close #7 as merged"));
        // What the refused push would have done.
        github.push_bookmarks(&["lower".to_owned()], "origin").unwrap();
        assert!(github.pr(7).merged, "{:?}", github.log);

        // The same when that head is immutable here, as a tagged commit
        // is: it is still the commit GitHub would find inside "lower".
        let mut tagged = local.clone();
        tagged.nodes[1].kind = NodeKind::Immutable;
        tagged.nodes[1].tags = vec!["v1".to_owned()];
        let github = ModelGitHub::new(
            &tagged,
            &[("main", "m"), ("lower", "p1"), ("upper", "q1")],
            &[(1, "lower", "main"), (7, "upper", "lower")],
        );
        let plan = plan_on(&tagged, &github, "lower", StackLink::Follow);
        assert!(
            plan.blockers.iter().any(|b| b.contains("GitHub would close #7 as merged")),
            "{:?}",
            plan.blockers
        );
    }

    #[test]
    fn bookmarks_that_slid_down_are_parked_against_a_push_that_goes_half_through() {
        // main(m) ── p1 ── q1 ── r1. On GitHub lower is q1 and upper is
        // r1; here both moved down one commit. Pushed whole, nothing ends
        // up inside anything. But if the remote takes "upper" and refuses
        // "lower", upper's branch is the very commit lower's still is.
        let local = snapshot(
            vec![
                node("r1", "r", NodeKind::WorkingCopy, &["q1"], &[]),
                node("q1", "q", NodeKind::Mutable, &["p1"], &["upper"]),
                node("p1", "p", NodeKind::Mutable, &["m"], &["lower"]),
                node("m", "release", NodeKind::Immutable, &[], &["main"]),
            ],
            vec![
                bookmark("main", "m", SyncState::Synced, true),
                bookmark("lower", "p1", SyncState::Behind, false),
                bookmark("upper", "q1", SyncState::Behind, false),
            ],
        );
        let on_github = || {
            ModelGitHub::new(
                &local,
                &[("main", "m"), ("lower", "q1"), ("upper", "r1")],
                &[(1, "lower", "main"), (7, "upper", "lower")],
            )
        };
        let github = on_github();
        let plan = plan_on(&local, &github, "upper", StackLink::Follow);
        assert!(plan.blockers.is_empty(), "{:?}", plan.blockers);
        assert_eq!(
            retargets_and_pushes(&plan),
            ["retarget #7->main", "push lower", "push upper", "retarget #7->lower"]
        );
        github.refused.borrow_mut().push("lower".to_owned());
        let outcome = execute_submit(&plan, &github, &github).unwrap();
        assert!(outcome.failed);
        assert!(github.pr(7).open, "{:?}", github.log);
        assert_eq!(github.pr(7).base, "main");

        // Unparked, that push closes it.
        let github = on_github();
        github.refused.borrow_mut().push("lower".to_owned());
        let _ = github.push_bookmarks(&["lower".to_owned(), "upper".to_owned()], "origin");
        assert!(github.pr(7).merged, "{:?}", github.log);
        // And pushed whole it is fine either way.
        let github = on_github();
        let outcome = execute_submit(&plan, &github, &github).unwrap();
        assert!(!outcome.failed, "{:?}", outcome.steps);
        assert!(github.pr(1).open && github.pr(7).open);
        assert_eq!(github.pr(7).base, "lower");

        // Where "lower" stands on GitHub is known here through its pull
        // request. Without one there is nothing to compare with, and #7 is
        // parked because its base moves at all.
        let bare_base = || {
            ModelGitHub::new(
                &local,
                &[("main", "m"), ("lower", "q1"), ("upper", "r1")],
                &[(7, "upper", "lower")],
            )
        };
        let github = bare_base();
        let plan = plan_on(&local, &github, "upper", StackLink::Follow);
        assert!(plan.blockers.is_empty(), "{:?}", plan.blockers);
        let steps = retargets_and_pushes(&plan);
        let at = |step: &str| steps.iter().position(|s| s == step).unwrap_or_else(|| panic!("{steps:?}"));
        assert!(at("retarget #7->main") < at("push lower"), "{steps:?}");
        assert!(at("push upper") < at("retarget #7->lower"), "{steps:?}");
        github.refused.borrow_mut().push("lower".to_owned());
        let outcome = execute_submit(&plan, &github, &github).unwrap();
        assert!(outcome.failed);
        assert!(github.pr(7).open, "{:?}", github.log);
        // Unparked, the same half-done push closes it.
        let github = bare_base();
        github.refused.borrow_mut().push("lower".to_owned());
        let _ = github.push_bookmarks(&["lower".to_owned(), "upper".to_owned()], "origin");
        assert!(github.pr(7).merged, "{:?}", github.log);
    }

    #[test]
    fn a_list_cut_short_is_completed_for_the_branches_a_publish_moves() {
        // GitHub's batched answer holds a hundred pull requests at most.
        // Here it was cut short and lacks #7, which is based on the
        // branch this publish moves.
        let local = slid_snapshot();
        let github = slid_github(&local);
        let cut_short = pr_state(
            model_prs(&github)
                .report
                .prs
                .into_iter()
                .filter(|pr| pr.number != 7)
                .collect(),
            true,
        );
        let plan = plan_submit(
            &local,
            &cut_short,
            &forge_repo(),
            &by_bookmark("lower"),
            &github,
            None,
            &SubmitOptions::default(),
        )
        .unwrap();
        assert_eq!(github.log.borrow().as_slice(), ["list open on lower"]);
        assert!(
            plan.blockers.iter().any(|b| b.contains("GitHub would close #7 as merged")),
            "{:?}",
            plan.blockers
        );
        // A complete list is not asked about again.
        github.log.borrow_mut().clear();
        plan_on(&local, &github, "lower", StackLink::Follow);
        assert!(github.log.borrow().is_empty());

        // The lookups go by base, so they cannot find a pull request that
        // is based on trunk. When that one is the bottom of a GitHub stack
        // the list also left out, the plan sees the stack's upper half
        // only, takes "lower" for unpublished, and would dissolve the
        // stack to rebuild it around a pull request it cannot open. It
        // stops there: nothing here needed a new shape.
        let synced = snapshot(
            vec![
                node("r1", "r", NodeKind::WorkingCopy, &["q1"], &["upper"]),
                node("q1", "q", NodeKind::Mutable, &["p1"], &["lower"]),
                node("p1", "p", NodeKind::Mutable, &["m"], &[]),
                node("m", "release", NodeKind::Immutable, &[], &["main"]),
            ],
            vec![
                bookmark("main", "m", SyncState::Synced, true),
                bookmark("lower", "q1", SyncState::Synced, false),
                bookmark("upper", "r1", SyncState::Synced, false),
            ],
        );
        let stacked = ModelGitHub::new(
            &synced,
            &[("main", "m"), ("lower", "q1"), ("upper", "r1")],
            &[(1, "lower", "main"), (7, "upper", "lower")],
        )
        .stacked(&[1, 7]);
        // With the whole list, an unchanged stack is left as it is.
        let reshapes = |plan: &SubmitPlan| {
            plan.actions.iter().any(|a| {
                matches!(
                    a,
                    SubmitAction::Unstack { .. }
                        | SubmitAction::LinkStack { .. }
                        | SubmitAction::CreatePr { .. }
                )
            })
        };
        let plan = plan_on(&synced, &stacked, "upper", StackLink::Follow);
        assert!(plan.blockers.is_empty() && !reshapes(&plan), "{:?}", plan.actions);
        // Cut short, without either pull request.
        let neither = pr_state(Vec::new(), true);
        let plan = plan_submit(
            &synced,
            &neither,
            &forge_repo(),
            &by_bookmark("upper"),
            &stacked,
            None,
            &SubmitOptions::default(),
        )
        .unwrap();
        assert!(
            plan.blockers.iter().any(|b| b.contains("are not all of GitHub stack 90")
                && b.contains("Jiji sees only #7")
                && b.contains("does not dissolve a stack it cannot see whole")),
            "{:?} {:?}",
            plan.blockers,
            plan.actions
        );
        // Asked for outright, a dissolve needs the whole stack in view
        // as well: every member leaves it, and one the list does not
        // show may be someone else's, which the plan could not name.
        let plan = plan_submit(
            &synced,
            &neither,
            &forge_repo(),
            &by_bookmark("upper"),
            &stacked,
            None,
            &SubmitOptions { stack: StackLink::Unlink },
        )
        .unwrap();
        assert!(
            plan.blockers.iter().any(|b| b.contains("Jiji sees only #7")
                && b.contains(
                    "does not dissolve a stack it cannot see whole, because it could not say \
                     whose pull requests leave it"
                )),
            "{:?}",
            plan.blockers
        );

        // Published whole, #7 is one of the stack's own pull requests, and
        // the list still lacks it. The plan has to know it all the same:
        // park it for the push, and not open "upper" a second time.
        let github = slid_github(&local);
        let plan = plan_submit(
            &local,
            &cut_short,
            &forge_repo(),
            &by_bookmark("upper"),
            &github,
            None,
            &SubmitOptions::default(),
        )
        .unwrap();
        assert_eq!(
            github.log.borrow().as_slice(),
            ["list open on lower", "list open on upper"]
        );
        assert!(plan.blockers.is_empty(), "{:?}", plan.blockers);
        assert!(
            !plan.actions.iter().any(|a| matches!(a, SubmitAction::CreatePr { .. })),
            "{:?}",
            plan.actions
        );
        assert_eq!(
            retargets_and_pushes(&plan),
            ["retarget #7->main", "push lower", "push upper", "retarget #7->lower"]
        );
        // With "upper" refused, the push that would have closed #7 leaves
        // it open.
        github.log.borrow_mut().clear();
        github.refused.borrow_mut().push("upper".to_owned());
        let outcome = execute_submit(&plan, &github, &github).unwrap();
        assert!(outcome.failed);
        assert!(github.pr(7).open, "{:?}", github.log);
    }

    /// main(m) ── a1 (auth) ── b1 (profile) ── t1 (their-fix, @): the
    /// pair with #9 by kim on top of it, taken over. `their-fix` reads
    /// `sync` against its remote.
    fn pair_with_taken_over(sync: SyncState) -> (RepoSnapshot, RepoPrState) {
        let mut snap = snapshot(
            vec![
                node("t1", "their fix", NodeKind::WorkingCopy, &["b1"], &["their-fix"]),
                node("b1", "profile: avatars", NodeKind::Mutable, &["a1"], &["profile"]),
                node("a1", "auth: login", NodeKind::Mutable, &["m"], &["auth"]),
                node("m", "release", NodeKind::Immutable, &[], &["main"]),
            ],
            vec![
                bookmark("main", "m", SyncState::Synced, true),
                bookmark("auth", "a1", SyncState::Synced, false),
                bookmark("profile", "b1", SyncState::Synced, false),
                bookmark("their-fix", "t1", sync, false),
            ],
        );
        snap.taken_over = vec![TakenOver {
            bookmark: "their-fix".into(),
            remote: "origin".into(),
            number: 9,
            author: Some("kim".into()),
        }];
        let pushed_at = |pr: PrSummary, commit: &str| PrSummary {
            head_commit: commit.into(),
            ..pr
        };
        let prs = pr_state(
            vec![
                pushed_at(text_pr(1, "auth", "main", "auth: login", ""), "ca1"),
                pushed_at(text_pr(7, "profile", "auth", "profile: avatars", ""), "cb1"),
                // Theirs, with a title and a body of their own.
                PrSummary {
                    title: "A fix, as its author called it".into(),
                    body: Some("Their own words.".into()),
                    ..their_pr(
                        9,
                        "their-fix",
                        "profile",
                        "kim",
                        if sync == SyncState::Synced { "ct1" } else { "0ldt1" },
                    )
                },
            ],
            false,
        );
        (snap, prs)
    }

    fn about_taking_over(plan: &SubmitPlan) -> Vec<&str> {
        plan.warnings
            .iter()
            .map(String::as_str)
            .filter(|w| w.contains("taken over") || w.contains("someone else"))
            .collect()
    }

    /// Taken over, their pull request is a segment like any other, and
    /// the plan carries whose it is. A publish that pushes its branch
    /// names the pull request and its author before anything runs; one
    /// that does not push it has nothing to say.
    #[test]
    fn a_publish_that_pushes_a_taken_over_branch_says_whose_it_is() {
        let kim = TakenOver {
            bookmark: "their-fix".into(),
            remote: "origin".into(),
            number: 9,
            author: Some("kim".into()),
        };
        // In step with its remote: a segment, marked, and nothing pushed.
        let (snap, prs) = pair_with_taken_over(SyncState::Synced);
        let plan = plan_through(&snap, &prs, "their-fix", StackLink::Follow);
        assert!(plan.blockers.is_empty(), "{:?}", plan.blockers);
        assert_eq!(
            plan.segments
                .iter()
                .map(|s| (s.bookmark.as_str(), s.pr.as_ref().map(|pr| pr.number), s.taken_over.clone()))
                .collect::<Vec<_>>(),
            [
                ("auth", Some(1), None),
                ("profile", Some(7), None),
                ("their-fix", Some(9), Some(kim.clone())),
            ]
        );
        assert!(retargets_and_pushes(&plan).is_empty(), "{:?}", plan.actions);
        assert!(about_taking_over(&plan).is_empty(), "{:?}", plan.warnings);
        // The pull request stays its author's: the title and the body
        // they wrote are not Jiji's to rewrite.
        assert!(
            !plan
                .actions
                .iter()
                .any(|a| matches!(a, SubmitAction::UpdatePrText { number: 9, .. })),
            "{:?}",
            plan.actions
        );

        // Changed here: the push is named for what it is.
        for sync in [SyncState::Ahead, SyncState::Diverged] {
            let (snap, prs) = pair_with_taken_over(sync);
            let plan = plan_through(&snap, &prs, "their-fix", StackLink::Follow);
            assert_eq!(retargets_and_pushes(&plan), ["push their-fix"]);
            assert_eq!(
                about_taking_over(&plan),
                ["This pushes \u{201c}their-fix\u{201d} (#9 by @kim, taken over): a branch someone \
                  else made. The push goes through only if the branch is where the last fetch \
                  saw it, so nothing pushed there since is overwritten"]
            );
        }

        // A push of the user's own branches says nothing about theirs.
        let (mut snap, prs) = pair_with_taken_over(SyncState::Synced);
        snap.bookmarks[2] = bookmark("profile", "b1", SyncState::Ahead, false);
        let plan = plan_through(&snap, &prs, "their-fix", StackLink::Follow);
        assert_eq!(retargets_and_pushes(&plan), ["push profile"]);
        assert!(about_taking_over(&plan).is_empty(), "{:?}", plan.warnings);

        // A record for another pull request is a name used again: the
        // branch this bookmark has now is the user's own.
        let (mut snap, prs) = pair_with_taken_over(SyncState::Ahead);
        snap.taken_over[0].number = 12;
        let plan = plan_through(&snap, &prs, "their-fix", StackLink::Follow);
        assert_eq!(retargets_and_pushes(&plan), ["push their-fix"]);
        assert_eq!(plan.segments[2].taken_over, None);
        assert!(about_taking_over(&plan).is_empty(), "{:?}", plan.warnings);
    }

    /// A taken-over bookmark that reads as conflicted is what a fetch
    /// leaves when its author pushed to it again after it was changed
    /// here. The refusal says that, and what to do.
    #[test]
    fn a_conflicted_taken_over_bookmark_says_its_author_pushed_again() {
        let (mut snap, prs) = pair_with_taken_over(SyncState::Diverged);
        snap.conflicts.push(ConflictItem {
            id: "bookmark-their-fix".into(),
            kind: ConflictKind::Bookmark,
            summary: "conflicted".into(),
            node_id: None,
            paths: vec![],
            more_paths: 0,
            targets: vec![],
            workspace: None,
        });
        let plan = plan_through(&snap, &prs, "their-fix", StackLink::Follow);
        assert_eq!(
            plan.blockers,
            ["\u{201c}their-fix\u{201d} is conflicted: it is #9 by @kim's branch, taken over, and \
              it was pushed to again after it was changed here. The new commits are on \
              their-fix@origin. Bring the two together (rebase theirs onto yours, or take \
              theirs), point \u{201c}their-fix\u{201d} at the result, and publish again"]
        );
    }

    /// The remote refuses a push of a branch that moved since the last
    /// fetch. `refusal` is the backend's sentence for it.
    struct Refusing(&'static str);

    impl SubmitVcs for Refusing {
        fn push_bookmarks(&self, _: &[String], _: &str) -> Result<String, BackendError> {
            Err(BackendError::MutationFailed(self.0.into()))
        }
        fn create_bookmark(&self, name: &str, _: &str) -> Result<String, BackendError> {
            Ok(format!("Created bookmark {name}"))
        }
    }

    /// Every push carries a lease, so a commit its author pushed after
    /// the last fetch is never overwritten. When that is why the push
    /// was refused, the step says so: who pushed, that nothing of theirs
    /// was lost, and what to do. Any other failure reads as it did.
    #[test]
    fn a_refused_push_of_a_taken_over_branch_says_who_pushed_and_what_to_do() {
        let (snap, prs) = pair_with_taken_over(SyncState::Diverged);
        let plan = plan_through(&snap, &prs, "their-fix", StackLink::Follow);
        let push_detail = |vcs: &Refusing| -> String {
            let outcome = execute_submit(&plan, vcs, &StubForge::default()).unwrap();
            assert!(outcome.failed);
            let step = outcome
                .steps
                .iter()
                .find(|step| matches!(step.action, SubmitAction::Push { .. }))
                .expect("the plan pushes");
            assert_eq!(step.status, SubmitStepStatus::Failed);
            step.detail.clone().unwrap_or_default()
        };
        let moved = "their-fix moved on the remote since the last fetch; fetch, reconcile, and \
                     push again";
        assert_eq!(
            push_detail(&Refusing(moved)),
            format!(
                "Could not apply the change: {moved}. \u{201c}their-fix\u{201d} is #9 by @kim's \
                 branch, taken over: it was pushed to again since the last fetch, and nothing \
                 there was overwritten. Fetch: the new commits arrive beside yours, and \
                 \u{201c}their-fix\u{201d} reads as conflicted until you have brought the two \
                 together (rebase theirs onto yours, or take theirs) and pointed it at the \
                 result. Then publish again"
            )
        );
        assert_eq!(
            push_detail(&Refusing("the remote hung up")),
            "Could not apply the change: the remote hung up"
        );
    }

    /// Asked to link the chain as a GitHub stack, a publish takes someone
    /// else's pull requests that sit on top of it into the stack, and
    /// says so by number and author: in a stack GitHub rebases their
    /// branches after each merge below. Unasked, it never reaches for
    /// them.
    #[test]
    fn asked_for_a_github_stack_takes_in_someone_elses_pull_requests_on_top() {
        let (snap, prs) = pair_with_others(SyncState::Synced);
        let plan = plan_through(&snap, &prs, "profile", StackLink::Link);
        assert_eq!(
            link_steps(&plan),
            ["link [\"auth\", \"profile\"] onto None with [9, 10] above"]
        );
        assert_eq!(
            plan.warnings
                .iter()
                .filter(|w| w.contains("GitHub stack"))
                .collect::<Vec<_>>(),
            ["#9 by @kim and #10 by @kim sit on top of this chain and are someone else's: they \
              go into the GitHub stack with it. In a stack GitHub rebases their branches itself \
              after each merge below, so they go on showing only their own changes; and their \
              bases cannot be changed while they are in the stack"]
        );
        // Left alone, or under the setting for new stacks: nothing of
        // theirs is touched.
        for unasked in [StackLink::Follow, StackLink::LinkNew] {
            let plan = plan_through(&snap, &prs, "profile", unasked);
            assert!(link_steps(&plan).is_empty(), "{:?}", plan.actions);
            assert!(!plan.warnings.iter().any(|w| w.contains("GitHub stack")), "{:?}", plan.warnings);
        }

        // One on top, said in the singular.
        let only_nine = pr_state(
            prs.report.prs.iter().filter(|pr| pr.number != 10).cloned().collect(),
            false,
        );
        let plan = plan_through(&snap, &only_nine, "profile", StackLink::Link);
        assert_eq!(
            link_steps(&plan),
            ["link [\"auth\", \"profile\"] onto None with [9] above"]
        );
        assert!(
            plan.warnings.iter().any(|w| w
                == "#9 by @kim sits on top of this chain and is someone else's: it goes into the \
                    GitHub stack with it. In a stack GitHub rebases its branch itself after each \
                    merge below, so it goes on showing only its own changes; and its base cannot \
                    be changed while it is in the stack"),
            "{:?}",
            plan.warnings
        );
    }

    /// A stack is one line, in one repository. What cannot go into it is
    /// left where it is, and the chain is linked without it.
    #[test]
    fn what_cannot_sit_in_a_stack_on_top_of_the_chain_is_not_taken_in() {
        let (snap, prs) = pair_with_others(SyncState::Synced);
        let with = |change: &dyn Fn(&mut Vec<PrSummary>)| {
            let mut all = prs.report.prs.clone();
            change(&mut all);
            pr_state(all, false)
        };
        let just_the_pair = ["link [\"auth\", \"profile\"] onto None"];

        // From a fork: its branch is in another repository.
        let fork = with(&|all| all[2].head_owner = Some("kim".into()));
        let plan = plan_through(&snap, &fork, "profile", StackLink::Link);
        assert_eq!(link_steps(&plan), just_the_pair);
        assert!(!plan.warnings.iter().any(|w| w.contains("on top of this chain")), "{:?}", plan.warnings);

        // Two that rest on the same branch: neither is the one on top.
        let two = with(&|all| all.push(their_pr(11, "their-other", "profile", "sam", "c0ffee11")));
        let plan = plan_through(&snap, &two, "profile", StackLink::Link);
        assert_eq!(link_steps(&plan), just_the_pair);

        // The line is taken as far as it is one: #9, and not the two
        // that rest on it.
        let forked_above = with(&|all| all.push(their_pr(11, "their-other", "their-top", "sam", "c0ffee11")));
        let plan = plan_through(&snap, &forked_above, "profile", StackLink::Link);
        assert_eq!(
            link_steps(&plan),
            ["link [\"auth\", \"profile\"] onto None with [9] above"]
        );

        // A chain that goes on above the published head: theirs on the
        // head is beside the chain, not on top of it.
        let on_auth = with(&|all| {
            all.truncate(2);
            all.push(their_pr(8, "their-mid", "auth", "sam", "c0ffee08"));
        });
        let plan = plan_through(&snap, &on_auth, "auth", StackLink::Link);
        assert!(link_steps(&plan).is_empty(), "{:?}", plan.actions);
        assert!(
            plan.warnings.iter().any(|w| w.contains("at least two pull requests")),
            "{:?}",
            plan.warnings
        );
    }

    /// The same against GitHub's rules: the link goes through with
    /// theirs on top, one pull request of the user's own is enough for a
    /// stack with theirs, and a stack that exists gets theirs added.
    #[test]
    fn taking_someone_elses_pull_request_into_a_stack_is_what_github_accepts() {
        let local = synced_pair();
        let branches = [
            ("main", "m"),
            ("auth", "a1"),
            ("profile", "b1"),
            ("their-top", "t9"),
            ("their-next", "t10"),
        ];
        let chain = [
            (1, "auth", "main"),
            (7, "profile", "auth"),
            (9, "their-top", "profile"),
            (10, "their-next", "their-top"),
        ];

        // A plain chain: one stack of all four, bottom first.
        let github = ModelGitHub::new(&local, &branches, &chain);
        let plan = plan_on(&local, &github, "profile", StackLink::Link);
        let outcome = execute_submit(&plan, &github, &github).unwrap();
        assert!(!outcome.failed, "{:?}", outcome.steps);
        assert_eq!(github.stack_of(90), [1, 7, 9, 10]);
        assert_eq!(
            outcome.steps.last().unwrap().detail.as_deref(),
            Some("Linked #1, #7, #9 and #10 as GitHub stack 90")
        );
        // Linked, there is nothing more to ask for.
        let plan = plan_on(&local, &github, "profile", StackLink::Link);
        assert!(retargets_and_pushes(&plan).is_empty(), "{:?}", plan.actions);

        // The chain is a stack already, and theirs came afterwards: it is
        // added on top. Unasked, the stack is left as it is.
        let github = ModelGitHub::new(&local, &branches, &chain[..3]).stacked(&[1, 7]);
        let plan = plan_on(&local, &github, "profile", StackLink::Follow);
        assert!(retargets_and_pushes(&plan).is_empty(), "{:?}", plan.actions);
        let plan = plan_on(&local, &github, "profile", StackLink::Link);
        assert_eq!(link_steps(&plan), ["link [] onto Some(90) with [9] above"]);
        let outcome = execute_submit(&plan, &github, &github).unwrap();
        assert!(!outcome.failed, "{:?}", outcome.steps);
        assert_eq!(github.stack_of(90), [1, 7, 9]);
        assert_eq!(
            outcome.steps.last().unwrap().detail.as_deref(),
            Some("Added #9 to GitHub stack 90")
        );

        // One pull request of the user's own, with theirs on top: two,
        // which is a stack.
        let lone = snapshot(
            vec![
                node("a1", "auth: login", NodeKind::WorkingCopy, &["m"], &["auth"]),
                node("m", "release", NodeKind::Immutable, &[], &["main"]),
            ],
            vec![
                bookmark("main", "m", SyncState::Synced, true),
                bookmark("auth", "a1", SyncState::Synced, false),
            ],
        );
        let github = ModelGitHub::new(
            &lone,
            &[("main", "m"), ("auth", "a1"), ("their-top", "t9")],
            &[(1, "auth", "main"), (9, "their-top", "auth")],
        );
        let plan = plan_on(&lone, &github, "auth", StackLink::Link);
        assert_eq!(link_steps(&plan), ["link [\"auth\"] onto None with [9] above"]);
        assert!(
            !plan.warnings.iter().any(|w| w.contains("at least two pull requests")),
            "{:?}",
            plan.warnings
        );
        let outcome = execute_submit(&plan, &github, &github).unwrap();
        assert!(!outcome.failed, "{:?}", outcome.steps);
        assert_eq!(github.stack_of(90), [1, 9]);
    }

    /// A pull request that is in a GitHub stack when it is taken over.
    /// Added to the user's own stack above their pull requests, it is a
    /// member like the others once its branch is a bookmark here, and a
    /// publish has nothing to do. As the bottom of a stack of its
    /// author's, built on the user's chain, it is now a part of that
    /// chain: the next publish dissolves their stack and links the whole
    /// chain, with what is still theirs back on top, and says so first.
    #[test]
    fn a_taken_over_pull_request_keeps_or_changes_its_github_stack_in_the_open() {
        let (local, _) = pair_with_taken_over(SyncState::Synced);
        let branches = [
            ("main", "m"),
            ("auth", "a1"),
            ("profile", "b1"),
            ("their-fix", "t1"),
            ("their-next", "t2"),
        ];
        let chain = [
            (1, "auth", "main"),
            (7, "profile", "auth"),
            (9, "their-fix", "profile"),
            (10, "their-next", "their-fix"),
        ];

        // In the user's own stack, above their pull requests.
        let github = ModelGitHub::new(&local, &branches, &chain[..3]).stacked(&[1, 7, 9]);
        for wanted in [StackLink::Follow, StackLink::Link] {
            let plan = plan_on(&local, &github, "their-fix", wanted);
            assert!(plan.blockers.is_empty(), "{:?}", plan.blockers);
            assert!(retargets_and_pushes(&plan).is_empty(), "{:?}", plan.actions);
        }

        // The bottom of a stack of its author's, on the user's plain chain.
        let github = ModelGitHub::new(&local, &branches, &chain).stacked(&[9, 10]);
        let plan = plan_on(&local, &github, "their-fix", StackLink::Follow);
        assert!(plan.blockers.is_empty(), "{:?}", plan.blockers);
        assert_eq!(
            link_steps(&plan),
            [
                "unstack 90",
                "link [\"auth\", \"profile\", \"their-fix\"] onto None with [10] above",
            ]
        );
        assert!(
            plan.warnings.iter().any(|w| w
                == "#10 sits above this chain in GitHub stack 90: the stack is dissolved to \
                    change shape and linked again with it back on top"),
            "{:?}",
            plan.warnings
        );
        let outcome = execute_submit(&plan, &github, &github).unwrap();
        assert!(!outcome.failed, "{:?}", outcome.steps);
        assert_eq!(github.stack_of(91), [1, 7, 9, 10]);
        // Told not to keep a GitHub stack, the publish only dissolves it.
        let github = ModelGitHub::new(&local, &branches, &chain).stacked(&[9, 10]);
        let plan = plan_on(&local, &github, "their-fix", StackLink::Unlink);
        assert_eq!(link_steps(&plan), ["unstack 90"]);
    }
}
