use crate::AppCommand;

use super::types::PaneId;

/// Ordered IME-side operations required when focus moves between panes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImeOp {
    CommitPreedit(PaneId),
    RetargetIme(PaneId),
}

/// Build the app-shell IME operation sequence for moving pane focus.
pub fn focus_switch_plan(losing: PaneId, winning: PaneId) -> Vec<ImeOp> {
    vec![ImeOp::CommitPreedit(losing), ImeOp::RetargetIme(winning)]
}

/// Resolve pane-scoped app commands to the currently focused pane.
pub fn resolve_pane_command_target(
    command: AppCommand,
    focused_pane: Option<PaneId>,
) -> Option<PaneId> {
    match command {
        AppCommand::Copy
        | AppCommand::Paste
        | AppCommand::Terminal(_)
        | AppCommand::FontSize(_)
        | AppCommand::Search(_)
        | AppCommand::ScrollViewport(_)
        | AppCommand::NewSplitLeft
        | AppCommand::NewSplitRight
        | AppCommand::NewSplitUp
        | AppCommand::NewSplitDown
        | AppCommand::FocusDirection(_)
        | AppCommand::ResizeSplit(_)
        | AppCommand::EqualizeSplits
        | AppCommand::ToggleSplitZoom
        | AppCommand::CloseTab => focused_pane,
        AppCommand::About
        | AppCommand::Preferences
        | AppCommand::NewTab
        | AppCommand::NewWindow
        | AppCommand::ToggleTabOverview
        | AppCommand::ToggleCommandPalette
        | AppCommand::OpenThemeSettings
        | AppCommand::ToggleFullscreen
        | AppCommand::ToggleQuickTerminal
        | AppCommand::ToggleSecureKeyboardEntry
        | AppCommand::ToggleSidebar
        | AppCommand::ToggleAutoApprove
        | AppCommand::SelectTab(_)
        | AppCommand::NextTab
        | AppCommand::PrevTab
        // Tab-scoped, not pane-scoped: the prompt names the native tab.
        | AppCommand::SetTabTitle
        | AppCommand::CloseWindow
        | AppCommand::Quit => None,
    }
}
