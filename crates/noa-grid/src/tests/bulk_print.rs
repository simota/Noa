// ── bulk print fast path (`Handler::print_str`) equivalence ─────────
//
// `Stream::feed` hands ground-state text runs to `Terminal::print_str`
// (→ `Screen::print_ascii_run`) in bulk. These differential tests pin the
// contract that the bulk path is byte-for-byte equivalent to calling
// `Handler::print` once per scalar, across the states that shape printing.

/// Feed `setup` (may contain escapes) to two fresh terminals, then print
/// `text` (printable scalars only) through the bulk stream path on one and
/// per-scalar `Handler::print` on the other, and assert identical state.
fn assert_bulk_print_matches_per_scalar(cols: u16, rows: u16, setup: &[u8], text: &str) {
    use noa_vt::Handler as _;

    let mut bulk = Terminal::new(GridSize::new(cols, rows));
    let mut s = Stream::new();
    s.feed(setup, &mut bulk);
    s.feed(text.as_bytes(), &mut bulk);

    let mut per = Terminal::new(GridSize::new(cols, rows));
    let mut s = Stream::new();
    s.feed(setup, &mut per);
    for c in text.chars() {
        per.print(c);
    }

    let (b, p) = (bulk.active(), per.active());
    assert_eq!(
        (b.cursor.x, b.cursor.y, b.cursor.pending_wrap),
        (p.cursor.x, p.cursor.y, p.cursor.pending_wrap),
        "cursor diverged for setup {setup:?} text {text:?}"
    );
    for (y, (br, pr)) in b.grid.iter().zip(p.grid.iter()).enumerate() {
        assert_eq!(br.cells, pr.cells, "row {y} cells diverged for {text:?}");
        assert_eq!(br.wrapped, pr.wrapped, "row {y} wrapped flag diverged");
    }
}

#[test]
fn bulk_print_matches_per_scalar_for_plain_wrap() {
    assert_bulk_print_matches_per_scalar(10, 4, b"", &"abcdefghij".repeat(2));
    assert_bulk_print_matches_per_scalar(10, 4, b"", &"x".repeat(25));
    // Ends exactly on the last column: the deferred-wrap latch must match.
    assert_bulk_print_matches_per_scalar(10, 4, b"", &"y".repeat(10));
}

#[test]
fn bulk_print_matches_per_scalar_with_autowrap_off() {
    assert_bulk_print_matches_per_scalar(10, 4, b"\x1b[?7l", &"z".repeat(30));
}

#[test]
fn bulk_print_matches_per_scalar_inside_horizontal_margins() {
    // DECLRMM + DECSLRM 3..6, cursor inside the margins.
    assert_bulk_print_matches_per_scalar(10, 4, b"\x1b[?69h\x1b[3;6s\x1b[1;4H", "abcdefghijkl");
    // Cursor right of the margin wraps at the screen edge.
    assert_bulk_print_matches_per_scalar(10, 4, b"\x1b[?69h\x1b[3;6s\x1b[1;9H", "abc");
}

#[test]
fn bulk_print_matches_per_scalar_across_horizontal_margin_boundaries() {
    for col in [1, 2, 3, 7, 8, 9, 10] {
        for autowrap in [true, false] {
            let setup = format!(
                "\x1b[?69h\x1b[3;7s\x1b[?7{}\x1b[1;{col}H",
                if autowrap { 'h' } else { 'l' }
            );
            for text in ["abcdefghijkl", "日本語日本語", "❤\u{fe0f}AB"] {
                assert_bulk_print_matches_per_scalar(10, 6, setup.as_bytes(), text);
            }
        }
    }
}

#[test]
fn bulk_print_matches_per_scalar_overwriting_wide_cells() {
    let setup = "日本語\x1b[1;2H".as_bytes();
    assert_bulk_print_matches_per_scalar(80, 24, setup, "XY");
    let setup = "日本語\r".as_bytes();
    assert_bulk_print_matches_per_scalar(80, 24, setup, "abcde");
}

#[test]
fn bulk_print_matches_per_scalar_with_sgr_pen() {
    assert_bulk_print_matches_per_scalar(20, 4, b"\x1b[31;43;4;1m", &"pen".repeat(10));
}

#[test]
fn bulk_print_matches_per_scalar_for_mixed_utf8() {
    assert_bulk_print_matches_per_scalar(12, 6, b"", "abc日本語def🐱g");
    assert_bulk_print_matches_per_scalar(80, 24, b"", "コンバイン: a\u{0301}e\u{0301}");
}

