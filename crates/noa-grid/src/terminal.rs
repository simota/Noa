//! [`Terminal`] — the top-level state model. Implements [`noa_vt::Handler`],
//! dispatching parsed operations onto the active [`Screen`] and queuing report
//! replies (DA/DSR) for the pty writer.

mod handler;
mod kitty_graphics;
mod reports;
mod seed;

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use crate::cell::{Hyperlink, HyperlinkId};
use crate::charset::CharsetState;
use crate::cursor::CursorStyle;
use crate::kitty::ImageStore;
use crate::kitty_keyboard::KittyKeyboard;
use crate::modes::ModeState;
use crate::osc::{Notification, Osc52Policy, TerminalColors};
use crate::screen::Screen;
use crate::search::SearchMatch;
use crate::selection::SelectionPoint;
use noa_core::{CellAttrs, Color, GridSize, Point};
use noa_vt::{EraseDisplay, SgrAttr};

/// Cap on the `XTWINOPS` title stack (`CSI 22/23 t`), mirroring the
/// unbounded-growth guardrails already used for `Screen::scrollback` and the
/// parser's `MAX_OSC_BYTES`/`MAX_PARAMS`. The oldest entry is evicted first.
const TITLE_STACK_CAP: usize = 64;

/// Cap on queued desktop notifications (OSC 9 / OSC 777) awaiting drain by the
/// app layer. A misbehaving program can emit these faster than the main thread
/// consumes them; the oldest entry is evicted first, same as the title stack.
const NOTIFICATION_QUEUE_CAP: usize = 32;

/// Cap on distinct OSC 8 hyperlinks. Cells (including scrollback rows) store
/// indices into the registry, so entries can never be evicted safely; once the
/// cap is hit, further *new* links print as plain text instead. This bounds
/// memory against a program streaming unique URIs forever.
pub(crate) const HYPERLINK_REGISTRY_CAP: usize = 8192;
const _: () = assert!(HYPERLINK_REGISTRY_CAP < u16::MAX as usize);

/// Cap on recorded OSC 133 shell marks. Marks whose rows scrolled out of
/// trimmed history are useless (`scroll_to_prompt` skips them), so those are
/// pruned first; if every mark is still reachable, the oldest is evicted.
pub(crate) const SHELL_MARK_CAP: usize = 4096;

