//! What does a font-size / DPI change actually cost?
//!
//! `FontGrid` carries `px_size` as an object-scoped field with no size in any
//! cache key, so a size or scale change is only expressible as whole-object
//! reconstruction (`FontGrid::new`) — 5 call sites in `noa-app`, x2 app-wide
//! grids (terminal + sidebar). This example measures the CPU half of that
//! rebuild, which has never been measured in this repo: no font/DPI workload
//! exists in `docs/performance-measurements.md`, and
//! `performance-resource-optimization-matrix.md` states its own figures are
//! "estimates ... not measured values".
//!
//! Phases measured, each as a median over N reps in one binary (per the
//! journal's methodology rule: only same-binary, same-path medians compare):
//!
//!   A  load_font_stack          — system font discovery, which `grid.rs`
//!                                 calls "the expensive part" of `new`
//!   B  with_stack               — metrics + empty atlases; the part a caller
//!                                 keeps by reusing an already-loaded stack.
//!                                 Measured directly, never as `C - A`: at
//!                                 ~20 ms with ms-scale jitter that
//!                                 subtraction is pure noise.
//!   C  FontGrid::new            — A + B
//!   D  prewarm                  — shape_run + raster_shaped over the warm
//!                                 glyph set, i.e. what has to be re-rastered
//!                                 before the first post-change frame looks
//!                                 the same as the last pre-change frame
//!
//! Run:  cargo run --offline --release -p noa-font --example bench_size_change

use std::time::{Duration, Instant};

use noa_font::{
    FontConfig, FontGrid, ShapeCell, StyleKey, load_font_stack, load_font_stack_with_primary,
    load_primary_font,
};

/// ppem rungs a user actually traverses: a 14pt font at scale 1.0 / 1.25 /
/// 1.5 / 2.0, plus the neighbouring integer steps of a size scrub.
const PPEM_RUNGS: &[f32] = &[13.0, 14.0, 15.0, 17.5, 21.0, 28.0];

fn median(mut v: Vec<Duration>) -> Duration {
    v.sort_unstable();
    v[v.len() / 2]
}

/// Median plus the observed spread — at ~20 ms with a few ms of jitter, a bare
/// median invites reading a difference that is really noise.
fn stats(mut v: Vec<Duration>) -> (Duration, Duration, Duration) {
    v.sort_unstable();
    (v[0], v[v.len() / 2], v[v.len() - 1])
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1e3
}

/// The set a terminal must have hot before the first frame after a size change
/// looks like the last frame before it: printable ASCII in all four styles.
fn warm_set() -> Vec<(char, StyleKey)> {
    let styles = [
        StyleKey {
            bold: false,
            italic: false,
        },
        StyleKey {
            bold: true,
            italic: false,
        },
        StyleKey {
            bold: false,
            italic: true,
        },
        StyleKey {
            bold: true,
            italic: true,
        },
    ];
    let mut out = Vec::new();
    for style in styles {
        for c in ' '..='~' {
            out.push((c, style));
        }
    }
    out
}

/// Rasterize the warm set through the real render path (`shape_run` then
/// `raster_shaped`), the way `noa-render` drives it — not the simpler
/// per-char `get_or_raster` cache.
fn prewarm(grid: &mut FontGrid, set: &[(char, StyleKey)]) -> usize {
    let mut glyphs = 0;
    for &(ch, style) in set {
        let cells = [ShapeCell {
            ch,
            combining: Vec::new(),
            style,
        }];
        let shaped = grid.shape_run(&cells);
        for g in shaped.iter() {
            grid.raster_shaped(g.face_id, g.glyph_id, style, 1);
            glyphs += 1;
        }
    }
    glyphs
}

