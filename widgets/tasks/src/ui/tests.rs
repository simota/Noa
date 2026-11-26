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

// ------------------------------------------- auto-assign (docs/specs/auto-assign.md, U6)

use std::sync::mpsc;

use noa_gpui_core::widgets::PullDownButton;
use tasks::auto_assign::{
    Assignment, AutoRecord, Cancel, Job, PLANNERS, Pick, PlanError, Planning,
};

use super::auto::{AutoState, Hooks};

/// What the fake planner was handed: the job, the channel its result goes
/// on, and its cancel flag.
type Planned = Rc<RefCell<Vec<(Job, mpsc::Sender<Result<Assignment, PlanError>>, Cancel)>>>;

/// The assignment's arrival time the fake clock gives.
const ARRIVED: Millis = 42;

/// Replaces the real planner thread: each start is recorded, and the test
/// sends its result.
fn fake_planner(screen: &mut Tasks) -> Planned {
    let planned: Planned = Rc::default();
    let log = planned.clone();
    screen.auto_hooks = Hooks {
        plan: Box::new(move |job, _wake| {
            let (tx, rx) = mpsc::channel();
            let cancel = Cancel::default();
            log.borrow_mut().push((job, tx, cancel.clone()));
            Planning { rx, cancel }
        }),
        clock: Box::new(|| ARRIVED),
    };
    planned
}

/// The 実行 form on t-1 with `agents` installed, and the fake planner.
fn run_form_with(agents: &[AgentKind]) -> (Tasks, Rc<RefCell<Vec<Intent>>>, TextStore, Planned) {
    let mut view = demo::view();
    view.agents = Some(agents.to_vec());
    let (mut screen, log, mut text) = screen_on(view);
    let planned = fake_planner(&mut screen);
    screen.select_ref(&local("t-1"));
    key(&mut screen, &mut text, 'x');
    assert!(screen.form.is_some(), "the 実行 form is open");
    (screen, log, text, planned)
}

fn find_under(tree: &UiTree, id: WidgetId, pred: &dyn Fn(WidgetId) -> bool) -> Option<WidgetId> {
    if pred(id) {
        return Some(id);
    }
    tree.children(id)
        .iter()
        .find_map(|c| find_under(tree, *c, pred))
}

fn auto_menu(screen: &Tasks) -> WidgetId {
    find_under(&screen.tree, screen.form_host, &|id| {
        screen.tree.get::<PullDownButton>(id).is_some()
    })
    .expect("no 自動で割り当て in the form")
}

fn agent_segment(screen: &Tasks) -> WidgetId {
    find_under(&screen.tree, screen.form_host, &|id| {
        screen.tree.get::<SegmentedControl>(id).is_some()
    })
    .unwrap()
}

fn agent_selected(screen: &Tasks) -> Vec<bool> {
    let id = agent_segment(screen);
    screen
        .tree
        .get::<SegmentedControl>(id)
        .unwrap()
        .selected()
        .to_vec()
}

fn visible(tree: &UiTree, id: WidgetId) -> bool {
    !tree.is_hidden(id) && !tree.ancestors(id).any(|a| tree.is_hidden(a))
}

/// The form's labels a reader sees, in tree order (empty ones left out).
fn form_texts(screen: &Tasks) -> Vec<String> {
    fn walk(tree: &UiTree, id: WidgetId, out: &mut Vec<String>) {
        if let Some(l) = tree.get::<Label>(id)
            && visible(tree, id)
            && !l.text().is_empty()
        {
            out.push(l.text().to_owned());
        }
        for c in tree.children(id) {
            walk(tree, *c, out);
        }
    }
    let mut out = Vec::new();
    walk(&screen.tree, screen.form_host, &mut out);
    out
}

/// Presses the form's button labelled `label` from the keyboard.
fn press_button(screen: &mut Tasks, text: &mut TextStore, label: &str) {
    let tree = &screen.tree;
    let button = find_under(tree, screen.form_host, &|id| {
        tree.get::<Button>(id).is_some_and(|b| b.label() == label)
    })
    .unwrap_or_else(|| panic!("no button {label:?}"));
    assert!(visible(&screen.tree, button), "{label} is hidden");
    screen.tree.set_focus(Some(button));
    special(screen, text, Key::Space);
}

