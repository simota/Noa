//! U1's UI tests: the columns and cards on the demo View (AC-4), the keys
//! on a recording feed (AC-5), the GitHub header (AC-6). They assert
//! labels, ids and tree order, never a position or a text width, so they
//! run on Linux too.

use std::cell::RefCell;
use std::rc::Rc;

use noa_gpui_core::widgets::{ScrollView, Tag};
use noa_gpui_scene::Size;

use tasks::model::types::GhFailure;

use super::*;
use crate::demo::{self, DemoFeed};

/// Hands out `view` once and records every intent.
struct TestFeed {
    view: Option<View>,
    sent: Rc<RefCell<Vec<Intent>>>,
}

impl Feed for TestFeed {
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
    let feed = TestFeed {
        view: Some(view),
        sent: sent.clone(),
    };
    let mut screen = Tasks::new(Box::new(feed));
    let mut text = TextStore::new(Default::default());
    screen.prepare(window(), &mut text);
    (screen, sent, text)
}

fn demo_screen() -> Tasks {
    let mut screen = Tasks::new(Box::<DemoFeed>::default());
    screen.prepare(window(), &mut TextStore::new(Default::default()));
    screen
}

fn labels_under(tree: &UiTree, id: WidgetId) -> Vec<String> {
    let own = tree
        .get::<Label>(id)
        .map(|l| l.text().to_owned())
        .or_else(|| tree.get::<Tag>(id).map(|t| t.text().to_owned()));
    own.into_iter()
        .chain(
            tree.children(id)
                .iter()
                .flat_map(|c| labels_under(tree, *c)),
        )
        .collect()
}

/// The tasks of the cards under `id`, in tree order.
fn cards_under(screen: &Tasks, id: WidgetId) -> Vec<TaskRef> {
    let own = screen
        .cards
        .iter()
        .find(|(card, _)| *card == id)
        .map(|(_, r)| r.clone());
    own.into_iter()
        .chain(
            screen
                .tree
                .children(id)
                .iter()
                .flat_map(|c| cards_under(screen, *c)),
        )
        .collect()
}

/// Each column's cards, in [`Column::ALL`] order.
fn shown(screen: &Tasks) -> Vec<Vec<TaskRef>> {
    screen
        .columns
        .iter()
        .map(|c| cards_under(screen, *c))
        .collect()
}

fn card_of(screen: &Tasks, r: &TaskRef) -> WidgetId {
    screen
        .cards
        .iter()
        .find(|(_, x)| x == r)
        .map(|(id, _)| *id)
        .unwrap_or_else(|| panic!("no card for {r:?}"))
}

fn local(id: &str) -> TaskRef {
    TaskRef::Local(id.into())
}

fn gh(number: u64) -> TaskRef {
    TaskRef::GitHub {
        repo: demo::REPO.into(),
        number,
    }
}

/// A key as the platform delivers it: the key, then the text it types.
fn key(screen: &mut Tasks, text: &mut TextStore, c: char) {
    screen.dispatch(
        Event::KeyDown {
            key: Key::Char(c),
            modifiers: Modifiers::default(),
        },
        text,
    );
    screen.dispatch(Event::Text(c.to_string()), text);
    screen.prepare(window(), text);
}

fn special(screen: &mut Tasks, text: &mut TextStore, key: Key) {
    screen.dispatch(
        Event::KeyDown {
            key,
            modifiers: Modifiers::default(),
        },
        text,
    );
    screen.prepare(window(), text);
}

fn sent(log: &Rc<RefCell<Vec<Intent>>>) -> Vec<Intent> {
    std::mem::take(&mut *log.borrow_mut())
}

// ---------------------------------------------------------------- AC-4

#[test]
fn ac04_four_columns_in_order() {
    let screen = demo_screen();
    let heads: Vec<String> = screen
        .columns
        .iter()
        .map(|c| labels_under(&screen.tree, *c)[0].clone())
        .collect();
    assert_eq!(heads, ["未着手", "実行中", "確認待ち", "完了"]);
}

