//! The detail pane beside the columns: the task's body (an issue's body
//! and comments), its runs newest first with each one's agent, age and
//! report, and the buttons for what can be done with them.
//!
//! The screen sends a button's `Intent` when it is pressed, except
//! `Intent::Edit` and `Intent::Run`, which open the 編集 / 実行 form filled
//! from the task instead (their fields are the form's to choose).

use noa_gpui_core::style::FlexDirection;
use noa_gpui_core::widgets::{Button, Container, ControlSize, Label, ScrollView, Tag};
use noa_gpui_core::{Role, Style, UiTree, WidgetId};
use noa_gpui_scene::Color;

use tasks::model::column::column;
use tasks::model::types::{
    AgentKind, AgentState, GhFailure, Isolation, IssueDetail, Liveness, Millis, ResultFact,
    RunView, State, Task, TaskRef,
};
use tasks::runs;

use super::Intent;
use super::columns::age;
use super::style::{self, gap};

/// A problem's or a failure's colour.
const PROBLEM: Color = Color::hex(0xff9f0a);

/// Fills `host` with `task`'s detail; `runs` are the task's, newest first;
/// `detail` is an issue's loaded body and comments (`None` until it
/// loads, and for a local task). The buttons it made, each with what a
/// click does.
pub(super) fn insert(
    tree: &mut UiTree,
    host: WidgetId,
    task: &Task,
    runs: &[&RunView],
    detail: Option<&Result<IssueDetail, GhFailure>>,
    now: Millis,
) -> Vec<(WidgetId, Intent)> {
    let mut actions = Vec::new();
    let pane = tree.insert(
        Some(host),
        ScrollView::new(Style {
            flex_direction: FlexDirection::Column,
            gap: gap(10.0),
            ..style::fill()
        }),
    );
    let col = column(task, runs.first().copied());

    let head = tree.insert(Some(pane), Container::new(style::row(6.0)));
    tree.insert(
        Some(head),
        Tag::new(task.r#ref.badge()).tone(style::source_tone(task.r#ref.source())),
    );
    tree.insert(
        Some(head),
        Tag::new(col.label()).tone(style::column_tone(col)),
    );
    tree.insert(
        Some(pane),
        Label::new(task.title.clone())
            .font_size(15.0)
            .weight(700)
            .wrap(true),
    );
    if !task.labels.is_empty() {
        let labels = tree.insert(Some(pane), Container::wrapping_row(4.0, 4.0));
        for label in &task.labels {
            tree.insert(Some(labels), Tag::new(label.clone()));
        }
    }
    if let Some(problem) = &task.problem {
        tree.insert(
            Some(pane),
            Label::new(problem.clone())
                .font_size(12.0)
                .wrap(true)
                .color(PROBLEM),
        );
    }

    // What can be done with the task.
    let buttons = tree.insert(Some(pane), Container::wrapping_row(6.0, 6.0));
    let mut button = |tree: &mut UiTree, parent, label: &str, intent: Intent| {
        let id = tree.insert(
            Some(parent),
            Button::new(label).control_size(ControlSize::Small),
        );
        actions.push((id, intent));
    };
    // While a run is live (also on a closed task): the run's ペインを開く,
    // below, instead. Never on a file that does not parse.
    if runs::may_start(task, runs.first().copied()).is_ok() {
        button(
            tree,
            buttons,
            "実行 (X)",
            Intent::Run {
                task: task.r#ref.clone(),
                // The form chooses both.
                agent: AgentKind::Claude,
                isolation: Isolation::Worktree,
            },
        );
    }
    match &task.r#ref {
        TaskRef::Local(id) => {
            if task.problem.is_none() {
                button(
                    tree,
                    buttons,
                    "編集",
                    Intent::Edit {
                        id: id.clone(),
                        title: task.title.clone(),
                        body: task.body.clone().unwrap_or_default(),
                        labels: task.labels.clone(),
                    },
                );
                let (label, state) = match task.state {
                    State::Open => ("完了にする (D)", State::Closed),
                    State::Closed => ("再開 (D)", State::Open),
                };
                button(
                    tree,
                    buttons,
                    label,
                    Intent::SetState {
                        id: id.clone(),
                        state,
                    },
                );
            }
        }
        // A marker is read from the code, never written: its body says
        // where it is.
        TaskRef::Code { .. } => {}
        TaskRef::GitHub { .. } => {
            if let Some(url) = &task.url {
                button(
                    tree,
                    buttons,
                    "GitHub で開く (O)",
                    Intent::OpenUrl(url.clone()),
                );
            }
        }
    }

    heading(tree, pane, "本文");
    match (&task.r#ref, detail) {
        (TaskRef::GitHub { .. }, Some(Ok(detail))) => {
            text(tree, pane, &detail.body, "（本文はありません）");
            heading(tree, pane, &format!("コメント {}", detail.comments.len()));
            for comment in &detail.comments {
                tree.insert(
                    Some(pane),
                    Label::new(format!("{} · {}", comment.author, age(now, comment.at)))
                        .font_size(11.0)
                        .color(Role::Muted),
                );
                text(tree, pane, &comment.body, "");
            }
        }
        (TaskRef::GitHub { .. }, Some(Err(failure))) => {
            tree.insert(
                Some(pane),
                Label::new(format!("本文を読めません: {}", failure.reason()))
                    .font_size(12.0)
                    .wrap(true)
                    .color(PROBLEM),
            );
        }
        (TaskRef::GitHub { .. }, None) if task.body.is_none() => {
            muted(tree, pane, "読み込み中…");
        }
        _ => text(
            tree,
            pane,
            task.body.as_deref().unwrap_or_default(),
            "（本文はありません）",
        ),
    }

    heading(tree, pane, &format!("実行 {}", runs.len()));
    if runs.is_empty() {
        muted(tree, pane, "まだ実行していません");
    }
    for run in runs {
        insert_run(tree, pane, run, now, &mut actions);
    }
    actions
}

