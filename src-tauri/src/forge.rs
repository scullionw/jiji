//! Forge-facing command surface: the GitHub connection for the Publish
//! section.
//!
//! Commands stay thin over `jiji-forge`, like `commands.rs` stays thin over
//! `jiji-core`. The forge client is deliberately synchronous, and Tauri
//! runs command bodies on its async runtime — even `(async)`-marked sync
//! fns land on a runtime worker, not a blocking pool. A blocking reqwest
//! client on a runtime thread panics in debug builds (reqwest's misuse
//! detector), which kills the command task before it can answer and leaves
//! the invoke promise pending forever. So every command here is an async
//! fn whose whole body hops to the blocking pool via [`run_blocking`].

use std::sync::Mutex;

use jiji_forge::{
    detect_github_repo, execute_land, execute_ship, execute_submit, no_github_remote, plan_land,
    plan_ship, plan_submit, pr_template_candidates, resolve_token, CiRerunReport, ForgeAuth,
    ForgeError, ForgeRepo, ForgeStatus, GitHubClient, KeychainTokenStore, LandOptions,
    LandOutcome, LandPlan, LandRepoForge, LandVcs, PrSummary, PrTemplate, RepoForge, RepoPrState,
    ShipOutcome, ShipPlan,
    ShipVcs, SubmitHead, SubmitOutcome, SubmitPlan, SubmitVcs, TokenSource, TokenStore as _,
};
use tauri::{AppHandle, Manager as _};

use crate::commands::{AppState, CommandError};

impl From<ForgeError> for CommandError {
    fn from(err: ForgeError) -> Self {
        Self::new(err.code(), err.to_string())
    }
}

/// Run a command body on the runtime's blocking pool. The body owns the
/// `AppHandle` and pulls managed state out of it itself — `State` guards
/// borrow the handle and cannot cross into a `'static` closure.
pub(crate) async fn run_blocking<T, F>(task: F) -> Result<T, CommandError>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, CommandError> + Send + 'static,
{
    tauri::async_runtime::spawn_blocking(task)
        .await
        .map_err(|err| CommandError::new("internal", err.to_string()))?
}

/// Session memory for the forge connection: the login the active token
/// last verified as, so `forge_status` answers without a network round
/// trip. Keyed by token source — switching sources (connecting, logging
/// out into an env token) re-verifies.
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

/// The GitHub repo behind an open repo's remotes, when there is one.
pub(crate) fn detected_repo(state: &AppState, repo_path: &str) -> Option<ForgeRepo> {
    let remotes = state.current_git_remotes(repo_path);
    detect_github_repo(remotes.iter().map(|r| (r.name.as_str(), r.url.as_str())))
}

/// Token storage keys off the forge host, so a GitHub Enterprise remote
/// would keep its token separate from github.com's.
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

fn assemble_status(
    state: &AppState,
    forge: &ForgeState,
    repo_path: &str,
) -> Result<ForgeStatus, CommandError> {
    let repo = detected_repo(state, repo_path);
    let source = resolve_token(&token_store(repo.as_ref()))?.map(|t| t.source);
    let login = source.and_then(|s| forge.login_for(s));
    Ok(ForgeStatus {
        repo,
        auth: ForgeAuth { source, login },
    })
}

/// Connection state without touching the network: the detected repo, where
/// a token would come from, and the login when this session verified one.
/// Blocking-pooled to keep the keychain read and `gh` subprocess off the
/// UI thread.
#[tauri::command]
pub async fn forge_status(app: AppHandle, repo_path: String) -> Result<ForgeStatus, CommandError> {
    run_blocking(move || {
        let state = app.state::<AppState>();
        let forge = app.state::<ForgeState>();
        assemble_status(&state, &forge, &repo_path)
    })
    .await
}

