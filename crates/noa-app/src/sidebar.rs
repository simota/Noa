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

use crate::session_store::{IconKind, SessionCard, SessionCardId, WallClock, format_relative_time};

/// Height of the top header band (status label / center title / name pill,
/// FR-5). Compile-time constant — no config knob (⚠G precedent, mirroring
/// `tab_overview.rs`'s fixed chrome bands).
pub const SIDEBAR_HEADER_H: u32 = 44;

/// Height of the toolbar band holding the `+` (new session) and `…` (menu)
/// buttons below the header.
pub const SIDEBAR_TOOLBAR_H: u32 = 36;

/// Height of one session card.
pub const SIDEBAR_CARD_H: u32 = 92;

/// Vertical gap between adjacent cards.
pub const SIDEBAR_CARD_GUTTER: u32 = 8;

/// Vertical stride from one card's top to the next (card + gutter).
pub const SIDEBAR_CARD_STRIDE: u32 = SIDEBAR_CARD_H + SIDEBAR_CARD_GUTTER;

/// Max characters of a cwd shown in the meta line before tail-first truncation.
const CWD_MAX_CHARS: usize = 28;

// Card interior metrics (all compile-time; see `card_rects`/`card_lines`).
const CARD_PAD: u32 = 12;
const CARD_ICON_W: u32 = 22;
const CARD_DOT_D: u32 = 10;
const CARD_MENU_W: u32 = 22;
const CARD_UPDATED_W: u32 = 72;
const CARD_LINE_H: u32 = 16;
const CARD_NAME_H: u32 = 20;

// Toolbar `+` / `…` button metrics.
const TOOLBAR_BUTTON_W: u32 = 28;
const TOOLBAR_BUTTON_H: u32 = 24;

// Header line height for the status / title / pill rects.
const HEADER_LINE_H: u32 = 20;

/// The three horizontal bands the sidebar is split into: a fixed top header, a
/// fixed toolbar (`+` / `…`), and the scrolling card viewport below.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SidebarBands {
    pub header: SidebarRect,
    pub toolbar: SidebarRect,
    pub viewport: SidebarRect,
}

/// Carve `bounds` into the header / toolbar / viewport bands. Each band height
/// clamps so a very short sidebar degrades to a zero-height viewport instead of
/// underflowing (mirrors `overview_chrome_bands`).
pub fn sidebar_bands(bounds: SidebarRect) -> SidebarBands {
    let header_h = SIDEBAR_HEADER_H.min(bounds.h);
    let after_header = bounds.h - header_h;
    let toolbar_h = SIDEBAR_TOOLBAR_H.min(after_header);
    let viewport_h = after_header - toolbar_h;

    SidebarBands {
        header: SidebarRect::new(bounds.x, bounds.y, bounds.w, header_h),
        toolbar: SidebarRect::new(bounds.x, bounds.y + header_h, bounds.w, toolbar_h),
        viewport: SidebarRect::new(bounds.x, bounds.y + header_h + toolbar_h, bounds.w, viewport_h),
    }
}

/// Header rects (FR-5): a left status label, a centered title, and a
/// right-aligned session-name pill. Geometry only — the caller renders text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HeaderRects {
    pub status_label: SidebarRect,
    pub title: SidebarRect,
    pub name_pill: SidebarRect,
}

/// Lay out the header band's three rects. Widths are proportional so the title
/// keeps the middle regardless of sidebar width; all rects clamp to the band.
pub fn header_rects(header: SidebarRect) -> HeaderRects {
    let line_h = HEADER_LINE_H.min(header.h);
    let cy = header.y + (header.h - line_h) / 2;
    let status_w = (header.w * 35 / 100).min(header.w);
    let pill_w = (header.w * 30 / 100).min(header.w);

    let status_x = header.x + CARD_PAD.min(header.w);
    let status_label = SidebarRect::new(status_x, cy, status_w, line_h);
    let pill_x = header
        .right()
        .saturating_sub(CARD_PAD)
        .saturating_sub(pill_w);
    let name_pill = SidebarRect::new(pill_x, cy, pill_w, line_h);

    let title_x = status_label.right() + 6;
    let title_w = name_pill.x.saturating_sub(title_x).saturating_sub(6);
    let title = SidebarRect::new(title_x, cy, title_w, line_h);

    HeaderRects {
        status_label,
        title,
        name_pill,
    }
}

