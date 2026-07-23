use std::fmt::Write;

use noa_core::{CellAttrs, Color, Rgb};
use noa_vt::{Charset, CharsetSlot};

use crate::cell::Cell;
use crate::cursor::{Cursor, CursorStyle, SavedCursor};
use crate::screen::Screen;

use super::Terminal;
use crate::TerminalProgress;

/// DEC private modes that affect client-mode input encoding or the replayed
/// screen state. Alternate-screen selection is emitted separately because it
/// must happen before repainting the visible grid.
const REPLAYED_PRIVATE_MODES: [u16; 17] = [
    1,    // DECCKM
    6,    // DECOM
    7,    // DECAWM
    9,    // X10 mouse tracking
    25,   // DECTCEM
    66,   // application keypad
    1000, // normal mouse tracking
    1002, // button-event mouse tracking
    1003, // any-event mouse tracking
    1004, // focus reporting
    1005, // UTF-8 mouse coordinates
    1006, // SGR mouse coordinates
    1007, // alternate scroll
    1015, // urxvt mouse coordinates
    2004, // bracketed paste
    2026, // synchronized output
    2027, // grapheme clustering
];

impl Terminal {
    /// Encode the visible terminal state as a synthetic VT repaint stream.
    ///
    /// Feeding the returned bytes through [`noa_vt::Stream`] into a fresh
    /// [`Terminal`] of the same size reconstructs the visible cells and the VT
    /// state needed by client-mode attachment. Scrollback, most window
    /// metadata, images, and hyperlinks are intentionally outside this seed;
    /// task progress is included because it is live session UI state.
    pub fn synthetic_seed(&self) -> Vec<u8> {
        let mut seed = String::new();
        seed.push_str("\x1bc"); // RIS: make the seed independent of replica state.
        write_dynamic_colors(&mut seed, &self.colors);

        // When the source is already inside a synchronized update, keep the
        // synthetic repaint hidden too and leave it held until the matching
        // live DECRST arrives. The mode is replayed again below with the rest
        // of the persistent state so the final seed is self-describing.
        write_private_mode(&mut seed, 2026, self.modes.synchronized_output());

        // Repainting must not wrap, use origin-relative addressing, or translate
        // ASCII through a pre-existing DEC Special Graphics designation.
        write_private_mode(&mut seed, 6, false);
        write_private_mode(&mut seed, 7, false);
        write_private_mode(&mut seed, 2027, true);
        seed.push_str("\x1b(B\x1b)B\x0f");
        write_kitty_keyboard_stack(&mut seed, self.kitty_keyboard.stack_values(false));

        if self.active_is_alt {
            // The primary screen is hidden now, but the next live `?1049l`
            // or equivalent makes it visible without repainting it. Seed it
            // first so leaving vim/another alternate-screen TUI cannot reveal
            // a blank replica screen.
            write_screen_snapshot(&mut seed, &self.primary, self.modes.autowrap());

            let mode = if self.modes.get(1049, false) {
                1049
            } else if self.modes.get(1047, false) {
                1047
            } else {
                47
            };
            write_private_mode(&mut seed, mode, true);
            write_private_mode(&mut seed, 6, false);
            write_private_mode(&mut seed, 7, false);
            seed.push_str("\x1b(B\x1b)B\x0f");
            write_kitty_keyboard_stack(&mut seed, self.kitty_keyboard.stack_values(true));
        } else {
            // DECSET 47 selects the retained alternate buffer without clearing
            // it or changing the primary saved cursor. Rebuild the hidden
            // screen even while primary is visible so a later live `?47h`
            // reveals the same retained contents and geometry.
            write_private_mode(&mut seed, 47, true);
            write_private_mode(&mut seed, 6, false);
            write_private_mode(&mut seed, 7, false);
            seed.push_str("\x1b(B\x1b)B\x0f");
            write_kitty_keyboard_stack(&mut seed, self.kitty_keyboard.stack_values(true));
            if let Some(alt) = &self.alt {
                write_screen_snapshot(&mut seed, alt, self.modes.autowrap());
            }
            write_private_mode(&mut seed, 47, false);
            write_private_mode(&mut seed, 6, false);
            write_private_mode(&mut seed, 7, false);
            seed.push_str("\x1b(B\x1b)B\x0f");
        }

        let screen = self.active();
        write_screen_snapshot(&mut seed, screen, self.modes.autowrap());

        for mode in REPLAYED_PRIVATE_MODES {
            let enabled = if mode == 25 {
                screen.cursor.visible
            } else {
                self.modes.get(mode, false)
            };
            write_private_mode(&mut seed, mode, enabled);
        }
        write_ansi_mode(&mut seed, 20, self.modes.linefeed_newline());

        // Restoring a screen without explicit horizontal margins disables
        // DECLRMM. Preserve the global mode when the active screen uses the
        // full width so subsequent live DECSLRM sequences keep their meaning.
        if screen.horizontal_margins.is_none()
            && self.modes.left_right_margin()
            && !restore_declrmm_from_inactive_screen(&mut seed, self)
        {
            write_private_mode(&mut seed, 69, true);
        }

        write_charset(&mut seed, self.charset.designations());
        if self.modify_other_keys_2 {
            seed.push_str("\x1b[>4;2m");
        }
        write_default_cursor_style(&mut seed, self.default_cursor_style);
        write_progress(&mut seed, self.progress);

        seed.into_bytes()
    }
}

