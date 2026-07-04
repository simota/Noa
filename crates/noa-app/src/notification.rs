//! Desktop notifications for terminal-requested OSC 9 / OSC 777 events.
//!
//! Deliberately uses the deprecated `NSUserNotification` API: the modern
//! `UNUserNotificationCenter` requires a code-signed, bundled app with the
//! right entitlements, so it silently no-ops for a bare `cargo run` binary or
//! an ad-hoc-signed dev build. `NSUserNotification` still posts from an
//! unbundled process — the common case for a terminal launched from a dev
//! checkout. A no-op on every non-macOS platform.

/// Whether a terminal-requested notification should surface. Ghostty suppresses
/// notifications for the window that already has OS focus (the user is looking
/// at that terminal), so they fire only for background activity.
pub(crate) fn should_notify(target_window_focused: bool) -> bool {
    !target_window_focused
}

/// The title to display: the requested one when non-empty, else `"noa"`.
pub(crate) fn notification_title(title: Option<&str>) -> &str {
    match title {
        Some(t) if !t.is_empty() => t,
        _ => "noa",
    }
}

/// Post `body` (with an optional `title`, defaulting to `"noa"`) to the macOS
/// notification center and bounce the Dock. Call only from the main thread.
pub(crate) fn post_notification(title: Option<&str>, body: &str) {
    post(notification_title(title), body);
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
    fn suppresses_notifications_for_the_focused_window() {
        assert!(!should_notify(true));
        assert!(should_notify(false));
    }

    #[test]
    fn falls_back_to_noa_for_a_missing_or_empty_title() {
        assert_eq!(notification_title(None), "noa");
        assert_eq!(notification_title(Some("")), "noa");
        assert_eq!(notification_title(Some("build done")), "build done");
    }
}
