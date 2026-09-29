//! UI-facing command surface.
//!
//! Commands stay thin: validate input, call the `RepoBackend` boundary in
//! `jiji-core`, store the latest snapshot, and broadcast it as an event so
//! every surface refreshes from the same immutable state.
//!
//! Several repos can be open at once (the frontend's tabs), so state is a
//! session map keyed by the canonical repo path and every repo-scoped
//! command names its repo explicitly. The snapshot event carries the
//! snapshot whole; the frontend routes it to the matching tab by its
//! `repoPath`, so background repos (watcher ticks, a pinned auto-land
//! job's mutations) stay fresh without owning the screen.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;

use jiji_core::snapshot::{
    ChangeDetail, ChangeDiff, ChangeEvolog, ConflictSide, FileAnnotation, HistorySearch,
    MutationOutcome, SnapshotView, SplitHunk, SplitSelection,
};
use jiji_core::{BackendError, JjBackend, MockBackend, RepoBackend, RepoSnapshot, RepoWatcher};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager as _, State};

pub const SNAPSHOT_UPDATED_EVENT: &str = "snapshot://updated";

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandError {
    pub code: String,
    pub message: String,
    /// The folder an error is about, when the UI can act on it (the Git
    /// root that `git_repo_without_jj` offers to set jj up in).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

impl CommandError {
    pub(crate) fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            path: None,
        }
    }
}

impl From<BackendError> for CommandError {
    fn from(err: BackendError) -> Self {
        let mut command = Self::new(err.code(), err.to_string());
        if let BackendError::GitRepoWithoutJj(root) = &err {
            command.path = Some(root.clone());
        }
        command
    }
}

/// One open repo (one tab): its latest snapshot, the view lens its
/// snapshots render, and the watcher that keeps it fresh while other tabs
/// own the screen.
struct RepoSession {
    snapshot: RepoSnapshot,
    /// The view lens snapshots render. Every refresh path reads it, so
    /// watcher ticks and post-mutation reloads keep the lens the user
    /// chose for this repo.
    view: SnapshotView,
    /// Watches the repo for out-of-band changes (file edits, CLI
    /// operations, git ref moves). Held for its lifetime only: dropping
    /// the session stops the watcher.
    _watcher: Option<RepoWatcher>,
}

pub struct AppState {
    backend: Box<dyn RepoBackend>,
    /// Sessions keyed by the canonical repo path (`snapshot.repo_path`,
    /// the workspace root). The frontend always passes that same string
    /// back, so lookups never chase aliases.
    sessions: Mutex<HashMap<String, RepoSession>>,
}

impl AppState {
    pub fn new() -> Self {
        // The mock stays available for UI work against stable, fabricated data.
        let backend: Box<dyn RepoBackend> =
            if std::env::var("JIJI_MOCK_BACKEND").as_deref() == Ok("1") {
                log::info!("JIJI_MOCK_BACKEND=1 set; serving mock snapshots");
                Box::new(MockBackend::default())
            } else {
                Box::new(JjBackend::default())
            };
        Self {
            backend,
            sessions: Mutex::new(HashMap::new()),
        }
    }

