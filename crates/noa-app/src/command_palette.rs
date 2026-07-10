//! Command palette (`cmd+shift+p`) — the GUI-agnostic half. Mirrors
//! `search_prompt.rs`: pure state + pure logic with no winit/window/GPU
//! types, so it is unit-testable without a display. `App` owns a
//! [`crate::app::CommandPaletteSession`] wrapping [`CommandPalette`] and its
//! `KeyboardInput` handler drives it, then feeds a
//! `noa_render::CommandPaletteSnapshot` (titles + resolved keybind hints)
//! into the renderer overlay.
//!
//! The palette exposes the existing `AppCommand` registry — the roadmap's
//! "action registry" — as a searchable modal. It adds no new command source:
//! [`command_palette_entries`] is a filtered projection of the same enum the
//! menu bar and keybinds already dispatch.

use crate::commands::{
    AppCommand, FontSizeAction, KeybindEngine, SearchAction, TerminalAction, ViewportScroll,
};
use crate::split_tree::Direction;

/// Human-readable title for **every** `AppCommand` variant.
///
/// The `match` is deliberately exhaustive with no `_` wildcard (NFR-4): a
/// new `AppCommand` variant fails to compile here until it is titled, so the
/// palette can never silently render a blank entry. The inner `usize` fall
/// through on `SelectTab` is not an `AppCommand` wildcard — every tab index
/// 1..=9 has its own registry row, and 1..9 are the only ones ever
/// constructed.
pub(crate) fn command_palette_title(command: AppCommand) -> &'static str {
    match command {
        AppCommand::About => "About Noa",
        AppCommand::Preferences => "Open Preferences",
        AppCommand::ReloadConfig => "Reload Configuration",
        AppCommand::Copy => "Copy to Clipboard",
        AppCommand::Paste => "Paste from Clipboard",
        AppCommand::SendSelectionToPane => "Send Selection to Pane",
        AppCommand::ExportScrollback => "Export Scrollback to File",
        AppCommand::PipeScrollbackToPager => "Pipe Scrollback to Pager",
        AppCommand::Terminal(TerminalAction::Clear) => "Clear Screen",
        AppCommand::Terminal(TerminalAction::ClearScrollback) => "Clear Scrollback",
        AppCommand::Terminal(TerminalAction::SelectAll) => "Select All",
        AppCommand::FontSize(FontSizeAction::Increase) => "Increase Font Size",
        AppCommand::FontSize(FontSizeAction::Decrease) => "Decrease Font Size",
        AppCommand::FontSize(FontSizeAction::Reset) => "Reset Font Size",
        AppCommand::Search(SearchAction::Find) => "Find\u{2026}",
        AppCommand::Search(SearchAction::FindNext) => "Find Next",
        AppCommand::Search(SearchAction::FindPrevious) => "Find Previous",
        AppCommand::Search(SearchAction::Clear) => "Clear Search",
        AppCommand::ScrollViewport(ViewportScroll::LineUp) => "Scroll Up One Line",
        AppCommand::ScrollViewport(ViewportScroll::LineDown) => "Scroll Down One Line",
        AppCommand::ScrollViewport(ViewportScroll::PageUp) => "Scroll Up One Page",
        AppCommand::ScrollViewport(ViewportScroll::PageDown) => "Scroll Down One Page",
        AppCommand::ScrollViewport(ViewportScroll::Top) => "Scroll to Top",
        AppCommand::ScrollViewport(ViewportScroll::Bottom) => "Scroll to Bottom",
        AppCommand::ScrollViewport(ViewportScroll::PrevPrompt) => "Jump to Previous Prompt",
        AppCommand::ScrollViewport(ViewportScroll::NextPrompt) => "Jump to Next Prompt",
        AppCommand::NewTab => "New Tab",
        AppCommand::NewWindow => "New Window",
        AppCommand::NewSplitLeft => "Add Pane Left",
        AppCommand::NewSplitRight => "Add Pane Right",
        AppCommand::NewSplitUp => "Add Pane Up",
        AppCommand::NewSplitDown => "Add Pane Down",
        AppCommand::FocusDirection(Direction::Left) => "Focus Split Left",
        AppCommand::FocusDirection(Direction::Right) => "Focus Split Right",
        AppCommand::FocusDirection(Direction::Up) => "Focus Split Up",
        AppCommand::FocusDirection(Direction::Down) => "Focus Split Down",
        AppCommand::ResizeSplit(Direction::Left) => "Resize Split Left",
        AppCommand::ResizeSplit(Direction::Right) => "Resize Split Right",
        AppCommand::ResizeSplit(Direction::Up) => "Resize Split Up",
        AppCommand::ResizeSplit(Direction::Down) => "Resize Split Down",
        AppCommand::EqualizeSplits => "Equalize Splits",
        AppCommand::ToggleSplitZoom => "Toggle Split Zoom",
        AppCommand::ToggleTabOverview => "Toggle Session Overview",
        AppCommand::ToggleFullscreen => "Toggle Full Screen",
        AppCommand::CloseTab => "Close Tab",
        AppCommand::SelectTab(index) => match index {
            1 => "Go to Tab 1",
            2 => "Go to Tab 2",
            3 => "Go to Tab 3",
            4 => "Go to Tab 4",
            5 => "Go to Tab 5",
            6 => "Go to Tab 6",
            7 => "Go to Tab 7",
            8 => "Go to Tab 8",
            9 => "Go to Tab 9",
            _ => "Go to Tab",
        },
        AppCommand::NextTab => "Next Tab",
        AppCommand::PrevTab => "Previous Tab",
        AppCommand::SetTabTitle => "Set Tab Title\u{2026}",
        AppCommand::CloseWindow => "Close Window",
        AppCommand::Quit => "Quit Noa",
        AppCommand::ToggleCommandPalette => "Toggle Command Palette",
        AppCommand::ToggleQuickTerminal => "Toggle Quick Terminal",
        AppCommand::ToggleSecureKeyboardEntry => "Toggle Secure Keyboard Entry",
        AppCommand::ToggleSidebar => "Toggle Sidebar",
        AppCommand::ToggleAutoApprove => "Toggle Auto Approve",
        // Deviation: the locked spec's literal label is Japanese
        // ("テーマ・設定を開く"), but every existing palette title is English
        // (this whole `match`) — following that established convention
        // instead of the spec's literal text.
        AppCommand::OpenThemeSettings => "Open Theme & Settings\u{2026}",
    }
}

