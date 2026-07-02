//! The winit [`ApplicationHandler`] — owns the window, the wgpu surface, the
//! renderer, the shared `Terminal`, and drives input + redraw + resize.
//!
//! Simplified inc-1 thread model: all rendering + presentation happens on
//! the winit main thread (macOS requires presenting on the thread that owns
//! the window). The io thread only touches the `Terminal` mutex and the pty.

use std::sync::{Arc, Mutex};
use std::time::Instant;

use crossbeam_channel::{Sender, TrySendError};
use noa_core::{GridSize, PixelSize, Point};
use noa_font::FontGrid;
use noa_grid::{Terminal, modes::MouseTracking};
use noa_pty::{Pty, PtyConfig};
use noa_render::{FrameSnapshot, Renderer, Theme};
use winit::application::ApplicationHandler;
use winit::dpi::{LogicalSize, PhysicalPosition, PhysicalSize};
use winit::event::{ElementState, Ime, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, EventLoopProxy};
use winit::keyboard::ModifiersState;
use winit::window::{Window, WindowAttributes, WindowId};

use crate::clipboard::SystemClipboard;
use crate::commands::{KeybindEngine, SearchAction};
use crate::events::UserEvent;
use crate::input;
use crate::mouse::{self, MouseSelectionState, SelectionGesture};
use crate::{AppCommand, ViewportScroll};

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
    pty_input_tx: Option<Sender<crate::io_thread::PtyInput>>,
    resize_tx: Option<Sender<GridSize>>,
    io_thread: Option<std::thread::JoinHandle<()>>,
    #[cfg(target_os = "macos")]
    macos_menu: Option<crate::macos_menu::MacosMenu>,
    modifiers: ModifiersState,
    grid_size: GridSize,
    mouse_selection: MouseSelectionState,
    last_mouse_cell: Option<Point>,
    pressed_mouse_button: Option<MouseButton>,
    clipboard: SystemClipboard,
    keybinds: KeybindEngine,
    ime_state: input::ImeState,
}

impl App {
    pub fn new(config: AppConfig, proxy: EventLoopProxy<UserEvent>) -> Self {
        let grid_size = GridSize::new(config.cols, config.rows);
        App {
            config,
            proxy,
            graphics: None,
            terminal: None,
            pty_input_tx: None,
            resize_tx: None,
            io_thread: None,
            #[cfg(target_os = "macos")]
            macos_menu: None,
            modifiers: ModifiersState::empty(),
            grid_size,
            mouse_selection: MouseSelectionState::default(),
            last_mouse_cell: None,
            pressed_mouse_button: None,
            clipboard: SystemClipboard::new(),
            keybinds: KeybindEngine::default(),
            ime_state: input::ImeState::default(),
        }
    }

