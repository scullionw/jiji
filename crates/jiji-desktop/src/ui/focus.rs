//! The keyboard's way through the window.
//!
//! Every control is a stop on it: Tab and Shift-Tab walk the stops in the
//! order they paint, Space and Return press the one that has focus (GPUI
//! makes a click of the pair), and a ring marks it while the keyboard put
//! it there. A click presses a control without taking focus from where it
//! was, as AppKit's buttons do, so the graph or the diff keeps its keys.
//!
//! A list is one stop: its arrows move within it and the ring rides its
//! current row ([`keyboard_on`]). A row of stops in a scroller comes into
//! view when Tab reaches it ([`FocusRow`]). A floating panel keeps Tab
//! inside it while it is open ([`Trap`]), and focus goes back to where it
//! was when the panel closes ([`Return`]).

use std::cell::Cell;
use std::rc::Rc;

use gpui::{
    actions, canvas, div, prelude::*, px, App, Bounds, BoxShadow, Div, ElementId, FocusHandle,
    Hsla, KeyBinding, MouseButton, Pixels, ScrollHandle, Stateful, StyleRefinement, Window,
};

use crate::theme::theme;

actions!(focus, [FocusNext, FocusPrev]);

pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("tab", FocusNext, None),
        KeyBinding::new("shift-tab", FocusPrev, None),
        // In an open menu the arrows step through its items, ahead of
        // whatever the arrows do in the view beneath it.
        KeyBinding::new("down", FocusNext, Some("Menu")),
        KeyBinding::new("up", FocusPrev, Some("Menu")),
    ]);
}

/// The ring: two pixels of `color` just inside the edge, over the fill and
/// under the content, so it takes no room and never moves a neighbour.
pub fn ring(color: Hsla) -> Vec<BoxShadow> {
    vec![BoxShadow::new(px(0.), px(0.), color)
        .spread_radius(px(2.))
        .inset()]
}

/// The ring outside a control: for one too small to hold it inside (a
/// checkbox), and around a filled button's inner one. Two pixels,
/// half-strength so it reads apart from an accent fill; it sits under the
/// control, so the control must be opaque.
pub fn halo(color: Hsla) -> Vec<BoxShadow> {
    vec![BoxShadow::new(px(0.), px(0.), color.opacity(0.55)).spread_radius(px(2.))]
}

/// Whether a list's container has focus from the keyboard: its current
/// row wears the ring then, and not after a click.
pub fn keyboard_on(focus: &FocusHandle, window: &Window) -> bool {
    focus.is_focused(window) && window.last_input_was_keyboard()
}

pub trait Tabbable: Sized {
    /// A stop on the keyboard's path, ringed in the accent.
    fn tabbable(self, cx: &App) -> Self {
        let accent = theme(cx).accent;
        self.tabbable_in(accent)
    }

    /// A stop ringed in `color` (the accent, where `cx` is already
    /// borrowed).
    fn tabbable_in(self, color: Hsla) -> Self {
        self.tabbable_styled(move |style| style.border_color(color).shadow(ring(color)))
    }

    /// A stop marked by `mark` while the keyboard holds it.
    fn tabbable_styled(
        self,
        mark: impl FnOnce(StyleRefinement) -> StyleRefinement + 'static,
    ) -> Self;
}

impl Tabbable for Stateful<Div> {
    fn tabbable_styled(
        self,
        mark: impl FnOnce(StyleRefinement) -> StyleRefinement + 'static,
    ) -> Self {
        self.focusable()
            .tab_stop(true)
            .focus_visible(mark)
            // A press leaves focus where it was: GPUI would otherwise hand
            // it to whatever focusable element took the press.
            .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
    }
}

/// A list row that holds tab stops, in a scroller. It knows when Tab is
/// inside it, so actions that show under the pointer can show then too,
/// and on the frame the keyboard brings focus into it, it scrolls into
/// view, as a browser scrolls to a focused control. Only then: a wheel
/// turned while focus sits in the row is never pulled back.
pub struct FocusRow {
    handle: FocusHandle,
    /// Focus was in the row last frame, from the keyboard.
    entered: Rc<Cell<bool>>,
}

impl FocusRow {
    /// The row known by `id`, kept across frames.
    pub fn new(id: impl Into<ElementId>, window: &mut Window, cx: &mut App) -> Self {
        let (handle, entered) = window.with_global_id(id.into(), |global_id, window| {
            window.with_element_state(
                global_id,
                |kept: Option<(FocusHandle, Rc<Cell<bool>>)>, _| {
                    let kept = kept.unwrap_or_else(|| (cx.focus_handle(), Rc::default()));
                    (kept.clone(), kept)
                },
            )
        });
        Self { handle, entered }
    }

