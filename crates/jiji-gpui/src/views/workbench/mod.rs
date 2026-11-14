//! The workbench: the graph owns the left pane; the continuous diff owns
//! the right. The graph is the pane that gives way, so the diff keeps the
//! width — up to giving way entirely (⌘⇧B) so the diff reads at full
//! width.

mod annotate;
mod diffpane;
mod dnd;
mod evolution;
mod graph;
mod header;
mod inspector;

pub use graph::VIEW_PRESETS;

use std::cell::Cell;
use std::collections::HashSet;
use std::rc::Rc;
use std::sync::Arc;

use gpui::{
    actions, div, prelude::*, px, App, Bounds, ClickEvent, Context, CursorStyle, Entity,
    FocusHandle, Focusable, KeyBinding, MouseButton, MouseMoveEvent, Pixels, ScrollHandle,
    Subscription, Window,
};
use jiji_core::snapshot::RepoSnapshot;

use crate::app::{app, AppEvent, AppModel, UiIntent, WorkbenchMode};
use crate::logic::graph::{
    build_focus_model, build_graph_model, node_row_height, resolve_focused_workstream,
    selectable_ids, FocusModel, GraphModel, GraphRow, RowDensity, ELISION_ROW_HEIGHT,
};
use crate::logic::inspect::{find_node, is_ancestor, stack_position, CompareMode};
use crate::shell::Dismiss;
use crate::store::SavedView;
use crate::theme::{theme, RAIL_W};
use crate::ui::{bounds_probe, PopoverState, TextInput};

actions!(
    workbench,
    [
        SelectNext,
        SelectPrevious,
        PageNext,
        PagePrevious,
        SelectFirst,
        SelectLast,
        /// jj's own name for the working copy: jump back to it from
        /// anywhere in history.
        GoToWorkingCopy,
        GraphMode,
        FocusMode,
        /// ⌘↵ in a tray: confirm it.
        TrayConfirm,
        /// ↵ in a tray whose list is the whole decision.
        TrayEnter,
        TrayUp,
        TrayDown,
        ScrubOlder,
        ScrubNewer,
        WidenOlder,
        WidenNewer,
    ]
);

pub fn bind_keys(cx: &mut App) {
    diffpane::bind_keys(cx);
    // Bare keys belong to the workbench only while nothing is being typed.
    let bare = Some("Workbench && !TextInput");
    // Page and Home/End travel the selection while the graph has focus;
    // anywhere else they keep scrolling the diff.
    let graph = Some("GraphPane && !TextInput");
    cx.bind_keys([
        KeyBinding::new("down", SelectNext, bare),
        KeyBinding::new("up", SelectPrevious, bare),
        KeyBinding::new("j", SelectNext, bare),
        KeyBinding::new("k", SelectPrevious, bare),
        KeyBinding::new("pagedown", PageNext, graph),
        KeyBinding::new("pageup", PagePrevious, graph),
        KeyBinding::new("home", SelectFirst, graph),
        KeyBinding::new("end", SelectLast, graph),
        KeyBinding::new("@", GoToWorkingCopy, bare),
        KeyBinding::new("g", GraphMode, bare),
        KeyBinding::new("w", FocusMode, bare),
        // The evolution pane's timeline; free keys while it is closed.
        KeyBinding::new("left", ScrubOlder, bare),
        KeyBinding::new("right", ScrubNewer, bare),
        KeyBinding::new("shift-left", WidenOlder, bare),
        KeyBinding::new("shift-right", WidenNewer, bare),
        KeyBinding::new("cmd-enter", TrayConfirm, Some("Tray")),
        KeyBinding::new("enter", TrayEnter, Some("Tray && !TextInput")),
        KeyBinding::new("up", TrayUp, Some("Tray")),
        KeyBinding::new("down", TrayDown, Some("Tray")),
    ]);
}

const GRAPH_MIN: f32 = 320.;
const GRAPH_MAX: f32 = 760.;
const GRAPH_INITIAL: f32 = 400.;
/// The diff never gets less than this, however the graph pane is dragged.
const DIFF_MIN: f32 = 320.;

/// The lens picker's panel state.
struct ViewMenu {
    popover: PopoverState,
    busy: bool,
    error: Option<String>,
    /// Naming the typed revset as a saved view.
    saving: bool,
    /// The saved view being renamed, by its current name.
    renaming: Option<String>,
    revset: Entity<TextInput>,
    name: Entity<TextInput>,
    rename: Entity<TextInput>,
    saved: Vec<SavedView>,
}

