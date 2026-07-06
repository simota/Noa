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

/// The height (physical px) of the window chrome overlapping the top of the
/// content view — titlebar plus native tab bar when the content view is
/// full-size (`transparent` style), 0 when the chrome sits *above* the
/// content (`native`, where `inner_size` already excludes it) or is absent
/// (`hidden`). Queried live from `NSWindow.contentLayoutRect` so the tab bar
/// appearing/disappearing is picked up on the next relayout. `None` when the
/// AppKit window can't be reached (caller falls back to a constant).
pub(crate) fn top_chrome_inset_px(window: &Window) -> Option<u32> {
    top_chrome_inset_px_impl(window)
}

#[cfg(target_os = "macos")]
fn top_chrome_inset_px_impl(window: &Window) -> Option<u32> {
    use objc2::msg_send;
    use objc2::runtime::AnyObject;
    use objc2_foundation::NSRect;
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

    let handle = window.window_handle().ok()?;
    let RawWindowHandle::AppKit(appkit) = handle.as_raw() else {
        return None;
    };
    let ns_view = appkit.ns_view.as_ptr().cast::<AnyObject>();

    // SAFETY: `ns_view` is winit's live AppKit `NSView` for this window and we
    // are on the main (window-owning) thread. `-window`/`-contentView` return
    // nil-checkable object pointers; `frame`/`contentLayoutRect` are plain
    // struct-returning property reads.
    let inset_pt = unsafe {
        let ns_window: *mut AnyObject = msg_send![ns_view, window];
        if ns_window.is_null() {
            return None;
        }
        let content_view: *mut AnyObject = msg_send![ns_window, contentView];
        if content_view.is_null() {
            return None;
        }
        // Both rects share the window coordinate space (origin bottom-left):
        // the chrome inset is the gap between the content view's top edge and
        // the layout rect's top edge.
        let view_frame: NSRect = msg_send![content_view, frame];
        let layout: NSRect = msg_send![ns_window, contentLayoutRect];
        (view_frame.size.height - (layout.origin.y + layout.size.height)).max(0.0)
    };
    Some((inset_pt * window.scale_factor()).round() as u32)
}

#[cfg(not(target_os = "macos"))]
fn top_chrome_inset_px_impl(_window: &Window) -> Option<u32> {
    None
}
