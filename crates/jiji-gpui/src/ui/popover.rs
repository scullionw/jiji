//! Floating surfaces and the menus built on them: one material, one
//! open/closed state, and one way to hang a panel off its trigger.

use gpui::{
    anchored, canvas, deferred, div, point, prelude::*, px, Anchor, App, Bounds, DispatchPhase,
    Div, ElementId, FontWeight, HitboxBehavior, MouseDownEvent, SharedString, Stateful, Window,
};

use super::{icon, truncated, IconName};
use crate::theme::{theme, RADIUS_L, RADIUS_S, TEXT_M, TEXT_S, TEXT_XS};

/// Floating surfaces: menus, popovers, the palette, drag cards. Opaque,
/// one hairline, the system's only shadow.
pub fn popover(cx: &App) -> Div {
    let t = theme(cx);
    div()
        .bg(t.bg2)
        .border_1()
        .border_color(t.border1)
        .rounded(px(RADIUS_L))
        .shadow(t.popover_shadow())
        .text_color(t.text1)
        .text_size(px(TEXT_M))
        .occlude()
}

/// A group heading inside a popover or menu.
pub fn menu_label(text: impl Into<SharedString>, cx: &App) -> Div {
    let t = theme(cx);
    div()
        .pt(px(8.))
        .px(px(8.))
        .pb(px(4.))
        .text_size(px(TEXT_XS))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(t.text3)
        .child(text.into())
}

/// One row of a menu: an optional icon, a label, an optional trailing hint.
pub fn menu_row(
    id: impl Into<ElementId>,
    glyph: Option<IconName>,
    label: impl Into<SharedString>,
    cx: &App,
) -> Stateful<Div> {
    let t = theme(cx);
    let mut row = div()
        .id(id)
        .flex()
        .flex_row()
        .items_center()
        .gap(px(8.))
        .h(px(26.))
        .px(px(8.))
        .rounded(px(RADIUS_S))
        .text_size(px(TEXT_S))
        .text_color(t.text1)
        .cursor_pointer()
        .hover(|style| style.bg(t.bg_hover));
    if let Some(glyph) = glyph {
        row = row.child(icon(glyph, 14., t.text3));
    }
    row.child(div().flex_1().min_w_0().child(truncated(label)))
}

/// Whether a trigger's popover is open.
#[derive(Default)]
pub struct PopoverState {
    open: bool,
}

impl PopoverState {
    pub fn is_open(&self) -> bool {
        self.open
    }

    pub fn open(&mut self) {
        self.open = true;
    }

    pub fn close(&mut self) {
        self.open = false;
    }

    pub fn toggle(&mut self) {
        self.open = !self.open;
    }
}

/// Everything under an open popover: it takes the press that dismisses the
/// popover, so that press does nothing else, and nothing beneath it hovers
/// or scrolls meanwhile — the trigger raises no tooltip over its own panel.
fn backdrop(
    on_dismiss: impl Fn(&MouseDownEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    canvas(
        |_, window, _| {
            let viewport = Bounds::new(point(px(0.), px(0.)), window.viewport_size());
            window.insert_hitbox(viewport, HitboxBehavior::BlockMouse)
        },
        move |_, hitbox, window, _| {
            window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
                if phase == DispatchPhase::Bubble && hitbox.is_hovered(window) {
                    cx.stop_propagation();
                    on_dismiss(event, window, cx);
                }
            });
        },
    )
    .absolute()
    .size_0()
}

/// An open popover's panel, hung from its trigger: add this as a child of
/// the `relative()` element that wraps the trigger. The panel's `corner`
/// sits `top` px below that element's top, on the matching side; it draws
/// above everything else and stays inside the window. A press anywhere
/// outside the panel calls `on_dismiss` and goes no further.
pub fn popover_layer(
    corner: Anchor,
    top: f32,
    panel: impl IntoElement,
    on_dismiss: impl Fn(&MouseDownEvent, &mut Window, &mut App) + 'static,
) -> Div {
    let seat = div().absolute().top(px(top));
    let seat = match corner {
        Anchor::TopRight | Anchor::BottomRight | Anchor::RightCenter => seat.right_0(),
        _ => seat.left_0(),
    };
    seat.child(
        deferred(
            div().absolute().child(backdrop(on_dismiss)).child(
                anchored()
                    .anchor(corner)
                    .snap_to_window_with_margin(px(8.))
                    .child(panel),
            ),
        )
        .with_priority(1),
    )
}