fn write_progress(seed: &mut String, progress: Option<TerminalProgress>) {
    let Some(progress) = progress else {
        return;
    };
    match progress {
        TerminalProgress::Normal(value) => write!(seed, "\x1b]9;4;1;{}\x1b\\", value.get()),
        TerminalProgress::Error(Some(value)) => {
            write!(seed, "\x1b]9;4;2;{}\x1b\\", value.get())
        }
        TerminalProgress::Error(None) => write!(seed, "\x1b]9;4;2\x1b\\"),
        TerminalProgress::Indeterminate => write!(seed, "\x1b]9;4;3;0\x1b\\"),
        TerminalProgress::Paused(Some(value)) => {
            write!(seed, "\x1b]9;4;4;{}\x1b\\", value.get())
        }
        TerminalProgress::Paused(None) => write!(seed, "\x1b]9;4;4\x1b\\"),
    }
    .expect("writing to String cannot fail");
}

fn restore_declrmm_from_inactive_screen(seed: &mut String, terminal: &Terminal) -> bool {
    let inactive = if terminal.active_is_alt {
        Some(&terminal.primary)
    } else {
        terminal.alt.as_ref()
    };
    let Some(inactive) = inactive.filter(|screen| screen.horizontal_margins.is_some()) else {
        return false;
    };

    // DECLRMM is global but enabling it materializes margins on the current
    // Screen. Toggle to the inactive screen, restore its already-present
    // margins and cursor, then return without changing the active screen's
    // intentional `None` geometry.
    write_private_mode(seed, 47, !terminal.active_is_alt);
    write_horizontal_margins(seed, inactive);
    write_cursor(seed, inactive, inactive.cursor, terminal.modes.autowrap());
    write_cursor_style_exact(seed, inactive.cursor.style);
    write_private_mode(seed, 47, terminal.active_is_alt);
    true
}

fn write_screen_snapshot(seed: &mut String, screen: &Screen, autowrap: bool) {
    // Painting and saved-cursor reconstruction use absolute CUP coordinates.
    // Disable DECLRMM first so a saved coordinate outside the later margins
    // is not clamped before DECSC captures it.
    write_private_mode(seed, 69, false);
    write_screen_geometry(seed, screen);
    if let Some(saved) = screen.saved_cursor {
        write_saved_cursor(seed, screen, saved, autowrap);
    }
    write_horizontal_margins(seed, screen);
    write_cursor(seed, screen, screen.cursor, autowrap);
    write_cursor_style_exact(seed, screen.cursor.style);

    // Restore this screen's REP (`CSI b`) state last, after every other
    // print-through-the-grid write above, so it isn't clobbered by
    // whichever cell the visible-grid repaint happened to touch last (see
    // `Handler::seed_set_last_printed`).
    if let Some(ch) = screen.last_printed() {
        write_seed_last_printed(seed, ch);
    }
}

fn write_dynamic_colors(seed: &mut String, colors: &crate::osc::TerminalColors) {
    for index in u8::MIN..=u8::MAX {
        if let Some(rgb) = colors.palette(index) {
            write!(seed, "\x1b]4;{index};").expect("writing to String cannot fail");
            write_osc_rgb(seed, rgb);
        }
    }
    for (code, rgb) in [
        (10, colors.default_fg()),
        (11, colors.default_bg()),
        (12, colors.cursor()),
    ] {
        if let Some(rgb) = rgb {
            write!(seed, "\x1b]{code};").expect("writing to String cannot fail");
            write_osc_rgb(seed, rgb);
        }
    }
}

fn write_osc_rgb(seed: &mut String, rgb: Rgb) {
    write!(seed, "#{:02x}{:02x}{:02x}\x1b\\", rgb.r, rgb.g, rgb.b)
        .expect("writing to String cannot fail");
}

fn write_screen_geometry(seed: &mut String, screen: &Screen) {
    write_visible_grid(seed, screen);
    write_tabstops(seed, screen);
    write!(seed, "\x1b[1;{}r", screen.rows).expect("writing to String cannot fail");
    write_wrapped_rows(seed, screen);
    write!(
        seed,
        "\x1b[{};{}r",
        screen.region.top.saturating_add(1),
        screen.region.bottom.saturating_add(1)
    )
    .expect("writing to String cannot fail");
}

fn write_horizontal_margins(seed: &mut String, screen: &Screen) {
    let Some(margins) = screen.horizontal_margins else {
        write_private_mode(seed, 69, false);
        return;
    };
    write_private_mode(seed, 69, true);
    write!(
        seed,
        "\x1b[{};{}s",
        margins.left.saturating_add(1),
        margins.right.saturating_add(1)
    )
    .expect("writing to String cannot fail");
}

