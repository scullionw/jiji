//! GitHub as a test captured it: the pull requests, what each one's merge
//! button says, and the repository's landing rules, answered through the
//! host's own forge seam (`host::forge::Forge`). Publish, review, land,
//! ship and auto-land run their real planning and execution over it with
//! no token and no network, and what they asked of GitHub is kept for the
//! test to read.
//!
//! It answers the way GitHub was seen to (2026-11, on a sandbox
//! repository) where landing depends on it: a merged pull request names
//! the commit it merged as; a pull request in a GitHub stack refuses the
//! plain merge endpoint, auto-merge, the queue mutation and a change of
//! base, and merges through the stacked endpoint with every open pull
//! request below it, all or none; a stack takes two or more open pull
//! requests whose bases chain and grows at the top only.
//!
//! Where it cannot follow GitHub: it never sees a push, so it cannot
//! close a pull request as merged because its head turned up inside its
//! base (a test puts one in that state with [`Pull::merged_as`]); and
//! after a stacked merge it leaves the remaining heads where they were,
//! because the mock repository cannot receive the commits GitHub's own
//! rebase would have pushed.

use std::sync::{Arc, Mutex, MutexGuard};

use jiji_forge::github::{AsyncMerge, Restack};
use jiji_forge::{
    ChecksRollup, CiRerunReport, ExistingComment, ForgeError, ForgeRepo, LandForge, MergeMethod,
    MergeStateStatus, PrLandState, PrState, PrStateReport, PrSummary, RepoPrState, ResolvedToken,
    ReviewDecision, StackCommentSource, StackSeat, SubmitForge, TokenSource,
};

use crate::host::forge::{Forge, ForgeClient};

/// A GitHub a test sets up. Clones share it: the test keeps one to change
/// what GitHub says and to read what it was asked.
#[derive(Clone)]
pub struct Fixture(Arc<Mutex<GitHub>>);

pub struct GitHub {
    /// Where the token comes from; `None` is a Mac with none anywhere.
    pub token: Option<TokenSource>,
    pub login: String,
    pub prs: Vec<Pull>,
    /// The open-PR query hit its cap.
    pub truncated: bool,
    /// The next open-PR query fails, with this message, once.
    pub refuse_prs: Option<String>,
    /// The repository's merge settings.
    pub squash: bool,
    pub rebase: bool,
    pub merge: bool,
    pub auto_merge_allowed: bool,
    /// Trunk is protected by a merge queue that merges this way.
    pub queue: Option<MergeMethod>,
    /// What was asked of GitHub that changes something, in order.
    pub asked: Vec<String>,
    /// The number the next GitHub stack gets.
    pub next_stack: u64,
}

impl GitHub {
    /// A pull request's seat in its GitHub stack, read off the others.
    fn seat(&self, pull: &Pull) -> Option<StackSeat> {
        let (number, position) = pull.stack?;
        Some(StackSeat {
            number,
            position,
            size: self
                .prs
                .iter()
                .filter(|other| other.stack.is_some_and(|(n, _)| n == number))
                .count() as u32,
            base: "main".into(),
        })
    }

    /// A pull request as the open-PR query and the by-number lookup
    /// answer it.
    fn summary(&self, pull: &Pull) -> PrSummary {
        PrSummary {
            stack: self.seat(pull),
            ..pull.summary.clone()
        }
    }
}

/// One pull request and what GitHub says of it beyond the open-PR query.
pub struct Pull {
    pub summary: PrSummary,
    pub conflicting: bool,
    /// The merge button's state; `None` reads it off the review and the
    /// checks, as GitHub does with nothing else required.
    pub merge_state: Option<MergeStateStatus>,
    pub auto_merge: bool,
    pub queued: bool,
    /// The workflows whose last run on the head failed.
    pub failed_runs: Vec<String>,
    pub comment: Option<ExistingComment>,
    /// The GitHub stack this pull request is linked into, and its seat
    /// there (1 at the bottom).
    pub stack: Option<(u64, u32)>,
}

impl Pull {
    /// Closed as merged, as `commit`: what GitHub reports for a real
    /// merge (a commit the repository has yet to fetch) and equally for a
    /// pull request whose head turned up inside its base branch after a
    /// push (then `commit` is its own head, and nothing reached trunk).
    pub fn merged_as(&mut self, commit: &str) {
        self.summary.state = PrState::Merged;
        self.summary.merge_commit = Some(commit.into());
    }

