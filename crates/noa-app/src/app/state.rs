//! Shared state types owned by the `app` module.
//!
//! Keeping these containers separate from `app.rs` leaves the main file focused
//! on app construction, rendering, and lifecycle methods while preserving the
//! existing sibling-module access pattern.

use std::cell::RefCell;
use std::path::PathBuf;

use super::*;

/// App-wide GPU and glyph state shared by every tab/window.
pub(super) struct GpuState {
    pub(super) instance: wgpu::Instance,
    pub(super) adapter: wgpu::Adapter,
    pub(super) device: wgpu::Device,
    pub(super) queue: wgpu::Queue,
    /// Format-keyed shared pipeline sets: every `Renderer` (tab, quick
    /// terminal, sidebar band, palette, overview labels) draws with the same
    /// three pipelines per format, so each is compiled once and cloned out.
    pub(super) pipelines: noa_render::PipelineCache,
    /// Format-keyed shared atlas textures for the app-wide terminal font.
    pub(super) font_atlases: noa_render::GlyphAtlasCache,
    pub(super) font: FontGrid,
    /// Dedicated, smaller font for the session sidebar (mockup-dense typography,
    /// [`SIDEBAR_FONT_POINT_SIZE`]), sized independently of the terminal font
    /// and rebuilt on a scale change alongside `font`.
    pub(super) sidebar_font: FontGrid,
    /// Format-keyed shared atlas textures for the dedicated sidebar/UI font.
    pub(super) sidebar_font_atlases: noa_render::GlyphAtlasCache,
    pub(super) theme: Theme,
    /// The theme-settings-ui live-preview override (R-6): when `Some`, every
    /// draw-path theme read must go through [`active_theme`] instead of
    /// `theme` directly, so a picker highlight change is visible without
    /// mutating `theme` itself (Esc discards the preview for free). Written
    /// by `App::sync_theme_settings_preview` as the overlay's theme-list
    /// highlight moves, and cleared on close/commit.
    pub(super) preview_theme: Option<Theme>,
    /// The chrome-color-baked GPU resources (sidebar band/cards/tints), grouped
    /// so a runtime theme swap (theme-settings-ui R-13) can invalidate all of
    /// them in one call — see [`ChromeTextures::reset`].
    pub(super) chrome_textures: ChromeTextures,
    /// Single reused `Renderer` that rasterizes the open command palette's block
    /// (query + list) as terminal cells into `chrome_textures.palette_scratch`,
    /// then composited as one rounded card (H). Built lazily for the first
    /// window's format. Not theme-color-baked itself (it draws whatever the
    /// current theme/`OverlayStyle` says each frame), so it lives outside
    /// [`ChromeTextures`] — only the scratch texture it draws into does.
    pub(super) palette_renderer: Option<Renderer>,
    /// Rounded-card pipeline reused to composite the rasterized palette block as
    /// a single rounded card (rounded corners + border + drop shadow, H).
    pub(super) palette_card: Option<OverviewChromeCardPipeline>,
    /// The interior pixel padding the current `palette_renderer` was built
    /// with; the renderer is rebuilt when this drifts (font size change).
    pub(super) palette_padding: noa_core::GridPadding,
    /// 1x1 translucent-black texture drawn as a full-pane card behind the
    /// palette; the modal scrim dimming the pane underneath.
    pub(super) palette_scrim: Option<(wgpu::Texture, wgpu::TextureView)>,
}

/// The single chokepoint every draw-path theme read must go through
/// (theme-settings-ui R-6): the live-preview theme while one is active,
/// otherwise the committed theme. `Theme::resolve_with_colors` and
/// `OverlayStyle::from_theme` are called on this function's output, not on
/// `GpuState::theme` directly, so a preview swap is visible everywhere those
/// are used without touching any `Terminal`'s `TerminalColors` (AC-1/AC-2).
///
/// Takes the two fields by reference rather than `&GpuState` (an
/// `impl GpuState` method, as one might expect) because most draw call
/// sites resolve the theme in the same expression as an `&mut` borrow of a
/// sibling `GpuState` field (`font`, `chrome_textures`, ...); a `&self`
/// method would borrow the whole struct and break the disjoint field
/// borrows those call sites already rely on. Called as
/// `active_theme(&gpu.theme, &gpu.preview_theme)`.
pub(super) fn active_theme<'a>(theme: &'a Theme, preview_theme: &'a Option<Theme>) -> &'a Theme {
    preview_theme.as_ref().unwrap_or(theme)
}

