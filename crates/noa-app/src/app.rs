//! The winit [`ApplicationHandler`] — owns native windows/tabs, per-tab
//! terminal sessions, and the shared GPU/font state used to render them.
//!
//! Rendering + presentation happens on the winit main thread (macOS requires
//! presenting on the thread that owns the window). Each io thread owns one
//! PTY, touches only its tab's `Terminal` mutex, and posts targeted user
//! events back to the main loop.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, TryLockError};
use std::time::{Duration, Instant};

use crossbeam_channel::Sender;
use noa_core::{DEFAULT_GRID_PADDING, GridPadding, GridSize, PixelSize, Point};
use noa_font::FontGrid;
use noa_grid::{
    CursorStyle, PromptJump, Terminal,
    modes::{MouseFormat, MouseTracking},
};
use noa_pty::{Pty, PtyConfig};
use noa_render::{
    CardPipeline, CardStyle, CardTexturePlacement, CardTilePlacement, CommandPaletteSnapshot,
    FrameSnapshot, HoverLink, OverviewThumbnailResources, PaneFrame, PaneId as RenderPaneId,
    PaneRect, Renderer, Theme,
};
use noa_vt::Stream;
use winit::application::ApplicationHandler;
use winit::dpi::{LogicalPosition, LogicalSize, PhysicalPosition, PhysicalSize};
use winit::event::{ElementState, Ime, KeyEvent, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoopProxy};
use winit::keyboard::{Key, ModifiersState, NamedKey};
#[cfg(target_os = "macos")]
use winit::platform::macos::{OptionAsAlt, WindowAttributesExtMacOS, WindowExtMacOS};
use winit::window::{CursorIcon, Window, WindowAttributes, WindowId};

