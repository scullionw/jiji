//! Shared primitives. Every control in the app is one of these, so a
//! button in a popover, a panel, and a section page are the same object.
//! Views add layout; they don't restyle the material.

pub mod a11y;
mod button;
mod files;
pub mod focus;
pub mod icon;
pub mod input;
mod marks;
pub mod motion;
mod page;
mod pinned;
mod popover;
pub mod scroll;
pub mod select;
mod spelling;
pub mod splitter;
mod text;
mod textlayout;
mod time;
pub mod tooltip;
pub mod truncate;
mod wrap;

use std::sync::Arc;

use gpui::{div, prelude::*, px, App, Div, FontFeatures, Hsla};

pub use self::a11y::NodeExt;
pub use self::button::{btn, checkbox, icon_btn, kbd, segmented, Btn, Segment};
pub use self::files::{file_glyph, file_icon};
pub use self::focus::{keyboard_on, ring, Tabbable};
pub use self::icon::{icon, IconName};
pub use self::input::{InputEvent, TextInput};
pub use self::marks::{line_of, mark_for, node_mark, Mark};
pub use self::page::{
    all_clear, consequences, empty_state, group_head, page_bar, page_bar_marked, page_filter,
    page_notice, plan_actions, plan_panel, plan_title, tray, PAGE_PAD,
};
pub use self::pinned::pinned;
pub use self::popover::{menu_label, menu_row, popover, popover_layer, PopoverState};
pub use self::scroll::{pan_scrollbar, scrollbar, vertical_only, wheel_first, AxisLock, ScrollY};
pub use self::select::selectable;
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
        self.text_style().font_features = Some(FontFeatures(Arc::new(vec![("tnum".into(), 1)])));
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

/// The same mark in a hue of its own: a workstream's lane, say.
pub fn lead_bar_in(color: Hsla) -> Div {
    div()
        .absolute()
        .left_0()
        .top_0()
        .bottom_0()
        .w(px(2.))
        .bg(color)
}

/// Up to two letters that stand for a name: "William Scullion" → "WS",
/// "jiji" → "J".
pub fn initials(name: &str) -> String {
    let words: Vec<&str> = name
        .split(|c: char| c.is_whitespace() || c == '@' || c == '-' || c == '_' || c == '.')
        .filter(|word| word.chars().any(char::is_alphanumeric))
        .collect();
    let first = |word: &str| {
        word.chars()
            .find(|c| c.is_alphanumeric())
            .map(|c| c.to_uppercase().collect::<String>())
            .unwrap_or_default()
    };
    match words.as_slice() {
        [] => "·".to_owned(),
        // An email or a handle stands for one person: one letter.
        [only] => first(only),
        [head, ..] if name.contains('@') => first(head),
        [head, .., last] => format!("{}{}", first(head), first(last)),
    }
}

/// A name's tile: its initials on a tint of its identity hue. Square-ish
/// for a repository, round for a person.
pub fn monogram(key: &str, letters: &str, size: f32, round: bool, cx: &App) -> Div {
    let t = theme(cx);
    let hue = t.identity(key);
    div()
        .flex()
        .flex_shrink_0()
        .items_center()
        .justify_center()
        .size(px(size))
        .rounded(px(if round {
            size / 2.
        } else {
            (size / 4.).round()
        }))
        .bg(crate::theme::Theme::fade(
            hue,
            if t.is_dark() { 0.2 } else { 0.14 },
        ))
        .text_color(hue)
        .text_size(px((size * 0.58).round()))
        .line_height(px(size))
        .font_weight(gpui::FontWeight::BOLD)
        .child(letters.to_owned())
}

/// A station: the graph's change marker, a ring in its line's hue, for
/// places outside the graph that point at a change on a line.
pub fn station(color: Hsla, size: f32) -> Div {
    div()
        .flex_shrink_0()
        .size(px(size))
        .rounded(px(size / 2.))
        .border_2()
        .border_color(color)
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

#[cfg(test)]
mod tests {
    use super::initials;

    #[test]
    fn initials_stand_for_a_name() {
        assert_eq!(initials("William Scullion"), "WS");
        assert_eq!(initials("jiji"), "J");
        assert_eq!(initials("Ada King Lovelace"), "AL");
        assert_eq!(initials("lauf@example.com"), "L");
        assert_eq!(initials("jj-lander"), "JL");
        assert_eq!(initials("  "), "·");
    }
}
