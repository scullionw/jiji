//! The app model: one entity holding everything the interface shares —
//! the open-repo tabs, each tab's selection and breadcrumbs, the forge
//! connection, upstream checks, auto-land jobs and preferences. Views read
//! it and call its methods; only this module (and its submodules) writes
//! it.

mod forge;
mod menu;
mod prefs;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use futures::StreamExt as _;
use gpui::{
    AnyWindowHandle, App, AppContext as _, Context, Entity, EventEmitter, Global, PromptLevel,
    SharedString, Task, WeakEntity,
};
use jiji_core::snapshot::{MutationOutcome, RepoSnapshot, SnapshotView};
use jiji_core::{BackendError, RepoBackend};

pub use self::forge::{AutoLandUi, ForgePhase, ForgeUi, UpstreamUi, UPSTREAM_INTERVALS};
pub use self::menu::OpenMenu;
pub use self::prefs::{CliUi, LicenseUi, ThemeMode, UpdateUi};
use crate::host::integrations::AppInfo;
use crate::host::{CommandError, Host, HostEvent, HostResult};
use crate::logic::inspect::{find_node, stack_position, CompareMode};
use crate::store::{PersistedRepo, PersistedTabs, RecentRepo, Store};

/// How many closed tabs ⌘⇧T can walk back through.
const MAX_CLOSED: usize = 10;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Section {
    Workbench,
    Conflicts,
    Publish,
    Bookmarks,
    Operations,
    Workspaces,
}

pub const SECTIONS: [Section; 6] = [
    Section::Workbench,
    Section::Conflicts,
    Section::Publish,
    Section::Bookmarks,
    Section::Operations,
    Section::Workspaces,
];

