//! Pure session-sidebar layout, hit-test, and scroll math (spec
//! `docs/specs/session-sidebar.md`, ADR 0001). Ghostty has no analog; this is a
//! noa addition.
//!
//! Mirrors [`crate::session_overview`]'s conventions: every function here is pure
//! geometry over crate-local [`SidebarRect`]/[`Point`] plus the GUI-agnostic
//! [`SessionCard`] state, so the sidebar's layout can be unit-tested without a
//! window or GPU. The module must not import `winit` or `wgpu` (NFR-6/AC-2,
//! enforced by the source-scan test below) and never locks the terminal
//! (NFR-1/AC-17) — it only reads already-published card state.
//!
//! PR2 wires the module into the crate but nothing consumes it yet; the
//! `dead_code` allow is temporary and removed when the app integrates the
//! sidebar (PR3).
#![allow(dead_code)]

pub use crate::split_tree::{Point, Rect as SidebarRect};

use crate::session_store::{IconKind, SessionCard, SessionCardId, WallClock, format_relative_time};

// All the `SIDEBAR_*`/`CARD_*` metrics below are the design values at scale
// 1.0, tuned for the sidebar's dedicated small font (≈11.5pt), so cards read
// compact and dense (mockup parity). `SidebarMetrics::new(scale)` multiplies
// them by the window DPR.

/// Height of the top header band (status label / center title / name pill,
/// FR-5). Collapsed to 0: the header duplicated the terminal title across its
/// center title and name pill and was removed as redundant, so the toolbar and
/// cards now sit at the sidebar's top. The band model is retained (zero height)
/// so `bands()`/`header_rects()` degrade gracefully rather than needing a
/// structural rewrite.
pub const SIDEBAR_HEADER_H: u32 = 0;

/// Height of the toolbar band holding the `+` (new session) button below the
/// header.
pub const SIDEBAR_TOOLBAR_H: u32 = 30;

/// Default number of last-output preview rows in one session card.
pub const DEFAULT_SIDEBAR_PREVIEW_LINES: usize = noa_config::DEFAULT_SIDEBAR_PREVIEW_LINES;

/// Vertical gap between adjacent cards.
pub const SIDEBAR_CARD_GUTTER: u32 = 8;

/// Horizontal margin between a card and the sidebar's left/right edges, so the
/// rounded card reads as a floating tile instead of touching the band edges
/// (matches `SIDEBAR_CARD_GUTTER` so the gap reads uniform on all sides).
pub const SIDEBAR_CARD_MARGIN_X: u32 = 8;

/// Max characters of a cwd shown on the meta row before middle truncation.
/// The cwd shares the row with the process badge and branch, so it is kept
/// tighter than when it owned a full row.
const CWD_MAX_CHARS: usize = 24;

// Card interior metrics (all compile-time; see `card_rects`/`card_lines`).
const CARD_PAD: u32 = 12;
/// Extra left inset for the terminal-output preview rows, on top of `CARD_PAD`.
/// The preview echoes raw shell/agent output (a prompt, `../noa main`, …) which
/// otherwise starts flush against the card's left pad and reads as cramped; a
/// small indent sets it apart from the dot-anchored name/cwd/meta rows above.
const CARD_PREVIEW_INSET: u32 = 8;
const CARD_ICON_W: u32 = 18;
const CARD_DOT_D: u32 = 8;
const CARD_MENU_W: u32 = 22;
const CARD_LINE_H: u32 = 15;
const CARD_NAME_H: u32 = 18;
/// Width of the right-aligned updated-time region on the name row (fits
/// `昨日 23:47` in the sidebar's small font).
const CARD_UPDATED_W: u32 = 78;

// Card interior row baselines (top-relative): the name row (dot, icon, name,
// right-aligned updated-time, `…`), the meta row (process badge + branch + cwd
// on one line), then a configured number of last-output preview lines. All
// rows are always reserved for the configured card height and left blank when
// a field is absent.
const CARD_NAME_Y: u32 = 10;
const CARD_META_Y: u32 = 30;
/// The preview block starts a full blank cell row below the meta row (the
/// sidebar cell is ~15 logical px, so a 34px offset from `CARD_META_Y`
/// guarantees the gap survives the pixel→cell rounding at any DPR). Without
/// it the preview rows land adjacent to the meta row and stop reading as a
/// separate last-output block. Kept phase-aligned with
/// `CARD_PREVIEW_LINE_STEP` (a multiple of 16) so the preview rows also stay
/// on *contiguous* cell rows across cell heights.
const CARD_PREVIEW_START_Y: u32 = 64;
const CARD_PREVIEW_LINE_STEP: u32 = 16;
const CARD_BOTTOM_PAD: u32 = 18;

fn card_height_for_preview_lines(preview_lines: usize) -> u32 {
    let preview_lines = u32::try_from(preview_lines).unwrap_or(u32::MAX);
    CARD_PREVIEW_START_Y
        .saturating_add(preview_lines.saturating_mul(CARD_PREVIEW_LINE_STEP))
        .saturating_add(CARD_BOTTOM_PAD)
}

/// Height of one session card at the default preview-line count.
pub const SIDEBAR_CARD_H: u32 = CARD_PREVIEW_START_Y
    + DEFAULT_SIDEBAR_PREVIEW_LINES as u32 * CARD_PREVIEW_LINE_STEP
    + CARD_BOTTOM_PAD;

/// Vertical stride from one default-height card's top to the next.
pub const SIDEBAR_CARD_STRIDE: u32 = SIDEBAR_CARD_H + SIDEBAR_CARD_GUTTER;

// Toolbar `+` button metrics (kept as comfortable hit targets). Nudged a little
// larger and toward the bottom-right corner of the toolbar band.
const TOOLBAR_BUTTON_W: u32 = 27;
const TOOLBAR_BUTTON_H: u32 = 22;
/// Right margin from the toolbar's right edge (smaller than `CARD_PAD` so the
/// button sits a touch further right than the card content below).
const TOOLBAR_BUTTON_MARGIN_RIGHT: u32 = 8;
/// Downward nudge (logical px) from the toolbar band's vertical center, so the
/// button rides slightly low rather than dead-center.
const TOOLBAR_BUTTON_OFFSET_Y: u32 = 1;
/// Minimum gap (logical px) kept below the button so its border never kisses the
/// card region that begins at the toolbar band's bottom edge.
const TOOLBAR_BUTTON_GAP_BOTTOM: u32 = 3;

// Header line height for the status / title / pill rects.
const HEADER_LINE_H: u32 = 16;

/// The three horizontal bands the sidebar is split into: a fixed top header, a
/// fixed toolbar (`+` / `…`), and the scrolling card viewport below.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SidebarBands {
    pub header: SidebarRect,
    pub toolbar: SidebarRect,
    pub viewport: SidebarRect,
}

/// Sub-rects of one laid-out session card, in window space, each clipped to the
/// scrolling viewport (a card scrolled partly off-screen yields clipped, and
/// possibly zero-size, sub-rects).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CardRects {
    pub id: SessionCardId,
    pub bounds: SidebarRect,
    pub icon: SidebarRect,
    pub name_line: SidebarRect,
    /// The meta row: running process (branded for known agents), the git
    /// branch, and the dim `~`-abbreviated cwd, on one line.
    pub meta: SidebarRect,
    /// Last-output preview rows (original ANSI colors, dim fallback), sized
    /// from `sidebar-preview-lines`.
    pub preview: Vec<SidebarRect>,
    /// The updated-time region (dim, right-aligned at draw time), on the name
    /// row between the name and the `…` button.
    pub updated: SidebarRect,
    pub dot: SidebarRect,
    pub menu_button: SidebarRect,
}

/// The full pure layout of the sidebar for one frame: header/toolbar rects, the
/// visible card rects (with per-card sub-rects), and the scroll extents.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SidebarLayout {
    pub new_button: SidebarRect,
    pub viewport: SidebarRect,
    pub cards: Vec<CardRects>,
    pub content_h: u32,
}

/// Every layout dimension resolved for one window's scale factor (DPR). The
/// pure `SIDEBAR_*`/`CARD_*` constants are the design metrics at scale 1.0; the
/// sidebar's pixel inset is already scale-multiplied (`sidebar_inset`), so the
/// bands, card heights, and interior offsets must scale by the same factor or a
/// Retina card would be half its intended height and clip its rows. Construct
/// once per frame from `window.scale_factor()` — the same source the inset uses
/// — and drive every geometry method off it. Pure and `Copy`, so layout stays
/// unit-testable at any scale without a window.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SidebarMetrics {
    scale: f32,
    /// Number of last-output preview rows each card reserves.
    pub preview_lines: usize,
    /// Header band height (scaled).
    pub header_h: u32,
    /// Toolbar band height (scaled).
    pub toolbar_h: u32,
    /// One card's height (scaled).
    pub card_h: u32,
    /// Vertical gap between cards (scaled).
    pub card_gutter: u32,
    /// Horizontal card margin from the sidebar edges (scaled).
    pub card_margin_x: u32,
    /// Card-to-card stride (`card_h + card_gutter`).
    pub card_stride: u32,
    /// `…` menu popup width (scaled).
    pub menu_w: u32,
    /// `…` menu item-row height (scaled).
    pub menu_item_h: u32,
}