    fn sessions(&self) -> std::sync::MutexGuard<'_, HashMap<String, RepoSession>> {
        self.sessions.lock().expect("repo sessions lock poisoned")
    }

    /// The view lens for an open repo; `None` when no session exists (a
    /// closed tab, or a path spelled differently than the canonical root).
    fn session_view(&self, path: &str) -> Option<SnapshotView> {
        self.sessions().get(path).map(|s| s.view.clone())
    }

    /// The lens refreshes should render for `path`: the session's chosen
    /// one, or the default for a repo not open yet.
    fn view_for(&self, path: &str) -> SnapshotView {
        self.session_view(path).unwrap_or_default()
    }

    /// Store the snapshot on its session (when one is open — a closed
    /// tab's late publish only emits) and broadcast it. The frontend
    /// routes the event by the snapshot's own repo path.
    fn publish(&self, app: &AppHandle, snapshot: &RepoSnapshot) {
        if let Some(session) = self.sessions().get_mut(&snapshot.repo_path) {
            session.snapshot = snapshot.clone();
        }
        if let Err(err) = app.emit(SNAPSHOT_UPDATED_EVENT, snapshot) {
            log::warn!("failed to emit snapshot update: {err}");
        }
    }

    /// Post-mutation reload: the mutation already synced the working copy
    /// and git state, so this only reads and republishes.
    fn load_and_publish(&self, app: &AppHandle, path: &Path) -> Result<RepoSnapshot, CommandError> {
        let key = path.to_string_lossy();
        let snapshot = self.backend.open_with(path, &self.view_for(&key))?;
        self.publish(app, &snapshot);
        Ok(snapshot)
    }

    /// Open/refresh: bring the repo up to date like running a jj command
    /// would (snapshot working-copy edits, import externally-moved git
    /// state), then publish.
    fn sync_and_publish(&self, app: &AppHandle, path: &Path) -> Result<RepoSnapshot, CommandError> {
        let key = path.to_string_lossy();
        let snapshot = self.backend.refresh_with(path, &self.view_for(&key))?;
        self.publish(app, &snapshot);
        Ok(snapshot)
    }

    /// Watch the repo so edits, CLI commands, and git ref moves refresh the
    /// snapshot without ⌘R — for background tabs too. Failing to watch only
    /// loses the automation, so it logs instead of failing the open.
    fn start_watcher(&self, app: &AppHandle, path: &str) -> Option<RepoWatcher> {
        let handle = app.clone();
        let repo_path = path.to_owned();
        self.backend
            .watch(
                Path::new(path),
                Box::new(move || auto_refresh(&handle, &repo_path)),
            )
            .unwrap_or_else(|err| {
                log::warn!("repo watching unavailable: {err}");
                None
            })
    }
}

/// One watcher tick: refresh and publish only when something actually
/// changed — republishing an identical snapshot would make every surface
/// rerender and the diff refetch for nothing. Runs on the watcher thread;
/// concurrent jj access from commands is safe the same way two CLI
/// processes are (op-store and working-copy locks). A session closed since
/// the tick was queued is left alone.
fn auto_refresh(app: &AppHandle, path: &str) {
    let state: State<'_, AppState> = app.state();
    let Some(view) = state.session_view(path) else {
        return;
    };
    match state.backend.refresh_with(Path::new(path), &view) {
        Ok(snapshot) => {
            {
                let sessions = state.sessions();
                if sessions.get(path).map(|s| &s.snapshot) == Some(&snapshot) {
                    return;
                }
            }
            state.publish(app, &snapshot);
        }
        // Transient by nature (e.g. lock contention with a running CLI
        // command); the next relevant change retries.
        Err(err) => log::warn!("auto-refresh failed: {err}"),
    }
}

/// jj works from anywhere inside a workspace, so opening does too: a
/// subfolder or file (picked in the open dialog) opens its enclosing repo.
/// A path that no longer exists passes through untouched, so a stale
/// recent entry reports itself missing instead of quietly opening
/// whichever repo encloses where it used to be.
fn resolve_open_path(path: String) -> String {
    let requested = Path::new(&path);
    if !requested.exists() {
        return path;
    }
    match crate::cli::repo_root_for(requested) {
        Some(root) => root.to_string_lossy().into_owned(),
        None => path,
    }
}

