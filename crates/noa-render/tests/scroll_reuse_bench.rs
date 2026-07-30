//! Snapshot-path benchmark for clean-row reuse under scroll (Bolt,
//! `perf/snapshot-scroll-reuse`).
//!
//! `bench/`'s `bench_throughput` measures the parser/grid ingest path and
//! never touches `FrameSnapshot` — it can't see this optimization at all.
//! This harness isolates the actual thing that changed:
//! `FrameSnapshot::from_terminal_recycle`'s per-call cost and clean-row
//! reuse rate, under three workloads:
//!
//!   - **scroll**: a pure-vertical-scroll flood (`cat` a large file) — the
//!     motivating case. Every call advances the live-following viewport by
//!     one line.
//!   - **static**: no output between snapshots — the pre-existing
//!     exact-key-match fast path, included as a control (unaffected by this
//!     change, should already reuse every row).
//!   - **mixed**: scroll interleaved with in-place rewrites (a `\r`
//!     progress-bar redraw before the newline) — exercises the ordinary
//!     per-row dirty bit doing its job on top of the realignment.
//!
//! No GPU/device is needed — this is pure `noa-grid`/`noa-render` snapshot
//! construction, so the harness always runs (headless CI included), unlike
//! the `pipeline.rs`/`tab_switch_bench.rs` tests that skip without an
//! adapter.
//!
//! Run standalone for numbers: `cargo test -p noa-render --offline --test
//! scroll_reuse_bench --release -- --nocapture --test-threads=1`

use noa_core::GridSize;
use noa_grid::Terminal;
use noa_render::FrameSnapshot;
use noa_vt::Stream;
use std::time::Instant;

const COLS: u16 = 120;
const ROWS: u16 = 40;
/// Snapshots taken per workload. Large enough that per-call noise (a few
/// hundred ns of scheduler jitter) averages out; small enough to run in a
/// fraction of a second even in an unoptimized debug build.
const ITERS: usize = 4_000;

struct WorkloadResult {
    name: &'static str,
    median_ns: u64,
    p90_ns: u64,
    /// Fraction of rows `Screen` itself reports clean (`row_dirty == false`)
    /// this frame. This is a *workload* characteristic (how much on-screen
    /// content is genuinely unchanged), not a measurement of the
    /// optimization: even the pre-fix code clones every row regardless of
    /// this bit once the recycle key mismatches (the whole bug this branch
    /// fixes), so a clean row and a *reused* row are different things. The
    /// timing columns are the actual evidence for the fix; this column is
    /// printed only so a reader can sanity-check the workload shape (a
    /// mostly-unchanged screen should show a high number here in every
    /// workload, fixed or not).
    dirty_free_rate: f64,
}

fn percentile(sorted: &[u64], p: f64) -> u64 {
    let idx = ((sorted.len() - 1) as f64 * p).round() as usize;
    sorted[idx]
}

/// Runs `ITERS` snapshot cycles, calling `step` before each one to advance
/// the terminal, and returns timing stats for
/// `FrameSnapshot::from_terminal_recycle` alone (terminal mutation via
/// `step` is excluded from the timed region) plus the diagnostic
/// dirty-free-row fraction (see [`WorkloadResult::dirty_free_rate`]).
fn run_workload(
    name: &'static str,
    mut term: Terminal,
    mut step: impl FnMut(&mut Terminal, usize),
) -> WorkloadResult {
    let mut recycle = FrameSnapshot::from_terminal(&mut term).into_recycle();
    let mut durations = Vec::with_capacity(ITERS);
    let mut total_rows = 0u64;
    let mut clean_rows = 0u64;

    for i in 0..ITERS {
        step(&mut term, i);
        let start = Instant::now();
        let snap = FrameSnapshot::from_terminal_recycle(&mut term, recycle);
        durations.push(start.elapsed().as_nanos() as u64);

        total_rows += snap.row_dirty.len() as u64;
        clean_rows += snap.row_dirty.iter().filter(|d| !**d).count() as u64;
        recycle = snap.into_recycle();
    }

    durations.sort_unstable();
    WorkloadResult {
        name,
        median_ns: percentile(&durations, 0.50),
        p90_ns: percentile(&durations, 0.90),
        dirty_free_rate: clean_rows as f64 / total_rows as f64,
    }
}

#[test]
fn scroll_reuse_bench() {
    let mut stream = Stream::new();
    let scroll = run_workload(
        "scroll (pure vertical flood)",
        Terminal::new(GridSize::new(COLS, ROWS)),
        move |term, i| {
            stream.feed(format!("line {i}\r\n").as_bytes(), term);
        },
    );

    let static_screen = run_workload(
        "static (no output)",
        {
            let mut term = Terminal::new(GridSize::new(COLS, ROWS));
            let mut stream = Stream::new();
            stream.feed(
                b"a static screen, unchanged between snapshots\r\n",
                &mut term,
            );
            term
        },
        |_term, _i| {},
    );

    let mut mixed_stream = Stream::new();
    let mixed = run_workload(
        "mixed (scroll + in-place rewrite)",
        Terminal::new(GridSize::new(COLS, ROWS)),
        move |term, i| {
            if i % 4 == 0 {
                // A progress-bar-style redraw: overwrite the current line in
                // place (no scroll) before the next one advances the flood.
                mixed_stream.feed(format!("\rprogress {i}").as_bytes(), term);
            }
            mixed_stream.feed(format!("line {i}\r\n").as_bytes(), term);
        },
    );

    for r in [&scroll, &static_screen, &mixed] {
        eprintln!(
            "[scroll-reuse-bench] {:<34} median={:>6}ns p90={:>6}ns dirty_free={:.1}%",
            r.name,
            r.median_ns,
            r.p90_ns,
            r.dirty_free_rate * 100.0,
        );
    }

    // Diagnostic sanity only — confirms the workloads have the shape they
    // claim (a mostly-unchanged screen), not that the optimization fired.
    // The actual before/after comparison is an external A/B (stash this
    // branch's `src/snapshot.rs` change, keep this bench file, rerun
    // interleaved — see METHODOLOGY.md and the commit body for the numbers).
    assert!(
        static_screen.dirty_free_rate > 0.9,
        "static screen control should be almost entirely dirty-free"
    );
    assert!(
        scroll.dirty_free_rate > 0.5,
        "single-line-per-frame scroll should leave most rows dirty-free, got {:.1}%",
        scroll.dirty_free_rate * 100.0
    );
}
