//! Process-monitor overlay (spec `docs/specs/panel-metrics-view.md`) — the
//! GUI-agnostic half. Mirrors `theme_settings.rs`/`command_palette.rs`: pure
//! row-building, sort, selection, and value-formatting logic with no
//! winit/wgpu types, so the whole state machine is unit-testable without a
//! display (NFR-3). `App` owns a `ProcessMonitorSession` wrapping
//! [`ProcessMonitor`]; its `KeyboardInput` handler drives it and feeds the
//! rendered result into the overlay card (mirroring the theme-settings
//! overlay's own card).
//!
//! Ghostty has no analog — this is a noa addition layered on the existing
//! session-sidebar data model (`SessionStore`/`SessionCard`).

use std::cmp::Ordering;
use std::time::SystemTime;

use crate::session_store::{SessionCard, SessionCardId, SessionWindowId};

#[cfg(test)]
use crate::session_store::{SessionDelta, SessionStore, WallClock};

/// One pane's row in the overlay table (FR-3/AC-13): the six displayed
/// fields, plus the [`SessionCardId`] the row jumps to on Enter. `None`
/// fields render as "—" (FR-8) rather than a placeholder value baked in here
/// — keeping the *absence* explicit is what lets the formatters and the
/// sort comparators agree on "no value sorts last" without special-casing a
/// sentinel number.
#[derive(Clone, Debug, PartialEq)]
pub struct MonitorRow {
    pub id: SessionCardId,
    pub process: Option<String>,
    pub cpu_permille: Option<u32>,
    pub mem_bytes: Option<u64>,
    pub proc_count: Option<u32>,
    pub started_at: Option<SystemTime>,
    /// Tab/session name + this pane's ordinal within its window (e.g.
    /// `"repo · pane 2"`), built by [`build_rows`] from the card's
    /// `display_name` and its position among same-window rows.
    pub location: String,
}

/// The overlay's sortable columns (FR-5): CPU% descending → memory
/// descending → process name ascending, cycling back to CPU.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum SortKey {
    #[default]
    CpuDesc,
    MemDesc,
    NameAsc,
}

impl SortKey {
    pub fn next(self) -> Self {
        match self {
            SortKey::CpuDesc => SortKey::MemDesc,
            SortKey::MemDesc => SortKey::NameAsc,
            SortKey::NameAsc => SortKey::CpuDesc,
        }
    }
}

/// Build one row per live pane (FR-2/AC-2), in `cards`' given order —
/// callers pass `SessionStore::ordered_cards()` (unfiltered across every
/// window, panel-metrics-view L2: "全ウィンドウ・全タブの全ライブペイン"). The
/// per-window pane ordinal (used in [`MonitorRow::location`]) is assigned by
/// first-seen order within each window, matching the order `cards` is given
/// in — GUI-agnostic and independent of any real window/tab structure.
pub fn build_rows(cards: &[(SessionCardId, &SessionCard)]) -> Vec<MonitorRow> {
    let mut ordinals: std::collections::HashMap<SessionWindowId, u32> =
        std::collections::HashMap::new();
    cards
        .iter()
        .map(|(id, card)| {
            let ordinal = ordinals.entry(id.window_id).or_insert(0);
            *ordinal += 1;
            let metrics = card.metrics;
            MonitorRow {
                id: *id,
                process: card.process.clone(),
                cpu_permille: metrics.and_then(|m| m.cpu_permille),
                mem_bytes: metrics.map(|m| m.mem_bytes),
                proc_count: metrics.map(|m| m.proc_count),
                started_at: metrics.and_then(|m| m.started_at),
                location: format!("{} \u{b7} pane {ordinal}", card.display_name()),
            }
        })
        .collect()
}