pub struct Terminal {
    pub primary: Screen,
    /// Alternate screen — populated in inc≥2.
    pub alt: Option<Screen>,
    pub active_is_alt: bool,
    /// Generation of the active screen's coordinate space. This is advanced
    /// whenever a control sequence replaces the active [`Screen`] without
    /// necessarily changing `active_is_alt`.
    screen_generation: u64,
    pub modes: ModeState,
    /// G0/G1 designation + active (GL) slot for `SCS`/`SO`/`SI`.
    charset: CharsetState,
    /// Window title from OSC 0/2 (stored; unused by the inc-1 renderer).
    pub title: String,
    /// Current working directory reported by OSC 7 as a decoded absolute path.
    pub cwd: Option<String>,
    /// OSC 8 hyperlink registry. Cells store indices into this table.
    pub hyperlinks: Vec<Hyperlink>,
    /// Reverse lookup for [`Self::hyperlinks`] so repeated OSC 8 sequences
    /// with the same target dedupe in O(1) instead of a linear registry scan.
    hyperlink_index: HashMap<Hyperlink, usize>,
    /// OSC 133 shell integration marks recorded at cursor positions.
    pub shell_marks: Vec<ShellIntegrationMark>,
    /// Dynamic colors set through safe OSC 4/10/11/12 sequences.
    pub colors: TerminalColors,
    /// Policy for OSC 52 clipboard writes/queries.
    pub osc52_policy: Osc52Policy,
    /// Whether `CSI 21 t` may report the window title (`title-report`).
    /// Off by default, matching Ghostty: the reply echoes program-settable
    /// text (OSC 0/2) back into the pty as input — an injection vector.
    pub title_report: bool,
    /// XTMODKEYS modifyOtherKeys is at level 2 (`CSI > 4 ; 2 m`), matching
    /// Ghostty's `modify_other_keys_2` flag. Levels 0/1 clear it.
    pub modify_other_keys_2: bool,
    pub size: GridSize,
    /// Bytes the terminal must write back to the pty (query replies).
    pub pending_writes: Vec<u8>,
    /// Whether [`Self::take_pending_writes`] may return terminal-generated
    /// replies. Remote replica terminals disable this because the server-side
    /// terminal remains the sole reply authority.
    reply_writes_enabled: bool,
    /// Text payloads accepted by OSC 52 and ready for the app clipboard layer.
    pub pending_clipboard_writes: Vec<String>,
    /// OSC 52 clipboard *read* requests (`OSC 52;<t>;?`) the policy allowed.
    /// Each entry is the selection target to echo in the reply (e.g. `"c"`).
    /// The grid can't read the system clipboard, so the app layer fulfills
    /// these and writes the base64 reply back to the pty.
    pub pending_clipboard_reads: Vec<String>,
    /// Desktop notifications requested via OSC 9 / OSC 777, awaiting drain by
    /// [`Terminal::take_pending_notifications`]. Bounded at
    /// [`NOTIFICATION_QUEUE_CAP`]; the oldest is evicted on overflow.
    pending_notifications: VecDeque<Notification>,
    /// Set by `BEL` (`0x07`); drained by [`Terminal::take_pending_bell`].
    pending_bell: bool,
    /// Cell size in pixels, from the last `noa-app` pixel-metrics update.
    /// Zero until the first resize (`CSI 16 t`).
    cell_width_px: u32,
    cell_height_px: u32,
    /// Text-area (grid content) size in pixels. Zero until the first resize
    /// (`CSI 14 t`).
    text_area_width_px: u32,
    text_area_height_px: u32,
    /// `XTWINOPS` window-title stack (`CSI 22/23 t`), window-title only —
    /// icon-title variants (`Ps[1] == 1`) are unsupported and no-op.
    title_stack: VecDeque<String>,
    /// Cursor style DECSCUSR 0 (`Default`) resets to. Seeded from
    /// `CursorStyle::default()`; `noa-app` overrides it from `cursor-style`.
    default_cursor_style: CursorStyle,
    /// Kitty keyboard protocol progressive-enhancement flag stacks (per screen).
    kitty_keyboard: KittyKeyboard,
    /// Kitty graphics image store (screen-independent; placements live on the
    /// screens). Owns decoded RGBA data and the global byte quota.
    pub kitty_images: ImageStore,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShellIntegrationMarkKind {
    PromptStart,
    InputStart,
    CommandStart,
    CommandEnd,
}

/// Direction for [`Terminal::scroll_to_prompt`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PromptJump {
    /// The nearest prompt above the current viewport top.
    Prev,
    /// The nearest prompt below the current viewport top.
    Next,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShellIntegrationMark {
    pub kind: ShellIntegrationMarkKind,
    pub point: SelectionPoint,
    pub exit_status: Option<i32>,
}

impl Terminal {
    pub fn new(size: GridSize) -> Self {
        Terminal {
            primary: Screen::new(size.cols, size.rows),
            alt: None,
            active_is_alt: false,
            screen_generation: 0,
            modes: ModeState::defaults(),
            charset: CharsetState::default(),
            title: String::new(),
            cwd: None,
            hyperlinks: Vec::new(),
            hyperlink_index: HashMap::new(),
            shell_marks: Vec::new(),
            colors: TerminalColors::default(),
            osc52_policy: Osc52Policy::default(),
            title_report: false,
            modify_other_keys_2: false,
            size,
            pending_writes: Vec::new(),
            reply_writes_enabled: true,
            pending_clipboard_writes: Vec::new(),
            pending_clipboard_reads: Vec::new(),
            pending_notifications: VecDeque::new(),
            pending_bell: false,
            cell_width_px: 0,
            cell_height_px: 0,
            text_area_width_px: 0,
            text_area_height_px: 0,
            title_stack: VecDeque::new(),
            default_cursor_style: CursorStyle::default(),
            kitty_keyboard: KittyKeyboard::default(),
            kitty_images: ImageStore::new(),
        }
    }

    /// The active Kitty keyboard progressive-enhancement flags for the current
    /// screen (main vs alternate). `0` means the legacy encoding is in effect.
    /// `noa-app` reads this to select its key-encoding path.
    pub fn kitty_keyboard_flags(&self) -> u8 {
        self.kitty_keyboard.flags(self.active_is_alt)
    }

    /// Identity of the active screen's coordinate space.
    pub const fn screen_generation(&self) -> u64 {
        self.screen_generation
    }

