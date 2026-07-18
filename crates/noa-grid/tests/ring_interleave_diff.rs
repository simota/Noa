//! Adversarial: interleave full-height line floods (which advance the
//! `RingGrid` base via the batch/scroll fast paths) with partial-region and
//! whole-screen edits (IL/DL/ED/2J/DECSTBM/erase/resize), then assert the
//! batched terminal (whole feed) is byte-identical to a per-byte reference
//! (which never engages the line-batch). A stale `base` — a slice path that
//! forgot to `canonicalize()`, or a mis-accounted `advance_base` — would
//! surface as a wrong row here.

use noa_core::GridSize;
use noa_grid::Terminal;
use noa_vt::Stream;

fn lcg(state: &mut u64) -> u64 {
    *state = state
        .wrapping_mul(6364136223846793005)
        .wrapping_add(1442695040888963407);
    *state >> 33
}

fn gen_stream(seed: u64, cols: u64, rows: u64, tokens: usize) -> Vec<u8> {
    let mut st = seed;
    let mut out = Vec::new();
    for _ in 0..tokens {
        match lcg(&mut st) % 20 {
            // Batchable plain LF flood of several lines (advances base).
            0..=2 => {
                let n = 1 + lcg(&mut st) % 6;
                for _ in 0..n {
                    let len = (lcg(&mut st) % (cols + 10)) as usize;
                    for _ in 0..len {
                        out.push(b'a' + (lcg(&mut st) % 26) as u8);
                    }
                    if lcg(&mut st).is_multiple_of(2) {
                        out.push(b'\r');
                    }
                    out.push(b'\n');
                }
            }
            // Batchable styled flood (per-line SGR template + reset).
            3 => {
                let n = 1 + lcg(&mut st) % 4;
                for _ in 0..n {
                    out.extend_from_slice(b"\x1b[38;5;208;48;5;22m");
                    let len = (lcg(&mut st) % cols) as usize;
                    for _ in 0..len {
                        out.push(b'0' + (lcg(&mut st) % 10) as u8);
                    }
                    out.extend_from_slice(b"\x1b[0m\n");
                }
            }
            // Absolute cursor position.
            4 => {
                let r = 1 + lcg(&mut st) % rows;
                let c = 1 + lcg(&mut st) % cols;
                out.extend_from_slice(format!("\x1b[{r};{c}H").as_bytes());
            }
            // Set DECSTBM region.
            5 => {
                let t = 1 + lcg(&mut st) % (rows / 2);
                let b = (rows / 2) + 1 + lcg(&mut st) % (rows / 2);
                out.extend_from_slice(format!("\x1b[{t};{b}r").as_bytes());
            }
            6 => out.extend_from_slice(b"\x1b[r"), // reset region
            // Insert lines (IL).
            7 => {
                let n = 1 + lcg(&mut st) % 4;
                out.extend_from_slice(format!("\x1b[{n}L").as_bytes());
            }
            // Delete lines (DL).
            8 => {
                let n = 1 + lcg(&mut st) % 4;
                out.extend_from_slice(format!("\x1b[{n}M").as_bytes());
            }
            // Erase display variants (ED): 0/1/2/3.
            9 => {
                let k = lcg(&mut st) % 4;
                out.extend_from_slice(format!("\x1b[{k}J").as_bytes());
            }
            // Erase line variants (EL).
            10 => {
                let k = lcg(&mut st) % 3;
                out.extend_from_slice(format!("\x1b[{k}K").as_bytes());
            }
            // Scroll up / down (SU/SD).
            11 => {
                let n = 1 + lcg(&mut st) % 5;
                let d = if lcg(&mut st).is_multiple_of(2) {
                    b'S'
                } else {
                    b'T'
                };
                out.extend_from_slice(format!("\x1b[{n}{}", d as char).as_bytes());
            }
            // Reverse index (RI) — can scroll the region down.
            12 => out.extend_from_slice(b"\x1bM"),
            // Wide chars then newline.
            13 => out.extend_from_slice("日本語ひらがな\n".as_bytes()),
            // Bare CR / TAB / BS.
            14 => out.push(b'\r'),
            15 => out.push(0x08),
            // A short partial line (no terminator) leaving cursor mid-row.
            16 => {
                let len = 1 + (lcg(&mut st) % cols) as usize;
                for _ in 0..len {
                    out.push(b'A' + (lcg(&mut st) % 26) as u8);
                }
            }
            // Home + single LF (advances base by 1 via scroll fast path).
            17 => out.extend_from_slice(b"\x1b[H\n"),
            // DECSET origin mode toggle + a positioned print.
            18 => {
                out.extend_from_slice(b"\x1b[?6h");
                out.extend_from_slice(b"xy\n");
                out.extend_from_slice(b"\x1b[?6l");
            }
            // Sticky BCE background.
            _ => out.extend_from_slice(b"\x1b[48;5;53m"),
        }
    }
    out
}