    fn land_state(&self, github: &GitHub) -> PrLandState {
        let pr = &self.summary;
        let derived = if pr.is_draft {
            MergeStateStatus::Draft
        } else if self.conflicting {
            MergeStateStatus::Dirty
        } else if matches!(pr.review, ReviewDecision::Approved | ReviewDecision::None)
            && matches!(pr.checks, ChecksRollup::Passing | ChecksRollup::None)
        {
            MergeStateStatus::Clean
        } else {
            MergeStateStatus::Blocked
        };
        PrLandState {
            node_id: node_id(pr.number),
            state: pr.state,
            is_draft: pr.is_draft,
            mergeable: if self.conflicting {
                jiji_forge::land::Mergeable::Conflicting
            } else {
                jiji_forge::land::Mergeable::Mergeable
            },
            merge_state: self.merge_state.unwrap_or(derived),
            review: pr.review,
            checks: pr.checks,
            base_branch: pr.base_branch.clone(),
            head_commit: pr.head_commit.clone(),
            merge_commit: pr.merge_commit.clone(),
            stack: github.seat(self),
            auto_merge_enabled: self.auto_merge,
            in_merge_queue: self.queued,
            auto_merge_allowed: github.auto_merge_allowed,
            queue_on_base: github.queue.is_some(),
            queue_method: github.queue,
            allows_squash: github.squash,
            allows_merge: github.merge,
            allows_rebase: github.rebase,
        }
    }
}

fn node_id(number: u64) -> String {
    format!("PR_{number}")
}

fn number_of(node_id: &str) -> Option<u64> {
    node_id.strip_prefix("PR_")?.parse().ok()
}

/// An open pull request from `head` into `base`, approved with its checks
/// passing; `commit` is its head's commit id.
pub fn open_pr(number: u64, head: &str, base: &str, commit: &str, title: &str) -> Pull {
    Pull {
        summary: PrSummary {
            number,
            title: title.into(),
            url: format!("https://github.com/jiji-mock/workbench/pull/{number}"),
            state: PrState::Open,
            is_draft: false,
            head_branch: head.into(),
            head_commit: commit.into(),
            head_owner: Some("jiji-mock".into()),
            base_branch: base.into(),
            body: None,
            review: ReviewDecision::Approved,
            checks: ChecksRollup::Passing,
            merge_commit: None,
            stack: None,
            author: None,
        },
        conflicting: false,
        merge_state: None,
        auto_merge: false,
        queued: false,
        failed_runs: Vec::new(),
        comment: None,
        stack: None,
    }
}

/// How GitHub words a refusal the real client turns into an API error.
fn refused(message: &str) -> ForgeError {
    ForgeError::Api(message.to_owned())
}

impl Fixture {
    /// A signed-in `gh` with no pull requests, on a repository that allows
    /// every merge method and auto-merge.
    pub fn new() -> Self {
        Self(Arc::new(Mutex::new(GitHub {
            token: Some(TokenSource::GhCli),
            login: "dev".into(),
            prs: Vec::new(),
            truncated: false,
            refuse_prs: None,
            squash: true,
            rebase: true,
            merge: true,
            auto_merge_allowed: true,
            queue: None,
            asked: Vec::new(),
            next_stack: 90,
        })))
    }

    /// Link these pull requests, bottom first, as a GitHub stack, the way
    /// someone accepting GitHub's banner would; answers its number.
    pub fn link_stack(&self, numbers: &[u64]) -> u64 {
        SubmitForge::create_stack(self, numbers).expect("the pull requests form a stack")
    }

    /// The mock repository's two-segment stack published: #41 for
    /// `diff-model` into trunk, #42 for `diff-virtualization` above it.
    pub fn mock_stack() -> Self {
        let fixture = Self::new();
        fixture.github().prs = vec![
            open_pr(
                41,
                "diff-model",
                "main",
                "8b19f3c2",
                "Sticky file headers in the diff surface",
            ),
            open_pr(
                42,
                "diff-virtualization",
                "diff-model",
                "c5e208d4",
                "Virtualize multi-file diff rendering",
            ),
        ];
        fixture
    }

