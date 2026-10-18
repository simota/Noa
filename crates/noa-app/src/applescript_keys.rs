//! AppleScript `send key` vocabulary (agent-automation REQ-010, spec
//! Appendix A): Ghostty's `Ghostty.Input.Key` raw names mapped to the macOS
//! virtual keycode used for layout translation, the winit physical
//! [`KeyCode`], and — for non-character keys — the winit [`NamedKey`] the
//! encoder sees for the same physical press.
//!
//! Pure and platform-independent: the macOS layout translation is injected as
//! a closure so the mapping is unit-testable without a keyboard.

use winit::keyboard::{Key, KeyCode, ModifiersState, NamedKey, SmolStr};

/// One `send key` name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ScriptKeyDef {
    pub(crate) name: &'static str,
    /// macOS virtual keycode, or `None` when the Mac keyboard has no such key
    /// (spec Appendix A `-`): such keys encode from the [`KeyCode`] alone.
    pub(crate) mac: Option<u16>,
    pub(crate) code: KeyCode,
    /// The logical key a physical press reports. `None` for character keys,
    /// whose logical key is the layout-translated text.
    pub(crate) named: Option<NamedKey>,
}

const fn ch(name: &'static str, mac: u16, code: KeyCode) -> ScriptKeyDef {
    ScriptKeyDef {
        name,
        mac: Some(mac),
        code,
        named: None,
    }
}

const fn nk(name: &'static str, mac: Option<u16>, code: KeyCode, named: NamedKey) -> ScriptKeyDef {
    ScriptKeyDef {
        name,
        mac,
        code,
        named: Some(named),
    }
}

/// A character-class key with no Mac keycode: no layout to translate through
/// and no named key, so it encodes from its physical code only.
const fn bare(name: &'static str, code: KeyCode) -> ScriptKeyDef {
    ScriptKeyDef {
        name,
        mac: None,
        code,
        named: None,
    }
}

use KeyCode as C;
use NamedKey as N;

