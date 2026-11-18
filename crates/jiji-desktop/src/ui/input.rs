//! The text field: one entity for single-line inputs (names, revsets,
//! searches, tokens) and multi-line editors (descriptions), with the
//! platform's input method plumbing, selection, clipboard and undo.
//!
//! The entity paints only its text; callers wrap it in whatever chrome the
//! surface needs (see [`field`]) and style font, size and color on the
//! wrapper — the element inherits them.

use std::ops::Range;
use std::time::{Duration, Instant};

use gpui::{
    actions, div, fill, point, prelude::*, px, relative, size, App, AvailableSpace, Bounds,
    ClipboardItem, Context, CursorStyle, Div, Element, ElementId, ElementInputHandler, Entity,
    EntityInputHandler, EventEmitter, FocusHandle, Focusable, GlobalElementId, Hsla,
    InspectorElementId, KeyBinding, KeyContext, LayoutId, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, Pixels, Point, ScrollEvent, SharedString, Style, TextAlign,
    TextRun, UTF16Selection, UnderlineStyle, Window, WrappedLine,
};
use unicode_segmentation::UnicodeSegmentation;

use crate::theme::{theme, RADIUS_M, TEXT_S};

actions!(
    text_input,
    [
        Backspace,
        Delete,
        Left,
        Right,
        SelectLeft,
        SelectRight,
        WordLeft,
        WordRight,
        SelectWordLeft,
        SelectWordRight,
        LineStart,
        LineEnd,
        SelectLineStart,
        SelectLineEnd,
        DocStart,
        DocEnd,
        SelectDocStart,
        SelectDocEnd,
        Up,
        Down,
        SelectUp,
        SelectDown,
        DeleteWordLeft,
        DeleteWordRight,
        DeleteToLineStart,
        SelectAll,
        Copy,
        Cut,
        Paste,
        Undo,
        Redo,
        Enter,
        Submit,
        SubmitBack,
        Escape,
    ]
);

