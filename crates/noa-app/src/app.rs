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
use noa_core::{DEFAULT_GRID_PADDING, GridPadding, GridSize, PixelSize, Point};
use noa_font::FontGrid;
use noa_grid::{Terminal, modes::MouseTracking};
use noa_pty::{Pty, PtyConfig};
use noa_render::{FrameSnapshot, PaneFrame, PaneId as RenderPaneId, PaneRect, Renderer, Theme};
use winit::application::ApplicationHandler;
use winit::dpi::{LogicalSize, PhysicalPosition, PhysicalSize};
use winit::event::{ElementState, Ime, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, EventLoopProxy};
use winit::keyboard::ModifiersState;
#[cfg(target_os = "macos")]
use winit::platform::macos::{WindowAttributesExtMacOS, WindowExtMacOS};
use winit::window::{Window, WindowAttributes, WindowId};

use crate::clipboard::SystemClipboard;
use crate::commands::{FontSizeAction, KeybindEngine, SearchAction, TerminalAction};
use crate::events::UserEvent;
use crate::input;
use crate::mouse::{self, MouseSelectionState, SelectionGesture};
use crate::split_tree::{
    self, Direction, HitTarget, ImeOp, MIN_PANE_SIZE_PX, PaneId, Rect as PaneRectApp,
    SPLIT_RESIZE_STEP_PX, SplitOrientation, SplitTree, equalize, focus_in_direction,
    focus_switch_plan, hit_test, resize_split, split_pane, zoom_resize_targets, zoom_toggle,
};
use crate::{AppCommand, ViewportScroll};

/// Configuration the binary passes into [`crate::run`].
#[derive(Debug, Clone)]
pub struct AppConfig {
    pub cols: u16,
    pub rows: u16,
    pub font_size: f32,
    pub theme: Option<String>,
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
    split_tree: SplitTree,
    zoomed: Option<PaneId>,
    focused_pane: PaneId,
    next_pane_id: u64,
    surfaces: HashMap<PaneId, Surface>,
    last_mouse_pane: Option<PaneId>,
    occluded: bool,
    title: String,
}

/// Terminal-owned state for one split leaf. `split_tree` leaves store the
/// `PaneId`; this map owns the corresponding live surface payload.
struct Surface {
    terminal: Arc<Mutex<Terminal>>,
    pty_input_tx: Sender<crate::io_thread::PtyInput>,
    resize_tx: Sender<GridSize>,
    io_thread: Option<crate::io_thread::IoThreadHandle>,
    grid_size: GridSize,
    mouse_selection: MouseSelectionState,
    last_mouse_cell: Option<Point>,
    pressed_mouse_button: Option<MouseButton>,
    ime_state: input::ImeState,
    rect: PaneRectApp,
}

impl WindowState {
    fn shutdown(&mut self) {
        shutdown_pane_io_threads(self.surfaces.values_mut());
    }

    fn focused_surface(&self) -> Option<&Surface> {
        self.surfaces.get(&self.focused_pane)
    }

    fn focused_surface_mut(&mut self) -> Option<&mut Surface> {
        self.surfaces.get_mut(&self.focused_pane)
    }

    fn pane_count(&self) -> usize {
        self.surfaces.len()
    }

    fn contains_pane(&self, pane_id: PaneId) -> bool {
        self.surfaces.contains_key(&pane_id)
    }
}

impl Surface {
    fn shutdown(&mut self) {
        if let Some(io_thread) = self.io_thread.take() {
            io_thread.shutdown_and_join();
        }
    }
}

