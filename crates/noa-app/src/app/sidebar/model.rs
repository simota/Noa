use super::text::{colored_prefix, elide_end, elide_middle, elide_start, width};
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

    fn session_card_is_remote(&self, card: &SessionCardId) -> bool {
        self.windows
            .get(&WindowId::from(card.window_id.0))
            .and_then(|state| state.surfaces.get(&card.pane_id))
            .is_some_and(Surface::is_remote)
    }

    /// Build the per-frame sidebar draw model for `window_id` (FR-2/FR-5), or
    /// `None` when the window has no visible sidebar. Reads only the store and
    /// the pure layout — never a `Terminal` (AC-17). Computed before the redraw
    /// path borrows `gpu`/`state` mutably, so the drawer can run inline.
    /// Whether `card` belongs to a floating pane (floating-panes REQ-016).
    pub(in crate::app) fn session_card_is_float(&self, card: &SessionCardId) -> bool {
        self.windows
            .get(&WindowId::from(card.window_id.0))
            .is_some_and(|state| state.is_float(card.pane_id))
    }

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
        // This window's own sidebar grid: on a mixed-DPI setup the primary
        // belongs to whichever window's scale changed last, and using it here
        // would lay the cards out for that window's cell size.
        let metrics = gpu
            .sidebar_fonts
            .get(crate::app::helpers::sidebar_font_pixel_size(
                self.config.sidebar_font_size,
                state.window.scale_factor(),
            ))
            .metrics();
        // Folds in `sidebar_font_zoom()` so chrome details drawn at this
        // scale (drop indicator, borders, radii, glyph size) zoom coherently
        // with the cards laid out by `sidebar_metrics()` — the same factor,
        // applied at these two choke points only.
        let scale = state.window.scale_factor() as f32 * self.sidebar_font_zoom();
        let layout_metrics = self.sidebar_metrics(window_id);
        let height = state.window.inner_size().height.max(1);
        let band = PaneRectApp::new(0, 0, inset, height);
        let grid = grid_size_for_pane_rect(band, metrics, SIDEBAR_GRID_PADDING);

        let bounds = self.sidebar_layout_bounds(window_id, inset);
        let specs = self.sidebar_card_specs(window_id);
        let layout = layout_metrics.layout_cards(bounds, &specs, state.sidebar_scroll);

        // Pixel → cell conversion, matching where a `Renderer` places cell (0,0):
        // at the padding origin. One closure per grid (band / card / menu).
        let cell_w = metrics.cell_w.max(1.0);
        let cell_h = metrics.cell_h.max(1.0);
        let pad_left = SIDEBAR_GRID_PADDING.left;
        let pad_top = SIDEBAR_GRID_PADDING.top;
        let to_cell = |grid: GridSize| {
            move |x: u32, y: u32| px_to_cell(x, y, pad_left, pad_top, cell_w, cell_h, grid)
        };
        let selected_id = Self::session_card_id(window_id, state.focused_pane);

        // The top header band (status label / center title / name pill) was
        // removed as redundant, and the dead `…` header menu (no v1 action) was
        // dropped — the toolbar's sole `+` action now sits at the sidebar's
        // top-right (SIDEBAR_HEADER_H is collapsed to 0). The `+` is drawn as its
        // own rounded chrome tile (see `draw_sidebar_band`), not baked into the
        // flat band, so it can carry a border + hover state like a real button.
        let btn = layout.new_button;
        let new_button_hover = state.sidebar_button_hover;

        // Every card has a local text texture. Clipping that texture preserves
        // the same row positions and selection treatment at both viewport edges.
        let now = crate::localtime::wall_clock_now();
        let now_instant = Instant::now();
        let home = home_dir();
        let theme = active_theme(&gpu.theme, &gpu.preview_theme);
        let panel_bg = theme.default_bg;
        let mut cards: Vec<SidebarCardDraw> = Vec::new();
        // The card texture uses the same width as the laid-out row. The margin
        // is currently zero for a seamless list, but stays in the metric so the
        // pure layout remains the single source of geometry.
        let card_w = layout_metrics.card_w(inset).max(1);
        let card_band = PaneRectApp::new(0, 0, card_w, layout_metrics.card_h);
        let card_grid = grid_size_for_pane_rect(card_band, metrics, SIDEBAR_GRID_PADDING);
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
            let is_float = self.session_card_is_float(&card_rects.id);
            let tab_title = (!is_float)
                .then(|| self.tab_title_override_for_card(&card_rects.id))
                .flatten();
            let mut lines: CardLines = card_lines(card, now, home, tab_title.as_deref());
            mark_float_card(&mut lines, is_float);
            lines.remote_unsupported = self.session_card_is_remote(&card_rects.id);
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
            let progress_bar = card_progress_bar(card);
            let selected = card_rects.id == selected_id;
            let auto_flash = self
                .auto_approve_flash_until
                .get(&card_rects.id)
                .is_some_and(|until| now_instant < *until);
            let attention_flash = self
                .attention_flash_until
                .get(&card_rects.id)
                .is_some_and(|until| now_instant < *until);
            let progress_flash = self
                .progress_flashes
                .get(&card_rects.id)
                .filter(|flash| now_instant < flash.until)
                .map(|flash| flash.kind);
            let spec = *specs.iter().find(|spec| spec.id == card_rects.id)?;
            let local = layout_metrics.card_local_rects_for(spec, card_w);
            let mut card_runs = Vec::new();
            emit_card_text(
                &mut card_runs,
                &local,
                card,
                &lines,
                &card_cell,
                renaming,
                menu_hint,
            );
            let src_uv = card_source_uv(card_rects, layout_metrics.card_h);
            let selected_bg = sidebar_selected_card_bg(panel_bg);
            let flash_fill = auto_flash || attention_flash || progress_flash.is_some();
            cards.push(SidebarCardDraw {
                rect: card_rects.bounds,
                grid: card_grid,
                bg: if auto_flash {
                    sidebar_auto_flash_card_bg(panel_bg)
                } else if attention_flash {
                    sidebar_attention_flash_card_bg(panel_bg, selected)
                } else if let Some(kind) = progress_flash {
                    sidebar_progress_flash_card_bg(panel_bg, selected, kind)
                } else if selected {
                    selected_bg
                } else if hovered_id == Some(card_rects.id) {
                    sidebar_hover_card_bg(panel_bg)
                } else {
                    sidebar_card_bg(panel_bg)
                },
                flash_fill,
                selected,
                hovered: hovered_id == Some(card_rects.id),
                progress_bar: progress_bar
                    .filter(|_| card_rects.clipped_top + card_rects.bounds.h == card_rects.full_h),
                runs: card_runs,
                src_uv,
            });
        }

        // Card `…` menu popup (FR-7): its own overlay, composited above the cards
        // so a card row can never hide it. The layout places it above the anchor
        // near the bottom, or omits it if the available area cannot contain it.
        let menu = state.sidebar_menu.and_then(|open| {
            let card_rects = layout.cards.iter().find(|c| c.id == open)?;
            let popup = layout_metrics.card_menu_popup_rect(
                card_rects.menu_button,
                CARD_MENU_ITEMS.len(),
                bounds,
            );
            if popup.w == 0 || popup.h == 0 || popup.bottom() > height {
                return None;
            }
            let menu_band = PaneRectApp::new(0, 0, popup.w, popup.h);
            let menu_grid = grid_size_for_pane_rect(menu_band, metrics, SIDEBAR_GRID_PADDING);
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
                let is_float = self.session_card_is_float(&drag.card);
                let tab_title = (!is_float)
                    .then(|| self.tab_title_override_for_card(&drag.card))
                    .flatten();
                let mut lines = card_lines(card, now, home, tab_title.as_deref());
                mark_float_card(&mut lines, is_float);
                lines.remote_unsupported = self.session_card_is_remote(&drag.card);
                let spec = *specs.iter().find(|spec| spec.id == drag.card)?;
                let local = layout_metrics.card_local_rects_for(spec, card_w);
                let mut card_runs = Vec::new();
                emit_card_text(
                    &mut card_runs,
                    &local,
                    card,
                    &lines,
                    &card_cell,
                    None,
                    false,
                );
                // Floating top follows the cursor, clamped inside the band.
                let max_top = height.saturating_sub(local.full_h) as i64;
                let top = (drag.current_y - drag.grab_dy).clamp(0, max_top) as u32;
                let float = SidebarCardDraw {
                    rect: SidebarRect::new(layout_metrics.card_margin_x, top, card_w, local.full_h),
                    grid: card_grid,
                    bg: sidebar_selected_card_bg(panel_bg),
                    flash_fill: false,
                    selected: true,
                    hovered: false,
                    progress_bar: card_progress_bar(card),
                    runs: card_runs,
                    src_uv: card_source_uv(&local, layout_metrics.card_h),
                };
                // Drop indicator at the target gap, spanning the card width with a
                // small horizontal inset so it reads as a rule, not a full band.
                let vp = layout.viewport;
                let py = drag.current_y.clamp(0, u32::MAX as i64) as u32;
                let idx = layout_metrics.drop_index_cards(vp, &specs, state.sidebar_scroll, py);
                let indicator = layout_metrics
                    .drop_indicator_y_cards(vp, &specs, state.sidebar_scroll, idx)
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
            runs: Vec::new(),
            new_button: btn,
            new_button_hover,
            cards,
            menu,
            dragging,
            drop_indicator,
            background_opacity: self.shown_background_opacity(),
        })
    }
}