/// One run: agent, age and state; its branch or work tree; its buttons;
/// its report.
fn insert_run(
    tree: &mut UiTree,
    parent: WidgetId,
    view: &RunView,
    now: Millis,
    actions: &mut Vec<(WidgetId, Intent)>,
) {
    let run = &view.run;
    let frame = tree.insert(
        Some(parent),
        Container::new(Style {
            flex_direction: FlexDirection::Column,
            gap: gap(6.0),
            padding: noa_gpui_core::style::Rect::length(10.0),
            flex_shrink: 0.0,
            ..Default::default()
        })
        .border(1.0, Role::Separator)
        .radius(8.0),
    );
    tree.insert(
        Some(frame),
        Label::new(format!(
            "{} · {} · {}",
            run.agent.command(),
            age(now, run.started),
            liveness(&view.liveness)
        ))
        .font_size(12.0)
        .weight(600),
    );
    let place = match &run.branch {
        Some(branch) => format!("{branch} · {}", run.workdir.display()),
        None => run.workdir.display().to_string(),
    };
    tree.insert(
        Some(frame),
        Label::new(place)
            .font_size(11.0)
            .wrap(true)
            .color(Role::Muted),
    );
    let buttons = tree.insert(Some(frame), Container::wrapping_row(6.0, 6.0));
    let mut button = |tree: &mut UiTree, label: &str, intent: Intent| {
        let id = tree.insert(
            Some(buttons),
            Button::new(label).control_size(ControlSize::Small),
        );
        actions.push((id, intent));
    };
    if matches!(view.liveness, Liveness::Alive { .. }) {
        button(tree, "ペインを開く", Intent::OpenPane(run.run.clone()));
    }
    // Until the run is closed: an ended agent's float stays open on the
    // board until it is.
    if run.closed.is_none() {
        button(tree, "閉じる", Intent::ClosePane(run.run.clone()));
    }
    button(
        tree,
        "worktree の場所を開く",
        Intent::OpenWorktree(run.run.clone()),
    );
    match (&view.report, &view.result) {
        (Some(report), _) => {
            text(tree, frame, &report.text, "（報告は空です）");
            if report.cut {
                muted(tree, frame, "（16 KiB / 200 行までを表示しています）");
            }
        }
        (None, ResultFact::Written { .. }) => muted(tree, frame, "報告を読めません"),
        (None, ResultFact::Missing) => muted(tree, frame, "報告はまだありません"),
    }
}

