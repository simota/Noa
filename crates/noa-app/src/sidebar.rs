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

pub use crate::split_tree::{Point, Rect as SidebarRect};

use crate::session_store::{
    AgentKind, SessionCard, SessionCardId, WallClock, format_relative_time,
};

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
#[cfg(test)]
pub const DEFAULT_SIDEBAR_PREVIEW_LINES: usize = noa_config::DEFAULT_SIDEBAR_PREVIEW_LINES;

/// Vertical gap between adjacent cards. Zero keeps the sidebar list flat and
/// continuous instead of drawing separated floating tiles.
pub const SIDEBAR_CARD_GUTTER: u32 = 0;

/// Horizontal margin between a card and the sidebar's left/right edges. Zero
/// lets each row span the whole sidebar band for a seamless flat list.
pub const SIDEBAR_CARD_MARGIN_X: u32 = 0;

// Card interior metrics (all compile-time; see `card_rects`/`card_lines`).
const CARD_PAD: u32 = 12;
/// Extra left inset for the terminal-output preview rows, on top of `CARD_PAD`.
/// The preview echoes raw shell/agent output (a prompt, `../noa main`, …) which
/// otherwise starts flush against the card's left pad and reads as cramped; a
/// small indent sets it apart from the aligned name/meta/context rows above.
const CARD_PREVIEW_INSET: u32 = 8;
const CARD_MENU_W: u32 = 22;
const CARD_LINE_H: u32 = 15;
const CARD_NAME_H: u32 = 15;
/// Width of the right-aligned updated-time region on the name row (fits
/// `Yday 23:47`, the widest form `format_relative_time` emits, in the
/// sidebar's small font).
const CARD_UPDATED_W: u32 = 78;

// Card interior row baselines (top-relative): identity and state are always
// present. A selected card adds context and the configured preview rows;
// an unselected actionable report adds just one detail row.
const CARD_NAME_Y: u32 = 12;
const CARD_META_Y: u32 = 28;
const CARD_CONTEXT_Y: u32 = 44;
/// Output follows context on the next row without reserving an empty row.
const CARD_PREVIEW_START_Y: u32 = 60;
const CARD_PREVIEW_LINE_STEP: u32 = 16;
const CARD_BOTTOM_PAD: u32 = 12;

fn card_height_for_preview_lines(preview_lines: usize) -> u32 {
    let preview_lines = u32::try_from(preview_lines).unwrap_or(u32::MAX);
    if preview_lines == 0 {
        return CARD_CONTEXT_Y + CARD_LINE_H + CARD_BOTTOM_PAD;
    }
    CARD_PREVIEW_START_Y
        .saturating_add((preview_lines - 1).saturating_mul(CARD_PREVIEW_LINE_STEP))
        .saturating_add(CARD_LINE_H)
        .saturating_add(CARD_BOTTOM_PAD)
}

/// Height of one session card at the default preview-line count.
#[cfg(test)]
pub const SIDEBAR_CARD_H: u32 = CARD_PREVIEW_START_Y
    + (DEFAULT_SIDEBAR_PREVIEW_LINES as u32 - 1) * CARD_PREVIEW_LINE_STEP
    + CARD_LINE_H
    + CARD_BOTTOM_PAD;

/// Vertical stride from one default-height card's top to the next.
#[cfg(test)]
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

/// The three horizontal bands the sidebar is split into: a fixed top header, a
/// fixed toolbar (`+` / `…`), and the scrolling card viewport below.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SidebarBands {
    pub header: SidebarRect,
    pub toolbar: SidebarRect,
    pub viewport: SidebarRect,
}

/// Detail is determined by selection and an actionable report, never hover.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SidebarCardMode {
    Compact,
    Attention,
    Expanded,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SidebarCardSpec {
    pub id: SessionCardId,
    pub mode: SidebarCardMode,
}

/// Sub-rects of one laid-out session card, in window space, each clipped to the
/// scrolling viewport (a card scrolled partly off-screen yields clipped, and
/// possibly zero-size, sub-rects).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CardRects {
    pub id: SessionCardId,
    pub bounds: SidebarRect,
    /// The unclipped height, which can differ from neighboring cards.
    pub full_h: u32,
    pub expanded: bool,
    /// Pixels clipped from the full card's top. The texture sampler needs this
    /// independently of `bounds.h` when both ends lie outside a short viewport.
    pub clipped_top: u32,
    pub icon: SidebarRect,
    pub name_line: SidebarRect,
    /// The meta row: running process (branded for known agents), agent state,
    /// and the auto-approve indicator.
    pub meta: SidebarRect,
    /// The working-context row: git branch and `~`-abbreviated cwd.
    pub context: SidebarRect,
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
    /// Actual font cell height, when a renderer is available. Font line metrics
    /// differ across platforms, so scaled design-space baselines alone can put
    /// two metadata lines on the same terminal row.
    cell_h: Option<f32>,
    /// Number of last-output preview rows the selected card reserves.
    pub preview_lines: usize,
    /// Header band height (scaled).
    pub header_h: u32,
    /// Toolbar band height (scaled).
    pub toolbar_h: u32,
    /// The maximum (expanded) card height, also the shared texture height.
    pub card_h: u32,
    /// Vertical gap between cards (scaled).
    pub card_gutter: u32,
    /// Horizontal card margin from the sidebar edges (scaled).
    pub card_margin_x: u32,
    /// `…` menu popup width (scaled).
    pub menu_w: u32,
    /// `…` menu item-row height (scaled).
    pub menu_item_h: u32,
}