impl Section {
    pub fn label(self) -> &'static str {
        match self {
            Section::Workbench => "Workbench",
            Section::Conflicts => "Conflicts",
            Section::Publish => "Publish",
            Section::Bookmarks => "Bookmarks",
            Section::Operations => "Operations",
            Section::Workspaces => "Workspaces",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiffLayout {
    Unified,
    Split,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkbenchMode {
    Graph,
    Focus,
}

/// A one-shot request from the command palette (or a menu) to the surface
/// that owns the matching UI: the change header opens its plan/confirm
/// panels, the diff pane owns the layout, the workbench the view mode,
/// Publish the land and ship plan cards. The owner takes the intent after
/// acting; intents it does not own it leaves alone.
#[derive(Clone, Debug, PartialEq)]
pub enum UiIntent {
    Describe,
    Bookmark,
    Rebase,
    /// `into` opens the split panel in its move-into-an-existing-change
    /// mode; `path` preselects one file.
    Split {
        into: bool,
        path: Option<String>,
    },
    Squash,
    Abandon,
    /// With a mode the comparison applies directly; without one the
    /// compare panel opens for picking.
    Compare(Option<CompareMode>),
    Evolog,
    Layout(DiffLayout),
    FileTree,
    Find,
    View(WorkbenchMode),
    Density(crate::logic::graph::RowDensity),
    Land {
        bookmark: String,
    },
    Ship {
        head: String,
    },
}

/// The breadcrumb after a mutation: what happened and which operation
/// recorded it, so the status bar can deep-link into the timeline.
#[derive(Clone, Debug, PartialEq)]
pub struct Breadcrumb {
    pub outcome: MutationOutcome,
    pub at: Instant,
}

/// The repo-scoped state of one tab.
#[derive(Clone, Debug)]
pub struct TabUi {
    pub section: Section,
    /// Change id selected in the workbench graph.
    pub selected_node: Option<String>,
    /// ⇧-click multi-selection: a contiguous run of one workstream,
    /// head-first, containing `selected_node` (the anchor). The diff pane
    /// renders it as one combined diff.
    pub selected_range: Option<Vec<String>>,
    /// Workstream expanded in the workbench; `None` falls back to the
    /// active (working-copy) workstream.
    pub focused_workstream: Option<String>,
    pub last_mutation: Option<Breadcrumb>,
    /// The conflict currently handed to the external merge tool
    /// (change id, path), for the whole time the tool's window is open.
    pub resolving_conflict: Option<(String, String)>,
    pub error: Option<String>,
    /// The annotate pane: which change's tree and file are being read
    /// line by line.
    pub annotate: Option<(String, String)>,
    /// The evolution pane: which change's rewrite history is on the diff
    /// surface.
    pub evolution: Option<String>,
}

impl Default for TabUi {
    fn default() -> Self {
        Self {
            section: Section::Workbench,
            selected_node: None,
            selected_range: None,
            focused_workstream: None,
            last_mutation: None,
            resolving_conflict: None,
            error: None,
            annotate: None,
            evolution: None,
        }
    }
}

pub struct Tab {
    /// Stable identity for keyed rendering and activation.
    pub id: u64,
    /// Canonical repo path; `None` while the tab is empty (the welcome
    /// screen).
    pub path: Option<String>,
    /// Label for the tab strip; `None` until the repo names itself.
    pub name: Option<String>,
    /// Latest snapshot the host published for this repo, kept fresh in the
    /// background by the repo's own watcher.
    pub snapshot: Option<Arc<RepoSnapshot>>,
    pub ui: TabUi,
    pub opening: bool,
    /// Whether the tab ever showed a selection of its own.
    seen: bool,
}

/// The tab strip's label: the repo's name, its path's last segment while
/// the name has not arrived, or the empty-tab placeholder.
pub fn tab_label(name: Option<&str>, path: Option<&str>) -> String {
    if let Some(name) = name.filter(|name| !name.is_empty()) {
        return name.to_owned();
    }
    path.and_then(|path| path.trim_end_matches('/').rsplit('/').next())
        .filter(|base| !base.is_empty())
        .unwrap_or("New Tab")
        .to_owned()
}

#[derive(Debug, Clone, PartialEq)]
pub struct TabTitle {
    pub label: String,
    /// The enclosing directories that tell this tab apart from others
    /// sharing its label; `None` while the label is unique.
    pub hint: Option<String>,
}

/// Labels for a strip of tabs, disambiguated the way editors do for
/// same-named files: repos sharing a name each gain the shortest run of
/// parent directories that no other shares.
pub fn tab_titles(tabs: &[(u64, Option<&str>, Option<&str>)]) -> Vec<(u64, TabTitle)> {
    let mut titles: Vec<(u64, TabTitle)> = tabs
        .iter()
        .map(|&(id, name, path)| {
            (
                id,
                TabTitle {
                    label: tab_label(name, path),
                    hint: None,
                },
            )
        })
        .collect();
    let parents: Vec<Option<Vec<&str>>> = tabs
        .iter()
        .map(|&(_, _, path)| {
            path.map(|path| {
                let mut segments: Vec<&str> = path.trim_end_matches('/').split('/').collect();
                segments.pop();
                segments
            })
        })
        .collect();
    let tail = |segments: &[&str], depth: usize| {
        segments[segments.len().saturating_sub(depth)..].join("/")
    };
    for ix in 0..tabs.len() {
        let Some(mine) = &parents[ix] else { continue };
        let group: Vec<usize> = (0..tabs.len())
            .filter(|&other| {
                other != ix
                    && parents[other].is_some()
                    && titles[other].1.label == titles[ix].1.label
            })
            .collect();
        if group.is_empty() {
            continue;
        }
        let mut depth = 1;
        while depth < mine.len()
            && group
                .iter()
                .any(|&other| tail(parents[other].as_ref().unwrap(), depth) == tail(mine, depth))
        {
            depth += 1;
        }
        let hint = tail(mine, depth);
        titles[ix].1.hint = Some(if hint.is_empty() { "/".into() } else { hint });
    }
    titles
}

#[derive(Clone, Debug)]
struct ClosedTab {
    path: String,
}

/// What changed, for views that derive expensive state.
#[derive(Clone, Debug, PartialEq)]
pub enum AppEvent {
    /// The active tab or its snapshot changed.
    Snapshot,
    /// The workbench selection moved.
    Selection,
    /// A new intent is pending.
    Intent,
    /// The active tab or section changed: section views rebuild.
    Navigation,
}

pub struct AppModel {
    pub host: Arc<Host>,
    pub store: Store,
    pub window: Option<AnyWindowHandle>,

    pub tabs: Vec<Tab>,
    pub active_id: u64,
    closed: Vec<ClosedTab>,
    next_tab_id: u64,

    pub sidebar_collapsed: bool,
    pub graph_collapsed: bool,
    pub recent_repos: Vec<RecentRepo>,
    /// Command palette (⌘K), the keyboard route to every action.
    pub palette_open: bool,
    /// History search overlay (⌘⇧F) and the query it opens with.
    pub search_open: bool,
    pub search_seed: String,
    /// The last query typed there, for the next open.
    pub search_last: String,
    /// Pending request for another surface; see [`UiIntent`].
    pub intent: Option<UiIntent>,

    pub forge: ForgeUi,
    pub upstream: UpstreamUi,
    pub autoland: AutoLandUi,
    pub cli: CliUi,
    pub editors: Vec<AppInfo>,
    pub terminals: Vec<AppInfo>,
    pub license: LicenseUi,
    pub update: UpdateUi,
    /// The registration popover in the top bar.
    pub registration_open: bool,
    /// What a graph drag in progress would do, for the status line.
    pub drag_hint: Option<DragHint>,
    /// The context menu on screen.
    pub menu: Option<OpenMenu>,
    menu_seq: u64,
    pub diffs: DiffCache,
    /// Description drafts an editor held when it closed, by (repo, change):
    /// the description the draft started from, and its text. A draft
    /// survives selection moves and tab switches; it is offered back only
    /// while the change still has the description it started from.
    pub drafts: std::collections::HashMap<(String, String), (String, String)>,
    pub mono_font: SharedString,
}

/// Which diff: a repo at one operation, a change, and what it is measured
/// from (`None` is its parent).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct DiffKey {
    pub repo: String,
    pub op: String,
    pub from: Option<String>,
    pub id: String,
}

impl DiffKey {
    /// The same comparison, whatever operation it was read at.
    pub fn same_comparison(&self, other: &DiffKey) -> bool {
        self.repo == other.repo && self.from == other.from && self.id == other.id
    }
}

/// Diffs are cached per repo operation: re-selecting a change renders
/// without a fetch, and snapshot republishes that record no new operation
/// (view switches, converged refreshes) skip the backend entirely.
#[derive(Default)]
pub struct DiffCache {
    entries: Vec<(DiffKey, Arc<jiji_core::snapshot::ChangeDiff>)>,
    inflight: std::collections::HashSet<DiffKey>,
}

const MAX_CACHED_DIFFS: usize = 16;

impl DiffCache {
    pub fn get(&mut self, key: &DiffKey) -> Option<Arc<jiji_core::snapshot::ChangeDiff>> {
        let at = self.entries.iter().position(|(cached, _)| cached == key)?;
        // Most recently used last.
        let entry = self.entries.remove(at);
        let diff = entry.1.clone();
        self.entries.push(entry);
        Some(diff)
    }

    pub fn insert(&mut self, key: DiffKey, diff: Arc<jiji_core::snapshot::ChangeDiff>) {
        self.inflight.remove(&key);
        self.entries.retain(|(cached, _)| *cached != key);
        self.entries.push((key, diff));
        if self.entries.len() > MAX_CACHED_DIFFS {
            self.entries.remove(0);
        }
    }
}

/// The live meaning of a drag gesture.
#[derive(Clone, Debug, PartialEq)]
pub struct DragHint {
    /// `Some(true)` a drop here applies, `Some(false)` it is refused,
    /// `None` nothing is under the pointer yet.
    pub allowed: Option<bool>,
    pub text: String,
}

impl EventEmitter<AppEvent> for AppModel {}

struct AppGlobal(Entity<AppModel>);

impl Global for AppGlobal {}

/// The app model.
pub fn app(cx: &App) -> Entity<AppModel> {
    cx.global::<AppGlobal>().0.clone()
}

impl AppModel {
    /// Build the model, restore the previous session's tabs and start the
    /// background rhythms.
    pub fn install(store: Store, mono_font: SharedString, cx: &mut App) -> Entity<Self> {
        let (host, mut events) = Host::new(store.dir().to_path_buf());
        let model = cx.new(|cx| {
            let mut this = Self {
                sidebar_collapsed: store.flag("rail.hidden"),
                graph_collapsed: store.flag("graph.collapsed"),
                recent_repos: store.recent_repos(),
                upstream: UpstreamUi::new(&store),
                cli: CliUi::new(&store),
                license: LicenseUi::load(&store),
                host,
                store,
                window: None,
                tabs: Vec::new(),
                active_id: 0,
                closed: Vec::new(),
                next_tab_id: 1,
                palette_open: false,
                search_open: false,
                search_seed: String::new(),
                search_last: String::new(),
                intent: None,
                forge: ForgeUi::default(),
                autoland: AutoLandUi::default(),
                editors: Vec::new(),
                terminals: Vec::new(),
                update: UpdateUi::default(),
                registration_open: false,
                drag_hint: None,
                menu: None,
                menu_seq: 0,
                diffs: DiffCache::default(),
                drafts: Default::default(),
                mono_font,
            };
            this.bootstrap(cx);
            this
        });
        cx.set_global(AppGlobal(model.clone()));

        // App-lifetime subscription: every snapshot the host publishes
        // lands on the tab of the repo it belongs to.
        let weak = model.downgrade();
        cx.spawn(async move |cx| {
            while let Some(event) = events.next().await {
                let delivered = weak.update(cx, |this, cx| match event {
                    HostEvent::Snapshot(snapshot) => this.apply_snapshot(snapshot, cx),
                    HostEvent::AutoLand(statuses) => this.set_autoland_jobs(statuses, cx),
                });
                if delivered.is_err() {
                    break;
                }
            }
        })
        .detach();
        model
    }

    fn bootstrap(&mut self, cx: &mut Context<Self>) {
        self.apply_theme(cx);
        self.autoland.jobs = self.host.autoland_state();

        // Restore the previous session's tabs. The active one opens right
        // away; background tabs load lazily on first activation.
        let persisted = self.store.tabs();
        for repo in &persisted.repos {
            self.create_tab(Some(repo.path.clone()), Some(repo.name.clone()));
        }
        if self.tabs.is_empty() {
            self.create_tab(None, None);
        }
        let active = persisted.active.min(self.tabs.len() - 1);
        self.active_id = self.tabs[active].id;
        if let Some(path) = self.tabs[active].path.clone() {
            self.open_repo_into(self.active_id, path, cx);
        }

        self.sync_cli_status(cx);
        self.sync_integrations(cx);
        self.revalidate_license(cx);
        self.start_upstream_checks(cx);
        self.start_update_checks(cx);
    }

    /// Run a blocking host call off the UI thread.
    pub fn bg<T: Send + 'static>(
        &self,
        cx: &App,
        call: impl FnOnce(&Arc<Host>) -> T + Send + 'static,
    ) -> Task<T> {
        let host = self.host.clone();
        cx.background_spawn(async move { call(&host) })
    }

    // ── The active tab ──

    pub fn tab(&self) -> &Tab {
        self.tabs
            .iter()
            .find(|tab| tab.id == self.active_id)
            .unwrap_or(&self.tabs[0])
    }

    pub fn tab_mut(&mut self) -> &mut Tab {
        let ix = self
            .tabs
            .iter()
            .position(|tab| tab.id == self.active_id)
            .unwrap_or(0);
        &mut self.tabs[ix]
    }

    fn tab_by_id(&mut self, id: u64) -> Option<&mut Tab> {
        self.tabs.iter_mut().find(|tab| tab.id == id)
    }

    fn tab_for_path(&mut self, path: &str) -> Option<&mut Tab> {
        self.tabs
            .iter_mut()
            .find(|tab| tab.path.as_deref() == Some(path))
    }

    pub fn snapshot(&self) -> Option<&Arc<RepoSnapshot>> {
        self.tab().snapshot.as_ref()
    }

    /// The active tab's repo path — what repo-scoped calls target.
    pub fn repo_path(&self) -> Option<String> {
        self.tab().path.clone()
    }

    pub fn ui(&self) -> &TabUi {
        &self.tab().ui
    }

    pub fn section(&self) -> Section {
        self.tab().ui.section
    }

    /// What keys the section views: they rebuild when it changes, so no
    /// view-local state (open panels, scroll) leaks between repos.
    pub fn view_key(&self) -> (u64, Option<Section>) {
        let tab = self.tab();
        (tab.id, tab.snapshot.as_ref().map(|_| tab.ui.section))
    }

    fn create_tab(&mut self, path: Option<String>, name: Option<String>) -> u64 {
        let id = self.next_tab_id;
        self.next_tab_id += 1;
        self.tabs.push(Tab {
            id,
            path,
            name: name.filter(|name| !name.is_empty()),
            snapshot: None,
            ui: TabUi::default(),
            opening: false,
            seen: false,
        });
        id
    }

    /// Only repo tabs survive a restart (empty tabs are ephemeral).
    fn persist_tabs(&mut self) {
        let repos: Vec<&Tab> = self.tabs.iter().filter(|tab| tab.path.is_some()).collect();
        let active = repos
            .iter()
            .position(|tab| tab.id == self.active_id)
            .unwrap_or(0);
        let payload = PersistedTabs {
            repos: repos
                .iter()
                .map(|tab| PersistedRepo {
                    path: tab.path.clone().unwrap_or_default(),
                    name: tab.name.clone().unwrap_or_default(),
                })
                .collect(),
            active,
        };
        self.store.set_tabs(&payload);
    }

    // ── Snapshots ──

    /// Route a published snapshot to its repo's tab; the active tab also
    /// puts it on screen. A tab's first snapshot is its open moment — land
    /// on the working copy's diff, not an overview. Snapshots for repos no
    /// tab holds (a just-closed tab's late publish) are dropped.
    pub fn apply_snapshot(&mut self, snapshot: Arc<RepoSnapshot>, cx: &mut Context<Self>) {
        let active_id = self.active_id;
        let Some(tab) = self.tab_for_path(&snapshot.repo_path) else {
            return;
        };
        // A republish of what the tab already holds changes nothing.
        if tab
            .snapshot
            .as_ref()
            .is_some_and(|current| Arc::ptr_eq(current, &snapshot))
        {
            return;
        }
        let first = tab.snapshot.is_none();
        tab.name = Some(snapshot.repo_name.clone());
        tab.snapshot = Some(snapshot);
        let is_active = tab.id == active_id;
        if first || !tab.seen {
            Self::select_working_copy(tab);
        }
        Self::settle_selection(tab);
        if !is_active {
            return;
        }
        tab.ui.error = None;
        self.follow_repo(cx);
        cx.emit(AppEvent::Snapshot);
        if first {
            cx.emit(AppEvent::Navigation);
        }
        cx.notify();
    }

    /// Opening a repo lands on the diff of the current work. On a
    /// divergent working copy (nodes keyed by commit id) or a view lens
    /// that hides it, nothing resolves and the overview stays.
    fn select_working_copy(tab: &mut Tab) {
        let Some(snapshot) = tab.snapshot.clone() else {
            return;
        };
        tab.seen = true;
        let id = &snapshot.working_copy;
        if find_node(&snapshot, id).is_none() {
            return;
        }
        tab.ui.selected_node = Some(id.clone());
        tab.ui.selected_range = None;
        if let Some(position) = stack_position(&snapshot, id) {
            tab.ui.focused_workstream = Some(position.workstream.id.clone());
        }
    }

    /// Drop selection state a refreshed snapshot no longer supports.
    fn settle_selection(tab: &mut Tab) {
        let Some(snapshot) = tab.snapshot.as_ref() else {
            return;
        };
        if let Some(id) = &tab.ui.selected_node {
            if find_node(snapshot, id).is_none() {
                tab.ui.selected_node = None;
                tab.ui.selected_range = None;
            }
        }
        if let Some(range) = &tab.ui.selected_range {
            if !crate::logic::inspect::valid_range(snapshot, range) {
                tab.ui.selected_range = None;
            }
        }
        if let Some(stream) = &tab.ui.focused_workstream {
            if !snapshot.workstreams.iter().any(|w| &w.id == stream) {
                tab.ui.focused_workstream = None;
            }
        }
        // Annotate and evolution read one selected change; they close
        // when it stops being the selection.
        let selected = tab.ui.selected_node.as_deref();
        if tab
            .ui
            .annotate
            .as_ref()
            .is_some_and(|(change, _)| Some(change.as_str()) != selected)
        {
            tab.ui.annotate = None;
        }
        if tab
            .ui
            .evolution
            .as_ref()
            .is_some_and(|change| Some(change.as_str()) != selected)
        {
            tab.ui.evolution = None;
        }
    }

    /// The shell-level state that follows the repo on screen: the forge
    /// connection and the upstream check.
    fn follow_repo(&mut self, cx: &mut Context<Self>) {
        let (remotes, path) = match self.snapshot() {
            Some(snapshot) => (
                Some(snapshot.git_remotes.clone()),
                Some(snapshot.repo_path.clone()),
            ),
            None => (None, None),
        };
        let has_remotes = remotes.as_ref().is_some_and(|remotes| !remotes.is_empty());
        self.sync_forge_connection(remotes, cx);
        self.sync_upstream_repo(path, has_remotes, cx);
    }

    // ── Tabs ──

    fn switch_to_tab(&mut self, id: u64, cx: &mut Context<Self>) {
        if self.active_id == id || !self.tabs.iter().any(|tab| tab.id == id) {
            return;
        }
        self.active_id = id;
        self.intent = None;
        let tab = self.tab_mut();
        tab.ui.selected_range = None;
        if tab.snapshot.is_some() && !tab.seen {
            Self::select_working_copy(tab);
        }
        self.persist_tabs();
        self.follow_repo(cx);
        cx.emit(AppEvent::Snapshot);
        cx.emit(AppEvent::Navigation);
        cx.notify();
    }

    /// Bring a repo tab's content up: open fresh when it has no snapshot,
    /// or quietly re-sync a tab that already has one so a missed watcher
    /// beat cannot leave stale state on screen.
    fn ensure_tab_loaded(&mut self, id: u64, cx: &mut Context<Self>) {
        let Some(tab) = self.tab_by_id(id) else {
            return;
        };
        let Some(path) = tab.path.clone() else {
            return;
        };
        if tab.snapshot.is_some() {
            self.bg(cx, move |host| {
                let _ = host.refresh_snapshot(&path);
            })
            .detach();
        } else {
            self.open_repo_into(id, path, cx);
        }
    }

    pub fn activate_tab(&mut self, id: u64, cx: &mut Context<Self>) {
        if self.active_id == id {
            return;
        }
        self.switch_to_tab(id, cx);
        self.ensure_tab_loaded(id, cx);
    }

    pub fn new_tab(&mut self, cx: &mut Context<Self>) {
        let id = self.create_tab(None, None);
        self.switch_to_tab(id, cx);
    }

    /// Close a tab; its host session drops (the watcher stops, a live
    /// auto-land job for the repo detaches into its resumable record). The
    /// last tab never closes into nothing — it gives way to an empty tab,
    /// except a lone empty tab, which just stays.
    pub fn close_tab(&mut self, id: u64, cx: &mut Context<Self>) {
        let Some(ix) = self.tabs.iter().position(|tab| tab.id == id) else {
            return;
        };
        if self.tabs.len() == 1 && self.tabs[ix].path.is_none() {
            return;
        }
        let was_active = self.active_id == id;
        let tab = self.tabs.remove(ix);
        if let Some(path) = tab.path {
            self.closed.retain(|closed| closed.path != path);
            self.closed.push(ClosedTab { path: path.clone() });
            if self.closed.len() > MAX_CLOSED {
                self.closed.remove(0);
            }
            self.bg(cx, move |host| host.close_repo(&path)).detach();
        }
        if self.tabs.is_empty() {
            self.create_tab(None, None);
        }
        if was_active {
            let neighbor = self.tabs[ix.min(self.tabs.len() - 1)].id;
            self.active_id = 0;
            self.switch_to_tab(neighbor, cx);
            self.ensure_tab_loaded(neighbor, cx);
        }
        self.persist_tabs();
        cx.notify();
    }

    pub fn close_active_tab(&mut self, cx: &mut Context<Self>) {
        self.close_tab(self.active_id, cx);
    }

    /// The label of the closed tab ⌘⇧T would reopen.
    pub fn reopenable_tab(&self) -> Option<String> {
        self.closed
            .iter()
            .rev()
            .find(|closed| {
                !self
                    .tabs
                    .iter()
                    .any(|tab| tab.path.as_deref() == Some(&closed.path))
            })
            .map(|closed| tab_label(None, Some(&closed.path)))
    }

    /// Bring back the most recently closed repo tab (⌘⇧T), newest first
    /// on repeat — skipping repos reopened another way meanwhile.
    pub fn reopen_closed_tab(&mut self, cx: &mut Context<Self>) {
        while let Some(closed) = self.closed.pop() {
            let open = self
                .tabs
                .iter()
                .any(|tab| tab.path.as_deref() == Some(&closed.path));
            if !open {
                self.open_repo_at(closed.path, cx);
                return;
            }
        }
    }

    /// Cycle the active tab (⌃Tab / ⌘⇧] with +1, reversed with -1).
    pub fn cycle_tab(&mut self, delta: isize, cx: &mut Context<Self>) {
        let count = self.tabs.len() as isize;
        if count < 2 {
            return;
        }
        let Some(ix) = self.tabs.iter().position(|tab| tab.id == self.active_id) else {
            return;
        };
        let next = (ix as isize + delta).rem_euclid(count) as usize;
        self.activate_tab(self.tabs[next].id, cx);
    }

    /// Open a repo somewhere sensible: the tab that already shows it, the
    /// active tab while it is empty, or a new tab.
    pub fn open_repo_at(&mut self, path: String, cx: &mut Context<Self>) {
        if let Some(existing) = self.tab_for_path(&path).map(|tab| tab.id) {
            self.activate_tab(existing, cx);
            return;
        }
        let id = if self.tab().path.is_none() {
            self.active_id
        } else {
            let id = self.create_tab(None, None);
            self.switch_to_tab(id, cx);
            id
        };
        self.open_repo_into(id, path, cx);
    }

    /// Open `path` into a tab. The tab can lose the screen, or close,
    /// while the open is in flight: everything after the open names the
    /// opened repo explicitly instead of trusting whichever tab is active
    /// by then.
    fn open_repo_into(&mut self, tab_id: u64, path: String, cx: &mut Context<Self>) {
        if let Some(tab) = self.tab_by_id(tab_id) {
            tab.opening = true;
            tab.ui.error = None;
        }
        cx.notify();
        let opened = self.bg(cx, {
            let path = path.clone();
            move |host| host.open_repo(&path)
        });
        cx.spawn(async move |this, cx| {
            let result = opened.await;
            let _ = this.update(cx, |this, cx| this.finish_open(tab_id, result, cx));
        })
        .detach();
    }

    fn finish_open(
        &mut self,
        tab_id: u64,
        result: HostResult<Arc<RepoSnapshot>>,
        cx: &mut Context<Self>,
    ) {
        let snapshot = match result {
            Ok(snapshot) => snapshot,
            Err(error) => {
                let Some(tab) = self.tab_by_id(tab_id) else {
                    return;
                };
                tab.opening = false;
                if error.code == "git_repo_without_jj" {
                    if let Some(root) = error.path.clone() {
                        tab.ui.error = Some(error.message);
                        cx.notify();
                        self.offer_jj_setup(tab_id, root, cx);
                        return;
                    }
                }
                tab.ui.error = Some(error.message);
                cx.notify();
                return;
            }
        };
        let repo_path = snapshot.repo_path.clone();
        if !self.tabs.iter().any(|tab| tab.id == tab_id) {
            // Closed mid-open: the session the host just started has no
            // tab to feed, so drop it — unless another tab holds the repo.
            if self.tab_for_path(&repo_path).is_none() {
                self.bg(cx, move |host| host.close_repo(&repo_path))
                    .detach();
            }
            return;
        }
        // The requested path may be an alias of the canonical root of a
        // repo already open in another tab — fold into that tab instead of
        // showing the same repo twice.
        let dupe = self
            .tabs
            .iter()
            .find(|tab| tab.id != tab_id && tab.path.as_deref() == Some(&repo_path))
            .map(|tab| tab.id);
        if let Some(dupe) = dupe {
            let was_active = self.active_id == tab_id;
            self.tabs.retain(|tab| tab.id != tab_id);
            if was_active {
                self.active_id = 0;
                self.switch_to_tab(dupe, cx);
            }
            self.apply_snapshot(snapshot, cx);
            self.persist_tabs();
            cx.notify();
            return;
        }
        if let Some(tab) = self.tab_by_id(tab_id) {
            tab.path = Some(repo_path.clone());
            tab.opening = false;
            tab.ui.section = Section::Workbench;
        }
        self.apply_snapshot(snapshot.clone(), cx);
        self.persist_tabs();
        self.recent_repos = self.store.remember_repo(&repo_path, &snapshot.repo_name);
        cx.notify();

        // The lens choice sticks per repo; a fresh session opens on the
        // default, so re-apply the persisted one. A choice that stopped
        // working (a custom revset naming a deleted bookmark) falls back
        // silently.
        let lens = self.store.repo_view(&repo_path);
        if lens != SnapshotView::Workstreams && lens != snapshot.view {
            let applied = self.bg(cx, {
                let repo_path = repo_path.clone();
                move |host| host.set_snapshot_view(&repo_path, lens)
            });
            cx.spawn(async move |this, cx| {
                let result = applied.await;
                let _ = this.update(cx, |this, cx| match result {
                    Ok(snapshot) => this.apply_snapshot(snapshot, cx),
                    // A tab closed meanwhile refuses because its session
                    // is gone, not because the lens broke.
                    Err(_) if this.tab_for_path(&repo_path).is_some() => {
                        this.store
                            .set_repo_view(&repo_path, &SnapshotView::Workstreams);
                    }
                    Err(_) => {}
                });
            })
            .detach();
        }
    }

    /// A plain Git repository: offer to set jj up in it, the way `jj git
    /// init --colocate` does, then open it. Nothing touches the folder
    /// unless the user says yes; declining leaves the refusal in the
    /// status bar.
    fn offer_jj_setup(&mut self, tab_id: u64, git_root: String, cx: &mut Context<Self>) {
        let Some(window) = self.window else {
            return;
        };
        let name = git_root
            .rsplit(['/', '\\'])
            .find(|segment| !segment.is_empty())
            .unwrap_or(&git_root)
            .to_owned();
        let detail = format!(
            "{git_root} is a Git repository without jj.\n\nJiji can set jj up here the way \
             `jj git init --colocate` does: a .jj folder next to .git, with your branches \
             imported as bookmarks. Git keeps working alongside it."
        );
        let answer = cx.update_window(window, |_, window, cx| {
            window.prompt(
                PromptLevel::Info,
                &format!("Set up jj in \u{201c}{name}\u{201d}?"),
                Some(&detail),
                &["Set up jj", "Cancel"],
                cx,
            )
        });
        let Ok(answer) = answer else {
            return;
        };
        cx.spawn(async move |this, cx| {
            if answer.await != Ok(0) {
                return;
            }
            let Ok(init) = this.update(cx, |this, cx| {
                if let Some(tab) = this.tab_by_id(tab_id) {
                    tab.opening = true;
                    tab.ui.error = None;
                }
                cx.notify();
                this.bg(cx, move |host| host.init_jj_repo(&git_root))
            }) else {
                return;
            };
            let result = init.await;
            let _ = this.update(cx, |this, cx| match result {
                Ok(root) => this.open_repo_into(tab_id, root, cx),
                Err(error) => {
                    if let Some(tab) = this.tab_by_id(tab_id) {
                        tab.opening = false;
                        tab.ui.error = Some(error.message);
                    }
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// Remove a repo from the recents list (the welcome screen's ×).
    pub fn forget_recent_repo(&mut self, path: &str, cx: &mut Context<Self>) {
        self.recent_repos = self.store.forget_repo(path);
        cx.notify();
    }

    /// The open dialog: a folder, opened like any other route.
    pub fn choose_repo(&mut self, cx: &mut Context<Self>) {
        let picked = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Open".into()),
        });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = picked.await {
                if let Some(path) = paths.into_iter().next() {
                    let _ = this.update(cx, |this, cx| {
                        this.open_repo_at(path.to_string_lossy().into_owned(), cx)
                    });
                }
            }
        })
        .detach();
    }

    /// Folders dropped on the window (or handed over by the `jiji`
    /// launcher) open like picks from the dialog, one tab each.
    pub fn open_paths(&mut self, paths: Vec<PathBuf>, cx: &mut Context<Self>) {
        for path in paths {
            let target = crate::host::repo_root_for(&path).unwrap_or(path);
            self.open_repo_at(target.to_string_lossy().into_owned(), cx);
        }
    }

    /// Switch the workbench's view lens. The choice sticks per repo. The
    /// returned error lets the picker render a refused custom revset
    /// inline; the current snapshot and lens stay as they were.
    pub fn set_snapshot_view(
        &mut self,
        view: SnapshotView,
        persist: bool,
        cx: &mut Context<Self>,
    ) -> Task<HostResult<()>> {
        let Some(path) = self.repo_path() else {
            return Task::ready(Err(no_repo_open()));
        };
        let applied = self.bg(cx, {
            let (path, view) = (path.clone(), view.clone());
            move |host| host.set_snapshot_view(&path, view)
        });
        cx.spawn(async move |this, cx| {
            let snapshot = applied.await?;
            let _ = this.update(cx, |this, cx| {
                this.apply_snapshot(snapshot, cx);
                if persist {
                    this.store.set_repo_view(&path, &view);
                }
            });
            Ok(())
        })
    }

    /// ⌘R: the fast local refresh, plus a re-ask of GitHub for PR state
    /// (deliberately not awaited — the local snapshot must not wait on the
    /// network). The git fetch itself stays with the upstream check.
    pub fn refresh_snapshot(&mut self, cx: &mut Context<Self>) {
        let Some(path) = self.repo_path() else {
            return;
        };
        if self.snapshot().is_none() {
            return;
        }
        self.refresh_forge_prs(cx);
        let refreshed = self.bg(cx, {
            let path = path.clone();
            move |host| host.refresh_snapshot(&path)
        });
        cx.spawn(async move |this, cx| {
            if let Err(error) = refreshed.await {
                let _ = this.update(cx, |this, cx| this.set_error_for(&path, error.message, cx));
            }
        })
        .detach();
    }

    fn set_error_for(&mut self, path: &str, message: String, cx: &mut Context<Self>) {
        if let Some(tab) = self.tab_for_path(path) {
            tab.ui.error = Some(message);
            cx.notify();
        }
    }

    /// Read one diff, from the cache when it has it. The backend call and
    /// the row-model build both happen off the UI thread.
    pub fn fetch_diff(
        &mut self,
        key: DiffKey,
        cx: &mut Context<Self>,
    ) -> Task<
        HostResult<(
            Arc<jiji_core::snapshot::ChangeDiff>,
            crate::diff::model::DiffDoc,
        )>,
    > {
        let cached = self.diffs.get(&key);
        let host = self.host.clone();
        let fetch_key = key.clone();
        let fetched = cx.background_spawn(async move {
            let diff = match cached {
                Some(diff) => diff,
                None => {
                    let path = Path::new(&fetch_key.repo);
                    let backend = host.backend();
                    Arc::new(match &fetch_key.from {
                        Some(from) => backend.compare_diff(path, from, &fetch_key.id),
                        None => backend.change_diff(path, &fetch_key.id),
                    }?)
                }
            };
            let doc = crate::diff::model::DiffDoc::build(&diff);
            Ok::<_, CommandError>((diff, doc))
        });
        cx.spawn(async move |this, cx| {
            let (diff, doc) = fetched.await?;
            let _ = this.update(cx, |this, _| this.diffs.insert(key, diff.clone()));
            Ok((diff, doc))
        })
    }

    /// Warm the cache for a diff the reader is one step away from.
    pub fn prefetch_diff(&mut self, key: DiffKey, cx: &mut Context<Self>) {
        if self.diffs.get(&key).is_some() || !self.diffs.inflight.insert(key.clone()) {
            return;
        }
        let host = self.host.clone();
        let fetch_key = key.clone();
        let fetched = cx.background_spawn(async move {
            let path = Path::new(&fetch_key.repo);
            match &fetch_key.from {
                Some(from) => host.backend().compare_diff(path, from, &fetch_key.id),
                None => host.backend().change_diff(path, &fetch_key.id),
            }
        });
        cx.spawn(async move |this, cx| {
            let result = fetched.await;
            let _ = this.update(cx, |this, _| match result {
                Ok(diff) => this.diffs.insert(key, Arc::new(diff)),
                Err(_) => {
                    this.diffs.inflight.remove(&key);
                }
            });
        })
        .detach();
    }

    /// Keep what a describe editor held when it closed; text identical to
    /// the description it started from is no draft at all.
    pub fn keep_draft(&mut self, repo: &str, change: &str, base: &str, text: &str) {
        let key = (repo.to_owned(), change.to_owned());
        if text == base {
            self.drafts.remove(&key);
        } else {
            self.drafts.insert(key, (base.to_owned(), text.to_owned()));
        }
    }

    /// The kept draft for a change whose description is still the one the
    /// draft started from.
    pub fn draft_for(&self, repo: &str, change: &str, description: &str) -> Option<&str> {
        self.drafts
            .get(&(repo.to_owned(), change.to_owned()))
            .filter(|(base, _)| base == description)
            .map(|(_, text)| text.as_str())
    }

    pub fn discard_draft(&mut self, repo: &str, change: &str) {
        self.drafts.remove(&(repo.to_owned(), change.to_owned()));
    }

    pub fn set_error(&mut self, message: impl Into<String>, cx: &mut Context<Self>) {
        self.tab_mut().ui.error = Some(message.into());
        cx.notify();
    }

    // ── Mutations ──

    /// Every mutation flows through this shape: backend action → refreshed
    /// snapshot → breadcrumb → selection follows the outcome's target (the
    /// new working copy after `new`, the parent after abandon or squash —
    /// without this the selection would dangle on a removed node). Errors
    /// come back to the caller so the initiating surface can show them
    /// inline (its editor or confirm panel stays open).
    pub fn mutate(
        &mut self,
        cx: &mut Context<Self>,
        action: impl FnOnce(&dyn RepoBackend, &Path) -> Result<MutationOutcome, BackendError>
            + Send
            + 'static,
    ) -> Task<HostResult<MutationOutcome>> {
        // The mutation targets the repo on screen when it starts; if
        // another tab takes over mid-flight, results land on that repo's
        // tab instead of the one now showing.
        let Some(path) = self.repo_path() else {
            return Task::ready(Err(no_repo_open()));
        };
        let ran = self.bg(cx, {
            let path = path.clone();
            move |host| host.mutate(&path, action)
        });
        cx.spawn(async move |this, cx| {
            let outcome = ran.await?;
            let _ = this.update(cx, |this, cx| this.after_mutation(&path, &outcome, cx));
            Ok(outcome)
        })
    }

    fn after_mutation(&mut self, path: &str, outcome: &MutationOutcome, cx: &mut Context<Self>) {
        // The refreshed snapshot also arrives as a host event; pulling it
        // here makes the flow self-contained and deterministic.
        if let Some(snapshot) = self.host.snapshot_for(path) {
            self.apply_snapshot(snapshot, cx);
        }
        let Some(tab) = self.tab_for_path(path) else {
            return;
        };
        if outcome.operation_id.is_some() {
            tab.ui.last_mutation = Some(Breadcrumb {
                outcome: outcome.clone(),
                at: Instant::now(),
            });
        }
        if let (Some(target), Some(snapshot)) = (&outcome.target_change, tab.snapshot.clone()) {
            if find_node(&snapshot, target).is_some() {
                tab.ui.selected_node = Some(target.clone());
                tab.ui.selected_range = None;
                if let Some(position) = stack_position(&snapshot, target) {
                    tab.ui.focused_workstream = Some(position.workstream.id.clone());
                }
                Self::settle_selection(tab);
            }
        }
        cx.emit(AppEvent::Selection);
        cx.notify();
    }

    /// Run a mutation whose surface has nowhere to render an error inline
    /// (palette rows, menu items, drag and drop): failures land in the
    /// status bar's error slot.
    pub fn mutate_quiet(
        &mut self,
        cx: &mut Context<Self>,
        action: impl FnOnce(&dyn RepoBackend, &Path) -> Result<MutationOutcome, BackendError>
            + Send
            + 'static,
    ) {
        let path = self.repo_path();
        let task = self.mutate(cx, action);
        self.report(path, task, cx);
    }

    /// Send a task's failure to the status bar of the tab it started on.
    pub fn report<T: 'static>(
        &mut self,
        path: Option<String>,
        task: Task<HostResult<T>>,
        cx: &mut Context<Self>,
    ) {
        cx.spawn(async move |this: WeakEntity<Self>, cx| {
            if let Err(error) = task.await {
                let _ = this.update(cx, |this, cx| match &path {
                    Some(path) => this.set_error_for(path, error.message, cx),
                    None => this.set_error(error.message, cx),
                });
            }
        })
        .detach();
    }

    /// Hand one conflicted file to the external merge tool and wait for
    /// the tool's window to close. `resolving_conflict` stays set the
    /// whole time so every Resolve affordance shows the waiting state.
    pub fn resolve_conflict(
        &mut self,
        change_id: String,
        file_path: String,
        cx: &mut Context<Self>,
    ) -> Task<HostResult<MutationOutcome>> {
        let repo = self.repo_path();
        self.tab_mut().ui.resolving_conflict = Some((change_id.clone(), file_path.clone()));
        cx.notify();
        let task = self.mutate(cx, move |backend, path| {
            backend.resolve_conflict(path, &change_id, &file_path)
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                // The waiting state belongs to the repo's tab, wherever
                // that tab is by now.
                if let Some(tab) = repo.as_deref().and_then(|repo| this.tab_for_path(repo)) {
                    tab.ui.resolving_conflict = None;
                }
                cx.notify();
            });
            result
        })
    }

