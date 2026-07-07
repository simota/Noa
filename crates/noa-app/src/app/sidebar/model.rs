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
        let scale = state.window.scale_factor() as f32;
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

        // Card text on the flat backdrop, plus a rounded overlay for every
        // fully-visible card (FR-2). Partially-scrolled cards stay flat.
        let now = sidebar_wall_clock_now();
        let home = std::env::var("HOME").ok();
        let palette = &active_theme(&gpu.theme, &gpu.preview_theme).palette;
        let mut cards: Vec<SidebarCardDraw> = Vec::new();
        // Cards are inset from the sidebar edges, so their texture (and cell
        // grid) is the margin-narrowed card width, not the full sidebar inset.
        let card_w = layout_metrics.card_w(inset).max(1);
        let card_band = PaneRectApp::new(0, 0, card_w, layout_metrics.card_h);
        let card_grid = grid_size_for_pane_rect(card_band, metrics, self.padding);
        let card_cell = to_cell(card_grid);
        // The card being drag-reordered is drawn as a floating copy below (2b),
        // so its static slot is left as a bare backdrop gap — the affordance that
        // the card has lifted out of the list.
        let dragging_id = state.sidebar_drag.filter(|d| d.active).map(|d| d.card);
        for card_rects in &layout.cards {
            if Some(card_rects.id) == dragging_id {
                continue;
            }
            let Some(card) = self.session_store.get(&card_rects.id) else {
                continue;
            };
            let tab_title = self.tab_title_override_for_card(&card_rects.id);
            let lines: CardLines = card_lines(card, now, home.as_deref(), tab_title.as_deref());
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
            let full = card_rects.bounds.h == layout_metrics.card_h;
            // A fully-visible card is covered by its opaque rounded overlay, so
            // its backdrop text would never show — only emit it for partial
            // (edge-clipped) cards, which have no overlay.
            if !full {
                emit_card_text(
                    &mut runs, card_rects, card, &lines, &band_cell, marker, palette, renaming,
                );
            }

            if full {
                let selected = card_rects.id == selected_id;
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
                );
                cards.push(SidebarCardDraw {
                    rect: card_rects.bounds,
                    grid: card_grid,
                    bg: if selected {
                        chrome().card_selected
                    } else {
                        chrome().card
                    },
                    selected,
                    attention: card.attention,
                    runs: card_runs,
                });
            }
        }

        // Card `…` menu popup (FR-7): its own overlay, composited above the cards
        // so a rounded card can never hide it. Skipped when the open card has
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
                let lines = card_lines(card, now, home.as_deref(), tab_title.as_deref());
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
                    bg: chrome().card_selected,
                    selected: true,
                    attention: false,
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

/// Emit one card's text runs (status dot, project icon, bold name, cwd, the
/// meta row `process · ⎇ branch`, configured preview rows, updated-time)
/// through `to_cell`. Shared by the flat backdrop (window coords) and each
/// rounded overlay (card-local coords) so both agree on layout. `renaming`
/// carries the live rename buffer when this card's inline rename is open —
/// it replaces the name run with the buffer + caret in the accent color.
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
) {
    out.extend(window_run(
        to_cell,
        rects.dot,
        "●".to_string(),
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
    out.extend(window_run(
        to_cell,
        rects.cwd_line,
        lines.cwd.clone(),
        chrome().dim_fg,
        false,
    ));

    // Meta row: a recognized AI agent gets its brand glyph/color/name (busy or
    // idle); any other process shows green `✳` while running, dim `❯` while
    // idle. The git branch follows on the same row, dim. A pending interaction
    // request (FR-16) overrides the badge with the attention color and appends
    // the waiting label; the label is held steady while pending (only the dot
    // blinks, via `effective_status_dot`) so it stays legible.
    if rects.meta.w > 0 && rects.meta.h > 0 {
        let (badge, badge_fg) = process_badge(&lines.process, card.busy);
        let (badge, badge_fg) = if card.attention {
            (format!("{badge} · {ATTENTION_LABEL}"), chrome().dot_red)
        } else {
            (badge, badge_fg)
        };
        let text = if lines.branch.is_empty() {
            badge
        } else {
            // The dim branch suffix is recolored inline; the run fg colors the
            // badge portion.
            format!("{badge}{} · ⎇ {}", sgr_fg(chrome().dim_fg), lines.branch)
        };
        out.extend(window_run(to_cell, rects.meta, text, badge_fg, false));
    }

    // Last-output preview rows, in their original ANSI colors: each
    // span is recolored inline via an embedded SGR prefix, so one run carries
    // the whole line. Rows the card has no preview line for stay blank.
    for (rect, line) in rects.preview.iter().zip(card.preview.iter()) {
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

    out.extend(window_run(
        to_cell,
        rects.updated,
        lines.updated.clone(),
        chrome().dim_fg,
        false,
    ));
}

/// Wall-clock now, in the viewer's local zone, for the sidebar's relative
/// updated-time (mirrors the io thread's stamp so both agree).
fn sidebar_wall_clock_now() -> crate::session_store::WallClock {
    let unix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or(0);
    crate::session_store::civil_from_unix_secs(unix + crate::localtime::local_offset_seconds())
}
