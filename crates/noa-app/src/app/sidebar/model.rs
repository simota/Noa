use super::*;

impl App {
    /// The manual title of the tab hosting `card`, if one is set (tab-title
    /// REQ-TTL-11). Cards are per pane; the override is per tab, so every
    /// card of a split tab reflects the same title.
    pub(in crate::app) fn tab_title_override_for_card(
        &self,
        card: &SessionCardId,
    ) -> Option<String> {
        self.windows
            .get(&WindowId::from(card.window_id.0))?
            .title_override
            .clone()
    }

    /// Build the per-frame sidebar draw model for `window_id` (FR-2/FR-5), or
    /// `None` when the window has no visible sidebar. Reads only the store and
    /// the pure layout — never a `Terminal` (AC-17). Computed before the redraw
    /// path borrows `gpu`/`state` mutably, so the drawer can run inline.
    pub(in crate::app) fn sidebar_draw_model(
        &self,
        window_id: WindowId,
    ) -> Option<SidebarDrawModel> {
        let inset = self.window_sidebar_inset_px(window_id);
        if inset == 0 {
            return None;
        }
        let gpu = self.gpu.as_ref()?;
        let state = self.windows.get(&window_id)?;
        // The sidebar rasterizes with its own dedicated, smaller font, so cell
        // placement uses that font's metrics (not the terminal font's).
        let metrics = gpu.sidebar_font.metrics();
        // Folds in `sidebar_font_zoom()` so chrome details drawn at this
        // scale (drop indicator, borders, radii, glyph size) zoom coherently
        // with the cards laid out by `sidebar_metrics()` — the same factor,
        // applied at these two choke points only.
        let scale = state.window.scale_factor() as f32 * self.sidebar_font_zoom();
        let layout_metrics = self.sidebar_metrics(window_id);
        let height = state.window.inner_size().height.max(1);
        let band = PaneRectApp::new(0, 0, inset, height);
        let grid = grid_size_for_pane_rect(band, metrics, self.padding);

        let bounds = self.sidebar_layout_bounds(window_id, inset);
        let windows = self.session_windows_for_window(window_id);
        let ids = self.session_store.ordered_ids_for_windows(&windows);
        let layout = layout_metrics.layout(bounds, &ids, state.sidebar_scroll);

        // Pixel → cell conversion, matching where a `Renderer` places cell (0,0):
        // at the padding origin. One closure per grid (band / card / menu).
        let cell_w = metrics.cell_w.max(1.0);
        let cell_h = metrics.cell_h.max(1.0);
        let pad_left = self.padding.left;
        let pad_top = self.padding.top;
        let to_cell = |grid: GridSize| {
            move |x: u32, y: u32| px_to_cell(x, y, pad_left, pad_top, cell_w, cell_h, grid)
        };
        let band_cell = to_cell(grid);

        let mut runs: Vec<SidebarTextRun> = Vec::new();
        let selected_id = Self::session_card_id(window_id, state.focused_pane);

        // The top header band (status label / center title / name pill) was
        // removed as redundant, and the dead `…` header menu (no v1 action) was
        // dropped — the toolbar's sole `+` action now sits at the sidebar's
        // top-right (SIDEBAR_HEADER_H is collapsed to 0). The `+` is drawn as its
        // own rounded chrome tile (see `draw_sidebar_band`), not baked into the
        // flat band, so it can carry a border + hover state like a real button.
        let btn = layout.new_button;
        let new_button_hover = state.sidebar_button_hover;

        // Card text on the flat backdrop, plus a transparent text overlay for
        // every fully-visible card (FR-2). Partially-scrolled cards stay on the
        // backdrop.
        let now = crate::localtime::wall_clock_now();
        let now_instant = Instant::now();
        let home = home_dir();
        let theme = active_theme(&gpu.theme, &gpu.preview_theme);
        let palette = &theme.palette;
        let panel_bg = theme.default_bg;
        let mut cards: Vec<SidebarCardDraw> = Vec::new();
        // The card texture uses the same width as the laid-out row. The margin
        // is currently zero for a seamless list, but stays in the metric so the
        // pure layout remains the single source of geometry.
        let card_w = layout_metrics.card_w(inset).max(1);
        let card_band = PaneRectApp::new(0, 0, card_w, layout_metrics.card_h);
        let card_grid = grid_size_for_pane_rect(card_band, metrics, self.padding);
        let card_cell = to_cell(card_grid);
        // The card being drag-reordered is drawn as a floating copy below (2b),
        // so its static slot is left as a bare backdrop gap — the affordance that
        // the card has lifted out of the list.
        let dragging_id = state.sidebar_drag.filter(|d| d.active).map(|d| d.card);
        let hovered_id = state.sidebar_card_hover;
        for card_rects in &layout.cards {
            if Some(card_rects.id) == dragging_id {
                continue;
            }
            let Some(card) = self.session_store.get(&card_rects.id) else {
                continue;
            };
            let tab_title = self.tab_title_override_for_card(&card_rects.id);
            let lines: CardLines = card_lines(card, now, home, tab_title.as_deref());
            let marker = self.attention_marker_visible(&card_rects.id);
            let renaming = self
                .sidebar_rename
                .as_ref()
                .filter(|session| session.window_id == window_id && session.card == card_rects.id)
                .map(|session| {
                    // Live IME composition appends to the displayed buffer
                    // (display only — it joins the real buffer on commit).
                    format!(
                        "{}{}",
                        session.buffer,
                        self.modal_preedit_for(window_id, ModalImeTarget::SidebarRename)
                    )
                });
            let renaming = renaming.as_deref();
            // The `…` glyph surfaces on the hovered card (discoverability) and
            // stays visible while its menu popup is open.
            let menu_hint =
                hovered_id == Some(card_rects.id) || state.sidebar_menu == Some(card_rects.id);
            let full = card_rects.bounds.h == layout_metrics.card_h;
            // Fully-visible cards draw text through their transparent overlay;
            // only partial edge-clipped cards emit text directly to the band.
            if !full {
                emit_card_text(
                    &mut runs, card_rects, card, &lines, &band_cell, marker, palette, renaming,
                    menu_hint,
                );
            }

            if full {
                let selected = card_rects.id == selected_id;
                let auto_flash = self
                    .auto_approve_flash_until
                    .get(&card_rects.id)
                    .is_some_and(|until| now_instant < *until);
                let local = layout_metrics.card_local_rects(card_rects.id, card_w);
                let mut card_runs = Vec::new();
                emit_card_text(
                    &mut card_runs,
                    &local,
                    card,
                    &lines,
                    &card_cell,
                    marker,
                    palette,
                    renaming,
                    menu_hint,
                );
                let selected_bg = sidebar_selected_card_bg(panel_bg);
                cards.push(SidebarCardDraw {
                    rect: card_rects.bounds,
                    grid: card_grid,
                    bg: if auto_flash {
                        sidebar_auto_flash_card_bg(panel_bg)
                    } else if selected {
                        selected_bg
                    } else if hovered_id == Some(card_rects.id) {
                        sidebar_hover_card_bg(panel_bg)
                    } else {
                        sidebar_card_bg(panel_bg)
                    },
                    selected,
                    attention: card.attention,
                    accent: card_accent(card, marker),
                    runs: card_runs,
                });
            }
        }

        // Card `…` menu popup (FR-7): its own overlay, composited above the cards
        // so a card row can never hide it. Skipped when the open card has
        // scrolled out of view or the popup would spill past the window bottom.
        let menu = state.sidebar_menu.and_then(|open| {
            let card_rects = layout.cards.iter().find(|c| c.id == open)?;
            let popup = layout_metrics.card_menu_popup_rect(
                card_rects.menu_button,
                CARD_MENU_ITEMS.len(),
                inset,
            );
            if popup.w == 0 || popup.h == 0 || popup.bottom() > height {
                return None;
            }
            let menu_band = PaneRectApp::new(0, 0, popup.w, popup.h);
            let menu_grid = grid_size_for_pane_rect(menu_band, metrics, self.padding);
            let menu_cell = to_cell(menu_grid);
            let mut menu_runs = Vec::new();
            for (index, &item) in CARD_MENU_ITEMS.iter().enumerate() {
                let item_rect = layout_metrics.card_menu_item_rect(popup, index);
                let (col, row) = menu_cell(
                    item_rect.x.saturating_sub(popup.x),
                    item_rect.y.saturating_sub(popup.y),
                );
                menu_runs.push(SidebarTextRun::new(
                    col,
                    row,
                    format!(" {}", crate::sidebar::card_menu_label(item)),
                    chrome().fg,
                ));
            }
            Some(SidebarMenuDraw {
                rect: popup,
                grid: menu_grid,
                runs: menu_runs,
            })
        });

        // Active card drag (FR: reordering): float a copy of the dragged card
        // under the cursor and mark the insertion gap with an accent line. The
        // static copy stays in its slot (occluded by the float while overlapping)
        // so the list reads continuously; the line shows where a drop lands.
        let (dragging, drop_indicator) = state
            .sidebar_drag
            .filter(|drag| drag.active)
            .and_then(|drag| {
                let card = self.session_store.get(&drag.card)?;
                let tab_title = self.tab_title_override_for_card(&drag.card);
                let lines = card_lines(card, now, home, tab_title.as_deref());
                let marker = self.attention_marker_visible(&drag.card);
                let local = layout_metrics.card_local_rects(drag.card, card_w);
                let mut card_runs = Vec::new();
                emit_card_text(
                    &mut card_runs,
                    &local,
                    card,
                    &lines,
                    &card_cell,
                    marker,
                    palette,
                    None,
                    false,
                );
                // Floating top follows the cursor, clamped inside the band.
                let max_top = height.saturating_sub(layout_metrics.card_h) as i64;
                let top = (drag.current_y - drag.grab_dy).clamp(0, max_top) as u32;
                let float = SidebarCardDraw {
                    rect: SidebarRect::new(
                        layout_metrics.card_margin_x,
                        top,
                        card_w,
                        layout_metrics.card_h,
                    ),
                    grid: card_grid,
                    bg: sidebar_selected_card_bg(panel_bg),
                    selected: true,
                    attention: false,
                    accent: None,
                    runs: card_runs,
                };
                // Drop indicator at the target gap, spanning the card width with a
                // small horizontal inset so it reads as a rule, not a full band.
                let vp = layout.viewport;
                let py = drag.current_y.clamp(0, u32::MAX as i64) as u32;
                let idx = layout_metrics.drop_index(vp, ids.len(), state.sidebar_scroll, py);
                let indicator = layout_metrics
                    .drop_indicator_y(vp, ids.len(), state.sidebar_scroll, idx)
                    .map(|y| {
                        let line_h = (SIDEBAR_DROP_INDICATOR_H * scale).round().max(1.0) as u32;
                        let inset_x = (12.0 * scale).round() as u32;
                        let w = inset.saturating_sub(inset_x.saturating_mul(2)).max(1);
                        SidebarRect::new(inset_x, y.saturating_sub(line_h / 2), w, line_h)
                    });
                Some((Some(float), indicator))
            })
            .unwrap_or((None, None));

        Some(SidebarDrawModel {
            inset,
            height,
            scale,
            card_h: layout_metrics.card_h,
            card_w,
            grid,
            runs,
            new_button: btn,
            new_button_hover,
            cards,
            menu,
            dragging,
            drop_indicator,
            background_opacity: self.config.background_opacity,
        })
    }
}

