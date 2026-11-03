//! Ghostty 1.4.0's "Review Windows": quitting, or closing a window, while
//! several tabs still run programs first asks whether to review them. The
//! review then confirms each such tab in turn, closing the ones confirmed;
//! cancelling any step stops there. Ghostty reviews windows; a Noa tab is
//! what Ghostty calls a window on macOS (one `NSWindow` per tab).

use super::*;

/// The first prompt, naming how many tabs run programs. Ghostty's alert
/// text is shortened to fit the confirm card's single line.
pub(super) fn review_windows_prompt(review: Review, tabs: usize) -> (String, String) {
    let (verb, end) = match review {
        Review::Quit => ("quitting", "terminate all"),
        Review::CloseWindow => ("closing", "close all"),
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
        Review::CloseWindow => (
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

    /// Ask whether to review `tabs` before `review` goes ahead.
    pub(super) fn open_review_windows(
        &mut self,
        window_id: WindowId,
        review: Review,
        tabs: Vec<WindowId>,
    ) {
        let (message, hint) = review_windows_prompt(review, tabs.len());
        self.open_confirm_dialog_with_hint(
            window_id,
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
        let (message, hint) = review_tab_prompt(review);
        self.open_confirm_dialog_with_hint(
            tab,
            message,
            hint,
            ConfirmAction::ReviewTab { review, tab, rest },
            None,
        );
    }

    /// Skip the review: quit, or close every reviewed tab.
    pub(super) fn end_reviewed_tabs(
        &mut self,
        event_loop: &ActiveEventLoop,
        review: Review,
        tabs: &[WindowId],
    ) {
        match review {
            Review::Quit => event_loop.exit(),
            Review::CloseWindow => {
                for tab in tabs {
                    if self.windows.contains_key(tab) {
                        self.close_tab(event_loop, *tab);
                    }
                }
            }
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
        let (message, hint) = review_windows_prompt(Review::CloseWindow, 2);
        assert!(message.ends_with("before closing?"));
        assert!(hint.contains("T: close all"));
        assert!(
            review_tab_prompt(Review::Quit)
                .0
                .starts_with("This window runs a process.")
        );
        assert!(
            review_tab_prompt(Review::CloseWindow)
                .0
                .starts_with("Close this window?")
        );
    }
}
