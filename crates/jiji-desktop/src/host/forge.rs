//! Forge-facing host calls: the GitHub connection behind the Publish
//! section. Thin over `jiji-forge`, like the repo calls stay thin over
//! `jiji-core`. Every call goes through one [`Forge`], which blocks on the
//! network, the keychain, or a `gh` subprocess in the app; an offline run
//! has one that reaches nothing, and tests put GitHub as they captured it
//! there (`testing::forge`).

use std::sync::{Arc, Mutex};

use jiji_core::snapshot::{LandedCheck, RepoSnapshot, TipCheck};
use jiji_core::BackendError;
use jiji_forge::{
    detect_github_repo, execute_land, execute_ship, execute_submit, execute_take_over,
    no_github_remote, plan_give_back, plan_land, plan_ship, plan_submit, plan_take_over,
    pr_template_candidates, resolve_token, AutoLandPrs, CiRerunReport, ForgeAuth, ForgeError,
    ForgeRepo, ForgeStatus, GitHubClient, KeychainTokenStore, LandForge, LandOptions, LandOutcome,
    LandPlan, LandRepoForge, LandVcs, PrSummary, PrTemplate, PublishStack, RepoForge, RepoPrState,
    ResolvedToken, ShipOutcome, ShipPlan, ShipVcs, SubmitForge, SubmitHead, SubmitOptions,
    SubmitOutcome, SubmitPlan, SubmitVcs, TakeOverOutcome, TakeOverPlan, TakeOverTarget,
    TakeOverVcs, TokenSource, TokenStore as _,
};

use super::{CommandError, Host, HostResult};

/// Where the host's forge calls go, and the token they go with.
pub trait Forge: Send + Sync {
    /// The token to act with on `repo`'s host, and where it came from;
    /// `None` when there is none anywhere.
    fn token(&self, repo: Option<&ForgeRepo>) -> Result<Option<ResolvedToken>, ForgeError>;
    /// Keep a token the forge just accepted as Jiji's own.
    fn store_token(&self, repo: Option<&ForgeRepo>, token: &str) -> Result<(), ForgeError>;
    /// Forget Jiji's own token; one from elsewhere stays where it is.
    fn delete_token(&self, repo: Option<&ForgeRepo>) -> Result<(), ForgeError>;
    /// A client acting with `token` on `repo`'s host, github.com's without
    /// one.
    fn client(
        &self,
        repo: Option<&ForgeRepo>,
        token: &str,
    ) -> Result<Box<dyn ForgeClient>, ForgeError>;
    /// Nothing may be reached from this run, so nothing fetches on its own
    /// either: `jj git fetch` would use the user's Git credentials.
    fn offline(&self) -> bool {
        false
    }
}

/// What the host asks of the forge with a token in hand, and the engines'
/// own seams over the same connection.
pub trait ForgeClient: Send + Sync {
    /// The login the token authenticates as.
    fn viewer(&self) -> Result<String, ForgeError>;
    /// The repo's open pull requests, the one batched query badges and
    /// every plan hang on.
    fn open_prs(&self, repo: &ForgeRepo) -> Result<RepoPrState, ForgeError>;
    /// One pull request by number, open or not.
    fn pr(&self, repo: &ForgeRepo, number: u64) -> Result<PrSummary, ForgeError>;
    /// Re-run the failed CI on a pull request's head commit.
    fn rerun_failed_ci(&self, repo: &ForgeRepo, head: &str) -> Result<CiRerunReport, ForgeError>;
    fn submit<'a>(&'a self, repo: &'a ForgeRepo) -> Box<dyn SubmitForge + 'a>;
    fn land<'a>(&'a self, repo: &'a ForgeRepo) -> Box<dyn LandForge + 'a>;
    fn ship<'a>(&'a self, repo: &'a ForgeRepo) -> Box<dyn jiji_forge::ShipForge + 'a>;
}

/// GitHub, with the user's own token: the keychain's, the environment's,
/// or the `gh` CLI's. Every read of the user's credentials comes through
/// here.
pub struct GitHub;

/// Token storage keys off the forge host, so a GitHub Enterprise remote
/// keeps its token separate from github.com's.
fn token_store(repo: Option<&ForgeRepo>) -> KeychainTokenStore {
    let host = repo.map_or("github.com", |r| r.host.as_str());
    KeychainTokenStore::new(host)
}

