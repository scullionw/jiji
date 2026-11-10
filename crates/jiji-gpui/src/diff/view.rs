//! The diff surface: one custom-painted GPUI element.
//!
//! Nothing here is a tree of per-row elements. The element owns the whole
//! viewport and paints it directly each frame: find the visible rows with a
//! binary search over the layout's prefix sums, fill their backgrounds,
//! shape and paint their text, then overlay the pinned file header and the
//! scroll thumbs. Scrolling, hit-testing, selection and copy all run
//! against the same row model, so there is no DOM to mount or unmount and
//! nothing for a scroll-anchoring heuristic to fight.

use std::collections::HashMap;
use std::ops::Range;

use gpui::{
    actions, fill, font, point, prelude::*, px, quad, relative, App, BorderStyle, Bounds,
    ClipboardItem, ContentMask, Context, CursorStyle, DispatchPhase, ElementId, Entity,
    EventEmitter, FocusHandle, Focusable, Font, FontStyle, FontWeight, GlobalElementId, Hitbox,
    HitboxBehavior, Hsla, InspectorElementId, KeyBinding, LayoutId, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, Pixels, Point, ScrollWheelEvent, ShapedLine, SharedString,
    StrikethroughStyle, Style, TextRun, TouchPhase, TransformationMatrix, Window,
};
use jiji_core::snapshot::FileStatus;

use crate::diff::highlight::{Mark, Syn};
use crate::diff::model::{
    DiffDoc, FileModel, Kind, Layout, Line, LineId, Row, GAP_H, HEADER_H, IMAGE_H, IMAGE_MAX_H,
    LINE_H, NOTE_H, REGION_H, SIDE_H,
};
use crate::theme::{theme, Theme};
use crate::ui::IconName;

actions!(
    diff,
    [
        CopySelection,
        SelectAll,
        PageDown,
        PageUp,
        ScrollTop,
        ScrollBottom,
        NextFile,
        PrevFile,
    ]
);

/// Keys that belong to the diff while it has focus.
pub fn bind_keys(cx: &mut App) {
    let diff = Some("DiffSurface");
    cx.bind_keys([
        KeyBinding::new("cmd-c", CopySelection, diff),
        KeyBinding::new("cmd-a", SelectAll, diff),
        KeyBinding::new("space", PageDown, diff),
        KeyBinding::new("pagedown", PageDown, diff),
        KeyBinding::new("shift-space", PageUp, diff),
        KeyBinding::new("pageup", PageUp, diff),
        KeyBinding::new("cmd-up", ScrollTop, diff),
        KeyBinding::new("home", ScrollTop, diff),
        KeyBinding::new("cmd-down", ScrollBottom, diff),
        KeyBinding::new("end", ScrollBottom, diff),
        KeyBinding::new("]", NextFile, diff),
        KeyBinding::new("[", PrevFile, diff),
    ]);
}

const CODE_SIZE: f32 = 12.;
const XS: f32 = 11.;
const SMALL: f32 = 12.;
/// Left padding inside a code cell, and the breathing room kept after the
/// widest line at full pan.
const CODE_PAD_L: f32 = 8.;
const CODE_PAD_R: f32 = 16.;
/// Width of the strip at the right edge that belongs to the scroll thumb.
const THUMB_ZONE: f32 = 12.;

/// What sits under a right-click, for the context menu.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DiffTarget {
    /// The file under the pointer, as an index into the diff's files.
    pub file: Option<usize>,
    pub path: Option<String>,
    /// The pointer is on a file header rather than a line.
    pub on_header: bool,
    pub old_no: Option<u32>,
    pub new_no: Option<u32>,
    /// The line's own text, without the gutter.
    pub line_text: Option<String>,
    /// The current text selection, when there is one.
    pub selection: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum DiffEvent {
    /// The file under the reading line (the surface's top edge) changed.
    ActiveFile(usize),
    Context {
        position: Point<Pixels>,
        target: DiffTarget,
    },
    /// A conflicted file's Resolve button was pressed.
    Resolve(usize),
}

/// The Resolve affordance on conflicted file headers.
#[derive(Clone, Debug, PartialEq)]
pub struct ResolveOffer {
    /// The merge tool's display name.
    pub tool: SharedString,
    /// The file currently handed to the tool, by path.
    pub waiting: Option<String>,
    /// Another conflict is with the tool; this one cannot start.
    pub disabled: bool,
}

