// Row occupancy-watermark edge cases (wish #3 cycle 3).
//
// The watermark (`Row::occ`) lets the scrollback seal path skip loading a
// row's blank tail. Its correctness contract: cells at `cells[occupied()..]`
// are `Cell::default()`, and — critically — *styled* blanks (BCE: EL/ED
// under an SGR background) are **not** default, must stay under the
// watermark, and must survive the pack/materialize roundtrip byte-for-byte.
// These tests drive real VT sequences through the print/erase paths (so the
// rows carry genuine `occ < cols` watermarks) and pin the packed results
// against both expected content and the immediate-packing reference.

/// BCE tail: `EL` under `SGR 41` writes red blanks to the row end. Those are
/// non-default and the watermark must cover them — after the rows seal into
/// scrollback the red tail must materialize intact, while default tails
/// (kind 0) and a wide char parked at the very watermark boundary (kind 2)
/// roundtrip too.
#[test]
fn bce_blank_tail_survives_deferred_seal_and_pack() {
    let mut bytes = Vec::new();
    for i in 0..200 {
        match i % 3 {
            0 => bytes.extend_from_slice(b"hello\r\n"),
            1 => bytes.extend_from_slice(b"\x1b[41mAB\x1b[K\x1b[0m\r\n"),
            // Wide char occupying the last two cells (columns 39-40 of 40):
            // the watermark sits exactly at the row width.
            _ => bytes.extend_from_slice("\x1b[39G漢\r\n".as_bytes()),
        }
    }
    let mut t = run_size(40, 5, &bytes);
    // Settle every deferred row through the watermarked pack path.
    t.primary.trim_memory();
    let n = t.scrollback_len();
    assert_eq!(n, 196, "200 lines minus the 4 still-live grid rows");

    for y in 0..n {
        let row = t.active().absolute_row(y).expect("scrollback row");
        assert_eq!(row.cells.len(), 40);
        match y % 3 {
            0 => {
                assert_eq!(
                    row.cells[..5].iter().map(|c| c.ch).collect::<String>(),
                    "hello",
                    "row {y}"
                );
                assert!(
                    row.cells[5..].iter().all(|c| *c == crate::Cell::default()),
                    "row {y}: default tail must stay default"
                );
            }
            1 => {
                assert_eq!(row.cells[0].ch, 'A', "row {y}");
                assert_eq!(row.cells[1].ch, 'B', "row {y}");
                assert!(
                    row.cells
                        .iter()
                        .all(|c| c.bg == Color::Palette(1)),
                    "row {y}: EL under SGR 41 paints the whole row red (BCE)"
                );
                assert!(
                    row.cells[2..].iter().all(|c| c.ch == ' '),
                    "row {y}: erased tail is blank"
                );
            }
            _ => {
                assert!(
                    row.cells[..38].iter().all(|c| *c == crate::Cell::default()),
                    "row {y}: leading blanks stay default"
                );
                assert_eq!(row.cells[38].ch, '漢', "row {y}");
                assert!(row.cells[38].attrs.contains(CellAttrs::WIDE), "row {y}");
                assert!(
                    row.cells[39].attrs.contains(CellAttrs::WIDE_SPACER),
                    "row {y}: spacer at the last column survives the trim"
                );
            }
        }
    }
}

/// Rows carrying real watermarks (`occ < cols`, produced by the print/erase
/// paths) must pack byte-identically through the deferred pipeline and the
/// immediate reference — the watermark-fed trim may only skip cells the full
/// scan would have trimmed anyway.
#[test]
fn watermarked_rows_pack_identically_deferred_and_immediate() {
    let mut bytes = Vec::new();
    // Six distinct shapes: plain short line / BCE red tail / wide char at
    // the right edge / full-width line (watermark == cols, nothing trims) /
    // ED-below styled tail / an untouched blank row (watermark 0).
    bytes.extend_from_slice(b"plain tail\r\n");
    bytes.extend_from_slice(b"\x1b[44mXY\x1b[K\x1b[0m\r\n");
    bytes.extend_from_slice("\x1b[39G漢\r\n".as_bytes());
    bytes.extend_from_slice(&[b'x'; 40]);
    bytes.extend_from_slice(b"\r\n\x1b[42m\x1b[J\x1b[0mZ\r\n");
    let t = run_size(40, 6, &bytes);

    let rows: Vec<crate::cell::Row> = t.primary.grid.clone();
    assert!(
        rows.iter().any(|r| r.occupied() < r.cells.len()),
        "precondition: at least one row exercises the tail-skip fast path"
    );

    let mut deferred = crate::scrollback::PagedScrollback::new(usize::MAX);
    let mut immediate = crate::scrollback::PagedScrollback::new(usize::MAX);
    for row in &rows {
        deferred.push_row_deferred(row.clone());
        immediate.push_row(row);
    }
    deferred.trim_memory();
    for y in 0..rows.len() {
        let d = deferred.row(y).expect("deferred row");
        let i = immediate.row(y).expect("immediate row");
        assert_eq!(d.cells, i.cells, "row {y} content diverged");
        assert_eq!(d.wrapped, i.wrapped, "row {y} wrapped flag diverged");
    }
}

/// DECAWM wrap at the last column: the wrapped row is full width (watermark
/// == cols, no trim) and its continuation carries the wrap flag — both must
/// survive sealing.
#[test]
fn decawm_wrapped_full_width_rows_roundtrip_through_scrollback() {
    let mut bytes = Vec::new();
    // 25 chars in a 10-col grid: rows of 10/10/5, first two soft-wrapped.
    bytes.extend_from_slice(b"abcdefghijklmnopqrstuvwxy\r\n");
    // Push everything into scrollback.
    for _ in 0..6 {
        bytes.extend_from_slice(b"\r\n");
    }
    let mut t = run_size(10, 3, &bytes);
    t.primary.trim_memory();
    assert!(t.scrollback_len() >= 3);

    let r0 = t.active().absolute_row(0).expect("first wrapped row");
    assert_eq!(
        r0.cells.iter().map(|c| c.ch).collect::<String>(),
        "abcdefghij"
    );
    assert!(r0.wrapped, "full row soft-wrapped into the next");
    let r1 = t.active().absolute_row(1).expect("second wrapped row");
    assert_eq!(
        r1.cells.iter().map(|c| c.ch).collect::<String>(),
        "klmnopqrst"
    );
    assert!(r1.wrapped);
    let r2 = t.active().absolute_row(2).expect("wrap tail row");
    assert_eq!(
        r2.cells[..5].iter().map(|c| c.ch).collect::<String>(),
        "uvwxy"
    );
    assert!(!r2.wrapped);
    assert!(r2.cells[5..].iter().all(|c| *c == crate::Cell::default()));
}
