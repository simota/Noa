use winit::keyboard::{Key, KeyCode, ModifiersState, NamedKey, PhysicalKey};

use super::kitty::{KittyOutcome, encode_kitty};
use super::text::encode_text;

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
        None,
        text,
        mods,
        true,
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
///
/// `unmodified_key` is winit's `key_without_modifiers()` — the key the same
/// physical press produces with no modifiers held — used by the Kitty encoder
/// to report the unshifted base key code (Shift+1 must report `1`, not `!`).
///
/// `alt_sends_esc` says whether Alt held with this press should ESC-prefix the
/// produced text. On macOS the Option key composes characters unless
/// `macos-option-as-alt` claims it, so the caller decides per event; on other
/// platforms it is simply `true`.
#[allow(clippy::too_many_arguments)]
pub fn encode_key_with_modes(
    logical_key: &Key,
    unmodified_key: Option<&Key>,
    physical_key: Option<PhysicalKey>,
    text: Option<&str>,
    mods: ModifiersState,
    alt_sends_esc: bool,
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
            unmodified_key,
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

    // The legacy encoding only ever sends bytes for a press or an OS
    // auto-repeat; a release produces no legacy input (the Kitty path above is
    // the only one that reports releases). Without this guard a released
    // non-text key (Enter/Backspace/Ctrl+C) would encode a second time and
    // double-send.
    if !pressed {
        return None;
    }

    // Ctrl+key -> the corresponding C0 control byte. Checked before the
    // general text path since terminals expect Ctrl+A..Z (and the classic
    // xterm symbol/digit mappings, e.g. Ctrl+Space=NUL, Ctrl+[=ESC) to send
    // their control byte regardless of what `text` the platform layer
    // produced.
    if mods.control_key() {
        match logical_key {
            Key::Character(s) => {
                let mut chars = s.chars();
                if let (Some(c), None) = (chars.next(), chars.next())
                    && let Some(byte) = ctrl_c0_byte(c)
                {
                    return Some(vec![byte]);
                }
            }
            // winit can report Space as a named key; Ctrl+Space is NUL
            // (emacs set-mark and friends).
            Key::Named(NamedKey::Space) => return Some(vec![0x00]),
            _ => {}
        }
    }

    if app_keypad
        && modifier_value(mods).is_none()
        && let Some(bytes) = application_keypad_bytes(physical_key)
    {
        return Some(bytes);
    }

    match logical_key {
        Key::Named(NamedKey::Enter) => {
            // Shift+Enter sends ESC CR so legacy-protocol line editors
            // (Claude Code and friends) can tell it apart from Enter and
            // insert a newline. Ghostty's stock encoder emits CSI 27;2;13~
            // here, which those apps print as garbage; the upstream-blessed
            // fix is `keybind = shift+enter=text:\x1b\r`, adopted as our
            // default. Kitty-protocol apps still get CSI 13;2u above.
            if mods.shift_key() && !mods.control_key() && !mods.alt_key() {
                Some(b"\x1b\r".to_vec())
            } else {
                Some(alt_prefixed(vec![0x0d], mods))
            }
        }
        Key::Named(NamedKey::Backspace) => {
            // Ctrl+Backspace sends BS (0x08); Alt prefixes ESC so readline
            // deletes a word (Ghostty/Terminal.app behavior).
            let byte = if mods.control_key() { 0x08 } else { 0x7f };
            Some(alt_prefixed(vec![byte], mods))
        }
        Key::Named(NamedKey::Tab) => {
            if mods.shift_key() {
                Some(b"\x1b[Z".to_vec()) // backtab
            } else {
                Some(vec![b'\t'])
            }
        }
        Key::Named(NamedKey::Escape) => Some(vec![0x1b]),
        Key::Named(named) => special_key_bytes(*named, mods, app_cursor_keys)
            .or_else(|| encode_key_text(text, mods, alt_sends_esc)),
        _ => encode_key_text(text, mods, alt_sends_esc),
    }
}

/// The C0 byte for Ctrl+`c` under the legacy encoding: letters map to
/// 0x01..0x1a, plus the classic xterm symbol and digit mappings.
fn ctrl_c0_byte(c: char) -> Option<u8> {
    let c = c.to_ascii_lowercase();
    let byte = match c {
        'a'..='z' => (c as u8) - b'a' + 1,
        ' ' | '@' | '2' => 0x00,
        '[' | '3' => 0x1b,
        '\\' | '4' => 0x1c,
        ']' | '5' => 0x1d,
        '^' | '6' => 0x1e,
        '_' | '7' | '/' | '-' => 0x1f,
        '8' | '?' => 0x7f,
        _ => return None,
    };
    Some(byte)
}

fn alt_prefixed(mut bytes: Vec<u8>, mods: ModifiersState) -> Vec<u8> {
    if mods.alt_key() {
        bytes.insert(0, 0x1b);
    }
    bytes
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

fn encode_key_text(
    text: Option<&str>,
    mods: ModifiersState,
    alt_sends_esc: bool,
) -> Option<Vec<u8>> {
    let mut bytes = encode_text(text?)?;
    // On macOS, Option that composed a character (macos-option-as-alt off for
    // that side) already put the composed text in `text`; it must pass through
    // without an ESC prefix. `alt_sends_esc` is the caller's per-event verdict.
    if mods.alt_key() && alt_sends_esc {
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