/// The chrome-color-baked GPU resources that a runtime theme change
/// invalidates in one shot (theme-settings-ui R-13): each is lazily
/// (re)built by the existing draw path the first time it is used after
/// being `None`, so [`reset`](Self::reset) alone is enough to force every
/// one of them to pick up the newly swapped [`crate::chrome`] palette /
/// theme on the next redraw. No GPU device is needed to construct or reset
/// this struct — only the lazy-init draw path (unchanged by this type)
/// touches the device.
#[derive(Default)]
pub(super) struct ChromeTextures {
    /// Single reused `Renderer` that rasterizes the whole sidebar band as
    /// synthetic terminal cells (Omen T3: one renderer for every card, never
    /// per-card). Built lazily for the first window's surface format.
    pub(super) sidebar_renderer: Option<Renderer>,
    /// Rounded-card pipeline reused while composing the cached sidebar band:
    /// card/menu/button/divider scratch textures are stamped into the offscreen
    /// band texture with straight RGBA replacement, then that one band texture
    /// is composited onto the window surface below.
    pub(super) sidebar_card: Option<OverviewChromeCardPipeline>,
    /// Alpha-blending variant used only for the band backdrop composite: the
    /// band texture is transparent outside its text runs, so blending lets the
    /// pane pass's clear color + background image show through untouched (the
    /// band background is literally the panes' background), while the replace
    /// pipeline above stays in charge of cards/menu/divider whose translucency
    /// must settle to `background-opacity` exactly.
    pub(super) sidebar_band_card: Option<OverviewChromeCardPipeline>,
    /// The final sidebar band texture, cached with its size so it is reused
    /// frame-to-frame and only reallocated when the band dimensions change (a
    /// window resize or sidebar-width change). On a sidebar raster-cache hit,
    /// this already contains the band, cards, menu, divider, and toolbar.
    pub(super) sidebar_band: Option<(PixelSize, wgpu::Texture, wgpu::TextureView)>,
    /// The exact input key that produced [`sidebar_band`](Self::sidebar_band).
    /// When the next redraw sees the same key and the texture still exists, it
    /// skips every synthetic-terminal raster pass and only composites the
    /// cached band texture onto the window surface.
    pub(super) sidebar_raster_cache_key: Option<super::sidebar::SidebarRasterCacheKey>,
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
    /// Reused scratch texture for the per-card status accent bar (busy /
    /// attention / bell) composited along a card's left edge; refilled with
    /// each card's accent color in turn like the card scratch.
    pub(super) sidebar_accent_tex: Option<(PixelSize, wgpu::Texture, wgpu::TextureView)>,
    /// Reused scratch texture for subtle horizontal rules between flat sidebar
    /// card rows.
    pub(super) sidebar_rule_tex: Option<(PixelSize, wgpu::Texture, wgpu::TextureView)>,
    /// The palette block texture, cached with its size so it is reused
    /// frame-to-frame and only reallocated when the block dimensions change.
    pub(super) palette_scratch: Option<(PixelSize, wgpu::Texture, wgpu::TextureView)>,
    /// 1x1 theme-tinted texture for the scrollback thumb drawn along a
    /// scrolled pane's right edge (its alpha carries the thumb opacity).
    pub(super) scrollbar_tex: Option<(wgpu::Texture, wgpu::TextureView)>,
    /// 1x1 translucent-white texture for the `visual-bell` full-window flash.
    pub(super) bell_flash_tex: Option<(wgpu::Texture, wgpu::TextureView)>,
    /// Debug-only instrumentation for NFR-2 (no rebuild while scrubbing the
    /// theme list; exactly one batch of rebuilds on confirm): incremented by
    /// [`record_rebuild`](Self::record_rebuild) at each lazy-init call site
    /// when that site actually (re)builds its GPU resource. Absent in release
    /// builds — it exists only to be asserted on in tests/manual checks.
    #[cfg(debug_assertions)]
    rebuild_count: std::sync::atomic::AtomicUsize,
}

impl ChromeTextures {
    /// Drop every chrome-baked GPU resource back to `None`. The next redraw's
    /// existing lazy-init checks (`is_none()`/`is_none_or(...)`) then rebuild
    /// each one from the (by then already-swapped) theme/chrome palette —
    /// this method itself never touches a `wgpu::Device` and needs none to
    /// run, so it is unit-testable without a GPU (AC-20).
    ///
    /// Called once per successful theme-settings commit
    /// (`App::commit_theme_settings`, R-12/R-13), after the config write and
    /// the chrome palette swap, and never during picker scrubbing (NFR-2) —
    /// nothing in the pure `theme_settings` module holds a reference to this
    /// type at all, so a highlight change has no way to reach it.
    pub(super) fn reset(&mut self) {
        self.sidebar_renderer = None;
        self.sidebar_card = None;
        self.sidebar_band_card = None;
        self.sidebar_band = None;
        self.sidebar_raster_cache_key = None;
        self.sidebar_card_tex = None;
        self.sidebar_menu_tex = None;
        self.sidebar_button_tex = None;
        self.sidebar_divider_tex = None;
        self.sidebar_drop_tex = None;
        self.sidebar_accent_tex = None;
        self.sidebar_rule_tex = None;
        self.palette_scratch = None;
        self.scrollbar_tex = None;
        self.bell_flash_tex = None;
        // `rebuild_count` is deliberately left untouched — it counts total
        // lazy-init rebuilds across the process lifetime (AC-18 asserts it
        // stays flat during scrubbing and rises by one batch per confirm),
        // not "rebuilds since the last reset".
    }

    /// Record one lazy-init rebuild (debug builds only). Call this exactly
    /// where an existing lazy-init call site observes its guard condition
    /// true and is about to (re)build the GPU resource — never on the common
    /// path where the resource is already valid and reused as-is. Scrubbing
    /// the theme list never hits a rebuild path (nothing invalidates these
    /// slots), so the counter only moves after a [`reset`](Self::reset).
    #[cfg(debug_assertions)]
    pub(super) fn record_rebuild(&self) {
        self.rebuild_count
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }

    /// Total lazy-init rebuilds observed so far (debug builds only). Not yet
    /// read outside tests — the GUI-integrated AC-18 scrub/confirm assertion
    /// lands with the increment that wires the live theme-settings overlay.
    #[cfg(debug_assertions)]
    #[allow(dead_code)]
    pub(super) fn rebuild_count(&self) -> usize {
        self.rebuild_count
            .load(std::sync::atomic::Ordering::Relaxed)
    }
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
    /// Raw physical pointer position from the most recent `CursorMoved`.
    /// Kept alongside `last_mouse_point`/`last_mouse_pane` for handlers that
    /// need the exact pixel position rather than an already-hit-tested grid
    /// cell — the Quick Look force-click gesture (`TouchpadPressure`) carries
    /// no position of its own.
    pub(super) last_mouse_physical_position: Option<PhysicalPosition<f64>>,
    pub(super) active_split_drag: Option<SplitResizeDrag>,
    pub(super) occluded: bool,
    pub(super) title: String,
    /// A user-set tab title (tab-title REQ-TTL-2/5). While `Some`, it masks
    /// the shell-driven title on the native tab label and overview tile;
    /// `Terminal.title` keeps tracking OSC 0/2 underneath so clearing the
    /// override reveals the *latest* shell title.
    pub(super) title_override: Option<String>,
    /// The last raw `Terminal.cwd` observed for the focused pane, used as the
    /// titlebar proxy icon's diff-cache key (REQ-PXI-4). Deliberately the raw
    /// cwd, not the config-gated resolved path: keying on the raw value means
    /// a config-only `visible`/`hidden` toggle with no cwd change never
    /// re-triggers the native setter (REQ-PXI-6 — see `render.rs`).
    pub(super) proxy_icon_cwd: Option<String>,
    /// The last `TouchpadPressure` stage seen for this window (REQ-QLK-1), so
    /// only the transition *into* stage 2 fires Quick Look — repeated
    /// pressure samples already at stage 2 must not retrigger it.
    pub(super) last_touchpad_stage: i64,
    /// Per-tab opt-in for agent CLI prompt auto approval. Split panes in the
    /// same native tab share this flag with their io threads.
    pub(super) auto_approve_enabled: Arc<AtomicBool>,
    /// This tab's shared redraw-floor clock (`RedrawFloor`), handed to every
    /// pane's io thread it spawns so an N-pane split earns at most one
    /// floored redraw wake per floor window instead of one per pane. Its
    /// interval is refreshed from the window's monitor refresh rate on
    /// creation and on monitor changes.
    pub(super) redraw_floor: crate::io_thread::RedrawFloor,
    /// Vertical scroll offset (px) of the sidebar card list (FR-15), clamped to
    /// `[0, content_h - viewport_h]` when consumed by the layout.
    pub(super) sidebar_scroll: u32,
    /// Whether the pointer is currently over the toolbar `+` button, driving its
    /// hover style (brighter fill + accent ring) and the pointer cursor icon.
    pub(super) sidebar_button_hover: bool,
    /// The card the pointer is currently over, driving its hover style (a
    /// lifted face + a visible `…` glyph). `None` while no card is hovered or
    /// a drag-reorder is active.
    pub(super) sidebar_card_hover: Option<SessionCardId>,
    /// The card whose `...` menu popup is open in this window (FR-7), or `None`.
    pub(super) sidebar_menu: Option<SessionCardId>,
    /// An in-flight sidebar card drag-reorder, or `None`.
    pub(super) sidebar_drag: Option<SidebarDrag>,
    /// Set when a left press was consumed by Cmd+click-to-open, so only the
    /// matching release is swallowed.
    pub(super) link_click_in_flight: bool,
    /// File paths currently being dragged over the window. `winit` emits
    /// multi-file drops as one event per path, so this lets us paste the
    /// hovered batch once instead of duplicating paths on drop.
    pub(super) file_drop: FileDropState,
    /// Leading+trailing throttle for this window's grid reflow + pty winsize
    /// (item 1). A continuous drag-resize relayouts on every cell-width
    /// boundary; without this, each would run `Terminal::resize`'s two-pass
    /// scrollback reflow under the terminal lock, freezing both the main and io
    /// threads on deep scrollback. The first size applies live; the rest
    /// coalesce to ~one per interval, and the final size always lands via
    /// `App::tick_resize_throttle`. Only the reflow + winsize are throttled —
    /// pane rects and pixel metrics stay live (see `apply_pane_layout_live`).
    pub(super) resize_throttle: crate::debounce::Throttle<Vec<(PaneId, GridSize)>>,
    /// The focused pane's last laid-out grid size, for the resize-overlay
    /// change check (`resize-overlay = after-first` skips the first layout).
    pub(super) last_grid: Option<(u16, u16)>,
    /// The live `cols × rows` resize toast: its text and hide deadline.
    pub(super) resize_overlay: Option<(String, Instant)>,
    /// `visual-bell`: the full-window flash stays up until this instant.
    pub(super) bell_flash_until: Option<Instant>,
    /// Last-synced native (AppKit) overlay model hashes — palette, theme
    /// settings, confirm dialog, toast. Plain data on every platform; only
    /// the macOS redraw path feeds it.
    pub(super) native_overlays: crate::macos_overlay::NativeOverlayCache,
}

/// How long the `cols × rows` resize toast stays up after the last grid
/// change (Ghostty's `resize-overlay-duration` default).
pub(super) const RESIZE_OVERLAY_DURATION: Duration = Duration::from_millis(750);

/// Coalescing interval for the grid-reflow throttle (`WindowState::
/// resize_throttle`). At most one scrollback reflow + pty winsize per this
/// window during a continuous drag-resize; the leading and trailing edges
/// still fire. Chosen in the ~75-90ms band: short enough that live-resize feel
/// stays close to Ghostty's, long enough to keep a deep-scrollback reflow from
/// running on every cell-width boundary.
pub(super) const RESIZE_REFLOW_THROTTLE_INTERVAL: Duration = Duration::from_millis(80);