impl Forge for GitHub {
    /// Under test there is never one. Tests must not read real
    /// credentials, and without a token nothing downstream talks to the
    /// forge, so a test that puts a repo on screen stays off the network.
    fn token(&self, repo: Option<&ForgeRepo>) -> Result<Option<ResolvedToken>, ForgeError> {
        if cfg!(test) {
            return Ok(None);
        }
        resolve_token(&token_store(repo))
    }

    fn store_token(&self, repo: Option<&ForgeRepo>, token: &str) -> Result<(), ForgeError> {
        token_store(repo).set(token)
    }

    fn delete_token(&self, repo: Option<&ForgeRepo>) -> Result<(), ForgeError> {
        token_store(repo).delete()
    }

    fn client(
        &self,
        repo: Option<&ForgeRepo>,
        token: &str,
    ) -> Result<Box<dyn ForgeClient>, ForgeError> {
        Ok(Box::new(match repo {
            Some(repo) => GitHubClient::for_repo(repo, token)?,
            None => GitHubClient::for_github_com(token)?,
        }))
    }
}

impl ForgeClient for GitHubClient {
    fn viewer(&self) -> Result<String, ForgeError> {
        GitHubClient::viewer(self)
    }

    fn open_prs(&self, repo: &ForgeRepo) -> Result<RepoPrState, ForgeError> {
        let report = GitHubClient::open_prs(self, &repo.owner, &repo.name)?;
        Ok(RepoPrState::new(report, &repo.owner))
    }

    fn pr(&self, repo: &ForgeRepo, number: u64) -> Result<PrSummary, ForgeError> {
        self.pr_by_number(&repo.owner, &repo.name, number)
    }

    fn rerun_failed_ci(&self, repo: &ForgeRepo, head: &str) -> Result<CiRerunReport, ForgeError> {
        jiji_forge::rerun_failed_ci(self, repo, head)
    }

    fn submit<'a>(&'a self, repo: &'a ForgeRepo) -> Box<dyn SubmitForge + 'a> {
        Box::new(RepoForge { client: self, repo })
    }

    fn land<'a>(&'a self, repo: &'a ForgeRepo) -> Box<dyn LandForge + 'a> {
        Box::new(LandRepoForge { client: self, repo })
    }

    fn ship<'a>(&'a self, repo: &'a ForgeRepo) -> Box<dyn jiji_forge::ShipForge + 'a> {
        Box::new(RepoForge { client: self, repo })
    }
}

/// A run with `JIJI_OFFLINE=1`: no token is looked up anywhere, and
/// nothing is asked of GitHub, so Publish reads as not connected.
pub struct Offline;

impl Forge for Offline {
    fn token(&self, _: Option<&ForgeRepo>) -> Result<Option<ResolvedToken>, ForgeError> {
        Ok(None)
    }

    fn store_token(&self, _: Option<&ForgeRepo>, _: &str) -> Result<(), ForgeError> {
        Err(offline())
    }

    fn delete_token(&self, _: Option<&ForgeRepo>) -> Result<(), ForgeError> {
        Ok(())
    }

    fn client(&self, _: Option<&ForgeRepo>, _: &str) -> Result<Box<dyn ForgeClient>, ForgeError> {
        Err(offline())
    }

    fn offline(&self) -> bool {
        true
    }
}

fn offline() -> ForgeError {
    ForgeError::Network("this run is offline (JIJI_OFFLINE=1)".into())
}

/// The open-PR half of a client, as the auto-land loop reads it every
/// round.
pub(crate) struct OpenPrs<'a> {
    pub(crate) client: &'a dyn ForgeClient,
    pub(crate) repo: &'a ForgeRepo,
}

impl AutoLandPrs for OpenPrs<'_> {
    fn open_prs(&self) -> Result<RepoPrState, ForgeError> {
        self.client.open_prs(self.repo)
    }
}

/// The forge this session talks to, and the login the active token last
/// verified as, so status answers without a network round trip. Keyed by
/// token source — switching sources re-verifies.
pub struct ForgeState {
    pub(crate) api: Box<dyn Forge>,
    verified: Mutex<Option<(TokenSource, String)>>,
}

impl ForgeState {
    pub fn new(api: Box<dyn Forge>) -> Self {
        Self {
            api,
            verified: Mutex::new(None),
        }
    }