/// Open a repo — or re-sync one that is already open (activating its tab
/// re-runs this; the session keeps its chosen view lens). A fresh repo
/// starts on the default lens and gets its own watcher.
///
/// This and the other snapshot reads below are `async`, off the main
/// thread: each one snapshots the working copy, imports git refs and
/// builds (then serializes) a whole snapshot, and while the main thread
/// is busy the window cannot present a frame or take input — a tab switch
/// would paint only once its re-sync finished. The watcher already runs
/// the same refresh from its own thread, so this adds no new concurrency.
#[tauri::command(async)]
pub fn open_repo(
    app: AppHandle,
    state: State<'_, AppState>,
    path: String,
) -> Result<RepoSnapshot, CommandError> {
    let path = resolve_open_path(path);
    let mut snapshot = state
        .backend
        .refresh_with(Path::new(&path), &state.view_for(&path))?;
    // The requested path may still be an alias of the canonical root (a
    // symlink, a non-canonical spelling). When the canonical repo is
    // already open under a chosen lens, re-render under that lens instead
    // of clobbering it.
    if snapshot.repo_path != path {
        if let Some(view) = state.session_view(&snapshot.repo_path) {
            if view != snapshot.view {
                snapshot = state
                    .backend
                    .refresh_with(Path::new(&snapshot.repo_path), &view)?;
            }
        }
    }
    let canonical = snapshot.repo_path.clone();
    {
        let mut sessions = state.sessions();
        match sessions.get_mut(&canonical) {
            Some(session) => session.snapshot = snapshot.clone(),
            None => {
                let view = snapshot.view.clone();
                let watcher = state.start_watcher(&app, &canonical);
                sessions.insert(
                    canonical.clone(),
                    RepoSession {
                        snapshot: snapshot.clone(),
                        view,
                        _watcher: watcher,
                    },
                );
            }
        }
    }
    if let Err(err) = app.emit(SNAPSHOT_UPDATED_EVENT, &snapshot) {
        log::warn!("failed to emit snapshot update: {err}");
    }
    Ok(snapshot)
}

/// Set jj up in a plain Git repository — `jj git init --colocate` at the
/// Git root enclosing `path` — and return that root for `open_repo`. Only
/// ever runs from an explicit confirmation in the UI.
#[tauri::command(async)]
pub fn init_jj_repo(state: State<'_, AppState>, path: String) -> Result<String, CommandError> {
    let root = jiji_core::backend::git_root_for(Path::new(&path)).ok_or_else(|| {
        CommandError::new("not_a_git_repo", format!("{path} is not inside a Git repository"))
    })?;
    state.backend.init_colocated(&root)?;
    Ok(root.to_string_lossy().into_owned())
}

/// Close a repo's session (its tab closed): the watcher stops and the
/// cached snapshot drops. A live auto-land job pinned to this repo is
/// detached first — its record survives as interrupted, so reopening the
/// repo offers Resume.
#[tauri::command]
pub fn close_repo(app: AppHandle, state: State<'_, AppState>, repo_path: String) {
    app.state::<crate::autoland::AutoLandHost>()
        .detach_for(&repo_path);
    state.sessions().remove(&repo_path);
}

#[tauri::command(async)]
pub fn refresh_snapshot(
    app: AppHandle,
    state: State<'_, AppState>,
    repo_path: String,
) -> Result<RepoSnapshot, CommandError> {
    state.require_open(&repo_path)?;
    state.sync_and_publish(&app, Path::new(&repo_path))
}

/// Switch the view lens snapshots render, refreshing and publishing under
/// the new one immediately. A custom revset that fails to parse or resolve
/// refuses with `revset_invalid` and leaves both the stored view and the
/// published snapshot untouched — the UI shows the error inline and the
/// graph keeps rendering what it was.
#[tauri::command(async)]
pub fn set_snapshot_view(
    app: AppHandle,
    state: State<'_, AppState>,
    repo_path: String,
    view: SnapshotView,
) -> Result<RepoSnapshot, CommandError> {
    state.require_open(&repo_path)?;
    let snapshot = state.backend.refresh_with(Path::new(&repo_path), &view)?;
    if let Some(session) = state.sessions().get_mut(&repo_path) {
        session.view = view;
    }
    state.publish(&app, &snapshot);
    Ok(snapshot)
}

/// The latest published snapshot for one open repo — how a reloading
/// frontend recovers a tab without a full re-sync.
#[tauri::command(async)]
pub fn current_snapshot(state: State<'_, AppState>, repo_path: String) -> Option<RepoSnapshot> {
    state.snapshot_for(&repo_path)
}

#[cfg(test)]
mod publish_tests {
    use super::*;

