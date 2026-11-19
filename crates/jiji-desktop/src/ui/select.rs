//! Text the pointer can select and copy, in part or whole, the way a
//! selectable label in a Mac app is: a drag selects, a double-click takes a
//! word and a triple-click a line, ⌘A takes it all, and ⌘C, Edit ▸ Copy or
//! the context menu's Copy copy what is selected. One line that gives way
//! with an ellipsis, or a paragraph that wraps.
//!
//! A press gives it the keys, as a click gives them to a selectable field,
//! so the menu bar's Copy finds it; it is no stop on Tab's path, and its
//! selection goes when the keys move on.

use std::cell::RefCell;
use std::ops::Range;
use std::rc::Rc;

use gpui::{
    fill, point, px, App, ClipboardItem, CursorStyle, DispatchPhase, Element, ElementId,
    FocusHandle, Font, GlobalElementId, HighlightStyle, Hitbox, HitboxBehavior, Hsla,
    InspectorElementId, KeyBinding, KeyContext, LayoutId, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, Pixels, Point, ShapedLine, SharedString, TextAlign, TextRun,
    Window,
};

use super::input::{Copy, SelectAll};
use super::textlayout::{lay_out, line_range, word_range, Layout};
use super::truncate::{self, Face};
use super::wrap;
use crate::theme::theme;

/// ⌘C and ⌘A while selectable text has the keys: the text fields' own
/// actions, so Edit ▸ Copy and Edit ▸ Select All reach it too.
pub fn bind_keys(cx: &mut App) {
    let context = Some("Selectable");
    cx.bind_keys([
        KeyBinding::new("cmd-c", Copy, context),
        KeyBinding::new("cmd-a", SelectAll, context),
    ]);
}

pub struct Selectable {
    id: ElementId,
    text: SharedString,
    highlights: Vec<(Range<usize>, HighlightStyle)>,
    wrap: bool,
}

/// `text`, selectable, on one line that ends in "…" when it does not fit.
pub fn selectable(id: impl Into<ElementId>, text: impl Into<SharedString>) -> Selectable {
    Selectable {
        id: id.into(),
        text: text.into(),
        highlights: Vec::new(),
        wrap: false,
    }
}

impl Selectable {
    /// A paragraph that wraps to the width it is given instead.
    pub fn wrap(mut self) -> Self {
        self.wrap = true;
        self
    }

    /// Emphasis over byte ranges, in order and apart: an id's unique
    /// prefix, say.
    pub fn highlights(
        mut self,
        highlights: impl IntoIterator<Item = (Range<usize>, HighlightStyle)>,
    ) -> Self {
        self.highlights = highlights.into_iter().collect();
        self
    }
}

/// What survives from frame to frame, under the element's id.
struct State {
    focus: FocusHandle,
    /// The text the selection indexes into.
    text: SharedString,
    selection: Range<usize>,
    /// The selection grew backwards: its start is the end that moves.
    reversed: bool,
    dragging: bool,
    /// The last paint's layout, for the pointer.
    layout: Option<Layout>,
    /// Where an ellipsis cut the line: the pointer past it means the end.
    cut: Option<usize>,
}

impl State {
    fn index_at(&self, position: Point<Pixels>) -> usize {
        let Some(layout) = &self.layout else {
            return 0;
        };
        let index = layout.index_at(position - layout.bounds.origin);
        match self.cut {
            Some(cut) if index > cut => self.text.len(),
            _ => index,
        }
    }

    fn select(&mut self, range: Range<usize>) {
        self.selection = range;
        self.reversed = false;
    }

    /// Move the selection's moving end to `offset`.
    fn select_to(&mut self, offset: usize) {
        if self.reversed {
            self.selection.start = offset;
        } else {
            self.selection.end = offset;
        }
        if self.selection.end < self.selection.start {
            self.reversed = !self.reversed;
            self.selection = self.selection.end..self.selection.start;
        }
    }

    fn selected(&self) -> Option<String> {
        (!self.selection.is_empty()).then(|| self.text[self.selection.clone()].to_owned())
    }
}

type Shared = Rc<RefCell<State>>;

pub struct Painted {
    state: Shared,
    hitbox: Hitbox,
    lines: Lines,
    boxes: Vec<gpui::Bounds<Pixels>>,
    selection_color: Hsla,
}

/// What paint draws: the laid-out text, or the one line cut to fit.
enum Lines {
    Laid,
    Cut(ShapedLine),
}

