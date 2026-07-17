// ── erase-family `Cell::set_from` — no residual state ──
//
// `erase_line`, `erase_display`, `insert_blank_chars`, `delete_chars`, and
// `erase_chars` all fill their target cells via `Cell::set_from` (a plain
// POD copy). `set_from` must still
// touch every field a struct-literal replace would have, so a cell that
// carried combining marks, a hyperlink, non-default colors, or wide flags
// leaves no trace after any of these five erase paths — including the
// `sanitize_wide_row` self-heal when an erase boundary splits a wide
// lead/spacer pair, which this refactor must not have disturbed.

/// A decorated wide cluster: bold + underline + palette fg + rgb underline
/// color + an active hyperlink, wrapping a wide CJK char with a combining
/// mark attached to its lead cell. Occupies two columns wherever printed.
const DECORATED_WIDE_CLUSTER: &str =
    "\x1b[1;4;38;5;9;58;2;10;20;30m\x1b]8;;https://example.test\x07日\u{0301}\x1b]8;;\x07\x1b[0m";

fn assert_full_blank(c: &crate::cell::Cell, bg: Color) {
    assert_eq!(c.ch, ' ', "erase left a stray scalar: {:?}", c.ch);
    assert!(
        c.combining().is_empty(),
        "stale combining marks survived erase: {:?}",
        c.combining()
    );
    assert_eq!(c.fg, Color::Default, "stale fg survived erase");
    assert_eq!(c.bg, bg, "erased cell bg does not reflect the current pen (bce)");
    assert_eq!(c.underline_color, None, "stale underline_color survived erase");
    assert_eq!(c.hyperlink, None, "stale hyperlink survived erase");
    assert!(c.attrs.is_empty(), "stale attrs survived erase: {:?}", c.attrs);
}

/// Sanity check that a `DECORATED_WIDE_CLUSTER` printed at `(x, y)` actually
/// carries every field `assert_full_blank` later checks got cleared — run
/// this on every test's pre-erase state so a broken `set_from` can't hide
/// behind an already-empty field (e.g. a hyperlink `set_from` never wrote in
/// the first place erases "clean" by accident).
fn assert_decorated(t: &Terminal, x: usize, y: usize) {
    let lead = cell(t, x, y);
    assert_eq!(lead.ch, '日');
    assert!(!lead.combining().is_empty(), "setup: combining mark missing");
    assert!(lead.hyperlink.is_some(), "setup: hyperlink missing");
    assert!(
        lead.attrs.contains(CellAttrs::BOLD | CellAttrs::UNDERLINE),
        "setup: attrs missing"
    );
    assert_eq!(lead.fg, Color::Palette(9), "setup: fg missing");
    assert!(lead.underline_color.is_some(), "setup: underline_color missing");
    assert!(
        cell(t, x + 1, y).attrs.contains(CellAttrs::WIDE_SPACER),
        "setup: wide spacer missing"
    );
}

#[test]
fn erase_line_right_heals_split_wide_lead_and_clears_full_state() {
    let prefix = format!("{DECORATED_WIDE_CLUSTER}XYZ");
    assert_decorated(&run_size(6, 1, prefix.as_bytes()), 0, 0);

    // Cursor lands on the spacer (col 1): the main loop only erases the
    // spacer directly, leaving an orphaned WIDE lead at col 0 that
    // `sanitize_wide_row` must heal to full blank too.
    let bytes = format!("{prefix}\x1b[41m\x1b[1;2H\x1b[0K");
    let t = run_size(6, 1, bytes.as_bytes());
    for x in 0..6 {
        assert_full_blank(&cell(&t, x, 0), Color::Palette(1));
    }
}

#[test]
fn erase_line_left_heals_orphaned_wide_spacer_and_clears_full_state() {
    let prefix = format!("{DECORATED_WIDE_CLUSTER}XYZ");
    assert_decorated(&run_size(6, 1, prefix.as_bytes()), 0, 0);

    // Cursor lands on the lead (col 0): the main loop only erases the lead
    // directly ([..=0]), leaving an orphaned WIDE_SPACER at col 1 that
    // `sanitize_wide_row` must heal to full blank too.
    let bytes = format!("{prefix}\x1b[41m\x1b[1;1H\x1b[1K");
    let t = run_size(6, 1, bytes.as_bytes());
    assert_full_blank(&cell(&t, 0, 0), Color::Palette(1));
    assert_full_blank(&cell(&t, 1, 0), Color::Palette(1));
    // Untouched control: EL Left never reaches past the cursor column.
    assert_eq!(cell(&t, 2, 0).ch, 'X');
    assert_eq!(cell(&t, 3, 0).ch, 'Y');
    assert_eq!(cell(&t, 4, 0).ch, 'Z');
}

#[test]
fn erase_line_complete_clears_full_row_state() {
    let prefix = format!("{DECORATED_WIDE_CLUSTER}XYZ");
    assert_decorated(&run_size(6, 1, prefix.as_bytes()), 0, 0);

    let bytes = format!("{prefix}\x1b[41m\x1b[2K");
    let t = run_size(6, 1, bytes.as_bytes());
    for x in 0..6 {
        assert_full_blank(&cell(&t, x, 0), Color::Palette(1));
    }
}

