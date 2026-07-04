//! Global (system-wide) hotkey registration for the quick terminal, via the
//! Carbon `RegisterEventHotKey` API.
//!
//! `RegisterEventHotKey` is deliberately chosen over a `CGEventTap`: it needs
//! **no Accessibility permission**, delivers the hotkey through the process's
//! normal Carbon/Cocoa event target (so the callback runs on the main thread
//! alongside winit), and is exactly the mechanism Ghostty uses for the same
//! feature. The tradeoff is that the chord is a fixed keycode+modifier combo
//! (no fuzzy matching), which is all a single toggle hotkey needs.
//!
//! The chord string (`cmd+grave`) is parsed by [`parse_hotkey`] into a Carbon
//! virtual keycode + modifier mask; that parse is pure and unit-tested. The
//! FFI registration ([`GlobalHotKey::register`]) is macOS-only and a no-op
//! elsewhere.

use winit::event_loop::EventLoopProxy;

use crate::UserEvent;

/// A parsed hotkey chord: a Carbon virtual keycode and modifier mask.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct HotkeyChord {
    pub keycode: u32,
    pub modifiers: u32,
}

// Carbon modifier masks (`Events.h`).
const CMD_KEY: u32 = 0x0100;
const SHIFT_KEY: u32 = 0x0200;
const OPTION_KEY: u32 = 0x0800;
const CONTROL_KEY: u32 = 0x1000;

/// Parse a config chord (`cmd+grave`, `ctrl+shift+t`, …) into a Carbon
/// keycode + modifier mask. Modifier aliases match the in-app keybind parser
/// (`cmd`/`command`/`super`/`meta`, `ctrl`/`control`, `alt`/`option`,
/// `shift`). Returns `None` for an empty chord, a missing/unknown key, or a
/// chord naming more than one non-modifier key.
pub(crate) fn parse_hotkey(spec: &str) -> Option<HotkeyChord> {
    let mut modifiers = 0u32;
    let mut keycode = None;
    for token in spec
        .split('+')
        .map(str::trim)
        .filter(|part| !part.is_empty())
    {
        let normalized = token.to_ascii_lowercase();
        match normalized.as_str() {
            "cmd" | "command" | "super" | "meta" => modifiers |= CMD_KEY,
            "ctrl" | "control" => modifiers |= CONTROL_KEY,
            "alt" | "option" => modifiers |= OPTION_KEY,
            "shift" => modifiers |= SHIFT_KEY,
            key => {
                if keycode.is_some() {
                    return None;
                }
                keycode = Some(carbon_keycode(key)?);
            }
        }
    }
    keycode.map(|keycode| HotkeyChord { keycode, modifiers })
}

/// Map a key token to its Carbon virtual keycode (`kVK_ANSI_*` / `kVK_*`).
/// Covers the ASCII letters/digits and the punctuation/named keys most likely
/// to anchor a global hotkey; unknown tokens return `None`.
fn carbon_keycode(key: &str) -> Option<u32> {
    let named = match key {
        "a" => 0x00,
        "s" => 0x01,
        "d" => 0x02,
        "f" => 0x03,
        "h" => 0x04,
        "g" => 0x05,
        "z" => 0x06,
        "x" => 0x07,
        "c" => 0x08,
        "v" => 0x09,
        "b" => 0x0B,
        "q" => 0x0C,
        "w" => 0x0D,
        "e" => 0x0E,
        "r" => 0x0F,
        "y" => 0x10,
        "t" => 0x11,
        "1" => 0x12,
        "2" => 0x13,
        "3" => 0x14,
        "4" => 0x15,
        "6" => 0x16,
        "5" => 0x17,
        "=" | "equal" => 0x18,
        "9" => 0x19,
        "7" => 0x1A,
        "-" | "minus" => 0x1B,
        "8" => 0x1C,
        "0" => 0x1D,
        "]" | "rightbracket" => 0x1E,
        "o" => 0x1F,
        "u" => 0x20,
        "[" | "leftbracket" => 0x21,
        "i" => 0x22,
        "p" => 0x23,
        "l" => 0x25,
        "j" => 0x26,
        "k" => 0x28,
        ";" | "semicolon" => 0x29,
        "\\" | "backslash" => 0x2A,
        "," | "comma" => 0x2B,
        "/" | "slash" => 0x2C,
        "n" => 0x2D,
        "m" => 0x2E,
        "." | "period" => 0x2F,
        "`" | "grave" | "backtick" => 0x32,
        "enter" | "return" => 0x24,
        "tab" => 0x30,
        "space" => 0x31,
        "escape" | "esc" => 0x35,
        _ => return None,
    };
    Some(named)
}

