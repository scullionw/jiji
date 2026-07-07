//! Auto-land job hosting: the desktop app as one host for `jiji-forge`'s
//! supervised watch loop.
//!
//! The engine (`jiji_forge::run_autoland`) owns the state machine; this
//! module owns what a host owes it — a thread, the stop signal, a place
//! to publish state, and now where the persisted job record lives. One
//! job at a time for now (multi-job wants richer bookkeeping): starting a
//! second is refused while one is alive, and switching repos stops the
//! job (the engine's own repo-path pin is the backstop if a round is
//! mid-flight).
//!
//! Job state reaches the shell two ways, mirroring snapshots: every phase
//! change is emitted as the `autoland://state` event, and `autoland_state`
//! answers the latest record whole so a reloading frontend can reattach.
//! The record also persists to one JSON file in the app data dir
//! (`jiji-forge` owns the format and the atomic write; this host owns the
//! path and when to save): saved on every publish, loaded back at launch,
//! cleared on dismiss — so a waiting or parked job survives an app
//! restart as resumable status. A record no thread is driving answers as
//! `live: false`, which the UI renders as "interrupted — resume?"; Resume
//! is a plain re-queue, and `autoland_start` seeds the engine from a
//! matching record so the story (rounds, landed PRs) continues.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use jiji_forge::{
    clear_autoland_record, load_autoland_record, no_github_remote, resolve_token, run_autoland,
    save_autoland_record, unix_now_ms, AutoLandConfig, AutoLandRecord, AutoLandState,
    AutoLandStatus, ForgeError, GitHubClient, LandRepoForge, StopSignal, AUTOLAND_RECORD_VERSION,
};
use tauri::{AppHandle, Emitter, Manager as _, Runtime};

use crate::commands::{AppState, CommandError};
use crate::forge::{detected_repo, token_store, HostVcs};

pub const AUTOLAND_EVENT: &str = "autoland://state";

/// Where the one-job record lives: the app data dir, resolved per call
/// (it cannot fail after setup, but a missing dir must never take the
/// job down — persistence degrades, the in-memory mirror still serves).
fn record_path<R: Runtime>(manager: &impl tauri::Manager<R>) -> Option<PathBuf> {
    match manager.path().app_data_dir() {
        Ok(dir) => Some(dir.join("autoland.json")),
        Err(err) => {
            log::warn!("no app data dir for the auto-land record: {err}");
            None
        }
    }
}

/// The active job's handles.
struct Job {
    stop: Arc<StopSignal>,
    thread: JoinHandle<()>,
}

pub struct AutoLandHost {
    job: Mutex<Option<Job>>,
    /// The one job record — the live thread's latest publish, or the
    /// record loaded from disk at launch. `None` until a job runs or a
    /// record loads, and again after dismiss.
    record: Arc<Mutex<Option<AutoLandRecord>>>,
}

impl AutoLandHost {
    pub fn new() -> Self {
        Self {
            job: Mutex::new(None),
            record: Arc::new(Mutex::new(None)),
        }
    }

    /// Load the persisted record at launch, if one survived. Called once
    /// from the app's setup hook.
    pub fn load_persisted<R: Runtime>(&self, manager: &impl tauri::Manager<R>) {
        if let Some(path) = record_path(manager) {
            if let Some(record) = load_autoland_record(&path) {
                *self.record.lock().expect("auto-land record lock poisoned") = Some(record);
            }
        }
    }

    /// Signal the active job to stop. Returns without waiting: a sleeping
    /// job wakes immediately, a mid-round job finishes its round first —
    /// mutations are never interrupted partway.
    pub fn stop_active(&self) {
        if let Some(job) = self
            .job
            .lock()
            .expect("auto-land job lock poisoned")
            .as_ref()
        {
            job.stop.stop();
        }
    }

    /// Whether a job thread is currently running.
    fn job_alive(&self) -> bool {
        self.job
            .lock()
            .expect("auto-land job lock poisoned")
            .as_ref()
            .is_some_and(|job| !job.thread.is_finished())
    }

    /// The record as a surface sees it: `live` while a thread drives it.
    /// A non-terminal record with no thread — a restart survivor, or a
    /// job whose thread died — is the "interrupted" state.
    fn status(&self) -> Option<AutoLandStatus> {
        let record = self
            .record
            .lock()
            .expect("auto-land record lock poisoned")
            .clone()?;
        let live = !record.state.phase.is_terminal() && self.job_alive();
        Some(AutoLandStatus { record, live })
    }
}

