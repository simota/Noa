//! [`Terminal`] — the top-level state model. Implements [`noa_vt::Handler`],
//! dispatching parsed operations onto the active [`Screen`] and queuing report
//! replies (DA/DSR) for the pty writer.

use std::collections::{HashMap, VecDeque};

use crate::cell::Hyperlink;
use crate::charset::CharsetState;
use crate::cursor::{Cursor, CursorStyle, ScrollRegion};
use crate::kitty::{ImageStore, KittyError, KittyImage, TransmitStep};
use crate::kitty_keyboard::{KittyKeyboard, SetMode};
use crate::kitty_placeholder::scan_row;
use crate::modes::ModeState;
use crate::osc::{
    CwdOsc, HyperlinkOsc, Notification, Osc52Policy, ShellIntegrationOsc, ShellIntegrationOscKind,
    TerminalColors, handle_clipboard_osc, handle_color_osc, parse_cwd_osc, parse_hyperlink_osc,
    parse_notification_osc, parse_shell_integration_osc,
};
use crate::screen::{KittyPlacement, Screen, VisibleKittyPlacement};
use crate::search::SearchMatch;
use crate::selection::SelectionPoint;
use noa_core::{CellAttrs, Color, GridSize, Point};
use noa_vt::{
    Charset, CharsetSlot, CursorStyle as VtCursorStyle, DaKind, DsrKind, EraseDisplay, EraseLine,
    Handler, KittyAction, KittyDelete, KittyGraphicsCommand, ModeRequest, SgrAttr,
};

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

/// Cap on recorded OSC 133 shell marks. Marks whose rows scrolled out of
/// trimmed history are useless (`scroll_to_prompt` skips them), so those are
/// pruned first; if every mark is still reachable, the oldest is evicted.
pub(crate) const SHELL_MARK_CAP: usize = 4096;

pub struct Terminal {
    pub primary: Screen,
    /// Alternate screen — populated in inc≥2.
    pub alt: Option<Screen>,
    pub active_is_alt: bool,
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
    pub size: GridSize,
    /// Bytes the terminal must write back to the pty (query replies).
    pub pending_writes: Vec<u8>,
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
            modes: ModeState::defaults(),
            charset: CharsetState::default(),
            title: String::new(),
            cwd: None,
            hyperlinks: Vec::new(),
            hyperlink_index: HashMap::new(),
            shell_marks: Vec::new(),
            colors: TerminalColors::default(),
            osc52_policy: Osc52Policy::default(),
            size,
            pending_writes: Vec::new(),
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

    pub fn select_line_at_viewport_point(&mut self, point: Point) {
        self.active_mut().select_line_at_viewport_point(point);
    }

    pub fn clear_selection(&mut self) {
        self.active_mut().clear_selection();
    }

