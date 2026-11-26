use super::*;

use std::cell::RefCell;
use std::rc::Rc;

use noa_gpui_core::widgets::{
    Button, Image, PopUpButton, ScrollView, TextArea, TextField, TextView,
};
use verify::model::reduce::Event;
use verify::model::types::{
    AgentChoice, BuiltinStrategy, CriteriaEdit, MIN_PANEL, Priority, Strategy, StrategyChoice,
    TargetKind, VerifierChoice,
};
use verify::store::Store;

use crate::installed::Installed;
use crate::live::{FakeLink, OpenPanes};

thread_local! {
    /// Run once right after a stored view read its log, before it goes on
    /// (a test's stand-in for another window writing at that moment).
    static AFTER_STORED_READ: RefCell<Option<Box<dyn FnOnce()>>> = RefCell::new(None);
}

/// Called by `show_stored` after its read (tests only).
pub(super) fn after_stored_read() {
    if let Some(hook) = AFTER_STORED_READ.with(|h| h.borrow_mut().take()) {
        hook();
    }
}

fn acts(screen: &Verify) -> Vec<Action> {
    screen.actions.iter().map(|(_, a)| a.clone()).collect()
}

fn widget_for(screen: &Verify, action: &Action) -> Option<WidgetId> {
    screen
        .actions
        .iter()
        .find(|(_, a)| a == action)
        .map(|(id, _)| *id)
}

fn act(screen: &mut Verify, action: Action) {
    let mut text = TextStore::new(Default::default());
    screen.handle_action(action, &mut text);
}

fn temp(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("noa-verify-ui-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// `log` written as the stored log `root/<name>`.
fn stored_log(root: &Path, name: &str, log: &[Vec<Event>]) -> PathBuf {
    let dir = root.join(name);
    let _ = std::fs::remove_dir_all(&dir);
    let mut store = Store::create(&dir, &log[0]).unwrap();
    store.append_all(&log[1..]).unwrap();
    dir
}

/// The finished demo verification written as a stored log under `root`.
fn stored_demo(root: &Path) -> PathBuf {
    stored_log(root, crate::demo::DONE_ID, &crate::demo::log())
}

/// The held demo (interrupted after round 1) stored under `root`.
fn stored_unfinished(root: &Path) -> PathBuf {
    stored_log(root, crate::demo::HELD_ID, &crate::demo::held_log())
}

/// A report that every kind is installed, already delivered.
fn installed_all() -> Receiver<Installed> {
    let (tx, rx) = std::sync::mpsc::channel();
    tx.send(Installed::Found(AgentKind::ALL.to_vec())).unwrap();
    rx
}

/// A live screen whose check already reported every kind installed.
fn live_ready(channel: Channel) -> Verify {
    let mut screen = Verify::live(channel, installed_all());
    screen.poll_live(&mut TextStore::new(Default::default()));
    screen
}

/// The same, showing the verification stored in `dir`.
fn stored_ready(channel: Channel, dir: &Path) -> Verify {
    let mut screen = Verify::live_stored(channel, installed_all(), dir);
    screen.poll_live(&mut TextStore::new(Default::default()));
    screen
}

fn snapshot(session: Session, dirs: Option<(PathBuf, PathBuf)>, generation: u64) -> Snapshot {
    let (reply_root, session_dir) = match dirs {
        Some((reply, dir)) => (Some(reply), Some(dir)),
        None => (None, None),
    };
    Snapshot {
        link: Link::Live,
        session: Some(Arc::new(session)),
        reply_root,
        session_dir,
        generation,
    }
}

/// Feeds `session` to a live screen as its runner would.
fn live_with(session: Session) -> (Verify, FakeLink) {
    let (channel, link) = Channel::fake();
    let mut screen = live_ready(channel);
    link.snapshots.send(snapshot(session, None, 0)).unwrap();
    screen.poll_live(&mut TextStore::new(Default::default()));
    (screen, link)
}

/// The same, with the store and reply dirs a runner's snapshot carries.
fn live_with_dirs(session: Session, reply: &Path) -> (Verify, FakeLink) {
    let (channel, link) = Channel::fake();
    let mut screen = live_ready(channel);
    link.snapshots
        .send(snapshot(
            session,
            Some((
                reply.to_owned(),
                PathBuf::from("/data/noa/verifications/ver-demo"),
            )),
            0,
        ))
        .unwrap();
    screen.poll_live(&mut TextStore::new(Default::default()));
    (screen, link)
}

const REPLY: &str = "/w/.noa/verify/ver-demo";

fn commands(link: &FakeLink) -> Vec<Command> {
    link.commands.try_iter().map(|(_, c)| c).collect()
}

/// Every label's and tag's text in `root`'s subtree, in tree order.
fn labels_under(tree: &UiTree, root: WidgetId) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack = vec![root];
    while let Some(id) = stack.pop() {
        if tree.is_hidden(id) {
            continue;
        }
        if let Some(label) = tree.get::<Label>(id) {
            out.push(label.text().to_owned());
        } else if let Some(tag) = tree.get::<Tag>(id) {
            out.push(tag.text().to_owned());
        } else if let Some(button) = tree.get::<Button>(id) {
            out.push(button.label().to_owned());
        }
        stack.extend(tree.children(id).iter().rev());
    }
    out
}

/// Every label of the right pane, joined.
fn all_text(screen: &Verify) -> String {
    labels_under(&screen.tree, screen.right_host).join("\n")
}

fn toasts(screen: &Verify) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack: Vec<WidgetId> = screen.tree.overlays().collect();
    while let Some(id) = stack.pop() {
        if let Some(toast) = screen.tree.get::<Toast>(id) {
            out.push(toast.message().to_owned());
        }
        stack.extend(screen.tree.children(id).iter().rev());
    }
    out
}

fn press(screen: &mut Verify, key: noa_gpui_core::Key) {
    let mut text = TextStore::new(Default::default());
    Screen::dispatch(
        screen,
        UiEvent::KeyDown {
            key,
            modifiers: Default::default(),
        },
        &mut text,
    );
}

/// Presses the visible button carrying `action` from the keyboard (focus,
/// then Space), as a person would.
fn press_button(screen: &mut Verify, action: &Action) {
    let id = widget_for(screen, action).unwrap_or_else(|| panic!("no button for {action:?}"));
    assert!(!screen.tree.is_hidden(id), "{action:?} is hidden");
    screen.tree.set_focus(Some(id));
    assert_eq!(
        screen.tree.focused(),
        Some(id),
        "{action:?} cannot take focus"
    );
    press(screen, noa_gpui_core::Key::Space);
}

/// The smallest window the app allows, in logical pixels.
const SMALLEST: (f32, f32) = (MIN_WINDOW.0 as f32, MIN_WINDOW.1 as f32);

/// Lays `screen` out in a window of `size`, as `--snapshot` does.
fn lay_out(screen: &mut Verify, size: (f32, f32)) {
    let mut text = TextStore::new(Default::default());
    let viewport = noa_gpui_scene::Size::new(size.0, size.1);
    Screen::prepare(screen, viewport, &mut text);
}

/// What under `host` reaches past its right edge: cut off where the pane
/// ends.
fn cut_off(screen: &Verify, host: WidgetId) -> Vec<WidgetId> {
    let edge = screen.tree.window_rect(host).unwrap().right();
    let mut out = Vec::new();
    let mut stack = screen.tree.children(host).to_vec();
    while let Some(id) = stack.pop() {
        if screen
            .tree
            .window_rect(id)
            .is_some_and(|r| r.right() > edge + 0.5)
        {
            out.push(id);
        }
        stack.extend_from_slice(screen.tree.children(id));
    }
    out
}

/// A recorder in place of `open`: what the screen asked to open.
fn record_opens(screen: &mut Verify) -> Rc<RefCell<Vec<Open>>> {
    let seen = Rc::new(RefCell::new(Vec::new()));
    let into = Rc::clone(&seen);
    screen.opener = Box::new(move |open| {
        into.borrow_mut().push(open.clone());
        Ok(())
    });
    seen
}

/// The finished demo as if it had been verified in `workdir`, with its
/// stored copies written there (each holding its own path), so 開く has
/// real files to open. Returns the session and its reply dir.
fn demo_with_files(workdir: &Path) -> (Session, PathBuf) {
    let mut session = crate::demo::session();
    session.workdir = workdir.to_owned();
    session.main_worktree = workdir.to_owned();
    let reply = store::reply_dir(&session);
    let write = |stored: &mut Stored, body: String| {
        let at = reply.join(&stored.path);
        std::fs::create_dir_all(at.parent().unwrap()).unwrap();
        std::fs::write(&at, &body).unwrap();
        // The record a real ingest makes of what it stored.
        stored.bytes = body.len() as u64;
        stored.digest = verify::capture::digest_bytes(body.as_bytes());
    };
    for item in &mut session.evidence {
        let key = item.key.clone();
        if let Some(stored) = &mut item.stored {
            write(stored, format!("stored copy of {key}\nexit 101\n"));
        }
        let log = match &mut item.rerun {
            Some(
                Rerun::Matched { log, .. }
                | Rerun::Differed { log, .. }
                | Rerun::TimedOut { log }
                | Rerun::NotRun { log, .. },
            ) => log.as_mut(),
            None => None,
        };
        if let Some(stored) = log {
            write(stored, format!("re-run of {key}\n"));
        }
    }
    (session, reply)
}

