//! The backend host: everything the interface asks of a repo, off the UI
//! thread.
//!
//! Calls stay thin over the `RepoBackend` boundary in `jiji-core`: validate,
//! call, store the latest snapshot on its session, and publish it so every
//! surface refreshes from the same immutable state. Several repos can be
//! open at once (the tabs), so state is a session map keyed by the canonical
//! repo path and every repo-scoped call names its repo. Published snapshots
//! travel whole over [`HostEvent`]; the app routes each to the matching tab,
//! so background repos (watcher ticks, a pinned auto-land job's mutations)
//! stay fresh without owning the screen.
//!
//! Every method here blocks (disk, subprocesses, the network): call them
//! from the background executor, never from a render or an event handler.

pub mod autoland;
pub mod cli;
pub mod forge;
pub mod integrations;
pub mod license;
pub mod update;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, Weak};

use futures::channel::mpsc::{unbounded, UnboundedReceiver, UnboundedSender};
use jiji_core::snapshot::{MutationOutcome, SnapshotView};
use jiji_core::{BackendError, JjBackend, MockBackend, RepoBackend, RepoSnapshot, RepoWatcher};
use jiji_forge::AutoLandStatus;

use self::autoland::AutoLandHost;
use self::forge::{Forge, ForgeState};

/// A refusal the interface can render: a stable code plus the message.
#[derive(Debug, Clone, PartialEq)]
pub struct CommandError {
    pub code: String,
    pub message: String,
    /// The folder the error is about, when the UI can act on it (the Git
    /// root that `git_repo_without_jj` offers to set jj up in).
    pub path: Option<String>,
}

impl CommandError {
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            path: None,
        }
    }
}

impl std::fmt::Display for CommandError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
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

impl From<jiji_forge::ForgeError> for CommandError {
    fn from(err: jiji_forge::ForgeError) -> Self {
        Self::new(err.code(), err.to_string())
    }
}

pub type HostResult<T> = Result<T, CommandError>;

/// What the host tells the interface without being asked.
pub enum HostEvent {
    /// A repo's snapshot changed (a mutation, a watcher tick, a re-sync).
    Snapshot(Arc<RepoSnapshot>),
    /// The auto-land records changed; carries every status the host knows.
    AutoLand(Vec<AutoLandStatus>),
}

/// One open repo (one tab): its latest snapshot, the view lens its
/// snapshots render, and the watcher that keeps it fresh while other tabs
/// own the screen.
struct RepoSession {
    snapshot: Arc<RepoSnapshot>,
    view: SnapshotView,
    /// Held for its lifetime only: dropping the session stops the watcher.
    _watcher: Option<RepoWatcher>,
}

pub struct Host {
    backend: Box<dyn RepoBackend>,
    /// Sessions keyed by the canonical repo path (`snapshot.repo_path`).
    sessions: Mutex<HashMap<String, RepoSession>>,
    events: UnboundedSender<HostEvent>,
    pub(crate) forge: ForgeState,
    pub(crate) autoland: AutoLandHost,
    data_dir: PathBuf,
    this: Weak<Host>,
}

impl Host {
    pub fn new(data_dir: PathBuf) -> (Arc<Self>, UnboundedReceiver<HostEvent>) {
        // The mock stays available for UI work against stable, fabricated data.
        let backend: Box<dyn RepoBackend> =
            if std::env::var("JIJI_MOCK_BACKEND").as_deref() == Ok("1") {
                log::info!("JIJI_MOCK_BACKEND=1 set; serving mock snapshots");
                Box::new(MockBackend::default())
            } else {
                Box::new(JjBackend::default())
            };
        let forge: Box<dyn Forge> = if let Some(double) = mock_forge() {
            log::info!("JIJI_MOCK_FORGE set; Publish is answered by the test double");
            double
        } else if std::env::var("JIJI_OFFLINE").as_deref() == Ok("1") {
            log::info!("JIJI_OFFLINE=1 set; no forge token is looked up or used");
            Box::new(forge::Offline)
        } else {
            Box::new(forge::GitHub)
        };
        Self::with_backend(backend, forge, data_dir)
    }

