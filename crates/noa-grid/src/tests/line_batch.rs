// ── ground-state line-batch (`Handler::print_ascii_lines`) equivalence ──
//
// `Stream::feed` hands whole complete-line spans to
// `Terminal::print_ascii_lines`, which applies them as one batched scroll
// (`Screen::apply_ascii_line_batch`). These differential tests pin the
// contract that the batched path leaves observable state identical to the
// per-action path: the reference terminal is fed one byte per
// `Stream::feed` call, which can never see two complete lines in one chunk
// and therefore never batches.

fn feed_per_byte(t: &mut Terminal, s: &mut Stream, bytes: &[u8]) {
    for b in bytes {
        s.feed(std::slice::from_ref(b), t);
    }
}

/// Compare every observable piece of screen state between the batched and
/// per-byte terminals.
fn assert_batch_state_matches(
    label: &str,
    batched: &mut Terminal,
    scalar: &Terminal,
    scalar_shift: usize,
) {
    batched.primary.trim_memory();
    let shift = batched.primary.take_scroll_shift();
    assert_eq!(shift, scalar_shift, "{label}: scroll_shift");
    let (b, s) = (batched.active(), scalar.active());
    assert_eq!(
        (b.cursor.x, b.cursor.y, b.cursor.pending_wrap),
        (s.cursor.x, s.cursor.y, s.cursor.pending_wrap),
        "{label}: cursor"
    );
    assert_eq!(b.scrollback_len(), s.scrollback_len(), "{label}: scrollback_len");
    assert_eq!(b.rows_evicted(), s.rows_evicted(), "{label}: rows_evicted");
    assert_eq!(b.viewport_offset(), s.viewport_offset(), "{label}: viewport_offset");
    assert_eq!(b.selection, s.selection, "{label}: selection");
    assert_eq!(b.last_printed(), s.last_printed(), "{label}: last_printed");
    assert_eq!(b.total_rows(), s.total_rows(), "{label}: total_rows");
    for y in 0..s.total_rows() {
        let br = b.absolute_row(y).expect("row in range");
        let sr = s.absolute_row(y).expect("row in range");
        assert_eq!(br.wrapped, sr.wrapped, "{label}: row {y} wrapped");
        if br.cells != sr.cells {
            for (x, (bc, sc)) in br.cells.iter().zip(&sr.cells).enumerate() {
                assert_eq!(bc, sc, "{label}: row {y} col {x}");
            }
        }
    }
    for (y, (br, sr)) in b.grid.iter().zip(&s.grid).enumerate() {
        assert_eq!(br.dirty, sr.dirty, "{label}: grid row {y} dirty");
        assert_eq!(br.occupied(), sr.occupied(), "{label}: grid row {y} occupancy");
    }
}

/// Feed `setup` then `body` into a per-byte reference terminal and into
/// batched terminals at several chunkings, asserting identical final state.
/// `prep` runs on each terminal between setup and body (limits, selection,
/// viewport, dirty-flag resets).
fn assert_line_batch_equivalence(
    label: &str,
    cols: u16,
    rows: u16,
    setup: &[u8],
    prep: impl Fn(&mut Terminal),
    body: &[u8],
) {
    let mut scalar = Terminal::new(GridSize::new(cols, rows));
    let mut ss = Stream::new();
    feed_per_byte(&mut scalar, &mut ss, setup);
    prep(&mut scalar);
    feed_per_byte(&mut scalar, &mut ss, body);
    scalar.primary.trim_memory();
    let scalar_shift = scalar.primary.take_scroll_shift();

    for &chunk in &[usize::MAX, 4096, 97, 33] {
        let mut batched = Terminal::new(GridSize::new(cols, rows));
        let mut bs = Stream::new();
        feed_per_byte(&mut batched, &mut bs, setup);
        prep(&mut batched);
        if chunk == usize::MAX {
            bs.feed(body, &mut batched);
        } else {
            for c in body.chunks(chunk) {
                bs.feed(c, &mut batched);
            }
        }
        assert_batch_state_matches(
            &format!("{label} chunk={chunk}"),
            &mut batched,
            &scalar,
            scalar_shift,
        );
    }
}