/// The comparison on the diff surface, as far as the graph needs it: what
/// is shown, and what it is measured from.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct ShownDiff {
    pub id: String,
    pub from: Option<String>,
}

pub struct WorkbenchView {
    app: Entity<AppModel>,
    focus: FocusHandle,
    graph_focus: FocusHandle,
    snapshot: Arc<RepoSnapshot>,
    model: Rc<GraphModel>,
    /// The focus lane's model and the workstream it was built for.
    focus_model: Option<(String, Rc<FocusModel>)>,
    mode: WorkbenchMode,
    density: RowDensity,
    scroll: ScrollHandle,
    graph_width: f32,
    resizing: bool,
    bounds: Rc<Cell<Bounds<Pixels>>>,
    /// The diff pane's width this frame: what the change header's facts
    /// and verbs, and the file tree, give way to.
    diff_width: f32,
    /// The selection the scroller last brought into view.
    revealed: Option<String>,
    view_menu: ViewMenu,
    /// What the diff surface currently measures (set by the diff pane).
    shown: Option<ShownDiff>,
    /// How the diff surface compares selections (parent, trunk, base, or a
    /// pinned change).
    compare: CompareMode,
    surface: Entity<crate::diff::view::DiffSurface>,
    diff: diffpane::DiffState,
    header: header::HeaderState,
    inspector: inspector::InspectorState,
    annotate: annotate::AnnotateState,
    evolution: evolution::EvolutionState,
    drag: dnd::DragState,
    /// How the rows answer the drag or plan in progress, set per frame.
    washes: dnd::Washes,
    _subscriptions: Vec<Subscription>,
}

impl Focusable for WorkbenchView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl WorkbenchView {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let app = app(cx);
        let model = app.read(cx);
        let snapshot = model
            .snapshot()
            .cloned()
            .expect("the workbench renders only with a snapshot");
        let mode = match model.store.pref::<String>("workbench.view").as_deref() {
            Some("focus") => WorkbenchMode::Focus,
            _ => WorkbenchMode::Graph,
        };
        let density = match model.store.pref::<String>("workbench.density").as_deref() {
            Some("classic") => RowDensity::Classic,
            _ => RowDensity::Compact,
        };
        let graph_width = model
            .store
            .pref::<f32>("pane.graph")
            .filter(|width| (GRAPH_MIN..=GRAPH_MAX).contains(width))
            .unwrap_or(GRAPH_INITIAL);
        let saved = model.store.saved_views(&snapshot.repo_path);
        let (layout, tree_on, tree_width) = Self::diff_prefs(model);
        let revset = cx
            .new(|cx| TextInput::new(window, cx).placeholder("e.g. mine() & description(\"wip\")"));
        let name = cx.new(|cx| TextInput::new(window, cx).placeholder("Name this view"));
        let rename = cx.new(|cx| TextInput::new(window, cx).placeholder("View name"));

        let surface = cx.new(|cx| {
            crate::diff::view::DiffSurface::new(layout == crate::app::DiffLayout::Split, cx)
        });
        let find_input = cx.new(|cx| TextInput::new(window, cx).placeholder("Find in diff"));
        let header = header::HeaderState::new(window, cx);
        let inspector = inspector::InspectorState::new(window, cx);