#[test]
fn erase_display_below_clears_full_state_across_rows() {
    // Row 1 (cursor row): decorated wide cluster at cols 0-1, plain "XYZ"
    // after. Row 2 (below cursor): plain content proving `Row::clear` still
    // fires on the boundary this diff touches.
    let prefix = format!("\x1b[2;1H{DECORATED_WIDE_CLUSTER}XYZ\x1b[3;1HPQR");
    assert_decorated(&run_size(6, 3, prefix.as_bytes()), 0, 1);

    let bytes = format!("{prefix}\x1b[41m\x1b[2;2H\x1b[J");
    let t = run_size(6, 3, bytes.as_bytes());
    // Row 0 (above cursor) is untouched by ED Below.
    assert_eq!(cell(&t, 0, 0).ch, ' ');
    for x in 0..6 {
        assert_full_blank(&cell(&t, x, 1), Color::Palette(1));
        assert_full_blank(&cell(&t, x, 2), Color::Palette(1));
    }
}

#[test]
fn erase_display_above_clears_full_state_across_rows() {
    // Row 0 (above cursor): plain content proving `Row::clear` still fires.
    // Row 1 (cursor row): decorated wide cluster at cols 0-1; cursor sits on
    // the lead so the orphaned-spacer heal is exercised here too.
    let prefix = format!("PQR\x1b[2;1H{DECORATED_WIDE_CLUSTER}XYZ");
    assert_decorated(&run_size(6, 3, prefix.as_bytes()), 0, 1);

    let bytes = format!("{prefix}\x1b[41m\x1b[2;1H\x1b[1J");
    let t = run_size(6, 3, bytes.as_bytes());
    for x in 0..6 {
        assert_full_blank(&cell(&t, x, 0), Color::Palette(1));
    }
    assert_full_blank(&cell(&t, 0, 1), Color::Palette(1));
    assert_full_blank(&cell(&t, 1, 1), Color::Palette(1));
    // Untouched control: ED Above never reaches past the cursor column on
    // the cursor's own row.
    assert_eq!(cell(&t, 2, 1).ch, 'X');
}

#[test]
fn insert_blank_chars_clears_content_rotated_into_target_range() {
    // Decorated wide cluster sits at the tail (cols 4-5); inserting at col 2
    // rotates it into the freed [2..4) range, which must end up full blank,
    // not merely re-stamped with a blank `ch`.
    let prefix = format!("ABCD{DECORATED_WIDE_CLUSTER}");
    assert_decorated(&run_size(6, 1, prefix.as_bytes()), 4, 0);

    let bytes = format!("{prefix}\x1b[41m\x1b[1;3H\x1b[2@");
    let t = run_size(6, 1, bytes.as_bytes());
    assert_full_blank(&cell(&t, 2, 0), Color::Palette(1));
    assert_full_blank(&cell(&t, 3, 0), Color::Palette(1));
    // Control: untouched prefix and the rotated-right plain tail.
    assert_eq!(cell(&t, 0, 0).ch, 'A');
    assert_eq!(cell(&t, 1, 0).ch, 'B');
    assert_eq!(cell(&t, 4, 0).ch, 'C');
    assert_eq!(cell(&t, 5, 0).ch, 'D');
}

#[test]
fn delete_chars_clears_content_rotated_into_tail_range() {
    // Decorated wide cluster sits at the cursor column (2-3); deleting there
    // rotates it into the freed tail ([4..6)), which must end up full blank.
    let prefix = format!("AB{DECORATED_WIDE_CLUSTER}EF");
    assert_decorated(&run_size(6, 1, prefix.as_bytes()), 2, 0);

    let bytes = format!("{prefix}\x1b[41m\x1b[1;3H\x1b[2P");
    let t = run_size(6, 1, bytes.as_bytes());
    assert_full_blank(&cell(&t, 4, 0), Color::Palette(1));
    assert_full_blank(&cell(&t, 5, 0), Color::Palette(1));
    // Control: untouched prefix and the rotated-left plain tail.
    assert_eq!(cell(&t, 0, 0).ch, 'A');
    assert_eq!(cell(&t, 1, 0).ch, 'B');
    assert_eq!(cell(&t, 2, 0).ch, 'E');
    assert_eq!(cell(&t, 3, 0).ch, 'F');
}

#[test]
fn erase_chars_clears_full_state_not_just_ch() {
    // Complements `erase_chars_sanitizes_split_wide_cell` (text_resize.rs),
    // which only checks `ch`/wide attrs — this checks every field.
    let prefix = format!("A{DECORATED_WIDE_CLUSTER}");
    assert_decorated(&run_size(6, 1, prefix.as_bytes()), 1, 0);

    let bytes = format!("{prefix}\x1b[41m\x1b[1;2H\x1b[2X");
    let t = run_size(6, 1, bytes.as_bytes());
    assert_full_blank(&cell(&t, 1, 0), Color::Palette(1));
    assert_full_blank(&cell(&t, 2, 0), Color::Palette(1));
    assert_eq!(cell(&t, 0, 0).ch, 'A'); // untouched control
}