/// Verify the resolved token against the API and remember the login. With
/// no token anywhere this is not an error — it answers the plain
/// unauthenticated status so the UI can render the connect state.
#[tauri::command]
pub async fn forge_verify(app: AppHandle, repo_path: String) -> Result<ForgeStatus, CommandError> {
    run_blocking(move || {
        let state = app.state::<AppState>();
        let forge = app.state::<ForgeState>();
        let repo = detected_repo(&state, &repo_path);
        let Some(resolved) = resolve_token(&token_store(repo.as_ref()))? else {
            forge.forget();
            return assemble_status(&state, &forge, &repo_path);
        };
        match client_for(repo.as_ref(), &resolved.token)?.viewer() {
            Ok(login) => forge.remember(resolved.source, login),
            Err(err) => {
                forge.forget();
                return Err(err.into());
            }
        }
        assemble_status(&state, &forge, &repo_path)
    })
    .await
}

/// The in-app login: validate the pasted token against the API, then store
/// it in the system keychain. An invalid token is refused and nothing is
/// stored.
#[tauri::command]
pub async fn forge_login(
    app: AppHandle,
    repo_path: String,
    token: String,
) -> Result<ForgeStatus, CommandError> {
    run_blocking(move || {
        let state = app.state::<AppState>();
        let forge = app.state::<ForgeState>();
        let token = token.trim();
        if token.is_empty() {
            return Err(ForgeError::AuthFailed("no token was entered".into()).into());
        }
        let repo = detected_repo(&state, &repo_path);
        let login = client_for(repo.as_ref(), token)?.viewer()?;
        token_store(repo.as_ref()).set(token)?;
        forge.remember(TokenSource::Keychain, login);
        assemble_status(&state, &forge, &repo_path)
    })
    .await
}

/// Remove Jiji's stored token. Tokens managed outside Jiji (environment,
/// gh CLI) are untouched, so the answered status may still be
/// authenticated — the UI states where that token lives instead of
/// offering a disconnect that cannot work.
#[tauri::command]
pub async fn forge_logout(app: AppHandle, repo_path: String) -> Result<ForgeStatus, CommandError> {
    run_blocking(move || {
        let state = app.state::<AppState>();
        let forge = app.state::<ForgeState>();
        let repo = detected_repo(&state, &repo_path);
        token_store(repo.as_ref()).delete()?;
        forge.forget();
        assemble_status(&state, &forge, &repo_path)
    })
    .await
}

/// The detected repo's open-PR state: the one batched query workbench
/// badges and publish flows hang on, answered with the bookmark-attachment
/// map already built (head branch → PR, fork-filtered).
#[tauri::command]
pub async fn forge_prs(app: AppHandle, repo_path: String) -> Result<RepoPrState, CommandError> {
    run_blocking(move || {
        let state = app.state::<AppState>();
        let forge = app.state::<ForgeState>();
        let repo = detected_repo(&state, &repo_path).ok_or_else(no_github_remote)?;
        let resolved = resolve_token(&token_store(Some(&repo)))?.ok_or(ForgeError::NoToken)?;
        let client = GitHubClient::for_repo(&repo, &resolved.token)?;
        let report = client.open_prs(&repo.owner, &repo.name)?;
        // A successful authenticated call is as good as a verification; keep
        // the session login fresh for status renders.
        if forge.login_for(resolved.source).is_none() {
            if let Ok(login) = client.viewer() {
                forge.remember(resolved.source, login);
            }
        }
        Ok(RepoPrState::new(report, &repo.owner))
    })
    .await
}

/// The connected repo, its client, and fresh open-PR state — what both
/// submit commands start from.
fn submit_context(
    state: &AppState,
    repo_path: &str,
) -> Result<(ForgeRepo, GitHubClient, RepoPrState), CommandError> {
    let repo = detected_repo(state, repo_path).ok_or_else(no_github_remote)?;
    let resolved = resolve_token(&token_store(Some(&repo)))?.ok_or(ForgeError::NoToken)?;
    let client = GitHubClient::for_repo(&repo, &resolved.token)?;
    let prs = RepoPrState::new(client.open_prs(&repo.owner, &repo.name)?, &repo.owner);
    Ok((repo, client, prs))
}

/// The repo's PR template from the trunk tree, when one exists — folded
/// into the bodies of PRs a submit plan creates.
fn pr_template(state: &AppState, repo_path: &str) -> Option<PrTemplate> {
    state
        .trunk_text_file(repo_path, &pr_template_candidates())
        .map(|(path, text)| PrTemplate { path, text })
}

