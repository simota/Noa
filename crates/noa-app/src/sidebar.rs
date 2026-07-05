//! Pure session-sidebar layout, hit-test, and scroll math (spec
//! `docs/specs/session-sidebar.md`, ADR 0001). Ghostty has no analog; this is a
//! noa addition.
//!
//! Mirrors [`crate::tab_overview`]'s conventions: every function here is pure
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

use crate::session_store::{
    IconKind, SessionCard, SessionCardId, WallClock, format_relative_time,
};

// All the `SIDEBAR_*`/`CARD_*` metrics below are the design values at scale
// 1.0, tuned for the sidebar's dedicated small font (≈11.5pt), so cards read
// compact and dense (mockup parity). `SidebarMetrics::new(scale)` multiplies
// them by the window DPR.

/// Height of the top header band (status label / center title / name pill,
/// FR-5). Compile-time constant — no config knob (⚠G precedent, mirroring
/// `tab_overview.rs`'s fixed chrome bands).
pub const SIDEBAR_HEADER_H: u32 = 36;

/// Height of the toolbar band holding the `+` (new session) and `…` (menu)
/// buttons below the header.
pub const SIDEBAR_TOOLBAR_H: u32 = 30;

/// Height of one session card. Five rows, one field per line (name / cwd /
/// branch / running process / updated-time) with comfortable spacing.
pub const SIDEBAR_CARD_H: u32 = 112;

/// Vertical gap between adjacent cards.
pub const SIDEBAR_CARD_GUTTER: u32 = 8;

/// Vertical stride from one card's top to the next (card + gutter).
pub const SIDEBAR_CARD_STRIDE: u32 = SIDEBAR_CARD_H + SIDEBAR_CARD_GUTTER;

/// Max characters of a cwd shown in the cwd line before tail-first truncation.
const CWD_MAX_CHARS: usize = 32;

// Card interior metrics (all compile-time; see `card_rects`/`card_lines`).
const CARD_PAD: u32 = 12;
const CARD_ICON_W: u32 = 18;
const CARD_DOT_D: u32 = 8;
const CARD_MENU_W: u32 = 22;
const CARD_LINE_H: u32 = 15;
const CARD_NAME_H: u32 = 18;

// Card interior row baselines (top-relative), one field per line: name, cwd,
// branch, running process, updated-time. The branch row is always reserved
// (fixed card height) and left blank when the session has no branch — simpler
// than a dynamic per-card height.
const CARD_NAME_Y: u32 = 10;
const CARD_CWD_Y: u32 = 30;
const CARD_BRANCH_Y: u32 = 50;
const CARD_PROCESS_Y: u32 = 70;
const CARD_UPDATED_Y: u32 = 90;

// Toolbar `+` / `…` button metrics (kept as comfortable hit targets).
const TOOLBAR_BUTTON_W: u32 = 26;
const TOOLBAR_BUTTON_H: u32 = 22;

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

/// Header rects (FR-5): a left status label, a centered title, and a
/// right-aligned session-name pill. Geometry only — the caller renders text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HeaderRects {
    pub status_label: SidebarRect,
    pub title: SidebarRect,
    pub name_pill: SidebarRect,
}

/// Sub-rects of one laid-out session card, in window space, each clipped to the
/// scrolling viewport (a card scrolled partly off-screen yields clipped, and
/// possibly zero-size, sub-rects).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CardRects {
    pub id: SessionCardId,
    pub bounds: SidebarRect,
    pub icon: SidebarRect,
    pub name_line: SidebarRect,
    /// The cwd row (dim, tail-truncated, full width).
    pub cwd_line: SidebarRect,
    /// The branch row (dim); blank when the session has no branch.
    pub branch_line: SidebarRect,
    /// The running-process row (foreground process name / shell state).
    pub process: SidebarRect,
    /// The updated-time row (dim), on its own bottom line.
    pub updated: SidebarRect,
    pub dot: SidebarRect,
    pub menu_button: SidebarRect,
}