/// Spec Appendix A, in Ghostty's declaration order.
pub(crate) const KEYS: &[ScriptKeyDef] = &[
    ch("backquote", 0x32, C::Backquote),
    ch("backslash", 0x2a, C::Backslash),
    ch("bracketLeft", 0x21, C::BracketLeft),
    ch("bracketRight", 0x1e, C::BracketRight),
    ch("comma", 0x2b, C::Comma),
    ch("digit0", 0x1d, C::Digit0),
    ch("digit1", 0x12, C::Digit1),
    ch("digit2", 0x13, C::Digit2),
    ch("digit3", 0x14, C::Digit3),
    ch("digit4", 0x15, C::Digit4),
    ch("digit5", 0x17, C::Digit5),
    ch("digit6", 0x16, C::Digit6),
    ch("digit7", 0x1a, C::Digit7),
    ch("digit8", 0x1c, C::Digit8),
    ch("digit9", 0x19, C::Digit9),
    ch("equal", 0x18, C::Equal),
    ch("intlBackslash", 0x0a, C::IntlBackslash),
    ch("intlRo", 0x5e, C::IntlRo),
    ch("intlYen", 0x5d, C::IntlYen),
    ch("a", 0x00, C::KeyA),
    ch("b", 0x0b, C::KeyB),
    ch("c", 0x08, C::KeyC),
    ch("d", 0x02, C::KeyD),
    ch("e", 0x0e, C::KeyE),
    ch("f", 0x03, C::KeyF),
    ch("g", 0x05, C::KeyG),
    ch("h", 0x04, C::KeyH),
    ch("i", 0x22, C::KeyI),
    ch("j", 0x26, C::KeyJ),
    ch("k", 0x28, C::KeyK),
    ch("l", 0x25, C::KeyL),
    ch("m", 0x2e, C::KeyM),
    ch("n", 0x2d, C::KeyN),
    ch("o", 0x1f, C::KeyO),
    ch("p", 0x23, C::KeyP),
    ch("q", 0x0c, C::KeyQ),
    ch("r", 0x0f, C::KeyR),
    ch("s", 0x01, C::KeyS),
    ch("t", 0x11, C::KeyT),
    ch("u", 0x20, C::KeyU),
    ch("v", 0x09, C::KeyV),
    ch("w", 0x0d, C::KeyW),
    ch("x", 0x07, C::KeyX),
    ch("y", 0x10, C::KeyY),
    ch("z", 0x06, C::KeyZ),
    ch("minus", 0x1b, C::Minus),
    ch("period", 0x2f, C::Period),
    ch("quote", 0x27, C::Quote),
    ch("semicolon", 0x29, C::Semicolon),
    ch("slash", 0x2c, C::Slash),
    nk("altLeft", Some(0x3a), C::AltLeft, N::Alt),
    nk("altRight", Some(0x3d), C::AltRight, N::Alt),
    nk("backspace", Some(0x33), C::Backspace, N::Backspace),
    nk("capsLock", Some(0x39), C::CapsLock, N::CapsLock),
    nk("contextMenu", Some(0x6e), C::ContextMenu, N::ContextMenu),
    nk("controlLeft", Some(0x3b), C::ControlLeft, N::Control),
    nk("controlRight", Some(0x3e), C::ControlRight, N::Control),
    nk("enter", Some(0x24), C::Enter, N::Enter),
    nk("metaLeft", Some(0x37), C::SuperLeft, N::Super),
    nk("metaRight", Some(0x36), C::SuperRight, N::Super),
    nk("shiftLeft", Some(0x38), C::ShiftLeft, N::Shift),
    nk("shiftRight", Some(0x3c), C::ShiftRight, N::Shift),
    nk("space", Some(0x31), C::Space, N::Space),
    nk("tab", Some(0x30), C::Tab, N::Tab),
    nk("convert", None, C::Convert, N::Convert),
    nk("kanaMode", None, C::KanaMode, N::KanaMode),
    nk("nonConvert", None, C::NonConvert, N::NonConvert),
    nk("delete", Some(0x75), C::Delete, N::Delete),
    nk("end", Some(0x77), C::End, N::End),
    nk("help", None, C::Help, N::Help),
    nk("home", Some(0x73), C::Home, N::Home),
    nk("insert", Some(0x72), C::Insert, N::Insert),
    nk("pageDown", Some(0x79), C::PageDown, N::PageDown),
    nk("pageUp", Some(0x74), C::PageUp, N::PageUp),
    nk("arrowDown", Some(0x7d), C::ArrowDown, N::ArrowDown),
    nk("arrowLeft", Some(0x7b), C::ArrowLeft, N::ArrowLeft),
    nk("arrowRight", Some(0x7c), C::ArrowRight, N::ArrowRight),
    nk("arrowUp", Some(0x7e), C::ArrowUp, N::ArrowUp),
    nk("numLock", Some(0x47), C::NumLock, N::NumLock),
    ch("numpad0", 0x52, C::Numpad0),
    ch("numpad1", 0x53, C::Numpad1),
    ch("numpad2", 0x54, C::Numpad2),
    ch("numpad3", 0x55, C::Numpad3),
    ch("numpad4", 0x56, C::Numpad4),
    ch("numpad5", 0x57, C::Numpad5),
    ch("numpad6", 0x58, C::Numpad6),
    ch("numpad7", 0x59, C::Numpad7),
    ch("numpad8", 0x5b, C::Numpad8),
    ch("numpad9", 0x5c, C::Numpad9),
    ch("numpadAdd", 0x45, C::NumpadAdd),
    nk("numpadBackspace", None, C::NumpadBackspace, N::Backspace),
    nk("numpadClear", None, C::NumpadClear, N::Clear),
    nk("numpadClearEntry", None, C::NumpadClearEntry, N::Clear),
    ch("numpadComma", 0x5f, C::NumpadComma),
    ch("numpadDecimal", 0x41, C::NumpadDecimal),
    ch("numpadDivide", 0x4b, C::NumpadDivide),
    nk("numpadEnter", Some(0x4c), C::NumpadEnter, N::Enter),
    ch("numpadEqual", 0x51, C::NumpadEqual),
    bare("numpadMemoryAdd", C::NumpadMemoryAdd),
    bare("numpadMemoryClear", C::NumpadMemoryClear),
    bare("numpadMemoryRecall", C::NumpadMemoryRecall),
    bare("numpadMemoryStore", C::NumpadMemoryStore),
    bare("numpadMemorySubtract", C::NumpadMemorySubtract),
    ch("numpadMultiply", 0x43, C::NumpadMultiply),
    bare("numpadParenLeft", C::NumpadParenLeft),
    bare("numpadParenRight", C::NumpadParenRight),
    ch("numpadSubtract", 0x4e, C::NumpadSubtract),
    bare("numpadSeparator", C::NumpadComma),
    // The numlock-off numpad navigation keys: winit has no dedicated codes,
    // so they report the digit key's physical code with the navigation
    // logical key — what a numlock-off keypad produces.
    nk("numpadUp", None, C::Numpad8, N::ArrowUp),
    nk("numpadDown", None, C::Numpad2, N::ArrowDown),
    nk("numpadRight", None, C::Numpad6, N::ArrowRight),
    nk("numpadLeft", None, C::Numpad4, N::ArrowLeft),
    nk("numpadBegin", None, C::Numpad5, N::Clear),
    nk("numpadHome", None, C::Numpad7, N::Home),
    nk("numpadEnd", None, C::Numpad1, N::End),
    nk("numpadInsert", None, C::Numpad0, N::Insert),
    nk("numpadDelete", None, C::NumpadDecimal, N::Delete),
    nk("numpadPageUp", None, C::Numpad9, N::PageUp),
    nk("numpadPageDown", None, C::Numpad3, N::PageDown),
    nk("escape", Some(0x35), C::Escape, N::Escape),
    nk("f1", Some(0x7a), C::F1, N::F1),
    nk("f2", Some(0x78), C::F2, N::F2),
    nk("f3", Some(0x63), C::F3, N::F3),
    nk("f4", Some(0x76), C::F4, N::F4),
    nk("f5", Some(0x60), C::F5, N::F5),
    nk("f6", Some(0x61), C::F6, N::F6),
    nk("f7", Some(0x62), C::F7, N::F7),
    nk("f8", Some(0x64), C::F8, N::F8),
    nk("f9", Some(0x65), C::F9, N::F9),
    nk("f10", Some(0x6d), C::F10, N::F10),
    nk("f11", Some(0x67), C::F11, N::F11),
    nk("f12", Some(0x6f), C::F12, N::F12),
    nk("f13", Some(0x69), C::F13, N::F13),
    nk("f14", Some(0x6b), C::F14, N::F14),
    nk("f15", Some(0x71), C::F15, N::F15),
    nk("f16", Some(0x6a), C::F16, N::F16),
    nk("f17", Some(0x40), C::F17, N::F17),
    nk("f18", Some(0x4f), C::F18, N::F18),
    nk("f19", Some(0x50), C::F19, N::F19),
    nk("f20", Some(0x5a), C::F20, N::F20),
    nk("f21", None, C::F21, N::F21),
    nk("f22", None, C::F22, N::F22),
    nk("f23", None, C::F23, N::F23),
    nk("f24", None, C::F24, N::F24),
    nk("f25", None, C::F25, N::F25),
    nk("fn", None, C::Fn, N::Fn),
    nk("fnLock", None, C::FnLock, N::FnLock),
    nk("printScreen", None, C::PrintScreen, N::PrintScreen),
    nk("scrollLock", None, C::ScrollLock, N::ScrollLock),
    nk("pause", None, C::Pause, N::Pause),
    nk("browserBack", None, C::BrowserBack, N::BrowserBack),
    nk(
        "browserFavorites",
        None,
        C::BrowserFavorites,
        N::BrowserFavorites,
    ),
    nk("browserForward", None, C::BrowserForward, N::BrowserForward),
    nk("browserHome", None, C::BrowserHome, N::BrowserHome),
    nk("browserRefresh", None, C::BrowserRefresh, N::BrowserRefresh),
    nk("browserSearch", None, C::BrowserSearch, N::BrowserSearch),
    nk("browserStop", None, C::BrowserStop, N::BrowserStop),
    nk("eject", None, C::Eject, N::Eject),
    nk("launchApp1", None, C::LaunchApp1, N::LaunchApplication1),
    nk("launchApp2", None, C::LaunchApp2, N::LaunchApplication2),
    nk("launchMail", None, C::LaunchMail, N::LaunchMail),
    nk("mediaPlayPause", None, C::MediaPlayPause, N::MediaPlayPause),
    nk("mediaSelect", None, C::MediaSelect, N::LaunchMediaPlayer),
    nk("mediaStop", None, C::MediaStop, N::MediaStop),
    nk("mediaTrackNext", None, C::MediaTrackNext, N::MediaTrackNext),
    nk(
        "mediaTrackPrevious",
        None,
        C::MediaTrackPrevious,
        N::MediaTrackPrevious,
    ),
    nk("power", None, C::Power, N::Power),
    nk("sleep", None, C::Sleep, N::Standby),
    nk(
        "audioVolumeDown",
        Some(0x49),
        C::AudioVolumeDown,
        N::AudioVolumeDown,
    ),
    nk(
        "audioVolumeMute",
        Some(0x4a),
        C::AudioVolumeMute,
        N::AudioVolumeMute,
    ),
    nk(
        "audioVolumeUp",
        Some(0x48),
        C::AudioVolumeUp,
        N::AudioVolumeUp,
    ),
    nk("wakeUp", None, C::WakeUp, N::WakeUp),
    nk("copy", None, C::Copy, N::Copy),
    nk("cut", None, C::Cut, N::Cut),
    nk("paste", None, C::Paste, N::Paste),
];

