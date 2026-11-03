//! What a pane shows when its child process exits (Ghostty 1.4.0's
//! `Surface.childExited` and the macOS `ChildExitedMessageBar`): a child that
//! exits within `abnormal-command-exit-runtime` failed to launch, so its pane
//! stays open with an error; otherwise the pane closes, unless `wait after
//! command` holds it open. On macOS the notice is a bar along the pane's
//! bottom edge; elsewhere it is printed into the terminal, as Ghostty does
//! when it has no native notice to show.

use super::*;

/// How the child exited: its runtime and, when it reported one, its code.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct ChildExit {
    pub(super) runtime_ms: u64,
    pub(super) exit_code: Option<i32>,
}

impl ChildExit {
    /// Exited too fast to have launched. On macOS any exit code counts, as
    /// in Ghostty (whose login wrapper hides the real one there).
    pub(super) fn abnormal(self, threshold_ms: u32) -> bool {
        self.runtime_ms <= u64::from(threshold_ms)
            && (cfg!(target_os = "macos") || self.exit_code != Some(0))
    }
}

/// The macOS bar's content (`Ghostty.ChildExitedMessage`).
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) struct ChildExitedMessage {
    pub(crate) text: String,
    /// Char indices drawn bold (the runtime).
    pub(crate) bold: std::ops::Range<usize>,
    pub(crate) error: bool,
}

impl ChildExitedMessage {
    pub(super) fn new(exit: ChildExit, threshold_ms: u32) -> Self {
        let failed = exit.exit_code != Some(0);
        if exit.runtime_ms <= u64::from(threshold_ms) {
            let prefix = "Process exited after ";
            let runtime = bar_runtime(exit.runtime_ms);
            let start = prefix.chars().count();
            Self {
                bold: start..start + runtime.chars().count(),
                text: format!("{prefix}{runtime}. Press any key to close the terminal."),
                error: true,
            }
        } else {
            Self {
                text: "Process exited. Press any key to close the terminal.".to_string(),
                bold: 0..0,
                error: failed,
            }
        }
    }
}

/// The runtime as Ghostty's `MeasurementFormatter` prints it: milliseconds up
/// to a second, then seconds.
fn bar_runtime(ms: u64) -> String {
    if ms <= 1000 {
        return format!("{ms} ms");
    }
    let secs = format!("{:.3}", ms as f64 / 1000.0);
    format!("{} sec", secs.trim_end_matches('0').trim_end_matches('.'))
}

/// The in-terminal error for an abnormal exit (`Surface.childExitedAbnormally`),
/// as bytes to feed the pane's terminal. `rule_cols` draws a rule first when
/// the screen already shows the command's own output.
pub(super) fn abnormal_exit_text(
    command: &str,
    exit: ChildExit,
    rule_cols: Option<u16>,
) -> Vec<u8> {
    let mut text = String::from("\r\x1b[0m");
    if let Some(cols) = rule_cols {
        text.push('\n');
        text.extend(std::iter::repeat_n('\u{2501}', usize::from(cols)));
        text.push_str("\r\n\n");
    }
    text.push_str("\x1b[91;1mNoa failed to launch the requested command:\x1b[0m");
    text.push_str(&format!("\r\n\n{command}\x1b[0m"));
    text.push_str(&format!(
        "\r\n\nRuntime: \x1b[31m{} ms\x1b[0m",
        exit.runtime_ms
    ));
    // Not on macOS, matching Ghostty, whose exit code is unreliable there.
    if !cfg!(target_os = "macos")
        && let Some(code) = exit.exit_code
    {
        text.push_str(&format!("\r\nExit Code: \x1b[31m{code}\x1b[0m"));
    }
    text.push_str("\r\n\nPress any key to close the window.");
    text.into_bytes()
}

/// What `wait after command` prints when the bar is unavailable
/// (agent-automation REQ-005).
const PROCESS_EXITED_MESSAGE: &[u8] =
    b"\r\n\x1b[0m[Process exited. Press any key to close the terminal.]";

impl App {
    /// Handle `pane_id`'s child exiting with `exit_code`: keep the pane open
    /// with a notice when it failed to launch or `wait after command` holds
    /// it, else close it.
    pub(super) fn on_child_exit(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        pane_id: PaneId,
        exit_code: Option<i32>,
    ) {
        if !self.hold_pane_after_exit(window_id, pane_id, exit_code) {
            self.close_exited_pane(event_loop, window_id, pane_id);
        }
    }

