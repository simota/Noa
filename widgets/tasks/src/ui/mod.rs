//! The Tasks screen: a header (title, repository, notice, 新規, 再取得), the
//! filter line (layout, source, label, text), the GitHub source's header,
//! the tasks in the chosen layout (the four columns, a list, a timeline or
//! a calendar: [`views`]), and the side pane (a task's detail, or a form).
//!
//! The screen is source-agnostic: it renders a [`View`] and sends
//! [`Intent`]s to a [`Feed`] (`LiveFeed`, or `DemoFeed` for `--demo`), and
//! learns of their outcome from the next `View`'s `notice`.
//!
//! Keys, while no text field has focus: `v` next layout, `h`/`l` column
//! (the board), `j`/`k` card or row (and the arrows), `[`/`]` month (the
//! calendar), `Enter` detail, `n` 新規, `x` 実行, `d` 完了にする /
//! 再開 (local tasks), `r` 再取得, `/` filter, `o` GitHub で開く, `a`
//! 関連を追加, Escape closes the form, leaves the filter or closes the
//! detail. While a menu
//! (or another overlay) has focus, every key is the menu's.

pub mod columns;
pub mod dates;
pub mod detail;
pub mod forms;
pub mod style;
pub mod views;

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use noa_gpui_core::style::{FlexDirection, Rect as LayoutRect, length};
use noa_gpui_core::widgets::{
    Button, ButtonClicked, Container, ControlSize, Label, PopUpButton, ScrollView, SegmentMode,
    SegmentedControl, SelectionChanged, TextArea, TextChanged, TextField, TextSubmitted,
};
use noa_gpui_core::{
    Appearance, Event, Key, Message, Modifiers, Role, Style, Theme, UiTree, WidgetId,
};
use noa_gpui_platform::Screen;
use noa_gpui_scene::Color;
use noa_gpui_text::TextStore;

use tasks::model::column::{Filter, column, matches};
use tasks::model::types::{
    AgentKind, Column, DONE_DAYS, GitHubView, Isolation, Millis, RunId, RunView, Source, State,
    Task, TaskRef,
};
use tasks::related::Related;
use tasks::task_links::SessionKind;
use tasks::timeline::Status;

use noa_ipc::timeline::Outcome;

use columns::Card;
use forms::{Form, FormOutcome};
use style::{DETAIL_WIDTH, GUTTER, gap};
pub use views::Layout;

/// Everything the screen shows, as the feed last saw it.
#[derive(Debug, Clone, PartialEq)]
pub struct View {
    pub repo: PathBuf,
    /// The local tasks; the issues are `github.tasks`.
    pub tasks: Vec<Task>,
    pub runs: Vec<RunView>,
    pub github: GitHubView,
    /// The agents installed (`None` while that is still being asked).
    pub agents: Option<Vec<AgentKind>>,
    /// The outcome of the last intent, or anything else to tell the person.
    pub notice: Option<String>,
    pub now: Millis,
}

/// What the person asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Intent {
    Create {
        title: String,
        body: String,
        labels: Vec<String>,
    },
    /// Other header keys of the file are kept.
    Edit {
        id: String,
        title: String,
        body: String,
        labels: Vec<String>,
    },
    SetState {
        id: String,
        state: State,
    },
    Run {
        task: TaskRef,
        agent: AgentKind,
        isolation: Isolation,
    },
    OpenPane(RunId),
    ClosePane(RunId),
    OpenWorktree(RunId),
    OpenUrl(String),
    Refresh,
    LoadDetail(u64),
    /// A linked Review or Verify session, in its own window.
    OpenSession {
        kind: SessionKind,
        session: String,
    },
    /// 関連を追加: what the person typed (a PR's URL or number).
    AddLink {
        task: TaskRef,
        input: String,
    },
    /// 外す: a PR link's id or a session's.
    RemoveLink {
        task: TaskRef,
        id: String,
    },
    /// 共有: the run's result on its board's timeline, with the outcome
    /// the person picked; `None` sends the staged share again (再試行).
    Share {
        run: RunId,
        outcome: Option<Outcome>,
    },
}

/// Where the screen's data comes from.
pub trait Feed {
    /// A new View when anything changed; never blocks.
    fn poll(&mut self) -> Option<View>;
    /// Outcomes come back as View.notice.
    fn act(&mut self, intent: Intent);
    /// Each task's 関連成果, as of the last View `poll` gave (a feed that
    /// links nothing has none).
    fn related(&self) -> Option<&BTreeMap<TaskRef, Related>> {
        None
    }
    /// Where each shared run's Board Timeline post stands, as of the last
    /// View `poll` gave (`None`: this feed cannot share, so no 共有).
    fn timeline(&self) -> Option<&BTreeMap<RunId, Status>> {
        None
    }
}

/// The filter line's source choices, in order.
const SOURCES: [(&str, Option<Source>); 4] = [
    ("すべて", None),
    ("ローカル", Some(Source::Local)),
    ("GitHub", Some(Source::GitHub)),
    ("コード", Some(Source::Code)),
];
/// The label filter's first choice.
const ALL_LABELS: &str = "すべてのラベル";
const FAILURE: Color = Color::hex(0xff9f0a);
const DAY_MS: Millis = 86_400_000;