fn write_wrapped_rows(seed: &mut String, screen: &Screen) {
    if screen.cols == 0 || screen.grid.len() < 2 {
        return;
    }
    write_private_mode(seed, 7, true);
    for y in 0..screen.grid.len() - 1 {
        if !screen.grid[y].wrapped {
            continue;
        }
        let (x, last) = pending_wrap_cell(screen, screen.cols - 1, y as u16);
        write_cup(seed, x, y as u16);
        write_pen(seed, last.fg, last.bg, last.underline_color, last.attrs);
        last.push_text_to(seed);

        // SGR does not clear the deferred-wrap latch. Printing the next row's
        // existing first cell therefore recreates the logical soft-wrap
        // boundary without changing the visible contents.
        let first = &screen.grid[y + 1].cells[0];
        write_pen(seed, first.fg, first.bg, first.underline_color, first.attrs);
        first.push_text_to(seed);
    }
    write_private_mode(seed, 7, false);
}

fn write_kitty_keyboard_stack(seed: &mut String, stack: &[u8]) {
    for flags in stack {
        write!(seed, "\x1b[>{flags}u").expect("writing to String cannot fail");
    }
}

fn write_visible_grid(seed: &mut String, screen: &Screen) {
    for (y, row) in screen.grid.iter().enumerate() {
        for (x, cell) in row.cells.iter().enumerate() {
            if cell.attrs.contains(CellAttrs::WIDE_SPACER) || cell == &Cell::default() {
                continue;
            }
            write_cup(seed, x as u16, y as u16);
            write_pen(seed, cell.fg, cell.bg, cell.underline_color, cell.attrs);
            cell.push_text_to(seed);
        }
    }
}

fn write_tabstops(seed: &mut String, screen: &Screen) {
    seed.push_str("\x1b[3g");
    for column in screen.tabstops.positions() {
        write_cup(seed, column, 0);
        seed.push_str("\x1bH");
    }
}

fn write_saved_cursor(seed: &mut String, screen: &Screen, cursor: SavedCursor, autowrap: bool) {
    write_cursor_state(
        seed,
        screen,
        cursor.x,
        cursor.y,
        cursor.pending_wrap,
        cursor.fg,
        cursor.bg,
        cursor.underline_color,
        cursor.attrs,
        autowrap,
    );
    seed.push_str("\x1b7");
}

fn write_cursor(seed: &mut String, screen: &Screen, cursor: Cursor, autowrap: bool) {
    write_cursor_state(
        seed,
        screen,
        cursor.x,
        cursor.y,
        cursor.pending_wrap,
        cursor.fg,
        cursor.bg,
        cursor.underline_color,
        cursor.attrs,
        autowrap,
    );
}

#[allow(clippy::too_many_arguments)]
fn write_cursor_state(
    seed: &mut String,
    screen: &Screen,
    x: u16,
    y: u16,
    pending_wrap: bool,
    fg: Color,
    bg: Color,
    underline_color: Option<Color>,
    attrs: CellAttrs,
    autowrap: bool,
) {
    if pending_wrap && x == screen.cols.saturating_sub(1) {
        // CUP clears the deferred-wrap latch. Reprint the cell under the
        // cursor with autowrap briefly enabled to recreate it without changing
        // the visible grid, then restore the cursor's independent pen state.
        let (print_x, cell) = pending_wrap_cell(screen, x, y);
        write_private_mode(seed, 7, true);
        write_cup(seed, print_x, y);
        write_pen(seed, cell.fg, cell.bg, cell.underline_color, cell.attrs);
        cell.push_text_to(seed);
        write_private_mode(seed, 7, autowrap);
    } else {
        write_cup(seed, x, y);
    }
    write_pen(seed, fg, bg, underline_color, attrs);
}

fn pending_wrap_cell(screen: &Screen, x: u16, y: u16) -> (u16, &Cell) {
    let row = &screen.grid[y.min(screen.rows.saturating_sub(1)) as usize];
    let cell = &row.cells[x.min(screen.cols.saturating_sub(1)) as usize];
    if cell.attrs.contains(CellAttrs::WIDE_SPACER) && x > 0 {
        (x - 1, &row.cells[(x - 1) as usize])
    } else {
        (x, cell)
    }
}

fn write_cup(seed: &mut String, x: u16, y: u16) {
    write!(
        seed,
        "\x1b[{};{}H",
        y.saturating_add(1),
        x.saturating_add(1)
    )
    .expect("writing to String cannot fail");
}

fn write_private_mode(seed: &mut String, mode: u16, enabled: bool) {
    write!(seed, "\x1b[?{mode}{}", if enabled { 'h' } else { 'l' })
        .expect("writing to String cannot fail");
}

fn write_ansi_mode(seed: &mut String, mode: u16, enabled: bool) {
    write!(seed, "\x1b[{mode}{}", if enabled { 'h' } else { 'l' })
        .expect("writing to String cannot fail");
}

