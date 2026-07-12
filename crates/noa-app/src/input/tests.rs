use super::*;
use noa_grid::{
    KITTY_DISAMBIGUATE, KITTY_REPORT_ALL_KEYS, KITTY_REPORT_ALTERNATE_KEYS,
    KITTY_REPORT_ASSOCIATED_TEXT, KITTY_REPORT_EVENT_TYPES,
};
use winit::event::Ime;
use winit::keyboard::{Key, KeyCode, ModifiersState, NamedKey, PhysicalKey};

/// Bolt perf harness (text-input hot path): per-keystroke encoding cost for
/// the legacy and Kitty-protocol paths. `#[ignore]`d so `cargo test` stays
/// fast; run explicitly with:
/// `cargo test -p noa-app --offline input::tests::bench_encode_key_with_modes -- --ignored --nocapture`
#[test]
#[ignore]
fn bench_encode_key_with_modes() {
    const ITERS: u32 = 200_000;

    let printable = Key::Character("a".into());
    let ctrl_c = Key::Character("c".into());
    let arrow = Key::Named(NamedKey::ArrowRight);

    let cases: &[(&str, &Key, Option<&str>, ModifiersState, u8)] = &[
        (
            "legacy printable 'a'",
            &printable,
            Some("a"),
            ModifiersState::empty(),
            0,
        ),
        (
            "legacy ctrl+c",
            &ctrl_c,
            Some("c"),
            ModifiersState::CONTROL,
            0,
        ),
        (
            "legacy arrow-right",
            &arrow,
            None,
            ModifiersState::empty(),
            0,
        ),
        (
            "kitty printable 'a'",
            &printable,
            Some("a"),
            ModifiersState::empty(),
            KITTY_DISAMBIGUATE,
        ),
        (
            "kitty arrow-right",
            &arrow,
            None,
            ModifiersState::empty(),
            KITTY_DISAMBIGUATE,
        ),
    ];

    for (label, key, text, mods, kitty_flags) in cases.iter().copied() {
        let start = std::time::Instant::now();
        for _ in 0..ITERS {
            let _ = std::hint::black_box(encode_key_with_modes(
                key,
                Some(key),
                Some(PhysicalKey::Code(KeyCode::KeyA)),
                text,
                mods,
                true,
                false,
                false,
                kitty_flags,
                true,
                false,
            ));
        }
        let elapsed = start.elapsed();
        eprintln!(
            "bench_encode_key_with_modes[{label}]: {:.1} ns/op ({ITERS} iters, {elapsed:?} total)",
            elapsed.as_nanos() as f64 / f64::from(ITERS)
        );
    }
}

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
    assert_eq!(state.preedit_text(), "");
}

#[test]
fn ime_commit_preedit_clears_composition_without_encoding_it() {
    // Mirrors the focus-loss path (`WindowEvent::Focused(false)`): unlike
    // `Ime::Commit`, there is no committed text to encode — only the
    // half-typed composition is discarded so it doesn't keep swallowing
    // keys via `keyboard_preedit_should_swallow_key` once focus returns.
    let mut state = ImeState::default();

    state.handle_event(&Ime::Preedit("にほん".into(), Some((0, 9))));
    assert!(state.preedit_active());

    state.commit_preedit();

    assert!(!state.preedit_active());
    assert_eq!(state.preedit_text(), "");
    assert_eq!(state.preedit_cursor(), None);
}

#[test]
fn ime_preedit_retains_text_and_cursor_range() {
    let mut state = ImeState::default();

    assert_eq!(
        state.handle_event(&Ime::Preedit("にほ".into(), Some((3, 6)))),
        None
    );
    assert!(state.preedit_active());
    assert_eq!(state.preedit_text(), "にほ");
    assert_eq!(state.preedit_cursor(), Some((3, 6)));

    // A later Preedit replaces the retained text and range wholesale.
    assert_eq!(
        state.handle_event(&Ime::Preedit("にほん".into(), None)),
        None
    );
    assert_eq!(state.preedit_text(), "にほん");
    assert_eq!(state.preedit_cursor(), None);
}