    fn app_cursor_keys(&self) -> bool {
        self.terminal
            .as_ref()
            .map(|t| {
                t.lock()
                    .expect("terminal mutex poisoned")
                    .modes
                    .app_cursor_keys()
            })
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
        update_ime_cursor_area(
            &graphics.window,
            graphics.font.metrics(),
            snapshot.cursor.x,
            snapshot.cursor.y,
        );

        graphics
            .renderer
            .rebuild_cells(&snapshot, &mut graphics.font, &graphics.theme);
        graphics
            .renderer
            .sync_atlas(&graphics.device, &graphics.queue, &mut graphics.font);

        let frame = match graphics.surface.get_current_texture() {
            Ok(frame) => frame,
            Err(wgpu::SurfaceError::Lost | wgpu::SurfaceError::Outdated) => {
                graphics
                    .surface
                    .configure(&graphics.device, &graphics.surface_config);
                graphics.window.request_redraw();
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

        graphics
            .renderer
            .draw(&graphics.device, &graphics.queue, &view);
        frame.present();
    }

    fn handle_app_command(&mut self, event_loop: &ActiveEventLoop, command: AppCommand) {
        match command {
            AppCommand::About => {
                log::info!("About noa selected");
            }
            AppCommand::Preferences => {
                log::debug!("Preferences selected before settings support exists");
            }
            AppCommand::Copy => self.copy_selection_to_clipboard(),
            AppCommand::Paste => self.paste_clipboard_to_pty(),
            AppCommand::Search(action) => self.handle_search_action(action),
            AppCommand::ScrollViewport(scroll) => self.scroll_viewport(scroll),
            AppCommand::CloseWindow | AppCommand::Quit => event_loop.exit(),
        }
    }

    #[cfg(target_os = "macos")]
    fn install_macos_menu_if_needed(&mut self) {
        if self.macos_menu.is_none() {
            self.macos_menu = Some(
                crate::macos_menu::MacosMenu::install(self.proxy.clone())
                    .expect("failed to install macOS app menu"),
            );
        }
    }
}

impl ApplicationHandler<UserEvent> for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.graphics.is_some() {
            return; // already initialized (e.g. redundant Resumed on macOS)
        }

        // Build the font grid first so we know the cell size to size the window.
        // `font_size` is a logical point size; the renderer consumes physical pixels.
        let monitor_scale_factor = event_loop
            .primary_monitor()
            .map(|monitor| monitor.scale_factor())
            .unwrap_or(1.0);
        let mut font = FontGrid::new(font_pixel_size(self.config.font_size, monitor_scale_factor))
            .expect("failed to load a system monospace font");
        let metrics = font.metrics();
        let inner_size = initial_window_logical_size(metrics, self.grid_size, monitor_scale_factor);

        let window_attrs = WindowAttributes::default()
            .with_title("noa")
            .with_inner_size(inner_size);
        let window = Arc::new(
            event_loop
                .create_window(window_attrs)
                .expect("failed to create window"),
        );
        let window_scale_factor = window.scale_factor();
        if (window_scale_factor - monitor_scale_factor).abs() > f64::EPSILON {
            font = FontGrid::new(font_pixel_size(self.config.font_size, window_scale_factor))
                .expect("failed to load a system monospace font");
            let inner_size =
                initial_window_logical_size(font.metrics(), self.grid_size, window_scale_factor);
            let _ = window.request_inner_size(inner_size);
        }
        window.set_ime_allowed(true);
        update_ime_cursor_area(&window, font.metrics(), 0, 0);

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
        let terminal = Arc::new(Mutex::new(Terminal::new(self.grid_size)));
        let (resize_tx, resize_rx) = crossbeam_channel::unbounded();
        let (pty_input_tx, pty_input_rx) = crate::io_thread::input_channel();

        let io_thread = crate::io_thread::spawn(
            pty,
            terminal.clone(),
            self.proxy.clone(),
            resize_rx,
            pty_input_rx,
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
        self.pty_input_tx = Some(pty_input_tx);
        self.resize_tx = Some(resize_tx);
        self.io_thread = Some(io_thread);
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: UserEvent) {
        match event {
            UserEvent::AppCommand(command) => self.handle_app_command(event_loop, command),
            UserEvent::ClipboardWrite(text) => {
                if let Err(err) = self.clipboard.set_text(&text) {
                    log::warn!("failed to write OSC 52 clipboard text: {err}");
                }
            }
            UserEvent::Redraw => {
                if let Some(g) = &self.graphics {
                    g.window.request_redraw();
                }
            }
            UserEvent::PtyExit => event_loop.exit(),
        }
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: WindowEvent,
    ) {
        let Some(graphics) = self.graphics.as_ref() else {
            return;
        };
        if graphics.window.id() != window_id {
            return;
        }

        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::RedrawRequested => self.redraw(),
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                self.on_scale_factor_changed(scale_factor)
            }
            WindowEvent::Resized(size) => self.on_resize(size),
            WindowEvent::ModifiersChanged(mods) => self.modifiers = mods.state(),
            WindowEvent::CursorMoved { position, .. } => self.on_cursor_moved(position),
            WindowEvent::MouseInput { state, button, .. } => self.on_mouse_input(state, button),
            WindowEvent::MouseWheel { delta, .. } => self.on_mouse_wheel(delta),
            WindowEvent::Ime(event) => self.on_ime_event(event),
            WindowEvent::KeyboardInput { event, .. } => {
                if event.state != ElementState::Pressed {
                    return;
                }
                if self.ime_state.preedit_active() {
                    return;
                }
                if let Some(command) = self.keybinds.resolve(&event.logical_key, self.modifiers) {
                    self.handle_app_command(event_loop, command);
                    return;
                }
                // Cmd-based combos are app shortcuts, not shell input. Unknown
                // Cmd combos remain swallowed to match the previous behavior.
                if self.modifiers.super_key() {
                    return;
                }
                let app_cursor_keys = self.app_cursor_keys();
                let bytes = input::encode_key(
                    &event.logical_key,
                    event.text.as_deref(),
                    self.modifiers,
                    app_cursor_keys,
                );
                if let Some(bytes) = bytes {
                    self.write_pty_bytes(&bytes);
                }
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        #[cfg(target_os = "macos")]
        self.install_macos_menu_if_needed();
    }
}

impl App {
    fn on_scale_factor_changed(&mut self, scale_factor: f64) {
        let (grid_size, window) = {
            let Some(graphics) = self.graphics.as_mut() else {
                return;
            };
            match FontGrid::new(font_pixel_size(self.config.font_size, scale_factor)) {
                Ok(font) => graphics.font = font,
                Err(err) => {
                    log::warn!("failed to rebuild font for scale factor {scale_factor}: {err}");
                }
            }
            (
                grid_size_for_physical_size(graphics.window.inner_size(), graphics.font.metrics()),
                graphics.window.clone(),
            )
        };

        self.resize_grid(grid_size);
        window.request_redraw();
    }

