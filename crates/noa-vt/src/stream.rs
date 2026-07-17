//! [`Stream`] — feeds bytes through a [`Parser`] and maps each [`Action`] onto
//! a [`Handler`]. This is the semantic layer: it knows what `CSI … H` *means*.

use std::sync::{Arc, Mutex};

use crate::action::Action;
use crate::csi::{Csi, Esc, Intermediates, MAX_INTERMEDIATES, MAX_PARAMS, Params, Separators};
use crate::handler::{
    Charset, CharsetSlot, CursorStyle, DaKind, DsrKind, EraseDisplay, EraseLine, Handler,
    ModeRequest,
};
use crate::parser::Parser;
use crate::sgr::{SgrAttr, parse_sgr_into};
use crate::state::State;

/// Parser storage shared only by streams whose state must be snapshotted at a
/// byte boundary, such as a raw terminal attach endpoint.
#[derive(Clone, Default)]
pub struct SharedParser(Arc<Mutex<Parser>>);

impl SharedParser {
    pub fn pending_bytes(&self) -> Option<Vec<u8>> {
        self.0
            .lock()
            .expect("shared VT parser mutex poisoned")
            .pending_bytes()
    }
}

enum ParserStorage {
    Owned(Parser),
    Shared(SharedParser),
}

/// Owns a [`Parser`] and drives a [`Handler`] from a byte stream.
pub struct Stream {
    parser: ParserStorage,
    /// Reused across `SGR` dispatches so the hot colored-output path doesn't
    /// allocate a fresh `Vec` per escape sequence (see `parse_sgr_into`).
    sgr_attrs: Vec<SgrAttr>,
    /// Sticky "something visible may have changed" flag, see
    /// [`Stream::take_display_dirty`]. Set by every dispatched action except
    /// the pure report queries in [`is_pure_query`].
    display_dirty: bool,
}

impl Default for Stream {
    fn default() -> Self {
        Self {
            parser: ParserStorage::Owned(Parser::new()),
            sgr_attrs: Vec::new(),
            display_dirty: false,
        }
    }
}

impl Stream {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_shared_parser(parser: SharedParser) -> Self {
        Self {
            parser: ParserStorage::Shared(parser),
            sgr_attrs: Vec::new(),
            display_dirty: false,
        }
    }

    /// Feed a chunk of bytes, dispatching all resulting operations to `handler`.
    pub fn feed<H: Handler>(&mut self, bytes: &[u8], handler: &mut H) {
        match &mut self.parser {
            ParserStorage::Owned(parser) => feed_parser(
                parser,
                &mut self.sgr_attrs,
                bytes,
                handler,
                &mut self.display_dirty,
            ),
            ParserStorage::Shared(parser) => {
                let mut parser = parser.0.lock().expect("shared VT parser mutex poisoned");
                feed_parser(
                    &mut parser,
                    &mut self.sgr_attrs,
                    bytes,
                    handler,
                    &mut self.display_dirty,
                );
            }
        }
    }

    /// Whether anything fed since the last take could have changed *visible*
    /// terminal state, and reset the flag. `false` means every completed
    /// action so far was a pure report query ([`is_pure_query`]: DSR, DA,
    /// DECRQM, XTVERSION, the Kitty keyboard query) — sequences that only
    /// queue a reply and can never dirty a frame. Callers that pace repaints
    /// off pty output (the io thread) use this to skip waking the render
    /// path for query/reply round-trips: a probe or TUI polling `CSI 6 n` in
    /// a tight loop otherwise forces snapshot passes that repaint an
    /// unchanged frame *and* stretch the reply's own tail latency by
    /// contending on the terminal lock mid-burst.
    ///
    /// Conservative by construction: the flag is set by *every* other
    /// action, including unknown/ignored sequences and bytes still mid-parse
    /// on a later chunk — a misclassification can only cause a spurious
    /// repaint, never a stale frame.
    pub fn take_display_dirty(&mut self) -> bool {
        std::mem::take(&mut self.display_dirty)
    }
}

/// A CSI action that only reports state back to the application (the
/// terminal queues a reply; nothing on screen can change): DSR (`CSI n`),
/// DA1/DA2 (`CSI c` / `CSI > c`), DECRQM (`CSI ? Pd $ p`), XTVERSION
/// (`CSI > q`), and the Kitty keyboard query (`CSI ? u`). Everything else —
/// prints, C0, other CSI/ESC/OSC/DCS/APC, and unknown sequences — counts as
/// display-dirtying for [`Stream::take_display_dirty`].
fn is_pure_query(action: &Action) -> bool {
    let Action::CsiDispatch(csi) = action else {
        return false;
    };
    is_pure_query_csi(csi)
}