/// Key bindings for every text field in the app.
pub fn bind_keys(cx: &mut App) {
    let any = Some("TextInput");
    let multi = Some("TextInput && multiline");
    cx.bind_keys([
        KeyBinding::new("backspace", Backspace, any),
        KeyBinding::new("shift-backspace", Backspace, any),
        KeyBinding::new("delete", Delete, any),
        KeyBinding::new("ctrl-d", Delete, any),
        KeyBinding::new("ctrl-h", Backspace, any),
        KeyBinding::new("left", Left, any),
        KeyBinding::new("right", Right, any),
        KeyBinding::new("ctrl-b", Left, any),
        KeyBinding::new("ctrl-f", Right, any),
        KeyBinding::new("shift-left", SelectLeft, any),
        KeyBinding::new("shift-right", SelectRight, any),
        KeyBinding::new("alt-left", WordLeft, any),
        KeyBinding::new("alt-right", WordRight, any),
        KeyBinding::new("alt-shift-left", SelectWordLeft, any),
        KeyBinding::new("alt-shift-right", SelectWordRight, any),
        KeyBinding::new("cmd-left", LineStart, any),
        KeyBinding::new("cmd-right", LineEnd, any),
        KeyBinding::new("home", LineStart, any),
        KeyBinding::new("end", LineEnd, any),
        KeyBinding::new("ctrl-a", LineStart, any),
        KeyBinding::new("ctrl-e", LineEnd, any),
        KeyBinding::new("cmd-shift-left", SelectLineStart, any),
        KeyBinding::new("cmd-shift-right", SelectLineEnd, any),
        KeyBinding::new("shift-home", SelectLineStart, any),
        KeyBinding::new("shift-end", SelectLineEnd, any),
        KeyBinding::new("cmd-up", DocStart, any),
        KeyBinding::new("cmd-down", DocEnd, any),
        KeyBinding::new("cmd-shift-up", SelectDocStart, any),
        KeyBinding::new("cmd-shift-down", SelectDocEnd, any),
        KeyBinding::new("up", Up, multi),
        KeyBinding::new("down", Down, multi),
        KeyBinding::new("ctrl-p", Up, multi),
        KeyBinding::new("ctrl-n", Down, multi),
        KeyBinding::new("shift-up", SelectUp, multi),
        KeyBinding::new("shift-down", SelectDown, multi),
        KeyBinding::new("alt-backspace", DeleteWordLeft, any),
        KeyBinding::new("ctrl-w", DeleteWordLeft, any),
        KeyBinding::new("alt-delete", DeleteWordRight, any),
        KeyBinding::new("cmd-backspace", DeleteToLineStart, any),
        KeyBinding::new("ctrl-u", DeleteToLineStart, any),
        KeyBinding::new("cmd-a", SelectAll, any),
        KeyBinding::new("cmd-c", Copy, any),
        KeyBinding::new("cmd-x", Cut, any),
        KeyBinding::new("cmd-v", Paste, any),
        KeyBinding::new("cmd-z", Undo, any),
        KeyBinding::new("cmd-shift-z", Redo, any),
        KeyBinding::new("enter", Enter, any),
        KeyBinding::new("shift-enter", Enter, multi),
        KeyBinding::new("shift-enter", SubmitBack, Some("TextInput && !multiline")),
        KeyBinding::new("cmd-enter", Submit, any),
        KeyBinding::new("escape", Escape, any),
    ]);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputEvent {
    /// The text changed (typing, paste, cut, undo).
    Change,
    /// Enter in a single-line field; ⌘Enter anywhere.
    Submit,
    /// ⇧Enter in a single-line field: the same step, backwards.
    SubmitBack,
    /// Escape.
    Cancel,
    Blur,
}

struct HistoryEntry {
    text: String,
    selection: Range<usize>,
}

pub struct TextInput {
    focus: FocusHandle,
    text: String,
    placeholder: SharedString,
    selection: Range<usize>,
    reversed: bool,
    marked: Option<Range<usize>>,
    multiline: bool,
    min_rows: usize,
    max_rows: usize,
    masked: bool,
    disabled: bool,
    selecting: bool,
    /// Column the caret tries to keep while moving vertically.
    goal_x: Option<Pixels>,
    scroll: Point<Pixels>,
    undo: Vec<HistoryEntry>,
    redo: Vec<HistoryEntry>,
    last_edit: Option<Instant>,
    /// Layout of the last paint, for hit-testing and vertical motion.
    layout: Option<Layout>,
}

struct Layout {
    bounds: Bounds<Pixels>,
    line_height: Pixels,
    lines: Vec<LaidLine>,
    scroll: Point<Pixels>,
    content_height: Pixels,
}

struct LaidLine {
    /// Byte offset of this paragraph in the display text.
    start: usize,
    /// Offset of its top from the content origin.
    top: Pixels,
    line: WrappedLine,
    /// Byte offsets (paragraph-relative) where each visual row begins.
    rows: Vec<usize>,
}

impl LaidLine {
    fn row_of(&self, index: usize) -> usize {
        match self.rows.binary_search(&index) {
            Ok(row) => row,
            Err(row) => row.saturating_sub(1),
        }
    }

    fn row_end(&self, row: usize) -> usize {
        self.rows.get(row + 1).copied().unwrap_or(self.line.len())
    }

    /// X of a paragraph-relative byte offset within its visual row.
    fn x_in_row(&self, index: usize, row: usize) -> Pixels {
        let layout = &self.line.unwrapped_layout;
        layout.x_for_index(index) - layout.x_for_index(self.rows[row])
    }

    fn height(&self, line_height: Pixels) -> Pixels {
        line_height * self.rows.len() as f32
    }
}

impl Layout {
    fn line_for(&self, index: usize) -> usize {
        match self.lines.binary_search_by(|line| line.start.cmp(&index)) {
            Ok(ix) => ix,
            Err(ix) => ix.saturating_sub(1),
        }
    }

    /// Content-space position of a display byte offset's caret.
    fn caret(&self, index: usize) -> Point<Pixels> {
        let Some(line) = self.lines.get(self.line_for(index)) else {
            return Point::default();
        };
        let local = (index - line.start).min(line.line.len());
        let row = line.row_of(local);
        point(
            line.x_in_row(local, row),
            line.top + self.line_height * row as f32,
        )
    }

    /// Display byte offset nearest a content-space position.
    fn index_at(&self, position: Point<Pixels>) -> usize {
        let Some(last) = self.lines.last() else {
            return 0;
        };
        if position.y < px(0.) {
            return 0;
        }
        if position.y >= last.top + last.height(self.line_height) {
            return last.start + last.line.len();
        }
        let ix = self
            .lines
            .partition_point(|line| line.top <= position.y)
            .saturating_sub(1);
        let line = &self.lines[ix];
        let row = (((position.y - line.top) / self.line_height) as usize).min(line.rows.len() - 1);
        let layout = &line.line.unwrapped_layout;
        let x = position.x.max(px(0.)) + layout.x_for_index(line.rows[row]);
        let mut local = layout.closest_index_for_x(x);
        // A click past a wrapped row's end belongs to that row, not the
        // start of the next.
        let end = line.row_end(row);
        if local >= end && row + 1 < line.rows.len() {
            local = prev_boundary(&line.line.text, end);
        }
        line.start + local.clamp(line.rows[row], line.line.len())
    }
}

fn prev_boundary(text: &str, offset: usize) -> usize {
    text[..offset.min(text.len())]
        .grapheme_indices(true)
        .next_back()
        .map_or(0, |(ix, _)| ix)
}

fn next_boundary(text: &str, offset: usize) -> usize {
    text[offset.min(text.len())..]
        .grapheme_indices(true)
        .nth(1)
        .map_or(text.len(), |(ix, _)| offset + ix)
}

fn is_word(piece: &str) -> bool {
    piece.chars().any(|c| c.is_alphanumeric() || c == '_')
}

fn prev_word(text: &str, offset: usize) -> usize {
    text[..offset]
        .split_word_bound_indices()
        .rev()
        .find(|(_, piece)| is_word(piece))
        .map_or(0, |(ix, _)| ix)
}

fn next_word(text: &str, offset: usize) -> usize {
    text[offset..]
        .split_word_bound_indices()
        .find(|(_, piece)| is_word(piece))
        .map_or(text.len(), |(ix, piece)| offset + ix + piece.len())
}

fn word_range(text: &str, offset: usize) -> Range<usize> {
    for (start, piece) in text.split_word_bound_indices() {
        let end = start + piece.len();
        if offset < end || end == text.len() {
            return start..end;
        }
    }
    0..0
}

const MASK: &str = "•";

impl TextInput {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus = cx.focus_handle();
        cx.on_blur(&focus, window, |this, _window, cx| {
            this.selecting = false;
            cx.emit(InputEvent::Blur);
            cx.notify();
        })
        .detach();
        cx.on_focus(&focus, window, |_, _, cx| cx.notify()).detach();
        Self {
            focus,
            text: String::new(),
            placeholder: SharedString::default(),
            selection: 0..0,
            reversed: false,
            marked: None,
            multiline: false,
            min_rows: 1,
            max_rows: 1,
            masked: false,
            disabled: false,
            selecting: false,
            goal_x: None,
            scroll: Point::default(),
            undo: Vec::new(),
            redo: Vec::new(),
            last_edit: None,
            layout: None,
        }
    }

    /// A wrapping editor that grows from `min_rows` to `max_rows`, then
    /// scrolls.
    pub fn multiline(mut self, min_rows: usize, max_rows: usize) -> Self {
        self.multiline = true;
        self.min_rows = min_rows.max(1);
        self.max_rows = max_rows.max(self.min_rows);
        self
    }

    pub fn placeholder(mut self, text: impl Into<SharedString>) -> Self {
        self.placeholder = text.into();
        self
    }

    /// Shows bullets instead of the text (tokens, license keys).
    pub fn masked(mut self) -> Self {
        self.masked = true;
        self
    }

    pub fn with_text(mut self, text: impl Into<String>) -> Self {
        self.text = text.into();
        self.selection = self.text.len()..self.text.len();
        self
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// Replace the content programmatically; no change event is emitted.
    pub fn set_text(&mut self, text: impl Into<String>, cx: &mut Context<Self>) {
        self.text = text.into();
        self.selection = self.text.len()..self.text.len();
        self.reversed = false;
        self.marked = None;
        self.undo.clear();
        self.redo.clear();
        self.scroll = Point::default();
        cx.notify();
    }

    pub fn set_disabled(&mut self, disabled: bool, cx: &mut Context<Self>) {
        self.disabled = disabled;
        cx.notify();
    }

    pub fn select_all_text(&mut self, cx: &mut Context<Self>) {
        self.selection = 0..self.text.len();
        self.reversed = false;
        cx.notify();
    }

    pub fn focus(&self, window: &mut Window, cx: &mut App) {
        window.focus(&self.focus, cx);
    }

    pub fn is_focused(&self, window: &Window) -> bool {
        self.focus.is_focused(window)
    }

    fn cursor(&self) -> usize {
        if self.reversed {
            self.selection.start
        } else {
            self.selection.end
        }
    }

    fn move_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        self.selection = offset..offset;
        self.reversed = false;
        cx.notify();
    }

    fn select_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        if self.reversed {
            self.selection.start = offset;
        } else {
            self.selection.end = offset;
        }
        if self.selection.end < self.selection.start {
            self.reversed = !self.reversed;
            self.selection = self.selection.end..self.selection.start;
        }
        cx.notify();
    }

    fn line_start(&self, offset: usize) -> usize {
        self.text[..offset].rfind('\n').map_or(0, |ix| ix + 1)
    }

    fn line_end(&self, offset: usize) -> usize {
        self.text[offset..]
            .find('\n')
            .map_or(self.text.len(), |ix| offset + ix)
    }

    // ── Display mapping (masked fields show one bullet per character) ──

    fn display_text(&self) -> SharedString {
        if self.masked {
            MASK.repeat(self.text.chars().count()).into()
        } else {
            self.text.clone().into()
        }
    }

    fn display_offset(&self, offset: usize) -> usize {
        if self.masked {
            self.text[..offset].chars().count() * MASK.len()
        } else {
            offset
        }
    }

    fn text_offset(&self, offset: usize) -> usize {
        if self.masked {
            self.text
                .char_indices()
                .nth(offset / MASK.len())
                .map_or(self.text.len(), |(ix, _)| ix)
        } else {
            offset.min(self.text.len())
        }
    }

    // ── Editing ──

    fn remember(&mut self) {
        let fresh = self
            .last_edit
            .is_none_or(|at| at.elapsed() > Duration::from_millis(600));
        if fresh || self.undo.is_empty() {
            self.undo.push(HistoryEntry {
                text: self.text.clone(),
                selection: self.selection.clone(),
            });
            if self.undo.len() > 200 {
                self.undo.remove(0);
            }
        }
        self.redo.clear();
        self.last_edit = Some(Instant::now());
    }

    fn edit(&mut self, range: Range<usize>, new_text: &str, cx: &mut Context<Self>) {
        if self.disabled {
            return;
        }
        let new_text = if self.multiline {
            new_text.replace("\r\n", "\n").replace('\r', "\n")
        } else {
            new_text.replace(['\n', '\r'], " ")
        };
        if range.is_empty() && new_text.is_empty() {
            return;
        }
        self.remember();
        self.text.replace_range(range.clone(), &new_text);
        let caret = range.start + new_text.len();
        self.selection = caret..caret;
        self.reversed = false;
        self.marked = None;
        self.goal_x = None;
        cx.emit(InputEvent::Change);
        cx.notify();
    }

    fn left(&mut self, _: &Left, _: &mut Window, cx: &mut Context<Self>) {
        self.goal_x = None;
        if self.selection.is_empty() {
            self.move_to(prev_boundary(&self.text, self.cursor()), cx);
        } else {
            self.move_to(self.selection.start, cx);
        }
    }

    fn right(&mut self, _: &Right, _: &mut Window, cx: &mut Context<Self>) {
        self.goal_x = None;
        if self.selection.is_empty() {
            self.move_to(next_boundary(&self.text, self.cursor()), cx);
        } else {
            self.move_to(self.selection.end, cx);
        }
    }

    fn select_left(&mut self, _: &SelectLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(prev_boundary(&self.text, self.cursor()), cx);
    }

    fn select_right(&mut self, _: &SelectRight, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(next_boundary(&self.text, self.cursor()), cx);
    }

    fn word_left(&mut self, _: &WordLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(prev_word(&self.text, self.cursor()), cx);
    }

    fn word_right(&mut self, _: &WordRight, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(next_word(&self.text, self.cursor()), cx);
    }

    fn select_word_left(&mut self, _: &SelectWordLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(prev_word(&self.text, self.cursor()), cx);
    }

    fn select_word_right(&mut self, _: &SelectWordRight, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(next_word(&self.text, self.cursor()), cx);
    }

    fn line_start_move(&mut self, _: &LineStart, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.line_start(self.cursor()), cx);
    }

    fn line_end_move(&mut self, _: &LineEnd, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.line_end(self.cursor()), cx);
    }

    fn select_line_start(&mut self, _: &SelectLineStart, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.line_start(self.cursor()), cx);
    }

    fn select_line_end(&mut self, _: &SelectLineEnd, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.line_end(self.cursor()), cx);
    }

    fn doc_start(&mut self, _: &DocStart, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(0, cx);
    }

    fn doc_end(&mut self, _: &DocEnd, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.text.len(), cx);
    }

    fn select_doc_start(&mut self, _: &SelectDocStart, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(0, cx);
    }

    fn select_doc_end(&mut self, _: &SelectDocEnd, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.text.len(), cx);
    }

    /// The offset one visual row above or below the caret.
    fn vertical(&mut self, rows: f32) -> usize {
        let Some(layout) = self.layout.as_ref() else {
            return self.cursor();
        };
        let caret = layout.caret(self.cursor());
        let x = *self.goal_x.get_or_insert(caret.x);
        let y = caret.y + layout.line_height * (rows + 0.5);
        if y < px(0.) {
            return 0;
        }
        if y > layout.content_height {
            return self.text.len();
        }
        layout.index_at(point(x, y))
    }

    fn up(&mut self, _: &Up, _: &mut Window, cx: &mut Context<Self>) {
        let target = self.vertical(-1.);
        let goal = self.goal_x;
        self.move_to(target, cx);
        self.goal_x = goal;
    }

    fn down(&mut self, _: &Down, _: &mut Window, cx: &mut Context<Self>) {
        let target = self.vertical(1.);
        let goal = self.goal_x;
        self.move_to(target, cx);
        self.goal_x = goal;
    }

    fn select_up(&mut self, _: &SelectUp, _: &mut Window, cx: &mut Context<Self>) {
        let target = self.vertical(-1.);
        self.select_to(target, cx);
    }

    fn select_down(&mut self, _: &SelectDown, _: &mut Window, cx: &mut Context<Self>) {
        let target = self.vertical(1.);
        self.select_to(target, cx);
    }

    fn backspace(&mut self, _: &Backspace, _: &mut Window, cx: &mut Context<Self>) {
        let range = if self.selection.is_empty() {
            prev_boundary(&self.text, self.cursor())..self.cursor()
        } else {
            self.selection.clone()
        };
        self.edit(range, "", cx);
    }

    fn delete(&mut self, _: &Delete, _: &mut Window, cx: &mut Context<Self>) {
        let range = if self.selection.is_empty() {
            self.cursor()..next_boundary(&self.text, self.cursor())
        } else {
            self.selection.clone()
        };
        self.edit(range, "", cx);
    }

    fn delete_word_left(&mut self, _: &DeleteWordLeft, _: &mut Window, cx: &mut Context<Self>) {
        let range = if self.selection.is_empty() {
            prev_word(&self.text, self.cursor())..self.cursor()
        } else {
            self.selection.clone()
        };
        self.edit(range, "", cx);
    }

    fn delete_word_right(&mut self, _: &DeleteWordRight, _: &mut Window, cx: &mut Context<Self>) {
        let range = if self.selection.is_empty() {
            self.cursor()..next_word(&self.text, self.cursor())
        } else {
            self.selection.clone()
        };
        self.edit(range, "", cx);
    }

    fn delete_to_line_start(
        &mut self,
        _: &DeleteToLineStart,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = if self.selection.is_empty() {
            self.line_start(self.cursor())..self.cursor()
        } else {
            self.selection.clone()
        };
        self.edit(range, "", cx);
    }

    fn select_all(&mut self, _: &SelectAll, _: &mut Window, cx: &mut Context<Self>) {
        self.select_all_text(cx);
    }

    fn copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        if !self.selection.is_empty() && !self.masked {
            cx.write_to_clipboard(ClipboardItem::new_string(
                self.text[self.selection.clone()].to_owned(),
            ));
        }
    }

    fn cut(&mut self, _: &Cut, _: &mut Window, cx: &mut Context<Self>) {
        if !self.selection.is_empty() && !self.masked {
            cx.write_to_clipboard(ClipboardItem::new_string(
                self.text[self.selection.clone()].to_owned(),
            ));
            self.edit(self.selection.clone(), "", cx);
        }
    }

    fn paste(&mut self, _: &Paste, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
            // One undo step per paste, whatever came just before.
            self.last_edit = None;
            self.edit(self.selection.clone(), &text, cx);
            self.last_edit = None;
        }
    }

    fn swap_history(&mut self, undo: bool, cx: &mut Context<Self>) {
        let (from, to) = if undo {
            (&mut self.undo, &mut self.redo)
        } else {
            (&mut self.redo, &mut self.undo)
        };
        let Some(entry) = from.pop() else {
            return;
        };
        to.push(HistoryEntry {
            text: std::mem::replace(&mut self.text, entry.text),
            selection: std::mem::replace(&mut self.selection, entry.selection),
        });
        self.reversed = false;
        self.marked = None;
        self.last_edit = None;
        cx.emit(InputEvent::Change);
        cx.notify();
    }

    fn undo(&mut self, _: &Undo, _: &mut Window, cx: &mut Context<Self>) {
        self.swap_history(true, cx);
    }

    fn redo(&mut self, _: &Redo, _: &mut Window, cx: &mut Context<Self>) {
        self.swap_history(false, cx);
    }

    fn enter(&mut self, _: &Enter, _: &mut Window, cx: &mut Context<Self>) {
        if self.multiline {
            self.edit(self.selection.clone(), "\n", cx);
        } else {
            cx.emit(InputEvent::Submit);
        }
    }

    fn submit(&mut self, _: &Submit, _: &mut Window, cx: &mut Context<Self>) {
        cx.emit(InputEvent::Submit);
    }

    fn submit_back(&mut self, _: &SubmitBack, _: &mut Window, cx: &mut Context<Self>) {
        cx.emit(InputEvent::SubmitBack);
    }

    fn escape(&mut self, _: &Escape, _: &mut Window, cx: &mut Context<Self>) {
        cx.emit(InputEvent::Cancel);
        // Surfaces that close on Escape bind it themselves too.
        cx.propagate();
    }

    // ── Pointer ──

    fn index_for_mouse(&self, position: Point<Pixels>) -> usize {
        let Some(layout) = self.layout.as_ref() else {
            return 0;
        };
        let local = position - layout.bounds.origin + layout.scroll;
        self.text_offset(layout.index_at(local))
    }

    fn on_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.focus, cx);
        self.goal_x = None;
        let offset = self.index_for_mouse(event.position);
        match event.click_count {
            2 => {
                let range = word_range(&self.text, offset);
                self.selection = range;
                self.reversed = false;
                cx.notify();
                return;
            }
            n if n >= 3 => {
                self.selection = self.line_start(offset)..self.line_end(offset);
                self.reversed = false;
                cx.notify();
                return;
            }
            _ => {}
        }
        self.selecting = true;
        if event.modifiers.shift {
            self.select_to(offset, cx);
        } else {
            self.move_to(offset, cx);
        }
    }

    /// Right-click: the clipboard menu, on the selection as it stands.
    fn on_context(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        cx.stop_propagation();
        if self.disabled {
            return;
        }
        window.focus(&self.focus, cx);
        // A masked field never gives its text away.
        let has_selection = !self.selection.is_empty() && !self.masked;
        let position = event.position;
        crate::app::app(cx).update(cx, |model, cx| {
            model.open_editable_menu(has_selection, false, position, cx)
        });
    }

    fn on_mouse_up(&mut self, _: &MouseUpEvent, _: &mut Window, _: &mut Context<Self>) {
        self.selecting = false;
    }

    fn on_mouse_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.selecting {
            if event.pressed_button != Some(MouseButton::Left) {
                self.selecting = false;
                return;
            }
            self.select_to(self.index_for_mouse(event.position), cx);
        }
    }

    fn on_scroll(&mut self, event: &ScrollEvent, window: &mut Window, cx: &mut Context<Self>) {
        let Some(layout) = self.layout.as_ref() else {
            return;
        };
        let max = (layout.content_height - layout.bounds.size.height).max(px(0.));
        if !self.multiline || max <= px(0.) {
            return;
        }
        let delta = event.delta.pixel_delta(window.line_height());
        let next = (self.scroll.y - delta.y).clamp(px(0.), max);
        if next != self.scroll.y {
            self.scroll.y = next;
            cx.stop_propagation();
            cx.notify();
        }
    }

    // ── UTF-16 plumbing for the platform input method ──

    fn offset_from_utf16(&self, offset: usize) -> usize {
        let mut utf8 = 0;
        let mut utf16 = 0;
        for ch in self.text.chars() {
            if utf16 >= offset {
                break;
            }
            utf16 += ch.len_utf16();
            utf8 += ch.len_utf8();
        }
        utf8
    }

    fn offset_to_utf16(&self, offset: usize) -> usize {
        self.text[..offset.min(self.text.len())]
            .chars()
            .map(char::len_utf16)
            .sum()
    }

    fn range_to_utf16(&self, range: &Range<usize>) -> Range<usize> {
        self.offset_to_utf16(range.start)..self.offset_to_utf16(range.end)
    }

    fn range_from_utf16(&self, range: &Range<usize>) -> Range<usize> {
        self.offset_from_utf16(range.start)..self.offset_from_utf16(range.end)
    }
}