#[test]
fn bulk_print_matches_per_scalar_extending_a_cluster_under_mode_2027() {
    // A trailing ZWJ makes the *next* scalar — even ASCII — extend the
    // cluster instead of printing a new cell.
    let setup = "\x1b[?2027h👩\u{200D}".as_bytes();
    assert_bulk_print_matches_per_scalar(80, 24, setup, "xy");
}

#[test]
fn bulk_print_matches_per_scalar_with_dec_special_graphics() {
    // `ESC ( 0` designates DEC line-drawing: `q` → `─` etc.
    assert_bulk_print_matches_per_scalar(20, 4, b"\x1b(0", "qxlkj abc");
}

#[test]
fn bulk_print_updates_last_printed_for_rep() {
    // REP (CSI b) repeats the last printed scalar — which the bulk path
    // must have recorded from the run's final byte.
    let t = run(b"ab\x1b[3b");
    assert_eq!(row_text(&t, 0, 6), "abbbb ");
}

#[test]
fn bulk_print_row_dirty_marks_every_touched_row() {
    let t = run_size(10, 4, &"d".repeat(25).into_bytes());
    assert!(t.primary.grid[0].dirty);
    assert!(t.primary.grid[1].dirty);
    assert!(t.primary.grid[2].dirty);
}

/// Manual throughput probe (not a regression gate — wall-clock):
/// `cargo test -p noa-grid --release -- --ignored throughput --nocapture`
#[test]
#[ignore = "manual wall-clock throughput probe"]
fn bulk_print_throughput_probe() {
    let line = "the quick brown fox jumps over the lazy dog 0123456789 ";
    let ascii: Vec<u8> = format!("{}\r\n", line.repeat(3)).into_bytes();
    let jp: Vec<u8> =
        format!("{}\r\n", "日本語のテキスト出力を含む行です。".repeat(6)).into_bytes();
    for (name, chunk) in [("ascii", &ascii), ("utf8", &jp)] {
        let mut t = Terminal::new(GridSize::new(120, 40));
        let mut s = Stream::new();
        let iterations = 200_000;
        let start = std::time::Instant::now();
        for _ in 0..iterations {
            s.feed(chunk, &mut t);
        }
        let elapsed = start.elapsed();
        let bytes = chunk.len() * iterations;
        eprintln!(
            "{name}: {:.1} MB in {elapsed:?} → {:.0} MB/s",
            bytes as f64 / 1e6,
            bytes as f64 / 1e6 / elapsed.as_secs_f64()
        );
    }
}

/// Manual stream-level scroll throughput probe (not a regression gate —
/// wall-clock): reconstructs the previous optimization round's
/// `bench/150MB_*.txt` workload in-process (same line pool as
/// `bench/generate_data.py`, no external data files needed) and feeds it
/// through one long-lived `Stream` into a 200x60 `Terminal` with scrollback
/// enabled, so every line past the initial 60 rows exercises
/// `PagedScrollback::push_row` — the architectural-round bottleneck.
/// `cargo test -p noa-grid --release --offline stream_scroll_throughput_probe -- --ignored --nocapture`
#[test]
#[ignore = "manual wall-clock throughput probe"]
fn stream_scroll_throughput_probe() {
    let ascii_lines = [
        "The quick brown fox jumps over the lazy dog.",
        "Ghostty is now undeniably the fastest terminal emulator in IO throughput.",
        "Lorem ipsum dolor sit amet, consectetur adipiscing elit.",
        "ASCII, Unicode, and CSI tests show Ghostty is more than 2x faster.",
        "0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ",
        "We are pair programming with a USER to solve their coding task.",
        "Speed improvements apply directly to libghostty-vt users.",
        "Testing various shapes of input: plain ASCII, heavy CSI, Unicode.",
    ];
    let unicode_lines = [
        "Ghostty は今や、IO スループットにおいて紛れもなく最速のターミナルエミュレータであり、圧倒的な差をつけています。",
        "ASCII、Unicode、CSI テストにおいて、Ghostty は他の主要な「高速」ターミナルよりも 2 倍以上速いです。",
        "これらの変更は libghostty に直接適用されているため、皆が得をします。 🚀🔥",
        "Hello World! こんにちは世界！ 안녕하세요! こんにちは！ Salut le monde!",
        "日本語と English と 🦀 Rust と 🐍 Python が混ざったテキストです。",
        "Wide characters: 繁體中文 简体中文 한국어日本語 Русский 𐎪𐎫𐎬",
        "Emoji test: 🌍🔥🚀💻⚡️🎨📈🛠️👁️‍🗨️",
        "CSI test cases and Unicode combined: \x1b[31mRed Text\x1b[0m and \x1b[32mGreen Text\x1b[0m.",
    ];

    fn build(lines: &[&str], target_bytes: usize) -> Vec<u8> {
        let mut buf = Vec::with_capacity(target_bytes + 4096);
        let mut i = 0usize;
        while buf.len() < target_bytes {
            buf.extend_from_slice(lines[i % lines.len()].as_bytes());
            buf.push(b'\n');
            i += 1;
        }
        buf
    }

    let target = 150 * 1024 * 1024;
    for (name, lines) in [("ascii", &ascii_lines[..]), ("unicode", &unicode_lines[..])] {
        let data = build(lines, target);
        let mut t = Terminal::new(GridSize::new(200, 60));
        let mut s = Stream::new();
        const CHUNK: usize = 1024 * 1024;
        let start = std::time::Instant::now();
        for chunk in data.chunks(CHUNK) {
            s.feed(chunk, &mut t);
            let _ = t.take_pending_writes();
        }
        let elapsed = start.elapsed();
        let mib = data.len() as f64 / (1024.0 * 1024.0);
        println!(
            "{name}: {mib:.1} MiB in {elapsed:?} = {:.1} MiB/s (200x60, scrollback {} rows)",
            mib / elapsed.as_secs_f64(),
            t.scrollback_len()
        );
    }
}