impl SidebarMetrics {
    /// Resolve the default-preview design metrics for `scale` (a non-finite or
    /// non-positive value falls back to 1.0).
    #[cfg(test)]
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
            cell_h: None,
            preview_lines,
            header_h: s(SIDEBAR_HEADER_H),
            toolbar_h: s(SIDEBAR_TOOLBAR_H),
            card_h,
            card_gutter,
            card_margin_x: s(SIDEBAR_CARD_MARGIN_X),
            menu_w: s(SIDEBAR_MENU_W),
            menu_item_h: s(SIDEBAR_MENU_ITEM_H),
        }
    }

    /// Scale a design-space length to physical px for this DPR.
    fn s(&self, v: u32) -> u32 {
        ((v as f32) * self.scale).round() as u32
    }

    /// Snap card rows to the sidebar font's real cell grid. Layout, texture
    /// sizing and hit-testing share these rows and a one-cell top inset.
    pub fn with_cell_height(mut self, cell_h: f32) -> Self {
        if !cell_h.is_finite() || cell_h <= 0.0 {
            return self;
        }
        self.cell_h = Some(cell_h.max(1.0));
        let last_y = if self.preview_lines == 0 {
            self.card_row_y(2, CARD_CONTEXT_Y)
        } else {
            self.preview_row_y(self.preview_lines - 1)
        };
        self.card_h = last_y
            .saturating_add(self.card_line_h())
            .saturating_add(self.s(CARD_BOTTOM_PAD));
        self
    }

    fn card_row_y(&self, row_offset: u32, design_y: u32) -> u32 {
        match self.cell_h {
            Some(cell_h) => {
                let name_row = (self.s(CARD_NAME_Y) as f32 / cell_h).round().max(1.0);
                ((name_row + row_offset as f32) * cell_h).round() as u32
            }
            None => self.s(design_y),
        }
    }

    fn preview_row_y(&self, index: usize) -> u32 {
        let index = u32::try_from(index).unwrap_or(u32::MAX);
        self.card_row_y(
            index.saturating_add(3),
            CARD_PREVIEW_START_Y.saturating_add(index.saturating_mul(CARD_PREVIEW_LINE_STEP)),
        )
    }

    fn card_line_h(&self) -> u32 {
        self.cell_h
            .map_or_else(|| self.s(CARD_LINE_H), |cell_h| cell_h.ceil() as u32)
    }

    fn card_name_h(&self) -> u32 {
        self.cell_h
            .map_or_else(|| self.s(CARD_NAME_H), |cell_h| cell_h.ceil() as u32)
    }

    fn card_height(&self, mode: SidebarCardMode) -> u32 {
        let last_y = match mode {
            SidebarCardMode::Compact => self.card_row_y(1, CARD_META_Y),
            SidebarCardMode::Attention if self.preview_lines > 0 => {
                self.card_row_y(2, CARD_CONTEXT_Y)
            }
            SidebarCardMode::Attention => self.card_row_y(1, CARD_META_Y),
            SidebarCardMode::Expanded => return self.card_h,
        };
        last_y
            .saturating_add(self.card_line_h())
            .saturating_add(self.s(CARD_BOTTOM_PAD))
    }

    /// A stable wheel step independent of which card is expanded.
    pub fn compact_card_height(&self) -> u32 {
        self.card_height(SidebarCardMode::Compact)
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

    /// All consumers use the same per-card heights, including offscreen cards.
    pub fn content_height_cards(&self, cards: &[SidebarCardSpec]) -> u32 {
        cards.iter().fold(0u32, |height, card| {
            height.saturating_add(self.card_height(card.mode).saturating_add(self.card_gutter))
        })
    }

    /// Lay out the sidebar: header/toolbar chrome plus every card at least
    /// partially visible after applying `scroll_offset` (clamped). Cards fully
    /// scrolled out are omitted; partly-visible cards are clipped to the
    /// viewport.
    pub fn layout_cards(
        &self,
        bounds: SidebarRect,
        specs: &[SidebarCardSpec],
        scroll_offset: u32,
    ) -> SidebarLayout {
        let bands = self.bands(bounds);
        let content_h = self.content_height_cards(specs);
        let scroll = clamp_scroll(scroll_offset, content_h, bands.viewport.h);
        let vp = bands.viewport;

        let mut cards = Vec::new();
        let mut offset = 0u32;
        for &spec in specs {
            // Window-space top of this card (may fall above/below the viewport).
            let top = vp.y as i64 + offset as i64 - scroll as i64;
            let card_h = self.card_height(spec.mode);
            let bottom = top + card_h as i64;
            offset = offset.saturating_add(card_h.saturating_add(self.card_gutter));
            // Skip cards entirely outside the viewport.
            if bottom <= vp.y as i64 || top >= vp.bottom() as i64 {
                continue;
            }
            cards.push(self.card_rects(spec, top, vp));
        }

        SidebarLayout {
            new_button: self.new_button_rect(bands.toolbar),
            viewport: vp,
            cards,
            content_h,
        }
    }

    /// One card's sub-rects in its own texture space (origin at the card's
    /// top-left, width `card_w`), unclipped. The per-card overlay renderer draws
    /// each card into its own texture and positions text with these. The
    /// window-space rects from [`layout_cards`](Self::layout_cards) determine its visible
    /// slice and interactive regions.
    pub fn card_local_rects_for(&self, spec: SidebarCardSpec, card_w: u32) -> CardRects {
        let vp = SidebarRect::new(0, 0, card_w, self.card_height(spec.mode));
        self.card_rects_at(spec, 0, card_w as i64, 0, vp)
    }

    /// The width of one card for a sidebar `sidebar_w` wide: the full width
    /// minus the horizontal margin on both sides. The default margin is zero so
    /// the rows form a seamless flat list.
    pub fn card_w(&self, sidebar_w: u32) -> u32 {
        sidebar_w.saturating_sub(2 * self.card_margin_x)
    }

    /// Build one card's clipped sub-rects from its (possibly off-viewport)
    /// window top `top` and the viewport `vp`, inset from the viewport edges by
    /// the horizontal card margin.
    fn card_rects(&self, spec: SidebarCardSpec, top: i64, vp: SidebarRect) -> CardRects {
        let margin = self.card_margin_x as i64;
        let lx = vp.x as i64 + margin;
        let w = (vp.w as i64 - 2 * margin).max(0);
        self.card_rects_at(spec, lx, w, top, vp)
    }

    /// The interior of one card whose left edge is `lx` and width `w` (window
    /// space), clipped to `vp`. Every interior offset scales with the DPR so
    /// the card's rows (name / meta / context / configured preview rows / updated)
    /// fit its scaled height on a Retina display.
    fn card_rects_at(
        &self,
        spec: SidebarCardSpec,
        lx: i64,
        w: i64,
        top: i64,
        vp: SidebarRect,
    ) -> CardRects {
        let pad = self.s(CARD_PAD) as i64;
        let card_menu_w = self.s(CARD_MENU_W) as i64;
        let line_h = self.card_line_h() as i64;
        let name_h = self.card_name_h() as i64;
        let gap6 = self.s(6) as i64;

        // Identity owns the left edge. Status is conveyed on the metadata row,
        // so it no longer takes a second icon column away from the name.
        let name_x = lx + pad;
        let menu_x = lx + w - pad - card_menu_w;
        let updated_w = self.s(CARD_UPDATED_W) as i64;
        let updated_x = menu_x - self.s(12) as i64 - updated_w;
        let name_w = updated_x - name_x - gap6;

        let body_w = w - 2 * pad;
        let row = |y: u32| iclip(lx + pad, top + y as i64, body_w, line_h, vp);
        // Preview rows carry an extra left inset so raw terminal output sits a
        // touch inboard of the aligned metadata rows above.
        let preview_inset = self.s(CARD_PREVIEW_INSET) as i64;
        let preview_row = |y: u32| {
            iclip(
                lx + pad + preview_inset,
                top + y as i64,
                body_w - preview_inset,
                line_h,
                vp,
            )
        };

        let name_y = top + self.card_row_y(0, CARD_NAME_Y) as i64;

        let preview = match spec.mode {
            SidebarCardMode::Compact => Vec::new(),
            SidebarCardMode::Attention if self.preview_lines > 0 => {
                vec![row(self.card_row_y(2, CARD_CONTEXT_Y))]
            }
            SidebarCardMode::Attention => Vec::new(),
            SidebarCardMode::Expanded => (0..self.preview_lines)
                .map(|index| preview_row(self.preview_row_y(index)))
                .collect(),
        };
        let full_h = self.card_height(spec.mode);
        let empty = SidebarRect::new(name_x.max(0) as u32, name_y.max(0) as u32, 0, 0);

        CardRects {
            id: spec.id,
            bounds: iclip(lx, top, w, full_h as i64, vp),
            full_h,
            expanded: spec.mode == SidebarCardMode::Expanded,
            clipped_top: (vp.y as i64 - top).clamp(0, full_h as i64) as u32,
            icon: empty,
            name_line: iclip(name_x, name_y, name_w, name_h, vp),
            meta: row(self.card_row_y(1, CARD_META_Y)),
            context: if spec.mode == SidebarCardMode::Expanded {
                row(self.card_row_y(2, CARD_CONTEXT_Y))
            } else {
                empty
            },
            preview,
            updated: iclip(updated_x, name_y, updated_w, name_h, vp),
            dot: empty,
            menu_button: iclip(menu_x, name_y, card_menu_w, name_h, vp),
        }
    }

    /// Resolve clicks against the same visible rectangles used for rendering.
    pub fn hit_test_cards(
        &self,
        bounds: SidebarRect,
        specs: &[SidebarCardSpec],
        scroll_offset: u32,
        point: Point,
    ) -> Option<SidebarHit> {
        let layout = self.layout_cards(bounds, specs, scroll_offset);
        if layout.new_button.contains(point) {
            return Some(SidebarHit::NewSession);
        }
        if !layout.viewport.contains(point) {
            return None;
        }
        let card = layout
            .cards
            .iter()
            .find(|card| card.bounds.contains(point))?;
        if card.menu_button.contains(point) {
            Some(SidebarHit::CardMenu(card.id))
        } else {
            Some(SidebarHit::Card(card.id))
        }
    }

    /// Choose the gap before or after each card using its actual midpoint.
    /// The dragged card keeps its slot, so the insertion targets stay still.
    pub fn drop_index_cards(
        &self,
        viewport: SidebarRect,
        specs: &[SidebarCardSpec],
        scroll_offset: u32,
        pointer_y: u32,
    ) -> usize {
        let scroll = clamp_scroll(scroll_offset, self.content_height_cards(specs), viewport.h);
        let py = pointer_y.clamp(viewport.y, viewport.bottom());
        let cy = u64::from(py - viewport.y) + u64::from(scroll);
        let mut top = 0u64;
        for (index, spec) in specs.iter().enumerate() {
            let stride = u64::from(self.card_height(spec.mode)) + u64::from(self.card_gutter);
            if cy < top + stride / 2 {
                return index;
            }
            top += stride;
        }
        specs.len()
    }

    /// Draw insertion feedback at the same cumulative gap used by hit-testing.
    pub fn drop_indicator_y_cards(
        &self,
        viewport: SidebarRect,
        specs: &[SidebarCardSpec],
        scroll_offset: u32,
        index: usize,
    ) -> Option<u32> {
        let scroll = clamp_scroll(scroll_offset, self.content_height_cards(specs), viewport.h);
        let offset = self.content_height_cards(&specs[..index.min(specs.len())]);
        let top = i64::from(offset) - i64::from(scroll) + i64::from(viewport.y);
        Some(top.clamp(i64::from(viewport.y), i64::from(viewport.bottom())) as u32)
    }

    /// Keep a selected card at its former viewport-relative top while cards
    /// before it collapse or expand. Clamp only when a content edge prevents it.
    pub fn scroll_for_card_anchor(
        &self,
        specs: &[SidebarCardSpec],
        anchor: SessionCardId,
        top_in_viewport: i64,
        viewport_h: u32,
    ) -> Option<u32> {
        let index = specs.iter().position(|spec| spec.id == anchor)?;
        let top = self.content_height_cards(&specs[..index]);
        let desired = (i64::from(top) - top_in_viewport).clamp(0, i64::from(u32::MAX)) as u32;
        Some(clamp_scroll(
            desired,
            self.content_height_cards(specs),
            viewport_h,
        ))
    }

    #[cfg(test)]
    fn expanded_specs(ids: &[SessionCardId]) -> Vec<SidebarCardSpec> {
        ids.iter()
            .map(|&id| SidebarCardSpec {
                id,
                mode: SidebarCardMode::Expanded,
            })
            .collect()
    }

    #[cfg(test)]
    fn uniform_specs(count: usize) -> Vec<SidebarCardSpec> {
        vec![
            SidebarCardSpec {
                id: SessionCardId::new(
                    crate::session_store::SessionWindowId(0),
                    crate::split_tree::PaneId::new(0)
                ),
                mode: SidebarCardMode::Expanded,
            };
            count
        ]
    }

    #[cfg(test)]
    pub fn content_height(&self, card_count: usize) -> u32 {
        self.content_height_cards(&Self::uniform_specs(card_count))
    }

    #[cfg(test)]
    pub fn layout(&self, bounds: SidebarRect, ids: &[SessionCardId], scroll: u32) -> SidebarLayout {
        self.layout_cards(bounds, &Self::expanded_specs(ids), scroll)
    }

    #[cfg(test)]
    pub fn card_local_rects(&self, id: SessionCardId, card_w: u32) -> CardRects {
        self.card_local_rects_for(
            SidebarCardSpec {
                id,
                mode: SidebarCardMode::Expanded,
            },
            card_w,
        )
    }

    #[cfg(test)]
    pub fn hit_test(
        &self,
        bounds: SidebarRect,
        ids: &[SessionCardId],
        scroll: u32,
        point: Point,
    ) -> Option<SidebarHit> {
        self.hit_test_cards(bounds, &Self::expanded_specs(ids), scroll, point)
    }

    #[cfg(test)]
    pub fn drop_index(
        &self,
        viewport: SidebarRect,
        count: usize,
        scroll: u32,
        pointer_y: u32,
    ) -> usize {
        self.drop_index_cards(viewport, &Self::uniform_specs(count), scroll, pointer_y)
    }

    #[cfg(test)]
    pub fn drop_indicator_y(
        &self,
        viewport: SidebarRect,
        count: usize,
        scroll: u32,
        index: usize,
    ) -> Option<u32> {
        self.drop_indicator_y_cards(viewport, &Self::uniform_specs(count), scroll, index)
    }

    /// Place a card's `…` menu below its button, or above it near the sidebar's
    /// bottom edge. Clamp to the usable band so the popup also avoids a native
    /// titlebar. If all items cannot fit, return an empty rect: an invisible
    /// action must never remain clickable.
    pub fn card_menu_popup_rect(
        &self,
        anchor: SidebarRect,
        item_count: usize,
        bounds: SidebarRect,
    ) -> SidebarRect {
        let w = self.menu_w.min(bounds.w);
        let h = self
            .menu_item_h
            .saturating_mul(u32::try_from(item_count).unwrap_or(u32::MAX));
        if w == 0 || h == 0 || h > bounds.h || anchor.w == 0 || anchor.h == 0 {
            return SidebarRect::new(bounds.x, bounds.y, 0, 0);
        }
        let x = anchor
            .right()
            .saturating_sub(w)
            .clamp(bounds.x, bounds.right() - w);
        let preferred_y = if anchor.bottom().saturating_add(h) <= bounds.bottom() {
            anchor.bottom()
        } else {
            anchor.y.saturating_sub(h)
        };
        let y = preferred_y.clamp(bounds.y, bounds.bottom() - h);
        SidebarRect::new(x, y, w, h)
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
/// `None` from [`SidebarMetrics::hit_test_cards`].
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
    /// Display name. The per-card rename shadows everything; a manual tab
    /// title shadows the shell-driven name. A
    /// shell-driven name is normalized (prompt-like titles fall back to the
    /// cwd's tail segment; a ` | <branch>` suffix duplicating the context row's
    /// branch is stripped).
    pub name: String,
    /// The cwd shown dim on the context row, with the home directory abbreviated
    /// to `~`. The renderer fits it to the actual available cell width.
    pub cwd: String,
    /// The branch shown on the context row; empty when the
    /// session has no branch.
    pub branch: String,
    /// The process shown on the meta row: the tty's foreground process name, or
    /// an activity-state fallback (`running` / `idle`) where detection is
    /// unavailable. Explicit agent reports take precedence over shell activity.
    pub process: String,
    /// Relative updated-time (`3m ago` / `Yday 23:47` / …). Empty for a running
    /// card: "updated just now" is a tautology while output is flowing, so
    /// only idle cards carry the age.
    pub updated: String,
    /// Client Mode panes have no local process probe or auto-approve worker;
    /// the renderer uses this to show both unsupported states explicitly.
    pub remote_unsupported: bool,
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
    // Name precedence: an explicit
    // per-card rename (FR-7) wins, then the tab's manual title, then the
    // shell-driven session name. Only the shell-driven fallback is normalized —
    // a name the user typed is shown verbatim.
    let name = match (&card.name_override, tab_title) {
        (Some(_), _) => card.display_name().to_string(),
        (None, Some(title)) => title.to_string(),
        (None, None) => normalize_shell_name(&card.name, &card.cwd, card.branch.as_deref()),
    };
    let cwd = format_cwd(&card.cwd, home, usize::MAX);
    let branch = card.branch.clone().unwrap_or_default();
    // The detected foreground process name; when unavailable (non-macOS, or not
    // yet polled) fall back to reported activity so the row is never blank.
    let process = card.process.clone().unwrap_or_else(|| {
        if card.is_running() {
            "running".to_string()
        } else {
            "idle".to_string()
        }
    });
    // A running card's age is always "now"; showing it is noise. Idle cards keep
    // the relative time (how long since this session last did anything).
    let updated = if card.agent_needs_attention() {
        format_relative_time(now, card.agent_status_at.unwrap_or(card.updated_at))
    } else if card.is_running() {
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
        remote_unsupported: false,
    }
}

/// Normalize a shell-driven session name for the card's title row.
///
/// - A shell-default prompt title (`user@host:~/path`, zsh/bash `%n@%m:%~`)
///   carries no identity beyond the cwd already on the card, and truncates
///   ugly; it falls back to the cwd's tail segment (usually the repo name).
/// - A ` | <branch>` suffix that duplicates the card's own branch (some
///   prompts title `repo | branch`) is stripped — the branch already shows on
///   the context row.
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
pub(crate) fn cwd_tail(cwd: &str) -> Option<&str> {
    cwd.rsplit('/').find(|segment| !segment.is_empty())
}

/// Whether a session's bell should escalate to an attention request (FR-A3):
/// a generic pane's beep stays a plain bell (NFR-A4).
pub fn bell_escalates_to_attention(kind: AgentKind) -> bool {
    kind != AgentKind::Generic
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
        IconKind, PreviewLine, PreviewSpan, SessionDelta, SessionStore, SessionWindowId,
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

    // AC-3 (FR-2): the card lines carry name, cwd, branch, and the
    // relative updated-time.
    #[test]
    fn card_lines_include_name_cwd_branch_and_time() {
        let card = sample_card(
            "build",
            "/Users/dev/repos/github.com/example/very-long-project",
            Some("main"),
        );
        let lines = card_lines(&card, wall(10, 3), None, None);

        // Identity is the display name without a decorative glyph prefix.
        assert_eq!(lines.name, "build");

        // Keep the full path here: the renderer knows how many cells remain
        // beside the branch and truncates only as much as that width requires.
        assert_eq!(lines.cwd, card.cwd);
        assert_eq!(lines.branch, "main");

        // updated-time matches the pure PR1 formatter.
        assert_eq!(
            lines.updated,
            format_relative_time(wall(10, 3), wall(10, 0))
        );
        assert_eq!(lines.updated, "3m ago");

        // The running-process row shows the detected foreground process.
        assert_eq!(lines.process, "cargo");
    }

    #[test]
    fn card_lines_use_agent_activity_and_preserve_attention_age() {
        use noa_grid::AgentState;
        for (state, busy, process, updated) in [
            (AgentState::Running, false, "running", ""),
            (AgentState::Finished, true, "idle", "5m ago"),
            (AgentState::Permission, true, "idle", "2m ago"),
        ] {
            let mut card = sample_card("agent", "/repo", None);
            card.process = None;
            card.busy = busy;
            card.agent_status = Some(noa_grid::AgentStatus {
                state,
                detail: String::new(),
                blocked: None,
                app: None,
            });
            card.agent_status_at = Some(wall(10, 3));
            let lines = card_lines(&card, wall(10, 5), None, None);
            assert_eq!(lines.process, process, "{state:?}");
            assert_eq!(lines.updated, updated, "{state:?}");
        }
    }

    // Shell-driven names normalize: a prompt-like default title falls back to
    // the cwd's tail segment, and a ` | <branch>` suffix duplicating the context
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
        // already rides the context row) …
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

    #[test]
    fn agent_display_names_use_the_brand_or_raw_process() {
        // The display name replaces the raw process for known agents.
        assert_eq!(
            agent_display_name(AgentKind::ClaudeCode, "claude"),
            "Claude Code"
        );
        assert_eq!(agent_display_name(AgentKind::Codex, "codex"), "Codex");
        assert_eq!(agent_display_name(AgentKind::Agy, "gemini"), "agy");
        assert_eq!(agent_display_name(AgentKind::Generic, "zsh"), "zsh");
    }

    // AC-A2 (FR-A3/NFR-A4): a known agent's bell escalates to attention; a
    // generic process — or an unresolved one — stays a plain bell.
    #[test]
    fn bell_escalates_only_for_known_agents() {
        for kind in [AgentKind::ClaudeCode, AgentKind::Codex, AgentKind::Agy] {
            assert!(bell_escalates_to_attention(kind));
        }
        assert!(!bell_escalates_to_attention(AgentKind::Generic));
    }

    #[test]
    fn card_lines_omit_branch_when_absent() {
        let card = sample_card("shell", "/repo", None);
        let lines = card_lines(&card, wall(10, 0), None, None);
        assert_eq!(lines.cwd, "/repo");
        assert_eq!(lines.branch, "");
    }

    #[test]
    fn card_lines_abbreviate_home_without_losing_context_before_layout() {
        let cwd = "/Users/dev/repos/日本語の長いプロジェクト/feature-worktree";
        let card = sample_card("worktree", cwd, Some("feature/sidebar"));
        let lines = card_lines(&card, wall(10, 0), Some("/Users/dev"), None);
        assert_eq!(
            lines.cwd,
            "~/repos/日本語の長いプロジェクト/feature-worktree"
        );
        assert_eq!(lines.branch, "feature/sidebar");
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
        let card_menu = Point::new(vp.x + menu_x + 5, vp.y + CARD_NAME_Y + 1);
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
        let bounds = SidebarRect::new(0, 0, 360, 380);
        let popup = m1().card_menu_popup_rect(anchor, CARD_MENU_ITEMS.len(), bounds);

        // Sits directly under the anchor and never spills past the sidebar edge.
        assert_eq!(popup.y, anchor.bottom());
        assert!(popup.right() <= 360);
        assert_eq!(popup.h, SIDEBAR_MENU_ITEM_H * CARD_MENU_ITEMS.len() as u32);

        // The item order stays the same regardless of where the popup opens.
        let item0 = m1().card_menu_item_rect(popup, 0);
        let hit = m1().card_menu_hit_test(popup, Point::new(item0.x + 4, item0.y + 4));
        assert_eq!(hit, Some(CardMenuItem::Close));
        assert_eq!(card_menu_label(CardMenuItem::Close), "Close");
        let item1 = m1().card_menu_item_rect(popup, 1);
        assert_eq!(
            m1().card_menu_hit_test(popup, Point::new(item1.x, item1.bottom() - 1)),
            Some(CardMenuItem::Rename)
        );

        // A click outside the popup is a dismiss (miss).
        assert_eq!(
            m1().card_menu_hit_test(popup, Point::new(popup.right() + 5, popup.y + 4)),
            None
        );
        assert_eq!(
            m1().card_menu_hit_test(popup, Point::new(popup.x, popup.bottom())),
            None
        );
    }

    // The anchor's own width (CARD_MENU_W) is private; alias it for the test.
    const CARD_MENU_W_ANCHOR: u32 = CARD_MENU_W;

    #[test]
    fn card_menu_popup_opens_above_a_bottom_anchor_and_keeps_both_items_clickable() {
        for scale in [1.0, 1.5, 2.0] {
            let metrics = SidebarMetrics::new(scale);
            let bounds = SidebarRect::new(0, metrics.s(30), metrics.s(360), metrics.s(350));
            let anchor = SidebarRect::new(
                metrics.s(300),
                bounds.bottom() - metrics.s(CARD_NAME_H),
                metrics.s(CARD_MENU_W),
                metrics.s(CARD_NAME_H),
            );
            let popup = metrics.card_menu_popup_rect(anchor, CARD_MENU_ITEMS.len(), bounds);
            assert_eq!(popup.bottom(), anchor.y);
            assert!(popup.y >= bounds.y);
            assert!(popup.right() <= bounds.right());
            for (index, &item) in CARD_MENU_ITEMS.iter().enumerate() {
                let row = metrics.card_menu_item_rect(popup, index);
                for y in [row.y, row.bottom() - 1] {
                    assert_eq!(
                        metrics.card_menu_hit_test(popup, Point::new(row.x, y)),
                        Some(item)
                    );
                }
            }
            assert_eq!(
                metrics.card_menu_hit_test(popup, Point::new(popup.x, anchor.y)),
                None
            );
        }
    }

    #[test]
    fn card_menu_popup_clamps_within_a_small_band_below_the_titlebar() {
        let bounds = SidebarRect::new(16, 32, 100, 60);
        let anchor = SidebarRect::new(82, 57, CARD_MENU_W, CARD_NAME_H);
        let popup = m1().card_menu_popup_rect(anchor, CARD_MENU_ITEMS.len(), bounds);
        assert_eq!(popup, SidebarRect::new(16, 32, 100, 48));
        assert_eq!(
            m1().card_menu_hit_test(popup, Point::new(popup.x, popup.y - 1)),
            None
        );

        // Rendering only part of the menu would leave ambiguous click targets.
        let too_short = SidebarRect::new(bounds.x, bounds.y, bounds.w, popup.h - 1);
        let hidden = m1().card_menu_popup_rect(anchor, CARD_MENU_ITEMS.len(), too_short);
        assert_eq!((hidden.w, hidden.h), (0, 0));
        assert_eq!(
            m1().card_menu_hit_test(hidden, Point::new(hidden.x, hidden.y)),
            None
        );

        let invisible_anchor = SidebarRect::new(anchor.x, anchor.y, 0, 0);
        let hidden = m1().card_menu_popup_rect(invisible_anchor, CARD_MENU_ITEMS.len(), bounds);
        assert_eq!((hidden.w, hidden.h), (0, 0));
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
    fn hit_test_keeps_cards_contiguous_without_a_gutter() {
        let (bounds, ids) = six_id_bounds();
        let vp = m1().bands(bounds).viewport;
        let second_card_top = Point::new(vp.x + 40, vp.y + SIDEBAR_CARD_H);
        assert_eq!(
            m1().hit_test(bounds, &ids, 0, second_card_top),
            Some(SidebarHit::Card(ids[1]))
        );
    }

    // AC-23 (FR-15): with more cards than fit, scroll clamps to [0, max] and the
    // first/last cards are each reachable at the extremes.
    #[test]
    fn scroll_clamp_bounds_and_endpoints() {
        let content = m1().content_height(6);
        let viewport_h = 3 * SIDEBAR_CARD_STRIDE;
        let max = content - viewport_h;

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

    // Identity begins at the common text edge; updated time and the menu stay
    // separate on the same row, with metadata/context/output below.
    #[test]
    fn card_rects_align_identity_and_reserve_space_before_the_menu() {
        let (bounds, ids) = six_id_bounds();
        let layout = m1().layout(bounds, &ids, 0);
        let card = &layout.cards[0];

        assert_eq!(card.name_line.x, card.bounds.x + CARD_PAD);
        assert_eq!(card.name_line.x, card.meta.x);
        assert_eq!(
            (card.dot.w, card.dot.h, card.icon.w, card.icon.h),
            (0, 0, 0, 0)
        );

        // Name, then the updated-time region, then the `…` hit region.
        assert!(card.name_line.right() <= card.updated.x);
        assert_eq!(card.updated.right() + 12, card.menu_button.x);
        assert_eq!(card.updated.y, card.name_line.y);
        assert!(card.updated.w > 0);

        // Identity, agent state, working context and output stay separate.
        assert!(card.name_line.bottom() <= card.meta.y);
        assert!(card.meta.bottom() <= card.context.y);
        assert_eq!(card.preview.len(), DEFAULT_SIDEBAR_PREVIEW_LINES);
        let mut previous_y = card.context.y;
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
        assert_eq!(m2.content_height(1), m2.card_h + m2.card_gutter);
        assert_eq!(m2.content_height(1), 2 * SIDEBAR_CARD_STRIDE);
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

    #[test]
    fn expanded_cards_end_after_their_last_configured_row() {
        for scale in [1.0, 1.5, 2.0] {
            for width in [200, 360, 600] {
                for preview_lines in [0, 1, 5, 20] {
                    let metrics = SidebarMetrics::new_with_preview_lines(scale, preview_lines);
                    let card = metrics.card_local_rects(card_id(1, 1), metrics.s(width));
                    assert_eq!(card.clipped_top, 0);
                    assert!(card.name_line.bottom() <= card.meta.y);
                    assert!(card.meta.bottom() <= card.context.y);
                    assert_eq!(card.context.h, metrics.s(CARD_LINE_H));
                    assert!(card.context.right() <= card.bounds.right());
                    let last_row = card.preview.last().unwrap_or(&card.context);
                    assert_eq!(card.preview.len(), preview_lines);
                    assert_eq!(last_row.h, metrics.s(CARD_LINE_H));
                    let bottom_pad = card.bounds.bottom() - last_row.bottom();
                    assert!(bottom_pad.abs_diff(metrics.s(CARD_BOTTOM_PAD)) <= 1);
                    if preview_lines == 0 {
                        assert_eq!(
                            card.bounds.h,
                            metrics.s(CARD_CONTEXT_Y + CARD_LINE_H + CARD_BOTTOM_PAD)
                        );
                    } else {
                        assert!(card.context.bottom() < card.preview[0].y);
                    }
                }
            }
        }
    }

    #[test]
    fn actual_font_cell_heights_keep_metadata_and_preview_on_distinct_rows() {
        for cell_h in [12.0, 14.0, 15.0, 17.5, 18.0, 24.0, 36.0] {
            for scale in [1.0, 2.0] {
                for preview_lines in [0, 5, 20] {
                    let metrics = SidebarMetrics::new_with_preview_lines(scale, preview_lines)
                        .with_cell_height(cell_h);
                    let card = metrics.card_local_rects(card_id(1, 1), metrics.s(360));
                    let cell_row = |rect: SidebarRect| (rect.y as f32 / cell_h).round() as u32;
                    let name_row = cell_row(card.name_line);
                    assert_eq!(cell_row(card.dot), name_row);
                    assert_eq!(cell_row(card.icon), name_row);
                    assert_eq!(cell_row(card.updated), name_row);
                    assert_eq!(cell_row(card.menu_button), name_row);
                    assert_eq!(cell_row(card.meta), name_row + 1);
                    assert_eq!(cell_row(card.context), name_row + 2);
                    assert_eq!(card.context.h, cell_h.ceil() as u32);
                    for (index, preview) in card.preview.iter().enumerate() {
                        assert_eq!(cell_row(*preview), name_row + 3 + index as u32);
                        assert_eq!(preview.h, cell_h.ceil() as u32);
                        assert!(preview.bottom() <= card.bounds.bottom());
                    }
                    let last_row = card.preview.last().unwrap_or(&card.context);
                    assert_eq!(
                        card.bounds.bottom() - last_row.bottom(),
                        metrics.s(CARD_BOTTOM_PAD)
                    );
                    // The texture's floor-sized grid must contain the last row,
                    // including a fractional font height rounded to pixels.
                    assert!((metrics.card_h as f32 / cell_h).floor() as u32 > cell_row(*last_row));
                    assert_eq!(
                        metrics.content_height(1),
                        metrics.card_h + metrics.card_gutter
                    );
                }
            }
        }
    }

    #[test]
    fn actual_font_menu_rows_match_the_click_target() {
        for cell_h in [12.0, 14.0, 15.0, 18.0, 24.0, 36.0] {
            let metrics = SidebarMetrics::new(1.0).with_cell_height(cell_h);
            let ids = vec![card_id(1, 1)];
            let bounds = SidebarRect::new(0, 0, 360, metrics.toolbar_h + metrics.card_h);
            let layout = metrics.layout(bounds, &ids, 0);
            let menu = layout.cards[0].menu_button;
            assert_eq!(menu.h, cell_h.ceil() as u32);
            for y in [menu.y, menu.bottom() - 1] {
                assert_eq!(
                    metrics.hit_test(bounds, &ids, 0, Point::new(menu.x, y)),
                    Some(SidebarHit::CardMenu(ids[0]))
                );
            }
            assert_eq!(
                metrics.hit_test(bounds, &ids, 0, Point::new(menu.x, menu.bottom())),
                Some(SidebarHit::Card(ids[0]))
            );
        }
    }

    fn mixed_card_specs() -> Vec<SidebarCardSpec> {
        [
            SidebarCardMode::Compact,
            SidebarCardMode::Expanded,
            SidebarCardMode::Attention,
            SidebarCardMode::Compact,
        ]
        .into_iter()
        .enumerate()
        .map(|(index, mode)| SidebarCardSpec {
            id: card_id(1, index as u64),
            mode,
        })
        .collect()
    }

    #[test]
    fn card_modes_reserve_only_their_visible_rows_on_the_actual_font_grid() {
        for cell_h in [12.0, 14.0, 15.0, 17.5, 18.0, 24.0, 36.0] {
            for scale in [1.0, 1.5, 2.0] {
                for preview_lines in [0, 1, 5, 20] {
                    let metrics = SidebarMetrics::new_with_preview_lines(scale, preview_lines)
                        .with_cell_height(cell_h);
                    let specs = mixed_card_specs();
                    for width in [200, 360, 600] {
                        let compact = metrics.card_local_rects_for(specs[0], metrics.s(width));
                        let expanded = metrics.card_local_rects_for(specs[1], metrics.s(width));
                        let attention = metrics.card_local_rects_for(specs[2], metrics.s(width));
                        assert!(!compact.expanded && !attention.expanded && expanded.expanded);
                        assert_eq!(compact.context.h, 0);
                        assert_eq!(attention.context.h, 0);
                        assert!(compact.preview.is_empty());
                        assert_eq!(attention.preview.len(), usize::from(preview_lines > 0));
                        assert_eq!(expanded.preview.len(), preview_lines);
                        if preview_lines == 0 {
                            assert_eq!(compact.full_h, attention.full_h);
                        } else {
                            assert!(compact.full_h < attention.full_h);
                        }
                        assert!(attention.full_h <= expanded.full_h);
                        assert_eq!(metrics.compact_card_height(), compact.full_h);
                        for card in [&compact, &attention, &expanded] {
                            let cell_row =
                                |rect: SidebarRect| (rect.y as f32 / cell_h).round() as u32;
                            let name_row = cell_row(card.name_line);
                            assert!(name_row >= 1);
                            assert_eq!(cell_row(card.meta), name_row + 1);
                            assert_eq!(card.name_line.x, metrics.s(CARD_PAD));
                            assert_eq!(card.name_line.x, card.meta.x);
                            assert_eq!(card.full_h, card.bounds.h);
                            assert_eq!(card.clipped_top, 0);
                            if card.expanded {
                                assert_eq!(cell_row(card.context), name_row + 2);
                            }
                            for (index, preview) in card.preview.iter().enumerate() {
                                let offset = if card.expanded { 3 } else { 2 };
                                assert_eq!(cell_row(*preview), name_row + offset + index as u32);
                                assert!(preview.right() <= card.bounds.right());
                            }
                            let last = card.preview.last().unwrap_or(if card.expanded {
                                &card.context
                            } else {
                                &card.meta
                            });
                            assert_eq!(card.full_h - last.bottom(), metrics.s(CARD_BOTTOM_PAD));
                            assert!((card.full_h as f32 / cell_h).floor() as u32 > cell_row(*last));
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn mixed_card_heights_share_stacking_clipping_and_hit_regions() {
        let metrics = SidebarMetrics::new(1.0).with_cell_height(18.0);
        let specs = mixed_card_specs();
        let local: Vec<_> = specs
            .iter()
            .map(|&spec| metrics.card_local_rects_for(spec, 200))
            .collect();
        let expected_h: u32 = local
            .iter()
            .map(|card| card.full_h + metrics.card_gutter)
            .sum();
        assert_eq!(metrics.content_height_cards(&specs), expected_h);
        let bounds = SidebarRect::new(10, 40, 200, metrics.toolbar_h + 140);
        let vp = metrics.bands(bounds).viewport;
        let mut top = 0;
        for (index, card) in local.iter().enumerate() {
            let all = metrics.layout_cards(
                SidebarRect::new(10, 40, 200, metrics.toolbar_h + expected_h),
                &specs,
                0,
            );
            assert_eq!(all.cards[index].bounds.y, vp.y + top);
            assert_eq!(all.cards[index].full_h, card.full_h);
            top += card.full_h + metrics.card_gutter;
        }
        for scroll in [0, 40, local[0].full_h + 32, u32::MAX] {
            let layout = metrics.layout_cards(bounds, &specs, scroll);
            assert_eq!(layout.content_h, expected_h);
            for card in &layout.cards {
                assert!(card.clipped_top + card.bounds.h <= card.full_h);
                assert!(card.bounds.y >= vp.y && card.bounds.bottom() <= vp.bottom());
                if card.menu_button.h > 0 {
                    let menu = card.menu_button;
                    assert_eq!(
                        metrics.hit_test_cards(
                            bounds,
                            &specs,
                            scroll,
                            Point::new(menu.x, menu.bottom() - 1)
                        ),
                        Some(SidebarHit::CardMenu(card.id))
                    );
                }
            }
            // Every visible pixel belongs to the same card in draw and hit-test,
            // including the boundary between a compact and an expanded card.
            for y in vp.y..vp.bottom() {
                let point = Point::new(vp.x + 1, y);
                let expected = layout
                    .cards
                    .iter()
                    .find(|card| card.bounds.contains(point))
                    .map(|card| SidebarHit::Card(card.id));
                assert_eq!(
                    metrics.hit_test_cards(bounds, &specs, scroll, point),
                    expected
                );
            }
        }
        let clipped = metrics.layout_cards(bounds, &specs, local[0].full_h + 12);
        assert_eq!(clipped.cards[0].id, specs[1].id);
        assert_eq!(clipped.cards[0].clipped_top, 12);
        assert_eq!(clipped.cards[0].bounds.h, vp.h);
        assert!(clipped.cards[0].clipped_top + vp.h < clipped.cards[0].full_h);
    }

    #[test]
    fn mixed_card_drop_targets_use_each_cards_midpoint_and_cumulative_gap() {
        let metrics = SidebarMetrics::new(1.0).with_cell_height(18.0);
        let specs = mixed_card_specs();
        let content = metrics.content_height_cards(&specs);
        let vp = SidebarRect::new(0, 30, 360, content);
        let mut top = vp.y;
        for (index, &spec) in specs.iter().enumerate() {
            let height = metrics.card_local_rects_for(spec, vp.w).full_h;
            let midpoint = top + (height + metrics.card_gutter) / 2;
            assert_eq!(metrics.drop_index_cards(vp, &specs, 0, midpoint - 1), index);
            assert_eq!(metrics.drop_index_cards(vp, &specs, 0, midpoint), index + 1);
            assert_eq!(
                metrics.drop_indicator_y_cards(vp, &specs, 0, index),
                Some(top)
            );
            top += height + metrics.card_gutter;
        }
        assert_eq!(
            metrics.drop_index_cards(vp, &specs, 0, vp.bottom()),
            specs.len()
        );
        assert_eq!(
            metrics.drop_indicator_y_cards(vp, &specs, 0, specs.len()),
            Some(vp.bottom())
        );

        let short = SidebarRect::new(vp.x, vp.y, vp.w, 80);
        let scroll = metrics.compact_card_height() + 10;
        let midpoint = short.y + metrics.card_h / 2 - 10;
        assert_eq!(
            metrics.drop_index_cards(short, &specs, scroll, midpoint - 1),
            1
        );
        assert_eq!(metrics.drop_index_cards(short, &specs, scroll, midpoint), 2);
        assert_eq!(
            metrics.drop_indicator_y_cards(short, &specs, scroll, 1),
            Some(short.y)
        );
        assert_eq!(
            metrics.drop_indicator_y_cards(short, &specs, scroll, 2),
            Some(short.bottom())
        );
    }

    #[test]
    fn changing_selection_preserves_the_clicked_card_screen_position() {
        let metrics = SidebarMetrics::new(1.0).with_cell_height(18.0);
        let mut before = mixed_card_specs();
        before.extend((4..12).map(|pane| SidebarCardSpec {
            id: card_id(1, pane),
            mode: SidebarCardMode::Compact,
        }));
        let mut after = before.clone();
        after[1].mode = SidebarCardMode::Compact;
        after[3].mode = SidebarCardMode::Expanded;
        let bounds = SidebarRect::new(0, 0, 360, metrics.toolbar_h + 250);
        let old_scroll = 180;
        let old_layout = metrics.layout_cards(bounds, &before, old_scroll);
        let old = old_layout
            .cards
            .iter()
            .find(|card| card.id == after[3].id)
            .unwrap();
        let relative_top =
            i64::from(old.bounds.y) - i64::from(old.clipped_top) - i64::from(old_layout.viewport.y);
        let new_scroll = metrics
            .scroll_for_card_anchor(&after, after[3].id, relative_top, old_layout.viewport.h)
            .unwrap();
        assert!(
            new_scroll < old_scroll,
            "the expanded card above the click collapsed"
        );
        let new_layout = metrics.layout_cards(bounds, &after, new_scroll);
        let new = new_layout
            .cards
            .iter()
            .find(|card| card.id == after[3].id)
            .unwrap();
        assert_eq!(old.bounds.y, new.bounds.y);
        assert!(new.expanded);

        assert_eq!(
            metrics.scroll_for_card_anchor(&after, after[0].id, 30, 250),
            Some(0)
        );
        assert_eq!(
            metrics.scroll_for_card_anchor(&after, after[11].id, -200, 250),
            Some(metrics.content_height_cards(&after) - 250)
        );
        assert_eq!(
            metrics.scroll_for_card_anchor(&after, card_id(99, 99), 0, 250),
            None
        );
    }

    #[test]
    fn invalid_font_cell_heights_keep_the_design_layout() {
        let metrics = SidebarMetrics::new(1.0);
        for cell_h in [0.0, -1.0, f32::NAN, f32::INFINITY] {
            assert_eq!(metrics.with_cell_height(cell_h), metrics);
        }
    }

    #[test]
    fn clipped_top_preserves_the_visible_slice_when_both_card_ends_are_hidden() {
        for scale in [1.0, 1.5, 2.0] {
            let metrics = SidebarMetrics::new(scale);
            let ids = vec![card_id(1, 1), card_id(1, 2)];
            let viewport_h = metrics.s(50);
            let bounds = SidebarRect::new(0, 0, metrics.s(200), metrics.toolbar_h + viewport_h);
            let scroll = metrics.s(32);
            let layout = metrics.layout(bounds, &ids, scroll);
            let card = &layout.cards[0];
            assert_eq!(layout.cards.len(), 1);
            assert_eq!(card.bounds, layout.viewport);
            assert_eq!(card.clipped_top, scroll);
            assert_eq!(card.bounds.h, viewport_h);
            assert!(card.clipped_top + card.bounds.h < metrics.card_h);
            assert_eq!(card.context.h, metrics.s(CARD_LINE_H));
            assert_eq!(
                card.context.y,
                layout.viewport.y + metrics.s(CARD_CONTEXT_Y) - scroll
            );

            let at_top = metrics.layout(bounds, &ids, 0);
            assert_eq!(at_top.cards[0].clipped_top, 0);
            let at_bottom = metrics.layout(bounds, &ids, u32::MAX);
            assert_eq!(at_bottom.cards[0].id, ids[1]);
            assert_eq!(
                at_bottom.cards[0].clipped_top + at_bottom.cards[0].bounds.h,
                metrics.card_h
            );
        }
    }

    #[test]
    fn menu_hit_target_matches_its_visible_top_and_bottom_edges() {
        for scale in [1.0, 1.5, 2.0] {
            let metrics = SidebarMetrics::new(scale);
            let ids = vec![card_id(1, 1)];
            let bounds = SidebarRect::new(0, 0, metrics.s(360), metrics.toolbar_h + metrics.card_h);
            let layout = metrics.layout(bounds, &ids, 0);
            let menu = layout.cards[0].menu_button;
            let x = menu.x + menu.w / 2;
            for y in [menu.y, menu.bottom() - 1] {
                assert_eq!(
                    metrics.hit_test(bounds, &ids, 0, Point::new(x, y)),
                    Some(SidebarHit::CardMenu(ids[0]))
                );
            }
            for y in [menu.y - 1, menu.bottom()] {
                assert_eq!(
                    metrics.hit_test(bounds, &ids, 0, Point::new(x, y)),
                    Some(SidebarHit::Card(ids[0]))
                );
            }
        }
    }

    // At scale 2.0 the bands, card height, and every interior row double, so a
    // Retina card keeps all rows (name / meta / context / preview rows) inside
    // its doubled height instead of clipping — and hit-test maps correctly
    // against the scaled geometry.
    #[test]
    fn layout_and_hit_test_scale_at_dpr_2() {
        let m2 = SidebarMetrics::new(2.0);
        let ids: Vec<_> = (0..6).map(|p| card_id(1, p)).collect();
        // Doubled width and height, with room for exactly three scaled rows.
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
        assert_eq!(card.name_line.x, card.bounds.x + m2.s(CARD_PAD));
        assert_eq!(
            (card.dot.w, card.dot.h, card.icon.w, card.icon.h),
            (0, 0, 0, 0)
        );
        assert_eq!(card.updated.y, card.name_line.y);
        assert!(card.name_line.bottom() <= card.meta.y);
        assert!(card.meta.bottom() <= card.context.y);
        assert_eq!(card.preview.len(), DEFAULT_SIDEBAR_PREVIEW_LINES);
        let mut previous_y = card.context.y;
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
        let card_menu = Point::new(vp.x + menu_x + 5, vp.y + m2.s(CARD_NAME_Y) + 1);
        assert_eq!(
            m2.hit_test(bounds, &ids, 0, card_menu),
            Some(SidebarHit::CardMenu(ids[0]))
        );

        // With the flat list there is no gutter; the next pixel row belongs to
        // the next card.
        let next_card = Point::new(vp.x + 80, vp.y + m2.card_h + 3);
        assert_eq!(
            m2.hit_test(bounds, &ids, 0, next_card),
            Some(SidebarHit::Card(ids[1]))
        );
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
