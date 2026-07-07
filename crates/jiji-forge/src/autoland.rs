//! The auto-land job engine: jjpr's `watch` loop as a supervised state
//! machine over the land engine's plan → execute round (see the jjpr
//! inspiration note). A queued job keeps re-deriving the landing plan from
//! fresh local and GitHub facts, runs a round when the plan has actions,
//! and waits when it does not — so the M4 land flow's deliberate
//! one-merge-per-run becomes the automated continue the product spec's
//! auto-land workflow describes.
//!
//! The engine stays headless and synchronous like the rest of the crate:
//! the host owns the thread (the Tauri app spawns one per job; a future
//! CLI can drive the same loop from `main`), a [`StopSignal`] makes the
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
//! Job state also persists (the M5 persistence slice): [`AutoLandRecord`]
//! is the one-job record a host saves on every publish and loads at the
//! next launch, so a waiting or parked job survives a restart as cleanly
//! resumable status. The format and file IO live here — plain JSON
//! written atomically via a sibling temp file and rename — so a future
//! CLI persists the exact same record; the host owns only where the file
//! lives and when to save. Deliberately no database crate: the concrete
//! requirement is one small human-debuggable record written at most once
//! per poll, which a JSON file meets outright (revisit if multi-job
//! bookkeeping or job history outgrows it).
//!
//! Deliberately deferred (later M5 slices): more than one job at a time,
//! and queue-position/ETA supervision beyond polling until GitHub's
//! automation finishes.

use std::io;
use std::path::Path;
use std::sync::{Condvar, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::error::ForgeError;
use crate::land::{
    execute_land, plan_land, LandAction, LandForge, LandOutcome, LandPlan, LandSegment,
    LandSegmentStatus, LandVcs,
};
use crate::pr::RepoPrState;
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
/// condvar gives the same behavior without the latency).
#[derive(Default)]
pub struct StopSignal {
    stopped: Mutex<bool>,
    bell: Condvar,
}

impl StopSignal {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn stop(&self) {
        *self.stopped.lock().expect("stop signal lock poisoned") = true;
        self.bell.notify_all();
    }

    pub fn is_stopped(&self) -> bool {
        *self.stopped.lock().expect("stop signal lock poisoned")
    }

    /// Sleep up to `duration`, waking early on `stop()`. Answers whether
    /// the job was stopped.
    fn wait(&self, duration: Duration) -> bool {
        let stopped = self.stopped.lock().expect("stop signal lock poisoned");
        let (stopped, _) = self
            .bell
            .wait_timeout_while(stopped, duration, |stopped| !*stopped)
            .expect("stop signal lock poisoned");
        *stopped
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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "camelCase")]
#[ts(export)]
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

/// A PR the job saw land — merged by a round or recognized as merged on
/// GitHub (auto-merge, the queue, or someone clicking the button).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct AutoLandMerged {
    pub number: u64,
    pub url: String,
    pub bookmark: String,
}

/// The job as a host renders it: published on every phase change and
/// answered whole when a surface reconnects.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct AutoLandState {
    pub head_bookmark: String,
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

/// The persisted-record format version: a file written by a different
/// version loads as nothing rather than being guessed at.
pub const AUTOLAND_RECORD_VERSION: u32 = 1;

/// The one-job record a host persists: the state plus which repo it
/// belongs to and when it was saved. Saved on every publish and loaded at
/// the next launch, so a waiting or parked job survives a restart as
/// resumable status; status only renders when the same repo is open
/// again, which is why the repo path rides along.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct AutoLandRecord {
    pub version: u32,
    /// The repo the job was queued in (`RepoSnapshot::repo_path`).
    pub repo_path: String,
    pub state: AutoLandState,
    /// Unix milliseconds of the save — the "as of" behind a restored
    /// status.
    pub saved_at_ms: u64,
}

/// A record as a host answers it to a (re)connecting surface: `live`
/// distinguishes a record a thread is driving right now from one that
/// survived from an earlier session (or whose thread died) — the UI's
/// "interrupted — resume?" state is a non-terminal record that is not
/// live.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
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

/// Save the record atomically: write a sibling temp file, then rename it
/// over the target, so a crash mid-write leaves the previous record
/// intact instead of half a JSON document.
pub fn save_autoland_record(path: &Path, record: &AutoLandRecord) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let json = serde_json::to_vec_pretty(record).map_err(io::Error::other)?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, &json)?;
    std::fs::rename(&tmp, path)
}