/// Chooses the pull-down's item `i` from the keyboard.
fn choose_planner(screen: &mut Tasks, text: &mut TextStore, i: usize) {
    let menu = auto_menu(screen);
    screen.tree.set_focus(Some(menu));
    special(screen, text, Key::Space);
    assert!(screen.in_overlay(), "the menu is open");
    for _ in 0..=i {
        special(screen, text, Key::ArrowDown);
    }
    special(screen, text, Key::Enter);
}

/// Moves the agent segment `by` from the keyboard and selects it.
fn pick_by_hand(screen: &mut Tasks, text: &mut TextStore, by: Key) {
    let segment = agent_segment(screen);
    screen.tree.set_focus(Some(segment));
    special(screen, text, by);
    special(screen, text, Key::Space);
}

fn answer(planned: &Planned, at: usize, result: Result<Assignment, PlanError>) {
    planned.borrow()[at].1.send(result).unwrap();
}

fn worker(agent: &str, reason: &str) -> Pick {
    Pick {
        slot: "worker".into(),
        agent: agent.into(),
        model: None,
        effort: None,
        reason: reason.into(),
    }
}

fn assignment(pick: Pick) -> Assignment {
    Assignment {
        planner: PLANNERS[0].label(),
        picks: vec![pick],
    }
}

const HEADING: &str =
    "自動割り当て（claude (sonnet · medium) が決定）— 内容を確認して「実行」を押してください";

/// L1-2 step 1 and L1-4: 「自動で割り当て」 is in the 実行 form under the
/// エージェント segment, its items are the installed planners (never agy),
/// and choosing one starts that planner on the task (L1-3), showing the
/// caption and 中止 in place of the pull-down.
#[test]
fn auto_the_pull_down_is_in_the_run_form_and_starts_the_chosen_planner() {
    let (mut screen, _, mut text, planned) = run_form_with(&AgentKind::ALL);
    let menu = auto_menu(&screen);
    assert!(screen.tree.is_enabled(menu));
    let texts = form_texts(&screen);
    let at = |s: &str| texts.iter().position(|t| t == s).unwrap();
    assert!(at("エージェント") < at("作業場所"), "{texts:?}");
    let auto = screen.form.as_ref().unwrap().auto().unwrap();
    let names: Vec<&str> = auto.planners().iter().map(|p| p.agent).collect();
    assert_eq!(names, ["claude", "codex"]);

    choose_planner(&mut screen, &mut text, 1);
    let mut jobs = planned.borrow_mut();
    let [(job, _, cancel)] = &mut jobs[..] else {
        panic!("one planner started");
    };
    assert_eq!(job.planner, PLANNERS[1], "codex");
    assert!(!cancel.load(std::sync::atomic::Ordering::Relaxed));
    let request = std::mem::replace(&mut job.request, Box::new(|| unreachable!()));
    let req = request();
    let task = screen.task(&local("t-1")).unwrap().clone();
    assert_eq!(req.slots.len(), 1);
    assert_eq!(req.slots[0].id, "worker");
    let candidates: Vec<&str> = req.candidates.iter().map(|c| c.agent.as_str()).collect();
    assert_eq!(
        candidates,
        ["claude", "codex", "agy"],
        "agy may be assigned"
    );
    assert!(
        req.candidates
            .iter()
            .all(|c| c.models.is_empty() && c.efforts.is_empty())
    );
    assert!(
        req.content.starts_with(&format!("Title: {}\n", task.title)),
        "{}",
        req.content
    );
    drop(jobs);
    assert!(
        !visible(&screen.tree, menu),
        "the caption replaces the pull-down"
    );
    assert!(
        form_texts(&screen)
            .contains(&"codex (gpt-6.2-sol · medium) が割り当てを決めています…".to_owned()),
        "{:?}",
        form_texts(&screen)
    );
    assert!(matches!(
        screen.form.as_ref().unwrap().auto().unwrap().state(),
        AutoState::Running { .. }
    ));
}

/// L1-4: with agy the only agent there is no planner: the pull-down is
/// disabled and the note says why.
#[test]
fn auto_with_only_agy_the_pull_down_is_disabled_and_says_why() {
    let (mut screen, _, mut text, planned) = run_form_with(&[AgentKind::Agy]);
    let menu = auto_menu(&screen);
    assert!(!screen.tree.is_enabled(menu));
    assert!(
        form_texts(&screen)
            .contains(&"自動割り当てに使えるエージェントがインストールされていません".to_owned())
    );
    screen.tree.set_focus(Some(menu));
    special(&mut screen, &mut text, Key::Space);
    assert!(planned.borrow().is_empty());
}

