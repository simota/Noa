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

/// Distinguishes coexisting global hotkeys so one shared Carbon hotkey-pressed
/// handler can dispatch each to its own event. Every live [`GlobalHotKey`]
/// installs a handler for the same event class, so each must filter on the
/// fired hotkey's id and only forward its own (see `macos::hotkey_handler`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HotkeyAction {
    QuickTerminal,
    Sidebar,
}

impl HotkeyAction {
    /// The Carbon hotkey id and the `UserEvent` this action forwards.
    fn id(self) -> u32 {
        match self {
            HotkeyAction::QuickTerminal => 1,
            HotkeyAction::Sidebar => 2,
        }
    }

    fn event(self) -> UserEvent {
        match self {
            HotkeyAction::QuickTerminal => UserEvent::ToggleQuickTerminal,
            HotkeyAction::Sidebar => UserEvent::ToggleSidebar,
        }
    }
}

impl GlobalHotKey {
    /// Register `spec` as a system-wide hotkey that posts `action`'s event
    /// through `proxy` when pressed. Returns `None` when the chord is
    /// unparseable, registration fails, or the platform has no global-hotkey
    /// support. Multiple actions can be registered at once; each filters on its
    /// own hotkey id so a press only fires its matching event.
    pub(crate) fn register(
        spec: &str,
        proxy: EventLoopProxy<UserEvent>,
        action: HotkeyAction,
    ) -> Option<Self> {
        let chord = parse_hotkey(spec)?;
        #[cfg(target_os = "macos")]
        {
            macos::Registration::install(chord, proxy, action).map(|registration| Self {
                _registration: registration,
            })
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = (chord, proxy, action);
            None
        }
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use std::ffi::c_void;

    use winit::event_loop::EventLoopProxy;

    use super::{HotkeyAction, HotkeyChord};
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
    // `kEventParamDirectObject` / `typeEventHotKeyID` — the event parameter
    // carrying the fired hotkey's `EventHotKeyID`, read to dispatch by id.
    const K_EVENT_PARAM_DIRECT_OBJECT: u32 = u32::from_be_bytes(*b"----");
    const TYPE_EVENT_HOTKEY_ID: u32 = u32::from_be_bytes(*b"hkid");

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
        fn GetEventParameter(
            event: EventRef,
            name: u32,
            param_type: u32,
            out_actual_type: *mut u32,
            buffer_size: usize,
            out_actual_size: *mut usize,
            out_data: *mut c_void,
        ) -> OsStatus;
    }

    /// The boxed state each installed handler borrows: the proxy to post
    /// through, the event this action forwards, and the hotkey id to match.
    struct HandlerData {
        proxy: EventLoopProxy<UserEvent>,
        event: UserEvent,
        id: u32,
    }

    /// The hotkey callback fires on the main thread (same run loop as winit).
    /// Because every [`Registration`] installs a handler for the same
    /// hotkey-pressed event class, each handler runs for *every* app hotkey, so
    /// it must read the fired hotkey's id and forward its event only on a match.
    extern "C" fn hotkey_handler(
        _call: EventHandlerCallRef,
        event: EventRef,
        user_data: *mut c_void,
    ) -> OsStatus {
        // SAFETY: `user_data` is the `Box<HandlerData>` leaked in `install` and
        // kept alive by the owning `Registration`, so the pointer is valid for
        // the whole time the handler is installed.
        let data = unsafe { &*(user_data as *const HandlerData) };
        let mut fired = EventHotKeyId {
            signature: 0,
            id: 0,
        };
        let mut actual_size: usize = 0;
        // SAFETY: `event` is the live Carbon event for this callback; `fired`
        // is a correctly-sized `EventHotKeyID` output buffer.
        let status = unsafe {
            GetEventParameter(
                event,
                K_EVENT_PARAM_DIRECT_OBJECT,
                TYPE_EVENT_HOTKEY_ID,
                std::ptr::null_mut(),
                std::mem::size_of::<EventHotKeyId>(),
                &mut actual_size,
                &mut fired as *mut EventHotKeyId as *mut c_void,
            )
        };
        if status == 0 && fired.signature == HOTKEY_SIGNATURE && fired.id == data.id {
            let _ = data.proxy.send_event(data.event.clone());
        }
        0
    }

    pub(super) struct Registration {
        hotkey_ref: EventHotKeyRef,
        handler_ref: EventHandlerRef,
        data: *mut HandlerData,
    }

    impl Registration {
        pub(super) fn install(
            chord: HotkeyChord,
            proxy: EventLoopProxy<UserEvent>,
            action: HotkeyAction,
        ) -> Option<Self> {
            let hotkey_id = action.id();
            let data = Box::into_raw(Box::new(HandlerData {
                proxy,
                event: action.event(),
                id: hotkey_id,
            }));
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
                    data as *mut c_void,
                    &mut handler_ref,
                );
                if status != 0 {
                    drop(Box::from_raw(data));
                    log::warn!("InstallEventHandler failed for global hotkey: {status}");
                    return None;
                }
                let hot_key_id = EventHotKeyId {
                    signature: HOTKEY_SIGNATURE,
                    id: hotkey_id,
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
                    drop(Box::from_raw(data));
                    log::warn!("RegisterEventHotKey failed for global hotkey: {status}");
                    return None;
                }
            }

            Some(Self {
                hotkey_ref,
                handler_ref,
                data,
            })
        }
    }

    impl Drop for Registration {
        fn drop(&mut self) {
            // SAFETY: both refs were produced by the matching register/install
            // calls and are unregistered exactly once here; the boxed handler
            // data is freed only after the handler that reads it is removed.
            unsafe {
                UnregisterEventHotKey(self.hotkey_ref);
                RemoveEventHandler(self.handler_ref);
                drop(Box::from_raw(self.data));
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