/// `lines` printable-ASCII lines of varied lengths (`0..cols + 20`-ish,
/// including widths that soft-wrap) each ending with `term`.
fn line_flood(lines: usize, term: &str, seed: usize) -> Vec<u8> {
    let mut out = Vec::new();
    for i in 0..lines {
        let len = (i * 7 + seed) % 100;
        for k in 0..len {
            out.push(b'a' + ((i + k) % 26) as u8);
        }
        out.extend_from_slice(term.as_bytes());
    }
    out
}

fn no_prep(_t: &mut Terminal) {}

fn clear_dirty(t: &mut Terminal) {
    for row in &mut t.primary.grid {
        row.dirty = false;
    }
}

#[test]
fn line_batch_matches_per_byte_for_crlf_floods() {
    assert_line_batch_equivalence(
        "crlf flood",
        80,
        24,
        b"",
        clear_dirty,
        &line_flood(120, "\r\n", 0),
    );
    // Widths hugging the wrap boundary: cols-1 / cols / cols+1.
    let mut body = Vec::new();
    for len in [79usize, 80, 81, 79, 80, 81, 0, 1] {
        body.extend(std::iter::repeat_n(b'w', len));
        body.extend_from_slice(b"\r\n");
    }
    assert_line_batch_equivalence("crlf wrap-boundary widths", 80, 24, b"", no_prep, &body);
}

#[test]
fn line_batch_matches_per_byte_for_bare_lf_floods() {
    // Bare LF without LNM: the cursor column carries across lines (staircase
    // output) and long lines soft-wrap mid-batch.
    assert_line_batch_equivalence(
        "bare-lf flood",
        80,
        24,
        b"",
        no_prep,
        &line_flood(120, "\n", 3),
    );
    // The drain benchmark shape: full-width-ish lines, bare LF.
    let mut body = Vec::new();
    for len in [79usize, 80, 81, 81] {
        for _ in 0..30 {
            body.extend(std::iter::repeat_n(b'd', len));
            body.push(b'\n');
        }
    }
    assert_line_batch_equivalence("bare-lf full-width flood", 80, 24, b"", no_prep, &body);
}

#[test]
fn line_batch_matches_per_byte_with_linefeed_newline_mode() {
    assert_line_batch_equivalence(
        "lnm flood",
        80,
        24,
        b"\x1b[20h",
        no_prep,
        &line_flood(80, "\n", 5),
    );
}

#[test]
fn line_batch_matches_per_byte_for_empty_and_boundary_line_counts() {
    assert_line_batch_equivalence(
        "empty lines",
        80,
        24,
        b"",
        no_prep,
        b"\n\n\n\r\n\r\nhi\n\n\r\nlast\r\n\n",
    );
    // Line counts around the region height on a small grid (L = 4): the
    // batch's seal/rotate/fill split shifts shape at K = L-1, L, L+1.
    for n in 1..=10usize {
        assert_line_batch_equivalence(
            &format!("small grid {n} lines"),
            10,
            4,
            b"",
            clear_dirty,
            &line_flood(n, "\r\n", 1),
        );
    }
}

#[test]
fn line_batch_matches_per_byte_with_sgr_pen_and_bce() {
    let mut body = Vec::new();
    for i in 0..60usize {
        if i % 4 == 0 {
            body.extend_from_slice(b"\x1b[31;44;1m");
        }
        if i % 4 == 2 {
            body.extend_from_slice(b"\x1b[0m");
        }
        body.extend(std::iter::repeat_n(b'c', (i * 5) % 90));
        body.extend_from_slice(b"\r\n");
    }
    assert_line_batch_equivalence("sgr+bce flood", 80, 24, b"", no_prep, &body);
}

#[test]
fn line_batch_matches_per_byte_for_scroll_regions() {
    // Partial region (top > 0): records no scrollback; pass-through rows
    // are dropped rather than sealed.
    assert_line_batch_equivalence(
        "partial region",
        80,
        24,
        b"\x1b[3;10r\x1b[10;1H",
        no_prep,
        &line_flood(60, "\n", 2),
    );
    // Top-anchored region shorter than the screen: records scrollback but
    // is not a pure viewport translation; tracked points shift per scroll.
    assert_line_batch_equivalence(
        "top-anchored region with selection",
        80,
        24,
        b"\x1b[1;10r\x1b[10;1H",
        |t| {
            t.primary.set_selection(
                crate::selection::SelectionPoint::new(2, 12),
                crate::selection::SelectionPoint::new(9, 14),
            );
        },
        &line_flood(60, "\r\n", 4),
    );
}

