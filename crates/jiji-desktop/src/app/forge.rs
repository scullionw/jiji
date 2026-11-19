//! The forge connection, the background upstream check and the auto-land
//! jobs: shell-level state that follows the repo on screen.
//!
//! One source of truth for the connection and open-PR state, shared by
//! every surface that renders it (Publish's connection cards, the
//! workbench's PR badges). The fetch cadence: repo open (or its remotes
//! changing), ⌘R, connection changes — and the upstream check, which
//! refreshes PR state on the same rhythm as its `jj git fetch`.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{Context, Task};
use jiji_core::snapshot::GitRemote;
use jiji_forge::{AutoLandStatus, ForgeStatus, LandOptions, RepoPrState};

use super::{AppModel, Breadcrumb};
use crate::host::HostResult;
use crate::store::{now_ms, Store};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ForgePhase {
    #[default]
    Idle,
    Loading,
    Verifying,
    Ready,
}

/// A repo's remotes, as (name, url) pairs.
type RemotesKey = Vec<(String, String)>;
/// What a tab's forge state was when the screen last left it.
type Remembered = (Option<ForgeStatus>, Option<Arc<RepoPrState>>);

#[derive(Default)]
pub struct ForgeUi {
    pub status: Option<ForgeStatus>,
    pub phase: ForgePhase,
    /// Connection-level failure (status/verify); Publish renders it.
    pub error: Option<String>,
    /// Open-PR state of the detected repo; `None` until fetched.
    pub prs: Option<Arc<RepoPrState>>,
    /// PR-fetch failure: badges quietly stay absent, Publish names it.
    pub prs_error: Option<String>,
    pub prs_loading: bool,

    // Stale-response guards: only the newest sync/fetch may write back.
    sync_seq: u64,
    prs_seq: u64,
    remotes_key: Option<Option<Vec<GitRemote>>>,
    /// Connection and PR state remembered per remotes, so switching back
    /// to a tab restores its badges instantly instead of blanking and
    /// re-verifying on every switch.
    cache: HashMap<RemotesKey, Remembered>,
}

impl ForgeUi {
    /// PRs are worth fetching once a repo is detected and the token
    /// verified this session — "Connected as …" is earned, and badges ride
    /// on it.
    pub fn connection_ready(&self) -> bool {
        self.status
            .as_ref()
            .is_some_and(|status| status.repo.is_some() && status.auth.login.is_some())
    }
}

fn cache_key(remotes: &[GitRemote]) -> Vec<(String, String)> {
    remotes
        .iter()
        .map(|remote| (remote.name.clone(), remote.url.clone()))
        .collect()
}

/// Cadence choices Settings offers; 0 turns background checks off (the
/// chip's manual Fetch now stays).
pub const UPSTREAM_INTERVALS: [(u32, &str); 4] = [(5, "5m"), (15, "15m"), (30, "30m"), (0, "Off")];

const DEFAULT_INTERVAL_MINUTES: u32 = 15;
/// The heartbeat that drives due-checks and ages the chip label.
const HEARTBEAT: Duration = Duration::from_secs(60);

/// One repo's check memory. `last_attempt` is the cadence gate (when a
/// check last *started*, success or not), kept separate from
/// `last_checked` so an unreachable remote retries on the interval instead
/// of every heartbeat.
#[derive(Default, Clone)]
struct UpstreamRecord {
    last_checked: Option<u64>,
    error: Option<String>,
    last_attempt: Option<Instant>,
}

pub struct UpstreamUi {
    pub checking: bool,
    /// When the last *successful* check completed (unix ms).
    pub last_checked: Option<u64>,
    /// The last check's failure, cleared by the next success.
    pub error: Option<String>,
    /// Heartbeat-updated clock the chip ages its label against.
    pub now: u64,
    /// Background checks run this far apart (minutes); 0 is off.
    pub interval_minutes: u32,
    records: HashMap<String, UpstreamRecord>,
    repo: Option<String>,
    seq: u64,
}

