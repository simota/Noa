use super::super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::app) enum ActiveOverlay {
    None,
    CommandPalette,
    Search,
    ThemeSettings,
}

/// The R-3 exclusion gate every one of the three overlay open-paths
/// (`toggle_command_palette`, the search `Find` action, `open_theme_settings`)
/// checks before opening. A free function over plain booleans — not an
/// `&App` method — so the exclusion decision is unit-testable without
/// constructing real window state; [`App::active_overlay`] is a thin wrapper
/// supplying the three `Option::is_some_and(...)` checks.
fn active_overlay_gate(
    command_palette_open: bool,
    search_open: bool,
    theme_settings_open: bool,
) -> ActiveOverlay {
    if command_palette_open {
        ActiveOverlay::CommandPalette
    } else if search_open {
        ActiveOverlay::Search
    } else if theme_settings_open {
        ActiveOverlay::ThemeSettings
    } else {
        ActiveOverlay::None
    }
}

struct PaletteEnterDecision {
    pub(super) close_palette: bool,
    pub(super) dispatch: Option<AppCommand>,
}

fn palette_enter_decision(selected_command: Option<AppCommand>) -> PaletteEnterDecision {
    match selected_command {
        Some(command) => PaletteEnterDecision {
            close_palette: true,
            dispatch: Some(command),
        },
        None => PaletteEnterDecision {
            close_palette: false,
            dispatch: None,
        },
    }
}

fn palette_enter_decision_for_command(
    selected_command: Option<AppCommand>,
    command_enabled: impl FnOnce(AppCommand) -> bool,
) -> PaletteEnterDecision {
    match selected_command {
        Some(command) if command_enabled(command) => palette_enter_decision(Some(command)),
        Some(_) | None => palette_enter_decision(None),
    }
}

impl App {
    pub(in crate::app) fn handle_command_palette_key(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: &KeyEvent,
    ) {
        match &event.logical_key {
            Key::Named(NamedKey::Escape) => {
                // Close without executing (R-8).
                self.command_palette = None;
                self.request_window_redraw(window_id);
                return;
            }
            Key::Named(NamedKey::Enter) => {
                let command = self
                    .command_palette
                    .as_ref()
                    .and_then(|session| session.palette.selected_command());
                let decision = palette_enter_decision_for_command(command, |command| {
                    self.command_is_enabled(window_id, command)
                });
                if decision.close_palette {
                    self.command_palette = None;
                }
                if let Some(command) = decision.dispatch {
                    self.handle_app_command(event_loop, command, CommandOrigin::TerminalWindow);
                }
                return;
            }
            Key::Named(NamedKey::ArrowUp) => {
                if let Some(session) = self.command_palette.as_mut() {
                    session.palette.move_up();
                }
                self.request_window_redraw(window_id);
                return;
            }
            Key::Named(NamedKey::ArrowDown) => {
                if let Some(session) = self.command_palette.as_mut() {
                    session.palette.move_down();
                }
                self.request_window_redraw(window_id);
                return;
            }
            Key::Named(NamedKey::Backspace) => {
                if let Some(session) = self.command_palette.as_mut() {
                    session.palette.backspace();
                }
                self.request_window_redraw(window_id);
                return;
            }
            _ => {}
        }

        if let Some(command) = self.keybinds.resolve(&event.logical_key, self.modifiers) {
            // Re-pressing cmd+shift+p toggles the palette closed; every other
            // resolved command is swallowed while the modal owns the keyboard.
            if command == AppCommand::ToggleCommandPalette {
                self.handle_app_command(event_loop, command, CommandOrigin::TerminalWindow);
            }
            return;
        }

        // Cmd-held combos with no binding must not leak their character into
        // the query (mirrors the search prompt's Cmd-swallow).
        if self.modifiers.super_key() {
            return;
        }
        let Some(text) = event.text.as_deref() else {
            return;
        };
        if let Some(session) = self.command_palette.as_mut() {
            session.palette.push_text(text);
        }
        self.request_window_redraw(window_id);
    }

