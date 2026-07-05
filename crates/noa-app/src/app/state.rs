//! Shared state types owned by the `app` module.
//!
//! Keeping these containers separate from `app.rs` leaves the main file focused
//! on app construction, rendering, and lifecycle methods while preserving the
//! existing sibling-module access pattern.

use super::*;

/// App-wide GPU and glyph state shared by every tab/window.
pub(super) struct GpuState {
    pub(super) instance: wgpu::Instance,
    pub(super) adapter: wgpu::Adapter,
    pub(super) device: wgpu::Device,
    pub(super) queue: wgpu::Queue,
    pub(super) font: FontGrid,
    /// Dedicated, smaller font for the session sidebar (mockup-dense typography,
    /// [`SIDEBAR_FONT_POINT_SIZE`]), sized independently of the terminal font
    /// and rebuilt on a scale change alongside `font`.
    pub(super) sidebar_font: FontGrid,
    pub(super) theme: Theme,
    /// Single reused `Renderer` that rasterizes the whole sidebar band as
    /// synthetic terminal cells (Omen T3: one renderer for every card, never
    /// per-card). Built lazily for the first window's surface format.
    pub(super) sidebar_renderer: Option<Renderer>,
    /// Rounded-card pipeline reused to composite the rasterized sidebar band
    /// onto each window's surface (CardStyle/overlay_texture_cards).
    pub(super) sidebar_card: Option<OverviewChromeCardPipeline>,
    /// The band texture the sidebar rasterizes into, cached with its size so it
    /// is reused frame-to-frame and only reallocated when the band dimensions
    /// change (a window resize or sidebar-width change). This is the flat dark
    /// backdrop (header/toolbar + card text) that per-card rounded cards overlay.
    pub(super) sidebar_band: Option<(PixelSize, wgpu::Texture, wgpu::TextureView)>,
    /// Reused scratch texture for one rounded session card (inset x card
    /// height): each visible card is rendered into it then composited as a
    /// rounded card in turn, so a single texture serves every card without a
    /// per-card allocation (Omen T3: still one renderer, one card texture).
    pub(super) sidebar_card_tex: Option<(PixelSize, wgpu::Texture, wgpu::TextureView)>,
    /// Reused scratch texture for the open card `...` menu popup, composited above
    /// the cards so a rounded card can never hide it.
    pub(super) sidebar_menu_tex: Option<(PixelSize, wgpu::Texture, wgpu::TextureView)>,
    /// Reused scratch texture for the toolbar `+` button tile, composited as a
    /// small rounded chrome card with a border (and an accent ring on hover).
    /// Refilled with the glyph color to also composite the two `+` bars.
    pub(super) sidebar_button_tex: Option<(PixelSize, wgpu::Texture, wgpu::TextureView)>,
    /// Reused scratch texture for the hairline divider at the sidebar/pane
    /// edge; the soft shadow comes from the band glow.
    pub(super) sidebar_divider_tex: Option<(PixelSize, wgpu::Texture, wgpu::TextureView)>,
    /// Reused scratch texture for the drag-reorder drop-indicator line: a solid
    /// `CHROME_ACCENT` strip composited at the insertion gap during an active
    /// card drag.
    pub(super) sidebar_drop_tex: Option<(PixelSize, wgpu::Texture, wgpu::TextureView)>,
    /// Single reused `Renderer` that rasterizes the open command palette's block
    /// (query + list) as terminal cells into `palette_scratch`, then composited
    /// as one rounded card (H). Built lazily for the first window's format.
    pub(super) palette_renderer: Option<Renderer>,
    /// Rounded-card pipeline reused to composite the rasterized palette block as
    /// a single rounded card (rounded corners + border + drop shadow, H).
    pub(super) palette_card: Option<OverviewChromeCardPipeline>,
    /// The palette block texture, cached with its size so it is reused
    /// frame-to-frame and only reallocated when the block dimensions change.
    pub(super) palette_scratch: Option<(PixelSize, wgpu::Texture, wgpu::TextureView)>,
    /// The interior pixel padding the current `palette_renderer` was built
    /// with; the renderer is rebuilt when this drifts (font size change).
    pub(super) palette_padding: noa_core::GridPadding,
    /// 1x1 translucent-black texture drawn as a full-pane card behind the
    /// palette; the modal scrim dimming the pane underneath.
    pub(super) palette_scrim: Option<(wgpu::Texture, wgpu::TextureView)>,
    /// 1x1 theme-tinted texture for the scrollback thumb drawn along a
    /// scrolled pane's right edge (its alpha carries the thumb opacity).
    pub(super) scrollbar_tex: Option<(wgpu::Texture, wgpu::TextureView)>,
    /// 1x1 translucent-white texture for the `visual-bell` full-window flash.
    pub(super) bell_flash_tex: Option<(wgpu::Texture, wgpu::TextureView)>,
}

