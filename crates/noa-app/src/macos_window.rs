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

/// Bring the quick-terminal window to the front and make it key. `Window::focus_window`
/// alone can be too weak when the global hotkey fires while Noa is not active.
pub(crate) fn show_quick_terminal_window(window: &Window) {
    show_quick_terminal_window_impl(window);
}

/// Toggle AppKit's native fullscreen Space for a normal terminal window.
/// Returns `false` only when the live NSWindow cannot be reached.
pub(crate) fn toggle_native_fullscreen(window: &Window) -> bool {
    toggle_native_fullscreen_impl(window)
}

/// Move `new_window` immediately after `anchor_window` in their native AppKit
/// tab group. Returns `true` only when the resulting native order is verified.
pub(crate) fn insert_tab_after(anchor_window: &Window, new_window: &Window) -> bool {
    insert_tab_after_impl(anchor_window, new_window)
}

#[cfg(target_os = "macos")]
fn insert_tab_after_impl(anchor_window: &Window, new_window: &Window) -> bool {
    use objc2::msg_send;
    use objc2::runtime::AnyObject;
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

    let Ok(anchor_handle) = anchor_window.window_handle() else {
        return false;
    };
    let RawWindowHandle::AppKit(anchor_appkit) = anchor_handle.as_raw() else {
        return false;
    };
    let Ok(new_handle) = new_window.window_handle() else {
        return false;
    };
    let RawWindowHandle::AppKit(new_appkit) = new_handle.as_raw() else {
        return false;
    };
    let anchor_view = anchor_appkit.ns_view.as_ptr().cast::<AnyObject>();
    let new_view = new_appkit.ns_view.as_ptr().cast::<AnyObject>();

    // SAFETY: tab creation runs on winit's main (window-owning) thread. Both
    // NSViews come from live winit window handles, every returned object is
    // nil-checked, and the insertion index is derived from the tab group's
    // current `windows` array before `insertWindow:atIndex:` is sent.
    unsafe {
        let anchor: *mut AnyObject = msg_send![anchor_view, window];
        let new: *mut AnyObject = msg_send![new_view, window];
        if anchor.is_null() || new.is_null() {
            return false;
        }
        let tab_group: *mut AnyObject = msg_send![anchor, tabGroup];
        if tab_group.is_null() {
            return false;
        }
        let windows: *mut AnyObject = msg_send![tab_group, windows];
        if windows.is_null() {
            return false;
        }
        let count: usize = msg_send![windows, count];
        let mut anchor_index = None;
        let mut new_index = None;
        for index in 0..count {
            let candidate: *mut AnyObject = msg_send![windows, objectAtIndex: index];
            if candidate == anchor {
                anchor_index = Some(index);
            }
            if candidate == new {
                new_index = Some(index);
            }
        }
        let Some(anchor_index) = anchor_index else {
            return false;
        };
        let target_index = anchor_index + 1;
        if new_index == Some(target_index) {
            return true;
        }
        let Ok(target_index) = isize::try_from(target_index) else {
            return false;
        };
        let _: () = msg_send![tab_group, insertWindow: new, atIndex: target_index];

        let windows: *mut AnyObject = msg_send![tab_group, windows];
        if !windows.is_null() {
            let count: usize = msg_send![windows, count];
            for index in 0..count.saturating_sub(1) {
                let candidate: *mut AnyObject = msg_send![windows, objectAtIndex: index];
                let next: *mut AnyObject = msg_send![windows, objectAtIndex: index + 1];
                if candidate == anchor && next == new {
                    return true;
                }
            }
        }

        // Preserve the established fallback contract: AppKit's default is to
        // append a newly created tab. If the verified move did not take,
        // remove any partial group placement before adding the window back at
        // the native end; the caller appends it to Noa's `window_order` too.
        let windows: *mut AnyObject = msg_send![tab_group, windows];
        if !windows.is_null() {
            let count: usize = msg_send![windows, count];
            for index in 0..count {
                let candidate: *mut AnyObject = msg_send![windows, objectAtIndex: index];
                if candidate == new {
                    let _: () = msg_send![tab_group, removeWindow: new];
                    break;
                }
            }
        }
        let _: () = msg_send![tab_group, addWindow: new];
        false
    }
}

