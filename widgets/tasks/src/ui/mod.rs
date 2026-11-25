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
//! 再開 (local tasks), `r` 再取得, `/` filter, `o` GitHub で開く, Escape
//! closes the form, leaves the filter or closes the detail. While a menu
//! (or another overlay) has focus, every key is the menu's.

pub mod columns;
pub mod dates;
pub mod detail;
pub mod forms;
pub mod related;
pub mod style;
pub mod views;

use std::collections::BTreeSet;
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

use tasks::links::{LinkId, Nav};
use tasks::model::column::{Filter, column, matches};
use tasks::model::types::{
    AgentKind, Column, DONE_DAYS, GitHubView, Isolation, Millis, RunId, RunView, Source, State,
    Task, TaskRef,
};

use columns::Card;
use forms::{Form, FormOutcome};
pub use related::{LinkKind, RelatedView, check_link_id};
use related::{RelatedAction, Unlinking};
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
    /// リンクを追加: `kind` `id` (checked by the feed: found, not linked).
    Link {
        task: TaskRef,
        kind: LinkKind,
        id: String,
    },
    /// リンクを外す, answered: these links of `task`'s 関連成果.
    Unlink {
        task: TaskRef,
        links: Vec<LinkId>,
    },
    /// A 関連成果 row's 開く that another widget shows.
    Navigate(Nav),
}

/// Where the screen's data comes from.
pub trait Feed {
    /// A new View when anything changed; never blocks.
    fn poll(&mut self) -> Option<View>;
    /// Outcomes come back as View.notice.
    fn act(&mut self, intent: Intent);
    /// The task whose detail is open (`None`: none): its 関連成果 is read
    /// now, every 10 s while it stays open, and on 再取得.
    fn watch(&mut self, _task: Option<&TaskRef>) {}
    /// A new 関連成果 when one was read since the last call; never blocks.
    fn related(&mut self) -> Option<RelatedView> {
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
    /// The 関連成果 last read, the task the feed was told is open, the
    /// section's buttons, and a リンクを外す waiting for its answer.
    related: Option<RelatedView>,
    watching: Option<TaskRef>,
    related_actions: Vec<(WidgetId, RelatedAction)>,
    unlinking: Option<Unlinking>,
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
            related: None,
            watching: None,
            related_actions: Vec::new(),
            unlinking: None,
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
        self.related_actions.clear();
        let shown =
            self.form.is_none() && self.detail.as_ref().is_some_and(|r| self.task(r).is_some());
        self.watch(shown.then(|| self.detail.clone()).flatten());
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
                self.detail_actions = detail::insert(
                    &mut self.tree,
                    self.detail_host,
                    &task,
                    &runs,
                    loaded,
                    self.view.now,
                );
                if let Some(&scroll) = self.tree.children(self.detail_host).first() {
                    let view = self.related.as_ref().filter(|v| v.task == r);
                    self.related_actions =
                        related::insert(&mut self.tree, scroll, view, self.unlinking.as_ref());
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

    /// Tells the feed whose detail is open when that changed, and takes a
    /// 関連成果 it already has (the demo's is at hand at once).
    fn watch(&mut self, task: Option<TaskRef>) {
        if task == self.watching {
            return;
        }
        self.unlinking = None;
        self.feed.watch(task.as_ref());
        self.watching = task;
        if let Some(view) = self.feed.related() {
            self.related = Some(view);
        }
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
        } else if self.unlinking.take().is_some() {
            self.rebuild_detail(text);
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

/// Whether `id` (`t-…`, `#N`, a marker's `path`) names `r`.
fn names(r: &TaskRef, id: &str) -> bool {
    match r {
        TaskRef::Local(local) => local == id,
        TaskRef::Code { path, .. } => path == id,
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
        let view = self.feed.poll();
        if view.is_none()
            && let Some(related) = self.feed.related()
        {
            self.related = Some(related);
            self.rebuild_detail(text);
        }
        if let Some(view) = view {
            // A new notice from the feed is newer than the screen's own.
            if view.notice.is_some() && view.notice != self.view.notice {
                self.own_notice = None;
            }
            self.view = view;
            if let Some(related) = self.feed.related() {
                self.related = Some(related);
            }
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
            match form.handle(&message, &mut self.tree) {
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
            } else if let Some((_, action)) = self
                .related_actions
                .iter()
                .find(|(id, _)| *id == source)
                .cloned()
            {
                self.related_action(action, text);
            }
        }
    }
}

impl Tasks {
    /// A detail button: 編集 and 実行 open their form, the rest are sent.
    fn detail_action(&mut self, intent: Intent, text: &mut TextStore) {
        match intent {
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

    /// A 関連成果 button.
    fn related_action(&mut self, action: RelatedAction, text: &mut TextStore) {
        let Some(task) = self.detail.clone() else {
            return;
        };
        match action {
            RelatedAction::Nav(nav) => self.navigate(nav, text),
            RelatedAction::AskUnlink { what, links } => {
                self.unlinking = Some(Unlinking { what, links });
                self.rebuild_detail(text);
            }
            RelatedAction::ConfirmUnlink => {
                if let Some(ask) = self.unlinking.take() {
                    self.act(Intent::Unlink {
                        task,
                        links: ask.links,
                    });
                }
                self.rebuild_detail(text);
            }
            RelatedAction::CancelUnlink => {
                self.unlinking = None;
                self.rebuild_detail(text);
            }
            RelatedAction::AddLink => {
                self.unlinking = None;
                let title = self
                    .task(&task)
                    .map(|t| t.title.clone())
                    .unwrap_or_default();
                self.open_form(Form::link(&task, &title), text);
            }
        }
    }

    /// A row's 開く: a run's pane (else its work tree) here, an id to the
    /// clipboard, a review, a verification or a task in its own window.
    fn navigate(&mut self, nav: Nav, text: &mut TextStore) {
        match nav {
            Nav::Copy(id) => {
                self.tree.clipboard().write_text(&id);
                self.own_notice = Some(format!("{id} をコピーしました"));
                self.show_notice();
            }
            Nav::Run(run) => {
                let id = RunId::new(run.clone());
                let logged = self.view.runs.iter().find(|v| v.run.run == id);
                let intent = match logged {
                    Some(v)
                        if matches!(v.liveness, tasks::model::types::Liveness::Alive { .. }) =>
                    {
                        Some(Intent::OpenPane(id))
                    }
                    Some(_) => Some(Intent::OpenWorktree(id)),
                    // A run another window's log holds: its work tree, as
                    // the store last saw it.
                    None => self
                        .related
                        .as_ref()
                        .and_then(|v| v.related.as_ref().ok())
                        .and_then(|r| r.runs.iter().find(|e| e.run == run))
                        .and_then(|e| e.info.workdir.as_ref())
                        .map(|dir| Intent::OpenUrl(dir.display().to_string())),
                };
                match intent {
                    Some(intent) => self.act(intent),
                    None => {
                        self.own_notice = Some(format!("{run} の作業場所がわかりません"));
                        self.rebuild(text);
                    }
                }
            }
            other => self.act(Intent::Navigate(other)),
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
