//! Shared remote workflow engine for PR stacks, landing, and auto-land.
//!
//! This crate stays headless — synchronous, no UI types, no async runtime —
//! so the same logic can be hosted by the Tauri app now and a future CLI
//! later. The engine's shape follows jjpr (see the jjpr inspiration note):
//! forge facts fetched fresh through one small client, explicit
//! plan-then-execute flows layered on top in later milestones.
//!
//! What lives here today:
//! - [`remote`]: detecting the GitHub repo behind a jj repo's git remotes
//! - [`auth`]: token storage (system keychain) and the resolution chain
//! - [`github`]: the API client (REST for identity, one batched GraphQL
//!   query for PR state)
//! - [`pr`]: PR state mapped into Jiji-owned, TS-exported DTOs

pub mod auth;
pub mod error;
pub mod github;
pub mod pr;
pub mod remote;

pub use auth::{
    resolve_token, ForgeAuth, ForgeStatus, KeychainTokenStore, MemoryTokenStore, ResolvedToken,
    TokenSource, TokenStore,
};
pub use error::ForgeError;
pub use github::GitHubClient;
pub use pr::{
    parse_open_prs, prs_by_branch, ChecksRollup, PrState, PrStateReport, PrSummary,
    ReviewDecision,
};
pub use remote::{detect_github_repo, no_github_remote, parse_github_url, ForgeProvider, ForgeRepo};
