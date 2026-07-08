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
        let now_instant = Instant::now();
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
        let hovered_id = state.sidebar_card_hover;
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
            // The `…` glyph surfaces on the hovered card (discoverability) and
            // stays visible while its menu popup is open.
            let menu_hint =
                hovered_id == Some(card_rects.id) || state.sidebar_menu == Some(card_rects.id);
            let full = card_rects.bounds.h == layout_metrics.card_h;
            // A fully-visible card is covered by its opaque rounded overlay, so
            // its backdrop text would never show — only emit it for partial
            // (edge-clipped) cards, which have no overlay.
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
                cards.push(SidebarCardDraw {
                    rect: card_rects.bounds,
                    grid: card_grid,
                    bg: if auto_flash {
                        mix_rgb(chrome().card_selected, chrome().accent, 0.35)
                    } else if selected {
                        chrome().card_selected
                    } else if hovered_id == Some(card_rects.id) {
                        // Hover face: halfway between resting and selected, so
                        // the card lifts without competing with the selection.
                        mix_rgb(chrome().card, chrome().card_selected, 0.5)
                    } else {
                        chrome().card
                    },
                    selected,
                    attention: card.attention,
                    accent: card_accent(card, marker),
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
                    bg: chrome().card_selected,
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
/// (window coords) and each rounded overlay (card-local coords) so both agree
/// on layout. `renaming` carries the live rename buffer when this card's
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

/// Wall-clock now, in the viewer's local zone, for the sidebar's relative
/// updated-time (mirrors the io thread's stamp so both agree).
fn sidebar_wall_clock_now() -> crate::session_store::WallClock {
    let unix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or(0);
    crate::session_store::civil_from_unix_secs(unix + crate::localtime::local_offset_seconds())
}