#[cfg(not(target_os = "macos"))]
fn insert_tab_after_impl(_anchor_window: &Window, _new_window: &Window) -> bool {
    false
}

/// Resolves `quick-terminal-screen`'s `mode` to the `CGDirectDisplayID` of the
/// target `NSScreen`, re-resolved fresh on every quick-terminal reveal (never
/// cached) so the hotkey always targets whatever the mode currently points
/// at. `None` when AppKit can't resolve a screen for `mode` (e.g. `mouse`
/// with no screen under the pointer) or off macOS — the caller falls back to
/// its existing anchor-window monitor.
pub(crate) fn quick_terminal_target_display(mode: noa_config::QuickTerminalScreen) -> Option<u32> {
    quick_terminal_target_display_impl(mode)
}

#[cfg(target_os = "macos")]
fn quick_terminal_target_display_impl(mode: noa_config::QuickTerminalScreen) -> Option<u32> {
    use objc2::msg_send;
    use objc2::runtime::{AnyClass, AnyObject};
    use objc2_foundation::{NSPoint, NSRect, NSString};

    let screen_class = AnyClass::get(c"NSScreen")?;

    // SAFETY: `NSScreen`/`NSEvent` are live AppKit singletons queried on the
    // main (window-owning) thread; every object pointer is nil-checked
    // before use.
    unsafe {
        let screen: *mut AnyObject = match mode {
            noa_config::QuickTerminalScreen::Main => msg_send![screen_class, mainScreen],
            noa_config::QuickTerminalScreen::MacosMenuBar => {
                let screens: *mut AnyObject = msg_send![screen_class, screens];
                if screens.is_null() {
                    std::ptr::null_mut()
                } else {
                    let count: usize = msg_send![screens, count];
                    if count == 0 {
                        std::ptr::null_mut()
                    } else {
                        msg_send![screens, objectAtIndex: 0_usize]
                    }
                }
            }
            noa_config::QuickTerminalScreen::Mouse => {
                let event_class = AnyClass::get(c"NSEvent")?;
                let point: NSPoint = msg_send![event_class, mouseLocation];
                let screens: *mut AnyObject = msg_send![screen_class, screens];
                let mut found: *mut AnyObject = std::ptr::null_mut();
                if !screens.is_null() {
                    let count: usize = msg_send![screens, count];
                    for i in 0..count {
                        let candidate: *mut AnyObject = msg_send![screens, objectAtIndex: i];
                        if candidate.is_null() {
                            continue;
                        }
                        // `frame` is in AppKit's global bottom-left-origin
                        // coordinate space, same as `mouseLocation`.
                        let frame: NSRect = msg_send![candidate, frame];
                        let contains = point.x >= frame.origin.x
                            && point.x < frame.origin.x + frame.size.width
                            && point.y >= frame.origin.y
                            && point.y < frame.origin.y + frame.size.height;
                        if contains {
                            found = candidate;
                            break;
                        }
                    }
                }
                found
            }
        };
        if screen.is_null() {
            return None;
        }

        let device_description: *mut AnyObject = msg_send![screen, deviceDescription];
        if device_description.is_null() {
            return None;
        }
        let key = NSString::from_str("NSScreenNumber");
        let number: *mut AnyObject = msg_send![device_description, objectForKey: &*key];
        if number.is_null() {
            return None;
        }
        let display_id: u32 = msg_send![number, unsignedIntValue];
        Some(display_id)
    }
}

#[cfg(not(target_os = "macos"))]
fn quick_terminal_target_display_impl(_mode: noa_config::QuickTerminalScreen) -> Option<u32> {
    None
}

/// The frontmost app's pid at the moment the quick terminal is about to
/// summon itself (Ghostty parity: it restores this app on hide instead of
/// leaving whatever macOS happens to raise next). `None` when the frontmost
/// app is already Noa itself — never store our own pid, or hide would
/// "restore" us to ourselves — or off macOS.
pub(crate) fn frontmost_app_pid() -> Option<i32> {
    frontmost_app_pid_impl()
}

