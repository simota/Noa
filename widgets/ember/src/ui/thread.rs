use ember::model::{Role as MessageRole, Run, RunKind, RunStatus, Scope};
use noa_gpui_core::style::{AlignItems, FlexDirection, Size as LayoutSize, length, percent};
use noa_gpui_core::widgets::{Container, Label};
use noa_gpui_core::{Role, Style, UiTree, WidgetId};

use super::views::{button, label, row};
use super::{Action, EmberScreen, Page, Suggestion, excerpt, style};

fn space(value: f32) -> f32 {
    crate::appearance::space(value)
}

impl EmberScreen {
    pub(super) fn history(&mut self) {
        let parent = self.ids.history;
        style::reading_width(&mut self.tree, parent, 760.0);
        let messages = self
            .view
            .state
            .messages
            .iter()
            .filter(|message| message.scope == self.scope)
            .cloned()
            .collect::<Vec<_>>();
        let skip = messages.len().saturating_sub(60);

        if messages.is_empty() {
            let welcome = self.tree.insert(
                Some(parent),
                Container::column()
                    .align_items(AlignItems::CENTER)
                    .padding(space(24.0))
                    .gap(space(14.0)),
            );
            style::wide(&mut self.tree, welcome);
            style::avatar(&mut self.tree, welcome, 52.0);
            label(
                &mut self.tree,
                welcome,
                "今日は、何を進めましょう。",
                23.0,
                false,
            );
            label(
                &mut self.tree,
                welcome,
                "考えを整理したり、次の一歩を一緒に考えたり。\nまずは気軽に話しかけてください。",
                14.0,
                true,
            );
            let suggestions = row(&mut self.tree, welcome);
            for (title, suggestion) in [
                ("考えを整理する", Suggestion::Conversation),
                ("文書をまとめる", Suggestion::Document),
                ("資料を調べる", Suggestion::Research),
            ] {
                button(
                    &mut self.tree,
                    suggestions,
                    title,
                    Action::Suggest(suggestion),
                    &mut self.actions,
                );
            }
        } else {
            if skip > 0 {
                label(
                    &mut self.tree,
                    parent,
                    format!(
                        "この範囲の直近60件を表示中。以前の{skip}件は設定の「保存データを表示」で確認できます。"
                    ),
                    11.0,
                    true,
                );
            }
            for message in messages.into_iter().skip(skip) {
                let user = message.role == MessageRole::User;
                let group = self.tree.insert(
                    Some(parent),
                    Container::new(Style {
                        flex_direction: FlexDirection::Column,
                        align_self: Some(if user {
                            AlignItems::END
                        } else {
                            AlignItems::START
                        }),
                        flex_shrink: 0.0,
                        min_size: LayoutSize {
                            width: length(0.0),
                            height: length(0.0),
                        },
                        max_size: LayoutSize {
                            width: percent(0.82),
                            ..Default::default()
                        },
                        ..Default::default()
                    })
                    .gap(space(7.0)),
                );
                self.tree
                    .set_accessible_label(group, format!("{}のメッセージ", message.role.label()));
                label(&mut self.tree, group, message.role.label(), 11.0, true);
                let bubble = self.tree.insert(
                    Some(group),
                    Container::column()
                        .padding(space(17.0))
                        .background(if user { Role::Accent } else { Role::Surface })
                        .radius(space(20.0)),
                );
                let body = self.tree.insert(
                    Some(bubble),
                    Label::new(excerpt(&message.content, 1800))
                        .font_size(crate::appearance::font(15.0))
                        .color(if user { Role::OnAccent } else { Role::Text })
                        .wrap(true),
                );
                style::wide(&mut self.tree, body);
                let controls = row(&mut self.tree, group);
                button(
                    &mut self.tree,
                    controls,
                    "全文を表示",
                    Action::ReadMessage(message.id.clone()),
                    &mut self.actions,
                );
                button(
                    &mut self.tree,
                    controls,
                    "記憶に保存…",
                    Action::Remember(message.id),
                    &mut self.actions,
                );
            }
        }

        if let Some(id) = self.active_conversation()
            && let Some(run) = self.view.state.runs.iter().find(|run| run.id == id)
        {
            label(
                &mut self.tree,
                parent,
                format!("エンバー · {}", run.status.label()),
                12.0,
                true,
            );
        }
        if let Some(run) = self
            .view
            .state
            .runs
            .iter()
            .rev()
            .find(|run| run.kind == RunKind::Conversation && run.scope == self.scope)
        {
            run_notice(&mut self.tree, parent, run);
        }

        let activity = activity_runs(&self.view.state.runs, &self.scope);
        if !activity.is_empty() {
            let heading = row(&mut self.tree, parent);
            label(&mut self.tree, heading, "最近のアクティビティ", 12.0, true);
            button(
                &mut self.tree,
                heading,
                "すべて見る",
                Action::Page(Page::Requests),
                &mut self.actions,
            );
        }
        for run in activity {
            let card = self.tree.insert(
                Some(parent),
                Container::column()
                    .padding(space(17.0))
                    .gap(space(9.0))
                    .background(Role::Surface)
                    .border(1.0, Role::Separator)
                    .radius(space(16.0)),
            );
            style::wide(&mut self.tree, card);
            label(
                &mut self.tree,
                card,
                format!("{} · {}", run.kind.label(), run.status.label()),
                11.0,
                true,
            );
            let title = run
                .artifact
                .as_ref()
                .map(|artifact| artifact.title.as_str())
                .filter(|title| !title.trim().is_empty())
                .unwrap_or(&run.objective);
            label(&mut self.tree, card, excerpt(title, 140), 15.0, false);
            run_notice(&mut self.tree, card, &run);
            let controls = row(&mut self.tree, card);
            if run.artifact.is_some() {
                button(
                    &mut self.tree,
                    controls,
                    "成果物を見る",
                    Action::Artifact(run.id.clone()),
                    &mut self.actions,
                );
            }
            if !run.status.terminal() && run.status != RunStatus::CancelRequested {
                button(
                    &mut self.tree,
                    controls,
                    "停止",
                    Action::Cancel(run.id),
                    &mut self.actions,
                );
            }
            button(
                &mut self.tree,
                controls,
                "依頼の詳細",
                Action::Page(Page::Requests),
                &mut self.actions,
            );
        }
    }
}