    pub fn with_backend(
        backend: Box<dyn RepoBackend>,
        forge: Box<dyn Forge>,
        data_dir: PathBuf,
    ) -> (Arc<Self>, UnboundedReceiver<HostEvent>) {
        let (events, receiver) = unbounded();
        let host = Arc::new_cyclic(|this| Self {
            backend,
            sessions: Mutex::new(HashMap::new()),
            events,
            forge: ForgeState::new(forge),
            autoland: AutoLandHost::new(),
            data_dir,
            this: this.clone(),
        });
        // A job record that survived the last session loads before any
        // surface asks, so the shell can offer "interrupted — resume?".
        host.autoland.load_persisted(&host.autoland_path());
        (host, receiver)
    }

    pub fn backend(&self) -> &dyn RepoBackend {
        self.backend.as_ref()
    }

    pub(crate) fn autoland_path(&self) -> PathBuf {
        self.data_dir.join("autoland.json")
    }

    pub(crate) fn emit(&self, event: HostEvent) {
        // The receiver only goes away with the app.
        let _ = self.events.unbounded_send(event);
    }

    fn sessions(&self) -> std::sync::MutexGuard<'_, HashMap<String, RepoSession>> {
        self.sessions.lock().expect("repo sessions lock poisoned")
    }

    fn session_view(&self, path: &str) -> Option<SnapshotView> {
        self.sessions().get(path).map(|s| s.view.clone())
    }

    /// The lens refreshes should render for `path`: the session's chosen
    /// one, or the default for a repo not open yet.
    fn view_for(&self, path: &str) -> SnapshotView {
        self.session_view(path).unwrap_or_default()
    }

    /// Store the snapshot on its session (when one is open — a closed
    /// tab's late publish only emits) and broadcast it. A republish equal
    /// to what the session already holds keeps the allocation it has, so
    /// the interface can tell "nothing changed" by pointer.
    fn publish(&self, snapshot: RepoSnapshot) -> Arc<RepoSnapshot> {
        let shared = {
            let mut sessions = self.sessions();
            match sessions.get_mut(&snapshot.repo_path) {
                Some(session) if *session.snapshot == snapshot => session.snapshot.clone(),
                Some(session) => {
                    session.snapshot = Arc::new(snapshot);
                    session.snapshot.clone()
                }
                None => Arc::new(snapshot),
            }
        };
        self.emit(HostEvent::Snapshot(shared.clone()));
        shared
    }

    /// Post-mutation reload: the mutation already synced the working copy
    /// and git state, so this only reads and republishes.
    fn load_and_publish(&self, path: &str) -> HostResult<Arc<RepoSnapshot>> {
        let snapshot = self
            .backend
            .open_with(Path::new(path), &self.view_for(path))?;
        Ok(self.publish(snapshot))
    }

    /// Watch the repo so edits, CLI commands, and git ref moves refresh the
    /// snapshot without ⌘R — for background tabs too. Failing to watch only
    /// loses the automation, so it logs instead of failing the open.
    fn start_watcher(&self, path: &str) -> Option<RepoWatcher> {
        let host = self.this.clone();
        let repo_path = path.to_owned();
        self.backend
            .watch(
                Path::new(path),
                Box::new(move || {
                    if let Some(host) = host.upgrade() {
                        host.auto_refresh(&repo_path);
                    }
                }),
            )
            .unwrap_or_else(|err| {
                log::warn!("repo watching unavailable: {err}");
                None
            })
    }

    /// One watcher tick: refresh and publish only when something actually
    /// changed. Runs on the watcher thread; concurrent jj access from
    /// commands is safe the same way two CLI processes are. A session
    /// closed since the tick was queued is left alone.
    fn auto_refresh(&self, path: &str) {
        let Some(view) = self.session_view(path) else {
            return;
        };
        match self.backend.refresh_with(Path::new(path), &view) {
            Ok(snapshot) => {
                let unchanged = self
                    .sessions()
                    .get(path)
                    .is_some_and(|session| *session.snapshot == snapshot);
                if !unchanged {
                    self.publish(snapshot);
                }
            }
            // Transient by nature (lock contention with a running CLI
            // command); the next relevant change retries.
            Err(err) => log::warn!("auto-refresh failed: {err}"),
        }
    }

    /// Open a repo — or re-sync one that is already open (the session
    /// keeps its chosen view lens). A fresh repo starts on the default lens
    /// and gets its own watcher.
    pub fn open_repo(&self, path: &str) -> HostResult<Arc<RepoSnapshot>> {
        let path = resolve_open_path(path);
        let mut snapshot = self
            .backend
            .refresh_with(Path::new(&path), &self.view_for(&path))?;
        // The requested path may still be an alias of the canonical root
        // (a symlink, a non-canonical spelling). When the canonical repo is
        // already open under a chosen lens, re-render under that lens
        // instead of clobbering it.
        if snapshot.repo_path != path {
            if let Some(view) = self.session_view(&snapshot.repo_path) {
                if view != snapshot.view {
                    snapshot = self
                        .backend
                        .refresh_with(Path::new(&snapshot.repo_path), &view)?;
                }
            }
        }
        let canonical = snapshot.repo_path.clone();
        if !self.sessions().contains_key(&canonical) {
            // Start watching before the session exists: the watcher takes a
            // moment to arm and must not hold the sessions lock meanwhile.
            let watcher = self.start_watcher(&canonical);
            let mut sessions = self.sessions();
            if !sessions.contains_key(&canonical) {
                sessions.insert(
                    canonical.clone(),
                    RepoSession {
                        snapshot: Arc::new(snapshot.clone()),
                        view: snapshot.view.clone(),
                        _watcher: watcher,
                    },
                );
            }
        }
        Ok(self.publish(snapshot))
    }

    /// Set jj up in a plain Git repository — `jj git init --colocate` at
    /// the Git root enclosing `path` — and return that root for
    /// `open_repo`. Only ever runs from an explicit confirmation.
    pub fn init_jj_repo(&self, path: &str) -> HostResult<String> {
        let root = jiji_core::backend::git_root_for(Path::new(path)).ok_or_else(|| {
            CommandError::new(
                "not_a_git_repo",
                format!("{path} is not inside a Git repository"),
            )
        })?;
        self.backend.init_colocated(&root)?;
        Ok(root.to_string_lossy().into_owned())
    }

    /// Close a repo's session (its tab closed): the watcher stops and the
    /// cached snapshot drops. A live auto-land job pinned to this repo is
    /// detached first — its record survives as interrupted, so reopening
    /// the repo offers Resume.
    pub fn close_repo(&self, repo_path: &str) {
        self.autoland.detach_for(repo_path);
        // Dropped outside the lock: stopping a watcher joins its thread.
        let session = self.sessions().remove(repo_path);
        drop(session);
    }

    /// Bring the repo up to date like running a jj command would (snapshot
    /// working-copy edits, import externally-moved git state), then publish.
    pub fn refresh_snapshot(&self, repo_path: &str) -> HostResult<Arc<RepoSnapshot>> {
        self.require_open(repo_path)?;
        let snapshot = self
            .backend
            .refresh_with(Path::new(repo_path), &self.view_for(repo_path))?;
        Ok(self.publish(snapshot))
    }

    /// Switch the view lens snapshots render. A custom revset that fails
    /// to parse or resolve refuses with `revset_invalid` and leaves both
    /// the stored view and the published snapshot untouched.
    pub fn set_snapshot_view(
        &self,
        repo_path: &str,
        view: SnapshotView,
    ) -> HostResult<Arc<RepoSnapshot>> {
        self.require_open(repo_path)?;
        let snapshot = self.backend.refresh_with(Path::new(repo_path), &view)?;
        if let Some(session) = self.sessions().get_mut(repo_path) {
            session.view = view;
        }
        Ok(self.publish(snapshot))
    }

    /// The latest published snapshot of one repo — what the user currently
    /// sees in its tab.
    pub fn snapshot_for(&self, path: &str) -> Option<Arc<RepoSnapshot>> {
        self.sessions().get(path).map(|s| s.snapshot.clone())
    }

    /// Git remotes of an open repo's latest snapshot; empty when it is not
    /// open. What forge detection reads.
    pub(crate) fn current_git_remotes(&self, path: &str) -> Vec<jiji_core::snapshot::GitRemote> {
        self.sessions()
            .get(path)
            .map(|s| s.snapshot.git_remotes.clone())
            .unwrap_or_default()
    }

    /// Publishing walks complete local stacks regardless of the graph's
    /// selected revset. Reading this never changes that view or the repo.
    pub fn workflow_snapshot(&self, path: &str) -> Result<Arc<RepoSnapshot>, BackendError> {
        let snapshot = self
            .snapshot_for(path)
            .ok_or_else(|| BackendError::MutationFailed("This repository is not open".into()))?;
        if matches!(
            snapshot.view,
            SnapshotView::Workstreams | SnapshotView::Team | SnapshotView::Recent
        ) {
            Ok(snapshot)
        } else {
            self.backend.open(Path::new(path)).map(Arc::new)
        }
    }

    /// Refuse repo-scoped work on a repo that is not open: a command
    /// arriving for a closed tab is a routing bug, and refusing keeps it
    /// visible.
    pub(crate) fn require_open(&self, path: &str) -> HostResult<()> {
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
    /// tree — how forge planning reads the repo's PR template.
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

    /// The shape every write shares: run the backend mutation against its
    /// repo, refresh the snapshot (published to all surfaces), and return
    /// the operation breadcrumb. The path is pinned by the caller, so a
    /// mutation in flight when another tab takes the screen still lands in
    /// its own repo.
    pub fn mutate(
        &self,
        path: &str,
        action: impl FnOnce(&dyn RepoBackend, &Path) -> Result<MutationOutcome, BackendError>,
    ) -> HostResult<MutationOutcome> {
        let outcome = action(self.backend.as_ref(), Path::new(path))?;
        self.load_and_publish(path)?;
        Ok(outcome)
    }

    pub(crate) fn push_and_publish(
        &self,
        path: &str,
        names: &[String],
        remote: &str,
    ) -> HostResult<MutationOutcome> {
        self.mutate(path, |backend, path| {
            backend.push_bookmarks(path, names, Some(remote))
        })
    }
}

