//! Taking over someone else's pull request, and giving it back.
//!
//! Someone opens a pull request on a stack to help, and wants the stack's
//! owner to push it over the line. Taking it over makes its branch a
//! bookmark here, and that is all it does to the repository: the backend's
//! `take_over_branch` is a track that is recorded. From then on the pull
//! request is a segment of the stack like any other. Publish pushes it,
//! Land merges it, and what was held under its branch is the user's to
//! rewrite again.
//!
//! What stays different is what the plans say. A push now moves a branch
//! someone else made, so every plan that pushes it names the pull request
//! and its author, and a push the remote refuses because they pushed again
//! says that, and what to do. Giving it back is the reverse.

use jiji_core::backend::BackendError;
use jiji_core::snapshot::{RepoSnapshot, SyncState, TakenOver};
use serde::{Deserialize, Serialize};

use crate::{ForgeError, ForgeRepo, PrState, PrSummary, SubmitStepStatus};

/// What the user named: a pull request by its number, or a branch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TakeOverTarget {
    Number(u64),
    Branch(String),
}

impl TakeOverTarget {
    /// "9" and "#9" name a pull request; anything else is a branch.
    pub fn parse(text: &str) -> Self {
        let text = text.trim();
        match text.strip_prefix('#').unwrap_or(text).parse::<u64>() {
            Ok(number) if number > 0 => Self::Number(number),
            _ => Self::Branch(text.to_owned()),
        }
    }
}

/// One step of taking a pull request over, or of giving it back.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum TakeOverAction {
    /// Fetch, so that the branch here is what GitHub has now.
    Fetch { remote: String },
    /// Make the branch a bookmark here, and record whose it was.
    #[serde(rename_all = "camelCase")]
    TakeOver {
        bookmark: String,
        remote: String,
        number: u64,
        author: Option<String>,
    },
    /// The reverse: no bookmark here any more, the branch left as it is.
    #[serde(rename_all = "camelCase")]
    GiveBack {
        bookmark: String,
        remote: String,
        number: u64,
        author: Option<String>,
    },
}

/// What taking a pull request over, or giving it back, will do. Made
/// before anything runs, and shown first: the notes say what it means.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TakeOverPlan {
    /// True for a plan that gives the pull request back.
    pub giving_back: bool,
    pub number: u64,
    pub author: Option<String>,
    /// The pull request's title; empty when GitHub was not asked.
    pub title: String,
    pub branch: String,
    /// The branch the pull request is based on, when GitHub was asked.
    pub base_branch: Option<String>,
    pub remote: String,
    pub actions: Vec<TakeOverAction>,
    pub blockers: Vec<String>,
    pub warnings: Vec<String>,
}

