//! Headless Cell-bandwidth probe for the fire/scroll axes: measures the three
//! grid costs the `Cell` representation dominates — bulk consume (parse +
//! grid store), per-frame `FrameSnapshot` construction, and raw row
//! clear/fill — without a pty, locks, or GPU.
//!
//! Usage:
//!   cargo run --release -p noa-render --example cell_bandwidth -- consume <file> [cols rows repeats]
//!   cargo run --release -p noa-render --example cell_bandwidth -- frames  <file> [cols rows repeats]
//!   cargo run --release -p noa-render --example cell_bandwidth -- store   [cols rows]
//!
//! * `consume` — `throughput`-equivalent: feed the file through one
//!   long-lived `Stream` into a `Terminal`, report MiB/s.
//! * `frames`  — consume + a full worst-case `FrameSnapshot` rebuild every
//!   64 KiB chunk (the flood cadence), reporting the consume and snapshot
//!   shares separately.
//! * `store`   — 50M-cell row clear/fill microbench, ns/cell.

use std::time::Instant;

use noa_core::GridSize;
use noa_grid::Terminal;
use noa_render::FrameSnapshot;

fn main() {
    let mut args = std::env::args().skip(1);
    let mode = args.next().expect("mode: consume | frames | store");
    match mode.as_str() {
        "store" => {
            let cols: u16 = args.next().map_or(319, |s| s.parse().expect("cols"));
            let rows: u16 = args.next().map_or(84, |s| s.parse().expect("rows"));
            store_bench(cols, rows);
        }
        "consume" | "frames" => {
            let path = args.next().expect("input file");
            let cols: u16 = args.next().map_or(319, |s| s.parse().expect("cols"));
            let rows: u16 = args.next().map_or(84, |s| s.parse().expect("rows"));
            let repeats: usize = args.next().map_or(1, |s| s.parse().expect("repeats"));
            feed_bench(&mode, &path, cols, rows, repeats);
        }
        other => panic!("unknown mode {other:?}"),
    }
}

fn feed_bench(mode: &str, path: &str, cols: u16, rows: u16, repeats: usize) {
    let data = std::fs::read(path).expect("read input file");
    let mut terminal = Terminal::new(GridSize::new(cols, rows));
    let mut stream = noa_vt::Stream::new();
    let snapshotting = mode == "frames";

    // The flood cadence: the io thread drains up to 1 MiB per lock hold, but
    // a saturated redraw loop snapshots far more often; 64 KiB approximates
    // one pty reader batch per frame.
    const CHUNK: usize = 64 * 1024;
    let mut rows_buf = Vec::new();
    let mut snapshot_time = std::time::Duration::ZERO;
    let mut snapshots = 0u64;
    let start = Instant::now();
    for _ in 0..repeats {
        for chunk in data.chunks(CHUNK) {
            stream.feed(chunk, &mut terminal);
            let _ = terminal.take_pending_writes();
            if snapshotting {
                let t = Instant::now();
                let snap = FrameSnapshot::from_terminal_recycled(
                    &mut terminal,
                    std::mem::take(&mut rows_buf),
                );
                snapshot_time += t.elapsed();
                snapshots += 1;
                rows_buf = snap.rows;
            }
        }
    }
    let elapsed = start.elapsed();

    let mb = (data.len() * repeats) as f64 / (1024.0 * 1024.0);
    let consume = elapsed - snapshot_time;
    print!(
        "{path}: {mb:.1} MiB in {elapsed:.3?} = {:.1} MiB/s total ({cols}x{rows})",
        mb / elapsed.as_secs_f64()
    );
    if snapshotting {
        print!(
            "; consume {:.1} MiB/s; snapshot {:.1} us/frame x{snapshots}",
            mb / consume.as_secs_f64(),
            snapshot_time.as_secs_f64() * 1e6 / snapshots as f64
        );
    }
    println!();
}

fn store_bench(cols: u16, rows: u16) {
    let mut terminal = Terminal::new(GridSize::new(cols, rows));
    let cells_per_pass = cols as usize * rows as usize;
    let passes = 50_000_000usize.div_ceil(cells_per_pass);

    // Alternate two blank templates so the clear can never be skipped.
    let mut stream = noa_vt::Stream::new();
    let start = Instant::now();
    for i in 0..passes {
        // ED 2 with alternating background pens: full-grid clear each pass.
        let seq: &[u8] = if i % 2 == 0 {
            b"\x1b[44m\x1b[2J"
        } else {
            b"\x1b[0m\x1b[2J"
        };
        stream.feed(seq, &mut terminal);
    }
    let elapsed = start.elapsed();
    let cells = passes * cells_per_pass;
    println!(
        "store: {cells} cells in {elapsed:.3?} = {:.3} ns/cell ({cols}x{rows})",
        elapsed.as_secs_f64() * 1e9 / cells as f64
    );
}
