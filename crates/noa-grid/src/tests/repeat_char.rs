// ── REP (`CSI n b`) fast path equivalence ──────────────────────────
//
// `Screen::repeat_preceding_char` writes whole row segments instead of
// printing the repeated scalar `n` times. These differential tests run each
// scenario through it and through the per-scalar reference loop
// (`repeat_preceding_char_per_scalar`) and assert identical screen,
// scrollback, cursor and latch state.

fn rep_screen(t: &mut Terminal) -> &mut crate::Screen {
    if t.active_is_alt {
        t.alt.as_mut().expect("alternate screen exists while active")
    } else {
        &mut t.primary
    }
}

/// Apply REP the way `Handler::repeat_preceding_char` does, through the
/// fast path or the reference loop.
fn apply_rep(t: &mut Terminal, n: u16, reference: bool) {
    let autowrap = t.modes.autowrap();
    let grapheme_clustering = t.modes.grapheme_clustering();
    let insert_mode = t.modes.insert_mode();
    let screen = rep_screen(t);
    if reference {
        screen.repeat_preceding_char_per_scalar(n, autowrap, grapheme_clustering, insert_mode);
    } else {
        screen.repeat_preceding_char(n, autowrap, grapheme_clustering, insert_mode);
    }
}

fn assert_screens_match(label: &str, f: &mut crate::Screen, r: &mut crate::Screen) {
    f.trim_memory();
    r.trim_memory();
    assert_eq!(f.take_scroll_shift(), r.take_scroll_shift(), "{label}: scroll_shift");
    assert_eq!(
        (f.cursor.x, f.cursor.y, f.cursor.pending_wrap),
        (r.cursor.x, r.cursor.y, r.cursor.pending_wrap),
        "{label}: cursor"
    );
    assert_eq!(f.last_printed(), r.last_printed(), "{label}: last_printed");
    assert_eq!(f.scrollback_len(), r.scrollback_len(), "{label}: scrollback_len");
    assert_eq!(f.rows_evicted(), r.rows_evicted(), "{label}: rows_evicted");
    assert_eq!(f.viewport_offset(), r.viewport_offset(), "{label}: viewport_offset");
    assert_eq!(f.selection, r.selection, "{label}: selection");
    assert_eq!(f.kitty_placements, r.kitty_placements, "{label}: kitty placements");
    assert_eq!(f.total_rows(), r.total_rows(), "{label}: total_rows");
    for y in 0..r.total_rows() {
        let fr = f.absolute_row(y).expect("row in range");
        let rr = r.absolute_row(y).expect("row in range");
        assert_eq!(fr.wrapped, rr.wrapped, "{label}: row {y} wrapped");
        if fr.cells != rr.cells {
            for (x, (fc, rc)) in fr.cells.iter().zip(&rr.cells).enumerate() {
                assert_eq!(fc, rc, "{label}: row {y} col {x}");
            }
        }
    }
    for (y, (fr, rr)) in f.grid.iter().zip(&r.grid).enumerate() {
        assert_eq!(fr.dirty, rr.dirty, "{label}: grid row {y} dirty");
        assert_eq!(fr.occupied(), rr.occupied(), "{label}: grid row {y} occupancy");
    }
}

/// Feed each `(setup, n)` round to two identical terminals, applying REP
/// `n` through the fast path on one and the reference loop on the other,
/// and compare both screens after every round.
fn assert_rep_matches(
    label: &str,
    cols: u16,
    rows: u16,
    prep: impl Fn(&mut Terminal),
    rounds: &[(Vec<u8>, u16)],
) {
    let mut fast = Terminal::new(GridSize::new(cols, rows));
    let mut reference = Terminal::new(GridSize::new(cols, rows));
    prep(&mut fast);
    prep(&mut reference);
    let (mut fs, mut rs) = (Stream::new(), Stream::new());
    for (i, (setup, n)) in rounds.iter().enumerate() {
        fs.feed(setup, &mut fast);
        rs.feed(setup, &mut reference);
        apply_rep(&mut fast, *n, false);
        apply_rep(&mut reference, *n, true);
        let label = format!("{label} round {i} setup {:?} n {n}", String::from_utf8_lossy(setup));
        assert_eq!(fast.active_is_alt, reference.active_is_alt, "{label}: active screen");
        assert_screens_match(&format!("{label} primary"), &mut fast.primary, &mut reference.primary);
        if let (Some(f), Some(r)) = (fast.alt.as_mut(), reference.alt.as_mut()) {
            assert_screens_match(&format!("{label} alt"), f, r);
        }
    }
}