/// The commands the palette lists, in registry declaration order (also the
/// tie-break order for subsequence matches — v1 has no scoring).
///
/// Two variants are excluded (R-3): `ToggleCommandPalette` (self-reference,
/// like the overview's self-exclusion) and `SelectTab(1..9)` (nine redundant
/// rows already reachable via `cmd+1..9` — kept in the title registry for
/// completeness, cut only from display).
pub(crate) fn command_palette_entries() -> &'static [AppCommand] {
    const ENTRIES: &[AppCommand] = &[
        AppCommand::About,
        AppCommand::Preferences,
        AppCommand::OpenThemeSettings,
        AppCommand::ReloadConfig,
        AppCommand::Copy,
        AppCommand::Paste,
        AppCommand::SendSelectionToPane,
        AppCommand::ExportScrollback,
        AppCommand::PipeScrollbackToPager,
        AppCommand::Terminal(TerminalAction::Clear),
        AppCommand::Terminal(TerminalAction::ClearScrollback),
        AppCommand::Terminal(TerminalAction::SelectAll),
        AppCommand::FontSize(FontSizeAction::Increase),
        AppCommand::FontSize(FontSizeAction::Decrease),
        AppCommand::FontSize(FontSizeAction::Reset),
        AppCommand::Search(SearchAction::Find),
        AppCommand::Search(SearchAction::FindNext),
        AppCommand::Search(SearchAction::FindPrevious),
        AppCommand::Search(SearchAction::Clear),
        AppCommand::ScrollViewport(ViewportScroll::LineUp),
        AppCommand::ScrollViewport(ViewportScroll::LineDown),
        AppCommand::ScrollViewport(ViewportScroll::PageUp),
        AppCommand::ScrollViewport(ViewportScroll::PageDown),
        AppCommand::ScrollViewport(ViewportScroll::Top),
        AppCommand::ScrollViewport(ViewportScroll::Bottom),
        AppCommand::ScrollViewport(ViewportScroll::PrevPrompt),
        AppCommand::ScrollViewport(ViewportScroll::NextPrompt),
        AppCommand::NewTab,
        AppCommand::NewWindow,
        AppCommand::NewSplitLeft,
        AppCommand::NewSplitRight,
        AppCommand::NewSplitUp,
        AppCommand::NewSplitDown,
        AppCommand::FocusDirection(Direction::Left),
        AppCommand::FocusDirection(Direction::Right),
        AppCommand::FocusDirection(Direction::Up),
        AppCommand::FocusDirection(Direction::Down),
        AppCommand::ResizeSplit(Direction::Left),
        AppCommand::ResizeSplit(Direction::Right),
        AppCommand::ResizeSplit(Direction::Up),
        AppCommand::ResizeSplit(Direction::Down),
        AppCommand::EqualizeSplits,
        AppCommand::ToggleSplitZoom,
        AppCommand::ToggleTabOverview,
        AppCommand::ToggleFullscreen,
        AppCommand::ToggleQuickTerminal,
        AppCommand::ToggleSecureKeyboardEntry,
        AppCommand::ToggleSidebar,
        AppCommand::ToggleAutoApprove,
        AppCommand::SetTabTitle,
        AppCommand::CloseTab,
        AppCommand::NextTab,
        AppCommand::PrevTab,
        AppCommand::CloseWindow,
        AppCommand::Quit,
    ];
    ENTRIES
}