/// The viewer's home directory, resolved once and cached: it drives every
/// card's cwd `~`-abbreviation and cannot change over the process lifetime, so
/// re-reading `HOME` on every redraw (once per frame) is pure overhead.
fn home_dir() -> Option<&'static str> {
    static HOME: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    HOME.get_or_init(|| std::env::var("HOME").ok()).as_deref()
}

/// Pixel → cell for a synthetic sidebar grid whose `Renderer` places cell (0,0)
/// at the padding origin, clamped into the grid.
fn px_to_cell(
    x: u32,
    y: u32,
    pad_left: f32,
    pad_top: f32,
    cell_w: f32,
    cell_h: f32,
    grid: GridSize,
) -> (u16, u16) {
    let col = ((x as f32 - pad_left) / cell_w).round().max(0.0) as u16;
    let row = ((y as f32 - pad_top) / cell_h).round().max(0.0) as u16;
    (
        col.min(grid.cols.saturating_sub(1)),
        row.min(grid.rows.saturating_sub(1)),
    )
}

/// A single-rect text run positioned via `to_cell`, or `None` for an empty rect
/// or text (so callers can `extend`).
fn window_run(
    to_cell: &impl Fn(u32, u32) -> (u16, u16),
    rect: SidebarRect,
    text: String,
    fg: Rgb,
    bold: bool,
) -> Option<SidebarTextRun> {
    if rect.w == 0 || rect.h == 0 || text.is_empty() {
        return None;
    }
    let (col, row) = to_cell(rect.x, rect.y);
    Some(SidebarTextRun {
        col,
        row,
        text,
        fg,
        bg: None,
        bold,
    })
}