#[cfg(target_os = "macos")]
fn frontmost_app_pid_impl() -> Option<i32> {
    use objc2::msg_send;
    use objc2::runtime::{AnyClass, AnyObject};

    // SAFETY: `NSWorkspace` is a live AppKit singleton queried on the main
    // (window-owning) thread; every object pointer is nil-checked before use.
    unsafe {
        let workspace_class = AnyClass::get(c"NSWorkspace")?;
        let workspace: *mut AnyObject = msg_send![workspace_class, sharedWorkspace];
        if workspace.is_null() {
            return None;
        }
        let app: *mut AnyObject = msg_send![workspace, frontmostApplication];
        if app.is_null() {
            return None;
        }
        let pid: i32 = msg_send![app, processIdentifier];
        if pid == std::process::id() as i32 {
            return None;
        }
        Some(pid)
    }
}

#[cfg(not(target_os = "macos"))]
fn frontmost_app_pid_impl() -> Option<i32> {
    None
}

/// Activate the running application identified by `pid` (`NSRunningApplication
/// .activateWithOptions:`), used to restore focus to whatever app was
/// frontmost before the quick terminal was summoned. `false` when the pid no
/// longer resolves to a running application (e.g. it quit while the panel was
/// open) or off macOS.
pub(crate) fn activate_app_with_pid(pid: i32) -> bool {
    activate_app_with_pid_impl(pid)
}

#[cfg(target_os = "macos")]
fn activate_app_with_pid_impl(pid: i32) -> bool {
    use objc2::msg_send;
    use objc2::runtime::{AnyClass, AnyObject};

    // NSApplicationActivationOptions: none — just bring the app forward
    // without also unhiding/activating all of its windows.
    const NS_APPLICATION_ACTIVATE_NONE: usize = 0;

    // SAFETY: `NSRunningApplication` is a live AppKit class queried on the
    // main (window-owning) thread; the looked-up object is nil-checked
    // before the selector is sent.
    unsafe {
        let Some(class) = AnyClass::get(c"NSRunningApplication") else {
            return false;
        };
        let app: *mut AnyObject = msg_send![class, runningApplicationWithProcessIdentifier: pid];
        if app.is_null() {
            return false;
        }
        msg_send![app, activateWithOptions: NS_APPLICATION_ACTIVATE_NONE]
    }
}

#[cfg(not(target_os = "macos"))]
fn activate_app_with_pid_impl(_pid: i32) -> bool {
    false
}

/// Whether Noa is currently the active (frontmost) application
/// (`NSApplication.isActive`). Used to guard restoring a stored previous-app
/// pid on quick-terminal hide: if Noa isn't active, the user already switched
/// away on their own and stealing focus back would fight them.
pub(crate) fn app_is_active() -> bool {
    app_is_active_impl()
}

#[cfg(target_os = "macos")]
fn app_is_active_impl() -> bool {
    use objc2::msg_send;
    use objc2::runtime::{AnyClass, AnyObject};

    // SAFETY: `NSApplication` is a live AppKit singleton queried on the main
    // (window-owning) thread; the shared instance is nil-checked before use.
    unsafe {
        let Some(app_class) = AnyClass::get(c"NSApplication") else {
            return false;
        };
        let app: *mut AnyObject = msg_send![app_class, sharedApplication];
        if app.is_null() {
            return false;
        }
        msg_send![app, isActive]
    }
}

#[cfg(not(target_os = "macos"))]
fn app_is_active_impl() -> bool {
    false
}

/// Bring the whole app to the front (`activateIgnoringOtherApps:`), for the
/// AppleScript application-level `activate` verb (applescript R-5). A no-op off
/// macOS.
#[cfg(target_os = "macos")]
pub(crate) fn activate_app() {
    use objc2::msg_send;
    use objc2::runtime::{AnyClass, AnyObject};

    // SAFETY: runs on the main event-loop thread; `NSApplication` is a live
    // singleton and the pointer is nil-checked before the selector is sent.
    unsafe {
        if let Some(app_class) = AnyClass::get(c"NSApplication") {
            let app: *mut AnyObject = msg_send![app_class, sharedApplication];
            if !app.is_null() {
                let _: () = msg_send![app, activateIgnoringOtherApps: true];
            }
        }
    }
}

