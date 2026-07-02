//! GitHub authentication: where the token lives and how it resolves.
//!
//! Jiji's own storage is the system keychain (macOS Keychain Services via
//! the `keyring` crate) — the desktop-app equivalent of `gh auth login`.
//! Zero-config setups keep working through the same fallback chain jjpr
//! uses: an explicit environment token, then the `gh` CLI's stored
//! credentials. The resolved source is surfaced to the UI so
//! externally-managed tokens read as such (disconnecting in Jiji only
//! clears the keychain).

use std::process::Command;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::error::ForgeError;
use crate::remote::ForgeRepo;

/// Where the active token came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum TokenSource {
    /// Stored by Jiji in the system keychain (the in-app login).
    Keychain,
    /// `GITHUB_TOKEN` / `GH_TOKEN` environment variable.
    Environment,
    /// Read from the `gh` CLI's credential store (`gh auth token`).
    GhCli,
}

/// Authentication state for the UI: which token would be used, and the
/// login it verified as (when a verification has happened this session).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ForgeAuth {
    /// `None` — no token anywhere; the UI offers connecting.
    pub source: Option<TokenSource>,
    /// GitHub login the token last verified as, when known.
    pub login: Option<String>,
}

/// Everything the Publish surface needs to render the forge connection:
/// the detected repository plus authentication state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ForgeStatus {
    /// GitHub repo behind the open jj repo's preferred remote, when one is.
    pub repo: Option<ForgeRepo>,
    pub auth: ForgeAuth,
}

/// A token plus where it came from.
#[derive(Debug, Clone)]
pub struct ResolvedToken {
    pub token: String,
    pub source: TokenSource,
}

/// Persistent token storage. One implementation is the real keychain; tests
/// and the harness use the in-memory store.
pub trait TokenStore: Send + Sync {
    fn get(&self) -> Result<Option<String>, ForgeError>;
    fn set(&self, token: &str) -> Result<(), ForgeError>;
    fn delete(&self) -> Result<(), ForgeError>;
}

/// System-keychain storage, one entry per forge host. On macOS the entry
/// lands in Keychain Services as service "jiji.github" with the host as the
/// account, so a future GitHub Enterprise host gets its own token.
pub struct KeychainTokenStore {
    host: String,
}

impl KeychainTokenStore {
    pub fn new(host: impl Into<String>) -> Self {
        Self { host: host.into() }
    }

    fn entry(&self) -> Result<keyring::Entry, ForgeError> {
        keyring::Entry::new("jiji.github", &self.host)
            .map_err(|err| ForgeError::Keychain(err.to_string()))
    }
}

impl TokenStore for KeychainTokenStore {
    fn get(&self) -> Result<Option<String>, ForgeError> {
        match self.entry()?.get_password() {
            Ok(token) if token.is_empty() => Ok(None),
            Ok(token) => Ok(Some(token)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(err) => Err(ForgeError::Keychain(err.to_string())),
        }
    }

    fn set(&self, token: &str) -> Result<(), ForgeError> {
        self.entry()?
            .set_password(token)
            .map_err(|err| ForgeError::Keychain(err.to_string()))
    }

    fn delete(&self) -> Result<(), ForgeError> {
        match self.entry()?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(err) => Err(ForgeError::Keychain(err.to_string())),
        }
    }
}

/// In-memory store for tests and stubbed hosts.
#[derive(Default)]
pub struct MemoryTokenStore {
    token: Mutex<Option<String>>,
}

impl TokenStore for MemoryTokenStore {
    fn get(&self) -> Result<Option<String>, ForgeError> {
        Ok(self.token.lock().expect("token lock poisoned").clone())
    }

    fn set(&self, token: &str) -> Result<(), ForgeError> {
        *self.token.lock().expect("token lock poisoned") = Some(token.to_owned());
        Ok(())
    }

    fn delete(&self) -> Result<(), ForgeError> {
        *self.token.lock().expect("token lock poisoned") = None;
        Ok(())
    }
}

/// Resolve the token Jiji should use: the keychain (Jiji's own login),
/// then `GITHUB_TOKEN`/`GH_TOKEN`, then the `gh` CLI's stored credentials.
/// `None` means nothing anywhere — the UI's connect state. A keychain read
/// failure degrades to the fallbacks (an env token should still work when
/// the keychain is locked) and is logged, not returned.
pub fn resolve_token(store: &dyn TokenStore) -> Result<Option<ResolvedToken>, ForgeError> {
    resolve_token_impl(
        store,
        &|var| std::env::var(var).ok(),
        &gh_auth_token,
    )
}

