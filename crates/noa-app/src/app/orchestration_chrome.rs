//! Persistent role labels above orchestration panes. The strip is outside
//! the terminal rect, so its pixels never obscure output or receive pty input.

use super::*;
use noa_core::Rgb;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

// Float strips use bit 63. Pane ids themselves never reach either bit.
const ROLE_RENDER_ID_BIT: u64 = 1 << 62;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum PaneRole {
    Orchestrator,
    Board,
}

impl PaneRole {
    pub(super) fn label(self, room: usize) -> &'static str {
        match self {
            Self::Orchestrator if room >= 12 => "Orchestrator",
            Self::Orchestrator if room >= 4 => "ORCH",
            Self::Orchestrator => "O",
            Self::Board if room >= 5 => "Board",
            Self::Board => "B",
        }
    }

    pub(super) fn colors(self, focused: bool) -> (Rgb, Rgb) {
        let palette = crate::chrome::palette();
        let accent = match self {
            Self::Orchestrator => palette.dot_blue,
            Self::Board => palette.accent,
        };
        let base = if focused {
            palette.card_selected
        } else {
            palette.band
        };
        let blend = |a: u8, b: u8| ((u16::from(a) * 3 + u16::from(b)) / 4) as u8;
        (
            Rgb::new(
                blend(base.r, accent.r),
                blend(base.g, accent.g),
                blend(base.b, accent.b),
            ),
            palette.fg,
        )
    }
}

pub(super) struct PaneChrome {
    pub(super) role: PaneRole,
    terminal: Terminal,
    cols: u16,
    bytes: String,
}

impl PaneChrome {
    fn new(role: PaneRole) -> Self {
        Self {
            role,
            terminal: Terminal::new(GridSize::new(1, 1)),
            cols: 1,
            bytes: String::new(),
        }
    }

    fn snapshot(&mut self, cols: u16, title: &str, focused: bool) -> FrameSnapshot {
        if self.cols != cols {
            self.terminal = Terminal::new(GridSize::new(cols, 1));
            self.cols = cols;
            self.bytes.clear();
        }
        let (bg, fg) = self.role.colors(focused);
        let label = header_text(self.role, title, usize::from(cols));
        let bytes = format!(
            "\x1b[?25l\x1b[?7l\x1b[0;48;2;{};{};{};38;2;{};{};{}m\x1b[H\x1b[2K\x1b[1m{label}",
            bg.r, bg.g, bg.b, fg.r, fg.g, fg.b
        );
        if self.bytes != bytes {
            Stream::new().feed(bytes.as_bytes(), &mut self.terminal);
            self.bytes = bytes;
        }
        let mut snapshot = FrameSnapshot::from_terminal(&mut self.terminal);
        snapshot.focused = false;
        snapshot.cursor.visible = false;
        snapshot
    }
}

/// Reserve a complete padded cell row only when a terminal row still fits.
/// The same geometry is used by pty sizing, pointer mapping and drawing.
pub(super) fn split_rect(
    outer: PaneRectApp,
    metrics: noa_font::Metrics,
    padding: GridPadding,
) -> (Option<PaneRectApp>, PaneRectApp) {
    let height = (metrics.cell_h + padding.top + padding.bottom)
        .ceil()
        .max(1.0) as u32;
    if outer.w == 0 || outer.h < height.saturating_mul(2) {
        return (None, outer);
    }
    (
        Some(PaneRectApp::new(outer.x, outer.y, outer.w, height)),
        PaneRectApp::new(outer.x, outer.y + height, outer.w, outer.h - height),
    )
}

fn header_text(role: PaneRole, title: &str, cols: usize) -> String {
    if cols == 0 {
        return String::new();
    }
    let label = role.label(cols.saturating_sub(2));
    let mut text = if cols >= label.len() + 2 {
        format!("[{label}]")
    } else {
        label.to_string()
    };
    if cols >= text.len() + 4 && !title.is_empty() {
        text.push_str(" · ");
        let mut room = cols - UnicodeWidthStr::width(text.as_str());
        for ch in title.chars().filter(|ch| !ch.is_control()) {
            let width = ch.width().unwrap_or(0);
            if width > room {
                break;
            }
            text.push(ch);
            room -= width;
        }
    }
    text
}

pub(super) struct RoleHeader {
    pub(super) pane: PaneId,
    pub(super) rect: PaneRectApp,
    title: String,
    cols: u16,
    focused: bool,
    bg: Rgb,
}

impl RoleHeader {
    pub(super) fn render_id(&self) -> RenderPaneId {
        RenderPaneId::new(self.pane.get() | ROLE_RENDER_ID_BIT)
    }

    pub(super) fn overlay_group(&self) -> noa_render::OverlayGroup {
        noa_render::OverlayGroup {
            outer: render_pane_rect(self.rect),
            header: Some((render_pane_rect(self.rect), self.bg)),
            panes: vec![self.render_id()],
            focused: false,
        }
    }
}

impl App {
    pub(super) fn orchestration_header_at(
        &self,
        window: WindowId,
        point: split_tree::Point,
    ) -> Option<PaneId> {
        self.orchestration_headers(window)
            .into_iter()
            .find(|header| header.rect.contains(point))
            .map(|header| header.pane)
    }