/// Look up a `send key` name (case-sensitive, spec L2.4).
pub(crate) fn lookup(name: &str) -> Option<&'static ScriptKeyDef> {
    KEYS.iter().find(|def| def.name == name)
}

/// Parse a `modifiers` string: comma-separated, whitespace-trimmed,
/// case-insensitive `shift`/`control`/`option`/`command` (spec L2.4). `None`
/// on any other token. An empty string means no modifiers.
pub(crate) fn parse_modifiers(spec: &str) -> Option<ModifiersState> {
    let mut mods = ModifiersState::empty();
    if spec.trim().is_empty() {
        return Some(mods);
    }
    for part in spec.split(',') {
        match part.trim().to_ascii_lowercase().as_str() {
            "shift" => mods |= ModifiersState::SHIFT,
            "control" => mods |= ModifiersState::CONTROL,
            "option" => mods |= ModifiersState::ALT,
            "command" => mods |= ModifiersState::SUPER,
            _ => return None,
        }
    }
    Some(mods)
}

/// The key event a `send key` resolves to, in the shape winit hands the
/// physical-press encoder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ResolvedKey {
    pub(crate) logical: Key,
    pub(crate) unmodified: Key,
    pub(crate) physical: KeyCode,
    pub(crate) text: Option<String>,
    /// Whether an Option press ESC-prefixes (Option claimed as Alt) rather
    /// than composing a character through the layout.
    pub(crate) alt_sends_esc: bool,
}

