use winit::keyboard::{Key, KeyCode, ModifiersState, NamedKey, PhysicalKey};

use super::kitty::{KittyOutcome, encode_kitty};
use super::text::encode_text;

/// The Option/Alt classification belongs to a physical press, including its
/// repeats and release: winit's release events carry no composed text.
#[derive(Default)]
pub(crate) struct KeyModifierState {
    alt_by_key: std::collections::HashMap<PhysicalKey, bool>,
}

impl KeyModifierState {
    pub(crate) fn alt_sends_esc(
        &mut self,
        key: PhysicalKey,
        pressed: bool,
        repeat: bool,
        event_alt_sends_esc: bool,
    ) -> bool {
        if !pressed {
            return self.alt_by_key.remove(&key).unwrap_or(event_alt_sends_esc);
        }
        if !repeat {
            self.alt_by_key.insert(key, event_alt_sends_esc);
        }
        *self.alt_by_key.entry(key).or_insert(event_alt_sends_esc)
    }

    pub(crate) fn clear(&mut self) {
        self.alt_by_key.clear();
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
        None,
        text,
        mods,
        true,
        app_cursor_keys,
        false,
        0,
        false,
        false,
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
/// `modify_other_keys` mirrors `Terminal::modify_other_keys_2` (xterm
/// `CSI > 4 ; 2 m`): Character keys pressed with Ctrl/Alt/Super are reported
/// as `CSI 27 ; mods ; codepoint ~` instead of the legacy C0/ESC forms. The
/// Kitty protocol, when active, still takes precedence.
///
/// `backarrow_key_mode` mirrors DECBKM (mode 67): when set, Backspace sends
/// BS (0x08) and Ctrl+Backspace sends DEL (0x7f), swapping the defaults.
/// Other modifier combinations and the Kitty protocol ignore it, as in
/// Ghostty.
///
/// `alt_sends_esc` says whether Alt held with this press should ESC-prefix the
/// produced text. On macOS the Option key composes characters unless
/// `macos-option-as-alt` claims it, so the caller retains the press's verdict
/// through repeats and release; on other platforms it is simply `true`.
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
    modify_other_keys: bool,
    backarrow_key_mode: bool,
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
            alt_sends_esc,
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

    // Ghostty encodes with either the Kitty protocol or the legacy one, never
    // both: a key the Kitty path leaves as text (Shift+A) must not become a
    // modifyOtherKeys report when an app enabled both.
    let modify_other_keys = modify_other_keys && kitty_flags == 0;

    // Shift+Enter's ESC CR (see the Enter arm below) covers the keypad Enter
    // too, as it did before the keypad table.
    if mods == ModifiersState::SHIFT
        && physical_key == Some(PhysicalKey::Code(KeyCode::NumpadEnter))
    {
        return Some(b"\x1b\r".to_vec());
    }

    // Keypad keys come first, as in Ghostty's PC-style function key table:
    // a modifier never turns them into a C0 byte or a modifyOtherKeys report.
    if let Some(bytes) = keypad_bytes(logical_key, physical_key, mods, app_keypad) {
        return Some(bytes);
    }

    // xterm modifyOtherKeys level 2: a Character key with Ctrl/Alt/Super
    // reports its codepoint after Shift/layout translation plus the modifier
    // value, so Ctrl+I is distinguishable from Tab. With Shift alone only
    // U+0040..U+007F and Space are reported (Option composing text on macOS
    // doesn't count as Alt).
    if modify_other_keys
        && let Some(bytes) = modify_other_keys_bytes(logical_key, mods, alt_sends_esc)
    {
        return Some(bytes);
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
                    // Alt still ESC-prefixes the control byte (Ctrl+Alt+A ->
                    // ESC 0x01) when it isn't composing text.
                    return Some(alt_esc_prefixed(vec![byte], mods, alt_sends_esc));
                }
            }
            // winit can report Space as a named key; Ctrl+Space is NUL
            // (emacs set-mark and friends).
            Key::Named(NamedKey::Space) => {
                return Some(alt_esc_prefixed(vec![0x00], mods, alt_sends_esc));
            }
            _ => {}
        }
    }

    match logical_key {
        Key::Named(NamedKey::Enter) => {
            // Shift+Enter sends ESC CR so legacy-protocol line editors
            // (Claude Code and friends) can tell it apart from Enter and
            // insert a newline. Ghostty's stock encoder emits CSI 27;2;13~
            // here, which those apps print as garbage; the upstream-blessed
            // fix is `keybind = shift+enter=text:\x1b\r`, adopted as our
            // default. Kitty-protocol apps still get CSI 13;2u above.
            if mods == ModifiersState::SHIFT {
                return Some(b"\x1b\r".to_vec());
            }
            Some(pc_style_key_bytes(0x0d, b"\r", mods, modify_other_keys))
        }
        Key::Named(NamedKey::Backspace) => {
            // Ghostty's table: Ctrl sends BS and every other modifier set
            // DEL, Alt prefixing ESC so readline deletes a word. Under
            // modifyOtherKeys 2 every set except bare Ctrl reports CSI 27.
            // DECBKM swaps the two bytes for bare and Ctrl-only Backspace.
            // Under the Kitty protocol a bare Backspace falls back to this
            // path but stays DEL: Kitty has no DECBKM.
            let swap = backarrow_key_mode && kitty_flags == 0;
            if mods.is_empty() {
                return Some(vec![if swap { 0x08 } else { 0x7f }]);
            }
            if mods == ModifiersState::CONTROL {
                return Some(vec![if swap { 0x7f } else { 0x08 }]);
            }
            if modify_other_keys {
                return Some(csi27_bytes(0x7f, mods));
            }
            let byte = if mods.control_key() { 0x08 } else { 0x7f };
            Some(alt_prefixed(vec![byte], mods))
        }
        Key::Named(NamedKey::Tab) => {
            if mods == ModifiersState::SHIFT && !modify_other_keys {
                return Some(b"\x1b[Z".to_vec()); // backtab
            }
            Some(pc_style_key_bytes(b'\t', b"\t", mods, modify_other_keys))
        }
        Key::Named(NamedKey::Escape) => {
            Some(pc_style_key_bytes(0x1b, b"\x1b", mods, modify_other_keys))
        }
        Key::Named(named) => special_key_bytes(*named, mods, app_cursor_keys)
            .or_else(|| encode_key_text(text, mods, alt_sends_esc)),
        _ => encode_key_text(text, mods, alt_sends_esc),
    }
}

