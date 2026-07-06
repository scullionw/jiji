//! Forge-facing command surface: the GitHub connection for the Publish
//! section.
//!
//! Commands stay thin over `jiji-forge`, like `commands.rs` stays thin over
//! `jiji-core`. The forge client is deliberately synchronous, so every
//! network-touching command here is `#[tauri::command(async)]` on a sync fn
//! — Tauri runs those on its blocking pool (the `resolve_conflict`
//! pattern), keeping the UI thread free.

use std::sync::Mutex;

use jiji_forge::{
    detect_github_repo, execute_land, execute_submit, no_github_remote, plan_land, plan_submit,
    pr_template_candidates, resolve_token, CiRerunReport, ForgeAuth, ForgeError, ForgeRepo,
    ForgeStatus, GitHubClient, KeychainTokenStore, LandOutcome, LandPlan, LandRepoForge, LandVcs,
    PrSummary, PrTemplate, RepoForge, RepoPrState, SubmitOutcome, SubmitPlan, SubmitVcs,
    TokenSource, TokenStore as _,
};
use tauri::{AppHandle, State};

use crate::commands::{AppState, CommandError};

impl From<ForgeError> for CommandError {
    fn from(err: ForgeError) -> Self {
        Self::new(err.code(), err.to_string())
    }
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

/// The GitHub repo behind the open repo's remotes, when there is one.
fn detected_repo(state: &AppState) -> Option<ForgeRepo> {
    let remotes = state.current_git_remotes();
    detect_github_repo(remotes.iter().map(|r| (r.name.as_str(), r.url.as_str())))
}

/// Token storage keys off the forge host, so a GitHub Enterprise remote
/// would keep its token separate from github.com's.
fn token_store(repo: Option<&ForgeRepo>) -> KeychainTokenStore {
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
) -> Result<ForgeStatus, CommandError> {
    let repo = detected_repo(state);
    let source = resolve_token(&token_store(repo.as_ref()))?.map(|t| t.source);
    let login = source.and_then(|s| forge.login_for(s));
    Ok(ForgeStatus {
        repo,
        auth: ForgeAuth { source, login },
    })
}

/// Connection state without touching the network: the detected repo, where
/// a token would come from, and the login when this session verified one.
/// Async only to keep the keychain read and `gh` subprocess off the UI
/// thread.
#[tauri::command(async)]
pub fn forge_status(
    state: State<'_, AppState>,
    forge: State<'_, ForgeState>,
) -> Result<ForgeStatus, CommandError> {
    assemble_status(&state, &forge)
}

/// Verify the resolved token against the API and remember the login. With
/// no token anywhere this is not an error — it answers the plain
/// unauthenticated status so the UI can render the connect state.
#[tauri::command(async)]
pub fn forge_verify(
    state: State<'_, AppState>,
    forge: State<'_, ForgeState>,
) -> Result<ForgeStatus, CommandError> {
    let repo = detected_repo(&state);
    let Some(resolved) = resolve_token(&token_store(repo.as_ref()))? else {
        forge.forget();
        return assemble_status(&state, &forge);
    };
    match client_for(repo.as_ref(), &resolved.token)?.viewer() {
        Ok(login) => forge.remember(resolved.source, login),
        Err(err) => {
            forge.forget();
            return Err(err.into());
        }
    }
    assemble_status(&state, &forge)
}

/// The in-app login: validate the pasted token against the API, then store
/// it in the system keychain. An invalid token is refused and nothing is
/// stored.
#[tauri::command(async)]
pub fn forge_login(
    state: State<'_, AppState>,
    forge: State<'_, ForgeState>,
    token: String,
) -> Result<ForgeStatus, CommandError> {
    let token = token.trim();
    if token.is_empty() {
        return Err(ForgeError::AuthFailed("no token was entered".into()).into());
    }
    let repo = detected_repo(&state);
    let login = client_for(repo.as_ref(), token)?.viewer()?;
    token_store(repo.as_ref()).set(token)?;
    forge.remember(TokenSource::Keychain, login);
    assemble_status(&state, &forge)
}

/// Remove Jiji's stored token. Tokens managed outside Jiji (environment,
/// gh CLI) are untouched, so the answered status may still be
/// authenticated — the UI states where that token lives instead of
/// offering a disconnect that cannot work.
#[tauri::command(async)]
pub fn forge_logout(
    state: State<'_, AppState>,
    forge: State<'_, ForgeState>,
) -> Result<ForgeStatus, CommandError> {
    let repo = detected_repo(&state);
    token_store(repo.as_ref()).delete()?;
    forge.forget();
    assemble_status(&state, &forge)
}

/// The detected repo's open-PR state: the one batched query workbench
/// badges and publish flows hang on, answered with the bookmark-attachment
/// map already built (head branch → PR, fork-filtered).
#[tauri::command(async)]
pub fn forge_prs(
    state: State<'_, AppState>,
    forge: State<'_, ForgeState>,
) -> Result<RepoPrState, CommandError> {
    let repo = detected_repo(&state).ok_or_else(no_github_remote)?;
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
}

/// The connected repo, its client, and fresh open-PR state — what both
/// submit commands start from.
fn submit_context(
    state: &AppState,
) -> Result<(ForgeRepo, GitHubClient, RepoPrState), CommandError> {
    let repo = detected_repo(state).ok_or_else(no_github_remote)?;
    let resolved = resolve_token(&token_store(Some(&repo)))?.ok_or(ForgeError::NoToken)?;
    let client = GitHubClient::for_repo(&repo, &resolved.token)?;
    let prs = RepoPrState::new(client.open_prs(&repo.owner, &repo.name)?, &repo.owner);
    Ok((repo, client, prs))
}

/// The repo's PR template from the trunk tree, when one exists — folded
/// into the bodies of PRs a submit plan creates.
fn pr_template(state: &AppState) -> Option<PrTemplate> {
    state
        .trunk_text_file(&pr_template_candidates())
        .map(|(path, text)| PrTemplate { path, text })
}

/// Plan submitting the stack under a bookmark: what would push, which PRs
/// would open against which bases, which existing PRs retarget, which PR
/// text and stack comments refresh. Read-only — GitHub is asked for fresh
/// open-PR state and existing stack comments, nothing else runs.
#[tauri::command(async)]
pub fn submit_plan(
    state: State<'_, AppState>,
    head_bookmark: String,
) -> Result<SubmitPlan, CommandError> {
    let snapshot = state
        .current_snapshot_clone()
        .ok_or_else(|| CommandError::new("no_repo_open", "No repository is currently open"))?;
    let (repo, client, prs) = submit_context(&state)?;
    let forge_side = RepoForge {
        client: &client,
        repo: &repo,
    };
    let template = pr_template(&state);
    plan_submit(
        &snapshot,
        &prs,
        &repo,
        &head_bookmark,
        &forge_side,
        template.as_ref(),
    )
    .map_err(Into::into)
}

/// The submit executor's jj half: pushes run through the shared mutation
/// path so the snapshot republishes to every surface mid-flow.
struct HostVcs<'a> {
    app: &'a AppHandle,
    state: &'a AppState,
}