/// Resolve `def` under `mods`, translating character keys once through the
/// current layout (`translate(mac_keycode, shift, option) -> text`), the way a
/// physical press is translated. Option participates in translation unless
/// `option_as_alt` claims it, in which case it ESC-prefixes instead.
pub(crate) fn resolve_key(
    def: &ScriptKeyDef,
    mods: ModifiersState,
    option_as_alt: bool,
    translate: impl Fn(u16, bool, bool) -> Option<String>,
) -> ResolvedKey {
    let shift = mods.shift_key();
    let option = mods.alt_key();
    let alt_sends_esc = !option || option_as_alt;
    let translate_option = option && !option_as_alt;
    // Space produces text through the layout like a character key (U.S.
    // Option+Space is U+00A0); only a plain space reports the named key.
    if def.named == Some(N::Space)
        && let Some(mac) = def.mac
    {
        let text = translate(mac, shift, translate_option)
            .filter(|text| !text.is_empty())
            .unwrap_or_else(|| " ".to_string());
        let logical = if text == " " {
            Key::Named(N::Space)
        } else {
            Key::Character(SmolStr::new(&text))
        };
        return ResolvedKey {
            logical,
            unmodified: Key::Named(N::Space),
            physical: def.code,
            text: Some(text),
            alt_sends_esc,
        };
    }
    if let Some(named) = def.named {
        let key = Key::Named(named);
        let text = match named {
            N::Enter => Some("\r".to_string()),
            N::Tab => Some("\t".to_string()),
            N::Backspace => Some("\u{7f}".to_string()),
            N::Escape => Some("\u{1b}".to_string()),
            _ => None,
        };
        return ResolvedKey {
            logical: key.clone(),
            unmodified: key,
            physical: def.code,
            text,
            alt_sends_esc,
        };
    }
    let Some(mac) = def.mac else {
        return ResolvedKey {
            logical: Key::Unidentified(winit::keyboard::NativeKey::Unidentified),
            unmodified: Key::Unidentified(winit::keyboard::NativeKey::Unidentified),
            physical: def.code,
            text: None,
            alt_sends_esc,
        };
    };
    let text = translate(mac, shift, translate_option).filter(|text| !text.is_empty());
    let unmodified = translate(mac, false, false).filter(|text| !text.is_empty());
    let as_key = |text: &Option<String>| match text {
        Some(text) => Key::Character(SmolStr::new(text)),
        None => Key::Unidentified(winit::keyboard::NativeKey::Unidentified),
    };
    ResolvedKey {
        logical: as_key(&text),
        unmodified: as_key(&unmodified),
        physical: def.code,
        text,
        alt_sends_esc,
    }
}

