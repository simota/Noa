//! GUI-boundary glue between the GUI-agnostic `noa-ipc` crate and the
//! winit-owned `App` (noa-server spec §L2 "クレート配置 & 統合点"). Mirrors
//! the AppleScript bridge's two seams (`crate::macos_applescript`): reads go
//! through a main-thread-published, lock-guarded snapshot; mutations are
//! injected as a [`crate::events::UserEvent`] over the `EventLoopProxy` and
//! answered through a pending-request table (DEC-C — `UserEvent` derives
//! `Eq`, so no reply channel can live inside a variant).
//!
//! This module itself stays winit/wgpu-free (only `WindowId`/`PaneId` values
//! are threaded through as opaque `u64`s from callers) so it composes with
//! `noa-ipc`'s dependency-free `IpcBackend` contract.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use crossbeam_channel::{Receiver, Sender, bounded};
use parking_lot::Mutex;

use noa_core::{CellAttrs, Color};
use noa_grid::{Cell, Row as GridRow, Terminal};
use noa_ipc::protocol::{Span, SpanColor};
use noa_ipc::{
    Attr, GridResult, IpcBackend, IpcError, PaneRef, Panel, Row as WireRow, SplitDirection,
    TextResult, TextSource, WindowRef,
};

use crate::events::UserEvent;
use crate::io_thread::{PtyInputQueue, QueueInputResult, RawAttachTap};
use crate::session_store::{PreviewLine, SessionCard, SessionCardId};

/// Maps IPC-visible pane ids (minted, monotonic u64s — DEC-B) to/from the
/// internal `(WindowId, PaneId)` address a caller resolves through `App`. A
/// pane that has since closed is caught by the liveness check every
/// `AppIpcBackend` call already performs against `App`'s live window/pane
/// set, so a stale registry entry only ever costs a `-32002 UnknownPane` —
/// never a stale mutation. `prune` additionally drops entries for panes gone
/// from the current snapshot (`App::sync_ipc_snapshot`), since `WindowId`/
/// `PaneId` are never reused and a closed pane's key can't reappear.
#[derive(Default)]
pub(crate) struct IpcRegistry {
    next_id: u64,
    by_pane: HashMap<(u64, u64), u64>,
    by_id: HashMap<u64, (u64, u64)>,
}

impl IpcRegistry {
    /// Returns the existing id for `(window_id, pane_id)`, minting a fresh
    /// one on first sight.
    pub(crate) fn mint(&mut self, window_id: u64, pane_id: u64) -> u64 {
        if let Some(id) = self.by_pane.get(&(window_id, pane_id)) {
            return *id;
        }
        self.next_id += 1;
        let id = self.next_id;
        self.by_pane.insert((window_id, pane_id), id);
        self.by_id.insert(id, (window_id, pane_id));
        id
    }

    pub(crate) fn resolve(&self, ipc_id: u64) -> Option<(u64, u64)> {
        self.by_id.get(&ipc_id).copied()
    }

    /// Removes every entry whose `(window_id, pane_id)` key is absent from
    /// `live_keys`.
    pub(crate) fn prune(&mut self, live_keys: &std::collections::HashSet<(u64, u64)>) {
        self.by_pane.retain(|key, id| {
            let live = live_keys.contains(key);
            if !live {
                self.by_id.remove(id);
            }
            live
        });
    }

    /// Pane-dnd P2-1: move `old`'s existing registration to `new`,
    /// preserving its minted ipc id — mirrors `SessionStore::rekey` (same id,
    /// new key). Without this, a cross-tab move leaves the registration
    /// keyed under the pane's old `(window_id, pane_id)`; the next
    /// `sync_ipc_snapshot` tick then sees no live pane at that key (`prune`)
    /// and mints a *brand-new* ipc id at the new key instead, changing the
    /// pane's wire-visible identity out from under any client that already
    /// resolved the old one. A no-op when `old` has no registration, or
    /// `new` is already registered (never expected in practice: `PaneId` is
    /// process-global and unique, so no other pane can occupy `new`).
    pub(crate) fn rekey(&mut self, old: (u64, u64), new: (u64, u64)) {
        if old == new || self.by_pane.contains_key(&new) {
            return;
        }
        let Some(id) = self.by_pane.remove(&old) else {
            return;
        };
        self.by_id.insert(id, new);
        self.by_pane.insert(new, id);
    }
}

