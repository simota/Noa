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
use crate::session_store::{SessionCard, SessionDelta, StatusDot, status_dot};
use crate::sidebar::{
    AgentKind, CARD_MENU_ITEMS, CardLines, CardRects, SidebarMetrics, SidebarRect,
    agent_display_name, card_lines, classify_agent, icon_glyph,
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
/// A `Bell`/`Attention` for the window that holds OS focus is also dropped
/// (`window_os_focused`), mirroring the OSC 9/777 suppression (FR-16): the
/// user is looking at that window, and focus is the only thing that clears the
/// flags, so applying them would leave a marker nothing clears until the
/// window loses and regains focus.
fn session_delta_should_apply(
    delta: &SessionDelta,
    window_eligible: bool,
    window_os_focused: bool,
) -> bool {
    match delta {
        SessionDelta::Upsert { .. } => window_eligible,
        SessionDelta::Bell { .. } | SessionDelta::Attention { .. } => {
            window_eligible && !window_os_focused
        }
        SessionDelta::Remove { .. }
        | SessionDelta::Branch { .. }
        | SessionDelta::Rename { .. }
        | SessionDelta::Process { .. } => true,
    }
}

/// Sidebar palette: the shared `crate::chrome` palette (polarity chosen from
/// the terminal theme at startup) so the sidebar and the tab overview stay
/// visually unified. The card faces double as the toolbar `+` button's hover
/// fill, and `fg`/`dim_fg` as its glyph rest/hover tones.
fn chrome() -> &'static crate::chrome::ChromePalette {
    crate::chrome::palette()
}

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

// Brand accents for recognized AI agents (agent branding). Truecolor, applied
// to the process row + header whenever the process classifies, busy or idle.
const AGENT_CLAUDE_FG: Rgb = Rgb::new(0xd9, 0x77, 0x57); // Anthropic clay
const AGENT_CODEX_FG: Rgb = Rgb::new(0x10, 0xa3, 0x7f); // OpenAI teal
const AGENT_AGY_FG: Rgb = Rgb::new(0x42, 0x85, 0xf4); // Google blue

/// The glyph + accent color + display label for a card's running process. A
/// recognized agent gets its brand glyph/color/name regardless of busy; a
/// generic process keeps the busy/idle dot semantics (green `✳` while running,
/// dim `❯` while idle). Glyphs: `✳` (proven), `◆` for Codex (a hexagon risks
/// tofu), `✦` for agy (a distinct four-point star; `★` is the safe fallback).
fn process_badge(process: &str, busy: bool) -> (String, Rgb) {
    match classify_agent(process) {
        AgentKind::ClaudeCode => (
            format!("✳ {}", agent_display_name(AgentKind::ClaudeCode, process)),
            AGENT_CLAUDE_FG,
        ),
        AgentKind::Codex => (
            format!("◆ {}", agent_display_name(AgentKind::Codex, process)),
            AGENT_CODEX_FG,
        ),
        AgentKind::Agy => (
            format!("✦ {}", agent_display_name(AgentKind::Agy, process)),
            AGENT_AGY_FG,
        ),
        AgentKind::Generic => {
            let glyph = if busy { "✳" } else { "❯" };
            let fg = if busy {
                chrome().dot_green
            } else {
                chrome().dim_fg
            };
            (format!("{glyph} {process}"), fg)
        }
    }
}

/// A project icon's tint (FR-9 mockup parity), so the icon column carries a
/// little color rather than flat gray.
fn icon_color(icon: crate::session_store::IconKind) -> Rgb {
    use crate::session_store::IconKind;
    match icon {
        IconKind::Rust => Rgb::new(0xd9, 0x82, 0x5a),
        IconKind::Node => Rgb::new(0x6c, 0xc2, 0x4a),
        IconKind::Terraform => Rgb::new(0x84, 0x4f, 0xba),
        IconKind::Go => Rgb::new(0x4c, 0xb9, 0xd4),
        IconKind::Python => Rgb::new(0x5a, 0x9f, 0xd4),
        IconKind::Git => Rgb::new(0xe0, 0x6c, 0x4e),
        IconKind::Folder => chrome().dim_fg,
    }
}

/// The card's status dot with the attention blink applied (FR-A1): while an
/// attention marker is in its hidden phase, show the underlying status (bell /
/// busy / idle) instead of the red attention dot, so the dot blinks red↔status.
/// A settled or visible-phase attention keeps the red dot.
fn effective_status_dot(card: &SessionCard, attention_marker: bool) -> StatusDot {
    if card.attention && !attention_marker {
        if card.unread_bell {
            StatusDot::Yellow
        } else if card.busy {
            StatusDot::Blue
        } else {
            StatusDot::Green
        }
    } else {
        status_dot(card)
    }
}

/// The dot glyph color for a card's status (FR-11), driven by the pure
/// `status_dot` mapping in `session_store` (AC-13).
fn status_dot_rgb(dot: StatusDot) -> Rgb {
    match dot {
        StatusDot::Blue => chrome().dot_blue,
        StatusDot::Green => chrome().dot_green,
        StatusDot::Yellow => chrome().dot_yellow,
        StatusDot::Red => chrome().dot_red,
    }
}

/// The label appended to a card's process row while it awaits the user's reply
/// (FR-16), e.g. `✳ Claude Code · 応答待ち`.
const ATTENTION_LABEL: &str = "応答待ち";

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
/// cells (used by the `…` menu popup and the selected-card accent bar); `None`
/// leaves the underlying background showing. `bold` renders the run's cells in
/// the bold weight (card names).
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

/// One session card's own rounded-card render: its window-space rect, the
/// per-card grid, background color, selection flag, and the text runs in the
/// card's local texture space. Only fully-visible cards get one; partially
/// scrolled cards stay flat on the backdrop.
struct SidebarCardDraw {
    rect: SidebarRect,
    grid: GridSize,
    bg: Rgb,
    selected: bool,
    /// A pending interaction request (FR-16): the card gets a red ring + glow
    /// instead of the blue focus ring. Held steady while the request is pending
    /// (the dot/label still blink) so the ring reads as a stable "this session
    /// needs you" marker rather than flickering in and out.
    attention: bool,
    runs: Vec<SidebarTextRun>,
}

/// The open card `…` menu popup, composited above the cards so a rounded card
/// can never hide it.
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
pub(super) struct SidebarDrawModel {
    inset: u32,
    height: u32,
    scale: f32,
    /// The scaled height of one card, sizing the per-card scratch texture.
    card_h: u32,
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

pub(super) use palette::{
    ScrollThumb, draw_bell_flash, draw_command_palette_card, draw_confirm_dialog_card,
    draw_scrollbar_thumbs, draw_toast_card,
};
pub(super) use render::draw_sidebar_band;

