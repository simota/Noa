//! CPU-side cost of the three per-frame renderer paths a terminal keeps paying:
//! an unchanged redraw (`draw`), a one-row update (`rebuild_cells` then `draw`),
//! and a frame that brings a new glyph (`sync_atlas` after a raster miss). The
//! `draw` samples include `queue.submit`; GPU retirement is drained every frame
//! outside the timed section so a backlog can't leak between samples.
//!
//! Run: cargo run --offline --release -p noa-render --example bench_frame
//! Needs a real adapter; exits with a message when none is available.

use std::time::Instant;

use noa_core::{DEFAULT_GRID_PADDING, GridSize, PixelSize};
use noa_font::{FontConfig, FontGrid};
use noa_grid::Terminal;
use noa_render::{FrameSnapshot, Renderer, Theme};
use noa_vt::Stream;

const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Bgra8UnormSrgb;
const COLS: u16 = 200;
const ROWS: u16 = 60;

fn report(label: &str, mut samples_us: Vec<f64>) {
    samples_us.sort_by(f64::total_cmp);
    let at = |pct: usize| samples_us[(samples_us.len() - 1) * pct / 100];
    println!(
        "{label:<22} n={:<5} p50_us={:<9.1} p95_us={:<9.1} p99_us={:<9.1}",
        samples_us.len(),
        at(50),
        at(95),
        at(99),
    );
}

fn device_queue() -> Option<(wgpu::Device, wgpu::Queue)> {
    let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor::default());
    let adapter =
        pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
            .ok()?;
    pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).ok()
}

fn main() {
    let Some((device, queue)) = device_queue() else {
        eprintln!("no wgpu adapter available");
        return;
    };
    let viewport = PixelSize {
        w: u32::from(COLS) * 10,
        h: u32::from(ROWS) * 20,
    };
    let target = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("bench-target"),
        size: wgpu::Extent3d {
            width: viewport.w,
            height: viewport.h,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    let view = target.create_view(&wgpu::TextureViewDescriptor::default());

    let mut terminal = Terminal::new(GridSize::new(COLS, ROWS));
    let mut stream = Stream::new();
    for row in 0..ROWS {
        stream.feed(
            format!(
                "\x1b[{}mrow {row} the quick brown fox 日本語\x1b[0m\r\n",
                31 + row % 7
            )
            .as_bytes(),
            &mut terminal,
        );
    }

    let theme = Theme::new();
    let mut font = FontGrid::new(14.0, FontConfig::default()).expect("system font");
    let mut renderer = Renderer::new(&device, &queue, FORMAT, &mut font, DEFAULT_GRID_PADDING)
        .expect("build renderer");
    renderer.resize(viewport);
    let snap = FrameSnapshot::from_terminal(&mut terminal);
    renderer.rebuild_cells(&snap, &mut font, &theme);
    renderer.sync_atlas(&device, &queue, &mut font);
    renderer.draw(&device, &queue, &view);
    let drain = || {
        let _ = device.poll(wgpu::PollType::wait_indefinitely());
    };
    drain();

    let mut samples = Vec::new();
    for _ in 0..2000 {
        let start = Instant::now();
        renderer.draw(&device, &queue, &view);
        samples.push(start.elapsed().as_secs_f64() * 1e6);
        drain();
    }
    report("draw_unchanged", samples);

    let mut samples = Vec::new();
    for i in 0..1000 {
        stream.feed(format!("\x1b[5;1Hupdate {i}").as_bytes(), &mut terminal);
        let snap = FrameSnapshot::from_terminal(&mut terminal);
        let start = Instant::now();
        renderer.rebuild_cells(&snap, &mut font, &theme);
        renderer.sync_atlas(&device, &queue, &mut font);
        renderer.draw(&device, &queue, &view);
        samples.push(start.elapsed().as_secs_f64() * 1e6);
        drain();
    }
    report("one_row_update", samples);

    let mut samples = Vec::new();
    for i in 0..1000u32 {
        let ch = char::from_u32(0x4e00 + i).expect("CJK ideograph");
        stream.feed(format!("\x1b[6;1H{ch}").as_bytes(), &mut terminal);
        let snap = FrameSnapshot::from_terminal(&mut terminal);
        let start = Instant::now();
        renderer.rebuild_cells(&snap, &mut font, &theme);
        renderer.sync_atlas(&device, &queue, &mut font);
        renderer.draw(&device, &queue, &view);
        samples.push(start.elapsed().as_secs_f64() * 1e6);
        drain();
    }
    report("new_glyph_frame", samples);
}
