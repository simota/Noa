//! Keyboard-event -> pty-byte encoding.
//!
//! `winit::event::KeyEvent` has a private platform-specific field, so it
//! can't be constructed in tests; [`encode_key`] takes the pieces we need
//! (`logical_key`, `text`, modifiers) directly so the encoding logic stays
//! unit-testable without a live `KeyEvent`.

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
    encode_key_with_modes(logical_key, None, text, mods, app_cursor_keys, false)
}

pub fn encode_key_with_modes(
    logical_key: &Key,
    physical_key: Option<PhysicalKey>,
    text: Option<&str>,
    mods: ModifiersState,
    app_cursor_keys: bool,
    app_keypad: bool,
) -> Option<Vec<u8>> {
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