impl SidebarMetrics {
    /// Resolve the design metrics for `scale` (a non-finite or non-positive
    /// value falls back to 1.0). `card_stride` is derived from the scaled parts
    /// so it always equals `card_h + card_gutter` exactly (the layout and
    /// hit-test both rely on that identity).
    pub fn new(scale: f32) -> Self {
        Self::new_with_preview_lines(scale, DEFAULT_SIDEBAR_PREVIEW_LINES)
    }

    /// Resolve design metrics using an explicit `sidebar-preview-lines` value.
    pub fn new_with_preview_lines(scale: f32, preview_lines: usize) -> Self {
        let scale = if scale.is_finite() && scale > 0.0 {
            scale
        } else {
            1.0
        };
        let s = |v: u32| ((v as f32) * scale).round() as u32;
        let card_h = s(card_height_for_preview_lines(preview_lines));
        let card_gutter = s(SIDEBAR_CARD_GUTTER);
        Self {
            scale,
            preview_lines,
            header_h: s(SIDEBAR_HEADER_H),
            toolbar_h: s(SIDEBAR_TOOLBAR_H),
            card_h,
            card_gutter,
            card_stride: card_h + card_gutter,
            card_margin_x: s(SIDEBAR_CARD_MARGIN_X),
            menu_w: s(SIDEBAR_MENU_W),
            menu_item_h: s(SIDEBAR_MENU_ITEM_H),
        }
    }

    /// Scale a design-space length to physical px for this DPR.
    fn s(&self, v: u32) -> u32 {
        ((v as f32) * self.scale).round() as u32
    }

    /// Carve `bounds` into the header / toolbar / viewport bands. Each band
    /// height clamps so a very short sidebar degrades to a zero-height viewport
    /// instead of underflowing (mirrors `overview_chrome_bands`).
    pub fn bands(&self, bounds: SidebarRect) -> SidebarBands {
        let header_h = self.header_h.min(bounds.h);
        let after_header = bounds.h - header_h;
        let toolbar_h = self.toolbar_h.min(after_header);
        let viewport_h = after_header - toolbar_h;

        SidebarBands {
            header: SidebarRect::new(bounds.x, bounds.y, bounds.w, header_h),
            toolbar: SidebarRect::new(bounds.x, bounds.y + header_h, bounds.w, toolbar_h),
            viewport: SidebarRect::new(
                bounds.x,
                bounds.y + header_h + toolbar_h,
                bounds.w,
                viewport_h,
            ),
        }
    }

    /// The `+` (new session) button rect in the toolbar band. Pinned to the
    /// top-right corner as the sidebar's sole toolbar action (the dead `…`
    /// header menu was removed), vertically centered in the toolbar band.
    pub fn new_button_rect(&self, toolbar: SidebarRect) -> SidebarRect {
        let btn_w = self.s(TOOLBAR_BUTTON_W);
        let btn_h = self.s(TOOLBAR_BUTTON_H);
        let h = btn_h.min(toolbar.h);
        // Centered in the band, then nudged down a little — but always keep a
        // bottom gap so the button's border never touches the card region that
        // starts at the band's bottom edge (clamped for a short toolbar).
        let slack = toolbar.h - h;
        let max_y_off = slack.saturating_sub(self.s(TOOLBAR_BUTTON_GAP_BOTTOM));
        let y = toolbar.y + (slack / 2 + self.s(TOOLBAR_BUTTON_OFFSET_Y)).min(max_y_off);
        let x = toolbar
            .right()
            .saturating_sub(self.s(TOOLBAR_BUTTON_MARGIN_RIGHT))
            .saturating_sub(btn_w);
        SidebarRect::new(x, y, btn_w.min(toolbar.w), h)
    }

    /// Total scrollable content height for `card_count` stacked cards.
    pub fn content_height(&self, card_count: usize) -> u32 {
        (card_count as u32).saturating_mul(self.card_stride)
    }

    /// Lay out the sidebar: header/toolbar chrome plus every card at least
    /// partially visible after applying `scroll_offset` (clamped). Cards fully
    /// scrolled out are omitted; partly-visible cards are clipped to the
    /// viewport.
    pub fn layout(
        &self,
        bounds: SidebarRect,
        ids: &[SessionCardId],
        scroll_offset: u32,
    ) -> SidebarLayout {
        let bands = self.bands(bounds);
        let content_h = self.content_height(ids.len());
        let scroll = clamp_scroll(scroll_offset, content_h, bands.viewport.h);
        let vp = bands.viewport;

        let mut cards = Vec::new();
        for (index, &id) in ids.iter().enumerate() {
            // Window-space top of this card (may fall above/below the viewport).
            let top = vp.y as i64 + (index as i64) * self.card_stride as i64 - scroll as i64;
            let bottom = top + self.card_h as i64;
            // Skip cards entirely outside the viewport.
            if bottom <= vp.y as i64 || top >= vp.bottom() as i64 {
                continue;
            }
            cards.push(self.card_rects(id, top, vp));
        }

        SidebarLayout {
            new_button: self.new_button_rect(bands.toolbar),
            viewport: vp,
            cards,
            content_h,
        }
    }

    /// One card's sub-rects in its own texture space (origin at the card's
    /// top-left, width `inset`), unclipped. The per-card rounded-card renderer
    /// draws each card into its own texture and positions text with these,
    /// matching the window-space rects [`layout`](Self::layout) emits for the
    /// flat backdrop.
    pub fn card_local_rects(&self, id: SessionCardId, card_w: u32) -> CardRects {
        let vp = SidebarRect::new(0, 0, card_w, self.card_h);
        self.card_rects_at(id, 0, card_w as i64, 0, vp)
    }

    /// The width of one card for a sidebar `sidebar_w` wide: the full width
    /// minus the horizontal margin on both sides.
    pub fn card_w(&self, sidebar_w: u32) -> u32 {
        sidebar_w.saturating_sub(2 * self.card_margin_x)
    }

    /// Build one card's clipped sub-rects from its (possibly off-viewport)
    /// window top `top` and the viewport `vp`, inset from the viewport edges by
    /// the horizontal card margin.
    fn card_rects(&self, id: SessionCardId, top: i64, vp: SidebarRect) -> CardRects {
        let margin = self.card_margin_x as i64;
        let lx = vp.x as i64 + margin;
        let w = (vp.w as i64 - 2 * margin).max(0);
        self.card_rects_at(id, lx, w, top, vp)
    }

    /// The interior of one card whose left edge is `lx` and width `w` (window
    /// space), clipped to `vp`. Every interior offset scales with the DPR so
    /// the card's rows (name / cwd / meta / configured preview rows / updated)
    /// fit its scaled height on a Retina display.
    fn card_rects_at(
        &self,
        id: SessionCardId,
        lx: i64,
        w: i64,
        top: i64,
        vp: SidebarRect,
    ) -> CardRects {
        let pad = self.s(CARD_PAD) as i64;
        let dot_d = self.s(CARD_DOT_D) as i64;
        let icon_w = self.s(CARD_ICON_W) as i64;
        let card_menu_w = self.s(CARD_MENU_W) as i64;
        let line_h = self.s(CARD_LINE_H) as i64;
        let name_h = self.s(CARD_NAME_H) as i64;
        let gap6 = self.s(6) as i64;

        // Name row, left to right: status dot, project icon, display name,
        // right-aligned updated-time, `…` hit region. The dot sits at the
        // card's left edge (mockup parity) as a small color chip; the icon and
        // name follow it.
        let dot_x = lx + pad;
        let icon_x = dot_x + dot_d + self.s(8) as i64;
        let name_x = icon_x + icon_w + gap6;
        let menu_x = lx + w - pad - card_menu_w;
        let updated_w = self.s(CARD_UPDATED_W) as i64;
        let updated_x = menu_x - gap6 - updated_w;
        let name_w = updated_x - name_x - gap6;

        let body_w = w - 2 * pad;
        let row = |y: u32| iclip(lx + pad, top + self.s(y) as i64, body_w, line_h, vp);
        // Preview rows carry an extra left inset so raw terminal output sits a
        // touch inboard of the dot-anchored rows above (mockup parity).
        let preview_inset = self.s(CARD_PREVIEW_INSET) as i64;
        let preview_row = |y: u32| {
            iclip(
                lx + pad + preview_inset,
                top + self.s(y) as i64,
                body_w - preview_inset,
                line_h,
                vp,
            )
        };

        let name_y = top + self.s(CARD_NAME_Y) as i64;

        let preview = (0..self.preview_lines)
            .map(|index| {
                preview_row(
                    CARD_PREVIEW_START_Y
                        + u32::try_from(index)
                            .unwrap_or(u32::MAX)
                            .saturating_mul(CARD_PREVIEW_LINE_STEP),
                )
            })
            .collect();

        CardRects {
            id,
            bounds: iclip(lx, top, w, self.card_h as i64, vp),
            icon: iclip(icon_x, name_y, icon_w, name_h, vp),
            name_line: iclip(name_x, name_y, name_w, name_h, vp),
            meta: row(CARD_META_Y),
            preview,
            updated: iclip(updated_x, name_y, updated_w, name_h, vp),
            // The dot rect shares the name row's y (not a centered sub-rect):
            // the draw path converts rect origins to cell rows by rounding, so
            // a vertically-centered y could land the dot one cell row below
            // the name it belongs to.
            dot: iclip(dot_x, name_y, dot_d, name_h, vp),
            menu_button: iclip(menu_x, name_y, card_menu_w, name_h, vp),
        }
    }

