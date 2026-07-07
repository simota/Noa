use noa_grid::{
    KITTY_REPORT_ALL_KEYS, KITTY_REPORT_ALTERNATE_KEYS, KITTY_REPORT_ASSOCIATED_TEXT,
    KITTY_REPORT_EVENT_TYPES,
};
use winit::keyboard::{Key, KeyCode, ModifiersState, NamedKey, PhysicalKey};

/// Outcome of the Kitty keyboard encoder.
pub(super) enum KittyOutcome {
    /// A complete Kitty escape sequence to send.
    Escape(Vec<u8>),
    /// This key is sent with its legacy encoding under the active flags -
    /// delegate to the legacy path (only reached for presses).
    Legacy,
    /// Nothing to send (e.g. a released text key, or a key winit can't map).
    Ignore,
}

/// How a key maps into the Kitty CSI form once we decide to escape-encode it.
struct KittyKey {
    /// Primary unicode-key-code / functional number.
    number: u32,
    /// Final byte: `u`, `~`, or a legacy letter (`A`/`H`/`P`/...).
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

#[allow(clippy::too_many_arguments)]
pub(super) fn encode_kitty(
    logical_key: &Key,
    unmodified_key: Option<&Key>,
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
        Key::Named(NamedKey::Space) => {
            if !report_all && !has_non_shift {
                return legacy_or_ignore(event);
            }
            KittyKey {
                number: 32,
                suffix: b'u',
                shifted: None,
            }
        }
        Key::Named(named) => match functional_key(*named) {
            // Functional keys (arrows, F-keys, Home/End/...) always escape-encode.
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
                let Some((base, shifted)) = character_key_codes(s, unmodified_key, mods) else {
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

/// A press or repeat falls back to the legacy encoding; only a release of a
/// legacy key is dropped. Keys that keep their legacy/text encoding under the
/// active flags must still repeat on OS auto-repeat (Kitty spec), so `event`
/// 2 (repeat) is delegated to the legacy path just like a press.
fn legacy_or_ignore(event: u8) -> KittyOutcome {
    if event == 3 {
        KittyOutcome::Ignore
    } else {
        KittyOutcome::Legacy
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
/// key the press produces with no modifiers (winit's `key_without_modifiers`,
/// lowercased) - Shift+1 reports base `1`, not `!`. Falls back to lowercasing
/// the produced character when the caller has no unmodified key (tests). The
/// shifted alternate is set only when shift changed the produced character.
fn character_key_codes(
    s: &str,
    unmodified_key: Option<&Key>,
    mods: ModifiersState,
) -> Option<(u32, Option<u32>)> {
    let c = single_char(s)?;
    let base_char = match unmodified_key {
        Some(Key::Character(u)) => single_char(u),
        _ => None,
    }
    .unwrap_or(c);
    let base_char = base_char.to_lowercase().next().unwrap_or(base_char);
    let base = base_char as u32;
    let shifted = (mods.shift_key() && c as u32 != base).then_some(c as u32);
    Some((base, shifted))
}

fn single_char(s: &str) -> Option<char> {
    let mut chars = s.chars();
    let c = chars.next()?;
    // Multi-char logical keys are out of scope.
    chars.next().is_none().then_some(c)
}

/// Associated-text code points (`c1:c2:...`) for genuinely printable text.
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

/// Kitty functional-key table: `NamedKey` -> `(unicode number, final byte)`.
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

/// Kitty dedicated keypad code points (`KP_0`=57399 ...), by physical key.
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