/// One occurrence of the find query: a file's path (its header), or a
/// stretch of one line.
#[derive(Clone, Debug, PartialEq)]
pub struct FindMatch {
    pub file: usize,
    pub line: Option<(LineId, Range<u32>)>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Side {
    Left,
    Right,
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
struct Pos {
    row: usize,
    col: usize,
}

/// A text selection. In the split layout it lives on one side, like
/// selecting in one pane of a side-by-side tool: dragging never drags the
/// other side's text into the clipboard.
#[derive(Clone, Copy, Debug)]
struct Selection {
    anchor: Pos,
    head: Pos,
    side: Side,
}

impl Selection {
    fn ordered(&self) -> (Pos, Pos) {
        if self.anchor <= self.head {
            (self.anchor, self.head)
        } else {
            (self.head, self.anchor)
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Drag {
    None,
    Select,
    /// Dragging the vertical thumb; the offset is where on the thumb it was
    /// grabbed.
    Thumb(f32),
}

#[derive(Clone, Copy, PartialEq)]
enum Axis {
    X,
    Y,
}

/// Which code column a paint pass covers.
#[derive(Clone, Copy, PartialEq)]
enum Column {
    /// Full-width lines: the unified layout, and conflict content.
    Full,
    Left,
    Right,
}

struct Metrics {
    code: Font,
    ui: Font,
    /// Advance of one column at the code size and at the gutter size.
    ch: f32,
    ch_xs: f32,
    plus: ShapedLine,
    minus: ShapedLine,
}

/// Where things sit this frame, in window coordinates.
#[derive(Clone, Copy)]
struct Geo {
    left: f32,
    right: f32,
    top: f32,
    bottom: f32,
    numw: f32,
    signw: f32,
    /// Left edge of the code cell in a full-width row.
    full_code: f32,
    /// The split divider, and each pane's code cell.
    mid: f32,
    left_code: f32,
    right_code: f32,
}

impl Geo {
    fn height(&self) -> f32 {
        self.bottom - self.top
    }
}

fn rect(x0: f32, y0: f32, x1: f32, y1: f32) -> Bounds<Pixels> {
    Bounds::from_corners(point(px(x0), px(y0)), point(px(x1), px(y1)))
}

pub struct DiffSurface {
    doc: DiffDoc,
    /// The layouts with collapsed files' bodies left out; `None` while
    /// every file is expanded.
    folded: Option<(Layout, Layout)>,
    collapsed: Vec<bool>,
    focus: FocusHandle,
    split: bool,
    scroll_y: f32,
    /// Horizontal offset per file: long lines pan sideways within their
    /// own file, so one wide file never leaves the others scrolled blank.
    pan: Vec<f32>,
    selection: Option<Selection>,
    drag: Drag,
    thumb_hover: bool,
    /// The file header under the pointer, and whether the pointer is on
    /// its Resolve button.
    header_hover: Option<(usize, bool)>,
    axis: Option<Axis>,
    metrics: Option<Metrics>,
    /// The palette the cached shapes were colored for.
    shaped_for: &'static str,
    geo: Option<Geo>,
    /// Shaped code lines near the viewport, kept across frames so a steady
    /// frame shapes nothing.
    shaped: HashMap<LineId, ShapedLine>,
    numbers: HashMap<(u32, u8), ShapedLine>,
    /// Bumped whenever the document is replaced, so highlight results for
    /// a previous document are dropped.
    generation: u64,
    find: Vec<FindMatch>,
    find_current: usize,
    resolve: Option<ResolveOffer>,
    active_file: usize,
    /// Dim the content: what is shown is being replaced.
    stale: bool,
}

impl Focusable for DiffSurface {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl EventEmitter<DiffEvent> for DiffSurface {}

impl DiffSurface {
    pub fn new(split: bool, cx: &mut Context<Self>) -> Self {
        Self {
            doc: DiffDoc::empty(),
            folded: None,
            collapsed: Vec::new(),
            focus: cx.focus_handle(),
            split,
            scroll_y: 0.,
            pan: Vec::new(),
            selection: None,
            drag: Drag::None,
            thumb_hover: false,
            header_hover: None,
            axis: None,
            metrics: None,
            shaped_for: "",
            geo: None,
            shaped: HashMap::new(),
            numbers: HashMap::new(),
            generation: 0,
            find: Vec::new(),
            find_current: 0,
            resolve: None,
            active_file: 0,
            stale: false,
        }
    }

    /// Put another diff on the surface. `keep_position` is for a refetch
    /// of the same comparison (an auto-refresh): the reader stays where
    /// they were, with the same files collapsed.
    pub fn set_doc(&mut self, mut doc: DiffDoc, keep_position: bool, cx: &mut Context<Self>) {
        self.generation += 1;
        let generation = self.generation;
        // Highlight every file off the main thread, one task per file, in
        // file order so the top of the diff colors first. Each result
        // lands as a plain repaint.
        for job in std::mem::take(&mut doc.jobs) {
            let work = cx.background_executor().spawn(async move { job.run() });
            cx.spawn(async move |this, cx| {
                let marks = work.await;
                this.update(cx, |this, cx| {
                    if this.generation == generation {
                        this.apply_marks(marks, cx);
                    }
                })
                .ok();
            })
            .detach();
        }
        let files = doc.files.len();
        if keep_position && self.collapsed.len() == files {
            // Same comparison: keep which files are folded, by path.
            let folded: Vec<&str> = self
                .doc
                .files
                .iter()
                .zip(&self.collapsed)
                .filter(|(_, collapsed)| **collapsed)
                .map(|(file, _)| file.path.as_ref())
                .collect();
            self.collapsed = doc
                .files
                .iter()
                .map(|file| folded.contains(&file.path.as_ref()))
                .collect();
        } else {
            self.collapsed = vec![false; files];
            self.scroll_y = 0.;
            self.active_file = 0;
        }
        self.pan = vec![0.; files];
        self.doc = doc;
        self.selection = None;
        self.drag = Drag::None;
        self.shaped.clear();
        self.find.clear();
        self.find_current = 0;
        self.refold();
        self.clamp_scroll();
        cx.notify();
    }

    pub fn files(&self) -> &[FileModel] {
        &self.doc.files
    }

    pub fn set_stale(&mut self, stale: bool, cx: &mut Context<Self>) {
        if self.stale != stale {
            self.stale = stale;
            cx.notify();
        }
    }

    pub fn set_resolve(&mut self, offer: Option<ResolveOffer>, cx: &mut Context<Self>) {
        if self.resolve != offer {
            self.resolve = offer;
            cx.notify();
        }
    }

    fn refold(&mut self) {
        self.folded = self.collapsed.iter().any(|collapsed| *collapsed).then(|| {
            (
                self.doc.unified.collapsing(&self.collapsed),
                self.doc.split.collapsing(&self.collapsed),
            )
        });
    }

    /// Fold or unfold one file's body. The header stays where the reader
    /// has it: collapsing a file being read mid-way lands on its header.
    pub fn toggle_file(&mut self, file: usize, cx: &mut Context<Self>) {
        let Some(slot) = self.collapsed.get_mut(file) else {
            return;
        };
        *slot = !*slot;
        self.selection = None;
        let was_above = self.layout().file_top(file) < self.scroll_y;
        self.refold();
        if was_above {
            self.scroll_y = self.layout().file_top(file);
        }
        self.clamp_scroll();
        cx.notify();
    }

    /// Bring a file's header to the top of the surface, unfolding it.
    pub fn scroll_to_file(&mut self, file: usize, cx: &mut Context<Self>) {
        if file >= self.doc.files.len() {
            return;
        }
        if self.collapsed.get(file).copied().unwrap_or(false) {
            self.collapsed[file] = false;
            self.refold();
        }
        let top = self.layout().file_top(file);
        self.scroll_to(top, cx);
    }

    // ── Find ──

    /// Search the diff for `query` (a case-insensitive literal): each
    /// file's path first — the header is a match surface too — then its
    /// lines top to bottom. Returns how many matches there are.
    pub fn set_find(&mut self, query: &str, cx: &mut Context<Self>) -> usize {
        self.find = find_in_doc(&self.doc, query);
        self.find_current = 0;
        cx.notify();
        self.find.len()
    }

    pub fn clear_find(&mut self, cx: &mut Context<Self>) {
        if !self.find.is_empty() {
            self.find.clear();
            self.find_current = 0;
            cx.notify();
        }
    }

    pub fn find_position(&self) -> (usize, usize) {
        (self.find_current, self.find.len())
    }

    /// Step to another match: its file unfolds if collapsed, then it
    /// scrolls to a third of the way down the surface.
    pub fn find_step(&mut self, delta: isize, cx: &mut Context<Self>) {
        let count = self.find.len() as isize;
        if count == 0 {
            return;
        }
        self.find_current = (self.find_current as isize + delta).rem_euclid(count) as usize;
        self.reveal_match(cx);
    }

    pub fn reveal_match(&mut self, cx: &mut Context<Self>) {
        let Some(found) = self.find.get(self.find_current).cloned() else {
            return;
        };
        if self.collapsed.get(found.file).copied().unwrap_or(false) && found.line.is_some() {
            self.collapsed[found.file] = false;
            self.refold();
        }
        let layout = self.layout();
        let y = match &found.line {
            None => layout.file_top(found.file),
            Some((line, _)) => match layout.row_of_line(found.file, *line) {
                Some(row) => {
                    let viewport = self.geo.map_or(600., |geo| geo.height());
                    layout.tops[row] - viewport / 3.
                }
                None => layout.file_top(found.file),
            },
        };
        self.scroll_to(y, cx);
    }

    /// The single-line text selection, if any: what ⌘F adopts as the
    /// query.
    pub fn selection_on_one_line(&self) -> Option<String> {
        self.selected_text().filter(|text| !text.contains('\n'))
    }

    fn apply_marks(&mut self, marks: Vec<(LineId, Vec<Mark>)>, cx: &mut Context<Self>) {
        for (id, marks) in marks {
            if let Some(line) = self.doc.lines.get_mut(id as usize) {
                line.marks = marks;
            }
            self.shaped.remove(&id);
        }
        cx.notify();
    }

    fn layout(&self) -> &Layout {
        match (&self.folded, self.split) {
            (Some((_, split)), true) => split,
            (Some((unified, _)), false) => unified,
            (None, true) => &self.doc.split,
            (None, false) => &self.doc.unified,
        }
    }

    fn max_scroll(&self) -> f32 {
        let viewport = self.geo.map_or(0., |geo| geo.height());
        (self.layout().height() - viewport).max(0.)
    }

    fn max_pan(&self, file: usize) -> f32 {
        let (Some(geo), Some(metrics), Some(file)) =
            (self.geo, self.metrics.as_ref(), self.doc.files.get(file))
        else {
            return 0.;
        };
        let content = file.max_cols as f32 * metrics.ch + CODE_PAD_L + CODE_PAD_R;
        let visible = if self.split {
            geo.mid - geo.left_code
        } else {
            geo.right - geo.full_code
        };
        (content - visible).max(0.)
    }

    /// A file's pan offset, held inside its current range (the range
    /// moves with the window width and the layout).
    fn pan_of(&self, file: usize) -> f32 {
        self.pan
            .get(file)
            .map_or(0., |pan| pan.clamp(0., self.max_pan(file)))
    }

    fn set_pan(&mut self, file: usize, pan: f32) {
        let max = self.max_pan(file);
        if let Some(slot) = self.pan.get_mut(file) {
            *slot = pan.clamp(0., max);
        }
    }

    fn clamp_scroll(&mut self) {
        self.scroll_y = self.scroll_y.clamp(0., self.max_scroll());
    }

    pub fn scroll_to(&mut self, y: f32, cx: &mut Context<Self>) {
        self.scroll_y = y;
        self.clamp_scroll();
        cx.notify();
    }

    /// The text line a row shows on a side, if it shows one.
    fn text_line(&self, row: usize, side: Side) -> Option<LineId> {
        match self.layout().rows.get(row)? {
            Row::Line { line, .. } => Some(*line),
            Row::Split { left, right } => match side {
                Side::Left => left.map(|cell| cell.line),
                Side::Right => right.map(|cell| cell.line),
            },
            _ => None,
        }
    }

    // ── Actions ──

    fn copy(&mut self, _: &CopySelection, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = self.selected_text() {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
    }

    pub fn selected_text(&self) -> Option<String> {
        let selection = self.selection?;
        let (start, end) = selection.ordered();
        if start == end {
            return None;
        }
        let mut out = String::new();
        let mut first = true;
        for row in start.row..=end.row {
            let Some(id) = self.text_line(row, selection.side) else {
                continue;
            };
            let line = &self.doc.lines[id as usize];
            let len = line.text.len();
            let from = if row == start.row {
                start.col.min(len)
            } else {
                0
            };
            let to = if row == end.row {
                end.col.min(len)
            } else {
                len
            };
            if !first {
                out.push('\n');
            }
            first = false;
            out.push_str(&line.source_slice(from..to.max(from)));
        }
        (!out.is_empty()).then_some(out)
    }

    fn select_all(&mut self, _: &SelectAll, _: &mut Window, cx: &mut Context<Self>) {
        let side = self.selection.map_or(Side::Right, |s| s.side);
        let last = self.layout().rows.len().saturating_sub(1);
        let col = self
            .text_line(last, side)
            .map_or(0, |id| self.doc.lines[id as usize].text.len());
        self.selection = Some(Selection {
            anchor: Pos { row: 0, col: 0 },
            head: Pos { row: last, col },
            side,
        });
        cx.notify();
    }

    /// Switch layouts, keeping the reader in the same file at the same
    /// relative depth: row indices differ between layouts, files do not.
    pub fn set_split(&mut self, split: bool, cx: &mut Context<Self>) {
        if split == self.split {
            return;
        }
        if self.doc.files.is_empty() {
            self.split = split;
            cx.notify();
            return;
        }
        let old = self.layout();
        let file = old.file_of(old.row_at(self.scroll_y));
        let span = (old.file_bottom(file) - old.file_top(file)).max(1.);
        let depth = ((self.scroll_y - old.file_top(file)) / span).clamp(0., 1.);
        self.split = split;
        let new = self.layout();
        self.scroll_y = new.file_top(file) + depth * (new.file_bottom(file) - new.file_top(file));
        self.selection = None;
        self.clamp_scroll();
        cx.notify();
    }

    fn page(&self) -> f32 {
        (self.geo.map_or(600., |geo| geo.height()) - 2. * LINE_H).max(LINE_H)
    }

    fn page_down(&mut self, _: &PageDown, _: &mut Window, cx: &mut Context<Self>) {
        self.scroll_to(self.scroll_y + self.page(), cx);
    }

    fn page_up(&mut self, _: &PageUp, _: &mut Window, cx: &mut Context<Self>) {
        self.scroll_to(self.scroll_y - self.page(), cx);
    }

    fn scroll_top(&mut self, _: &ScrollTop, _: &mut Window, cx: &mut Context<Self>) {
        self.scroll_to(0., cx);
    }

    fn scroll_bottom(&mut self, _: &ScrollBottom, _: &mut Window, cx: &mut Context<Self>) {
        self.scroll_to(f32::MAX, cx);
    }

    fn next_file(&mut self, _: &NextFile, _: &mut Window, cx: &mut Context<Self>) {
        let layout = self.layout();
        let file = layout.file_of(layout.row_at(self.scroll_y));
        if file + 1 < layout.file_rows.len() {
            self.scroll_to(layout.file_top(file + 1), cx);
        }
    }

    fn prev_file(&mut self, _: &PrevFile, _: &mut Window, cx: &mut Context<Self>) {
        let layout = self.layout();
        let file = layout.file_of(layout.row_at(self.scroll_y));
        let top = layout.file_top(file);
        // Mid-file, go to its top first; at the top, go to the previous one.
        let target = if self.scroll_y > top + 1. || file == 0 {
            top
        } else {
            layout.file_top(file - 1)
        };
        self.scroll_to(target, cx);
    }

    // ── Input ──

    fn on_scroll(&mut self, event: &ScrollWheelEvent, cx: &mut Context<Self>) {
        let precise = event.delta.precise();
        let delta = event.delta.pixel_delta(px(LINE_H * 2.5));
        let (mut dx, mut dy) = (f32::from(delta.x), f32::from(delta.y));
        if precise {
            // A trackpad gesture commits to the axis it starts on, so a
            // vertical fling never drifts the code sideways. The lock holds
            // through the momentum tail and clears when fingers land again.
            if matches!(event.touch_phase, TouchPhase::Started) {
                self.axis = None;
            }
            if self.axis.is_none() && (dx != 0. || dy != 0.) {
                self.axis = Some(if dx.abs() > dy.abs() {
                    Axis::X
                } else {
                    Axis::Y
                });
            }
            match self.axis {
                Some(Axis::X) => dy = 0.,
                Some(Axis::Y) => dx = 0.,
                None => {}
            }
        }
        // Sideways travel pans the file under the pointer.
        let file = self.geo.map_or(0, |geo| {
            let y = f32::from(event.position.y) - geo.top + self.scroll_y;
            self.layout().file_of(self.layout().row_at(y))
        });
        let before = (self.scroll_y, self.pan_of(file));
        self.scroll_y -= dy;
        self.clamp_scroll();
        self.set_pan(file, before.1 - dx);
        if before != (self.scroll_y, self.pan_of(file)) {
            cx.notify();
        }
    }

    fn thumb(&self) -> Option<(f32, f32)> {
        let geo = self.geo?;
        let max = self.max_scroll();
        if max <= 0. {
            return None;
        }
        let viewport = geo.height();
        let height = (viewport * viewport / self.layout().height()).max(28.);
        let top = geo.top + self.scroll_y / max * (viewport - height);
        Some((top, height))
    }

    fn on_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.focus);
        let Some(geo) = self.geo else { return };
        let (x, y) = (f32::from(event.position.x), f32::from(event.position.y));
        if let Some((file, on_resolve)) = self.header_at(x, y, window) {
            // A header is the collapse toggle, except on its Resolve
            // button.
            if on_resolve {
                let free = self.resolve.as_ref().is_some_and(|offer| !offer.disabled);
                if free {
                    cx.emit(DiffEvent::Resolve(file));
                }
            } else {
                self.toggle_file(file, cx);
            }
            return;
        }
        if x >= geo.right - THUMB_ZONE {
            if let Some((top, height)) = self.thumb() {
                // Off the thumb, the click centers it there and the drag
                // carries on from its middle.
                let grab = if y >= top && y <= top + height {
                    y - top
                } else {
                    height / 2.
                };
                self.drag = Drag::Thumb(grab);
                self.drag_thumb(y, grab);
                cx.notify();
            }
            return;
        }
        let (pos, side) = self.hit(event.position, None, window, cx);
        match (event.click_count, self.selection) {
            (1, Some(mut selection)) if event.modifiers.shift => {
                selection.head = self.hit(event.position, Some(selection.side), window, cx).0;
                self.selection = Some(selection);
            }
            (1, _) => {
                self.selection = Some(Selection {
                    anchor: pos,
                    head: pos,
                    side,
                })
            }
            (count, _) => {
                let len_and_word = self.text_line(pos.row, side).map(|id| {
                    let text = &self.doc.lines[id as usize].text;
                    (text.len(), word_at(text, pos.col))
                });
                if let Some((len, word)) = len_and_word {
                    let (from, to) = if count == 2 { word } else { (0, len) };
                    self.selection = Some(Selection {
                        anchor: Pos {
                            row: pos.row,
                            col: from,
                        },
                        head: Pos {
                            row: pos.row,
                            col: to,
                        },
                        side,
                    });
                }
            }
        }
        self.drag = if event.click_count == 1 {
            Drag::Select
        } else {
            Drag::None
        };
        cx.notify();
    }

    fn drag_thumb(&mut self, y: f32, grab: f32) {
        let (Some(geo), Some((_, height))) = (self.geo, self.thumb()) else {
            return;
        };
        let travel = (geo.height() - height).max(1.);
        self.scroll_y = (y - grab - geo.top) / travel * self.max_scroll();
        self.clamp_scroll();
    }

    /// The file header under a window point — a header row, or the one
    /// pinned to the top edge — and whether the point is on its Resolve
    /// button.
    fn header_at(&self, x: f32, y: f32, window: &Window) -> Option<(usize, bool)> {
        let geo = self.geo?;
        if x < geo.left || x >= geo.right - THUMB_ZONE || y < geo.top || y >= geo.bottom {
            return None;
        }
        let layout = self.layout();
        if layout.rows.is_empty() {
            return None;
        }
        let first = layout.row_at(self.scroll_y);
        let pinned_file = layout.file_of(first);
        let pinned_y =
            geo.top + (layout.file_bottom(pinned_file) - self.scroll_y - HEADER_H).min(0.);
        let (file, header_y) = if y < pinned_y + HEADER_H {
            (pinned_file, pinned_y)
        } else {
            let row = layout.row_at(y - geo.top + self.scroll_y);
            match layout.rows[row] {
                Row::FileHeader { file } => {
                    (file as usize, geo.top + layout.tops[row] - self.scroll_y)
                }
                _ => return None,
            }
        };
        let model = self.doc.files.get(file)?;
        let on_resolve = match (&self.resolve, self.metrics.as_ref()) {
            (Some(offer), Some(metrics)) if model.has_conflict => {
                let medium = Font {
                    weight: FontWeight::MEDIUM,
                    ..metrics.ui.clone()
                };
                let text = label(
                    resolve_label(offer, model),
                    &medium,
                    XS,
                    gpui::black(),
                    window,
                );
                resolve_rect(&text, &geo, header_y).contains(&point(px(x), px(y)))
            }
            _ => false,
        };
        Some((file, on_resolve))
    }

    /// Right-click: say what is under the pointer and let the owner offer
    /// a menu. A click inside the selection keeps it; elsewhere it moves
    /// nothing.
    fn on_context(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        cx.stop_propagation();
        window.focus(&self.focus);
        let (x, y) = (f32::from(event.position.x), f32::from(event.position.y));
        let mut target = DiffTarget {
            selection: self.selected_text(),
            ..DiffTarget::default()
        };
        if let Some((file, _)) = self.header_at(x, y, window) {
            target.file = Some(file);
            target.path = self.doc.files.get(file).map(|f| f.path.to_string());
            target.on_header = true;
        } else if !self.layout().rows.is_empty() {
            let (pos, side) = self.hit(event.position, None, window, cx);
            let layout = self.layout();
            let file = layout.file_of(pos.row);
            target.file = Some(file);
            target.path = self.doc.files.get(file).map(|f| f.path.to_string());
            match &layout.rows[pos.row] {
                Row::Line { old_no, new_no, .. } => {
                    target.old_no = *old_no;
                    target.new_no = *new_no;
                }
                Row::Split { left, right } => {
                    target.old_no = left.map(|cell| cell.no);
                    target.new_no = right.map(|cell| cell.no);
                    match side {
                        Side::Left if left.is_some() => target.new_no = None,
                        Side::Right if right.is_some() => target.old_no = None,
                        _ => {}
                    }
                }
                _ => {}
            }
            target.line_text = self
                .text_line(pos.row, side)
                .map(|id| self.doc.lines[id as usize].source_text().to_owned());
        }
        cx.emit(DiffEvent::Context {
            position: event.position,
            target,
        });
    }

    fn on_mouse_move(
        &mut self,
        event: &MouseMoveEvent,
        inside: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let held = event.pressed_button == Some(MouseButton::Left);
        let header = if inside && self.drag == Drag::None {
            self.header_at(
                f32::from(event.position.x),
                f32::from(event.position.y),
                window,
            )
        } else {
            None
        };
        if header != self.header_hover {
            self.header_hover = header;
            cx.notify();
        }
        match self.drag {
            Drag::Select if held => {
                if let Some(mut selection) = self.selection {
                    let head = self.hit(event.position, Some(selection.side), window, cx).0;
                    if head != selection.head {
                        selection.head = head;
                        self.selection = Some(selection);
                        cx.notify();
                    }
                }
            }
            Drag::Thumb(grab) if held => {
                self.drag_thumb(f32::from(event.position.y), grab);
                cx.notify();
            }
            Drag::None => {
                let hover = self.geo.is_some_and(|geo| {
                    let (x, y) = (f32::from(event.position.x), f32::from(event.position.y));
                    x >= geo.right - THUMB_ZONE && x <= geo.right && y >= geo.top && y <= geo.bottom
                });
                if hover != self.thumb_hover {
                    self.thumb_hover = hover;
                    cx.notify();
                }
            }
            // The button came up outside the window.
            _ => self.drag = Drag::None,
        }
    }

    fn on_mouse_up(&mut self, _: &MouseUpEvent, cx: &mut Context<Self>) {
        if self.drag != Drag::None {
            self.drag = Drag::None;
            cx.notify();
        }
    }

    /// The text position under a window point. `side` pins the answer to
    /// the side a drag began on; otherwise the point picks it.
    fn hit(
        &mut self,
        position: Point<Pixels>,
        side: Option<Side>,
        window: &Window,
        cx: &App,
    ) -> (Pos, Side) {
        let theme = theme(cx);
        let Some(geo) = self.geo else {
            return (Pos { row: 0, col: 0 }, Side::Right);
        };
        let (x, y) = (f32::from(position.x), f32::from(position.y));
        // A drag past either edge selects to the edge row; the autoscroll
        // in `paint` brings more rows under it.
        let y = y.clamp(geo.top, (geo.bottom - 1.).max(geo.top));
        let row = self.layout().row_at(y - geo.top + self.scroll_y);
        let side = side.unwrap_or(if self.split && x < geo.mid {
            Side::Left
        } else {
            Side::Right
        });
        let code_x = match &self.layout().rows[row] {
            Row::Split { .. } if side == Side::Left => geo.left_code,
            Row::Split { .. } => geo.right_code,
            _ => geo.full_code,
        };
        let col = match (self.text_line(row, side), self.metrics.as_ref()) {
            (Some(id), Some(metrics)) => {
                let line = shape_code(
                    &mut self.shaped,
                    &self.doc.lines,
                    id,
                    metrics,
                    &theme,
                    window,
                );
                let pan = self.pan_of(self.layout().file_of(row));
                line.closest_index_for_x(px(x - (code_x + CODE_PAD_L - pan)))
            }
            _ => 0,
        };
        (Pos { row, col }, side)
    }

    // ── Paint ──

    fn ensure_metrics(&mut self, theme: &Theme, window: &Window) {
        if self.metrics.is_some() && self.shaped_for == theme.id {
            return;
        }
        // Another palette: every cached shape carries the old colors.
        self.shaped.clear();
        self.numbers.clear();
        self.shaped_for = theme.id;
        let system = window.text_system();
        let code = font(theme.mono.clone());
        let ui = font(crate::theme::FONT_UI);
        let id = system.resolve_font(&code);
        let advance = |size: f32| {
            system
                .ch_advance(id, px(size))
                .map(f32::from)
                .unwrap_or(size * 0.6)
        };
        let bold = Font {
            weight: FontWeight::SEMIBOLD,
            ..code.clone()
        };
        self.metrics = Some(Metrics {
            ch: advance(CODE_SIZE),
            ch_xs: advance(XS),
            plus: label("+", &bold, XS, theme.add_fg, window),
            minus: label("−", &bold, XS, theme.del_fg, window),
            code,
            ui,
        });
    }

    fn geometry(&self, bounds: Bounds<Pixels>) -> Geo {
        let ch_xs = self.metrics.as_ref().map_or(6.6, |m| m.ch_xs);
        let left = f32::from(bounds.left());
        let right = f32::from(bounds.right());
        let numw = ((self.doc.digits + 1) as f32 * ch_xs).ceil();
        let signw = (2. * ch_xs).ceil();
        let mid = (left + (right - left) / 2.).floor();
        Geo {
            left,
            right,
            top: f32::from(bounds.top()),
            bottom: f32::from(bounds.bottom()),
            numw,
            signw,
            full_code: left + 2. * numw + signw,
            mid,
            left_code: left + numw + signw,
            right_code: mid + 1. + numw + signw,
        }
    }

    fn paint(&mut self, bounds: Bounds<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        let theme = theme(cx);
        self.ensure_metrics(&theme, window);
        let geo = self.geometry(bounds);
        self.geo = Some(geo);
        window.paint_quad(fill(bounds, theme.bg1));
        if self.layout().rows.is_empty() {
            return;
        }
        self.tick_autoscroll(window, cx);
        self.clamp_scroll();

        // Snap to device pixels so glyphs land on the same grid every frame.
        let scale = window.scale_factor();
        let scroll = (self.scroll_y * scale).round() / scale;
        let split = self.split;
        let selection = self.selection.filter(|s| s.anchor != s.head);
        let thumb = self.thumb();
        let thumb_hot = self.thumb_hover || matches!(self.drag, Drag::Thumb(_));
        let theme = &*theme;
        let header_hover = self.header_hover;
        let stale = self.stale;
        let resolve = self.resolve.clone();
        let collapsed = self.collapsed.clone();
        // Find matches on the rows about to paint, by line.
        let current_match = self.find.get(self.find_current).cloned();
        let mut hits: HashMap<LineId, Vec<(Range<u32>, bool)>> = HashMap::new();
        let mut path_hits: HashMap<usize, bool> = HashMap::new();
        for found in &self.find {
            let current = current_match.as_ref() == Some(found);
            match &found.line {
                Some((line, range)) => hits
                    .entry(*line)
                    .or_default()
                    .push((range.clone(), current)),
                None => {
                    path_hits.insert(found.file, current);
                }
            }
        }

        // The visible row range, and the pan offset and range of each file
        // it touches.
        let (first, last, first_file, pans) = {
            let layout = self.layout();
            let first = layout.row_at(scroll);
            let mut last = first;
            while last < layout.rows.len() && layout.tops[last] < scroll + geo.height() {
                last += 1;
            }
            let first_file = layout.file_of(first);
            let last_file = layout.file_of(last.saturating_sub(1)).max(first_file);
            let pans: Vec<(f32, f32)> = (first_file..=last_file)
                .map(|file| {
                    (
                        (self.pan_of(file) * scale).round() / scale,
                        self.max_pan(file),
                    )
                })
                .collect();
            (first, last, first_file, pans)
        };

        // The file under the reading line, for the tree's active row.
        if first_file != self.active_file {
            self.active_file = first_file;
            cx.emit(DiffEvent::ActiveFile(first_file));
        }

        let Self {
            doc,
            folded,
            shaped,
            numbers,
            metrics,
            ..
        } = self;
        let metrics = metrics.as_ref().expect("metrics resolved above");
        let layout = match (folded.as_ref(), split) {
            (Some((_, layout)), true) => layout,
            (Some((layout, _)), false) => layout,
            (None, true) => &doc.split,
            (None, false) => &doc.unified,
        };
        let lines = &doc.lines;
        let files = &doc.files;

        let row_y = |row: usize| geo.top + layout.tops[row] - scroll;
        let header = |file: usize, y: f32| HeaderPaint {
            file: &files[file],
            index: file,
            y,
            collapsed: collapsed.get(file).copied().unwrap_or(false),
            hover: header_hover
                .filter(|(hovered, _)| *hovered == file)
                .map(|(_, on_resolve)| on_resolve),
            path_hit: path_hits.get(&file).copied(),
            resolve: resolve.as_ref().filter(|_| files[file].has_conflict),
        };

        // Pass 1 — everything but code text: fills, gutter rails, numbers,
        // signs, seams and file headers.
        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            for row in first..last {
                let y = row_y(row);
                match &layout.rows[row] {
                    Row::Pad => {}
                    Row::FileHeader { file } => {
                        header(*file as usize, y).paint(&geo, metrics, theme, window, cx)
                    }
                    Row::Region { text, side } => {
                        // A conflict region opens with its ordinal; each
                        // side is captioned with what it is measured from.
                        let height = if side.is_some() { SIDE_H } else { REGION_H };
                        window.paint_quad(fill(
                            rect(geo.left, y, geo.right, y + height),
                            if side.is_some() {
                                theme.recess
                            } else {
                                Theme::fade(theme.danger, 0.08)
                            },
                        ));
                        let medium = Font {
                            weight: FontWeight::MEDIUM,
                            ..metrics.ui.clone()
                        };
                        let name = label(
                            text.clone(),
                            &medium,
                            XS,
                            if side.is_some() {
                                theme.text2
                            } else {
                                theme.danger
                            },
                            window,
                        );
                        let x = geo.left + 16.;
                        name.paint(point(px(x), px(y)), px(height), window, cx).ok();
                        if let Some(caption) = side {
                            label(caption.clone(), &metrics.ui, XS, theme.text3, window)
                                .paint(
                                    point(px(x + f32::from(name.width) + 10.), px(y)),
                                    px(height),
                                    window,
                                    cx,
                                )
                                .ok();
                        }
                    }
                    Row::Gap { skipped, context } => {
                        window.paint_quad(fill(
                            rect(geo.left, y, geo.right, y + GAP_H),
                            theme.recess,
                        ));
                        let mut x =
                            (if split { geo.left_code } else { geo.full_code }) + CODE_PAD_L;
                        if let Some(context) = context {
                            let symbol: SharedString = match context.char_indices().nth(64) {
                                Some((cut, _)) => format!("{}…", &context[..cut]).into(),
                                None => context.clone(),
                            };
                            let symbol = label(symbol, &metrics.code, XS, theme.text2, window);
                            symbol
                                .paint(point(px(x), px(y)), px(GAP_H), window, cx)
                                .ok();
                            x += f32::from(symbol.width) + 12.;
                        }
                        let noun = if *skipped == 1 { "line" } else { "lines" };
                        let count = label(
                            format!("{skipped} unchanged {noun}"),
                            &metrics.ui,
                            XS,
                            theme.text3,
                            window,
                        );
                        count.paint(point(px(x), px(y)), px(GAP_H), window, cx).ok();
                    }
                    Row::Image { file } => {
                        // Old and new side by side as plain hairlined
                        // boxes: the pictures carry the content, the
                        // caption labels the add/remove hues.
                        let Some(sides) = files
                            .get(*file as usize)
                            .and_then(|file| file.images.as_ref())
                        else {
                            continue;
                        };
                        let present = sides.iter().flatten().count().max(1) as f32;
                        let room = (geo.right - geo.left - 32. - 16. * (present - 1.)) / present;
                        let box_w = room.clamp(80., 480.);
                        let box_h = IMAGE_MAX_H + 24.;
                        let mut x = geo.left + 16.;
                        for (side, which) in sides.iter().zip(["Old", "New"]) {
                            let Some(side) = side else {
                                continue;
                            };
                            let frame = rect(x, y + 16., x + box_w, y + 16. + box_h);
                            window.paint_quad(quad(
                                frame,
                                px(0.),
                                theme.recess,
                                px(1.),
                                theme.border2,
                                BorderStyle::Solid,
                            ));
                            let mut caption = vec![(
                                which.to_owned(),
                                if which == "Old" {
                                    theme.del_fg
                                } else {
                                    theme.add_fg
                                },
                            )];
                            let picture = side
                                .image
                                .clone()
                                .and_then(|image| image.use_render_image(window, cx));
                            match picture {
                                Some(picture) => {
                                    let natural = picture.size(0);
                                    let (w, h) = (
                                        natural.width.0 as f32 / window.scale_factor(),
                                        natural.height.0 as f32 / window.scale_factor(),
                                    );
                                    // Fit inside the box; never enlarge
                                    // past the picture's own pixels.
                                    let scale = ((box_w - 24.) / w.max(1.))
                                        .min(IMAGE_MAX_H / h.max(1.))
                                        .min(window.scale_factor());
                                    let (w, h) = (w * scale, h * scale);
                                    let left = x + (box_w - w) / 2.;
                                    let top = y + 16. + (box_h - h) / 2.;
                                    let _ = window.paint_image(
                                        rect(left, top, left + w, top + h),
                                        gpui::Corners::default(),
                                        picture,
                                        0,
                                        false,
                                    );
                                    caption.push((
                                        format!("{}×{}", natural.width.0, natural.height.0),
                                        theme.text3,
                                    ));
                                }
                                None => {
                                    let note = if side.image.is_some() {
                                        "Loading…"
                                    } else {
                                        "Too large to preview"
                                    };
                                    let shaped =
                                        label(note, &metrics.ui, SMALL, theme.text3, window);
                                    let left = x + (box_w - f32::from(shaped.width)) / 2.;
                                    shaped
                                        .paint(
                                            point(px(left), px(y + 16. + box_h / 2. - 9.)),
                                            px(18.),
                                            window,
                                            cx,
                                        )
                                        .ok();
                                }
                            }
                            caption.push((side.size.clone(), theme.text3));
                            let mut cx_x = x;
                            for (text, color) in caption {
                                let shaped = label(text, &metrics.ui, XS, color, window);
                                shaped
                                    .paint(
                                        point(px(cx_x), px(y + 16. + box_h + 6.)),
                                        px(18.),
                                        window,
                                        cx,
                                    )
                                    .ok();
                                cx_x += f32::from(shaped.width) + 12.;
                            }
                            x += box_w + 16.;
                        }
                        let _ = IMAGE_H;
                    }
                    Row::Note { text, danger } => {
                        let color = if *danger { theme.danger } else { theme.text3 };
                        if *danger {
                            window.paint_quad(fill(
                                rect(geo.left, y, geo.right, y + NOTE_H),
                                Hsla {
                                    a: 0.08,
                                    ..theme.danger
                                },
                            ));
                        }
                        label(text.clone(), &metrics.ui, SMALL, color, window)
                            .paint(point(px(geo.left + 16.), px(y)), px(NOTE_H), window, cx)
                            .ok();
                    }
                    Row::Line {
                        kind,
                        old_no,
                        new_no,
                        ..
                    } => {
                        let rail = Rail {
                            x: geo.left,
                            code: geo.full_code,
                            end: geo.right,
                            y,
                        };
                        rail.fill(*kind, theme, window);
                        for (slot, no) in [old_no, new_no].into_iter().enumerate() {
                            if let Some(no) = no {
                                let right_edge = geo.left + (slot + 1) as f32 * geo.numw;
                                paint_number(
                                    numbers, *no, *kind, right_edge, y, metrics, theme, window, cx,
                                );
                            }
                        }
                        paint_sign(
                            *kind,
                            geo.left + 2. * geo.numw,
                            y,
                            &geo,
                            metrics,
                            window,
                            cx,
                        );
                    }
                    Row::Split { left, right } => {
                        window.paint_quad(fill(
                            rect(geo.mid, y, geo.mid + 1., y + LINE_H),
                            theme.border2,
                        ));
                        let panes = [
                            (left, geo.left, geo.left_code, geo.mid),
                            (right, geo.mid + 1., geo.right_code, geo.right),
                        ];
                        for (cell, x, code, end) in panes {
                            let Some(cell) = cell else {
                                // No counterpart on this side: "nothing
                                // here", not an empty line.
                                window.paint_quad(fill(rect(x, y, end, y + LINE_H), theme.recess));
                                continue;
                            };
                            Rail { x, code, end, y }.fill(cell.kind, theme, window);
                            paint_number(
                                numbers,
                                cell.no,
                                cell.kind,
                                x + geo.numw,
                                y,
                                metrics,
                                theme,
                                window,
                                cx,
                            );
                            paint_sign(cell.kind, x + geo.numw, y, &geo, metrics, window, cx);
                        }
                    }
                }
            }
        });

        // Pass 2 — code text, one clipped column at a time. Text slides
        // under the column's mask on the shared pan offset; the rails
        // painted above never move.
        let columns: &[(Column, f32, f32)] = if split {
            &[
                (Column::Left, geo.left_code, geo.mid),
                (Column::Right, geo.right_code, geo.right),
                (Column::Full, geo.full_code, geo.right),
            ]
        } else {
            &[(Column::Full, geo.full_code, geo.right)]
        };
        let (mut min_id, mut max_id) = (LineId::MAX, 0);
        for &(column, code_x, end) in columns {
            let mask = ContentMask {
                bounds: rect(code_x, geo.top, end, geo.bottom).intersect(&bounds),
            };
            window.with_content_mask(Some(mask), |window| {
                for row in first..last {
                    let (id, kind, side) = match (&layout.rows[row], column) {
                        (Row::Line { line, kind, .. }, Column::Full) => (*line, *kind, None),
                        (
                            Row::Split {
                                left: Some(cell), ..
                            },
                            Column::Left,
                        ) => (cell.line, cell.kind, Some(Side::Left)),
                        (
                            Row::Split {
                                right: Some(cell), ..
                            },
                            Column::Right,
                        ) => (cell.line, cell.kind, Some(Side::Right)),
                        _ => continue,
                    };
                    let y = row_y(row);
                    let origin_x = code_x + CODE_PAD_L - pans[layout.file_of(row) - first_file].0;
                    let line = &lines[id as usize];
                    min_id = min_id.min(id);
                    max_id = max_id.max(id);
                    let text = (!line.text.is_empty())
                        .then(|| shape_code(shaped, lines, id, metrics, theme, window));
                    let x_at = |index: usize| {
                        origin_x
                            + text
                                .as_ref()
                                .map_or(0., |t| f32::from(t.x_for_index(index)))
                    };

                    // Word-level emphasis, only where the line mixes
                    // changed and unchanged text.
                    let word = match kind {
                        Kind::Added => theme.add_word,
                        Kind::Removed => theme.del_word,
                        Kind::Context => theme.bg1,
                    };
                    for range in &line.changed {
                        window.paint_quad(quad(
                            rect(
                                x_at(range.start as usize),
                                y + 1.,
                                x_at(range.end as usize),
                                y + LINE_H - 1.,
                            ),
                            px(2.),
                            word,
                            px(0.),
                            word,
                            BorderStyle::default(),
                        ));
                    }

                    if let Some(found) = hits.get(&id) {
                        for (range, current) in found {
                            window.paint_quad(quad(
                                rect(
                                    x_at(range.start as usize),
                                    y + 1.,
                                    x_at(range.end as usize),
                                    y + LINE_H - 1.,
                                ),
                                px(2.),
                                Theme::fade(theme.warn, if *current { 0.55 } else { 0.28 }),
                                px(0.),
                                gpui::transparent_black(),
                                BorderStyle::default(),
                            ));
                        }
                    }

                    if let Some(selection) =
                        selection.filter(|s| side.is_none_or(|side| side == s.side))
                    {
                        let (start, end_pos) = selection.ordered();
                        if row >= start.row && row <= end_pos.row {
                            let len = line.text.len();
                            let from = if row == start.row {
                                start.col.min(len)
                            } else {
                                0
                            };
                            let to = if row == end_pos.row {
                                end_pos.col.min(len)
                            } else {
                                len
                            };
                            // A selection running on to the next row also
                            // covers this row's line break.
                            let tail = if row < end_pos.row { metrics.ch } else { 0. };
                            if to > from || tail > 0. {
                                window.paint_quad(fill(
                                    rect(x_at(from), y, x_at(to.max(from)) + tail, y + LINE_H),
                                    theme.selection,
                                ));
                            }
                        }
                    }

                    if let Some(text) = &text {
                        text.paint(point(px(origin_x), px(y)), px(LINE_H), window, cx)
                            .ok();
                    }
                }
            });
        }

        // The header of the file under the top edge stays pinned while its
        // lines pass, and is pushed off by the next file's header.
        if !layout.file_rows.is_empty() {
            let file = layout.file_of(first);
            if layout.file_top(file) - scroll < 0. {
                let pinned = (layout.file_bottom(file) - scroll - HEADER_H).min(0.);
                window.with_content_mask(Some(ContentMask { bounds }), |window| {
                    header(file, geo.top + pinned).paint(&geo, metrics, theme, window, cx)
                });
            }
        }

        // Overlay scroll thumbs.
        let thumb_color = |hot: bool| {
            if hot {
                theme.scrollbar_hover
            } else {
                theme.scrollbar
            }
        };
        if let Some((top, height)) = thumb {
            window.paint_quad(quad(
                rect(geo.right - 10., top + 2., geo.right - 2., top + height - 2.),
                px(4.),
                thumb_color(thumb_hot),
                px(0.),
                thumb_color(thumb_hot),
                BorderStyle::default(),
            ));
        }
        // A horizontal thumb for each visible file that pans, one per code
        // column (both panes of the split ride the file's one offset). It
        // rides the bottom of the viewport while the file passes and rests
        // on the file's own end when that comes first.
        let tracks: &[(f32, f32)] = if split {
            &[
                (geo.left_code, geo.mid),
                (geo.right_code, geo.right - THUMB_ZONE),
            ]
        } else {
            &[(geo.full_code, geo.right - THUMB_ZONE)]
        };
        for (offset, &(pan, max_pan)) in pans.iter().enumerate() {
            if max_pan <= 0. || layout.file_rows.is_empty() {
                continue;
            }
            let file = first_file + offset;
            let seat = (geo.top + layout.file_bottom(file) - scroll).min(geo.bottom);
            let body_top =
                (geo.top + layout.file_top(file) - scroll + HEADER_H).max(geo.top + HEADER_H);
            if seat - body_top < 2. * LINE_H {
                continue;
            }
            for &(x0, x1) in tracks {
                let track = x1 - x0;
                let width = (track * track / (track + max_pan)).max(28.);
                let x = x0 + pan / max_pan * (track - width);
                window.paint_quad(quad(
                    rect(x, seat - 10., x + width, seat - 2.),
                    px(4.),
                    thumb_color(false),
                    px(0.),
                    thumb_color(false),
                    BorderStyle::default(),
                ));
            }
        }

        // Keep shaped lines for the rows around the viewport only.
        if shaped.len() > 1024 && min_id <= max_id {
            let keep = min_id.saturating_sub(256)..=max_id.saturating_add(256);
            shaped.retain(|id, _| keep.contains(id));
        }
        if numbers.len() > 4096 {
            numbers.clear();
        }

        // What is shown is on its way out: dim it rather than blank it.
        if stale {
            window.paint_quad(fill(bounds, Theme::fade(theme.bg1, 0.4)));
        }
    }

    /// While a selection drag sits past the top or bottom edge, scroll
    /// toward it every frame and extend the selection to the new edge row.
    fn tick_autoscroll(&mut self, window: &mut Window, cx: &App) {
        if self.drag != Drag::Select {
            return;
        }
        let (Some(geo), Some(mut selection)) = (self.geo, self.selection) else {
            return;
        };
        let mouse = window.mouse_position();
        let y = f32::from(mouse.y);
        let over = if y < geo.top {
            y - geo.top
        } else if y > geo.bottom {
            y - geo.bottom
        } else {
            return;
        };
        let before = self.scroll_y;
        self.scroll_y += (over * 0.35).clamp(-48., 48.);
        self.clamp_scroll();
        if self.scroll_y != before {
            selection.head = self.hit(mouse, Some(selection.side), window, cx).0;
            self.selection = Some(selection);
            window.request_animation_frame();
        }
    }
}

/// The word around a byte offset, or the single character there when it is
/// not part of one.
fn word_at(text: &str, col: usize) -> (usize, usize) {
    let is_word = |ch: char| ch.is_alphanumeric() || ch == '_';
    let col = col.min(text.len());
    let mut start = col;
    for (index, ch) in text[..col].char_indices().rev() {
        if !is_word(ch) {
            break;
        }
        start = index;
    }
    let mut end = col;
    for (index, ch) in text[col..].char_indices() {
        if !is_word(ch) {
            break;
        }
        end = col + index + ch.len_utf8();
    }
    if start == end {
        if let Some(ch) = text[col..].chars().next() {
            end = col + ch.len_utf8();
        }
    }
    (start, end)
}

/// A one-style line of chrome text. GPUI's frame cache keeps these cheap:
/// the same label shaped on consecutive frames is a lookup.
fn label(
    text: impl Into<SharedString>,
    font: &Font,
    size: f32,
    color: Hsla,
    window: &Window,
) -> ShapedLine {
    let text = text.into();
    let run = TextRun {
        len: text.len(),
        font: font.clone(),
        color,
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    window
        .text_system()
        .shape_line(text, px(size), &[run], None)
}

/// A code line shaped with its syntax colors, from the cache when the line
/// was on screen recently.
fn shape_code(
    shaped: &mut HashMap<LineId, ShapedLine>,
    lines: &[Line],
    id: LineId,
    metrics: &Metrics,
    theme: &Theme,
    window: &Window,
) -> ShapedLine {
    if let Some(line) = shaped.get(&id) {
        return line.clone();
    }
    let line = &lines[id as usize];
    let len = line.text.len();
    let run = |len: usize, class: Option<Syn>| {
        let mut font = metrics.code.clone();
        match class {
            Some(Syn::Comment | Syn::Em) => font.style = FontStyle::Italic,
            Some(Syn::Heading | Syn::Strong) => font.weight = FontWeight::SEMIBOLD,
            _ => {}
        }
        TextRun {
            len,
            font,
            color: class
                .and_then(|class| theme.syn(class))
                .unwrap_or(theme.text1),
            background_color: None,
            underline: None,
            strikethrough: None,
        }
    };
    let mut runs = Vec::with_capacity(line.marks.len() * 2 + 1);
    let mut at = 0;
    for mark in &line.marks {
        let (start, end) = (
            mark.range.start as usize,
            (mark.range.end as usize).min(len),
        );
        if start < at || end <= start {
            continue;
        }
        if start > at {
            runs.push(run(start - at, None));
        }
        runs.push(run(end - start, Some(mark.class)));
        at = end;
    }
    if at < len {
        runs.push(run(len - at, None));
    }
    let line = window
        .text_system()
        .shape_line(line.text.clone(), px(CODE_SIZE), &runs, None);
    shaped.insert(id, line.clone());
    line
}

/// One row's gutter rail and code fill. Changed lines speak through three
/// redundant channels — fill, tinted rail, sign.
struct Rail {
    x: f32,
    code: f32,
    end: f32,
    y: f32,
}

impl Rail {
    fn fill(&self, kind: Kind, theme: &Theme, window: &mut Window) {
        let (rail, tint) = match kind {
            Kind::Context => (theme.gutter_surface, None),
            Kind::Added => (theme.add_gutter, Some(theme.add_bg)),
            Kind::Removed => (theme.del_gutter, Some(theme.del_bg)),
        };
        window.paint_quad(fill(rect(self.x, self.y, self.code, self.y + LINE_H), rail));
        if let Some(tint) = tint {
            window.paint_quad(fill(
                rect(self.code, self.y, self.end, self.y + LINE_H),
                tint,
            ));
        }
    }
}

/// A line number, right-aligned to `right_edge` less the rail's one column
/// of padding.
#[allow(clippy::too_many_arguments)]
fn paint_number(
    numbers: &mut HashMap<(u32, u8), ShapedLine>,
    no: u32,
    kind: Kind,
    right_edge: f32,
    y: f32,
    metrics: &Metrics,
    theme: &Theme,
    window: &mut Window,
    cx: &mut App,
) {
    let (tone, color) = match kind {
        Kind::Context => (0, theme.text3),
        Kind::Added => (1, theme.add_fg),
        Kind::Removed => (2, theme.del_fg),
    };
    let line = numbers
        .entry((no, tone))
        .or_insert_with(|| label(no.to_string(), &metrics.code, XS, color, window));
    let x = right_edge - metrics.ch_xs - f32::from(line.width);
    line.paint(point(px(x), px(y)), px(LINE_H), window, cx).ok();
}

fn paint_sign(
    kind: Kind,
    x: f32,
    y: f32,
    geo: &Geo,
    metrics: &Metrics,
    window: &mut Window,
    cx: &mut App,
) {
    let sign = match kind {
        Kind::Context => return,
        Kind::Added => &metrics.plus,
        Kind::Removed => &metrics.minus,
    };
    let x = x + (geo.signw - f32::from(sign.width)) / 2.;
    sign.paint(point(px(x), px(y)), px(LINE_H), window, cx).ok();
}

/// How wide a header's Resolve button is, and where it sits.
fn resolve_rect(label: &ShapedLine, geo: &Geo, y: f32) -> Bounds<Pixels> {
    let width = f32::from(label.width) + 16.;
    let right = geo.right - THUMB_ZONE - 4.;
    rect(right - width, y + 4., right, y + 4. + 22.)
}

fn resolve_label(offer: &ResolveOffer, file: &FileModel) -> String {
    if offer.waiting.as_deref() == Some(file.path.as_ref()) {
        format!("Waiting for {}…", offer.tool)
    } else {
        format!("Resolve in {}", offer.tool)
    }
}

/// One file header: the collapse chevron, status letter, path, where a
/// rename came from, the conflict chip, the stats and — on a conflicted
/// file — the Resolve button.
struct HeaderPaint<'a> {
    file: &'a FileModel,
    index: usize,
    y: f32,
    collapsed: bool,
    /// The pointer is over this header; `true` when on its Resolve button.
    hover: Option<bool>,
    /// The find query matched this file's path; `true` for the current
    /// match.
    path_hit: Option<bool>,
    resolve: Option<&'a ResolveOffer>,
}

impl HeaderPaint<'_> {
    fn paint(
        &self,
        geo: &Geo,
        metrics: &Metrics,
        theme: &Theme,
        window: &mut Window,
        cx: &mut App,
    ) {
        let (file, y) = (self.file, self.y);
        let height = HEADER_H - 1.;
        let fill_color = if self.hover.is_some() {
            Theme::blend(theme.text1, 0.03, theme.file_head)
        } else {
            theme.file_head
        };
        window.paint_quad(fill(rect(geo.left, y, geo.right, y + height), fill_color));
        window.paint_quad(fill(
            rect(geo.left, y + height, geo.right, y + HEADER_H),
            theme.border2,
        ));
        if self.index > 0 {
            window.paint_quad(fill(rect(geo.left, y, geo.right, y + 1.), theme.border1));
        }
        let at = |x: f32| point(px(x), px(y));

        // The chevron: the whole left run of the header is the collapse
        // toggle.
        let mut x = geo.left + 10.;
        let chevron = if self.collapsed {
            IconName::ChevronRight
        } else {
            IconName::ChevronDown
        };
        let _ = window.paint_svg(
            rect(x, y + (height - 12.) / 2., x + 12., y + (height + 12.) / 2.),
            chevron.path(),
            TransformationMatrix::unit(),
            if self.hover.is_some() {
                theme.text2
            } else {
                theme.text3
            },
            cx,
        );
        x += 12. + 6.;

        // A/M/D/R in the status's hue — a letter, not a tile.
        let (letter, hue) = match file.status {
            FileStatus::Added => ("A", theme.add_fg),
            FileStatus::Modified => ("M", theme.warn),
            FileStatus::Removed => ("D", theme.del_fg),
            FileStatus::Renamed => ("R", theme.accent_strong),
            FileStatus::Copied => ("C", theme.accent_strong),
        };
        let bold = Font {
            weight: FontWeight::SEMIBOLD,
            ..metrics.code.clone()
        };
        label(letter, &bold, XS, hue, window)
            .paint(at(x), px(height), window, cx)
            .ok();
        x += 12. + 8.;

        // Directory quiet, file name carrying the weight.
        let gone = file.status == FileStatus::Removed;
        let run = |len: usize, color: Hsla, weight: FontWeight, strike: bool| TextRun {
            len,
            font: Font {
                weight,
                ..metrics.code.clone()
            },
            color,
            background_color: None,
            underline: None,
            strikethrough: strike.then_some(StrikethroughStyle {
                thickness: px(1.),
                color: Some(color),
            }),
        };
        let name_color = if gone { theme.text2 } else { theme.text1 };
        let runs: Vec<TextRun> = [
            run(file.dir.len(), theme.text3, FontWeight::NORMAL, false),
            run(file.name.len(), name_color, FontWeight::MEDIUM, gone),
        ]
        .into_iter()
        .filter(|run| run.len > 0)
        .collect();
        let path = window
            .text_system()
            .shape_line(file.path.clone(), px(SMALL), &runs, None);
        if let Some(current) = self.path_hit {
            // The header when the file's path matched the query; stronger
            // while it is the current match.
            let name_x = x + f32::from(path.x_for_index(file.dir.len()));
            window.paint_quad(quad(
                rect(
                    name_x - 2.,
                    y + 6.,
                    x + f32::from(path.width) + 2.,
                    y + height - 6.,
                ),
                px(3.),
                Theme::fade(theme.warn, if current { 0.55 } else { 0.28 }),
                px(0.),
                gpui::transparent_black(),
                BorderStyle::default(),
            ));
        }
        path.paint(at(x), px(height), window, cx).ok();
        x += f32::from(path.width) + 8.;

        if let Some(from) = &file.renamed_from {
            let from = label(format!("← {from}"), &metrics.code, XS, theme.text3, window);
            from.paint(at(x), px(height), window, cx).ok();
            x += f32::from(from.width) + 8.;
        }
        if file.has_conflict {
            let medium = Font {
                weight: FontWeight::MEDIUM,
                ..metrics.ui.clone()
            };
            let chip = if file.regions > 1 {
                format!("{} conflicts", file.regions)
            } else {
                "conflict".to_owned()
            };
            label(chip, &medium, XS, theme.danger, window)
                .paint(at(x), px(height), window, cx)
                .ok();
        }

        // The Resolve button, then +N −M to its left — all clear of the
        // scroll thumb.
        let mut right = geo.right - THUMB_ZONE - 4.;
        if let Some(offer) = self.resolve {
            let waiting = offer.waiting.as_deref() == Some(file.path.as_ref());
            let hot = waiting || (self.hover == Some(true) && !offer.disabled);
            let medium = Font {
                weight: FontWeight::MEDIUM,
                ..metrics.ui.clone()
            };
            let mut color = if hot {
                theme.accent_strong
            } else {
                theme.text1
            };
            if offer.disabled && !waiting {
                color = Theme::fade(color, 0.55);
            }
            let text = label(resolve_label(offer, file), &medium, XS, color, window);
            let button = resolve_rect(&text, geo, y);
            window.paint_quad(quad(
                button,
                px(4.),
                if hot { theme.accent_dim } else { theme.bg3 },
                px(1.),
                if hot {
                    Theme::fade(theme.accent, 0.4)
                } else {
                    theme.border1
                },
                BorderStyle::default(),
            ));
            text.paint(
                point(button.left() + px(8.), button.top()),
                px(22.),
                window,
                cx,
            )
            .ok();
            right = f32::from(button.left()) - 10.;
        }
        for (count, sign, color) in [
            (file.removed, "−", theme.del_fg),
            (file.added, "+", theme.add_fg),
        ] {
            if count == 0 {
                continue;
            }
            let stat = label(format!("{sign}{count}"), &metrics.code, XS, color, window);
            right -= f32::from(stat.width);
            stat.paint(at(right), px(height), window, cx).ok();
            right -= 6.;
        }
    }
}

/// Every occurrence of `query` in the document, in reading order.
/// Case-insensitive literal; offsets index the line as displayed.
pub fn find_in_doc(doc: &DiffDoc, query: &str) -> Vec<FindMatch> {
    let mut matches = Vec::new();
    if query.is_empty() {
        return matches;
    }
    let needle: Vec<char> = query.chars().flat_map(char::to_lowercase).collect();
    // Byte ranges of every match in `text`, compared char by char so a
    // case fold that changes a character's length cannot drift offsets.
    let occurrences = |text: &str| -> Vec<Range<u32>> {
        let chars: Vec<(usize, char)> = text.char_indices().collect();
        let mut found = Vec::new();
        let mut start = 0;
        while start < chars.len() {
            let mut at = start;
            let mut matched = 0;
            while matched < needle.len() && at < chars.len() {
                let mut lower = chars[at].1.to_lowercase();
                let first = lower.next();
                if first != Some(needle[matched]) {
                    break;
                }
                matched += 1;
                // A character whose lowercase is several characters must
                // match them all.
                let mut whole = true;
                for extra in lower {
                    if needle.get(matched) == Some(&extra) {
                        matched += 1;
                    } else {
                        whole = false;
                        break;
                    }
                }
                if !whole {
                    matched = usize::MAX;
                    break;
                }
                at += 1;
            }
            if matched == needle.len() {
                let end = chars.get(at).map_or(text.len(), |(byte, _)| *byte);
                found.push(chars[start].0 as u32..end as u32);
                start = at.max(start + 1);
            } else {
                start += 1;
            }
        }
        found
    };
    for (index, file) in doc.files.iter().enumerate() {
        let renamed = file
            .renamed_from
            .as_ref()
            .is_some_and(|from| !occurrences(from).is_empty());
        if renamed || !occurrences(&file.path).is_empty() {
            matches.push(FindMatch {
                file: index,
                line: None,
            });
        }
        for id in file.lines.clone() {
            for range in occurrences(&doc.lines[id as usize].text) {
                matches.push(FindMatch {
                    file: index,
                    line: Some((id, range)),
                });
            }
        }
    }
    matches
}

/// The element that hands the viewport to `DiffSurface::paint` and routes
/// window mouse events back to the view.
struct DiffElement {
    view: Entity<DiffSurface>,
}

impl IntoElement for DiffElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for DiffElement {
    type RequestLayoutState = ();
    /// The whole surface, and the strip at its right edge that belongs to
    /// the scroll thumb.
    type PrepaintState = (Hitbox, Hitbox);

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let mut style = Style {
            flex_grow: 1.,
            flex_shrink: 1.,
            flex_basis: px(0.).into(),
            ..Style::default()
        };
        style.min_size.height = px(0.).into();
        style.size.width = relative(1.).into();
        style.size.height = relative(1.).into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        _cx: &mut App,
    ) -> Self::PrepaintState {
        let surface = window.insert_hitbox(bounds, HitboxBehavior::Normal);
        let mut strip = bounds;
        strip.origin.x = bounds.right() - px(THUMB_ZONE);
        strip.size.width = px(THUMB_ZONE);
        let strip = window.insert_hitbox(strip, HitboxBehavior::Normal);
        (surface, strip)
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        (surface, strip): &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        self.view
            .update(cx, |view, cx| view.paint(bounds, window, cx));

        // The later request wins where both are hovered: text cursor over
        // the code, arrow over headers and the thumb strip.
        let over_header = self.view.read(cx).header_hover.is_some();
        window.set_cursor_style(
            if over_header {
                CursorStyle::PointingHand
            } else {
                CursorStyle::IBeam
            },
            surface,
        );
        window.set_cursor_style(CursorStyle::Arrow, strip);

        let view = self.view.clone();
        let hitbox = surface.clone();
        window.on_mouse_event(move |event: &ScrollWheelEvent, phase, window, cx| {
            if phase == DispatchPhase::Bubble && hitbox.should_handle_scroll(window) {
                view.update(cx, |view, cx| view.on_scroll(event, cx));
            }
        });
        let view = self.view.clone();
        let hitbox = surface.clone();
        window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
            if phase != DispatchPhase::Bubble || !hitbox.is_hovered(window) {
                return;
            }
            match event.button {
                MouseButton::Left => {
                    view.update(cx, |view, cx| view.on_mouse_down(event, window, cx))
                }
                MouseButton::Right => {
                    view.update(cx, |view, cx| view.on_context(event, window, cx))
                }
                _ => {}
            }
        });
        // Moves and releases are taken window-wide so a drag keeps working
        // after it leaves the surface.
        let view = self.view.clone();
        let hitbox = surface.clone();
        window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
            if phase == DispatchPhase::Bubble {
                let inside = hitbox.is_hovered(window);
                view.update(cx, |view, cx| view.on_mouse_move(event, inside, window, cx));
            }
        });
        let view = self.view.clone();
        window.on_mouse_event(move |event: &MouseUpEvent, phase, _window, cx| {
            if phase == DispatchPhase::Bubble && event.button == MouseButton::Left {
                view.update(cx, |view, cx| view.on_mouse_up(event, cx));
            }
        });
    }
}