/// Bytes an unmodified Enter press writes to the pty under the given Kitty
/// keyboard flags: `CSI 13 u` once report-all-keys is in effect, legacy CR
/// otherwise. Delegates to [`encode_key_with_modes`] so a synthesized Enter
/// (the send-selection trailing Enter) can never diverge from a typed one.
pub(crate) fn encode_enter_key(kitty_flags: u8) -> Vec<u8> {
    encode_key_with_modes(
        &Key::Named(NamedKey::Enter),
        None,
        None,
        None,
        ModifiersState::empty(),
        true,
        false,
        false,
        kitty_flags,
        false,
        false,
        true,
        false,
    )
    .expect("an unmodified Enter press always encodes")
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

/// ESC-prefix `bytes` for Alt, but only when Alt is acting as a modifier
/// (`alt_sends_esc`) rather than composing text via macOS Option.
fn alt_esc_prefixed(mut bytes: Vec<u8>, mods: ModifiersState, alt_sends_esc: bool) -> Vec<u8> {
    if mods.alt_key() && alt_sends_esc {
        bytes.insert(0, 0x1b);
    }
    bytes
}

/// xterm `modifyOtherKeys=2` encoding, as Ghostty 1.4.0 copies xterm's
/// `ModifyOtherKeys`: `CSI 27 ; <mods> ; <codepoint> ~` for a Character (or
/// Space) key pressed with Ctrl, Alt-as-modifier, or Super, or with Shift
/// alone when the codepoint is in U+0040..U+007F or is Space. The logical
/// key preserves Shift and keyboard-layout translation, so Ctrl+Shift+1 on a
/// US layout reports `33` (`!`) with the Shift bit.
fn modify_other_keys_bytes(
    logical_key: &Key,
    mods: ModifiersState,
    alt_sends_esc: bool,
) -> Option<Vec<u8>> {
    let alt = mods.alt_key() && alt_sends_esc;
    let codepoint = match logical_key {
        Key::Character(s) => {
            let mut chars = s.chars();
            match (chars.next(), chars.next()) {
                (Some(c), None) => c as u32,
                _ => return None,
            }
        }
        Key::Named(NamedKey::Space) => u32::from(' '),
        _ => return None,
    };
    let modified = mods.control_key()
        || alt
        || mods.super_key()
        || (mods.shift_key() && (matches!(codepoint, 0x40..=0x7f) || codepoint == 0x20));
    if !modified {
        return None;
    }
    let mut value = 1;
    if mods.shift_key() {
        value += 1;
    }
    if alt {
        value += 2;
    }
    if mods.control_key() {
        value += 4;
    }
    if mods.super_key() {
        value += 8;
    }
    Some(format!("\x1b[27;{value};{codepoint}~").into_bytes())
}

fn alt_prefixed(mut bytes: Vec<u8>, mods: ModifiersState) -> Vec<u8> {
    if mods.alt_key() {
        bytes.insert(0, 0x1b);
    }
    bytes
}

/// Ghostty's keypad entries. With DECKPAM a key sends `SS3 <final>`, or
/// `ESC O <mods> <final>` with modifiers; otherwise it sends its character
/// whatever the modifiers. Only keys that produce their character (Num Lock
/// on) qualify; the navigation keys a keypad sends otherwise are named keys.
fn keypad_bytes(
    logical_key: &Key,
    physical_key: Option<PhysicalKey>,
    mods: ModifiersState,
    app_keypad: bool,
) -> Option<Vec<u8>> {
    let PhysicalKey::Code(code) = physical_key? else {
        return None;
    };
    let (final_byte, text) = match code {
        KeyCode::Numpad0 => (b'p', b'0'),
        KeyCode::Numpad1 => (b'q', b'1'),
        KeyCode::Numpad2 => (b'r', b'2'),
        KeyCode::Numpad3 => (b's', b'3'),
        KeyCode::Numpad4 => (b't', b'4'),
        KeyCode::Numpad5 => (b'u', b'5'),
        KeyCode::Numpad6 => (b'v', b'6'),
        KeyCode::Numpad7 => (b'w', b'7'),
        KeyCode::Numpad8 => (b'x', b'8'),
        KeyCode::Numpad9 => (b'y', b'9'),
        KeyCode::NumpadDecimal => (b'n', b'.'),
        KeyCode::NumpadAdd => (b'k', b'+'),
        KeyCode::NumpadSubtract => (b'm', b'-'),
        KeyCode::NumpadMultiply => (b'j', b'*'),
        KeyCode::NumpadDivide => (b'o', b'/'),
        KeyCode::NumpadEnter => (b'M', b'\r'),
        _ => return None,
    };
    if code != KeyCode::NumpadEnter && !matches!(logical_key, Key::Character(_)) {
        return None;
    }
    Some(match (app_keypad, mods_value(mods)) {
        (false, _) => vec![text],
        (true, 1) => vec![0x1b, b'O', final_byte],
        (true, value) => format!("\x1bO{value}{}", final_byte as char).into_bytes(),
    })
}

/// xterm's modifier parameter: 1 plus Shift 1, Alt 2, Ctrl 4, Super 8.
fn mods_value(mods: ModifiersState) -> u8 {
    1 + u8::from(mods.shift_key())
        + 2 * u8::from(mods.alt_key())
        + 4 * u8::from(mods.control_key())
        + 8 * u8::from(mods.super_key())
}

/// `CSI 27 ; <mods> ; <code> ~`, xterm's modifyOtherKeys form.
fn csi27_bytes(code: u8, mods: ModifiersState) -> Vec<u8> {
    format!("\x1b[27;{};{code}~", mods_value(mods)).into_bytes()
}

/// Ghostty's table for Enter, Tab and Escape: the plain key sends `plain`;
/// Alt alone ESC-prefixes it unless modifyOtherKeys 2 is on; every other
/// modifier set reports `CSI 27 ; <mods> ; <code> ~`.
fn pc_style_key_bytes(
    code: u8,
    plain: &[u8],
    mods: ModifiersState,
    modify_other_keys: bool,
) -> Vec<u8> {
    if mods.is_empty() {
        return plain.to_vec();
    }
    if mods == ModifiersState::ALT && !modify_other_keys {
        return alt_prefixed(plain.to_vec(), mods);
    }
    csi27_bytes(code, mods)
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
