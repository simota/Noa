//! Session-sidebar subsystem — the `App`-side glue that turns the pure
//! [`crate::session_store`] + [`crate::sidebar`] modules into a live feature:
//! applying io-thread deltas, garbage-collecting torn-down sessions,
//! per-window toggle + grid-first resize, click routing, and the draw path.
//!
//! Everything visual/windowing lives here (not in the two pure modules), so
//! `session_store.rs`/`sidebar.rs` stay GUI-agnostic (NFR-6). The draw path
//! reads only the store and the pure layout — it never locks a `Terminal`
//! (NFR-1/AC-17).

use super::*;
use crate::session_store::{AgentKind, SessionCard, SessionDelta};
use crate::sidebar::{
    CARD_MENU_ITEMS, CardLines, CardRects, SidebarMetrics, SidebarRect, agent_display_name,
    card_lines,
};
use noa_core::Rgb;
use std::collections::HashSet;

/// Whether an io-thread [`SessionDelta`] targeting a window with the given
/// sidebar eligibility should reach the store (FR-14/AC-16b). Quick-terminal
/// windows are ineligible and must never get a card, so their io-thread-posted
/// `Upsert`/`Bell` are dropped here at the apply boundary — a QT pane shares the
/// app-wide publish gate, so with a sidebar open elsewhere it would otherwise
/// leak a card into every window's sidebar. App-originated `Remove`/`Branch`/
/// `Rename` only ever target real windows, so they pass through unconditionally
/// (and dropping a QT `Remove` would be harmless anyway).
///
/// Only the selected pane in the OS-focused window is already being read.
/// Notifications from its sibling panes must still reach the sidebar.
fn session_delta_should_apply(
    delta: &SessionDelta,
    window_eligible: bool,
    pane_os_focused: bool,
) -> bool {
    match delta {
        SessionDelta::Upsert { .. }
        | SessionDelta::Progress { .. }
        | SessionDelta::AgentStatus { .. }
        | SessionDelta::ProgressComplete { .. }
        | SessionDelta::ProgressError { .. } => window_eligible,
        SessionDelta::Bell { .. } | SessionDelta::Attention { .. } => {
            window_eligible && !pane_os_focused
        }
        SessionDelta::Remove { .. }
        | SessionDelta::Branch { .. }
        | SessionDelta::Rename { .. }
        | SessionDelta::Process { .. }
        | SessionDelta::Metrics { .. } => true,
    }
}

/// Sidebar palette: the shared `crate::chrome` palette (polarity chosen from
/// the terminal theme at startup) so the sidebar and the tab overview stay
/// visually unified. The card faces double as the toolbar `+` button's hover
/// fill, and `fg`/`dim_fg` as its glyph rest/hover tones.
fn chrome() -> crate::chrome::ChromePalette {
    crate::chrome::palette()
}

// CardRects already includes the sidebar's own insets. Terminal padding would
// shift this separate grid and can collapse the status and project-icon slots.
const SIDEBAR_GRID_PADDING: GridPadding = GridPadding::ZERO;

// Flat card color tuning. The base row mostly follows the resolved terminal
// panel background so it does not read as a separate navy card surface.
const SIDEBAR_CARD_PANEL_BG_MIX: f32 = 0.08;
const SIDEBAR_CARD_SELECTED_BG_MIX: f32 = 0.065;
const SIDEBAR_CARD_HOVER_BG_MIX: f32 = 0.035;
const SIDEBAR_CARD_AUTO_FLASH_ACCENT_MIX: f32 = 0.12;
const SIDEBAR_CARD_ATTENTION_FLASH_MIX: f32 = 0.10;
const SIDEBAR_CARD_PROGRESS_FLASH_MIX: f32 = 0.12;

// Toolbar `+` button chrome (logical px). Borderless: just the `+` glyph at
// rest, with a subtle rounded fill + brighter glyph on hover.
const TOOLBAR_BUTTON_RADIUS: f32 = crate::chrome::RADIUS_SM;
/// The `+` glyph geometry (logical px): each arm's length and the bar thickness.
const TOOLBAR_PLUS_ARM: f32 = 12.0;
const TOOLBAR_PLUS_THICKNESS: f32 = 2.0;

// Seam treatment between the sidebar band and the terminal panes (logical px,
// scaled at draw time): a soft shadow the band casts rightward plus a crisp
// 1px hairline, so the two independently-themed surfaces meet with depth
// instead of a bare color boundary.
const SEAM_SHADOW_WIDTH: f32 = 5.0;
const SEAM_HAIRLINE_WIDTH: f32 = 1.0;

/// Pointer travel (logical px, scaled at use) that promotes a card press to a
/// drag-reorder. Below it a press-then-release stays a plain card-select click.
const SIDEBAR_DRAG_THRESHOLD: f32 = 5.0;

