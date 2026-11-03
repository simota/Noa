//! What a pane shows when its child process exits (Ghostty 1.4.0's
//! `Surface.childExited` and the macOS `ChildExitedMessageBar`): a child that
//! exits with a failure within `abnormal-command-exit-runtime` failed to
//! launch, so its pane stays open with an error; otherwise the pane closes,
//! unless `wait after command` holds it open. On macOS the error is a red bar
//! along the pane's bottom edge; elsewhere it is printed into the terminal, as
//! Ghostty does when it has no native notice to show.
//!
//! Unlike Ghostty on macOS, the exit code counts there too: Ghostty ignores
//! it because its `login` wrapper hides it, but Noa spawns the child
//! directly and gets the real one.

use super::*;
use crate::events::ChildExit;

/// Exited with a failure too fast to have launched. A missing exit code
/// (the pty read failed and the waiter never reported) counts as a failure.
pub(super) fn abnormal(exit: ChildExit, threshold_ms: u32) -> bool {
    exit.runtime_ms <= u64::from(threshold_ms) && exit.exit_code != Some(0)
}

/// The macOS bar's content (`Ghostty.ChildExitedMessage` for a failed
/// launch).
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) struct ChildExitedMessage {
    pub(crate) text: String,
    /// Char indices drawn bold (the runtime).
    pub(crate) bold: std::ops::Range<usize>,
}

impl ChildExitedMessage {
    pub(super) fn new(exit: ChildExit) -> Self {
        let prefix = "Process exited after ";
        let runtime = bar_runtime(exit.runtime_ms);
        let start = prefix.chars().count();
        Self {
            bold: start..start + runtime.chars().count(),
            text: format!("{prefix}{runtime}. Press any key to close the terminal."),
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
    // Printed, never interpreted: an argv may carry escape sequences.
    let command: String = command
        .chars()
        .map(|c| if c.is_control() { '\u{fffd}' } else { c })
        .collect();
    text.push_str(&format!("\r\n\n{command}\x1b[0m"));
    text.push_str(&format!(
        "\r\n\nRuntime: \x1b[31m{} ms\x1b[0m",
        exit.runtime_ms
    ));
    if let Some(code) = exit.exit_code {
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
    /// Handle `pane_id`'s child exiting: keep the pane open with a notice
    /// when it failed to launch or `wait after command` holds it, else close
    /// it.
    pub(super) fn on_child_exit(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        pane_id: PaneId,
        exit: ChildExit,
    ) {
        if !self.hold_pane_after_exit(window_id, pane_id, exit) {
            self.close_exited_pane(event_loop, window_id, pane_id);
        }
    }

    /// Mark `pane_id` exited and show its notice when it stays open; `false`
    /// when it should close instead.
    fn hold_pane_after_exit(
        &mut self,
        window_id: WindowId,
        pane_id: PaneId,
        exit: ChildExit,
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
        let Some(command) = surface.child_command.as_deref() else {
            return false;
        };
        let abnormal = abnormal(exit, threshold_ms);
        if !abnormal && !surface.wait_after_command {
            return false;
        }
        surface.exited = true;
        // Ghostty skips the native notice for a zero runtime (an exit code
        // from launch) and prints into the terminal instead.
        if abnormal && cfg!(target_os = "macos") && exit.runtime_ms > 0 {
            surface.child_exited = Some(ChildExitedMessage::new(exit));
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
                // `wait after command` keeps its notice in the terminal, where
                // a script reads it back (agent-automation REQ-005).
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
    /// The bars of `window_id`'s visible exited panes, in window points,
    /// with the pane whose bar is under the pointer. That bar is left out,
    /// as Ghostty hides it on hover so the text under it shows.
    pub(super) fn visible_child_exited_bars(
        &self,
        window_id: WindowId,
    ) -> (Vec<crate::macos_overlay::ChildExitedBar>, Option<PaneId>) {
        let (Some(gpu), Some(state)) = (self.gpu.as_ref(), self.windows.get(&window_id)) else {
            return (Vec::new(), None);
        };
        let scale = state.window.scale_factor();
        let cell_h = f64::from(gpu.fonts.get(state.font_px).metrics().cell_h) / scale;
        let bars = visible_pane_ids(&state.split_tree, state.zoomed)
            .into_iter()
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
                    ),
                ))
            })
            .collect::<Vec<_>>();
        let hovered = state.last_mouse_physical_position.and_then(|position| {
            bars.iter()
                .find(|(_, bar)| bar.contains(position.x / scale, position.y / scale))
                .map(|(pane_id, _)| *pane_id)
        });
        let bars = bars
            .into_iter()
            .filter(|(pane_id, _)| Some(*pane_id) != hovered)
            .map(|(_, bar)| bar)
            .collect();
        (bars, hovered)
    }

