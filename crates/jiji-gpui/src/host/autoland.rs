//! Auto-land job hosting: the desktop app as one host for `jiji-forge`'s
//! supervised watch loop.
//!
//! The engine (`jiji_forge::run_autoland`) owns the state machine; this
//! module owns what a host owes it — a thread, the stop signal, a place to
//! publish state, and the persisted job records. One *live* job at a time
//! (a deliberate simplicity cap): starting a second is refused while one is
//! alive. Records are a list keyed by repo and head bookmark, so every job
//! story the host has seen keeps its own slot.
//!
//! Every change is published as [`HostEvent::AutoLand`] carrying all
//! statuses, and the records persist to one JSON file in the app data dir:
//! saved on every publish, loaded back at launch, removed one at a time on
//! dismiss. A record no thread is driving answers as `live: false`, which
//! the UI renders as "interrupted — resume?"; Resume is a plain re-queue.
//!
//! Closing the job's tab *detaches* it rather than stopping it on the
//! record: the job winds down, but the terminal phase it publishes on the
//! way out is an artifact of the close, so it stays off the record and the
//! last watching state survives as interrupted.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use jiji_forge::{
    load_autoland_records, remove_autoland_record, run_autoland, save_autoland_records,
    unix_now_ms, upsert_autoland_record, AutoLandConfig, AutoLandRecord, AutoLandState,
    AutoLandStatus, ForgeError, GitHubClient, LandOptions, LandRepoForge, StopSignal,
};

use super::forge::{resolved_token, HostVcs};
use super::{CommandError, Host, HostEvent, HostResult};

/// The active job's handles.
struct Job {
    stop: Arc<StopSignal>,
    thread: JoinHandle<()>,
    repo_path: String,
    head_bookmark: String,
    /// Set when the job is stopped because its repo's tab closed: terminal
    /// publishes after this stay off the record.
    detached: Arc<AtomicBool>,
}

pub struct AutoLandHost {
    job: Mutex<Option<Job>>,
    records: Arc<Mutex<Vec<AutoLandRecord>>>,
}