impl SubmitVcs for HostVcs<'_> {
    fn push_bookmarks(
        &self,
        bookmarks: &[String],
        remote: &str,
    ) -> Result<String, jiji_core::BackendError> {
        self.state
            .push_and_publish(self.app, bookmarks, remote)
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
            .mutate(self.app, |backend, path| {
                backend.git_fetch(path, Some(&remotes))
            })
            .map(|outcome| outcome.summary)
            .map_err(|err| jiji_core::BackendError::MutationFailed(err.message))
    }

    fn snapshot(&self) -> Result<jiji_core::snapshot::RepoSnapshot, jiji_core::BackendError> {
        self.state.current_snapshot_clone().ok_or_else(|| {
            jiji_core::BackendError::MutationFailed("No repository is currently open".into())
        })
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
            .mutate(self.app, |backend, path| {
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
            .mutate(self.app, |backend, path| backend.delete_bookmark(path, name))
            .map(|outcome| outcome.summary)
            .map_err(|err| jiji_core::BackendError::MutationFailed(err.message))
    }

    fn abandon_changes(&self, change_ids: &[String]) -> Result<String, jiji_core::BackendError> {
        self.state
            .mutate(self.app, |backend, path| {
                backend.abandon_changes(path, change_ids)
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
#[tauri::command(async)]
pub fn submit_stack(
    app: AppHandle,
    state: State<'_, AppState>,
    head_bookmark: String,
    plan: SubmitPlan,
) -> Result<SubmitOutcome, CommandError> {
    let snapshot = state
        .current_snapshot_clone()
        .ok_or_else(|| CommandError::new("no_repo_open", "No repository is currently open"))?;
    let (repo, client, prs) = submit_context(&state)?;
    let forge_side = RepoForge {
        client: &client,
        repo: &repo,
    };
    let template = pr_template(&state);
    let fresh = plan_submit(
        &snapshot,
        &prs,
        &repo,
        &head_bookmark,
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
    };
    execute_submit(&fresh, &vcs, &forge_side).map_err(Into::into)
}

/// One PR by number — the review flow's lookup for PRs the batched
/// open-PR state cannot see (past the 100 cap, or closed), so "fetch any
/// PR" really means any.
#[tauri::command(async)]
pub fn forge_pr(
    state: State<'_, AppState>,
    number: u64,
) -> Result<PrSummary, CommandError> {
    let repo = detected_repo(&state).ok_or_else(no_github_remote)?;
    let resolved = resolve_token(&token_store(Some(&repo)))?.ok_or(ForgeError::NoToken)?;
    let client = GitHubClient::for_repo(&repo, &resolved.token)?;
    client
        .pr_by_number(&repo.owner, &repo.name, number)
        .map_err(Into::into)
}

/// Re-run the failed GitHub Actions runs on a PR's head. The PR is
/// re-fetched first so the re-run targets the head GitHub currently has,
/// not the possibly-stale one a badge was drawn from; the answer says
/// which workflow runs went back to work (empty means the failing check
/// lives outside GitHub Actions' reach).
#[tauri::command(async)]
pub fn rerun_failed_ci(
    state: State<'_, AppState>,
    number: u64,
) -> Result<CiRerunReport, CommandError> {
    let repo = detected_repo(&state).ok_or_else(no_github_remote)?;
    let resolved = resolve_token(&token_store(Some(&repo)))?.ok_or(ForgeError::NoToken)?;
    let client = GitHubClient::for_repo(&repo, &resolved.token)?;
    let pr = client.pr_by_number(&repo.owner, &repo.name, number)?;
    jiji_forge::rerun_failed_ci(&client, &repo, &pr.head_commit).map_err(Into::into)
}

/// Plan landing the stack under a bookmark: what already merged on GitHub,
/// whether the bottom PR merges now (or hands off to auto-merge or a merge
/// queue), and the reconcile that follows — fetch, rebase, push, retarget,
/// bookmark and change cleanup. Read-only: GitHub is asked for fresh
/// open-PR state, the candidate's land state, and merged-PR recognition,
/// nothing else runs.
#[tauri::command(async)]
pub fn land_plan(
    state: State<'_, AppState>,
    head_bookmark: String,
) -> Result<LandPlan, CommandError> {
    let snapshot = state
        .current_snapshot_clone()
        .ok_or_else(|| CommandError::new("no_repo_open", "No repository is currently open"))?;
    let (repo, client, prs) = submit_context(&state)?;
    let forge_side = LandRepoForge {
        client: &client,
        repo: &repo,
    };
    plan_land(&snapshot, &prs, &repo, &head_bookmark, &forge_side).map_err(Into::into)
}

/// Execute a confirmed land plan. The plan is re-derived against the
/// current snapshot and fresh GitHub state first and refused when its
/// actions no longer match what the user confirmed (the stack, the PR, or
/// its checks moved under the panel — the same never-clobber posture as
/// submit); the merge step re-checks GitHub once more just before merging.
#[tauri::command(async)]
pub fn land_stack(
    app: AppHandle,
    state: State<'_, AppState>,
    head_bookmark: String,
    plan: LandPlan,
) -> Result<LandOutcome, CommandError> {
    let snapshot = state
        .current_snapshot_clone()
        .ok_or_else(|| CommandError::new("no_repo_open", "No repository is currently open"))?;
    let (repo, client, prs) = submit_context(&state)?;
    let forge_side = LandRepoForge {
        client: &client,
        repo: &repo,
    };
    let fresh = plan_land(&snapshot, &prs, &repo, &head_bookmark, &forge_side)?;
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
    };
    execute_land(&fresh, &vcs, &forge_side).map_err(Into::into)
}