impl TakeOverPlan {
    /// "#9 by @kim", or "#9" when GitHub names no author.
    pub fn who(&self) -> String {
        by_author(self.number, self.author.as_deref())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TakeOverStep {
    pub action: TakeOverAction,
    pub status: SubmitStepStatus,
    pub detail: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TakeOverOutcome {
    pub steps: Vec<TakeOverStep>,
    pub failed: bool,
}

/// The repository, as taking over needs it.
pub trait TakeOverVcs {
    fn git_fetch(&self, remote: &str) -> Result<String, BackendError>;
    /// `RepoBackend::take_over_branch`; the answer is its summary.
    fn take_over(
        &self,
        bookmark: &str,
        remote: &str,
        number: u64,
        author: Option<&str>,
    ) -> Result<String, BackendError>;
    /// `RepoBackend::give_back_branch`; the answer is its summary.
    fn give_back(&self, bookmark: &str, remote: &str) -> Result<String, BackendError>;
}

fn by_author(number: u64, author: Option<&str>) -> String {
    match author {
        Some(login) => format!("#{number} by @{login}"),
        None => format!("#{number}"),
    }
}

/// "@kim", or "its author" when GitHub names none.
fn author_of(author: Option<&str>) -> String {
    author.map_or_else(|| "its author".to_owned(), |login| format!("@{login}"))
}

/// The record that `bookmark` was taken over, when it stands for the pull
/// request the bookmark has now. A record for another pull request is a
/// name used again, and says nothing about this one.
pub fn taken_over(
    snapshot: &RepoSnapshot,
    bookmark: &str,
    pr: Option<&PrSummary>,
) -> Option<TakenOver> {
    let entry = snapshot
        .taken_over
        .iter()
        .find(|entry| entry.bookmark == bookmark)?;
    match pr {
        Some(pr) if pr.number != entry.number => None,
        // GitHub's word for the author, when there is one now.
        Some(pr) if pr.author.is_some() => Some(TakenOver {
            author: pr.author.clone(),
            ..entry.clone()
        }),
        _ => Some(entry.clone()),
    }
}

/// Plan taking `pr` over. Read-only.
///
/// `pr` is the pull request as GitHub has it now: asked for by number, so
/// that a closed or merged one is told as that, or found among the open
/// ones by its branch.
pub fn plan_take_over(snapshot: &RepoSnapshot, repo: &ForgeRepo, pr: &PrSummary) -> TakeOverPlan {
    let who = pr.by_author();
    let author = author_of(pr.author.as_deref());
    let branch = pr.head_branch.as_str();
    let remote = repo.remote.as_str();
    let mut blockers: Vec<String> = Vec::new();

    match pr.state {
        PrState::Open => {}
        PrState::Merged => blockers.push(format!(
            "{who} has merged: there is nothing left to take over"
        )),
        PrState::Closed => blockers.push(format!(
            "{who} is closed. Reopen it on GitHub, and take it over then"
        )),
    }
    // A fork's branch is in another repository. Nothing here fetches it
    // as a branch or pushes to it, so there is nothing to make a bookmark
    // of that Publish could push.
    let from_this_repository = pr
        .head_owner
        .as_deref()
        .is_some_and(|owner| owner.eq_ignore_ascii_case(&repo.owner));
    if !from_this_repository {
        blockers.push(format!(
            "{who} is from a fork{}: its branch \u{201c}{branch}\u{201d} is not on {remote}, and \
             Jiji pushes only there. GitHub lets a maintainer push to a fork's branch when \
             its author allows edits, and that takes the fork as a git remote of its own, \
             which Jiji does not set up. To look at its commits here, fetch it for review \
             (pr fetch {})",
            pr.head_owner
                .as_deref()
                .map(|owner| format!(" ({owner}/{})", repo.name))
                .unwrap_or_default(),
            pr.number
        ));
    } else if let Some(here) = snapshot
        .bookmarks
        .iter()
        .find(|b| b.is_local && b.name == branch)
    {
        blockers.push(
            if snapshot.taken_over.iter().any(|entry| entry.bookmark == branch) {
                format!(
                    "{who} is taken over already: \u{201c}{branch}\u{201d} is a bookmark here \
                     (give-back {} hands it back)",
                    pr.number
                )
            } else if here.remote.is_some() {
                format!(
                    "\u{201c}{branch}\u{201d} is a bookmark of yours already, and {who} a part \
                     of your stack: there is nothing to take over"
                )
            } else {
                format!(
                    "a bookmark \u{201c}{branch}\u{201d} exists here and is not their branch (it \
                     follows nothing on {remote}). Rename it, and take {who} over then"
                )
            },
        );
    }

    // A pull request is taken over where it becomes a part of a stack of
    // the user's: on trunk, or on a branch that is a bookmark here. One
    // that rests on another pull request of someone else's would sit on
    // commits that are not the user's, and no publish could base it.
    let base = pr.base_branch.as_str();
    let rests_here = base == snapshot.trunk_bookmark
        || snapshot
            .bookmarks
            .iter()
            .any(|b| b.is_local && b.name == base);
    if from_this_repository && !rests_here {
        blockers.push(format!(
            "{who} is based on \u{201c}{base}\u{201d}, which is neither {} nor a bookmark here: \
             taken over, it would be no part of a stack of yours. If \u{201c}{base}\u{201d} is \
             the branch of another pull request, take that one over first (take-over {base})",
            snapshot.trunk_bookmark
        ));
    }

    let mut warnings = vec![
        format!(
            "Nothing is pushed now. \u{201c}{branch}\u{201d} becomes a bookmark here, at what \
             GitHub has for it, and {who} a part of your stack: Publish pushes it with the \
             rest, and Land merges it and removes its branch once it has landed"
        ),
        format!(
            "If \u{201c}{branch}\u{201d} was built on an earlier copy of your changes (you \
             amended them since, or landed what was under them), its own changes are rebased \
             onto the copy you have now, here only. The next Publish pushes that, and says so"
        ),
        format!(
            "From then on Jiji pushes to a branch {author} made. Each push goes through only \
             if the branch is where the last fetch saw it: a commit pushed to it in the \
             meantime is never overwritten, and the push says so"
        ),
        format!(
            "{who} stays {author}'s pull request on GitHub, and the commits keep their \
             author. Give back (give-back {}) hands it back",
            pr.number
        ),
    ];
    if let Some(seat) = &pr.stack {
        warnings.push(format!(
            "{who} is in GitHub stack {}. The next publish of your stack says what becomes of \
             that stack",
            seat.number
        ));
    }

    TakeOverPlan {
        giving_back: false,
        number: pr.number,
        author: pr.author.clone(),
        title: pr.title.clone(),
        branch: branch.to_owned(),
        base_branch: Some(pr.base_branch.clone()),
        remote: remote.to_owned(),
        actions: vec![
            TakeOverAction::Fetch {
                remote: remote.to_owned(),
            },
            TakeOverAction::TakeOver {
                bookmark: branch.to_owned(),
                remote: remote.to_owned(),
                number: pr.number,
                author: pr.author.clone(),
            },
        ],
        blockers,
        warnings,
    }
}

/// Plan giving back what `target` names. Read-only. `open` are GitHub's
/// open pull requests when they are at hand; they only supply the title.
///
/// Only a branch that was taken over can be given back: anything else is
/// the user's own, and is refused as an error, not as a blocked plan.
pub fn plan_give_back(
    snapshot: &RepoSnapshot,
    open: &[PrSummary],
    target: &TakeOverTarget,
) -> Result<TakeOverPlan, ForgeError> {
    let entry = snapshot
        .taken_over
        .iter()
        .find(|entry| match target {
            TakeOverTarget::Number(number) => entry.number == *number,
            TakeOverTarget::Branch(branch) => entry.bookmark == *branch,
        })
        .ok_or_else(|| {
            ForgeError::Plan(match target {
                TakeOverTarget::Number(number) => format!(
                    "#{number} was not taken over here, so there is nothing to give back"
                ),
                TakeOverTarget::Branch(branch) => format!(
                    "\u{201c}{branch}\u{201d} was not taken over here, so there is nothing to \
                     give back"
                ),
            })
        })?;
    let pr = open.iter().find(|pr| pr.number == entry.number);
    let who = entry.by_author();
    let (branch, remote) = (entry.bookmark.as_str(), entry.remote.as_str());
    let mut blockers: Vec<String> = Vec::new();

    // What is here and not there would be left without a bookmark, as a
    // second copy of their changes. A branch goes back as its remote has
    // it.
    let here = snapshot
        .bookmarks
        .iter()
        .find(|b| b.is_local && b.name == branch);
    let conflicted = snapshot
        .conflicts
        .iter()
        .any(|c| c.id == format!("bookmark-{branch}"));
    let differs = match here.map(|b| b.sync) {
        _ if conflicted => Some("it is conflicted: it was changed here and pushed to there"),
        Some(SyncState::Synced) => None,
        Some(SyncState::Ahead) => Some("there are changes here that were not published"),
        Some(SyncState::Behind) => Some("it was moved back here"),
        Some(SyncState::Diverged) => {
            Some("it was rewritten here, and the rewrite was not published")
        }
        _ => Some("it does not follow that branch any more"),
    };
    if let Some(why) = differs {
        blockers.push(format!(
            "\u{201c}{branch}\u{201d} is not what {remote} has for it: {why}. Publish it, or \
             point it back at {branch}@{remote}, and give it back then"
        ));
    }
    // Work of the user's own on top of it would be left resting on
    // someone else's branch: jj would call it, and the stack under it,
    // history that is theirs.
    if let Some(at) = here.map(|b| b.target.as_str()) {
        let mut on_it: Vec<&str> = vec![at];
        // Nodes come children before parents: one pass from the bottom up
        // finds everything that has `at` below it.
        for node in snapshot.nodes.iter().rev() {
            if node.parents.iter().any(|p| on_it.contains(&p.as_str())) {
                on_it.push(node.id.as_str());
            }
        }
        let mut resting: Vec<&str> = snapshot
            .bookmarks
            .iter()
            .filter(|b| b.is_local && !b.is_trunk && b.name != branch)
            .filter(|b| on_it.contains(&b.target.as_str()))
            .map(|b| b.name.as_str())
            .collect();
        resting.sort_unstable();
        resting.dedup();
        if !resting.is_empty() {
            let one = resting.len() == 1;
            let listed: Vec<String> = resting
                .iter()
                .map(|name| format!("\u{201c}{name}\u{201d}"))
                .collect();
            blockers.push(format!(
                "{} of yours {} on \u{201c}{branch}\u{201d}: given back, that work would rest on \
                 someone else's branch, and jj would call the stack under it theirs. Move \
                 {} off first, or keep \u{201c}{branch}\u{201d}",
                match listed.as_slice() {
                    [one] => one.clone(),
                    [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
                    [] => unreachable!("checked non-empty"),
                },
                if one { "sits" } else { "sit" },
                if one { "it" } else { "them" },
            ));
        }
    }

    Ok(TakeOverPlan {
        giving_back: true,
        number: entry.number,
        author: entry.author.clone(),
        title: pr.map(|pr| pr.title.clone()).unwrap_or_default(),
        branch: branch.to_owned(),
        base_branch: pr.map(|pr| pr.base_branch.clone()),
        remote: remote.to_owned(),
        actions: vec![TakeOverAction::GiveBack {
            bookmark: branch.to_owned(),
            remote: remote.to_owned(),
            number: entry.number,
            author: entry.author.clone(),
        }],
        blockers,
        warnings: vec![
            format!(
                "\u{201c}{branch}\u{201d} stops being a bookmark here. The branch on {remote} is \
                 not touched, and nothing is pushed"
            ),
            format!(
                "{who} is someone else's again: Publish and Land leave its branch alone, and \
                 the changes of yours under it read as immutable while its branch builds on \
                 them"
            ),
        ],
    })
}

/// Run a plan made by [`plan_take_over`] or [`plan_give_back`]. A step
/// that fails stops the run; nothing here needs undoing, because the
/// fetch changes nothing of the user's and the other step is one
/// operation.
pub fn execute_take_over(
    plan: &TakeOverPlan,
    vcs: &dyn TakeOverVcs,
) -> Result<TakeOverOutcome, ForgeError> {
    if !plan.blockers.is_empty() {
        return Err(ForgeError::Plan(format!(
            "the plan has blockers: {}",
            plan.blockers.join("; ")
        )));
    }
    let mut steps: Vec<TakeOverStep> = plan
        .actions
        .iter()
        .map(|action| TakeOverStep {
            action: action.clone(),
            status: SubmitStepStatus::Skipped,
            detail: None,
        })
        .collect();
    let mut failed = false;
    for step in &mut steps {
        let result = match &step.action {
            TakeOverAction::Fetch { remote } => vcs.git_fetch(remote),
            TakeOverAction::TakeOver {
                bookmark,
                remote,
                number,
                author,
            } => vcs.take_over(bookmark, remote, *number, author.as_deref()),
            TakeOverAction::GiveBack {
                bookmark, remote, ..
            } => vcs.give_back(bookmark, remote),
        };
        match result {
            Ok(detail) => {
                step.status = SubmitStepStatus::Done;
                step.detail = Some(detail);
            }
            Err(err) => {
                step.status = SubmitStepStatus::Failed;
                step.detail = Some(err.to_string());
                failed = true;
                break;
            }
        }
    }
    Ok(TakeOverOutcome { steps, failed })
}

/// The branches a push named as having moved on the remote, read off the
/// backend's refusal ("a, b moved on the remote since the last fetch",
/// with or without "Pushed x to origin, but" before it).
fn refused_by_lease(error: &str) -> Vec<&str> {
    let Some(end) = error.find(" moved on the remote since the last fetch") else {
        return Vec::new();
    };
    let named = &error[..end];
    let named = named.rsplit(", but ").next().unwrap_or(named);
    let named = named.rsplit(": ").next().unwrap_or(named);
    named.split(", ").map(str::trim).collect()
}

/// What to say after a push that the remote refused because a branch had
/// moved, when a branch among the refused is one that was taken over:
/// its author pushed to it again. `then` is what to do once the two are
/// brought together ("publish again"). `None` for any other failure.
pub fn pushed_to_again(error: &str, taken: &[TakenOver], then: &str) -> Option<String> {
    let refused = refused_by_lease(error);
    let stories: Vec<String> = taken
        .iter()
        .filter(|entry| refused.contains(&entry.bookmark.as_str()))
        .map(|entry| {
            format!(
                "\u{201c}{}\u{201d} is {}'s branch, taken over: it was pushed to again since \
                 the last fetch, and nothing there was overwritten. Fetch: the new commits \
                 arrive beside yours, and \u{201c}{}\u{201d} reads as conflicted until you have \
                 brought the two together (rebase theirs onto yours, or take theirs) and \
                 pointed it at the result. Then {then}",
                entry.bookmark,
                entry.by_author(),
                entry.bookmark
            )
        })
        .collect();
    (!stories.is_empty()).then(|| stories.join(". "))
}

/// The refusal for a taken-over bookmark that reads as conflicted: that
/// is what a fetch leaves after its author pushed to it again while it
/// was changed here.
pub(crate) fn conflicted_story(entry: &TakenOver) -> String {
    format!(
        "\u{201c}{branch}\u{201d} is conflicted: it is {who}'s branch, taken over, and it was \
         pushed to again after it was changed here. The new commits are on \
         {branch}@{remote}. Bring the two together (rebase theirs onto yours, or take \
         theirs), point \u{201c}{branch}\u{201d} at the result, and publish again",
        branch = entry.bookmark,
        who = entry.by_author(),
        remote = entry.remote
    )
}

/// The note for a plan that pushes taken-over branches: whose they are,
/// and that the push cannot overwrite what was pushed to them since.
pub(crate) fn pushes_their_branch(taken: &[&TakenOver]) -> Option<String> {
    let listed: Vec<String> = taken
        .iter()
        .map(|entry| {
            format!(
                "\u{201c}{}\u{201d} ({}, taken over)",
                entry.bookmark,
                entry.by_author()
            )
        })
        .collect();
    let named = match listed.as_slice() {
        [] => return None,
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    };
    let one = taken.len() == 1;
    Some(format!(
        "This pushes {named}: {} someone else made. {} goes through only if the branch is \
         where the last fetch saw it, so nothing pushed there since is overwritten",
        if one { "a branch" } else { "branches" },
        if one { "The push" } else { "Each push" },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pr::{ChecksRollup, ReviewDecision, StackSeat};
    use crate::remote::ForgeProvider;
    use jiji_core::snapshot::{
        BookmarkState, ConflictItem, ConflictKind, GraphNode, NodeKind, SnapshotView,
    };
    use std::cell::RefCell;

    fn node(id: &str, parents: &[&str], bookmarks: &[&str]) -> GraphNode {
        GraphNode {
            id: id.into(),
            change_id: id.into(),
            commit_id: format!("c{id}"),
            description: format!("change {id}"),
            author: "Test <t@example.com>".into(),
            timestamp: "2026-07-01T12:00:00Z".into(),
            kind: NodeKind::Mutable,
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

    fn bookmark(name: &str, target: &str, sync: SyncState) -> BookmarkState {
        BookmarkState {
            name: name.into(),
            target: target.into(),
            remote: (sync != SyncState::LocalOnly).then(|| "origin".into()),
            sync,
            is_trunk: name == "main",
            is_local: sync != SyncState::RemoteOnly,
            tracked: !matches!(sync, SyncState::LocalOnly | SyncState::RemoteOnly),
            is_merged: false,
            target_description: String::new(),
            target_author: String::new(),
            target_timestamp: String::new(),
        }
    }

    /// main(m) ── a1 (auth) ── b1 (profile), and above it t1, which is
    /// their branch "their-fix": fetched, and no bookmark here.
    fn stack_with_theirs() -> RepoSnapshot {
        RepoSnapshot {
            repo_path: "/tmp/repo".into(),
            repo_name: "repo".into(),
            backend: "test".into(),
            view: SnapshotView::default(),
            truncated: false,
            trunk_bookmark: "main".into(),
            working_copy: "t1".into(),
            workspaces: vec![],
            workstreams: vec![],
            nodes: vec![
                node("t1", &["b1"], &[]),
                node("b1", &["a1"], &["profile"]),
                node("a1", &["m"], &["auth"]),
                node("m", &[], &["main"]),
            ],
            bookmarks: vec![
                bookmark("main", "m", SyncState::Synced),
                bookmark("auth", "a1", SyncState::Synced),
                bookmark("profile", "b1", SyncState::Synced),
                bookmark("their-fix", "t1", SyncState::RemoteOnly),
            ],
            git_remotes: vec![],
            conflicts: vec![],
            operations: vec![],
            resolve_tool: None,
            taken_over: Vec::new(),
        }
    }

    /// The same once "their-fix" has been taken over for #9 by kim.
    fn taken(sync: SyncState) -> RepoSnapshot {
        let mut snapshot = stack_with_theirs();
        snapshot.nodes[0].bookmarks = vec!["their-fix".into()];
        snapshot.bookmarks[3] = bookmark("their-fix", "t1", sync);
        snapshot.taken_over = vec![TakenOver {
            bookmark: "their-fix".into(),
            remote: "origin".into(),
            number: 9,
            author: Some("kim".into()),
        }];
        snapshot
    }

    fn repo() -> ForgeRepo {
        ForgeRepo {
            provider: ForgeProvider::GitHub,
            remote: "origin".into(),
            host: "github.com".into(),
            owner: "o".into(),
            name: "r".into(),
        }
    }

    fn their_pr() -> PrSummary {
        PrSummary {
            number: 9,
            title: "A helping change".into(),
            url: "https://github.com/o/r/pull/9".into(),
            state: PrState::Open,
            is_draft: false,
            head_branch: "their-fix".into(),
            head_commit: "ct1".into(),
            head_owner: Some("o".into()),
            base_branch: "profile".into(),
            body: None,
            review: ReviewDecision::None,
            checks: ChecksRollup::None,
            merge_commit: None,
            stack: None,
            author: Some("kim".into()),
        }
    }

    /// What was asked of the repository, in order; `fail` names the call
    /// that answers with an error.
    #[derive(Default)]
    struct Vcs {
        calls: RefCell<Vec<String>>,
        fail: Option<&'static str>,
    }

    impl Vcs {
        fn answer(&self, call: String, kind: &str) -> Result<String, BackendError> {
            self.calls.borrow_mut().push(call.clone());
            if self.fail == Some(kind) {
                Err(BackendError::MutationFailed(format!("no {kind} today")))
            } else {
                Ok(format!("did {call}"))
            }
        }
    }

    impl TakeOverVcs for Vcs {
        fn git_fetch(&self, remote: &str) -> Result<String, BackendError> {
            self.answer(format!("fetch {remote}"), "fetch")
        }
        fn take_over(
            &self,
            bookmark: &str,
            remote: &str,
            number: u64,
            author: Option<&str>,
        ) -> Result<String, BackendError> {
            self.answer(
                format!("take over {bookmark}@{remote} #{number} {}", author.unwrap_or("?")),
                "take",
            )
        }
        fn give_back(&self, bookmark: &str, remote: &str) -> Result<String, BackendError> {
            self.answer(format!("give back {bookmark}@{remote}"), "give")
        }
    }

    #[test]
    fn a_target_is_a_number_or_a_branch() {
        assert_eq!(TakeOverTarget::parse("9"), TakeOverTarget::Number(9));
        assert_eq!(TakeOverTarget::parse("#31"), TakeOverTarget::Number(31));
        assert_eq!(
            TakeOverTarget::parse("their-fix"),
            TakeOverTarget::Branch("their-fix".into())
        );
        // Not a pull request number: a branch can be called anything.
        assert_eq!(TakeOverTarget::parse("0"), TakeOverTarget::Branch("0".into()));
        assert_eq!(
            TakeOverTarget::parse("#not-a-number"),
            TakeOverTarget::Branch("#not-a-number".into())
        );
    }

    /// The plan is what the user reads before anything happens: a fetch,
    /// then the one step, and in its notes what taking over means. Run, it
    /// fetches first and takes the branch over for that pull request and
    /// author; a fetch that fails leaves the branch as it was.
    #[test]
    fn taking_over_says_what_it_means_and_fetches_first() {
        let plan = plan_take_over(&stack_with_theirs(), &repo(), &their_pr());
        assert!(!plan.giving_back);
        assert_eq!(plan.who(), "#9 by @kim");
        assert_eq!(plan.base_branch.as_deref(), Some("profile"));
        assert!(plan.blockers.is_empty(), "{:?}", plan.blockers);
        assert_eq!(
            plan.actions,
            [
                TakeOverAction::Fetch {
                    remote: "origin".into()
                },
                TakeOverAction::TakeOver {
                    bookmark: "their-fix".into(),
                    remote: "origin".into(),
                    number: 9,
                    author: Some("kim".into()),
                },
            ]
        );
        assert_eq!(
            plan.warnings,
            [
                "Nothing is pushed now. \u{201c}their-fix\u{201d} becomes a bookmark here, at what \
                 GitHub has for it, and #9 by @kim a part of your stack: Publish pushes it with \
                 the rest, and Land merges it and removes its branch once it has landed",
                "If \u{201c}their-fix\u{201d} was built on an earlier copy of your changes (you \
                 amended them since, or landed what was under them), its own changes are \
                 rebased onto the copy you have now, here only. The next Publish pushes that, \
                 and says so",
                "From then on Jiji pushes to a branch @kim made. Each push goes through only if \
                 the branch is where the last fetch saw it: a commit pushed to it in the \
                 meantime is never overwritten, and the push says so",
                "#9 by @kim stays @kim's pull request on GitHub, and the commits keep their \
                 author. Give back (give-back 9) hands it back",
            ]
        );

        let vcs = Vcs::default();
        let outcome = execute_take_over(&plan, &vcs).unwrap();
        assert!(!outcome.failed);
        assert_eq!(
            *vcs.calls.borrow(),
            ["fetch origin", "take over their-fix@origin #9 kim"]
        );
        assert_eq!(
            outcome.steps[1].detail.as_deref(),
            Some("did take over their-fix@origin #9 kim")
        );

        let vcs = Vcs {
            fail: Some("fetch"),
            ..Vcs::default()
        };
        let outcome = execute_take_over(&plan, &vcs).unwrap();
        assert!(outcome.failed);
        assert_eq!(*vcs.calls.borrow(), ["fetch origin"]);
        assert_eq!(outcome.steps[0].status, SubmitStepStatus::Failed);
        assert_eq!(outcome.steps[1].status, SubmitStepStatus::Skipped);
    }

    /// A fork's branch is in another repository. Nothing here fetches it
    /// as a branch or pushes to it, so it cannot be taken over, and the
    /// plan says what GitHub would allow and what Jiji does not do.
    #[test]
    fn a_pull_request_from_a_fork_is_refused_with_the_reason() {
        let from_a_fork = PrSummary {
            head_owner: Some("kim".into()),
            ..their_pr()
        };
        let plan = plan_take_over(&stack_with_theirs(), &repo(), &from_a_fork);
        assert_eq!(
            plan.blockers,
            ["#9 by @kim is from a fork (kim/r): its branch \u{201c}their-fix\u{201d} is not on \
              origin, and Jiji pushes only there. GitHub lets a maintainer push to a fork's \
              branch when its author allows edits, and that takes the fork as a git remote of \
              its own, which Jiji does not set up. To look at its commits here, fetch it for \
              review (pr fetch 9)"]
        );
        assert!(execute_take_over(&plan, &Vcs::default()).is_err());
        // A fork that is gone has no owner to name.
        let gone = PrSummary {
            head_owner: None,
            ..their_pr()
        };
        let plan = plan_take_over(&stack_with_theirs(), &repo(), &gone);
        assert!(plan.blockers[0].starts_with("#9 by @kim is from a fork: its branch"));
        // The owner's name is compared as GitHub compares it.
        let same = PrSummary {
            head_owner: Some("O".into()),
            ..their_pr()
        };
        assert!(plan_take_over(&stack_with_theirs(), &repo(), &same).blockers.is_empty());
    }

    #[test]
    fn what_is_closed_merged_or_a_bookmark_here_already_is_not_taken_over() {
        let blocked = |snapshot: &RepoSnapshot, pr: &PrSummary| {
            let plan = plan_take_over(snapshot, &repo(), pr);
            assert_eq!(plan.blockers.len(), 1, "{:?}", plan.blockers);
            plan.blockers[0].clone()
        };
        let merged = PrSummary {
            state: PrState::Merged,
            ..their_pr()
        };
        assert_eq!(
            blocked(&stack_with_theirs(), &merged),
            "#9 by @kim has merged: there is nothing left to take over"
        );
        let closed = PrSummary {
            state: PrState::Closed,
            ..their_pr()
        };
        assert_eq!(
            blocked(&stack_with_theirs(), &closed),
            "#9 by @kim is closed. Reopen it on GitHub, and take it over then"
        );
        // Taken over already.
        assert_eq!(
            blocked(&taken(SyncState::Synced), &their_pr()),
            "#9 by @kim is taken over already: \u{201c}their-fix\u{201d} is a bookmark here \
             (give-back 9 hands it back)"
        );
        // A bookmark of the user's that follows the branch, with no record:
        // tracked by hand, or the user's own from the start.
        let mut own = taken(SyncState::Synced);
        own.taken_over.clear();
        assert_eq!(
            blocked(&own, &their_pr()),
            "\u{201c}their-fix\u{201d} is a bookmark of yours already, and #9 by @kim a part of \
             your stack: there is nothing to take over"
        );
        // A bookmark of that name that is not their branch.
        let mut clash = taken(SyncState::LocalOnly);
        clash.taken_over.clear();
        assert_eq!(
            blocked(&clash, &their_pr()),
            "a bookmark \u{201c}their-fix\u{201d} exists here and is not their branch (it follows \
             nothing on origin). Rename it, and take #9 by @kim over then"
        );
        // One that rests on another pull request of someone else's: the
        // one under it comes first.
        let on_theirs = PrSummary {
            number: 10,
            head_branch: "their-next".into(),
            base_branch: "their-fix".into(),
            ..their_pr()
        };
        assert_eq!(
            blocked(&stack_with_theirs(), &on_theirs),
            "#10 by @kim is based on \u{201c}their-fix\u{201d}, which is neither main nor a \
             bookmark here: taken over, it would be no part of a stack of yours. If \
             \u{201c}their-fix\u{201d} is the branch of another pull request, take that one over \
             first (take-over their-fix)"
        );
        // With that one taken over, or straight on trunk, it is one.
        assert!(plan_take_over(&taken(SyncState::Synced), &repo(), &on_theirs).blockers.is_empty());
        let on_trunk = PrSummary {
            base_branch: "main".into(),
            ..their_pr()
        };
        assert!(plan_take_over(&stack_with_theirs(), &repo(), &on_trunk).blockers.is_empty());
    }

    /// Giving back is the reverse, for what was taken over and nothing
    /// else, by number or by branch.
    #[test]
    fn giving_back_is_for_what_was_taken_over() {
        let snapshot = taken(SyncState::Synced);
        for target in [
            TakeOverTarget::Number(9),
            TakeOverTarget::Branch("their-fix".into()),
        ] {
            let plan = plan_give_back(&snapshot, &[their_pr()], &target).unwrap();
            assert!(plan.giving_back);
            assert_eq!((plan.who(), plan.title.as_str()), ("#9 by @kim".into(), "A helping change"));
            assert!(plan.blockers.is_empty(), "{:?}", plan.blockers);
            assert_eq!(
                plan.actions,
                [TakeOverAction::GiveBack {
                    bookmark: "their-fix".into(),
                    remote: "origin".into(),
                    number: 9,
                    author: Some("kim".into()),
                }]
            );
            assert_eq!(
                plan.warnings,
                [
                    "\u{201c}their-fix\u{201d} stops being a bookmark here. The branch on origin is \
                     not touched, and nothing is pushed",
                    "#9 by @kim is someone else's again: Publish and Land leave its branch \
                     alone, and the changes of yours under it read as immutable while its \
                     branch builds on them",
                ]
            );
            let vcs = Vcs::default();
            assert!(!execute_take_over(&plan, &vcs).unwrap().failed);
            assert_eq!(*vcs.calls.borrow(), ["give back their-fix@origin"]);
        }
        // GitHub is not needed for it.
        let plan = plan_give_back(&snapshot, &[], &TakeOverTarget::Number(9)).unwrap();
        assert_eq!((plan.title.as_str(), plan.base_branch.as_deref()), ("", None));

        // The user's own, and what is not here at all, are not given back.
        for (target, told) in [
            (
                TakeOverTarget::Number(7),
                "#7 was not taken over here, so there is nothing to give back",
            ),
            (
                TakeOverTarget::Branch("profile".into()),
                "\u{201c}profile\u{201d} was not taken over here, so there is nothing to give back",
            ),
        ] {
            let err = plan_give_back(&snapshot, &[], &target).unwrap_err();
            assert!(err.to_string().contains(told), "{err}");
        }
    }

    /// A branch goes back as its remote has it, and with nothing of the
    /// user's left resting on it.
    #[test]
    fn giving_back_is_refused_while_something_would_be_left_behind() {
        let blocked = |snapshot: &RepoSnapshot| {
            plan_give_back(snapshot, &[], &TakeOverTarget::Number(9))
                .unwrap()
                .blockers
        };
        for (sync, why) in [
            (SyncState::Ahead, "there are changes here that were not published"),
            (
                SyncState::Diverged,
                "it was rewritten here, and the rewrite was not published",
            ),
            (SyncState::Behind, "it was moved back here"),
        ] {
            assert_eq!(
                blocked(&taken(sync)),
                [format!(
                    "\u{201c}their-fix\u{201d} is not what origin has for it: {why}. Publish it, or \
                     point it back at their-fix@origin, and give it back then"
                )]
            );
        }
        let mut conflicted = taken(SyncState::Diverged);
        conflicted.conflicts.push(ConflictItem {
            id: "bookmark-their-fix".into(),
            kind: ConflictKind::Bookmark,
            summary: String::new(),
            node_id: None,
            paths: vec![],
            more_paths: 0,
            targets: vec![],
            workspace: None,
        });
        assert!(
            blocked(&conflicted)[0].contains(
                "it is conflicted: it was changed here and pushed to there"
            ),
            "{:?}",
            blocked(&conflicted)
        );

        // Work of the user's own on top of it.
        let mut built_on = taken(SyncState::Synced);
        built_on.nodes.insert(0, node("u1", &["t1"], &["mine"]));
        built_on.bookmarks.push(bookmark("mine", "u1", SyncState::LocalOnly));
        assert_eq!(
            blocked(&built_on),
            ["\u{201c}mine\u{201d} of yours sits on \u{201c}their-fix\u{201d}: given back, that work \
              would rest on someone else's branch, and jj would call the stack under it theirs. \
              Move it off first, or keep \u{201c}their-fix\u{201d}"]
        );
        // What is below it, or beside it, does not count.
        assert!(blocked(&taken(SyncState::Synced)).is_empty());
    }

    /// The record names a pull request. A bookmark of that name that has
    /// another pull request now is a name used again.
    #[test]
    fn the_record_stands_for_the_pull_request_it_names() {
        let snapshot = taken(SyncState::Synced);
        assert_eq!(
            taken_over(&snapshot, "their-fix", Some(&their_pr())).map(|t| t.by_author()),
            Some("#9 by @kim".into())
        );
        // GitHub's word for the author wins over what was recorded.
        let renamed = PrSummary {
            author: Some("kim-b".into()),
            ..their_pr()
        };
        assert_eq!(
            taken_over(&snapshot, "their-fix", Some(&renamed)).unwrap().author.as_deref(),
            Some("kim-b")
        );
        // Without GitHub the record speaks.
        assert_eq!(taken_over(&snapshot, "their-fix", None).unwrap().number, 9);
        let another = PrSummary {
            number: 12,
            ..their_pr()
        };
        assert_eq!(taken_over(&snapshot, "their-fix", Some(&another)), None);
        assert_eq!(taken_over(&snapshot, "profile", None), None);
    }

    #[test]
    fn the_refused_branches_are_read_off_the_backends_words() {
        assert_eq!(
            refused_by_lease(
                "Could not apply the change: their-fix moved on the remote since the last \
                 fetch; fetch, reconcile, and push again"
            ),
            ["their-fix"]
        );
        assert_eq!(
            refused_by_lease(
                "Could not apply the change: Pushed upper to origin, but their-fix, other \
                 moved on the remote since the last fetch; fetch, reconcile, and push again"
            ),
            ["their-fix", "other"]
        );
        assert!(refused_by_lease("the remote hung up").is_empty());
    }

    /// A push that the remote refused because a branch had moved: when it
    /// is a branch that was taken over, its author pushed to it again,
    /// and the user is told that and what to do.
    #[test]
    fn a_refused_push_of_a_taken_over_branch_says_it_was_pushed_to_again() {
        let taken = taken(SyncState::Diverged).taken_over;
        let refusal = "Could not apply the change: Pushed profile to origin, but their-fix \
                       moved on the remote since the last fetch; fetch, reconcile, and push again";
        assert_eq!(
            pushed_to_again(refusal, &taken, "publish again").as_deref(),
            Some(
                "\u{201c}their-fix\u{201d} is #9 by @kim's branch, taken over: it was pushed to \
                 again since the last fetch, and nothing there was overwritten. Fetch: the new \
                 commits arrive beside yours, and \u{201c}their-fix\u{201d} reads as conflicted \
                 until you have brought the two together (rebase theirs onto yours, or take \
                 theirs) and pointed it at the result. Then publish again"
            )
        );
        // A branch of the user's own that moved is the user's own story.
        let own = "Could not apply the change: profile moved on the remote since the last \
                   fetch; fetch, reconcile, and push again";
        assert_eq!(pushed_to_again(own, &taken, "publish again"), None);
        assert_eq!(pushed_to_again("the remote hung up", &taken, "publish again"), None);
    }

    /// A pull request in a GitHub stack: the plan says that the next
    /// publish decides what becomes of that stack.
    #[test]
    fn a_pull_request_in_a_github_stack_is_taken_over_with_a_word_about_the_stack() {
        let stacked = PrSummary {
            stack: Some(StackSeat {
                number: 91,
                position: 1,
                size: 2,
                base: "profile".into(),
            }),
            ..their_pr()
        };
        let plan = plan_take_over(&stack_with_theirs(), &repo(), &stacked);
        assert!(plan.blockers.is_empty(), "{:?}", plan.blockers);
        assert_eq!(
            plan.warnings.last().map(String::as_str),
            Some(
                "#9 by @kim is in GitHub stack 91. The next publish of your stack says what \
                 becomes of that stack"
            )
        );
    }
}
