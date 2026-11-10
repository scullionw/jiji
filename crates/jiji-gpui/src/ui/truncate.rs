//! One line of text that gives way with an ellipsis.
//!
//! Unlike `.truncate()` on a div, this measures to whatever width the
//! layout finally grants and cuts at paint time, so it works as a
//! shrinking flex item and under a `max_w` — the two places a row needs
//! it.

use gpui::{
    px, size, App, Bounds, Element, ElementId, GlobalElementId, InspectorElementId, IntoElement,
    LayoutId, Pixels, SharedString, Style, Window,
};

pub struct Truncated {
    text: SharedString,
}

/// `text` on one line, ending in "…" when it does not fit.
pub fn truncated(text: impl Into<SharedString>) -> Truncated {
    Truncated { text: text.into() }
}

impl IntoElement for Truncated {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for Truncated {
    type RequestLayoutState = ();
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
        let font_size = text_style.font_size.to_pixels(window.rem_size());
        let line_height = window.line_height();
        let full = window
            .text_system()
            .shape_line(
                self.text.clone(),
                font_size,
                &[text_style.to_run(self.text.len())],
                None,
            )
            .width
            .ceil();
        let mut style = Style {
            flex_shrink: 1.,
            ..Style::default()
        };
        style.min_size.width = px(0.).into();
        let id = window.request_measured_layout(style, move |known, available, _, _| {
            // Its content size is the whole line, whatever space is on
            // offer: the layout grants the final width (a flex row shrinks
            // it past the explicit zero minimum above, a block parent
            // stretches it to its own width) and paint cuts to fit. Sizing
            // to the offered space instead would collapse it to nothing
            // whenever a parent is probed at zero width.
            let _ = available;
            let width = known.width.unwrap_or(full);
            size(width, line_height)
        });
        (id, ())
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
        _request_layout: &mut Self::RequestLayoutState,
        _prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let text_style = window.text_style();
        let font_size = text_style.font_size.to_pixels(window.rem_size());
        let line_height = window.line_height();
        let shape = |text: SharedString, window: &mut Window| {
            let run = text_style.to_run(text.len());
            window
                .text_system()
                .shape_line(text, font_size, &[run], None)
        };
        let mut line = shape(self.text.clone(), window);
        if line.width > bounds.size.width + px(0.5) {
            let ellipsis = shape("…".into(), window).width;
            let room = bounds.size.width - ellipsis;
            if room <= px(0.) {
                return;
            }
            let mut cut = line.index_for_x(room).unwrap_or(self.text.len());
            while cut > 0 && !self.text.is_char_boundary(cut) {
                cut -= 1;
            }
            let kept = self.text[..cut].trim_end();
            line = shape(format!("{kept}…").into(), window);
        }
        let _ = line.paint(bounds.origin, line_height, window, cx);
    }
}
