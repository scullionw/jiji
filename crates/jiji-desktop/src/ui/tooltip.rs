//! Plain text tooltips, and the guard that takes them down when the user
//! has moved on.
//!
//! GPUI retires a tooltip only when the pointer next moves off its
//! element, and never cancels one that is still waiting out its delay. So
//! a click that opens a menu would still raise the trigger's tooltip over
//! it half a second later, and a tooltip on screen when ⌘K is pressed
//! would float above the palette. The guard watches every press and key:
//! a tooltip already showing stops drawing, and one whose delay the input
//! interrupted never draws.

use std::time::{Duration, Instant};

use gpui::{
    canvas, div, prelude::*, px, AnyView, App, Context, DispatchPhase, Global, MouseDownEvent,
    Render, SharedString, Window,
};

use crate::theme::{theme, FONT_UI, RADIUS_S, TEXT_XS};

/// GPUI's own delay between hovering an element and building its tooltip.
/// Elements that raise their own tooltips wait as long.
pub const SHOW_DELAY: Duration = Duration::from_millis(500);

#[derive(Default)]
struct TooltipGuard {
    /// Counts the inputs seen; a tooltip belongs to the count it was
    /// raised under.
    epoch: u64,
    last_input: Option<Instant>,
    /// A tooltip drew since the last input.
    showing: bool,
}

impl Global for TooltipGuard {}

fn dismiss(window: &mut Window, cx: &mut App) {
    let guard = cx.default_global::<TooltipGuard>();
    guard.epoch += 1;
    guard.last_input = Some(Instant::now());
    if std::mem::take(&mut guard.showing) {
        window.refresh();
    }
}

/// Start watching keys on behalf of every tooltip. Call once at startup.
/// (An observer, not a key listener: a key that a binding claims never
/// reaches listeners, and those are the keys that open overlays.)
pub fn init(cx: &mut App) {
    cx.observe_keystrokes(|_, window, cx| dismiss(window, cx))
        .detach();
}

/// Watches the window's presses on behalf of every tooltip. Put one at the
/// window's root.
pub fn tooltip_guard() -> impl IntoElement {
    canvas(
        |_, _, _| (),
        |_, (), window, _| {
            window.on_mouse_event(|_: &MouseDownEvent, phase, window, cx| {
                if phase == DispatchPhase::Capture {
                    dismiss(window, cx);
                }
            });
        },
    )
    .absolute()
    .size_0()
}

struct TooltipView {
    text: SharedString,
    /// The guard's count when this was raised; `None` when an input
    /// interrupted the wait, so it was stale before it ever drew.
    born: Option<u64>,
}

impl Render for TooltipView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let guard = cx.default_global::<TooltipGuard>();
        if self.born != Some(guard.epoch) {
            return div();
        }
        guard.showing = true;
        let t = theme(cx);
        // Offset off the pointer so the tip never sits under it.
        div().pt(px(14.)).pl(px(6.)).child(
            div()
                .bg(t.bg2)
                .border_1()
                .border_color(t.border1)
                .rounded(px(RADIUS_S))
                .shadow(t.popover_shadow())
                .px(px(7.))
                .py(px(3.))
                .max_w(px(360.))
                .text_size(px(TEXT_XS))
                .text_color(t.text1)
                .font_family(FONT_UI)
                .child(self.text.clone()),
        )
    }
}

/// A plain text tooltip, for `.tooltip(...)`.
pub fn tooltip(text: impl Into<SharedString>) -> impl Fn(&mut Window, &mut App) -> AnyView {
    let text = text.into();
    move |_window, cx| {
        let text = text.clone();
        let guard = cx.default_global::<TooltipGuard>();
        let interrupted = guard.last_input.is_some_and(|at| at.elapsed() < SHOW_DELAY);
        let born = (!interrupted).then_some(guard.epoch);
        cx.new(|_| TooltipView { text, born }).into()
    }
}
