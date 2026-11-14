//! The furniture of a full pane: empty states, section openings, and the
//! plan panel that states what a consequential action will do.

use gpui::{div, prelude::*, px, App, Div, FontWeight, SharedString};

use super::{h_flex, icon, truncated, v_flex, Btn, IconName};
use crate::theme::{theme, TEXT_M, TEXT_S, TEXT_XS};

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

/// A full-page section's opening: its name and one paragraph of what the
/// surface does. The icon echoes the rail, small and quiet.
pub fn section_header(
    glyph: IconName,
    title: impl Into<SharedString>,
    description: impl Into<SharedString>,
    cx: &App,
) -> Div {
    let t = theme(cx);
    v_flex()
        .mb(px(20.))
        .child(
            h_flex()
                .gap(px(8.))
                .text_size(px(crate::theme::TEXT_TITLE))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(t.text1)
                .child(icon(glyph, 16., t.text3))
                .child(title.into()),
        )
        .child(
            div()
                .mt(px(4.))
                .max_w(px(760.))
                .text_size(px(TEXT_S))
                .line_height(gpui::relative(1.55))
                .text_color(t.text2)
                .child(description.into()),
        )
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
