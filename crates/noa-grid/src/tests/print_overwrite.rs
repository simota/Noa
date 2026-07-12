// ── `Screen::print` overwrite via `Cell::set_from` — no residual state ──
//
// `Screen::print`'s narrow, wide-lead/spacer, and `promote_cluster_to_wide`
// spacer writes now go through `Cell::set_from` (in-place, capacity-reusing)
// instead of a struct-literal replace. `set_from` must still touch every
// field a struct-literal replace would have, so a destination cell that
// already carried combining marks, a hyperlink, or wide flags from earlier
// content leaves no trace in the freshly printed cell.

#[test]
fn print_narrow_overwrite_clears_prior_combining_and_hyperlink() {
    use noa_vt::Handler as _;

    let mut t = Terminal::new(GridSize::new(80, 24));
    let mut s = Stream::new();
    // Cell (0,0) starts as an accented 'e' under an active hyperlink.
    s.feed(b"\x1b]8;;https://example.test\x07", &mut t);
    t.print('e');
    t.print('\u{0301}'); // combining acute attaches to 'e'
    s.feed(b"\x1b]8;;\x07\x1b[1;1H", &mut t);
    // A plain scalar reprints the same cell with no hyperlink active.
    t.print('z');

    let c = cell(&t, 0, 0);
    assert_eq!(c.ch, 'z');
    assert!(
        c.combining.is_empty(),
        "stale combining marks leaked into the reprinted cell: {:?}",
        c.combining
    );
    assert_eq!(
        c.hyperlink, None,
        "stale hyperlink leaked into the reprinted cell"
    );
    assert!(!c.attrs.intersects(CellAttrs::WIDE | CellAttrs::WIDE_SPACER));
}

#[test]
fn print_wide_overwrite_clears_prior_lead_combining_and_hyperlink() {
    use noa_vt::Handler as _;

    let mut t = Terminal::new(GridSize::new(80, 24));
    let mut s = Stream::new();
    // A wide CJK char under an active hyperlink, with a combining mark
    // attached to its lead cell (attach targets the lead, not the spacer —
    // see `Screen::resolve_cluster_target`).
    s.feed(b"\x1b]8;;https://example.test\x07", &mut t);
    t.print('日');
    t.print('\u{0301}');
    s.feed(b"\x1b]8;;\x07\x1b[1;1H", &mut t);
    // A different wide char reprints the same lead/spacer pair with no
    // hyperlink active.
    t.print('本');

    let lead = cell(&t, 0, 0);
    let spacer = cell(&t, 1, 0);
    assert_eq!(lead.ch, '本');
    assert!(
        lead.combining.is_empty(),
        "stale combining marks leaked into the new wide lead: {:?}",
        lead.combining
    );
    assert_eq!(
        lead.hyperlink, None,
        "stale hyperlink leaked into the new wide lead"
    );
    assert!(lead.attrs.contains(CellAttrs::WIDE));
    assert_eq!(spacer.ch, ' ');
    assert!(spacer.attrs.contains(CellAttrs::WIDE_SPACER));
    assert_eq!(
        spacer.hyperlink, None,
        "stale hyperlink leaked into the new wide spacer"
    );
}

#[test]
fn cluster_promotion_to_wide_clears_prior_spacer_state() {
    use noa_vt::Handler as _;

    let mut t = Terminal::new(GridSize::new(80, 24));
    let mut s = Stream::new();
    s.feed(b"\x1b[?2027h", &mut t); // mode 2027: grapheme clustering on
    // Cell (1,0) — the position `promote_cluster_to_wide` is about to claim
    // as a spacer — starts with an accented 'e' under an active hyperlink.
    s.feed(b"\x1b[1;2H\x1b]8;;https://example.test\x07", &mut t);
    t.print('e');
    t.print('\u{0301}');
    s.feed(b"\x1b]8;;\x07\x1b[1;1H", &mut t);
    // A narrow smiley base, then VS16 (emoji presentation) widens the
    // cluster and claims cell (1,0) as its `WIDE_SPACER`.
    t.print('\u{263A}');
    t.print('\u{FE0F}');

    let lead = cell(&t, 0, 0);
    let spacer = cell(&t, 1, 0);
    assert!(lead.attrs.contains(CellAttrs::WIDE));
    assert_eq!(spacer.ch, ' ', "promoted spacer must be blank, not leftover 'e'");
    assert!(
        spacer.combining.is_empty(),
        "stale combining marks leaked into the promoted spacer: {:?}",
        spacer.combining
    );
    assert_eq!(
        spacer.hyperlink, None,
        "stale hyperlink leaked into the promoted spacer"
    );
    assert!(spacer.attrs.contains(CellAttrs::WIDE_SPACER));
}