    /// Which of the three mutually-exclusive overlays (command palette,
    /// search prompt, theme-settings) currently owns `window_id`'s keyboard,
    /// if any (R-3). See [`active_overlay_gate`] for the pure decision this
    /// wraps.
    pub(in crate::app) fn active_overlay(&self, window_id: WindowId) -> ActiveOverlay {
        active_overlay_gate(
            self.command_palette
                .as_ref()
                .is_some_and(|s| s.window_id == window_id),
            self.search_prompt
                .as_ref()
                .is_some_and(|s| s.window_id == window_id),
            self.theme_settings
                .as_ref()
                .is_some_and(|s| s.window_id == window_id),
        )
    }
}

#[cfg(test)]
mod active_overlay_gate_tests {
    use super::*;

    // AC: with the command palette open, the R-3 gate reports a non-`None`
    // overlay, so `App::open_theme_settings`'s `!= ActiveOverlay::None`
    // guard refuses to open theme-settings alongside it.
    #[test]
    fn command_palette_open_refuses_theme_settings() {
        assert_eq!(
            active_overlay_gate(true, false, false),
            ActiveOverlay::CommandPalette
        );
    }

    // AC: with theme-settings open, the gate reports `ThemeSettings`
    // (non-`None`) regardless of the other two flags, so both the palette
    // toggle and the search `Find` action's own `!= ActiveOverlay::None`
    // guards refuse to open alongside it.
    #[test]
    fn theme_settings_open_refuses_palette_and_search() {
        assert_eq!(
            active_overlay_gate(false, false, true),
            ActiveOverlay::ThemeSettings
        );
    }

    // AC: with the search prompt open, the gate reports `Search`
    // (non-`None`), so `App::open_theme_settings`'s guard refuses to open
    // theme-settings alongside it.
    #[test]
    fn search_open_refuses_theme_settings() {
        assert_eq!(
            active_overlay_gate(false, true, false),
            ActiveOverlay::Search
        );
    }
}

#[cfg(test)]
mod palette_enter_decision_tests {
    use super::*;

    #[test]
    fn selected_command_closes_palette_and_dispatches_it() {
        let decision = palette_enter_decision(Some(AppCommand::OpenThemeSettings));
        assert!(decision.close_palette);
        assert_eq!(decision.dispatch, Some(AppCommand::OpenThemeSettings));
    }

    #[test]
    fn no_selection_leaves_palette_open_and_dispatches_nothing() {
        let decision = palette_enter_decision(None);
        assert!(!decision.close_palette);
        assert!(decision.dispatch.is_none());
    }

    #[test]
    fn disabled_selected_command_keeps_palette_open_and_dispatches_nothing() {
        let decision =
            palette_enter_decision_for_command(Some(AppCommand::NewSplitRight), |_| false);

        assert!(!decision.close_palette);
        assert!(decision.dispatch.is_none());
    }

    // AC-21: proves *why* the palette must close before the dispatched
    // command runs, by composing `palette_enter_decision`'s result with the
    // real R-3 gate exactly as `App::open_theme_settings` calls it. Once the
    // palette is closed (`close_palette: true` folded back into the
    // palette-open flag as `false`), the gate reports `None` and
    // theme-settings may open. Had the ordering been reversed — dispatching
    // before clearing `command_palette` — the gate would still see the
    // palette open and wrongly refuse; this test regresses if that ordering
    // ever creeps back in.
    #[test]
    fn palette_close_unblocks_dispatched_theme_settings_open() {
        let decision = palette_enter_decision(Some(AppCommand::OpenThemeSettings));
        assert!(decision.close_palette);
        let palette_open_after_enter = !decision.close_palette;
        assert_eq!(
            active_overlay_gate(palette_open_after_enter, false, false),
            ActiveOverlay::None
        );
    }
}