    fn on_resize(&mut self, size: winit::dpi::PhysicalSize<u32>) {
        let (grid_size, window) = {
            let Some(graphics) = self.graphics.as_mut() else {
                return;
            };
            if size.width == 0 || size.height == 0 {
                return;
            }
            graphics.surface_config.width = size.width;
            graphics.surface_config.height = size.height;
            graphics
                .surface
                .configure(&graphics.device, &graphics.surface_config);
            graphics.renderer.resize(PixelSize {
                w: size.width,
                h: size.height,
            });
            (
                grid_size_for_physical_size(size, graphics.font.metrics()),
                graphics.window.clone(),
            )
        };

        self.resize_grid(grid_size);
        window.request_redraw();
    }

    fn resize_grid(&mut self, grid_size: GridSize) {
        self.grid_size = grid_size;
        // Grid-first ordering: resize the shared Terminal grid BEFORE telling
        // the pty its new winsize. Otherwise the shell's SIGWINCH repaint (at
        // the new size) can reach the io thread and be fed into a still-old
        // grid, clamping it into the wrong cells until the next frame.
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
    }

    fn on_cursor_moved(&mut self, position: PhysicalPosition<f64>) {
        let Some(graphics) = self.graphics.as_ref() else {
            return;
        };
        let metrics = graphics.font.metrics();
        let cell = mouse::physical_position_to_grid_point(
            position.x,
            position.y,
            metrics.cell_w,
            metrics.cell_h,
            self.grid_size,
        );
        self.last_mouse_cell = Some(cell);

        let tracking = self.sgr_mouse_tracking();
        if tracking != MouseTracking::Off && !self.modifiers.shift_key() {
            if let Some(bytes) = mouse::encode_sgr_mouse_motion(
                tracking,
                self.pressed_mouse_button,
                cell,
                self.modifiers,
            ) {
                self.write_pty_bytes(&bytes);
            }
            return;
        }

        let gesture = self.mouse_selection.cursor_moved(cell);
        self.apply_selection_gesture(gesture);
    }

