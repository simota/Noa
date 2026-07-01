//! The winit [`ApplicationHandler`] — owns the window, the wgpu surface, the
//! renderer, the shared `Terminal`, and drives input + redraw + resize.
//!
//! Simplified inc-1 thread model: all rendering + presentation happens on
//! the winit main thread (macOS requires presenting on the thread that owns
//! the window). The io thread only touches the `Terminal` mutex and the pty.

use std::sync::{Arc, Mutex};

use crossbeam_channel::Sender;
use noa_core::{GridSize, PixelSize};
use noa_font::FontGrid;
use noa_grid::Terminal;
use noa_pty::{Pty, PtyConfig, PtyWriter};
use noa_render::{FrameSnapshot, Renderer, Theme};
use winit::application::ApplicationHandler;
use winit::event::{ElementState, WindowEvent};
use winit::event_loop::{ActiveEventLoop, EventLoopProxy};
use winit::keyboard::{Key, ModifiersState};
use winit::window::{Window, WindowAttributes, WindowId};

use crate::events::UserEvent;
use crate::input;

/// Configuration the binary passes into [`crate::run`].
#[derive(Debug, Clone)]
pub struct AppConfig {
    pub cols: u16,
    pub rows: u16,
    pub font_size: f32,
}

/// GPU + window state that only exists once `resumed()` has run.
struct GraphicsState {
    window: Arc<Window>,
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    surface_config: wgpu::SurfaceConfiguration,
    renderer: Renderer,
    font: FontGrid,
    theme: Theme,
}

pub struct App {
    config: AppConfig,
    proxy: EventLoopProxy<UserEvent>,
    graphics: Option<GraphicsState>,
    terminal: Option<Arc<Mutex<Terminal>>>,
    pty_writer: Option<PtyWriter>,
    resize_tx: Option<Sender<GridSize>>,
    io_thread: Option<std::thread::JoinHandle<()>>,
    modifiers: ModifiersState,
    grid_size: GridSize,
}

impl App {
    pub fn new(config: AppConfig, proxy: EventLoopProxy<UserEvent>) -> Self {
        let grid_size = GridSize::new(config.cols, config.rows);
        App {
            config,
            proxy,
            graphics: None,
            terminal: None,
            pty_writer: None,
            resize_tx: None,
            io_thread: None,
            modifiers: ModifiersState::empty(),
            grid_size,
        }
    }

    fn app_cursor_keys(&self) -> bool {
        self.terminal
            .as_ref()
            .map(|t| t.lock().expect("terminal mutex poisoned").modes.app_cursor_keys())
            .unwrap_or(false)
    }

    fn redraw(&mut self) {
        let (Some(graphics), Some(terminal)) = (self.graphics.as_mut(), self.terminal.as_ref())
        else {
            return;
        };

        let snapshot = {
            let term = terminal.lock().expect("terminal mutex poisoned");
            FrameSnapshot::from_terminal(&term)
        };

        graphics
            .renderer
            .rebuild_cells(&snapshot, &mut graphics.font, &graphics.theme);
        graphics
            .renderer
            .sync_atlas(&graphics.device, &graphics.queue, &mut graphics.font);

        let frame = match graphics.surface.get_current_texture() {
            Ok(frame) => frame,
            Err(wgpu::SurfaceError::Lost | wgpu::SurfaceError::Outdated) => {
                graphics.surface.configure(&graphics.device, &graphics.surface_config);
                return;
            }
            Err(e) => {
                log::warn!("surface error: {e}");
                return;
            }
        };
        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());

        graphics.renderer.draw(&graphics.device, &graphics.queue, &view);
        frame.present();
    }
}