impl UpstreamUi {
    /// An offline run's checks are off for the run, the saved choice
    /// untouched: `jj git fetch` would use the user's Git credentials.
    pub fn new(store: &Store, offline: bool) -> Self {
        let interval = if offline {
            0
        } else {
            store
                .pref::<u32>("upstream.interval")
                .filter(|minutes| UPSTREAM_INTERVALS.iter().any(|(m, _)| m == minutes))
                .unwrap_or(DEFAULT_INTERVAL_MINUTES)
        };
        Self {
            checking: false,
            last_checked: None,
            error: None,
            now: now_ms(),
            interval_minutes: interval,
            records: HashMap::new(),
            repo: None,
            seq: 0,
        }
    }
}

#[derive(Default)]
pub struct AutoLandUi {
    /// Every job status as last published; terminal and interrupted
    /// records stay until dismissed. At most one is live.
    pub jobs: Vec<AutoLandStatus>,
    /// A stop was requested and the definitive stopped phase is pending.
    pub stopping: bool,
}

/// A non-terminal record no thread is driving: a restart survivor or a
/// detached job, offering resume.
pub fn is_interrupted(status: &AutoLandStatus) -> bool {
    !status.live && !status.record.state.phase.is_terminal()
}

impl AppModel {
    // ── Forge connection ──

    /// Re-derive the connection when the on-screen repo's remotes change
    /// (open, tab switch, remote edits). Keyed on the remotes, so snapshot
    /// refreshes that leave them alone cost nothing.
    pub(super) fn sync_forge_connection(
        &mut self,
        remotes: Option<Vec<GitRemote>>,
        cx: &mut Context<Self>,
    ) {
        if self.forge.remotes_key.as_ref() == Some(&remotes) {
            return;
        }
        if let Some(Some(previous)) = self.forge.remotes_key.take() {
            self.forge.cache.insert(
                cache_key(&previous),
                (self.forge.status.clone(), self.forge.prs.clone()),
            );
        }
        self.forge.remotes_key = Some(remotes.clone());
        self.forge.sync_seq += 1;
        self.forge.prs_seq += 1; // orphan any in-flight PR fetch for the previous repo
        let seq = self.forge.sync_seq;
        self.forge.prs = None;
        self.forge.prs_error = None;
        self.forge.prs_loading = false;
        self.forge.error = None;
        cx.notify();
        let Some(remotes) = remotes else {
            // No open repo: back to the blank slate.
            self.forge.status = None;
            self.forge.phase = ForgePhase::Idle;
            return;
        };
        // A repo seen this session comes back as it left, then re-fetches
        // PRs quietly so badges stay honest.
        if let Some((Some(status), prs)) = self.forge.cache.get(&cache_key(&remotes)).cloned() {
            self.forge.status = Some(status);
            self.forge.prs = prs;
            self.forge.phase = ForgePhase::Ready;
            self.refresh_forge_prs(cx);
            return;
        }
        let Some(path) = self.repo_path() else {
            return;
        };
        self.forge.phase = ForgePhase::Loading;
        let status = self.bg(cx, {
            let path = path.clone();
            move |host| host.forge_status(&path)
        });
        cx.spawn(async move |this, cx| {
            let status = status.await;
            // A token without a session-verified login: check it against
            // the API once so "Connected as …" is earned, not assumed.
            let verify = this.update(cx, |this, cx| {
                if this.forge.sync_seq != seq {
                    return None;
                }
                match status {
                    Ok(status) => {
                        let unverified =
                            status.auth.source.is_some() && status.auth.login.is_none();
                        this.forge.status = Some(status);
                        if unverified {
                            this.forge.phase = ForgePhase::Verifying;
                            cx.notify();
                            return Some(this.bg(cx, move |host| host.forge_verify(&path)));
                        }
                        this.forge.phase = ForgePhase::Ready;
                        this.refresh_forge_prs(cx);
                    }
                    Err(error) => {
                        this.forge.error = Some(error.message);
                        this.forge.phase = ForgePhase::Ready;
                    }
                }
                cx.notify();
                None
            });
            let Ok(Some(verify)) = verify else {
                return;
            };
            let verified = verify.await;
            let _ = this.update(cx, |this, cx| {
                if this.forge.sync_seq != seq {
                    return;
                }
                match verified {
                    Ok(status) => this.forge.status = Some(status),
                    Err(error) => this.forge.error = Some(error.message),
                }
                this.forge.phase = ForgePhase::Ready;
                this.refresh_forge_prs(cx);
                cx.notify();
            });
        })
        .detach();
    }

