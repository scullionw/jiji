//! A divider between two panes: the pointer drags it, and the keyboard
//! moves it once Tab reaches it — ← and → by [`STEP`], Home and End to its
//! limits. Its owner keeps the width and acts on the moves; this draws the
//! hairline, its grip, the mark of keyboard focus, and tells assistive
//! technology what it resizes and where it stands.

use gpui::{
    actions, div, prelude::*, px, App, CursorStyle, Div, FocusHandle, KeyBinding, MouseButton,
    Orientation, Role, SharedString, Stateful, Window,
};

use super::keyboard_on;
use crate::theme::theme;

actions!(splitter, [Narrower, Wider, Narrowest, Widest]);

/// One arrow press.
pub const STEP: f32 = 16.;

pub fn bind_keys(cx: &mut App) {
    let splitter = Some("Splitter");
    cx.bind_keys([
        KeyBinding::new("left", Narrower, splitter),
        KeyBinding::new("right", Wider, splitter),
        KeyBinding::new("home", Narrowest, splitter),
        KeyBinding::new("end", Widest, splitter),
    ]);
}

/// Where a move takes `width`, inside `min..=max`.
pub fn moved(width: f32, min: f32, max: f32, by: f32) -> f32 {
    (width + by).clamp(min, max.max(min))
}

/// The divider for a pane `width` wide inside `min..=max`, lit while
/// `dragging`. Its owner adds the actions that move it, and through `grip`
/// the presses that drag and reset it.
#[allow(clippy::too_many_arguments)]
pub fn splitter(
    id: &'static str,
    name: impl Into<SharedString>,
    focus: &FocusHandle,
    width: f32,
    min: f32,
    max: f32,
    dragging: bool,
    window: &Window,
    cx: &App,
    grip: impl FnOnce(Stateful<Div>) -> Stateful<Div>,
) -> Stateful<Div> {
    let t = theme(cx);
    let keyed = keyboard_on(focus, window);
    div()
        .id(id)
        .key_context("Splitter")
        .track_focus(focus)
        .role(Role::Splitter)
        .aria_label(name)
        .aria_orientation(Orientation::Vertical)
        .aria_numeric_value(width.round() as f64)
        .aria_min_numeric_value(min.round() as f64)
        .aria_max_numeric_value(max.round() as f64)
        .aria_numeric_value_step(STEP as f64)
        .relative()
        .w(px(1.))
        .flex_shrink_0()
        .h_full()
        .bg(if dragging { t.accent } else { t.border2 })
        // With the keys on it, the hairline thickens into the accent's
        // two-pixel rule plus one: the divider is what the arrows move.
        .when(keyed, |el| {
            el.child(
                div()
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .left(px(-1.))
                    .w(px(3.))
                    .bg(t.accent),
            )
        })
        // The grip, wider than the hairline it holds. A press on it drags;
        // it never takes the keys, as no control's press does.
        .child(grip(
            div()
                .id("grip")
                .absolute()
                .top_0()
                .bottom_0()
                .left(px(-3.))
                .w(px(7.))
                .cursor(CursorStyle::ResizeLeftRight)
                .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default()),
        ))
}

#[cfg(test)]
mod tests {
    use super::moved;

    #[test]
    fn a_move_stays_inside_the_limits() {
        assert_eq!(moved(400., 320., 760., 16.), 416.);
        assert_eq!(moved(330., 320., 760., -16.), 320.);
        assert_eq!(moved(400., 320., 760., f32::NEG_INFINITY), 320.);
        assert_eq!(moved(400., 320., 760., f32::INFINITY), 760.);
        // A window too narrow for the minimum holds the minimum.
        assert_eq!(moved(400., 320., 300., 16.), 320.);
    }
}
