//! The winit [`ApplicationHandler`] — owns native windows/tabs, per-tab
//! terminal sessions, and the shared GPU/font state used to render them.
//!
//! Rendering + presentation happens on the winit main thread (macOS requires
//! presenting on the thread that owns the window). Each io thread owns one
//! PTY, touches only its tab's `Terminal` mutex, and posts targeted user
//! events back to the main loop.

use std::collections::HashMap;
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
#[cfg(target_os = "macos")]
use winit::platform::macos::{WindowAttributesExtMacOS, WindowExtMacOS};
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

/// App-wide GPU and glyph state shared by every tab/window.
struct GpuState {
    instance: wgpu::Instance,
    adapter: wgpu::Adapter,
    device: wgpu::Device,
    queue: wgpu::Queue,
    font: FontGrid,
    theme: Theme,
}

/// State for one native tab. On macOS, each tab is an NSWindow in the same
/// AppKit tab group; winit still reports them as distinct `WindowId`s.
struct WindowState {
    window: Arc<Window>,
    surface: wgpu::Surface<'static>,
    surface_config: wgpu::SurfaceConfiguration,
    renderer: Renderer,
    terminal: Arc<Mutex<Terminal>>,
    pty_input_tx: Sender<crate::io_thread::PtyInput>,
    resize_tx: Sender<GridSize>,
    io_thread: Option<crate::io_thread::IoThreadHandle>,
    grid_size: GridSize,
    mouse_selection: MouseSelectionState,
    last_mouse_cell: Option<Point>,
    pressed_mouse_button: Option<MouseButton>,
    ime_state: input::ImeState,
    occluded: bool,
    title: String,
}

impl WindowState {
    fn shutdown(&mut self) {
        if let Some(io_thread) = self.io_thread.take() {
            io_thread.shutdown_and_join();
        }
    }
}

pub struct App {
    config: AppConfig,
    proxy: EventLoopProxy<UserEvent>,
    gpu: Option<GpuState>,
    windows: HashMap<WindowId, WindowState>,
    window_order: Vec<WindowId>,
    focused: Option<WindowId>,
    #[cfg(target_os = "macos")]
    macos_menu: Option<crate::macos_menu::MacosMenu>,
    #[cfg(target_os = "macos")]
    tab_group_identifier: String,
    modifiers: ModifiersState,
    clipboard: SystemClipboard,
    keybinds: KeybindEngine,
}

impl App {
    pub fn new(config: AppConfig, proxy: EventLoopProxy<UserEvent>) -> Self {
        App {
            config,
            proxy,
            gpu: None,
            windows: HashMap::new(),
            window_order: Vec::new(),
            focused: None,
            #[cfg(target_os = "macos")]
            macos_menu: None,
            #[cfg(target_os = "macos")]
            tab_group_identifier: format!("noa.tabs.{}", std::process::id()),
            modifiers: ModifiersState::empty(),
            clipboard: SystemClipboard::new(),
            keybinds: KeybindEngine::default(),
        }
    }

    fn focused_window(&self) -> Option<Arc<Window>> {
        self.focused
            .and_then(|id| self.windows.get(&id))
            .map(|state| state.window.clone())
    }

    fn app_cursor_keys(&self, window_id: WindowId) -> bool {
        self.windows
            .get(&window_id)
            .map(|state| {
                state
                    .terminal
                    .lock()
                    .expect("terminal mutex poisoned")
                    .modes
                    .app_cursor_keys()
            })
            .unwrap_or(false)
    }

