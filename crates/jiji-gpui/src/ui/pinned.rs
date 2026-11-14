//! A group whose heading stays in view while the group scrolls past —
//! CSS's `position: sticky` for the content of a scroller.

use gpui::{
    point, px, AnyElement, App, Bounds, Display, Element, ElementId, GlobalElementId,
    InspectorElementId, IntoElement, LayoutId, Pixels, Style, Window,
};

pub struct Pinned {
    heading: AnyElement,
    body: AnyElement,
}

/// `heading` above `body`. Once the group's top has scrolled out of the
/// scroller, the heading rides the scroller's top edge until the group's
/// end pushes it out. Give the heading an opaque fill: rows pass under it.
pub fn pinned(heading: impl IntoElement, body: impl IntoElement) -> Pinned {
    Pinned {
        heading: heading.into_any_element(),
        body: body.into_any_element(),
    }
}

impl IntoElement for Pinned {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for Pinned {
    /// The heading's layout, for its height.
    type RequestLayoutState = LayoutId;
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
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let heading = self.heading.request_layout(window, cx);
        let body = self.body.request_layout(window, cx);
        let style = Style {
            display: Display::Block,
            ..Style::default()
        };
        (window.request_layout(style, [heading, body], cx), heading)
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        heading: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        self.body.prepaint(window, cx);
        // The scroller clips its content, so its top edge is the clip's.
        let viewport_top = window.content_mask().bounds.top();
        let heading_height = window.layout_bounds(*heading).size.height;
        let travel = (bounds.size.height - heading_height).max(px(0.));
        let slide = (viewport_top - bounds.top()).clamp(px(0.), travel);
        // Prepainted after the body, so it takes the pointer over the rows
        // beneath it.
        window.with_element_offset(point(px(0.), slide), |window| {
            self.heading.prepaint(window, cx)
        });
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _heading: &mut Self::RequestLayoutState,
        _prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        self.body.paint(window, cx);
        self.heading.paint(window, cx);
    }
}