#[inline]
fn is_pure_query_csi(csi: &Csi) -> bool {
    match csi.final_byte {
        b'n' | b'c' => csi.intermediates().is_empty(),
        b'p' => csi.intermediates() == [b'$'],
        b'q' => csi.private == b'>' && csi.intermediates().is_empty(),
        b'u' => csi.private == b'?',
        _ => false,
    }
}

fn feed_parser<H: Handler>(
    parser: &mut Parser,
    sgr_attrs: &mut Vec<SgrAttr>,
    bytes: &[u8],
    handler: &mut H,
    display_dirty: &mut bool,
) {
    let mut i = 0;
    // Cached exclusive end of the printable run containing `i` (bytes in
    // `i..run_end` are all `is_run_byte`), plus whether that whole run is
    // pure ASCII. Caching both across DFA detours for invalid UTF-8 keeps
    // the run scan linear even on hostile input.
    let mut run_end = 0;
    let mut run_ascii = false;
    while i < bytes.len() {
        if parser.in_ground_plain() {
            // Fast path: in plain ground state every byte until the next C0
            // control (ESC included) is print data, so the dominant
            // bulk-output case hands whole decoded runs to
            // `Handler::print_str` and skips the per-byte DFA dispatch
            // entirely (Ghostty analog: `stream.zig`'s ground scan).
            if is_run_byte(bytes[i]) {
                if run_end <= i {
                    let (end, ascii) = scan_run(&bytes[i..]);
                    run_end = i + end;
                    run_ascii = ascii;
                }
                if run_ascii {
                    // SAFETY: `scan_run` only sets `ascii` when every byte in
                    // `bytes[i..run_end]` is `< 0x80`, which is always valid
                    // single-byte-per-scalar UTF-8, so skipping the redundant
                    // `from_utf8` re-scan of a range we already proved ASCII
                    // is sound.
                    let text = unsafe { core::str::from_utf8_unchecked(&bytes[i..run_end]) };
                    *display_dirty = true;
                    // `scan_run`'s `ascii` flag means every byte here is
                    // `0x20..=0x7e` (it only spans `is_run_byte` bytes,
                    // already `>= 0x20` and `!= 0x7f`, further narrowed to
                    // `< 0x80`) — the exact guarantee `print_ascii_str`
                    // needs to skip `print_str`'s internal re-classification
                    // of text this scan already proved ASCII.
                    handler.print_ascii_str(text);
                    i = run_end;
                    continue;
                }
                // SIMD validation (NEON/SSE): the run was already
                // boundary-scanned by `scan_run`, so this pass is purely
                // UTF-8 structure. `compat` keeps std's `valid_up_to`
                // semantics for the invalid-suffix path.
                match simdutf8::compat::from_utf8(&bytes[i..run_end]) {
                    Ok(text) => {
                        *display_dirty = true;
                        handler.print_str(text);
                        i = run_end;
                        continue;
                    }
                    Err(err) => {
                        // Bulk-print the valid prefix, then let the DFA own
                        // the invalid/incomplete sequence byte-by-byte below
                        // (it carries the replacement + cross-chunk resume
                        // semantics), re-entering this fast path once it
                        // returns to plain ground.
                        let valid = err.valid_up_to();
                        if valid > 0 {
                            let text = core::str::from_utf8(&bytes[i..i + valid])
                                .expect("valid_up_to marks a valid UTF-8 prefix");
                            *display_dirty = true;
                            handler.print_str(text);
                            i += valid;
                        }
                    }
                }
            } else if bytes[i] == 0x1b
                && bytes.get(i + 1) == Some(&b'[')
                && let Some((csi, len)) = try_scan_csi(&bytes[i..])
            {
                // Fast path: a complete plain CSI sequence inside the chunk
                // (SGR-dense floods are dominated by these) is lexed in one
                // tight scan and dispatched directly, skipping the per-byte
                // DFA entirely. `try_scan_csi` is pure lookahead: it bails
                // to the DFA on anything the DFA treats specially, so the
                // dispatched `Csi` is bit-identical to the per-byte path's.
                *display_dirty |= !is_pure_query_csi(&csi);
                dispatch_csi(&csi, handler, sgr_attrs);
                i += len;
                continue;
            } else if bytes[i] != 0x1b {
                // Fast path: `is_run_byte` already ruled out printable ASCII
                // and DEL, and this isn't the CSI lead byte, so what's left
                // is a lone C0 control (LF/CR/BS/TAB/BEL/…, the common case
                // in any line-oriented flood) or DEL. In ground state every
                // C0 byte dispatches straight to `Execute` with no state
                // change — `Parser::st_ground`'s own arm for it, plus the
                // "anywhere" CAN/SUB case, which forces the state back to
                // Ground and is therefore a no-op here since it already is
                // — and DEL is silently dropped. Skipping `advance` for this
                // byte skips its four dead prefix checks (state is
                // `Ground`, `utf8_rem` is `0`: both guaranteed by
                // `in_ground_plain`), the `c1_control` call (`b` can't be in
                // `0x80..=0x9f` here), and the closure-sink indirection;
                // observable behavior is byte-for-byte the same action.
                let b = bytes[i];
                if b != 0x7f {
                    *display_dirty = true; // Execute is never a pure query
                    handler.execute_c0(b);
                }
                i += 1;
                continue;
            }
        } else if parser.state() == State::CsiParam {
            // Batch parameter accumulation for a CSI resumed across a feed
            // boundary (or one the whole-sequence fast path bailed on): runs
            // of digits/`;`/`:` skip the per-byte DFA dispatch.
            let n = parser.scan_csi_params(&bytes[i..]);
            if n > 0 {
                i += n;
                continue;
            }
        }
        parser.advance(bytes[i], &mut |action| {
            *display_dirty |= !is_pure_query(&action);
            dispatch(action, handler, sgr_attrs);
        });
        i += 1;
    }
}

