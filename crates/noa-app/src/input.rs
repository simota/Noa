//! Keyboard-event -> pty-byte encoding.
//!
//! `winit::event::KeyEvent` has a private platform-specific field, so it
//! can't be constructed in tests; [`encode_key`] takes the pieces we need
//! (`logical_key`, `text`, modifiers) directly so the encoding logic stays
//! unit-testable without a live `KeyEvent`.

use winit::keyboard::{Key, ModifiersState, NamedKey};

/// Encode a pressed key into the bytes that should be written to the pty, if
/// any. `app_cursor_keys` mirrors `ModeState::app_cursor_keys()` (DECCKM):
/// when set, arrow keys send `SS3` (`ESC O <letter>`) instead of `CSI`
/// (`ESC [ <letter>`).
pub fn encode_key(
    logical_key: &Key,
    text: Option<&str>,
    mods: ModifiersState,
    app_cursor_keys: bool,
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

    match logical_key {
        Key::Named(NamedKey::Enter) => Some(vec![0x0d]),
        Key::Named(NamedKey::Backspace) => Some(vec![0x7f]),
        Key::Named(NamedKey::Tab) => Some(vec![b'\t']),
        Key::Named(NamedKey::Escape) => Some(vec![0x1b]),
        Key::Named(NamedKey::ArrowUp) => Some(arrow_bytes(b'A', app_cursor_keys)),
        Key::Named(NamedKey::ArrowDown) => Some(arrow_bytes(b'B', app_cursor_keys)),
        Key::Named(NamedKey::ArrowRight) => Some(arrow_bytes(b'C', app_cursor_keys)),
        Key::Named(NamedKey::ArrowLeft) => Some(arrow_bytes(b'D', app_cursor_keys)),
        _ => text.filter(|s| !s.is_empty()).map(|s| s.as_bytes().to_vec()),
    }
}

fn arrow_bytes(letter: u8, app_cursor_keys: bool) -> Vec<u8> {
    if app_cursor_keys {
        vec![0x1b, b'O', letter]
    } else {
        vec![0x1b, b'[', letter]
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
            encode_key(&Key::Named(NamedKey::Tab), None, ModifiersState::empty(), false),
            Some(vec![b'\t'])
        );
        assert_eq!(
            encode_key(&Key::Named(NamedKey::Escape), None, ModifiersState::empty(), false),
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
}