    /// Set the cursor style DECSCUSR 0 resets to, and apply it immediately as
    /// the active cursor style. Called by `noa-app` from the `cursor-style`
    /// config at surface creation.
    pub fn set_default_cursor_style(&mut self, style: CursorStyle) {
        self.default_cursor_style = style;
        self.active_mut().cursor.style = style;
    }

    /// The active screen.
    pub fn active(&self) -> &Screen {
        if self.active_is_alt {
            self.alt.as_ref().unwrap_or(&self.primary)
        } else {
            &self.primary
        }
    }

    pub fn scrollback_len(&self) -> usize {
        self.active().scrollback_len()
    }

    /// Number of currently retained addressable rows in the active screen.
    pub fn active_total_rows(&self) -> usize {
        self.active().total_rows()
    }

    /// Oldest retained session-absolute row coordinate.
    pub fn active_oldest_row(&self) -> usize {
        self.active().rows_evicted()
    }

    /// Exclusive end of the active screen's retained session-absolute range.
    pub fn active_next_row(&self) -> usize {
        self.active_oldest_row()
            .saturating_add(self.active_total_rows())
    }

    /// A row in the active screen's stable session-absolute space. `None`
    /// if `y` has been evicted or is beyond [`Self::active_next_row`].
    pub fn active_absolute_row(&self, y: usize) -> Option<crate::cell::Row> {
        self.active().absolute_row(y)
    }

    pub fn viewport_offset(&self) -> usize {
        self.active().viewport_offset()
    }

    pub fn scroll_viewport_up(&mut self, rows: usize) {
        self.active_mut().scroll_viewport_up(rows);
    }

    pub fn scroll_viewport_down(&mut self, rows: usize) {
        self.active_mut().scroll_viewport_down(rows);
    }

    pub fn scroll_viewport_to_top(&mut self) {
        self.active_mut().scroll_viewport_to_top();
    }

    pub fn scroll_viewport_to_bottom(&mut self) {
        self.active_mut().scroll_viewport_to_bottom();
    }

    /// Explicitly pin the active viewport against pty-output following. This
    /// is distinct from the ordinary `viewport_offset > 0` lock and therefore
    /// also works when activated at the live bottom.
    pub fn lock_viewport(&mut self) {
        self.active_mut().set_viewport_locked(true);
    }

    pub(crate) fn copy_mode_points(&self) -> Option<crate::screen::TrackedCopyModePoints> {
        self.active().copy_mode_points()
    }

    pub(crate) fn set_copy_mode_points(
        &mut self,
        cursor: SelectionPoint,
        anchor: Option<SelectionPoint>,
    ) {
        self.active_mut().set_copy_mode_points(cursor, anchor);
    }

    /// Release any copy-mode viewport locks, including a screen that became
    /// inactive after a primary/alternate-screen switch.
    pub fn unlock_viewport(&mut self) {
        self.primary.set_viewport_locked(false);
        if let Some(alt) = &mut self.alt {
            alt.set_viewport_locked(false);
        }
    }

    /// Clear every screen-local artifact owned by copy mode. This is
    /// deliberately independent of the active screen because a pty may switch
    /// primary/alternate screens while the app-level session is still alive.
    pub fn exit_copy_mode(&mut self) {
        self.primary.clear_selection();
        self.primary.clear_copy_mode_points();
        self.primary.set_viewport_locked(false);
        self.primary.scroll_viewport_to_bottom();
        if let Some(alt) = &mut self.alt {
            alt.clear_selection();
            alt.clear_copy_mode_points();
            alt.set_viewport_locked(false);
            alt.scroll_viewport_to_bottom();
        }
    }

    pub fn set_selection(&mut self, anchor: SelectionPoint, focus: SelectionPoint) {
        self.active_mut().set_selection(anchor, focus);
    }

    pub fn set_viewport_selection(&mut self, anchor: Point, focus: Point) {
        self.active_mut().set_viewport_selection(anchor, focus);
    }

    pub fn viewport_point_to_selection_point(&self, point: Point) -> SelectionPoint {
        self.active().viewport_point_to_selection_point(point)
    }

    /// Rows evicted from the active screen's scrollback over its lifetime.
    /// Selection coordinates shift down by the same amount, so a caller
    /// holding a `SelectionPoint` across events can re-align it.
    pub fn selection_rows_evicted(&self) -> usize {
        self.active().rows_evicted()
    }

