//! Headless parse+grid throughput probe: feeds a file through one long-lived
//! `noa_vt::Stream` into a `Terminal` — the exact hot path the io thread runs
//! under the terminal lock — with no pty, locks, or rendering, isolating how
//! much of `bench/run_benchmark.sh`'s wall time is parse/grid cost.
//!
//! Usage:
//!   cargo run --release -p noa-grid --example throughput -- bench/150MB_ascii.txt [cols rows]

use std::time::Instant;

use noa_core::GridSize;
use noa_grid::Terminal;

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args.next().expect("usage: throughput <file> [cols rows repeats]");
    let cols: u16 = args.next().map_or(100, |s| s.parse().expect("cols"));
    let rows: u16 = args.next().map_or(30, |s| s.parse().expect("rows"));
    let repeats: usize = args.next().map_or(1, |s| s.parse().expect("repeats"));

    let data = std::fs::read(&path).expect("read input file");
    let mut terminal = Terminal::new(GridSize::new(cols, rows));
    let mut stream = noa_vt::Stream::new();

    // Feed in 1 MiB chunks to mirror PTY_DATA_DRAIN_BYTE_LIMIT batching.
    const CHUNK: usize = 1024 * 1024;
    let start = Instant::now();
    for _ in 0..repeats {
        for chunk in data.chunks(CHUNK) {
            stream.feed(chunk, &mut terminal);
            // The io thread drains these once per batch; keep the probe honest.
            let _ = terminal.take_pending_writes();
        }
    }
    let elapsed = start.elapsed();

    let mb = (data.len() * repeats) as f64 / (1024.0 * 1024.0);
    println!(
        "{path}: {mb:.1} MiB in {elapsed:.3?} = {:.0} MiB/s ({cols}x{rows})",
        mb / elapsed.as_secs_f64()
    );
}
