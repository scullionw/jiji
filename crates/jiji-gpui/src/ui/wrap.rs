//! A paragraph that keeps its natural width while there is room and wraps
//! when there is not.
//!
//! GPUI's own text wraps to whatever width the layout engine offers while
//! it is still probing, so a paragraph in a shrinkable flex item collapses
//! to its narrowest — a word, or a character, per line — as soon as a
//! sibling is free to grow. This one always reports its unwrapped size as
//! its content size and wraps only to the width the layout finally grants.

use gpui::{
    point, px, size, App, AvailableSpace, Bounds, Element, ElementId, Font, GlobalElementId, Hsla,
    InspectorElementId, IntoElement, LayoutId, Pixels, SharedString, Size, Style, TextAlign,
    TextRun, Window, WrappedLine,
};

/// The narrowest a paragraph is ever really laid out.
const NARROWEST: Pixels = px(120.);

pub struct Wrapped {
    text: SharedString,
}

/// `text` as a paragraph that wraps to the width it is given.
pub fn wrapped(text: impl Into<SharedString>) -> Wrapped {
    Wrapped { text: text.into() }
}

/// The face the paragraph was measured in; paint keeps it and takes only
/// the color of the moment (see `truncate::Face`).
#[derive(Clone)]
pub struct Face {
    font: Font,
    font_size: Pixels,
    line_height: Pixels,
}

fn shape(
    text: &SharedString,
    face: &Face,
    color: Hsla,
    wrap_width: Option<Pixels>,
    window: &Window,
) -> Vec<WrappedLine> {
    let run = TextRun {
        len: text.len(),
        font: face.font.clone(),
        color,
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    window
        .text_system()
        .shape_text(text.clone(), face.font_size, &[run], wrap_width, None)
        .map(|lines| lines.into_vec())
        .unwrap_or_default()
}

/// The box the shaped lines fill: the widest line, every line's height.
fn extent(lines: &[WrappedLine], line_height: Pixels) -> Size<Pixels> {
    lines.iter().fold(size(px(0.), px(0.)), |total, line| {
        let line = line.size(line_height);
        size(total.width.max(line.width), total.height + line.height)
    })
}

impl IntoElement for Wrapped {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for Wrapped {
    type RequestLayoutState = Face;
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        _cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let text_style = window.text_style();
        let face = Face {
            font: text_style.font(),
            font_size: text_style.font_size.to_pixels(window.rem_size()),
            line_height: window.line_height(),
        };
        let color = text_style.color;
        let natural = extent(
            &shape(&self.text, &face, color, None, window),
            face.line_height,
        );
        let natural = size(natural.width.ceil(), natural.height.max(face.line_height));
        let mut style = Style {
            flex_shrink: 1.,
            ..Style::default()
        };
        style.min_size.width = px(0.).into();
        let (text, measured) = (self.text.clone(), face.clone());
        let id = window.request_measured_layout(style, move |known, available, window, _| {
            // The width to wrap to: one the layout has fixed, or a definite
            // space it offers.
            let width = known.width.or(match available.width {
                AvailableSpace::Definite(width) => Some(width),
                AvailableSpace::MinContent | AvailableSpace::MaxContent => None,
            });
            match width {
                // Narrower than any paragraph: the engine is probing how
                // small the row could get, not granting a width — and the
                // height answered here would be taken as the row's own.
                Some(width) if width < NARROWEST => {
                    size(known.width.unwrap_or(natural.width), natural.height)
                }
                Some(width) if width < natural.width => {
                    let lines = shape(&text, &measured, color, Some(width), window);
                    size(width, extent(&lines, measured.line_height).height)
                }
                Some(width) if known.width.is_some() => size(width, natural.height),
                _ => natural,
            }
        });
        (id, face)
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        _window: &mut Window,
        _cx: &mut App,
    ) -> Self::PrepaintState {
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        face: &mut Self::RequestLayoutState,
        _prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let color = window.text_style().color;
        let lines = shape(&self.text, face, color, Some(bounds.size.width), window);
        let mut top = bounds.top();
        for line in &lines {
            let _ = line.paint(
                point(bounds.left(), top),
                face.line_height,
                TextAlign::Left,
                Some(bounds),
                window,
                cx,
            );
            top += line.size(face.line_height).height;
        }
    }
}
