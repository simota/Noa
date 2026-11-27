//! The screen on the demo state and on a recording backend. The tests
//! assert labels, ids and tree order, never a position or a text width, so
//! they run on Linux too; the one exception asks only whether the selected
//! row lies within the list's visible area.

use std::cell::RefCell;
use std::rc::Rc;

use noa_gpui_core::widgets::{Checkbox, ScrollView, TextField};
use noa_gpui_scene::Size;

use scaffold::model::{EvidenceKind, EvidenceRef, Lifecycle, Observation};
use scaffold::registry::LoadError;

use super::*;
use crate::demo;

/// The demo state (or `state`), recording every action; actions succeed,
/// and a hand-off links a task to its candidate as the live store does.
struct Recorder {
    state: Rc<RefCell<State>>,
    sent: Rc<RefCell<Vec<Action>>>,
    refuse: Option<String>,
}

impl Backend for Recorder {
    fn scan(&mut self) -> State {
        self.state.borrow().clone()
    }

    fn act(&mut self, action: Action, _state: &State) -> Result<String, String> {
        self.sent.borrow_mut().push(action.clone());
        if let Some(why) = &self.refuse {
            return Err(why.clone());
        }
        let task_subject = match action {
            Action::HandOff { improvement } => Some(Subject::Improvement(improvement)),
            Action::SetupTask => Some(Subject::Improvement(self.state.borrow().setup.task_key())),
            _ => None,
        };
        if let Some(subject) = task_subject {
            let id = format!("task-{}", self.sent.borrow().len());
            self.state
                .borrow_mut()
                .observations
                .push(Observation::Task {
                    subject,
                    path: format!("/r/.noa/tasks/{id}.md").into(),
                    id,
                    at: demo::NOW,
                });
        }
        Ok("記録しました".into())
    }

    fn now(&self) -> Millis {
        demo::NOW
    }

    fn offset(&self, _at: Millis) -> Millis {
        0
    }
}

fn window() -> Size {
    Size::new(1280.0, 860.0)
}

/// The screen on `state`, which the test may change before a rescan.
type Shared = Rc<RefCell<State>>;

fn screen_shared(
    state: State,
    refuse: Option<&str>,
) -> (Scaffold, Rc<RefCell<Vec<Action>>>, TextStore, Shared) {
    let sent = Rc::new(RefCell::new(Vec::new()));
    let shared = Rc::new(RefCell::new(state));
    let backend = Recorder {
        state: shared.clone(),
        sent: sent.clone(),
        refuse: refuse.map(str::to_owned),
    };
    let mut screen = Scaffold::new(Box::new(backend));
    let mut text = TextStore::new(Default::default());
    screen.set_tab(Tab::Assets, &mut text);
    screen.prepare(window(), &mut text);
    (screen, sent, text, shared)
}

fn screen_on(
    state: State,
    refuse: Option<&str>,
) -> (Scaffold, Rc<RefCell<Vec<Action>>>, TextStore) {
    let (screen, sent, text, _) = screen_shared(state, refuse);
    (screen, sent, text)
}

fn demo_screen() -> (Scaffold, Rc<RefCell<Vec<Action>>>, TextStore) {
    screen_on(demo::state(), None)
}

fn labels_under(tree: &UiTree, id: WidgetId) -> Vec<String> {
    let own = tree
        .get::<Label>(id)
        .map(|l| l.text().to_owned())
        .or_else(|| tree.get::<Tag>(id).map(|t| t.text().to_owned()))
        .or_else(|| tree.get::<Button>(id).map(|b| b.label().to_owned()));
    own.into_iter()
        .chain(
            tree.children(id)
                .iter()
                .flat_map(|c| labels_under(tree, *c)),
        )
        .collect()
}

fn list_labels(s: &Scaffold) -> Vec<String> {
    labels_under(&s.tree, s.ids.list)
}

fn detail_labels(s: &Scaffold) -> Vec<String> {
    labels_under(&s.tree, s.ids.detail)
}

fn header(s: &Scaffold) -> Vec<String> {
    [s.ids.repo, s.ids.commit, s.ids.score, s.ids.scanned]
        .iter()
        .flat_map(|id| labels_under(&s.tree, *id))
        .collect()
}

fn key(s: &mut Scaffold, text: &mut TextStore, c: char) {
    s.dispatch(
        Event::KeyDown {
            key: Key::Char(c),
            modifiers: Modifiers::default(),
        },
        text,
    );
    s.dispatch(Event::Text(c.to_string()), text);
    s.prepare(window(), text);
}

fn special(s: &mut Scaffold, text: &mut TextStore, key: Key) {
    s.dispatch(
        Event::KeyDown {
            key,
            modifiers: Modifiers::default(),
        },
        text,
    );
    s.prepare(window(), text);
}

fn sent(log: &Rc<RefCell<Vec<Action>>>) -> Vec<Action> {
    std::mem::take(&mut *log.borrow_mut())
}

fn has(labels: &[String], text: &str) -> bool {
    labels.iter().any(|l| l.contains(text))
}

/// Clicks the detail button labelled `label`.
fn click(s: &mut Scaffold, text: &mut TextStore, label: &str) {
    let id = s
        .presses
        .iter()
        .map(|(id, _)| *id)
        .chain(
            s.form
                .iter()
                .flat_map(|_| button_ids(&s.tree, s.ids.form_host)),
        )
        .find(|id| {
            s.tree
                .get::<Button>(*id)
                .is_some_and(|b| b.label() == label)
        })
        .unwrap_or_else(|| panic!("no button {label:?}"));
    let message = message_from(&mut s.tree, id);
    s.handle(message, text);
    s.prepare(window(), text);
}