/// A registered global hotkey. Dropping it unregisters the hotkey and removes
/// the event handler, and frees the boxed proxy the callback borrowed.
pub(crate) struct GlobalHotKey {
    // Held only for its `Drop` (unregister); never read.
    #[cfg(target_os = "macos")]
    _registration: macos::Registration,
    #[cfg(not(target_os = "macos"))]
    _unused: (),
}

impl GlobalHotKey {
    /// Register `spec` as a system-wide hotkey that posts
    /// [`UserEvent::ToggleQuickTerminal`] through `proxy` when pressed. Returns
    /// `None` when the chord is unparseable, registration fails, or the
    /// platform has no global-hotkey support.
    pub(crate) fn register(spec: &str, proxy: EventLoopProxy<UserEvent>) -> Option<Self> {
        let chord = parse_hotkey(spec)?;
        #[cfg(target_os = "macos")]
        {
            macos::Registration::install(chord, proxy).map(|registration| Self {
                _registration: registration,
            })
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = (chord, proxy);
            None
        }
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use std::ffi::c_void;

    use winit::event_loop::EventLoopProxy;

    use super::HotkeyChord;
    use crate::UserEvent;

    type OsStatus = i32;
    type EventTargetRef = *mut c_void;
    type EventHandlerRef = *mut c_void;
    type EventHandlerCallRef = *mut c_void;
    type EventRef = *mut c_void;
    type EventHotKeyRef = *mut c_void;
    type EventHandlerUpp = extern "C" fn(EventHandlerCallRef, EventRef, *mut c_void) -> OsStatus;

    #[repr(C)]
    struct EventTypeSpec {
        event_class: u32,
        event_kind: u32,
    }

    #[repr(C)]
    struct EventHotKeyId {
        signature: u32,
        id: u32,
    }

    const K_EVENT_CLASS_KEYBOARD: u32 = u32::from_be_bytes(*b"keyb");
    const K_EVENT_HOTKEY_PRESSED: u32 = 6;
    // FourCharCode signature identifying this app's hotkeys.
    const HOTKEY_SIGNATURE: u32 = u32::from_be_bytes(*b"noaq");

    #[link(name = "Carbon", kind = "framework")]
    unsafe extern "C" {
        fn GetApplicationEventTarget() -> EventTargetRef;
        fn RegisterEventHotKey(
            key_code: u32,
            modifiers: u32,
            hot_key_id: EventHotKeyId,
            target: EventTargetRef,
            options: u32,
            out_ref: *mut EventHotKeyRef,
        ) -> OsStatus;
        fn UnregisterEventHotKey(hot_key: EventHotKeyRef) -> OsStatus;
        fn InstallEventHandler(
            target: EventTargetRef,
            handler: EventHandlerUpp,
            num_types: u32,
            list: *const EventTypeSpec,
            user_data: *mut c_void,
            out_ref: *mut EventHandlerRef,
        ) -> OsStatus;
        fn RemoveEventHandler(handler: EventHandlerRef) -> OsStatus;
    }

    /// The hotkey callback fires on the main thread (same run loop as winit),
    /// so it just forwards a toggle event through the proxy it was handed.
    extern "C" fn hotkey_handler(
        _call: EventHandlerCallRef,
        _event: EventRef,
        user_data: *mut c_void,
    ) -> OsStatus {
        // SAFETY: `user_data` is the `Box<EventLoopProxy>` leaked in `install`
        // and kept alive by the owning `Registration`, so the pointer is valid
        // for the whole time the handler is installed.
        let proxy = unsafe { &*(user_data as *const EventLoopProxy<UserEvent>) };
        let _ = proxy.send_event(UserEvent::ToggleQuickTerminal);
        0
    }

    pub(super) struct Registration {
        hotkey_ref: EventHotKeyRef,
        handler_ref: EventHandlerRef,
        proxy: *mut EventLoopProxy<UserEvent>,
    }

    impl Registration {
        pub(super) fn install(
            chord: HotkeyChord,
            proxy: EventLoopProxy<UserEvent>,
        ) -> Option<Self> {
            let proxy = Box::into_raw(Box::new(proxy));
            let spec = EventTypeSpec {
                event_class: K_EVENT_CLASS_KEYBOARD,
                event_kind: K_EVENT_HOTKEY_PRESSED,
            };
            let mut handler_ref: EventHandlerRef = std::ptr::null_mut();
            let mut hotkey_ref: EventHotKeyRef = std::ptr::null_mut();

            // SAFETY: all pointers are valid for the duration of the call. The
            // handler UPP is a plain function pointer (modern Carbon UPPs are),
            // and `proxy` outlives the handler because `Registration` owns it
            // and frees it only in `Drop`, after removing the handler.
            unsafe {
                let target = GetApplicationEventTarget();
                let status = InstallEventHandler(
                    target,
                    hotkey_handler,
                    1,
                    &spec,
                    proxy as *mut c_void,
                    &mut handler_ref,
                );
                if status != 0 {
                    drop(Box::from_raw(proxy));
                    log::warn!("InstallEventHandler failed for quick-terminal hotkey: {status}");
                    return None;
                }
                let hot_key_id = EventHotKeyId {
                    signature: HOTKEY_SIGNATURE,
                    id: 1,
                };
                let status = RegisterEventHotKey(
                    chord.keycode,
                    chord.modifiers,
                    hot_key_id,
                    target,
                    0,
                    &mut hotkey_ref,
                );
                if status != 0 {
                    RemoveEventHandler(handler_ref);
                    drop(Box::from_raw(proxy));
                    log::warn!("RegisterEventHotKey failed for quick-terminal hotkey: {status}");
                    return None;
                }
            }

            Some(Self {
                hotkey_ref,
                handler_ref,
                proxy,
            })
        }
    }

    impl Drop for Registration {
        fn drop(&mut self) {
            // SAFETY: both refs were produced by the matching register/install
            // calls and are unregistered exactly once here; the boxed proxy is
            // freed only after the handler that reads it is removed.
            unsafe {
                UnregisterEventHotKey(self.hotkey_ref);
                RemoveEventHandler(self.handler_ref);
                drop(Box::from_raw(self.proxy));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_cmd_grave() {
        let chord = parse_hotkey("cmd+grave").expect("cmd+grave parses");
        assert_eq!(chord.keycode, 0x32);
        assert_eq!(chord.modifiers, CMD_KEY);
    }

    #[test]
    fn backtick_and_grave_are_equivalent() {
        assert_eq!(parse_hotkey("cmd+`"), parse_hotkey("cmd+grave"));
    }

    #[test]
    fn accumulates_all_modifiers_in_any_order() {
        let chord = parse_hotkey("shift+ctrl+alt+cmd+t").expect("parses");
        assert_eq!(chord.keycode, 0x11);
        assert_eq!(
            chord.modifiers,
            CMD_KEY | SHIFT_KEY | OPTION_KEY | CONTROL_KEY
        );
    }

    #[test]
    fn modifier_aliases_match_the_keybind_parser() {
        assert_eq!(parse_hotkey("command+space"), parse_hotkey("cmd+space"));
        assert_eq!(parse_hotkey("super+space"), parse_hotkey("cmd+space"));
        assert_eq!(parse_hotkey("option+space"), parse_hotkey("alt+space"));
        assert_eq!(parse_hotkey("control+space"), parse_hotkey("ctrl+space"));
    }

    #[test]
    fn rejects_missing_key_unknown_key_and_multiple_keys() {
        assert_eq!(parse_hotkey("cmd+shift"), None);
        assert_eq!(parse_hotkey(""), None);
        assert_eq!(parse_hotkey("cmd+f13"), None);
        assert_eq!(parse_hotkey("cmd+a+b"), None);
    }

    #[test]
    fn a_bare_key_parses_with_no_modifiers() {
        let chord = parse_hotkey("f10").map(|c| c.modifiers);
        // "f10" is not a mapped key, so it is rejected; a mapped bare key is fine.
        assert_eq!(chord, None);
        assert_eq!(parse_hotkey("space").map(|c| c.modifiers), Some(0));
    }
}
