//! Workspace interactions through the same keys and controls as the window.
//! These assertions concern state, focus and persisted values, not font metrics.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};

use noa_gpui_core::widgets::CommandPalette;
use noa_gpui_scene::Size;

use tasks::workspace::{self, Change, Priority, Sort, Workflow, Workspace};

use super::organizer::Action;
use super::*;
use crate::demo;

struct TestFeed {
    pending: Rc<RefCell<VecDeque<View>>>,
    sent: Rc<RefCell<Vec<Intent>>>,
    path: Option<PathBuf>,
}

impl Feed for TestFeed {
    fn poll(&mut self) -> Option<View> {
        self.pending.borrow_mut().pop_front()
    }

    fn act(&mut self, intent: Intent) {
        self.sent.borrow_mut().push(intent);
    }

    fn workspace_path(&self) -> Option<PathBuf> {
        self.path.clone()
    }
}

struct Harness {
    screen: Tasks,
    text: TextStore,
    pending: Rc<RefCell<VecDeque<View>>>,
    sent: Rc<RefCell<Vec<Intent>>>,
}

impl Harness {
    fn new(path: Option<PathBuf>) -> Self {
        let pending = Rc::new(RefCell::new(VecDeque::from([demo::view()])));
        let sent = Rc::new(RefCell::new(Vec::new()));
        let feed = TestFeed {
            pending: pending.clone(),
            sent: sent.clone(),
            path,
        };
        let mut screen = Tasks::new(Box::new(feed));
        screen.tree.set_reduce_motion(true);
        let mut text = TextStore::new(Default::default());
        screen.initialize_layout(None, &mut text);
        let mut harness = Self {
            screen,
            text,
            pending,
            sent,
        };
        harness.prepare();
        harness
    }

    fn prepare(&mut self) {
        self.screen
            .prepare(Size::new(1280.0, 860.0), &mut self.text);
        let now = self.screen.tree.now() + 1.0;
        self.screen.tree.animate(now, &mut self.text);
        for message in self.screen.tree.take_messages() {
            self.screen.handle(message, &mut self.text);
        }
        self.screen
            .prepare(Size::new(1280.0, 860.0), &mut self.text);
    }

    fn key(&mut self, key: Key) {
        self.screen.dispatch(
            Event::KeyDown {
                key,
                modifiers: Modifiers::default(),
            },
            &mut self.text,
        );
        self.prepare();
    }

    fn character(&mut self, c: char) {
        self.key(Key::Char(c));
        self.screen
            .dispatch(Event::Text(c.to_string()), &mut self.text);
        self.prepare();
    }

    fn command(&mut self, key: Key) {
        self.screen.dispatch(
            Event::KeyDown {
                key,
                modifiers: Modifiers {
                    ctrl: true,
                    command: true,
                    ..Default::default()
                },
            },
            &mut self.text,
        );
        self.prepare();
    }

    fn type_text(&mut self, value: &str) {
        self.screen
            .dispatch(Event::Text(value.into()), &mut self.text);
        self.prepare();
    }

    fn search(&mut self, value: &str) {
        self.screen.tree.set_focus(Some(self.screen.text));
        self.command(Key::Char('a'));
        self.type_text(value);
    }

    fn activate(&mut self, id: WidgetId) {
        self.screen.tree.set_focus(Some(id));
        assert_eq!(self.screen.tree.focused(), Some(id));
        self.key(Key::Space);
    }

    fn select_popup(&mut self, id: WidgetId, index: usize) {
        let selected = self.screen.tree.get::<PopUpButton>(id).unwrap().selected();
        self.activate(id);
        for _ in 0..selected.abs_diff(index) {
            self.key(if index < selected {
                Key::ArrowUp
            } else {
                Key::ArrowDown
            });
        }
        self.key(Key::Enter);
    }

    fn select_task(&mut self, id: &str) {
        assert!(self.screen.select(id, &mut self.text));
        self.screen.tree.set_focus(None);
        self.prepare();
    }

    fn navigate(&mut self, section: &str) {
        let id = self
            .screen
            .planning
            .nav_controls
            .buttons
            .iter()
            .find_map(|(id, action)| {
                matches!(action, Action::Navigate(key) if key == section).then_some(*id)
            })
            .expect("navigation entry");
        self.activate(id);
    }

    fn save_button(&self) -> WidgetId {
        button_under(&self.screen.tree, self.screen.form_host, "保存")
    }

