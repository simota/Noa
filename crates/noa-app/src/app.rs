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
    self, Direction, HitTarget, ImeOp, MIN_PANE_SIZE_PX, PaneId, Rect as PaneRectApp,
    SPLIT_RESIZE_STEP_PX, SplitOrientation, SplitResizeDrag, SplitTree, equalize,
    focus_in_direction, focus_switch_plan, hit_test, resize_split, resize_split_to_drag_point,
    split_pane, split_resize_drag_target_at_point, zoom_resize_targets, zoom_toggle,
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
mod split_ops;
mod state;
mod timers;

pub use config::AppConfig;
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
            attention_blink_deadline: None,
            hovered_link: None,
            search_prompt: None,
            command_palette: None,
            theme_settings: None,
            confirm_dialog: None,
            sidebar_rename: None,
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

    fn redraw(&mut self, window_id: WindowId) {
        // Build the sidebar's draw model up front (reads only the store + pure
        // layout, AC-17) before borrowing `gpu`/`state` mutably, so the band can
        // be composited inline after the panes without a second borrow.
        let sidebar_model = self.sidebar_draw_model(window_id);
        let padding = self.padding;
        // Resolve the open palette's render payload up front (like the sidebar
        // model) so the rounded card can be composited after the panes without
        // re-borrowing `self` — the palette is drawn as its own card (H), not
        // inline in the pane cell pass.
        let palette_card = self
            .command_palette
            .as_ref()
            .filter(|session| session.window_id == window_id)
            .map(|session| {
                let mut snapshot = command_palette_snapshot(&self.keybinds, &session.palette);
                // Live IME composition appends to the displayed query
                // (display only — it filters entries once committed).
                snapshot
                    .query
                    .push_str(self.modal_preedit_for(window_id, ModalImeTarget::CommandPalette));
                (snapshot, session.opened_at)
            });
        // Same for the theme-settings overlay: its own modal card, mutually
        // exclusive with the palette (R-3) so only one of the two is ever
        // `Some` here.
        let theme_settings_card = self
            .theme_settings
            .as_ref()
            .filter(|session| session.window_id == window_id)
            .map(|session| (session.state.clone(), session.opened_at));
        // Same for the confirm dialog: composited as its own modal card after
        // the panes (and above the palette — it blocks input), not inline in
        // the pane cell pass.
        let dialog_card = self
            .confirm_dialog
            .as_ref()
            .filter(|session| session.window_id == window_id)
            .map(|session| noa_render::ConfirmDialogSnapshot {
                message: session.message.clone(),
                hint: session.hint.clone(),
            });
        // Resolved before the `gpu`/`state` borrows below (the snapshot loop
        // holds them mutably).
        let search_preedit = self
            .modal_preedit_for(window_id, ModalImeTarget::SearchPrompt)
            .to_string();
        let (Some(gpu), Some(state)) = (self.gpu.as_mut(), self.windows.get_mut(&window_id)) else {
            return;
        };
        #[cfg(target_os = "macos")]
        {
            crate::macos_window::set_window_background_color(
                &state.window,
                gpu.theme.default_bg,
                self.config.background_opacity,
            );
            if needs_macos_titlebar_backdrop(self.config.background_opacity) {
                crate::macos_window::install_titlebar_backdrop(&state.window, gpu.theme.default_bg);
            }
        }
        if state.occluded {
            return;
        }

        let mut snapshots = Vec::new();
        let mut title = "Noa".to_string();
        // Scrolled panes' scrollbar-thumb state, captured under the same
        // terminal lock the snapshot takes (no extra lock later).
        let mut scroll_thumbs: Vec<sidebar::ScrollThumb> = Vec::new();
        let visible_panes = visible_pane_ids(&state.split_tree, state.zoomed);
        for pane_id in visible_panes {
            let Some(surface) = state.surfaces.get_mut(&pane_id) else {
                continue;
            };
            let mut term = surface.terminal.lock();
            if pane_id == state.focused_pane {
                title = tab_title(&term.title);
            }
            if term.viewport_offset() > 0 {
                scroll_thumbs.push(sidebar::ScrollThumb {
                    rect: render_pane_rect(surface.rect),
                    offset: term.viewport_offset(),
                    scrollback: term.scrollback_len(),
                    viewport_rows: term.active().rows,
                });
            }
            let mut snapshot = FrameSnapshot::from_terminal_recycled(
                &mut term,
                std::mem::take(&mut surface.snapshot_recycle),
            );
            snapshot.search_prompt = self
                .search_prompt
                .as_ref()
                .filter(|session| session.window_id == window_id && session.pane_id == pane_id)
                .map(|session| {
                    // Live IME composition appends to the displayed query
                    // (display only — it joins the real buffer on commit).
                    format!("{}{search_preedit}", session.prompt.buffer())
                });
            // A pane draws a solid cursor only when it is both the split's
            // focused pane AND its window has OS focus; otherwise (an
            // inactive split pane, or any pane in an unfocused window) it
            // draws the hollow outline instead of hiding the cursor outright.
            // An open search prompt also hollows the cursor: keystrokes go to
            // the prompt, not the shell, so the pane must not read as
            // type-able while the prompt has the keyboard.
            snapshot.focused =
                pane_owns_keyboard_focus(window_id, pane_id, self.os_focused, state.focused_pane)
                    && snapshot.search_prompt.is_none();
            snapshot.cursor_blink_visible = self.cursor_blink_visible;
            snapshot.hover_link = surface.hover_link;
            // Neither the palette nor the confirm dialog draws in the pane
            // cell pass — both are composited as rounded modal cards after
            // the panes (H). Leave `snapshot.command_palette` and
            // `snapshot.confirm_dialog` at their `None` defaults here.
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
        state.renderer.rebuild_panes(
            &panes,
            &mut gpu.font,
            active_theme(&gpu.theme, &gpu.preview_theme),
        );
        state
            .renderer
            .sync_atlas(&gpu.device, &gpu.queue, &mut gpu.font);

        let frame = match state.surface.get_current_texture() {
            Ok(frame) => frame,
            // OutOfMemory is not recoverable by reconfiguring; anything else
            // (Lost/Outdated/Timeout/Other) gets a reconfigure + retry so a
            // transient error can't leave the window permanently frozen.
            Err(wgpu::SurfaceError::OutOfMemory) => {
                log::error!("surface out of memory; skipping frame");
                return;
            }
            Err(e) => {
                if !matches!(e, wgpu::SurfaceError::Lost | wgpu::SurfaceError::Outdated) {
                    log::warn!("surface error: {e}; reconfiguring");
                }
                state.surface.configure(&gpu.device, &state.surface_config);
                state.window.request_redraw();
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
        // Scrollback thumbs along scrolled panes' right edges (state-driven:
        // only panes with `viewport_offset > 0` collected one).
        if !scroll_thumbs.is_empty() {
            let surface_size = PixelSize {
                w: state.surface_config.width,
                h: state.surface_config.height,
            };
            sidebar::draw_scrollbar_thumbs(
                gpu,
                state.surface_config.format,
                &view,
                surface_size,
                &scroll_thumbs,
                state.window.scale_factor() as f32,
            );
        }
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
        // Composite the open command palette as a rounded card over the focused
        // pane, on top of the panes and sidebar so the modal always wins (H).
        // A brief eased fade-in on open; repaints ride request_redraw until
        // the fade settles.
        if let Some((palette, opened_at)) = palette_card.as_ref()
            && let Some((_, rect, snapshot)) = snapshots
                .iter()
                .find(|(pane_id, _, _)| *pane_id == state.focused_pane)
        {
            let surface_size = PixelSize {
                w: state.surface_config.width,
                h: state.surface_config.height,
            };
            let fade = crate::anim::Tween::new(*opened_at, crate::anim::DUR_FAST);
            let now = Instant::now();
            sidebar::draw_command_palette_card(
                gpu,
                state.surface_config.format,
                &view,
                surface_size,
                palette,
                render_pane_rect(*rect),
                snapshot.cols,
                snapshot.rows_n,
                padding,
                state.window.scale_factor() as f32,
                fade.progress(now),
            );
            if !fade.done(now) {
                state.window.request_redraw();
            }
        }
        // The theme-settings overlay composites at the same tier as the
        // palette (mutually exclusive with it, R-3) — same fade-in.
        if let Some((theme_settings, opened_at)) = theme_settings_card.as_ref()
            && let Some((_, rect, snapshot)) = snapshots
                .iter()
                .find(|(pane_id, _, _)| *pane_id == state.focused_pane)
        {
            let surface_size = PixelSize {
                w: state.surface_config.width,
                h: state.surface_config.height,
            };
            let fade = crate::anim::Tween::new(*opened_at, crate::anim::DUR_FAST);
            let now = Instant::now();
            sidebar::draw_theme_settings_card(
                gpu,
                state.surface_config.format,
                &view,
                surface_size,
                theme_settings,
                render_pane_rect(*rect),
                snapshot.cols,
                snapshot.rows_n,
                padding,
                state.window.scale_factor() as f32,
                fade.progress(now),
            );
            if !fade.done(now) {
                state.window.request_redraw();
            }
        }
        // The confirm dialog composites last: it blocks input, so it must win
        // over the palette card too.
        if let Some(dialog) = dialog_card.as_ref()
            && let Some((_, rect, snapshot)) = snapshots
                .iter()
                .find(|(pane_id, _, _)| *pane_id == state.focused_pane)
        {
            let surface_size = PixelSize {
                w: state.surface_config.width,
                h: state.surface_config.height,
            };
            sidebar::draw_confirm_dialog_card(
                gpu,
                state.surface_config.format,
                &view,
                surface_size,
                dialog,
                render_pane_rect(*rect),
                snapshot.cols,
                snapshot.rows_n,
                padding,
                state.window.scale_factor() as f32,
            );
        }
        // Transient overlays last, above every modal: the `cols × rows`
        // resize toast and the visual-bell flash (both expire via
        // `tick_transient_overlays`).
        let now = Instant::now();
        if let Some((text, until)) = state.resize_overlay.clone()
            && now < until
        {
            let surface_size = PixelSize {
                w: state.surface_config.width,
                h: state.surface_config.height,
            };
            sidebar::draw_toast_card(
                gpu,
                state.surface_config.format,
                &view,
                surface_size,
                &text,
                state.window.scale_factor() as f32,
            );
        }
        if state.bell_flash_until.is_some_and(|until| now < until) {
            let surface_size = PixelSize {
                w: state.surface_config.width,
                h: state.surface_config.height,
            };
            sidebar::draw_bell_flash(gpu, state.surface_config.format, &view, surface_size);
        }
        frame.present();

        // An atlas-eviction-unstable frame may have drawn some glyphs with
        // another glyph's pixels; ask for one more frame so the display
        // converges instead of sticking on the corrupt one.
        if state.renderer.needs_follow_up_frame() {
            state.window.request_redraw();
        }

        // Hand each snapshot's row buffer back to its pane so the next
        // frame's `from_terminal_recycled` reuses the allocations.
        for (pane_id, _, snapshot) in snapshots {
            if let Some(surface) = state.surfaces.get_mut(&pane_id) {
                surface.snapshot_recycle = snapshot.rows;
            }
        }
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
            AppCommand::ToggleTabOverview => self.toggle_tab_overview(),
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
            AppCommand::OpenThemeSettings => self.open_theme_settings(),
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
        } else if let Some(window_id) = self.focused
            && self.active_overlay(window_id) == ActiveOverlay::None
        {
            self.command_palette = Some(CommandPaletteSession {
                window_id,
                palette: CommandPalette::open(),
                opened_at: Instant::now(),
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
            GroupChoice::Fresh => {
                let group = self.allocate_group_id();
                // A fresh logical window starts with the configured sidebar
                // default; a tab joining an existing group inherits that
                // group's current state by construction.
                if self.config.sidebar_enabled {
                    self.sidebar_visible_groups.insert(group);
                }
                group
            }
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
                .unwrap_or_else(|e| gpu_init_fatal("could not create the window surface", e));
            let adapter =
                pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
                    power_preference: wgpu::PowerPreference::default(),
                    compatible_surface: Some(&surface),
                    force_fallback_adapter: false,
                }))
                .unwrap_or_else(|e| gpu_init_fatal("no compatible GPU adapter found", e));
            let (device, queue) =
                pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
                    label: Some("noa-device"),
                    required_features: wgpu::Features::empty(),
                    required_limits: wgpu::Limits::default(),
                    experimental_features: wgpu::ExperimentalFeatures::default(),
                    memory_hints: wgpu::MemoryHints::default(),
                    trace: wgpu::Trace::Off,
                }))
                .unwrap_or_else(|e| gpu_init_fatal("could not open a GPU device", e));
            // Validation errors and device loss must not abort inside the
            // macOS winit delegate (non-unwinding); log them instead. Device
            // loss then surfaces as SurfaceError::Lost on the next frame and
            // goes through the reconfigure path in `redraw`.
            device.set_device_lost_callback(|reason, message| {
                log::error!("wgpu device lost ({reason:?}): {message}");
            });
            device.on_uncaptured_error(Arc::new(|err| {
                log::error!("wgpu uncaptured error: {err}");
            }));
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
                .unwrap_or_else(|e| gpu_init_fatal("could not load the sidebar font", e)),
                theme: {
                    let theme = crate::theme::resolve_theme_with_overrides(
                        self.config.theme.as_deref(),
                        &self.theme_overrides(),
                    );
                    // Chrome (sidebar/overview) polarity follows the terminal
                    // theme: a light theme gets light chrome.
                    crate::chrome::select_palette(theme.is_light());
                    theme
                },
                preview_theme: None,
                chrome_textures: ChromeTextures::default(),
                palette_renderer: None,
                palette_card: None,
                palette_padding: noa_core::GridPadding::ZERO,
                palette_scrim: None,
            });
            surface
        } else {
            let gpu = self.gpu.as_ref().expect("gpu initialized");
            gpu.instance
                .create_surface(window.clone())
                .unwrap_or_else(|e| gpu_init_fatal("could not create the window surface", e))
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
            .unwrap_or_else(|e| gpu_init_fatal("could not build the renderer", e));
            renderer.set_background_opacity(self.config.background_opacity);
            renderer.set_background_image(&gpu.device, &gpu.queue, self.background_image.clone());
            renderer.resize(PixelSize {
                w: surface_config.width,
                h: surface_config.height,
            });
            (surface_config, renderer)
        };

        // A translucent window leaves native titlebar/tab chrome compositing
        // against undefined pixels; back the strip with an opaque theme view.
        #[cfg(target_os = "macos")]
        {
            let bg = self.gpu.as_ref().expect("gpu initialized").theme.default_bg;
            crate::macos_window::set_window_background_color(
                &window,
                bg,
                self.config.background_opacity,
            );
            if needs_macos_titlebar_backdrop(self.config.background_opacity) {
                crate::macos_window::install_titlebar_backdrop(&window, bg);
            }
        }

        let window_id = window.id();
        let initial_pane = PaneId::new(1);
        let initial_rect = content_inset_bounds(
            PaneRectApp::new(0, 0, surface_config.width, surface_config.height),
            crate::macos_window::top_chrome_inset_px(&window).unwrap_or_else(|| {
                titlebar_top_inset_px(self.config.macos_titlebar_style, window.scale_factor())
            }),
            content_margin_px(self.config.macos_titlebar_style, window.scale_factor()),
        );
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
                sidebar_scroll: 0,
                sidebar_button_hover: false,
                sidebar_menu: None,
                sidebar_drag: None,
                link_click_in_flight: false,
                last_grid: None,
                resize_overlay: None,
                bell_flash_until: None,
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
        terminal.title_report = self.config.title_report;
        terminal.set_scrollback_limit_bytes(self.config.scrollback_limit);
        if let Some(gpu) = self.gpu.as_ref() {
            // Deliberately `gpu.theme` directly, not the `active_theme()`
            // resolver: a live theme preview must never reach a `Terminal`'s
            // `TerminalColors` (AC-2, spec L2 "Terminal生成箇所には手を入れない").
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
            preview_lines: self.sidebar_preview_lines_gate.clone(),
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
            selection_anchor: None,
            last_mouse_cell: None,
            pressed_mouse_button: None,
            ime_state: input::ImeState::default(),
            rect,
            hover_link: None,
            overview_snapshot,
            snapshot_recycle: Vec::new(),
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
        // The Overview overlay lives inside its host window; closing the host
        // tears the overlay down with it (before `close_tab_outcome`, so the
        // last-window case quits instead of keeping a ghost overlay alive).
        if self.overview_host() == Some(window_id) {
            self.overview_window = None;
            self.overview_visible = false;
            self.overview_visible_gate.store(false, Ordering::Relaxed);
        }
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
        // Same leak shape as the palette: a theme-settings overlay bound to
        // the closed window would strand a dead-window reference. Drop the
        // preview along with it — nothing else can clear it once its owning
        // window is gone.
        if self
            .theme_settings
            .as_ref()
            .is_some_and(|session| session.window_id == window_id)
        {
            self.theme_settings = None;
            if let Some(gpu) = self.gpu.as_mut() {
                gpu.preview_theme = None;
            }
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
        // Drop sidebar visibility for a group whose last tab just closed, so
        // the set only ever holds live logical windows.
        let live_groups: HashSet<WindowGroupId> =
            self.windows.values().map(|state| state.group).collect();
        self.sidebar_visible_groups
            .retain(|group| live_groups.contains(group));
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
        if !self.config.confirm_quit {
            event_loop.exit();
            return;
        }
        let count = self.app_running_program_count();
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
