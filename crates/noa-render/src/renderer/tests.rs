//! Split out of the former monolithic `renderer.rs` — unit tests.

use super::*;
use noa_core::{Color, GridSize, Rgb};
use noa_font::{FontConfig, ShapeCell, StyleKey};
use noa_grid::{Cell, Cursor, SearchMatch, SelectionPoint, Terminal};
use noa_vt::Stream;

use crate::segment::CellRenderInfo;

mod atlas;
mod cache;
mod cell;
mod color;
mod cursor;
mod overlay;
mod shape;

fn skip_font() -> Option<FontGrid> {
    match FontGrid::new(14.0, FontConfig::default()) {
        Ok(font) => Some(font),
        Err(err) => {
            eprintln!("skipping: no system monospace font available: {err}");
            None
        }
    }
}

/// Acquire a real device+queue, or `None` when no adapter exists (skip).
/// Mirrors `noa-render/tests/pipeline.rs`'s headless-GPU skip pattern.
fn device_queue() -> Option<(wgpu::Device, wgpu::Queue)> {
    let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor::default());
    let adapter =
        pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
            .ok()?;
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("noa-renderer-test-device"),
        required_features: wgpu::Features::empty(),
        required_limits: wgpu::Limits::default(),
        experimental_features: wgpu::ExperimentalFeatures::default(),
        memory_hints: wgpu::MemoryHints::default(),
        trace: wgpu::Trace::Off,
    }))
    .ok()?;
    Some((device, queue))
}

fn metrics(ascent: f32) -> Metrics {
    Metrics {
        cell_w: 10.0,
        cell_h: 24.0,
        ascent,
        descent: 6.0,
        line_gap: 0.0,
        underline_position: 0.0,
        underline_thickness: 1.0,
    }
}

fn font_with_rasterized_m() -> Option<FontGrid> {
    let mut font = match FontGrid::new(14.0, FontConfig::default()) {
        Ok(font) => font,
        Err(err) => {
            eprintln!("skipping: no system monospace font available: {err}");
            return None;
        }
    };
    if font.get_or_raster('M').atlas_size == [0, 0] {
        eprintln!("skipping: installed monospace font did not rasterize 'M'");
        return None;
    }
    Some(font)
}

fn rgb_from_instance(instance: &CellInstance) -> Rgb {
    Rgb::new(instance.color[0], instance.color[1], instance.color[2])
}

fn one_cell_terminal_with_cursor_style(style: CursorStyle) -> Terminal {
    let mut terminal = Terminal::new(GridSize::new(1, 1));
    terminal.primary.cursor.x = 0;
    terminal.primary.cursor.y = 0;
    terminal.primary.cursor.style = style;
    terminal.primary.grid[0].cells[0].ch = 'M';
    terminal.primary.grid[0].cells[0].fg = Color::Rgb(Rgb::new(240, 10, 20));
    terminal.primary.grid[0].cells[0].bg = Color::Rgb(Rgb::new(2, 3, 4));
    terminal
}

fn baseline_snapshot(chars: [char; 3]) -> FrameSnapshot {
    let rows = chars
        .into_iter()
        .map(|ch| Row {
            cells: vec![Cell {
                ch,
                ..Cell::default()
            }],
            wrapped: false,
            dirty: false,
        })
        .collect();
    FrameSnapshot {
        scroll_shift: 0,
        rows,
        row_dirty: vec![false, false, false],
        cursor: Cursor::default(),
        colors: TerminalColors::default(),
        selection: None,
        search: SearchState::default(),
        row_base: 0,
        abs_row_base: 0,
        active_is_alt: false,
        cols: 1,
        rows_n: 3,
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
