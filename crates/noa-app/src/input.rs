//! Keyboard-event -> pty-byte encoding.
//!
//! `winit::event::KeyEvent` has a private platform-specific field, so it
//! can't be constructed in tests; [`encode_key`] takes the pieces we need
//! (`logical_key`, `text`, modifiers) directly so the encoding logic stays
//! unit-testable without a live `KeyEvent`.

use noa_grid::{
    KITTY_REPORT_ALL_KEYS, KITTY_REPORT_ALTERNATE_KEYS, KITTY_REPORT_ASSOCIATED_TEXT,
    KITTY_REPORT_EVENT_TYPES,
};
use winit::event::Ime;
use winit::keyboard::{Key, KeyCode, ModifiersState, NamedKey, PhysicalKey};

/// Tracks IME composition state and encodes committed IME text for the pty.
#[derive(Debug, Default)]
pub struct ImeState {
    preedit_active: bool,
}

impl ImeState {
    pub fn handle_event(&mut self, event: &Ime) -> Option<Vec<u8>> {
        match event {
            Ime::Enabled | Ime::Disabled => {
                self.preedit_active = false;
                None
            }
            Ime::Preedit(text, _cursor_range) => {
                self.preedit_active = !text.is_empty();
                None
            }
            Ime::Commit(text) => {
                self.preedit_active = false;
                encode_text(text)
            }
        }
    }

    pub fn preedit_active(&self) -> bool {
        self.preedit_active
    }

    pub fn commit_preedit(&mut self) {
        self.preedit_active = false;
    }
}

/// Encode a pressed key into the bytes that should be written to the pty, if
/// any. `app_cursor_keys` mirrors `ModeState::app_cursor_keys()` (DECCKM):
/// when set, arrow keys send `SS3` (`ESC O <letter>`) instead of `CSI`
/// (`ESC [ <letter>`).
#[cfg(test)]
pub fn encode_key(
    logical_key: &Key,
    text: Option<&str>,
    mods: ModifiersState,
    app_cursor_keys: bool,
) -> Option<Vec<u8>> {
    encode_key_with_modes(
        logical_key,
        None,
        text,
        mods,
        app_cursor_keys,
        false,
        0,
        true,
        false,
    )
}

/// Encode a key event for the pty. `kitty_flags` are the active Kitty keyboard
/// progressive-enhancement flags (`Terminal::kitty_keyboard_flags`); `0`
/// selects the legacy encoding and every existing behavior is preserved
/// unchanged. `pressed`/`repeat` come from the winit `KeyEvent` and only affect
/// the Kitty path.
#[allow(clippy::too_many_arguments)]
pub fn encode_key_with_modes(
    logical_key: &Key,
    physical_key: Option<PhysicalKey>,
    text: Option<&str>,
    mods: ModifiersState,
    app_cursor_keys: bool,
    app_keypad: bool,
    kitty_flags: u8,
    pressed: bool,
    repeat: bool,
) -> Option<Vec<u8>> {
    // Kitty keyboard protocol: when any progressive-enhancement flag is active
    // it fully governs encoding. Keys that stay legacy under the active flags
    // (bare printables, unmodified Enter/Tab/Backspace) fall through to the
    // legacy path below; released legacy keys are dropped.
    if kitty_flags != 0 {
        match encode_kitty(
            logical_key,
            physical_key,
            text,
            mods,
            kitty_flags,
            pressed,
            repeat,
        ) {
            KittyOutcome::Escape(bytes) => return Some(bytes),
            KittyOutcome::Ignore => return None,
            KittyOutcome::Legacy => {}
        }
    }

    // Ctrl+letter -> the corresponding C0 control byte. Checked before the
    // general text path since terminals expect Ctrl+A..Z to send 0x01..0x1a
    // regardless of what `text` the platform layer produced.
    if mods.control_key()
        && let Key::Character(s) = logical_key
    {
        let mut chars = s.chars();
        if let (Some(c), None) = (chars.next(), chars.next()) {
            let lower = c.to_ascii_lowercase();
            if lower.is_ascii_lowercase() {
                let byte = (lower as u8) - b'a' + 1; // Ctrl+A=0x01 .. Ctrl+Z=0x1a
                return Some(vec![byte]);
            }
        }
    }

    if app_keypad
        && modifier_value(mods).is_none()
        && let Some(bytes) = application_keypad_bytes(physical_key)
    {
        return Some(bytes);
    }

    match logical_key {
        Key::Named(NamedKey::Enter) => Some(vec![0x0d]),
        Key::Named(NamedKey::Backspace) => Some(vec![0x7f]),
        Key::Named(NamedKey::Tab) => Some(vec![b'\t']),
        Key::Named(NamedKey::Escape) => Some(vec![0x1b]),
        Key::Named(named) => {
            special_key_bytes(*named, mods, app_cursor_keys).or_else(|| encode_key_text(text, mods))
        }
        _ => encode_key_text(text, mods),
    }
}

fn application_keypad_bytes(physical_key: Option<PhysicalKey>) -> Option<Vec<u8>> {
    let PhysicalKey::Code(code) = physical_key? else {
        return None;
    };
    let final_byte = match code {
        KeyCode::Numpad0 => b'p',
        KeyCode::Numpad1 => b'q',
        KeyCode::Numpad2 => b'r',
        KeyCode::Numpad3 => b's',
        KeyCode::Numpad4 => b't',
        KeyCode::Numpad5 => b'u',
        KeyCode::Numpad6 => b'v',
        KeyCode::Numpad7 => b'w',
        KeyCode::Numpad8 => b'x',
        KeyCode::Numpad9 => b'y',
        KeyCode::NumpadDecimal => b'n',
        KeyCode::NumpadAdd => b'k',
        KeyCode::NumpadSubtract => b'm',
        KeyCode::NumpadMultiply => b'j',
        KeyCode::NumpadDivide => b'o',
        KeyCode::NumpadEnter => b'M',
        KeyCode::NumpadEqual => b'X',
        _ => return None,
    };
    Some(vec![0x1b, b'O', final_byte])
}