fn button_ids(tree: &UiTree, id: WidgetId) -> Vec<WidgetId> {
    let own = tree.get::<Button>(id).map(|_| id);
    own.into_iter()
        .chain(tree.children(id).iter().flat_map(|c| button_ids(tree, *c)))
        .collect()
}

/// A click on `id`, as the tree reports it: a pointer press would need
/// its laid-out position; Space on the focused button does the same.
fn message_from(tree: &mut UiTree, id: WidgetId) -> Message {
    tree.set_focus(Some(id));
    let mut text = TextStore::new(Default::default());
    tree.dispatch(
        Event::KeyDown {
            key: Key::Space,
            modifiers: Modifiers::default(),
        },
        &mut text,
    );
    tree.take_messages()
        .into_iter()
        .find(|m| m.source == id && m.downcast_ref::<ButtonClicked>().is_some())
        .expect("the button reports a click")
}

fn type_into(s: &mut Scaffold, text: &mut TextStore, field: WidgetId, typed: &str) {
    s.tree
        .update::<TextField, _>(field, |f, cx| f.set_text(cx, typed));
    s.prepare(window(), text);
}

fn fields(tree: &UiTree, id: WidgetId) -> Vec<WidgetId> {
    let own = tree.get::<TextField>(id).map(|_| id);
    own.into_iter()
        .chain(tree.children(id).iter().flat_map(|c| fields(tree, *c)))
        .collect()
}

// ------------------------------------------------------------ header

#[test]
fn the_header_shows_repo_commit_score_and_scan_time() {
    let (s, _, _) = demo_screen();
    assert_eq!(
        header(&s),
        [
            "/Users/demo/src/noa",
            "commit 4cb1762",
            "整合 2/4",
            "最終スキャン 2026-11-26 09:30",
        ]
    );
}

#[test]
fn no_score_without_a_registry_or_with_an_unknown_source() {
    let mut state = demo::state();
    state.registry = Err(LoadError::Absent);
    state.commit = None;
    let (s, _, _) = screen_on(state, None);
    assert!(has(&header(&s), "スコアなし（registry なし）"));
    assert!(has(&header(&s), "commit 不明"));

    let mut state = demo::state();
    state.report.sources[0].state = SourceState::Unknown;
    state.report.sources[0].error = Some("invalid JSON".into());
    let (s, _, _) = screen_on(state, None);
    assert!(has(&header(&s), "スコアなし（不明な情報源あり）"));
}

// ------------------------------------------------------------ tabs

#[test]
fn the_assets_tab_lists_each_asset_with_its_states() {
    let (s, _, _) = demo_screen();
    assert_eq!(s.tab, Tab::Assets);
    assert_eq!(
        s.items(Tab::Assets),
        [
            Item::Asset("check-widget-registration".into()),
            Item::Asset("add-widget-runbook".into()),
        ]
    );
    let labels = list_labels(&s);
    assert_eq!(
        labels[0], "ウィジェット登録の整合チェック",
        "the human title leads the row"
    );
    assert!(has(
        &labels,
        "チェッカー · scripts/check-widget-registration.py"
    ));
    assert!(has(&labels, "評価中"));
    assert!(has(&labels, "確認済み"));
    assert!(has(&labels, "試作"));
    assert!(has(&labels, "省略"));
    assert!(!has(&labels, "記録に矛盾"));
}

#[test]
fn a_contradictory_history_is_flagged() {
    let mut state = demo::state();
    let loaded = state.registry.as_mut().unwrap();
    loaded.registry.assets[1].decisions[0].to = Lifecycle::Adopted;
    let (s, _, _) = screen_on(state, None);
    assert!(has(&list_labels(&s), "記録に矛盾"));
}

#[test]
fn the_candidates_tab_lists_open_then_resolved() {
    let (mut s, _, mut text) = demo_screen();
    key(&mut s, &mut text, '2');
    assert_eq!(s.tab, Tab::Candidates);
    let labels = list_labels(&s);
    let widgets: Vec<&String> = labels
        .iter()
        .filter(|l| l.contains('/') && !l.contains(':'))
        .collect();
    assert_eq!(
        widgets,
        [
            "scaffold/verify",
            "tasks/macos_bundle",
            "agent-usage/linux_package"
        ]
    );
    assert!(has(&labels, "登録漏れ"));
    assert!(has(&labels, "scripts/verify.sh:79"));
    assert!(has(&labels, "未着手"));
    assert!(has(&labels, "引き渡し済み（t-1795680000000）"));
    assert!(has(&labels, "解消"));
}

/// The window's minimum size (`main.rs`, `options`).
fn smallest() -> Size {
    Size::new(960.0, 600.0)
}

/// Presses `key` and lays the screen out at the smallest window.
fn key_small(s: &mut Scaffold, text: &mut TextStore, key: Key) {
    s.dispatch(
        Event::KeyDown {
            key,
            modifiers: Modifiers::default(),
        },
        text,
    );
    if let Key::Char(c) = key {
        s.dispatch(Event::Text(c.to_string()), text);
    }
    s.prepare(smallest(), text);
}

