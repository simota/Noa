//! The winit [`ApplicationHandler`] — owns native windows/tabs, per-tab
//! terminal sessions, and the shared GPU/font state used to render them.
//!
//! Rendering + presentation happens on the winit main thread (macOS requires
//! presenting on the thread that owns the window). Each io thread owns one
//! PTY, touches only its tab's `Terminal` mutex, and posts targeted user
//! events back to the main loop.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use parking_lot::Mutex;
use std::time::{Duration, Instant};

use crossbeam_channel::Sender;
use noa_core::{GridPadding, GridSize, PixelSize, Point};
use noa_font::FontGrid;
use noa_grid::{
    CursorStyle, PromptJump, Terminal,
    modes::{MouseFormat, MouseTracking},
};
use noa_pty::{Pty, PtyConfig};
use noa_render::{
    CardPipeline, CardStyle, CardTexturePlacement, CardTilePlacement, CommandPaletteSnapshot,
    FrameSnapshot, HoverLink, OverviewThumbnailResources, PaletteRow, PaneFrame,
    PaneId as RenderPaneId, PaneRect, Renderer, Theme,
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
use crate::session_overview::{
    OVERVIEW_GRID_CAP, OVERVIEW_MAX_RENDER_TILES_PER_FRAME, OVERVIEW_TILE_MIN_RENDER_INTERVAL,
    OverviewAction, OverviewChrome, OverviewEscapeAction, OverviewLayout, OverviewMetrics,
    OverviewRenderCandidate, compute_overview_grid, hit_test_overview_grid,
    move_overview_selection, overview_backlog_decision, overview_bg_color, overview_border_color,
    overview_card_color, overview_chrome_bands, overview_chrome_border_color,
    overview_chrome_pill_color, overview_close_hit_test, overview_escape_action,
    overview_focus_ring_color, overview_hint_bar_rect, overview_hint_bar_row,
    overview_initial_selection, overview_key_action, overview_label_padding,
    overview_placeholder_source_ids, overview_search_field_rect, overview_search_field_row,
    overview_tab_filter, overview_tile_labels, overview_title_bar_color, overview_zoom_rect,
    sanitize_placeholder_label, select_due_overview_tile_ids, title_bar_row_ansi,
};
use crate::session_store::{SessionCardId, SessionStore, SessionWindowId};
use crate::split_tree::{
    self, Direction, HitTarget, ImeOp, MAX_PANES_PER_TAB, MIN_PANE_SIZE_PX, PaneId,
    Rect as PaneRectApp, SPLIT_RESIZE_STEP_PX, SplitOrientation, SplitResizeDrag, SplitTree,
    can_add_pane_in_direction, equalize, focus_in_direction, focus_switch_plan, hit_test,
    resize_split, resize_split_to_drag_point, split_pane_in_direction,
    split_resize_drag_target_at_point, zoom_resize_targets, zoom_toggle,
};
use crate::{AppCommand, ViewportScroll};

mod auto_approve;
mod commands;
mod config;
mod config_reload;
mod event_loop;
mod helpers;
mod input_ops;
mod lifecycle;
mod overview;
mod quick_terminal;
mod render;
mod session_restore;
mod sidebar;
mod split_ops;
mod state;
mod timers;

pub use config::AppConfig;
use config_reload::ConfigWatcher;
use quick_terminal::QuickTerminalState;
use state::*;

#[cfg(target_os = "macos")]
use config::{apply_macos_titlebar_style, macos_option_as_alt, needs_macos_titlebar_backdrop};
use config::{
    decode_background_image, font_config_from_noa_config, resolve_cursor_style,
    resolve_grid_padding,
};
use helpers::*;
use input_ops::ActiveOverlay;
#[cfg(test)]
use quick_terminal::{
    ease_out_cubic, quick_terminal_height, quick_terminal_progress,
    quick_terminal_reveal_top_offset, quick_terminal_slide_reveal, quick_terminal_top_offset,
};

pub struct App {
    config: AppConfig,
    config_watcher: ConfigWatcher,
    /// Grid padding derived once from `window-padding-x/y`, applied to every
    /// pane's geometry.
    padding: GridPadding,
    /// Initial cursor style from `cursor-style` / `cursor-style-blink`, applied
    /// to each terminal at creation. `None` keeps the terminal default.
    initial_cursor_style: Option<CursorStyle>,
    /// Decoded `background-image` (startup-time load, PNG-only). Shared (cheap
    /// `Arc` clone) into every surface's renderer at creation. `None` when
    /// unset or the file was missing/undecodable.
    background_image: Option<noa_render::BackgroundImage>,
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
    /// Short-lived card flash after an automatic approval is injected.
    auto_approve_flash_until: HashMap<SessionCardId, Instant>,
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
    /// The open theme-settings overlay (theme-settings-ui R-1), if any — see
    /// [`ThemeSettingsSession`]. Mutually exclusive with `command_palette`
    /// and `search_prompt` (R-3, `App::active_overlay`).
    theme_settings: Option<ThemeSettingsSession>,
    /// The open confirmation dialog (paste protection / clipboard-read), if
    /// any — see [`ConfirmDialogSession`].
    confirm_dialog: Option<ConfirmDialogSession>,
    /// The open inline sidebar-card rename, if any — see
    /// [`SidebarRenameSession`].
    sidebar_rename: Option<SidebarRenameSession>,
    /// The open "Set Tab Title" prompt (tab-title REQ-TTL-1), if any — see
    /// [`TabTitlePromptSession`].
    tab_title_prompt: Option<TabTitlePromptSession>,
    /// Live IME composition text owned by whichever modal currently holds the
    /// keyboard (see `App::modal_ime_target`). Mirrored into that modal's
    /// input-row display and committed into its buffer, instead of being fed
    /// to the focused pane's `ime_state` (which would draw the composition at
    /// the terminal cursor, behind the modal).
    modal_preedit: Option<String>,
    /// Next scheduled relative-time repaint for visible sidebars, so a card's
    /// `3分前` keeps advancing without pty output. Armed only while at least
    /// one sidebar is visible; ticks once a minute (the formatter's finest
    /// granularity).
    sidebar_clock_deadline: Option<Instant>,
    /// Next scheduled recency re-sort for visible sidebars
    /// (`SIDEBAR_AUTOSORT_INTERVAL`), so the card order tracks each session's
    /// last output without re-sorting on every upsert. Armed only while at
    /// least one sidebar is visible.
    sidebar_autosort_deadline: Option<Instant>,
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
    /// Shared with every pane's io thread so `sidebar-preview-lines` can be
    /// changed by Theme & Settings without respawning PTYs.
    sidebar_preview_lines_gate: Arc<AtomicUsize>,
    /// The tab groups (logical windows) whose session sidebar is currently
    /// shown (FR-4). Per-window, not app-wide: toggling flips every tab of the
    /// focused window's group at once (tabs of one native window stay
    /// consistent) while other windows keep their own state. Fresh groups are
    /// seeded from `sidebar-enabled`. A quick-terminal window never shows it
    /// regardless (`window_sidebar_eligible`, FR-14).
    sidebar_visible_groups: HashSet<WindowGroupId>,
    /// The registered global `sidebar-hotkey`, kept alive for the app's
    /// lifetime (dropping it unregisters). `None` until installed, or when no
    /// `sidebar-hotkey` is configured / registration failed.
    sidebar_hotkey: Option<crate::macos_hotkey::GlobalHotKey>,
    /// The dedicated branch-poll worker (FR-8/FR-9): receives OSC-7-driven
    /// cwd-change requests and posts back git branch + project icon as
    /// [`crate::session_store::SessionDelta::Branch`]. Kept off the io read loop
    /// (NFR-2/AC-18) and joined at teardown (Omen T6, in `Drop`).
    branch_poll: Option<crate::branch_poll::BranchPollHandle>,
    /// Serializes and writes session state off the main thread; captures still
    /// happen on the caller. Its `Drop` (as an `App` field) flushes the last
    /// queued state to disk, covering the quit path.
    session_persister: crate::session_persist::SessionPersister,
}

impl App {
    pub fn new(config: AppConfig, proxy: EventLoopProxy<UserEvent>) -> Self {
        let padding = resolve_grid_padding(config.window_padding_x, config.window_padding_y);
        let initial_cursor_style =
            resolve_cursor_style(config.cursor_style, config.cursor_style_blink);
        // Decode the background image once at startup; a missing/undecodable
        // file logs a diagnostic inside and leaves this `None` (spec FR-8/NFR-1).
        let background_image = decode_background_image(&config);
        // Clone the proxy for the session-metadata worker before `proxy` is
        // moved into the struct — it posts `SessionDelta::Branch`/`Process` back
        // over it. The worker also shares the sidebar-visible gate so its
        // process poll only ticks while a sidebar is shown (AC-18).
        let proxy_for_branch_poll = proxy.clone();
        let sidebar_visible_gate = Arc::new(AtomicBool::new(false));
        let sidebar_preview_lines_gate = Arc::new(AtomicUsize::new(config.sidebar_preview_lines));
        App {
            config_watcher: ConfigWatcher::new(),
            padding,
            initial_cursor_style,
            background_image,
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
            auto_approve_flash_until: HashMap::new(),
            attention_blink_deadline: None,
            hovered_link: None,
            search_prompt: None,
            command_palette: None,
            theme_settings: None,
            confirm_dialog: None,
            sidebar_rename: None,
            tab_title_prompt: None,
            modal_preedit: None,
            sidebar_clock_deadline: None,
            sidebar_autosort_deadline: None,
            session_restore_attempted: false,
            restoring: false,
            quick_terminal: None,
            quick_terminal_hotkey: None,
            hotkey_install_attempted: false,
            secure_input: crate::secure_input::SecureInput::new(),
            session_store: SessionStore::new(),
            sidebar_hotkey: None,
            branch_poll: Some(crate::branch_poll::spawn(proxy_for_branch_poll)),
            session_persister: crate::session_persist::SessionPersister::spawn(),
            sidebar_visible_gate,
            sidebar_preview_lines_gate,
            sidebar_visible_groups: HashSet::new(),
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
            .map(|surface| surface.terminal.lock().modes.app_cursor_keys())
            .unwrap_or(false)
    }

    fn app_keypad(&self, window_id: WindowId) -> bool {
        self.windows
            .get(&window_id)
            .and_then(WindowState::focused_surface)
            .map(|surface| surface.terminal.lock().modes.app_keypad())
            .unwrap_or(false)
    }

    fn kitty_keyboard_flags(&self, window_id: WindowId) -> u8 {
        self.windows
            .get(&window_id)
            .and_then(WindowState::focused_surface)
            .map(|surface| surface.terminal.lock().kitty_keyboard_flags())
            .unwrap_or(0)
    }

    fn focus_reporting(&self, window_id: WindowId) -> bool {
        self.windows
            .get(&window_id)
            .and_then(WindowState::focused_surface)
            .map(|surface| surface.terminal.lock().modes.focus_reporting())
            .unwrap_or(false)
    }

    fn report_focus_event(&self, window_id: WindowId, focused: bool) {
        if let Some(bytes) = focus_report_bytes(focused, self.focus_reporting(window_id)) {
            self.write_pty_bytes(window_id, bytes);
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