pub struct Tasks {
    tree: UiTree,
    feed: Box<dyn Feed>,
    view: View,
    filter: Filter,
    /// The column the selection is in, in [`Column::ALL`] order.
    sel_col: usize,
    selected: Option<TaskRef>,
    /// The task whose detail the side pane shows.
    detail: Option<TaskRef>,
    /// A `--select` not listed yet, opened once a View lists it; dropped
    /// at the person's first key or press.
    select_later: Option<String>,
    form: Option<Form>,
    /// What the screen itself has to say (a refused 実行), until the next
    /// key or press, or a new notice from the feed.
    own_notice: Option<String>,
    /// A key taken as a shortcut, whose text event is dropped.
    swallow: Option<char>,
    notice: WidgetId,
    repo_label: WidgetId,
    new_button: WidgetId,
    refresh_button: WidgetId,
    source: WidgetId,
    label_host: WidgetId,
    label_popup: WidgetId,
    /// The label filter's choices after [`ALL_LABELS`].
    label_choices: Vec<String>,
    text: WidgetId,
    gh_status: WidgetId,
    gh_failure: WidgetId,
    columns_host: WidgetId,
    side: WidgetId,
    detail_host: WidgetId,
    form_host: WidgetId,
    columns: [WidgetId; 4],
    /// Each column's cards' scroll view, once built: its position is kept
    /// across a rebuild.
    column_scrolls: Option<[WidgetId; 4]>,
    /// The detail's scroll view and whose detail it shows: its position is
    /// kept while the same task's detail is rebuilt.
    detail_scroll: Option<(TaskRef, WidgetId)>,
    cards: Vec<(WidgetId, TaskRef)>,
    detail_actions: Vec<(WidgetId, Intent)>,
    layout: Layout,
    layout_control: WidgetId,
    /// The calendar's month, from this one.
    month: i32,
    /// The calendar's 前の月 / 今月 / 次の月.
    month_buttons: Option<[WidgetId; 3]>,
    /// The list's, timeline's or calendar's scroll view and which it is
    /// (the calendar's month with it): its position is kept across a
    /// rebuild of the same one.
    layout_scroll: Option<((Layout, i32), WidgetId)>,
}

impl Tasks {
    pub fn new(feed: Box<dyn Feed>) -> Tasks {
        let mut tree = UiTree::new();
        tree.set_theme(Theme::for_appearance(Appearance::Dark));
        let root = tree.insert(
            None,
            Container::new(Style {
                flex_direction: FlexDirection::Column,
                size: style::full(),
                ..Default::default()
            })
            .background(Role::Background),
        );

        let header = style::bar(&mut tree, root, 14.0);
        tree.insert(
            Some(header),
            Label::new("タスク").font_size(15.0).weight(700),
        );
        let repo_label = tree.insert(
            Some(header),
            Label::new("").font_size(12.0).color(Role::Muted),
        );
        style::spacer(&mut tree, header);
        let notice = tree.insert(Some(header), Label::new("").font_size(12.0));
        let new_button = tree.insert(Some(header), Button::new("新規 (N)"));
        let refresh_button = tree.insert(Some(header), Button::new("再取得 (R)"));
        style::hairline(&mut tree, root);

        let filters = style::bar(&mut tree, root, 10.0);
        let layout_control = tree.insert(
            Some(filters),
            SegmentedControl::new(Layout::ALL.map(Layout::label), SegmentMode::SelectOne)
                .control_size(ControlSize::Small)
                .with_selected(&[0]),
        );
        tree.insert(
            Some(filters),
            Label::new("取得元").font_size(12.0).color(Role::Muted),
        );
        let source = tree.insert(
            Some(filters),
            SegmentedControl::new(SOURCES.map(|(label, _)| label), SegmentMode::SelectOne)
                .control_size(ControlSize::Small)
                .with_selected(&[0]),
        );
        let label_host = tree.insert(Some(filters), Container::new(style::row(0.0)));
        let label_popup = tree.insert(
            Some(label_host),
            PopUpButton::new([ALL_LABELS]).control_size(ControlSize::Small),
        );
        let text = tree.insert(
            Some(filters),
            TextField::new("タイトル・ID・#番号で絞り込み（/）", 300.0),
        );
        style::hairline(&mut tree, root);

        let github = style::bar(&mut tree, root, 10.0);
        tree.insert(
            Some(github),
            Label::new("GitHub").font_size(12.0).weight(700),
        );
        let gh_status = tree.insert(
            Some(github),
            Label::new("").font_size(12.0).color(Role::Muted),
        );
        let gh_failure = tree.insert(Some(github), Label::new("").font_size(12.0).color(FAILURE));
        style::hairline(&mut tree, root);

        let body = tree.insert(
            Some(root),
            Container::new(Style {
                flex_direction: FlexDirection::Row,
                padding: LayoutRect::length(GUTTER),
                gap: gap(GUTTER),
                ..style::fill()
            }),
        );
        let columns_host = tree.insert(
            Some(body),
            Container::new(Style {
                flex_direction: FlexDirection::Row,
                gap: gap(12.0),
                ..style::fill()
            }),
        );
        let side = tree.insert(
            Some(body),
            Container::new(Style {
                flex_direction: FlexDirection::Column,
                flex_shrink: 0.0,
                size: noa_gpui_core::style::Size {
                    width: length(DETAIL_WIDTH),
                    height: noa_gpui_core::style::percent(1.0),
                },
                padding: LayoutRect::length(12.0),
                ..Default::default()
            })
            .background(Role::Surface)
            .border(1.0, Role::Separator)
            .radius(10.0)
            .clip(true),
        );
        let detail_host = tree.insert(Some(side), Container::new(style::fill()));
        let form_host = tree.insert(Some(side), Container::new(style::fill()));

        let mut screen = Tasks {
            tree,
            feed,
            view: View {
                repo: PathBuf::new(),
                tasks: Vec::new(),
                runs: Vec::new(),
                github: GitHubView::default(),
                agents: None,
                notice: None,
                now: 0,
            },
            filter: Filter::default(),
            sel_col: 0,
            selected: None,
            detail: None,
            select_later: None,
            form: None,
            own_notice: None,
            swallow: None,
            notice,
            repo_label,
            new_button,
            refresh_button,
            source,
            label_host,
            label_popup,
            label_choices: Vec::new(),
            text,
            gh_status,
            gh_failure,
            columns_host,
            side,
            detail_host,
            form_host,
            columns: [repo_label; 4],
            column_scrolls: None,
            detail_scroll: None,
            cards: Vec::new(),
            detail_actions: Vec::new(),
            layout: Layout::Board,
            layout_control,
            month: 0,
            month_buttons: None,
            layout_scroll: None,
        };
        // Nothing is laid out yet: any store serves the first build.
        let mut text = TextStore::new(Default::default());
        if let Some(view) = screen.feed.poll() {
            screen.view = view;
        }
        screen.rebuild(&mut text);
        screen
    }