/// One scored fuzzy match of the query against an entry's title: the matched
/// command, its rank `score` (higher sorts first), and the char indices in the
/// title that the query matched (`positions`, used to highlight them, C).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PaletteMatch {
    pub command: AppCommand,
    pub score: i32,
    pub positions: Vec<usize>,
}

/// Rank every entry whose title matches `query` as a case-insensitive
/// subsequence, best-first (B). An empty query keeps every entry in
/// declaration order (score 0, no highlight). Ties keep declaration order via
/// a *stable* sort (R-7), so e.g. `Find\u{2026}` stays above `Find Next`.
/// O(N) over the registry — one pass, hand-written scoring (NFR-1).
pub(crate) fn command_palette_matches(query: &str) -> Vec<PaletteMatch> {
    let mut matches: Vec<PaletteMatch> = command_palette_entries()
        .iter()
        .copied()
        .filter_map(|command| {
            fuzzy_match(query, command_palette_title(command)).map(|(score, positions)| {
                PaletteMatch {
                    command,
                    score,
                    positions,
                }
            })
        })
        .collect();
    // Stable: equal scores preserve the registry declaration order they were
    // collected in.
    matches.sort_by_key(|b| std::cmp::Reverse(b.score));
    matches
}

/// Score `query` against `title` as a case-insensitive subsequence, returning
/// `(score, matched-char-positions)` or `None` when `query` is not a
/// subsequence of `title`. An empty query yields `(0, [])`. Ranking favors, in
/// descending weight: a prefix match, matches at word starts (after a space),
/// and contiguous runs — hand-rolled to avoid a `fuzzy-matcher`/`nucleo`
/// dependency (NFR-1). Greedy leftmost matching keeps it a single O(len) pass.
pub(crate) fn fuzzy_match(query: &str, title: &str) -> Option<(i32, Vec<usize>)> {
    let title_chars: Vec<char> = title.chars().collect();
    let mut positions = Vec::new();
    let mut ti = 0usize;
    for q in query.chars() {
        loop {
            let t = *title_chars.get(ti)?;
            ti += 1;
            if q.eq_ignore_ascii_case(&t) {
                positions.push(ti - 1);
                break;
            }
        }
    }
    if positions.is_empty() {
        // Empty query: every entry matches with no highlight, no ranking.
        return Some((0, positions));
    }

    let mut score = 0i32;
    for (k, &pos) in positions.iter().enumerate() {
        score += 1; // base weight per matched char
        let word_start = pos == 0 || title_chars[pos - 1] == ' ';
        if word_start {
            score += 8;
        }
        if k > 0 && positions[k - 1] + 1 == pos {
            score += 4; // contiguous with the previous match
        }
    }
    if positions[0] == 0 {
        score += 16; // whole query anchored at the title's front
    }
    score -= positions[0] as i32; // prefer an earlier first match
    Some((score, positions))
}

/// The keybind hint shown for `command`, reverse-looked-up from the current
/// bindings (R-4). `None` when the command has no binding. The engine is the
/// single source of truth — no second keybind table.
pub(crate) fn command_palette_keybind(
    keybinds: &KeybindEngine,
    command: AppCommand,
) -> Option<String> {
    keybinds.chord_for(command)
}

/// The section a command belongs to (F). Only used to group the empty-query
/// view under muted headings; a non-empty query renders a flat ranked list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CommandCategory {
    Application,
    Clipboard,
    View,
    Search,
    Scroll,
    Splits,
    Tabs,
    Window,
    Toggles,
}

impl CommandCategory {
    /// The heading text shown above the category's entries.
    pub(crate) fn label(self) -> &'static str {
        match self {
            CommandCategory::Application => "Application",
            CommandCategory::Clipboard => "Clipboard",
            CommandCategory::View => "View",
            CommandCategory::Search => "Search",
            CommandCategory::Scroll => "Scroll",
            CommandCategory::Splits => "Splits",
            CommandCategory::Tabs => "Tabs",
            CommandCategory::Window => "Window",
            CommandCategory::Toggles => "Toggles",
        }
    }

    /// Category display order for the grouped (empty-query) view.
    pub(crate) const ORDER: [CommandCategory; 9] = [
        CommandCategory::Application,
        CommandCategory::Clipboard,
        CommandCategory::View,
        CommandCategory::Search,
        CommandCategory::Scroll,
        CommandCategory::Splits,
        CommandCategory::Tabs,
        CommandCategory::Window,
        CommandCategory::Toggles,
    ];
}