    fn remember(&self, source: TokenSource, login: String) {
        *self.verified.lock().expect("forge state lock poisoned") = Some((source, login));
    }

    fn forget(&self) {
        *self.verified.lock().expect("forge state lock poisoned") = None;
    }

    fn login_for(&self, source: TokenSource) -> Option<String> {
        self.verified
            .lock()
            .expect("forge state lock poisoned")
            .as_ref()
            .filter(|(verified_source, _)| *verified_source == source)
            .map(|(_, login)| login.clone())
    }
}

fn plan_stale(what: &str, verb: &str) -> CommandError {
    CommandError::new(
        "plan_stale",
        format!(
            "{what} changed since this plan was made; review the updated plan and {verb} again"
        ),
    )
}

impl Host {
    /// The GitHub repo behind an open repo's remotes, when there is one.
    pub(crate) fn detected_repo(&self, repo_path: &str) -> Option<ForgeRepo> {
        let remotes = self.current_git_remotes(repo_path);
        detect_github_repo(remotes.iter().map(|r| (r.name.as_str(), r.url.as_str())))
    }

    fn assemble_status(&self, repo_path: &str) -> HostResult<ForgeStatus> {
        let repo = self.detected_repo(repo_path);
        let source = self.forge.api.token(repo.as_ref())?.map(|t| t.source);
        let login = source.and_then(|s| self.forge.login_for(s));
        Ok(ForgeStatus {
            repo,
            auth: ForgeAuth { source, login },
        })
    }

    /// Connection state without touching the network: the detected repo,
    /// where a token would come from, and the login when this session
    /// verified one.
    pub fn forge_status(&self, repo_path: &str) -> HostResult<ForgeStatus> {
        self.assemble_status(repo_path)
    }

    /// Verify the resolved token against the API and remember the login.
    /// With no token anywhere this is not an error — it answers the plain
    /// unauthenticated status so the UI can render the connect state.
    pub fn forge_verify(&self, repo_path: &str) -> HostResult<ForgeStatus> {
        let repo = self.detected_repo(repo_path);
        let Some(resolved) = self.forge.api.token(repo.as_ref())? else {
            self.forge.forget();
            return self.assemble_status(repo_path);
        };
        let verified = self
            .forge
            .api
            .client(repo.as_ref(), &resolved.token)
            .and_then(|client| client.viewer());
        match verified {
            Ok(login) => self.forge.remember(resolved.source, login),
            Err(err) => {
                self.forge.forget();
                return Err(err.into());
            }
        }
        self.assemble_status(repo_path)
    }

    /// The in-app login: validate the pasted token against the API, then
    /// store it in the system keychain. An invalid token is refused and
    /// nothing is stored.
    pub fn forge_login(&self, repo_path: &str, token: &str) -> HostResult<ForgeStatus> {
        let token = token.trim();
        if token.is_empty() {
            return Err(ForgeError::AuthFailed("no token was entered".into()).into());
        }
        let repo = self.detected_repo(repo_path);
        let login = self.forge.api.client(repo.as_ref(), token)?.viewer()?;
        self.forge.api.store_token(repo.as_ref(), token)?;
        self.forge.remember(TokenSource::Keychain, login);
        self.assemble_status(repo_path)
    }

    /// Remove Jiji's stored token. Tokens managed outside Jiji
    /// (environment, gh CLI) are untouched, so the answered status may
    /// still be authenticated.
    pub fn forge_logout(&self, repo_path: &str) -> HostResult<ForgeStatus> {
        let repo = self.detected_repo(repo_path);
        self.forge.api.delete_token(repo.as_ref())?;
        self.forge.forget();
        self.assemble_status(repo_path)
    }

    /// The connected repo and a client acting with the resolved token.
    pub(crate) fn forge_client(
        &self,
        repo_path: &str,
    ) -> HostResult<(ForgeRepo, Box<dyn ForgeClient>, TokenSource)> {
        let repo = self.detected_repo(repo_path).ok_or_else(no_github_remote)?;
        let resolved = self
            .forge
            .api
            .token(Some(&repo))?
            .ok_or(ForgeError::NoToken)?;
        let client = self.forge.api.client(Some(&repo), &resolved.token)?;
        Ok((repo, client, resolved.source))
    }