    /// Resolve a click at `point` against the sidebar (FR-3/AC-4). Checks the
    /// toolbar buttons first, then the scrolling card region: within a card, the
    /// per-card `…` button wins over the card body (callers need not fall back
    /// like the overview's separate close hit-test). Returns `None` for the
    /// header, gutters between cards, or any point outside the sidebar.
    pub fn hit_test(
        &self,
        bounds: SidebarRect,
        ids: &[SessionCardId],
        scroll_offset: u32,
        point: Point,
    ) -> Option<SidebarHit> {
        let bands = self.bands(bounds);

        if self.new_button_rect(bands.toolbar).contains(point) {
            return Some(SidebarHit::NewSession);
        }

        let vp = bands.viewport;
        if !vp.contains(point) {
            return None;
        }

        let content_h = self.content_height(ids.len());
        let scroll = clamp_scroll(scroll_offset, content_h, vp.h);

        // Translate the point into scroll-content space (origin at the viewport
        // top-left, offset by the scroll). Both terms are non-negative because
        // the point is inside the viewport.
        let cx = point.x - vp.x;
        let cy = (point.y - vp.y) + scroll;

        let index = (cy / self.card_stride) as usize;
        if index >= ids.len() || cy % self.card_stride >= self.card_h {
            // Past the last card, or in the gutter between two cards.
            return None;
        }
        let id = ids[index];

        // Per-card `…` button, in content space (same math as `card_rects`).
        let card_top = index as u32 * self.card_stride;
        let menu_x =
            vp.w.saturating_sub(self.card_margin_x)
                .saturating_sub(self.s(CARD_PAD))
                .saturating_sub(self.s(CARD_MENU_W));
        let menu = SidebarRect::new(
            menu_x,
            card_top + self.s(6),
            self.s(CARD_MENU_W),
            self.s(CARD_NAME_H),
        );
        if menu.contains(Point::new(cx, cy)) {
            return Some(SidebarHit::CardMenu(id));
        }
        Some(SidebarHit::Card(id))
    }

    /// The insertion index a drag-reorder drop at `pointer_y` (window px) maps
    /// to, in `[0, card_count]`: 0 drops before the first card, `card_count`
    /// past the last. The pointer is clamped into the viewport and translated to
    /// scroll-content space, then snapped to the nearest card *gap* (half-stride
    /// rounding) so a drop lands where the drop indicator is drawn. The caller
    /// turns the index into a neighbor id for
    /// [`SessionStore::move_card_before`](crate::session_store::SessionStore::move_card_before).
    pub fn drop_index(
        &self,
        viewport: SidebarRect,
        card_count: usize,
        scroll_offset: u32,
        pointer_y: u32,
    ) -> usize {
        if self.card_stride == 0 {
            return 0;
        }
        let content_h = self.content_height(card_count);
        let scroll = clamp_scroll(scroll_offset, content_h, viewport.h);
        let py = pointer_y.clamp(viewport.y, viewport.bottom());
        let cy = (py - viewport.y) as u64 + scroll as u64;
        // Round to the nearest gap: the gap before card `i` sits at `i * stride`.
        let idx = ((cy + self.card_stride as u64 / 2) / self.card_stride as u64) as usize;
        idx.min(card_count)
    }

    /// The y of the drop-indicator line (window px) for insertion `index`, or
    /// `None` when it would fall outside the viewport. Mirrors
    /// [`drop_index`](Self::drop_index)'s gap positions: index `i` sits at the
    /// top of card `i` (`i * stride`), clamped to the viewport edges.
    pub fn drop_indicator_y(
        &self,
        viewport: SidebarRect,
        card_count: usize,
        scroll_offset: u32,
        index: usize,
    ) -> Option<u32> {
        let content_h = self.content_height(card_count);
        let scroll = clamp_scroll(scroll_offset, content_h, viewport.h);
        let top = index as i64 * self.card_stride as i64 - scroll as i64 + viewport.y as i64;
        // Clamp to the viewport so a drop at the very top/bottom still shows a
        // line flush with the edge.
        let clamped = top.clamp(viewport.y as i64, viewport.bottom() as i64);
        Some(clamped as u32)
    }

    /// The popup rect for a card's `…` menu, anchored just below its menu button
    /// (`anchor` = the card's `menu_button` rect) and right-aligned to it,
    /// clamped so it never spills past the sidebar's right edge (`sidebar_w`).
    pub fn card_menu_popup_rect(
        &self,
        anchor: SidebarRect,
        item_count: usize,
        sidebar_w: u32,
    ) -> SidebarRect {
        let w = self.menu_w.min(sidebar_w);
        let h = self.menu_item_h.saturating_mul(item_count as u32);
        let x = anchor
            .right()
            .saturating_sub(w)
            .min(sidebar_w.saturating_sub(w));
        SidebarRect::new(x, anchor.bottom(), w, h)
    }

    /// The rect of item `index` within a popup laid out by
    /// [`card_menu_popup_rect`](Self::card_menu_popup_rect).
    pub fn card_menu_item_rect(&self, popup: SidebarRect, index: usize) -> SidebarRect {
        SidebarRect::new(
            popup.x,
            popup.y + self.menu_item_h * index as u32,
            popup.w,
            self.menu_item_h,
        )
    }

    /// Resolve a click at `point` against an open card menu popup, returning the
    /// item hit (or `None` for a click outside the popup — a dismiss).
    pub fn card_menu_hit_test(&self, popup: SidebarRect, point: Point) -> Option<CardMenuItem> {
        if !popup.contains(point) {
            return None;
        }
        let index = ((point.y - popup.y) / self.menu_item_h) as usize;
        CARD_MENU_ITEMS.get(index).copied()
    }
}

/// Clamp a scroll offset to `[0, max]`, where `max = content_h - viewport_h`
/// (0 when the content fits, FR-15/AC-23). The `0` floor is implicit in `u32`.
pub fn clamp_scroll(offset: u32, content_h: u32, viewport_h: u32) -> u32 {
    let max = content_h.saturating_sub(viewport_h);
    offset.min(max)
}

/// Intersect an integer-space rect with `clip`, returning a `u32` rect. A rect
/// with no overlap collapses to zero size at the clamped origin.
fn iclip(x: i64, y: i64, w: i64, h: i64, clip: SidebarRect) -> SidebarRect {
    let x0 = x.max(clip.x as i64);
    let y0 = y.max(clip.y as i64);
    let x1 = (x + w.max(0)).min(clip.right() as i64);
    let y1 = (y + h.max(0)).min(clip.bottom() as i64);
    if x1 <= x0 || y1 <= y0 {
        SidebarRect::new(x0.max(0) as u32, y0.max(0) as u32, 0, 0)
    } else {
        SidebarRect::new(x0 as u32, y0 as u32, (x1 - x0) as u32, (y1 - y0) as u32)
    }
}

/// Width of the per-card `…` menu popup at scale 1.0 (FR-7).
pub const SIDEBAR_MENU_W: u32 = 128;

/// Height of one popup menu item row at scale 1.0.
pub const SIDEBAR_MENU_ITEM_H: u32 = 24;

/// One action in a card's `…` menu (FR-7). `Rename` opens the inline
/// name-editing session on the card (the store-level override,
/// [`crate::session_store::SessionDelta::Rename`], has existed since v1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CardMenuItem {
    Close,
    Rename,
}

/// The card `…` menu's items, in top-to-bottom order (FR-7).
pub const CARD_MENU_ITEMS: [CardMenuItem; 2] = [CardMenuItem::Close, CardMenuItem::Rename];

/// The label rendered for a popup menu item.
pub fn card_menu_label(item: CardMenuItem) -> &'static str {
    match item {
        CardMenuItem::Close => "Close",
        CardMenuItem::Rename => "Rename",
    }
}

/// What a click on the sidebar resolves to (FR-3/FR-6/FR-7). A miss returns
/// `None` from [`SidebarMetrics::hit_test`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SidebarHit {
    /// A card body: switch focus to this session (FR-3).
    Card(SessionCardId),
    /// A card's `…` button: open its close/rename menu (FR-7).
    CardMenu(SessionCardId),
    /// The toolbar `+` button: open a new session (FR-6).
    NewSession,
}

/// The text lines rendered on a card (FR-2/AC-3). `now` is a parameter (no
/// `Instant::now()`) so the relative-time line is pure and testable.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CardLines {
    /// `<icon glyph> <display name>` — the per-card rename override shadows
    /// everything; a manual tab title shadows the shell-driven name. A
    /// shell-driven name is normalized (prompt-like titles fall back to the
    /// cwd's tail segment; a ` | <branch>` suffix duplicating the meta row's
    /// branch is stripped).
    pub name: String,
    /// The cwd shown dim at the end of the meta row: `~`-abbreviated and
    /// middle-truncated so both the path root and the most-specific tail
    /// segment stay visible.
    pub cwd: String,
    /// The branch shown on the meta row after the process; empty when the
    /// session has no branch.
    pub branch: String,
    /// The process shown on the meta row: the tty's foreground process name, or
    /// a shell state fallback (`running` / `idle`) where detection is
    /// unavailable. The caller styles it by the card's `busy` flag.
    pub process: String,
    /// Relative updated-time (`3分前` / `昨日 23:47` / …). Empty for a busy
    /// card: "updated just now" is a tautology while output is flowing, so
    /// only idle cards carry the age.
    pub updated: String,
}