    /// Fetch the open-PR state badges and publish surfaces render (open,
    /// ⌘R, connect, and every upstream check); a no-op without a ready
    /// connection.
    pub fn refresh_forge_prs(&mut self, cx: &mut Context<Self>) {
        if !self.forge.connection_ready() {
            return;
        }
        let Some(path) = self.repo_path() else {
            return;
        };
        self.forge.prs_seq += 1;
        let seq = self.forge.prs_seq;
        self.forge.prs_loading = true;
        cx.notify();
        let fetched = self.bg(cx, move |host| host.forge_prs(&path));
        cx.spawn(async move |this, cx| {
            let result = fetched.await;
            let _ = this.update(cx, |this, cx| {
                if this.forge.prs_seq != seq {
                    return;
                }
                match result {
                    Ok(prs) => {
                        // An unchanged answer keeps its allocation, so
                        // views can tell by pointer.
                        let same = this.forge.prs.as_deref() == Some(&prs);
                        if !same {
                            this.forge.prs = Some(Arc::new(prs));
                        }
                        this.forge.prs_error = None;
                    }
                    Err(error) => this.forge.prs_error = Some(error.message),
                }
                this.forge.prs_loading = false;
                cx.notify();
            });
        })
        .detach();
    }

    /// Validate and store a pasted token (Publish's connect flow), then
    /// light the PR state up. Refusals come back so the form renders them
    /// inline.
    pub fn connect_forge(&mut self, token: String, cx: &mut Context<Self>) -> Task<HostResult<()>> {
        let Some(path) = self.repo_path() else {
            return Task::ready(Err(super::no_repo_open()));
        };
        let login = self.bg(cx, move |host| host.forge_login(&path, &token));
        cx.spawn(async move |this, cx| {
            let status = login.await?;
            let _ = this.update(cx, |this, cx| {
                this.forge.status = Some(status);
                this.forge.error = None;
                this.forge.phase = ForgePhase::Ready;
                this.refresh_forge_prs(cx);
                cx.notify();
            });
            Ok(())
        })
    }

