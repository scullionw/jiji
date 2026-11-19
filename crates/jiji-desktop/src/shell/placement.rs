//! Where the window opens, and what is kept of where it was left.
//!
//! GPUI places a window on one display, in that display's own coordinates,
//! and macOS never reports a zoomed window as zoomed: GPUI's windowed
//! bounds are the zoomed frame. So the state kept is explicit
//! (`store::WindowState`): the display, the frame the window has when it
//! is neither zoomed nor full screen, and whether it was either. A zoomed
//! window reopens on that frame and zooms from it, which is what lets
//! unzooming take it back there.

use std::time::Duration;

use gpui::{point, px, size, App, Bounds, DisplayId, Pixels, Size, Window, WindowBounds};

use crate::store::WindowState;

/// The window a first launch opens, and the size one falls back to.
pub const DEFAULT_SIZE: Size<Pixels> = size(px(1280.), px(820.));

/// The smallest the window's layout holds together at.
pub const MIN_SIZE: Size<Pixels> = size(px(980.), px(640.));

/// How long the window holds still before where it is counts as where it
/// was left. Zooming and full screen animate through frames on the way,
/// and none of those may become the frame a zoomed window unzooms to.
pub const SETTLE: Duration = Duration::from_millis(500);

/// A display as placement sees it: all of it, and the part the menu bar
/// and the Dock leave, both from its own top-left.
#[derive(Clone, Debug)]
pub struct Screen {
    pub id: DisplayId,
    pub uuid: Option<String>,
    pub bounds: Bounds<Pixels>,
    pub visible: Bounds<Pixels>,
}

/// The displays, the primary first.
pub fn screens(cx: &App) -> Vec<Screen> {
    let primary = cx.primary_display().map(|display| display.id());
    let mut screens: Vec<Screen> = cx
        .displays()
        .iter()
        .map(|display| Screen {
            id: display.id(),
            uuid: display.uuid().ok().map(|uuid| uuid.to_string()),
            bounds: display.bounds(),
            visible: display.visible_bounds(),
        })
        .collect();
    screens.sort_by_key(|screen| Some(screen.id) != primary);
    screens
}

/// Where the window opens and how.
#[derive(Clone, Debug, PartialEq)]
pub struct Placement {
    pub display: Option<DisplayId>,
    pub bounds: WindowBounds,
}

/// Put the window back where it was left. A frame still on its display
/// stays as it was; one the display no longer holds (its resolution went
/// down) is moved back onto it; one whose display is gone opens centered
/// on the primary display, at its size where that fits.
pub fn restore(saved: Option<&WindowState>, screens: &[Screen]) -> Placement {
    // Numbers that are no frame (a damaged file) are no state at all.
    let saved = saved.filter(|state| {
        [state.x, state.y, state.width, state.height]
            .iter()
            .all(|n| n.is_finite())
    });
    let own = saved.and_then(|state| match &state.display {
        Some(uuid) => screens
            .iter()
            .find(|screen| screen.uuid.as_ref() == Some(uuid)),
        // Kept before the display was: it opened on the primary.
        None => screens.first(),
    });
    let Some(screen) = own.or(screens.first()) else {
        let frame = saved.map_or(Bounds::new(point(px(0.), px(0.)), DEFAULT_SIZE), frame);
        return Placement {
            display: None,
            bounds: WindowBounds::Windowed(frame),
        };
    };
    let frame = match saved {
        Some(state) if own.is_some() => onto(frame(state), screen),
        Some(state) => centered(frame(state).size, screen),
        None => centered(DEFAULT_SIZE, screen),
    };
    let bounds = match saved {
        Some(state) if state.fullscreen => WindowBounds::Fullscreen(frame),
        // GPUI opens it on the frame, then zooms: AppKit keeps the frame
        // to unzoom to.
        Some(state) if state.zoomed => WindowBounds::Maximized(frame),
        _ => WindowBounds::Windowed(frame),
    };
    Placement {
        display: Some(screen.id),
        bounds,
    }
}

/// The window as it is once it holds still.
#[derive(Clone, Debug, PartialEq)]
pub struct Seen {
    pub display: Option<String>,
    pub frame: Bounds<Pixels>,
    pub zoomed: bool,
    pub fullscreen: bool,
}

/// The window as it is now.
pub fn seen(window: &Window, cx: &App) -> Seen {
    Seen {
        display: window
            .display(cx)
            .and_then(|display| display.uuid().ok())
            .map(|uuid| uuid.to_string()),
        frame: window.bounds(),
        // What macOS calls zoomed: filling the part of the display the
        // menu bar and Dock leave.
        zoomed: window.is_maximized(),
        fullscreen: window.is_fullscreen(),
    }
}