#[test]
fn ime_commit_clears_retained_preedit_text() {
    let mut state = ImeState::default();

    state.handle_event(&Ime::Preedit("にほん".into(), Some((0, 9))));
    assert_eq!(
        state.handle_event(&Ime::Commit("日本".into())),
        Some("日本".as_bytes().to_vec())
    );
    assert_eq!(state.preedit_text(), "");
    assert_eq!(state.preedit_cursor(), None);
}

#[test]
fn ime_commit_suppresses_matching_keyboard_text_echo_once() {
    let mut state = ImeState::default();

    assert_eq!(
        state.handle_event(&Ime::Commit("無".into())),
        Some("無".as_bytes().to_vec())
    );
    assert!(state.consume_commit_echo(Some("無")));
    assert!(!state.consume_commit_echo(Some("無")));
}

#[test]
fn ime_commit_echo_survives_empty_preedit_clear() {
    // macOS emits `Commit` → `Preedit("")` (marked-text clear) → the
    // commit's `KeyboardInput.text` echo; the guard must outlive the
    // empty preedit or the committed text is sent twice.
    let mut state = ImeState::default();

    assert_eq!(
        state.handle_event(&Ime::Commit("出".into())),
        Some("出".as_bytes().to_vec())
    );
    assert_eq!(state.handle_event(&Ime::Preedit(String::new(), None)), None);
    assert!(state.consume_commit_echo(Some("出")));
}

#[test]
fn ime_new_composition_drops_stale_commit_echo_guard() {
    let mut state = ImeState::default();

    state.handle_event(&Ime::Commit("出".into()));
    state.handle_event(&Ime::Preedit("に".into(), Some((0, 3))));
    assert!(!state.consume_commit_echo(Some("出")));
}

#[test]
fn ime_commit_echo_mismatch_does_not_suppress_text() {
    let mut state = ImeState::default();

    state.handle_event(&Ime::Commit("無".into()));
    assert!(!state.consume_commit_echo(Some("効")));
    assert!(!state.consume_commit_echo(Some("無")));
}