impl ApplicationHandler<UserEvent> for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.graphics.is_some() {
            return; // already initialized (e.g. redundant Resumed on macOS)
        }

        // Build the font grid first so we know the cell size to size the window.
        let mut font = FontGrid::new(self.config.font_size).expect("failed to load a system monospace font");
        let metrics = font.metrics();
        let px_w = (metrics.cell_w * self.grid_size.cols as f32).ceil().max(1.0) as u32;
        let px_h = (metrics.cell_h * self.grid_size.rows as f32).ceil().max(1.0) as u32;

        let window_attrs = WindowAttributes::default()
            .with_title("noa")
            .with_inner_size(winit::dpi::PhysicalSize::new(px_w, px_h));
        let window = Arc::new(
            event_loop
                .create_window(window_attrs)
                .expect("failed to create window"),
        );

        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor::default());
        let surface = instance
            .create_surface(window.clone())
            .expect("failed to create wgpu surface");

        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::default(),
            compatible_surface: Some(&surface),
            force_fallback_adapter: false,
        }))
        .expect("failed to find a compatible wgpu adapter");

        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("noa-device"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::default(),
            experimental_features: wgpu::ExperimentalFeatures::default(),
            memory_hints: wgpu::MemoryHints::default(),
            trace: wgpu::Trace::Off,
        }))
        .expect("failed to request a wgpu device");

        let caps = surface.get_capabilities(&adapter);
        let surface_format = caps
            .formats
            .iter()
            .copied()
            .find(|f| *f == wgpu::TextureFormat::Bgra8UnormSrgb)
            .unwrap_or(caps.formats[0]);

        let size = window.inner_size();
        let surface_config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format: surface_format,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode: wgpu::PresentMode::Fifo,
            desired_maximum_frame_latency: 2,
            alpha_mode: caps.alpha_modes[0],
            view_formats: vec![],
        };
        surface.configure(&device, &surface_config);

        let mut renderer = Renderer::new(&device, &queue, surface_format, &mut font)
            .expect("failed to build the renderer");
        renderer.resize(PixelSize {
            w: surface_config.width,
            h: surface_config.height,
        });

        // Spawn the pty + shared terminal + io thread.
        let pty_config = PtyConfig {
            size: self.grid_size,
            ..Default::default()
        };
        let pty = Pty::spawn(pty_config).expect("failed to spawn pty");
        let pty_writer = pty.writer();
        let terminal = Arc::new(Mutex::new(Terminal::new(self.grid_size)));
        let (resize_tx, resize_rx) = crossbeam_channel::unbounded();

        let io_thread = crate::io_thread::spawn(
            pty,
            pty_writer.clone(),
            terminal.clone(),
            self.proxy.clone(),
            resize_rx,
        );

        self.graphics = Some(GraphicsState {
            window,
            surface,
            device,
            queue,
            surface_config,
            renderer,
            font,
            theme: crate::theme::default_theme(),
        });
        self.terminal = Some(terminal);
        self.pty_writer = Some(pty_writer);
        self.resize_tx = Some(resize_tx);
        self.io_thread = Some(io_thread);
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: UserEvent) {
        match event {
            UserEvent::Redraw => {
                if let Some(g) = &self.graphics {
                    g.window.request_redraw();
                }
            }
            UserEvent::PtyExit => event_loop.exit(),
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, window_id: WindowId, event: WindowEvent) {
        let Some(graphics) = self.graphics.as_ref() else {
            return;
        };
        if graphics.window.id() != window_id {
            return;
        }

        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::RedrawRequested => self.redraw(),
            WindowEvent::Resized(size) => self.on_resize(size),
            WindowEvent::ModifiersChanged(mods) => self.modifiers = mods.state(),
            WindowEvent::KeyboardInput { event, .. } => {
                if event.state != ElementState::Pressed {
                    return;
                }
                // Cmd-based combos are macOS app shortcuts, not shell input:
                // handle Cmd+Q / Cmd+W here and never forward a Cmd combo to
                // the pty (the default menu bar also binds Cmd+Q).
                if self.modifiers.super_key() {
                    if let Key::Character(c) = &event.logical_key
                        && matches!(c.as_str(), "q" | "w")
                    {
                        event_loop.exit();
                    }
                    return;
                }
                let app_cursor_keys = self.app_cursor_keys();
                let bytes = input::encode_key(
                    &event.logical_key,
                    event.text.as_deref(),
                    self.modifiers,
                    app_cursor_keys,
                );
                if let (Some(bytes), Some(writer)) = (bytes, self.pty_writer.as_ref()) {
                    let _ = writer.write(&bytes);
                    let _ = writer.flush();
                }
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {}
}

impl App {
    fn on_resize(&mut self, size: winit::dpi::PhysicalSize<u32>) {
        let Some(graphics) = self.graphics.as_mut() else {
            return;
        };
        if size.width == 0 || size.height == 0 {
            return;
        }
        graphics.surface_config.width = size.width;
        graphics.surface_config.height = size.height;
        graphics.surface.configure(&graphics.device, &graphics.surface_config);
        graphics.renderer.resize(PixelSize {
            w: size.width,
            h: size.height,
        });

        let metrics = graphics.font.metrics();
        let cols = (size.width as f32 / metrics.cell_w).floor().max(1.0) as u16;
        let rows = (size.height as f32 / metrics.cell_h).floor().max(1.0) as u16;
        self.grid_size = GridSize::new(cols, rows);
        // Grid-first ordering: resize the shared Terminal grid BEFORE telling
        // the pty its new winsize. Otherwise the shell's SIGWINCH repaint (at
        // the new size) can reach the io thread and be fed into a still-old
        // grid, clamping it into the wrong cells until the next frame. This is
        // a grid resize, not soft-wrap reflow (re-wrapping lands in inc≥3).
        if let Some(terminal) = &self.terminal {
            terminal
                .lock()
                .expect("terminal mutex poisoned")
                .resize(self.grid_size);
        }
        // Then the pty winsize (the io thread owns the Pty).
        if let Some(resize_tx) = &self.resize_tx {
            let _ = resize_tx.send(self.grid_size);
        }
        if let Some(g) = &self.graphics {
            g.window.request_redraw();
        }
    }
}
