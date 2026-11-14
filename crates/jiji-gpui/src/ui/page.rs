//! The furniture of a full pane: empty states, section openings, and the
//! plan panel that states what a consequential action will do.

use gpui::{div, prelude::*, px, App, Div, Entity, FontWeight, SharedString, Window};

use super::{h_flex, icon, truncated, v_flex, Btn, IconName, TextInput};
use crate::theme::{theme, Theme, RADIUS_M, ROW_H, TEXT_M, TEXT_S, TEXT_XS, TOOLBAR_H};

/// The gutter a page's bar, bands and rows share.
pub const PAGE_PAD: f32 = 16.;

/// A quiet placeholder for a surface with nothing on it: the glyph in
/// tertiary ink, one line of what, one of why, and the jj command that
/// would change it.
pub fn empty_state(
    glyph: IconName,
    title: impl Into<SharedString>,
    body: impl Into<SharedString>,
    hint: Option<SharedString>,
    cx: &App,
) -> Div {
    let t = theme(cx);
    let mut el = v_flex()
        .size_full()
        .items_center()
        .justify_center()
        .gap(px(6.))
        .p(px(32.))
        .child(div().mb(px(8.)).child(icon(glyph, 20., t.text3)))
        .child(
            div()
                .text_size(px(TEXT_M))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(t.text1)
                .child(title.into()),
        )
        .child(
            div()
                .max_w(px(380.))
                .text_size(px(TEXT_S))
                .line_height(gpui::relative(1.55))
                .text_color(t.text2)
                .text_center()
                .child(body.into()),
        );
    if let Some(hint) = hint {
        el = el.child(
            div()
                .mt(px(8.))
                .text_size(px(TEXT_XS))
                .font_family(t.mono.clone())
                .text_color(t.text3)
                .child(hint),
        );
    }
    el
}

/// A section page's bar, on the toolbar line every pane shares: the
/// page's name and how much it holds, then whatever the page acts with.
pub fn page_bar(title: impl Into<SharedString>, count: Option<String>, cx: &App) -> Div {
    let t = theme(cx);
    let mut bar = h_flex()
        .h(px(TOOLBAR_H))
        .flex_shrink_0()
        .gap(px(10.))
        .pl(px(PAGE_PAD))
        .pr(px(8.))
        .border_b_1()
        .border_color(t.border2)
        .child(
            div()
                .flex_shrink_0()
                .text_size(px(TEXT_M))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(t.text1)
                .child(title.into()),
        );
    if let Some(count) = count {
        bar = bar.child(
            div()
                .flex_shrink_0()
                .font_family(t.mono.clone())
                .text_size(px(TEXT_XS))
                .text_color(t.text3)
                .child(count),
        );
    }
    bar
}

/// The filter a page bar carries: one quiet line, the accent while it has
/// the keys.
pub fn page_filter(input: &Entity<TextInput>, window: &Window, cx: &App) -> Div {
    let t = theme(cx);
    let focused = input.read(cx).is_focused(window);
    h_flex()
        .flex_1()
        .min_w(px(120.))
        .max_w(px(360.))
        .gap(px(7.))
        .h(px(26.))
        .px(px(8.))
        .bg(t.bg_well)
        .border_1()
        .border_color(if focused { t.accent } else { t.border2 })
        .rounded(px(RADIUS_M))
        .text_size(px(TEXT_S))
        .line_height(px(18.))
        .child(icon(IconName::Search, 12., t.text3))
        .child(input.clone())
}

/// A page's group heading: a band that stays put (under [`pinned`]) while
/// its rows pass beneath. The note says what the group is, once.
///
/// [`pinned`]: super::pinned
pub fn group_head(
    label: impl Into<SharedString>,
    count: Option<usize>,
    note: Option<SharedString>,
    cx: &App,
) -> Div {
    let t = theme(cx);
    let mut head = h_flex()
        .h(px(ROW_H))
        .gap(px(8.))
        .px(px(PAGE_PAD))
        .bg(t.recess)
        .border_b_1()
        .border_color(t.border2)
        .text_size(px(TEXT_S))
        .child(
            div()
                .flex_shrink_0()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(t.text2)
                .child(label.into()),
        );
    if let Some(count) = count {
        head = head.child(
            div()
                .flex_shrink_0()
                .font_family(t.mono.clone())
                .text_size(px(TEXT_XS))
                .text_color(t.text3)
                .child(count.to_string()),
        );
    }
    if let Some(note) = note {
        head = head.child(
            div()
                .flex_1()
                .min_w_0()
                .pl(px(4.))
                .text_color(t.text3)
                .child(truncated(note)),
        );
    }
    head
}

