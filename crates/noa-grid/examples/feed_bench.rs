//! Headless bulk-output throughput bench: feed a file through
//! `Stream` + `Terminal` exactly like the io thread does, no pty, no GPU.
//!
//! Usage: `cargo run --release -p noa-grid --example feed_bench -- <file> [cols] [rows] [reps]`

use noa_core::geometry::GridSize;
use noa_grid::Terminal;
use noa_vt::Stream;
use std::time::Instant;

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args.next().expect("usage: feed_bench <file> [cols] [rows] [reps]");
    let cols: u16 = args.next().map_or(120, |s| s.parse().unwrap());
    let rows: u16 = args.next().map_or(40, |s| s.parse().unwrap());
    let reps: usize = args.next().map_or(1, |s| s.parse().unwrap());

    let data = std::fs::read(&path).expect("read input file");
    let mib = data.len() as f64 / (1024.0 * 1024.0);

    for rep in 0..reps {
        let mut term = Terminal::new(GridSize::new(cols, rows));
        let mut stream = Stream::new();
        let start = Instant::now();
        // Feed in pty-read-sized chunks to mirror the io thread's shape.
        for chunk in data.chunks(64 * 1024) {
            stream.feed(chunk, &mut term);
            // Drain reports like the io thread does (usually empty here).
            let _ = term.take_pending_writes();
        }
        let elapsed = start.elapsed();
        println!(
            "rep {rep}: {mib:.1} MiB in {elapsed:.3?} = {:.1} MiB/s",
            mib / elapsed.as_secs_f64()
        );
    }
}