    #[test]
    fn publishing_reads_full_stacks_without_changing_the_graph_lens() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join(".jj")).unwrap();
        let backend = MockBackend::default();
        let full = backend.open(dir.path()).unwrap();
        let path = full.repo_path.clone();
        let mut filtered = full.clone();
        filtered.view = SnapshotView::Trunk;
        filtered
            .nodes
            .retain(|node| node.kind == jiji_core::snapshot::NodeKind::Immutable);
        let state = AppState {
            backend: Box::new(backend),
            sessions: Mutex::new(HashMap::from([(
                path.clone(),
                RepoSession {
                    snapshot: filtered.clone(),
                    view: SnapshotView::Trunk,
                    _watcher: None,
                },
            )])),
        };
        let workflow = state.workflow_snapshot(&path).unwrap();
        assert_eq!(workflow.nodes, full.nodes);
        assert_eq!(state.snapshot_for(&path).unwrap(), filtered);
        assert!(state.workflow_snapshot("/closed/repo").is_err());
    }
}

#[cfg(test)]
mod open_path_tests {
    use super::*;

    fn display(path: &Path) -> String {
        path.to_string_lossy().into_owned()
    }

    #[test]
    fn opening_inside_a_repo_opens_its_root() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        std::fs::create_dir(root.join(".jj")).unwrap();
        let nested = root.join("src/lib");
        std::fs::create_dir_all(&nested).unwrap();
        let file = nested.join("api.ts");
        std::fs::write(&file, "").unwrap();

        assert_eq!(resolve_open_path(display(&root)), display(&root));
        assert_eq!(resolve_open_path(display(&nested)), display(&root));
        assert_eq!(resolve_open_path(display(&file)), display(&root));
    }

    #[test]
    fn paths_outside_any_repo_or_missing_pass_through() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let plain = root.join("plain");
        std::fs::create_dir(&plain).unwrap();
        assert_eq!(resolve_open_path(display(&plain)), display(&plain));

        // A deleted repo that sat inside another one must not open the
        // outer repo in its place.
        std::fs::create_dir(root.join(".jj")).unwrap();
        let gone = root.join("deleted-repo");
        assert_eq!(resolve_open_path(display(&gone)), display(&gone));
    }
}

impl AppState {
    /// Git remotes of an open repo's latest snapshot; empty when it is not
    /// open. What forge detection reads.
    pub(crate) fn current_git_remotes(&self, path: &str) -> Vec<jiji_core::snapshot::GitRemote> {
        self.sessions()
            .get(path)
            .map(|s| s.snapshot.git_remotes.clone())
            .unwrap_or_default()
    }

    /// The latest published snapshot of one repo — what the user currently
    /// sees in its tab, which is what submit plans must be derived from.
    pub(crate) fn snapshot_for(&self, path: &str) -> Option<RepoSnapshot> {
        self.sessions().get(path).map(|s| s.snapshot.clone())
    }

    /// Publishing walks complete local stacks regardless of the graph's
    /// selected revset. Reading this never changes that view or the repo.
    pub(crate) fn workflow_snapshot(&self, path: &str) -> Result<RepoSnapshot, BackendError> {
        let snapshot = self.snapshot_for(path).ok_or_else(|| {
            BackendError::MutationFailed("This repository is not open".into())
        })?;
        if matches!(
            snapshot.view,
            SnapshotView::Workstreams | SnapshotView::Team | SnapshotView::Recent
        ) {
            Ok(snapshot)
        } else {
            self.backend.open(Path::new(path))
        }
    }

    /// Refuse repo-scoped work on a repo that is not open. Mutations do
    /// not strictly need the session (the backend opens by path), but a
    /// command arriving for a closed tab is a routing bug — refusing keeps
    /// it visible.
    pub(crate) fn require_open(&self, path: &str) -> Result<(), CommandError> {
        if self.sessions().contains_key(path) {
            Ok(())
        } else {
            Err(CommandError::new(
                "no_repo_open",
                "This repository is not open",
            ))
        }
    }

    /// The first of `candidates` existing as a text file in the trunk
    /// tree — how forge planning reads the repo's PR template. A failed
    /// read only loses that nicety, so it logs instead of erroring.
    pub(crate) fn trunk_text_file(
        &self,
        path: &str,
        candidates: &[String],
    ) -> Option<(String, String)> {
        self.backend
            .trunk_text_file(Path::new(path), candidates)
            .unwrap_or_else(|err| {
                log::warn!("could not probe trunk for a PR template: {err}");
                None
            })
    }

