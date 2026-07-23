//! The winit [`ApplicationHandler`] — owns native windows/tabs, per-tab
//! terminal sessions, and the shared GPU/font state used to render them.
//!
//! Rendering + presentation happens on the winit main thread (macOS requires
//! presenting on the thread that owns the window). Each io thread owns one
//! PTY, touches only its tab's `Terminal` mutex, and posts targeted user
//! events back to the main loop.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

use parking_lot::Mutex;
use std::time::{Duration, Instant};

use crossbeam_channel::Sender;
use noa_core::{GridPadding, GridSize, PixelSize, Point};
use noa_font::FontGrid;
use noa_grid::{
    CursorStyle, PromptJump, Terminal,
    modes::{MouseFormat, MouseTracking},
};
use noa_pty::{Pty, PtyConfig, PtyWriter};
use noa_render::{
    BackgroundImage, CardPipeline, CardStyle, CardTexturePlacement, CardTilePlacement,
    CommandPaletteSnapshot, FrameSnapshot, HoverLink, OverviewThumbnailResources, PaletteRow,
    PaneFrame, PaneId as RenderPaneId, PaneRect, Renderer, Theme,
};
use noa_vt::Stream;
use winit::application::ApplicationHandler;
use winit::dpi::{LogicalPosition, LogicalSize, PhysicalPosition, PhysicalSize};
use winit::event::{ElementState, Ime, KeyEvent, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoopProxy};
use winit::keyboard::{Key, ModifiersState, NamedKey, PhysicalKey};
#[cfg(target_os = "macos")]
use winit::platform::macos::{MonitorHandleExtMacOS, WindowAttributesExtMacOS, WindowExtMacOS};
use winit::window::{CursorIcon, Window, WindowAttributes, WindowId};

use crate::clipboard::{self, PasteContents, SystemClipboard};
use crate::command_palette::{self, CommandPalette};
use crate::commands::{
    CopyModeAction, FontSizeAction, KeybindEngine, SearchAction, TerminalAction,
};
use crate::events::UserEvent;
use crate::input;
use crate::link_open::{self, LinkTarget};
use crate::mouse::{self, MouseSelectionState, SelectionGesture};
use crate::search_prompt::{SearchPrompt, SearchPromptEffect};
use crate::session;
use crate::session_overview::{
    OVERVIEW_GRID_CAP, OVERVIEW_MAX_RENDER_TILES_PER_FRAME, OVERVIEW_TILE_MIN_RENDER_INTERVAL,
    OverviewAction, OverviewChrome, OverviewEscapeAction, OverviewLayout, OverviewMetrics,
    OverviewRenderCandidate, PaneZone, classify_pane_zone, compute_overview_grid,
    hit_test_overview_grid, move_overview_selection, overview_backlog_decision, overview_bg_color,
    overview_border_color, overview_card_color, overview_chrome_bands,
    overview_chrome_border_color, overview_chrome_pill_color, overview_close_hit_test,
    overview_escape_action, overview_focus_ring_color, overview_hint_bar_rect,
    overview_hint_bar_row, overview_initial_selection, overview_key_action, overview_label_padding,
    overview_placeholder_source_ids, overview_search_field_rect, overview_search_field_row,
    overview_tab_filter, overview_tile_labels, overview_title_bar_color, overview_zoom_rect,
    sanitize_placeholder_label, select_due_overview_tile_ids, title_bar_row_ansi,
};
use crate::session_store::{SessionCardId, SessionStore, SessionWindowId};
use crate::split_tree::{
    self, Direction, HitTarget, ImeOp, MAX_PANES_PER_TAB, MIN_PANE_SIZE_PX, PaneId,
    Rect as PaneRectApp, SPLIT_RESIZE_STEP_PX, SplitOrientation, SplitResizeDrag, SplitTree,
    can_add_pane_in_direction, equalize, focus_in_direction, focus_switch_plan, hit_test,
    move_pane_with_zoom, resize_split, resize_split_to_drag_point, split_pane_in_direction,
    split_resize_drag_target_at_point, swap_pane_with_zoom, zoom_resize_targets, zoom_toggle,
};
use crate::{AppCommand, ViewportScroll};

mod applescript;
mod auto_approve;
mod commands;
mod config;
mod config_reload;
mod event_loop;
mod helpers;
mod input_ops;
mod ipc;
mod lifecycle;
mod overview;
mod pane_drag;
mod pane_drag_render;
mod quick_terminal;
mod remote_ui;
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