    pub fn select_word_at_viewport_point(&mut self, point: Point) {
        self.active_mut().select_word_at_viewport_point(point);
    }

    /// Read-only word lookup at `point` for Quick Look force-click
    /// (REQ-QLK-2): the word text and its start point, without mutating
    /// selection state.
    pub fn word_at_viewport_point(&self, point: Point) -> Option<(String, Point)> {
        self.active().word_at_viewport_point(point)
    }

    pub fn select_line_at_viewport_point(&mut self, point: Point) {
        self.active_mut().select_line_at_viewport_point(point);
    }

    pub fn clear_selection(&mut self) {
        self.active_mut().clear_selection();
    }

    pub fn selected_text(&self) -> Option<String> {
        self.active().selected_text()
    }

    pub fn scrollback_text(&mut self) -> Option<String> {
        self.active_mut().scrollback_text()
    }

    /// Tail-bounded scrollback text (noa-server spec NFR-4 / FR-8): see
    /// [`crate::screen::Screen::scrollback_text_tail`].
    pub fn scrollback_text_tail(&mut self, max_bytes: usize) -> Option<(String, bool)> {
        self.active_mut().scrollback_text_tail(max_bytes)
    }

    /// Merge older plain-text history without disturbing either live screen.
    /// `trailing_wrapped` marks whether `text`'s last row soft-wraps into
    /// whatever content already sits at the merge boundary (see
    /// [`crate::screen::Screen::prepend_plain_text_history`]). Returns the
    /// number of source rows inserted before retention eviction.
    pub fn prepend_scrollback_text(&mut self, text: &str, trailing_wrapped: bool) -> usize {
        let inserted = self
            .primary
            .prepend_plain_text_history(text, trailing_wrapped);
        if inserted > 0 {
            for mark in &mut self.shell_marks {
                mark.point.y = mark.point.y.saturating_add(inserted);
            }
        }
        inserted
    }

    pub fn set_search_query(&mut self, query: impl Into<String>) {
        self.active_mut().set_search_query(query);
    }

    pub fn clear_search(&mut self) {
        self.active_mut().clear_search();
    }

    pub fn search_next(&mut self) -> Option<SearchMatch> {
        self.active_mut().search_next()
    }

    pub fn search_previous(&mut self) -> Option<SearchMatch> {
        self.active_mut().search_previous()
    }

    /// Whether the cursor sits at a shell-integration prompt/input line
    /// rather than mid-command output.
    ///
    /// Ghostty parity: `Terminal.cursorIsAtPrompt`, which checks the cursor's
    /// row `semantic_prompt` tag and then the cursor cell's `semantic_content`
    /// directly (input/prompt → true, output → false) — Ghostty tags every
    /// row and cell as it prints. `noa-grid` has no per-row/per-cell semantic
    /// tags, only the flat `shell_marks` vector, so this approximates the
    /// same answer by scanning marks from the cursor upward (most recently
    /// recorded first) for the nearest row-tagging mark: `CommandEnd`
    /// (OSC 133;D) tags no row and is skipped; the first remaining mark at or
    /// above the cursor decides — `PromptStart` / `InputStart` means the
    /// cursor is at a prompt, `CommandStart` means a command is still
    /// running. No such mark means shell integration hasn't tagged anything
    /// yet, so treat it as not-a-prompt. This agrees with Ghostty's per-cell
    /// check for every realistic OSC 133 sequence (idle prompt, typing input,
    /// running command, post-command before the next prompt, no integration
    /// at all).
    pub fn cursor_is_at_prompt(&self) -> bool {
        if self.active_is_alt {
            return false;
        }
        let cursor_abs = self.primary.rows_evicted()
            + self.primary.scrollback_len()
            + usize::from(self.primary.cursor.y);
        self.shell_marks
            .iter()
            .rev()
            .filter(|mark| mark.kind != ShellIntegrationMarkKind::CommandEnd)
            .find(|mark| mark.point.y <= cursor_abs)
            .is_some_and(|mark| {
                matches!(
                    mark.kind,
                    ShellIntegrationMarkKind::PromptStart | ShellIntegrationMarkKind::InputStart
                )
            })
    }