    /// Push bookmarks through the backend and republish the refreshed
    /// snapshot, like every other mutation — the submit executor's jj half.
    pub(crate) fn push_and_publish(
        &self,
        app: &AppHandle,
        path: &str,
        names: &[String],
        remote: &str,
    ) -> Result<MutationOutcome, CommandError> {
        self.mutate(app, path, |backend, path| {
            backend.push_bookmarks(path, names, Some(remote))
        })
    }
}

// The per-selection reads below are `(async)` so they run on the blocking
// pool instead of the main thread: walking the graph fires one diff fetch
// per selection, and serializing those whole-repo loads on the main thread
// let stale fetches queue ahead of the newest one (seconds of skeleton on
// big changes) while also freezing the rest of the UI.
#[tauri::command(async)]
pub fn change_detail(
    state: State<'_, AppState>,
    repo_path: String,
    change_id: String,
) -> Result<ChangeDetail, CommandError> {
    state
        .backend
        .change_detail(Path::new(&repo_path), &change_id)
        .map_err(Into::into)
}

/// Heavier sibling of `change_detail`: materializes file contents to build
/// the renderable hunks for the diff surface.
#[tauri::command(async)]
pub fn change_diff(
    state: State<'_, AppState>,
    repo_path: String,
    change_id: String,
) -> Result<ChangeDiff, CommandError> {
    state
        .backend
        .change_diff(Path::new(&repo_path), &change_id)
        .map_err(Into::into)
}

/// One change's evolution (`jj evolog`): its predecessor versions with the
/// operations that recorded them. `(async)` like the other per-selection
/// reads — the walk loads operations and commits.
#[tauri::command(async)]
pub fn change_evolog(
    state: State<'_, AppState>,
    repo_path: String,
    change_id: String,
) -> Result<ChangeEvolog, CommandError> {
    state
        .backend
        .change_evolog(Path::new(&repo_path), &change_id)
        .map_err(Into::into)
}

/// One file annotated line by line (`jj file annotate`): which change
/// introduced each line as the file stands in this change's tree.
/// `(async)` like the other per-selection reads — the attribution walk
/// loads every commit that touched the file.
#[tauri::command(async)]
pub fn file_annotate(
    state: State<'_, AppState>,
    repo_path: String,
    change_id: String,
    file_path: String,
) -> Result<FileAnnotation, CommandError> {
    state
        .backend
        .file_annotate(Path::new(&repo_path), &change_id, &file_path)
        .map_err(Into::into)
}

/// History content/filename search (`jj log -S` semantics): which changes
/// introduced or removed the query string, or touched a path containing
/// it. `(async)` like the other whole-repo reads — a search walks and
/// diffs many commits.
#[tauri::command(async)]
pub fn search_history(
    state: State<'_, AppState>,
    repo_path: String,
    query: String,
) -> Result<HistorySearch, CommandError> {
    state
        .backend
        .search_history(Path::new(&repo_path), &query)
        .map_err(Into::into)
}

/// Commit-to-commit comparison (`jj diff --from --to`): the diff surface's
/// stack-relative and arbitrary-revision views.
#[tauri::command(async)]
pub fn compare_diff(
    state: State<'_, AppState>,
    repo_path: String,
    from_change_id: String,
    to_change_id: String,
) -> Result<ChangeDiff, CommandError> {
    state
        .backend
        .compare_diff(Path::new(&repo_path), &from_change_id, &to_change_id)
        .map_err(Into::into)
}

impl AppState {
    /// The shape every write action shares: run the backend mutation
    /// against its repo, refresh the snapshot (published to all surfaces),
    /// and return the operation breadcrumb. The path is pinned by the
    /// caller, so a mutation in flight when another tab takes the screen
    /// still lands in its own repo. `pub(crate)` so the forge module's
    /// executors can drive their jj half through the same path.
    pub(crate) fn mutate(
        &self,
        app: &AppHandle,
        path: &str,
        action: impl FnOnce(&dyn RepoBackend, &Path) -> Result<MutationOutcome, BackendError>,
    ) -> Result<MutationOutcome, CommandError> {
        let outcome = action(self.backend.as_ref(), Path::new(path))?;
        self.load_and_publish(app, Path::new(path))?;
        Ok(outcome)
    }
}

