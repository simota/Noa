//! Headless throughput benchmark: how fast the from-scratch VT parser + grid
//! model consume program output and turn it into screen state (the hot path
//! behind on-screen updates, minus the GPU present).
//!
//!   cargo run -p noa-grid --example bench_throughput --release
//!
//! Generates a representative workload (plain text, SGR-colored runs, cursor
//! moves, scrolling) and reports MiB/s and lines/s over several iterations.

use std::time::Instant;

use noa_core::GridSize;
use noa_grid::Terminal;
use noa_vt::Stream;

/// Build a deterministic, representative chunk of terminal output.
/// Mixes plain ASCII (logs/source), SGR color runs (build output, `ls`),
/// and cursor control — the shapes a real terminal actually swallows.
fn workload(target_bytes: usize) -> Vec<u8> {
    let mut buf = Vec::with_capacity(target_bytes + 4096);
    let mut seed: u32 = 0x1234_5678;
    let mut next = || {
        // xorshift — deterministic, no rng dependency
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        seed
    };
    let words = [
        "fn", "let", "match", "self", "Terminal", "render", "grid", "cursor", "0x1f4a9", "commit",
        "parser", "stream", "handler", "cell", "atlas",
    ];
    while buf.len() < target_bytes {
        match next() % 10 {
            // colored build-output-style line
            0..=3 => {
                let color = 31 + (next() % 7);
                buf.extend_from_slice(format!("\x1b[1;{color}m").as_bytes());
                let n = 4 + (next() % 8) as usize;
                for _ in 0..n {
                    buf.extend_from_slice(words[(next() as usize) % words.len()].as_bytes());
                    buf.push(b' ');
                }
                buf.extend_from_slice(b"\x1b[0m\r\n");
            }
            // plain log/source line
            4..=7 => {
                let n = 6 + (next() % 10) as usize;
                for _ in 0..n {
                    buf.extend_from_slice(words[(next() as usize) % words.len()].as_bytes());
                    buf.push(b' ');
                }
                buf.extend_from_slice(b"\r\n");
            }
            // cursor addressing + partial clear (TUI-style repaint)
            _ => {
                let row = 1 + (next() % 24);
                let col = 1 + (next() % 80);
                buf.extend_from_slice(format!("\x1b[{row};{col}H\x1b[K").as_bytes());
                buf.extend_from_slice(words[(next() as usize) % words.len()].as_bytes());
            }
        }
    }
    buf
}

fn main() {
    let mib: usize = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(64);
    let bytes = mib * 1024 * 1024;
    let data = workload(bytes);
    let lines = data.iter().filter(|&&b| b == b'\n').count();

    // Warm up (fill caches, page in).
    {
        let mut t = Terminal::new(GridSize::new(80, 24));
        let mut s = Stream::new();
        s.feed(&data, &mut t);
        let _ = t.take_pending_writes();
    }

    let iters = 5;
    let mut best = f64::INFINITY;
    let mut total = 0.0;
    for i in 0..iters {
        let mut t = Terminal::new(GridSize::new(80, 24));
        let mut s = Stream::new();
        let start = Instant::now();
        s.feed(&data, &mut t);
        let _ = t.take_pending_writes();
        let secs = start.elapsed().as_secs_f64();
        total += secs;
        if secs < best {
            best = secs;
        }
        let mibps = (data.len() as f64 / (1024.0 * 1024.0)) / secs;
        println!("iter {i}: {secs:.4}s  {mibps:8.1} MiB/s");
    }

    let avg = total / iters as f64;
    let best_mibps = (data.len() as f64 / (1024.0 * 1024.0)) / best;
    let avg_mibps = (data.len() as f64 / (1024.0 * 1024.0)) / avg;
    let lines_per_s = lines as f64 / best;
    println!("------------------------------------------------------");
    println!("payload : {mib} MiB ({} lines), 80x24 grid", lines);
    println!("best    : {best:.4}s  {best_mibps:8.1} MiB/s");
    println!("avg     : {avg:.4}s  {avg_mibps:8.1} MiB/s");
    println!("lines/s : {lines_per_s:.0}");
}
