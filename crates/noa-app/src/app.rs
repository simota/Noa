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
use crate::session;
use crate::session_store::{SessionCardId, SessionStore, SessionWindowId};
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
    overview_tab_filter, overview_tile_labels, overview_zoom_rect, sanitize_placeholder_label,
    select_due_overview_tile_ids, title_bar_row_ansi,
};
use crate::{AppCommand, ViewportScroll};

mod config;
mod event_loop;
mod helpers;
mod input_ops;
mod overview;
mod quick_terminal;
mod session_restore;
mod sidebar;

pub use config::AppConfig;
use quick_terminal::QuickTerminalState;

#[cfg(target_os = "macos")]
use config::{apply_macos_titlebar_style, macos_option_as_alt};
use config::{font_config_from_noa_config, resolve_cursor_style, resolve_grid_padding};
use helpers::*;
#[cfg(test)]
use quick_terminal::{
    ease_out_cubic, quick_terminal_height, quick_terminal_progress, quick_terminal_top_offset,
};

/// App-wide GPU and glyph state shared by every tab/window.
struct GpuState {
    instance: wgpu::Instance,
    adapter: wgpu::Adapter,
    device: wgpu::Device,
    queue: wgpu::Queue,
    font: FontGrid,
    /// Dedicated, smaller font for the session sidebar (mockup-dense typography,
    /// [`SIDEBAR_FONT_POINT_SIZE`]), sized independently of the terminal font
    /// and rebuilt on a scale change alongside `font`.
    sidebar_font: FontGrid,
    theme: Theme,
    /// Single reused `Renderer` that rasterizes the whole sidebar band as
    /// synthetic terminal cells (Omen T3: one renderer for every card, never
    /// per-card). Built lazily for the first window's surface format.
    sidebar_renderer: Option<Renderer>,
    /// Rounded-card pipeline reused to composite the rasterized sidebar band
    /// onto each window's surface (CardStyle/overlay_texture_cards).
    sidebar_card: Option<OverviewChromeCardPipeline>,
    /// The band texture the sidebar rasterizes into, cached with its size so it
    /// is reused frame-to-frame and only reallocated when the band dimensions
    /// change (a window resize or sidebar-width change). This is the flat dark
    /// backdrop (header/toolbar + card text) that per-card rounded cards overlay.
    sidebar_band: Option<(PixelSize, wgpu::Texture, wgpu::TextureView)>,
    /// Reused scratch texture for one rounded session card (inset x card
    /// height): each visible card is rendered into it then composited as a
    /// rounded card in turn, so a single texture serves every card without a
    /// per-card allocation (Omen T3: still one renderer, one card texture).
    sidebar_card_tex: Option<(PixelSize, wgpu::Texture, wgpu::TextureView)>,
    /// Reused scratch texture for the open card `…` menu popup, composited above
    /// the cards so a rounded card can never hide it.
    sidebar_menu_tex: Option<(PixelSize, wgpu::Texture, wgpu::TextureView)>,
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
    /// Whether this window currently shows the session sidebar (FR-4). Seeded
    /// from `sidebar-enabled` at creation and flipped per-window by the sidebar
    /// hotkey; drives the pane-area inset and the sidebar draw. Always `false`
    /// for a quick-terminal window (FR-14).
    sidebar_visible: bool,
    /// Vertical scroll offset (px) of the sidebar card list (FR-15), clamped to
    /// `[0, content_h - viewport_h]` when consumed by the layout.
    sidebar_scroll: u32,
    /// The card whose `…` menu popup is open in this window (FR-7), or `None`.
    /// Opened by a `…` click, dismissed by the next click anywhere or by the
    /// sidebar toggling off.
    sidebar_menu: Option<SessionCardId>,
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
    /// The tile index (same source order as `selected`) currently under the
    /// mouse cursor, for hover feedback — an accent border drawn over the
    /// hovered card. Recomputed on `CursorMoved`, cleared when the cursor
    /// leaves every tile.
    hovered: Option<usize>,
    /// Whether the selected tile is zoomed (Tab toggles): an enlarged centered
    /// re-composite of the tile drawn above the grid, quick-look style.
    zoomed: bool,
    /// The live "Search sessions" filter query (REQ-OV-16). Printable keys append
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

/// An open inline rename on a sidebar card (FR-7 Rename). Modal for its
/// window's keyboard while it is open — the `KeyboardInput` handler routes
/// keystrokes in `window_id` to [`App::handle_sidebar_rename_key`]: printable
/// text appends, Backspace pops, Enter commits a
/// [`crate::session_store::SessionDelta::Rename`], Escape cancels. Only one
/// exists at a time app-wide.
struct SidebarRenameSession {
    window_id: WindowId,
    card: SessionCardId,
    buffer: String,
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
    /// Close one split pane, discarding its PTY.
    ClosePane {
        window_id: WindowId,
        pane_id: PaneId,
    },
    /// Close one native tab/session, discarding every pane in it.
    CloseTab { window_id: WindowId },
    /// Close every tab in one logical window group.
    CloseWindow { group: WindowGroupId },
    /// Quit the app, discarding every live session.
    Quit,
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
    /// The Session Overview mirror's read-only publish slot (Fix B, REQ-NF-6):
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

/// How long an attention marker blinks before settling to a steady mark
/// (FR-A1). Compile-time (⚠G — no config knob in v1).
const ATTENTION_BLINK_DURATION: Duration = Duration::from_secs(6);
/// The blink half-period (~1.5 Hz) — the marker toggles on/off every interval
/// for `ATTENTION_BLINK_DURATION`.
const ATTENTION_BLINK_INTERVAL: Duration = Duration::from_millis(333);

/// Compile-time card styling for the Session Overview composite (REQ-OV-12/14, v2
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
    /// Next scheduled Session Overview wake-up, set by `redraw_overview`'s
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
    /// Monotonic onset of each card's current attention request (FR-A1), keyed
    /// by card id. Set on the `false→true` attention transition and removed
    /// when the flag is cleared (window focus) or the session is torn down.
    /// Drives the blink phase (`sidebar::attention_blink_on(onset.elapsed())`)
    /// and whether the blink timer stays armed. FR-A7: never reset while an
    /// attention is already pending, so a repeat request doesn't restart the
    /// blink.
    attention_onset: HashMap<SessionCardId, Instant>,
    /// Next scheduled attention-blink repaint; `None` while no card is within
    /// its blink window (the event loop then needs no wake-up for this).
    attention_blink_deadline: Option<Instant>,
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
    /// The open inline sidebar-card rename, if any — see
    /// [`SidebarRenameSession`].
    sidebar_rename: Option<SidebarRenameSession>,
    /// Next scheduled relative-time repaint for visible sidebars, so a card's
    /// `3分前` keeps advancing without pty output. Armed only while at least
    /// one sidebar is visible; ticks once a minute (the formatter's finest
    /// granularity).
    sidebar_clock_deadline: Option<Instant>,
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
    /// The central session registry (FR-1), the single source of truth for the
    /// session sidebar. Owned here on the main thread; mutated only by applying
    /// [`crate::session_store::SessionDelta`]s posted by io threads and by
    /// [`SessionStore::reconcile_sessions`] at teardown (FR-12).
    session_store: SessionStore,
    /// Shared with every pane's io thread (`io_thread::SidebarPublish`) so it
    /// gates its per-feed card-state extraction behind one atomic load
    /// (FR-1/AC-19). Deliberately distinct from `overview_visible_gate` (Omen
    /// T1); flipped on while any window shows its sidebar.
    sidebar_visible_gate: Arc<AtomicBool>,
    /// The registered global `sidebar-hotkey`, kept alive for the app's
    /// lifetime (dropping it unregisters). `None` until installed, or when no
    /// `sidebar-hotkey` is configured / registration failed.
    sidebar_hotkey: Option<crate::macos_hotkey::GlobalHotKey>,
    /// The dedicated branch-poll worker (FR-8/FR-9): receives OSC-7-driven
    /// cwd-change requests and posts back git branch + project icon as
    /// [`crate::session_store::SessionDelta::Branch`]. Kept off the io read loop
    /// (NFR-2/AC-18) and joined at teardown (Omen T6, in `Drop`).
    branch_poll: Option<crate::branch_poll::BranchPollHandle>,
}

impl App {
    pub fn new(config: AppConfig, proxy: EventLoopProxy<UserEvent>) -> Self {
        let padding = resolve_grid_padding(config.window_padding_x, config.window_padding_y);
        let initial_cursor_style =
            resolve_cursor_style(config.cursor_style, config.cursor_style_blink);
        // Clone the proxy for the session-metadata worker before `proxy` is
        // moved into the struct — it posts `SessionDelta::Branch`/`Process` back
        // over it. The worker also shares the sidebar-visible gate so its
        // process poll only ticks while a sidebar is shown (AC-18).
        let proxy_for_branch_poll = proxy.clone();
        let sidebar_visible_gate = Arc::new(AtomicBool::new(false));
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
            attention_onset: HashMap::new(),
            attention_blink_deadline: None,
            hovered_link: None,
            search_prompt: None,
            command_palette: None,
            confirm_dialog: None,
            sidebar_rename: None,
            sidebar_clock_deadline: None,
            session_restore_attempted: false,
            restoring: false,
            quick_terminal: None,
            quick_terminal_hotkey: None,
            hotkey_install_attempted: false,
            secure_input: crate::secure_input::SecureInput::new(),
            session_store: SessionStore::new(),
            sidebar_hotkey: None,
            branch_poll: Some(crate::branch_poll::spawn(
                proxy_for_branch_poll,
                sidebar_visible_gate.clone(),
            )),
            sidebar_visible_gate,
        }
    }

