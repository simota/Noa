//! The detail pane beside the columns: the task's 関連成果 (its runs,
//! change versions, reviews, verifications and PRs,
//! `docs/specs/task-links.md` L1-6), its body (an issue's body and
//! comments), its runs newest first with each one's agent, age and report,
//! and the buttons for what can be done with them.
//!
//! The screen sends a button's `Intent` when it is pressed, except
//! `Intent::Edit`, `Intent::Run` and `Intent::AddLink`, which open the 編集
//! / 実行 / 関連を追加 form filled from the task instead (their fields are
//! the form's to choose).

use noa_gpui_core::style::{AlignItems, FlexDirection, auto, length};
use noa_gpui_core::widgets::{Button, Container, ControlSize, Label, ScrollView, Tag, Tone};
use noa_gpui_core::{Role, Style, UiTree, WidgetId};
use noa_gpui_scene::Color;

use tasks::model::column::column;
use tasks::model::types::{
    AgentKind, AgentState, GhFailure, Isolation, IssueDetail, Liveness, Millis, ResultFact, RunId,
    RunView, State, Task, TaskRef,
};
use tasks::related::{self, Related};
use tasks::runs;
use tasks::task_links::SessionKind;
use tasks::timeline::Status;

use noa_ipc::timeline::Outcome;
use std::collections::BTreeMap;

use super::Intent;
use super::columns::age;
use super::style::{self, gap};

/// A problem's or a failure's colour.
const PROBLEM: Color = Color::hex(0xff9f0a);

/// Fills `host` with `task`'s detail; `runs` are the task's, newest first;
/// `detail` is an issue's loaded body and comments (`None` until it
/// loads, and for a local task); `related` its 関連成果 (`None`: nothing
/// linked); `runs.1` where each run's Board Timeline share stands
/// ([`Shares`]). The buttons it made, each with what a click does.
pub(super) fn insert(
    tree: &mut UiTree,
    host: WidgetId,
    task: &Task,
    runs: (&[&RunView], Shares<'_>),
    detail: Option<&Result<IssueDetail, GhFailure>>,
    related: Option<&Related>,
    now: Millis,
) -> Vec<(WidgetId, Intent)> {
    let (runs, shares) = runs;
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
                auto: None,
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

    let none = Related::default();
    insert_related(
        tree,
        pane,
        &task.r#ref,
        related.unwrap_or(&none),
        runs,
        &mut actions,
    );

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
        let share = shares.map(|all| all.get(&run.run.run));
        insert_run(tree, pane, run, share, now, &mut actions);
    }
    actions
}