fn assert_terminals_match(label: &str, whole: &Terminal, byte: &Terminal) {
    let (w, b) = (whole.active(), byte.active());
    assert_eq!(w.total_rows(), b.total_rows(), "{label}: total_rows");
    assert_eq!(
        w.scrollback_len(),
        b.scrollback_len(),
        "{label}: scrollback_len"
    );
    assert_eq!(
        w.viewport_offset(),
        b.viewport_offset(),
        "{label}: viewport_offset"
    );
    assert_eq!(w.rows_evicted(), b.rows_evicted(), "{label}: rows_evicted");
    assert_eq!(
        (w.cursor.x, w.cursor.y, w.cursor.pending_wrap),
        (b.cursor.x, b.cursor.y, b.cursor.pending_wrap),
        "{label}: cursor",
    );
    let total = w.total_rows();
    for y in 0..total {
        let wr = w.absolute_row(y).expect("row");
        let br = b.absolute_row(y).expect("row");
        assert_eq!(wr.wrapped, br.wrapped, "{label}: row {y} wrapped");
        for x in 0..wr.cells.len().max(br.cells.len()) {
            let wc = wr.cells.get(x);
            let bc = br.cells.get(x);
            assert_eq!(
                wc.map(|c| (c.ch, c.fg, c.bg, c.attrs)),
                bc.map(|c| (c.ch, c.fg, c.bg, c.attrs)),
                "{label}: row {y} col {x}",
            );
        }
    }
}

#[test]
fn ring_base_survives_interleaved_region_edits() {
    for &(cols, rows) in &[(80u16, 24u16), (12, 6), (20, 8), (10, 4)] {
        for seed in 1..=40u64 {
            let stream = gen_stream(seed, cols as u64, rows as u64, 200);

            let mut whole = Terminal::new(GridSize::new(cols, rows));
            let mut ws = Stream::new();
            ws.feed(&stream, &mut whole);
            whole.primary.trim_memory();

            let mut byte = Terminal::new(GridSize::new(cols, rows));
            let mut bs = Stream::new();
            for b in &stream {
                bs.feed(std::slice::from_ref(b), &mut byte);
            }
            byte.primary.trim_memory();

            assert_terminals_match(&format!("{cols}x{rows} seed {seed}"), &whole, &byte);

            // Also compare mid-size chunkings — chunk boundaries can split an
            // escape or land a CR at end-of-chunk with its LF next chunk.
            for chunk in [3usize, 7, 29, 128] {
                let mut chunked = Terminal::new(GridSize::new(cols, rows));
                let mut cs = Stream::new();
                for c in stream.chunks(chunk) {
                    cs.feed(c, &mut chunked);
                }
                chunked.primary.trim_memory();
                assert_terminals_match(
                    &format!("{cols}x{rows} seed {seed} chunk {chunk}"),
                    &chunked,
                    &byte,
                );
            }
        }
    }
}