#[tauri::command]
pub fn describe_change(
    app: AppHandle,
    state: State<'_, AppState>,
    repo_path: String,
    change_id: String,
    description: String,
) -> Result<MutationOutcome, CommandError> {
    state.mutate(&app, &repo_path, |backend, path| {
        backend.describe(path, &change_id, &description)
    })
}

#[tauri::command]
pub fn new_change(
    app: AppHandle,
    state: State<'_, AppState>,
    repo_path: String,
    parent_change_id: String,
) -> Result<MutationOutcome, CommandError> {
    state.mutate(&app, &repo_path, |backend, path| {
        backend.new_change(path, &parent_change_id)
    })
}

#[tauri::command]
pub fn edit_change(
    app: AppHandle,
    state: State<'_, AppState>,
    repo_path: String,
    change_id: String,
) -> Result<MutationOutcome, CommandError> {
    state.mutate(&app, &repo_path, |backend, path| {
        backend.edit_change(path, &change_id)
    })
}

#[tauri::command]
pub fn abandon_change(
    app: AppHandle,
    state: State<'_, AppState>,
    repo_path: String,
    change_id: String,
) -> Result<MutationOutcome, CommandError> {
    state.mutate(&app, &repo_path, |backend, path| {
        backend.abandon_change(path, &change_id)
    })
}

#[tauri::command]
pub fn squash_change(
    app: AppHandle,
    state: State<'_, AppState>,
    repo_path: String,
    change_id: String,
) -> Result<MutationOutcome, CommandError> {
    state.mutate(&app, &repo_path, |backend, path| {
        backend.squash_change(path, &change_id)
    })
}

#[tauri::command]
pub fn restore_file(
    app: AppHandle,
    state: State<'_, AppState>,
    repo_path: String,
    change_id: String,
    file_path: String,
    hunks: Option<Vec<SplitHunk>>,
) -> Result<MutationOutcome, CommandError> {
    state.mutate(&app, &repo_path, |backend, path| {
        backend.restore_file(path, &change_id, &file_path, hunks.as_deref())
    })
}

/// Restore a change to one of its earlier versions (tree and description;
/// the change does not move). The version must come from the change's own
/// predecessor chain — the evolution panel's rows.
#[tauri::command]
pub fn restore_change_version(
    app: AppHandle,
    state: State<'_, AppState>,
    repo_path: String,
    change_id: String,
    commit_id: String,
) -> Result<MutationOutcome, CommandError> {
    state.mutate(&app, &repo_path, |backend, path| {
        backend.restore_change_version(path, &change_id, &commit_id)
    })
}

#[tauri::command]
pub fn duplicate_change(
    app: AppHandle,
    state: State<'_, AppState>,
    repo_path: String,
    change_id: String,
) -> Result<MutationOutcome, CommandError> {
    state.mutate(&app, &repo_path, |backend, path| {
        backend.duplicate_change(path, &change_id)
    })
}

#[tauri::command]
pub fn revert_change(
    app: AppHandle,
    state: State<'_, AppState>,
    repo_path: String,
    change_id: String,
) -> Result<MutationOutcome, CommandError> {
    state.mutate(&app, &repo_path, |backend, path| {
        backend.revert_change(path, &change_id)
    })
}

#[tauri::command]
pub fn absorb_change(
    app: AppHandle,
    state: State<'_, AppState>,
    repo_path: String,
    change_id: String,
) -> Result<MutationOutcome, CommandError> {
    state.mutate(&app, &repo_path, |backend, path| {
        backend.absorb_change(path, &change_id)
    })
}

#[tauri::command]
pub fn split_change(
    app: AppHandle,
    state: State<'_, AppState>,
    repo_path: String,
    change_id: String,
    selection: Vec<SplitSelection>,
    description: String,
) -> Result<MutationOutcome, CommandError> {
    state.mutate(&app, &repo_path, |backend, path| {
        backend.split_change(path, &change_id, &selection, &description)
    })
}

#[tauri::command]
pub fn squash_into(
    app: AppHandle,
    state: State<'_, AppState>,
    repo_path: String,
    change_id: String,
    selection: Vec<SplitSelection>,
    destination_id: String,
) -> Result<MutationOutcome, CommandError> {
    state.mutate(&app, &repo_path, |backend, path| {
        backend.squash_into(path, &change_id, &selection, &destination_id)
    })
}