    /// The detected repo's open-PR state: the one batched query workbench
    /// badges and publish flows hang on, answered with the
    /// bookmark-attachment map already built.
    pub fn forge_prs(&self, repo_path: &str) -> HostResult<RepoPrState> {
        let (repo, client, source) = self.forge_client(repo_path)?;
        let prs = client.open_prs(&repo)?;
        // A successful authenticated call is as good as a verification;
        // keep the session login fresh for status renders.
        if self.forge.login_for(source).is_none() {
            if let Ok(login) = client.viewer() {
                self.forge.remember(source, login);
            }
        }
        Ok(prs)
    }

    /// The connected repo, its client, and fresh open-PR state — what the
    /// submit and land calls start from.
    fn submit_context(
        &self,
        repo_path: &str,
    ) -> HostResult<(ForgeRepo, Box<dyn ForgeClient>, RepoPrState)> {
        let (repo, client, _) = self.forge_client(repo_path)?;
        let prs = client.open_prs(&repo)?;
        Ok((repo, client, prs))
    }

    /// The repo's PR template from the trunk tree, when one exists.
    fn pr_template(&self, repo_path: &str) -> Option<PrTemplate> {
        self.trunk_text_file(repo_path, &pr_template_candidates())
            .map(|(path, text)| PrTemplate { path, text })
    }

    /// Plan submitting the stack under a head. Read-only — GitHub is asked
    /// for fresh open-PR state and existing stack comments, nothing else
    /// runs.
    pub fn submit_plan(
        &self,
        repo_path: &str,
        head: &SubmitHead,
        options: Option<SubmitOptions>,
    ) -> HostResult<SubmitPlan> {
        let snapshot = self.workflow_snapshot(repo_path)?;
        let (repo, client, prs) = self.submit_context(repo_path)?;
        let template = self.pr_template(repo_path);
        let forge = client.submit(&repo);
        plan_submit(
            &snapshot,
            &prs,
            &repo,
            head,
            &*forge,
            template.as_ref(),
            &options.unwrap_or_default(),
        )
        .map_err(Into::into)
    }

    /// Execute a confirmed submit plan. The plan is re-derived first,
    /// under the options the confirmed plan carries, and refused when it
    /// no longer matches what the user confirmed.
    pub fn submit_stack(
        &self,
        repo_path: &str,
        head: &SubmitHead,
        plan: &SubmitPlan,
    ) -> HostResult<SubmitOutcome> {
        let snapshot = self.workflow_snapshot(repo_path)?;
        let (repo, client, prs) = self.submit_context(repo_path)?;
        let template = self.pr_template(repo_path);
        let forge = client.submit(&repo);
        let fresh = plan_submit(
            &snapshot,
            &prs,
            &repo,
            head,
            &*forge,
            template.as_ref(),
            &plan.options,
        )?;
        if fresh.actions != plan.actions {
            return Err(plan_stale("The stack or its pull requests", "publish"));
        }
        let vcs = HostVcs {
            host: self,
            repo_path: repo_path.to_owned(),
        };
        execute_submit(&fresh, &vcs, &*forge).map_err(Into::into)
    }

    /// One PR by number — the review flow's lookup for PRs the batched
    /// open-PR state cannot see (past the 100 cap, or closed).
    pub fn forge_pr(&self, repo_path: &str, number: u64) -> HostResult<PrSummary> {
        let (repo, client, _) = self.forge_client(repo_path)?;
        client.pr(&repo, number).map_err(Into::into)
    }

    /// Plan taking over the pull request `number`. Read-only: GitHub is
    /// asked for that pull request as it is now, so that one that has
    /// closed, merged or come from a fork is told as that.
    pub fn take_over_plan(&self, repo_path: &str, number: u64) -> HostResult<TakeOverPlan> {
        let snapshot = self.workflow_snapshot(repo_path)?;
        let (repo, client, _) = self.forge_client(repo_path)?;
        let pr = client.pr(&repo, number)?;
        Ok(plan_take_over(&snapshot, &repo, &pr))
    }

    /// Plan giving back the pull request `number`, which was taken over.
    /// Read-only, and GitHub is not asked: `pr` is the pull request as the
    /// stack already shows it, and gives the plan its title.
    pub fn give_back_plan(
        &self,
        repo_path: &str,
        number: u64,
        pr: Option<&PrSummary>,
    ) -> HostResult<TakeOverPlan> {
        let snapshot = self.workflow_snapshot(repo_path)?;
        plan_give_back(
            &snapshot,
            pr.map(std::slice::from_ref).unwrap_or_default(),
            &TakeOverTarget::Number(number),
        )
        .map_err(Into::into)
    }

