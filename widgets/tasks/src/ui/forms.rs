//! The 新規 / 編集 / 実行 / リンクを追加 forms, shown in the side pane in place of the
//! detail. The screen inserts a form once when it opens, passes it every
//! message, and closes it on `Cancel` (or Escape) and on `Submit`, whose
//! intent it sends.

use std::cell::Cell;

use noa_gpui_core::style::FlexDirection;
use noa_gpui_core::widgets::{
    Button, ButtonClicked, Container, ControlSize, Label, ScrollView, SegmentMode,
    SegmentedControl, TextArea, TextField, TextSubmitted,
};
use noa_gpui_core::{Message, Role, Style, UiTree, WidgetId};

use tasks::model::types::{AgentKind, Isolation, Task, TaskRef};

use super::Intent;
use super::related::{LinkKind, check_link_id};
use super::style::{self, DETAIL_WIDTH, gap};

/// What a message did to the form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum FormOutcome {
    Pending,
    Cancel,
    Submit(Intent),
}

/// The isolation choices of 実行, in order: the default first.
const ISOLATIONS: [(&str, Isolation); 2] = [
    ("新しい worktree", Isolation::Worktree),
    ("この作業ツリー", Isolation::Here),
];

/// Which form, and what it was opened on.
#[derive(Debug, Clone)]
enum Kind {
    New,
    Edit {
        id: String,
    },
    Run {
        task: TaskRef,
        agents: Vec<AgentKind>,
    },
    /// リンクを追加 under `task`'s 関連成果.
    Link {
        task: TaskRef,
        title: String,
    },
}

/// The widgets [`Form::insert`] made that [`Form::handle`] reads.
#[derive(Debug, Clone, Copy)]
struct Ids {
    /// Title, body, labels (新規 / 編集).
    fields: Option<(WidgetId, WidgetId, WidgetId)>,
    /// Agent, isolation (実行).
    choices: Option<(WidgetId, WidgetId)>,
    /// 種類, ID, and the label saying why an ID is refused (リンクを追加).
    link: Option<(WidgetId, WidgetId, WidgetId)>,
    submit: WidgetId,
    cancel: WidgetId,
}

/// One open form and what has been typed or chosen in it.
#[derive(Debug)]
pub(super) struct Form {
    kind: Kind,
    heading: String,
    /// What the fields start with.
    title: String,
    body: String,
    labels: String,
    /// Set when the widgets are built (`insert` takes `&self`).
    ids: Cell<Option<Ids>>,
}

impl Form {
    fn new(kind: Kind, heading: String, title: String, body: String, labels: String) -> Form {
        Form {
            kind,
            heading,
            title,
            body,
            labels,
            ids: Cell::new(None),
        }
    }

    /// 新規: title, body, labels; submits `Intent::Create`.
    pub fn new_task() -> Form {
        Form::new(
            Kind::New,
            "新規タスク".to_owned(),
            String::new(),
            String::new(),
            String::new(),
        )
    }

    /// 編集 of local task `id`: its title, body and labels; submits `Intent::Edit`.
    pub fn edit(id: &str, task: &Task) -> Form {
        Form::new(
            Kind::Edit { id: id.to_owned() },
            format!("{id} を編集"),
            task.title.clone(),
            task.body.clone().unwrap_or_default(),
            task.labels.join(", "),
        )
    }

    /// 実行 on `task`: one of `agents` and the isolation (default `Isolation::Worktree`);
    /// submits `Intent::Run`.
    pub fn run(task: &Task, agents: &[AgentKind]) -> Form {
        Form::new(
            Kind::Run {
                task: task.r#ref.clone(),
                agents: agents.to_vec(),
            },
            format!("実行: {}", task.title),
            String::new(),
            String::new(),
            String::new(),
        )
    }

    /// リンクを追加 on `task` (titled `title`): 種類 and ID; submits
    /// `Intent::Link` once the ID has its kind's form, else says why.
    pub fn link(task: &TaskRef, title: &str) -> Form {
        Form::new(
            Kind::Link {
                task: task.clone(),
                title: title.to_owned(),
            },
            format!("リンクを追加: {title}"),
            String::new(),
            String::new(),
            String::new(),
        )
    }