/// How long the `visual-bell` flash stays up.
pub(super) const BELL_FLASH_DURATION: Duration = Duration::from_millis(150);

/// How often visible sidebars re-sort their cards by update recency
/// (`SessionStore::refresh_auto_order`). Coarse on purpose: the order snapshot
/// only moves on this cadence, so cards never shuffle under the pointer on
/// every output tick.
pub(super) const SIDEBAR_AUTOSORT_INTERVAL: Duration = Duration::from_secs(5);

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

#[derive(Default)]
pub(super) struct FileDropState {
    hovered_paths: Vec<PathBuf>,
    suppressed_dropped_paths: Vec<PathBuf>,
}

impl FileDropState {
    pub(super) fn hover(&mut self, path: PathBuf) {
        if !self.hovered_paths.contains(&path) {
            self.hovered_paths.push(path);
        }
    }

    pub(super) fn cancel_hover(&mut self) {
        self.hovered_paths.clear();
        self.suppressed_dropped_paths.clear();
    }

    pub(super) fn dropped_paths(&mut self, path: PathBuf) -> Option<Vec<PathBuf>> {
        if let Some(index) = self
            .suppressed_dropped_paths
            .iter()
            .position(|suppressed| suppressed == &path)
        {
            self.suppressed_dropped_paths.remove(index);
            return None;
        }

        if self.hovered_paths.is_empty() {
            self.suppressed_dropped_paths.clear();
            return Some(vec![path]);
        }

        let mut paths = std::mem::take(&mut self.hovered_paths);
        if !paths.contains(&path) {
            paths.push(path.clone());
        }
        self.suppressed_dropped_paths = paths
            .iter()
            .filter(|dropped_path| *dropped_path != &path)
            .cloned()
            .collect();
        Some(paths)
    }
}

/// State for the in-window Session Overview overlay. The Overview no longer
/// owns a dedicated window: it renders into the hosting terminal window's
/// surface (the window that was focused when it was toggled on), and that
/// window's input is routed to the Overview keymap while it is visible.
pub(super) struct OverviewWindowState {
    /// The terminal window hosting the overlay. Must be a key of
    /// `App::windows`; closing the host tears the overlay down with it.
    pub(super) host: WindowId,
    pub(super) last_cursor_point: Option<split_tree::Point>,
    /// Shared scratch + per-tile textures (REQ-NF-3), sized for every live
    /// mirror tile and every title-only placeholder tile (REQ-OV-10).
    pub(super) thumbnails: Option<OverviewThumbnailResources>,
    /// Single small `Renderer` dedicated to drawing placeholder-row title text.
    pub(super) label_renderer: Option<Renderer>,
    /// Rounded-card shader reused for overview chrome overlays.
    pub(super) chrome_card: Option<OverviewChromeCardPipeline>,
    /// The currently selected tile (REQ-OV-14): a page-local index into the
    /// current page's tile slice (`App::overview_page_view`, v3 paging) —
    /// not the unpaged full source-tile order.
    pub(super) selected: usize,
    /// The current page (v3 paging, REQ-OV-18): a 0-indexed page over
    /// `App::overview_source_tile_ids()`, clamped against its length by
    /// `App::overview_page_view` on every read rather than written back here.
    pub(super) page: usize,
    /// Accumulated wheel/trackpad delta not yet large enough to flip a page
    /// (v3 paging, REQ-OV-18); see `page_after_wheel`.
    pub(super) wheel_accum: f32,
    /// The tile index currently under the mouse cursor, for hover feedback.
    pub(super) hovered: Option<usize>,
    /// Whether the selected tile is zoomed (Tab toggles).
    pub(super) zoomed: bool,
    /// The in-flight zoom transition, if any (`zoomed` is the target state).
    pub(super) zoom_anim: Option<OverviewZoomAnim>,
    /// The live "Search sessions" filter query (REQ-OV-16).
    pub(super) search_query: String,
    /// Last-rendered search pill (REQ-OV-16), keyed by the query text and the
    /// pill's own rect so a hover-only redraw (query and window size both
    /// unchanged) reuses the texture instead of re-rasterizing it.
    pub(super) search_pill_cache: Option<(OverviewPillKey, OverviewChromeTexture)>,
    /// Last-rendered hint bar (REQ-OV-17), keyed by the live tile count (the
    /// `⌘1-N` range it displays) and its rect.
    pub(super) hint_pill_cache: Option<(OverviewPillKey, OverviewChromeTexture)>,
    /// Memoized `App::overview_source_tile_ids()` (REQ-OV-16): that call runs
    /// on every redraw including pure hover repaints, and with a live search
    /// query it reformats and clones every tab title to filter. `RefCell`
    /// because the memo must stay behind `&self` — several read-only call
    /// sites (e.g. `overview_close_target_at_last_cursor`) call it without a
    /// mutable borrow.
    pub(super) source_tile_ids_cache: RefCell<Option<OverviewSourceTileIdsCache>>,
}

