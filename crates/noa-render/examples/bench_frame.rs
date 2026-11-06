//! CPU-side cost of the three per-frame renderer paths a terminal keeps paying:
//! an unchanged redraw (`draw`), a one-row update (`rebuild_cells` then `draw`),
//! and a frame that brings a new glyph (`sync_atlas` after a raster miss). The
//! `draw` samples include `queue.submit`; GPU retirement is drained every frame
//! outside the timed section so a backlog can't leak between samples. The
//! `rebuild_*` and `selection_drag` sections time only `rebuild_cells`, the CPU
//! work behind output and selection frames, and `pixels` digests rendered frames
//! so two builds can be compared for identical output.
//!
//! Run: cargo run --offline --release -p noa-render --example bench_frame -- [section] [rebuild reps]
//! `section` keeps only the sections whose name contains it (for profiling).
//! Needs a real adapter; exits with a message when none is available.

use std::time::Instant;

use noa_core::{DEFAULT_GRID_PADDING, GridSize, PixelSize};
use noa_font::{FontConfig, FontGrid};
use noa_grid::{SelectionPoint, Terminal};
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

/// FNV-1a of the pixels `view`'s texture holds.
fn pixel_digest(device: &wgpu::Device, queue: &wgpu::Queue, target: &wgpu::Texture) -> u64 {
    let (width, height) = (target.width(), target.height());
    let row_bytes = (width * 4).next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("bench-readback"),
        size: u64::from(row_bytes * height),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
    encoder.copy_texture_to_buffer(
        target.as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(row_bytes),
                rows_per_image: Some(height),
            },
        },
        target.size(),
    );
    queue.submit([encoder.finish()]);
    buffer.slice(..).map_async(wgpu::MapMode::Read, |_| {});
    let _ = device.poll(wgpu::PollType::wait_indefinitely());
    let pixels = buffer.slice(..).get_mapped_range();
    pixels
        .chunks_exact(row_bytes as usize)
        .flat_map(|row| &row[..width as usize * 4])
        .fold(0xcbf2_9ce4_8422_2325, |hash, &byte| {
            (hash ^ u64::from(byte)).wrapping_mul(0x100_0000_01b3)
        })
}

/// A full screen of new text on every row: `width` printable columns per row.
fn screenful(frame: usize, width: usize) -> Vec<u8> {
    let line: String = "the quick brown fox jumps over the lazy dog 0123456789 "
        .chars()
        .cycle()
        .skip(frame % 53)
        .take(width)
        .collect();
    let mut out = Vec::new();
    for row in 0..usize::from(ROWS) {
        out.extend_from_slice(format!("\x1b[{};1H{line}", row + 1).as_bytes());
    }
    out
}

fn main() {
    let section = std::env::args().nth(1).unwrap_or_default();
    let on = |name: &str| name.contains(section.as_str());
    let reps: usize = std::env::args()
        .nth(2)
        .map_or(500, |s| s.parse().expect("reps"));
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
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
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

    if on("draw_unchanged") {
        let mut samples = Vec::new();
        for _ in 0..2000 {
            let start = Instant::now();
            renderer.draw(&device, &queue, &view);
            samples.push(start.elapsed().as_secs_f64() * 1e6);
            drain();
        }
        report("draw_unchanged", samples);
    }

    if on("one_row_update") {
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
    }

    if on("new_glyph_frame") {
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

    for (label, width) in [("rebuild_sparse", 40), ("rebuild_dense", usize::from(COLS))] {
        if !on(label) {
            continue;
        }
        let mut samples = Vec::new();
        for frame in 0..reps {
            stream.feed(&screenful(frame, width), &mut terminal);
            let snap = FrameSnapshot::from_terminal(&mut terminal);
            let start = Instant::now();
            renderer.rebuild_cells(&snap, &mut font, &theme);
            samples.push(start.elapsed().as_secs_f64() * 1e6);
        }
        report(label, samples);
    }

    if on("selection_drag") {
        stream.feed(&screenful(0, usize::from(COLS)), &mut terminal);
        let snap = FrameSnapshot::from_terminal(&mut terminal);
        renderer.rebuild_cells(&snap, &mut font, &theme);
        let mut samples = Vec::new();
        let mut rows_rebuilt = 0;
        for i in 0..1000usize {
            terminal.set_selection(
                SelectionPoint::new(0, 10),
                SelectionPoint::new((i % 200) as u16, 10 + i / 200),
            );
            let snap = FrameSnapshot::from_terminal(&mut terminal);
            let start = Instant::now();
            renderer.rebuild_cells(&snap, &mut font, &theme);
            samples.push(start.elapsed().as_secs_f64() * 1e6);
            rows_rebuilt += renderer.rows_rebuilt_last_frame();
        }
        terminal.clear_selection();
        report("selection_drag", samples);
        println!(
            "selection_drag rows_rebuilt_per_frame={}",
            rows_rebuilt / 1000
        );
    }

    if on("pixels") {
        let mut digest = 0u64;
        let scripts: [&[u8]; 6] = [
            "\x1b[2J\x1b[Hplain ascii\r\n\x1b[41;33mred bg\x1b[K\x1b[0m\r\n\x1b[7minverse\x1b[0m \x1b[1;3mbold italic\x1b[0m\r\n".as_bytes(),
            "\x1b[4munder\x1b[0m \x1b[9mstrike\x1b[0m \x1b[2mfaint\x1b[0m \x1b[8mhidden\x1b[0m\r\n日本語 한글 e\u{301} 🎉 ☕\u{fe0f} 1\u{fe0f}\u{20e3}\r\n".as_bytes(),
            "\x1b]8;;http://x\x1b\\link\x1b]8;;\x1b\\ \x1b[38;2;10;200;30mtruecolor\x1b[0m\r\n─│┌┐└┘ ▀▄█░\r\n".as_bytes(),
            b"\x1b[20;190H right edge text overflows\r\n\x1b[5 q",
            b"\x1b[30;5H\x1b[44m\x1b[K\x1b[0m\x1b[31;5Hmoved",
            b"\x1b[10;10H\x1b[2 q\x1b[35;1H != => -> == ",
        ];
        for script in scripts {
            for selected in [false, true] {
                stream.feed(script, &mut terminal);
                if selected {
                    terminal.set_selection(SelectionPoint::new(2, 1), SelectionPoint::new(30, 3));
                } else {
                    terminal.clear_selection();
                }
                let snap = FrameSnapshot::from_terminal(&mut terminal);
                renderer.rebuild_cells(&snap, &mut font, &theme);
                renderer.sync_atlas(&device, &queue, &mut font);
                renderer.draw(&device, &queue, &view);
                drain();
                digest = digest
                    .rotate_left(7)
                    .wrapping_add(pixel_digest(&device, &queue, &target));
            }
        }
        println!("pixels digest={digest:016x}");
    }
}