    /// Builds the form's widgets under `host`.
    pub fn insert(&self, tree: &mut UiTree, host: WidgetId) {
        // Scrolls, as the detail does: the body field grows with its text,
        // and the side pane clips what does not fit.
        let column = tree.insert(
            Some(host),
            ScrollView::new(Style {
                flex_direction: FlexDirection::Column,
                gap: gap(8.0),
                ..style::fill()
            }),
        );
        tree.insert(
            Some(column),
            Label::new(self.heading.clone())
                .font_size(15.0)
                .weight(700)
                .wrap(true),
        );
        let field_width = DETAIL_WIDTH - 40.0;
        let muted = |tree: &mut UiTree, text: &str| {
            tree.insert(
                Some(column),
                Label::new(text).font_size(12.0).color(Role::Muted),
            );
        };
        let mut fields = None;
        let mut choices = None;
        let mut link = None;
        let submit_label = match &self.kind {
            Kind::New | Kind::Edit { .. } => {
                muted(tree, "タイトル（必須）");
                let title = tree.insert(Some(column), TextField::new("タイトル", field_width));
                muted(tree, "本文（Markdown）");
                let body = tree.insert(
                    Some(column),
                    TextArea::new("手順と完了条件")
                        .width(field_width)
                        .min_lines(8),
                );
                muted(tree, "ラベル（カンマ区切り）");
                let labels = tree.insert(Some(column), TextField::new("bug, ui", field_width));
                tree.update::<TextField, _>(title, |f, cx| f.set_text(cx, self.title.clone()));
                tree.update::<TextArea, _>(body, |f, cx| f.set_text(cx, self.body.clone()));
                tree.update::<TextField, _>(labels, |f, cx| f.set_text(cx, self.labels.clone()));
                // The form is shown where it goes (the screen shows its host
                // and the side pane next anyway); a hidden field takes no focus.
                let hidden: Vec<WidgetId> = tree
                    .ancestors(host)
                    .filter(|id| tree.is_hidden(*id))
                    .collect();
                for id in hidden {
                    tree.set_hidden(id, false);
                }
                tree.set_focus(Some(title));
                fields = Some((title, body, labels));
                if matches!(self.kind, Kind::New) {
                    "作成"
                } else {
                    "保存"
                }
            }
            Kind::Run { agents, .. } => {
                muted(tree, "エージェント");
                let agent = tree.insert(
                    Some(column),
                    SegmentedControl::new(
                        agents.iter().map(|a| a.command()),
                        SegmentMode::SelectOne,
                    )
                    .control_size(ControlSize::Small)
                    .with_selected(&[0]),
                );
                muted(tree, "作業場所");
                let isolation = tree.insert(
                    Some(column),
                    SegmentedControl::new(
                        ISOLATIONS.map(|(label, _)| label),
                        SegmentMode::SelectOne,
                    )
                    .control_size(ControlSize::Small)
                    .with_selected(&[0]),
                );
                muted(
                    tree,
                    "エージェントはボードのタブにフロートで開きます（自動承認なし）",
                );
                choices = Some((agent, isolation));
                "実行"
            }
            Kind::Link { .. } => {
                muted(tree, "種類");
                let kind = tree.insert(
                    Some(column),
                    SegmentedControl::new(
                        LinkKind::ALL.map(LinkKind::label),
                        SegmentMode::SelectOne,
                    )
                    .control_size(ControlSize::Small)
                    .with_selected(&[0]),
                );
                muted(tree, "ID（rev-…、ver-…、7 文字以上の SHA、run-…）");
                let id = tree.insert(Some(column), TextField::new("ID", field_width));
                let why = tree.insert(
                    Some(column),
                    Label::new("")
                        .font_size(12.0)
                        .wrap(true)
                        .color(noa_gpui_scene::Color::hex(0xff9f0a)),
                );
                // As 新規: shown where it goes, so the ID field takes focus.
                let hidden: Vec<WidgetId> = tree
                    .ancestors(host)
                    .filter(|id| tree.is_hidden(*id))
                    .collect();
                for id in hidden {
                    tree.set_hidden(id, false);
                }
                tree.set_focus(Some(id));
                link = Some((kind, id, why));
                "追加"
            }
        };
        let buttons = tree.insert(Some(column), Container::new(style::row(8.0)));
        let submit = tree.insert(Some(buttons), Button::new(submit_label));
        let cancel = tree.insert(Some(buttons), Button::new("キャンセル (Esc)"));
        self.ids.set(Some(Ids {
            fields,
            choices,
            link,
            submit,
            cancel,
        }));
    }