    /// Opens the detail of the task `id` names (`t-…`, or `#N` for an
    /// issue); whether there is one.
    pub fn select(&mut self, id: &str, text: &mut TextStore) -> bool {
        let Some(task) = self.all_tasks().into_iter().find(|t| names(&t.r#ref, id)) else {
            return false;
        };
        let r = task.r#ref.clone();
        self.filter = Filter::default();
        self.rebuild(text);
        self.select_ref(&r);
        self.open_detail(r, text);
        true
    }

    /// Shows the tasks in `layout` (`v`, the layout control, `--layout`).
    pub fn set_layout(&mut self, layout: Layout, text: &mut TextStore) {
        self.layout = layout;
        let at = Layout::ALL.iter().position(|l| *l == layout).unwrap_or(0);
        self.tree
            .update::<SegmentedControl, _>(self.layout_control, |c, cx| c.set_selected(cx, &[at]));
        self.rebuild(text);
    }

    /// `--select` in the live window: opens task `id`'s detail now, or
    /// once a later View lists it (issues and the first scan arrive after
    /// the window opens).
    pub fn select_when_listed(&mut self, id: &str, text: &mut TextStore) {
        if !self.select(id, text) {
            self.select_later = Some(id.to_owned());
        }
    }

    // ------------------------------------------------------------ the view

    fn all_tasks(&self) -> Vec<&Task> {
        all_tasks(&self.view)
    }

    fn last_run(&self, r: &TaskRef) -> Option<&RunView> {
        last_run(&self.view, r)
    }

    fn column_refs(&self) -> [Vec<TaskRef>; 4] {
        lists(&self.view, &self.filter)
            .map(|list| list.iter().map(|c| c.task.r#ref.clone()).collect())
    }

    fn task(&self, r: &TaskRef) -> Option<&Task> {
        self.all_tasks().into_iter().find(|t| &t.r#ref == r)
    }

    fn selected_task(&self) -> Option<&Task> {
        self.selected.as_ref().and_then(|r| self.task(r))
    }

    // ------------------------------------------------------------ building

    /// `text` must be the store the tree was laid out with: removed widgets
    /// release their text layouts in it.
    fn rebuild(&mut self, text: &mut TextStore) {
        self.keep_selection();
        self.show_notice();
        let repo = self.view.repo.display().to_string();
        self.tree
            .update::<Label, _>(self.repo_label, |l, cx| l.set_text(cx, repo));
        let (status, failure) = github_header(&self.view.github, self.view.now);
        self.tree
            .update::<Label, _>(self.gh_status, |l, cx| l.set_text(cx, status));
        let failed = failure.is_empty();
        self.tree
            .update::<Label, _>(self.gh_failure, |l, cx| l.set_text(cx, failure));
        self.tree.set_hidden(self.gh_failure, failed);
        self.rebuild_labels(text);

        if self.layout != Layout::Board {
            self.rebuild_layout(text);
            self.rebuild_detail(text);
            return;
        }
        self.month_buttons = None;
        // A rebuild (a new View each minute, a key) keeps where the reader
        // was in each column.
        let kept = self
            .column_scrolls
            .map(|ids| ids.map(|id| scroll_position(&self.tree, id)));
        clear(&mut self.tree, self.columns_host, text);
        let lists = lists(&self.view, &self.filter);
        let built = columns::build(
            &mut self.tree,
            self.columns_host,
            &lists,
            self.selected.as_ref(),
            self.view.now,
        );
        self.columns = built.columns;
        self.cards = built.cards;
        if let Some(kept) = kept {
            for (id, at) in built.scrolls.into_iter().zip(kept) {
                set_scroll_position(&mut self.tree, id, at);
            }
        }
        self.column_scrolls = Some(built.scrolls);
        self.rebuild_detail(text);
    }

    /// The list, timeline or calendar in place of the columns.
    fn rebuild_layout(&mut self, text: &mut TextStore) {
        let which = (self.layout, self.month);
        let kept = self
            .layout_scroll
            .take()
            .filter(|(w, _)| *w == which)
            .map(|(_, id)| scroll_position(&self.tree, id));
        clear(&mut self.tree, self.columns_host, text);
        let pane = self.tree.insert(
            Some(self.columns_host),
            Container::new(Style {
                flex_direction: FlexDirection::Column,
                gap: gap(8.0),
                ..style::fill()
            }),
        );
        let cards = flat(&self.view, &self.filter);
        let now = self.view.now;
        let built = match self.layout {
            Layout::List => views::list(&mut self.tree, pane, &cards, self.selected.as_ref(), now),
            Layout::Timeline => views::timeline(
                &mut self.tree,
                pane,
                &cards,
                &self.view.runs,
                self.selected.as_ref(),
                now,
            ),
            Layout::Calendar | Layout::Board => {
                let tasks: Vec<&Task> = cards.iter().map(|c| c.task).collect();
                views::calendar(
                    &mut self.tree,
                    pane,
                    &tasks,
                    &self.view.runs,
                    self.month,
                    now,
                )
            }
        };
        if let Some(at) = kept {
            set_scroll_position(&mut self.tree, built.scroll, at);
        }
        self.layout_scroll = Some((which, built.scroll));
        self.month_buttons = built.months;
        self.columns = [pane; 4];
        self.column_scrolls = None;
        self.cards = built.rows;
        // A task this layout does not show (the timeline's and the
        // calendar's are dated) is not what a shortcut acts on.
        if self.selected.as_ref().is_some_and(|r| !self.shows(r)) {
            self.selected = None;
        }
    }

    /// The screen's own notice, else the feed's.
    fn show_notice(&mut self) {
        let notice = self
            .own_notice
            .clone()
            .or_else(|| self.view.notice.clone())
            .unwrap_or_default();
        self.tree
            .update::<Label, _>(self.notice, |l, cx| l.set_text(cx, notice));
    }

    /// The label filter's choices, from the tasks' labels; rebuilt only
    /// when they changed.
    fn rebuild_labels(&mut self, text: &mut TextStore) {
        let labels: BTreeSet<String> = self
            .all_tasks()
            .iter()
            .flat_map(|t| t.labels.iter().cloned())
            .chain(self.filter.label.clone())
            .collect();
        let labels: Vec<String> = labels.into_iter().collect();
        if labels == self.label_choices {
            return;
        }
        let at = self
            .filter
            .label
            .as_ref()
            .and_then(|l| labels.iter().position(|c| c == l))
            .map_or(0, |i| i + 1);
        clear(&mut self.tree, self.label_host, text);
        self.label_popup = self.tree.insert(
            Some(self.label_host),
            PopUpButton::new(std::iter::once(ALL_LABELS.to_owned()).chain(labels.iter().cloned()))
                .control_size(ControlSize::Small)
                .selected_index(at),
        );
        self.label_choices = labels;
    }

    fn rebuild_detail(&mut self, text: &mut TextStore) {
        // The same task's detail rebuilt keeps its position; another's starts
        // at the top.
        let kept = self
            .detail_scroll
            .take()
            .filter(|(r, _)| self.detail.as_ref() == Some(r))
            .map(|(_, id)| scroll_position(&self.tree, id));
        clear(&mut self.tree, self.detail_host, text);
        self.detail_actions.clear();
        let shown =
            self.form.is_none() && self.detail.as_ref().is_some_and(|r| self.task(r).is_some());
        if shown && let Some(r) = self.detail.clone() {
            let task = self.task(&r).cloned();
            if let Some(task) = task {
                let mut runs: Vec<&RunView> =
                    self.view.runs.iter().filter(|v| v.run.task == r).collect();
                runs.sort_by(|a, b| b.run.started.cmp(&a.run.started));
                let loaded = match &r {
                    TaskRef::GitHub { number, .. } => self.view.github.details.get(number),
                    TaskRef::Local(_) | TaskRef::Code { .. } => None,
                };
                let related = self.feed.related().and_then(|all| all.get(&r));
                self.detail_actions = detail::insert(
                    &mut self.tree,
                    self.detail_host,
                    &task,
                    (&runs, self.feed.timeline()),
                    loaded,
                    related,
                    self.view.now,
                );
                if let Some(&scroll) = self.tree.children(self.detail_host).first() {
                    if let Some(at) = kept {
                        set_scroll_position(&mut self.tree, scroll, at);
                    }
                    self.detail_scroll = Some((r, scroll));
                }
            }
        }
        self.tree.set_hidden(self.detail_host, !shown);
        self.tree.set_hidden(self.form_host, self.form.is_none());
        self.tree
            .set_hidden(self.side, !shown && self.form.is_none());
    }

    /// Keeps the selection on a card that is shown: the same task when it
    /// still is, else the first card of the selected column.
    fn keep_selection(&mut self) {
        // The list, timeline and calendar keep a selection only while one
        // of their rows shows it: checked once their rows are built.
        if self.layout != Layout::Board {
            return;
        }
        let lists = self.column_refs();
        if let Some(r) = &self.selected
            && let Some(col) = lists.iter().position(|l| l.contains(r))
        {
            self.sel_col = col;
            return;
        }
        self.selected = lists[self.sel_col].first().cloned();
    }

    fn select_ref(&mut self, r: &TaskRef) {
        if self.layout != Layout::Board {
            if self.shows(r) {
                self.selected = Some(r.clone());
            }
            return;
        }
        if let Some(col) = self.column_refs().iter().position(|l| l.contains(r)) {
            self.sel_col = col;
            self.selected = Some(r.clone());
        }
    }

    /// Whether a row or card on screen shows task `r`.
    fn shows(&self, r: &TaskRef) -> bool {
        self.cards.iter().any(|(_, x)| x == r)
    }

    // ------------------------------------------------------------ actions

    fn act(&mut self, intent: Intent) {
        self.feed.act(intent);
    }

    /// Loads an issue's detail unless it is loaded: a failed load is tried
    /// again.
    fn open_detail(&mut self, r: TaskRef, text: &mut TextStore) {
        if let TaskRef::GitHub { number, .. } = &r
            && !matches!(self.view.github.details.get(number), Some(Ok(_)))
        {
            self.act(Intent::LoadDetail(*number));
        }
        self.detail = Some(r);
        self.rebuild(text);
    }

    fn open_form(&mut self, form: Form, text: &mut TextStore) {
        clear(&mut self.tree, self.form_host, text);
        form.insert(&mut self.tree, self.form_host);
        self.form = Some(form);
        self.rebuild_detail(text);
    }

    fn close_form(&mut self, text: &mut TextStore) {
        self.form = None;
        clear(&mut self.tree, self.form_host, text);
        self.rebuild_detail(text);
    }

    /// 実行 on the selected task: the form, unless it is running or no
    /// agent is known.
    fn run_form(&mut self, task: &Task, text: &mut TextStore) {
        // The rule the detail's 実行 button and the run itself follow.
        if let Err(why) = tasks::runs::may_start(task, self.last_run(&task.r#ref)) {
            self.own_notice = Some(why);
            self.rebuild(text);
            return;
        }
        match self.view.agents.clone() {
            None => {
                self.own_notice = Some("インストール済みのエージェントを確認しています".to_owned());
                self.rebuild(text);
            }
            Some(agents) if agents.is_empty() => {
                self.own_notice = Some("起動できるエージェントが見つかりません".to_owned());
                self.rebuild(text);
            }
            Some(agents) => self.open_form(Form::run(task, &agents), text),
        }
    }

    /// A shortcut; whether it was one.
    fn press(&mut self, key: &Key, text: &mut TextStore) -> bool {
        if self.layout != Layout::Board
            && let Some(taken) = self.press_in_layout(key, text)
        {
            return taken;
        }
        let lists = self.column_refs();
        let at = self
            .selected
            .as_ref()
            .and_then(|r| lists[self.sel_col].iter().position(|x| x == r));
        let taken = match key {
            Key::Char('v') => {
                self.set_layout(self.layout.next(), text);
                return true;
            }
            Key::Char('h') | Key::ArrowLeft => {
                self.move_column(-1, at, &lists);
                true
            }
            Key::Char('l') | Key::ArrowRight => {
                self.move_column(1, at, &lists);
                true
            }
            Key::Char('j') | Key::ArrowDown => {
                let list = &lists[self.sel_col];
                if let Some(next) = at
                    .map_or(Some(0), |i| Some(i + 1))
                    .filter(|i| *i < list.len())
                {
                    self.selected = Some(list[next].clone());
                }
                true
            }
            Key::Char('k') | Key::ArrowUp => {
                let list = &lists[self.sel_col];
                if let Some(i) = at.and_then(|i| i.checked_sub(1)) {
                    self.selected = Some(list[i].clone());
                }
                true
            }
            Key::Enter => match self.selected.clone() {
                Some(r) => {
                    self.open_detail(r, text);
                    true
                }
                None => false,
            },
            Key::Char('n') => {
                self.open_form(Form::new_task(), text);
                true
            }
            Key::Char('x') => {
                if let Some(task) = self.selected_task().cloned() {
                    self.run_form(&task, text);
                }
                true
            }
            Key::Char('d') => {
                if let Some(Task {
                    r#ref: TaskRef::Local(id),
                    state,
                    ..
                }) = self.selected_task()
                {
                    let state = match state {
                        State::Open => State::Closed,
                        State::Closed => State::Open,
                    };
                    let id = id.clone();
                    self.act(Intent::SetState { id, state });
                }
                true
            }
            Key::Char('r') => {
                self.act(Intent::Refresh);
                true
            }
            Key::Char('a') => {
                if let Some(task) = self.selected.clone() {
                    self.open_form(Form::add_link(task), text);
                }
                true
            }
            Key::Char('/') => {
                self.tree.set_focus(Some(self.text));
                true
            }
            Key::Char('o') => {
                if let Some(Task {
                    r#ref: TaskRef::GitHub { .. },
                    url: Some(url),
                    ..
                }) = self.selected_task()
                {
                    let url = url.clone();
                    self.act(Intent::OpenUrl(url));
                }
                true
            }
            _ => false,
        };
        if taken {
            self.rebuild(text);
            // Moving the selection brings its card into view: focusing it
            // makes its column scroll to it (a card's focus leaves keys
            // shortcuts).
            let moved = matches!(
                key,
                Key::Char('h' | 'j' | 'k' | 'l')
                    | Key::ArrowLeft
                    | Key::ArrowRight
                    | Key::ArrowUp
                    | Key::ArrowDown
            );
            if moved
                && let Some(card) = self
                    .selected
                    .as_ref()
                    .and_then(|r| self.cards.iter().find(|(_, x)| x == r).map(|(id, _)| *id))
            {
                self.tree.set_focus(Some(card));
            }
        }
        taken
    }

    /// The keys the list, timeline and calendar take their own way:
    /// `j`/`k` along what is shown, top to bottom; `h`/`l` nothing; `[`/`]`
    /// the calendar's month. `None`: as on the board.
    fn press_in_layout(&mut self, key: &Key, text: &mut TextStore) -> Option<bool> {
        let order: Vec<TaskRef> = {
            let mut seen = BTreeSet::new();
            self.cards
                .iter()
                .filter(|(_, r)| seen.insert(r.clone()))
                .map(|(_, r)| r.clone())
                .collect()
        };
        let at = self
            .selected
            .as_ref()
            .and_then(|r| order.iter().position(|x| x == r));
        let step = match key {
            Key::Char('j') | Key::ArrowDown => 1,
            Key::Char('k') | Key::ArrowUp => -1,
            Key::Char('h' | 'l') | Key::ArrowLeft | Key::ArrowRight => return Some(true),
            Key::Char('[') | Key::Char(']') if self.layout == Layout::Calendar => {
                self.month += if *key == Key::Char('[') { -1 } else { 1 };
                self.rebuild(text);
                return Some(true);
            }
            _ => return None,
        };
        let next = match at {
            None if step > 0 => Some(0),
            None => None,
            Some(i) => i.checked_add_signed(step),
        }
        .filter(|i| *i < order.len());
        if let Some(next) = next {
            self.selected = Some(order[next].clone());
        }
        self.rebuild(text);
        if let Some(row) = self
            .selected
            .as_ref()
            .and_then(|r| self.cards.iter().find(|(_, x)| x == r).map(|(id, _)| *id))
        {
            self.tree.set_focus(Some(row));
        }
        Some(true)
    }

    fn move_column(&mut self, by: isize, at: Option<usize>, lists: &[Vec<TaskRef>; 4]) {
        let col = self
            .sel_col
            .saturating_add_signed(by)
            .min(Column::ALL.len() - 1);
        self.sel_col = col;
        let list = &lists[col];
        self.selected = if list.is_empty() {
            None
        } else {
            Some(list[at.unwrap_or(0).min(list.len() - 1)].clone())
        };
    }

    /// Escape: closes the form, else leaves the filter, else closes the
    /// detail.
    fn escape(&mut self, text: &mut TextStore) -> bool {
        if self.form.is_some() {
            self.close_form(text);
        } else if self.tree.focused() == Some(self.text) {
            self.tree.set_focus(None);
        } else if self.detail.take().is_some() {
            self.rebuild_detail(text);
        } else {
            return false;
        }
        true
    }

    /// Whether keys go to a text input: one has focus, or a form is open.
    fn typing(&self) -> bool {
        self.form.is_some()
            || self.tree.focused().is_some_and(|id| {
                self.tree.get::<TextField>(id).is_some() || self.tree.get::<TextArea>(id).is_some()
            })
    }

    /// Whether the focus is in an overlay (an open menu takes it): its keys,
    /// Escape included, are the overlay's.
    fn in_overlay(&self) -> bool {
        self.tree.focused().is_some_and(|focus| {
            let overlays: Vec<WidgetId> = self.tree.overlays().collect();
            self.tree.ancestors(focus).any(|id| overlays.contains(&id))
        })
    }
}

/// The local tasks, then the issues (a ref listed twice once).
fn all_tasks(view: &View) -> Vec<&Task> {
    let mut seen = BTreeSet::new();
    view.tasks
        .iter()
        .chain(&view.github.tasks)
        .filter(|t| seen.insert(&t.r#ref))
        .collect()
}

/// The last run started on `r`.
fn last_run<'a>(view: &'a View, r: &TaskRef) -> Option<&'a RunView> {
    view.runs
        .iter()
        .filter(|v| &v.run.task == r)
        .max_by_key(|v| v.run.started)
}

/// Every task `filter` lets through, old 完了 ones too, newest activity
/// first: the list's, timeline's and calendar's tasks.
fn flat<'a>(view: &'a View, filter: &Filter) -> Vec<Card<'a>> {
    let mut cards: Vec<Card<'a>> = all_tasks(view)
        .into_iter()
        .filter(|t| matches(t, filter))
        .map(|task| Card {
            task,
            last: last_run(view, &task.r#ref),
        })
        .collect();
    let activity = |c: &Card<'_>| c.task.updated.max(c.last.map_or(0, |v| v.run.started));
    cards.sort_by(|a, b| {
        activity(b)
            .cmp(&activity(a))
            .then_with(|| a.task.r#ref.cmp(&b.task.r#ref))
    });
    cards
}

/// The cards of each column, `filter` applied, newest activity first;
/// 完了 only for the last [`DONE_DAYS`].
fn lists<'a>(view: &'a View, filter: &Filter) -> [Vec<Card<'a>>; 4] {
    let now = view.now;
    let mut lists: [Vec<Card<'a>>; 4] = Default::default();
    for task in all_tasks(view) {
        if !matches(task, filter) {
            continue;
        }
        let last = last_run(view, &task.r#ref);
        let col = column(task, last);
        let since = task.closed.unwrap_or(task.updated);
        if col == Column::Done && now.saturating_sub(since) > DONE_DAYS * DAY_MS {
            continue;
        }
        lists[col as usize].push(Card { task, last });
    }
    let activity = |c: &Card<'_>| c.task.updated.max(c.last.map_or(0, |v| v.run.started));
    for list in &mut lists {
        list.sort_by(|a, b| {
            activity(b)
                .cmp(&activity(a))
                .then_with(|| a.task.r#ref.cmp(&b.task.r#ref))
        });
    }
    lists
}

/// Whether `id` (`t-…`, `#N`, a marker's `path`, or `code:<key>:<path>`:
/// the key holds no `:`, so the first one after `code:` ends it) names `r`.
fn names(r: &TaskRef, id: &str) -> bool {
    match r {
        TaskRef::Local(local) => local == id,
        TaskRef::Code { path, key } => match id
            .strip_prefix("code:")
            .and_then(|rest| rest.split_once(':'))
        {
            Some((k, p)) => k == key && p == path,
            None => path == id,
        },
        TaskRef::GitHub { number, .. } => id
            .strip_prefix('#')
            .is_some_and(|n| n.parse() == Ok(*number)),
    }
}

/// The GitHub header's status (`owner/name · n 分前の取得`, `取得中`) and
/// the last refresh's failure (empty when none).
fn github_header(gh: &GitHubView, now: Millis) -> (String, String) {
    let mut parts: Vec<String> = gh.repo.iter().cloned().collect();
    if gh.refreshing {
        parts.push("取得中".to_owned());
    } else if let Some(fetched) = gh.fetched {
        parts.push(format!(
            "{} 分前の取得",
            now.saturating_sub(fetched) / 60_000
        ));
    }
    let status = if parts.is_empty() {
        "未取得".to_owned()
    } else {
        parts.join(" · ")
    };
    let failure = gh.failure.as_ref().map(|f| f.reason()).unwrap_or_default();
    (status, failure)
}

fn clear(tree: &mut UiTree, parent: WidgetId, text: &mut TextStore) {
    for child in tree.children(parent).to_vec() {
        tree.remove(child, text);
    }
}

impl Screen for Tasks {
    fn tree(&self) -> &UiTree {
        &self.tree
    }

    fn tree_mut(&mut self) -> &mut UiTree {
        &mut self.tree
    }

    fn woken(&mut self, text: &mut TextStore) {
        if let Some(view) = self.feed.poll() {
            // A new notice from the feed is newer than the screen's own.
            if view.notice.is_some() && view.notice != self.view.notice {
                self.own_notice = None;
            }
            self.view = view;
            self.rebuild(text);
            if let Some(id) = self.select_later.take()
                && !self.select(&id, text)
            {
                self.select_later = Some(id);
            }
        }
    }

    fn dispatch(&mut self, event: Event, text: &mut TextStore) {
        let swallow = self.swallow.take();
        // The screen's own notice lasts until the next key or press.
        if matches!(event, Event::KeyDown { .. } | Event::PointerDown { .. }) {
            self.select_later = None;
        }
        if matches!(event, Event::KeyDown { .. } | Event::PointerDown { .. })
            && self.own_notice.take().is_some()
        {
            self.show_notice();
        }
        match &event {
            // The text a shortcut's key types is not typed anywhere.
            Event::Text(typed)
                if swallow.is_some_and(|c| typed.to_lowercase() == c.to_string()) =>
            {
                return;
            }
            Event::KeyDown { .. } if self.in_overlay() => {}
            Event::KeyDown {
                key: Key::Escape, ..
            } if self.escape(text) => return,
            Event::KeyDown { key, modifiers }
                if !shortcut_modifiers(key, modifiers) || self.typing() => {}
            Event::KeyDown { key, .. } => {
                // Enter and the arrows are the focused control's (a source
                // segment, the label menu), unless a card or nothing has
                // focus.
                let free = self
                    .tree
                    .focused()
                    .is_none_or(|id| self.cards.iter().any(|(card, _)| *card == id));
                let letter = matches!(key, Key::Char(_));
                if (letter || free) && self.press(key, text) {
                    if let Key::Char(c) = key {
                        self.swallow = Some(*c);
                    }
                    return;
                }
            }
            _ => {}
        }
        self.tree.dispatch(event, text);
        for message in self.tree.take_messages() {
            self.handle(message, text);
        }
    }

    fn handle(&mut self, message: Message, text: &mut TextStore) {
        if let Some(form) = &mut self.form {
            match form.handle(&message, &self.tree) {
                FormOutcome::Pending => {}
                FormOutcome::Cancel => {
                    self.close_form(text);
                    return;
                }
                FormOutcome::Submit(intent) => {
                    self.close_form(text);
                    self.act(intent);
                    return;
                }
            }
        }
        let source = message.source;
        if let Some(SelectionChanged(i)) = message.downcast_ref::<SelectionChanged>() {
            if source == self.layout_control {
                if let Some(layout) = Layout::ALL.get(*i) {
                    self.layout = *layout;
                    self.rebuild(text);
                }
            } else if source == self.source {
                self.filter.source = SOURCES.get(*i).and_then(|(_, s)| *s);
                self.rebuild(text);
            } else if source == self.label_popup {
                self.filter.label = i
                    .checked_sub(1)
                    .and_then(|i| self.label_choices.get(i).cloned());
                self.rebuild(text);
            }
        } else if let Some(TextChanged(typed)) = message.downcast_ref::<TextChanged>() {
            if source == self.text {
                self.filter.text = typed.clone();
                self.rebuild(text);
            }
        } else if message.downcast_ref::<TextSubmitted>().is_some() {
            if source == self.text {
                self.tree.set_focus(None);
            }
        } else if message.downcast_ref::<ButtonClicked>().is_some() {
            if source == self.new_button {
                self.open_form(Form::new_task(), text);
            } else if source == self.refresh_button {
                self.act(Intent::Refresh);
            } else if let Some(at) = self
                .month_buttons
                .and_then(|ids| ids.iter().position(|id| *id == source))
            {
                self.month = match at {
                    0 => self.month - 1,
                    1 => 0,
                    _ => self.month + 1,
                };
                self.rebuild(text);
            } else if let Some((_, r)) = self.cards.iter().find(|(id, _)| *id == source).cloned() {
                self.select_ref(&r);
                self.open_detail(r, text);
            } else if let Some((_, intent)) = self
                .detail_actions
                .iter()
                .find(|(id, _)| *id == source)
                .cloned()
            {
                self.detail_action(intent, text);
            }
        }
    }
}

impl Tasks {
    /// A detail button: 編集, 実行 and 関連を追加 open their form, the rest
    /// are sent.
    fn detail_action(&mut self, intent: Intent, text: &mut TextStore) {
        match intent {
            Intent::AddLink { task, .. } => self.open_form(Form::add_link(task), text),
            Intent::Edit { id, .. } => {
                if let Some(task) = self.task(&TaskRef::Local(id.clone())).cloned() {
                    self.open_form(Form::edit(&id, &task), text);
                }
            }
            Intent::Run { task, .. } => {
                if let Some(task) = self.task(&task).cloned() {
                    self.run_form(&task, text);
                }
            }
            other => self.act(other),
        }
    }
}

/// Shortcuts are plain keys; Shift is allowed on a letter, as a capital
/// types it, but not on a symbol: Shift + `/` types `?`, not `/`.
fn shortcut_modifiers(key: &Key, m: &Modifiers) -> bool {
    let shifted_symbol = m.shift && matches!(key, Key::Char(c) if !c.is_alphabetic());
    !(m.ctrl || m.alt || m.meta || m.command || shifted_symbol)
}

#[cfg(test)]
mod tests;

/// Where the scroll view `id` is scrolled to; the top when it is gone.
fn scroll_position(tree: &UiTree, id: WidgetId) -> noa_gpui_scene::Point {
    tree.get::<ScrollView>(id)
        .map(ScrollView::scroll_position)
        .unwrap_or_default()
}

fn set_scroll_position(tree: &mut UiTree, id: WidgetId, at: noa_gpui_scene::Point) {
    tree.update::<ScrollView, _>(id, |s, cx| s.set_scroll_position(cx, at));
}

#[cfg(test)]
mod links_tests {
    //! The task-links parts of the screen (`docs/specs/task-links.md`):
    //! `--select=code:<key>:<path>` and the `a` key.

    use std::cell::RefCell;
    use std::rc::Rc;

    use noa_gpui_scene::Size;
    use tasks::task_links::{self, TaskKey};

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

    fn key(screen: &mut Tasks, text: &mut TextStore, key: Key) {
        screen.dispatch(
            Event::KeyDown {
                key,
                modifiers: Modifiers::default(),
            },
            text,
        );
        if let Key::Char(c) = key {
            screen.dispatch(Event::Text(c.to_string()), text);
        }
        screen.prepare(window(), text);
    }

    /// `--select=code:<key>:<path>` (what Review and Verify pass for a
    /// marker task) selects that marker, also when a marker in another file
    /// has the same key.
    #[test]
    fn links_ac5_select_code_key_path_names_one_marker_among_files_sharing_its_key() {
        let mut view = demo::view();
        view.tasks.extend(tasks::code::parse(
            b"src/a.rs\x007\x00    // TODO: same words\nsrc/b:c.rs\x007\x00    // TODO: same words\n",
            |_| demo::NOW,
        ));
        let markers: Vec<TaskRef> = view
            .tasks
            .iter()
            .map(|t| t.r#ref.clone())
            .filter(|r| matches!(r, TaskRef::Code { path, .. } if path.starts_with("src/")))
            .collect();
        let [a, b] = &markers[..] else {
            panic!("{markers:?}");
        };
        let (TaskRef::Code { key: ka, .. }, TaskRef::Code { key: kb, .. }) = (a, b) else {
            unreachable!()
        };
        assert_eq!(ka, kb, "the same marker text in two files shares its key");
        let (mut screen, _, mut text) = screen_on(view);
        for marker in [a, b] {
            let arg =
                task_links::select_arg(&TaskKey(serde_json::to_value(marker).unwrap())).unwrap();
            assert!(arg.starts_with("code:"), "{arg}");
            assert!(screen.select(&arg, &mut text), "{arg}");
            assert_eq!(screen.detail.as_ref(), Some(marker), "{arg}");
        }
        // The path still names a marker; a key with another path does not.
        assert!(screen.select("src/a.rs", &mut text));
        assert_eq!(screen.detail.as_ref(), Some(a));
        assert!(!screen.select(&format!("code:{ka}:src/c.rs"), &mut text));
        assert!(!screen.select("code:nope:src/a.rs", &mut text));
    }

    /// `a` opens 関連を追加 for the selected task; an empty field does not
    /// submit; Return submits `AddLink` with what was typed.
    #[test]
    fn links_ac6_the_a_key_opens_add_link_and_submits_what_was_typed() {
        let (mut screen, log, mut text) = screen_on(demo::view());
        let t1 = TaskRef::Local("t-1".into());
        screen.select_ref(&t1);
        key(&mut screen, &mut text, Key::Char('a'));
        assert!(screen.form.is_some(), "the form is open");
        key(&mut screen, &mut text, Key::Enter);
        assert!(screen.form.is_some(), "empty: not submitted");
        assert!(log.borrow().is_empty());
        screen.dispatch(Event::Text("#45".into()), &mut text);
        key(&mut screen, &mut text, Key::Enter);
        assert!(screen.form.is_none(), "closed on submit");
        assert_eq!(
            *log.borrow(),
            [Intent::AddLink {
                task: t1,
                input: "#45".into(),
            }]
        );
    }
}
