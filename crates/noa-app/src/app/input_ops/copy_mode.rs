use super::super::*;
use winit::keyboard::KeyCode;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CopyModeFixedKey {
    Move(noa_grid::CopyDirection, bool),
    CopyAndExit,
    Cancel,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CopyModeReleaseAction {
    Consume,
    PassThrough,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CopyModeCommandTransition {
    Preserve,
    RepairAndCaptureSelection,
    Exit,
    CaptureSelectionAndExit,
}

fn copy_mode_release_action(press_consumed: bool) -> CopyModeReleaseAction {
    if press_consumed {
        CopyModeReleaseAction::Consume
    } else {
        CopyModeReleaseAction::PassThrough
    }
}

fn copy_mode_should_suppress_repeat(repeat: bool, exit_press_consumed: bool) -> bool {
    repeat && exit_press_consumed
}

fn copy_mode_fixed_key_blocks_repeat(key: CopyModeFixedKey) -> bool {
    !matches!(key, CopyModeFixedKey::Move(..))
}

pub(in crate::app) fn copy_mode_should_exit_for_pty_bytes(
    mode_active: bool,
    logical_key: &Key,
    physical_key: PhysicalKey,
    bytes: Option<&[u8]>,
) -> bool {
    mode_active
        && !copy_mode_modifier_key(logical_key, physical_key)
        && bytes.is_some_and(|bytes| !bytes.is_empty())
}

pub(in crate::app) fn copy_mode_should_swallow_super_key(
    modifiers: ModifiersState,
    directional_passthrough: bool,
) -> bool {
    modifiers.super_key() && !directional_passthrough
}

fn copy_mode_modifier_key(logical_key: &Key, physical_key: PhysicalKey) -> bool {
    matches!(
        logical_key,
        Key::Named(
            NamedKey::Alt
                | NamedKey::AltGraph
                | NamedKey::CapsLock
                | NamedKey::Control
                | NamedKey::Fn
                | NamedKey::FnLock
                | NamedKey::Hyper
                | NamedKey::Meta
                | NamedKey::NumLock
                | NamedKey::ScrollLock
                | NamedKey::Shift
                | NamedKey::Super
                | NamedKey::Symbol
                | NamedKey::SymbolLock
        )
    ) || matches!(
        physical_key,
        PhysicalKey::Code(
            KeyCode::ShiftLeft
                | KeyCode::ShiftRight
                | KeyCode::ControlLeft
                | KeyCode::ControlRight
                | KeyCode::AltLeft
                | KeyCode::AltRight
                | KeyCode::SuperLeft
                | KeyCode::SuperRight
                | KeyCode::CapsLock
                | KeyCode::Fn
                | KeyCode::FnLock
                | KeyCode::NumLock
                | KeyCode::ScrollLock
        )
    )
}

fn exit_copy_mode_terminal(terminal: &Arc<Mutex<Terminal>>) -> bool {
    let mut terminal = terminal.lock();
    let viewport_before = terminal.viewport_offset();
    terminal.exit_copy_mode();
    terminal.viewport_offset() != viewport_before
}

fn repair_and_capture_copy_mode_selection(
    state: &mut noa_grid::CopyModeState,
    terminal: &mut Terminal,
) -> Option<String> {
    state.repair_eviction(terminal);
    terminal.selected_text()
}

fn copy_mode_fixed_key(key: &Key, modifiers: ModifiersState) -> Option<CopyModeFixedKey> {
    let extend = modifiers == ModifiersState::SHIFT;
    if !modifiers.is_empty() && !extend {
        return None;
    }
    match key {
        Key::Named(NamedKey::ArrowLeft) => Some(CopyModeFixedKey::Move(
            noa_grid::CopyDirection::Left,
            extend,
        )),
        Key::Named(NamedKey::ArrowRight) => Some(CopyModeFixedKey::Move(
            noa_grid::CopyDirection::Right,
            extend,
        )),
        Key::Named(NamedKey::ArrowUp) => {
            Some(CopyModeFixedKey::Move(noa_grid::CopyDirection::Up, extend))
        }
        Key::Named(NamedKey::ArrowDown) => Some(CopyModeFixedKey::Move(
            noa_grid::CopyDirection::Down,
            extend,
        )),
        Key::Named(NamedKey::Enter) if modifiers.is_empty() => Some(CopyModeFixedKey::CopyAndExit),
        Key::Named(NamedKey::Escape) if modifiers.is_empty() => Some(CopyModeFixedKey::Cancel),
        _ => None,
    }
}

fn copy_mode_directional_action_passes_through_on_alt(
    command: AppCommand,
    active_is_alt: bool,
) -> bool {
    active_is_alt && matches!(command, AppCommand::CopyMode(CopyModeAction::Extend(_)))
}

fn copy_mode_state_for_start(
    terminal: &mut Terminal,
    action: CopyModeAction,
) -> Option<noa_grid::CopyModeState> {
    if terminal.active_is_alt && matches!(action, CopyModeAction::Extend(_)) {
        return None;
    }
    let mut state = noa_grid::CopyModeState::enter(terminal)?;
    if let CopyModeAction::Extend(direction) = action {
        state.move_cursor(terminal, direction, true);
    }
    Some(state)
}

fn command_opens_overlay(command: AppCommand) -> bool {
    matches!(
        command,
        AppCommand::SendSelectionToPane
            | AppCommand::Preferences
            | AppCommand::Search(SearchAction::Find)
            | AppCommand::SetTabTitle
            | AppCommand::ToggleTabOverview
            | AppCommand::ToggleCommandPalette
            | AppCommand::OpenThemePicker
            | AppCommand::OpenSettings
            | AppCommand::ToggleProcessMonitor
    )
}

fn command_requires_pre_dispatch_exit(command: AppCommand) -> bool {
    matches!(
        command,
        AppCommand::CloseTab
            | AppCommand::CloseWindow
            | AppCommand::PipeScrollbackToPager
            | AppCommand::ToggleQuickTerminal
            | AppCommand::ToggleScratchTerminal
    )
}

fn command_invalidates_copy_mode_state(command: AppCommand) -> bool {
    matches!(
        command,
        AppCommand::Terminal(
            TerminalAction::Clear | TerminalAction::ClearScrollback | TerminalAction::SelectAll
        )
    )
}

fn copy_mode_command_transition(command: AppCommand) -> CopyModeCommandTransition {
    if matches!(command, AppCommand::CopyMode(_)) {
        CopyModeCommandTransition::Preserve
    } else if command == AppCommand::Copy {
        CopyModeCommandTransition::RepairAndCaptureSelection
    } else if command == AppCommand::SendSelectionToPane {
        CopyModeCommandTransition::CaptureSelectionAndExit
    } else if command_opens_overlay(command)
        || command_requires_pre_dispatch_exit(command)
        || command_invalidates_copy_mode_state(command)
    {
        CopyModeCommandTransition::Exit
    } else {
        CopyModeCommandTransition::Preserve
    }
}

impl App {
    /// Apply copy-mode command transitions before the shared app-command
    /// dispatcher mutates overlays, focus, selection, or terminal contents.
    /// Returns `true` when this method fully handled the command.
    pub(in crate::app) fn handle_copy_mode_before_app_command(
        &mut self,
        command: AppCommand,
    ) -> bool {
        if self.copy_mode.is_none() {
            return false;
        }

        match copy_mode_command_transition(command) {
            CopyModeCommandTransition::Preserve => false,
            CopyModeCommandTransition::RepairAndCaptureSelection => {
                let selected_text = self.copy_mode.as_mut().and_then(|session| {
                    let mut terminal = session.terminal.lock();
                    repair_and_capture_copy_mode_selection(&mut session.state, &mut terminal)
                });
                if let Some(text) = selected_text {
                    self.write_text_to_clipboard(&text);
                }
                true
            }
            CopyModeCommandTransition::Exit => {
                self.end_copy_mode();
                false
            }
            CopyModeCommandTransition::CaptureSelectionAndExit => {
                let payload = self.copy_mode.as_mut().and_then(|session| {
                    let mut terminal = session.terminal.lock();
                    let selected_text =
                        repair_and_capture_copy_mode_selection(&mut session.state, &mut terminal)?;
                    Some((session.window_id, session.pane_id, selected_text))
                });
                self.end_copy_mode();
                if let Some((source_window_id, source_pane, selected_text)) = payload {
                    self.open_send_selection_picker_with_payload(
                        source_window_id,
                        source_pane,
                        selected_text,
                    );
                }
                true
            }
        }
    }

    /// Enforce copy-mode focus and modal ownership after any app command,
    /// regardless of whether it originated from a keybind, menu, or IPC path.
    pub(in crate::app) fn reconcile_copy_mode_after_app_command(&mut self) {
        self.end_copy_mode_if_focus_changed();
        let Some(window_id) = self.copy_mode.as_ref().map(|session| session.window_id) else {
            return;
        };
        let modal_opened = self.active_overlay(window_id) != ActiveOverlay::CopyMode
            || self
                .confirm_dialog
                .as_ref()
                .is_some_and(|session| session.window_id == window_id)
            || self
                .tab_title_prompt
                .as_ref()
                .is_some_and(|session| session.window_id == window_id)
            || self.overview_visible;
        if modal_opened {
            self.end_copy_mode();
        }
    }

    /// Returns `false` when the command was rejected. Keyboard callers use
    /// that result to continue through normal pty encoding instead of
    /// swallowing a directional press after an alt-screen race.
    pub(in crate::app) fn start_copy_mode(
        &mut self,
        window_id: WindowId,
        action: CopyModeAction,
    ) -> bool {
        if self.copy_mode.is_some()
            || self.active_overlay(window_id) != ActiveOverlay::None
            || self.overview_visible
            || self
                .confirm_dialog
                .as_ref()
                .is_some_and(|session| session.window_id == window_id)
            || self
                .tab_title_prompt
                .as_ref()
                .is_some_and(|session| session.window_id == window_id)
            || self
                .sidebar_rename
                .as_ref()
                .is_some_and(|session| session.window_id == window_id)
        {
            return false;
        }

        let Some((pane_id, terminal)) = self.windows.get(&window_id).and_then(|window| {
            let pane_id = window.focused_pane;
            let terminal = Arc::clone(&window.surfaces.get(&pane_id)?.terminal);
            Some((pane_id, terminal))
        }) else {
            return false;
        };

        let (state, viewport_changed) = {
            let mut terminal_guard = terminal.lock();
            let viewport_before = terminal_guard.viewport_offset();
            let Some(state) = copy_mode_state_for_start(&mut terminal_guard, action) else {
                return false;
            };
            let viewport_changed = terminal_guard.viewport_offset() != viewport_before;
            (state, viewport_changed)
        };

        self.copy_mode = Some(CopyModeSession {
            window_id,
            pane_id,
            terminal,
            state,
        });
        if viewport_changed {
            self.invalidate_copy_mode_held_snapshot(window_id, pane_id);
        }
        self.request_window_redraw(window_id);
        true
    }

    /// Central, idempotent exit used by every copy-mode lifecycle path.
    pub(in crate::app) fn end_copy_mode(&mut self) {
        let Some(session) = self.copy_mode.take() else {
            return;
        };
        exit_copy_mode_terminal(&session.terminal);
        if let Some(surface) = self
            .windows
            .get_mut(&session.window_id)
            .and_then(|window| window.surfaces.get_mut(&session.pane_id))
        {
            // A held synchronized-output frame may still contain the copy
            // selection captured before exit. Force the next redraw to take
            // one coherent snapshot of the now-current rows and selection.
            surface.held_snapshot = None;
        }
        self.request_window_redraw(session.window_id);
    }

    /// Drop a synchronized-output hold only for app-owned viewport movement
    /// in the pane currently bound to copy mode. PTY-owned scrolling must keep
    /// the hold so intermediate synchronized rows remain hidden.
    pub(in crate::app) fn invalidate_copy_mode_held_snapshot(
        &mut self,
        window_id: WindowId,
        pane_id: PaneId,
    ) {
        if !self
            .copy_mode
            .as_ref()
            .is_some_and(|session| session.window_id == window_id && session.pane_id == pane_id)
        {
            return;
        }
        if let Some(surface) = self
            .windows
            .get_mut(&window_id)
            .and_then(|window| window.surfaces.get_mut(&pane_id))
        {
            surface.held_snapshot = None;
        }
    }

    pub(in crate::app) fn remember_copy_mode_activation_press(
        &mut self,
        window_id: WindowId,
        physical_key: PhysicalKey,
        mode_was_active: bool,
    ) {
        if !mode_was_active
            && self
                .copy_mode
                .as_ref()
                .is_some_and(|session| session.window_id == window_id)
        {
            self.copy_mode_suppressed_releases.insert(physical_key);
        }
    }

    pub(in crate::app) fn handle_copy_mode_key_release(
        &mut self,
        _window_id: WindowId,
        physical_key: PhysicalKey,
    ) -> bool {
        self.copy_mode_suppressed_repeats.remove(&physical_key);
        let press_consumed = self.copy_mode_suppressed_releases.remove(&physical_key);
        match copy_mode_release_action(press_consumed) {
            CopyModeReleaseAction::Consume => true,
            CopyModeReleaseAction::PassThrough => false,
        }
    }

    pub(in crate::app) fn copy_mode_key_repeat_is_suppressed(
        &self,
        physical_key: PhysicalKey,
        repeat: bool,
    ) -> bool {
        copy_mode_should_suppress_repeat(
            repeat,
            self.copy_mode_suppressed_repeats.contains(&physical_key),
        )
    }

    pub(in crate::app) fn ime_commit_should_end_copy_mode(
        modal_owned: bool,
        event: &Ime,
        has_pty_bytes: bool,
    ) -> bool {
        !modal_owned && has_pty_bytes && matches!(event, Ime::Commit(_))
    }

    pub(in crate::app) fn end_copy_mode_for_window(&mut self, window_id: WindowId) {
        if self
            .copy_mode
            .as_ref()
            .is_some_and(|session| session.window_id == window_id)
        {
            self.end_copy_mode();
        }
    }

    pub(in crate::app) fn end_copy_mode_for_pane(&mut self, window_id: WindowId, pane_id: PaneId) {
        if self
            .copy_mode
            .as_ref()
            .is_some_and(|session| session.window_id == window_id && session.pane_id == pane_id)
        {
            self.end_copy_mode();
        }
    }

    pub(in crate::app) fn end_copy_mode_if_focus_changed(&mut self) {
        let Some(session) = self.copy_mode.as_ref() else {
            return;
        };
        let still_bound = self.os_focused == Some(session.window_id)
            && self.windows.get(&session.window_id).is_some_and(|window| {
                window.focused_pane == session.pane_id
                    && window.surfaces.contains_key(&session.pane_id)
            });
        if !still_bound {
            self.end_copy_mode();
        }
    }

    pub(in crate::app) fn copy_mode_directional_action_passes_through(
        &self,
        window_id: WindowId,
        command: AppCommand,
    ) -> bool {
        let active_is_alt = self
            .windows
            .get(&window_id)
            .and_then(WindowState::focused_surface)
            .is_some_and(|surface| surface.terminal.lock().active_is_alt);
        copy_mode_directional_action_passes_through_on_alt(command, active_is_alt)
    }

    /// Handle the active-mode priority tier. `false` means the caller must
    /// continue with normal pty encoding after this method performed the
    /// exit-and-passthrough transition.
    pub(in crate::app) fn handle_copy_mode_key(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: &KeyEvent,
    ) -> bool {
        if self
            .copy_mode
            .as_ref()
            .is_none_or(|session| session.window_id != window_id)
        {
            return false;
        }
        if event.state == ElementState::Released {
            return false;
        }

        if let Some(fixed) = copy_mode_fixed_key(&event.logical_key, self.modifiers) {
            self.copy_mode_suppressed_releases
                .insert(event.physical_key);
            if copy_mode_fixed_key_blocks_repeat(fixed) {
                self.copy_mode_suppressed_repeats.insert(event.physical_key);
            }
            match fixed {
                CopyModeFixedKey::Move(direction, extend) => {
                    let viewport_changed = self.copy_mode.as_mut().is_some_and(|session| {
                        let mut terminal = session.terminal.lock();
                        let viewport_before = terminal.viewport_offset();
                        session.state.move_cursor(&mut terminal, direction, extend);
                        terminal.viewport_offset() != viewport_before
                    });
                    if viewport_changed
                        && let Some(pane_id) =
                            self.copy_mode.as_ref().map(|session| session.pane_id)
                    {
                        self.invalidate_copy_mode_held_snapshot(window_id, pane_id);
                    }
                    self.request_window_redraw(window_id);
                }
                CopyModeFixedKey::CopyAndExit => {
                    let selected_text = self.copy_mode.as_mut().and_then(|session| {
                        let mut terminal = session.terminal.lock();
                        repair_and_capture_copy_mode_selection(&mut session.state, &mut terminal)
                    });
                    if let Some(text) = selected_text {
                        self.write_text_to_clipboard(&text);
                    }
                    self.end_copy_mode();
                }
                CopyModeFixedKey::Cancel => {
                    let should_exit = self.copy_mode.as_mut().is_some_and(|session| {
                        let mut terminal = session.terminal.lock();
                        session.state.cancel(&mut terminal) == noa_grid::CopyModeCancel::Exit
                    });
                    if should_exit {
                        self.end_copy_mode();
                    } else {
                        self.request_window_redraw(window_id);
                    }
                }
            }
            return true;
        }

        if let Some(command) = self.keybinds.resolve(&event.logical_key, self.modifiers) {
            self.copy_mode_suppressed_releases
                .insert(event.physical_key);
            if matches!(command, AppCommand::CopyMode(_)) {
                return true;
            }
            self.handle_app_command(event_loop, command, CommandOrigin::TerminalWindow);
            return true;
        }

        false
    }

    /// Validate that copy mode is still bound to this window's focused pane.
    /// Redraw performs coordinate repair after acquiring that pane's terminal
    /// lock so repair and snapshot capture are one atomic read.
    pub(in crate::app) fn copy_mode_pane_for_redraw(
        &mut self,
        window_id: WindowId,
    ) -> Option<PaneId> {
        let session = self.copy_mode.as_ref()?;
        if session.window_id != window_id {
            return None;
        }
        let still_bound = self.os_focused == Some(window_id)
            && self.windows.get(&window_id).is_some_and(|window| {
                window.focused_pane == session.pane_id
                    && window
                        .surfaces
                        .get(&session.pane_id)
                        .is_some_and(|surface| Arc::ptr_eq(&surface.terminal, &session.terminal))
            });
        if !still_bound {
            self.end_copy_mode();
            return None;
        }
        Some(session.pane_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_keys_use_only_each_events_shift_bit() {
        assert_eq!(
            copy_mode_fixed_key(&Key::Named(NamedKey::ArrowRight), ModifiersState::SHIFT),
            Some(CopyModeFixedKey::Move(noa_grid::CopyDirection::Right, true))
        );
        assert_eq!(
            copy_mode_fixed_key(&Key::Named(NamedKey::ArrowRight), ModifiersState::empty()),
            Some(CopyModeFixedKey::Move(
                noa_grid::CopyDirection::Right,
                false
            ))
        );
        assert_eq!(
            copy_mode_fixed_key(
                &Key::Named(NamedKey::ArrowRight),
                ModifiersState::SHIFT | ModifiersState::SUPER
            ),
            None
        );
    }

    #[test]
    fn only_exit_fixed_keys_block_repeat_until_release() {
        assert!(!copy_mode_fixed_key_blocks_repeat(CopyModeFixedKey::Move(
            noa_grid::CopyDirection::Right,
            false
        )));
        assert!(copy_mode_fixed_key_blocks_repeat(
            CopyModeFixedKey::CopyAndExit
        ));
        assert!(copy_mode_fixed_key_blocks_repeat(CopyModeFixedKey::Cancel));

        assert!(!copy_mode_should_suppress_repeat(false, true));
        assert!(copy_mode_should_suppress_repeat(true, true));
        assert!(!copy_mode_should_suppress_repeat(true, false));
    }

    #[test]
    fn overlay_openers_are_identified_before_dispatch() {
        assert!(command_opens_overlay(AppCommand::ToggleCommandPalette));
        assert!(command_opens_overlay(AppCommand::Search(
            SearchAction::Find
        )));
        assert!(!command_opens_overlay(AppCommand::Copy));
        assert!(command_requires_pre_dispatch_exit(AppCommand::CloseTab));
        assert!(command_requires_pre_dispatch_exit(
            AppCommand::PipeScrollbackToPager
        ));
        assert!(!command_requires_pre_dispatch_exit(AppCommand::Copy));
        for command in [
            AppCommand::NewTab,
            AppCommand::NewWindow,
            AppCommand::NewSplitRight,
            AppCommand::FocusDirection(Direction::Right),
            AppCommand::SelectTab(1),
            AppCommand::NextTab,
            AppCommand::PrevTab,
        ] {
            assert!(!command_requires_pre_dispatch_exit(command));
            assert_eq!(
                copy_mode_command_transition(command),
                CopyModeCommandTransition::Preserve
            );
        }
    }

    #[test]
    fn unmatched_release_passes_through_without_exiting_early() {
        assert_eq!(
            copy_mode_release_action(false),
            CopyModeReleaseAction::PassThrough
        );
    }

    #[test]
    fn activation_press_consumes_its_release() {
        assert_eq!(
            copy_mode_release_action(true),
            CopyModeReleaseAction::Consume
        );
    }

    #[test]
    fn fixed_key_press_consumes_its_release() {
        assert_eq!(
            copy_mode_release_action(true),
            CopyModeReleaseAction::Consume
        );
    }

    #[test]
    fn unpaired_modifier_release_passes_through() {
        assert_eq!(
            copy_mode_release_action(false),
            CopyModeReleaseAction::PassThrough
        );
    }

    #[test]
    fn exit_key_release_is_consumed_after_mode_ends() {
        assert_eq!(
            copy_mode_release_action(true),
            CopyModeReleaseAction::Consume
        );
    }

    #[test]
    fn copy_mode_exits_only_for_non_modifier_pty_bytes() {
        let character_key = Key::Character("x".into());
        let character = PhysicalKey::Code(KeyCode::KeyX);
        assert!(!copy_mode_should_exit_for_pty_bytes(
            true,
            &character_key,
            character,
            None
        ));
        assert!(!copy_mode_should_exit_for_pty_bytes(
            true,
            &character_key,
            character,
            Some(&[])
        ));
        assert!(copy_mode_should_exit_for_pty_bytes(
            true,
            &character_key,
            character,
            Some(b"x")
        ));
        assert!(!copy_mode_should_exit_for_pty_bytes(
            false,
            &character_key,
            character,
            Some(b"x")
        ));

        for modifier in [
            KeyCode::ShiftLeft,
            KeyCode::ShiftRight,
            KeyCode::ControlLeft,
            KeyCode::ControlRight,
            KeyCode::AltLeft,
            KeyCode::AltRight,
            KeyCode::SuperLeft,
            KeyCode::SuperRight,
            KeyCode::CapsLock,
            KeyCode::Fn,
            KeyCode::FnLock,
            KeyCode::NumLock,
            KeyCode::ScrollLock,
        ] {
            assert!(!copy_mode_should_exit_for_pty_bytes(
                true,
                &character_key,
                PhysicalKey::Code(modifier),
                Some(b"\x1b[57441u")
            ));
        }

        assert!(!copy_mode_should_exit_for_pty_bytes(
            true,
            &Key::Named(NamedKey::Shift),
            character,
            Some(b"\x1b[57441u")
        ));
    }

    #[test]
    fn alt_screen_directional_action_bypasses_super_key_swallow() {
        assert!(copy_mode_directional_action_passes_through_on_alt(
            AppCommand::CopyMode(CopyModeAction::Extend(noa_grid::CopyDirection::Right)),
            true
        ));
        assert!(!copy_mode_directional_action_passes_through_on_alt(
            AppCommand::CopyMode(CopyModeAction::CursorOnly),
            true
        ));
        assert!(!copy_mode_directional_action_passes_through_on_alt(
            AppCommand::CopyMode(CopyModeAction::Extend(noa_grid::CopyDirection::Right)),
            false
        ));
        assert!(!copy_mode_should_swallow_super_key(
            ModifiersState::SUPER,
            true
        ));
        assert!(copy_mode_should_swallow_super_key(
            ModifiersState::SUPER,
            false
        ));
        assert!(!copy_mode_should_swallow_super_key(
            ModifiersState::empty(),
            false
        ));
    }

    #[test]
    fn directional_start_rechecks_alt_screen_under_terminal_lock() {
        let mut terminal = Terminal::new(noa_core::GridSize::new(4, 2));
        noa_vt::Stream::new().feed(b"\x1b[?1049h", &mut terminal);
        assert!(terminal.active_is_alt);

        assert!(
            copy_mode_state_for_start(
                &mut terminal,
                CopyModeAction::Extend(noa_grid::CopyDirection::Right)
            )
            .is_none()
        );
    }

    #[test]
    fn terminal_coordinate_changes_exit_copy_mode_before_dispatch() {
        for action in [
            TerminalAction::Clear,
            TerminalAction::ClearScrollback,
            TerminalAction::SelectAll,
        ] {
            assert!(command_invalidates_copy_mode_state(AppCommand::Terminal(
                action
            )));
        }
        assert!(!command_invalidates_copy_mode_state(AppCommand::Copy));
    }

    #[test]
    fn command_transition_is_independent_of_command_origin() {
        for command in [
            AppCommand::Search(SearchAction::Find),
            AppCommand::Terminal(TerminalAction::SelectAll),
            AppCommand::Terminal(TerminalAction::Clear),
            AppCommand::Terminal(TerminalAction::ClearScrollback),
        ] {
            assert_eq!(
                copy_mode_command_transition(command),
                CopyModeCommandTransition::Exit
            );
        }
        assert_eq!(
            copy_mode_command_transition(AppCommand::SendSelectionToPane),
            CopyModeCommandTransition::CaptureSelectionAndExit
        );
        assert_eq!(
            copy_mode_command_transition(AppCommand::Copy),
            CopyModeCommandTransition::RepairAndCaptureSelection
        );
        assert_eq!(
            copy_mode_command_transition(AppCommand::ScrollViewport(ViewportScroll::PageUp)),
            CopyModeCommandTransition::Preserve
        );
    }

    #[test]
    fn ime_exit_decision_preserves_modal_commits_and_targets_pty_commits() {
        let commit = Ime::Commit("日本語".into());
        assert!(!App::ime_commit_should_end_copy_mode(true, &commit, true));
        assert!(App::ime_commit_should_end_copy_mode(false, &commit, true));
        assert!(!App::ime_commit_should_end_copy_mode(
            false,
            &Ime::Preedit("日本".into(), None),
            false
        ));
        assert!(!App::ime_commit_should_end_copy_mode(
            false,
            &Ime::Commit(String::new()),
            false
        ));
    }

    #[test]
    fn central_terminal_exit_clears_selection_and_viewport_lock() {
        let terminal = Arc::new(Mutex::new(Terminal::new(GridSize::new(8, 2))));
        {
            let mut terminal = terminal.lock();
            Stream::new().feed(b"abcd", &mut *terminal);
            let mut state = noa_grid::CopyModeState::enter(&mut terminal).expect("copy mode");
            assert!(state.move_cursor(&mut terminal, noa_grid::CopyDirection::Left, true));
            assert!(terminal.active().selection.is_some());
            assert!(terminal.primary.viewport_locked());
        }

        assert!(!exit_copy_mode_terminal(&terminal));

        let terminal = terminal.try_lock().expect("exit releases terminal lock");
        assert_eq!(terminal.active().selection, None);
        assert!(!terminal.primary.viewport_locked());
    }

    #[test]
    fn terminal_exit_reports_live_bottom_viewport_change() {
        let terminal = Arc::new(Mutex::new(Terminal::new(GridSize::new(8, 2))));
        {
            let mut terminal = terminal.lock();
            Stream::new().feed(b"old\r\nmiddle\r\nlatest", &mut *terminal);
            terminal.scroll_viewport_to_top();
            assert!(terminal.viewport_offset() > 0);
            noa_grid::CopyModeState::enter(&mut terminal).expect("copy mode");
        }

        assert!(exit_copy_mode_terminal(&terminal));
        assert_eq!(terminal.lock().viewport_offset(), 0);
    }

    #[test]
    fn enter_capture_repairs_eviction_and_reasserts_selection_under_one_lock() {
        let mut terminal = Terminal::new(GridSize::new(8, 2));
        Stream::new().feed(b"oldest\r\nmiddle\r\nlatest", &mut terminal);
        terminal.scroll_viewport_to_top();
        let mut state = noa_grid::CopyModeState::enter(&mut terminal).expect("copy mode");
        assert!(state.move_cursor(&mut terminal, noa_grid::CopyDirection::Up, false));
        assert!(state.move_cursor(&mut terminal, noa_grid::CopyDirection::Left, true));

        let evicted_before = terminal.selection_rows_evicted();
        terminal.set_scrollback_limit_bytes(0);
        assert!(terminal.selection_rows_evicted() > evicted_before);
        terminal.clear_selection();
        assert_eq!(terminal.selected_text(), None);

        let captured = repair_and_capture_copy_mode_selection(&mut state, &mut terminal);

        assert!(captured.is_some());
        assert_eq!(terminal.selected_text(), captured);
        assert_eq!(
            terminal.active().selection,
            Some(noa_grid::Selection::new(
                state.anchor().expect("selection anchor"),
                state.cursor()
            ))
        );
    }

    #[test]
    fn enter_capture_without_selection_returns_none() {
        let mut terminal = Terminal::new(GridSize::new(8, 2));
        let mut state = noa_grid::CopyModeState::enter(&mut terminal).expect("copy mode");

        assert_eq!(
            repair_and_capture_copy_mode_selection(&mut state, &mut terminal),
            None
        );
    }

    #[test]
    fn captured_selection_survives_copy_mode_terminal_exit() {
        let mut terminal = Terminal::new(GridSize::new(8, 2));
        Stream::new().feed(b"abcd", &mut terminal);
        terminal.primary.cursor.x = 0;
        let mut state = noa_grid::CopyModeState::enter(&mut terminal).expect("copy mode");
        assert!(state.move_cursor(&mut terminal, noa_grid::CopyDirection::Right, true));

        let captured = repair_and_capture_copy_mode_selection(&mut state, &mut terminal);
        terminal.exit_copy_mode();

        assert_eq!(captured.as_deref(), Some("ab"));
        assert_eq!(terminal.selected_text(), None);
    }
}