#[cfg(target_os = "macos")]
fn toggle_native_fullscreen_impl(window: &Window) -> bool {
    use objc2::msg_send;
    use objc2::runtime::AnyObject;
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

    let Ok(handle) = window.window_handle() else {
        return false;
    };
    let RawWindowHandle::AppKit(appkit) = handle.as_raw() else {
        return false;
    };
    let ns_view = appkit.ns_view.as_ptr().cast::<AnyObject>();

    // SAFETY: `ns_view` is winit's live AppKit `NSView` for this window and we
    // are on the main event loop thread. `toggleFullScreen:` is an AppKit
    // window action; a nil sender is the documented programmatic form.
    unsafe {
        let ns_window: *mut AnyObject = msg_send![ns_view, window];
        if ns_window.is_null() {
            return false;
        }
        let sender: *mut AnyObject = std::ptr::null_mut();
        let _: () = msg_send![ns_window, toggleFullScreen: sender];
    }
    true
}

#[cfg(not(target_os = "macos"))]
fn toggle_native_fullscreen_impl(_window: &Window) -> bool {
    false
}

#[cfg(target_os = "macos")]
fn show_quick_terminal_window_impl(window: &Window) {
    use objc2::msg_send;
    use objc2::runtime::{AnyClass, AnyObject};
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

    let Ok(handle) = window.window_handle() else {
        return;
    };
    let RawWindowHandle::AppKit(appkit) = handle.as_raw() else {
        return;
    };
    let ns_view = appkit.ns_view.as_ptr().cast::<AnyObject>();

    // SAFETY: command dispatch runs on the main event-loop thread, and the
    // objects are live AppKit objects owned by winit. Each pointer is nil-
    // checked before selectors are sent.
    unsafe {
        if let Some(app_class) = AnyClass::get(c"NSApplication") {
            let app: *mut AnyObject = msg_send![app_class, sharedApplication];
            if !app.is_null() {
                let _: () = msg_send![app, activateIgnoringOtherApps: true];
            }
        }

        let ns_window: *mut AnyObject = msg_send![ns_view, window];
        if ns_window.is_null() {
            return;
        }
        let sender: *mut AnyObject = std::ptr::null_mut();
        let _: () = msg_send![ns_window, makeKeyAndOrderFront: sender];
        let _: () = msg_send![ns_window, orderFrontRegardless];
    }
}

