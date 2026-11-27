//! The auto-land job engine: jjpr's `watch` loop as a supervised state
//! machine over the land engine's plan → execute round. A queued job keeps
//! re-deriving the landing plan from fresh local and GitHub facts, runs a
//! round when the plan has actions, and waits when it does not — so the
//! land flow's deliberate one-merge-per-run becomes an automated continue.
//!
//! The engine stays headless and synchronous like the rest of the crate:
//! the host owns the thread (the desktop app spawns one per job; `jiji
//! watch` drives the same loop from `main`), a [`StopSignal`] makes the
//! between-round sleep interruptible, and every phase change goes out
//! through a plain callback so any host can render it. jjpr's supervision
//! lessons are kept: a consecutive-error budget absorbs transient forge
//! and network failures instead of dying on the first flake, waiting
//! states are classified (remote conditions GitHub clears by itself
//! versus states that need the user) so the shell can phrase them
//! honestly, and the job never mutates anything a fresh plan did not just
//! derive — each round is exactly the plan → execute the manual Land
//! button runs, including its just-in-time re-checks.
//!
//! Job state also persists: [`AutoLandRecord`] is one job's record, and a
//! host saves its records on every publish and loads them at the next
//! launch, so a waiting or parked job survives a restart as cleanly
//! resumable status. The format and file IO live here — plain JSON
//! written atomically via a sibling temp file and rename — so the app and
//! the CLI persist the exact same records; the host owns only where the
//! file lives and when to save. Deliberately no database crate: the
//! requirement is a small human-debuggable file written at most once per
//! poll, which JSON meets outright.
//!
//! Deliberately deferred: queue-position/ETA supervision beyond polling
//! until GitHub's automation finishes.

use std::collections::{HashMap, HashSet};
use std::io;
use std::path::Path;
use std::sync::{Condvar, Mutex};
use std::time::Duration;

use jiji_core::snapshot::{GraphNode, RepoSnapshot};
use serde::{Deserialize, Serialize};

use crate::error::ForgeError;
use crate::land::{
    execute_land, plan_land, LandAction, LandForge, LandOptions, LandOutcome, LandPlan,
    LandSegment, LandSegmentStatus, LandStep, LandVcs,
};
use crate::pr::{PrState, RepoPrState};
use crate::remote::ForgeRepo;
use crate::submit::SubmitStepStatus;

/// Fresh batched open-PR state, re-asked at the top of every poll — jjpr
/// re-reads its PR map every iteration for the same reason: every round's
/// plan must derive from current facts, never from the state the job was
/// started with.
pub trait AutoLandPrs {
    fn open_prs(&self) -> Result<RepoPrState, ForgeError>;
}

impl AutoLandPrs for crate::land::LandRepoForge<'_> {
    fn open_prs(&self) -> Result<RepoPrState, ForgeError> {
        Ok(RepoPrState::new(
            self.client.open_prs(&self.repo.owner, &self.repo.name)?,
            &self.repo.owner,
        ))
    }
}

/// Cross-thread stop flag with an interruptible wait: `stop()` wakes a
/// sleeping job immediately instead of letting it doze through the rest of
/// its poll interval (jjpr chunks its sleep to poll an `AtomicBool`; a
/// condvar gives the same behavior without the latency). `nudge()` wakes
/// the same sleep without ending the job — how a host asks for a fresh
/// poll right now (say, its window regaining focus) instead of letting the
/// user stare at up-to-30-seconds-stale state. A nudge is pending until a
/// wait consumes it, so one landing mid-round shortens the round's
/// following sleep rather than vanishing; an extra poll is always safe —
/// every poll re-derives the plan from fresh facts.
#[derive(Default)]
struct SignalState {
    stopped: bool,
    nudged: bool,
}

#[derive(Default)]
pub struct StopSignal {
    state: Mutex<SignalState>,
    bell: Condvar,
}

impl StopSignal {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn stop(&self) {
        self.state.lock().expect("stop signal lock poisoned").stopped = true;
        self.bell.notify_all();
    }

    /// Ask the job to poll again as soon as it is between rounds. Never
    /// stops or fails anything; a job that is already awake polls once
    /// more promptly.
    pub fn nudge(&self) {
        self.state.lock().expect("stop signal lock poisoned").nudged = true;
        self.bell.notify_all();
    }

    pub fn is_stopped(&self) -> bool {
        self.state.lock().expect("stop signal lock poisoned").stopped
    }

    /// Sleep up to `duration`, waking early on `stop()` or `nudge()` (the
    /// pending nudge is consumed). Answers whether the job was stopped.
    fn wait(&self, duration: Duration) -> bool {
        let state = self.state.lock().expect("stop signal lock poisoned");
        let (mut state, _) = self
            .bell
            .wait_timeout_while(state, duration, |state| !state.stopped && !state.nudged)
            .expect("stop signal lock poisoned");
        state.nudged = false;
        state.stopped
    }
}

/// The job's pacing and patience. Defaults follow jjpr's watch loop: a
/// 30-second poll and a budget of 10 consecutive failures before the job
/// parks itself.
#[derive(Debug, Clone)]
pub struct AutoLandConfig {
    pub poll_interval: Duration,
    pub max_consecutive_errors: u32,
}

impl Default for AutoLandConfig {
    fn default() -> Self {
        Self {
            poll_interval: Duration::from_secs(30),
            max_consecutive_errors: 10,
        }
    }
}

/// Where the job stands right now.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum AutoLandPhase {
    /// Between rounds, watching for conditions to change. `attention`
    /// distinguishes "GitHub clears this by itself" (checks running, a
    /// review pending) from "the user has to act before landing can
    /// continue" (failing checks, requested changes, an unpublished
    /// stack) — the job keeps polling either way, so fixing the named
    /// problem resumes it without any restart.
    #[serde(rename_all = "camelCase")]
    Waiting {
        attention: bool,
        reasons: Vec<String>,
    },
    /// A landing round is executing right now.
    Round,
    /// The whole stack landed and cleaned up. Terminal.
    Done,
    /// The job parked itself: the error budget ran out, or the repo moved
    /// under it. Terminal — the message says what to fix before queueing
    /// again.
    #[serde(rename_all = "camelCase")]
    Failed { message: String },
    /// Stopped on request. Terminal.
    Stopped,
}

impl AutoLandPhase {
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            AutoLandPhase::Done | AutoLandPhase::Failed { .. } | AutoLandPhase::Stopped
        )
    }
}

/// A PR the job saw land: merged by a round, or by GitHub (auto-merge,
/// the queue, someone clicking the button), and then found on trunk by
/// the round's own check.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutoLandMerged {
    pub number: u64,
    pub url: String,
    pub bookmark: String,
}

/// The job as a host renders it: published on every phase change and
/// answered whole when a surface reconnects.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutoLandState {
    pub head_bookmark: String,
    /// The options every round lands with — the method and bypass chosen
    /// at queueing, persisted so a resumed job keeps judging by them.
    #[serde(default)]
    pub options: LandOptions,
    pub phase: AutoLandPhase,
    /// Rounds that executed actions (merges, hand-offs, reconciles).
    pub rounds: u32,
    /// PRs confirmed merged so far, in the order the job saw them land.
    pub merged: Vec<AutoLandMerged>,
    /// The latest plan's segments — the same per-segment story the manual
    /// land card renders.
    pub segments: Vec<LandSegment>,
    /// The last executed round's steps, for the job card.
    pub last_outcome: Option<LandOutcome>,
}

impl AutoLandState {
    /// A fresh job queued on a bookmark, about to take its first look.
    pub fn queued(head_bookmark: &str) -> Self {
        Self {
            head_bookmark: head_bookmark.to_owned(),
            options: LandOptions::default(),
            phase: AutoLandPhase::Waiting {
                attention: false,
                reasons: vec!["Sizing up the stack".to_owned()],
            },
            rounds: 0,
            merged: Vec::new(),
            segments: Vec::new(),
            last_outcome: None,
        }
    }

    /// A restart survivor picking back up: progress (rounds, merged PRs,
    /// the last round's steps) carries over so the story continues, while
    /// the phase resets to sizing-up — every fact the old phase rested on
    /// went stale while the job was away, and the first fresh poll
    /// re-derives all of it.
    pub fn resumed(prior: AutoLandState) -> Self {
        Self {
            phase: AutoLandPhase::Waiting {
                attention: false,
                reasons: vec!["Sizing up the stack".to_owned()],
            },
            ..prior
        }
    }
}

/// The persisted-file format version: a file written by a different
/// version loads as nothing rather than being guessed at. Version 1 was a
/// single-record file; it still loads, lifted into a one-record list, so
/// a job that survived the upgrade keeps its story.
const AUTOLAND_RECORDS_VERSION: u32 = 2;

/// One job's record as a host persists it: the state plus which repo it
/// belongs to and when it was saved. Every job story the host knows keeps
/// its own record in one list file, keyed by repo and head bookmark — so
/// queueing a stack never supersedes another stack's (or another repo's)
/// interrupted story. Saved on every publish and loaded at the next
/// launch; a record only renders while its repo is open, which is why the
/// repo path rides along.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutoLandRecord {
    /// The repo the job was queued in (`RepoSnapshot::repo_path`).
    pub repo_path: String,
    pub state: AutoLandState,
    /// Unix milliseconds of the save — the "as of" behind a restored
    /// status.
    pub saved_at_ms: u64,
}

impl AutoLandRecord {
    /// Whether this record tells the story of `head_bookmark` in the repo
    /// at `repo_path` — the record key: one job story per stack per repo.
    pub fn is_for(&self, repo_path: &str, head_bookmark: &str) -> bool {
        self.repo_path == repo_path && self.state.head_bookmark == head_bookmark
    }
}

/// The list file on disk. Private: hosts hold a plain
/// `Vec<AutoLandRecord>` and go through [`save_autoland_records`] /
/// [`load_autoland_records`].
#[derive(Serialize, Deserialize)]
struct AutoLandRecordsFile {
    version: u32,
    records: Vec<AutoLandRecord>,
}

/// The retired version-1 file shape — one record wearing its own version
/// field — kept only so [`load_autoland_records`] can lift it.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AutoLandRecordV1 {
    version: u32,
    repo_path: String,
    state: AutoLandState,
    saved_at_ms: u64,
}

/// A record as a host answers it to a (re)connecting surface: `live`
/// distinguishes a record a thread is driving right now from one that
/// survived from an earlier session (or whose thread died) — the UI's
/// "interrupted — resume?" state is a non-terminal record that is not
/// live.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutoLandStatus {
    pub record: AutoLandRecord,
    pub live: bool,
}

/// Unix milliseconds now — the record's `saved_at_ms` clock. Lives here so
/// every host stamps records the same way (the engine itself stays
/// clock-free; only the persistence edge tells time).
pub fn unix_now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Replace the record telling this same story — same repo and head
/// bookmark — or append a new one. Order stays stable: an updated record
/// keeps its place, a new job's record goes last.
pub fn upsert_autoland_record(records: &mut Vec<AutoLandRecord>, record: AutoLandRecord) {
    let key = (record.repo_path.clone(), record.state.head_bookmark.clone());
    match records.iter_mut().find(|r| r.is_for(&key.0, &key.1)) {
        Some(slot) => *slot = record,
        None => records.push(record),
    }
}

/// Remove the record for this stack — dismissing it. Answers whether one
/// was there.
pub fn remove_autoland_record(
    records: &mut Vec<AutoLandRecord>,
    repo_path: &str,
    head_bookmark: &str,
) -> bool {
    let before = records.len();
    records.retain(|r| !r.is_for(repo_path, head_bookmark));
    records.len() != before
}

/// Save the records atomically: write a sibling temp file, then rename it
/// over the target, so a crash mid-write leaves the previous file intact
/// instead of half a JSON document. An empty list removes the file — no
/// records and no file are the same state.
pub fn save_autoland_records(path: &Path, records: &[AutoLandRecord]) -> io::Result<()> {
    if records.is_empty() {
        return match std::fs::remove_file(path) {
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
            other => other,
        };
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let file = AutoLandRecordsFile {
        version: AUTOLAND_RECORDS_VERSION,
        records: records.to_vec(),
    };
    let json = serde_json::to_vec_pretty(&file).map_err(io::Error::other)?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, &json)?;
    std::fs::rename(&tmp, path)
}

