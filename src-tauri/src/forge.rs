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
    detect_github_repo, no_github_remote, resolve_token, ForgeAuth, ForgeError, ForgeRepo,
    ForgeStatus, GitHubClient, KeychainTokenStore, RepoPrState, TokenSource, TokenStore as _,
};
use tauri::State;

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