#[test]
fn bulk_wide_run_matches_per_scalar_for_cjk_wrap() {
    // Even columns: wide scalars tile exactly; odd columns: every row ends
    // with one spare cell, forcing the wide-at-margin wrap each row.
    for cols in [10, 11] {
        assert_bulk_print_matches_per_scalar(cols, 6, b"", &"日本語のテキスト出力".repeat(4));
    }
    // Ends exactly at the margin: the deferred-wrap latch must match.
    assert_bulk_print_matches_per_scalar(10, 4, b"", "あいうえお");
}

#[test]
fn bulk_wide_run_matches_per_scalar_with_autowrap_off() {
    assert_bulk_print_matches_per_scalar(10, 4, b"\x1b[?7l", &"漢".repeat(12));
}

#[test]
fn bulk_wide_run_matches_per_scalar_inside_horizontal_margins() {
    assert_bulk_print_matches_per_scalar(12, 4, b"\x1b[?69h\x1b[3;8s\x1b[1;4H", "日本語のテキスト");
    // Degenerate one-column margin region: falls back to the per-scalar path.
    assert_bulk_print_matches_per_scalar(12, 4, b"\x1b[?69h\x1b[5;5s\x1b[1;5H", "日本");
}

#[test]
fn bulk_wide_run_matches_per_scalar_overwriting_and_mixed() {
    // Wide over narrow, narrow over wide, and alternating segments.
    assert_bulk_print_matches_per_scalar(20, 6, "abcdefghij\r".as_bytes(), "日本語");
    assert_bulk_print_matches_per_scalar(20, 6, "日本語あいう\r".as_bytes(), "xy日z本語w");
    assert_bulk_print_matches_per_scalar(20, 6, b"\x1b[35;44;3m", "混ぜmix交ぜmix");
}

#[test]
fn bulk_wide_run_matches_per_scalar_extending_a_cluster_under_mode_2027() {
    // Trailing ZWJ: the run's first wide scalar extends the cluster instead
    // of printing a new cell.
    let setup = "\x1b[?2027h👩\u{200D}".as_bytes();
    assert_bulk_print_matches_per_scalar(80, 24, setup, "👧日本");
    // Fitzpatrick modifiers are wide but cluster-extending: they must stay
    // per-scalar and attach, not print into fresh cells.
    assert_bulk_print_matches_per_scalar(80, 24, "\x1b[?2027h👍".as_bytes(), "\u{1F3FD}日本");
}