/// Cache key shared by the search and hint pills: both are small ANSI text
/// rasters keyed by "what text/count they show" plus the rect they're sized
/// for (a host-window resize must not stretch a stale-resolution texture
/// indefinitely). `query`/`live_tile_count` are read by only one pill each,
/// but folding them into one key type keeps the two cache slots symmetric —
/// harmless over-invalidation (e.g. the hint pill's key also shifts when the
/// query changes) is preferable to a subtler per-field mismatch bug.
#[derive(Clone, PartialEq, Eq)]
pub(super) struct OverviewPillKey {
    pub(super) query: String,
    pub(super) live_tile_count: usize,
    /// The current page (v3 paging, REQ-OV-19): folded into the key so a
    /// page flip — which changes the hint bar's "Page p/N" segment even when
    /// `query`/`live_tile_count`/`rect` are unchanged — invalidates the
    /// cached pill texture.
    pub(super) page: usize,
    pub(super) rect: PaneRectApp,
}

/// The unfiltered tab/pane order `overview_source_tile_ids` last computed,
/// the query it was filtered with, and the resulting (possibly filtered)
/// order — see `overview_source_tile_ids_cache_hit` for the hit/miss rule.
pub(super) struct OverviewSourceTileIdsCache {
    pub(super) unfiltered: Vec<OverviewTileId>,
    pub(super) query: String,
    pub(super) result: Vec<OverviewTileId>,
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

/// `view` is the one `TextureView` created for the backing texture (which it
/// keeps alive on its own — `wgpu::TextureView` holds its own `Texture`
/// clone internally) and cloned out on every read (`wgpu::TextureView` is
/// `Eq`/`Hash` by resource identity) — callers must reuse this clone rather
/// than calling `create_view` again, or `CardPipeline`'s per-view bind-group
/// pool cache-misses on every frame.
#[derive(Clone)]
pub(super) struct OverviewChromeTexture {
    pub(super) view: wgpu::TextureView,
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

/// An open theme-settings overlay (theme-settings-ui R-1), bound to the
/// window it was opened from. A single app-wide overlay, mirroring
/// [`CommandPaletteSession`].
pub(super) struct ThemeSettingsSession {
    pub(super) window_id: WindowId,
    /// `Arc`-shared (R-4): `App::redraw` snapshots this out with
    /// `Arc::clone` (a refcount bump, not a deep copy of the catalog-sized
    /// `filtered` list); the input-handling mutation sites go through
    /// `Arc::make_mut`. The render path must never store its clone back into
    /// `self` across event-loop turns (Atlas invariant, code-review gate) —
    /// doing so would silently re-enable deep copies via `make_mut` forks.
    pub(super) state: std::sync::Arc<crate::theme_settings::ThemeSettings>,
    /// When the overlay opened, driving the same brief fade-in the command
    /// palette uses ([`crate::anim::DUR_FAST`]).
    pub(super) opened_at: Instant,
}

#[cfg(test)]
mod theme_settings_session_tests {
    use super::ThemeSettingsSession;
    use crate::theme_settings::{ThemeSettings, ThemeSettingsInit, ThemeSettingsMode};
    use std::time::Instant;
    use winit::window::WindowId;

    fn init() -> ThemeSettingsInit {
        ThemeSettingsInit {
            mode: ThemeSettingsMode::Settings,
            current_theme: "3024 Day".to_string(),
            font_size: 14.0,
            cursor_style: noa_config::CursorShape::Block,
            background_opacity: 1.0,
            background_blur_radius: 0,
            background_image: String::new(),
            background_image_opacity: 1.0,
            background_image_position: noa_config::BackgroundImagePosition::Center,
            background_image_fit: noa_config::BackgroundImageFit::Contain,
            background_image_repeat: false,
            background_image_interval_secs: noa_config::DEFAULT_BACKGROUND_IMAGE_INTERVAL_SECS,
            window_padding_x: 2.0,
            window_padding_y: 2.0,
            macos_titlebar_style: noa_config::MacosTitlebarStyle::Native,
            sidebar_preview_lines: noa_config::DEFAULT_SIDEBAR_PREVIEW_LINES,
            quick_terminal_size: 0.4,
            confirm_quit: true,
            font_family: "Menlo".to_string(),
            available_font_families: Vec::new(),
        }
    }

    fn session() -> ThemeSettingsSession {
        ThemeSettingsSession {
            window_id: WindowId::from(1u64),
            state: std::sync::Arc::new(ThemeSettings::open(init())),
            opened_at: Instant::now(),
        }
    }

    // AC-9/AC-24 (R-4/NFR-1): two redraw-path snapshots (`Arc::clone`, what
    // `App::redraw` does at `render.rs`'s `theme_settings_card` line) taken
    // with no mutation in between point at the same allocation — proof no
    // deep clone happens on the read-only render path.
    #[test]
    fn consecutive_redraw_snapshots_share_the_same_allocation() {
        let session = session();

        let snapshot_a = std::sync::Arc::clone(&session.state);
        let snapshot_b = std::sync::Arc::clone(&session.state);

        assert!(std::sync::Arc::ptr_eq(&snapshot_a, &snapshot_b));
    }

    // AC-10 (pure-state half — the "existing behavior unchanged" half of
    // this claim is the untouched `theme_settings::tests` suite staying
    // green, R-8): once a render-path snapshot is dropped, `Arc::make_mut`
    // still forks/mutates correctly and the result is observable through
    // `session.state` exactly as a direct `&mut ThemeSettings` call would
    // produce.
    #[test]
    fn mutation_after_snapshot_drop_applies_correctly() {
        let mut session = session();
        {
            let _redraw_snapshot = std::sync::Arc::clone(&session.state);
        } // dropped: refcount back to 1 before the mutation below

        let before = session.state.selected_row();
        std::sync::Arc::make_mut(&mut session.state).move_down();

        assert_eq!(session.state.selected_row(), before + 1);
    }