    pub fn holds_focus(&self, window: &Window, cx: &App) -> bool {
        self.handle.contains_focused(window, cx)
    }

    /// The row's element, tracking the row and keeping it in view of
    /// `scroll` when the keyboard arrives.
    pub fn row(
        &self,
        row: Stateful<Div>,
        scroll: &ScrollHandle,
        window: &Window,
        cx: &App,
    ) -> Stateful<Div> {
        self.kept(row, false, scroll, window, cx)
    }

    /// A row that is itself the control (made [`Tabbable`] by its owner):
    /// the row's handle is its stop.
    pub fn stop(
        &self,
        row: Stateful<Div>,
        scroll: &ScrollHandle,
        window: &Window,
        cx: &App,
    ) -> Stateful<Div> {
        self.kept(row, true, scroll, window, cx)
    }

    fn kept(
        &self,
        row: Stateful<Div>,
        stop: bool,
        scroll: &ScrollHandle,
        window: &Window,
        cx: &App,
    ) -> Stateful<Div> {
        let arrived = self.holds_focus(window, cx) && window.last_input_was_keyboard();
        let first = arrived && !self.entered.get();
        self.entered.set(arrived);
        let scroll = scroll.clone();
        row.track_focus(&self.handle.clone().tab_stop(stop))
            .when(first, |row| {
                row.child(
                    canvas(
                        move |bounds, window, _| reveal(bounds, &scroll, window),
                        |_, _, _, _| {},
                    )
                    .absolute()
                    .size_full(),
                )
            })
    }
}

/// Scroll `scroll` the least that brings `bounds` into its view.
fn reveal(bounds: Bounds<Pixels>, scroll: &ScrollHandle, window: &mut Window) {
    let view = scroll.bounds();
    let above = view.top() - bounds.top();
    let below = bounds.bottom() - view.bottom();
    let by = if above > px(0.) {
        above
    } else if below > px(0.) {
        -below.min(-above)
    } else {
        return;
    };
    let mut offset = scroll.offset();
    offset.y = (offset.y + by).clamp(-scroll.max_offset().y, px(0.));
    scroll.set_offset(offset);
    // The rows are placed for this frame already: the next one draws them
    // where they now are, and a prepaint asks for no frame of its own.
    window.request_animation_frame();
}

/// Holds Tab inside a floating panel while it is open: past its last stop
/// Tab comes back in at the first, and Shift-Tab the other way round. The
/// panel itself takes focus when it opens, so the next Tab enters it.
pub struct Trap {
    panel: FocusHandle,
    /// A stop-less mark after the panel's last control: Shift-Tab from
    /// there reaches it.
    end: FocusHandle,
}

impl Trap {
    pub fn new(cx: &mut App) -> Self {
        Self {
            panel: cx.focus_handle(),
            end: cx.focus_handle(),
        }
    }

    /// Put focus on the panel itself.
    pub fn focus(&self, window: &mut Window, cx: &mut App) {
        window.focus(&self.panel, cx);
    }

    pub fn contains_focus(&self, window: &Window, cx: &App) -> bool {
        self.panel.contains_focused(window, cx)
    }

    /// The panel's element, holding Tab.
    pub fn hold(&self, panel: Stateful<Div>) -> Stateful<Div> {
        let start = self.panel.clone();
        let (back_start, back_end) = (self.panel.clone(), self.end.clone());
        panel
            .track_focus(&self.panel)
            .on_action(move |_: &FocusNext, window, cx| {
                cycle(&start, &start, true, window, cx);
            })
            .on_action(move |_: &FocusPrev, window, cx| {
                cycle(&back_start, &back_end, false, window, cx);
            })
            .child(div().track_focus(&self.end))
    }
}

/// One Tab inside `panel`: off its end, in again at `wrap` (the panel
/// itself going forward, its end mark going back).
fn cycle(
    panel: &FocusHandle,
    wrap: &FocusHandle,
    forward: bool,
    window: &mut Window,
    cx: &mut App,
) {
    let step = |window: &mut Window, cx: &mut App| {
        if forward {
            window.focus_next(cx);
        } else {
            window.focus_prev(cx);
        }
    };
    step(window, cx);
    if panel.contains_focused(window, cx) {
        return;
    }
    window.focus(wrap, cx);
    step(window, cx);
    // A panel with nothing to stop at keeps the focus itself.
    if !panel.contains_focused(window, cx) {
        window.focus(panel, cx);
    }
}