    /// Ghostty parity: `Termio.clearScreen(history=true)` — the `clear_screen`
    /// keybind (Cmd+K). An emulator-level clear on the alternate screen would
    /// corrupt a running full-screen program's own idea of the display, so
    /// this is a complete no-op there. Otherwise scrollback is always erased;
    /// at a shell-integration prompt the whole display is erased too and the
    /// return value tells the caller to write a form feed (`0x0C`) so the
    /// shell repaints its prompt. Mid-command (or with no shell integration
    /// at all), only the rows above the cursor are erased — the cursor's row
    /// becomes row 0 — since nothing above it is a prompt the shell would
    /// otherwise redraw.
    ///
    /// Returns whether the caller must write a form feed to the pty.
    pub fn clear_screen_and_scrollback(&mut self) -> bool {
        if self.active_is_alt {
            return false;
        }

        let at_prompt = self.cursor_is_at_prompt();
        let old_sb_len = self.primary.scrollback_len();
        let old_live_top = self.primary.rows_evicted() + old_sb_len;
        self.primary.erase_display(EraseDisplay::Scrollback);
        // `erase_display(Scrollback)` collapses the session-absolute
        // coordinate space by `old_sb_len`, re-anchoring surviving Kitty
        // placements accordingly (see the `EraseDisplay::Scrollback` arm in
        // `screen/edit.rs`). `shell_marks` live on `Terminal`, not `Screen`,
        // so that call can't reach them — collapse them here the same way:
        // drop marks anchored in the now-cleared history, shift survivors
        // down by the same amount.
        self.shell_marks.retain_mut(|mark| {
            if mark.point.y < old_live_top {
                false
            } else {
                mark.point.y -= old_sb_len;
                true
            }
        });

        if at_prompt {
            self.primary.erase_display(EraseDisplay::Complete);
            // Ghostty parity: the erased rows' shell-integration tags die
            // with them. Dropping every mark also guarantees a repeated
            // Cmd+K before the shell repaints its prompt returns `false`
            // (no double form feed).
            self.shell_marks.clear();
            self.primary.clear_search();
            true
        } else {
            let old_cursor_abs = self.primary.rows_evicted()
                + self.primary.scrollback_len()
                + usize::from(self.primary.cursor.y);
            self.primary.erase_rows_above_cursor();
            let new_cursor_abs = self.primary.rows_evicted()
                + self.primary.scrollback_len()
                + usize::from(self.primary.cursor.y);
            let shift = old_cursor_abs - new_cursor_abs;
            // Marks above the cursor's row were just erased along with their
            // rows (Ghostty parity: a row's tag dies with the row); survivors
            // shift down by the same amount the cursor's own row did.
            self.shell_marks.retain_mut(|mark| {
                if mark.point.y < old_cursor_abs {
                    false
                } else {
                    mark.point.y -= shift;
                    true
                }
            });
            self.primary.clear_search();
            false
        }
    }

    pub fn clear_scrollback(&mut self) {
        self.primary.clear_scrollback();
    }

    /// Set the scrollback byte limit at runtime (`0` disables scrollback),
    /// evicting immediately. Applies to the primary screen; the alternate
    /// screen keeps no history. `noa-app` calls this from the `scrollback-limit`
    /// config at surface creation.
    pub fn set_scrollback_limit_bytes(&mut self, bytes: usize) {
        self.primary.set_scrollback_limit_bytes(bytes);
    }

    /// Set the Kitty/SIXEL image storage byte budget (`image-storage-limit`),
    /// evicting immediately if the new limit is smaller.
    pub fn set_kitty_image_limit(&mut self, bytes: usize) {
        self.kitty_images.set_byte_limit(bytes);
    }

    /// Advance Kitty graphics animations to the monotonic time `now_ms` (supplied
    /// by the app layer so `noa-grid` stays timer-free). Returns whether any
    /// frame changed (repaint) and the next animation deadline in the same clock.
    pub fn advance_kitty_animations(&mut self, now_ms: u64) -> crate::kitty::AnimationTick {
        self.kitty_images.advance_animations(now_ms)
    }

    /// Whether any stored image is currently animating (drives redraw scheduling).
    pub fn has_kitty_animation(&self) -> bool {
        self.kitty_images.has_running_animation()
    }