impl AutoLandHost {
    pub fn new() -> Self {
        Self {
            job: Mutex::new(None),
            records: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub fn load_persisted(&self, path: &Path) {
        let records = load_autoland_records(path);
        if !records.is_empty() {
            *self
                .records
                .lock()
                .expect("auto-land records lock poisoned") = records;
        }
    }

    /// Detach the live job driving `repo_path` (its tab is closing). Jobs
    /// for other repos are left alone.
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
    /// its interval — called when the window regains focus.
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

    fn live_job_key(&self) -> Option<(String, String)> {
        self.job
            .lock()
            .expect("auto-land job lock poisoned")
            .as_ref()
            .filter(|job| !job.thread.is_finished())
            .map(|job| (job.repo_path.clone(), job.head_bookmark.clone()))
    }

    /// Every record as a surface sees it: `live` only for the one a thread
    /// is driving.
    pub fn statuses(&self) -> Vec<AutoLandStatus> {
        // Job liveness first, records lock second — `start` nests them the
        // other way, so overlapping here could deadlock.
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

fn save(path: &Path, records: &[AutoLandRecord]) {
    if let Err(err) = save_autoland_records(path, records) {
        log::warn!("failed to save the auto-land records: {err}");
    }
}

impl Host {
    /// Queue the stack under a bookmark for auto-land. Refusals a plan
    /// would make (no GitHub remote, no token) answer here before any
    /// thread starts; a job already running is refused. A surviving
    /// non-terminal record for the same stack seeds the job, so Resume and
    /// a plain re-queue are the same path.
    pub fn autoland_start(
        self: &Arc<Self>,
        repo_path: &str,
        head_bookmark: &str,
        options: Option<LandOptions>,
    ) -> HostResult<Vec<AutoLandStatus>> {
        let auto = &self.autoland;
        let mut slot = auto.job.lock().expect("auto-land job lock poisoned");
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
        // Build the forge context up front so connection problems refuse
        // the queueing instead of parking a just-started job.
        self.require_open(repo_path)?;
        let repo = self
            .detected_repo(repo_path)
            .ok_or_else(jiji_forge::no_github_remote)?;
        let resolved = resolved_token(Some(&repo))?.ok_or(ForgeError::NoToken)?;
        let client = GitHubClient::for_repo(&repo, &resolved.token)?;

        let prior = auto
            .records
            .lock()
            .expect("auto-land records lock poisoned")
            .iter()
            .find(|record| record.is_for(repo_path, head_bookmark))
            .filter(|record| !record.state.phase.is_terminal())
            .cloned();
        let mut initial = match prior {
            Some(record) => AutoLandState::resumed(record.state),
            None => AutoLandState::queued(head_bookmark),
        };
        // Explicit options at queueing win; a plain resume keeps the ones
        // the record was queued with.
        if let Some(options) = options {
            initial.options = options;
        }

        let stop = Arc::new(StopSignal::new());
        let detached = Arc::new(AtomicBool::new(false));
        let record_path = self.autoland_path();
        {
            let mut records = auto
                .records
                .lock()
                .expect("auto-land records lock poisoned");
            upsert_autoland_record(
                &mut records,
                AutoLandRecord {
                    repo_path: repo_path.to_owned(),
                    state: initial.clone(),
                    saved_at_ms: unix_now_ms(),
                },
            );
            save(&record_path, &records);
        }

        let thread = {
            let host = Arc::clone(self);
            let stop = Arc::clone(&stop);
            let detached = Arc::clone(&detached);
            let records_slot = Arc::clone(&auto.records);
            let repo_path = repo_path.to_owned();
            let head_bookmark = head_bookmark.to_owned();
            std::thread::spawn(move || {
                let vcs = HostVcs {
                    host: &host,
                    repo_path: repo_path.clone(),
                };
                let forge_side = LandRepoForge {
                    client: &client,
                    repo: &repo,
                };
                let mut publish = |published: &AutoLandState| {
                    let terminal = published.phase.is_terminal();
                    let mut records = records_slot
                        .lock()
                        .expect("auto-land records lock poisoned");
                    // A detached job's terminal phase is an artifact of the
                    // tab closing, not the job's story.
                    if !(terminal && detached.load(Ordering::SeqCst)) {
                        upsert_autoland_record(
                            &mut records,
                            AutoLandRecord {
                                repo_path: repo_path.clone(),
                                state: published.clone(),
                                saved_at_ms: unix_now_ms(),
                            },
                        );
                        save(&record_path, &records);
                    }
                    // Only this thread can be live, so the statuses need no
                    // job lock.
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
                    host.emit(HostEvent::AutoLand(statuses));
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
            repo_path: repo_path.to_owned(),
            head_bookmark: head_bookmark.to_owned(),
            detached,
        });
        drop(slot);
        Ok(auto.statuses())
    }

    /// Ask the active job to stop. The definitive `stopped` phase arrives
    /// on the event once the job actually winds down.
    pub fn autoland_stop(&self) -> Vec<AutoLandStatus> {
        if let Some(job) = self
            .autoland
            .job
            .lock()
            .expect("auto-land job lock poisoned")
            .as_ref()
        {
            job.stop.stop();
        }
        self.autoland.statuses()
    }

    pub fn autoland_state(&self) -> Vec<AutoLandStatus> {
        self.autoland.statuses()
    }

    /// Clear one finished or interrupted job's record, memory and disk
    /// both. The record a live thread is driving is refused.
    pub fn autoland_dismiss(
        &self,
        repo_path: &str,
        head_bookmark: &str,
    ) -> HostResult<Vec<AutoLandStatus>> {
        let live = self.autoland.live_job_key();
        if live.is_some_and(|(repo, head)| repo == repo_path && head == head_bookmark) {
            return Err(CommandError::new(
                "autoland_running",
                "The auto-land job is still running — stop it before dismissing",
            ));
        }
        let mut records = self
            .autoland
            .records
            .lock()
            .expect("auto-land records lock poisoned");
        remove_autoland_record(&mut records, repo_path, head_bookmark);
        save(&self.autoland_path(), &records);
        drop(records);
        Ok(self.autoland.statuses())
    }
}
