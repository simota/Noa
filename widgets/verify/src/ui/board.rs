//! The board around the center pane: the left cards with スナップショットを開く,
//! the header (target, status, verdict; コピー, レポート保存, 証跡ごと書き出す,
//! 新しい検証) and the status bar with the round controls, ペインをすべて閉じる
//! and ワークツリーを削除.

use noa_gpui_core::style::AlignItems;
use noa_gpui_core::widgets::{Button, Container, ControlSize, Label, Tone, Truncation};
use noa_gpui_core::{Style, UiTree, WidgetId};
use noa_gpui_text::TextStore;

use noa_gpui_core::style::{Size as LayoutSize, auto, length};
use verify::model::types::{Phase, WorktreeState};

use super::{
    Action, EXTEND_BY, Source, Verify, caption, cards, clear_children, decision_tone, tag,
};
use crate::live::Link;

fn small(label: &str) -> Button {
    Button::new(label).control_size(ControlSize::Small)
}

fn button(
    tree: &mut UiTree,
    parent: WidgetId,
    button: Button,
    action: Action,
    actions: &mut Vec<(WidgetId, Action)>,
) -> WidgetId {
    let id = tree.insert(Some(parent), button);
    actions.push((id, action));
    id
}

impl Verify {
    pub(super) fn rebuild_left(&mut self, text: &mut TextStore) {
        clear_children(&mut self.tree, self.left_host, text);
        let Some(session) = self.session.clone() else {
            return;
        };
        let live = matches!(self.source, Source::Live(_) | Source::Demo) && !self.read_only;
        let target = cards::insert(
            &mut self.tree,
            self.left_host,
            &session,
            live,
            super::system_now(),
            &mut self.actions,
        );
        // L1-7: the target card opens the snapshot, when there is one.
        if self.reply_root.is_some() {
            let row = self.tree.insert(Some(target), Container::row());
            button(
                &mut self.tree,
                row,
                small("スナップショットを開く"),
                Action::OpenSnapshot,
                &mut self.actions,
            );
        }
    }

    pub(super) fn rebuild_header(&mut self, text: &mut TextStore) {
        clear_children(&mut self.tree, self.header_host, text);
        let Some(session) = self.session.clone() else {
            return;
        };
        let column = self.tree.insert(
            Some(self.header_host),
            Container::column().gap(6.0).padding(10.0),
        );
        let row = self.tree.insert(
            Some(column),
            Container::row().align_items(AlignItems::CENTER).gap(8.0),
        );
        let title = self.tree.insert(
            Some(row),
            Container::new(Style {
                flex_grow: 1.0,
                flex_shrink: 1.0,
                min_size: LayoutSize {
                    width: length(0.0),
                    height: auto(),
                },
                ..Default::default()
            }),
        );
        self.tree.insert(
            Some(title),
            Label::new(session.manifest.label.clone())
                .font_size(14.0)
                .weight(600)
                .truncation(Truncation::Tail),
        );
        tag(
            &mut self.tree,
            row,
            session.target.kind.label(),
            Tone::Neutral,
        );
        let (status, tone) = if session.closed {
            ("完了", Tone::Neutral)
        } else if session.waits_to_continue() {
            ("停止中", Tone::Warning)
        } else if session.phase() == Phase::Confirming {
            ("基準確認待ち", Tone::Warning)
        } else {
            ("進行中", Tone::Accent)
        };
        tag(&mut self.tree, row, status, tone);
        if let Some(verdict) = session.verdict() {
            tag(
                &mut self.tree,
                row,
                format!("総合判定: {}", verdict.decision.label()),
                decision_tone(verdict.decision),
            );
        } else if session.ended_by_user() {
            tag(&mut self.tree, row, "判定なし", Tone::Neutral);
        }
        let buttons = self.tree.insert(
            Some(column),
            Container::wrapping_row(6.0, 4.0).align_items(AlignItems::CENTER),
        );
        button(
            &mut self.tree,
            buttons,
            small("コピー"),
            Action::CopyReport,
            &mut self.actions,
        );
        button(
            &mut self.tree,
            buttons,
            small("レポート保存…").tooltip(
                "レポートだけを保存します（画像・動画はこのマシンの .noa/verify を指す絶対パスのリンク）",
            ),
            Action::SaveReport,
            &mut self.actions,
        );
        button(
            &mut self.tree,
            buttons,
            small("証跡ごと書き出す…").tooltip(
                "新しいフォルダにレポートと引用した証跡のコピーを書き出します（相対リンク・共有用）",
            ),
            Action::ExportBundle,
            &mut self.actions,
        );
        button(
            &mut self.tree,
            buttons,
            small("新しい検証"),
            Action::NewVerification,
            &mut self.actions,
        );
    }

