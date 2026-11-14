//! Text that is more than a string: inline emphasis, ref labels, and
//! measuring a line before the layout engine has.

use gpui::{div, prelude::*, px, App, Div, FontWeight, Hsla, SharedString, Window};

use super::{icon, truncated, IconName};
use crate::theme::{theme, FONT_UI, RADIUS_S, TEXT_XS};

/// A ref label: a bookmark, tag or remote name riding a row.
pub fn ref_label(glyph: Option<IconName>, text: impl Into<SharedString>, cx: &App) -> Div {
    let t = theme(cx);
    let mut el = div()
        .flex()
        .flex_row()
        .flex_shrink_0()
        .items_center()
        .gap(px(3.))
        .h(px(17.))
        .px(px(5.))
        .rounded(px(RADIUS_S))
        .bg(t.ref_bg)
        .text_color(t.text2)
        .font_family(t.mono.clone())
        .text_size(px(TEXT_XS))
        .line_height(px(17.))
        .whitespace_nowrap();
    if let Some(glyph) = glyph {
        el = el.child(icon(glyph, 10., t.text3));
    }
    el.child(truncated(text))
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