    /// Run a take-over or a give-back the user confirmed. The plan is
    /// made again first, and refused when it is no longer the one the
    /// user saw.
    pub fn take_over(&self, repo_path: &str, plan: &TakeOverPlan) -> HostResult<TakeOverOutcome> {
        let fresh = if plan.giving_back {
            self.give_back_plan(repo_path, plan.number, None)?
        } else {
            self.take_over_plan(repo_path, plan.number)?
        };
        if fresh.actions != plan.actions || !fresh.blockers.is_empty() {
            return Err(plan_stale(
                "The pull request or its branch",
                if plan.giving_back {
                    "give it back"
                } else {
                    "take it over"
                },
            ));
        }
        let vcs = HostVcs {
            host: self,
            repo_path: repo_path.to_owned(),
        };
        execute_take_over(&fresh, &vcs).map_err(Into::into)
    }

    /// Re-run the failed GitHub Actions runs on a PR's head. The PR is
    /// re-fetched first so the re-run targets the head GitHub currently
    /// has.
    pub fn rerun_failed_ci(&self, repo_path: &str, number: u64) -> HostResult<CiRerunReport> {
        let (repo, client, _) = self.forge_client(repo_path)?;
        let pr = client.pr(&repo, number)?;
        client
            .rerun_failed_ci(&repo, &pr.head_commit)
            .map_err(Into::into)
    }

    /// Complete local stacks for Publish, without changing the graph view.
    pub fn publish_stacks(
        &self,
        repo_path: &str,
        prs: Option<&RepoPrState>,
    ) -> HostResult<(Arc<RepoSnapshot>, Vec<PublishStack>)> {
        let snapshot = self.workflow_snapshot(repo_path)?;
        let stacks = jiji_forge::discover_stacks(&snapshot, prs);
        Ok((snapshot, stacks))
    }

    /// Plan landing the stack under a bookmark. Read-only.
    pub fn land_plan(
        &self,
        repo_path: &str,
        head_bookmark: &str,
        options: Option<LandOptions>,
    ) -> HostResult<LandPlan> {
        let snapshot = self.workflow_snapshot(repo_path)?;
        let (repo, client, prs) = self.submit_context(repo_path)?;
        let forge = client.land(&repo);
        // Planning asks the repository whether merges GitHub reports are
        // on trunk here; it reads, nothing more.
        let vcs = HostVcs {
            host: self,
            repo_path: repo_path.to_owned(),
        };
        plan_land(
            &snapshot,
            &prs,
            &repo,
            head_bookmark,
            &*forge,
            &vcs,
            &options.unwrap_or_default(),
        )
        .map_err(Into::into)
    }

    /// Execute a confirmed land plan, re-derived under the options the
    /// confirmed plan carries; the merge step re-checks GitHub once more
    /// just before merging.
    pub fn land_stack(
        &self,
        repo_path: &str,
        head_bookmark: &str,
        plan: &LandPlan,
    ) -> HostResult<LandOutcome> {
        let snapshot = self.workflow_snapshot(repo_path)?;
        let (repo, client, prs) = self.submit_context(repo_path)?;
        let forge = client.land(&repo);
        let vcs = HostVcs {
            host: self,
            repo_path: repo_path.to_owned(),
        };
        let fresh = plan_land(
            &snapshot,
            &prs,
            &repo,
            head_bookmark,
            &*forge,
            &vcs,
            &plan.options,
        )?;
        if fresh.actions != plan.actions {
            return Err(plan_stale("The stack or its pull requests", "land"));
        }
        execute_land(&fresh, &vcs, &*forge).map_err(Into::into)
    }