/// The category a command groups under (F). Exhaustive with no `_` arm (like
/// [`command_palette_title`], NFR-4): a new `AppCommand` variant fails to
/// compile here until it is filed under a section.
pub(crate) fn command_category(command: AppCommand) -> CommandCategory {
    match command {
        AppCommand::About
        | AppCommand::Preferences
        | AppCommand::OpenThemeSettings
        | AppCommand::ReloadConfig
        | AppCommand::Quit => CommandCategory::Application,
        AppCommand::Copy
        | AppCommand::Paste
        | AppCommand::SendSelectionToPane
        | AppCommand::Terminal(TerminalAction::SelectAll) => CommandCategory::Clipboard,
        AppCommand::ExportScrollback | AppCommand::PipeScrollbackToPager => CommandCategory::Scroll,
        AppCommand::Terminal(TerminalAction::Clear)
        | AppCommand::Terminal(TerminalAction::ClearScrollback)
        | AppCommand::FontSize(_)
        | AppCommand::ToggleFullscreen => CommandCategory::View,
        AppCommand::Search(_) => CommandCategory::Search,
        AppCommand::ScrollViewport(_) => CommandCategory::Scroll,
        AppCommand::NewSplitLeft
        | AppCommand::NewSplitRight
        | AppCommand::NewSplitUp
        | AppCommand::NewSplitDown
        | AppCommand::FocusDirection(_)
        | AppCommand::ResizeSplit(_)
        | AppCommand::EqualizeSplits
        | AppCommand::ToggleSplitZoom => CommandCategory::Splits,
        AppCommand::NewTab
        | AppCommand::SetTabTitle
        | AppCommand::CloseTab
        | AppCommand::NextTab
        | AppCommand::PrevTab
        | AppCommand::SelectTab(_)
        | AppCommand::ToggleTabOverview => CommandCategory::Tabs,
        AppCommand::NewWindow | AppCommand::CloseWindow => CommandCategory::Window,
        AppCommand::ToggleCommandPalette
        | AppCommand::ToggleQuickTerminal
        | AppCommand::ToggleSecureKeyboardEntry
        | AppCommand::ToggleSidebar
        | AppCommand::ToggleAutoApprove => CommandCategory::Toggles,
    }
}

/// Render a config-format chord (e.g. `"cmd+shift+]"`) as macOS-style key
/// symbols (e.g. `"\u{21e7}\u{2318}]"`, ⇧⌘]) for display only (E). Modifiers
/// are emitted in the macOS-standard order ⌃⌥⇧⌘ regardless of their order in
/// the chord, with no separators, followed by the key. Purely a display
/// transform — the config/parse round-trip (`KeyTrigger::Display`) is
/// untouched.
pub(crate) fn keybind_symbols(chord: &str) -> String {
    let mut ctrl = false;
    let mut alt = false;
    let mut shift = false;
    let mut cmd = false;
    let mut key = String::new();
    for token in chord.split('+').map(str::trim).filter(|t| !t.is_empty()) {
        match token.to_ascii_lowercase().as_str() {
            "cmd" | "command" | "super" | "meta" => cmd = true,
            "ctrl" | "control" => ctrl = true,
            "alt" | "option" => alt = true,
            "shift" => shift = true,
            other => key = keybind_key_symbol(other),
        }
    }
    let mut out = String::new();
    if ctrl {
        out.push('\u{2303}'); // ⌃
    }
    if alt {
        out.push('\u{2325}'); // ⌥
    }
    if shift {
        out.push('\u{21e7}'); // ⇧
    }
    if cmd {
        out.push('\u{2318}'); // ⌘
    }
    out.push_str(&key);
    out
}

/// The display glyph/label for a chord's key token (E). A single character is
/// upper-cased; named tokens map to their macOS glyph or short label.
fn keybind_key_symbol(token: &str) -> String {
    match token {
        "arrowup" | "up" => "\u{2191}".to_string(),       // ↑
        "arrowdown" | "down" => "\u{2193}".to_string(),   // ↓
        "arrowleft" | "left" => "\u{2190}".to_string(),   // ←
        "arrowright" | "right" => "\u{2192}".to_string(), // →
        "pageup" => "PgUp".to_string(),
        "pagedown" => "PgDn".to_string(),
        "home" => "Home".to_string(),
        "end" => "End".to_string(),
        "enter" | "return" => "\u{23ce}".to_string(), // ⏎
        "plus" => "+".to_string(),
        other => {
            let mut chars = other.chars();
            match (chars.next(), chars.next()) {
                (Some(ch), None) => ch.to_ascii_uppercase().to_string(),
                _ => other.to_string(),
            }
        }
    }
}

