//! Desktop notifications for terminal-requested OSC 9 / OSC 777 events.
//!
//! Deliberately uses the deprecated `NSUserNotification` API: the modern
//! `UNUserNotificationCenter` requires a code-signed, bundled app with the
//! right entitlements, so it silently no-ops for a bare `cargo run` binary or
//! an ad-hoc-signed dev build. `NSUserNotification` still posts from an
//! unbundled process — the common case for a terminal launched from a dev
//! checkout. On Linux notifications go through the freedesktop notification
//! service via `notify-send` (libnotify), and the bell plays the sound theme's
//! `bell` event via `canberra-gtk-play`; both degrade silently when the tool
//! is missing.

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

/// Play the system alert sound for terminal BEL: `NSBeep` on macOS, the
/// XDG sound theme's `bell` event elsewhere.
pub(crate) fn beep() {
    #[cfg(target_os = "macos")]
    // SAFETY: `NSBeep` is a documented AppKit function and is called on the
    // winit main thread from BEL event handling.
    unsafe {
        NSBeep();
    }
    #[cfg(not(target_os = "macos"))]
    if bell_rate_limit_allows(std::time::Instant::now()) {
        spawn_detached(
            "canberra-gtk-play",
            &["--id=bell", "--description=Noa bell"],
        );
    }
}

/// A BEL flood (`yes $'\a'`) must not fork a sound player per byte: at most
/// one bell sound per [`BELL_MIN_INTERVAL`].
#[cfg(not(target_os = "macos"))]
const BELL_MIN_INTERVAL: std::time::Duration = std::time::Duration::from_millis(100);

#[cfg(not(target_os = "macos"))]
fn bell_rate_limit_allows(now: std::time::Instant) -> bool {
    use std::sync::Mutex;
    static LAST: Mutex<Option<std::time::Instant>> = Mutex::new(None);
    let mut last = LAST.lock().unwrap_or_else(|poison| poison.into_inner());
    if last.is_some_and(|at| now.saturating_duration_since(at) < BELL_MIN_INTERVAL) {
        return false;
    }
    *last = Some(now);
    true
}

/// Run a helper program without blocking the event loop; its exit is reaped
/// on a throwaway thread so it never lingers as a zombie. A missing program
/// is not an error worth surfacing (headless box, minimal desktop).
#[cfg(not(target_os = "macos"))]
fn spawn_detached(program: &'static str, args: &[&str]) {
    let mut command = std::process::Command::new(program);
    command
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    match command.spawn() {
        Ok(mut child) => {
            std::thread::spawn(move || {
                let _ = child.wait();
            });
        }
        Err(err) => log::debug!("{program} unavailable: {err}"),
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
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
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
fn post(title: &str, body: &str) {
    spawn_detached("notify-send", &notify_send_args(title, body));
}

/// `notify-send` arguments: `--` ends option parsing so a body that starts
/// with `-` (an OSC 9 payload is arbitrary program output) is never read as
/// a flag.
#[cfg(not(target_os = "macos"))]
fn notify_send_args<'a>(title: &'a str, body: &'a str) -> [&'a str; 5] {
    ["--app-name=Noa", "--icon=com.simota.noa", "--", title, body]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn notify_send_args_end_option_parsing_before_payload() {
        assert_eq!(
            notify_send_args("--title", "-rf body"),
            [
                "--app-name=Noa",
                "--icon=com.simota.noa",
                "--",
                "--title",
                "-rf body"
            ]
        );
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn bell_sound_is_rate_limited() {
        let start = std::time::Instant::now() + std::time::Duration::from_secs(3600);
        assert!(bell_rate_limit_allows(start));
        assert!(!bell_rate_limit_allows(
            start + std::time::Duration::from_millis(10)
        ));
        assert!(bell_rate_limit_allows(start + BELL_MIN_INTERVAL));
    }

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