/// Encode already-committed text for the pty.
pub fn encode_text(text: &str) -> Option<Vec<u8>> {
    if text.is_empty() {
        None
    } else {
        Some(text.as_bytes().to_vec())
    }
}

/// Encode pasted text for the pty. When DECSET 2004 is active, shells and
/// editors receive explicit paste boundaries and can avoid executing content
/// as if it were typed interactively.
pub fn encode_paste(text: &str, bracketed_paste: bool) -> Option<Vec<u8>> {
    if text.is_empty() {
        return None;
    }

    let payload = sanitize_paste_payload(text.as_bytes());
    if payload.is_empty() {
        return None;
    }
    if bracketed_paste {
        let mut bytes = Vec::with_capacity(payload.len() + b"\x1b[200~".len() + b"\x1b[201~".len());
        bytes.extend_from_slice(b"\x1b[200~");
        bytes.extend_from_slice(&payload);
        bytes.extend_from_slice(b"\x1b[201~");
        Some(bytes)
    } else {
        Some(payload)
    }
}

/// Whether a paste should be confirmed before being sent
/// (`clipboard-paste-protection`). A newline can submit a command line on its
/// own, so unbracketed multi-line pastes are the risk. In bracketed-paste
/// mode the receiving program frames the paste itself, so it is treated as
/// safe (Ghostty's `clipboard-paste-bracketed-safe`, on by default).
pub(crate) fn paste_is_unsafe(text: &str, bracketed_paste: bool) -> bool {
    !bracketed_paste && text.contains(['\n', '\r'])
}

fn sanitize_paste_payload(bytes: &[u8]) -> Vec<u8> {
    let mut sanitized = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i..].starts_with(b"\x1b[200~") {
            i += b"\x1b[200~".len();
        } else if bytes[i..].starts_with(b"\x1b[201~") {
            i += b"\x1b[201~".len();
        } else {
            sanitized.push(bytes[i]);
            i += 1;
        }
    }
    sanitized
}

fn encode_key_text(text: Option<&str>, mods: ModifiersState) -> Option<Vec<u8>> {
    let mut bytes = encode_text(text?)?;
    if mods.alt_key() {
        bytes.insert(0, 0x1b);
    }
    Some(bytes)
}

fn special_key_bytes(
    named: NamedKey,
    mods: ModifiersState,
    app_cursor_keys: bool,
) -> Option<Vec<u8>> {
    // Ghostty's macOS default keybinds map bare alt+left/right to the
    // readline word-motion escapes (`esc:b` / `esc:f`) instead of the xterm
    // modified-arrow sequences, regardless of DECCKM. Any extra modifier
    // falls through to the normal modified-arrow encoding below.
    if cfg!(target_os = "macos") && mods == ModifiersState::ALT {
        match named {
            NamedKey::ArrowLeft => return Some(vec![0x1b, b'b']),
            NamedKey::ArrowRight => return Some(vec![0x1b, b'f']),
            _ => {}
        }
    }

    let modifier = modifier_value(mods);

    match named {
        NamedKey::ArrowUp => Some(final_key_bytes(b'A', modifier, app_cursor_keys)),
        NamedKey::ArrowDown => Some(final_key_bytes(b'B', modifier, app_cursor_keys)),
        NamedKey::ArrowRight => Some(final_key_bytes(b'C', modifier, app_cursor_keys)),
        NamedKey::ArrowLeft => Some(final_key_bytes(b'D', modifier, app_cursor_keys)),
        NamedKey::Home => Some(final_key_bytes(b'H', modifier, false)),
        NamedKey::End => Some(final_key_bytes(b'F', modifier, false)),
        NamedKey::Insert => Some(tilde_key_bytes(2, modifier)),
        NamedKey::Delete => Some(tilde_key_bytes(3, modifier)),
        NamedKey::PageUp => Some(tilde_key_bytes(5, modifier)),
        NamedKey::PageDown => Some(tilde_key_bytes(6, modifier)),
        NamedKey::F1 => Some(final_key_bytes(b'P', modifier, true)),
        NamedKey::F2 => Some(final_key_bytes(b'Q', modifier, true)),
        NamedKey::F3 => Some(final_key_bytes(b'R', modifier, true)),
        NamedKey::F4 => Some(final_key_bytes(b'S', modifier, true)),
        NamedKey::F5 => Some(tilde_key_bytes(15, modifier)),
        NamedKey::F6 => Some(tilde_key_bytes(17, modifier)),
        NamedKey::F7 => Some(tilde_key_bytes(18, modifier)),
        NamedKey::F8 => Some(tilde_key_bytes(19, modifier)),
        NamedKey::F9 => Some(tilde_key_bytes(20, modifier)),
        NamedKey::F10 => Some(tilde_key_bytes(21, modifier)),
        NamedKey::F11 => Some(tilde_key_bytes(23, modifier)),
        NamedKey::F12 => Some(tilde_key_bytes(24, modifier)),
        _ => None,
    }
}