/// One row of the palette's display list (F): either a non-selectable category
/// heading (empty-query grouped view) or a selectable command entry carrying
/// the char positions in its title that the current query matched (C).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum PaletteItem {
    Header(CommandCategory),
    Entry {
        command: AppCommand,
        positions: Vec<usize>,
    },
}

impl PaletteItem {
    fn is_entry(&self) -> bool {
        matches!(self, PaletteItem::Entry { .. })
    }
}

/// The pure editable state behind an open palette: the query buffer, the
/// current display list (`items`: headers + ranked entries), and the
/// highlighted row. Holds no window/pane binding of its own (that lives in the
/// `App`-side session), so its editing/navigation logic is unit-testable
/// without a display — mirroring [`crate::search_prompt::SearchPrompt`].
pub(crate) struct CommandPalette {
    query: String,
    items: Vec<PaletteItem>,
    /// Index into `items`, always pointing at an [`PaletteItem::Entry`] (never
    /// a header) while any entry exists.
    selected: usize,
}

impl CommandPalette {
    /// Open with an empty query, every entry shown grouped by category, and the
    /// first entry highlighted.
    pub(crate) fn open() -> Self {
        let mut palette = CommandPalette {
            query: String::new(),
            items: Vec::new(),
            selected: 0,
        };
        palette.refilter();
        palette
    }

    pub(crate) fn query(&self) -> &str {
        &self.query
    }

    /// The display list (headers + entries) for the current query.
    pub(crate) fn items(&self) -> &[PaletteItem] {
        &self.items
    }

    /// The highlighted row's index into [`Self::items`].
    pub(crate) fn selected(&self) -> usize {
        self.selected
    }

    /// The number of selectable entries (excludes headers) — backs the palette's
    /// `shown/total` counter (A).
    pub(crate) fn entry_count(&self) -> usize {
        self.items.iter().filter(|item| item.is_entry()).count()
    }

    /// The highlighted command, or `None` when no entry is highlighted (an
    /// empty result set), so Enter is a no-op rather than an invalid index.
    pub(crate) fn selected_command(&self) -> Option<AppCommand> {
        match self.items.get(self.selected) {
            Some(PaletteItem::Entry { command, .. }) => Some(*command),
            _ => None,
        }
    }

    /// Append a keypress's resolved text (control characters dropped, as in
    /// the search prompt), then re-filter and reset the highlight to the top
    /// (R-7).
    pub(crate) fn push_text(&mut self, text: &str) {
        let filtered: String = text.chars().filter(|c| !c.is_control()).collect();
        if filtered.is_empty() {
            return;
        }
        self.query.push_str(&filtered);
        self.refilter();
    }

    /// Pop one character (Backspace), re-filter, reset the highlight (R-7).
    pub(crate) fn backspace(&mut self) {
        self.query.pop();
        self.refilter();
    }

    /// Move the highlight to the previous entry, skipping headers, clamped at
    /// the first entry (no wrap, R-8).
    pub(crate) fn move_up(&mut self) {
        if let Some(prev) = self.items[..self.selected.min(self.items.len())]
            .iter()
            .rposition(PaletteItem::is_entry)
        {
            self.selected = prev;
        }
    }

    /// Move the highlight to the next entry, skipping headers, clamped at the
    /// last entry (no wrap, R-8). A no-op on an empty list.
    pub(crate) fn move_down(&mut self) {
        let from = self.selected + 1;
        if let Some(rel) = self
            .items
            .get(from..)
            .and_then(|rest| rest.iter().position(PaletteItem::is_entry))
        {
            self.selected = from + rel;
        }
    }

    /// Rebuild `items` from the query and point `selected` at the first entry.
    /// An empty query groups every entry under category headings (F); a
    /// non-empty query renders a flat, ranked, header-less list (B).
    fn refilter(&mut self) {
        self.items = if self.query.is_empty() {
            build_grouped_items()
        } else {
            command_palette_matches(&self.query)
                .into_iter()
                .map(|m| PaletteItem::Entry {
                    command: m.command,
                    positions: m.positions,
                })
                .collect()
        };
        self.selected = self
            .items
            .iter()
            .position(PaletteItem::is_entry)
            .unwrap_or(0);
    }
}

