//! Auto-land job hosting: the desktop app as one host for `jiji-forge`'s
//! supervised watch loop.
//!
//! The engine (`jiji_forge::run_autoland`) owns the state machine; this
//! module owns what a host owes it — a thread, the stop signal, a place
//! to publish state, and the persisted job records. One *live* job at a
//! time (a deliberate simplicity cap — one supervised story to follow):
//! starting a second is refused while one is alive.
//! Records, though, are a list keyed by repo and head bookmark, so every
//! job story the host has seen keeps its own slot — queueing a stack
//! never silently supersedes another stack's (or another repo's)
//! interrupted record.
//!
//! Job state reaches the shell two ways, mirroring snapshots: every
//! change is emitted as the `autoland://state` event carrying all
//! statuses, and `autoland_state` answers the same list whole so a
//! reloading frontend can reattach. The records also persist to one JSON
//! file in the app data dir (`jiji-forge` owns the format and the atomic
//! write; this host owns the path and when to save): saved on every
//! publish, loaded back at launch, removed one at a time on dismiss — so
//! a waiting or parked job survives an app restart as resumable status.
//! A record no thread is driving answers as `live: false`, which the UI
//! renders as "interrupted — resume?"; Resume is a plain re-queue, and
//! `autoland_start` seeds the engine from a matching record so the story
//! (rounds, landed PRs) continues.
//!
//! The job is pinned to its repo path, so other tabs taking the screen
//! never disturb it. Closing the job's tab *detaches* it rather than
//! stopping it on the record: the job winds down (its session is being
//! dropped), but the terminal phase it publishes on the way out is an
//! artifact of the close, not the job's story — so it stays off the
//! record, and the last watching state survives as interrupted.
//! Reopening the repo offers Resume, since nobody asked the job itself
//! to stop.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use jiji_forge::{
    load_autoland_records, no_github_remote, remove_autoland_record, resolve_token, run_autoland,
    save_autoland_records, unix_now_ms, upsert_autoland_record, AutoLandConfig, AutoLandRecord,
    AutoLandState, AutoLandStatus, ForgeError, GitHubClient, LandOptions, LandRepoForge,
    StopSignal,
};
use tauri::{AppHandle, Emitter, Manager as _, Runtime};

use crate::commands::{AppState, CommandError};
use crate::forge::{detected_repo, run_blocking, token_store, HostVcs};

pub const AUTOLAND_EVENT: &str = "autoland://state";

/// Where the job records live: the app data dir, resolved per call (it
/// cannot fail after setup, but a missing dir must never take the job
/// down — persistence degrades, the in-memory mirror still serves).
fn record_path<R: Runtime>(manager: &impl tauri::Manager<R>) -> Option<PathBuf> {
    match manager.path().app_data_dir() {
        Ok(dir) => Some(dir.join("autoland.json")),
        Err(err) => {
            log::warn!("no app data dir for the auto-land records: {err}");
            None
        }
    }
}

/// The active job's handles.
struct Job {
    stop: Arc<StopSignal>,
    thread: JoinHandle<()>,
    /// The record this thread drives.
    repo_path: String,
    head_bookmark: String,
    /// Set when the job is stopped because its repo was switched away:
    /// terminal publishes after this stay off the record, so the story
    /// survives as interrupted (resumable) instead of terminal.
    detached: Arc<AtomicBool>,
}

pub struct AutoLandHost {
    job: Mutex<Option<Job>>,
    /// Every job story the host knows — the live thread's latest publish
    /// plus records loaded from disk at launch, keyed by repo and head
    /// bookmark. Dismiss removes one at a time.
    records: Arc<Mutex<Vec<AutoLandRecord>>>,
}