/// Where focus was when a panel, tray or overlay opened, to give it back
/// when it closes: to the control that opened it from the keyboard, or to
/// whatever held the keys when a click opened it.
#[derive(Default)]
pub struct Return(Option<FocusHandle>);

impl Return {
    pub fn take(&mut self, window: &Window, cx: &App) {
        self.0 = window.focused(cx);
    }

    /// Focus goes back, if it was anywhere. Anything else that wants it
    /// takes it after this.
    pub fn give_back(&mut self, window: &mut Window, cx: &mut App) {
        if let Some(handle) = self.0.take() {
            window.focus(&handle, cx);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use gpui::{div, prelude::*, Context, FocusHandle, Render, TestAppContext, Window};

    use super::{FocusNext, FocusPrev, Tabbable as _};
    use crate::testing::press;

    /// Three controls in a row; each press is logged by name.
    struct Row {
        root: FocusHandle,
        pressed: Rc<RefCell<Vec<&'static str>>>,
    }

    impl Render for Row {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            let mut row = div()
                .track_focus(&self.root)
                .on_action(|_: &FocusNext, window, cx| window.focus_next(cx))
                .on_action(|_: &FocusPrev, window, cx| window.focus_prev(cx));
            for name in ["first", "second", "third"] {
                let pressed = self.pressed.clone();
                row = row.child(
                    div()
                        .id(name)
                        .tabbable_in(gpui::blue())
                        .on_click(move |_, _, _| pressed.borrow_mut().push(name)),
                );
            }
            row
        }
    }

    /// Thirty rows of one stop each, in a scroller ten rows tall; each row
    /// notes whether it holds the keys.
    struct Rows {
        root: FocusHandle,
        scroll: gpui::ScrollHandle,
        holding: Rc<RefCell<Vec<usize>>>,
    }

    impl Render for Rows {
        fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            self.holding.borrow_mut().clear();
            let mut list = div()
                .id("rows")
                .h(gpui::px(300.))
                .overflow_y_scroll()
                .track_scroll(&self.scroll);
            for ix in 0..30 {
                let kept = super::FocusRow::new(("row", ix), window, cx);
                if kept.holds_focus(window, cx) {
                    self.holding.borrow_mut().push(ix);
                }
                let row = div()
                    .id(("row-wrap", ix))
                    .h(gpui::px(30.))
                    .child(div().id(("stop", ix)).size_full().tabbable_in(gpui::blue()));
                list = list.child(kept.row(row, &self.scroll, window, cx));
            }
            div()
                .track_focus(&self.root)
                .on_action(|_: &FocusNext, window, cx| window.focus_next(cx))
                .child(list)
        }
    }

    #[gpui::test]
    fn a_row_tab_reaches_below_the_fold_scrolls_into_view(cx: &mut TestAppContext) {
        cx.update(crate::ui::focus::bind_keys);
        let scroll = gpui::ScrollHandle::new();
        let holding = Rc::new(RefCell::new(Vec::new()));
        let window = cx.add_window({
            let (scroll, holding) = (scroll.clone(), holding.clone());
            move |window, cx| {
                let root = cx.focus_handle();
                window.focus(&root, cx);
                Rows {
                    root,
                    scroll,
                    holding,
                }
            }
        });
        let keys = window.into();
        for _ in 0..14 {
            press(keys, "tab", cx);
        }
        // The frame the move asked for.
        cx.update_window(keys, |_, window, cx| {
            window.refresh();
            window.draw(cx).clear(cx);
        })
        .unwrap();
        assert_eq!(*holding.borrow(), [13]);
        // A child's bounds are laid out before the scroll moves them.
        let (view, row) = (scroll.bounds(), scroll.bounds_for_item(13).unwrap());
        let shown = row.top() + scroll.offset().y;
        assert!(
            shown >= view.top() && shown + row.size.height <= view.bottom(),
            "row 14 shown at {shown:?}, outside {view:?}"
        );
    }

    #[gpui::test]
    fn tab_walks_the_controls_and_space_and_return_press_the_one_it_is_on(cx: &mut TestAppContext) {
        cx.update(crate::ui::focus::bind_keys);
        let pressed = Rc::new(RefCell::new(Vec::new()));
        let window = cx.add_window({
            let pressed = pressed.clone();
            move |window, cx| {
                let root = cx.focus_handle();
                window.focus(&root, cx);
                Row { root, pressed }
            }
        });
        let keys = window.into();
        press(keys, "tab space", cx);
        press(keys, "tab enter", cx);
        press(keys, "tab tab space", cx);
        press(keys, "shift-tab shift-tab enter", cx);
        assert_eq!(*pressed.borrow(), ["first", "second", "first", "second"]);
    }
}