/// Attempt to lex one complete CSI sequence starting at `bytes[0] == ESC`,
/// `bytes[1] == '['` (caller-checked), entirely within `bytes`. Pure
/// lookahead: nothing is committed unless the whole sequence — through its
/// final byte — is present and consists only of bytes whose DFA handling is
/// plain collect/param/dispatch. Any byte the DFA treats specially mid-CSI
/// (C0 executes, CAN/SUB cancels, a second ESC, DEL, 8-bit bytes, the
/// param-overflow and intermediate-overflow quirks, `CsiIgnore` transitions)
/// returns `None`, and the caller feeds the same bytes through the per-byte
/// DFA unchanged — so observable behavior is byte-for-byte identical.
#[inline]
fn try_scan_csi(bytes: &[u8]) -> Option<(Csi, usize)> {
    debug_assert!(bytes.len() >= 2 && bytes[0] == 0x1b && bytes[1] == b'[');
    let mut i = 2;
    let mut params = Params::default();
    let mut sep_colon = Separators::default();
    let mut intermediates = Intermediates::default();
    let mut private = 0u8;
    // Private marker — only valid immediately after `[` (CsiEntry).
    if let Some(&b @ 0x3c..=0x3f) = bytes.get(i) {
        private = b;
        i += 1;
    }
    // Parameter bytes. The value in flight accumulates in a register and is
    // committed per separator / at sequence end, which lands in the same
    // `Params` the per-byte `param_digit` / `param_sep` pair builds.
    let mut cur: u16 = 0;
    let mut any_params = false;
    loop {
        let b = *bytes.get(i)?;
        match b {
            0x30..=0x39 => {
                cur = cur.saturating_mul(10).saturating_add(u16::from(b - 0x30));
                any_params = true;
            }
            0x3a | 0x3b => {
                params.push(cur);
                if params.len() >= MAX_PARAMS {
                    // The DFA's overflow quirk folds later digits into the
                    // last param; defer to it so that path stays byte-exact.
                    return None;
                }
                sep_colon.push(b == 0x3a);
                cur = 0;
                any_params = true;
            }
            _ => break,
        }
        i += 1;
    }
    if any_params {
        params.push(cur);
    }
    // Intermediate bytes, then the final byte.
    loop {
        let b = *bytes.get(i)?;
        match b {
            0x20..=0x2f => {
                if intermediates.len() >= MAX_INTERMEDIATES {
                    return None; // DFA drops extras; defer to keep it byte-exact
                }
                intermediates.push(b);
                i += 1;
            }
            0x40..=0x7e => {
                return Some((
                    Csi::from_parts(params, sep_colon, intermediates, private, b),
                    i + 1,
                ));
            }
            _ => return None,
        }
    }
}

/// A byte that stays on the ground-state print path: anything but a C0
/// control or DEL. `0x80..=0xff` are UTF-8 sequence bytes (the parser never
/// treats raw C1 bytes as controls in ground).
#[inline]
fn is_run_byte(b: u8) -> bool {
    b >= 0x20 && b != 0x7f
}