/// Whether the selected row lies within the list's visible area.
fn selected_row_shown(s: &Scaffold) -> bool {
    let selection = s.selection().expect("a selection");
    let (row, _) = s.rows.iter().find(|(_, i)| *i == selection).unwrap();
    let row = s.tree.window_rect(*row).unwrap();
    let view = s.tree.window_rect(s.ids.list).unwrap();
    row.y >= view.y - 0.5 && row.y + row.height <= view.y + view.height + 0.5
}

#[test]
fn moving_the_selection_keeps_its_row_in_view() {
    let (mut s, _, mut text) = demo_screen();
    s.prepare(smallest(), &mut text);
    let mut overflowed = false;
    for (tab, digit) in Tab::ALL.into_iter().zip(['0', '1', '2', '3']) {
        key_small(&mut s, &mut text, Key::Char(digit));
        assert_eq!(s.tab, tab);
        let n = s.items(tab).len();
        assert!(n > 1, "{tab:?}");
        for (moves, by) in [(n, 'j'), (n, 'k'), (n, 'j')] {
            for _ in 0..moves {
                key_small(&mut s, &mut text, Key::Char(by));
                assert!(selected_row_shown(&s), "{tab:?} {by} {:?}", s.selection());
            }
        }
        // Left at the last row: a rescan keeps it and keeps it in view.
        assert_eq!(s.selection(), s.items(tab).last().cloned());
        key_small(&mut s, &mut text, Key::Char('r'));
        assert!(selected_row_shown(&s), "{tab:?} after r");
        let list = s.tree.get::<ScrollView>(s.ids.list).unwrap();
        overflowed |= list.offset() > 0.0;
    }
    // The case at hand: some tab is longer than the smallest window.
    assert!(overflowed);
    // Each tab keeps its selection (the last row); the list's offset is
    // shared, so switching must bring that tab's row into view.
    for digit in ['3', '1', '3', '2', '3'] {
        key_small(&mut s, &mut text, Key::Char(digit));
        assert!(selected_row_shown(&s), "tab {digit}");
    }
    // The arrows move too.
    key_small(&mut s, &mut text, Key::ArrowUp);
    assert!(selected_row_shown(&s));
}

#[test]
fn the_sources_tab_lists_every_source_and_unknown_errors() {
    let mut state = demo::state();
    state.report.sources[0].state = SourceState::Unknown;
    state.report.sources[0].error = Some("anchor not found: fn launch_args".into());
    let (mut s, _, mut text) = screen_on(state, None);
    key(&mut s, &mut text, '3');
    let labels = list_labels(&s);
    assert_eq!(s.items(Tab::Sources).len(), 18);
    assert!(has(&labels, "不明"));
    assert!(has(&labels, "anchor not found: fn launch_args"));
    assert_eq!(labels.iter().filter(|l| *l == "ok").count(), 17);
}

#[test]
fn an_unregistered_repo_shows_sources_only() {
    let mut state = demo::state();
    state.registry = Err(LoadError::Absent);
    let (mut s, _, mut text) = screen_on(state, None);
    assert!(s.items(Tab::Assets).is_empty());
    assert!(has(&list_labels(&s), "未登録リポジトリ"));
    key(&mut s, &mut text, '2');
    assert!(s.items(Tab::Candidates).is_empty());
    assert!(has(&list_labels(&s), "未登録リポジトリ"));
    key(&mut s, &mut text, '3');
    assert_eq!(s.items(Tab::Sources).len(), 18);
}

// ------------------------------------------------------------ keys

#[test]
fn j_k_move_enter_opens_escape_closes() {
    let (mut s, _, mut text) = demo_screen();
    assert!(s.tree.is_hidden(s.ids.detail));
    key(&mut s, &mut text, 'j');
    assert_eq!(
        s.selection(),
        Some(Item::Asset("add-widget-runbook".into()))
    );
    key(&mut s, &mut text, 'j');
    assert_eq!(
        s.selection(),
        Some(Item::Asset("add-widget-runbook".into()))
    );
    key(&mut s, &mut text, 'k');
    assert_eq!(
        s.selection(),
        Some(Item::Asset("check-widget-registration".into()))
    );
    special(&mut s, &mut text, Key::Enter);
    assert!(!s.tree.is_hidden(s.ids.detail));
    let detail = detail_labels(&s);
    assert!(has(&detail, "ウィジェット登録の整合チェック"));
    assert!(has(&detail, "id: check-widget-registration"));
    assert!(has(&detail, "根拠: dev/scaffold/registry.json:31"));
    assert!(has(&detail, "検証状態: 確認済み"));
    assert!(has(
        &detail,
        "2026-11-24 評価中: 登録漏れを 2 件見つけた（根拠: ver-1795500000000）"
    ));
    assert!(has(&detail, "検証 ver-1795500000000"));
    for b in [
        "外す",
        "検証を関連付け (L)",
        "レビューを関連付け",
        "省略にする",
        "決定を記録",
    ] {
        assert!(detail.iter().any(|l| l == b), "{b}: {detail:?}");
    }
    special(&mut s, &mut text, Key::Escape);
    assert!(s.tree.is_hidden(s.ids.detail));
}

