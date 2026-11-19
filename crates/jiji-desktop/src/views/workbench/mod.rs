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
use crate::theme::RAIL_W;
use crate::ui::splitter::{self, moved, splitter, Narrower, Narrowest, Wider, Widest, STEP};
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
    annotate::bind_keys(cx);
    diffpane::bind_keys(cx);
    splitter::bind_keys(cx);
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
    divider_focus: FocusHandle,
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
        // Switching section or tab drops this view (the shell rebuilds its
        // content on navigation), and an editor open in it goes too: its
        // text is kept as the change's draft, as it is on every other way
        // out of the editor.
        subscriptions.push(cx.on_release(|this, cx| this.keep_open_draft(cx)));
        let evolution = evolution::EvolutionState::new(layout == crate::app::DiffLayout::Split, cx);
        subscriptions.push(
            cx.subscribe_in(&evolution.surface, window, |this, _, event, _, cx| {
                this.on_evolution_event(event, cx)
            }),
        );
        let mut this = Self {
            drag: dnd::DragState::default(),
            washes: dnd::Washes::default(),
            annotate: annotate::AnnotateState::new(cx),
            evolution,
            inspector,
            surface,
            diff: diffpane::DiffState::new(
                layout,
                tree_on,
                tree_width,
                find_input,
                cx.focus_handle().tab_stop(true),
                cx.focus_handle().tab_stop(true),
            ),
            header,
            model: Rc::new(build_graph_model(&snapshot)),
            snapshot,
            app,
            focus: cx.focus_handle(),
            // The graph is one stop: its arrows travel the selection.
            graph_focus: cx.focus_handle().tab_stop(true),
            divider_focus: cx.focus_handle().tab_stop(true),
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
                popover: PopoverState::new(cx),
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
            self.view_menu.popover.close(window, cx);
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

    /// How narrow and how wide the graph pane can be in this window: the
    /// diff keeps its minimum, and the graph's own gives way to it.
    fn graph_limits(&self) -> (f32, f32) {
        let available: f32 = self.bounds.get().size.width.into();
        let limit = if available > 0. {
            GRAPH_MAX.min((available - DIFF_MIN).max(0.))
        } else {
            GRAPH_MAX
        };
        (GRAPH_MIN.min(limit), limit)
    }

    fn shown_width(&self) -> f32 {
        let (min, max) = self.graph_limits();
        self.graph_width.clamp(min, max)
    }

    /// The divider moved from the keyboard, from where it stands on screen;
    /// the width is kept at once, as a drag's is when it ends.
    fn nudge_graph(&mut self, by: f32, cx: &mut Context<Self>) {
        let (min, max) = self.graph_limits();
        self.graph_width = moved(self.shown_width(), min, max, by);
        let width = self.graph_width.round();
        self.app
            .update(cx, |model, _| model.store.set_pref("pane.graph", &width));
        cx.notify();
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
        let selection = target.selection.clone().unwrap_or_default();
        // Only a diff read at the repository, operation and comparison on
        // screen names a file to act on. While another selection loads, or
        // after a refresh failed, the dimmed diff is an older one, and its
        // paths, hunks and conflicts would be taken for the current
        // change's: the menu is the bare surface's until they match.
        let loaded = self.diff.loaded.clone().filter(|_| self.diff.fresh());
        let file = loaded.as_ref().and_then(|(_, diff)| {
            let path = target.path.as_deref()?;
            diff.files.iter().find(|file| file.path == path)
        });
        let (Some((key, _)), Some(file)) = (&loaded, file) else {
            self.app.update(cx, |model, cx| {
                model.open_surface_menu(position, &selection, cx)
            });
            return;
        };
        let line = target.line();
        // A comparison's hunks are not the change's own, and a file's
        // discard would undo the change's edits rather than the span on
        // screen: reading verbs only. Acting happens on the change's own
        // diff, one Esc away.
        let readonly = key.from.is_some();
        self.app.update(cx, |model, cx| {
            model.open_diff_menu(file, &key.id, readonly, line, &selection, position, cx)
        });
    }
}