/// `JIJI_MOCK_FORGE`, in a `headless` build over the mock backend: GitHub
/// as the tests capture it stands behind the mock repository, so a
/// scripted run can draw Publish with pull requests on it. Nothing is
/// looked up and nothing is reached, as with `JIJI_OFFLINE`. `1` is the
/// mock stack's two pull requests as a plain chain, `stack` the same two
/// linked as a GitHub stack, `stray` the lower one closed as merged
/// without having landed, `others` the plain chain with two pull requests
/// by other people on it (one on each of its branches).
#[cfg(all(feature = "headless", not(test)))]
fn mock_forge() -> Option<Box<dyn Forge>> {
    use crate::testing::forge::Fixture;
    if std::env::var("JIJI_MOCK_BACKEND").as_deref() != Ok("1") {
        return None;
    }
    let fixture = Fixture::mock_stack();
    match std::env::var("JIJI_MOCK_FORGE").ok()?.as_str() {
        "1" => {}
        "stack" => {
            fixture.link_stack(&[41, 42]);
        }
        "stray" => {
            fixture.pull(41, |pull| {
                pull.summary.base_branch = "diff-virtualization".into();
                pull.merged_as("8b19f3c2");
            });
            fixture.pull(42, |pull| pull.summary.base_branch = "main".into());
        }
        "others" => {
            use crate::testing::forge::open_pr;
            for (number, head, base, commit, title, author) in [
                (
                    50,
                    "sticky-header-shadow",
                    "diff-model",
                    "0ddba11c",
                    "Shadow under the sticky header",
                    "kim",
                ),
                (
                    51,
                    "virtual-scroll-bench",
                    "diff-virtualization",
                    "5ca1ab1e",
                    "Benchmark for the virtualized diff",
                    "sam",
                ),
            ] {
                let mut theirs = open_pr(number, head, base, commit, title);
                theirs.summary.author = Some(author.into());
                fixture.github().prs.push(theirs);
            }
        }
        _ => return None,
    }
    Some(fixture.boxed())
}