    /// A cheap `Arc` clone of a flag mirroring [`Self::has_kitty_animation`],
    /// meant to be fetched once per pane (at surface creation) and polled via
    /// `AtomicBool::load` from then on — the app layer's idle-animation timer
    /// uses this to skip locking a terminal that isn't animating, instead of
    /// locking every pane every tick just to ask.
    pub fn kitty_animation_flag(&self) -> Arc<AtomicBool> {
        self.kitty_images.animation_flag()
    }

    pub fn select_all(&mut self) {
        self.active_mut().select_all();
    }

    /// Resize the terminal to a new cell grid (from a window resize). Resizes
    /// every screen, reflows soft-wrapped lines, and updates the recorded size.
    pub fn resize(&mut self, size: GridSize) {
        self.primary.resize(size.cols, size.rows);
        if let Some(alt) = &mut self.alt {
            alt.resize(size.cols, size.rows);
        }
        self.size = size;
    }

    /// Enable or suppress terminal-generated report replies at the drain
    /// boundary. Disabling also discards replies queued before this call.
    ///
    /// Local terminals leave this enabled. Client-mode replica terminals must
    /// disable it so DA/DSR and similar replies are not reflected to the remote
    /// attach channel.
    pub fn set_reply_writes_enabled(&mut self, enabled: bool) {
        self.reply_writes_enabled = enabled;
        if !enabled {
            self.pending_writes.clear();
        }
    }

    /// Take the queued report-reply bytes (for the io thread → pty writer).
    /// The queue is always drained, but suppressed terminals return no bytes.
    pub fn take_pending_writes(&mut self) -> Vec<u8> {
        let pending = std::mem::take(&mut self.pending_writes);
        if self.reply_writes_enabled {
            pending
        } else {
            Vec::new()
        }
    }

    pub fn take_pending_clipboard_writes(&mut self) -> Vec<String> {
        std::mem::take(&mut self.pending_clipboard_writes)
    }

    /// Drain OSC 52 clipboard read requests for the app to fulfill. Each is a
    /// selection target string to echo in the base64 reply.
    pub fn take_pending_clipboard_reads(&mut self) -> Vec<String> {
        std::mem::take(&mut self.pending_clipboard_reads)
    }

    /// Drain queued desktop notifications (OSC 9 / OSC 777) for the app layer,
    /// oldest first. Empty when nothing was requested since the last drain.
    pub fn take_pending_notifications(&mut self) -> Vec<Notification> {
        self.pending_notifications.drain(..).collect()
    }

    /// Build the OSC 52 reply carrying `text` for selection `target`
    /// (e.g. `"c"`), to be written back to the pty by the app layer.
    pub fn osc52_read_reply(target: &str, text: &str) -> Vec<u8> {
        crate::osc::osc52_reply_bytes(target.as_bytes(), text.as_bytes())
    }

    /// Drain the BEL latch: `true` the first call after a `0x07`, `false`
    /// otherwise. Mirrors [`Terminal::take_pending_writes`]'s drain shape.
    pub fn take_pending_bell(&mut self) -> bool {
        std::mem::take(&mut self.pending_bell)
    }

    /// Whether shell integration currently reports a foreground command.
    ///
    /// OSC 133 `C` marks command start and the next `D`/prompt mark clears it.
    /// If no integration marks have arrived, return false rather than treating
    /// an idle login shell as a running program.
    pub fn has_running_program(&self) -> bool {
        self.shell_marks
            .last()
            .is_some_and(|mark| mark.kind == ShellIntegrationMarkKind::CommandStart)
    }

    /// Update the pixel-metric fields backing `XTWINOPS` reports
    /// (`CSI 14/16 t`). The sole caller is `noa-app`'s pane-resize path —
    /// the only place outside this crate that reaches into `Terminal`'s
    /// window-geometry state.
    pub fn set_pixel_metrics(
        &mut self,
        cell_w: u32,
        cell_h: u32,
        text_area_w: u32,
        text_area_h: u32,
    ) {
        self.cell_width_px = cell_w;
        self.cell_height_px = cell_h;
        self.text_area_width_px = text_area_w;
        self.text_area_height_px = text_area_h;
    }

    pub fn set_base_colors(
        &mut self,
        default_fg: noa_core::Rgb,
        default_bg: noa_core::Rgb,
        cursor: noa_core::Rgb,
        palette: [noa_core::Rgb; 256],
    ) {
        self.colors
            .set_base_colors(default_fg, default_bg, cursor, palette);
    }