/// A truecolor SGR foreground prefix, embeddable inside a run's text (the run
/// text is fed through a `Stream`, so inline escapes recolor mid-run).
fn sgr_fg(color: Rgb) -> String {
    format!("\x1b[38;2;{};{};{}m", color.r, color.g, color.b)
}

/// Resolve a preview span's pure `noa_core::Color` to a concrete sidebar RGB:
/// the theme palette for indexed colors, the raw value for truecolor, and the
/// sidebar's dim fg for the default (so uncolored output reads as secondary
/// text on the card).
fn resolve_preview_color(color: noa_core::Color, palette: &[Rgb; 256]) -> Rgb {
    match color {
        noa_core::Color::Default => chrome().dim_fg,
        noa_core::Color::Palette(index) => palette[index as usize],
        noa_core::Color::Rgb(rgb) => rgb,
    }
}

/// A text run right-aligned within `rect`: the run's column is backed off from
/// the rect's right edge by the text's display width (ASCII narrow, everything
/// else wide — enough for the updated-time strings this positions).
fn right_aligned_run(
    to_cell: &impl Fn(u32, u32) -> (u16, u16),
    rect: SidebarRect,
    text: String,
    fg: Rgb,
) -> Option<SidebarTextRun> {
    if rect.w == 0 || rect.h == 0 || text.is_empty() {
        return None;
    }
    let (left_col, row) = to_cell(rect.x, rect.y);
    let (right_col, _) = to_cell(rect.x + rect.w, rect.y);
    let width: u16 = text
        .chars()
        .map(|c| if c.is_ascii() { 1u16 } else { 2 })
        .sum();
    let col = right_col.saturating_sub(width).max(left_col);
    Some(SidebarTextRun {
        col,
        row,
        text,
        fg,
        bg: None,
        bold: false,
    })
}