    /// Mark `pane_id` exited and show its notice when it stays open; `false`
    /// when it should close instead.
    fn hold_pane_after_exit(
        &mut self,
        window_id: WindowId,
        pane_id: PaneId,
        exit_code: Option<i32>,
    ) -> bool {
        let threshold_ms = self.config.abnormal_command_exit_runtime_ms;
        let Some(surface) = self
            .windows
            .get_mut(&window_id)
            .and_then(|state| state.surfaces.get_mut(&pane_id))
        else {
            return false;
        };
        if surface.exited {
            return false;
        }
        let Some((started_at, command)) = surface.child_started.as_ref() else {
            return false;
        };
        let exit = ChildExit {
            runtime_ms: u64::try_from(started_at.elapsed().as_millis()).unwrap_or(u64::MAX),
            exit_code,
        };
        let abnormal = exit.abnormal(threshold_ms);
        if !abnormal && !surface.wait_after_command {
            return false;
        }
        surface.exited = true;
        // Ghostty skips the native notice for a zero runtime (an exit code
        // from launch) and prints into the terminal instead.
        if cfg!(target_os = "macos") && exit.runtime_ms > 0 {
            surface.child_exited = Some(ChildExitedMessage::new(exit, threshold_ms));
        } else {
            let mut terminal = surface.terminal.lock();
            let bytes = if abnormal {
                let screen = terminal.active();
                let shows_output = screen
                    .visible_rows()
                    .iter()
                    .any(|row| row.cells.iter().any(|cell| cell.ch != ' '));
                abnormal_exit_text(command, exit, shows_output.then_some(screen.cols))
            } else {
                PROCESS_EXITED_MESSAGE.to_vec()
            };
            noa_vt::Stream::new().feed(&bytes, &mut *terminal);
        }
        self.request_window_redraw(window_id);
        true
    }

    /// Close `pane_id` after its child exited. The quick terminal and the
    /// scratch popup aren't tabbed windows, so their child exiting tears the
    /// whole window down instead.
    pub(super) fn close_exited_pane(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        pane_id: PaneId,
    ) {
        if self.is_quick_terminal_window(window_id) {
            self.end_copy_mode_for_window(window_id);
            self.destroy_quick_terminal();
        } else if self.is_scratch_terminal_window(window_id) {
            // scratch-terminal R4-c: the popup's own shell exiting (e.g.
            // `exit`) tears the whole popup down, same as the quick terminal.
            self.destroy_scratch_terminal();
        } else {
            self.close_pane_after_pty_exit(event_loop, window_id, pane_id)
        }
    }

    /// The exited pane a key press in `window_id` should close, if the
    /// focused pane is being held open after its process exited.
    pub(super) fn exited_focused_pane(&self, window_id: WindowId) -> Option<PaneId> {
        let state = self.windows.get(&window_id)?;
        state
            .surfaces
            .get(&state.focused_pane)
            .is_some_and(|surface| surface.exited)
            .then_some(state.focused_pane)
    }
}

#[cfg(target_os = "macos")]
impl App {
    /// The bars of `window_id`'s exited panes in `panes`, in window points,
    /// each with its pane.
    fn child_exited_bars_in(
        &self,
        window_id: WindowId,
        panes: impl Iterator<Item = PaneId>,
    ) -> Vec<(PaneId, crate::macos_overlay::ChildExitedBar)> {
        let (Some(gpu), Some(state)) = (self.gpu.as_ref(), self.windows.get(&window_id)) else {
            return Vec::new();
        };
        let scale = state.window.scale_factor();
        let cell_h = f64::from(gpu.fonts.get(state.font_px).metrics().cell_h) / scale;
        panes
            .filter_map(|pane_id| {
                let surface = state.surfaces.get(&pane_id)?;
                let message = surface.child_exited.as_ref()?;
                let rect = surface.rect;
                let pane = crate::macos_overlay::PaneRectPt::from_px(
                    rect.x, rect.y, rect.w, rect.h, scale,
                );
                Some((
                    pane_id,
                    crate::macos_overlay::ChildExitedBar::new(
                        pane,
                        cell_h,
                        message.text.clone(),
                        message.bold.clone(),
                        message.error,
                    ),
                ))
            })
            .collect()
    }

    /// The pane whose bar is under the pointer in `window_id`.
    fn child_exited_bar_under_pointer(
        &self,
        window_id: WindowId,
        bars: &[(PaneId, crate::macos_overlay::ChildExitedBar)],
    ) -> Option<PaneId> {
        let state = self.windows.get(&window_id)?;
        let position = state.last_mouse_physical_position?;
        let scale = state.window.scale_factor();
        bars.iter()
            .find(|(_, bar)| bar.contains(position.x / scale, position.y / scale))
            .map(|(pane_id, _)| *pane_id)
    }