/// The main-thread-published, lock-guarded read surface `AppIpcBackend`
/// serves `noa.listPanels`/`noa.getText`/`noa.getGrid` from — the read half
/// of the two-seam pattern (spec "制約": mutation via `EventLoopProxy` /
/// reads via shared snapshot). Rebuilt each `about_to_wait` tick by
/// `App::sync_ipc_snapshot`, mirroring `sync_applescript_snapshot`'s cadence
/// discipline.
#[derive(Default)]
pub(crate) struct IpcShared {
    pub(crate) registry: IpcRegistry,
    /// Wire-form panels, prebuilt so `noa.listPanels` and diffing never touch
    /// a `Terminal` lock.
    pub(crate) panels: Vec<Panel>,
    /// `(window_id, pane_id) -> Arc<Mutex<Terminal>>` for the off-main-thread
    /// `getText`/`getGrid` reads (short-held lock, per spec "制約").
    pub(crate) terminals: HashMap<(u64, u64), Arc<Mutex<Terminal>>>,
    /// Pane-local raw attach endpoints. Unlike `terminals`, these are wired
    /// eagerly at pane spawn so an attach does not wait for the coarse read
    /// snapshot refresh.
    pub(crate) attach_panes: HashMap<(u64, u64), IpcAttachPane>,
}

impl IpcShared {
    /// Pane-dnd P2-1/L2(e): move a pane's IPC registrations — its minted
    /// registry id, its raw-attach registration, and its `terminals` read
    /// handle — to its new window key after a cross-tab move, so a live raw
    /// attach connection and its wire-visible ipc id both survive the move
    /// instead of the registry entry being pruned and re-minted (severing
    /// the attach) on the next `App::sync_ipc_snapshot` tick.
    ///
    /// P2-4 (review round 4): `terminals` *is* rebuilt fresh from the live
    /// `Surface` set on every `sync_ipc_snapshot` tick, but that tick only
    /// runs on its own `about_to_wait` cadence — a `getText`/`getGrid` call
    /// landing between this move committing and the next tick would
    /// otherwise resolve the pane's ipc id through `registry` (already
    /// rekeyed, above) to its new key, then find no entry for it in
    /// `terminals` (still under the old key) and fail the lookup as a
    /// spurious `PaneClosed`. Moving it here, in the same lock as the other
    /// two maps, closes that window instead of leaving it open until
    /// whenever the next tick happens to run.
    pub(crate) fn rekey_pane(&mut self, old: (u64, u64), new: (u64, u64)) {
        self.registry.rekey(old, new);
        if old != new
            && let Some(attach) = self.attach_panes.remove(&old)
        {
            self.attach_panes.insert(new, attach);
        }
        if old != new
            && let Some(terminal) = self.terminals.remove(&old)
        {
            self.terminals.insert(new, terminal);
        }
    }

    /// P2 (review round 11): resolve a pane's ipc id to its live `terminals`
    /// read handle in a *single* lock hold. `getText`/`getGrid` previously
    /// resolved the id (locking, then unlocking) and only then looked the
    /// handle up under a second lock acquisition — a cross-tab
    /// [`Self::rekey_pane`] interleaving between the two moved the terminal
    /// to the new key, so the second lookup missed the (now stale) resolved
    /// key and failed as a spurious `PaneClosed` for a live pane. Doing both
    /// the registry resolution and the handle clone here, under one lock,
    /// closes that window — a rekey can only run entirely before or entirely
    /// after, and either ordering resolves a matching key/handle pair.
    /// Mirrors [`AppIpcBackend::resolve_attach_pane`]'s single-lock shape.
    pub(crate) fn resolve_terminal(&self, pane: PaneRef) -> Result<Arc<Mutex<Terminal>>, IpcError> {
        let key = self.registry.resolve(pane).ok_or(IpcError::UnknownPane)?;
        self.terminals
            .get(&key)
            .cloned()
            .ok_or(IpcError::PaneClosed)
    }
}

/// The local pane resources needed by the raw attach backend. Clones retain
/// the pane endpoint but [`RawAttachTap::shutdown`] permanently rejects a
/// raced open after the pane has closed.
#[derive(Clone)]
pub(crate) struct IpcAttachPane {
    terminal: Arc<Mutex<Terminal>>,
    raw_output: RawAttachTap,
    input: PtyInputQueue,
}

impl IpcAttachPane {
    pub(crate) fn new(
        terminal: Arc<Mutex<Terminal>>,
        raw_output: RawAttachTap,
        input: PtyInputQueue,
    ) -> Self {
        Self {
            terminal,
            raw_output,
            input,
        }
    }

    fn validate(&self) -> Result<(), IpcError> {
        self.raw_output
            .is_available()
            .then_some(())
            .ok_or(IpcError::PaneClosed)
    }

    fn open(
        &self,
        generation: u64,
        output: noa_ipc::AttachOutputSender,
    ) -> Result<Vec<u8>, IpcError> {
        // This is the seed/live ordering boundary: the io thread also takes
        // this Terminal lock before parsing bytes and cloning the raw sink.
        // No socket or backpressured channel send occurs in this section.
        let terminal = self.terminal.lock();
        self.raw_output
            .register_and_seed(generation, output, &terminal)
            .map_err(|()| IpcError::PaneClosed)
    }

