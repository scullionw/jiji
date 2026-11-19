//! Motion, kept for what it tells: work under way (a spinning fetch, a
//! working job's pulse, a placeholder breathing while its read is out),
//! and structure changing (a tray unfolding, graph rows moving to where a
//! rewrite put them). Switching section, view or overlay is instant, as is
//! closing anything, and nothing moves while the app is idle.
//!
//! Under the system's Reduce Motion everything here is still: GPUI's own
//! animations read `App::reduce_motion`, which follows that setting. A
//! scripted run holds it on, so screenshots are the same every time,
//! unless a step lets motion run.

use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use gpui::{
    div, prelude::*, px, svg, Animation, AnimationExt as _, App, Bounds, ElementId, Global, Hsla,
    Pixels, Styled, TimedTransformation, Transformation, Window,
};

use super::icon::IconName;

/// A tray or panel opening.
pub const UNFOLD: Duration = Duration::from_millis(100);
/// Graph rows moving to the places a rewrite gave them.
pub const STRUCTURE: Duration = Duration::from_millis(160);

/// Decelerating: quick off the mark, settling into place.
pub fn ease_out(t: f32) -> f32 {
    1. - (1. - t).powi(3)
}

/// Motion is held still for a scripted run.
struct Held;

impl Global for Held {}

/// Follow the system's Reduce Motion, unless a scripted run holds motion
/// still. Read at launch and whenever the window comes forward, which is
/// where the setting is changed from.
pub fn follow_system(cx: &mut App) {
    if !cx.has_global::<Held>() {
        cx.set_reduce_motion(crate::appkit::reduce_motion());
    }
}

/// A scripted run: everything still, whatever the system says.
pub fn hold(cx: &mut App) {
    cx.set_global(Held);
    cx.set_reduce_motion(true);
}

/// A scripted run's step lets motion run from here on.
pub fn release(cx: &mut App) {
    cx.set_reduce_motion(false);
}

/// A tray or panel unfolding as it opens: its height grows from nothing
/// to its own, pushing what is below aside rather than jumping it, and its
/// content fades in a little ahead of the fold. Each opening under a new
/// `id` unfolds again; closing takes it away at once.
pub fn unfold(
    id: impl Into<ElementId>,
    panel: impl IntoElement,
    window: &mut Window,
) -> impl IntoElement {
    let id = id.into();
    // The panel's own height, read where it lays out unclipped inside the
    // fold, and used from the frame after.
    let height = window.with_global_id(id.clone(), |global_id, window| {
        window.with_element_state(global_id, |kept: Option<Rc<Cell<Bounds<Pixels>>>>, _| {
            let kept = kept.unwrap_or_default();
            (kept.clone(), kept)
        })
    });
    let measured = height.get().size.height;
    div()
        .debug_selector(|| "unfold".into())
        .child(
            div()
                .relative()
                .child(panel)
                .child(super::bounds_probe(height)),
        )
        .with_animation(
            id,
            Animation::new(UNFOLD).with_easing(ease_out),
            move |fold, t| {
                if t >= 1. {
                    fold
                } else {
                    fold.max_h(measured * t)
                        .overflow_hidden()
                        .opacity((t * 1.6).min(1.))
                }
            },
        )
}

/// A loading placeholder breathing while what it stands for is read: a
/// slow fade to half strength and back. Repaints are capped, as nothing
/// here needs a display's every frame.
pub fn breathing<E: IntoElement + Styled + 'static>(
    id: impl Into<ElementId>,
    placeholder: E,
) -> impl IntoElement {
    placeholder.with_animation(
        id,
        Animation::new(Duration::from_millis(1600))
            .repeat()
            .with_max_fps(30.),
        |placeholder, t| placeholder.opacity(1. - 0.45 * (t * std::f32::consts::PI).sin()),
    )
}

/// A status dot with a slow heartbeat, for a job that is working. The
/// compositor fades it: no view is drawn again for it.
pub fn pulse_dot(color: Hsla, size: f32) -> impl IntoElement {
    svg()
        .path(IconName::Disc.path())
        .size(px(size))
        .flex_shrink_0()
        .text_color(color)
        .with_timed_transformation(
            TimedTransformation::new(Duration::from_secs(2))
                .from(Transformation::default())
                .to(Transformation::alpha(0.35))
                .with_easing(gpui::Easing::EaseInOut)
                .alternate(),
        )
}

/// An icon turning, for a fetch under way; the compositor turns it.
pub fn spinning(icon: gpui::Svg) -> gpui::Svg {
    icon.with_timed_transformation(TimedTransformation::rotation(
        Duration::from_millis(900),
        Transformation::default(),
    ))
}
