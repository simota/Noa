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
/// content (`native`, where `inner_size` already excludes it).
/// Queried live from `NSWindow.contentLayoutRect` so the tab bar
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

/// Marker `NSView.identifier` for our opaque titlebar backdrop, so a repeat
/// call finds and refreshes the existing view instead of stacking a second.
#[cfg(target_os = "macos")]
const TITLEBAR_BACKDROP_ID: &str = "noa.titlebar.opaque-backdrop";

/// Install (or refresh) an opaque, `bg`-colored view filling the native
/// titlebar + tab-bar strip.
///
/// Meaningful for translucent normal windows (`background-opacity < 1.0`) with
/// visible AppKit titlebar/tab chrome: AppKit composites its tab chrome — the
/// lazily-allocated hover highlight `NSVisualEffectView` especially — against
/// undefined semi-transparent underlay pixels, which surfaces as magenta
/// diagonal-stripe garbage on some machines. Backing the strip with an opaque
/// layer (the iTerm2/Ghostty approach) gives that chrome defined content to
/// composite over. No-op off macOS or when the AppKit hierarchy can't be
/// reached.
///
/// Idempotent via [`TITLEBAR_BACKDROP_ID`]; a repeat call (e.g. a theme
/// reload) updates the existing view's color rather than adding another.
pub(crate) fn install_titlebar_backdrop(window: &Window, bg: noa_core::Rgb) {
    install_titlebar_backdrop_impl(window, bg);
}

#[cfg(target_os = "macos")]
fn install_titlebar_backdrop_impl(window: &Window, bg: noa_core::Rgb) {
    use objc2::msg_send;
    use objc2::runtime::{AnyClass, AnyObject};
    use objc2_foundation::{NSRect, NSString};
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

    // NSWindowOrderingMode::Below — order the backdrop behind its siblings so
    // the tab bar and title controls keep drawing on top of it.
    const NS_WINDOW_BELOW: isize = -1;
    // NSAutoresizingMaskOptions: width- + height-sizable, so the backdrop
    // tracks the strip as the tab bar appears/disappears.
    const NS_VIEW_WIDTH_SIZABLE: usize = 1 << 1;
    const NS_VIEW_HEIGHT_SIZABLE: usize = 1 << 4;
    // The titlebar container's Objective-C class name (private but stable).
    const CONTAINER_CLASS: &[u8] = b"NSTitlebarContainerView";

    let Ok(handle) = window.window_handle() else {
        return;
    };
    let RawWindowHandle::AppKit(appkit) = handle.as_raw() else {
        return;
    };
    let ns_view = appkit.ns_view.as_ptr().cast::<AnyObject>();
    let identifier = NSString::from_str(TITLEBAR_BACKDROP_ID);

    // SAFETY: `ns_view` is winit's live AppKit `NSView` for this window and we
    // are on the main (window-owning) thread. Every selector below is
    // documented AppKit API on the object it is sent to; each object pointer
    // is nil-checked before use. `NSView`/`NSColor` are looked up at runtime
    // (matching `app_actions.rs`) so no extra objc2-app-kit feature is needed.
    unsafe {
        let ns_window: *mut AnyObject = msg_send![ns_view, window];
        if ns_window.is_null() {
            return;
        }
        let content_view: *mut AnyObject = msg_send![ns_window, contentView];
        if content_view.is_null() {
            return;
        }
        // contentView.superview is the NSThemeFrame; the titlebar container is
        // one of its direct subviews.
        let theme_frame: *mut AnyObject = msg_send![content_view, superview];
        if theme_frame.is_null() {
            return;
        }
        let mut queue = vec![theme_frame];
        let mut container: *mut AnyObject = std::ptr::null_mut();
        while let Some(current_view) = queue.pop() {
            if current_view.is_null() {
                continue;
            }
            let class: *const AnyClass = msg_send![current_view, class];
            if let Some(class) = class.as_ref()
                && class
                    .name()
                    .to_bytes()
                    .windows(CONTAINER_CLASS.len())
                    .any(|w| w == CONTAINER_CLASS)
            {
                container = current_view;
                break;
            }
            let subviews: *mut AnyObject = msg_send![current_view, subviews];
            if !subviews.is_null() {
                let count: usize = msg_send![subviews, count];
                for i in 0..count {
                    let subview: *mut AnyObject = msg_send![subviews, objectAtIndex: i];
                    if !subview.is_null() {
                        queue.push(subview);
                    }
                }
            }
        }
        if container.is_null() {
            return;
        }

        // The opaque theme background as a CGColor for the layer.
        let Some(color_class) = AnyClass::get(c"NSColor") else {
            return;
        };
        let color: *mut AnyObject = msg_send![
            color_class,
            colorWithSRGBRed: bg.r as f64 / 255.0,
            green: bg.g as f64 / 255.0,
            blue: bg.b as f64 / 255.0,
            alpha: 1.0_f64,
        ];
        if color.is_null() {
            return;
        }
        let cg_color: *mut AnyObject = msg_send![color, CGColor];

        // Idempotency: reuse an existing backdrop, just refreshing its color.
        let container_subviews: *mut AnyObject = msg_send![container, subviews];
        if !container_subviews.is_null() {
            let n: usize = msg_send![container_subviews, count];
            for i in 0..n {
                let view: *mut AnyObject = msg_send![container_subviews, objectAtIndex: i];
                if view.is_null() {
                    continue;
                }
                let ident: *mut AnyObject = msg_send![view, identifier];
                if !ident.is_null() {
                    let same: bool = msg_send![ident, isEqualToString: &*identifier];
                    if same {
                        let layer: *mut AnyObject = msg_send![view, layer];
                        if !layer.is_null() {
                            let _: () = msg_send![layer, setBackgroundColor: cg_color];
                        }
                        return;
                    }
                }
            }
        }

        // Create the opaque, layer-backed backdrop sized to the container.
        let Some(view_class) = AnyClass::get(c"NSView") else {
            return;
        };
        let bounds: NSRect = msg_send![container, bounds];
        let alloc: *mut AnyObject = msg_send![view_class, alloc];
        let view: *mut AnyObject = msg_send![alloc, initWithFrame: bounds];
        if view.is_null() {
            return;
        }
        let _: () = msg_send![view, setIdentifier: &*identifier];
        let _: () = msg_send![view, setWantsLayer: true];
        let _: () =
            msg_send![view, setAutoresizingMask: NS_VIEW_WIDTH_SIZABLE | NS_VIEW_HEIGHT_SIZABLE];
        let layer: *mut AnyObject = msg_send![view, layer];
        if !layer.is_null() {
            let _: () = msg_send![layer, setBackgroundColor: cg_color];
            let _: () = msg_send![layer, setOpaque: true];
        }
        // Positioned below all existing subviews so tab-bar chrome stays on top.
        let _: () = msg_send![
            container,
            addSubview: view,
            positioned: NS_WINDOW_BELOW,
            relativeTo: std::ptr::null_mut::<AnyObject>(),
        ];
    }
}