    fn on_mouse_input(&mut self, state: ElementState, button: MouseButton) {
        let tracking = self.sgr_mouse_tracking();
        if tracking != MouseTracking::Off && !self.modifiers.shift_key() {
            if let Some(cell) = self.last_mouse_cell
                && let Some(bytes) =
                    mouse::encode_sgr_mouse_input(button, state, cell, self.modifiers)
            {
                self.write_pty_bytes(&bytes);
            }

            match state {
                ElementState::Pressed => self.pressed_mouse_button = Some(button),
                ElementState::Released => {
                    if self.pressed_mouse_button == Some(button) {
                        self.pressed_mouse_button = None;
                    }
                }
            }
            return;
        }

        if button != MouseButton::Left {
            return;
        }
        if let Some(cell) = self.last_mouse_cell {
            let _ = self.mouse_selection.cursor_moved(cell);
        }

        let gesture = match state {
            ElementState::Pressed => self.mouse_selection.left_pressed(Instant::now()),
            ElementState::Released => self.mouse_selection.left_released(),
        };
        self.apply_selection_gesture(gesture);
    }

    fn on_mouse_wheel(&mut self, delta: MouseScrollDelta) {
        if self.sgr_mouse_tracking() != MouseTracking::Off && !self.modifiers.shift_key() {
            let Some(cell) = self.last_mouse_cell else {
                return;
            };
            let delta_y = match delta {
                MouseScrollDelta::LineDelta(_, y) => y,
                MouseScrollDelta::PixelDelta(position) => position.y as f32,
            };
            if let Some(bytes) = mouse::encode_sgr_mouse_wheel(delta_y, cell, self.modifiers) {
                self.write_pty_bytes(&bytes);
            }
            return;
        }

        let cell_h = self
            .graphics
            .as_ref()
            .map(|graphics| graphics.font.metrics().cell_h)
            .unwrap_or(1.0);
        if let Some(scroll) = mouse_wheel_viewport_scroll(delta, cell_h) {
            self.scroll_mouse_wheel_viewport(scroll);
        }
    }

    fn on_ime_event(&mut self, event: Ime) {
        if let Some(bytes) = self.ime_state.handle_event(&event) {
            self.write_pty_bytes(&bytes);
        }
    }

    fn scroll_viewport(&mut self, scroll: ViewportScroll) {
        let Some(terminal) = &self.terminal else {
            return;
        };

        apply_viewport_scroll(
            &mut terminal.lock().expect("terminal mutex poisoned"),
            self.grid_size,
            scroll,
        );

        if let Some(graphics) = &self.graphics {
            graphics.window.request_redraw();
        }
    }

    fn scroll_mouse_wheel_viewport(&mut self, scroll: MouseWheelViewportScroll) {
        let Some(terminal) = &self.terminal else {
            return;
        };

        apply_mouse_wheel_viewport_scroll(
            &mut terminal.lock().expect("terminal mutex poisoned"),
            scroll,
        );

        if let Some(graphics) = &self.graphics {
            graphics.window.request_redraw();
        }
    }

    fn handle_search_action(&mut self, action: SearchAction) {
        let Some(terminal) = &self.terminal else {
            return;
        };

        let mut terminal = terminal.lock().expect("terminal mutex poisoned");
        match action {
            SearchAction::Find => {
                log::debug!("search UI command selected before search prompt support exists");
                return;
            }
            SearchAction::FindNext => {
                terminal.search_next();
            }
            SearchAction::FindPrevious => {
                terminal.search_previous();
            }
            SearchAction::Clear => terminal.clear_search(),
        }
        drop(terminal);

        if let Some(graphics) = &self.graphics {
            graphics.window.request_redraw();
        }
    }