/// Load a saved record. Anything wrong — no file, unreadable JSON, a
/// different format version — answers `None`: a state file must never
/// stop the app from starting, and stale-format records are dropped
/// rather than guessed at.
pub fn load_autoland_record(path: &Path) -> Option<AutoLandRecord> {
    let bytes = std::fs::read(path).ok()?;
    let record: AutoLandRecord = serde_json::from_slice(&bytes).ok()?;
    (record.version == AUTOLAND_RECORD_VERSION).then_some(record)
}

/// Remove the saved record — dismissing it. A missing file is fine.
pub fn clear_autoland_record(path: &Path) -> io::Result<()> {
    match std::fs::remove_file(path) {
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
        other => other,
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

    let plan = match plan_land(&snapshot, &pr_state, repo, &state.head_bookmark, forge) {
        Ok(plan) => plan,
        Err(err) => return transient(config, consecutive_errors, err.to_string()),
    };
    state.segments = plan.segments.clone();
    // Merges recognized at plan time (GitHub's automation or a manual
    // merge finishing) are progress worth recording before any round runs.
    for segment in &plan.segments {
        if let LandSegmentStatus::Merged { number, url } = &segment.status {
            note_merged(state, *number, url, &segment.bookmark);
        }
    }

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
    let outcome = match execute_land(&plan, vcs, forge) {
        Ok(outcome) => outcome,
        Err(err) => return transient(config, consecutive_errors, err.to_string()),
    };
    state.rounds += 1;
    // A merge that went through is progress even when a later step failed.
    for step in &outcome.steps {
        if step.status != SubmitStepStatus::Done {
            continue;
        }
        if let LandAction::MergePr {
            number, bookmark, ..
        } = &step.action
        {
            let url = plan
                .segments
                .iter()
                .find(|s| s.bookmark == *bookmark)
                .and_then(|s| s.pr.as_ref())
                .map(|pr| pr.url.clone())
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
    } else {
        "Landed a round — waiting for the rebased stack's checks before the next".to_owned()
    };
    Step::Wait(AutoLandPhase::Waiting {
        attention: false,
        reasons: vec![reason],
    })
}

/// The round handed the merge to GitHub's automation instead of merging.
fn hands_off(plan: &LandPlan) -> bool {
    plan.actions.iter().any(|a| {
        matches!(
            a,
            LandAction::EnableAutoMerge { .. } | LandAction::EnqueuePr { .. }
        )
    })
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
    use jiji_core::snapshot::{BookmarkState, GraphNode, NodeKind, RepoSnapshot, SyncState};
    use jiji_core::BackendError;
    use std::cell::{Cell, RefCell};
    use std::collections::HashMap;

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
            is_empty: false,
            has_conflict: false,
            is_divergent: false,
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
        }
    }

    fn snapshot(nodes: Vec<GraphNode>, bookmarks: Vec<BookmarkState>) -> RepoSnapshot {
        RepoSnapshot {
            repo_path: "/tmp/repo".into(),
            repo_name: "repo".into(),
            backend: "test".into(),
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
            head_commit: "feedface".into(),
            head_owner: Some("o".into()),
            base_branch: base.into(),
            body: None,
            review: ReviewDecision::None,
            checks: ChecksRollup::None,
        }
    }

    fn merged_pr(number: u64, head: &str) -> PrSummary {
        PrSummary {
            state: PrState::Merged,
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
            review: ReviewDecision::Approved,
            checks: ChecksRollup::Passing,
            base_branch: "main".into(),
            head_commit: "feedface".into(),
            auto_merge_enabled: false,
            in_merge_queue: false,
            auto_merge_allowed: true,
            queue_on_base: false,
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
        snapshot(
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
        )
    }

    /// After auth landed and reconciled: profile alone on the new trunk.
    fn after_first_land() -> RepoSnapshot {
        snapshot(
            vec![
                node("b1", "profile: avatars", NodeKind::WorkingCopy, &["s"], &["profile"]),
                node("s", "auth (squash)", NodeKind::Immutable, &["m"], &["main"]),
                node("m", "release", NodeKind::Immutable, &[], &[]),
            ],
            vec![
                bookmark("main", "s", SyncState::Synced, true),
                bookmark("profile", "b1", SyncState::Synced, false),
            ],
        )
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
    }

    struct Sim {
        world: RefCell<World>,
        script: RefCell<HashMap<usize, Box<dyn Fn(&mut World)>>>,
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
            Ok("Fetched".into())
        }
        fn snapshot(&self) -> Result<RepoSnapshot, BackendError> {
            Ok(self.world.borrow().snapshot.clone())
        }
        fn rebase_onto_trunk(&self, root_change: &str) -> Result<String, BackendError> {
            self.calls.borrow_mut().push(format!("rebase:{root_change}"));
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
        fn delete_bookmark(&self, name: &str) -> Result<String, BackendError> {
            self.calls.borrow_mut().push(format!("delete:{name}"));
            Ok(format!("Deleted {name}"))
        }
        fn abandon_changes(&self, change_ids: &[String]) -> Result<String, BackendError> {
            self.calls
                .borrow_mut()
                .push(format!("abandon:{}", change_ids.join(",")));
            Ok("Abandoned".into())
        }
    }

    impl LandForge for Sim {
        fn find_merged_pr(&self, branch: &str) -> Result<Option<PrSummary>, ForgeError> {
            Ok(self.world.borrow().merged.get(branch).cloned())
        }
        fn pr_land_state(&self, number: u64, _base: &str) -> Result<PrLandState, ForgeError> {
            self.world
                .borrow()
                .land_states
                .get(&number)
                .cloned()
                .ok_or_else(|| ForgeError::Api(format!("no stub state for #{number}")))
        }
        fn merge_pr(
            &self,
            number: u64,
            method: crate::land::MergeMethod,
            _expected_head: &str,
        ) -> Result<(), ForgeError> {
            if self.world.borrow().merge_fails {
                return Err(ForgeError::Api("HTTP 405: merge refused".into()));
            }
            self.calls
                .borrow_mut()
                .push(format!("merge:{number}:{}", method.rest_name()));
            Ok(())
        }
        fn enable_auto_merge(
            &self,
            node_id: &str,
            method: crate::land::MergeMethod,
        ) -> Result<(), ForgeError> {
            self.calls
                .borrow_mut()
                .push(format!("auto_merge:{node_id}:{}", method.rest_name()));
            Ok(())
        }
        fn enqueue_pr(&self, node_id: &str) -> Result<(), ForgeError> {
            self.calls.borrow_mut().push(format!("enqueue:{node_id}"));
            Ok(())
        }
        fn update_pr_base(&self, number: u64, base: &str) -> Result<(), ForgeError> {
            self.calls
                .borrow_mut()
                .push(format!("retarget:{number}:{base}"));
            Ok(())
        }
    }

    fn test_config() -> AutoLandConfig {
        AutoLandConfig {
            poll_interval: Duration::ZERO,
            max_consecutive_errors: 10,
        }
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
        assert!(calls.contains(&"abandon:a2,a1".to_owned()), "{calls:?}");
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
    fn parks_after_the_error_budget() {
        let sim = Sim::new(World {
            snapshot: single_stack(),
            prs: vec![],
            land_states: HashMap::new(),
            merged: HashMap::new(),
            prs_fail: true,
            merge_fails: false,
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
            version: AUTOLAND_RECORD_VERSION,
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
    fn record_round_trips_through_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("autoland.json");
        let record = waiting_record("auth");

        save_autoland_record(&path, &record).unwrap();
        assert_eq!(load_autoland_record(&path), Some(record.clone()));
        // The atomic-write temp file does not linger.
        assert!(!path.with_extension("json.tmp").exists());

        // Saving again overwrites in place — one record slot.
        let mut newer = record;
        newer.state.rounds = 3;
        save_autoland_record(&path, &newer).unwrap();
        assert_eq!(load_autoland_record(&path), Some(newer));

        clear_autoland_record(&path).unwrap();
        assert_eq!(load_autoland_record(&path), None);
        // Clearing a record that is already gone is fine.
        clear_autoland_record(&path).unwrap();
    }

    #[test]
    fn broken_or_foreign_records_load_as_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("autoland.json");

        // No file at all.
        assert_eq!(load_autoland_record(&path), None);
        // Garbage on disk must never stop the app from starting.
        std::fs::write(&path, b"{ not json").unwrap();
        assert_eq!(load_autoland_record(&path), None);
        // A record from a different format version is dropped, not guessed at.
        let mut record = waiting_record("auth");
        record.version = AUTOLAND_RECORD_VERSION + 1;
        std::fs::write(&path, serde_json::to_vec(&record).unwrap()).unwrap();
        assert_eq!(load_autoland_record(&path), None);
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
}