/// The runs of `text` in `face` and `color`, with `highlights` over them.
fn runs(
    len: usize,
    face: &Face,
    color: Hsla,
    highlights: &[(Range<usize>, HighlightStyle)],
) -> Vec<TextRun> {
    let run = |len: usize, highlight: Option<&HighlightStyle>| TextRun {
        len,
        font: Font {
            weight: highlight
                .and_then(|h| h.font_weight)
                .unwrap_or(face.font.weight),
            ..face.font.clone()
        },
        color: highlight.and_then(|h| h.color).unwrap_or(color),
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    let mut runs = Vec::new();
    let mut at = 0;
    for (range, highlight) in highlights {
        let range = range.start.clamp(at, len)..range.end.clamp(at, len);
        if range.start > at {
            runs.push(run(range.start - at, None));
        }
        if !range.is_empty() {
            runs.push(run(range.len(), Some(highlight)));
        }
        at = range.end.max(at);
    }
    if at < len || runs.is_empty() {
        runs.push(run(len - at, None));
    }
    runs
}

/// The first `cut` bytes of `runs`.
fn cut_runs(runs: &[TextRun], cut: usize) -> Vec<TextRun> {
    let mut kept = Vec::new();
    let mut at = 0;
    for run in runs {
        if at >= cut {
            break;
        }
        kept.push(TextRun {
            len: run.len.min(cut - at),
            ..run.clone()
        });
        at += run.len;
    }
    kept
}

impl Element for Selectable {
    type RequestLayoutState = Face;
    type PrepaintState = Painted;

    fn id(&self) -> Option<ElementId> {
        Some(self.id.clone())
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        _cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        if self.wrap {
            wrap::measure(&self.text, window)
        } else {
            truncate::measure(&self.text, window)
        }
    }

    fn prepaint(
        &mut self,
        id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: gpui::Bounds<Pixels>,
        face: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let state = window.with_element_state(id.unwrap(), |state: Option<Shared>, _| {
            let state = state.unwrap_or_else(|| {
                Rc::new(RefCell::new(State {
                    focus: cx.focus_handle(),
                    text: self.text.clone(),
                    selection: 0..0,
                    reversed: false,
                    dragging: false,
                    layout: None,
                    cut: None,
                }))
            });
            (state.clone(), state)
        });
        let focused = {
            let mut s = state.borrow_mut();
            // New words, or the keys gone elsewhere: nothing is selected.
            let focused = s.focus.is_focused(window);
            if s.text != self.text || (!focused && !s.dragging) {
                s.text = self.text.clone();
                s.select(0..0);
            }
            window.set_focus_handle(&s.focus, cx);
            focused
        };
        let hitbox = window.insert_hitbox(bounds, HitboxBehavior::Normal);

        let color = window.text_style().color;
        let runs = runs(self.text.len(), face, color, &self.highlights);
        let wrap_width = self.wrap.then_some(bounds.size.width);
        let (laid, content_height) = lay_out(
            self.text.clone(),
            &runs,
            face.font_size,
            wrap_width,
            face.line_height,
            window,
        );
        let layout = Layout {
            bounds,
            line_height: face.line_height,
            lines: laid,
            scroll: Point::default(),
            content_height,
        };
        // One line that overruns gives way at an ellipsis, as `truncated`
        // does; the selection reads through to the end beneath it.
        let mut lines = Lines::Laid;
        let mut cut = None;
        if let (false, Some(line)) = (self.wrap, layout.lines.first()) {
            if line.line.width() > bounds.size.width + px(0.5) {
                let plain = TextRun {
                    len: "…".len(),
                    font: face.font.clone(),
                    color,
                    background_color: None,
                    underline: None,
                    strikethrough: None,
                };
                let ellipsis = window.text_system().shape_line(
                    "…".into(),
                    face.font_size,
                    std::slice::from_ref(&plain),
                    None,
                );
                let room = bounds.size.width - ellipsis.width;
                let mut at = line
                    .line
                    .unwrapped_layout
                    .index_for_x(room.max(px(0.)))
                    .unwrap_or(self.text.len());
                while at > 0 && !self.text.is_char_boundary(at) {
                    at -= 1;
                }
                let kept = self.text[..at].trim_end();
                let mut kept_runs = cut_runs(&runs, kept.len());
                kept_runs.push(plain);
                lines = Lines::Cut(window.text_system().shape_line(
                    format!("{kept}…").into(),
                    face.font_size,
                    &kept_runs,
                    None,
                ));
                cut = Some(kept.len());
            }
        }
        let mut s = state.borrow_mut();
        let boxes = if focused {
            layout.boxes(s.selection.clone(), bounds.origin)
        } else {
            Vec::new()
        };
        s.layout = Some(layout);
        s.cut = cut;
        drop(s);
        Painted {
            state,
            hitbox,
            lines,
            boxes,
            selection_color: theme(cx).selection,
        }
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: gpui::Bounds<Pixels>,
        face: &mut Self::RequestLayoutState,
        painted: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let view = window.current_view();
        let state = painted.state.clone();
        window.set_key_context(KeyContext::parse("Selectable").expect("a valid context"));
        window.set_cursor_style(CursorStyle::IBeam, &painted.hitbox);

        // Copy is on offer only while something is selected, so the menu
        // bar greys Edit ▸ Copy out otherwise, as it does for a field.
        if !state.borrow().selection.is_empty() {
            let state = state.clone();
            window.on_action(std::any::TypeId::of::<Copy>(), move |_, phase, _, cx| {
                if phase == DispatchPhase::Bubble {
                    if let Some(text) = state.borrow().selected() {
                        cx.write_to_clipboard(ClipboardItem::new_string(text));
                    }
                }
            });
        }
        {
            let state = state.clone();
            window.on_action(
                std::any::TypeId::of::<SelectAll>(),
                move |_, phase, _, cx| {
                    if phase == DispatchPhase::Bubble {
                        let mut s = state.borrow_mut();
                        let all = 0..s.text.len();
                        s.select(all);
                        cx.notify(view);
                    }
                },
            );
        }
        {
            let (state, hitbox) = (state.clone(), painted.hitbox.clone());
            window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
                if phase != DispatchPhase::Bubble || !hitbox.is_hovered(window) {
                    return;
                }
                let mut s = state.borrow_mut();
                let offset = s.index_at(event.position);
                match event.button {
                    MouseButton::Left => {
                        // The keys come here, and no further out.
                        window.focus(&s.focus, cx);
                        window.prevent_default();
                        match event.click_count {
                            1 if event.modifiers.shift => s.select_to(offset),
                            1 => {
                                s.select(offset..offset);
                                s.dragging = true;
                            }
                            2 => {
                                let word = word_range(&s.text, offset);
                                s.select(word);
                            }
                            _ => {
                                let line = line_range(&s.text, offset);
                                s.select(line);
                            }
                        }
                    }
                    MouseButton::Right => {
                        window.focus(&s.focus, cx);
                        window.prevent_default();
                        cx.stop_propagation();
                        // A right-click outside the selection takes the
                        // word under it first, as Mac text does.
                        let inside = s.selection.start <= offset && offset <= s.selection.end;
                        if s.selection.is_empty() || !inside {
                            let word = word_range(&s.text, offset);
                            s.select(word);
                        }
                        let has_selection = !s.selection.is_empty();
                        drop(s);
                        let position = event.position;
                        crate::app::app(cx).update(cx, |model, cx| {
                            model.open_editable_menu(has_selection, true, None, position, cx)
                        });
                    }
                    _ => return,
                }
                cx.notify(view);
            });
        }
        {
            // Moves and releases are taken window-wide, so a drag keeps
            // selecting past the text's edges.
            let state = state.clone();
            window.on_mouse_event(move |event: &MouseMoveEvent, phase, _, cx| {
                if phase != DispatchPhase::Bubble {
                    return;
                }
                let mut s = state.borrow_mut();
                if !s.dragging {
                    return;
                }
                if event.pressed_button != Some(MouseButton::Left) {
                    s.dragging = false;
                    return;
                }
                let offset = s.index_at(event.position);
                s.select_to(offset);
                cx.notify(view);
            });
        }
        window.on_mouse_event(move |event: &MouseUpEvent, phase, _, _| {
            if phase == DispatchPhase::Bubble && event.button == MouseButton::Left {
                state.borrow_mut().dragging = false;
            }
        });

        window.with_content_mask(Some(gpui::ContentMask { bounds }), |window| {
            for selected in &painted.boxes {
                window.paint_quad(fill(*selected, painted.selection_color));
            }
            match &painted.lines {
                Lines::Cut(line) => {
                    let _ = line.paint(
                        bounds.origin,
                        face.line_height,
                        TextAlign::Left,
                        None,
                        window,
                        cx,
                    );
                }
                Lines::Laid => {
                    let s = painted.state.borrow();
                    if let Some(layout) = &s.layout {
                        for line in &layout.lines {
                            let _ = line.line.paint(
                                point(bounds.left(), bounds.top() + line.top),
                                face.line_height,
                                TextAlign::Left,
                                Some(bounds),
                                window,
                                cx,
                            );
                        }
                    }
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emphasis_runs_cover_the_text_once_in_order() {
        let face = Face {
            font: gpui::font("Menlo"),
            font_size: px(12.),
            line_height: px(18.),
        };
        let bold = HighlightStyle {
            font_weight: Some(gpui::FontWeight::BOLD),
            ..Default::default()
        };
        let lens = |runs: Vec<TextRun>| runs.iter().map(|run| run.len).collect::<Vec<_>>();
        let ink = Hsla::default();
        assert_eq!(lens(runs(8, &face, ink, &[(0..2, bold)])), [2, 6]);
        assert_eq!(lens(runs(8, &face, ink, &[(2..4, bold)])), [2, 2, 4]);
        assert_eq!(lens(runs(8, &face, ink, &[])), [8]);
        assert_eq!(lens(runs(0, &face, ink, &[])), [0]);
        // Emphasis past the end is cut to it.
        assert_eq!(lens(runs(3, &face, ink, &[(1..9, bold)])), [1, 2]);
        let all = runs(8, &face, ink, &[(0..2, bold)]);
        assert_eq!(lens(cut_runs(&all, 5)), [2, 3]);
        assert_eq!(lens(cut_runs(&all, 1)), [1]);
    }
}