#[test]
fn ime_disabled_clears_preedit_state() {
    let mut state = ImeState::default();

    assert_eq!(
        state.handle_event(&Ime::Preedit("a".into(), Some((0, 1)))),
        None
    );
    assert!(state.preedit_active());
    assert_eq!(state.preedit_cursor(), Some((0, 1)));
    assert_eq!(state.handle_event(&Ime::Disabled), None);
    assert!(!state.preedit_active());
    assert_eq!(state.preedit_text(), "");
    assert_eq!(state.preedit_cursor(), None);
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
fn shift_enter_is_esc_cr() {
    let key = Key::Named(NamedKey::Enter);
    assert_eq!(
        encode_key(&key, Some("\r"), ModifiersState::SHIFT, false),
        Some(b"\x1b\r".to_vec())
    );
    // Ctrl/Alt combos keep the plain-CR (alt: ESC-prefixed CR) encoding.
    assert_eq!(
        encode_key(
            &key,
            Some("\r"),
            ModifiersState::SHIFT | ModifiersState::CONTROL,
            false
        ),
        Some(vec![0x0d])
    );
    assert_eq!(
        encode_key(
            &key,
            Some("\r"),
            ModifiersState::SHIFT | ModifiersState::ALT,
            true
        ),
        Some(vec![0x1b, 0x0d])
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
fn shift_tab_is_backtab() {
    assert_eq!(
        encode_key(
            &Key::Named(NamedKey::Tab),
            None,
            ModifiersState::SHIFT,
            false
        ),
        Some(b"\x1b[Z".to_vec())
    );
}

#[test]
fn ctrl_space_is_nul() {
    // winit may report Space as a named key or a character key.
    assert_eq!(
        encode_key(
            &Key::Named(NamedKey::Space),
            Some(" "),
            ModifiersState::CONTROL,
            false
        ),
        Some(vec![0x00])
    );
    assert_eq!(
        encode_key(
            &Key::Character(" ".into()),
            Some(" "),
            ModifiersState::CONTROL,
            false
        ),
        Some(vec![0x00])
    );
}

#[test]
fn ctrl_symbols_and_digits_send_c0_bytes() {
    // Classic xterm mappings: Ctrl+[ = ESC, Ctrl+\ = FS, … Ctrl+2..8
    // mirror the symbol column.
    let cases: [(&str, u8); 13] = [
        ("@", 0x00),
        ("2", 0x00),
        ("[", 0x1b),
        ("3", 0x1b),
        ("\\", 0x1c),
        ("4", 0x1c),
        ("]", 0x1d),
        ("5", 0x1d),
        ("^", 0x1e),
        ("6", 0x1e),
        ("_", 0x1f),
        ("/", 0x1f),
        ("?", 0x7f),
    ];
    for (s, byte) in cases {
        assert_eq!(
            encode_key(
                &Key::Character(s.into()),
                Some(s),
                ModifiersState::CONTROL,
                false
            ),
            Some(vec![byte]),
            "ctrl+{s}"
        );
    }
}

#[test]
fn modified_backspace_and_enter() {
    // Alt+Backspace = ESC DEL (readline word delete), Ctrl+Backspace = BS.
    assert_eq!(
        encode_key(
            &Key::Named(NamedKey::Backspace),
            None,
            ModifiersState::ALT,
            false
        ),
        Some(vec![0x1b, 0x7f])
    );
    assert_eq!(
        encode_key(
            &Key::Named(NamedKey::Backspace),
            None,
            ModifiersState::CONTROL,
            false
        ),
        Some(vec![0x08])
    );
    assert_eq!(
        encode_key(
            &Key::Named(NamedKey::Enter),
            Some("\r"),
            ModifiersState::ALT,
            false
        ),
        Some(vec![0x1b, 0x0d])
    );
}

#[test]
fn composed_option_text_passes_through_without_esc() {
    // macOS Option composed a character (macos-option-as-alt off): the
    // caller passes alt_sends_esc = false and the composed text must not
    // gain an ESC prefix.
    assert_eq!(
        encode_key_with_modes(
            &Key::Character("å".into()),
            Some(&Key::Character("a".into())),
            None,
            Some("å"),
            ModifiersState::ALT,
            false,
            false,
            false,
            0,
            true,
            false,
        ),
        Some("å".as_bytes().to_vec())
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
            None,
            Some(PhysicalKey::Code(KeyCode::Numpad1)),
            Some("1"),
            ModifiersState::empty(),
            true,
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
            None,
            Some(PhysicalKey::Code(KeyCode::NumpadEnter)),
            Some("\r"),
            ModifiersState::empty(),
            true,
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
            None,
            Some(PhysicalKey::Code(KeyCode::Numpad1)),
            Some("1"),
            ModifiersState::empty(),
            true,
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
            None,
            Some(PhysicalKey::Code(KeyCode::NumpadEnter)),
            Some("\r"),
            ModifiersState::empty(),
            true,
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

// applescript AC-8: `input text` bytes match the clipboard-paste encoding —
// raw on the primary screen, ESC[200~/201~-wrapped under DECSET 2004.
#[test]
fn applescript_input_matches_paste_encoding() {
    assert_eq!(
        applescript_input_bytes("echo hi\n", false),
        Some(b"echo hi\n".to_vec())
    );
    assert_eq!(
        applescript_input_bytes("echo hi\n", true),
        Some(b"\x1b[200~echo hi\n\x1b[201~".to_vec())
    );
    assert_eq!(applescript_input_bytes("", false), None);
}

// applescript Amendment 1.5: an oversized `input text` payload is truncated to
// the cap on a UTF-8 boundary before encoding, never split mid-codepoint.
#[test]
fn applescript_input_caps_oversized_payload_on_char_boundary() {
    let cap = super::paste::APPLESCRIPT_INPUT_TEXT_CAP;
    // Multi-byte trailing chars: the cut must land on a boundary, so the
    // encoded length is at most the cap and always valid UTF-8.
    let text = "a".repeat(cap - 1) + "あ"; // 'あ' is 3 bytes, straddling the cap
    let bytes = applescript_input_bytes(&text, false).expect("non-empty");
    assert!(bytes.len() <= cap);
    assert!(std::str::from_utf8(&bytes).is_ok());
    // The final 'あ' is dropped whole (its bytes cross the cap), leaving only
    // the ASCII run.
    assert_eq!(bytes.len(), cap - 1);
}

// noa-server sendText paste:false: bytes pass through untouched, unlike the
// paste path which strips embedded bracket markers and can wrap in ESC[200~.
#[test]
fn raw_input_bytes_writes_text_unwrapped() {
    use super::paste::raw_input_bytes;
    assert_eq!(raw_input_bytes("\r"), Some(b"\r".to_vec()));
    assert_eq!(raw_input_bytes("echo hi\n"), Some(b"echo hi\n".to_vec()));
    // Unlike encode_paste, embedded bracket markers are left alone — this is
    // keyboard-like input, not a paste, so nothing sanitizes them.
    assert_eq!(
        raw_input_bytes("a\x1b[201~b"),
        Some(b"a\x1b[201~b".to_vec())
    );
    assert_eq!(raw_input_bytes(""), None);
}

#[test]
fn raw_input_bytes_caps_oversized_payload_on_char_boundary() {
    use super::paste::raw_input_bytes;
    let cap = super::paste::APPLESCRIPT_INPUT_TEXT_CAP;
    let text = "a".repeat(cap - 1) + "あ";
    let bytes = raw_input_bytes(&text).expect("non-empty");
    assert!(bytes.len() <= cap);
    assert!(std::str::from_utf8(&bytes).is_ok());
    assert_eq!(bytes.len(), cap - 1);
}

// ── Kitty keyboard protocol encoding ───────────────────────────────

/// Encode a press with the given Kitty flags (no physical key, not repeat).
fn kitty_press(
    logical: &Key,
    text: Option<&str>,
    mods: ModifiersState,
    flags: u8,
) -> Option<Vec<u8>> {
    encode_key_with_modes(
        logical, None, None, text, mods, true, false, false, flags, true, false,
    )
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
fn kitty_disambiguate_named_space_stays_text() {
    // winit can report Space as a named key rather than a character key.
    // A bare printable still takes the legacy text path when report-all is
    // disabled.
    assert_eq!(
        kitty_press(
            &Key::Named(NamedKey::Space),
            Some(" "),
            ModifiersState::empty(),
            KITTY_DISAMBIGUATE
        ),
        Some(b" ".to_vec())
    );
}

#[test]
fn kitty_disambiguate_modified_named_space_is_csi_u() {
    // Ctrl+Space has no printable legacy byte that preserves the modifier,
    // so Kitty disambiguation reports it as CSI 32;5u.
    assert_eq!(
        kitty_press(
            &Key::Named(NamedKey::Space),
            Some(" "),
            ModifiersState::CONTROL,
            KITTY_DISAMBIGUATE
        ),
        Some(b"\x1b[32;5u".to_vec())
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
fn kitty_report_all_keys_encodes_named_space() {
    // winit reports Space as a named key on some platforms. Under Kitty
    // report-all it must still emit a CSI-u space instead of being ignored.
    assert_eq!(
        kitty_press(
            &Key::Named(NamedKey::Space),
            Some(" "),
            ModifiersState::empty(),
            KITTY_REPORT_ALL_KEYS
        ),
        Some(b"\x1b[32u".to_vec())
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
fn kitty_shifted_symbol_reports_unshifted_base_key() {
    // Ctrl+Shift+1 produces '!' but the Kitty base key code must be the
    // unshifted layout key '1' (49), with '!' (33) as the shifted
    // alternate under the alternate-keys flag.
    assert_eq!(
        encode_key_with_modes(
            &Key::Character("!".into()),
            Some(&Key::Character("1".into())),
            None,
            Some("!"),
            ModifiersState::SHIFT | ModifiersState::CONTROL,
            true,
            false,
            false,
            KITTY_REPORT_ALTERNATE_KEYS,
            true,
            false,
        ),
        Some(b"\x1b[49:33;6u".to_vec())
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
            None,
            ModifiersState::empty(),
            true,
            false,
            false,
            KITTY_REPORT_EVENT_TYPES,
            false,
            // released
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
            None,
            ModifiersState::empty(),
            true,
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
            None,
            Some("a"),
            ModifiersState::empty(),
            true,
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
            None,
            Some(PhysicalKey::Code(KeyCode::ShiftLeft)),
            None,
            ModifiersState::SHIFT,
            true,
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
            None,
            Some(PhysicalKey::Code(KeyCode::ShiftLeft)),
            None,
            ModifiersState::SHIFT,
            true,
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
fn legacy_release_sends_nothing() {
    // flags=0: a released non-text key must not re-encode its press bytes
    // (Enter/Backspace/Ctrl+C would otherwise double-send).
    let cases: [(Key, Option<&str>, ModifiersState); 3] = [
        (
            Key::Named(NamedKey::Enter),
            Some("\r"),
            ModifiersState::empty(),
        ),
        (
            Key::Named(NamedKey::Backspace),
            None,
            ModifiersState::empty(),
        ),
        (
            Key::Character("c".into()),
            Some("c"),
            ModifiersState::CONTROL,
        ),
    ];
    for (logical, text, mods) in cases {
        assert!(
            encode_key_with_modes(
                &logical, None, None, text, mods, true, false, false, 0, true, false
            )
            .is_some(),
            "press {logical:?} should still send"
        );
        assert_eq!(
            encode_key_with_modes(
                &logical, None, None, text, mods, true, false, false, 0, false, false
            ),
            None,
            "release {logical:?} should send nothing"
        );
    }
}

#[test]
fn kitty_event_types_repeat_legacy_keys_but_drop_their_release() {
    // Event-types on, report-all off: keys that keep legacy/text encoding
    // must still repeat on OS auto-repeat, while their release is dropped.
    let flags = KITTY_DISAMBIGUATE | KITTY_REPORT_EVENT_TYPES; // 3
    assert_eq!(
        encode_key_with_modes(
            &Key::Character("a".into()),
            None,
            None,
            Some("a"),
            ModifiersState::empty(),
            true,
            false,
            false,
            flags,
            true,
            true, // repeat
        ),
        Some(b"a".to_vec())
    );
    assert_eq!(
        encode_key_with_modes(
            &Key::Named(NamedKey::Backspace),
            None,
            None,
            None,
            ModifiersState::empty(),
            true,
            false,
            false,
            flags,
            true,
            true, // repeat
        ),
        Some(vec![0x7f])
    );
    // Release of the same legacy keys stays silent.
    assert_eq!(
        encode_key_with_modes(
            &Key::Character("a".into()),
            None,
            None,
            Some("a"),
            ModifiersState::empty(),
            true,
            false,
            false,
            flags,
            false,
            false,
        ),
        None
    );
    assert_eq!(
        encode_key_with_modes(
            &Key::Named(NamedKey::Backspace),
            None,
            None,
            None,
            ModifiersState::empty(),
            true,
            false,
            false,
            flags,
            false,
            false,
        ),
        None
    );
    // A CSI-u key (Ctrl+A) still reports its repeat with the :2 suffix.
    assert_eq!(
        encode_key_with_modes(
            &Key::Character("a".into()),
            None,
            None,
            Some("a"),
            ModifiersState::CONTROL,
            true,
            false,
            false,
            flags,
            true,
            true,
        ),
        Some(b"\x1b[97;5:2u".to_vec())
    );
}

#[test]
fn kitty_keypad_uses_dedicated_codes_under_report_all() {
    assert_eq!(
        encode_key_with_modes(
            &Key::Character("1".into()),
            None,
            Some(PhysicalKey::Code(KeyCode::Numpad1)),
            Some("1"),
            ModifiersState::empty(),
            true,
            false,
            false,
            KITTY_REPORT_ALL_KEYS,
            true,
            false,
        ),
        Some(b"\x1b[57400u".to_vec())
    );
}
