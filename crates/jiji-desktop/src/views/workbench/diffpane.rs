//! The right pane: the continuous multi-file diff for the current
//! selection — every changed file in one scroll, with the file tree as its
//! map and navigation, not a gate.

use std::cell::Cell;
use std::collections::HashSet;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use gpui::{
    div, prelude::*, px, AnyElement, Bounds, Context, Entity, Focusable, FontWeight, MouseButton,
    MouseDownEvent, MouseMoveEvent, Pixels, ScrollHandle, Window,
};
use jiji_core::snapshot::{ChangeDiff, FileStatus};

use super::{ShownDiff, WorkbenchView};
use crate::app::{DiffKey, DiffLayout, DiffRead, UiIntent};
use crate::diff::model::Prepared;
use crate::diff::view::{DiffEvent, DiffTarget, ResolveOffer};
use crate::logic::conflicts::{can_resolve, merge_tool_label};
use crate::logic::inspect::{range_compare_from, resolve_compare_from, CompareMode};
use crate::logic::text::short_id;
use crate::logic::tree::{active_row_key, build_tree_rows, visible_tree_rows, TreeFile, TreeRow};
use crate::theme::{theme, Theme, RADIUS_S, TEXT_M, TEXT_S, TEXT_XS, TOOLBAR_H};
use crate::ui::splitter::{moved, splitter, Narrower, Narrowest, Wider, Widest, STEP};
use crate::ui::ScrollY as _;
use crate::ui::{
    bounds_probe, diffstat, empty_state, h_flex, icon, icon_btn, keyboard_on, lead_bar, popover,
    ring, scrollbar, segmented, tooltip, truncated, v_flex, IconName, InputEvent, NodeExt as _,
    Segment, Tabbable as _, TextInput,
};

pub(super) const TREE_MIN: f32 = 160.;
pub(super) const TREE_MAX: f32 = 400.;
const TREE_INITIAL: f32 = 220.;
const TREE_ROW_H: f32 = 26.;
/// Five seven-point blocks and their four gaps (`ui::diffstat`).
const DIFFSTAT_W: f32 = 41.;

gpui::actions!(
    file_tree,
    [
        TreeDown,
        TreeUp,
        TreeHome,
        TreeEnd,
        TreeLeft,
        TreeRight,
        TreeActivate
    ]
);

pub fn bind_keys(cx: &mut gpui::App) {
    let tree = Some("FileTree");
    cx.bind_keys([
        gpui::KeyBinding::new("down", TreeDown, tree),
        gpui::KeyBinding::new("up", TreeUp, tree),
        gpui::KeyBinding::new("home", TreeHome, tree),
        gpui::KeyBinding::new("end", TreeEnd, tree),
        gpui::KeyBinding::new("left", TreeLeft, tree),
        gpui::KeyBinding::new("right", TreeRight, tree),
        gpui::KeyBinding::new("enter", TreeActivate, tree),
        gpui::KeyBinding::new("space", TreeActivate, tree),
    ]);
}

#[derive(Clone, Copy)]
pub(super) enum TreeKey {
    Down,
    Up,
    Home,
    End,
    Left,
    Right,
    Activate,
}

pub(super) struct DiffState {
    /// The comparison the selection asks for.
    want: Option<DiffKey>,
    /// What the surface holds.
    pub(super) loaded: Option<(DiffKey, Arc<ChangeDiff>)>,
    pub(super) error: Option<String>,
    /// The fetch outlasted the slow timer.
    slow: bool,
    seq: u64,
    pub(super) layout: DiffLayout,
    pub(super) tree_on: bool,
    pub(super) tree_width: f32,
    tree_resizing: bool,
    pub(super) tree_rows: Rc<Vec<TreeRow>>,
    pub(super) tree_collapsed: HashSet<String>,
    pub(super) tree_scroll: ScrollHandle,
    pub(super) tree_focus: gpui::FocusHandle,
    pub(super) tree_divider: gpui::FocusHandle,
    /// The row keyboard travel is on (a directory's or a file's path).
    pub(super) tree_cursor: Option<String>,
    /// The row last brought into view for the file being read.
    tree_revealed: Option<String>,
    pub(super) active_file: usize,
    find_open: bool,
    find_input: Entity<TextInput>,
    resolve_error: Option<String>,
    /// The selection a change-pinned comparison was picked for.
    pub(super) pinned_to: Option<String>,
    bounds: Rc<Cell<Bounds<Pixels>>>,
}

impl DiffState {
    pub(super) fn new(
        layout: DiffLayout,
        tree_on: bool,
        tree_width: f32,
        find_input: Entity<TextInput>,
        tree_focus: gpui::FocusHandle,
        tree_divider: gpui::FocusHandle,
    ) -> Self {
        Self {
            tree_focus,
            tree_divider,
            tree_cursor: None,
            tree_revealed: None,
            want: None,
            loaded: None,
            error: None,
            slow: false,
            seq: 0,
            layout,
            tree_on,
            tree_width,
            tree_resizing: false,
            tree_rows: Rc::default(),
            tree_collapsed: HashSet::new(),
            tree_scroll: ScrollHandle::new(),
            active_file: 0,
            find_open: false,
            find_input,
            resolve_error: None,
            pinned_to: None,
            bounds: Rc::default(),
        }
    }

    /// The loaded diff when it is the one the selection asks for.
    pub(super) fn current(&self) -> Option<&Arc<ChangeDiff>> {
        match (&self.loaded, &self.want) {
            (Some((loaded, diff)), Some(want)) if loaded.same_comparison(want) => Some(diff),
            _ => None,
        }
    }

    /// Current, and read at the operation and repo on screen: only then
    /// may it supply file mutations.
    pub(super) fn fresh(&self) -> bool {
        matches!((&self.loaded, &self.want), (Some((loaded, _)), Some(want)) if loaded == want)
    }
}