#[test]
fn wide_graphemes_match_across_stream_feed_partitions() {
    for setup in ["", "\x1b[?2027l", "\x1b[?7l", "\x1b[?69h\x1b[2;5s\x1b[1;2H"] {
        for text in [
            "ᄀᄀX",
            "ᄀᄀᄀᄀX",
            "ᄀ가X",
            "\u{a960}\u{a960}X",
            "日本ᄀᄀX",
            "日本ᄀᄀᄀ日本X",
        ] {
            let mut expected = Terminal::new(GridSize::new(6, 3));
            let mut stream = Stream::new();
            stream.feed(setup.as_bytes(), &mut expected);
            for byte in text.bytes() {
                stream.feed(&[byte], &mut expected);
            }
            for split in 0..=text.len() {
                let mut actual = Terminal::new(GridSize::new(6, 3));
                let mut stream = Stream::new();
                stream.feed(setup.as_bytes(), &mut actual);
                stream.feed(&text.as_bytes()[..split], &mut actual);
                stream.feed(&text.as_bytes()[split..], &mut actual);
                let (a, e) = (actual.active(), expected.active());
                assert_eq!(
                    (a.cursor.x, a.cursor.y, a.cursor.pending_wrap),
                    (e.cursor.x, e.cursor.y, e.cursor.pending_wrap),
                    "{text:?}, split {split}, setup {setup:?}"
                );
                for (ar, er) in a.grid.iter().zip(e.grid.iter()) {
                    assert_eq!(
                        ar.cells, er.cells,
                        "{text:?}, split {split}, setup {setup:?}"
                    );
                    assert_eq!(ar.wrapped, er.wrapped);
                }
            }
        }
    }
    let t = run_size(12, 1, "ᄀᄀX".as_bytes());
    assert_eq!(cell(&t, 0, 0).text(), "ᄀᄀ");
    assert_eq!(cell(&t, 2, 0).ch, 'X');
    assert_eq!(t.primary.cursor.x, 3);
    let mut direct = Terminal::new(GridSize::new(12, 1));
    direct.primary.print_wide_run("ᄀᄀ", true, true);
    direct.primary.print('X', true, true);
    assert_eq!(direct.primary.cursor.x, 3);
    assert_eq!(direct.primary.grid[0].cells, t.primary.grid[0].cells);
}

#[test]
fn bulk_wide_candidates_have_boundaries_between_repeated_scalars() {
    use unicode_segmentation::UnicodeSegmentation as _;
    for ch in (0..=0x10ffff).filter_map(char::from_u32) {
        if crate::screen::Screen::is_plain_wide(ch) {
            let text = format!("{ch}{ch}");
            assert_eq!(
                text.graphemes(true).count(),
                2,
                "U+{:04X} must take the cluster path",
                ch as u32
            );
        }
    }
}

#[test]
fn bulk_narrow_run_matches_per_scalar_for_wrap_and_autowrap_off() {
    for cols in [7, 10] {
        assert_bulk_print_matches_per_scalar(cols, 6, b"", &"Русский текст αβγ".repeat(4));
    }
    // Ends exactly at the margin: the deferred-wrap latch must match.
    assert_bulk_print_matches_per_scalar(10, 4, b"", &"ж".repeat(10));
    assert_bulk_print_matches_per_scalar(10, 4, b"\x1b[?7l", &"щ".repeat(25));
}

#[test]
fn bulk_narrow_run_matches_per_scalar_inside_and_across_margins() {
    for col in [1, 3, 5, 7, 9] {
        let setup = format!("\x1b[?69h\x1b[3;7s\x1b[1;{col}H");
        assert_bulk_print_matches_per_scalar(10, 6, setup.as_bytes(), "абвгдежзийкл");
    }
}

#[test]
fn bulk_narrow_run_matches_per_scalar_overwriting_wide_and_mixed() {
    assert_bulk_print_matches_per_scalar(20, 6, "日本語あいう\x1b[1;2H".as_bytes(), "абвгд");
    assert_bulk_print_matches_per_scalar(20, 6, "日本語\r".as_bytes(), "ы日ж\u{0301}з📢й");
    assert_bulk_print_matches_per_scalar(20, 6, b"\x1b[32;41;1m", "смесь mix 𐎪𐎫𐎬 €");
}

#[test]
fn bulk_narrow_run_matches_per_scalar_extending_a_cluster_under_mode_2027() {
    // Trailing ZWJ: a narrow pictograph prefix extends the cluster.
    let setup = "\x1b[?2027h👩\u{200D}".as_bytes();
    assert_bulk_print_matches_per_scalar(80, 24, setup, "☺жз");
    let setup = "\x1b[?2027hé".as_bytes();
    assert_bulk_print_matches_per_scalar(80, 24, setup, "\u{0301}жз");
}