#[test]
fn the_selection_is_kept_per_tab() {
    let (mut s, _, mut text) = demo_screen();
    key(&mut s, &mut text, 'j');
    key(&mut s, &mut text, '2');
    key(&mut s, &mut text, 'j');
    let second = s.selection();
    key(&mut s, &mut text, '1');
    assert_eq!(
        s.selection(),
        Some(Item::Asset("add-widget-runbook".into()))
    );
    key(&mut s, &mut text, '2');
    assert_eq!(s.selection(), second);
    key(&mut s, &mut text, 'r');
    assert_eq!(s.selection(), second, "a rescan keeps the selection");
}

#[test]
fn t_hands_the_selected_candidate_off() {
    let (mut s, log, mut text) = demo_screen();
    key(&mut s, &mut text, 't');
    assert_eq!(sent(&log), [], "t does nothing on the assets tab");
    key(&mut s, &mut text, '2');
    let Some(Item::Candidate(id)) = s.selection() else {
        panic!("a candidate is selected")
    };
    key(&mut s, &mut text, 't');
    assert_eq!(sent(&log), [Action::HandOff { improvement: id }]);
    assert!(has(&header_notice(&s), "記録しました"));
}

fn header_notice(s: &Scaffold) -> Vec<String> {
    labels_under(&s.tree, s.ids.notice)
}

#[test]
fn the_brief_shows_the_task_file() {
    let (mut s, _, mut text) = demo_screen();
    key(&mut s, &mut text, '2');
    special(&mut s, &mut text, Key::Enter);
    let detail = detail_labels(&s);
    assert!(has(&detail, "scaffold verify: 登録漏れ"));
    assert!(has(&detail, "widgets/scaffold/Cargo.toml:2"));
    assert!(detail.iter().any(|l| l == "タスクにする (T)"));
    click(&mut s, &mut text, "作業指示を表示");
    let detail = detail_labels(&s);
    assert!(has(&detail, "scaffold-asset: check-widget-registration"));
    assert!(has(&detail, "## 完了の条件"));
    assert!(detail.iter().any(|l| l == "作業指示を隠す"));
}

// ------------------------------------------------------------ forms

#[test]
fn l_opens_the_link_form_and_its_letters_go_to_the_field() {
    let (mut s, log, mut text) = demo_screen();
    key(&mut s, &mut text, 'l');
    assert!(s.form.is_some());
    let field = s.form.as_ref().unwrap().first_field().unwrap();
    assert_eq!(s.tree.focused(), Some(field));
    // The `l` that opened the form is not typed; `j` is typed, not moved.
    assert_eq!(s.tree.get::<TextField>(field).unwrap().text(), "");
    let before = s.selection();
    key(&mut s, &mut text, 'j');
    assert_eq!(s.selection(), before);
    assert_eq!(s.tree.get::<TextField>(field).unwrap().text(), "j");
    type_into(&mut s, &mut text, field, "ver-42");
    click(&mut s, &mut text, "関連付ける");
    assert_eq!(
        sent(&log),
        [Action::Link {
            subject: Subject::Asset("check-widget-registration".into()),
            kind: LinkKind::Verify,
            id: "ver-42".into(),
        }]
    );
    assert!(s.form.is_none(), "an accepted form closes");
}

#[test]
fn a_refused_action_keeps_the_form_and_shows_why() {
    let (mut s, log, mut text) = screen_on(demo::state(), Some("検証 ver-9 が見つかりません"));
    key(&mut s, &mut text, 'l');
    let field = s.form.as_ref().unwrap().first_field().unwrap();
    type_into(&mut s, &mut text, field, "ver-9");
    click(&mut s, &mut text, "関連付ける");
    assert_eq!(sent(&log).len(), 1);
    assert!(s.form.is_some());
    assert!(has(&header_notice(&s), "検証 ver-9 が見つかりません"));
    special(&mut s, &mut text, Key::Escape);
    assert!(s.form.is_none());
    assert!(
        !s.tree.is_hidden(s.ids.detail),
        "escape closes the form first"
    );
}

#[test]
fn skipping_needs_a_reason() {
    let (mut s, log, mut text) = demo_screen();
    special(&mut s, &mut text, Key::Enter);
    click(&mut s, &mut text, "省略にする");
    click(&mut s, &mut text, "省略を記録");
    assert_eq!(sent(&log), []);
    assert!(has(&header_notice(&s), "省略の理由を書いてください"));
    let field = s.form.as_ref().unwrap().first_field().unwrap();
    type_into(&mut s, &mut text, field, "今回は不要");
    click(&mut s, &mut text, "省略を記録");
    assert_eq!(
        sent(&log),
        [Action::Skip {
            subject: Subject::Asset("check-widget-registration".into()),
            reason: "今回は不要".into(),
        }]
    );
}

