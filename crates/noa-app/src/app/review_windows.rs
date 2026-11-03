//! Ghostty 1.4.0's "Review Windows": quitting, or closing a window, while
//! several tabs still run programs first asks whether to review them. The
//! review then confirms each such tab in turn; cancelling any step stops
//! there. Ghostty reviews windows; a Noa tab is what Ghostty calls a window
//! on macOS (one `NSWindow` per tab).
//!
//! Closing a window closes each confirmed tab at once and, as in Ghostty,
//! leaves tabs running nothing open. Quitting closes nothing until every tab
//! is confirmed and Noa exits, unlike Ghostty: a tab closed on the way would
//! drop out of the session Noa saves at exit, scrollback records included.

use super::*;

/// The first prompt, naming how many tabs run programs. Ghostty's alert
/// text is shortened to fit the confirm card's single line.
pub(super) fn review_windows_prompt(review: Review, tabs: usize) -> (String, String) {
    let (verb, end) = match review {
        Review::Quit => ("quitting", "terminate all"),
        Review::CloseWindow { .. } => ("closing", "close all"),
    };
    (
        format!("{tabs} windows run processes. Review them before {verb}?"),
        format!("Enter: review    T: {end}    Esc: cancel"),
    )
}

/// The prompt for one reviewed tab.
pub(super) fn review_tab_prompt(review: Review) -> (String, String) {
    match review {
        Review::Quit => (
            "This window runs a process. Quit Noa and kill it?".to_string(),
            "Enter: terminate    Esc: cancel".to_string(),
        ),
        Review::CloseWindow { .. } => (
            "Close this window? Its sessions will be terminated.".to_string(),
            "Enter: close    Esc: cancel".to_string(),
        ),
    }
}

impl App {
    /// The tabs in `tabs` that still run a program, in order.
    pub(super) fn tabs_with_running_programs(&self, tabs: &[WindowId]) -> Vec<WindowId> {
        tabs.iter()
            .copied()
            .filter(|tab| self.tab_running_program_count(*tab) > 0)
            .collect()
    }

    /// Ask whether to review `tabs` before `review` goes ahead. The prompt
    /// opens over `window_id` when it is a tab, else over the first of
    /// `tabs` (the quick terminal is none), brought forward either way so
    /// it is drawn and gets the keys.
    pub(super) fn open_review_windows(
        &mut self,
        window_id: WindowId,
        review: Review,
        tabs: Vec<WindowId>,
    ) {
        let target = if self.window_order.contains(&window_id) {
            window_id
        } else {
            tabs[0]
        };
        self.focus_tab(target);
        let (message, hint) = review_windows_prompt(review, tabs.len());
        self.open_confirm_dialog_with_hint(
            target,
            message,
            hint,
            ConfirmAction::ReviewWindows { review, tabs },
            None,
        );
    }

    /// Bring the next still-open tab of `tabs` forward and confirm it; once
    /// none is left, a quit goes ahead.
    pub(super) fn review_next_tab(
        &mut self,
        event_loop: &ActiveEventLoop,
        review: Review,
        tabs: Vec<WindowId>,
    ) {
        let mut tabs = tabs
            .into_iter()
            .filter(|tab| self.windows.contains_key(tab));
        let Some(tab) = tabs.next() else {
            if review == Review::Quit {
                event_loop.exit();
            }
            return;
        };
        let rest = tabs.collect();
        self.focus_tab(tab);
        // A tab just closed on macOS restores focus on the next turn to the
        // tab AppKit picked, skipped once `tab` is focused instead; restore
        // `tab` the same way, so its content view takes the keys again.
        #[cfg(target_os = "macos")]
        let _ = self
            .proxy
            .send_event(crate::events::UserEvent::RestoreFocus { window_id: tab });
        let (message, hint) = review_tab_prompt(review);
        self.open_confirm_dialog_with_hint(
            tab,
            message,
            hint,
            ConfirmAction::ReviewTab { review, tab, rest },
            None,
        );
    }

    /// Skip the review: quit, or close the whole window (Ghostty's
    /// "Close" button).
    pub(super) fn end_reviewed_tabs(&mut self, event_loop: &ActiveEventLoop, review: Review) {
        match review {
            Review::Quit => event_loop.exit(),
            Review::CloseWindow { group } => self.close_group(event_loop, group),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_review_prompts_name_the_count_and_the_keys() {
        let (message, hint) = review_windows_prompt(Review::Quit, 3);
        assert_eq!(
            message,
            "3 windows run processes. Review them before quitting?"
        );
        assert_eq!(hint, "Enter: review    T: terminate all    Esc: cancel");
        let (message, hint) = review_windows_prompt(
            Review::CloseWindow {
                group: WindowGroupId(1),
            },
            2,
        );
        assert!(message.ends_with("before closing?"));
        assert!(hint.contains("T: close all"));
        assert!(
            review_tab_prompt(Review::Quit)
                .0
                .starts_with("This window runs a process.")
        );
        assert!(
            review_tab_prompt(Review::CloseWindow {
                group: WindowGroupId(1)
            })
            .0
            .starts_with("Close this window?")
        );
    }
}
