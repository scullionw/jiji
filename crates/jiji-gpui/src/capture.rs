//! What scripted runs need from AppKit that GPUI does not expose.
//!
//! Input: hand this process's own view an `NSEvent` built here, so a
//! scripted click takes exactly the path a real one does (AppKit event →
//! GPUI's conversion → hit-testing → listeners). Nothing enters the system
//! event stream: the pointer does not move, no other app sees anything,
//! and focus stays where the user left it.
//!
//! Self-capture: write this process's own window to a PNG. An app may
//! always read its own windows, so this works where `screencapture` is
//! refused for lack of Screen Recording permission — which is what lets an
//! agent look at the GPUI window at all, now that there is no webview to
//! load in a headless browser.
//!
//! Staying visible: GPUI stops its display link while a window is fully
//! covered, so a window opened behind the user's work never paints a
//! second frame. A background run floats its window instead of taking
//! focus.

use std::ffi::{c_char, c_void, CString};
use std::path::Path;

use gpui::{Modifiers, MouseButton, Window};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};

#[repr(C)]
#[derive(Clone, Copy)]
struct CGRect {
    origin: [f64; 2],
    size: [f64; 2],
}

type Ref = *const c_void;

#[repr(C)]
#[derive(Clone, Copy)]
struct CGPoint {
    x: f64,
    y: f64,
}

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    static CGRectNull: CGRect;
    fn CGWindowListCreateImage(bounds: CGRect, list: u32, window: u32, image: u32) -> Ref;
}

#[link(name = "ImageIO", kind = "framework")]
extern "C" {
    fn CGImageDestinationCreateWithURL(url: Ref, kind: Ref, count: usize, options: Ref) -> Ref;
    fn CGImageDestinationAddImage(destination: Ref, image: Ref, properties: Ref);
    fn CGImageDestinationFinalize(destination: Ref) -> bool;
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFRelease(object: Ref);
    fn CFStringCreateWithCString(allocator: Ref, text: *const c_char, encoding: u32) -> Ref;
    fn CFURLCreateFromFileSystemRepresentation(
        allocator: Ref,
        path: *const u8,
        len: isize,
        is_directory: bool,
    ) -> Ref;
}

#[link(name = "objc")]
extern "C" {
    fn sel_registerName(name: *const c_char) -> Ref;
    fn objc_getClass(name: *const c_char) -> Ref;
    fn objc_msgSend();
}

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGEventCreateScrollWheelEvent2(
        source: Ref,
        units: u32,
        wheel_count: u32,
        wheel1: i32,
        wheel2: i32,
        wheel3: i32,
    ) -> Ref;
    fn CGEventCreateMouseEvent(source: Ref, kind: u32, position: CGPoint, button: u32) -> Ref;
    fn CGEventSetFlags(event: Ref, flags: u64);
    fn CGEventSetLocation(event: Ref, location: CGPoint);
    fn CGEventGetLocation(event: Ref) -> CGPoint;
    fn CGEventSetIntegerValueField(event: Ref, field: u32, value: i64);
}

const UTF8: u32 = 0x0800_0100;
/// kCGWindowListOptionIncludingWindow: just the one window.
const INCLUDING_WINDOW: u32 = 1 << 3;
/// kCGWindowImageBoundsIgnoreFraming | kCGWindowImageBestResolution.
const IMAGE_OPTIONS: u32 = (1 << 0) | (1 << 3);

/// The NSView backing a GPUI window.
fn ns_view(window: &Window) -> Result<Ref, String> {
    let handle = HasWindowHandle::window_handle(window).map_err(|err| err.to_string())?;
    let RawWindowHandle::AppKit(handle) = handle.as_raw() else {
        return Err("not an AppKit window".into());
    };
    Ok(handle.ns_view.as_ptr())
}

/// The NSWindow behind a GPUI window.
fn ns_window(window: &Window) -> Result<Ref, String> {
    let view = ns_view(window)?;
    // SAFETY: a plain message send on the live NSView backing this window.
    let ns_window = unsafe {
        let send: unsafe extern "C" fn(Ref, Ref) -> Ref =
            std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
        send(view, sel_registerName(c"window".as_ptr()))
    };
    if ns_window.is_null() {
        return Err("the view has no window".into());
    }
    Ok(ns_window)
}

/// Put the window above other apps' windows without activating this app:
/// it keeps painting, and the user's keyboard focus stays where it was.
pub fn float_without_focus(window: &Window) {
    let Ok(ns_window) = ns_window(window) else {
        return;
    };
    /// NSFloatingWindowLevel.
    const FLOATING: isize = 3;
    // SAFETY: message sends with their documented signatures.
    unsafe {
        let set_level: unsafe extern "C" fn(Ref, Ref, isize) =
            std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
        set_level(ns_window, sel_registerName(c"setLevel:".as_ptr()), FLOATING);
        let send: unsafe extern "C" fn(Ref, Ref) =
            std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
        send(
            ns_window,
            sel_registerName(c"orderFrontRegardless".as_ptr()),
        );
    }
}