fn modifier_value(mods: ModifiersState) -> Option<u8> {
    let mut value = 1;
    if mods.shift_key() {
        value += 1;
    }
    if mods.alt_key() {
        value += 2;
    }
    if mods.control_key() {
        value += 4;
    }
    (value > 1).then_some(value)
}

fn final_key_bytes(final_byte: u8, modifier: Option<u8>, ss3_unmodified: bool) -> Vec<u8> {
    match modifier {
        Some(modifier) => format!("\x1b[1;{modifier}{}", final_byte as char).into_bytes(),
        None if ss3_unmodified => vec![0x1b, b'O', final_byte],
        None => vec![0x1b, b'[', final_byte],
    }
}

fn tilde_key_bytes(code: u8, modifier: Option<u8>) -> Vec<u8> {
    match modifier {
        Some(modifier) => format!("\x1b[{code};{modifier}~").into_bytes(),
        None => format!("\x1b[{code}~").into_bytes(),
    }
}

/// Outcome of the Kitty keyboard encoder.
enum KittyOutcome {
    /// A complete Kitty escape sequence to send.
    Escape(Vec<u8>),
    /// This key is sent with its legacy encoding under the active flags —
    /// delegate to the legacy path (only reached for presses).
    Legacy,
    /// Nothing to send (e.g. a released text key, or a key winit can't map).
    Ignore,
}

/// How a key maps into the Kitty CSI form once we decide to escape-encode it.
struct KittyKey {
    /// Primary unicode-key-code / functional number.
    number: u32,
    /// Final byte: `u`, `~`, or a legacy letter (`A`/`H`/`P`/…).
    suffix: u8,
    /// Shifted alternate key code (reported only under alternate-keys flag).
    shifted: Option<u32>,
}

/// Kitty modifier bitmask value: `1 + sum of active-modifier bits`. winit only
/// surfaces shift/alt/ctrl/super, so hyper/meta/caps-lock/num-lock are never set.
fn kitty_modifier_value(mods: ModifiersState) -> u32 {
    let mut value = 1;
    if mods.shift_key() {
        value += 1;
    }
    if mods.alt_key() {
        value += 2;
    }
    if mods.control_key() {
        value += 4;
    }
    if mods.super_key() {
        value += 8;
    }
    value
}

fn encode_kitty(
    logical_key: &Key,
    physical_key: Option<PhysicalKey>,
    text: Option<&str>,
    mods: ModifiersState,
    flags: u8,
    pressed: bool,
    repeat: bool,
) -> KittyOutcome {
    let report_events = flags & KITTY_REPORT_EVENT_TYPES != 0;
    let report_all = flags & KITTY_REPORT_ALL_KEYS != 0;
    let report_alt = flags & KITTY_REPORT_ALTERNATE_KEYS != 0;
    let report_text = flags & KITTY_REPORT_ASSOCIATED_TEXT != 0;

    // Release/repeat are only distinguished when event reporting is on; without
    // it, a release is not sent at all and a repeat is an ordinary press.
    if !pressed && !report_events {
        return KittyOutcome::Ignore;
    }
    let event = if !pressed {
        3 // release
    } else if repeat && report_events {
        2 // repeat
    } else {
        1 // press
    };

    let mods_value = kitty_modifier_value(mods);
    let has_non_shift = mods.control_key() || mods.alt_key() || mods.super_key();

    // Classify the key and decide whether it escape-encodes under these flags.
    let key = match logical_key {
        Key::Named(NamedKey::Escape) => KittyKey {
            number: 27,
            suffix: b'u',
            shifted: None,
        },
        Key::Named(NamedKey::Enter) => {
            if mods_value == 1 && !report_all {
                return legacy_or_ignore(event);
            }
            KittyKey {
                number: 13,
                suffix: b'u',
                shifted: None,
            }
        }
        Key::Named(NamedKey::Tab) => {
            if mods_value == 1 && !report_all {
                return legacy_or_ignore(event);
            }
            KittyKey {
                number: 9,
                suffix: b'u',
                shifted: None,
            }
        }
        Key::Named(NamedKey::Backspace) => {
            if mods_value == 1 && !report_all {
                return legacy_or_ignore(event);
            }
            KittyKey {
                number: 127,
                suffix: b'u',
                shifted: None,
            }
        }
        Key::Named(named) => match functional_key(*named) {
            // Functional keys (arrows, F-keys, Home/End/…) always escape-encode.
            Some((number, suffix)) => KittyKey {
                number,
                suffix,
                shifted: None,
            },
            // Modifier keys alone are reported only with report-all-keys.
            None => match modifier_key_code(physical_key) {
                Some(number) if report_all => KittyKey {
                    number,
                    suffix: b'u',
                    shifted: None,
                },
                _ => return KittyOutcome::Ignore,
            },
        },
        Key::Character(s) => {
            // Numpad keys get their dedicated codes only under report-all.
            if report_all && let Some(number) = keypad_key_code(physical_key) {
                KittyKey {
                    number,
                    suffix: b'u',
                    shifted: None,
                }
            } else {
                let Some((base, shifted)) = character_key_codes(s, mods) else {
                    return legacy_or_ignore(event);
                };
                // A bare (or shift-only) printable stays legacy text unless
                // report-all forces the escape form.
                if !report_all && !has_non_shift {
                    return legacy_or_ignore(event);
                }
                KittyKey {
                    number: base,
                    suffix: b'u',
                    shifted,
                }
            }
        }
        _ => return legacy_or_ignore(event),
    };

    // Associated text: only for press/repeat, only when no modifier other than
    // shift is active, and only for genuinely printable text.
    let assoc_text = if report_text && event != 3 && !has_non_shift {
        associated_text_codepoints(text)
    } else {
        None
    };

    KittyOutcome::Escape(assemble_kitty(
        &key, mods_value, event, report_alt, assoc_text,
    ))
}

