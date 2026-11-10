//! Forge-facing host calls: the GitHub connection behind the Publish
//! section. Thin over `jiji-forge`, like the repo calls stay thin over
//! `jiji-core`. The forge client is synchronous; every method blocks on
//! the network, the keychain, or a `gh` subprocess.

use std::sync::{Arc, Mutex};

use jiji_core::snapshot::RepoSnapshot;
use jiji_core::BackendError;
use jiji_forge::{
    detect_github_repo, execute_land, execute_ship, execute_submit, no_github_remote, plan_land,
    plan_ship, plan_submit, pr_template_candidates, resolve_token, CiRerunReport, ForgeAuth,
    ForgeError, ForgeRepo, ForgeStatus, GitHubClient, KeychainTokenStore, LandOptions, LandOutcome,
    LandPlan, LandRepoForge, LandVcs, PrSummary, PrTemplate, PublishStack, RepoForge, RepoPrState,
    ShipOutcome, ShipPlan, ShipVcs, SubmitHead, SubmitOutcome, SubmitPlan, SubmitVcs, TokenSource,
    TokenStore as _,
};

use super::{CommandError, Host, HostResult};

/// Session memory for the forge connection: the login the active token
/// last verified as, so status answers without a network round trip. Keyed
/// by token source — switching sources re-verifies.
pub struct ForgeState {
    verified: Mutex<Option<(TokenSource, String)>>,
}

