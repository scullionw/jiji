//! JJ-native repo snapshot and mutation boundaries for Jiji.
//!
//! This crate should stay focused on local repository state and JJ operations.
//! Remote forges, PRs, and auto-land orchestration belong in `jiji-forge`.
//!
//! The UI consumes immutable snapshot DTOs (`snapshot`) produced through the
//! `RepoBackend` boundary (`backend`). The real implementation is the
//! jj-lib-backed `JjBackend` (`jj`); a deterministic mock (`mock`) remains
//! available for UI development against stable data.

pub mod backend;
pub mod jj;
pub(crate) mod merge_tool;
pub mod mock;
pub mod settings;
pub mod snapshot;
pub mod watch;

pub use backend::{BackendError, MockBackend, RepoBackend};
pub use jj::JjBackend;
pub use settings::UserConfigSource;
pub use snapshot::RepoSnapshot;
pub use watch::RepoWatcher;
