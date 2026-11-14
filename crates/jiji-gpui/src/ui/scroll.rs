//! Overlay-thin scrollbars: present, never loud. One element that reads a
//! [`ScrollHandle`] and paints (and drags) the thumb for it; lay it over
//! the scroller's right edge.

use std::cell::Cell;
use std::rc::Rc;

use gpui::{
    fill, point, px, relative, size, App, Bounds, DispatchPhase, Element, ElementId,
    GlobalElementId, Hitbox, HitboxBehavior, InspectorElementId, IntoElement, LayoutId,
    MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Position, ScrollHandle,
    StatefulInteractiveElement, Style, Styled, Window,
};

use crate::theme::theme;

const TRACK_W: f32 = 10.;
const THUMB_INSET: f32 = 3.;
const MIN_THUMB: f32 = 28.;

/// Scrolling down a list, and only down it. GPUI otherwise spends a
/// purely sideways swipe on vertical travel, so a list would lurch when
/// the fingers drift across it.
pub trait ScrollY: StatefulInteractiveElement + Styled {
    fn scroll_y(mut self) -> Self {
        self.style().restrict_scroll_to_axis = Some(true);
        self.overflow_y_scroll()
    }
}

impl<E: StatefulInteractiveElement + Styled> ScrollY for E {}

/// The same restraint for a `uniform_list`, which scrolls on its own.
pub fn vertical_only<E: Styled>(mut list: E) -> E {
    list.style().restrict_scroll_to_axis = Some(true);
    list
}

/// A vertical overlay scrollbar for `handle`. Place it as the last child
/// of the scroller's (relative) parent.
pub fn scrollbar(id: impl Into<ElementId>, handle: &ScrollHandle) -> Scrollbar {
    Scrollbar {
        id: id.into(),
        handle: handle.clone(),
    }
}

pub struct Scrollbar {
    id: ElementId,
    handle: ScrollHandle,
}

/// The press that started a thumb drag: where on the thumb it landed.
#[derive(Default)]
struct DragState {
    grab: Cell<Option<Pixels>>,
}

struct Geometry {
    track: Bounds<Pixels>,
    thumb: Bounds<Pixels>,
    /// Scroll distance per pixel of thumb travel.
    ratio: f32,
}

fn geometry(bounds: Bounds<Pixels>, handle: &ScrollHandle) -> Option<Geometry> {
    let max = handle.max_offset().height;
    let viewport = handle.bounds().size.height;
    if max <= px(0.5) || viewport <= px(0.) {
        return None;
    }
    let track_h = bounds.size.height;
    let content = viewport + max;
    let thumb_h = (track_h * (viewport / content))
        .max(px(MIN_THUMB))
        .min(track_h);
    let travel = track_h - thumb_h;
    let progress = (-handle.offset().y / max).clamp(0., 1.);
    let thumb = Bounds::new(
        point(
            bounds.right() - px(TRACK_W - THUMB_INSET),
            bounds.top() + travel * progress,
        ),
        size(px(TRACK_W - THUMB_INSET * 2.), thumb_h),
    );
    Some(Geometry {
        track: bounds,
        thumb,
        ratio: if travel > px(0.) { max / travel } else { 0. },
    })
}

impl IntoElement for Scrollbar {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for Scrollbar {
    type RequestLayoutState = ();
    type PrepaintState = Option<Hitbox>;

    fn id(&self) -> Option<ElementId> {
        Some(self.id.clone())
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
        let mut style = Style {
            position: Position::Absolute,
            ..Style::default()
        };
        style.inset.top = px(0.).into();
        style.inset.bottom = px(0.).into();
        style.inset.right = px(0.).into();
        style.size.width = px(TRACK_W).into();
        style.size.height = relative(1.).into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        _cx: &mut App,
    ) -> Self::PrepaintState {
        // Only the thumb takes the pointer; the rest of the track lets
        // clicks through to the rows underneath.
        geometry(bounds, &self.handle)
            .map(|geo| window.insert_hitbox(geo.thumb, HitboxBehavior::Normal))
    }

    fn paint(
        &mut self,
        id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        hitbox: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let Some(geo) = geometry(bounds, &self.handle) else {
            return;
        };
        let Some(hitbox) = hitbox.clone() else {
            return;
        };
        let state: Rc<DragState> = window.with_element_state(
            id.expect("scrollbars carry an id"),
            |state: Option<Rc<DragState>>, _| {
                let state = state.unwrap_or_default();
                (state.clone(), state)
            },
        );
        let t = theme(cx);
        let dragging = state.grab.get().is_some();
        let hot = dragging || hitbox.is_hovered(window);
        window.paint_quad(
            fill(geo.thumb, if hot { t.scrollbar_hover } else { t.scrollbar }).corner_radii(px(4.)),
        );

        let handle = self.handle.clone();
        let thumb = geo.thumb;
        let track = geo.track;
        let ratio = geo.ratio;

        window.on_mouse_event({
            let state = state.clone();
            let hitbox = hitbox.clone();
            move |event: &MouseDownEvent, phase, window, cx| {
                if phase != DispatchPhase::Bubble || event.button != MouseButton::Left {
                    return;
                }
                if hitbox.is_hovered(window) {
                    state.grab.set(Some(event.position.y - thumb.top()));
                    cx.stop_propagation();
                    window.refresh();
                }
            }
        });
        window.on_mouse_event({
            let state = state.clone();
            let handle = handle.clone();
            move |event: &MouseMoveEvent, phase, window, _cx| {
                if phase != DispatchPhase::Capture {
                    return;
                }
                let Some(grab) = state.grab.get() else {
                    return;
                };
                if event.pressed_button != Some(MouseButton::Left) {
                    state.grab.set(None);
                    window.refresh();
                    return;
                }
                let top = event.position.y - grab - track.top();
                let max = handle.max_offset().height;
                let target = (top * ratio).clamp(px(0.), max);
                let mut offset = handle.offset();
                offset.y = -target;
                handle.set_offset(offset);
                window.refresh();
            }
        });
        window.on_mouse_event({
            let state = state.clone();
            move |_: &MouseUpEvent, phase, window, _cx| {
                if phase == DispatchPhase::Capture && state.grab.take().is_some() {
                    window.refresh();
                }
            }
        });
    }
}
