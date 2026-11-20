//! CPU cost and heap allocations of the per-frame split-pane draw plan
//! (`build_draw_plan_with_overlays`, called by every `Renderer::draw_panes`):
//! one pane, a four-pane grid, and the four-pane grid under a two-pane
//! overlay group. CPU only — no adapter needed.
//!
//! Run: cargo run --offline --release -p noa-render --example bench_draw_plan -- [iterations]

use std::alloc::{GlobalAlloc, Layout, System};
use std::hint::black_box;
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};
use std::time::Instant;

use noa_render::{OverlayGroup, PaneId, PaneRect, build_draw_plan_with_overlays};

/// Counts allocations so the report can show what a plan costs the heap.
struct Counting;

static ALLOCS: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCS.fetch_add(1, Relaxed);
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

fn run(label: &str, iterations: usize, layout: &[(PaneId, PaneRect)], overlays: &[OverlayGroup]) {
    let focused = Some(layout[0].0);
    let allocs_before = ALLOCS.load(Relaxed);
    let start = Instant::now();
    for _ in 0..iterations {
        black_box(build_draw_plan_with_overlays(
            black_box(layout),
            black_box(overlays),
            focused,
            None,
            false,
        ));
    }
    let elapsed = start.elapsed();
    let allocs = ALLOCS.load(Relaxed) - allocs_before;
    println!(
        "{label:<16} n={iterations:<8} ns_per_plan={:<8.1} allocs_per_plan={:.2}",
        elapsed.as_nanos() as f64 / iterations as f64,
        allocs as f64 / iterations as f64,
    );
}

fn main() {
    let iterations: usize = std::env::args()
        .nth(1)
        .map_or(2_000_000, |s| s.parse().expect("iterations"));
    let pane = |id, x, y, w, h| (PaneId::new(id), PaneRect::new(x, y, w, h));

    let single = [pane(1, 0, 0, 2000, 1200)];
    run("single", iterations, &single, &[]);

    let grid = [
        pane(1, 0, 0, 999, 599),
        pane(2, 1000, 0, 1000, 599),
        pane(3, 0, 600, 999, 600),
        pane(4, 1000, 600, 1000, 600),
    ];
    run("grid4", iterations, &grid, &[]);

    let mut with_overlay = grid.to_vec();
    with_overlay.push(pane(10, 400, 300, 1200, 24));
    with_overlay.push(pane(11, 400, 324, 1200, 576));
    let overlays = [OverlayGroup {
        outer: PaneRect::new(400, 300, 1200, 600),
        header: None,
        panes: vec![PaneId::new(10), PaneId::new(11)],
        focused: true,
    }];
    run("grid4_overlay", iterations, &with_overlay, &overlays);
}