    /// The breadcrumb's Undo: revert the operation the last mutation
    /// recorded. The undo itself records an operation and becomes the new
    /// breadcrumb, so its own Undo is redo.
    pub fn undo_last_mutation(&mut self, cx: &mut Context<Self>) {
        let Some(op_id) = self
            .ui()
            .last_mutation
            .as_ref()
            .and_then(|crumb| crumb.outcome.operation_id.clone())
        else {
            return;
        };
        self.mutate_quiet(cx, move |backend, path| {
            backend.revert_operation(path, &op_id)
        });
    }

    pub fn dismiss_breadcrumb(&mut self, cx: &mut Context<Self>) {
        self.tab_mut().ui.last_mutation = None;
        cx.notify();
    }

    pub fn dismiss_error(&mut self, cx: &mut Context<Self>) {
        self.tab_mut().ui.error = None;
        cx.notify();
    }

    // ── Navigation ──

    pub fn go_to_section(&mut self, section: Section, cx: &mut Context<Self>) {
        if self.snapshot().is_none() || self.section() == section {
            return;
        }
        self.tab_mut().ui.section = section;
        cx.emit(AppEvent::Navigation);
        cx.notify();
    }

    pub fn toggle_sidebar(&mut self, cx: &mut Context<Self>) {
        self.sidebar_collapsed = !self.sidebar_collapsed;
        self.store.set_pref("rail.hidden", &self.sidebar_collapsed);
        cx.notify();
    }