    fn apply_sgr(&mut self, attrs: &[SgrAttr]) {
        let c = &mut self.active_mut().cursor;
        for a in attrs {
            match *a {
                SgrAttr::Reset => {
                    c.fg = Color::Default;
                    c.bg = Color::Default;
                    c.underline_color = None;
                    c.attrs = CellAttrs::empty();
                }
                SgrAttr::Bold => c.attrs.insert(CellAttrs::BOLD),
                SgrAttr::Faint => c.attrs.insert(CellAttrs::FAINT),
                SgrAttr::Italic => c.attrs.insert(CellAttrs::ITALIC),
                SgrAttr::Underline => {
                    c.attrs.remove(CellAttrs::underline_styles());
                    c.attrs.insert(CellAttrs::UNDERLINE);
                }
                SgrAttr::DoubleUnderline => {
                    c.attrs.remove(CellAttrs::underline_styles());
                    c.attrs.insert(CellAttrs::DOUBLE_UNDERLINE);
                }
                SgrAttr::CurlyUnderline => {
                    c.attrs.remove(CellAttrs::underline_styles());
                    c.attrs.insert(CellAttrs::CURLY_UNDERLINE);
                }
                SgrAttr::DottedUnderline => {
                    c.attrs.remove(CellAttrs::underline_styles());
                    c.attrs.insert(CellAttrs::DOTTED_UNDERLINE);
                }
                SgrAttr::DashedUnderline => {
                    c.attrs.remove(CellAttrs::underline_styles());
                    c.attrs.insert(CellAttrs::DASHED_UNDERLINE);
                }
                SgrAttr::Blink => c.attrs.insert(CellAttrs::BLINK),
                SgrAttr::Inverse => c.attrs.insert(CellAttrs::INVERSE),
                SgrAttr::Invisible => c.attrs.insert(CellAttrs::INVISIBLE),
                SgrAttr::Strike => c.attrs.insert(CellAttrs::STRIKETHROUGH),
                SgrAttr::Overline => c.attrs.insert(CellAttrs::OVERLINE),
                SgrAttr::ResetBold => c.attrs.remove(CellAttrs::BOLD | CellAttrs::FAINT),
                SgrAttr::ResetItalic => c.attrs.remove(CellAttrs::ITALIC),
                SgrAttr::ResetUnderline => c.attrs.remove(CellAttrs::underline_styles()),
                SgrAttr::ResetBlink => c.attrs.remove(CellAttrs::BLINK),
                SgrAttr::ResetInverse => c.attrs.remove(CellAttrs::INVERSE),
                SgrAttr::ResetInvisible => c.attrs.remove(CellAttrs::INVISIBLE),
                SgrAttr::ResetStrike => c.attrs.remove(CellAttrs::STRIKETHROUGH),
                SgrAttr::ResetOverline => c.attrs.remove(CellAttrs::OVERLINE),
                SgrAttr::Fg(col) => c.fg = col,
                SgrAttr::Bg(col) => c.bg = col,
                SgrAttr::UnderlineColor(col) => c.underline_color = Some(col),
                SgrAttr::DefaultFg => c.fg = Color::Default,
                SgrAttr::DefaultBg => c.bg = Color::Default,
                SgrAttr::DefaultUnderlineColor => c.underline_color = None,
            }
        }
    }

    fn active_mut(&mut self) -> &mut Screen {
        if self.active_is_alt {
            let cols = self.size.cols;
            let rows = self.size.rows;
            self.alt
                .get_or_insert_with(|| Screen::alternate(cols, rows))
        } else {
            &mut self.primary
        }
    }

    fn set_current_hyperlink(&mut self, hyperlink: Hyperlink) {
        let id = match self.hyperlink_index.get(&hyperlink) {
            Some(&id) => id,
            None if self.hyperlinks.len() >= HYPERLINK_REGISTRY_CAP => {
                self.active_mut().cursor.hyperlink = None;
                return;
            }
            None => {
                let id = self.hyperlinks.len();
                self.hyperlinks.push(hyperlink.clone());
                self.hyperlink_index.insert(hyperlink, id);
                id
            }
        };
        self.active_mut().cursor.hyperlink = HyperlinkId::new(id);
    }

    fn clear_current_hyperlink(&mut self) {
        self.active_mut().cursor.hyperlink = None;
    }

