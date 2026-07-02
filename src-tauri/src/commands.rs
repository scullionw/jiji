//! UI-facing command surface.
//!
//! Commands stay thin: validate input, call the `RepoBackend` boundary in
//! `jiji-core`, store the latest snapshot, and broadcast it as an event so
//! every surface refreshes from the same immutable state.

use std::path::Path;
use std::sync::Mutex;

use jiji_core::snapshot::{ChangeDetail, ChangeDiff, MutationOutcome, SplitSelection};
use jiji_core::{BackendError, JjBackend, MockBackend, RepoBackend, RepoSnapshot, RepoWatcher};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager as _, State};

pub const SNAPSHOT_UPDATED_EVENT: &str = "snapshot://updated";

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandError {
    pub code: String,
    pub message: String,
}

impl CommandError {
    pub(crate) fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }
}

impl From<BackendError> for CommandError {
    fn from(err: BackendError) -> Self {
        Self::new(err.code(), err.to_string())
    }
}

pub struct AppState {
    backend: Box<dyn RepoBackend>,
    current: Mutex<Option<RepoSnapshot>>,
    /// Watches the open repo for out-of-band changes (file edits, CLI
    /// operations, git ref moves); replaced when another repo opens.
    watcher: Mutex<Option<RepoWatcher>>,
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
            current: Mutex::new(None),
            watcher: Mutex::new(None),
        }
    }

    fn publish(&self, app: &AppHandle, snapshot: &RepoSnapshot) {
        *self.current.lock().expect("snapshot state lock poisoned") = Some(snapshot.clone());
        if let Err(err) = app.emit(SNAPSHOT_UPDATED_EVENT, snapshot) {
            log::warn!("failed to emit snapshot update: {err}");
        }
    }

    /// Post-mutation reload: the mutation already synced the working copy
    /// and git state, so this only reads and republishes.
    fn load_and_publish(&self, app: &AppHandle, path: &Path) -> Result<RepoSnapshot, CommandError> {
        let snapshot = self.backend.open(path)?;
        self.publish(app, &snapshot);
        Ok(snapshot)
    }

    /// Open/refresh: bring the repo up to date like running a jj command
    /// would (snapshot working-copy edits, import externally-moved git
    /// state), then publish.
    fn sync_and_publish(&self, app: &AppHandle, path: &Path) -> Result<RepoSnapshot, CommandError> {
        let snapshot = self.backend.refresh(path)?;
        self.publish(app, &snapshot);
        Ok(snapshot)
    }

    /// Watch the repo so edits, CLI commands, and git ref moves refresh the
    /// snapshot without ⌘R. Failing to watch only loses the automation, so
    /// it logs instead of failing the open.
    fn start_watching(&self, app: &AppHandle, path: &Path) {
        let handle = app.clone();
        let watcher = self
            .backend
            .watch(path, Box::new(move || auto_refresh(&handle)))
            .unwrap_or_else(|err| {
                log::warn!("repo watching unavailable: {err}");
                None
            });
        *self.watcher.lock().expect("watcher state lock poisoned") = watcher;
    }
}