#[test]
fn line_batch_matches_per_byte_on_the_alt_screen() {
    assert_line_batch_equivalence(
        "alt screen",
        80,
        24,
        b"\x1b[?1049h\x1b[24;1H",
        no_prep,
        &line_flood(60, "\r\n", 6),
    );
}

#[test]
fn line_batch_matches_per_byte_with_scrollback_disabled() {
    assert_line_batch_equivalence(
        "scrollback limit 0",
        80,
        24,
        b"",
        |t| t.set_scrollback_limit_bytes(0),
        &line_flood(80, "\r\n", 7),
    );
}

#[test]
fn line_batch_matches_per_byte_over_wide_remnants() {
    // Wide pairs on the bottom row, cursor parked above: the replayed lines
    // march the cursor down and the batch prefix prints over the remnants
    // with the real wide-cell cleanup.
    assert_line_batch_equivalence(
        "wide remnants under the prefix",
        80,
        24,
        "\x1b[24;1H日本語漢字テスト\x1b[22;3H".as_bytes(),
        no_prep,
        &line_flood(40, "\n", 8),
    );
}

#[test]
fn line_batch_matches_per_byte_with_mixed_utf8_lines() {
    let mut body = Vec::new();
    for i in 0..40usize {
        if i % 3 == 0 {
            body.extend_from_slice("漢字ワイド行です\n".as_bytes());
        } else {
            body.extend(std::iter::repeat_n(b'm', (i * 11) % 85));
            body.extend_from_slice(b"\r\n");
        }
    }
    assert_line_batch_equivalence("mixed utf8 lines", 80, 24, b"", no_prep, &body);
}

#[test]
fn line_batch_matches_per_byte_from_a_mid_screen_cursor() {
    // Precondition fails until the replayed lines reach the region bottom,
    // then the batch takes over mid-span with a text-led prefix.
    assert_line_batch_equivalence(
        "mid-screen cursor",
        80,
        24,
        b"\x1b[5;7H",
        no_prep,
        &line_flood(50, "\r\n", 9),
    );
}

#[test]
fn line_batch_matches_per_byte_with_the_wrap_latch_set() {
    let mut setup = b"\x1b[24;1H".to_vec();
    setup.extend(std::iter::repeat_n(b'q', 80)); // exactly cols: latch engages
    assert_line_batch_equivalence(
        "pending-wrap entry",
        80,
        24,
        &setup,
        no_prep,
        &line_flood(30, "\r\n", 2),
    );
    assert_line_batch_equivalence(
        "pending-wrap entry, bare LF",
        80,
        24,
        &setup,
        no_prep,
        &line_flood(30, "\n", 2),
    );
}

#[test]
fn line_batch_matches_per_byte_inside_horizontal_margins() {
    // DECLRMM margins disqualify the batch: every line replays.
    assert_line_batch_equivalence(
        "horizontal margins",
        80,
        24,
        b"\x1b[?69h\x1b[3;60s\x1b[24;5H",
        no_prep,
        &line_flood(40, "\r\n", 3),
    );
}

#[test]
fn line_batch_matches_per_byte_with_autowrap_off() {
    assert_line_batch_equivalence(
        "autowrap off",
        80,
        24,
        b"\x1b[?7l",
        no_prep,
        &line_flood(60, "\n", 4),
    );
}

#[test]
fn line_batch_matches_per_byte_with_grapheme_clustering_on() {
    // Mode 2027 with a trailing ZWJ on the bottom row: the first replayed
    // scalar extends the cluster on both paths.
    assert_line_batch_equivalence(
        "mode 2027 cluster at entry",
        80,
        24,
        "\x1b[?2027h\x1b[24;1H👩\u{200D}".as_bytes(),
        no_prep,
        &line_flood(30, "\r\n", 5),
    );
}

#[test]
fn line_batch_matches_per_byte_with_a_pinned_viewport() {
    assert_line_batch_equivalence(
        "pinned viewport",
        80,
        24,
        &line_flood(100, "\r\n", 0),
        |t| t.primary.scroll_viewport_up(5),
        &line_flood(50, "\r\n", 1),
    );
}

