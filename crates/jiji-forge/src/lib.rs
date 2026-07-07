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
//! - [`github`]: the API client (REST for identity and PR writes, one
//!   batched GraphQL query for PR state)
//! - [`pr`]: PR state mapped into Jiji-owned, TS-exported DTOs
//! - [`submit`]: the analyze → plan → execute submission engine
//! - [`reconcile`]: fingerprinted PR title/description reconciliation
//! - [`comment`]: the stack-info comment GitHub readers see
//! - [`land`]: the merge → fetch → reconcile landing engine
//! - [`autoland`]: the supervised auto-land job loop over the land engine
//! - [`template`]: the repo's PR template folded into new PR bodies
//! - [`ci`]: the re-run-failed-CI review helper over GitHub Actions

pub mod auth;
pub mod autoland;
pub mod ci;
pub mod comment;
pub mod error;
pub mod github;
pub mod land;
pub mod pr;
pub mod reconcile;
pub mod remote;
pub mod submit;
pub mod template;

pub use auth::{
    resolve_token, ForgeAuth, ForgeStatus, KeychainTokenStore, MemoryTokenStore, ResolvedToken,
    TokenSource, TokenStore,
};
pub use autoland::{
    clear_autoland_record, load_autoland_record, run_autoland, save_autoland_record, unix_now_ms,
    AutoLandConfig, AutoLandMerged, AutoLandPhase, AutoLandPrs, AutoLandRecord, AutoLandState,
    AutoLandStatus, StopSignal, AUTOLAND_RECORD_VERSION,
};
pub use ci::{rerun_failed_ci, CiRerunReport};
pub use error::ForgeError;
pub use github::GitHubClient;
pub use land::{
    execute_land, plan_land, LandAction, LandBlocker, LandForge, LandOutcome, LandPlan,
    LandRepoForge, LandSegment, LandSegmentStatus, LandStep, LandVcs, MergeMethod, PrLandState,
};
pub use pr::{
    parse_open_prs, prs_by_branch, ChecksRollup, PrState, PrStateReport, PrSummary,
    RepoPrState, ReviewDecision,
};
pub use remote::{detect_github_repo, no_github_remote, parse_github_url, ForgeProvider, ForgeRepo};
pub use submit::{
    execute_submit, plan_submit, ExistingComment, RepoForge, StackCommentSource, SubmitAction,
    SubmitForge, SubmitOutcome, SubmitPlan, SubmitSegment, SubmitStep, SubmitStepStatus,
    SubmitVcs,
};
pub use template::{new_pr_body, pr_template_candidates, PrTemplate};