    /// The bars to draw over `visible` panes: all but the hovered one.
    pub(super) fn visible_child_exited_bars(
        &self,
        window_id: WindowId,
        visible: impl Iterator<Item = PaneId>,
    ) -> Vec<crate::macos_overlay::ChildExitedBar> {
        let bars = self.child_exited_bars_in(window_id, visible);
        let hovered = self.child_exited_bar_under_pointer(window_id, &bars);
        bars.into_iter()
            .filter(|(pane_id, _)| Some(*pane_id) != hovered)
            .map(|(_, bar)| bar)
            .collect()
    }

    /// Redraw `window_id` when the pointer moves onto or off a bar.
    pub(super) fn sync_child_exited_hover(&mut self, window_id: WindowId) {
        let hovered = self.windows.get(&window_id).and_then(|state| {
            let panes: Vec<PaneId> = state.surfaces.keys().copied().collect();
            let bars = self.child_exited_bars_in(window_id, panes.into_iter());
            self.child_exited_bar_under_pointer(window_id, &bars)
        });
        let Some(state) = self.windows.get_mut(&window_id) else {
            return;
        };
        if state.native_overlays.child_exited_hovered != hovered {
            state.native_overlays.child_exited_hovered = hovered;
            self.request_window_redraw(window_id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exit(runtime_ms: u64, exit_code: Option<i32>) -> ChildExit {
        ChildExit {
            runtime_ms,
            exit_code,
        }
    }

    #[test]
    fn a_fast_exit_is_abnormal_and_a_slow_one_is_not() {
        assert!(exit(250, Some(1)).abnormal(250));
        assert!(!exit(251, Some(1)).abnormal(250));
        assert!(exit(0, None).abnormal(0));
        // A clean fast exit is abnormal only on macOS, as in Ghostty.
        assert_eq!(exit(10, Some(0)).abnormal(250), cfg!(target_os = "macos"));
    }

    #[test]
    fn the_bar_names_the_runtime_of_a_fast_exit() {
        let message = ChildExitedMessage::new(exit(42, Some(0)), 250);
        assert_eq!(
            message.text,
            "Process exited after 42 ms. Press any key to close the terminal."
        );
        let bold: String = message
            .text
            .chars()
            .skip(message.bold.start)
            .take(message.bold.len())
            .collect();
        assert_eq!(bold, "42 ms");
        assert!(message.error);
    }

    #[test]
    fn the_bar_after_a_normal_exit_follows_the_exit_code() {
        let success = ChildExitedMessage::new(exit(5000, Some(0)), 250);
        assert_eq!(
            success.text,
            "Process exited. Press any key to close the terminal."
        );
        assert!(!success.error);
        assert!(ChildExitedMessage::new(exit(5000, Some(2)), 250).error);
        assert!(ChildExitedMessage::new(exit(5000, None), 250).error);
    }

    #[test]
    fn bar_runtime_switches_to_seconds_past_one_second() {
        assert_eq!(bar_runtime(1000), "1000 ms");
        assert_eq!(bar_runtime(1500), "1.5 sec");
        assert_eq!(bar_runtime(2000), "2 sec");
    }

    #[test]
    fn abnormal_exit_text_reports_the_command_and_runtime() {
        let mut terminal = Terminal::new(GridSize::new(60, 10));
        noa_vt::Stream::new().feed(
            &abnormal_exit_text("/bin/false -x", exit(12, Some(1)), None),
            &mut terminal,
        );
        let text: Vec<String> = terminal
            .active()
            .visible_rows()
            .iter()
            .map(|row| row.cells.iter().map(|cell| cell.ch).collect::<String>())
            .map(|row| row.trim_end().to_string())
            .collect();
        assert_eq!(text[0], "Noa failed to launch the requested command:");
        assert_eq!(text[2], "/bin/false -x");
        assert_eq!(text[4], "Runtime: 12 ms");
        let (code_row, close_row) = if cfg!(target_os = "macos") {
            (None, 6)
        } else {
            (Some(5), 7)
        };
        if let Some(row) = code_row {
            assert_eq!(text[row], "Exit Code: 1");
        }
        assert_eq!(text[close_row], "Press any key to close the window.");
    }

    #[test]
    fn abnormal_exit_text_rules_off_earlier_output() {
        let mut terminal = Terminal::new(GridSize::new(50, 12));
        noa_vt::Stream::new().feed(b"oops", &mut terminal);
        noa_vt::Stream::new().feed(
            &abnormal_exit_text("x", exit(1, Some(1)), Some(50)),
            &mut terminal,
        );
        let rows = terminal.active().visible_rows();
        let text: Vec<String> = rows
            .iter()
            .map(|row| row.cells.iter().map(|cell| cell.ch).collect::<String>())
            .map(|row| row.trim_end().to_string())
            .collect();
        assert_eq!(text[0], "oops");
        assert_eq!(text[1], "\u{2501}".repeat(50), "{text:?}");
        assert_eq!(text[2], "");
        assert_eq!(text[3], "Noa failed to launch the requested command:");
    }
}