/// L1-2 step 3 and L1-7: a valid assignment selects its agent's segment
/// and shows the block with the reason; 実行 sends `Intent::Run` with the
/// `AutoRecord` of the kept pick.
#[test]
fn auto_a_fill_selects_the_segment_shows_the_reason_and_run_carries_auto() {
    let (mut screen, log, mut text, planned) = run_form_with(&AgentKind::ALL);
    assert_eq!(agent_selected(&screen), [true, false, false]);
    choose_planner(&mut screen, &mut text, 0);
    let pick = worker("agy", "画面の文言修正だけなので軽いエージェントで足りる");
    answer(&planned, 0, Ok(assignment(pick.clone())));
    screen.woken(&mut text);
    screen.prepare(window(), &mut text);
    assert_eq!(agent_selected(&screen), [false, false, true]);
    let block =
        format!("{HEADING}\nエージェント: agy — 画面の文言修正だけなので軽いエージェントで足りる");
    assert!(
        form_texts(&screen).contains(&block),
        "{:?}",
        form_texts(&screen)
    );
    assert!(
        visible(&screen.tree, auto_menu(&screen)),
        "the pull-down is back"
    );

    press_button(&mut screen, &mut text, "実行");
    assert_eq!(
        sent(&log),
        [Intent::Run {
            task: local("t-1"),
            agent: AgentKind::Agy,
            isolation: Isolation::Worktree,
            auto: Some(AutoRecord {
                planner: "claude (sonnet · medium)".into(),
                at: ARRIVED,
                picks: vec![pick],
                overridden: Vec::new(),
            }),
        }]
    );
}

/// L1-2 step 6 and L1-7: an agent changed by hand after the fill is marked
/// 「（手動で変更）」 and not recorded: Tasks' one pick changed leaves the
/// run manual (`auto: None`).
#[test]
fn auto_an_agent_changed_after_the_fill_is_marked_and_not_recorded() {
    let (mut screen, log, mut text, planned) = run_form_with(&AgentKind::ALL);
    choose_planner(&mut screen, &mut text, 0);
    answer(&planned, 0, Ok(assignment(worker("agy", "軽い"))));
    screen.woken(&mut text);
    pick_by_hand(&mut screen, &mut text, Key::ArrowLeft);
    assert_eq!(agent_selected(&screen), [false, true, false]);
    let block = format!("{HEADING}\nエージェント: agy — 軽い（手動で変更）");
    assert!(
        form_texts(&screen).contains(&block),
        "{:?}",
        form_texts(&screen)
    );
    press_button(&mut screen, &mut text, "実行");
    assert_eq!(
        sent(&log),
        [Intent::Run {
            task: local("t-1"),
            agent: AgentKind::Codex,
            isolation: Isolation::Worktree,
            auto: None,
        }]
    );
}

/// L1-2 step 5: a failure leaves the form as it was and names its cause.
#[test]
fn auto_a_failure_leaves_the_form_and_names_the_cause() {
    let (mut screen, log, mut text, planned) = run_form_with(&AgentKind::ALL);
    choose_planner(&mut screen, &mut text, 0);
    answer(
        &planned,
        0,
        Err(PlanError::Exit {
            code: Some(1),
            stderr_tail: "Error: not logged in\n".into(),
        }),
    );
    screen.woken(&mut text);
    assert_eq!(agent_selected(&screen), [true, false, false], "untouched");
    let note = "自動割り当てに失敗しました: claude (sonnet · medium) が終了コード 1 で終了しました（Error: not logged in）。手動で選ぶか、別のエージェントで決めてください";
    assert!(
        form_texts(&screen).contains(&note.to_owned()),
        "{:?}",
        form_texts(&screen)
    );
    assert!(visible(&screen.tree, auto_menu(&screen)));

    // Another planner may be tried; an invalid answer is a failure too.
    choose_planner(&mut screen, &mut text, 1);
    answer(
        &planned,
        1,
        Err(PlanError::Invalid("worker の割り当てがありません".into())),
    );
    screen.woken(&mut text);
    let note = "自動割り当てに失敗しました: codex (gpt-6.2-sol · medium) の割り当てが不正です: worker の割り当てがありません。手動で選ぶか、別のエージェントで決めてください";
    assert!(
        form_texts(&screen).contains(&note.to_owned()),
        "{:?}",
        form_texts(&screen)
    );
    press_button(&mut screen, &mut text, "実行");
    assert_eq!(
        sent(&log),
        [Intent::Run {
            task: local("t-1"),
            agent: AgentKind::Claude,
            isolation: Isolation::Worktree,
            auto: None,
        }]
    );
}