    /// Remove Jiji's stored token. A fallback token (env/gh) may take over
    /// — it gets verified so the connection stays earned — and the PR
    /// state follows the answer.
    pub fn disconnect_forge(&mut self, cx: &mut Context<Self>) -> Task<HostResult<()>> {
        let Some(path) = self.repo_path() else {
            return Task::ready(Err(super::no_repo_open()));
        };
        let logout = self.bg(cx, move |host| {
            let status = host.forge_logout(&path)?;
            if status.auth.source.is_some() && status.auth.login.is_none() {
                return host.forge_verify(&path);
            }
            Ok(status)
        });
        self.forge.phase = ForgePhase::Verifying;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = logout.await;
            let _ = this.update(cx, |this, cx| {
                this.forge.phase = ForgePhase::Ready;
                if let Ok(status) = &result {
                    this.forge.status = Some(status.clone());
                    if this.forge.connection_ready() {
                        this.refresh_forge_prs(cx);
                    } else {
                        this.forge.prs = None;
                        this.forge.prs_error = None;
                    }
                }
                cx.notify();
            });
            result.map(|_| ())
        })
    }

    // ── Upstream checks ──

    pub fn set_upstream_interval(&mut self, minutes: u32, cx: &mut Context<Self>) {
        self.upstream.interval_minutes = minutes;
        self.store.set_pref("upstream.interval", &minutes);
        cx.notify();
    }

    /// Follow the repo on screen: a different repo (or none) swaps in its
    /// own check memory, and a repo never checked before gets its first
    /// check right away.
    pub(super) fn sync_upstream_repo(
        &mut self,
        path: Option<String>,
        has_remotes: bool,
        cx: &mut Context<Self>,
    ) {
        if path == self.upstream.repo {
            return;
        }
        self.upstream.repo = path.clone();
        self.upstream.seq += 1; // orphan any in-flight check's shell-state writes
        let record = path.as_ref().map(|path| {
            self.upstream
                .records
                .entry(path.clone())
                .or_default()
                .clone()
        });
        self.upstream.checking = false;
        self.upstream.last_checked = record.as_ref().and_then(|r| r.last_checked);
        self.upstream.error = record.as_ref().and_then(|r| r.error.clone());
        // Off means no unrequested fetches at all — not even the
        // open-check.
        let never_tried = record.is_some_and(|record| record.last_attempt.is_none());
        if path.is_some() && has_remotes && self.upstream.interval_minutes != 0 && never_tried {
            self.check_upstream(false, cx);
        }
    }

    /// One upstream check: fetch from the default remotes, then refresh
    /// the PR state on the same rhythm. `breadcrumb` is the manual
    /// Fetch-now flavor — the recorded operation lands in the status bar
    /// like any other mutation (background ticks stay silent). Results
    /// always land on the repo's own record; the shell chip only follows
    /// while that repo still owns the screen.
    pub fn check_upstream(&mut self, breadcrumb: bool, cx: &mut Context<Self>) {
        let Some(snapshot) = self.snapshot() else {
            return;
        };
        if snapshot.git_remotes.is_empty() || self.upstream.checking {
            return;
        }
        let path = snapshot.repo_path.clone();
        self.upstream.seq += 1;
        let seq = self.upstream.seq;
        self.upstream.checking = true;
        self.upstream
            .records
            .entry(path.clone())
            .or_default()
            .last_attempt = Some(Instant::now());
        cx.notify();
        let fetched = self.bg(cx, {
            let path = path.clone();
            move |host| host.mutate(&path, |backend, path| backend.git_fetch(path, None))
        });
        cx.spawn(async move |this, cx| {
            let result = fetched.await;
            let _ = this.update(cx, |this, cx| {
                let on_screen = this.upstream.seq == seq;
                let record = this.upstream.records.entry(path.clone()).or_default();
                match result {
                    Ok(outcome) => {
                        let at = now_ms();
                        record.error = None;
                        record.last_checked = Some(at);
                        if let Some(snapshot) = this.host.snapshot_for(&path) {
                            this.apply_snapshot(snapshot, cx);
                        }
                        if on_screen {
                            this.upstream.error = None;
                            this.upstream.last_checked = Some(at);
                            this.upstream.now = at;
                            if breadcrumb && outcome.operation_id.is_some() {
                                this.tab_mut().ui.last_mutation = Some(Breadcrumb {
                                    outcome,
                                    at: Instant::now(),
                                });
                            }
                        }
                    }
                    Err(error) => {
                        record.error = Some(error.message.clone());
                        if on_screen {
                            this.upstream.error = Some(error.message);
                        }
                    }
                }
                if on_screen {
                    this.upstream.checking = false;
                }
                this.refresh_forge_prs(cx);
                cx.notify();
            });
        })
        .detach();
    }

    /// Start the background cadence. A one-minute heartbeat ages the chip
    /// label and starts a check whenever the on-screen repo's last attempt
    /// is a full interval old.
    pub(super) fn start_upstream_checks(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| loop {
            cx.background_executor().timer(HEARTBEAT).await;
            let alive = this.update(cx, |this, cx| {
                this.upstream.now = now_ms();
                cx.notify();
                let Some(snapshot) = this.snapshot() else {
                    return;
                };
                if snapshot.git_remotes.is_empty()
                    || this.upstream.checking
                    || this.upstream.interval_minutes == 0
                {
                    return;
                }
                let interval = Duration::from_secs(this.upstream.interval_minutes as u64 * 60);
                let due = this
                    .upstream
                    .records
                    .get(&snapshot.repo_path)
                    .and_then(|record| record.last_attempt)
                    .is_none_or(|at| at.elapsed() >= interval);
                if due {
                    this.check_upstream(false, cx);
                }
            });
            if alive.is_err() {
                break;
            }
        })
        .detach();
    }

    // ── Auto-land ──

    pub(super) fn set_autoland_jobs(
        &mut self,
        statuses: Vec<AutoLandStatus>,
        cx: &mut Context<Self>,
    ) {
        // The stop the flag was waiting on has landed once nothing is live.
        if !statuses.iter().any(|status| status.live) {
            self.autoland.stopping = false;
        }
        self.autoland.jobs = statuses;
        cx.notify();
    }

    /// Queue a stack — or resume an interrupted job for the same stack,
    /// which is deliberately the same call. Refusals (a job already
    /// running, no token) come back so the initiating surface renders them
    /// inline.
    pub fn start_autoland(
        &mut self,
        head_bookmark: String,
        options: Option<LandOptions>,
        cx: &mut Context<Self>,
    ) -> Task<HostResult<()>> {
        let Some(path) = self.repo_path() else {
            return Task::ready(Err(super::no_repo_open()));
        };
        let started = self.bg(cx, move |host| {
            host.autoland_start(&path, &head_bookmark, options)
        });
        cx.spawn(async move |this, cx| {
            let statuses = started.await?;
            let _ = this.update(cx, |this, cx| {
                this.set_autoland_jobs(statuses, cx);
                this.autoland.stopping = false;
            });
            Ok(())
        })
    }

    /// Ask the live job to stop. It winds down at the next safe point and
    /// the stopped phase arrives on the event.
    pub fn stop_autoland(&mut self, cx: &mut Context<Self>) {
        self.autoland.stopping = true;
        let statuses = self.host.autoland_stop();
        self.set_autoland_jobs(statuses, cx);
        self.autoland.stopping = self.autoland.jobs.iter().any(|status| status.live);
    }

    /// Clear one finished or interrupted job's record — from the shell
    /// and from the host's persisted list.
    pub fn dismiss_autoland(&mut self, status: &AutoLandStatus, cx: &mut Context<Self>) {
        if !status.record.state.phase.is_terminal() && !is_interrupted(status) {
            return;
        }
        let repo = status.record.repo_path.clone();
        let head = status.record.state.head_bookmark.clone();
        self.autoland
            .jobs
            .retain(|job| !job.record.is_for(&repo, &head));
        cx.notify();
        let dismissed = self.bg(cx, move |host| host.autoland_dismiss(&repo, &head));
        cx.spawn(async move |this, cx| {
            if let Ok(statuses) = dismissed.await {
                let _ = this.update(cx, |this, cx| this.set_autoland_jobs(statuses, cx));
            }
        })
        .detach();
    }

    /// Coming back to the app is the moment stale job state hurts: nudge
    /// a watching auto-land job to poll now.
    pub fn window_activated(&mut self) {
        self.host.autoland.poke_active();
    }
}