use config::{
    BackgroundImageRuntime, alpha_blending_mode, apply_palette_overrides, effective_theme_name,
    font_config_from_noa_config, load_background_image_runtime, resolve_cursor_style,
    resolve_grid_padding,
};
#[cfg(target_os = "macos")]
use config::{apply_macos_titlebar_style, macos_option_as_alt, needs_macos_titlebar_backdrop};
use helpers::*;
use input_ops::ActiveOverlay;
#[cfg(test)]
use quick_terminal::{
    ease_out_cubic, quick_terminal_anchor_window_id, quick_terminal_position_geometry,
    quick_terminal_progress, quick_terminal_reveal_origin,
    quick_terminal_should_autohide_on_focus_loss, quick_terminal_should_suppress_redraw,
    quick_terminal_size_footprint, quick_terminal_slide_reveal,
};

#[derive(Clone)]
struct LiveWallpaperTransition {
    previous: Option<BackgroundImage>,
    current: Option<BackgroundImage>,
    started_at: Instant,
    duration: Duration,
}

/// One hover-path existence probe, keyed in `App::path_probe_cache` by the
/// resolved absolute path it stats. See `pointer.rs::probe_path_exists` for
/// the state machine.
struct PathProbeEntry {
    /// Last completed answer, if any; served (even stale) while a
    /// revalidation is in flight so a stationary hover never flickers.
    answer: Option<bool>,
    /// When `answer` was recorded (or the first probe started).
    at: Instant,
    /// Generation of the outstanding worker probe — at most one per path,
    /// no matter how long a wedged volume blocks it. An answer arriving
    /// with any other generation is a superseded straggler and dropped.
    in_flight: Option<u64>,
    /// Windows whose hover asked about this path while the probe was in
    /// flight; each is re-synced when the answer lands.
    waiters: Vec<WindowId>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ProgressFlashKind {
    Success,
    Error,
}

#[derive(Clone, Copy, Debug)]
struct ProgressFlash {
    kind: ProgressFlashKind,
    until: Instant,
}

pub struct App {
    config: AppConfig,
    config_watcher: ConfigWatcher,
    /// Current macOS system appearance (light/dark), seeded from the first
    /// window's `Window::theme()` and kept live via
    /// `WindowEvent::ThemeChanged`. Only consulted when `config.theme_appearance`
    /// (a `theme = light:...,dark:...` pair) is set.
    system_appearance: winit::window::Theme,
    /// Grid padding derived once from `window-padding-x/y`, applied to every
    /// pane's geometry.
    padding: GridPadding,
    /// Initial cursor style from `cursor-style` / `cursor-style-blink`, applied
    /// to each terminal at creation. `None` keeps the terminal default.
    initial_cursor_style: Option<CursorStyle>,
    /// Static or directory-backed `background-image` runtime. Static mode keeps
    /// the existing one decoded PNG; slideshow mode owns the directory snapshot
    /// and the current decoded image.
    background_image: BackgroundImageRuntime,
    /// Next scheduled slideshow rotation wake-up. `None` while static,
    /// exhausted, occluded, or backgrounded.
    live_wallpaper_deadline: Option<Instant>,
    /// Short cross-fade currently being applied after a slideshow rotation.
    live_wallpaper_transition: Option<LiveWallpaperTransition>,
    runtime_font_size: f32,
    proxy: EventLoopProxy<UserEvent>,
    gpu: Option<GpuState>,
    windows: HashMap<WindowId, WindowState>,
    window_order: Vec<WindowId>,
    overview_window: Option<OverviewWindowState>,
    /// Per-TAB overview tile render state (Overview U1): the grid lays out one
    /// tile per tab (window), so dirty/last-render is tracked by `WindowId`.
    /// A pane's pty output marks its whole tab dirty via
    /// `mark_overview_tile_dirty` (which derives the tab from the pane's id),
    /// and the tab's tile is recomposited from all its panes on the next due
    /// frame.
    overview_tiles: HashMap<WindowId, OverviewTileRenderState>,
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
    /// Last keyboard/pty activity on the focused surface, driving the
    /// `cursor-stop-blinking-after` idle stop: once
    /// `cursor_stop_blinking_after_secs` elapse with no activity,
    /// `tick_cursor_blink` settles the cursor solid and disarms its wake-up
    /// so a fully idle app schedules no blink timer at all. Refreshed by
    /// `reset_cursor_blink_phase` (keyboard input, focus/occlusion
    /// transitions, config reload) and by pty-driven redraws targeting the
    /// focused surface.
    cursor_blink_activity_at: Instant,
    /// One-shot deadline for the post-burst memory trim
    /// (`tick_memory_trim`): re-armed by every pty-driven redraw and once at
    /// startup, `None` after firing — an idle app pays no wake-up for this.
    memory_trim_deadline: Option<Instant>,
    /// Monotonic origin for the Kitty-graphics animation clock. Set lazily on the
    /// first animation tick; `advance_kitty_animations` takes ms since this so
    /// `noa-grid` stays timer-free.
    kitty_anim_origin: Option<Instant>,
    /// Next scheduled Kitty animation frame wake-up; `None` while no stored image
    /// is animating (the event loop then needs no wake-up for this).
    kitty_anim_deadline: Option<Instant>,
    /// Deadline for each card's one-shot attention-arrival emphasis, keyed by
    /// card id. Set only on the `false→true` transition, so repeat requests do
    /// not restart visual motion while attention is already pending.
    attention_flash_until: HashMap<SessionCardId, Instant>,
    /// Bounded, one-shot completion/error feedback for `OSC 9;4` transitions.
    /// The persistent bar and label carry meaning after this cue expires.
    progress_flashes: HashMap<SessionCardId, ProgressFlash>,
    /// Short-lived card flash after an automatic approval is injected.
    auto_approve_flash_until: HashMap<SessionCardId, Instant>,
    /// The `(window, pane)` currently carrying a non-`None` `Surface::hover_link`,
    /// if any — tracked so [`App::sync_hover_link`] can clear it when the
    /// mouse moves to a different pane/window (or off any pane) without
    /// having to scan every surface.
    hovered_link: Option<(WindowId, PaneId)>,
    /// Hover-path existence probe results, keyed by resolved absolute path.
    /// Entries expire (see `pointer.rs`) so a file created or deleted after
    /// the probe is re-checked; at a size cap the completed entries are
    /// dropped wholesale (hover churn is tiny and sessions are long) while
    /// in-flight ones survive, keeping each path's worker unique.
    path_probe_cache: HashMap<std::path::PathBuf, PathProbeEntry>,
    /// Monotonic id for [`PathProbeEntry::in_flight`]; a probe answer whose
    /// generation no longer matches is a superseded straggler and dropped.
    next_path_probe_generation: u64,
    /// The open search prompt (Cmd+F), if any — see [`SearchPromptSession`].
    search_prompt: Option<SearchPromptSession>,
    /// Keyboard copy mode, bound to exactly one focused window/pane.
    copy_mode: Option<CopyModeSession>,
    /// Physical presses consumed by copy mode whose matching Kitty release
    /// must not reach the pty, even when the press itself exited the mode.
    copy_mode_suppressed_releases: HashSet<PhysicalKey>,
    /// Enter/Escape presses consumed by copy mode. Their auto-repeats stay
    /// suppressed until release, even when the initial press ended the mode.
    copy_mode_suppressed_repeats: HashSet<PhysicalKey>,
    /// The open command palette (`cmd+shift+p`), if any — see
    /// [`CommandPaletteSession`].
    command_palette: Option<CommandPaletteSession>,
    /// The open send-selection target picker, if any.
    send_selection_picker: Option<SendSelectionPickerSession>,
    /// The endpoint/discovery/target-picker overlay for the single
    /// `Attach Remote` command-palette flow.
    remote_ui: Option<remote_ui::RemoteUiSession>,
    /// The open theme-settings overlay (theme-settings-ui R-1), if any — see
    /// [`ThemeSettingsSession`]. Mutually exclusive with `command_palette`
    /// and `search_prompt` (R-3, `App::active_overlay`).
    theme_settings: Option<ThemeSettingsSession>,
    /// The open process-monitor overlay (panel-metrics-view FR-1), if any —
    /// see [`ProcessMonitorSession`]. Mutually exclusive with the palette,
    /// theme-settings, and search prompt (R-3, `App::active_overlay`).
    process_monitor: Option<ProcessMonitorSession>,
    /// R-29/ADR-5: the theme-settings-v2 favorites store — lazily loaded,
    /// mirrored read-only into each `ThemeSettings` session and updated
    /// (persisted immediately) by a `⌃F` toggle.
    theme_favorites: crate::theme_favorites::ThemeFavorites,
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
    modal_preedit: Option<ModalPreedit>,
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
    /// changed by the Settings overlay without respawning PTYs.
    sidebar_preview_lines_gate: Arc<AtomicUsize>,
    /// The tab groups (logical windows) whose session sidebar is currently
    /// shown (FR-4). Per-window, not app-wide: toggling flips every tab of the
    /// focused window's group at once (tabs of one native window stay
    /// consistent) while other windows keep their own state. Fresh groups are
    /// seeded from `sidebar-enabled`. A quick-terminal window never shows it
    /// regardless (`window_sidebar_eligible`, FR-14).
    sidebar_visible_groups: HashSet<WindowGroupId>,
    /// The dedicated branch-poll worker (FR-8/FR-9): receives OSC-7-driven
    /// cwd-change requests and posts back git branch + project icon as
    /// [`crate::session_store::SessionDelta::Branch`]. Kept off the io read loop
    /// (NFR-2/AC-18) and joined at teardown (Omen T6, in `Drop`).
    branch_poll: Option<crate::branch_poll::BranchPollHandle>,
    /// Serializes and writes session state off the main thread; captures still
    /// happen on the caller. Its `Drop` (as an `App` field) flushes the last
    /// queued state to disk, covering the quit path.
    session_persister: crate::session_persist::SessionPersister,
    /// The installed AppleScript / Apple Event bridge (applescript R-2), kept
    /// alive for the app's lifetime (dropping it removes the handlers). `None`
    /// until installed, when `macos-applescript` is false, or off macOS.
    applescript: Option<crate::macos_applescript::Registration>,
    /// The read-only window/tab/terminal projection the Apple Event handler
    /// answers property queries from (applescript Amendment 1.1). The main
    /// thread rebuilds it in `about_to_wait` while the bridge is installed; the
    /// handler only ever locks and reads it, so scripting never blocks the loop.
    applescript_snapshot: Arc<Mutex<crate::macos_applescript::AppStateSnapshot>>,
    /// Guards the one-time Apple Event handler registration to the first
    /// `resumed`, mirroring `hotkey_install_attempted`.
    applescript_install_attempted: bool,
    /// The cheap structural signature (topology/focus/titles) of the last
    /// AppleScript snapshot rebuild, so `about_to_wait` skips the full
    /// per-pane `terminal.lock()` rebuild when nothing relevant changed.
    applescript_snapshot_sig: u64,
    /// When the AppleScript snapshot was last rebuilt, for the coarse
    /// time-based refresh that keeps cwd/title current under sustained output
    /// without locking every terminal each frame.
    applescript_snapshot_at: Option<Instant>,
    /// The running `noa-ipc` server (noa-server spec FR-1/FR-2), if
    /// `server-enable` is true and the loopback bind succeeded. Dropping it
    /// stops the accept loop; kept alive for the app's lifetime otherwise.
    ipc_server: Option<noa_ipc::ServerHandle>,
    /// The single `Broadcaster` for the app's lifetime (independent of any
    /// one `ipc_server` instance): panes wire their `IpcOutputTap` to a
    /// clone of this, so a config-reload server restart (which drops and
    /// recreates `ipc_server`) never orphans an already-spawned pane's
    /// output push — the new server registers its connections on the same
    /// `Broadcaster` those panes already hold.
    ipc_broadcaster: noa_ipc::Broadcaster,
    /// The main-thread-published IPC read snapshot + pane-id registry (DEC-B),
    /// shared with `AppIpcBackend`'s off-main-thread reads (applescript
    /// bridge's `applescript_snapshot` analog).
    ipc_shared: Arc<Mutex<crate::ipc_bridge::IpcShared>>,
    /// In-flight IPC mutations awaiting a main-thread reply (DEC-C).
    ipc_pending: crate::ipc_bridge::IpcPendingTable,
    /// Monotonic `UserEvent::IpcAction` request-id source.
    ipc_next_request: Arc<AtomicU64>,
    /// Guards the one-time `noa-ipc` server startup to the first `resumed`,
    /// mirroring `applescript_install_attempted`.
    ipc_install_attempted: bool,
    /// The cheap structural signature of the last IPC snapshot rebuild
    /// (mirrors `applescript_snapshot_sig`), so `about_to_wait` skips the
    /// full per-pane rebuild when nothing relevant changed.
    ipc_snapshot_sig: u64,
    /// When the IPC snapshot was last rebuilt, for the coarse time-based
    /// refresh (mirrors `applescript_snapshot_at`).
    ipc_snapshot_at: Option<Instant>,
    /// The short reason the last `install_ipc_server_if_needed` bind attempt
    /// failed (settings-panel-server-status), or `None` while the server is
    /// running, stopped-because-disabled, or has never failed to bind. Never
    /// holds the token itself — only a bind/token-path failure message, and
    /// `noa_ipc::load_or_create_token`'s error text never includes the
    /// token value it failed to load/create. Read by
    /// [`Self::server_status_display`], the Settings panel's read-only
    /// `ServerStatus` row.
    ipc_last_error: Option<String>,
    /// Results from short-lived remote discovery/create workers. UserEvent
    /// carries only the Eq request id; Panel values never cross that enum.
    remote_pending: remote_ui::RemotePendingTable,
    /// Short-lived remote discovery/create/cleanup workers. Keeping their
    /// joins lets final shutdown wait until every result has entered the
    /// pending table before orphan cleanup drains it.
    remote_workers: Vec<std::thread::JoinHandle<()>>,
    remote_next_request: Arc<AtomicU64>,
    /// A pty + default shell spawned concurrently at construction so the
    /// first tab's shell boots in parallel with font discovery, window
    /// creation, and GPU init instead of serialized after them (startup
    /// C1). Consumed by the first [`App::spawn_pane_surface`] whose request
    /// matches what was anticipated (the CLI `-e` command if one was given,
    /// otherwise the default shell; initial grid size; inherited cwd); any
    /// non-matching first spawn discards it (the child is killed by `Pty`'s
    /// `Drop`). With `-e` the prespawn runs that command — prespawning a
    /// default shell instead would hand the first pane a shell and silently
    /// drop the command. `Mutex` only because `spawn_pane_surface` takes
    /// `&self`; there is no cross-thread access.
    prespawned_pty: Mutex<Option<PrespawnedPty>>,
    /// Terminal + sidebar font discovery and the GPU prewarm, started at the
    /// top of [`crate::run`] (see [`StartupTasks`]), consumed by the first
    /// tab spawn.
    startup_tasks: Option<StartupTasks>,
    /// The CLI `-e` command, waiting for the first [`App::spawn_pane_surface`]
    /// to consume it. One-shot (Ghostty `initial-command` parity): only the
    /// first surface runs the command; every later tab/split/quick-terminal
    /// spawn finds the slot empty and gets the normal shell. `Mutex` only
    /// because `spawn_pane_surface` takes `&self`.
    initial_command: Mutex<Option<Vec<String>>>,
    /// Global throttle gate for the tab-switch-stall background pane-cache
    /// refresh (see `helpers::dispatch::background_refresh_selection`):
    /// when any occluded window last had its cache opportunistically
    /// refreshed, across ALL windows — not per-window. A per-window gate
    /// would admit up to one full-viewport rebuild per window per interval,
    /// which with N busy occluded tabs stalls the event loop (and so the
    /// foreground window) for up to `N` rebuilds every interval.
    last_bg_refresh: Option<Instant>,
    /// Occluded windows with pty output observed since their last background
    /// refresh (or since becoming occluded, if never refreshed) — candidates
    /// for the next globally-throttled refresh. Populated only by
    /// `UserEvent::Redraw` (pty-driven), never by a self-armed wake-up, so an
    /// app with no occluded pty activity never spends a cycle on this.
    dirty_occluded_windows: HashSet<WindowId>,
    /// One-shot trailing wake-up for the background-refresh backlog (kaizen
    /// cycle 6, finding P2): armed whenever `dirty_occluded_windows` is
    /// non-empty, at the earliest instant the global throttle reopens —
    /// so a candidate blocked purely by timing (its output landed inside the
    /// throttle window, and no further pty output ever arrives to re-trigger
    /// a check) still gets exactly one retry, rather than sitting stale
    /// until an unrelated event. `None` whenever the backlog is empty: the
    /// `about_to_wait` + `WaitUntil` mechanism then arms no timer for this at
    /// all, same as every other idle-power-sensitive tick here.
    bg_refresh_wake_deadline: Option<Instant>,
}

/// The wgpu foundation prewarmed on a worker: adapter/device are requested
/// without a compatible surface (macOS exposes a single Metal adapter, which
/// can present to any surface), so the ~10 ms of GPU bring-up overlaps
/// event-loop construction instead of serializing after window creation.
pub(crate) struct PrewarmedGpu {
    pub(crate) instance: wgpu::Instance,
    pub(crate) adapter: wgpu::Adapter,
    pub(crate) device: wgpu::Device,
    pub(crate) queue: wgpu::Queue,
}

/// Both system-font discoveries (terminal + sidebar) plus the GPU prewarm
/// running on worker threads, spawned at the very top of [`crate::run`] —
/// before the winit event loop is even built — so the ~60 ms font discovery
/// and the GPU bring-up overlap event-loop construction, `App::new`, and
/// window creation instead of serializing in front of the first window
/// (startup W1). Consumed once by the first tab spawn (the
/// `self.gpu.is_none()` path in `lifecycle.rs`).
///
/// The terminal worker resolves the primary face first, then blocks until
/// the main thread sends the pixel size (unknown until a monitor is known)
/// over `terminal_px_tx`, replies with the primary's cell [`noa_font::Metrics`]
/// on `terminal_metrics_rx` (sizing the first window needs only this), and
/// finishes the full fallback-stack discovery in the background.
pub(crate) struct StartupTasks {
    terminal_px_tx: Sender<f32>,
    terminal_metrics_rx: crossbeam_channel::Receiver<noa_font::Metrics>,
    terminal_stack: std::thread::JoinHandle<Result<noa_font::FontStack, noa_font::FontError>>,
    sidebar: std::thread::JoinHandle<
        Result<(noa_font::FontStack, noa_font::FontConfig), noa_font::FontError>,
    >,
    gpu: std::thread::JoinHandle<Result<PrewarmedGpu, String>>,
}

impl StartupTasks {
    pub(crate) fn spawn(config: &AppConfig) -> Self {
        let font_cfg = font_config_from_noa_config(&config.font);
        let (terminal_px_tx, px_rx) = crossbeam_channel::bounded(1);
        let (metrics_tx, terminal_metrics_rx) = crossbeam_channel::bounded(1);
        let terminal_stack = std::thread::spawn({
            let font_cfg = font_cfg.clone();
            move || {
                let primary = noa_font::load_primary_font(&font_cfg)?;
                // A disconnected px channel means the app is exiting without
                // ever spawning a window; finishing the stack is harmless.
                if let Ok(px) = px_rx.recv()
                    && let Ok(font_ref) = primary.font_ref()
                {
                    let _ = metrics_tx.send(noa_font::Metrics::compute(font_ref, px));
                }
                noa_font::load_font_stack_with_primary(primary, &font_cfg)
            }
        });
        // The sidebar font repeats the same (relatively slow) discovery at its
        // own pixel size; the stack is scale-independent (only
        // `FontGrid::with_stack` consumes a pixel size), so it runs fully in
        // parallel and is joined only when the first `GpuState` is built.
        let sidebar = std::thread::spawn(move || {
            noa_font::load_font_stack(&font_cfg).map(|stack| (stack, font_cfg))
        });
        let gpu = std::thread::spawn(|| {
            let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor::default());
            let adapter =
                pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
                    power_preference: wgpu::PowerPreference::default(),
                    compatible_surface: None,
                    force_fallback_adapter: false,
                }))
                .map_err(|e| format!("no compatible GPU adapter found ({e})"))?;
            crate::startup_trace::mark("gpu-adapter-ready");
            let (device, queue) =
                pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
                    label: Some("noa-device"),
                    required_features: wgpu::Features::empty(),
                    required_limits: wgpu::Limits::default(),
                    experimental_features: wgpu::ExperimentalFeatures::default(),
                    memory_hints: wgpu::MemoryHints::default(),
                    trace: wgpu::Trace::Off,
                }))
                .map_err(|e| format!("could not open a GPU device ({e})"))?;
            crate::startup_trace::mark("gpu-device-ready");
            Ok(PrewarmedGpu {
                instance,
                adapter,
                device,
                queue,
            })
        });
        Self {
            terminal_px_tx,
            terminal_metrics_rx,
            terminal_stack,
            sidebar,
            gpu,
        }
    }

    /// Ask the terminal-font worker for the primary face's metrics at
    /// `px_size`. An `Err` means the worker failed before resolving a primary
    /// (no usable font): join [`Self::into_handles`]' stack for the cause.
    pub(crate) fn terminal_metrics(&self, px_size: f32) -> Result<noa_font::Metrics, ()> {
        let _ = self.terminal_px_tx.send(px_size);
        self.terminal_metrics_rx.recv().map_err(|_| ())
    }

    #[allow(clippy::type_complexity)]
    pub(crate) fn into_handles(
        self,
    ) -> (
        std::thread::JoinHandle<Result<noa_font::FontStack, noa_font::FontError>>,
        std::thread::JoinHandle<
            Result<(noa_font::FontStack, noa_font::FontConfig), noa_font::FontError>,
        >,
        std::thread::JoinHandle<Result<PrewarmedGpu, String>>,
    ) {
        (self.terminal_stack, self.sidebar, self.gpu)
    }
}