        let mut subscriptions = vec![
            cx.subscribe_in(&surface, window, |this, _, event, window, cx| {
                this.on_diff_event(event, window, cx)
            }),
            cx.subscribe_in(&find_input, window, |this, _, event, window, cx| {
                this.on_find_input(event, window, cx)
            }),
            cx.observe_in(&app, window, |this, _, window, cx| {
                this.sync(window, cx);
                cx.notify();
            }),
            cx.subscribe_in(&app, window, |this, _, event: &AppEvent, window, cx| {
                if *event == AppEvent::Intent {
                    this.take_intents(window, cx);
                }
            }),
            cx.subscribe_in(&revset, window, |this, _, event, window, cx| {
                if *event == crate::ui::InputEvent::Submit {
                    this.apply_revset(window, cx);
                }
                cx.notify();
            }),
            cx.subscribe_in(&name, window, |this, _, event, _, cx| {
                match event {
                    crate::ui::InputEvent::Submit => this.commit_save_view(cx),
                    crate::ui::InputEvent::Cancel => this.view_menu.saving = false,
                    _ => {}
                }
                cx.notify();
            }),
            cx.subscribe_in(&rename, window, |this, _, event, _, cx| {
                match event {
                    crate::ui::InputEvent::Submit => this.commit_rename_view(cx),
                    crate::ui::InputEvent::Cancel => this.view_menu.renaming = None,
                    _ => {}
                }
                cx.notify();
            }),
        ];
        for (which, input) in header.inputs().into_iter().enumerate() {
            subscriptions.push(cx.subscribe_in(
                input,
                window,
                move |this, _, event, window, cx| this.on_header_input(which, event, window, cx),
            ));
        }
        subscriptions.push(
            cx.subscribe_in(&inspector.rename, window, |this, _, event, _, cx| {
                this.on_inspector_input(event, cx)
            }),
        );
        let mut this = Self {
            drag: dnd::DragState::default(),
            washes: dnd::Washes::default(),
            annotate: annotate::AnnotateState::default(),
            evolution: evolution::EvolutionState::new(layout == crate::app::DiffLayout::Split, cx),
            inspector,
            surface,
            diff: diffpane::DiffState::new(
                layout,
                tree_on,
                tree_width,
                find_input,
                cx.focus_handle(),
            ),
            header,
            model: Rc::new(build_graph_model(&snapshot)),
            snapshot,
            app,
            focus: cx.focus_handle(),
            graph_focus: cx.focus_handle(),
            focus_model: None,
            mode,
            density,
            scroll: ScrollHandle::new(),
            graph_width,
            resizing: false,
            bounds: Rc::default(),
            diff_width: f32::INFINITY,
            revealed: None,
            view_menu: ViewMenu {
                popover: PopoverState::default(),
                busy: false,
                error: None,
                saving: false,
                renaming: None,
                revset,
                name,
                rename,
                saved,
            },
            shown: None,
            compare: CompareMode::Parent,
            _subscriptions: subscriptions,
        };
        this.sync(window, cx);
        this.take_intents(window, cx);
        this
    }

    /// Re-derive what follows the model: the graph for a new snapshot, the
    /// focus lane for the current workstream, the scroll position for a
    /// moved selection.
    fn sync(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let model = self.app.read(cx);
        let Some(snapshot) = model.snapshot().cloned() else {
            return;
        };
        let focused = model.ui().focused_workstream.clone();
        let selected = model.ui().selected_node.clone();
        let collapsed = model.graph_collapsed;
        if !Arc::ptr_eq(&snapshot, &self.snapshot) {
            if snapshot.repo_path != self.snapshot.repo_path {
                self.view_menu.saved = model.store.saved_views(&snapshot.repo_path);
            }
            self.model = Rc::new(build_graph_model(&snapshot));
            self.snapshot = snapshot;
            self.focus_model = None;
            // Rows moved under the selection: bring it back into view.
            self.revealed = None;
        }
        let lane = resolve_focused_workstream(&self.snapshot, focused.as_deref());
        match (self.mode, lane) {
            (WorkbenchMode::Focus, Some(lane)) => {
                if self.focus_model.as_ref().map(|(id, _)| id) != Some(&lane.id) {
                    self.focus_model = Some((
                        lane.id.clone(),
                        Rc::new(build_focus_model(&self.snapshot, lane)),
                    ));
                    self.revealed = None;
                }
            }
            _ => self.focus_model = None,
        }
        // Keep the selected row visible no matter where the selection came
        // from (keyboard, row click, a jump from the diff header). While
        // the pane is collapsed there is nothing to keep visible — but
        // expanding it re-runs the scroll for the current selection.
        if collapsed {
            self.revealed = None;
        } else if selected != self.revealed {
            if let Some(id) = &selected {
                self.reveal(id);
            }
            self.revealed = selected;
        }
        self.sync_header(window, cx);
        self.sync_diff(cx);
        self.sync_annotate(cx);
        self.sync_evolution(cx);
    }

    fn row_height(&self, row: &GraphRow) -> f32 {
        match row {
            GraphRow::Node(_) => node_row_height(self.density),
            GraphRow::Elision(_) => ELISION_ROW_HEIGHT,
        }
    }

    /// Scroll the graph the least distance that puts a change's row fully
    /// in view.
    fn reveal(&mut self, id: &str) {
        if self.mode != WorkbenchMode::Graph {
            return;
        }
        let Some((index, _)) = self.model.row_of(&self.snapshot, id) else {
            return;
        };
        let top: f32 = self.model.rows[..index]
            .iter()
            .map(|row| self.row_height(row))
            .sum();
        let bottom = top + self.row_height(&self.model.rows[index]);
        let viewport: f32 = self.scroll.bounds().size.height.into();
        if viewport <= 0. {
            // Not laid out yet: the first frame's render retries.
            self.revealed = None;
            return;
        }
        let mut offset = self.scroll.offset();
        let start: f32 = (-offset.y).into();
        let next = if top < start {
            top
        } else if bottom > start + viewport {
            bottom - viewport
        } else {
            return;
        };
        offset.y = px(-next);
        self.scroll.set_offset(offset);
    }

    /// The ids the keyboard walks, top to bottom, in the current view.
    fn walkable(&self) -> Vec<String> {
        match (&self.focus_model, self.mode) {
            (Some((_, focus)), WorkbenchMode::Focus) => {
                selectable_ids(&focus.graph, &self.snapshot)
            }
            (None, WorkbenchMode::Focus) => Vec::new(),
            _ => selectable_ids(&self.model, &self.snapshot),
        }
    }

    /// The ⇧-click range when it is still coherent, and the change the
    /// diff pane shows: the range's head (its diff runs base → head), else
    /// the single selection.
    fn subject(&self, cx: &App) -> (Option<Vec<String>>, Option<String>) {
        let ui = self.app.read(cx).ui();
        let range = ui.selected_range.clone().filter(|range| {
            ui.selected_node
                .as_ref()
                .is_some_and(|anchor| range.contains(anchor))
                && crate::logic::inspect::valid_range(&self.snapshot, range)
        });
        let node = match &range {
            Some(range) => range.first().cloned(),
            None => ui.selected_node.clone(),
        }
        .filter(|id| find_node(&self.snapshot, id).is_some());
        (range, node)
    }

    fn selected_ids(&self, cx: &App) -> HashSet<String> {
        let (range, _) = self.subject(cx);
        match range {
            Some(range) => range.into_iter().collect(),
            None => self
                .app
                .read(cx)
                .ui()
                .selected_node
                .iter()
                .cloned()
                .collect(),
        }
    }

    fn set_mode(&mut self, mode: WorkbenchMode, window: &mut Window, cx: &mut Context<Self>) {
        if self.mode == mode {
            return;
        }
        self.mode = mode;
        self.revealed = None;
        self.scroll.set_offset(gpui::Point::default());
        let name = match mode {
            WorkbenchMode::Graph => "graph",
            WorkbenchMode::Focus => "focus",
        };
        self.app
            .update(cx, |model, _| model.store.set_pref("workbench.view", &name));
        self.sync(window, cx);
        cx.notify();
    }

    fn toggle_density(&mut self, cx: &mut Context<Self>) {
        self.set_density(
            match self.density {
                RowDensity::Compact => RowDensity::Classic,
                RowDensity::Classic => RowDensity::Compact,
            },
            cx,
        );
    }

    fn set_density(&mut self, density: RowDensity, cx: &mut Context<Self>) {
        if self.density == density {
            return;
        }
        self.density = density;
        let name = match self.density {
            RowDensity::Compact => "compact",
            RowDensity::Classic => "classic",
        };
        self.app.update(cx, |model, _| {
            model.store.set_pref("workbench.density", &name)
        });
        self.revealed = None;
        cx.notify();
    }

    /// Route a pending intent to the part of the workbench that owns it:
    /// the view mode here, layout/tree/find to the diff pane, the plan and
    /// confirm panels to the change header.
    fn take_intents(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let intent = self.app.update(cx, |model, _| {
            model.take_intent(|intent| {
                !matches!(intent, UiIntent::Land { .. } | UiIntent::Ship { .. })
            })
        });
        let Some(intent) = intent else {
            return;
        };
        match intent {
            UiIntent::View(mode) => self.set_mode(mode, window, cx),
            UiIntent::Density(density) => self.set_density(density, cx),
            UiIntent::Compare(Some(mode)) => self.set_compare(mode, cx),
            UiIntent::Layout(_) | UiIntent::FileTree | UiIntent::Find => {
                self.take_diff_intent(intent, window, cx)
            }
            other => {
                // Single-change verbs act on the anchor, and their panels
                // live in the change header — which a range replaces.
                // Collapse the range so the header is there to take it.
                let ranged = self.app.read(cx).ui().selected_range.is_some();
                if ranged {
                    self.app.update(cx, |model, cx| {
                        let anchor = model.ui().selected_node.clone();
                        model.tab_mut().ui.selected_range = None;
                        model.select_node(anchor, cx);
                    });
                    self.sync(window, cx);
                }
                self.take_header_intent(other, window, cx);
            }
        }
        cx.notify();
    }

    // ── Selection ──

    /// Selecting a row also makes its workstream the current one, so the
    /// emphasis and graph move together.
    ///
    /// ⇧-click while a change in the same lane is selected extends the
    /// selection to the contiguous run between them; the diff pane renders
    /// the run as one combined diff.
    ///
    /// ⌘-click while another change is selected compares instead of
    /// selects. The diff always reads forward — when the clicked row is a
    /// descendant of the selection, the selection moves to it and the old
    /// selection becomes the base, so picking the two ends in either order
    /// shows old → new. ⌘-clicking the active compare base again returns
    /// to the parent diff.
    fn select(&mut self, id: String, modifiers: gpui::Modifiers, cx: &mut Context<Self>) {
        let selected = self.app.read(cx).ui().selected_node.clone();
        let command = modifiers.platform || modifiers.control;
        if modifiers.shift && !command && selected.is_some() {
            self.app
                .update(cx, |model, cx| model.extend_selection(id, cx));
            return;
        }
        if let Some(selected) = selected.filter(|selected| command && *selected != id) {
            let base = self.shown.as_ref().and_then(|shown| shown.from.clone());
            if base.as_deref() == Some(id.as_str()) {
                self.set_compare(CompareMode::Parent, cx);
            } else if is_ancestor(&self.snapshot, &selected, &id) {
                self.apply_selection(id, cx);
                self.set_compare(CompareMode::Change(selected), cx);
            } else {
                self.set_compare(CompareMode::Change(id), cx);
            }
            return;
        }
        self.apply_selection(id, cx);
    }

    fn apply_selection(&mut self, id: String, cx: &mut Context<Self>) {
        let stream = stack_position(&self.snapshot, &id).map(|p| p.workstream.id.clone());
        self.app.update(cx, |model, cx| {
            if let Some(stream) = stream {
                model.focus_workstream(Some(stream), cx);
            }
            model.select_node(Some(id), cx);
        });
    }

    fn focus_workstream(&mut self, id: String, cx: &mut Context<Self>) {
        self.app.update(cx, |model, cx| {
            model.focus_workstream(Some(id), cx);
            model.select_node(None, cx);
        });
    }

    /// Move the selection to the row `to` picks from the current index
    /// (`None` with nothing selected), clamped to the rows on screen.
    fn travel(&mut self, to: impl FnOnce(Option<usize>, usize) -> isize, cx: &mut Context<Self>) {
        let ids = self.walkable();
        if ids.is_empty() {
            return;
        }
        let selected = self.app.read(cx).ui().selected_node.clone();
        let index = selected.and_then(|id| ids.iter().position(|candidate| *candidate == id));
        let target = to(index, ids.len()).clamp(0, ids.len() as isize - 1) as usize;
        self.apply_selection(ids[target].clone(), cx);
    }

    /// `delta` rows on from the selection; with nothing selected, stepping
    /// down starts at the top and stepping up at the bottom.
    fn step(&mut self, delta: isize, cx: &mut Context<Self>) {
        self.travel(
            |index, len| match index {
                Some(index) => index as isize + delta,
                None if delta > 0 => 0,
                None => len as isize - 1,
            },
            cx,
        );
    }

    fn page_rows(&self) -> isize {
        let viewport: f32 = self.scroll.bounds().size.height.into();
        ((viewport / node_row_height(self.density)).floor() as isize - 1).max(1)
    }

    /// Esc, staged: a pane over the diff (annotate or evolution) closes
    /// back to it first, then a ⇧-click range collapses to its anchor,
    /// then a comparison on screen leaves; the next Esc clears the
    /// selection.
    fn dismiss(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.view_menu.popover.is_open() {
            self.view_menu.popover.close();
            cx.notify();
            return;
        }
        if self.cancel_drag(cx)
            || self.dismiss_inspector(cx)
            || self.dismiss_header(window, cx)
            || self.dismiss_find(window, cx)
        {
            return;
        }
        let comparing = self.compare != CompareMode::Parent;
        let handled = self.app.update(cx, |model, cx| {
            let ui = model.ui();
            if ui.annotate.is_some() {
                model.close_annotate(cx);
            } else if ui.evolution.is_some() {
                model.close_evolution(cx);
            } else if ui.selected_range.is_some() {
                let anchor = ui.selected_node.clone();
                model.tab_mut().ui.selected_range = None;
                model.select_node(anchor, cx);
                cx.notify();
            } else if comparing {
                return false;
            } else if ui.selected_node.is_some() {
                model.select_node(None, cx);
            } else {
                return false;
            }
            true
        });
        if handled {
            return;
        }
        if comparing {
            self.set_compare(CompareMode::Parent, cx);
        } else {
            cx.propagate();
        }
    }

    // ── The split ──

    fn shown_width(&self) -> f32 {
        let available: f32 = self.bounds.get().size.width.into();
        let limit = if available > 0. {
            GRAPH_MAX.min((available - DIFF_MIN).max(0.))
        } else {
            GRAPH_MAX
        };
        self.graph_width.clamp(GRAPH_MIN.min(limit), limit)
    }

    fn drag_divider(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        if !self.resizing {
            return;
        }
        if event.pressed_button != Some(MouseButton::Left) {
            self.end_resize(cx);
            return;
        }
        let left: f32 = self.bounds.get().left().into();
        let x: f32 = event.position.x.into();
        self.graph_width = x - left;
        self.graph_width = self.shown_width();
        cx.notify();
    }

    fn end_resize(&mut self, cx: &mut Context<Self>) {
        if !self.resizing {
            return;
        }
        self.resizing = false;
        let width = self.graph_width.round();
        self.app
            .update(cx, |model, _| model.store.set_pref("pane.graph", &width));
        cx.notify();
    }

    fn divider_click(&mut self, event: &ClickEvent, _: &mut Window, cx: &mut Context<Self>) {
        if event.click_count() == 2 {
            self.graph_width = GRAPH_INITIAL;
            self.resizing = true;
            self.end_resize(cx);
        }
    }

    /// Right-click on the diff: what the menu offers for what is under the
    /// pointer.
    fn open_diff_menu(
        &mut self,
        position: gpui::Point<Pixels>,
        target: crate::diff::view::DiffTarget,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let loaded = self.diff.loaded.clone();
        let file = loaded.as_ref().and_then(|(_, diff)| {
            let path = target.path.as_deref()?;
            diff.files.iter().find(|file| file.path == path)
        });
        let (Some((key, _)), Some(file)) = (&loaded, file) else {
            self.app
                .update(cx, |model, cx| model.open_surface_menu(position, cx));
            return;
        };
        let line = (!target.on_header && (target.old_no.is_some() || target.new_no.is_some()))
            .then_some((target.old_no, target.new_no));
        let selection = target.selection.unwrap_or_default();
        // A comparison's hunks are not the change's own: reading verbs
        // only.
        let readonly = key.from.is_some();
        self.app.update(cx, |model, cx| {
            model.open_diff_menu(file, &key.id, readonly, line, &selection, position, cx)
        });
    }
}