    pub fn github(&self) -> MutexGuard<'_, GitHub> {
        self.0.lock().expect("fixture lock poisoned")
    }

    pub fn pull(&self, number: u64, edit: impl FnOnce(&mut Pull)) {
        let mut github = self.github();
        let pull = github
            .prs
            .iter_mut()
            .find(|pull| pull.summary.number == number)
            .expect("the fixture has this pull request");
        edit(pull);
    }

    /// What was asked of GitHub since the last look.
    pub fn take_asked(&self) -> Vec<String> {
        std::mem::take(&mut self.github().asked)
    }

    pub fn boxed(&self) -> Box<dyn Forge> {
        Box::new(self.clone())
    }

    fn ask(&self, what: String) {
        self.github().asked.push(what);
    }

    fn with_pull<T>(&self, number: u64, f: impl FnOnce(&mut Pull) -> T) -> Result<T, ForgeError> {
        let mut github = self.github();
        let pull = github
            .prs
            .iter_mut()
            .find(|pull| pull.summary.number == number)
            .ok_or_else(|| ForgeError::NotFound(format!("pull request #{number} was not found")))?;
        Ok(f(pull))
    }
}

impl Forge for Fixture {
    fn token(&self, _: Option<&ForgeRepo>) -> Result<Option<ResolvedToken>, ForgeError> {
        Ok(self.github().token.map(|source| ResolvedToken {
            token: "fixture".into(),
            source,
        }))
    }

    fn store_token(&self, _: Option<&ForgeRepo>, _: &str) -> Result<(), ForgeError> {
        self.github().token = Some(TokenSource::Keychain);
        Ok(())
    }

    fn delete_token(&self, _: Option<&ForgeRepo>) -> Result<(), ForgeError> {
        let mut github = self.github();
        if github.token == Some(TokenSource::Keychain) {
            github.token = None;
        }
        Ok(())
    }

    fn client(&self, _: Option<&ForgeRepo>, _: &str) -> Result<Box<dyn ForgeClient>, ForgeError> {
        Ok(Box::new(self.clone()))
    }
}

impl ForgeClient for Fixture {
    fn viewer(&self) -> Result<String, ForgeError> {
        Ok(self.github().login.clone())
    }

    fn open_prs(&self, repo: &ForgeRepo) -> Result<RepoPrState, ForgeError> {
        let mut github = self.github();
        if let Some(message) = github.refuse_prs.take() {
            return Err(ForgeError::Network(message));
        }
        let report = PrStateReport {
            prs: github
                .prs
                .iter()
                .filter(|pull| pull.summary.state == PrState::Open)
                .map(|pull| github.summary(pull))
                .collect(),
            truncated: github.truncated,
        };
        Ok(RepoPrState::new(report, &repo.owner))
    }

    fn pr(&self, _: &ForgeRepo, number: u64) -> Result<PrSummary, ForgeError> {
        let github = self.github();
        github
            .prs
            .iter()
            .find(|pull| pull.summary.number == number)
            .map(|pull| github.summary(pull))
            .ok_or_else(|| ForgeError::NotFound(format!("pull request #{number} was not found")))
    }

    /// Each failed workflow is sent back to work: its checks read as
    /// running until the test says otherwise.
    fn rerun_failed_ci(&self, _: &ForgeRepo, head: &str) -> Result<CiRerunReport, ForgeError> {
        let mut github = self.github();
        let mut report = CiRerunReport {
            rerun: Vec::new(),
            refused: Vec::new(),
        };
        if let Some(pull) = github
            .prs
            .iter_mut()
            .find(|pull| pull.summary.head_commit == head)
        {
            report.rerun = std::mem::take(&mut pull.failed_runs);
            if !report.rerun.is_empty() {
                pull.summary.checks = ChecksRollup::Pending;
            }
        }
        github.asked.push(format!("rerun {head}"));
        Ok(report)
    }

    fn submit<'a>(&'a self, _: &'a ForgeRepo) -> Box<dyn SubmitForge + 'a> {
        Box::new(self.clone())
    }

    fn land<'a>(&'a self, _: &'a ForgeRepo) -> Box<dyn LandForge + 'a> {
        Box::new(self.clone())
    }

    fn ship<'a>(&'a self, _: &'a ForgeRepo) -> Box<dyn jiji_forge::ShipForge + 'a> {
        Box::new(self.clone())
    }
}

impl jiji_forge::ShipForge for Fixture {
    fn update_pr_base(&self, number: u64, base: &str) -> Result<(), ForgeError> {
        SubmitForge::update_pr_base(self, number, base)
    }

    fn unstack(&self, stack: u64) -> Result<Vec<u64>, ForgeError> {
        SubmitForge::unstack(self, stack)
    }

