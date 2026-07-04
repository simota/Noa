//! Native macOS window background blur via the private CoreGraphics Services
//! (CGS) API — the same mechanism Ghostty and Alacritty use. Blur only reads
//! as blur when the window is also translucent (`background-opacity` < 1.0);
//! it is applied regardless, with a hint logged otherwise. A no-op on every
//! non-macOS platform.

use winit::window::Window;

/// Apply `background-blur-radius` to `window`'s background. A startup-time
/// action: called once per terminal window right after creation. `opacity` is
/// only used to warn when blur was requested on a fully opaque window (where
/// it has no visible effect).
pub(crate) fn apply_background_blur(window: &Window, radius: u16, opacity: f32) {
    if radius == 0 {
        return;
    }
    if opacity >= 1.0 {
        log::debug!(
            "background-blur-radius = {radius} has no visible effect while background-opacity is 1.0"
        );
    }
    set_window_blur(window, radius);
}

#[cfg(target_os = "macos")]
fn set_window_blur(window: &Window, radius: u16) {
    use objc2::msg_send;
    use objc2::runtime::AnyObject;
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

    let Ok(handle) = window.window_handle() else {
        return;
    };
    let RawWindowHandle::AppKit(appkit) = handle.as_raw() else {
        return;
    };
    let ns_view = appkit.ns_view.as_ptr().cast::<AnyObject>();

    // SAFETY: `ns_view` is winit's live AppKit `NSView` for this window and we
    // are on the main (window-owning) thread. `-window` returns the owning
    // `NSWindow` (or nil before the view is installed) and `-windowNumber` its
    // global window id, both plain reads. The CGS calls take that id; they are
    // private but stable and used by other terminals for exactly this.
    unsafe {
        let ns_window: *mut AnyObject = msg_send![ns_view, window];
        if ns_window.is_null() {
            return;
        }
        let window_number: isize = msg_send![ns_window, windowNumber];
        let connection = CGSMainConnectionID();
        let status =
            CGSSetWindowBackgroundBlurRadius(connection, window_number as i32, i32::from(radius));
        if status != 0 {
            log::warn!("CGSSetWindowBackgroundBlurRadius failed (status {status})");
        }
    }
}

#[cfg(not(target_os = "macos"))]
fn set_window_blur(_window: &Window, _radius: u16) {}

#[cfg(target_os = "macos")]
#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGSMainConnectionID() -> i32;
    fn CGSSetWindowBackgroundBlurRadius(connection: i32, window: i32, radius: i32) -> i32;
}