pub fn window_png(window: &Window, path: &Path) -> Result<(), String> {
    let ns_window = ns_window(window)?;
    let path = CString::new(path.to_string_lossy().as_bytes()).map_err(|err| err.to_string())?;
    // SAFETY: a message send with its documented signature, and
    // Create-rule Core Foundation objects released below.
    unsafe {
        let send_int: unsafe extern "C" fn(Ref, Ref) -> isize =
            std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
        let number = send_int(ns_window, sel_registerName(c"windowNumber".as_ptr()));

        let image =
            CGWindowListCreateImage(CGRectNull, INCLUDING_WINDOW, number as u32, IMAGE_OPTIONS);
        if image.is_null() {
            return Err("the window server returned no image".into());
        }
        let url = CFURLCreateFromFileSystemRepresentation(
            std::ptr::null(),
            path.as_ptr().cast(),
            path.as_bytes().len() as isize,
            false,
        );
        let png = CFStringCreateWithCString(std::ptr::null(), c"public.png".as_ptr(), UTF8);
        let destination = CGImageDestinationCreateWithURL(url, png, 1, std::ptr::null());
        let written = !destination.is_null() && {
            CGImageDestinationAddImage(destination, image, std::ptr::null());
            CGImageDestinationFinalize(destination)
        };
        for object in [destination, png, url, image] {
            if !object.is_null() {
                CFRelease(object);
            }
        }
        written
            .then_some(())
            .ok_or_else(|| "could not write the PNG".to_owned())
    }
}

/// One pointer event of a scripted gesture.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pointer {
    Move,
    Down,
    Up,
    /// A move with the button held.
    Drag,
}

fn modifier_flags(modifiers: Modifiers) -> usize {
    let mut flags = 0;
    if modifiers.shift {
        flags |= 1 << 17;
    }
    if modifiers.control {
        flags |= 1 << 18;
    }
    if modifiers.alt {
        flags |= 1 << 19;
    }
    if modifiers.platform {
        flags |= 1 << 20;
    }
    flags
}

unsafe fn uptime() -> f64 {
    let class_send: unsafe extern "C" fn(Ref, Ref) -> Ref =
        std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
    let info = class_send(
        objc_getClass(c"NSProcessInfo".as_ptr()),
        sel_registerName(c"processInfo".as_ptr()),
    );
    let send: unsafe extern "C" fn(Ref, Ref) -> f64 =
        std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
    send(info, sel_registerName(c"systemUptime".as_ptr()))
}

unsafe fn window_number(ns_window: Ref) -> isize {
    let send: unsafe extern "C" fn(Ref, Ref) -> isize =
        std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
    send(ns_window, sel_registerName(c"windowNumber".as_ptr()))
}

/// What delivering an event needs from a window, read while GPUI lends
/// it out. Events themselves must be delivered *outside* any GPUI update:
/// the view's handler re-enters the app.
#[derive(Clone, Copy)]
pub struct NativeTarget {
    view: Ref,
    window: Ref,
    height: f32,
    origin: (f32, f32),
}

impl NativeTarget {
    pub fn of(window: &Window) -> Result<Self, String> {
        let origin = window.bounds().origin;
        Ok(Self {
            view: ns_view(window)?,
            window: ns_window(window)?,
            height: window.viewport_size().height.into(),
            origin: (origin.x.into(), origin.y.into()),
        })
    }

    /// The NSEvent for a Core Graphics event, placed at window point
    /// `(x, y)`. An event that was never posted belongs to no window, so
    /// AppKit reports its location in screen coordinates; wherever the
    /// first wrapping lands, the event is shifted by the difference and
    /// wrapped again.
    unsafe fn wrap_at(&self, cg_event: Ref, (x, y): (f32, f32)) -> Ref {
        let wrap: unsafe extern "C" fn(Ref, Ref, Ref) -> Ref =
            std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
        let location: unsafe extern "C" fn(Ref, Ref) -> CGPoint =
            std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
        let class = objc_getClass(c"NSEvent".as_ptr());
        let from_cg = sel_registerName(c"eventWithCGEvent:".as_ptr());
        let event = wrap(class, from_cg, cg_event);
        if event.is_null() {
            return event;
        }
        let at = location(event, sel_registerName(c"locationInWindow".as_ptr()));
        // Window coordinates run bottom-up, Core Graphics' top-down.
        let want = (x as f64, (self.height - y) as f64);
        let placed = CGEventGetLocation(cg_event);
        CGEventSetLocation(
            cg_event,
            CGPoint {
                x: placed.x + (want.0 - at.x),
                y: placed.y - (want.1 - at.y),
            },
        );
        wrap(class, from_cg, cg_event)
    }

