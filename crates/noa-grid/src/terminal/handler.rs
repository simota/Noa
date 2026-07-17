use crate::cursor::{Cursor, ScrollRegion};
use crate::kitty_keyboard::SetMode;
use crate::osc::{
    CwdOsc, HyperlinkOsc, ShellIntegrationOsc, ShellIntegrationOscKind, handle_clipboard_osc,
    handle_color_osc, parse_cwd_osc, parse_hyperlink_osc, parse_notification_osc,
    parse_shell_integration_osc,
};
use noa_core::{CellAttrs, Color};
use noa_vt::{
    AsciiLines, Charset, CharsetSlot, CursorStyle as VtCursorStyle, DaKind, DsrKind, EraseDisplay,
    EraseLine, Handler, KittyAction, KittyGraphicsCommand, ModeRequest, SgrAttr,
    SixelGraphicsCommand,
};

use super::{ShellIntegrationMarkKind, Terminal};

impl Handler for Terminal {
    fn print(&mut self, c: char) {
        let autowrap = self.modes.autowrap();
        let grapheme_clustering = self.modes.grapheme_clustering();
        let c = self.charset.translate(c);
        self.active_mut().print(c, autowrap, grapheme_clustering);
    }

    /// Bulk fast path for ground-state text runs (Ghostty analog:
    /// `printString`): mode and charset lookups are hoisted out of the
    /// per-scalar loop, and ASCII sub-runs go through
    /// [`Screen::print_ascii_run`]'s chunked cell writes. Semantically
    /// identical to per-scalar [`Handler::print`].
    fn print_str(&mut self, s: &str) {
        let autowrap = self.modes.autowrap();
        let grapheme_clustering = self.modes.grapheme_clustering();
        if !self.charset.active_is_ascii() {
            // DEC Special Graphics (or any future set) rewrites scalars —
            // stay on the per-scalar path.
            for c in s.chars() {
                let c = self.charset.translate(c);
                self.active_mut().print(c, autowrap, grapheme_clustering);
            }
            return;
        }
        let screen = self.active_mut();
        let bytes = s.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i].is_ascii() {
                let end = bytes[i..]
                    .iter()
                    .position(|&b| !b.is_ascii())
                    .map_or(bytes.len(), |off| i + off);
                screen.print_ascii_run(&bytes[i..end], autowrap, grapheme_clustering);
                i = end;
            } else {
                let c = s[i..].chars().next().expect("s is valid UTF-8");
                if crate::screen::Screen::is_plain_wide(c) {
                    // CJK/emoji runs take the bulk width-2 path. The
                    // `take_while` classification doubles as the decode:
                    // each scalar reaches the screen without a second
                    // boundary scan + re-decode of the same text.
                    let mut consumed = 0usize;
                    let run = s[i..]
                        .chars()
                        .take_while(|&c| crate::screen::Screen::is_plain_wide(c))
                        .inspect(|c| consumed += c.len_utf8());
                    screen.print_wide_scalars(run, autowrap, grapheme_clustering);
                    i += consumed;
                } else {
                    // Combining/cluster scalars stay on the per-scalar path.
                    screen.print(c, autowrap, grapheme_clustering);
                    i += c.len_utf8();
                }
            }
        }
    }

    /// Bulk fast path for ground-state line floods (`text (CR)? LF`
    /// groups): the active screen applies as many whole lines as it can in
    /// one batched scroll ([`crate::screen::Screen::apply_ascii_line_batch`]),
    /// and any line it cannot take (cursor off the region bottom, margins,
    /// autowrap off, …) is replayed through the canonical per-line calls —
    /// so the batch is semantically identical to the default trait body.
    fn print_ascii_lines(&mut self, data: &[u8]) {
        let autowrap = self.modes.autowrap();
        let lnm = self.modes.linefeed_newline();
        let grapheme_clustering = self.modes.grapheme_clustering();
        let ascii_charset = self.charset.active_is_ascii();
        let mut rest = data;
        while !rest.is_empty() {
            if ascii_charset && autowrap {
                let consumed = self.active_mut().apply_ascii_line_batch(
                    rest,
                    autowrap,
                    lnm,
                    grapheme_clustering,
                );
                if consumed > 0 {
                    rest = &rest[consumed..];
                    continue;
                }
            }
            // Per-line replay (the default trait body's exact semantics)
            // until the batch preconditions hold again.
            let mut lines = AsciiLines::new(rest);
            let Some(line) = lines.next() else {
                // Unterminated tail: a contract violation, but print it like
                // the default trait body rather than dropping bytes.
                debug_assert!(false, "print_ascii_lines data holds only complete lines");
                let text =
                    core::str::from_utf8(rest).expect("print_ascii_lines text is printable ASCII");
                self.print_str(text);
                return;
            };
            if !line.text.is_empty() {
                let text = core::str::from_utf8(line.text)
                    .expect("print_ascii_lines text is printable ASCII");
                self.print_str(text);
            }
            if line.crlf {
                self.execute_c0(0x0d);
            }
            self.execute_c0(0x0a);
            rest = lines.remainder();
        }
    }

    /// `s` is caller-verified printable ASCII (see the trait doc): the
    /// per-byte ASCII/wide/combining dispatch `print_str` needs for mixed
    /// content collapses to a single [`Screen::print_ascii_run`] call —
    /// same DEC-Special-Graphics guard, since that remaps ASCII *bytes* to
    /// line-drawing glyphs regardless of their own ASCII-ness.
    fn print_ascii_str(&mut self, s: &str) {
        let autowrap = self.modes.autowrap();
        let grapheme_clustering = self.modes.grapheme_clustering();
        if !self.charset.active_is_ascii() {
            for c in s.chars() {
                let c = self.charset.translate(c);
                self.active_mut().print(c, autowrap, grapheme_clustering);
            }
            return;
        }
        self.active_mut()
            .print_ascii_run(s.as_bytes(), autowrap, grapheme_clustering);
    }

    fn execute_c0(&mut self, byte: u8) {
        match byte {
            0x0e => return self.locking_shift(CharsetSlot::G1), // SO
            0x0f => return self.locking_shift(CharsetSlot::G0), // SI
            0x07 => return self.bell(),                         // BEL — no grid-state side effect.
            _ => {}
        }
        let linefeed_newline = self.modes.linefeed_newline();
        let screen = self.active_mut();
        match byte {
            0x08 => screen.backspace(),
            0x09 => screen.tab(1),
            0x0a..=0x0c => {
                if linefeed_newline {
                    screen.carriage_return();
                }
                screen.index();
            }
            0x0d => screen.carriage_return(),
            _ => {}
        }
    }

    fn cursor_up(&mut self, n: u16) {
        self.active_mut().cursor_up(n);
    }
    fn cursor_down(&mut self, n: u16) {
        self.active_mut().cursor_down(n);
    }
    fn cursor_forward(&mut self, n: u16) {
        self.active_mut().cursor_forward(n);
    }
    fn cursor_backward(&mut self, n: u16) {
        self.active_mut().cursor_backward(n);
    }
    fn cursor_position(&mut self, row: u16, col: u16) {
        self.active_mut().cursor_position(row, col);
    }
    fn cursor_col_abs(&mut self, col: u16) {
        self.active_mut().cursor_col_abs(col);
    }
    fn cursor_row_abs(&mut self, row: u16) {
        self.active_mut().cursor_row_abs(row);
    }

    fn erase_display(&mut self, mode: EraseDisplay) {
        if matches!(mode, EraseDisplay::Scrollback) && self.active().scrollback_len() > 0 {
            self.invalidate_grid_coordinate_space();
        }
        self.active_mut().erase_display(mode);
    }
    fn erase_line(&mut self, mode: EraseLine) {
        self.active_mut().erase_line(mode);
    }

    fn screen_alignment_test(&mut self) {
        self.active_mut().screen_alignment_test();
    }

    fn set_attributes(&mut self, attrs: &[SgrAttr]) {
        self.apply_sgr(attrs);
    }

    fn set_cursor_style(&mut self, style: VtCursorStyle) {
        let default_style = self.default_cursor_style;
        self.active_mut().cursor.style = match style {
            VtCursorStyle::Default => default_style,
            VtCursorStyle::BlinkingBlock => crate::cursor::CursorStyle::BlinkingBlock,
            VtCursorStyle::SteadyBlock => crate::cursor::CursorStyle::SteadyBlock,
            VtCursorStyle::BlinkingUnderline => crate::cursor::CursorStyle::BlinkingUnderline,
            VtCursorStyle::SteadyUnderline => crate::cursor::CursorStyle::SteadyUnderline,
            VtCursorStyle::BlinkingBar => crate::cursor::CursorStyle::BlinkingBar,
            VtCursorStyle::SteadyBar => crate::cursor::CursorStyle::SteadyBar,
        };
    }

    fn set_horizontal_margins(&mut self, left: u16, right: u16) {
        if self.modes.left_right_margin() {
            self.active_mut().set_horizontal_margins(left, right);
        }
    }

    fn set_application_keypad(&mut self, on: bool) {
        self.modes.set(66, false, on);
    }

    fn request_mode(&mut self, request: ModeRequest) {
        let state = match (request.value, request.ansi) {
            (20, true)
            | (1, false)
            | (6, false)
            | (7, false)
            | (9, false)
            | (25, false)
            | (47, false)
            | (66, false)
            | (69, false)
            | (1004, false)
            | (1000, false)
            | (1002, false)
            | (1003, false)
            | (1005, false)
            | (1006, false)
            | (1007, false)
            | (1015, false)
            | (1047, false)
            | (1048, false)
            | (1049, false)
            | (2026, false)
            | (2027, false)
            | (2004, false) => {
                if self.modes.get(request.value, request.ansi) {
                    1
                } else {
                    2
                }
            }
            _ => 0,
        };
        if request.ansi {
            self.pending_writes
                .extend_from_slice(format!("\x1b[{};{}$y", request.value, state).as_bytes());
        } else {
            self.pending_writes
                .extend_from_slice(format!("\x1b[?{};{}$y", request.value, state).as_bytes());
        }
    }

    fn bell(&mut self) {
        self.pending_bell = true;
    }

    fn designate_charset(&mut self, slot: CharsetSlot, set: Charset) {
        self.charset.designate(slot, set);
    }

    fn locking_shift(&mut self, slot: CharsetSlot) {
        self.charset.shift(slot);
    }

    fn set_mode(&mut self, value: u16, ansi: bool, on: bool) {
        self.modes.set(value, ansi, on);
        if !ansi {
            match value {
                25 => self.active_mut().cursor.visible = on, // DECTCEM
                69 => {
                    if on {
                        self.active_mut().enable_horizontal_margins();
                    } else {
                        self.active_mut().disable_horizontal_margins();
                    }
                }
                47 => {
                    if on {
                        self.enter_alt_screen(false);
                    } else {
                        self.leave_alt_screen(false, false);
                    }
                }
                1047 => {
                    if on {
                        self.enter_alt_screen(false);
                    } else {
                        self.leave_alt_screen(false, true);
                    }
                }
                1048 => {
                    if on {
                        self.active_mut().save_cursor();
                    } else {
                        self.active_mut().restore_cursor();
                    }
                }
                1049 => {
                    if on {
                        self.primary.save_cursor();
                        self.enter_alt_screen(true);
                    } else {
                        self.leave_alt_screen(true, true);
                    }
                }
                _ => {}
            }
        }
    }

    fn carriage_return(&mut self) {
        self.active_mut().carriage_return();
    }
    fn linefeed(&mut self) {
        self.active_mut().index();
    }
    fn tab(&mut self, n: u16) {
        self.active_mut().tab(n);
    }
    fn tab_back(&mut self, n: u16) {
        self.active_mut().tab_back(n);
    }
    fn reverse_index(&mut self) {
        self.active_mut().reverse_index();
    }
    fn save_cursor(&mut self) {
        self.active_mut().save_cursor();
    }
    fn restore_cursor(&mut self) {
        self.active_mut().restore_cursor();
    }
    fn set_tab_stop(&mut self) {
        self.active_mut().set_tab_stop();
    }
    fn clear_tab_stop(&mut self) {
        self.active_mut().clear_tab_stop();
    }
    fn clear_all_tab_stops(&mut self) {
        self.active_mut().clear_all_tab_stops();
    }

    fn full_reset(&mut self) {
        self.invalidate_grid_coordinate_space();
        let scrollback_limit = self.primary.scrollback_limit_bytes();
        self.primary = crate::screen::Screen::new(self.size.cols, self.size.rows);
        self.primary.set_scrollback_limit_bytes(scrollback_limit);
        self.alt = None;
        self.active_is_alt = false;
        self.screen_generation = self.screen_generation.wrapping_add(1);
        self.modes = crate::modes::ModeState::defaults();
        self.charset = crate::charset::CharsetState::default();
        self.title.clear();
        self.cwd = None;
        self.hyperlinks.clear();
        self.hyperlink_index.clear();
        self.shell_marks.clear();
        self.colors.reset_dynamic_overrides();
        self.pending_clipboard_writes.clear();
        self.pending_clipboard_reads.clear();
        self.pending_notifications.clear();
        self.pending_bell = false;
        self.kitty_keyboard.reset();
        self.kitty_images.clear();
        self.clear_selection();
        self.clear_search();
    }

    fn soft_reset(&mut self) {
        // DECTCEM on, DECOM off — tracked bits only; screen content untouched.
        self.modes.set(25, false, true);
        self.modes.set(6, false, false);
        self.charset = crate::charset::CharsetState::default();
        let last_row = self.size.rows.saturating_sub(1);
        let screen = self.active_mut();
        screen.cursor.visible = true;
        screen.region = ScrollRegion {
            top: 0,
            bottom: last_row,
        };
        // Clears the margin value only; the DECLRMM capability bit (mode 69)
        // in `self.modes` is untouched.
        screen.disable_horizontal_margins();
        screen.cursor.fg = Color::Default;
        screen.cursor.bg = Color::Default;
        screen.cursor.underline_color = None;
        screen.cursor.attrs = CellAttrs::empty();
        // Next DECRC restores to the default position/attributes, not
        // whatever was saved before the reset.
        screen.saved_cursor = Some(Cursor::default().into());
    }

    fn insert_blank_chars(&mut self, n: u16) {
        self.active_mut().insert_blank_chars(n);
    }
    fn insert_lines(&mut self, n: u16) {
        self.active_mut().insert_lines(n);
    }
    fn delete_lines(&mut self, n: u16) {
        self.active_mut().delete_lines(n);
    }
    fn delete_chars(&mut self, n: u16) {
        self.active_mut().delete_chars(n);
    }
    fn scroll_up(&mut self, n: u16) {
        self.active_mut().scroll_up_region(n);
    }
    fn scroll_down(&mut self, n: u16) {
        self.active_mut().scroll_down_region(n);
    }
    fn erase_chars(&mut self, n: u16) {
        self.active_mut().erase_chars(n);
    }
    fn repeat_preceding_char(&mut self, n: u16) {
        let autowrap = self.modes.autowrap();
        let grapheme_clustering = self.modes.grapheme_clustering();
        self.active_mut()
            .repeat_preceding_char(n, autowrap, grapheme_clustering);
    }

    fn seed_set_last_printed(&mut self, ch: char) {
        self.active_mut().set_last_printed(ch);
    }

    fn seed_set_cursor_hollow(&mut self) {
        let cursor = &mut self.active_mut().cursor;
        cursor.style = match cursor.style {
            crate::cursor::CursorStyle::BlinkingBlock => {
                crate::cursor::CursorStyle::BlinkingBlockHollow
            }
            crate::cursor::CursorStyle::SteadyBlock => {
                crate::cursor::CursorStyle::SteadyBlockHollow
            }
            other => other,
        };
    }

    fn seed_set_default_cursor_style(&mut self, ps: u16, hollow: bool) {
        use crate::cursor::CursorStyle;
        // Mirrors the `DECSCUSR` numbering `write_cursor_style` emits, plus
        // the seed-only `hollow` bit for the two block variants standard
        // `DECSCUSR` cannot express (see `seed_set_cursor_hollow`).
        self.default_cursor_style = match ps {
            2 if hollow => CursorStyle::SteadyBlockHollow,
            2 => CursorStyle::SteadyBlock,
            3 => CursorStyle::BlinkingUnderline,
            4 => CursorStyle::SteadyUnderline,
            5 => CursorStyle::BlinkingBar,
            6 => CursorStyle::SteadyBar,
            1 if hollow => CursorStyle::BlinkingBlockHollow,
            _ => CursorStyle::BlinkingBlock,
        };
    }

    fn device_attributes(&mut self, kind: DaKind) {
        match kind {
            // DA1: "I am a VT220 with these features" (matches Ghostty's reply shape).
            DaKind::Primary => self.pending_writes.extend_from_slice(b"\x1b[?62;4;22c"),
            DaKind::Secondary => self.pending_writes.extend_from_slice(b"\x1b[>1;0;0c"),
        }
    }

    fn device_status_report(&mut self, kind: DsrKind) {
        match kind {
            DsrKind::Status => self.pending_writes.extend_from_slice(b"\x1b[0n"),
            DsrKind::CursorPosition => {
                let row = self.active().cursor.y + 1;
                let col = self.active().cursor.x + 1;
                self.pending_writes
                    .extend_from_slice(format!("\x1b[{row};{col}R").as_bytes());
            }
        }
    }

    fn window_op(&mut self, ps: u16, p1: u16, _p2: u16) {
        match ps {
            14 => self.pending_writes.extend_from_slice(
                format!(
                    "\x1b[4;{};{}t",
                    self.text_area_height_px, self.text_area_width_px
                )
                .as_bytes(),
            ),
            16 => self.pending_writes.extend_from_slice(
                format!("\x1b[6;{};{}t", self.cell_height_px, self.cell_width_px).as_bytes(),
            ),
            18 => self.pending_writes.extend_from_slice(
                format!("\x1b[8;{};{}t", self.size.rows, self.size.cols).as_bytes(),
            ),
            // Gated on `title-report` (default off, Ghostty parity): the
            // title is program-settable via OSC 0/2, so an unconditional
            // reply lets any displayed byte stream inject text into stdin.
            21 if self.title_report => {
                self.pending_writes.extend_from_slice(b"\x1b]l");
                self.pending_writes.extend_from_slice(self.title.as_bytes());
                self.pending_writes.extend_from_slice(b"\x1b\\");
            }
            // Ps[1] == 0 or 2 both mean "window title" (icon-title tracking
            // is unsupported); Ps[1] == 1 (icon-only) and anything else
            // falls through to the no-op/no-reply arm below.
            22 if matches!(p1, 0 | 2) => self.push_title(),
            23 if matches!(p1, 0 | 2) => self.pop_title(),
            _ => {} // 4/8/9/10/19/20, icon-only push/pop, unknown Ps — ignore (Ghostty parity).
        }
    }

    fn kitty_keyboard_query(&mut self) {
        let flags = self.kitty_keyboard.flags(self.active_is_alt);
        self.pending_writes
            .extend_from_slice(format!("\x1b[?{flags}u").as_bytes());
    }

    fn kitty_keyboard_push(&mut self, flags: u8) {
        self.kitty_keyboard.push(self.active_is_alt, flags);
    }

    fn kitty_keyboard_pop(&mut self, n: u16) {
        self.kitty_keyboard.pop(self.active_is_alt, n);
    }

    fn kitty_keyboard_set(&mut self, flags: u8, mode: u16) {
        self.kitty_keyboard
            .set(self.active_is_alt, flags, SetMode::from_param(mode));
    }

    fn osc_dispatch(&mut self, data: &[u8]) {
        if handle_color_osc(data, &mut self.colors, &mut self.pending_writes) {
            return;
        }
        if handle_clipboard_osc(
            data,
            &self.osc52_policy,
            &mut self.pending_clipboard_writes,
            &mut self.pending_clipboard_reads,
        ) {
            return;
        }
        if let Some(action) = parse_hyperlink_osc(data) {
            match action {
                HyperlinkOsc::Start(hyperlink) => self.set_current_hyperlink(hyperlink),
                HyperlinkOsc::End => self.clear_current_hyperlink(),
                HyperlinkOsc::Malformed => {}
            }
            return;
        }
        if let Some(notification) = parse_notification_osc(data) {
            self.push_notification(notification);
            return;
        }
        if let Some(action) = parse_cwd_osc(data) {
            match action {
                CwdOsc::Set(cwd) => self.cwd = Some(cwd),
                CwdOsc::Reset => self.cwd = None,
                CwdOsc::Malformed => {}
            }
            return;
        }
        if let Some(action) = parse_shell_integration_osc(data) {
            if let ShellIntegrationOsc::Mark { kind, exit_status } = action {
                let kind = match kind {
                    ShellIntegrationOscKind::PromptStart => ShellIntegrationMarkKind::PromptStart,
                    ShellIntegrationOscKind::InputStart => ShellIntegrationMarkKind::InputStart,
                    ShellIntegrationOscKind::CommandStart => ShellIntegrationMarkKind::CommandStart,
                    ShellIntegrationOscKind::CommandEnd => ShellIntegrationMarkKind::CommandEnd,
                };
                self.record_shell_mark(kind, exit_status);
            }
            return;
        }

        // OSC 0 (icon+title) / 2 (title): "<code>;<text>".
        let sep = data.iter().position(|&b| b == b';');
        if let Some(i) = sep {
            let code = &data[..i];
            if code == b"0" || code == b"2" {
                self.title = String::from_utf8_lossy(&data[i + 1..]).into_owned();
            }
        }
    }

    fn dcs_dispatch(&mut self, data: &[u8]) {
        if let Some(request) = data.strip_prefix(b"$q") {
            self.handle_decrqss(request);
        } else if let Some(payload) = data.strip_prefix(b"+q") {
            self.handle_xtgettcap(payload);
        }
    }

    fn sixel_graphics(&mut self, cmd: SixelGraphicsCommand) {
        Terminal::sixel_graphics(self, cmd);
    }

    fn xtversion_query(&mut self) {
        self.push_dcs_response(format!(">|noa {}", env!("CARGO_PKG_VERSION")).as_bytes());
    }

    fn set_modify_other_keys(&mut self, level: u16) {
        self.modify_other_keys_2 = level == 2;
    }

    fn kitty_graphics(&mut self, cmd: KittyGraphicsCommand) {
        // A truncated APC still identifies itself; reply EFBIG so the client
        // isn't left waiting on the response protocol.
        if cmd.truncated {
            self.kitty_images.abort();
            self.kitty_reply(
                cmd.image_id,
                cmd.image_number,
                cmd.image_id,
                cmd.placement_id,
                cmd.quiet,
                Err(crate::kitty::KittyError::TooBig),
            );
            return;
        }
        if cmd.parse_error {
            self.kitty_reply(
                cmd.image_id,
                cmd.image_number,
                cmd.image_id,
                cmd.placement_id,
                cmd.quiet,
                Err(crate::kitty::KittyError::Invalid),
            );
            return;
        }

        // A non-transmit command arriving mid-chunk aborts the pending transfer
        // (continuation chunks always parse as `Transmit`). Frame transfers
        // (`a=f`) are also chunkable, so they must not abort a pending one.
        if self.kitty_images.transfer_in_progress()
            && !matches!(
                cmd.action,
                KittyAction::Transmit
                    | KittyAction::TransmitAndDisplay
                    | KittyAction::TransmitFrame
            )
        {
            self.kitty_images.abort();
        }

        match cmd.action {
            KittyAction::Transmit
            | KittyAction::TransmitAndDisplay
            | KittyAction::Query
            | KittyAction::TransmitFrame => {
                self.kitty_transmit(cmd);
            }
            KittyAction::Animate => self.kitty_animate(&cmd),
            KittyAction::Compose => self.kitty_compose(&cmd),
            KittyAction::Put => self.kitty_put(&cmd),
            KittyAction::Delete => self.kitty_delete(&cmd),
        }
        // This is the sole entry point for every Kitty graphics action, so
        // resyncing the shared animation flag once here (rather than at each
        // `ImageStore` mutation site — transmit/animate/compose/delete all
        // funnel through the match above) can't miss a state change.
        self.kitty_images.sync_animation_flag();
    }

    fn set_scroll_region(&mut self, top: u16, bottom: u16) {
        let screen = self.active_mut();
        let last = screen.rows.saturating_sub(1);
        let t = top.saturating_sub(1).min(last);
        let b = if bottom == 0 {
            last
        } else {
            bottom.saturating_sub(1).min(last)
        };
        if t < b {
            screen.region = ScrollRegion { top: t, bottom: b };
            screen.cursor_position(1, 1); // DECSTBM homes the cursor after a valid region.
        }
    }
}
