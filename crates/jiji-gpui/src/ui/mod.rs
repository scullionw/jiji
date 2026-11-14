//! Shared primitives. Every control in the app is one of these, so a
//! button in a popover, a panel, and a section page are the same object.
//! Views add layout; they don't restyle the material.

mod button;
pub mod icon;
pub mod input;
mod page;
mod pinned;
mod popover;
pub mod scroll;
mod text;
mod time;
pub mod tooltip;
pub mod truncate;
mod wrap;

use std::sync::Arc;

use gpui::{div, prelude::*, px, App, Div, FontFeatures, Hsla};

pub use self::button::{btn, checkbox, icon_btn, kbd, segmented, Btn};
pub use self::icon::{icon, IconName};
pub use self::input::{InputEvent, TextInput};
pub use self::page::{
    all_clear, consequences, empty_state, group_head, page_bar, page_filter, page_notice,
    plan_actions, plan_panel, plan_title, tray, PAGE_PAD,
};
pub use self::pinned::pinned;
pub use self::popover::{menu_label, menu_row, popover, popover_layer, PopoverState};
pub use self::scroll::{scrollbar, vertical_only, wheel_first, AxisLock, ScrollY};
pub use self::text::{
    diffstat, label_box, middot, ref_label, rich, tag, text_width, text_width_in, trunk_label,
    LABEL_H,
};
pub use self::time::{absolute_time, from_now, short_age};
pub use self::tooltip::{tooltip, tooltip_guard};
pub use self::truncate::truncated;
pub use self::wrap::wrapped;
use crate::theme::theme;

/// Figures that line up: every digit one width, so ages, counts and times
/// in a column hold still as they change.
pub trait Tabular: Styled {
    fn tabular(mut self) -> Self {
        self.text_style()
            .get_or_insert_with(Default::default)
            .font_features = Some(FontFeatures(Arc::new(vec![("tnum".into(), 1)])));
        self
    }
}

impl<E: Styled> Tabular for E {}

pub fn h_flex() -> Div {
    div().flex().flex_row().items_center()
}

pub fn v_flex() -> Div {
    div().flex().flex_col()
}

/// A hairline between groups of a menu or panel.
pub fn separator(cx: &App) -> Div {
    div().h(px(1.)).my(px(4.)).bg(theme(cx).border2)
}

/// The mark of what is current: a two-pixel accent bar on the leading edge
/// of a `relative()` row.
pub fn lead_bar(cx: &App) -> Div {
    div()
        .absolute()
        .left_0()
        .top_0()
        .bottom_0()
        .w(px(2.))
        .bg(theme(cx).accent)
}

/// A small status dot.
pub fn dot(color: Hsla, size: f32) -> Div {
    div()
        .flex_shrink_0()
        .size(px(size))
        .rounded(px(size / 2.))
        .bg(color)
}

/// Reports where an element landed: lay it over (or inside) the element
/// whose bounds a later event handler needs.
pub fn bounds_probe(
    cell: std::rc::Rc<std::cell::Cell<gpui::Bounds<gpui::Pixels>>>,
) -> impl IntoElement {
    gpui::canvas(move |bounds, _, _| cell.set(bounds), |_, _, _, _| {})
        .absolute()
        .size_full()
}