    pub fn toggle_graph_panel(&mut self, cx: &mut Context<Self>) {
        self.graph_collapsed = !self.graph_collapsed;
        self.store
            .set_pref("graph.collapsed", &self.graph_collapsed);
        cx.notify();
    }

    pub fn toggle_palette(&mut self, cx: &mut Context<Self>) {
        self.palette_open = !self.palette_open;
        if self.palette_open {
            self.search_open = false;
        }
        cx.notify();
    }

    pub fn close_palette(&mut self, cx: &mut Context<Self>) {
        self.palette_open = false;
        cx.notify();
    }

    pub fn open_search(&mut self, seed: impl Into<String>, cx: &mut Context<Self>) {
        if self.snapshot().is_none() {
            return;
        }
        self.search_seed = seed.into();
        self.search_open = true;
        self.palette_open = false;
        cx.notify();
    }

    pub fn close_search(&mut self, cx: &mut Context<Self>) {
        self.search_open = false;
        cx.notify();
    }

    /// Select one change in the workbench graph (a row click). Moving the
    /// selection collapses any range and closes the reading panes.
    pub fn select_node(&mut self, id: Option<String>, cx: &mut Context<Self>) {
        let tab = self.tab_mut();
        if tab.ui.selected_node == id && tab.ui.selected_range.is_none() {
            return;
        }
        tab.ui.selected_node = id;
        tab.ui.selected_range = None;
        Self::settle_selection(tab);
        cx.emit(AppEvent::Selection);
        cx.notify();
    }