    fn theme_overrides(&self) -> crate::theme::ThemeOverrides {
        crate::theme::ThemeOverrides {
            background: self.config.background,
            foreground: self.config.foreground,
            cursor: self.config.cursor_color,
            selection_fg: self.config.selection_foreground,
            selection_bg: self.config.selection_background,
            minimum_contrast: self.config.minimum_contrast,
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
        // Build the sidebar's draw model up front (reads only the store + pure
        // layout, AC-17) before borrowing `gpu`/`state` mutably, so the band can
        // be composited inline after the panes without a second borrow.
        let sidebar_model = self.sidebar_draw_model(window_id);
        let padding = self.padding;
        let (Some(gpu), Some(state)) = (self.gpu.as_mut(), self.windows.get_mut(&window_id)) else {
            return;
        };
        if state.occluded {
            return;
        }

        let mut snapshots = Vec::new();
        let mut title = "Noa".to_string();
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
            snapshot.focused =
                pane_owns_keyboard_focus(window_id, pane_id, self.os_focused, state.focused_pane);
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
            // Inline IME composition: draw the focused pane's live pre-edit run
            // at the cursor. Only the focused pane composes, so guard on it the
            // same way the palette does.
            snapshot.preedit = (pane_id == state.focused_pane
                && surface.ime_state.preedit_active())
            .then(|| noa_render::Preedit {
                text: surface.ime_state.preedit_text().to_string(),
                cursor_byte_range: surface.ime_state.preedit_cursor(),
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
        // Composite the session sidebar over the reserved left inset (FR-2/FR-5),
        // after the panes so it isn't overdrawn. The pane area was already inset
        // by `relayout_and_resize_window`, so this fills that band.
        if let Some(model) = sidebar_model.as_ref() {
            let surface_size = PixelSize {
                w: state.surface_config.width,
                h: state.surface_config.height,
            };
            sidebar::draw_sidebar_band(
                gpu,
                state.surface_config.format,
                padding,
                &view,
                surface_size,
                model,
            );
        }
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

    /// Wake the Session Overview once the earliest throttle-blocked dirty tile
    /// becomes due, instead of `redraw_overview` re-requesting a redraw
    /// every pass while `should_render_tile` keeps rejecting it until
    /// `OVERVIEW_TILE_MIN_RENDER_INTERVAL` has elapsed (Fix A — see
    /// `redraw_overview`'s post-frame backlog check and
    /// `tab_overview::overview_backlog_decision`). Piggybacks on the same
    /// `about_to_wait` + `WaitUntil` wake-up mechanism as
    /// `tick_cursor_blink` rather than adding a second timer source.
    /// Whether an attention marker is currently visible for `id` (FR-A1) — the
    /// blink phase during the first `ATTENTION_BLINK_DURATION`, then steady on.
    /// `true` when no onset is tracked (a settled/legacy attention with no blink
    /// state still shows). Shared by the sidebar draw and the overview label.
    pub(super) fn attention_marker_visible(&self, id: &SessionCardId) -> bool {
        match self.attention_onset.get(id) {
            Some(onset) => crate::sidebar::attention_blink_on(
                onset.elapsed(),
                ATTENTION_BLINK_DURATION,
                ATTENTION_BLINK_INTERVAL,
            ),
            None => true,
        }
    }

    /// Whether any tracked attention is still inside its blink window, so the
    /// blink timer must keep waking the loop. Settled attentions (past the
    /// window) stay in the map but draw steady, so they don't arm the timer.
    fn any_attention_blinking(&self) -> bool {
        self.attention_onset
            .values()
            .any(|onset| onset.elapsed() < ATTENTION_BLINK_DURATION)
    }

    /// Advance the attention blink (FR-A1/NFR-A2): while any card is within its
    /// blink window, repaint the sidebars and overview tiles once per interval
    /// and report the next wake-up; once none remain, do a final settle repaint
    /// and disarm. Piggybacks on the shared `about_to_wait` + `WaitUntil` timer
    /// like `tick_cursor_blink`, so no second timer source is added.
    fn tick_attention_blink(&mut self) -> Option<Instant> {
        if !self.any_attention_blinking() {
            if self.attention_blink_deadline.take().is_some() {
                // The last blink just settled — repaint once so the steady mark
                // replaces the mid-blink frame.
                self.request_sidebar_redraw();
                self.mark_attention_overview_tiles_dirty();
            }
            return None;
        }
        let now = Instant::now();
        match self.attention_blink_deadline {
            Some(deadline) if now < deadline => Some(deadline),
            _ => {
                let next = now + ATTENTION_BLINK_INTERVAL;
                self.attention_blink_deadline = Some(next);
                self.request_sidebar_redraw();
                self.mark_attention_overview_tiles_dirty();
                Some(next)
            }
        }
    }

    /// Mark every attention card's overview tile dirty and request an overview
    /// redraw, so a blink toggle re-stamps its `●` title band (FR-A2).
    fn mark_attention_overview_tiles_dirty(&mut self) {
        let ids: Vec<SessionCardId> = self.attention_onset.keys().copied().collect();
        for id in ids {
            let window_id = WindowId::from(id.window_id.0);
            self.mark_overview_tile_dirty(OverviewTileId::new(window_id, id.pane_id));
        }
        self.request_overview_redraw();
    }

    /// Repaint visible sidebars once a minute so the relative updated-time
    /// (`3分前`) keeps advancing while a session produces no output. Disarmed
    /// while no sidebar is visible, so an all-hidden app adds no wake-ups.
    fn tick_sidebar_clock(&mut self) -> Option<Instant> {
        let any_visible = self
            .windows
            .values()
            .any(|state| state.sidebar_visible);
        if !any_visible {
            self.sidebar_clock_deadline = None;
            return None;
        }
        let now = Instant::now();
        match self.sidebar_clock_deadline {
            Some(deadline) if now < deadline => Some(deadline),
            _ => {
                if self.sidebar_clock_deadline.take().is_some() {
                    self.request_sidebar_redraw();
                }
                let next = now + Duration::from_secs(60);
                self.sidebar_clock_deadline = Some(next);
                Some(next)
            }
        }
    }

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
            AppCommand::About => crate::app_actions::show_about(),
            AppCommand::Preferences => crate::app_actions::open_config_file(),
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
                    self.request_close_focused_pane_or_tab(event_loop, window_id);
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
            AppCommand::ToggleSidebar => self.toggle_sidebar(),
            AppCommand::CloseWindow => self.request_close_window(event_loop),
            AppCommand::Quit => self.request_quit(event_loop),
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
                sidebar_font: FontGrid::new(
                    sidebar_font_pixel_size(window_scale_factor),
                    font_config_from_noa_config(&self.config.font),
                )
                .expect("failed to load the sidebar font"),
                theme: crate::theme::resolve_theme_with_overrides(
                    self.config.theme.as_deref(),
                    &self.theme_overrides(),
                ),
                sidebar_renderer: None,
                sidebar_card: None,
                sidebar_band: None,
                sidebar_card_tex: None,
                sidebar_menu_tex: None,
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
                title: "Noa".to_string(),
                sidebar_visible: self.config.sidebar_enabled,
                sidebar_scroll: 0,
                sidebar_menu: None,
                link_click_in_flight: false,
            },
        );
        self.window_order.push(window_id);
        self.mark_overview_tile_dirty(OverviewTileId::new(window_id, initial_pane));
        // A window seeded with the sidebar visible (`sidebar-enabled`) must turn
        // the io-thread gate on so its panes start publishing card state.
        self.refresh_sidebar_visible_gate();
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
            .with_title("Noa")
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
            .with_title("Session Overview")
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
        // Hand the foreground-process probe to the session-metadata worker
        // (running-process display) before the pty moves into the io thread.
        // Quick-terminal panes never get a sidebar card, so they need no probe.
        if self.window_sidebar_eligible(window_id)
            && let Some(worker) = self.branch_poll.as_ref()
            && let Some(probe) = pty.foreground_probe()
        {
            worker.register_process_probe(Self::session_card_id(window_id, pane_id), probe);
        }
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
        let sidebar_publish = crate::io_thread::SidebarPublish {
            visible: self.sidebar_visible_gate.clone(),
        };
        let io_thread = crate::io_thread::spawn(
            pty,
            terminal.clone(),
            self.proxy.clone(),
            crate::io_thread::IoThreadTarget { window_id, pane_id },
            resize_rx,
            pty_input_rx,
            overview_publish,
            sidebar_publish,
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

    fn pane_has_running_program(&self, window_id: WindowId, pane_id: PaneId) -> bool {
        self.windows
            .get(&window_id)
            .and_then(|state| state.surfaces.get(&pane_id))
            .is_some_and(surface_has_running_program)
    }

    fn tab_running_program_count(&self, window_id: WindowId) -> usize {
        self.windows
            .get(&window_id)
            .map(|state| running_program_count(state.surfaces.values()))
            .unwrap_or(0)
    }

    fn group_running_program_count(&self, group: WindowGroupId) -> usize {
        self.window_order
            .iter()
            .filter_map(|window_id| self.windows.get(window_id))
            .filter(|state| state.group == group)
            .map(|state| running_program_count(state.surfaces.values()))
            .sum()
    }

    fn app_running_program_count(&self) -> usize {
        self.windows
            .values()
            .map(|state| running_program_count(state.surfaces.values()))
            .sum()
    }

    fn request_close_tab(&mut self, event_loop: &ActiveEventLoop, window_id: WindowId) {
        let count = self.tab_running_program_count(window_id);
        if count > 0 {
            self.open_confirm_dialog(
                window_id,
                close_confirm_message(CloseConfirmTarget::Session, count),
                ConfirmAction::CloseTab { window_id },
            );
            return;
        }
        self.close_tab(event_loop, window_id);
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
        // GC choke point (FR-12): the tab's cards (and, via window-remove, a
        // whole group's) are gone from `windows` now, so drop them from the
        // store too. `close_group`/`close_pane_after_pty_exit` reach this
        // through their `close_tab` calls.
        self.reconcile_session_store();
        // A closed window may have been the only one showing a sidebar.
        self.refresh_sidebar_visible_gate();
        self.persist_session();
    }

    /// Close the entire focused logical window: every tab in its AppKit tab
    /// group (`cmd+shift+w` / File → Close Window). Each tab is torn down via
    /// [`App::close_tab`], so all its per-tab cleanup (io-thread shutdown,
    /// modal/search/palette de-leak, focus repoint) runs, and closing the last
    /// remaining window's last tab still quits the app through
    /// [`TabCloseOutcome::Quit`]. A no-op when nothing is focused.
    fn request_close_window(&mut self, event_loop: &ActiveEventLoop) {
        let Some(window_id) = self.focused else {
            return;
        };
        let Some(group) = self.windows.get(&window_id).map(|state| state.group) else {
            return;
        };
        let count = self.group_running_program_count(group);
        if count > 0 {
            self.open_confirm_dialog(
                window_id,
                close_confirm_message(CloseConfirmTarget::Window, count),
                ConfirmAction::CloseWindow { group },
            );
            return;
        }
        self.close_group(event_loop, group);
    }

    fn close_group(&mut self, event_loop: &ActiveEventLoop, group: WindowGroupId) {
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

    fn request_close_focused_pane_or_tab(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
    ) {
        let Some(state) = self.windows.get(&window_id) else {
            return;
        };
        if state.pane_count() <= 1 {
            self.request_close_tab(event_loop, window_id);
            return;
        }
        let pane_id = state.focused_pane;
        self.request_close_pane(event_loop, window_id, pane_id);
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

    fn request_close_pane(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        pane_id: PaneId,
    ) {
        if self.pane_has_running_program(window_id, pane_id) {
            self.open_confirm_dialog(
                window_id,
                close_confirm_message(CloseConfirmTarget::Pane, 1),
                ConfirmAction::ClosePane { window_id, pane_id },
            );
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
        // GC choke point (FR-12): the closed pane's card is dropped from the
        // store. `close_pane_after_pty_exit` reaches this through `close_pane`.
        self.reconcile_session_store();
        self.relayout_and_resize_window(window_id);
        self.update_focused_ime_cursor_area(window_id);
        window.request_redraw();
        self.request_overview_redraw();
        self.persist_session();
    }

    fn request_quit(&mut self, event_loop: &ActiveEventLoop) {
        let count = self.app_running_program_count();
        if count == 0 {
            event_loop.exit();
            return;
        }
        let Some(window_id) = self
            .focused
            .filter(|id| self.windows.contains_key(id))
            .or_else(|| self.window_order.last().copied())
            .or_else(|| self.windows.keys().copied().next())
        else {
            event_loop.exit();
            return;
        };
        self.open_confirm_dialog(
            window_id,
            close_confirm_message(CloseConfirmTarget::App, count),
            ConfirmAction::Quit,
        );
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
        // Same sidebar inset as the pane layout so the zoom decision sees the
        // real pane area, not the full window (Omen P1).
        let inset = self.window_sidebar_inset_px(window_id);
        let bounds = self
            .windows
            .get(&window_id)
            .map(|state| sidebar_inset_bounds(pane_bounds_for_size(state.window.inner_size()), inset))
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
        self.overview_window.take();
        for state in self.windows.values_mut() {
            state.shutdown();
        }
        self.windows.clear();
        self.window_order.clear();
        self.focused = None;
        // Stop and join the branch-poll worker (Omen T6) before the proxy is
        // dropped with the rest of `App`.
        if let Some(mut branch_poll) = self.branch_poll.take() {
            branch_poll.shutdown();
        }
        self.gpu.take();
    }
}

