//! Boundary-safe forge errors, mirroring `jiji-core`'s `BackendError` shape:
//! plain-language messages plus a stable machine-readable code for the UI.

#[derive(Debug, thiserror::Error)]
pub enum ForgeError {
    #[error(
        "No GitHub token found. Connect in the Publish section, set GITHUB_TOKEN, or run `gh auth login`"
    )]
    NoToken,
    #[error("GitHub rejected the token: {0}")]
    AuthFailed(String),
    #[error("GitHub rate limit exceeded: {0}")]
    RateLimited(String),
    #[error("Not found on GitHub: {0}")]
    NotFound(String),
    #[error("GitHub request failed: {0}")]
    Api(String),
    /// The repository, or its host, has no stacked pull requests: the
    /// stacks endpoints answer a bare 404 there. Kept apart from other
    /// failed requests because a publish that links a stack only by a
    /// setting goes on without the link.
    #[error("GitHub request failed: {0}")]
    NoStacks(String),
    #[error("Could not reach GitHub: {0}")]
    Network(String),
    #[error("Could not access the system keychain: {0}")]
    Keychain(String),
    #[error("Cannot submit this stack: {0}")]
    Plan(String),
    #[error("Cannot land this stack: {0}")]
    Land(String),
    #[error("Cannot ship this stack: {0}")]
    Ship(String),
}

impl ForgeError {
    /// Stable machine-readable code for the UI.
    pub fn code(&self) -> &'static str {
        match self {
            ForgeError::NoToken => "no_token",
            ForgeError::AuthFailed(_) => "auth_failed",
            ForgeError::RateLimited(_) => "rate_limited",
            ForgeError::NotFound(_) => "not_found",
            ForgeError::Api(_) | ForgeError::NoStacks(_) => "api_failed",
            ForgeError::Network(_) => "network_failed",
            ForgeError::Keychain(_) => "keychain_failed",
            ForgeError::Plan(_) => "plan_failed",
            ForgeError::Land(_) => "plan_failed",
            ForgeError::Ship(_) => "plan_failed",
        }
    }
}
