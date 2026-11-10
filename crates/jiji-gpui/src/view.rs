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
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui::{
    actions, div, fill, font, point, prelude::*, px, quad, relative, App, BorderStyle, Bounds,
    ClickEvent, ClipboardItem, ContentMask, Context, CursorStyle, DispatchPhase, ElementId, Entity,
    FocusHandle, Focusable, Font, FontStyle, FontWeight, GlobalElementId, Hitbox, HitboxBehavior,
    Hsla, InspectorElementId, Keystroke, LayoutId, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, Pixels, Point, ScrollWheelEvent, ShapedLine, SharedString, StrikethroughStyle,
    Style, TextRun, TouchPhase, Window,
};
use jiji_core::snapshot::FileStatus;

use crate::highlight::{Mark, Syn};
use crate::model::{DiffDoc, Kind, Layout, Line, LineId, Row, GAP_H, HEADER_H, LINE_H, NOTE_H};
use crate::theme::Theme;

actions!(
    diff,
    [
        CopySelection,
        SelectAll,
        ToggleLayout,
        PageDown,
        PageUp,
        ScrollTop,
        ScrollBottom,
        NextFile,
        PrevFile,
        RunBench,
        ToggleHud,
        Quit,
    ]
);

const CODE_SIZE: f32 = 12.;
const XS: f32 = 11.;
const SMALL: f32 = 12.;
/// Left padding inside a code cell, and the breathing room kept after the
/// widest line at full pan.
const CODE_PAD_L: f32 = 8.;
const CODE_PAD_R: f32 = 16.;
const TOOLBAR_H: f32 = 36.;
/// Width of the strip at the right edge that belongs to the scroll thumb.
const THUMB_ZONE: f32 = 12.;

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

pub struct BenchPlan {
    /// Scroll speeds to sweep the diff at, in px/s.
    pub speeds: Vec<f32>,
    pub exit: bool,
}

/// A sweep stops at the end of the diff or after this long, whichever
/// comes first: a few hundred frames is plenty for the percentiles.
const BENCH_PASS_SECONDS: f32 = 6.;

/// CPU seconds this process has used so far, across all threads — the
/// renderer's own work included, not just this file's paint.
fn process_cpu_seconds() -> f64 {
    // SAFETY: getrusage fills the struct it is handed.
    let usage = unsafe {
        let mut usage: libc::rusage = std::mem::zeroed();
        libc::getrusage(libc::RUSAGE_SELF, &mut usage);
        usage
    };
    let seconds = |time: libc::timeval| time.tv_sec as f64 + time.tv_usec as f64 / 1e6;
    seconds(usage.ru_utime) + seconds(usage.ru_stime)
}

struct Bench {
    plan: BenchPlan,
    pass: usize,
    started: Instant,
    cpu_at_start: f64,
    last_frame: Option<Instant>,
    intervals: Vec<f32>,
    paints: Vec<f32>,
}

#[derive(Default)]
struct FrameStats {
    frames: usize,
    last_start: Option<Instant>,
    paint_ms: f32,
    /// Smoothed interval over a run of back-to-back frames, and how long
    /// that run is; a lone repaint says nothing about frame rate.
    interval_ms: f32,
    streak: usize,
    rows: usize,
    bench_summary: Option<SharedString>,
}

/// One step of a scripted run (`--script`). Each drives the same handler
/// the pointer or keyboard reaches, in window coordinates, so a run can be
/// checked end to end without taking over the user's mouse.
pub enum Step {
    Scroll(f32),
    Pan(f32),
    Split(bool),
    /// Press at the first point, move to the second, release.
    Drag(f32, f32, f32, f32),
    /// A single, double or triple click.
    Click(usize, f32, f32),
    /// Type a keystroke (`s`, `cmd-a`, `]`) through GPUI's key dispatch.
    Key(String),
    /// Print what cmd-c would copy, without touching the clipboard.
    Print,
    /// Write the window to a PNG.
    Shot(std::path::PathBuf),
    Quit,
}

pub struct Options {
    pub split: bool,
    pub bench: Option<BenchPlan>,
    pub script: Vec<Step>,
    pub launched: Instant,
}

