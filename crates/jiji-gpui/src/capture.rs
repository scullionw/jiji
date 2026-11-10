//! What scripted runs need from AppKit that GPUI does not expose.
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

use gpui::Window;
use raw_window_handle::{HasWindowHandle, RawWindowHandle};

#[repr(C)]
#[derive(Clone, Copy)]
struct CGRect {
    origin: [f64; 2],
    size: [f64; 2],
}

type Ref = *const c_void;

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
    fn objc_msgSend();
}

const UTF8: u32 = 0x0800_0100;
/// kCGWindowListOptionIncludingWindow: just the one window.
const INCLUDING_WINDOW: u32 = 1 << 3;
/// kCGWindowImageBoundsIgnoreFraming | kCGWindowImageBestResolution.
const IMAGE_OPTIONS: u32 = (1 << 0) | (1 << 3);

/// The NSWindow behind a GPUI window.
fn ns_window(window: &Window) -> Result<Ref, String> {
    let handle = HasWindowHandle::window_handle(window).map_err(|err| err.to_string())?;
    let RawWindowHandle::AppKit(handle) = handle.as_raw() else {
        return Err("not an AppKit window".into());
    };
    // SAFETY: a plain message send on the live NSView backing this window.
    let ns_window = unsafe {
        let send: unsafe extern "C" fn(Ref, Ref) -> Ref =
            std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
        send(
            handle.ns_view.as_ptr(),
            sel_registerName(c"window".as_ptr()),
        )
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