#[cfg(test)]
mod tests {
    use gpui::TestAppContext;

    use super::*;
    use crate::host::forge::Offline;
    use crate::host::Host;

    #[gpui::test]
    fn an_offline_run_asks_github_nothing_and_fetches_nothing_on_its_own(cx: &mut TestAppContext) {
        let dir = crate::testing::scratch("offline-run");
        let repo = dir.join("repo");
        std::fs::create_dir_all(repo.join(".jj")).unwrap();
        // The saved choice is a check every five minutes.
        let mut store = Store::at(dir.join("data"));
        store.set_pref("upstream.interval", &5u32);
        let model = cx.update(|cx| {
            crate::testing::bind(cx);
            let host = Host::with_backend(
                Box::new(jiji_core::MockBackend::default()),
                Box::new(Offline),
                dir.join("data"),
            );
            AppModel::install_over(host, store, "Menlo".into(), cx)
        });
        model.update(cx, |model, cx| model.open_paths(vec![repo.clone()], cx));
        cx.run_until_parked();
        model.read_with(cx, |model, _| {
            // The remote is still known for what it is; no token was
            // looked for, and nothing was fetched.
            let status = model.forge.status.clone().expect("the connection settled");
            assert!(status.repo.is_some());
            assert_eq!(status.auth.source, None);
            assert_eq!(model.forge.phase, ForgePhase::Ready);
            assert_eq!(model.upstream.interval_minutes, 0);
            assert_eq!(model.upstream.last_checked, None);
        });
        // A pasted token goes nowhere either.
        let connect = model.update(cx, |model, cx| model.connect_forge("ghp_pasted".into(), cx));
        cx.run_until_parked();
        let refused = cx.foreground_executor().block_test(connect).unwrap_err();
        assert_eq!(
            refused.message,
            "Could not reach GitHub: this run is offline (JIJI_OFFLINE=1)"
        );
        // The run's checks are off; the saved choice is not touched.
        let saved = Store::at(dir.join("data")).pref::<u32>("upstream.interval");
        assert_eq!(saved, Some(5));
        let _ = std::fs::remove_dir_all(dir);
    }
}