/// Every byte repeated into all 8 lanes of a `u64`, so a single `u64`
/// arithmetic op tests all 8 bytes of a word at once (word-at-a-time / SWAR).
const ONES: u64 = 0x0101_0101_0101_0101;
/// The high bit of every lane — where the byte-wise comparison tricks below
/// park their "matched" flag.
const HIGH: u64 = 0x8080_8080_8080_8080;

/// Scan the printable run starting at `bytes[0]` (caller guarantees
/// `bytes[0]` itself is a run byte). Returns `(run_end, ascii)`: `run_end` is
/// the exclusive offset of the first non-run byte (control byte, `DEL`, or
/// end of slice) — identical to
/// `bytes.iter().position(|&b| !is_run_byte(b)).unwrap_or(bytes.len())`, and
/// `ascii` is `true` iff every byte in `bytes[..run_end]` is `< 0x80`.
///
/// Merges what used to be two separate linear passes (the boundary search,
/// then a `from_utf8` re-validation of the same range) into one, and
/// processes 8 bytes at a time via SWAR bit tricks instead of a per-byte
/// closure. See "Bit Twiddling Hacks" (Sean Eron Anderson),
/// "Determine if a word has a byte less than n" / "...has a byte equal to n",
/// for the `haslessthan`/`haszero` formulas used below.
#[inline]
pub(crate) fn scan_run(bytes: &[u8]) -> (usize, bool) {
    let len = bytes.len();
    let mut i = 0;
    let mut nonascii = false;
    while i + 8 <= len {
        let chunk = u64::from_le_bytes(bytes[i..i + 8].try_into().unwrap());
        // haslessthan(chunk, 0x20): each lane's HIGH bit set iff that byte < 0x20.
        let lt20 = chunk.wrapping_sub(ONES * 0x20) & !chunk & HIGH;
        // haszero(chunk ^ 0x7f): each lane's HIGH bit set iff that byte == 0x7f.
        let xor7f = chunk ^ (ONES * 0x7f);
        let eq7f = xor7f.wrapping_sub(ONES) & !xor7f & HIGH;
        let boundary = lt20 | eq7f;
        if boundary != 0 {
            // Lowest set lane == first (lowest-address) boundary byte, since
            // `from_le_bytes` maps byte k to bits [8k, 8k+8) and each lane's
            // flag lives at bit 8k+7.
            let lane = (boundary.trailing_zeros() / 8) as usize;
            for &b in &bytes[i..i + lane] {
                nonascii |= b >= 0x80;
            }
            return (i + lane, !nonascii);
        }
        nonascii |= chunk & HIGH != 0;
        i += 8;
    }
    while i < len && is_run_byte(bytes[i]) {
        nonascii |= bytes[i] >= 0x80;
        i += 1;
    }
    (i, !nonascii)
}

fn dispatch<H: Handler>(action: Action, h: &mut H, sgr_attrs: &mut Vec<SgrAttr>) {
    match action {
        Action::Print(c) => h.print(c),
        Action::Execute(b) => h.execute_c0(b),
        Action::CsiDispatch(csi) => dispatch_csi(&csi, h, sgr_attrs),
        Action::EscDispatch(esc) => dispatch_esc(&esc, h),
        Action::OscDispatch(data) => h.osc_dispatch(&data),
        Action::DcsDispatch(payload) => {
            if let Some(cmd) = crate::sixel::parse(&payload.data) {
                h.sixel_graphics(cmd);
            } else {
                h.dcs_dispatch(&payload.data);
            }
        }
        Action::ApcDispatch { data, truncated } => {
            // Only Kitty graphics (`G`) is captured; other APC strings are dropped.
            if let [b'G', rest @ ..] = data.as_slice() {
                h.kitty_graphics(crate::kitty_graphics::parse(rest, truncated));
            }
        }
    }
}