    fn create_stack(&self, pull_requests: &[u64]) -> Result<u64, ForgeError> {
        SubmitForge::create_stack(self, pull_requests)
    }

    /// Asked after a ship has pushed trunk. The double never sees a push,
    /// so it applies GitHub's rule here: the pull requests asked about
    /// hold what was shipped, and one based on trunk is merged by that.
    fn await_merged(&self, numbers: &[u64]) -> Result<Vec<(u64, PrState)>, ForgeError> {
        let mut github = self.github();
        let mut states = Vec::new();
        for number in numbers {
            let pull = github
                .prs
                .iter_mut()
                .find(|pull| pull.summary.number == *number)
                .ok_or_else(|| {
                    ForgeError::NotFound(format!("pull request #{number} was not found"))
                })?;
            if pull.summary.state == PrState::Open && pull.summary.base_branch == "main" {
                let head = pull.summary.head_commit.clone();
                pull.merged_as(&head);
            }
            states.push((*number, pull.summary.state));
        }
        Ok(states)
    }

    /// The double keeps pull requests, not branches: where trunk is on
    /// GitHub cannot be seen through it.
    fn branch_head(&self, branch: &str) -> Result<String, ForgeError> {
        Err(ForgeError::NotFound(format!(
            "the double does not model the branch \u{201c}{branch}\u{201d}"
        )))
    }
}

impl StackCommentSource for Fixture {
    fn stack_comment(&self, number: u64) -> Result<Option<ExistingComment>, ForgeError> {
        self.with_pull(number, |pull| pull.comment.clone())
    }
}

impl SubmitForge for Fixture {
    fn create_pr(
        &self,
        title: &str,
        body: &str,
        head: &str,
        base: &str,
        draft: bool,
    ) -> Result<PrSummary, ForgeError> {
        let mut github = self.github();
        let number = github
            .prs
            .iter()
            .map(|pull| pull.summary.number)
            .max()
            .unwrap_or(40)
            + 1;
        let mut pull = open_pr(number, head, base, "", title);
        pull.summary.body = Some(body.into());
        pull.summary.is_draft = draft;
        pull.summary.review = ReviewDecision::ReviewRequired;
        pull.summary.checks = ChecksRollup::Pending;
        let summary = pull.summary.clone();
        github.prs.push(pull);
        github
            .asked
            .push(format!("create #{number} {head} → {base}"));
        Ok(summary)
    }

    fn update_pr_base(&self, number: u64, base: &str) -> Result<(), ForgeError> {
        self.with_pull(number, |pull| {
            if pull.stack.is_some() {
                return Err(refused(
                    "HTTP 422: Cannot change the base branch because the pull request is \
                     part of a stack.",
                ));
            }
            pull.summary.base_branch = base.into();
            Ok(())
        })??;
        self.ask(format!("retarget #{number} → {base}"));
        Ok(())
    }

    fn update_pr_text(
        &self,
        number: u64,
        title: Option<&str>,
        body: &str,
    ) -> Result<(), ForgeError> {
        self.with_pull(number, |pull| {
            if let Some(title) = title {
                pull.summary.title = title.into();
            }
            pull.summary.body = Some(body.into());
        })?;
        self.ask(format!("describe #{number}"));
        Ok(())
    }

    fn create_comment(&self, number: u64, body: &str) -> Result<(), ForgeError> {
        self.with_pull(number, |pull| {
            pull.comment = Some(ExistingComment {
                id: number * 100,
                body: body.into(),
            })
        })?;
        self.ask(format!("comment #{number}"));
        Ok(())
    }

    fn update_comment(&self, comment_id: u64, body: &str) -> Result<(), ForgeError> {
        self.with_pull(comment_id / 100, |pull| {
            pull.comment = Some(ExistingComment {
                id: comment_id,
                body: body.into(),
            })
        })?;
        self.ask(format!("comment #{}", comment_id / 100));
        Ok(())
    }

