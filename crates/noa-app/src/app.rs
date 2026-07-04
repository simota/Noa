//! The winit [`ApplicationHandler`] — owns native windows/tabs, per-tab
//! terminal sessions, and the shared GPU/font state used to render them.
//!
//! Rendering + presentation happens on the winit main thread (macOS requires
//! presenting on the thread that owns the window). Each io thread owns one
//! PTY, touches only its tab's `Terminal` mutex, and posts targeted user
//! events back to the main loop.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crossbeam_channel::{Sender, TrySendError};
use noa_core::{DEFAULT_GRID_PADDING, GridPadding, GridSize, PixelSize, Point};
use noa_font::FontGrid;
use noa_grid::{CursorStyle, PromptJump, Terminal, modes::MouseTracking};
use noa_pty::{Pty, PtyConfig};
use noa_render::{
    CommandPaletteSnapshot, FrameSnapshot, HoverLink, OverviewThumbnailResources, PaneFrame,
    PaneId as RenderPaneId, PaneRect, Renderer, Theme,
};
use noa_vt::Stream;
use winit::application::ApplicationHandler;
use winit::dpi::{LogicalSize, PhysicalPosition, PhysicalSize};
use winit::event::{ElementState, Ime, KeyEvent, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoopProxy};
use winit::keyboard::{Key, ModifiersState, NamedKey};
#[cfg(target_os = "macos")]
use winit::platform::macos::{WindowAttributesExtMacOS, WindowExtMacOS};
use winit::window::{CursorIcon, Window, WindowAttributes, WindowId};

use crate::clipboard::{self, PasteContents, SystemClipboard};
use crate::command_palette::{self, CommandPalette};
use crate::commands::{FontSizeAction, KeybindEngine, SearchAction, TerminalAction};
use crate::events::UserEvent;
use crate::input;
use crate::link_open;
use crate::mouse::{self, MouseSelectionState, SelectionGesture};
use crate::search_prompt::{SearchPrompt, SearchPromptEffect};
use crate::split_tree::{
    self, Direction, HitTarget, ImeOp, MIN_PANE_SIZE_PX, PaneId, Rect as PaneRectApp,
    SPLIT_RESIZE_STEP_PX, SplitOrientation, SplitResizeDrag, SplitTree, equalize,
    focus_in_direction, focus_switch_plan, hit_test, resize_split, resize_split_to_drag_point,
    split_pane, split_resize_drag_target_at_point, zoom_resize_targets, zoom_toggle,
};
use crate::tab_overview::{
    OVERVIEW_GRID_CAP, OVERVIEW_MAX_RENDER_TILES_PER_FRAME, OVERVIEW_TILE_MIN_RENDER_INTERVAL,
    OverviewLayout, OverviewRenderCandidate, compute_overview_grid, hit_test_overview_grid,
    overview_placeholder_source_ids, overview_tile_labels, sanitize_placeholder_label,
    select_due_overview_tile_ids,
};
use crate::{AppCommand, ViewportScroll};

/// Configuration the binary passes into [`crate::run`].
#[derive(Debug, Clone)]
pub struct AppConfig {
    pub cols: u16,
    pub rows: u16,
    pub font_size: f32,
    pub theme: Option<String>,
    /// Parsed font settings from `noa-config` (ADR-R1: a distinct type from
    /// `noa_font::FontConfig` — mapped to it via [`font_config_from_noa_config`]
    /// right before each `FontGrid::new` call, keeping `noa-font` free of any
    /// `noa-config`/`dirs` dependency).
    pub font: noa_config::FontConfig,
    /// OSC 52 clipboard read (query) policy.
    pub clipboard_read: noa_config::ClipboardAccess,
    /// Whether to confirm before pasting content that could run commands.
    pub clipboard_paste_protection: bool,
    /// `window-padding-x/y`: `None` keeps the built-in default for that axis.
    /// Resolved to a `GridPadding` once in [`App::new`].
    pub window_padding_x: Option<f32>,
    pub window_padding_y: Option<f32>,
    /// Theme color overrides (`background`, `foreground`, `cursor-color`,
    /// `selection-foreground`, `selection-background`).
    pub background: Option<noa_core::Rgb>,
    pub foreground: Option<noa_core::Rgb>,
    pub cursor_color: Option<noa_core::Rgb>,
    pub selection_foreground: Option<noa_core::Rgb>,
    pub selection_background: Option<noa_core::Rgb>,
    /// `cursor-style` shape and `cursor-style-blink` toggle.
    pub cursor_style: Option<noa_config::CursorShape>,
    pub cursor_style_blink: Option<bool>,
    /// `background-opacity`, clamped to `0.0..=1.0`. Drives window
    /// transparency: below 1.0 the window is created transparent, a
    /// non-Opaque surface alpha mode is chosen, and the renderer scales its
    /// clear-color alpha to match.
    pub background_opacity: f32,
    /// `background-blur-radius` in points (`0..=64`, 0 = off). Applied as a
    /// native macOS window background blur; a no-op on other platforms.
    pub background_blur_radius: u16,
}

/// Maps the parsed `noa-config` font settings onto the `noa-font` runtime
/// config consumed by `FontGrid::new` (ADR-R1). WP0 only threads the values
/// through; later WPs make more of them observably load-bearing.
fn font_config_from_noa_config(cfg: &noa_config::FontConfig) -> noa_font::FontConfig {
    let default = noa_font::FontConfig::default();
    let synthetic_style = match cfg.synthetic_style {
        None | Some(noa_config::SyntheticStyleMode::Both) => default.synthetic_style,
        Some(noa_config::SyntheticStyleMode::Neither) => noa_font::SyntheticStyle {
            bold: false,
            italic: false,
        },
        Some(noa_config::SyntheticStyleMode::NoBold) => noa_font::SyntheticStyle {
            bold: false,
            italic: true,
        },
        Some(noa_config::SyntheticStyleMode::NoItalic) => noa_font::SyntheticStyle {
            bold: true,
            italic: false,
        },
    };
    let alpha_blending = match cfg.alpha_blending {
        None | Some(noa_config::AlphaBlendingMode::Native) => noa_font::AlphaBlending::Native,
        Some(
            noa_config::AlphaBlendingMode::Linear | noa_config::AlphaBlendingMode::LinearCorrected,
        ) => noa_font::AlphaBlending::LinearFallback,
    };

    noa_font::FontConfig {
        families: cfg.families.clone(),
        families_bold: cfg.families_bold.clone(),
        families_italic: cfg.families_italic.clone(),
        families_bold_italic: cfg.families_bold_italic.clone(),
        features: cfg
            .features
            .iter()
            .map(|feature| noa_font::FontFeature {
                tag: feature.tag,
                enabled: feature.enabled,
            })
            .collect(),
        variations: map_font_variations(&cfg.variations),
        variations_bold: map_font_variations(&cfg.variations_bold),
        variations_italic: map_font_variations(&cfg.variations_italic),
        variations_bold_italic: map_font_variations(&cfg.variations_bold_italic),
        synthetic_style,
        alpha_blending,
        thicken: cfg.thicken.unwrap_or(default.thicken),
        thicken_strength: cfg.thicken_strength.unwrap_or(default.thicken_strength),
    }
}

fn map_font_variations(variations: &[noa_config::FontVariation]) -> Vec<noa_font::FontVariation> {
    variations
        .iter()
        .map(|variation| noa_font::FontVariation {
            tag: variation.tag,
            value: variation.value,
        })
        .collect()
}

/// Derive the grid padding from `window-padding-x/y`. An unset axis keeps the
/// corresponding edge(s) of [`DEFAULT_GRID_PADDING`]; a set axis applies its
/// value to both edges of that axis.
fn resolve_grid_padding(x: Option<f32>, y: Option<f32>) -> GridPadding {
    let default = DEFAULT_GRID_PADDING;
    GridPadding {
        top: y.unwrap_or(default.top),
        right: x.unwrap_or(default.right),
        bottom: y.unwrap_or(default.bottom),
        left: x.unwrap_or(default.left),
    }
}

/// Map `cursor-style` + `cursor-style-blink` onto a grid [`CursorStyle`].
/// Returns `None` when neither key is set, so the terminal keeps its own
/// default (Ghostty's blinking block). When only the blink toggle is set the
/// shape defaults to block; when only the shape is set it defaults to blinking.
fn resolve_cursor_style(
    shape: Option<noa_config::CursorShape>,
    blink: Option<bool>,
) -> Option<CursorStyle> {
    if shape.is_none() && blink.is_none() {
        return None;
    }
    let shape = shape.unwrap_or(noa_config::CursorShape::Block);
    let blinking = blink.unwrap_or(true);
    Some(match (shape, blinking) {
        (noa_config::CursorShape::Block, true) => CursorStyle::BlinkingBlock,
        (noa_config::CursorShape::Block, false) => CursorStyle::SteadyBlock,
        (noa_config::CursorShape::Bar, true) => CursorStyle::BlinkingBar,
        (noa_config::CursorShape::Bar, false) => CursorStyle::SteadyBar,
        (noa_config::CursorShape::Underline, true) => CursorStyle::BlinkingUnderline,
        (noa_config::CursorShape::Underline, false) => CursorStyle::SteadyUnderline,
    })
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
    last_mouse_point: Option<split_tree::Point>,
    active_split_drag: Option<SplitResizeDrag>,
    occluded: bool,
    title: String,
    /// Set when a left press was consumed by Cmd+click-to-open, so only the
    /// matching release is swallowed. Gating the release on "is a link
    /// still hovered" instead would eat the release of an unrelated
    /// selection drag or SGR-reported press whenever Cmd happens to be held
    /// over a link at mouse-up, desyncing those state machines.
    link_click_in_flight: bool,
}

/// State for the dedicated overview window. It deliberately is not part of
/// `windows`/`window_order`, which are terminal-tab collections.
struct OverviewWindowState {
    window: Arc<Window>,
    occluded: bool,
    last_cursor_point: Option<split_tree::Point>,
    surface: wgpu::Surface<'static>,
    surface_config: wgpu::SurfaceConfiguration,
    /// Shared scratch + per-tile textures (REQ-NF-3), sized for every live
    /// mirror tile *and* every title-only placeholder tile (REQ-OV-10).
    /// Rebuilt lazily in `redraw_overview` whenever the grid layout or
    /// surface size changes; `None` while there are zero tabs to show.
    thumbnails: Option<OverviewThumbnailResources>,
    /// Single small `Renderer` dedicated to drawing placeholder-row title
    /// text (REQ-OV-10). Reused across every placeholder tile and frame —
    /// this is not a per-tab renderer, so it doesn't violate REQ-NF-1.
    label_renderer: Option<Renderer>,
}

#[derive(Clone, Copy, Debug, Default)]
struct OverviewTileRenderState {
    dirty: bool,
    last_render_at: Option<Instant>,
}

/// An open search prompt (Cmd+F), scoped to the window/pane it was opened
/// for. Only one can be open at a time app-wide (opening a second one is a
/// no-op — see `App::handle_search_action`'s `SearchAction::Find` arm); the
/// `KeyboardInput` handler routes every keystroke in `window_id` here
/// instead of the normal keybind-resolve/pty-encode path while it is open.
struct SearchPromptSession {
    window_id: WindowId,
    pane_id: PaneId,
    prompt: SearchPrompt,
}

/// An open command palette (`cmd+shift+p`), bound to the window it was opened
/// from. Only one exists at a time app-wide (`App::toggle_command_palette`).
/// Unlike [`SearchPromptSession`] it carries **no `pane_id`**: the palette's
/// commands re-resolve their own target at dispatch (R-10), so it is window-
/// bound only — which also simplifies leak cleanup to a single `close_tab`
/// site (R-11). The `KeyboardInput` handler routes every keystroke in
/// `window_id` to [`App::handle_command_palette_key`] while it is open.
struct CommandPaletteSession {
    window_id: WindowId,
    palette: CommandPalette,
}

/// An open confirmation dialog (paste protection or OSC 52 clipboard-read),
/// bound to the window it was raised from. Fully modal: while it is up the
/// `KeyboardInput` handler routes every keystroke in `window_id` to
/// [`App::handle_confirm_dialog_key`] — Enter/`y` confirms, Escape/`n`
/// cancels. Only one exists at a time app-wide.
struct ConfirmDialogSession {
    window_id: WindowId,
    message: String,
    hint: String,
    action: ConfirmAction,
}