fn run_notice(tree: &mut UiTree, parent: WidgetId, run: &Run) {
    if let Some(error) = &run.error {
        label(
            tree,
            parent,
            format!("{}：{}", run.status.label(), excerpt(error, 500)),
            12.0,
            true,
        );
    }
    match run.status {
        RunStatus::Canceled => {
            label(
                tree,
                parent,
                "ローカルの応答受信を停止しました。API側の処理・利用料金は未確認です。",
                12.0,
                true,
            );
        }
        RunStatus::Uncertain => {
            label(
                tree,
                parent,
                "結果が確定していないため、自動では同じ処理をやり直しません。",
                12.0,
                true,
            );
        }
        _ => {}
    }
}

fn activity_runs(runs: &[Run], scope: &Scope) -> Vec<Run> {
    let recent = runs
        .iter()
        .rev()
        .filter(|run| run.kind != RunKind::Conversation && !run.archived && run.scope == *scope)
        .collect::<Vec<_>>();
    let completed = recent
        .iter()
        .copied()
        .find(|run| run.status == RunStatus::Completed && run.artifact.is_some());
    let active_limit = if completed.is_some() { 2 } else { 3 };
    let mut selected = recent
        .iter()
        .copied()
        .filter(|run| !run.status.terminal())
        .take(active_limit)
        .collect::<Vec<_>>();
    // Keep a completed document reachable when newer work is still running.
    if let Some(completed) = completed {
        selected.push(completed);
    }
    for run in recent {
        if selected.len() == 3 {
            break;
        }
        if !selected.iter().any(|selected| selected.id == run.id) {
            selected.push(run);
        }
    }
    selected.into_iter().cloned().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ember::model::Artifact;

    fn run(id: &str, status: RunStatus) -> Run {
        Run {
            id: id.into(),
            kind: RunKind::Document,
            status,
            ..Default::default()
        }
    }

    #[test]
    fn completed_artifact_stays_visible_alongside_newer_active_work() {
        let mut completed = run("document", RunStatus::Completed);
        completed.artifact = Some(Artifact::default());
        let selected = activity_runs(
            &[
                completed,
                run("queued-1", RunStatus::Queued),
                run("queued-2", RunStatus::Queued),
                run("running", RunStatus::Running),
            ],
            &Scope::Global,
        );
        assert_eq!(selected.len(), 3);
        assert_eq!(selected[0].id, "running");
        assert_eq!(selected[1].id, "queued-2");
        assert_eq!(selected[2].id, "document");
    }

    #[test]
    fn activity_excludes_other_scopes_archived_runs_and_conversations() {
        let mut project = run("other-project", RunStatus::Running);
        project.scope = Scope::Project {
            root: "/other".into(),
        };
        let mut archived = run("archived", RunStatus::Completed);
        archived.archived = true;
        let mut conversation = run("conversation", RunStatus::Running);
        conversation.kind = RunKind::Conversation;
        let selected = activity_runs(
            &[
                run("visible", RunStatus::Failed),
                project,
                archived,
                conversation,
            ],
            &Scope::Global,
        );
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].id, "visible");
    }
}