/// One watcher tick: refresh and publish only when something actually
/// changed — republishing an identical snapshot would make every surface
/// rerender and the diff refetch for nothing. Runs on the watcher thread;
/// concurrent jj access from commands is safe the same way two CLI
/// processes are (op-store and working-copy locks).
fn auto_refresh(app: &AppHandle) {
    let state: State<'_, AppState> = app.state();
    let Ok(path) = state.open_repo_path() else {
        return;
    };
    match state.backend.refresh(Path::new(&path)) {
        Ok(snapshot) => {
            {
                let current = state.current.lock().expect("snapshot state lock poisoned");
                if current.as_ref() == Some(&snapshot) {
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

#[tauri::command]
pub fn open_repo(
    app: AppHandle,
    state: State<'_, AppState>,
    path: String,
) -> Result<RepoSnapshot, CommandError> {
    let snapshot = state.sync_and_publish(&app, Path::new(&path))?;
    state.start_watching(&app, Path::new(&path));
    Ok(snapshot)
}

#[tauri::command]
pub fn refresh_snapshot(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<RepoSnapshot, CommandError> {
    let repo_path = state
        .current
        .lock()
        .expect("snapshot state lock poisoned")
        .as_ref()
        .map(|s| s.repo_path.clone());
    match repo_path {
        Some(path) => state.sync_and_publish(&app, Path::new(&path)),
        None => Err(CommandError::new(
            "no_repo_open",
            "No repository is currently open",
        )),
    }
}

#[tauri::command]
pub fn current_snapshot(state: State<'_, AppState>) -> Option<RepoSnapshot> {
    state
        .current
        .lock()
        .expect("snapshot state lock poisoned")
        .clone()
}

impl AppState {
    /// Git remotes of the open repo's latest snapshot; empty when no repo
    /// is open. What forge detection reads.
    pub(crate) fn current_git_remotes(&self) -> Vec<jiji_core::snapshot::GitRemote> {
        self.current
            .lock()
            .expect("snapshot state lock poisoned")
            .as_ref()
            .map(|s| s.git_remotes.clone())
            .unwrap_or_default()
    }

    fn open_repo_path(&self) -> Result<String, CommandError> {
        self.current
            .lock()
            .expect("snapshot state lock poisoned")
            .as_ref()
            .map(|s| s.repo_path.clone())
            .ok_or_else(|| CommandError::new("no_repo_open", "No repository is currently open"))
    }
}

#[tauri::command]
pub fn change_detail(
    state: State<'_, AppState>,
    change_id: String,
) -> Result<ChangeDetail, CommandError> {
    let repo_path = state.open_repo_path()?;
    state
        .backend
        .change_detail(Path::new(&repo_path), &change_id)
        .map_err(Into::into)
}

/// Heavier sibling of `change_detail`: materializes file contents to build
/// the renderable hunks for the diff surface.
#[tauri::command]
pub fn change_diff(
    state: State<'_, AppState>,
    change_id: String,
) -> Result<ChangeDiff, CommandError> {
    let repo_path = state.open_repo_path()?;
    state
        .backend
        .change_diff(Path::new(&repo_path), &change_id)
        .map_err(Into::into)
}

/// Commit-to-commit comparison (`jj diff --from --to`): the diff surface's
/// stack-relative and arbitrary-revision views.
#[tauri::command]
pub fn compare_diff(
    state: State<'_, AppState>,
    from_change_id: String,
    to_change_id: String,
) -> Result<ChangeDiff, CommandError> {
    let repo_path = state.open_repo_path()?;
    state
        .backend
        .compare_diff(Path::new(&repo_path), &from_change_id, &to_change_id)
        .map_err(Into::into)
}

impl AppState {
    /// The shape every write action shares: run the backend mutation,
    /// refresh the snapshot (published to all surfaces), and return the
    /// operation breadcrumb.
    fn mutate(
        &self,
        app: &AppHandle,
        action: impl FnOnce(&dyn RepoBackend, &Path) -> Result<MutationOutcome, BackendError>,
    ) -> Result<MutationOutcome, CommandError> {
        let repo_path = self.open_repo_path()?;
        let outcome = action(self.backend.as_ref(), Path::new(&repo_path))?;
        self.load_and_publish(app, Path::new(&repo_path))?;
        Ok(outcome)
    }
}

#[tauri::command]
pub fn describe_change(
    app: AppHandle,
    state: State<'_, AppState>,
    change_id: String,
    description: String,
) -> Result<MutationOutcome, CommandError> {
    state.mutate(&app, |backend, path| {
        backend.describe(path, &change_id, &description)
    })
}

#[tauri::command]
pub fn new_change(
    app: AppHandle,
    state: State<'_, AppState>,
    parent_change_id: String,
) -> Result<MutationOutcome, CommandError> {
    state.mutate(&app, |backend, path| {
        backend.new_change(path, &parent_change_id)
    })
}

#[tauri::command]
pub fn edit_change(
    app: AppHandle,
    state: State<'_, AppState>,
    change_id: String,
) -> Result<MutationOutcome, CommandError> {
    state.mutate(&app, |backend, path| backend.edit_change(path, &change_id))
}

#[tauri::command]
pub fn abandon_change(
    app: AppHandle,
    state: State<'_, AppState>,
    change_id: String,
) -> Result<MutationOutcome, CommandError> {
    state.mutate(&app, |backend, path| {
        backend.abandon_change(path, &change_id)
    })
}

#[tauri::command]
pub fn squash_change(
    app: AppHandle,
    state: State<'_, AppState>,
    change_id: String,
) -> Result<MutationOutcome, CommandError> {
    state.mutate(&app, |backend, path| {
        backend.squash_change(path, &change_id)
    })
}

#[tauri::command]
pub fn split_change(
    app: AppHandle,
    state: State<'_, AppState>,
    change_id: String,
    selection: Vec<SplitSelection>,
    description: String,
) -> Result<MutationOutcome, CommandError> {
    state.mutate(&app, |backend, path| {
        backend.split_change(path, &change_id, &selection, &description)
    })
}

#[tauri::command]
pub fn squash_into(
    app: AppHandle,
    state: State<'_, AppState>,
    change_id: String,
    selection: Vec<SplitSelection>,
    destination_id: String,
) -> Result<MutationOutcome, CommandError> {
    state.mutate(&app, |backend, path| {
        backend.squash_into(path, &change_id, &selection, &destination_id)
    })
}

#[tauri::command]
pub fn rebase_change(
    app: AppHandle,
    state: State<'_, AppState>,
    change_id: String,
    destination_id: String,
) -> Result<MutationOutcome, CommandError> {
    state.mutate(&app, |backend, path| {
        backend.rebase_change(path, &change_id, &destination_id)
    })
}

#[tauri::command]
pub fn move_change(
    app: AppHandle,
    state: State<'_, AppState>,
    change_id: String,
    destination_id: String,
) -> Result<MutationOutcome, CommandError> {
    state.mutate(&app, |backend, path| {
        backend.move_change(path, &change_id, &destination_id)
    })
}

#[tauri::command]
pub fn create_bookmark(
    app: AppHandle,
    state: State<'_, AppState>,
    name: String,
    change_id: String,
) -> Result<MutationOutcome, CommandError> {
    state.mutate(&app, |backend, path| {
        backend.create_bookmark(path, &name, &change_id)
    })
}

#[tauri::command]
pub fn move_bookmark(
    app: AppHandle,
    state: State<'_, AppState>,
    name: String,
    change_id: String,
) -> Result<MutationOutcome, CommandError> {
    state.mutate(&app, |backend, path| {
        backend.move_bookmark(path, &name, &change_id)
    })
}

#[tauri::command]
pub fn rename_bookmark(
    app: AppHandle,
    state: State<'_, AppState>,
    old_name: String,
    new_name: String,
) -> Result<MutationOutcome, CommandError> {
    state.mutate(&app, |backend, path| {
        backend.rename_bookmark(path, &old_name, &new_name)
    })
}

#[tauri::command]
pub fn delete_bookmark(
    app: AppHandle,
    state: State<'_, AppState>,
    name: String,
) -> Result<MutationOutcome, CommandError> {
    state.mutate(&app, |backend, path| backend.delete_bookmark(path, &name))
}

#[tauri::command]
pub fn revert_operation(
    app: AppHandle,
    state: State<'_, AppState>,
    op_id: String,
) -> Result<MutationOutcome, CommandError> {
    state.mutate(&app, |backend, path| backend.revert_operation(path, &op_id))
}

#[tauri::command]
pub fn restore_operation(
    app: AppHandle,
    state: State<'_, AppState>,
    op_id: String,
) -> Result<MutationOutcome, CommandError> {
    state.mutate(&app, |backend, path| {
        backend.restore_operation(path, &op_id)
    })
}

/// Launches the external merge tool and blocks until its window closes —
/// minutes, not milliseconds. `async` moves it off the main thread so the
/// UI keeps rendering (and auto-refresh keeps running) while the merge
/// window is open; every other command stays main-thread like before.
#[tauri::command(async)]
pub fn resolve_conflict(
    app: AppHandle,
    state: State<'_, AppState>,
    change_id: String,
    file_path: String,
) -> Result<MutationOutcome, CommandError> {
    state.mutate(&app, |backend, path| {
        backend.resolve_conflict(path, &change_id, &file_path)
    })
}

/// The inbox's stale-workspace recovery (`jj workspace update-stale`): the
/// one mutation allowed while the working copy is stale.
#[tauri::command]
pub fn update_stale_workspace(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<MutationOutcome, CommandError> {
    state.mutate(&app, |backend, path| backend.update_stale_workspace(path))
}