#[test]
fn the_decision_form_offers_the_allowed_stages_and_the_links() {
    let (mut s, log, mut text) = demo_screen();
    special(&mut s, &mut text, Key::Enter);
    click(&mut s, &mut text, "決定を記録");
    let form = labels_under(&s.tree, s.ids.form_host);
    // check-widget-registration is 評価中: 採用, 試作, 廃止.
    assert!(has(&form, "決定を記録"));
    let checks: Vec<WidgetId> = checkboxes(&s.tree, s.ids.form_host);
    assert_eq!(checks.len(), 1);
    let ids = fields(&s.tree, s.ids.form_host);
    type_into(&mut s, &mut text, ids[0], "効果を確認した");
    type_into(&mut s, &mut text, ids[1], "docs/runbooks/add-widget.md");
    s.tree.set_focus(Some(checks[0]));
    s.tree.dispatch(
        Event::KeyDown {
            key: Key::Space,
            modifiers: Modifiers::default(),
        },
        &mut text,
    );
    let _ = s.tree.take_messages();
    click(&mut s, &mut text, "記録する");
    assert_eq!(
        sent(&log),
        [Action::Decide {
            asset: "check-widget-registration".into(),
            to: Lifecycle::Adopted,
            reason: "効果を確認した".into(),
            evidence: vec![
                EvidenceRef {
                    kind: EvidenceKind::Verify,
                    r#ref: "ver-1795500000000".into(),
                },
                EvidenceRef {
                    kind: EvidenceKind::Path,
                    r#ref: "docs/runbooks/add-widget.md".into(),
                },
            ],
        }]
    );
}

#[test]
fn an_absolute_evidence_path_is_refused_in_the_form() {
    let (mut s, log, mut text) = demo_screen();
    special(&mut s, &mut text, Key::Enter);
    click(&mut s, &mut text, "決定を記録");
    let ids = fields(&s.tree, s.ids.form_host);
    type_into(&mut s, &mut text, ids[0], "r");
    type_into(&mut s, &mut text, ids[1], "/etc/passwd");
    click(&mut s, &mut text, "記録する");
    assert_eq!(sent(&log), []);
    assert!(has(&header_notice(&s), "相対パス"));
}

fn checkboxes(tree: &UiTree, id: WidgetId) -> Vec<WidgetId> {
    let own = tree.get::<Checkbox>(id).map(|_| id);
    own.into_iter()
        .chain(tree.children(id).iter().flat_map(|c| checkboxes(tree, *c)))
        .collect()
}

#[test]
fn the_demo_refuses_to_record() {
    let mut s = Scaffold::new(Box::new(demo::Demo));
    let mut text = TextStore::new(Default::default());
    s.prepare(window(), &mut text);
    assert!(has(&header(&s), "デモ表示 · 最終スキャン 2026-11-26 09:30"));
    key(&mut s, &mut text, '2');
    key(&mut s, &mut text, 't');
    assert!(has(&header_notice(&s), "デモ表示のため記録しません"));
}

// ------------------------------------------- selection across rescans

/// The first open candidate that is not handed off, as the list orders it.
fn open_candidate(s: &Scaffold) -> String {
    s.candidates()
        .into_iter()
        .find(|i| s.status_of(i) == ImprovementStatus::Open)
        .map(|i| i.id.clone())
        .expect("the demo has an open candidate")
}

/// A copy of candidate `id` that sorts before every other candidate.
fn sorting_first(state: &State, id: &str, new_id: &str) -> Improvement {
    let mut b = state
        .improvements
        .iter()
        .find(|i| i.id == id)
        .unwrap()
        .clone();
    b.id = new_id.into();
    b.widget = "!first".into();
    b
}

#[test]
fn an_open_detail_follows_its_candidate_across_a_rescan() {
    let (mut s, log, mut text, shared) = screen_shared(demo::state(), None);
    key(&mut s, &mut text, '2');
    // The first row, opened without moving: the selection is only the
    // fallback to the first row until it is pinned.
    let Some(Item::Candidate(a)) = s.selection() else {
        panic!("a candidate is selected")
    };
    assert_eq!(a, open_candidate(&s), "the first row is open");
    special(&mut s, &mut text, Key::Enter);
    assert!(has(&detail_labels(&s), &format!("id: {a}")));

    let b = sorting_first(&shared.borrow(), &a, "imp-new");
    shared.borrow_mut().improvements.push(b);
    key(&mut s, &mut text, 'r');
    assert_eq!(
        s.items(Tab::Candidates)[0],
        Item::Candidate("imp-new".into())
    );
    assert_eq!(s.selection(), Some(Item::Candidate(a.clone())));
    let detail = detail_labels(&s);
    assert!(detail.contains(&format!("id: {a}")), "{detail:?}");
    assert!(!detail.contains(&"id: imp-new".to_owned()), "{detail:?}");

    key(&mut s, &mut text, 't');
    assert_eq!(sent(&log), [Action::HandOff { improvement: a }]);
}

#[test]
fn the_30s_tick_keeps_the_selection_too() {
    let (mut s, _, mut text, shared) = screen_shared(demo::state(), None);
    key(&mut s, &mut text, '1');
    special(&mut s, &mut text, Key::Enter);
    let Some(Item::Asset(a)) = s.selection() else {
        panic!("an asset is selected")
    };
    {
        let mut st = shared.borrow_mut();
        let loaded = st.registry.as_mut().unwrap();
        let mut first = loaded.registry.assets[0].clone();
        first.id = "aaa-new".into();
        loaded.registry.assets.insert(0, first);
        st.scanned_at = demo::NOW + RESCAN_MS;
    }
    s.rescan(&mut text);
    assert_eq!(s.items(Tab::Assets)[0], Item::Asset("aaa-new".into()));
    assert_eq!(s.selection(), Some(Item::Asset(a.clone())));
    assert!(has(&detail_labels(&s), &format!("id: {a}")));
}