pub(super) fn status_glyph(status: FileStatus, theme: &Theme) -> (&'static str, gpui::Hsla) {
    match status {
        FileStatus::Added => ("A", theme.add_fg),
        // The usual case stays quiet; color marks the files that came or
        // went.
        FileStatus::Modified => ("M", theme.text3),
        FileStatus::Removed => ("D", theme.del_fg),
        FileStatus::Renamed => ("R", theme.accent_strong),
        FileStatus::Copied => ("C", theme.accent_strong),
    }
}

impl WorkbenchView {
    pub(super) fn diff_prefs(app: &crate::app::AppModel) -> (DiffLayout, bool, f32) {
        let layout = match app.store.pref::<String>("diff.layout").as_deref() {
            Some("split") => DiffLayout::Split,
            _ => DiffLayout::Unified,
        };
        let tree_on = app.store.pref::<bool>("diff.tree").unwrap_or(true);
        let tree_width = app
            .store
            .pref::<f32>("pane.filetree")
            .filter(|width| (TREE_MIN..=TREE_MAX).contains(width))
            .unwrap_or(TREE_INITIAL);
        (layout, tree_on, tree_width)
    }

    pub(super) fn on_diff_event(
        &mut self,
        event: &DiffEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            DiffEvent::ActiveFile(file) => {
                self.diff.active_file = *file;
                cx.notify();
            }
            DiffEvent::Resolve(file) => {
                let path = self
                    .surface
                    .read(cx)
                    .files()
                    .get(*file)
                    .map(|file| file.path.to_string());
                if let Some(path) = path {
                    self.resolve_file(path, cx);
                }
            }
            DiffEvent::Context { position, target } => {
                self.open_diff_menu(*position, target.clone(), window, cx);
            }
        }
    }

    pub(super) fn on_find_input(
        &mut self,
        event: &InputEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            InputEvent::Change => {
                let query = self.diff.find_input.read(cx).text().to_owned();
                self.surface.update(cx, |surface, cx| {
                    if surface.set_find(&query, cx) > 0 {
                        surface.reveal_match(cx);
                    }
                });
            }
            InputEvent::Submit => self
                .surface
                .update(cx, |surface, cx| surface.find_step(1, cx)),
            InputEvent::SubmitBack => self
                .surface
                .update(cx, |surface, cx| surface.find_step(-1, cx)),
            InputEvent::Cancel => self.close_find(window, cx),
            InputEvent::Blur => {}
        }
        cx.notify();
    }

    // ── What to show ──

    /// What the diff is measured from for the current subject: an active
    /// range owns the comparison (root parent → head, always), else the
    /// compare mode resolves against the selection.
    pub(super) fn compare_from(&self, range: Option<&[String]>, node: &str) -> Option<String> {
        match range {
            Some(range) => range_compare_from(&self.snapshot, range),
            None => resolve_compare_from(&self.snapshot, node, &self.compare),
        }
    }

    pub(super) fn op_id(&self) -> String {
        let ops = &self.snapshot.operations;
        ops.iter()
            .find(|op| op.is_current)
            .or(ops.first())
            .map(|op| op.id.clone())
            .unwrap_or_default()
    }

    pub(super) fn set_compare(&mut self, mode: CompareMode, cx: &mut Context<Self>) {
        let (_, node) = self.subject(cx);
        self.diff.pinned_to = match &mode {
            CompareMode::Change(_) => node,
            _ => None,
        };
        self.compare = mode;
        self.sync_diff(cx);
        cx.notify();
    }

    /// Point the surface at the selection: fetch when the comparison (or
    /// the operation it was read at) changed, keep the previous diff on
    /// screen meanwhile.
    pub(super) fn sync_diff(&mut self, cx: &mut Context<Self>) {
        let (range, node) = self.subject(cx);
        let Some(node) = node else {
            self.diff.want = None;
            self.diff.error = None;
            self.diff.resolve_error = None;
            self.shown = None;
            return;
        };
        // A pinned change comparison belongs to the pair it was picked
        // for; moving the selection anywhere else drops back to the
        // parent diff — otherwise every later selection keeps comparing
        // against a change that stopped mattering.
        if matches!(self.compare, CompareMode::Change(_))
            && self.diff.pinned_to.as_deref() != Some(node.as_str())
        {
            self.compare = CompareMode::Parent;
            self.diff.pinned_to = None;
        }
        let from = self.compare_from(range.as_deref(), &node);
        let key = DiffKey {
            repo: self.snapshot.repo_path.clone(),
            op: self.op_id(),
            from,
            id: node,
        };
        if self.diff.want.as_ref() != Some(&key) {
            if self.diff.want.as_ref().is_none_or(|want| want.id != key.id) {
                self.diff.resolve_error = None;
            }
            self.request_diff(key, cx);
        }
        self.sync_resolve_offer(cx);
    }

    fn request_diff(&mut self, key: DiffKey, cx: &mut Context<Self>) {
        self.diff.want = Some(key.clone());
        self.diff.seq += 1;
        let seq = self.diff.seq;
        self.diff.error = None;
        self.diff.slow = false;
        let read = self
            .app
            .update(cx, |model, cx| model.fetch_diff(key.clone(), cx));
        let pending = match read {
            // Read and laid out before: on screen in this same frame.
            DiffRead::Ready(prepared) => {
                self.show_diff(key, prepared, cx);
                return;
            }
            DiffRead::Pending(pending) => pending,
        };
        let stale = self
            .diff
            .loaded
            .as_ref()
            .is_some_and(|(loaded, _)| *loaded != key);
        self.surface
            .update(cx, |surface, cx| surface.set_stale(stale, cx));
        // The skeleton appears only when the fetch outlasts this beat, so
        // it never flashes on a fast switch.
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(200))
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.diff.seq == seq && !this.diff.fresh() {
                    this.diff.slow = true;
                    cx.notify();
                }
            });
        })
        .detach();
        cx.spawn(async move |this, cx| {
            let result = pending.await;
            let _ = this.update(cx, |this, cx| {
                if this.diff.seq != seq {
                    return;
                }
                this.diff.slow = false;
                match result {
                    Ok(prepared) => this.show_diff(key, prepared, cx),
                    Err(error) => this.diff.error = Some(error.message),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// Put a diff the selection asked for on the surface.
    fn show_diff(&mut self, key: DiffKey, prepared: Prepared, cx: &mut Context<Self>) {
        // A refetch of the same comparison (a new operation landed) keeps
        // the reader's place.
        let keep = self
            .diff
            .loaded
            .as_ref()
            .is_some_and(|(loaded, _)| loaded.same_comparison(&key));
        self.surface.update(cx, |surface, cx| {
            surface.set_doc(&prepared, keep, cx);
            surface.set_stale(false, cx);
        });
        if !keep {
            // Another comparison is another tree: folds and place start
            // over.
            self.diff.tree_collapsed.clear();
            self.diff.tree_scroll.set_offset(gpui::Point::default());
            self.diff.tree_revealed = None;
            self.diff.active_file = 0;
        }
        let diff = prepared.diff().clone();
        self.shown = Some(ShownDiff {
            id: diff.id.clone(),
            from: diff.from.clone(),
        });
        self.diff.loaded = Some((key, diff));
        self.rebuild_tree(cx);
        self.refind(cx);
        self.sync_resolve_offer(cx);
        self.prefetch_neighbors(cx);
        cx.notify();
    }

    /// The error band's Retry: read the comparison the selection asks for
    /// again.
    pub(super) fn retry_diff(&mut self, cx: &mut Context<Self>) {
        if let Some(key) = self.diff.want.clone() {
            self.request_diff(key, cx);
        }
    }

    /// Warm the cache for the selections one arrow-key step away, so
    /// walking the graph swaps diffs without a fetch wait. Neighbors of a
    /// pinned comparison get their parent diff — moving the selection
    /// exits the pinned pair.
    fn prefetch_neighbors(&mut self, cx: &mut Context<Self>) {
        let Some(selected) = self.app.read(cx).ui().selected_node.clone() else {
            return;
        };
        let ids = self.walkable();
        let Some(at) = ids.iter().position(|id| *id == selected) else {
            return;
        };
        let mode = match &self.compare {
            CompareMode::Change(_) => CompareMode::Parent,
            other => other.clone(),
        };
        let op = self.op_id();
        let neighbors: Vec<DiffKey> = [at.checked_sub(1), Some(at + 1)]
            .into_iter()
            .flatten()
            .filter_map(|ix| ids.get(ix))
            .map(|id| DiffKey {
                repo: self.snapshot.repo_path.clone(),
                op: op.clone(),
                from: resolve_compare_from(&self.snapshot, id, &mode),
                id: id.clone(),
            })
            .collect();
        self.app.update(cx, |model, cx| {
            for key in neighbors {
                model.prefetch_diff(key, cx);
            }
        });
    }

    fn rebuild_tree(&mut self, cx: &mut Context<Self>) {
        let surface = self.surface.read(cx);
        let files: Vec<TreeFile> = surface
            .files()
            .iter()
            .map(|file| TreeFile {
                path: &file.path,
                renamed_from: file.renamed_from.as_deref(),
                status: file.status,
                has_conflict: file.has_conflict,
                added: file.added,
                removed: file.removed,
            })
            .collect();
        self.diff.tree_rows = Rc::new(build_tree_rows(&files));
    }

    /// Resolve on conflicted file headers: only on the change's own diff —
    /// a comparison span can mark files conflicted at its endpoints, but
    /// the action rewrites the selection.
    fn sync_resolve_offer(&mut self, cx: &mut Context<Self>) {
        let offer = self.diff.want.as_ref().and_then(|want| {
            let tool = self.snapshot.resolve_tool.as_deref()?;
            if want.from.is_some()
                || !self.diff.fresh()
                || !can_resolve(&self.snapshot, Some(&want.id))
            {
                return None;
            }
            let resolving = self.app.read(cx).ui().resolving_conflict.clone();
            Some(ResolveOffer {
                tool: merge_tool_label(tool).to_owned().into(),
                waiting: resolving
                    .as_ref()
                    .filter(|(change, _)| *change == want.id)
                    .map(|(_, path)| path.clone()),
                disabled: resolving.is_some(),
            })
        });
        self.surface
            .update(cx, |surface, cx| surface.set_resolve(offer, cx));
    }

    fn resolve_file(&mut self, path: String, cx: &mut Context<Self>) {
        let Some(change) = self.diff.want.as_ref().map(|want| want.id.clone()) else {
            return;
        };
        self.diff.resolve_error = None;
        let task = self
            .app
            .update(cx, |model, cx| model.resolve_conflict(change, path, cx));
        cx.spawn(async move |this, cx| {
            if let Err(error) = task.await {
                let _ = this.update(cx, |this, cx| {
                    this.diff.resolve_error = Some(error.message);
                    cx.notify();
                });
            }
        })
        .detach();
        cx.notify();
    }

    // ── Layout, tree, find ──

    pub(super) fn set_layout(&mut self, layout: DiffLayout, cx: &mut Context<Self>) {
        self.diff.layout = layout;
        let name = match layout {
            DiffLayout::Unified => "unified",
            DiffLayout::Split => "split",
        };
        self.app
            .update(cx, |model, _| model.store.set_pref("diff.layout", &name));
        self.surface.update(cx, |surface, cx| {
            surface.set_split(layout == DiffLayout::Split, cx)
        });
        self.set_evolution_layout(layout, cx);
        cx.notify();
    }

    pub(super) fn set_tree(&mut self, on: bool, cx: &mut Context<Self>) {
        self.diff.tree_on = on;
        self.app
            .update(cx, |model, _| model.store.set_pref("diff.tree", &on));
        cx.notify();
    }

    pub(super) fn jump_to_file(&mut self, index: usize, cx: &mut Context<Self>) {
        self.surface
            .update(cx, |surface, cx| surface.scroll_to_file(index, cx));
    }

    fn refind(&mut self, cx: &mut Context<Self>) {
        if !self.diff.find_open {
            return;
        }
        // Another diff on screen: matches carry over, the position resets.
        let query = self.diff.find_input.read(cx).text().to_owned();
        self.surface.update(cx, |surface, cx| {
            surface.set_find(&query, cx);
        });
    }

    /// ⌘F: the query persists while the bar is open, across selections —
    /// walking a stack with a term in hand is the point. A one-line text
    /// selection in hand becomes the query: the "find this" flow.
    pub(super) fn open_find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(selection) = self.surface.read(cx).selection_on_one_line() {
            self.diff
                .find_input
                .update(cx, |input, cx| input.set_text(selection, cx));
        }
        self.diff.find_open = true;
        self.refind(cx);
        self.diff.find_input.update(cx, |input, cx| {
            input.focus(window, cx);
            input.select_all_text(cx);
        });
        cx.notify();
    }

    fn close_find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.diff.find_open {
            return;
        }
        self.diff.find_open = false;
        self.surface.update(cx, |surface, cx| {
            surface.clear_find(cx);
            window.focus(&surface.focus_handle(cx), cx);
        });
        cx.notify();
    }

    /// Esc with the find bar up closes it first.
    pub(super) fn dismiss_find(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let open = self.diff.find_open;
        if open {
            self.close_find(window, cx);
        }
        open
    }

    /// Intents the diff pane owns: layout, the file tree, find.
    pub(super) fn take_diff_intent(
        &mut self,
        intent: UiIntent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match intent {
            UiIntent::Layout(layout) => self.set_layout(layout, cx),
            UiIntent::FileTree => self.set_tree(!self.diff.tree_on, cx),
            UiIntent::Find if self.diff.want.is_some() => {
                self.open_find(window, cx);
            }
            _ => {}
        }
    }

    fn drag_tree_divider(&mut self, event: &MouseMoveEvent, cx: &mut Context<Self>) {
        if !self.diff.tree_resizing {
            return;
        }
        if event.pressed_button != Some(MouseButton::Left) {
            self.end_tree_resize(cx);
            return;
        }
        let left: f32 = self.diff.bounds.get().left().into();
        let x: f32 = event.position.x.into();
        self.diff.tree_width = (x - left).clamp(TREE_MIN, TREE_MAX);
        cx.notify();
    }

    fn end_tree_resize(&mut self, cx: &mut Context<Self>) {
        if !self.diff.tree_resizing {
            return;
        }
        self.diff.tree_resizing = false;
        self.keep_tree_width(cx);
    }

    /// The tree's divider moved from the keyboard.
    fn nudge_tree(&mut self, by: f32, cx: &mut Context<Self>) {
        self.diff.tree_width = moved(self.diff.tree_width, TREE_MIN, TREE_MAX, by);
        self.keep_tree_width(cx);
    }

    fn keep_tree_width(&mut self, cx: &mut Context<Self>) {
        let width = self.diff.tree_width.round();
        self.app
            .update(cx, |model, _| model.store.set_pref("pane.filetree", &width));
        cx.notify();
    }

    // ── Rendering ──

    /// A range owns the header: what run is shown and what it is measured
    /// from, in place of the single-change details. Single-change verbs
    /// return with the anchor when the range collapses (esc / ×).
    fn render_range_header(&mut self, range: &[String], cx: &mut Context<Self>) -> AnyElement {
        let t = theme(cx);
        let short = |id: &str| -> (String, String) {
            (
                id.chars().take(2).collect(),
                id.chars().skip(2).take(6).collect(),
            )
        };
        let head = range.first().cloned().unwrap_or_default();
        let root = range.last().cloned().unwrap_or_default();
        let from = range_compare_from(&self.snapshot, range).unwrap_or_default();
        let from_short = short_id(&from, 8);
        let root_short = short_id(&root, 8);
        let id = |id: &str| {
            let (lead, rest) = short(id);
            h_flex()
                .child(
                    div()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(t.text1)
                        .child(lead),
                )
                .child(rest)
        };
        h_flex()
            .h(px(TOOLBAR_H))
            .flex_shrink_0()
            .gap(px(8.))
            .px(px(12.))
            .border_b_1()
            .border_color(t.border2)
            .text_size(px(TEXT_S))
            .child(icon(IconName::Stack, 13., t.text3))
            .child(
                div()
                    .font_weight(FontWeight::MEDIUM)
                    .child(format!("{} changes as one diff", range.len())),
            )
            .child(
                h_flex()
                    .id("range-span")
                    .gap(px(6.))
                    .font_family(t.mono.clone())
                    .text_color(t.text3)
                    .tooltip(tooltip(format!(
                        "Everything these {} changes add together — measured from {from_short}, the parent of {root_short}",
                        range.len()
                    )))
                    .child(id(&root))
                    .child("→")
                    .child(id(&head)),
            )
            .child(
                div()
                    .text_color(t.text3)
                    .child(format!("vs {from_short}")),
            )
            .child(div().flex_1())
            .child(self.tree_toggle(cx))
            .child(self.layout_toggle(cx))
            .child(
                icon_btn("range-close", IconName::Close)
                    .size(24., 13.)
                    .tooltip("Back to a single change (esc)")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.app.update(cx, |model, cx| {
                            let anchor = model.ui().selected_node.clone();
                            model.tab_mut().ui.selected_range = None;
                            model.select_node(anchor, cx);
                            cx.notify();
                        });
                    })),
            )
            .into_any_element()
    }

    pub(super) fn tree_toggle(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let on = self.diff.tree_on;
        icon_btn("toggle-tree", IconName::ListTree)
            .size(24., 13.)
            .on(on)
            .pressed(on)
            .tooltip(if on {
                "Hide the file tree"
            } else {
                "Show the file tree"
            })
            .on_click(cx.listener(move |this, _, _, cx| this.set_tree(!on, cx)))
    }

    pub(super) fn layout_toggle(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let this = cx.entity().downgrade();
        segmented(
            "diff-layout",
            "Diff layout",
            vec![
                Segment::icon(DiffLayout::Unified, IconName::DiffUnified, "Unified diff"),
                Segment::icon(DiffLayout::Split, IconName::DiffSplit, "Side-by-side diff"),
            ],
            &self.diff.layout,
            cx,
            move |layout, _, cx| {
                let layout = *layout;
                let _ = this.update(cx, |this, cx| this.set_layout(layout, cx));
            },
        )
    }

    /// The diff's map, docked on the left: rows mirror the scroller — the
    /// file under the reading line carries the accent — and directories
    /// fold their subtree into an aggregate.
    /// Keyboard travel in the file tree — the WAI-ARIA tree contract,
    /// minus type-ahead: ↑/↓ and Home/End move, ← collapses a directory or
    /// climbs to the parent, → expands or steps in, Enter activates.
    pub(super) fn tree_key(&mut self, key: TreeKey, cx: &mut Context<Self>) {
        let rows = self.diff.tree_rows.clone();
        let visible = visible_tree_rows(&rows, &self.diff.tree_collapsed);
        if visible.is_empty() {
            return;
        }
        // Travel starts from the cursor, else from the file being read.
        let start = self.diff.tree_cursor.clone().unwrap_or_else(|| {
            let active = self
                .surface
                .read(cx)
                .files()
                .get(self.diff.active_file)
                .map(|file| file.path.to_string())
                .unwrap_or_default();
            active_row_key(&rows, &self.diff.tree_collapsed, &active).to_owned()
        });
        let index = visible
            .iter()
            .position(|row| row.path() == start)
            .unwrap_or(0);
        let last = visible.len() - 1;
        let row = visible[index];
        let target = match key {
            TreeKey::Down => (index + 1).min(last),
            TreeKey::Up => index.saturating_sub(1),
            TreeKey::Home => 0,
            TreeKey::End => last,
            TreeKey::Left => match row {
                TreeRow::Dir(dir) if !self.diff.tree_collapsed.contains(&dir.path) => {
                    self.diff.tree_collapsed.insert(dir.path.clone());
                    index
                }
                // The nearest row above that is one level out.
                _ => {
                    let depth = row.depth();
                    (0..index)
                        .rev()
                        .find(|&above| visible[above].depth() < depth)
                        .unwrap_or(index)
                }
            },
            TreeKey::Right => match row {
                TreeRow::Dir(dir) if self.diff.tree_collapsed.contains(&dir.path) => {
                    self.diff.tree_collapsed.remove(&dir.path);
                    index
                }
                TreeRow::Dir(_) => (index + 1).min(last),
                TreeRow::File(_) => index,
            },
            TreeKey::Activate => match row {
                TreeRow::Dir(dir) => {
                    if !self.diff.tree_collapsed.remove(&dir.path) {
                        self.diff.tree_collapsed.insert(dir.path.clone());
                    }
                    index
                }
                TreeRow::File(file) => {
                    let file_index = file.index;
                    self.diff.tree_cursor = Some(file.path.clone());
                    self.jump_to_file(file_index, cx);
                    return;
                }
            },
        };
        self.diff.tree_cursor = Some(visible[target].path().to_owned());
        self.diff.tree_scroll.scroll_to_item(target);
        cx.notify();
    }

    /// A file row's right-click: the menu its diff header offers, through
    /// the same door — so its actions, what they act on and whether they
    /// are on offer while another diff loads are the header's.
    pub(super) fn open_tree_menu(
        &mut self,
        index: usize,
        position: gpui::Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let path = self
            .surface
            .read(cx)
            .files()
            .get(index)
            .map(|file| file.path.to_string());
        let target = DiffTarget {
            file: Some(index),
            path,
            on_header: true,
            ..DiffTarget::default()
        };
        self.open_diff_menu(position, target, window, cx);
    }

    fn render_tree(&mut self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx);
        let rows = self.diff.tree_rows.clone();
        let active_path = self
            .surface
            .read(cx)
            .files()
            .get(self.diff.active_file)
            .map(|file| file.path.to_string())
            .unwrap_or_default();
        let active = active_row_key(&rows, &self.diff.tree_collapsed, &active_path).to_owned();
        let visible = visible_tree_rows(&rows, &self.diff.tree_collapsed);
        // The row standing for the file being read comes into view whenever
        // it changes: the diff scrolled on, or a fold made a directory
        // stand in for it. The scroller places it once this frame is laid
        // out.
        if self.diff.tree_revealed.as_deref() != Some(active.as_str()) {
            if let Some(ix) = visible.iter().position(|row| row.path() == active) {
                self.diff.tree_scroll.scroll_to_item(ix);
            }
            self.diff.tree_revealed = Some(active.clone());
        }
        // The keyboard's place: where travel starts, the cursor or else the
        // file being read. It wears the ring while the keyboard drives the
        // tree, and is what assistive technology reads as focused.
        let focused = self.diff.tree_focus.is_focused(window);
        let cursor = self
            .diff
            .tree_cursor
            .clone()
            .unwrap_or_else(|| active.clone());
        let ringed = keyboard_on(&self.diff.tree_focus, window);
        // Counts close each row as the five-block diffstat, in a column of
        // its own so names keep one width; the numbers are in the tooltip.
        let stat = |added: u32, removed: u32| {
            div()
                .w(px(DIFFSTAT_W))
                .flex_shrink_0()
                .when(added + removed > 0, |el| {
                    el.child(diffstat(added, removed, cx))
                })
        };
        let mut tree = div()
            .id("file-tree")
            .role(gpui::Role::Tree)
            .aria_label("Changed files")
            .key_context("FileTree")
            .track_focus(&self.diff.tree_focus)
            .on_action(cx.listener(|this, _: &TreeDown, _, cx| this.tree_key(TreeKey::Down, cx)))
            .on_action(cx.listener(|this, _: &TreeUp, _, cx| this.tree_key(TreeKey::Up, cx)))
            .on_action(cx.listener(|this, _: &TreeHome, _, cx| this.tree_key(TreeKey::Home, cx)))
            .on_action(cx.listener(|this, _: &TreeEnd, _, cx| this.tree_key(TreeKey::End, cx)))
            .on_action(cx.listener(|this, _: &TreeLeft, _, cx| this.tree_key(TreeKey::Left, cx)))
            .on_action(cx.listener(|this, _: &TreeRight, _, cx| this.tree_key(TreeKey::Right, cx)))
            .on_action(
                cx.listener(|this, _: &TreeActivate, _, cx| this.tree_key(TreeKey::Activate, cx)),
            )
            .size_full()
            .pt(px(4.))
            .pb(px(24.))
            .scroll_y()
            .track_scroll(&self.diff.tree_scroll);
        for (ix, row) in visible.into_iter().enumerate() {
            let depth = row.depth();
            let mut guides = h_flex().flex_shrink_0().h_full();
            for _ in 0..depth {
                // One cell per ancestor level, each threading a hairline
                // under its parent's chevron.
                guides = guides.child(
                    div()
                        .w(px(14.))
                        .h_full()
                        .flex()
                        .justify_center()
                        .child(div().w(px(1.)).h_full().bg(t.border2)),
                );
            }
            // Rows run edge to edge, like the graph's: the one on screen
            // takes the selection fill and the leading bar. Each is a child
            // of the scroller, which is what lets it scroll a row into view.
            let here = cursor == row.path();
            let base = h_flex()
                .id(("tree-row", ix))
                .role(gpui::Role::TreeItem)
                .aria_level(depth + 1)
                .aria_selected(active == row.path())
                .when(focused && here, |el| el.aria_active_descendant())
                .when(ringed && here, |el| el.shadow(ring(t.accent)))
                .relative()
                .w_full()
                .h(px(TREE_ROW_H))
                .pl(px(8.))
                .pr(px(10.))
                .text_size(px(TEXT_M))
                .cursor_pointer()
                .child(guides);
            let el = match row {
                TreeRow::Dir(dir) => {
                    let open = !self.diff.tree_collapsed.contains(&dir.path);
                    let path = dir.path.clone();
                    let mut el = base
                        .aria_label(dir.label.clone())
                        .aria_expanded(open)
                        .aria_description(format!(
                            "{}, {}",
                            dir.path,
                            crate::logic::text::counted(dir.file_count, "file")
                        ))
                        .tooltip(tooltip(dir.path.clone()))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if !this.diff.tree_collapsed.remove(&path) {
                                this.diff.tree_collapsed.insert(path.clone());
                            }
                            cx.notify();
                        }))
                        .child(div().w(px(14.)).flex().justify_center().child(icon(
                            if open {
                                IconName::ChevronDown
                            } else {
                                IconName::ChevronRight
                            },
                            11.,
                            t.text3,
                        )))
                        .child(
                            div()
                                .ml(px(5.))
                                .min_w_0()
                                .text_color(t.text2)
                                .child(truncated(dir.label.clone())),
                        );
                    if !open {
                        if dir.has_conflict {
                            el = el.child(div().ml(px(6.)).child(crate::ui::dot(t.danger, 5.)));
                        }
                        el = el.child(div().flex_1()).child(
                            h_flex()
                                .flex_shrink_0()
                                .pl(px(8.))
                                .gap(px(8.))
                                .font_family(t.mono.clone())
                                .text_size(px(TEXT_XS))
                                .text_color(t.text3)
                                .child(dir.file_count.to_string())
                                .child(stat(dir.added, dir.removed)),
                        );
                    }
                    if active == dir.path {
                        el.bg(t.selected).child(lead_bar(cx))
                    } else {
                        el.hover(|style| style.bg(t.bg_hover))
                    }
                }
                TreeRow::File(file) => {
                    let (glyph, hue) = status_glyph(file.status, &t);
                    let index = file.index;
                    let gone = file.status == FileStatus::Removed;
                    let mut el = base
                        .aria_label(file.name.clone())
                        .aria_description(file.title())
                        .tooltip(tooltip(file.title()))
                        .on_click(cx.listener(move |this, _, _, cx| this.jump_to_file(index, cx)))
                        .on_mouse_down(
                            MouseButton::Right,
                            cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                                cx.stop_propagation();
                                this.open_tree_menu(index, event.position, window, cx);
                            }),
                        )
                        .child(
                            // The file's kind in its language's hue, in the
                            // chevron's cell so names align with folders.
                            div()
                                .w(px(14.))
                                .flex()
                                .justify_center()
                                .child(crate::ui::file_icon(&file.name, 14., cx)),
                        )
                        .child(
                            div()
                                .ml(px(5.))
                                .min_w_0()
                                .text_color(if gone { t.text2 } else { t.text1 })
                                .when(gone, |el| el.line_through())
                                .child(truncated(file.name.clone())),
                        );
                    if file.has_conflict {
                        el = el.child(div().ml(px(6.)).child(crate::ui::dot(t.danger, 5.)));
                    }
                    // What came, went or moved says so before the counts;
                    // the usual modified file carries no mark.
                    let mark = (file.status != FileStatus::Modified).then(|| {
                        div()
                            .font_family(t.mono.clone())
                            .text_size(px(TEXT_XS))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(hue)
                            .child(glyph)
                    });
                    el = el.child(div().flex_1()).child(
                        h_flex()
                            .flex_shrink_0()
                            .pl(px(8.))
                            .gap(px(8.))
                            .children(mark)
                            .child(stat(file.added, file.removed)),
                    );
                    if active == file.path {
                        el.bg(t.selected).child(lead_bar(cx))
                    } else {
                        el.hover(|style| style.bg(t.bg_hover))
                    }
                }
            };
            // A press anywhere on a row hands the tree the keys.
            let (focus, key) = (self.diff.tree_focus.clone(), row.path().to_owned());
            tree = tree.child(el.on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, window, cx| {
                    window.focus(&focus, cx);
                    this.diff.tree_cursor = Some(key.clone());
                    cx.notify();
                }),
            ));
        }
        div()
            .relative()
            .size_full()
            .child(tree)
            .child(scrollbar("file-tree-scrollbar", &self.diff.tree_scroll))
    }

    /// The find bar floats over the surface's top-right, clear of the
    /// pinned file headers. Enter/⇧Enter cycle, esc closes.
    fn render_find_bar(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx);
        let (current, count) = self.surface.read(cx).find_position();
        let typed = !self.diff.find_input.read(cx).is_empty();
        let (position, spoken) = match (typed, count) {
            (false, _) => (String::new(), String::new()),
            (true, 0) => ("0/0".to_owned(), "No matches".to_owned()),
            (true, count) => (
                format!("{}/{count}", current + 1),
                format!("Match {} of {count}", current + 1),
            ),
        };
        let nav = |id: &'static str, glyph: IconName, tip: &'static str, enabled: bool| {
            icon_btn(id, glyph)
                .size(22., 12.)
                .disabled(!enabled)
                .tooltip(tip)
        };
        popover(cx)
            .id("find-bar")
            .role(gpui::Role::Search)
            .aria_label("Find in diff")
            .key_context("FindBar")
            .absolute()
            .top(px(8.))
            .right(px(16.))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(8.))
            .h(px(32.))
            .pl(px(10.))
            .pr(px(4.))
            .child(icon(IconName::Search, 12., t.text3))
            .child(
                div()
                    .w(px(180.))
                    .flex()
                    .text_size(px(TEXT_M))
                    .line_height(px(20.))
                    .child(self.diff.find_input.clone()),
            )
            .child(
                div()
                    .id("find-position")
                    .status(spoken)
                    .flex_shrink_0()
                    .min_w(px(28.))
                    .text_size(px(TEXT_XS))
                    .text_color(if typed && count == 0 { t.warn } else { t.text3 })
                    .child(position),
            )
            .child(
                nav(
                    "find-prev",
                    IconName::ArrowUp,
                    "Previous match (⇧↩)",
                    count > 0,
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    this.surface
                        .update(cx, |surface, cx| surface.find_step(-1, cx));
                    cx.notify();
                })),
            )
            .child(
                nav(
                    "find-next",
                    IconName::ArrowDown,
                    "Next match (↩)",
                    count > 0,
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    this.surface
                        .update(cx, |surface, cx| surface.find_step(1, cx));
                    cx.notify();
                })),
            )
            .child(
                nav("find-close", IconName::Close, "Close (esc)", true)
                    .on_click(cx.listener(|this, _, window, cx| this.close_find(window, cx))),
            )
    }

    /// Loading: the shape of a diff, still.
    fn render_skeleton(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx);
        let mut el = v_flex().gap(px(9.)).p(px(16.));
        for width in [0.42, 0.78, 0.66, 0.84, 0.51, 0.7] {
            el = el.child(
                div()
                    .h(px(10.))
                    .w(gpui::relative(width))
                    .rounded(px(3.))
                    .bg(t.bg_hover),
            );
        }
        el
    }

    pub(super) fn render_diff_pane(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let t = theme(cx);
        let (range, node) = self.subject(cx);
        let Some(node_id) = node else {
            return self.render_inspector(window, cx).into_any_element();
        };
        let header = match &range {
            Some(range) => self.render_range_header(range, cx),
            None => self.render_change_header(&node_id, window, cx),
        };
        // Below these widths the tree would squeeze the diff into a ribbon
        // (side-by-side needs room for two columns of code); it steps
        // aside.
        let narrow = self.diff_width
            < if self.diff.layout == DiffLayout::Split {
                1040.
            } else {
                760.
            };
        let current = self.diff.current().cloned();
        // What the body renders. While a switch's fetch is in flight the
        // previous diff stays on screen (dimmed) instead of blanking to
        // the skeleton; the skeleton appears only when the fetch outlasts
        // the slow timer or nothing has loaded yet. An error clears the
        // surface — except mid-refetch of the same comparison.
        let displayed = if self.diff.error.is_some() || current.is_some() {
            current.clone()
        } else if self.diff.slow {
            None
        } else {
            self.diff.loaded.as_ref().map(|(_, diff)| diff.clone())
        };
        let tree_visible = self.diff.tree_on
            && !narrow
            && displayed.as_ref().is_some_and(|diff| diff.files.len() > 1);
        let node = crate::logic::inspect::find_node(&self.snapshot, &node_id);

        let body: AnyElement = match &displayed {
            Some(diff) if diff.files.is_empty() => div()
                .size_full()
                .child(empty_state(
                    IconName::Commit,
                    "No file changes",
                    if range.is_some() {
                        "Nothing differs across this selection — its changes cancel out."
                    } else if self
                        .diff
                        .want
                        .as_ref()
                        .is_some_and(|want| want.from.is_some())
                    {
                        "Nothing differs between these two changes."
                    } else if node.is_some_and(|node| node.is_empty) {
                        "This change is empty — nothing differs from its parent."
                    } else {
                        "Nothing differs from the parent tree."
                    },
                    None,
                    cx,
                ))
                .into_any_element(),
            Some(diff) => {
                let mut el = v_flex()
                    .size_full()
                    .child(div().flex_1().min_h_0().child(self.surface.clone()));
                if diff.truncated {
                    el = el.child(
                        div()
                            .flex_shrink_0()
                            .px(px(16.))
                            .py(px(6.))
                            .border_t_1()
                            .border_color(t.border2)
                            .text_size(px(TEXT_S))
                            .text_color(t.text3)
                            .child(
                                "File list truncated — this change touches more files than Jiji renders",
                            ),
                    );
                }
                el.into_any_element()
            }
            None if self.diff.error.is_some() => div()
                .size_full()
                .child(empty_state(
                    IconName::DiffUnified,
                    "Couldn’t load this diff",
                    "Retry to load the file changes.",
                    None,
                    cx,
                ))
                .into_any_element(),
            None => self.render_skeleton(cx).into_any_element(),
        };

        let mut surface_area = div()
            .relative()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_row()
            .on_mouse_move(cx.listener(|this, event, _, cx| this.drag_tree_divider(event, cx)))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| this.end_tree_resize(cx)),
            )
            .child(bounds_probe(self.diff.bounds.clone()));
        if tree_visible {
            let resizing = self.diff.tree_resizing;
            let press = cx.listener(|this: &mut Self, _: &MouseDownEvent, _, cx| {
                this.diff.tree_resizing = true;
                cx.notify();
            });
            surface_area = surface_area
                .child(
                    div()
                        .w(px(self.diff.tree_width))
                        .flex_shrink_0()
                        .h_full()
                        .child(self.render_tree(window, cx)),
                )
                .child(
                    splitter(
                        "tree-divider",
                        "Resize file tree",
                        &self.diff.tree_divider,
                        self.diff.tree_width,
                        TREE_MIN,
                        TREE_MAX,
                        resizing,
                        window,
                        cx,
                        |grip| grip.on_mouse_down(MouseButton::Left, press),
                    )
                    .on_action(cx.listener(|this, _: &Narrower, _, cx| this.nudge_tree(-STEP, cx)))
                    .on_action(cx.listener(|this, _: &Wider, _, cx| this.nudge_tree(STEP, cx)))
                    .on_action(cx.listener(|this, _: &Narrowest, _, cx| {
                        this.nudge_tree(f32::NEG_INFINITY, cx)
                    }))
                    .on_action(
                        cx.listener(|this, _: &Widest, _, cx| this.nudge_tree(f32::INFINITY, cx)),
                    ),
                );
        }
        let mut right = div().relative().flex_1().min_w_0().h_full().child(body);
        if self.diff.find_open {
            right = right.child(self.render_find_bar(cx));
        }
        surface_area = surface_area.child(right);
        // A reading pane over the diff takes its place while open.
        let over = if range.is_none() {
            match self.render_annotate(window, cx) {
                Some(pane) => Some(pane),
                None => self.render_evolution(window, cx),
            }
        } else {
            None
        };
        let surface_area = match over {
            Some(pane) => pane,
            None => surface_area.into_any_element(),
        };

        let mut pane = v_flex().size_full().child(header);
        if let Some(error) = &self.diff.resolve_error {
            pane = pane.child(
                div()
                    .id("resolve-error")
                    .alert(error.clone())
                    .flex_shrink_0()
                    .px(px(16.))
                    .py(px(6.))
                    .text_size(px(TEXT_S))
                    .text_color(t.danger)
                    .bg(Theme::fade(t.danger, 0.08))
                    .border_b_1()
                    .border_color(t.border2)
                    .child(error.clone()),
            );
        }
        if let Some(error) = &self.diff.error {
            pane = pane.child(
                h_flex()
                    .id("diff-error")
                    .flex_shrink_0()
                    .gap(px(8.))
                    .px(px(16.))
                    .py(px(6.))
                    .text_size(px(TEXT_S))
                    .text_color(t.danger)
                    .bg(Theme::fade(t.danger, 0.08))
                    .border_b_1()
                    .border_color(t.border2)
                    .child(icon(IconName::Conflicts, 14., t.danger))
                    .child(
                        div()
                            .id("diff-error-text")
                            .alert(error.clone())
                            .flex_1()
                            .min_w_0()
                            .child(error.clone()),
                    )
                    .child(
                        div()
                            .id("diff-retry")
                            .role(gpui::Role::Button)
                            .aria_label("Retry")
                            .tabbable(cx)
                            .flex_shrink_0()
                            .px(px(6.))
                            .rounded(px(RADIUS_S))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(t.text1)
                            .cursor_pointer()
                            .hover(|style| style.bg(t.bg_hover))
                            .on_click(cx.listener(|this, _, _, cx| this.retry_diff(cx)))
                            .child("Retry"),
                    ),
            );
        }
        pane.child(surface_area).into_any_element()
    }
}