/// Translate a macOS virtual keycode through the current keyboard layout
/// (spec L2.4): the current input source's `kTISPropertyUnicodeKeyLayoutData`,
/// falling back to the current ASCII-capable source when the current one
/// (e.g. an IME) has none. Dead keys are not composed. `None` when no layout
/// data is available or the key produces nothing.
#[cfg(target_os = "macos")]
pub(crate) fn translate_with_current_layout(
    keycode: u16,
    shift: bool,
    option: bool,
) -> Option<String> {
    mac_layout::translate(None, keycode, shift, option)
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn translate_with_current_layout(
    _keycode: u16,
    _shift: bool,
    _option: bool,
) -> Option<String> {
    None
}

#[cfg(target_os = "macos")]
pub(crate) mod mac_layout {
    use core::ffi::c_void;

    type CFTypeRef = *const c_void;
    type CFStringRef = *const c_void;
    type CFDataRef = *const c_void;
    type CFArrayRef = *const c_void;
    type CFDictionaryRef = *const c_void;
    type TISInputSourceRef = *const c_void;
    type OSStatus = i32;

    #[link(name = "Carbon", kind = "framework")]
    unsafe extern "C" {
        static kTISPropertyUnicodeKeyLayoutData: CFStringRef;
        static kTISPropertyInputSourceID: CFStringRef;
        fn TISCopyCurrentKeyboardLayoutInputSource() -> TISInputSourceRef;
        fn TISCopyCurrentASCIICapableKeyboardLayoutInputSource() -> TISInputSourceRef;
        fn TISCreateInputSourceList(properties: CFDictionaryRef, include_all: u8) -> CFArrayRef;
        fn TISGetInputSourceProperty(source: TISInputSourceRef, key: CFStringRef) -> CFTypeRef;
        fn LMGetKbdType() -> u8;
        fn UCKeyTranslate(
            layout: *const c_void,
            virtual_key_code: u16,
            key_action: u16,
            modifier_key_state: u32,
            keyboard_type: u32,
            key_translate_options: u32,
            dead_key_state: *mut u32,
            max_string_length: usize,
            actual_string_length: *mut usize,
            unicode_string: *mut u16,
        ) -> OSStatus;
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        static kCFTypeDictionaryKeyCallBacks: c_void;
        static kCFTypeDictionaryValueCallBacks: c_void;
        fn CFRelease(cf: CFTypeRef);
        fn CFDataGetBytePtr(data: CFDataRef) -> *const u8;
        fn CFArrayGetCount(array: CFArrayRef) -> isize;
        fn CFArrayGetValueAtIndex(array: CFArrayRef, idx: isize) -> CFTypeRef;
        fn CFDictionaryCreate(
            allocator: CFTypeRef,
            keys: *const CFTypeRef,
            values: *const CFTypeRef,
            num_values: isize,
            key_callbacks: *const c_void,
            value_callbacks: *const c_void,
        ) -> CFDictionaryRef;
        fn CFStringCreateWithBytes(
            allocator: CFTypeRef,
            bytes: *const u8,
            num_bytes: isize,
            encoding: u32,
            is_external: u8,
        ) -> CFStringRef;
    }

    const K_UC_KEY_ACTION_DOWN: u16 = 0;
    const K_UC_KEY_TRANSLATE_NO_DEAD_KEYS_MASK: u32 = 1;
    const K_CF_STRING_ENCODING_UTF8: u32 = 0x0800_0100;
    /// Carbon `shiftKey`/`optionKey` (`Events.h`) shifted into
    /// `UCKeyTranslate`'s `modifierKeyState` byte.
    const SHIFT: u32 = 0x0200 >> 8;
    const OPTION: u32 = 0x0800 >> 8;

    /// Translate through the input source with id `source_id` (tests pin
    /// `com.apple.keylayout.US`), or the current layout when `None`.
    pub(crate) fn translate(
        source_id: Option<&str>,
        keycode: u16,
        shift: bool,
        option: bool,
    ) -> Option<String> {
        // SAFETY: plain HIToolbox/CoreFoundation calls; every returned
        // object is nil-checked and released exactly once (the Copy/Create
        // rule), and the layout bytes are only read while their owning
        // source is retained.
        unsafe {
            let (source, list) = match source_id {
                Some(id) => {
                    let list = source_list_with_id(id)?;
                    if CFArrayGetCount(list) < 1 {
                        CFRelease(list);
                        return None;
                    }
                    (CFArrayGetValueAtIndex(list, 0), Some(list))
                }
                None => {
                    let mut source = TISCopyCurrentKeyboardLayoutInputSource();
                    if source.is_null()
                        || TISGetInputSourceProperty(source, kTISPropertyUnicodeKeyLayoutData)
                            .is_null()
                    {
                        if !source.is_null() {
                            CFRelease(source);
                        }
                        source = TISCopyCurrentASCIICapableKeyboardLayoutInputSource();
                    }
                    if source.is_null() {
                        return None;
                    }
                    (source, None)
                }
            };
            let result = translate_with_source(source, keycode, shift, option);
            match list {
                Some(list) => CFRelease(list),
                None => CFRelease(source),
            }
            result
        }
    }

    unsafe fn source_list_with_id(id: &str) -> Option<CFArrayRef> {
        unsafe {
            let value = CFStringCreateWithBytes(
                std::ptr::null(),
                id.as_ptr(),
                id.len() as isize,
                K_CF_STRING_ENCODING_UTF8,
                0,
            );
            if value.is_null() {
                return None;
            }
            let keys = [kTISPropertyInputSourceID];
            let values = [value];
            let dict = CFDictionaryCreate(
                std::ptr::null(),
                keys.as_ptr(),
                values.as_ptr(),
                1,
                &kCFTypeDictionaryKeyCallBacks,
                &kCFTypeDictionaryValueCallBacks,
            );
            CFRelease(value);
            if dict.is_null() {
                return None;
            }
            let list = TISCreateInputSourceList(dict, 1);
            CFRelease(dict);
            (!list.is_null()).then_some(list)
        }
    }

    unsafe fn translate_with_source(
        source: TISInputSourceRef,
        keycode: u16,
        shift: bool,
        option: bool,
    ) -> Option<String> {
        unsafe {
            let data = TISGetInputSourceProperty(source, kTISPropertyUnicodeKeyLayoutData);
            if data.is_null() {
                return None;
            }
            let layout = CFDataGetBytePtr(data);
            if layout.is_null() {
                return None;
            }
            let mut modifiers = 0;
            if shift {
                modifiers |= SHIFT;
            }
            if option {
                modifiers |= OPTION;
            }
            let mut dead_key_state = 0u32;
            let mut chars = [0u16; 8];
            let mut len = 0usize;
            let status = UCKeyTranslate(
                layout.cast(),
                keycode,
                K_UC_KEY_ACTION_DOWN,
                modifiers,
                u32::from(LMGetKbdType()),
                K_UC_KEY_TRANSLATE_NO_DEAD_KEYS_MASK,
                &mut dead_key_state,
                chars.len(),
                &mut len,
                chars.as_mut_ptr(),
            );
            if status != 0 || len == 0 {
                return None;
            }
            String::from_utf16(&chars[..len.min(chars.len())]).ok()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn appendix_a_names_resolve_to_their_keycodes() {
        let enter = lookup("enter").expect("enter");
        assert_eq!(
            (enter.mac, enter.code, enter.named),
            (Some(0x24), KeyCode::Enter, Some(NamedKey::Enter))
        );
        let up = lookup("arrowUp").expect("arrowUp");
        assert_eq!((up.mac, up.code), (Some(0x7e), KeyCode::ArrowUp));
        let a = lookup("a").expect("a");
        assert_eq!((a.mac, a.code, a.named), (Some(0x00), KeyCode::KeyA, None));
        let one = lookup("digit1").expect("digit1");
        assert_eq!((one.mac, one.code), (Some(0x12), KeyCode::Digit1));
        let f21 = lookup("f21").expect("f21");
        assert_eq!((f21.mac, f21.code), (None, KeyCode::F21));
        assert!(lookup("foo").is_none());
        // Case-sensitive (spec L2.4).
        assert!(lookup("Enter").is_none());
    }

    #[test]
    fn every_appendix_name_is_unique() {
        let mut names: Vec<&str> = KEYS.iter().map(|def| def.name).collect();
        names.sort_unstable();
        let before = names.len();
        names.dedup();
        assert_eq!(before, names.len());
        // Spec Appendix A (Ghostty's `Ghostty.Input.Key`) lists 175 names.
        assert_eq!(before, 175);
    }

    #[test]
    fn modifier_strings_parse_case_insensitively() {
        assert_eq!(
            parse_modifiers("shift, Control"),
            Some(ModifiersState::SHIFT | ModifiersState::CONTROL)
        );
        assert_eq!(
            parse_modifiers("option,command"),
            Some(ModifiersState::ALT | ModifiersState::SUPER)
        );
        assert_eq!(parse_modifiers(""), Some(ModifiersState::empty()));
        assert_eq!(parse_modifiers("hyper"), None);
        assert_eq!(parse_modifiers("shift,"), None);
    }

    #[test]
    fn character_keys_translate_through_the_layout_once() {
        let semicolon = lookup("semicolon").expect("semicolon");
        let fake = |code: u16, shift: bool, _option: bool| {
            (code == 0x29).then(|| if shift { ":" } else { ";" }.to_string())
        };
        let key = resolve_key(semicolon, ModifiersState::SHIFT, false, fake);
        assert_eq!(key.logical, Key::Character(SmolStr::new(":")));
        assert_eq!(key.unmodified, Key::Character(SmolStr::new(";")));
        assert_eq!(key.text.as_deref(), Some(":"));
        assert_eq!(key.physical, KeyCode::Semicolon);
    }

    #[test]
    fn option_composes_unless_claimed_as_alt() {
        let e = lookup("e").expect("e");
        let fake =
            |_: u16, _shift: bool, option: bool| Some(if option { "´" } else { "e" }.to_string());
        let composed = resolve_key(e, ModifiersState::ALT, false, fake);
        assert_eq!(composed.text.as_deref(), Some("´"));
        assert!(!composed.alt_sends_esc);
        let alt = resolve_key(e, ModifiersState::ALT, true, fake);
        assert_eq!(alt.text.as_deref(), Some("e"));
        assert!(alt.alt_sends_esc);
    }

    #[test]
    fn space_translates_through_the_layout() {
        let space = lookup("space").expect("space");
        let fake = |code: u16, _shift: bool, option: bool| {
            (code == 0x31).then(|| if option { "\u{a0}" } else { " " }.to_string())
        };
        let plain = resolve_key(space, ModifiersState::empty(), false, fake);
        assert_eq!(plain.logical, Key::Named(NamedKey::Space));
        assert_eq!(plain.text.as_deref(), Some(" "));
        // Option composes a no-break space unless Option is claimed as Alt.
        let composed = resolve_key(space, ModifiersState::ALT, false, fake);
        assert_eq!(composed.logical, Key::Character(SmolStr::new("\u{a0}")));
        assert_eq!(composed.text.as_deref(), Some("\u{a0}"));
        let alt = resolve_key(space, ModifiersState::ALT, true, fake);
        assert_eq!(alt.logical, Key::Named(NamedKey::Space));
        assert!(alt.alt_sends_esc);
        // No layout data: still a space.
        let bare = resolve_key(space, ModifiersState::ALT, false, |_, _, _| None);
        assert_eq!(bare.text.as_deref(), Some(" "));
    }

    #[test]
    fn named_keys_skip_translation() {
        let enter = lookup("enter").expect("enter");
        let key = resolve_key(enter, ModifiersState::empty(), false, |_, _, _| {
            panic!("named keys must not be layout-translated")
        });
        assert_eq!(key.logical, Key::Named(NamedKey::Enter));
        assert_eq!(key.text.as_deref(), Some("\r"));
    }

    fn encode(
        name: &str,
        mods: ModifiersState,
        app_cursor_keys: bool,
        kitty_flags: u8,
        pressed: bool,
    ) -> Option<Vec<u8>> {
        let def = lookup(name).expect("known key");
        let key = resolve_key(def, mods, false, |code, shift, _| {
            // Enough of a U.S. layout for these cases.
            match code {
                0x00 => Some(if shift { "A" } else { "a" }.to_string()),
                0x08 => Some(if shift { "C" } else { "c" }.to_string()),
                _ => None,
            }
        });
        crate::input::encode_key_with_modes(
            &key.logical,
            Some(&key.unmodified),
            Some(winit::keyboard::PhysicalKey::Code(key.physical)),
            key.text.as_deref(),
            mods,
            key.alt_sends_esc,
            app_cursor_keys,
            false,
            kitty_flags,
            false,
            pressed,
            false,
        )
    }

    /// AC-012: scripted keys go through the physical-press encoder, so the
    /// pane's modes shape the bytes exactly as they would for a real key.
    #[test]
    fn applescript_send_key_encodes_like_a_physical_press() {
        assert_eq!(
            encode("enter", ModifiersState::empty(), false, 0, true).as_deref(),
            Some(&b"\r"[..])
        );
        assert_eq!(
            encode("arrowUp", ModifiersState::empty(), false, 0, true).as_deref(),
            Some(&b"\x1b[A"[..])
        );
        // DECCKM switches arrows to SS3.
        assert_eq!(
            encode("arrowUp", ModifiersState::empty(), true, 0, true).as_deref(),
            Some(&b"\x1bOA"[..])
        );
        assert_eq!(
            encode("c", ModifiersState::CONTROL, false, 0, true).as_deref(),
            Some(&b"\x03"[..])
        );
        assert_eq!(
            encode("a", ModifiersState::SHIFT, false, 0, true).as_deref(),
            Some(&b"A"[..])
        );
        // A release sends nothing in legacy mode …
        assert!(
            encode("a", ModifiersState::empty(), false, 0, false)
                .is_none_or(|bytes| bytes.is_empty())
        );
        // … and a Kitty release event once report-event-types (0b10) is on
        // (with report-all-keys, 0b1000, since `a` produces text).
        let release = encode("a", ModifiersState::empty(), false, 0b1011, false).expect("release");
        let release = String::from_utf8(release).expect("ascii");
        assert!(
            release.starts_with("\x1b[97;") && release.ends_with(":3u"),
            "{release:?}"
        );
    }

    /// AC-012: translation through the real U.S. layout data, plus the
    /// `CFArrayGetCount(list) < 1` guard: an id that matches no input source
    /// (unknown, empty) yields no translation. One match is the U.S. case;
    /// ids are unique, so a list of two cannot be built.
    ///
    /// One test on purpose: HIToolbox's input-source calls are not safe to
    /// make from two threads at once (parallel tests abort), and the app only
    /// ever makes them on the main thread.
    #[cfg(target_os = "macos")]
    #[test]
    fn layout_translation_through_real_input_sources() {
        let translated = mac_layout::translate(Some("com.apple.keylayout.US"), 0x29, true, false);
        assert_eq!(translated.as_deref(), Some(":"));
        let plain = mac_layout::translate(Some("com.apple.keylayout.US"), 0x00, false, false);
        assert_eq!(plain.as_deref(), Some("a"));
        assert_eq!(
            mac_layout::translate(Some("com.example.no-such-layout"), 0x00, false, false),
            None
        );
        assert_eq!(mac_layout::translate(Some(""), 0x00, false, false), None);
        // U.S. Option+Space is a no-break space.
        assert_eq!(
            mac_layout::translate(Some("com.apple.keylayout.US"), 0x31, false, true).as_deref(),
            Some("\u{a0}")
        );
    }
}