impl Render for WorkbenchView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
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
                .child({
                    let (min, max) = self.graph_limits();
                    let press = cx.listener(|this: &mut Self, _: &gpui::MouseDownEvent, _, cx| {
                        this.resizing = true;
                        cx.notify();
                    });
                    let click = cx.listener(Self::divider_click);
                    splitter(
                        "graph-divider",
                        "Resize graph panel",
                        &self.divider_focus,
                        width,
                        min,
                        max,
                        resizing,
                        window,
                        cx,
                        |grip| grip.on_mouse_down(MouseButton::Left, press).on_click(click),
                    )
                    .on_action(cx.listener(|this, _: &Narrower, _, cx| this.nudge_graph(-STEP, cx)))
                    .on_action(cx.listener(|this, _: &Wider, _, cx| this.nudge_graph(STEP, cx)))
                    .on_action(cx.listener(|this, _: &Narrowest, _, cx| {
                        this.nudge_graph(f32::NEG_INFINITY, cx)
                    }))
                    .on_action(
                        cx.listener(|this, _: &Widest, _, cx| this.nudge_graph(f32::INFINITY, cx)),
                    )
                });
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

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::Arc;

    use gpui::{point, px, TestAppContext, WindowHandle};

    use crate::app::AppModel;
    use crate::diff::view::{DiffEvent, DiffTarget};
    use crate::logic::inspect::CompareMode;
    use crate::shell::Shell;

    /// The working copy's two-hunk file in the mock repo, and its second
    /// changed line: a target for every file, hunk and line action.
    const VIEWPORT: &str = "src/lib/components/diff/DiffViewport.svelte";
    /// A file the conflicted mock change `pmwzqkvt` leaves unresolved.
    const CONFLICTED: &str = "src/lib/components/conflicts/ConflictList.svelte";

    /// What may change the repository from a diff's menu.
    const CHANGING: [&str; 5] = [
        "diff.moveFile",
        "diff.restoreFile",
        "diff.restoreHunk",
        "diff.restoreLine",
        "diff.resolve",
    ];

    fn on_line(path: &str, old: u32) -> DiffTarget {
        DiffTarget {
            path: Some(path.into()),
            old_no: Some(old),
            selection: Some("rows.slice".into()),
            ..DiffTarget::default()
        }
    }

    fn on_header(path: &str) -> DiffTarget {
        DiffTarget {
            path: Some(path.into()),
            on_header: true,
            ..DiffTarget::default()
        }
    }

    fn workbench(
        window: WindowHandle<Shell>,
        cx: &mut TestAppContext,
    ) -> gpui::Entity<super::WorkbenchView> {
        window
            .update(cx, |shell, _, _| shell.section::<super::WorkbenchView>())
            .unwrap()
            .expect("the workbench is on screen")
    }

    /// Right-click the diff surface over `target`, as its hit-testing
    /// reports it, and read back what the menu offers.
    fn menu_for(
        model: &gpui::Entity<AppModel>,
        window: WindowHandle<Shell>,
        target: DiffTarget,
        cx: &mut TestAppContext,
    ) -> Vec<&'static str> {
        model.update(cx, |model, cx| model.close_menu(cx));
        let surface = workbench(window, cx).read_with(cx, |view, _| view.surface.clone());
        surface.update(cx, |_, cx| {
            cx.emit(DiffEvent::Context {
                position: point(px(900.), px(300.)),
                target,
            })
        });
        model.read_with(cx, |model, _| {
            model
                .menu
                .iter()
                .flat_map(|menu| menu.sections.iter().flatten())
                .map(|item| item.id)
                .collect()
        })
    }

    fn changing(ids: &[&'static str]) -> Vec<&'static str> {
        ids.iter()
            .copied()
            .filter(|id| CHANGING.contains(id))
            .collect()
    }

    /// The mock repo planted in a window, settled with the working copy's
    /// diff on screen.
    fn mock_workbench(
        name: &str,
        cx: &mut TestAppContext,
    ) -> (
        std::path::PathBuf,
        gpui::Entity<AppModel>,
        WindowHandle<Shell>,
    ) {
        let dir = crate::testing::scratch(name);
        let (model, repo) = crate::testing::install_mock(&dir, cx);
        let snapshot = jiji_core::mock::mock_snapshot(Path::new(&repo));
        model.update(cx, |model, cx| model.plant_repo(snapshot, cx));
        let window = cx.add_window(Shell::new);
        cx.run_until_parked();
        (dir, model, window)
    }

    /// `lwnouzpy`'s evolution open on its middle rewrite (`9e12ff30` →
    /// `b7f2c9d1`), whose diff is not the change's own: the step reworked
    /// `VirtualDiffList.svelte` and added `virtual.test.ts`.
    fn evolution_step(
        model: &gpui::Entity<AppModel>,
        window: WindowHandle<Shell>,
        cx: &mut TestAppContext,
    ) -> gpui::Entity<crate::diff::view::DiffSurface> {
        model.update(cx, |model, cx| {
            model.select_node(Some("lwnouzpy".into()), cx);
            model.open_evolution("lwnouzpy".into(), cx);
        });
        cx.run_until_parked();
        let view = workbench(window, cx);
        view.update(cx, |view, cx| assert!(view.scrub_evolution(-1, false, cx)));
        cx.run_until_parked();
        view.read_with(cx, |view, cx| {
            let surface = view.evolution.surface.clone();
            let paths: Vec<String> = surface
                .read(cx)
                .files()
                .iter()
                .map(|file| file.path.to_string())
                .collect();
            assert!(
                paths.iter().any(|path| path.ends_with("virtual.test.ts")),
                "{paths:?}"
            );
            surface
        })
    }

    /// Choose an Edit menu item the way the menu bar does: it is offered
    /// when the focused view answers it, and choosing it dispatches it
    /// there. False when the item would be greyed out.
    fn choose(action: &dyn gpui::Action, cx: &mut TestAppContext) -> bool {
        cx.update(|cx| {
            let offered = cx.is_action_available(action);
            if offered {
                cx.dispatch_action(action);
            }
            offered
        })
    }

    /// Give `surface` the keys in the key window, the one the menu bar
    /// acts on.
    fn focus(
        window: WindowHandle<Shell>,
        surface: &gpui::Entity<crate::diff::view::DiffSurface>,
        cx: &mut TestAppContext,
    ) {
        window
            .update(cx, |_, window, cx| {
                window.activate_window();
                window.focus(&gpui::Focusable::focus_handle(surface.read(cx), cx), cx)
            })
            .unwrap();
        cx.run_until_parked();
    }

    /// Edit ▸ Select All, then Edit ▸ Copy, on a focused diff that shows
    /// `line`.
    fn select_all_and_copy(
        window: WindowHandle<Shell>,
        surface: &gpui::Entity<crate::diff::view::DiffSurface>,
        line: &str,
        cx: &mut TestAppContext,
    ) {
        use crate::ui::input::{Copy, SelectAll};
        focus(window, surface, cx);
        // Nothing is selected yet: Copy is greyed out.
        assert!(!choose(&Copy, cx));
        assert!(choose(&SelectAll, cx));
        let all = surface
            .read_with(cx, |surface, _| surface.selected_text())
            .expect("Select All selects the diff");
        assert!(all.contains(line), "{all}");
        assert!(choose(&Copy, cx));
        let copied = cx.read_from_clipboard().and_then(|item| item.text());
        assert_eq!(copied.as_deref(), Some(all.as_str()));
    }

    /// The menu on screen, item for item: what each says and does, what it
    /// acts on, and whether it is on offer.
    fn sections(
        model: &gpui::Entity<AppModel>,
        cx: &mut TestAppContext,
    ) -> Vec<crate::logic::menu::MenuSection> {
        model.read_with(cx, |model, _| {
            model
                .menu
                .as_ref()
                .map(|menu| menu.sections.clone())
                .unwrap_or_default()
        })
    }

    /// Right-click a file's row in the tree, and read back the menu.
    fn tree_menu(
        model: &gpui::Entity<AppModel>,
        window: WindowHandle<Shell>,
        path: &str,
        cx: &mut TestAppContext,
    ) -> Vec<crate::logic::menu::MenuSection> {
        model.update(cx, |model, cx| model.close_menu(cx));
        let view = workbench(window, cx);
        let index = view
            .read_with(cx, |view, cx| {
                let files = view.surface.read(cx).files();
                files.iter().position(|file| file.path.as_ref() == path)
            })
            .unwrap_or_else(|| panic!("{path} is in the tree"));
        window
            .update(cx, |_, window, cx| {
                view.update(cx, |view, cx| {
                    view.open_tree_menu(index, point(px(120.), px(300.)), window, cx)
                })
            })
            .unwrap();
        sections(model, cx)
    }

    #[gpui::test]
    fn a_file_in_the_tree_offers_what_its_diff_header_offers(cx: &mut TestAppContext) {
        let (dir, model, window) = mock_workbench("tree-menu", cx);
        let working = model.read_with(cx, |model, _| {
            model.snapshot().unwrap().working_copy.clone()
        });
        for (change, path) in [
            // Modified, in the working copy.
            (working.as_str(), VIEWPORT),
            // Removed.
            ("mvkortsq", "src/lib/components/diff/rowHeightCache.ts"),
            // Conflicted.
            ("pmwzqkvt", CONFLICTED),
            // In an immutable change.
            ("uvkmrtpz", "CHANGELOG.md"),
        ] {
            model.update(cx, |model, cx| model.select_node(Some(change.into()), cx));
            cx.run_until_parked();
            menu_for(&model, window, on_header(path), cx);
            let header = sections(&model, cx);
            let tree = tree_menu(&model, window, path, cx);
            assert!(
                tree.iter().flatten().any(|item| item.id == "diff.copyPath"),
                "{path}"
            );
            assert_eq!(tree, header, "{path}");
        }

        // While another change's diff loads (one not read before), the
        // tree's rows are the old diff's, and offer what its dimmed header
        // does: nothing to change.
        model.update(cx, |model, cx| {
            model.select_node(Some("toznkvpq".into()), cx)
        });
        let loading = tree_menu(&model, window, "CHANGELOG.md", cx);
        assert!(
            loading
                .iter()
                .flatten()
                .all(|item| !item.id.starts_with("diff.")),
            "{loading:?}"
        );
        menu_for(&model, window, on_header("CHANGELOG.md"), cx);
        assert_eq!(loading, sections(&model, cx));
        cx.run_until_parked();
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Whether the tree row for `key` lies wholly inside the tree's
    /// viewport on the last frame.
    fn tree_row_in_view(
        view: &gpui::Entity<super::WorkbenchView>,
        key: &str,
        cx: &mut TestAppContext,
    ) -> bool {
        view.read_with(cx, |view, _| {
            let rows = crate::logic::tree::visible_tree_rows(
                &view.diff.tree_rows,
                &view.diff.tree_collapsed,
            );
            let index = rows
                .iter()
                .position(|row| row.path() == key)
                .unwrap_or_else(|| panic!("{key} is a visible row"));
            let scroll = &view.diff.tree_scroll;
            let (row, viewport) = (scroll.bounds_for_item(index).unwrap(), scroll.bounds());
            let top = row.top() + scroll.offset().y;
            top >= viewport.top() && top + row.size.height <= viewport.bottom()
        })
    }

    #[gpui::test]
    fn the_tree_keeps_the_file_being_read_and_the_keyboard_in_view(cx: &mut TestAppContext) {
        let dir = crate::testing::scratch("tree-in-view");
        let model = crate::testing::install(&dir, cx);
        let repo = crate::testing::jj_repo(&dir, "wide");
        // Sixty new files in two folders: a tree taller than its pane.
        for folder in ["alpha", "omega"] {
            std::fs::create_dir_all(Path::new(&repo).join(folder)).unwrap();
            for n in 0..30 {
                let file = Path::new(&repo)
                    .join(folder)
                    .join(format!("file{n:02}.txt"));
                std::fs::write(file, "a line\n").unwrap();
            }
        }
        let window = cx.add_window(Shell::new);
        model.update(cx, |model, cx| model.open_repo_at(repo.clone(), cx));
        cx.run_until_parked();
        let view = workbench(window, cx);
        let surface = view.read_with(cx, |view, _| view.surface.clone());
        assert!(!tree_row_in_view(&view, "omega/file29.txt", cx));

        // The file being read: the diff scrolled to its end.
        let read = |cx: &mut TestAppContext| {
            view.read_with(cx, |view, cx| {
                let path = view.surface.read(cx).files()[view.diff.active_file]
                    .path
                    .clone();
                let rows = &view.diff.tree_rows;
                crate::logic::tree::active_row_key(rows, &view.diff.tree_collapsed, &path)
                    .to_owned()
            })
        };
        let to_end = |cx: &mut TestAppContext| {
            surface.update(cx, |surface, cx| surface.scroll_to(0., cx));
            cx.run_until_parked();
            surface.update(cx, |surface, cx| surface.scroll_to(f32::MAX, cx));
            cx.run_until_parked();
        };
        let keys = |cx: &mut TestAppContext| {
            window
                .update(cx, |_, window, cx| {
                    let focus = view.read(cx).diff.tree_focus.clone();
                    window.focus(&focus, cx);
                })
                .unwrap();
            cx.simulate_keystrokes(window.into(), "end");
            let last = view.read_with(cx, |view, _| view.diff.tree_cursor.clone().unwrap());
            assert!(tree_row_in_view(&view, &last, cx), "{last} after End");
            cx.simulate_keystrokes(window.into(), "home");
            assert!(tree_row_in_view(&view, "alpha", cx), "alpha after Home");
        };

        to_end(cx);
        let reading = read(cx);
        assert!(reading.starts_with("omega/"), "{reading}");
        assert!(tree_row_in_view(&view, &reading, cx), "{reading}");
        keys(cx);

        // Folded, the directory stands in for the file being read, and is
        // the row brought into view.
        view.update(cx, |view, cx| {
            view.diff.tree_collapsed.insert("omega".into());
            cx.notify();
        });
        to_end(cx);
        assert_eq!(read(cx), "omega");
        assert!(tree_row_in_view(&view, "omega", cx));
        keys(cx);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[gpui::test]
    fn a_diff_seen_before_is_back_on_screen_in_the_same_frame(cx: &mut TestAppContext) {
        let (dir, model, window) = mock_workbench("warm-diff", cx);
        let paths = |cx: &mut TestAppContext| {
            workbench(window, cx).read_with(cx, |view, cx| {
                let files = view.surface.read(cx).files();
                files
                    .iter()
                    .map(|file| file.path.to_string())
                    .collect::<Vec<_>>()
            })
        };
        model.update(cx, |model, cx| {
            model.select_node(Some("mvkortsq".into()), cx)
        });
        cx.run_until_parked();
        let measured = paths(cx);
        model.update(cx, |model, cx| {
            model.select_node(Some("pmwzqkvt".into()), cx)
        });
        cx.run_until_parked();
        assert_ne!(paths(cx), measured);

        // Nothing runs between the click and the next frame: the cached
        // diff is on the surface already, fresh, and nothing dims.
        model.update(cx, |model, cx| {
            model.select_node(Some("mvkortsq".into()), cx)
        });
        assert_eq!(paths(cx), measured);
        workbench(window, cx).read_with(cx, |view, _| assert!(view.diff.fresh()));
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Right-click the evolution's diff over `target`, and read back the
    /// menu.
    fn evolution_menu(
        model: &gpui::Entity<AppModel>,
        surface: &gpui::Entity<crate::diff::view::DiffSurface>,
        target: DiffTarget,
        cx: &mut TestAppContext,
    ) -> Vec<crate::logic::menu::MenuSection> {
        model.update(cx, |model, cx| model.close_menu(cx));
        surface.update(cx, |_, cx| {
            cx.emit(DiffEvent::Context {
                position: point(px(900.), px(300.)),
                target,
            })
        });
        sections(model, cx)
    }

    /// Choose a menu item by its id, as a click on it would, and read the
    /// clipboard it filled.
    fn copied(
        model: &gpui::Entity<AppModel>,
        menu: &[crate::logic::menu::MenuSection],
        id: &str,
        cx: &mut TestAppContext,
    ) -> String {
        let item = menu
            .iter()
            .flatten()
            .find(|item| item.id == id)
            .unwrap_or_else(|| panic!("{id} is on offer"));
        let action = item.action.clone().expect("a copy acts");
        model.update(cx, |model, cx| model.run_action(action, cx));
        cx.read_from_clipboard()
            .and_then(|item| item.text())
            .expect("copied text")
    }

    #[gpui::test]
    fn the_evolution_diff_offers_its_version_to_read_and_nothing_to_change(
        cx: &mut TestAppContext,
    ) {
        const LIST: &str = "src/lib/components/diff/VirtualDiffList.svelte";
        let (dir, model, window) = mock_workbench("evolution-menu", cx);
        let surface = evolution_step(&model, window, cx);
        // The step's first added line, in the file it reworked.
        let on_added = DiffTarget {
            path: Some(LIST.into()),
            new_no: Some(3),
            ..DiffTarget::default()
        };
        let menu = evolution_menu(&model, &surface, on_added.clone(), cx);
        let ids: Vec<&str> = menu.iter().flatten().map(|item| item.id).collect();
        assert!(changing(&ids).is_empty(), "{ids:?}");

        let step = "--- a/src/lib/components/diff/VirtualDiffList.svelte\n\
                    +++ b/src/lib/components/diff/VirtualDiffList.svelte\n\
                    @@ -2,2 +2,3 @@\n   let { files, viewport } = $props();\n\
                    -  const rows = $derived(files.flatMap(fileRows));\n\
                    +  const rows = $derived(files.flatMap(fileRows).slice(range.first, range.last + 1));\n\
                    +  const range = $derived(visibleRange(viewport, rows.length));\n";
        assert_eq!(copied(&model, &menu, "diff.copyHunk", cx), step);
        // The file as that version changed it, not as the change adds it
        // today (whole, from /dev/null).
        assert_eq!(copied(&model, &menu, "diff.copyFilePatch", cx), step);
        assert_eq!(copied(&model, &menu, "diff.copyPath", cx), LIST);

        // Widened, the span's diff has yet to arrive: the surface still
        // holds the step's, and names no file until it does.
        let view = workbench(window, cx);
        view.update(cx, |view, cx| assert!(view.scrub_evolution(-1, true, cx)));
        let loading = evolution_menu(&model, &surface, on_added.clone(), cx);
        assert!(
            loading
                .iter()
                .flatten()
                .all(|item| !item.id.starts_with("diff.")),
            "{loading:?}"
        );
        cx.run_until_parked();
        let widened = evolution_menu(&model, &surface, on_added, cx);
        assert!(widened
            .iter()
            .flatten()
            .any(|item| item.id == "diff.copyFilePatch"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[gpui::test]
    fn annotate_reaches_its_last_column_by_a_dragged_thumb(cx: &mut TestAppContext) {
        use gpui::{size, Modifiers, MouseButton, VisualTestContext};
        let dir = crate::testing::scratch("annotate-pan");
        let (model, repo) = crate::testing::install_mock(&dir, cx);
        let snapshot = jiji_core::mock::mock_snapshot(Path::new(&repo));
        let working = snapshot.working_copy.clone();
        model.update(cx, |model, cx| model.plant_repo(snapshot, cx));
        // A pane narrow enough that the file's lines outrun its code column.
        let window = cx.open_window(size(px(900.), px(700.)), Shell::new);
        model.update(cx, |model, cx| {
            model.open_annotate(working, VIEWPORT.into(), cx)
        });
        cx.run_until_parked();
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        let track = cx
            .debug_bounds("annotate-pan")
            .expect("the thumb is there when the lines outrun the pane");

        // Grab the thumb at rest, at the track's left end, and drag it past
        // the right end, the way a mouse would.
        let at = point(track.left() + px(6.), track.center().y);
        let none = Modifiers::none();
        cx.simulate_mouse_move(at, None, none);
        cx.simulate_mouse_down(at, MouseButton::Left, none);
        cx.simulate_mouse_move(
            point(track.right() + px(60.), at.y),
            MouseButton::Left,
            none,
        );
        cx.simulate_mouse_up(
            point(track.right() + px(60.), at.y),
            MouseButton::Left,
            none,
        );
        let (pan, max) = workbench(window, &mut cx)
            .read_with(&cx, |view, _| (view.annotate.pan, view.annotate.max_pan));
        assert!(max > 0. && pan == max, "panned {pan} of {max}");

        // The origins do not pan: the first line's still takes the graph to
        // the change it came from.
        let lines = cx.debug_bounds("annotate-lines").unwrap();
        cx.simulate_click(point(lines.left() + px(40.), lines.top() + px(9.)), none);
        model.read_with(&cx, |model, _| {
            assert_eq!(model.ui().selected_node.as_deref(), Some("nzpwlxvr"));
        });
        let _ = std::fs::remove_dir_all(dir);
    }

    #[gpui::test]
    fn edit_copy_and_select_all_reach_a_focused_diff(cx: &mut TestAppContext) {
        let (dir, model, window) = mock_workbench("edit-menu-diff", cx);
        let main = workbench(window, cx).read_with(cx, |view, _| view.surface.clone());
        select_all_and_copy(window, &main, "rows.slice", cx);

        let evolution = evolution_step(&model, window, cx);
        select_all_and_copy(
            window,
            &evolution,
            "visibleRange(viewport, rows.length)",
            cx,
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[gpui::test]
    fn a_diff_that_is_not_the_one_asked_for_offers_nothing_to_change(cx: &mut TestAppContext) {
        let dir = crate::testing::scratch("stale-diff-menu");
        let (model, repo) = crate::testing::install_mock(&dir, cx);
        let snapshot = jiji_core::mock::mock_snapshot(Path::new(&repo));
        model.update(cx, |model, cx| model.plant_repo(snapshot, cx));
        let window = cx.add_window(Shell::new);
        cx.run_until_parked();

        // The working copy's own diff, read at the operation on screen.
        let fresh = menu_for(&model, window, on_line(VIEWPORT, 39), cx);
        assert_eq!(
            changing(&fresh),
            [
                "diff.moveFile",
                "diff.restoreLine",
                "diff.restoreHunk",
                "diff.restoreFile"
            ]
        );

        // Another change selected, its diff still being read: the working
        // copy's stays on screen, dimmed, and offers nothing to change —
        // the bare surface's menu, with the selection still copying.
        model.update(cx, |model, cx| {
            model.select_node(Some("pmwzqkvt".into()), cx)
        });
        let loading = menu_for(&model, window, on_line(VIEWPORT, 39), cx);
        assert!(
            loading.iter().all(|id| !id.starts_with("diff.")),
            "{loading:?}"
        );
        assert!(loading.contains(&"copy.selection") && loading.contains(&"app.refresh"));
        cx.run_until_parked();
        let conflicted = menu_for(&model, window, on_header(CONFLICTED), cx);
        assert_eq!(
            changing(&conflicted),
            ["diff.resolve", "diff.moveFile", "diff.restoreFile"]
        );

        // A new operation lands and reading the diff at it fails: the diff
        // on screen was read at the previous operation, so it offers
        // nothing to change until a read at this one succeeds.
        let next = model.read_with(cx, |model, _| {
            let backend = model.host.backend();
            backend
                .describe(Path::new(&repo), "lwnouzpy", "elsewhere")
                .unwrap();
            backend.open(Path::new(&repo)).unwrap()
        });
        let (jj, away) = (Path::new(&repo).join(".jj"), dir.join("jj-away"));
        std::fs::rename(&jj, &away).unwrap();
        model.update(cx, |model, cx| model.apply_snapshot(Arc::new(next), cx));
        cx.run_until_parked();
        let failed = workbench(window, cx).read_with(cx, |view, _| view.diff.error.clone());
        assert!(failed.is_some());
        let stale = menu_for(&model, window, on_header(CONFLICTED), cx);
        assert!(stale.iter().all(|id| !id.starts_with("diff.")), "{stale:?}");

        std::fs::rename(&away, &jj).unwrap();
        workbench(window, cx).update(cx, |view, cx| view.retry_diff(cx));
        cx.run_until_parked();
        let retried = menu_for(&model, window, on_header(CONFLICTED), cx);
        assert_eq!(
            changing(&retried),
            ["diff.resolve", "diff.moveFile", "diff.restoreFile"]
        );

        // A comparison asked for while the change's own diff is on screen:
        // nothing to change while it loads, and only reading once it is
        // there — a comparison's hunks are not the change's own.
        workbench(window, cx).update(cx, |view, cx| view.set_compare(CompareMode::Trunk, cx));
        let comparing = menu_for(&model, window, on_header(CONFLICTED), cx);
        assert!(
            comparing.iter().all(|id| !id.starts_with("diff.")),
            "{comparing:?}"
        );
        cx.run_until_parked();
        let compared = workbench(window, cx).read_with(cx, |view, _| {
            let (key, diff) = view.diff.loaded.clone().expect("the comparison loaded");
            assert!(key.from.is_some() && view.diff.fresh());
            diff.files[0].path.clone()
        });
        let reading = menu_for(&model, window, on_header(&compared), cx);
        assert!(reading.contains(&"diff.copyPath"), "{reading:?}");
        assert!(changing(&reading).is_empty(), "{reading:?}");
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Whether `handle` has the keys in `window`.
    fn has_keys(
        window: WindowHandle<Shell>,
        handle: impl Fn(&super::WorkbenchView) -> gpui::FocusHandle,
        cx: &mut TestAppContext,
    ) -> bool {
        let view = workbench(window, cx);
        window
            .update(cx, |_, window, cx| handle(view.read(cx)).is_focused(window))
            .unwrap()
    }

    /// Tab from wherever the keys are until `handle` has them.
    fn tab_to(
        window: WindowHandle<Shell>,
        handle: impl Fn(&super::WorkbenchView) -> gpui::FocusHandle + Copy,
        cx: &mut TestAppContext,
    ) {
        for _ in 0..40 {
            if has_keys(window, handle, cx) {
                return;
            }
            crate::testing::press(window.into(), "tab", cx);
        }
        panic!("Tab never got there");
    }

    #[gpui::test]
    fn the_file_tree_is_one_stop_its_keys_travel_and_fold(cx: &mut TestAppContext) {
        let (dir, _model, window) = mock_workbench("keyboard-tree", cx);
        let keys = window.into();
        let tree = |view: &super::WorkbenchView| view.diff.tree_focus.clone();
        tab_to(window, tree, cx);
        // The ring rides the row the keys start from, the file being read.
        let view = workbench(window, cx);
        window
            .update(cx, |_, window, cx| {
                let focus = view.read(cx).diff.tree_focus.clone();
                assert!(crate::ui::keyboard_on(&focus, window));
            })
            .unwrap();
        // ↓ from the file being read to the next folder; Return folds it,
        // Space opens it again.
        crate::testing::press(keys, "down", cx);
        let folder = view.read_with(cx, |view, _| view.diff.tree_cursor.clone().unwrap());
        assert_eq!(folder, "src/lib/styles");
        let folded = |cx: &mut TestAppContext| {
            view.read_with(cx, |view, _| view.diff.tree_collapsed.contains(&folder))
        };
        crate::testing::press(keys, "enter", cx);
        assert!(folded(cx));
        crate::testing::press(keys, "space", cx);
        assert!(!folded(cx));
        // Shift-Tab leaves the tree for the control before it; Tab is back.
        crate::testing::press(keys, "shift-tab", cx);
        assert!(!has_keys(window, tree, cx));
        crate::testing::press(keys, "tab", cx);
        assert!(has_keys(window, tree, cx));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[gpui::test]
    fn an_annotate_origin_is_followed_from_the_keyboard(cx: &mut TestAppContext) {
        let (dir, model, window) = mock_workbench("keyboard-annotate", cx);
        let keys = window.into();
        let working = model.read_with(cx, |model, _| {
            model.snapshot().unwrap().working_copy.clone()
        });
        model.update(cx, |model, cx| {
            model.open_annotate(working, VIEWPORT.into(), cx)
        });
        cx.run_until_parked();
        let lines = |view: &super::WorkbenchView| view.annotate.focus.clone();
        tab_to(window, lines, cx);
        let view = workbench(window, cx);
        // Where each run of one origin's lines begins, and its origin.
        let runs: Vec<(usize, String)> = view.read_with(cx, |view, _| {
            let data = view.annotate.data.clone().expect("the file is annotated");
            super::annotate::run_starts(&data)
                .map(|at| (at, data.origins[data.lines[at].origin as usize].id.clone()))
                .collect()
        });
        assert!(runs.len() > 1, "{runs:?}");
        // ↓ lands on the first run, then steps to the next; ↑ steps back.
        let cursor = |cx: &mut TestAppContext| view.read_with(cx, |view, _| view.annotate.cursor);
        crate::testing::press(keys, "down", cx);
        assert_eq!(cursor(cx), Some(runs[0].0));
        crate::testing::press(keys, "down down up", cx);
        assert_eq!(cursor(cx), Some(runs[1].0));
        // Return goes to the change the run came from.
        crate::testing::press(keys, "enter", cx);
        model.read_with(cx, |model, _| {
            assert_eq!(
                model.ui().selected_node.as_deref(),
                Some(runs[1].1.as_str())
            );
        });
        let _ = std::fs::remove_dir_all(dir);
    }

    #[gpui::test]
    fn a_split_is_made_from_the_keyboard_alone(cx: &mut TestAppContext) {
        let (dir, model, window) = mock_workbench("keyboard-split", cx);
        let keys = window.into();
        let view = workbench(window, cx);
        let focused = |cx: &mut TestAppContext| {
            window
                .update(cx, |_, window, cx| window.focused(cx))
                .unwrap()
        };
        // From the graph: the divider, then the working copy's verbs —
        // Describe, New, Bookmark — and More, which Space opens.
        let graph = view.read_with(cx, |view, _| view.graph_focus.clone());
        window
            .update(cx, |_, window, cx| window.focus(&graph, cx))
            .unwrap();
        crate::testing::press(keys, "tab tab tab tab tab", cx);
        let more = focused(cx);
        crate::testing::press(keys, "space", cx);
        assert!(view.read_with(cx, |view, _| view.header.more.is_open()));
        // The menu's arrows walk it: Rebase, then Split, which Space picks.
        crate::testing::press(keys, "down down space", cx);
        view.read_with(cx, |view, _| {
            assert_eq!(view.header.tray, super::header::Tray::Split);
            assert!(!view.header.more.is_open());
        });
        // Into the tray: past its two modes to the first file, checked.
        crate::testing::press(keys, "tab tab tab space", cx);
        view.read_with(cx, |view, _| {
            assert!(view.header.split.contains_key(VIEWPORT), "nothing checked");
        });
        // Past the file's hunks, the other file, the description and
        // Cancel to Split, now on offer; Return makes the split.
        crate::testing::press(keys, "tab tab tab tab tab enter", cx);
        let done = model.read_with(cx, |model, _| model.ui().last_mutation.clone());
        assert!(done.is_some(), "the split did not run");
        // The tray closes, and the keys are back on More.
        assert_eq!(
            view.read_with(cx, |view, _| view.header.tray.clone()),
            super::header::Tray::None
        );
        assert_eq!(focused(cx), more);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[gpui::test]
    fn the_dividers_move_from_the_keyboard_and_the_widths_are_kept(cx: &mut TestAppContext) {
        let (dir, model, window) = mock_workbench("keyboard-dividers", cx);
        let keys = window.into();
        let view = workbench(window, cx);
        let widths = |cx: &mut TestAppContext| {
            view.read_with(cx, |view, _| (view.shown_width(), view.diff.tree_width))
        };
        let (graph, tree) = widths(cx);
        let (min, max) = view.read_with(cx, |view, _| view.graph_limits());

        tab_to(window, |view| view.divider_focus.clone(), cx);
        crate::testing::press(keys, "right", cx);
        assert_eq!(widths(cx).0, graph + 16.);
        crate::testing::press(keys, "left left", cx);
        assert_eq!(widths(cx).0, graph - 16.);
        crate::testing::press(keys, "home", cx);
        assert_eq!(widths(cx).0, min);
        crate::testing::press(keys, "end", cx);
        assert_eq!(widths(cx).0, max);
        crate::testing::press(keys, "left", cx);

        tab_to(window, |view| view.diff.tree_divider.clone(), cx);
        crate::testing::press(keys, "right right", cx);
        assert_eq!(widths(cx).1, tree + 32.);
        crate::testing::press(keys, "home", cx);
        assert_eq!(widths(cx).1, super::diffpane::TREE_MIN);
        crate::testing::press(keys, "end left", cx);
        assert_eq!(widths(cx).1, super::diffpane::TREE_MAX - 16.);

        // Both are kept as they moved, and a relaunch reads them back.
        let kept = |key: &str| crate::store::Store::at(dir.join("data")).pref::<f32>(key);
        assert_eq!(kept("pane.graph"), Some(max - 16.));
        assert_eq!(kept("pane.filetree"), Some(super::diffpane::TREE_MAX - 16.));
        drop(model);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[gpui::test]
    fn typing_in_a_field_types_and_tab_leaves_it(cx: &mut TestAppContext) {
        let (dir, model, window) = mock_workbench("keyboard-typing", cx);
        let keys = window.into();
        let view = workbench(window, cx);
        window
            .update(cx, |_, window, cx| {
                view.update(cx, |view, cx| view.open_editor(window, cx))
            })
            .unwrap();
        let selected = |cx: &mut TestAppContext| {
            model.read_with(cx, |model, _| model.ui().selected_node.clone())
        };
        let before = selected(cx);
        // The workbench's bare keys, Space and the arrows are text here.
        crate::testing::press(keys, "j k g w space left right", cx);
        cx.simulate_input(keys, "@");
        let (text, mode, editing) = view.read_with(cx, |view, cx| {
            (
                view.header.editor.read(cx).text().to_owned(),
                view.mode,
                view.header.tray.clone(),
            )
        });
        assert_eq!(text, "jkgw @");
        assert_eq!(mode, crate::app::WorkbenchMode::Graph);
        assert_eq!(editing, super::header::Tray::Edit);
        assert_eq!(selected(cx), before);
        // Tab moves on, as from any field: the editor keeps its text.
        crate::testing::press(keys, "tab", cx);
        let still_in = window
            .update(cx, |_, window, cx| {
                view.read(cx).header.editor.read(cx).is_focused(window)
            })
            .unwrap();
        assert!(!still_in);
        let _ = std::fs::remove_dir_all(dir);
    }
}