impl AutoLandHost {
    pub fn new() -> Self {
        Self {
            job: Mutex::new(None),
            records: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Load the persisted records at launch, if any survived. Called once
    /// from the app's setup hook.
    pub fn load_persisted<R: Runtime>(&self, manager: &impl tauri::Manager<R>) {
        if let Some(path) = record_path(manager) {
            let records = load_autoland_records(&path);
            if !records.is_empty() {
                *self
                    .records
                    .lock()
                    .expect("auto-land records lock poisoned") = records;
            }
        }
    }

    /// Detach the live job driving `repo_path`: its tab is closing, so the
    /// job winds down, but the terminal phase it publishes on the way out
    /// stays off the record — the last watching state survives as
    /// interrupted, and reopening the repo offers Resume. Jobs for other
    /// repos are left alone.
    pub fn detach_for(&self, repo_path: &str) {
        if let Some(job) = self
            .job
            .lock()
            .expect("auto-land job lock poisoned")
            .as_ref()
        {
            if job.repo_path == repo_path {
                job.detached.store(true, Ordering::SeqCst);
                job.stop.stop();
            }
        }
    }

    /// Ask the active job to poll now instead of waiting out the rest of
    /// its interval — called when the window regains focus, so the user
    /// coming back to the app sees fresh job state instead of up to a
    /// poll-interval of staleness. A no-op without a live job.
    pub fn poke_active(&self) {
        if let Some(job) = self
            .job
            .lock()
            .expect("auto-land job lock poisoned")
            .as_ref()
        {
            if !job.thread.is_finished() {
                job.stop.nudge();
            }
        }
    }

    /// The record the live thread drives, if one is running.
    fn live_job_key(&self) -> Option<(String, String)> {
        self.job
            .lock()
            .expect("auto-land job lock poisoned")
            .as_ref()
            .filter(|job| !job.thread.is_finished())
            .map(|job| (job.repo_path.clone(), job.head_bookmark.clone()))
    }

    /// Every record as a surface sees it: `live` only for the one a
    /// thread is driving. A non-terminal record with no thread — a
    /// restart survivor, a detached job, or one whose thread died — is
    /// the "interrupted" state.
    fn statuses(&self) -> Vec<AutoLandStatus> {
        // Job liveness first, records lock second — `autoland_start`
        // nests them the other way, so overlapping here could deadlock.
        let live = self.live_job_key();
        self.records
            .lock()
            .expect("auto-land records lock poisoned")
            .iter()
            .map(|record| AutoLandStatus {
                live: !record.state.phase.is_terminal()
                    && live
                        .as_ref()
                        .is_some_and(|(repo, head)| record.is_for(repo, head)),
                record: record.clone(),
            })
            .collect()
    }
}

/// Queue the stack under a bookmark for auto-land: the supervised job
/// keeps re-deriving the landing plan and running rounds as remote
/// conditions allow, until the stack is fully landed. Refusals a plan
/// would make (no GitHub remote, no token) answer here before any thread
/// starts; a job already running is refused — stop it first. When a
/// persisted record matches this repo and bookmark and is not terminal,
/// the job resumes from it — the restart-survivor's Resume and a plain
/// re-queue of the same stack are deliberately the same path.
#[tauri::command]
pub async fn autoland_start(
    app: AppHandle,
    repo_path: String,
    head_bookmark: String,
    options: Option<LandOptions>,
) -> Result<Vec<AutoLandStatus>, CommandError> {
    run_blocking(move || autoland_start_impl(app, repo_path, head_bookmark, options)).await
}

fn autoland_start_impl(
    app: AppHandle,
    repo_path: String,
    head_bookmark: String,
    options: Option<LandOptions>,
) -> Result<Vec<AutoLandStatus>, CommandError> {
    let state = app.state::<AppState>();
    let host = app.state::<AutoLandHost>();
    let mut slot = host.job.lock().expect("auto-land job lock poisoned");
    if let Some(job) = slot.as_ref() {
        if !job.thread.is_finished() {
            let running = job.head_bookmark.clone();
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
    state.require_open(&repo_path)?;
    let repo = detected_repo(&state, &repo_path).ok_or_else(no_github_remote)?;
    let resolved = resolve_token(&token_store(Some(&repo)))?.ok_or(ForgeError::NoToken)?;
    let client = GitHubClient::for_repo(&repo, &resolved.token)?;

    // A surviving record for this exact stack seeds the job so its story
    // continues; a terminal one is replaced by the fresh start. Other
    // stacks' records stay untouched either way.
    let prior = host
        .records
        .lock()
        .expect("auto-land records lock poisoned")
        .iter()
        .find(|record| record.is_for(&repo_path, &head_bookmark))
        .filter(|record| !record.state.phase.is_terminal())
        .cloned();
    let mut initial = match prior {
        Some(record) => AutoLandState::resumed(record.state),
        None => AutoLandState::queued(&head_bookmark),
    };
    // Explicit options at queueing win; a plain resume (no options sent)
    // keeps the ones the record was queued with.
    if let Some(options) = options {
        initial.options = options;
    }

    let stop = Arc::new(StopSignal::new());
    let detached = Arc::new(AtomicBool::new(false));
    let record = AutoLandRecord {
        repo_path: repo_path.clone(),
        state: initial.clone(),
        saved_at_ms: unix_now_ms(),
    };
    {
        let mut records = host
            .records
            .lock()
            .expect("auto-land records lock poisoned");
        upsert_autoland_record(&mut records, record);
        if let Some(path) = record_path(&app) {
            if let Err(err) = save_autoland_records(&path, &records) {
                log::warn!("failed to save the auto-land records: {err}");
            }
        }
    }

    let thread = {
        let app = app.clone();
        let stop = Arc::clone(&stop);
        let detached = Arc::clone(&detached);
        let records_slot = Arc::clone(&host.records);
        let repo_path = repo_path.clone();
        let head_bookmark = head_bookmark.clone();
        std::thread::spawn(move || {
            let state: tauri::State<'_, AppState> = app.state();
            let vcs = HostVcs {
                app: &app,
                state: &state,
                repo_path: repo_path.clone(),
            };
            let forge_side = LandRepoForge {
                client: &client,
                repo: &repo,
            };
            let path = record_path(&app);
            let mut publish = |published: &AutoLandState| {
                let terminal = published.phase.is_terminal();
                let mut records = records_slot
                    .lock()
                    .expect("auto-land records lock poisoned");
                // A detached job's terminal phase is an artifact of the
                // repo switch, not the job's story: the last watching
                // record stays, reading interrupted once the thread ends.
                // Non-terminal publishes are real progress and still land.
                if !(terminal && detached.load(Ordering::SeqCst)) {
                    let record = AutoLandRecord {
                        repo_path: repo_path.clone(),
                        state: published.clone(),
                        saved_at_ms: unix_now_ms(),
                    };
                    upsert_autoland_record(&mut records, record);
                    if let Some(path) = &path {
                        if let Err(err) = save_autoland_records(path, &records) {
                            log::warn!("failed to save the auto-land records: {err}");
                        }
                    }
                }
                // Only this thread can be live, so the statuses need no
                // job lock: live is this record, until the terminal
                // publish on the way out.
                let statuses: Vec<AutoLandStatus> = records
                    .iter()
                    .map(|record| AutoLandStatus {
                        live: !terminal
                            && !record.state.phase.is_terminal()
                            && record.is_for(&repo_path, &head_bookmark),
                        record: record.clone(),
                    })
                    .collect();
                drop(records);
                if let Err(err) = app.emit(AUTOLAND_EVENT, &statuses) {
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
    *slot = Some(Job {
        stop,
        thread,
        repo_path,
        head_bookmark,
        detached,
    });
    drop(slot);
    Ok(host.statuses())
}

/// Ask the active job to stop. Answers the statuses as of the request;
/// the definitive `stopped` phase arrives on the event once the job
/// actually winds down (immediately from a wait, after the round from a
/// round).
#[tauri::command]
pub fn autoland_stop(host: tauri::State<'_, AutoLandHost>) -> Vec<AutoLandStatus> {
    if let Some(job) = host
        .job
        .lock()
        .expect("auto-land job lock poisoned")
        .as_ref()
    {
        job.stop.stop();
    }
    host.statuses()
}

/// Every job status the host knows, terminal states and restart survivors
/// included — how a freshly loaded frontend reattaches to whatever the
/// host is doing.
#[tauri::command]
pub fn autoland_state(host: tauri::State<'_, AutoLandHost>) -> Vec<AutoLandStatus> {
    host.statuses()
}

/// Clear one finished or interrupted job's record, memory and disk both.
/// The record a live thread is driving is refused — stop the job first;
/// every other record dismisses independently.
#[tauri::command]
pub fn autoland_dismiss(
    app: AppHandle,
    host: tauri::State<'_, AutoLandHost>,
    repo_path: String,
    head_bookmark: String,
) -> Result<Vec<AutoLandStatus>, CommandError> {
    // Job liveness first, records lock second — `autoland_start` nests
    // the locks the other way around, so overlapping could deadlock.
    let live = host.live_job_key();
    if live.is_some_and(|(repo, head)| repo == repo_path && head == head_bookmark) {
        return Err(CommandError::new(
            "autoland_running",
            "The auto-land job is still running — stop it before dismissing",
        ));
    }
    let mut records = host
        .records
        .lock()
        .expect("auto-land records lock poisoned");
    remove_autoland_record(&mut records, &repo_path, &head_bookmark);
    if let Some(path) = record_path(&app) {
        if let Err(err) = save_autoland_records(&path, &records) {
            log::warn!("failed to save the auto-land records: {err}");
        }
    }
    drop(records);
    Ok(host.statuses())
}