impl EventEmitter<InputEvent> for TextInput {}

impl Focusable for TextInput {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl EntityInputHandler for TextInput {
    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        actual_range: &mut Option<Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        let range = self.range_from_utf16(&range_utf16);
        actual_range.replace(self.range_to_utf16(&range));
        Some(self.text[range].to_owned())
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: self.range_to_utf16(&self.selection),
            reversed: self.reversed,
        })
    }

    fn marked_text_range(
        &self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Range<usize>> {
        self.marked.as_ref().map(|range| self.range_to_utf16(range))
    }

    fn unmark_text(&mut self, _window: &mut Window, _cx: &mut Context<Self>) {
        self.marked = None;
    }

    fn replace_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = range_utf16
            .as_ref()
            .map(|range| self.range_from_utf16(range))
            .or(self.marked.clone())
            .unwrap_or(self.selection.clone());
        self.edit(range, new_text, cx);
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        new_selected_range_utf16: Option<Range<usize>>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.disabled {
            return;
        }
        let range = range_utf16
            .as_ref()
            .map(|range| self.range_from_utf16(range))
            .or(self.marked.clone())
            .unwrap_or(self.selection.clone());
        self.remember();
        self.text.replace_range(range.clone(), new_text);
        self.marked = (!new_text.is_empty()).then(|| range.start..range.start + new_text.len());
        self.selection = new_selected_range_utf16
            .as_ref()
            .map(|selected| {
                // The selection arrives relative to the inserted text.
                let utf8 = |offset: usize| {
                    let mut taken = 0;
                    let mut bytes = 0;
                    for ch in new_text.chars() {
                        if taken >= offset {
                            break;
                        }
                        taken += ch.len_utf16();
                        bytes += ch.len_utf8();
                    }
                    bytes
                };
                range.start + utf8(selected.start)..range.start + utf8(selected.end)
            })
            .unwrap_or_else(|| range.start + new_text.len()..range.start + new_text.len());
        self.reversed = false;
        cx.emit(InputEvent::Change);
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        bounds: Bounds<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let layout = self.layout.as_ref()?;
        let range = self.range_from_utf16(&range_utf16);
        let start = layout.caret(self.display_offset(range.start)) - layout.scroll;
        let end = layout.caret(self.display_offset(range.end)) - layout.scroll;
        Some(Bounds::from_corners(
            bounds.origin + start,
            bounds.origin + point(end.x, end.y + layout.line_height),
        ))
    }

    fn character_index_for_point(
        &mut self,
        point: Point<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        Some(self.offset_to_utf16(self.index_for_mouse(point)))
    }
}

