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
    MouseMoveEvent, MouseUpEvent, Pixels, Point, Role, ScrollEvent, SharedString, Style, TextAlign,
    TextRun, UTF16Selection, UnderlineStyle, Window,
};

use super::spelling::{self, Spelling};
use super::textlayout::{
    lay_out, next_boundary, next_word, prev_boundary, prev_word, word_range, Layout,
};
use super::{keyboard_on, NodeExt as _};
use crate::logic::menu::SpellChoice;
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
        Shorter,
        Taller,
        Shortest,
        Tallest,
    ]
);

/// The most rows a height set by hand runs to.
const MOST_ROWS: usize = 24;

/// A choice from a misspelled word's menu, for the field it was made in.
#[derive(Clone, PartialEq, Debug, gpui::Action)]
#[action(namespace = text_input, no_json)]
pub struct Spell(pub SpellChoice);

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
        KeyBinding::new("up", Shorter, Some("FieldGrip")),
        KeyBinding::new("down", Taller, Some("FieldGrip")),
        KeyBinding::new("home", Shortest, Some("FieldGrip")),
        KeyBinding::new("end", Tallest, Some("FieldGrip")),
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
    /// What the field is for, for assistive technology, when its
    /// placeholder or the words beside it do not say.
    label: Option<SharedString>,
    selection: Range<usize>,
    reversed: bool,
    marked: Option<Range<usize>>,
    multiline: bool,
    min_rows: usize,
    max_rows: usize,
    /// A height set by hand with the grip, in rows; the text's own while
    /// `None`.
    rows: Option<usize>,
    /// The grip is being dragged: where the pointer took it, and the rows
    /// then.
    resizing: Option<(Pixels, usize)>,
    grip_focus: FocusHandle,
    /// The system's spell checker and what it flagged, in a field that
    /// asked for it.
    spelling: Option<Spelling>,
    /// The misspelled word a menu was opened on.
    spell_target: Option<Range<usize>>,
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

const MASK: &str = "•";

impl TextInput {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus = cx.focus_handle().tab_stop(true);
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
            label: None,
            selection: 0..0,
            reversed: false,
            marked: None,
            multiline: false,
            min_rows: 1,
            max_rows: 1,
            rows: None,
            resizing: None,
            grip_focus: cx.focus_handle().tab_stop(true),
            spelling: None,
            spell_target: None,
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

    pub fn label(mut self, text: impl Into<SharedString>) -> Self {
        self.label = Some(text.into());
        self
    }

    /// Underline misspellings through the system's spell checker, and
    /// offer its guesses on a right-click, as the user's spelling settings
    /// have it. Prose fields only: names, revsets and keys are not words.
    pub fn spell_checked(mut self) -> Self {
        self.spelling = spelling::system().map(Spelling::new);
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
        if let Some(spelling) = &mut self.spelling {
            spelling.typed_to = None;
        }
        self.check_spelling(cx);
        cx.notify();
    }

    /// Hold the editor at `rows` (within its limits), or give its height
    /// back to the text.
    pub fn set_rows(&mut self, rows: Option<usize>, cx: &mut Context<Self>) {
        let (least, most) = self.row_limits();
        self.rows = rows.map(|rows| rows.clamp(least, most));
        cx.notify();
    }

    /// The rows a height set by hand runs between.
    fn row_limits(&self) -> (usize, usize) {
        (self.min_rows, MOST_ROWS.max(self.max_rows))
    }

    /// The rows on screen, as last painted.
    fn rows_shown(&self) -> usize {
        self.rows.unwrap_or_else(|| {
            self.layout.as_ref().map_or(self.min_rows, |layout| {
                (layout.bounds.size.height / layout.line_height).round() as usize
            })
        })
    }

    fn nudge_rows(&mut self, by: isize, cx: &mut Context<Self>) {
        let rows = (self.rows_shown() as isize).saturating_add(by).max(1) as usize;
        self.set_rows(Some(rows), cx);
    }

    #[cfg(test)]
    pub fn is_spell_checked(&self) -> bool {
        self.spelling.is_some()
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
        self.left_word();
        cx.notify();
    }