fn main() {
    let reps: usize = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(9);
    let cfg = FontConfig::default();

    println!("# font size/DPI change cost — {reps} reps, medians, one binary");
    println!("# primary family: platform default (FontConfig::default)\n");

    // ---- A: system font discovery -----------------------------------------
    // Warm once so the first-call page-fault / CoreText cache cost is not
    // charged to the median (a size change in a running app is never cold).
    let _ = load_font_stack(&cfg).expect("font stack");
    let a: Vec<Duration> = (0..reps)
        .map(|_| {
            let t = Instant::now();
            let stack = load_font_stack(&cfg).expect("font stack");
            let d = t.elapsed();
            drop(stack);
            d
        })
        .collect();
    let (a_lo, a_med, a_hi) = stats(a);

    // ---- A1/A2: split A, because `load_font_stack` builds a font-kit
    // `SystemSource` TWICE (once in `load_primary_font`, once in
    // `load_font_stack_with_primary`) and the second stage additionally walks
    // `all_families()` looking for Nerd Font families. Knowing which half
    // dominates decides what a fix should target.
    let a1: Vec<Duration> = (0..reps)
        .map(|_| {
            let t = Instant::now();
            let p = load_primary_font(&cfg).expect("primary");
            let d = t.elapsed();
            drop(p);
            d
        })
        .collect();
    let (_, a1_med, _) = stats(a1);
    let a2: Vec<Duration> = (0..reps)
        .map(|_| {
            let primary = load_primary_font(&cfg).expect("primary");
            let t = Instant::now();
            let s = load_font_stack_with_primary(primary, &cfg).expect("stack");
            let d = t.elapsed();
            drop(s);
            d
        })
        .collect();
    let (_, a2_med, _) = stats(a2);

    // ---- B: with_stack alone, measured DIRECTLY ---------------------------
    // Not derived as C - A: at ~20 ms with ms-scale jitter the subtraction is
    // dominated by noise (it can even go negative). A fresh stack per rep is
    // required because `with_stack` consumes it, but the load is untimed.
    let b: Vec<Duration> = (0..reps)
        .map(|_| {
            let stack = load_font_stack(&cfg).expect("font stack");
            let t = Instant::now();
            let g = FontGrid::with_stack(stack, 14.0, cfg.clone()).expect("grid");
            let d = t.elapsed();
            drop(g);
            d
        })
        .collect();
    let (b_lo, b_med, b_hi) = stats(b);

    // ---- C: FontGrid::new (A + B) -----------------------------------------
    let c: Vec<Duration> = (0..reps)
        .map(|_| {
            let t = Instant::now();
            let g = FontGrid::new(14.0, cfg.clone()).expect("grid");
            let d = t.elapsed();
            drop(g);
            d
        })
        .collect();
    let (c_lo, c_med, c_hi) = stats(c);

    println!(
        "## construction        {:>10} {:>10} {:>10}",
        "min", "med", "max"
    );
    println!(
        "A  load_font_stack     {:10.3} {:10.3} {:10.3} ms",
        ms(a_lo),
        ms(a_med),
        ms(a_hi)
    );
    println!(
        "A1   load_primary_font {:>10} {:10.3} {:>10} ms   ({:.0}% of A)",
        "",
        ms(a1_med),
        "",
        100.0 * ms(a1_med) / ms(a_med)
    );
    println!(
        "A2   ..._with_primary  {:>10} {:10.3} {:>10} ms   ({:.0}% of A)",
        "",
        ms(a2_med),
        "",
        100.0 * ms(a2_med) / ms(a_med)
    );
    println!(
        "B  with_stack only     {:10.3} {:10.3} {:10.3} ms   <- what with_stack() keeps",
        ms(b_lo),
        ms(b_med),
        ms(b_hi)
    );
    println!(
        "C  FontGrid::new       {:10.3} {:10.3} {:10.3} ms",
        ms(c_lo),
        ms(c_med),
        ms(c_hi)
    );
    // A and C measure the same dominant quantity, so A/C is noise around 100%
    // and is not the honest statistic. (C - B)/C is: B is the only part of
    // `new` that is not discovery, and it is measured directly.
    println!(
        "   discovery share of new: {:.2}%  (= (C-B)/C; A/C is noise around 100%)\n",
        100.0 * (ms(c_med) - ms(b_med)) / ms(c_med)
    );

    // ---- D: prewarm the warm set at each ppem rung ------------------------
    let set = warm_set();
    println!("## prewarm ({} chars x 4 styles)", set.len() / 4);
    println!(
        "{:>7}  {:>10}  {:>8}  {:>10}  {:>12}",
        "ppem", "prewarm ms", "glyphs", "us/glyph", "atlas KiB"
    );

    let mut rung_rows = Vec::new();
    for &px in PPEM_RUNGS {
        let mut samples = Vec::new();
        let mut glyphs = 0;
        let mut atlas_kib = 0.0;
        for _ in 0..reps {
            let stack = load_font_stack(&cfg).expect("font stack");
            let mut grid = FontGrid::with_stack(stack, px, cfg.clone()).expect("grid");
            let t = Instant::now();
            glyphs = prewarm(&mut grid, &set);
            samples.push(t.elapsed());
            let (w, h) = grid.mask_atlas_size();
            atlas_kib = (w as f64 * h as f64) / 1024.0;
        }
        let med = median(samples);
        println!(
            "{px:7.1}  {:10.3}  {glyphs:8}  {:8.2}  {atlas_kib:12.1}",
            ms(med),
            ms(med) * 1e3 / glyphs as f64
        );
        rung_rows.push((px, med, atlas_kib));
    }

    // ---- D-split: where does the prewarm actually go? ---------------------
    // Prewarm now dominates a size step, so the next optimization round needs
    // to know which stage owns it. `shape_run` and `raster_shaped` are both
    // public, so the split needs no instrumentation.
    {
        let stack = load_font_stack(&cfg).expect("font stack");
        let mut grid = FontGrid::with_stack(stack, 14.0, cfg.clone()).expect("grid");
        let mut shape_total = Duration::ZERO;
        let mut raster_total = Duration::ZERO;
        let mut n = 0usize;
        for &(ch, style) in &set {
            let cells = [ShapeCell {
                ch,
                combining: Vec::new(),
                style,
            }];
            let t = Instant::now();
            let shaped = grid.shape_run(&cells);
            shape_total += t.elapsed();
            for g in shaped.iter() {
                let t = Instant::now();
                grid.raster_shaped(g.face_id, g.glyph_id, style, 1);
                raster_total += t.elapsed();
                n += 1;
            }
        }
        let total = shape_total + raster_total;
        println!("\n## prewarm split at 14 ppem (cold caches, {n} glyphs)");
        println!(
            "  shape_run     {:8.3} ms  ({:4.1}%)  {:6.2} us/glyph",
            ms(shape_total),
            100.0 * ms(shape_total) / ms(total),
            ms(shape_total) * 1e3 / n as f64
        );
        println!(
            "  raster_shaped {:8.3} ms  ({:4.1}%)  {:6.2} us/glyph",
            ms(raster_total),
            100.0 * ms(raster_total) / ms(total),
            ms(raster_total) * 1e3 / n as f64
        );

        // Second pass over the same grid: every cache is now hot, so this is
        // the floor a "keep the old size resident" scheme (ppem in the cache
        // key) would converge to on a RETURN to an already-seen size.
        let t = Instant::now();
        let warm_n = prewarm(&mut grid, &set);
        let warm = t.elapsed();
        println!(
            "  all-hit replay{:8.3} ms  ({:6.2} us/glyph, {warm_n} glyphs) <- ppem-in-key ceiling",
            ms(warm),
            ms(warm) * 1e3 / warm_n as f64
        );
    }

    // ---- D-split-2: how much of the raster is the `thicken` dilation? -----
    // `thicken` is a config flag, so this needs no instrumentation. It is the
    // one part of the raster path noa owns outright (swash owns the rest), so
    // it is the only part a fix could target without replacing the scaler.
    {
        let mut plain = cfg.clone();
        plain.thicken = false;
        let mut samples_on = Vec::new();
        let mut samples_off = Vec::new();
        for _ in 0..reps {
            // Interleaved, same binary, same path — a cross-run comparison of
            // these would sit inside the layout-noise floor.
            let stack = load_font_stack(&cfg).expect("stack");
            let mut g = FontGrid::with_stack(stack, 14.0, cfg.clone()).expect("grid");
            let t = Instant::now();
            prewarm(&mut g, &set);
            samples_on.push(t.elapsed());

            let stack = load_font_stack(&plain).expect("stack");
            let mut g = FontGrid::with_stack(stack, 14.0, plain.clone()).expect("grid");
            let t = Instant::now();
            prewarm(&mut g, &set);
            samples_off.push(t.elapsed());
        }
        let on = median(samples_on);
        let off = median(samples_off);
        println!("\n## thicken share of the raster (interleaved A/B)");
        println!("  prewarm, thicken on   {:8.3} ms", ms(on));
        println!("  prewarm, thicken off  {:8.3} ms", ms(off));
        println!(
            "  thicken costs         {:8.3} ms  ({:.1}% of prewarm)",
            ms(on) - ms(off),
            100.0 * (ms(on) - ms(off)) / ms(on)
        );
    }

    // ---- the number that matters ------------------------------------------
    let (px14, warm14, atlas14) = rung_rows
        .iter()
        .find(|(px, _, _)| (*px - 14.0).abs() < f32::EPSILON)
        .copied()
        .expect("14.0 rung");
    let one_grid = c_med + warm14;
    let both_grids = one_grid * 2;

    println!("\n## one size step, as the app actually performs it (at {px14} ppem)");
    println!(
        "  FontGrid::new + prewarm, 1 grid     {:8.3} ms",
        ms(one_grid)
    );
    println!(
        "  x2 app-wide grids (terminal+sidebar){:8.3} ms   <- CPU cost of ONE step",
        ms(both_grids)
    );
    println!(
        "  avoidable by with_stack() alone     {:8.3} ms   ({:.1}% of the step)",
        ms(a_med * 2),
        100.0 * ms(a_med * 2) / ms(both_grids)
    );
    println!("  mask atlas re-uploaded per window   {atlas14:8.1} KiB (R8, 1 byte/px)");
    println!("\n# NOT measured here: GPU texture upload, bind-group rebuild,");
    println!("# relayout, and the color atlas. CPU half only.");
}