#[tauri::command]
pub fn rebase_change(
    app: AppHandle,
    state: State<'_, AppState>,
    repo_path: String,
    change_id: String,
    destination_id: String,
) -> Result<MutationOutcome, CommandError> {
    state.mutate(&app, &repo_path, |backend, path| {
        backend.rebase_change(path, &change_id, &destination_id)
    })
}

#[tauri::command]
pub fn move_change(
    app: AppHandle,
    state: State<'_, AppState>,
    repo_path: String,
    change_id: String,
    destination_id: String,
) -> Result<MutationOutcome, CommandError> {
    state.mutate(&app, &repo_path, |backend, path| {
        backend.move_change(path, &change_id, &destination_id)
    })
}

#[tauri::command]
pub fn create_bookmark(
    app: AppHandle,
    state: State<'_, AppState>,
    repo_path: String,
    name: String,
    change_id: String,
) -> Result<MutationOutcome, CommandError> {
    state.mutate(&app, &repo_path, |backend, path| {
        backend.create_bookmark(path, &name, &change_id)
    })
}

#[tauri::command]
pub fn move_bookmark(
    app: AppHandle,
    state: State<'_, AppState>,
    repo_path: String,
    name: String,
    change_id: String,
) -> Result<MutationOutcome, CommandError> {
    state.mutate(&app, &repo_path, |backend, path| {
        backend.move_bookmark(path, &name, &change_id)
    })
}

#[tauri::command]
pub fn rename_bookmark(
    app: AppHandle,
    state: State<'_, AppState>,
    repo_path: String,
    old_name: String,
    new_name: String,
) -> Result<MutationOutcome, CommandError> {
    state.mutate(&app, &repo_path, |backend, path| {
        backend.rename_bookmark(path, &old_name, &new_name)
    })
}

#[tauri::command]
pub fn delete_bookmark(
    app: AppHandle,
    state: State<'_, AppState>,
    repo_path: String,
    name: String,
) -> Result<MutationOutcome, CommandError> {
    state.mutate(&app, &repo_path, |backend, path| {
        backend.delete_bookmark(path, &name)
    })
}

#[tauri::command]
pub fn track_bookmark(
    app: AppHandle,
    state: State<'_, AppState>,
    repo_path: String,
    name: String,
    remote: String,
) -> Result<MutationOutcome, CommandError> {
    state.mutate(&app, &repo_path, |backend, path| {
        backend.track_bookmark(path, &name, &remote)
    })
}

#[tauri::command]
pub fn untrack_bookmark(
    app: AppHandle,
    state: State<'_, AppState>,
    repo_path: String,
    name: String,
    remote: String,
) -> Result<MutationOutcome, CommandError> {
    state.mutate(&app, &repo_path, |backend, path| {
        backend.untrack_bookmark(path, &name, &remote)
    })
}

/// Push bookmarks to a remote (`None` resolves like the CLI: the
/// `git.push` setting, else the sole remote, else "origin"). `async` for
/// the same reason as `git_fetch`: a push blocks on the network, and on the
/// main thread that froze the whole window until the remote answered.
#[tauri::command(async)]
pub fn push_bookmarks(
    app: AppHandle,
    state: State<'_, AppState>,
    repo_path: String,
    names: Vec<String>,
    remote: Option<String>,
) -> Result<MutationOutcome, CommandError> {
    state.mutate(&app, &repo_path, |backend, path| {
        backend.push_bookmarks(path, &names, remote.as_deref())
    })
}

/// Delete a bookmark on a remote: forget it locally, then push the
/// deletion. `async` because that push blocks on the network.
#[tauri::command(async)]
pub fn delete_remote_bookmark(
    app: AppHandle,
    state: State<'_, AppState>,
    repo_path: String,
    name: String,
    remote: String,
) -> Result<MutationOutcome, CommandError> {
    state.mutate(&app, &repo_path, |backend, path| {
        backend.delete_remote_bookmark(path, &name, &remote)
    })
}

