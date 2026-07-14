use super::super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::app) enum ActiveOverlay {
    None,
    CommandPalette,
    SendSelectionPicker,
    RemoteUi,
    Search,
    ThemeSettings,
    ProcessMonitor,
    CopyMode,
}

/// The R-3 exclusion gate every one of the three overlay open-paths
/// (`toggle_command_palette`, the search `Find` action, `open_theme_settings`)
/// checks before opening. A free function over plain booleans — not an
/// `&App` method — so the exclusion decision is unit-testable without
/// constructing real window state; [`App::active_overlay`] is a thin wrapper
/// supplying the three `Option::is_some_and(...)` checks.
fn active_overlay_gate(
    command_palette_open: bool,
    send_selection_picker_open: bool,
    remote_ui_open: bool,
    search_open: bool,
    theme_settings_open: bool,
    process_monitor_open: bool,
    copy_mode_open: bool,
) -> ActiveOverlay {
    if command_palette_open {
        ActiveOverlay::CommandPalette
    } else if send_selection_picker_open {
        ActiveOverlay::SendSelectionPicker
    } else if remote_ui_open {
        ActiveOverlay::RemoteUi
    } else if search_open {
        ActiveOverlay::Search
    } else if theme_settings_open {
        ActiveOverlay::ThemeSettings
    } else if process_monitor_open {
        ActiveOverlay::ProcessMonitor
    } else if copy_mode_open {
        ActiveOverlay::CopyMode
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
    pub(in crate::app) fn open_send_selection_picker(&mut self) {
        let Some((source_window_id, source_pane)) =
            self.resolve_pane_command_target(AppCommand::SendSelectionToPane)
        else {
            return;
        };
        let Some((selected_text, targets)) =
            self.send_selection_picker_payload_and_targets(source_window_id, source_pane)
        else {
            return;
        };

        self.show_send_selection_picker(source_window_id, source_pane, selected_text, targets);
    }

    pub(in crate::app) fn open_send_selection_picker_with_payload(
        &mut self,
        source_window_id: WindowId,
        source_pane: PaneId,
        selected_text: String,
    ) {
        if selected_text.is_empty() {
            return;
        }
        let Some(targets) = self.available_send_selection_targets(source_window_id, source_pane)
        else {
            return;
        };

        self.show_send_selection_picker(source_window_id, source_pane, selected_text, targets);
    }

    fn show_send_selection_picker(
        &mut self,
        source_window_id: WindowId,
        source_pane: PaneId,
        selected_text: String,
        targets: Vec<SendSelectionTarget>,
    ) {
        self.send_selection_picker = Some(SendSelectionPickerSession {
            window_id: source_window_id,
            source_pane,
            selected_text,
            targets,
            selected: 0,
            opened_at: Instant::now(),
        });
        self.request_window_redraw(source_window_id);
    }

    pub(in crate::app) fn can_open_send_selection_picker_for_pane(
        &self,
        source_window_id: WindowId,
        source_pane: PaneId,
    ) -> bool {
        self.send_selection_picker_payload_and_targets(source_window_id, source_pane)
            .is_some()
    }

    fn send_selection_picker_payload_and_targets(
        &self,
        source_window_id: WindowId,
        source_pane: PaneId,
    ) -> Option<(String, Vec<SendSelectionTarget>)> {
        let targets = self.available_send_selection_targets(source_window_id, source_pane)?;

        let selected_text = self
            .windows
            .get(&source_window_id)
            .and_then(|state| state.surfaces.get(&source_pane))
            .and_then(|surface| surface.terminal.lock().selected_text());
        let selected_text = selected_text.filter(|text| !text.is_empty())?;

        Some((selected_text, targets))
    }

    fn available_send_selection_targets(
        &self,
        source_window_id: WindowId,
        source_pane: PaneId,
    ) -> Option<Vec<SendSelectionTarget>> {
        if self.active_overlay(source_window_id) != ActiveOverlay::None
            || self
                .confirm_dialog
                .as_ref()
                .is_some_and(|session| session.window_id == source_window_id)
            || self
                .tab_title_prompt
                .as_ref()
                .is_some_and(|session| session.window_id == source_window_id)
            || self
                .sidebar_rename
                .as_ref()
                .is_some_and(|session| session.window_id == source_window_id)
        {
            return None;
        }

        let targets = self.send_selection_targets(source_window_id, source_pane);
        if targets.is_empty() {
            return None;
        }

        Some(targets)
    }

    fn send_selection_targets(
        &self,
        source_window_id: WindowId,
        source_pane: PaneId,
    ) -> Vec<SendSelectionTarget> {
        inter_pane_targets_in_group(
            &self.window_order,
            |window_id| self.windows.get(&window_id).map(|state| state.group),
            |window_id| {
                self.windows
                    .get(&window_id)
                    .map(|state| split_tree_pane_ids(&state.split_tree))
                    .unwrap_or_default()
            },
            source_window_id,
            source_pane,
        )
        .into_iter()
        .filter_map(|target| {
            let state = self.windows.get(&target.window_id)?;
            if !state.surfaces.contains_key(&target.pane_id) {
                return None;
            }
            let label = inter_pane_target_label(
                target.tab_index,
                state
                    .title_override
                    .as_deref()
                    .or(Some(state.title.as_str())),
                target.pane_index,
                target.pane_id.get(),
            );
            Some(SendSelectionTarget {
                window_id: target.window_id,
                pane_id: target.pane_id,
                label,
            })
        })
        .collect()
    }

    pub(in crate::app) fn handle_send_selection_picker_key(
        &mut self,
        window_id: WindowId,
        event: &KeyEvent,
    ) {
        match &event.logical_key {
            Key::Named(NamedKey::Escape) => {
                self.send_selection_picker = None;
                self.request_window_redraw(window_id);
            }
            Key::Named(NamedKey::ArrowUp) => {
                if let Some(session) = self.send_selection_picker.as_mut() {
                    session.move_up();
                }
                self.request_window_redraw(window_id);
            }
            Key::Named(NamedKey::ArrowDown) => {
                if let Some(session) = self.send_selection_picker.as_mut() {
                    session.move_down();
                }
                self.request_window_redraw(window_id);
            }
            Key::Named(NamedKey::Enter) => {
                if let Some(selected) = self
                    .send_selection_picker
                    .as_ref()
                    .map(|session| session.selected)
                {
                    self.commit_send_selection_picker_target(selected);
                }
            }
            Key::Character(s) => {
                let Some(index) = picker_digit_index(s) else {
                    return;
                };
                self.commit_send_selection_picker_target(index);
            }
            _ => {}
        }
    }

    fn commit_send_selection_picker_target(&mut self, index: usize) {
        let Some(session) = self.send_selection_picker.take() else {
            return;
        };
        let Some(target) = session.targets.get(index).cloned() else {
            self.send_selection_picker = Some(session);
            return;
        };
        let confirm_window_id = session.window_id;
        self.request_window_redraw(confirm_window_id);
        self.paste_text_to_pane_with_confirm_window(
            confirm_window_id,
            target.window_id,
            target.pane_id,
            session.selected_text,
            self.config.send_selection_send_enter,
        );
    }

    pub(in crate::app) fn send_selection_picker_snapshot(
        &self,
        window_id: WindowId,
    ) -> Option<(CommandPaletteSnapshot, Instant)> {
        let session = self
            .send_selection_picker
            .as_ref()
            .filter(|session| session.window_id == window_id)?;
        let rows = session
            .targets
            .iter()
            .enumerate()
            .map(|(index, target)| PaletteRow::Entry {
                title: target.label.clone(),
                hint: (index < 9).then(|| (index + 1).to_string()),
                match_positions: Vec::new(),
                enabled: true,
            })
            .collect::<Vec<_>>();
        Some((
            CommandPaletteSnapshot {
                query: format!("Send selection from PaneId {}", session.source_pane.get()),
                rows,
                selected: session.selected,
                total_entries: session.targets.len(),
            },
            session.opened_at,
        ))
    }

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
            self.send_selection_picker
                .as_ref()
                .is_some_and(|s| s.window_id == window_id),
            self.remote_ui
                .as_ref()
                .is_some_and(|s| s.window_id == window_id),
            self.search_prompt
                .as_ref()
                .is_some_and(|s| s.window_id == window_id),
            self.theme_settings
                .as_ref()
                .is_some_and(|s| s.window_id == window_id),
            self.process_monitor
                .as_ref()
                .is_some_and(|s| s.window_id == window_id),
            self.copy_mode
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
            active_overlay_gate(true, false, false, false, false, false, false),
            ActiveOverlay::CommandPalette
        );
    }

    #[test]
    fn send_selection_picker_refuses_other_overlays() {
        assert_eq!(
            active_overlay_gate(false, true, false, false, false, false, false),
            ActiveOverlay::SendSelectionPicker
        );
    }

    // AC: with theme-settings open, the gate reports `ThemeSettings`
    // (non-`None`) regardless of the other two flags, so both the palette
    // toggle and the search `Find` action's own `!= ActiveOverlay::None`
    // guards refuse to open alongside it.
    #[test]
    fn theme_settings_open_refuses_palette_and_search() {
        assert_eq!(
            active_overlay_gate(false, false, false, false, true, false, false),
            ActiveOverlay::ThemeSettings
        );
    }

    // AC: with the search prompt open, the gate reports `Search`
    // (non-`None`), so `App::open_theme_settings`'s guard refuses to open
    // theme-settings alongside it.
    #[test]
    fn search_open_refuses_theme_settings() {
        assert_eq!(
            active_overlay_gate(false, false, false, true, false, false, false),
            ActiveOverlay::Search
        );
    }

    // AC-1/AC-7 (panel-metrics-view R-3): with the process monitor open, the
    // gate reports `ProcessMonitor` (non-`None`), so every other overlay's
    // own guard refuses to open alongside it.
    #[test]
    fn process_monitor_open_refuses_other_overlays() {
        assert_eq!(
            active_overlay_gate(false, false, false, false, false, true, false),
            ActiveOverlay::ProcessMonitor
        );
    }

    #[test]
    fn copy_mode_excludes_other_overlays_but_has_lower_modal_priority() {
        assert_eq!(
            active_overlay_gate(false, false, false, false, false, false, true),
            ActiveOverlay::CopyMode
        );
        assert_eq!(
            active_overlay_gate(true, false, false, false, false, false, true),
            ActiveOverlay::CommandPalette
        );
    }

    #[test]
    fn remote_ui_refuses_other_overlays() {
        assert_eq!(
            active_overlay_gate(false, false, true, false, false, false, false),
            ActiveOverlay::RemoteUi
        );
    }
}