    /// ⇧-click: extend from the anchor (the selected change) along its
    /// lane. Falls back to a plain select when the two share no lane.
    pub fn extend_selection(&mut self, target: String, cx: &mut Context<Self>) {
        let Some(snapshot) = self.snapshot().cloned() else {
            return;
        };
        let anchor = self.ui().selected_node.clone();
        let range = anchor
            .as_deref()
            .and_then(|anchor| crate::logic::inspect::extend_range(&snapshot, anchor, &target));
        match range {
            Some(range) if range.len() >= 2 => {
                self.tab_mut().ui.selected_range = Some(range);
                cx.emit(AppEvent::Selection);
                cx.notify();
            }
            Some(_) => {
                self.tab_mut().ui.selected_range = None;
                cx.emit(AppEvent::Selection);
                cx.notify();
            }
            None => self.select_node(Some(target), cx),
        }
    }

    pub fn focus_workstream(&mut self, id: Option<String>, cx: &mut Context<Self>) {
        self.tab_mut().ui.focused_workstream = id;
        cx.notify();
    }

    /// Move the workbench selection to a change (the palette's "go to"),
    /// focusing its workstream like a graph-row click would.
    pub fn jump_to_change(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(snapshot) = self.snapshot().cloned() else {
            return;
        };
        if find_node(&snapshot, id).is_none() {
            return;
        }
        let navigated = self.section() != Section::Workbench;
        let tab = self.tab_mut();
        tab.ui.section = Section::Workbench;
        tab.ui.selected_node = Some(id.to_owned());
        tab.ui.selected_range = None;
        if let Some(position) = stack_position(&snapshot, id) {
            tab.ui.focused_workstream = Some(position.workstream.id.clone());
        }
        Self::settle_selection(tab);
        if navigated {
            cx.emit(AppEvent::Navigation);
        }
        cx.emit(AppEvent::Selection);
        cx.notify();
    }