/// The `+` (new session) button rect in the toolbar band, pinned to the right.
pub fn new_button_rect(toolbar: SidebarRect) -> SidebarRect {
    let h = TOOLBAR_BUTTON_H.min(toolbar.h);
    let y = toolbar.y + (toolbar.h - h) / 2;
    let x = toolbar
        .right()
        .saturating_sub(CARD_PAD)
        .saturating_sub(TOOLBAR_BUTTON_W);
    SidebarRect::new(x, y, TOOLBAR_BUTTON_W.min(toolbar.w), h)
}

/// The header-level `…` menu button rect, immediately left of the `+` button.
pub fn menu_button_rect(toolbar: SidebarRect) -> SidebarRect {
    let plus = new_button_rect(toolbar);
    let x = plus.x.saturating_sub(6).saturating_sub(TOOLBAR_BUTTON_W);
    SidebarRect::new(x, plus.y, TOOLBAR_BUTTON_W.min(toolbar.w), plus.h)
}

/// Total scrollable content height for `card_count` stacked cards.
pub fn content_height(card_count: usize) -> u32 {
    (card_count as u32).saturating_mul(SIDEBAR_CARD_STRIDE)
}

/// Clamp a scroll offset to `[0, max]`, where `max = content_h - viewport_h`
/// (0 when the content fits, FR-15/AC-23). The `0` floor is implicit in `u32`.
pub fn clamp_scroll(offset: u32, content_h: u32, viewport_h: u32) -> u32 {
    let max = content_h.saturating_sub(viewport_h);
    offset.min(max)
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
    pub meta_line: SidebarRect,
    pub preview: [SidebarRect; 2],
    pub dot: SidebarRect,
    pub updated: SidebarRect,
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

/// Lay out the sidebar: header/toolbar chrome plus every card that is at least
/// partially visible after applying `scroll_offset` (clamped). Cards fully
/// scrolled out of the viewport are omitted; partly-visible cards have their
/// rects clipped to the viewport.
pub fn sidebar_layout(
    bounds: SidebarRect,
    ids: &[SessionCardId],
    scroll_offset: u32,
) -> SidebarLayout {
    let bands = sidebar_bands(bounds);
    let content_h = content_height(ids.len());
    let scroll = clamp_scroll(scroll_offset, content_h, bands.viewport.h);
    let vp = bands.viewport;

    let mut cards = Vec::new();
    for (index, &id) in ids.iter().enumerate() {
        // Window-space top of this card (may fall above/below the viewport).
        let top = vp.y as i64 + (index as i64) * SIDEBAR_CARD_STRIDE as i64 - scroll as i64;
        let bottom = top + SIDEBAR_CARD_H as i64;
        // Skip cards entirely outside the viewport.
        if bottom <= vp.y as i64 || top >= vp.bottom() as i64 {
            continue;
        }
        cards.push(card_rects(id, top, vp));
    }

    SidebarLayout {
        header: header_rects(bands.header),
        new_button: new_button_rect(bands.toolbar),
        menu_button: menu_button_rect(bands.toolbar),
        viewport: vp,
        cards,
        content_h,
    }
}

/// Build one card's clipped sub-rects from its (possibly off-viewport) window
/// top `top` and the viewport `vp`.
fn card_rects(id: SessionCardId, top: i64, vp: SidebarRect) -> CardRects {
    let lx = vp.x as i64;
    let w = vp.w as i64;
    let pad = CARD_PAD as i64;

    let menu_x = lx + w - pad - CARD_MENU_W as i64;
    let dot_x = menu_x - 6 - CARD_DOT_D as i64;
    let name_x = lx + pad + CARD_ICON_W as i64 + 6;
    let name_w = dot_x - name_x - 6;
    let updated_x = lx + w - pad - CARD_UPDATED_W as i64;
    let meta_w = updated_x - (lx + pad) - 6;
    let body_w = w - 2 * pad;

    let name_y = top + 8;
    let meta_y = top + 32;
    let preview0_y = top + 50;
    let preview1_y = top + 68;

    CardRects {
        id,
        bounds: iclip(lx, top, w, SIDEBAR_CARD_H as i64, vp),
        icon: iclip(lx + pad, name_y, CARD_ICON_W as i64, CARD_NAME_H as i64, vp),
        name_line: iclip(name_x, name_y, name_w, CARD_NAME_H as i64, vp),
        meta_line: iclip(lx + pad, meta_y, meta_w, CARD_LINE_H as i64, vp),
        preview: [
            iclip(lx + pad, preview0_y, body_w, CARD_LINE_H as i64, vp),
            iclip(lx + pad, preview1_y, body_w, CARD_LINE_H as i64, vp),
        ],
        dot: iclip(dot_x, name_y + 5, CARD_DOT_D as i64, CARD_DOT_D as i64, vp),
        updated: iclip(updated_x, meta_y, CARD_UPDATED_W as i64, CARD_LINE_H as i64, vp),
        menu_button: iclip(menu_x, top + 6, CARD_MENU_W as i64, CARD_NAME_H as i64, vp),
    }
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

/// What a click on the sidebar resolves to (FR-3/FR-6/FR-7). A miss returns
/// `None` from [`sidebar_hit_test`].
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

/// Resolve a click at `point` against the sidebar (FR-3/AC-4). Checks the
/// toolbar buttons first, then the scrolling card region: within a card, the
/// per-card `…` button wins over the card body (callers need not fall back like
/// the overview's separate close hit-test). Returns `None` for the header,
/// gutters between cards, or any point outside the sidebar.
pub fn sidebar_hit_test(
    bounds: SidebarRect,
    ids: &[SessionCardId],
    scroll_offset: u32,
    point: Point,
) -> Option<SidebarHit> {
    let bands = sidebar_bands(bounds);

    if new_button_rect(bands.toolbar).contains(point) {
        return Some(SidebarHit::NewSession);
    }
    if menu_button_rect(bands.toolbar).contains(point) {
        return Some(SidebarHit::Menu);
    }

    let vp = bands.viewport;
    if !vp.contains(point) {
        return None;
    }

    let content_h = content_height(ids.len());
    let scroll = clamp_scroll(scroll_offset, content_h, vp.h);

    // Translate the point into scroll-content space (origin at the viewport
    // top-left, offset by the scroll). Both terms are non-negative because the
    // point is inside the viewport.
    let cx = point.x - vp.x;
    let cy = (point.y - vp.y) + scroll;

    let index = (cy / SIDEBAR_CARD_STRIDE) as usize;
    if index >= ids.len() || cy % SIDEBAR_CARD_STRIDE >= SIDEBAR_CARD_H {
        // Past the last card, or in the gutter between two cards.
        return None;
    }
    let id = ids[index];

    // Per-card `…` button, in content space (same math as `card_rects`).
    let card_top = index as u32 * SIDEBAR_CARD_STRIDE;
    let menu_x = vp.w.saturating_sub(CARD_PAD).saturating_sub(CARD_MENU_W);
    let menu = SidebarRect::new(menu_x, card_top + 6, CARD_MENU_W, CARD_NAME_H);
    if menu.contains(Point::new(cx, cy)) {
        return Some(SidebarHit::CardMenu(id));
    }
    Some(SidebarHit::Card(id))
}

/// The text lines rendered on a card (FR-2/AC-3). `now` is a parameter (no
/// `Instant::now()`) so the relative-time line is pure and testable.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CardLines {
    /// `<icon glyph> <display name>` — the rename override shadows the title.
    pub name: String,
    /// `<cwd, tail-first truncated> <branch?>` — branch omitted when absent.
    pub meta: String,
    /// Up to two last-output preview lines.
    pub preview: Vec<String>,
    /// Relative updated-time (`3分前` / `昨日 23:47` / …).
    pub updated: String,
}

/// Build a card's display lines from its state and the current wall clock.
pub fn card_lines(card: &SessionCard, now: WallClock) -> CardLines {
    let name = format!("{} {}", icon_glyph(card.icon), card.display_name());
    let cwd = truncate_tail(&card.cwd, CWD_MAX_CHARS);
    let meta = match card.branch.as_deref() {
        Some(branch) if !branch.is_empty() => format!("{cwd}  {branch}"),
        _ => cwd,
    };
    let preview = card.preview.iter().take(2).cloned().collect();
    let updated = format_relative_time(now, card.updated_at);

    CardLines {
        name,
        meta,
        preview,
        updated,
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

/// Project-icon glyph for a card (FR-9). ASCII placeholders by default to avoid
/// font-fallback tofu (same manual-verify caveat as
/// `tab_overview::title_bar_row_with_close`); swap to Nerd Font glyphs at this
/// one site once they are confirmed to render, and record the deviation.
fn icon_glyph(icon: IconKind) -> &'static str {
    match icon {
        IconKind::Rust => "[rs]",
        IconKind::Node => "[js]",
        IconKind::Terraform => "[tf]",
        IconKind::Go => "[go]",
        IconKind::Python => "[py]",
        IconKind::Git => "[git]",
        IconKind::Folder => "[dir]",
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
    use crate::session_store::{SessionDelta, SessionStore, SessionWindowId};
    use crate::split_tree::PaneId;

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
            preview: vec!["line one".to_string(), "line two".to_string()],
        });
        store.apply(SessionDelta::Branch {
            id,
            branch: branch.map(str::to_string),
            icon: IconKind::Rust,
        });
        store.get(&id).unwrap().clone()
    }

    // 6 ids stacked; a viewport tall enough for exactly 3 cards. bounds height =
    // header(44) + toolbar(36) + 3*stride(300) = 380.
    fn six_id_bounds() -> (SidebarRect, Vec<SessionCardId>) {
        let ids: Vec<_> = (0..6).map(|p| card_id(1, p)).collect();
        (SidebarRect::new(0, 0, 360, 380), ids)
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

        // `[icon] name`: the icon glyph prefixes the display name.
        assert!(lines.name.starts_with(icon_glyph(IconKind::Rust)));
        assert!(lines.name.contains("build"));

        // `cwd … branch`: the long cwd is tail-truncated (ellipsis) and the
        // branch follows it.
        assert!(lines.meta.contains('…'));
        assert!(lines.meta.contains("very-long-project"));
        assert!(lines.meta.contains("main"));

        // updated-time matches the pure PR1 formatter.
        assert_eq!(lines.updated, format_relative_time(wall(10, 3), wall(10, 0)));
        assert_eq!(lines.updated, "3分前");
    }

    #[test]
    fn card_lines_omit_branch_when_absent() {
        let card = sample_card("shell", "/repo", None);
        let lines = card_lines(&card, wall(10, 0));
        assert_eq!(lines.meta, "/repo");
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
        let bands = sidebar_bands(bounds);
        let vp = bands.viewport;

        // First card body (name area, left of the `…` button).
        let body = Point::new(vp.x + 100, vp.y + 30);
        assert_eq!(
            sidebar_hit_test(bounds, &ids, 0, body),
            Some(SidebarHit::Card(ids[0]))
        );

        // First card `…` button (top-right corner of the card).
        let menu_x = vp.w - CARD_PAD - CARD_MENU_W;
        let card_menu = Point::new(vp.x + menu_x + 5, vp.y + 11);
        assert_eq!(
            sidebar_hit_test(bounds, &ids, 0, card_menu),
            Some(SidebarHit::CardMenu(ids[0]))
        );

        // Toolbar `+` and `…`.
        let plus = new_button_rect(bands.toolbar);
        let plus_pt = Point::new(plus.x + 2, plus.y + 2);
        assert_eq!(
            sidebar_hit_test(bounds, &ids, 0, plus_pt),
            Some(SidebarHit::NewSession)
        );
        let menu = menu_button_rect(bands.toolbar);
        let menu_pt = Point::new(menu.x + 2, menu.y + 2);
        assert_eq!(
            sidebar_hit_test(bounds, &ids, 0, menu_pt),
            Some(SidebarHit::Menu)
        );

        // Header band (above the toolbar): a miss.
        assert_eq!(
            sidebar_hit_test(bounds, &ids, 0, Point::new(bounds.x + 100, 10)),
            None
        );
        // Outside the sidebar entirely: a miss.
        assert_eq!(
            sidebar_hit_test(bounds, &ids, 0, Point::new(10_000, 10_000)),
            None
        );
    }

    #[test]
    fn hit_test_misses_the_gutter_between_cards() {
        let (bounds, ids) = six_id_bounds();
        let vp = sidebar_bands(bounds).viewport;
        // The gutter sits just below the first card (stride 100, card height 92).
        let gutter = Point::new(vp.x + 40, vp.y + SIDEBAR_CARD_H + 3);
        assert_eq!(sidebar_hit_test(bounds, &ids, 0, gutter), None);
    }

    // AC-23 (FR-15): with more cards than fit, scroll clamps to [0, max] and the
    // first/last cards are each reachable at the extremes.
    #[test]
    fn scroll_clamp_bounds_and_endpoints() {
        let content = content_height(6); // 600
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
        let vp = sidebar_bands(bounds).viewport;
        let content = content_height(ids.len());
        let max = clamp_scroll(u32::MAX, content, vp.h);

        // At the top, the first card is reachable near the viewport top.
        let top_pt = Point::new(vp.x + 60, vp.y + 20);
        assert_eq!(
            sidebar_hit_test(bounds, &ids, 0, top_pt),
            Some(SidebarHit::Card(ids[0]))
        );

        // At the bottom, the last card is reachable near the viewport bottom.
        let bottom_pt = Point::new(vp.x + 60, vp.bottom() - 20);
        assert_eq!(
            sidebar_hit_test(bounds, &ids, max, bottom_pt),
            Some(SidebarHit::Card(ids[5]))
        );

        // The last card is NOT reachable while scrolled to the top.
        assert_eq!(sidebar_hit_test(bounds, &ids, 0, bottom_pt), Some(SidebarHit::Card(ids[2])));
    }

    // AC-20 (NFR-4, FR-15): the layout stacks cards, skips fully-scrolled-out
    // ones, and keeps every emitted rect inside the viewport.
    #[test]
    fn layout_stacks_visible_cards_within_the_viewport() {
        let (bounds, ids) = six_id_bounds();
        let vp = sidebar_bands(bounds).viewport;

        let layout = sidebar_layout(bounds, &ids, 0);
        assert_eq!(layout.content_h, content_height(6));
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
        let vp = sidebar_bands(bounds).viewport;
        let max = clamp_scroll(u32::MAX, content_height(ids.len()), vp.h);

        let layout = sidebar_layout(bounds, &ids, max);
        let shown: Vec<_> = layout.cards.iter().map(|c| c.id).collect();
        assert_eq!(shown, vec![ids[3], ids[4], ids[5]]);
    }

    #[test]
    fn layout_over_scroll_is_clamped_like_clamp_scroll() {
        let (bounds, ids) = six_id_bounds();
        // An absurd offset resolves to the same layout as the clamped maximum.
        let clamped = sidebar_layout(bounds, &ids, 1_000_000);
        let vp = sidebar_bands(bounds).viewport;
        let max = clamp_scroll(u32::MAX, content_height(ids.len()), vp.h);
        assert_eq!(clamped.cards, sidebar_layout(bounds, &ids, max).cards);
    }

    #[test]
    fn header_rects_place_status_title_and_pill_left_to_right() {
        let header = SidebarRect::new(0, 0, 360, SIDEBAR_HEADER_H);
        let h = header_rects(header);
        assert!(h.status_label.x < h.title.x);
        assert!(h.title.x < h.name_pill.x);
        // Pill is right-aligned within the header.
        assert!(h.name_pill.right() <= header.right());
    }

    #[test]
    fn bands_clamp_without_underflow_in_a_short_sidebar() {
        // Shorter than the header alone: toolbar and viewport collapse to zero.
        let bands = sidebar_bands(SidebarRect::new(0, 0, 200, 20));
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