fn write_pen(
    seed: &mut String,
    fg: Color,
    bg: Color,
    underline_color: Option<Color>,
    attrs: CellAttrs,
) {
    seed.push_str("\x1b[0");
    for (attr, parameter) in [
        (CellAttrs::BOLD, ";1"),
        (CellAttrs::FAINT, ";2"),
        (CellAttrs::ITALIC, ";3"),
        (CellAttrs::BLINK, ";5"),
        (CellAttrs::INVERSE, ";7"),
        (CellAttrs::INVISIBLE, ";8"),
        (CellAttrs::STRIKETHROUGH, ";9"),
        (CellAttrs::OVERLINE, ";53"),
    ] {
        if attrs.contains(attr) {
            seed.push_str(parameter);
        }
    }

    if attrs.contains(CellAttrs::DOUBLE_UNDERLINE) {
        seed.push_str(";4:2");
    } else if attrs.contains(CellAttrs::CURLY_UNDERLINE) {
        seed.push_str(";4:3");
    } else if attrs.contains(CellAttrs::DOTTED_UNDERLINE) {
        seed.push_str(";4:4");
    } else if attrs.contains(CellAttrs::DASHED_UNDERLINE) {
        seed.push_str(";4:5");
    } else if attrs.contains(CellAttrs::UNDERLINE) {
        seed.push_str(";4");
    }

    write_color(seed, 38, fg);
    write_color(seed, 48, bg);
    if let Some(color) = underline_color {
        write_color(seed, 58, color);
    }
    seed.push('m');
}

fn write_color(seed: &mut String, parameter: u8, color: Color) {
    match color {
        Color::Default => {}
        Color::Palette(index) => {
            write!(seed, ";{parameter};5;{index}").expect("writing to String cannot fail");
        }
        Color::Rgb(rgb) => {
            write!(seed, ";{parameter};2;{};{};{}", rgb.r, rgb.g, rgb.b)
                .expect("writing to String cannot fail");
        }
    }
}

fn write_cursor_style(seed: &mut String, style: CursorStyle) {
    let parameter = match style {
        CursorStyle::BlinkingBlock | CursorStyle::BlinkingBlockHollow => 1,
        CursorStyle::SteadyBlock | CursorStyle::SteadyBlockHollow => 2,
        CursorStyle::BlinkingUnderline => 3,
        CursorStyle::SteadyUnderline => 4,
        CursorStyle::BlinkingBar => 5,
        CursorStyle::SteadyBar => 6,
    };
    write!(seed, "\x1b[{parameter} q").expect("writing to String cannot fail");
}

/// [`write_cursor_style`] plus, for the hollow variants standard `DECSCUSR`
/// cannot express, the seed-only follow-up that recovers the exact shape
/// (see `Handler::seed_set_cursor_hollow`). Without it `block_hollow`
/// round-trips as a plain block and the replica's cursor shape diverges
/// from the source immediately after attach.
fn write_cursor_style_exact(seed: &mut String, style: CursorStyle) {
    write_cursor_style(seed, style);
    if matches!(
        style,
        CursorStyle::BlinkingBlockHollow | CursorStyle::SteadyBlockHollow
    ) {
        seed.push_str("\x1b[>$t");
    }
}

/// Seed-only `CSI > Ps ; Ph $ q` restoration of the DECSCUSR-0 default
/// cursor style (see `Handler::seed_set_default_cursor_style`). This is
/// independent of the active cursor's own style, which
/// [`write_cursor_style_exact`] already restored above; without it a
/// replica's own configured default (`noa-app`'s `cursor-style`) leaks back
/// in the moment a live `CSI 0 q` arrives after attach.
fn write_default_cursor_style(seed: &mut String, style: CursorStyle) {
    let ps = match style {
        CursorStyle::BlinkingBlock | CursorStyle::BlinkingBlockHollow => 1,
        CursorStyle::SteadyBlock | CursorStyle::SteadyBlockHollow => 2,
        CursorStyle::BlinkingUnderline => 3,
        CursorStyle::SteadyUnderline => 4,
        CursorStyle::BlinkingBar => 5,
        CursorStyle::SteadyBar => 6,
    };
    let hollow = matches!(
        style,
        CursorStyle::BlinkingBlockHollow | CursorStyle::SteadyBlockHollow
    );
    write!(seed, "\x1b[>{ps};{}$q", u8::from(hollow)).expect("writing to String cannot fail");
}

/// Seed-only `REP` (`CSI b`) state restoration (see
/// `Handler::seed_set_last_printed`). `ch` is split across two `u16`
/// parameters (`Ph << 16 | Pl`) because a `CSI` parameter caps out at
/// `u16::MAX`, short of the full Unicode scalar range.
fn write_seed_last_printed(seed: &mut String, ch: char) {
    let codepoint = ch as u32;
    write!(seed, "\x1b[>{};{}$s", codepoint >> 16, codepoint & 0xffff)
        .expect("writing to String cannot fail");
}

