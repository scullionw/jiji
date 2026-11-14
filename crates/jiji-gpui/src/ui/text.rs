//! Text that is more than a string: inline emphasis, ref labels, and
//! measuring a line before the layout engine has.

use gpui::{div, prelude::*, px, App, Div, FontWeight, Hsla, SharedString, Window};

use super::{icon, truncated, IconName};
use crate::theme::{theme, Theme, FONT_UI, RADIUS_S, TEXT_XS};

/// The height every label riding a row shares.
pub const LABEL_H: f32 = 18.;

/// A ref label: a bookmark, tag or remote name riding a row. A hairline
/// box with no fill, so a row of them stays lighter than the title beside
/// it.
pub fn ref_label(glyph: Option<IconName>, text: impl Into<SharedString>, cx: &App) -> Div {
    let t = theme(cx);
    let mut el = label_box(t.text2, t.border1)
        .gap(px(3.))
        .font_family(t.mono.clone());
    if let Some(glyph) = glyph {
        el = el.child(icon(glyph, 10., t.text3));
    }
    el.child(truncated(text))
}

/// The trunk bookmark's label: the one ref everything measures against,
/// in the accent.
pub fn trunk_label(text: impl Into<SharedString>, cx: &App) -> Div {
    let t = theme(cx);
    label_box(t.accent_strong, Theme::fade(t.accent, 0.5))
        .font_family(t.mono.clone())
        .child(truncated(text))
}

/// A state word riding a row ("conflict", "merged", "trunk"): the tone's
/// ink inside the tone's own hairline.
pub fn tag(text: impl Into<SharedString>, color: Hsla) -> Div {
    label_box(color, Theme::fade(color, 0.36)).child(text.into())
}

/// The box [`ref_label`] and [`tag`] share: callers add the content.
pub fn label_box(ink: Hsla, line: Hsla) -> Div {
    div()
        .flex()
        .flex_row()
        .flex_shrink_0()
        .items_center()
        .h(px(LABEL_H))
        .px(px(5.))
        .rounded(px(RADIUS_S))
        .border_1()
        .border_color(line)
        .text_color(ink)
        .text_size(px(TEXT_XS))
        .line_height(px(LABEL_H - 2.))
        .whitespace_nowrap()
}

/// A sentence with inline emphasis: `**strong**` reads in full ink and
/// weight (ids, names), `~~quiet~~` recedes (quoted titles). The text
/// wraps like any paragraph.
pub fn rich(markup: &str, cx: &App) -> gpui::StyledText {
    let t = theme(cx);
    let mut text = String::with_capacity(markup.len());
    let mut highlights = Vec::new();
    let mut strong: Option<usize> = None;
    let mut quiet: Option<usize> = None;
    let mut rest = markup;
    while !rest.is_empty() {
        if let Some(after) = rest.strip_prefix("**") {
            match strong.take() {
                Some(start) => highlights.push((
                    start..text.len(),
                    gpui::HighlightStyle {
                        color: Some(t.text1),
                        font_weight: Some(FontWeight::SEMIBOLD),
                        ..Default::default()
                    },
                )),
                None => strong = Some(text.len()),
            }
            rest = after;
        } else if let Some(after) = rest.strip_prefix("~~") {
            match quiet.take() {
                Some(start) => highlights.push((
                    start..text.len(),
                    gpui::HighlightStyle {
                        color: Some(t.text3),
                        ..Default::default()
                    },
                )),
                None => quiet = Some(text.len()),
            }
            rest = after;
        } else {
            let ch = rest.chars().next().unwrap();
            text.push(ch);
            rest = &rest[ch.len_utf8()..];
        }
    }
    highlights.sort_by_key(|(range, _)| range.start);
    gpui::StyledText::new(text).with_highlights(highlights)
}

/// How wide `text` runs on one line in the UI face — for layouts that must
/// choose before the layout engine has measured anything.
pub fn text_width(text: &str, size: f32, weight: FontWeight, window: &Window) -> f32 {
    text_width_in(text, &SharedString::from(FONT_UI), size, weight, window)
}

/// [`text_width`] in a named face.
pub fn text_width_in(
    text: &str,
    family: &SharedString,
    size: f32,
    weight: FontWeight,
    window: &Window,
) -> f32 {
    if text.is_empty() {
        return 0.;
    }
    let run = gpui::TextRun {
        len: text.len(),
        font: gpui::Font {
            weight,
            ..gpui::font(family.clone())
        },
        color: Hsla::default(),
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    window
        .text_system()
        .shape_line(SharedString::from(text.to_owned()), px(size), &[run], None)
        .width
        .into()
}
