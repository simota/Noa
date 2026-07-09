//! Desktop notifications for terminal-requested OSC 9 / OSC 777 events.
//!
//! Deliberately uses the deprecated `NSUserNotification` API: the modern
//! `UNUserNotificationCenter` requires a code-signed, bundled app with the
//! right entitlements, so it silently no-ops for a bare `cargo run` binary or
//! an ad-hoc-signed dev build. `NSUserNotification` still posts from an
//! unbundled process — the common case for a terminal launched from a dev
//! checkout. A no-op on every non-macOS platform.

/// Whether a terminal-requested notification should surface. Ghostty suppresses
/// notifications only for the window that *actually* holds OS focus (the user is
/// looking at that terminal); a backgrounded app (`os_focused == None`, no
/// window focused) always surfaces them. `os_focused` must reflect real OS
/// focus, not the last-focused window — otherwise the main case (a build
/// finishing while the user is in another app) never fires.
pub(crate) fn should_notify<Id: PartialEq>(os_focused: Option<Id>, target: Id) -> bool {
    os_focused != Some(target)
}

/// Whether a BEL should play the audible system bell under the resolved config.
pub(crate) fn should_ring_audible_bell<Id: PartialEq>(
    audible_bell: bool,
    when_unfocused: bool,
    os_focused: Option<Id>,
    target: Id,
) -> bool {
    audible_bell && (!when_unfocused || should_notify(os_focused, target))
}

/// The title to display: the requested one when non-empty, else `"Noa"`.
pub(crate) fn notification_title(title: Option<&str>) -> &str {
    match title {
        Some(t) if !t.is_empty() => t,
        _ => "Noa",
    }
}

/// Post `body` (with an optional `title`, defaulting to `"Noa"`) to the macOS
/// notification center and bounce the Dock. Call only from the main thread.
pub(crate) fn post_notification(title: Option<&str>, body: &str) {
    post(notification_title(title), body);
}

/// Play the macOS system alert sound for terminal BEL. A no-op off macOS.
pub(crate) fn beep() {
    #[cfg(target_os = "macos")]
    // SAFETY: `NSBeep` is a documented AppKit function and is called on the
    // winit main thread from BEL event handling.
    unsafe {
        NSBeep();
    }
}

#[cfg(target_os = "macos")]
#[link(name = "AppKit", kind = "framework")]
unsafe extern "C" {
    fn NSBeep();
}

#[cfg(target_os = "macos")]
fn post(title: &str, body: &str) {
    use objc2::msg_send;
    use objc2::runtime::{AnyClass, AnyObject};
    use objc2_foundation::NSString;

    // SAFETY: every object below is an AppKit runtime object created and
    // consumed on the main thread, and every selector is the documented
    // NSUserNotification / NSApplication API. The classes are looked up at
    // runtime rather than linked, because objc2-app-kit's 0.3 bindings omit
    // the deprecated NSUserNotification family; we only send plain
    // `new`/setter/post/`release` messages.
    unsafe {
        let (Some(center_class), Some(note_class)) = (
            AnyClass::get(c"NSUserNotificationCenter"),
            AnyClass::get(c"NSUserNotification"),
        ) else {
            return;
        };
        let center: *mut AnyObject = msg_send![center_class, defaultUserNotificationCenter];
        if center.is_null() {
            return;
        }
        let note: *mut AnyObject = msg_send![note_class, new];
        if note.is_null() {
            return;
        }
        let title_ns = NSString::from_str(title);
        let body_ns = NSString::from_str(body);
        let _: () = msg_send![note, setTitle: &*title_ns];
        let _: () = msg_send![note, setInformativeText: &*body_ns];
        let _: () = msg_send![center, deliverNotification: note];
        // `new` handed us a +1 retain; the center holds its own, so drop ours.
        let _: () = msg_send![note, release];

        request_dock_attention();
    }
}

/// Bounce the Dock icon once without posting an OS notification (FR-A5): used
/// when an agent session rings the bell to request attention, where a full
/// notification-center entry per bell would be too noisy. Call only from the
/// main thread. A no-op off macOS.
pub(crate) fn bounce_dock() {
    #[cfg(target_os = "macos")]
    // SAFETY: `request_dock_attention` sends the documented AppKit
    // `requestUserAttention:` on the main-thread shared application.
    unsafe {
        request_dock_attention();
    }
}

/// Bounce the Dock icon once (stops when the app is activated). Separate from
/// the notification post so a missing notification center still gets attention.
#[cfg(target_os = "macos")]
unsafe fn request_dock_attention() {
    use objc2::msg_send;
    use objc2::runtime::{AnyClass, AnyObject};

    // NSInformationalRequest: a single bounce, cleared on activation.
    const NS_INFORMATIONAL_REQUEST: isize = 10;
    let Some(app_class) = AnyClass::get(c"NSApplication") else {
        return;
    };
    // SAFETY: main-thread `sharedApplication` returns the live app instance;
    // `requestUserAttention:` is a plain AppKit call taking the request enum.
    unsafe {
        let app: *mut AnyObject = msg_send![app_class, sharedApplication];
        if app.is_null() {
            return;
        }
        let _: isize = msg_send![app, requestUserAttention: NS_INFORMATIONAL_REQUEST];
    }
}

#[cfg(not(target_os = "macos"))]
fn post(_title: &str, _body: &str) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suppresses_notifications_only_for_the_os_focused_window() {
        // The OS-focused target is suppressed…
        assert!(!should_notify(Some(1), 1));
        // …but a different focused window still surfaces it…
        assert!(should_notify(Some(2), 1));
        // …and a fully backgrounded app (nothing focused) always fires.
        assert!(should_notify(None, 1));
    }

    #[test]
    fn audible_bell_respects_enabled_flag_and_focus_gate() {
        assert!(!should_ring_audible_bell(false, false, Some(1), 1));
        assert!(should_ring_audible_bell(true, false, Some(1), 1));
        assert!(!should_ring_audible_bell(true, true, Some(1), 1));
        assert!(should_ring_audible_bell(true, true, Some(2), 1));
        assert!(should_ring_audible_bell(true, true, None::<i32>, 1));
    }

    #[test]
    fn falls_back_to_noa_for_a_missing_or_empty_title() {
        assert_eq!(notification_title(None), "Noa");
        assert_eq!(notification_title(Some("")), "Noa");
        assert_eq!(notification_title(Some("build done")), "build done");
    }
}