/// See [`App::prespawned_pty`].
struct PrespawnedPty {
    size: GridSize,
    /// The `-e` command the prespawn was started with (`None` = default
    /// shell). A spawn request only consumes the prespawn when it asks for
    /// exactly this command — a mismatch here is how `-e` used to be
    /// silently discarded in favor of the prespawned shell.
    command: Option<Vec<String>>,
    handle: std::thread::JoinHandle<noa_pty::Result<Pty>>,
}

/// Whether a pane's spawn request matches what the prespawned pty anticipated
/// (see [`App::prespawned_pty`]): the same child command (the CLI `-e` argv,
/// or `None` for the default shell), the initial grid size, inheriting the
/// process cwd, with login + shell-integration defaults.
fn prespawn_matches(
    config: &PtyConfig,
    prespawn_size: GridSize,
    prespawn_command: Option<&[String]>,
) -> bool {
    config.command.as_deref() == prespawn_command
        && config.cwd.is_none()
        && config.shell.is_none()
        && config.size == prespawn_size
        && config.login
        && config.shell_integration
}

impl App {
    pub fn new(
        config: AppConfig,
        proxy: EventLoopProxy<UserEvent>,
        startup_tasks: StartupTasks,
    ) -> Self {
        let padding = resolve_grid_padding(config.window_padding_x, config.window_padding_y);
        let initial_cursor_style =
            resolve_cursor_style(config.cursor_style, config.cursor_style_blink);
        let background_image = load_background_image_runtime(&config);
        let (keybinds, keybind_diagnostics) =
            KeybindEngine::from_config(&config.keybinds, config.sidebar_hotkey.as_deref());
        for diagnostic in keybind_diagnostics {
            log::warn!("config keybind: {diagnostic}");
        }
        // Clone the proxy for the session-metadata worker before `proxy` is
        // moved into the struct — it posts `SessionDelta::Branch`/`Process` back
        // over it. The worker also shares the sidebar-visible gate so its
        // process poll only ticks while a sidebar is shown (AC-18).
        let proxy_for_branch_poll = proxy.clone();
        let initial_command = config.launch_command.clone();
        let sidebar_visible_gate = Arc::new(AtomicBool::new(false));
        let sidebar_preview_lines_gate = Arc::new(AtomicUsize::new(config.sidebar_preview_lines));
        // Boot the first tab's child now — the CLI `-e` command if one was
        // given, otherwise the default shell — in parallel with everything
        // between here and `spawn_pane_surface` (font discovery, window
        // creation, GPU init) — see the `prespawned_pty` field doc. Client
        // mode attaches to a remote pane and never spawns locally, so skip
        // it there.
        let prespawned_pty = (config.client_remote.is_none()).then(|| {
            let size = GridSize::new(config.cols, config.rows);
            let command = initial_command.clone();
            PrespawnedPty {
                size,
                command: command.clone(),
                handle: std::thread::spawn(move || {
                    let pty = Pty::spawn(PtyConfig {
                        size,
                        command,
                        ..Default::default()
                    });
                    crate::startup_trace::mark("pty-prespawned");
                    pty
                }),
            }
        });
        App {
            config_watcher: ConfigWatcher::new(config.config_default_files),
            // Corrected once the first window exists and can report
            // `Window::theme()` (see `lifecycle.rs`); light is a harmless
            // placeholder until then since startup theme resolution for a
            // `theme_appearance` pair happens after that point.
            system_appearance: winit::window::Theme::Light,
            padding,
            initial_cursor_style,
            background_image,
            live_wallpaper_deadline: None,
            live_wallpaper_transition: None,
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
            keybinds,
            overview_visible: false,
            overview_visible_gate: Arc::new(AtomicBool::new(false)),
            overview_wake_deadline: None,
            cursor_blink_visible: true,
            cursor_blink_deadline: None,
            cursor_blink_activity_at: Instant::now(),
            // Armed at startup so the launch transients (font-discovery and
            // config-parse scratch) are returned to the OS shortly after the
            // first window settles.
            memory_trim_deadline: Some(Instant::now() + timers::MEMORY_TRIM_QUIESCENCE),
            kitty_anim_origin: None,
            kitty_anim_deadline: None,
            attention_flash_until: HashMap::new(),
            progress_flashes: HashMap::new(),
            auto_approve_flash_until: HashMap::new(),
            hovered_link: None,
            path_probe_cache: HashMap::new(),
            next_path_probe_generation: 0,
            search_prompt: None,
            copy_mode: None,
            copy_mode_suppressed_releases: HashSet::new(),
            copy_mode_suppressed_repeats: HashSet::new(),
            command_palette: None,
            send_selection_picker: None,
            remote_ui: None,
            theme_settings: None,
            process_monitor: None,
            theme_favorites: crate::theme_favorites::ThemeFavorites::new(),
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
            branch_poll: Some(crate::branch_poll::spawn(proxy_for_branch_poll)),
            session_persister: crate::session_persist::SessionPersister::spawn(),
            sidebar_visible_gate,
            sidebar_preview_lines_gate,
            sidebar_visible_groups: HashSet::new(),
            applescript: None,
            applescript_snapshot: Arc::new(Mutex::new(
                crate::macos_applescript::AppStateSnapshot::default(),
            )),
            applescript_install_attempted: false,
            applescript_snapshot_sig: 0,
            applescript_snapshot_at: None,
            ipc_server: None,
            ipc_broadcaster: noa_ipc::Broadcaster::new(),
            ipc_shared: Arc::new(Mutex::new(crate::ipc_bridge::IpcShared::default())),
            ipc_pending: Arc::new(Mutex::new(HashMap::new())),
            ipc_next_request: Arc::new(AtomicU64::new(1)),
            ipc_install_attempted: false,
            ipc_snapshot_sig: 0,
            ipc_snapshot_at: None,
            ipc_last_error: None,
            remote_pending: Arc::new(Mutex::new(HashMap::new())),
            remote_workers: Vec::new(),
            remote_next_request: Arc::new(AtomicU64::new(1)),
            prespawned_pty: Mutex::new(prespawned_pty),
            startup_tasks: Some(startup_tasks),
            initial_command: Mutex::new(initial_command),
            last_bg_refresh: None,
            dirty_occluded_windows: HashSet::new(),
            bg_refresh_wake_deadline: None,
        }
    }