fn one(setup: &str, n: u16) -> Vec<(Vec<u8>, u16)> {
    vec![(setup.as_bytes().to_vec(), n)]
}

fn no_rep_prep(_t: &mut Terminal) {}

/// Scalars REP is asked to repeat: ASCII, space, plain narrow, plain wide,
/// emoji, combining mark, regional indicator, Hangul L, ZWJ, VS16, keycap
/// base, skin-tone modifier.
const REP_CHARS: &[&str] = &[
    "A", " ", "é", "漢", "😀", "\u{301}", "🇦", "ᄀ", "\u{200d}", "\u{fe0f}", "#", "\u{1f3fb}",
];

#[test]
fn rep_matches_per_scalar_for_hand_picked_cases() {
    let cases: &[(&str, u16, u16, &str, u16)] = &[
        ("plain narrow", 10, 4, "A", 3),
        ("narrow fills screen and scrolls", 10, 4, "A", 65535),
        ("narrow from mid row", 10, 4, "\x1b[2;5HA", 37),
        ("narrow exact row fill", 10, 4, "A", 9),
        ("narrow from pending wrap", 10, 4, "\x1b[1;10HA", 25),
        ("wide fills and scrolls", 10, 4, "漢", 65535),
        ("wide odd cols", 9, 4, "漢", 100),
        ("wide at right edge", 9, 3, "\x1b[1;8H漢", 7),
        ("wide pending at edge", 10, 3, "\x1b[1;9H漢", 5),
        ("wide in 1 col", 1, 3, "漢", 9),
        ("narrow in 1 col", 1, 3, "A", 9),
        ("emoji", 11, 4, "😀", 400),
        ("over wide pairs", 10, 3, "漢字漢字漢\r\x1b[1CA", 6),
        ("wide over narrow-wide mix", 10, 3, "a漢b字c漢\r\x1b[1C漢", 3),
        ("scroll region", 10, 6, "\x1b[2;4r\x1b[3;1HA", 500),
        ("below scroll region", 10, 6, "\x1b[2;4r\x1b[6;1HA", 500),
        ("above scroll region", 10, 6, "\x1b[3;5r\x1b[1;1HA", 500),
        ("origin mode", 10, 6, "\x1b[2;4r\x1b[?6h\x1b[2;3HA", 200),
        ("lr margins", 12, 5, "\x1b[?69h\x1b[3;8s\x1b[2;4HA", 300),
        ("lr margins wide", 12, 5, "\x1b[?69h\x1b[3;8s\x1b[2;4H漢", 300),
        ("left of margins", 12, 5, "\x1b[?69h\x1b[4;8s\x1b[2;1HA", 300),
        ("right of margins", 12, 5, "\x1b[?69h\x1b[3;8s\x1b[2;10HA", 300),
        ("right of margins wide", 12, 5, "\x1b[?69h\x1b[3;8s\x1b[2;10H漢", 300),
        ("one-col margin wide", 12, 5, "\x1b[?69h\x1b[5;5s\x1b[2;5H漢", 300),
        ("margins and region", 12, 6, "\x1b[2;5r\x1b[?69h\x1b[3;9s\x1b[5;3HA", 300),
        ("autowrap off", 10, 4, "\x1b[?7l\x1b[2;3HA", 65535),
        ("autowrap off wide", 9, 4, "\x1b[?7l\x1b[2;3H漢", 65535),
        ("autowrap off wide even", 10, 4, "\x1b[?7l\x1b[2;2H漢", 300),
        ("autowrap off pending", 10, 4, "\x1b[1;10HA\x1b[?7l", 50),
        ("insert mode", 10, 4, "hello world\x1b[1;3H\x1b[4hA", 30),
        ("insert mode wide", 10, 4, "hello world\x1b[1;3H\x1b[4h漢", 30),
        ("insert autowrap off", 10, 4, "hello\x1b[1;3H\x1b[4h\x1b[?7lA", 300),
        ("insert autowrap off wide", 9, 4, "hello\x1b[1;2H\x1b[4h\x1b[?7l漢", 300),
        ("combining", 10, 4, "e\u{301}", 5),
        ("grapheme mode narrow", 10, 4, "\x1b[?2027hA", 300),
        ("grapheme mode wide", 10, 4, "\x1b[?2027h漢", 300),
        ("grapheme mode regional indicator", 10, 4, "\x1b[?2027h🇦", 7),
        ("grapheme mode zwj prefix", 10, 4, "\x1b[?2027h😀\u{200d}😀", 30),
        ("grapheme mode hangul", 10, 4, "\x1b[?2027hᄀ", 9),
        ("grapheme mode skin tone", 10, 4, "\x1b[?2027h👍\u{1f3fb}", 9),
        ("pen colors", 10, 4, "\x1b[31;44;1;4mA", 75),
        ("bce scroll", 10, 4, "\x1b[44mA", 75),
        ("hyperlink", 10, 4, "\x1b]8;;http://x\x1b\\A", 75),
        ("alt screen", 10, 4, "\x1b[?1049hA", 65535),
    ];
    for &(label, cols, rows, setup, n) in cases {
        assert_rep_matches(label, cols, rows, no_rep_prep, &one(setup, n));
    }
}