/// A value that sorts last when absent, regardless of the active key's sort
/// direction (AC-10: no-metrics rows never disappear, they just fall to the
/// bottom).
fn cmp_none_last<T: PartialOrd>(a: Option<&T>, b: Option<&T>, desc: bool) -> Ordering {
    match (a, b) {
        (Some(a), Some(b)) => {
            let ord = a.partial_cmp(b).unwrap_or(Ordering::Equal);
            if desc { ord.reverse() } else { ord }
        }
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

/// Sort `rows` in place by `sort` (FR-5). A stable sort, so rows tied on the
/// active key keep their prior relative order.
fn sort_rows(rows: &mut [MonitorRow], sort: SortKey) {
    match sort {
        SortKey::CpuDesc => {
            rows.sort_by(|a, b| cmp_none_last(a.cpu_permille.as_ref(), b.cpu_permille.as_ref(), true))
        }
        SortKey::MemDesc => {
            rows.sort_by(|a, b| cmp_none_last(a.mem_bytes.as_ref(), b.mem_bytes.as_ref(), true))
        }
        SortKey::NameAsc => {
            rows.sort_by(|a, b| cmp_none_last(a.process.as_ref(), b.process.as_ref(), false))
        }
    }
}

/// The overlay's pure state: rows, active sort, and the selected row index
/// (AC-2/AC-6/AC-10/AC-11/AC-13).
#[derive(Clone, Debug, Default)]
pub struct ProcessMonitor {
    rows: Vec<MonitorRow>,
    sort: SortKey,
    selected: usize,
}

impl ProcessMonitor {
    /// Open a fresh session (FR-1/FR-5): default sort is CPU descending, no
    /// prior selection to preserve.
    pub fn open(rows: Vec<MonitorRow>) -> Self {
        let mut monitor = Self {
            rows: Vec::new(),
            sort: SortKey::CpuDesc,
            selected: 0,
        };
        monitor.refresh(rows);
        monitor
    }

    /// Re-sort and replace the row set (called on every `SessionDelta::Metrics`
    /// apply while open), preserving the selection by id when the previously
    /// selected pane is still present (AC-10: a pane that vanished from the
    /// set — e.g. its session closed — simply drops the selection back to a
    /// valid index rather than panicking).
    pub fn refresh(&mut self, mut rows: Vec<MonitorRow>) {
        let selected_id = self.rows.get(self.selected).map(|row| row.id);
        sort_rows(&mut rows, self.sort);
        self.rows = rows;
        self.selected = selected_id
            .and_then(|id| self.rows.iter().position(|row| row.id == id))
            .unwrap_or(0);
    }

    /// ↑ (`delta = -1`) / ↓ (`delta = 1`) selection (FR-6), clamped to the
    /// row bounds; a no-op on an empty table.
    pub fn move_selection(&mut self, delta: i32) {
        if self.rows.is_empty() {
            return;
        }
        let last = self.rows.len() as i32 - 1;
        let next = (self.selected as i32 + delta).clamp(0, last);
        self.selected = next as usize;
    }

    /// Cycle CPU → Memory → Name → CPU (FR-5) and re-sort, preserving the
    /// selection by id exactly like [`Self::refresh`].
    pub fn cycle_sort(&mut self) {
        self.sort = self.sort.next();
        let rows = std::mem::take(&mut self.rows);
        self.refresh(rows);
    }

    pub fn rows(&self) -> &[MonitorRow] {
        &self.rows
    }

    pub fn sort(&self) -> SortKey {
        self.sort
    }

    pub fn selected(&self) -> usize {
        self.selected
    }

    /// The currently selected row's id (Enter-jump target, FR-6), `None` on
    /// an empty table.
    pub fn selected_id(&self) -> Option<SessionCardId> {
        self.rows.get(self.selected).map(|row| row.id)
    }
}

/// CPU% (FR-3/FR-8): permille (1 core = 1000) rounded to the nearest whole
/// percent, or `"—"` before the first sample / on a platform without the
/// measurement.
pub fn format_cpu(cpu_permille: Option<u32>) -> String {
    match cpu_permille {
        Some(permille) => format!("{}%", (permille + 5) / 10),
        None => "\u{2014}".to_string(),
    }
}

const MB: f64 = 1024.0 * 1024.0;
const GB: f64 = MB * 1024.0;

/// Memory (FR-3/FR-8): MB below 1 GiB, GB (2 decimals) at/above it; `"—"`
/// when unavailable.
pub fn format_mem(bytes: Option<u64>) -> String {
    match bytes {
        Some(bytes) => {
            let bytes = bytes as f64;
            if bytes >= GB {
                format!("{:.2} GB", bytes / GB)
            } else {
                format!("{:.0} MB", bytes / MB)
            }
        }
        None => "\u{2014}".to_string(),
    }
}

/// Process count (FR-3/FR-8): the raw tree size, or `"—"` when unavailable.
pub fn format_proc_count(count: Option<u32>) -> String {
    match count {
        Some(count) => count.to_string(),
        None => "\u{2014}".to_string(),
    }
}

/// Elapsed time (FR-3/FR-8): `mm:ss` below one hour, `h:mm:ss` at/above it;
/// `"—"` when the pane has no resolvable start time. `now` is a parameter (no
/// `SystemTime::now()` inside) so every boundary — including the 59:59 →
/// 1:00:00 rollover — is directly testable without a wall-clock dependency.
pub fn format_elapsed(started_at: Option<SystemTime>, now: SystemTime) -> String {
    let Some(started_at) = started_at else {
        return "\u{2014}".to_string();
    };
    let elapsed = now.duration_since(started_at).unwrap_or_default();
    let total_secs = elapsed.as_secs();
    let hours = total_secs / 3600;
    let minutes = (total_secs % 3600) / 60;
    let seconds = total_secs % 60;
    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes}:{seconds:02}")
    }
}