/// Load the saved records. Anything wrong — no file, unreadable JSON, a
/// foreign format version — answers an empty list: a state file must
/// never stop the app from starting, and stale-format records are dropped
/// rather than guessed at. The one exception is the retired version-1
/// single-record file, which lifts into a one-record list so a job that
/// survived the format upgrade keeps its story.
pub fn load_autoland_records(path: &Path) -> Vec<AutoLandRecord> {
    let Ok(bytes) = std::fs::read(path) else {
        return Vec::new();
    };
    if let Ok(file) = serde_json::from_slice::<AutoLandRecordsFile>(&bytes) {
        return if file.version == AUTOLAND_RECORDS_VERSION {
            file.records
        } else {
            Vec::new()
        };
    }
    match serde_json::from_slice::<AutoLandRecordV1>(&bytes) {
        Ok(v1) if v1.version == 1 => vec![AutoLandRecord {
            repo_path: v1.repo_path,
            state: v1.state,
            saved_at_ms: v1.saved_at_ms,
        }],
        _ => Vec::new(),
    }
}

/// What one poll decided.
enum Step {
    /// Publish this phase and sleep before the next poll.
    Wait(AutoLandPhase),
    /// The job is over.
    End(AutoLandPhase),
}

/// Run the supervised auto-land loop until the stack is fully landed, the
/// job is stopped, or it parks itself. Blocking — the host owns the
/// thread. Every phase change is pushed through `publish` (the final state
/// included), and the final state is also returned. `initial` is
/// [`AutoLandState::queued`] for a fresh job or [`AutoLandState::resumed`]
/// for one picking back up after a restart — prior progress rides in, and
/// the first poll re-derives everything else from fresh facts.
#[allow(clippy::too_many_arguments)]
pub fn run_autoland(
    repo: &ForgeRepo,
    initial: AutoLandState,
    vcs: &dyn LandVcs,
    forge: &dyn LandForge,
    prs: &dyn AutoLandPrs,
    config: &AutoLandConfig,
    stop: &StopSignal,
    publish: &mut dyn FnMut(&AutoLandState),
) -> AutoLandState {
    let mut state = initial;
    let mut consecutive_errors: u32 = 0;
    // Pinned from the first snapshot: the job must never act on a
    // different repo than the one it was queued for.
    let mut repo_path: Option<String> = None;
    // The "branch moved on GitHub" blockers the job last fetched for, so
    // a fetch that did not clear them is not repeated on every poll.
    let mut fetched_for: Option<Vec<String>> = None;

    loop {
        if stop.is_stopped() {
            state.phase = AutoLandPhase::Stopped;
            publish(&state);
            return state;
        }
        let step = poll_once(
            repo,
            vcs,
            forge,
            prs,
            config,
            &mut state,
            &mut consecutive_errors,
            &mut repo_path,
            &mut fetched_for,
            publish,
        );
        match step {
            Step::End(phase) => {
                state.phase = phase;
                publish(&state);
                return state;
            }
            Step::Wait(phase) => {
                state.phase = phase;
                publish(&state);
                if stop.wait(config.poll_interval) {
                    state.phase = AutoLandPhase::Stopped;
                    publish(&state);
                    return state;
                }
            }
        }
    }
}

/// One poll: gather fresh facts, re-derive the plan, and either run the
/// round or say what the job is waiting on.
#[allow(clippy::too_many_arguments)]
fn poll_once(
    repo: &ForgeRepo,
    vcs: &dyn LandVcs,
    forge: &dyn LandForge,
    prs: &dyn AutoLandPrs,
    config: &AutoLandConfig,
    state: &mut AutoLandState,
    consecutive_errors: &mut u32,
    repo_path: &mut Option<String>,
    fetched_for: &mut Option<Vec<String>>,
    publish: &mut dyn FnMut(&AutoLandState),
) -> Step {
    // Transient failures (network flakes, GitHub hiccups, a jj op-store
    // lock held by a CLI command) wait and retry until the budget runs
    // out — jjpr's consecutive-error posture.
    fn transient(config: &AutoLandConfig, errors: &mut u32, story: String) -> Step {
        *errors += 1;
        if *errors >= config.max_consecutive_errors {
            Step::End(AutoLandPhase::Failed {
                message: format!(
                    "Auto-land gave up after {errors} straight failures — the last one: {story}"
                ),
            })
        } else {
            Step::Wait(AutoLandPhase::Waiting {
                attention: false,
                reasons: vec![format!("{story} — retrying on the next check")],
            })
        }
    }

    // Open-PR state first: it advances test scripts and is the one fact
    // every plan needs fresh (the per-PR queries ride on it).
    let pr_state = match prs.open_prs() {
        Ok(prs) => prs,
        Err(err) => return transient(config, consecutive_errors, err.to_string()),
    };
    let snapshot = match vcs.snapshot() {
        Ok(snapshot) => snapshot,
        Err(err) => return transient(config, consecutive_errors, err.to_string()),
    };
    match repo_path {
        Some(pinned) if *pinned != snapshot.repo_path => {
            return Step::End(AutoLandPhase::Failed {
                message: format!(
                    "The open repository changed while auto-land was watching \
                     \u{201c}{}\u{201d} in {pinned}",
                    state.head_bookmark
                ),
            });
        }
        Some(_) => {}
        None => *repo_path = Some(snapshot.repo_path.clone()),
    }
    if !snapshot
        .bookmarks
        .iter()
        .any(|b| b.name == state.head_bookmark && b.is_local)
    {
        return Step::End(AutoLandPhase::Failed {
            message: format!(
                "\u{201c}{}\u{201d} is no longer a bookmark here — the stack moved or \
                 was cleaned up outside the job",
                state.head_bookmark
            ),
        });
    }

    let plan = match plan_land(
        &snapshot,
        &pr_state,
        repo,
        &state.head_bookmark,
        forge,
        vcs,
        &state.options,
    ) {
        Ok(plan) => plan,
        Err(err) => return transient(config, consecutive_errors, err.to_string()),
    };
    // A branch that moved on GitHub since the last fetch (a bot's commit,
    // an "update branch") stops a person's Land so they can look first.
    // Nobody is looking here, and the job was asked to land what passes:
    // fetch once, so the repository has what GitHub has, and plan again.
    // The plan then lands only commits that are here, and the round's
    // check compares against them. If the bookmark did not follow (it had
    // moved here too), the fresh plan says what is in the way, and the
    // job does not fetch for the same reason again.
    let mut fetched: Option<String> = None;
    let behind: Vec<String> = plan
        .blockers
        .iter()
        .filter(|b| b.fetch)
        .map(|b| b.message.clone())
        .collect();
    let (plan, snapshot) = if !behind.is_empty() && fetched_for.as_ref() != Some(&behind) {
        let story = match vcs.git_fetch(&repo.remote) {
            Ok(story) => story,
            // Not remembered as fetched for: the next poll tries again,
            // within the job's budget for failures.
            Err(err) => return transient(config, consecutive_errors, err.to_string()),
        };
        *fetched_for = Some(behind);
        let before = snapshot;
        let snapshot = match vcs.snapshot() {
            Ok(snapshot) => snapshot,
            Err(err) => return transient(config, consecutive_errors, err.to_string()),
        };
        // The fetch is for a branch that gained commits. One that was
        // rewritten or cut back on GitHub comes in the same way: the
        // bookmark follows, and the commits the remote no longer has are
        // dropped here, or stay above the bookmark when another bookmark
        // holds them. Either way they are no longer under the stack's
        // head. Every change the stack had before must still be there; if
        // one is not, the job ends here rather than land what is left.
        // (It has to end: on the next poll the bookmark and GitHub would
        // agree, and nothing would stop the shorter branch from landing.)
        let stack = under(&snapshot, &state.head_bookmark);
        let gone: Vec<&str> = plan
            .segments
            .iter()
            .flat_map(|segment| segment.change_ids.iter())
            .filter(|id| {
                // A change is still there under its change id, or under a
                // commit id if it now has more than one commit.
                let change = before
                    .nodes
                    .iter()
                    .find(|n| n.id == **id)
                    .map_or(id.as_str(), |n| n.change_id.as_str());
                !stack.iter().any(|n| n.id == **id || n.change_id == change)
            })
            .map(String::as_str)
            .collect();
        if !gone.is_empty() {
            return Step::End(AutoLandPhase::Failed {
                message: format!(
                    "A branch of \u{201c}{}\u{201d} was rewritten or cut back on GitHub: after \
                     the fetch, {} {} no longer in the stack. Auto-land stopped rather than land \
                     what is left — look at the stack, and queue it again if that is what you want",
                    state.head_bookmark,
                    gone.join(", "),
                    if gone.len() == 1 { "is" } else { "are" },
                ),
            });
        }
        match plan_land(
            &snapshot,
            &pr_state,
            repo,
            &state.head_bookmark,
            forge,
            vcs,
            &state.options,
        ) {
            Ok(plan) => {
                fetched = Some(story);
                (plan, snapshot)
            }
            Err(err) => return transient(config, consecutive_errors, err.to_string()),
        }
    } else {
        (plan, snapshot)
    };
    // The same question across looks, and across a restart, where
    // `state.segments` is what the job last saw (saved with the job). A
    // fetch somebody else made can bring a cut-back branch just as the
    // job's own can; the bookmark and GitHub then agree at once, and the
    // job would land the shorter branch without ever having fetched. So
    // for every branch still to land, each change the last look listed
    // under it has to be under the stack's head still. A branch that has
    // landed since, by this job or by anyone, is not asked about.
    let stack = under(&snapshot, &state.head_bookmark);
    let short: Vec<(&str, Vec<&str>)> = plan
        .segments
        .iter()
        .filter(|fresh| !matches!(fresh.status, LandSegmentStatus::Merged { .. }))
        .filter_map(|fresh| {
            let before = state.segments.iter().find(|s| s.bookmark == fresh.bookmark)?;
            let gone: Vec<&str> = before
                .change_ids
                .iter()
                .filter(|id| !stack.iter().any(|n| n.id == **id || n.change_id == **id))
                .map(String::as_str)
                .collect();
            (!gone.is_empty()).then_some((fresh.bookmark.as_str(), gone))
        })
        .collect();
    if let Some((bookmark, gone)) = short.first() {
        return Step::End(AutoLandPhase::Failed {
            message: format!(
                "\u{201c}{bookmark}\u{201d} no longer holds {}, which this job was asked to \
                 land: the branch was rewritten or cut back, here or on GitHub. Auto-land \
                 stopped rather than land what is left \u{2014} look at the stack, and queue it \
                 again if that is what you want",
                gone.join(", "),
            ),
        });
    }
    // And a branch the last look listed that is not part of the stack at
    // all any more. Its changes may still be under the head (its bookmark
    // was deleted or renamed), or it may have landed by another hand.
    // Otherwise the stack was reshaped around it, and landing what is
    // left would end "done" with one of the job's pull requests open.
    for before in state.segments.iter().filter(|before| {
        !matches!(before.status, LandSegmentStatus::Merged { .. })
            && !state.merged.iter().any(|m| m.bookmark == before.bookmark)
            && !plan.segments.iter().any(|fresh| fresh.bookmark == before.bookmark)
    }) {
        let gone: Vec<&str> = before
            .change_ids
            .iter()
            .filter(|id| !stack.iter().any(|n| n.id == **id || n.change_id == **id))
            .map(String::as_str)
            .collect();
        if gone.is_empty() {
            continue;
        }
        // Landed by another hand means: the pull request the last look
        // recorded for this branch, that one and no other, has merged,
        // and into trunk. A branch name can have had an earlier pull
        // request that merged, and a pull request can be merged into some
        // other branch; neither is this work on trunk.
        let landed = match &before.pr {
            Some(pr) => match forge.pr_land_state(pr.number, &plan.base_branch) {
                Ok(land) => {
                    land.state == PrState::Merged && land.merged_into() == plan.base_branch
                }
                Err(err) => return transient(config, consecutive_errors, err.to_string()),
            },
            None => false,
        };
        if landed {
            continue;
        }
        return Step::End(AutoLandPhase::Failed {
            message: format!(
                "\u{201c}{}\u{201d} is no longer part of the stack under \u{201c}{}\u{201d}, \
                 and it has not landed: the stack was reshaped, here or on GitHub. Auto-land \
                 stopped rather than land the rest without {} \u{2014} look at the stack, and \
                 queue it again if that is what you want",
                before.bookmark,
                state.head_bookmark,
                gone.join(", "),
            ),
        });
    }
    state.segments = plan.segments.clone();

    if !plan.blockers.is_empty() {
        *consecutive_errors = 0;
        let attention = plan.blockers.iter().any(|b| !b.wait);
        return Step::Wait(AutoLandPhase::Waiting {
            attention,
            reasons: plan.blockers.into_iter().map(|b| b.message).collect(),
        });
    }
    if plan.actions.is_empty() {
        // GitHub's automation is already driving, or there is simply
        // nothing to do yet; the plan's warnings carry the story.
        *consecutive_errors = 0;
        let reasons = if plan.warnings.is_empty() {
            vec!["Nothing to run yet — watching for the stack's conditions to change".to_owned()]
        } else {
            plan.warnings.clone()
        };
        return Step::Wait(AutoLandPhase::Waiting {
            attention: false,
            reasons,
        });
    }

    // A round runs: exactly what the manual Land button executes,
    // just-in-time re-checks included.
    state.phase = AutoLandPhase::Round;
    publish(state);
    let mut outcome = match execute_land(&plan, vcs, forge) {
        Ok(outcome) => outcome,
        Err(err) => return transient(config, consecutive_errors, err.to_string()),
    };
    // The fetch that came before the plan is part of what this round did,
    // and it is how commits nobody here pushed came to land: say so first.
    if let Some(story) = fetched {
        outcome.steps.insert(
            0,
            LandStep {
                action: LandAction::FetchRemote {
                    remote: repo.remote.clone(),
                },
                status: SubmitStepStatus::Done,
                detail: Some(format!(
                    "{story} — a branch of this stack had moved on GitHub since the last \
                     fetch; the round lands what is there now"
                )),
            },
        );
    }
    state.rounds += 1;
    // A landing is progress even when a later step failed, and it is
    // recorded where it is proven: at the check that found the pull
    // request's changes on trunk. GitHub calling a pull request merged is
    // not that (it says so of one pushed under another branch), so
    // neither a merge step nor a plan that merely read "merged" counts.
    for step in &outcome.steps {
        if step.status != SubmitStepStatus::Done {
            continue;
        }
        if let LandAction::CheckLanded {
            number, bookmark, ..
        } = &step.action
        {
            let url = plan
                .segments
                .iter()
                .find(|s| s.bookmark == *bookmark)
                .and_then(|s| match &s.status {
                    LandSegmentStatus::Merged { url, .. } => Some(url.clone()),
                    _ => s.pr.as_ref().map(|pr| pr.url.clone()),
                })
                .unwrap_or_default();
            note_merged(state, *number, &url, bookmark);
        }
    }
    let failed_story = outcome
        .steps
        .iter()
        .find(|s| s.status == SubmitStepStatus::Failed)
        .and_then(|s| s.detail.clone());
    let failed = outcome.failed;
    state.last_outcome = Some(outcome);
    if failed {
        *consecutive_errors += 1;
        let story = failed_story.unwrap_or_else(|| "a landing step failed".to_owned());
        if *consecutive_errors >= config.max_consecutive_errors {
            return Step::End(AutoLandPhase::Failed {
                message: format!(
                    "Auto-land gave up after {consecutive_errors} rounds that could not \
                     finish — the last stop: {story}"
                ),
            });
        }
        return Step::Wait(AutoLandPhase::Waiting {
            attention: true,
            reasons: vec![format!("{story} — retrying on the next check")],
        });
    }
    *consecutive_errors = 0;
    if stack_completed(&plan) {
        return Step::End(AutoLandPhase::Done);
    }
    let reason = if hands_off(&plan) {
        "GitHub is driving the merge now — watching for it to finish".to_owned()
    } else if chained(&plan) {
        "Landed a round — GitHub auto-merges the next PR once its rebased checks pass"
            .to_owned()
    } else {
        "Landed a round — waiting for the rebased stack's checks before the next".to_owned()
    };
    Step::Wait(AutoLandPhase::Waiting {
        attention: false,
        reasons: vec![reason],
    })
}