#[test]
fn rep_matches_per_scalar_with_scrollback_limits_selection_and_viewport() {
    // Line limits stay under 8: above that, which page boundary eviction
    // lands on depends on when the background packer finishes a batch
    // (`PagedScrollback::deferred_boundary` never waits on it), so even two
    // reference runs can retain different row counts. At `limit / 4 <= 1`
    // every push past the first flushes synchronously.
    let fill = "line\r\n".repeat(20);
    let small_limit = |t: &mut Terminal| t.set_scrollback_limit_lines(7);
    assert_rep_matches("evicting scrollback", 10, 4, small_limit, &one("A", 65535));
    assert_rep_matches("evicting scrollback wide", 9, 4, small_limit, &one("漢", 65535));
    let scrolled = format!("{fill}A");
    let rounds = vec![(scrolled.into_bytes(), 300)];
    let selected = |t: &mut Terminal| {
        t.set_scrollback_limit_lines(7);
        t.primary.set_viewport_selection(Point { x: 1, y: 0 }, Point { x: 3, y: 2 });
    };
    assert_rep_matches("selection", 10, 4, selected, &rounds);
    let viewport = |t: &mut Terminal| {
        let mut s = Stream::new();
        s.feed("x\r\n".repeat(10).as_bytes(), t);
        t.primary.scroll_viewport_up(3);
    };
    assert_rep_matches("scrolled viewport", 10, 4, viewport, &one("A", 200));
    let locked = |t: &mut Terminal| t.primary.set_viewport_locked(true);
    assert_rep_matches("locked viewport", 10, 4, locked, &one("A", 200));
    // A recorded partial region (top row 0) leaves the rows below it in
    // place, so a selection there moves with each push into scrollback.
    let below = |t: &mut Terminal| {
        t.set_scrollback_limit_lines(7);
        t.primary.set_viewport_selection(Point { x: 1, y: 4 }, Point { x: 3, y: 5 });
    };
    for c in ["A", "漢"] {
        let setup = format!("\x1b[1;4r\x1b[4;1H{c}");
        let rounds = vec![(setup.into_bytes(), 300)];
        assert_rep_matches(&format!("selection below region {c}"), 10, 6, below, &rounds);
        let rounds = vec![(format!("{fill}\x1b[1;4r\x1b[4;1H{c}").into_bytes(), 300)];
        assert_rep_matches(&format!("scrolled viewport region {c}"), 10, 6, viewport, &rounds);
        assert_rep_matches(&format!("locked viewport region {c}"), 10, 6, locked, &rounds);
        // Locked at the live bottom: only a full-height push pins it.
        let rounds = one(&format!("\x1b[1;4r\x1b[4;1H{c}"), 300);
        assert_rep_matches(&format!("locked live viewport region {c}"), 10, 6, locked, &rounds);
    }
}