    /// Takes in `message` (from any widget; the form's own are recognised).
    pub fn handle(&mut self, message: &Message, tree: &mut UiTree) -> FormOutcome {
        let Some(ids) = self.ids.get() else {
            return FormOutcome::Pending;
        };
        let clicked = message.downcast_ref::<ButtonClicked>().is_some();
        if clicked && message.source == ids.cancel {
            return FormOutcome::Cancel;
        }
        // Return in the title (the ID) submits too.
        let submitted = message.downcast_ref::<TextSubmitted>().is_some()
            && (ids
                .fields
                .is_some_and(|(title, _, _)| title == message.source)
                || ids.link.is_some_and(|(_, id, _)| id == message.source));
        if !(clicked && message.source == ids.submit) && !submitted {
            return FormOutcome::Pending;
        }
        if let (Kind::Link { task, title }, Some((kind, id, why))) = (&self.kind, ids.link) {
            let kind = chosen(tree, kind)
                .and_then(|i| LinkKind::ALL.get(i).copied())
                .unwrap_or(LinkKind::Review);
            let typed = tree
                .get::<TextField>(id)
                .map(|f| f.text().to_owned())
                .unwrap_or_default();
            return match check_link_id(kind, &typed) {
                Ok(id) => FormOutcome::Submit(Intent::Link {
                    task: task.clone(),
                    title: title.clone(),
                    kind,
                    id,
                }),
                Err(reason) => {
                    tree.update::<Label, _>(why, |l, cx| l.set_text(cx, reason));
                    FormOutcome::Pending
                }
            };
        }
        self.intent(&ids, tree)
            .map_or(FormOutcome::Pending, FormOutcome::Submit)
    }

    /// What the form submits now; `None` while it may not (an empty title).
    fn intent(&self, ids: &Ids, tree: &UiTree) -> Option<Intent> {
        match &self.kind {
            Kind::New | Kind::Edit { .. } => {
                let (title, body, labels) = ids.fields?;
                let title = tree.get::<TextField>(title)?.text().trim().to_owned();
                if title.is_empty() {
                    return None;
                }
                let body = tree.get::<TextArea>(body)?.text().to_owned();
                let labels = tree
                    .get::<TextField>(labels)?
                    .text()
                    .split(',')
                    .map(str::trim)
                    .filter(|l| !l.is_empty())
                    .map(str::to_owned)
                    .collect();
                Some(match &self.kind {
                    Kind::Edit { id } => Intent::Edit {
                        id: id.clone(),
                        title,
                        body,
                        labels,
                    },
                    _ => Intent::Create {
                        title,
                        body,
                        labels,
                    },
                })
            }
            Kind::Run { task, agents } => {
                let (agent, isolation) = ids.choices?;
                let chosen = |id| chosen(tree, id).unwrap_or(0);
                Some(Intent::Run {
                    task: task.clone(),
                    agent: *agents.get(chosen(agent))?,
                    isolation: ISOLATIONS.get(chosen(isolation))?.1,
                })
            }
            // Submitted in `handle`, where a refused ID is said.
            Kind::Link { .. } => None,
        }
    }
}