    fn create_stack(&self, pull_requests: &[u64]) -> Result<u64, ForgeError> {
        let mut github = self.github();
        if pull_requests.len() < 2 {
            return Err(refused(
                "HTTP 422: Invalid property /pull_requests: 2 items required",
            ));
        }
        let members: Vec<&Pull> = pull_requests
            .iter()
            .filter_map(|number| {
                github
                    .prs
                    .iter()
                    .find(|pull| pull.summary.number == *number)
            })
            .collect();
        if members.len() != pull_requests.len()
            || members
                .iter()
                .any(|pull| pull.summary.state != PrState::Open)
            || members
                .windows(2)
                .any(|pair| pair[1].summary.base_branch != pair[0].summary.head_branch)
        {
            return Err(refused(
                "HTTP 422: Pull requests must form a stack, where each PR's base ref is the \
                 previous PR's head ref",
            ));
        }
        if members.iter().any(|pull| pull.stack.is_some()) {
            return Err(refused(
                "HTTP 422: Pull requests are already part of a stack",
            ));
        }
        let stack = github.next_stack;
        github.next_stack += 1;
        for (index, number) in pull_requests.iter().enumerate() {
            if let Some(pull) = github
                .prs
                .iter_mut()
                .find(|pull| pull.summary.number == *number)
            {
                pull.stack = Some((stack, index as u32 + 1));
            }
        }
        github
            .asked
            .push(format!("stack {stack} = {pull_requests:?}"));
        Ok(stack)
    }

    fn add_to_stack(&self, stack: u64, pull_requests: &[u64]) -> Result<(), ForgeError> {
        let mut github = self.github();
        let top = github
            .prs
            .iter()
            .filter(|pull| pull.stack.is_some_and(|(number, _)| number == stack))
            .max_by_key(|pull| pull.stack.map(|(_, position)| position))
            .map(|pull| {
                (
                    pull.summary.head_branch.clone(),
                    pull.stack.map_or(0, |s| s.1),
                )
            })
            .ok_or_else(|| ForgeError::NotFound("Not Found".into()))?;
        let (mut below, mut position) = top;
        for number in pull_requests {
            let fits = github.prs.iter().any(|pull| {
                pull.summary.number == *number
                    && pull.stack.is_none()
                    && pull.summary.state == PrState::Open
                    && pull.summary.base_branch == below
            });
            if !fits {
                return Err(refused(
                    "HTTP 422: the pull requests can't be appended to the stack",
                ));
            }
            position += 1;
            let pull = github
                .prs
                .iter_mut()
                .find(|pull| pull.summary.number == *number)
                .expect("checked just above");
            pull.stack = Some((stack, position));
            below = pull.summary.head_branch.clone();
        }
        github
            .asked
            .push(format!("stack {stack} += {pull_requests:?}"));
        Ok(())
    }

    fn unstack(&self, stack: u64) -> Result<Vec<u64>, ForgeError> {
        let mut github = self.github();
        // Merged pull requests keep their seats; the rest go back to
        // being a plain chain, with the bases they had.
        for pull in github.prs.iter_mut() {
            if pull.stack.is_some_and(|(number, _)| number == stack)
                && pull.summary.state == PrState::Open
            {
                pull.stack = None;
            }
        }
        github.asked.push(format!("unstack {stack}"));
        // None is ever kept: the double has no merge queue to hold one.
        Ok(Vec::new())
    }
}

impl LandForge for Fixture {
    fn find_merged_pr(&self, branch: &str) -> Result<Option<PrSummary>, ForgeError> {
        let github = self.github();
        Ok(github
            .prs
            .iter()
            .find(|pull| {
                pull.summary.head_branch == branch && pull.summary.state == PrState::Merged
            })
            .map(|pull| github.summary(pull)))
    }

    fn pr_land_state(&self, number: u64, _base: &str) -> Result<PrLandState, ForgeError> {
        let github = self.github();
        let pull = github
            .prs
            .iter()
            .find(|pull| pull.summary.number == number)
            .ok_or_else(|| ForgeError::NotFound(format!("pull request #{number} was not found")))?;
        Ok(pull.land_state(&github))
    }

    fn open_prs_based_on(&self, base: &str) -> Result<Vec<PrSummary>, ForgeError> {
        let github = self.github();
        Ok(github
            .prs
            .iter()
            .filter(|pull| pull.summary.state == PrState::Open && pull.summary.base_branch == base)
            .map(|pull| github.summary(pull))
            .collect())
    }