/// Plan submitting the stack under a head — a bookmark, or an
/// unbookmarked head change whose branch name the plan derives: what
/// would push, which PRs would open against which bases, which existing
/// PRs retarget, which PR text and stack comments refresh. Read-only —
/// GitHub is asked for fresh open-PR state and existing stack comments,
/// nothing else runs (a derived bookmark is only created on confirm).
#[tauri::command]
pub async fn submit_plan(
    app: AppHandle,
    repo_path: String,
    head: SubmitHead,
) -> Result<SubmitPlan, CommandError> {
    run_blocking(move || {
        let state = app.state::<AppState>();
        let snapshot = state.workflow_snapshot(&repo_path)?;
        let (repo, client, prs) = submit_context(&state, &repo_path)?;
        let forge_side = RepoForge {
            client: &client,
            repo: &repo,
        };
        let template = pr_template(&state, &repo_path);
        plan_submit(
            &snapshot,
            &prs,
            &repo,
            &head,
            &forge_side,
            template.as_ref(),
        )
        .map_err(Into::into)
    })
    .await
}

/// The submit executor's jj half: pushes run through the shared mutation
/// path so the snapshot republishes to every surface mid-flow. Also the
/// auto-land job's vcs — its thread builds one over the same state. The
/// repo path is pinned at construction, so a flow in progress keeps
/// mutating its own repo while other tabs own the screen.
pub(crate) struct HostVcs<'a> {
    pub(crate) app: &'a AppHandle,
    pub(crate) state: &'a AppState,
    pub(crate) repo_path: String,
}

impl SubmitVcs for HostVcs<'_> {
    fn push_bookmarks(
        &self,
        bookmarks: &[String],
        remote: &str,
    ) -> Result<String, jiji_core::BackendError> {
        self.state
            .push_and_publish(self.app, &self.repo_path, bookmarks, remote)
            .map(|outcome| outcome.summary)
            .map_err(|err| jiji_core::BackendError::MutationFailed(err.message))
    }

    fn create_bookmark(&self, name: &str, change: &str) -> Result<String, jiji_core::BackendError> {
        self.state
            .mutate(self.app, &self.repo_path, |backend, path| {
                backend.create_bookmark(path, name, change)
            })
            .map(|outcome| outcome.summary)
            .map_err(|err| jiji_core::BackendError::MutationFailed(err.message))
    }
}

/// The land executor's jj half: every mutation runs through the shared
/// `AppState::mutate` path, so the snapshot republishes to every surface
/// after each step and the cleanup steps read honestly-fresh state.
impl LandVcs for HostVcs<'_> {
    fn git_fetch(&self, remote: &str) -> Result<String, jiji_core::BackendError> {
        let remotes = vec![remote.to_owned()];
        self.state
            .mutate(self.app, &self.repo_path, |backend, path| {
                backend.git_fetch(path, Some(&remotes))
            })
            .map(|outcome| outcome.summary)
            .map_err(|err| jiji_core::BackendError::MutationFailed(err.message))
    }

    fn snapshot(&self) -> Result<jiji_core::snapshot::RepoSnapshot, jiji_core::BackendError> {
        self.state.workflow_snapshot(&self.repo_path)
    }

    fn rebase_onto_trunk(&self, root_change: &str) -> Result<String, jiji_core::BackendError> {
        let trunk_target = LandVcs::snapshot(self)?
            .bookmarks
            .iter()
            .find(|b| b.is_trunk)
            .map(|b| b.target.clone())
            .ok_or_else(|| {
                jiji_core::BackendError::MutationFailed(
                    "the repository has no trunk bookmark to rebase onto".into(),
                )
            })?;
        self.state
            .mutate(self.app, &self.repo_path, |backend, path| {
                backend.rebase_change(path, root_change, &trunk_target)
            })
            .map(|outcome| outcome.summary)
            .map_err(|err| jiji_core::BackendError::MutationFailed(err.message))
    }

    fn push_bookmarks(
        &self,
        bookmarks: &[String],
        remote: &str,
    ) -> Result<String, jiji_core::BackendError> {
        SubmitVcs::push_bookmarks(self, bookmarks, remote)
    }

    fn delete_bookmark(&self, name: &str) -> Result<String, jiji_core::BackendError> {
        self.state
            .mutate(self.app, &self.repo_path, |backend, path| {
                backend.delete_bookmark(path, name)
            })
            .map(|outcome| outcome.summary)
            .map_err(|err| jiji_core::BackendError::MutationFailed(err.message))
    }

    fn abandon_changes(&self, change_ids: &[String]) -> Result<String, jiji_core::BackendError> {
        self.state
            .mutate(self.app, &self.repo_path, |backend, path| {
                backend.abandon_changes(path, change_ids)
            })
            .map(|outcome| outcome.summary)
            .map_err(|err| jiji_core::BackendError::MutationFailed(err.message))
    }
}

