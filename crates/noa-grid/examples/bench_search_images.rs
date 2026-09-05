//! Headless timings for image indexing and search lock work.
//! Run with --sync to measure a synchronous search on the same data.

use noa_core::GridSize;
use noa_grid::{ImageStore, Terminal};
use noa_vt::Stream;
use std::hint::black_box;
use std::time::Instant;

fn summarize(label: &str, mut samples: Vec<f64>) {
    samples.sort_by(f64::total_cmp);
    println!(
        "{label}: median_us={:.3} p95_us={:.3} p99_us={:.3}",
        samples[samples.len() / 2],
        samples[samples.len() * 95 / 100],
        samples[samples.len() * 99 / 100],
    );
}

fn main() {
    let synchronous = std::env::args().any(|arg| arg == "--sync");
    for count in [1usize, 1024, 2048, 4096] {
        let mut times = Vec::new();
        for _ in 0..31 {
            let mut store = ImageStore::new();
            let start = Instant::now();
            for _ in 0..count {
                store
                    .insert_rgba((4096 / count) as u32, 1, vec![1; 16384 / count])
                    .unwrap();
            }
            for id in (1..=count as u32).rev() {
                black_box(store.get(id));
            }
            times.push(start.elapsed().as_secs_f64() * 1e6);
            black_box(store);
        }
        summarize(&format!("equal_16KiB_image_pixels count={count}"), times);
    }
    let mut terminal = Terminal::new(GridSize::new(120, 40));
    Stream::new().feed(
        "ordinary search line abcdefghijklmnopqrstuvwxyz\r\n"
            .repeat(20000)
            .as_bytes(),
        &mut terminal,
    );
    let mut full = Vec::new();
    let mut held = Vec::new();
    for _ in 0..31 {
        let start = Instant::now();
        if synchronous {
            terminal.set_search_query("search");
            held.push(start.elapsed().as_secs_f64() * 1e6);
        } else {
            let snapshot = terminal.active().search_snapshot();
            let lock_us = start.elapsed().as_secs_f64() * 1e6;
            let matches = snapshot.find_matches("search", || false).unwrap();
            let matches = std::sync::Arc::from(matches.into_boxed_slice());
            let apply = Instant::now();
            assert!(terminal.apply_search_snapshot(&snapshot, "search".into(), matches));
            held.push(lock_us + apply.elapsed().as_secs_f64() * 1e6);
        }
        full.push(start.elapsed().as_secs_f64() * 1e6);
    }
    summarize("search_20k_rows_total", full);
    summarize("search_20k_rows_terminal_lock_work", held);
    black_box(terminal);
}