fn dispatch_csi<H: Handler>(csi: &Csi, h: &mut H, sgr_attrs: &mut Vec<SgrAttr>) {
    let plain = csi.private == 0 && csi.intermediates().is_empty();
    match csi.final_byte {
        b'@' if plain => h.insert_blank_chars(csi.param(0, 1)),
        b'A' => h.cursor_up(csi.param(0, 1)),
        b'B' | b'e' => h.cursor_down(csi.param(0, 1)),
        b'C' | b'a' => h.cursor_forward(csi.param(0, 1)),
        b'D' => h.cursor_backward(csi.param(0, 1)),
        b'E' => h.cursor_next_line(csi.param(0, 1)),
        b'F' => h.cursor_prev_line(csi.param(0, 1)),
        b'G' | b'`' => h.cursor_col_abs(csi.param(0, 1)),
        b'I' if plain => h.tab(csi.param(0, 1)),
        b'L' if plain => h.insert_lines(csi.param(0, 1)),
        b'M' if plain => h.delete_lines(csi.param(0, 1)),
        b'P' if plain => h.delete_chars(csi.param(0, 1)),
        b'S' if plain => h.scroll_up(csi.param(0, 1)),
        b'T' if plain => h.scroll_down(csi.param(0, 1)),
        b'X' if plain => h.erase_chars(csi.param(0, 1)),
        b'Z' if plain => h.tab_back(csi.param(0, 1)),
        b'b' if plain => h.repeat_preceding_char(csi.param(0, 1)),
        b'd' => h.cursor_row_abs(csi.param(0, 1)),
        b'H' | b'f' => h.cursor_position(csi.param(0, 1), csi.param(1, 1)),
        b'J' => h.erase_display(match csi.param(0, 0) {
            1 => EraseDisplay::Above,
            2 => EraseDisplay::Complete,
            3 => EraseDisplay::Scrollback,
            _ => EraseDisplay::Below,
        }),
        b'K' => h.erase_line(match csi.param(0, 0) {
            1 => EraseLine::Left,
            2 => EraseLine::Complete,
            _ => EraseLine::Right,
        }),
        b'm' if plain => {
            // Lone truecolor pen (`38;2;r;g;b` / `48;2;r;g;b`) — the
            // per-cell shape SGR-dense floods emit two of per cell.
            // Dispatch straight off the stack, skipping the shared vec's
            // clear/push/deref round-trip; the decoded attr is identical to
            // `parse_sgr_into`'s fast arm (same slots, both separator
            // forms).
            if let [code @ (38 | 48), 2, r, g, b] = *csi.params() {
                let color = noa_core::Color::Rgb(noa_core::Rgb::new(r as u8, g as u8, b as u8));
                let attr = if code == 38 {
                    SgrAttr::Fg(color)
                } else {
                    SgrAttr::Bg(color)
                };
                h.set_attributes(&[attr]);
                return;
            }
            parse_sgr_into(csi, sgr_attrs);
            h.set_attributes(sgr_attrs);
        }
        // XTMODKEYS `CSI > Pp ; Pv m` sets an xterm key-modifier resource;
        // `CSI > Pp m` (and bare `CSI > m`) resets it. Only modifyOtherKeys
        // (Pp=4) is tracked. Must not fall through to SGR: `CSI > 4;2 m`
        // read as SGR is underline-on + faint, sticking underline on every
        // cell printed afterwards.
        b'm' if csi.private == b'>' => {
            if csi.params().is_empty() || csi.param(0, 0) == 4 {
                h.set_modify_other_keys(csi.param(1, 0));
            }
        }
        b'p' if csi.intermediates() == [b'$'] => {
            h.request_mode(ModeRequest {
                value: csi.param(0, 0),
                ansi: csi.private != b'?',
            });
        }
        b'p' if csi.intermediates() == [b'!'] => h.soft_reset(), // DECSTR
        b'q' if csi.private == 0 && csi.intermediates() == [b' '] => {
            // `param` collapses an explicit `0` to its default, but DECSCUSR 0
            // must reset to the configured default, so match the raw param:
            // an explicit `0` is `Default`, an absent param is blinking block.
            let style = match csi.params().first().copied() {
                Some(0) => CursorStyle::Default,
                Some(3) => CursorStyle::BlinkingUnderline,
                Some(4) => CursorStyle::SteadyUnderline,
                Some(5) => CursorStyle::BlinkingBar,
                Some(6) => CursorStyle::SteadyBar,
                Some(2) => CursorStyle::SteadyBlock,
                _ => CursorStyle::BlinkingBlock,
            };
            h.set_cursor_style(style);
        }
        // Client-mode seed-only: see `Handler::seed_set_default_cursor_style`.
        // The `$` intermediate keeps this clear of `CSI > q` / `CSI > 0 q`
        // (XTVERSION) below, which has no intermediate of its own.
        b'q' if csi.private == b'>' && csi.intermediates() == b"$" => {
            h.seed_set_default_cursor_style(csi.param(0, 1), csi.param(1, 0) != 0);
        }
        b'q' if csi.private == b'>' && csi.intermediates().is_empty() => h.xtversion_query(),
        b'h' | b'l' => {
            let on = csi.final_byte == b'h';
            let ansi = csi.private != b'?';
            for &value in csi.params() {
                h.set_mode(value, ansi, on);
            }
        }
        b'c' => match csi.private {
            0 => h.device_attributes(DaKind::Primary),
            b'>' => h.device_attributes(DaKind::Secondary),
            _ => {}
        },
        b'n' => match csi.param(0, 0) {
            5 => h.device_status_report(DsrKind::Status),
            6 => h.device_status_report(DsrKind::CursorPosition),
            _ => {}
        },
        b'r' if csi.private == 0 => h.set_scroll_region(csi.param(0, 1), csi.param(1, 0)),
        b's' if csi.private == 0 && csi.params().is_empty() => h.save_cursor(),
        b's' if csi.private == 0 => h.set_horizontal_margins(csi.param(0, 1), csi.param(1, 0)),
        // Client-mode seed-only: see `Handler::seed_set_last_printed`. The
        // `$` intermediate keeps this clear of xterm's `CSI > Ps s`
        // (XTSHIFTESCAPE) namespace.
        b's' if csi.private == b'>' && csi.intermediates() == b"$" => {
            let codepoint = (u32::from(csi.param(0, 0)) << 16) | u32::from(csi.param(1, 0));
            if let Some(ch) = char::from_u32(codepoint) {
                h.seed_set_last_printed(ch);
            }
        }
        // Plain `CSI u` is SCORC (restore cursor). The private markers select
        // the Kitty keyboard protocol progressive-enhancement operations.
        b'u' if csi.private == 0 && csi.intermediates().is_empty() => h.restore_cursor(),
        b'u' if csi.private == b'?' => h.kitty_keyboard_query(),
        b'u' if csi.private == b'>' => h.kitty_keyboard_push(csi.param(0, 0) as u8),
        b'u' if csi.private == b'<' => h.kitty_keyboard_pop(csi.param(0, 1)),
        b'u' if csi.private == b'=' => {
            h.kitty_keyboard_set(csi.param(0, 0) as u8, csi.param(1, 1));
        }
        b'g' if plain => match csi.param(0, 0) {
            0 => h.clear_tab_stop(),
            3 => h.clear_all_tab_stops(),
            _ => {}
        },
        b't' if plain => h.window_op(csi.param(0, 0), csi.param(1, 0), csi.param(2, 0)), // XTWINOPS
        // Client-mode seed-only: see `Handler::seed_set_cursor_hollow`. The
        // `$` intermediate keeps this clear of xterm's `CSI > Ps ; Ps t`
        // (title-mode set) namespace.
        b't' if csi.private == b'>' && csi.intermediates() == b"$" => h.seed_set_cursor_hollow(),
        _ => {} // unknown / inc>=2
    }
}