    fn write(&self, generation: u64, bytes: &[u8]) -> Result<(), IpcError> {
        match self.raw_output.queue_input(generation, &self.input, bytes) {
            Ok(QueueInputResult::Queued | QueueInputResult::Deferred) => Ok(()),
            Ok(QueueInputResult::Dropped) => Err(IpcError::Internal(
                "attach input queue capacity exceeded".to_string(),
            )),
            Ok(QueueInputResult::Disconnected) => Err(IpcError::PaneClosed),
            Err(()) => Err(IpcError::PaneClosed),
        }
    }

    fn detach(&self, generation: u64) {
        self.raw_output.detach(generation);
    }

    pub(crate) fn shutdown(&self) {
        self.raw_output.shutdown();
    }
}

/// One in-flight IPC mutation awaiting the main thread's reply (DEC-C).
pub(crate) struct PendingIpcAction {
    pub(crate) action: IpcActionKind,
    pub(crate) reply: Sender<Result<IpcActionReply, IpcError>>,
}

/// GUI-owned mutations, re-validated and executed on the main thread through
/// the same internal methods the existing `UserEvent` arms already call.
pub(crate) enum IpcActionKind {
    FocusPane {
        pane: PaneRef,
    },
    NewTab {
        window: Option<WindowRef>,
    },
    Split {
        pane: PaneRef,
        direction: SplitDirection,
    },
    ClosePane {
        pane: PaneRef,
    },
    SendText {
        pane: PaneRef,
        text: String,
        paste: bool,
    },
    ResizePane {
        pane: PaneRef,
        cols: u16,
        rows: u16,
    },
}

pub(crate) enum IpcActionReply {
    Ok,
    NewPane(PaneRef),
}

/// The shared pending-request table `UserEvent::IpcAction` resolves against.
pub(crate) type IpcPendingTable = Arc<Mutex<HashMap<u64, PendingIpcAction>>>;

/// How long a connection thread blocks for the main thread's reply before
/// treating the request as failed (guards against a wedged event loop). A
/// mutation that times out here may still land on the pty/window state
/// later — the main thread keeps executing the already-dispatched
/// `UserEvent::IpcAction` regardless of whether anyone is still waiting on
/// `rx`, it just fails to deliver a reply. Callers observe this as an
/// `Internal` error even though the action succeeds; v1 accepts
/// at-least-once/timeout-may-still-execute semantics for `control`/`input`
/// mutations rather than adding a cancellation path (spec F-7).
const REQUEST_TIMEOUT: Duration = Duration::from_secs(2);

/// Hard ceiling on `noa.getGrid`'s `rowCount`, independent of the client's
/// requested value (F-1 / NFR-2/NFR-4): bounds how many rows a single call
/// walks under the `Terminal` lock, regardless of `cap_grid_rows`' later
/// byte-budget trim. Sized generously above the 256KiB response cap's
/// typical row count so `hasMore` paging — not this clamp — is normally
/// what limits a response.
const MAX_GRID_ROWS_PER_REQUEST: u64 = 2048;

fn terminal_grid_result(terminal: &Terminal, start_row: u64, row_count: u64) -> GridResult {
    let cols = terminal.active().cols as u32;
    let coordinate_generation = terminal.grid_coordinate_generation();
    let oldest_row = terminal.active_oldest_row() as u64;
    let next_row = terminal.active_next_row() as u64;
    // Clamp independent of `cap_grid_rows`' later byte-budget trim (F-1):
    // never loop over an unclamped client-supplied `row_count` while holding
    // the `Terminal` lock.
    let clamped_row_count = row_count.min(MAX_GRID_ROWS_PER_REQUEST);
    let requested_end = start_row.saturating_add(row_count).min(next_row);
    let start = start_row.max(oldest_row).min(next_row);
    let end = start
        .saturating_add(clamped_row_count)
        .min(requested_end)
        .min(next_row)
        .max(start);
    let has_more = end < requested_end;
    let rows = (start..end)
        .filter_map(|y| {
            terminal
                .active_absolute_row(y as usize)
                .map(|grid_row| WireRow {
                    row: y,
                    spans: row_to_spans(&grid_row),
                })
        })
        .collect();
    GridResult {
        cols,
        coordinate_generation,
        oldest_row,
        next_row,
        rows,
        has_more,
    }
}