#[test]
fn rep_matches_per_scalar_with_kitty_placements() {
    let with_metrics = |t: &mut Terminal| {
        t.set_scrollback_limit_lines(7);
        t.set_pixel_metrics(10, 20, 100, 120);
    };
    let place = |setup: &str| {
        let mut b64 = Vec::new();
        // 20 rows tall: still partly on screen once REP reaches its
        // steady state, so every skipped scroll must still move it.
        crate::osc::encode_base64(&vec![0u8; 10 * 400 * 4], &mut b64);
        let mut out = setup.as_bytes().to_vec();
        out.extend_from_slice(b"\x1b[2;2H\x1b_Ga=T,f=32,s=10,v=400,i=1,C=1;");
        out.extend_from_slice(&b64);
        out.extend_from_slice(b"\x1b\\\x1b[4;1HA");
        out
    };
    for (label, setup) in [
        ("primary", ""),
        ("alt screen", "\x1b[?1049h"),
        ("partial region", "\x1b[1;5r"),
        ("region below top", "\x1b[2;6r"),
    ] {
        for n in [5, 30, 95, 65535] {
            let rounds = vec![(place(setup), n)];
            assert_rep_matches(&format!("kitty {label}"), 10, 6, with_metrics, &rounds);
        }
    }
}

/// xorshift64*: deterministic, dependency-free scenario generator.
struct RepRng(u64);

impl RepRng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }

    fn pick<'a>(&mut self, items: &[&'a str]) -> &'a str {
        items[self.below(items.len() as u64) as usize]
    }
}

/// A random setup: text, cursor moves and mode/margin changes, ending in a
/// print of a random scalar so REP has something to repeat.
fn random_setup(rng: &mut RepRng, cols: u16, rows: u16) -> Vec<u8> {
    let mut out = String::new();
    for _ in 0..rng.below(8) {
        let col = rng.below(u64::from(cols) + 2) + 1;
        let row = rng.below(u64::from(rows) + 2) + 1;
        match rng.below(16) {
            0..=2 => {
                for _ in 0..rng.below(2 * u64::from(cols)) {
                    out.push_str(rng.pick(&["a", "b", "漢", "é", " ", "😀", "\u{301}"]));
                }
            }
            3..=5 => out.push_str(&format!("\x1b[{row};{col}H")),
            6 => {
                let top = rng.below(u64::from(rows)) + 1;
                let bottom = top + rng.below(u64::from(rows) + 1 - top);
                out.push_str(&format!("\x1b[{top};{bottom}r"));
            }
            7 => {
                let left = rng.below(u64::from(cols)) + 1;
                let right = left + rng.below(u64::from(cols) + 1 - left);
                out.push_str(&format!("\x1b[?69h\x1b[{left};{right}s"));
            }
            8 => out.push_str(rng.pick(&["\x1b[?7l", "\x1b[?7h"])),
            9 => out.push_str(rng.pick(&["\x1b[4h", "\x1b[4l"])),
            10 => out.push_str(rng.pick(&["\x1b[?2027h", "\x1b[?2027l"])),
            11 => out.push_str(rng.pick(&["\x1b[?6h", "\x1b[?6l"])),
            12 => out.push_str(rng.pick(&["\x1b[31m", "\x1b[44m", "\x1b[0m", "\x1b[1;7m"])),
            13 => out.push_str(rng.pick(&["\x1b[?69l", "\x1b[r"])),
            14 => out.push_str(&format!("\x1b[{row};{col}H{}", rng.pick(REP_CHARS))),
            _ => out.push_str(rng.pick(&["\r\n", "\x1b[?1049h", "\x1b[?1049l"])),
        }
    }
    out.push_str(rng.pick(REP_CHARS));
    out.into_bytes()
}