/// What a bookmark's stack is made of, as far as the snapshot draws it:
/// the bookmark's commit and everything below it, landed commits
/// included. When the bookmark's commit is not drawn there is nothing to
/// walk from, and the answer is everything the snapshot draws.
fn under<'a>(snapshot: &'a RepoSnapshot, bookmark: &str) -> Vec<&'a GraphNode> {
    let by_id: HashMap<&str, &GraphNode> =
        snapshot.nodes.iter().map(|n| (n.id.as_str(), n)).collect();
    let top = snapshot
        .bookmarks
        .iter()
        .find(|b| b.name == bookmark && b.is_local)
        .and_then(|b| by_id.get(b.target.as_str()).copied());
    let Some(top) = top else {
        return snapshot.nodes.iter().collect();
    };
    let mut seen: HashSet<&str> = HashSet::new();
    let mut stack = Vec::new();
    let mut queue = vec![top];
    while let Some(node) = queue.pop() {
        if !seen.insert(node.id.as_str()) {
            continue;
        }
        stack.push(node);
        queue.extend(node.parents.iter().filter_map(|p| by_id.get(p.as_str()).copied()));
    }
    stack
}

/// The round handed the *candidate's* merge to GitHub's automation
/// instead of merging. A plan that merges and then arms auto-merge on the
/// next PR (the chain coda) is a real merge round, not a hand-off.
fn hands_off(plan: &LandPlan) -> bool {
    !plan
        .actions
        .iter()
        .any(|a| matches!(a, LandAction::MergePr { .. }))
        && plan.actions.iter().any(|a| {
            matches!(
                a,
                LandAction::EnableAutoMerge { .. } | LandAction::EnqueuePr { .. }
            )
        })
}

/// The round merged and armed auto-merge on the next PR up.
fn chained(plan: &LandPlan) -> bool {
    plan.actions
        .iter()
        .any(|a| matches!(a, LandAction::MergePr { .. }))
        && plan
            .actions
            .iter()
            .any(|a| matches!(a, LandAction::EnableAutoMerge { .. }))
}

/// The executed round finished the whole stack: every segment either
/// already merged or landed in this round, and nothing was handed off to
/// GitHub (a hand-off means the merge is still pending remotely).
fn stack_completed(plan: &LandPlan) -> bool {
    !hands_off(plan)
        && plan.segments.iter().all(|s| {
            matches!(
                s.status,
                LandSegmentStatus::Merged { .. } | LandSegmentStatus::Landing
            )
        })
}