impl Render for TextInput {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut context = KeyContext::new_with_defaults(cx.platform_info().os);
        context.add("TextInput");
        if self.multiline {
            context.add("multiline");
        }
        div()
            .flex()
            .flex_1()
            .min_w_0()
            .key_context(context)
            .track_focus(&self.focus)
            .cursor(CursorStyle::IBeam)
            .on_action(cx.listener(Self::backspace))
            .on_action(cx.listener(Self::delete))
            .on_action(cx.listener(Self::left))
            .on_action(cx.listener(Self::right))
            .on_action(cx.listener(Self::select_left))
            .on_action(cx.listener(Self::select_right))
            .on_action(cx.listener(Self::word_left))
            .on_action(cx.listener(Self::word_right))
            .on_action(cx.listener(Self::select_word_left))
            .on_action(cx.listener(Self::select_word_right))
            .on_action(cx.listener(Self::line_start_move))
            .on_action(cx.listener(Self::line_end_move))
            .on_action(cx.listener(Self::select_line_start))
            .on_action(cx.listener(Self::select_line_end))
            .on_action(cx.listener(Self::doc_start))
            .on_action(cx.listener(Self::doc_end))
            .on_action(cx.listener(Self::select_doc_start))
            .on_action(cx.listener(Self::select_doc_end))
            .on_action(cx.listener(Self::up))
            .on_action(cx.listener(Self::down))
            .on_action(cx.listener(Self::select_up))
            .on_action(cx.listener(Self::select_down))
            .on_action(cx.listener(Self::delete_word_left))
            .on_action(cx.listener(Self::delete_word_right))
            .on_action(cx.listener(Self::delete_to_line_start))
            .on_action(cx.listener(Self::select_all))
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::cut))
            .on_action(cx.listener(Self::paste))
            .on_action(cx.listener(Self::undo))
            .on_action(cx.listener(Self::redo))
            .on_action(cx.listener(Self::enter))
            .on_action(cx.listener(Self::submit))
            .on_action(cx.listener(Self::submit_back))
            .on_action(cx.listener(Self::escape))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_down(MouseButton::Right, cx.listener(Self::on_context))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .on_scroll(cx.listener(Self::on_scroll))
            .child(TextElement { input: cx.entity() })
    }
}