    /// The caret moved on: the word it was typing is finished.
    fn left_word(&mut self) {
        if let Some(spelling) = &mut self.spelling {
            spelling.typed_to = None;
        }
    }

    /// Check the text again once typing pauses.
    fn respell(&mut self, cx: &mut Context<Self>) {
        let Some(spelling) = &mut self.spelling else {
            return;
        };
        spelling.pending = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(spelling::PAUSE).await;
            let _ = this.update(cx, |this, cx| this.check_spelling(cx));
        }));
    }

    fn check_spelling(&mut self, cx: &mut Context<Self>) {
        // Mid-composition the text is not the user's yet; the composition's
        // end is an edit, which checks again.
        if self.marked.is_some() {
            return;
        }
        let Some(spelling) = &mut self.spelling else {
            return;
        };
        spelling.flagged = spelling.speller.misspelled(&self.text);
        cx.notify();
    }

    fn select_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        self.left_word();
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
        if let Some(spelling) = &mut self.spelling {
            spelling.edited(range, &new_text);
        }
        self.respell(cx);
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
        self.left_word();
        self.check_spelling(cx);
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

    /// Right-click: the clipboard menu, on the selection as it stands. On
    /// a misspelled word it takes the word and leads with the checker's
    /// guesses, as Mac text does.
    fn on_context(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        cx.stop_propagation();
        if self.disabled {
            return;
        }
        window.focus(&self.focus, cx);
        let offset = self.index_for_mouse(event.position);
        self.spell_target = self
            .spelling
            .as_ref()
            .and_then(|spelling| spelling.word_at(offset));
        let guesses = self.spell_target.clone().map(|word| {
            self.selection = word.clone();
            self.reversed = false;
            cx.notify();
            self.spelling
                .as_ref()
                .map(|spelling| spelling.speller.guesses(&self.text, word))
                .unwrap_or_default()
        });
        // A masked field never gives its text away.
        let has_selection = !self.selection.is_empty() && !self.masked;
        let position = event.position;
        crate::app::app(cx).update(cx, |model, cx| {
            model.open_editable_menu(has_selection, false, guesses, position, cx)
        });
    }

    fn spell(&mut self, action: &Spell, _: &mut Window, cx: &mut Context<Self>) {
        let Some(word) = self
            .spell_target
            .take()
            .filter(|word| word.end <= self.text.len())
        else {
            return;
        };
        let Some(speller) = self.spelling.as_ref().map(|s| s.speller.clone()) else {
            return;
        };
        match &action.0 {
            SpellChoice::Use(guess) => {
                // One undo step, whatever came just before.
                self.last_edit = None;
                self.edit(word, guess, cx);
                self.last_edit = None;
                self.check_spelling(cx);
            }
            SpellChoice::Ignore => {
                speller.ignore(&self.text[word]);
                self.check_spelling(cx);
            }
            SpellChoice::Learn => {
                speller.learn(&self.text[word]);
                self.check_spelling(cx);
            }
        }
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
        if let Some(spelling) = &mut self.spelling {
            spelling.edited(range, new_text);
        }
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
        // A disabled field is passed over by Tab, as a disabled control is.
        let focus = self.focus.clone().tab_stop(!self.disabled);
        let role = if self.masked {
            Role::PasswordInput
        } else if self.multiline {
            Role::MultilineTextInput
        } else {
            Role::TextInput
        };
        let mut field = div().id("text-input").role(role);
        if let Some(label) = &self.label {
            field = field.aria_label(label.clone());
        }
        if !self.placeholder.is_empty() {
            field = field.aria_placeholder(self.placeholder.clone());
        }
        // A key's bullets are all anyone sees; nothing reads it out.
        if !self.masked {
            field = field.aria_value(self.text.clone());
        }
        if self.disabled {
            field = field.a11y(|node| node.set_disabled());
        }
        field
            .flex()
            .flex_1()
            .min_w_0()
            .key_context(context)
            .track_focus(&focus)
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
            .on_action(cx.listener(Self::spell))
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

/// The handle along a multi-line field's bottom edge that sets its height
/// by hand, as a text area's resize corner does: drag it, or Tab to it and
/// move it with ↑ and ↓ (Home and End for its limits); a double-click
/// gives the height back to the text. The draft is never touched. Put it in
/// the field's `relative()` chrome, after the input.
pub fn grip(
    input: &Entity<TextInput>,
    name: impl Into<SharedString>,
    window: &Window,
    cx: &App,
) -> impl IntoElement {
    let t = theme(cx);
    let state = input.read(cx);
    let (least, most) = state.row_limits();
    let lit = keyboard_on(&state.grip_focus, window) || state.resizing.is_some();
    let (ink, accent) = (t.text3, t.accent);
    let nudge = |by: isize| {
        let input = input.clone();
        move |_: &dyn gpui::Action, _: &mut Window, cx: &mut App| {
            input.update(cx, |input, cx| input.nudge_rows(by, cx))
        }
    };
    let (press, release, drag, reset) =
        (input.clone(), input.clone(), input.clone(), input.clone());
    div()
        .id("field-grip")
        .key_context("FieldGrip")
        .track_focus(&state.grip_focus)
        .role(Role::Splitter)
        .aria_label(name)
        .aria_orientation(gpui::Orientation::Horizontal)
        .aria_numeric_value(state.rows_shown() as f64)
        .aria_min_numeric_value(least as f64)
        .aria_max_numeric_value(most as f64)
        .aria_numeric_value_step(1.)
        .absolute()
        .left_0()
        .right_0()
        .bottom(px(-4.))
        .h(px(9.))
        .cursor(CursorStyle::ResizeUpDown)
        .on_action({
            let nudge = nudge(-1);
            move |action: &Shorter, window, cx| nudge(action, window, cx)
        })
        .on_action({
            let nudge = nudge(1);
            move |action: &Taller, window, cx| nudge(action, window, cx)
        })
        .on_action({
            let nudge = nudge(-(MOST_ROWS as isize));
            move |action: &Shortest, window, cx| nudge(action, window, cx)
        })
        .on_action({
            let nudge = nudge(MOST_ROWS as isize);
            move |action: &Tallest, window, cx| nudge(action, window, cx)
        })
        // A press drags; it never takes the keys, as no control's press
        // does.
        .on_mouse_down(MouseButton::Left, move |event, window, cx| {
            window.prevent_default();
            press.update(cx, |input, cx| {
                input.resizing = Some((event.position.y, input.rows_shown()));
                cx.notify();
            });
        })
        .on_click(move |event, _, cx| {
            if event.click_count() == 2 {
                reset.update(cx, |input, cx| input.set_rows(None, cx));
            }
        })
        .child(
            gpui::canvas(
                |_, _, _| (),
                move |bounds, (), window, cx| {
                    // Moves and releases are taken window-wide while the
                    // grip is held, so it follows a pointer that leaves it.
                    if drag.read(cx).resizing.is_some() {
                        let drag = drag.clone();
                        window.on_mouse_event(move |event: &MouseMoveEvent, phase, _, cx| {
                            if phase != gpui::DispatchPhase::Bubble {
                                return;
                            }
                            drag.update(cx, |input, cx| {
                                let Some((from, rows)) = input.resizing else {
                                    return;
                                };
                                if event.pressed_button != Some(MouseButton::Left) {
                                    input.resizing = None;
                                    cx.notify();
                                    return;
                                }
                                let line = input
                                    .layout
                                    .as_ref()
                                    .map_or(px(18.), |layout| layout.line_height);
                                let by = ((event.position.y - from) / line).round() as isize;
                                input.set_rows(Some((rows as isize + by).max(1) as usize), cx);
                            });
                        });
                        let release = release.clone();
                        window.on_mouse_event(move |event: &MouseUpEvent, phase, _, cx| {
                            if phase == gpui::DispatchPhase::Bubble
                                && event.button == MouseButton::Left
                            {
                                release.update(cx, |input, cx| {
                                    input.resizing = None;
                                    cx.notify();
                                });
                            }
                        });
                    }
                    // While held, the field's bottom edge is the accent's
                    // rule; the corner always carries the grip's mark.
                    if lit {
                        let inset = px(RADIUS_M);
                        window.paint_quad(fill(
                            Bounds::new(
                                point(bounds.left() + inset, bounds.center().y - px(1.)),
                                size(bounds.size.width - inset * 2., px(2.)),
                            ),
                            accent,
                        ));
                    }
                    let corner = point(bounds.right() - px(4.), bounds.center().y - px(1.));
                    for reach in [9., 5.] {
                        let mut path = gpui::PathBuilder::stroke(px(1.));
                        path.move_to(point(corner.x - px(reach), corner.y));
                        path.line_to(point(corner.x, corner.y - px(reach)));
                        if let Ok(path) = path.build() {
                            window.paint_path(path, ink);
                        }
                    }
                },
            )
            .size_full(),
        )
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
    /// The runs of misspelled words, and how far down a run its baseline
    /// sits.
    misspelled: Vec<Bounds<Pixels>>,
    baseline: Pixels,
    misspelled_color: Hsla,
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
        let (min_rows, max_rows, held) = (input.min_rows, input.max_rows, input.rows);
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
            let rows = held.unwrap_or_else(|| {
                let (lines, _) = lay_out(text, &[run], font_size, width, line_height, window);
                let rows: usize = lines.iter().map(|line| line.rows.len()).sum();
                rows.clamp(min_rows, max_rows)
            });
            size(width.unwrap_or(px(200.)), line_height * rows as f32)
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
        let selections = layout.boxes(selection.clone(), origin);
        let caret = (focused && selection.is_empty() && !input.disabled).then(|| {
            Bounds::new(
                origin + point(caret.x, caret.y + px(1.)),
                size(px(1.5), line_height - px(2.)),
            )
        });
        let misspelled = match &input.spelling {
            Some(spelling) if !empty => spelling
                .shown()
                .flat_map(|word| layout.boxes(word, origin))
                .collect(),
            _ => Vec::new(),
        };
        // Where a row's baseline sits, as a line paints it: its glyphs
        // centred in the row.
        let baseline = layout.lines.first().map_or(line_height, |line| {
            let glyphs = &line.line.unwrapped_layout;
            (line_height - glyphs.ascent - glyphs.descent) / 2. + glyphs.ascent
        });
        Prepaint {
            layout,
            selections,
            caret,
            selection_color: t.selection,
            caret_color: t.accent_strong,
            misspelled,
            baseline,
            misspelled_color: t.danger,
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
            for word in &prepaint.misspelled {
                spelling::paint_dots(*word, prepaint.baseline, prepaint.misspelled_color, window);
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

    #[gpui::test]
    fn a_prose_field_underlines_misspellings_and_offers_their_guesses(
        cx: &mut gpui::TestAppContext,
    ) {
        let dir = crate::testing::scratch("spelling");
        let model = crate::testing::install(&dir, cx);
        let (input, cx) = cx.add_window_view(|window, cx| {
            TextInput::new(window, cx).multiline(4, 12).spell_checked()
        });
        cx.update(|window, cx| {
            let focus = input.read(cx).focus.clone();
            window.focus(&focus, cx);
        });
        let flagged = |cx: &mut gpui::VisualTestContext| {
            input.read_with(cx, |input, _| {
                let spelling = input.spelling.as_ref().unwrap();
                (
                    spelling.flagged.clone(),
                    spelling.shown().collect::<Vec<_>>(),
                )
            })
        };
        let pause = |cx: &mut gpui::VisualTestContext| {
            cx.executor().advance_clock(spelling::PAUSE);
            cx.run_until_parked();
        };

        // Checked once typing pauses.
        cx.simulate_input("fix teh bug");
        assert!(flagged(cx).0.is_empty());
        pause(cx);
        assert_eq!(flagged(cx), (vec![4..7], vec![4..7]));
        // A word still being typed is not judged until it is finished.
        cx.simulate_input(" wrod");
        pause(cx);
        assert_eq!(flagged(cx), (vec![4..7, 12..16], vec![4..7]));
        cx.simulate_keystrokes("left");
        assert_eq!(flagged(cx).1, [4..7, 12..16]);

        // A right-click on a misspelling takes the word, and the menu
        // leads with its guesses and the ways to leave it be.
        let on = |offset: usize, cx: &mut gpui::VisualTestContext| {
            input.read_with(cx, |input, _| {
                let layout = input.layout.as_ref().unwrap();
                layout.bounds.origin + layout.caret(offset) + point(px(0.), layout.line_height / 2.)
            })
        };
        let teh = on(5, cx);
        cx.simulate_mouse_down(teh, MouseButton::Right, gpui::Modifiers::none());
        cx.run_until_parked();
        assert_eq!(
            input.read_with(cx, |input, _| input.selection.clone()),
            4..7
        );
        let menu: Vec<(&str, String)> = model.read_with(cx, |model, _| {
            model
                .menu
                .as_ref()
                .unwrap()
                .sections
                .iter()
                .flatten()
                .map(|item| (item.id, item.label.clone()))
                .collect()
        });
        let words: Vec<&str> = menu.iter().map(|(_, label)| label.as_str()).collect();
        assert_eq!(
            words,
            [
                "the",
                "ten",
                "tea",
                "Ignore Spelling",
                "Learn Spelling",
                "Cut",
                "Copy",
                "Paste",
                "Select All"
            ]
        );

        // Choosing a guess puts it in place, as one step undo takes back.
        cx.dispatch_action(Spell(SpellChoice::Use("the".into())));
        assert_eq!(
            input.read_with(cx, |input, _| input.text.clone()),
            "fix the bug wrod"
        );
        assert_eq!(flagged(cx).0, [12..16]);
        cx.simulate_keystrokes("cmd-z");
        assert_eq!(
            input.read_with(cx, |input, _| input.text.clone()),
            "fix teh bug wrod"
        );

        // Ignoring a word stops flagging it in this field.
        let wrod = on(13, cx);
        cx.simulate_mouse_down(wrod, MouseButton::Right, gpui::Modifiers::none());
        cx.run_until_parked();
        cx.dispatch_action(Spell(SpellChoice::Ignore));
        assert_eq!(flagged(cx).0, [4..7]);

        // A field that did not ask is never checked: its menu is the
        // clipboard's alone.
        let plain = cx.update(|window, cx| cx.new(|cx| TextInput::new(window, cx)));
        plain.read_with(cx, |plain, _| assert!(plain.spelling.is_none()));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[gpui::test]
    fn a_field_tells_its_name_value_and_placeholder_and_keeps_a_key_to_itself(
        cx: &mut gpui::TestAppContext,
    ) {
        use gpui::accesskit::{Node, Role};

        let dir = crate::testing::scratch("field-node");
        crate::testing::install(&dir, cx);
        let window = cx.add_empty_window();
        let (query, key) = window.update(|window, cx| {
            let query = cx.new(|cx| {
                TextInput::new(window, cx)
                    .placeholder("Find a stack…")
                    .label("Find a stack")
                    .with_text("main")
            });
            let key = cx.new(|cx| {
                TextInput::new(window, cx)
                    .placeholder("github_pat_…")
                    .masked()
                    .with_text("ghp_secret")
            });
            (query, key)
        });
        window.update(|window, cx| {
            let read = |input: &Entity<TextInput>, window: &mut Window, cx: &mut App| {
                input.update(cx, |input, cx| {
                    let element = input.render(window, cx).into_element();
                    let role = element.a11y_role();
                    let mut node = Node::new(role.unwrap_or(Role::Unknown));
                    element.write_a11y_info(&mut node);
                    (role, node)
                })
            };
            let (role, query) = read(&query, window, cx);
            assert_eq!(role, Some(Role::TextInput));
            assert_eq!(query.label(), Some("Find a stack"));
            assert_eq!(query.value(), Some("main"));
            assert_eq!(query.placeholder(), Some("Find a stack…"));
            let (role, key) = read(&key, window, cx);
            assert_eq!(role, Some(Role::PasswordInput));
            assert_eq!(key.value(), None);
        });
        let _ = std::fs::remove_dir_all(dir);
    }
}