/// What a run's pane is doing, in a word.
fn liveness(liveness: &Liveness) -> &'static str {
    match liveness {
        Liveness::Starting => "起動中",
        Liveness::Alive { agent } => match agent {
            None | Some(AgentState::Running) => "実行中",
            Some(AgentState::Permission) => "許可待ち",
            Some(AgentState::Input) => "入力待ち",
            Some(AgentState::Finished) => "終了を報告",
            Some(AgentState::Error) => "エラー",
        },
        Liveness::Gone => "終了",
    }
}

fn heading(tree: &mut UiTree, parent: WidgetId, text: &str) {
    tree.insert(
        Some(parent),
        Label::new(text)
            .font_size(12.0)
            .weight(700)
            .color(Role::Muted),
    );
}

fn muted(tree: &mut UiTree, parent: WidgetId, text: &str) {
    tree.insert(
        Some(parent),
        Label::new(text)
            .font_size(12.0)
            .wrap(true)
            .color(Role::Muted),
    );
}

/// `body` as wrapped, selectable text; `empty` when it is blank.
fn text(tree: &mut UiTree, parent: WidgetId, body: &str, empty: &str) {
    let body = body.trim_end();
    if body.is_empty() {
        if !empty.is_empty() {
            muted(tree, parent, empty);
        }
        return;
    }
    tree.insert(
        Some(parent),
        Label::new(body).font_size(12.0).wrap(true).selectable(true),
    );
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use noa_gpui_core::{Event, Key, Modifiers};
    use noa_gpui_platform::Screen;
    use noa_gpui_scene::Size;
    use noa_gpui_text::TextStore;

    use tasks::model::types::{Report, RunId};

    use super::super::{Feed, Tasks, View};
    use super::*;
    use crate::demo;

    struct Recorder {
        view: Option<View>,
        sent: Rc<RefCell<Vec<Intent>>>,
    }

    impl Feed for Recorder {
        fn poll(&mut self) -> Option<View> {
            self.view.take()
        }

        fn act(&mut self, intent: Intent) {
            self.sent.borrow_mut().push(intent);
        }
    }

    fn window() -> Size {
        Size::new(1280.0, 860.0)
    }

    fn screen_on(view: View) -> (Tasks, Rc<RefCell<Vec<Intent>>>, TextStore) {
        let sent = Rc::new(RefCell::new(Vec::new()));
        let mut screen = Tasks::new(Box::new(Recorder {
            view: Some(view),
            sent: sent.clone(),
        }));
        let mut text = TextStore::new(Default::default());
        screen.prepare(window(), &mut text);
        (screen, sent, text)
    }

    fn texts(tree: &UiTree, id: WidgetId, out: &mut Vec<String>) {
        if let Some(l) = tree.get::<Label>(id) {
            out.push(l.text().to_owned());
        }
        if let Some(t) = tree.get::<Tag>(id) {
            out.push(t.text().to_owned());
        }
        if let Some(b) = tree.get::<Button>(id) {
            out.push(b.label().to_owned());
        }
        for c in tree.children(id) {
            texts(tree, *c, out);
        }
    }

    fn shown(screen: &Tasks) -> Vec<String> {
        let mut out = Vec::new();
        texts(&screen.tree, screen.detail_host, &mut out);
        out
    }

    fn at(list: &[String], what: &str) -> usize {
        list.iter()
            .position(|t| t.contains(what))
            .unwrap_or_else(|| panic!("no {what:?} in {list:?}"))
    }

    fn button(screen: &Tasks, label: &str) -> WidgetId {
        screen
            .detail_actions
            .iter()
            .map(|(id, _)| *id)
            .find(|id| {
                screen
                    .tree
                    .get::<Button>(*id)
                    .is_some_and(|b| b.label() == label)
            })
            .unwrap_or_else(|| panic!("no button {label:?}"))
    }

    fn click(screen: &mut Tasks, text: &mut TextStore, label: &str) {
        let id = button(screen, label);
        screen.tree.set_focus(Some(id));
        screen.dispatch(
            Event::KeyDown {
                key: Key::Space,
                modifiers: Modifiers::default(),
            },
            text,
        );
        screen.prepare(window(), text);
    }

    fn sent(log: &Rc<RefCell<Vec<Intent>>>) -> Vec<Intent> {
        std::mem::take(&mut *log.borrow_mut())
    }

    /// A run whose agent ended but was never closed still offers 閉じる:
    /// its float may be open on the board.
    #[test]
    fn a_run_ended_but_not_closed_still_offers_close() {
        let mut view = demo::view();
        let at = view
            .runs
            .iter()
            .position(|v| v.run.closed.is_none())
            .expect("a run not closed");
        view.runs[at].liveness = Liveness::Gone;
        let task = view.runs[at].run.task.clone();
        let run = view.runs[at].run.run.clone();
        let (mut screen, log, mut text) = screen_on(view);
        screen.select_ref(&task);
        screen.open_detail(task, &mut text);
        screen.prepare(window(), &mut text);
        click(&mut screen, &mut text, "閉じる");
        assert!(sent(&log).contains(&Intent::ClosePane(run)));
    }

    /// The demo view, with an older second run on t-3 whose report is cut.
    fn view() -> View {
        let mut view = demo::view();
        let mut older = view
            .runs
            .iter()
            .find(|r| r.run.task == TaskRef::Local("t-3".into()))
            .unwrap()
            .clone();
        older.run.run = RunId::new("run-older");
        older.run.agent = AgentKind::Codex;
        older.run.started -= 2 * 86_400_000;
        older.report = Some(Report {
            text: "前回の報告".into(),
            cut: true,
        });
        view.runs.push(older);
        view
    }

    #[test]
    fn ac19_a_local_task_shows_its_body_and_runs_newest_first_with_reports() {
        let (mut screen, _, mut text) = screen_on(view());
        assert!(screen.select("t-3", &mut text));
        screen.prepare(window(), &mut text);
        let list = shown(&screen);
        assert_eq!(list[0], "ローカル");
        assert!(list.contains(&"確認待ち".to_owned()), "{list:?}");
        assert!(list.contains(&"docs".to_owned()), "{list:?}");
        let body = at(&list, "手順と完了条件はこの本文に書きます。");
        let runs = at(&list, "実行 2");
        assert!(body < runs);
        // Newest first: claude 19 hours ago, then codex two days before.
        let newest = at(&list, "claude · 19 時間前 · 終了");
        let older = at(&list, "codex · 2 日前 · 終了");
        assert!(runs < newest && newest < older, "{list:?}");
        let report = at(&list, "SKILL.md の例を新しい見出しの形式に書き換えました。");
        assert!(newest < report && report < older);
        assert!(at(&list, "前回の報告") > older);
        assert!(at(&list, "200 行") > older, "a cut report says so");
        assert!(list[newest + 1].contains("task-run-"), "{list:?}");
    }

    #[test]
    fn ac19_an_issue_shows_its_body_and_comments_and_its_loading_and_failure() {
        let (mut screen, _, mut text) = screen_on(view());
        assert!(screen.select("#12", &mut text));
        let list = shown(&screen);
        assert_eq!(list[0], "#12");
        let body = at(&list, "タブを Cmd+Shift+[ / ] で左右に動かしたい。");
        let comments = at(&list, "コメント 1");
        let author = at(&list, "reviewer · 3 時間前");
        let comment = at(&list, "split の移動と同じキー割り当ての考え方でよさそう。");
        assert!(body < comments && comments < author && author < comment);
        assert!(list.contains(&"まだ実行していません".to_owned()));

        // #21's body is not loaded yet; a failed load says why.
        let (mut screen, _, mut text) = screen_on(view());
        assert!(screen.select("#21", &mut text));
        assert!(shown(&screen).contains(&"読み込み中…".to_owned()));
        let mut failed = view();
        failed.github.details.insert(21, Err(GhFailure::Timeout));
        let (mut screen, _, mut text) = screen_on(failed);
        assert!(screen.select("#21", &mut text));
        at(&shown(&screen), &GhFailure::Timeout.reason());
    }

    #[test]
    fn ac19_the_buttons_map_to_their_intents() {
        let view = view();
        let (mut screen, log, mut text) = screen_on(view.clone());
        // A running local task: its pane, no 実行.
        assert!(screen.select("t-2", &mut text));
        let run = view
            .runs
            .iter()
            .find(|r| r.run.task == TaskRef::Local("t-2".into()))
            .unwrap()
            .run
            .run
            .clone();
        let intents: Vec<Intent> = screen
            .detail_actions
            .iter()
            .map(|(_, i)| i.clone())
            .collect();
        assert!(
            !intents.iter().any(|i| matches!(i, Intent::Run { .. })),
            "{intents:?}"
        );
        click(&mut screen, &mut text, "ペインを開く");
        assert_eq!(sent(&log), [Intent::OpenPane(run.clone())]);
        click(&mut screen, &mut text, "閉じる");
        assert_eq!(sent(&log), [Intent::ClosePane(run.clone())]);
        click(&mut screen, &mut text, "worktree の場所を開く");
        assert_eq!(sent(&log), [Intent::OpenWorktree(run)]);
        click(&mut screen, &mut text, "完了にする (D)");
        assert_eq!(
            sent(&log),
            [Intent::SetState {
                id: "t-2".into(),
                state: State::Closed
            }]
        );
        // 編集 and 実行 open their forms instead of sending.
        click(&mut screen, &mut text, "編集");
        assert!(screen.form.is_some());
        assert!(sent(&log).is_empty());
        screen.close_form(&mut text);

        assert!(screen.select("t-1", &mut text));
        click(&mut screen, &mut text, "実行 (X)");
        assert!(screen.form.is_some());
        assert!(sent(&log).is_empty());
        screen.close_form(&mut text);

        // An issue: GitHub で開く, no 編集 or 完了にする.
        assert!(screen.select("#12", &mut text));
        let list = shown(&screen);
        assert!(
            !list
                .iter()
                .any(|t| t == "編集" || t.starts_with("完了にする")),
            "{list:?}"
        );
        click(&mut screen, &mut text, "GitHub で開く (O)");
        assert_eq!(
            sent(&log),
            [Intent::OpenUrl(format!(
                "https://github.com/{}/issues/12",
                demo::REPO
            ))]
        );
        // A closed local task is reopened.
        assert!(screen.select("t-4", &mut text));
        click(&mut screen, &mut text, "再開 (D)");
        assert_eq!(
            sent(&log),
            [Intent::SetState {
                id: "t-4".into(),
                state: State::Open
            }]
        );
    }

    fn intents(screen: &Tasks) -> Vec<Intent> {
        screen
            .detail_actions
            .iter()
            .map(|(_, i)| i.clone())
            .collect()
    }

    #[test]
    fn ac19_no_run_button_on_a_closed_task_whose_run_is_live_or_on_a_broken_file() {
        let mut view = view();
        // t-2's run is live; the task is closed under it (`d`).
        let t2 = view
            .tasks
            .iter_mut()
            .find(|t| t.r#ref == TaskRef::Local("t-2".into()))
            .unwrap();
        t2.state = State::Closed;
        let mut broken = t2.clone();
        broken.r#ref = TaskRef::Local("t-9".into());
        broken.title = "t-9".into();
        broken.state = State::Open;
        broken.body = None;
        broken.problem = Some("読めません: 1 行目が --- ではありません".into());
        view.tasks.push(broken);
        let (mut screen, _, mut text) = screen_on(view);
        assert!(screen.select("t-2", &mut text));
        let got = intents(&screen);
        assert!(
            !got.iter().any(|i| matches!(i, Intent::Run { .. })),
            "{got:?}"
        );
        assert!(
            got.iter().any(|i| matches!(i, Intent::OpenPane(_))),
            "its pane instead: {got:?}"
        );
        assert!(screen.select("t-9", &mut text));
        let got = intents(&screen);
        assert!(
            !got.iter().any(|i| matches!(i, Intent::Run { .. })),
            "{got:?}"
        );
        at(&shown(&screen), "読めません");
    }
}