impl Render for DiffSurface {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        gpui::div()
            .id("diff-surface")
            .key_context("DiffSurface")
            .track_focus(&self.focus)
            .size_full()
            .flex()
            .flex_col()
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::select_all))
            .on_action(cx.listener(Self::page_down))
            .on_action(cx.listener(Self::page_up))
            .on_action(cx.listener(Self::scroll_top))
            .on_action(cx.listener(Self::scroll_bottom))
            .on_action(cx.listener(Self::next_file))
            .on_action(cx.listener(Self::prev_file))
            .child(DiffElement { view: cx.entity() })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn word_at_takes_the_identifier_under_the_cursor() {
        let text = "let snake_case = foo(bar);";
        assert_eq!(word_at(text, 6), (4, 14));
        assert_eq!(word_at(text, 4), (4, 14));
        // At a word's trailing edge, still that word.
        assert_eq!(word_at(text, 20), (17, 20));
        // On punctuation, the single character.
        assert_eq!(word_at(text, 15), (15, 16));
        // Past the end, nothing.
        assert_eq!(word_at(text, text.len()), (text.len(), text.len()));
    }

    #[test]
    fn selection_orders_its_ends() {
        let selection = Selection {
            anchor: Pos { row: 4, col: 2 },
            head: Pos { row: 1, col: 9 },
            side: Side::Right,
        };
        assert_eq!(
            selection.ordered(),
            (Pos { row: 1, col: 9 }, Pos { row: 4, col: 2 })
        );
    }
}