/// What to remember of a window seen at rest. Zoomed or full screen, it
/// keeps the frame it had before: the one it goes back to.
pub fn settle(kept: Option<&WindowState>, seen: Seen) -> WindowState {
    match kept {
        Some(kept) if seen.fullscreen => WindowState {
            fullscreen: true,
            ..kept.clone()
        },
        Some(kept) if seen.zoomed => WindowState {
            zoomed: true,
            fullscreen: false,
            ..kept.clone()
        },
        _ => WindowState {
            display: seen.display,
            x: seen.frame.origin.x.into(),
            y: seen.frame.origin.y.into(),
            width: seen.frame.size.width.into(),
            height: seen.frame.size.height.into(),
            zoomed: seen.zoomed,
            fullscreen: seen.fullscreen,
        },
    }
}

/// The kept frame, at no less than the smallest window.
fn frame(state: &WindowState) -> Bounds<Pixels> {
    Bounds::new(
        point(px(state.x), px(state.y)),
        size(px(state.width), px(state.height)).max(&MIN_SIZE),
    )
}

/// A frame on the display it was left on: as it was while all of it is
/// there, else moved, and shrunk if it must be, onto the part the menu bar
/// and Dock leave.
fn onto(frame: Bounds<Pixels>, screen: &Screen) -> Bounds<Pixels> {
    // A point of slack for frames that sit flush with an edge.
    let slack = px(1.);
    let area = &screen.bounds;
    let inside = frame.left() >= area.left() - slack
        && frame.top() >= area.top() - slack
        && frame.right() <= area.right() + slack
        && frame.bottom() <= area.bottom() + slack;
    if inside {
        return frame;
    }
    let visible = &screen.visible;
    let size = frame.size.min(&visible.size);
    let last = point(visible.right() - size.width, visible.bottom() - size.height);
    Bounds::new(frame.origin.clamp(&visible.origin, &last), size)
}

/// A window of `size` in the middle of what the menu bar and Dock leave.
fn centered(size: Size<Pixels>, screen: &Screen) -> Bounds<Pixels> {
    Bounds::centered_at(screen.visible.center(), size.min(&screen.visible.size))
}

#[cfg(test)]
mod tests {
    use gpui::{Entity, TestAppContext};

    use super::*;
    use crate::app::AppModel;
    use crate::shell::Shell;
    use crate::testing::{install, scratch};

    fn kept(model: &Entity<AppModel>, cx: &mut TestAppContext) -> Option<WindowState> {
        model.read_with(cx, |model, _| model.store.window())
    }