/// A text field's standard chrome around an input: recessed, hairline,
/// accent border while focused.
pub fn field(input: &Entity<TextInput>, window: &Window, cx: &App) -> Div {
    let t = theme(cx);
    let state = input.read(cx);
    let focused = state.is_focused(window);
    let multiline = state.multiline;
    let mut el = div()
        .flex()
        .flex_row()
        .min_w_0()
        .px(px(8.))
        .text_size(px(TEXT_S))
        .line_height(px(18.))
        .text_color(t.text1)
        .bg(t.bg_well)
        .border_1()
        .border_color(if focused { t.accent } else { t.border1 })
        .rounded(px(RADIUS_M))
        .child(input.clone());
    if multiline {
        el = el.py(px(6.));
    } else {
        el = el.h(px(28.)).items_center();
    }
    if state.disabled {
        el = el.opacity(0.6);
    }
    el
}

struct TextElement {
    input: Entity<TextInput>,
}

struct Prepaint {
    layout: Layout,
    selections: Vec<Bounds<Pixels>>,
    caret: Option<Bounds<Pixels>>,
    selection_color: Hsla,
    caret_color: Hsla,
}

/// Shape the display text and locate each paragraph's visual rows.
fn lay_out(
    text: SharedString,
    runs: &[TextRun],
    font_size: Pixels,
    wrap_width: Option<Pixels>,
    line_height: Pixels,
    window: &mut Window,
) -> (Vec<LaidLine>, Pixels) {
    let shaped = window
        .text_system()
        .shape_text(text, font_size, runs, wrap_width, None)
        .unwrap_or_default();
    let mut lines = Vec::with_capacity(shaped.len());
    let mut start = 0;
    let mut top = px(0.);
    for line in shaped {
        let mut rows = vec![0];
        for boundary in line.wrap_boundaries() {
            let glyph = &line.unwrapped_layout.runs[boundary.run_ix].glyphs[boundary.glyph_ix];
            rows.push(glyph.index);
        }
        let len = line.len();
        let laid = LaidLine {
            start,
            top,
            line,
            rows,
        };
        top += laid.height(line_height);
        start += len + 1;
        lines.push(laid);
    }
    (lines, top)
}