/// The `noa-ipc` backend implementation wired to a running `App`. Cheap to
/// clone; every clone shares the same registry/snapshot/pending-table state.
#[derive(Clone)]
pub(crate) struct AppIpcBackend {
    pub(crate) shared: Arc<Mutex<IpcShared>>,
    pub(crate) proxy: winit::event_loop::EventLoopProxy<UserEvent>,
    pub(crate) pending: IpcPendingTable,
    pub(crate) next_request: Arc<AtomicU64>,
}

impl AppIpcBackend {
    /// Resolve a pane's ipc id to its live `terminals` read handle under a
    /// single `IpcShared` lock (P2 review round 11) — see
    /// [`IpcShared::resolve_terminal`] for why the resolution and the handle
    /// clone must not straddle two lock acquisitions.
    fn resolve_terminal(&self, pane: PaneRef) -> Result<Arc<Mutex<Terminal>>, IpcError> {
        self.shared.lock().resolve_terminal(pane)
    }

    fn resolve_attach_pane(&self, pane: PaneRef) -> Result<IpcAttachPane, IpcError> {
        let shared = self.shared.lock();
        let key = shared.registry.resolve(pane).ok_or(IpcError::UnknownPane)?;
        shared
            .attach_panes
            .get(&key)
            .cloned()
            .ok_or(IpcError::PaneClosed)
    }

    /// Submit a mutation to the main thread and block for its reply
    /// (DEC-C). A dropped reply sender (main thread tore the pane down
    /// mid-flight) surfaces as `PaneClosed`; a timeout as `Internal`.
    fn submit(&self, action: IpcActionKind) -> Result<IpcActionReply, IpcError> {
        let request_id = self.next_request.fetch_add(1, Ordering::SeqCst);
        let (tx, rx): (Sender<Result<IpcActionReply, IpcError>>, Receiver<_>) = bounded(1);
        self.pending
            .lock()
            .insert(request_id, PendingIpcAction { action, reply: tx });
        if self
            .proxy
            .send_event(UserEvent::IpcAction { request_id })
            .is_err()
        {
            self.pending.lock().remove(&request_id);
            return Err(IpcError::Internal("event loop gone".to_string()));
        }
        match rx.recv_timeout(REQUEST_TIMEOUT) {
            Ok(result) => result,
            Err(_) => {
                self.pending.lock().remove(&request_id);
                Err(IpcError::Internal("request timed out".to_string()))
            }
        }
    }
}

impl IpcBackend for AppIpcBackend {
    fn list_panels(&self) -> Vec<Panel> {
        self.shared.lock().panels.clone()
    }

    fn get_text(
        &self,
        pane: PaneRef,
        source: TextSource,
        max_bytes: usize,
    ) -> Result<TextResult, IpcError> {
        let terminal = self.resolve_terminal(pane)?;
        let mut terminal = terminal.lock();
        let (text, truncated) = match source {
            TextSource::Screen => (screen_text(&terminal.active().visible_rows()), false),
            // Walks rows from the tail under the lock rather than
            // materializing the full `scrollback_text()` and truncating
            // after the fact (F-1 / NFR-4).
            TextSource::Scrollback => terminal.scrollback_text_tail(max_bytes).unwrap_or_default(),
        };
        Ok(TextResult { text, truncated })
    }

    fn get_grid(
        &self,
        pane: PaneRef,
        start_row: u64,
        row_count: u64,
    ) -> Result<GridResult, IpcError> {
        let terminal = self.resolve_terminal(pane)?;
        let terminal = terminal.lock();
        Ok(terminal_grid_result(&terminal, start_row, row_count))
    }

    fn send_text(&self, pane: PaneRef, text: &str, paste: bool) -> Result<(), IpcError> {
        match self.submit(IpcActionKind::SendText {
            pane,
            text: text.to_string(),
            paste,
        })? {
            IpcActionReply::Ok => Ok(()),
            IpcActionReply::NewPane(_) => Ok(()),
        }
    }

    fn focus_pane(&self, pane: PaneRef) -> Result<(), IpcError> {
        self.submit(IpcActionKind::FocusPane { pane }).map(|_| ())
    }

    fn new_tab(&self, window: Option<WindowRef>) -> Result<PaneRef, IpcError> {
        match self.submit(IpcActionKind::NewTab { window })? {
            IpcActionReply::NewPane(pane) => Ok(pane),
            IpcActionReply::Ok => Err(IpcError::Internal("new_tab returned no pane".to_string())),
        }
    }

    fn split(&self, pane: PaneRef, direction: SplitDirection) -> Result<PaneRef, IpcError> {
        match self.submit(IpcActionKind::Split { pane, direction })? {
            IpcActionReply::NewPane(pane) => Ok(pane),
            IpcActionReply::Ok => Err(IpcError::Internal("split returned no pane".to_string())),
        }
    }

    fn close_pane(&self, pane: PaneRef) -> Result<(), IpcError> {
        self.submit(IpcActionKind::ClosePane { pane }).map(|_| ())
    }