/// The full pure layout of the sidebar for one frame: header/toolbar rects, the
/// visible card rects (with per-card sub-rects), and the scroll extents.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SidebarLayout {
    pub header: HeaderRects,
    pub new_button: SidebarRect,
    pub menu_button: SidebarRect,
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
    /// Header band height (scaled).
    pub header_h: u32,
    /// Toolbar band height (scaled).
    pub toolbar_h: u32,
    /// One card's height (scaled).
    pub card_h: u32,
    /// Vertical gap between cards (scaled).
    pub card_gutter: u32,
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
        let scale = if scale.is_finite() && scale > 0.0 {
            scale
        } else {
            1.0
        };
        let s = |v: u32| ((v as f32) * scale).round() as u32;
        let card_h = s(SIDEBAR_CARD_H);
        let card_gutter = s(SIDEBAR_CARD_GUTTER);
        Self {
            scale,
            header_h: s(SIDEBAR_HEADER_H),
            toolbar_h: s(SIDEBAR_TOOLBAR_H),
            card_h,
            card_gutter,
            card_stride: card_h + card_gutter,
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

    /// Lay out the header band's three rects. Widths are proportional so the
    /// title keeps the middle regardless of sidebar width; all rects clamp to
    /// the band.
    pub fn header_rects(&self, header: SidebarRect) -> HeaderRects {
        let line_h = self.s(HEADER_LINE_H).min(header.h);
        let cy = header.y + (header.h - line_h) / 2;
        let status_w = (header.w * 35 / 100).min(header.w);
        let pill_w = (header.w * 30 / 100).min(header.w);
        let pad = self.s(CARD_PAD);
        let gap = self.s(6);

        let status_x = header.x + pad.min(header.w);
        let status_label = SidebarRect::new(status_x, cy, status_w, line_h);
        let pill_x = header.right().saturating_sub(pad).saturating_sub(pill_w);
        let name_pill = SidebarRect::new(pill_x, cy, pill_w, line_h);

        let title_x = status_label.right() + gap;
        let title_w = name_pill.x.saturating_sub(title_x).saturating_sub(gap);
        let title = SidebarRect::new(title_x, cy, title_w, line_h);

        HeaderRects {
            status_label,
            title,
            name_pill,
        }
    }

    /// The `+` (new session) button rect in the toolbar band, pinned right.
    pub fn new_button_rect(&self, toolbar: SidebarRect) -> SidebarRect {
        let btn_w = self.s(TOOLBAR_BUTTON_W);
        let btn_h = self.s(TOOLBAR_BUTTON_H);
        let h = btn_h.min(toolbar.h);
        let y = toolbar.y + (toolbar.h - h) / 2;
        let x = toolbar
            .right()
            .saturating_sub(self.s(CARD_PAD))
            .saturating_sub(btn_w);
        SidebarRect::new(x, y, btn_w.min(toolbar.w), h)
    }

    /// The header-level `…` menu button rect, just left of the `+` button.
    pub fn menu_button_rect(&self, toolbar: SidebarRect) -> SidebarRect {
        let plus = self.new_button_rect(toolbar);
        let btn_w = self.s(TOOLBAR_BUTTON_W);
        let x = plus.x.saturating_sub(self.s(6)).saturating_sub(btn_w);
        SidebarRect::new(x, plus.y, btn_w.min(toolbar.w), plus.h)
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
            header: self.header_rects(bands.header),
            new_button: self.new_button_rect(bands.toolbar),
            menu_button: self.menu_button_rect(bands.toolbar),
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
    pub fn card_local_rects(&self, id: SessionCardId, inset: u32) -> CardRects {
        self.card_rects(id, 0, SidebarRect::new(0, 0, inset, self.card_h))
    }

    /// Build one card's clipped sub-rects from its (possibly off-viewport)
    /// window top `top` and the viewport `vp`. Every interior offset scales with
    /// the DPR so the card's five rows (name / cwd / branch / process / updated)
    /// fit its scaled height on a Retina display.
    fn card_rects(&self, id: SessionCardId, top: i64, vp: SidebarRect) -> CardRects {
        let lx = vp.x as i64;
        let w = vp.w as i64;
        let pad = self.s(CARD_PAD) as i64;
        let dot_d = self.s(CARD_DOT_D) as i64;
        let icon_w = self.s(CARD_ICON_W) as i64;
        let card_menu_w = self.s(CARD_MENU_W) as i64;
        let line_h = self.s(CARD_LINE_H) as i64;
        let name_h = self.s(CARD_NAME_H) as i64;
        let gap6 = self.s(6) as i64;

        // Name row, left to right: status dot, project icon, display name. The
        // dot sits at the card's left edge (mockup parity) as a small color
        // chip; the icon and name follow it. The `…` hit region pins the far
        // corner, and the name now runs the full width up to it (the
        // updated-time moved to its own bottom row).
        let dot_x = lx + pad;
        let icon_x = dot_x + dot_d + self.s(8) as i64;
        let name_x = icon_x + icon_w + gap6;
        let menu_x = lx + w - pad - card_menu_w;
        let name_w = menu_x - name_x - gap6;

        let body_w = w - 2 * pad;
        let row = |y: u32| iclip(lx + pad, top + self.s(y) as i64, body_w, line_h, vp);

        let name_y = top + self.s(CARD_NAME_Y) as i64;

        CardRects {
            id,
            bounds: iclip(lx, top, w, self.card_h as i64, vp),
            icon: iclip(icon_x, name_y, icon_w, name_h, vp),
            name_line: iclip(name_x, name_y, name_w, name_h, vp),
            cwd_line: row(CARD_CWD_Y),
            branch_line: row(CARD_BRANCH_Y),
            process: row(CARD_PROCESS_Y),
            updated: row(CARD_UPDATED_Y),
            dot: iclip(dot_x, name_y + (name_h - dot_d) / 2, dot_d, dot_d, vp),
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
        if self.menu_button_rect(bands.toolbar).contains(point) {
            return Some(SidebarHit::Menu);
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
        let menu_x = vp
            .w
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

/// One action in a card's `…` menu (FR-7). Only [`CardMenuItem::Close`] is wired
/// in v1 (see [`CARD_MENU_ITEMS`]); `Rename` names the store-level override that
/// exists and is tested (AC-9) but whose inline-text-input UI is deferred.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CardMenuItem {
    Close,
    Rename,
}

/// The card `…` menu's items, in top-to-bottom order (FR-7). v1 ships **Close
/// only** — a rename needs an inline text-input surface the sidebar does not
/// have, so its UI is a deliberate deferral rather than a menu entry that does
/// nothing (the store already supports [`crate::session_store::SessionDelta::Rename`]).
pub const CARD_MENU_ITEMS: [CardMenuItem; 1] = [CardMenuItem::Close];

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
    /// The toolbar `…` button: the header-level menu.
    Menu,
}

/// The text lines rendered on a card (FR-2/AC-3). `now` is a parameter (no
/// `Instant::now()`) so the relative-time line is pure and testable.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CardLines {
    /// `<icon glyph> <display name>` — the rename override shadows the title.
    pub name: String,
    /// The cwd row, tail-first truncated.
    pub cwd: String,
    /// The branch row; empty when the session has no branch.
    pub branch: String,
    /// The running-process row: the tty's foreground process name, or a shell
    /// state fallback (`running` / `idle`) where detection is unavailable. The
    /// caller styles it by the card's `busy` flag.
    pub process: String,
    /// Relative updated-time (`3分前` / `昨日 23:47` / …).
    pub updated: String,
}

/// Build a card's display lines from its state and the current wall clock. Each
/// field is its own line (name / cwd / branch / process / updated).
pub fn card_lines(card: &SessionCard, now: WallClock) -> CardLines {
    // The project icon is rendered from its own rect (`CardRects::icon`), so the
    // display name here carries no glyph prefix.
    let name = card.display_name().to_string();
    let cwd = truncate_tail(&card.cwd, CWD_MAX_CHARS);
    let branch = card.branch.clone().unwrap_or_default();
    // The detected foreground process name; when unavailable (non-macOS, or not
    // yet polled) fall back to the shell state so the row is never blank.
    let process = card
        .process
        .clone()
        .unwrap_or_else(|| if card.busy { "running".to_string() } else { "idle".to_string() });
    let updated = format_relative_time(now, card.updated_at);

    CardLines {
        name,
        cwd,
        branch,
        process,
        updated,
    }
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
/// matched on the executable basename.
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
    match base.as_str() {
        "claude" => AgentKind::ClaudeCode,
        "codex" => AgentKind::Codex,
        "agy" | "gemini" => AgentKind::Agy,
        _ => AgentKind::Generic,
    }
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

/// The header status label text (FR-5): `● Running` when any session is busy,
/// else `Idle`. Degraded per SHAPE — no real process name in v1.
pub fn header_status_label(busy_count: usize) -> String {
    if busy_count > 0 {
        "● Running".to_string()
    } else {
        "Idle".to_string()
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
    if visible && eligible {
        width_px
    } else {
        0.0
    }
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
            preview: plain_preview(&["line one", "line two"]),
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

    // 6 ids stacked; a viewport tall enough for exactly 3 cards. bounds height =
    // header(36) + toolbar(30) + 3*stride(120) = 426.
    fn six_id_bounds() -> (SidebarRect, Vec<SessionCardId>) {
        let ids: Vec<_> = (0..6).map(|p| card_id(1, p)).collect();
        (SidebarRect::new(0, 0, 360, 426), ids)
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
        let lines = card_lines(&card, wall(10, 3));

        // The name line is the display name (the icon renders from its own
        // rect, so no glyph prefix here); the icon glyph stays resolvable.
        assert_eq!(lines.name, "build");
        assert!(!icon_glyph(IconKind::Rust).is_empty());

        // The cwd row is tail-truncated (ellipsis) and the branch is its own row.
        assert!(lines.cwd.contains('…'));
        assert!(lines.cwd.contains("very-long-project"));
        assert_eq!(lines.branch, "main");

        // updated-time matches the pure PR1 formatter.
        assert_eq!(lines.updated, format_relative_time(wall(10, 3), wall(10, 0)));
        assert_eq!(lines.updated, "3分前");

        // The running-process row shows the detected foreground process.
        assert_eq!(lines.process, "cargo");
    }

    // Agent branding: known AI agents classify (case-insensitively, on the
    // basename); everything else is generic and keeps its raw name.
    #[test]
    fn classify_agent_maps_known_agents() {
        use AgentKind::*;
        for (input, expect) in [
            ("claude", ClaudeCode),
            ("Claude", ClaudeCode),
            ("CLAUDE", ClaudeCode),
            ("/usr/local/bin/claude", ClaudeCode),
            ("codex", Codex),
            ("Codex", Codex),
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
        assert_eq!(agent_display_name(classify_agent("claude"), "claude"), "Claude Code");
        assert_eq!(agent_display_name(classify_agent("codex"), "codex"), "Codex");
        assert_eq!(agent_display_name(classify_agent("gemini"), "gemini"), "agy");
        assert_eq!(agent_display_name(classify_agent("zsh"), "zsh"), "zsh");
    }

    #[test]
    fn card_lines_omit_branch_when_absent() {
        let card = sample_card("shell", "/repo", None);
        let lines = card_lines(&card, wall(10, 0));
        assert_eq!(lines.cwd, "/repo");
        assert_eq!(lines.branch, "");
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
            preview: Vec::new(),
        });
        let idle = card_lines(store.get(&id).unwrap(), wall(10, 0));
        assert_eq!(idle.process, "idle");

        store.apply(SessionDelta::Upsert {
            id,
            seq: 2,
            name: "shell".to_string(),
            cwd: "/repo".to_string(),
            busy: true,
            updated_at: wall(10, 0),
            preview: Vec::new(),
        });
        let busy = card_lines(store.get(&id).unwrap(), wall(10, 0));
        assert_eq!(busy.process, "running");
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

        // Toolbar `+` and `…`.
        let plus = m1().new_button_rect(bands.toolbar);
        let plus_pt = Point::new(plus.x + 2, plus.y + 2);
        assert_eq!(
            m1().hit_test(bounds, &ids, 0, plus_pt),
            Some(SidebarHit::NewSession)
        );
        let menu = m1().menu_button_rect(bands.toolbar);
        let menu_pt = Point::new(menu.x + 2, menu.y + 2);
        assert_eq!(
            m1().hit_test(bounds, &ids, 0, menu_pt),
            Some(SidebarHit::Menu)
        );

        // Header band (above the toolbar): a miss.
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

    #[test]
    fn hit_test_misses_the_gutter_between_cards() {
        let (bounds, ids) = six_id_bounds();
        let vp = m1().bands(bounds).viewport;
        // The gutter sits just below the first card (stride 100, card height 92).
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
        assert_eq!(m1().hit_test(bounds, &ids, 0, bottom_pt), Some(SidebarHit::Card(ids[2])));
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

    // FR-2 mockup parity: the status dot sits at the card's left edge (left of
    // the icon and name), and the updated-time rides the name row's right side,
    // just left of the invisible `…` hit region. The preview heading row falls
    // between the meta line and the two preview rows.
    #[test]
    fn card_rects_place_dot_left_and_updated_on_the_name_row() {
        let (bounds, ids) = six_id_bounds();
        let layout = m1().layout(bounds, &ids, 0);
        let card = &layout.cards[0];

        // Dot is the leftmost element, ahead of the icon and name.
        assert!(card.dot.x < card.icon.x);
        assert!(card.icon.x < card.name_line.x);

        // The name row runs full width up to the `…` hit region (updated-time
        // is no longer on it).
        assert!(card.name_line.right() <= card.menu_button.x);

        // One field per line, stacked top to bottom: name, cwd, branch,
        // process, updated.
        assert!(card.name_line.y < card.cwd_line.y);
        assert!(card.cwd_line.y < card.branch_line.y);
        assert!(card.branch_line.y < card.process.y);
        assert!(card.process.y < card.updated.y);
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
        // Stride stays exactly card + gutter at any scale.
        assert_eq!(m2.card_stride, m2.card_h + m2.card_gutter);
        assert_eq!(m2.card_stride, 2 * SIDEBAR_CARD_STRIDE);
        assert_eq!(m2.menu_item_h, 2 * SIDEBAR_MENU_ITEM_H);

        assert_eq!(SidebarMetrics::new(0.0).card_h, SIDEBAR_CARD_H);
        assert_eq!(SidebarMetrics::new(-2.0).card_h, SIDEBAR_CARD_H);
        assert_eq!(SidebarMetrics::new(f32::NAN).card_h, SIDEBAR_CARD_H);
    }

    // At scale 2.0 the bands, card height, and every interior row double, so a
    // Retina card keeps all five rows (name / meta / heading / preview×2) inside
    // its doubled height instead of clipping — and hit-test maps correctly
    // against the scaled geometry.
    #[test]
    fn layout_and_hit_test_scale_at_dpr_2() {
        let m2 = SidebarMetrics::new(2.0);
        let ids: Vec<_> = (0..6).map(|p| card_id(1, p)).collect();
        // header(72) + toolbar(60) + 3*stride(240) = 852, with a doubled width.
        let bounds = SidebarRect::new(0, 0, 720, 852);

        let bands = m2.bands(bounds);
        assert_eq!(bands.header.h, 2 * SIDEBAR_HEADER_H);
        assert_eq!(bands.toolbar.h, 2 * SIDEBAR_TOOLBAR_H);

        let layout = m2.layout(bounds, &ids, 0);
        assert_eq!(layout.cards.len(), 3);

        // The first card is a full doubled height and its five rows stack in
        // order, with the last (updated) row fitting inside the card.
        let card = &layout.cards[0];
        assert_eq!(card.bounds.h, m2.card_h);
        assert!(card.dot.x < card.icon.x && card.icon.x < card.name_line.x);
        assert!(card.name_line.y < card.cwd_line.y);
        assert!(card.cwd_line.y < card.branch_line.y);
        assert!(card.branch_line.y < card.process.y);
        assert!(card.process.y < card.updated.y);
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
        let menu_x = vp.w - m2.s(CARD_PAD) - m2.s(CARD_MENU_W);
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
    fn header_rects_place_status_title_and_pill_left_to_right() {
        let header = SidebarRect::new(0, 0, 360, SIDEBAR_HEADER_H);
        let h = m1().header_rects(header);
        assert!(h.status_label.x < h.title.x);
        assert!(h.title.x < h.name_pill.x);
        // Pill is right-aligned within the header.
        assert!(h.name_pill.right() <= header.right());
    }

    #[test]
    fn bands_clamp_without_underflow_in_a_short_sidebar() {
        // Shorter than the header alone: toolbar and viewport collapse to zero.
        let bands = m1().bands(SidebarRect::new(0, 0, 200, 20));
        assert_eq!(bands.header.h, 20);
        assert_eq!(bands.toolbar.h, 0);
        assert_eq!(bands.viewport.h, 0);
    }

    #[test]
    fn header_status_label_reflects_busy_count() {
        assert_eq!(header_status_label(0), "Idle");
        assert_eq!(header_status_label(2), "● Running");
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
        assert_eq!(sidebar_inset(true, is_sidebar_eligible(false), width), width);
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