#[tauri::command]
pub fn revert_operation(
    app: AppHandle,
    state: State<'_, AppState>,
    repo_path: String,
    op_id: String,
) -> Result<MutationOutcome, CommandError> {
    state.mutate(&app, &repo_path, |backend, path| {
        backend.revert_operation(path, &op_id)
    })
}

#[tauri::command]
pub fn restore_operation(
    app: AppHandle,
    state: State<'_, AppState>,
    repo_path: String,
    op_id: String,
) -> Result<MutationOutcome, CommandError> {
    state.mutate(&app, &repo_path, |backend, path| {
        backend.restore_operation(path, &op_id)
    })
}

/// Launches the external merge tool and blocks until its window closes —
/// minutes, not milliseconds. `async` moves it off the main thread so the
/// UI keeps rendering (and auto-refresh keeps running) while the merge
/// window is open; local-only mutations stay main-thread like before.
#[tauri::command(async)]
pub fn resolve_conflict(
    app: AppHandle,
    state: State<'_, AppState>,
    repo_path: String,
    change_id: String,
    file_path: String,
) -> Result<MutationOutcome, CommandError> {
    state.mutate(&app, &repo_path, |backend, path| {
        backend.resolve_conflict(path, &change_id, &file_path)
    })
}

/// Resolves one conflicted file by keeping one side wholesale (the CLI's
/// built-in `:ours`/`:theirs` merge tools). No external tool launches, so
/// this stays a plain main-thread mutation.
#[tauri::command]
pub fn resolve_conflict_take(
    app: AppHandle,
    state: State<'_, AppState>,
    repo_path: String,
    change_id: String,
    file_path: String,
    side: ConflictSide,
) -> Result<MutationOutcome, CommandError> {
    state.mutate(&app, &repo_path, |backend, path| {
        backend.resolve_conflict_take(path, &change_id, &file_path, side)
    })
}

/// Fetch from the repo's git remotes (`jj git fetch` with the CLI's
/// default remote resolution) — the upstream check behind the shell's
/// fetch affordance and the background cadence. `async` moves it off the
/// main thread: a fetch blocks on the network for as long as the remote
/// takes to answer.
#[tauri::command(async)]
pub fn git_fetch(
    app: AppHandle,
    state: State<'_, AppState>,
    repo_path: String,
) -> Result<MutationOutcome, CommandError> {
    state.mutate(&app, &repo_path, |backend, path| {
        backend.git_fetch(path, None)
    })
}

/// Fetch a pull request's head (`refs/pull/N/head` via git against the
/// repo's git store — jj cannot fetch non-branch refs, jj-vcs/jj#4388)
/// into a new local bookmark for local review. `async` because it blocks
/// on the network like any fetch.
#[tauri::command(async)]
pub fn fetch_pr(
    app: AppHandle,
    state: State<'_, AppState>,
    repo_path: String,
    remote: String,
    number: u64,
    bookmark: String,
) -> Result<MutationOutcome, CommandError> {
    state.mutate(&app, &repo_path, |backend, path| {
        backend.fetch_pr_head(path, &remote, number, &bookmark)
    })
}

/// The inbox's stale-workspace recovery (`jj workspace update-stale`): the
/// one mutation allowed while the working copy is stale.
#[tauri::command]
pub fn update_stale_workspace(
    app: AppHandle,
    state: State<'_, AppState>,
    repo_path: String,
) -> Result<MutationOutcome, CommandError> {
    state.mutate(&app, &repo_path, |backend, path| {
        backend.update_stale_workspace(path)
    })
}

/// Keep the native window background in lockstep with the active theme.
///
/// During a live resize the OS fills newly exposed pixels with the native
/// window's background color; the webview compositor only catches up a few
/// frames later. The webview draws no background of its own (see the
/// `macos-private-api` note in the workspace Cargo.toml), so this color is
/// what shows in that gap — it must match the page or resizing flashes.
/// Called by the theme store on boot and on every theme change.
#[tauri::command]
pub fn set_background_color(
    window: tauri::WebviewWindow,
    color: tauri::utils::config::Color,
) -> Result<(), CommandError> {
    window
        .set_background_color(Some(color))
        .map_err(|err| CommandError::new("window_background", err.to_string()))
}
