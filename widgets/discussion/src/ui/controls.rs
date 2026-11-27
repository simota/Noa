//! The footer keeps the current state and next step in view. Result tools
//! and less frequent controls share an expandable panel above it.

use noa_gpui_core::Role;
use noa_gpui_core::style::AlignItems;
use noa_gpui_core::widgets::{Badge, Button, Container, ControlSize, Label, Tone};
use noa_gpui_text::TextStore;

use super::{
    Action, Discussion, EXTEND_BY, Link, Source, caption, clear_children, note, phase_label,
};

impl Discussion {
    pub(super) fn rebuild_tools(&mut self, text: &mut TextStore) {
        clear_children(&mut self.tree, self.tools_host, text);
        self.tree
            .set_hidden(self.tools_host, !self.tools_open || self.session.is_none());
        let Some(session) = self.session.clone() else {
            return;
        };
        let read_only = self.read_only();
        let navigates = self.navigates();
        let kinds = self.available_kinds();
        let small = |label: &str| Button::new(label).control_size(ControlSize::Small);

        let result = self.tree.insert(
            Some(self.tools_host),
            Container::wrapping_row(8.0, 6.0).align_items(AlignItems::CENTER),
        );
        caption(&mut self.tree, result, "結果");
        let copy = self.tree.insert(Some(result), small("結果をコピー"));
        self.actions.push((copy, Action::CopyResult));
        let export = self.tree.insert(Some(result), small("議事録を書き出す…"));
        self.actions.push((export, Action::ExportMinutes));
        if !read_only && session.workdir.is_some() {
            caption(&mut self.tree, result, "実装を依頼");
            for kind in kinds {
                let button = self.tree.insert(Some(result), small(kind.command()));
                self.actions.push((button, Action::Implement(kind)));
            }
        } else if !read_only {
            caption(
                &mut self.tree,
                result,
                "実装の依頼には作業ディレクトリが必要です",
            );
        }

        let controls = self.tree.insert(
            Some(self.tools_host),
            Container::wrapping_row(8.0, 6.0).align_items(AlignItems::CENTER),
        );
        caption(&mut self.tree, controls, "議論");
        // A completed stored session can resume with more rounds; a
        // one-off snapshot cannot start the runner needed to do that.
        if session.extendable().is_ok() && (!read_only || (session.closed && navigates)) {
            for by in EXTEND_BY {
                let more = self.tree.insert(
                    Some(controls),
                    small(&format!("+{by} ラウンド")).tooltip(if read_only {
                        "再開してラウンドを追加します（エージェントが起動します）"
                    } else {
                        "ラウンド数の上限を増やします。終了していれば議論を続けます"
                    }),
                );
                self.tree
                    .set_accessible_label(more, format!("ラウンドを {by} 追加"));
                self.actions.push((more, Action::Extend(by)));
            }
        }
        if !read_only {
            if session.closed {
                let close = self.tree.insert(
                    Some(controls),
                    small("ペインをすべて閉じる")
                        .tooltip("司会とパネリストのペインを Noa でまとめて閉じます"),
                );
                self.actions.push((close, Action::ClosePanes));
            } else {
                let end = self.tree.insert(Some(controls), small("議論を終える"));
                self.actions.push((end, Action::Close));
            }
        }
        if navigates && !session.closed {
            let new = self.tree.insert(Some(controls), small("新しい議論"));
            self.tree
                .set_accessible_label(new, "この議論を離れて開始画面に戻る");
            self.actions.push((new, Action::NewDiscussion));
        }
        let limit = session.max_rounds.map_or_else(
            || "ラウンド上限なし".to_owned(),
            |n| format!("上限 {n} ラウンド"),
        );
        caption(
            &mut self.tree,
            controls,
            format!(
                "論点 {} · 発言 {} · 討論 {} · {limit}",
                session.issues.issues.len(),
                session.messages.len(),
                session.debates.len(),
            ),
        );
        if let Link::Down(why) = &self.link {
            note(&mut self.tree, self.tools_host, why.clone());
        }
        if let Some(status) = &self.timeline_state {
            use crate::live::timeline::Status;
            use noa_ipc::timeline_outbox::PublishState;
            match status {
                Status::Waiting { why } => {
                    note(&mut self.tree, self.tools_host, why.clone());
                }
                Status::Publish(PublishState::Published { post_id, .. }) => {
                    note(
                        &mut self.tree,
                        self.tools_host,
                        format!("タイムライン postId: {post_id}"),
                    );
                }
                Status::Publish(PublishState::Failed { message, .. }) => {
                    note(&mut self.tree, self.tools_host, message.clone());
                }
                Status::Publish(PublishState::Pending) => {}
            };
        }
    }