    /// Hand out the prespawned pty if `config` matches what it anticipated
    /// (see [`App::prespawned_pty`]). The slot is emptied on the first call
    /// either way: a non-matching first spawn (restore forcing a cwd, a
    /// differently sized quick terminal) discards it on a reaper thread so
    /// the stale shell child is killed without blocking this spawn.
    fn take_prespawned_pty(&self, config: &PtyConfig) -> Option<Pty> {
        let prespawn = self.prespawned_pty.lock().take()?;
        if !prespawn_matches(config, prespawn.size, prespawn.command.as_deref()) {
            std::thread::spawn(move || drop(prespawn.handle.join()));
            return None;
        }
        match prespawn.handle.join() {
            Ok(Ok(pty)) => Some(pty),
            Ok(Err(err)) => {
                // Fall back to the caller's own `Pty::spawn`, which reports
                // its own (almost certainly identical) error to the user.
                log::warn!("prespawned pty failed, respawning: {err}");
                None
            }
            Err(_) => {
                log::warn!("prespawned pty thread panicked, respawning");
                None
            }
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

    /// The three keyboard-encoding modes read on every key-encode pass,
    /// returned `(app_cursor_keys, app_keypad, kitty_keyboard_flags)` under a
    /// single terminal lock. One acquisition rather than three keeps the
    /// key-input path off the io thread's output-batch lock longer than
    /// necessary (input-latency under heavy pty output).
    fn key_encode_modes(&self, window_id: WindowId) -> (bool, bool, u8) {
        self.windows
            .get(&window_id)
            .and_then(WindowState::focused_surface)
            .map(|surface| {
                let terminal = surface.terminal.lock();
                (
                    terminal.modes.app_cursor_keys(),
                    terminal.modes.app_keypad(),
                    terminal.kitty_keyboard_flags(),
                )
            })
            .unwrap_or((false, false, 0))
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
        self.shutdown_remote_requests();
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

#[cfg(test)]
mod tests {
    use super::*;

    fn initial_size() -> GridSize {
        GridSize::new(80, 24)
    }

    fn dash_e_argv() -> Vec<String> {
        vec!["/bin/sh".into(), "-c".into(), "echo hi".into()]
    }

    // Regression: a `-e <command>` first spawn must NOT consume a prespawned
    // default shell — doing so silently discarded the requested command
    // (`noa -e /bin/sh -c '...'` ran a plain shell instead).
    #[test]
    fn command_spawn_rejects_a_default_shell_prespawn() {
        let config = PtyConfig {
            size: initial_size(),
            command: Some(dash_e_argv()),
            ..Default::default()
        };

        assert!(!prespawn_matches(&config, initial_size(), None));
    }

    // With `-e`, the prespawn is started with that command and the first
    // spawn must still consume it (warm startup stays on the fast path).
    #[test]
    fn command_spawn_consumes_a_prespawn_of_the_same_command() {
        let config = PtyConfig {
            size: initial_size(),
            command: Some(dash_e_argv()),
            ..Default::default()
        };

        assert!(prespawn_matches(
            &config,
            initial_size(),
            Some(dash_e_argv()).as_deref()
        ));
    }

    // The inverse: a default-shell spawn must not steal a `-e` prespawn.
    #[test]
    fn shell_spawn_rejects_a_command_prespawn() {
        let config = PtyConfig {
            size: initial_size(),
            ..Default::default()
        };

        assert!(!prespawn_matches(
            &config,
            initial_size(),
            Some(dash_e_argv()).as_deref()
        ));
    }

    // The default first spawn (no -e, no cwd override, initial size) is
    // exactly what the default prespawn anticipated — the warm-startup fast
    // path.
    #[test]
    fn prespawn_matches_the_default_first_spawn() {
        let config = PtyConfig {
            size: initial_size(),
            ..Default::default()
        };

        assert!(prespawn_matches(&config, initial_size(), None));
    }

    // Non-default requests (restored cwd, custom shell, different grid size,
    // no login/integration) must respawn rather than reuse the prespawn.
    #[test]
    fn prespawn_is_rejected_for_non_default_requests() {
        let base = || PtyConfig {
            size: initial_size(),
            ..Default::default()
        };

        let mut with_cwd = base();
        with_cwd.cwd = Some("/tmp".into());
        assert!(!prespawn_matches(&with_cwd, initial_size(), None));

        let mut with_shell = base();
        with_shell.shell = Some("/bin/bash".into());
        assert!(!prespawn_matches(&with_shell, initial_size(), None));

        let resized = base();
        assert!(!prespawn_matches(&resized, GridSize::new(120, 40), None));

        let mut no_login = base();
        no_login.login = false;
        assert!(!prespawn_matches(&no_login, initial_size(), None));

        let mut no_integration = base();
        no_integration.shell_integration = false;
        assert!(!prespawn_matches(&no_integration, initial_size(), None));
    }
}