    fn apply_selection_gesture(&mut self, gesture: SelectionGesture) {
        if gesture == SelectionGesture::None {
            return;
        }

        if let Some(terminal) = &self.terminal {
            let mut terminal = terminal.lock().expect("terminal mutex poisoned");
            match gesture {
                SelectionGesture::None => {}
                SelectionGesture::Clear => terminal.clear_selection(),
                SelectionGesture::Extend { anchor, focus } => {
                    terminal.set_viewport_selection(anchor, focus);
                }
                SelectionGesture::SelectWord(point) => {
                    terminal.select_word_at_viewport_point(point)
                }
                SelectionGesture::SelectLine(point) => {
                    terminal.select_line_at_viewport_point(point)
                }
            }
        }

        if let Some(graphics) = &self.graphics {
            graphics.window.request_redraw();
        }
    }

    fn copy_selection_to_clipboard(&mut self) {
        let selected_text = self.terminal.as_ref().and_then(|terminal| {
            terminal
                .lock()
                .expect("terminal mutex poisoned")
                .selected_text()
        });
        let Some(selected_text) = selected_text else {
            return;
        };

        if let Err(err) = self.clipboard.set_text(&selected_text) {
            log::warn!("failed to copy selection to clipboard: {err}");
        }
    }

    fn paste_clipboard_to_pty(&mut self) {
        let text = match self.clipboard.get_text() {
            Ok(text) => text,
            Err(err) => {
                log::warn!("failed to read clipboard for paste: {err}");
                return;
            }
        };
        let bracketed_paste = self.bracketed_paste();
        if let Some(bytes) = input::encode_paste(&text, bracketed_paste) {
            self.write_pty_bytes(&bytes);
        }
    }

    fn bracketed_paste(&self) -> bool {
        self.terminal
            .as_ref()
            .map(|terminal| {
                terminal
                    .lock()
                    .expect("terminal mutex poisoned")
                    .modes
                    .bracketed_paste()
            })
            .unwrap_or(false)
    }

    fn sgr_mouse_tracking(&self) -> MouseTracking {
        self.terminal
            .as_ref()
            .map(|terminal| {
                let terminal = terminal.lock().expect("terminal mutex poisoned");
                if terminal.modes.sgr_mouse_reporting() {
                    terminal.modes.mouse_tracking()
                } else {
                    MouseTracking::Off
                }
            })
            .unwrap_or(MouseTracking::Off)
    }

    fn write_pty_bytes(&self, bytes: &[u8]) {
        let Some(input_tx) = self.pty_input_tx.as_ref() else {
            return;
        };
        match input_tx.try_send(bytes.to_vec().into_boxed_slice()) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => {
                log::warn!("dropping pty input because the io thread queue is full");
            }
            Err(TrySendError::Disconnected(_)) => {
                log::warn!("failed to queue pty input because the io thread is gone");
            }
        }
    }
}

