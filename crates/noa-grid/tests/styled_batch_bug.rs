use noa_core::{Color, GridSize};
use noa_grid::Terminal;
use noa_vt::Stream;

fn feed_whole(cols: u16, rows: u16, s: &[u8]) -> Terminal {
    let mut t = Terminal::new(GridSize::new(cols, rows));
    let mut st = Stream::new();
    st.feed(s, &mut t);
    t
}
fn feed_byte(cols: u16, rows: u16, s: &[u8]) -> Terminal {
    let mut t = Terminal::new(GridSize::new(cols, rows));
    let mut st = Stream::new();
    for b in s {
        st.feed(std::slice::from_ref(b), &mut t);
    }
    t
}

/// The (fg, bg) of the first cell of the row whose text (trimmed) starts with
/// `needle`.
fn style_of_row(t: &Terminal, needle: &str) -> (Color, Color) {
    let s = t.active();
    for y in 0..s.total_rows() {
        let r = s.absolute_row(y).unwrap();
        let text: String = r.cells.iter().map(|c| c.ch).collect();
        if let Some(pos) = text.find(needle) {
            let c = r.cells[pos];
            return (c.fg, c.bg);
        }
    }
    panic!("row starting with {needle:?} not found");
}

#[test]
fn plain_line_after_styled_line_in_batch_inherits_stale_template() {
    // 20x4. Print "X" on the bottom row so the batch's prefix LF has content,
    // then a styled line "AAA" whose `\x1b[0m` tail resets the pen, then a
    // plain line "BBB". Ghostty prints "BBB" with the (reset) default pen.
    let stream = b"\x1b[4;1HX\n\x1b[41mAAA\x1b[0m\nBBB\n";
    let (cols, rows) = (20u16, 4u16);

    let whole = feed_whole(cols, rows, stream);
    let byte = feed_byte(cols, rows, stream);

    let (whole_fg, whole_bg) = style_of_row(&whole, "BBB");
    let (byte_fg, byte_bg) = style_of_row(&byte, "BBB");

    eprintln!("batched  BBB = fg {whole_fg:?} bg {whole_bg:?}");
    eprintln!("per-byte BBB = fg {byte_fg:?} bg {byte_bg:?}");

    // Ground truth (the canonical per-byte path): BBB is unstyled.
    assert_eq!(
        byte_bg,
        Color::Default,
        "per-byte: BBB bg should be default"
    );

    // The batch must match. It does not: it paints BBB with AAA's red bg.
    assert_eq!(
        (whole_fg, whole_bg),
        (byte_fg, byte_bg),
        "BATCH FIDELITY BUG: a plain line after a styled line inherits the stale \
         lead template",
    );
}