    pub(super) fn rebuild_status(&mut self, text: &mut TextStore) {
        clear_children(&mut self.tree, self.status_leading, text);
        clear_children(&mut self.tree, self.status_trailing, text);
        let (badge, tone) = match (&self.source, &self.link) {
            (Source::Demo, _) => ("DEMO", Tone::Neutral),
            (_, Link::Live) => ("LIVE", Tone::Neutral),
            (_, Link::Connecting) => ("接続中", Tone::Warning),
            (_, Link::Down(_)) => ("オフライン", Tone::Critical),
            (_, Link::Stored(_)) => ("記録", Tone::Neutral),
        };
        let badge = self
            .tree
            .insert(Some(self.status_leading), Badge::text(badge).tone(tone));
        if let Link::Down(why) = &self.link {
            self.tree
                .set_accessible_label(badge, format!("オフライン: {why}"));
        }
        let Some(session) = self.session.clone() else {
            if let Link::Down(why) = &self.link {
                self.tree.insert(
                    Some(self.status_leading),
                    Label::new(why.clone())
                        .font_size(crate::appearance::font(12.0))
                        .color(Role::Muted)
                        .truncation(noa_gpui_core::widgets::Truncation::Tail),
                );
            }
            return;
        };
        if let Some(round) = session.current_round() {
            caption(
                &mut self.tree,
                self.status_leading,
                format!("R{} · {}", round.no, phase_label(round.phase)),
            );
        }
        if session.closed {
            self.tree.insert(
                Some(self.status_leading),
                Badge::text("終了").tone(Tone::Neutral),
            );
        } else if session.waits_to_continue() {
            self.tree.insert(
                Some(self.status_leading),
                Badge::text("一時停止中").tone(Tone::Warning),
            );
        }
        if self.read_only() {
            let label = caption(&mut self.tree, self.status_leading, "読み取りのみ");
            self.tree
                .set_accessible_label(label, "読み取りのみ（エージェントは動いていません）");
        }
        if let Some(status) = &self.timeline_state {
            use crate::live::timeline::Status;
            use noa_ipc::timeline_outbox::PublishState;
            match status {
                Status::Waiting { why } => {
                    self.tree.insert(
                        Some(self.status_leading),
                        Badge::text("タイムライン: 要確認").tone(Tone::Warning),
                    );
                    let reason = self.tree.insert(
                        Some(self.status_leading),
                        Button::new("理由を確認")
                            .control_size(ControlSize::Small)
                            .tooltip(why.clone()),
                    );
                    self.tree.set_accessible_label(
                        reason,
                        format!("タイムラインに投稿できません。{why}。理由を表示"),
                    );
                    self.actions.push((reason, Action::ShowTimelineDetails));
                }
                Status::Publish(state) => {
                    let label = crate::live::timeline::label(state);
                    if let PublishState::Failed { message, .. } = state {
                        let button = self.tree.insert(
                            Some(self.status_leading),
                            Button::new(label)
                                .tooltip(message.clone())
                                .control_size(ControlSize::Small),
                        );
                        self.actions.push((button, Action::RetryTimeline));
                    } else {
                        let shown = caption(&mut self.tree, self.status_leading, label.clone());
                        if let PublishState::Published { post_id, .. } = state {
                            self.tree
                                .set_accessible_label(shown, format!("{label} ({post_id})"));
                        }
                    }
                }
            }
        }
        if !session.closed && !self.read_only() {
            let hold = self.tree.insert(
                Some(self.status_trailing),
                Button::new("ラウンドごとに停止")
                    .selected(session.hold)
                    .control_size(ControlSize::Small)
                    .tooltip(if session.hold {
                        "オン: 司会の要約のあと、次のラウンドを始める前に止まります"
                    } else {
                        "オフ: ラウンドを続けて進めます"
                    }),
            );
            self.actions.push((hold, Action::SetHold(!session.hold)));
            if session.waits_to_continue() {
                let go = self.tree.insert(
                    Some(self.status_trailing),
                    Button::new("次のラウンドへ")
                        .default_button(true)
                        .control_size(ControlSize::Small),
                );
                self.actions.push((go, Action::Continue));
            }
        }
        if let (false, Link::Stored(dir), true) = (session.closed, &self.link, self.navigates()) {
            let resume = self.tree.insert(
                Some(self.status_trailing),
                Button::new("再開")
                    .default_button(true)
                    .control_size(ControlSize::Small),
            );
            self.actions.push((resume, Action::Resume(dir.clone())));
        }
        if session.closed && self.navigates() {
            let new = self.tree.insert(
                Some(self.status_trailing),
                Button::new("新しい議論")
                    .default_button(true)
                    .control_size(ControlSize::Small),
            );
            self.tree
                .set_accessible_label(new, "この議論を離れて開始画面に戻る");
            self.actions.push((new, Action::NewDiscussion));
        }
        let tools = self.tree.insert(
            Some(self.status_trailing),
            Button::new("操作")
                .selected(self.tools_open)
                .control_size(ControlSize::Small)
                .tooltip("結果のコピー・書き出し、実装の依頼、ラウンドの追加など"),
        );
        self.tree.set_accessible_label(
            tools,
            if self.tools_open {
                "議論の操作を閉じる"
            } else {
                "議論の操作を表示"
            },
        );
        self.actions.push((tools, Action::ToggleTools));
    }
}