fn apply_viewport_scroll(terminal: &mut Terminal, grid_size: GridSize, scroll: ViewportScroll) {
    let page_rows = usize::from(grid_size.rows.saturating_sub(1).max(1));
    match scroll {
        ViewportScroll::LineUp => terminal.scroll_viewport_up(1),
        ViewportScroll::LineDown => terminal.scroll_viewport_down(1),
        ViewportScroll::PageUp => terminal.scroll_viewport_up(page_rows),
        ViewportScroll::PageDown => terminal.scroll_viewport_down(page_rows),
        ViewportScroll::Top => terminal.scroll_viewport_to_top(),
        ViewportScroll::Bottom => terminal.scroll_viewport_to_bottom(),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MouseWheelViewportScroll {
    Up(usize),
    Down(usize),
}

fn mouse_wheel_viewport_scroll(
    delta: MouseScrollDelta,
    cell_height: f32,
) -> Option<MouseWheelViewportScroll> {
    let (delta_y, rows) = match delta {
        MouseScrollDelta::LineDelta(_, y) => (y, y.abs().ceil() as usize),
        MouseScrollDelta::PixelDelta(position) => {
            let y = position.y as f32;
            let rows = (y.abs() / cell_height.max(f32::EPSILON)).ceil() as usize;
            (y, rows)
        }
    };

    if !delta_y.is_finite() || delta_y == 0.0 || rows == 0 {
        return None;
    }

    if delta_y > 0.0 {
        Some(MouseWheelViewportScroll::Up(rows))
    } else {
        Some(MouseWheelViewportScroll::Down(rows))
    }
}

fn apply_mouse_wheel_viewport_scroll(terminal: &mut Terminal, scroll: MouseWheelViewportScroll) {
    match scroll {
        MouseWheelViewportScroll::Up(rows) => terminal.scroll_viewport_up(rows),
        MouseWheelViewportScroll::Down(rows) => terminal.scroll_viewport_down(rows),
    }
}

fn font_pixel_size(point_size: f32, scale_factor: f64) -> f32 {
    (point_size * scale_factor.max(f64::EPSILON) as f32).max(1.0)
}

fn initial_window_logical_size(
    metrics: noa_font::Metrics,
    grid_size: GridSize,
    scale_factor: f64,
) -> LogicalSize<f64> {
    let scale_factor = scale_factor.max(f64::EPSILON) as f32;
    let physical_w = (metrics.cell_w * grid_size.cols as f32).ceil().max(1.0);
    let physical_h = (metrics.cell_h * grid_size.rows as f32).ceil().max(1.0);

    LogicalSize::new(
        (physical_w / scale_factor) as f64,
        (physical_h / scale_factor) as f64,
    )
}

fn grid_size_for_physical_size(size: PhysicalSize<u32>, metrics: noa_font::Metrics) -> GridSize {
    let cols = (size.width as f32 / metrics.cell_w.max(f32::EPSILON))
        .floor()
        .clamp(1.0, u16::MAX as f32) as u16;
    let rows = (size.height as f32 / metrics.cell_h.max(f32::EPSILON))
        .floor()
        .clamp(1.0, u16::MAX as f32) as u16;
    GridSize::new(cols, rows)
}

fn update_ime_cursor_area(window: &Window, metrics: noa_font::Metrics, x: u16, y: u16) {
    let (position, size) = ime_cursor_area(metrics, x, y);
    window.set_ime_cursor_area(position, size);
}

fn ime_cursor_area(
    metrics: noa_font::Metrics,
    x: u16,
    y: u16,
) -> (PhysicalPosition<i32>, PhysicalSize<u32>) {
    let position = PhysicalPosition::new(
        (metrics.cell_w * x as f32).round().max(0.0) as i32,
        (metrics.cell_h * y as f32).round().max(0.0) as i32,
    );
    let size = PhysicalSize::new(
        metrics.cell_w.ceil().max(1.0) as u32,
        metrics.cell_h.ceil().max(1.0) as u32,
    );
    (position, size)
}

#[cfg(test)]
mod tests {
    use super::*;
    use noa_vt::Stream;

    fn metrics(cell_w: f32, cell_h: f32) -> noa_font::Metrics {
        noa_font::Metrics {
            cell_w,
            cell_h,
            ascent: cell_h * 0.75,
            descent: cell_h * 0.25,
            line_gap: 0.0,
            underline_position: 0.0,
            underline_thickness: 1.0,
        }
    }

    fn terminal_with_scrollback(grid_size: GridSize) -> Terminal {
        let mut terminal = Terminal::new(grid_size);
        let mut stream = Stream::new();
        stream.feed(b"A\r\nB\r\nC\r\nD\r\nE\r\nF", &mut terminal);
        terminal
    }

    #[test]
    fn font_pixel_size_scales_logical_points() {
        assert_eq!(font_pixel_size(14.0, 1.0), 14.0);
        assert_eq!(font_pixel_size(14.0, 2.0), 28.0);
    }

    #[test]
    fn initial_window_size_converts_physical_metrics_to_logical_size() {
        let size = initial_window_logical_size(metrics(16.0, 32.0), GridSize::new(80, 24), 2.0);

        assert_eq!(size.width, 640.0);
        assert_eq!(size.height, 384.0);
    }

    #[test]
    fn scale_factor_grid_recompute_uses_new_cell_metrics() {
        let size = PhysicalSize::new(960, 600);

        assert_eq!(
            grid_size_for_physical_size(size, metrics(12.0, 24.0)),
            GridSize::new(80, 25)
        );
        assert_eq!(
            grid_size_for_physical_size(size, metrics(16.0, 30.0)),
            GridSize::new(60, 20)
        );
        assert_eq!(
            grid_size_for_physical_size(PhysicalSize::new(1, 1), metrics(16.0, 30.0)),
            GridSize::new(1, 1)
        );
    }

    #[test]
    fn ime_cursor_area_tracks_grid_cell_in_physical_pixels() {
        let (position, size) = ime_cursor_area(metrics(7.5, 15.25), 2, 3);

        assert_eq!(position.x, 15);
        assert_eq!(position.y, 46);
        assert_eq!(size.width, 8);
        assert_eq!(size.height, 16);
    }

    #[test]
    fn viewport_scroll_commands_move_by_line_page_and_extremes() {
        let grid_size = GridSize::new(5, 3);
        let mut terminal = terminal_with_scrollback(grid_size);

        apply_viewport_scroll(&mut terminal, grid_size, ViewportScroll::LineUp);
        assert_eq!(terminal.viewport_offset(), 1);

        apply_viewport_scroll(&mut terminal, grid_size, ViewportScroll::PageUp);
        assert_eq!(terminal.viewport_offset(), 3);

        apply_viewport_scroll(&mut terminal, grid_size, ViewportScroll::LineDown);
        assert_eq!(terminal.viewport_offset(), 2);

        apply_viewport_scroll(&mut terminal, grid_size, ViewportScroll::PageDown);
        assert_eq!(terminal.viewport_offset(), 0);

        apply_viewport_scroll(&mut terminal, grid_size, ViewportScroll::Top);
        assert_eq!(terminal.viewport_offset(), terminal.scrollback_len());

        apply_viewport_scroll(&mut terminal, grid_size, ViewportScroll::Bottom);
        assert_eq!(terminal.viewport_offset(), 0);
    }

    #[test]
    fn mouse_wheel_delta_maps_to_viewport_scroll_rows() {
        assert_eq!(
            mouse_wheel_viewport_scroll(MouseScrollDelta::LineDelta(0.0, 2.0), 20.0),
            Some(MouseWheelViewportScroll::Up(2))
        );
        assert_eq!(
            mouse_wheel_viewport_scroll(MouseScrollDelta::LineDelta(0.0, -1.0), 20.0),
            Some(MouseWheelViewportScroll::Down(1))
        );
        assert_eq!(
            mouse_wheel_viewport_scroll(
                MouseScrollDelta::PixelDelta(PhysicalPosition::new(0.0, 45.0)),
                15.0,
            ),
            Some(MouseWheelViewportScroll::Up(3))
        );
        assert_eq!(
            mouse_wheel_viewport_scroll(
                MouseScrollDelta::PixelDelta(PhysicalPosition::new(0.0, -20.0)),
                15.0,
            ),
            Some(MouseWheelViewportScroll::Down(2))
        );
        assert_eq!(
            mouse_wheel_viewport_scroll(MouseScrollDelta::LineDelta(0.0, 0.0), 20.0),
            None
        );
    }

    #[test]
    fn mouse_wheel_viewport_scroll_moves_terminal_viewport() {
        let grid_size = GridSize::new(5, 3);
        let mut terminal = terminal_with_scrollback(grid_size);

        apply_mouse_wheel_viewport_scroll(&mut terminal, MouseWheelViewportScroll::Up(2));
        assert_eq!(terminal.viewport_offset(), 2);

        apply_mouse_wheel_viewport_scroll(&mut terminal, MouseWheelViewportScroll::Down(1));
        assert_eq!(terminal.viewport_offset(), 1);
    }
}