    #[gpui::test]
    fn the_window_is_kept_where_it_holds_still(cx: &mut TestAppContext) {
        let dir = scratch("window-settle");
        let model = install(&dir, cx);
        let window = cx.add_window(Shell::new);
        cx.run_until_parked();
        let before = kept(&model, cx);

        // Frames passed through on the way, as an animation's are: none
        // is where the window was left.
        for width in [1100., 1250., 1400.] {
            cx.simulate_window_resize(window.into(), size(px(width), px(900.)));
            cx.executor().advance_clock(SETTLE / 2);
            cx.run_until_parked();
            assert_eq!(kept(&model, cx), before);
        }
        cx.executor().advance_clock(SETTLE);
        cx.run_until_parked();
        let held = kept(&model, cx).expect("kept once still");
        assert_eq!((held.width, held.height), (1400., 900.));
        assert!(!held.zoomed && !held.fullscreen && held.display.is_some());

        // Full screen and back: the frame it goes back to stays.
        for fullscreen in [true, false] {
            window
                .update(cx, |_, window, _| window.toggle_fullscreen())
                .unwrap();
            cx.run_until_parked();
            cx.executor().advance_clock(SETTLE);
            cx.run_until_parked();
            assert_eq!(
                kept(&model, cx),
                Some(WindowState {
                    fullscreen,
                    ..held.clone()
                })
            );
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A 2560×1440 display with a 25-point menu bar and a 70-point Dock.
    fn screen(id: u64, uuid: &str) -> Screen {
        Screen {
            id: DisplayId::new(id),
            uuid: Some(uuid.into()),
            bounds: Bounds::new(point(px(0.), px(0.)), size(px(2560.), px(1440.))),
            visible: Bounds::new(point(px(0.), px(25.)), size(px(2560.), px(1345.))),
        }
    }

    fn state(display: Option<&str>, x: f32, y: f32, width: f32, height: f32) -> WindowState {
        WindowState {
            display: display.map(Into::into),
            x,
            y,
            width,
            height,
            zoomed: false,
            fullscreen: false,
        }
    }

    fn rect(x: f32, y: f32, width: f32, height: f32) -> Bounds<Pixels> {
        Bounds::new(point(px(x), px(y)), size(px(width), px(height)))
    }

    #[test]
    fn a_zoomed_window_reopens_zoomed_over_the_frame_it_unzooms_to() {
        let screens = [screen(1, "main"), screen(2, "side")];
        let zoomed = WindowState {
            zoomed: true,
            ..state(Some("side"), 300., 140., 1400., 900.)
        };
        assert_eq!(
            restore(Some(&zoomed), &screens),
            Placement {
                display: Some(DisplayId::new(2)),
                bounds: WindowBounds::Maximized(rect(300., 140., 1400., 900.)),
            }
        );
        let full = WindowState {
            fullscreen: true,
            ..zoomed
        };
        assert_eq!(
            restore(Some(&full), &screens).bounds,
            WindowBounds::Fullscreen(rect(300., 140., 1400., 900.))
        );
    }

    #[test]
    fn a_window_whose_display_is_gone_opens_centered_on_the_primary() {
        let laptop = [Screen {
            bounds: rect(0., 0., 1512., 982.),
            visible: rect(0., 38., 1512., 944.),
            ..screen(1, "laptop")
        }];
        let zoomed = WindowState {
            zoomed: true,
            ..state(Some("unplugged"), 1800., 300., 1400., 900.)
        };
        assert_eq!(
            restore(Some(&zoomed), &laptop),
            Placement {
                display: Some(DisplayId::new(1)),
                bounds: WindowBounds::Maximized(rect(56., 60., 1400., 900.)),
            }
        );
        // Larger than all of it: as large as it can be.
        let wide = state(Some("unplugged"), 0., 0., 2400., 1300.);
        assert_eq!(
            restore(Some(&wide), &laptop).bounds,
            WindowBounds::Windowed(rect(0., 38., 1512., 944.))
        );
    }

    #[test]
    fn a_frame_its_display_no_longer_holds_moves_back_onto_it() {
        let screens = [screen(1, "main")];
        // Flush against the bottom-right corner, under the Dock: as left.
        let flush = state(Some("main"), 1560., 640., 1000., 800.);
        assert_eq!(
            restore(Some(&flush), &screens).bounds,
            WindowBounds::Windowed(rect(1560., 640., 1000., 800.))
        );
        // Past the right edge since the resolution went down: moved in, at
        // its size.
        let past = state(Some("main"), 2100., 200., 1000., 800.);
        assert_eq!(
            restore(Some(&past), &screens).bounds,
            WindowBounds::Windowed(rect(1560., 200., 1000., 800.))
        );
    }

    #[test]
    fn a_state_from_before_displays_were_kept_means_the_primary() {
        let screens = [screen(1, "main"), screen(2, "side")];
        // The shape earlier builds wrote: no display, no zoom.
        let old: WindowState = serde_json::from_value(serde_json::json!({
            "x": 248.0, "y": 137.0, "width": 1929.0, "height": 1141.0, "fullscreen": false,
        }))
        .unwrap();
        assert_eq!(
            restore(Some(&old), &screens),
            Placement {
                display: Some(DisplayId::new(1)),
                bounds: WindowBounds::Windowed(rect(248., 137., 1929., 1141.)),
            }
        );
        // Nothing kept, or nothing a window could be: the default,
        // centered.
        let first = Placement {
            display: Some(DisplayId::new(1)),
            bounds: WindowBounds::Windowed(rect(640., 287.5, 1280., 820.)),
        };
        assert_eq!(restore(None, &screens), first);
        let damaged = state(None, f32::NAN, 0., 1280., 820.);
        assert_eq!(restore(Some(&damaged), &screens), first);
        // Smaller than the layout holds together at: the smallest window.
        let tiny = state(Some("side"), 40., 60., 300., 200.);
        assert_eq!(
            restore(Some(&tiny), &screens).bounds,
            WindowBounds::Windowed(rect(40., 60., 980., 640.))
        );
    }

    #[test]
    fn zooming_and_full_screen_keep_the_frame_the_window_goes_back_to() {
        let at = |frame, zoomed, fullscreen| Seen {
            display: Some("main".into()),
            frame,
            zoomed,
            fullscreen,
        };
        let windowed = settle(None, at(rect(300., 140., 1400., 900.), false, false));
        assert_eq!(windowed, state(Some("main"), 300., 140., 1400., 900.));

        // Zoomed: the frame it fills now is not the one it goes back to.
        let zoomed = settle(
            Some(&windowed),
            at(rect(0., 25., 2560., 1345.), true, false),
        );
        assert_eq!(
            zoomed,
            WindowState {
                zoomed: true,
                ..windowed.clone()
            }
        );
        // Full screen from zoomed, and back out to zoomed.
        let full = settle(Some(&zoomed), at(rect(0., 0., 2560., 1440.), true, true));
        assert_eq!(
            full,
            WindowState {
                fullscreen: true,
                ..zoomed.clone()
            }
        );
        assert_eq!(
            settle(Some(&full), at(rect(0., 25., 2560., 1345.), true, false)),
            zoomed
        );
        // Unzoomed, then moved to another display.
        let moved = Seen {
            display: Some("side".into()),
            ..at(rect(100., 90., 1400., 900.), false, false)
        };
        assert_eq!(
            settle(Some(&zoomed), moved),
            state(Some("side"), 100., 90., 1400., 900.)
        );
    }
}