    /// Deliver one mouse event at window point `(x, y)` (top-left origin)
    /// to this window's own view.
    pub fn send_mouse(
        &self,
        kind: Pointer,
        button: MouseButton,
        (x, y): (f32, f32),
        clicks: usize,
        modifiers: Modifiers,
    ) -> Result<(), String> {
        let (event_type, selector): (usize, &std::ffi::CStr) = match (kind, button) {
            (Pointer::Move, _) => (5, c"mouseMoved:"),
            (Pointer::Down, MouseButton::Left) => (1, c"mouseDown:"),
            (Pointer::Up, MouseButton::Left) => (2, c"mouseUp:"),
            (Pointer::Drag, MouseButton::Left) => (6, c"mouseDragged:"),
            (Pointer::Down, MouseButton::Right) => (3, c"rightMouseDown:"),
            (Pointer::Up, MouseButton::Right) => (4, c"rightMouseUp:"),
            (Pointer::Drag, MouseButton::Right) => (7, c"rightMouseDragged:"),
            (Pointer::Down, _) => (25, c"otherMouseDown:"),
            (Pointer::Up, _) => (26, c"otherMouseUp:"),
            (Pointer::Drag, _) => (27, c"otherMouseDragged:"),
        };
        // AppKit numbers every event it builds from scratch as button 0,
        // and GPUI reads the button from that number: the other buttons
        // are built as Core Graphics events, which carry theirs.
        if button != MouseButton::Left && kind != Pointer::Move {
            let number = if button == MouseButton::Right { 1 } else { 2 };
            // SAFETY: a Create-rule CGEvent released below, never posted;
            // message sends with their documented signatures.
            unsafe {
                let cg_event = CGEventCreateMouseEvent(
                    std::ptr::null(),
                    event_type as u32,
                    CGPoint {
                        x: (self.origin.0 + x) as f64,
                        y: (self.origin.1 + y) as f64,
                    },
                    number,
                );
                if cg_event.is_null() {
                    return Err("CoreGraphics built no mouse event".into());
                }
                /// kCGMouseEventClickState.
                const CLICK_STATE: u32 = 1;
                CGEventSetIntegerValueField(cg_event, CLICK_STATE, clicks as i64);
                CGEventSetFlags(cg_event, modifier_flags(modifiers) as u64);
                let window = window_number(self.window) as i64;
                CGEventSetIntegerValueField(cg_event, 91, window);
                CGEventSetIntegerValueField(cg_event, 92, window);
                let event = self.wrap_at(cg_event, (x, y));
                CFRelease(cg_event);
                if event.is_null() {
                    return Err("AppKit built no mouse event".into());
                }
                let deliver: unsafe extern "C" fn(Ref, Ref, Ref) =
                    std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
                deliver(self.view, sel_registerName(selector.as_ptr()), event);
            }
            return Ok(());
        }
        // SAFETY: message sends with their documented signatures; the
        // event is autoreleased and only used for the duration of the call.
        unsafe {
            type MakeEvent = unsafe extern "C" fn(
                Ref,
                Ref,
                usize,
                CGPoint,
                usize,
                f64,
                isize,
                Ref,
                isize,
                isize,
                f32,
            ) -> Ref;
            let make: MakeEvent = std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
            let event = make(
                objc_getClass(c"NSEvent".as_ptr()),
                sel_registerName(
                    c"mouseEventWithType:location:modifierFlags:timestamp:windowNumber:context:eventNumber:clickCount:pressure:"
                        .as_ptr(),
                ),
                event_type,
                CGPoint {
                    x: x as f64,
                    y: (self.height - y) as f64,
                },
                modifier_flags(modifiers),
                uptime(),
                window_number(self.window),
                std::ptr::null(),
                0,
                clicks as isize,
                if kind == Pointer::Up { 0. } else { 1. },
            );
            if event.is_null() {
                return Err("AppKit built no event".into());
            }
            let deliver: unsafe extern "C" fn(Ref, Ref, Ref) =
                std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
            deliver(self.view, sel_registerName(selector.as_ptr()), event);
        }
        Ok(())
    }

    /// Deliver one precise (trackpad-style) scroll of `(dx, dy)` points at
    /// window point `(x, y)` to this window's own view.
    pub fn send_scroll(&self, (x, y): (f32, f32), (dx, dy): (f32, f32)) -> Result<(), String> {
        // SAFETY: a Create-rule CGEvent released below, never posted;
        // message sends with their documented signatures.
        unsafe {
            /// kCGScrollEventUnitPixel.
            const PIXELS: u32 = 0;
            let cg_event = CGEventCreateScrollWheelEvent2(
                std::ptr::null(),
                PIXELS,
                2,
                dy.round() as i32,
                dx.round() as i32,
                0,
            );
            if cg_event.is_null() {
                return Err("CoreGraphics built no scroll event".into());
            }
            CGEventSetLocation(
                cg_event,
                CGPoint {
                    x: (self.origin.0 + x) as f64,
                    y: (self.origin.1 + y) as f64,
                },
            );
            let number = window_number(self.window) as i64;
            // kCGMouseEventWindowUnderMousePointer and its
            // ...ThatCanHandleThisEvent twin: which window the event is for.
            CGEventSetIntegerValueField(cg_event, 91, number);
            CGEventSetIntegerValueField(cg_event, 92, number);
            let event = self.wrap_at(cg_event, (x, y));
            CFRelease(cg_event);
            if event.is_null() {
                return Err("AppKit built no scroll event".into());
            }
            let deliver: unsafe extern "C" fn(Ref, Ref, Ref) =
                std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
            deliver(self.view, sel_registerName(c"scrollWheel:".as_ptr()), event);
        }
        Ok(())
    }
}