/// A press falls back to the legacy encoding; a release/repeat of a legacy key
/// is not reported.
fn legacy_or_ignore(event: u8) -> KittyOutcome {
    if event == 1 {
        KittyOutcome::Legacy
    } else {
        KittyOutcome::Ignore
    }
}

/// Assemble `CSI number[:shifted] [; mods[:event]] [; text] suffix`.
fn assemble_kitty(
    key: &KittyKey,
    mods_value: u32,
    event: u8,
    report_alt: bool,
    assoc_text: Option<String>,
) -> Vec<u8> {
    let mut number_field = key.number.to_string();
    if report_alt && let Some(shifted) = key.shifted {
        number_field.push(':');
        number_field.push_str(&shifted.to_string());
    }

    let mods_needed = mods_value > 1 || event != 1;
    let mods_field = if mods_needed {
        if event != 1 {
            format!("{mods_value}:{event}")
        } else {
            mods_value.to_string()
        }
    } else {
        String::new()
    };

    let letter_suffix = key.suffix != b'u' && key.suffix != b'~';
    let mut seq = String::from("\x1b[");
    // Letter-final keys with number 1 and no trailing fields collapse to the
    // bare legacy form (`CSI A`), matching xterm.
    if letter_suffix && number_field == "1" && !mods_needed && assoc_text.is_none() {
        seq.push(key.suffix as char);
        return seq.into_bytes();
    }
    seq.push_str(&number_field);
    if mods_needed || assoc_text.is_some() {
        seq.push(';');
        seq.push_str(&mods_field);
        if let Some(text) = assoc_text {
            seq.push(';');
            seq.push_str(&text);
        }
    }
    seq.push(key.suffix as char);
    seq.into_bytes()
}

/// Base (unshifted) and shifted key codes for a character key. The base is the
/// lowercased code point; the shifted alternate is set only when shift changed
/// the produced character.
fn character_key_codes(s: &str, mods: ModifiersState) -> Option<(u32, Option<u32>)> {
    let mut chars = s.chars();
    let c = chars.next()?;
    if chars.next().is_some() {
        return None; // multi-char logical keys are out of scope
    }
    let base_char = c.to_lowercase().next().unwrap_or(c);
    let base = base_char as u32;
    let shifted = (mods.shift_key() && c as u32 != base).then_some(c as u32);
    Some((base, shifted))
}

/// Associated-text code points (`c1:c2:…`) for genuinely printable text.
fn associated_text_codepoints(text: Option<&str>) -> Option<String> {
    let text = text?;
    if text.is_empty() || text.chars().any(|c| c.is_control()) {
        return None;
    }
    let joined = text
        .chars()
        .map(|c| (c as u32).to_string())
        .collect::<Vec<_>>()
        .join(":");
    Some(joined)
}

/// Kitty functional-key table: `NamedKey` → `(unicode number, final byte)`.
/// Covers the keys with legacy escape forms; text keys and modifiers are
/// handled elsewhere.
fn functional_key(named: NamedKey) -> Option<(u32, u8)> {
    let entry = match named {
        NamedKey::ArrowUp => (1, b'A'),
        NamedKey::ArrowDown => (1, b'B'),
        NamedKey::ArrowRight => (1, b'C'),
        NamedKey::ArrowLeft => (1, b'D'),
        NamedKey::Home => (1, b'H'),
        NamedKey::End => (1, b'F'),
        NamedKey::Insert => (2, b'~'),
        NamedKey::Delete => (3, b'~'),
        NamedKey::PageUp => (5, b'~'),
        NamedKey::PageDown => (6, b'~'),
        NamedKey::F1 => (1, b'P'),
        NamedKey::F2 => (1, b'Q'),
        NamedKey::F3 => (13, b'~'),
        NamedKey::F4 => (1, b'S'),
        NamedKey::F5 => (15, b'~'),
        NamedKey::F6 => (17, b'~'),
        NamedKey::F7 => (18, b'~'),
        NamedKey::F8 => (19, b'~'),
        NamedKey::F9 => (20, b'~'),
        NamedKey::F10 => (21, b'~'),
        NamedKey::F11 => (23, b'~'),
        NamedKey::F12 => (24, b'~'),
        NamedKey::CapsLock => (57358, b'u'),
        NamedKey::ScrollLock => (57359, b'u'),
        NamedKey::NumLock => (57360, b'u'),
        NamedKey::PrintScreen => (57361, b'u'),
        NamedKey::Pause => (57362, b'u'),
        NamedKey::ContextMenu => (57363, b'u'),
        _ => return None,
    };
    Some(entry)
}

/// Kitty code points for modifier keys pressed alone, distinguished left/right
/// by physical key.
fn modifier_key_code(physical_key: Option<PhysicalKey>) -> Option<u32> {
    let PhysicalKey::Code(code) = physical_key? else {
        return None;
    };
    let number = match code {
        KeyCode::ShiftLeft => 57441,
        KeyCode::ControlLeft => 57442,
        KeyCode::AltLeft => 57443,
        KeyCode::SuperLeft => 57444,
        KeyCode::ShiftRight => 57447,
        KeyCode::ControlRight => 57448,
        KeyCode::AltRight => 57449,
        KeyCode::SuperRight => 57450,
        _ => return None,
    };
    Some(number)
}

