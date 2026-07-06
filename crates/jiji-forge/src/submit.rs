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
//! Re-submitting also reconciles what GitHub shows: existing PR titles and
//! descriptions update from the commits through the fingerprint machinery
//! in [`crate::reconcile`] (hand edits are detected and respected, never
//! clobbered), and every PR in a multi-PR stack carries the stack-info
//! comment from [`crate::comment`], edited in place on later submits.
//!
//! What this slice deliberately leaves to later slices: draft handling,
//! foreign bases (a coworker's branch in the stack's ancestry — the
//! snapshot does not carry remote-only bookmarks on nodes yet), and
//! recognizing already-merged PRs (needs a per-bookmark merged-PR query;
//! the land flow owns it).

use jiji_core::snapshot::{GraphNode, NodeKind, RepoSnapshot, SyncState};
use jiji_core::BackendError;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::comment::{inherit_fossils, parse_stack_data, render_stack_comment, StackEntry};
use crate::error::ForgeError;
use crate::github::GitHubClient;
use crate::pr::{PrSummary, RepoPrState};
use crate::reconcile::{plan_pr_text, TextWarning};
use crate::remote::ForgeRepo;
use crate::template::{new_pr_body, PrTemplate};

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
    /// The stack-info comment as it will read, rendered for the panel
    /// when the plan syncs comments (PRs created by this plan appear as
    /// plain names — their links exist only after execution).
    pub stack_comment_preview: Option<String>,
    /// Where the repo's PR template lives, when trunk carries one and this
    /// plan creates a PR whose body folds it in — so the panel can say new
    /// descriptions start from it.
    pub pr_template_path: Option<String>,
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
}

/// [`SubmitForge`] over the real GitHub client, bound to a detected repo.
pub struct RepoForge<'a> {
    pub client: &'a GitHubClient,
    pub repo: &'a ForgeRepo,
}

impl StackCommentSource for RepoForge<'_> {
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
    ) -> Result<PrSummary, ForgeError> {
        self.client
            .create_pr(&self.repo.owner, &self.repo.name, title, body, head, base)
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
}

/// One bookmark-bounded run of the stack, bottom-up — the shape submit and
/// land planning share.
pub(crate) struct RawSegment<'a> {
    pub bookmark: &'a str,
    pub nodes: Vec<&'a GraphNode>,
}

/// Walk the snapshot graph from `head_bookmark` down first parents to the
/// immutable base and cut the mutable chain at local non-trunk bookmarks.
/// Several names on one change collapse into one segment (the PR-known
/// name preferred, the head bookmark always publishing through itself),
/// reported in the returned warnings. Errors are plain messages for each
/// caller to wrap in its own `ForgeError` flavor.
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
        return Err(format!(
            "\u{201c}{head_bookmark}\u{201d} points at immutable history — everything \
             under it is already part of trunk"
        ));
    }
    chain.reverse(); // bottom-up, ending at the head bookmark's change

    // Segment the chain at bookmarked changes. Every local non-trunk
    // bookmark ends a segment; several on one change collapse into one
    // segment published through one of them (jjpr collapses the same way).
    let mut warnings: Vec<String> = Vec::new();
    let local_bookmarks: std::collections::HashSet<&str> = snapshot
        .bookmarks
        .iter()
        .filter(|b| b.is_local && !b.is_trunk)
        .map(|b| b.name.as_str())
        .collect();
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
    Ok((raw_segments, warnings))
}