/// Thickness (logical px, scaled) of the drop-indicator line drawn at the
/// insertion gap during an active card drag.
const SIDEBAR_DROP_INDICATOR_H: f32 = 2.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ProgressBarKind {
    Determinate(u8),
    Indeterminate,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct ProgressBar {
    kind: ProgressBarKind,
    color: Rgb,
}

fn card_progress_bar(card: &SessionCard) -> Option<ProgressBar> {
    use noa_grid::TerminalProgress;
    let progress = card.progress?;
    let (kind, color) = match progress {
        TerminalProgress::Normal(value) => {
            (ProgressBarKind::Determinate(value.get()), chrome().dot_blue)
        }
        TerminalProgress::Error(value) => (
            value.map_or(ProgressBarKind::Indeterminate, |value| {
                ProgressBarKind::Determinate(value.get())
            }),
            chrome().dot_red,
        ),
        TerminalProgress::Indeterminate => (ProgressBarKind::Indeterminate, chrome().dot_blue),
        TerminalProgress::Paused(value) => (
            value.map_or(ProgressBarKind::Indeterminate, |value| {
                ProgressBarKind::Determinate(value.get())
            }),
            chrome().dot_yellow,
        ),
    };
    Some(ProgressBar { kind, color })
}

/// Linear mix of two chrome tones (`t` = 0 → `a`, 1 → `b`), for derived states
/// the palette has no dedicated slot for (the card hover face sits between the
/// resting and selected faces).
fn mix_rgb(a: Rgb, b: Rgb, t: f32) -> Rgb {
    let mix = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * t).round() as u8;
    Rgb::new(mix(a.r, b.r), mix(a.g, b.g), mix(a.b, b.b))
}

fn sidebar_card_bg(panel_bg: Rgb) -> Rgb {
    mix_rgb(panel_bg, chrome().card, SIDEBAR_CARD_PANEL_BG_MIX)
}

fn sidebar_selected_card_bg(panel_bg: Rgb) -> Rgb {
    mix_rgb(panel_bg, chrome().fg, SIDEBAR_CARD_SELECTED_BG_MIX)
}

fn sidebar_hover_card_bg(panel_bg: Rgb) -> Rgb {
    mix_rgb(panel_bg, chrome().fg, SIDEBAR_CARD_HOVER_BG_MIX)
}

fn sidebar_auto_flash_card_bg(panel_bg: Rgb) -> Rgb {
    mix_rgb(
        sidebar_selected_card_bg(panel_bg),
        chrome().accent,
        SIDEBAR_CARD_AUTO_FLASH_ACCENT_MIX,
    )
}

fn sidebar_attention_flash_card_bg(panel_bg: Rgb, selected: bool) -> Rgb {
    let base = if selected {
        sidebar_selected_card_bg(panel_bg)
    } else {
        sidebar_card_bg(panel_bg)
    };
    mix_rgb(base, chrome().dot_red, SIDEBAR_CARD_ATTENTION_FLASH_MIX)
}

fn sidebar_progress_flash_card_bg(panel_bg: Rgb, selected: bool, kind: ProgressFlashKind) -> Rgb {
    let base = if selected {
        sidebar_selected_card_bg(panel_bg)
    } else {
        sidebar_card_bg(panel_bg)
    };
    let accent = match kind {
        ProgressFlashKind::Success => chrome().dot_green,
        ProgressFlashKind::Error => chrome().dot_red,
    };
    mix_rgb(base, accent, SIDEBAR_CARD_PROGRESS_FLASH_MIX)
}

/// OSC 9/777 means a notification exists, not necessarily that a program is
/// blocked on user input. Keep the label accurate until a dedicated
/// response-required protocol exists.
const ATTENTION_LABEL: &str = "notice";

fn rgb_to_rgba(color: Rgb) -> [f32; 4] {
    [
        color.r as f32 / 255.0,
        color.g as f32 / 255.0,
        color.b as f32 / 255.0,
        1.0,
    ]
}

/// One positioned text run in a synthetic sidebar grid (already converted from
/// the pure layout's pixel rects to cell coordinates). `bg` fills the run's
/// cells (used by the `…` menu popup and selected-card treatment); `None`
/// leaves the underlying background showing. `bold` renders the run's cells in
/// the bold weight (card names).
#[derive(Clone, Debug, PartialEq)]
struct SidebarTextRun {
    col: u16,
    row: u16,
    text: String,
    fg: Rgb,
    bg: Option<Rgb>,
    bold: bool,
}

impl SidebarTextRun {
    fn new(col: u16, row: u16, text: String, fg: Rgb) -> Self {
        Self {
            col,
            row,
            text,
            fg,
            bg: None,
            bold: false,
        }
    }
}