/// Build a card's display lines from its state and the current wall clock.
/// `home` (the viewer's home directory, if known) drives the cwd's `~`
/// abbreviation; a parameter — like `now` — so the formatter stays pure.
/// `tab_title` carries the card's tab's manual title override (tab-title
/// REQ-TTL-11), if any — also a parameter so the formatter stays store-pure.
pub fn card_lines(
    card: &SessionCard,
    now: WallClock,
    home: Option<&str>,
    tab_title: Option<&str>,
) -> CardLines {
    // The project icon is rendered from its own rect (`CardRects::icon`), so the
    // display name here carries no glyph prefix. Name precedence: an explicit
    // per-card rename (FR-7) wins, then the tab's manual title, then the
    // shell-driven session name. Only the shell-driven fallback is normalized —
    // a name the user typed is shown verbatim.
    let name = match (&card.name_override, tab_title) {
        (Some(_), _) => card.display_name().to_string(),
        (None, Some(title)) => title.to_string(),
        (None, None) => normalize_shell_name(&card.name, &card.cwd, card.branch.as_deref()),
    };
    let cwd = format_cwd(&card.cwd, home, CWD_MAX_CHARS);
    let branch = card.branch.clone().unwrap_or_default();
    // The detected foreground process name; when unavailable (non-macOS, or not
    // yet polled) fall back to the shell state so the row is never blank.
    let process = card.process.clone().unwrap_or_else(|| {
        if card.busy {
            "running".to_string()
        } else {
            "idle".to_string()
        }
    });
    // A busy card's age is always "now"; showing it is noise. Idle cards keep
    // the relative time (how long since this session last did anything).
    let updated = if card.busy {
        String::new()
    } else {
        format_relative_time(now, card.updated_at)
    };

    CardLines {
        name,
        cwd,
        branch,
        process,
        updated,
    }
}

/// Normalize a shell-driven session name for the card's title row.
///
/// - A shell-default prompt title (`user@host:~/path`, zsh/bash `%n@%m:%~`)
///   carries no identity beyond the cwd already on the card, and truncates
///   ugly; it falls back to the cwd's tail segment (usually the repo name).
/// - A ` | <branch>` suffix that duplicates the card's own branch (some
///   prompts title `repo | branch`) is stripped — the branch already shows on
///   the meta row.
fn normalize_shell_name(name: &str, cwd: &str, branch: Option<&str>) -> String {
    if is_prompt_like_name(name)
        && let Some(tail) = cwd_tail(cwd)
    {
        return tail.to_string();
    }
    if let Some(branch) = branch.filter(|branch| !branch.is_empty())
        && let Some(stripped) = name.strip_suffix(&format!(" | {branch}"))
        && !stripped.trim().is_empty()
    {
        return stripped.trim_end().to_string();
    }
    name.to_string()
}

/// Whether a session name looks like a shell's default prompt title:
/// `user@host:` followed by a path (`~` or `/`).
fn is_prompt_like_name(name: &str) -> bool {
    let Some((user_host, path)) = name.split_once(':') else {
        return false;
    };
    user_host.contains('@')
        && !user_host.contains(' ')
        && (path.starts_with('~') || path.starts_with('/'))
}

/// The last non-empty path segment of `cwd`, if any.
fn cwd_tail(cwd: &str) -> Option<&str> {
    cwd.rsplit('/').find(|segment| !segment.is_empty())
}

/// A recognized AI coding agent, inferred from the tty's foreground process
/// name so the sidebar can brand it distinctly (FR — agent branding).
/// `Generic` is every other process (its raw name is shown as-is).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentKind {
    ClaudeCode,
    Codex,
    Agy,
    Generic,
}

/// Classify a foreground process name into a known AI agent. Case-insensitive,
/// matched on the executable basename's leading stem (so a target-triple suffix
/// on a distribution binary — `codex-aarch64-apple-darwin` — still classifies).
///
/// Note: `proc_name` can report a wrapper (e.g. `node`) rather than the agent
/// for some installs, so an agent launched through a wrapper is classified
/// `Generic` — we match direct basenames only, an accepted limitation.
pub fn classify_agent(process: &str) -> AgentKind {
    let base = process
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(process)
        .trim()
        .to_ascii_lowercase();
    // Claude Code's native installer names the versioned executable after its
    // bare version (`…/claude/versions/2.1.203`), so the tty's accounting name
    // is `2.1.203` — no agent stem at all. A pure dotted-version name is
    // claimed by Claude Code, the only known agent shipping that layout.
    if is_bare_version(&base) {
        return AgentKind::ClaudeCode;
    }
    // Distribution binaries carry a target-triple suffix (Homebrew's Codex cask
    // installs `codex-aarch64-apple-darwin` and symlinks `codex` to it, so the
    // tty's foreground name reports the real basename, not `codex`). Match on the
    // leading stem before the first `-`/`.` so the suffix doesn't defeat branding.
    let stem = base.split(['-', '.']).next().unwrap_or(&base);
    match stem {
        "claude" => AgentKind::ClaudeCode,
        "codex" => AgentKind::Codex,
        "agy" | "gemini" => AgentKind::Agy,
        _ => AgentKind::Generic,
    }
}

/// Whether `name` is a bare dotted version number (`2.1.203`): only ASCII
/// digits and dots, at least one dot, with no empty component.
fn is_bare_version(name: &str) -> bool {
    name.contains('.')
        && name
            .split('.')
            .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
}

/// Whether a session's bell should escalate to an attention request (FR-A3):
/// true only when the foreground process classifies as a known coding agent.
/// A generic process — or an unresolved one (`None`, e.g. non-macOS or not yet
/// polled) — stays a plain bell, so a generic beep is never mistaken for an
/// interaction request (NFR-A4).
pub fn bell_escalates_to_attention(process: Option<&str>) -> bool {
    process.is_some_and(|p| classify_agent(p) != AgentKind::Generic)
}

/// The branded display name for a classified agent, or the raw process name for
/// a generic process.
pub fn agent_display_name(kind: AgentKind, process: &str) -> &str {
    match kind {
        AgentKind::ClaudeCode => "Claude Code",
        AgentKind::Codex => "Codex",
        AgentKind::Agy => "agy",
        AgentKind::Generic => process,
    }
}

/// Whether an attention marker is in its **visible** phase (FR-A1). `elapsed`
/// is the time since the attention onset; the marker blinks at a 50% duty cycle
/// with a half-period of `interval` for the first `duration`, then settles to a
/// steady on. Pure and unit-testable — the caller supplies the wall/monotonic
/// elapsed so no clock is read here (mirrors the `now`-as-parameter rule).
pub fn attention_blink_on(
    elapsed: std::time::Duration,
    duration: std::time::Duration,
    interval: std::time::Duration,
) -> bool {
    if elapsed >= duration {
        // Settled: steady visible until the window is focused (FR-16 clear).
        return true;
    }
    let interval_ms = interval.as_millis().max(1);
    let ticks = elapsed.as_millis() / interval_ms;
    // Even half-periods are "on", odd are "off", so the marker starts visible.
    ticks.is_multiple_of(2)
}

/// The elapsed time (since onset) of the next [`attention_blink_on`] phase
/// boundary strictly after `elapsed`, clamped to `duration` (the settle
/// instant, so the final partial half-period still gets a wake-up), or `None`
/// once settled. Blink repaints must be scheduled at these boundaries — not at
/// `now + interval` — because the visible phase is computed from the onset: an
/// unaligned deadline paints every flip late and the duty cycle jitters.
pub fn next_attention_blink_boundary(
    elapsed: std::time::Duration,
    duration: std::time::Duration,
    interval: std::time::Duration,
) -> Option<std::time::Duration> {
    if elapsed >= duration {
        return None;
    }
    let interval_ms = interval.as_millis().max(1);
    let next_ms = (elapsed.as_millis() / interval_ms + 1) * interval_ms;
    let boundary = std::time::Duration::from_millis(next_ms.min(u128::from(u64::MAX)) as u64);
    Some(boundary.min(duration))
}

/// Truncate `s` tail-first to at most `max` characters, prefixing an ellipsis
/// so the most-specific (rightmost) path segment stays visible (FR-2).
fn truncate_tail(s: &str, max: usize) -> String {
    let count = s.chars().count();
    if count <= max {
        return s.to_string();
    }
    let keep = max.saturating_sub(1);
    let tail: String = s.chars().skip(count - keep).collect();
    format!("…{tail}")
}

