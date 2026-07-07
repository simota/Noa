use super::Direction;
use winit::keyboard::{Key, ModifiersState, NamedKey};

/// An Overview-focused keyboard action, resolved directly from the raw
/// keypress rather than through the general `AppCommand`/`KeybindEngine` ->
/// `overview_command_scope` path (REQ-OV-15). This lets Return/arrows/Esc and
/// Cmd+1..9 work while every other `AppCommand` still resolves to a
/// `CommandScope::Overview` no-op.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OverviewAction {
    MoveSelection(Direction),
    Activate,
    SwitchToLive(usize),
    Dismiss,
    /// Tab: toggle the quick-look zoom of the selected tile. Tab is the one
    /// comfortable non-printable key left — every printable key (including
    /// Space) types into the "Search sessions" field.
    ToggleZoom,
}

/// Resolve an Overview-focused keypress to its [`OverviewAction`], or `None`
/// if the key isn't part of the Overview keymap (REQ-OV-15) — printable text
/// for the "Search sessions" field (REQ-OV-16) is handled separately by the
/// caller, since it isn't an action but a query edit.
pub fn overview_key_action(logical_key: &Key, modifiers: ModifiersState) -> Option<OverviewAction> {
    match logical_key {
        Key::Named(NamedKey::ArrowLeft) => Some(OverviewAction::MoveSelection(Direction::Left)),
        Key::Named(NamedKey::ArrowRight) => Some(OverviewAction::MoveSelection(Direction::Right)),
        Key::Named(NamedKey::ArrowUp) => Some(OverviewAction::MoveSelection(Direction::Up)),
        Key::Named(NamedKey::ArrowDown) => Some(OverviewAction::MoveSelection(Direction::Down)),
        Key::Named(NamedKey::Enter) => Some(OverviewAction::Activate),
        Key::Named(NamedKey::Escape) => Some(OverviewAction::Dismiss),
        Key::Named(NamedKey::Tab) => Some(OverviewAction::ToggleZoom),
        // Plain Cmd+<digit> only (mirrors `cmd+1`..`cmd+9`'s keybind chords,
        // which likewise require no other modifier) — a shifted/alt'd combo
        // falls through to `None` rather than misfiring a tile switch.
        Key::Character(text)
            if modifiers.super_key()
                && !modifiers.shift_key()
                && !modifiers.control_key()
                && !modifiers.alt_key() =>
        {
            text.chars()
                .next()
                .and_then(|c| c.to_digit(10))
                .filter(|&n| (1..=9).contains(&n))
                .map(|n| OverviewAction::SwitchToLive(n as usize))
        }
        _ => None,
    }
}

/// Move `selected` one step within a row-major grid `cols` wide and
/// `tile_count` cells long (REQ-OV-15a). `selected` indexes directly into the
/// combined live-tile + placeholder-tile source order (both share the same
/// `cols`), so this covers both without translation.
///
/// Grid edges clamp — arrows never wrap. A `Down`/`Right` move that would
/// land on a cell past `tile_count` (the trailing row can be shorter than
/// `cols`, REQ-OV-3) is dropped instead of snapping sideways to the last
/// tile: jumping to an unrelated column would be more surprising than simply
/// not moving for that one keypress.
pub fn move_overview_selection(
    selected: usize,
    cols: usize,
    tile_count: usize,
    direction: Direction,
) -> usize {
    if tile_count == 0 || cols == 0 {
        return 0;
    }
    let selected = selected.min(tile_count - 1);
    let col = selected % cols;
    let row = selected / cols;

    let candidate = match direction {
        Direction::Left => (col > 0).then(|| selected - 1),
        Direction::Right => (col + 1 < cols).then(|| selected + 1),
        Direction::Up => (row > 0).then(|| selected - cols),
        Direction::Down => Some(selected + cols),
    };

    candidate
        .filter(|&index| index < tile_count)
        .unwrap_or(selected)
}

/// Initial Overview selection on open (REQ-OV-14): the focused tab's position
/// within `source_ids` when it is a *live* tile (index `< live_tile_count`),
/// else the first tile (`0`).
pub fn overview_initial_selection<Id: PartialEq>(
    source_ids: &[Id],
    live_tile_count: usize,
    focused_id: Option<&Id>,
) -> usize {
    focused_id
        .and_then(|focused| source_ids.iter().position(|id| id == focused))
        .filter(|&index| index < live_tile_count)
        .unwrap_or(0)
}

/// Case-insensitive **contiguous substring** filter over tab titles
/// (REQ-OV-16) — deliberately distinct from `command_palette::fuzzy_match`'s
/// non-contiguous subsequence semantics; "Search sessions" is a plain substring
/// search, not fuzzy matching. An empty query matches every title.
pub fn overview_tab_filter<Id: Copy>(query: &str, titles: &[(Id, String)]) -> Vec<Id> {
    let query = query.to_ascii_lowercase();
    titles
        .iter()
        .filter(|(_, title)| query.is_empty() || title.to_ascii_lowercase().contains(&query))
        .map(|(id, _)| *id)
        .collect()
}

/// Two-stage Escape semantics for the Overview search field (REQ-OV-16). A
/// non-empty query swallows the first Escape to clear itself and keeps the
/// Overview open; an empty query dismisses the Overview. The command palette
/// has no two-stage-Escape precedent (its Escape always closes), so this
/// behavior is defined here per the P3 brief.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OverviewEscapeAction {
    /// Clear the (non-empty) query; leave the Overview visible.
    ClearSearch,
    /// Dismiss the Overview (query already empty).
    Dismiss,
}

/// Resolve the Escape keypress against the current `query` (REQ-OV-16).
pub fn overview_escape_action(query: &str) -> OverviewEscapeAction {
    if query.is_empty() {
        OverviewEscapeAction::Dismiss
    } else {
        OverviewEscapeAction::ClearSearch
    }
}
