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
        _ => text
            .filter(|s| !s.is_empty())
            .map(|s| s.as_bytes().to_vec()),
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