#[cfg(not(target_os = "macos"))]
fn install_titlebar_backdrop_impl(_window: &Window, _bg: noa_core::Rgb) {}

pub(crate) fn set_window_background_color(window: &Window, bg: noa_core::Rgb, alpha: f32) {
    set_window_background_color_impl(window, bg, alpha);
}

#[cfg(target_os = "macos")]
fn set_window_background_color_impl(window: &Window, bg: noa_core::Rgb, alpha: f32) {
    use objc2::msg_send;
    use objc2::runtime::AnyClass;
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

    let Ok(handle) = window.window_handle() else {
        return;
    };
    let RawWindowHandle::AppKit(appkit) = handle.as_raw() else {
        return;
    };
    let ns_view = appkit.ns_view.as_ptr().cast::<objc2::runtime::AnyObject>();

    unsafe {
        let ns_window: *mut objc2::runtime::AnyObject = msg_send![ns_view, window];
        if ns_window.is_null() {
            return;
        }

        let Some(color_class) = AnyClass::get(c"NSColor") else {
            return;
        };
        let color: *mut objc2::runtime::AnyObject = msg_send![
            color_class,
            colorWithSRGBRed: bg.r as f64 / 255.0,
            green: bg.g as f64 / 255.0,
            blue: bg.b as f64 / 255.0,
            alpha: alpha as f64,
        ];
        if color.is_null() {
            return;
        }

        let _: () = msg_send![ns_window, setBackgroundColor: color];
    }
}

#[cfg(not(target_os = "macos"))]
fn set_window_background_color_impl(_window: &Window, _bg: noa_core::Rgb, _alpha: f32) {}
