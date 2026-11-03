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

/// The prompt for one reviewed tab; `quick_terminal` when it is the quick
/// terminal, which the prompt can't be shown over.
pub(super) fn review_tab_prompt(review: Review, quick_terminal: bool) -> (String, String) {
    match review {
        Review::Quit if quick_terminal => (
            "The Quick Terminal runs a process. Quit Noa and kill it?".to_string(),
            "Enter: terminate    Esc: cancel".to_string(),
        ),
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

    /// The tabs a quit reviews: every tab running a program, then the quick
    /// terminal when it runs one (Ghostty reviews it like any window).
    pub(super) fn quit_review_tabs(&self) -> Vec<WindowId> {
        let mut tabs = self.tabs_with_running_programs(&self.window_order);
        if let Some(quick) = self.quick_terminal.as_ref().map(|qt| qt.window_id)
            && self.tab_running_program_count(quick) > 0
        {
            tabs.push(quick);
        }
        tabs
    }

    /// Bring `tab` forward for a review prompt. A tab just closed on macOS
    /// restores focus on the next turn to the tab AppKit picked, skipped
    /// once `tab` is focused instead; restore `tab` the same way, so its
    /// content view takes the keys again.
    fn present_review_tab(&mut self, tab: WindowId) {
        self.focus_tab(tab);
        #[cfg(target_os = "macos")]
        let _ = self
            .proxy
            .send_event(crate::events::UserEvent::RestoreFocus { window_id: tab });
    }

    /// A regular tab to show a prompt over: `preferred` when it is one, else
    /// the first such tab of `tabs`, else the frontmost tab.
    fn review_anchor(&self, preferred: WindowId, tabs: &[WindowId]) -> Option<WindowId> {
        let regular = |tab: &WindowId| self.window_order.contains(tab);
        Some(preferred)
            .filter(regular)
            .or_else(|| tabs.iter().copied().find(regular))
            .or_else(|| self.focused.filter(regular))
            .or_else(|| self.window_order.last().copied())
    }

    /// Ask whether to review `tabs` before `review` goes ahead, over
    /// `window_id` when it is a regular tab (not the quick terminal), else
    /// over the first such tab of `tabs`, brought forward either way so it
    /// is drawn and gets the keys.
    pub(super) fn open_review_windows(
        &mut self,
        window_id: WindowId,
        review: Review,
        tabs: Vec<WindowId>,
    ) {
        let Some(target) = self.review_anchor(window_id, &tabs) else {
            return;
        };
        self.present_review_tab(target);
        let (message, hint) = review_windows_prompt(review, tabs.len());
        self.open_confirm_dialog_with_hint(
            target,
            message,
            hint,
            ConfirmAction::ReviewWindows { review, tabs },
            None,
        );
    }

    /// Whether `tab` still needs `review`: open, running a program, and for
    /// a closing window still one of its tabs (it may have been dragged out).
    fn still_under_review(&self, review: Review, tab: WindowId) -> bool {
        let Some(state) = self.windows.get(&tab) else {
            return false;
        };
        let in_scope = match review {
            Review::Quit => true,
            Review::CloseWindow { group } => state.group == group,
        };
        in_scope && self.tab_running_program_count(tab) > 0
    }

    /// Bring the next tab of `tabs` that still needs `review` forward and
    /// confirm it; once none is left, a quit goes ahead.
    pub(super) fn review_next_tab(
        &mut self,
        event_loop: &ActiveEventLoop,
        review: Review,
        tabs: Vec<WindowId>,
    ) {
        let mut tabs = tabs
            .into_iter()
            .filter(|tab| self.still_under_review(review, *tab));
        let Some(tab) = tabs.next() else {
            if review == Review::Quit {
                event_loop.exit();
            }
            return;
        };
        let rest = tabs.collect();
        // The quick terminal's prompt shows over a regular tab instead.
        let quick_terminal = self.is_quick_terminal_window(tab);
        let Some(anchor) = self.review_anchor(tab, &[]) else {
            return;
        };
        self.present_review_tab(anchor);
        let (message, hint) = review_tab_prompt(review, quick_terminal);
        self.open_confirm_dialog_with_hint(
            anchor,
            message,
            hint,
            ConfirmAction::ReviewTab { review, tab, rest },
            None,
        );
    }

    /// Start reviewing `tabs` (the first prompt's Enter). With fewer than
    /// two left to review, the request starts over instead, as it would
    /// now: a single confirmation, or none.
    pub(super) fn start_review(
        &mut self,
        event_loop: &ActiveEventLoop,
        review: Review,
        tabs: Vec<WindowId>,
    ) {
        let left = tabs
            .iter()
            .filter(|tab| self.still_under_review(review, **tab))
            .count();
        if left < 2 {
            self.restart_review_request(event_loop, review);
        } else {
            self.review_next_tab(event_loop, review, tabs);
        }
    }

    /// Ask again for the quit or window close `review` was for, from the
    /// current state of the tabs.
    fn restart_review_request(&mut self, event_loop: &ActiveEventLoop, review: Review) {
        match review {
            Review::Quit => self.request_quit(event_loop),
            Review::CloseWindow { group } => {
                // The focused tab when it is one of the window's, else its
                // first, brought forward: its prompt must be drawn and get
                // the keys.
                let in_group = |tab: &WindowId| {
                    self.windows
                        .get(tab)
                        .is_some_and(|state| state.group == group)
                };
                let tab = self
                    .focused
                    .filter(in_group)
                    .or_else(|| self.window_order.iter().copied().find(in_group));
                if let Some(tab) = tab {
                    self.present_review_tab(tab);
                    self.request_close_window_of(event_loop, tab);
                }
            }
        }
    }

    /// Carry on with a review whose prompt was over `closed`, a tab that
    /// closed on its own (its process exited): the review goes on with the
    /// tabs left, the prompt's own tab included when that was another one
    /// (the quick terminal's prompt shows over a regular tab), or, before
    /// the review began, the request starts over.
    pub(super) fn resume_review(
        &mut self,
        event_loop: &ActiveEventLoop,
        closed: WindowId,
        action: ConfirmAction,
    ) {
        match action {
            ConfirmAction::ReviewTab { review, tab, rest } => {
                let tabs = std::iter::once(tab)
                    .filter(|tab| *tab != closed)
                    .chain(rest)
                    .collect();
                self.review_next_tab(event_loop, review, tabs)
            }
            ConfirmAction::ReviewWindows { review, .. } => {
                self.restart_review_request(event_loop, review)
            }
            _ => {}
        }
    }

    /// The review accepted `tab`. Closing a window closes it, unless it has
    /// meanwhile been moved to another window; quitting keeps it.
    pub(super) fn accept_reviewed_tab(
        &mut self,
        event_loop: &ActiveEventLoop,
        review: Review,
        tab: WindowId,
    ) {
        if let Review::CloseWindow { group } = review
            && self
                .windows
                .get(&tab)
                .is_some_and(|state| state.group == group)
        {
            self.close_tab(event_loop, tab);
        }
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
            review_tab_prompt(Review::Quit, false)
                .0
                .starts_with("This window runs")
        );
        assert!(
            review_tab_prompt(Review::Quit, true)
                .0
                .starts_with("The Quick Terminal")
        );
        let close = Review::CloseWindow {
            group: WindowGroupId(1),
        };
        assert!(
            review_tab_prompt(close, false)
                .0
                .starts_with("Close this window?")
        );
    }
}