use crate::clipboard::{self, PasteContents, SystemClipboard};
use crate::command_palette::{self, CommandPalette};
use crate::commands::{FontSizeAction, KeybindEngine, SearchAction, TerminalAction};
use crate::events::UserEvent;
use crate::input;
use crate::link_open;
use crate::mouse::{self, MouseSelectionState, SelectionGesture};
use crate::search_prompt::{SearchPrompt, SearchPromptEffect};
use crate::session;
use crate::split_tree::{
    self, Direction, HitTarget, ImeOp, MIN_PANE_SIZE_PX, PaneId, Rect as PaneRectApp,
    SPLIT_RESIZE_STEP_PX, SplitOrientation, SplitResizeDrag, SplitTree, equalize,
    focus_in_direction, focus_switch_plan, hit_test, resize_split, resize_split_to_drag_point,
    split_pane, split_resize_drag_target_at_point, zoom_resize_targets, zoom_toggle,
};
use crate::tab_overview::{
    OVERVIEW_BG_COLOR, OVERVIEW_BORDER_COLOR, OVERVIEW_CARD_BORDER_WIDTH, OVERVIEW_CARD_COLOR,
    OVERVIEW_CARD_CORNER_RADIUS, OVERVIEW_CARD_FOCUS_GLOW_WIDTH, OVERVIEW_CARD_FOCUS_WIDTH,
    OVERVIEW_CHROME_BORDER_COLOR, OVERVIEW_CHROME_PILL_COLOR, OVERVIEW_FOCUS_RING_COLOR,
    OVERVIEW_GRID_CAP, OVERVIEW_MAX_RENDER_TILES_PER_FRAME, OVERVIEW_OUTER_MARGIN,
    OVERVIEW_TILE_GUTTER, OVERVIEW_TILE_MIN_RENDER_INTERVAL, OVERVIEW_TITLE_BAR_COLOR,
    OVERVIEW_TITLE_BAR_H, OverviewAction, OverviewChrome, OverviewEscapeAction, OverviewLayout,
    OverviewRenderCandidate, center_label, compute_overview_grid, hit_test_overview_grid,
    move_overview_selection, overview_backlog_decision, overview_chrome_bands,
    overview_close_hit_test, overview_escape_action, overview_hint_bar_rect,
    overview_hint_bar_text, overview_initial_selection, overview_key_action,
    overview_placeholder_source_ids, overview_search_field_rect, overview_search_field_row,
    overview_tab_filter, overview_tile_labels, sanitize_placeholder_label,
    select_due_overview_tile_ids, title_bar_row_with_close,
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
    /// `scrollback-limit`: total bytes of scrollback storage retained per pane
    /// before page-granular eviction (`0` disables scrollback). Applied to each
    /// new terminal at surface creation.
    pub scrollback_limit: usize,
    /// `window-save-state`: whether the window/tab/split session is saved on
    /// exit and restored on launch. `never` disables both.
    pub window_save_state: noa_config::WindowSaveState,
    /// `macos-option-as-alt`: which Option key(s) the macOS window layer
    /// rewrites as terminal Alt.
    pub macos_option_as_alt: noa_config::MacosOptionAsAlt,
    /// `macos-titlebar-style`: titlebar presentation for ordinary terminal
    /// windows.
    pub macos_titlebar_style: noa_config::MacosTitlebarStyle,
    /// Set when the user passed an explicit grid size on the CLI (`--cols` /
    /// `--rows`). Session restore is suppressed in that case so the requested
    /// dimensions win over the saved topology (Ghostty parity).
    pub cli_grid_override: bool,
    /// `quick-terminal-hotkey`: the global hotkey chord toggling the drop-down
    /// quick terminal (e.g. `cmd+grave`). `None` leaves the feature disabled.
    pub quick_terminal_hotkey: Option<String>,
    /// `quick-terminal-size`: the quick terminal's height as a fraction of the
    /// screen height (`0.1..=1.0`).
    pub quick_terminal_size: f32,
    /// `quick-terminal-autohide`: hide the quick terminal when it loses focus.
    pub quick_terminal_autohide: bool,
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

#[cfg(target_os = "macos")]
fn macos_option_as_alt(value: noa_config::MacosOptionAsAlt) -> OptionAsAlt {
    match value {
        noa_config::MacosOptionAsAlt::None => OptionAsAlt::None,
        noa_config::MacosOptionAsAlt::Left => OptionAsAlt::OnlyLeft,
        noa_config::MacosOptionAsAlt::Right => OptionAsAlt::OnlyRight,
        noa_config::MacosOptionAsAlt::Both => OptionAsAlt::Both,
    }
}

#[cfg(target_os = "macos")]
fn apply_macos_titlebar_style(
    attrs: WindowAttributes,
    style: noa_config::MacosTitlebarStyle,
) -> WindowAttributes {
    match style {
        noa_config::MacosTitlebarStyle::Native => attrs,
        noa_config::MacosTitlebarStyle::Transparent => attrs
            .with_titlebar_transparent(true)
            .with_fullsize_content_view(true),
        noa_config::MacosTitlebarStyle::Hidden => attrs
            .with_title_hidden(true)
            .with_titlebar_hidden(true)
            .with_fullsize_content_view(true),
    }
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

/// Identifies one logical window — i.e. one AppKit tab group. Every native
/// tab ([`WindowState`]) carries the id of the window it belongs to; tabs
/// sharing an id are tabbed together (macOS) and cycle/select among
/// themselves, while a fresh id (minted by [`App::allocate_group_id`] on
/// `New Window`) starts a separate native window. The macOS `tabbingIdentifier`
/// string is derived from it in [`App::tabbing_identifier`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct WindowGroupId(u64);

/// Whether a spawned tab joins the focused window or opens a new one — the
/// only difference between `New Tab` (`cmd+t`) and `New Window` (`cmd+n`),
/// which otherwise share [`App::spawn_tab`]'s whole creation path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SpawnTarget {
    /// Join the focused window's tab group (a fresh group if nothing is
    /// focused, e.g. the very first tab at startup).
    CurrentWindow,
    /// Always start a fresh tab group / native window.
    NewWindow,
}

/// State for one native tab. On macOS, each tab is an NSWindow in the same
/// AppKit tab group; winit still reports them as distinct `WindowId`s.
struct WindowState {
    window: Arc<Window>,
    /// The logical window (AppKit tab group) this tab belongs to.
    group: WindowGroupId,
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
    /// Rounded-card shader reused for overview chrome overlays (search and
    /// hint pills). Kept outside thumbnail resources so chrome can render even
    /// when the tile pool is rebuilt.
    chrome_card: Option<OverviewChromeCardPipeline>,
    /// The currently selected tile (REQ-OV-14): an index directly into the
    /// row-major source-tile order (`App::overview_source_tile_ids`) —
    /// live tiles first, then any overflow placeholder tiles — so one index
    /// serves both without translation (REQ-OV-15b: placeholders are
    /// selectable too). Reset on every `show_tab_overview` and clamped in
    /// `redraw_overview` as source panes come and go.
    selected: usize,
    /// The live "Search tabs" filter query (REQ-OV-16). Printable keys append
    /// and Backspace pops while the Overview is focused; the filtered result
    /// set drives every downstream consumer via `App::overview_source_tile_ids`
    /// (redraw, hit-test, nav, Cmd+N, title bars, placeholders). Cleared on
    /// every `show_tab_overview` and by the first Escape when non-empty.
    search_query: String,
}

struct OverviewChromeCardPipeline {
    format: wgpu::TextureFormat,
    pipeline: CardPipeline,
}

struct OverviewChromeTexture {
    texture: wgpu::Texture,
    rect: PaneRectApp,
}

#[derive(Clone, Copy, Debug, Default)]
struct OverviewTileRenderState {
    dirty: bool,
    last_render_at: Option<Instant>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct OverviewTileId {
    window_id: WindowId,
    pane_id: PaneId,
}

impl OverviewTileId {
    const fn new(window_id: WindowId, pane_id: PaneId) -> Self {
        Self { window_id, pane_id }
    }
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
    /// The Tab Overview mirror's read-only publish slot (Fix B, REQ-NF-6):
    /// this pane's io thread opportunistically drops a fresh
    /// `FrameSnapshot::peek` here whenever it already holds the `Terminal`
    /// lock feeding pty bytes in and the overview is visible (see
    /// `io_thread::feed_terminal`), so `App::render_due_overview_tiles`
    /// never has to lock `terminal` itself. `None` until the first publish
    /// (or `App::seed_overview_snapshots`'s one-time fallback) lands.
    overview_snapshot: Arc<Mutex<Option<Arc<FrameSnapshot>>>>,
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

/// How long the quick terminal takes to slide fully in or out.
const QUICK_TERMINAL_SLIDE_DURATION: Duration = Duration::from_millis(200);
/// The quick terminal repaints/repositions at roughly this cadence while
/// sliding (≈60 fps), driven off the `about_to_wait` `WaitUntil` timer.
const QUICK_TERMINAL_FRAME_INTERVAL: Duration = Duration::from_millis(16);

/// Runtime state for the drop-down quick terminal. The window itself is a
/// normal [`WindowState`] entry in `App::windows`; this tracks the slide
/// geometry (physical px, relative to the target monitor) and animation.
struct QuickTerminalState {
    window_id: WindowId,
    /// Left edge of the panel (monitor origin x).
    origin_x: i32,
    /// The monitor's top edge — the panel's fully-revealed top.
    top_y: i32,
    /// Panel width and height in physical pixels.
    width: u32,
    height: u32,
    /// Whether the panel is revealed (or animating toward revealed). When
    /// `false` and `anim` is `None`, the window is hidden.
    visible: bool,
    /// The in-flight slide, if any.
    anim: Option<QuickTerminalAnim>,
}

/// One in-flight quick-terminal slide.
struct QuickTerminalAnim {
    start: Instant,
    /// `true` while sliding in (revealing), `false` while sliding out (hiding).
    revealing: bool,
}

/// Cubic ease-out (fast start, gentle stop) for the quick-terminal slide.
fn ease_out_cubic(t: f32) -> f32 {
    let inv = 1.0 - t.clamp(0.0, 1.0);
    1.0 - inv * inv * inv
}

/// The panel's top edge in physical px relative to the monitor top, for a
/// slide `progress` in `0.0..=1.0` (0 = fully hidden above the screen, 1 =
/// fully revealed). `height` is the panel height in px.
fn quick_terminal_top_offset(height: f32, progress: f32) -> f32 {
    -height * (1.0 - ease_out_cubic(progress))
}

/// Linear slide progress (`0.0..=1.0`) for `elapsed` of `duration`.
fn quick_terminal_progress(elapsed: Duration, duration: Duration) -> f32 {
    if duration.is_zero() {
        return 1.0;
    }
    (elapsed.as_secs_f32() / duration.as_secs_f32()).clamp(0.0, 1.0)
}

/// The panel height in physical px for a screen `screen_height` px tall and a
/// `size` fraction, clamped to at least one row's worth of pixels.
fn quick_terminal_height(screen_height: u32, size: f32) -> u32 {
    let raw = (screen_height as f32 * size.clamp(0.05, 1.0)).round() as u32;
    raw.clamp(1, screen_height.max(1))
}

/// Compile-time card styling for the Tab Overview composite (REQ-OV-12/14, v2
/// mockup parity; ⚠G: no config knob). Bundles the `tab_overview` color/metric
/// constants into the `noa-render` [`CardStyle`] carrier.
const OVERVIEW_CARD_STYLE: CardStyle = CardStyle {
    background: OVERVIEW_BG_COLOR,
    border_color: OVERVIEW_BORDER_COLOR,
    focus_color: OVERVIEW_FOCUS_RING_COLOR,
    corner_radius: OVERVIEW_CARD_CORNER_RADIUS,
    border_width: OVERVIEW_CARD_BORDER_WIDTH,
    focus_width: OVERVIEW_CARD_FOCUS_WIDTH,
    focus_glow_width: OVERVIEW_CARD_FOCUS_GLOW_WIDTH,
};

/// Rounded styling for Overview chrome pills (search and shortcut hint).
const OVERVIEW_CHROME_CARD_STYLE: CardStyle = CardStyle {
    background: OVERVIEW_BG_COLOR,
    border_color: OVERVIEW_CHROME_BORDER_COLOR,
    focus_color: OVERVIEW_CHROME_BORDER_COLOR,
    corner_radius: OVERVIEW_CARD_CORNER_RADIUS,
    border_width: 1.0,
    focus_width: 1.0,
    focus_glow_width: 0.0,
};

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
    overview_tiles: HashMap<OverviewTileId, OverviewTileRenderState>,
    /// The window the user last interacted with — drives tab/window spawning,
    /// command targets, and the quick terminal. Deliberately *sticky*: it keeps
    /// pointing at the last-focused window while the app is backgrounded so a
    /// global hotkey still has a target. Not a source of truth for "does a
    /// window have OS focus right now" — use [`Self::os_focused`] for that.
    focused: Option<WindowId>,
    /// The window that currently holds real OS focus, or `None` when the whole
    /// app is backgrounded. Unlike [`Self::focused`] this is cleared on
    /// `Focused(false)`, so notification suppression reflects actual focus.
    os_focused: Option<WindowId>,
    #[cfg(target_os = "macos")]
    macos_menu: Option<crate::macos_menu::MacosMenu>,
    /// Monotonic source of [`WindowGroupId`]s; bumped by
    /// [`App::allocate_group_id`] each time a `New Window` starts a fresh tab
    /// group so no two logical windows ever collide.
    next_group_id: u64,
    modifiers: ModifiersState,
    clipboard: SystemClipboard,
    keybinds: KeybindEngine,
    overview_visible: bool,
    /// Shared with every pane's io thread (`io_thread::OverviewPublish`) so
    /// it can gate its opportunistic `FrameSnapshot::peek` publish behind a
    /// single atomic load instead of touching app state (Fix B, REQ-NF-6).
    /// Kept in lockstep with `overview_visible` at every write site.
    overview_visible_gate: Arc<AtomicBool>,
    /// Next scheduled Tab Overview wake-up, set by `redraw_overview`'s
    /// post-frame backlog check (Fix A) when every remaining dirty tile is
    /// merely throttle-blocked rather than due right now. Consumed by
    /// `tick_overview_backlog`, which piggybacks on the cursor-blink
    /// `about_to_wait` + `WaitUntil` wake-up mechanism instead of adding a
    /// second timer source.
    overview_wake_deadline: Option<Instant>,
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
    /// Guards session restore to the first `resumed` only, so a later resume
    /// (e.g. after the whole app is backgrounded and restored) can't spawn a
    /// second copy of the saved topology on top of the live one.
    session_restore_attempted: bool,
    /// Set while rebuilding the saved topology so the per-spawn/-split
    /// `persist_session` calls don't write half-built intermediate sessions
    /// back to disk.
    restoring: bool,
    /// The drop-down quick terminal, if it has ever been opened. Its window
    /// lives in `windows` (so it reuses the whole redraw/input/resize path)
    /// but is deliberately kept out of `window_order`, so it is excluded from
    /// session capture and the tab select/cycle collections — mirroring the
    /// `overview_window` precedent for a non-tab auxiliary window.
    quick_terminal: Option<QuickTerminalState>,
    /// The registered global quick-terminal hotkey, kept alive for the app's
    /// lifetime (dropping it unregisters). `None` until installed, or when no
    /// `quick-terminal-hotkey` is configured / registration failed.
    quick_terminal_hotkey: Option<crate::macos_hotkey::GlobalHotKey>,
    /// Guards global-hotkey registration to a single attempt (it needs a
    /// running NSApplication, so it happens lazily in `about_to_wait`, like
    /// the native menu).
    hotkey_install_attempted: bool,
    /// Secure Keyboard Entry state. Enabled only while both the user has
    /// toggled it on and the app is frontmost, and always released on exit.
    secure_input: crate::secure_input::SecureInput,
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
            os_focused: None,
            #[cfg(target_os = "macos")]
            macos_menu: None,
            next_group_id: 0,
            modifiers: ModifiersState::empty(),
            clipboard: SystemClipboard::new(),
            keybinds: KeybindEngine::default(),
            overview_visible: false,
            overview_visible_gate: Arc::new(AtomicBool::new(false)),
            overview_wake_deadline: None,
            cursor_blink_visible: true,
            cursor_blink_deadline: None,
            hovered_link: None,
            search_prompt: None,
            command_palette: None,
            confirm_dialog: None,
            session_restore_attempted: false,
            restoring: false,
            quick_terminal: None,
            quick_terminal_hotkey: None,
            hotkey_install_attempted: false,
            secure_input: crate::secure_input::SecureInput::new(),
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

    fn kitty_keyboard_flags(&self, window_id: WindowId) -> u8 {
        self.windows
            .get(&window_id)
            .and_then(WindowState::focused_surface)
            .map(|surface| {
                surface
                    .terminal
                    .lock()
                    .expect("terminal mutex poisoned")
                    .kitty_keyboard_flags()
            })
            .unwrap_or(0)
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

    /// Advance the cursor blink phase and report the next instant it needs
    /// another look — `Some(deadline)` only while a blinking cursor is
    /// displayable, `None` otherwise (no busy wake-ups needed). Called from
    /// `about_to_wait` every pass, so a style/focus/visibility change is
    /// picked up on the very next event instead of waiting for a stale
    /// deadline. `about_to_wait` merges this with `tick_overview_backlog`'s
    /// deadline before setting the event loop's `ControlFlow` once.
    fn tick_cursor_blink(&mut self) -> Option<Instant> {
        if !self.focused_cursor_wants_blink() {
            self.cursor_blink_visible = true;
            self.cursor_blink_deadline = None;
            return None;
        }

        let now = Instant::now();
        let deadline = *self
            .cursor_blink_deadline
            .get_or_insert(now + CURSOR_BLINK_INTERVAL);
        if now < deadline {
            return Some(deadline);
        }

        self.cursor_blink_visible = !self.cursor_blink_visible;
        let next = now + CURSOR_BLINK_INTERVAL;
        self.cursor_blink_deadline = Some(next);
        if let Some(window_id) = self.focused
            && let Some(state) = self.windows.get(&window_id)
        {
            state.window.request_redraw();
        }
        Some(next)
    }

    /// Wake the Tab Overview once the earliest throttle-blocked dirty tile
    /// becomes due, instead of `redraw_overview` re-requesting a redraw
    /// every pass while `should_render_tile` keeps rejecting it until
    /// `OVERVIEW_TILE_MIN_RENDER_INTERVAL` has elapsed (Fix A — see
    /// `redraw_overview`'s post-frame backlog check and
    /// `tab_overview::overview_backlog_decision`). Piggybacks on the same
    /// `about_to_wait` + `WaitUntil` wake-up mechanism as
    /// `tick_cursor_blink` rather than adding a second timer source.
    fn tick_overview_backlog(&mut self) -> Option<Instant> {
        let deadline = self.overview_wake_deadline?;
        if Instant::now() < deadline {
            return Some(deadline);
        }
        self.overview_wake_deadline = None;
        self.request_overview_redraw();
        None
    }

    fn handle_app_command(
        &mut self,
        event_loop: &ActiveEventLoop,
        command: AppCommand,
        origin: CommandOrigin,
    ) {
        if overview_should_intercept_command(command, self.overview_visible, origin) {
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
                let _ = self.spawn_tab(event_loop, SpawnTarget::CurrentWindow);
            }
            AppCommand::NewWindow => {
                let _ = self.spawn_tab(event_loop, SpawnTarget::NewWindow);
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
            AppCommand::ToggleQuickTerminal => self.toggle_quick_terminal(event_loop),
            AppCommand::ToggleSecureKeyboardEntry => self.toggle_secure_keyboard_entry(),
            AppCommand::CloseWindow => self.close_window(event_loop),
            AppCommand::Quit => event_loop.exit(),
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

    /// Toggle Secure Keyboard Entry. A toggle only reaches us while the app is
    /// frontmost, so the switch takes effect immediately; focus changes and app
    /// exit reconcile it afterwards. The menu checkmark tracks the user intent.
    fn toggle_secure_keyboard_entry(&mut self) {
        let desired = self
            .secure_input
            .toggle(true, &mut crate::secure_input::CarbonSecureInput);
        #[cfg(target_os = "macos")]
        if let Some(menu) = self.macos_menu.as_ref() {
            menu.set_secure_keyboard_entry_checked(desired);
        }
        let _ = desired;
    }

    fn spawn_tab(
        &mut self,
        event_loop: &ActiveEventLoop,
        target: SpawnTarget,
    ) -> anyhow::Result<WindowId> {
        self.spawn_tab_with_cwd(event_loop, target, None)
    }

    /// Spawn a tab, optionally forcing the initial pane's cwd. `cwd_override`
    /// is `None` for interactive New Tab/New Window (which inherit the focused
    /// shell's cwd) and `Some(cwd)` for session restore, where the initial
    /// pane must open in its saved directory rather than inheriting.
    fn spawn_tab_with_cwd(
        &mut self,
        event_loop: &ActiveEventLoop,
        target: SpawnTarget,
        cwd_override: Option<Option<String>>,
    ) -> anyhow::Result<WindowId> {
        // Inherit the focused shell's cwd before `self.focused` is repointed
        // at the new tab below (unless restore forces a specific cwd).
        let inherited_cwd = match cwd_override {
            Some(cwd) => cwd,
            None => self.focused_pane_cwd(),
        };
        // Resolve which logical window (tab group) this tab joins before the
        // window is created — the macOS `tabbingIdentifier` is baked into the
        // window attributes and can't change afterward.
        let focused_group = self
            .focused
            .and_then(|id| self.windows.get(&id))
            .map(|state| state.group);
        let group = match spawn_group_choice(target, focused_group) {
            GroupChoice::Existing(group) => group,
            GroupChoice::Fresh => self.allocate_group_id(),
        };
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

        let window_attrs = self.tab_window_attributes(inner_size, group);
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
                group,
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
        self.mark_overview_tile_dirty(OverviewTileId::new(window_id, initial_pane));
        self.focused = Some(window_id);
        window.focus_window();
        self.request_overview_redraw();
        self.persist_session();
        Ok(window_id)
    }

    fn tab_window_attributes(
        &self,
        inner_size: LogicalSize<f64>,
        group: WindowGroupId,
    ) -> WindowAttributes {
        let attrs = WindowAttributes::default()
            .with_title("noa")
            .with_inner_size(inner_size)
            // A transparent window is required for `background-opacity` to
            // reveal anything behind it; the surface alpha mode and the
            // renderer's clear alpha carry the actual opacity.
            .with_transparent(self.config.background_opacity < 1.0);
        #[cfg(target_os = "macos")]
        {
            // Tabs in the same group share a `tabbingIdentifier`, so AppKit
            // tabs them into one window; a distinct group id yields a distinct
            // identifier and thus a separate native window.
            let attrs = attrs
                .with_option_as_alt(macos_option_as_alt(self.config.macos_option_as_alt))
                .with_tabbing_identifier(&self.tabbing_identifier(group));
            apply_macos_titlebar_style(attrs, self.config.macos_titlebar_style)
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = group;
            attrs
        }
    }

    /// Mint the next unique [`WindowGroupId`]. Each `New Window` calls this so
    /// its tab group can never collide with an existing window's.
    fn allocate_group_id(&mut self) -> WindowGroupId {
        let id = WindowGroupId(self.next_group_id);
        self.next_group_id += 1;
        id
    }

    /// The macOS `tabbingIdentifier` for a logical window. Per-process
    /// (`std::process::id()`) so two noa instances never merge their tabs, and
    /// per-group so each window keeps a separate tab bar.
    #[cfg(target_os = "macos")]
    fn tabbing_identifier(&self, group: WindowGroupId) -> String {
        format!("noa.tabs.{}.{}", std::process::id(), group.0)
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
        terminal.set_scrollback_limit_bytes(self.config.scrollback_limit);
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
        let overview_snapshot = Arc::new(Mutex::new(None));
        let overview_publish = crate::io_thread::OverviewPublish {
            slot: overview_snapshot.clone(),
            visible: self.overview_visible_gate.clone(),
        };
        let io_thread = crate::io_thread::spawn(
            pty,
            terminal.clone(),
            self.proxy.clone(),
            crate::io_thread::IoThreadTarget { window_id, pane_id },
            resize_rx,
            pty_input_rx,
            overview_publish,
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
            overview_snapshot,
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
        self.overview_tiles
            .retain(|tile_id, _| tile_id.window_id != window_id);
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
        self.persist_session();
    }

    /// Close the entire focused logical window: every tab in its AppKit tab
    /// group (`cmd+shift+w` / File → Close Window). Each tab is torn down via
    /// [`App::close_tab`], so all its per-tab cleanup (io-thread shutdown,
    /// modal/search/palette de-leak, focus repoint) runs, and closing the last
    /// remaining window's last tab still quits the app through
    /// [`TabCloseOutcome::Quit`]. A no-op when nothing is focused.
    fn close_window(&mut self, event_loop: &ActiveEventLoop) {
        let Some(group) = self
            .focused
            .and_then(|id| self.windows.get(&id))
            .map(|state| state.group)
        else {
            return;
        };
        // Snapshot the group's tabs first — `close_tab` mutates `window_order`
        // and `focused` on each call, so iterating it live would be unsound.
        let tabs = ids_in_group(
            &self.window_order,
            |id| self.windows.get(&id).map(|state| state.group),
            group,
        );
        for window_id in tabs {
            self.close_tab(event_loop, window_id);
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
        self.overview_tiles
            .remove(&OverviewTileId::new(window_id, pane_id));
        self.mark_all_overview_tiles_dirty();
        self.relayout_and_resize_window(window_id);
        self.update_focused_ime_cursor_area(window_id);
        window.request_redraw();
        self.request_overview_redraw();
        self.persist_session();
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
        self.mark_all_overview_tiles_dirty();
        self.request_overview_redraw();
        self.persist_session();
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
                // Tile cards and chrome pills are composited through render
                // passes; no direct copy into the surface is required.
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
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
                chrome_card: None,
                selected: 0,
                search_query: String::new(),
            });
        }

        self.overview_visible = true;
        self.overview_visible_gate.store(true, Ordering::Relaxed);
        self.seed_overview_snapshots();
        self.mark_all_overview_tiles_dirty();
        // Reopening the Overview always starts with an empty filter (REQ-OV-16)
        // so the focused-tab initial selection below sees the full tab set.
        if let Some(overview) = self.overview_window.as_mut() {
            overview.search_query.clear();
        }
        // REQ-OV-14: the focused pane's tile if it's live, else the first.
        let source_tile_ids = self.overview_source_tile_ids();
        let live_tile_count = OVERVIEW_GRID_CAP.min(source_tile_ids.len());
        let focused_tile = self.focused.and_then(|window_id| {
            let state = self.windows.get(&window_id)?;
            Some(OverviewTileId::new(window_id, state.focused_pane))
        });
        let selected =
            overview_initial_selection(&source_tile_ids, live_tile_count, focused_tile.as_ref());
        if let Some(overview) = self.overview_window.as_mut() {
            overview.selected = selected;
            overview.window.set_visible(true);
            overview.window.focus_window();
            overview.window.request_redraw();
        }
    }

    /// One-time re-peek for each open pane's overview mirror on every
    /// `show_tab_overview` call (Fix B). Once `overview_visible_gate` is
    /// set, each pane's io thread publishes a fresh `FrameSnapshot::peek`
    /// opportunistically on its own next pty output — but the gate was
    /// clear the whole time the overview was hidden, so a tab that kept
    /// producing output while hidden published nothing during that window,
    /// and its slot holds whatever it last published before hiding (or
    /// `None` on first open). Re-peeking unconditionally here — rather than
    /// only when the slot is still `None` — is what makes reopening show
    /// current content instead of that stale frame; a tab that publishes
    /// on its own moments later just gets overwritten immediately anyway.
    /// Runs once per `show_tab_overview` call, not per frame, so
    /// `render_due_overview_tiles` itself still never locks a pane's
    /// `Terminal`.
    fn seed_overview_snapshots(&self) {
        for state in self.windows.values() {
            for surface in state.surfaces.values() {
                let Some(snapshot) = try_peek_overview_snapshot(&surface.terminal) else {
                    continue;
                };
                *surface
                    .overview_snapshot
                    .lock()
                    .expect("overview snapshot mutex poisoned") = Some(snapshot);
            }
        }
    }

    fn hide_tab_overview(&mut self) {
        self.overview_visible = false;
        self.overview_visible_gate.store(false, Ordering::Relaxed);
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

    fn mark_overview_tile_dirty(&mut self, tile_id: OverviewTileId) {
        self.overview_tiles.entry(tile_id).or_default().dirty = true;
    }

    fn mark_all_overview_tiles_dirty(&mut self) {
        for tile_id in self.overview_source_tile_ids() {
            self.mark_overview_tile_dirty(tile_id);
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

    /// Build the pure due/backlog-decision input from each live source
    /// window's current dirty/last-render tile state. Shared by
    /// `due_overview_tile_ids` (pre-frame selection) and `redraw_overview`
    /// (post-frame backlog check), which read it at different points in
    /// the frame.
    fn overview_tile_candidates(
        &self,
        source_tile_ids: &[OverviewTileId],
    ) -> Vec<OverviewRenderCandidate<OverviewTileId>> {
        source_tile_ids
            .iter()
            .filter_map(|tile_id| {
                let state = self.windows.get(&tile_id.window_id)?;
                if !state.contains_pane(tile_id.pane_id) {
                    return None;
                }
                let tile = self
                    .overview_tiles
                    .get(tile_id)
                    .copied()
                    .unwrap_or_default();
                Some(OverviewRenderCandidate {
                    id: *tile_id,
                    dirty: tile.dirty,
                    last_render_at: tile.last_render_at,
                })
            })
            .collect()
    }

    fn due_overview_tile_ids(
        &self,
        source_tile_ids: &[OverviewTileId],
        now: Instant,
    ) -> Vec<OverviewTileId> {
        let candidates = self.overview_tile_candidates(source_tile_ids);
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
                OVERVIEW_TITLE_BAR_H,
                OVERVIEW_CARD_COLOR,
            ));
        }
    }

    /// Render each due tile's source pane into the shared scratch texture and
    /// blit it down into that pane's tile texture (REQ-OV-4 live mirror,
    /// REQ-NF-1 reuse the tab's own `Renderer`, REQ-NF-3 shared-scratch
    /// blit-downscale). `tile_index` is `source_tile_ids`' position, which
    /// is index-parallel with `layout.tiles` (see `overview_tile_target_at_point`).
    fn render_due_overview_tiles(
        &mut self,
        due_tile_ids: &[OverviewTileId],
        source_tile_ids: &[OverviewTileId],
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

        for &tile_id in due_tile_ids {
            let Some(tile_index) = source_tile_ids.iter().position(|id| *id == tile_id) else {
                continue;
            };
            let Some(state) = self.windows.get_mut(&tile_id.window_id) else {
                continue;
            };
            let Some(surface) = state.surfaces.get(&tile_id.pane_id) else {
                continue;
            };
            let source_viewport = PixelSize {
                w: surface.rect.w.max(1),
                h: surface.rect.h.max(1),
            };
            // Read-only publish slot (Fix B, REQ-NF-6): the io thread
            // already holds `Terminal`'s lock on every pty feed and
            // opportunistically publishes a `FrameSnapshot::peek` there
            // (cursor already hidden by `peek`), so this render path never
            // locks a tab's `Terminal` itself. `None` only for a tab that
            // hasn't published since the overview opened;
            // `seed_overview_snapshots`'s one-time fallback covers that gap.
            let Some(snapshot) = surface
                .overview_snapshot
                .lock()
                .expect("overview snapshot mutex poisoned")
                .clone()
            else {
                continue;
            };

            // Reuse this tab's own `Renderer` unmodified (REQ-NF-1): point it
            // at the source pane's real pixel size just long enough to draw
            // one frame into the Overview scratch texture, then restore its
            // real surface viewport so the tab's own next redraw is unaffected.
            let own_viewport = PixelSize {
                w: state.surface_config.width,
                h: state.surface_config.height,
            };
            state.renderer.resize(source_viewport);
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
                source_viewport,
                tile_index,
            ) {
                log::warn!(
                    "overview tile render failed for {:?}/pane {}: {err:#}",
                    tile_id.window_id,
                    tile_id.pane_id.get()
                );
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

    fn ensure_overview_chrome_card_pipeline(&mut self) {
        let Some(gpu) = self.gpu.as_ref() else {
            return;
        };
        let Some(overview) = self.overview_window.as_mut() else {
            return;
        };
        let format = overview.surface_config.format;
        let stale = overview
            .chrome_card
            .as_ref()
            .is_none_or(|chrome| chrome.format != format);
        if stale {
            overview.chrome_card = Some(OverviewChromeCardPipeline {
                format,
                pipeline: CardPipeline::new(&gpu.device, format),
            });
        }
    }

    /// Draw the source pane label into the top title-bar band of every due
    /// live tile
    /// (REQ-OV-12). Runs after `render_due_overview_tiles`, whose mirror blit
    /// re-clears the band to the card color, so the label must be re-stamped
    /// for exactly the tiles that were re-rendered this frame. Live labels are
    /// routed through the same tested `overview_tile_labels` seam as
    /// placeholder labels (AC-OV-12).
    fn render_due_overview_title_bands(
        &mut self,
        due_tile_ids: &[OverviewTileId],
        source_tile_ids: &[OverviewTileId],
        layout: &OverviewLayout,
    ) {
        let live_count = layout.tiles.len().min(source_tile_ids.len());
        let live_ids = &source_tile_ids[..live_count];
        let labels = overview_tile_labels(live_ids, |id| self.overview_tile_label(id));

        let jobs: Vec<(usize, String)> = labels
            .iter()
            .enumerate()
            .filter(|(index, _)| due_tile_ids.contains(&live_ids[*index]))
            .map(|(index, label)| (index, label.label.clone()))
            .collect();
        for (tile_index, title) in jobs {
            self.render_tile_title_band(tile_index, &title);
        }
    }

    /// Fill every placeholder-row tile (REQ-OV-10) with the card color and its
    /// source label band. Placeholders have no live mirror, so the whole tile is
    /// cleared to the card face before the title band is stamped on top.
    fn render_overview_placeholder_labels(
        &mut self,
        source_tile_ids: &[OverviewTileId],
        layout: &OverviewLayout,
    ) {
        if layout.placeholders.is_empty() {
            return;
        }
        let live_count = layout.tiles.len();
        let overflow_ids = overview_placeholder_source_ids(source_tile_ids, live_count);
        let labels = overview_tile_labels(overflow_ids, |id| self.overview_tile_label(id));

        let jobs: Vec<(usize, String)> = labels
            .iter()
            .enumerate()
            .map(|(index, label)| (live_count + index, label.label.clone()))
            .collect();
        for (tile_index, title) in jobs {
            if let (Some(gpu), Some(overview)) = (self.gpu.as_ref(), self.overview_window.as_ref())
                && let Some(thumbnails) = overview.thumbnails.as_ref()
            {
                thumbnails.clear_tile(&gpu.device, &gpu.queue, tile_index);
            }
            self.render_tile_title_band(tile_index, &title);
        }
    }

    /// Render `title` into `tile_index`'s dedicated title-band texture via the
    /// shared label `Renderer`, then stamp it onto the top `OVERVIEW_TITLE_BAR_H`
    /// rows of the tile (REQ-OV-12). The band is cleared to a distinct
    /// title-bar color (`set_clear_color` after `rebuild_cells`) so it reads as
    /// a band separate from the card face. Shared by live and placeholder tiles.
    fn render_tile_title_band(&mut self, tile_index: usize, title: &str) {
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
        let tile_w = thumbnails.tile_size().w;
        let bar_h = thumbnails.title_bar_h();
        if tile_w == 0 || bar_h == 0 {
            return;
        }
        let band_size = PixelSize {
            w: tile_w.max(1),
            h: bar_h.max(1),
        };
        let grid_size = grid_size_for_pane_rect(
            PaneRectApp::new(0, 0, band_size.w, band_size.h),
            gpu.font.metrics(),
            DEFAULT_GRID_PADDING,
        );
        let sanitized = sanitize_placeholder_label(title, grid_size.cols);
        // REQ-OV-13: the centered title plus a close glyph in the last column.
        let text = title_bar_row_with_close(&sanitized, grid_size.cols);

        let mut term = Terminal::new(GridSize::new(grid_size.cols, 1));
        Stream::new().feed(text.as_bytes(), &mut term);
        let mut snapshot = FrameSnapshot::from_terminal(&mut term);
        snapshot.cursor.visible = false;

        label_renderer.resize(band_size);
        label_renderer.rebuild_cells(&snapshot, &mut gpu.font, &gpu.theme);
        // After `rebuild_cells` (which resets it from the snapshot bg) so the
        // band gets its distinct title-bar color, not the terminal default.
        label_renderer.set_clear_color(OVERVIEW_TITLE_BAR_COLOR);
        label_renderer.sync_atlas(&gpu.device, &gpu.queue, &mut gpu.font);

        let Some(view) = thumbnails.title_texture_view(tile_index) else {
            return;
        };
        label_renderer.draw(&gpu.device, &gpu.queue, &view);
        thumbnails.stamp_title_band(&gpu.device, &gpu.queue, tile_index);
    }

    /// Render the top "Search tabs" field (REQ-OV-16) into a fresh pill-sized
    /// texture and return it for compositing into the reserved top search band.
    /// Shows the live query, or the placeholder while it is empty. `None` when
    /// there is no usable search band (a window too short to reserve one).
    fn render_overview_search_texture(&mut self) -> Option<OverviewChromeTexture> {
        let chrome = self.overview_chrome()?;
        let rect = overview_search_field_rect(chrome.search_band);
        if rect.w == 0 || rect.h == 0 {
            return None;
        }
        let query = self
            .overview_window
            .as_ref()
            .map_or(String::new(), |overview| overview.search_query.clone());
        self.ensure_overview_label_renderer();
        let gpu = self.gpu.as_mut()?;
        let overview = self.overview_window.as_mut()?;
        let label_renderer = overview.label_renderer.as_mut()?;

        let band_size = PixelSize {
            w: rect.w.max(1),
            h: rect.h.max(1),
        };
        let search_texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("noa-overview-search-pill"),
            size: wgpu::Extent3d {
                width: band_size.w,
                height: band_size.h,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: overview.surface_config.format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let view = search_texture.create_view(&wgpu::TextureViewDescriptor::default());

        let grid_size = grid_size_for_pane_rect(
            PaneRectApp::new(0, 0, band_size.w, band_size.h),
            gpu.font.metrics(),
            DEFAULT_GRID_PADDING,
        );
        let text = overview_search_field_row(&query, grid_size.cols);
        let mut term = Terminal::new(GridSize::new(grid_size.cols, 1));
        Stream::new().feed(text.as_bytes(), &mut term);
        let mut snapshot = FrameSnapshot::from_terminal(&mut term);
        snapshot.cursor.visible = false;

        label_renderer.resize(band_size);
        label_renderer.rebuild_cells(&snapshot, &mut gpu.font, &gpu.theme);
        label_renderer.set_clear_color(OVERVIEW_CHROME_PILL_COLOR);
        label_renderer.sync_atlas(&gpu.device, &gpu.queue, &mut gpu.font);
        label_renderer.draw(&gpu.device, &gpu.queue, &view);

        Some(OverviewChromeTexture {
            texture: search_texture,
            rect,
        })
    }

    /// Render the bottom hint bar (REQ-OV-17) into a fresh pill-sized texture
    /// and return it for compositing onto the surface. `None` when there is no
    /// usable hint band (a window too short to reserve one). The `⌘1-N` range
    /// tracks the live tile count dynamically.
    fn render_overview_hint_texture(
        &mut self,
        live_tile_count: usize,
    ) -> Option<OverviewChromeTexture> {
        let chrome = self.overview_chrome()?;
        let rect = overview_hint_bar_rect(chrome.hint_band);
        if rect.w == 0 || rect.h == 0 {
            return None;
        }
        self.ensure_overview_label_renderer();
        let gpu = self.gpu.as_mut()?;
        let overview = self.overview_window.as_mut()?;
        let label_renderer = overview.label_renderer.as_mut()?;

        let band_size = PixelSize {
            w: rect.w.max(1),
            h: rect.h.max(1),
        };
        let hint_texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("noa-overview-hint-pill"),
            size: wgpu::Extent3d {
                width: band_size.w,
                height: band_size.h,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: overview.surface_config.format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let view = hint_texture.create_view(&wgpu::TextureViewDescriptor::default());

        let grid_size = grid_size_for_pane_rect(
            PaneRectApp::new(0, 0, band_size.w, band_size.h),
            gpu.font.metrics(),
            DEFAULT_GRID_PADDING,
        );
        let text = center_label(&overview_hint_bar_text(live_tile_count), grid_size.cols);
        let mut term = Terminal::new(GridSize::new(grid_size.cols, 1));
        Stream::new().feed(text.as_bytes(), &mut term);
        let mut snapshot = FrameSnapshot::from_terminal(&mut term);
        snapshot.cursor.visible = false;

        label_renderer.resize(band_size);
        label_renderer.rebuild_cells(&snapshot, &mut gpu.font, &gpu.theme);
        label_renderer.set_clear_color(OVERVIEW_CHROME_PILL_COLOR);
        label_renderer.sync_atlas(&gpu.device, &gpu.queue, &mut gpu.font);
        label_renderer.draw(&gpu.device, &gpu.queue, &view);

        Some(OverviewChromeTexture {
            texture: hint_texture,
            rect,
        })
    }

    /// Composite every live-mirror and placeholder tile onto the overview
    /// surface as a rounded card (REQ-OV-12/14), then overlay the bottom hint
    /// bar (REQ-OV-17), and present. Empty grid cells stay the backdrop color.
    fn present_overview_frame(&mut self, layout: &OverviewLayout) {
        // Render the hint band first (it borrows the label renderer / gpu
        // mutably); the returned texture is owned, so the borrows are released
        // before compositing.
        let live_count = layout.tiles.len();
        let search_texture = self.render_overview_search_texture();
        let hint_texture = self.render_overview_hint_texture(live_count);
        self.ensure_overview_chrome_card_pipeline();
        let selected = self
            .overview_window
            .as_ref()
            .map_or(0, |overview| overview.selected);

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
        let surface_size = PixelSize {
            w: overview.surface_config.width,
            h: overview.surface_config.height,
        };

        // Card composite (also clears the surface to the backdrop color).
        if let Some(thumbnails) = overview.thumbnails.as_ref() {
            let live_count = layout.tiles.len();
            let placeholders = layout
                .placeholders
                .iter()
                .enumerate()
                .map(|(index, rect)| (live_count + index, rect));
            let placements: Vec<CardTilePlacement> = layout
                .tiles
                .iter()
                .enumerate()
                .chain(placeholders)
                .map(|(tile_index, rect)| CardTilePlacement {
                    tile_index,
                    x: rect.x,
                    y: rect.y,
                    w: rect.w,
                    h: rect.h,
                    selected: tile_index == selected,
                })
                .collect();
            thumbnails.composite_cards(
                &gpu.device,
                &gpu.queue,
                &view,
                surface_size,
                &OVERVIEW_CARD_STYLE,
                &placements,
            );
        } else {
            // No tiles: still clear the surface to the backdrop color.
            clear_overview_surface(&gpu.device, &gpu.queue, &view, OVERVIEW_BG_COLOR);
        }

        // Overlay the search and hint pills with the same rounded-card shader
        // as tiles, but without clearing the already-composited frame.
        if let Some(chrome_card) = overview.chrome_card.as_ref() {
            let search_view = search_texture.as_ref().map(|chrome| {
                chrome
                    .texture
                    .create_view(&wgpu::TextureViewDescriptor::default())
            });
            let hint_view = hint_texture.as_ref().map(|chrome| {
                chrome
                    .texture
                    .create_view(&wgpu::TextureViewDescriptor::default())
            });
            let mut placements = Vec::new();
            if let (Some(chrome), Some(view)) = (search_texture.as_ref(), search_view.as_ref()) {
                placements.push(CardTexturePlacement {
                    texture_view: view,
                    x: chrome.rect.x,
                    y: chrome.rect.y,
                    w: chrome.rect.w,
                    h: chrome.rect.h,
                    selected: false,
                });
            }
            if let (Some(chrome), Some(view)) = (hint_texture.as_ref(), hint_view.as_ref()) {
                placements.push(CardTexturePlacement {
                    texture_view: view,
                    x: chrome.rect.x,
                    y: chrome.rect.y,
                    w: chrome.rect.w,
                    h: chrome.rect.h,
                    selected: false,
                });
            }
            chrome_card.pipeline.overlay_texture_cards(
                &gpu.device,
                &gpu.queue,
                &view,
                surface_size,
                &OVERVIEW_CHROME_CARD_STYLE,
                &placements,
            );
        }

        frame.present();
    }

    fn finish_overview_tile_renders(&mut self, tile_ids: &[OverviewTileId], now: Instant) {
        for tile_id in tile_ids {
            let tile = self.overview_tiles.entry(*tile_id).or_default();
            tile.dirty = false;
            tile.last_render_at = Some(now);
        }
    }

    fn overview_source_tile_ids(&self) -> Vec<OverviewTileId> {
        let ordered = overview_tile_source_order(
            &self.window_order,
            |id| self.windows.contains_key(&id),
            |id| self.overview_pane_ids_for_window(id),
            None,
        )
        .into_iter()
        .map(|(window_id, pane_id)| OverviewTileId::new(window_id, pane_id))
        .collect::<Vec<_>>();
        // REQ-OV-16: the "Search tabs" filter narrows the source set here, the
        // single seam every downstream consumer (redraw / hit-test / nav /
        // Cmd+N / title bars / placeholders) reads, so the whole Overview sees
        // one filtered order. An empty query is the identity (short-circuited
        // to skip cloning titles on the common path).
        let query = self
            .overview_window
            .as_ref()
            .map_or("", |overview| overview.search_query.as_str());
        if query.is_empty() {
            return ordered;
        }
        let titles: Vec<(OverviewTileId, String)> = ordered
            .iter()
            .map(|id| {
                let title = self.overview_tile_label(*id).unwrap_or_default();
                (*id, title)
            })
            .collect();
        overview_tab_filter(query, &titles)
    }

    fn overview_pane_ids_for_window(&self, window_id: WindowId) -> Vec<PaneId> {
        let Some(state) = self.windows.get(&window_id) else {
            return Vec::new();
        };
        split_tree::compute_layout(&state.split_tree, PaneRectApp::new(0, 0, 1001, 1001))
            .into_iter()
            .filter_map(|(pane_id, _)| state.contains_pane(pane_id).then_some(pane_id))
            .collect()
    }

    fn overview_tile_label(&self, tile_id: OverviewTileId) -> Option<String> {
        let state = self.windows.get(&tile_id.window_id)?;
        if !state.contains_pane(tile_id.pane_id) {
            return None;
        }
        let title = state.title.clone();
        if state.pane_count() <= 1 {
            return Some(title);
        }
        let pane_number = self
            .overview_pane_ids_for_window(tile_id.window_id)
            .iter()
            .position(|pane_id| *pane_id == tile_id.pane_id)
            .map(|index| index + 1)
            .unwrap_or_else(|| tile_id.pane_id.get() as usize);
        Some(format!("{title} [pane {pane_number}]"))
    }

    /// The Overview window's search / grid / hint bands (REQ-OV-11/16/17).
    /// The grid is laid out inside `grid_bounds`, so P3's search-field draw
    /// won't reflow the tiles, and the hint bar draws into `hint_band`.
    fn overview_chrome(&self) -> Option<OverviewChrome> {
        let overview = self.overview_window.as_ref()?;
        let bounds = pane_bounds_for_size(overview.window.inner_size());
        Some(overview_chrome_bands(bounds))
    }

    fn overview_layout(&self, source_tile_ids: &[OverviewTileId]) -> Option<OverviewLayout> {
        let chrome = self.overview_chrome()?;
        Some(compute_overview_grid(
            source_tile_ids.len(),
            chrome.grid_bounds,
            OVERVIEW_GRID_CAP,
            OVERVIEW_TILE_GUTTER,
            OVERVIEW_OUTER_MARGIN,
        ))
    }

    fn redraw_overview(&mut self) {
        let Some(overview) = self.overview_window.as_ref() else {
            return;
        };
        if !self.overview_visible || overview.occluded {
            return;
        }

        let source_tile_ids = self.overview_source_tile_ids();
        let Some(layout) = self.overview_layout(&source_tile_ids) else {
            return;
        };
        // REQ-OV-14: keep the selection in range as source panes come and go.
        if let Some(overview) = self.overview_window.as_mut() {
            overview.selected = overview
                .selected
                .min(source_tile_ids.len().saturating_sub(1));
        }
        let now = Instant::now();
        let due_tile_ids = self.due_overview_tile_ids(&source_tile_ids, now);

        self.ensure_overview_thumbnails(&layout);
        self.render_due_overview_tiles(&due_tile_ids, &source_tile_ids);
        self.render_due_overview_title_bands(&due_tile_ids, &source_tile_ids, &layout);
        self.render_overview_placeholder_labels(&source_tile_ids, &layout);
        self.present_overview_frame(&layout);

        self.finish_overview_tile_renders(&due_tile_ids, now);

        // OVERVIEW_MAX_RENDER_TILES_PER_FRAME caps how many tiles one frame
        // regenerates, and idle tabs produce no pty output to trigger the
        // next frame — so a dirty backlog can survive this frame for two
        // different reasons, and only one of them justifies re-requesting a
        // redraw right away (Fix A): a due-but-capped tile (immediate), vs.
        // a tile that is merely inside its 10Hz throttle window (schedule
        // one delayed wake-up via `tick_overview_backlog` instead of
        // spinning `present_overview_frame` until it's due).
        let candidates = self.overview_tile_candidates(&source_tile_ids);
        let decision =
            overview_backlog_decision(&candidates, now, OVERVIEW_TILE_MIN_RENDER_INTERVAL);
        if decision.request_immediate_redraw {
            self.overview_wake_deadline = None;
            self.request_overview_redraw();
        } else {
            self.overview_wake_deadline = decision.wake_at;
        }
    }

    fn focus_overview_tile_at_last_cursor(&mut self) {
        let Some(overview) = self.overview_window.as_ref() else {
            return;
        };
        let Some(point) = overview.last_cursor_point else {
            return;
        };

        let source_tile_ids = self.overview_source_tile_ids();
        let Some(layout) = self.overview_layout(&source_tile_ids) else {
            return;
        };
        let Some(target) = overview_tile_target_at_point(&source_tile_ids, &layout.tiles, point)
        else {
            return;
        };
        // The clicked tile becomes the selection too, not just the focus
        // target — a click and an arrow-keyed Return should leave the
        // Overview in the same selected state.
        if let Some(index) = source_tile_ids.iter().position(|id| *id == target)
            && let Some(overview) = self.overview_window.as_mut()
        {
            overview.selected = index;
        }
        self.focus_tile_from_overview(target);
    }

    /// The close-button (✕) target under the last cursor point, or `None`
    /// (REQ-OV-13). Spans live tiles and placeholder rows — both carry a title
    /// bar with a close button, and both map back to a live source pane.
    fn overview_close_target_at_last_cursor(&self) -> Option<OverviewTileId> {
        let overview = self.overview_window.as_ref()?;
        let point = overview.last_cursor_point?;
        let source_tile_ids = self.overview_source_tile_ids();
        let layout = self.overview_layout(&source_tile_ids)?;
        let tile_rects: Vec<PaneRectApp> = layout
            .tiles
            .iter()
            .chain(layout.placeholders.iter())
            .copied()
            .collect();
        overview_close_target_at_point(&source_tile_ids, &tile_rects, point)
    }

    fn focus_tile_from_overview(&mut self, tile_id: OverviewTileId) {
        let Some(window) = self
            .windows
            .get(&tile_id.window_id)
            .map(|state| state.window.clone())
        else {
            return;
        };
        self.focus_pane(tile_id.window_id, tile_id.pane_id);
        self.focused = Some(tile_id.window_id);
        window.focus_window();
    }

    /// Drives the Overview-focused keymap directly from the keypress
    /// (REQ-OV-15), mirroring `handle_search_prompt_key`'s
    /// keypress-interception shape: arrows/Return/Esc/Cmd+1..9 are resolved
    /// here and never reach `handle_app_command`, so they can't be swallowed
    /// by `overview_command_scope`'s blanket `AppCommand` no-op. Every other
    /// key falls through to the normal keybind-resolve path, which still
    /// classifies terminal commands as Overview no-ops (REQ-OV-7).
    fn handle_overview_key(&mut self, event_loop: &ActiveEventLoop, event: &KeyEvent) {
        if let Some(action) = overview_key_action(&event.logical_key, self.modifiers) {
            match action {
                OverviewAction::MoveSelection(direction) => self.step_overview_selection(direction),
                OverviewAction::Activate => self.activate_overview_selection(),
                OverviewAction::SwitchToLive(n) => self.switch_to_live_overview_tile(n),
                OverviewAction::Dismiss => self.dismiss_or_clear_overview_search(),
            }
            return;
        }
        // Printable text / Backspace edits the "Search tabs" query (REQ-OV-16),
        // slotted after the Overview action keymap (arrows/Return/Esc/Cmd+N win)
        // and before the normal keybind fallthrough. Nothing here reaches a pty
        // (REQ-OV-7).
        if self.apply_overview_search_edit(event) {
            return;
        }
        if let Some(command) = self.keybinds.resolve(&event.logical_key, self.modifiers) {
            self.handle_app_command(event_loop, command, CommandOrigin::OverviewWindow);
        }
    }

    /// Escape while the Overview is focused (REQ-OV-16): a non-empty search
    /// query is cleared first and the Overview stays open, an empty query
    /// dismisses it (two-stage Escape; no command-palette precedent, so the
    /// semantics are defined by `overview_escape_action`).
    fn dismiss_or_clear_overview_search(&mut self) {
        let query = self
            .overview_window
            .as_ref()
            .map_or("", |overview| overview.search_query.as_str());
        match overview_escape_action(query) {
            OverviewEscapeAction::ClearSearch => self.set_overview_search_query(String::new()),
            OverviewEscapeAction::Dismiss => self.hide_tab_overview(),
        }
    }

    /// Apply a printable-text append or Backspace pop to the "Search tabs"
    /// query (REQ-OV-16). Returns `true` when the key was consumed as a query
    /// edit. Cmd/Ctrl/Alt combos are not swallowed here (they fall through to
    /// the keybind path, mirroring the command palette's Cmd-swallow), so e.g.
    /// the Overview toggle chord still works while typing.
    fn apply_overview_search_edit(&mut self, event: &KeyEvent) -> bool {
        let Some(mut query) = self
            .overview_window
            .as_ref()
            .map(|overview| overview.search_query.clone())
        else {
            return false;
        };
        match &event.logical_key {
            Key::Named(NamedKey::Backspace) => {
                if query.pop().is_none() {
                    // Already empty: still consumed (Backspace has no other
                    // meaning in the Overview) but no redraw is needed.
                    return true;
                }
            }
            _ => {
                if self.modifiers.super_key()
                    || self.modifiers.control_key()
                    || self.modifiers.alt_key()
                {
                    return false;
                }
                let Some(text) = event.text.as_deref() else {
                    return false;
                };
                let mut appended = false;
                for c in text.chars().filter(|c| !c.is_control()) {
                    query.push(c);
                    appended = true;
                }
                if !appended {
                    return false;
                }
            }
        }
        self.set_overview_search_query(query);
        true
    }

    /// Replace the search query, reset the selection to the first tile (a
    /// query change re-orders the result set, REQ-OV-16 / palette R-7 parity),
    /// and request a redraw.
    fn set_overview_search_query(&mut self, query: String) {
        if let Some(overview) = self.overview_window.as_mut() {
            overview.search_query = query;
            overview.selected = 0;
        } else {
            return;
        }
        // Filtering remaps each window to a new tile slot, so the now-visible
        // set must re-render into those slots instead of showing the previous
        // ordering's stale mirrors. Re-rendering still flows through the 10Hz
        // throttle (REQ-NF-4), so tiles refresh at the next due tick.
        self.mark_all_overview_tiles_dirty();
        self.request_overview_redraw();
    }

    /// Arrow-key Overview selection move (REQ-OV-15a).
    fn step_overview_selection(&mut self, direction: Direction) {
        let source_tile_ids = self.overview_source_tile_ids();
        let Some(layout) = self.overview_layout(&source_tile_ids) else {
            return;
        };
        let Some(overview) = self.overview_window.as_mut() else {
            return;
        };
        overview.selected = move_overview_selection(
            overview.selected,
            layout.cols,
            source_tile_ids.len(),
            direction,
        );
        overview.window.request_redraw();
    }

    /// Return activates the selected Overview tile (REQ-OV-15b). `selected`
    /// indexes directly into the combined live + placeholder source order,
    /// so a selected placeholder row resolves to its source pane exactly the
    /// same way a selected live tile does.
    fn activate_overview_selection(&mut self) {
        let source_tile_ids = self.overview_source_tile_ids();
        let Some(overview) = self.overview_window.as_ref() else {
            return;
        };
        let Some(&target) = source_tile_ids.get(overview.selected) else {
            return;
        };
        self.focus_tile_from_overview(target);
    }

    /// Cmd+`n` (1-indexed) jumps straight to the `n`-th live Overview tile
    /// (REQ-OV-15c). Out-of-range `n` (beyond the live tile count) is a
    /// no-op rather than a panic — there is no tile to switch to.
    fn switch_to_live_overview_tile(&mut self, n: usize) {
        let source_tile_ids = self.overview_source_tile_ids();
        let live_tile_count = OVERVIEW_GRID_CAP.min(source_tile_ids.len());
        if n == 0 || n > live_tile_count {
            return;
        }
        let target = source_tile_ids[n - 1];
        if let Some(overview) = self.overview_window.as_mut() {
            overview.selected = n - 1;
        }
        self.focus_tile_from_overview(target);
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
                    // REQ-OV-13: the close-button corner wins over tile-focus.
                    // Close the targeted pane; `close_pane` falls back to
                    // closing the tab when it was the last pane.
                    if let Some(target) = self.overview_close_target_at_last_cursor() {
                        self.close_pane(event_loop, target.window_id, target.pane_id);
                    } else {
                        self.focus_overview_tile_at_last_cursor();
                    }
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
                self.handle_overview_key(event_loop, &event);
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

/// Session persistence (`window-save-state`): capture the live window/tab/split
/// topology + per-pane cwd, and rebuild it on launch.
impl App {
    /// Capture the current topology into a serializable [`session::SessionState`].
    /// Windows are grouped by their AppKit tab group (`WindowGroupId`) so each
    /// logical window carries its tabs; the focused window/tab/pane are recorded
    /// as indices into that structure.
    fn capture_session(&self) -> session::SessionState {
        // Group native tabs by logical window, preserving `window_order`.
        let mut groups: Vec<(WindowGroupId, Vec<WindowId>)> = Vec::new();
        for window_id in &self.window_order {
            let Some(state) = self.windows.get(window_id) else {
                continue;
            };
            match groups.iter_mut().find(|(group, _)| *group == state.group) {
                Some((_, tabs)) => tabs.push(*window_id),
                None => groups.push((state.group, vec![*window_id])),
            }
        }

        let focused_group = self
            .focused
            .and_then(|id| self.windows.get(&id))
            .map(|s| s.group);
        let focused_window =
            focused_group.and_then(|group| groups.iter().position(|(g, _)| *g == group));

        let windows = groups
            .iter()
            .map(|(_, tabs)| self.capture_window(tabs))
            .collect();

        session::SessionState {
            windows,
            focused_window,
        }
    }

    fn capture_window(&self, tabs: &[WindowId]) -> session::WindowSession {
        let frame = tabs
            .first()
            .and_then(|id| self.windows.get(id))
            .map(|state| capture_window_frame(&state.window));
        let focused_tab = self
            .focused
            .and_then(|focused| tabs.iter().position(|id| *id == focused))
            .unwrap_or(0);
        let tab_sessions = tabs
            .iter()
            .filter_map(|id| {
                let state = self.windows.get(id)?;
                Some(self.capture_tab(*id, state))
            })
            .collect();
        session::WindowSession {
            frame,
            focused_tab,
            tabs: tab_sessions,
        }
    }

    fn capture_tab(&self, window_id: WindowId, state: &WindowState) -> session::TabSession {
        let split = self.split_tree_to_node(window_id, &state.split_tree);
        let mut leaves = Vec::new();
        collect_leaf_ids(&state.split_tree, &mut leaves);
        let focused_leaf = leaves
            .iter()
            .position(|pane| *pane == state.focused_pane)
            .unwrap_or(0);
        session::TabSession {
            focused_leaf,
            split,
        }
    }

    fn split_tree_to_node(&self, window_id: WindowId, tree: &SplitTree) -> session::PaneNode {
        match tree {
            SplitTree::Leaf { pane } => session::PaneNode::Leaf {
                cwd: self.pane_cwd(window_id, *pane),
            },
            SplitTree::Split {
                orientation,
                ratio,
                first,
                second,
            } => session::PaneNode::Split {
                orientation: orientation_to_session(*orientation),
                ratio: *ratio,
                first: Box::new(self.split_tree_to_node(window_id, first)),
                second: Box::new(self.split_tree_to_node(window_id, second)),
            },
        }
    }

    /// Persist the current session to disk (atomic). A no-op while restoring,
    /// when `window-save-state = never`, or when no windows are live — the last
    /// case deliberately leaves the previously written file intact so the
    /// close-last-window path still restores that final window next launch.
    fn persist_session(&mut self) {
        if self.restoring || !self.config.window_save_state.restores() || self.windows.is_empty() {
            return;
        }
        let Some(path) = noa_config::session_state_path() else {
            return;
        };
        let state = self.capture_session();
        if let Err(err) = session::save(&path, &state) {
            log::warn!("failed to save session state: {err}");
        }
    }

    /// Restore the saved session on launch, if enabled and present. Suppressed
    /// entirely by `window-save-state = never` or an explicit CLI grid size. A
    /// missing/malformed/empty file is a silent no-op — startup is never
    /// blocked by session state.
    fn restore_session_if_enabled(&mut self, event_loop: &ActiveEventLoop) {
        if !self.config.window_save_state.restores() || self.config.cli_grid_override {
            return;
        }
        let Some(path) = noa_config::session_state_path() else {
            return;
        };
        let Some(state) = session::load(&path) else {
            return;
        };
        if state.windows.is_empty() {
            return;
        }
        self.restoring = true;
        self.restore_session(event_loop, &state);
        self.restoring = false;
    }

    fn restore_session(&mut self, event_loop: &ActiveEventLoop, state: &session::SessionState) {
        // One entry per saved logical window: the native-tab `WindowId`s
        // spawned for it, in tab order, used to restore focus at the end.
        let mut restored_groups: Vec<Vec<WindowId>> = Vec::new();
        for window in &state.windows {
            let mut tab_ids = Vec::new();
            for tab in &window.tabs {
                // The first tab starts a fresh logical window (tab group);
                // the rest join it, matching how `new tab` vs `new window`
                // pick a group.
                let target = if tab_ids.is_empty() {
                    SpawnTarget::NewWindow
                } else {
                    SpawnTarget::CurrentWindow
                };
                let first_leaf_cwd = tab.split.first_leaf_cwd();
                let window_id =
                    match self.spawn_tab_with_cwd(event_loop, target, Some(first_leaf_cwd)) {
                        Ok(window_id) => window_id,
                        Err(err) => {
                            log::warn!("session restore: failed to spawn tab: {err}");
                            continue;
                        }
                    };
                tab_ids.push(window_id);
                self.materialize_tab(window_id, tab);
            }
            if let Some(first) = tab_ids.first() {
                self.apply_window_frame(*first, window.frame.as_ref());
            }
            restored_groups.push(tab_ids);
        }

        if let Some(focused_window) = state.focused_window
            && let (Some(group), Some(saved)) = (
                restored_groups.get(focused_window),
                state.windows.get(focused_window),
            )
            && let Some(window_id) = group.get(saved.focused_tab).or_else(|| group.first())
        {
            self.focused = Some(*window_id);
            if let Some(target) = self.windows.get(window_id) {
                target.window.clone().focus_window();
            }
        }
    }

    /// Rebuild a tab's saved split topology onto its just-spawned single pane.
    /// The initial pane becomes the tree's first (left-most) leaf — its cwd was
    /// already set at spawn — and fresh panes are spawned for every other leaf.
    fn materialize_tab(&mut self, window_id: WindowId, tab: &session::TabSession) {
        if tab.split.leaf_count() <= 1 {
            return;
        }
        let Some((root_pane, next_pane_id, placeholder_rect)) =
            self.windows.get(&window_id).map(|state| {
                (
                    state.focused_pane,
                    state.next_pane_id,
                    PaneRectApp::new(
                        0,
                        0,
                        state.surface_config.width,
                        state.surface_config.height,
                    ),
                )
            })
        else {
            return;
        };

        let mut minter = PaneMinter {
            next: next_pane_id,
            root: Some(root_pane),
        };
        let mut leaves = Vec::new();
        let tree = build_split_tree(&tab.split, &mut minter, &mut leaves);

        // Spawn surfaces for the non-root leaves before mutating window state
        // (`spawn_pane_surface` borrows `&self`). A rough grid/rect is fine —
        // `relayout_and_resize_window` fixes every pane's geometry below.
        let placeholder_grid = GridSize::new(self.config.cols, self.config.rows);
        let mut spawned = Vec::new();
        for leaf in &leaves {
            if leaf.is_root {
                continue;
            }
            match self.spawn_pane_surface(
                window_id,
                leaf.pane,
                placeholder_grid,
                placeholder_rect,
                leaf.cwd.clone(),
            ) {
                Ok(surface) => spawned.push((leaf.pane, surface)),
                Err(err) => log::warn!("session restore: failed to spawn split pane: {err}"),
            }
        }

        let focused_pane = leaves
            .get(tab.focused_leaf)
            .map(|leaf| leaf.pane)
            .unwrap_or(root_pane);
        if let Some(state) = self.windows.get_mut(&window_id) {
            state.split_tree = tree;
            state.next_pane_id = minter.next;
            for (pane, surface) in spawned {
                state.surfaces.insert(pane, surface);
            }
            if state.surfaces.contains_key(&focused_pane) {
                state.focused_pane = focused_pane;
                state.last_mouse_pane = Some(focused_pane);
            }
        }
        self.relayout_and_resize_window(window_id);
    }

    fn apply_window_frame(&self, window_id: WindowId, frame: Option<&session::WindowFrame>) {
        let Some(frame) = frame else {
            return;
        };
        let Some(state) = self.windows.get(&window_id) else {
            return;
        };
        if let Some((x, y)) = frame.position {
            state.window.set_outer_position(LogicalPosition::new(x, y));
        }
        let _ = state
            .window
            .request_inner_size(LogicalSize::new(frame.width, frame.height));
    }
}

/// Mints pane ids while rebuilding a saved split tree, handing out the existing
/// initial pane (`root`) for the first leaf and fresh sequential ids after.
struct PaneMinter {
    next: u64,
    root: Option<PaneId>,
}

impl PaneMinter {
    /// Returns the next pane id and whether it is the reused initial pane.
    fn mint(&mut self) -> (PaneId, bool) {
        match self.root.take() {
            Some(pane) => (pane, true),
            None => {
                let pane = PaneId::new(self.next);
                self.next += 1;
                (pane, false)
            }
        }
    }
}

/// A leaf of a rebuilt split tree: its minted pane id, saved cwd, and whether
/// it reuses the tab's initial pane (whose surface already exists).
struct LeafSpec {
    pane: PaneId,
    cwd: Option<String>,
    is_root: bool,
}

/// Build a `SplitTree` from a saved [`session::PaneNode`], minting pane ids and
/// collecting the leaves in pre-order (matching [`collect_leaf_ids`] and the
/// serialized `focused_leaf` index).
fn build_split_tree(
    node: &session::PaneNode,
    minter: &mut PaneMinter,
    leaves: &mut Vec<LeafSpec>,
) -> SplitTree {
    match node {
        session::PaneNode::Leaf { cwd } => {
            let (pane, is_root) = minter.mint();
            leaves.push(LeafSpec {
                pane,
                cwd: cwd.clone(),
                is_root,
            });
            SplitTree::leaf(pane)
        }
        session::PaneNode::Split {
            orientation,
            ratio,
            first,
            second,
        } => {
            let first_tree = build_split_tree(first, minter, leaves);
            let second_tree = build_split_tree(second, minter, leaves);
            SplitTree::split(
                orientation_from_session(*orientation),
                *ratio,
                first_tree,
                second_tree,
            )
        }
    }
}

fn collect_leaf_ids(tree: &SplitTree, out: &mut Vec<PaneId>) {
    match tree {
        SplitTree::Leaf { pane } => out.push(*pane),
        SplitTree::Split { first, second, .. } => {
            collect_leaf_ids(first, out);
            collect_leaf_ids(second, out);
        }
    }
}

fn orientation_to_session(orientation: SplitOrientation) -> session::Orientation {
    match orientation {
        SplitOrientation::Horizontal => session::Orientation::Horizontal,
        SplitOrientation::Vertical => session::Orientation::Vertical,
    }
}

fn orientation_from_session(orientation: session::Orientation) -> SplitOrientation {
    match orientation {
        session::Orientation::Horizontal => SplitOrientation::Horizontal,
        session::Orientation::Vertical => SplitOrientation::Vertical,
    }
}

/// Read a window's logical-pixel frame (scale-independent) for persistence.
/// The position may be unavailable on some platforms; the size always is.
fn capture_window_frame(window: &Window) -> session::WindowFrame {
    let scale = window.scale_factor();
    let size = window.inner_size().to_logical::<f64>(scale);
    let position = window
        .outer_position()
        .ok()
        .map(|position| position.to_logical::<f64>(scale))
        .map(|position| (position.x, position.y));
    session::WindowFrame {
        position,
        width: size.width,
        height: size.height,
    }
}

/// Quick terminal (drop-down) support.
impl App {
    fn is_quick_terminal_window(&self, window_id: WindowId) -> bool {
        self.quick_terminal
            .as_ref()
            .is_some_and(|qt| qt.window_id == window_id)
    }

    /// Register the global `quick-terminal-hotkey` once, after the app is
    /// running. A no-op when unset, already attempted, or registration failed
    /// (the failure is logged, not fatal).
    fn install_global_hotkey_if_needed(&mut self) {
        if self.hotkey_install_attempted {
            return;
        }
        self.hotkey_install_attempted = true;
        let Some(spec) = self.config.quick_terminal_hotkey.clone() else {
            return;
        };
        match crate::macos_hotkey::GlobalHotKey::register(&spec, self.proxy.clone()) {
            Some(hotkey) => self.quick_terminal_hotkey = Some(hotkey),
            None => log::warn!("failed to register quick-terminal-hotkey `{spec}`"),
        }
    }

    /// Toggle the quick terminal: reveal it (creating its window on first use)
    /// when hidden, slide it away when shown. A no-op before the GPU exists
    /// (i.e. before the first real window), which also means it can't be the
    /// app's only window.
    fn toggle_quick_terminal(&mut self, event_loop: &ActiveEventLoop) {
        if self.gpu.is_none() {
            return;
        }
        match self.quick_terminal.as_ref() {
            Some(qt) if qt.visible => self.start_quick_terminal_hide(),
            _ => self.start_quick_terminal_show(event_loop),
        }
    }

    /// The target monitor's origin and the panel's full-width × fractional-
    /// height footprint, all in physical pixels.
    fn quick_terminal_geometry(
        &self,
        event_loop: &ActiveEventLoop,
    ) -> Option<(i32, i32, u32, u32)> {
        let monitor = event_loop.primary_monitor().or_else(|| {
            self.focused_window()
                .and_then(|window| window.current_monitor())
        })?;
        let position = monitor.position();
        let size = monitor.size();
        let height = quick_terminal_height(size.height, self.config.quick_terminal_size);
        Some((position.x, position.y, size.width, height))
    }

    fn start_quick_terminal_show(&mut self, event_loop: &ActiveEventLoop) {
        let Some((origin_x, top_y, width, height)) = self.quick_terminal_geometry(event_loop)
        else {
            return;
        };
        if self.quick_terminal.is_none() {
            let Some(window_id) =
                self.create_quick_terminal(event_loop, origin_x, top_y, width, height)
            else {
                return;
            };
            self.quick_terminal = Some(QuickTerminalState {
                window_id,
                origin_x,
                top_y,
                width,
                height,
                visible: false,
                anim: None,
            });
        } else if let Some(qt) = self.quick_terminal.as_mut() {
            // Re-derive geometry each open: the active monitor (or its
            // resolution) may have changed since last time.
            qt.origin_x = origin_x;
            qt.top_y = top_y;
            qt.width = width;
            qt.height = height;
            if let Some(state) = self.windows.get(&qt.window_id) {
                let _ = state
                    .window
                    .request_inner_size(PhysicalSize::new(width, height));
            }
        }

        let Some(qt) = self.quick_terminal.as_mut() else {
            return;
        };
        qt.visible = true;
        qt.anim = Some(QuickTerminalAnim {
            start: Instant::now(),
            revealing: true,
        });
        let window_id = qt.window_id;
        let hidden_top = top_y + quick_terminal_top_offset(height as f32, 0.0).round() as i32;
        if let Some(state) = self.windows.get(&window_id) {
            state
                .window
                .set_outer_position(PhysicalPosition::new(origin_x, hidden_top));
            state.window.set_visible(true);
            state.window.focus_window();
            state.window.request_redraw();
        }
        self.focused = Some(window_id);
    }

    fn start_quick_terminal_hide(&mut self) {
        let Some(qt) = self.quick_terminal.as_mut() else {
            return;
        };
        let already_hiding = !qt.visible && qt.anim.as_ref().is_some_and(|anim| !anim.revealing);
        if already_hiding {
            return;
        }
        qt.visible = false;
        qt.anim = Some(QuickTerminalAnim {
            start: Instant::now(),
            revealing: false,
        });
        let window_id = qt.window_id;
        if let Some(state) = self.windows.get(&window_id) {
            state.window.request_redraw();
        }
    }

    /// Hide the quick terminal when it loses focus, if `quick-terminal-autohide`
    /// is enabled. Called from the window's `Focused(false)` event.
    fn maybe_autohide_quick_terminal(&mut self) {
        if !self.config.quick_terminal_autohide {
            return;
        }
        if self.quick_terminal.as_ref().is_some_and(|qt| qt.visible) {
            self.start_quick_terminal_hide();
        }
    }

    /// Advance the slide, repositioning the window each frame. Reports the next
    /// wake instant while animating (folded into `about_to_wait`'s deadline),
    /// and `None` once the slide settles — hiding the window on a completed
    /// slide-out.
    fn tick_quick_terminal(&mut self) -> Option<Instant> {
        let (window_id, origin_x, top_y, height, start, revealing) = {
            let qt = self.quick_terminal.as_ref()?;
            let anim = qt.anim.as_ref()?;
            (
                qt.window_id,
                qt.origin_x,
                qt.top_y,
                qt.height,
                anim.start,
                anim.revealing,
            )
        };
        let now = Instant::now();
        let progress =
            quick_terminal_progress(now.duration_since(start), QUICK_TERMINAL_SLIDE_DURATION);
        let reveal = if revealing { progress } else { 1.0 - progress };
        let top = top_y + quick_terminal_top_offset(height as f32, reveal).round() as i32;
        if let Some(state) = self.windows.get(&window_id) {
            state
                .window
                .set_outer_position(PhysicalPosition::new(origin_x, top));
            state.window.request_redraw();
        }
        if progress >= 1.0 {
            if let Some(qt) = self.quick_terminal.as_mut() {
                qt.anim = None;
            }
            if !revealing && let Some(state) = self.windows.get(&window_id) {
                state.window.set_visible(false);
            }
            return None;
        }
        Some(now + QUICK_TERMINAL_FRAME_INTERVAL)
    }

    /// Tear down the quick terminal outright (its shell exited). Unlike hide,
    /// this drops the window and io thread so a fresh one is spawned next open.
    fn destroy_quick_terminal(&mut self) {
        let Some(qt) = self.quick_terminal.take() else {
            return;
        };
        if let Some(mut state) = self.windows.remove(&qt.window_id) {
            state.shutdown();
        }
        if self.focused == Some(qt.window_id) {
            self.focused = self.window_order.last().copied();
            if let Some(window_id) = self.focused
                && let Some(state) = self.windows.get(&window_id)
            {
                state.window.focus_window();
            }
        }
    }

    /// Build the quick-terminal window + its single pane, inserting it into
    /// `windows` (but deliberately not `window_order`). Assumes the GPU is
    /// already initialized (guaranteed by `toggle_quick_terminal`).
    fn create_quick_terminal(
        &mut self,
        event_loop: &ActiveEventLoop,
        origin_x: i32,
        top_y: i32,
        width: u32,
        height: u32,
    ) -> Option<WindowId> {
        let attrs = WindowAttributes::default()
            .with_title("Quick Terminal")
            .with_decorations(false)
            .with_inner_size(PhysicalSize::new(width, height))
            .with_position(PhysicalPosition::new(origin_x, top_y - height as i32))
            .with_transparent(self.config.background_opacity < 1.0);
        #[cfg(target_os = "macos")]
        let attrs = attrs.with_option_as_alt(macos_option_as_alt(self.config.macos_option_as_alt));
        let window = Arc::new(event_loop.create_window(attrs).ok()?);
        window.set_ime_allowed(true);
        crate::macos_blur::apply_background_blur(
            &window,
            self.config.background_blur_radius,
            self.config.background_opacity,
        );
        crate::macos_window::configure_quick_terminal_window(&window);

        let surface = {
            let gpu = self.gpu.as_ref()?;
            gpu.instance.create_surface(window.clone()).ok()?
        };
        let (surface_config, renderer) = {
            let gpu = self.gpu.as_mut()?;
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
            .ok()?;
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
        let metrics = self.gpu.as_ref()?.font.metrics();
        let grid = grid_size_for_pane_rect(initial_rect, metrics, self.padding);
        let initial_surface = self
            .spawn_pane_surface(window_id, initial_pane, grid, initial_rect, None)
            .ok()?;
        let mut surfaces = HashMap::new();
        surfaces.insert(initial_pane, initial_surface);
        let group = self.allocate_group_id();
        self.windows.insert(
            window_id,
            WindowState {
                window,
                group,
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
        self.relayout_and_resize_window(window_id);
        Some(window_id)
    }
}

impl ApplicationHandler<UserEvent> for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if !self.windows.is_empty() {
            return;
        }
        if !self.session_restore_attempted {
            self.session_restore_attempted = true;
            self.restore_session_if_enabled(event_loop);
        }
        // Restore may have found no session, an empty one, or failed every
        // spawn — always guarantee at least one window.
        if self.windows.is_empty() {
            let _ = self.spawn_tab(event_loop, SpawnTarget::CurrentWindow);
        }
    }

    fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
        // Release Secure Keyboard Entry if we still hold it, so the process
        // never leaves the process-global switch enabled for the rest of the
        // system after quitting.
        self.secure_input
            .disable_for_exit(&mut crate::secure_input::CarbonSecureInput);
        // Clean-quit (cmd+Q) path: windows are still live here, so capture the
        // freshest topology/cwd/focus. The all-windows-closed path leaves the
        // last file written by `persist_session` intact (this is a no-op when
        // `windows` is empty), matching "restore the last session".
        self.persist_session();
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: UserEvent) {
        match event {
            UserEvent::AppCommand(command) => {
                self.handle_app_command(event_loop, command, CommandOrigin::App)
            }
            UserEvent::ToggleQuickTerminal => self.toggle_quick_terminal(event_loop),
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
            UserEvent::Notify {
                window_id,
                pane_id,
                title,
                body,
            } => {
                if !self
                    .windows
                    .get(&window_id)
                    .is_some_and(|state| state.contains_pane(pane_id))
                {
                    return;
                }
                if crate::notification::should_notify(self.os_focused, window_id) {
                    crate::notification::post_notification(title.as_deref(), &body);
                }
            }
            UserEvent::Redraw(window_id, pane_id) => {
                let pane_state = self
                    .windows
                    .get(&window_id)
                    .map(|state| (state.contains_pane(pane_id), state.occluded));
                if pane_state.is_some_and(|(pane_exists, _)| pane_exists) {
                    self.mark_overview_tile_dirty(OverviewTileId::new(window_id, pane_id));
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
                // The quick terminal isn't a saved/tabbed window, so its shell
                // exiting tears the whole drop-down down rather than routing
                // through the tab-close path (which walks `window_order`).
                if self.is_quick_terminal_window(window_id) {
                    self.destroy_quick_terminal();
                } else {
                    self.close_pane_after_pty_exit(event_loop, window_id, pane_id)
                }
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
            WindowEvent::CloseRequested if self.is_quick_terminal_window(window_id) => {
                // Closing the drop-down just hides it; it isn't a real tab.
                self.start_quick_terminal_hide();
            }
            WindowEvent::CloseRequested => self.close_tab(event_loop, window_id),
            WindowEvent::RedrawRequested => self.redraw(window_id),
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                self.on_scale_factor_changed(window_id, scale_factor)
            }
            WindowEvent::Resized(size) => self.on_resize(window_id, size),
            WindowEvent::Focused(true) => {
                self.focused = Some(window_id);
                self.os_focused = Some(window_id);
                self.report_focus_event(window_id, true);
                self.secure_input
                    .on_focus_change(true, &mut crate::secure_input::CarbonSecureInput);
            }
            WindowEvent::Focused(false) => {
                // Only clear if this window is the one we recorded as focused —
                // when macOS switches between our own windows the incoming
                // `Focused(true)` may already have repointed `os_focused`, and
                // the outgoing window's `Focused(false)` must not undo it.
                if self.os_focused == Some(window_id) {
                    self.os_focused = None;
                }
                self.finish_active_split_drag(window_id);
                self.report_focus_event(window_id, false);
                // Release Secure Keyboard Entry while backgrounded so it never
                // blocks key input to the rest of the system; a matching
                // `Focused(true)` (including switching between our own windows)
                // restores it.
                self.secure_input
                    .on_focus_change(false, &mut crate::secure_input::CarbonSecureInput);
                if self.is_quick_terminal_window(window_id) {
                    self.maybe_autohide_quick_terminal();
                }
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
                let pressed = event.state == ElementState::Pressed;
                if pressed {
                    // Any keypress snaps the focused cursor back to its visible
                    // blink phase and restarts the interval, matching common
                    // terminal behavior (typing shouldn't leave the cursor
                    // stuck invisible mid-blink).
                    self.cursor_blink_visible = true;
                    self.cursor_blink_deadline = None;
                }
                // IME composition and the modal UI layers (confirm dialog,
                // search prompt, command palette) fully own the keyboard while
                // active — they act on presses and swallow releases so nothing
                // leaks to keybinds or the pty. Only the Kitty keyboard
                // protocol (below) ever emits release events.
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
                    if pressed {
                        self.handle_confirm_dialog_key(window_id, &event);
                    }
                    return;
                }
                if self
                    .search_prompt
                    .as_ref()
                    .is_some_and(|session| session.window_id == window_id)
                {
                    if pressed {
                        self.handle_search_prompt_key(event_loop, window_id, &event);
                    }
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
                    if pressed {
                        self.handle_command_palette_key(event_loop, window_id, &event);
                    }
                    return;
                }
                if pressed
                    && let Some(command) = self.keybinds.resolve(&event.logical_key, self.modifiers)
                {
                    self.handle_app_command(event_loop, command, CommandOrigin::TerminalWindow);
                    return;
                }
                // The Overview has its own window/event path. If a terminal
                // window receives this key while the Overview is still visible,
                // that terminal owns focus and must keep accepting shell input.
                // Cmd-based combos are app shortcuts, not shell input. Unknown
                // Cmd combos remain swallowed to match the previous behavior.
                if self.modifiers.super_key() {
                    return;
                }
                let app_cursor_keys = self.app_cursor_keys(window_id);
                let app_keypad = self.app_keypad(window_id);
                let kitty_flags = self.kitty_keyboard_flags(window_id);
                let bytes = input::encode_key_with_modes(
                    &event.logical_key,
                    Some(event.physical_key),
                    event.text.as_deref(),
                    self.modifiers,
                    app_cursor_keys,
                    app_keypad,
                    kitty_flags,
                    pressed,
                    event.repeat,
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
        self.install_global_hotkey_if_needed();
        // Each tick reports its own next wake-up instead of setting
        // `ControlFlow` directly, so a `WaitUntil` from one can't clobber a
        // more urgent one from the others — this pass sets it exactly once,
        // at the earliest across them.
        let blink_deadline = self.tick_cursor_blink();
        let overview_deadline = self.tick_overview_backlog();
        let quick_terminal_deadline = self.tick_quick_terminal();
        let deadline = [blink_deadline, overview_deadline, quick_terminal_deadline]
            .into_iter()
            .flatten()
            .min();
        event_loop.set_control_flow(match deadline {
            Some(deadline) => ControlFlow::WaitUntil(deadline),
            None => ControlFlow::Wait,
        });
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

        let (tracking, format) = self.mouse_report_modes(window_id, pane_id);
        if tracking != MouseTracking::Off && !self.modifiers.shift_key() {
            let pressed_mouse_button = self
                .windows
                .get(&window_id)
                .and_then(|state| state.surfaces.get(&pane_id))
                .and_then(|surface| surface.pressed_mouse_button);
            if let Some(bytes) = mouse::encode_mouse_motion(
                format,
                tracking,
                pressed_mouse_button,
                cell,
                self.modifiers,
            ) {
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

        let (tracking, format) = self.mouse_report_modes(window_id, pane_id);
        if tracking != MouseTracking::Off && !self.modifiers.shift_key() {
            let last_mouse_cell = self
                .windows
                .get(&window_id)
                .and_then(|state| state.surfaces.get(&pane_id))
                .and_then(|surface| surface.last_mouse_cell);
            if let Some(cell) = last_mouse_cell
                && let Some(bytes) =
                    mouse::encode_mouse_input(format, tracking, button, state, cell, self.modifiers)
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

        let (tracking, format) = self.mouse_report_modes(window_id, pane_id);
        let cell = self
            .windows
            .get(&window_id)
            .and_then(|state| state.surfaces.get(&pane_id))
            .and_then(|surface| surface.last_mouse_cell);
        let delta_y = match delta {
            MouseScrollDelta::LineDelta(_, y) => y,
            MouseScrollDelta::PixelDelta(position) => position.y as f32,
        };
        // A tracked mode that reports this wheel event consumes it; otherwise
        // (X10, Shift override, no known cell) fall through to local scrolling.
        if let Some(bytes) = mouse::route_mouse_wheel(
            tracking,
            format,
            self.modifiers.shift_key(),
            delta_y,
            cell,
            self.modifiers,
        ) {
            self.write_pane_pty_bytes(window_id, pane_id, &bytes);
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
        let Some((terminal, grid_size, overview_snapshot)) = self
            .windows
            .get(&window_id)
            .and_then(|state| state.surfaces.get(&pane_id))
            .map(|surface| {
                (
                    surface.terminal.clone(),
                    surface.grid_size,
                    surface.overview_snapshot.clone(),
                )
            })
        else {
            return;
        };

        let snapshot = apply_viewport_scroll_and_snapshot(
            &mut terminal.lock().expect("terminal mutex poisoned"),
            grid_size,
            scroll,
        );
        *overview_snapshot
            .lock()
            .expect("overview snapshot mutex poisoned") = Some(snapshot);
        self.mark_overview_tile_dirty(OverviewTileId::new(window_id, pane_id));
        self.request_overview_redraw();

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
        let Some((terminal, overview_snapshot)) = self
            .windows
            .get(&window_id)
            .and_then(|state| state.surfaces.get(&pane_id))
            .map(|surface| (surface.terminal.clone(), surface.overview_snapshot.clone()))
        else {
            return;
        };

        let snapshot = apply_mouse_wheel_viewport_scroll_and_snapshot(
            &mut terminal.lock().expect("terminal mutex poisoned"),
            scroll,
        );
        *overview_snapshot
            .lock()
            .expect("overview snapshot mutex poisoned") = Some(snapshot);
        self.mark_overview_tile_dirty(OverviewTileId::new(window_id, pane_id));
        self.request_overview_redraw();

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
                self.handle_app_command(event_loop, command, CommandOrigin::TerminalWindow);
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
                    self.handle_app_command(event_loop, command, CommandOrigin::TerminalWindow);
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
                self.handle_app_command(event_loop, command, CommandOrigin::TerminalWindow);
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
        self.write_pane_pty_bytes_lossless(window_id, pane_id, &bytes);
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
            } => self.write_pane_pty_bytes_lossless(window_id, pane_id, &bytes),
            ConfirmAction::ClipboardRead {
                window_id,
                pane_id,
                target,
            } => self.fulfill_clipboard_read(window_id, pane_id, &target),
        }
    }

    fn mouse_report_modes(
        &self,
        window_id: WindowId,
        pane_id: PaneId,
    ) -> (MouseTracking, MouseFormat) {
        self.windows
            .get(&window_id)
            .and_then(|state| state.surfaces.get(&pane_id))
            .map(|surface| {
                let terminal = surface.terminal.lock().expect("terminal mutex poisoned");
                (
                    terminal.modes.mouse_tracking(),
                    terminal.modes.mouse_format(),
                )
            })
            .unwrap_or((MouseTracking::Off, MouseFormat::Legacy))
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
        match crate::io_thread::try_queue_input(
            &surface.pty_input_tx,
            bytes.to_vec().into_boxed_slice(),
        ) {
            Ok(()) => {}
            Err(crate::io_thread::QueuePtyInputError::Full(_)) => {
                log::warn!("dropping pty input because the io thread queue is full");
            }
            Err(crate::io_thread::QueuePtyInputError::Disconnected) => {
                log::warn!("failed to queue pty input because the io thread is gone");
            }
        }
    }

    fn write_pane_pty_bytes_lossless(&self, window_id: WindowId, pane_id: PaneId, bytes: &[u8]) {
        let Some(surface) = self
            .windows
            .get(&window_id)
            .and_then(|state| state.surfaces.get(&pane_id))
        else {
            return;
        };
        match crate::io_thread::queue_input_lossless(
            surface.pty_input_tx.clone(),
            bytes.to_vec().into_boxed_slice(),
        ) {
            crate::io_thread::LosslessQueueResult::Queued => {}
            crate::io_thread::LosslessQueueResult::Deferred => {
                log::debug!("deferred pty input until the io thread queue has capacity");
            }
            crate::io_thread::LosslessQueueResult::Disconnected => {
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
        let url = noa_grid::detect_url_at_column(&row, cell.x)?;
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
        noa_grid::detect_url_at_column(&row, cell.x).map(|url| url.uri)
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

fn apply_viewport_scroll_and_snapshot(
    terminal: &mut Terminal,
    grid_size: GridSize,
    scroll: ViewportScroll,
) -> Arc<FrameSnapshot> {
    apply_viewport_scroll(terminal, grid_size, scroll);
    Arc::new(FrameSnapshot::peek(terminal))
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

fn apply_mouse_wheel_viewport_scroll_and_snapshot(
    terminal: &mut Terminal,
    scroll: MouseWheelViewportScroll,
) -> Arc<FrameSnapshot> {
    apply_mouse_wheel_viewport_scroll(terminal, scroll);
    Arc::new(FrameSnapshot::peek(terminal))
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

/// Which tab group a spawned tab should join, given the spawn target and the
/// focused window's group (if any). The `Fresh` arm defers minting an id to
/// the caller ([`App::allocate_group_id`]) so this stays a pure decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GroupChoice<G> {
    Existing(G),
    Fresh,
}

fn spawn_group_choice<G: Copy>(target: SpawnTarget, focused_group: Option<G>) -> GroupChoice<G> {
    match target {
        SpawnTarget::NewWindow => GroupChoice::Fresh,
        SpawnTarget::CurrentWindow => match focused_group {
            Some(group) => GroupChoice::Existing(group),
            None => GroupChoice::Fresh,
        },
    }
}

/// The ids in `order` whose group is `group`, preserving `order`. Backs
/// [`App::close_window`] (which closes every tab of the focused window's
/// group) and keeps the group-membership filter unit-testable without a live
/// window map.
fn ids_in_group<Id: Copy, G: Copy + Eq>(
    order: &[Id],
    group_of: impl Fn(Id) -> Option<G>,
    group: G,
) -> Vec<Id> {
    order
        .iter()
        .copied()
        .filter(|id| group_of(*id) == Some(group))
        .collect()
}

fn overview_tile_source_order<W: Copy + Eq, P: Copy>(
    window_order: &[W],
    mut live_window: impl FnMut(W) -> bool,
    mut pane_ids_for_window: impl FnMut(W) -> Vec<P>,
    overview_window: Option<W>,
) -> Vec<(W, P)> {
    window_order
        .iter()
        .copied()
        .filter(|id| Some(*id) != overview_window && live_window(*id))
        .flat_map(|window_id| {
            pane_ids_for_window(window_id)
                .into_iter()
                .map(move |pane_id| (window_id, pane_id))
        })
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

/// The close-button (✕) target for `point` (REQ-OV-13). Deliberately a
/// separate hit-test surface from [`overview_tile_target_at_point`]: the caller
/// checks this one *first* so a click landing on the title bar's close-button
/// corner closes the tab rather than focusing it, even though both rects
/// overlap there. `tile_rects` covers both live tiles and placeholder rows —
/// every tile has a title bar with a close button.
fn overview_close_target_at_point<Id: Copy>(
    source_ids: &[Id],
    tile_rects: &[PaneRectApp],
    point: split_tree::Point,
) -> Option<Id> {
    let tiles = source_ids
        .iter()
        .copied()
        .zip(tile_rects.iter().copied())
        .collect::<Vec<_>>();
    overview_close_hit_test(&tiles, point)
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CommandOrigin {
    App,
    TerminalWindow,
    OverviewWindow,
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
        | AppCommand::NewWindow
        | AppCommand::ToggleCommandPalette
        | AppCommand::ToggleQuickTerminal
        | AppCommand::ToggleSecureKeyboardEntry
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
        AppCommand::About
        | AppCommand::Preferences
        | AppCommand::Quit
        | AppCommand::ToggleQuickTerminal
        | AppCommand::ToggleSecureKeyboardEntry => CommandScope::App,
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
        | AppCommand::NewWindow
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

fn overview_should_intercept_command(
    command: AppCommand,
    overview_visible: bool,
    origin: CommandOrigin,
) -> bool {
    overview_visible
        && origin != CommandOrigin::TerminalWindow
        && overview_command_scope(command) == CommandScope::Overview
}

fn try_peek_overview_snapshot(terminal: &Arc<Mutex<Terminal>>) -> Option<Arc<FrameSnapshot>> {
    match terminal.try_lock() {
        Ok(term) => Some(Arc::new(FrameSnapshot::peek(&term))),
        Err(TryLockError::WouldBlock) => None,
        Err(TryLockError::Poisoned(_)) => panic!("terminal mutex poisoned"),
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

/// Clear the overview surface to the backdrop color when there are no tiles to
/// composite (the card composite pass otherwise does the clear itself).
fn clear_overview_surface(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    view: &wgpu::TextureView,
    color: [f32; 4],
) {
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("noa-overview-empty-clear-encoder"),
    });
    {
        let _pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("noa-overview-empty-clear-pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color {
                        r: f64::from(color[0]),
                        g: f64::from(color[1]),
                        b: f64::from(color[2]),
                        a: f64::from(color[3]),
                    }),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
        });
    }
    queue.submit(Some(encoder.finish()));
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

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_option_as_alt_maps_to_winit_modes() {
        assert_eq!(
            macos_option_as_alt(noa_config::MacosOptionAsAlt::None),
            OptionAsAlt::None
        );
        assert_eq!(
            macos_option_as_alt(noa_config::MacosOptionAsAlt::Left),
            OptionAsAlt::OnlyLeft
        );
        assert_eq!(
            macos_option_as_alt(noa_config::MacosOptionAsAlt::Right),
            OptionAsAlt::OnlyRight
        );
        assert_eq!(
            macos_option_as_alt(noa_config::MacosOptionAsAlt::Both),
            OptionAsAlt::Both
        );
    }

    #[test]
    fn quick_terminal_slide_offset_spans_hidden_to_revealed() {
        let height = 400.0;
        // Fully hidden: the whole panel sits above the screen top.
        assert!((quick_terminal_top_offset(height, 0.0) - (-height)).abs() < 0.001);
        // Fully revealed: flush with the screen top.
        assert!(quick_terminal_top_offset(height, 1.0).abs() < 0.001);
        // Monotonic: more reveal never moves the panel back up.
        let quarter = quick_terminal_top_offset(height, 0.25);
        let half = quick_terminal_top_offset(height, 0.5);
        assert!(quarter < half);
        assert!(half < 0.0);
    }

    #[test]
    fn ease_out_cubic_is_clamped_and_anchored() {
        assert!((ease_out_cubic(0.0)).abs() < 0.001);
        assert!((ease_out_cubic(1.0) - 1.0).abs() < 0.001);
        // Clamps out-of-range input rather than overshooting.
        assert!((ease_out_cubic(-1.0)).abs() < 0.001);
        assert!((ease_out_cubic(2.0) - 1.0).abs() < 0.001);
        // Ease-out front-loads progress: past the midpoint by t=0.5.
        assert!(ease_out_cubic(0.5) > 0.5);
    }

    #[test]
    fn quick_terminal_progress_is_linear_and_clamped() {
        let duration = Duration::from_millis(200);
        assert!((quick_terminal_progress(Duration::ZERO, duration)).abs() < 0.001);
        assert!(
            (quick_terminal_progress(Duration::from_millis(100), duration) - 0.5).abs() < 0.001
        );
        assert!(
            (quick_terminal_progress(Duration::from_millis(400), duration) - 1.0).abs() < 0.001
        );
        // A zero-length slide is instantly complete (no divide-by-zero).
        assert!((quick_terminal_progress(Duration::ZERO, Duration::ZERO) - 1.0).abs() < 0.001);
    }

    #[test]
    fn quick_terminal_height_is_a_clamped_screen_fraction() {
        assert_eq!(quick_terminal_height(1000, 0.4), 400);
        assert_eq!(quick_terminal_height(1000, 1.0), 1000);
        // Fraction is clamped to a usable range and never exceeds the screen.
        assert_eq!(quick_terminal_height(1000, 2.0), 1000);
        assert_eq!(quick_terminal_height(1000, 0.0), 50);
    }

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
    fn viewport_scroll_snapshot_tracks_scrolled_row_base() {
        let grid_size = GridSize::new(5, 3);
        let mut terminal = terminal_with_scrollback(grid_size);
        let before_row_base = terminal.active().visible_row_base();

        let snapshot =
            apply_viewport_scroll_and_snapshot(&mut terminal, grid_size, ViewportScroll::LineUp);

        assert_eq!(terminal.viewport_offset(), 1);
        assert_ne!(snapshot.row_base, before_row_base);
        assert_eq!(snapshot.row_base, terminal.active().visible_row_base());
        assert_eq!(
            snapshot.abs_row_base,
            terminal.active().rows_evicted() + terminal.active().visible_row_base()
        );
        assert!(
            snapshot.row_dirty.iter().all(|&dirty| dirty),
            "overview snapshots are full-row dirty"
        );
        assert!(!snapshot.cursor.visible);
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
    fn mouse_wheel_viewport_scroll_snapshot_tracks_scrolled_row_base() {
        let grid_size = GridSize::new(5, 3);
        let mut terminal = terminal_with_scrollback(grid_size);

        let snapshot = apply_mouse_wheel_viewport_scroll_and_snapshot(
            &mut terminal,
            MouseWheelViewportScroll::Up(2),
        );

        assert_eq!(terminal.viewport_offset(), 2);
        assert_eq!(snapshot.row_base, terminal.active().visible_row_base());
        assert_eq!(
            snapshot.abs_row_base,
            terminal.active().rows_evicted() + terminal.active().visible_row_base()
        );
        assert!(!snapshot.cursor.visible);
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
    fn spawn_group_choice_routes_new_tab_and_new_window() {
        // New Tab joins the focused window's group; with no focus (startup) it
        // falls back to a fresh group.
        assert_eq!(
            spawn_group_choice(SpawnTarget::CurrentWindow, Some(7_u64)),
            GroupChoice::Existing(7)
        );
        assert_eq!(
            spawn_group_choice::<u64>(SpawnTarget::CurrentWindow, None),
            GroupChoice::Fresh
        );
        // New Window always starts a fresh group, even when one is focused.
        assert_eq!(
            spawn_group_choice(SpawnTarget::NewWindow, Some(7_u64)),
            GroupChoice::Fresh
        );
        assert_eq!(
            spawn_group_choice::<u64>(SpawnTarget::NewWindow, None),
            GroupChoice::Fresh
        );
    }

    #[test]
    fn ids_in_group_filters_focused_windows_tabs() {
        // Two windows: tabs 1,3 in group 0; tabs 2,4 in group 1. Close Window
        // for the group-0 window must target exactly its tabs, in order.
        let order = [1_u8, 2, 3, 4];
        let group_of = |id: u8| match id {
            1 | 3 => Some(0_u8),
            2 | 4 => Some(1_u8),
            _ => None,
        };
        assert_eq!(ids_in_group(&order, group_of, 0), vec![1, 3]);
        assert_eq!(ids_in_group(&order, group_of, 1), vec![2, 4]);
        // A group with no live tabs yields nothing.
        assert_eq!(ids_in_group(&order, group_of, 9), Vec::<u8>::new());
    }

    #[test]
    fn overview_window_order_excludes_overview_and_closed_tabs() {
        let window_order = [1_u8, 2, 3, 4];
        let live_windows = |id| id != 3;
        let panes_for_window = |id| vec![id + 10];

        let sources =
            overview_tile_source_order(&window_order, live_windows, panes_for_window, Some(4));

        assert_eq!(sources, vec![(1, 11), (2, 12)]);
    }

    #[test]
    fn overview_window_order_expands_each_tab_to_panes_in_leaf_order() {
        let window_order = [1_u8, 2, 3];
        let live_windows = |id| id != 2;
        let panes_for_window = |id| match id {
            1 => vec![11, 12, 13],
            3 => vec![31],
            _ => Vec::new(),
        };

        let sources =
            overview_tile_source_order(&window_order, live_windows, panes_for_window, None);

        assert_eq!(sources, vec![(1, 11), (1, 12), (1, 13), (3, 31)]);
    }

    #[test]
    fn overview_click_hit_test_resolves_only_live_tiles() {
        let source_ids = [10_u8, 11, 12, 13, 14, 15, 16, 17, 18, 19];
        let layout =
            compute_overview_grid(source_ids.len(), PaneRectApp::new(0, 0, 90, 120), 9, 0, 0);

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
    fn overview_close_hit_test_is_exclusive_with_tile_focus() {
        let source_ids = [10_u8, 11, 12, 13];
        let layout =
            compute_overview_grid(source_ids.len(), PaneRectApp::new(0, 0, 200, 200), 9, 0, 0);
        // Tile 0's close button sits at its top-right corner; its body center
        // sits well inside. The two must resolve disjointly (REQ-OV-13).
        let tile0 = layout.tiles[0];
        let close_point = split_tree::Point::new(tile0.right() - 2, tile0.y + 2);
        let body_point = split_tree::Point::new(tile0.x + tile0.w / 2, tile0.y + tile0.h / 2);

        assert_eq!(
            overview_close_target_at_point(&source_ids, &layout.tiles, close_point),
            Some(10)
        );
        assert_eq!(
            overview_tile_target_at_point(&source_ids, &layout.tiles, close_point),
            Some(10),
            "both rects overlap at the corner; the caller's close-first ordering picks the close"
        );
        // The body center is a focus hit but never a close hit.
        assert_eq!(
            overview_close_target_at_point(&source_ids, &layout.tiles, body_point),
            None
        );
        assert_eq!(
            overview_tile_target_at_point(&source_ids, &layout.tiles, body_point),
            Some(10)
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
    fn overview_intercepts_only_non_terminal_window_commands() {
        let command = AppCommand::Paste;

        assert!(overview_should_intercept_command(
            command,
            true,
            CommandOrigin::OverviewWindow
        ));
        assert!(overview_should_intercept_command(
            command,
            true,
            CommandOrigin::App
        ));
        assert!(!overview_should_intercept_command(
            command,
            true,
            CommandOrigin::TerminalWindow
        ));
        assert!(!overview_should_intercept_command(
            command,
            false,
            CommandOrigin::OverviewWindow
        ));
        assert!(!overview_should_intercept_command(
            AppCommand::ToggleTabOverview,
            true,
            CommandOrigin::OverviewWindow
        ));
    }

    #[test]
    fn overview_snapshot_seed_skips_locked_terminal_without_waiting() {
        let terminal = Arc::new(Mutex::new(Terminal::new(GridSize::new(5, 3))));
        let _guard = terminal.lock().expect("terminal mutex poisoned");

        assert!(try_peek_overview_snapshot(&terminal).is_none());
    }

    #[test]
    fn overview_snapshot_seed_peeks_available_terminal() {
        let terminal = Arc::new(Mutex::new(Terminal::new(GridSize::new(5, 3))));

        assert!(try_peek_overview_snapshot(&terminal).is_some());
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