    fn validate_attach(&self, pane: PaneRef) -> Result<(), IpcError> {
        self.resolve_attach_pane(pane)?.validate()
    }

    fn open_attach(
        &self,
        pane: PaneRef,
        generation: u64,
        output: noa_ipc::AttachOutputSender,
    ) -> Result<Vec<u8>, IpcError> {
        self.resolve_attach_pane(pane)?.open(generation, output)
    }

    fn write_attach(&self, pane: PaneRef, generation: u64, bytes: &[u8]) -> Result<(), IpcError> {
        self.resolve_attach_pane(pane)?.write(generation, bytes)
    }

    fn detach_attach(&self, pane: PaneRef, generation: u64) -> Result<(), IpcError> {
        // A pane-close cleanup may already have removed the bridge. Detach is
        // intentionally idempotent, and a stale generation can never clear
        // a newer one because RawAttachTap performs the generation check.
        if let Ok(attach) = self.resolve_attach_pane(pane) {
            attach.detach(generation);
        }
        Ok(())
    }

    fn resize_pane(&self, pane: PaneRef, cols: u16, rows: u16) -> Result<(), IpcError> {
        self.resolve_attach_pane(pane)?.validate()?;
        self.submit(IpcActionKind::ResizePane { pane, cols, rows })
            .map(|_| ())
    }
}

/// Join visible screen rows into `noa.getText(source: "screen")` plain text
/// (R-2). Trailing spaces are trimmed per unwrapped row — a soft-wrapped
/// row is full width, so its trailing spaces are real content continued on
/// the next row and must survive the join (mirrors `noa-grid`'s
/// `push_selected_row_text`). The trim tracks each row's own start offset
/// so it can never eat into a preceding wrapped row's real trailing spaces.
fn screen_text(rows: &[GridRow]) -> String {
    rows.iter().fold(String::new(), |mut out, row| {
        let before_len = out.len();
        for cell in &row.cells {
            cell.push_text_to(&mut out);
        }
        if !row.wrapped {
            while out.len() > before_len && out.ends_with(' ') {
                out.pop();
            }
            out.push('\n');
        }
        out
    })
}

/// Coalesce a grid row's cells into color/attr runs (mirrors
/// `io_thread::sidebar::preview_spans`' fg-only coalescing, widened to also
/// track bg/attrs per spec §L2 "Grid ペイロード").
pub(crate) fn row_to_spans(row: &GridRow) -> Vec<Span> {
    let mut spans: Vec<Span> = Vec::new();
    for cell in &row.cells {
        if cell.attrs.contains(CellAttrs::WIDE_SPACER) {
            continue;
        }
        let fg = wire_color(cell.fg);
        let bg = wire_color(cell.bg);
        let attrs = wire_attrs(cell);
        let same_style = spans
            .last()
            .is_some_and(|span: &Span| span.fg == fg && span.bg == bg && span.attrs == attrs);
        if same_style {
            let span = spans.last_mut().expect("checked above");
            cell.push_text_to(&mut span.text);
        } else {
            let mut text = String::new();
            cell.push_text_to(&mut text);
            spans.push(Span {
                text,
                fg,
                bg,
                attrs,
            });
        }
    }
    spans
}

fn wire_color(color: Color) -> Option<SpanColor> {
    match color {
        Color::Default => None,
        Color::Palette(index) => Some(SpanColor::Palette(index)),
        Color::Rgb(rgb) => Some(SpanColor::rgb(rgb.r, rgb.g, rgb.b)),
    }
}

fn wire_attrs(cell: &Cell) -> Option<Vec<Attr>> {
    let mut out = Vec::new();
    let attrs = cell.attrs;
    if attrs.contains(CellAttrs::BOLD) {
        out.push(Attr::Bold);
    }
    if attrs.contains(CellAttrs::FAINT) {
        out.push(Attr::Faint);
    }
    if attrs.contains(CellAttrs::ITALIC) {
        out.push(Attr::Italic);
    }
    if attrs.contains(CellAttrs::DOUBLE_UNDERLINE) {
        out.push(Attr::DoubleUnderline);
    } else if attrs.contains(CellAttrs::CURLY_UNDERLINE) {
        out.push(Attr::CurlyUnderline);
    } else if attrs.contains(CellAttrs::DOTTED_UNDERLINE) {
        out.push(Attr::DottedUnderline);
    } else if attrs.contains(CellAttrs::DASHED_UNDERLINE) {
        out.push(Attr::DashedUnderline);
    } else if attrs.contains(CellAttrs::UNDERLINE) {
        out.push(Attr::Underline);
    }
    if attrs.contains(CellAttrs::BLINK) {
        out.push(Attr::Blink);
    }
    if attrs.contains(CellAttrs::INVERSE) {
        out.push(Attr::Inverse);
    }
    if attrs.contains(CellAttrs::INVISIBLE) {
        out.push(Attr::Invisible);
    }
    if attrs.contains(CellAttrs::STRIKETHROUGH) {
        out.push(Attr::Strikethrough);
    }
    if attrs.contains(CellAttrs::OVERLINE) {
        out.push(Attr::Overline);
    }
    if out.is_empty() { None } else { Some(out) }
}