    fn record_shell_mark(&mut self, kind: ShellIntegrationMarkKind, exit_status: Option<i32>) {
        let screen = self.active();
        // Session-absolute row: stays valid across scrollback trimming, so a
        // recorded mark keeps pointing at the same line as history scrolls off
        // (see `Screen::rows_evicted`).
        let rows_evicted = screen.rows_evicted();
        let point = SelectionPoint::new(
            screen.cursor.x,
            rows_evicted + screen.scrollback_len() + screen.cursor.y as usize,
        );
        if self.shell_marks.len() >= SHELL_MARK_CAP {
            self.shell_marks.retain(|mark| mark.point.y >= rows_evicted);
            if self.shell_marks.len() >= SHELL_MARK_CAP {
                self.shell_marks.remove(0);
            }
        }
        self.shell_marks.push(ShellIntegrationMark {
            kind,
            point,
            exit_status,
        });
    }

    fn push_notification(&mut self, notification: Notification) {
        if self.pending_notifications.len() >= NOTIFICATION_QUEUE_CAP {
            self.pending_notifications.pop_front();
        }
        self.pending_notifications.push_back(notification);
    }

    /// Scroll the viewport to the nearest shell-integration prompt mark
    /// (`OSC 133;A`) in `direction`, relative to the current viewport top.
    /// Returns `true` if a target prompt was found and the viewport moved.
    ///
    /// Marks are compared in session-absolute coordinates so trimmed history
    /// (evicted rows) is skipped rather than jumped into.
    pub fn scroll_to_prompt(&mut self, direction: PromptJump) -> bool {
        let screen = self.active();
        let rows_evicted = screen.rows_evicted();
        let abs_top = rows_evicted + screen.visible_row_base();

        let target = self
            .shell_marks
            .iter()
            .filter(|mark| mark.kind == ShellIntegrationMarkKind::PromptStart)
            .map(|mark| mark.point.y)
            .filter(|&abs| abs >= rows_evicted)
            .fold(None, |best: Option<usize>, abs| match direction {
                PromptJump::Prev if abs < abs_top => Some(best.map_or(abs, |b| b.max(abs))),
                PromptJump::Next if abs > abs_top => Some(best.map_or(abs, |b| b.min(abs))),
                _ => best,
            });

        let Some(target_abs) = target else {
            return false;
        };
        self.active_mut()
            .scroll_viewport_to_history_index(target_abs - rows_evicted);
        true
    }

    /// `CSI 22 t` — push the current window title onto the title stack,
    /// evicting the oldest entry once [`TITLE_STACK_CAP`] is reached.
    fn push_title(&mut self) {
        if self.title_stack.len() >= TITLE_STACK_CAP {
            self.title_stack.pop_front();
        }
        self.title_stack.push_back(self.title.clone());
    }

    /// `CSI 23 t` — pop the most recently pushed title back into effect.
    /// No-op on an empty stack.
    fn pop_title(&mut self) {
        if let Some(title) = self.title_stack.pop_back() {
            self.title = title;
        }
    }

    fn enter_alt_screen(&mut self, clear: bool) {
        if clear || self.alt.is_none() {
            let mut alt = Screen::alternate(self.size.cols, self.size.rows);
            alt.cursor.visible = self.modes.cursor_visible();
            self.alt = Some(alt);
            self.screen_generation = self.screen_generation.wrapping_add(1);
        } else if let Some(alt) = &mut self.alt {
            alt.cursor.visible = self.modes.cursor_visible();
        }
        self.active_is_alt = true;
        self.primary.clear_selection();
        self.primary.clear_search();
        if let Some(alt) = &mut self.alt {
            alt.clear_selection();
            alt.clear_search();
        }
    }

    fn leave_alt_screen(&mut self, restore_cursor: bool, clear_alt: bool) {
        let was_alt = self.active_is_alt;
        self.active_is_alt = false;
        self.primary.scroll_viewport_to_bottom();
        self.primary.cursor.visible = self.modes.cursor_visible();
        if restore_cursor {
            self.primary.restore_cursor();
        }
        if clear_alt && was_alt {
            let mut alt = Screen::alternate(self.size.cols, self.size.rows);
            alt.cursor.visible = self.modes.cursor_visible();
            self.alt = Some(alt);
        }
        if was_alt {
            self.primary.clear_selection();
            self.primary.clear_search();
            if let Some(alt) = &mut self.alt {
                alt.clear_selection();
                alt.clear_search();
            }
        }
    }
}