#[test]
fn rep_matches_per_scalar_for_random_scenarios() {
    let mut rng = RepRng(0x9e37_79b9_7f4a_7c15);
    for case in 0..1000 {
        const COLS: [u16; 9] = [1, 2, 3, 5, 8, 9, 10, 13, 80];
        let cols = COLS[rng.below(COLS.len() as u64) as usize];
        let rows = rng.below(6) as u16 + 1;
        let limit = rng.below(4);
        let prep = move |t: &mut Terminal| match limit {
            0 => t.set_scrollback_limit_lines(5),
            1 => t.set_scrollback_limit_lines(0),
            2 => t.set_scrollback_limit_lines(2),
            _ => {}
        };
        // The comparison walks every scrollback row: keep the unlimited
        // default from holding tens of thousands of one-column rows.
        let max_n = if limit == 3 { 2000 } else { u16::MAX };
        let mut rounds = Vec::new();
        for _ in 0..rng.below(3) + 1 {
            let setup = random_setup(&mut rng, cols, rows);
            // The maximum runs ~0.4 s per case in a debug build (the
            // reference loop plus the row-by-row comparison), so it is rare;
            // at a few columns it would scroll tens of thousands of rows.
            let n = match rng.below(20) {
                0 if cols >= 8 => 65535,
                0 => rng.below(5000) as u16 + 1,
                1..=6 => rng.below(1000) as u16 + 1,
                7 => 0,
                _ => rng.below(3 * u64::from(cols) + 3) as u16 + 1,
            };
            rounds.push((setup, n.min(max_n)));
        }
        assert_rep_matches(&format!("random case {case} {cols}x{rows}"), cols, rows, prep, &rounds);
    }
}

/// The freeze this fast path removes: one 64 KiB pty chunk of
/// `<scalar> CSI 65535 b`, fed under the terminal lock. Times the chunk
/// through `Stream::feed` on the primary screen (rows sealed into
/// scrollback), the alternate screen and a partial scroll region (rows
/// discarded), against the per-scalar reference loop on the primary screen.
#[test]
#[ignore = "benchmark; run with `--release --ignored --nocapture`"]
fn rep_flood_chunk_benchmark() {
    use noa_vt::Handler as _;
    for (cols, rows) in [(80u16, 24u16), (200, 50)] {
        for c in ["A", "漢"] {
            let unit = format!("{c}\x1b[65535b");
            let reps = 65536 / unit.len();
            let chunk = unit.repeat(reps);
            let region = format!("\x1b[2;{rows}r\x1b[{rows};1H");
            let mut timings = Vec::new();
            for (label, setup) in [("primary", ""), ("alt", "\x1b[?1049h"), ("region", &region)] {
                let mut t = Terminal::new(GridSize::new(cols, rows));
                let mut s = Stream::new();
                s.feed(setup.as_bytes(), &mut t);
                let start = std::time::Instant::now();
                s.feed(chunk.as_bytes(), &mut t);
                timings.push(format!("{label} {:?}", start.elapsed()));
            }

            let mut r = Terminal::new(GridSize::new(cols, rows));
            let ch = c.chars().next().expect("one scalar");
            let start = std::time::Instant::now();
            for _ in 0..reps {
                r.print(ch);
                apply_rep(&mut r, 65535, true);
            }
            let reference = start.elapsed();

            eprintln!(
                "{cols}x{rows} {c:?} x{reps} ({} B): {}; per-scalar primary {reference:?}",
                chunk.len(),
                timings.join(", "),
            );
        }
    }
}