/// The segment chosen in the control `id`.
fn chosen(tree: &UiTree, id: WidgetId) -> Option<usize> {
    tree.get::<SegmentedControl>(id)
        .and_then(|c| c.selected().iter().position(|on| *on))
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use noa_gpui_core::widgets::Label;
    use noa_gpui_core::{Event, Key, Modifiers};
    use noa_gpui_platform::Screen;
    use noa_gpui_scene::Size;
    use noa_gpui_text::TextStore;

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

    fn down(screen: &mut Tasks, text: &mut TextStore, key: Key) {
        screen.dispatch(
            Event::KeyDown {
                key,
                modifiers: Modifiers::default(),
            },
            text,
        );
        screen.prepare(window(), text);
    }

    fn typed(screen: &mut Tasks, text: &mut TextStore, s: &str) {
        screen.dispatch(Event::Text(s.to_owned()), text);
        screen.prepare(window(), text);
    }

    /// Presses the form's button labelled `label` from the keyboard.
    fn press(screen: &mut Tasks, text: &mut TextStore, label: &str) {
        let button = find(&screen.tree, screen.form_host, &|tree, id| {
            tree.get::<Button>(id).is_some_and(|b| b.label() == label)
        })
        .unwrap_or_else(|| panic!("no button {label:?}"));
        screen.tree.set_focus(Some(button));
        down(screen, text, Key::Space);
    }

    fn find(
        tree: &UiTree,
        id: WidgetId,
        pred: &dyn Fn(&UiTree, WidgetId) -> bool,
    ) -> Option<WidgetId> {
        if pred(tree, id) {
            return Some(id);
        }
        tree.children(id).iter().find_map(|c| find(tree, *c, pred))
    }

    fn all<W: noa_gpui_core::Widget>(tree: &UiTree, id: WidgetId, out: &mut Vec<WidgetId>) {
        if tree.get::<W>(id).is_some() {
            out.push(id);
        }
        for c in tree.children(id) {
            all::<W>(tree, *c, out);
        }
    }

    fn fields(screen: &Tasks) -> (WidgetId, WidgetId, WidgetId) {
        screen
            .form
            .as_ref()
            .unwrap()
            .ids
            .get()
            .unwrap()
            .fields
            .unwrap()
    }

    fn sent(log: &Rc<RefCell<Vec<Intent>>>) -> Vec<Intent> {
        std::mem::take(&mut *log.borrow_mut())
    }

    #[test]
    fn ac20_new_refuses_an_empty_title_and_submits_create() {
        let (mut screen, log, mut text) = screen_on(demo::view());
        screen.dispatch(
            Event::KeyDown {
                key: Key::Char('n'),
                modifiers: Modifiers::default(),
            },
            &mut text,
        );
        screen.dispatch(Event::Text("n".into()), &mut text);
        screen.prepare(window(), &mut text);
        assert!(screen.form.is_some());
        let (title, body, labels) = fields(&screen);
        // The title has focus, and the `n` that opened the form is not in it.
        assert_eq!(screen.tree.focused(), Some(title));
        assert_eq!(screen.tree.get::<TextField>(title).unwrap().text(), "");
        // An empty title (spaces only) is refused: the form stays.
        typed(&mut screen, &mut text, "   ");
        press(&mut screen, &mut text, "作成");
        assert!(screen.form.is_some());
        assert!(sent(&log).is_empty());

        screen.tree.set_focus(Some(title));
        typed(&mut screen, &mut text, "フォントを直す");
        screen.tree.set_focus(Some(body));
        typed(&mut screen, &mut text, "手順");
        screen.tree.set_focus(Some(labels));
        typed(&mut screen, &mut text, "bug, ui ,");
        press(&mut screen, &mut text, "作成");
        assert_eq!(
            sent(&log),
            [Intent::Create {
                title: "フォントを直す".into(),
                body: "手順".into(),
                labels: vec!["bug".into(), "ui".into()],
            }]
        );
        assert!(screen.form.is_none(), "closed on submit");
    }

    #[test]
    fn ac20_a_long_body_scrolls_and_the_buttons_stay_reachable_at_minimum_height() {
        let small = Size::new(1280.0, 600.0);
        let mut view = demo::view();
        let long: String = (1..=40).map(|n| format!("{n} 行目の手順\n")).collect();
        view.tasks[0].body = Some(long.clone());
        let task = view.tasks[0].clone();
        let (mut screen, log, mut text) = screen_on(view);
        screen.prepare(small, &mut text);
        screen.open_form(Form::edit("t-1", &task), &mut text);
        screen.prepare(small, &mut text);
        let ids = screen.form.as_ref().unwrap().ids.get().unwrap();
        let (_, _, labels) = ids.fields.unwrap();
        for id in [labels, ids.submit, ids.cancel] {
            // Focus brings it into view, and it is where a click lands.
            screen.tree.set_focus(Some(id));
            screen.prepare(small, &mut text);
            let rect = screen.tree.window_rect(id).unwrap();
            assert!(
                rect.y >= 0.0 && rect.y + rect.height <= small.height,
                "{id:?} at {rect:?}"
            );
            let center =
                noa_gpui_scene::Point::new(rect.x + rect.width / 2.0, rect.y + rect.height / 2.0);
            let hit = screen.tree.hit_test(center).unwrap();
            assert!(
                hit == id || screen.tree.ancestors(hit).any(|a| a == id),
                "{id:?} is covered or clipped: {hit:?}"
            );
        }
        screen.tree.set_focus(Some(ids.submit));
        down(&mut screen, &mut text, Key::Space);
        let got = sent(&log);
        assert!(
            matches!(got.as_slice(), [Intent::Edit { body, .. }] if *body == long),
            "{got:?}"
        );
    }

    #[test]
    fn ac20_edit_starts_from_the_task_and_submits_edit() {
        let view = demo::view();
        let task = view.tasks[0].clone();
        let (mut screen, log, mut text) = screen_on(view);
        screen.open_form(Form::edit("t-1", &task), &mut text);
        screen.prepare(window(), &mut text);
        let (title, body, labels) = fields(&screen);
        assert_eq!(
            screen.tree.get::<TextField>(title).unwrap().text(),
            task.title
        );
        assert_eq!(
            Some(screen.tree.get::<TextArea>(body).unwrap().text().to_owned()),
            task.body
        );
        assert_eq!(
            screen.tree.get::<TextField>(labels).unwrap().text(),
            "config"
        );
        // Return in the title submits.
        screen.tree.set_focus(Some(title));
        typed(&mut screen, &mut text, "!");
        down(&mut screen, &mut text, Key::Enter);
        let got = sent(&log);
        let [
            Intent::Edit {
                id,
                title,
                body,
                labels,
            },
        ] = got.as_slice()
        else {
            panic!("{got:?}");
        };
        assert_eq!(id, "t-1");
        assert_eq!(Some(body), task.body.as_ref());
        assert_eq!(labels, &["config"]);
        assert!(
            title.starts_with(&task.title) && title.len() > task.title.len(),
            "{title}"
        );
    }

    #[test]
    fn ac20_run_lists_only_the_installed_agents_defaults_to_a_new_worktree_and_escape_cancels() {
        let mut view = demo::view();
        view.agents = Some(vec![AgentKind::Codex, AgentKind::Agy]);
        let (mut screen, log, mut text) = screen_on(view);
        let t1 = TaskRef::Local("t-1".into());
        screen.select_ref(&t1);
        down(&mut screen, &mut text, Key::Char('x'));
        assert!(screen.form.is_some());
        let mut controls = Vec::new();
        all::<SegmentedControl>(&screen.tree, screen.form_host, &mut controls);
        let [agent, isolation] = controls[..] else {
            panic!("{controls:?}");
        };
        let mut labels = Vec::new();
        all::<Label>(&screen.tree, screen.form_host, &mut labels);
        let texts: Vec<String> = labels
            .iter()
            .map(|id| screen.tree.get::<Label>(*id).unwrap().text().to_owned())
            .collect();
        assert!(
            texts
                .iter()
                .any(|t| t.contains(&screen.task(&t1).unwrap().title))
        );
        let segment = |id| {
            screen
                .tree
                .get::<SegmentedControl>(id)
                .unwrap()
                .selected()
                .to_vec()
        };
        assert_eq!(segment(agent), [true, false], "codex and agy only");
        assert_eq!(segment(isolation), [true, false], "新しい worktree first");
        press(&mut screen, &mut text, "実行");
        assert_eq!(
            sent(&log),
            [Intent::Run {
                task: t1.clone(),
                agent: AgentKind::Codex,
                isolation: Isolation::Worktree,
            }]
        );

        // この作業ツリー and agy, chosen from the keyboard.
        down(&mut screen, &mut text, Key::Char('x'));
        let mut controls = Vec::new();
        all::<SegmentedControl>(&screen.tree, screen.form_host, &mut controls);
        for id in [controls[0], controls[1]] {
            screen.tree.set_focus(Some(id));
            down(&mut screen, &mut text, Key::ArrowRight);
            down(&mut screen, &mut text, Key::Space);
        }
        press(&mut screen, &mut text, "実行");
        assert_eq!(
            sent(&log),
            [Intent::Run {
                task: t1,
                agent: AgentKind::Agy,
                isolation: Isolation::Here,
            }]
        );

        // Escape cancels: nothing sent; so does キャンセル.
        down(&mut screen, &mut text, Key::Char('x'));
        assert!(screen.form.is_some());
        down(&mut screen, &mut text, Key::Escape);
        assert!(screen.form.is_none());
        down(&mut screen, &mut text, Key::Char('n'));
        press(&mut screen, &mut text, "キャンセル (Esc)");
        assert!(screen.form.is_none());
        assert!(sent(&log).is_empty());
    }
}