impl Element for TextElement {
    type RequestLayoutState = ();
    type PrepaintState = Prepaint;

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
            ..Style::default()
        };
        style.size.width = relative(1.).into();
        let line_height = window.line_height();
        let input = self.input.read(cx);
        if !input.multiline {
            style.size.height = line_height.into();
            return (window.request_layout(style, [], cx), ());
        }
        let text_style = window.text_style();
        let font_size = text_style.font_size.to_pixels(window.rem_size());
        let font = text_style.font();
        let (min_rows, max_rows) = (input.min_rows, input.max_rows);
        let entity = self.input.clone();
        let id = window.request_measured_layout(style, move |known, available, window, cx| {
            let width = known.width.or(match available.width {
                AvailableSpace::Definite(width) => Some(width),
                _ => None,
            });
            let text = entity.read(cx).display_text();
            let run = TextRun {
                len: text.len(),
                font: font.clone(),
                color: Hsla::default(),
                background_color: None,
                underline: None,
                strikethrough: None,
            };
            let (lines, _) = lay_out(text, &[run], font_size, width, line_height, window);
            let rows: usize = lines.iter().map(|line| line.rows.len()).sum();
            size(
                width.unwrap_or(px(200.)),
                line_height * rows.clamp(min_rows, max_rows) as f32,
            )
        });
        (id, ())
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
        let t = theme(cx);
        let line_height = window.line_height();
        let text_style = window.text_style();
        let font_size = text_style.font_size.to_pixels(window.rem_size());
        let input = self.input.read(cx);
        let focused = input.focus.is_focused(window);
        let empty = input.text.is_empty();
        let (text, color) = if empty {
            (input.placeholder.clone(), t.text3)
        } else {
            (input.display_text(), text_style.color)
        };
        let base = TextRun {
            len: text.len(),
            font: text_style.font(),
            color,
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let marked = input
            .marked
            .as_ref()
            .filter(|_| !empty)
            .map(|range| input.display_offset(range.start)..input.display_offset(range.end));
        let runs: Vec<TextRun> = match marked {
            Some(marked) => [
                TextRun {
                    len: marked.start,
                    ..base.clone()
                },
                TextRun {
                    len: marked.end - marked.start,
                    underline: Some(UnderlineStyle {
                        color: Some(color),
                        thickness: px(1.),
                        wavy: false,
                    }),
                    ..base.clone()
                },
                TextRun {
                    len: text.len() - marked.end,
                    ..base
                },
            ]
            .into_iter()
            .filter(|run| run.len > 0)
            .collect(),
            None => vec![base],
        };
        let wrap = input.multiline.then_some(bounds.size.width);
        let (lines, content_height) = lay_out(text, &runs, font_size, wrap, line_height, window);
        let mut layout = Layout {
            bounds,
            line_height,
            lines,
            scroll: input.scroll,
            content_height,
        };

        let selection = if empty {
            0..0
        } else {
            input.display_offset(input.selection.start)..input.display_offset(input.selection.end)
        };
        let caret_index = if empty {
            0
        } else {
            input.display_offset(input.cursor())
        };
        let caret = layout.caret(caret_index);

        // Keep the caret in view: sideways in a single-line field, up and
        // down in an editor.
        if focused {
            if input.multiline {
                let max = (content_height - bounds.size.height).max(px(0.));
                if caret.y < layout.scroll.y {
                    layout.scroll.y = caret.y;
                } else if caret.y + line_height > layout.scroll.y + bounds.size.height {
                    layout.scroll.y = caret.y + line_height - bounds.size.height;
                }
                layout.scroll.y = layout.scroll.y.clamp(px(0.), max);
            } else {
                let width = bounds.size.width;
                let content = layout.lines.first().map_or(px(0.), |l| l.line.width());
                let max = (content + px(2.) - width).max(px(0.));
                if caret.x < layout.scroll.x {
                    layout.scroll.x = caret.x;
                } else if caret.x > layout.scroll.x + width - px(2.) {
                    layout.scroll.x = caret.x - width + px(2.);
                }
                layout.scroll.x = layout.scroll.x.clamp(px(0.), max);
            }
        } else if !input.multiline {
            layout.scroll.x = px(0.);
        }

        let origin = bounds.origin - layout.scroll;
        let mut selections = Vec::new();
        if !selection.is_empty() {
            for line in &layout.lines {
                let end = line.start + line.line.len();
                if selection.end < line.start || selection.start > end {
                    continue;
                }
                let from = selection.start.max(line.start) - line.start;
                let to = selection.end.min(end) - line.start;
                for row in line.row_of(from)..=line.row_of(to) {
                    let row_start = line.rows[row];
                    let row_end = line.row_end(row);
                    let a = from.max(row_start);
                    let b = to.min(row_end);
                    if a > b {
                        continue;
                    }
                    let mut right = line.x_in_row(b, row);
                    // A selected line break shows as a sliver past the
                    // row's last glyph.
                    if (b == row_end && to > row_end)
                        || (b == line.line.len() && selection.end > end)
                    {
                        right += px(4.);
                    }
                    let left = line.x_in_row(a, row);
                    if right <= left {
                        continue;
                    }
                    selections.push(Bounds::from_corners(
                        origin + point(left, line.top + line_height * row as f32),
                        origin + point(right, line.top + line_height * (row + 1) as f32),
                    ));
                }
            }
        }
        let caret = (focused && selection.is_empty() && !input.disabled).then(|| {
            Bounds::new(
                origin + point(caret.x, caret.y + px(1.)),
                size(px(1.5), line_height - px(2.)),
            )
        });
        Prepaint {
            layout,
            selections,
            caret,
            selection_color: t.selection,
            caret_color: t.accent_strong,
        }
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let focus = self.input.read(cx).focus.clone();
        window.handle_input(
            &focus,
            ElementInputHandler::new(bounds, self.input.clone()),
            cx,
        );
        let layout = &prepaint.layout;
        let origin = bounds.origin - layout.scroll;
        let line_height = layout.line_height;
        window.with_content_mask(Some(gpui::ContentMask { bounds }), |window| {
            for selection in &prepaint.selections {
                window.paint_quad(fill(*selection, prepaint.selection_color));
            }
            for line in &layout.lines {
                let top = origin.y + line.top;
                if top > bounds.bottom() || top + line.height(line_height) < bounds.top() {
                    continue;
                }
                let _ = line.line.paint(
                    point(origin.x, top),
                    line_height,
                    TextAlign::Left,
                    None,
                    window,
                    cx,
                );
            }
            if let Some(caret) = prepaint.caret {
                window.paint_quad(fill(caret, prepaint.caret_color));
            }
        });
        let layout = Layout {
            bounds,
            line_height,
            lines: std::mem::take(&mut prepaint.layout.lines),
            scroll: prepaint.layout.scroll,
            content_height: prepaint.layout.content_height,
        };
        self.input.update(cx, |input, _| {
            input.scroll = layout.scroll;
            input.layout = Some(layout);
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grapheme_boundaries_step_over_clusters() {
        let text = "ae\u{301}🇨🇦z";
        assert_eq!(next_boundary(text, 0), 1);
        assert_eq!(next_boundary(text, 1), 4);
        assert_eq!(next_boundary(text, 4), 12);
        assert_eq!(prev_boundary(text, 12), 4);
        assert_eq!(prev_boundary(text, 0), 0);
        assert_eq!(next_boundary(text, text.len()), text.len());
    }

    #[test]
    fn word_motion_skips_punctuation_and_space() {
        let text = "fix(graph): lane colors";
        assert_eq!(next_word(text, 0), 3);
        assert_eq!(next_word(text, 3), 9);
        assert_eq!(next_word(text, 9), 16);
        assert_eq!(prev_word(text, text.len()), 17);
        assert_eq!(prev_word(text, 12), 4);
        assert_eq!(prev_word(text, 2), 0);
    }

    #[test]
    fn double_click_selects_the_word_under_the_pointer() {
        let text = "push main now";
        assert_eq!(word_range(text, 6), 5..9);
        assert_eq!(word_range(text, 0), 0..4);
        assert_eq!(word_range(text, text.len()), 10..13);
    }
}