    /// Select a change a search hit names, wherever it is: a change the
    /// current lens draws is a plain jump; one it does not re-renders the
    /// graph under a transient custom revset pinning that change.
    /// Deliberately not persisted — a refresh or lens switch returns to the
    /// user's own choice.
    pub fn reveal_change(&mut self, id: String, cx: &mut Context<Self>) {
        let drawn = self
            .snapshot()
            .is_some_and(|snapshot| find_node(snapshot, &id).is_some());
        if drawn {
            self.jump_to_change(&id, cx);
            return;
        }
        let path = self.repo_path();
        let applied =
            self.set_snapshot_view(SnapshotView::Custom { revset: id.clone() }, false, cx);
        let task = cx.spawn(async move |this, cx| {
            applied.await?;
            let _ = this.update(cx, |this, cx| this.jump_to_change(&id, cx));
            Ok(())
        });
        self.report(path, task, cx);
    }

    /// Open one file's line-by-line annotation over the diff surface.
    pub fn open_annotate(&mut self, change_id: String, path: String, cx: &mut Context<Self>) {
        let navigated = self.section() != Section::Workbench;
        let tab = self.tab_mut();
        tab.ui.section = Section::Workbench;
        tab.ui.evolution = None;
        tab.ui.annotate = Some((change_id, path));
        if navigated {
            cx.emit(AppEvent::Navigation);
        }
        cx.notify();
    }