/// Format a cwd for the card's cwd row: abbreviate `home` to `~`, then — if the
/// result still exceeds `max` characters — drop *middle* path segments
/// (`~/repos/…/noa`) so both the root and the identifying tail segments stay
/// visible. A tail-first cut (`…hub.com/example/noa`) would hide the owner of
/// the repository, which is usually the disambiguating part. A single overlong
/// segment falls back to the plain tail-first cut.
pub fn format_cwd(cwd: &str, home: Option<&str>, max: usize) -> String {
    let abbreviated = match home {
        Some(home) if !home.is_empty() && cwd == home => "~".to_string(),
        Some(home)
            if !home.is_empty() && cwd.starts_with(home) && cwd[home.len()..].starts_with('/') =>
        {
            format!("~{}", &cwd[home.len()..])
        }
        _ => cwd.to_string(),
    };
    if abbreviated.chars().count() <= max {
        return abbreviated;
    }

    let segments: Vec<&str> = abbreviated.split('/').collect();
    if segments.len() >= 3 {
        let head = segments[0];
        // Keep the head plus as many tail segments as fit alongside `…`.
        let head_len = head.chars().count();
        let mut kept: Vec<&str> = Vec::new();
        let mut used = head_len + 2; // head + "/…"
        for segment in segments.iter().rev() {
            let extra = segment.chars().count() + 1; // "/segment"
            if used + extra > max || kept.len() + 2 >= segments.len() {
                break;
            }
            kept.push(segment);
            used += extra;
        }
        if !kept.is_empty() {
            kept.reverse();
            return format!("{head}/…/{}", kept.join("/"));
        }
    }
    truncate_tail(&abbreviated, max)
}

/// Project-icon glyph for a card (FR-9). Nerd Font glyphs (mockup parity); an
/// environment without a Nerd Font falls back to tofu / a replacement box for
/// these private-use codepoints (AC-21 [manual] — graceful degradation, NFR-5),
/// which is why the branch of the app that renders these never depends on the
/// glyph being present. The codepoints are Nerd Font private-use area
/// (`nf-seti-*` / `nf-dev-*`).
pub fn icon_glyph(icon: IconKind) -> &'static str {
    match icon {
        IconKind::Rust => "\u{e7a8}",      // nf-dev-rust
        IconKind::Node => "\u{e718}",      // nf-dev-nodejs_small
        IconKind::Terraform => "\u{e69a}", // nf-seti-terraform
        IconKind::Go => "\u{e627}",        // nf-seti-go
        IconKind::Python => "\u{e606}",    // nf-seti-python
        IconKind::Git => "\u{e702}",       // nf-dev-git
        IconKind::Folder => "\u{e5ff}",    // nf-custom-folder
    }
}

/// Whether a window should host a sidebar (FR-14/AC-16b): quick-terminal
/// windows are excluded, so they never get a card in the store nor a width
/// inset. Kept as a predicate so the exclusion rule lives in one place.
pub fn is_sidebar_eligible(is_quick_terminal: bool) -> bool {
    !is_quick_terminal
}