/// L1-2 step 4: an agent chosen by hand while the planner ran is kept: the
/// assignment is not filled in, and the note says why.
#[test]
fn auto_a_fill_after_the_agent_changed_meanwhile_is_refused() {
    let (mut screen, log, mut text, planned) = run_form_with(&AgentKind::ALL);
    choose_planner(&mut screen, &mut text, 0);
    pick_by_hand(&mut screen, &mut text, Key::ArrowRight);
    assert_eq!(agent_selected(&screen), [false, true, false]);
    answer(&planned, 0, Ok(assignment(worker("agy", "軽い"))));
    screen.woken(&mut text);
    assert_eq!(agent_selected(&screen), [false, true, false], "not filled");
    let note = "割り当て中にエージェントの選択が変わったため反映しませんでした。もう一度「自動で割り当て」を押してください";
    assert!(
        form_texts(&screen).contains(&note.to_owned()),
        "{:?}",
        form_texts(&screen)
    );
    press_button(&mut screen, &mut text, "実行");
    let got = sent(&log);
    assert!(
        matches!(
            &got[..],
            [Intent::Run {
                agent: AgentKind::Codex,
                auto: None,
                ..
            }]
        ),
        "{got:?}"
    );
}

/// L1-2 step 2: 中止 cancels the planner (its flag is set, so `run` kills
/// its group), the pull-down comes back, and a result that arrives after
/// is dropped. 実行 while a planner runs starts the manual pick and cancels
/// that planner too.
#[test]
fn auto_stop_and_run_while_planning_cancel_the_planner() {
    let (mut screen, log, mut text, planned) = run_form_with(&AgentKind::ALL);
    let cancelled = |at: usize| {
        planned.borrow()[at]
            .2
            .load(std::sync::atomic::Ordering::Relaxed)
    };
    choose_planner(&mut screen, &mut text, 0);
    press_button(&mut screen, &mut text, "中止");
    assert!(cancelled(0));
    assert!(visible(&screen.tree, auto_menu(&screen)));
    assert!(
        !form_texts(&screen)
            .iter()
            .any(|t| t.contains("決めています")),
        "{:?}",
        form_texts(&screen)
    );
    // The stopped planner's late answer is not filled in.
    let _ = planned.borrow()[0]
        .1
        .send(Ok(assignment(worker("agy", "軽い"))));
    screen.woken(&mut text);
    assert_eq!(agent_selected(&screen), [true, false, false]);

    choose_planner(&mut screen, &mut text, 0);
    assert!(!cancelled(1));
    press_button(&mut screen, &mut text, "実行");
    assert!(cancelled(1), "closing the form stops its planner");
    assert_eq!(
        sent(&log),
        [Intent::Run {
            task: local("t-1"),
            agent: AgentKind::Claude,
            isolation: Isolation::Worktree,
            auto: None,
        }]
    );
}

/// The planner reads an issue's body once its detail is loaded, as the
/// run's agent does (L1-3: the material `runs::brief` writes).
#[test]
fn auto_the_planner_reads_a_loaded_issues_body() {
    let mut view = demo::view();
    view.agents = Some(vec![AgentKind::Claude]);
    let issue = view.github.tasks[0].clone();
    let TaskRef::GitHub { number, .. } = issue.r#ref.clone() else {
        panic!("{issue:?}");
    };
    view.github.details.insert(
        number,
        Ok(tasks::model::types::IssueDetail {
            body: "issue の本文".into(),
            comments: Vec::new(),
        }),
    );
    let (mut screen, _, mut text) = screen_on(view);
    let planned = fake_planner(&mut screen);
    screen.select_ref(&issue.r#ref);
    key(&mut screen, &mut text, 'x');
    choose_planner(&mut screen, &mut text, 0);
    let mut jobs = planned.borrow_mut();
    let request = std::mem::replace(&mut jobs[0].0.request, Box::new(|| unreachable!()));
    let content = request().content;
    assert!(content.contains("issue の本文"), "{content}");
    assert!(content.contains(&format!("Source: #{number}")), "{content}");
}