#[test]
fn a_vanished_item_closes_its_detail() {
    let (mut s, _, mut text, shared) = screen_shared(demo::state(), None);
    key(&mut s, &mut text, '2');
    key(&mut s, &mut text, 'j');
    special(&mut s, &mut text, Key::Enter);
    let Some(Item::Candidate(a)) = s.selection() else {
        panic!("a candidate is selected")
    };
    assert!(!s.tree.is_hidden(s.ids.detail));
    shared.borrow_mut().improvements.retain(|i| i.id != a);
    key(&mut s, &mut text, 'r');
    assert!(s.tree.is_hidden(s.ids.detail), "no neighbour is shown");
    assert_ne!(s.selection(), Some(Item::Candidate(a)));
}

// ---------------------------------------------- hand-off only once

#[test]
fn t_does_not_hand_off_a_handed_off_candidate_again() {
    let (mut s, log, mut text) = demo_screen();
    key(&mut s, &mut text, '2');
    let a = open_candidate(&s);
    key(&mut s, &mut text, 't');
    assert_eq!(
        sent(&log),
        [Action::HandOff {
            improvement: a.clone()
        }]
    );
    // Again, and as a held key repeats it.
    for _ in 0..3 {
        key(&mut s, &mut text, 't');
    }
    assert_eq!(sent(&log), [], "no second task");
    assert!(has(&header_notice(&s), "task-1"), "{:?}", header_notice(&s));
    // The completed hand-off offers its destination, not another create action.
    assert!(!detail_labels(&s).contains(&"タスクにする (T)".into()));
    assert!(has(&detail_labels(&s), "View → Tasks"));
}

// ------------------------------------------- repo root from a subdir

#[test]
fn a_subdirectory_opens_its_repository() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    std::fs::create_dir_all(root.join(".git")).unwrap();
    std::fs::write(root.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
    std::fs::create_dir_all(root.join("dev/scaffold")).unwrap();
    std::fs::write(root.join(REGISTRY), demo::registry().bytes).unwrap();
    let sub = root.join("widgets/alpha");
    std::fs::create_dir_all(&sub).unwrap();

    let state = Live::new(sub.clone(), None).scan();
    assert_eq!(state.repo, root);
    assert!(!state.unregistered(), "{:?}", state.registry);

    let mut s = Scaffold::new(Box::new(Live::new(sub, None)));
    let mut text = TextStore::new(Default::default());
    s.prepare(window(), &mut text);
    let shown = header(&s);
    assert!(shown.contains(&root.display().to_string()), "{shown:?}");
    assert!(!has(&list_labels(&s), "未登録"));
}

#[test]
fn outside_any_repository_the_given_directory_is_used() {
    let dir = tempfile::tempdir().unwrap();
    let plain = dir.path().canonicalize().unwrap().join("plain");
    std::fs::create_dir_all(&plain).unwrap();
    let state = Live::new(plain.clone(), None).scan();
    assert_eq!(state.repo, plain);
    assert!(state.unregistered());
}

// ----------------------------------------------- setup and discovery

fn generic_setup_state() -> State {
    use scaffold::scan::Evidence;
    use scaffold::setup::{Check, CheckState, Setup};
    let mut state = demo::state();
    let mut assets = state.registry.as_ref().unwrap().registry.assets.clone();
    for asset in &mut assets {
        asset.decisions.clear();
    }
    state.setup = Setup {
        stacks: vec!["Rust".into()],
        checks: vec![
            Check {
                id: "getting-started".into(),
                title: "開発手順".into(),
                description: "初回の開発手順を確認します".into(),
                state: CheckState::Found,
                evidence: vec![Evidence::new("README.md", 1)],
                next_step: "README.md に沿って開発環境を確認してください".into(),
            },
            Check {
                id: "verification".into(),
                title: "検証の入口".into(),
                description: "テストの実行手順を整えます".into(),
                state: CheckState::Missing,
                evidence: vec![],
                next_step: "テスト手順を README.md に追加してください".into(),
            },
            Check {
                id: "automation".into(),
                title: "継続的な検証".into(),
                description: "CI の設定を読み込めませんでした".into(),
                state: CheckState::Unknown,
                evidence: vec![Evidence::new(".github/workflows/ci.yml", 1)],
                next_step: "CI の設定を確認してください".into(),
            },
        ],
        assets,
        sources: vec![],
        registration_applicable: false,
    };
    state.registry = Err(LoadError::Absent);
    state.improvements.clear();
    state.observations.clear();
    state
}

fn search_for(s: &mut Scaffold, text: &mut TextStore, query: &str) {
    let id = s.ids.search;
    s.tree
        .update::<TextField, _>(id, |field, cx| field.set_text(cx, ""));
    s.tree.set_focus(Some(id));
    s.dispatch(Event::Text(query.into()), text);
    s.prepare(window(), text);
}

#[test]
fn a_new_window_starts_with_setup_and_explains_the_next_action() {
    let shared = Rc::new(RefCell::new(generic_setup_state()));
    let actions = Rc::new(RefCell::new(Vec::new()));
    let mut s = Scaffold::new(Box::new(Recorder {
        state: shared,
        sent: actions.clone(),
        refuse: None,
    }));
    let mut text = TextStore::new(Default::default());
    s.prepare(window(), &mut text);
    assert_eq!(s.tab, Tab::Setup);
    assert!(has(&list_labels(&s), "開発を始める準備を確認"));
    assert!(has(&list_labels(&s), "登録内容を確認"));
    assert!(has(&list_labels(&s), "不足を整える作業指示を確認"));
    assert!(has(&header(&s), "構成を検出 1/3"));
    assert!(!has(&header(&s), "整合 0/0"));
    assert!(
        sent(&actions).is_empty(),
        "opening the window writes nothing"
    );
}

