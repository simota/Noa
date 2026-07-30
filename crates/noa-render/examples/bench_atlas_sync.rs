//! The GPU half of a font-size / DPI change.
//!
//! `crates/noa-font/examples/bench_size_change.rs` measures the CPU half
//! (`load_font_stack` + `FontGrid` construction + glyph re-raster) and
//! explicitly does not cover what happens after: on a size or scale change
//! `noa-app` calls `Renderer::sync_atlas` for **every window**
//! (`app/event_loop.rs`'s `on_scale_factor_changed`), which re-uploads the
//! glyph atlas and rebuilds every stale pane bind group.
//!
//! Measured here, at 14 ppem with the atlas warmed to a realistic occupancy:
//!
//!   S1  first sync after a rebuild  — new `atlas_identity` ⇒ texture
//!                                     recreated, full contents uploaded,
//!                                     every pane bind group rebuilt
//!   S2  steady-state sync           — nothing changed; the per-frame cost
//!                                     that must stay ~free
//!   S3  raw write_texture           — the upload alone, isolated from
//!                                     bind-group work
//!
//! GPU work is queued, not synchronous, so every phase is reported twice: the
//! CPU-side call, and the same call followed by `submit` + a blocking `poll`
//! (`+drain`) so the queue has actually retired. Reporting only the former
//! would credit the driver's laziness as speed.
//!
//! Run: cargo run --offline --release -p noa-render --example bench_atlas_sync
//! Needs a real adapter; exits with a message when none is available.

use std::time::{Duration, Instant};

use noa_core::GridPadding;
use noa_font::{FontConfig, FontGrid, ShapeCell, StyleKey};
use noa_render::Renderer;

const PPEM: f32 = 14.0;
const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Bgra8UnormSrgb;

fn stats(mut v: Vec<Duration>) -> (Duration, Duration, Duration) {
    v.sort_unstable();
    (v[0], v[v.len() / 2], v[v.len() - 1])
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1e3
}

fn device_queue() -> Option<(wgpu::Device, wgpu::Queue)> {
    let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor::default());
    let adapter =
        pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
            .ok()?;
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("noa-bench-device"),
        required_features: wgpu::Features::empty(),
        required_limits: wgpu::Limits::default(),
        experimental_features: wgpu::ExperimentalFeatures::default(),
        memory_hints: wgpu::MemoryHints::default(),
        trace: wgpu::Trace::Off,
    }))
    .ok()?;
    Some((device, queue))
}

/// Block until the queue has retired everything submitted so far.
fn drain(device: &wgpu::Device) {
    device
        .poll(wgpu::PollType::wait_indefinitely())
        .expect("poll device");
}

/// Warm the atlas the way the first full-screen frame after a size change
/// does: printable ASCII in all four styles, through the real shape/raster
/// path.
fn warm(font: &mut FontGrid) {
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
    for style in styles {
        for ch in ' '..='~' {
            let cells = [ShapeCell {
                ch,
                combining: Vec::new(),
                style,
            }];
            for g in font.shape_run(&cells).iter() {
                font.raster_shaped(g.face_id, g.glyph_id, style, 1);
            }
        }
    }
}

fn fresh_font() -> FontGrid {
    let mut font = FontGrid::new(PPEM, FontConfig::default()).expect("font grid");
    warm(&mut font);
    font
}