fn resolve_token_impl(
    store: &dyn TokenStore,
    env: &dyn Fn(&str) -> Option<String>,
    gh: &dyn Fn() -> Option<String>,
) -> Result<Option<ResolvedToken>, ForgeError> {
    match store.get() {
        Ok(Some(token)) => {
            return Ok(Some(ResolvedToken {
                token,
                source: TokenSource::Keychain,
            }));
        }
        Ok(None) => {}
        Err(err) => log::warn!("keychain token read failed; trying fallbacks: {err}"),
    }
    for var in ["GITHUB_TOKEN", "GH_TOKEN"] {
        if let Some(token) = env(var).filter(|t| !t.is_empty()) {
            return Ok(Some(ResolvedToken {
                token,
                source: TokenSource::Environment,
            }));
        }
    }
    if let Some(token) = gh() {
        return Ok(Some(ResolvedToken {
            token,
            source: TokenSource::GhCli,
        }));
    }
    Ok(None)
}

/// `gh auth token`, looked up like the merge tool looks up Sublime Merge:
/// PATH first, then the usual install locations — GUI-launched macOS apps
/// get launchd's minimal PATH, which misses Homebrew.
fn gh_auth_token() -> Option<String> {
    const CANDIDATES: [&str; 3] = ["gh", "/opt/homebrew/bin/gh", "/usr/local/bin/gh"];
    for program in CANDIDATES {
        let Ok(output) = Command::new(program).args(["auth", "token"]).output() else {
            continue;
        };
        if !output.status.success() {
            // gh exists but has no token; the other candidates are the same
            // binary, so stop here.
            return None;
        }
        let token = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        return (!token.is_empty()).then_some(token);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn no_env(_: &str) -> Option<String> {
        None
    }

    fn no_gh() -> Option<String> {
        None
    }

    #[test]
    fn keychain_wins_over_everything() {
        let store = MemoryTokenStore::default();
        store.set("stored").unwrap();
        let resolved = resolve_token_impl(
            &store,
            &|_| Some("from-env".into()),
            &|| Some("from-gh".into()),
        )
        .unwrap()
        .unwrap();
        assert_eq!(resolved.token, "stored");
        assert_eq!(resolved.source, TokenSource::Keychain);
    }

    #[test]
    fn env_wins_over_gh_and_prefers_github_token() {
        let store = MemoryTokenStore::default();
        let resolved = resolve_token_impl(
            &store,
            &|var| match var {
                "GITHUB_TOKEN" => Some("primary".into()),
                "GH_TOKEN" => Some("secondary".into()),
                _ => None,
            },
            &|| Some("from-gh".into()),
        )
        .unwrap()
        .unwrap();
        assert_eq!(resolved.token, "primary");
        assert_eq!(resolved.source, TokenSource::Environment);

        // An empty GITHUB_TOKEN falls through to GH_TOKEN.
        let resolved = resolve_token_impl(
            &store,
            &|var| match var {
                "GITHUB_TOKEN" => Some(String::new()),
                "GH_TOKEN" => Some("secondary".into()),
                _ => None,
            },
            &no_gh,
        )
        .unwrap()
        .unwrap();
        assert_eq!(resolved.token, "secondary");
    }

    #[test]
    fn gh_cli_is_the_last_fallback_and_none_means_none() {
        let store = MemoryTokenStore::default();
        let resolved = resolve_token_impl(&store, &no_env, &|| Some("gho_x".into()))
            .unwrap()
            .unwrap();
        assert_eq!(resolved.source, TokenSource::GhCli);

        assert!(resolve_token_impl(&store, &no_env, &no_gh)
            .unwrap()
            .is_none());
    }

    #[test]
    fn broken_keychain_degrades_to_fallbacks() {
        struct BrokenStore;
        impl TokenStore for BrokenStore {
            fn get(&self) -> Result<Option<String>, ForgeError> {
                Err(ForgeError::Keychain("locked".into()))
            }
            fn set(&self, _: &str) -> Result<(), ForgeError> {
                unreachable!()
            }
            fn delete(&self) -> Result<(), ForgeError> {
                unreachable!()
            }
        }
        let resolved = resolve_token_impl(
            &BrokenStore,
            &|var| (var == "GITHUB_TOKEN").then(|| "env-token".into()),
            &no_gh,
        )
        .unwrap()
        .unwrap();
        assert_eq!(resolved.source, TokenSource::Environment);
    }
}
