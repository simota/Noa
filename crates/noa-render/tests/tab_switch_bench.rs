//! Headless baseline benchmark for the "tab switch to a busy tab momentarily
//! freezes" problem (Kaizen cycle 1, measurement only — no behavior change).
//!
//! Diagnosis: while a macOS-tab window is occluded, `redraw()` early-returns
//! so `PaneRenderCache` goes stale; on reveal, the first frame's cache lookup
//! misses (the pane's absolute row base advanced while backgrounded, or the
//! shared glyph atlas evicted entries other tabs needed), forcing
//! `rebuild_pane_cached`'s `full=true` path — every visible row is
//! re-shaped and, for glyphs no longer resident, re-rasterized.
//!
//! This benchmark isolates that cost on a real (not synthetic) VT-parsed
//! terminal: a 200×60 grid of varied colored ASCII + CJK + emoji text, fed
//! through an actual `noa_vt::Stream` into a real `noa_grid::Terminal`, then
//! rebuilt with `Renderer::rebuild_cells`. It measures:
//!
//!   - a cold-atlas full rebuild (fresh `FontGrid`, nothing rasterized yet —
//!     the worst case: every glyph shapes AND rasterizes), and
//!   - a warm-atlas full rebuild (same glyphs, but the atlas already holds
//!     them from a prior rebuild — isolates row re-shaping/layout cost from
//!     rasterization).
//!
//! A full rebuild is forced for both by advancing `abs_row_base` between
//! builds (mirrors a backgrounded tab's shell producing output while
//! occluded), the same cache-miss trigger `rebuild_pane_cached` uses in
//! production — no internal (`pub(super)`) API is touched.
//!
//! Skips gracefully where no GPU adapter is available (headless CI without a
//! Metal/Vulkan device), matching `pipeline.rs`'s convention.

// Shared with `pipeline.rs`'s test binary; this binary only needs
// `device_queue`, so the rest of the module's helpers go unused here.
#[path = "pipeline/shared.rs"]
#[allow(dead_code)]
mod shared;

use noa_core::{DEFAULT_GRID_PADDING, GridSize, PixelSize};
use noa_font::FontGrid;
use noa_grid::Terminal;
use noa_render::{FrameSnapshot, Renderer, Theme};
use noa_vt::Stream;
use std::time::Instant;

const COLS: u16 = 200;
const ROWS: u16 = 60;

/// Feeds a real VT byte stream into a fresh `Terminal`, producing a busy
/// full-viewport frame: every row cycles through 8 SGR foreground colors and
/// a mix of ASCII, CJK, and emoji text (so glyph shaping/rasterization sees
/// realistic width and script variety, not a single repeated glyph).
fn busy_terminal() -> Terminal {
    let mut terminal = Terminal::new(GridSize::new(COLS, ROWS));
    let mut stream = Stream::new();
    let words = [
        "fn",
        "ホスト",
        "端末",
        "🦀",
        "match",
        "パイプ",
        "Ok(0)",
        "😀",
    ];
    let mut bytes = Vec::new();
    for row in 0..ROWS {
        let color = 31 + (row as usize % 8); // SGR 31..=38
        bytes.extend_from_slice(format!("\x1b[{color}m").as_bytes());
        let mut written = 0usize;
        let mut i = 0;
        while written < COLS as usize {
            let word = words[(row as usize + i) % words.len()];
            bytes.extend_from_slice(word.as_bytes());
            written += word.chars().count();
            i += 1;
        }
        bytes.extend_from_slice(b"\x1b[0m\r\n");
    }
    stream.feed(&bytes, &mut terminal);
    terminal
}

/// A snapshot of `terminal` with `abs_row_base` bumped by `bump` — mirrors
/// what a backgrounded tab's advancing scrollback does across an occluded
/// period, which is what forces `rebuild_pane_cached`'s cache-miss `full`
/// path on reveal (a real cache, not touched here, keys off exactly this
/// field — see `FrameInvalidationKey` in `noa-render/src/renderer/cell.rs`).
fn snapshot_with_row_base(terminal: &mut Terminal, bump: usize) -> FrameSnapshot {
    let mut snap = FrameSnapshot::from_terminal(terminal);
    snap.abs_row_base += bump;
    snap.row_dirty = vec![true; snap.rows.len()];
    snap
}

#[test]
fn tab_switch_full_rebuild_baseline() {
    let Some((device, queue)) = shared::device_queue() else {
        eprintln!("no wgpu adapter available — skipping tab-switch bench");
        return;
    };

    let mut terminal = busy_terminal();
    let theme = Theme::new();
    let viewport = PixelSize {
        w: COLS as u32 * 10,
        h: ROWS as u32 * 20,
    };

    // Cold atlas: nothing rasterized yet, so the first rebuild both shapes
    // every row AND rasterizes every distinct glyph into the atlas.
    let mut cold_font =
        FontGrid::new(14.0, noa_font::FontConfig::default()).expect("load a system monospace font");
    let mut cold_renderer = Renderer::new(
        &device,
        &queue,
        wgpu::TextureFormat::Bgra8UnormSrgb,
        &mut cold_font,
        DEFAULT_GRID_PADDING,
    )
    .expect("build cold-atlas renderer");
    cold_renderer.resize(viewport);

    let cold_snap = snapshot_with_row_base(&mut terminal, 0);
    let cold_start = Instant::now();
    cold_renderer.rebuild_cells(&cold_snap, &mut cold_font, &theme);
    cold_renderer.sync_atlas(&device, &queue, &mut cold_font);
    let cold_elapsed = cold_start.elapsed();
    let cold_rows_rebuilt = cold_renderer.rows_rebuilt_last_frame();

    // Warm atlas: same font/atlas already holding every glyph from the
    // build above, but a *fresh* cache miss (bumped `abs_row_base`, as a
    // backgrounded tab's scrollback would) forces a second full rebuild —
    // isolating row-shaping/layout cost with rasterization taken out.
    let warm_snap = snapshot_with_row_base(&mut terminal, 5_000);
    let warm_start = Instant::now();
    cold_renderer.rebuild_cells(&warm_snap, &mut cold_font, &theme);
    cold_renderer.sync_atlas(&device, &queue, &mut cold_font);
    let warm_elapsed = warm_start.elapsed();
    let warm_rows_rebuilt = cold_renderer.rows_rebuilt_last_frame();

    eprintln!(
        "[tab-switch-bench] {}x{} full rebuild: cold-atlas {:.3}ms (rows_rebuilt={}), warm-atlas {:.3}ms (rows_rebuilt={})",
        COLS,
        ROWS,
        cold_elapsed.as_secs_f64() * 1e3,
        cold_rows_rebuilt,
        warm_elapsed.as_secs_f64() * 1e3,
        warm_rows_rebuilt,
    );

    assert_eq!(
        cold_rows_rebuilt, ROWS as u64,
        "first-ever rebuild must be a full rebuild of every row"
    );
    assert_eq!(
        warm_rows_rebuilt, ROWS as u64,
        "bumped abs_row_base must force a second full rebuild (cache-miss path)"
    );
}