fn dispatch_esc<H: Handler>(esc: &Esc, h: &mut H) {
    match esc.intermediates() {
        [] => match esc.final_byte {
            b'c' => h.full_reset(),                  // RIS
            b'7' => h.save_cursor(),                 // DECSC
            b'8' => h.restore_cursor(),              // DECRC
            b'=' => h.set_application_keypad(true),  // DECPAM
            b'>' => h.set_application_keypad(false), // DECPNM
            b'M' => h.reverse_index(),               // RI
            b'D' => h.linefeed(),                    // IND (index, no CR)
            b'E' => {
                // NEL
                h.carriage_return();
                h.linefeed();
            }
            b'H' => h.set_tab_stop(), // HTS
            _ => {}
        },
        // SCS: `ESC ( x` designates G0, `ESC ) x` designates G1.
        [b'('] => h.designate_charset(CharsetSlot::G0, charset_from(esc.final_byte)),
        [b')'] => h.designate_charset(CharsetSlot::G1, charset_from(esc.final_byte)),
        [b'#'] if esc.final_byte == b'8' => h.screen_alignment_test(), // DECALN
        _ => {} // DECDHL/DECSWL etc. — no-op (out of scope)
    }
}

/// Map an `SCS` final byte to the [`Charset`] it designates. Lite scope only
/// distinguishes ASCII vs DEC Special Graphics; every other final byte (UK,
/// …) falls back to ASCII.
fn charset_from(final_byte: u8) -> Charset {
    match final_byte {
        b'0' => Charset::DecSpecialGraphics,
        _ => Charset::Ascii,
    }
}
