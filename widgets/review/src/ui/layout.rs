//! A narrow window shows one of the existing panes at a time. Switching
//! layout never rebuilds editors, so drafts, selection and scroll survive.

use noa_gpui_core::WidgetId;
use noa_gpui_core::widgets::{Button, SplitPane, SplitView};

use super::{CENTER_MIN, LEFT_WIDTH, RIGHT_WIDTH, Review};

impl Review {
    pub(super) fn adapt_layout(&mut self, width: f32) {
        self.viewport_width = width;
        super::start::adapt(&mut self.tree, &mut self.start, width);
        let focused = self.tree.focused();
        let compact = width < LEFT_WIDTH + CENTER_MIN + RIGHT_WIDTH + 12.0;
        self.tree
            .set_hidden(self.navigation, !compact || self.session.is_none());
        let pane = self.compact_pane.min(2);
        if compact == self.compact && (!compact || self.applied_pane == Some(pane)) {
            return;
        }
        if !self.compact {
            if let Some(view) = self.tree.get::<SplitView>(self.split) {
                let sizes = view.sizes();
                self.wide_sizes = (
                    sizes.first().copied().flatten().unwrap_or(LEFT_WIDTH),
                    sizes.get(2).copied().flatten().unwrap_or(RIGHT_WIDTH),
                );
            }
        }
        self.compact = compact;
        self.applied_pane = Some(pane);
        let split = if compact {
            (0..3).fold(SplitView::horizontal(), |view, index| {
                view.pane(if index == pane {
                    SplitPane::flexible().min(CENTER_MIN)
                } else {
                    SplitPane::fixed(0.0).collapsed(true)
                })
            })
        } else {
            let room = width - CENTER_MIN - 12.0;
            let extra_left = (self.wide_sizes.0 - 220.0).max(0.0);
            let extra_right = (self.wide_sizes.1 - 260.0).max(0.0);
            let scale = ((room - 480.0) / (extra_left + extra_right).max(1.0)).min(1.0);
            SplitView::horizontal()
                .pane(SplitPane::fixed(220.0 + extra_left * scale).min(220.0))
                .pane(SplitPane::flexible().min(CENTER_MIN))
                .pane(SplitPane::fixed(260.0 + extra_right * scale).min(260.0))
        };
        self.tree.update::<SplitView, _>(self.split, |view, cx| {
            *view = split;
            cx.request_layout();
        });
        let losing_focus = focused.is_some_and(|id| {
            self.pane_roots
                .iter()
                .enumerate()
                .any(|(index, root)| compact && index != pane && self.within(id, *root))
        });
        for (index, root) in self.pane_roots.into_iter().enumerate() {
            self.tree.set_hidden(root, compact && index != pane);
        }
        for (index, (button, name)) in self
            .navigation_buttons
            .into_iter()
            .zip(["対象・パネル", "指摘・結果", "選択の詳細"])
            .enumerate()
        {
            self.tree.update::<Button, _>(button, |button, cx| {
                button.set_label(
                    cx,
                    if index == pane {
                        format!("● {name}")
                    } else {
                        name.into()
                    },
                );
            });
        }
        if losing_focus {
            self.tree.set_focus(Some(self.navigation_buttons[pane]));
        } else if !compact && focused.is_some_and(|id| self.within(id, self.navigation)) {
            self.focus_action(&super::Action::ShowTab(self.tab), 0);
        }
    }

    pub(super) fn within(&self, mut id: WidgetId, root: WidgetId) -> bool {
        loop {
            if id == root {
                return true;
            }
            match self.tree.parent(id) {
                Some(parent) => id = parent,
                None => return false,
            }
        }
    }
}