#[test]
fn line_batch_matches_per_byte_with_rep_and_tabs() {
    let mut body = Vec::new();
    body.extend_from_slice(b"alpha\r\nbeta\r\n");
    body.extend_from_slice(b"\x1b[5b\r\n"); // REP repeats the batch's last scalar
    body.extend_from_slice(b"col1\tcol2\tcol3\n");
    body.extend_from_slice(&line_flood(20, "\r\n", 6));
    body.extend_from_slice(b"\x1b[3b\n");
    assert_line_batch_equivalence("rep and tabs", 80, 24, b"", no_prep, &body);
}

// ── seeded pseudo-random differential fuzz ─────────────────────────

fn lcg_next(state: &mut u64) -> u64 {
    *state = state
        .wrapping_mul(6364136223846793005)
        .wrapping_add(1442695040888963407);
    *state >> 33
}

fn fuzz_body(seed: u64, cols: u64, tokens: usize) -> Vec<u8> {
    let mut st = seed;
    let mut out = Vec::new();
    for _ in 0..tokens {
        match lcg_next(&mut st) % 24 {
            0 => out.extend_from_slice(b"\x1b[31;42m"),
            1 => out.extend_from_slice(b"\x1b[0m"),
            2 => {
                let r = 1 + lcg_next(&mut st) % 24;
                let c = 1 + lcg_next(&mut st) % cols;
                out.extend_from_slice(format!("\x1b[{r};{c}H").as_bytes());
            }
            3 => {
                let t = 1 + lcg_next(&mut st) % 5;
                let b = 6 + lcg_next(&mut st) % 18;
                out.extend_from_slice(format!("\x1b[{t};{b}r").as_bytes());
            }
            4 => out.extend_from_slice(b"\x1b[r"),
            5 => out.push(b'\r'),
            6 => out.push(b'\t'),
            7 => out.extend_from_slice("広い文字\n".as_bytes()),
            8 => out.extend_from_slice(b"\x1b[2K"),
            9 => out.extend_from_slice(b"\x1b[3b"),
            10 => out.push(0x7f),
            _ => {
                let len = (lcg_next(&mut st) % (cols + 25)) as usize;
                for _ in 0..len {
                    out.push(b' ' + (lcg_next(&mut st) % 95) as u8);
                }
                if lcg_next(&mut st).is_multiple_of(3) {
                    out.push(b'\r');
                }
                out.push(b'\n');
            }
        }
    }
    out
}

#[test]
fn line_batch_matches_per_byte_under_random_token_mixes() {
    for seed in 1..=3u64 {
        assert_line_batch_equivalence(
            &format!("fuzz 80x24 seed {seed}"),
            80,
            24,
            b"",
            no_prep,
            &fuzz_body(seed, 80, 300),
        );
        assert_line_batch_equivalence(
            &format!("fuzz 10x5 seed {seed}"),
            10,
            5,
            b"",
            no_prep,
            &fuzz_body(seed.wrapping_add(100), 10, 250),
        );
    }
}

#[test]
fn apply_ascii_line_batch_engages_and_consumes_whole_spans() {
    // Direct engagement pin: with the cursor on the region bottom the batch
    // consumes the whole span, seals the pass-through rows, and leaves the
    // last lines on the grid — proving the differential tests above compare
    // the batched path and not a permanent per-line fallback.
    let mut t = Terminal::new(GridSize::new(80, 24));
    let mut s = Stream::new();
    s.feed(b"\x1b[24;1H", &mut t);
    let data = b"\nabc\r\nde\n";
    let consumed = t.primary.apply_ascii_line_batch(data, true, false, false);
    assert_eq!(consumed, data.len());
    // Prefix LF scroll + two batched emissions.
    assert_eq!(t.primary.take_scroll_shift(), 3);
    assert_eq!(t.primary.scrollback_len(), 3);
    let text_of = |row: &crate::cell::Row| -> String {
        row.cells.iter().map(|c| c.ch).collect::<String>().trim_end().to_owned()
    };
    assert_eq!(text_of(&t.primary.grid[21]), "abc");
    assert_eq!(text_of(&t.primary.grid[22]), "de");
    assert_eq!(text_of(&t.primary.grid[23]), "");
    assert_eq!((t.primary.cursor.x, t.primary.cursor.y), (2, 23));
    assert!(!t.primary.cursor.pending_wrap);
}
