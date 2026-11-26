//! Shared remote workflow engine for PR stacks, landing, and auto-land.
//!
//! This crate stays headless — synchronous, no UI types, no async runtime —
//! so the desktop app and the CLI host the same logic. The engine's shape
//! follows jjpr: forge facts fetched fresh through one small client, with
//! explicit plan-then-execute flows layered on top.
//!
//! What lives here:
//! - [`remote`]: detecting the GitHub repo behind a jj repo's git remotes
//! - [`auth`]: token storage (system keychain) and the resolution chain
//! - [`github`]: the API client (REST for identity, PR writes, GitHub's
//!   stacks and its asynchronous merge; one batched GraphQL query for PR
//!   state)
//! - [`pr`]: PR state mapped into Jiji-owned DTOs
//! - [`submit`]: the analyze → plan → execute submission engine
//! - [`naming`]: branch-name derivation for unbookmarked stack heads
//! - [`reconcile`]: fingerprinted PR title/description reconciliation
//! - [`comment`]: the stack-info comment GitHub readers see
//! - [`land`]: the merge → fetch → check → reconcile landing engine
//! - [`autoland`]: the supervised auto-land job loop over the land engine
//! - [`ship`]: the direct-to-trunk shipping engine (no PR — move trunk, push)
//! - [`template`]: the repo's PR template folded into new PR bodies
//! - [`ci`]: the re-run-failed-CI review helper over GitHub Actions
//! - [`stacks`]: shared local and remote PR-stack discovery and status
//! - [`readiness`]: marking a stack's PRs ready for review or back to drafts
//! - [`prefs`]: the preferences the app and the command share

pub mod auth;
pub mod autoland;
pub mod ci;
pub mod comment;
pub mod error;
pub mod github;
pub mod land;
pub mod naming;
pub mod pr;
pub mod prefs;
pub mod readiness;
pub mod reconcile;
pub mod remote;
pub mod ship;
pub mod stacks;
pub mod submit;
pub mod template;

pub use auth::{
    resolve_token, ForgeAuth, ForgeStatus, KeychainTokenStore, MemoryTokenStore, ResolvedToken,
    TokenSource, TokenStore,
};
pub use autoland::{
    load_autoland_records, remove_autoland_record, run_autoland, save_autoland_records,
    unix_now_ms, upsert_autoland_record, AutoLandConfig, AutoLandMerged, AutoLandPhase,
    AutoLandPrs, AutoLandRecord, AutoLandState, AutoLandStatus, StopSignal,
};
pub use ci::{rerun_failed_ci, CiRerunReport};
pub use error::ForgeError;
pub use github::GitHubClient;
pub use land::{
    execute_land, plan_land, LandAction, LandBlocker, LandForge, LandOptions, LandOutcome,
    LandPlan, LandRepoForge, LandSegment, LandSegmentStatus, LandStep, LandVcs, LandsWith,
    MergeMethod, MergeStateStatus, PrLandState,
};
pub use naming::{derive_branch_name, slug_description};
pub use pr::{
    others_on, parse_open_prs, prs_by_branch, ChecksRollup, PrState, PrStateReport, PrSummary,
    RepoPrState, ReviewDecision, StackSeat,
};
pub use readiness::{
    execute_readiness, plan_readiness, PrDraftState, PrReadiness, ReadinessAction,
    ReadinessForge, ReadinessOutcome, ReadinessPlan, ReadinessStep, ReadinessUnchanged,
};
pub use remote::{detect_github_repo, no_github_remote, parse_github_url, ForgeProvider, ForgeRepo};
pub use ship::{
    execute_ship, plan_ship, ShipAction, ShipForge, ShipOutcome, ShipPlan, ShipStep, ShipVcs,
    SHIPPED_WITHOUT_GITHUB,
};
pub use stacks::{discover_stacks, GithubStack, PublishStack, StackSegment, StackStatus};
pub use submit::{
    execute_submit, plan_submit, ExistingComment, RepoForge, StackCommentSource, StackLink,
    SubmitAction, SubmitForge, SubmitHead, SubmitOptions, SubmitOutcome, SubmitPlan,
    SubmitSegment, SubmitStep, SubmitStepStatus, SubmitVcs,
};
pub use template::{new_pr_body, pr_template_candidates, PrTemplate};