    pub(super) fn mark_orchestration_role(&mut self, pane: u64, role: PaneRole) {
        let Ok((window, pane)) = self.resolve_ipc_pane(pane) else {
            return;
        };
        let Some(surface) = self
            .windows
            .get_mut(&window)
            .and_then(|state| state.surfaces.get_mut(&pane))
        else {
            return;
        };
        // A board may orchestrate its own workers. Its Board identity wins.
        if let Some(chrome) = surface.orchestration_chrome.as_mut() {
            if chrome.role == PaneRole::Board || chrome.role == role {
                return;
            }
            chrome.role = role;
        } else {
            surface.orchestration_chrome = Some(PaneChrome::new(role));
        }
        self.relayout_and_resize_window(window);
        self.request_window_redraw(window);
    }

    pub(super) fn orchestration_headers(&self, window: WindowId) -> Vec<RoleHeader> {
        let Some(state) = self.windows.get(&window) else {
            return Vec::new();
        };
        // Like pane-anchored floats, labels wait for the zoom tween to finish
        // rather than cover siblings while their clip is moving.
        if state.live_split_zoom_anim().is_some() {
            return Vec::new();
        }
        let Some(gpu) = self.gpu.as_ref() else {
            return Vec::new();
        };
        let metrics = gpu.fonts.get(self.window_font_px(window)).metrics();
        zoom_resize_targets(
            &state.split_tree,
            state.draw_zoomed(),
            self.window_pane_bounds(window),
        )
        .into_iter()
        .filter(|(pane, _)| state.draw_zoomed().is_none_or(|zoomed| zoomed == *pane))
        .filter_map(|(pane, outer)| {
            let surface = state.surfaces.get(&pane)?;
            let chrome = surface.orchestration_chrome.as_ref()?;
            let (strip, _) = split_rect(outer, metrics, self.padding);
            let rect = strip?;
            let title = self
                .session_store
                .get(&Self::session_card_id(window, pane))
                .map(|card| card.name_override.as_deref().unwrap_or(&card.name))
                .unwrap_or("")
                .to_string();
            let cols = ((rect.w as f32 - self.padding.left - self.padding.right).max(0.0)
                / metrics.cell_w)
                .floor()
                .clamp(1.0, f32::from(u16::MAX)) as u16;
            let focused = state.focused_pane == pane;
            Some(RoleHeader {
                pane,
                rect,
                title,
                cols,
                focused,
                bg: chrome.role.colors(focused).0,
            })
        })
        .collect()
    }
}

impl WindowState {
    pub(super) fn orchestration_header_snapshot(&mut self, header: &RoleHeader) -> FrameSnapshot {
        self.surfaces
            .get_mut(&header.pane)
            .unwrap()
            .orchestration_chrome
            .as_mut()
            .unwrap()
            .snapshot(header.cols, &header.title, header.focused)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn metrics() -> noa_font::Metrics {
        noa_font::Metrics {
            cell_w: 8.0,
            cell_h: 16.0,
            ascent: 12.0,
            descent: 4.0,
            line_gap: 0.0,
            underline_position: -2.0,
            underline_thickness: 1.0,
        }
    }

    #[test]
    fn header_preserves_the_terminal_extent_without_overlaying_its_cells() {
        let outer = PaneRectApp::new(100, 50, 400, 300);
        let padding = GridPadding {
            left: 8.0,
            right: 8.0,
            top: 4.0,
            bottom: 4.0,
        };
        let (strip, content) = split_rect(outer, metrics(), padding);
        let strip = strip.unwrap();
        assert_eq!(strip, PaneRectApp::new(100, 50, 400, 24));
        assert_eq!(content, PaneRectApp::new(100, 74, 400, 276));
        assert_eq!(strip.bottom(), content.y);
        assert_eq!(content.bottom(), outer.bottom());
        assert_eq!(
            grid_size_for_pane_rect(content, metrics(), padding).rows,
            16
        );
        let tiny = PaneRectApp::new(100, 50, 400, 47);
        assert_eq!(split_rect(tiny, metrics(), padding), (None, tiny));
    }

    #[test]
    fn narrow_labels_keep_the_role_and_titles_cannot_inject_terminal_controls() {
        for role in [PaneRole::Orchestrator, PaneRole::Board] {
            for cols in 1..80 {
                let label = header_text(role, "日本語の長いタイトル\x1b[2J\nname", cols);
                assert!(
                    UnicodeWidthStr::width(label.as_str()) <= cols,
                    "{cols}: {label}"
                );
                assert!(!label.chars().any(char::is_control));
                assert!(label.contains(role.label(cols.saturating_sub(2))));
            }
        }
        assert_eq!(header_text(PaneRole::Orchestrator, "title", 6), "[ORCH]");
        assert_eq!(header_text(PaneRole::Orchestrator, "title", 1), "O");
    }

    #[test]
    fn both_focus_states_render_the_role_without_a_cursor() {
        let mut chrome = PaneChrome::new(PaneRole::Orchestrator);
        for focused in [false, true, false] {
            let snapshot = chrome.snapshot(40, "build coordinator", focused);
            let row: String = snapshot.rows[0].cells.iter().map(|cell| cell.ch).collect();
            assert!(
                row.starts_with("[Orchestrator] · build coordinator"),
                "{row}"
            );
            assert!(!snapshot.cursor.visible);
        }
        let snapshot = chrome.snapshot(6, "title", true);
        let row: String = snapshot.rows[0].cells.iter().map(|cell| cell.ch).collect();
        assert_eq!(row, "[ORCH]");
    }
}
