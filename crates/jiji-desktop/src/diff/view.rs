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
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{
    actions, fill, font, point, prelude::*, px, quad, relative, AnyTooltip, AnyView, App,
    BorderStyle, Bounds, ClipboardItem, ContentMask, Context, CursorStyle, DispatchPhase,
    ElementId, Entity, EventEmitter, FocusHandle, Focusable, Font, FontStyle, FontWeight,
    GlobalElementId, Hitbox, HitboxBehavior, Hsla, InspectorElementId, KeyBinding, LayoutId,
    MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Point, ScrollEvent,
    ShapedLine, SharedString, StrikethroughStyle, Style, Task, TextAlign, TextRun,
    TransformationMatrix, Window,
};
use jiji_core::snapshot::FileStatus;

use crate::diff::highlight::Syn;
use crate::diff::model::{
    DiffDoc, FileModel, Kind, Layout, LineId, Prepared, Row, GAP_H, HEADER_H, IMAGE_MAX_H, LINE_H,
    NOTE_H, REGION_H, SIDE_H,
};
use crate::diff::picture::{frame_at, PictureLoader};
use crate::logic::text::{diffstat_blocks, file_title, Block};
use crate::theme::{theme, Theme};
use crate::ui::input::{Copy, SelectAll};
use crate::ui::{AxisLock, IconName};

actions!(
    diff,
    [
        PageDown,
        PageUp,
        ScrollTop,
        ScrollBottom,
        NextFile,
        PrevFile,
    ]
);