#[test]
fn ac04_each_card_is_in_the_column_column_gives() {
    let screen = demo_screen();
    let view = demo::view();
    let shown = shown(&screen);
    for (at, refs) in shown.iter().enumerate() {
        for r in refs {
            let task = all_tasks(&view)
                .into_iter()
                .find(|t| &t.r#ref == r)
                .unwrap();
            assert_eq!(column(task, last_run(&view, r)), Column::ALL[at], "{r:?}");
        }
    }
    // The demo has a card in every column.
    let has = |col: Column, r: &TaskRef| shown[col as usize].contains(r);
    assert!(has(Column::Todo, &local("t-1")) && has(Column::Todo, &gh(12)));
    assert!(has(Column::Running, &local("t-2")) && has(Column::Running, &gh(15)));
    assert!(has(Column::Review, &local("t-3")) && has(Column::Review, &gh(18)));
    assert!(has(Column::Done, &local("t-4")) && has(Column::Done, &gh(9)));
    // Each card once.
    let all: Vec<&TaskRef> = shown.iter().flatten().collect();
    let unique: BTreeSet<&TaskRef> = all.iter().copied().collect();
    assert_eq!(all.len(), unique.len());
    // The count beside each column's name.
    for (at, refs) in shown.iter().enumerate() {
        assert_eq!(
            labels_under(&screen.tree, screen.columns[at])[1],
            refs.len().to_string()
        );
    }
}

#[test]
fn ac04_cards_carry_their_source_badge() {
    let screen = demo_screen();
    for (id, r) in &screen.cards {
        let labels = labels_under(&screen.tree, *id);
        let badge = match r {
            TaskRef::Local(_) => "ローカル".to_owned(),
            TaskRef::GitHub { number, .. } => format!("#{number}"),
            TaskRef::Code { path, .. } => path.rsplit('/').next().unwrap().to_owned(),
        };
        assert_eq!(labels[0], badge, "{r:?}: {labels:?}");
    }
    let labels = labels_under(&screen.tree, card_of(&screen, &gh(12)));
    assert!(labels.contains(&"enhancement".to_owned()), "{labels:?}");
    // The last run's agent and age.
    let labels = labels_under(&screen.tree, card_of(&screen, &local("t-2")));
    assert!(
        labels.contains(&"claude · 2 時間前".to_owned()),
        "{labels:?}"
    );
}

#[test]
fn ac04_done_hides_tasks_closed_more_than_seven_days_ago() {
    let screen = demo_screen();
    let shown: Vec<TaskRef> = shown(&screen).concat();
    // t-6 closed 10 days ago, #3 30 days ago.
    assert!(!shown.contains(&local("t-6")), "{shown:?}");
    assert!(!shown.contains(&gh(3)), "{shown:?}");
    // Exactly seven days is still shown; a day more is not.
    let mut view = demo::view();
    view.tasks[5].updated = view.now - DONE_DAYS * DAY_MS;
    let (screen, _, _) = screen_on(view.clone());
    assert!(self::shown(&screen)[Column::Done as usize].contains(&local("t-6")));
    view.tasks[5].updated = view.now - DONE_DAYS * DAY_MS - 1;
    let (screen, _, _) = screen_on(view);
    assert!(!self::shown(&screen)[Column::Done as usize].contains(&local("t-6")));
}

/// 完了's 7 days count from when the task was closed: an issue closed long
/// ago and commented on today (a new `updatedAt`) stays hidden.
#[test]
fn ac04_done_counts_from_when_it_was_closed_not_last_updated() {
    let mut view = demo::view();
    view.tasks[5].updated = view.now;
    view.tasks[5].closed = Some(view.now - 10 * DAY_MS);
    let (screen, _, _) = screen_on(view.clone());
    assert!(!self::shown(&screen)[Column::Done as usize].contains(&local("t-6")));
    view.tasks[5].closed = Some(view.now - DONE_DAYS * DAY_MS);
    let (screen, _, _) = screen_on(view);
    assert!(self::shown(&screen)[Column::Done as usize].contains(&local("t-6")));
}

#[test]
fn ac04_a_broken_local_file_shows_its_problem() {
    let screen = demo_screen();
    let broken = local("t-5");
    let problem = demo::view()
        .tasks
        .into_iter()
        .find(|t| t.r#ref == broken)
        .and_then(|t| t.problem)
        .unwrap();
    assert!(self::shown(&screen)[Column::Todo as usize].contains(&broken));
    let labels = labels_under(&screen.tree, card_of(&screen, &broken));
    assert_eq!(labels[0], "ローカル");
    assert!(labels.contains(&problem), "{labels:?}");
}

// ---------------------------------------------------------------- AC-5

#[test]
fn ac05_hjkl_move_the_selection() {
    let (mut screen, log, mut text) = screen_on(demo::view());
    let cols = shown(&screen);
    assert!(cols[0].len() >= 2 && !cols[1].is_empty());
    assert_eq!(screen.selected.as_ref(), Some(&cols[0][0]));
    key(&mut screen, &mut text, 'k');
    assert_eq!(
        screen.selected.as_ref(),
        Some(&cols[0][0]),
        "stays at the top"
    );
    key(&mut screen, &mut text, 'j');
    assert_eq!(screen.selected.as_ref(), Some(&cols[0][1]));
    key(&mut screen, &mut text, 'k');
    assert_eq!(screen.selected.as_ref(), Some(&cols[0][0]));
    key(&mut screen, &mut text, 'h');
    assert_eq!(screen.sel_col, 0, "stays in the first column");
    key(&mut screen, &mut text, 'l');
    assert_eq!(screen.sel_col, 1);
    assert_eq!(screen.selected.as_ref(), Some(&cols[1][0]));
    for _ in 0..5 {
        key(&mut screen, &mut text, 'l');
    }
    assert_eq!(screen.sel_col, 3);
    assert_eq!(screen.selected.as_ref(), Some(&cols[3][0]));
    key(&mut screen, &mut text, 'h');
    assert_eq!(screen.sel_col, 2);
    assert_eq!(screen.selected.as_ref(), Some(&cols[2][0]));
    // The selected card is drawn as such.
    let card = card_of(&screen, &cols[2][0]);
    assert!(
        screen
            .tree
            .get::<noa_gpui_core::widgets::Pressable>(card)
            .unwrap()
            .is_selected()
    );
    // Moving sends nothing.
    assert!(sent(&log).is_empty());
}

#[test]
fn ac05_slash_focuses_the_filter_and_it_filters_the_columns() {
    let (mut screen, log, mut text) = screen_on(demo::view());
    key(&mut screen, &mut text, '/');
    assert_eq!(screen.tree.focused(), Some(screen.text));
    // The `/` itself is not typed into it.
    assert_eq!(
        screen.tree.get::<TextField>(screen.text).unwrap().text(),
        ""
    );
    screen.dispatch(Event::Text("リサイズ".into()), &mut text);
    screen.prepare(window(), &mut text);
    assert_eq!(shown(&screen).concat(), [local("t-2")]);
    // While it has focus, keys type: `j` does not move the selection.
    key(&mut screen, &mut text, 'j');
    assert_eq!(screen.filter.text, "リサイズj");
    assert!(shown(&screen).concat().is_empty());
    // Escape leaves it; the filter stays.
    special(&mut screen, &mut text, Key::Escape);
    assert_eq!(screen.tree.focused(), None);
    assert_eq!(screen.filter.text, "リサイズj");
    assert!(sent(&log).is_empty());

    // An issue's #N.
    let (mut screen, _, mut text) = screen_on(demo::view());
    key(&mut screen, &mut text, '/');
    screen.dispatch(Event::Text("#12".into()), &mut text);
    screen.prepare(window(), &mut text);
    assert_eq!(shown(&screen).concat(), [gh(12)]);
}

#[test]
fn ac05_the_source_and_label_filters_narrow_the_columns() {
    let (mut screen, log, mut text) = screen_on(demo::view());
    // ローカル: the source segments from the keyboard.
    screen.tree.set_focus(Some(screen.source));
    special(&mut screen, &mut text, Key::ArrowRight);
    special(&mut screen, &mut text, Key::Space);
    assert_eq!(screen.filter.source, Some(Source::Local));
    let refs = shown(&screen).concat();
    assert!(!refs.is_empty());
    assert!(refs.iter().all(|r| r.source() == Source::Local), "{refs:?}");
    // GitHub, labelled docs.
    special(&mut screen, &mut text, Key::ArrowRight);
    special(&mut screen, &mut text, Key::Space);
    assert_eq!(screen.filter.source, Some(Source::GitHub));
    let docs = screen
        .label_choices
        .iter()
        .position(|l| l == "docs")
        .unwrap();
    screen.tree.set_focus(Some(screen.label_popup));
    for _ in 0..=docs {
        special(&mut screen, &mut text, Key::ArrowDown);
    }
    assert_eq!(screen.filter.label.as_deref(), Some("docs"));
    assert_eq!(shown(&screen).concat(), [gh(18)]);
    assert!(sent(&log).is_empty());
}

#[test]
fn ac05_r_sends_refresh() {
    let (mut screen, log, mut text) = screen_on(demo::view());
    key(&mut screen, &mut text, 'r');
    assert_eq!(sent(&log), [Intent::Refresh]);
}

#[test]
fn ac05_o_opens_an_issue_and_nothing_for_a_local_task() {
    let (mut screen, log, mut text) = screen_on(demo::view());
    screen.select_ref(&gh(12));
    key(&mut screen, &mut text, 'o');
    assert_eq!(
        sent(&log),
        [Intent::OpenUrl(format!(
            "https://github.com/{}/issues/12",
            demo::REPO
        ))]
    );
    screen.select_ref(&local("t-1"));
    key(&mut screen, &mut text, 'o');
    assert!(sent(&log).is_empty());
}

#[test]
fn ac05_d_closes_a_local_task_and_nothing_for_an_issue() {
    let (mut screen, log, mut text) = screen_on(demo::view());
    screen.select_ref(&local("t-1"));
    key(&mut screen, &mut text, 'd');
    assert_eq!(
        sent(&log),
        [Intent::SetState {
            id: "t-1".into(),
            state: State::Closed,
        }]
    );
    screen.select_ref(&gh(12));
    key(&mut screen, &mut text, 'd');
    assert!(sent(&log).is_empty());
    // A closed local task is reopened (再開).
    screen.select_ref(&local("t-4"));
    key(&mut screen, &mut text, 'd');
    assert_eq!(
        sent(&log),
        [Intent::SetState {
            id: "t-4".into(),
            state: State::Open,
        }]
    );
}

#[test]
fn ac05_enter_on_an_issue_loads_its_detail() {
    let mut view = demo::view();
    view.github.details.clear();
    let (mut screen, log, mut text) = screen_on(view);
    screen.select_ref(&gh(15));
    special(&mut screen, &mut text, Key::Enter);
    assert_eq!(sent(&log), [Intent::LoadDetail(15)]);
    assert_eq!(screen.detail, Some(gh(15)));
    assert!(!screen.tree.is_hidden(screen.side));
    // A local task has nothing to load; Escape closes the detail.
    screen.select_ref(&local("t-1"));
    special(&mut screen, &mut text, Key::Enter);
    assert!(sent(&log).is_empty());
    assert_eq!(screen.detail, Some(local("t-1")));
    special(&mut screen, &mut text, Key::Escape);
    assert_eq!(screen.detail, None);
    assert!(screen.tree.is_hidden(screen.side));
    // A loaded issue is not loaded again.
    let (mut screen, log, mut text) = screen_on(demo::view());
    screen.select_ref(&gh(12));
    special(&mut screen, &mut text, Key::Enter);
    assert!(sent(&log).is_empty());
}

#[test]
fn ac05_enter_on_an_issue_whose_detail_failed_loads_it_again() {
    let mut view = demo::view();
    view.github.details.insert(15, Err(GhFailure::Timeout));
    let (mut screen, log, mut text) = screen_on(view);
    screen.select_ref(&gh(15));
    special(&mut screen, &mut text, Key::Enter);
    assert_eq!(sent(&log), [Intent::LoadDetail(15)]);
}

/// Opens the label filter's menu from the keyboard.
fn open_label_menu(screen: &mut Tasks, text: &mut TextStore) {
    screen.tree.set_focus(Some(screen.label_popup));
    special(screen, text, Key::Space);
    assert!(
        screen
            .tree
            .get::<PopUpButton>(screen.label_popup)
            .unwrap()
            .is_open()
    );
    assert!(screen.in_overlay());
}

#[test]
fn ac05_letters_go_to_an_open_label_menu_not_to_the_shortcuts() {
    let (mut screen, log, mut text) = screen_on(demo::view());
    screen.select_ref(&local("t-1"));
    open_label_menu(&mut screen, &mut text);
    // `d` type-selects docs; it neither closes t-1 nor is dropped.
    key(&mut screen, &mut text, 'd');
    for c in ['r', 'n', 'x', 'o', '/', 'h', 'j', 'k', 'l'] {
        screen.dispatch(
            Event::KeyDown {
                key: Key::Char(c),
                modifiers: Modifiers::default(),
            },
            &mut text,
        );
    }
    assert!(sent(&log).is_empty());
    assert!(screen.form.is_none());
    assert_ne!(screen.tree.focused(), Some(screen.text));
    assert_eq!(screen.selected, Some(local("t-1")));
    special(&mut screen, &mut text, Key::Enter);
    assert_eq!(screen.filter.label.as_deref(), Some("docs"));
}

#[test]
fn ac05_escape_closes_an_open_menu_before_the_detail() {
    let (mut screen, _, mut text) = screen_on(demo::view());
    screen.select_ref(&local("t-1"));
    special(&mut screen, &mut text, Key::Enter);
    assert_eq!(screen.detail, Some(local("t-1")));
    open_label_menu(&mut screen, &mut text);
    special(&mut screen, &mut text, Key::Escape);
    assert!(
        !screen
            .tree
            .get::<PopUpButton>(screen.label_popup)
            .unwrap()
            .is_open()
    );
    assert_eq!(screen.detail, Some(local("t-1")), "the detail stays");
    // The next Escape is the screen's again.
    screen.tree.set_focus(None);
    special(&mut screen, &mut text, Key::Escape);
    assert_eq!(screen.detail, None);
}

#[test]
fn ac05_shift_slash_is_not_the_filter_key() {
    let (mut screen, log, mut text) = screen_on(demo::view());
    let shift = Modifiers {
        shift: true,
        ..Modifiers::default()
    };
    // As the platform delivers `?`: the unshifted key, then the text.
    screen.dispatch(
        Event::KeyDown {
            key: Key::Char('/'),
            modifiers: shift,
        },
        &mut text,
    );
    screen.dispatch(Event::Text("?".into()), &mut text);
    screen.prepare(window(), &mut text);
    assert_ne!(screen.tree.focused(), Some(screen.text));
    assert_eq!(screen.filter.text, "");
    assert_eq!(
        screen.tree.get::<TextField>(screen.text).unwrap().text(),
        ""
    );
    // A capital is still its letter's shortcut, and types nothing.
    screen.dispatch(
        Event::KeyDown {
            key: Key::Char('r'),
            modifiers: shift,
        },
        &mut text,
    );
    screen.dispatch(Event::Text("R".into()), &mut text);
    assert_eq!(sent(&log), [Intent::Refresh]);
}

/// Hands out each view pushed onto `views`.
struct QueueFeed {
    views: Rc<RefCell<Vec<View>>>,
    sent: Rc<RefCell<Vec<Intent>>>,
}

impl Feed for QueueFeed {
    fn poll(&mut self) -> Option<View> {
        let mut views = self.views.borrow_mut();
        (!views.is_empty()).then(|| views.remove(0))
    }

    fn act(&mut self, intent: Intent) {
        self.sent.borrow_mut().push(intent);
    }
}

/// `--select=#12` before the issues arrive opens #12 once a View lists it;
/// a key pressed meanwhile drops the request.
#[test]
fn a_select_before_the_task_is_listed_opens_it_once_it_is() {
    let full = demo::view();
    let mut empty = full.clone();
    empty.github.tasks.clear();
    let views = Rc::new(RefCell::new(vec![empty.clone()]));
    let mut screen = Tasks::new(Box::new(QueueFeed {
        views: views.clone(),
        sent: Rc::default(),
    }));
    let mut text = TextStore::new(Default::default());
    screen.woken(&mut text);
    screen.prepare(window(), &mut text);
    screen.select_when_listed("#12", &mut text);
    assert_eq!(screen.detail, None);
    views.borrow_mut().push(full.clone());
    screen.woken(&mut text);
    assert_eq!(screen.detail, Some(gh(12)));

    // A key in between: the person moved on, nothing jumps later.
    let views = Rc::new(RefCell::new(vec![empty]));
    let mut screen = Tasks::new(Box::new(QueueFeed {
        views: views.clone(),
        sent: Rc::default(),
    }));
    screen.woken(&mut text);
    screen.prepare(window(), &mut text);
    screen.select_when_listed("#12", &mut text);
    key(&mut screen, &mut text, 'j');
    views.borrow_mut().push(full);
    screen.woken(&mut text);
    assert_ne!(screen.detail, Some(gh(12)));
}

fn column_scroll(screen: &Tasks, column: Column) -> WidgetId {
    screen.tree.children(screen.columns[column as usize])[1]
}

fn detail_scroll(screen: &Tasks) -> WidgetId {
    screen.tree.children(screen.detail_host)[0]
}

fn offset(screen: &Tasks, scroll: WidgetId) -> f32 {
    screen.tree.get::<ScrollView>(scroll).unwrap().offset()
}

fn scroll_to(screen: &mut Tasks, scroll: WidgetId, y: f32) {
    screen
        .tree
        .update::<ScrollView, _>(scroll, |s, cx| s.set_offset(cx, y));
}

/// Many open local tasks with long bodies, so 未着手 and a detail scroll.
fn long_view() -> View {
    let mut view = demo::view();
    let body = (0..80).map(|i| format!("line {i}\n")).collect::<String>();
    for n in 0..40 {
        let mut task = view.tasks[0].clone();
        task.r#ref = local(&format!("t-9{n:02}"));
        task.title = format!("task {n}");
        task.body = Some(body.clone());
        task.problem = None;
        task.state = State::Open;
        view.tasks.push(task);
    }
    view
}

/// A View that only moves the clock (the feed sends one each minute) must
/// not throw the reader back to the top of a column or of the detail.
#[test]
fn a_new_view_keeps_where_the_columns_and_the_detail_were_scrolled() {
    let view = long_view();
    let views = Rc::new(RefCell::new(vec![view.clone()]));
    let mut screen = Tasks::new(Box::new(QueueFeed {
        views: views.clone(),
        sent: Rc::default(),
    }));
    let mut text = TextStore::new(Default::default());
    screen.woken(&mut text);
    screen.prepare(window(), &mut text);
    assert!(screen.select("t-905", &mut text));
    screen.prepare(window(), &mut text);
    let (col, det) = (column_scroll(&screen, Column::Todo), detail_scroll(&screen));
    scroll_to(&mut screen, col, 300.0);
    scroll_to(&mut screen, det, 200.0);
    screen.prepare(window(), &mut text);
    let mut later = view;
    later.now += 60_000;
    views.borrow_mut().push(later);
    screen.woken(&mut text);
    screen.prepare(window(), &mut text);
    assert_eq!(offset(&screen, column_scroll(&screen, Column::Todo)), 300.0);
    assert_eq!(offset(&screen, detail_scroll(&screen)), 200.0);
}

/// `j` past the bottom of a column brings the selected card into view.
#[test]
fn j_past_the_bottom_of_a_column_scrolls_the_selected_card_into_view() {
    let (mut screen, _, mut text) = screen_on(long_view());
    for _ in 0..30 {
        key(&mut screen, &mut text, 'j');
    }
    screen.prepare(window(), &mut text);
    let selected = screen.selected.clone().unwrap();
    let card = card_of(&screen, &selected);
    let card = screen.tree.window_rect(card).unwrap();
    let view = screen
        .tree
        .window_rect(column_scroll(&screen, Column::Todo))
        .unwrap();
    assert!(
        card.y >= view.y - 0.5 && card.y + card.height <= view.y + view.height + 0.5,
        "{selected:?} at {card:?} outside {view:?}"
    );
}

fn notice(screen: &Tasks) -> String {
    screen
        .tree
        .get::<Label>(screen.notice)
        .unwrap()
        .text()
        .to_owned()
}

/// `x` refuses what `runs::may_start` refuses, as the detail's 実行 button
/// does: a local file that does not parse, and a closed task whose run is
/// still live (one live run per task).
#[test]
fn x_opens_no_run_form_on_a_broken_file_or_a_closed_task_with_a_live_run() {
    let mut view = demo::view();
    view.agents = Some(AgentKind::ALL.to_vec());
    let live = gh(15);
    let at = view
        .github
        .tasks
        .iter()
        .position(|t| t.r#ref == live)
        .unwrap();
    view.github.tasks[at].state = State::Closed;
    view.github.tasks[at].closed = Some(view.now);
    let (mut screen, _, mut text) = screen_on(view);
    for r in [local("t-5"), live] {
        screen.select_ref(&r);
        key(&mut screen, &mut text, 'x');
        assert!(screen.form.is_none(), "a run form opened on {r:?}");
        assert!(!notice(&screen).is_empty(), "no reason given for {r:?}");
    }
}

#[test]
fn the_screens_own_notice_gives_way_to_a_press_and_to_the_feeds_next_notice() {
    let mut view = demo::view();
    view.agents = None;
    let views = Rc::new(RefCell::new(vec![view.clone()]));
    let log = Rc::new(RefCell::new(Vec::new()));
    let mut screen = Tasks::new(Box::new(QueueFeed {
        views: views.clone(),
        sent: log.clone(),
    }));
    let mut text = TextStore::new(Default::default());
    screen.prepare(window(), &mut text);
    let refused = "インストール済みのエージェントを確認しています";

    // A new notice from the feed replaces the screen's own.
    screen.select_ref(&local("t-1"));
    key(&mut screen, &mut text, 'x');
    assert_eq!(notice(&screen), refused);
    view.notice = Some("GitHub の取得に失敗しました".into());
    views.borrow_mut().push(view.clone());
    screen.woken(&mut text);
    assert_eq!(notice(&screen), "GitHub の取得に失敗しました");

    // A View without a new notice keeps it; a click on 再取得 clears it.
    key(&mut screen, &mut text, 'x');
    assert_eq!(notice(&screen), refused);
    views.borrow_mut().push(view.clone());
    screen.woken(&mut text);
    assert_eq!(notice(&screen), refused);
    sent(&log);
    let at = screen.tree.window_rect(screen.refresh_button).unwrap();
    let pos = noa_gpui_scene::Point::new(at.x + at.width / 2.0, at.y + at.height / 2.0);
    let button = noa_gpui_core::PointerButton::Primary;
    screen.dispatch(Event::PointerDown { pos, button }, &mut text);
    screen.dispatch(Event::PointerUp { pos, button }, &mut text);
    assert_eq!(sent(&log), [Intent::Refresh]);
    assert_eq!(notice(&screen), "GitHub の取得に失敗しました");
}

// ---------------------------------------------------------------- AC-6

fn header(screen: &Tasks) -> (String, Option<String>) {
    let status = screen
        .tree
        .get::<Label>(screen.gh_status)
        .unwrap()
        .text()
        .to_owned();
    let failure = (!screen.tree.is_hidden(screen.gh_failure)).then(|| {
        screen
            .tree
            .get::<Label>(screen.gh_failure)
            .unwrap()
            .text()
            .to_owned()
    });
    (status, failure)
}

#[test]
fn ac06_the_github_header_gives_the_repo_and_the_listings_age() {
    let mut view = demo::view();
    view.github.fetched = Some(view.now - 3 * 60_000 - 59_000);
    let (screen, _, _) = screen_on(view.clone());
    assert_eq!(
        header(&screen),
        (format!("{} · 3 分前の取得", demo::REPO), None)
    );

    view.github.refreshing = true;
    let (screen, _, _) = screen_on(view.clone());
    assert_eq!(header(&screen), (format!("{} · 取得中", demo::REPO), None));

    view.github = GitHubView::default();
    let (screen, _, _) = screen_on(view);
    assert_eq!(header(&screen), ("未取得".to_owned(), None));
}

#[test]
fn ac06_each_failure_gives_its_reason_and_leaves_local_cards_alone() {
    let local_cards = |screen: &Tasks| -> Vec<Vec<TaskRef>> {
        shown(screen)
            .into_iter()
            .map(|c| {
                c.into_iter()
                    .filter(|r| r.source() == Source::Local)
                    .collect()
            })
            .collect()
    };
    let (fine, _, _) = screen_on(demo::view());
    let expected = local_cards(&fine);
    let failures = [
        GhFailure::NoGh,
        GhFailure::NotAuthenticated,
        GhFailure::NotGitHub,
        GhFailure::Timeout,
        GhFailure::Failed("HTTP 502".into()),
    ];
    let mut reasons = BTreeSet::new();
    for failure in failures {
        let mut view = demo::view();
        view.github.failure = Some(failure.clone());
        let (screen, _, _) = screen_on(view);
        let (status, shown_failure) = header(&screen);
        assert!(status.starts_with(demo::REPO), "{status}");
        assert_eq!(shown_failure, Some(failure.reason()));
        assert!(!failure.reason().is_empty());
        reasons.insert(failure.reason());
        assert_eq!(local_cards(&screen), expected, "{failure:?}");
    }
    assert_eq!(reasons.len(), 5, "each failure has its own reason");
    assert!(
        GhFailure::Failed("HTTP 502".into())
            .reason()
            .contains("HTTP 502")
    );
    // Without a listing at all, the local cards are still there.
    let mut view = demo::view();
    view.github = GitHubView {
        failure: Some(GhFailure::NoGh),
        ..GitHubView::default()
    };
    let (screen, _, _) = screen_on(view);
    assert_eq!(
        header(&screen),
        ("未取得".to_owned(), Some(GhFailure::NoGh.reason()))
    );
    assert_eq!(local_cards(&screen), expected);
}

// ------------------------------------------------- list, timeline, calendar

fn click(screen: &mut Tasks, text: &mut TextStore, id: WidgetId) {
    screen.prepare(window(), text);
    let at = screen.tree.window_rect(id).unwrap();
    let pos = noa_gpui_scene::Point::new(at.x + at.width / 2.0, at.y + at.height / 2.0);
    let button = noa_gpui_core::PointerButton::Primary;
    screen.dispatch(Event::PointerDown { pos, button }, text);
    screen.dispatch(Event::PointerUp { pos, button }, text);
    screen.prepare(window(), text);
}

fn rows(screen: &Tasks) -> Vec<TaskRef> {
    screen.cards.iter().map(|(_, r)| r.clone()).collect()
}

fn layout_selected(screen: &Tasks) -> usize {
    let control = screen
        .tree
        .get::<SegmentedControl>(screen.layout_control)
        .unwrap();
    control.selected().iter().position(|s| *s).unwrap()
}

#[test]
fn v_cycles_the_layouts_and_the_control_follows() {
    let (mut screen, _, mut text) = screen_on(demo::view());
    for (n, expected) in [
        Layout::List,
        Layout::Timeline,
        Layout::Calendar,
        Layout::Board,
    ]
    .into_iter()
    .enumerate()
    {
        key(&mut screen, &mut text, 'v');
        assert_eq!(screen.layout, expected, "after {} presses", n + 1);
        assert_eq!(layout_selected(&screen), (n + 1) % 4);
    }
}

#[test]
fn the_list_shows_every_task_the_filter_lets_through_old_done_ones_too() {
    let (mut screen, _, mut text) = screen_on(demo::view());
    screen.set_layout(Layout::List, &mut text);
    let listed = rows(&screen);
    let view = demo::view();
    assert_eq!(listed.len(), view.tasks.len() + view.github.tasks.len());
    // The board leaves these out of 完了 (closed 10 and 30 days ago).
    assert!(listed.contains(&local("t-6")) && listed.contains(&gh(3)));
    // Newest activity first: the running issue (40 minutes ago) leads.
    assert_eq!(listed[0], gh(15));
    // The filter applies.
    screen.filter.source = Some(tasks::model::types::Source::GitHub);
    screen.rebuild(&mut text);
    assert!(
        rows(&screen)
            .iter()
            .all(|r| matches!(r, TaskRef::GitHub { .. }))
    );
}

#[test]
fn the_timeline_shows_what_ran_or_was_closed_in_the_last_two_weeks() {
    let (mut screen, _, mut text) = screen_on(demo::view());
    screen.set_layout(Layout::Timeline, &mut text);
    let shown = rows(&screen);
    // Running now, closed 10 days ago: in; closed 30 days ago, never run: out.
    assert!(
        shown.contains(&gh(15)) && shown.contains(&local("t-6")),
        "{shown:?}"
    );
    assert!(
        !shown.contains(&gh(3)) && !shown.contains(&gh(12)),
        "{shown:?}"
    );
}

#[test]
fn the_calendar_lists_by_day_and_moves_by_month() {
    let (mut screen, _, mut text) = screen_on(demo::view());
    screen.set_layout(Layout::Calendar, &mut text);
    let this_month = rows(&screen);
    assert!(this_month.contains(&gh(15)), "{this_month:?}");
    // Next month holds nothing yet: nothing is planned, only done.
    key(&mut screen, &mut text, ']');
    assert_eq!(screen.month, 1);
    assert!(rows(&screen).is_empty());
    // Two back, then 今月.
    key(&mut screen, &mut text, '[');
    key(&mut screen, &mut text, '[');
    assert_eq!(screen.month, -1);
    let [_, today, _] = screen.month_buttons.unwrap();
    click(&mut screen, &mut text, today);
    assert_eq!(screen.month, 0);
    assert_eq!(rows(&screen), this_month);
}

#[test]
fn a_row_pressed_opens_its_detail_in_every_layout() {
    for layout in [Layout::List, Layout::Timeline, Layout::Calendar] {
        let (mut screen, _, mut text) = screen_on(demo::view());
        screen.set_layout(layout, &mut text);
        screen.prepare(window(), &mut text);
        let (row, r) = screen.cards[0].clone();
        click(&mut screen, &mut text, row);
        assert_eq!(screen.detail, Some(r.clone()), "{layout:?}");
        assert_eq!(screen.selected, Some(r), "{layout:?}");
    }
}

#[test]
fn j_and_k_walk_the_rows_top_to_bottom_and_h_l_do_nothing() {
    let (mut screen, _, mut text) = screen_on(demo::view());
    screen.set_layout(Layout::List, &mut text);
    screen.selected = None;
    let order = rows(&screen);
    key(&mut screen, &mut text, 'j');
    assert_eq!(screen.selected.as_ref(), Some(&order[0]));
    key(&mut screen, &mut text, 'j');
    assert_eq!(screen.selected.as_ref(), Some(&order[1]));
    key(&mut screen, &mut text, 'l');
    assert_eq!(screen.selected.as_ref(), Some(&order[1]));
    key(&mut screen, &mut text, 'k');
    assert_eq!(screen.selected.as_ref(), Some(&order[0]));
    key(&mut screen, &mut text, 'k');
    assert_eq!(screen.selected.as_ref(), Some(&order[0]));
}

/// `j` with no row shown (an empty month, a filter matching nothing) does
/// nothing, in every layout.
#[test]
fn j_with_no_rows_shown_does_nothing() {
    for layout in [Layout::List, Layout::Timeline, Layout::Calendar] {
        let (mut screen, _, mut text) = screen_on(demo::view());
        screen.set_layout(layout, &mut text);
        screen.filter.text = "no task has this".into();
        screen.rebuild(&mut text);
        assert!(rows(&screen).is_empty(), "{layout:?}");
        screen.selected = None;
        key(&mut screen, &mut text, 'j');
        special(&mut screen, &mut text, Key::ArrowDown);
        assert_eq!(screen.selected, None, "{layout:?}");
    }
}

/// A task the timeline or the calendar does not show (no run, never
/// closed) is not the one a shortcut acts on there.
#[test]
fn a_shortcut_never_acts_on_a_task_the_dated_layouts_do_not_show() {
    let (mut screen, log, mut text) = screen_on(demo::view());
    // t-1: open, local, never run.
    screen.select_ref(&local("t-1"));
    assert_eq!(screen.selected, Some(local("t-1")));
    for layout in [Layout::Timeline, Layout::Calendar] {
        screen.set_layout(layout, &mut text);
        assert!(!rows(&screen).contains(&local("t-1")), "{layout:?}");
        assert_eq!(screen.selected, None, "{layout:?}");
        key(&mut screen, &mut text, 'd');
        assert!(sent(&log).is_empty(), "{layout:?}");
    }
}

/// A run that started this very moment is on the timeline already.
#[test]
fn the_timeline_shows_a_run_started_just_now() {
    let mut view = demo::view();
    let mut run = view.runs[0].clone();
    run.run.task = gh(12);
    run.run.started = view.now;
    run.run.closed = None;
    view.runs.push(run);
    let (mut screen, _, mut text) = screen_on(view);
    screen.set_layout(Layout::Timeline, &mut text);
    assert!(rows(&screen).contains(&gh(12)), "{:?}", rows(&screen));
}

/// コード in the source filter shows only the code's markers.
#[test]
fn the_code_source_filter_shows_only_markers() {
    let (mut screen, _, mut text) = screen_on(demo::view());
    screen.set_layout(Layout::List, &mut text);
    screen.filter.source = Some(tasks::model::types::Source::Code);
    screen.rebuild(&mut text);
    let listed = rows(&screen);
    assert_eq!(listed.len(), 2, "{listed:?}");
    assert!(listed.iter().all(|r| matches!(r, TaskRef::Code { .. })));
    assert_eq!(
        SOURCES[3],
        ("コード", Some(tasks::model::types::Source::Code))
    );
}

// ------------------------------------------------------ 関連成果 (#93)

use tasks::links::{
    self as lk, ChangeId, Current, Info, LinkEvent, LinkId, Nav, Node, Origin, Presence, Rel,
    TaskKey,
};

use super::related::RelatedAction;

/// Hands out `view` once, the 関連成果 `related` for the task it is told
/// is open (once per telling), and records every intent.
struct LinkFeed {
    view: Option<View>,
    related: RelatedView,
    watched: Option<TaskRef>,
    sent: Rc<RefCell<Vec<Intent>>>,
}

impl Feed for LinkFeed {
    fn poll(&mut self) -> Option<View> {
        self.view.take()
    }

    fn act(&mut self, intent: Intent) {
        self.sent.borrow_mut().push(intent);
    }

    fn watch(&mut self, task: Option<&TaskRef>) {
        self.watched = task.cloned();
    }

    fn related(&mut self) -> Option<RelatedView> {
        self.watched
            .take()
            .filter(|t| *t == self.related.task)
            .map(|_| self.related.clone())
    }
}

const DATA: &str = "/data/noa";
const SHA1: &str = "1111111111111111111111111111111111111111";
const SHA2: &str = "2222222222222222222222222222222222222222";

fn linked(n: u64, from: Node, to: Node, rel: Rel) -> LinkEvent {
    LinkEvent::Linked {
        id: LinkId::new(format!("lnk-{n}")),
        from,
        to,
        rel,
        origin: Origin::Auto,
        at: n,
    }
}

fn commit(sha: &str) -> Node {
    Node::Change(ChangeId::Commit(sha.to_owned()))
}

/// t-3's 関連成果 from `events`, with v2 (`SHA2`) of `run-1` current and
/// every node found except those in `missing`.
fn related_of(events: &[LinkEvent], missing: &[(Node, &str)]) -> RelatedView {
    let set = lk::fold(events);
    let key = TaskKey::Local("t-3".into());
    let current = Current {
        run: "run-1".into(),
        change: ChangeId::Commit(SHA2.into()),
    };
    let mut presence: std::collections::BTreeMap<Node, Presence> = set
        .nodes()
        .into_iter()
        .map(|n| (n.clone(), Presence::Found))
        .collect();
    for (node, what) in missing {
        presence.insert(
            node.clone(),
            Presence::Missing {
                what: (*what).to_owned(),
            },
        );
    }
    RelatedView {
        task: local("t-3"),
        related: Ok(lk::related(&set, &key, Some(&current))),
        current: Some(current),
        presence,
        data: PathBuf::from(DATA),
        repo: PathBuf::from("/src/noa"),
        at: 0,
    }
}

/// run-1 produced v1 then v2; rev-1 judged v1 (About the task), ver-1
/// judged v2 (About the run).
fn history() -> Vec<LinkEvent> {
    let task = Node::Task(TaskKey::Local("t-3".into()));
    let run = Node::Run("run-1".into());
    let (rev, ver) = (Node::Review("rev-1".into()), Node::Verify("ver-1".into()));
    vec![
        linked(1, task.clone(), run.clone(), Rel::Ran),
        LinkEvent::Observed {
            node: run.clone(),
            info: Info {
                label: Some("claude · task:run-1".into()),
                workdir: Some("/src/noa-wt/task-run-1".into()),
                branch: Some("task-run-1".into()),
                ..Info::default()
            },
            at: 1,
        },
        linked(2, run.clone(), commit(SHA1), Rel::Produced),
        linked(3, rev.clone(), commit(SHA1), Rel::Judged),
        linked(4, rev, task, Rel::About),
        linked(5, run.clone(), commit(SHA2), Rel::Produced),
        linked(6, ver.clone(), commit(SHA2), Rel::Judged),
        linked(7, ver, run, Rel::About),
    ]
}

fn link_screen(related: RelatedView) -> (Tasks, Rc<RefCell<Vec<Intent>>>, TextStore) {
    let sent = Rc::new(RefCell::new(Vec::new()));
    let mut screen = Tasks::new(Box::new(LinkFeed {
        view: Some(demo::view()),
        related,
        watched: None,
        sent: sent.clone(),
    }));
    let mut text = TextStore::new(Default::default());
    screen.prepare(window(), &mut text);
    assert!(screen.select("t-3", &mut text));
    screen.prepare(window(), &mut text);
    (screen, sent, text)
}

/// The detail's texts (labels, tags, buttons), in tree order, from 関連成果 on.
fn section(screen: &Tasks) -> Vec<String> {
    fn walk(tree: &UiTree, id: WidgetId, out: &mut Vec<String>) {
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
            walk(tree, *c, out);
        }
    }
    let mut all = Vec::new();
    walk(&screen.tree, screen.detail_host, &mut all);
    let from = all
        .iter()
        .position(|t| t == "関連成果")
        .unwrap_or_else(|| panic!("no 関連成果 in {all:?}"));
    all.split_off(from)
}

fn pos(list: &[String], what: &str) -> usize {
    list.iter()
        .position(|t| t == what)
        .unwrap_or_else(|| panic!("no {what:?} in {list:?}"))
}

/// Presses the 関連成果 button whose action `pick` matches.
fn press_related(screen: &mut Tasks, text: &mut TextStore, pick: impl Fn(&RelatedAction) -> bool) {
    let id = screen
        .related_actions
        .iter()
        .find(|(_, a)| pick(a))
        .map(|(id, _)| *id)
        .unwrap_or_else(|| panic!("no such button in {:?}", screen.related_actions));
    screen.tree.set_focus(Some(id));
    special(screen, text, Key::Space);
}

/// AC3: the run, then each version it produced in order, each with the
/// results that judged exactly it; each row's button gives its Nav.
#[test]
fn ac3_related_rows_are_in_order_and_each_button_gives_its_nav() {
    let (mut screen, log, mut text) = link_screen(related_of(&history(), &[]));
    let list = section(&screen);
    let order = [
        "関連成果",
        "リンクを追加",
        "実行 run-1 · claude · task:run-1",
        "task-run-1 · /src/noa-wt/task-run-1",
        "版 1111111",
        "レビュー rev-1",
        "旧版 1111111 に対する結果",
        "版 2222222",
        "現在",
        "検証 ver-1",
        "2222222 に対する結果",
    ];
    let at: Vec<usize> = order.iter().map(|w| pos(&list, w)).collect();
    assert!(at.windows(2).all(|w| w[0] < w[1]), "{list:?}");
    // Only the task's own links can be removed here: the run's Ran and
    // the review's About the task; not the run's changes or the
    // verification (About the run, shared with the run's other tasks).
    let actions: Vec<RelatedAction> = screen
        .related_actions
        .iter()
        .map(|(_, a)| a.clone())
        .collect();
    let review = PathBuf::from(DATA).join("reviews/rev-1");
    let verify = PathBuf::from(DATA).join("verifications/ver-1");
    assert_eq!(
        actions,
        [
            RelatedAction::AddLink,
            RelatedAction::Nav(Nav::Run("run-1".into())),
            RelatedAction::AskUnlink {
                what: "実行 run-1".into(),
                links: vec![LinkId::new("lnk-1")],
            },
            RelatedAction::Nav(Nav::Copy(SHA1.into())),
            RelatedAction::Nav(Nav::Review {
                dir: review.clone()
            }),
            RelatedAction::AskUnlink {
                what: "レビュー rev-1".into(),
                links: vec![LinkId::new("lnk-4")],
            },
            RelatedAction::Nav(Nav::Copy(SHA2.into())),
            RelatedAction::Nav(Nav::Verify {
                dir: verify.clone()
            }),
        ]
    );

    // A review opens its stored view in Review: `review --session=<dir>`.
    press_related(&mut screen, &mut text, |a| {
        matches!(a, RelatedAction::Nav(Nav::Review { .. }))
    });
    let got = sent(&log);
    let [Intent::Navigate(nav)] = got.as_slice() else {
        panic!("{got:?}");
    };
    assert_eq!(
        *nav,
        Nav::Review {
            dir: review.clone()
        }
    );
    assert_eq!(nav.argv(), [format!("--session={}", review.display())]);
    assert_eq!(nav.sibling(), Some(lk::REVIEW));
    press_related(&mut screen, &mut text, |a| {
        matches!(a, RelatedAction::Nav(Nav::Verify { .. }))
    });
    assert_eq!(sent(&log), [Intent::Navigate(Nav::Verify { dir: verify })]);
    // A version's id goes to the clipboard; nothing is sent.
    press_related(&mut screen, &mut text, |a| {
        *a == RelatedAction::Nav(Nav::Copy(SHA1.into()))
    });
    assert!(sent(&log).is_empty());
    assert_eq!(screen.tree.clipboard().read_text().as_deref(), Some(SHA1));
    // A run this window logged: its pane when alive, else its worktree.
    // (The demo's runs are not run-1: its work tree, as the store saw it;
    // `/src/noa-wt/task-run-1` is not there, so nothing is opened.)
    press_related(&mut screen, &mut text, |a| {
        matches!(a, RelatedAction::Nav(Nav::Run(_)))
    });
    assert!(sent(&log).is_empty());
}

/// `history` with run-1's work tree observed at `workdir`.
fn history_at(workdir: &std::path::Path) -> Vec<LinkEvent> {
    let mut events = history();
    for event in &mut events {
        if let LinkEvent::Observed { info, .. } = event {
            info.workdir = Some(workdir.to_owned());
        }
    }
    events
}

/// 開く on a run with no pane whose work tree was removed passes no path
/// to the OS: it says the work tree is gone, and the row keeps its
/// 記録が見つからない. A work tree still there is opened.
#[test]
fn opening_a_run_whose_work_tree_is_gone_says_so_and_opens_nothing() {
    let gone = std::env::temp_dir().join("noa-tasks-no-such-worktree-7f3a");
    assert!(!gone.exists());
    let missing = "run-1 /data/noa/tasks/runs.jsonl";
    let related = related_of(&history_at(&gone), &[(Node::Run("run-1".into()), missing)]);
    let (mut screen, log, mut text) = link_screen(related);
    press_related(&mut screen, &mut text, |a| {
        matches!(a, RelatedAction::Nav(Nav::Run(_)))
    });
    assert!(sent(&log).is_empty());
    let notice = screen.own_notice.clone().unwrap_or_default();
    assert_eq!(
        notice,
        format!("run-1 の作業ツリー {} はもうありません", gone.display())
    );
    pos(
        &section(&screen),
        &format!("記録が見つからない（{missing}）"),
    );

    let there = std::env::temp_dir();
    let (mut screen, log, mut text) = link_screen(related_of(&history_at(&there), &[]));
    press_related(&mut screen, &mut text, |a| {
        matches!(a, RelatedAction::Nav(Nav::Run(_)))
    });
    assert_eq!(sent(&log), [Intent::OpenUrl(there.display().to_string())]);
}

/// Hands out `view` once and each 関連成果 put in `next` once, whatever
/// task is open: as the live feed's 10 s refresh does.
struct RefreshFeed {
    view: Option<View>,
    next: Rc<RefCell<Option<RelatedView>>>,
}

impl Feed for RefreshFeed {
    fn poll(&mut self) -> Option<View> {
        self.view.take()
    }

    fn act(&mut self, _: Intent) {}

    fn related(&mut self) -> Option<RelatedView> {
        self.next.borrow_mut().take()
    }
}

/// A refresh that reads the same 関連成果 again (only its time differs)
/// leaves the detail as it is, and the focus on 開く with it; one that
/// reads something new rebuilds it.
#[test]
fn a_refresh_with_the_same_related_keeps_the_detail_and_its_focus() {
    let first = related_of(&history(), &[]);
    let next = Rc::new(RefCell::new(Some(first.clone())));
    let mut screen = Tasks::new(Box::new(RefreshFeed {
        view: Some(demo::view()),
        next: next.clone(),
    }));
    let mut text = TextStore::new(Default::default());
    screen.prepare(window(), &mut text);
    assert!(screen.select("t-3", &mut text));
    screen.prepare(window(), &mut text);
    let open = screen
        .related_actions
        .iter()
        .find(|(_, a)| matches!(a, RelatedAction::Nav(Nav::Run(_))))
        .map(|(id, _)| *id)
        .expect("開く on the run");
    screen.tree.set_focus(Some(open));
    let before = screen.related_actions.clone();

    *next.borrow_mut() = Some(RelatedView {
        at: first.at + 10_000,
        ..first.clone()
    });
    screen.woken(&mut text);
    assert_eq!(screen.tree.focused(), Some(open));
    assert_eq!(screen.related_actions, before);
    assert_eq!(
        screen.related.as_ref().map(|v| v.at),
        Some(first.at + 10_000)
    );

    // ver-1 is gone from what is read: shown at once.
    let mut events = history();
    events.truncate(6);
    *next.borrow_mut() = Some(RelatedView {
        at: first.at + 20_000,
        ..related_of(&events, &[])
    });
    screen.woken(&mut text);
    assert!(!section(&screen).iter().any(|t| t == "検証 ver-1"));
    assert_ne!(screen.related_actions, before);
}

/// A refresh whose store gained nodes of another task only (a review
/// recorded for it) leaves this task's detail and its focus as they are;
/// a presence change on a row this task shows rebuilds it.
#[test]
fn a_refresh_with_only_another_tasks_nodes_new_keeps_the_focus() {
    let first = related_of(&history(), &[]);
    let next = Rc::new(RefCell::new(Some(first.clone())));
    let mut screen = Tasks::new(Box::new(RefreshFeed {
        view: Some(demo::view()),
        next: next.clone(),
    }));
    let mut text = TextStore::new(Default::default());
    screen.prepare(window(), &mut text);
    assert!(screen.select("t-3", &mut text));
    screen.prepare(window(), &mut text);
    let open = screen
        .related_actions
        .iter()
        .find(|(_, a)| matches!(a, RelatedAction::Nav(Nav::Run(_))))
        .map(|(id, _)| *id)
        .expect("開く on the run");
    screen.tree.set_focus(Some(open));
    let before = screen.related_actions.clone();

    // rev-9 judged another task's change: in the store, not in this view.
    let mut presence = first.presence.clone();
    presence.insert(Node::Review("rev-9".into()), Presence::Found);
    presence.insert(
        Node::Task(TaskKey::Local("t-9".into())),
        Presence::Missing { what: "t-9".into() },
    );
    *next.borrow_mut() = Some(RelatedView {
        presence,
        at: first.at + 10_000,
        ..first.clone()
    });
    screen.woken(&mut text);
    assert_eq!(screen.tree.focused(), Some(open));
    assert_eq!(screen.related_actions, before);

    // rev-1's record is gone: its row says so at once.
    *next.borrow_mut() = Some(RelatedView {
        at: first.at + 20_000,
        ..related_of(&history(), &[(Node::Review("rev-1".into()), "rev-1")])
    });
    screen.woken(&mut text);
    assert!(
        section(&screen)
            .iter()
            .any(|t| t == "記録が見つからない（rev-1）")
    );
}

/// AC5: v1's review is shown as a result for an older version while v2 is
/// current; a current version nothing judged yet says so.
#[test]
fn ac5_a_result_for_v1_is_older_while_v2_is_current() {
    let (screen, _, _) = link_screen(related_of(&history(), &[]));
    let list = section(&screen);
    let review = pos(&list, "レビュー rev-1");
    assert_eq!(list[review + 1], "旧版 1111111 に対する結果");
    let v2 = pos(&list, "版 2222222");
    assert_eq!(list[v2 + 1], "現在", "{list:?}");
    assert_eq!(list.iter().filter(|t| *t == "現在").count(), 1);
    assert!(!list.iter().any(|t| t == "旧版 2222222 に対する結果"));

    // Without the verification, v2 has no result yet.
    let mut events = history();
    events.truncate(6);
    let (screen, _, _) = link_screen(related_of(&events, &[]));
    let list = section(&screen);
    let v2 = pos(&list, "版 2222222");
    let none = pos(&list, "この版の結果はまだありません");
    assert!(v2 < none, "{list:?}");
    assert_eq!(
        list.iter()
            .filter(|t| *t == "この版の結果はまだありません")
            .count(),
        1,
        "v1 has its review: {list:?}"
    );
}

/// AC6: a record that cannot be found keeps its row and its links, and
/// says 記録が見つからない with what it pointed to.
#[test]
fn ac6_a_missing_record_keeps_its_row_and_link_and_names_its_path() {
    let gone = format!("rev-1 {DATA}/reviews/rev-1/events.jsonl");
    let related = related_of(
        &history(),
        &[
            (Node::Review("rev-1".into()), gone.as_str()),
            (commit(SHA1), SHA1),
        ],
    );
    let (screen, _, _) = link_screen(related);
    let list = section(&screen);
    let review = pos(&list, "レビュー rev-1");
    let mark = pos(&list, &format!("記録が見つからない（{gone}）"));
    assert!(review < mark, "{list:?}");
    pos(&list, &format!("記録が見つからない（{SHA1}）"));
    // Still listed with its own link, which can still be removed.
    assert!(screen.related_actions.iter().any(|(_, a)| *a
        == RelatedAction::AskUnlink {
            what: "レビュー rev-1".into(),
            links: vec![LinkId::new("lnk-4")],
        }));
    // The found ones are not marked.
    assert_eq!(
        list.iter()
            .filter(|t| t.starts_with("記録が見つからない"))
            .count(),
        2
    );
}

/// A review or verification whose record is missing offers no 開く (its
/// window would only exit), but keeps リンクを外す; a found one opens.
#[test]
fn a_missing_review_offers_no_open_but_keeps_unlink() {
    let gone = format!("rev-1 {DATA}/reviews/rev-1/events.jsonl");
    let related = related_of(&history(), &[(Node::Review("rev-1".into()), gone.as_str())]);
    let (screen, _, _) = link_screen(related);
    let opens = |node: &Node| {
        let nav = Nav::of(
            node,
            std::path::Path::new(DATA),
            std::path::Path::new("/src/noa"),
        )
        .expect("a nav");
        screen
            .related_actions
            .iter()
            .any(|(_, a)| *a == RelatedAction::Nav(nav.clone()))
    };
    assert!(
        !opens(&Node::Review("rev-1".into())),
        "開く on a missing review"
    );
    assert!(opens(&Node::Verify("ver-1".into())), "開く on a found one");
    assert!(screen.related_actions.iter().any(|(_, a)| *a
        == RelatedAction::AskUnlink {
            what: "レビュー rev-1".into(),
            links: vec![LinkId::new("lnk-4")],
        }));
}

/// AC7 (screen): リンクを追加 checks the ID's form, says why it refuses
/// one, and sends the link; リンクを外す asks first and sends nothing
/// until 外す.
#[test]
fn ac7_add_link_form_and_unlink_after_confirmation() {
    let (mut screen, log, mut text) = link_screen(related_of(&history(), &[]));
    press_related(&mut screen, &mut text, |a| *a == RelatedAction::AddLink);
    assert!(screen.form.is_some());
    // 検証, the second kind.
    let mut controls = Vec::new();
    fn all_controls(tree: &UiTree, id: WidgetId, out: &mut Vec<WidgetId>) {
        if tree.get::<SegmentedControl>(id).is_some() {
            out.push(id);
        }
        for c in tree.children(id) {
            all_controls(tree, *c, out);
        }
    }
    all_controls(&screen.tree, screen.form_host, &mut controls);
    let [kind] = controls[..] else {
        panic!("{controls:?}");
    };
    screen.tree.set_focus(Some(kind));
    special(&mut screen, &mut text, Key::ArrowRight);
    special(&mut screen, &mut text, Key::Space);
    let field = {
        fn find(tree: &UiTree, id: WidgetId) -> Option<WidgetId> {
            if tree.get::<TextField>(id).is_some() {
                return Some(id);
            }
            tree.children(id).iter().find_map(|c| find(tree, *c))
        }
        find(&screen.tree, screen.form_host).unwrap()
    };
    // A review's id typed as a verification: refused, with why.
    screen.tree.set_focus(Some(field));
    screen.dispatch(Event::Text("rev-12".into()), &mut text);
    special(&mut screen, &mut text, Key::Enter);
    assert!(screen.form.is_some());
    assert!(sent(&log).is_empty());
    let form_text = labels_under(&screen.tree, screen.form_host);
    assert!(
        form_text
            .iter()
            .any(|t| t == "検証 の ID は ver-… の形式です"),
        "{form_text:?}"
    );
    screen
        .tree
        .update::<TextField, _>(field, |f, cx| f.set_text(cx, String::new()));
    screen.tree.set_focus(Some(field));
    screen.dispatch(Event::Text(" ver-42 ".into()), &mut text);
    special(&mut screen, &mut text, Key::Enter);
    assert_eq!(
        sent(&log),
        [Intent::Link {
            task: local("t-3"),
            title: "noa-tasks スキルの例を最新の形式にする".into(),
            kind: LinkKind::Verify,
            id: "ver-42".into(),
        }]
    );
    assert!(screen.form.is_none());

    // リンクを外す asks; やめる sends nothing.
    let review_row = |a: &RelatedAction| matches!(a, RelatedAction::AskUnlink { what, .. } if what == "レビュー rev-1");
    press_related(&mut screen, &mut text, review_row);
    let list = section(&screen);
    pos(&list, "「レビュー rev-1」のリンクを外しますか？");
    press_related(&mut screen, &mut text, |a| {
        *a == RelatedAction::CancelUnlink
    });
    assert!(sent(&log).is_empty());
    assert!(!section(&screen).iter().any(|t| t.ends_with("外しますか？")));
    // 外す sends the row's own links.
    press_related(&mut screen, &mut text, review_row);
    press_related(&mut screen, &mut text, |a| {
        *a == RelatedAction::ConfirmUnlink
    });
    assert_eq!(
        sent(&log),
        [Intent::Unlink {
            task: local("t-3"),
            links: vec![LinkId::new("lnk-4")],
        }]
    );
}

/// The demo's t-3 shows its 関連成果 at once (`--demo --select=t-3
/// --snapshot` renders it without a live feed).
#[test]
fn the_demo_shows_t3s_related_section() {
    let mut screen = Tasks::new(Box::<DemoFeed>::default());
    let mut text = TextStore::new(Default::default());
    screen.prepare(window(), &mut text);
    assert!(screen.select("t-3", &mut text));
    let list = section(&screen);
    pos(&list, "現在");
    assert!(list.iter().any(|t| t.starts_with("旧版 ")), "{list:?}");
    assert!(list.iter().any(|t| t.starts_with("検証 ver-")), "{list:?}");
    assert!(screen.select("t-1", &mut text));
    pos(&section(&screen), "関連する成果はまだありません");
}