pub struct App {
    config: AppConfig,
    runtime_font_size: f32,
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
            runtime_font_size: config.font_size,
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
            .and_then(WindowState::focused_surface)
            .map(|surface| {
                surface
                    .terminal
                    .lock()
                    .expect("terminal mutex poisoned")
                    .modes
                    .app_cursor_keys()
            })
            .unwrap_or(false)
    }

    fn app_keypad(&self, window_id: WindowId) -> bool {
        self.windows
            .get(&window_id)
            .and_then(WindowState::focused_surface)
            .map(|surface| {
                surface
                    .terminal
                    .lock()
                    .expect("terminal mutex poisoned")
                    .modes
                    .app_keypad()
            })
            .unwrap_or(false)
    }

    fn focus_reporting(&self, window_id: WindowId) -> bool {
        self.windows
            .get(&window_id)
            .and_then(WindowState::focused_surface)
            .map(|surface| {
                surface
                    .terminal
                    .lock()
                    .expect("terminal mutex poisoned")
                    .modes
                    .focus_reporting()
            })
            .unwrap_or(false)
    }

    fn report_focus_event(&self, window_id: WindowId, focused: bool) {
        if let Some(bytes) = focus_report_bytes(focused, self.focus_reporting(window_id)) {
            self.write_pty_bytes(window_id, bytes);
        }
    }

    fn redraw(&mut self, window_id: WindowId) {
        let (Some(gpu), Some(state)) = (self.gpu.as_mut(), self.windows.get_mut(&window_id)) else {
            return;
        };
        if state.occluded {
            return;
        }

        let mut snapshots = Vec::new();
        let mut title = "noa".to_string();
        let visible_panes = visible_pane_ids(&state.split_tree, state.zoomed);
        for pane_id in visible_panes {
            let Some(surface) = state.surfaces.get(&pane_id) else {
                continue;
            };
            let term = surface.terminal.lock().expect("terminal mutex poisoned");
            if pane_id == state.focused_pane {
                title = tab_title(&term.title);
            }
            snapshots.push((pane_id, surface.rect, FrameSnapshot::from_terminal(&term)));
        }
        if state.title != title {
            state.window.set_title(&title);
            state.title = title;
        }
        if let Some((_, rect, snapshot)) = snapshots
            .iter()
            .find(|(pane_id, _, _)| *pane_id == state.focused_pane)
        {
            update_ime_cursor_area(
                &state.window,
                gpu.font.metrics(),
                snapshot.cursor.x,
                snapshot.cursor.y,
                *rect,
                DEFAULT_GRID_PADDING,
            );
        }

        let panes = snapshots
            .iter()
            .map(|(pane_id, rect, snapshot)| PaneFrame {
                pane: render_pane_id(*pane_id),
                rect: render_pane_rect(*rect),
                snapshot,
            })
            .collect::<Vec<_>>();
        state
            .renderer
            .rebuild_panes(&panes, &mut gpu.font, &gpu.theme);
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

        state.renderer.draw_rebuilt_panes(
            &gpu.device,
            &gpu.queue,
            &view,
            state.zoomed.map(render_pane_id),
        );
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
            AppCommand::NewSplitRight => {
                if let Some(window_id) = self.focused {
                    self.new_split(window_id, SplitOrientation::Horizontal);
                }
            }
            AppCommand::NewSplitDown => {
                if let Some(window_id) = self.focused {
                    self.new_split(window_id, SplitOrientation::Vertical);
                }
            }
            AppCommand::FocusDirection(direction) => {
                if let Some(window_id) = self.focused {
                    self.focus_split_direction(window_id, direction);
                }
            }
            AppCommand::ResizeSplit(direction) => {
                if let Some(window_id) = self.focused {
                    self.resize_focused_split(window_id, direction);
                }
            }
            AppCommand::EqualizeSplits => {
                if let Some(window_id) = self.focused {
                    self.equalize_splits(window_id);
                }
            }
            AppCommand::ToggleSplitZoom => {
                if let Some(window_id) = self.focused {
                    self.toggle_split_zoom(window_id);
                }
            }
            AppCommand::CloseTab => {
                if let Some(window_id) = self.focused {
                    self.close_focused_pane_or_tab(event_loop, window_id);
                }
            }
            AppCommand::SelectTab(index) => self.select_tab(index),
            AppCommand::NextTab => self.select_next_tab(),
            AppCommand::PrevTab => self.select_previous_tab(),
            AppCommand::Copy => self.copy_selection_to_clipboard(),
            AppCommand::Paste => self.paste_clipboard_to_pty(),
            AppCommand::Terminal(action) => self.handle_terminal_action(action),
            AppCommand::FontSize(action) => self.handle_font_size_action(action),
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
                FontGrid::new(font_pixel_size(
                    self.runtime_font_size,
                    monitor_scale_factor,
                ))
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
        let inner_size = initial_window_logical_size(
            metrics,
            initial_grid_size,
            monitor_scale_factor,
            DEFAULT_GRID_PADDING,
        );

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
            *font = FontGrid::new(font_pixel_size(self.runtime_font_size, window_scale_factor))
                .expect("failed to load a system monospace font");
            let inner_size = initial_window_logical_size(
                font.metrics(),
                initial_grid_size,
                window_scale_factor,
                DEFAULT_GRID_PADDING,
            );
            let _ = window.request_inner_size(inner_size);
        }
        window.set_ime_allowed(true);
        update_ime_cursor_area(
            &window,
            metrics,
            0,
            0,
            PaneRectApp::new(0, 0, 0, 0),
            DEFAULT_GRID_PADDING,
        );

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
                theme: crate::theme::resolve_theme(self.config.theme.as_deref()),
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
                alpha_mode: preferred_surface_alpha_mode(&caps),
                view_formats: vec![],
            };
            surface.configure(&gpu.device, &surface_config);

            let mut renderer = Renderer::new(
                &gpu.device,
                &gpu.queue,
                surface_format,
                &mut gpu.font,
                DEFAULT_GRID_PADDING,
            )
            .expect("failed to build the renderer");
            renderer.resize(PixelSize {
                w: surface_config.width,
                h: surface_config.height,
            });
            (surface_config, renderer)
        };

        let window_id = window.id();
        let initial_pane = PaneId::new(1);
        let initial_rect = PaneRectApp::new(0, 0, surface_config.width, surface_config.height);
        let initial_surface =
            self.spawn_pane_surface(window_id, initial_pane, initial_grid_size, initial_rect)?;
        let mut surfaces = HashMap::new();
        surfaces.insert(initial_pane, initial_surface);

        self.windows.insert(
            window_id,
            WindowState {
                window: window.clone(),
                surface,
                surface_config,
                renderer,
                split_tree: SplitTree::leaf(initial_pane),
                zoomed: None,
                focused_pane: initial_pane,
                next_pane_id: 2,
                surfaces,
                last_mouse_pane: Some(initial_pane),
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

    fn spawn_pane_surface(
        &self,
        window_id: WindowId,
        pane_id: PaneId,
        grid_size: GridSize,
        rect: PaneRectApp,
    ) -> anyhow::Result<Surface> {
        let pty_config = PtyConfig {
            size: grid_size,
            ..Default::default()
        };
        let pty = Pty::spawn(pty_config)?;
        let mut terminal = Terminal::new(grid_size);
        if let Some(gpu) = self.gpu.as_ref() {
            terminal.set_base_colors(
                gpu.theme.default_fg,
                gpu.theme.default_bg,
                gpu.theme.cursor,
                gpu.theme.palette,
            );
        }
        let terminal = Arc::new(Mutex::new(terminal));
        let (resize_tx, resize_rx) = crossbeam_channel::unbounded();
        let (pty_input_tx, pty_input_rx) = crate::io_thread::input_channel();
        let io_thread = crate::io_thread::spawn(
            pty,
            terminal.clone(),
            self.proxy.clone(),
            window_id,
            pane_id,
            resize_rx,
            pty_input_rx,
        );

        Ok(Surface {
            terminal,
            pty_input_tx,
            resize_tx,
            io_thread: Some(io_thread),
            grid_size,
            mouse_selection: MouseSelectionState::default(),
            last_mouse_cell: None,
            pressed_mouse_button: None,
            ime_state: input::ImeState::default(),
            rect,
        })
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

    fn close_focused_pane_or_tab(&mut self, event_loop: &ActiveEventLoop, window_id: WindowId) {
        let Some(state) = self.windows.get(&window_id) else {
            return;
        };
        if state.pane_count() <= 1 {
            self.close_tab(event_loop, window_id);
            return;
        }
        let pane_id = state.focused_pane;
        self.close_pane(event_loop, window_id, pane_id);
    }

    fn close_pane_after_pty_exit(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        pane_id: PaneId,
    ) {
        let Some(state) = self.windows.get(&window_id) else {
            return;
        };
        if !state.contains_pane(pane_id) {
            return;
        }
        if state.pane_count() <= 1 {
            self.close_tab(event_loop, window_id);
            return;
        }
        self.close_pane(event_loop, window_id, pane_id);
    }

    fn close_pane(&mut self, event_loop: &ActiveEventLoop, window_id: WindowId, pane_id: PaneId) {
        let should_close_tab = self
            .windows
            .get(&window_id)
            .is_some_and(|state| state.contains_pane(pane_id) && state.pane_count() <= 1);
        if should_close_tab {
            self.close_tab(event_loop, window_id);
            return;
        }

        let mut tab_should_close = false;
        let window = {
            let Some(state) = self.windows.get_mut(&window_id) else {
                return;
            };
            if !state.contains_pane(pane_id) {
                return;
            }

            if let Some(mut surface) = state.surfaces.remove(&pane_id) {
                surface.shutdown();
            }
            let outcome =
                split_tree::close_pane_with_zoom(&mut state.split_tree, pane_id, state.zoomed);
            state.zoomed = outcome.zoomed;
            if outcome.close_outcome.tab_should_close {
                tab_should_close = true;
            } else {
                state.focused_pane = outcome
                    .close_outcome
                    .next_focus
                    .filter(|pane| state.contains_pane(*pane))
                    .or_else(|| state.surfaces.keys().copied().next())
                    .unwrap_or(state.focused_pane);
                state.last_mouse_pane = Some(state.focused_pane);
            }
            state.window.clone()
        };

        if tab_should_close {
            self.close_tab(event_loop, window_id);
            return;
        }
        self.relayout_and_resize_window(window_id);
        self.update_focused_ime_cursor_area(window_id);
        window.request_redraw();
    }

    fn new_split(&mut self, window_id: WindowId, orientation: SplitOrientation) {
        let Some(gpu) = self.gpu.as_ref() else {
            return;
        };
        let Some((focused_pane, new_pane, focused_rect)) =
            self.windows.get_mut(&window_id).and_then(|state| {
                let focused_rect = state.focused_surface()?.rect;
                if !can_split_rect(focused_rect, orientation) {
                    return None;
                }
                let new_pane = PaneId::new(state.next_pane_id);
                state.next_pane_id = state.next_pane_id.saturating_add(1);
                Some((state.focused_pane, new_pane, focused_rect))
            })
        else {
            return;
        };

        let grid_size =
            grid_size_for_pane_rect(focused_rect, gpu.font.metrics(), DEFAULT_GRID_PADDING);
        let new_surface =
            match self.spawn_pane_surface(window_id, new_pane, grid_size, focused_rect) {
                Ok(surface) => surface,
                Err(err) => {
                    log::warn!("failed to spawn split pty: {err}");
                    return;
                }
            };

        let window = {
            let Some(state) = self.windows.get_mut(&window_id) else {
                let mut surface = new_surface;
                surface.shutdown();
                return;
            };
            if !split_pane(&mut state.split_tree, focused_pane, new_pane, orientation) {
                let mut surface = new_surface;
                surface.shutdown();
                return;
            }
            state.surfaces.insert(new_pane, new_surface);
            state.focused_pane = new_pane;
            state.zoomed = None;
            state.last_mouse_pane = Some(new_pane);
            state.window.clone()
        };

        self.relayout_and_resize_window(window_id);
        self.update_focused_ime_cursor_area(window_id);
        window.request_redraw();
    }

    fn focus_split_direction(&mut self, window_id: WindowId, direction: Direction) {
        let Some(next) = self.windows.get(&window_id).and_then(|state| {
            focus_in_direction(&state.split_tree, state.focused_pane, direction)
                .filter(|pane| state.contains_pane(*pane))
        }) else {
            return;
        };
        self.focus_pane(window_id, next);
    }

    fn focus_pane(&mut self, window_id: WindowId, pane_id: PaneId) {
        let Some(state) = self.windows.get(&window_id) else {
            return;
        };
        if !state.contains_pane(pane_id) || state.focused_pane == pane_id {
            return;
        }
        let losing = state.focused_pane;
        let plan = focus_switch_plan(losing, pane_id);

        if let Some(state) = self.windows.get_mut(&window_id) {
            for op in plan {
                match op {
                    ImeOp::CommitPreedit(pane) => {
                        if let Some(surface) = state.surfaces.get_mut(&pane) {
                            surface.ime_state.commit_preedit();
                        }
                    }
                    ImeOp::RetargetIme(pane) => {
                        if state.contains_pane(pane) {
                            state.focused_pane = pane;
                            state.last_mouse_pane = Some(pane);
                        }
                    }
                }
            }
        }
        self.update_focused_ime_cursor_area(window_id);
        if let Some(state) = self.windows.get(&window_id) {
            state.window.request_redraw();
        }
    }

    fn resize_focused_split(&mut self, window_id: WindowId, direction: Direction) {
        let window = {
            let Some(state) = self.windows.get_mut(&window_id) else {
                return;
            };
            resize_split(
                &mut state.split_tree,
                state.focused_pane,
                direction,
                SPLIT_RESIZE_STEP_PX,
            );
            state.window.clone()
        };
        self.relayout_and_resize_window(window_id);
        window.request_redraw();
    }

    fn equalize_splits(&mut self, window_id: WindowId) {
        let window = {
            let Some(state) = self.windows.get_mut(&window_id) else {
                return;
            };
            equalize(&mut state.split_tree);
            state.window.clone()
        };
        self.relayout_and_resize_window(window_id);
        window.request_redraw();
    }

    fn toggle_split_zoom(&mut self, window_id: WindowId) {
        let bounds = self
            .windows
            .get(&window_id)
            .map(|state| pane_bounds_for_size(state.window.inner_size()))
            .unwrap_or(PaneRectApp::new(0, 0, 0, 0));
        let window = {
            let Some(state) = self.windows.get_mut(&window_id) else {
                return;
            };
            let decision = zoom_toggle(&state.split_tree, state.zoomed, state.focused_pane, bounds);
            state.zoomed = decision.zoomed;
            state.window.clone()
        };
        self.relayout_and_resize_window(window_id);
        window.request_redraw();
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

    #[cfg(target_os = "macos")]
    fn show_macos_split_context_menu(&self, window_id: WindowId) {
        let Some(menu) = self.macos_menu.as_ref() else {
            return;
        };
        let Some(window) = self
            .windows
            .get(&window_id)
            .map(|state| state.window.clone())
        else {
            return;
        };
        if let Err(error) = menu.show_split_context_menu(window.as_ref(), None) {
            log::debug!("failed to show macOS split context menu: {error:#}");
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
            UserEvent::ClipboardWrite {
                window_id,
                pane_id,
                text,
            } => {
                if !self
                    .windows
                    .get(&window_id)
                    .is_some_and(|state| state.contains_pane(pane_id))
                {
                    return;
                }
                if let Err(err) = self.clipboard.set_text(&text) {
                    log::warn!("failed to write OSC 52 clipboard text: {err}");
                }
            }
            UserEvent::Redraw(window_id, pane_id) => match pane_user_event_redraw_decision(
                self.windows
                    .get(&window_id)
                    .map(|state| (state.contains_pane(pane_id), state.occluded)),
            ) {
                TargetedRedrawDecision::Request => {
                    if let Some(state) = self.windows.get(&window_id) {
                        state.window.request_redraw();
                    }
                }
                TargetedRedrawDecision::Stale | TargetedRedrawDecision::Suppress => {}
            },
            UserEvent::PtyExit(window_id, pane_id) => {
                self.close_pane_after_pty_exit(event_loop, window_id, pane_id)
            }
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
            WindowEvent::Focused(true) => {
                self.focused = Some(window_id);
                self.report_focus_event(window_id, true);
            }
            WindowEvent::Focused(false) => self.report_focus_event(window_id, false),
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
                    .and_then(WindowState::focused_surface)
                    .is_some_and(|surface| surface.ime_state.preedit_active())
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
                let app_keypad = self.app_keypad(window_id);
                let bytes = input::encode_key_with_modes(
                    &event.logical_key,
                    Some(event.physical_key),
                    event.text.as_deref(),
                    self.modifiers,
                    app_cursor_keys,
                    app_keypad,
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
            match FontGrid::new(font_pixel_size(self.runtime_font_size, scale_factor)) {
                Ok(font) => gpu.font = font,
                Err(err) => {
                    log::warn!("failed to rebuild font for scale factor {scale_factor}: {err}");
                }
            }
        }
        let Some(state) = self.windows.get(&window_id) else {
            return;
        };
        let window = state.window.clone();
        self.relayout_and_resize_window(window_id);
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
        let window = state.window.clone();
        self.relayout_and_resize_window(window_id);
        window.request_redraw();
    }

    fn on_cursor_moved(&mut self, window_id: WindowId, position: PhysicalPosition<f64>) {
        let Some(gpu) = self.gpu.as_ref() else {
            return;
        };
        let metrics = gpu.font.metrics();
        let Some((pane_id, cell)) = self.pane_cell_at_position(window_id, position, metrics) else {
            if let Some(state) = self.windows.get_mut(&window_id) {
                state.last_mouse_pane = None;
            }
            return;
        };

        if let Some(state) = self.windows.get_mut(&window_id) {
            state.last_mouse_pane = Some(pane_id);
            if let Some(surface) = state.surfaces.get_mut(&pane_id) {
                surface.last_mouse_cell = Some(cell);
            }
        }

        let tracking = self.sgr_mouse_tracking(window_id, pane_id);
        if tracking != MouseTracking::Off && !self.modifiers.shift_key() {
            let pressed_mouse_button = self
                .windows
                .get(&window_id)
                .and_then(|state| state.surfaces.get(&pane_id))
                .and_then(|surface| surface.pressed_mouse_button);
            if let Some(bytes) =
                mouse::encode_sgr_mouse_motion(tracking, pressed_mouse_button, cell, self.modifiers)
            {
                self.write_pane_pty_bytes(window_id, pane_id, &bytes);
            }
            return;
        }

        let gesture = self
            .windows
            .get_mut(&window_id)
            .and_then(|state| state.surfaces.get_mut(&pane_id))
            .map(|surface| surface.mouse_selection.cursor_moved(cell))
            .unwrap_or(SelectionGesture::None);
        self.apply_selection_gesture(window_id, pane_id, gesture);
    }

    fn on_mouse_input(&mut self, window_id: WindowId, state: ElementState, button: MouseButton) {
        let pane_id = self
            .windows
            .get(&window_id)
            .and_then(|state| state.last_mouse_pane)
            .or_else(|| self.windows.get(&window_id).map(|state| state.focused_pane));
        let Some(pane_id) = pane_id else {
            return;
        };

        if button == MouseButton::Left && state == ElementState::Pressed {
            self.focus_pane(window_id, pane_id);
        }

        let tracking = self.sgr_mouse_tracking(window_id, pane_id);
        if tracking != MouseTracking::Off && !self.modifiers.shift_key() {
            let last_mouse_cell = self
                .windows
                .get(&window_id)
                .and_then(|state| state.surfaces.get(&pane_id))
                .and_then(|surface| surface.last_mouse_cell);
            if let Some(cell) = last_mouse_cell
                && let Some(bytes) =
                    mouse::encode_sgr_mouse_input(button, state, cell, self.modifiers)
            {
                self.write_pane_pty_bytes(window_id, pane_id, &bytes);
            }

            if let Some(tab) = self.windows.get_mut(&window_id)
                && let Some(surface) = tab.surfaces.get_mut(&pane_id)
            {
                match state {
                    ElementState::Pressed => surface.pressed_mouse_button = Some(button),
                    ElementState::Released => {
                        if surface.pressed_mouse_button == Some(button) {
                            surface.pressed_mouse_button = None;
                        }
                    }
                }
            }
            return;
        }

        if button == MouseButton::Right {
            if state == ElementState::Pressed {
                self.focused = Some(window_id);
                self.focus_pane(window_id, pane_id);
                #[cfg(target_os = "macos")]
                {
                    self.install_macos_menu_if_needed();
                    self.show_macos_split_context_menu(window_id);
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
            .and_then(|tab| tab.surfaces.get(&pane_id))
            .and_then(|surface| surface.last_mouse_cell)
            && let Some(tab) = self.windows.get_mut(&window_id)
            && let Some(surface) = tab.surfaces.get_mut(&pane_id)
        {
            let _ = surface.mouse_selection.cursor_moved(cell);
        }

        let gesture = self
            .windows
            .get_mut(&window_id)
            .and_then(|tab| tab.surfaces.get_mut(&pane_id))
            .map(|surface| match state {
                ElementState::Pressed => surface.mouse_selection.left_pressed(Instant::now()),
                ElementState::Released => surface.mouse_selection.left_released(),
            })
            .unwrap_or(SelectionGesture::None);
        self.apply_selection_gesture(window_id, pane_id, gesture);
    }

    fn on_mouse_wheel(&mut self, window_id: WindowId, delta: MouseScrollDelta) {
        let pane_id = self
            .windows
            .get(&window_id)
            .and_then(|state| state.last_mouse_pane)
            .or_else(|| self.windows.get(&window_id).map(|state| state.focused_pane));
        let Some(pane_id) = pane_id else {
            return;
        };

        if self.sgr_mouse_tracking(window_id, pane_id) != MouseTracking::Off
            && !self.modifiers.shift_key()
        {
            let Some(cell) = self
                .windows
                .get(&window_id)
                .and_then(|state| state.surfaces.get(&pane_id))
                .and_then(|surface| surface.last_mouse_cell)
            else {
                return;
            };
            let delta_y = match delta {
                MouseScrollDelta::LineDelta(_, y) => y,
                MouseScrollDelta::PixelDelta(position) => position.y as f32,
            };
            if let Some(bytes) = mouse::encode_sgr_mouse_wheel(delta_y, cell, self.modifiers) {
                self.write_pane_pty_bytes(window_id, pane_id, &bytes);
            }
            return;
        }

        let cell_h = self
            .gpu
            .as_ref()
            .map(|gpu| gpu.font.metrics().cell_h)
            .unwrap_or(1.0);
        if let Some(scroll) = mouse_wheel_viewport_scroll(delta, cell_h) {
            self.scroll_mouse_wheel_viewport(window_id, pane_id, scroll);
        }
    }

    fn on_ime_event(&mut self, window_id: WindowId, event: Ime) {
        let pane_id = self.windows.get(&window_id).map(|state| state.focused_pane);
        let bytes = self
            .windows
            .get_mut(&window_id)
            .and_then(|state| state.focused_surface_mut())
            .and_then(|surface| surface.ime_state.handle_event(&event));
        if let (Some(pane_id), Some(bytes)) = (pane_id, bytes) {
            self.write_pane_pty_bytes(window_id, pane_id, &bytes);
        }
    }

    fn scroll_viewport(&mut self, scroll: ViewportScroll) {
        let Some((window_id, pane_id)) =
            self.resolve_pane_command_target(AppCommand::ScrollViewport(scroll))
        else {
            return;
        };
        let Some((terminal, grid_size)) = self
            .windows
            .get(&window_id)
            .and_then(|state| state.surfaces.get(&pane_id))
            .map(|surface| (surface.terminal.clone(), surface.grid_size))
        else {
            return;
        };

        apply_viewport_scroll(
            &mut terminal.lock().expect("terminal mutex poisoned"),
            grid_size,
            scroll,
        );

        if let Some(state) = self.windows.get(&window_id) {
            state.window.request_redraw();
        }
    }

    fn scroll_mouse_wheel_viewport(
        &mut self,
        window_id: WindowId,
        pane_id: PaneId,
        scroll: MouseWheelViewportScroll,
    ) {
        let Some(terminal) = self
            .windows
            .get(&window_id)
            .and_then(|state| state.surfaces.get(&pane_id))
            .map(|surface| surface.terminal.clone())
        else {
            return;
        };

        apply_mouse_wheel_viewport_scroll(
            &mut terminal.lock().expect("terminal mutex poisoned"),
            scroll,
        );

        if let Some(state) = self.windows.get(&window_id) {
            state.window.request_redraw();
        }
    }

    fn handle_terminal_action(&mut self, action: TerminalAction) {
        let Some((window_id, pane_id)) =
            self.resolve_pane_command_target(AppCommand::Terminal(action))
        else {
            return;
        };
        let Some(terminal) = self
            .windows
            .get(&window_id)
            .and_then(|state| state.surfaces.get(&pane_id))
            .map(|surface| surface.terminal.clone())
        else {
            return;
        };

        apply_terminal_action(
            &mut terminal.lock().expect("terminal mutex poisoned"),
            action,
        );

        if let Some(state) = self.windows.get(&window_id) {
            state.window.request_redraw();
        }
    }

    fn handle_font_size_action(&mut self, action: FontSizeAction) {
        let Some((window_id, _pane_id)) =
            self.resolve_pane_command_target(AppCommand::FontSize(action))
        else {
            return;
        };
        let Some(scale_factor) = self
            .windows
            .get(&window_id)
            .map(|state| state.window.scale_factor())
        else {
            return;
        };
        let update =
            runtime_font_size_update(self.runtime_font_size, self.config.font_size, action);
        if !update.changed {
            if let Some(state) = self.windows.get(&window_id) {
                state.window.request_redraw();
            }
            return;
        }

        let Some(gpu) = self.gpu.as_mut() else {
            return;
        };
        let font = match FontGrid::new(font_pixel_size(update.point_size, scale_factor)) {
            Ok(font) => font,
            Err(err) => {
                log::warn!(
                    "failed to rebuild font for runtime size {} at scale factor {scale_factor}: {err}",
                    update.point_size
                );
                return;
            }
        };
        gpu.font = font;
        self.runtime_font_size = update.point_size;
        for state in self.windows.values_mut() {
            state
                .renderer
                .sync_atlas(&gpu.device, &gpu.queue, &mut gpu.font);
        }
        let windows = self
            .window_order
            .iter()
            .filter_map(|id| {
                self.windows
                    .get(id)
                    .map(|state| (*id, state.window.inner_size(), state.window.clone()))
            })
            .collect::<Vec<_>>();
        for (window_id, _, _) in &windows {
            self.relayout_and_resize_window(*window_id);
        }
        for (_, _, window) in windows {
            window.request_redraw();
        }
    }

    fn handle_search_action(&mut self, action: SearchAction) {
        let Some((window_id, pane_id)) =
            self.resolve_pane_command_target(AppCommand::Search(action))
        else {
            return;
        };
        let Some(terminal) = self
            .windows
            .get(&window_id)
            .and_then(|state| state.surfaces.get(&pane_id))
            .map(|surface| surface.terminal.clone())
        else {
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

        if let Some(state) = self.windows.get(&window_id) {
            state.window.request_redraw();
        }
    }

    fn apply_selection_gesture(
        &mut self,
        window_id: WindowId,
        pane_id: PaneId,
        gesture: SelectionGesture,
    ) {
        if gesture == SelectionGesture::None {
            return;
        }

        if let Some(surface) = self
            .windows
            .get(&window_id)
            .and_then(|state| state.surfaces.get(&pane_id))
        {
            let mut terminal = surface.terminal.lock().expect("terminal mutex poisoned");
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
        let Some((window_id, pane_id)) = self.resolve_pane_command_target(AppCommand::Copy) else {
            return;
        };
        let selected_text = self
            .windows
            .get(&window_id)
            .and_then(|state| state.surfaces.get(&pane_id))
            .and_then(|surface| {
                surface
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
        let Some((window_id, pane_id)) = self.resolve_pane_command_target(AppCommand::Paste) else {
            return;
        };
        let text = match self.clipboard.get_text() {
            Ok(text) => text,
            Err(err) => {
                log::warn!("failed to read clipboard for paste: {err}");
                return;
            }
        };
        let bracketed_paste = self.bracketed_paste(window_id, pane_id);
        if let Some(bytes) = input::encode_paste(&text, bracketed_paste) {
            self.write_pane_pty_bytes(window_id, pane_id, &bytes);
        }
    }

    fn bracketed_paste(&self, window_id: WindowId, pane_id: PaneId) -> bool {
        self.windows
            .get(&window_id)
            .and_then(|state| state.surfaces.get(&pane_id))
            .map(|surface| {
                surface
                    .terminal
                    .lock()
                    .expect("terminal mutex poisoned")
                    .modes
                    .bracketed_paste()
            })
            .unwrap_or(false)
    }

    fn sgr_mouse_tracking(&self, window_id: WindowId, pane_id: PaneId) -> MouseTracking {
        self.windows
            .get(&window_id)
            .and_then(|state| state.surfaces.get(&pane_id))
            .map(|surface| {
                let terminal = surface.terminal.lock().expect("terminal mutex poisoned");
                if terminal.modes.sgr_mouse_reporting() {
                    terminal.modes.mouse_tracking()
                } else {
                    MouseTracking::Off
                }
            })
            .unwrap_or(MouseTracking::Off)
    }

    fn write_pty_bytes(&self, window_id: WindowId, bytes: &[u8]) {
        let Some(pane_id) = self.windows.get(&window_id).map(|state| state.focused_pane) else {
            return;
        };
        self.write_pane_pty_bytes(window_id, pane_id, bytes);
    }

    fn write_pane_pty_bytes(&self, window_id: WindowId, pane_id: PaneId, bytes: &[u8]) {
        let Some(surface) = self
            .windows
            .get(&window_id)
            .and_then(|state| state.surfaces.get(&pane_id))
        else {
            return;
        };
        match surface
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

    fn resolve_pane_command_target(&self, command: AppCommand) -> Option<(WindowId, PaneId)> {
        let window_id = resolve_command_target(command, self.focused)?;
        let state = self.windows.get(&window_id)?;
        let pane_id = split_tree::resolve_pane_command_target(command, Some(state.focused_pane))?;
        state.contains_pane(pane_id).then_some((window_id, pane_id))
    }

    fn relayout_and_resize_window(&mut self, window_id: WindowId) {
        let Some(metrics) = self.gpu.as_ref().map(|gpu| gpu.font.metrics()) else {
            return;
        };
        let Some(state) = self.windows.get(&window_id) else {
            return;
        };
        let bounds = pane_bounds_for_size(state.window.inner_size());
        let targets = zoom_resize_targets(&state.split_tree, state.zoomed, bounds)
            .into_iter()
            .map(|(pane_id, rect)| {
                (
                    pane_id,
                    rect,
                    grid_size_for_pane_rect(rect, metrics, DEFAULT_GRID_PADDING),
                )
            })
            .collect::<Vec<_>>();

        let Some(state) = self.windows.get_mut(&window_id) else {
            return;
        };
        apply_pane_resize_batch(state, &targets);
    }

    fn update_focused_ime_cursor_area(&self, window_id: WindowId) {
        let Some(gpu) = self.gpu.as_ref() else {
            return;
        };
        let Some(state) = self.windows.get(&window_id) else {
            return;
        };
        let Some(surface) = state.focused_surface() else {
            return;
        };
        let cursor = {
            let terminal = surface.terminal.lock().expect("terminal mutex poisoned");
            terminal.active().cursor
        };
        update_ime_cursor_area(
            &state.window,
            gpu.font.metrics(),
            cursor.x,
            cursor.y,
            surface.rect,
            DEFAULT_GRID_PADDING,
        );
    }

    fn pane_cell_at_position(
        &self,
        window_id: WindowId,
        position: PhysicalPosition<f64>,
        metrics: noa_font::Metrics,
    ) -> Option<(PaneId, Point)> {
        let state = self.windows.get(&window_id)?;
        let point = split_point_from_physical_position(position)?;
        let layout = visible_pane_ids(&state.split_tree, state.zoomed)
            .into_iter()
            .filter_map(|pane_id| {
                state
                    .surfaces
                    .get(&pane_id)
                    .map(|surface| (pane_id, surface.rect))
            })
            .collect::<Vec<_>>();
        let pane_id = match hit_test(&layout, point) {
            Some(HitTarget::Pane(pane_id)) => pane_id,
            Some(HitTarget::Divider) | None => return None,
        };
        let surface = state.surfaces.get(&pane_id)?;
        let local_x = position.x - f64::from(surface.rect.x);
        let local_y = position.y - f64::from(surface.rect.y);
        let cell = mouse::physical_position_to_grid_point(
            local_x,
            local_y,
            metrics.cell_w,
            metrics.cell_h,
            surface.grid_size,
            DEFAULT_GRID_PADDING,
        );
        Some((pane_id, cell))
    }
}

const MIN_RUNTIME_FONT_SIZE: f32 = 6.0;
const MAX_RUNTIME_FONT_SIZE: f32 = 96.0;

#[derive(Clone, Copy, Debug, PartialEq)]
struct RuntimeFontSizeUpdate {
    point_size: f32,
    changed: bool,
}

fn runtime_font_size_update(
    current: f32,
    startup: f32,
    action: FontSizeAction,
) -> RuntimeFontSizeUpdate {
    let requested = match action {
        FontSizeAction::Increase => current + 1.0,
        FontSizeAction::Decrease => current - 1.0,
        FontSizeAction::Reset => startup,
    };
    let point_size = clamp_runtime_font_size(requested);
    RuntimeFontSizeUpdate {
        point_size,
        changed: !current.is_finite() || (point_size - current).abs() > f32::EPSILON,
    }
}

fn clamp_runtime_font_size(point_size: f32) -> f32 {
    if point_size.is_finite() {
        point_size.clamp(MIN_RUNTIME_FONT_SIZE, MAX_RUNTIME_FONT_SIZE)
    } else {
        MIN_RUNTIME_FONT_SIZE
    }
}

#[cfg(test)]
fn font_size_resize_plan<Id: Copy>(
    windows: impl IntoIterator<Item = (Id, PhysicalSize<u32>)>,
    metrics: noa_font::Metrics,
    padding: GridPadding,
) -> Vec<(Id, GridSize)> {
    windows
        .into_iter()
        .map(|(id, size)| (id, grid_size_for_physical_size(size, metrics, padding)))
        .collect()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PaneResizeAction<Id> {
    GridResize(Id, GridSize),
    PtyResize(Id, GridSize),
}

fn pane_resize_batch_plan<Id: Copy>(
    panes: impl IntoIterator<Item = (Id, GridSize)>,
) -> Vec<PaneResizeAction<Id>> {
    let panes = panes.into_iter().collect::<Vec<_>>();
    let mut plan = Vec::with_capacity(panes.len().saturating_mul(2));
    plan.extend(
        panes
            .iter()
            .map(|(pane_id, grid_size)| PaneResizeAction::GridResize(*pane_id, *grid_size)),
    );
    plan.extend(
        panes
            .iter()
            .map(|(pane_id, grid_size)| PaneResizeAction::PtyResize(*pane_id, *grid_size)),
    );
    plan
}

fn apply_pane_resize_batch(state: &mut WindowState, targets: &[(PaneId, PaneRectApp, GridSize)]) {
    let plan = pane_resize_batch_plan(
        targets
            .iter()
            .map(|(pane_id, _, grid_size)| (*pane_id, *grid_size)),
    );

    for action in &plan {
        let PaneResizeAction::GridResize(pane_id, grid_size) = *action else {
            continue;
        };
        let Some(surface) = state.surfaces.get_mut(&pane_id) else {
            continue;
        };
        if let Some((_, rect, _)) = targets.iter().find(|(target, _, _)| *target == pane_id) {
            surface.rect = *rect;
        }
        surface.grid_size = grid_size;
        surface
            .terminal
            .lock()
            .expect("terminal mutex poisoned")
            .resize(grid_size);
    }

    for action in plan {
        let PaneResizeAction::PtyResize(pane_id, grid_size) = action else {
            continue;
        };
        if let Some(surface) = state.surfaces.get(&pane_id) {
            let _ = surface.resize_tx.send(grid_size);
        }
    }
}

fn shutdown_pane_io_threads<'a>(surfaces: impl IntoIterator<Item = &'a mut Surface>) {
    for surface in surfaces {
        surface.shutdown();
    }
}

fn pane_bounds_for_size(size: PhysicalSize<u32>) -> PaneRectApp {
    PaneRectApp::new(0, 0, size.width, size.height)
}

fn can_split_rect(rect: PaneRectApp, orientation: SplitOrientation) -> bool {
    let required = MIN_PANE_SIZE_PX
        .saturating_mul(2)
        .saturating_add(split_tree::DIVIDER_WIDTH_PX);
    match orientation {
        SplitOrientation::Horizontal => rect.w >= required,
        SplitOrientation::Vertical => rect.h >= required,
    }
}

fn grid_size_for_pane_rect(
    rect: PaneRectApp,
    metrics: noa_font::Metrics,
    padding: GridPadding,
) -> GridSize {
    grid_size_for_physical_size(PhysicalSize::new(rect.w, rect.h), metrics, padding)
}

fn split_point_from_physical_position(
    position: PhysicalPosition<f64>,
) -> Option<split_tree::Point> {
    if !position.x.is_finite() || !position.y.is_finite() || position.x < 0.0 || position.y < 0.0 {
        return None;
    }
    Some(split_tree::Point::new(
        position.x.floor().min(f64::from(u32::MAX)) as u32,
        position.y.floor().min(f64::from(u32::MAX)) as u32,
    ))
}

fn render_pane_id(pane_id: PaneId) -> RenderPaneId {
    RenderPaneId::new(pane_id.get())
}

fn render_pane_rect(rect: PaneRectApp) -> PaneRect {
    PaneRect::new(rect.x, rect.y, rect.w, rect.h)
}

fn visible_pane_ids(tree: &SplitTree, zoomed: Option<PaneId>) -> Vec<PaneId> {
    split_tree::zoom_decision(tree, zoomed, PaneRectApp::new(0, 0, 0, 0)).draw_panes
}

fn tab_title(title: &str) -> String {
    if title.is_empty() {
        "noa".to_string()
    } else {
        title.to_string()
    }
}

fn apply_terminal_action(terminal: &mut Terminal, action: TerminalAction) {
    match action {
        TerminalAction::Clear => terminal.clear_active_display_and_scrollback(),
        TerminalAction::ClearScrollback => terminal.clear_scrollback(),
        TerminalAction::SelectAll => terminal.select_all(),
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

fn pane_user_event_redraw_decision(pane_state: Option<(bool, bool)>) -> TargetedRedrawDecision {
    let Some((pane_exists, occluded)) = pane_state else {
        return TargetedRedrawDecision::Stale;
    };
    targeted_redraw_decision(pane_exists, occluded)
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
        | AppCommand::Terminal(_)
        | AppCommand::FontSize(_)
        | AppCommand::Search(_)
        | AppCommand::ScrollViewport(_)
        | AppCommand::NewSplitRight
        | AppCommand::NewSplitDown
        | AppCommand::FocusDirection(_)
        | AppCommand::ResizeSplit(_)
        | AppCommand::EqualizeSplits
        | AppCommand::ToggleSplitZoom
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

fn preferred_surface_alpha_mode(caps: &wgpu::SurfaceCapabilities) -> wgpu::CompositeAlphaMode {
    if caps.alpha_modes.contains(&wgpu::CompositeAlphaMode::Opaque) {
        wgpu::CompositeAlphaMode::Opaque
    } else {
        caps.alpha_modes
            .first()
            .copied()
            .unwrap_or(wgpu::CompositeAlphaMode::Auto)
    }
}

fn focus_report_bytes(focused: bool, focus_reporting: bool) -> Option<&'static [u8]> {
    if !focus_reporting {
        return None;
    }
    if focused {
        Some(b"\x1b[I")
    } else {
        Some(b"\x1b[O")
    }
}

fn font_pixel_size(point_size: f32, scale_factor: f64) -> f32 {
    (point_size * scale_factor.max(f64::EPSILON) as f32).max(1.0)
}

fn initial_window_logical_size(
    metrics: noa_font::Metrics,
    grid_size: GridSize,
    scale_factor: f64,
    padding: GridPadding,
) -> LogicalSize<f64> {
    let scale_factor = scale_factor.max(f64::EPSILON) as f32;
    let physical_w = (metrics.cell_w * grid_size.cols as f32 + padding.horizontal())
        .ceil()
        .max(1.0);
    let physical_h = (metrics.cell_h * grid_size.rows as f32 + padding.vertical())
        .ceil()
        .max(1.0);

    LogicalSize::new(
        (physical_w / scale_factor) as f64,
        (physical_h / scale_factor) as f64,
    )
}

fn grid_size_for_physical_size(
    size: PhysicalSize<u32>,
    metrics: noa_font::Metrics,
    padding: GridPadding,
) -> GridSize {
    let content_width = (size.width as f32 - padding.horizontal()).max(0.0);
    let content_height = (size.height as f32 - padding.vertical()).max(0.0);
    let cols = (content_width / metrics.cell_w.max(f32::EPSILON))
        .floor()
        .clamp(1.0, u16::MAX as f32) as u16;
    let rows = (content_height / metrics.cell_h.max(f32::EPSILON))
        .floor()
        .clamp(1.0, u16::MAX as f32) as u16;
    GridSize::new(cols, rows)
}

fn update_ime_cursor_area(
    window: &Window,
    metrics: noa_font::Metrics,
    x: u16,
    y: u16,
    pane_rect: PaneRectApp,
    padding: GridPadding,
) {
    let (position, size) = ime_cursor_area(metrics, x, y, pane_rect, padding);
    window.set_ime_cursor_area(position, size);
}

fn ime_cursor_area(
    metrics: noa_font::Metrics,
    x: u16,
    y: u16,
    pane_rect: PaneRectApp,
    padding: GridPadding,
) -> (PhysicalPosition<i32>, PhysicalSize<u32>) {
    let position = PhysicalPosition::new(
        (pane_rect.x as f32 + padding.left + metrics.cell_w * x as f32)
            .round()
            .max(0.0) as i32,
        (pane_rect.y as f32 + padding.top + metrics.cell_h * y as f32)
            .round()
            .max(0.0) as i32,
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
        let size = initial_window_logical_size(
            metrics(16.0, 32.0),
            GridSize::new(80, 24),
            2.0,
            DEFAULT_GRID_PADDING,
        );

        assert_eq!(size.width, 656.0);
        assert_eq!(size.height, 392.0);
    }

    #[test]
    fn surface_alpha_mode_prefers_opaque_to_keep_terminal_colors_solid() {
        let caps = wgpu::SurfaceCapabilities {
            alpha_modes: vec![
                wgpu::CompositeAlphaMode::PreMultiplied,
                wgpu::CompositeAlphaMode::Opaque,
            ],
            ..Default::default()
        };

        assert_eq!(
            preferred_surface_alpha_mode(&caps),
            wgpu::CompositeAlphaMode::Opaque
        );
    }

    #[test]
    fn surface_alpha_mode_falls_back_when_opaque_is_unavailable() {
        let caps = wgpu::SurfaceCapabilities {
            alpha_modes: vec![wgpu::CompositeAlphaMode::Inherit],
            ..Default::default()
        };

        assert_eq!(
            preferred_surface_alpha_mode(&caps),
            wgpu::CompositeAlphaMode::Inherit
        );
    }

    #[test]
    fn scale_factor_grid_recompute_uses_new_cell_metrics() {
        let size = PhysicalSize::new(968, 600);

        assert_eq!(
            grid_size_for_physical_size(size, metrics(12.0, 24.0), DEFAULT_GRID_PADDING),
            GridSize::new(78, 24)
        );
        assert_eq!(
            grid_size_for_physical_size(size, metrics(16.0, 30.0), DEFAULT_GRID_PADDING),
            GridSize::new(58, 19)
        );
        assert_eq!(
            grid_size_for_physical_size(
                PhysicalSize::new(1, 1),
                metrics(16.0, 30.0),
                DEFAULT_GRID_PADDING,
            ),
            GridSize::new(1, 1)
        );
    }

    #[test]
    fn runtime_font_size_actions_adjust_and_reset_to_startup_size() {
        assert_eq!(
            runtime_font_size_update(15.0, 15.0, FontSizeAction::Increase),
            RuntimeFontSizeUpdate {
                point_size: 16.0,
                changed: true
            }
        );
        assert_eq!(
            runtime_font_size_update(15.0, 15.0, FontSizeAction::Decrease),
            RuntimeFontSizeUpdate {
                point_size: 14.0,
                changed: true
            }
        );
        assert_eq!(
            runtime_font_size_update(18.0, 15.0, FontSizeAction::Reset),
            RuntimeFontSizeUpdate {
                point_size: 15.0,
                changed: true
            }
        );
        assert_eq!(
            runtime_font_size_update(15.0, 15.0, FontSizeAction::Reset),
            RuntimeFontSizeUpdate {
                point_size: 15.0,
                changed: false
            }
        );
    }

    #[test]
    fn runtime_font_size_actions_clamp_to_supported_range() {
        assert_eq!(
            runtime_font_size_update(96.0, 15.0, FontSizeAction::Increase),
            RuntimeFontSizeUpdate {
                point_size: 96.0,
                changed: false
            }
        );
        assert_eq!(
            runtime_font_size_update(6.0, 15.0, FontSizeAction::Decrease),
            RuntimeFontSizeUpdate {
                point_size: 6.0,
                changed: false
            }
        );
        assert_eq!(
            runtime_font_size_update(120.0, 15.0, FontSizeAction::Decrease),
            RuntimeFontSizeUpdate {
                point_size: 96.0,
                changed: true
            }
        );
        assert_eq!(
            runtime_font_size_update(f32::NAN, 15.0, FontSizeAction::Increase),
            RuntimeFontSizeUpdate {
                point_size: 6.0,
                changed: true
            }
        );
    }

    #[test]
    fn font_size_resize_plan_recomputes_each_window_grid_from_new_metrics() {
        let plan = font_size_resize_plan(
            [
                (1_u8, PhysicalSize::new(968, 600)),
                (2_u8, PhysicalSize::new(488, 300)),
            ],
            metrics(16.0, 30.0),
            DEFAULT_GRID_PADDING,
        );

        assert_eq!(
            plan,
            vec![(1, GridSize::new(58, 19)), (2, GridSize::new(28, 9))]
        );
    }

    #[test]
    fn ime_cursor_area_tracks_grid_cell_in_physical_pixels() {
        let (position, size) = ime_cursor_area(
            metrics(7.5, 15.25),
            2,
            3,
            PaneRectApp::new(0, 0, 100, 100),
            DEFAULT_GRID_PADDING,
        );

        assert_eq!(position.x, 31);
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
    fn terminal_clear_action_uses_grid_clear_api() {
        let mut terminal = terminal_with_scrollback(GridSize::new(5, 3));
        terminal.scroll_viewport_up(1);
        terminal.pending_writes.extend_from_slice(b"reply");

        apply_terminal_action(&mut terminal, TerminalAction::Clear);

        assert_eq!(terminal.scrollback_len(), 0);
        assert_eq!(terminal.viewport_offset(), 0);
        assert_eq!(terminal.pending_writes, b"reply");
    }

    #[test]
    fn terminal_clear_scrollback_action_preserves_live_grid() {
        let mut terminal = terminal_with_scrollback(GridSize::new(5, 3));

        apply_terminal_action(&mut terminal, TerminalAction::ClearScrollback);

        assert_eq!(terminal.scrollback_len(), 0);
        assert_eq!(terminal.primary.grid[0].cells[0].ch, 'D');
        assert_eq!(terminal.primary.grid[1].cells[0].ch, 'E');
        assert_eq!(terminal.primary.grid[2].cells[0].ch, 'F');
    }

    #[test]
    fn terminal_select_all_action_uses_grid_selection_api() {
        let mut terminal = terminal_with_scrollback(GridSize::new(5, 3));

        apply_terminal_action(&mut terminal, TerminalAction::SelectAll);

        assert_eq!(
            terminal.selected_text().as_deref(),
            Some("A\nB\nC\nD\nE\nF")
        );
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
    fn stale_pane_user_event_redraw_decision_noops_without_panicking() {
        assert_eq!(
            pane_user_event_redraw_decision(None),
            TargetedRedrawDecision::Stale
        );
        assert_eq!(
            pane_user_event_redraw_decision(Some((false, false))),
            TargetedRedrawDecision::Stale
        );
        assert_eq!(
            pane_user_event_redraw_decision(Some((true, true))),
            TargetedRedrawDecision::Suppress
        );
        assert_eq!(
            pane_user_event_redraw_decision(Some((true, false))),
            TargetedRedrawDecision::Request
        );
    }

    #[test]
    fn multi_pane_resize_batching_resizes_all_grids_before_pty_winsize_sends() {
        let first = PaneId::new(1);
        let second = PaneId::new(2);
        let third = PaneId::new(3);

        let plan = pane_resize_batch_plan([
            (first, GridSize::new(40, 12)),
            (second, GridSize::new(41, 12)),
            (third, GridSize::new(80, 6)),
        ]);

        assert_eq!(
            plan,
            vec![
                PaneResizeAction::GridResize(first, GridSize::new(40, 12)),
                PaneResizeAction::GridResize(second, GridSize::new(41, 12)),
                PaneResizeAction::GridResize(third, GridSize::new(80, 6)),
                PaneResizeAction::PtyResize(first, GridSize::new(40, 12)),
                PaneResizeAction::PtyResize(second, GridSize::new(41, 12)),
                PaneResizeAction::PtyResize(third, GridSize::new(80, 6)),
            ]
        );
    }

    #[test]
    fn focus_reporting_encodes_csi_i_and_csi_o_only_when_enabled() {
        assert_eq!(focus_report_bytes(true, true), Some(b"\x1b[I".as_slice()));
        assert_eq!(focus_report_bytes(false, true), Some(b"\x1b[O".as_slice()));
        assert_eq!(focus_report_bytes(true, false), None);
        assert_eq!(focus_report_bytes(false, false), None);
    }

    #[test]
    fn command_target_resolution_uses_focused_tab_only_for_terminal_commands() {
        let focused = Some(42_u8);
        for command in [
            AppCommand::Copy,
            AppCommand::Paste,
            AppCommand::Terminal(TerminalAction::Clear),
            AppCommand::Terminal(TerminalAction::ClearScrollback),
            AppCommand::Terminal(TerminalAction::SelectAll),
            AppCommand::FontSize(FontSizeAction::Increase),
            AppCommand::FontSize(FontSizeAction::Decrease),
            AppCommand::FontSize(FontSizeAction::Reset),
            AppCommand::Search(SearchAction::Find),
            AppCommand::Search(SearchAction::FindNext),
            AppCommand::Search(SearchAction::FindPrevious),
            AppCommand::Search(SearchAction::Clear),
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