fn write_charset(seed: &mut String, state: (Charset, Charset, CharsetSlot)) {
    let (g0, g1, active) = state;
    seed.push_str(match g0 {
        Charset::Ascii => "\x1b(B",
        Charset::DecSpecialGraphics => "\x1b(0",
    });
    seed.push_str(match g1 {
        Charset::Ascii => "\x1b)B",
        Charset::DecSpecialGraphics => "\x1b)0",
    });
    seed.push(match active {
        CharsetSlot::G0 => '\x0f',
        CharsetSlot::G1 => '\x0e',
    });
}

#[cfg(test)]
mod tests {
    use noa_core::{GridSize, Rgb};
    use noa_vt::Stream;

    use super::*;

    #[test]
    fn synthetic_seed_round_trips_client_mode_state() {
        let size = GridSize::new(12, 6);
        let mut source = Terminal::new(size);
        let mut stream = Stream::new();
        stream.feed(
            b"primary\x1b[?1049h\
              \x1b[1;2H\x1b[1;3;4:3;38;2;10;20;30;48;5;17;58;5;123mA\
              \x1b[2;4H\x1b[2;9;38;5;200mB\
              \x1b[3;2H\x1b[4:5;53;48;2;1;2;3mC\
              \x1b[4;8H\x1b[1;38;5;123;48;2;4;5;6m\x1b7\
              \x1b[3g\x1b[1;3H\x1bH\x1b[1;7H\x1bH\
              \x1b[2;5r\x1b[?2004h\x1b[?1003h\x1b[?1006h\
              \x1b[?7l\x1b[?6h\x1b[?1h\x1b[?25l\
              \x1b[3;5H\x1b[3;4:2;38;2;90;80;70;48;5;42;58;2;7;8;9m\
              \x1b[6 q\x1b(0\x1b)0\x0e",
            &mut source,
        );

        let seed = source.synthetic_seed();
        let mut replica = Terminal::new(size);
        let mut replica_stream = Stream::new();
        replica_stream.feed(&seed, &mut replica);

        assert_eq!(replica.active_is_alt, source.active_is_alt);
        assert_screen_state(source.active(), replica.active());
        for mode in REPLAYED_PRIVATE_MODES {
            assert_eq!(
                replica.modes.get(mode, false),
                source.modes.get(mode, false),
                "DEC private mode {mode}"
            );
        }
        assert_eq!(
            replica.charset.designations(),
            source.charset.designations()
        );
    }

    #[test]
    fn synthetic_seed_recreates_deferred_wrap_for_cursor_and_saved_cursor() {
        let size = GridSize::new(4, 2);
        let mut source = Terminal::new(size);
        let mut stream = Stream::new();
        stream.feed(b"abcd\x1b7\x1b[2;1Hxyzq", &mut source);

        let seed = source.synthetic_seed();
        let mut replica = Terminal::new(size);
        let mut replica_stream = Stream::new();
        replica_stream.feed(&seed, &mut replica);

        assert_screen_state(source.active(), replica.active());
    }

    #[test]
    fn synthetic_seed_preserves_primary_screen_across_alternate_screen_exit() {
        let size = GridSize::new(8, 3);
        let mut source = Terminal::new(size);
        let mut source_stream = Stream::new();
        source_stream.feed(b"primary\x1b[2;3H\x1b[?1049halt\x1b[2;2H", &mut source);

        let mut replica = Terminal::new(size);
        let mut replica_stream = Stream::new();
        replica_stream.feed(&source.synthetic_seed(), &mut replica);
        assert_screen_state(source.active(), replica.active());

        source_stream.feed(b"\x1b[?1049l", &mut source);
        replica_stream.feed(b"\x1b[?1049l", &mut replica);
        assert!(!source.active_is_alt);
        assert!(!replica.active_is_alt);
        assert_screen_state(source.active(), replica.active());
    }

    #[test]
    fn synthetic_seed_preserves_retained_alternate_screen_while_primary_is_active() {
        let size = GridSize::new(10, 4);
        let mut source = Terminal::new(size);
        let mut source_stream = Stream::new();
        source_stream.feed(
            b"\x1b[?47halt\x1b[2;5H\x1bH\x1b[?69h\x1b[2;8s\x1b[3;7H\x1b7\x1b[?47lprimary",
            &mut source,
        );

        let mut replica = Terminal::new(size);
        let mut replica_stream = Stream::new();
        replica_stream.feed(&source.synthetic_seed(), &mut replica);
        assert!(!replica.active_is_alt);
        assert_screen_state(source.active(), replica.active());

        source_stream.feed(b"\x1b[?47h", &mut source);
        replica_stream.feed(b"\x1b[?47h", &mut replica);
        assert!(replica.active_is_alt);
        assert_screen_state(source.active(), replica.active());
    }