/// Every file under `dir` with its bytes and modification time.
fn files_with_times(dir: &Path) -> Vec<(PathBuf, Vec<u8>, std::time::SystemTime)> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_owned()];
    while let Some(at) = stack.pop() {
        for entry in std::fs::read_dir(&at).unwrap().filter_map(Result::ok) {
            let meta = entry.metadata().unwrap();
            if meta.is_dir() {
                stack.push(entry.path());
            } else {
                let bytes = std::fs::read(entry.path()).unwrap();
                out.push((entry.path(), bytes, meta.modified().unwrap()));
            }
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

fn key(text: &str) -> EvidenceKey {
    EvidenceKey::new(text)
}

// ---- the start view ----

#[test]
fn the_start_view_offers_the_target_criteria_panel_isolation_and_past_verifications() {
    let screen = Verify::demo_start();
    assert!(!screen.tree.is_hidden(screen.start.root));
    assert!(screen.tree.is_hidden(screen.board));
    let a = acts(&screen);
    for action in [Action::Start, Action::AddPanelRow, Action::PickSpec] {
        assert!(a.contains(&action), "{action:?}");
    }
    assert!(a.iter().any(|a| matches!(a, Action::ResumePast(_))));
    assert!(a.iter().any(|a| matches!(a, Action::OpenPast(_))));
    let form = start::read(&screen.tree, &screen.start);
    assert_eq!(form.kind, TargetKind::Diff);
    assert_eq!(form.panel.len(), 3);
    assert_eq!(
        form.panel[1].strategy,
        StrategyChoice::Builtin(BuiltinStrategy::Falsify)
    );
    assert_eq!(form.judge.setup.model.as_deref(), Some("opus"));
    assert_eq!(form.spec, "docs/specs/cwd-inherit.md");
    assert!(form.confirm, "基準を確認してから検証 is on by default");
    assert_eq!(form.max_rounds, 2);
    assert_eq!(form.isolation, verify::model::types::Isolation::Worktree);
    // The past list: verdict tag and the criteria tally.
    let text = labels_under(&screen.tree, screen.start.root).join("\n");
    assert!(text.contains("過去の検証"), "{text}");
    assert!(
        text.contains("検証済 2 · 不合格 1 · 判定不能 1 · 対象外 1"),
        "{text}"
    );
    assert!(text.contains("不合格"), "{text}");
}

#[test]
fn panel_rows_stop_at_six_and_at_two() {
    let mut screen = Verify::demo_start();
    for _ in 0..5 {
        act(&mut screen, Action::AddPanelRow);
    }
    assert_eq!(screen.start.panel.len(), 6);
    assert!(!screen.tree.is_enabled(screen.start.add_row));
    let form = start::read(&screen.tree, &screen.start);
    // New rows take the strategies nobody holds yet.
    let strategies: Vec<StrategyChoice> =
        form.panel[3..].iter().map(|r| r.strategy.clone()).collect();
    assert!(strategies.contains(&StrategyChoice::Builtin(BuiltinStrategy::E2e)));
    for _ in 0..6 {
        act(&mut screen, Action::RemovePanelRow(0));
    }
    assert_eq!(screen.start.panel.len(), MIN_PANEL);
}

#[test]
fn a_state_target_hides_the_base_and_a_custom_strategy_shows_its_text() {
    let mut screen = Verify::demo_start();
    let mut text = TextStore::new(Default::default());
    let form = start::Form {
        kind: TargetKind::State,
        panel: vec![
            VerifierChoice {
                agent: AgentKind::Claude.into(),
                strategy: StrategyChoice::Custom("全ロケールで確かめる".into()),
            },
            VerifierChoice {
                agent: AgentKind::Codex.into(),
                strategy: StrategyChoice::Builtin(BuiltinStrategy::Nonfunctional),
            },
        ],
        ..start::Form::default()
    };
    start::fill(&mut screen.tree, &mut screen.start, &form, &mut text);
    let read = start::read(&screen.tree, &screen.start);
    assert_eq!(read.kind, TargetKind::State);
    assert_eq!(read.panel, form.panel);
    assert!(!screen.tree.is_hidden(screen.start.panel[0].custom));
    assert!(screen.tree.is_hidden(screen.start.panel[1].custom));
}

#[test]
fn a_refused_start_shows_why_under_the_form() {
    let (channel, link) = Channel::fake();
    let mut screen = live_ready(channel);
    // The repository is empty: refused before anything is sent.
    act(&mut screen, Action::Start);
    assert!(commands(&link).is_empty());
    assert!(!screen.starting);
    let text = labels_under(&screen.tree, screen.start.root).join("\n");
    assert!(text.contains("開始できません"), "{text}");
    let repo = temp("refused");
    let mut text = TextStore::new(Default::default());
    let form = start::Form {
        repo: repo.display().to_string(),
        criteria: "新しいタブは cwd を引き継ぐ\n\n分割も同じ\n".into(),
        ..start::Form::default()
    };
    start::fill(&mut screen.tree, &mut screen.start, &form, &mut text);
    act(&mut screen, Action::Start);
    let sent = commands(&link);
    let [
        Command::Start {
            user_criteria,
            confirm,
            max_rounds,
            ..
        },
    ] = sent.as_slice()
    else {
        panic!("{sent:?}");
    };
    assert_eq!(
        user_criteria,
        &["新しいタブは cwd を引き継ぐ", "分割も同じ"]
    );
    assert!(*confirm);
    assert_eq!(*max_rounds, 2);
    assert!(screen.starting);
    link.notices
        .send((0, "検証は git の作業ツリーでのみ行えます".into()))
        .unwrap();
    link.released.send(()).unwrap();
    screen.poll_live(&mut text);
    assert!(!screen.starting);
    assert!(!screen.tree.is_hidden(screen.start.root));
    let shown = labels_under(&screen.tree, screen.start.root).join("\n");
    assert!(
        shown.contains("検証は git の作業ツリーでのみ行えます"),
        "{shown}"
    );
    let _ = std::fs::remove_dir_all(repo);
}

/// A link-log notice after a successful open (L1-4: it never fails the
/// session) is not a start refusal, whichever channel is read first.
#[test]
fn a_link_log_notice_after_an_open_is_no_start_refusal() {
    const NOTICE: &str = "タスクとの関連を記録できません: links.jsonl.lock: Permission denied";
    for notice_first in [false, true] {
        let (channel, link) = Channel::fake();
        let mut screen = live_ready(channel);
        let repo = temp(&format!("link-notice-{notice_first}"));
        let mut text = TextStore::new(Default::default());
        let form = start::Form {
            repo: repo.display().to_string(),
            criteria: "新しいタブは cwd を引き継ぐ\n".into(),
            ..start::Form::default()
        };
        start::fill(&mut screen.tree, &mut screen.start, &form, &mut text);
        act(&mut screen, Action::Start);
        assert!(screen.starting, "notice_first={notice_first}");
        let session = snapshot(crate::demo::session(), None, 0);
        if notice_first {
            link.notices.send((0, NOTICE.into())).unwrap();
            link.snapshots.send(session).unwrap();
        } else {
            link.snapshots.send(session).unwrap();
            link.notices.send((0, NOTICE.into())).unwrap();
        }
        screen.poll_live(&mut text);
        assert!(!screen.starting, "notice_first={notice_first}");
        assert!(screen.session.is_some(), "notice_first={notice_first}");
        assert_eq!(screen.refusal, None, "notice_first={notice_first}");
        assert!(!screen.tree.is_hidden(screen.board));
        assert!(screen.tree.is_hidden(screen.start.root));
        let shown = labels_under(&screen.tree, screen.start.root).join("\n");
        assert!(!shown.contains("開始できません"), "{shown}");
        assert!(!shown.contains(NOTICE), "{shown}");
        assert!(
            toasts(&screen).iter().any(|t| t == NOTICE),
            "notice_first={notice_first}"
        );
        let _ = std::fs::remove_dir_all(repo);
    }
}

#[test]
fn the_spec_file_can_be_chosen_in_an_open_panel() {
    let mut screen = Verify::demo_start();
    act(&mut screen, Action::PickSpec);
    let asked = Screen::take_file_dialogs(&mut screen);
    assert_eq!(asked.len(), 1);
    assert!(!asked[0].save);
    let mut text = TextStore::new(Default::default());
    Screen::file_dialog_finished(
        &mut screen,
        FileDialogResult::Opened(vec![PathBuf::from("/repo/docs/spec.md")]),
        &mut text,
    );
    assert_eq!(
        start::read(&screen.tree, &screen.start).spec,
        "/repo/docs/spec.md"
    );
    act(&mut screen, Action::PickSpec);
    let _ = Screen::take_file_dialogs(&mut screen);
    Screen::file_dialog_finished(&mut screen, FileDialogResult::Cancelled, &mut text);
    assert_eq!(
        start::read(&screen.tree, &screen.start).spec,
        "/repo/docs/spec.md"
    );
}

// ---- installed agents (as review's) ----

fn live_pending() -> (
    Verify,
    FakeLink,
    std::sync::mpsc::Sender<Installed>,
    TextStore,
) {
    let (channel, link) = Channel::fake();
    let (tx, rx) = std::sync::mpsc::channel();
    let screen = Verify::live(channel, rx);
    (screen, link, tx, TextStore::new(Default::default()))
}

fn report(
    screen: &mut Verify,
    tx: &std::sync::mpsc::Sender<Installed>,
    installed: Installed,
    text: &mut TextStore,
) {
    tx.send(installed).unwrap();
    screen.poll_live(text);
}

fn found(kinds: &[AgentKind]) -> Installed {
    Installed::Found(kinds.to_vec())
}

fn kinds_of(panel: &[VerifierChoice]) -> Vec<AgentKind> {
    panel.iter().map(|row| row.agent.kind).collect()
}

/// How many entries `popup` has, by walking down its menu with the
/// keyboard to the end.
fn menu_len(screen: &mut Verify, popup: WidgetId, text: &mut TextStore) -> usize {
    screen.tree.set_focus(Some(popup));
    for _ in 0..AgentKind::ALL.len() + 2 {
        Screen::dispatch(
            screen,
            UiEvent::KeyDown {
                key: noa_gpui_core::Key::ArrowDown,
                modifiers: Default::default(),
            },
            text,
        );
    }
    screen.tree.get::<PopUpButton>(popup).unwrap().selected() + 1
}

fn press_down(screen: &mut Verify, popup: WidgetId, text: &mut TextStore) {
    screen.tree.set_focus(Some(popup));
    Screen::dispatch(
        screen,
        UiEvent::KeyDown {
            key: noa_gpui_core::Key::ArrowDown,
            modifiers: Default::default(),
        },
        text,
    );
}

/// A form that passes the start checks (an existing folder).
fn startable_form(repo: &Path) -> start::Form {
    start::Form {
        repo: repo.display().to_string(),
        ..start::Form::default()
    }
}

#[test]
fn the_start_button_waits_for_the_installed_check() {
    let repo = temp("waits");
    let (mut screen, link, tx, mut text) = live_pending();
    let start_button = screen.start.start_button;
    assert!(!screen.tree.is_enabled(start_button));
    start::fill(
        &mut screen.tree,
        &mut screen.start,
        &startable_form(&repo),
        &mut text,
    );
    act(&mut screen, Action::Start);
    assert!(commands(&link).is_empty());
    assert!(!screen.starting);
    report(&mut screen, &tx, found(&[AgentKind::Codex]), &mut text);
    assert!(screen.tree.is_enabled(start_button));
    start::fill(
        &mut screen.tree,
        &mut screen.start,
        &startable_form(&repo),
        &mut text,
    );
    act(&mut screen, Action::Start);
    assert!(matches!(&commands(&link)[..], [Command::Start { .. }]));
    let _ = std::fs::remove_dir_all(repo);
}

#[test]
fn the_start_button_stays_off_when_no_agent_is_installed() {
    let (mut screen, link, tx, mut text) = live_pending();
    report(&mut screen, &tx, found(&[]), &mut text);
    assert!(!screen.tree.is_enabled(screen.start.start_button));
    assert_eq!(
        screen.detection.blocked_reason(),
        "claude / codex / agy のいずれもログインシェルの PATH に見つかりません"
    );
    act(&mut screen, Action::Start);
    assert!(commands(&link).is_empty());
}

#[test]
fn an_unknown_check_offers_every_kind_and_allows_starting() {
    let (mut screen, _link, tx, mut text) = live_pending();
    report(
        &mut screen,
        &tx,
        Installed::Unknown("zsh".into()),
        &mut text,
    );
    assert!(screen.tree.is_enabled(screen.start.start_button));
    assert_eq!(screen.start.kinds(), AgentKind::ALL);
    let (judge, row) = (screen.start.judge, screen.start.panel[0].kind);
    assert_eq!(
        menu_len(&mut screen, judge, &mut text),
        AgentKind::ALL.len()
    );
    assert_eq!(menu_len(&mut screen, row, &mut text), AgentKind::ALL.len());
}

#[test]
fn menus_offer_only_the_installed_kinds_and_map_by_position() {
    let repo = temp("menus");
    let (mut screen, link, tx, mut text) = live_pending();
    report(
        &mut screen,
        &tx,
        found(&[AgentKind::Codex, AgentKind::Agy]),
        &mut text,
    );
    let form = start::read(&screen.tree, &screen.start);
    assert_eq!(form.judge.kind, AgentKind::Codex);
    assert_eq!(kinds_of(&form.panel), [AgentKind::Codex, AgentKind::Agy]);
    let (judge, row) = (screen.start.judge, screen.start.panel[1].kind);
    assert_eq!(menu_len(&mut screen, judge, &mut text), 2);
    assert_eq!(menu_len(&mut screen, row, &mut text), 2);
    let form = start::read(&screen.tree, &screen.start);
    assert_eq!(form.judge.kind, AgentKind::Agy);
    assert_eq!(form.panel[1].agent.kind, AgentKind::Agy);
    act(&mut screen, Action::AddPanelRow);
    let form = start::read(&screen.tree, &screen.start);
    assert_eq!(form.panel[2].agent.kind, AgentKind::Agy);
    let mut started = startable_form(&repo);
    started.judge = AgentKind::Codex.into();
    started.panel = form.panel.clone();
    start::fill(&mut screen.tree, &mut screen.start, &started, &mut text);
    act(&mut screen, Action::Start);
    let Some(Command::Start { judge, panel, .. }) = commands(&link).into_iter().next() else {
        panic!("not started");
    };
    assert_eq!(judge.kind, AgentKind::Codex);
    assert!(
        kinds_of(&panel)
            .iter()
            .all(|k| [AgentKind::Codex, AgentKind::Agy].contains(k))
    );
    let _ = std::fs::remove_dir_all(repo);
}

#[test]
fn a_report_after_the_form_was_changed_keeps_what_is_left_and_says_what_went() {
    let (mut screen, _link, tx, mut text) = live_pending();
    let judge = screen.start.judge;
    press_down(&mut screen, judge, &mut text);
    assert_eq!(
        start::read(&screen.tree, &screen.start).judge.kind,
        AgentKind::Codex
    );
    report(
        &mut screen,
        &tx,
        found(&[AgentKind::Claude, AgentKind::Agy]),
        &mut text,
    );
    let form = start::read(&screen.tree, &screen.start);
    assert_eq!(form.judge.kind, AgentKind::Claude, "codex is not installed");
    assert!(kinds_of(&form.panel).iter().all(|k| *k != AgentKind::Codex));
    assert!(form.panel.len() >= MIN_PANEL);
    assert!(
        toasts(&screen)
            .iter()
            .any(|t| t.contains("インストールされていないため除外: codex"))
    );
}

#[test]
fn a_new_verification_starts_from_the_installed_kinds() {
    let (mut screen, _link, tx, mut text) = live_pending();
    report(&mut screen, &tx, found(&[AgentKind::Codex]), &mut text);
    act(&mut screen, Action::NewVerification);
    let form = start::read(&screen.tree, &screen.start);
    assert_eq!(form.judge.kind, AgentKind::Codex);
    assert_eq!(kinds_of(&form.panel), [AgentKind::Codex, AgentKind::Codex]);
}

#[test]
fn a_past_verification_whose_agent_is_gone_says_so_instead_of_resuming() {
    let root = temp("gone");
    let dir = stored_unfinished(&root);
    let session = store::read_session(&dir).unwrap();
    let (mut screen, link, tx, mut text) = live_pending();
    act(&mut screen, Action::ResumePast(dir.clone()));
    assert!(commands(&link).is_empty(), "still checking");
    report(&mut screen, &tx, found(&[AgentKind::Claude]), &mut text);
    act(&mut screen, Action::ResumePast(dir.clone()));
    assert!(commands(&link).is_empty());
    assert!(
        screen
            .detection
            .resume_blocker(&session)
            .is_some_and(|why| why.contains("codex"))
    );
    screen.detection = start::Detection::all_installed();
    act(&mut screen, Action::ResumePast(dir.clone()));
    assert_eq!(commands(&link), [Command::Resume { dir }]);
    let _ = std::fs::remove_dir_all(root);
}

// ---- the board ----

#[test]
fn ac22_the_matrix_shows_the_latest_result_per_cell_with_rerun_and_cross_check_marks() {
    let mut screen = Verify::demo();
    assert_eq!(screen.tab, CenterTab::Criteria);
    let shown = labels_under(&screen.tree, screen.center_host);
    // claude / codex / claude-2 on AC-1, then the ruling.
    let at = shown.iter().position(|l| l == "AC-1").unwrap();
    let row: Vec<&String> = shown[at..].iter().take(7).collect();
    assert_eq!(
        row,
        [
            "AC-1",
            "必須",
            "新しいタブはフォーカス中のペインの作業ディレクトリで開く",
            "✗ ↻",
            "✗ ↻ ⇄",
            "✓ ≠ ⇄",
            "不合格"
        ],
        "{shown:#?}"
    );
    // AC-5: claude's latest is its round-2 update, not its R1 not_tested.
    let at = shown.iter().position(|l| l == "AC-5").unwrap();
    assert_eq!(shown[at + 3], "✓ ↻");
    assert_eq!(shown[at + 4], "✓ ↻ ⇄");
    assert_eq!(shown[at + 5], "⊘");
    // A click on a row selects its criterion; the thread follows.
    let row = widget_for(&screen, &Action::SelectCriterion(Some("AC-1".into()))).unwrap();
    assert!(
        screen
            .tree
            .get::<noa_gpui_core::widgets::Pressable>(row)
            .is_some()
    );
    act(&mut screen, Action::SelectCriterion(Some("AC-1".into())));
    assert_eq!(screen.selected, Some(Selected::Criterion("AC-1".into())));
    let thread = all_text(&screen);
    assert!(thread.contains("基準 AC-1"), "{thread}");
    assert!(thread.contains("判定の履歴"), "{thread}");
    // Pressed again, the selected row lets it go.
    assert!(acts(&screen).contains(&Action::SelectCriterion(None)));
    act(&mut screen, Action::SelectCriterion(None));
    assert_eq!(screen.selected, None);
}

#[test]
fn ac22_a_cross_check_on_an_earlier_result_marks_not_the_latest_one() {
    use verify::model::types::{CriterionResult, CrossCheck, CrossOutcome, Outcome, Subject};
    let mut session = crate::demo::session();
    let (claude, codex) = (PartId::new("claude"), PartId::new("codex"));
    let result = |round, outcome| CriterionResult {
        round,
        part: claude.clone(),
        criterion: "AC-1".into(),
        outcome,
        note: String::new(),
        evidence: Vec::new(),
    };
    // claude failed AC-1 in R1 and passed it in R2; codex's R2 check is
    // about the R1 fail, the result its brief named.
    session.results = vec![result(1, Outcome::Fail), result(2, Outcome::Pass)];
    session.cross_checks = vec![CrossCheck {
        round: 2,
        by: codex,
        subject: Subject::Result {
            part: claude.clone(),
            criterion: "AC-1".into(),
        },
        outcome: CrossOutcome::Reproduced,
        note: String::new(),
        evidence: Vec::new(),
    }];
    assert_eq!(matrix::cell(&session, &claude, "AC-1").text(), "✓");
    // A check of R3 is about the R2 pass, the result the cell shows.
    session.cross_checks[0].round = 3;
    assert_eq!(matrix::cell(&session, &claude, "AC-1").text(), "✓ ⇄");
}

#[test]
fn the_matrix_and_the_criteria_tab_fit_the_smallest_window() {
    for mut screen in [Verify::demo(), Verify::demo_held(), Verify::demo_criteria()] {
        lay_out(&mut screen, SMALLEST);
        assert_eq!(cut_off(&screen, screen.center_host), vec![]);
        assert_eq!(cut_off(&screen, screen.progress_host), vec![]);
        assert_eq!(cut_off(&screen, screen.header_host), vec![]);
        assert_eq!(cut_off(&screen, screen.tabs_host), vec![]);
    }
}

fn shown_keys(screen: &Verify) -> Vec<String> {
    evidence::shown(screen.session.as_ref().unwrap(), &screen.evidence_filters)
        .into_iter()
        .map(|e| e.key.to_string())
        .collect()
}

#[test]
fn ac23_the_evidence_tab_filters_by_verifier_round_kind_criterion_and_rerun_outcome() {
    let mut screen = Verify::demo().with_view(Some(CenterTab::Evidence), None);
    let all = shown_keys(&screen);
    assert_eq!(all.len(), screen.session.as_ref().unwrap().evidence.len());
    let filter = |screen: &mut Verify, f: evidence::Filter| {
        act(screen, Action::FilterEvidence(f));
        shown_keys(screen)
    };
    // By verifier, and あなた.
    let claude = filter(&mut screen, evidence::Filter::Owner(Some("claude".into())));
    assert!(
        claude.iter().all(|k| k.starts_with("claude/")),
        "{claude:?}"
    );
    assert_eq!(claude.len(), 7);
    let mine = filter(&mut screen, evidence::Filter::Owner(Some("user".into())));
    assert_eq!(mine, ["user/1"]);
    act(
        &mut screen,
        Action::FilterEvidence(evidence::Filter::Owner(None)),
    );
    // By round: the person's screenshot belongs to the round it came in.
    let r2 = filter(&mut screen, evidence::Filter::Round(Some(2)));
    assert_eq!(
        r2,
        [
            "claude/r2/e1",
            "claude/r2/e2",
            "codex/r2/e1",
            "claude-2/r2/e1"
        ]
    );
    act(
        &mut screen,
        Action::FilterEvidence(evidence::Filter::Round(None)),
    );
    // By kind.
    let video = filter(
        &mut screen,
        evidence::Filter::Kind(Some(evidence::Kind::Video)),
    );
    assert_eq!(video, ["claude/r1/v1"]);
    let seen = filter(
        &mut screen,
        evidence::Filter::Kind(Some(evidence::Kind::Observation)),
    );
    assert_eq!(seen, ["codex/r1/o1"]);
    act(
        &mut screen,
        Action::FilterEvidence(evidence::Filter::Kind(None)),
    );
    // By re-run outcome.
    let differed = filter(
        &mut screen,
        evidence::Filter::Rerun(Some(evidence::RerunKind::Differed)),
    );
    assert_eq!(differed, ["claude-2/r1/e1", "claude-2/r2/e1"]);
    let timed_out = filter(
        &mut screen,
        evidence::Filter::Rerun(Some(evidence::RerunKind::TimedOut)),
    );
    assert_eq!(timed_out, ["codex/r1/e3"]);
    // By criterion, on top of a re-run outcome: AC-1's differing re-runs.
    act(
        &mut screen,
        Action::FilterEvidence(evidence::Filter::Rerun(Some(evidence::RerunKind::Differed))),
    );
    let ac1 = filter(
        &mut screen,
        evidence::Filter::Criterion(Some("AC-1".into())),
    );
    assert_eq!(ac1, ["claude-2/r1/e1", "claude-2/r2/e1"]);
    // The chip that is on lets its group go when pressed again.
    assert!(acts(&screen).contains(&Action::FilterEvidence(evidence::Filter::Criterion(None))));
    act(&mut screen, Action::ClearEvidenceFilters);
    assert_eq!(shown_keys(&screen), all);
    // Each chip says how many choosing it would show.
    let labels = labels_under(&screen.tree, screen.center_host);
    for expected in [
        "claude 7",
        "あなた 1",
        "R2 4",
        "動画 1",
        "≠ 相違 2",
        "AC-1 7",
    ] {
        assert!(
            labels.iter().any(|l| l == expected),
            "{expected}: {labels:?}"
        );
    }
}

#[test]
fn ac23_opening_evidence_opens_the_stored_copy_not_the_worktree_file() {
    let workdir = temp("open");
    let (session, reply) = demo_with_files(&workdir);
    let (mut screen, _link) = live_with_dirs(session.clone(), &reply);
    let opened = record_opens(&mut screen);
    act(&mut screen, Action::ShowTab(CenterTab::Evidence));
    assert!(acts(&screen).contains(&Action::OpenEvidence(key("claude/r1/v1"))));
    // A video: the system player, on the stored copy.
    act(&mut screen, Action::OpenEvidence(key("claude/r1/v1")));
    let stored = reply.join("evidence/r1/claude/video/new-tab.mov");
    assert_eq!(*opened.borrow(), [Open::File(stored.clone())]);
    let worktree = session.worktrees[&PartId::new("claude")]
        .path
        .join("video/new-tab.mov");
    assert!(!opened.borrow().contains(&Open::File(worktree)));
    act(&mut screen, Action::RevealEvidence(key("claude/r1/v1")));
    assert_eq!(opened.borrow().last(), Some(&Open::Reveal(stored)));
    // A command: its stored log in the window's read-only view.
    let before = opened.borrow().len();
    act(&mut screen, Action::OpenEvidence(key("claude/r1/e1")));
    assert_eq!(opened.borrow().len(), before, "a log is not handed to open");
    assert_eq!(screen.log_open, Some(key("claude/r1/e1")));
    assert!(!screen.tree.is_hidden(screen.log.root));
    assert!(screen.tree.is_hidden(screen.center_host));
    let view = screen.tree.get::<TextView>(screen.log.view).unwrap();
    assert_eq!(view.text(), "stored copy of claude/r1/e1\nexit 101\n");
    // Its search highlights what is typed.
    screen.tree.set_focus(Some(screen.log.search));
    let mut text = TextStore::new(Default::default());
    Screen::dispatch(&mut screen, UiEvent::Text("101".into()), &mut text);
    let view = screen.tree.get::<TextView>(screen.log.view).unwrap();
    assert_eq!(view.found_ranges().len(), 1);
    assert!(acts(&screen).contains(&Action::CloseLog));
    act(&mut screen, Action::CloseLog);
    assert!(screen.tree.is_hidden(screen.log.root));
    assert!(!screen.tree.is_hidden(screen.center_host));
    // An item whose copy is gone opens nothing.
    std::fs::remove_file(reply.join("evidence/r1/claude-2/shots/split.png")).unwrap();
    let before = opened.borrow().len();
    act(&mut screen, Action::OpenEvidence(key("claude-2/r1/s1")));
    assert_eq!(opened.borrow().len(), before);
    assert!(toasts(&screen).iter().any(|t| t.contains("見つかりません")));
    let _ = std::fs::remove_dir_all(workdir);
}

/// Whether any widget under `root` draws a picture.
fn draws_a_picture(tree: &UiTree, root: WidgetId) -> bool {
    let mut stack = vec![root];
    while let Some(id) = stack.pop() {
        if tree.get::<Image>(id).is_some() {
            return true;
        }
        stack.extend(tree.children(id).iter().copied());
    }
    false
}

#[test]
fn ac32_media_show_as_placeholder_cards_without_a_picture_or_a_thumbnail() {
    let workdir = temp("cards");
    let (session, reply) = demo_with_files(&workdir);
    let before = files_with_times(&workdir);
    let (mut screen, _link) = live_with_dirs(session, &reply);
    let opened = record_opens(&mut screen);
    // The thread's strip: kind, format, size, caption, who, round, ▶ and
    // the video's marks.
    act(&mut screen, Action::SelectCriterion(Some("AC-1".into())));
    let thread = all_text(&screen);
    for expected in [
        "動画 · MOV",
        "▶",
        "cd /tmp、⌘T、新しいタブで pwd",
        "バイト",
        "▶ 00:04 ⌘T を押す",
        "▶ 00:06.5 新しいタブが $HOME を表示",
        "画像 · PNG",
        "あなた",
        "右のペインで cd /tmp のあと ⌘T、新しいタブは $HOME",
    ] {
        assert!(thread.contains(expected), "{expected}: {thread}");
    }
    // The size is the stored copy's, in bytes.
    let video = screen
        .session
        .as_ref()
        .unwrap()
        .evidence_item(&key("claude/r1/v1"))
        .unwrap()
        .stored
        .clone()
        .unwrap();
    assert!(thread.contains(&evidence::bytes(video.bytes)), "{thread}");
    // The gallery: the same cards, grouped by criterion, drawn (laid out
    // and painted as a snapshot is).
    act(&mut screen, Action::ShowTab(CenterTab::Gallery));
    lay_out(&mut screen, (1440.0, 900.0));
    let gallery = labels_under(&screen.tree, screen.center_host);
    assert!(gallery.iter().any(|l| l == "動画 · MOV"), "{gallery:?}");
    assert!(
        gallery
            .iter()
            .any(|l| l == "AC-2 分割したペインも元のペインの作業ディレクトリを引き継ぐ"),
        "{gallery:?}"
    );
    assert!(
        !gallery.iter().any(|l| l.starts_with("$ cargo")),
        "the gallery shows media only"
    );
    // No picture anywhere, and nothing written beside the stored copies.
    let root = screen.tree.children(screen.board)[0];
    assert!(!draws_a_picture(&screen.tree, root));
    assert_eq!(files_with_times(&workdir), before);
    // 開く on a card asks for the stored copy.
    act(&mut screen, Action::OpenEvidence(key("user/1")));
    assert_eq!(
        *opened.borrow(),
        [Open::File(reply.join("evidence/user/1.png"))]
    );
    let _ = std::fs::remove_dir_all(workdir);
}

#[test]
fn a_criterions_thread_leaves_out_media_whose_shows_names_another_criterion() {
    let mut session = crate::demo::session();
    let video = key("claude/r1/v1");
    let caption = "cd /tmp、⌘T、新しいタブで pwd";
    // The video shows AC-1 only; claude's AC-2 result cites it too.
    let item = session
        .evidence
        .iter_mut()
        .find(|e| e.key == video)
        .unwrap();
    let EvidenceBody::File { shows, .. } = &mut item.body else {
        panic!("{:?}", item.body);
    };
    *shows = vec!["AC-1".into()];
    session
        .results
        .iter_mut()
        .find(|r| r.part == PartId::new("claude") && r.criterion == "AC-2")
        .unwrap()
        .evidence
        .push(video.clone());
    let item = session.evidence_item(&video).unwrap();
    assert!(!evidence::relates(&session, item, "AC-2"));
    let (mut screen, _link) = live_with(session);
    act(&mut screen, Action::SelectCriterion(Some("AC-2".into())));
    let thread = all_text(&screen);
    assert!(thread.contains("基準 AC-2"), "{thread}");
    assert!(!thread.contains(caption), "{thread}");
    act(&mut screen, Action::SelectCriterion(Some("AC-1".into())));
    assert!(all_text(&screen).contains(caption));
}

#[test]
fn the_defects_board_filters_and_a_row_opens_its_thread_with_repro_steps() {
    let mut screen = Verify::demo().with_view(Some(CenterTab::Defects), None);
    let rows = |screen: &Verify| {
        acts(screen)
            .into_iter()
            .filter(|a| matches!(a, Action::SelectDefect(Some(_))))
            .count()
    };
    assert_eq!(rows(&screen), 2);
    act(&mut screen, Action::ToggleSeverity(Severity::High));
    assert_eq!(rows(&screen), 1);
    act(&mut screen, Action::ToggleSeverity(Severity::High));
    act(&mut screen, Action::ToggleCategory("falsify".into()));
    assert_eq!(rows(&screen), 1);
    act(&mut screen, Action::ClearDefectFilters);
    act(
        &mut screen,
        Action::FilterReporter(Some(PartId::new("claude"))),
    );
    assert_eq!(rows(&screen), 2, "claude raised D1 and cross-checked D2");
    act(
        &mut screen,
        Action::FilterReporter(Some(PartId::new("claude-2"))),
    );
    assert_eq!(rows(&screen), 0);
    act(&mut screen, Action::ClearDefectFilters);
    act(&mut screen, Action::SelectDefect(Some("D2".into())));
    let thread = all_text(&screen);
    assert!(thread.contains("不具合 D2"), "{thread}");
    assert!(thread.contains("再現手順"), "{thread}");
    assert!(thread.contains("⇄ 再現"), "{thread}");
    let board = labels_under(&screen.tree, screen.center_host).join("\n");
    assert!(
        board.contains("再現手順: 1. cd /tmp のペインを閉じる"),
        "{board}"
    );
}

#[test]
fn the_report_tab_is_provisional_until_a_verdict() {
    let held = Verify::demo_held();
    let tabs = labels_under(&held.tree, held.tabs_host);
    assert!(tabs.contains(&"レポート（暫定）".to_owned()), "{tabs:?}");
    let done = Verify::demo().with_view(Some(CenterTab::Report), None);
    let tabs = labels_under(&done.tree, done.tabs_host);
    assert!(tabs.contains(&"レポート".to_owned()), "{tabs:?}");
    let report = labels_under(&done.tree, done.center_host).join("\n");
    assert!(report.contains("総合判定"), "{report}");
}

#[test]
fn the_progress_strip_names_the_round_steps_and_re_runs() {
    let held = Verify::demo_held();
    let strip = labels_under(&held.tree, held.progress_host);
    for expected in [
        "ラウンド 1 / 2 · 独立検証",
        "✓ 基準策定",
        "✓ 判定",
        "再実行 9 / 9",
        "停止中（次のラウンドへ で再開）",
    ] {
        assert!(strip.iter().any(|l| l == expected), "{expected}: {strip:?}");
    }
    assert!(!strip.iter().any(|l| l == "完了"), "{strip:?}");
    let confirming = Verify::demo_criteria();
    let strip = labels_under(&confirming.tree, confirming.progress_host);
    assert!(strip.iter().any(|l| l == "基準確認待ち"), "{strip:?}");
    assert!(
        strip.iter().any(|l| l == "● 基準策定（確認待ち）"),
        "{strip:?}"
    );
}

#[test]
fn a_verifier_card_shows_its_strategy_results_and_worktree() {
    let screen = Verify::demo_held();
    let cards = labels_under(&screen.tree, screen.left_host);
    for expected in ["仕様適合", "反証", "回帰", "R1 の結果 ✓ 3 ✗ 1 ⊘ 0 – 1"] {
        assert!(cards.iter().any(|l| l == expected), "{expected}: {cards:?}");
    }
    assert!(acts(&screen).contains(&Action::OpenWorktree(PartId::new("claude"))));
    // The finished demo removed its worktrees: nothing to open.
    let done = Verify::demo();
    assert!(
        !acts(&done)
            .iter()
            .any(|a| matches!(a, Action::OpenWorktree(_)))
    );
}

#[test]
fn ac27_worktree_and_snapshot_buttons_open_finder_through_the_opener() {
    let workdir = temp("worktree");
    let mut session = crate::demo::held();
    let tree_dir = workdir.join("verify-ver-demo-claude");
    std::fs::create_dir_all(&tree_dir).unwrap();
    session.workdir = workdir.clone();
    session
        .worktrees
        .get_mut(&PartId::new("claude"))
        .unwrap()
        .path = tree_dir.clone();
    let reply = store::reply_dir(&session);
    std::fs::create_dir_all(store::target_dir(&reply)).unwrap();
    let (mut screen, link) = live_with_dirs(session, &reply);
    let opened = record_opens(&mut screen);
    act(&mut screen, Action::OpenWorktree(PartId::new("claude")));
    act(&mut screen, Action::OpenSnapshot);
    assert_eq!(
        *opened.borrow(),
        [Open::File(tree_dir), Open::File(store::target_dir(&reply))]
    );
    act(&mut screen, Action::FocusPane(PartId::new("codex")));
    assert_eq!(
        commands(&link),
        [Command::Focus {
            part: PartId::new("codex")
        }]
    );
    let _ = std::fs::remove_dir_all(workdir);
}

// ---- round controls and the composer ----

/// L1-4 item 2, L1-7 item 2: the progress strip counts every item asked
/// for re-run, those of a verifier whose script could not be written
/// (`not_run`, no `RerunRequested`) too, as the round's evidence lists them.
#[test]
fn the_rerun_count_holds_the_items_whose_script_could_not_be_written() {
    use verify::model::types::{EvidenceBody, Isolation, Rerun};
    let mut session = crate::demo::held();
    assert_eq!(session.isolation, Isolation::Worktree);
    let asked: Vec<(PartId, EvidenceKey)> = session
        .evidence
        .iter()
        .filter(|e| matches!(e.body, EvidenceBody::Command { rerun: true, .. }))
        .filter_map(|e| {
            e.key
                .split()
                .filter(|(_, r, _)| *r == 1)
                .map(|(p, ..)| (p, e.key.clone()))
        })
        .collect();
    let parts: std::collections::BTreeSet<&PartId> = asked.iter().map(|(p, _)| p).collect();
    assert!(parts.len() >= 2, "{asked:?}");
    // One verifier's script was never written: its items end `not_run`.
    let unwritten = (*parts.iter().next().unwrap()).clone();
    session.rounds[0].reruns.remove(&unwritten);
    for e in &mut session.evidence {
        if asked.iter().any(|(p, k)| *p == unwritten && *k == e.key) {
            e.rerun = Some(Rerun::NotRun {
                why: "再実行スクリプトを書けません".into(),
                log: None,
            });
        }
    }
    let round = session.rounds[0].clone();
    assert_eq!(
        progress::reruns(&session, &round),
        (asked.len(), asked.len())
    );
    // Not yet ended: counted as asked, not done.
    for e in &mut session.evidence {
        if asked.iter().any(|(p, k)| *p == unwritten && *k == e.key) {
            e.rerun = None;
        }
    }
    let unwritten_items = asked.iter().filter(|(p, _)| *p == unwritten).count();
    assert_eq!(
        progress::reruns(&session, &round),
        (asked.len() - unwritten_items, asked.len())
    );
}

/// L1-4 item 6, L1-7: an ended verification whose worktree is still
/// there (its removal failed: locked, say) offers no +1 / +3, which the
/// engine would refuse until it is removed; once removed, it does.
#[test]
fn extending_is_not_offered_while_an_ended_verifications_worktree_is_left() {
    let mut session = crate::demo::session();
    assert!(session.closed && session.extendable().is_ok());
    session
        .worktrees
        .get_mut(&PartId::new("codex"))
        .unwrap()
        .state = verify::model::types::WorktreeState::Made;
    let (screen, _link) = live_with(session);
    let a = acts(&screen);
    assert!(
        !a.contains(&Action::Extend(1)) && !a.contains(&Action::Extend(3)),
        "{a:?}"
    );
    let (screen, _link) = live_with(crate::demo::session());
    let a = acts(&screen);
    assert!(
        a.contains(&Action::Extend(1)) && a.contains(&Action::Extend(3)),
        "{a:?}"
    );
}

#[test]
fn round_controls_and_pane_jumps_reach_the_runner() {
    let (mut screen, link) = live_with(crate::demo::held());
    let a = acts(&screen);
    for part in ["claude", "codex", "claude-2", "judge"] {
        assert!(a.contains(&Action::FocusPane(PartId::new(part))), "{part}");
    }
    assert!(widget_for(&screen, &Action::Continue).is_some());
    act(&mut screen, Action::Continue);
    act(&mut screen, Action::SetHold(false));
    act(&mut screen, Action::Extend(3));
    act(&mut screen, Action::Retry);
    assert_eq!(
        commands(&link),
        [
            Command::Continue,
            Command::SetHold { hold: false },
            Command::Extend { by: 3 },
            Command::Retry,
        ]
    );
    assert!(!screen.tree.is_hidden(screen.composer.root));
}

#[test]
fn retry_is_offered_exactly_when_the_engine_has_something_to_retry() {
    use verify::model::types::{Phase, Worktree, WorktreeState};
    let offers = |session: Session| {
        let (screen, _link) = live_with(session);
        acts(&screen).contains(&Action::Retry)
    };
    let gave_up = Some("reply rejected 2 times".to_owned());
    // The judge's criteria, turned down up to the limit before round 1.
    let mut s = crate::demo::criteria_held();
    s.criteria_accepted = false;
    s.criteria_brief.as_mut().unwrap().receipt.failed = gave_up.clone();
    assert!(offers(s), "the criteria brief");
    // Round 1's worktrees could not be made (a full disk).
    let mut s = crate::demo::criteria_held();
    s.frozen_at = Some(1);
    s.worktrees.insert(
        PartId::new("claude"),
        Worktree {
            path: PathBuf::from("/w-wt/verify-ver-demo-criteria-claude"),
            state: WorktreeState::Failed {
                reason: "No space left on device".into(),
            },
        },
    );
    assert!(offers(s), "the worktrees");
    // A verifier that gave up while the round is verified...
    let mut s = crate::demo::held();
    let round = s.rounds.last_mut().unwrap();
    round.phase = Phase::Verifying;
    round.briefs[0].receipt.replied = None;
    round.briefs[0].receipt.failed = gave_up.clone();
    assert!(offers(s.clone()), "a verifier's work brief");
    // ...is not taken up once the round is ruled: only the judge's is.
    let round = s.rounds.last_mut().unwrap();
    round.phase = Phase::Ruling;
    let judge = round.judge_brief.as_mut().unwrap();
    judge.receipt.replied = None;
    assert!(!offers(s.clone()), "nothing the engine retries");
    s.rounds
        .last_mut()
        .unwrap()
        .judge_brief
        .as_mut()
        .unwrap()
        .receipt
        .failed = gave_up;
    assert!(offers(s), "the ruling brief");
}

#[test]
fn return_on_a_held_verification_goes_to_the_next_round() {
    let (mut screen, link) = live_with(crate::demo::held());
    press(&mut screen, noa_gpui_core::Key::Enter);
    assert_eq!(commands(&link), vec![Command::Continue]);
}

fn type_draft(screen: &mut Verify, body: &str) {
    screen
        .tree
        .update::<TextArea, _>(screen.composer.area, |a, cx| a.set_text(cx, body));
}

fn draft(screen: &Verify) -> String {
    composer::text(&screen.tree, &screen.composer)
}

#[test]
fn return_on_a_running_verification_sends_the_draft() {
    let mut running = crate::demo::held();
    running.hold = false;
    let (mut screen, link) = live_with(running);
    type_draft(&mut screen, "AC-2 は Linux でも");
    press(&mut screen, noa_gpui_core::Key::Enter);
    assert!(matches!(
        commands(&link).as_slice(),
        [Command::Intervene { body, to }] if body == "AC-2 は Linux でも" && to.is_empty()
    ));
}

#[test]
fn the_composer_addresses_everyone_each_verifier_or_the_judge() {
    let session = crate::demo::held();
    let labels: Vec<String> = composer::choices(&session)
        .into_iter()
        .map(|(l, _)| l)
        .collect();
    assert_eq!(labels, ["全員", "claude", "codex", "claude-2", "判定役"]);
    let (mut screen, link) = live_with(session);
    let mut text = TextStore::new(Default::default());
    // The menu's last entry: the judge.
    for _ in 0..4 {
        let recipients = screen.composer.recipients;
        press_down(&mut screen, recipients, &mut text);
    }
    type_draft(&mut screen, "AC-5 は保留で");
    act(&mut screen, Action::Send);
    assert_eq!(
        commands(&link),
        [Command::Intervene {
            to: vec![PartId::new("judge")],
            body: "AC-5 は保留で".into()
        }]
    );
}

fn with_intervention(mut session: Session, body: &str) -> Session {
    let n = session.messages.len() + 1;
    session.messages.push(verify::model::types::Message {
        id: verify::model::types::MessageId::nth(n),
        round: 1,
        author: verify::model::types::Author::User,
        kind: verify::model::types::MsgKind::Intervention,
        to: Vec::new(),
        body: body.to_owned(),
        at: 1,
    });
    session
}

#[test]
fn a_sent_draft_clears_when_it_reaches_the_log_unless_rewritten() {
    for (rewrite, left) in [(None, ""), (Some("別の下書き"), "別の下書き")] {
        let (mut screen, link) = live_with(crate::demo::held());
        type_draft(&mut screen, "AC-2 も見て");
        act(&mut screen, Action::Send);
        if let Some(text) = rewrite {
            type_draft(&mut screen, text);
        }
        assert_eq!(commands(&link).len(), 1);
        let mut next = with_intervention(crate::demo::held(), "AC-2 も見て");
        next.rev += 1;
        link.snapshots.send(snapshot(next, None, 0)).unwrap();
        screen.poll_live(&mut TextStore::new(Default::default()));
        assert_eq!(draft(&screen), left);
        assert_eq!(screen.pending_intervention, None);
    }
}

#[test]
fn a_judge_summary_with_the_sent_text_does_not_count_as_the_send() {
    let (mut screen, link) = live_with(crate::demo::held());
    type_draft(&mut screen, "AC-2 も見て");
    act(&mut screen, Action::Send);
    assert_eq!(commands(&link).len(), 1);
    let mut next = crate::demo::held();
    let n = next.messages.len() + 1;
    next.messages.push(verify::model::types::Message {
        id: verify::model::types::MessageId::nth(n),
        round: 1,
        author: verify::model::types::Author::Part(PartId::new("judge")),
        kind: verify::model::types::MsgKind::Summary,
        to: Vec::new(),
        body: "AC-2 も見て".to_owned(),
        at: 2,
    });
    next.rev += 1;
    link.snapshots.send(snapshot(next, None, 0)).unwrap();
    screen.poll_live(&mut TextStore::new(Default::default()));
    assert_eq!(draft(&screen), "AC-2 も見て");
    assert!(screen.pending_intervention.is_some());
}

#[test]
fn ending_a_verification_asks_first_and_return_keeps_it_running() {
    let (mut screen, link) = live_with(crate::demo::held());
    act(&mut screen, Action::Close);
    assert!(screen.close_alert.is_some());
    assert_eq!(commands(&link), vec![]);
    press(&mut screen, noa_gpui_core::Key::Enter);
    assert!(screen.close_alert.is_none());
    assert_eq!(commands(&link), vec![]);
    act(&mut screen, Action::Close);
    press(&mut screen, noa_gpui_core::Key::Escape);
    assert_eq!(commands(&link), vec![]);
    act(&mut screen, Action::Close);
    press(&mut screen, noa_gpui_core::Key::Tab);
    press(&mut screen, noa_gpui_core::Key::Space);
    assert!(screen.close_alert.is_none());
    assert_eq!(commands(&link), vec![Command::Close]);
}

#[test]
fn the_shown_tab_and_the_filters_on_are_selected_buttons() {
    let mut screen = Verify::demo().with_view(Some(CenterTab::Defects), None);
    let is_selected = |screen: &Verify, action: &Action| {
        let id = widget_for(screen, action).unwrap();
        screen.tree.get::<Button>(id).unwrap().is_selected()
    };
    assert!(is_selected(&screen, &Action::ShowTab(CenterTab::Defects)));
    assert!(!is_selected(&screen, &Action::ShowTab(CenterTab::Criteria)));
    assert!(is_selected(
        &screen,
        &Action::ToggleStatus(DefectStatus::Agreed)
    ));
    act(&mut screen, Action::ToggleSeverity(Severity::Critical));
    assert!(is_selected(
        &screen,
        &Action::ToggleSeverity(Severity::Critical)
    ));
    // The gallery is the evidence tab's other face: its tab stays on.
    act(&mut screen, Action::ShowTab(CenterTab::Gallery));
    assert!(is_selected(&screen, &Action::ShowTab(CenterTab::Gallery)));
}

/// Presses the focused widget with Space and returns the action the
/// widget focused afterwards carries.
fn press_focused(screen: &mut Verify, action: &Action) -> Option<Action> {
    let id = widget_for(screen, action).unwrap();
    screen.tree.set_focus(Some(id));
    press(screen, noa_gpui_core::Key::Space);
    let focused = screen.tree.focused()?;
    screen
        .actions
        .iter()
        .find(|(id, _)| *id == focused)
        .map(|(_, a)| a.clone())
}

#[test]
fn keyboard_focus_stays_on_what_was_pressed() {
    let mut screen = Verify::demo();
    let ac2 = Action::SelectCriterion(Some("AC-2".into()));
    let clear = Action::SelectCriterion(None);
    assert_eq!(press_focused(&mut screen, &ac2), Some(clear.clone()));
    assert_eq!(screen.selected, Some(Selected::Criterion("AC-2".into())));
    assert_eq!(press_focused(&mut screen, &clear), Some(ac2));
    assert_eq!(screen.selected, None);
    let evidence = Action::ShowTab(CenterTab::Evidence);
    assert_eq!(press_focused(&mut screen, &evidence), Some(evidence));
}

#[test]
fn keyboard_focus_survives_a_new_state_of_the_verification() {
    let (mut screen, link) = live_with(crate::demo::held());
    let tab = Action::ShowTab(CenterTab::Defects);
    let id = widget_for(&screen, &tab).unwrap();
    screen.tree.set_focus(Some(id));
    let mut next = crate::demo::held();
    next.rev += 1;
    link.snapshots.send(snapshot(next, None, 0)).unwrap();
    screen.poll_live(&mut TextStore::new(Default::default()));
    let focused = screen.tree.focused().expect("focus kept");
    assert_ne!(focused, id, "the screen was rebuilt");
    assert_eq!(widget_for(&screen, &tab), Some(focused));
}

struct RefusingClipboard;

impl noa_gpui_core::clipboard::Clipboard for RefusingClipboard {
    fn read_text(&mut self) -> Option<String> {
        Some("前の内容".to_owned())
    }
    fn write_text(&mut self, _text: &str) {}
}

struct WriteOnlyClipboard;

impl noa_gpui_core::clipboard::Clipboard for WriteOnlyClipboard {
    fn read_text(&mut self) -> Option<String> {
        None
    }
    fn write_text(&mut self, _text: &str) {}
}

#[test]
fn copying_the_report_says_whether_it_reached_the_clipboard() {
    let mut screen = Verify::demo();
    act(&mut screen, Action::CopyReport);
    assert_eq!(
        screen.tree.clipboard().read_text(),
        Some(store::report_text(screen.session.as_ref().unwrap()))
    );
    assert_eq!(
        toasts(&screen),
        ["レポートをクリップボードにコピーしました"]
    );
    let mut screen = Verify::demo();
    screen.tree.set_clipboard(Box::new(RefusingClipboard));
    act(&mut screen, Action::CopyReport);
    assert_eq!(
        toasts(&screen),
        ["レポートをクリップボードにコピーできませんでした"]
    );
    let mut screen = Verify::demo();
    screen.tree.set_clipboard(Box::new(WriteOnlyClipboard));
    act(&mut screen, Action::CopyReport);
    assert_eq!(
        toasts(&screen),
        ["レポートをコピーしましたが、クリップボードを読み返して確かめられませんでした"]
    );
}

#[test]
fn the_report_is_saved_alone_or_exported_with_its_evidence() {
    let workdir = temp("save");
    let (session, reply) = demo_with_files(&workdir);
    let (mut screen, _link) = live_with_dirs(session.clone(), &reply);
    let mut text = TextStore::new(Default::default());
    // レポートのみ.
    act(&mut screen, Action::SaveReport);
    let asked = Screen::take_file_dialogs(&mut screen);
    assert_eq!(asked.len(), 1);
    assert!(asked[0].save);
    assert_eq!(asked[0].file_name, "ver-demo.md");
    let alone = workdir.join("out/ver-demo.md");
    std::fs::create_dir_all(alone.parent().unwrap()).unwrap();
    Screen::file_dialog_finished(
        &mut screen,
        FileDialogResult::Saved(alone.clone()),
        &mut text,
    );
    assert_eq!(
        std::fs::read_to_string(&alone).unwrap(),
        store::report_text(&session)
    );
    // 証跡ごと書き出す: a new folder with the report and every copy.
    act(&mut screen, Action::ExportBundle);
    let asked = Screen::take_file_dialogs(&mut screen);
    assert!(asked[0].save);
    let bundle = workdir.join("out/bundle");
    Screen::file_dialog_finished(
        &mut screen,
        FileDialogResult::Saved(bundle.clone()),
        &mut text,
    );
    assert_eq!(
        std::fs::read_to_string(bundle.join("report.md")).unwrap(),
        export::report_markdown(&session, None)
    );
    assert!(
        bundle
            .join("evidence/r1/claude/video/new-tab.mov")
            .is_file()
    );
    assert!(bundle.join("evidence/user/1.png").is_file());
    // Never over what is there.
    act(&mut screen, Action::ExportBundle);
    let _ = Screen::take_file_dialogs(&mut screen);
    Screen::file_dialog_finished(&mut screen, FileDialogResult::Saved(bundle), &mut text);
    assert!(
        toasts(&screen)
            .iter()
            .any(|t| t.starts_with("書き出せませんでした")),
        "{:?}",
        toasts(&screen)
    );
    let _ = std::fs::remove_dir_all(workdir);
}

// ---- requirement A: ペインをすべて閉じる ----

#[test]
fn ac36_the_agents_panes_close_together_only_once_the_verification_has_finished() {
    // Running: not offered.
    let (screen, _link) = live_with(crate::demo::held());
    assert!(!acts(&screen).contains(&Action::ClosePanes));
    // Ended with a verdict, in the window whose runner holds it: offered.
    let finished = crate::demo::session();
    assert!(finished.closed);
    let (mut screen, link) = live_with(finished.clone());
    assert!(acts(&screen).contains(&Action::ClosePanes));
    // It asks first: Return and Escape answer キャンセル.
    act(&mut screen, Action::ClosePanes);
    assert!(screen.panes_alert.is_some());
    press(&mut screen, noa_gpui_core::Key::Enter);
    assert!(screen.panes_alert.is_none());
    act(&mut screen, Action::ClosePanes);
    press(&mut screen, noa_gpui_core::Key::Escape);
    assert_eq!(commands(&link), vec![]);
    // Chosen on purpose, it closes them.
    act(&mut screen, Action::ClosePanes);
    press(&mut screen, noa_gpui_core::Key::Tab);
    press(&mut screen, noa_gpui_core::Key::Space);
    assert_eq!(commands(&link), vec![Command::ClosePanes]);
    // The status bar holds it at the smallest window.
    let (mut fresh, _link) = live_with(finished);
    lay_out(&mut fresh, SMALLEST);
    assert_eq!(cut_off(&fresh, fresh.status_trailing), vec![]);
    // Ended with 終了 (no verdict) counts as finished too.
    let mut ended = crate::demo::held();
    ended.closed = true;
    let (screen, _link) = live_with(ended);
    assert!(acts(&screen).contains(&Action::ClosePanes));
    // A read-only stored window has no Noa to close panes in.
    let root = temp("panes-ro");
    let stored = Verify::stored(&stored_demo(&root)).unwrap();
    assert!(!acts(&stored).contains(&Action::ClosePanes));
    // Nor does the demo.
    assert!(!acts(&Verify::demo()).contains(&Action::ClosePanes));
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn ac36_an_interrupted_stored_verification_is_not_offered_to_close_its_panes() {
    let root = temp("panes-interrupted");
    let dir = stored_unfinished(&root);
    let (channel, link) = Channel::fake();
    let mut screen = stored_ready(channel, &dir);
    // Where its timeline posts stand is asked; nothing to probe.
    assert_eq!(
        commands(&link),
        [
            Command::Leave,
            Command::StoredTimeline {
                dir: dir.clone(),
                resend: false,
            }
        ],
        "nothing to probe"
    );
    link.panes
        .send(OpenPanes {
            generation: 1,
            dir: dir.clone(),
            count: 4,
        })
        .unwrap();
    screen.poll_live(&mut TextStore::new(Default::default()));
    assert!(!acts(&screen).contains(&Action::ClosePanes));
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn ac36_a_finished_stored_verification_probes_its_panes_and_closes_them_by_folder() {
    let root = temp("panes-stored");
    let dir = stored_demo(&root);
    let (channel, link) = Channel::fake();
    let mut screen = stored_ready(channel, &dir);
    assert_eq!(
        commands(&link),
        [
            Command::Leave,
            Command::ProbePanes { dir: dir.clone() },
            Command::StoredTimeline {
                dir: dir.clone(),
                resend: false,
            }
        ]
    );
    assert!(
        !acts(&screen).contains(&Action::ClosePanes),
        "not yet answered"
    );
    // Another folder's answer, or an older generation's, is not this one.
    for (generation, at, count) in [
        (1, root.join("other"), 3),
        (0, dir.clone(), 3),
        (1, dir.clone(), 0),
    ] {
        link.panes
            .send(OpenPanes {
                generation,
                dir: at,
                count,
            })
            .unwrap();
        screen.poll_live(&mut TextStore::new(Default::default()));
        assert!(!acts(&screen).contains(&Action::ClosePanes));
    }
    link.panes
        .send(OpenPanes {
            generation: 1,
            dir: dir.clone(),
            count: 4,
        })
        .unwrap();
    screen.poll_live(&mut TextStore::new(Default::default()));
    assert!(acts(&screen).contains(&Action::ClosePanes));
    act(&mut screen, Action::ClosePanes);
    press(&mut screen, noa_gpui_core::Key::Enter);
    assert_eq!(commands(&link), vec![]);
    act(&mut screen, Action::ClosePanes);
    press(&mut screen, noa_gpui_core::Key::Tab);
    press(&mut screen, noa_gpui_core::Key::Space);
    assert_eq!(
        commands(&link),
        [Command::ClosePanesStored { dir: dir.clone() }]
    );
    assert!(!acts(&screen).contains(&Action::ClosePanes));
    let _ = std::fs::remove_dir_all(root);
}

// ---- criteria editing ----

fn row<'a>(screen: &'a Verify, id: &str) -> &'a criteria::Row {
    screen
        .editor
        .rows
        .iter()
        .find(|r| r.id == id)
        .unwrap_or_else(|| panic!("no row for {id}"))
}

/// AC-5, AC-6, L1-2: the only criterion left, though its source could be
/// removed, offers no 削除 (the engine keeps at least one), and says why.
#[test]
fn ac06_the_last_criterion_cannot_be_removed_and_its_button_says_why() {
    use noa_gpui_core::Widget as _;
    let tooltip = |screen: &Verify, id: &str| {
        let remove = widget_for(screen, &Action::RemoveCriterion(id.into())).unwrap();
        let text = screen
            .tree
            .get::<noa_gpui_core::widgets::Button>(remove)
            .and_then(|b| b.tooltip(noa_gpui_scene::Point::default()));
        (screen.tree.is_enabled(remove), text.unwrap_or_default())
    };
    let mut session = crate::demo::criteria_held();
    session.criteria.retain(|c| c.id == "AC-3");
    let (screen, _link) = live_with(session);
    let (enabled, text) = tooltip(&screen, "AC-3");
    assert!(!enabled, "{text}");
    assert!(text.contains("1件以上"), "{text}");
    // With another criterion beside it, it goes.
    let mut session = crate::demo::criteria_held();
    session
        .criteria
        .retain(|c| c.id == "AC-3" || c.id == "AC-1");
    let (screen, _link) = live_with(session);
    let (enabled, text) = tooltip(&screen, "AC-3");
    assert!(enabled, "{text}");
    assert_eq!(text, "この基準を外します");
}

#[test]
fn ac06_each_criteria_change_is_sent_as_one_edit() {
    let (mut screen, link) = live_with(crate::demo::criteria_held());
    assert_eq!(screen.tab, CenterTab::Criteria);
    // The person's line and the spec's cannot go; the judge's own can.
    for (id, removable) in [
        ("AC-1", false),
        ("AC-2", false),
        ("AC-3", true),
        ("AC-4", true),
    ] {
        let remove = widget_for(&screen, &Action::RemoveCriterion(id.into())).unwrap();
        assert_eq!(screen.tree.is_enabled(remove), removable, "{id}");
    }
    act(&mut screen, Action::RemoveCriterion("AC-1".into()));
    assert!(commands(&link).is_empty());
    act(&mut screen, Action::RemoveCriterion("AC-3".into()));
    // A statement edited and saved: only what changed is sent.
    let statement = row(&screen, "AC-3").statement;
    screen.tree.update::<TextField, _>(statement, |f, cx| {
        f.set_text(cx, "OSC 7 がなくてもホームで開く")
    });
    act(&mut screen, Action::SaveCriterion("AC-3".into()));
    // Saving with nothing changed sends nothing.
    act(&mut screen, Action::SaveCriterion("AC-2".into()));
    act(&mut screen, Action::MoveCriterion("AC-2".into(), 0));
    // A new criterion.
    let add = screen.editor.add.as_ref().unwrap();
    let (s, h) = (add.statement, add.how);
    screen.tree.update::<TextField, _>(s, |f, cx| {
        f.set_text(cx, "タブを閉じても他のタブの cwd は変わらない")
    });
    screen
        .tree
        .update::<TextArea, _>(h, |a, cx| a.set_text(cx, "三つ開いて一つ閉じ、残りで pwd"));
    act(&mut screen, Action::AddCriterion);
    act(&mut screen, Action::StartVerify);
    let sent: Vec<Command> = commands(&link);
    assert_eq!(
        sent,
        [
            Command::EditCriteria {
                edit: CriteriaEdit::Remove { id: "AC-3".into() }
            },
            Command::EditCriteria {
                edit: CriteriaEdit::Edit {
                    id: "AC-3".into(),
                    statement: Some("OSC 7 がなくてもホームで開く".into()),
                    how: None,
                    priority: None,
                }
            },
            Command::EditCriteria {
                edit: CriteriaEdit::Move {
                    id: "AC-2".into(),
                    to: 0
                }
            },
            Command::EditCriteria {
                edit: CriteriaEdit::Add {
                    statement: "タブを閉じても他のタブの cwd は変わらない".into(),
                    how: "三つ開いて一つ閉じ、残りで pwd".into(),
                    priority: Priority::Must,
                }
            },
            Command::StartVerify,
        ]
    );
    assert!(
        toasts(&screen)
            .iter()
            .any(|t| t.contains("AC-2 に変更はありません"))
    );
}

#[test]
fn a_priority_chosen_in_its_menu_is_sent_at_once() {
    let (mut screen, link) = live_with(crate::demo::criteria_held());
    let mut text = TextStore::new(Default::default());
    let priority = row(&screen, "AC-1").priority;
    press_down(&mut screen, priority, &mut text);
    assert_eq!(
        commands(&link),
        [Command::EditCriteria {
            edit: CriteriaEdit::Edit {
                id: "AC-1".into(),
                statement: None,
                how: None,
                priority: Some(Priority::Should),
            }
        }]
    );
}

#[test]
fn a_criterion_being_typed_survives_a_new_state_until_the_log_has_it() {
    let (mut screen, link) = live_with(crate::demo::criteria_held());
    let statement = row(&screen, "AC-5").statement;
    screen
        .tree
        .update::<TextField, _>(statement, |f, cx| f.set_text(cx, "書きかけの基準"));
    // Another edit lands meanwhile: the screen is rebuilt.
    let mut next = crate::demo::criteria_held();
    next.rev += 1;
    link.snapshots
        .send(snapshot(next.clone(), None, 0))
        .unwrap();
    screen.poll_live(&mut TextStore::new(Default::default()));
    let statement = row(&screen, "AC-5").statement;
    assert_eq!(
        screen.tree.get::<TextField>(statement).unwrap().text(),
        "書きかけの基準"
    );
    // Saved and logged: the field shows the log's text, the draft gone.
    next.criteria
        .iter_mut()
        .find(|c| c.id == "AC-5")
        .unwrap()
        .statement = "書きかけの基準".into();
    next.rev += 1;
    link.snapshots.send(snapshot(next, None, 0)).unwrap();
    screen.poll_live(&mut TextStore::new(Default::default()));
    assert!(!screen.editor.drafts.contains_key("AC-5"));
}

#[test]
fn the_add_row_keeps_the_priority_chosen_across_a_tab_change() {
    let (mut screen, link) = live_with(crate::demo::criteria_held());
    let mut text = TextStore::new(Default::default());
    let add = screen.editor.add.as_ref().unwrap();
    let (s, h, p) = (add.statement, add.how, add.priority);
    screen
        .tree
        .update::<TextField, _>(s, |f, cx| f.set_text(cx, "閉じたタブは復元できる"));
    screen
        .tree
        .update::<TextArea, _>(h, |a, cx| a.set_text(cx, "⇧⌘T で戻す"));
    press_down(&mut screen, p, &mut text);
    assert_eq!(screen.tree.get::<PopUpButton>(p).unwrap().selected(), 1);
    assert!(commands(&link).is_empty());
    act(&mut screen, Action::ShowTab(CenterTab::Evidence));
    act(&mut screen, Action::ShowTab(CenterTab::Criteria));
    let p = screen.editor.add.as_ref().unwrap().priority;
    assert_eq!(screen.tree.get::<PopUpButton>(p).unwrap().selected(), 1);
    act(&mut screen, Action::AddCriterion);
    assert_eq!(
        commands(&link),
        [Command::EditCriteria {
            edit: CriteriaEdit::Add {
                statement: "閉じたタブは復元できる".into(),
                how: "⇧⌘T で戻す".into(),
                priority: Priority::Should,
            }
        }]
    );
}

#[test]
fn the_criteria_wait_for_confirmation_with_their_sources_and_a_start_button() {
    let screen = Verify::demo_criteria();
    let shown = labels_under(&screen.tree, screen.center_host);
    for expected in [
        "出典: あなた",
        "出典: 仕様 12 行目",
        "出典: 依頼",
        "出典: 推定",
        "変更済",
        "基準を確定して検証を開始",
    ] {
        assert!(shown.iter().any(|l| l == expected), "{expected}: {shown:?}");
    }
    assert!(acts(&screen).contains(&Action::StartVerify));
    // A stored one shows them read as they are.
    let root = temp("criteria-ro");
    let mut log = crate::demo::held_log();
    log.truncate(5);
    let dir = stored_log(&root, crate::demo::CRITERIA_ID, &log);
    let stored = Verify::stored(&dir).unwrap();
    assert!(stored.session.as_ref().unwrap().criteria_accepted);
    assert!(
        !acts(&stored)
            .iter()
            .any(|a| matches!(a, Action::SaveCriterion(_)))
    );
    let _ = std::fs::remove_dir_all(root);
}

// ---- 証跡を追加 ----

fn drop_files(screen: &mut Verify, files: &[&str]) {
    let mut text = TextStore::new(Default::default());
    Screen::dispatch(
        screen,
        UiEvent::Drop {
            pos: Default::default(),
            data: DragData::Files(files.iter().map(PathBuf::from).collect()),
        },
        &mut text,
    );
}

#[test]
fn dropped_media_waits_for_its_caption_and_is_attached_with_its_marks() {
    let (mut screen, link) = live_with(crate::demo::held());
    act(&mut screen, Action::SelectCriterion(Some("AC-1".into())));
    drop_files(&mut screen, &["/shots/rec.mov", "/shots/tab.png"]);
    assert!(!screen.tree.is_hidden(screen.attach.root));
    assert!(acts(&screen).contains(&Action::AttachSubmit));
    // The selected criterion is filled in; the caption is required.
    assert_eq!(
        screen
            .tree
            .get::<TextField>(screen.attach.shows)
            .unwrap()
            .text(),
        "AC-1"
    );
    act(&mut screen, Action::AttachSubmit);
    assert!(commands(&link).is_empty());
    let caption = screen.attach.caption;
    screen
        .tree
        .update::<TextArea, _>(caption, |a, cx| a.set_text(cx, "⌘T のあと $HOME"));
    let marks = screen.attach.marks;
    assert!(!screen.tree.is_hidden(marks), "a video takes marks");
    screen.tree.update::<TextArea, _>(marks, |a, cx| {
        a.set_text(cx, "00:06.5 新しいタブ\n00:04 ⌘T")
    });
    act(&mut screen, Action::AttachSubmit);
    assert!(commands(&link).is_empty(), "marks out of order");
    screen.tree.update::<TextArea, _>(marks, |a, cx| {
        a.set_text(cx, "00:04 ⌘T\n00:06.5 新しいタブ")
    });
    act(&mut screen, Action::AttachSubmit);
    assert_eq!(
        commands(&link),
        [Command::Attach(Box::new(Attachment {
            source: PathBuf::from("/shots/rec.mov"),
            caption: "⌘T のあと $HOME".into(),
            marks: vec![
                verify::model::types::Mark {
                    t_ms: 4_000,
                    note: "⌘T".into()
                },
                verify::model::types::Mark {
                    t_ms: 6_500,
                    note: "新しいタブ".into()
                },
            ],
            shows: vec!["AC-1".into()],
        }))]
    );
    // The next file comes up; an image takes no marks.
    assert!(!screen.tree.is_hidden(screen.attach.root));
    assert!(screen.tree.is_hidden(screen.attach.marks));
    act(&mut screen, Action::AttachCancel);
    assert!(screen.tree.is_hidden(screen.attach.root));
}

#[test]
fn media_is_attached_from_the_file_chooser_only_while_the_verification_runs() {
    let (mut screen, _link) = live_with(crate::demo::held());
    act(&mut screen, Action::ShowTab(CenterTab::Evidence));
    assert!(acts(&screen).contains(&Action::AttachEvidence));
    act(&mut screen, Action::AttachEvidence);
    let asked = Screen::take_file_dialogs(&mut screen);
    assert_eq!(asked.len(), 1);
    assert!(!asked[0].save);
    let mut text = TextStore::new(Default::default());
    Screen::file_dialog_finished(
        &mut screen,
        FileDialogResult::Opened(vec![PathBuf::from("/shots/a.png")]),
        &mut text,
    );
    assert!(!screen.tree.is_hidden(screen.attach.root));
    // Finished: nothing can be attached any more.
    let (mut done, _link) = live_with(crate::demo::session());
    act(&mut done, Action::ShowTab(CenterTab::Evidence));
    assert!(!acts(&done).contains(&Action::AttachEvidence));
    drop_files(&mut done, &["/shots/a.png"]);
    assert!(done.tree.is_hidden(done.attach.root));
    assert!(toasts(&done).iter().any(|t| t.contains("追加できます")));
    // Before round 1 neither.
    let (mut confirming, _link) = live_with(crate::demo::criteria_held());
    drop_files(&mut confirming, &["/shots/a.png"]);
    assert!(confirming.tree.is_hidden(confirming.attach.root));
}

#[test]
fn an_open_attach_form_keeps_its_text_but_cannot_add_once_the_verification_closes() {
    let (mut screen, link) = live_with(crate::demo::held());
    drop_files(&mut screen, &["/shots/tab.png"]);
    let caption = screen.attach.caption;
    screen
        .tree
        .update::<TextArea, _>(caption, |a, cx| a.set_text(cx, "新しいタブ"));
    assert!(acts(&screen).contains(&Action::AttachSubmit));
    assert!(screen.tree.is_hidden(screen.attach.closed));
    // The verdict arrives while the form is open.
    let mut closed = crate::demo::held();
    closed.closed = true;
    closed.rev += 1;
    link.snapshots.send(snapshot(closed, None, 0)).unwrap();
    screen.poll_live(&mut TextStore::new(Default::default()));
    assert!(!screen.tree.is_hidden(screen.attach.root));
    assert!(!acts(&screen).contains(&Action::AttachSubmit));
    assert!(!screen.tree.is_enabled(screen.attach.submit));
    assert!(!screen.tree.is_hidden(screen.attach.closed));
    assert_eq!(
        screen.tree.get::<TextArea>(caption).unwrap().text(),
        "新しいタブ"
    );
    assert!(commands(&link).is_empty());
    act(&mut screen, Action::AttachCancel);
    assert!(screen.tree.is_hidden(screen.attach.root));
}

// ---- stored views ----

#[test]
fn a_stored_verification_opens_read_only_without_composer_or_pane_buttons() {
    let root = temp("ro");
    let dir = stored_demo(&root);
    let before = files_with_times(&dir);
    let screen = Verify::stored(&dir).unwrap();
    assert!(!store::report_path(&dir).exists());
    assert_eq!(files_with_times(&dir), before);
    assert!(screen.read_only);
    assert!(screen.tree.is_hidden(screen.composer.root));
    let a = acts(&screen);
    assert!(!a.iter().any(|a| matches!(a, Action::FocusPane(_))));
    assert!(!a.contains(&Action::Close));
    assert_eq!(screen.session.as_ref().unwrap().rounds.len(), 2);
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn an_unreadable_log_is_refused() {
    let root = temp("bad");
    let dir = root.join("ver-x");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(store::EVENTS_FILE), "nope\n").unwrap();
    assert!(Verify::stored(&dir).err().unwrap().contains("を開けません"));
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn an_unfinished_verification_is_resumed_from_its_read_only_view_and_goes_live() {
    let root = temp("resume");
    let dir = stored_unfinished(&root);
    let (channel, link) = Channel::fake();
    let mut screen = stored_ready(channel, &dir);
    assert!(screen.read_only);
    assert!(acts(&screen).contains(&Action::ResumeShown));
    // It has no runner to say where its timeline posts stand.
    assert_eq!(
        commands(&link),
        [
            Command::Leave,
            Command::StoredTimeline {
                dir: dir.clone(),
                resend: false,
            }
        ]
    );
    act(&mut screen, Action::ResumeShown);
    assert_eq!(commands(&link), [Command::Resume { dir: dir.clone() }]);
    assert!(
        !acts(&screen).contains(&Action::ResumeShown),
        "offered twice"
    );
    let session = store::read_session(&dir).unwrap();
    link.snapshots.send(snapshot(session, None, 1)).unwrap();
    screen.poll_live(&mut TextStore::new(Default::default()));
    assert!(!screen.read_only);
    assert!(acts(&screen).contains(&Action::Close));
    let done = stored_demo(&root);
    let (channel, _link) = Channel::fake();
    let screen = stored_ready(channel, &done);
    assert!(!acts(&screen).contains(&Action::ResumeShown));
    // It ran all its rounds and no runner holds it: rounds can be added.
    assert!(acts(&screen).contains(&Action::Extend(1)));
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn worktrees_of_a_stored_interrupted_verification_are_removed_on_demand() {
    let root = temp("worktrees");
    let dir = stored_unfinished(&root);
    let (channel, link) = Channel::fake();
    let mut screen = stored_ready(channel, &dir);
    let _ = commands(&link);
    assert!(acts(&screen).contains(&Action::RemoveWorktrees));
    act(&mut screen, Action::RemoveWorktrees);
    assert_eq!(commands(&link), [Command::RemoveWorktrees { dir }]);
    // The finished demo removed its own; the running one has its runner.
    let done = stored_demo(&root);
    let (channel, _link) = Channel::fake();
    assert!(!acts(&stored_ready(channel, &done)).contains(&Action::RemoveWorktrees));
    let (live, _link) = live_with(crate::demo::held());
    assert!(!acts(&live).contains(&Action::RemoveWorktrees));
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn a_missing_report_is_rewritten_from_the_log_only_when_asked() {
    let root = temp("report");
    let dir = stored_demo(&root);
    let (channel, _link) = Channel::fake();
    let mut screen = stored_ready(channel, &dir);
    assert!(!store::report_path(&dir).exists());
    assert!(acts(&screen).contains(&Action::RegenerateReport));
    act(&mut screen, Action::RegenerateReport);
    let session = store::read_session(&dir).unwrap();
    assert_eq!(
        std::fs::read_to_string(store::report_path(&dir)).unwrap(),
        store::report_text(&session)
    );
    assert!(!acts(&screen).contains(&Action::RegenerateReport));
    assert!(!store::is_busy(&dir), "the log stayed locked");
    let _ = std::fs::remove_dir_all(root);
}

/// Writes `log` as the log in `dir`, as another window's runner would.
fn rewrite_log(dir: &Path, log: &[Vec<Event>]) {
    std::fs::remove_file(dir.join(store::EVENTS_FILE)).unwrap();
    let mut store = Store::create(dir, &log[0]).unwrap();
    store.append_all(&log[1..]).unwrap();
}

/// The stored view's next look at its log and lock.
fn look_again(screen: &mut Verify) {
    screen.busy_checked = None;
    screen.poll_live(&mut TextStore::new(Default::default()));
}

/// The finished demo's log under the held demo's id (so it is the same
/// verification, finished).
fn held_finished_log() -> Vec<Vec<Event>> {
    let mut log = crate::demo::log();
    if let Event::Created { id, .. } = &mut log[0][0] {
        *id = verify::model::types::SessionId::new(crate::demo::HELD_ID);
    }
    log
}

#[test]
fn a_stored_verification_finished_elsewhere_drops_resume_and_probes_its_panes() {
    let root = temp("finished");
    let dir = stored_unfinished(&root);
    let (channel, link) = Channel::fake();
    let mut screen = stored_ready(channel, &dir);
    assert!(acts(&screen).contains(&Action::ResumeShown));
    let _ = commands(&link);
    rewrite_log(&dir, &held_finished_log());
    look_again(&mut screen);
    assert!(screen.session.as_ref().unwrap().closed);
    assert!(!acts(&screen).contains(&Action::ResumeShown));
    assert_eq!(commands(&link), [Command::ProbePanes { dir }]);
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn a_stored_verification_finished_while_it_was_being_shown_is_caught_at_the_next_look() {
    let root = temp("mid-show");
    let dir = stored_unfinished(&root);
    let finished = dir.clone();
    AFTER_STORED_READ.with(|h| {
        *h.borrow_mut() = Some(Box::new(move || {
            rewrite_log(&finished, &held_finished_log())
        }));
    });
    let (channel, _link) = Channel::fake();
    let mut screen = stored_ready(channel, &dir);
    assert!(!screen.session.as_ref().unwrap().closed);
    look_again(&mut screen);
    assert!(screen.session.as_ref().unwrap().closed);
    assert!(!acts(&screen).contains(&Action::ResumeShown));
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn a_stored_verification_another_window_runs_shows_the_running_note() {
    let root = temp("held-note");
    let dir = stored_unfinished(&root);
    let held = Store::open(&dir).unwrap();
    let (channel, _link) = Channel::fake();
    let mut screen = stored_ready(channel, &dir);
    assert!(all_text(&screen).contains("検証の実行中です"));
    assert!(!acts(&screen).contains(&Action::ResumeShown));
    drop(held);
    look_again(&mut screen);
    assert!(!all_text(&screen).contains("検証の実行中です"));
    assert!(acts(&screen).contains(&Action::ResumeShown));
    let _ = std::fs::remove_dir_all(root);
}

// ---- 修正を依頼 ----

fn fix_actions(screen: &Verify) -> Vec<(AgentKind, FixScope)> {
    acts(screen)
        .into_iter()
        .filter_map(|a| match a {
            Action::RequestFix { agent, scope } => Some((agent, scope)),
            _ => None,
        })
        .collect()
}

#[test]
fn the_verdict_offers_a_fix_of_its_failed_criteria_and_defects_by_every_agent() {
    let (screen, _link) = live_with_dirs(crate::demo::session(), Path::new(REPLY));
    let offered = fix_actions(&screen);
    for scope in [FixScope::FailedCriteria, FixScope::Defects] {
        for kind in AgentKind::ALL {
            assert!(
                offered.contains(&(kind, scope.clone())),
                "{kind:?} {scope:?}"
            );
        }
    }
    let text = all_text(&screen);
    assert!(text.contains("不合格の基準 1 件:"), "{text}");
    assert!(text.contains("不具合 2 件:"), "{text}");
}

#[test]
fn a_fix_request_carries_the_failed_criteria_the_defects_and_the_store() {
    let (mut screen, link) = live_with_dirs(crate::demo::session(), Path::new(REPLY));
    act(
        &mut screen,
        Action::RequestFix {
            agent: AgentKind::Codex,
            scope: FixScope::FailedCriteria,
        },
    );
    act(
        &mut screen,
        Action::RequestFix {
            agent: AgentKind::Claude,
            scope: FixScope::Defects,
        },
    );
    let sent = commands(&link);
    let [Command::RequestFix(failed), Command::RequestFix(defects)] = sent.as_slice() else {
        panic!("{sent:?}");
    };
    assert_eq!(failed.agent, AgentKind::Codex);
    assert_eq!(failed.criteria, ["AC-1"]);
    assert!(failed.defects.is_empty());
    assert_eq!(
        failed.dir,
        PathBuf::from("/data/noa/verifications/ver-demo")
    );
    assert_eq!(defects.defects, ["D1", "D2"]);
    assert!(defects.criteria.is_empty());
}

#[test]
fn a_selection_offers_a_fix_of_itself_only() {
    let (mut screen, link) = live_with_dirs(crate::demo::session(), Path::new(REPLY));
    act(&mut screen, Action::SelectCriterion(Some("AC-1".into())));
    let offered = fix_actions(&screen);
    assert_eq!(offered.len(), AgentKind::ALL.len());
    assert!(
        offered
            .iter()
            .all(|(_, scope)| *scope == FixScope::Criterion("AC-1".into()))
    );
    act(
        &mut screen,
        Action::RequestFix {
            agent: AgentKind::Agy,
            scope: FixScope::Criterion("AC-1".into()),
        },
    );
    act(&mut screen, Action::SelectDefect(Some("D2".into())));
    act(
        &mut screen,
        Action::RequestFix {
            agent: AgentKind::Agy,
            scope: FixScope::Defect("D2".into()),
        },
    );
    let sent = commands(&link);
    let [Command::RequestFix(criterion), Command::RequestFix(defect)] = sent.as_slice() else {
        panic!("{sent:?}");
    };
    assert_eq!(
        (criterion.criteria.as_slice(), criterion.defects.len()),
        (&["AC-1".to_owned()][..], 0)
    );
    assert_eq!(defect.defects, ["D2"]);
    // A request for a selection no longer shown takes nothing.
    act(&mut screen, Action::SelectDefect(None));
    act(
        &mut screen,
        Action::RequestFix {
            agent: AgentKind::Agy,
            scope: FixScope::Defect("D2".into()),
        },
    );
    assert!(commands(&link).is_empty());
}

#[test]
fn no_fix_is_offered_while_the_agent_check_runs_or_in_a_headless_view() {
    let (mut screen, _link, _tx, mut text) = live_pending();
    screen.session = Some(Arc::new(crate::demo::session()));
    screen.rebuild(&mut text);
    assert!(fix_actions(&screen).is_empty());
    assert!(all_text(&screen).contains("インストール済みのエージェントを確認しています"));
    let root = temp("fix-headless");
    let screen = Verify::stored(&stored_demo(&root)).unwrap();
    assert!(fix_actions(&screen).is_empty());
    assert!(!all_text(&screen).contains("修正を依頼"));
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn a_stored_verification_in_the_live_window_can_be_fixed() {
    let root = temp("fix-live");
    let dir = stored_demo(&root);
    let (channel, link) = Channel::fake();
    let mut screen = stored_ready(channel, &dir);
    act(
        &mut screen,
        Action::RequestFix {
            agent: AgentKind::Claude,
            scope: FixScope::FailedCriteria,
        },
    );
    let sent: Vec<Command> = commands(&link)
        .into_iter()
        .filter(|c| matches!(c, Command::RequestFix(_)))
        .collect();
    let [Command::RequestFix(req)] = sent.as_slice() else {
        panic!("{sent:?}");
    };
    assert_eq!(req.dir, dir);
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn a_noa_older_than_0_9_13_gets_a_reason_instead_of_fix_buttons() {
    let (mut screen, link) = live_with_dirs(crate::demo::session(), Path::new(REPLY));
    for (seq, (version, offered)) in [("0.9.12", false), ("0.9.13", true), ("0.9.12", false)]
        .into_iter()
        .enumerate()
    {
        link.versions
            .send((seq as u64 + 1, version.to_owned()))
            .unwrap();
        screen.poll_live(&mut TextStore::new(Default::default()));
        assert_eq!(!fix_actions(&screen).is_empty(), offered, "{version}");
    }
    // An older connection's report arriving late does not win.
    link.versions.send((1, "0.9.13".to_owned())).unwrap();
    screen.poll_live(&mut TextStore::new(Default::default()));
    assert!(fix_actions(&screen).is_empty());
}

// ---- 同じ設定で再検証 ----

/// The demo as if it had verified `repo`.
fn demo_in(repo: &Path) -> Session {
    let mut session = crate::demo::session();
    session.target.repo = repo.to_owned();
    session.target.paths = vec!["crates/".into(), "docs/a b.md".into()];
    session.verifiers[2].strategy = Some(Strategy::Custom {
        id: "custom".into(),
        text: "全ロケールで確かめる".into(),
    });
    session
}

fn panel_of(session: &Session) -> Vec<VerifierChoice> {
    session
        .verifiers
        .iter()
        .map(|p| VerifierChoice {
            agent: AgentChoice {
                kind: p.kind,
                setup: p.setup.clone(),
            },
            strategy: match p.strategy.as_ref().unwrap() {
                Strategy::Builtin { id } => StrategyChoice::Builtin(*id),
                Strategy::Custom { text, .. } => StrategyChoice::Custom(text.clone()),
            },
        })
        .collect()
}

#[test]
fn verifying_again_fills_the_start_view_with_the_frozen_criteria_and_the_verdict() {
    let repo = temp("again");
    let session = demo_in(&repo);
    let (mut screen, _link) = live_with(session.clone());
    assert!(acts(&screen).contains(&Action::VerifyAgain(None)));
    act(&mut screen, Action::VerifyAgain(None));
    assert!(screen.session.is_none());
    assert!(!screen.tree.is_hidden(screen.start.root));
    let form = start::read(&screen.tree, &screen.start);
    assert_eq!(form.target().unwrap(), session.target);
    assert_eq!(form.panel, panel_of(&session));
    assert_eq!(form.judge.kind, session.judge.kind);
    assert_eq!(form.max_rounds, session.max_rounds);
    assert_eq!(form.isolation, session.isolation);
    // The frozen criteria as the person's, word for word.
    let statements: Vec<&str> = session
        .criteria
        .iter()
        .map(|c| c.statement.as_str())
        .collect();
    assert_eq!(form.user_criteria(), statements);
    // The request keeps what was asked and adds the verdict and blocking.
    assert!(form.request.starts_with(session.request.trim_end()));
    assert!(
        form.request
            .contains("（前回の検証 ver-demo の総合判定: 不合格）"),
        "{}",
        form.request
    );
    assert!(form.request.contains("- AC-1 [不合格]"), "{}", form.request);
    assert!(form.request.contains("- D1 [高]"), "{}", form.request);
    // Again after that: the earlier note gives way to the new one.
    let mut twice = session.clone();
    twice.request = form.request.clone();
    let again = start::Form::from_session(&twice).request;
    assert_eq!(again, form.request);
    assert_eq!(again.matches(start::AGAIN_MARK).count(), 1);
    assert_eq!(screen.tree.focused(), Some(screen.start.repo));
    let _ = std::fs::remove_dir_all(repo);
}

#[test]
fn starting_the_filled_form_starts_the_same_verification_in_two_steps() {
    let repo = temp("again-start");
    let session = demo_in(&repo);
    let (mut screen, link) = live_with(session.clone());
    press_button(&mut screen, &Action::VerifyAgain(None));
    press_button(&mut screen, &Action::Start);
    let started: Vec<Command> = commands(&link)
        .into_iter()
        .filter(|c| matches!(c, Command::Start { .. }))
        .collect();
    let [
        Command::Start {
            target,
            panel,
            user_criteria,
            isolation,
            ..
        },
    ] = started.as_slice()
    else {
        panic!("{started:?}");
    };
    assert_eq!(**target, session.target);
    assert_eq!(*panel, panel_of(&session));
    assert_eq!(user_criteria.len(), session.criteria.len());
    assert_eq!(*isolation, session.isolation);
    let _ = std::fs::remove_dir_all(repo);
}

#[test]
fn a_verification_without_a_verdict_keeps_its_request_as_it_was() {
    let session = crate::demo::held();
    assert!(session.verdict().is_none());
    assert_eq!(start::Form::from_session(&session).request, session.request);
}

#[test]
fn past_verifications_can_be_verified_again_from_the_list() {
    let mut screen = Verify::demo_start();
    let again: Vec<Action> = acts(&screen)
        .into_iter()
        .filter(|a| matches!(a, Action::VerifyAgain(Some(_))))
        .collect();
    let readable = screen.past.iter().filter(|l| l.summary.is_ok()).count();
    assert!(screen.past.len() > readable);
    assert_eq!(again.len(), readable);
    let Action::VerifyAgain(Some(dir)) = again[1].clone() else {
        unreachable!()
    };
    let stored = screen.load_stored(&dir).unwrap();
    act(&mut screen, again[1].clone());
    let form = start::read(&screen.tree, &screen.start);
    assert_eq!(form.kind, stored.target.kind);
    assert_eq!(form.panel, panel_of(&stored));
}

#[test]
fn a_refill_keeps_a_limit_raised_past_six() {
    let repo = temp("rounds");
    let mut session = demo_in(&repo);
    session.max_rounds = 9;
    let (mut screen, link) = live_with(session);
    act(&mut screen, Action::VerifyAgain(None));
    let form = start::read(&screen.tree, &screen.start);
    assert_eq!((form.max_rounds, form.round_cap), (9, 9));
    act(&mut screen, Action::Start);
    let started: Vec<Command> = commands(&link)
        .into_iter()
        .filter(|c| matches!(c, Command::Start { .. }))
        .collect();
    let [
        Command::Start {
            max_rounds,
            round_cap,
            ..
        },
    ] = started.as_slice()
    else {
        panic!("{started:?}");
    };
    assert_eq!((*max_rounds, *round_cap), (9, 9));
    act(&mut screen, Action::NewVerification);
    let form = start::read(&screen.tree, &screen.start);
    assert_eq!(form.round_cap, verify::model::types::MAX_MAX_ROUNDS);
    let _ = std::fs::remove_dir_all(repo);
}

// ---- --repo ----

#[test]
fn the_focused_panes_directory_fills_the_repository() {
    let (channel, _link) = Channel::fake();
    let screen = live_ready(channel).with_place(Path::new("/Users/me/src/noa"));
    let form = start::read(&screen.tree, &screen.start);
    assert_eq!(form.repo, "/Users/me/src/noa");
    let clean = start::Form::for_kinds(&AgentKind::ALL);
    assert_eq!(form.panel, clean.panel);
    assert_eq!(form.kind, clean.kind);
}

#[test]
fn the_place_stays_when_the_agent_check_reports_later() {
    let (screen, _link, tx, mut text) = live_pending();
    let mut screen = screen.with_place(Path::new("/Users/me/src/noa"));
    report(&mut screen, &tx, found(&[AgentKind::Codex]), &mut text);
    let form = start::read(&screen.tree, &screen.start);
    assert_eq!(form.repo, "/Users/me/src/noa");
    assert!(
        form.panel
            .iter()
            .all(|row| row.agent.kind == AgentKind::Codex)
    );
}

// ---- 過去の検証をさらに表示 ----

fn stored_many(name: &str, log: &[Vec<Event>], base: usize, n: usize) -> PathBuf {
    let root = temp(name);
    for i in 0..n {
        stored_log(&root, &format!("ver-{}", base + i), log);
    }
    root
}

fn past_screen(root: &Path) -> (Verify, FakeLink) {
    let (channel, link) = Channel::fake();
    let mut screen = live_ready(channel);
    screen.past_root = Some(root.to_owned());
    screen.refresh_past();
    screen.rebuild(&mut TextStore::new(Default::default()));
    (screen, link)
}

#[test]
fn past_verifications_beyond_the_first_page_can_be_listed() {
    let n = start::PAST_LIMIT + 2;
    let root = stored_many("past", &crate::demo::log(), 1_000, n);
    let (mut screen, _link) = past_screen(&root);
    let listed = |screen: &Verify| {
        acts(screen)
            .iter()
            .filter(|a| matches!(a, Action::VerifyAgain(Some(_))))
            .count()
    };
    assert_eq!(listed(&screen), start::PAST_LIMIT);
    let text = labels_under(&screen.tree, screen.start.root).join(" ");
    assert!(
        text.contains(&format!("全 {n} 件のうち新しい {} 件", start::PAST_LIMIT)),
        "{text}"
    );
    act(&mut screen, Action::MorePast);
    assert_eq!(listed(&screen), n);
    assert!(!acts(&screen).contains(&Action::MorePast));
    assert!(screen.past.last().unwrap().dir.ends_with("ver-1000"));
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn a_short_past_list_offers_no_more() {
    let screen = Verify::demo_start();
    assert!(!acts(&screen).contains(&Action::MorePast));
}

#[test]
fn a_past_verification_that_could_not_be_read_is_tried_again() {
    use std::os::unix::fs::PermissionsExt;
    let root = stored_many("unreadable", &crate::demo::log(), 5_000, 2);
    let log = root.join("ver-5001").join(store::EVENTS_FILE);
    std::fs::set_permissions(&log, std::fs::Permissions::from_mode(0o000)).unwrap();
    let (mut screen, _link) = past_screen(&root);
    let row = |screen: &Verify| {
        screen
            .past
            .iter()
            .find(|l| l.dir.ends_with("ver-5001"))
            .unwrap()
            .summary
            .is_ok()
    };
    assert!(!row(&screen));
    std::fs::set_permissions(&log, std::fs::Permissions::from_mode(0o644)).unwrap();
    screen.refresh_past();
    assert!(row(&screen));
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn the_start_view_scrolls_back_to_the_top_when_filled_again() {
    let mut screen = Verify::demo_start();
    screen
        .tree
        .update::<ScrollView, _>(screen.start.root, |s, cx| s.set_offset(cx, 2000.0));
    let again = acts(&screen)
        .into_iter()
        .find(|a| matches!(a, Action::VerifyAgain(Some(_))))
        .unwrap();
    press_button(&mut screen, &again);
    let offset = screen
        .tree
        .get::<ScrollView>(screen.start.root)
        .unwrap()
        .offset();
    assert_eq!(offset, 0.0);
}

// ---- more of review's, adapted ----

#[test]
fn a_shown_verification_whose_agent_is_gone_is_not_resumed_or_extended() {
    let root = temp("gone-shown");
    let dir = stored_unfinished(&root);
    let (channel, link) = Channel::fake();
    let (tx, rx) = std::sync::mpsc::channel();
    let mut screen = Verify::live_stored(channel, rx, &dir);
    let mut text = TextStore::new(Default::default());
    let _ = commands(&link);
    report(&mut screen, &tx, found(&[AgentKind::Claude]), &mut text);
    act(&mut screen, Action::ResumeShown);
    act(&mut screen, Action::Extend(1));
    assert!(commands(&link).is_empty());
    assert!(!screen.extending);
    screen.detection = start::Detection::all_installed();
    act(&mut screen, Action::ResumeShown);
    assert_eq!(commands(&link), [Command::Resume { dir }]);
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn viewing_a_stored_verification_changes_no_file_in_its_folder() {
    let root = temp("ro-files");
    let dir = stored_demo(&root);
    // A stale report and a stray file are kept as they are.
    std::fs::write(store::report_path(&dir), "old report").unwrap();
    std::fs::write(dir.join("stray.json"), "{}").unwrap();
    let before = files_with_times(&dir);
    std::thread::sleep(std::time::Duration::from_millis(20));
    let _ = Verify::stored(&dir).unwrap();
    let (channel, _link) = Channel::fake();
    let screen = stored_ready(channel, &dir);
    assert!(acts(&screen).contains(&Action::RegenerateReport));
    assert_eq!(files_with_times(&dir), before);
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn a_change_that_could_not_be_read_is_read_at_a_later_look() {
    use std::os::unix::fs::PermissionsExt;
    let root = temp("unread-change");
    let dir = stored_unfinished(&root);
    let (channel, _link) = Channel::fake();
    let mut screen = stored_ready(channel, &dir);
    rewrite_log(&dir, &held_finished_log());
    let log = dir.join(store::EVENTS_FILE);
    std::fs::set_permissions(&log, std::fs::Permissions::from_mode(0o000)).unwrap();
    look_again(&mut screen);
    assert!(!screen.session.as_ref().unwrap().closed);
    std::fs::set_permissions(&log, std::fs::Permissions::from_mode(0o644)).unwrap();
    look_again(&mut screen);
    assert!(screen.session.as_ref().unwrap().closed);
    assert!(!acts(&screen).contains(&Action::ResumeShown));
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn a_report_written_before_the_writer_let_go_is_no_longer_offered_again() {
    let root = temp("late-report");
    let dir = stored_demo(&root);
    let (held, session) = Store::open(&dir).unwrap();
    let (channel, _link) = Channel::fake();
    let mut screen = stored_ready(channel, &dir);
    assert!(screen.report_stale);
    store::write_report(&dir, &store::report_text(&session)).unwrap();
    drop(held);
    look_again(&mut screen);
    assert!(screen.stored_free);
    assert!(!acts(&screen).contains(&Action::RegenerateReport));
    let _ = std::fs::remove_dir_all(root);
}

fn page_to_the_end(screen: &mut Verify, limit: usize) {
    for _ in 0..limit {
        if !acts(screen).contains(&Action::MorePast) {
            return;
        }
        act(screen, Action::MorePast);
    }
    panic!("さらに表示 never went away");
}

#[test]
fn more_past_verifications_reread_a_changed_log_and_ask_the_lock_again() {
    let n = start::PAST_LIMIT + 3;
    let root = stored_many("paging", &crate::demo::held_log(), 2_000, n);
    let (mut screen, _link) = past_screen(&root);
    let first = screen.past[0].dir.clone();
    assert!(!screen.past[0].summary.as_ref().unwrap().closed);
    rewrite_log(&first, &held_finished_log());
    let (_held, _) = Store::open(&screen.past[1].dir).unwrap();
    act(&mut screen, Action::MorePast);
    assert_eq!(screen.past.len(), n);
    assert!(screen.past[0].summary.as_ref().unwrap().closed);
    assert!(!acts(&screen).contains(&Action::ResumePast(first)));
    assert!(screen.past[1].summary.as_ref().unwrap().busy);
    let oldest = screen.past.last().unwrap().dir.clone();
    assert!(oldest.ends_with("ver-2000"));
    assert!(acts(&screen).contains(&Action::ResumePast(oldest)));
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn past_verifications_added_or_gone_between_pages_end_without_gaps_or_doubles() {
    let log = crate::demo::log();
    let n = 2 * start::PAST_LIMIT + 1;
    let root = stored_many("paging-new", &log, 3_000, n);
    let (mut screen, _link) = past_screen(&root);
    stored_log(&root, "ver-9999", &log);
    page_to_the_end(&mut screen, 5);
    let dirs: Vec<PathBuf> = screen.past.iter().map(|l| l.dir.clone()).collect();
    assert_eq!(dirs.len(), n + 1);
    assert_eq!(screen.past_total, n + 1);
    let unique: BTreeSet<_> = dirs.iter().collect();
    assert_eq!(unique.len(), dirs.len());
    let _ = std::fs::remove_dir_all(&root);

    let root = stored_many("paging-gone", &log, 4_000, n);
    let (mut screen, _link) = past_screen(&root);
    let last = screen.past.last().unwrap().dir.clone();
    std::fs::remove_dir_all(&last).unwrap();
    page_to_the_end(&mut screen, 5);
    let dirs: Vec<PathBuf> = screen.past.iter().map(|l| l.dir.clone()).collect();
    assert_eq!(dirs.len(), n - 1);
    assert!(!dirs.contains(&last));
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn focus_on_the_second_of_two_alike_buttons_stays_on_the_second() {
    // A selected criterion's row and the thread's 選択を解除 both clear the
    // selection; focus on the latter stays there through a rebuild.
    let (mut screen, link) = live_with(crate::demo::held());
    act(&mut screen, Action::SelectCriterion(Some("AC-2".into())));
    let clear = Action::SelectCriterion(None);
    let alike = |screen: &Verify| -> Vec<WidgetId> {
        screen
            .actions
            .iter()
            .filter(|(_, a)| *a == clear)
            .map(|(id, _)| *id)
            .collect()
    };
    assert_eq!(alike(&screen).len(), 2);
    screen.tree.set_focus(Some(alike(&screen)[1]));
    let mut next = crate::demo::held();
    next.rev += 1;
    link.snapshots.send(snapshot(next, None, 0)).unwrap();
    screen.poll_live(&mut TextStore::new(Default::default()));
    assert_eq!(screen.tree.focused(), Some(alike(&screen)[1]));
}

#[test]
fn a_new_draft_survives_resending_an_instruction_already_in_the_log() {
    let earlier = "AC-2 も見て";
    let (mut screen, link) = live_with(with_intervention(crate::demo::held(), earlier));
    type_draft(&mut screen, earlier);
    act(&mut screen, Action::Send);
    assert_eq!(commands(&link).len(), 1);
    type_draft(&mut screen, "次は AC-5 も");
    act(&mut screen, Action::ShowTab(CenterTab::Defects));
    assert_eq!(draft(&screen), "次は AC-5 も");
}

#[test]
fn a_filter_that_hides_the_selected_defect_lets_it_go() {
    let (mut screen, _link) = live_with_dirs(crate::demo::session(), Path::new(REPLY));
    act(&mut screen, Action::SelectDefect(Some("D2".into())));
    act(&mut screen, Action::ToggleSeverity(Severity::High));
    assert_eq!(screen.selected, None);
    assert!(
        fix_actions(&screen)
            .iter()
            .all(|(_, s)| *s != FixScope::Defect("D2".into()))
    );
    act(&mut screen, Action::SelectDefect(Some("D1".into())));
    act(&mut screen, Action::ToggleCategory("conformance".into()));
    assert_eq!(screen.selected, Some(Selected::Defect("D1".into())));
}