    /// Redraw `window_id` when the pointer moves onto or off a bar. The
    /// redraw records the hovered pane it drew with.
    pub(super) fn sync_child_exited_hover(&mut self, window_id: WindowId) {
        let (_, hovered) = self.visible_child_exited_bars(window_id);
        let changed = self
            .windows
            .get(&window_id)
            .is_some_and(|state| state.native_overlays.child_exited_hovered != hovered);
        if changed {
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
    fn a_fast_failure_is_abnormal_and_anything_else_is_not() {
        assert!(abnormal(exit(250, Some(1)), 250));
        assert!(abnormal(exit(0, None), 0));
        assert!(!abnormal(exit(251, Some(1)), 250));
        // A clean fast exit launched fine (`/bin/echo done`).
        assert!(!abnormal(exit(10, Some(0)), 250));
    }

    #[test]
    fn the_bar_names_the_runtime() {
        let message = ChildExitedMessage::new(exit(42, Some(1)));
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
    }

    #[test]
    fn bar_runtime_switches_to_seconds_past_one_second() {
        assert_eq!(bar_runtime(1000), "1000 ms");
        assert_eq!(bar_runtime(1500), "1.5 sec");
        assert_eq!(bar_runtime(2000), "2 sec");
    }

    fn screen_text(terminal: &Terminal) -> Vec<String> {
        terminal
            .active()
            .visible_rows()
            .iter()
            .map(|row| row.cells.iter().map(|cell| cell.ch).collect::<String>())
            .map(|row| row.trim_end().to_string())
            .collect()
    }

    #[test]
    fn abnormal_exit_text_reports_the_command_runtime_and_code() {
        let mut terminal = Terminal::new(GridSize::new(60, 10));
        noa_vt::Stream::new().feed(
            &abnormal_exit_text("/bin/false -x", exit(12, Some(1)), None),
            &mut terminal,
        );
        let text = screen_text(&terminal);
        assert_eq!(text[0], "Noa failed to launch the requested command:");
        assert_eq!(text[2], "/bin/false -x");
        assert_eq!(text[4], "Runtime: 12 ms");
        assert_eq!(text[5], "Exit Code: 1");
        assert_eq!(text[7], "Press any key to close the window.");
    }

    #[test]
    fn abnormal_exit_text_prints_control_characters_instead_of_running_them() {
        let mut terminal = Terminal::new(GridSize::new(60, 10));
        noa_vt::Stream::new().feed(
            &abnormal_exit_text("a\x1b]0;pwned\x07b", exit(1, Some(1)), None),
            &mut terminal,
        );
        assert_eq!(screen_text(&terminal)[2], "a\u{fffd}]0;pwned\u{fffd}b");
        assert_ne!(terminal.title, "pwned");
    }

    #[test]
    fn abnormal_exit_text_rules_off_earlier_output() {
        let mut terminal = Terminal::new(GridSize::new(50, 12));
        noa_vt::Stream::new().feed(b"oops", &mut terminal);
        noa_vt::Stream::new().feed(
            &abnormal_exit_text("x", exit(1, Some(1)), Some(50)),
            &mut terminal,
        );
        let text = screen_text(&terminal);
        assert_eq!(text[0], "oops");
        assert_eq!(text[1], "\u{2501}".repeat(50), "{text:?}");
        assert_eq!(text[2], "");
        assert_eq!(text[3], "Noa failed to launch the requested command:");
    }
}