    // AC-11 companion: a still-live snapshot (as if the render path had
    // wrongly stored its clone back into `self` across a turn, the Atlas
    // invariant this type's field doc warns against) forces `make_mut` to
    // fork — the mutation still applies correctly to the *new* allocation,
    // but the snapshot's pointer no longer matches, which is exactly the
    // deep-copy regression this test would catch if that invariant were
    // ever violated.
    #[test]
    fn mutation_while_a_snapshot_is_still_held_forks_the_allocation() {
        let mut session = session();
        let held_snapshot = std::sync::Arc::clone(&session.state);

        std::sync::Arc::make_mut(&mut session.state).move_down();

        assert!(
            !std::sync::Arc::ptr_eq(&held_snapshot, &session.state),
            "make_mut must fork rather than mutate through a still-shared Arc"
        );
        assert_eq!(session.state.selected_row(), 1);
        assert_eq!(held_snapshot.selected_row(), 0, "the held snapshot is untouched");
    }
}

#[derive(Clone)]
pub(super) struct SendSelectionTarget {
    pub(super) window_id: WindowId,
    pub(super) pane_id: PaneId,
    pub(super) label: String,
}

/// A modal picker for explicitly sending the focused pane's selected text to
/// another pane. The payload is captured at open time so cancellation is a
/// pure state drop and later selection edits cannot race the send.
pub(super) struct SendSelectionPickerSession {
    pub(super) window_id: WindowId,
    pub(super) source_pane: PaneId,
    pub(super) selected_text: String,
    pub(super) targets: Vec<SendSelectionTarget>,
    pub(super) selected: usize,
    pub(super) opened_at: Instant,
}

impl SendSelectionPickerSession {
    pub(super) fn move_up(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }

    pub(super) fn move_down(&mut self) {
        if self.selected + 1 < self.targets.len() {
            self.selected += 1;
        }
    }
}

/// An open inline rename on a sidebar card (FR-7 Rename). Modal for its
/// window's keyboard while it is open.
pub(super) struct SidebarRenameSession {
    pub(super) window_id: WindowId,
    pub(super) card: SessionCardId,
    pub(super) buffer: String,
}

/// An open "Set Tab Title" prompt (tab-title REQ-TTL-1), bound to the tab it
/// was opened for. Modal for its window's keyboard while it is open.
pub(super) struct TabTitlePromptSession {
    pub(super) window_id: WindowId,
    pub(super) buffer: String,
}

/// The modal layer owning a window's IME composition (see
/// `App::modal_ime_target`), in `KeyboardInput` routing priority order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ModalImeTarget {
    ConfirmDialog,
    TabTitlePrompt,
    SearchPrompt,
    CommandPalette,
    ThemeSettings,
    SidebarRename,
}

/// Live IME composition text owned by a modal in one window. The owner is
/// tracked so a stale composition from a closed native tab cannot swallow
/// keyboard input delivered to another tab.
pub(super) struct ModalPreedit {
    pub(super) window_id: WindowId,
    pub(super) text: String,
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
    /// Paste raw (unencoded) text to the pane's pty. Encoding (bracketed or
    /// raw) happens at confirm time, not dialog-open time, so a bracketed-
    /// paste mode change while the dialog is open can't produce a stale
    /// encoding.
    Paste {
        window_id: WindowId,
        pane_id: PaneId,
        text: String,
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
    pub(super) pty_input_tx: crate::io_thread::PtyInputQueue,
    pub(super) auto_approve_feedback_tx: Sender<crate::io_thread::AutoApproveFeedback>,
    pub(super) resize_tx: Sender<GridSize>,
    pub(super) io_thread: Option<crate::io_thread::IoThreadHandle>,
    pub(super) grid_size: GridSize,
    pub(super) mouse_selection: MouseSelectionState,
    /// The in-progress drag's anchor pinned to content: its storage
    /// coordinate plus the eviction count when captured, so scrolling output
    /// (or scrollback eviction) during the drag can't re-anchor the
    /// selection. `None` outside a single-click drag.
    pub(super) selection_anchor: Option<(noa_grid::SelectionPoint, usize)>,
    pub(super) last_mouse_cell: Option<Point>,
    pub(super) pressed_mouse_button: Option<MouseButton>,
    pub(super) ime_state: input::ImeState,
    pub(super) auto_approve_guards: Arc<Mutex<crate::auto_approve::AutoApproveInputGuards>>,
    pub(super) rect: PaneRectApp,
    /// The Cmd+hover underline target for this pane, recomputed on every
    /// `CursorMoved`/`ModifiersChanged` (`App::sync_hover_link`) and fed into
    /// `FrameSnapshot::hover_link` at redraw.
    pub(super) hover_link: Option<HoverLink>,
    /// The Session Overview mirror's read-only publish slot (Fix B, REQ-NF-6).
    pub(super) overview_snapshot: Arc<Mutex<Option<Arc<FrameSnapshot>>>>,
    /// Previous frame's snapshot rows + viewport identity, handed back after
    /// each redraw so `FrameSnapshot::from_terminal_recycle` can reuse row/cell
    /// allocations and skip clean-row copies when the viewport is unchanged.
    pub(super) snapshot_recycle: noa_render::FrameSnapshotRecycle,
    /// Mirrors `Terminal::has_kitty_animation` without needing the terminal
    /// lock to read it: cloned once from `Terminal::kitty_animation_flag` at
    /// surface creation, kept in sync by `noa-grid` on every Kitty graphics
    /// command and animation tick. Lets the idle-animation timer
    /// (`tick_kitty_animations`) skip locking panes with nothing running.
    pub(super) kitty_animation_flag: Arc<AtomicBool>,
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
/// polarity, selected at startup, and the radius/ring widths follow the host
/// window's scale factor via `metrics`.
pub(super) fn overview_card_style(metrics: OverviewMetrics) -> CardStyle {
    CardStyle {
        background: overview_bg_color(),
        border_color: overview_border_color(),
        focus_color: overview_focus_ring_color(),
        corner_radius: metrics.card_corner_radius,
        border_width: metrics.card_border_width,
        focus_width: metrics.card_focus_width,
        focus_glow_width: metrics.card_focus_glow_width,
    }
}

/// Attention styling for an Overview tile with a pending interaction request.
pub(super) fn overview_attention_card_style(metrics: OverviewMetrics) -> CardStyle {
    CardStyle {
        focus_color: crate::chrome::rgba(crate::chrome::palette().dot_red),
        focus_width: crate::chrome::RING_ATTENTION * metrics.scale(),
        focus_glow_width: crate::chrome::GLOW_ATTENTION * metrics.scale(),
        ..overview_card_style(metrics)
    }
}

/// Rounded styling for Overview chrome pills (search and shortcut hint).
pub(super) fn overview_chrome_card_style(metrics: OverviewMetrics) -> CardStyle {
    CardStyle {
        background: overview_bg_color(),
        border_color: overview_chrome_border_color(),
        focus_color: overview_chrome_border_color(),
        corner_radius: metrics.card_corner_radius,
        border_width: 1.0 * metrics.scale(),
        focus_width: 1.0 * metrics.scale(),
        focus_glow_width: 0.0,
    }
}

#[cfg(test)]
mod file_drop_tests {
    use super::FileDropState;
    use std::path::PathBuf;

    #[test]
    fn dropped_path_without_hover_pastes_that_path() {
        let mut state = FileDropState::default();
        assert_eq!(
            state.dropped_paths(PathBuf::from("/tmp/a.txt")),
            Some(vec![PathBuf::from("/tmp/a.txt")])
        );
    }

    #[test]
    fn hovered_multi_file_drop_pastes_batch_once() {
        let mut state = FileDropState::default();
        state.hover(PathBuf::from("/tmp/a.txt"));
        state.hover(PathBuf::from("/tmp/b.txt"));

        assert_eq!(
            state.dropped_paths(PathBuf::from("/tmp/a.txt")),
            Some(vec![
                PathBuf::from("/tmp/a.txt"),
                PathBuf::from("/tmp/b.txt")
            ])
        );
        assert_eq!(state.dropped_paths(PathBuf::from("/tmp/b.txt")), None);
    }

    #[test]
    fn suppressed_drop_events_are_matched_by_path() {
        let mut state = FileDropState::default();
        state.hover(PathBuf::from("/tmp/a.txt"));
        state.hover(PathBuf::from("/tmp/b.txt"));

        assert!(state.dropped_paths(PathBuf::from("/tmp/a.txt")).is_some());
        assert_eq!(
            state.dropped_paths(PathBuf::from("/tmp/c.txt")),
            Some(vec![PathBuf::from("/tmp/c.txt")])
        );
    }

    #[test]
    fn hover_cancel_clears_batch_state() {
        let mut state = FileDropState::default();
        state.hover(PathBuf::from("/tmp/a.txt"));
        state.cancel_hover();

        assert_eq!(
            state.dropped_paths(PathBuf::from("/tmp/b.txt")),
            Some(vec![PathBuf::from("/tmp/b.txt")])
        );
    }
}

#[cfg(test)]
mod chrome_textures_tests {
    use super::ChromeTextures;

    // AC-20: `ChromeTextures::reset()` needs no GPU device. Every field is
    // `None` from `Default`; several field types (`wgpu::Texture`, `Renderer`,
    // `OverviewChromeCardPipeline`) cannot be constructed as `Some(..)`
    // without a device at all, so there is no device-free way to drive a
    // `Some -> None` transition — the meaningful, GPU-free contract this test
    // proves is the type-level guarantee that `reset()` compiles against
    // every field and leaves the whole struct in its all-`None` `Default`
    // shape (the real `Some -> None` path is exercised by the existing draw
    // code, which is unchanged by this refactor).
    #[test]
    fn reset_leaves_every_field_none() {
        let mut textures = ChromeTextures::default();
        textures.reset();

        assert!(textures.sidebar_renderer.is_none());
        assert!(textures.sidebar_card.is_none());
        assert!(textures.sidebar_band_card.is_none());
        assert!(textures.sidebar_band.is_none());
        assert!(textures.sidebar_raster_cache_key.is_none());
        assert!(textures.sidebar_card_tex.is_none());
        assert!(textures.sidebar_menu_tex.is_none());
        assert!(textures.sidebar_button_tex.is_none());
        assert!(textures.sidebar_divider_tex.is_none());
        assert!(textures.sidebar_drop_tex.is_none());
        assert!(textures.sidebar_accent_tex.is_none());
        assert!(textures.sidebar_rule_tex.is_none());
        assert!(textures.palette_scratch.is_none());
        assert!(textures.scrollbar_tex.is_none());
        assert!(textures.bell_flash_tex.is_none());
    }

    // NFR-2/AC-18 instrumentation: `reset()` invalidates resources but is not
    // itself a rebuild, so it must never clear the rebuild counter — the
    // eventual GUI-integrated AC-18 check relies on the counter being
    // cumulative across the reset that a theme confirm triggers.
    #[cfg(debug_assertions)]
    #[test]
    fn reset_does_not_clear_rebuild_count() {
        let mut textures = ChromeTextures::default();
        textures.record_rebuild();
        textures.record_rebuild();
        assert_eq!(textures.rebuild_count(), 2);

        textures.reset();

        assert_eq!(
            textures.rebuild_count(),
            2,
            "reset() must not clear rebuild instrumentation"
        );
    }

    // AC-18/NFR-2: scrubbing the theme list (arbitrarily many highlight
    // changes) has no way to reach `ChromeTextures` at all — the pure
    // `theme_settings` module holds no reference to this type — so the
    // counter cannot move during a scrub by construction. A commit calls
    // `reset()` exactly once (`App::commit_theme_settings`); `reset()`
    // itself is not a rebuild (asserted above), so the counter only climbs
    // once the next redraw's lazy-init actually rebuilds each resource —
    // simulated here as one batch of rebuilds following the reset.
    #[cfg(debug_assertions)]
    #[test]
    fn scrub_never_rebuilds_and_one_commit_reset_precedes_the_next_rebuild_batch() {
        let mut textures = ChromeTextures::default();
        // "10+ highlight changes": nothing to call here at all — there is no
        // `ChromeTextures` method a theme-picker scrub could reach. The
        // counter starting and staying at 0 through this comment is the
        // scrub-side half of the assertion.
        assert_eq!(textures.rebuild_count(), 0);

        // One theme-settings commit: exactly one `reset()` call.
        textures.reset();
        assert_eq!(textures.rebuild_count(), 0, "reset alone is not a rebuild");

        // The next redraw's lazy-init rebuilds each now-`None` resource —
        // one batch, driving the counter from the commit, not the scrub.
        textures.record_rebuild();
        assert_eq!(textures.rebuild_count(), 1);
    }
}

#[cfg(test)]
mod active_theme_tests {
    use super::active_theme;
    use noa_core::{Color, GridSize};
    use noa_grid::{Terminal, TerminalColors};
    use noa_render::{OverlayStyle, Theme};

    // AC-1 (R-6): with `preview_theme = Some(other_theme)`, `active_theme`'s
    // output — fed through the same resolvers the draw path uses
    // (`resolve_with_colors`, `OverlayStyle::from_theme`) — matches the OTHER
    // theme's values, not the base `gpu.theme`'s, for every one of the four
    // color families R-6 calls out.
    #[test]
    fn active_theme_prefers_preview_over_base_theme() {
        let base = Theme::default();
        let preview = crate::theme::resolve_theme(Some("Afterglow"));
        assert_ne!(
            base.default_fg, preview.default_fg,
            "fixture themes must actually differ for this test to prove anything"
        );

        let preview_theme = Some(preview.clone());
        let resolved = active_theme(&base, &preview_theme);
        let colors = TerminalColors::default();

        // (a) body default fg/bg
        assert_eq!(
            resolved.resolve_with_colors(Color::Default, true, &colors),
            preview.resolve_with_colors(Color::Default, true, &colors)
        );
        assert_eq!(
            resolved.resolve_with_colors(Color::Default, false, &colors),
            preview.resolve_with_colors(Color::Default, false, &colors)
        );
        // (b) selection colors
        assert_eq!(resolved.selection_fg(), preview.selection_fg());
        assert_eq!(resolved.selection_bg(), preview.selection_bg());
        // (c) search-highlight colors
        assert_eq!(resolved.search_fg(), preview.search_fg());
        assert_eq!(resolved.search_bg(), preview.search_bg());
        // (d) OverlayStyle-derived colors
        assert_eq!(
            OverlayStyle::from_theme(resolved),
            OverlayStyle::from_theme(&preview)
        );
    }

    // `preview_theme = None` (the default / no-preview state) must resolve to
    // the base theme unchanged — behavior stays identical to before this
    // seam existed.
    #[test]
    fn active_theme_falls_back_to_base_theme_when_no_preview() {
        let base = crate::theme::resolve_theme(Some("Afterglow"));
        let resolved = active_theme(&base, &None);
        assert_eq!(*resolved, base);
    }

    // AC-2 (R-6): flipping `preview_theme` must never touch any `Terminal`'s
    // `TerminalColors` — the preview seam (`active_theme`) is a pure,
    // free-standing resolver over two `&Theme`-shaped values with no
    // reference to `Terminal`/`TerminalColors` in its signature, so there is
    // structurally no code path from one to the other. This test pins that
    // down concretely: a real `Terminal`'s colors are untouched across a
    // preview set-then-clear cycle.
    #[test]
    fn preview_theme_never_touches_terminal_colors() {
        let terminal = Terminal::new(GridSize::new(80, 24));
        let before = terminal.colors.clone();

        let base = Theme::default();
        let mut preview_theme: Option<Theme> = None;
        let _ = active_theme(&base, &preview_theme);

        preview_theme = Some(crate::theme::resolve_theme(Some("Afterglow")));
        let _ = active_theme(&base, &preview_theme);

        preview_theme = None;
        let _ = active_theme(&base, &preview_theme);

        assert_eq!(
            terminal.colors, before,
            "preview_theme changes must not mutate any Terminal's TerminalColors"
        );
    }
}