    #[test]
    fn synthetic_seed_saves_cursor_before_horizontal_margins_can_clamp_it() {
        let size = GridSize::new(10, 4);
        let mut source = Terminal::new(size);
        let mut source_stream = Stream::new();
        source_stream.feed(b"\x1b[2;9H\x1b7\x1b[?69h\x1b[2;6s\x1b[3;4H", &mut source);

        let mut replica = Terminal::new(size);
        let mut replica_stream = Stream::new();
        replica_stream.feed(&source.synthetic_seed(), &mut replica);
        assert_screen_state(source.active(), replica.active());

        source_stream.feed(b"\x1b8X", &mut source);
        replica_stream.feed(b"\x1b8X", &mut replica);
        assert_screen_state(source.active(), replica.active());
    }

    #[test]
    fn synthetic_seed_preserves_soft_wraps_for_copy_and_reflow() {
        let size = GridSize::new(4, 3);
        let mut source = Terminal::new(size);
        let mut source_stream = Stream::new();
        source_stream.feed(b"abcdef", &mut source);

        let mut replica = Terminal::new(size);
        let mut replica_stream = Stream::new();
        replica_stream.feed(&source.synthetic_seed(), &mut replica);
        assert_screen_state(source.active(), replica.active());

        source.resize(GridSize::new(3, 3));
        replica.resize(GridSize::new(3, 3));
        assert_screen_state(source.active(), replica.active());
    }

    #[test]
    fn synthetic_seed_preserves_input_modes_and_kitty_stacks_per_screen() {
        let size = GridSize::new(8, 3);
        let mut source = Terminal::new(size);
        let mut source_stream = Stream::new();
        source_stream.feed(
            b"\x1b[?66h\x1b[?1004h\x1b[?1007l\x1b[>4;2m\x1b[>1u\x1b[>5u\x1b[?1049h\x1b[>8u\x1b[>3u",
            &mut source,
        );

        let mut replica = Terminal::new(size);
        let mut replica_stream = Stream::new();
        replica_stream.feed(&source.synthetic_seed(), &mut replica);
        for mode in [66, 1004, 1007] {
            assert_eq!(
                replica.modes.get(mode, false),
                source.modes.get(mode, false)
            );
        }
        assert!(replica.modify_other_keys_2);
        assert_eq!(replica.kitty_keyboard_flags(), 3);
        replica_stream.feed(b"\x1b[<1u", &mut replica);
        assert_eq!(replica.kitty_keyboard_flags(), 8);
        replica_stream.feed(b"\x1b[?1049l", &mut replica);
        assert_eq!(replica.kitty_keyboard_flags(), 5);
        replica_stream.feed(b"\x1b[<1u", &mut replica);
        assert_eq!(replica.kitty_keyboard_flags(), 1);
    }

    #[test]
    fn synthetic_seed_preserves_lnm_and_horizontal_margins_for_live_bytes() {
        let size = GridSize::new(8, 4);
        let mut source = Terminal::new(size);
        let mut source_stream = Stream::new();
        source_stream.feed(
            b"\x1b[20h\x1b[?69h\x1b[2;6s\x1b[2;6H\x1b[?47h\x1b[3;7s\x1b[2;7H",
            &mut source,
        );

        let mut replica = Terminal::new(size);
        let mut replica_stream = Stream::new();
        replica_stream.feed(&source.synthetic_seed(), &mut replica);

        assert!(replica.modes.linefeed_newline());
        assert!(replica.modes.left_right_margin());
        assert_eq!(
            replica.primary.horizontal_margins,
            source.primary.horizontal_margins
        );
        assert_eq!(
            replica.active().horizontal_margins,
            source.active().horizontal_margins
        );

        source_stream.feed(b"\nZ", &mut source);
        replica_stream.feed(b"\nZ", &mut replica);
        assert_screen_state(source.active(), replica.active());

        source_stream.feed(b"\x1b[?47l\nP", &mut source);
        replica_stream.feed(b"\x1b[?47l\nP", &mut replica);
        assert_screen_state(source.active(), replica.active());
    }

    #[test]
    fn synthetic_seed_preserves_dynamic_color_overrides() {
        let size = GridSize::new(4, 2);
        let mut source = Terminal::new(size);
        let mut source_stream = Stream::new();
        source_stream.feed(
            b"\x1b]4;1;#123456;200;#abcdef\x1b\\\
              \x1b]10;#102030\x1b\\\
              \x1b]11;#405060\x1b\\\
              \x1b]12;#708090\x1b\\\
              \x1b[31;48;5;200mX",
            &mut source,
        );

        let mut replica = Terminal::new(size);
        let mut replica_stream = Stream::new();
        replica_stream.feed(&source.synthetic_seed(), &mut replica);

        assert_eq!(replica.colors, source.colors);
        assert_screen_state(source.active(), replica.active());
    }

    #[test]
    fn synthetic_seed_ris_preserves_replica_scrollback_limit() {
        let source = Terminal::new(GridSize::new(4, 2));

        for limit in [0, 1_234] {
            let mut replica = Terminal::new(source.size);
            replica.set_scrollback_limit_bytes(limit);
            let mut stream = Stream::new();
            stream.feed(&source.synthetic_seed(), &mut replica);

            assert_eq!(replica.primary.scrollback_limit_bytes(), limit);
        }
    }