/// Build the grouped empty-query display list: each non-empty category's
/// heading (in [`CommandCategory::ORDER`]) followed by its entries in registry
/// declaration order (F).
fn build_grouped_items() -> Vec<PaletteItem> {
    let entries = command_palette_entries();
    let mut items = Vec::new();
    for category in CommandCategory::ORDER {
        let mut pushed_header = false;
        for &command in entries {
            if command_category(command) != category {
                continue;
            }
            if !pushed_header {
                items.push(PaletteItem::Header(category));
                pushed_header = true;
            }
            items.push(PaletteItem::Entry {
                command,
                positions: Vec::new(),
            });
        }
    }
    items
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every `AppCommand` variant, including the display-excluded ones, so
    /// the title-registry completeness assertion (AC-3) covers the whole
    /// enum — not just what the palette lists.
    fn all_commands() -> Vec<AppCommand> {
        let mut commands = vec![
            AppCommand::About,
            AppCommand::Preferences,
            AppCommand::OpenThemeSettings,
            AppCommand::ReloadConfig,
            AppCommand::Copy,
            AppCommand::Paste,
            AppCommand::SendSelectionToPane,
            AppCommand::ExportScrollback,
            AppCommand::PipeScrollbackToPager,
            AppCommand::Terminal(TerminalAction::Clear),
            AppCommand::Terminal(TerminalAction::ClearScrollback),
            AppCommand::Terminal(TerminalAction::SelectAll),
            AppCommand::FontSize(FontSizeAction::Increase),
            AppCommand::FontSize(FontSizeAction::Decrease),
            AppCommand::FontSize(FontSizeAction::Reset),
            AppCommand::Search(SearchAction::Find),
            AppCommand::Search(SearchAction::FindNext),
            AppCommand::Search(SearchAction::FindPrevious),
            AppCommand::Search(SearchAction::Clear),
            AppCommand::ScrollViewport(ViewportScroll::LineUp),
            AppCommand::ScrollViewport(ViewportScroll::LineDown),
            AppCommand::ScrollViewport(ViewportScroll::PageUp),
            AppCommand::ScrollViewport(ViewportScroll::PageDown),
            AppCommand::ScrollViewport(ViewportScroll::Top),
            AppCommand::ScrollViewport(ViewportScroll::Bottom),
            AppCommand::ScrollViewport(ViewportScroll::PrevPrompt),
            AppCommand::ScrollViewport(ViewportScroll::NextPrompt),
            AppCommand::NewTab,
            AppCommand::NewWindow,
            AppCommand::NewSplitLeft,
            AppCommand::NewSplitRight,
            AppCommand::NewSplitUp,
            AppCommand::NewSplitDown,
            AppCommand::FocusDirection(Direction::Left),
            AppCommand::FocusDirection(Direction::Right),
            AppCommand::FocusDirection(Direction::Up),
            AppCommand::FocusDirection(Direction::Down),
            AppCommand::ResizeSplit(Direction::Left),
            AppCommand::ResizeSplit(Direction::Right),
            AppCommand::ResizeSplit(Direction::Up),
            AppCommand::ResizeSplit(Direction::Down),
            AppCommand::EqualizeSplits,
            AppCommand::ToggleSplitZoom,
            AppCommand::ToggleTabOverview,
            AppCommand::ToggleFullscreen,
            AppCommand::CloseTab,
            AppCommand::NextTab,
            AppCommand::PrevTab,
            AppCommand::CloseWindow,
            AppCommand::Quit,
            AppCommand::ToggleCommandPalette,
            AppCommand::ToggleQuickTerminal,
            AppCommand::ToggleSecureKeyboardEntry,
            AppCommand::ToggleSidebar,
            AppCommand::ToggleAutoApprove,
        ];
        commands.extend((1..=9).map(AppCommand::SelectTab));
        commands
    }

    #[test]
    fn every_command_has_a_non_empty_title() {
        // AC-3 / NFR-4: the registry is exhaustive over the enum. (The `match`
        // in `command_palette_title` has no `_` AppCommand arm, so a new
        // variant would fail to compile before this test could even run.)
        for command in all_commands() {
            assert!(
                !command_palette_title(command).is_empty(),
                "missing title for {command:?}"
            );
        }
    }

    #[test]
    fn entries_exclude_self_and_select_tab_but_keep_everything_else() {
        // AC-4.
        let entries = command_palette_entries();
        assert!(!entries.contains(&AppCommand::ToggleCommandPalette));
        for index in 1..=9 {
            assert!(!entries.contains(&AppCommand::SelectTab(index)));
        }
        for command in all_commands() {
            let excluded = matches!(
                command,
                AppCommand::ToggleCommandPalette | AppCommand::SelectTab(_)
            );
            assert_eq!(
                entries.contains(&command),
                !excluded,
                "unexpected membership for {command:?}"
            );
        }
    }

    #[test]
    fn entries_follow_registry_declaration_order() {
        // AC-4: order matches the registry (About first, Quit last here).
        let entries = command_palette_entries();
        assert_eq!(entries.first(), Some(&AppCommand::About));
        assert_eq!(entries.last(), Some(&AppCommand::Quit));
        let new_tab = entries.iter().position(|c| *c == AppCommand::NewTab);
        let split_left = entries.iter().position(|c| *c == AppCommand::NewSplitLeft);
        let split_right = entries.iter().position(|c| *c == AppCommand::NewSplitRight);
        assert!(new_tab < split_left, "NewTab precedes NewSplitLeft");
        assert!(
            split_left < split_right,
            "Add Pane Left precedes Add Pane Right"
        );
    }

    #[test]
    fn matches_are_case_insensitive_non_contiguous_subsequences() {
        let commands: Vec<AppCommand> = command_palette_matches("add")
            .into_iter()
            .map(|m| m.command)
            .collect();
        // "add" keeps the pane-addition commands in declaration order.
        assert!(commands.contains(&AppCommand::NewSplitLeft));
        assert!(commands.contains(&AppCommand::NewSplitRight));
        assert!(commands.contains(&AppCommand::NewSplitUp));
        assert!(commands.contains(&AppCommand::NewSplitDown));
        assert!(!commands.contains(&AppCommand::Copy));

        // Equal-scoring prefix matches keep registry order across pane-addition commands.
        let left = commands.iter().position(|c| *c == AppCommand::NewSplitLeft);
        let right = commands
            .iter()
            .position(|c| *c == AppCommand::NewSplitRight);
        let up = commands.iter().position(|c| *c == AppCommand::NewSplitUp);
        let down = commands.iter().position(|c| *c == AppCommand::NewSplitDown);
        assert!(left < right);
        assert!(right < up);
        assert!(up < down);

        // Case-insensitive: "QUIT" matches "Quit Noa".
        let quit: Vec<AppCommand> = command_palette_matches("QUIT")
            .into_iter()
            .map(|m| m.command)
            .collect();
        assert!(quit.contains(&AppCommand::Quit));
        assert!(!quit.contains(&AppCommand::Copy));
    }

    #[test]
    fn typing_refilters_and_resets_the_highlight() {
        // AC-8 / AC-9.
        let mut palette = CommandPalette::open();
        // Empty query is the grouped view: every entry is shown plus one
        // heading per non-empty category.
        assert_eq!(palette.entry_count(), command_palette_entries().len());
        // The highlight lands on the first *entry*, which sits just after the
        // first category heading (never on a header).
        assert!(matches!(
            palette.items()[palette.selected()],
            PaletteItem::Entry { .. }
        ));

        palette.push_text("new");
        assert_eq!(palette.query(), "new");
        assert!(palette.selected_command().is_some());
        assert!(
            palette
                .items()
                .iter()
                .all(|item| matches!(item, PaletteItem::Entry { .. })),
            "a non-empty query renders a flat, header-less list"
        );

        palette.backspace();
        assert_eq!(palette.query(), "ne");
    }

    #[test]
    fn navigation_clamps_without_wrapping() {
        // AC-10: drive a small, known three-entry list.
        let mut palette = CommandPalette::open();
        palette.query = "x".to_string();
        palette.items = vec![
            PaletteItem::Entry {
                command: AppCommand::NewTab,
                positions: Vec::new(),
            },
            PaletteItem::Entry {
                command: AppCommand::NewSplitRight,
                positions: Vec::new(),
            },
            PaletteItem::Entry {
                command: AppCommand::NewSplitDown,
                positions: Vec::new(),
            },
        ];
        palette.selected = 0;
        palette.move_down();
        palette.move_down();
        palette.move_down();
        assert_eq!(palette.selected(), 2, "clamps at the last row, no wrap");
        palette.move_up();
        palette.move_up();
        palette.move_up();
        assert_eq!(palette.selected(), 0, "clamps at the top, no wrap");
    }

    #[test]
    fn navigation_skips_category_headers() {
        // F: up/down land only on entries, never on the muted headings.
        let mut palette = CommandPalette::open();
        assert!(palette.query().is_empty(), "grouped view under test");
        // Walk the whole list top-to-bottom; the highlight must always sit on
        // an entry after each move.
        for _ in 0..palette.items().len() {
            assert!(
                matches!(
                    palette.items()[palette.selected()],
                    PaletteItem::Entry { .. }
                ),
                "selection never rests on a header"
            );
            palette.move_down();
        }
        // And back up.
        for _ in 0..palette.items().len() {
            assert!(matches!(
                palette.items()[palette.selected()],
                PaletteItem::Entry { .. }
            ));
            palette.move_up();
        }
    }

    #[test]
    fn find_ranks_the_exact_command_above_its_neighbors() {
        // B: typing "find" puts `Find…` above `Find Next`.
        let matches = command_palette_matches("find");
        let find = matches
            .iter()
            .position(|m| m.command == AppCommand::Search(SearchAction::Find));
        let find_next = matches
            .iter()
            .position(|m| m.command == AppCommand::Search(SearchAction::FindNext));
        assert!(find < find_next, "Find… ranks above Find Next");
        assert_eq!(
            matches.first().map(|m| m.command),
            find.map(|_| AppCommand::Search(SearchAction::Find))
        );
    }

    #[test]
    fn fuzzy_match_reports_positions_and_prefix_beats_scatter() {
        // B/C: a prefix, contiguous match outranks a scattered subsequence, and
        // the reported positions are the matched char indices.
        let (prefix_score, positions) = fuzzy_match("add", "Add Pane Right").unwrap();
        assert_eq!(positions, vec![0, 1, 2]);
        let (scatter_score, _) = fuzzy_match("split", "Toggle Split Zoom").unwrap();
        assert!(prefix_score > scatter_score);
        assert!(fuzzy_match("zzz", "Add Pane Right").is_none());
        // Empty query: matches with no highlight.
        assert_eq!(fuzzy_match("", "anything"), Some((0, vec![])));
    }

    #[test]
    fn category_assignment_is_stable_for_representative_commands() {
        // F: a handful of representative bindings land in the expected section.
        assert_eq!(
            command_category(AppCommand::About),
            CommandCategory::Application
        );
        assert_eq!(
            command_category(AppCommand::Copy),
            CommandCategory::Clipboard
        );
        assert_eq!(
            command_category(AppCommand::FontSize(FontSizeAction::Increase)),
            CommandCategory::View
        );
        assert_eq!(
            command_category(AppCommand::NewSplitRight),
            CommandCategory::Splits
        );
        assert_eq!(command_category(AppCommand::NewTab), CommandCategory::Tabs);
        assert_eq!(
            command_category(AppCommand::NewWindow),
            CommandCategory::Window
        );
        // Grouped view: headers appear in ORDER and precede their entries.
        let items = build_grouped_items();
        assert!(matches!(
            items.first(),
            Some(PaletteItem::Header(CommandCategory::Application))
        ));
    }

    #[test]
    fn keybind_symbols_render_macos_glyphs_in_canonical_order() {
        // E: modifiers reorder to ⌃⌥⇧⌘ and keys map to their symbols.
        assert_eq!(keybind_symbols("cmd+shift+]"), "\u{21e7}\u{2318}]");
        assert_eq!(keybind_symbols("cmd+c"), "\u{2318}C");
        assert_eq!(keybind_symbols("cmd+plus"), "\u{2318}+");
        assert_eq!(keybind_symbols("cmd+arrowup"), "\u{2318}\u{2191}");
        assert_eq!(
            keybind_symbols("shift+cmd+ctrl+alt+k"),
            "\u{2303}\u{2325}\u{21e7}\u{2318}K"
        );
        assert_eq!(keybind_symbols("pageup"), "PgUp");
        assert_eq!(keybind_symbols("cmd+enter"), "\u{2318}\u{23ce}");
    }

    #[test]
    fn enter_on_an_empty_result_set_has_no_command() {
        // AC-13: no valid index, so no dispatch and no panic.
        let mut palette = CommandPalette::open();
        palette.push_text("zzzzzz");
        assert_eq!(palette.entry_count(), 0);
        assert_eq!(palette.selected_command(), None);
    }

    #[test]
    fn keybind_hints_reverse_lookup_from_the_engine() {
        // AC-5.
        let engine = KeybindEngine::default();
        assert_eq!(
            command_palette_keybind(&engine, AppCommand::Copy).as_deref(),
            Some("cmd+c")
        );
        assert_eq!(
            command_palette_keybind(&engine, AppCommand::NewTab).as_deref(),
            Some("cmd+t")
        );
        assert_eq!(
            command_palette_keybind(&engine, AppCommand::Search(SearchAction::Find)).as_deref(),
            Some("cmd+f")
        );
        assert_eq!(
            command_palette_keybind(&engine, AppCommand::Quit).as_deref(),
            Some("cmd+q")
        );
        assert_eq!(
            command_palette_keybind(
                &engine,
                AppCommand::Terminal(TerminalAction::ClearScrollback)
            ),
            None
        );
    }
}