    fn set_property(&mut self, name: &str, value: &str) -> WidgetId {
        let id = field_under(&self.screen.tree, self.screen.form_host, name);
        self.screen
            .tree
            .update::<TextField, _>(id, |field, cx| field.set_text(cx, value));
        id
    }
}

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let seq = NEXT.fetch_add(1, Ordering::Relaxed);
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "noa-tasks-planning-ui-{}-{stamp}-{seq}",
            std::process::id()
        ));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn file(&self) -> PathBuf {
        self.0.join(workspace::FILE_NAME)
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn local(id: &str) -> TaskRef {
    TaskRef::Local(id.into())
}

fn descendants(tree: &UiTree, id: WidgetId) -> Vec<WidgetId> {
    std::iter::once(id)
        .chain(
            tree.children(id)
                .iter()
                .flat_map(|child| descendants(tree, *child)),
        )
        .collect()
}

fn button_under(tree: &UiTree, host: WidgetId, label: &str) -> WidgetId {
    descendants(tree, host)
        .into_iter()
        .find(|id| {
            tree.get::<Button>(*id)
                .is_some_and(|button| button.label() == label)
        })
        .expect("button with label")
}

fn field_under(tree: &UiTree, host: WidgetId, label: &str) -> WidgetId {
    for parent in descendants(tree, host) {
        for children in tree.children(parent).windows(2) {
            if tree
                .get::<Label>(children[0])
                .is_some_and(|heading| heading.text() == label)
                && tree.get::<TextField>(children[1]).is_some()
            {
                return children[1];
            }
        }
    }
    panic!("field headed {label}");
}

fn popups_under(tree: &UiTree, host: WidgetId) -> Vec<WidgetId> {
    descendants(tree, host)
        .into_iter()
        .filter(|id| tree.get::<PopUpButton>(*id).is_some())
        .collect()
}

fn visible(screen: &Tasks) -> BTreeSet<TaskRef> {
    screen.cards.iter().map(|(_, task)| task.clone()).collect()
}

#[test]
fn command_palette_works_while_search_has_focus_and_executes_its_choice() {
    let mut h = Harness::new(None);
    h.search("config");
    h.command(Key::Char('k'));
    let id = h.screen.tree.focused().unwrap();
    assert!(h.screen.tree.get::<CommandPalette>(id).is_some());
    h.type_text("新規タスク");
    let palette = h.screen.tree.get::<CommandPalette>(id).unwrap();
    assert_eq!(palette.query_text(), "新規タスク");
    assert_eq!(
        palette.commands()[palette.highlighted().unwrap()].title(),
        "新規タスク"
    );
    h.key(Key::Enter);
    assert!(h.screen.form.is_some());
    assert!(!h.screen.planning.form_open());
    assert_eq!(h.screen.filter.text, "config");
    assert!(h.screen.tree.overlays().next().is_none());
    assert!(h.sent.borrow().is_empty());
}

#[test]
fn space_and_row_toggle_select_tasks_for_one_bulk_change_and_one_undo() {
    let mut h = Harness::new(None);
    h.select_task("t-1");
    h.key(Key::Space);
    assert_eq!(h.screen.planning.checked, BTreeSet::from([local("t-1")]));
    let toggle = h
        .screen
        .planning
        .row_controls
        .buttons
        .iter()
        .find_map(|(id, action)| {
            matches!(action, Action::ToggleSelection(task) if *task == local("t-2")).then_some(*id)
        })
        .unwrap();
    h.activate(toggle);
    let expected = BTreeSet::from([local("t-1"), local("t-2")]);
    assert_eq!(h.screen.planning.checked, expected);
    assert_eq!(h.screen.detail, Some(local("t-1")));
    let status = popups_under(&h.screen.tree, h.screen.planning.toolbar_host)[0];
    h.screen.tree.set_focus(Some(status));
    h.key(Key::ArrowDown);
    for task in &expected {
        assert_eq!(
            h.screen.planning.workspace.meta(task).workflow,
            Some(Workflow::Backlog)
        );
    }
    assert_eq!(
        h.screen.planning.workspace.meta(&local("t-3")).workflow,
        None
    );
    assert_eq!(h.screen.planning.checked, expected);
    h.screen.tree.set_focus(None);
    h.command(Key::Char('z'));
    for task in &expected {
        assert_eq!(h.screen.planning.workspace.meta(task).workflow, None);
    }
    assert!(!h.screen.planning.workspace.can_undo());
    assert!(h.sent.borrow().is_empty());
}

#[test]
fn an_inline_priority_change_updates_only_its_row_and_does_not_open_detail() {
    let mut h = Harness::new(None);
    h.select_task("t-1");
    let open = h
        .screen
        .cards
        .iter()
        .find(|(_, task)| *task == local("t-2"))
        .unwrap()
        .0;
    let row = h.screen.tree.parent(open).unwrap();
    let frame = h.screen.tree.parent(row).unwrap();
    let priority = popups_under(&h.screen.tree, frame)[1];
    h.screen.tree.set_focus(Some(priority));
    h.key(Key::ArrowDown);
    assert_eq!(
        h.screen.planning.workspace.meta(&local("t-2")).priority,
        Priority::Urgent
    );
    assert_eq!(
        h.screen.planning.workspace.meta(&local("t-1")).priority,
        Priority::None
    );
    assert_eq!(h.screen.detail, Some(local("t-1")));
    assert!(h.screen.planning.checked.is_empty());
}

#[test]
fn metadata_search_prunes_bulk_selection_and_typing_does_not_run_shortcuts() {
    let mut h = Harness::new(None);
    h.screen
        .planning
        .workspace
        .apply(
            &[local("t-1")],
            &[
                Change::Project("改善".into()),
                Change::Priority(Priority::High),
            ],
            demo::NOW,
        )
        .unwrap();
    h.screen.tree.set_focus(None);
    h.command(Key::Char('a'));
    assert!(h.screen.planning.checked.len() > 1);
    h.search("project:改善 priority:high");
    let expected = BTreeSet::from([local("t-1")]);
    assert_eq!(visible(&h.screen), expected);
    assert_eq!(h.screen.planning.checked, expected);
    h.search("csp");
    h.character('c');
    h.character('s');
    h.character('p');
    assert_eq!(h.screen.filter.text, "cspcsp");
    assert!(h.screen.form.is_none());
    assert!(!h.screen.planning.form_open());
    assert!(h.screen.tree.overlays().next().is_none());
    assert!(h.screen.planning.checked.is_empty());
    assert!(h.sent.borrow().is_empty());
}

#[test]
fn rejected_properties_and_live_refresh_keep_the_draft_until_a_valid_save() {
    let mut h = Harness::new(None);
    h.select_task("t-1");
    h.character('e');
    let project = h.set_property("プロジェクト", "入力中のプロジェクト");
    let due = h.set_property("期限", "2026-02-30");
    h.activate(h.save_button());
    assert!(h.screen.planning.property_form.is_some());
    assert!(
        h.screen
            .planning
            .workspace
            .meta(&local("t-1"))
            .project
            .is_empty()
    );
    assert!(
        h.screen
            .own_notice
            .as_deref()
            .is_some_and(|notice| notice.contains("期限"))
    );
    let mut refreshed = demo::view();
    refreshed.tasks[0].title = "取得元で更新されたタイトル".into();
    h.pending.borrow_mut().push_back(refreshed);
    h.screen.woken(&mut h.text);
    h.prepare();
    assert_eq!(
        h.screen.tree.get::<TextField>(project).unwrap().text(),
        "入力中のプロジェクト"
    );
    assert_eq!(
        h.screen.tree.get::<TextField>(due).unwrap().text(),
        "2026-02-30"
    );
    h.set_property("期限", "2026-12-01");
    h.activate(h.save_button());
    assert!(!h.screen.planning.form_open());
    let meta = h.screen.planning.workspace.meta(&local("t-1"));
    assert_eq!(meta.project, "入力中のプロジェクト");
    assert_eq!(meta.due, "2026-12-01");
    assert!(h.sent.borrow().is_empty());
}

#[test]
fn a_refreshed_external_edit_is_not_overwritten_by_an_open_property_draft() {
    let dir = TestDirectory::new();
    let mut h = Harness::new(Some(dir.file()));
    h.select_task("t-1");
    h.character('e');
    let project = h.set_property("プロジェクト", "自分の編集中の値");
    let mut other = Workspace::open(dir.file()).unwrap();
    other
        .apply(
            &[local("t-1")],
            &[Change::Project("別の画面の編集".into())],
            demo::NOW,
        )
        .unwrap();
    h.screen.woken(&mut h.text);
    h.activate(h.save_button());
    assert!(h.screen.planning.property_form.is_some());
    assert_eq!(
        h.screen.tree.get::<TextField>(project).unwrap().text(),
        "自分の編集中の値"
    );
    assert_eq!(
        h.screen.planning.workspace.meta(&local("t-1")).project,
        "別の画面の編集"
    );
    assert_eq!(
        Workspace::open(dir.file())
            .unwrap()
            .meta(&local("t-1"))
            .project,
        "別の画面の編集"
    );
}

#[test]
fn saved_view_restores_source_label_query_sort_layout_and_survives_reopening() {
    let dir = TestDirectory::new();
    let mut h = Harness::new(Some(dir.file()));
    h.screen.tree.set_focus(Some(h.screen.source));
    h.key(Key::ArrowRight);
    h.key(Key::ArrowRight);
    h.key(Key::Space);
    assert_eq!(h.screen.filter.source, Some(Source::GitHub));
    let label = h
        .screen
        .label_choices
        .iter()
        .position(|label| label == "enhancement")
        .unwrap()
        + 1;
    h.select_popup(h.screen.label_popup, label);
    h.search("state:todo");
    let sort = popups_under(&h.screen.tree, h.screen.planning.toolbar_host)[0];
    h.select_popup(
        sort,
        Sort::ALL
            .iter()
            .position(|sort| *sort == Sort::Title)
            .unwrap(),
    );
    h.screen.tree.set_focus(Some(h.screen.layout_control));
    h.key(Key::ArrowLeft);
    h.key(Key::Space);
    assert_eq!(h.screen.layout, Layout::Board);
    let expected = visible(&h.screen);
    assert!(!expected.is_empty());
    let save = h
        .screen
        .planning
        .nav_controls
        .buttons
        .iter()
        .find_map(|(id, action)| matches!(action, Action::SaveView).then_some(*id))
        .unwrap();
    h.activate(save);
    let name = h.screen.tree.focused().expect("view name receives focus");
    assert!(h.screen.tree.get::<TextField>(name).is_some());
    h.type_text("GitHub の未着手");
    h.activate(h.save_button());
    let saved = h
        .screen
        .planning
        .workspace
        .data()
        .views
        .last()
        .unwrap()
        .clone();
    assert_eq!(saved.query, "state:todo");
    assert_eq!(saved.source.as_deref(), Some("github"));
    assert_eq!(saved.label.as_deref(), Some("enhancement"));
    assert_eq!(saved.sort, Sort::Title);
    assert_eq!(saved.layout, "board");

    h.search("config");
    h.screen.tree.set_focus(Some(h.screen.source));
    h.key(Key::ArrowLeft);
    h.key(Key::ArrowLeft);
    h.key(Key::Space);
    h.select_popup(h.screen.label_popup, 0);
    h.screen.set_layout(Layout::List, &mut h.text);
    h.navigate(&format!("saved:{}", saved.id));
    assert_restored(&h, &expected);

    let reopened = Harness::new(Some(dir.file()));
    assert_eq!(
        reopened.screen.planning.section,
        format!("saved:{}", saved.id)
    );
    assert_restored(&reopened, &expected);
}

fn assert_restored(h: &Harness, expected: &BTreeSet<TaskRef>) {
    assert_eq!(h.screen.filter.text, "state:todo");
    assert_eq!(
        h.screen
            .tree
            .get::<TextField>(h.screen.text)
            .unwrap()
            .text(),
        "state:todo"
    );
    assert_eq!(h.screen.filter.source, Some(Source::GitHub));
    assert!(
        h.screen
            .tree
            .get::<SegmentedControl>(h.screen.source)
            .unwrap()
            .selected()[2]
    );
    assert_eq!(h.screen.filter.label.as_deref(), Some("enhancement"));
    let at = h
        .screen
        .tree
        .get::<PopUpButton>(h.screen.label_popup)
        .unwrap()
        .selected();
    assert_eq!(h.screen.label_choices[at - 1], "enhancement");
    assert_eq!(h.screen.planning.sort, Sort::Title);
    assert_eq!(h.screen.layout, Layout::Board);
    assert!(
        h.screen
            .tree
            .get::<SegmentedControl>(h.screen.layout_control)
            .unwrap()
            .selected()[0]
    );
    assert_eq!(&visible(&h.screen), expected);
}