/// `SessionCard` -> wire `Panel` (spec §L2 "ID モデル & Panel メタデータ":
/// "`SessionCard` を鏡写しにする"). `preview` reuses the card's own color
/// runs, converted through the same [`wire_color`] mapping as grid rows.
pub(crate) fn card_to_panel(
    ipc_id: u64,
    window_group_id: u64,
    window_id: u64,
    card: &SessionCard,
    attachable: bool,
) -> Panel {
    Panel {
        window_group_id: window_group_id.into(),
        window_id: window_id.into(),
        pane_id: ipc_id.into(),
        name: card.display_name().to_string(),
        cwd: card.cwd.clone(),
        branch: card.branch.clone(),
        process: card.process.clone(),
        busy: card.busy,
        attention: card.attention,
        attachable,
        preview: card
            .preview
            .iter()
            .enumerate()
            .map(|(row, line)| preview_line_to_row(row as u64, line))
            .collect(),
    }
}

/// `row` is a 0-based index into `preview`'s lines, not an absolute grid
/// row — previews are relative to the pane's most recent viewport.
fn preview_line_to_row(row: u64, line: &PreviewLine) -> WireRow {
    WireRow {
        row,
        spans: line
            .iter()
            .map(|span| Span {
                text: span.text.clone(),
                fg: wire_color(span.fg),
                bg: None,
                attrs: None,
            })
            .collect(),
    }
}

/// `SessionCardId` -> the `(window_id, pane_id)` key `IpcRegistry`/
/// `IpcShared::terminals` index by.
pub(crate) fn registry_key(id: SessionCardId) -> (u64, u64) {
    (id.window_id.0, id.pane_id.get())
}

#[cfg(test)]
mod tests {
    use super::*;
    use noa_core::Rgb;
    use noa_grid::cell::Cell;

    #[test]
    fn registry_mint_is_idempotent_and_resolves_both_ways() {
        let mut registry = IpcRegistry::default();
        let first = registry.mint(10, 1);
        let second = registry.mint(10, 1);
        assert_eq!(
            first, second,
            "minting the same pane twice returns the same id"
        );

        let third = registry.mint(10, 2);
        assert_ne!(first, third);

        assert_eq!(registry.resolve(first), Some((10, 1)));
        assert_eq!(
            registry.resolve(999),
            None,
            "unminted id resolves to nothing"
        );
    }

    #[test]
    fn prune_drops_entries_absent_from_the_live_set_and_keeps_the_rest() {
        let mut registry = IpcRegistry::default();
        let closed = registry.mint(10, 1);
        let kept = registry.mint(10, 2);

        let mut live = std::collections::HashSet::new();
        live.insert((10, 2));
        registry.prune(&live);

        assert_eq!(
            registry.resolve(closed),
            None,
            "pane absent from the live set is pruned"
        );
        assert_eq!(
            registry.resolve(kept),
            Some((10, 2)),
            "pane present in the live set survives"
        );

        // A closed pane's id never comes back, even if its key is minted
        // again (fresh id, not a resurrection of the pruned one).
        let reminted = registry.mint(10, 1);
        assert_ne!(reminted, closed);
    }

    // Pane-dnd P2-1: a cross-tab move must preserve the pane's minted ipc id
    // under its new `(window_id, pane_id)` key — not prune the old key and
    // mint a fresh id, which would change the pane's wire-visible identity
    // out from under a client that already resolved the old one.
    #[test]
    fn rekey_preserves_the_minted_id_under_the_new_key() {
        let mut registry = IpcRegistry::default();
        let other = registry.mint(10, 2);
        let id = registry.mint(10, 1);

        registry.rekey((10, 1), (20, 1));

        assert_eq!(
            registry.resolve(id),
            Some((20, 1)),
            "the id now resolves to the new key"
        );
        assert_eq!(
            registry.by_pane.get(&(10, 1)),
            None,
            "the old key is no longer registered"
        );
        assert_eq!(
            registry.mint(20, 1),
            id,
            "re-minting the new key returns the same (preserved) id, not a fresh one"
        );
        assert_eq!(
            registry.resolve(other),
            Some((10, 2)),
            "an unrelated pane's registration is untouched"
        );
    }