    pub fn selected_text(&self) -> Option<String> {
        self.active().selected_text()
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

    pub fn clear_active_display_and_scrollback(&mut self) {
        if self.active_is_alt {
            let active = self.active_mut();
            active.clear_display();
            active.clear_selection();
            active.clear_search();
        } else {
            self.primary.clear_display();
            self.primary.clear_scrollback();
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

    /// Take the queued report-reply bytes (for the io thread → pty writer).
    pub fn take_pending_writes(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.pending_writes)
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
        self.active_mut().cursor.hyperlink = Some(id);
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

    fn push_dcs_response(&mut self, body: &[u8]) {
        self.pending_writes.extend_from_slice(b"\x1bP");
        self.pending_writes.extend_from_slice(body);
        self.pending_writes.extend_from_slice(b"\x1b\\");
    }

    /// Emit a Kitty graphics reply (`ESC _ G i=<id>[,I=..][,p=..];<body> ESC \`).
    fn push_apc_response(&mut self, id: u32, number: u32, placement: u32, body: &[u8]) {
        self.pending_writes.extend_from_slice(b"\x1b_G");
        self.pending_writes.extend_from_slice(b"i=");
        self.pending_writes
            .extend_from_slice(id.to_string().as_bytes());
        if number != 0 {
            self.pending_writes.extend_from_slice(b",I=");
            self.pending_writes
                .extend_from_slice(number.to_string().as_bytes());
        }
        if placement != 0 {
            self.pending_writes.extend_from_slice(b",p=");
            self.pending_writes
                .extend_from_slice(placement.to_string().as_bytes());
        }
        self.pending_writes.push(b';');
        self.pending_writes.extend_from_slice(body);
        self.pending_writes.extend_from_slice(b"\x1b\\");
    }

    /// Decide whether to emit a Kitty graphics reply, honoring the quiet level
    /// (`q=`) and the "no reply when neither `i=` nor `I=` was given" rule, then
    /// emit it. `assigned_id` is the id the store actually used (may differ from
    /// `req_id` when auto-assigned).
    fn kitty_reply(
        &mut self,
        req_id: u32,
        req_number: u32,
        assigned_id: u32,
        placement: u32,
        quiet: u8,
        result: Result<(), KittyError>,
    ) {
        if req_id == 0 && req_number == 0 {
            return;
        }
        let ok = result.is_ok();
        if quiet >= 2 || (quiet >= 1 && ok) {
            return;
        }
        let body: &[u8] = match &result {
            Ok(()) => b"OK",
            Err(e) => e.reply_body().as_bytes(),
        };
        let id = if assigned_id != 0 {
            assigned_id
        } else {
            req_id
        };
        self.push_apc_response(id, req_number, placement, body);
    }

    /// Feed a data-carrying Kitty graphics command (`a=t`/`a=T`/`a=q`) into the
    /// image store, then—for `a=T`—place it, replying on completion.
    fn kitty_transmit(&mut self, cmd: KittyGraphicsCommand) {
        match self.kitty_images.transmit(&cmd) {
            TransmitStep::NeedMore => {}
            TransmitStep::Done(done) => {
                let ctrl = done.ctrl;
                let result = done.result.and_then(|id| {
                    if ctrl.action == KittyAction::TransmitAndDisplay {
                        self.kitty_place(&ctrl, id).map(|()| id)
                    } else {
                        Ok(id)
                    }
                });
                let assigned = *result.as_ref().unwrap_or(&ctrl.image_id);
                if result.is_ok() {
                    self.kitty_images
                        .enforce_quota(&self.referenced_image_ids());
                }
                self.kitty_reply(
                    ctrl.image_id,
                    ctrl.image_number,
                    assigned,
                    ctrl.placement_id,
                    ctrl.quiet,
                    result.map(|_| ()),
                );
            }
        }
    }

    /// Display a stored image (`a=p`), placing it on the active screen.
    fn kitty_put(&mut self, cmd: &KittyGraphicsCommand) {
        let image_id = self.resolve_put_image(cmd);
        let result = match image_id {
            Some(id) => self.kitty_place(cmd, id),
            None => Err(KittyError::NoEnt),
        };
        let assigned = image_id.unwrap_or(cmd.image_id);
        self.kitty_reply(
            cmd.image_id,
            cmd.image_number,
            assigned,
            cmd.placement_id,
            cmd.quiet,
            result,
        );
    }

    /// Resolve the image an `a=p` command targets: `i=` id, else `I=` number.
    fn resolve_put_image(&self, cmd: &KittyGraphicsCommand) -> Option<u32> {
        if cmd.image_id != 0 {
            return self.kitty_images.get(cmd.image_id).map(|_| cmd.image_id);
        }
        if cmd.image_number != 0 {
            return self
                .kitty_images
                .get_by_number(cmd.image_number)
                .map(|img| img.id);
        }
        None
    }

    /// Create a placement of image `image_id` on the active screen from `ctrl`,
    /// resolving the effective cell span and moving the cursor unless `C=1`.
    fn kitty_place(
        &mut self,
        ctrl: &KittyGraphicsCommand,
        image_id: u32,
    ) -> Result<(), KittyError> {
        let (img_w, img_h) = match self.kitty_images.get(image_id) {
            Some(img) => (img.width, img.height),
            None => return Err(KittyError::NoEnt),
        };
        let (cell_w, cell_h) = (self.cell_width_px, self.cell_height_px);
        if cell_w == 0 || cell_h == 0 {
            // Cell metrics arrive with the first resize; without them the cell
            // span is undefined.
            return Err(KittyError::Invalid);
        }

        let src_w = if ctrl.src_w != 0 {
            ctrl.src_w
        } else {
            img_w.saturating_sub(ctrl.src_x)
        };
        let src_h = if ctrl.src_h != 0 {
            ctrl.src_h
        } else {
            img_h.saturating_sub(ctrl.src_y)
        };
        if src_w == 0 || src_h == 0 {
            return Err(KittyError::Invalid);
        }
        let cols = if ctrl.columns != 0 {
            ctrl.columns
        } else {
            src_w.div_ceil(cell_w)
        };
        let rows = if ctrl.rows != 0 {
            ctrl.rows
        } else {
            src_h.div_ceil(cell_h)
        };
        let cols = cols.clamp(1, u16::MAX as u32) as u16;
        let rows = rows.clamp(1, u16::MAX as u32) as u16;
        let cropped = ctrl.src_x != 0 || ctrl.src_y != 0 || ctrl.src_w != 0 || ctrl.src_h != 0;
        let src = cropped.then_some([ctrl.src_x, ctrl.src_y, src_w, src_h]);
        let cell_x_off = ctrl.cell_x_off.min(cell_w - 1) as u16;
        let cell_y_off = ctrl.cell_y_off.min(cell_h - 1) as u16;

        let screen = self.active_mut();
        let anchor_abs_row =
            screen.rows_evicted() + screen.scrollback_len() + screen.cursor.y as usize;
        let anchor_col = screen.cursor.x;
        screen.insert_kitty_placement(KittyPlacement {
            image_id,
            placement_id: ctrl.placement_id,
            anchor_abs_row,
            anchor_col,
            cell_x_off,
            cell_y_off,
            src,
            cols,
            rows,
            z: ctrl.z_index,
            is_virtual: ctrl.virtual_placement,
        });

        if !ctrl.cursor_no_move && !ctrl.virtual_placement {
            // Move to the image's last row, one column past its right edge.
            for _ in 1..rows {
                screen.index();
            }
            let max_x = screen.cols.saturating_sub(1);
            screen.cursor.x = (anchor_col as usize + cols as usize).min(max_x as usize) as u16;
            screen.cursor.pending_wrap = false;
        }
        Ok(())
    }

    /// Delete placements (and, for uppercase specifiers, image data) per `a=d`.
    fn kitty_delete(&mut self, cmd: &KittyGraphicsCommand) {
        let Some(spec) = cmd.delete else {
            return;
        };
        if let KittyDelete::AnimationFrames { .. } = spec {
            self.kitty_reply(
                cmd.image_id,
                cmd.image_number,
                cmd.image_id,
                cmd.placement_id,
                cmd.quiet,
                Err(KittyError::Unsupported),
            );
            return;
        }
        let free = kitty_delete_frees(spec);
        let number_ids: Vec<u32> = match spec {
            KittyDelete::ByNumber { .. } => self.kitty_images.ids_with_number(cmd.image_number),
            _ => Vec::new(),
        };
        let (cursor_abs, cursor_col) = {
            let s = self.active();
            (
                s.rows_evicted() + s.scrollback_len() + s.cursor.y as usize,
                s.cursor.x,
            )
        };
        let live_top = {
            let s = self.active();
            s.rows_evicted() + s.scrollback_len()
        };
        // Cell coords in `a=d` are 1-based grid columns/rows; convert to
        // session-absolute for intersection tests.
        let target_col = cmd.src_x.saturating_sub(1) as u16;
        let target_abs = live_top + cmd.src_y.saturating_sub(1) as usize;

        let removed = self.active_mut().delete_kitty_placements(|p| match spec {
            KittyDelete::All { .. } => true,
            KittyDelete::ById { .. } => {
                p.image_id == cmd.image_id
                    && (cmd.placement_id == 0 || p.placement_id == cmd.placement_id)
            }
            KittyDelete::ByNumber { .. } => number_ids.contains(&p.image_id),
            KittyDelete::AtCursor { .. } => p.covers_abs(cursor_abs, cursor_col),
            KittyDelete::AtCell { .. } => p.covers_abs(target_abs, target_col),
            KittyDelete::AtCellZ { .. } => {
                p.covers_abs(target_abs, target_col) && p.z == cmd.z_index
            }
            KittyDelete::ByIdRange { .. } => p.image_id >= cmd.src_x && p.image_id <= cmd.src_y,
            KittyDelete::ByColumn { .. } => {
                target_col >= p.anchor_col && target_col < p.anchor_col.saturating_add(p.cols)
            }
            KittyDelete::ByRow { .. } => {
                target_abs >= p.anchor_abs_row && target_abs < p.anchor_abs_row + p.rows as usize
            }
            KittyDelete::ByZ { .. } => p.z == cmd.z_index,
            KittyDelete::AnimationFrames { .. } => false,
        });

        if free {
            for id in removed {
                if !self.image_referenced(id) {
                    self.kitty_images.remove(id);
                }
            }
        }
    }

    /// Whether any placement on either screen still references image `id`.
    fn image_referenced(&self, id: u32) -> bool {
        self.primary
            .kitty_placements
            .iter()
            .chain(self.alt.iter().flat_map(|s| s.kitty_placements.iter()))
            .any(|p| p.image_id == id)
    }

    /// Image ids kept alive by a placement on either screen (spared by the quota
    /// sweep).
    fn referenced_image_ids(&self) -> std::collections::HashSet<u32> {
        self.primary
            .kitty_placements
            .iter()
            .chain(self.alt.iter().flat_map(|s| s.kitty_placements.iter()))
            .map(|p| p.image_id)
            .collect()
    }

    /// Placements on the active screen projected into the current viewport,
    /// sorted by z ascending. The renderer pairs each with [`Terminal::kitty_image`].
    pub fn kitty_visible_placements(&self) -> Vec<VisibleKittyPlacement> {
        self.active().visible_kitty_placements()
    }

    /// Placements synthesized from Unicode placeholder cells (`U+10EEEE`) that
    /// reference a virtual placement (`U=1`) on the active screen, projected into
    /// the current viewport. Each returned placement covers one fused run of
    /// placeholder cells and carries the image source sub-rectangle for that run.
    ///
    /// Returns empty unless the active screen holds a virtual placement, so the
    /// common no-image path pays only one `any` scan. Each run is matched to a
    /// virtual placement by image id (and placement id when the placeholder
    /// encodes one); the virtual placement supplies the image's cell grid
    /// (`cols`×`rows`), any crop, and the z-index, from which the run's source
    /// rectangle is carved.
    pub fn kitty_placeholder_placements(&self) -> Vec<VisibleKittyPlacement> {
        let screen = self.active();
        if !screen.kitty_placements.iter().any(|p| p.is_virtual) {
            return Vec::new();
        }
        let mut out = Vec::new();
        for y in 0..screen.rows {
            let Some(row) = screen.visible_row(y) else {
                continue;
            };
            for run in scan_row(&row.cells) {
                let Some(vp) = screen.kitty_placements.iter().find(|p| {
                    p.is_virtual
                        && p.image_id == run.image_id
                        && (run.placement_id == 0 || p.placement_id == run.placement_id)
                }) else {
                    continue;
                };
                let Some(img) = self.kitty_images.get(run.image_id) else {
                    continue;
                };
                // The virtual placement spreads its (optionally cropped) image
                // across a `cols`×`rows` cell grid; this run covers image row
                // `virt_row`, columns `[virt_col_start, +cols)` of that grid.
                let base = vp.src.unwrap_or([0, 0, img.width, img.height]);
                let cell_w = f64::from(base[2]) / f64::from(vp.cols.max(1));
                let cell_h = f64::from(base[3]) / f64::from(vp.rows.max(1));
                let sx = f64::from(base[0]) + f64::from(run.virt_col_start) * cell_w;
                let sy = f64::from(base[1]) + f64::from(run.virt_row) * cell_h;
                let sw = f64::from(run.cols) * cell_w;
                let src = [
                    sx.round() as u32,
                    sy.round() as u32,
                    (sw.round() as u32).max(1),
                    (cell_h.round() as u32).max(1),
                ];
                out.push(VisibleKittyPlacement {
                    image_id: run.image_id,
                    placement_id: run.placement_id,
                    grid_x: i32::from(run.screen_x),
                    grid_y: i32::from(y),
                    cell_x_off: 0,
                    cell_y_off: 0,
                    cols: run.cols,
                    rows: 1,
                    src: Some(src),
                    z: vp.z,
                });
            }
        }
        out
    }

    /// A stored image by id (for the renderer to upload/sample).
    pub fn kitty_image(&self, id: u32) -> Option<&KittyImage> {
        self.kitty_images.get(id)
    }

    fn push_decrqss_response(&mut self, valid: bool, request: &[u8], setting: &[u8]) {
        self.pending_writes.extend_from_slice(b"\x1bP");
        if valid {
            self.pending_writes.extend_from_slice(b"1$r");
            self.pending_writes.extend_from_slice(setting);
        } else {
            self.pending_writes.extend_from_slice(b"0$r");
            self.pending_writes.extend_from_slice(request);
        }
        self.pending_writes.extend_from_slice(b"\x1b\\");
    }

    fn handle_decrqss(&mut self, request: &[u8]) {
        match request {
            b"m" => {
                let setting = self.current_sgr_report();
                self.push_decrqss_response(true, request, setting.as_bytes());
            }
            b" q" => {
                let setting = format!("{} q", self.cursor_style_number());
                self.push_decrqss_response(true, request, setting.as_bytes());
            }
            b"r" => {
                let region = self.active().region;
                let setting = format!("{};{}r", region.top + 1, region.bottom + 1);
                self.push_decrqss_response(true, request, setting.as_bytes());
            }
            b"s" => {
                let (left, right) = self
                    .active()
                    .horizontal_margins
                    .map(|m| (m.left + 1, m.right + 1))
                    .unwrap_or((1, self.size.cols));
                let setting = format!("{left};{right}s");
                self.push_decrqss_response(true, request, setting.as_bytes());
            }
            _ => self.push_decrqss_response(false, request, &[]),
        }
    }

    fn cursor_style_number(&self) -> u8 {
        match self.active().cursor.style {
            CursorStyle::BlinkingBlock => 1,
            CursorStyle::SteadyBlock => 2,
            CursorStyle::BlinkingUnderline => 3,
            CursorStyle::SteadyUnderline => 4,
            CursorStyle::BlinkingBar => 5,
            CursorStyle::SteadyBar => 6,
        }
    }

    fn current_sgr_report(&self) -> String {
        let c = &self.active().cursor;
        let mut params = vec!["0".to_string()];
        if c.attrs.contains(CellAttrs::BOLD) {
            params.push("1".to_string());
        }
        if c.attrs.contains(CellAttrs::FAINT) {
            params.push("2".to_string());
        }
        if c.attrs.contains(CellAttrs::ITALIC) {
            params.push("3".to_string());
        }
        if c.attrs.contains(CellAttrs::UNDERLINE) {
            params.push("4".to_string());
        } else if c.attrs.contains(CellAttrs::DOUBLE_UNDERLINE) {
            params.push("21".to_string());
        } else if c.attrs.contains(CellAttrs::CURLY_UNDERLINE) {
            params.push("4:3".to_string());
        } else if c.attrs.contains(CellAttrs::DOTTED_UNDERLINE) {
            params.push("4:4".to_string());
        } else if c.attrs.contains(CellAttrs::DASHED_UNDERLINE) {
            params.push("4:5".to_string());
        }
        if c.attrs.contains(CellAttrs::BLINK) {
            params.push("5".to_string());
        }
        if c.attrs.contains(CellAttrs::INVERSE) {
            params.push("7".to_string());
        }
        if c.attrs.contains(CellAttrs::INVISIBLE) {
            params.push("8".to_string());
        }
        if c.attrs.contains(CellAttrs::STRIKETHROUGH) {
            params.push("9".to_string());
        }
        if c.attrs.contains(CellAttrs::OVERLINE) {
            params.push("53".to_string());
        }
        push_color_params(&mut params, 30, 90, 38, c.fg);
        push_color_params(&mut params, 40, 100, 48, c.bg);
        if let Some(color) = c.underline_color {
            push_color_params(&mut params, 0, 0, 58, color);
        }
        format!("{}m", params.join(";"))
    }

    fn handle_xtgettcap(&mut self, payload: &[u8]) {
        for encoded_name in payload
            .split(|&b| b == b';')
            .filter(|name| !name.is_empty())
        {
            let Some(name) = decode_xtgettcap_name(encoded_name) else {
                self.push_xtgettcap_response(false, encoded_name, &[]);
                continue;
            };
            let value = match name.as_slice() {
                b"TN" => Some(b"noa".as_slice()),
                b"RGB" => Some(b"8:8:8".as_slice()),
                b"Co" => Some(b"256".as_slice()),
                _ => None,
            };
            if let Some(value) = value {
                self.push_xtgettcap_response(true, encoded_name, value);
            } else {
                self.push_xtgettcap_response(false, encoded_name, &[]);
            }
        }
    }

    fn push_xtgettcap_response(&mut self, valid: bool, encoded_name: &[u8], value: &[u8]) {
        self.pending_writes.extend_from_slice(b"\x1bP");
        self.pending_writes
            .extend_from_slice(if valid { b"1+r" } else { b"0+r" });
        self.pending_writes.extend_from_slice(encoded_name);
        if valid {
            self.pending_writes.push(b'=');
            push_hex_bytes(&mut self.pending_writes, value);
        }
        self.pending_writes.extend_from_slice(b"\x1b\\");
    }

    fn enter_alt_screen(&mut self, clear: bool) {
        if clear || self.alt.is_none() {
            let mut alt = Screen::alternate(self.size.cols, self.size.rows);
            alt.cursor.visible = self.modes.cursor_visible();
            self.alt = Some(alt);
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

fn push_color_params(
    params: &mut Vec<String>,
    base: u16,
    bright_base: u16,
    extended: u16,
    color: Color,
) {
    match color {
        Color::Default => {}
        Color::Palette(index) if index < 8 && base != 0 => {
            params.push((base + index as u16).to_string());
        }
        Color::Palette(index) if index < 16 && bright_base != 0 => {
            params.push((bright_base + index as u16 - 8).to_string());
        }
        Color::Palette(index) => params.push(format!("{extended};5;{index}")),
        Color::Rgb(rgb) => params.push(format!("{extended};2;{};{};{}", rgb.r, rgb.g, rgb.b)),
    }
}

fn decode_xtgettcap_name(encoded: &[u8]) -> Option<Vec<u8>> {
    if !encoded.len().is_multiple_of(2) {
        return None;
    }
    let mut out = Vec::with_capacity(encoded.len() / 2);
    for pair in encoded.chunks_exact(2) {
        let hi = hex_value(pair[0])?;
        let lo = hex_value(pair[1])?;
        out.push((hi << 4) | lo);
    }
    Some(out)
}

fn hex_value(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

fn push_hex_bytes(out: &mut Vec<u8>, bytes: &[u8]) {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    for &b in bytes {
        out.push(HEX[(b >> 4) as usize]);
        out.push(HEX[(b & 0x0f) as usize]);
    }
}

/// Whether an `a=d` specifier is the uppercase form that also frees image data.
fn kitty_delete_frees(spec: KittyDelete) -> bool {
    match spec {
        KittyDelete::All { free }
        | KittyDelete::ById { free }
        | KittyDelete::ByNumber { free }
        | KittyDelete::AtCursor { free }
        | KittyDelete::AnimationFrames { free }
        | KittyDelete::AtCell { free }
        | KittyDelete::AtCellZ { free }
        | KittyDelete::ByIdRange { free }
        | KittyDelete::ByColumn { free }
        | KittyDelete::ByRow { free }
        | KittyDelete::ByZ { free } => free,
    }
}

impl Handler for Terminal {
    fn print(&mut self, c: char) {
        let autowrap = self.modes.autowrap();
        let grapheme_clustering = self.modes.grapheme_clustering();
        let c = self.charset.translate(c);
        self.active_mut().print(c, autowrap, grapheme_clustering);
    }

    fn execute_c0(&mut self, byte: u8) {
        match byte {
            0x0e => return self.locking_shift(CharsetSlot::G1), // SO
            0x0f => return self.locking_shift(CharsetSlot::G0), // SI
            0x07 => return self.bell(),                         // BEL — no grid-state side effect.
            _ => {}
        }
        let linefeed_newline = self.modes.linefeed_newline();
        let screen = self.active_mut();
        match byte {
            0x08 => screen.backspace(),
            0x09 => screen.tab(1),
            0x0a..=0x0c => {
                if linefeed_newline {
                    screen.carriage_return();
                }
                screen.index();
            }
            0x0d => screen.carriage_return(),
            _ => {}
        }
    }

    fn cursor_up(&mut self, n: u16) {
        self.active_mut().cursor_up(n);
    }
    fn cursor_down(&mut self, n: u16) {
        self.active_mut().cursor_down(n);
    }
    fn cursor_forward(&mut self, n: u16) {
        self.active_mut().cursor_forward(n);
    }
    fn cursor_backward(&mut self, n: u16) {
        self.active_mut().cursor_backward(n);
    }
    fn cursor_position(&mut self, row: u16, col: u16) {
        self.active_mut().cursor_position(row, col);
    }
    fn cursor_col_abs(&mut self, col: u16) {
        self.active_mut().cursor_col_abs(col);
    }
    fn cursor_row_abs(&mut self, row: u16) {
        self.active_mut().cursor_row_abs(row);
    }

    fn erase_display(&mut self, mode: EraseDisplay) {
        self.active_mut().erase_display(mode);
    }
    fn erase_line(&mut self, mode: EraseLine) {
        self.active_mut().erase_line(mode);
    }

    fn screen_alignment_test(&mut self) {
        self.active_mut().screen_alignment_test();
    }

    fn set_attributes(&mut self, attrs: &[SgrAttr]) {
        self.apply_sgr(attrs);
    }

    fn set_cursor_style(&mut self, style: VtCursorStyle) {
        let default_style = self.default_cursor_style;
        self.active_mut().cursor.style = match style {
            VtCursorStyle::Default => default_style,
            VtCursorStyle::BlinkingBlock => CursorStyle::BlinkingBlock,
            VtCursorStyle::SteadyBlock => CursorStyle::SteadyBlock,
            VtCursorStyle::BlinkingUnderline => CursorStyle::BlinkingUnderline,
            VtCursorStyle::SteadyUnderline => CursorStyle::SteadyUnderline,
            VtCursorStyle::BlinkingBar => CursorStyle::BlinkingBar,
            VtCursorStyle::SteadyBar => CursorStyle::SteadyBar,
        };
    }

    fn set_horizontal_margins(&mut self, left: u16, right: u16) {
        if self.modes.left_right_margin() {
            self.active_mut().set_horizontal_margins(left, right);
        }
    }

    fn set_application_keypad(&mut self, on: bool) {
        self.modes.set(66, false, on);
    }

    fn request_mode(&mut self, request: ModeRequest) {
        let state = match (request.value, request.ansi) {
            (20, true)
            | (1, false)
            | (6, false)
            | (7, false)
            | (9, false)
            | (25, false)
            | (47, false)
            | (66, false)
            | (69, false)
            | (1004, false)
            | (1000, false)
            | (1002, false)
            | (1003, false)
            | (1005, false)
            | (1006, false)
            | (1015, false)
            | (1047, false)
            | (1048, false)
            | (1049, false)
            | (2026, false)
            | (2027, false)
            | (2004, false) => {
                if self.modes.get(request.value, request.ansi) {
                    1
                } else {
                    2
                }
            }
            _ => 0,
        };
        if request.ansi {
            self.pending_writes
                .extend_from_slice(format!("\x1b[{};{}$y", request.value, state).as_bytes());
        } else {
            self.pending_writes
                .extend_from_slice(format!("\x1b[?{};{}$y", request.value, state).as_bytes());
        }
    }

    fn bell(&mut self) {
        self.pending_bell = true;
    }

    fn designate_charset(&mut self, slot: CharsetSlot, set: Charset) {
        self.charset.designate(slot, set);
    }

    fn locking_shift(&mut self, slot: CharsetSlot) {
        self.charset.shift(slot);
    }

    fn set_mode(&mut self, value: u16, ansi: bool, on: bool) {
        self.modes.set(value, ansi, on);
        if !ansi {
            match value {
                25 => self.active_mut().cursor.visible = on, // DECTCEM
                69 => {
                    if on {
                        self.active_mut().enable_horizontal_margins();
                    } else {
                        self.active_mut().disable_horizontal_margins();
                    }
                }
                47 => {
                    if on {
                        self.enter_alt_screen(false);
                    } else {
                        self.leave_alt_screen(false, false);
                    }
                }
                1047 => {
                    if on {
                        self.enter_alt_screen(false);
                    } else {
                        self.leave_alt_screen(false, true);
                    }
                }
                1048 => {
                    if on {
                        self.active_mut().save_cursor();
                    } else {
                        self.active_mut().restore_cursor();
                    }
                }
                1049 => {
                    if on {
                        self.primary.save_cursor();
                        self.enter_alt_screen(true);
                    } else {
                        self.leave_alt_screen(true, true);
                    }
                }
                _ => {}
            }
        }
    }

    fn carriage_return(&mut self) {
        self.active_mut().carriage_return();
    }
    fn linefeed(&mut self) {
        self.active_mut().index();
    }
    fn tab(&mut self, n: u16) {
        self.active_mut().tab(n);
    }
    fn tab_back(&mut self, n: u16) {
        self.active_mut().tab_back(n);
    }
    fn reverse_index(&mut self) {
        self.active_mut().reverse_index();
    }
    fn save_cursor(&mut self) {
        self.active_mut().save_cursor();
    }
    fn restore_cursor(&mut self) {
        self.active_mut().restore_cursor();
    }
    fn set_tab_stop(&mut self) {
        self.active_mut().set_tab_stop();
    }
    fn clear_tab_stop(&mut self) {
        self.active_mut().clear_tab_stop();
    }
    fn clear_all_tab_stops(&mut self) {
        self.active_mut().clear_all_tab_stops();
    }

    fn full_reset(&mut self) {
        self.primary = Screen::new(self.size.cols, self.size.rows);
        self.alt = None;
        self.active_is_alt = false;
        self.modes = ModeState::defaults();
        self.charset = CharsetState::default();
        self.title.clear();
        self.cwd = None;
        self.hyperlinks.clear();
        self.hyperlink_index.clear();
        self.shell_marks.clear();
        self.colors.reset_dynamic_overrides();
        self.pending_clipboard_writes.clear();
        self.pending_clipboard_reads.clear();
        self.pending_notifications.clear();
        self.pending_bell = false;
        self.kitty_keyboard.reset();
        self.kitty_images.clear();
        self.clear_selection();
        self.clear_search();
    }

    fn soft_reset(&mut self) {
        // DECTCEM on, DECOM off — tracked bits only; screen content untouched.
        self.modes.set(25, false, true);
        self.modes.set(6, false, false);
        self.charset = CharsetState::default();
        let last_row = self.size.rows.saturating_sub(1);
        let screen = self.active_mut();
        screen.cursor.visible = true;
        screen.region = ScrollRegion {
            top: 0,
            bottom: last_row,
        };
        // Clears the margin value only; the DECLRMM capability bit (mode 69)
        // in `self.modes` is untouched.
        screen.disable_horizontal_margins();
        screen.cursor.fg = Color::Default;
        screen.cursor.bg = Color::Default;
        screen.cursor.underline_color = None;
        screen.cursor.attrs = CellAttrs::empty();
        // Next DECRC restores to the default position/attributes, not
        // whatever was saved before the reset.
        screen.saved_cursor = Some(Cursor::default().into());
    }

    fn insert_blank_chars(&mut self, n: u16) {
        self.active_mut().insert_blank_chars(n);
    }
    fn insert_lines(&mut self, n: u16) {
        self.active_mut().insert_lines(n);
    }
    fn delete_lines(&mut self, n: u16) {
        self.active_mut().delete_lines(n);
    }
    fn delete_chars(&mut self, n: u16) {
        self.active_mut().delete_chars(n);
    }
    fn scroll_up(&mut self, n: u16) {
        self.active_mut().scroll_up_region(n);
    }
    fn scroll_down(&mut self, n: u16) {
        self.active_mut().scroll_down_region(n);
    }
    fn erase_chars(&mut self, n: u16) {
        self.active_mut().erase_chars(n);
    }
    fn repeat_preceding_char(&mut self, n: u16) {
        let autowrap = self.modes.autowrap();
        let grapheme_clustering = self.modes.grapheme_clustering();
        self.active_mut()
            .repeat_preceding_char(n, autowrap, grapheme_clustering);
    }

    fn device_attributes(&mut self, kind: DaKind) {
        match kind {
            // DA1: "I am a VT220 with these features" (matches Ghostty's reply shape).
            DaKind::Primary => self.pending_writes.extend_from_slice(b"\x1b[?62;22c"),
            DaKind::Secondary => self.pending_writes.extend_from_slice(b"\x1b[>1;0;0c"),
        }
    }

    fn device_status_report(&mut self, kind: DsrKind) {
        match kind {
            DsrKind::Status => self.pending_writes.extend_from_slice(b"\x1b[0n"),
            DsrKind::CursorPosition => {
                let row = self.active().cursor.y + 1;
                let col = self.active().cursor.x + 1;
                self.pending_writes
                    .extend_from_slice(format!("\x1b[{row};{col}R").as_bytes());
            }
        }
    }

    fn window_op(&mut self, ps: u16, p1: u16, _p2: u16) {
        match ps {
            14 => self.pending_writes.extend_from_slice(
                format!(
                    "\x1b[4;{};{}t",
                    self.text_area_height_px, self.text_area_width_px
                )
                .as_bytes(),
            ),
            16 => self.pending_writes.extend_from_slice(
                format!("\x1b[6;{};{}t", self.cell_height_px, self.cell_width_px).as_bytes(),
            ),
            18 => self.pending_writes.extend_from_slice(
                format!("\x1b[8;{};{}t", self.size.rows, self.size.cols).as_bytes(),
            ),
            21 => {
                self.pending_writes.extend_from_slice(b"\x1b]l");
                self.pending_writes.extend_from_slice(self.title.as_bytes());
                self.pending_writes.extend_from_slice(b"\x1b\\");
            }
            // Ps[1] == 0 or 2 both mean "window title" (icon-title tracking
            // is unsupported); Ps[1] == 1 (icon-only) and anything else
            // falls through to the no-op/no-reply arm below.
            22 if matches!(p1, 0 | 2) => self.push_title(),
            23 if matches!(p1, 0 | 2) => self.pop_title(),
            _ => {} // 4/8/9/10/19/20, icon-only push/pop, unknown Ps — ignore (Ghostty parity).
        }
    }

    fn kitty_keyboard_query(&mut self) {
        let flags = self.kitty_keyboard.flags(self.active_is_alt);
        self.pending_writes
            .extend_from_slice(format!("\x1b[?{flags}u").as_bytes());
    }

    fn kitty_keyboard_push(&mut self, flags: u8) {
        self.kitty_keyboard.push(self.active_is_alt, flags);
    }

    fn kitty_keyboard_pop(&mut self, n: u16) {
        self.kitty_keyboard.pop(self.active_is_alt, n);
    }

    fn kitty_keyboard_set(&mut self, flags: u8, mode: u16) {
        self.kitty_keyboard
            .set(self.active_is_alt, flags, SetMode::from_param(mode));
    }

    fn osc_dispatch(&mut self, data: &[u8]) {
        if handle_color_osc(data, &mut self.colors, &mut self.pending_writes) {
            return;
        }
        if handle_clipboard_osc(
            data,
            &self.osc52_policy,
            &mut self.pending_clipboard_writes,
            &mut self.pending_clipboard_reads,
        ) {
            return;
        }
        if let Some(action) = parse_hyperlink_osc(data) {
            match action {
                HyperlinkOsc::Start(hyperlink) => self.set_current_hyperlink(hyperlink),
                HyperlinkOsc::End => self.clear_current_hyperlink(),
                HyperlinkOsc::Malformed => {}
            }
            return;
        }
        if let Some(notification) = parse_notification_osc(data) {
            self.push_notification(notification);
            return;
        }
        if let Some(action) = parse_cwd_osc(data) {
            if let CwdOsc::Set(cwd) = action {
                self.cwd = Some(cwd);
            }
            return;
        }
        if let Some(action) = parse_shell_integration_osc(data) {
            if let ShellIntegrationOsc::Mark { kind, exit_status } = action {
                let kind = match kind {
                    ShellIntegrationOscKind::PromptStart => ShellIntegrationMarkKind::PromptStart,
                    ShellIntegrationOscKind::InputStart => ShellIntegrationMarkKind::InputStart,
                    ShellIntegrationOscKind::CommandStart => ShellIntegrationMarkKind::CommandStart,
                    ShellIntegrationOscKind::CommandEnd => ShellIntegrationMarkKind::CommandEnd,
                };
                self.record_shell_mark(kind, exit_status);
            }
            return;
        }

        // OSC 0 (icon+title) / 2 (title): "<code>;<text>".
        let sep = data.iter().position(|&b| b == b';');
        if let Some(i) = sep {
            let code = &data[..i];
            if code == b"0" || code == b"2" {
                self.title = String::from_utf8_lossy(&data[i + 1..]).into_owned();
            }
        }
    }

    fn dcs_dispatch(&mut self, data: &[u8]) {
        if let Some(request) = data.strip_prefix(b"$q") {
            self.handle_decrqss(request);
        } else if let Some(payload) = data.strip_prefix(b"+q") {
            self.handle_xtgettcap(payload);
        } else if data == b">q" {
            self.push_dcs_response(format!(">|noa {}", env!("CARGO_PKG_VERSION")).as_bytes());
        }
    }

    fn kitty_graphics(&mut self, cmd: KittyGraphicsCommand) {
        // A truncated APC still identifies itself; reply EFBIG so the client
        // isn't left waiting on the response protocol.
        if cmd.truncated {
            self.kitty_images.abort();
            self.kitty_reply(
                cmd.image_id,
                cmd.image_number,
                cmd.image_id,
                cmd.placement_id,
                cmd.quiet,
                Err(KittyError::TooBig),
            );
            return;
        }
        if cmd.parse_error {
            self.kitty_reply(
                cmd.image_id,
                cmd.image_number,
                cmd.image_id,
                cmd.placement_id,
                cmd.quiet,
                Err(KittyError::Invalid),
            );
            return;
        }

        // A non-transmit command arriving mid-chunk aborts the pending transfer
        // (continuation chunks always parse as `Transmit`).
        if self.kitty_images.transfer_in_progress()
            && !matches!(
                cmd.action,
                KittyAction::Transmit | KittyAction::TransmitAndDisplay
            )
        {
            self.kitty_images.abort();
        }

        match cmd.action {
            KittyAction::Unsupported => self.kitty_reply(
                cmd.image_id,
                cmd.image_number,
                cmd.image_id,
                cmd.placement_id,
                cmd.quiet,
                Err(KittyError::Unsupported),
            ),
            KittyAction::Transmit | KittyAction::TransmitAndDisplay | KittyAction::Query => {
                self.kitty_transmit(cmd);
            }
            KittyAction::Put => self.kitty_put(&cmd),
            KittyAction::Delete => self.kitty_delete(&cmd),
        }
    }

    fn set_scroll_region(&mut self, top: u16, bottom: u16) {
        let screen = self.active_mut();
        let last = screen.rows.saturating_sub(1);
        let t = top.saturating_sub(1).min(last);
        let b = if bottom == 0 {
            last
        } else {
            bottom.saturating_sub(1).min(last)
        };
        if t < b {
            screen.region = ScrollRegion { top: t, bottom: b };
            screen.cursor_position(1, 1); // DECSTBM homes the cursor after a valid region.
        }
    }
}