/// Queue the stack under a bookmark for auto-land: the supervised job
/// keeps re-deriving the landing plan and running rounds as remote
/// conditions allow, until the stack is fully landed. Refusals a plan
/// would make (no GitHub remote, no token) answer here before any thread
/// starts; a job already running is refused — stop it first. When the
/// persisted record matches this repo and bookmark and is not terminal,
/// the job resumes from it — the restart-survivor's Resume and a plain
/// re-queue of the same stack are deliberately the same path.
#[tauri::command(async)]
pub fn autoland_start(
    app: AppHandle,
    state: tauri::State<'_, AppState>,
    host: tauri::State<'_, AutoLandHost>,
    head_bookmark: String,
) -> Result<AutoLandStatus, CommandError> {
    let mut slot = host.job.lock().expect("auto-land job lock poisoned");
    if let Some(job) = slot.as_ref() {
        if !job.thread.is_finished() {
            let running = host
                .record
                .lock()
                .expect("auto-land record lock poisoned")
                .as_ref()
                .map(|r| r.state.head_bookmark.clone())
                .unwrap_or_default();
            return Err(CommandError::new(
                "autoland_running",
                format!(
                    "An auto-land job for \u{201c}{running}\u{201d} is already running — \
                     stop it before queueing another stack"
                ),
            ));
        }
    }
    // Build the forge context up front so connection problems refuse the
    // queueing instead of parking a just-started job.
    let repo_path = state.open_repo_path()?;
    let repo = detected_repo(&state).ok_or_else(no_github_remote)?;
    let resolved = resolve_token(&token_store(Some(&repo)))?.ok_or(ForgeError::NoToken)?;
    let client = GitHubClient::for_repo(&repo, &resolved.token)?;

    // A surviving record for this exact stack seeds the job so its story
    // continues; anything else (another stack, a terminal record) is
    // superseded by the fresh start.
    let prior = host
        .record
        .lock()
        .expect("auto-land record lock poisoned")
        .clone()
        .filter(|record| {
            record.repo_path == repo_path
                && record.state.head_bookmark == head_bookmark
                && !record.state.phase.is_terminal()
        });
    let initial = match prior {
        Some(record) => AutoLandState::resumed(record.state),
        None => AutoLandState::queued(&head_bookmark),
    };

    let stop = Arc::new(StopSignal::new());
    let record = AutoLandRecord {
        version: AUTOLAND_RECORD_VERSION,
        repo_path: repo_path.clone(),
        state: initial.clone(),
        saved_at_ms: unix_now_ms(),
    };
    *host.record.lock().expect("auto-land record lock poisoned") = Some(record.clone());
    if let Some(path) = record_path(&app) {
        if let Err(err) = save_autoland_record(&path, &record) {
            log::warn!("failed to save the auto-land record: {err}");
        }
    }

    let thread = {
        let app = app.clone();
        let stop = Arc::clone(&stop);
        let record_slot = Arc::clone(&host.record);
        std::thread::spawn(move || {
            let state: tauri::State<'_, AppState> = app.state();
            let vcs = HostVcs {
                app: &app,
                state: &state,
            };
            let forge_side = LandRepoForge {
                client: &client,
                repo: &repo,
            };
            let path = record_path(&app);
            let mut publish = |published: &AutoLandState| {
                let record = AutoLandRecord {
                    version: AUTOLAND_RECORD_VERSION,
                    repo_path: repo_path.clone(),
                    state: published.clone(),
                    saved_at_ms: unix_now_ms(),
                };
                let status = AutoLandStatus {
                    record: record.clone(),
                    live: !published.phase.is_terminal(),
                };
                *record_slot.lock().expect("auto-land record lock poisoned") = Some(record.clone());
                if let Some(path) = &path {
                    if let Err(err) = save_autoland_record(path, &record) {
                        log::warn!("failed to save the auto-land record: {err}");
                    }
                }
                if let Err(err) = app.emit(AUTOLAND_EVENT, &status) {
                    log::warn!("failed to emit auto-land state: {err}");
                }
            };
            run_autoland(
                &repo,
                initial,
                &vcs,
                &forge_side,
                &forge_side,
                &AutoLandConfig::default(),
                &stop,
                &mut publish,
            );
        })
    };
    *slot = Some(Job { stop, thread });
    Ok(AutoLandStatus {
        record,
        live: true,
    })
}

/// Ask the active job to stop. Answers the status as of the request; the
/// definitive `stopped` phase arrives on the event once the job actually
/// winds down (immediately from a wait, after the round from a round).
#[tauri::command]
pub fn autoland_stop(host: tauri::State<'_, AutoLandHost>) -> Option<AutoLandStatus> {
    host.stop_active();
    host.status()
}

/// The latest job status, terminal states and restart survivors included —
/// how a freshly loaded frontend reattaches to whatever the host knows.
#[tauri::command]
pub fn autoland_state(host: tauri::State<'_, AutoLandHost>) -> Option<AutoLandStatus> {
    host.status()
}

/// Clear a finished or interrupted job's record, memory and disk both.
/// Refused while a thread is driving it — stop the job first.
#[tauri::command]
pub fn autoland_dismiss(
    app: AppHandle,
    host: tauri::State<'_, AutoLandHost>,
) -> Result<(), CommandError> {
    // Job liveness first, record lock second — `autoland_start` nests the
    // locks the other way around, so overlapping them here could deadlock.
    let alive = host.job_alive();
    let dismissable = host
        .record
        .lock()
        .expect("auto-land record lock poisoned")
        .as_ref()
        .is_none_or(|record| record.state.phase.is_terminal() || !alive);
    if !dismissable {
        return Err(CommandError::new(
            "autoland_running",
            "The auto-land job is still running — stop it before dismissing",
        ));
    }
    *host.record.lock().expect("auto-land record lock poisoned") = None;
    if let Some(path) = record_path(&app) {
        if let Err(err) = clear_autoland_record(&path) {
            log::warn!("failed to clear the auto-land record: {err}");
        }
    }
    Ok(())
}