#[cfg(not(all(feature = "headless", not(test))))]
fn mock_forge() -> Option<Box<dyn Forge>> {
    None
}

/// The nearest ancestor that is a jj workspace root. The backend wants the
/// exact root, but "open this repo" usually happens deep inside it; a file
/// selects its containing repo.
pub fn repo_root_for(path: &Path) -> Option<PathBuf> {
    let start = if path.is_dir() { path } else { path.parent()? };
    start
        .ancestors()
        .find(|dir| dir.join(".jj").is_dir())
        .map(Path::to_path_buf)
}

/// jj works from anywhere inside a workspace, so opening does too: a
/// subfolder or file opens its enclosing repo. A path that no longer exists
/// passes through untouched, so a stale recent entry reports itself missing
/// instead of quietly opening whichever repo encloses where it used to be.
fn resolve_open_path(path: &str) -> String {
    let requested = Path::new(path);
    if !requested.exists() {
        return path.to_owned();
    }
    match repo_root_for(requested) {
        Some(root) => root.to_string_lossy().into_owned(),
        None => path.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("jiji-host-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.canonicalize().unwrap()
    }

    fn display(path: &Path) -> String {
        path.to_string_lossy().into_owned()
    }

    #[test]
    fn opening_inside_a_repo_opens_its_root() {
        let root = scratch("open-root");
        std::fs::create_dir(root.join(".jj")).unwrap();
        let nested = root.join("src/lib");
        std::fs::create_dir_all(&nested).unwrap();
        let file = nested.join("api.ts");
        std::fs::write(&file, "").unwrap();

        assert_eq!(resolve_open_path(&display(&root)), display(&root));
        assert_eq!(resolve_open_path(&display(&nested)), display(&root));
        assert_eq!(resolve_open_path(&display(&file)), display(&root));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn paths_outside_any_repo_or_missing_pass_through() {
        let root = scratch("open-plain");
        let plain = root.join("plain");
        std::fs::create_dir(&plain).unwrap();
        assert_eq!(resolve_open_path(&display(&plain)), display(&plain));

        // A deleted repo that sat inside another one must not open the
        // outer repo in its place.
        std::fs::create_dir(root.join(".jj")).unwrap();
        let gone = root.join("deleted-repo");
        assert_eq!(resolve_open_path(&display(&gone)), display(&gone));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn publishing_reads_full_stacks_without_changing_the_graph_lens() {
        let dir = scratch("workflow");
        std::fs::create_dir(dir.join(".jj")).unwrap();
        let (host, _events) = Host::with_backend(
            Box::new(MockBackend::default()),
            Box::new(forge::Offline),
            dir.clone(),
        );
        let full = host.open_repo(&display(&dir)).unwrap();
        let path = full.repo_path.clone();
        let filtered = host.set_snapshot_view(&path, SnapshotView::Trunk).unwrap();
        assert_eq!(filtered.view, SnapshotView::Trunk);
        let workflow = host.workflow_snapshot(&path).unwrap();
        assert_eq!(workflow.nodes, full.nodes);
        assert_eq!(host.snapshot_for(&path).unwrap().view, SnapshotView::Trunk);
        assert!(host.workflow_snapshot("/closed/repo").is_err());
        host.close_repo(&path);
        assert!(host.snapshot_for(&path).is_none());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn an_unchanged_republish_keeps_the_same_allocation() {
        let dir = scratch("settle");
        std::fs::create_dir(dir.join(".jj")).unwrap();
        let (host, _events) = Host::with_backend(
            Box::new(MockBackend::default()),
            Box::new(forge::Offline),
            dir.clone(),
        );
        let first = host.open_repo(&display(&dir)).unwrap();
        let again = host.refresh_snapshot(&first.repo_path).unwrap();
        assert!(Arc::ptr_eq(&first, &again));
        let _ = std::fs::remove_dir_all(dir);
    }
}