/// Emit one card's text runs (status dot, project icon, bold name with the
/// right-aligned updated-time, the meta row `process · ⎇ branch · cwd`, and
/// the configured preview rows) through `to_cell`. Shared by the flat backdrop
/// (window coords) and each card overlay (card-local coords) so both agree on
/// layout. `renaming` carries the live rename buffer when this card's
/// inline rename is open — it replaces the name run with the buffer + caret in
/// the accent color. `menu_hint` surfaces the `…` glyph over its (always-live)
/// hit region — on for the hovered card and while the card's menu is open.
#[allow(clippy::too_many_arguments)]
fn emit_card_text(
    out: &mut Vec<SidebarTextRun>,
    rects: &CardRects,
    card: &SessionCard,
    lines: &CardLines,
    to_cell: &impl Fn(u32, u32) -> (u16, u16),
    attention_marker: bool,
    palette: &[Rgb; 256],
    renaming: Option<&str>,
    menu_hint: bool,
) {
    // The status dot is nf-fa-circle (U+F111), not `●` (U+25CF): the project
    // icon next to it is a Nerd Font glyph whose optical center sits at the
    // cell's vertical center, while `●` sits on the text baseline (~1.5px
    // lower at 14px) — drawing the dot from the same Nerd Font keeps the two
    // glyphs on the name row vertically aligned.
    out.extend(window_run(
        to_cell,
        rects.dot,
        "\u{f111}".to_string(),
        status_dot_rgb(effective_status_dot(card, attention_marker)),
        false,
    ));
    out.extend(window_run(
        to_cell,
        rects.icon,
        icon_glyph(card.icon).to_string(),
        icon_color(card.icon),
        false,
    ));
    let (name_text, name_fg) = match renaming {
        // Inline rename (FR-7): the buffer plus a caret, in the accent color.
        Some(buffer) => (format!("{buffer}▏"), chrome().accent),
        None => (lines.name.clone(), chrome().fg),
    };
    out.extend(window_run(
        to_cell,
        rects.name_line,
        name_text,
        name_fg,
        true,
    ));
    // Updated-time, right-aligned on the name row. Emitted after the name so
    // an overlong name's overflow cells are overwritten by the time, not the
    // other way around.
    out.extend(right_aligned_run(
        to_cell,
        rects.updated,
        lines.updated.clone(),
        chrome().dim_fg,
    ));
    // The `…` menu affordance (its hit region is always live; the glyph shows
    // on hover / while its menu is open). Emitted after the name/time for the
    // same overflow-overwrite reason.
    if menu_hint {
        out.extend(window_run(
            to_cell,
            rects.menu_button,
            "⋯".to_string(),
            chrome().dim_fg,
            false,
        ));
    }

    // Meta row: a recognized AI agent gets its brand glyph/color/name (busy or
    // idle); any other process shows green `✳` while running, dim `❯` while
    // idle. The git branch and the dim cwd follow on the same row. A pending
    // interaction request (FR-16) overrides the badge with the attention color
    // and appends the waiting label; the label is held steady while pending
    // (only the dot blinks, via `effective_status_dot`) so it stays legible.
    if rects.meta.w > 0 && rects.meta.h > 0 {
        let (badge, badge_fg) = process_badge(&lines.process, card.busy);
        let (badge, badge_fg) = if card.attention {
            (format!("{badge} · {ATTENTION_LABEL}"), chrome().dot_red)
        } else {
            (badge, badge_fg)
        };
        let mut text = String::new();
        let run_fg = if card.auto_approve_enabled {
            text.push_str("AUTO ON");
            text.push_str(&sgr_fg(chrome().dim_fg));
            text.push_str(" · ");
            text.push_str(&sgr_fg(badge_fg));
            text.push_str(&badge);
            chrome().accent
        } else {
            text.push_str(&badge);
            badge_fg
        };
        // The dim suffix (branch + cwd) is recolored inline; the run fg colors
        // the badge portion.
        if !lines.branch.is_empty() {
            text.push_str(&sgr_fg(chrome().dim_fg));
            text.push_str(&format!(" · ⎇ {}", lines.branch));
        }
        if !lines.cwd.is_empty() {
            text.push_str(&sgr_fg(chrome().dim_fg));
            text.push_str(&format!(" · {}", lines.cwd));
        }
        out.extend(window_run(to_cell, rects.meta, text, run_fg, false));
    }

    // The first preview row carries the auto-approve audit summary when
    // present; remaining rows show last-output preview in their original ANSI
    // colors. Rows the card has no preview line for stay blank.
    let audit = card.auto_approve_audit.back().map(|entry| {
        format!(
            "AUTO APPROVED {} · {} {}",
            card.auto_approve_audit.len(),
            entry.agent,
            entry.prompt
        )
    });
    let mut preview_index = 0;
    for (row_index, rect) in rects.preview.iter().enumerate() {
        if row_index == 0
            && let Some(audit) = audit.as_ref()
        {
            out.extend(window_run(
                to_cell,
                *rect,
                audit.clone(),
                chrome().accent,
                false,
            ));
            continue;
        }
        let Some(line) = card.preview.get(preview_index) else {
            continue;
        };
        preview_index += 1;
        let mut text = String::new();
        for span in line {
            text.push_str(&sgr_fg(resolve_preview_color(span.fg, palette)));
            text.push_str(&span.text);
        }
        let fg = line
            .first()
            .map(|span| resolve_preview_color(span.fg, palette))
            .unwrap_or(chrome().dim_fg);
        out.extend(window_run(to_cell, *rect, text, fg, false));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session_store::{PreviewSpan, SessionStore, WallClock};
    use noa_core::Color;

    fn wall(hour: u32, minute: u32) -> WallClock {
        WallClock {
            year: 2026,
            month: 7,
            day: 8,
            hour,
            minute,
        }
    }

    /// A 10x20-cell grid with no padding, so `px_to_cell(x, y)` is simply
    /// `(x / 10, y / 20)` clamped into the grid — a stable fixture for the
    /// positioning helpers.
    fn cell_at() -> impl Fn(u32, u32) -> (u16, u16) {
        move |x, y| {
            px_to_cell(
                x,
                y,
                0.0,
                0.0,
                10.0,
                20.0,
                GridSize {
                    cols: 100,
                    rows: 100,
                },
            )
        }
    }

    #[test]
    fn px_to_cell_maps_origin_and_rounds_to_nearest_cell() {
        assert_eq!(
            px_to_cell(
                0,
                0,
                0.0,
                0.0,
                10.0,
                20.0,
                GridSize {
                    cols: 100,
                    rows: 100
                }
            ),
            (0, 0)
        );
        // 24px / 10 = 2.4 → col 2; 30px / 20 = 1.5 → row 2 (round half up).
        assert_eq!(
            px_to_cell(
                24,
                30,
                0.0,
                0.0,
                10.0,
                20.0,
                GridSize {
                    cols: 100,
                    rows: 100
                }
            ),
            (2, 2)
        );
    }

    #[test]
    fn px_to_cell_applies_padding_origin() {
        // With an 8px left pad, x=18 sits one cell past the padding origin.
        assert_eq!(
            px_to_cell(
                18,
                0,
                8.0,
                0.0,
                10.0,
                20.0,
                GridSize {
                    cols: 100,
                    rows: 100
                }
            ),
            (1, 0)
        );
    }

    #[test]
    fn px_to_cell_clamps_to_grid_bounds() {
        let grid = GridSize { cols: 4, rows: 3 };
        assert_eq!(px_to_cell(9999, 9999, 0.0, 0.0, 10.0, 20.0, grid), (3, 2));
        // A negative logical position (x below the padding origin) clamps to 0.
        assert_eq!(px_to_cell(0, 0, 50.0, 50.0, 10.0, 20.0, grid), (0, 0));
    }

    #[test]
    fn window_run_rejects_empty_rect_or_text() {
        let to_cell = cell_at();
        assert!(
            window_run(
                &to_cell,
                SidebarRect::new(0, 0, 0, 20),
                "x".into(),
                Rgb::new(1, 2, 3),
                false
            )
            .is_none()
        );
        assert!(
            window_run(
                &to_cell,
                SidebarRect::new(0, 0, 10, 0),
                "x".into(),
                Rgb::new(1, 2, 3),
                false
            )
            .is_none()
        );
        assert!(
            window_run(
                &to_cell,
                SidebarRect::new(0, 0, 10, 20),
                String::new(),
                Rgb::new(1, 2, 3),
                false
            )
            .is_none()
        );
    }

    #[test]
    fn window_run_positions_and_carries_style() {
        let to_cell = cell_at();
        let run = window_run(
            &to_cell,
            SidebarRect::new(20, 40, 30, 20),
            "hi".into(),
            Rgb::new(9, 8, 7),
            true,
        )
        .unwrap();
        assert_eq!((run.col, run.row), (2, 2));
        assert_eq!(run.text, "hi");
        assert_eq!(run.fg, Rgb::new(9, 8, 7));
        assert!(run.bold);
        assert_eq!(run.bg, None);
    }

    #[test]
    fn right_aligned_run_backs_off_by_display_width() {
        let to_cell = cell_at();
        // Rect spans cols [1, 6); "12分前" is 3 wide (2) + 2 ascii = width 8?
        // Use a plain ASCII string for a deterministic width: "3m" → width 2.
        let run = right_aligned_run(
            &to_cell,
            SidebarRect::new(10, 0, 50, 20),
            "3m".into(),
            Rgb::new(1, 1, 1),
        )
        .unwrap();
        // Right edge is col 6 (x=60), backed off by width 2 → col 4.
        assert_eq!(run.col, 4);
        assert_eq!(run.row, 0);
    }

    #[test]
    fn right_aligned_run_counts_wide_chars_as_two() {
        let to_cell = cell_at();
        // "昨日" is two wide chars → display width 4.
        let run = right_aligned_run(
            &to_cell,
            SidebarRect::new(0, 0, 100, 20),
            "昨日".into(),
            Rgb::new(1, 1, 1),
        )
        .unwrap();
        // Right edge col 10 (x=100) minus width 4 → col 6.
        assert_eq!(run.col, 6);
    }

    #[test]
    fn right_aligned_run_clamps_to_left_edge_when_too_wide() {
        let to_cell = cell_at();
        // A string wider than the rect can't push the column below the left edge.
        let run = right_aligned_run(
            &to_cell,
            SidebarRect::new(20, 0, 20, 20),
            "wwwwwwww".into(),
            Rgb::new(1, 1, 1),
        )
        .unwrap();
        assert_eq!(run.col, 2); // left edge = x 20 → col 2
    }

    #[test]
    fn right_aligned_run_rejects_empty() {
        let to_cell = cell_at();
        assert!(
            right_aligned_run(
                &to_cell,
                SidebarRect::new(0, 0, 0, 20),
                "x".into(),
                Rgb::new(1, 1, 1)
            )
            .is_none()
        );
        assert!(
            right_aligned_run(
                &to_cell,
                SidebarRect::new(0, 0, 10, 20),
                String::new(),
                Rgb::new(1, 1, 1)
            )
            .is_none()
        );
    }

    #[test]
    fn resolve_preview_color_maps_each_color_kind() {
        let mut palette = [Rgb::new(0, 0, 0); 256];
        palette[5] = Rgb::new(50, 60, 70);
        assert_eq!(
            resolve_preview_color(Color::Default, &palette),
            chrome().dim_fg
        );
        assert_eq!(
            resolve_preview_color(Color::Palette(5), &palette),
            Rgb::new(50, 60, 70)
        );
        assert_eq!(
            resolve_preview_color(Color::Rgb(Rgb::new(1, 2, 3)), &palette),
            Rgb::new(1, 2, 3)
        );
    }

    #[test]
    fn sgr_fg_emits_truecolor_escape() {
        assert_eq!(sgr_fg(Rgb::new(10, 20, 30)), "\x1b[38;2;10;20;30m");
    }

    fn store_card(busy: bool, process: &str) -> SessionCard {
        let mut store = SessionStore::new();
        let id = SessionCardId::new(SessionWindowId(1), PaneId::new(0));
        store.apply(SessionDelta::Upsert {
            id,
            seq: 1,
            name: "build".to_string(),
            cwd: "/Users/dev/proj".to_string(),
            busy,
            updated_at: wall(10, 0),
            preview: Some(vec![vec![PreviewSpan {
                text: "output".to_string(),
                fg: Color::Default,
            }]]),
        });
        store.apply(SessionDelta::Process {
            id,
            process: Some(process.to_string()),
        });
        store.get(&id).unwrap().clone()
    }

    /// Non-zero sub-rects so every run region emits; one preview row.
    fn card_rects() -> CardRects {
        CardRects {
            id: SessionCardId::new(SessionWindowId(1), PaneId::new(0)),
            bounds: SidebarRect::new(0, 0, 200, 120),
            icon: SidebarRect::new(10, 0, 20, 20),
            name_line: SidebarRect::new(30, 0, 120, 20),
            meta: SidebarRect::new(10, 40, 180, 20),
            preview: vec![SidebarRect::new(10, 60, 180, 20)],
            updated: SidebarRect::new(120, 0, 60, 20),
            dot: SidebarRect::new(0, 0, 10, 20),
            menu_button: SidebarRect::new(180, 0, 20, 20),
        }
    }

    #[test]
    fn emit_card_text_emits_bold_name_run() {
        let card = store_card(false, "cargo");
        let lines = card_lines(&card, wall(10, 3), None, None);
        let palette = [Rgb::new(0, 0, 0); 256];
        let to_cell = cell_at();
        let mut out = Vec::new();
        emit_card_text(
            &mut out,
            &card_rects(),
            &card,
            &lines,
            &to_cell,
            false,
            &palette,
            None,
            false,
        );

        let name = out.iter().find(|r| r.text == "build").expect("name run");
        assert!(name.bold, "the card name renders bold");
        assert_eq!(name.fg, chrome().fg);
    }

    #[test]
    fn emit_card_text_shows_updated_time_when_idle() {
        let card = store_card(false, "cargo");
        let lines = card_lines(&card, wall(10, 3), None, None);
        assert!(
            !lines.updated.is_empty(),
            "idle card carries a relative time"
        );
        let palette = [Rgb::new(0, 0, 0); 256];
        let to_cell = cell_at();
        let mut out = Vec::new();
        emit_card_text(
            &mut out,
            &card_rects(),
            &card,
            &lines,
            &to_cell,
            false,
            &palette,
            None,
            false,
        );
        assert!(
            out.iter().any(|r| r.text == lines.updated),
            "the updated-time run is emitted for an idle card"
        );
    }

    #[test]
    fn emit_card_text_hides_updated_time_when_busy() {
        let card = store_card(true, "cargo");
        let lines = card_lines(&card, wall(10, 3), None, None);
        assert!(lines.updated.is_empty(), "a busy card has no relative time");
        let palette = [Rgb::new(0, 0, 0); 256];
        let to_cell = cell_at();
        let mut out = Vec::new();
        emit_card_text(
            &mut out,
            &card_rects(),
            &card,
            &lines,
            &to_cell,
            false,
            &palette,
            None,
            false,
        );
        // With an empty updated string, no right-aligned time run is produced.
        assert!(
            out.iter().all(|r| !r.text.is_empty()),
            "no empty-text run is emitted"
        );
    }

    #[test]
    fn emit_card_text_renaming_replaces_name_with_caret_in_accent() {
        let card = store_card(false, "cargo");
        let lines = card_lines(&card, wall(10, 3), None, None);
        let palette = [Rgb::new(0, 0, 0); 256];
        let to_cell = cell_at();
        let mut out = Vec::new();
        emit_card_text(
            &mut out,
            &card_rects(),
            &card,
            &lines,
            &to_cell,
            false,
            &palette,
            Some("foo"),
            false,
        );

        let rename = out
            .iter()
            .find(|r| r.text.starts_with("foo"))
            .expect("rename run");
        assert!(rename.text.contains('▏'), "the rename buffer shows a caret");
        assert_eq!(rename.fg, chrome().accent);
        assert!(
            out.iter().all(|r| r.text != "build"),
            "the original name is replaced"
        );
    }

    #[test]
    fn emit_card_text_menu_hint_emits_ellipsis_glyph() {
        let card = store_card(false, "cargo");
        let lines = card_lines(&card, wall(10, 3), None, None);
        let palette = [Rgb::new(0, 0, 0); 256];
        let to_cell = cell_at();

        let mut with_hint = Vec::new();
        emit_card_text(
            &mut with_hint,
            &card_rects(),
            &card,
            &lines,
            &to_cell,
            false,
            &palette,
            None,
            true,
        );
        assert!(
            with_hint.iter().any(|r| r.text == "⋯"),
            "menu hint shows the ⋯ glyph"
        );

        let mut without_hint = Vec::new();
        emit_card_text(
            &mut without_hint,
            &card_rects(),
            &card,
            &lines,
            &to_cell,
            false,
            &palette,
            None,
            false,
        );
        assert!(
            without_hint.iter().all(|r| r.text != "⋯"),
            "no ⋯ glyph without the hint"
        );
    }
}