impl Render for WorkbenchView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx);
        // The first frame has no viewport to scroll within; reveal again
        // once one exists.
        if self.revealed.is_none() {
            self.sync(window, cx);
        }
        let model = self.app.read(cx);
        let collapsed = model.graph_collapsed;
        let rail = if model.sidebar_collapsed { 0. } else { RAIL_W };
        let width = self.shown_width();
        let resizing = self.resizing;
        // Measured from the window, not from last frame's layout, so the
        // header's breakpoints never trail a resize.
        let workbench = f32::from(window.viewport_size().width) - rail;
        self.diff_width = if collapsed {
            workbench
        } else {
            workbench - width - 1.
        };
        // Rows record where they paint; last frame's places are stale.
        self.drag_frame(window, cx);
        self.drag.rows.borrow_mut().clear();
        self.washes = self.washes();

        let mut root =
            div()
                .id("workbench")
                .key_context("Workbench")
                .track_focus(&self.focus)
                .relative()
                .size_full()
                .flex()
                .flex_row()
                .on_action(cx.listener(|this, _: &SelectNext, _, cx| this.step(1, cx)))
                .on_action(cx.listener(|this, _: &SelectPrevious, _, cx| this.step(-1, cx)))
                .on_action(cx.listener(|this, _: &PageNext, _, cx| {
                    let rows = this.page_rows();
                    this.step(rows, cx);
                }))
                .on_action(cx.listener(|this, _: &PagePrevious, _, cx| {
                    let rows = this.page_rows();
                    this.step(-rows, cx);
                }))
                .on_action(cx.listener(|this, _: &SelectFirst, _, cx| this.travel(|_, _| 0, cx)))
                .on_action(cx.listener(|this, _: &SelectLast, _, cx| {
                    this.travel(|_, len| len as isize - 1, cx)
                }))
                .on_action(cx.listener(|this, _: &GoToWorkingCopy, _, cx| {
                    let id = this.snapshot.working_copy.clone();
                    this.app
                        .update(cx, |model, cx| model.jump_to_change(&id, cx));
                }))
                .on_action(cx.listener(|this, _: &GraphMode, window, cx| {
                    this.set_mode(WorkbenchMode::Graph, window, cx)
                }))
                .on_action(cx.listener(|this, _: &FocusMode, window, cx| {
                    this.set_mode(WorkbenchMode::Focus, window, cx)
                }))
                .on_action(cx.listener(|this, _: &Dismiss, window, cx| this.dismiss(window, cx)))
                .on_action(cx.listener(|this, _: &ScrubOlder, _, cx| {
                    if !this.scrub_evolution(-1, false, cx) {
                        cx.propagate();
                    }
                }))
                .on_action(cx.listener(|this, _: &ScrubNewer, _, cx| {
                    if !this.scrub_evolution(1, false, cx) {
                        cx.propagate();
                    }
                }))
                .on_action(cx.listener(|this, _: &WidenOlder, _, cx| {
                    if !this.scrub_evolution(-1, true, cx) {
                        cx.propagate();
                    }
                }))
                .on_action(cx.listener(|this, _: &WidenNewer, _, cx| {
                    if !this.scrub_evolution(1, true, cx) {
                        cx.propagate();
                    }
                }))
                .on_mouse_move(cx.listener(Self::drag_divider))
                .on_mouse_move(cx.listener(Self::drag_row_move))
                .on_mouse_up(
                    MouseButton::Left,
                    cx.listener(|this, _, _, cx| {
                        this.end_resize(cx);
                        this.drop_row(cx);
                    }),
                )
                .on_modifiers_changed(cx.listener(
                    |this, event: &gpui::ModifiersChangedEvent, _, cx| {
                        this.drag_modifiers(event.modifiers.alt, cx)
                    },
                ))
                .children(self.render_drag_card(window, cx))
                .child(bounds_probe(self.bounds.clone()));
        if resizing {
            root = root.cursor(CursorStyle::ResizeLeftRight);
        } else if self.drag.active {
            root = root.cursor(CursorStyle::ClosedHand);
        }
        if !collapsed {
            root = root
                .child(
                    div()
                        .w(px(width))
                        .flex_shrink_0()
                        .h_full()
                        .child(self.render_graph_pane(width, window, cx)),
                )
                .child(
                    // The divider: a hairline with a wider grip around it.
                    div()
                        .id("graph-divider")
                        .relative()
                        .w(px(1.))
                        .flex_shrink_0()
                        .h_full()
                        .bg(if resizing { t.accent } else { t.border2 })
                        .child(
                            div()
                                .id("graph-divider-grip")
                                .absolute()
                                .top_0()
                                .bottom_0()
                                .left(px(-3.))
                                .w(px(7.))
                                .cursor(CursorStyle::ResizeLeftRight)
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(|this, _, _, cx| {
                                        this.resizing = true;
                                        cx.notify();
                                    }),
                                )
                                .on_click(cx.listener(Self::divider_click)),
                        ),
                );
        }
        root.child(
            div()
                .flex_1()
                .min_w_0()
                .h_full()
                .child(self.render_diff_pane(window, cx)),
        )
    }
}
