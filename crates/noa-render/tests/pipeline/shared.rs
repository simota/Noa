use noa_core::{CellAttrs, Color};
use noa_font::FontGrid;
use noa_grid::{Cell, Cursor, Row, SearchState, TerminalColors};
use noa_render::{FrameSnapshot, ImagePlacementSnapshot, Renderer, SnapshotImage, Theme};
use std::sync::Arc;

/// Shared title-bar band height + card color for the overview headless tests
/// (mirrors `noa_app::session_overview`'s compile-time constants; noa-render can't
/// depend on noa-app, so the tests re-state them).
pub(crate) const TEST_TITLE_BAR_H: u32 = 30;
pub(crate) const TEST_CARD_COLOR: [f32; 4] = [0.078, 0.091, 0.127, 1.0];

/// Acquire a real device+queue, or `None` when no adapter exists (skip).
pub(crate) fn device_queue() -> Option<(wgpu::Device, wgpu::Queue)> {
    let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor::default());
    let adapter =
        pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
            .ok()?;
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("noa-test-device"),
        required_features: wgpu::Features::empty(),
        required_limits: wgpu::Limits::default(),
        experimental_features: wgpu::ExperimentalFeatures::default(),
        memory_hints: wgpu::MemoryHints::default(),
        trace: wgpu::Trace::Off,
    }))
    .ok()?;
    Some((device, queue))
}

pub(crate) fn snapshot_for_text(text: &str) -> FrameSnapshot {
    let mut cells = text
        .chars()
        .map(|ch| Cell {
            ch,
            combining: String::new(),
            fg: Color::Default,
            bg: Color::Default,
            underline_color: None,
            hyperlink: None,
            attrs: CellAttrs::empty(),
        })
        .collect::<Vec<_>>();
    if cells.is_empty() {
        cells.push(Cell::default());
    }

    let cols = cells.len().min(u16::MAX as usize) as u16;
    FrameSnapshot {
        scroll_shift: 0,
        rows: vec![Row {
            cells,
            wrapped: false,
            dirty: true,
        }],
        row_dirty: vec![true],
        cursor: Cursor::default(),
        colors: TerminalColors::default(),
        selection: None,
        search: SearchState::default(),
        row_base: 0,
        abs_row_base: 0,
        active_is_alt: false,
        cols,
        rows_n: 1,
        focused: true,
        cursor_blink_visible: true,
        hover_link: None,
        search_prompt: None,
        command_palette: None,
        confirm_dialog: None,
        preedit: None,
        image_placements: Vec::new(),
        images: Vec::new(),
    }
}

pub(crate) fn render_target(
    device: &wgpu::Device,
    width: u32,
    height: u32,
) -> (wgpu::Texture, wgpu::TextureView) {
    let target = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("noa-test-target"),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Bgra8UnormSrgb,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT
            | wgpu::TextureUsages::COPY_SRC
            | wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    });
    let view = target.create_view(&wgpu::TextureViewDescriptor::default());
    (target, view)
}

/// Read back a `Bgra8UnormSrgb` render target as RGBA8 bytes (`width *
/// height * 4`, row-major, B/R already swapped to RGBA order).
pub(crate) fn read_rgba_pixels(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
    width: u32,
    height: u32,
) -> Vec<u8> {
    let unpadded_bytes_per_row = width * 4;
    let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let padded_bytes_per_row = unpadded_bytes_per_row.div_ceil(align) * align;

    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("noa-test-readback-buffer"),
        size: (padded_bytes_per_row * height) as wgpu::BufferAddress,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });

    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("noa-test-readback-encoder"),
    });
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded_bytes_per_row),
                rows_per_image: Some(height),
            },
        },
        wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
    );
    queue.submit(Some(encoder.finish()));

    let slice = buffer.slice(..);
    let (tx, rx) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |result| {
        let _ = tx.send(result);
    });
    device
        .poll(wgpu::PollType::wait_indefinitely())
        .expect("poll device for map_async");
    rx.recv()
        .expect("map_async callback never fired")
        .expect("map readback buffer");

    let data = slice.get_mapped_range();
    let mut out = Vec::with_capacity((width * height * 4) as usize);
    for row in 0..height as usize {
        let start = row * padded_bytes_per_row as usize;
        let row_bytes = &data[start..start + (width * 4) as usize];
        // Bgra8UnormSrgb texture: swap B/R so callers get RGBA order.
        for px in row_bytes.chunks_exact(4) {
            out.push(px[2]);
            out.push(px[1]);
            out.push(px[0]);
            out.push(px[3]);
        }
    }
    drop(data);
    buffer.unmap();
    out
}

pub(crate) fn hash_pixels(bytes: &[u8]) -> u64 {
    use std::hash::{Hash, Hasher};

    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut hasher);
    hasher.finish()
}

pub(crate) fn rebuild_text_frame(
    renderer: &mut Renderer,
    font: &mut FontGrid,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    text: &str,
) {
    let snap = snapshot_for_text(text);
    renderer.rebuild_cells(&snap, font, &Theme::new());
    renderer.sync_atlas(device, queue, font);
}

/// A grid of `cols`×`rows` cells with an explicit background color, plus one
/// image placement (id 1, solid `image_color`) covering the whole grid at
/// z-index `z`.
pub(crate) fn image_snapshot(
    cols: u16,
    rows_n: u16,
    bg: Color,
    image_color: [u8; 4],
    epoch: u64,
    z: i32,
) -> FrameSnapshot {
    let rows: Vec<Row> = (0..rows_n)
        .map(|_| Row {
            cells: vec![
                Cell {
                    ch: ' ',
                    combining: String::new(),
                    fg: Color::Default,
                    bg,
                    underline_color: None,
                    hyperlink: None,
                    attrs: CellAttrs::empty(),
                };
                cols as usize
            ],
            wrapped: false,
            dirty: true,
        })
        .collect();
    // 4×4 solid-color image; the Linear sampler stretches it over the quad.
    let rgba: Vec<u8> = image_color
        .iter()
        .copied()
        .cycle()
        .take(4 * 4 * 4)
        .collect();
    FrameSnapshot {
        scroll_shift: 0,
        row_dirty: vec![true; rows.len()],
        rows,
        cursor: Cursor::default(),
        colors: TerminalColors::default(),
        selection: None,
        search: SearchState::default(),
        row_base: 0,
        abs_row_base: 0,
        active_is_alt: false,
        cols,
        rows_n,
        focused: true,
        cursor_blink_visible: true,
        hover_link: None,
        search_prompt: None,
        command_palette: None,
        confirm_dialog: None,
        preedit: None,
        image_placements: vec![ImagePlacementSnapshot {
            image_id: 1,
            epoch,
            grid_x: 0,
            grid_y: 0,
            cell_x_off: 0,
            cell_y_off: 0,
            cols,
            rows: rows_n,
            src: None,
            z,
        }],
        images: vec![SnapshotImage {
            id: 1,
            epoch,
            width: 4,
            height: 4,
            rgba: Arc::from(rgba),
        }],
    }
}