    pub fn close_annotate(&mut self, cx: &mut Context<Self>) {
        self.tab_mut().ui.annotate = None;
        cx.notify();
    }

    /// Open a change's evolution timeline over the diff surface. Shares
    /// the surface with annotate, so one replaces the other.
    pub fn open_evolution(&mut self, change_id: String, cx: &mut Context<Self>) {
        let navigated = self.section() != Section::Workbench;
        let tab = self.tab_mut();
        tab.ui.section = Section::Workbench;
        tab.ui.annotate = None;
        tab.ui.evolution = Some(change_id);
        if navigated {
            cx.emit(AppEvent::Navigation);
        }
        cx.notify();
    }

    pub fn close_evolution(&mut self, cx: &mut Context<Self>) {
        self.tab_mut().ui.evolution = None;
        cx.notify();
    }

    /// Every intent switches to the section whose surface owns it — the
    /// workbench for the change/diff panels, Publish for the land and ship
    /// plans — so the owning surface mounts (if it wasn't) and takes it.
    pub fn send_intent(&mut self, intent: UiIntent, cx: &mut Context<Self>) {
        if self.snapshot().is_none() {
            return;
        }
        let section = match intent {
            UiIntent::Land { .. } | UiIntent::Ship { .. } => Section::Publish,
            _ => Section::Workbench,
        };
        let navigated = self.section() != section;
        self.tab_mut().ui.section = section;
        self.intent = Some(intent);
        if navigated {
            cx.emit(AppEvent::Navigation);
        }
        cx.emit(AppEvent::Intent);
        cx.notify();
    }