/// The horizontal inset (points) the sidebar takes from a window's pane area
/// (FR-4/FR-14). Zero unless the sidebar is both visible and the window is
/// eligible — so a quick-terminal window (never eligible) always insets 0
/// regardless of visibility (AC-16a). PR3 applies this at the resize call site
/// only; `pane_bounds_for_size` itself stays untouched (Omen P1).
pub fn sidebar_inset(visible: bool, eligible: bool, width_px: f32) -> f32 {
    if visible && eligible { width_px } else { 0.0 }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session_store::{
        PreviewLine, PreviewSpan, SessionDelta, SessionStore, SessionWindowId,
    };
    use crate::split_tree::PaneId;
    use noa_core::Color;

    /// Layout metrics at scale 1.0 — the design-space basis the constants
    /// encode, so existing assertions keep their literal expectations.
    fn m1() -> SidebarMetrics {
        SidebarMetrics::new(1.0)
    }

    fn plain_preview(lines: &[&str]) -> Vec<PreviewLine> {
        lines
            .iter()
            .map(|text| {
                vec![PreviewSpan {
                    text: (*text).to_string(),
                    fg: Color::Default,
                }]
            })
            .collect()
    }

    fn card_id(window: u64, pane: u64) -> SessionCardId {
        SessionCardId::new(SessionWindowId(window), PaneId::new(pane))
    }

    fn wall(hour: u32, minute: u32) -> WallClock {
        WallClock {
            year: 2026,
            month: 7,
            day: 5,
            hour,
            minute,
        }
    }

    // Build a `SessionCard` through the store's public delta path (its `seq`
    // field is private, so it can't be constructed directly here). The `Branch`
    // delta sets the icon to Rust and the (optional) branch.
    fn sample_card(name: &str, cwd: &str, branch: Option<&str>) -> SessionCard {
        let mut store = SessionStore::new();
        let id = card_id(1, 1);
        store.apply(SessionDelta::Upsert {
            id,
            seq: 1,
            name: name.to_string(),
            cwd: cwd.to_string(),
            busy: false,
            updated_at: wall(10, 0),
            preview: Some(plain_preview(&["line one", "line two"])),
        });
        store.apply(SessionDelta::Branch {
            id,
            branch: branch.map(str::to_string),
            icon: IconKind::Rust,
        });
        store.apply(SessionDelta::Process {
            id,
            process: Some("cargo".to_string()),
        });
        store.get(&id).unwrap().clone()
    }

    // 6 ids stacked; a viewport tall enough for exactly 3 default-height cards.
    // bounds height = header(0, collapsed) + toolbar(30) + 3 strides.
    fn six_id_bounds() -> (SidebarRect, Vec<SessionCardId>) {
        let ids: Vec<_> = (0..6).map(|p| card_id(1, p)).collect();
        (
            SidebarRect::new(
                0,
                0,
                360,
                SIDEBAR_HEADER_H + SIDEBAR_TOOLBAR_H + 3 * SIDEBAR_CARD_STRIDE,
            ),
            ids,
        )
    }

    // AC-3 (FR-2): the card lines carry `[icon] name`, `cwd … branch`, and the
    // relative updated-time.
    #[test]
    fn card_lines_include_icon_name_cwd_branch_and_time() {
        let card = sample_card(
            "build",
            "/Users/dev/repos/github.com/example/very-long-project",
            Some("main"),
        );
        let lines = card_lines(&card, wall(10, 3), None, None);

        // The name line is the display name (the icon renders from its own
        // rect, so no glyph prefix here); the icon glyph stays resolvable.
        assert_eq!(lines.name, "build");
        assert!(!icon_glyph(IconKind::Rust).is_empty());

        // The cwd is middle-truncated (ellipsis) keeping the identifying tail
        // segment; the branch rides the meta row alongside it.
        assert!(lines.cwd.contains('…'));
        assert!(lines.cwd.ends_with("very-long-project"));
        assert!(lines.cwd.chars().count() <= CWD_MAX_CHARS);
        assert_eq!(lines.branch, "main");

        // updated-time matches the pure PR1 formatter.
        assert_eq!(
            lines.updated,
            format_relative_time(wall(10, 3), wall(10, 0))
        );
        assert_eq!(lines.updated, "3分前");

        // The running-process row shows the detected foreground process.
        assert_eq!(lines.process, "cargo");
    }

    // Shell-driven names normalize: a prompt-like default title falls back to
    // the cwd's tail segment, and a ` | <branch>` suffix duplicating the meta
    // row's branch is stripped. User-authored names stay verbatim.
    #[test]
    fn card_lines_normalize_shell_driven_names() {
        // zsh/bash default prompt title → the repo (cwd tail) name.
        let card = sample_card(
            "simota@mac:~/repos/github.com/noa",
            "/Users/dev/repos/github.com/noa",
            Some("main"),
        );
        assert_eq!(card_lines(&card, wall(10, 0), None, None).name, "noa");

        // `repo | branch` title → the branch suffix is stripped (the branch
        // already rides the meta row) …
        let card = sample_card("noa | main", "/repo", Some("main"));
        assert_eq!(card_lines(&card, wall(10, 0), None, None).name, "noa");
        // … but only when it matches the card's own branch.
        let card = sample_card("noa | dev", "/repo", Some("main"));
        assert_eq!(card_lines(&card, wall(10, 0), None, None).name, "noa | dev");

        // A tab title or a rename override is never normalized.
        let mut card = sample_card("user@host:/repo", "/repo", None);
        assert_eq!(
            card_lines(&card, wall(10, 0), None, Some("user@host:/x")).name,
            "user@host:/x"
        );
        card.name_override = Some("web | main".to_string());
        assert_eq!(
            card_lines(&card, wall(10, 0), None, None).name,
            "web | main"
        );
    }

    // Agent branding: known AI agents classify (case-insensitively, on the
    // basename); everything else is generic and keeps its raw name.
    #[test]
    fn classify_agent_maps_known_agents() {
        use AgentKind::*;
        for (input, expect) in [
            ("claude", ClaudeCode),
            // Claude Code's native installer names the versioned executable
            // after its bare version, so the accounting name is the version.
            ("2.1.203", ClaudeCode),
            (
                "/Users/dev/.local/share/claude/versions/2.1.203",
                ClaudeCode,
            ),
            ("2", Generic),
            ("1.2.3a", Generic),
            ("Claude", ClaudeCode),
            ("CLAUDE", ClaudeCode),
            ("/usr/local/bin/claude", ClaudeCode),
            ("codex", Codex),
            ("Codex", Codex),
            // Distribution binaries carry a target-triple suffix; the stem still
            // classifies, independent of arch or version.
            ("codex-aarch64-apple-darwin", Codex),
            ("codex-x86_64-apple-darwin", Codex),
            ("agy", Agy),
            ("gemini", Agy),
            ("Gemini", Agy),
            ("zsh", Generic),
            ("cargo", Generic),
            ("node", Generic),
        ] {
            assert_eq!(classify_agent(input), expect, "input {input:?}");
        }

        // The display name replaces the raw process for known agents.
        assert_eq!(
            agent_display_name(classify_agent("claude"), "claude"),
            "Claude Code"
        );
        assert_eq!(
            agent_display_name(classify_agent("codex"), "codex"),
            "Codex"
        );
        assert_eq!(
            agent_display_name(classify_agent("gemini"), "gemini"),
            "agy"
        );
        assert_eq!(agent_display_name(classify_agent("zsh"), "zsh"), "zsh");
    }

    // AC-A2 (FR-A3/NFR-A4): a known agent's bell escalates to attention; a
    // generic process — or an unresolved one — stays a plain bell.
    #[test]
    fn bell_escalates_only_for_known_agents() {
        assert!(bell_escalates_to_attention(Some("claude")));
        assert!(bell_escalates_to_attention(Some("codex")));
        assert!(bell_escalates_to_attention(Some("agy")));
        assert!(bell_escalates_to_attention(Some("/usr/local/bin/gemini")));
        assert!(!bell_escalates_to_attention(Some("zsh")));
        assert!(!bell_escalates_to_attention(Some("cargo")));
        assert!(!bell_escalates_to_attention(Some("node")));
        assert!(!bell_escalates_to_attention(None));
    }

    #[test]
    fn card_lines_omit_branch_when_absent() {
        let card = sample_card("shell", "/repo", None);
        let lines = card_lines(&card, wall(10, 0), None, None);
        assert_eq!(lines.cwd, "/repo");
        assert_eq!(lines.branch, "");
    }

    // tab-title REQ-TTL-11: a manual tab title shadows the shell-driven name
    // on the card, but an explicit per-card rename (FR-7) still wins.
    #[test]
    fn card_name_precedence_is_rename_then_tab_title_then_shell() {
        let mut card = sample_card("shell", "/repo", None);
        assert_eq!(
            card_lines(&card, wall(10, 0), None, Some("api server")).name,
            "api server"
        );
        card.name_override = Some("my session".to_string());
        assert_eq!(
            card_lines(&card, wall(10, 0), None, Some("api server")).name,
            "my session"
        );
        card.name_override = None;
        assert_eq!(card_lines(&card, wall(10, 0), None, None).name, "shell");
    }

    // The cwd row abbreviates the home directory to `~` and middle-truncates
    // an overlong path, keeping the head and the identifying tail segments.
    #[test]
    fn format_cwd_abbreviates_home_and_middle_truncates() {
        // Home abbreviation, exact and prefix forms.
        assert_eq!(format_cwd("/Users/dev", Some("/Users/dev"), 32), "~");
        assert_eq!(
            format_cwd("/Users/dev/repos/noa", Some("/Users/dev"), 32),
            "~/repos/noa"
        );
        // A sibling like /Users/dev2 must NOT abbreviate.
        assert_eq!(
            format_cwd("/Users/dev2/repos", Some("/Users/dev"), 32),
            "/Users/dev2/repos"
        );

        // Middle truncation keeps `~` and the tail segments.
        let long = "/Users/dev/repos/github.com/example/very-long-project";
        let out = format_cwd(long, Some("/Users/dev"), 32);
        assert!(out.starts_with("~/…/"), "{out}");
        assert!(out.ends_with("very-long-project"), "{out}");
        assert!(out.chars().count() <= 32, "{out}");

        // A single overlong segment falls back to the tail-first cut.
        let blob = "x".repeat(64);
        let out = format_cwd(&blob, None, 16);
        assert!(out.starts_with('…'));
        assert_eq!(out.chars().count(), 16);
    }

    // The process row falls back to a shell state when no process is detected
    // (non-macOS, or not yet polled): `running` when busy, else `idle`.
    #[test]
    fn card_lines_process_falls_back_to_shell_state() {
        let mut store = SessionStore::new();
        let id = card_id(1, 1);
        store.apply(SessionDelta::Upsert {
            id,
            seq: 1,
            name: "shell".to_string(),
            cwd: "/repo".to_string(),
            busy: false,
            updated_at: wall(10, 0),
            preview: None,
        });
        let idle = card_lines(store.get(&id).unwrap(), wall(10, 0), None, None);
        assert_eq!(idle.process, "idle");
        // Idle cards carry the relative age …
        assert!(!idle.updated.is_empty());

        store.apply(SessionDelta::Upsert {
            id,
            seq: 2,
            name: "shell".to_string(),
            cwd: "/repo".to_string(),
            busy: true,
            updated_at: wall(10, 0),
            preview: None,
        });
        let busy = card_lines(store.get(&id).unwrap(), wall(10, 0), None, None);
        assert_eq!(busy.process, "running");
        // … while a busy card's is omitted ("updated just now" is noise).
        assert!(busy.updated.is_empty());
    }

    #[test]
    fn truncate_tail_keeps_the_rightmost_segment() {
        assert_eq!(truncate_tail("/repo", 28), "/repo");
        let long = "/a/very/deeply/nested/path/to/the/project";
        let out = truncate_tail(long, 28);
        assert!(out.starts_with('…'));
        assert_eq!(out.chars().count(), 28);
        assert!(out.ends_with("project"));
    }

    // AC-4 (FR-3): hit-test resolves the card body, the per-card `…` button, the
    // toolbar `+` and `…`, and misses.
    #[test]
    fn hit_test_resolves_cards_buttons_and_misses() {
        let (bounds, ids) = six_id_bounds();
        let bands = m1().bands(bounds);
        let vp = bands.viewport;

        // First card body (name area, left of the `…` button).
        let body = Point::new(vp.x + 100, vp.y + 30);
        assert_eq!(
            m1().hit_test(bounds, &ids, 0, body),
            Some(SidebarHit::Card(ids[0]))
        );

        // First card `…` button (top-right corner of the card).
        let menu_x = vp.w - CARD_PAD - CARD_MENU_W;
        let card_menu = Point::new(vp.x + menu_x + 5, vp.y + 11);
        assert_eq!(
            m1().hit_test(bounds, &ids, 0, card_menu),
            Some(SidebarHit::CardMenu(ids[0]))
        );

        // Toolbar `+` (the sole toolbar action; the dead `…` was removed).
        let plus = m1().new_button_rect(bands.toolbar);
        let plus_pt = Point::new(plus.x + 2, plus.y + 2);
        assert_eq!(
            m1().hit_test(bounds, &ids, 0, plus_pt),
            Some(SidebarHit::NewSession)
        );

        // The toolbar's empty area (left of the pinned button, above the
        // viewport): a miss.
        assert_eq!(
            m1().hit_test(bounds, &ids, 0, Point::new(bounds.x + 100, 10)),
            None
        );
        // Outside the sidebar entirely: a miss.
        assert_eq!(
            m1().hit_test(bounds, &ids, 0, Point::new(10_000, 10_000)),
            None
        );
    }

    // FR-7: the card `…` menu popup lays out below its anchor, resolves an item
    // click, clamps within the sidebar, and treats an outside click as a miss.
    #[test]
    fn card_menu_popup_layout_and_hit_test() {
        let anchor = SidebarRect::new(300, 40, CARD_MENU_W_ANCHOR, 20);
        let popup = m1().card_menu_popup_rect(anchor, CARD_MENU_ITEMS.len(), 360);

        // Sits directly under the anchor and never spills past the sidebar edge.
        assert_eq!(popup.y, anchor.bottom());
        assert!(popup.right() <= 360);
        assert_eq!(popup.h, SIDEBAR_MENU_ITEM_H * CARD_MENU_ITEMS.len() as u32);

        // A click on the first row resolves to Close (the only v1 item).
        let item0 = m1().card_menu_item_rect(popup, 0);
        let hit = m1().card_menu_hit_test(popup, Point::new(item0.x + 4, item0.y + 4));
        assert_eq!(hit, Some(CardMenuItem::Close));
        assert_eq!(card_menu_label(CardMenuItem::Close), "Close");

        // A click outside the popup is a dismiss (miss).
        assert_eq!(
            m1().card_menu_hit_test(popup, Point::new(popup.right() + 5, popup.y + 4)),
            None
        );
    }

    // The anchor's own width (CARD_MENU_W) is private; alias it for the test.
    const CARD_MENU_W_ANCHOR: u32 = CARD_MENU_W;

    // FR-7 clip guard: a `…` anchored near the sidebar's bottom edge produces a
    // popup that spills past the window bottom. The draw path skips rendering it
    // (`popup.bottom() > height`), and `handle_sidebar_press` mirrors that guard
    // so a click in the invisible region can't fire an item.
    #[test]
    fn card_menu_popup_can_overflow_the_sidebar_bottom() {
        let sidebar_h = 380;
        // Anchor flush with the bottom edge (as a bottom-most card's `…` would).
        let anchor = SidebarRect::new(300, sidebar_h, CARD_MENU_W_ANCHOR, CARD_NAME_H);
        let popup = m1().card_menu_popup_rect(anchor, CARD_MENU_ITEMS.len(), 360);
        assert!(
            popup.bottom() > sidebar_h,
            "a bottom-anchored popup overflows the window and is not drawn"
        );
    }

    // Drag-reorder drop math: the pointer snaps to the nearest card gap, and
    // the indicator y tracks that gap. 3 cards, viewport tall enough for all.
    #[test]
    fn drop_index_snaps_to_nearest_gap() {
        let (bounds, ids) = six_id_bounds();
        let vp = m1().bands(bounds).viewport;
        let n = ids.len();

        // Near the top of the first card → insert before it (index 0).
        assert_eq!(m1().drop_index(vp, n, 0, vp.y + 2), 0);
        // Just past the first card's midpoint → gap after it (index 1).
        assert_eq!(m1().drop_index(vp, n, 0, vp.y + SIDEBAR_CARD_H), 1);
        // A drop far below every card clamps to the viewport, so the index is
        // bounded by how many cards fit on screen, never past `n`.
        let idx = m1().drop_index(vp, n, 0, vp.bottom());
        assert!(idx <= n);

        // The indicator sits at the gap's window y: index 1 → one stride down.
        let y0 = m1().drop_indicator_y(vp, n, 0, 0).unwrap();
        let y1 = m1().drop_indicator_y(vp, n, 0, 1).unwrap();
        assert_eq!(y0, vp.y);
        assert_eq!(y1, vp.y + SIDEBAR_CARD_STRIDE);
    }

    // With the list scrolled, the drop index accounts for the scroll offset so a
    // drop maps to the card actually under the cursor.
    #[test]
    fn drop_index_accounts_for_scroll() {
        let (bounds, ids) = six_id_bounds();
        let vp = m1().bands(bounds).viewport;
        let n = ids.len();
        let max = clamp_scroll(u32::MAX, m1().content_height(n), vp.h);
        // Scrolled to the bottom, a drop at the very bottom lands at/after the
        // last card (index near `n`).
        let idx = m1().drop_index(vp, n, max, vp.bottom());
        assert!(idx >= n - 1 && idx <= n);
    }

    #[test]
    fn hit_test_misses_the_gutter_between_cards() {
        let (bounds, ids) = six_id_bounds();
        let vp = m1().bands(bounds).viewport;
        // The gutter sits just below the first card (stride = card + gutter).
        let gutter = Point::new(vp.x + 40, vp.y + SIDEBAR_CARD_H + 3);
        assert_eq!(m1().hit_test(bounds, &ids, 0, gutter), None);
    }

    // AC-23 (FR-15): with more cards than fit, scroll clamps to [0, max] and the
    // first/last cards are each reachable at the extremes.
    #[test]
    fn scroll_clamp_bounds_and_endpoints() {
        let content = m1().content_height(6); // 600
        let viewport_h = 3 * SIDEBAR_CARD_STRIDE; // 300
        let max = content - viewport_h; // 300

        assert_eq!(clamp_scroll(0, content, viewport_h), 0);
        assert_eq!(clamp_scroll(50, content, viewport_h), 50);
        assert_eq!(clamp_scroll(max, content, viewport_h), max);
        assert_eq!(clamp_scroll(10_000, content, viewport_h), max);
        // Content fits within the viewport → no scroll room.
        assert_eq!(clamp_scroll(100, 200, 300), 0);
    }

    #[test]
    fn scroll_endpoints_reach_first_and_last_card() {
        let (bounds, ids) = six_id_bounds();
        let vp = m1().bands(bounds).viewport;
        let content = m1().content_height(ids.len());
        let max = clamp_scroll(u32::MAX, content, vp.h);

        // At the top, the first card is reachable near the viewport top.
        let top_pt = Point::new(vp.x + 60, vp.y + 20);
        assert_eq!(
            m1().hit_test(bounds, &ids, 0, top_pt),
            Some(SidebarHit::Card(ids[0]))
        );

        // At the bottom, the last card is reachable near the viewport bottom.
        let bottom_pt = Point::new(vp.x + 60, vp.bottom() - 20);
        assert_eq!(
            m1().hit_test(bounds, &ids, max, bottom_pt),
            Some(SidebarHit::Card(ids[5]))
        );

        // The last card is NOT reachable while scrolled to the top.
        assert_eq!(
            m1().hit_test(bounds, &ids, 0, bottom_pt),
            Some(SidebarHit::Card(ids[2]))
        );
    }

    // AC-20 (NFR-4, FR-15): the layout stacks cards, skips fully-scrolled-out
    // ones, and keeps every emitted rect inside the viewport.
    #[test]
    fn layout_stacks_visible_cards_within_the_viewport() {
        let (bounds, ids) = six_id_bounds();
        let vp = m1().bands(bounds).viewport;

        let layout = m1().layout(bounds, &ids, 0);
        assert_eq!(layout.content_h, m1().content_height(6));
        assert_eq!(layout.viewport, vp);

        // 3 cards fit; a 4th peeks in only if partially visible. Here the
        // viewport is an exact multiple of the stride, so exactly 3 show.
        assert_eq!(layout.cards.len(), 3);
        assert_eq!(layout.cards[0].id, ids[0]);
        assert_eq!(layout.cards[2].id, ids[2]);

        // Every emitted card (and its body sub-rect) stays within the viewport.
        for card in &layout.cards {
            assert!(card.bounds.x >= vp.x && card.bounds.right() <= vp.right());
            assert!(card.bounds.y >= vp.y && card.bounds.bottom() <= vp.bottom());
        }

        // Cards stack top-to-bottom without vertical overlap.
        for pair in layout.cards.windows(2) {
            assert!(pair[0].bounds.bottom() <= pair[1].bounds.y);
        }
    }

    #[test]
    fn layout_shows_the_tail_cards_when_scrolled_to_max() {
        let (bounds, ids) = six_id_bounds();
        let vp = m1().bands(bounds).viewport;
        let max = clamp_scroll(u32::MAX, m1().content_height(ids.len()), vp.h);

        let layout = m1().layout(bounds, &ids, max);
        let shown: Vec<_> = layout.cards.iter().map(|c| c.id).collect();
        assert_eq!(shown, vec![ids[3], ids[4], ids[5]]);
    }

    #[test]
    fn layout_over_scroll_is_clamped_like_clamp_scroll() {
        let (bounds, ids) = six_id_bounds();
        // An absurd offset resolves to the same layout as the clamped maximum.
        let clamped = m1().layout(bounds, &ids, 1_000_000);
        let vp = m1().bands(bounds).viewport;
        let max = clamp_scroll(u32::MAX, m1().content_height(ids.len()), vp.h);
        assert_eq!(clamped.cards, m1().layout(bounds, &ids, max).cards);
    }

    // FR-2 mockup parity: the name row carries, left to right, the status dot,
    // icon, name, right-aligned updated-time region, and the `…` hit region —
    // all sharing one y — with the meta row and preview rows stacked below.
    #[test]
    fn card_rects_place_dot_left_and_updated_on_the_name_row() {
        let (bounds, ids) = six_id_bounds();
        let layout = m1().layout(bounds, &ids, 0);
        let card = &layout.cards[0];

        // Dot is the leftmost element, ahead of the icon and name, and shares
        // the name row's y so the draw path lands them on one cell row.
        assert!(card.dot.x < card.icon.x);
        assert!(card.icon.x < card.name_line.x);
        assert_eq!(card.dot.y, card.name_line.y);

        // Name, then the updated-time region, then the `…` hit region.
        assert!(card.name_line.right() <= card.updated.x);
        assert!(card.updated.right() <= card.menu_button.x);
        assert_eq!(card.updated.y, card.name_line.y);
        assert!(card.updated.w > 0);

        // Rows stack top to bottom: name, meta (process · branch · cwd), the
        // configured preview rows.
        assert!(card.name_line.y < card.meta.y);
        assert_eq!(card.preview.len(), DEFAULT_SIDEBAR_PREVIEW_LINES);
        let mut previous_y = card.meta.y;
        for preview in &card.preview {
            assert!(previous_y < preview.y);
            previous_y = preview.y;
        }
        assert!(previous_y < card.bounds.bottom());
    }

    // DPR scaling: the metrics double at scale 2.0, and a degenerate scale
    // falls back to 1.0 so a bad `scale_factor` never zeroes the layout.
    #[test]
    fn metrics_scale_by_dpr() {
        let m2 = SidebarMetrics::new(2.0);
        assert_eq!(m2.header_h, 2 * SIDEBAR_HEADER_H);
        assert_eq!(m2.toolbar_h, 2 * SIDEBAR_TOOLBAR_H);
        assert_eq!(m2.card_h, 2 * SIDEBAR_CARD_H);
        assert_eq!(m2.card_gutter, 2 * SIDEBAR_CARD_GUTTER);
        assert_eq!(m2.card_margin_x, 2 * SIDEBAR_CARD_MARGIN_X);
        // Stride stays exactly card + gutter at any scale.
        assert_eq!(m2.card_stride, m2.card_h + m2.card_gutter);
        assert_eq!(m2.card_stride, 2 * SIDEBAR_CARD_STRIDE);
        assert_eq!(m2.menu_item_h, 2 * SIDEBAR_MENU_ITEM_H);

        assert_eq!(SidebarMetrics::new(0.0).card_h, SIDEBAR_CARD_H);
        assert_eq!(SidebarMetrics::new(-2.0).card_h, SIDEBAR_CARD_H);
        assert_eq!(SidebarMetrics::new(f32::NAN).card_h, SIDEBAR_CARD_H);
    }

    #[test]
    fn metrics_size_cards_from_preview_line_count() {
        let m0 = SidebarMetrics::new_with_preview_lines(1.0, 0);
        let m3 = SidebarMetrics::new_with_preview_lines(1.0, 3);
        let m5 = SidebarMetrics::new_with_preview_lines(1.0, 5);

        assert_eq!(m5.card_h, SIDEBAR_CARD_H);
        assert!(m0.card_h < m3.card_h);
        assert_eq!(m5.card_h - m3.card_h, 2 * CARD_PREVIEW_LINE_STEP);

        let id = card_id(1, 1);
        assert_eq!(m0.card_local_rects(id, 320).preview.len(), 0);
        assert_eq!(m3.card_local_rects(id, 320).preview.len(), 3);
        assert_eq!(m5.card_local_rects(id, 320).preview.len(), 5);
    }

    // At scale 2.0 the bands, card height, and every interior row double, so a
    // Retina card keeps all rows (name / meta / preview rows) inside
    // its doubled height instead of clipping — and hit-test maps correctly
    // against the scaled geometry.
    #[test]
    fn layout_and_hit_test_scale_at_dpr_2() {
        let m2 = SidebarMetrics::new(2.0);
        let ids: Vec<_> = (0..6).map(|p| card_id(1, p)).collect();
        // header(0, collapsed) + toolbar(60) + 3*stride(296) = 948, doubled width.
        let bounds = SidebarRect::new(
            0,
            0,
            720,
            2 * (SIDEBAR_HEADER_H + SIDEBAR_TOOLBAR_H + 3 * SIDEBAR_CARD_STRIDE),
        );

        let bands = m2.bands(bounds);
        assert_eq!(bands.header.h, 2 * SIDEBAR_HEADER_H);
        assert_eq!(bands.toolbar.h, 2 * SIDEBAR_TOOLBAR_H);

        let layout = m2.layout(bounds, &ids, 0);
        assert_eq!(layout.cards.len(), 3);

        // The first card is a full doubled height and its rows stack in
        // order, with every preview row fitting inside the card.
        let card = &layout.cards[0];
        assert_eq!(card.bounds.h, m2.card_h);
        assert!(card.dot.x < card.icon.x && card.icon.x < card.name_line.x);
        assert_eq!(card.updated.y, card.name_line.y);
        assert!(card.name_line.y < card.meta.y);
        assert_eq!(card.preview.len(), DEFAULT_SIDEBAR_PREVIEW_LINES);
        let mut previous_y = card.meta.y;
        for preview in &card.preview {
            assert!(previous_y < preview.y);
            previous_y = preview.y;
        }
        assert!(previous_y < card.bounds.bottom());
        assert!(card.updated.h > 0);
        assert!(card.updated.bottom() <= card.bounds.bottom());

        // Hit-test resolves the first card's body and its `…` region against the
        // doubled geometry.
        let vp = bands.viewport;
        let body = Point::new(vp.x + 200, vp.y + 40);
        assert_eq!(
            m2.hit_test(bounds, &ids, 0, body),
            Some(SidebarHit::Card(ids[0]))
        );
        let menu_x = vp.w - m2.card_margin_x - m2.s(CARD_PAD) - m2.s(CARD_MENU_W);
        let card_menu = Point::new(vp.x + menu_x + 5, vp.y + 20);
        assert_eq!(
            m2.hit_test(bounds, &ids, 0, card_menu),
            Some(SidebarHit::CardMenu(ids[0]))
        );

        // The gutter past the doubled card height is a miss.
        let gutter = Point::new(vp.x + 80, vp.y + m2.card_h + 3);
        assert_eq!(m2.hit_test(bounds, &ids, 0, gutter), None);
    }

    #[test]
    fn bands_clamp_without_underflow_in_a_short_sidebar() {
        // The header band is collapsed (0), so a 20px sidebar gives its height to
        // the toolbar and the viewport still collapses to zero.
        let bands = m1().bands(SidebarRect::new(0, 0, 200, 20));
        assert_eq!(bands.header.h, 0);
        assert_eq!(bands.toolbar.h, 20);
        assert_eq!(bands.viewport.h, 0);
    }

    // AC-A1 (FR-A1): the blink is visible at onset, hidden a half-period in,
    // and steady-visible once the blink window elapses.
    #[test]
    fn attention_blink_on_toggles_then_settles() {
        use std::time::Duration;
        let dur = Duration::from_secs(6);
        let iv = Duration::from_millis(333);

        // At onset and just after → visible.
        assert!(attention_blink_on(Duration::ZERO, dur, iv));
        assert!(attention_blink_on(Duration::from_millis(100), dur, iv));
        // One half-period in → hidden.
        assert!(!attention_blink_on(Duration::from_millis(400), dur, iv));
        // Two half-periods in → visible again.
        assert!(attention_blink_on(Duration::from_millis(700), dur, iv));
        // Past the blink window → steady visible regardless of phase.
        assert!(attention_blink_on(dur, dur, iv));
        assert!(attention_blink_on(Duration::from_secs(60), dur, iv));
        // Degenerate zero interval never divides by zero.
        assert!(attention_blink_on(
            Duration::from_millis(100),
            dur,
            Duration::ZERO
        ));
    }

    // FR-A1: the next blink wake-up lands exactly on the onset-relative phase
    // boundary (not `now + interval`), is clamped to the settle instant, and
    // stops once settled.
    #[test]
    fn next_attention_blink_boundary_is_phase_aligned_and_clamped() {
        use std::time::Duration;
        let dur = Duration::from_secs(6);
        let iv = Duration::from_millis(333);
        // Mid-phase: the next boundary is the end of the current half-period.
        assert_eq!(
            next_attention_blink_boundary(Duration::from_millis(100), dur, iv),
            Some(Duration::from_millis(333))
        );
        // Exactly on a boundary: the *next* one, never the same instant.
        assert_eq!(
            next_attention_blink_boundary(Duration::from_millis(333), dur, iv),
            Some(Duration::from_millis(666))
        );
        // The last partial half-period clamps to the settle instant so the
        // final repaint still gets a wake-up.
        assert_eq!(
            next_attention_blink_boundary(Duration::from_millis(5994), dur, iv),
            Some(dur)
        );
        // Settled: no further boundaries.
        assert_eq!(next_attention_blink_boundary(dur, dur, iv), None);
        assert_eq!(
            next_attention_blink_boundary(Duration::from_secs(60), dur, iv),
            None
        );
        // Degenerate zero interval never divides by zero.
        assert_eq!(
            next_attention_blink_boundary(Duration::from_millis(1), dur, Duration::ZERO),
            Some(Duration::from_millis(2))
        );
    }

    // AC-16b (FR-14): the eligibility predicate excludes quick-terminal windows.
    #[test]
    fn quick_terminal_is_not_sidebar_eligible() {
        assert!(!is_sidebar_eligible(true));
        assert!(is_sidebar_eligible(false));
    }

    // AC-16a (FR-14): a quick-terminal window insets 0 regardless of visibility;
    // an eligible visible window insets the full width.
    #[test]
    fn sidebar_inset_is_zero_for_quick_terminal_regardless_of_visible() {
        let width = 360.0;
        // Quick-terminal (never eligible): inset 0 whether visible or not.
        assert_eq!(sidebar_inset(true, is_sidebar_eligible(true), width), 0.0);
        assert_eq!(sidebar_inset(false, is_sidebar_eligible(true), width), 0.0);
        // Eligible window: inset only when visible.
        assert_eq!(
            sidebar_inset(true, is_sidebar_eligible(false), width),
            width
        );
        assert_eq!(sidebar_inset(false, is_sidebar_eligible(false), width), 0.0);
    }

    // AC-2 / AC-22 (NFR-6): this module must stay GUI-agnostic. Assert its
    // source imports no windowing/GPU crate. Needles are assembled at runtime so
    // this test file does not trip its own scan.
    #[test]
    fn sidebar_is_gui_agnostic() {
        let source = include_str!("sidebar.rs");
        for forbidden in [
            ["use ", "winit"].concat(),
            ["use ", "wgpu"].concat(),
            ["winit", "::"].concat(),
            ["wgpu", "::"].concat(),
        ] {
            assert!(
                !source.contains(&forbidden),
                "sidebar.rs must not reference `{forbidden}`"
            );
        }
    }

    // AC-17 (NFR-1): the sidebar layout path never locks the terminal — it reads
    // only already-published card state.
    #[test]
    fn sidebar_never_locks_the_terminal() {
        let source = include_str!("sidebar.rs");
        let needle = ["terminal", ".lock()"].concat();
        assert!(
            !source.contains(&needle),
            "sidebar.rs must not call `{needle}`"
        );
    }
}