/// Hands out the View last put in `next`, once, as a feed does when its
/// sources change; records nothing.
struct Later {
    next: Rc<RefCell<Option<View>>>,
}

impl Feed for Later {
    fn poll(&mut self) -> Option<View> {
        self.next.borrow_mut().take()
    }

    fn act(&mut self, _intent: Intent) {}
}

/// The request the fake planner's start `at` was handed.
fn planned_request(planned: &Planned, at: usize) -> tasks::auto_assign::Request {
    let mut jobs = planned.borrow_mut();
    let request = std::mem::replace(&mut jobs[at].0.request, Box::new(|| unreachable!()));
    request()
}

/// L1-3: the planner reads the task as it is when a planner is chosen, not
/// when the form opened. 実行 pressed on an issue before its detail loaded,
/// then the body arriving while the form is open: the next planner reads
/// that body, as the run's brief would; before it arrived, the planner was
/// told it is not loaded, as the run would be.
#[test]
fn auto_the_planner_reads_an_issues_body_loaded_after_the_form_opened() {
    let mut view = demo::view();
    view.agents = Some(vec![AgentKind::Claude]);
    let issue = view.github.tasks[0].clone();
    let TaskRef::GitHub { number, .. } = issue.r#ref.clone() else {
        panic!("{issue:?}");
    };
    // Its detail not loaded yet (the demo has some loaded).
    view.github.details.remove(&number);
    assert!(issue.body.is_none());
    let next = Rc::new(RefCell::new(Some(view.clone())));
    let mut screen = Tasks::new(Box::new(Later { next: next.clone() }));
    let mut text = TextStore::new(Default::default());
    screen.prepare(window(), &mut text);
    let planned = fake_planner(&mut screen);
    screen.select_ref(&issue.r#ref);
    key(&mut screen, &mut text, 'x');
    assert!(screen.form.is_some(), "the 実行 form is open");

    choose_planner(&mut screen, &mut text, 0);
    let before = planned_request(&planned, 0).content;
    assert!(
        before.ends_with("(The issue's text is not loaded.)\n"),
        "{before}"
    );
    press_button(&mut screen, &mut text, "中止");

    // `LoadDetail` finishes: the feed's next View has the body.
    view.github.details.insert(
        number,
        Ok(tasks::model::types::IssueDetail {
            body: "あとから読み込んだ本文".into(),
            comments: Vec::new(),
        }),
    );
    *next.borrow_mut() = Some(view);
    screen.woken(&mut text);
    screen.prepare(window(), &mut text);
    assert!(screen.form.is_some(), "the form stays open");

    choose_planner(&mut screen, &mut text, 0);
    let after = planned_request(&planned, 1).content;
    assert!(after.contains("あとから読み込んだ本文"), "{after}");
    assert!(!after.contains("not loaded"), "{after}");
}

/// `--demo` starts nothing: 「自動で割り当て」 is answered by the demo's
/// canned planner, not a CLI, and the demo has no waker, so the answer is
/// shown as the planner is chosen, without a wake.
#[test]
fn auto_in_the_demo_the_canned_answer_shows_without_a_wake() {
    let mut screen = Tasks::new(Box::<DemoFeed>::default());
    let mut text = TextStore::new(Default::default());
    screen.prepare(window(), &mut text);
    assert!(screen.feed.waker().is_none(), "the demo cannot wake");
    screen.select_ref(&local("t-1"));
    key(&mut screen, &mut text, 'x');
    assert!(screen.form.is_some(), "the 実行 form is open");
    assert_eq!(agent_selected(&screen), [true, false, false]);

    // No `woken` between choosing codex and reading the form.
    choose_planner(&mut screen, &mut text, 1);
    assert!(
        matches!(
            screen.form.as_ref().unwrap().auto().unwrap().state(),
            AutoState::Filled { .. }
        ),
        "{:?}",
        screen.form.as_ref().unwrap().auto()
    );
    assert_eq!(agent_selected(&screen), [false, true, false]);
    let block = format!(
        "自動割り当て（codex (gpt-6.2-sol · medium) が決定）— 内容を確認して「実行」を押してください\nエージェント: codex — {}",
        demo::PLANNED
    );
    assert!(
        form_texts(&screen).contains(&block),
        "{:?}",
        form_texts(&screen)
    );
}