#[test]
fn setup_details_explain_the_evidence_and_the_next_step() {
    let (mut s, _, mut text) = screen_on(generic_setup_state(), None);
    key(&mut s, &mut text, '0');
    special(&mut s, &mut text, Key::Enter);
    let detail = detail_labels(&s);
    assert!(has(&detail, "確認の根拠"));
    assert!(has(&detail, "README.md:1"));
    assert!(has(&detail, "次の手順"));
    assert!(has(&detail, "README.md に沿って"));
}

#[test]
fn registering_assets_requires_review_and_an_explicit_confirmation() {
    let (mut s, log, mut text) = screen_on(generic_setup_state(), None);
    key(&mut s, &mut text, '0');
    click(&mut s, &mut text, "登録内容を確認");
    assert!(sent(&log).is_empty());
    let labels = list_labels(&s);
    assert!(has(&labels, "書込先: dev/scaffold/registry.json"));
    assert!(has(&labels, "「候補」"));
    assert!(has(&labels, "未実施"));
    assert!(has(&labels, "scripts/check-widget-registration.py"));
    click(&mut s, &mut text, "この内容で登録");
    assert_eq!(sent(&log), [Action::RegisterAssets]);
    assert!(s.setup_preview.is_none());
}

#[test]
fn a_background_scan_cannot_change_the_registration_being_approved() {
    let (mut s, log, mut text, shared) = screen_shared(generic_setup_state(), None);
    key(&mut s, &mut text, '0');
    click(&mut s, &mut text, "登録内容を確認");
    shared.borrow_mut().setup.assets.pop();
    s.rescan(&mut text);
    assert!(has(&list_labels(&s), "検出内容が変わりました"));
    assert!(!list_labels(&s).contains(&"この内容で登録".into()));
    s.do_press(Press::RegisterAssets, &mut text);
    assert!(
        sent(&log).is_empty(),
        "a stale button message cannot approve new content"
    );
}

#[test]
fn setup_tasks_are_previewed_and_an_existing_task_is_not_recreated() {
    let (mut s, log, mut text) = screen_on(generic_setup_state(), None);
    key(&mut s, &mut text, '0');
    click(&mut s, &mut text, "不足を整える作業指示を確認");
    assert!(sent(&log).is_empty());
    assert!(has(&list_labels(&s), "scaffold-setup:"));
    click(&mut s, &mut text, "この内容でタスクを作成");
    assert_eq!(sent(&log), [Action::SetupTask]);
    assert!(has(&list_labels(&s), "task-1"));
    assert!(!list_labels(&s).contains(&"不足を整える作業指示を確認".into()));
    s.do_press(Press::SetupTask, &mut text);
    assert!(sent(&log).is_empty());
}

#[test]
fn a_changed_setup_plan_requires_a_new_preview() {
    let (mut s, log, mut text, shared) = screen_shared(generic_setup_state(), None);
    key(&mut s, &mut text, '0');
    click(&mut s, &mut text, "不足を整える作業指示を確認");
    shared.borrow_mut().setup.checks[1].state = scaffold::setup::CheckState::Found;
    s.rescan(&mut text);
    assert!(has(&list_labels(&s), "準備状況が変わりました"));
    assert!(!list_labels(&s).contains(&"この内容でタスクを作成".into()));
    s.do_press(Press::SetupTask, &mut text);
    assert!(sent(&log).is_empty());
}

#[test]
fn setup_does_not_offer_a_task_when_every_check_is_found() {
    let mut state = generic_setup_state();
    for check in &mut state.setup.checks {
        check.state = scaffold::setup::CheckState::Found;
    }
    let (mut s, _, mut text) = screen_on(state, None);
    key(&mut s, &mut text, '0');
    assert!(has(&list_labels(&s), "検出項目は揃っています"));
    assert!(!list_labels(&s).contains(&"不足を整える作業指示を確認".into()));
}

#[test]
fn a_broken_registry_offers_recovery_instead_of_registration() {
    let mut state = generic_setup_state();
    state.registry = Err(LoadError::Invalid("invalid JSON".into()));
    let (mut s, log, mut text) = screen_on(state, None);
    assert!(has(&list_labels(&s), "資産台帳を読み込めません"));
    assert!(!has(&list_labels(&s), "未登録リポジトリ"));
    key(&mut s, &mut text, '0');
    assert!(has(&list_labels(&s), "資産台帳を読み込めません"));
    assert!(!list_labels(&s).contains(&"登録内容を確認".into()));
    click(&mut s, &mut text, "情報源を確認");
    assert_eq!(s.tab, Tab::Sources);
    assert!(sent(&log).is_empty());
}

#[test]
fn searching_matches_titles_paths_and_multiple_case_insensitive_words() {
    let (mut s, _, mut text) = screen_on(generic_setup_state(), None);
    key(&mut s, &mut text, '0');
    search_for(&mut s, &mut text, "開発 readme.MD");
    assert_eq!(s.items(Tab::Setup), [Item::Setup("getting-started".into())]);
    special(&mut s, &mut text, Key::Escape);
    assert_eq!(s.items(Tab::Setup).len(), 3);
    assert_eq!(s.tree.focused(), Some(s.ids.search));
    special(&mut s, &mut text, Key::Escape);
    assert_eq!(s.tree.focused(), None);
}