/// One line of 関連成果: its text and its buttons.
struct Line {
    text: String,
    buttons: Vec<(&'static str, Intent)>,
}

impl Line {
    fn plain(text: String) -> Line {
        Line {
            text,
            buttons: Vec::new(),
        }
    }
}

/// The 関連成果 section (L1-6): one group per kind (実行, 変更, レビュー,
/// 検証, PR) that has lines, its tag first; newest first inside a kind
/// (versions: current first), a 未登録 line last in its kind; then 関連を
/// 追加.
fn insert_related(
    tree: &mut UiTree,
    pane: WidgetId,
    task: &TaskRef,
    related: &Related,
    runs: &[&RunView],
    actions: &mut Vec<(WidgetId, Intent)>,
) {
    heading(tree, pane, "関連成果");
    let mut groups: Vec<(&str, Tone, Vec<Line>)> = Vec::new();

    let lines = related
        .runs
        .iter()
        .map(|id| match runs.iter().find(|v| &v.run.run == id) {
            Some(view) => {
                let mut buttons = Vec::new();
                if matches!(view.liveness, Liveness::Alive { .. }) {
                    buttons.push(("ペインを開く", Intent::OpenPane(id.clone())));
                }
                buttons.push(("worktree の場所を開く", Intent::OpenWorktree(id.clone())));
                Line {
                    text: format!(
                        "{id} · {} · {}",
                        view.run.agent.command(),
                        liveness(&view.liveness)
                    ),
                    buttons,
                }
            }
            None => Line::plain(id.to_string()),
        })
        .collect();
    groups.push(("実行", Tone::Accent, lines));

    let current = related.current().map(|v| v.no);
    // The current version first, then the others newest first.
    let mut versions: Vec<_> = related.versions.iter().collect();
    versions.sort_by_key(|v| (Some(v.no) != current, std::cmp::Reverse(v.no)));
    let lines = versions
        .into_iter()
        .map(|v| {
            let place = runs
                .iter()
                .find(|view| view.run.run == v.run)
                .map(|view| {
                    view.run
                        .branch
                        .clone()
                        .unwrap_or_else(|| "この作業ツリー".to_owned())
                })
                .unwrap_or_else(|| v.run.to_string());
            let mark = if Some(v.no) == current {
                "（現在）"
            } else {
                ""
            };
            let short: String = v.commit.chars().take(7).collect();
            Line::plain(format!("v{}{mark} {short} · {place}", v.no))
        })
        .collect();
    groups.push(("変更", Tone::Neutral, lines));

    for (kind, tag, tone) in [
        (SessionKind::Review, "レビュー", Tone::Warning),
        (SessionKind::Verify, "検証", Tone::Success),
    ] {
        let mut lines: Vec<Line> = related
            .checked(kind)
            .iter()
            .rev()
            .map(|c| Line {
                text: format!(
                    "{} · {} · {}",
                    c.session,
                    related::version_text(&c.target),
                    related::outcome_label(kind, &c.outcome)
                ),
                buttons: vec![
                    (
                        "開く",
                        Intent::OpenSession {
                            kind,
                            session: c.session.clone(),
                        },
                    ),
                    (
                        "外す",
                        Intent::RemoveLink {
                            task: task.clone(),
                            id: c.session.clone(),
                        },
                    ),
                ],
            })
            .collect();
        if let Some(no) = current
            && !related.registered(kind)
        {
            lines.push(Line::plain(format!("{tag} v{no} 未登録")));
        }
        groups.push((tag, tone, lines));
    }

    let lines = related
        .prs
        .iter()
        .rev()
        .map(|pr| Line {
            text: match pr.number {
                Some(n) => format!("#{n} {}", pr.url),
                None => pr.url.clone(),
            },
            buttons: vec![
                ("開く", Intent::OpenUrl(pr.url.clone())),
                (
                    "外す",
                    Intent::RemoveLink {
                        task: task.clone(),
                        id: pr.id.clone(),
                    },
                ),
            ],
        })
        .collect();
    groups.push(("PR", Tone::Neutral, lines));

    groups.retain(|(_, _, lines)| !lines.is_empty());
    if groups.is_empty() {
        muted(tree, pane, "まだ関連する成果はありません");
    }
    for (tag, tone, lines) in groups {
        let group = tree.insert(
            Some(pane),
            Container::new(Style {
                align_items: AlignItems::START,
                ..style::row(8.0)
            }),
        );
        let slot = tree.insert(
            Some(group),
            Container::new(Style {
                flex_shrink: 0.0,
                size: noa_gpui_core::style::Size {
                    width: length(64.0),
                    height: auto(),
                },
                ..Default::default()
            }),
        );
        tree.insert(Some(slot), Tag::new(tag).tone(tone));
        let items = tree.insert(
            Some(group),
            Container::new(Style {
                gap: gap(4.0),
                ..style::fill()
            }),
        );
        for line in lines {
            tree.insert(
                Some(items),
                Label::new(line.text).font_size(12.0).wrap(true),
            );
            if line.buttons.is_empty() {
                continue;
            }
            let buttons = tree.insert(Some(items), Container::wrapping_row(6.0, 6.0));
            for (label, intent) in line.buttons {
                let id = tree.insert(
                    Some(buttons),
                    Button::new(label).control_size(ControlSize::Small),
                );
                actions.push((id, intent));
            }
        }
    }
    let add = tree.insert(Some(pane), Container::wrapping_row(6.0, 6.0));
    let id = tree.insert(
        Some(add),
        Button::new("関連を追加 (A)").control_size(ControlSize::Small),
    );
    actions.push((
        id,
        Intent::AddLink {
            task: task.clone(),
            input: String::new(),
        },
    ));
}

/// Where each run's Board Timeline share stands, as the feed said
/// ([`super::Feed::timeline`]); `None`: the feed cannot share.
pub(super) type Shares<'a> = Option<&'a BTreeMap<RunId, Status>>;

/// One run: agent, age and state; its branch or work tree; its buttons;
/// its report. `share`: where its timeline share stands (`Some(None)`:
/// never shared; `None`: the feed cannot share).
fn insert_run(
    tree: &mut UiTree,
    parent: WidgetId,
    view: &RunView,
    share: Option<Option<&Status>>,
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
    if let Some(share) = share {
        insert_share(tree, frame, view, share, actions);
    }
}

/// The run's 共有 to its board's timeline: the two ways to share a written
/// result (its outcome left 未判定, or 成功), or where the share stands,
/// with 再試行 while it is not published.
fn insert_share(
    tree: &mut UiTree,
    frame: WidgetId,
    view: &RunView,
    share: Option<&Status>,
    actions: &mut Vec<(WidgetId, Intent)>,
) {
    let run = &view.run.run;
    let row = tree.insert(Some(frame), Container::wrapping_row(6.0, 6.0));
    let mut button = |tree: &mut UiTree, label: &str, tooltip: &str, outcome| {
        let id = tree.insert(
            Some(row),
            Button::new(label)
                .control_size(ControlSize::Small)
                .tooltip(tooltip),
        );
        actions.push((
            id,
            Intent::Share {
                run: run.clone(),
                outcome,
            },
        ));
    };
    match share.and_then(Status::label).zip(share) {
        Some((label, status)) => {
            tree.insert(
                Some(row),
                Label::new(label).font_size(11.0).color(if status.failed() {
                    Role::Text
                } else {
                    Role::Muted
                }),
            );
            // Why it failed or waits, or the post the server stored.
            if let Some(detail) = status.detail() {
                tree.insert(
                    Some(row),
                    Label::new(detail).font_size(11.0).color(Role::Muted),
                );
            }
            if !matches!(status, Status::Published { .. }) {
                button(
                    tree,
                    "再試行",
                    "同じ内容でタイムラインへもう一度送ります",
                    None,
                );
            }
        }
        None if matches!(view.result, ResultFact::Written { .. }) => {
            button(
                tree,
                "共有",
                "結果をボードのタイムラインに投稿します（成否は未判定）",
                Some(Outcome::Unknown),
            );
            button(
                tree,
                "成功として共有",
                "結果を成功としてボードのタイムラインに投稿します",
                Some(Outcome::Succeeded),
            );
        }
        None => {}
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
    use tasks::related::Related;

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

    // ------------------------------------------------------------ 関連成果

    /// A feed with 関連成果, recording what is sent.
    struct Linked {
        view: Option<View>,
        related: std::collections::BTreeMap<TaskRef, Related>,
        sent: Rc<RefCell<Vec<Intent>>>,
    }

    impl Feed for Linked {
        fn poll(&mut self) -> Option<View> {
            self.view.take()
        }

        fn act(&mut self, intent: Intent) {
            self.sent.borrow_mut().push(intent);
        }

        fn related(&self) -> Option<&std::collections::BTreeMap<TaskRef, Related>> {
            Some(&self.related)
        }
    }

    /// The demo View and its 関連成果 from `events` (the demo's link log
    /// when `None`), with t-3's detail open.
    fn linked_screen(
        events: Option<Vec<tasks::task_links::LinkEvent>>,
    ) -> (Tasks, Rc<RefCell<Vec<Intent>>>, TextStore, Related) {
        let view = demo::view();
        let events = events.unwrap_or_else(|| demo::link_events(&view));
        let runs: Vec<_> = view.runs.iter().map(|v| v.run.clone()).collect();
        let related = related::fold(&runs, &events);
        let t3 = related[&TaskRef::Local(demo::LINKED_TASK.into())].clone();
        let sent = Rc::new(RefCell::new(Vec::new()));
        let mut screen = Tasks::new(Box::new(Linked {
            view: Some(view),
            related,
            sent: sent.clone(),
        }));
        let mut text = TextStore::new(Default::default());
        screen.prepare(window(), &mut text);
        assert!(screen.select(demo::LINKED_TASK, &mut text));
        screen.prepare(window(), &mut text);
        (screen, sent, text, t3)
    }

    /// Where `what` is, exactly, in `list` from `from` on.
    fn exact(list: &[String], what: &str, from: usize) -> usize {
        list.iter()
            .skip(from)
            .position(|t| t == what)
            .map(|i| i + from)
            .unwrap_or_else(|| panic!("no {what:?} after {from} in {list:?}"))
    }

    fn click_intent(screen: &mut Tasks, text: &mut TextStore, intent: &Intent) {
        let id = screen
            .detail_actions
            .iter()
            .find(|(_, i)| i == intent)
            .map(|(id, _)| *id)
            .unwrap_or_else(|| panic!("no button for {intent:?}"));
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

    #[test]
    fn links_ac1_the_related_section_lists_each_kind_in_order_with_its_buttons() {
        let (mut screen, log, mut text, t3) = linked_screen(None);
        let task = TaskRef::Local(demo::LINKED_TASK.into());
        let run = t3.runs[0].clone();
        let review = t3.reviews[0].session.clone();
        let verify = t3.verifications[0].session.clone();
        let pr = t3.prs[0].clone();
        let list = shown(&screen);
        // After the task's buttons, before 本文.
        let heading = exact(&list, "関連成果", 0);
        assert!(exact(&list, "編集", 0) < heading);
        let body = exact(&list, "本文", heading);
        let tags: Vec<usize> = ["実行", "変更", "レビュー", "検証", "PR"]
            .iter()
            .map(|tag| exact(&list, tag, heading))
            .collect();
        assert!(tags.windows(2).all(|w| w[0] < w[1]), "{tags:?} in {list:?}");
        assert!(*tags.last().unwrap() < body);
        let branch = format!("task-{run}");
        let rows = [
            format!("{run} · claude · 終了"),
            format!("v2（現在） 1a2b3c4 · {branch}"),
            format!("v1 9f8e7d6 · {branch}"),
            format!("{review} · v2 · 承認"),
            format!("{verify} · v1 · 合格"),
            "検証 v2 未登録".to_owned(),
            format!("#45 {}", pr.url),
            "関連を追加 (A)".to_owned(),
        ];
        let mut last = heading;
        for row in &rows {
            let i = exact(&list, row, last);
            assert!(i < body, "{row:?} after 本文");
            last = i;
        }
        // Each item once.
        for row in &rows {
            assert_eq!(list.iter().filter(|t| *t == row).count(), 1, "{row:?}");
        }
        // A run that is not live has no ペインを開く among them.
        assert!(!list[heading..body].contains(&"ペインを開く".to_owned()));

        for intent in [
            Intent::OpenWorktree(run.clone()),
            Intent::OpenSession {
                kind: SessionKind::Review,
                session: review.clone(),
            },
            Intent::RemoveLink {
                task: task.clone(),
                id: review.clone(),
            },
            Intent::OpenSession {
                kind: SessionKind::Verify,
                session: verify.clone(),
            },
            Intent::RemoveLink {
                task: task.clone(),
                id: verify,
            },
            Intent::OpenUrl(pr.url.clone()),
            Intent::RemoveLink {
                task: task.clone(),
                id: pr.id.clone(),
            },
        ] {
            click_intent(&mut screen, &mut text, &intent);
            assert_eq!(sent(&log), [intent]);
        }
        // 関連を追加 opens its form instead of sending.
        click_intent(
            &mut screen,
            &mut text,
            &Intent::AddLink {
                task,
                input: String::new(),
            },
        );
        assert!(screen.form.is_some());
        assert!(sent(&log).is_empty());
    }

    #[test]
    fn links_ac1_a_live_run_offers_its_pane_and_a_task_with_nothing_says_so() {
        let view = view();
        let (mut screen, _, mut text) = screen_on(view);
        // A feed without links: every detail has the empty section.
        assert!(screen.select("t-1", &mut text));
        let list = shown(&screen);
        let heading = exact(&list, "関連成果", 0);
        assert_eq!(list[heading + 1], "まだ関連する成果はありません");
        assert_eq!(list[heading + 2], "関連を追加 (A)");

        let mut view = demo::view();
        view.runs
            .retain(|r| r.run.task == TaskRef::Local("t-2".into()));
        let runs: Vec<_> = view.runs.iter().map(|v| v.run.clone()).collect();
        let related = related::fold(&runs, &[]);
        let sent = Rc::new(RefCell::new(Vec::new()));
        let mut screen = Tasks::new(Box::new(Linked {
            view: Some(view),
            related,
            sent,
        }));
        let mut text = TextStore::new(Default::default());
        assert!(screen.select("t-2", &mut text));
        let list = shown(&screen);
        let heading = exact(&list, "関連成果", 0);
        let run = exact(&list, "実行", heading);
        assert!(list[run + 1].ends_with(" · claude · 実行中"), "{list:?}");
        assert_eq!(list[run + 2], "ペインを開く");
        assert_eq!(list[run + 3], "worktree の場所を開く");
        // No version yet: no 未登録 line.
        assert!(!list.iter().any(|t| t.contains("未登録")), "{list:?}");
    }

    #[test]
    fn links_ac4_current_v2_review_v2_verify_v1() {
        let (screen, _, _, t3) = linked_screen(None);
        assert_eq!(t3.current().map(|v| v.no), Some(2));
        assert!(t3.registered(SessionKind::Review));
        assert!(!t3.registered(SessionKind::Verify));
        let list = shown(&screen);
        let review = &t3.reviews[0].session;
        let verify = &t3.verifications[0].session;
        assert!(
            list.iter().any(|t| t.starts_with("v2（現在） ")),
            "{list:?}"
        );
        assert!(list.contains(&format!("{review} · v2 · 承認")), "{list:?}");
        assert!(list.contains(&format!("{verify} · v1 · 合格")), "{list:?}");
        assert!(list.contains(&"検証 v2 未登録".to_owned()), "{list:?}");
        assert!(!list.contains(&"レビュー v2 未登録".to_owned()), "{list:?}");
    }

    /// The current version is listed first even when the head went back
    /// to an earlier one (L1-6: versions, current first).
    #[test]
    fn links_ac4_a_head_back_on_v1_lists_v1_first() {
        use tasks::task_links::LinkEvent;
        let view = demo::view();
        let mut events = demo::link_events(&view);
        let (run, v1) = events
            .iter()
            .find_map(|e| match e {
                LinkEvent::ChangeSeen { run, commit, .. } => Some((run.clone(), commit.clone())),
                _ => None,
            })
            .unwrap();
        events.push(LinkEvent::ChangeSeen {
            run,
            commit: v1,
            at: Millis::MAX,
        });
        let (screen, _, _, t3) = linked_screen(Some(events));
        assert_eq!(t3.current().map(|v| v.no), Some(1));
        let list = shown(&screen);
        let first = list.iter().position(|t| t.starts_with("v1")).unwrap();
        let second = list.iter().position(|t| t.starts_with("v2")).unwrap();
        assert!(list[first].starts_with("v1（現在） "), "{list:?}");
        assert!(first < second, "{list:?}");
    }

    #[test]
    fn links_ac4_an_uncommitted_target_leaves_v2_unregistered_and_a_base_target_is_at_start() {
        use tasks::task_links::LinkEvent;
        let view = demo::view();
        let mut events = demo::link_events(&view);
        let base = events
            .iter()
            .find_map(|e| match e {
                LinkEvent::ChangeBase { commit, .. } => Some(commit.clone()),
                _ => None,
            })
            .unwrap();
        let v2 = format!("{:0<40}", "1a2b3c4");
        for event in &mut events {
            if let LinkEvent::SessionLinked {
                kind,
                commit,
                uncommitted,
                ..
            } = event
            {
                match kind {
                    // The verification checked v2 with uncommitted changes.
                    SessionKind::Verify => {
                        *commit = Some(v2.clone());
                        *uncommitted = true;
                    }
                    // The review checked the commit the run started from.
                    SessionKind::Review => *commit = Some(base.clone()),
                }
            }
        }
        let (screen, _, _, t3) = linked_screen(Some(events));
        let list = shown(&screen);
        let review = &t3.reviews[0].session;
        let verify = &t3.verifications[0].session;
        assert!(
            list.contains(&format!("{verify} · v2+未コミット · 合格")),
            "{list:?}"
        );
        assert!(
            list.contains(&format!("{review} · 着手時点 · 承認")),
            "{list:?}"
        );
        assert!(list.contains(&"検証 v2 未登録".to_owned()), "{list:?}");
        assert!(list.contains(&"レビュー v2 未登録".to_owned()), "{list:?}");
    }

    mod timeline {
        use super::*;

        /// A feed that can share: its runs' states are `shares`.
        struct Sharing {
            view: Option<View>,
            sent: Rc<RefCell<Vec<Intent>>>,
            shares: BTreeMap<RunId, Status>,
        }

        impl Feed for Sharing {
            fn poll(&mut self) -> Option<View> {
                self.view.take()
            }

            fn act(&mut self, intent: Intent) {
                self.sent.borrow_mut().push(intent);
            }

            fn timeline(&self) -> Option<&BTreeMap<RunId, Status>> {
                Some(&self.shares)
            }
        }

        /// The states a test feed gives, made from the run shown.
        type Shares = fn(&RunId) -> BTreeMap<RunId, Status>;

        /// The detail of a demo task whose newest run wrote its result,
        /// on a feed whose shares are `shares` (`None`: one that cannot
        /// share).
        fn detail(shares: Option<Shares>) -> (Tasks, Rc<RefCell<Vec<Intent>>>, TextStore, RunId) {
            let view = demo::view();
            let run = view
                .runs
                .iter()
                .find(|v| matches!(v.result, ResultFact::Written { .. }))
                .expect("a run with a result")
                .clone();
            let sent = Rc::new(RefCell::new(Vec::new()));
            let feed: Box<dyn Feed> = match shares {
                Some(shares) => Box::new(Sharing {
                    view: Some(view),
                    sent: sent.clone(),
                    shares: shares(&run.run.run),
                }),
                None => Box::new(Recorder {
                    view: Some(view),
                    sent: sent.clone(),
                }),
            };
            let mut screen = Tasks::new(feed);
            let mut text = TextStore::new(Default::default());
            screen.prepare(window(), &mut text);
            screen.select_ref(&run.run.task);
            screen.open_detail(run.run.task.clone(), &mut text);
            screen.prepare(window(), &mut text);
            (screen, sent, text, run.run.run)
        }

        #[test]
        fn ac5_share_offers_two_outcomes_and_says_how_it_stands() {
            let (mut screen, log, mut text, run) = detail(Some(|_| BTreeMap::new()));
            let list = shown(&screen);
            at(&list, "共有");
            click(&mut screen, &mut text, "成功として共有");
            assert_eq!(
                sent(&log),
                [Intent::Share {
                    run: run.clone(),
                    outcome: Some(Outcome::Succeeded),
                }]
            );
            click(&mut screen, &mut text, "共有");
            assert_eq!(
                sent(&log),
                [Intent::Share {
                    run,
                    outcome: Some(Outcome::Unknown),
                }]
            );
        }

        #[test]
        fn ac5_state_visible() {
            let (mut screen, log, mut text, run) = detail(Some(|run| {
                BTreeMap::from([(
                    run.clone(),
                    Status::Failed {
                        message: "busy".into(),
                    },
                )])
            }));
            let list = shown(&screen);
            at(&list, "タイムライン: 投稿失敗（再試行）");
            // Why it failed, kept with it.
            at(&list, "busy");
            assert!(!list.iter().any(|t| t == "成功として共有"), "{list:?}");
            click(&mut screen, &mut text, "再試行");
            assert_eq!(sent(&log), [Intent::Share { run, outcome: None }]);

            let (screen, _, _, _) = detail(Some(|run| {
                BTreeMap::from([(
                    run.clone(),
                    Status::Published {
                        seq: 4,
                        post_id: "po-0000000000000004".into(),
                    },
                )])
            }));
            let list = shown(&screen);
            at(&list, "タイムライン: 投稿済み #4");
            // The post the server stored.
            at(&list, "postId po-0000000000000004");
            assert!(
                !list.iter().any(|t| t == "共有" || t == "再試行"),
                "{list:?}"
            );

            let (screen, _, _, _) =
                detail(Some(|run| BTreeMap::from([(run.clone(), Status::Pending)])));
            at(&shown(&screen), "タイムライン: 投稿待ち");

            // Waiting for a run: why, and 再試行.
            let (screen, _, _, _) = detail(Some(|run| {
                BTreeMap::from([(
                    run.clone(),
                    Status::Waiting {
                        why: tasks::timeline::NOT_REGISTERED.into(),
                    },
                )])
            }));
            let list = shown(&screen);
            at(&list, "タイムライン: 投稿待ち");
            at(&list, tasks::timeline::NOT_REGISTERED);
            at(&list, "再試行");

            // A feed that cannot share (the demo) offers none.
            let (screen, _, _, _) = detail(None);
            assert!(!shown(&screen).iter().any(|t| t.contains("共有")));
        }
    }
}