    fn merge_pr(
        &self,
        number: u64,
        method: MergeMethod,
        _expected_head: &str,
        _commit_title: Option<&str>,
        _commit_message: Option<&str>,
    ) -> Result<(), ForgeError> {
        self.with_pull(number, |pull| {
            if pull.stack.is_some() {
                return Err(refused(
                    "Merging stacked PRs via this endpoint is not supported. Use the \
                     asynchronous merge endpoint instead.",
                ));
            }
            // A new commit on trunk, which the repository has not fetched.
            pull.merged_as(&format!("merge{number:0>36}"));
            Ok(())
        })??;
        self.ask(format!("merge #{number} by {}", method.label()));
        Ok(())
    }

    fn enable_auto_merge(
        &self,
        node_id: &str,
        method: MergeMethod,
        _expected_head: &str,
        _commit_title: Option<&str>,
        _commit_message: Option<&str>,
    ) -> Result<(), ForgeError> {
        let number = number_of(node_id).expect("the fixture's own node id");
        self.with_pull(number, |pull| {
            if pull.stack.is_some() {
                return Err(refused(
                    "Auto-merge is not supported for stacked pull requests.",
                ));
            }
            pull.auto_merge = true;
            Ok(())
        })??;
        self.ask(format!("auto-merge #{number} by {}", method.label()));
        Ok(())
    }

    fn enqueue_pr(&self, node_id: &str, _expected_head: &str) -> Result<(), ForgeError> {
        let number = number_of(node_id).expect("the fixture's own node id");
        self.with_pull(number, |pull| {
            if pull.stack.is_some() {
                return Err(refused(
                    "This pull request is part of a stack and must be merged using the \
                     asynchronous merge REST API.",
                ));
            }
            pull.queued = true;
            Ok(())
        })??;
        self.ask(format!("enqueue #{number}"));
        Ok(())
    }

    fn update_pr_base(&self, number: u64, base: &str) -> Result<(), ForgeError> {
        SubmitForge::update_pr_base(self, number, base)
    }

    fn mark_ready(&self, node_id: &str) -> Result<(), ForgeError> {
        let number = number_of(node_id).expect("the fixture's own node id");
        self.with_pull(number, |pull| pull.summary.is_draft = false)?;
        self.ask(format!("ready #{number}"));
        Ok(())
    }

    fn merge_stacked(
        &self,
        number: u64,
        method: MergeMethod,
        _expected_head: &str,
        _commit_title: Option<&str>,
        _commit_message: Option<&str>,
        _bypass: bool,
    ) -> Result<AsyncMerge, ForgeError> {
        let mut github = self.github();
        let (stack, position) = github
            .prs
            .iter()
            .find(|pull| pull.summary.number == number)
            .and_then(|pull| pull.stack)
            .ok_or_else(|| refused("the pull request is not in a stack"))?;
        // The one asked for and every open one below it, all or none.
        let taken = |pull: &Pull| {
            pull.stack.is_some_and(|(n, p)| n == stack && p <= position)
                && pull.summary.state == PrState::Open
        };
        if github
            .prs
            .iter()
            .any(|pull| taken(pull) && pull.summary.is_draft)
        {
            return Err(refused(
                "GitHub did not merge: Pull request must be open and not in draft mode \
                 in order to be merged.",
            ));
        }
        let mut merged: Vec<u64> = Vec::new();
        for pull in github.prs.iter_mut() {
            if taken(pull) {
                let own = pull.summary.number;
                pull.merged_as(&format!("merge{own:0>36}"));
                merged.push(own);
            }
        }
        // GitHub points the next one up at the stack's base itself.
        if let Some(next) = github
            .prs
            .iter_mut()
            .filter(|pull| pull.stack.is_some_and(|(n, p)| n == stack && p > position))
            .min_by_key(|pull| pull.stack.map(|(_, p)| p))
        {
            next.summary.base_branch = "main".into();
        }
        github.asked.push(format!(
            "merge {} together by {} (stack {stack})",
            merged
                .iter()
                .map(|n| format!("#{n}"))
                .collect::<Vec<_>>()
                .join(", "),
            method.label()
        ));
        Ok(AsyncMerge::Merged {
            commit: format!("merge{number:0>36}"),
        })
    }

    fn enqueue_stacked(&self, number: u64, _expected_head: &str) -> Result<AsyncMerge, ForgeError> {
        self.with_pull(number, |pull| pull.queued = true)?;
        self.ask(format!("enqueue #{number} with its stack"));
        Ok(AsyncMerge::Enqueued)
    }

    fn await_restack(&self, _number: u64, _from_head: &str) -> Result<Restack, ForgeError> {
        // See the module note: the double's heads never move.
        Ok(Restack::Rebased)
    }
}