    /// Plan shipping the stack under a change directly to trunk.
    /// Fetch-first: `jj git fetch` from the detected repo's remote runs
    /// before planning, so the plan previews against current remote trunk
    /// and the push's force-with-lease expectation is pinned freshly.
    pub fn ship_plan(&self, repo_path: &str, head_change: &str) -> HostResult<ShipPlan> {
        let repo = self.detected_repo(repo_path).ok_or_else(no_github_remote)?;
        let remotes = vec![repo.remote.clone()];
        self.mutate(repo_path, |backend, path| {
            backend.git_fetch(path, Some(&remotes))
        })?;
        let snapshot = self.workflow_snapshot(repo_path)?;
        let prs = self.ship_side(repo_path, &repo)?.map(|(_, prs)| prs);
        let mut plan = plan_ship(&snapshot, prs.as_ref(), &repo.remote, head_change)?;
        if prs.is_none() {
            plan.warnings
                .push(jiji_forge::SHIPPED_WITHOUT_GITHUB.into());
        }
        Ok(plan)
    }

    /// GitHub for a ship: the client and the open pull requests, or
    /// nothing when no token is connected. Shipping itself needs no
    /// GitHub API, so without a token the plan is made without pull
    /// requests and says so. A token that is there and a read that fails
    /// is another matter: a ship that could not see the pull requests
    /// would leave them open without a word, so the failure is told.
    fn ship_side(
        &self,
        repo_path: &str,
        repo: &ForgeRepo,
    ) -> HostResult<Option<(Box<dyn ForgeClient>, RepoPrState)>> {
        let client = match self.forge_client(repo_path) {
            Ok((_, client, _)) => client,
            Err(err) if err.code == "no_token" => return Ok(None),
            Err(err) => return Err(err),
        };
        let prs = client.open_prs(repo)?;
        Ok(Some((client, prs)))
    }

    /// Execute a confirmed ship plan. Deliberately no second fetch: the
    /// plan's fetch set the lease, and a remote moved since is the push
    /// step's honest refusal. The plan is derived again, with the pull
    /// requests as GitHub has them now (the plan finishes them), and
    /// refused when it is no longer what the user confirmed.
    pub fn ship_stack(
        &self,
        repo_path: &str,
        head_change: &str,
        plan: &ShipPlan,
    ) -> HostResult<ShipOutcome> {
        let snapshot = self.workflow_snapshot(repo_path)?;
        let repo = self.detected_repo(repo_path).ok_or_else(no_github_remote)?;
        // As in `ship_plan`: no token plans without pull requests, and
        // such a plan asks GitHub nothing; a read that fails is told.
        let side = self.ship_side(repo_path, &repo)?;
        let prs = side.as_ref().map(|(_, prs)| prs);
        let fresh = plan_ship(&snapshot, prs, &repo.remote, head_change)?;
        if fresh.actions != plan.actions {
            return Err(plan_stale("The stack or its pull requests", "ship"));
        }
        let vcs = HostVcs {
            host: self,
            repo_path: repo_path.to_owned(),
        };
        let forge = side.as_ref().map(|(client, _)| client.ship(&repo));
        execute_ship(&fresh, &vcs, forge.as_deref()).map_err(Into::into)
    }
}

/// The executors' jj half: every mutation runs through `Host::mutate`, so
/// the snapshot republishes to every surface after each step. The repo path
/// is pinned at construction, so a flow in progress keeps mutating its own
/// repo while other tabs own the screen.
pub(crate) struct HostVcs<'a> {
    pub(crate) host: &'a Host,
    pub(crate) repo_path: String,
}

impl HostVcs<'_> {
    fn run(
        &self,
        action: impl FnOnce(
            &dyn jiji_core::RepoBackend,
            &std::path::Path,
        ) -> Result<jiji_core::snapshot::MutationOutcome, BackendError>,
    ) -> Result<String, BackendError> {
        self.host
            .mutate(&self.repo_path, action)
            .map(|outcome| outcome.summary)
            .map_err(|err| BackendError::MutationFailed(err.message))
    }
}

impl SubmitVcs for HostVcs<'_> {
    fn push_bookmarks(&self, bookmarks: &[String], remote: &str) -> Result<String, BackendError> {
        self.host
            .push_and_publish(&self.repo_path, bookmarks, remote)
            .map(|outcome| outcome.summary)
            .map_err(|err| BackendError::MutationFailed(err.message))
    }

    fn create_bookmark(&self, name: &str, change: &str) -> Result<String, BackendError> {
        self.run(|backend, path| backend.create_bookmark(path, name, change))
    }
}