    fn redraw(&mut self, window_id: WindowId) {
        let (Some(gpu), Some(state)) = (self.gpu.as_mut(), self.windows.get_mut(&window_id)) else {
            return;
        };
        if state.occluded {
            return;
        }

        let (snapshot, title) = {
            let term = state.terminal.lock().expect("terminal mutex poisoned");
            (FrameSnapshot::from_terminal(&term), tab_title(&term.title))
        };
        if state.title != title {
            state.window.set_title(&title);
            state.title = title;
        }
        update_ime_cursor_area(
            &state.window,
            gpu.font.metrics(),
            snapshot.cursor.x,
            snapshot.cursor.y,
        );

        state
            .renderer
            .rebuild_cells(&snapshot, &mut gpu.font, &gpu.theme);
        state
            .renderer
            .sync_atlas(&gpu.device, &gpu.queue, &mut gpu.font);

        let frame = match state.surface.get_current_texture() {
            Ok(frame) => frame,
            Err(wgpu::SurfaceError::Lost | wgpu::SurfaceError::Outdated) => {
                state.surface.configure(&gpu.device, &state.surface_config);
                state.window.request_redraw();
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

        state.renderer.draw(&gpu.device, &gpu.queue, &view);
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
            AppCommand::NewTab => {
                let _ = self.spawn_tab(event_loop);
            }
            AppCommand::CloseTab => {
                if let Some(window_id) = self.focused {
                    self.close_tab(event_loop, window_id);
                }
            }
            AppCommand::SelectTab(index) => self.select_tab(index),
            AppCommand::NextTab => self.select_next_tab(),
            AppCommand::PrevTab => self.select_previous_tab(),
            AppCommand::Copy => self.copy_selection_to_clipboard(),
            AppCommand::Paste => self.paste_clipboard_to_pty(),
            AppCommand::Search(action) => self.handle_search_action(action),
            AppCommand::ScrollViewport(scroll) => self.scroll_viewport(scroll),
            AppCommand::CloseWindow | AppCommand::Quit => event_loop.exit(),
        }
    }

    fn spawn_tab(&mut self, event_loop: &ActiveEventLoop) -> anyhow::Result<WindowId> {
        let initial_grid_size = GridSize::new(self.config.cols, self.config.rows);
        let monitor_scale_factor = event_loop
            .primary_monitor()
            .map(|monitor| monitor.scale_factor())
            .unwrap_or(1.0);

        let mut first_font = if self.gpu.is_none() {
            Some(
                FontGrid::new(font_pixel_size(self.config.font_size, monitor_scale_factor))
                    .expect("failed to load a system monospace font"),
            )
        } else {
            None
        };
        let metrics = first_font
            .as_ref()
            .map(FontGrid::metrics)
            .or_else(|| self.gpu.as_ref().map(|gpu| gpu.font.metrics()))
            .expect("font must exist before creating a tab");
        let inner_size =
            initial_window_logical_size(metrics, initial_grid_size, monitor_scale_factor);

        let window_attrs = self.tab_window_attributes(inner_size);
        let window = Arc::new(
            event_loop
                .create_window(window_attrs)
                .expect("failed to create window"),
        );
        let window_scale_factor = window.scale_factor();
        if let Some(font) = first_font.as_mut()
            && (window_scale_factor - monitor_scale_factor).abs() > f64::EPSILON
        {
            *font = FontGrid::new(font_pixel_size(self.config.font_size, window_scale_factor))
                .expect("failed to load a system monospace font");
            let inner_size =
                initial_window_logical_size(font.metrics(), initial_grid_size, window_scale_factor);
            let _ = window.request_inner_size(inner_size);
        }
        window.set_ime_allowed(true);
        update_ime_cursor_area(&window, metrics, 0, 0);

        let surface = if self.gpu.is_none() {
            let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor::default());
            let surface = instance
                .create_surface(window.clone())
                .expect("failed to create wgpu surface");
            let adapter =
                pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
                    power_preference: wgpu::PowerPreference::default(),
                    compatible_surface: Some(&surface),
                    force_fallback_adapter: false,
                }))
                .expect("failed to find a compatible wgpu adapter");
            let (device, queue) =
                pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
                    label: Some("noa-device"),
                    required_features: wgpu::Features::empty(),
                    required_limits: wgpu::Limits::default(),
                    experimental_features: wgpu::ExperimentalFeatures::default(),
                    memory_hints: wgpu::MemoryHints::default(),
                    trace: wgpu::Trace::Off,
                }))
                .expect("failed to request a wgpu device");
            self.gpu = Some(GpuState {
                instance,
                adapter,
                device,
                queue,
                font: first_font.expect("first tab must initialize the font"),
                theme: crate::theme::default_theme(),
            });
            surface
        } else {
            let gpu = self.gpu.as_ref().expect("gpu initialized");
            gpu.instance
                .create_surface(window.clone())
                .expect("failed to create wgpu surface")
        };

        let (surface_config, renderer) = {
            let gpu = self.gpu.as_mut().expect("gpu initialized");
            let caps = surface.get_capabilities(&gpu.adapter);
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
            surface.configure(&gpu.device, &surface_config);

            let mut renderer =
                Renderer::new(&gpu.device, &gpu.queue, surface_format, &mut gpu.font)
                    .expect("failed to build the renderer");
            renderer.resize(PixelSize {
                w: surface_config.width,
                h: surface_config.height,
            });
            (surface_config, renderer)
        };

        let window_id = window.id();
        let pty_config = PtyConfig {
            size: initial_grid_size,
            ..Default::default()
        };
        let pty = Pty::spawn(pty_config).expect("failed to spawn pty");
        let terminal = Arc::new(Mutex::new(Terminal::new(initial_grid_size)));
        let (resize_tx, resize_rx) = crossbeam_channel::unbounded();
        let (pty_input_tx, pty_input_rx) = crate::io_thread::input_channel();
        let io_thread = crate::io_thread::spawn(
            pty,
            terminal.clone(),
            self.proxy.clone(),
            window_id,
            resize_rx,
            pty_input_rx,
        );

        self.windows.insert(
            window_id,
            WindowState {
                window: window.clone(),
                surface,
                surface_config,
                renderer,
                terminal,
                pty_input_tx,
                resize_tx,
                io_thread: Some(io_thread),
                grid_size: initial_grid_size,
                mouse_selection: MouseSelectionState::default(),
                last_mouse_cell: None,
                pressed_mouse_button: None,
                ime_state: input::ImeState::default(),
                occluded: false,
                title: "noa".to_string(),
            },
        );
        self.window_order.push(window_id);
        self.focused = Some(window_id);
        window.focus_window();
        Ok(window_id)
    }

    fn tab_window_attributes(&self, inner_size: LogicalSize<f64>) -> WindowAttributes {
        let attrs = WindowAttributes::default()
            .with_title("noa")
            .with_inner_size(inner_size);
        #[cfg(target_os = "macos")]
        {
            attrs.with_tabbing_identifier(&self.tab_group_identifier)
        }
        #[cfg(not(target_os = "macos"))]
        {
            attrs
        }
    }

    fn close_tab(&mut self, event_loop: &ActiveEventLoop, window_id: WindowId) {
        let outcome = close_tab_outcome(&self.window_order, self.focused, window_id);
        if outcome == TabCloseOutcome::Stale {
            return;
        }

        if let Some(mut state) = self.windows.remove(&window_id) {
            state.shutdown();
        }
        self.window_order.retain(|id| *id != window_id);

        match outcome {
            TabCloseOutcome::Stale => {}
            TabCloseOutcome::Quit => {
                self.focused = None;
                event_loop.exit();
            }
            TabCloseOutcome::Continue { focused } => {
                self.focused = focused;
                if let Some(window) = self.focused_window() {
                    window.focus_window();
                }
            }
        }
    }

    fn select_tab(&mut self, index: usize) {
        if index == 0 {
            return;
        }
        #[cfg(target_os = "macos")]
        {
            if let Some(window) = self.focused_window() {
                window.select_tab_at_index(index - 1);
            }
        }
        #[cfg(not(target_os = "macos"))]
        {
            if let Some(window_id) = self.window_order.get(index - 1).copied() {
                self.focused = Some(window_id);
                if let Some(window) = self.focused_window() {
                    window.focus_window();
                }
            }
        }
    }

    fn select_next_tab(&mut self) {
        #[cfg(target_os = "macos")]
        {
            if let Some(window) = self.focused_window() {
                window.select_next_tab();
            }
        }
        #[cfg(not(target_os = "macos"))]
        self.cycle_fallback_tab(1);
    }

    fn select_previous_tab(&mut self) {
        #[cfg(target_os = "macos")]
        {
            if let Some(window) = self.focused_window() {
                window.select_previous_tab();
            }
        }
        #[cfg(not(target_os = "macos"))]
        self.cycle_fallback_tab(-1);
    }

    #[cfg(not(target_os = "macos"))]
    fn cycle_fallback_tab(&mut self, direction: isize) {
        if self.window_order.is_empty() {
            return;
        }
        let Some(focused) = self.focused else {
            return;
        };
        let Some(current) = self.window_order.iter().position(|id| *id == focused) else {
            return;
        };
        let len = self.window_order.len() as isize;
        let next = (current as isize + direction).rem_euclid(len) as usize;
        self.focused = Some(self.window_order[next]);
        if let Some(window) = self.focused_window() {
            window.focus_window();
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

impl Drop for App {
    fn drop(&mut self) {
        for state in self.windows.values_mut() {
            state.shutdown();
        }
    }
}

impl ApplicationHandler<UserEvent> for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.windows.is_empty() {
            let _ = self.spawn_tab(event_loop);
        }
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: UserEvent) {
        match event {
            UserEvent::AppCommand(command) => self.handle_app_command(event_loop, command),
            UserEvent::ClipboardWrite { window_id, text } => {
                if !self.windows.contains_key(&window_id) {
                    return;
                }
                if let Err(err) = self.clipboard.set_text(&text) {
                    log::warn!("failed to write OSC 52 clipboard text: {err}");
                }
            }
            UserEvent::Redraw(window_id) => match targeted_redraw_decision(
                self.windows.contains_key(&window_id),
                self.windows
                    .get(&window_id)
                    .map(|state| state.occluded)
                    .unwrap_or(false),
            ) {
                TargetedRedrawDecision::Request => {
                    if let Some(state) = self.windows.get(&window_id) {
                        state.window.request_redraw();
                    }
                }
                TargetedRedrawDecision::Stale | TargetedRedrawDecision::Suppress => {}
            },
            UserEvent::PtyExit(window_id) => self.close_tab(event_loop, window_id),
        }
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: WindowEvent,
    ) {
        if !self.windows.contains_key(&window_id) {
            return;
        }

        match event {
            WindowEvent::CloseRequested => self.close_tab(event_loop, window_id),
            WindowEvent::RedrawRequested => self.redraw(window_id),
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                self.on_scale_factor_changed(window_id, scale_factor)
            }
            WindowEvent::Resized(size) => self.on_resize(window_id, size),
            WindowEvent::Focused(true) => self.focused = Some(window_id),
            WindowEvent::Focused(false) => {}
            WindowEvent::Occluded(occluded) => {
                if let Some(state) = self.windows.get_mut(&window_id) {
                    state.occluded = occluded;
                    if !occluded {
                        state.window.request_redraw();
                    }
                }
            }
            WindowEvent::ModifiersChanged(mods) => self.modifiers = mods.state(),
            WindowEvent::CursorMoved { position, .. } => self.on_cursor_moved(window_id, position),
            WindowEvent::MouseInput { state, button, .. } => {
                self.on_mouse_input(window_id, state, button)
            }
            WindowEvent::MouseWheel { delta, .. } => self.on_mouse_wheel(window_id, delta),
            WindowEvent::Ime(event) => self.on_ime_event(window_id, event),
            WindowEvent::KeyboardInput { event, .. } => {
                if event.state != ElementState::Pressed {
                    return;
                }
                if self
                    .windows
                    .get(&window_id)
                    .is_some_and(|state| state.ime_state.preedit_active())
                {
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
                let app_cursor_keys = self.app_cursor_keys(window_id);
                let bytes = input::encode_key(
                    &event.logical_key,
                    event.text.as_deref(),
                    self.modifiers,
                    app_cursor_keys,
                );
                if let Some(bytes) = bytes {
                    self.write_pty_bytes(window_id, &bytes);
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
    fn on_scale_factor_changed(&mut self, window_id: WindowId, scale_factor: f64) {
        if let Some(gpu) = self.gpu.as_mut() {
            match FontGrid::new(font_pixel_size(self.config.font_size, scale_factor)) {
                Ok(font) => gpu.font = font,
                Err(err) => {
                    log::warn!("failed to rebuild font for scale factor {scale_factor}: {err}");
                }
            }
        }
        let Some(state) = self.windows.get(&window_id) else {
            return;
        };
        let Some(gpu) = self.gpu.as_ref() else {
            return;
        };
        let grid_size = grid_size_for_physical_size(state.window.inner_size(), gpu.font.metrics());
        let window = state.window.clone();
        self.resize_grid(window_id, grid_size);
        window.request_redraw();
    }

    fn on_resize(&mut self, window_id: WindowId, size: PhysicalSize<u32>) {
        let Some(gpu) = self.gpu.as_mut() else {
            return;
        };
        let Some(state) = self.windows.get_mut(&window_id) else {
            return;
        };
        if size.width == 0 || size.height == 0 {
            return;
        }
        state.surface_config.width = size.width;
        state.surface_config.height = size.height;
        state.surface.configure(&gpu.device, &state.surface_config);
        state.renderer.resize(PixelSize {
            w: size.width,
            h: size.height,
        });
        let grid_size = grid_size_for_physical_size(size, gpu.font.metrics());
        let window = state.window.clone();
        self.resize_grid(window_id, grid_size);
        window.request_redraw();
    }

    fn resize_grid(&mut self, window_id: WindowId, grid_size: GridSize) {
        let Some(state) = self.windows.get_mut(&window_id) else {
            return;
        };
        state.grid_size = grid_size;
        // Grid-first ordering: resize the tab's Terminal grid BEFORE telling
        // the pty its new winsize. Otherwise the shell's SIGWINCH repaint can
        // reach the io thread and be fed into a still-old grid.
        state
            .terminal
            .lock()
            .expect("terminal mutex poisoned")
            .resize(state.grid_size);
        let _ = state.resize_tx.send(state.grid_size);
    }

    fn on_cursor_moved(&mut self, window_id: WindowId, position: PhysicalPosition<f64>) {
        let Some(gpu) = self.gpu.as_ref() else {
            return;
        };
        let Some(state) = self.windows.get(&window_id) else {
            return;
        };
        let metrics = gpu.font.metrics();
        let cell = mouse::physical_position_to_grid_point(
            position.x,
            position.y,
            metrics.cell_w,
            metrics.cell_h,
            state.grid_size,
        );
        if let Some(state) = self.windows.get_mut(&window_id) {
            state.last_mouse_cell = Some(cell);
        }

        let tracking = self.sgr_mouse_tracking(window_id);
        if tracking != MouseTracking::Off && !self.modifiers.shift_key() {
            let pressed_mouse_button = self
                .windows
                .get(&window_id)
                .and_then(|state| state.pressed_mouse_button);
            if let Some(bytes) =
                mouse::encode_sgr_mouse_motion(tracking, pressed_mouse_button, cell, self.modifiers)
            {
                self.write_pty_bytes(window_id, &bytes);
            }
            return;
        }

        let gesture = self
            .windows
            .get_mut(&window_id)
            .map(|state| state.mouse_selection.cursor_moved(cell))
            .unwrap_or(SelectionGesture::None);
        self.apply_selection_gesture(window_id, gesture);
    }

    fn on_mouse_input(&mut self, window_id: WindowId, state: ElementState, button: MouseButton) {
        let tracking = self.sgr_mouse_tracking(window_id);
        if tracking != MouseTracking::Off && !self.modifiers.shift_key() {
            let last_mouse_cell = self
                .windows
                .get(&window_id)
                .and_then(|state| state.last_mouse_cell);
            if let Some(cell) = last_mouse_cell
                && let Some(bytes) =
                    mouse::encode_sgr_mouse_input(button, state, cell, self.modifiers)
            {
                self.write_pty_bytes(window_id, &bytes);
            }

            if let Some(tab) = self.windows.get_mut(&window_id) {
                match state {
                    ElementState::Pressed => tab.pressed_mouse_button = Some(button),
                    ElementState::Released => {
                        if tab.pressed_mouse_button == Some(button) {
                            tab.pressed_mouse_button = None;
                        }
                    }
                }
            }
            return;
        }

        if button != MouseButton::Left {
            return;
        }
        if let Some(cell) = self
            .windows
            .get(&window_id)
            .and_then(|tab| tab.last_mouse_cell)
            && let Some(tab) = self.windows.get_mut(&window_id)
        {
            let _ = tab.mouse_selection.cursor_moved(cell);
        }

        let gesture = self
            .windows
            .get_mut(&window_id)
            .map(|tab| match state {
                ElementState::Pressed => tab.mouse_selection.left_pressed(Instant::now()),
                ElementState::Released => tab.mouse_selection.left_released(),
            })
            .unwrap_or(SelectionGesture::None);
        self.apply_selection_gesture(window_id, gesture);
    }

    fn on_mouse_wheel(&mut self, window_id: WindowId, delta: MouseScrollDelta) {
        if self.sgr_mouse_tracking(window_id) != MouseTracking::Off && !self.modifiers.shift_key() {
            let Some(cell) = self
                .windows
                .get(&window_id)
                .and_then(|state| state.last_mouse_cell)
            else {
                return;
            };
            let delta_y = match delta {
                MouseScrollDelta::LineDelta(_, y) => y,
                MouseScrollDelta::PixelDelta(position) => position.y as f32,
            };
            if let Some(bytes) = mouse::encode_sgr_mouse_wheel(delta_y, cell, self.modifiers) {
                self.write_pty_bytes(window_id, &bytes);
            }
            return;
        }

        let cell_h = self
            .gpu
            .as_ref()
            .map(|gpu| gpu.font.metrics().cell_h)
            .unwrap_or(1.0);
        if let Some(scroll) = mouse_wheel_viewport_scroll(delta, cell_h) {
            self.scroll_mouse_wheel_viewport(window_id, scroll);
        }
    }

    fn on_ime_event(&mut self, window_id: WindowId, event: Ime) {
        let bytes = self
            .windows
            .get_mut(&window_id)
            .and_then(|state| state.ime_state.handle_event(&event));
        if let Some(bytes) = bytes {
            self.write_pty_bytes(window_id, &bytes);
        }
    }

    fn scroll_viewport(&mut self, scroll: ViewportScroll) {
        let Some(window_id) =
            resolve_command_target(AppCommand::ScrollViewport(scroll), self.focused)
        else {
            return;
        };
        let Some(state) = self.windows.get(&window_id) else {
            return;
        };

        apply_viewport_scroll(
            &mut state.terminal.lock().expect("terminal mutex poisoned"),
            state.grid_size,
            scroll,
        );

        if let Some(state) = self.windows.get(&window_id) {
            state.window.request_redraw();
        }
    }

    fn scroll_mouse_wheel_viewport(
        &mut self,
        window_id: WindowId,
        scroll: MouseWheelViewportScroll,
    ) {
        let Some(state) = self.windows.get(&window_id) else {
            return;
        };

        apply_mouse_wheel_viewport_scroll(
            &mut state.terminal.lock().expect("terminal mutex poisoned"),
            scroll,
        );

        if let Some(state) = self.windows.get(&window_id) {
            state.window.request_redraw();
        }
    }

    fn handle_search_action(&mut self, action: SearchAction) {
        let Some(window_id) = resolve_command_target(AppCommand::Search(action), self.focused)
        else {
            return;
        };
        let Some(state) = self.windows.get(&window_id) else {
            return;
        };

        let mut terminal = state.terminal.lock().expect("terminal mutex poisoned");
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

        if let Some(state) = self.windows.get(&window_id) {
            state.window.request_redraw();
        }
    }

    fn apply_selection_gesture(&mut self, window_id: WindowId, gesture: SelectionGesture) {
        if gesture == SelectionGesture::None {
            return;
        }

        if let Some(state) = self.windows.get(&window_id) {
            let mut terminal = state.terminal.lock().expect("terminal mutex poisoned");
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

        if let Some(state) = self.windows.get(&window_id) {
            state.window.request_redraw();
        }
    }

    fn copy_selection_to_clipboard(&mut self) {
        let Some(window_id) = resolve_command_target(AppCommand::Copy, self.focused) else {
            return;
        };
        let selected_text = self.windows.get(&window_id).and_then(|state| {
            state
                .terminal
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
        let Some(window_id) = resolve_command_target(AppCommand::Paste, self.focused) else {
            return;
        };
        let text = match self.clipboard.get_text() {
            Ok(text) => text,
            Err(err) => {
                log::warn!("failed to read clipboard for paste: {err}");
                return;
            }
        };
        let bracketed_paste = self.bracketed_paste(window_id);
        if let Some(bytes) = input::encode_paste(&text, bracketed_paste) {
            self.write_pty_bytes(window_id, &bytes);
        }
    }

    fn bracketed_paste(&self, window_id: WindowId) -> bool {
        self.windows
            .get(&window_id)
            .map(|state| {
                state
                    .terminal
                    .lock()
                    .expect("terminal mutex poisoned")
                    .modes
                    .bracketed_paste()
            })
            .unwrap_or(false)
    }

    fn sgr_mouse_tracking(&self, window_id: WindowId) -> MouseTracking {
        self.windows
            .get(&window_id)
            .map(|state| {
                let terminal = state.terminal.lock().expect("terminal mutex poisoned");
                if terminal.modes.sgr_mouse_reporting() {
                    terminal.modes.mouse_tracking()
                } else {
                    MouseTracking::Off
                }
            })
            .unwrap_or(MouseTracking::Off)
    }

    fn write_pty_bytes(&self, window_id: WindowId, bytes: &[u8]) {
        let Some(state) = self.windows.get(&window_id) else {
            return;
        };
        match state
            .pty_input_tx
            .try_send(bytes.to_vec().into_boxed_slice())
        {
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

fn tab_title(title: &str) -> String {
    if title.is_empty() {
        "noa".to_string()
    } else {
        title.to_string()
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TabCloseOutcome<Id> {
    Stale,
    Quit,
    Continue { focused: Option<Id> },
}

fn close_tab_outcome<Id: Copy + Eq>(
    order: &[Id],
    focused: Option<Id>,
    closing: Id,
) -> TabCloseOutcome<Id> {
    let Some(closing_index) = order.iter().position(|id| *id == closing) else {
        return TabCloseOutcome::Stale;
    };
    if order.len() == 1 {
        return TabCloseOutcome::Quit;
    }

    let next_focus = if focused == Some(closing) {
        order.get(closing_index + 1).copied().or_else(|| {
            closing_index
                .checked_sub(1)
                .and_then(|idx| order.get(idx).copied())
        })
    } else {
        focused.filter(|id| {
            order
                .iter()
                .any(|existing| existing == id && *existing != closing)
        })
    };
    TabCloseOutcome::Continue {
        focused: next_focus.or_else(|| order.iter().copied().find(|id| *id != closing)),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TargetedRedrawDecision {
    Stale,
    Suppress,
    Request,
}

fn targeted_redraw_decision(exists: bool, occluded: bool) -> TargetedRedrawDecision {
    if !exists {
        TargetedRedrawDecision::Stale
    } else if occluded {
        TargetedRedrawDecision::Suppress
    } else {
        TargetedRedrawDecision::Request
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CommandScope {
    App,
    FocusedTab,
    NativeTabGroup,
}

fn command_scope(command: AppCommand) -> CommandScope {
    match command {
        AppCommand::Copy
        | AppCommand::Paste
        | AppCommand::Search(_)
        | AppCommand::ScrollViewport(_)
        | AppCommand::CloseTab => CommandScope::FocusedTab,
        AppCommand::SelectTab(_) | AppCommand::NextTab | AppCommand::PrevTab => {
            CommandScope::NativeTabGroup
        }
        AppCommand::About
        | AppCommand::Preferences
        | AppCommand::NewTab
        | AppCommand::CloseWindow
        | AppCommand::Quit => CommandScope::App,
    }
}

fn resolve_command_target<Id: Copy>(command: AppCommand, focused: Option<Id>) -> Option<Id> {
    if command_scope(command) == CommandScope::FocusedTab {
        focused
    } else {
        None
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

    #[test]
    fn close_tab_outcome_is_unambiguous() {
        assert_eq!(
            close_tab_outcome(&[1, 2, 3], Some(2), 9),
            TabCloseOutcome::Stale
        );
        assert_eq!(close_tab_outcome(&[1], Some(1), 1), TabCloseOutcome::Quit);
        assert_eq!(
            close_tab_outcome(&[1, 2, 3], Some(2), 2),
            TabCloseOutcome::Continue { focused: Some(3) }
        );
        assert_eq!(
            close_tab_outcome(&[1, 2, 3], Some(3), 3),
            TabCloseOutcome::Continue { focused: Some(2) }
        );
        assert_eq!(
            close_tab_outcome(&[1, 2, 3], Some(1), 2),
            TabCloseOutcome::Continue { focused: Some(1) }
        );
    }

    #[test]
    fn targeted_redraw_decision_drops_stale_and_suppresses_occluded_tabs() {
        assert_eq!(
            targeted_redraw_decision(false, false),
            TargetedRedrawDecision::Stale
        );
        assert_eq!(
            targeted_redraw_decision(true, true),
            TargetedRedrawDecision::Suppress
        );
        assert_eq!(
            targeted_redraw_decision(true, false),
            TargetedRedrawDecision::Request
        );
    }

    #[test]
    fn command_target_resolution_uses_focused_tab_only_for_terminal_commands() {
        let focused = Some(42_u8);
        for command in [
            AppCommand::Copy,
            AppCommand::Paste,
            AppCommand::Search(SearchAction::FindNext),
            AppCommand::ScrollViewport(ViewportScroll::PageDown),
            AppCommand::CloseTab,
        ] {
            assert_eq!(resolve_command_target(command, focused), focused);
        }

        for command in [
            AppCommand::NewTab,
            AppCommand::SelectTab(1),
            AppCommand::NextTab,
            AppCommand::PrevTab,
            AppCommand::Quit,
        ] {
            assert_eq!(resolve_command_target(command, focused), None);
        }
    }

    #[test]
    fn empty_terminal_title_falls_back_to_app_name() {
        assert_eq!(tab_title(""), "noa");
        assert_eq!(tab_title("shell"), "shell");
    }
}
