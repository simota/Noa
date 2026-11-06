//! Per-miss cost of `FontGrid::shape_run` on the shape a text flood produces:
//! every dirty row is new text. Reported on both sides of the shape cache cap,
//! plus the cost of a warm hit, with a digest of the glyph output so two builds
//! can be compared for identical shaping.
//!
//! Run: cargo run --offline --release -p noa-font --example bench_shape -- [reps] [font-family]

use std::time::Instant;

use noa_font::{FontConfig, FontGrid, ShapeCell, StyleKey};

const COLS: usize = 120;

fn row_cells(row: usize) -> Vec<ShapeCell> {
    format!("{row:08} fn main() -> Result<(), Error> {{ let x = y.map(|v| v + 1); }} != => /// ")
        .chars()
        .cycle()
        .take(COLS)
        .map(|ch| ShapeCell {
            ch,
            combining: Vec::new(),
            style: StyleKey::default(),
        })
        .collect()
}

fn report(label: &str, mut samples_us: Vec<f64>, digest: u64) {
    samples_us.sort_by(f64::total_cmp);
    let at = |pct: usize| samples_us[(samples_us.len() - 1) * pct / 100];
    println!(
        "{label:<22} n={:<6} p50_us={:<9.1} p95_us={:<9.1} p99_us={:<9.1} digest={digest:016x}",
        samples_us.len(),
        at(50),
        at(95),
        at(99),
    );
}

fn run(grid: &mut FontGrid, label: &str, rows: std::ops::Range<usize>) {
    let mut samples = Vec::with_capacity(rows.len());
    let mut digest = 0xcbf2_9ce4_8422_2325u64;
    for row in rows {
        let cells = row_cells(row);
        let start = Instant::now();
        let glyphs = grid.shape_run(&cells);
        samples.push(start.elapsed().as_secs_f64() * 1e6);
        for glyph in glyphs.iter() {
            for value in [
                i64::from(glyph.glyph_id),
                i64::from(glyph.face_id.0),
                i64::from(glyph.x_advance),
                i64::from(glyph.x_offset),
                i64::from(glyph.y_offset),
                i64::from(glyph.cluster),
            ] {
                digest = (digest ^ value as u64).wrapping_mul(0x100_0000_01b3);
            }
        }
    }
    report(label, samples, digest);
}

fn main() {
    let reps: usize = std::env::args().nth(1).map_or(1, |s| s.parse().unwrap());
    let mut config = FontConfig::default();
    if let Some(family) = std::env::args().nth(2) {
        config.families = vec![family];
    }
    for _ in 0..reps {
        let start = Instant::now();
        let mut grid = FontGrid::new(28.0, config.clone()).expect("default font stack");
        println!(
            "font_grid_new_ms={:.1}",
            start.elapsed().as_secs_f64() * 1e3
        );
        run(&mut grid, "miss_below_cache_cap", 0..4000);
        run(&mut grid, "hit_warm", 0..4000);
        run(&mut grid, "miss_to_cap", 4000..8192);
        run(&mut grid, "miss_above_cache_cap", 8192..12192);
    }
}