/// The ship executor's jj half: the shared paths again, plus the two
/// mutations shipping adds (moving the trunk bookmark, respawning a
/// shipped working copy).
impl ShipVcs for HostVcs<'_> {
    fn snapshot(&self) -> Result<jiji_core::snapshot::RepoSnapshot, jiji_core::BackendError> {
        LandVcs::snapshot(self)
    }

    fn rebase_onto_trunk(&self, root_change: &str) -> Result<String, jiji_core::BackendError> {
        LandVcs::rebase_onto_trunk(self, root_change)
    }

    fn move_bookmark(&self, name: &str, to_change: &str) -> Result<String, jiji_core::BackendError> {
        self.state
            .mutate(self.app, &self.repo_path, |backend, path| {
                backend.move_bookmark(path, name, to_change)
            })
            .map(|outcome| outcome.summary)
            .map_err(|err| jiji_core::BackendError::MutationFailed(err.message))
    }

    fn push_bookmarks(
        &self,
        bookmarks: &[String],
        remote: &str,
    ) -> Result<String, jiji_core::BackendError> {
        SubmitVcs::push_bookmarks(self, bookmarks, remote)
    }

    fn new_change(&self, parent_change: &str) -> Result<String, jiji_core::BackendError> {
        self.state
            .mutate(self.app, &self.repo_path, |backend, path| {
                backend.new_change(path, parent_change)
            })
            .map(|outcome| outcome.summary)
            .map_err(|err| jiji_core::BackendError::MutationFailed(err.message))
    }
}

/// Execute a confirmed submit plan: one batched push, PR creations
/// bottom-up, then base retargets. The plan is re-derived against the
/// current snapshot and fresh PR state first, and refused when it no
/// longer matches what the user confirmed — the stack or GitHub moved
/// under the panel (the same never-clobber posture as split's hunk
/// verification).
#[tauri::command]
pub async fn submit_stack(
    app: AppHandle,
    repo_path: String,
    head: SubmitHead,
    plan: SubmitPlan,
) -> Result<SubmitOutcome, CommandError> {
    run_blocking(move || {
        let state = app.state::<AppState>();
        let snapshot = state.workflow_snapshot(&repo_path)?;
        let (repo, client, prs) = submit_context(&state, &repo_path)?;
        let forge_side = RepoForge {
            client: &client,
            repo: &repo,
        };
        let template = pr_template(&state, &repo_path);
        let fresh = plan_submit(
            &snapshot,
            &prs,
            &repo,
            &head,
            &forge_side,
            template.as_ref(),
        )?;
        if fresh.actions != plan.actions {
            return Err(CommandError::new(
                "plan_stale",
                "The stack or its pull requests changed since this plan was made; \
                 review the updated plan and publish again",
            ));
        }
        let vcs = HostVcs {
            app: &app,
            state: &state,
            repo_path: repo_path.clone(),
        };
        execute_submit(&fresh, &vcs, &forge_side).map_err(Into::into)
    })
    .await
}