/// Build the submission plan for the stack under `head_bookmark`: walk the
/// snapshot graph from the bookmark down first parents to the immutable
/// base, segment the mutable chain at local non-trunk bookmarks, and
/// compare each segment against the forge's open-PR state — including each
/// existing PR's title/description text and its stack comment, so the plan
/// enumerates every remote write before anything runs and an up-to-date
/// stack still plans nothing.
pub fn plan_submit(
    snapshot: &RepoSnapshot,
    prs: &RepoPrState,
    repo: &ForgeRepo,
    head_bookmark: &str,
    comments: &dyn StackCommentSource,
    pr_template: Option<&PrTemplate>,
) -> Result<SubmitPlan, ForgeError> {
    let (raw_segments, mut warnings) =
        stack_segments(snapshot, prs, head_bookmark).map_err(ForgeError::Plan)?;
    let mut blockers: Vec<String> = Vec::new();

    // Compare each segment with the forge state, bottom-up, tracking the
    // effective base like jjpr: trunk first, then each live segment's
    // bookmark.
    let mut segments: Vec<SubmitSegment> = Vec::new();
    let mut actions: Vec<SubmitAction> = Vec::new();
    let mut pr_actions: Vec<SubmitAction> = Vec::new();
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
                                if raw.nodes.len() != 1 {
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
                pr_actions.push(SubmitAction::CreatePr {
                    bookmark: name.to_owned(),
                    base: effective_base.clone(),
                    title: title.clone(),
                    // Sentinel-wrapped and fingerprinted from birth, so
                    // later submits can update it without guessing; the
                    // repo's PR template (when trunk carries one) rides
                    // below the managed section as user-space text.
                    body: new_pr_body(&body, &title, pr_template),
                });
            }
        }

        live.push((name.to_owned(), pr.clone()));
        effective_base = name.to_owned();
        segments.push(segment);
    }
    // Pushes first (a new PR's head and base branches must exist), then PR
    // creations bottom-up, then retargets, then text updates, then the
    // stack comments (they mention the PRs everything above sets up).
    let (creates, retargets): (Vec<_>, Vec<_>) = pr_actions
        .into_iter()
        .partition(|a| matches!(a, SubmitAction::CreatePr { .. }));
    let creating = !creates.is_empty();
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
    let pr_template_path = pr_template
        .filter(|_| {
            actions
                .iter()
                .any(|a| matches!(a, SubmitAction::CreatePr { .. }))
        })
        .map(|t| t.path.clone());

    Ok(SubmitPlan {
        head_bookmark: head_bookmark.to_owned(),
        remote: repo.remote.clone(),
        base_branch: snapshot.trunk_bookmark.clone(),
        segments,
        actions,
        blockers,
        warnings,
        stack_comment_preview,
        pr_template_path,
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
/// retargets, text updates, and stack comments. The first failure stops
/// execution — later steps report as skipped rather than running against a
/// half-updated remote. A plan with blockers refuses to run at all.
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

    // PRs opened by this run, for the comment steps that follow them.
    let mut created: std::collections::HashMap<String, PrSummary> =
        std::collections::HashMap::new();
    if !failed {
        for step in &mut steps {
            let result: Result<(String, Option<PrSummary>), ForgeError> = match &step.action {
                SubmitAction::Push { .. } => continue,
                SubmitAction::CreatePr {
                    bookmark,
                    base,
                    title,
                    body,
                } => forge.create_pr(title, body, bookmark, base).map(|pr| {
                    created.insert(bookmark.clone(), pr.clone());
                    (format!("Opened #{} for {bookmark}", pr.number), Some(pr))
                }),
                SubmitAction::RetargetPr {
                    number,
                    bookmark,
                    from_base,
                    to_base,
                } => forge.update_pr_base(*number, to_base).map(|()| {
                    (
                        format!(
                            "Retargeted #{number} ({bookmark}) from {from_base} to {to_base}"
                        ),
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
            };
            match result {
                Ok((detail, pr)) => {
                    step.status = SubmitStepStatus::Done;
                    step.detail = Some(detail);
                    step.pr = pr;
                }
                Err(err) => {
                    step.status = SubmitStepStatus::Failed;
                    step.detail = Some(err.to_string());
                    failed = true;
                    break;
                }
            }
        }
    }

    Ok(SubmitOutcome { steps, failed })
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
    use crate::pr::{ChecksRollup, PrState, PrStateReport, ReviewDecision};
    use crate::reconcile::{extract_managed_body, fingerprint, stored_body_fp, wrap_managed_body};
    use crate::remote::ForgeProvider;
    use jiji_core::snapshot::{BookmarkState, ConflictItem, ConflictKind};
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

        let plan = plan_submit(&snap, &prs, &forge_repo(), "profile", &NoComments, None).unwrap();
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
            "profile",
            &NoComments,
            Some(&template),
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
            "profile",
            &NoComments,
            Some(&template),
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
        let plan = plan_submit(&snap, &prs, &forge_repo(), "profile", &comments, None).unwrap();
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
        let plan = plan_submit(&snap, &prs, &forge_repo(), "profile", &comments, None).unwrap();
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
        let plan = plan_submit(&snap, &prs, &forge_repo(), "profile", &comments, None).unwrap();
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
        let plan = plan_submit(&snap, &prs, &forge_repo(), "profile", &comments, None).unwrap();
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
        let plan = plan_submit(&snap, &prs, &forge_repo(), "profile", &comments, None).unwrap();
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
        let plan = plan_submit(&snap, &prs, &forge_repo(), "profile", &comments, None).unwrap();
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
        let plan = plan_submit(&snap, &prs, &forge_repo(), "profile", &comments, None).unwrap();
        assert!(plan.actions.is_empty(), "{:?}", plan.actions);
        assert!(plan.warnings.is_empty(), "{:?}", plan.warnings);
    }

    #[test]
    fn stack_comments_sync_only_when_stale() {
        // One comment drifted (say the user edited it): exactly one
        // update plans, aimed at that comment.
        let (snap, prs, mut comments) = consistent_stack();
        comments.0.get_mut(&7).unwrap().body = "someone touched this".into();
        let plan = plan_submit(&snap, &prs, &forge_repo(), "profile", &comments, None).unwrap();
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
        let plan = plan_submit(&snap, &prs, &forge_repo(), "profile", &NoComments, None).unwrap();
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
        let plan = plan_submit(&snap, &prs, &forge_repo(), "profile", &NoComments, None).unwrap();
        assert!(
            !plan.actions.iter().any(|a| matches!(a, SubmitAction::SyncStackComment { .. })),
            "{:?}",
            plan.actions
        );

        // …but an existing one keeps updating as the stack shrinks: the
        // now-gone `auth` entry is inherited as a fossil.
        let entries = vec![crate::comment::StackEntry {
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
        let plan = plan_submit(&snap, &prs, &forge_repo(), "profile", &comments, None).unwrap();
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
        let plan = plan_submit(&snap, &prs, &forge_repo(), "profile", &NoComments, None).unwrap();
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
        let plan = plan_submit(&synced, &prs, &forge_repo(), "profile", &NoComments, None).unwrap();
        assert!(plan.blockers.is_empty(), "{:?}", plan.blockers);
    }

    #[test]
    fn empty_segments_skip_but_still_base_the_stack() {
        let mut snap = stack_snapshot();
        snap.nodes[1].is_empty = true; // a2
        snap.nodes[2].is_empty = true; // a1 — auth's whole segment empty
        let prs = pr_state(vec![], true);
        let plan = plan_submit(&snap, &prs, &forge_repo(), "profile", &NoComments, None).unwrap();

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
        let plan = plan_submit(&snap, &prs, &forge_repo(), "profile", &NoComments, None).unwrap();
        assert!(
            plan.blockers.iter().any(|b| b.contains("is conflicted")),
            "{:?}",
            plan.blockers
        );

        let snap = stack_snapshot();
        let err = plan_submit(&snap, &prs, &forge_repo(), "nope", &NoComments, None).unwrap_err();
        assert_eq!(err.code(), "plan_failed");
        let err = plan_submit(&snap, &prs, &forge_repo(), "main", &NoComments, None).unwrap_err();
        assert!(err.to_string().contains("trunk"), "{err}");
        // A bookmark parked on immutable history has nothing to publish.
        let mut on_trunk = stack_snapshot();
        on_trunk.bookmarks.push(bookmark("old", "m", SyncState::LocalOnly, false));
        on_trunk.nodes[3].bookmarks.push("old".into());
        let err = plan_submit(&on_trunk, &prs, &forge_repo(), "old", &NoComments, None).unwrap_err();
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

        let plan = plan_submit(&snap, &prs, &forge_repo(), "profile", &NoComments, None).unwrap();
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
    }

    fn plan_with_all_action_kinds() -> SubmitPlan {
        let mut snap = stack_snapshot();
        snap.bookmarks[1].target = "a2".into();
        snap.nodes[1].bookmarks = vec!["auth".into()];
        snap.nodes[2].bookmarks = vec![];
        let prs = pr_state(vec![open_pr(7, "profile", "main")], false);
        plan_submit(&snap, &prs, &forge_repo(), "profile", &NoComments, None).unwrap()
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
        let plan = plan_submit(&snap, &prs, &forge_repo(), "profile", &comments, None).unwrap();

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
            plan_submit(&snap, &prs, &forge_repo(), "profile", &replan_comments, None).unwrap();
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
