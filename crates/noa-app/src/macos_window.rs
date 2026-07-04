//! macOS NSWindow tweaks for the quick terminal, applied via `objc2` message
//! sends against winit's live `NSWindow` (same handle-extraction pattern as
//! `macos_blur`).
//!
//! winit can't create a true `NSPanel`, so the quick terminal is an
//! `NSWindow` nudged to behave like one: it floats above normal windows,
//! joins every Space (so the hotkey drops it in wherever you are, including
//! over a fullscreen app), and doesn't steal the "main window" role. A no-op
//! on every non-macOS platform.

use winit::window::Window;

/// Make `window` behave like a floating, all-Spaces quick-terminal panel.
/// Called once, right after the quick-terminal window is created.
pub(crate) fn configure_quick_terminal_window(window: &Window) {
    apply(window);
}

#[cfg(target_os = "macos")]
fn apply(window: &Window) {
    use objc2::msg_send;
    use objc2::runtime::AnyObject;
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

    // NSWindowLevel: float above ordinary windows (NSFloatingWindowLevel = 3).
    const NS_FLOATING_WINDOW_LEVEL: isize = 3;
    // NSWindowCollectionBehavior: canJoinAllSpaces (1<<0) | fullScreenAuxiliary
    // (1<<8) — visible on every Space and allowed over a fullscreen window.
    const NS_COLLECTION_BEHAVIOR: usize = (1 << 0) | (1 << 8);

    let Ok(handle) = window.window_handle() else {
        return;
    };
    let RawWindowHandle::AppKit(appkit) = handle.as_raw() else {
        return;
    };
    let ns_view = appkit.ns_view.as_ptr().cast::<AnyObject>();

    // SAFETY: `ns_view` is winit's live AppKit `NSView` for this window and we
    // are on the main (window-owning) thread. `-window` returns the owning
    // `NSWindow` (nil before the view is installed); the setters are plain
    // AppKit property writes.
    unsafe {
        let ns_window: *mut AnyObject = msg_send![ns_view, window];
        if ns_window.is_null() {
            return;
        }
        let _: () = msg_send![ns_window, setLevel: NS_FLOATING_WINDOW_LEVEL];
        let _: () = msg_send![ns_window, setCollectionBehavior: NS_COLLECTION_BEHAVIOR];
        // Don't let the drop-down become the app's "main" window or persist in
        // the window cycle; it is a transient accessory.
        let _: () = msg_send![ns_window, setHidesOnDeactivate: false];
    }
}

#[cfg(not(target_os = "macos"))]
fn apply(_window: &Window) {}
