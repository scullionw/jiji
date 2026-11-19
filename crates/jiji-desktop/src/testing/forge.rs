//! GitHub as a test captured it: the pull requests, what each one's merge
//! button says, and the repository's landing rules, answered through the
//! host's own forge seam (`host::forge::Forge`). Publish, review, land,
//! ship and auto-land run their real planning and execution over it with
//! no token and no network, and what they asked of GitHub is kept for the
//! test to read.

use std::sync::{Arc, Mutex, MutexGuard};

use jiji_forge::{
    ChecksRollup, CiRerunReport, ExistingComment, ForgeError, ForgeRepo, LandForge, MergeMethod,
    MergeStateStatus, PrLandState, PrState, PrStateReport, PrSummary, RepoPrState, ResolvedToken,
    ReviewDecision, StackCommentSource, SubmitForge, TokenSource,
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
}

impl Pull {
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
        },
        conflicting: false,
        merge_state: None,
        auto_merge: false,
        queued: false,
        failed_runs: Vec::new(),
        comment: None,
    }
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
        })))
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
                .map(|pull| pull.summary.clone())
                .collect(),
            truncated: github.truncated,
        };
        Ok(RepoPrState::new(report, &repo.owner))
    }

    fn pr(&self, _: &ForgeRepo, number: u64) -> Result<PrSummary, ForgeError> {
        self.with_pull(number, |pull| pull.summary.clone())
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
        self.with_pull(number, |pull| pull.summary.base_branch = base.into())?;
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
}

impl LandForge for Fixture {
    fn find_merged_pr(&self, branch: &str) -> Result<Option<PrSummary>, ForgeError> {
        Ok(self
            .github()
            .prs
            .iter()
            .find(|pull| {
                pull.summary.head_branch == branch && pull.summary.state == PrState::Merged
            })
            .map(|pull| pull.summary.clone()))
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

    fn merge_pr(
        &self,
        number: u64,
        method: MergeMethod,
        _expected_head: &str,
        _commit_title: Option<&str>,
        _commit_message: Option<&str>,
    ) -> Result<(), ForgeError> {
        self.with_pull(number, |pull| pull.summary.state = PrState::Merged)?;
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
        self.with_pull(number, |pull| pull.auto_merge = true)?;
        self.ask(format!("auto-merge #{number} by {}", method.label()));
        Ok(())
    }

    fn enqueue_pr(&self, node_id: &str, _expected_head: &str) -> Result<(), ForgeError> {
        let number = number_of(node_id).expect("the fixture's own node id");
        self.with_pull(number, |pull| pull.queued = true)?;
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
}