    #[test]
    fn synthetic_seed_preserves_synchronized_output_through_live_release() {
        let size = GridSize::new(8, 3);
        let mut source = Terminal::new(size);
        let mut source_stream = Stream::new();
        source_stream.feed(b"before\x1b[?2026h\x1b[2;2H", &mut source);

        let mut replica = Terminal::new(size);
        let mut replica_stream = Stream::new();
        replica_stream.feed(&source.synthetic_seed(), &mut replica);
        assert!(replica.modes.synchronized_output());

        source_stream.feed(b"live\x1b[?2026l", &mut source);
        replica_stream.feed(b"live\x1b[?2026l", &mut replica);
        assert!(!replica.modes.synchronized_output());
        assert_screen_state(source.active(), replica.active());
    }

    fn assert_screen_state(expected: &Screen, actual: &Screen) {
        for (y, (expected_row, actual_row)) in
            expected.grid.iter().zip(actual.grid.iter()).enumerate()
        {
            assert_eq!(actual_row.cells, expected_row.cells, "grid row {y}");
            assert_eq!(
                actual_row.wrapped, expected_row.wrapped,
                "grid row {y} wrap"
            );
        }

        assert_cursor(expected.cursor, actual.cursor);
        match (expected.saved_cursor, actual.saved_cursor) {
            (Some(expected), Some(actual)) => assert_saved_cursor(expected, actual),
            (None, None) => {}
            states => panic!("saved cursor mismatch: {states:?}"),
        }
        assert_eq!(actual.region.top, expected.region.top);
        assert_eq!(actual.region.bottom, expected.region.bottom);
        assert_eq!(actual.horizontal_margins, expected.horizontal_margins);
        assert_eq!(
            actual.tabstops.positions().collect::<Vec<_>>(),
            expected.tabstops.positions().collect::<Vec<_>>()
        );
    }

    fn assert_cursor(expected: Cursor, actual: Cursor) {
        assert_eq!((actual.x, actual.y), (expected.x, expected.y));
        assert_eq!(actual.pending_wrap, expected.pending_wrap);
        assert_eq!(actual.fg, expected.fg);
        assert_eq!(actual.bg, expected.bg);
        assert_eq!(actual.underline_color, expected.underline_color);
        assert_eq!(actual.attrs, expected.attrs);
        assert_eq!(actual.visible, expected.visible);
        assert_eq!(actual.style, expected.style);
    }

    fn assert_saved_cursor(expected: SavedCursor, actual: SavedCursor) {
        assert_eq!((actual.x, actual.y), (expected.x, expected.y));
        assert_eq!(actual.pending_wrap, expected.pending_wrap);
        assert_eq!(actual.fg, expected.fg);
        assert_eq!(actual.bg, expected.bg);
        assert_eq!(actual.underline_color, expected.underline_color);
        assert_eq!(actual.attrs, expected.attrs);
    }

    #[test]
    fn synthetic_seed_restores_last_printed_char_independent_of_repaint_order() {
        let size = GridSize::new(6, 3);
        let mut source = Terminal::new(size);
        let mut source_stream = Stream::new();
        // Z lands on a later grid row, but the cursor then moves back and
        // prints X on an earlier row — X is the source's true REP target
        // even though a row-major grid repaint visits Z's cell last.
        source_stream.feed(b"\x1b[3;1HZ\x1b[1;1HX", &mut source);

        let mut replica = Terminal::new(size);
        let mut replica_stream = Stream::new();
        replica_stream.feed(&source.synthetic_seed(), &mut replica);

        // A live REP after attach must repeat the same character on both:
        // if the replica's `last_printed` had been left as 'Z' by the
        // repaint order, this would diverge from the source.
        source_stream.feed(b"\x1b[2;1H\x1b[3b", &mut source);
        replica_stream.feed(b"\x1b[2;1H\x1b[3b", &mut replica);
        assert_screen_state(source.active(), replica.active());
    }

    #[test]
    fn synthetic_seed_restores_last_printed_char_per_screen() {
        let size = GridSize::new(6, 3);
        let mut source = Terminal::new(size);
        let mut source_stream = Stream::new();
        // Primary's REP target is 'P'; entering the alternate screen and
        // printing 'A' there must not disturb primary's independent state,
        // and the alt screen gets its own REP target too.
        source_stream.feed(b"\x1b[1;1HP\x1b[?1049h\x1b[1;1HA", &mut source);

        let mut replica = Terminal::new(size);
        let mut replica_stream = Stream::new();
        replica_stream.feed(&source.synthetic_seed(), &mut replica);

        source_stream.feed(b"\x1b[2;1H\x1b[3b", &mut source);
        replica_stream.feed(b"\x1b[2;1H\x1b[3b", &mut replica);
        assert_screen_state(source.active(), replica.active());

        source_stream.feed(b"\x1b[?1049l\x1b[2;1H\x1b[3b", &mut source);
        replica_stream.feed(b"\x1b[?1049l\x1b[2;1H\x1b[3b", &mut replica);
        assert!(!source.active_is_alt);
        assert!(!replica.active_is_alt);
        assert_screen_state(source.active(), replica.active());
    }

