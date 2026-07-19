//! Adversarial: does `print_ascii_run`'s edge-only wide-pair cleanup produce
//! byte-identical grid state to the per-scalar `Handler::print` path?
//!
//! The line-batch differential harness cannot catch a bug here: BOTH its
//! batched terminal and its per-byte reference route ASCII through
//! `print_ascii_run`. So compare `print_ascii_run` (via `print_str`) against
//! a genuine per-char `print()` reference over adversarial wide remnants.

use noa_core::GridSize;
use noa_grid::{Cell, Terminal};
use noa_vt::Handler;

const COLS: u16 = 24;
const ROWS: u16 = 6;

fn lcg(state: &mut u64) -> u64 {
    *state = state
        .wrapping_mul(6364136223846793005)
        .wrapping_add(1442695040888963407);
    *state >> 33
}

fn assert_rows_identical(label: &str, a: &Terminal, b: &Terminal) {
    for y in 0..ROWS as usize {
        let ra = &a.primary.grid[y];
        let rb = &b.primary.grid[y];
        assert_eq!(ra.occupied(), rb.occupied(), "{label}: row {y} occ");
        assert_eq!(ra.wrapped, rb.wrapped, "{label}: row {y} wrapped");
        for x in 0..COLS as usize {
            let ca: Cell = ra.cells[x];
            let cb: Cell = rb.cells[x];
            assert_eq!(
                (
                    ca.ch,
                    ca.fg,
                    ca.bg,
                    ca.attrs,
                    ca.hyperlink,
                    ca.underline_color
                ),
                (
                    cb.ch,
                    cb.fg,
                    cb.bg,
                    cb.attrs,
                    cb.hyperlink,
                    cb.underline_color
                ),
                "{label}: row {y} col {x} (ref {:?} vs bulk {:?})",
                ca.ch,
                cb.ch,
            );
        }
    }
    assert_eq!(
        (
            a.active().cursor.x,
            a.active().cursor.y,
            a.active().cursor.pending_wrap
        ),
        (
            b.active().cursor.x,
            b.active().cursor.y,
            b.active().cursor.pending_wrap
        ),
        "{label}: cursor",
    );
}

#[test]
fn print_ascii_run_wide_cleanup_matches_per_char_print() {
    // A curated width-2 set: CJK, Hangul, emoji, and a plane-2 ideograph.
    let wide_set = ['日', '本', '語', '中', '한', '글', '😀', '𠀀'];

    let mut seed = 0x1234_5678_9abc_def0u64;
    for trial in 0..8000u64 {
        let mut refr = Terminal::new(GridSize::new(COLS, ROWS));
        let mut bulk = Terminal::new(GridSize::new(COLS, ROWS));

        // ── Phase 1: identical setup row (per-char on both sides) ──
        let ncells = 4 + (lcg(&mut seed) % (COLS as u64 - 6)) as usize;
        let mut placed: Vec<char> = Vec::new();
        let mut used = 0usize;
        while used < ncells {
            let pick = lcg(&mut seed) % 3;
            if pick == 0 && used + 2 <= COLS as usize {
                let w = wide_set[(lcg(&mut seed) % wide_set.len() as u64) as usize];
                placed.push(w);
                used += 2;
            } else {
                let a = b'a' + (lcg(&mut seed) % 26) as u8;
                placed.push(a as char);
                used += 1;
            }
        }
        for &c in &placed {
            Handler::print(&mut refr, c);
            Handler::print(&mut bulk, c);
        }
        assert_rows_identical(&format!("trial {trial} setup"), &refr, &bulk);

        // ── Phase 2: place cursor at a random column on row 0 ──
        let target_x = (lcg(&mut seed) % COLS as u64) as u16;
        Handler::cursor_position(&mut refr, 1, target_x + 1);
        Handler::cursor_position(&mut bulk, 1, target_x + 1);

        // ── Phase 3: overwrite with an ASCII run ──
        let runlen = 1 + (lcg(&mut seed) % 8) as usize;
        let text: String = (0..runlen)
            .map(|_| (b'A' + (lcg(&mut seed) % 26) as u8) as char)
            .collect();

        for c in text.chars() {
            Handler::print(&mut refr, c);
        }
        Handler::print_str(&mut bulk, &text);

        assert_rows_identical(
            &format!("trial {trial} placed={placed:?} target_x={target_x} text={text:?}"),
            &refr,
            &bulk,
        );
    }
}