fn main() {
    let reps: usize = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(9);

    let Some((device, queue)) = device_queue() else {
        eprintln!("no wgpu adapter available — skipping (run outside the sandbox)");
        return;
    };
    let info = "real adapter acquired";

    let mut font = fresh_font();
    let (aw, ah) = font.mask_atlas_size();
    let mask_kib = (aw as f64 * ah as f64) / 1024.0;
    let (cw, ch) = font.color_atlas_size();
    let color_kib = (cw as f64 * ch as f64 * 4.0) / 1024.0;

    let mut renderer = Renderer::new(
        &device,
        &queue,
        FORMAT,
        &mut font,
        GridPadding::new(0.0, 0.0, 0.0, 0.0),
    )
    .expect("renderer");
    renderer.sync_atlas(&device, &queue, &mut font);
    drain(&device);

    println!("# glyph atlas GPU sync cost — {reps} reps, medians ({info})");
    println!(
        "# {PPEM} ppem, mask atlas {aw}x{ah} = {mask_kib:.0} KiB R8, color atlas {color_kib:.0} KiB\n"
    );

    // ---- S0: an empty submit + blocking poll. Every "+drain" figure below
    // carries this fixed round trip, and in the running app the atlas upload
    // rides along with the frame's own submit rather than paying its own.
    let s0: Vec<Duration> = (0..reps)
        .map(|_| {
            let t = Instant::now();
            queue.submit(std::iter::empty());
            drain(&device);
            t.elapsed()
        })
        .collect();
    let (_, s0_med, _) = stats(s0);

    // ---- S2 first: steady state, nothing changed --------------------------
    let s2: Vec<Duration> = (0..reps)
        .map(|_| {
            let t = Instant::now();
            renderer.sync_atlas(&device, &queue, &mut font);
            t.elapsed()
        })
        .collect();
    let (s2_lo, s2_med, s2_hi) = stats(s2);

    // ---- S1: the size-change path -----------------------------------------
    // A rebuilt `FontGrid` carries a new `atlas_identity`, which forces the
    // shared atlas to recreate its texture and re-upload in full. Building the
    // replacement grid is NOT timed — that is the CPU half, already measured.
    let mut s1 = Vec::with_capacity(reps);
    let mut s1_drained = Vec::with_capacity(reps);
    for _ in 0..reps {
        let mut next = fresh_font();
        let t = Instant::now();
        renderer.sync_atlas(&device, &queue, &mut next);
        s1.push(t.elapsed());
        // `write_texture` only stages; without a submit the poll below returns
        // immediately and the "+drain" column would be measuring nothing.
        queue.submit(std::iter::empty());
        drain(&device);
        s1_drained.push(t.elapsed());
        font = next;
    }
    let (s1_lo, s1_med, s1_hi) = stats(s1);
    let (_, s1d_med, _) = stats(s1_drained);

    // ---- S3: the upload alone ---------------------------------------------
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("noa-bench-mask-atlas"),
        size: wgpu::Extent3d {
            width: aw,
            height: ah,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::R8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    let data = font.mask_atlas_data().to_vec();
    let mut s3 = Vec::with_capacity(reps);
    let mut s3_drained = Vec::with_capacity(reps);
    for _ in 0..reps {
        let t = Instant::now();
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &data,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(aw),
                rows_per_image: Some(ah),
            },
            wgpu::Extent3d {
                width: aw,
                height: ah,
                depth_or_array_layers: 1,
            },
        );
        s3.push(t.elapsed());
        queue.submit(std::iter::empty());
        drain(&device);
        s3_drained.push(t.elapsed());
    }
    let (_, s3_med, _) = stats(s3);
    let (_, s3d_med, _) = stats(s3_drained);

    println!(
        "{:<34}{:>9}{:>9}{:>9}{:>11}",
        "phase", "min", "med", "max", "med+drain"
    );
    println!(
        "{:<34}{:>9}{:>9}{:>9}{:11.3}  ms   <- fixed round trip",
        "S0 empty submit + poll",
        "-",
        "-",
        "-",
        ms(s0_med)
    );
    println!(
        "{:<34}{:9.3}{:9.3}{:9.3}{:11.3}  ms",
        "S1 sync after rebuild (new atlas)",
        ms(s1_lo),
        ms(s1_med),
        ms(s1_hi),
        ms(s1d_med)
    );
    println!(
        "{:<34}{:9.3}{:9.3}{:9.3}{:>11}  ms",
        "S2 steady-state sync (no change)",
        ms(s2_lo),
        ms(s2_med),
        ms(s2_hi),
        "-"
    );
    println!(
        "{:<34}{:>9}{:9.3}{:>9}{:11.3}  ms",
        "S3 write_texture alone (mask)",
        "-",
        ms(s3_med),
        "-",
        ms(s3d_med)
    );

    println!("\n## per size step, as the app performs it");
    println!("  sync_atlas runs once PER WINDOW (`on_scale_factor_changed`):");
    for windows in [1usize, 2, 4] {
        println!(
            "    {windows} window(s): {:8.3} ms CPU-side  /  {:8.3} ms to drain",
            ms(s1_med) * windows as f64,
            ms(s1d_med) * windows as f64
        );
    }
    println!(
        "\n  upload work above the fixed round trip: S1 {:.3} ms, S3 {:.3} ms",
        ms(s1d_med) - ms(s0_med),
        ms(s3d_med) - ms(s0_med)
    );
    println!("\n# Not covered: relayout, the first frame's instance rebuild, and");
    println!("# present. This is atlas upload + bind-group refresh only.");
}