/// The deferred side effect a [`ConfirmDialogSession`] runs on confirmation.
enum ConfirmAction {
    /// Send already-encoded paste bytes to the pane's pty.
    Paste {
        window_id: WindowId,
        pane_id: PaneId,
        bytes: Vec<u8>,
    },
    /// Fulfill an OSC 52 clipboard read: read the clipboard now and write the
    /// base64 reply to the pane's pty.
    ClipboardRead {
        window_id: WindowId,
        pane_id: PaneId,
        target: String,
    },
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
    /// The Cmd+hover underline target for this pane, recomputed on every
    /// `CursorMoved`/`ModifiersChanged` (`App::sync_hover_link`) and fed
    /// into `FrameSnapshot::hover_link` at redraw.
    hover_link: Option<HoverLink>,
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

/// DECSCUSR `Blinking*` cursor styles toggle visibility on this interval
/// while focused and displayable. Matches common terminal defaults
/// (Ghostty/iTerm2 ballpark); not user-configurable yet.
const CURSOR_BLINK_INTERVAL: Duration = Duration::from_millis(600);

pub struct App {
    config: AppConfig,
    /// Grid padding derived once from `window-padding-x/y`, applied to every
    /// pane's geometry.
    padding: GridPadding,
    /// Initial cursor style from `cursor-style` / `cursor-style-blink`, applied
    /// to each terminal at creation. `None` keeps the terminal default.
    initial_cursor_style: Option<CursorStyle>,
    runtime_font_size: f32,
    proxy: EventLoopProxy<UserEvent>,
    gpu: Option<GpuState>,
    windows: HashMap<WindowId, WindowState>,
    window_order: Vec<WindowId>,
    overview_window: Option<OverviewWindowState>,
    overview_tiles: HashMap<WindowId, OverviewTileRenderState>,
    focused: Option<WindowId>,
    #[cfg(target_os = "macos")]
    macos_menu: Option<crate::macos_menu::MacosMenu>,
    #[cfg(target_os = "macos")]
    tab_group_identifier: String,
    modifiers: ModifiersState,
    clipboard: SystemClipboard,
    keybinds: KeybindEngine,
    overview_visible: bool,
    /// Current blink-timer phase for the focused pane's cursor, fed into
    /// `FrameSnapshot::cursor_blink_visible` on every redraw. Toggled by
    /// `tick_cursor_blink` and snapped back to `true` on keyboard input.
    cursor_blink_visible: bool,
    /// Next scheduled blink toggle; `None` while no focused pane has a
    /// displayable `Blinking*` cursor (the event loop then sits at
    /// `ControlFlow::Wait`, no busy wake-ups).
    cursor_blink_deadline: Option<Instant>,
    /// The `(window, pane)` currently carrying a non-`None` `Surface::hover_link`,
    /// if any — tracked so [`App::sync_hover_link`] can clear it when the
    /// mouse moves to a different pane/window (or off any pane) without
    /// having to scan every surface.
    hovered_link: Option<(WindowId, PaneId)>,
    /// The open search prompt (Cmd+F), if any — see [`SearchPromptSession`].
    search_prompt: Option<SearchPromptSession>,
    /// The open command palette (`cmd+shift+p`), if any — see
    /// [`CommandPaletteSession`].
    command_palette: Option<CommandPaletteSession>,
    /// The open confirmation dialog (paste protection / clipboard-read), if
    /// any — see [`ConfirmDialogSession`].
    confirm_dialog: Option<ConfirmDialogSession>,
}

impl App {
    pub fn new(config: AppConfig, proxy: EventLoopProxy<UserEvent>) -> Self {
        let padding = resolve_grid_padding(config.window_padding_x, config.window_padding_y);
        let initial_cursor_style =
            resolve_cursor_style(config.cursor_style, config.cursor_style_blink);
        App {
            padding,
            initial_cursor_style,
            runtime_font_size: config.font_size,
            config,
            proxy,
            gpu: None,
            windows: HashMap::new(),
            window_order: Vec::new(),
            overview_window: None,
            overview_tiles: HashMap::new(),
            focused: None,
            #[cfg(target_os = "macos")]
            macos_menu: None,
            #[cfg(target_os = "macos")]
            tab_group_identifier: format!("noa.tabs.{}", std::process::id()),
            modifiers: ModifiersState::empty(),
            clipboard: SystemClipboard::new(),
            keybinds: KeybindEngine::default(),
            overview_visible: false,
            cursor_blink_visible: true,
            cursor_blink_deadline: None,
            hovered_link: None,
            search_prompt: None,
            command_palette: None,
            confirm_dialog: None,
        }
    }

    fn theme_overrides(&self) -> crate::theme::ThemeOverrides {
        crate::theme::ThemeOverrides {
            background: self.config.background,
            foreground: self.config.foreground,
            cursor: self.config.cursor_color,
            selection_fg: self.config.selection_foreground,
            selection_bg: self.config.selection_background,
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
            let mut term = surface.terminal.lock().expect("terminal mutex poisoned");
            if pane_id == state.focused_pane {
                title = tab_title(&term.title);
            }
            let mut snapshot = FrameSnapshot::from_terminal(&mut term);
            // A pane draws a solid cursor only when it is both the split's
            // focused pane AND its window has OS focus; otherwise (an
            // inactive split pane, or any pane in an unfocused window) it
            // draws the hollow outline instead of hiding the cursor outright.
            snapshot.focused = pane_id == state.focused_pane && self.focused == Some(window_id);
            snapshot.cursor_blink_visible = self.cursor_blink_visible;
            snapshot.hover_link = surface.hover_link;
            snapshot.search_prompt = self
                .search_prompt
                .as_ref()
                .filter(|session| session.window_id == window_id && session.pane_id == pane_id)
                .map(|session| session.prompt.buffer().to_string());
            // C3: draw the window-bound palette exactly once — over the
            // focused pane — rather than once per visible split.
            snapshot.command_palette = self
                .command_palette
                .as_ref()
                .filter(|session| session.window_id == window_id && pane_id == state.focused_pane)
                .map(|session| command_palette_snapshot(&self.keybinds, &session.palette));
            // Like the palette: draw the window-bound confirm dialog once,
            // over the focused pane.
            snapshot.confirm_dialog = self
                .confirm_dialog
                .as_ref()
                .filter(|session| session.window_id == window_id && pane_id == state.focused_pane)
                .map(|session| noa_render::ConfirmDialogSnapshot {
                    message: session.message.clone(),
                    hint: session.hint.clone(),
                });
            snapshots.push((pane_id, surface.rect, snapshot));
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
                self.padding,
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
            Some(render_pane_id(state.focused_pane)),
            state.zoomed.map(render_pane_id),
        );
        frame.present();
    }

    /// Whether the currently OS-focused window's focused pane has a
    /// displayable `Blinking*` cursor (DECTCEM on, viewport not scrolled
    /// away from the live cursor). Drives whether [`App::tick_cursor_blink`]
    /// keeps the event loop on a `WaitUntil` timer at all.
    fn focused_cursor_wants_blink(&self) -> bool {
        let Some(window_id) = self.focused else {
            return false;
        };
        let Some(surface) = self
            .windows
            .get(&window_id)
            .and_then(WindowState::focused_surface)
        else {
            return false;
        };
        let terminal = surface.terminal.lock().expect("terminal mutex poisoned");
        let cursor = terminal.active().cursor;
        cursor.visible
            && terminal.viewport_offset() == 0
            && matches!(
                cursor.style,
                CursorStyle::BlinkingBlock
                    | CursorStyle::BlinkingUnderline
                    | CursorStyle::BlinkingBar
            )
    }

    /// Advance the cursor blink phase and keep the event loop's
    /// `ControlFlow` in lockstep with whether a blink timer is even needed
    /// right now — `WaitUntil` only while a blinking cursor is displayable,
    /// `Wait` (no wake-ups) otherwise. Called from `about_to_wait` every
    /// pass, so a style/focus/visibility change is picked up on the very
    /// next event instead of waiting for a stale deadline.
    fn tick_cursor_blink(&mut self, event_loop: &ActiveEventLoop) {
        if !self.focused_cursor_wants_blink() {
            self.cursor_blink_visible = true;
            self.cursor_blink_deadline = None;
            event_loop.set_control_flow(ControlFlow::Wait);
            return;
        }

        let now = Instant::now();
        let deadline = *self
            .cursor_blink_deadline
            .get_or_insert(now + CURSOR_BLINK_INTERVAL);
        if now < deadline {
            event_loop.set_control_flow(ControlFlow::WaitUntil(deadline));
            return;
        }

        self.cursor_blink_visible = !self.cursor_blink_visible;
        let next = now + CURSOR_BLINK_INTERVAL;
        self.cursor_blink_deadline = Some(next);
        if let Some(window_id) = self.focused
            && let Some(state) = self.windows.get(&window_id)
        {
            state.window.request_redraw();
        }
        event_loop.set_control_flow(ControlFlow::WaitUntil(next));
    }

    fn handle_app_command(&mut self, event_loop: &ActiveEventLoop, command: AppCommand) {
        if self.overview_visible && overview_command_scope(command) == CommandScope::Overview {
            return;
        }
        // C1 (FM1): dispatching any command means leaving the palette. Close
        // it here so a command routed around the palette's own Enter path —
        // notably a menu-bar click while the palette is open — can't leave
        // two modals owning the keyboard. Idempotent with the Enter-path
        // close; skipped for the toggle itself so re-pressing still works.
        if command != AppCommand::ToggleCommandPalette {
            self.command_palette = None;
        }
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
            AppCommand::ToggleTabOverview => self.toggle_tab_overview(event_loop),
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
            AppCommand::ToggleCommandPalette => self.toggle_command_palette(),
            AppCommand::CloseWindow | AppCommand::Quit => event_loop.exit(),
        }
    }

    /// Toggle the single app-wide command palette (R-5). Opening binds it to
    /// the focused window with an empty query and every entry shown;
    /// re-firing while open closes it. A no-op when there is no focused
    /// window to bind to.
    fn toggle_command_palette(&mut self) {
        if self.command_palette.is_some() {
            self.command_palette = None;
        } else if let Some(window_id) = self.focused {
            self.command_palette = Some(CommandPaletteSession {
                window_id,
                palette: CommandPalette::open(),
            });
        }
        if let Some(window_id) = self.focused
            && let Some(state) = self.windows.get(&window_id)
        {
            state.window.request_redraw();
        }
    }