impl ForgeState {
    pub fn new() -> Self {
        Self {
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

/// Token storage keys off the forge host, so a GitHub Enterprise remote
/// keeps its token separate from github.com's.
pub(crate) fn token_store(repo: Option<&ForgeRepo>) -> KeychainTokenStore {
    let host = repo.map_or("github.com", |r| r.host.as_str());
    KeychainTokenStore::new(host)
}

fn client_for(repo: Option<&ForgeRepo>, token: &str) -> Result<GitHubClient, ForgeError> {
    match repo {
        Some(repo) => GitHubClient::for_repo(repo, token),
        None => GitHubClient::for_github_com(token),
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
        let source = resolve_token(&token_store(repo.as_ref()))?.map(|t| t.source);
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
        let Some(resolved) = resolve_token(&token_store(repo.as_ref()))? else {
            self.forge.forget();
            return self.assemble_status(repo_path);
        };
        match client_for(repo.as_ref(), &resolved.token)?.viewer() {
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
        let login = client_for(repo.as_ref(), token)?.viewer()?;
        token_store(repo.as_ref()).set(token)?;
        self.forge.remember(TokenSource::Keychain, login);
        self.assemble_status(repo_path)
    }

    /// Remove Jiji's stored token. Tokens managed outside Jiji
    /// (environment, gh CLI) are untouched, so the answered status may
    /// still be authenticated.
    pub fn forge_logout(&self, repo_path: &str) -> HostResult<ForgeStatus> {
        let repo = self.detected_repo(repo_path);
        token_store(repo.as_ref()).delete()?;
        self.forge.forget();
        self.assemble_status(repo_path)
    }

    /// The connected repo and its client.
    fn forge_client(&self, repo_path: &str) -> HostResult<(ForgeRepo, GitHubClient, TokenSource)> {
        let repo = self.detected_repo(repo_path).ok_or_else(no_github_remote)?;
        let resolved = resolve_token(&token_store(Some(&repo)))?.ok_or(ForgeError::NoToken)?;
        let client = GitHubClient::for_repo(&repo, &resolved.token)?;
        Ok((repo, client, resolved.source))
    }

    /// The detected repo's open-PR state: the one batched query workbench
    /// badges and publish flows hang on, answered with the
    /// bookmark-attachment map already built.
    pub fn forge_prs(&self, repo_path: &str) -> HostResult<RepoPrState> {
        let (repo, client, source) = self.forge_client(repo_path)?;
        let report = client.open_prs(&repo.owner, &repo.name)?;
        // A successful authenticated call is as good as a verification;
        // keep the session login fresh for status renders.
        if self.forge.login_for(source).is_none() {
            if let Ok(login) = client.viewer() {
                self.forge.remember(source, login);
            }
        }
        Ok(RepoPrState::new(report, &repo.owner))
    }

    /// The connected repo, its client, and fresh open-PR state — what the
    /// submit and land calls start from.
    fn submit_context(
        &self,
        repo_path: &str,
    ) -> HostResult<(ForgeRepo, GitHubClient, RepoPrState)> {
        let (repo, client, _) = self.forge_client(repo_path)?;
        let prs = RepoPrState::new(client.open_prs(&repo.owner, &repo.name)?, &repo.owner);
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
    pub fn submit_plan(&self, repo_path: &str, head: &SubmitHead) -> HostResult<SubmitPlan> {
        let snapshot = self.workflow_snapshot(repo_path)?;
        let (repo, client, prs) = self.submit_context(repo_path)?;
        let forge_side = RepoForge {
            client: &client,
            repo: &repo,
        };
        let template = self.pr_template(repo_path);
        plan_submit(&snapshot, &prs, &repo, head, &forge_side, template.as_ref())
            .map_err(Into::into)
    }

    /// Execute a confirmed submit plan. The plan is re-derived first and
    /// refused when it no longer matches what the user confirmed.
    pub fn submit_stack(
        &self,
        repo_path: &str,
        head: &SubmitHead,
        plan: &SubmitPlan,
    ) -> HostResult<SubmitOutcome> {
        let snapshot = self.workflow_snapshot(repo_path)?;
        let (repo, client, prs) = self.submit_context(repo_path)?;
        let forge_side = RepoForge {
            client: &client,
            repo: &repo,
        };
        let template = self.pr_template(repo_path);
        let fresh = plan_submit(&snapshot, &prs, &repo, head, &forge_side, template.as_ref())?;
        if fresh.actions != plan.actions {
            return Err(plan_stale("The stack or its pull requests", "publish"));
        }
        let vcs = HostVcs {
            host: self,
            repo_path: repo_path.to_owned(),
        };
        execute_submit(&fresh, &vcs, &forge_side).map_err(Into::into)
    }

    /// One PR by number — the review flow's lookup for PRs the batched
    /// open-PR state cannot see (past the 100 cap, or closed).
    pub fn forge_pr(&self, repo_path: &str, number: u64) -> HostResult<PrSummary> {
        let (repo, client, _) = self.forge_client(repo_path)?;
        client
            .pr_by_number(&repo.owner, &repo.name, number)
            .map_err(Into::into)
    }

    /// Re-run the failed GitHub Actions runs on a PR's head. The PR is
    /// re-fetched first so the re-run targets the head GitHub currently
    /// has.
    pub fn rerun_failed_ci(&self, repo_path: &str, number: u64) -> HostResult<CiRerunReport> {
        let (repo, client, _) = self.forge_client(repo_path)?;
        let pr = client.pr_by_number(&repo.owner, &repo.name, number)?;
        jiji_forge::rerun_failed_ci(&client, &repo, &pr.head_commit).map_err(Into::into)
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
        let forge_side = LandRepoForge {
            client: &client,
            repo: &repo,
        };
        plan_land(
            &snapshot,
            &prs,
            &repo,
            head_bookmark,
            &forge_side,
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
        let forge_side = LandRepoForge {
            client: &client,
            repo: &repo,
        };
        let fresh = plan_land(
            &snapshot,
            &prs,
            &repo,
            head_bookmark,
            &forge_side,
            &plan.options,
        )?;
        if fresh.actions != plan.actions {
            return Err(plan_stale("The stack or its pull requests", "land"));
        }
        let vcs = HostVcs {
            host: self,
            repo_path: repo_path.to_owned(),
        };
        execute_land(&fresh, &vcs, &forge_side).map_err(Into::into)
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
        let prs = ship_pr_facts(&repo);
        plan_ship(&snapshot, prs.as_ref(), &repo.remote, head_change).map_err(Into::into)
    }

    /// Execute a confirmed ship plan. Deliberately no second fetch: the
    /// plan's fetch set the lease, and a remote moved since is the push
    /// step's honest refusal.
    pub fn ship_stack(
        &self,
        repo_path: &str,
        head_change: &str,
        plan: &ShipPlan,
    ) -> HostResult<ShipOutcome> {
        let snapshot = self.workflow_snapshot(repo_path)?;
        let repo = self.detected_repo(repo_path).ok_or_else(no_github_remote)?;
        // PR facts feed warnings only, never actions — skip the network call.
        let fresh = plan_ship(&snapshot, None, &repo.remote, head_change)?;
        if fresh.actions != plan.actions {
            return Err(plan_stale("The stack", "ship"));
        }
        let vcs = HostVcs {
            host: self,
            repo_path: repo_path.to_owned(),
        };
        execute_ship(&fresh, &vcs).map_err(Into::into)
    }
}

/// Open-PR facts for a ship plan's marks-it-merged warning, when a token
/// resolves. Shipping itself needs no GitHub API, so no token (or a failed
/// fetch of PR state) degrades to a plan without the warning.
fn ship_pr_facts(repo: &ForgeRepo) -> Option<RepoPrState> {
    let resolved = resolve_token(&token_store(Some(repo))).ok().flatten()?;
    let client = GitHubClient::for_repo(repo, &resolved.token).ok()?;
    let report = client.open_prs(&repo.owner, &repo.name).ok()?;
    Some(RepoPrState::new(report, &repo.owner))
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

    fn rebase_onto_trunk(&self, root_change: &str) -> Result<String, BackendError> {
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

    fn push_bookmarks(&self, bookmarks: &[String], remote: &str) -> Result<String, BackendError> {
        SubmitVcs::push_bookmarks(self, bookmarks, remote)
    }

    fn delete_bookmark(&self, name: &str) -> Result<String, BackendError> {
        self.run(|backend, path| backend.delete_bookmark(path, name))
    }

    fn abandon_changes(&self, change_ids: &[String]) -> Result<String, BackendError> {
        self.run(|backend, path| backend.abandon_changes(path, change_ids))
    }
}

impl ShipVcs for HostVcs<'_> {
    fn snapshot(&self) -> Result<RepoSnapshot, BackendError> {
        LandVcs::snapshot(self)
    }

    fn rebase_onto_trunk(&self, root_change: &str) -> Result<String, BackendError> {
        LandVcs::rebase_onto_trunk(self, root_change)
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