/// Identifies one logical window, i.e. one AppKit tab group. Every native tab
/// ([`WindowState`]) carries the id of the window it belongs to; tabs sharing an
/// id are tabbed together on macOS, while a fresh id starts a separate native
/// window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) struct WindowGroupId(pub(super) u64);

/// Whether a spawned tab joins the focused window or opens a new one; the only
/// difference between `New Tab` (`cmd+t`) and `New Window` (`cmd+n`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SpawnTarget {
    /// Join the focused window's tab group (a fresh group if nothing is
    /// focused, e.g. the very first tab at startup).
    CurrentWindow,
    /// Always start a fresh tab group / native window.
    NewWindow,
}

/// State for one native tab. On macOS, each tab is an NSWindow in the same
/// AppKit tab group; winit still reports them as distinct `WindowId`s.
pub(super) struct WindowState {
    pub(super) window: Arc<Window>,
    /// The logical window (AppKit tab group) this tab belongs to.
    pub(super) group: WindowGroupId,
    pub(super) surface: wgpu::Surface<'static>,
    pub(super) surface_config: wgpu::SurfaceConfiguration,
    pub(super) renderer: Renderer,
    pub(super) split_tree: SplitTree,
    pub(super) zoomed: Option<PaneId>,
    pub(super) focused_pane: PaneId,
    pub(super) next_pane_id: u64,
    pub(super) surfaces: HashMap<PaneId, Surface>,
    pub(super) last_mouse_pane: Option<PaneId>,
    pub(super) last_mouse_point: Option<split_tree::Point>,
    pub(super) active_split_drag: Option<SplitResizeDrag>,
    pub(super) occluded: bool,
    pub(super) title: String,
    /// Vertical scroll offset (px) of the sidebar card list (FR-15), clamped to
    /// `[0, content_h - viewport_h]` when consumed by the layout.
    pub(super) sidebar_scroll: u32,
    /// Whether the pointer is currently over the toolbar `+` button, driving its
    /// hover style (brighter fill + accent ring) and the pointer cursor icon.
    pub(super) sidebar_button_hover: bool,
    /// The card whose `...` menu popup is open in this window (FR-7), or `None`.
    pub(super) sidebar_menu: Option<SessionCardId>,
    /// An in-flight sidebar card drag-reorder, or `None`.
    pub(super) sidebar_drag: Option<SidebarDrag>,
    /// Set when a left press was consumed by Cmd+click-to-open, so only the
    /// matching release is swallowed.
    pub(super) link_click_in_flight: bool,
    /// The focused pane's last laid-out grid size, for the resize-overlay
    /// change check (`resize-overlay = after-first` skips the first layout).
    pub(super) last_grid: Option<(u16, u16)>,
    /// The live `cols × rows` resize toast: its text and hide deadline.
    pub(super) resize_overlay: Option<(String, Instant)>,
    /// `visual-bell`: the full-window flash stays up until this instant.
    pub(super) bell_flash_until: Option<Instant>,
}

/// How long the `cols × rows` resize toast stays up after the last grid
/// change (Ghostty's `resize-overlay-duration` default).
pub(super) const RESIZE_OVERLAY_DURATION: Duration = Duration::from_millis(750);

/// How long the `visual-bell` flash stays up.
pub(super) const BELL_FLASH_DURATION: Duration = Duration::from_millis(150);

/// An in-flight sidebar card drag-reorder (FR: card reordering). Recorded on a
/// left-press over a card; the drag only becomes `active` once the pointer
/// moves past a threshold, so a press-then-release without movement stays a
/// plain card-select click.
#[derive(Clone, Copy, Debug)]
pub(super) struct SidebarDrag {
    /// The card being dragged.
    pub(super) card: SessionCardId,
    /// Pointer y at press (physical px), to measure the drag threshold.
    pub(super) start_y: i64,
    /// Offset (physical px) from the card's top to the grab point, so the
    /// floating card follows the cursor without jumping.
    pub(super) grab_dy: i64,
    /// Latest pointer y (physical px), updated on cursor-moved.
    pub(super) current_y: i64,
    /// True once the pointer moved past the threshold; distinguishes a drag
    /// from a click.
    pub(super) active: bool,
}