    #[test]
    fn synthetic_seed_preserves_hollow_cursor_style() {
        let size = GridSize::new(4, 2);
        for style in [
            CursorStyle::BlinkingBlockHollow,
            CursorStyle::SteadyBlockHollow,
        ] {
            let mut source = Terminal::new(size);
            source.set_default_cursor_style(style);
            let mut source_stream = Stream::new();
            // DECSCUSR 0 resets to the configured default, the only VT path
            // that can land the cursor on a hollow style.
            source_stream.feed(b"\x1b[0 q", &mut source);
            assert_eq!(source.active().cursor.style, style);

            let mut replica = Terminal::new(size);
            let mut replica_stream = Stream::new();
            replica_stream.feed(&source.synthetic_seed(), &mut replica);
            assert_eq!(
                replica.active().cursor.style,
                style,
                "hollow cursor style should round-trip exactly for {style:?}"
            );
        }
    }

    #[test]
    fn synthetic_seed_restores_default_cursor_style_independent_of_replica_config() {
        let size = GridSize::new(4, 2);
        for default_style in [
            CursorStyle::SteadyUnderline,
            CursorStyle::BlinkingBlockHollow,
            CursorStyle::SteadyBlockHollow,
        ] {
            let mut source = Terminal::new(size);
            source.set_default_cursor_style(default_style);
            // Move the *live* cursor style away from the default so the seed
            // exercises both `write_cursor_style_exact` (current) and
            // `write_default_cursor_style` (DECSCUSR-0 target) independently.
            let mut source_stream = Stream::new();
            source_stream.feed(b"\x1b[6 q", &mut source);

            // The replica has a different configured default (as a real
            // client-mode attach would, before any seed arrives).
            let mut replica = Terminal::new(size);
            replica.set_default_cursor_style(CursorStyle::BlinkingBar);
            let mut replica_stream = Stream::new();
            replica_stream.feed(&source.synthetic_seed(), &mut replica);

            assert_eq!(replica.active().cursor.style, source.active().cursor.style);

            // A live DECSCUSR-0 after attach must resolve to the source's
            // configured default on both sides, hollow shapes included.
            source_stream.feed(b"\x1b[0 q", &mut source);
            replica_stream.feed(b"\x1b[0 q", &mut replica);
            assert_eq!(
                replica.active().cursor.style,
                source.active().cursor.style,
                "default cursor style should round-trip exactly for {default_style:?}"
            );
            assert_eq!(replica.active().cursor.style, default_style);
        }
    }

    #[test]
    fn synthetic_seed_encodes_rgb_color_without_allocating_terminal_metadata() {
        let mut source = Terminal::new(GridSize::new(2, 1));
        source.primary.grid[0].cells[0].fg = Color::Rgb(Rgb::new(1, 2, 3));
        source.primary.grid[0].cells[0].ch = 'X';

        let mut replica = Terminal::new(source.size);
        let mut stream = Stream::new();
        stream.feed(&source.synthetic_seed(), &mut replica);

        assert_eq!(replica.primary.grid[0].cells, source.primary.grid[0].cells);
    }

    #[test]
    fn synthetic_seed_replaces_or_clears_task_progress() {
        let size = GridSize::new(4, 2);
        let mut source = Terminal::new(size);
        let mut source_stream = Stream::new();
        source_stream.feed(b"\x1b]9;4;4;37\x07", &mut source);

        let mut replica = Terminal::new(size);
        let mut replica_stream = Stream::new();
        replica_stream.feed(b"\x1b]9;4;2;80\x07", &mut replica);
        let _ = replica.take_pending_progress_update();
        replica_stream.feed(&source.synthetic_seed(), &mut replica);

        assert_eq!(replica.progress(), source.progress());
        let progress = replica.progress().unwrap();
        assert!(matches!(progress, crate::TerminalProgress::Paused(Some(_))));
        assert_eq!(
            replica.take_pending_progress_update(),
            Some(crate::ProgressUpdate::Set(progress))
        );

        source_stream.feed(b"\x1b]9;4;4\x07", &mut source);
        replica_stream.feed(&source.synthetic_seed(), &mut replica);
        assert_eq!(
            replica.progress(),
            Some(crate::TerminalProgress::Paused(None))
        );
        assert_eq!(
            replica.take_pending_progress_update(),
            Some(crate::ProgressUpdate::Set(crate::TerminalProgress::Paused(
                None
            )))
        );

        source_stream.feed(b"\x1b]9;4;0;0\x07", &mut source);
        replica_stream.feed(&source.synthetic_seed(), &mut replica);
        assert_eq!(replica.progress(), None);
        assert_eq!(
            replica.take_pending_progress_update(),
            Some(crate::ProgressUpdate::Clear)
        );
    }
}