    /// Take the pending intent when `owns` says it is this surface's.
    pub fn take_intent(&mut self, owns: impl FnOnce(&UiIntent) -> bool) -> Option<UiIntent> {
        if self.intent.as_ref().is_some_and(owns) {
            self.intent.take()
        } else {
            None
        }
    }
}

pub fn no_repo_open() -> CommandError {
    CommandError::new("no_repo_open", "No repository is currently open")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tab_labels_fall_back_to_the_path_then_a_placeholder() {
        assert_eq!(tab_label(Some("jiji"), Some("/x/y")), "jiji");
        assert_eq!(tab_label(None, Some("/work/api/")), "api");
        assert_eq!(tab_label(Some(""), None), "New Tab");
    }

    #[test]
    fn same_named_repos_gain_the_shortest_distinguishing_parents() {
        let titles = tab_titles(&[
            (1, Some("api"), Some("/home/me/work/api")),
            (2, Some("api"), Some("/home/me/oss/api")),
            (3, Some("web"), Some("/home/me/work/web")),
            (4, None, None),
        ]);
        assert_eq!(titles[0].1.hint.as_deref(), Some("work"));
        assert_eq!(titles[1].1.hint.as_deref(), Some("oss"));
        assert_eq!(titles[2].1.hint, None);
        assert_eq!(titles[3].1.label, "New Tab");

        // Sharing the immediate parent too: one more level tells them apart.
        let deeper = tab_titles(&[
            (1, Some("api"), Some("/a/x/src/api")),
            (2, Some("api"), Some("/a/y/src/api")),
        ]);
        assert_eq!(deeper[0].1.hint.as_deref(), Some("x/src"));
        assert_eq!(deeper[1].1.hint.as_deref(), Some("y/src"));
    }
}