/// A notice across the top of a page: what went wrong, and the way to put
/// it away.
pub fn page_notice(
    id: &'static str,
    text: impl Into<SharedString>,
    cx: &App,
    on_dismiss: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
) -> Div {
    let t = theme(cx);
    h_flex()
        .min_h(px(ROW_H))
        .gap(px(8.))
        .py(px(5.))
        .pl(px(PAGE_PAD))
        .pr(px(8.))
        .bg(Theme::fade(t.danger, 0.08))
        .border_b_1()
        .border_color(Theme::fade(t.danger, 0.3))
        .text_size(px(TEXT_S))
        .text_color(t.danger)
        .child(div().flex_1().min_w_0().child(truncated(text)))
        .child(
            super::icon_btn(id, IconName::Close)
                .size(20., 11.)
                .color(t.danger)
                .tooltip("Dismiss")
                .on_click(on_dismiss),
        )
}

/// A tray: a consequence-stating panel that opens across a page, recessed
/// below the pane rather than floated over it.
pub fn tray(cx: &App) -> Div {
    let t = theme(cx);
    v_flex()
        .py(px(12.))
        .px(px(PAGE_PAD))
        .bg(t.bg_well)
        .border_b_1()
        .border_color(t.border2)
        .text_size(px(TEXT_S))
}

/// The plan step before a consequential action: a recessed panel that
/// states what will happen, in the workbench trays' vocabulary.
pub fn plan_panel(cx: &App) -> Div {
    let t = theme(cx);
    v_flex()
        .py(px(12.))
        .px(px(16.))
        .bg(t.bg_well)
        .border_1()
        .border_color(t.border2)
        .rounded(px(crate::theme::RADIUS_L))
}

/// A plan panel's heading: what will be done, and to what.
pub fn plan_title(
    action: impl Into<SharedString>,
    context: impl Into<SharedString>,
    cx: &App,
) -> Div {
    let t = theme(cx);
    h_flex()
        .gap(px(8.))
        .min_w_0()
        .text_size(px(TEXT_M))
        .child(
            div()
                .flex_shrink_0()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(t.text1)
                .child(action.into()),
        )
        .child(
            div()
                .min_w_0()
                .text_color(t.text3)
                .child(truncated(context.into())),
        )
}

/// What the action will do, one bullet per consequence.
pub fn consequences<T: Into<SharedString>>(lines: impl IntoIterator<Item = T>, cx: &App) -> Div {
    let t = theme(cx);
    let mut list = v_flex()
        .mt(px(8.))
        .gap(px(2.))
        .text_size(px(TEXT_S))
        .line_height(gpui::relative(1.5))
        .text_color(t.text2);
    for line in lines {
        list = list.child(
            div()
                .flex()
                .flex_row()
                .gap(px(8.))
                .child(div().flex_shrink_0().text_color(t.text3).child("•"))
                .child(div().flex_1().min_w_0().child(line.into())),
        );
    }
    list
}

/// A plan panel's closing row: the confirm hint, an inline error when the
/// action was refused, then the buttons.
pub fn plan_actions(
    hint: impl Into<SharedString>,
    error: Option<&str>,
    buttons: impl IntoIterator<Item = Btn>,
    cx: &App,
) -> Div {
    let t = theme(cx);
    let mut row = h_flex().gap(px(8.)).mt(px(12.)).child(
        div()
            .flex_1()
            .min_w_0()
            .text_size(px(crate::theme::TEXT_XS))
            .text_color(t.text3)
            .child(hint.into()),
    );
    if let Some(error) = error {
        row = row.child(
            div()
                .min_w_0()
                .max_w(px(340.))
                .text_size(px(crate::theme::TEXT_XS))
                .text_color(t.danger)
                .child(truncated(error.to_owned())),
        );
    }
    row.children(buttons)
}