/// State for the dedicated overview window. It deliberately is not part of
/// `windows`/`window_order`, which are terminal-tab collections.
pub(super) struct OverviewWindowState {
    pub(super) window: Arc<Window>,
    pub(super) occluded: bool,
    pub(super) last_cursor_point: Option<split_tree::Point>,
    pub(super) surface: wgpu::Surface<'static>,
    pub(super) surface_config: wgpu::SurfaceConfiguration,
    /// Shared scratch + per-tile textures (REQ-NF-3), sized for every live
    /// mirror tile and every title-only placeholder tile (REQ-OV-10).
    pub(super) thumbnails: Option<OverviewThumbnailResources>,
    /// Single small `Renderer` dedicated to drawing placeholder-row title text.
    pub(super) label_renderer: Option<Renderer>,
    /// Rounded-card shader reused for overview chrome overlays.
    pub(super) chrome_card: Option<OverviewChromeCardPipeline>,
    /// The currently selected tile (REQ-OV-14): an index directly into the
    /// row-major source-tile order (`App::overview_source_tile_ids`).
    pub(super) selected: usize,
    /// The tile index currently under the mouse cursor, for hover feedback.
    pub(super) hovered: Option<usize>,
    /// Whether the selected tile is zoomed (Tab toggles).
    pub(super) zoomed: bool,
    /// The in-flight zoom transition, if any (`zoomed` is the target state).
    pub(super) zoom_anim: Option<OverviewZoomAnim>,
    /// The live "Search sessions" filter query (REQ-OV-16).
    pub(super) search_query: String,
}

/// One in-flight quick-look zoom transition on the overview's selected tile.
#[derive(Clone, Copy, Debug)]
pub(super) struct OverviewZoomAnim {
    pub(super) tween: crate::anim::Tween,
    /// `true` while expanding toward the zoomed rect, `false` collapsing back.
    pub(super) expanding: bool,
}

pub(super) struct OverviewChromeCardPipeline {
    pub(super) format: wgpu::TextureFormat,
    pub(super) pipeline: CardPipeline,
}

pub(super) struct OverviewChromeTexture {
    pub(super) texture: wgpu::Texture,
    pub(super) rect: PaneRectApp,
}

#[derive(Clone, Copy, Debug, Default)]
pub(super) struct OverviewTileRenderState {
    pub(super) dirty: bool,
    pub(super) last_render_at: Option<Instant>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) struct OverviewTileId {
    pub(super) window_id: WindowId,
    pub(super) pane_id: PaneId,
}

impl OverviewTileId {
    pub(super) const fn new(window_id: WindowId, pane_id: PaneId) -> Self {
        Self { window_id, pane_id }
    }
}

/// An open search prompt (Cmd+F), scoped to the window/pane it was opened for.
pub(super) struct SearchPromptSession {
    pub(super) window_id: WindowId,
    pub(super) pane_id: PaneId,
    pub(super) prompt: SearchPrompt,
}

/// An open command palette (`cmd+shift+p`), bound to the window it was opened
/// from. Only one exists at a time app-wide (`App::toggle_command_palette`).
pub(super) struct CommandPaletteSession {
    pub(super) window_id: WindowId,
    pub(super) palette: CommandPalette,
    /// When the palette opened, driving its brief fade-in
    /// ([`crate::anim::DUR_FAST`]).
    pub(super) opened_at: Instant,
}

/// An open inline rename on a sidebar card (FR-7 Rename). Modal for its
/// window's keyboard while it is open.
pub(super) struct SidebarRenameSession {
    pub(super) window_id: WindowId,
    pub(super) card: SessionCardId,
    pub(super) buffer: String,
}

/// An open confirmation dialog (paste protection or OSC 52 clipboard-read),
/// bound to the window it was raised from.
pub(super) struct ConfirmDialogSession {
    pub(super) window_id: WindowId,
    pub(super) message: String,
    pub(super) hint: String,
    pub(super) action: ConfirmAction,
}

