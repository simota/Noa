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
        AppCommand::About => "About noa",
        AppCommand::Preferences => "Open Preferences",
        AppCommand::Copy => "Copy to Clipboard",
        AppCommand::Paste => "Paste from Clipboard",
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
        AppCommand::NewSplitRight => "Split Right",
        AppCommand::NewSplitDown => "Split Down",
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
        AppCommand::ToggleTabOverview => "Toggle Tab Overview",
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
        AppCommand::CloseWindow => "Close Window",
        AppCommand::Quit => "Quit noa",
        AppCommand::ToggleCommandPalette => "Toggle Command Palette",
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
        AppCommand::Copy,
        AppCommand::Paste,
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
        AppCommand::NewSplitRight,
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
        AppCommand::CloseTab,
        AppCommand::NextTab,
        AppCommand::PrevTab,
        AppCommand::CloseWindow,
        AppCommand::Quit,
    ];
    ENTRIES
}

/// The palette entries whose title matches `query` as a case-insensitive
/// subsequence, in declaration order (R-7). An empty query matches every
/// entry. O(N) over the registry — one pass, hand-written match (NFR-1).
pub(crate) fn command_palette_filter(query: &str) -> Vec<AppCommand> {
    command_palette_entries()
        .iter()
        .copied()
        .filter(|&command| is_subsequence_ci(query, command_palette_title(command)))
        .collect()
}

/// Whether `needle` appears in `haystack` as a subsequence (its characters in
/// order, not necessarily contiguous), comparing ASCII case-insensitively.
/// An empty needle is a subsequence of anything. Hand-written to avoid a
/// `fuzzy-matcher`/`nucleo` dependency (NFR-1).
pub(crate) fn is_subsequence_ci(needle: &str, haystack: &str) -> bool {
    let mut haystack_chars = haystack.chars();
    'needle: for n in needle.chars() {
        for h in haystack_chars.by_ref() {
            if n.eq_ignore_ascii_case(&h) {
                continue 'needle;
            }
        }
        return false;
    }
    true
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

/// The pure editable state behind an open palette: the query buffer, the
/// current filtered result set, and the highlighted index. Holds no
/// window/pane binding of its own (that lives in the `App`-side session), so
/// its editing/navigation logic is unit-testable without a display —
/// mirroring [`crate::search_prompt::SearchPrompt`].
pub(crate) struct CommandPalette {
    query: String,
    filtered: Vec<AppCommand>,
    selected: usize,
}

impl CommandPalette {
    /// Open with an empty query, every entry shown, first row highlighted.
    pub(crate) fn open() -> Self {
        CommandPalette {
            query: String::new(),
            filtered: command_palette_entries().to_vec(),
            selected: 0,
        }
    }

    pub(crate) fn query(&self) -> &str {
        &self.query
    }

    pub(crate) fn filtered(&self) -> &[AppCommand] {
        &self.filtered
    }

    pub(crate) fn selected(&self) -> usize {
        self.selected
    }

    /// The highlighted command, or `None` when the filtered list is empty
    /// (so Enter on no results is a no-op rather than an invalid index).
    pub(crate) fn selected_command(&self) -> Option<AppCommand> {
        self.filtered.get(self.selected).copied()
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

    /// Move the highlight up one row, clamped at the top (no wrap, R-8).
    pub(crate) fn move_up(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }

    /// Move the highlight down one row, clamped at the last row (no wrap,
    /// R-8). A no-op on an empty list.
    pub(crate) fn move_down(&mut self) {
        if let Some(last) = self.filtered.len().checked_sub(1) {
            self.selected = (self.selected + 1).min(last);
        }
    }

    fn refilter(&mut self) {
        self.filtered = command_palette_filter(&self.query);
        self.selected = 0;
    }
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
            AppCommand::Copy,
            AppCommand::Paste,
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
            AppCommand::NewSplitRight,
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
            AppCommand::CloseTab,
            AppCommand::NextTab,
            AppCommand::PrevTab,
            AppCommand::CloseWindow,
            AppCommand::Quit,
            AppCommand::ToggleCommandPalette,
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
        let split_right = entries.iter().position(|c| *c == AppCommand::NewSplitRight);
        assert!(new_tab < split_right, "NewTab precedes NewSplitRight");
    }

    #[test]
    fn subsequence_match_is_case_insensitive_and_non_contiguous() {
        assert!(is_subsequence_ci("", "anything"));
        assert!(is_subsequence_ci("splt", "Split Right"));
        assert!(is_subsequence_ci("QUIT", "Quit noa"));
        assert!(!is_subsequence_ci("zzz", "Split Right"));
        assert!(!is_subsequence_ci("tips", "Split Right"), "order matters");
    }

    #[test]
    fn filter_keeps_declaration_order_and_resets_are_subsequence_only() {
        // AC-8: "splt" keeps only titles containing s..p..l..t in order.
        let matches = command_palette_filter("splt");
        assert!(matches.contains(&AppCommand::NewSplitRight));
        assert!(matches.contains(&AppCommand::NewSplitDown));
        assert!(matches.contains(&AppCommand::ToggleSplitZoom));
        assert!(!matches.contains(&AppCommand::Copy));

        // Declaration order is preserved (Split Right precedes Split Down).
        let right = matches.iter().position(|c| *c == AppCommand::NewSplitRight);
        let down = matches.iter().position(|c| *c == AppCommand::NewSplitDown);
        assert!(right < down);

        // Case-insensitive: "QUIT" matches "Quit noa" (and any other title
        // carrying q..u..i..t as a subsequence).
        let quit = command_palette_filter("QUIT");
        assert!(quit.contains(&AppCommand::Quit));
        assert!(!quit.contains(&AppCommand::Copy));
    }

    #[test]
    fn typing_refilters_and_resets_the_highlight() {
        // AC-8 / AC-9.
        let mut palette = CommandPalette::open();
        assert_eq!(palette.selected(), 0);
        assert_eq!(palette.filtered().len(), command_palette_entries().len());

        palette.push_text("new");
        assert_eq!(palette.query(), "new");
        assert!(palette.filtered().contains(&AppCommand::NewTab));
        assert_eq!(palette.selected(), 0);

        palette.backspace();
        assert_eq!(palette.query(), "ne");
        assert_eq!(palette.selected(), 0);
    }

    #[test]
    fn navigation_clamps_without_wrapping() {
        // AC-10: drive a small, known three-entry list.
        let mut palette = CommandPalette::open();
        palette.filtered = vec![
            AppCommand::NewTab,
            AppCommand::NewSplitRight,
            AppCommand::NewSplitDown,
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
    fn enter_on_an_empty_result_set_has_no_command() {
        // AC-13: no valid index, so no dispatch and no panic.
        let mut palette = CommandPalette::open();
        palette.push_text("zzzzzz");
        assert!(palette.filtered().is_empty());
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