fn card_source_uv(rects: &CardRects, card_h: u32) -> [f32; 4] {
    let height = card_h.max(1) as f32;
    [
        0.0,
        rects.clipped_top as f32 / height,
        1.0,
        rects.bounds.h as f32 / height,
    ]
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

/// Available terminal columns between the rectangle's quantized edges.
fn text_columns(to_cell: &impl Fn(u32, u32) -> (u16, u16), rect: SidebarRect) -> usize {
    if rect.w == 0 || rect.h == 0 {
        return 0;
    }
    let (left, _) = to_cell(rect.x, rect.y);
    let (right, _) = to_cell(rect.right(), rect.y);
    usize::from(right.saturating_sub(left))
}

/// A bounded, single-line text run, or `None` when no text fits.
fn window_run(
    to_cell: &impl Fn(u32, u32) -> (u16, u16),
    rect: SidebarRect,
    text: String,
    fg: Rgb,
    bold: bool,
) -> Option<SidebarTextRun> {
    let text = elide_end(&text, text_columns(to_cell, rect));
    if text.is_empty() {
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

/// Fit the plain text before adding color escapes, preserving whole graphemes
/// even when the source spans split a combining or emoji sequence.
fn colored_run(
    to_cell: &impl Fn(u32, u32) -> (u16, u16),
    rect: SidebarRect,
    spans: &[(&str, Rgb)],
) -> Option<SidebarTextRun> {
    let text = colored_prefix(spans, text_columns(to_cell, rect));
    if text.is_empty() {
        return None;
    }
    let (col, row) = to_cell(rect.x, rect.y);
    Some(SidebarTextRun::new(col, row, text, spans.first()?.1))
}

/// A text run right-aligned within `rect`: the run's column is backed off from
/// the rect's right edge by the text's display width.
fn right_aligned_run(
    to_cell: &impl Fn(u32, u32) -> (u16, u16),
    rect: SidebarRect,
    text: String,
    fg: Rgb,
) -> Option<SidebarTextRun> {
    let text = elide_end(&text, text_columns(to_cell, rect));
    if text.is_empty() {
        return None;
    }
    let (left_col, row) = to_cell(rect.x, rect.y);
    let (right_col, _) = to_cell(rect.x + rect.w, rect.y);
    let col = right_col.saturating_sub(width(&text) as u16).max(left_col);
    Some(SidebarTextRun {
        col,
        row,
        text,
        fg,
        bg: None,
        bold: false,
    })
}

/// A single status label, shown on the name row. Routine activity stays
/// neutral; semantic color is reserved for a request that needs attention.
fn card_state_label(card: &SessionCard) -> Option<(&'static str, Rgb)> {
    use noa_grid::AgentState;
    if let Some(report) = &card.agent_status
        && report.state.needs_attention()
    {
        let label = match report.state {
            AgentState::Permission => "approval",
            AgentState::Input => "reply",
            AgentState::Error => "error",
            _ => report.state.label(),
        };
        return Some((label, chrome().dot_red));
    }
    if card.attention {
        return Some((ATTENTION_LABEL, chrome().dot_red));
    }
    if card.unread_bell {
        return Some(("bell", chrome().dim_fg));
    }
    if card
        .agent_status
        .as_ref()
        .is_some_and(|report| report.state == AgentState::Finished)
    {
        return Some(("done", chrome().dim_fg));
    }
    card.is_running().then_some(("running", chrome().dim_fg))
}

fn working_directory_name(cwd: &str) -> &str {
    let trimmed = cwd.trim_end_matches(['/', '\\']);
    trimmed
        .rsplit(['/', '\\'])
        .find(|part| !part.is_empty())
        .unwrap_or(cwd)
}

/// Compact identification, without repeating the state already on the title.
/// The directory tail and automation mode remain readable at narrow widths.
fn agent_meta(card: &SessionCard, lines: &CardLines, cols: usize) -> Vec<(String, Rgb)> {
    let agent = if lines.remote_unsupported {
        "PROCESS N/A"
    } else if card.agent.kind == AgentKind::ClaudeCode {
        "Claude"
    } else if card.agent.kind == AgentKind::Generic
        && matches!(lines.process.as_str(), "idle" | "running")
    {
        ""
    } else {
        agent_display_name(card.agent.kind, &lines.process)
    };
    let cwd = working_directory_name(&lines.cwd);
    let auto = if lines.remote_unsupported {
        "AUTO N/A"
    } else if card.auto_approve_enabled {
        "AUTO ON"
    } else {
        ""
    };
    let nonempty = [agent, cwd, auto]
        .into_iter()
        .filter(|s| !s.is_empty())
        .count();
    let total = width(agent) + width(cwd) + width(auto) + 3 * nonempty.saturating_sub(1);
    let gap = if total <= cols { " · " } else { " " };
    let auto = elide_end(auto, cols);
    let remaining =
        cols.saturating_sub(width(&auto) + if auto.is_empty() { 0 } else { width(gap) });
    let agent_budget = if cwd.is_empty() {
        remaining
    } else {
        remaining.saturating_sub(width(cwd).min(5) + width(gap))
    };
    let agent = elide_end(agent, agent_budget);
    let cwd_budget =
        remaining.saturating_sub(width(&agent) + if agent.is_empty() { 0 } else { width(gap) });
    let cwd = elide_start(cwd, cwd_budget);
    let mut spans = Vec::new();
    for text in [agent, cwd, auto] {
        if text.is_empty() {
            continue;
        }
        if !spans.is_empty() {
            spans.push((gap.to_string(), chrome().dim_fg));
        }
        spans.push((text, chrome().dim_fg));
    }
    spans
}

/// Detailed context belongs to the selected card. The compact metadata already
/// identifies the directory, so narrow layouts can spend this row on the branch
/// instead of producing two unrelated fragments.
fn project_context(lines: &CardLines, cols: usize) -> String {
    if lines.branch.is_empty() {
        return elide_middle(&lines.cwd, cols);
    }
    let branch = format!("⎇ {}", lines.branch);
    if lines.cwd.is_empty() || cols < 36 {
        return elide_middle(&branch, cols);
    }
    let available = cols - 3;
    let branch_cols =
        width(&branch).min((available / 2).max(available.saturating_sub(width(&lines.cwd))));
    let cwd_cols = available - branch_cols;
    format!(
        "{} · {}",
        elide_middle(&branch, branch_cols),
        elide_middle(&lines.cwd, cwd_cols)
    )
}

/// Emit identity, status, project context, and the configured preview rows.
/// `renaming` carries the live rename buffer when this card's
/// inline rename is open — it replaces the name run with the buffer + caret in
/// the accent color. `menu_hint` surfaces the `…` glyph over its (always-live)
/// hit region — on for the hovered card and while the card's menu is open.
fn emit_card_text(
    out: &mut Vec<SidebarTextRun>,
    rects: &CardRects,
    card: &SessionCard,
    lines: &CardLines,
    to_cell: &impl Fn(u32, u32) -> (u16, u16),
    renaming: Option<&str>,
    menu_hint: bool,
) {
    let title = SidebarRect::new(
        rects.name_line.x,
        rects.name_line.y,
        rects.updated.right().saturating_sub(rects.name_line.x),
        rects.menu_button.h,
    );
    let title_cols = text_columns(to_cell, title);
    let (right, right_fg) = if renaming.is_some() {
        (String::new(), chrome().dim_fg)
    } else if let Some((label, color)) = card_state_label(card) {
        (elide_end(label, title_cols.saturating_sub(5)), color)
    } else if rects.expanded || menu_hint {
        // Age is secondary: reveal it while inspecting the card, and only if
        // there is room to keep an identifying portion of its name.
        let fits = title_cols >= width(&lines.updated) + 1 + width(&lines.name).min(12);
        (
            if fits {
                lines.updated.clone()
            } else {
                String::new()
            },
            chrome().dim_fg,
        )
    } else {
        (String::new(), chrome().dim_fg)
    };
    let name_cols = title_cols.saturating_sub(width(&right) + usize::from(!right.is_empty()));
    let (name_text, name_fg) = match renaming {
        Some(buffer) => (
            elide_start(&format!("{buffer}▏"), name_cols),
            chrome().accent,
        ),
        None => (elide_end(&lines.name, name_cols), chrome().fg),
    };
    out.extend(window_run(to_cell, title, name_text, name_fg, true));
    out.extend(right_aligned_run(to_cell, title, right, right_fg));
    // The `…` menu affordance (its hit region is always live; the glyph shows
    // on hover / while its menu is open).
    if menu_hint {
        out.extend(window_run(
            to_cell,
            rects.menu_button,
            "⋯".to_string(),
            chrome().dim_fg,
            false,
        ));
    }

    let meta = agent_meta(card, lines, text_columns(to_cell, rects.meta));
    let spans: Vec<_> = meta.iter().map(|(text, fg)| (text.as_str(), *fg)).collect();
    out.extend(colored_run(to_cell, rects.meta, &spans));
    out.extend(window_run(
        to_cell,
        rects.context,
        project_context(lines, text_columns(to_cell, rects.context)),
        chrome().dim_fg,
        false,
    ));

    let detail = card
        .agent_status
        .as_ref()
        .filter(|report| !report.detail.is_empty());
    // An unselected request gets one concise line. Routine cards never show
    // their terminal output until selected, even if the source still collects it.
    if !rects.expanded {
        if let Some(report) = detail.filter(|report| report.state.needs_attention())
            && let Some(rect) = rects.preview.first()
        {
            out.extend(window_run(
                to_cell,
                *rect,
                report.detail.clone(),
                chrome().dim_fg,
                false,
            ));
        }
        return;
    }
    // Current work takes precedence over historical approvals. An audit is
    // useful only when there is no current detail competing with it.
    let summary = detail.map(|report| report.detail.clone()).or_else(|| {
        if card.agent_needs_attention() || card.attention {
            return None;
        }
        card.auto_approve_audit
            .back()
            .map(|entry| format!("Last approval · {}", entry.prompt))
    });
    let output_rows = rects
        .preview
        .len()
        .saturating_sub(usize::from(summary.is_some()));
    let mut preview_index = card.preview.len().saturating_sub(output_rows);
    for (row_index, rect) in rects.preview.iter().enumerate() {
        if row_index == 0
            && let Some(text) = &summary
        {
            out.extend(window_run(
                to_cell,
                *rect,
                text.clone(),
                chrome().dim_fg,
                false,
            ));
            continue;
        }
        let Some(line) = card.preview.get(preview_index) else {
            continue;
        };
        preview_index += 1;
        // The sidebar is a quiet summary. Preserve the original ANSI colors in
        // the terminal/store while showing preview text with one secondary tone.
        let spans: Vec<_> = line
            .iter()
            .map(|span| (span.text.as_str(), chrome().dim_fg))
            .collect();
        out.extend(colored_run(to_cell, *rect, &spans));
    }
}

/// The mark a floating pane's card carries in front of its name
/// (floating-panes REQ-016).
pub(in crate::app) const FLOAT_CARD_MARK: &str = "\u{25f0} ";

fn mark_float_card(lines: &mut CardLines, is_float: bool) {
    if is_float {
        lines.name.insert_str(0, FLOAT_CARD_MARK);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session_store::{AutoApproveAuditEntry, PreviewSpan, SessionStore, WallClock};
    use noa_core::Color;

    #[test]
    fn full_card_samples_the_whole_texture() {
        assert_eq!(card_source_uv(&card_rects(), 140), [0.0, 0.0, 1.0, 1.0]);
    }

    #[test]
    fn partial_cards_sample_the_correct_top_or_bottom_slice() {
        let mut rects = card_rects();
        rects.bounds.h = 70;
        assert_eq!(card_source_uv(&rects, 140), [0.0, 0.0, 1.0, 0.5]);
        rects.clipped_top = 70;
        assert_eq!(card_source_uv(&rects, 140), [0.0, 0.5, 1.0, 0.5]);
    }

    #[test]
    fn short_viewport_keeps_the_middle_slice_without_squashing_content() {
        let mut rects = card_rects();
        rects.clipped_top = 35;
        rects.bounds.h = 35;
        assert_eq!(card_source_uv(&rects, 140), [0.0, 0.25, 1.0, 0.25]);
    }

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
    fn output_color_does_not_compete_with_the_state_label() {
        let mut card = store_card(false, "cargo");
        let rects = card_rects();
        let to_cell = cell_at();
        for color in [
            Color::Default,
            Color::Palette(5),
            Color::Rgb(Rgb::new(255, 0, 0)),
        ] {
            card.preview[0][0].fg = color;
            let lines = card_lines(&card, wall(10, 3), None, None);
            let mut out = Vec::new();
            emit_card_text(&mut out, &rects, &card, &lines, &to_cell, None, false);
            let row = to_cell(rects.preview[0].x, rects.preview[0].y).1;
            let preview = out
                .iter()
                .find(|run| run.row == row)
                .expect("output preview");
            let term = rendered_terminal(&preview.text);
            assert_eq!(
                term.active().grid[0].cells[0].fg,
                Color::Rgb(chrome().dim_fg)
            );
            assert_eq!(
                card.preview[0][0].fg, color,
                "the terminal's source colors are retained"
            );
        }
    }

    #[test]
    fn colored_prefix_emits_truecolor_without_spending_text_columns() {
        let color = Rgb::new(10, 20, 30);
        let text = colored_prefix(&[("color", color)], 5);
        assert!(text.contains("\x1b[38;2;10;20;30m"));
        let term = rendered_terminal(&text);
        assert_eq!(term.active().cursor.x, 5);
        assert_eq!(term.active().grid[0].cells[0].fg, Color::Rgb(color));
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

    fn waiting_card() -> SessionCard {
        let mut card = store_card(true, "claude");
        card.name_override =
            Some("検索結果の視認性を改善 👩🏽‍💻 1\u{20e3} cafe\u{301} long session title".to_string());
        card.cwd = "/Users/dev/projects/a-very-long-working-directory/Noa".to_string();
        card.branch = Some("feature/agent-sidebar-visibility-日本語のブランチ".to_string());
        card.auto_approve_enabled = true;
        card.agent_status = Some(noa_grid::AgentStatus {
            state: noa_grid::AgentState::Permission,
            detail: "Review permission for cargo test --workspace 日本語の詳細".to_string(),
            blocked: Some(noa_grid::AgentBlock::Permission),
            app: Some("claude-code".to_string()),
        });
        card.agent_status_at = Some(wall(10, 1));
        card.auto_approve_audit.push_back(AutoApproveAuditEntry {
            at: wall(10, 0),
            agent: "Claude Code".to_string(),
            prompt: "previous command".to_string(),
        });
        card.preview = (0..20)
            .map(|index| {
                vec![
                    PreviewSpan {
                        text: format!("output {index}: "),
                        fg: Color::Rgb(Rgb::new(40, 120, 210)),
                    },
                    PreviewSpan {
                        text: "日本語 👩🏽‍💻 1\u{20e3} cafe\u{301} passed".to_string(),
                        fg: Color::Rgb(Rgb::new(90, 180, 110)),
                    },
                ]
            })
            .collect();
        card
    }

    // Use the same VT/grid engine as the renderer, independently of the text
    // fitting helper: emoji clusters, keycaps and SGR must consume real cells.
    fn rendered_terminal(text: &str) -> Terminal {
        let mut term = Terminal::new(GridSize {
            cols: 1024,
            rows: 1,
        });
        let mut stream = Stream::new();
        stream.feed(b"\x1b[?7l", &mut term);
        stream.feed(text.as_bytes(), &mut term);
        term
    }

    fn rendered_width(text: &str) -> usize {
        usize::from(rendered_terminal(text).active().cursor.x)
    }

    fn assert_runs_fit_without_overlap(runs: &[SidebarTextRun], grid: GridSize) {
        let mut occupied = HashSet::new();
        for run in runs {
            let end = usize::from(run.col) + rendered_width(&run.text);
            assert!(run.row < grid.rows, "row outside {grid:?}: {run:?}");
            assert!(
                end <= usize::from(grid.cols),
                "text outside {grid:?}: {run:?}"
            );
            for col in usize::from(run.col)..end {
                assert!(
                    occupied.insert((col, run.row)),
                    "overlapping cell ({col}, {}) in {grid:?}: {run:?}",
                    run.row
                );
            }
        }
    }

    #[test]
    fn real_font_metrics_keep_all_card_modes_inside_distinct_cells() {
        let card = waiting_card();
        let lines = card_lines(&card, wall(10, 3), Some("/Users/dev"), None);
        let rename = "長い日本語の名前 👩🏽‍💻 1\u{20e3} cafe\u{301} 編集中";
        for dpr in [1.0, 1.5, 2.0] {
            for font_size in [8.0, 11.5, 20.0] {
                let font = FontGrid::new(
                    crate::app::helpers::sidebar_font_pixel_size(font_size, dpr),
                    Default::default(),
                )
                .expect("sidebar system font");
                let font_metrics = font.metrics();
                let scale = dpr as f32 * font_size / SIDEBAR_FONT_POINT_SIZE;
                for preview_lines in [0, 1, 5, 20] {
                    let metrics = SidebarMetrics::new_with_preview_lines(scale, preview_lines)
                        .with_cell_height(font_metrics.cell_h);
                    for width in [200, 360, 600] {
                        let card_w = (f64::from(width) * dpr).round() as u32;
                        let grid = grid_size_for_pane_rect(
                            PaneRectApp::new(0, 0, card_w, metrics.card_h),
                            font_metrics,
                            SIDEBAR_GRID_PADDING,
                        );
                        let to_cell = |x, y| {
                            px_to_cell(
                                x,
                                y,
                                0.0,
                                0.0,
                                font_metrics.cell_w,
                                font_metrics.cell_h,
                                grid,
                            )
                        };
                        for mode in [
                            SidebarCardMode::Compact,
                            SidebarCardMode::Attention,
                            SidebarCardMode::Expanded,
                        ] {
                            let spec = SidebarCardSpec {
                                id: card_rects().id,
                                mode,
                            };
                            let rects = metrics.card_local_rects_for(spec, card_w);
                            for renaming in [None, Some(rename)] {
                                let mut out = Vec::new();
                                emit_card_text(
                                    &mut out, &rects, &card, &lines, &to_cell, renaming, true,
                                );
                                assert_runs_fit_without_overlap(&out, grid);
                                for run in &out {
                                    assert!(
                                        (f32::from(run.row) + 1.0) * font_metrics.cell_h
                                            <= rects.full_h as f32 + 1.0
                                    );
                                }
                                let title = out.iter().find(|run| run.bold).expect("visible title");
                                if renaming.is_some() {
                                    assert!(title.text.ends_with('▏'), "{title:?}");
                                }
                                assert!(out.iter().any(|run| run.text == "⋯"));
                                let meta_row = to_cell(rects.meta.x, rects.meta.y).1;
                                assert!(out.iter().any(|run| run.row == meta_row));
                                if rects.expanded {
                                    let context_row = to_cell(rects.context.x, rects.context.y).1;
                                    assert_ne!(meta_row, context_row);
                                    assert!(out.iter().any(|run| run.row == context_row));
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn compact_metadata_keeps_project_and_auto_without_repeating_state() {
        let card = waiting_card();
        let lines = card_lines(&card, wall(10, 3), None, None);
        for cols in [25, 34, 48, 80] {
            let spans = agent_meta(&card, &lines, cols);
            let plain: String = spans.iter().map(|(text, _)| text.as_str()).collect();
            let spans: Vec<_> = spans
                .iter()
                .map(|(text, color)| (text.as_str(), *color))
                .collect();
            let colored = colored_prefix(&spans, cols);
            assert!(
                !plain.contains("approval"),
                "state is shown once, on the title"
            );
            assert!(plain.contains("Noa"), "{cols}: {plain}");
            assert!(plain.contains("AUTO ON"), "{cols}: {plain}");
            assert!(rendered_width(&colored) <= cols, "{cols}: {plain}");
        }
    }

    #[test]
    fn context_uses_the_branch_at_narrow_widths_and_adds_the_path_when_it_fits() {
        let card = waiting_card();
        let lines = card_lines(&card, wall(10, 3), Some("/Users/dev"), None);
        for cols in [25, 34, 48, 80] {
            let context = project_context(&lines, cols);
            assert!(context.starts_with("⎇ "), "{context}");
            if cols >= 36 {
                assert!(context.ends_with("/Noa"), "{context}");
            } else {
                assert!(
                    !context.contains(" · "),
                    "narrow context prioritizes the branch"
                );
            }
            assert!(rendered_width(&context) <= cols, "{cols}: {context}");
        }
    }

    #[test]
    fn narrow_titles_reclaim_unused_timestamp_columns() {
        let to_cell = cell_at();
        for busy in [false, true] {
            let mut card = store_card(busy, "cargo");
            card.name_override = Some("a long descriptive session name".to_string());
            let lines = card_lines(&card, wall(10, 3), None, None);
            let metrics = SidebarMetrics::new(1.0).with_cell_height(20.0);
            let rects = metrics.card_local_rects(card_rects().id, 200);
            let mut out = Vec::new();
            emit_card_text(&mut out, &rects, &card, &lines, &to_cell, None, true);
            let title = out.iter().find(|run| run.bold).expect("title");
            assert!(
                rendered_width(&title.text) >= 5,
                "retain an identifying name"
            );
            assert!(
                out.iter()
                    .all(|run| lines.updated.is_empty() || run.text != lines.updated)
            );
            assert_runs_fit_without_overlap(
                &out,
                GridSize {
                    cols: 20,
                    rows: 100,
                },
            );
        }
    }

    #[test]
    fn one_preview_row_prioritizes_pending_details_over_previous_approval() {
        let card = waiting_card();
        let lines = card_lines(&card, wall(10, 3), None, None);
        let to_cell = cell_at();
        let rects = card_rects();
        let mut out = Vec::new();
        emit_card_text(&mut out, &rects, &card, &lines, &to_cell, None, false);
        let preview_row = to_cell(rects.preview[0].x, rects.preview[0].y).1;
        let detail = out
            .iter()
            .find(|run| run.row == preview_row)
            .expect("pending detail");
        assert!(detail.text.starts_with("Review permission"));
        assert_eq!(detail.fg, chrome().dim_fg);
        assert_eq!(
            out.iter().filter(|run| run.fg == chrome().dot_red).count(),
            1
        );
        assert!(out.iter().all(|run| !run.text.contains("AUTO APPROVED")));
    }

    #[test]
    fn a_current_request_without_detail_does_not_surface_a_previous_approval() {
        let mut card = waiting_card();
        card.agent_status.as_mut().unwrap().detail.clear();
        let lines = card_lines(&card, wall(10, 3), None, None);
        let mut out = Vec::new();
        emit_card_text(
            &mut out,
            &card_rects(),
            &card,
            &lines,
            &cell_at(),
            None,
            false,
        );
        assert!(out.iter().any(|run| run.text == "approval"));
        assert!(out.iter().all(|run| !run.text.contains("Last approval")));
    }

    #[test]
    fn two_preview_rows_keep_pending_details_and_latest_output_in_a_quiet_tone() {
        let mut card = waiting_card();
        card.auto_approve_audit.clear();
        let lines = card_lines(&card, wall(10, 3), None, None);
        let to_cell = cell_at();
        let mut rects = card_rects();
        rects.bounds.h = 160;
        rects.preview.push(SidebarRect::new(10, 120, 340, 20));
        let mut out = Vec::new();
        emit_card_text(&mut out, &rects, &card, &lines, &to_cell, None, false);
        let detail_row = to_cell(rects.preview[0].x, rects.preview[0].y).1;
        let output_row = to_cell(rects.preview[1].x, rects.preview[1].y).1;
        let detail = out
            .iter()
            .find(|run| run.row == detail_row)
            .expect("pending detail");
        assert!(detail.text.starts_with("Review permission"));
        let output = out
            .iter()
            .find(|run| run.row == output_row)
            .expect("latest output");
        assert!(output.text.contains("output 19:"), "{}", output.text);
        assert!(!output.text.contains("output 18:"));
        let term = rendered_terminal(&output.text);
        assert_eq!(
            term.active().grid[0].cells[0].fg,
            Color::Rgb(chrome().dim_fg)
        );
        assert_eq!(
            term.active().grid[0].cells[11].fg,
            Color::Rgb(chrome().dim_fg)
        );
        assert_runs_fit_without_overlap(&out, GridSize { cols: 36, rows: 8 });
    }

    #[test]
    fn status_labels_keep_activity_neutral_and_requests_explicit() {
        use noa_grid::AgentState;
        let mut card = waiting_card();
        for (state, label, color) in [
            (AgentState::Running, "running", chrome().dim_fg),
            (AgentState::Permission, "approval", chrome().dot_red),
            (AgentState::Input, "reply", chrome().dot_red),
            (AgentState::Error, "error", chrome().dot_red),
            (AgentState::Finished, "done", chrome().dim_fg),
        ] {
            card.agent_status.as_mut().unwrap().state = state;
            assert_eq!(card_state_label(&card), Some((label, color)));
        }
    }

    #[test]
    fn the_single_status_label_prioritizes_attention_over_bell_and_activity() {
        let mut card = store_card(false, "cargo");
        assert_eq!(card_state_label(&card), None);
        card.busy = true;
        assert_eq!(card_state_label(&card), Some(("running", chrome().dim_fg)));
        card.unread_bell = true;
        assert_eq!(card_state_label(&card), Some(("bell", chrome().dim_fg)));
        card.attention = true;
        assert_eq!(
            card_state_label(&card),
            Some((ATTENTION_LABEL, chrome().dot_red))
        );
    }

    #[test]
    fn compact_cards_show_only_identity_until_selected() {
        let card = store_card(true, "codex");
        let lines = card_lines(&card, wall(10, 3), None, None);
        let metrics = SidebarMetrics::new(1.0).with_cell_height(20.0);
        let to_cell = cell_at();
        let mut counts = Vec::new();
        for mode in [SidebarCardMode::Compact, SidebarCardMode::Expanded] {
            let rects = metrics.card_local_rects_for(
                SidebarCardSpec {
                    id: card_rects().id,
                    mode,
                },
                360,
            );
            let mut out = Vec::new();
            emit_card_text(&mut out, &rects, &card, &lines, &to_cell, None, false);
            let rows: HashSet<_> = out.iter().map(|run| run.row).collect();
            if mode == SidebarCardMode::Compact {
                assert_eq!(rows.len(), 2);
                assert!(out.iter().all(|run| !run.text.contains("output")));
                assert_eq!(out.iter().filter(|run| run.text == "running").count(), 1);
            } else {
                assert!(out.iter().any(|run| run.text.contains("output")));
            }
            counts.push(rects.full_h);
        }
        assert!(counts[0] < counts[1]);
    }

    #[test]
    fn compact_attention_keeps_only_the_current_request_and_zero_disables_details() {
        let card = waiting_card();
        let lines = card_lines(&card, wall(10, 3), None, None);
        for preview_lines in [0, 1, 5] {
            let metrics =
                SidebarMetrics::new_with_preview_lines(1.0, preview_lines).with_cell_height(20.0);
            let rects = metrics.card_local_rects_for(
                SidebarCardSpec {
                    id: card_rects().id,
                    mode: SidebarCardMode::Attention,
                },
                360,
            );
            let mut out = Vec::new();
            emit_card_text(&mut out, &rects, &card, &lines, &cell_at(), None, false);
            let rows: HashSet<_> = out.iter().map(|run| run.row).collect();
            assert_eq!(rows.len(), if preview_lines == 0 { 2 } else { 3 });
            assert!(
                out.iter()
                    .all(|run| !run.text.contains("output") && !run.text.contains("Last approval"))
            );
            assert_eq!(
                out.iter().filter(|run| run.fg == chrome().dot_red).count(),
                1
            );
        }
    }

    /// Non-zero sub-rects so every run region emits; one preview row.
    fn card_rects() -> CardRects {
        CardRects {
            id: SessionCardId::new(SessionWindowId(1), PaneId::new(0)),
            bounds: SidebarRect::new(0, 0, 360, 140),
            full_h: 140,
            expanded: true,
            clipped_top: 0,
            icon: SidebarRect::new(10, 0, 20, 20),
            name_line: SidebarRect::new(30, 0, 220, 20),
            meta: SidebarRect::new(10, 40, 340, 20),
            context: SidebarRect::new(10, 60, 340, 20),
            preview: vec![SidebarRect::new(10, 100, 340, 20)],
            updated: SidebarRect::new(260, 0, 60, 20),
            dot: SidebarRect::new(0, 0, 10, 20),
            menu_button: SidebarRect::new(330, 0, 20, 20),
        }
    }

    #[test]
    fn emit_card_text_emits_bold_name_run() {
        let card = store_card(false, "cargo");
        let lines = card_lines(&card, wall(10, 3), None, None);
        let to_cell = cell_at();
        let mut out = Vec::new();
        emit_card_text(
            &mut out,
            &card_rects(),
            &card,
            &lines,
            &to_cell,
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
        let to_cell = cell_at();
        let mut out = Vec::new();
        emit_card_text(
            &mut out,
            &card_rects(),
            &card,
            &lines,
            &to_cell,
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
        let to_cell = cell_at();
        let mut out = Vec::new();
        emit_card_text(
            &mut out,
            &card_rects(),
            &card,
            &lines,
            &to_cell,
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
    fn emit_card_text_uses_neutral_notification_copy_for_attention() {
        let mut card = store_card(true, "claude");
        card.attention = true;
        let lines = card_lines(&card, wall(10, 3), None, None);
        let to_cell = cell_at();
        let mut out = Vec::new();
        emit_card_text(
            &mut out,
            &card_rects(),
            &card,
            &lines,
            &to_cell,
            None,
            false,
        );

        assert!(out.iter().any(|run| run.text.contains(ATTENTION_LABEL)));
        assert!(out.iter().all(|run| !run.text.contains("応答待ち")));
    }

    #[test]
    fn emit_card_text_renaming_replaces_name_with_caret_in_accent() {
        let card = store_card(false, "cargo");
        let lines = card_lines(&card, wall(10, 3), None, None);
        let to_cell = cell_at();
        let mut out = Vec::new();
        emit_card_text(
            &mut out,
            &card_rects(),
            &card,
            &lines,
            &to_cell,
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
        let to_cell = cell_at();

        let mut with_hint = Vec::new();
        emit_card_text(
            &mut with_hint,
            &card_rects(),
            &card,
            &lines,
            &to_cell,
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
            None,
            false,
        );
        assert!(
            without_hint.iter().all(|r| r.text != "⋯"),
            "no ⋯ glyph without the hint"
        );
    }

    #[test]
    fn remote_card_names_process_and_auto_approve_as_unsupported() {
        let card = store_card(false, "idle");
        let mut lines = card_lines(&card, wall(10, 3), None, None);
        lines.remote_unsupported = true;
        let to_cell = cell_at();
        let mut out = Vec::new();
        emit_card_text(
            &mut out,
            &card_rects(),
            &card,
            &lines,
            &to_cell,
            None,
            false,
        );

        assert!(
            out.iter()
                .any(|run| { run.text.contains("AUTO N/A") && run.text.contains("PROCESS N/A") })
        );
    }

    #[test]
    fn attention_flash_tints_both_resting_and_selected_backgrounds() {
        let panel = Rgb::new(10, 20, 30);
        let resting = sidebar_attention_flash_card_bg(panel, false);
        let selected = sidebar_attention_flash_card_bg(panel, true);

        assert_ne!(resting, sidebar_card_bg(panel));
        assert_ne!(selected, sidebar_selected_card_bg(panel));
        assert_ne!(selected, resting);
    }
}