fn note_merged(state: &mut AutoLandState, number: u64, url: &str, bookmark: &str) {
    if !state.merged.iter().any(|m| m.number == number) {
        state.merged.push(AutoLandMerged {
            number,
            url: url.to_owned(),
            bookmark: bookmark.to_owned(),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::land::{Mergeable, PrLandState};
    use crate::pr::{ChecksRollup, PrState, PrStateReport, PrSummary, ReviewDecision};
    use crate::remote::ForgeProvider;
    use jiji_core::snapshot::{
        BookmarkState, GraphNode, LandedCheck, NodeKind, RepoSnapshot, SyncState, TipCheck,
    };
    use jiji_core::BackendError;
    use std::cell::{Cell, RefCell};
    use std::collections::HashMap;
    use std::sync::Arc;

    fn node(
        id: &str,
        description: &str,
        kind: NodeKind,
        parents: &[&str],
        bookmarks: &[&str],
    ) -> GraphNode {
        GraphNode {
            id: id.into(),
            change_id: id.into(),
            commit_id: format!("c{id}"),
            description: description.into(),
            author: "Test <t@example.com>".into(),
            timestamp: "2026-07-01T12:00:00Z".into(),
            kind,
            parents: parents.iter().map(|p| p.to_string()).collect(),
            elided_parents: vec![],
            bookmarks: bookmarks.iter().map(|b| b.to_string()).collect(),
            tags: Vec::new(),
            is_empty: false,
            has_conflict: false,
            is_divergent: false,
            held: false,
            old_copies_held: false,
        }
    }

    fn bookmark(name: &str, target: &str, sync: SyncState, is_trunk: bool) -> BookmarkState {
        BookmarkState {
            name: name.into(),
            target: target.into(),
            remote: (sync != SyncState::LocalOnly).then(|| "origin".into()),
            sync,
            is_trunk,
            is_local: true,
            tracked: sync != SyncState::LocalOnly,
            is_merged: false,
            target_description: String::new(),
            target_author: String::new(),
            target_timestamp: String::new(),
        }
    }

    fn snapshot(nodes: Vec<GraphNode>, bookmarks: Vec<BookmarkState>) -> RepoSnapshot {
        RepoSnapshot {
            repo_path: "/tmp/repo".into(),
            repo_name: "repo".into(),
            backend: "test".into(),
            view: jiji_core::snapshot::SnapshotView::default(),
            truncated: false,
            trunk_bookmark: "main".into(),
            working_copy: nodes.first().map(|n| n.id.clone()).unwrap_or_default(),
            workspaces: vec![],
            workstreams: vec![],
            nodes,
            bookmarks,
            git_remotes: vec![],
            conflicts: vec![],
            operations: vec![],
            resolve_tool: None,
            taken_over: Vec::new(),
        }
    }

    fn open_pr(number: u64, head: &str, base: &str) -> PrSummary {
        PrSummary {
            number,
            title: format!("PR {number}"),
            url: format!("https://github.com/o/r/pull/{number}"),
            state: PrState::Open,
            is_draft: false,
            head_branch: head.into(),
            head_commit: "ca2feedface".into(),
            head_owner: Some("o".into()),
            base_branch: base.into(),
            body: None,
            review: ReviewDecision::None,
            checks: ChecksRollup::None,
            merge_commit: None,
            stack: None,
            author: None,
        }
    }

    /// A PR GitHub merged into trunk, as commit `landed<number>`.
    fn merged_pr(number: u64, head: &str) -> PrSummary {
        PrSummary {
            state: PrState::Merged,
            merge_commit: Some(format!("landed{number}")),
            ..open_pr(number, head, "main")
        }
    }

    fn forge_repo() -> ForgeRepo {
        ForgeRepo {
            provider: ForgeProvider::GitHub,
            remote: "origin".into(),
            host: "github.com".into(),
            owner: "o".into(),
            name: "r".into(),
        }
    }

    fn ready_state() -> PrLandState {
        PrLandState {
            node_id: "NODE1".into(),
            state: PrState::Open,
            is_draft: false,
            mergeable: Mergeable::Mergeable,
            merge_state: crate::land::MergeStateStatus::Unknown,
            review: ReviewDecision::Approved,
            checks: ChecksRollup::Passing,
            base_branch: "main".into(),
            head_commit: "ca2feedface".into(),
            merge_commit: None,
            stack: None,
            auto_merge_enabled: false,
            in_merge_queue: false,
            auto_merge_allowed: true,
            queue_on_base: false,
            queue_method: None,
            allows_squash: true,
            allows_merge: true,
            allows_rebase: true,
        }
    }

    /// main(m) ── a1 ── a2 (auth, synced, @): one publishable segment.
    fn single_stack() -> RepoSnapshot {
        snapshot(
            vec![
                node("a2", "auth: sessions", NodeKind::WorkingCopy, &["a1"], &["auth"]),
                node("a1", "auth: login flow", NodeKind::Mutable, &["m"], &[]),
                node("m", "release", NodeKind::Immutable, &[], &["main"]),
            ],
            vec![
                bookmark("main", "m", SyncState::Synced, true),
                bookmark("auth", "a2", SyncState::Synced, false),
            ],
        )
    }

    /// main(m) ── a1 ── a2 (auth, synced) ── b1 (profile, synced, @).
    fn two_stack() -> RepoSnapshot {
        let mut answer = snapshot(
            vec![
                node("b1", "profile: avatars", NodeKind::WorkingCopy, &["a2"], &["profile"]),
                node("a2", "auth: sessions", NodeKind::Mutable, &["a1"], &["auth"]),
                node("a1", "auth: login flow", NodeKind::Mutable, &["m"], &[]),
                node("m", "release", NodeKind::Immutable, &[], &["main"]),
            ],
            vec![
                bookmark("main", "m", SyncState::Synced, true),
                bookmark("auth", "a2", SyncState::Synced, false),
                bookmark("profile", "b1", SyncState::Synced, false),
            ],
        );
        // The simulator's push is a no-op, so seed the Git head the
        // chained auto-merge lease would see after a real push.
        answer.nodes[0].commit_id = "ca2feedface".into();
        answer
    }

    /// After auth landed and reconciled: profile alone on the new trunk.
    fn after_first_land() -> RepoSnapshot {
        let mut answer = snapshot(
            vec![
                node("b1", "profile: avatars", NodeKind::WorkingCopy, &["s"], &["profile"]),
                node("s", "auth (squash)", NodeKind::Immutable, &["m"], &["main"]),
                node("m", "release", NodeKind::Immutable, &[], &[]),
            ],
            vec![
                bookmark("main", "s", SyncState::Synced, true),
                bookmark("profile", "b1", SyncState::Synced, false),
            ],
        );
        // The rebased commit that was pushed: what GitHub reports as #7's
        // head from here on.
        answer.nodes[0].commit_id = "ca2feedface".into();
        answer
    }

    /// The scripted world one job run acts on. `script` mutates it at the
    /// start of the numbered poll (0-based) — how tests flip remote and
    /// local state between checks.
    struct World {
        snapshot: RepoSnapshot,
        prs: Vec<PrSummary>,
        land_states: HashMap<u64, PrLandState>,
        merged: HashMap<String, PrSummary>,
        prs_fail: bool,
        merge_fails: bool,
        /// What the repository says of a landing commit; one it has no
        /// answer for is on trunk and holds the changes.
        landings: HashMap<String, LandedCheck>,
        /// What the repository looks like once a fetch has run, when a
        /// fetch changes it: a bookmark that followed its branch.
        after_fetch: Option<RepoSnapshot>,
        /// How many fetches fail before one goes through.
        fetch_fails: u32,
    }

    /// Poll-indexed world mutations a test scripts ahead of time.
    type Script = HashMap<usize, Box<dyn Fn(&mut World)>>;

    struct Sim {
        world: RefCell<World>,
        script: RefCell<Script>,
        poll: Cell<usize>,
        calls: RefCell<Vec<String>>,
    }

    impl Sim {
        fn new(world: World) -> Self {
            Self {
                world: RefCell::new(world),
                script: RefCell::new(HashMap::new()),
                poll: Cell::new(0),
                calls: RefCell::new(vec![]),
            }
        }

        fn at_poll(&self, index: usize, mutation: impl Fn(&mut World) + 'static) {
            self.script.borrow_mut().insert(index, Box::new(mutation));
        }
    }

    impl AutoLandPrs for Sim {
        fn open_prs(&self) -> Result<RepoPrState, ForgeError> {
            // The engine asks once per poll, first — the script advances here.
            let index = self.poll.get();
            self.poll.set(index + 1);
            if let Some(mutation) = self.script.borrow().get(&index) {
                mutation(&mut self.world.borrow_mut());
            }
            let world = self.world.borrow();
            if world.prs_fail {
                return Err(ForgeError::Network("GitHub is unreachable".into()));
            }
            Ok(RepoPrState::new(
                PrStateReport {
                    prs: world.prs.clone(),
                    truncated: false,
                },
                "o",
            ))
        }
    }

    impl LandVcs for Sim {
        fn git_fetch(&self, remote: &str) -> Result<String, BackendError> {
            self.calls.borrow_mut().push(format!("fetch:{remote}"));
            let mut world = self.world.borrow_mut();
            if world.fetch_fails > 0 {
                world.fetch_fails -= 1;
                return Err(BackendError::MutationFailed("the remote hung up".into()));
            }
            if let Some(fetched) = world.after_fetch.take() {
                world.snapshot = fetched;
            }
            Ok("Fetched".into())
        }
        fn snapshot(&self) -> Result<RepoSnapshot, BackendError> {
            Ok(self.world.borrow().snapshot.clone())
        }
        fn rebase_onto_trunk(
            &self,
            _remote: &str,
            _trunk: &str,
            root_change: &str,
            _landed: &[String],
        ) -> Result<String, BackendError> {
            self.calls.borrow_mut().push(format!("rebase:{root_change}"));
            // The chain leaves what it sat on, as jj's rebase does.
            let mut world = self.world.borrow_mut();
            let trunk = world.snapshot.bookmarks.iter().find(|b| b.is_trunk).map(|b| b.target.clone());
            if let (Some(trunk), Some(root)) =
                (trunk, world.snapshot.nodes.iter_mut().find(|n| n.id == root_change))
            {
                root.parents = vec![trunk];
            }
            Ok(format!("Rebased {root_change}"))
        }
        fn push_bookmarks(
            &self,
            bookmarks: &[String],
            remote: &str,
        ) -> Result<String, BackendError> {
            self.calls
                .borrow_mut()
                .push(format!("push:{}:{remote}", bookmarks.join(",")));
            Ok("Pushed".into())
        }
        fn delete_bookmark_at(&self, name: &str, _commit: &str) -> Result<String, BackendError> {
            self.calls.borrow_mut().push(format!("delete:{name}"));
            // The bookmark goes, as in the repository: the sweep that
            // follows reads from the snapshot whether it was left.
            let mut world = self.world.borrow_mut();
            world
                .snapshot
                .bookmarks
                .retain(|b| !(b.name == name && b.is_local));
            for node in world.snapshot.nodes.iter_mut() {
                node.bookmarks.retain(|b| b != name);
            }
            Ok(format!("Deleted {name}"))
        }
        fn resting_on(&self, _commits: &[String]) -> Result<Vec<String>, BackendError> {
            Ok(Vec::new())
        }
        fn abandon_changes(&self, commits: &[String]) -> Result<String, BackendError> {
            self.calls
                .borrow_mut()
                .push(format!("abandon:{}", commits.join(",")));
            // The commits go, and what sat on them sits on what was below
            // them, as after jj's abandon.
            let mut world = self.world.borrow_mut();
            let gone: Vec<(String, Vec<String>)> = world
                .snapshot
                .nodes
                .iter()
                .filter(|n| commits.contains(&n.commit_id))
                .map(|n| (n.id.clone(), n.parents.clone()))
                .collect();
            world.snapshot.nodes.retain(|n| !gone.iter().any(|(id, _)| *id == n.id));
            for node in world.snapshot.nodes.iter_mut() {
                for parent in node.parents.iter_mut() {
                    while let Some((_, below)) = gone.iter().find(|(id, _)| id == parent) {
                        *parent = below.first().cloned().unwrap_or_default();
                    }
                }
            }
            Ok("Abandoned".into())
        }
        fn lands_as_tip(
            &self,
            _remote: &str,
            _trunk: &str,
            _bottom: &str,
            _first_top: &str,
            _tip: &str,
        ) -> Result<TipCheck, BackendError> {
            Ok(TipCheck::Same)
        }
        fn landed(
            &self,
            _remote: &str,
            _trunk: &str,
            landing: &str,
            _head: &str,
            _bottom: &str,
            _top: &str,
        ) -> Result<LandedCheck, BackendError> {
            Ok(self
                .world
                .borrow()
                .landings
                .get(landing)
                .cloned()
                .unwrap_or(LandedCheck::Landed))
        }
    }

    impl LandForge for Sim {
        fn find_merged_pr(&self, branch: &str) -> Result<Option<PrSummary>, ForgeError> {
            Ok(self.world.borrow().merged.get(branch).cloned())
        }
        fn open_prs_based_on(&self, base: &str) -> Result<Vec<PrSummary>, ForgeError> {
            // As GitHub lists them: the open ones, by the base they have.
            Ok(self
                .world
                .borrow()
                .prs
                .iter()
                .filter(|pr| pr.base_branch == base)
                .cloned()
                .collect())
        }
        fn pr_land_state(&self, number: u64, _base: &str) -> Result<PrLandState, ForgeError> {
            let world = self.world.borrow();
            // A PR that merged before the run answers as GitHub does for
            // one: merged, with the commit it landed as.
            let recognized = world.merged.values().find(|pr| pr.number == number).map(|pr| {
                PrLandState {
                    state: PrState::Merged,
                    base_branch: pr.base_branch.clone(),
                    merge_commit: pr.merge_commit.clone(),
                    ..ready_state()
                }
            });
            recognized
                .or_else(|| world.land_states.get(&number).cloned())
                .ok_or_else(|| ForgeError::Api(format!("no stub state for #{number}")))
        }
        fn merge_pr(
            &self,
            number: u64,
            method: crate::land::MergeMethod,
            _expected_head: &str,
            _commit_title: Option<&str>,
            _commit_message: Option<&str>,
        ) -> Result<(), ForgeError> {
            if self.world.borrow().merge_fails {
                return Err(ForgeError::Api("HTTP 405: merge refused".into()));
            }
            self.calls
                .borrow_mut()
                .push(format!("merge:{number}:{}", method.rest_name()));
            // From here on GitHub reports the PR merged, as a new commit.
            if let Some(state) = self.world.borrow_mut().land_states.get_mut(&number) {
                state.state = PrState::Merged;
                state.merge_commit = Some(format!("landed{number}"));
            }
            Ok(())
        }
        fn enable_auto_merge(
            &self,
            node_id: &str,
            method: crate::land::MergeMethod,
            _expected_head: &str,
            _commit_title: Option<&str>,
            _commit_message: Option<&str>,
        ) -> Result<(), ForgeError> {
            self.calls
                .borrow_mut()
                .push(format!("auto_merge:{node_id}:{}", method.rest_name()));
            Ok(())
        }
        fn enqueue_pr(&self, node_id: &str, _expected_head: &str) -> Result<(), ForgeError> {
            self.calls.borrow_mut().push(format!("enqueue:{node_id}"));
            Ok(())
        }
        fn update_pr_base(&self, number: u64, base: &str) -> Result<(), ForgeError> {
            self.calls
                .borrow_mut()
                .push(format!("retarget:{number}:{base}"));
            // GitHub reports the new base from here on.
            if let Some(state) = self.world.borrow_mut().land_states.get_mut(&number) {
                state.base_branch = base.to_owned();
            }
            Ok(())
        }
        fn mark_ready(&self, node_id: &str) -> Result<(), ForgeError> {
            self.calls.borrow_mut().push(format!("mark_ready:{node_id}"));
            Ok(())
        }
        fn merge_stacked(
            &self,
            number: u64,
            method: crate::land::MergeMethod,
            _expected_head: &str,
            _commit_title: Option<&str>,
            _commit_message: Option<&str>,
            _bypass: bool,
        ) -> Result<crate::github::AsyncMerge, ForgeError> {
            self.calls
                .borrow_mut()
                .push(format!("merge_stacked:{number}:{}", method.rest_name()));
            // GitHub lands the PR asked for and every stacked PR below it.
            let mut world = self.world.borrow_mut();
            let Some(seat) = world.land_states.get(&number).and_then(|s| s.stack.clone()) else {
                return Err(ForgeError::Api(format!("#{number} is not in a stack")));
            };
            for (below, state) in world.land_states.iter_mut() {
                let in_merge = state.stack.as_ref().is_some_and(|s| {
                    s.number == seat.number && s.position <= seat.position
                });
                if in_merge && state.state == PrState::Open {
                    state.state = PrState::Merged;
                    state.merge_commit = Some(format!("landed{below}"));
                }
            }
            Ok(crate::github::AsyncMerge::Merged {
                commit: format!("landed{number}"),
            })
        }
        fn enqueue_stacked(
            &self,
            number: u64,
            _expected_head: &str,
        ) -> Result<crate::github::AsyncMerge, ForgeError> {
            self.calls.borrow_mut().push(format!("enqueue_stacked:{number}"));
            Ok(crate::github::AsyncMerge::Enqueued)
        }
        fn await_restack(
            &self,
            number: u64,
            _from_head: &str,
        ) -> Result<crate::github::Restack, ForgeError> {
            self.calls.borrow_mut().push(format!("await_restack:{number}"));
            Ok(crate::github::Restack::Rebased)
        }
    }

    fn test_config() -> AutoLandConfig {
        AutoLandConfig {
            poll_interval: Duration::ZERO,
            max_consecutive_errors: 10,
        }
    }

    #[test]
    fn nudge_wakes_the_wait_without_stopping() {
        let signal = Arc::new(StopSignal::new());
        let waker = Arc::clone(&signal);
        let handle = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(20));
            waker.nudge();
        });
        let started = std::time::Instant::now();
        let stopped = signal.wait(Duration::from_secs(30));
        handle.join().unwrap();
        assert!(!stopped, "a nudge must not read as a stop");
        assert!(!signal.is_stopped());
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "the nudge should wake the wait long before the timeout"
        );
    }

    #[test]
    fn pending_nudge_is_consumed_by_one_wait() {
        let signal = StopSignal::new();
        // A nudge landing while the job is mid-round (not waiting) makes
        // the round's following sleep instant instead of vanishing.
        signal.nudge();
        let started = std::time::Instant::now();
        assert!(!signal.wait(Duration::from_secs(30)));
        assert!(started.elapsed() < Duration::from_secs(10));
        // Consumed: the next wait sleeps its full duration again.
        let started = std::time::Instant::now();
        assert!(!signal.wait(Duration::from_millis(40)));
        assert!(started.elapsed() >= Duration::from_millis(40));
    }

    #[test]
    fn stop_wins_over_a_pending_nudge() {
        let signal = StopSignal::new();
        signal.nudge();
        signal.stop();
        assert!(signal.wait(Duration::from_secs(30)));
        assert!(signal.is_stopped());
    }

    /// Drive a job to its end, logging every published state. The safety
    /// stop keeps a wrong loop from spinning forever.
    fn run(sim: &Sim, head: &str, config: &AutoLandConfig, stop: &StopSignal) -> (AutoLandState, Vec<AutoLandState>) {
        run_from(sim, AutoLandState::queued(head), config, stop)
    }

    fn run_from(
        sim: &Sim,
        initial: AutoLandState,
        config: &AutoLandConfig,
        stop: &StopSignal,
    ) -> (AutoLandState, Vec<AutoLandState>) {
        let log: RefCell<Vec<AutoLandState>> = RefCell::new(vec![]);
        let mut publish = |state: &AutoLandState| {
            log.borrow_mut().push(state.clone());
            if log.borrow().len() > 100 {
                stop.stop();
            }
        };
        let final_state = run_autoland(
            &forge_repo(),
            initial,
            sim,
            sim,
            sim,
            config,
            stop,
            &mut publish,
        );
        (final_state, log.into_inner())
    }

    #[test]
    fn lands_a_ready_single_stack_and_finishes() {
        let mut land_states = HashMap::new();
        land_states.insert(1, ready_state());
        let sim = Sim::new(World {
            snapshot: single_stack(),
            prs: vec![open_pr(1, "auth", "main")],
            land_states,
            merged: HashMap::new(),
            prs_fail: false,
            merge_fails: false,
            landings: HashMap::new(),
            after_fetch: None,
            fetch_fails: 0,
        });

        let (state, log) = run(&sim, "auth", &test_config(), &StopSignal::new());
        assert_eq!(state.phase, AutoLandPhase::Done);
        assert_eq!(state.rounds, 1);
        assert_eq!(
            state.merged,
            vec![AutoLandMerged {
                number: 1,
                url: "https://github.com/o/r/pull/1".into(),
                bookmark: "auth".into(),
            }]
        );
        assert!(!state.last_outcome.as_ref().unwrap().failed);
        // One round, no waiting: the job went straight to work and ended.
        assert_eq!(log.len(), 2);
        assert_eq!(log[0].phase, AutoLandPhase::Round);
        assert_eq!(log[1].phase, AutoLandPhase::Done);
        let calls = sim.calls.borrow();
        assert!(calls.contains(&"merge:1:squash".to_owned()), "{calls:?}");
        assert!(calls.contains(&"abandon:ca2,ca1".to_owned()), "{calls:?}");
    }

    #[test]
    fn waits_quietly_while_checks_run_then_lands() {
        let mut land_states = HashMap::new();
        land_states.insert(
            1,
            PrLandState {
                checks: ChecksRollup::Pending,
                auto_merge_allowed: false,
                ..ready_state()
            },
        );
        let sim = Sim::new(World {
            snapshot: single_stack(),
            prs: vec![open_pr(1, "auth", "main")],
            land_states,
            merged: HashMap::new(),
            prs_fail: false,
            merge_fails: false,
            landings: HashMap::new(),
            after_fetch: None,
            fetch_fails: 0,
        });
        // The checks pass before the third poll.
        sim.at_poll(2, |world| {
            world.land_states.insert(1, ready_state());
        });

        let (state, log) = run(&sim, "auth", &test_config(), &StopSignal::new());
        assert_eq!(state.phase, AutoLandPhase::Done);
        assert_eq!(state.rounds, 1);
        // Two quiet waits — remote conditions, nothing for the user — then
        // the round.
        match &log[0].phase {
            AutoLandPhase::Waiting { attention, reasons } => {
                assert!(!attention);
                assert!(
                    reasons.iter().any(|r| r.contains("checks are still running")),
                    "{reasons:?}"
                );
            }
            other => panic!("expected waiting, got {other:?}"),
        }
        assert_eq!(log[0].phase, log[1].phase);
        assert_eq!(log[2].phase, AutoLandPhase::Round);
    }

    #[test]
    fn needs_user_states_read_as_attention() {
        let mut land_states = HashMap::new();
        land_states.insert(
            1,
            PrLandState {
                review: ReviewDecision::ChangesRequested,
                ..ready_state()
            },
        );
        let sim = Sim::new(World {
            snapshot: single_stack(),
            prs: vec![open_pr(1, "auth", "main")],
            land_states,
            merged: HashMap::new(),
            prs_fail: false,
            merge_fails: false,
            landings: HashMap::new(),
            after_fetch: None,
            fetch_fails: 0,
        });

        // Stop after the first published wait; the point is the phrasing.
        let stop = StopSignal::new();
        let log: RefCell<Vec<AutoLandState>> = RefCell::new(vec![]);
        let mut publish = |state: &AutoLandState| {
            log.borrow_mut().push(state.clone());
            stop.stop();
        };
        let final_state = run_autoland(
            &forge_repo(),
            AutoLandState::queued("auth"),
            &sim,
            &sim,
            &sim,
            &AutoLandConfig {
                poll_interval: Duration::from_secs(60),
                ..test_config()
            },
            &stop,
            &mut publish,
        );
        assert_eq!(final_state.phase, AutoLandPhase::Stopped);
        let log = log.into_inner();
        match &log[0].phase {
            AutoLandPhase::Waiting { attention, reasons } => {
                assert!(*attention, "changes requested needs the user");
                assert!(
                    reasons.iter().any(|r| r.contains("changes were requested")),
                    "{reasons:?}"
                );
            }
            other => panic!("expected waiting, got {other:?}"),
        }
    }

    #[test]
    fn supervises_a_github_side_merge_to_the_end() {
        let mut land_states = HashMap::new();
        land_states.insert(
            1,
            PrLandState {
                checks: ChecksRollup::Pending,
                ..ready_state()
            },
        );
        let sim = Sim::new(World {
            snapshot: single_stack(),
            prs: vec![open_pr(1, "auth", "main")],
            land_states,
            merged: HashMap::new(),
            prs_fail: false,
            merge_fails: false,
            landings: HashMap::new(),
            after_fetch: None,
            fetch_fails: 0,
        });
        // GitHub's auto-merge finishes before the second poll: the open PR
        // disappears and the merged-PR recognition finds it.
        sim.at_poll(1, |world| {
            world.prs.clear();
            world.merged.insert("auth".into(), merged_pr(1, "auth"));
        });

        let (state, log) = run(&sim, "auth", &test_config(), &StopSignal::new());
        assert_eq!(state.phase, AutoLandPhase::Done);
        // Two rounds: the hand-off, then the reconcile once GitHub merged.
        assert_eq!(state.rounds, 2);
        assert_eq!(state.merged.len(), 1);
        assert_eq!(state.merged[0].number, 1);
        let calls = sim.calls.borrow();
        assert!(
            calls.iter().any(|c| c.starts_with("auto_merge:")),
            "{calls:?}"
        );
        assert!(
            !calls.iter().any(|c| c.starts_with("merge:")),
            "GitHub drove the merge, not the job: {calls:?}"
        );
        // Between the rounds the job said GitHub was driving.
        let waited_on_github = log.iter().any(|s| {
            matches!(
                &s.phase,
                AutoLandPhase::Waiting { attention: false, reasons }
                    if reasons.iter().any(|r| r.contains("GitHub is driving"))
            )
        });
        assert!(waited_on_github, "{log:?}");
    }

    #[test]
    fn lands_a_two_segment_stack_across_rounds() {
        let mut land_states = HashMap::new();
        land_states.insert(1, ready_state());
        land_states.insert(
            7,
            PrLandState {
                base_branch: "auth".into(),
                ..ready_state()
            },
        );
        let sim = Sim::new(World {
            snapshot: two_stack(),
            prs: vec![open_pr(1, "auth", "main"), open_pr(7, "profile", "auth")],
            land_states,
            merged: HashMap::new(),
            prs_fail: false,
            merge_fails: false,
            landings: HashMap::new(),
            after_fetch: None,
            fetch_fails: 0,
        });
        // After the first round reconciled, the world looks freshly
        // rebased: profile alone on the new trunk, its PR retargeted and
        // re-checked.
        sim.at_poll(1, |world| {
            world.snapshot = after_first_land();
            world.prs = vec![open_pr(7, "profile", "main")];
            world.land_states.insert(7, ready_state());
        });

        let (state, log) = run(&sim, "profile", &test_config(), &StopSignal::new());
        assert_eq!(state.phase, AutoLandPhase::Done);
        assert_eq!(state.rounds, 2);
        assert_eq!(
            state.merged.iter().map(|m| m.number).collect::<Vec<_>>(),
            vec![1, 7]
        );
        // The wait between the rounds names the automated continue.
        let continued = log.iter().any(|s| {
            matches!(
                &s.phase,
                AutoLandPhase::Waiting { reasons, .. }
                    if reasons.iter().any(|r| r.contains("Landed a round"))
            )
        });
        assert!(continued, "{log:?}");
        let calls = sim.calls.borrow();
        assert!(calls.contains(&"merge:1:squash".to_owned()), "{calls:?}");
        assert!(calls.contains(&"merge:7:squash".to_owned()), "{calls:?}");
        assert!(calls.contains(&"retarget:7:main".to_owned()), "{calls:?}");
    }

    #[test]
    fn a_merge_that_did_not_land_asks_for_attention_and_touches_nothing() {
        // Reproduced on GitHub: a reordered push closed #1 as merged into
        // the branch above it. The old job recognized "merged", ran the
        // reconcile, and dropped the change.
        let sim = Sim::new(World {
            snapshot: two_stack(),
            prs: vec![open_pr(7, "profile", "main")],
            land_states: HashMap::new(),
            merged: HashMap::from([(
                "auth".to_owned(),
                PrSummary {
                    base_branch: "profile".into(),
                    merge_commit: Some("ca2feedface".into()),
                    ..merged_pr(1, "auth")
                },
            )]),
            prs_fail: false,
            merge_fails: false,
            landings: HashMap::from([("ca2feedface".to_owned(), LandedCheck::Elsewhere)]),
            after_fetch: None,
            fetch_fails: 0,
        });
        let (state, log) = run(&sim, "profile", &test_config(), &StopSignal::new());
        // The job keeps watching (publishing again resumes it) and says
        // what is wrong every time; the safety stop ends the test.
        assert_eq!(state.phase, AutoLandPhase::Stopped);
        assert!(log.iter().all(|s| match &s.phase {
            AutoLandPhase::Waiting { attention, reasons } => {
                reasons == &["Sizing up the stack".to_owned()]
                    || (*attention
                        && reasons
                            .iter()
                            .any(|r| r.contains("merged into \u{201c}profile\u{201d}, not \u{201c}main\u{201d}")))
            }
            AutoLandPhase::Stopped => true,
            _ => false,
        }));
        // No round ran, nothing is listed as landed, nothing was touched.
        assert_eq!(state.rounds, 0);
        assert!(state.merged.is_empty(), "{:?}", state.merged);
        assert!(sim.calls.borrow().is_empty(), "{:?}", sim.calls);
    }

    #[test]
    fn a_landing_counts_only_once_the_check_found_it_on_trunk() {
        // GitHub says #1 merged into main as a commit not fetched yet; the
        // round fetches, and the repository then says trunk lacks part of
        // it. The pull request must not be listed as landed.
        let sim = Sim::new(World {
            snapshot: single_stack(),
            prs: vec![],
            land_states: HashMap::new(),
            merged: HashMap::from([("auth".to_owned(), merged_pr(1, "auth"))]),
            prs_fail: false,
            merge_fails: false,
            landings: HashMap::from([("landed1".to_owned(), LandedCheck::Unfetched)]),
            after_fetch: None,
            fetch_fails: 0,
        });
        // Once the round has fetched, the repository knows the commit.
        sim.at_poll(1, |world| {
            world.landings.insert(
                "landed1".to_owned(),
                LandedCheck::Missing { paths: vec!["auth.rs".into()], more: 0 },
            );
        });
        let (state, log) = run(&sim, "auth", &test_config(), &StopSignal::new());
        assert_eq!(state.phase, AutoLandPhase::Stopped);
        assert_eq!(state.rounds, 1);
        assert!(state.merged.is_empty(), "{:?}", state.merged);
        // The failed round's story, then the plan's own blocker.
        assert!(log.iter().any(|s| matches!(
            &s.phase,
            AutoLandPhase::Waiting { attention: true, reasons }
                if reasons.iter().any(|r| r.contains("the fetch did not bring that commit"))
        )));
        assert!(log.iter().any(|s| matches!(
            &s.phase,
            AutoLandPhase::Waiting { attention: true, reasons }
                if reasons.iter().any(|r| r.contains("does not hold everything under \u{201c}auth\u{201d} (auth.rs)"))
        )));
        // The one round fetched and stopped at the check.
        assert_eq!(sim.calls.borrow().as_slice(), ["fetch:origin"]);
    }

    #[test]
    fn lands_a_github_stack_in_one_round() {
        let seat = |position| {
            Some(crate::pr::StackSeat { number: 90, position, size: 2, base: "main".into() })
        };
        let mut land_states = HashMap::new();
        land_states.insert(1, PrLandState { stack: seat(1), ..ready_state() });
        land_states.insert(
            7,
            PrLandState { stack: seat(2), base_branch: "auth".into(), ..ready_state() },
        );
        let sim = Sim::new(World {
            snapshot: two_stack(),
            prs: vec![
                PrSummary { stack: seat(1), ..open_pr(1, "auth", "main") },
                PrSummary { stack: seat(2), ..open_pr(7, "profile", "auth") },
            ],
            land_states,
            merged: HashMap::new(),
            prs_fail: false,
            merge_fails: false,
            landings: HashMap::new(),
            after_fetch: None,
            fetch_fails: 0,
        });
        let (state, _) = run(&sim, "profile", &test_config(), &StopSignal::new());
        assert_eq!(state.phase, AutoLandPhase::Done);
        assert_eq!(state.rounds, 1);
        // Both landed, bottom first, from one request for the top.
        assert_eq!(
            state.merged.iter().map(|m| m.number).collect::<Vec<_>>(),
            vec![1, 7]
        );
        let calls = sim.calls.borrow();
        assert_eq!(
            calls.iter().filter(|c| c.starts_with("merge")).collect::<Vec<_>>(),
            ["merge_stacked:7:squash"]
        );
        assert!(!calls.iter().any(|c| c.starts_with("auto_merge")), "{calls:?}");
    }

    #[test]
    fn parks_after_the_error_budget() {
        let sim = Sim::new(World {
            snapshot: single_stack(),
            prs: vec![],
            land_states: HashMap::new(),
            merged: HashMap::new(),
            prs_fail: true,
            merge_fails: false,
            landings: HashMap::new(),
            after_fetch: None,
            fetch_fails: 0,
        });

        let config = AutoLandConfig {
            max_consecutive_errors: 3,
            ..test_config()
        };
        let (state, log) = run(&sim, "auth", &config, &StopSignal::new());
        match &state.phase {
            AutoLandPhase::Failed { message } => {
                assert!(message.contains("gave up after 3"), "{message}");
                assert!(message.contains("unreachable"), "{message}");
            }
            other => panic!("expected failed, got {other:?}"),
        }
        // The two waits before the budget ran out said it was retrying.
        assert!(matches!(
            &log[0].phase,
            AutoLandPhase::Waiting { attention: false, reasons }
                if reasons.iter().any(|r| r.contains("retrying"))
        ));
        assert_eq!(log.len(), 3);
    }

    #[test]
    fn failed_rounds_retry_then_park() {
        let mut land_states = HashMap::new();
        land_states.insert(1, ready_state());
        let sim = Sim::new(World {
            snapshot: single_stack(),
            prs: vec![open_pr(1, "auth", "main")],
            land_states,
            merged: HashMap::new(),
            prs_fail: false,
            merge_fails: true,
            landings: HashMap::new(),
            after_fetch: None,
            fetch_fails: 0,
        });

        let config = AutoLandConfig {
            max_consecutive_errors: 2,
            ..test_config()
        };
        let (state, log) = run(&sim, "auth", &config, &StopSignal::new());
        assert_eq!(state.rounds, 2, "each retry re-planned and re-ran");
        match &state.phase {
            AutoLandPhase::Failed { message } => {
                assert!(message.contains("could not finish"), "{message}");
                assert!(message.contains("merge refused"), "{message}");
            }
            other => panic!("expected failed, got {other:?}"),
        }
        // The retry wait carried the failed step's story and asked for
        // attention.
        let retried = log.iter().any(|s| {
            matches!(
                &s.phase,
                AutoLandPhase::Waiting { attention: true, reasons }
                    if reasons.iter().any(|r| r.contains("merge refused"))
            )
        });
        assert!(retried, "{log:?}");
        assert!(state.last_outcome.as_ref().unwrap().failed);
    }

    #[test]
    fn a_vanished_bookmark_parks_the_job() {
        let mut land_states = HashMap::new();
        land_states.insert(
            1,
            PrLandState {
                checks: ChecksRollup::Pending,
                auto_merge_allowed: false,
                ..ready_state()
            },
        );
        let sim = Sim::new(World {
            snapshot: single_stack(),
            prs: vec![open_pr(1, "auth", "main")],
            land_states,
            merged: HashMap::new(),
            prs_fail: false,
            merge_fails: false,
            landings: HashMap::new(),
            after_fetch: None,
            fetch_fails: 0,
        });
        sim.at_poll(1, |world| {
            world.snapshot.bookmarks.retain(|b| b.name != "auth");
        });

        let (state, _log) = run(&sim, "auth", &test_config(), &StopSignal::new());
        match &state.phase {
            AutoLandPhase::Failed { message } => {
                assert!(message.contains("no longer a bookmark"), "{message}");
            }
            other => panic!("expected failed, got {other:?}"),
        }
    }

    #[test]
    fn a_repo_switch_parks_the_job() {
        let mut land_states = HashMap::new();
        land_states.insert(
            1,
            PrLandState {
                checks: ChecksRollup::Pending,
                auto_merge_allowed: false,
                ..ready_state()
            },
        );
        let sim = Sim::new(World {
            snapshot: single_stack(),
            prs: vec![open_pr(1, "auth", "main")],
            land_states,
            merged: HashMap::new(),
            prs_fail: false,
            merge_fails: false,
            landings: HashMap::new(),
            after_fetch: None,
            fetch_fails: 0,
        });
        sim.at_poll(1, |world| {
            world.snapshot.repo_path = "/somewhere/else".into();
        });

        let (state, _log) = run(&sim, "auth", &test_config(), &StopSignal::new());
        match &state.phase {
            AutoLandPhase::Failed { message } => {
                assert!(message.contains("repository changed"), "{message}");
            }
            other => panic!("expected failed, got {other:?}"),
        }
    }

    fn waiting_record(head: &str) -> AutoLandRecord {
        AutoLandRecord {
            repo_path: "/tmp/repo".into(),
            state: AutoLandState {
                phase: AutoLandPhase::Waiting {
                    attention: false,
                    reasons: vec!["GitHub is driving the merge now".into()],
                },
                rounds: 2,
                merged: vec![AutoLandMerged {
                    number: 100,
                    url: "https://github.com/o/r/pull/100".into(),
                    bookmark: "profile".into(),
                }],
                ..AutoLandState::queued(head)
            },
            saved_at_ms: 1_751_500_000_000,
        }
    }

    #[test]
    fn records_round_trip_through_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("autoland.json");
        let auth = waiting_record("auth");
        let mut profile = waiting_record("profile");
        profile.repo_path = "/tmp/other".into();

        let mut records = vec![auth.clone(), profile.clone()];
        save_autoland_records(&path, &records).unwrap();
        assert_eq!(load_autoland_records(&path), records);
        // The atomic-write temp file does not linger.
        assert!(!path.with_extension("json.tmp").exists());

        // Upserting the same story overwrites in place — order stays put.
        let mut newer = auth.clone();
        newer.state.rounds = 3;
        upsert_autoland_record(&mut records, newer.clone());
        assert_eq!(records, vec![newer.clone(), profile.clone()]);
        save_autoland_records(&path, &records).unwrap();
        assert_eq!(load_autoland_records(&path), records);

        // A new story appends; removing takes only its own record along.
        upsert_autoland_record(&mut records, waiting_record("search"));
        assert_eq!(records.len(), 3);
        assert!(remove_autoland_record(&mut records, "/tmp/repo", "search"));
        assert!(!remove_autoland_record(&mut records, "/tmp/repo", "search"));
        // Same bookmark, different repo — not this story.
        assert!(!remove_autoland_record(&mut records, "/tmp/repo", "profile"));
        assert_eq!(records, vec![newer, profile]);

        // Saving an empty list removes the file; twice is fine.
        save_autoland_records(&path, &[]).unwrap();
        assert_eq!(load_autoland_records(&path), Vec::new());
        assert!(!path.exists());
        save_autoland_records(&path, &[]).unwrap();
    }

    #[test]
    fn broken_or_foreign_records_load_as_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("autoland.json");

        // No file at all.
        assert_eq!(load_autoland_records(&path), Vec::new());
        // Garbage on disk must never stop the app from starting.
        std::fs::write(&path, b"{ not json").unwrap();
        assert_eq!(load_autoland_records(&path), Vec::new());
        // A file from a different format version is dropped, not guessed at.
        let foreign = serde_json::json!({
            "version": AUTOLAND_RECORDS_VERSION + 1,
            "records": [waiting_record("auth")],
        });
        std::fs::write(&path, serde_json::to_vec(&foreign).unwrap()).unwrap();
        assert_eq!(load_autoland_records(&path), Vec::new());
    }

    #[test]
    fn a_version_one_file_lifts_into_a_one_record_list() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("autoland.json");
        let record = waiting_record("auth");

        // The retired single-record shape, byte-for-byte as version 1
        // wrote it (camelCase keys, the record wearing its own version).
        let v1 = serde_json::json!({
            "version": 1,
            "repoPath": record.repo_path,
            "state": record.state,
            "savedAtMs": record.saved_at_ms,
        });
        std::fs::write(&path, serde_json::to_vec(&v1).unwrap()).unwrap();
        assert_eq!(load_autoland_records(&path), vec![record.clone()]);

        // A single-record file claiming any other version stays dropped.
        let stale = serde_json::json!({
            "version": 3,
            "repoPath": record.repo_path,
            "state": record.state,
            "savedAtMs": record.saved_at_ms,
        });
        std::fs::write(&path, serde_json::to_vec(&stale).unwrap()).unwrap();
        assert_eq!(load_autoland_records(&path), Vec::new());
    }

    #[test]
    fn resumed_state_keeps_progress_and_resets_the_phase() {
        let prior = AutoLandState {
            phase: AutoLandPhase::Waiting {
                attention: true,
                reasons: vec!["the checks are failing".into()],
            },
            ..waiting_record("auth").state
        };
        let resumed = AutoLandState::resumed(prior.clone());
        assert_eq!(resumed.rounds, prior.rounds);
        assert_eq!(resumed.merged, prior.merged);
        assert_eq!(resumed.head_bookmark, "auth");
        // The old phase rested on stale facts; a resumed job sizes up fresh.
        match &resumed.phase {
            AutoLandPhase::Waiting { attention, reasons } => {
                assert!(!attention);
                assert!(reasons.iter().any(|r| r.contains("Sizing up")), "{reasons:?}");
            }
            other => panic!("expected waiting, got {other:?}"),
        }
    }

    #[test]
    fn a_resumed_job_carries_prior_progress_through_to_done() {
        // The world is ready to land the remaining `auth` segment; the
        // resumed state remembers #100 already landed in 2 earlier rounds
        // (before the restart).
        let mut land_states = HashMap::new();
        land_states.insert(1, ready_state());
        let sim = Sim::new(World {
            snapshot: single_stack(),
            prs: vec![open_pr(1, "auth", "main")],
            land_states,
            merged: HashMap::new(),
            prs_fail: false,
            merge_fails: false,
            landings: HashMap::new(),
            after_fetch: None,
            fetch_fails: 0,
        });

        let initial = AutoLandState::resumed(waiting_record("auth").state);
        let (state, _log) = run_from(&sim, initial, &test_config(), &StopSignal::new());
        assert_eq!(state.phase, AutoLandPhase::Done);
        assert_eq!(state.rounds, 3, "rounds accumulate across the restart");
        assert_eq!(
            state
                .merged
                .iter()
                .map(|m| m.number)
                .collect::<Vec<_>>(),
            vec![100, 1],
            "the pre-restart merge stays first in the story"
        );
    }

    #[test]
    fn a_branch_someone_else_pushed_to_is_fetched_once_and_then_landed() {
        // A bot added a commit to #1's branch. The plan a person would
        // get stops there ("fetch and look"). The job fetches, so the
        // repository has what GitHub has, and lands what is there.
        let mut land_states = HashMap::new();
        land_states.insert(
            1,
            PrLandState { head_commit: "cbot0123".into(), ..ready_state() },
        );
        let followed = snapshot(
            vec![
                node("bot", "style: format", NodeKind::WorkingCopy, &["a2"], &["auth"]),
                node("a2", "auth: sessions", NodeKind::Mutable, &["a1"], &[]),
                node("a1", "auth: login flow", NodeKind::Mutable, &["m"], &[]),
                node("m", "release", NodeKind::Immutable, &[], &["main"]),
            ],
            vec![
                bookmark("main", "m", SyncState::Synced, true),
                bookmark("auth", "bot", SyncState::Synced, false),
            ],
        );
        let world = |after_fetch: Option<RepoSnapshot>, fetch_fails: u32| World {
            snapshot: single_stack(),
            prs: vec![PrSummary { head_commit: "cbot0123".into(), ..open_pr(1, "auth", "main") }],
            land_states: land_states.clone(),
            merged: HashMap::new(),
            prs_fail: false,
            merge_fails: false,
            landings: HashMap::new(),
            after_fetch,
            fetch_fails,
        };

        let sim = Sim::new(world(Some(followed.clone()), 0));
        let (state, _) = run(&sim, "auth", &test_config(), &StopSignal::new());
        assert_eq!(state.phase, AutoLandPhase::Done, "{:?}", state.phase);
        assert_eq!(state.merged.len(), 1);
        let calls = sim.calls.borrow();
        assert_eq!(&calls[..2], ["fetch:origin", "merge:1:squash"], "{calls:?}");
        // What landed is all three commits, the bot's included, and the
        // round's story begins with how they got here.
        assert!(calls.contains(&"abandon:cbot,ca2,ca1".to_owned()), "{calls:?}");
        let first = &state.last_outcome.as_ref().unwrap().steps[0];
        assert!(matches!(first.action, LandAction::FetchRemote { .. }));
        assert!(
            first.detail.as_deref().unwrap().contains("had moved on GitHub since the last fetch"),
            "{:?}",
            first.detail
        );

        // A fetch that does not bring the bookmark along (it had moved
        // here too) leaves the blocker standing: the job asks for the
        // user, and does not fetch again for the same reason.
        let sim = Sim::new(world(None, 0));
        let stop = StopSignal::new();
        let polls = Cell::new(0);
        let log: RefCell<Vec<AutoLandState>> = RefCell::new(vec![]);
        let mut publish = |state: &AutoLandState| {
            log.borrow_mut().push(state.clone());
            polls.set(polls.get() + 1);
            if polls.get() >= 3 {
                stop.stop();
            }
        };
        run_autoland(
            &forge_repo(),
            AutoLandState::queued("auth"),
            &sim,
            &sim,
            &sim,
            &test_config(),
            &stop,
            &mut publish,
        );
        let log = log.into_inner();
        assert!(
            matches!(&log[0].phase, AutoLandPhase::Waiting { attention: true, reasons }
                if reasons[0].contains("#1's branch on GitHub is at")),
            "{:?}",
            log[0].phase
        );
        assert_eq!(
            sim.calls.borrow().iter().filter(|c| c.starts_with("fetch")).count(),
            1,
            "{:?}",
            sim.calls
        );
        assert!(!sim.calls.borrow().iter().any(|c| c.starts_with("merge")));

        // A fetch that fails is tried again on the next poll, like any
        // other failure, and the job goes on once it gets through.
        let sim = Sim::new(world(Some(followed), 1));
        let (state, log) = run(&sim, "auth", &test_config(), &StopSignal::new());
        assert_eq!(state.phase, AutoLandPhase::Done, "{:?}", state.phase);
        assert!(
            matches!(&log[0].phase, AutoLandPhase::Waiting { attention: false, reasons }
                if reasons[0].contains("the remote hung up") && reasons[0].contains("retrying")),
            "{:?}",
            log[0].phase
        );
        let calls = sim.calls.borrow();
        assert_eq!(&calls[..3], ["fetch:origin", "fetch:origin", "merge:1:squash"], "{calls:?}");
    }

    #[test]
    fn a_branch_cut_back_on_github_ends_the_job_instead_of_landing_what_is_left() {
        // Someone force-pushed #1's branch back to its first commit. The
        // plan sees GitHub's head differ from the bookmark and the job
        // fetches; jj then drops the second commit here too, and from the
        // next poll on the bookmark and GitHub would agree.
        let mut land_states = HashMap::new();
        land_states.insert(
            1,
            PrLandState { head_commit: "ca1feedface".into(), ..ready_state() },
        );
        let cut_back = snapshot(
            vec![
                node("a1", "auth: login flow", NodeKind::WorkingCopy, &["m"], &["auth"]),
                node("m", "release", NodeKind::Immutable, &[], &["main"]),
            ],
            vec![
                bookmark("main", "m", SyncState::Synced, true),
                bookmark("auth", "a1", SyncState::Synced, false),
            ],
        );
        let sim = Sim::new(World {
            snapshot: single_stack(),
            prs: vec![PrSummary { head_commit: "ca1feedface".into(), ..open_pr(1, "auth", "main") }],
            land_states,
            merged: HashMap::new(),
            prs_fail: false,
            merge_fails: false,
            landings: HashMap::new(),
            after_fetch: Some(cut_back),
            fetch_fails: 0,
        });
        let (state, _) = run(&sim, "auth", &test_config(), &StopSignal::new());
        assert!(
            matches!(&state.phase, AutoLandPhase::Failed { message }
                if message.contains("rewritten or cut back on GitHub")
                    && message.contains("a2 is no longer in the stack")),
            "{:?}",
            state.phase
        );
        assert_eq!(sim.calls.borrow().as_slice(), ["fetch:origin"]);
        assert!(state.merged.is_empty());

        // The same when another bookmark holds the dropped commit, so jj
        // keeps it: it is above "auth" now, not under it, and #1 no
        // longer has it.
        let mut land_states = HashMap::new();
        land_states.insert(
            1,
            PrLandState { head_commit: "ca1feedface".into(), ..ready_state() },
        );
        let held = snapshot(
            vec![
                node("a2", "auth: sessions", NodeKind::WorkingCopy, &["a1"], &["later"]),
                node("a1", "auth: login flow", NodeKind::Mutable, &["m"], &["auth"]),
                node("m", "release", NodeKind::Immutable, &[], &["main"]),
            ],
            vec![
                bookmark("main", "m", SyncState::Synced, true),
                bookmark("auth", "a1", SyncState::Synced, false),
                bookmark("later", "a2", SyncState::LocalOnly, false),
            ],
        );
        let mut before = single_stack();
        before.bookmarks.push(bookmark("later", "a2", SyncState::LocalOnly, false));
        let sim = Sim::new(World {
            snapshot: before,
            prs: vec![PrSummary { head_commit: "ca1feedface".into(), ..open_pr(1, "auth", "main") }],
            land_states,
            merged: HashMap::new(),
            prs_fail: false,
            merge_fails: false,
            landings: HashMap::new(),
            after_fetch: Some(held),
            fetch_fails: 0,
        });
        let (state, _) = run(&sim, "auth", &test_config(), &StopSignal::new());
        assert!(
            matches!(&state.phase, AutoLandPhase::Failed { message }
                if message.contains("a2 is no longer in the stack")),
            "{:?}",
            state.phase
        );
        assert_eq!(sim.calls.borrow().as_slice(), ["fetch:origin"]);
    }

    /// The cut-back branch can also arrive by a fetch the job did not
    /// make (the app's own, or `jj git fetch` in a terminal): the bookmark
    /// and GitHub agree by the job's next look, and nothing asks it to
    /// fetch. The job remembers what it was asked to land, across looks
    /// and across a restart, and ends when a change of a branch that is
    /// still to land is no longer under the stack.
    #[test]
    fn a_branch_cut_back_between_looks_or_before_a_restart_ends_the_job() {
        let waiting = || PrLandState {
            checks: ChecksRollup::Pending,
            auto_merge_allowed: false,
            ..ready_state()
        };
        fn cut_back() -> RepoSnapshot {
            snapshot(
                vec![
                    node("a1", "auth: login flow", NodeKind::WorkingCopy, &["m"], &["auth"]),
                    node("m", "release", NodeKind::Immutable, &[], &["main"]),
                ],
                vec![
                    bookmark("main", "m", SyncState::Synced, true),
                    bookmark("auth", "a1", SyncState::Synced, false),
                ],
            )
        }
        let shorter_and_ready = |world: &mut World| {
            world.snapshot = cut_back();
            world.prs = vec![PrSummary { head_commit: "ca1feedface".into(), ..open_pr(1, "auth", "main") }];
            world.land_states.insert(
                1,
                PrLandState { head_commit: "ca1feedface".into(), ..ready_state() },
            );
        };
        let world = |snapshot: RepoSnapshot, state: PrLandState| World {
            snapshot,
            prs: vec![open_pr(1, "auth", "main")],
            land_states: HashMap::from([(1, state)]),
            merged: HashMap::new(),
            prs_fail: false,
            merge_fails: false,
            landings: HashMap::new(),
            after_fetch: None,
            fetch_fails: 0,
        };

        // Between two looks. The first one waits on checks and lists a1
        // and a2 under "auth".
        let sim = Sim::new(world(single_stack(), waiting()));
        sim.at_poll(1, shorter_and_ready);
        let (state, log) = run(&sim, "auth", &test_config(), &StopSignal::new());
        assert!(
            matches!(&state.phase, AutoLandPhase::Failed { message }
                if message.contains("\u{201c}auth\u{201d} no longer holds a2")
                    && message.contains("here or on GitHub")),
            "{:?}",
            state.phase
        );
        assert!(sim.calls.borrow().iter().all(|c| !c.starts_with("merge")), "{:?}", sim.calls);
        assert!(state.merged.is_empty());

        // Across a restart: the saved job, resumed against the shorter
        // stack.
        let saved = log
            .iter()
            .find(|s| s.segments.iter().any(|seg| seg.change_ids == ["a1", "a2"]))
            .cloned()
            .expect("the first look saved what it saw");
        let mut later = world(cut_back(), ready_state());
        shorter_and_ready(&mut later);
        let sim = Sim::new(later);
        let (state, _) =
            run_from(&sim, AutoLandState::resumed(saved), &test_config(), &StopSignal::new());
        assert!(
            matches!(&state.phase, AutoLandPhase::Failed { message }
                if message.contains("no longer holds a2")),
            "{:?}",
            state.phase
        );
        assert!(sim.calls.borrow().is_empty(), "{:?}", sim.calls);
    }

    /// What the job is asked about is the branches still to land. One
    /// that someone else landed and cleaned up between two looks took its
    /// changes with it, and that ends nothing.
    #[test]
    fn a_branch_someone_else_landed_between_looks_does_not_end_the_job() {
        let mut land_states = HashMap::new();
        land_states.insert(
            1,
            PrLandState {
                checks: ChecksRollup::Pending,
                auto_merge_allowed: false,
                ..ready_state()
            },
        );
        land_states.insert(7, PrLandState { base_branch: "auth".into(), ..ready_state() });
        let sim = Sim::new(World {
            snapshot: two_stack(),
            prs: vec![open_pr(1, "auth", "main"), open_pr(7, "profile", "auth")],
            land_states,
            merged: HashMap::new(),
            prs_fail: false,
            merge_fails: false,
            landings: HashMap::new(),
            after_fetch: None,
            fetch_fails: 0,
        });
        sim.at_poll(1, |world| {
            world.snapshot = after_first_land();
            world.prs = vec![open_pr(7, "profile", "main")];
            world.land_states.insert(7, ready_state());
            // Landed: GitHub calls #1 merged.
            world.merged.insert("auth".to_owned(), merged_pr(1, "auth"));
        });
        let (state, _) = run(&sim, "profile", &test_config(), &StopSignal::new());
        assert_eq!(state.phase, AutoLandPhase::Done, "{:?}", state.phase);
        assert_eq!(state.merged.iter().map(|m| m.number).collect::<Vec<_>>(), vec![7]);
    }

    /// A branch can also leave the stack without landing. While "auth"
    /// waits on its checks, "profile" is moved off it onto trunk and its
    /// pull request pointed there. The stack under "profile" is then
    /// "profile" alone, and landing it would end the job as done with
    /// #1 open and its changes where they were. The job was asked to land
    /// both: it ends and says which one is gone, at the next look and
    /// when resumed.
    #[test]
    fn a_branch_that_left_the_stack_without_landing_ends_the_job() {
        fn reshaped() -> RepoSnapshot {
            let mut answer = snapshot(
                vec![
                    node("b1", "profile: avatars", NodeKind::WorkingCopy, &["m"], &["profile"]),
                    node("a2", "auth: sessions", NodeKind::Mutable, &["a1"], &["auth"]),
                    node("a1", "auth: login flow", NodeKind::Mutable, &["m"], &[]),
                    node("m", "release", NodeKind::Immutable, &[], &["main"]),
                ],
                vec![
                    bookmark("main", "m", SyncState::Synced, true),
                    bookmark("auth", "a2", SyncState::Synced, false),
                    bookmark("profile", "b1", SyncState::Synced, false),
                ],
            );
            answer.nodes[0].commit_id = "ca2feedface".into();
            answer
        }
        let waiting = PrLandState {
            checks: ChecksRollup::Pending,
            auto_merge_allowed: false,
            ..ready_state()
        };
        let world = |snapshot: RepoSnapshot, first: PrLandState| World {
            snapshot,
            prs: vec![open_pr(1, "auth", "main"), open_pr(7, "profile", "auth")],
            land_states: HashMap::from([
                (1, first),
                (7, PrLandState { base_branch: "auth".into(), ..ready_state() }),
            ]),
            merged: HashMap::new(),
            prs_fail: false,
            merge_fails: false,
            landings: HashMap::new(),
            after_fetch: None,
            fetch_fails: 0,
        };
        let moved_off = |world: &mut World| {
            world.snapshot = reshaped();
            world.prs = vec![open_pr(1, "auth", "main"), open_pr(7, "profile", "main")];
            world.land_states.insert(7, ready_state());
        };

        let sim = Sim::new(world(two_stack(), waiting.clone()));
        sim.at_poll(1, moved_off);
        let (state, log) = run(&sim, "profile", &test_config(), &StopSignal::new());
        assert!(
            matches!(&state.phase, AutoLandPhase::Failed { message }
                if message.contains("\u{201c}auth\u{201d} is no longer part of the stack under \u{201c}profile\u{201d}")
                    && message.contains("it has not landed")
                    && message.contains("without a1, a2")),
            "{:?}",
            state.phase
        );
        assert!(sim.calls.borrow().iter().all(|c| !c.starts_with("merge")), "{:?}", sim.calls);
        assert!(state.merged.is_empty());

        // Resumed against the reshaped stack.
        let saved = log
            .iter()
            .find(|s| s.segments.iter().any(|seg| seg.bookmark == "auth"))
            .cloned()
            .expect("the first look saved both branches");
        let mut later = world(two_stack(), waiting.clone());
        moved_off(&mut later);
        let sim = Sim::new(later);
        let (state, _) =
            run_from(&sim, AutoLandState::resumed(saved), &test_config(), &StopSignal::new());
        assert!(
            matches!(&state.phase, AutoLandPhase::Failed { message }
                if message.contains("is no longer part of the stack")),
            "{:?}",
            state.phase
        );
        assert!(sim.calls.borrow().iter().all(|c| !c.starts_with("merge")), "{:?}", sim.calls);

        // What counts as landed is the pull request the job recorded, #1,
        // merged into trunk. Not an earlier pull request that once merged
        // from a branch of the same name:
        let mut reused = world(two_stack(), waiting.clone());
        reused.merged.insert("auth".to_owned(), merged_pr(3, "auth"));
        let sim = Sim::new(reused);
        sim.at_poll(1, moved_off);
        let (state, _) = run(&sim, "profile", &test_config(), &StopSignal::new());
        assert!(
            matches!(&state.phase, AutoLandPhase::Failed { message }
                if message.contains("it has not landed")),
            "{:?}",
            state.phase
        );
        assert!(sim.calls.borrow().iter().all(|c| !c.starts_with("merge")), "{:?}", sim.calls);
        // And not #1 itself merged into some other branch.
        let sim = Sim::new(world(two_stack(), waiting));
        sim.at_poll(1, |world| {
            world.snapshot = reshaped();
            world.prs = vec![open_pr(7, "profile", "main")];
            world.land_states.insert(7, ready_state());
            world.merged.insert(
                "auth".to_owned(),
                PrSummary { base_branch: "integration".into(), ..merged_pr(1, "auth") },
            );
        });
        let (state, _) = run(&sim, "profile", &test_config(), &StopSignal::new());
        assert!(
            matches!(&state.phase, AutoLandPhase::Failed { message }
                if message.contains("it has not landed")),
            "{:?}",
            state.phase
        );
        assert!(sim.calls.borrow().iter().all(|c| !c.starts_with("merge")), "{:?}", sim.calls);
    }

    #[test]
    fn a_job_resumed_after_a_merge_commit_landing_finishes_it() {
        // The round merged #1 with a merge commit and fetched, and the
        // job stopped there. "auth" now points at trunk history: there is
        // no mutable stack under it. Resumed, the job used to read that
        // as an error and give up after its retries with nothing counted.
        let landed = snapshot(
            vec![
                node("mm", "Merge pull request #1", NodeKind::Immutable, &["m", "a2"], &["main"]),
                node("a2", "auth: sessions", NodeKind::Immutable, &["a1"], &["auth"]),
                node("a1", "auth: login flow", NodeKind::Immutable, &["m"], &[]),
                node("m", "release", NodeKind::Immutable, &[], &[]),
            ],
            vec![
                bookmark("main", "mm", SyncState::Synced, true),
                bookmark("auth", "a2", SyncState::Synced, false),
            ],
        );
        let sim = Sim::new(World {
            snapshot: landed,
            prs: vec![],
            land_states: HashMap::new(),
            merged: HashMap::from([("auth".to_owned(), merged_pr(1, "auth"))]),
            prs_fail: false,
            merge_fails: false,
            landings: HashMap::new(),
            after_fetch: None,
            fetch_fails: 0,
        });
        let prior = AutoLandState { rounds: 1, ..AutoLandState::queued("auth") };
        let (state, _) = run_from(
            &sim,
            AutoLandState::resumed(prior),
            &test_config(),
            &StopSignal::new(),
        );
        assert_eq!(state.phase, AutoLandPhase::Done, "{:?}", state.phase);
        assert_eq!(
            state.merged,
            vec![AutoLandMerged {
                number: 1,
                url: "https://github.com/o/r/pull/1".into(),
                bookmark: "auth".into(),
            }]
        );
        assert_eq!(
            sim.calls.borrow().as_slice(),
            ["fetch:origin", "delete:auth", "push:auth:origin"],
            "the bookmark is cleaned up and nothing immutable is abandoned"
        );

        // The same for a job saved with what it had been asked to land,
        // when the snapshot no longer draws the landed commits at all (it
        // draws nothing inside trunk's history that no mutable change
        // sits on). They landed. They were not cut back.
        let mut undrawn = snapshot(
            vec![
                node("mm", "Merge pull request #1", NodeKind::Immutable, &["m"], &["main"]),
                node("m", "release", NodeKind::Immutable, &[], &[]),
            ],
            vec![
                bookmark("main", "mm", SyncState::Synced, true),
                bookmark("auth", "a2", SyncState::Synced, false),
            ],
        );
        undrawn.bookmarks[1].is_merged = true;
        let sim = Sim::new(World {
            snapshot: undrawn,
            prs: vec![],
            land_states: HashMap::new(),
            merged: HashMap::from([("auth".to_owned(), merged_pr(1, "auth"))]),
            prs_fail: false,
            merge_fails: false,
            landings: HashMap::new(),
            after_fetch: None,
            fetch_fails: 0,
        });
        let prior = AutoLandState {
            rounds: 1,
            segments: vec![LandSegment {
                bookmark: "auth".into(),
                change_ids: vec!["a1".into(), "a2".into()],
                pr: None,
                status: LandSegmentStatus::Landing,
            }],
            ..AutoLandState::queued("auth")
        };
        let (state, _) = run_from(
            &sim,
            AutoLandState::resumed(prior),
            &test_config(),
            &StopSignal::new(),
        );
        assert_eq!(state.phase, AutoLandPhase::Done, "{:?}", state.phase);
        assert!(sim.calls.borrow().contains(&"delete:auth".to_owned()), "{:?}", sim.calls);
    }
}