pub struct DiffView {
    doc: DiffDoc,
    theme: Rc<Theme>,
    title: SharedString,
    focus: FocusHandle,
    split: bool,
    scroll_y: f32,
    /// Horizontal offset per file: long lines pan sideways within their
    /// own file, so one wide file never leaves the others scrolled blank.
    pan: Vec<f32>,
    selection: Option<Selection>,
    drag: Drag,
    thumb_hover: bool,
    axis: Option<Axis>,
    metrics: Option<Metrics>,
    geo: Option<Geo>,
    /// Shaped code lines near the viewport, kept across frames so a steady
    /// frame shapes nothing.
    shaped: HashMap<LineId, ShapedLine>,
    numbers: HashMap<(u32, u8), ShapedLine>,
    pending_jobs: usize,
    stats: FrameStats,
    show_hud: bool,
    bench: Option<Bench>,
    bench_plan: Option<BenchPlan>,
    launched: Instant,
}

impl Focusable for DiffView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl DiffView {
    pub fn new(
        mut doc: DiffDoc,
        title: SharedString,
        options: Options,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        // Highlight every file off the main thread, one task per file, in
        // file order so the top of the diff colors first. Each result
        // lands as a plain repaint.
        let jobs = std::mem::take(&mut doc.jobs);
        let pending_jobs = jobs.len();
        for job in jobs {
            let work = cx.background_executor().spawn(async move { job.run() });
            cx.spawn(async move |this, cx| {
                let marks = work.await;
                this.update(cx, |this, cx| this.apply_marks(marks, cx)).ok();
            })
            .detach();
        }
        if !options.script.is_empty() {
            let steps = options.script;
            cx.spawn_in(window, async move |this, cx| {
                // Let the first frames paint: steps hit-test against the
                // geometry of the frame before them.
                cx.background_executor()
                    .timer(Duration::from_millis(600))
                    .await;
                for step in steps {
                    match step {
                        // Dispatched from outside the view: the bound
                        // action re-enters it through its own listeners.
                        Step::Key(key) => {
                            cx.update(|window, cx| match Keystroke::parse(&key) {
                                Ok(keystroke) => {
                                    window.dispatch_keystroke(keystroke, cx);
                                }
                                Err(err) => eprintln!("jiji-gpui: bad key `{key}`: {err}"),
                            })
                            .ok();
                        }
                        step => {
                            this.update_in(cx, |this, window, cx| this.run_step(step, window, cx))
                                .ok();
                        }
                    }
                    cx.background_executor()
                        .timer(Duration::from_millis(150))
                        .await;
                }
            })
            .detach();
        }
        Self {
            pan: vec![0.; doc.files.len()],
            doc,
            theme: Rc::new(Theme::midnight()),
            title,
            focus: cx.focus_handle(),
            split: options.split,
            scroll_y: 0.,
            selection: None,
            drag: Drag::None,
            thumb_hover: false,
            axis: None,
            metrics: None,
            geo: None,
            shaped: HashMap::new(),
            numbers: HashMap::new(),
            pending_jobs,
            stats: FrameStats::default(),
            show_hud: true,
            bench: None,
            bench_plan: options.bench,
            launched: options.launched,
        }
    }

    fn run_step(&mut self, step: Step, window: &mut Window, cx: &mut Context<Self>) {
        let down = |count: usize, x: f32, y: f32| MouseDownEvent {
            button: MouseButton::Left,
            position: point(px(x), px(y)),
            click_count: count,
            ..Default::default()
        };
        match step {
            Step::Scroll(y) => self.scroll_to(y, cx),
            Step::Pan(x) => {
                let file = self.layout().file_of(self.layout().row_at(self.scroll_y));
                self.set_pan(file, x);
                cx.notify();
            }
            Step::Split(split) => self.set_split(split, cx),
            Step::Click(count, x, y) => {
                self.on_mouse_down(&down(count, x, y), window, cx);
                self.on_mouse_up(&MouseUpEvent::default(), cx);
            }
            Step::Drag(x0, y0, x1, y1) => {
                self.on_mouse_down(&down(1, x0, y0), window, cx);
                let moved = MouseMoveEvent {
                    position: point(px(x1), px(y1)),
                    pressed_button: Some(MouseButton::Left),
                    ..Default::default()
                };
                self.on_mouse_move(&moved, window, cx);
                self.on_mouse_up(&MouseUpEvent::default(), cx);
            }
            Step::Key(_) => {}
            Step::Print => {
                let text = self.selected_text().unwrap_or_default();
                let shown: String = text.chars().take(600).collect();
                let more = if shown.len() < text.len() {
                    "\n…"
                } else {
                    ""
                };
                println!(
                    "--- selection ({} bytes) ---\n{shown}{more}\n--- end ---",
                    text.len()
                );
            }
            Step::Shot(path) => match crate::capture::window_png(window, &path) {
                Ok(()) => println!("wrote {}", path.display()),
                Err(err) => eprintln!("jiji-gpui: shot failed: {err}"),
            },
            Step::Quit => cx.quit(),
        }
    }