#[test]
fn typing_shortcut_letters_in_search_never_changes_tabs_or_creates_work() {
    let (mut s, log, mut text) = demo_screen();
    key(&mut s, &mut text, '2');
    key(&mut s, &mut text, '/');
    for c in ['t', 'r', '0', '1', 'j', 'l'] {
        key(&mut s, &mut text, c);
    }
    assert_eq!(s.tab, Tab::Candidates);
    assert_eq!(s.query, "tr01jl");
    assert_eq!(
        s.tree.get::<TextField>(s.ids.search).unwrap().text(),
        "tr01jl"
    );
    assert!(sent(&log).is_empty());
    assert!(s.form.is_none());
    assert!(has(&list_labels(&s), "一致する項目がありません"));
    click(&mut s, &mut text, "検索・絞り込みを解除");
    assert_eq!(s.items(Tab::Candidates).len(), 3);
    assert!(s.query.is_empty());
}

#[test]
fn the_attention_filter_keeps_actionable_checks_candidates_and_sources() {
    let (mut s, _, mut text) = screen_on(generic_setup_state(), None);
    key(&mut s, &mut text, '0');
    s.attention_only = true;
    s.filters_changed(&mut text);
    assert_eq!(
        s.items(Tab::Setup),
        [
            Item::Setup("verification".into()),
            Item::Setup("automation".into())
        ]
    );

    let (mut s, _, mut text) = demo_screen();
    key(&mut s, &mut text, '2');
    s.attention_only = true;
    s.filters_changed(&mut text);
    assert_eq!(s.items(Tab::Candidates).len(), 1);
    assert_eq!(
        s.items(Tab::Candidates)[0],
        Item::Candidate(open_candidate(&s))
    );
    s.state.report.sources[0].state = SourceState::Unknown;
    s.set_tab(Tab::Sources, &mut text);
    assert_eq!(s.items(Tab::Sources).len(), 1);
}

#[test]
fn hiding_a_selected_item_closes_its_detail_and_rescans_keep_filtered_selection() {
    let (mut s, _, mut text, shared) = screen_shared(demo::state(), None);
    special(&mut s, &mut text, Key::Enter);
    let selected = s.selection();
    assert!(!s.tree.is_hidden(s.ids.detail));
    search_for(&mut s, &mut text, "add-widget");
    assert!(s.tree.is_hidden(s.ids.detail));
    assert_ne!(s.selection(), selected);
    let filtered = s.selection();
    shared
        .borrow_mut()
        .registry
        .as_mut()
        .unwrap()
        .registry
        .assets
        .reverse();
    s.rescan(&mut text);
    assert_eq!(s.selection(), filtered);
    assert_eq!(s.query, "add-widget");
}

#[test]
fn a_resolved_candidate_has_no_create_action_even_for_the_keyboard_shortcut() {
    let (mut s, log, mut text) = demo_screen();
    key(&mut s, &mut text, '2');
    let item = s.items(Tab::Candidates).last().cloned().unwrap();
    s.select(item, &mut text);
    special(&mut s, &mut text, Key::Enter);
    assert!(!detail_labels(&s).contains(&"タスクにする (T)".into()));
    key(&mut s, &mut text, 't');
    assert!(sent(&log).is_empty());
    assert!(has(&header_notice(&s), "解消済み"));
}

#[test]
fn the_toolbar_and_detail_fit_the_minimum_window() {
    let (mut s, _, mut text) = screen_on(generic_setup_state(), None);
    key_small(&mut s, &mut text, Key::Char('0'));
    for id in [
        s.ids.repo,
        s.ids.commit,
        s.ids.score,
        s.ids.scanned,
        s.ids.rescan,
        s.ids.tabs,
        s.ids.search,
        s.ids.filter,
        s.ids.count,
    ] {
        let rect = s.tree.window_rect(id).unwrap();
        assert!(
            rect.x >= 0.0 && rect.x + rect.width <= 960.5,
            "toolbar overflows: {rect:?}"
        );
        assert!(
            rect.y >= 0.0 && rect.y + rect.height <= 600.5,
            "toolbar overflows: {rect:?}"
        );
    }
    key_small(&mut s, &mut text, Key::Enter);
    let detail = s.tree.window_rect(s.ids.detail).unwrap();
    assert!(
        detail.x >= 0.0 && detail.x + detail.width <= 960.5,
        "detail overflows: {detail:?}"
    );
    let list = s.tree.window_rect(s.ids.list).unwrap();
    assert!(list.width >= 350.0, "the list is too narrow: {list:?}");
    assert!(selected_row_shown(&s));
}

#[test]
fn list_rows_announce_their_human_title_and_state() {
    let (mut s, _, mut text) = screen_on(generic_setup_state(), None);
    key(&mut s, &mut text, '0');
    let label = s.tree.get::<Pressable>(s.rows[0].0).unwrap().label_text();
    assert!(label.contains("開発手順"));
    assert!(label.contains("見つかりました"));
    click(&mut s, &mut text, "登録内容を確認");
    special(&mut s, &mut text, Key::Enter);
    assert!(
        s.tree.is_hidden(s.ids.detail),
        "a preview does not open an invisible checklist row"
    );
}