    // P2-4 (pane-dnd review round 4): `IpcShared::rekey_pane` must move all
    // three of a pane's registrations — registry id, raw-attach endpoint,
    // *and* its `terminals` read handle — to the new key in the same call,
    // not just the first two. A `getText`/`getGrid` landing between a
    // cross-tab move and the next `sync_ipc_snapshot` tick resolves the
    // pane's ipc id through the (already-rekeyed) registry, so a stale
    // `terminals` entry would fail that lookup as a spurious `PaneClosed`.
    #[test]
    fn rekey_pane_moves_registry_attach_and_terminals_to_the_new_key() {
        let mut shared = IpcShared::default();
        let ipc_id = shared.registry.mint(10, 1);
        let terminal = Arc::new(Mutex::new(Terminal::new(noa_core::GridSize::new(80, 24))));
        shared.terminals.insert((10, 1), terminal.clone());
        let (queue, _rx) = crate::io_thread::input_channel();
        shared.attach_panes.insert(
            (10, 1),
            IpcAttachPane::new(terminal.clone(), RawAttachTap::default(), queue),
        );

        shared.rekey_pane((10, 1), (20, 1));

        assert_eq!(
            shared.registry.resolve(ipc_id),
            Some((20, 1)),
            "the minted registry id follows the pane"
        );
        assert!(
            !shared.terminals.contains_key(&(10, 1)),
            "the old terminals key is gone"
        );
        assert!(
            Arc::ptr_eq(
                shared
                    .terminals
                    .get(&(20, 1))
                    .expect("terminal moved to the new key"),
                &terminal
            ),
            "the same terminal handle follows the pane, not a fresh lookup"
        );
        assert!(
            !shared.attach_panes.contains_key(&(10, 1)),
            "the old attach_panes key is gone"
        );
        assert!(
            shared.attach_panes.contains_key(&(20, 1)),
            "the raw-attach registration follows the pane"
        );
    }

    // P2 (pane-dnd review round 11): `getText`/`getGrid` must resolve the
    // pane's ipc id AND clone its `terminals` handle under one lock. The old
    // two-phase path resolved the key, released the lock, then re-acquired it
    // to fetch the handle — a cross-tab `rekey_pane` interleaving between the
    // two moved the terminal off the resolved key, so the second lookup
    // missed and returned a spurious `PaneClosed` for a live pane. This test
    // pins the two-phase break (a key resolved *before* a rekey no longer
    // finds its terminal) and shows the single-call resolve still returns the
    // live handle.
    #[test]
    fn resolve_terminal_survives_a_rekey_that_breaks_the_two_phase_lookup() {
        let mut shared = IpcShared::default();
        let ipc_id = shared.registry.mint(10, 1);
        let terminal = Arc::new(Mutex::new(Terminal::new(noa_core::GridSize::new(80, 24))));
        shared.terminals.insert((10, 1), terminal.clone());

        // Phase 1 of the old path: resolve the id to its current key.
        let stale_key = shared
            .registry
            .resolve(ipc_id)
            .expect("live pane resolves before the move");
        assert_eq!(stale_key, (10, 1));

        // A cross-tab move rekeys the pane before the old path's phase 2.
        shared.rekey_pane((10, 1), (20, 1));
        assert!(
            !shared.terminals.contains_key(&stale_key),
            "the two-phase lookup would now miss: terminal moved off the resolved key"
        );

        // The single-lock resolve resolves the (rekeyed) id and clones its
        // handle together, so it returns the live terminal rather than a
        // spurious PaneClosed.
        let fetched = shared
            .resolve_terminal(ipc_id)
            .expect("live pane still resolves through the combined path");
        assert!(
            Arc::ptr_eq(&fetched, &terminal),
            "the combined resolve returns the same live handle at the new key"
        );
    }

    #[test]
    fn resolve_terminal_reports_unknown_pane_and_pane_closed_distinctly() {
        let mut shared = IpcShared::default();
        // An id that was never minted is UnknownPane.
        assert!(matches!(
            shared.resolve_terminal(999),
            Err(IpcError::UnknownPane)
        ));
        // A minted id whose terminal handle is absent is PaneClosed.
        let ipc_id = shared.registry.mint(10, 1);
        assert!(matches!(
            shared.resolve_terminal(ipc_id),
            Err(IpcError::PaneClosed)
        ));
    }

