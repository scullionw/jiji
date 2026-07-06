//! Auto-land job hosting: the desktop app as one host for `jiji-forge`'s
//! supervised watch loop.
//!
//! The engine (`jiji_forge::run_autoland`) owns the state machine; this
//! module owns what a host owes it — a thread, the stop signal, and a
//! place to publish state. One job at a time for now (the later M5
//! persistence slice revisits multiples): starting a second is refused
//! while one is alive, and switching repos stops the job (the engine's
//! own repo-path pin is the backstop if a round is mid-flight).
//!
//! Job state reaches the shell two ways, mirroring snapshots: every phase
//! change is emitted as the `autoland://state` event, and `autoland_state`
//! answers the latest state whole so a reloading frontend can reattach.

use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use jiji_forge::{
    no_github_remote, resolve_token, run_autoland, AutoLandConfig, AutoLandPhase, AutoLandState,
    ForgeError, GitHubClient, LandRepoForge, StopSignal,
};
use tauri::{AppHandle, Emitter, Manager as _, State};

use crate::commands::{AppState, CommandError};
use crate::forge::{detected_repo, token_store, HostVcs};

pub const AUTOLAND_EVENT: &str = "autoland://state";

/// The active job's handles. `latest` mirrors what the thread last
/// published; kept after the job ends so a reattaching frontend still sees
/// the terminal state (persistence across app restarts is a later slice).
struct Job {
    stop: Arc<StopSignal>,
    latest: Arc<Mutex<AutoLandState>>,
    thread: JoinHandle<()>,
}

pub struct AutoLandHost {
    job: Mutex<Option<Job>>,
}

impl AutoLandHost {
    pub fn new() -> Self {
        Self {
            job: Mutex::new(None),
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

    fn latest_state(&self) -> Option<AutoLandState> {
        self.job
            .lock()
            .expect("auto-land job lock poisoned")
            .as_ref()
            .map(|job| {
                job.latest
                    .lock()
                    .expect("auto-land state lock poisoned")
                    .clone()
            })
    }
}

/// Queue the stack under a bookmark for auto-land: the supervised job
/// keeps re-deriving the landing plan and running rounds as remote
/// conditions allow, until the stack is fully landed. Refusals a plan
/// would make (no GitHub remote, no token) answer here before any thread
/// starts; a job already running is refused — stop it first.
#[tauri::command(async)]
pub fn autoland_start(
    app: AppHandle,
    state: State<'_, AppState>,
    host: State<'_, AutoLandHost>,
    head_bookmark: String,
) -> Result<AutoLandState, CommandError> {
    let mut slot = host.job.lock().expect("auto-land job lock poisoned");
    if let Some(job) = slot.as_ref() {
        if !job.thread.is_finished() {
            let running = job
                .latest
                .lock()
                .expect("auto-land state lock poisoned")
                .head_bookmark
                .clone();
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
    let repo = detected_repo(&state).ok_or_else(no_github_remote)?;
    let resolved = resolve_token(&token_store(Some(&repo)))?.ok_or(ForgeError::NoToken)?;
    let client = GitHubClient::for_repo(&repo, &resolved.token)?;

    let stop = Arc::new(StopSignal::new());
    let initial = AutoLandState {
        head_bookmark: head_bookmark.clone(),
        phase: AutoLandPhase::Waiting {
            attention: false,
            reasons: vec!["Sizing up the stack".to_owned()],
        },
        rounds: 0,
        merged: Vec::new(),
        segments: Vec::new(),
        last_outcome: None,
    };
    let latest = Arc::new(Mutex::new(initial.clone()));

    let thread = {
        let app = app.clone();
        let stop = Arc::clone(&stop);
        let latest = Arc::clone(&latest);
        std::thread::spawn(move || {
            let state: State<'_, AppState> = app.state();
            let vcs = HostVcs {
                app: &app,
                state: &state,
            };
            let forge_side = LandRepoForge {
                client: &client,
                repo: &repo,
            };
            let mut publish = |published: &AutoLandState| {
                *latest.lock().expect("auto-land state lock poisoned") = published.clone();
                if let Err(err) = app.emit(AUTOLAND_EVENT, published) {
                    log::warn!("failed to emit auto-land state: {err}");
                }
            };
            run_autoland(
                &repo,
                &head_bookmark,
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
        latest,
        thread,
    });
    Ok(initial)
}

/// Ask the active job to stop. Answers the state as of the request; the
/// definitive `stopped` phase arrives on the event once the job actually
/// winds down (immediately from a wait, after the round from a round).
#[tauri::command]
pub fn autoland_stop(host: State<'_, AutoLandHost>) -> Option<AutoLandState> {
    host.stop_active();
    host.latest_state()
}

/// The latest job state, terminal states included — how a freshly loaded
/// frontend reattaches to a job the shell already shows.
#[tauri::command]
pub fn autoland_state(host: State<'_, AutoLandHost>) -> Option<AutoLandState> {
    host.latest_state()
}