/// One PR by number — the review flow's lookup for PRs the batched
/// open-PR state cannot see (past the 100 cap, or closed), so "fetch any
/// PR" really means any.
#[tauri::command]
pub async fn forge_pr(
    app: AppHandle,
    repo_path: String,
    number: u64,
) -> Result<PrSummary, CommandError> {
    run_blocking(move || {
        let state = app.state::<AppState>();
        let repo = detected_repo(&state, &repo_path).ok_or_else(no_github_remote)?;
        let resolved = resolve_token(&token_store(Some(&repo)))?.ok_or(ForgeError::NoToken)?;
        let client = GitHubClient::for_repo(&repo, &resolved.token)?;
        client
            .pr_by_number(&repo.owner, &repo.name, number)
            .map_err(Into::into)
    })
    .await
}

/// Re-run the failed GitHub Actions runs on a PR's head. The PR is
/// re-fetched first so the re-run targets the head GitHub currently has,
/// not the possibly-stale one a badge was drawn from; the answer says
/// which workflow runs went back to work (empty means the failing check
/// lives outside GitHub Actions' reach).
#[tauri::command]
pub async fn rerun_failed_ci(
    app: AppHandle,
    repo_path: String,
    number: u64,
) -> Result<CiRerunReport, CommandError> {
    run_blocking(move || {
        let state = app.state::<AppState>();
        let repo = detected_repo(&state, &repo_path).ok_or_else(no_github_remote)?;
        let resolved = resolve_token(&token_store(Some(&repo)))?.ok_or(ForgeError::NoToken)?;
        let client = GitHubClient::for_repo(&repo, &resolved.token)?;
        let pr = client.pr_by_number(&repo.owner, &repo.name, number)?;
        jiji_forge::rerun_failed_ci(&client, &repo, &pr.head_commit).map_err(Into::into)
    })
    .await
}

/// Complete local stacks for Publish, without changing the graph view.
/// `async` so deserializing the whole snapshot it is handed (on every
/// refresh while Publish is open) stays off the main thread.
#[tauri::command(async)]
pub fn publish_stacks(
    snapshot: jiji_core::RepoSnapshot,
    prs: Option<RepoPrState>,
) -> Vec<jiji_forge::PublishStack> {
    jiji_forge::discover_stacks(&snapshot, prs.as_ref())
}

/// Complete local stacks for Publish, without changing the graph view.
#[tauri::command]
pub async fn publish_snapshot(
    app: AppHandle,
    repo_path: String,
) -> Result<jiji_core::RepoSnapshot, CommandError> {
    run_blocking(move || {
        app.state::<AppState>()
            .workflow_snapshot(&repo_path)
            .map_err(Into::into)
    })
    .await
}

/// Plan landing the stack under a bookmark: what already merged on GitHub,
/// whether the bottom PR merges now (or hands off to auto-merge or a merge
/// queue), and the reconcile that follows — fetch, rebase, push, retarget,
/// bookmark and change cleanup. Read-only: GitHub is asked for fresh
/// open-PR state, the candidate's land state, and merged-PR recognition,
/// nothing else runs.
#[tauri::command]
pub async fn land_plan(
    app: AppHandle,
    repo_path: String,
    head_bookmark: String,
    options: Option<LandOptions>,
) -> Result<LandPlan, CommandError> {
    run_blocking(move || {
        let state = app.state::<AppState>();
        let snapshot = state.workflow_snapshot(&repo_path)?;
        let (repo, client, prs) = submit_context(&state, &repo_path)?;
        let forge_side = LandRepoForge {
            client: &client,
            repo: &repo,
        };
        plan_land(
            &snapshot,
            &prs,
            &repo,
            &head_bookmark,
            &forge_side,
            &options.unwrap_or_default(),
        )
        .map_err(Into::into)
    })
    .await
}