    fn spawn_tab(&mut self, event_loop: &ActiveEventLoop) -> anyhow::Result<WindowId> {
        // Inherit the focused shell's cwd before `self.focused` is repointed
        // at the new tab below.
        let inherited_cwd = self.focused_pane_cwd();
        let initial_grid_size = GridSize::new(self.config.cols, self.config.rows);
        let monitor_scale_factor = event_loop
            .primary_monitor()
            .map(|monitor| monitor.scale_factor())
            .unwrap_or(1.0);

        let mut first_font = if self.gpu.is_none() {
            Some(
                FontGrid::new(
                    font_pixel_size(self.runtime_font_size, monitor_scale_factor),
                    font_config_from_noa_config(&self.config.font),
                )
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
            self.padding,
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
            *font = FontGrid::new(
                font_pixel_size(self.runtime_font_size, window_scale_factor),
                font_config_from_noa_config(&self.config.font),
            )
            .expect("failed to load a system monospace font");
            let inner_size = initial_window_logical_size(
                font.metrics(),
                initial_grid_size,
                window_scale_factor,
                self.padding,
            );
            let _ = window.request_inner_size(inner_size);
        }
        window.set_ime_allowed(true);
        crate::macos_blur::apply_background_blur(
            &window,
            self.config.background_blur_radius,
            self.config.background_opacity,
        );
        update_ime_cursor_area(
            &window,
            metrics,
            0,
            0,
            PaneRectApp::new(0, 0, 0, 0),
            self.padding,
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
                theme: crate::theme::resolve_theme_with_overrides(
                    self.config.theme.as_deref(),
                    &self.theme_overrides(),
                ),
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
            let surface_format = preferred_surface_format(&caps.formats);

            let size = window.inner_size();
            let surface_config = wgpu::SurfaceConfiguration {
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                format: surface_format,
                width: size.width.max(1),
                height: size.height.max(1),
                present_mode: wgpu::PresentMode::Fifo,
                desired_maximum_frame_latency: 2,
                alpha_mode: preferred_surface_alpha_mode(
                    &caps,
                    self.config.background_opacity < 1.0,
                ),
                view_formats: vec![],
            };
            surface.configure(&gpu.device, &surface_config);

            let mut renderer = Renderer::new(
                &gpu.device,
                &gpu.queue,
                surface_format,
                &mut gpu.font,
                self.padding,
            )
            .expect("failed to build the renderer");
            renderer.set_background_opacity(self.config.background_opacity);
            renderer.resize(PixelSize {
                w: surface_config.width,
                h: surface_config.height,
            });
            (surface_config, renderer)
        };

        let window_id = window.id();
        let initial_pane = PaneId::new(1);
        let initial_rect = PaneRectApp::new(0, 0, surface_config.width, surface_config.height);
        let initial_surface = self.spawn_pane_surface(
            window_id,
            initial_pane,
            initial_grid_size,
            initial_rect,
            inherited_cwd,
        )?;
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
                last_mouse_point: None,
                active_split_drag: None,
                occluded: false,
                title: "noa".to_string(),
                link_click_in_flight: false,
            },
        );
        self.window_order.push(window_id);
        self.mark_overview_tile_dirty(window_id);
        self.focused = Some(window_id);
        window.focus_window();
        self.request_overview_redraw();
        Ok(window_id)
    }

    fn tab_window_attributes(&self, inner_size: LogicalSize<f64>) -> WindowAttributes {
        let attrs = WindowAttributes::default()
            .with_title("noa")
            .with_inner_size(inner_size)
            // A transparent window is required for `background-opacity` to
            // reveal anything behind it; the surface alpha mode and the
            // renderer's clear alpha carry the actual opacity.
            .with_transparent(self.config.background_opacity < 1.0);
        #[cfg(target_os = "macos")]
        {
            attrs.with_tabbing_identifier(&self.tab_group_identifier)
        }
        #[cfg(not(target_os = "macos"))]
        {
            attrs
        }
    }

    fn overview_window_attributes(&self) -> WindowAttributes {
        WindowAttributes::default()
            .with_title("Tab Overview")
            .with_inner_size(LogicalSize::new(900.0, 600.0))
    }

    /// The working directory reported by a pane's shell over OSC 7, if it
    /// points at a directory that still exists locally. A new tab or split
    /// inherits it so it opens where the focused shell is (Ghostty parity).
    /// Stale or remote paths (which usually don't resolve locally) fall back
    /// to `None`, i.e. the process's own cwd.
    fn pane_cwd(&self, window_id: WindowId, pane_id: PaneId) -> Option<String> {
        let cwd = self
            .windows
            .get(&window_id)?
            .surfaces
            .get(&pane_id)?
            .terminal
            .lock()
            .expect("terminal mutex poisoned")
            .cwd
            .clone()?;
        std::path::Path::new(&cwd).is_dir().then_some(cwd)
    }

    /// cwd of the currently focused pane, for a newly spawned tab to inherit.
    fn focused_pane_cwd(&self) -> Option<String> {
        let window_id = self.focused?;
        let pane_id = self.windows.get(&window_id)?.focused_pane;
        self.pane_cwd(window_id, pane_id)
    }

    fn spawn_pane_surface(
        &self,
        window_id: WindowId,
        pane_id: PaneId,
        grid_size: GridSize,
        rect: PaneRectApp,
        cwd: Option<String>,
    ) -> anyhow::Result<Surface> {
        let pty_config = PtyConfig {
            size: grid_size,
            cwd,
            ..Default::default()
        };
        let pty = Pty::spawn(pty_config)?;
        let mut terminal = Terminal::new(grid_size);
        if let Some(style) = self.initial_cursor_style {
            terminal.set_default_cursor_style(style);
        }
        // A read (query) request is only queued when reads aren't fully
        // denied; the finer allow-vs-ask decision is made by the app layer
        // when a request arrives.
        terminal.osc52_policy.allow_read =
            self.config.clipboard_read != noa_config::ClipboardAccess::Deny;
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
            hover_link: None,
        })
    }

    fn close_tab(&mut self, event_loop: &ActiveEventLoop, window_id: WindowId) {
        let outcome = close_tab_outcome(
            &self.window_order,
            self.focused,
            window_id,
            self.overview_visible,
        );
        if outcome == TabCloseOutcome::Stale {
            return;
        }

        if let Some(mut state) = self.windows.remove(&window_id) {
            state.shutdown();
        }
        self.window_order.retain(|id| *id != window_id);
        self.overview_tiles.remove(&window_id);
        // A prompt targeting the closed window would otherwise linger
        // forever: its window can no longer deliver keys (so not even
        // Escape reaches it) and the open-guard would block every future
        // cmd+f app-wide.
        if self
            .search_prompt
            .as_ref()
            .is_some_and(|session| session.window_id == window_id)
        {
            self.search_prompt = None;
        }
        // C4 (R-11, FM3): the window-bound palette leaks the same way — a
        // closed window can deliver no keys (not even Escape), so a palette
        // still targeting it would strand a dead-window reference and the
        // toggle would never rebuild a fresh session. `close_pane` needs no
        // twin clear: the palette is window-bound, so a pane-only close
        // leaves it valid, and a whole-tab close always routes here.
        if self
            .command_palette
            .as_ref()
            .is_some_and(|session| session.window_id == window_id)
        {
            self.command_palette = None;
        }
        // Same leak shape as the palette: a confirm dialog bound to the closed
        // window could deliver no keys (not even Escape), stranding a modal.
        if self
            .confirm_dialog
            .as_ref()
            .is_some_and(|session| session.window_id == window_id)
        {
            self.confirm_dialog = None;
        }

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
                } else if self.overview_visible {
                    self.focus_overview_window();
                }
                self.request_overview_redraw();
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
        let window =
            {
                let Some(state) = self.windows.get_mut(&window_id) else {
                    return;
                };
                if !state.contains_pane(pane_id) {
                    return;
                }

                if let Some(mut surface) = state.surfaces.remove(&pane_id) {
                    surface.shutdown();
                }
                if self.search_prompt.as_ref().is_some_and(|session| {
                    session.window_id == window_id && session.pane_id == pane_id
                }) {
                    // The prompt's target pane is gone; keeping the session
                    // would leave a modal prompt bound to a dead pane and
                    // block every future cmd+f behind the open-guard.
                    self.search_prompt = None;
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

        let grid_size = grid_size_for_pane_rect(focused_rect, gpu.font.metrics(), self.padding);
        let inherited_cwd = self.pane_cwd(window_id, focused_pane);
        let new_surface = match self.spawn_pane_surface(
            window_id,
            new_pane,
            grid_size,
            focused_rect,
            inherited_cwd,
        ) {
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

    fn toggle_tab_overview(&mut self, event_loop: &ActiveEventLoop) {
        if let Some(next_visible) = tab_overview_visibility_after_dispatch(
            AppCommand::ToggleTabOverview,
            self.overview_visible,
        ) {
            if next_visible {
                self.show_tab_overview(event_loop);
            } else {
                self.hide_tab_overview();
            }
        }
    }

    fn show_tab_overview(&mut self, event_loop: &ActiveEventLoop) {
        if self.overview_window.is_none() {
            let window = Arc::new(
                event_loop
                    .create_window(self.overview_window_attributes())
                    .expect("failed to create Tab Overview window"),
            );
            window.set_ime_allowed(false);

            // The overview window only ever opens once a tab already exists
            // (it is reachable only via a keybind/menu/command dispatched to
            // a live tab), so GPU state is always initialized here.
            let gpu = self
                .gpu
                .as_ref()
                .expect("gpu initialized before overview window opens");
            let surface = gpu
                .instance
                .create_surface(window.clone())
                .expect("failed to create wgpu overview surface");
            let caps = surface.get_capabilities(&gpu.adapter);
            let surface_format = preferred_surface_format(&caps.formats);
            let size = window.inner_size();
            let surface_config = wgpu::SurfaceConfiguration {
                // COPY_DST (in addition to the RENDER_ATTACHMENT every
                // surface needs) lets `present_overview_frame` composite
                // per-tab tile textures directly via `copy_texture_to_texture`
                // instead of a second blit pass.
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_DST,
                format: surface_format,
                width: size.width.max(1),
                height: size.height.max(1),
                present_mode: wgpu::PresentMode::Fifo,
                desired_maximum_frame_latency: 2,
                // The overview window stays opaque: it composites tab tiles,
                // not live terminal background, so transparency would only
                // bleed the desktop through the switcher.
                alpha_mode: preferred_surface_alpha_mode(&caps, false),
                view_formats: vec![],
            };
            surface.configure(&gpu.device, &surface_config);

            self.overview_window = Some(OverviewWindowState {
                window,
                occluded: false,
                last_cursor_point: None,
                surface,
                surface_config,
                thumbnails: None,
                label_renderer: None,
            });
        }

        self.overview_visible = true;
        self.mark_all_overview_tiles_dirty();
        if let Some(overview) = self.overview_window.as_ref() {
            overview.window.set_visible(true);
            overview.window.focus_window();
            overview.window.request_redraw();
        }
    }

    fn hide_tab_overview(&mut self) {
        self.overview_visible = false;
        if let Some(overview) = self.overview_window.as_ref() {
            overview.window.set_visible(false);
        }
    }

    fn focus_overview_window(&self) {
        if let Some(overview) = self.overview_window.as_ref() {
            overview.window.focus_window();
        }
    }

    fn is_overview_window(&self, window_id: WindowId) -> bool {
        self.overview_window
            .as_ref()
            .is_some_and(|overview| overview.window.id() == window_id)
    }

    fn request_overview_redraw(&self) {
        let Some(overview) = self.overview_window.as_ref() else {
            return;
        };
        if self.overview_visible && !overview.occluded {
            overview.window.request_redraw();
        }
    }

    fn overview_window_occluded(&self) -> bool {
        self.overview_window
            .as_ref()
            .is_none_or(|overview| overview.occluded)
    }

    fn mark_overview_tile_dirty(&mut self, window_id: WindowId) {
        self.overview_tiles.entry(window_id).or_default().dirty = true;
    }

    fn mark_all_overview_tiles_dirty(&mut self) {
        for window_id in self.overview_source_window_ids() {
            self.mark_overview_tile_dirty(window_id);
        }
    }

    fn overview_redraw_decision_for_pane(
        &self,
        window_id: WindowId,
        pane_id: PaneId,
    ) -> TargetedRedrawDecision {
        overview_redraw_decision(
            self.windows
                .get(&window_id)
                .map(|state| (state.contains_pane(pane_id), state.occluded)),
            self.overview_visible,
            self.overview_window_occluded(),
        )
    }

    fn due_overview_tile_ids(&self, source_window_ids: &[WindowId], now: Instant) -> Vec<WindowId> {
        let candidates = source_window_ids
            .iter()
            .filter_map(|window_id| {
                self.windows.get(window_id)?;
                let tile = self
                    .overview_tiles
                    .get(window_id)
                    .copied()
                    .unwrap_or_default();
                Some(OverviewRenderCandidate {
                    id: *window_id,
                    dirty: tile.dirty,
                    last_render_at: tile.last_render_at,
                })
            })
            .collect::<Vec<_>>();

        select_due_overview_tile_ids(
            &candidates,
            now,
            OVERVIEW_TILE_MIN_RENDER_INTERVAL,
            OVERVIEW_MAX_RENDER_TILES_PER_FRAME,
        )
    }

    /// (Re)build the shared scratch + per-tile thumbnail textures (REQ-NF-3)
    /// whenever the grid layout, overview surface size, or surface format has
    /// drifted from what they were built for. Cheap to call every frame: the
    /// common case (nothing changed) is a handful of field comparisons.
    fn ensure_overview_thumbnails(&mut self, layout: &OverviewLayout) {
        let Some(gpu) = self.gpu.as_ref() else {
            return;
        };
        let Some(overview) = self.overview_window.as_mut() else {
            return;
        };

        // Placeholder tiles (REQ-OV-10) are the same uniform size as live
        // tiles (`rect_at` computes one `tile_w`/`tile_h` for the whole
        // grid), so they share this single texture pool; live tiles occupy
        // indices `[0, tiles.len())`, placeholders `[tiles.len(), tile_count)`.
        let tile_count = layout.tiles.len() + layout.placeholders.len();
        if tile_count == 0 {
            overview.thumbnails = None;
            return;
        }
        let tile_size = PixelSize {
            w: layout.tiles[0].w.max(1),
            h: layout.tiles[0].h.max(1),
        };
        let scratch_size = PixelSize {
            w: overview.surface_config.width,
            h: overview.surface_config.height,
        };
        let format = overview.surface_config.format;

        let stale = overview.thumbnails.as_ref().is_none_or(|thumbnails| {
            thumbnails.format() != format
                || thumbnails.scratch_size() != scratch_size
                || thumbnails.tile_size() != tile_size
                || thumbnails.tile_count() != tile_count
        });
        if stale {
            overview.thumbnails = Some(OverviewThumbnailResources::new(
                &gpu.device,
                format,
                scratch_size,
                tile_size,
                tile_count,
            ));
        }
    }

    /// Render each due tile's source tab into the shared scratch texture and
    /// blit it down into that tab's tile texture (REQ-OV-4 live mirror,
    /// REQ-NF-1 reuse the tab's own `Renderer`, REQ-NF-3 shared-scratch
    /// blit-downscale). `tile_index` is `source_window_ids`' position, which
    /// is index-parallel with `layout.tiles` (see `overview_tile_target_at_point`).
    fn render_due_overview_tiles(
        &mut self,
        due_window_ids: &[WindowId],
        source_window_ids: &[WindowId],
    ) {
        let Some(gpu) = self.gpu.as_mut() else {
            return;
        };
        let Some(overview) = self.overview_window.as_mut() else {
            return;
        };
        let Some(thumbnails) = overview.thumbnails.as_mut() else {
            return;
        };

        for &window_id in due_window_ids {
            let Some(tile_index) = source_window_ids.iter().position(|id| *id == window_id) else {
                continue;
            };
            let Some(state) = self.windows.get_mut(&window_id) else {
                continue;
            };
            let Some(surface) = state.surfaces.get(&state.focused_pane) else {
                continue;
            };
            let mut snapshot = {
                let mut term = surface.terminal.lock().expect("terminal mutex poisoned");
                FrameSnapshot::from_terminal(&mut term)
            };
            // Mirrors the "not the focused pane" convention `redraw()` already
            // uses for background panes within one window.
            snapshot.cursor.visible = false;

            // Reuse this tab's own `Renderer` unmodified (REQ-NF-1): point it
            // at the shared scratch resolution just long enough to draw one
            // frame into it, then restore its real surface viewport so the
            // tab's own next redraw is unaffected.
            let own_viewport = PixelSize {
                w: state.surface_config.width,
                h: state.surface_config.height,
            };
            state.renderer.resize(thumbnails.scratch_size());
            state
                .renderer
                .rebuild_cells(&snapshot, &mut gpu.font, &gpu.theme);
            state
                .renderer
                .sync_atlas(&gpu.device, &gpu.queue, &mut gpu.font);
            if let Err(err) = thumbnails.render_existing_renderer_to_tile(
                &gpu.device,
                &gpu.queue,
                &mut state.renderer,
                tile_index,
            ) {
                log::warn!("overview tile render failed for {window_id:?}: {err:#}");
            }
            state.renderer.resize(own_viewport);
        }
    }

    /// Lazily (re)build the dedicated placeholder-title `Renderer` (REQ-OV-10).
    /// A single instance is reused across every placeholder tile and frame —
    /// this does not create a per-tab renderer, so it doesn't conflict with
    /// REQ-NF-1's "reuse the tab's own `Renderer`" rule for live mirrors.
    fn ensure_overview_label_renderer(&mut self) {
        let Some(gpu) = self.gpu.as_mut() else {
            return;
        };
        let Some(overview) = self.overview_window.as_mut() else {
            return;
        };
        let format = overview.surface_config.format;
        let stale = overview
            .label_renderer
            .as_ref()
            .is_none_or(|renderer| renderer.target_format() != format);
        if stale {
            overview.label_renderer = Some(
                Renderer::new(&gpu.device, &gpu.queue, format, &mut gpu.font, self.padding)
                    .expect("failed to build the overview label renderer"),
            );
        }
    }

    /// Render title-only text into every placeholder-row tile (REQ-OV-10):
    /// tabs beyond the live tile cap get no live mirror, just their title.
    /// Cheap enough to redo on every redraw — placeholders only exist once
    /// tab count exceeds `OVERVIEW_GRID_CAP`, and each is a synthetic 1-row
    /// `Terminal`, not a real pty-backed one.
    fn render_overview_placeholder_labels(
        &mut self,
        source_window_ids: &[WindowId],
        layout: &OverviewLayout,
    ) {
        if layout.placeholders.is_empty() {
            return;
        }
        let live_count = layout.tiles.len();
        let overflow_ids = overview_placeholder_source_ids(source_window_ids, live_count);
        let labels = overview_tile_labels(overflow_ids, |id| {
            self.windows.get(&id).map(|state| state.title.clone())
        });

        self.ensure_overview_label_renderer();
        let Some(gpu) = self.gpu.as_mut() else {
            return;
        };
        let Some(overview) = self.overview_window.as_mut() else {
            return;
        };
        let (Some(label_renderer), Some(thumbnails)) = (
            overview.label_renderer.as_mut(),
            overview.thumbnails.as_mut(),
        ) else {
            return;
        };
        let metrics = gpu.font.metrics();

        for (index, label) in labels.iter().enumerate() {
            let Some(rect) = layout.placeholders.get(index) else {
                continue;
            };
            let tile_index = live_count + index;
            let tile_size = PixelSize {
                w: rect.w.max(1),
                h: rect.h.max(1),
            };
            let grid_size = grid_size_for_pane_rect(
                PaneRectApp::new(0, 0, rect.w, rect.h),
                metrics,
                self.padding,
            );

            let mut term = Terminal::new(GridSize::new(grid_size.cols, 1));
            let text = sanitize_placeholder_label(&label.label, grid_size.cols);
            Stream::new().feed(text.as_bytes(), &mut term);
            let mut snapshot = FrameSnapshot::from_terminal(&mut term);
            snapshot.cursor.visible = false;

            label_renderer.resize(tile_size);
            label_renderer.rebuild_cells(&snapshot, &mut gpu.font, &gpu.theme);
            label_renderer.sync_atlas(&gpu.device, &gpu.queue, &mut gpu.font);

            let Some(tile_texture) = thumbnails.tile_texture_for_test(tile_index) else {
                continue;
            };
            let view = tile_texture.create_view(&wgpu::TextureViewDescriptor::default());
            label_renderer.draw(&gpu.device, &gpu.queue, &view);
        }
    }

    /// Composite every live-mirror and placeholder-title tile texture into
    /// the overview surface and present it. Empty grid cells are left as the
    /// clear color.
    fn present_overview_frame(&mut self, layout: &OverviewLayout) {
        let Some(gpu) = self.gpu.as_ref() else {
            return;
        };
        let Some(overview) = self.overview_window.as_mut() else {
            return;
        };

        let frame = match overview.surface.get_current_texture() {
            Ok(frame) => frame,
            Err(wgpu::SurfaceError::Lost | wgpu::SurfaceError::Outdated) => {
                overview
                    .surface
                    .configure(&gpu.device, &overview.surface_config);
                overview.window.request_redraw();
                return;
            }
            Err(e) => {
                log::warn!("overview surface error: {e}");
                return;
            }
        };
        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());

        let mut encoder = gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("noa-overview-composite-encoder"),
            });
        {
            let _clear_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("noa-overview-clear-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
        }
        if let Some(thumbnails) = overview.thumbnails.as_ref() {
            let live_count = layout.tiles.len();
            let placeholder_tiles = layout
                .placeholders
                .iter()
                .enumerate()
                .map(|(index, rect)| (live_count + index, rect));
            for (tile_index, rect) in layout.tiles.iter().enumerate().chain(placeholder_tiles) {
                let Some(tile_texture) = thumbnails.tile_texture_for_test(tile_index) else {
                    continue;
                };
                composite_overview_tile(&mut encoder, tile_texture, &frame.texture, *rect);
            }
        }
        gpu.queue.submit(Some(encoder.finish()));
        frame.present();
    }

    fn finish_overview_tile_renders(&mut self, window_ids: &[WindowId], now: Instant) {
        for window_id in window_ids {
            let tile = self.overview_tiles.entry(*window_id).or_default();
            tile.dirty = false;
            tile.last_render_at = Some(now);
        }
    }

    fn overview_source_window_ids(&self) -> Vec<WindowId> {
        overview_tile_source_order(
            &self.window_order,
            |id| self.windows.contains_key(&id),
            None,
        )
    }

    fn redraw_overview(&mut self) {
        let Some(overview) = self.overview_window.as_ref() else {
            return;
        };
        if !self.overview_visible || overview.occluded {
            return;
        }

        let bounds = pane_bounds_for_size(overview.window.inner_size());
        let source_window_ids = self.overview_source_window_ids();
        let layout = compute_overview_grid(source_window_ids.len(), bounds, OVERVIEW_GRID_CAP);
        let now = Instant::now();
        let due_window_ids = self.due_overview_tile_ids(&source_window_ids, now);

        self.ensure_overview_thumbnails(&layout);
        self.render_due_overview_tiles(&due_window_ids, &source_window_ids);
        self.render_overview_placeholder_labels(&source_window_ids, &layout);
        self.present_overview_frame(&layout);

        self.finish_overview_tile_renders(&due_window_ids, now);

        // OVERVIEW_MAX_RENDER_TILES_PER_FRAME caps how many tiles one frame
        // regenerates, and idle tabs produce no pty output to trigger the
        // next frame — so keep requesting redraws until the dirty backlog
        // drains (a full 9-tile grid fills in ~5 frames). Stops as soon as
        // every source tile is clean; a dirty-but-throttled tile may re-run
        // this for up to OVERVIEW_TILE_MIN_RENDER_INTERVAL transiently.
        let backlog_remains = source_window_ids.iter().any(|window_id| {
            self.overview_tiles
                .get(window_id)
                .is_some_and(|tile| tile.dirty)
        });
        if backlog_remains {
            self.request_overview_redraw();
        }
    }

    fn focus_overview_tile_at_last_cursor(&mut self) {
        let Some(overview) = self.overview_window.as_ref() else {
            return;
        };
        let Some(point) = overview.last_cursor_point else {
            return;
        };

        let bounds = pane_bounds_for_size(overview.window.inner_size());
        let source_window_ids = self.overview_source_window_ids();
        let layout = compute_overview_grid(source_window_ids.len(), bounds, OVERVIEW_GRID_CAP);
        let Some(target) = overview_tile_target_at_point(&source_window_ids, &layout.tiles, point)
        else {
            return;
        };
        self.focus_tab_from_overview(target);
    }

    fn focus_tab_from_overview(&mut self, window_id: WindowId) {
        let Some(window) = self
            .windows
            .get(&window_id)
            .map(|state| state.window.clone())
        else {
            return;
        };
        self.focused = Some(window_id);
        window.focus_window();
    }

    fn overview_window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: WindowEvent,
    ) {
        if !self.is_overview_window(window_id) {
            return;
        }

        match event {
            WindowEvent::CloseRequested => self.hide_tab_overview(),
            WindowEvent::RedrawRequested => self.redraw_overview(),
            WindowEvent::Resized(size) => self.on_overview_resize(size),
            WindowEvent::CursorMoved { position, .. } => {
                let point = split_point_from_physical_position(position);
                if let Some(overview) = self.overview_window.as_mut() {
                    overview.last_cursor_point = point;
                }
            }
            WindowEvent::MouseInput { state, button, .. } => {
                if button == MouseButton::Left && state == ElementState::Pressed {
                    self.focus_overview_tile_at_last_cursor();
                }
            }
            WindowEvent::Occluded(occluded) => {
                if let Some(overview) = self.overview_window.as_mut() {
                    overview.occluded = occluded;
                    if !occluded && self.overview_visible {
                        overview.window.request_redraw();
                    }
                }
            }
            WindowEvent::ModifiersChanged(mods) => self.modifiers = mods.state(),
            WindowEvent::KeyboardInput { event, .. } => {
                if event.state != ElementState::Pressed {
                    return;
                }
                if let Some(command) = self.keybinds.resolve(&event.logical_key, self.modifiers) {
                    self.handle_app_command(event_loop, command);
                }
            }
            _ => {}
        }
    }
}