    fn apply_marks(&mut self, marks: Vec<(LineId, Vec<Mark>)>, cx: &mut Context<Self>) {
        for (id, marks) in marks {
            self.doc.lines[id as usize].marks = marks;
            self.shaped.remove(&id);
        }
        self.pending_jobs = self.pending_jobs.saturating_sub(1);
        if self.pending_jobs == 0 {
            eprintln!(
                "jiji-gpui: highlighting finished {:.0?} after launch",
                self.launched.elapsed()
            );
        }
        cx.notify();
    }

    fn layout(&self) -> &Layout {
        if self.split {
            &self.doc.split
        } else {
            &self.doc.unified
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

    fn scroll_to(&mut self, y: f32, cx: &mut Context<Self>) {
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

    fn selected_text(&self) -> Option<String> {
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

    fn toggle_layout(&mut self, _: &ToggleLayout, _: &mut Window, cx: &mut Context<Self>) {
        self.set_split(!self.split, cx);
    }

    /// Switch layouts, keeping the reader in the same file at the same
    /// relative depth: row indices differ between layouts, files do not.
    fn set_split(&mut self, split: bool, cx: &mut Context<Self>) {
        if split == self.split {
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

    fn run_bench(&mut self, _: &RunBench, _: &mut Window, cx: &mut Context<Self>) {
        if self.bench.is_none() {
            self.bench_plan = Some(BenchPlan {
                speeds: vec![9_000., 18_000.],
                exit: false,
            });
            cx.notify();
        }
    }

    fn toggle_hud(&mut self, _: &ToggleHud, _: &mut Window, cx: &mut Context<Self>) {
        self.show_hud = !self.show_hud;
        cx.notify();
    }

    // ── Input ──

    fn on_scroll(&mut self, event: &ScrollWheelEvent, cx: &mut Context<Self>) {
        if self.bench.is_some() {
            return;
        }
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
        let (pos, side) = self.hit(event.position, None, window);
        match (event.click_count, self.selection) {
            (1, Some(mut selection)) if event.modifiers.shift => {
                selection.head = self.hit(event.position, Some(selection.side), window).0;
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

    fn on_mouse_move(
        &mut self,
        event: &MouseMoveEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let held = event.pressed_button == Some(MouseButton::Left);
        match self.drag {
            Drag::Select if held => {
                if let Some(mut selection) = self.selection {
                    let head = self.hit(event.position, Some(selection.side), window).0;
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
    fn hit(&mut self, position: Point<Pixels>, side: Option<Side>, window: &Window) -> (Pos, Side) {
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
                    &self.theme,
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

    fn ensure_metrics(&mut self, window: &Window) {
        if self.metrics.is_some() {
            return;
        }
        let system = window.text_system();
        // The webview asks for ui-monospace, which is SF Mono; it is only a
        // named family when installed, so fall back to the Menlo every Mac
        // has.
        let names = system.all_font_names();
        let family = ["SF Mono", "Menlo"]
            .into_iter()
            .find(|family| names.iter().any(|name| name == family))
            .unwrap_or("Menlo");
        let code = font(family);
        let ui = font(".SystemUIFont");
        let id = system.resolve_font(&code);
        let advance = |size: f32| {
            system
                .ch_advance(id, px(size))
                .map(f32::from)
                .unwrap_or(size * 0.6)
        };
        let theme = &self.theme;
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

    fn paint(&mut self, bounds: Bounds<Pixels>, window: &mut Window, cx: &mut App) {
        let frame_start = Instant::now();
        let interval = self
            .stats
            .last_start
            .map_or(f32::MAX, |last| (frame_start - last).as_secs_f32() * 1000.);
        if interval < 40. {
            let stats = &mut self.stats;
            stats.interval_ms = if stats.streak == 0 {
                interval
            } else {
                stats.interval_ms * 0.85 + interval * 0.15
            };
            stats.streak += 1;
        } else {
            self.stats.streak = 0;
        }
        self.stats.last_start = Some(frame_start);
        if self.stats.frames == 0 {
            eprintln!(
                "jiji-gpui: first frame {:.0?} after launch",
                frame_start - self.launched
            );
        }
        self.stats.frames += 1;

        self.ensure_metrics(window);
        let geo = self.geometry(bounds);
        self.geo = Some(geo);
        self.tick_bench(frame_start, window, cx);
        self.tick_autoscroll(window);
        self.clamp_scroll();

        // Snap to device pixels so glyphs land on the same grid every frame.
        let scale = window.scale_factor();
        let scroll = (self.scroll_y * scale).round() / scale;
        let split = self.split;
        let selection = self.selection.filter(|s| s.anchor != s.head);
        let thumb = self.thumb();
        let thumb_hot = self.thumb_hover || matches!(self.drag, Drag::Thumb(_));
        let theme = self.theme.clone();
        let theme = &*theme;

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

        let Self {
            doc,
            shaped,
            numbers,
            metrics,
            ..
        } = self;
        let metrics = metrics.as_ref().expect("metrics resolved above");
        let layout = if split { &doc.split } else { &doc.unified };
        let lines = &doc.lines;
        let files = &doc.files;

        let row_y = |row: usize| geo.top + layout.tops[row] - scroll;

        window.paint_quad(fill(bounds, theme.bg1));

        // Pass 1 — everything but code text: fills, gutter rails, numbers,
        // signs, seams and file headers.
        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            for row in first..last {
                let y = row_y(row);
                match &layout.rows[row] {
                    Row::Pad => {}
                    Row::FileHeader { file } => paint_header(
                        &files[*file as usize],
                        *file,
                        y,
                        &geo,
                        metrics,
                        theme,
                        window,
                        cx,
                    ),
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
                    paint_header(
                        &files[file],
                        file as u32,
                        geo.top + pinned,
                        &geo,
                        metrics,
                        theme,
                        window,
                        cx,
                    )
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

        self.stats.rows = last - first;
        self.stats.paint_ms = frame_start.elapsed().as_secs_f32() * 1000.;
        if let Some(bench) = &mut self.bench {
            bench.paints.push(self.stats.paint_ms);
        }
    }

    /// While a selection drag sits past the top or bottom edge, scroll
    /// toward it every frame and extend the selection to the new edge row.
    fn tick_autoscroll(&mut self, window: &mut Window) {
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
            selection.head = self.hit(mouse, Some(selection.side), window).0;
            self.selection = Some(selection);
            window.request_animation_frame();
        }
    }

    // ── Benchmark ──

    /// Sweeps the whole diff at fixed speeds, driven by wall-clock time: a
    /// late frame travels further instead of slowing the scroll, so missed
    /// frames show up in the intervals rather than hiding in a longer run.
    fn tick_bench(&mut self, now: Instant, window: &mut Window, cx: &mut App) {
        if self.bench.is_none() {
            if self.bench_plan.is_none() {
                return;
            }
            // Start once highlighting has settled and the window has
            // painted a few frames, so the sweep measures the steady state
            // the reader scrolls through rather than startup.
            if self.pending_jobs > 0 || self.stats.frames < 30 {
                window.request_animation_frame();
                return;
            }
            let plan = self.bench_plan.take().unwrap();
            self.stats.bench_summary = None;
            self.selection = None;
            self.bench = Some(Bench {
                plan,
                pass: 0,
                started: now,
                cpu_at_start: process_cpu_seconds(),
                last_frame: None,
                intervals: Vec::new(),
                paints: Vec::new(),
            });
        }
        let max = self.max_scroll();
        let rows = self.layout().rows.len();
        let layout = if self.split { "split" } else { "unified" };
        let bench = self.bench.as_mut().unwrap();
        if let Some(last) = bench.last_frame {
            bench.intervals.push((now - last).as_secs_f32() * 1000.);
        }
        bench.last_frame = Some(now);
        let speed = bench.plan.speeds[bench.pass];
        let elapsed = (now - bench.started).as_secs_f32();
        let y = speed * elapsed;
        if y < max && elapsed < BENCH_PASS_SECONDS {
            self.scroll_y = y;
            window.request_animation_frame();
            return;
        }

        let cpu = process_cpu_seconds();
        let cpu_ms = ((cpu - bench.cpu_at_start) * 1000.) as f32;
        bench.cpu_at_start = cpu;
        let (report, short) = summarize(
            layout,
            rows,
            y.min(max),
            speed,
            elapsed,
            cpu_ms,
            &mut bench.intervals,
            &mut bench.paints,
        );
        println!("{report}");
        self.stats.bench_summary = Some(short.into());
        bench.pass += 1;
        bench.started = now;
        bench.last_frame = None;
        bench.intervals.clear();
        bench.paints.clear();
        self.scroll_y = 0.;
        if bench.pass < bench.plan.speeds.len() {
            window.request_animation_frame();
        } else {
            let exit = bench.plan.exit;
            self.bench = None;
            if exit {
                cx.defer(|cx| cx.quit());
            }
        }
    }
}

fn percentile(sorted: &[f32], p: f32) -> f32 {
    if sorted.is_empty() {
        return 0.;
    }
    sorted[((sorted.len() - 1) as f32 * p).round() as usize]
}

#[allow(clippy::too_many_arguments)]
fn summarize(
    layout: &str,
    rows: usize,
    distance: f32,
    speed: f32,
    seconds: f32,
    cpu_ms: f32,
    intervals: &mut [f32],
    paints: &mut [f32],
) -> (String, String) {
    intervals.sort_by(f32::total_cmp);
    paints.sort_by(f32::total_cmp);
    let median = percentile(intervals, 0.5);
    let late = intervals.iter().filter(|&&ms| ms > median * 1.5).count();
    let fps = intervals.len() as f32 / seconds.max(0.001);
    let short = format!("{speed:.0} px/s: {fps:.0} fps, {late} late frames");
    let report = format!(
        "bench {layout} {rows} rows, {distance:.0}px at {speed:.0}px/s: {frames} frames in {seconds:.2}s ({fps:.1} fps) | \
         frame interval p50 {median:.2} p95 {p95:.2} p99 {p99:.2} max {max:.2} ms, {late} late | \
         paint p50 {paint50:.2} p95 {paint95:.2} max {paint_max:.2} ms | \
         process cpu {cpu_frame:.2} ms/frame",
        frames = intervals.len() + 1,
        cpu_frame = cpu_ms / (intervals.len() + 1) as f32,
        p95 = percentile(intervals, 0.95),
        p99 = percentile(intervals, 0.99),
        max = intervals.last().copied().unwrap_or(0.),
        paint50 = percentile(paints, 0.5),
        paint95 = percentile(paints, 0.95),
        paint_max = paints.last().copied().unwrap_or(0.),
    );
    (report, short)
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

#[allow(clippy::too_many_arguments)]
fn paint_header(
    file: &crate::model::FileModel,
    index: u32,
    y: f32,
    geo: &Geo,
    metrics: &Metrics,
    theme: &Theme,
    window: &mut Window,
    cx: &mut App,
) {
    let height = HEADER_H - 1.;
    window.paint_quad(fill(
        rect(geo.left, y, geo.right, y + height),
        theme.file_head,
    ));
    window.paint_quad(fill(
        rect(geo.left, y + height, geo.right, y + HEADER_H),
        theme.border2,
    ));
    if index > 0 {
        window.paint_quad(fill(rect(geo.left, y, geo.right, y + 1.), theme.border1));
    }
    let at = |x: f32| point(px(x), px(y));

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
    let mut x = geo.left + 12.;
    label(letter, &bold, XS, hue, window)
        .paint(at(x), px(height), window, cx)
        .ok();
    x += 12. + 8.;

    // Directory quiet, file name carrying the weight.
    let gone = file.status == FileStatus::Removed;
    let path: SharedString = format!("{}{}", file.dir, file.name).into();
    let run = |len: usize, color: Hsla, weight: FontWeight, strike: bool| TextRun {
        len,
        font: Font {
            weight,
            ..metrics.ui.clone()
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
        .shape_line(path, px(SMALL), &runs, None);
    path.paint(at(x), px(height), window, cx).ok();
    x += f32::from(path.width) + 8.;

    if let Some(from) = &file.renamed_from {
        let from = label(format!("← {from}"), &metrics.ui, XS, theme.text3, window);
        from.paint(at(x), px(height), window, cx).ok();
        x += f32::from(from.width) + 8.;
    }
    if file.has_conflict {
        let medium = Font {
            weight: FontWeight::MEDIUM,
            ..metrics.ui.clone()
        };
        label("conflict", &medium, XS, theme.danger, window)
            .paint(at(x), px(height), window, cx)
            .ok();
    }

    // +N −M, right-aligned clear of the scroll thumb.
    let mut right = geo.right - 16.;
    for (count, sign, color) in [
        (file.removed, "−", theme.del_fg),
        (file.added, "+", theme.add_fg),
    ] {
        if count == 0 {
            continue;
        }
        let stat = label(format!("{sign}{count}"), &metrics.ui, XS, color, window);
        right -= f32::from(stat.width);
        stat.paint(at(right), px(height), window, cx).ok();
        right -= 6.;
    }
}

/// The element that hands the viewport to `DiffView::paint` and routes
/// window mouse events back to the view.
struct DiffElement {
    view: Entity<DiffView>,
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
        // Take whatever height the toolbar leaves.
        let mut style = Style {
            flex_grow: 1.,
            flex_shrink: 1.,
            flex_basis: px(0.).into(),
            ..Style::default()
        };
        style.min_size.height = px(0.).into();
        style.size.width = relative(1.).into();
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
        // the diff, arrow over the thumb strip.
        window.set_cursor_style(CursorStyle::IBeam, surface);
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
            if phase == DispatchPhase::Bubble
                && event.button == MouseButton::Left
                && hitbox.is_hovered(window)
            {
                view.update(cx, |view, cx| view.on_mouse_down(event, window, cx));
            }
        });
        // Moves and releases are taken window-wide so a drag keeps working
        // after it leaves the surface.
        let view = self.view.clone();
        window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
            if phase == DispatchPhase::Bubble {
                view.update(cx, |view, cx| view.on_mouse_move(event, window, cx));
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

impl Render for DiffView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = self.theme.clone();
        let files = self.doc.files.len();
        let segment = |id: &'static str, text: &'static str, active: bool, split: bool| {
            div()
                .id(id)
                .h(px(22.))
                .px(px(10.))
                .flex()
                .items_center()
                .rounded(px(4.))
                .text_size(px(XS))
                .cursor_pointer()
                .text_color(if active { theme.text1 } else { theme.text3 })
                .when(active, |this| this.bg(theme.bg3))
                .child(text)
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.set_split(split, cx)))
        };
        let hud = self.show_hud.then(|| {
            let stats = &self.stats;
            let line = if self.bench.is_some() {
                "benchmarking…".to_owned()
            } else if self.pending_jobs > 0 {
                format!("highlighting… {} files left", self.pending_jobs)
            } else if let Some(summary) = &stats.bench_summary {
                summary.to_string()
            } else if stats.streak >= 8 {
                format!(
                    "paint {:.2} ms · {} rows · {:.0} fps",
                    stats.paint_ms,
                    stats.rows,
                    1000. / stats.interval_ms
                )
            } else {
                format!("paint {:.2} ms · {} rows", stats.paint_ms, stats.rows)
            };
            div().text_size(px(XS)).text_color(theme.text3).child(line)
        });

        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(theme.bg1)
            .text_color(theme.text1)
            .font_family(".SystemUIFont")
            .text_size(px(SMALL))
            .key_context("DiffView")
            .track_focus(&self.focus)
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::select_all))
            .on_action(cx.listener(Self::toggle_layout))
            .on_action(cx.listener(Self::page_down))
            .on_action(cx.listener(Self::page_up))
            .on_action(cx.listener(Self::scroll_top))
            .on_action(cx.listener(Self::scroll_bottom))
            .on_action(cx.listener(Self::next_file))
            .on_action(cx.listener(Self::prev_file))
            .on_action(cx.listener(Self::run_bench))
            .on_action(cx.listener(Self::toggle_hud))
            .child(
                // Title bar and toolbar in one: the window's traffic lights
                // sit in the left inset.
                div()
                    .h(px(TOOLBAR_H))
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap(px(12.))
                    .pl(px(84.))
                    .pr(px(12.))
                    .bg(theme.bg0)
                    .border_b_1()
                    .border_color(theme.border2)
                    .child(
                        div()
                            .font_weight(FontWeight::MEDIUM)
                            .child(self.title.clone()),
                    )
                    .child(
                        div()
                            .flex()
                            .gap(px(8.))
                            .text_size(px(XS))
                            .child(div().text_color(theme.text3).child(format!(
                                "{files} {}",
                                if files == 1 { "file" } else { "files" }
                            )))
                            .child(
                                div()
                                    .text_color(theme.add_fg)
                                    .child(format!("+{}", self.doc.added)),
                            )
                            .child(
                                div()
                                    .text_color(theme.del_fg)
                                    .child(format!("−{}", self.doc.removed)),
                            ),
                    )
                    .child(div().flex_1())
                    .children(hud)
                    .child(
                        div()
                            .flex()
                            .p(px(2.))
                            .rounded(px(6.))
                            .bg(theme.bg1)
                            .border_1()
                            .border_color(theme.border2)
                            .child(segment("layout-unified", "Unified", !self.split, false))
                            .child(segment("layout-split", "Split", self.split, true)),
                    ),
            )
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