/// Kitty dedicated keypad code points (`KP_0`=57399 …), by physical key.
fn keypad_key_code(physical_key: Option<PhysicalKey>) -> Option<u32> {
    let PhysicalKey::Code(code) = physical_key? else {
        return None;
    };
    let number = match code {
        KeyCode::Numpad0 => 57399,
        KeyCode::Numpad1 => 57400,
        KeyCode::Numpad2 => 57401,
        KeyCode::Numpad3 => 57402,
        KeyCode::Numpad4 => 57403,
        KeyCode::Numpad5 => 57404,
        KeyCode::Numpad6 => 57405,
        KeyCode::Numpad7 => 57406,
        KeyCode::Numpad8 => 57407,
        KeyCode::Numpad9 => 57408,
        KeyCode::NumpadDecimal => 57409,
        KeyCode::NumpadDivide => 57410,
        KeyCode::NumpadMultiply => 57411,
        KeyCode::NumpadSubtract => 57412,
        KeyCode::NumpadAdd => 57413,
        KeyCode::NumpadEnter => 57414,
        KeyCode::NumpadEqual => 57415,
        _ => return None,
    };
    Some(number)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paste_protection_flags_unbracketed_multiline_only() {
        // Newline outside bracketed paste can submit a command → unsafe.
        assert!(paste_is_unsafe("git push\n", false));
        assert!(paste_is_unsafe("a\rb", false));
        // Bracketed paste frames the data itself → safe even with newlines.
        assert!(!paste_is_unsafe("git push\n", true));
        // Single-line paste has nothing to auto-submit.
        assert!(!paste_is_unsafe("just some words", false));
        assert!(!paste_is_unsafe("", false));
    }

    #[test]
    fn printable_uses_text() {
        let key = Key::Character("a".into());
        assert_eq!(
            encode_key(&key, Some("a"), ModifiersState::empty(), false),
            Some(b"a".to_vec())
        );
    }

    #[test]
    fn alt_printable_uses_escape_prefix() {
        let key = Key::Character("a".into());
        assert_eq!(
            encode_key(&key, Some("a"), ModifiersState::ALT, false),
            Some(b"\x1ba".to_vec())
        );
    }

    #[test]
    fn ctrl_letter_takes_priority_over_alt_prefix() {
        let key = Key::Character("c".into());
        assert_eq!(
            encode_key(
                &key,
                Some("c"),
                ModifiersState::CONTROL | ModifiersState::ALT,
                false
            ),
            Some(vec![0x03])
        );
    }

    #[test]
    fn named_text_key_falls_back_to_text() {
        let key = Key::Named(NamedKey::Space);
        assert_eq!(
            encode_key(&key, Some(" "), ModifiersState::empty(), false),
            Some(b" ".to_vec())
        );
    }

    #[test]
    fn committed_ime_text_is_utf8() {
        assert_eq!(encode_text("日本語"), Some("日本語".as_bytes().to_vec()));
        assert_eq!(encode_text(""), None);
    }

    #[test]
    fn ime_commit_emits_text_without_leaking_preedit() {
        let mut state = ImeState::default();

        assert_eq!(state.handle_event(&Ime::Enabled), None);
        assert!(!state.preedit_active());
        assert_eq!(
            state.handle_event(&Ime::Preedit("nihongo".into(), None)),
            None
        );
        assert!(state.preedit_active());
        assert_eq!(
            state.handle_event(&Ime::Commit("日本語".into())),
            Some("日本語".as_bytes().to_vec())
        );
        assert!(!state.preedit_active());
    }

    #[test]
    fn ime_disabled_clears_preedit_state() {
        let mut state = ImeState::default();

        assert_eq!(state.handle_event(&Ime::Preedit("a".into(), None)), None);
        assert!(state.preedit_active());
        assert_eq!(state.handle_event(&Ime::Disabled), None);
        assert!(!state.preedit_active());
    }

    #[test]
    fn enter_is_cr() {
        let key = Key::Named(NamedKey::Enter);
        assert_eq!(
            encode_key(&key, Some("\r"), ModifiersState::empty(), false),
            Some(vec![0x0d])
        );
    }

    #[test]
    fn backspace_is_del() {
        let key = Key::Named(NamedKey::Backspace);
        assert_eq!(
            encode_key(&key, None, ModifiersState::empty(), false),
            Some(vec![0x7f])
        );
    }

    #[test]
    fn tab_and_escape() {
        assert_eq!(
            encode_key(
                &Key::Named(NamedKey::Tab),
                None,
                ModifiersState::empty(),
                false
            ),
            Some(vec![b'\t'])
        );
        assert_eq!(
            encode_key(
                &Key::Named(NamedKey::Escape),
                None,
                ModifiersState::empty(),
                false
            ),
            Some(vec![0x1b])
        );
    }

    #[test]
    fn ctrl_c_is_0x03() {
        let key = Key::Character("c".into());
        assert_eq!(
            encode_key(&key, Some("c"), ModifiersState::CONTROL, false),
            Some(vec![0x03])
        );
    }

    #[test]
    fn ctrl_d_is_0x04() {
        let key = Key::Character("d".into());
        assert_eq!(
            encode_key(&key, Some("d"), ModifiersState::CONTROL, false),
            Some(vec![0x04])
        );
    }

    #[test]
    fn arrow_up_csi_by_default() {
        let key = Key::Named(NamedKey::ArrowUp);
        assert_eq!(
            encode_key(&key, None, ModifiersState::empty(), false),
            Some(vec![0x1b, b'[', b'A'])
        );
    }

    #[test]
    fn arrow_up_ss3_when_app_cursor_keys() {
        let key = Key::Named(NamedKey::ArrowUp);
        assert_eq!(
            encode_key(&key, None, ModifiersState::empty(), true),
            Some(vec![0x1b, b'O', b'A'])
        );
    }

    #[test]
    fn application_keypad_uses_ss3_for_numpad_digits_and_enter() {
        assert_eq!(
            encode_key_with_modes(
                &Key::Character("1".into()),
                Some(PhysicalKey::Code(KeyCode::Numpad1)),
                Some("1"),
                ModifiersState::empty(),
                false,
                true,
                0,
                true,
                false,
            ),
            Some(b"\x1bOq".to_vec())
        );
        assert_eq!(
            encode_key_with_modes(
                &Key::Named(NamedKey::Enter),
                Some(PhysicalKey::Code(KeyCode::NumpadEnter)),
                Some("\r"),
                ModifiersState::empty(),
                false,
                true,
                0,
                true,
                false,
            ),
            Some(b"\x1bOM".to_vec())
        );
    }

    #[test]
    fn numeric_keypad_uses_text_or_standard_enter() {
        assert_eq!(
            encode_key_with_modes(
                &Key::Character("1".into()),
                Some(PhysicalKey::Code(KeyCode::Numpad1)),
                Some("1"),
                ModifiersState::empty(),
                false,
                false,
                0,
                true,
                false,
            ),
            Some(b"1".to_vec())
        );
        assert_eq!(
            encode_key_with_modes(
                &Key::Named(NamedKey::Enter),
                Some(PhysicalKey::Code(KeyCode::NumpadEnter)),
                Some("\r"),
                ModifiersState::empty(),
                false,
                false,
                0,
                true,
                false,
            ),
            Some(vec![0x0d])
        );
    }

    #[test]
    fn unmodified_special_keys_use_standard_sequences() {
        let cases = [
            (NamedKey::ArrowUp, b"\x1b[A".as_slice()),
            (NamedKey::ArrowDown, b"\x1b[B"),
            (NamedKey::ArrowRight, b"\x1b[C"),
            (NamedKey::ArrowLeft, b"\x1b[D"),
            (NamedKey::Home, b"\x1b[H"),
            (NamedKey::End, b"\x1b[F"),
            (NamedKey::Insert, b"\x1b[2~"),
            (NamedKey::Delete, b"\x1b[3~"),
            (NamedKey::PageUp, b"\x1b[5~"),
            (NamedKey::PageDown, b"\x1b[6~"),
            (NamedKey::F1, b"\x1bOP"),
            (NamedKey::F2, b"\x1bOQ"),
            (NamedKey::F3, b"\x1bOR"),
            (NamedKey::F4, b"\x1bOS"),
            (NamedKey::F5, b"\x1b[15~"),
            (NamedKey::F6, b"\x1b[17~"),
            (NamedKey::F7, b"\x1b[18~"),
            (NamedKey::F8, b"\x1b[19~"),
            (NamedKey::F9, b"\x1b[20~"),
            (NamedKey::F10, b"\x1b[21~"),
            (NamedKey::F11, b"\x1b[23~"),
            (NamedKey::F12, b"\x1b[24~"),
        ];

        for (named, expected) in cases {
            assert_eq!(
                encode_key(&Key::Named(named), None, ModifiersState::empty(), false),
                Some(expected.to_vec()),
                "{named:?}"
            );
        }
    }

    #[test]
    fn modified_final_special_keys_use_xterm_modify_key_sequences() {
        let mods = ModifiersState::SHIFT | ModifiersState::ALT | ModifiersState::CONTROL;
        let cases = [
            (NamedKey::ArrowUp, b"\x1b[1;8A".as_slice()),
            (NamedKey::ArrowDown, b"\x1b[1;8B"),
            (NamedKey::ArrowRight, b"\x1b[1;8C"),
            (NamedKey::ArrowLeft, b"\x1b[1;8D"),
            (NamedKey::Home, b"\x1b[1;8H"),
            (NamedKey::End, b"\x1b[1;8F"),
            (NamedKey::F1, b"\x1b[1;8P"),
            (NamedKey::F2, b"\x1b[1;8Q"),
            (NamedKey::F3, b"\x1b[1;8R"),
            (NamedKey::F4, b"\x1b[1;8S"),
        ];

        for (named, expected) in cases {
            assert_eq!(
                encode_key(&Key::Named(named), None, mods, false),
                Some(expected.to_vec()),
                "{named:?}"
            );
        }
    }

    #[test]
    fn modified_tilde_special_keys_use_xterm_modify_key_sequences() {
        let cases = [
            (
                NamedKey::Insert,
                ModifiersState::SHIFT,
                b"\x1b[2;2~".as_slice(),
            ),
            (NamedKey::Delete, ModifiersState::ALT, b"\x1b[3;3~"),
            (NamedKey::PageUp, ModifiersState::CONTROL, b"\x1b[5;5~"),
            (
                NamedKey::PageDown,
                ModifiersState::SHIFT | ModifiersState::CONTROL,
                b"\x1b[6;6~",
            ),
            (
                NamedKey::F5,
                ModifiersState::SHIFT | ModifiersState::ALT | ModifiersState::CONTROL,
                b"\x1b[15;8~",
            ),
            (NamedKey::F6, ModifiersState::SHIFT, b"\x1b[17;2~"),
            (NamedKey::F7, ModifiersState::ALT, b"\x1b[18;3~"),
            (NamedKey::F8, ModifiersState::CONTROL, b"\x1b[19;5~"),
            (NamedKey::F9, ModifiersState::SHIFT, b"\x1b[20;2~"),
            (NamedKey::F10, ModifiersState::ALT, b"\x1b[21;3~"),
            (NamedKey::F11, ModifiersState::CONTROL, b"\x1b[23;5~"),
            (
                NamedKey::F12,
                ModifiersState::SHIFT | ModifiersState::ALT,
                b"\x1b[24;4~",
            ),
        ];

        for (named, mods, expected) in cases {
            assert_eq!(
                encode_key(&Key::Named(named), None, mods, false),
                Some(expected.to_vec()),
                "{named:?}"
            );
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn alt_left_right_send_readline_word_motion_escapes() {
        // Ghostty macOS default keybinds: alt+left = esc:b, alt+right = esc:f.
        assert_eq!(
            encode_key(
                &Key::Named(NamedKey::ArrowLeft),
                None,
                ModifiersState::ALT,
                false
            ),
            Some(b"\x1bb".to_vec())
        );
        assert_eq!(
            encode_key(
                &Key::Named(NamedKey::ArrowRight),
                None,
                ModifiersState::ALT,
                false
            ),
            Some(b"\x1bf".to_vec())
        );
        // The keybind wins even in DECCKM application-cursor mode.
        assert_eq!(
            encode_key(
                &Key::Named(NamedKey::ArrowLeft),
                None,
                ModifiersState::ALT,
                true
            ),
            Some(b"\x1bb".to_vec())
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn alt_with_extra_modifiers_keeps_modified_arrow_encoding() {
        assert_eq!(
            encode_key(
                &Key::Named(NamedKey::ArrowLeft),
                None,
                ModifiersState::ALT | ModifiersState::SHIFT,
                false
            ),
            Some(b"\x1b[1;4D".to_vec())
        );
        // Alt+up/down have no word-motion binding and stay modified arrows.
        assert_eq!(
            encode_key(
                &Key::Named(NamedKey::ArrowUp),
                None,
                ModifiersState::ALT,
                false
            ),
            Some(b"\x1b[1;3A".to_vec())
        );
    }

    #[test]
    fn modified_arrow_uses_csi_even_in_app_cursor_mode() {
        let key = Key::Named(NamedKey::ArrowUp);
        assert_eq!(
            encode_key(&key, None, ModifiersState::SHIFT, true),
            Some(b"\x1b[1;2A".to_vec())
        );
    }

    #[test]
    fn paste_is_plain_without_bracketed_paste() {
        assert_eq!(
            encode_paste("echo hi\n", false),
            Some(b"echo hi\n".to_vec())
        );
    }

    #[test]
    fn paste_is_wrapped_when_bracketed_paste_is_enabled() {
        assert_eq!(
            encode_paste("echo hi\n", true),
            Some(b"\x1b[200~echo hi\n\x1b[201~".to_vec())
        );
    }

    #[test]
    fn empty_paste_emits_no_bytes() {
        assert_eq!(encode_paste("", false), None);
        assert_eq!(encode_paste("", true), None);
    }

    #[test]
    fn paste_strips_nested_bracket_markers_from_payload() {
        assert_eq!(
            encode_paste("a\x1b[201~b\x1b[200~c", true),
            Some(b"\x1b[200~abc\x1b[201~".to_vec())
        );
        assert_eq!(
            encode_paste("a\x1b[201~b\x1b[200~c", false),
            Some(b"abc".to_vec())
        );
    }

    #[test]
    fn paste_with_only_bracket_markers_emits_no_bytes() {
        assert_eq!(encode_paste("\x1b[200~\x1b[201~", true), None);
    }

    // ── Kitty keyboard protocol encoding ───────────────────────────────

    use noa_grid::KITTY_DISAMBIGUATE;

    /// Encode a press with the given Kitty flags (no physical key, not repeat).
    fn kitty_press(
        logical: &Key,
        text: Option<&str>,
        mods: ModifiersState,
        flags: u8,
    ) -> Option<Vec<u8>> {
        encode_key_with_modes(logical, None, text, mods, false, false, flags, true, false)
    }

    #[test]
    fn kitty_disambiguate_escape_key() {
        // Escape always uses the CSI-u form once any flag is set.
        assert_eq!(
            kitty_press(
                &Key::Named(NamedKey::Escape),
                None,
                ModifiersState::empty(),
                1
            ),
            Some(b"\x1b[27u".to_vec())
        );
    }

    #[test]
    fn kitty_disambiguate_ctrl_c_is_csi_u() {
        // Ctrl+C legacy-collides with 0x03; disambiguate sends CSI 99 ; 5 u.
        assert_eq!(
            kitty_press(
                &Key::Character("c".into()),
                Some("c"),
                ModifiersState::CONTROL,
                1
            ),
            Some(b"\x1b[99;5u".to_vec())
        );
    }

    #[test]
    fn kitty_disambiguate_plain_char_stays_text() {
        // A bare or shift-only printable is still sent as legacy text.
        assert_eq!(
            kitty_press(
                &Key::Character("a".into()),
                Some("a"),
                ModifiersState::empty(),
                1
            ),
            Some(b"a".to_vec())
        );
        assert_eq!(
            kitty_press(
                &Key::Character("A".into()),
                Some("A"),
                ModifiersState::SHIFT,
                1
            ),
            Some(b"A".to_vec())
        );
    }

    #[test]
    fn kitty_disambiguate_modified_arrow() {
        // Ctrl+Shift+Up: modifier value = 1 + shift(1) + ctrl(4) = 6.
        assert_eq!(
            kitty_press(
                &Key::Named(NamedKey::ArrowUp),
                None,
                ModifiersState::SHIFT | ModifiersState::CONTROL,
                1,
            ),
            Some(b"\x1b[1;6A".to_vec())
        );
    }

    #[test]
    fn kitty_unmodified_arrow_collapses_to_bare_csi() {
        // With no modifier/event/text the letter-final form drops the leading 1.
        assert_eq!(
            kitty_press(
                &Key::Named(NamedKey::ArrowUp),
                None,
                ModifiersState::empty(),
                1
            ),
            Some(b"\x1b[A".to_vec())
        );
    }

    #[test]
    fn kitty_report_all_keys_encodes_enter_and_char() {
        // Report-all-keys escape-encodes even text-producing keys.
        assert_eq!(
            kitty_press(
                &Key::Named(NamedKey::Enter),
                Some("\r"),
                ModifiersState::empty(),
                8
            ),
            Some(b"\x1b[13u".to_vec())
        );
        assert_eq!(
            kitty_press(
                &Key::Character("a".into()),
                Some("a"),
                ModifiersState::empty(),
                8
            ),
            Some(b"\x1b[97u".to_vec())
        );
    }

    #[test]
    fn kitty_alternate_keys_report_shifted_code() {
        // Ctrl+Shift+A with alternate-keys flag: 97:65 base:shifted, mods 6.
        assert_eq!(
            kitty_press(
                &Key::Character("A".into()),
                Some("A"),
                ModifiersState::SHIFT | ModifiersState::CONTROL,
                KITTY_REPORT_ALTERNATE_KEYS,
            ),
            Some(b"\x1b[97:65;6u".to_vec())
        );
    }

    #[test]
    fn kitty_associated_text_appended_for_shifted_char() {
        // Report-all + associated-text: shift+a -> CSI 97 ; 2 ; 65 u.
        assert_eq!(
            kitty_press(
                &Key::Character("A".into()),
                Some("A"),
                ModifiersState::SHIFT,
                KITTY_REPORT_ALL_KEYS | KITTY_REPORT_ASSOCIATED_TEXT,
            ),
            Some(b"\x1b[97;2;65u".to_vec())
        );
        // Plain 'a' keeps an empty modifier field before the text field.
        assert_eq!(
            kitty_press(
                &Key::Character("a".into()),
                Some("a"),
                ModifiersState::empty(),
                KITTY_REPORT_ALL_KEYS | KITTY_REPORT_ASSOCIATED_TEXT,
            ),
            Some(b"\x1b[97;;97u".to_vec())
        );
    }

    #[test]
    fn kitty_event_types_report_release_and_repeat() {
        // Release of a bare arrow: event 3 forces the modifier field (1:3).
        assert_eq!(
            encode_key_with_modes(
                &Key::Named(NamedKey::ArrowUp),
                None,
                None,
                ModifiersState::empty(),
                false,
                false,
                KITTY_REPORT_EVENT_TYPES,
                false, // released
                false,
            ),
            Some(b"\x1b[1;1:3A".to_vec())
        );
        // Repeat of the same key: event 2.
        assert_eq!(
            encode_key_with_modes(
                &Key::Named(NamedKey::ArrowUp),
                None,
                None,
                ModifiersState::empty(),
                false,
                false,
                KITTY_REPORT_EVENT_TYPES,
                true,
                true, // repeat
            ),
            Some(b"\x1b[1;1:2A".to_vec())
        );
    }

    #[test]
    fn kitty_release_of_text_key_without_report_all_is_dropped() {
        // A plain char release is not reported under event-types alone.
        assert_eq!(
            encode_key_with_modes(
                &Key::Character("a".into()),
                None,
                Some("a"),
                ModifiersState::empty(),
                false,
                false,
                KITTY_REPORT_EVENT_TYPES,
                false,
                false,
            ),
            None
        );
    }

    #[test]
    fn kitty_modifier_key_alone_reported_only_with_report_all() {
        // Left Shift pressed alone with report-all: dedicated code 57441,
        // modifier value 2 (shift now active).
        assert_eq!(
            encode_key_with_modes(
                &Key::Named(NamedKey::Shift),
                Some(PhysicalKey::Code(KeyCode::ShiftLeft)),
                None,
                ModifiersState::SHIFT,
                false,
                false,
                KITTY_REPORT_ALL_KEYS,
                true,
                false,
            ),
            Some(b"\x1b[57441;2u".to_vec())
        );
        // Without report-all the lone modifier is silent.
        assert_eq!(
            encode_key_with_modes(
                &Key::Named(NamedKey::Shift),
                Some(PhysicalKey::Code(KeyCode::ShiftLeft)),
                None,
                ModifiersState::SHIFT,
                false,
                false,
                KITTY_DISAMBIGUATE,
                true,
                false,
            ),
            None
        );
    }

    #[test]
    fn kitty_keypad_uses_dedicated_codes_under_report_all() {
        assert_eq!(
            encode_key_with_modes(
                &Key::Character("1".into()),
                Some(PhysicalKey::Code(KeyCode::Numpad1)),
                Some("1"),
                ModifiersState::empty(),
                false,
                false,
                KITTY_REPORT_ALL_KEYS,
                true,
                false,
            ),
            Some(b"\x1b[57400u".to_vec())
        );
    }
}