    pub(super) fn rebuild_status(&mut self, text: &mut TextStore) {
        clear_children(&mut self.tree, self.status_leading, text);
        clear_children(&mut self.tree, self.status_trailing, text);
        let (link_label, link_tone) = match &self.link {
            Link::Live => ("Noa 接続中", Tone::Success),
            Link::Connecting => ("Noa 接続待ち…", Tone::Warning),
            Link::Down(_) => ("Noa 未接続", Tone::Critical),
        };
        if !self.read_only {
            tag(&mut self.tree, self.status_leading, link_label, link_tone);
        }
        let Some(session) = self.session.clone() else {
            return;
        };
        let phase_text = if session.closed {
            "終了".to_owned()
        } else if let Some(r) = session.current_round() {
            // A round's own close is its ruling done, not the verification's end.
            let phase = match r.phase {
                Phase::Closed => "判定済",
                phase => phase.label(),
            };
            format!("R{} ({}) · {phase}", r.no, r.kind.label())
        } else {
            session.phase().label().to_owned()
        };
        caption(&mut self.tree, self.status_leading, phase_text);
        if self.read_only {
            tag(
                &mut self.tree,
                self.status_leading,
                "閲覧専用",
                Tone::Neutral,
            );
        }
        let trailing = self.status_trailing;
        let tree = &mut self.tree;
        let actions = &mut self.actions;
        let live_window = matches!(self.source, Source::Live(_));
        let stored_free = self.read_only
            && self.stored_dir.is_some()
            && self.stored_free
            && !self.extending
            && live_window;
        if !self.read_only && !session.closed {
            button(
                tree,
                trailing,
                small(if session.hold {
                    "ラウンド後に停止: オン"
                } else {
                    "ラウンド後に停止: オフ"
                })
                .tooltip("オンにすると、判定のあと次のラウンドを始める前に止まります"),
                Action::SetHold(!session.hold),
                actions,
            );
            if session.waits_to_continue() {
                button(
                    tree,
                    trailing,
                    small("次のラウンドへ").default_button(true),
                    Action::Continue,
                    actions,
                );
            }
            if session.phase() == Phase::Confirming {
                button(
                    tree,
                    trailing,
                    small("検証を開始").tooltip("基準を確定して R1 を始めます"),
                    Action::StartVerify,
                    actions,
                );
            }
        }
        if session.extendable().is_ok() && (!self.read_only || (session.closed && stored_free)) {
            caption(
                tree,
                trailing,
                format!("上限 {} ラウンド", session.max_rounds),
            );
            for by in EXTEND_BY {
                let more = button(
                    tree,
                    trailing,
                    small(&format!("+{by}")).tooltip(if self.read_only {
                        "再開してラウンドを追加します（エージェントが起動します）"
                    } else {
                        "ラウンド上限を増やします。上限で終了していれば検証を続けます"
                    }),
                    Action::Extend(by),
                    actions,
                );
                tree.set_accessible_label(more, format!("ラウンドを {by} 追加"));
            }
        }
        if stored_free && !session.closed {
            button(
                tree,
                trailing,
                small("再開").tooltip("この検証を続けます（エージェントが起動します）"),
                Action::ResumeShown,
                actions,
            );
        }
        // L1-3: an ended or interrupted stored verification's worktrees go
        // on demand (an interrupted one rebuilds them on 再開).
        if stored_free
            && session
                .worktrees
                .values()
                .any(|w| w.state == WorktreeState::Made)
        {
            button(
                tree,
                trailing,
                small("ワークツリーを削除").tooltip(
                    "この検証が作った検証者の worktree を削除します（中断した検証は再開時に作り直します）",
                ),
                Action::RemoveWorktrees,
                actions,
            );
        }
        if self.read_only && self.report_stale && self.stored_free && !self.extending {
            button(
                tree,
                trailing,
                small("レポートを再生成").tooltip(
                    "保存されたレポートがログと一致しません（書き出し前に止まった可能性があります）。ログから書き直します",
                ),
                Action::RegenerateReport,
                actions,
            );
        }
        if !self.read_only
            && !session.closed
            && session.current_round().is_some_and(|r| {
                r.briefs
                    .iter()
                    .chain(&r.rerun_briefs)
                    .chain(&r.judge_brief)
                    .any(|b| b.receipt.failed.is_some() && b.receipt.replied.is_none())
            })
        {
            button(tree, trailing, small("再試行"), Action::Retry, actions);
        }
        button(
            tree,
            trailing,
            small("同じ設定で再検証").tooltip(
                "同じ対象・基準・パネルで開始ビューを埋めます（前回の総合判定と要対応を依頼に添えます）",
            ),
            Action::VerifyAgain(None),
            actions,
        );
        if !self.read_only && !session.closed {
            button(tree, trailing, small("終了"), Action::Close, actions);
        }
        // Requirement A: once finished (a verdict, or 終了), in a live
        // window: the runner's own panes, or a stored verification's that
        // Noa says are still open.
        let stored_open = self.read_only
            && self
                .stored_panes
                .as_ref()
                .is_some_and(|(dir, n)| *n > 0 && self.stored_dir.as_ref() == Some(dir));
        if live_window && session.closed && (!self.read_only || stored_open) {
            button(
                tree,
                trailing,
                small("ペインをすべて閉じる").tooltip(
                    "判定役と検証者のペインを Noa でまとめて閉じます（修正依頼のタブは残します）",
                ),
                Action::ClosePanes,
                actions,
            );
        }
    }
}