/// Execute a confirmed land plan. The plan is re-derived against the
/// current snapshot and fresh GitHub state first and refused when its
/// actions no longer match what the user confirmed (the stack, the PR, or
/// its checks moved under the panel — the same never-clobber posture as
/// submit); the merge step re-checks GitHub once more just before merging.
#[tauri::command]
pub async fn land_stack(
    app: AppHandle,
    repo_path: String,
    head_bookmark: String,
    plan: LandPlan,
) -> Result<LandOutcome, CommandError> {
    run_blocking(move || {
        let state = app.state::<AppState>();
        let snapshot = state.workflow_snapshot(&repo_path)?;
        let (repo, client, prs) = submit_context(&state, &repo_path)?;
        let forge_side = LandRepoForge {
            client: &client,
            repo: &repo,
        };
        // Re-derived under the options the confirmed plan carries, so the
        // user's method and bypass choices survive the re-check.
        let fresh = plan_land(&snapshot, &prs, &repo, &head_bookmark, &forge_side, &plan.options)?;
        if fresh.actions != plan.actions {
            return Err(CommandError::new(
                "plan_stale",
                "The stack or its pull requests changed since this plan was made; \
                 review the updated plan and land again",
            ));
        }
        let vcs = HostVcs {
            app: &app,
            state: &state,
            repo_path: repo_path.clone(),
        };
        execute_land(&fresh, &vcs, &forge_side).map_err(Into::into)
    })
    .await
}

/// Open-PR facts for a ship plan's marks-it-merged warning, when a token
/// resolves. Shipping itself needs no GitHub API — the push authenticates
/// through git — so no token (or a failed fetch of PR state) degrades to
/// a plan without the warning rather than a refusal.
fn ship_pr_facts(repo: &ForgeRepo) -> Option<RepoPrState> {
    let resolved = resolve_token(&token_store(Some(repo))).ok().flatten()?;
    let client = GitHubClient::for_repo(repo, &resolved.token).ok()?;
    let report = client.open_prs(&repo.owner, &repo.name).ok()?;
    Some(RepoPrState::new(report, &repo.owner))
}

/// Plan shipping the stack under a change directly to trunk. The spec's
/// fetch-first shape: `jj git fetch` from the detected repo's remote runs
/// before planning, so the plan previews against current remote trunk
/// (a fetch that changes nothing records nothing; a real import publishes
/// like any mutation) and the push's force-with-lease expectation is
/// pinned freshly — trunk moving *after* this fetch is exactly what the
/// push step refuses.
#[tauri::command]
pub async fn ship_plan(
    app: AppHandle,
    repo_path: String,
    head_change: String,
) -> Result<ShipPlan, CommandError> {
    run_blocking(move || {
        let state = app.state::<AppState>();
        let repo = detected_repo(&state, &repo_path).ok_or_else(no_github_remote)?;
        let remotes = vec![repo.remote.clone()];
        state.mutate(&app, &repo_path, |backend, path| {
            backend.git_fetch(path, Some(&remotes))
        })?;
        let snapshot = state.workflow_snapshot(&repo_path)?;
        let prs = ship_pr_facts(&repo);
        plan_ship(&snapshot, prs.as_ref(), &repo.remote, &head_change).map_err(Into::into)
    })
    .await
}

/// Execute a confirmed ship plan. The plan is re-derived against the
/// current snapshot first and refused when its actions no longer match
/// what the user confirmed (the stack moved under the panel — submit's
/// never-clobber posture). Deliberately no second fetch: the plan's fetch
/// set the lease, and re-fetching here could shift the very plan the user
/// just read — a remote moved since is the push step's honest refusal,
/// and re-running Ship is the retry.
#[tauri::command]
pub async fn ship_stack(
    app: AppHandle,
    repo_path: String,
    head_change: String,
    plan: ShipPlan,
) -> Result<ShipOutcome, CommandError> {
    run_blocking(move || {
        let state = app.state::<AppState>();
        let snapshot = state.workflow_snapshot(&repo_path)?;
        let repo = detected_repo(&state, &repo_path).ok_or_else(no_github_remote)?;
        // PR facts feed warnings only, never actions — skip the network call.
        let fresh = plan_ship(&snapshot, None, &repo.remote, &head_change)?;
        if fresh.actions != plan.actions {
            return Err(CommandError::new(
                "plan_stale",
                "The stack changed since this plan was made; review the updated plan and \
                 ship again",
            ));
        }
        let vcs = HostVcs {
            app: &app,
            state: &state,
            repo_path: repo_path.clone(),
        };
        execute_ship(&fresh, &vcs).map_err(Into::into)
    })
    .await
}