/// Keys that belong to the diff while it has focus. Copy and Select All
/// are the text fields' own actions, so Edit ▸ Copy and Edit ▸ Select All
/// in the menu bar reach a focused diff as they reach a field.
pub fn bind_keys(cx: &mut App) {
    let diff = Some("DiffSurface");
    cx.bind_keys([
        KeyBinding::new("cmd-c", Copy, diff),
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
/// Depth of the band along a file's bottom edge where its sideways thumb
/// takes the pointer.
const THUMB_BAND: f32 = 12.;
const MIN_THUMB: f32 = 28.;
/// How far inside a code column's edges a selection drag starts pulling
/// the lines along: the belt WebKit keeps.
const PULL_BELT: f32 = 20.;
/// Pull speed per point of overshoot, and its ceiling, per second.
const PULL_RATE: f32 = 21.;
const PULL_MAX: f32 = 2880.;

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

impl DiffTarget {
    /// The line under the pointer, by its old and new numbers; `None` on a
    /// header or between lines.
    pub fn line(&self) -> Option<(Option<u32>, Option<u32>)> {
        (!self.on_header && (self.old_no.is_some() || self.new_no.is_some()))
            .then_some((self.old_no, self.new_no))
    }
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
    /// Dragging a file's sideways thumb.
    Pan(PanGrab),
}

/// A hold on a sideways thumb: whose it is, the track it runs on, and
/// where on the thumb it was grabbed.
#[derive(Clone, Copy, PartialEq)]
struct PanGrab {
    file: usize,
    track: (f32, f32),
    grab: f32,
}

/// A file's sideways scroll bar: one thumb per code column, all riding
/// the file's one offset.
#[derive(Clone, Copy)]
struct PanBar {
    file: usize,
    /// The edge the thumbs rest on: the viewport's bottom while the file
    /// passes, the file's own end when that comes first.
    seat: f32,
    pan: f32,
    max_pan: f32,
    /// Each code column's left and right edge.
    tracks: [(f32, f32); 2],
    columns: usize,
}

impl PanBar {
    fn tracks(&self) -> &[(f32, f32)] {
        &self.tracks[..self.columns]
    }

    /// A column's thumb: its left edge and width.
    fn thumb(&self, (x0, x1): (f32, f32)) -> (f32, f32) {
        let span = x1 - x0;
        let width = (span * span / (span + self.max_pan))
            .max(MIN_THUMB)
            .min(span);
        (x0 + self.pan / self.max_pan * (span - width), width)
    }

    /// The offset that puts a column's thumb with its left edge at `left`.
    fn pan_at(&self, track: (f32, f32), left: f32) -> f32 {
        let (_, width) = self.thumb(track);
        let travel = (track.1 - track.0 - width).max(1.);
        (left - track.0) / travel * self.max_pan
    }

    /// The thumb under a point, held where the point is.
    fn grab(&self, x: f32, y: f32) -> Option<PanGrab> {
        if y < self.seat - THUMB_BAND || y > self.seat {
            return None;
        }
        self.tracks().iter().find_map(|&track| {
            let (left, width) = self.thumb(track);
            (x >= left && x <= left + width).then_some(PanGrab {
                file: self.file,
                track,
                grab: x - left,
            })
        })
    }
}

/// A tooltip in waiting over a file header. Once the pointer has rested
/// there a tooltip's delay, a header that could not show its path whole
/// names it.
struct HeaderTip {
    file: usize,
    due: bool,
    /// The tooltip, and where the pointer was when it went up: where it
    /// stays.
    shown: Option<(AnyView, Point<Pixels>)>,
    _wait: Task<()>,
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
    doc: Arc<DiffDoc>,
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
    tip: Option<HeaderTip>,
    /// The file under the pointer: the one whose sideways thumb shows.
    pointer_file: Option<usize>,
    /// The pointer is on that thumb.
    pan_hover: bool,
    wheel: AxisLock,
    /// When a selection drag last pulled the view along, for its pace.
    pulled_at: Option<Instant>,
    metrics: Option<Metrics>,
    /// The palette the cached shapes were colored for.
    shaped_for: &'static str,
    geo: Option<Geo>,
    /// Shaped code lines near the viewport, kept across frames so a steady
    /// frame shapes nothing.
    shaped: HashMap<LineId, ShapedLine>,
    numbers: HashMap<(u32, u8), ShapedLine>,
    find: Vec<FindMatch>,
    find_current: usize,
    resolve: Option<ResolveOffer>,
    active_file: usize,
    /// Dim the content: what is shown is being replaced.
    stale: bool,
    /// When this diff went on screen: where its animations' loops start.
    shown_at: Instant,
    /// The wake for the next frame of an animated picture on screen.
    animating: Option<Task<()>>,
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
            doc: Arc::new(DiffDoc::empty()),
            folded: None,
            collapsed: Vec::new(),
            // A stop of its own: Space, the page keys and ⌘C read it.
            focus: cx.focus_handle().tab_stop(true),
            split,
            scroll_y: 0.,
            pan: Vec::new(),
            selection: None,
            drag: Drag::None,
            thumb_hover: false,
            header_hover: None,
            tip: None,
            pointer_file: None,
            pan_hover: false,
            wheel: AxisLock::default(),
            pulled_at: None,
            metrics: None,
            shaped_for: "",
            geo: None,
            shaped: HashMap::new(),
            numbers: HashMap::new(),
            find: Vec::new(),
            find_current: 0,
            resolve: None,
            active_file: 0,
            stale: false,
            shown_at: Instant::now(),
            animating: None,
        }
    }

    /// Put another diff on the surface. `keep_position` is for a refetch
    /// of the same comparison (an auto-refresh): the reader stays where
    /// they were, with the same files collapsed.
    pub fn set_doc(&mut self, prepared: &Prepared, keep_position: bool, cx: &mut Context<Self>) {
        let doc = prepared.doc().clone();
        // Highlighting runs off the main thread, a file at a time; a diff
        // shown before brings its colours with it. Each file that colours
        // while this one is on screen repaints it.
        for file in prepared.highlighting(cx.background_executor()) {
            if file.peek().is_some() {
                continue;
            }
            let (file, colored) = (file.clone(), doc.clone());
            cx.spawn(async move |this, cx| {
                file.await;
                let _ = this.update(cx, |this, cx| {
                    if Arc::ptr_eq(&this.doc, &colored) {
                        this.shaped.clear();
                        cx.notify();
                    }
                });
            })
            .detach();
        }
        let files = doc.files.len();
        if keep_position && self.collapsed.len() == files {
            // Same comparison: keep which files are folded and how far
            // each is panned, by path.
            let kept: HashMap<&str, (bool, f32)> = self
                .doc
                .files
                .iter()
                .zip(self.collapsed.iter().zip(&self.pan))
                .map(|(file, (collapsed, pan))| (file.path.as_ref(), (*collapsed, *pan)))
                .collect();
            (self.collapsed, self.pan) = doc
                .files
                .iter()
                .map(|file| kept.get(file.path.as_ref()).copied().unwrap_or_default())
                .unzip();
        } else {
            self.collapsed = vec![false; files];
            self.pan = vec![0.; files];
            self.scroll_y = 0.;
            self.active_file = 0;
            self.shown_at = Instant::now();
        }
        self.doc = doc;
        self.selection = None;
        self.drag = Drag::None;
        self.tip = None;
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
        if let Some((line, range)) = found.line {
            self.reveal_columns(found.file, line, range);
        }
        self.scroll_to(y, cx);
    }

    /// Pan a file until a stretch of one of its lines is in view, a third
    /// of the way across the column. A stretch already in view stays put.
    fn reveal_columns(&mut self, file: usize, line: LineId, range: Range<u32>) {
        let (Some(geo), Some(metrics)) = (self.geo, self.metrics.as_ref()) else {
            return;
        };
        let Some(text) = self.doc.lines.get(line as usize).map(|line| &line.text) else {
            return;
        };
        // The code face is fixed-pitch: a column is a character.
        let x_of = |byte: u32| {
            let cols = text
                .get(..byte as usize)
                .map_or(0, |head| head.chars().count());
            CODE_PAD_L + cols as f32 * metrics.ch
        };
        let (from, to) = (x_of(range.start), x_of(range.end));
        let visible = self.code_width(&geo) - THUMB_ZONE;
        let pan = self.pan_of(file);
        if from < pan + CODE_PAD_L || to > pan + visible - CODE_PAD_R {
            self.set_pan(file, from - visible / 3.);
        }
    }

    /// The single-line text selection, if any: what ⌘F adopts as the
    /// query.
    pub fn selection_on_one_line(&self) -> Option<String> {
        self.selected_text().filter(|text| !text.contains('\n'))
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
        (content - self.code_width(&geo)).max(0.)
    }

    /// The width a file's lines pan within.
    fn code_width(&self, geo: &Geo) -> f32 {
        if self.split {
            geo.mid - geo.left_code
        } else {
            geo.right - geo.full_code
        }
    }

    /// The code column a side's lines sit in: its left and right edge,
    /// short of the vertical thumb's strip.
    fn column(&self, geo: &Geo, side: Side) -> (f32, f32) {
        match (self.split, side) {
            (true, Side::Left) => (geo.left_code, geo.mid),
            (true, Side::Right) => (geo.right_code, geo.right - THUMB_ZONE),
            (false, _) => (geo.full_code, geo.right - THUMB_ZONE),
        }
    }

    /// A file's sideways scroll bar, when its lines outrun the column and
    /// there is room under its header to seat one.
    fn pan_bar(&self, file: usize) -> Option<PanBar> {
        let geo = self.geo?;
        let layout = self.layout();
        let max_pan = self.max_pan(file);
        if max_pan <= 0. || file >= layout.file_rows.len() {
            return None;
        }
        let seat = (geo.top + layout.file_bottom(file) - self.scroll_y).min(geo.bottom);
        let body_top = (geo.top + layout.file_top(file) - self.scroll_y).max(geo.top) + HEADER_H;
        if seat - body_top < 2. * LINE_H {
            return None;
        }
        let right = self.column(&geo, Side::Right);
        let (tracks, columns) = if self.split {
            ([self.column(&geo, Side::Left), right], 2)
        } else {
            ([right; 2], 1)
        };
        Some(PanBar {
            file,
            seat,
            pan: self.pan_of(file),
            max_pan,
            tracks,
            columns,
        })
    }

    /// The bar on show: the file's under the pointer, or the one a drag
    /// holds wherever the pointer has gone since.
    fn shown_pan_bar(&self) -> Option<PanBar> {
        let file = match self.drag {
            Drag::Pan(grab) => Some(grab.file),
            _ => self.pointer_file,
        };
        self.pan_bar(file?)
    }

    /// The sideways thumb under a point, held where the point is. The
    /// thumb sits in its own file's rows, so the point names the file.
    fn pan_grab(&self, position: Point<Pixels>) -> Option<PanGrab> {
        let (x, y) = (f32::from(position.x), f32::from(position.y));
        self.pan_bar(self.file_at(y)?)?.grab(x, y)
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

    fn copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
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

    fn on_scroll(&mut self, event: &ScrollEvent, cx: &mut Context<Self>) {
        self.drop_tip(cx);
        let (dx, dy) = self.wheel.travel(event, LINE_H * 2.5);
        // Sideways travel pans the file under the pointer.
        let file = self.geo.map_or(0, |geo| {
            let y = f32::from(event.position.y) - geo.top + self.scroll_y;
            self.layout().file_of(self.layout().row_at(y))
        });
        let before = (self.scroll_y, self.pan_of(file));
        self.scroll_y -= dy;
        self.clamp_scroll();
        self.set_pan(file, before.1 - dx);
        // The rows moved under a still pointer: it may be over another
        // file now.
        let pointer_file = self.file_at(f32::from(event.position.y));
        if before != (self.scroll_y, self.pan_of(file)) || pointer_file != self.pointer_file {
            self.pointer_file = pointer_file;
            self.pan_hover = self.drag == Drag::None && self.pan_grab(event.position).is_some();
            cx.notify();
        }
    }

    /// The file whose rows sit at window height `y`.
    fn file_at(&self, y: f32) -> Option<usize> {
        let geo = self.geo?;
        let layout = self.layout();
        if layout.file_rows.is_empty() || y < geo.top || y > geo.bottom {
            return None;
        }
        Some(layout.file_of(layout.row_at(y - geo.top + self.scroll_y)))
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
        window.focus(&self.focus, cx);
        self.drop_tip(cx);
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
        // The sideways thumb lies over the code, so only the thumb itself
        // takes the press; the rest of its band still selects text.
        if let Some(grab) = self.pan_grab(event.position) {
            self.drag = Drag::Pan(grab);
            cx.notify();
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
        window.focus(&self.focus, cx);
        self.drop_tip(cx);
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
        let resting = header.map(|(file, _)| file);
        if resting != self.tip.as_ref().map(|tip| tip.file) {
            self.drop_tip(cx);
            self.tip = resting.map(|file| HeaderTip {
                file,
                due: false,
                shown: None,
                _wait: cx.spawn(async move |this, cx| {
                    cx.background_executor()
                        .timer(crate::ui::tooltip::SHOW_DELAY)
                        .await;
                    let _ = this.update(cx, |this, cx| {
                        if let Some(tip) = &mut this.tip {
                            tip.due = true;
                            cx.notify();
                        }
                    });
                }),
            });
        }
        let pointer_file = inside
            .then(|| self.file_at(f32::from(event.position.y)))
            .flatten();
        if pointer_file != self.pointer_file {
            self.pointer_file = pointer_file;
            cx.notify();
        }
        match self.drag {
            Drag::Select if held => {
                if let Some(mut selection) = self.selection {
                    let head = self.hit(event.position, Some(selection.side), window, cx).0;
                    let moved = head != selection.head;
                    selection.head = head;
                    self.selection = Some(selection);
                    // A pointer held at an edge moves no head, yet the
                    // next frame has rows or columns to pull in under it.
                    if moved || self.pull(event.position, &selection) != (0., 0.) {
                        cx.notify();
                    }
                }
            }
            Drag::Thumb(grab) if held => {
                self.drag_thumb(f32::from(event.position.y), grab);
                cx.notify();
            }
            Drag::Pan(grab) if held => {
                if let Some(bar) = self.pan_bar(grab.file) {
                    let left = f32::from(event.position.x) - grab.grab;
                    self.set_pan(grab.file, bar.pan_at(grab.track, left));
                    cx.notify();
                }
            }
            Drag::None => {
                let hover = self.geo.is_some_and(|geo| {
                    let (x, y) = (f32::from(event.position.x), f32::from(event.position.y));
                    x >= geo.right - THUMB_ZONE && x <= geo.right && y >= geo.top && y <= geo.bottom
                });
                let pan_hover = inside && self.pan_grab(event.position).is_some();
                if hover != self.thumb_hover || pan_hover != self.pan_hover {
                    self.thumb_hover = hover;
                    self.pan_hover = pan_hover;
                    cx.notify();
                }
            }
            // The button came up outside the window.
            _ => self.drag = Drag::None,
        }
    }

    fn on_mouse_up(&mut self, event: &MouseUpEvent, cx: &mut Context<Self>) {
        if self.drag != Drag::None {
            self.drag = Drag::None;
            self.pulled_at = None;
            self.pan_hover = self.pan_grab(event.position).is_some();
            cx.notify();
        }
    }

    /// Take down a header's tooltip, shown or waiting.
    fn drop_tip(&mut self, cx: &mut Context<Self>) {
        if self.tip.take().is_some_and(|tip| tip.shown.is_some()) {
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
                let line = shape_code(&mut self.shaped, &self.doc, id, metrics, &theme, window);
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
        // Read where the pointer is now rather than where it last moved:
        // the rows shift under a still pointer when a tray opens above.
        let mouse = window.mouse_position();
        self.pointer_file = bounds
            .contains(&mouse)
            .then(|| self.file_at(f32::from(mouse.y)))
            .flatten();
        let pan_bar = self.shown_pan_bar();
        let pan_hot = self.pan_hover || matches!(self.drag, Drag::Pan(_));
        let stale = self.stale;
        // How far into their loops animated pictures are; `next_frame` is
        // how soon the first of those on screen moves on.
        let playing = self.shown_at.elapsed();
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

        // The visible row range, and the pan offset of each file it
        // touches.
        let (first, last, first_file, pans) = {
            let layout = self.layout();
            let first = layout.row_at(scroll);
            let mut last = first;
            while last < layout.rows.len() && layout.tops[last] < scroll + geo.height() {
                last += 1;
            }
            let first_file = layout.file_of(first);
            let last_file = layout.file_of(last.saturating_sub(1)).max(first_file);
            let pans: Vec<f32> = (first_file..=last_file)
                .map(|file| (self.pan_of(file) * scale).round() / scale)
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

        // Headers that could not show their path whole this frame.
        let mut clipped = Vec::new();
        let mut next_frame: Option<Duration> = None;

        // Pass 1 — everything but code text: fills, gutter rails, numbers,
        // signs, seams and file headers.
        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            for row in first..last {
                let y = row_y(row);
                match &layout.rows[row] {
                    Row::Pad => {}
                    Row::FileHeader { file } => {
                        let file = *file as usize;
                        if header(file, y).paint(&geo, metrics, theme, window, cx) {
                            clipped.push(file);
                        }
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
                        name.paint(
                            point(px(x), px(y)),
                            px(height),
                            TextAlign::Left,
                            None,
                            window,
                            cx,
                        )
                        .ok();
                        if let Some(caption) = side {
                            label(caption.clone(), &metrics.ui, XS, theme.text3, window)
                                .paint(
                                    point(px(x + f32::from(name.width) + 10.), px(y)),
                                    px(height),
                                    TextAlign::Left,
                                    None,
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
                        let code = if split { geo.left_code } else { geo.full_code };
                        // The fold mark, centered in the number gutter:
                        // lines were left out here.
                        let fold = label("⋯", &metrics.ui, SMALL, theme.text3, window);
                        let fold_x = geo.left + (code - geo.left - f32::from(fold.width)) / 2.;
                        fold.paint(
                            point(px(fold_x), px(y)),
                            px(GAP_H),
                            TextAlign::Left,
                            None,
                            window,
                            cx,
                        )
                        .ok();
                        let mut x = code + CODE_PAD_L;
                        if let Some(context) = context {
                            let symbol: SharedString = match context.char_indices().nth(64) {
                                Some((cut, _)) => format!("{}…", &context[..cut]).into(),
                                None => context.clone(),
                            };
                            let symbol = label(symbol, &metrics.code, XS, theme.text2, window);
                            symbol
                                .paint(
                                    point(px(x), px(y)),
                                    px(GAP_H),
                                    TextAlign::Left,
                                    None,
                                    window,
                                    cx,
                                )
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
                        count
                            .paint(
                                point(px(x), px(y)),
                                px(GAP_H),
                                TextAlign::Left,
                                None,
                                window,
                                cx,
                            )
                            .ok();
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
                            // Decoded the first time it is drawn; `None`
                            // until then.
                            let decoded =
                                side.picture
                                    .as_ref()
                                    .map_err(|why| *why)
                                    .and_then(|picture| {
                                        window.use_asset::<PictureLoader>(picture, cx).transpose()
                                    });
                            match decoded {
                                Ok(Some(decoded)) => {
                                    let natural = decoded.image.size(0);
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
                                    let area = rect(left, top, left + w, top + h);
                                    if !decoded.opaque {
                                        paint_checker(area, theme, window);
                                    }
                                    let (frame, holds) = frame_at(&decoded.image, playing);
                                    if let Some(holds) = holds {
                                        next_frame =
                                            Some(next_frame.map_or(holds, |next| holds.min(next)));
                                    }
                                    let _ = window.paint_image(
                                        area,
                                        area,
                                        gpui::Corners::default(),
                                        decoded.image,
                                        frame,
                                        false,
                                    );
                                    caption.push((
                                        format!("{}×{}", natural.width.0, natural.height.0),
                                        theme.text3,
                                    ));
                                }
                                state => {
                                    let note = match state {
                                        Err(why) => why.note(&side.mime),
                                        Ok(_) => "Loading…".to_owned(),
                                    };
                                    let note = elide_end(&note, box_w - 16., &|text| {
                                        label(
                                            text.to_owned(),
                                            &metrics.ui,
                                            SMALL,
                                            theme.text3,
                                            window,
                                        )
                                    })
                                    .unwrap_or_default();
                                    let shaped =
                                        label(note, &metrics.ui, SMALL, theme.text3, window);
                                    let left = x + (box_w - f32::from(shaped.width)) / 2.;
                                    shaped
                                        .paint(
                                            point(px(left), px(y + 16. + box_h / 2. - 9.)),
                                            px(18.),
                                            TextAlign::Left,
                                            None,
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
                                        TextAlign::Left,
                                        None,
                                        window,
                                        cx,
                                    )
                                    .ok();
                                cx_x += f32::from(shaped.width) + 12.;
                            }
                            x += box_w + 16.;
                        }
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
                            .paint(
                                point(px(geo.left + 16.), px(y)),
                                px(NOTE_H),
                                TextAlign::Left,
                                None,
                                window,
                                cx,
                            )
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
                    let origin_x = code_x + CODE_PAD_L - pans[layout.file_of(row) - first_file];
                    let line = &lines[id as usize];
                    min_id = min_id.min(id);
                    max_id = max_id.max(id);
                    let text = (!line.text.is_empty())
                        .then(|| shape_code(shaped, doc, id, metrics, theme, window));
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
                        text.paint(
                            point(px(origin_x), px(y)),
                            px(LINE_H),
                            TextAlign::Left,
                            None,
                            window,
                            cx,
                        )
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
                    if header(file, geo.top + pinned).paint(&geo, metrics, theme, window, cx) {
                        clipped.push(file);
                    }
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
        // The sideways thumbs of the file under the pointer, when it
        // pans. The other files keep their last line clear.
        if let Some(bar) = pan_bar {
            for &track in bar.tracks() {
                let (x, width) = bar.thumb(track);
                window.paint_quad(quad(
                    rect(x, bar.seat - 10., x + width, bar.seat - 2.),
                    px(4.),
                    thumb_color(pan_hot),
                    px(0.),
                    thumb_color(pan_hot),
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

        // An animated picture on screen asks for a frame when its next one
        // is due; with none on screen, nothing wakes the surface.
        self.animating = next_frame.map(|due| {
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(due).await;
                let _ = this.update(cx, |_, cx| cx.notify());
            })
        });

        // The pointer has rested on a header that was cut: name the path
        // whole. The tooltip goes up from the next frame's prepaint, which
        // a notify from paint would not bring.
        if let Some(tip) = self.tip.as_mut() {
            if tip.due && tip.shown.is_none() && clipped.contains(&tip.file) {
                let file = &self.doc.files[tip.file];
                let text = file_title(&file.path, file.renamed_from.as_deref());
                let view = crate::ui::tooltip(text)(window, cx);
                tip.shown = Some((view, window.mouse_position()));
                window.request_animation_frame();
            }
        }
    }

    /// Where a selection drag is pulling the view, as overshoot in points:
    /// sideways once the pointer is in the belt at either edge of the
    /// selection's column, up or down once it is past the surface. Zero
    /// on an axis with nothing further that way to bring in.
    fn pull(&self, position: Point<Pixels>, selection: &Selection) -> (f32, f32) {
        let Some(geo) = self.geo else {
            return (0., 0.);
        };
        let past = |at: f32, low: f32, high: f32| (at - low).min(0.) + (at - high).max(0.);
        let with_room = |pull: f32, at: f32, max: f32| {
            if (pull < 0. && at > 0.) || (pull > 0. && at < max) {
                pull
            } else {
                0.
            }
        };
        let (x0, x1) = self.column(&geo, selection.side);
        // A press in the belt is not a pull until the drag has set off.
        let pull_x = if selection.anchor == selection.head {
            0.
        } else {
            past(position.x.into(), x0 + PULL_BELT, x1 - PULL_BELT)
        };
        let pull_y = past(position.y.into(), geo.top, geo.bottom);
        // Long lines pan within their own file: the one under the head.
        let file = self.layout().file_of(selection.head.row);
        (
            with_room(pull_x, self.pan_of(file), self.max_pan(file)),
            with_room(pull_y, self.scroll_y, self.max_scroll()),
        )
    }

    /// While a selection drag pulls at an edge, move toward it every frame
    /// and extend the selection to what comes into view. The pace follows
    /// the clock, not the display's refresh rate.
    fn tick_autoscroll(&mut self, window: &mut Window, cx: &App) {
        let (Drag::Select, Some(mut selection)) = (self.drag, self.selection) else {
            return;
        };
        let mouse = window.mouse_position();
        let (pull_x, pull_y) = self.pull(mouse, &selection);
        if (pull_x, pull_y) == (0., 0.) {
            self.pulled_at = None;
            return;
        }
        let now = Instant::now();
        let elapsed = self.pulled_at.map_or(1. / 60., |at| {
            now.duration_since(at).as_secs_f32().min(0.05)
        });
        self.pulled_at = Some(now);
        let step = |pull: f32| (pull * PULL_RATE).clamp(-PULL_MAX, PULL_MAX) * elapsed;

        let file = self.layout().file_of(selection.head.row);
        self.scroll_y += step(pull_y);
        self.clamp_scroll();
        self.set_pan(file, self.pan_of(file) + step(pull_x));
        selection.head = self.hit(mouse, Some(selection.side), window, cx).0;
        self.selection = Some(selection);
        window.request_animation_frame();
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

/// A quiet checker under a picture that lets what is behind it show,
/// drawn from the recess its box sits on, so transparency reads as
/// transparency in every theme without shouting.
fn paint_checker(area: Bounds<Pixels>, theme: &Theme, window: &mut Window) {
    const CELL: f32 = 7.;
    let dark = Theme::blend(theme.text1, 0.06, theme.recess);
    let (left, top) = (f32::from(area.left()), f32::from(area.top()));
    let (right, bottom) = (f32::from(area.right()), f32::from(area.bottom()));
    window.paint_quad(fill(area, theme.recess));
    let (mut row, mut y) = (0, top);
    while y < bottom {
        let (mut col, mut x) = (row % 2, left + (row % 2) as f32 * CELL);
        while x < right {
            window.paint_quad(fill(
                rect(x, y, (x + CELL).min(right), (y + CELL).min(bottom)),
                dark,
            ));
            col += 2;
            x = left + col as f32 * CELL;
        }
        row += 1;
        y = top + row as f32 * CELL;
    }
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
    doc: &DiffDoc,
    id: LineId,
    metrics: &Metrics,
    theme: &Theme,
    window: &Window,
) -> ShapedLine {
    if let Some(line) = shaped.get(&id) {
        return line.clone();
    }
    let line = &doc.lines[id as usize];
    let marks = doc.marks(id);
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
    let mut runs = Vec::with_capacity(marks.len() * 2 + 1);
    let mut at = 0;
    for mark in marks {
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
            // The change's edge: a solid stroke in its side's hue down the
            // gutter's outer edge, so runs of changes read as blocks from
            // across the room.
            let edge = if kind == Kind::Added {
                theme.add_fg
            } else {
                theme.del_fg
            };
            window.paint_quad(fill(
                rect(self.x, self.y, self.x + 3., self.y + LINE_H),
                Theme::fade(edge, 0.9),
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
    line.paint(
        point(px(x), px(y)),
        px(LINE_H),
        TextAlign::Left,
        None,
        window,
        cx,
    )
    .ok();
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
    sign.paint(
        point(px(x), px(y)),
        px(LINE_H),
        TextAlign::Left,
        None,
        window,
        cx,
    )
    .ok();
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

/// `text` cut to `room` points from its start, "…" standing in for what
/// went: a path keeps the end that names the file. `None` when nothing
/// but the ellipsis would fit.
fn elide_start(text: &str, room: f32, shape: &dyn Fn(&str) -> ShapedLine) -> Option<String> {
    let whole = shape(text);
    if f32::from(whole.width) <= room {
        return Some(text.to_owned());
    }
    let keep = room - f32::from(shape("…").width);
    if keep <= 0. {
        return None;
    }
    let mut cut = whole
        .index_for_x(whole.width - px(keep))
        .map_or(text.len(), |at| at + 1);
    while cut < text.len() && !text.is_char_boundary(cut) {
        cut += 1;
    }
    (cut < text.len()).then(|| format!("…{}", &text[cut..]))
}

/// `text` cut to `room` points at its end.
fn elide_end(text: &str, room: f32, shape: &dyn Fn(&str) -> ShapedLine) -> Option<String> {
    let whole = shape(text);
    if f32::from(whole.width) <= room {
        return Some(text.to_owned());
    }
    let keep = room - f32::from(shape("…").width);
    let mut cut = whole.index_for_x(px(keep.max(0.))).unwrap_or(0);
    while cut > 0 && !text.is_char_boundary(cut) {
        cut -= 1;
    }
    let kept = text[..cut].trim_end();
    (!kept.is_empty()).then(|| format!("{kept}…"))
}

/// One file header: the collapse chevron, the file's icon, its path, what
/// came, went or moved, where a rename came from, the conflict mark, the
/// counts and diffstat and — on a conflicted file — the Resolve button.
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
    /// Paint the header; true when its path did not fit whole.
    fn paint(
        &self,
        geo: &Geo,
        metrics: &Metrics,
        theme: &Theme,
        window: &mut Window,
        cx: &mut App,
    ) -> bool {
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
        let medium = Font {
            weight: FontWeight::MEDIUM,
            ..metrics.ui.clone()
        };

        // The right end first — the Resolve button, then +N −M and the
        // diffstat to its left, all clear of the scroll thumb — so the path
        // knows how much of the line is its own.
        let mut right = geo.right - THUMB_ZONE - 4.;
        if let Some(offer) = self.resolve {
            let waiting = offer.waiting.as_deref() == Some(file.path.as_ref());
            let hot = waiting || (self.hover == Some(true) && !offer.disabled);
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
                if hot { theme.accent_dim } else { theme.btn_bg },
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
                TextAlign::Left,
                None,
                window,
                cx,
            )
            .ok();
            right = f32::from(button.left()) - 10.;
        }
        // The five-block diffstat closes the line, the counts to its left.
        if file.added + file.removed > 0 {
            const BLOCK: f32 = 7.;
            const GAP: f32 = 1.5;
            let top = y + ((height - BLOCK) / 2.).round();
            right -= 5. * BLOCK + 4. * GAP;
            for (ix, block) in diffstat_blocks(file.added, file.removed)
                .into_iter()
                .enumerate()
            {
                let x = right + ix as f32 * (BLOCK + GAP);
                window.paint_quad(
                    fill(
                        rect(x, top, x + BLOCK, top + BLOCK),
                        match block {
                            Block::Added => theme.add_fg,
                            Block::Removed => theme.del_fg,
                            Block::Unchanged => theme.border1,
                        },
                    )
                    .corner_radii(px(1.5)),
                );
            }
            right -= 10.;
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
            stat.paint(at(right), px(height), TextAlign::Left, None, window, cx)
                .ok();
            right -= 6.;
        }

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
            None,
            TransformationMatrix::unit(),
            if self.hover.is_some() {
                theme.text2
            } else {
                theme.text3
            },
            cx,
        );
        x += 12. + 6.;

        // The file's kind in its language's hue.
        let (glyph, hue) = crate::ui::file_glyph(&file.name);
        const GLYPH: f32 = 14.;
        let _ = window.paint_svg(
            rect(
                x,
                y + ((height - GLYPH) / 2.).round(),
                x + GLYPH,
                y + ((height - GLYPH) / 2.).round() + GLYPH,
            ),
            glyph.path(),
            None,
            TransformationMatrix::unit(),
            theme.file_hue(hue),
            cx,
        );
        x += GLYPH + 8.;

        // Directory quiet, file name carrying the weight. What came or went
        // says so in its side's hue; a modified file, the usual case,
        // carries no word.
        let gone = file.status == FileStatus::Removed;
        let system = window.text_system().clone();
        let shape_with = |font: Font, size: f32, color: Hsla, strike: bool| {
            let system = system.clone();
            move |text: &str| {
                let run = TextRun {
                    len: text.len(),
                    font: font.clone(),
                    color,
                    background_color: None,
                    underline: None,
                    strikethrough: strike.then_some(StrikethroughStyle {
                        thickness: px(1.),
                        color: Some(color),
                    }),
                };
                system.shape_line(text.to_owned().into(), px(size), &[run], None)
            }
        };
        let dir_shape = shape_with(metrics.code.clone(), SMALL, theme.text3, false);
        let name_shape = shape_with(
            Font {
                weight: FontWeight::MEDIUM,
                ..metrics.code.clone()
            },
            SMALL,
            if gone { theme.text2 } else { theme.text1 },
            gone,
        );
        let from_shape = shape_with(metrics.code.clone(), XS, theme.text3, false);
        let word = match file.status {
            FileStatus::Added => Some(("new file", theme.add_fg)),
            FileStatus::Removed => Some(("deleted", theme.del_fg)),
            FileStatus::Renamed => Some(("renamed", theme.accent_strong)),
            FileStatus::Copied => Some(("copied", theme.accent_strong)),
            FileStatus::Modified => None,
        }
        .map(|(word, hue)| label(word, &medium, XS, hue, window));
        let chip = file.has_conflict.then(|| {
            let chip = if file.regions > 1 {
                format!("{} conflicts", file.regions)
            } else {
                "conflict".to_owned()
            };
            label(chip, &medium, XS, theme.danger, window)
        });
        let width = |line: &ShapedLine| f32::from(line.width);
        let fixed = word.as_ref().map_or(0., |word| width(word) + 6.)
            + chip.as_ref().map_or(0., |chip| width(chip) + 8.);

        // Fit the run into what the right end leaves. What gives way, in
        // turn: where a rename came from, then the directory, then the
        // name itself, each cut where it says least.
        let room = right - 12. - x;
        let (mut dir, mut name) = (file.dir.to_string(), Some(file.name.to_string()));
        let mut from = file.renamed_from.as_ref().map(|from| from.to_string());
        let need = |dir: &str, name: &Option<String>, from: &Option<String>| {
            width(&dir_shape(dir))
                + name.as_deref().map_or(0., |name| width(&name_shape(name)))
                + 10.
                + fixed
                + from
                    .as_deref()
                    .map_or(0., |from| width(&from_shape(&format!("from {from}"))) + 8.)
        };
        let clipped = need(&dir, &name, &from) > room;
        if clipped {
            if let Some(whole) = from.take() {
                let spare = room - need(&dir, &name, &None) - 8.;
                let prefix = width(&from_shape("from "));
                from = elide_start(&whole, spare - prefix, &from_shape)
                    .filter(|kept| kept.chars().count() > 4);
            }
            if need(&dir, &name, &from) > room {
                let spare = room - need("", &name, &from);
                dir = elide_start(&dir, spare, &dir_shape).unwrap_or_default();
            }
            if need(&dir, &name, &from) > room {
                let spare = room - need(&dir, &None, &from);
                name = elide_end(&file.name, spare, &name_shape);
            }
        }

        let dir = (!dir.is_empty()).then(|| dir_shape(&dir));
        let name = name.map(|name| name_shape(&name));
        if let Some(dir) = &dir {
            dir.paint(at(x), px(height), TextAlign::Left, None, window, cx)
                .ok();
            x += width(dir);
        }
        if let Some(name) = &name {
            if let Some(current) = self.path_hit {
                // The header when the file's path matched the query;
                // stronger while it is the current match.
                window.paint_quad(quad(
                    rect(x - 2., y + 6., x + width(name) + 2., y + height - 6.),
                    px(3.),
                    Theme::fade(theme.warn, if current { 0.55 } else { 0.28 }),
                    px(0.),
                    gpui::transparent_black(),
                    BorderStyle::default(),
                ));
            }
            name.paint(at(x), px(height), TextAlign::Left, None, window, cx)
                .ok();
            x += width(name);
        }
        x += 10.;
        if let Some(word) = &word {
            word.paint(at(x), px(height), TextAlign::Left, None, window, cx)
                .ok();
            x += width(word) + 6.;
        }
        if let Some(from) = from {
            let from = from_shape(&format!("from {from}"));
            from.paint(at(x), px(height), TextAlign::Left, None, window, cx)
                .ok();
            x += width(&from) + 8.;
        }
        if let Some(chip) = &chip {
            chip.paint(at(x), px(height), TextAlign::Left, None, window, cx)
                .ok();
        }
        clipped
    }
}

/// One character as the find bar compares it. These are the rules of a
/// case-insensitive JavaScript regular expression without the `u` flag,
/// which the find bar always used: a character stands for its uppercase
/// when that is one character in the Basic Multilingual Plane, and not an
/// ASCII letter standing in for a non-ASCII one. So σ, Σ and final ς are
/// one letter, while ß is not "SS", the long ſ is not s, and the Kelvin
/// sign is not k.
fn fold(ch: char) -> char {
    let bmp = |ch: char| u32::from(ch) <= 0xFFFF;
    let mut upper = ch.to_uppercase();
    match (upper.next(), upper.next()) {
        (Some(up), None) if bmp(ch) && bmp(up) && (ch.is_ascii() || !up.is_ascii()) => up,
        _ => ch,
    }
}

/// Every occurrence of `query` in the document, in reading order: each
/// file's path first (the header is a match surface too, for either name
/// of a rename), then its lines top to bottom. Offsets index the line as
/// painted. Matching folds case one character at a time (`fold`), so a
/// match always spans as many characters as the query.
pub fn find_in_doc(doc: &DiffDoc, query: &str) -> Vec<FindMatch> {
    let mut matches = Vec::new();
    let needle: Vec<char> = query.chars().map(fold).collect();
    if needle.is_empty() {
        return matches;
    }
    // Byte ranges of every match in `text`, left to right, none overlapping.
    let occurrences = |text: &str| -> Vec<Range<u32>> {
        let chars: Vec<(usize, char)> =
            text.char_indices().map(|(at, ch)| (at, fold(ch))).collect();
        let mut found = Vec::new();
        let mut start = 0;
        while start + needle.len() <= chars.len() {
            let hit = chars[start..start + needle.len()]
                .iter()
                .zip(&needle)
                .all(|((_, ch), want)| ch == want);
            if hit {
                let end = chars
                    .get(start + needle.len())
                    .map_or(text.len(), |(at, _)| *at);
                found.push(chars[start].0 as u32..end as u32);
                start += needle.len();
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
        cx: &mut App,
    ) -> Self::PrepaintState {
        let tip = self
            .view
            .read(cx)
            .tip
            .as_ref()
            .and_then(|tip| tip.shown.clone());
        if let Some((view, at)) = tip {
            window.set_tooltip(AnyTooltip {
                view,
                mouse_position: at,
                check_visible_and_update: std::rc::Rc::new(|_, _, _| true),
            });
        }
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
        let view = self.view.read(cx);
        let on_pan_thumb = view.pan_hover || matches!(view.drag, Drag::Pan(_));
        window.set_cursor_style(
            if view.header_hover.is_some() {
                CursorStyle::PointingHand
            } else if on_pan_thumb {
                CursorStyle::Arrow
            } else {
                CursorStyle::IBeam
            },
            surface,
        );
        window.set_cursor_style(CursorStyle::Arrow, strip);

        let view = self.view.clone();
        let hitbox = surface.clone();
        window.on_mouse_event(move |event: &ScrollEvent, phase, window, cx| {
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
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Copy is on offer only while there is something to copy: the
        // menu bar greys Edit ▸ Copy out otherwise, as it does for a field.
        let selected = self.selection.is_some_and(|s| s.anchor != s.head);
        // With the keys on it from the keyboard, the document wears the
        // ring around its edge, over what it paints.
        let ringed = crate::ui::keyboard_on(&self.focus, window);
        let accent = crate::theme::theme(cx).accent;
        gpui::div()
            .id("diff-surface")
            .role(gpui::Role::Document)
            .aria_label(format!(
                "Diff, {}",
                crate::logic::text::counted(self.doc.files.len(), "file")
            ))
            .key_context("DiffSurface")
            .track_focus(&self.focus)
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .when(selected, |el| el.on_action(cx.listener(Self::copy)))
            .on_action(cx.listener(Self::select_all))
            .on_action(cx.listener(Self::page_down))
            .on_action(cx.listener(Self::page_up))
            .on_action(cx.listener(Self::scroll_top))
            .on_action(cx.listener(Self::scroll_bottom))
            .on_action(cx.listener(Self::next_file))
            .on_action(cx.listener(Self::prev_file))
            .child(DiffElement { view: cx.entity() })
            .when(ringed, |el| {
                el.child(
                    gpui::div()
                        .absolute()
                        .inset_0()
                        .border_2()
                        .border_color(accent),
                )
            })
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

    /// A diff of files whose lines are all added, each named by its path
    /// and, for a rename, where it came from.
    fn diff_of(files: &[(&str, Option<&str>, &[&str])]) -> jiji_core::snapshot::ChangeDiff {
        use jiji_core::snapshot::{
            ChangeDiff, DiffHunk, DiffLine, DiffLineKind, DiffSegment, FileDiff, FileDiffContent,
        };
        let files = files
            .iter()
            .map(|(path, from, lines)| FileDiff {
                path: (*path).into(),
                status: if from.is_some() {
                    FileStatus::Renamed
                } else {
                    FileStatus::Added
                },
                renamed_from: from.map(str::to_owned),
                has_conflict: false,
                content: FileDiffContent::Text {
                    hunks: vec![DiffHunk {
                        old_start: 0,
                        new_start: 1,
                        context: None,
                        lines: lines
                            .iter()
                            .map(|text| DiffLine {
                                kind: DiffLineKind::Added,
                                segments: vec![DiffSegment {
                                    text: (*text).into(),
                                    changed: true,
                                }],
                                pair: None,
                            })
                            .collect(),
                    }],
                    truncated: false,
                },
            })
            .collect();
        ChangeDiff {
            id: "x".into(),
            from: None,
            files,
            truncated: false,
        }
    }

    fn added_doc(path: &str, lines: &[&str]) -> DiffDoc {
        DiffDoc::build(&diff_of(&[(path, None, lines)]))
    }

    #[gpui::test]
    fn a_cut_header_names_its_whole_path_on_hover(cx: &mut gpui::TestAppContext) {
        let dir = crate::testing::scratch("header-tip");
        let _model = crate::testing::install(&dir, cx);
        let (surface, cx) = cx.add_window_view(|_, cx| DiffSurface::new(false, cx));
        let long = format!("src/{}measure.ts", "a/deeply/nested/directory/".repeat(40));
        let diff = diff_of(&[
            (&long, Some("src/lib/rowHeight.ts"), &["one"]),
            ("b.rs", None, &["two"]),
        ]);
        let prepared = Prepared::new(Arc::new(diff));
        surface.update(cx, |surface, cx| surface.set_doc(&prepared, false, cx));
        cx.run_until_parked();
        let rest_on = |y: f32, cx: &mut gpui::VisualTestContext| {
            cx.simulate_mouse_move(point(px(240.), px(y)), None, gpui::Modifiers::none());
            cx.executor().advance_clock(crate::ui::tooltip::SHOW_DELAY);
            cx.run_until_parked();
        };
        let tip = |surface: &Entity<DiffSurface>, cx: &mut gpui::VisualTestContext| {
            surface.read_with(cx, |surface, _| {
                surface
                    .tip
                    .as_ref()
                    .map(|tip| (tip.file, tip.shown.is_some()))
            })
        };

        rest_on(HEADER_H / 2., cx);
        assert_eq!(tip(&surface, cx), Some((0, true)));
        // A path that fits says nothing more on hover.
        let second = surface.read_with(cx, |surface, _| surface.layout().file_top(1));
        rest_on(second + HEADER_H / 2., cx);
        assert_eq!(tip(&surface, cx), Some((1, false)));
        // A press takes the tooltip down.
        rest_on(HEADER_H / 2., cx);
        cx.simulate_mouse_down(
            point(px(240.), px(HEADER_H / 2.)),
            MouseButton::Left,
            gpui::Modifiers::none(),
        );
        assert_eq!(tip(&surface, cx), None);
        let _ = std::fs::remove_dir_all(dir);
    }

    fn found_text<'a>(doc: &'a DiffDoc, found: &FindMatch) -> &'a str {
        let (line, range) = found.line.clone().expect("a line match");
        &doc.lines[line as usize].text[range.start as usize..range.end as usize]
    }

    #[test]
    fn find_takes_sigma_in_every_form_in_paths_and_lines() {
        let doc = added_doc("notes/ὀδυσσεύς.md", &["Ὀδυσσεύς · ΣΟΦΟΣ · σοφός"]);
        let found = find_in_doc(&doc, "σ");
        // The path, whose only sigma is final, then the line left to right.
        assert_eq!(
            found[0],
            FindMatch {
                file: 0,
                line: None
            }
        );
        let hits: Vec<&str> = found[1..].iter().map(|m| found_text(&doc, m)).collect();
        assert_eq!(hits, ["σ", "σ", "ς", "Σ", "Σ", "σ", "ς"]);
        // Offsets are the painted line's bytes: Ὀ is three, δ and υ two.
        assert_eq!(found[1].line.clone().unwrap().1, 7..9);
        assert_eq!(find_in_doc(&doc, "Σ"), found);
        assert_eq!(find_in_doc(&doc, "ς"), found);
        assert_eq!(find_in_doc(&doc, "ΣΟΦ").len(), 2);
    }

    #[test]
    fn find_folds_only_what_the_old_find_bar_folded() {
        let doc = added_doc("a.md", &["Straße · \u{17F} · \u{212A} · \u{130}"]);
        // ß is not "ss", the long s is not s, the Kelvin sign is not k, and
        // dotted İ is not i: each would take an ASCII letter for another.
        assert!(find_in_doc(&doc, "ss").is_empty());
        assert_eq!(find_in_doc(&doc, "s").len(), 1);
        assert!(find_in_doc(&doc, "k").is_empty());
        assert!(find_in_doc(&doc, "i").is_empty());
        let hits: Vec<&str> = find_in_doc(&doc, "STRASSE")
            .iter()
            .map(|m| found_text(&doc, m))
            .collect();
        assert!(hits.is_empty(), "{hits:?}");
        assert_eq!(find_in_doc(&doc, "straße").len(), 1);
        assert!(find_in_doc(&doc, "").is_empty());
    }

    #[gpui::test]
    fn an_animation_asks_for_frames_only_while_it_is_on_screen(cx: &mut gpui::TestAppContext) {
        use base64::Engine as _;
        use jiji_core::snapshot::{FileDiff, FileDiffContent, ImageContent};
        let dir = crate::testing::scratch("animation-frames");
        let _model = crate::testing::install(&dir, cx);
        let (surface, cx) = cx.add_window_view(|_, cx| DiffSurface::new(false, cx));
        let gif = include_bytes!("fixtures/anim.gif");
        let lines: Vec<String> = (0..400).map(|n| format!("line {n}")).collect();
        let lines: Vec<&str> = lines.iter().map(String::as_str).collect();
        let mut diff = diff_of(&[("notes.txt", None, &lines)]);
        diff.files.insert(
            0,
            FileDiff {
                path: "spinner.gif".into(),
                status: FileStatus::Added,
                renamed_from: None,
                has_conflict: false,
                content: FileDiffContent::Image {
                    old: None,
                    new: Some(ImageContent {
                        mime: "image/gif".into(),
                        size: gif.len() as u64,
                        data: Some(base64::engine::general_purpose::STANDARD.encode(gif)),
                    }),
                },
            },
        );
        let prepared = Prepared::new(Arc::new(diff));
        surface.update(cx, |surface, cx| surface.set_doc(&prepared, false, cx));
        cx.run_until_parked();
        let animating = |surface: &Entity<DiffSurface>, cx: &mut gpui::VisualTestContext| {
            surface.read_with(cx, |surface, _| surface.animating.is_some())
        };
        assert!(animating(&surface, cx), "the two-frame GIF is on screen");

        // Scrolled out of view, it costs nothing until it is back.
        surface.update(cx, |surface, cx| surface.scroll_to(f32::MAX, cx));
        cx.run_until_parked();
        assert!(!animating(&surface, cx));
        surface.update(cx, |surface, cx| surface.scroll_to(0., cx));
        cx.run_until_parked();
        assert!(animating(&surface, cx));
        let _ = std::fs::remove_dir_all(dir);
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
