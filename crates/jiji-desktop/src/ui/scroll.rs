//! Overlay-thin scrollbars: present, never loud. One element that reads a
//! [`ScrollHandle`] and paints (and drags) the thumb for it; lay it over
//! the scroller's right edge.

use std::cell::Cell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui::{
    canvas, fill, point, px, relative, size, App, Axis, Bounds, DispatchPhase, Element, ElementId,
    GlobalElementId, Hitbox, HitboxBehavior, InspectorElementId, IntoElement, LayoutId,
    MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Position, ScrollEvent,
    ScrollHandle, StatefulInteractiveElement, Style, Styled, TouchPhase, Window,
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

/// A gesture is over once the device has said nothing for this long.
const GESTURE_GAP: Duration = Duration::from_millis(120);
/// Travel across a gesture's axis, beyond its travel along it, that turns
/// the gesture onto the other axis.
const TURN_AFTER: f32 = 12.;

/// The axis a scroll gesture travels. A view that scrolls both ways puts
/// its wheel events through one, so a vertical fling never drifts the
/// content sideways and a sideways swipe never nudges the rows.
///
/// A gesture begins where the device says fingers landed, or after a
/// pause: smooth-scrolling mice send precise deltas and never report a
/// landing, so a lock that waited for one would hold its first axis for
/// good. The gesture follows the axis it sets off on and turns once travel
/// across that axis has clearly outrun travel along it.
#[derive(Default)]
pub struct AxisLock {
    axis: Option<Axis>,
    /// Travel across the axis that travel along it has not answered.
    stray: f32,
    last: Option<Instant>,
}

impl AxisLock {
    /// The travel `event` asks for in pixels, on the gesture's axis only.
    /// A notched wheel reports one axis at a time and passes as it is.
    pub fn travel(&mut self, event: &ScrollEvent, line_height: f32) -> (f32, f32) {
        let delta = event.delta.pixel_delta(px(line_height));
        let delta = (f32::from(delta.x), f32::from(delta.y));
        if !event.delta.precise() {
            return delta;
        }
        let now = Instant::now();
        let paused = self
            .last
            .is_none_or(|last| now.duration_since(last) > GESTURE_GAP);
        self.last = Some(now);
        let landed = matches!(event.touch_phase, TouchPhase::Started);
        self.steer(paused || landed, delta)
    }

    fn steer(&mut self, fresh: bool, (dx, dy): (f32, f32)) -> (f32, f32) {
        if fresh {
            self.axis = None;
            self.stray = 0.;
        }
        if dx == 0. && dy == 0. {
            return (0., 0.);
        }
        let axis = match self.axis {
            None if dx.abs() > dy.abs() => Axis::Horizontal,
            None => Axis::Vertical,
            Some(axis) => {
                let (along, across) = match axis {
                    Axis::Horizontal => (dx.abs(), dy.abs()),
                    Axis::Vertical => (dy.abs(), dx.abs()),
                };
                self.stray = (self.stray + across - along).max(0.);
                if self.stray >= TURN_AFTER {
                    self.stray = 0.;
                    axis.invert()
                } else {
                    axis
                }
            }
        };
        self.axis = Some(axis);
        match axis {
            Axis::Horizontal => (dx, 0.),
            Axis::Vertical => (0., dy),
        }
    }
}

/// Hear the wheel over the parent's box before anything inside it does.
/// A scroller handles the wheel on its own listener, which runs ahead of
/// its ancestors'; this is how an ancestor takes a sideways swipe off a
/// list before the list spends the swipe's drift on its rows. Add it as
/// the last child of a `relative` parent; stop propagation to keep the
/// event.
pub fn wheel_first(
    on_wheel: impl Fn(&ScrollEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    canvas(
        |bounds, window, _| window.insert_hitbox(bounds, HitboxBehavior::Normal),
        move |_, hitbox, window, _| {
            window.on_mouse_event(move |event: &ScrollEvent, phase, window, cx| {
                if phase == DispatchPhase::Capture && hitbox.should_handle_scroll(window) {
                    on_wheel(event, window, cx);
                }
            });
        },
    )
    .absolute()
    .inset_0()
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
    let max = handle.max_offset().y;
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
                let max = handle.max_offset().y;
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

/// A sideways overlay scrollbar for an offset the caller keeps rather than
/// a scroll handle: `offset` of `max`, over content panning inside the
/// bar's own width. Lay it along the bottom of what pans; `on_pan` hears
/// each offset a drag of the thumb asks for.
pub fn pan_scrollbar(
    id: impl Into<ElementId>,
    offset: f32,
    max: f32,
    on_pan: impl Fn(f32, &mut Window, &mut App) + 'static,
) -> PanScrollbar {
    PanScrollbar {
        id: id.into(),
        offset,
        max,
        on_pan: Rc::new(on_pan),
    }
}

/// What a sideways scrollbar tells its owner: the offset a drag asks for.
type OnPan = Rc<dyn Fn(f32, &mut Window, &mut App)>;

pub struct PanScrollbar {
    id: ElementId,
    offset: f32,
    max: f32,
    on_pan: OnPan,
}

/// Where a sideways thumb sits on its track, and how far the content pans
/// per point of thumb travel. The track is the viewport: the thumb is to
/// the track as the viewport is to the content.
fn pan_geometry(bounds: Bounds<Pixels>, offset: f32, max: f32) -> Option<(Bounds<Pixels>, f32)> {
    let track: f32 = bounds.size.width.into();
    if max <= 0.5 || track <= 0. {
        return None;
    }
    let width = (track * track / (track + max)).max(MIN_THUMB).min(track);
    let travel = track - width;
    let left = f32::from(bounds.left()) + travel * (offset / max).clamp(0., 1.);
    let thumb = Bounds::new(
        point(px(left), bounds.top() + px(THUMB_INSET)),
        size(px(width), px(TRACK_W - THUMB_INSET * 2.)),
    );
    Some((thumb, if travel > 0. { max / travel } else { 0. }))
}

impl Element for PanScrollbar {
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
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.size.height = px(TRACK_W).into();
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
        // Only the thumb takes the pointer; the track lets the lines under
        // it be clicked.
        pan_geometry(bounds, self.offset, self.max)
            .map(|(thumb, _)| window.insert_hitbox(thumb, HitboxBehavior::Normal))
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
        let (Some((thumb, ratio)), Some(hitbox)) =
            (pan_geometry(bounds, self.offset, self.max), hitbox.clone())
        else {
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
        let hot = state.grab.get().is_some() || hitbox.is_hovered(window);
        window.paint_quad(
            fill(thumb, if hot { t.scrollbar_hover } else { t.scrollbar }).corner_radii(px(4.)),
        );

        let (max, track) = (self.max, bounds.left());
        window.on_mouse_event({
            let state = state.clone();
            move |event: &MouseDownEvent, phase, window, cx| {
                if phase == DispatchPhase::Bubble
                    && event.button == MouseButton::Left
                    && hitbox.is_hovered(window)
                {
                    state.grab.set(Some(event.position.x - thumb.left()));
                    cx.stop_propagation();
                    window.refresh();
                }
            }
        });
        window.on_mouse_event({
            let state = state.clone();
            let on_pan = self.on_pan.clone();
            move |event: &MouseMoveEvent, phase, window, cx| {
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
                let left: f32 = (event.position.x - grab - track).into();
                on_pan((left * ratio).clamp(0., max), window, cx);
            }
        });
        window.on_mouse_event(move |_: &MouseUpEvent, phase, window, _cx| {
            if phase == DispatchPhase::Capture && state.grab.take().is_some() {
                window.refresh();
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pan_thumb_is_to_its_track_as_the_view_is_to_the_content() {
        let track = Bounds::new(point(px(100.), px(0.)), size(px(200.), px(TRACK_W)));
        // 200 points of view over 600 of content: a third of the track.
        let (thumb, ratio) = pan_geometry(track, 0., 400.).unwrap();
        assert_eq!(thumb.size.width, px(200. / 3.));
        assert_eq!(thumb.left(), px(100.));
        let (end, _) = pan_geometry(track, 400., 400.).unwrap();
        assert_eq!(end.right(), px(300.));
        // A thumb's full travel pans the whole way.
        assert_eq!(ratio * (200. - 200. / 3.), 400.);
        assert!(pan_geometry(track, 0., 0.).is_none());
    }

    #[test]
    fn a_gesture_keeps_the_axis_it_sets_off_on() {
        let mut lock = AxisLock::default();
        assert_eq!(lock.steer(true, (1., -9.)), (0., -9.));
        // Drift across a vertical fling is dropped.
        assert_eq!(lock.steer(false, (3., -20.)), (0., -20.));
        assert_eq!(lock.steer(false, (2., 0.)), (0., 0.));
        assert_eq!(lock.steer(false, (1., -6.)), (0., -6.));
    }

    #[test]
    fn the_next_gesture_picks_its_own_axis() {
        let mut lock = AxisLock::default();
        assert_eq!(lock.steer(true, (0., -30.)), (0., -30.));
        // A device that never reports a landing: the pause is the boundary.
        assert_eq!(lock.steer(true, (-14., 0.)), (-14., 0.));
        assert_eq!(lock.steer(false, (-14., 3.)), (-14., 0.));
    }

    #[test]
    fn sustained_travel_across_the_axis_turns_the_gesture() {
        let mut lock = AxisLock::default();
        // A sideways swipe whose first report leaned down.
        assert_eq!(lock.steer(true, (1., 1.)), (0., 1.));
        assert_eq!(lock.steer(false, (-5., 0.)), (0., 0.));
        assert_eq!(lock.steer(false, (-5., 0.)), (0., 0.));
        assert_eq!(lock.steer(false, (-5., 1.)), (-5., 0.));
        assert_eq!(lock.steer(false, (-8., 2.)), (-8., 0.));
    }

    #[test]
    fn an_idle_report_decides_nothing() {
        let mut lock = AxisLock::default();
        assert_eq!(lock.steer(true, (0., 0.)), (0., 0.));
        assert_eq!(lock.steer(false, (-4., 1.)), (-4., 0.));
    }
}