impl Drop for App {
    fn drop(&mut self) {
        self.overview_window.take();
        for state in self.windows.values_mut() {
            state.shutdown();
        }
        self.windows.clear();
        self.window_order.clear();
        self.focused = None;
        self.gpu.take();
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
            UserEvent::ClipboardRead {
                window_id,
                pane_id,
                target,
            } => {
                if !self
                    .windows
                    .get(&window_id)
                    .is_some_and(|state| state.contains_pane(pane_id))
                {
                    return;
                }
                match self.config.clipboard_read {
                    noa_config::ClipboardAccess::Allow => {
                        self.fulfill_clipboard_read(window_id, pane_id, &target);
                    }
                    noa_config::ClipboardAccess::Ask => {
                        self.prompt_clipboard_read(window_id, pane_id, target);
                    }
                    // The grid only queues reads when not denied; a Deny here
                    // would be a stale policy — ignore it.
                    noa_config::ClipboardAccess::Deny => {}
                }
            }
            UserEvent::Redraw(window_id, pane_id) => {
                let pane_state = self
                    .windows
                    .get(&window_id)
                    .map(|state| (state.contains_pane(pane_id), state.occluded));
                if pane_state.is_some_and(|(pane_exists, _)| pane_exists) {
                    self.mark_overview_tile_dirty(window_id);
                }
                let pane_decision = pane_user_event_redraw_decision(pane_state);
                let overview_decision = self.overview_redraw_decision_for_pane(window_id, pane_id);

                if pane_decision == TargetedRedrawDecision::Request
                    && let Some(state) = self.windows.get(&window_id)
                {
                    state.window.request_redraw();
                }
                if overview_decision == TargetedRedrawDecision::Request {
                    self.request_overview_redraw();
                }
            }
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
        if self.is_overview_window(window_id) {
            self.overview_window_event(event_loop, window_id, event);
            return;
        }
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
            WindowEvent::Focused(false) => {
                self.finish_active_split_drag(window_id);
                self.report_focus_event(window_id, false);
            }
            WindowEvent::Occluded(occluded) => {
                if let Some(state) = self.windows.get_mut(&window_id) {
                    state.occluded = occluded;
                    if !occluded {
                        state.window.request_redraw();
                    }
                }
            }
            WindowEvent::ModifiersChanged(mods) => {
                self.modifiers = mods.state();
                // Cmd pressed/released with the mouse stationary must still
                // toggle the hover underline + pointer cursor.
                self.sync_hover_link(window_id);
            }
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
                // Any keypress snaps the focused cursor back to its visible
                // blink phase and restarts the interval, matching common
                // terminal behavior (typing shouldn't leave the cursor
                // stuck invisible mid-blink).
                self.cursor_blink_visible = true;
                self.cursor_blink_deadline = None;
                if self
                    .windows
                    .get(&window_id)
                    .and_then(WindowState::focused_surface)
                    .is_some_and(|surface| surface.ime_state.preedit_active())
                {
                    return;
                }
                // A confirmation dialog is fully modal — it sits ahead of
                // every other keyboard branch so nothing (search prompt,
                // palette, keybinds, pty) sees a key while it is up.
                if self
                    .confirm_dialog
                    .as_ref()
                    .is_some_and(|session| session.window_id == window_id)
                {
                    self.handle_confirm_dialog_key(window_id, &event);
                    return;
                }
                if self
                    .search_prompt
                    .as_ref()
                    .is_some_and(|session| session.window_id == window_id)
                {
                    self.handle_search_prompt_key(event_loop, window_id, &event);
                    return;
                }
                // C2 (FM2): the palette branch sits exactly between the
                // search-prompt branch and keybind-resolve. Order is
                // load-bearing — IME-preedit → search_prompt → palette →
                // keybind-resolve. Because search_prompt is checked first a
                // palette cannot open while it is up (its keys are consumed
                // there); because this branch consumes every key while the
                // palette is open, nothing leaks to keybind-resolve or the
                // pty (modal).
                if self
                    .command_palette
                    .as_ref()
                    .is_some_and(|session| session.window_id == window_id)
                {
                    self.handle_command_palette_key(event_loop, window_id, &event);
                    return;
                }
                if let Some(command) = self.keybinds.resolve(&event.logical_key, self.modifiers) {
                    self.handle_app_command(event_loop, command);
                    return;
                }
                if self.overview_visible {
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

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        #[cfg(target_os = "macos")]
        self.install_macos_menu_if_needed();
        self.tick_cursor_blink(event_loop);
    }
}

impl App {
    fn on_scale_factor_changed(&mut self, window_id: WindowId, scale_factor: f64) {
        if let Some(gpu) = self.gpu.as_mut() {
            match FontGrid::new(
                font_pixel_size(self.runtime_font_size, scale_factor),
                font_config_from_noa_config(&self.config.font),
            ) {
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

    fn on_overview_resize(&mut self, size: PhysicalSize<u32>) {
        if size.width == 0 || size.height == 0 {
            return;
        }
        let Some(gpu) = self.gpu.as_ref() else {
            return;
        };
        let Some(overview) = self.overview_window.as_mut() else {
            return;
        };
        overview.surface_config.width = size.width;
        overview.surface_config.height = size.height;
        overview
            .surface
            .configure(&gpu.device, &overview.surface_config);
        // Stale relative to the new surface size; `ensure_overview_thumbnails`
        // rebuilds it from the next recomputed grid layout.
        overview.thumbnails = None;
        let window = overview.window.clone();
        self.mark_all_overview_tiles_dirty();
        window.request_redraw();
    }

    fn on_cursor_moved(&mut self, window_id: WindowId, position: PhysicalPosition<f64>) {
        let Some(gpu) = self.gpu.as_ref() else {
            return;
        };
        let metrics = gpu.font.metrics();
        let point = split_point_from_physical_position(position);
        if let Some(state) = self.windows.get_mut(&window_id) {
            state.last_mouse_point = point;
        }
        let Some(point) = point else {
            if let Some(state) = self.windows.get_mut(&window_id) {
                state.last_mouse_pane = None;
            }
            self.sync_hover_link(window_id);
            return;
        };
        if self.drag_active_split(window_id, point) {
            return;
        }

        let Some((pane_id, cell)) = self.pane_cell_at_position(window_id, position, metrics) else {
            if let Some(state) = self.windows.get_mut(&window_id) {
                state.last_mouse_pane = None;
            }
            self.sync_hover_link(window_id);
            return;
        };

        if let Some(state) = self.windows.get_mut(&window_id) {
            state.last_mouse_pane = Some(pane_id);
            if let Some(surface) = state.surfaces.get_mut(&pane_id) {
                surface.last_mouse_cell = Some(cell);
            }
        }
        self.sync_hover_link(window_id);

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
        if button == MouseButton::Left {
            match state {
                ElementState::Pressed => {
                    if self.start_split_drag_at_last_mouse_point(window_id) {
                        return;
                    }
                    // Cmd+click on a hovered link opens it and is fully
                    // consumed: no selection start, no SGR mouse report.
                    // Without a hovered link this falls through to the
                    // existing click handling below.
                    if let Some(uri) = self.open_hovered_link(window_id) {
                        if let Some(state) = self.windows.get_mut(&window_id) {
                            state.link_click_in_flight = true;
                        }
                        link_open::open_uri(&uri);
                        return;
                    }
                }
                ElementState::Released => {
                    if self.finish_active_split_drag(window_id) {
                        return;
                    }
                    // The matching half of the Cmd+click-to-open consume
                    // above: swallow the release only when its press was
                    // consumed, so an unrelated selection drag or SGR press
                    // still sees its mouse-up.
                    if let Some(state) = self.windows.get_mut(&window_id)
                        && state.link_click_in_flight
                    {
                        state.link_click_in_flight = false;
                        return;
                    }
                }
            }
        }

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

        if self
            .search_prompt
            .as_ref()
            .is_some_and(|session| session.window_id == window_id)
        {
            // The prompt is modal: a committed IME composition edits its
            // buffer instead of being written to the pty. `ime_state` above
            // already observed the event (clearing its preedit flag); the
            // pty-encoded `bytes` above are simply discarded here.
            if let Ime::Commit(text) = &event {
                let effect = self
                    .search_prompt
                    .as_mut()
                    .and_then(|session| session.prompt.push_text(text));
                if let Some(effect) = effect {
                    self.apply_search_prompt_effect(window_id, effect);
                }
            }
            return;
        }

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
        let font = match FontGrid::new(
            font_pixel_size(update.point_size, scale_factor),
            font_config_from_noa_config(&self.config.font),
        ) {
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
                // Only one prompt is tracked app-wide; cmd+f while one is
                // already open (in this pane or another) is a no-op —
                // the `KeyboardInput` handler routes every other keystroke
                // to it in the common case (same window), and this guard
                // covers the cross-window case.
                if self.search_prompt.is_some() {
                    return;
                }
                let query = terminal.active().search.query().to_string();
                drop(terminal);
                self.search_prompt = Some(SearchPromptSession {
                    window_id,
                    pane_id,
                    prompt: SearchPrompt::open(query),
                });
                if let Some(state) = self.windows.get(&window_id) {
                    state.window.request_redraw();
                }
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

    /// Drives the open search prompt's buffer from a keypress instead of
    /// the normal keybind-resolve -> pty-encode path (the prompt is modal
    /// while open). `cmd+g`/`cmd+shift+g` still navigate matches without
    /// closing it; every other keystroke is either consumed by the prompt
    /// (Escape/Enter/Backspace/printable text) or swallowed outright —
    /// nothing falls through to the pty while the prompt is open. Only
    /// called when `self.search_prompt` targets `window_id` (checked by
    /// the caller).
    fn handle_search_prompt_key(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: &KeyEvent,
    ) {
        match &event.logical_key {
            Key::Named(NamedKey::Escape) => {
                self.close_search_prompt(true);
                return;
            }
            Key::Named(NamedKey::Enter) => {
                self.close_search_prompt(false);
                return;
            }
            Key::Named(NamedKey::Backspace) => {
                let effect = self
                    .search_prompt
                    .as_mut()
                    .map(|session| session.prompt.backspace());
                if let Some(effect) = effect {
                    self.apply_search_prompt_effect(window_id, effect);
                }
                return;
            }
            _ => {}
        }

        if let Some(command) = self.keybinds.resolve(&event.logical_key, self.modifiers) {
            if matches!(
                command,
                AppCommand::Search(SearchAction::FindNext | SearchAction::FindPrevious)
            ) {
                self.handle_app_command(event_loop, command);
            }
            // Every other resolved command (including a repeated Find) is
            // swallowed while the modal prompt owns the keyboard.
            return;
        }

        // Cmd-held combos with no keybind (e.g. an unbound cmd+<letter>)
        // must not leak their character into the query, matching the
        // normal Cmd-swallow convention below the prompt-open branch.
        if self.modifiers.super_key() {
            return;
        }
        let Some(text) = event.text.as_deref() else {
            return;
        };
        let effect = self
            .search_prompt
            .as_mut()
            .and_then(|session| session.prompt.push_text(text));
        if let Some(effect) = effect {
            self.apply_search_prompt_effect(window_id, effect);
        }
    }

    /// Apply a [`SearchPromptEffect`] to the prompt's target terminal and
    /// redraw. No-op if `window_id` no longer matches the open prompt (the
    /// prompt closed between the keypress and this call).
    fn apply_search_prompt_effect(&mut self, window_id: WindowId, effect: SearchPromptEffect) {
        let Some(pane_id) = self
            .search_prompt
            .as_ref()
            .filter(|session| session.window_id == window_id)
            .map(|session| session.pane_id)
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
        {
            let mut terminal = terminal.lock().expect("terminal mutex poisoned");
            match effect {
                SearchPromptEffect::UpdateQuery(query) => terminal.set_search_query(query),
                SearchPromptEffect::ClearQuery => terminal.clear_search(),
            }
        }
        if let Some(state) = self.windows.get(&window_id) {
            state.window.request_redraw();
        }
    }

    /// Close the open search prompt (no-op if none is open). `clear` also
    /// clears the underlying terminal search (Escape); committing with
    /// Enter passes `clear = false` so highlights + the active match
    /// survive and `cmd+g`/`cmd+shift+g` keep navigating.
    fn close_search_prompt(&mut self, clear: bool) {
        let Some(session) = self.search_prompt.take() else {
            return;
        };
        if clear
            && let Some(terminal) = self
                .windows
                .get(&session.window_id)
                .and_then(|state| state.surfaces.get(&session.pane_id))
                .map(|surface| surface.terminal.clone())
        {
            terminal
                .lock()
                .expect("terminal mutex poisoned")
                .clear_search();
        }
        if let Some(state) = self.windows.get(&session.window_id) {
            state.window.request_redraw();
        }
    }

    /// Drive the open command palette from a keypress instead of the normal
    /// keybind-resolve → pty-encode path (the palette is modal while open,
    /// R-6). Mirrors [`App::handle_search_prompt_key`]: Escape cancels, Enter
    /// runs the highlighted command, arrows move the selection, Backspace and
    /// printable text edit the query; every other key is swallowed so nothing
    /// reaches the pty. Only called when `self.command_palette` targets
    /// `window_id` (checked by the caller).
    fn handle_command_palette_key(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: &KeyEvent,
    ) {
        match &event.logical_key {
            Key::Named(NamedKey::Escape) => {
                // Close without executing (R-8).
                self.command_palette = None;
                self.request_window_redraw(window_id);
                return;
            }
            Key::Named(NamedKey::Enter) => {
                let command = self
                    .command_palette
                    .as_ref()
                    .and_then(|session| session.palette.selected_command());
                // With a highlighted command, close BEFORE the side effect
                // (R-10): a command that opens another modal (e.g.
                // Search(Find)) must not leave the palette open alongside it.
                // An empty result set yields `None` — a no-op that leaves the
                // palette open (R-9).
                if let Some(command) = command {
                    self.command_palette = None;
                    self.handle_app_command(event_loop, command);
                }
                return;
            }
            Key::Named(NamedKey::ArrowUp) => {
                if let Some(session) = self.command_palette.as_mut() {
                    session.palette.move_up();
                }
                self.request_window_redraw(window_id);
                return;
            }
            Key::Named(NamedKey::ArrowDown) => {
                if let Some(session) = self.command_palette.as_mut() {
                    session.palette.move_down();
                }
                self.request_window_redraw(window_id);
                return;
            }
            Key::Named(NamedKey::Backspace) => {
                if let Some(session) = self.command_palette.as_mut() {
                    session.palette.backspace();
                }
                self.request_window_redraw(window_id);
                return;
            }
            _ => {}
        }

        if let Some(command) = self.keybinds.resolve(&event.logical_key, self.modifiers) {
            // Re-pressing cmd+shift+p toggles the palette closed; every other
            // resolved command is swallowed while the modal owns the keyboard.
            if command == AppCommand::ToggleCommandPalette {
                self.handle_app_command(event_loop, command);
            }
            return;
        }

        // Cmd-held combos with no binding must not leak their character into
        // the query (mirrors the search prompt's Cmd-swallow).
        if self.modifiers.super_key() {
            return;
        }
        let Some(text) = event.text.as_deref() else {
            return;
        };
        if let Some(session) = self.command_palette.as_mut() {
            session.palette.push_text(text);
        }
        self.request_window_redraw(window_id);
    }

    fn request_window_redraw(&self, window_id: WindowId) {
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

    fn start_split_drag_at_last_mouse_point(&mut self, window_id: WindowId) -> bool {
        let Some(target) = self.split_drag_target_at_last_mouse_point(window_id) else {
            return false;
        };
        let Some(state) = self.windows.get_mut(&window_id) else {
            return false;
        };
        self.focused = Some(window_id);
        state.last_mouse_pane = None;
        state.active_split_drag = Some(target);
        true
    }

    fn split_drag_target_at_last_mouse_point(
        &self,
        window_id: WindowId,
    ) -> Option<SplitResizeDrag> {
        let state = self.windows.get(&window_id)?;
        if state.zoomed.is_some() {
            return None;
        }
        let point = state.last_mouse_point?;
        let bounds = pane_bounds_for_size(state.window.inner_size());
        split_resize_drag_target_at_point(&state.split_tree, bounds, point)
    }

    fn drag_active_split(&mut self, window_id: WindowId, point: split_tree::Point) -> bool {
        let window = {
            let Some(state) = self.windows.get_mut(&window_id) else {
                return false;
            };
            let Some(target) = state.active_split_drag.clone() else {
                return false;
            };
            resize_split_to_drag_point(&mut state.split_tree, &target, point);
            state.window.clone()
        };
        self.relayout_and_resize_window(window_id);
        self.update_focused_ime_cursor_area(window_id);
        window.request_redraw();
        true
    }

    fn finish_active_split_drag(&mut self, window_id: WindowId) -> bool {
        self.windows
            .get_mut(&window_id)
            .and_then(|state| state.active_split_drag.take())
            .is_some()
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
        let contents = match self.clipboard.get_paste_contents() {
            Ok(contents) => contents,
            Err(err) => {
                log::warn!("failed to read clipboard for paste: {err}");
                return;
            }
        };
        let text = match contents {
            PasteContents::FileUrls(paths) => clipboard::file_urls_to_paste_string(&paths),
            PasteContents::Image(png_bytes) => match clipboard::write_temp_png(&png_bytes) {
                Ok(path) => clipboard::shell_escape(&path.to_string_lossy()),
                Err(err) => {
                    log::warn!("failed to save pasted image to a temp file: {err}");
                    return;
                }
            },
            PasteContents::Text(text) => text,
            PasteContents::Empty => String::new(),
        };
        let bracketed_paste = self.bracketed_paste(window_id, pane_id);
        let Some(bytes) = input::encode_paste(&text, bracketed_paste) else {
            return;
        };
        // Paste protection: confirm before sending content that could run a
        // command on its own (a newline), or that tries to break out of
        // bracketed paste.
        if self.config.clipboard_paste_protection && input::paste_is_unsafe(&text, bracketed_paste)
        {
            let lines = text.lines().count().max(1);
            self.open_confirm_dialog(
                window_id,
                format!("Paste {lines} line(s) of text?"),
                ConfirmAction::Paste {
                    window_id,
                    pane_id,
                    bytes,
                },
            );
            return;
        }
        self.write_pane_pty_bytes(window_id, pane_id, &bytes);
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

    /// Read the system clipboard and write its OSC 52 base64 reply to the
    /// pane's pty. The reply travels the same route as DA/DSR reports — into
    /// the pty so the requesting program reads it on its input.
    fn fulfill_clipboard_read(&mut self, window_id: WindowId, pane_id: PaneId, target: &str) {
        let text = match self.clipboard.get_text() {
            Ok(text) => text,
            Err(err) => {
                log::warn!("failed to read clipboard for OSC 52 reply: {err}");
                return;
            }
        };
        let reply = Terminal::osc52_read_reply(target, &text);
        self.write_pane_pty_bytes(window_id, pane_id, &reply);
    }

    /// Raise a confirmation dialog before revealing the clipboard to a program
    /// over OSC 52 (`clipboard-read = ask`).
    fn prompt_clipboard_read(&mut self, window_id: WindowId, pane_id: PaneId, target: String) {
        self.open_confirm_dialog(
            window_id,
            "Send clipboard contents to the terminal?".to_string(),
            ConfirmAction::ClipboardRead {
                window_id,
                pane_id,
                target,
            },
        );
    }

    /// Open the single app-wide confirmation dialog bound to `window_id`. Any
    /// existing dialog is replaced (the newest request wins).
    fn open_confirm_dialog(&mut self, window_id: WindowId, message: String, action: ConfirmAction) {
        self.confirm_dialog = Some(ConfirmDialogSession {
            window_id,
            message,
            hint: "Enter: confirm    Esc: cancel".to_string(),
            action,
        });
        self.request_window_redraw(window_id);
    }

    /// Keystroke routing for the modal confirmation dialog. Enter (or `y`)
    /// confirms and runs the deferred action; Escape (or `n`) cancels; every
    /// other key is swallowed.
    fn handle_confirm_dialog_key(&mut self, window_id: WindowId, event: &KeyEvent) {
        let confirm = match &event.logical_key {
            Key::Named(NamedKey::Enter) => true,
            Key::Named(NamedKey::Escape) => false,
            Key::Character(s) if s.eq_ignore_ascii_case("y") => true,
            Key::Character(s) if s.eq_ignore_ascii_case("n") => false,
            _ => return, // swallow anything else while modal
        };
        let Some(session) = self.confirm_dialog.take() else {
            return;
        };
        if confirm {
            self.run_confirm_action(session.action);
        }
        self.request_window_redraw(window_id);
    }

    fn run_confirm_action(&mut self, action: ConfirmAction) {
        match action {
            ConfirmAction::Paste {
                window_id,
                pane_id,
                bytes,
            } => self.write_pane_pty_bytes(window_id, pane_id, &bytes),
            ConfirmAction::ClipboardRead {
                window_id,
                pane_id,
                target,
            } => self.fulfill_clipboard_read(window_id, pane_id, &target),
        }
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
        let padding = self.padding;
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
                    grid_size_for_pane_rect(rect, metrics, padding),
                )
            })
            .collect::<Vec<_>>();

        let Some(state) = self.windows.get_mut(&window_id) else {
            return;
        };
        apply_pane_resize_batch(state, &targets, metrics, padding);
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
            self.padding,
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
            self.padding,
        );
        Some((pane_id, cell))
    }

    /// The Cmd+hover link under the mouse in `window_id`'s focused-under-
    /// pointer pane, if `Cmd` is held and the cell under `last_mouse_cell`
    /// carries an OSC 8 hyperlink or sits inside an auto-detected
    /// `https?://` URL run. Reuses `last_mouse_pane`/`last_mouse_cell`
    /// (already kept up to date by every `CursorMoved`) instead of
    /// recomputing a pixel hit-test, so it can also be called from
    /// `ModifiersChanged` with the mouse stationary.
    fn hover_link_target(&self, window_id: WindowId) -> Option<(PaneId, HoverLink)> {
        if !self.modifiers.super_key() {
            return None;
        }
        let state = self.windows.get(&window_id)?;
        let pane_id = state.last_mouse_pane?;
        let surface = state.surfaces.get(&pane_id)?;
        let cell = surface.last_mouse_cell?;

        let terminal = surface.terminal.lock().expect("terminal mutex poisoned");
        let row = terminal.active().visible_row(cell.y)?;
        if let Some(link_id) = row.cells.get(cell.x as usize).and_then(|c| c.hyperlink) {
            return Some((pane_id, HoverLink::Registry(link_id)));
        }
        let url = noa_grid::detect_url_at_column(row, cell.x)?;
        Some((
            pane_id,
            HoverLink::Range {
                y: cell.y,
                x_start: url.start_x,
                x_end: url.end_x,
            },
        ))
    }

    /// Recompute the Cmd+hover target for `window_id` and reconcile it into
    /// `Surface::hover_link` + the window's cursor icon. Called from every
    /// event that can change the answer: `CursorMoved` (pointer or pane
    /// moved) and `ModifiersChanged` (Cmd pressed/released with the mouse
    /// stationary).
    fn sync_hover_link(&mut self, window_id: WindowId) {
        let target = self.hover_link_target(window_id);
        let target_pane = target.as_ref().map(|(pane_id, _)| *pane_id);

        // Clear a stale hover on whichever pane held it previously, if the
        // target has moved to a different pane/window or disappeared. This
        // is the only place a hover can go stale outside its own pane: a
        // pane's own hover_link is otherwise only ever written here.
        if let Some((prev_window, prev_pane)) = self.hovered_link
            && (prev_window != window_id || Some(prev_pane) != target_pane)
        {
            let cleared = self
                .windows
                .get_mut(&prev_window)
                .and_then(|state| state.surfaces.get_mut(&prev_pane))
                .is_some_and(|surface| surface.hover_link.take().is_some());
            if cleared && let Some(state) = self.windows.get(&prev_window) {
                state.window.request_redraw();
            }
            self.hovered_link = None;
        }

        if let Some((pane_id, link)) = target {
            self.hovered_link = Some((window_id, pane_id));
            let changed = self
                .windows
                .get_mut(&window_id)
                .and_then(|state| state.surfaces.get_mut(&pane_id))
                .is_some_and(|surface| {
                    let changed = surface.hover_link != Some(link);
                    surface.hover_link = Some(link);
                    changed
                });
            if changed && let Some(state) = self.windows.get(&window_id) {
                state.window.request_redraw();
            }
        }

        self.update_cursor_icon(window_id);
    }

    /// Pointer cursor while a link is Cmd+hovered in `window_id`'s
    /// under-the-mouse pane, the platform default otherwise.
    fn update_cursor_icon(&self, window_id: WindowId) {
        let Some(state) = self.windows.get(&window_id) else {
            return;
        };
        let hovering = state
            .last_mouse_pane
            .and_then(|pane_id| state.surfaces.get(&pane_id))
            .is_some_and(|surface| surface.hover_link.is_some());
        state.window.set_cursor(if hovering {
            CursorIcon::Pointer
        } else {
            CursorIcon::Default
        });
    }

    /// Resolve the currently Cmd+hovered link in `window_id`'s under-the-
    /// mouse pane to its URI text, re-deriving it from live grid state
    /// (rather than caching the string on `Surface::hover_link`, which the
    /// renderer only needs the geometry of).
    fn open_hovered_link(&self, window_id: WindowId) -> Option<String> {
        let state = self.windows.get(&window_id)?;
        let pane_id = state.last_mouse_pane?;
        let surface = state.surfaces.get(&pane_id)?;
        surface.hover_link?;
        let cell = surface.last_mouse_cell?;

        let terminal = surface.terminal.lock().expect("terminal mutex poisoned");
        let row = terminal.active().visible_row(cell.y)?;
        if let Some(link_id) = row.cells.get(cell.x as usize).and_then(|c| c.hyperlink) {
            return terminal
                .hyperlinks
                .get(link_id)
                .map(|link| link.uri.clone());
        }
        noa_grid::detect_url_at_column(row, cell.x).map(|url| url.uri)
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

/// Pixel metrics for `XTWINOPS` reports (`CSI 14/16 t`). Derived from the
/// same `rect`/`padding` the caller already used to compute this pane's
/// `GridSize` (via `grid_size_for_pane_rect`) — not reconstructed
/// independently as `cell_w × cols`, which would drift from `rect` whenever
/// the pane's pixel size isn't an exact multiple of the cell size.
fn pixel_metrics_for_pane(
    rect: PaneRectApp,
    metrics: noa_font::Metrics,
    padding: GridPadding,
) -> (u32, u32, u32, u32) {
    let cell_w_px = metrics.cell_w.round().max(0.0) as u32;
    let cell_h_px = metrics.cell_h.round().max(0.0) as u32;
    let text_area_w_px = (rect.w as f32 - padding.horizontal()).max(0.0).round() as u32;
    let text_area_h_px = (rect.h as f32 - padding.vertical()).max(0.0).round() as u32;
    (cell_w_px, cell_h_px, text_area_w_px, text_area_h_px)
}

fn apply_pane_resize_batch(
    state: &mut WindowState,
    targets: &[(PaneId, PaneRectApp, GridSize)],
    metrics: noa_font::Metrics,
    padding: GridPadding,
) {
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
        let rect = targets
            .iter()
            .find(|(target, _, _)| *target == pane_id)
            .map(|(_, rect, _)| *rect);
        if let Some(rect) = rect {
            surface.rect = rect;
        }
        surface.grid_size = grid_size;
        let mut terminal = surface.terminal.lock().expect("terminal mutex poisoned");
        terminal.resize(grid_size);
        if let Some(rect) = rect {
            let (cw, ch, taw, tah) = pixel_metrics_for_pane(rect, metrics, padding);
            terminal.set_pixel_metrics(cw, ch, taw, tah);
        }
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
        ViewportScroll::PrevPrompt => {
            terminal.scroll_to_prompt(PromptJump::Prev);
        }
        ViewportScroll::NextPrompt => {
            terminal.scroll_to_prompt(PromptJump::Next);
        }
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
    keep_alive_when_empty: bool,
) -> TabCloseOutcome<Id> {
    let Some(closing_index) = order.iter().position(|id| *id == closing) else {
        return TabCloseOutcome::Stale;
    };
    if order.len() == 1 {
        if keep_alive_when_empty {
            return TabCloseOutcome::Continue { focused: None };
        }
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

fn overview_tile_source_order<Id: Copy + Eq>(
    window_order: &[Id],
    mut live_window: impl FnMut(Id) -> bool,
    overview_window: Option<Id>,
) -> Vec<Id> {
    window_order
        .iter()
        .copied()
        .filter(|id| Some(*id) != overview_window && live_window(*id))
        .collect()
}

fn overview_tile_target_at_point<Id: Copy>(
    source_ids: &[Id],
    tile_rects: &[PaneRectApp],
    point: split_tree::Point,
) -> Option<Id> {
    let tiles = source_ids
        .iter()
        .copied()
        .zip(tile_rects.iter().copied())
        .collect::<Vec<_>>();
    hit_test_overview_grid(&tiles, point)
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

fn overview_redraw_decision(
    source_state: Option<(bool, bool)>,
    overview_visible: bool,
    overview_occluded: bool,
) -> TargetedRedrawDecision {
    let Some((source_exists, source_occluded)) = source_state else {
        return TargetedRedrawDecision::Stale;
    };
    if !overview_visible || !source_exists {
        TargetedRedrawDecision::Stale
    } else if overview_occluded || source_occluded {
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
    Overview,
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
        AppCommand::ToggleTabOverview
        | AppCommand::SelectTab(_)
        | AppCommand::NextTab
        | AppCommand::PrevTab => CommandScope::NativeTabGroup,
        AppCommand::About
        | AppCommand::Preferences
        | AppCommand::NewTab
        | AppCommand::ToggleCommandPalette
        | AppCommand::CloseWindow
        | AppCommand::Quit => CommandScope::App,
    }
}

/// Build the render-facing palette payload from the app-side session,
/// resolving each filtered command's title and (current) keybind hint. Takes
/// no terminal lock — the palette is terminal-independent (R-12).
fn command_palette_snapshot(
    keybinds: &KeybindEngine,
    palette: &CommandPalette,
) -> CommandPaletteSnapshot {
    let rows = palette
        .filtered()
        .iter()
        .map(|&command| {
            (
                command_palette::command_palette_title(command).to_string(),
                command_palette::command_palette_keybind(keybinds, command),
            )
        })
        .collect();
    CommandPaletteSnapshot {
        query: palette.query().to_string(),
        rows,
        selected: palette.selected(),
    }
}

fn overview_command_scope(command: AppCommand) -> CommandScope {
    match command {
        AppCommand::ToggleTabOverview => CommandScope::NativeTabGroup,
        AppCommand::About | AppCommand::Preferences | AppCommand::Quit => CommandScope::App,
        // The palette does not open while the overview is focused (v1, R-10):
        // Overview scope makes `ToggleCommandPalette` a no-op there (AC-15).
        AppCommand::ToggleCommandPalette
        | AppCommand::Copy
        | AppCommand::Paste
        | AppCommand::Terminal(_)
        | AppCommand::FontSize(_)
        | AppCommand::Search(_)
        | AppCommand::ScrollViewport(_)
        | AppCommand::NewTab
        | AppCommand::NewSplitRight
        | AppCommand::NewSplitDown
        | AppCommand::FocusDirection(_)
        | AppCommand::ResizeSplit(_)
        | AppCommand::EqualizeSplits
        | AppCommand::ToggleSplitZoom
        | AppCommand::CloseTab
        | AppCommand::SelectTab(_)
        | AppCommand::NextTab
        | AppCommand::PrevTab
        | AppCommand::CloseWindow => CommandScope::Overview,
    }
}

fn resolve_command_target<Id: Copy>(command: AppCommand, focused: Option<Id>) -> Option<Id> {
    if command_scope(command) == CommandScope::FocusedTab {
        focused
    } else {
        None
    }
}

fn tab_overview_visibility_after_dispatch(
    command: AppCommand,
    overview_visible: bool,
) -> Option<bool> {
    match command {
        AppCommand::ToggleTabOverview => Some(!overview_visible),
        _ => None,
    }
}

/// Copy one overview tile texture (live mirror or placeholder title, both
/// already rendered at exactly `rect`'s pixel size) into its grid position on
/// the overview surface texture. No scaling: `copy_texture_to_texture` needs
/// matching extents, which holds because every tile texture is allocated at
/// its `OverviewLayout` rect's size (`ensure_overview_thumbnails`).
fn composite_overview_tile(
    encoder: &mut wgpu::CommandEncoder,
    tile_texture: &wgpu::Texture,
    surface_texture: &wgpu::Texture,
    rect: PaneRectApp,
) {
    encoder.copy_texture_to_texture(
        wgpu::TexelCopyTextureInfo {
            texture: tile_texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyTextureInfo {
            texture: surface_texture,
            mip_level: 0,
            origin: wgpu::Origin3d {
                x: rect.x,
                y: rect.y,
                z: 0,
            },
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::Extent3d {
            width: rect.w,
            height: rect.h,
            depth_or_array_layers: 1,
        },
    );
}

/// Choose the swapchain surface format, preferring a **non-sRGB** format
/// (`Bgra8Unorm`) over an sRGB one (`Bgra8UnormSrgb`).
///
/// This is the WP3 (REQ-AA-1) "native gamma-correct AA" fix. When the
/// surface format `.is_srgb()`, the GPU's fixed-function alpha blend unit
/// decodes stored texels to linear before blending and re-encodes to sRGB
/// on write — so `wgpu::BlendState::ALPHA_BLENDING` (`pipeline.rs`) executes
/// in **linear** space. That's a different blend space than Ghostty's
/// `native` macOS text-rendering mode, which blends glyph coverage against
/// the background directly in gamma-encoded space (how CoreText/FreeType
/// render by default) — the mismatch visibly thins dark-on-light glyph
/// edges relative to Ghostty.
///
/// Preferring a non-sRGB surface format makes all blending — solid
/// backgrounds, selection highlights, and glyph coverage — happen in gamma
/// space, matching `native`. This is in lockstep with
/// `Renderer::new`'s `target_format_is_srgb: format.is_srgb()`
/// (`noa-render/src/renderer.rs`), which routes `surface_output_rgba`
/// (`noa-render/src/renderer.rs`) into its no-op branch whenever the
/// surface format is non-sRGB: colors are written to the target unchanged,
/// no double-gamma. Do **not** "fix" this back to preferring
/// `Bgra8UnormSrgb` — that reintroduces the linear-blend thinning bug.
/// Falls back to `Bgra8UnormSrgb`, then to the first available format, if
/// the adapter offers no non-sRGB option.
fn preferred_surface_format(available: &[wgpu::TextureFormat]) -> wgpu::TextureFormat {
    available
        .iter()
        .copied()
        .find(|f| *f == wgpu::TextureFormat::Bgra8Unorm)
        .or_else(|| {
            available
                .iter()
                .copied()
                .find(|f| *f == wgpu::TextureFormat::Bgra8UnormSrgb)
        })
        .unwrap_or(available[0])
}

/// Pick the surface's composite-alpha mode. An opaque window keeps the
/// existing Opaque preference (solid terminal colors). A transparent window
/// (`background-opacity` below 1.0) instead prefers, in order, `PostMultiplied`
/// (our colors are straight, non-premultiplied), then `PreMultiplied`, then
/// `Inherit`, before falling back to whatever the surface offers first.
fn preferred_surface_alpha_mode(
    caps: &wgpu::SurfaceCapabilities,
    transparent: bool,
) -> wgpu::CompositeAlphaMode {
    let preference: &[wgpu::CompositeAlphaMode] = if transparent {
        &[
            wgpu::CompositeAlphaMode::PostMultiplied,
            wgpu::CompositeAlphaMode::PreMultiplied,
            wgpu::CompositeAlphaMode::Inherit,
        ]
    } else {
        &[wgpu::CompositeAlphaMode::Opaque]
    };
    preference
        .iter()
        .copied()
        .find(|mode| caps.alpha_modes.contains(mode))
        .or_else(|| caps.alpha_modes.first().copied())
        .unwrap_or(wgpu::CompositeAlphaMode::Auto)
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
    fn resolve_grid_padding_keeps_defaults_for_unset_axes() {
        assert_eq!(resolve_grid_padding(None, None), DEFAULT_GRID_PADDING);
    }

    #[test]
    fn resolve_grid_padding_applies_value_to_both_edges_of_an_axis() {
        let padding = resolve_grid_padding(Some(8.0), Some(4.0));
        assert_eq!(padding, GridPadding::new(4.0, 8.0, 4.0, 8.0));

        // Only x set: y keeps the asymmetric default (top 0, bottom 16).
        let x_only = resolve_grid_padding(Some(10.0), None);
        assert_eq!(x_only, GridPadding::new(0.0, 10.0, 16.0, 10.0));

        // Only y set: x keeps the default 16 on both sides.
        let y_only = resolve_grid_padding(None, Some(2.0));
        assert_eq!(y_only, GridPadding::new(2.0, 16.0, 2.0, 16.0));
    }

    #[test]
    fn resolve_cursor_style_is_none_when_nothing_is_configured() {
        assert_eq!(resolve_cursor_style(None, None), None);
    }

    #[test]
    fn resolve_cursor_style_defaults_shape_and_blink() {
        // Only blink toggled: shape defaults to block.
        assert_eq!(
            resolve_cursor_style(None, Some(false)),
            Some(CursorStyle::SteadyBlock)
        );
        // Only shape set: blink defaults on.
        assert_eq!(
            resolve_cursor_style(Some(noa_config::CursorShape::Bar), None),
            Some(CursorStyle::BlinkingBar)
        );
    }

    #[test]
    fn resolve_cursor_style_maps_every_combination() {
        use noa_config::CursorShape;
        let cases = [
            (CursorShape::Block, true, CursorStyle::BlinkingBlock),
            (CursorShape::Block, false, CursorStyle::SteadyBlock),
            (CursorShape::Bar, true, CursorStyle::BlinkingBar),
            (CursorShape::Bar, false, CursorStyle::SteadyBar),
            (CursorShape::Underline, true, CursorStyle::BlinkingUnderline),
            (CursorShape::Underline, false, CursorStyle::SteadyUnderline),
        ];
        for (shape, blink, expected) in cases {
            assert_eq!(
                resolve_cursor_style(Some(shape), Some(blink)),
                Some(expected)
            );
        }
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
    fn surface_format_prefers_non_srgb_for_native_gamma_correct_blending() {
        // WP3 / REQ-AA-1 / AC-WP3-01: a non-sRGB surface format keeps the
        // fixed-function alpha blend unit in gamma space, matching
        // Ghostty's `native` macOS text-rendering mode.
        assert_eq!(
            preferred_surface_format(&[
                wgpu::TextureFormat::Bgra8UnormSrgb,
                wgpu::TextureFormat::Bgra8Unorm,
            ]),
            wgpu::TextureFormat::Bgra8Unorm
        );
    }

    #[test]
    fn surface_format_falls_back_to_srgb_when_no_non_srgb_option_exists() {
        assert_eq!(
            preferred_surface_format(&[wgpu::TextureFormat::Bgra8UnormSrgb]),
            wgpu::TextureFormat::Bgra8UnormSrgb
        );
    }

    #[test]
    fn surface_format_falls_back_to_first_available_when_neither_bgra8_option_exists() {
        assert_eq!(
            preferred_surface_format(&[
                wgpu::TextureFormat::Rgba16Float,
                wgpu::TextureFormat::Rgba8Unorm,
            ]),
            wgpu::TextureFormat::Rgba16Float
        );
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
            preferred_surface_alpha_mode(&caps, false),
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
            preferred_surface_alpha_mode(&caps, false),
            wgpu::CompositeAlphaMode::Inherit
        );
    }

    #[test]
    fn surface_alpha_mode_prefers_post_multiplied_when_transparent() {
        let caps = wgpu::SurfaceCapabilities {
            alpha_modes: vec![
                wgpu::CompositeAlphaMode::Opaque,
                wgpu::CompositeAlphaMode::PreMultiplied,
                wgpu::CompositeAlphaMode::PostMultiplied,
            ],
            ..Default::default()
        };

        assert_eq!(
            preferred_surface_alpha_mode(&caps, true),
            wgpu::CompositeAlphaMode::PostMultiplied
        );
    }

    #[test]
    fn surface_alpha_mode_transparent_falls_back_through_preference_order() {
        // No PostMultiplied — the next preferred transparent mode wins.
        let caps = wgpu::SurfaceCapabilities {
            alpha_modes: vec![
                wgpu::CompositeAlphaMode::Opaque,
                wgpu::CompositeAlphaMode::PreMultiplied,
            ],
            ..Default::default()
        };

        assert_eq!(
            preferred_surface_alpha_mode(&caps, true),
            wgpu::CompositeAlphaMode::PreMultiplied
        );
    }

    #[test]
    fn surface_alpha_mode_transparent_falls_back_to_first_when_none_preferred() {
        // Only Opaque is offered — a transparent window still has to pick
        // something, so it takes the surface's first advertised mode.
        let caps = wgpu::SurfaceCapabilities {
            alpha_modes: vec![wgpu::CompositeAlphaMode::Opaque],
            ..Default::default()
        };

        assert_eq!(
            preferred_surface_alpha_mode(&caps, true),
            wgpu::CompositeAlphaMode::Opaque
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
            close_tab_outcome(&[1, 2, 3], Some(2), 9, false),
            TabCloseOutcome::Stale
        );
        assert_eq!(
            close_tab_outcome(&[1], Some(1), 1, false),
            TabCloseOutcome::Quit
        );
        assert_eq!(
            close_tab_outcome(&[1], Some(1), 1, true),
            TabCloseOutcome::Continue { focused: None }
        );
        assert_eq!(
            close_tab_outcome(&[1, 2, 3], Some(2), 2, false),
            TabCloseOutcome::Continue { focused: Some(3) }
        );
        assert_eq!(
            close_tab_outcome(&[1, 2, 3], Some(3), 3, false),
            TabCloseOutcome::Continue { focused: Some(2) }
        );
        assert_eq!(
            close_tab_outcome(&[1, 2, 3], Some(1), 2, false),
            TabCloseOutcome::Continue { focused: Some(1) }
        );
    }

    #[test]
    fn overview_window_order_excludes_overview_and_closed_tabs() {
        let window_order = [1_u8, 2, 3, 4];
        let live_windows = |id| id != 3;

        let sources = overview_tile_source_order(&window_order, live_windows, Some(4));

        assert_eq!(sources, vec![1, 2]);
    }

    #[test]
    fn overview_click_hit_test_resolves_only_live_tiles() {
        let source_ids = [10_u8, 11, 12, 13, 14, 15, 16, 17, 18, 19];
        let layout = compute_overview_grid(source_ids.len(), PaneRectApp::new(0, 0, 90, 120), 9);

        assert_eq!(
            overview_tile_target_at_point(
                &source_ids,
                &layout.tiles,
                split_tree::Point::new(45, 45)
            ),
            Some(14)
        );
        assert_eq!(
            overview_tile_target_at_point(
                &source_ids,
                &layout.tiles,
                split_tree::Point::new(15, 105)
            ),
            None
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
    fn overview_redraw_decision_respects_visibility_and_occlusion() {
        assert_eq!(
            overview_redraw_decision(None, true, false),
            TargetedRedrawDecision::Stale
        );
        assert_eq!(
            overview_redraw_decision(Some((false, false)), true, false),
            TargetedRedrawDecision::Stale
        );
        assert_eq!(
            overview_redraw_decision(Some((true, false)), false, false),
            TargetedRedrawDecision::Stale
        );
        assert_eq!(
            overview_redraw_decision(Some((true, false)), true, true),
            TargetedRedrawDecision::Suppress
        );
        assert_eq!(
            overview_redraw_decision(Some((true, true)), true, false),
            TargetedRedrawDecision::Suppress
        );
        assert_eq!(
            overview_redraw_decision(Some((true, false)), true, false),
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

    // FM-4 regression: text-area px must come from the same `rect`/padding
    // grid_size_for_pane_rect used, not an independent cell_w × cols
    // multiplication — which would drift whenever the pane's pixel size
    // isn't an exact multiple of the cell size (as here: 137px / 9px cells).
    #[test]
    fn pixel_metrics_for_pane_derive_text_area_from_rect_not_from_grid_size() {
        let rect = PaneRectApp::new(0, 0, 137, 245);
        let metrics = metrics(9.0, 18.0);

        let (cw, ch, taw, tah) = pixel_metrics_for_pane(rect, metrics, DEFAULT_GRID_PADDING);

        assert_eq!(cw, 9);
        assert_eq!(ch, 18);
        // 137 - (16 left + 16 right) = 105, 245 - (0 top + 16 bottom) = 229 —
        // NOT floor(105/9)=11 cols * 9 = 99, which cell_w × cols would give.
        assert_eq!(taw, 105);
        assert_eq!(tah, 229);
    }

    #[test]
    fn pixel_metrics_for_pane_clamps_padding_larger_than_rect_to_zero() {
        let rect = PaneRectApp::new(0, 0, 10, 10);
        let metrics = metrics(9.0, 18.0);

        let (_, _, taw, tah) = pixel_metrics_for_pane(rect, metrics, DEFAULT_GRID_PADDING);

        assert_eq!(taw, 0);
        assert_eq!(tah, 0);
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
    fn toggle_tab_overview_is_a_native_tab_group_command() {
        assert_eq!(
            command_scope(AppCommand::ToggleTabOverview),
            CommandScope::NativeTabGroup
        );
        assert_eq!(
            resolve_command_target(AppCommand::ToggleTabOverview, Some(42_u8)),
            None
        );
    }

    #[test]
    fn overview_command_scope_resolves_terminal_commands_to_no_ops() {
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
            AppCommand::NewSplitRight,
            AppCommand::NewSplitDown,
            AppCommand::FocusDirection(Direction::Left),
            AppCommand::ResizeSplit(Direction::Right),
            AppCommand::EqualizeSplits,
            AppCommand::ToggleSplitZoom,
            AppCommand::CloseTab,
        ] {
            assert_eq!(overview_command_scope(command), CommandScope::Overview);
            assert_eq!(resolve_command_target(command, focused), focused);
        }

        assert_eq!(
            overview_command_scope(AppCommand::ToggleTabOverview),
            CommandScope::NativeTabGroup
        );
    }

    #[test]
    fn toggle_tab_overview_dispatch_flips_visibility() {
        let overview_visible =
            tab_overview_visibility_after_dispatch(AppCommand::ToggleTabOverview, false)
                .expect("toggle command should update overview state");
        assert!(overview_visible);
        assert_eq!(
            tab_overview_visibility_after_dispatch(AppCommand::ToggleTabOverview, overview_visible),
            Some(false)
        );
        assert_eq!(
            tab_overview_visibility_after_dispatch(AppCommand::Copy, overview_visible),
            None
        );
    }

    #[test]
    fn empty_terminal_title_falls_back_to_app_name() {
        assert_eq!(tab_title(""), "noa");
        assert_eq!(tab_title("shell"), "shell");
    }

    #[test]
    fn command_palette_toggle_is_app_scoped_and_overview_no_op() {
        // AC-1: openable from any tab. AC-15: a no-op while the overview is
        // focused (Overview scope).
        assert_eq!(
            command_scope(AppCommand::ToggleCommandPalette),
            CommandScope::App
        );
        assert_eq!(
            overview_command_scope(AppCommand::ToggleCommandPalette),
            CommandScope::Overview
        );
    }

    #[test]
    fn command_palette_snapshot_reflects_query_selection_and_keybinds() {
        // AC-18: the render payload mirrors the session (query / filtered
        // titles + keybind hints / selected) with no terminal involved.
        let keybinds = KeybindEngine::default();
        let palette = CommandPalette::open();

        let snapshot = command_palette_snapshot(&keybinds, &palette);
        assert_eq!(snapshot.query, "");
        assert_eq!(snapshot.selected, 0);
        assert_eq!(
            snapshot.rows.len(),
            command_palette::command_palette_entries().len()
        );
        // First entry is About (no binding); Copy carries its cmd+c hint.
        assert_eq!(snapshot.rows[0], ("About noa".to_string(), None));
        assert!(
            snapshot
                .rows
                .contains(&("Copy to Clipboard".to_string(), Some("cmd+c".to_string()))),
            "keybind hints are resolved from the engine"
        );
    }
}