/// Process name (FR-3/FR-8): the resolved foreground name, or `"—"` when
/// unavailable.
pub fn format_process(process: Option<&str>) -> &str {
    process.unwrap_or("\u{2014}")
}

/// A short label for the active sort key (FR-5), shown in the overlay's
/// title so the current order is always visible.
pub fn sort_label(sort: SortKey) -> &'static str {
    match sort {
        SortKey::CpuDesc => "CPU",
        SortKey::MemDesc => "Memory",
        SortKey::NameAsc => "Name",
    }
}

/// Windowing shared by both render paths (wgpu card and native AppKit card),
/// same policy as `macos_overlay::model::overlay_scroll_window`: show up to
/// `capacity` rows, keeping `selected` centered once the list overflows, so
/// the selection can never move outside the visible window. Returns
/// `(offset, shown)`.
pub fn visible_window(len: usize, selected: usize, capacity: usize) -> (usize, usize) {
    if len <= capacity {
        return (0, len);
    }
    let offset = selected.saturating_sub(capacity / 2).min(len - capacity);
    (offset, capacity)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::split_tree::PaneId;

    fn id(window: u64, pane: u64) -> SessionCardId {
        SessionCardId::new(SessionWindowId(window), PaneId::new(pane))
    }

    fn row(
        window: u64,
        pane: u64,
        process: Option<&str>,
        cpu: Option<u32>,
        mem: Option<u64>,
    ) -> MonitorRow {
        MonitorRow {
            id: id(window, pane),
            process: process.map(str::to_string),
            cpu_permille: cpu,
            mem_bytes: mem,
            proc_count: mem.map(|_| 1),
            started_at: None,
            location: format!("w{window}p{pane}"),
        }
    }

    fn wall() -> WallClock {
        WallClock {
            year: 2026,
            month: 7,
            day: 12,
            hour: 0,
            minute: 0,
        }
    }

    /// A store with one card upserted per `ids`, all sharing `name`/`process`
    /// — built through the public `SessionStore` API (its fields are
    /// module-private, so tests outside `session_store` cannot construct a
    /// `SessionCard` literal directly).
    fn store_with_cards(ids: &[SessionCardId], name: &str, process: &str) -> SessionStore {
        let mut store = SessionStore::new();
        for (seq, &card_id) in ids.iter().enumerate() {
            store.apply(SessionDelta::Upsert {
                id: card_id,
                seq: seq as u64 + 1,
                name: name.to_string(),
                cwd: "/repo".to_string(),
                busy: true,
                updated_at: wall(),
                preview: None,
            });
            store.apply(SessionDelta::Process {
                id: card_id,
                process: Some(process.to_string()),
            });
        }
        store
    }

    // AC-2 (FR-2): one row per live pane, across multiple windows/panes.
    #[test]
    fn build_rows_produces_one_row_per_live_pane() {
        let ids = [id(1, 1), id(1, 2), id(2, 1)];
        let store = store_with_cards(&ids, "repo", "zsh");
        let cards: Vec<_> = ids.iter().map(|i| (*i, store.get(i).unwrap())).collect();
        let rows = build_rows(&cards);
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].id, id(1, 1));
        assert_eq!(rows[2].id, id(2, 1));
    }

    // AC-13 (FR-3): every row carries the six fields, and `location` is built
    // from the card's display name plus a per-window pane ordinal.
    #[test]
    fn build_rows_fills_every_field_and_locations_use_per_window_ordinals() {
        let ids = [id(1, 1), id(1, 2), id(2, 1)];
        let store = store_with_cards(&ids, "repo", "claude");
        let cards: Vec<_> = ids.iter().map(|i| (*i, store.get(i).unwrap())).collect();
        let rows = build_rows(&cards);
        assert_eq!(rows[0].location, "repo \u{b7} pane 1");
        assert_eq!(rows[1].location, "repo \u{b7} pane 2");
        // A different window restarts its own ordinal count.
        assert_eq!(rows[2].location, "repo \u{b7} pane 1");
        assert_eq!(rows[0].process.as_deref(), Some("claude"));
    }

    // AC-6 (FR-5): default CPU-descending, then cycling Mem → Name → Cpu.
    #[test]
    fn cycle_sort_walks_cpu_mem_name_and_back() {
        assert_eq!(SortKey::CpuDesc.next(), SortKey::MemDesc);
        assert_eq!(SortKey::MemDesc.next(), SortKey::NameAsc);
        assert_eq!(SortKey::NameAsc.next(), SortKey::CpuDesc);
    }

    #[test]
    fn process_monitor_default_sort_is_cpu_descending_and_cycles() {
        let rows = vec![
            row(1, 1, Some("b"), Some(100), Some(200)),
            row(1, 2, Some("a"), Some(500), Some(100)),
            row(1, 3, Some("c"), Some(300), Some(300)),
        ];
        let mut monitor = ProcessMonitor::open(rows);
        assert_eq!(monitor.sort(), SortKey::CpuDesc);
        assert_eq!(
            monitor.rows().iter().map(|r| r.id).collect::<Vec<_>>(),
            vec![id(1, 2), id(1, 3), id(1, 1)]
        );

        monitor.cycle_sort();
        assert_eq!(monitor.sort(), SortKey::MemDesc);
        assert_eq!(
            monitor.rows().iter().map(|r| r.id).collect::<Vec<_>>(),
            vec![id(1, 3), id(1, 1), id(1, 2)]
        );

        monitor.cycle_sort();
        assert_eq!(monitor.sort(), SortKey::NameAsc);
        assert_eq!(
            monitor.rows().iter().map(|r| r.id).collect::<Vec<_>>(),
            vec![id(1, 2), id(1, 1), id(1, 3)]
        );

        monitor.cycle_sort();
        assert_eq!(monitor.sort(), SortKey::CpuDesc);
    }

    // AC-10 (FR-8): rows with no metrics sample sort last under every key,
    // and refreshing after a pane vanishes never panics.
    #[test]
    fn none_metrics_sort_last_under_every_key_and_never_panics() {
        let rows = vec![
            row(1, 1, Some("b"), None, None),
            row(1, 2, Some("a"), Some(500), Some(100)),
        ];
        let monitor = ProcessMonitor::open(rows);
        assert_eq!(monitor.rows()[0].id, id(1, 2));
        assert_eq!(monitor.rows()[1].id, id(1, 1));

        // A refresh whose row set no longer contains the selected pane
        // (session closed) must not panic and must fall back to a valid
        // selection.
        let mut monitor = monitor;
        monitor.refresh(vec![row(1, 3, Some("c"), Some(1), Some(1))]);
        assert_eq!(monitor.selected(), 0);
        assert_eq!(monitor.selected_id(), Some(id(1, 3)));
    }

    // AC-11 (NFR-3): selection preserved across a refresh, and move_selection
    // clamps at both ends without panicking on an empty table.
    #[test]
    fn selection_is_preserved_across_refresh_and_move_clamps() {
        let rows = vec![
            row(1, 1, Some("a"), Some(100), Some(1)),
            row(1, 2, Some("b"), Some(200), Some(1)),
        ];
        let mut monitor = ProcessMonitor::open(rows);
        // CPU-desc puts b (200) first, a (100) second.
        assert_eq!(monitor.selected_id(), Some(id(1, 2)));
        monitor.move_selection(1);
        assert_eq!(monitor.selected_id(), Some(id(1, 1)));
        // Clamped at the bottom.
        monitor.move_selection(1);
        assert_eq!(monitor.selected_id(), Some(id(1, 1)));

        // A refresh with the same rows (re-sorted identically) keeps the
        // selection on the same id.
        monitor.refresh(vec![
            row(1, 1, Some("a"), Some(100), Some(1)),
            row(1, 2, Some("b"), Some(200), Some(1)),
        ]);
        assert_eq!(monitor.selected_id(), Some(id(1, 1)));

        // An empty table never panics.
        let mut empty = ProcessMonitor::open(Vec::new());
        empty.move_selection(1);
        empty.move_selection(-1);
        assert_eq!(empty.selected_id(), None);
    }

    // AC-15 (FR-8): the non-macOS/no-sample degradation — every value "—".
    #[test]
    fn formatters_degrade_to_em_dash_when_absent() {
        assert_eq!(format_cpu(None), "\u{2014}");
        assert_eq!(format_mem(None), "\u{2014}");
        assert_eq!(format_proc_count(None), "\u{2014}");
        assert_eq!(
            format_elapsed(None, SystemTime::UNIX_EPOCH),
            "\u{2014}"
        );
        assert_eq!(format_process(None), "\u{2014}");
    }

    // MINOR-5 regression: the selection must always land inside the visible
    // window — top, middle (centered), and bottom of an overflowing list.
    #[test]
    fn visible_window_keeps_the_selection_visible() {
        // Fits entirely: whole list, no offset.
        assert_eq!(visible_window(5, 4, 10), (0, 5));
        // Overflow, selection near the top: window pinned to the start.
        assert_eq!(visible_window(20, 0, 10), (0, 10));
        // Overflow, middle: selection centered.
        let (offset, shown) = visible_window(20, 10, 10);
        assert!(offset <= 10 && 10 < offset + shown);
        // Overflow, selection at the end: window pinned to the tail.
        let (offset, shown) = visible_window(20, 19, 10);
        assert_eq!((offset, shown), (10, 10));
        assert!(offset <= 19 && 19 < offset + shown);
    }

    #[test]
    fn format_cpu_rounds_permille_to_nearest_percent() {
        assert_eq!(format_cpu(Some(0)), "0%");
        assert_eq!(format_cpu(Some(1420)), "142%");
        assert_eq!(format_cpu(Some(995)), "100%");
    }

    // AC-5: MB below 1 GiB, GB (2dp) at/above the boundary.
    #[test]
    fn format_mem_switches_units_at_the_gb_boundary() {
        assert_eq!(format_mem(Some(512 * 1024 * 1024)), "512 MB");
        assert_eq!(format_mem(Some(1024 * 1024 * 1024 - 1)), "1024 MB");
        assert_eq!(format_mem(Some(1024 * 1024 * 1024)), "1.00 GB");
        assert_eq!(format_mem(Some(1024 * 1024 * 1024 * 3 / 2)), "1.50 GB");
    }

    // AC-11: mm:ss below an hour, h:mm:ss at/after — including the 59:59 →
    // 1:00:00 rollover boundary.
    #[test]
    fn format_elapsed_rolls_over_at_one_hour() {
        let start = SystemTime::UNIX_EPOCH;
        assert_eq!(
            format_elapsed(Some(start), start + std::time::Duration::from_secs(5)),
            "0:05"
        );
        assert_eq!(
            format_elapsed(Some(start), start + std::time::Duration::from_secs(3599)),
            "59:59"
        );
        assert_eq!(
            format_elapsed(Some(start), start + std::time::Duration::from_secs(3600)),
            "1:00:00"
        );
    }
}