/// The deferred side effect a [`ConfirmDialogSession`] runs on confirmation.
pub(super) enum ConfirmAction {
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
pub(super) struct Surface {
    pub(super) terminal: Arc<Mutex<Terminal>>,
    pub(super) pty_input_tx: Sender<crate::io_thread::PtyInput>,
    pub(super) resize_tx: Sender<GridSize>,
    pub(super) io_thread: Option<crate::io_thread::IoThreadHandle>,
    pub(super) grid_size: GridSize,
    pub(super) mouse_selection: MouseSelectionState,
    pub(super) last_mouse_cell: Option<Point>,
    pub(super) pressed_mouse_button: Option<MouseButton>,
    pub(super) ime_state: input::ImeState,
    pub(super) rect: PaneRectApp,
    /// The Cmd+hover underline target for this pane, recomputed on every
    /// `CursorMoved`/`ModifiersChanged` (`App::sync_hover_link`) and fed into
    /// `FrameSnapshot::hover_link` at redraw.
    pub(super) hover_link: Option<HoverLink>,
    /// The Session Overview mirror's read-only publish slot (Fix B, REQ-NF-6).
    pub(super) overview_snapshot: Arc<Mutex<Option<Arc<FrameSnapshot>>>>,
    /// Previous frame's snapshot rows, handed back after each redraw so
    /// `FrameSnapshot::from_terminal_recycled` can reuse the row/cell
    /// allocations instead of cloning the grid into fresh heap every frame.
    pub(super) snapshot_recycle: Vec<noa_grid::Row>,
}

impl WindowState {
    pub(super) fn shutdown(&mut self) {
        shutdown_pane_io_threads(self.surfaces.values_mut());
    }

    pub(super) fn focused_surface(&self) -> Option<&Surface> {
        self.surfaces.get(&self.focused_pane)
    }

    pub(super) fn focused_surface_mut(&mut self) -> Option<&mut Surface> {
        self.surfaces.get_mut(&self.focused_pane)
    }

    pub(super) fn pane_count(&self) -> usize {
        self.surfaces.len()
    }

    pub(super) fn contains_pane(&self, pane_id: PaneId) -> bool {
        self.surfaces.contains_key(&pane_id)
    }
}

impl Surface {
    pub(super) fn shutdown(&mut self) {
        if let Some(io_thread) = self.io_thread.take() {
            io_thread.shutdown_and_join();
        }
    }
}

/// DECSCUSR `Blinking*` cursor styles toggle visibility on this interval while
/// focused and displayable.
pub(super) const CURSOR_BLINK_INTERVAL: Duration = Duration::from_millis(600);

/// How long an attention marker blinks before settling to a steady mark
/// (FR-A1). Compile-time (no config knob in v1).
pub(super) const ATTENTION_BLINK_DURATION: Duration = Duration::from_secs(6);

/// The blink half-period (~1.5 Hz).
pub(super) const ATTENTION_BLINK_INTERVAL: Duration = Duration::from_millis(333);

/// Card styling for the Session Overview composite (REQ-OV-12/14). A function
/// (not a const) because the chrome colors follow the terminal theme's
/// polarity, selected at startup.
pub(super) fn overview_card_style() -> CardStyle {
    CardStyle {
        background: overview_bg_color(),
        border_color: overview_border_color(),
        focus_color: overview_focus_ring_color(),
        corner_radius: OVERVIEW_CARD_CORNER_RADIUS,
        border_width: OVERVIEW_CARD_BORDER_WIDTH,
        focus_width: OVERVIEW_CARD_FOCUS_WIDTH,
        focus_glow_width: OVERVIEW_CARD_FOCUS_GLOW_WIDTH,
    }
}

/// Attention styling for an Overview tile with a pending interaction request.
pub(super) fn overview_attention_card_style() -> CardStyle {
    CardStyle {
        focus_color: crate::chrome::rgba(crate::chrome::palette().dot_red),
        focus_width: crate::chrome::RING_ATTENTION,
        focus_glow_width: crate::chrome::GLOW_ATTENTION,
        ..overview_card_style()
    }
}

/// Rounded styling for Overview chrome pills (search and shortcut hint).
pub(super) fn overview_chrome_card_style() -> CardStyle {
    CardStyle {
        background: overview_bg_color(),
        border_color: overview_chrome_border_color(),
        focus_color: overview_chrome_border_color(),
        corner_radius: OVERVIEW_CARD_CORNER_RADIUS,
        border_width: 1.0,
        focus_width: 1.0,
        focus_glow_width: 0.0,
    }
}