/// One session card's window-space rect, grid, interaction state, and local
/// text runs. Every visible card gets a texture; a partially scrolled card
/// carries a clipping `src_uv`.
#[derive(Clone, Debug, PartialEq)]
struct SidebarCardDraw {
    rect: SidebarRect,
    grid: GridSize,
    bg: Rgb,
    /// A bounded arrival/progress cue also fills an otherwise resting row.
    flash_fill: bool,
    selected: bool,
    hovered: bool,
    /// Actual `OSC 9;4` progress along the card's bottom edge. This is kept
    /// separate from the textual state so progress remains unambiguous.
    progress_bar: Option<ProgressBar>,
    runs: Vec<SidebarTextRun>,
    /// Source-UV sub-rect `[u, v, w, h]` for sampling the full-height card
    /// texture. `[0, 0, 1, 1]` for a fully-visible card; a partial (edge-
    /// clipped) card samples only its visible vertical slice so its
    /// full-height content is not squashed into the clipped `rect`.
    src_uv: [f32; 4],
}

/// The open card `…` menu popup, composited above the cards so a rounded card
/// can never hide it.
#[derive(Clone, Debug, PartialEq)]
struct SidebarMenuDraw {
    rect: SidebarRect,
    grid: GridSize,
    runs: Vec<SidebarTextRun>,
}

/// The full per-frame sidebar draw model. Built with only the store + pure
/// layout (no `Terminal` lock — AC-17). `runs` is the flat dark backdrop
/// (header/toolbar chrome + every card's text) rasterized into the band
/// texture; `cards` are the per-card rounded overlays drawn on top for fully
/// visible cards; `menu` is the optional popup above them all.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct SidebarDrawModel {
    inset: u32,
    height: u32,
    scale: f32,
    /// The scaled height of one card, sizing the per-card scratch texture.
    card_h: u32,
    /// The scaled width of one card (the inset minus the horizontal card
    /// margin on both sides), sizing the per-card scratch texture.
    card_w: u32,
    grid: GridSize,
    runs: Vec<SidebarTextRun>,
    /// The toolbar `+` button: its window-space rect and whether the pointer is
    /// over it. Drawn as its own rounded chrome tile with a geometric `+` glyph
    /// (two centered bars — not a font glyph, which the coarse cell grid can't
    /// center in a small tile) so it reads as a real, hoverable button.
    new_button: SidebarRect,
    new_button_hover: bool,
    cards: Vec<SidebarCardDraw>,
    menu: Option<SidebarMenuDraw>,
    /// The floating copy of the card being drag-reordered, positioned under the
    /// cursor and composited above every static card. `None` unless a drag is
    /// active in this window.
    dragging: Option<SidebarCardDraw>,
    /// The accent drop-indicator line at the insertion gap during an active
    /// drag, or `None`.
    drop_indicator: Option<SidebarRect>,
    /// `background-opacity` (FR: sidebar translucency), applied to the flat
    /// band and card backdrops so they show the macOS blur the same way the
    /// terminal panes do. The divider hairline and the `…` menu popup stay
    /// opaque regardless (crisp seam edge / readable transient overlay).
    background_opacity: f32,
}

/// The `SessionWindowId`s belonging to `target` among `pairs` (spec
/// `sidebar-per-window-sessions` R1/R2): a pure, winit-independent derivation
/// so the sidebar's group-scoping can be unit-tested without a window
/// (AC-10). Native tabs share one `WindowGroupId` but have distinct winit
/// `WindowId`s, so this is what makes sibling tabs' sessions show up
/// together in the sidebar.
fn windows_in_group(
    pairs: impl IntoIterator<Item = (SessionWindowId, WindowGroupId)>,
    target: WindowGroupId,
) -> HashSet<SessionWindowId> {
    pairs
        .into_iter()
        .filter(|(_, group)| *group == target)
        .map(|(window_id, _)| window_id)
        .collect()
}

mod interaction;
mod model;
mod palette;
mod render;
mod state;
mod text;

pub(super) use palette::{ScrollThumb, draw_bell_flash, draw_scrollbar_thumbs};
// The wgpu modal cards are the non-macOS fallback; macOS composites native
// AppKit cards instead (`crate::macos_overlay`).
#[cfg(not(target_os = "macos"))]
pub(super) use palette::{
    context_menu_geometry, draw_command_palette_card, draw_confirm_dialog_card, draw_context_menu,
    draw_process_monitor_card, draw_tab_bar, draw_theme_settings_card, draw_toast_card,
    tab_bar_geometry,
};
pub(super) use render::{SidebarRasterCacheKey, draw_sidebar_band};