#[cfg(test)]
mod palette_enter_decision_tests {
    use super::*;

    #[test]
    fn selected_command_closes_palette_and_dispatches_it() {
        let decision = palette_enter_decision(Some(AppCommand::OpenThemePicker));
        assert!(decision.close_palette);
        assert_eq!(decision.dispatch, Some(AppCommand::OpenThemePicker));
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
        let decision = palette_enter_decision(Some(AppCommand::OpenThemePicker));
        assert!(decision.close_palette);
        let palette_open_after_enter = !decision.close_palette;
        assert_eq!(
            active_overlay_gate(palette_open_after_enter, false, false, false, false, false, false),
            ActiveOverlay::None
        );
    }

    #[test]
    fn picker_digit_index_maps_number_keys_to_zero_based_indices() {
        assert_eq!(picker_digit_index("1"), Some(0));
        assert_eq!(picker_digit_index("9"), Some(8));
        assert_eq!(picker_digit_index("0"), None);
        assert_eq!(picker_digit_index("10"), None);
        assert_eq!(picker_digit_index("x"), None);
    }
}

fn picker_digit_index(text: &str) -> Option<usize> {
    let mut chars = text.chars();
    let ch = chars.next()?;
    if chars.next().is_some() {
        return None;
    }
    ch.to_digit(10)
        .and_then(|digit| digit.checked_sub(1))
        .map(|index| index as usize)
}