    #[test]
    fn rekey_is_a_no_op_when_the_old_key_has_no_registration_or_the_new_key_is_taken() {
        let mut registry = IpcRegistry::default();
        let a = registry.mint(10, 1);
        let b = registry.mint(10, 2);

        // Nothing registered under (99, 9) — rekeying it must not disturb
        // any existing entry.
        registry.rekey((99, 9), (100, 1));
        assert_eq!(registry.resolve(a), Some((10, 1)));
        assert_eq!(registry.resolve(b), Some((10, 2)));

        // The destination key is already taken by `b` — `a` must not
        // clobber it (this never occurs in practice, since `PaneId` is
        // process-global and unique, but the guard must still hold).
        registry.rekey((10, 1), (10, 2));
        assert_eq!(
            registry.resolve(a),
            Some((10, 1)),
            "a's registration is left in place when the destination is taken"
        );
        assert_eq!(registry.resolve(b), Some((10, 2)));
    }

    fn cell(ch: char, fg: Color) -> Cell {
        Cell {
            ch,
            fg,
            ..Cell::default()
        }
    }

    #[test]
    fn row_to_spans_coalesces_same_style_runs() {
        let row = GridRow::from_cells(
            vec![
                cell('a', Color::Default),
                cell('b', Color::Default),
                cell('c', Color::Rgb(Rgb::new(255, 0, 0))),
            ],
            false,
            false,
        );
        let spans = row_to_spans(&row);
        assert_eq!(
            spans.len(),
            2,
            "adjacent same-style cells fold into one span"
        );
        assert_eq!(spans[0].text, "ab");
        assert_eq!(spans[0].fg, None, "Color::Default omits the wire fg field");
        assert_eq!(spans[1].text, "c");
        assert_eq!(spans[1].fg, Some(SpanColor::rgb(255, 0, 0)));
    }

    #[test]
    fn screen_text_preserves_wrapped_row_trailing_spaces() {
        // R-2: a wrapped row's real trailing spaces must survive even when
        // the row after it is blank and unwrapped — the trim on the blank
        // row must not reach back across the row boundary into the wrapped
        // row's content.
        let wrapped = GridRow::from_cells(
            vec![
                cell('a', Color::Default),
                cell(' ', Color::Default),
                cell(' ', Color::Default),
            ],
            true,
            false,
        );
        let blank = GridRow::from_cells(vec![cell(' ', Color::Default)], false, false);
        let text = screen_text(&[wrapped, blank]);
        assert_eq!(
            text, "a  \n",
            "wrapped row's trailing spaces survive; only the blank row's own space is trimmed"
        );
    }

    #[test]
    fn wire_color_maps_every_color_variant() {
        assert_eq!(wire_color(Color::Default), None);
        assert_eq!(wire_color(Color::Palette(5)), Some(SpanColor::Palette(5)));
        assert_eq!(
            wire_color(Color::Rgb(Rgb::new(1, 2, 3))),
            Some(SpanColor::rgb(1, 2, 3))
        );
    }

    #[test]
    fn grid_result_reports_tail_bounds_and_stable_rows_after_eviction() {
        let mut terminal = Terminal::new(noa_core::GridSize::new(80, 4));
        let mut bytes = Vec::new();
        for i in 0..2_000 {
            bytes.extend_from_slice(format!("line-{i:04}-{}\r\n", "x".repeat(68)).as_bytes());
        }
        noa_vt::Stream::new().feed(&bytes, &mut terminal);
        terminal.set_scrollback_limit_bytes(1);

        let oldest = terminal.active_oldest_row() as u64;
        let next = terminal.active_next_row() as u64;
        let tail_start = next.saturating_sub(48).max(oldest);
        let result = terminal_grid_result(&terminal, tail_start, 48);

        assert!(oldest > 0, "test setup must evict retained scrollback");
        assert_eq!(result.oldest_row, oldest);
        assert_eq!(result.next_row, next);
        assert_eq!(
            result.coordinate_generation,
            terminal.grid_coordinate_generation()
        );
        assert_eq!(result.rows.first().map(|row| row.row), Some(tail_start));
        assert_eq!(result.rows.last().map(|row| row.row), Some(next - 1));
        assert!(result.rows.iter().all(|row| row.row >= oldest));
    }

    #[test]
    fn grid_result_applies_row_cap_after_the_evicted_prefix() {
        let mut terminal = Terminal::new(noa_core::GridSize::new(80, 4));
        let mut bytes = Vec::new();
        for i in 0..6_000 {
            bytes.extend_from_slice(format!("line-{i:04}-{}\r\n", "x".repeat(68)).as_bytes());
        }
        noa_vt::Stream::new().feed(&bytes, &mut terminal);
        terminal.set_scrollback_limit_bytes(1);

        let oldest = terminal.active_oldest_row() as u64;
        assert!(oldest > MAX_GRID_ROWS_PER_REQUEST);
        let result = terminal_grid_result(&terminal, 0, oldest + 1);

        assert_eq!(result.rows.len(), 1);
        assert_eq!(result.rows[0].row, oldest);
        assert!(!result.has_more);
    }
}