#[cfg(not(target_os = "macos"))]
fn show_quick_terminal_window_impl(window: &Window) {
    window.focus_window();
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

/// Remove Noa's titlebar backdrop view when a full-size content view can supply
/// defined pixels itself, such as a visible terminal background image.
pub(crate) fn remove_titlebar_backdrop(window: &Window) {
    remove_titlebar_backdrop_impl(window);
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
        // `-CGColor` returns `^{CGColor=}`, not an object — the opaque type
        // keeps objc2's debug-mode encoding verification satisfied.
        let cg_color: *mut crate::macos_overlay::cg::CGColor = msg_send![color, CGColor];

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

#[cfg(target_os = "macos")]
fn remove_titlebar_backdrop_impl(window: &Window) {
    use objc2::msg_send;
    use objc2::runtime::{AnyClass, AnyObject};
    use objc2_foundation::NSString;
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

    const CONTAINER_CLASS: &[u8] = b"NSTitlebarContainerView";

    let Ok(handle) = window.window_handle() else {
        return;
    };
    let RawWindowHandle::AppKit(appkit) = handle.as_raw() else {
        return;
    };
    let ns_view = appkit.ns_view.as_ptr().cast::<AnyObject>();
    let identifier = NSString::from_str(TITLEBAR_BACKDROP_ID);

    unsafe {
        let ns_window: *mut AnyObject = msg_send![ns_view, window];
        if ns_window.is_null() {
            return;
        }
        let content_view: *mut AnyObject = msg_send![ns_window, contentView];
        if content_view.is_null() {
            return;
        }
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

        let container_subviews: *mut AnyObject = msg_send![container, subviews];
        if container_subviews.is_null() {
            return;
        }
        let n: usize = msg_send![container_subviews, count];
        for i in 0..n {
            let view: *mut AnyObject = msg_send![container_subviews, objectAtIndex: i];
            if view.is_null() {
                continue;
            }
            let ident: *mut AnyObject = msg_send![view, identifier];
            if ident.is_null() {
                continue;
            }
            let same: bool = msg_send![ident, isEqualToString: &*identifier];
            if same {
                let _: () = msg_send![view, removeFromSuperview];
                return;
            }
        }
    }
}

#[cfg(not(target_os = "macos"))]
fn remove_titlebar_backdrop_impl(_window: &Window) {}

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

/// Set (or clear) the titlebar proxy icon: `NSWindow.representedURL`, the
/// folder/file glyph Finder can Cmd-click or drag from (REQ-PXI-2).
/// `path: None` clears it to `nil`. No file-existence check is made
/// (REQ-PXI-5, Ghostty parity) — a stale/deleted directory still sets it.
pub(crate) fn set_represented_url(window: &Window, path: Option<&str>) {
    set_represented_url_impl(window, path);
}

#[cfg(target_os = "macos")]
fn set_represented_url_impl(window: &Window, path: Option<&str>) {
    use objc2::msg_send;
    use objc2::runtime::{AnyClass, AnyObject};
    use objc2_foundation::NSString;
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

    let Ok(handle) = window.window_handle() else {
        return;
    };
    let RawWindowHandle::AppKit(appkit) = handle.as_raw() else {
        return;
    };
    let ns_view = appkit.ns_view.as_ptr().cast::<AnyObject>();

    // SAFETY: `ns_view` is winit's live AppKit `NSView` for this window and
    // we are on the main (window-owning) thread. `setRepresentedURL:` is a
    // plain AppKit property write that accepts nil to clear the icon.
    unsafe {
        let ns_window: *mut AnyObject = msg_send![ns_view, window];
        if ns_window.is_null() {
            return;
        }
        let Some(path) = path else {
            let nil: *mut AnyObject = std::ptr::null_mut();
            let _: () = msg_send![ns_window, setRepresentedURL: nil];
            return;
        };
        let Some(url_class) = AnyClass::get(c"NSURL") else {
            return;
        };
        let ns_path = NSString::from_str(path);
        let url: *mut AnyObject = msg_send![url_class, fileURLWithPath: &*ns_path];
        if url.is_null() {
            return;
        }
        let _: () = msg_send![ns_window, setRepresentedURL: url];
    }
}

#[cfg(not(target_os = "macos"))]
fn set_represented_url_impl(_window: &Window, _path: Option<&str>) {}

/// Show the system dictionary/definition popup for `text`, anchored at
/// `(point_x, point_y)` — AppKit view coordinates: points, bottom-left
/// origin (Quick Look force-click, REQ-QLK-4). `font_name`/`font_size` are a
/// best-effort `NSFont` attribute (REQ-QLK-6): a lookup failure still shows
/// the popup, just without a font attribute.
pub(crate) fn show_definition(
    window: &Window,
    text: &str,
    font_name: Option<&str>,
    font_size: f32,
    point_x: f64,
    point_y: f64,
) {
    show_definition_impl(window, text, font_name, font_size, point_x, point_y);
}

#[cfg(target_os = "macos")]
fn show_definition_impl(
    window: &Window,
    text: &str,
    font_name: Option<&str>,
    font_size: f32,
    point_x: f64,
    point_y: f64,
) {
    use objc2::msg_send;
    use objc2::runtime::{AnyClass, AnyObject};
    use objc2_foundation::{NSPoint, NSString};
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

    let Ok(handle) = window.window_handle() else {
        return;
    };
    let RawWindowHandle::AppKit(appkit) = handle.as_raw() else {
        return;
    };
    let ns_view = appkit.ns_view.as_ptr().cast::<AnyObject>();
    let ns_text = NSString::from_str(text);
    let point = NSPoint {
        x: point_x,
        y: point_y,
    };

    let font = font_name.and_then(|name| resolve_font(name, font_size));

    // SAFETY: main (window-owning) thread; every object pointer is
    // nil-checked before use. `NSAttributedString`/`NSDictionary` are built
    // via the codebase's raw `AnyClass::get` + `msg_send!` pattern (as
    // `NSColor`/`NSView` already are above) rather than objc2-foundation's
    // typed, feature-gated wrappers — that feature isn't declared in this
    // crate (only `muda` pulls in `NSAttributedString` transitively today),
    // so relying on it would be fragile and declaring it is an unneeded
    // Cargo.toml change for this one call.
    unsafe {
        let Some(string_class) = AnyClass::get(c"NSAttributedString") else {
            return;
        };
        let alloc: *mut AnyObject = msg_send![string_class, alloc];
        if alloc.is_null() {
            return;
        }

        // `NSFontAttributeName`'s value is the stable, documented string
        // `"NSFont"` — used directly rather than linking the constant symbol.
        let dict: *mut AnyObject = match font.zip(AnyClass::get(c"NSDictionary")) {
            Some((font, dict_class)) => {
                let key = NSString::from_str("NSFont");
                msg_send![dict_class, dictionaryWithObject: font, forKey: &*key]
            }
            None => std::ptr::null_mut(),
        };

        let attributed: *mut AnyObject = if dict.is_null() {
            msg_send![alloc, initWithString: &*ns_text]
        } else {
            msg_send![alloc, initWithString: &*ns_text, attributes: dict]
        };
        if attributed.is_null() {
            return;
        }

        let _: () =
            msg_send![ns_view, showDefinitionForAttributedString: attributed, atPoint: point];

        // `alloc`+`initWithString:`/`initWithString:attributes:` above
        // yielded a +1-retained object; release our reference now that
        // `showDefinitionForAttributedString:atPoint:` (which retains its
        // own copy internally) has returned.
        let _: () = msg_send![attributed, release];
    }
}

#[cfg(not(target_os = "macos"))]
fn show_definition_impl(
    _window: &Window,
    _text: &str,
    _font_name: Option<&str>,
    _font_size: f32,
    _point_x: f64,
    _point_y: f64,
) {
}

/// Resolves `name`/`size` to a live `NSFont` via `fontWithName:size:`,
/// returning `None` on lookup failure (unknown family) rather than
/// panicking (REQ-QLK-6) — the caller still shows the definition popup,
/// just without a font attribute.
#[cfg(target_os = "macos")]
fn resolve_font(name: &str, size: f32) -> Option<*mut objc2::runtime::AnyObject> {
    use objc2::msg_send;
    use objc2::runtime::{AnyClass, AnyObject};
    use objc2_foundation::NSString;

    let font_class = AnyClass::get(c"NSFont")?;
    let ns_name = NSString::from_str(name);
    // SAFETY: `fontWithName:size:` is a plain AppKit class method; an
    // unknown family name returns nil rather than throwing.
    let font: *mut AnyObject =
        unsafe { msg_send![font_class, fontWithName: &*ns_name, size: f64::from(size)] };
    if font.is_null() { None } else { Some(font) }
}

/// The `com.apple.trackpad.forceClick` user default (REQ-QLK-1): fires when
/// the key is absent or `true` (Apple's factory default is force-click
/// enabled), and is suppressed only when it's explicitly `false` — a bare
/// `boolForKey:` would return `false` for an absent key and silently disable
/// Quick Look on never-customized systems, so the key's presence is checked
/// first via `objectForKey:`.
pub(crate) fn force_click_preference_enabled() -> bool {
    force_click_preference_enabled_impl()
}

#[cfg(target_os = "macos")]
fn force_click_preference_enabled_impl() -> bool {
    use objc2::msg_send;
    use objc2::runtime::{AnyClass, AnyObject};
    use objc2_foundation::NSString;

    let Some(defaults_class) = AnyClass::get(c"NSUserDefaults") else {
        return true;
    };
    let key = NSString::from_str("com.apple.trackpad.forceClick");

    // SAFETY: `NSUserDefaults`/`objectForKey:`/`boolForKey:` are plain,
    // main-thread-safe Foundation reads.
    unsafe {
        let defaults: *mut AnyObject = msg_send![defaults_class, standardUserDefaults];
        if defaults.is_null() {
            return true;
        }
        let value: *mut AnyObject = msg_send![defaults, objectForKey: &*key];
        if value.is_null() {
            return true;
        }
        msg_send![defaults, boolForKey: &*key]
    }
}

#[cfg(not(target_os = "macos"))]
fn force_click_preference_enabled_impl() -> bool {
    true
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;

    // AC-QLK-8: an unknown font family must not panic, and resolves to
    // `None` so the caller falls back to no font attribute.
    #[test]
    fn resolve_font_returns_none_for_an_unknown_family() {
        assert!(resolve_font("Definitely-Not-A-Real-Font-XYZ-12345", 12.0).is_none());
    }
}
