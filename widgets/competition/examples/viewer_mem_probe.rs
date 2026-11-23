//! Heap held by `compare.html` refreshes in the 10-round `perf_probe` setup:
//! the peak and what stays allocated afterwards, uncached and cached.
//!
//! `cargo --offline run --release --example viewer_mem_probe`

use std::alloc::{GlobalAlloc, Layout, System};
use std::fs;
use std::hint::black_box;
use std::sync::atomic::{AtomicUsize, Ordering};

use competition::model::types::PartId;
use competition::model::viewer;
use competition::preview;
use competition::store;

#[path = "perf_probe.rs"]
#[allow(dead_code)]
mod perf_probe;

use perf_probe::{ROUNDS, populate_snapshot, simulate_competition};

struct Counting;

static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let p = unsafe { System.alloc(layout) };
        if !p.is_null() {
            let live = LIVE.fetch_add(layout.size(), Ordering::Relaxed) + layout.size();
            PEAK.fetch_max(live, Ordering::Relaxed);
        }
        p
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) };
        LIVE.fetch_sub(layout.size(), Ordering::Relaxed);
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let p = unsafe { System.realloc(ptr, layout, new_size) };
        if !p.is_null() {
            if new_size >= layout.size() {
                let live = LIVE.fetch_add(new_size - layout.size(), Ordering::Relaxed)
                    + (new_size - layout.size());
                PEAK.fetch_max(live, Ordering::Relaxed);
            } else {
                LIVE.fetch_sub(layout.size() - new_size, Ordering::Relaxed);
            }
        }
        p
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

/// Runs `f`, returning (peak heap above the starting point, heap still held
/// afterwards above the starting point) in bytes.
fn measure<T>(f: impl FnOnce() -> T) -> (usize, isize, T) {
    let start = LIVE.load(Ordering::Relaxed);
    PEAK.store(start, Ordering::Relaxed);
    let out = f();
    let peak = PEAK.load(Ordering::Relaxed) - start;
    let held = LIVE.load(Ordering::Relaxed) as isize - start as isize;
    (peak, held, out)
}

fn kib(bytes: usize) -> f64 {
    bytes as f64 / 1024.0
}

fn main() {
    let (session, _, _, _) = simulate_competition(ROUNDS);
    let panel = [
        PartId::new("claude"),
        PartId::new("codex"),
        PartId::new("agy"),
    ];
    let root = std::env::temp_dir().join(format!("noa-comp-mem-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    for r in 1..=ROUNDS {
        for p in &panel {
            let snap = store::artifact_snapshot_dir(&root, r, p);
            populate_snapshot(&snap);
            if r == 1 && p.as_str() == "claude" {
                let _ = preview::write(&snap).unwrap();
            }
        }
    }
    let html_len = viewer::render_html(&session, &root).len();
    println!("=== viewer_mem_probe ({ROUNDS} rounds x 3 contestants, 40 refreshes) ===");
    println!("compare.html at R{ROUNDS}: {:.1} KiB", kib(html_len));

    let (peak, held, _) = measure(|| {
        let mut partial = session.clone();
        partial.rounds.clear();
        for r in &session.rounds {
            partial.rounds.push(r.clone());
            for _ in 0..4 {
                black_box(viewer::render_html(&partial, &root));
            }
        }
        partial
    });
    println!(
        "uncached render_html: peak {:8.1} KiB  held after {:8.1} KiB (incl. session copy)",
        kib(peak),
        held as f64 / 1024.0
    );

    let mut cache = viewer::ViewerCache::new();
    let (peak, held, _) = measure(|| {
        let mut partial = session.clone();
        partial.rounds.clear();
        for r in &session.rounds {
            partial.rounds.push(r.clone());
            for _ in 0..4 {
                black_box(viewer::render_html_cached(&partial, &root, &mut cache));
            }
        }
        partial
    });
    println!(
        "cached render_html:   peak {:8.1} KiB  held after {:8.1} KiB (incl. session copy + cache)",
        kib(peak),
        held as f64 / 1024.0
    );
    let (_, cache_bytes, _) = measure(|| drop(cache));
    println!(
        "  of which ViewerCache: {:8.1} KiB",
        -cache_bytes as f64 / 1024.0
    );

    let _ = fs::remove_dir_all(&root);
}
