//! Scripted runs with nothing on screen.
//!
//! `JIJI_HEADLESS=1` runs the real app — the real platform, executors,
//! watchers and text system — with a window that is never shown: no Dock
//! icon, no focus taken, nothing over the user's work, and nothing of the
//! user's pointer or keyboard reaching it. Two things a visible window gets
//! from the display have to come from here instead:
//!
//! - **Frames.** macOS only asks a window for frames while it is visible,
//!   so [`pump_frames`] stands in for the display link.
//! - **Pixels.** [`screenshot`] renders the last frame's scene into a
//!   texture and writes it out. It is the GPUI scene only: the traffic
//!   lights and the window's rounded corners are AppKit's and are not in
//!   it.
//!
//! Both need GPUI's `test-support` (offscreen rendering, frame stepping),
//! which a shipping build must not carry: they are compiled only with this
//! crate's `headless` feature.

use std::path::Path;

use gpui::{AnyWindowHandle, App, Window};

/// Whether this build can run headless and take screenshots.
pub const SUPPORTED: bool = cfg!(feature = "headless");

/// How to get a build that can.
pub const HOW: &str = "build with `--features headless`";

/// Write the window's current frame to `path` as a PNG, at the display's
/// scale.
#[cfg(feature = "headless")]
pub fn screenshot(window: &Window, path: &Path) -> Result<(), String> {
    let image = window.render_to_image().map_err(|err| err.to_string())?;
    image.save(path).map_err(|err| err.to_string())
}

#[cfg(not(feature = "headless"))]
pub fn screenshot(_window: &Window, _path: &Path) -> Result<(), String> {
    Err(format!("this build cannot take screenshots: {HOW}"))
}

/// Deliver frames to a window the display never asks for any, the way
/// GPUI's own frame handler does: next-frame callbacks first, then a draw
/// if — and only if — something asked for one. A view that changes state
/// without notifying stays as stale here as it would on screen.
#[cfg(feature = "headless")]
pub fn pump_frames(window: AnyWindowHandle, cx: &mut App) {
    use gpui::AppContext as _;
    use std::time::Duration;

    /// One display refresh at 60 Hz.
    const FRAME: Duration = Duration::from_millis(16);

    cx.spawn(async move |cx| loop {
        let Some(demand) = cx.update(|cx| FrameDemand::of(window, cx)) else {
            break;
        };
        if demand.callbacks {
            let ran = cx.update_window(window, |_, window, cx| {
                window.simulate_next_frame(cx);
            });
            if ran.is_err() {
                break;
            }
        }
        // A callback may be what dirtied the window: ask again.
        let dirty = if demand.callbacks {
            cx.update(|cx| FrameDemand::of(window, cx))
                .is_some_and(|demand| demand.dirty)
        } else {
            demand.dirty
        };
        if dirty {
            let drawn = cx.update_window(window, |_, window, cx| window.draw(cx).clear(cx));
            if drawn.is_err() {
                break;
            }
        }
        cx.background_executor().timer(FRAME).await;
    })
    .detach();
}

#[cfg(not(feature = "headless"))]
pub fn pump_frames(_window: AnyWindowHandle, _cx: &mut App) {}

/// What a window is waiting on a frame for.
#[cfg(feature = "headless")]
struct FrameDemand {
    /// `on_next_frame` callbacks are queued.
    callbacks: bool,
    /// Something invalidated the window since its last draw.
    dirty: bool,
}

#[cfg(feature = "headless")]
impl FrameDemand {
    /// `None` once the window is gone.
    ///
    /// GPUI keeps a window's dirty flag to itself; the one place it shows is
    /// the diagnostic snapshot, whose layout is not promised. If the flag is
    /// not where it was, every frame is treated as wanted: the run still
    /// works, it only stops telling a missed notify from a made one.
    fn of(window: AnyWindowHandle, cx: &App) -> Option<Self> {
        if !cx.windows().contains(&window) {
            return None;
        }
        let id = window.window_id().as_u64();
        let state = cx.debug_state();
        let frame = state
            .get("windows")
            .and_then(|windows| windows.as_array())
            .and_then(|windows| {
                windows
                    .iter()
                    .find(|entry| entry.get("id").and_then(|value| value.as_u64()) == Some(id))
            })
            .and_then(|entry| entry.get("frame"));
        let read = |key: &str| frame.and_then(|frame| frame.get(key));
        Some(Self {
            callbacks: read("next_frame_callbacks")
                .and_then(|count| count.as_u64())
                .is_none_or(|count| count > 0),
            dirty: read("dirty")
                .and_then(|dirty| dirty.as_bool())
                .unwrap_or(true),
        })
    }
}
