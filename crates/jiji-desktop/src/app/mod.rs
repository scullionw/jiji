//! The app model: one entity holding everything the interface shares —
//! the open-repo tabs, each tab's selection and breadcrumbs, the forge
//! connection, upstream checks, auto-land jobs and preferences. Views read
//! it and call its methods; only this module (and its submodules) writes
//! it.

mod diffs;
mod forge;
mod menu;
mod mutations;
mod navigation;
mod prefs;
mod tabs;

use std::sync::Arc;
use std::time::Instant;

use futures::channel::mpsc::UnboundedReceiver;
use futures::StreamExt as _;
use gpui::{
    AnyWindowHandle, App, AppContext as _, Context, Entity, EventEmitter, Global, SharedString,
    Task,
};
use jiji_core::snapshot::{MutationOutcome, RepoSnapshot, SnapshotView};

pub use self::diffs::{DiffCache, DiffKey, DiffRead};
pub use self::forge::{AutoLandUi, ForgePhase, ForgeUi, UpstreamUi, UPSTREAM_INTERVALS};
pub use self::menu::OpenMenu;
pub use self::prefs::{CliUi, LicenseUi, ThemeMode, UpdateUi};
pub use self::tabs::{tab_label, tab_titles};
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
    /// The section's glyph, on the rail and at the head of its page.
    pub fn glyph(self) -> crate::ui::IconName {
        use crate::ui::IconName;
        match self {
            Section::Workbench => IconName::Workbench,
            Section::Conflicts => IconName::Conflicts,
            Section::Publish => IconName::Publish,
            Section::Bookmarks => IconName::Bookmark,
            Section::Operations => IconName::Operations,
            Section::Workspaces => IconName::Workspaces,
        }
    }

    /// The section's own hue: its rail tile while open, its page's mark.
    /// Drawn from the palette's status and lane colors.
    pub fn hue(self, t: &crate::theme::Theme) -> gpui::Hsla {
        match self {
            Section::Workbench => t.accent_strong,
            Section::Conflicts => t.danger,
            Section::Publish => t.ok,
            Section::Bookmarks => t.lane(4),
            Section::Operations => t.lane(2),
            Section::Workspaces => t.lane(1),
        }
    }

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
    /// Open Publish on the plan to take over someone else's pull request.
    TakeOver {
        number: u64,
    },
    /// Open Publish on the plan to give back one that was taken over.
    GiveBack {
        number: u64,
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
    /// The path an open in flight is bringing into this tab. The tab is
    /// spoken for from the moment the open starts, not from when it lands.
    pub opening: Option<String>,
    /// Whether the tab ever showed a selection of its own.
    seen: bool,
}

impl Tab {
    /// Empty, with nothing on its way in: a repo may open here.
    fn is_free(&self) -> bool {
        self.path.is_none() && self.opening.is_none()
    }
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
    /// The last session ended in a panic (its record is in the log): the
    /// status line says so until dismissed.
    pub crashed: bool,
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
        let host = Host::new(store.dir().to_path_buf());
        Self::install_over(host, store, mono_font, cx)
    }

    /// `install` over a host of the caller's making: tests serve the mock
    /// backend through one.
    pub(crate) fn install_over(
        (host, mut events): (Arc<Host>, UnboundedReceiver<HostEvent>),
        mut store: Store,
        mono_font: SharedString,
        cx: &mut App,
    ) -> Entity<Self> {
        let license = LicenseUi::load(&mut store);
        let crashed = crate::diagnostics::take_crash_mark(store.dir());
        let offline = host.forge.api.offline();
        let model = cx.new(|cx| {
            let mut this = Self {
                sidebar_collapsed: store.flag("rail.hidden"),
                graph_collapsed: store.flag("graph.collapsed"),
                recent_repos: store.recent_repos(),
                upstream: UpstreamUi::new(&store, offline),
                cli: CliUi::new(&store),
                license,
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
                crashed,
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
        self.reread_license(cx);
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
            opening: None,
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
            let result = refreshed.await;
            let _ = this.update(cx, |this, cx| match result {
                // A refresh that worked answers the error a failed one
                // left, even when nothing changed: no new snapshot arrives
                // to clear it.
                Ok(_) => this.set_error_for(&path, None, cx),
                Err(error) => this.set_error_for(&path, Some(error.message), cx),
            });
        })
        .detach();
    }

    fn set_error_for(&mut self, path: &str, message: Option<String>, cx: &mut Context<Self>) {
        if let Some(tab) = self.tab_for_path(path) {
            tab.ui.error = message;
            cx.notify();
        }
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
}

#[cfg(test)]
impl AppModel {
    /// Put a repo on the active tab the way a landed open does, minus the
    /// host session and minus following it to the forge and upstream: both
    /// reach for credentials and the network, which a test must not.
    pub fn plant_repo(&mut self, snapshot: RepoSnapshot, cx: &mut Context<Self>) {
        let tab = self.tab_mut();
        tab.path = Some(snapshot.repo_path.clone());
        tab.name = Some(snapshot.repo_name.clone());
        tab.snapshot = Some(Arc::new(snapshot));
        Self::select_working_copy(tab);
        cx.emit(AppEvent::Snapshot);
        cx.emit(AppEvent::Navigation);
        cx.notify();
    }
}

pub fn no_repo_open() -> CommandError {
    CommandError::new("no_repo_open", "No repository is currently open")
}

#[cfg(test)]
mod tests {
    use gpui::TestAppContext;

    use crate::diagnostics::{take_crash_mark, CRASH_MARK};
    use crate::store::{PersistedRepo, PersistedTabs, Store};
    use crate::testing::{install, jj_repo, scratch};

    #[gpui::test]
    fn a_session_that_ended_in_a_panic_is_news_at_the_next_launch(cx: &mut TestAppContext) {
        let dir = scratch("crash-notice");
        let alpha = jj_repo(&dir, "alpha");
        // The last session had a tab open when its panic struck.
        Store::at(dir.join("data")).set_tabs(&PersistedTabs {
            repos: vec![PersistedRepo {
                path: alpha.clone(),
                name: "alpha".into(),
            }],
            active: 0,
        });
        std::fs::write(dir.join("data").join(CRASH_MARK), "then").unwrap();

        let model = install(&dir, cx);
        cx.run_until_parked();
        model.read_with(cx, |model, _| {
            assert!(model.crashed);
            assert_eq!(model.tab().path.as_deref(), Some(alpha.as_str()));
        });
        // Said once: the launch after this one has nothing to say.
        assert!(!take_crash_mark(&dir.join("data")));
        model.update(cx, |model, cx| model.dismiss_crash_notice(cx));
        model.read_with(cx, |model, _| assert!(!model.crashed));
        let _ = std::fs::remove_dir_all(dir);
    }
}