impl TakeOverVcs for HostVcs<'_> {
    fn git_fetch(&self, remote: &str) -> Result<String, BackendError> {
        LandVcs::git_fetch(self, remote)
    }

    fn take_over(
        &self,
        bookmark: &str,
        remote: &str,
        number: u64,
        author: Option<&str>,
    ) -> Result<String, BackendError> {
        self.run(|backend, path| backend.take_over_branch(path, bookmark, remote, number, author))
    }

    fn give_back(&self, bookmark: &str, remote: &str) -> Result<String, BackendError> {
        self.run(|backend, path| backend.give_back_branch(path, bookmark, remote))
    }
}

impl LandVcs for HostVcs<'_> {
    fn git_fetch(&self, remote: &str) -> Result<String, BackendError> {
        let remotes = vec![remote.to_owned()];
        self.run(|backend, path| backend.git_fetch(path, Some(&remotes)))
    }

    fn snapshot(&self) -> Result<RepoSnapshot, BackendError> {
        self.host
            .workflow_snapshot(&self.repo_path)
            .map(|snapshot| (*snapshot).clone())
    }

    fn rebase_onto_trunk(
        &self,
        remote: &str,
        trunk: &str,
        root_change: &str,
        landed: &[String],
    ) -> Result<String, BackendError> {
        // Onto the plan's trunk branch as the remote has it after the
        // fetch, the commit the landing was checked against. Not onto
        // `trunk()` or the local trunk bookmark: a fetch can change what
        // the first names, and both can be ahead with commits nobody
        // pushed.
        self.run(|backend, path| {
            backend.rebase_onto_trunk(path, remote, trunk, root_change, landed)
        })
    }

    fn push_bookmarks(&self, bookmarks: &[String], remote: &str) -> Result<String, BackendError> {
        SubmitVcs::push_bookmarks(self, bookmarks, remote)
    }

    fn delete_bookmark_at(&self, name: &str, commit: &str) -> Result<String, BackendError> {
        self.run(|backend, path| backend.delete_bookmark_at(path, name, commit))
    }

    fn abandon_changes(&self, commits: &[String]) -> Result<String, BackendError> {
        // The sweep after a landing: refused while anything else sits on
        // the landed commits.
        self.run(|backend, path| backend.abandon_landed(path, commits))
    }

    fn resting_on(&self, commits: &[String]) -> Result<Vec<String>, BackendError> {
        self.host
            .backend()
            .branches_resting_on(std::path::Path::new(&self.repo_path), commits)
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
        self.host.backend().landed_check(
            std::path::Path::new(&self.repo_path),
            remote,
            trunk,
            landing,
            head,
            bottom,
            top,
        )
    }

    fn lands_as_tip(
        &self,
        remote: &str,
        trunk: &str,
        bottom: &str,
        first_top: &str,
        tip: &str,
    ) -> Result<TipCheck, BackendError> {
        self.host.backend().lands_as_tip(
            std::path::Path::new(&self.repo_path),
            remote,
            trunk,
            bottom,
            first_top,
            tip,
        )
    }
}

impl ShipVcs for HostVcs<'_> {
    fn snapshot(&self) -> Result<RepoSnapshot, BackendError> {
        LandVcs::snapshot(self)
    }

    fn rebase_onto_trunk(&self, root_change: &str) -> Result<String, BackendError> {
        // Ship moves the local trunk bookmark onto the stack and pushes
        // it, so the stack goes onto that bookmark, unpushed commits and
        // all: they ship with it.
        let trunk_target = LandVcs::snapshot(self)?
            .bookmarks
            .iter()
            .find(|b| b.is_trunk)
            .map(|b| b.target.clone())
            .ok_or_else(|| {
                BackendError::MutationFailed(
                    "the repository has no trunk bookmark to rebase onto".into(),
                )
            })?;
        self.run(|backend, path| backend.rebase_change(path, root_change, &trunk_target))
    }

    fn move_bookmark(&self, name: &str, to_change: &str) -> Result<String, BackendError> {
        self.run(|backend, path| backend.move_bookmark(path, name, to_change))
    }

    fn push_bookmarks(&self, bookmarks: &[String], remote: &str) -> Result<String, BackendError> {
        SubmitVcs::push_bookmarks(self, bookmarks, remote)
    }

    fn new_change(&self, parent_change: &str) -> Result<String, BackendError> {
        self.run(|backend, path| backend.new_change(path, parent_change))
    }
}
