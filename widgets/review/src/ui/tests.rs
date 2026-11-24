use super::*;

use std::cell::RefCell;

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

use review::model::types::{AgentKind, LensChoice, MIN_PANEL, ReviewerChoice, TargetKind};

use crate::installed::Installed;
use review::store::Store;

fn acts(screen: &Review) -> Vec<Action> {
    screen.actions.iter().map(|(_, a)| a.clone()).collect()
}

fn widget_for(screen: &Review, action: &Action) -> Option<WidgetId> {
    screen
        .actions
        .iter()
        .find(|(_, a)| a == action)
        .map(|(id, _)| *id)
}

fn act(screen: &mut Review, action: Action) {
    let mut text = TextStore::new(Default::default());
    screen.handle_action(action, &mut text);
}

fn rows(screen: &Review) -> usize {
    acts(screen)
        .iter()
        .filter(|a| matches!(a, Action::SelectFinding(Some(_))))
        .count()
}

/// The finished demo review written as a stored log under `root`.
fn stored_demo(root: &Path) -> PathBuf {
    let dir = root.join("rev-demo");
    let _ = std::fs::remove_dir_all(&dir);
    let log = crate::demo::log();
    let mut store = Store::create(&dir, &log[0]).unwrap();
    store.append_all(&log[1..]).unwrap();
    dir
}

#[test]
fn ac24_the_start_view_offers_the_three_targets_the_panel_and_past_reviews() {
    let screen = Review::demo_start();
    assert!(!screen.tree.is_hidden(screen.start.root));
    assert!(screen.tree.is_hidden(screen.board));
    let a = acts(&screen);
    assert!(a.contains(&Action::Start));
    assert!(a.contains(&Action::AddPanelRow));
    assert!(a.iter().any(|a| matches!(a, Action::ResumePast(_))));
    assert!(a.iter().any(|a| matches!(a, Action::OpenPast(_))));
    let form = start::read(&screen.tree, &screen.start);
    assert_eq!(form.kind, TargetKind::Git);
    assert_eq!(form.panel.len(), 3);
    assert_eq!(
        form.panel[2].lens,
        LensChoice::Custom("API 互換性と移行手順".into())
    );
    assert_eq!(form.lead.setup.model.as_deref(), Some("opus"));
}

#[test]
fn panel_rows_stop_at_six_and_at_two() {
    let mut screen = Review::demo_start();
    for _ in 0..5 {
        act(&mut screen, Action::AddPanelRow);
    }
    assert_eq!(screen.start.panel.len(), 6);
    assert!(!screen.tree.is_enabled(screen.start.add_row));
    let form = start::read(&screen.tree, &screen.start);
    // New rows take the lenses nobody holds yet.
    let lenses: Vec<LensChoice> = form.panel[3..].iter().map(|r| r.lens.clone()).collect();
    assert!(lenses.contains(&LensChoice::Builtin(
        review::model::types::BuiltinLens::Performance
    )));
    for _ in 0..6 {
        act(&mut screen, Action::RemovePanelRow(0));
    }
    assert_eq!(screen.start.panel.len(), 2);
}

#[test]
fn ac25_each_filter_changes_the_shown_count() {
    let mut screen = Review::demo();
    // Default: live findings (open, agreed, disputed).
    assert_eq!(rows(&screen), 4);
    act(&mut screen, Action::ToggleSeverity(Severity::Critical));
    assert_eq!(rows(&screen), 1);
    act(&mut screen, Action::ToggleSeverity(Severity::Critical));
    act(&mut screen, Action::ToggleLens("correctness".into()));
    assert_eq!(rows(&screen), 2);
    act(&mut screen, Action::ToggleLens("correctness".into()));
    act(&mut screen, Action::ToggleStatus(FindingStatus::Rejected));
    assert_eq!(rows(&screen), 5, "rejected F4 shows too");
    act(&mut screen, Action::ToggleStatus(FindingStatus::Rejected));
    act(&mut screen, Action::ToggleAgreement(Agreement::Split));
    assert_eq!(rows(&screen), 0);
    act(&mut screen, Action::ToggleAgreement(Agreement::Split));
    act(&mut screen, Action::ClearFilters);
    assert_eq!(rows(&screen), 5);

    // In the finished demo every reviewer spoke on every finding, so the
    // reviewer filter is checked after round 1, where agy raised one.
    let (mut screen, _link) = live_with(crate::demo::held());
    assert_eq!(rows(&screen), 4);
    act(
        &mut screen,
        Action::FilterReviewer(Some(PartId::new("agy"))),
    );
    assert_eq!(rows(&screen), 1, "only the finding agy raised");
    act(&mut screen, Action::FilterReviewer(None));
    assert_eq!(rows(&screen), 4);
}

#[test]
fn ac25_a_matrix_cell_opens_the_board_filtered_and_a_row_opens_its_thread() {
    let mut screen = Review::demo().with_view(Some(CenterTab::Matrix), None);
    assert!(acts(&screen).contains(&Action::ShowCell(
        "security".into(),
        Some(Severity::Critical)
    )));
    act(
        &mut screen,
        Action::ShowCell("security".into(), Some(Severity::Critical)),
    );
    assert_eq!(screen.tab, CenterTab::Findings);
    assert_eq!(rows(&screen), 1);
    act(&mut screen, Action::SelectFinding(Some("F2".into())));
    assert_eq!(screen.selected.as_deref(), Some("F2"));
    assert!(acts(&screen).contains(&Action::SelectFinding(None)));
}

#[test]
fn ac26_a_stored_review_opens_read_only_without_composer_or_pane_buttons() {
    let root = std::env::temp_dir().join(format!("noa-review-ui-{}", std::process::id()));
    let dir = stored_demo(&root);
    let before = std::fs::read(dir.join(store::EVENTS_FILE)).unwrap();
    let screen = Review::stored(&dir).unwrap();
    // AC-23: a read-only view writes nothing into the session's directory.
    assert!(!store::report_path(&dir).exists());
    assert_eq!(std::fs::read(dir.join(store::EVENTS_FILE)).unwrap(), before);
    assert!(screen.read_only);
    assert!(screen.tree.is_hidden(screen.composer.root));
    let a = acts(&screen);
    assert!(!a.iter().any(|a| matches!(a, Action::FocusPane(_))));
    assert!(!a.contains(&Action::Close));
    // It ran all its rounds, and no runner holds it: rounds can be added.
    assert!(a.contains(&Action::Extend(1)));
    assert_eq!(screen.session.as_ref().unwrap().rounds.len(), 3);
    let _ = std::fs::remove_dir_all(root);
    // `REVIEW_FIXTURE_OUT=DIR` leaves the stored log in DIR/rev-demo, for
    // rendering it with `review --session=… --snapshot=…` by hand.
    if let Some(out) = std::env::var_os("REVIEW_FIXTURE_OUT") {
        stored_demo(Path::new(&out));
    }
}

#[test]
fn an_unreadable_log_is_refused() {
    let root = std::env::temp_dir().join(format!("noa-review-ui-bad-{}", std::process::id()));
    let dir = root.join("rev-x");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(store::EVENTS_FILE), "nope\n").unwrap();
    assert!(Review::stored(&dir).err().unwrap().contains("を開けません"));
    let _ = std::fs::remove_dir_all(root);
}

/// Feeds `session` to a live screen as its runner would.
fn live_with(session: Session) -> (Review, crate::live::FakeLink) {
    let (channel, link) = Channel::fake();
    let mut screen = live_ready(channel);
    link.snapshots
        .send(Snapshot {
            link: Link::Live,
            session: Some(Arc::new(session)),
            reply_root: None,
            session_dir: None,
            generation: 0,
        })
        .unwrap();
    let mut text = TextStore::new(Default::default());
    screen.poll_live(&mut text);
    (screen, link)
}

fn commands(link: &crate::live::FakeLink) -> Vec<Command> {
    link.commands.try_iter().map(|(_, c)| c).collect()
}

#[test]
fn round_controls_and_pane_jumps_reach_the_runner() {
    let (mut screen, link) = live_with(crate::demo::held());
    let a = acts(&screen);
    for part in ["claude", "codex", "agy", "lead"] {
        assert!(a.contains(&Action::FocusPane(PartId::new(part))), "{part}");
    }
    assert!(widget_for(&screen, &Action::Continue).is_some());
    act(&mut screen, Action::Continue);
    act(&mut screen, Action::SetHold(false));
    act(&mut screen, Action::Extend(3));
    act(&mut screen, Action::FocusPane(PartId::new("codex")));
    assert_eq!(
        commands(&link),
        [
            Command::Continue,
            Command::SetHold { hold: false },
            Command::Extend { by: 3 },
            Command::Focus {
                part: PartId::new("codex")
            },
        ]
    );
    assert!(!screen.tree.is_hidden(screen.composer.root));
}

#[test]
fn a_refused_start_shows_why_under_the_form() {
    let (channel, link) = Channel::fake();
    let mut screen = live_ready(channel);
    // The form's repository is empty: refused before anything is sent.
    act(&mut screen, Action::Start);
    assert!(commands(&link).is_empty());
    assert!(!screen.starting);
    let mut text = TextStore::new(Default::default());
    let form = start::Form {
        kind: TargetKind::Document,
        text: "plan".into(),
        ..start::Form::default()
    };
    start::fill(&mut screen.tree, &mut screen.start, &form, &mut text);
    act(&mut screen, Action::Start);
    assert!(matches!(&commands(&link)[..], [Command::Start { .. }]));
    assert!(screen.starting);
    link.notices.send((0, "対象が大きすぎます".into())).unwrap();
    // The runner that refused it let go of nothing, but says it stopped.
    link.released.send(()).unwrap();
    screen.poll_live(&mut text);
    assert!(!screen.starting);
    assert!(!screen.tree.is_hidden(screen.start.root));
}

/// The demo review's log up to the end of round 1 (unfinished), stored
/// under `root`.
fn stored_unfinished(root: &Path) -> PathBuf {
    let dir = root.join("rev-demo");
    let _ = std::fs::remove_dir_all(&dir);
    let log = crate::demo::log();
    let mut store = Store::create(&dir, &log[0]).unwrap();
    let mut session = review::model::reduce::create(&log[0]).unwrap();
    for batch in &log[1..] {
        review::model::reduce::apply(&mut session, batch).unwrap();
        store.append(batch).unwrap();
        if session.round(1).is_some_and(|r| r.outcome.is_some()) {
            break;
        }
    }
    assert!(!session.closed);
    dir
}

#[test]
fn ac20_an_unfinished_review_is_resumed_from_its_read_only_view_and_goes_live() {
    let root = std::env::temp_dir().join(format!("noa-review-ui-resume-{}", std::process::id()));
    let dir = stored_unfinished(&root);
    let (channel, link) = Channel::fake();
    let mut screen = stored_ready(channel, &dir);
    assert!(screen.read_only);
    assert!(acts(&screen).contains(&Action::ResumeShown));
    // Opening it left whatever was on screen before.
    assert_eq!(commands(&link), [Command::Leave]);
    act(&mut screen, Action::ResumeShown);
    assert_eq!(commands(&link), [Command::Resume { dir: dir.clone() }]);
    assert!(
        !acts(&screen).contains(&Action::ResumeShown),
        "offered twice"
    );
    // The runner's first snapshot of it turns the view live.
    let session = store::read_session(&dir).unwrap();
    link.snapshots
        .send(Snapshot {
            link: Link::Live,
            session: Some(Arc::new(session)),
            reply_root: None,
            session_dir: None,
            // The view's own leave moved the channel on one generation.
            generation: 1,
        })
        .unwrap();
    let mut text = TextStore::new(Default::default());
    screen.poll_live(&mut text);
    assert!(!screen.read_only);
    assert!(acts(&screen).contains(&Action::Close));
    // A finished review is not offered it.
    let done = stored_demo(&root);
    let (channel, _link) = Channel::fake();
    let screen = stored_ready(channel, &done);
    assert!(!acts(&screen).contains(&Action::ResumeShown));
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn ac23_a_missing_report_is_rewritten_from_the_log_only_when_asked() {
    let root = std::env::temp_dir().join(format!("noa-review-ui-report-{}", std::process::id()));
    let dir = stored_demo(&root);
    let (channel, _link) = Channel::fake();
    let mut screen = stored_ready(channel, &dir);
    // Viewing writes nothing, even with the report missing.
    assert!(!store::report_path(&dir).exists());
    assert!(acts(&screen).contains(&Action::RegenerateReport));
    act(&mut screen, Action::RegenerateReport);
    let session = store::read_session(&dir).unwrap();
    assert_eq!(
        std::fs::read_to_string(store::report_path(&dir)).unwrap(),
        export::report_markdown(&session)
    );
    assert!(!acts(&screen).contains(&Action::RegenerateReport));
    assert!(!store::is_busy(&dir), "the log stayed locked");
    // A report that matches the log is not offered again.
    let (channel, _link) = Channel::fake();
    let screen = stored_ready(channel, &dir);
    assert!(!acts(&screen).contains(&Action::RegenerateReport));
    let _ = std::fs::remove_dir_all(root);
}

/// Every label's and tag's text in `root`'s subtree, in tree order.
fn labels_under(tree: &UiTree, root: WidgetId) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack = vec![root];
    while let Some(id) = stack.pop() {
        if let Some(label) = tree.get::<noa_gpui_core::widgets::Label>(id) {
            out.push(label.text().to_owned());
        } else if let Some(tag) = tree.get::<noa_gpui_core::widgets::Tag>(id) {
            out.push(tag.text().to_owned());
        }
        stack.extend(tree.children(id).iter().rev());
    }
    out
}

#[test]
fn an_ended_review_lists_raw_findings_the_lead_never_consolidated_on_its_board() {
    let mut session = crate::demo::held();
    let mut raw = session.raw[0].clone();
    raw.key = review::model::types::RawKey {
        part: PartId::new("agy"),
        id: "9".into(),
    };
    raw.round = 2;
    raw.claim = "未整理のままの指摘".into();
    session.raw.push(raw);
    let (screen, _link) = live_with(session.clone());
    let shown = labels_under(&screen.tree, screen.center_host);
    assert!(
        !shown.iter().any(|l| l.contains("未整理のままの指摘")),
        "listed while live"
    );
    session.closed = true;
    let (screen, _link) = live_with(session);
    let shown = labels_under(&screen.tree, screen.center_host);
    assert!(
        shown
            .iter()
            .any(|l| l.contains("取りまとめられずに終わった指摘 1 件")),
        "{shown:#?}"
    );
    assert!(
        shown.iter().any(|l| l == "未整理のままの指摘"),
        "{shown:#?}"
    );
}

#[test]
fn the_matrix_always_has_its_other_row_and_greys_every_lens_nobody_holds() {
    let mut session = crate::demo::held();
    // A finding in a lens nobody holds.
    let unheld = session.unheld_lenses()[0];
    session.findings[0].category = unheld.id().to_owned();
    let (mut screen, _link) = live_with(session);
    act(&mut screen, Action::ShowTab(CenterTab::Matrix));
    let shown = labels_under(&screen.tree, screen.center_host);
    assert!(shown.iter().any(|l| l == "その他"), "{shown:#?}");
    let tags = shown.iter().filter(|l| *l == "担当なし").count();
    assert_eq!(tags, 6 - 3, "every unheld lens is tagged: {shown:#?}");
}

#[test]
fn ac13_the_board_hides_a_merged_finding_until_merged_is_asked_for() {
    let mut session = crate::demo::held();
    let id = session.findings[1].id.clone();
    let into = session.findings[0].id.clone();
    session.findings[1].status = FindingStatus::Merged;
    session.findings[1].merged_into = Some(into);
    let (mut screen, _link) = live_with(session);
    let shown = |screen: &Review| {
        acts(screen)
            .into_iter()
            .filter_map(|a| match a {
                Action::SelectFinding(Some(f)) => Some(f),
                _ => None,
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(rows(&screen), 3);
    assert!(!shown(&screen).contains(&id));
    act(&mut screen, Action::ToggleStatus(FindingStatus::Merged));
    assert!(shown(&screen).contains(&id));
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

#[test]
fn ac23_viewing_a_stored_review_changes_no_file_in_its_folder() {
    let root = std::env::temp_dir().join(format!("noa-review-ui-ro-{}", std::process::id()));
    let dir = stored_demo(&root);
    // A stale report and a reply left by an agent are kept as they are.
    std::fs::write(store::report_path(&dir), "old report").unwrap();
    std::fs::create_dir_all(dir.join("r3")).unwrap();
    std::fs::write(dir.join("r3/claude.json"), "{}").unwrap();
    let before = files_with_times(&dir);
    std::thread::sleep(std::time::Duration::from_millis(20));
    let _ = Review::stored(&dir).unwrap();
    let (channel, _link) = Channel::fake();
    let screen = stored_ready(channel, &dir);
    assert!(acts(&screen).contains(&Action::RegenerateReport));
    assert_eq!(files_with_times(&dir), before);
    let _ = std::fs::remove_dir_all(root);
}

/// A live screen whose check already reported every kind installed.
fn live_ready(channel: Channel) -> Review {
    let mut screen = Review::live(channel, installed_all());
    screen.poll_live(&mut TextStore::new(Default::default()));
    screen
}

/// The same, showing the review stored in `dir`.
fn stored_ready(channel: Channel, dir: &Path) -> Review {
    let mut screen = Review::live_stored(channel, installed_all(), dir);
    screen.poll_live(&mut TextStore::new(Default::default()));
    screen
}

/// A report that every kind is installed, already delivered.
fn installed_all() -> std::sync::mpsc::Receiver<Installed> {
    let (tx, rx) = std::sync::mpsc::channel();
    tx.send(Installed::Found(AgentKind::ALL.to_vec())).unwrap();
    rx
}

/// A live screen still waiting for the installed-agents report, and the
/// end that delivers it.
fn live_pending() -> (
    Review,
    crate::live::FakeLink,
    std::sync::mpsc::Sender<Installed>,
    TextStore,
) {
    let (channel, link) = Channel::fake();
    let (tx, rx) = std::sync::mpsc::channel();
    let screen = Review::live(channel, rx);
    (screen, link, tx, TextStore::new(Default::default()))
}

/// `screen` once the check reported `installed`.
fn report(
    screen: &mut Review,
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

/// How many entries `popup` has, by walking down its menu with the
/// keyboard to the end.
fn menu_len(screen: &mut Review, popup: WidgetId, text: &mut TextStore) -> usize {
    screen.tree.set_focus(Some(popup));
    for _ in 0..AgentKind::ALL.len() + 2 {
        screen.dispatch(
            noa_gpui_core::Event::KeyDown {
                key: noa_gpui_core::Key::ArrowDown,
                modifiers: Default::default(),
            },
            text,
        );
    }
    screen
        .tree
        .get::<noa_gpui_core::widgets::PopUpButton>(popup)
        .unwrap()
        .selected()
        + 1
}

fn press_down(screen: &mut Review, popup: WidgetId, text: &mut TextStore) {
    screen.tree.set_focus(Some(popup));
    screen.dispatch(
        noa_gpui_core::Event::KeyDown {
            key: noa_gpui_core::Key::ArrowDown,
            modifiers: Default::default(),
        },
        text,
    );
}

fn kinds_of(panel: &[ReviewerChoice]) -> Vec<AgentKind> {
    panel.iter().map(|row| row.agent.kind).collect()
}

/// A document form that passes the start checks.
fn document_form() -> start::Form {
    start::Form {
        kind: TargetKind::Document,
        text: "plan".into(),
        ..start::Form::default()
    }
}

#[test]
fn the_start_button_waits_for_the_installed_check() {
    let (mut screen, link, tx, mut text) = live_pending();
    let start_button = screen.start.start_button;
    assert!(!screen.tree.is_enabled(start_button));
    // A start that gets through anyway sends nothing.
    let form = document_form();
    start::fill(&mut screen.tree, &mut screen.start, &form, &mut text);
    act(&mut screen, Action::Start);
    assert!(commands(&link).is_empty());
    assert!(!screen.starting);

    report(&mut screen, &tx, found(&[AgentKind::Codex]), &mut text);
    assert!(screen.tree.is_enabled(start_button));
    act(&mut screen, Action::Start);
    assert!(matches!(&commands(&link)[..], [Command::Start { .. }]));
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
    let form = document_form();
    start::fill(&mut screen.tree, &mut screen.start, &form, &mut text);
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
    let (lead, row) = (screen.start.lead, screen.start.panel[0].kind);
    assert_eq!(menu_len(&mut screen, lead, &mut text), AgentKind::ALL.len());
    assert_eq!(menu_len(&mut screen, row, &mut text), AgentKind::ALL.len());
    assert!(
        screen
            .detection
            .blocked_reason()
            .contains("すべて表示しています")
    );
}

#[test]
fn menus_offer_only_the_installed_kinds_and_map_by_position() {
    let (mut screen, link, tx, mut text) = live_pending();
    report(
        &mut screen,
        &tx,
        found(&[AgentKind::Codex, AgentKind::Agy]),
        &mut text,
    );
    // The untouched form is led by the first installed kind, one row each.
    let form = start::read(&screen.tree, &screen.start);
    assert_eq!(form.lead.kind, AgentKind::Codex);
    assert_eq!(kinds_of(&form.panel), [AgentKind::Codex, AgentKind::Agy]);
    let (lead, row) = (screen.start.lead, screen.start.panel[1].kind);
    assert_eq!(menu_len(&mut screen, lead, &mut text), 2);
    assert_eq!(menu_len(&mut screen, row, &mut text), 2);
    // Menu position 1 is agy here, not codex (ALL's second entry).
    let form = start::read(&screen.tree, &screen.start);
    assert_eq!(form.lead.kind, AgentKind::Agy);
    assert_eq!(form.panel[1].agent.kind, AgentKind::Agy);
    // A row added takes the kind of the last one.
    act(&mut screen, Action::AddPanelRow);
    let form = start::read(&screen.tree, &screen.start);
    assert_eq!(form.panel[2].agent.kind, AgentKind::Agy);
    // Starting sends only installed kinds.
    let form = document_form();
    start::fill(&mut screen.tree, &mut screen.start, &form, &mut text);
    act(&mut screen, Action::Start);
    let Some(Command::Start { lead, panel, .. }) = commands(&link).into_iter().next() else {
        panic!("not started");
    };
    assert_eq!(lead.kind, AgentKind::Codex);
    assert!(
        kinds_of(&panel)
            .iter()
            .all(|k| [AgentKind::Codex, AgentKind::Agy].contains(k))
    );
}

#[test]
fn the_lead_row_offers_only_the_installed_kinds() {
    let (mut screen, _link, tx, mut text) = live_pending();
    report(&mut screen, &tx, found(&[AgentKind::Agy]), &mut text);
    let lead = screen.start.lead;
    assert_eq!(menu_len(&mut screen, lead, &mut text), 1);
    let form = start::read(&screen.tree, &screen.start);
    assert_eq!(form.lead.kind, AgentKind::Agy);
    // One kind installed still fills a panel of the minimum size.
    assert_eq!(kinds_of(&form.panel), [AgentKind::Agy, AgentKind::Agy]);
    assert_ne!(form.panel[0].lens, form.panel[1].lens);
    // The lead's pop-up change is read through the offered list.
    press_down(&mut screen, lead, &mut text);
    assert_eq!(
        start::read(&screen.tree, &screen.start).lead.kind,
        AgentKind::Agy
    );
}

#[test]
fn a_report_after_the_form_was_changed_keeps_what_is_left_and_says_what_went() {
    let (mut screen, _link, tx, mut text) = live_pending();
    // The user picked codex for the lead before the check came back.
    let lead = screen.start.lead;
    press_down(&mut screen, lead, &mut text);
    assert_eq!(
        start::read(&screen.tree, &screen.start).lead.kind,
        AgentKind::Codex
    );
    report(
        &mut screen,
        &tx,
        found(&[AgentKind::Claude, AgentKind::Agy]),
        &mut text,
    );
    let form = start::read(&screen.tree, &screen.start);
    assert_eq!(form.lead.kind, AgentKind::Claude, "codex is not installed");
    assert!(kinds_of(&form.panel).iter().all(|k| *k != AgentKind::Codex));
    assert!(form.panel.len() >= MIN_PANEL);
}

#[test]
fn the_form_a_clean_review_gets_has_a_row_per_available_kind_and_at_least_two() {
    assert_eq!(kinds_of(&start::Form::default().panel), AgentKind::ALL);
    let form = start::Form::for_kinds(&[AgentKind::Codex, AgentKind::Agy]);
    assert_eq!(kinds_of(&form.panel), [AgentKind::Codex, AgentKind::Agy]);
    assert_eq!(form.lead.kind, AgentKind::Codex);
    let form = start::Form::for_kinds(&[AgentKind::Agy]);
    assert_eq!(kinds_of(&form.panel), [AgentKind::Agy, AgentKind::Agy]);
}

#[test]
fn a_new_review_starts_from_the_installed_kinds() {
    let (mut screen, _link, tx, mut text) = live_pending();
    report(&mut screen, &tx, found(&[AgentKind::Codex]), &mut text);
    act(&mut screen, Action::NewReview);
    let form = start::read(&screen.tree, &screen.start);
    assert_eq!(form.lead.kind, AgentKind::Codex);
    assert_eq!(kinds_of(&form.panel), [AgentKind::Codex, AgentKind::Codex]);
}

#[test]
fn a_past_review_whose_agent_is_gone_says_so_instead_of_resuming() {
    let root = std::env::temp_dir().join(format!("noa-review-ui-gone-{}", std::process::id()));
    let dir = stored_unfinished(&root);
    let session = store::read_session(&dir).unwrap();
    let kinds: Vec<AgentKind> = std::iter::once(&session.lead)
        .chain(&session.reviewers)
        .map(|p| p.kind)
        .collect();
    assert!(kinds.contains(&AgentKind::Codex), "the demo uses codex");

    let (mut screen, link, tx, mut text) = live_pending();
    // Still checking: nothing is sent.
    act(&mut screen, Action::ResumePast(dir.clone()));
    assert!(commands(&link).is_empty());
    // Codex missing: refused, with its name.
    report(
        &mut screen,
        &tx,
        found(&[AgentKind::Claude, AgentKind::Agy]),
        &mut text,
    );
    act(&mut screen, Action::ResumePast(dir.clone()));
    assert!(commands(&link).is_empty());
    assert!(
        screen
            .detection
            .resume_blocker(&session)
            .is_some_and(|why| why.contains("codex") && !why.contains("agy"))
    );
    // Everything installed: it resumes.
    screen.detection = start::Detection::all_installed();
    act(&mut screen, Action::ResumePast(dir.clone()));
    assert_eq!(commands(&link), [Command::Resume { dir: dir.clone() }]);
    // A check that could not tell does not hold it back either.
    screen.detection = start::Detection::Done(Installed::Unknown("x".into()));
    assert!(screen.detection.resume_blocker(&session).is_none());
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn a_shown_review_whose_agent_is_gone_is_not_resumed_or_extended() {
    let root = std::env::temp_dir().join(format!("noa-review-ui-gone2-{}", std::process::id()));
    let dir = stored_unfinished(&root);
    let (channel, link) = Channel::fake();
    let (tx, rx) = std::sync::mpsc::channel();
    let mut screen = Review::live_stored(channel, rx, &dir);
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

/// The smallest window the app allows, in logical pixels.
const SMALLEST: (f32, f32) = (MIN_WINDOW.0 as f32, MIN_WINDOW.1 as f32);

/// Lays `screen` out in a window of `size`, as `--snapshot` does.
fn lay_out(screen: &mut Review, size: (f32, f32)) {
    let mut text = TextStore::new(Default::default());
    let viewport = noa_gpui_scene::Size::new(size.0, size.1);
    noa_gpui_platform::Screen::prepare(screen, viewport, &mut text);
}

/// What under `host` reaches past its right edge: cut off where the pane
/// ends.
fn cut_off(screen: &Review, host: WidgetId) -> Vec<WidgetId> {
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

#[test]
fn the_progress_strip_fits_the_smallest_window() {
    for mut screen in [Review::demo(), Review::demo_held()] {
        lay_out(&mut screen, SMALLEST);
        assert_eq!(cut_off(&screen, screen.progress_host), vec![]);
    }
}

fn press(screen: &mut Review, key: noa_gpui_core::Key) {
    let mut text = TextStore::new(Default::default());
    noa_gpui_platform::Screen::dispatch(
        screen,
        noa_gpui_core::Event::KeyDown {
            key,
            modifiers: Default::default(),
        },
        &mut text,
    );
}

fn is_selected(screen: &Review, action: &Action) -> bool {
    let id = widget_for(screen, action).unwrap();
    screen
        .tree
        .get::<noa_gpui_core::widgets::Button>(id)
        .unwrap()
        .is_selected()
}

#[test]
fn return_on_a_held_review_goes_to_the_next_round() {
    let (mut screen, link) = live_with(crate::demo::held());
    press(&mut screen, noa_gpui_core::Key::Enter);
    assert_eq!(commands(&link), vec![Command::Continue]);
}

#[test]
fn return_on_a_running_review_sends_the_draft() {
    let mut running = crate::demo::held();
    running.hold = false;
    let (mut screen, link) = live_with(running);
    screen
        .tree
        .update::<noa_gpui_core::widgets::TextArea, _>(screen.composer.area, |a, cx| {
            a.set_text(cx, "認可も見てください")
        });
    press(&mut screen, noa_gpui_core::Key::Enter);
    assert!(matches!(
        commands(&link).as_slice(),
        [Command::Intervene { body, .. }] if body == "認可も見てください"
    ));
}

#[test]
fn the_shown_tab_and_the_filters_on_are_selected_buttons() {
    let mut screen = Review::demo();
    assert!(is_selected(&screen, &Action::ShowTab(CenterTab::Findings)));
    assert!(!is_selected(&screen, &Action::ShowTab(CenterTab::Matrix)));
    assert!(is_selected(
        &screen,
        &Action::ToggleStatus(FindingStatus::Agreed)
    ));
    assert!(!is_selected(
        &screen,
        &Action::ToggleSeverity(Severity::Critical)
    ));
    act(&mut screen, Action::ToggleSeverity(Severity::Critical));
    assert!(is_selected(
        &screen,
        &Action::ToggleSeverity(Severity::Critical)
    ));
}

/// Presses the focused widget with Space and returns the action the widget
/// focused afterwards carries.
fn press_focused(screen: &mut Review, action: &Action) -> Option<Action> {
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
    let mut screen = Review::demo();
    let critical = Action::ToggleSeverity(Severity::Critical);
    assert_eq!(
        press_focused(&mut screen, &critical),
        Some(critical.clone())
    );
    assert!(screen.filters.severities.contains(&Severity::Critical));
    let matrix = Action::ShowTab(CenterTab::Matrix);
    assert_eq!(press_focused(&mut screen, &matrix), Some(matrix));
    let findings = Action::ShowTab(CenterTab::Findings);
    assert_eq!(press_focused(&mut screen, &findings), Some(findings));
    // A finding's row: selecting and clearing keep focus on that row.
    let f2 = Action::SelectFinding(Some("F2".into()));
    let selected = Action::SelectFinding(None);
    assert_eq!(press_focused(&mut screen, &f2), Some(selected.clone()));
    assert_eq!(screen.selected.as_deref(), Some("F2"));
    assert_eq!(press_focused(&mut screen, &selected), Some(f2));
    assert_eq!(screen.selected, None);
}

/// The tags' texts under `host`, in tree order.
fn tag_texts(screen: &Review, host: WidgetId) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack = vec![host];
    while let Some(id) = stack.pop() {
        if let Some(tag) = screen.tree.get::<noa_gpui_core::widgets::Tag>(id) {
            out.push(tag.text().to_owned());
        }
        stack.extend(screen.tree.children(id).iter().rev());
    }
    out
}

#[test]
fn a_held_review_does_not_read_as_finished() {
    let screen = Review::demo_held();
    let tags = tag_texts(&screen, screen.progress_host);
    assert!(tags.contains(&"✓ ラウンド完了".to_owned()), "{tags:?}");
    assert!(!tags.contains(&"✓ 完了".to_owned()), "{tags:?}");
}

#[test]
fn ending_a_review_asks_first_and_return_keeps_it_running() {
    let (mut screen, link) = live_with(crate::demo::held());
    act(&mut screen, Action::Close);
    assert!(screen.close_alert.is_some());
    assert_eq!(commands(&link), vec![]);
    // Return answers the default button, キャンセル.
    press(&mut screen, noa_gpui_core::Key::Enter);
    assert!(screen.close_alert.is_none());
    assert_eq!(commands(&link), vec![]);
    // Escape cancels too.
    act(&mut screen, Action::Close);
    press(&mut screen, noa_gpui_core::Key::Escape);
    assert_eq!(commands(&link), vec![]);
    // レビューを終了, chosen on purpose, ends it.
    act(&mut screen, Action::Close);
    press(&mut screen, noa_gpui_core::Key::Tab);
    press(&mut screen, noa_gpui_core::Key::Space);
    assert!(screen.close_alert.is_none());
    assert_eq!(commands(&link), vec![Command::Close]);
}

#[test]
fn the_agents_panes_close_together_once_the_review_has_ended() {
    let (screen, _link) = live_with(crate::demo::held());
    assert!(!acts(&screen).contains(&Action::ClosePanes));
    let finished = crate::demo::session();
    assert!(finished.closed);
    let (mut screen, link) = live_with(finished);
    assert!(acts(&screen).contains(&Action::ClosePanes));
    act(&mut screen, Action::ClosePanes);
    assert_eq!(commands(&link), vec![Command::ClosePanes]);
    lay_out(&mut screen, SMALLEST);
    assert_eq!(cut_off(&screen, screen.status_trailing), vec![]);
    // A stored review has no runner and no panes of its own.
    let root = std::env::temp_dir().join(format!("noa-review-ui-panes-{}", std::process::id()));
    let stored = Review::stored(&stored_demo(&root)).unwrap();
    assert!(!acts(&stored).contains(&Action::ClosePanes));
    let _ = std::fs::remove_dir_all(root);
}

fn draft(screen: &Review) -> String {
    composer::text(&screen.tree, &screen.composer)
}

fn type_draft(screen: &mut Review, body: &str) {
    screen
        .tree
        .update::<noa_gpui_core::widgets::TextArea, _>(screen.composer.area, |a, cx| {
            a.set_text(cx, body)
        });
}

fn with_intervention(mut session: Session, body: &str) -> Session {
    let n = session.messages.len() + 1;
    session.messages.push(review::model::types::Message {
        id: review::model::types::MessageId::nth(n),
        round: 1,
        author: review::model::types::Author::User,
        kind: review::model::types::MsgKind::Intervention,
        to: Vec::new(),
        body: body.to_owned(),
        at: 1,
    });
    session
}

#[test]
fn a_new_draft_survives_resending_an_instruction_already_in_the_log() {
    let earlier = "認可も見てください";
    let (mut screen, link) = live_with(with_intervention(crate::demo::held(), earlier));
    type_draft(&mut screen, earlier);
    act(&mut screen, Action::Send);
    assert_eq!(commands(&link).len(), 1);
    // The next draft, then a tab switch with no new snapshot: the earlier
    // message in the log is not this send arriving.
    type_draft(&mut screen, "次はテストも");
    act(&mut screen, Action::ShowTab(CenterTab::Matrix));
    assert_eq!(draft(&screen), "次はテストも");
}

#[test]
fn a_sent_draft_clears_when_it_reaches_the_log_unless_rewritten() {
    for (rewrite, left) in [(None, ""), (Some("別の下書き"), "別の下書き")] {
        let (mut screen, link) = live_with(crate::demo::held());
        type_draft(&mut screen, "認可も見てください");
        act(&mut screen, Action::Send);
        if let Some(text) = rewrite {
            type_draft(&mut screen, text);
        }
        assert_eq!(commands(&link).len(), 1);
        let mut next = with_intervention(crate::demo::held(), "認可も見てください");
        next.rev += 1;
        link.snapshots
            .send(Snapshot {
                link: Link::Live,
                session: Some(Arc::new(next)),
                reply_root: None,
                session_dir: None,
                generation: 0,
            })
            .unwrap();
        screen.poll_live(&mut TextStore::new(Default::default()));
        assert_eq!(draft(&screen), left);
        assert_eq!(screen.pending_intervention, None);
    }
}

/// The messages of the toasts on screen, oldest first.
fn toasts(screen: &Review) -> Vec<String> {
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

/// A clipboard that refuses the write, as the OS one can: it keeps what it
/// held.
struct RefusingClipboard;

impl noa_gpui_core::clipboard::Clipboard for RefusingClipboard {
    fn read_text(&mut self) -> Option<String> {
        Some("前の内容".to_owned())
    }
    fn write_text(&mut self, _text: &str) {}
}

#[test]
fn copying_the_report_says_whether_it_reached_the_clipboard() {
    let mut screen = Review::demo();
    act(&mut screen, Action::CopyReport);
    assert_eq!(
        screen.tree.clipboard().read_text(),
        Some(export::report_markdown(screen.session.as_ref().unwrap()))
    );
    assert_eq!(
        toasts(&screen),
        ["レポートをクリップボードにコピーしました"]
    );

    let mut screen = Review::demo();
    screen.tree.set_clipboard(Box::new(RefusingClipboard));
    act(&mut screen, Action::CopyReport);
    assert_eq!(
        toasts(&screen),
        ["レポートをクリップボードにコピーできませんでした"]
    );
}

#[test]
fn the_matrix_fits_the_smallest_window() {
    let mut screen = Review::demo().with_view(Some(CenterTab::Matrix), None);
    lay_out(&mut screen, SMALLEST);
    assert_eq!(cut_off(&screen, screen.center_host), vec![]);
}

/// The height of the scrolled area under the center tabs.
fn center_scroll_height(screen: &Review) -> f32 {
    let scroll = screen
        .tree
        .children(screen.center_host)
        .iter()
        .copied()
        .find(|&id| {
            screen
                .tree
                .get::<noa_gpui_core::widgets::ScrollView>(id)
                .is_some()
        })
        .unwrap();
    screen.tree.window_rect(scroll).unwrap().height
}

#[test]
fn a_held_review_in_the_smallest_window_keeps_room_for_its_findings() {
    let mut screen = Review::demo_held();
    lay_out(&mut screen, SMALLEST);
    // Two finding rows fit once the filters scroll away.
    let row = widget_for(&screen, &Action::SelectFinding(Some("F2".into()))).unwrap();
    let row = screen.tree.window_rect(row).unwrap().height;
    let height = center_scroll_height(&screen);
    assert!(height >= 2.0 * row, "{height} < 2 × {row}");
}

fn button_label(screen: &Review, action: &Action) -> String {
    let id = widget_for(screen, action).unwrap();
    screen
        .tree
        .get::<noa_gpui_core::widgets::Button>(id)
        .unwrap()
        .label()
        .to_owned()
}

#[test]
fn the_agreement_filters_count_live_findings_as_the_severities_do() {
    let screen = Review::demo();
    let session = screen.session.as_ref().unwrap();
    let live: Vec<_> = session
        .findings
        .iter()
        .filter(|f| f.status.is_live())
        .collect();
    for a in Agreement::ALL {
        let n = live
            .iter()
            .filter(|f| session.tally(f).agreement == a)
            .count();
        assert_eq!(
            button_label(&screen, &Action::ToggleAgreement(a)),
            format!("{} {n}", a.label())
        );
    }
    let total: usize = Agreement::ALL
        .iter()
        .map(|a| {
            live.iter()
                .filter(|f| session.tally(f).agreement == *a)
                .count()
        })
        .sum();
    assert_eq!(total, live.len());
}

#[test]
fn the_document_file_can_be_chosen_in_an_open_panel() {
    use noa_gpui_platform::Screen as _;
    let mut screen = Review::demo_start();
    assert!(acts(&screen).contains(&Action::PickDocument));
    act(&mut screen, Action::PickDocument);
    let asked = screen.take_file_dialogs();
    assert_eq!(asked.len(), 1);
    assert!(!asked[0].save);
    let mut text = TextStore::new(Default::default());
    screen.file_dialog_finished(
        FileDialogResult::Opened(vec![PathBuf::from("/tmp/plan.md")]),
        &mut text,
    );
    assert_eq!(
        start::read(&screen.tree, &screen.start).document_file,
        "/tmp/plan.md"
    );
    // A cancelled panel leaves the field as it was.
    screen.file_dialog_finished(FileDialogResult::Cancelled, &mut text);
    assert_eq!(
        start::read(&screen.tree, &screen.start).document_file,
        "/tmp/plan.md"
    );
}

#[test]
fn a_lead_summary_with_the_sent_text_does_not_count_as_the_send() {
    let (mut screen, link) = live_with(crate::demo::held());
    type_draft(&mut screen, "認可も見てください");
    act(&mut screen, Action::Send);
    assert_eq!(commands(&link).len(), 1);
    // The lead's summary happens to read the same: it is not the user's
    // instruction arriving, so the draft and the wait stay.
    let mut next = crate::demo::held();
    let n = next.messages.len() + 1;
    next.messages.push(review::model::types::Message {
        id: review::model::types::MessageId::nth(n),
        round: 1,
        author: review::model::types::Author::Part(review::model::types::PartId::new(
            review::model::types::LEAD_ID,
        )),
        kind: review::model::types::MsgKind::Summary,
        to: Vec::new(),
        body: "認可も見てください".to_owned(),
        at: 2,
    });
    next.rev += 1;
    link.snapshots
        .send(Snapshot {
            link: Link::Live,
            session: Some(Arc::new(next)),
            reply_root: None,
            session_dir: None,
            generation: 0,
        })
        .unwrap();
    screen.poll_live(&mut TextStore::new(Default::default()));
    assert_eq!(draft(&screen), "認可も見てください");
    assert!(screen.pending_intervention.is_some());
}

/// A clipboard that takes the text but cannot be read back.
struct WriteOnlyClipboard;

impl noa_gpui_core::clipboard::Clipboard for WriteOnlyClipboard {
    fn read_text(&mut self) -> Option<String> {
        None
    }
    fn write_text(&mut self, _text: &str) {}
}

#[test]
fn a_copy_that_cannot_be_read_back_is_reported_as_unconfirmed() {
    let mut screen = Review::demo();
    screen.tree.set_clipboard(Box::new(WriteOnlyClipboard));
    act(&mut screen, Action::CopyReport);
    assert_eq!(
        toasts(&screen),
        ["レポートをコピーしましたが、クリップボードを読み返して確かめられませんでした"]
    );
    // Holding other text afterwards is a failure.
    let mut screen = Review::demo();
    screen.tree.set_clipboard(Box::new(RefusingClipboard));
    act(&mut screen, Action::CopyReport);
    assert_eq!(
        toasts(&screen),
        ["レポートをクリップボードにコピーできませんでした"]
    );
}

#[test]
fn every_filter_count_is_what_choosing_that_value_alone_shows() {
    let mut screen = Review::demo();
    // Each step adds to the filters before it: none, all cleared, then a
    // severity, then one reviewer.
    let steps = [
        None,
        Some(Action::ClearFilters),
        Some(Action::ToggleSeverity(Severity::Critical)),
        Some(Action::FilterReviewer(Some(PartId::new("codex")))),
    ];
    for step in steps {
        if let Some(action) = step {
            act(&mut screen, action);
        }
        let session = screen.session.clone().unwrap();
        let only = |change: &dyn Fn(&mut Filters)| {
            let mut filters = screen.filters.clone();
            change(&mut filters);
            board::shown(&session, &filters).len()
        };
        for s in Severity::ALL {
            let n = only(&|f| f.severities = [s].into());
            let action = Action::ToggleSeverity(s);
            assert_eq!(button_label(&screen, &action), format!("{} {n}", s.label()));
        }
        for s in FindingStatus::ALL {
            let n = only(&|f| f.statuses = [status_key(s)].into());
            let action = Action::ToggleStatus(s);
            assert_eq!(button_label(&screen, &action), format!("{} {n}", s.label()));
        }
        for a in Agreement::ALL {
            let n = only(&|f| f.agreements = [agreement_key(a)].into());
            let action = Action::ToggleAgreement(a);
            assert_eq!(button_label(&screen, &action), format!("{} {n}", a.label()));
        }
    }
}

#[test]
fn keyboard_focus_survives_a_new_state_of_the_review() {
    let (mut screen, link) = live_with(crate::demo::held());
    let chip = Action::ToggleSeverity(Severity::Critical);
    let id = widget_for(&screen, &chip).unwrap();
    screen.tree.set_focus(Some(id));
    let mut next = crate::demo::held();
    next.rev += 1;
    link.snapshots
        .send(Snapshot {
            link: Link::Live,
            session: Some(Arc::new(next)),
            reply_root: None,
            session_dir: None,
            generation: 0,
        })
        .unwrap();
    screen.poll_live(&mut TextStore::new(Default::default()));
    let focused = screen.tree.focused().expect("focus kept");
    assert_ne!(focused, id, "the screen was rebuilt");
    assert_eq!(widget_for(&screen, &chip), Some(focused));
}

#[test]
fn focus_on_the_second_of_two_alike_buttons_stays_on_the_second() {
    // A selected finding's row and the thread's 選択を解除 both clear the
    // selection; focus on the latter stays there through a rebuild.
    let (mut screen, link) = live_with(crate::demo::held());
    act(&mut screen, Action::SelectFinding(Some("F2".into())));
    let clear = Action::SelectFinding(None);
    let alike: Vec<WidgetId> = screen
        .actions
        .iter()
        .filter(|(_, a)| *a == clear)
        .map(|(id, _)| *id)
        .collect();
    assert_eq!(alike.len(), 2);
    screen.tree.set_focus(Some(alike[1]));
    let mut next = crate::demo::held();
    next.rev += 1;
    link.snapshots
        .send(Snapshot {
            link: Link::Live,
            session: Some(Arc::new(next)),
            reply_root: None,
            session_dir: None,
            generation: 0,
        })
        .unwrap();
    screen.poll_live(&mut TextStore::new(Default::default()));
    let now: Vec<WidgetId> = screen
        .actions
        .iter()
        .filter(|(_, a)| *a == clear)
        .map(|(id, _)| *id)
        .collect();
    assert_eq!(screen.tree.focused(), Some(now[1]));
}

/// The top of the first finding row and how many rows show whole above the
/// bottom of the scrolled list, with the screen laid out at `size`.
fn first_rows(screen: &mut Review, size: (f32, f32)) -> (f32, usize) {
    lay_out(screen, size);
    let scroll = screen
        .tree
        .children(screen.center_host)
        .iter()
        .copied()
        .rfind(|&id| {
            screen
                .tree
                .get::<noa_gpui_core::widgets::ScrollView>(id)
                .is_some()
        })
        .unwrap();
    let bottom = screen.tree.window_rect(scroll).unwrap().bottom();
    let rows: Vec<_> = screen
        .actions
        .iter()
        .filter(|(_, a)| matches!(a, Action::SelectFinding(Some(_))))
        .filter_map(|(id, _)| screen.tree.window_rect(*id))
        .collect();
    let top = rows.iter().map(|r| r.y).fold(f32::MAX, f32::min);
    let whole = rows.iter().filter(|r| r.bottom() <= bottom + 0.5).count();
    (top, whole)
}

#[test]
fn the_smallest_window_shows_as_much_of_the_findings_as_before() {
    // Laid out by 601c4c3b (before the filters scrolled with the list): the
    // first row's top and the rows shown whole at the smallest window.
    let root = std::env::temp_dir().join(format!("noa-review-rows-{}", std::process::id()));
    let stored = Review::stored(&stored_demo(&root)).unwrap();
    // The stored review opens its snapshot from the target card (L1-5).
    assert!(acts(&stored).contains(&Action::OpenSnapshot(None)));
    for (mut screen, top_before, whole_before) in [
        (Review::demo(), 300.2, 3),
        (Review::demo_held(), 423.2, 0),
        (stored, 300.2, 3),
    ] {
        let (top, whole) = first_rows(&mut screen, SMALLEST);
        assert!(top <= top_before, "{top} > {top_before}");
        assert!(whole >= whole_before, "{whole} < {whole_before}");
    }
    let _ = std::fs::remove_dir_all(root);
}

/// The demo review with its findings repeated up to `n` (ids F1…Fn).
fn many_findings(n: usize) -> Session {
    let mut session = crate::demo::session();
    let base = session.findings.clone();
    session.findings = (0..n)
        .map(|i| {
            let mut f = base[i % base.len()].clone();
            f.id = format!("F{}", i + 1);
            f
        })
        .collect();
    session
}

#[test]
fn a_rebuild_ranks_the_findings_at_most_once() {
    // Fifteen chips and the summary count; only the list needs an order.
    for nothing_shown in [false, true] {
        let (mut screen, _link) = live_with(many_findings(50));
        if nothing_shown {
            screen.filters.severities = [Severity::Info].into();
        }
        let mut text = TextStore::new(Default::default());
        board::RANKINGS.with(|n| n.set(0));
        screen.rebuild_center(&mut text);
        assert!(board::RANKINGS.with(|n| n.get()) <= 1);
        // The whole screen too, with 修正を依頼 counting the shown scope.
        assert!(screen.session.as_ref().unwrap().workdir.is_some());
        board::RANKINGS.with(|n| n.set(0));
        screen.rebuild(&mut text);
        assert!(
            fix_actions(&screen)
                .iter()
                .any(|(_, s)| *s == FixScope::Shown)
                != nothing_shown
        );
        assert!(board::RANKINGS.with(|n| n.get()) <= 1);
    }
}

// ---- 修正を依頼 ----

/// Feeds `session` to a live screen with its store and reply dirs, as a
/// runner's snapshot carries them.
fn live_with_dirs(session: Session) -> (Review, crate::live::FakeLink) {
    let (channel, link) = Channel::fake();
    let mut screen = live_ready(channel);
    link.snapshots
        .send(Snapshot {
            link: Link::Live,
            session: Some(Arc::new(session)),
            reply_root: Some(PathBuf::from("/w/.noa/reviews/rev-demo")),
            session_dir: Some(PathBuf::from("/data/noa/reviews/rev-demo")),
            generation: 0,
        })
        .unwrap();
    screen.poll_live(&mut TextStore::new(Default::default()));
    (screen, link)
}

/// Every label of the right pane, joined.
fn all_text(screen: &Review) -> String {
    labels_under(&screen.tree, screen.right_host).join("\n")
}

fn fix_actions(screen: &Review) -> Vec<(AgentKind, FixScope)> {
    acts(screen)
        .into_iter()
        .filter_map(|a| match a {
            Action::RequestFix { agent, scope } => Some((agent, scope)),
            _ => None,
        })
        .collect()
}

#[test]
fn the_verdict_offers_a_fix_of_its_must_fix_and_of_the_shown_findings_by_every_agent() {
    let (screen, _link) = live_with_dirs(crate::demo::session());
    let offered = fix_actions(&screen);
    for scope in [FixScope::MustFix, FixScope::Shown] {
        for kind in AgentKind::ALL {
            assert!(
                offered.contains(&(kind, scope.clone())),
                "{kind:?} {scope:?}"
            );
        }
    }
    assert_eq!(offered.len(), 2 * AgentKind::ALL.len());
    // Each button says what it does to a screen reader.
    let codex = widget_for(
        &screen,
        &Action::RequestFix {
            agent: AgentKind::Codex,
            scope: FixScope::MustFix,
        },
    )
    .unwrap();
    let label = screen.tree.accessible_label(codex).unwrap_or_default();
    assert!(label.starts_with("codex に要修正"), "{label}");
}

#[test]
fn a_fix_request_carries_the_must_fix_findings_and_the_reviews_store() {
    let session = crate::demo::session();
    let must_fix = session.verdict().unwrap().must_fix.clone();
    assert!(!must_fix.is_empty());
    let (mut screen, link) = live_with_dirs(session);
    act(
        &mut screen,
        Action::RequestFix {
            agent: AgentKind::Codex,
            scope: FixScope::MustFix,
        },
    );
    let sent = commands(&link);
    let [Command::RequestFix(req)] = sent.as_slice() else {
        panic!("{sent:?}");
    };
    assert_eq!(req.agent, AgentKind::Codex);
    assert_eq!(req.ids, must_fix);
    assert_eq!(req.dir, PathBuf::from("/data/noa/reviews/rev-demo"));
}

#[test]
fn the_shown_scope_follows_the_board_filters() {
    let (mut screen, link) = live_with_dirs(crate::demo::session());
    act(&mut screen, Action::ToggleSeverity(Severity::Critical));
    let shown: Vec<String> = board::shown(screen.session.as_ref().unwrap(), &screen.filters)
        .into_iter()
        .map(|f| f.id.clone())
        .collect();
    assert!(!shown.is_empty());
    act(
        &mut screen,
        Action::RequestFix {
            agent: AgentKind::Claude,
            scope: FixScope::Shown,
        },
    );
    let sent = commands(&link);
    let [Command::RequestFix(req)] = sent.as_slice() else {
        panic!("{sent:?}");
    };
    assert_eq!(req.ids, shown);
}

#[test]
fn a_selected_finding_offers_a_fix_of_itself_only() {
    let (mut screen, link) = live_with_dirs(crate::demo::session());
    act(&mut screen, Action::SelectFinding(Some("F2".into())));
    let offered = fix_actions(&screen);
    assert_eq!(offered.len(), AgentKind::ALL.len());
    assert!(
        offered
            .iter()
            .all(|(_, scope)| *scope == FixScope::Finding("F2".into()))
    );
    act(
        &mut screen,
        Action::RequestFix {
            agent: AgentKind::Agy,
            scope: FixScope::Finding("F2".into()),
        },
    );
    let sent = commands(&link);
    let [Command::RequestFix(req)] = sent.as_slice() else {
        panic!("{sent:?}");
    };
    assert_eq!(req.ids, ["F2"]);
}

#[test]
fn only_installed_agents_are_offered_a_fix() {
    let (mut screen, _link, tx, mut text) = live_pending();
    report(
        &mut screen,
        &tx,
        Installed::Found(vec![AgentKind::Codex]),
        &mut text,
    );
    screen.session = Some(Arc::new(crate::demo::session()));
    screen.rebuild(&mut text);
    let offered = fix_actions(&screen);
    assert!(!offered.is_empty());
    assert!(offered.iter().all(|(k, _)| *k == AgentKind::Codex));
}

#[test]
fn no_fix_is_offered_while_the_agent_check_runs_or_without_a_tree_to_fix() {
    // The check has not reported.
    let (mut screen, _link, _tx, mut text) = live_pending();
    screen.session = Some(Arc::new(crate::demo::session()));
    screen.rebuild(&mut text);
    assert!(fix_actions(&screen).is_empty());
    assert!(all_text(&screen).contains("インストール済みのエージェントを確認しています"));

    // A document review has no work tree to fix.
    let mut document = crate::demo::session();
    document.target.kind = TargetKind::Document;
    let (screen, _link) = live_with_dirs(document);
    assert!(fix_actions(&screen).is_empty());
    assert!(all_text(&screen).contains("文書のレビューでは修正を依頼できません"));

    // Nor does a review without a workdir.
    let mut nowhere = crate::demo::session();
    nowhere.workdir = None;
    let (screen, _link) = live_with_dirs(nowhere);
    assert!(fix_actions(&screen).is_empty());
    assert!(all_text(&screen).contains("作業ディレクトリのあるレビューが必要です"));
}

#[test]
fn a_headless_stored_view_shows_no_fix_card() {
    let root = std::env::temp_dir().join(format!("noa-review-ui-fix-{}", std::process::id()));
    let dir = stored_demo(&root);
    let screen = Review::stored(&dir).unwrap();
    assert!(fix_actions(&screen).is_empty());
    assert!(!all_text(&screen).contains("修正を依頼"));
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn a_stored_review_in_the_live_window_can_be_fixed() {
    let root = std::env::temp_dir().join(format!("noa-review-ui-fix-live-{}", std::process::id()));
    let dir = stored_demo(&root);
    let (channel, link) = Channel::fake();
    let mut screen = stored_ready(channel, &dir);
    act(
        &mut screen,
        Action::RequestFix {
            agent: AgentKind::Claude,
            scope: FixScope::MustFix,
        },
    );
    // (Showing it left whatever was on screen first.)
    let sent: Vec<Command> = commands(&link)
        .into_iter()
        .filter(|c| *c != Command::Leave)
        .collect();
    let [Command::RequestFix(req)] = sent.as_slice() else {
        panic!("{sent:?}");
    };
    assert_eq!(req.dir, dir);
    let _ = std::fs::remove_dir_all(root);
}

// ---- 同じ設定で再レビュー ----

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("noa-review-again-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// The demo review as if it had reviewed `repo`, with a custom lens and a
/// path filter.
fn demo_in(repo: &Path) -> Session {
    let mut session = crate::demo::session();
    session.target.path = Some(repo.to_owned());
    session.target.paths = vec!["src/".into(), "docs/a b.md".into()];
    session.reviewers[2].lens = Some(review::model::types::Lens::Custom {
        id: "custom".into(),
        text: "API 互換性と移行手順".into(),
    });
    session
}

fn panel_of(session: &Session) -> Vec<ReviewerChoice> {
    session
        .reviewers
        .iter()
        .map(|p| ReviewerChoice {
            agent: review::model::types::AgentChoice {
                kind: p.kind,
                setup: p.setup.clone(),
            },
            lens: match p.lens.as_ref().unwrap() {
                review::model::types::Lens::Builtin { id } => LensChoice::Builtin(*id),
                review::model::types::Lens::Custom { text, .. } => LensChoice::Custom(text.clone()),
            },
        })
        .collect()
}

#[test]
fn reviewing_again_fills_the_start_view_with_the_same_target_panel_and_limit() {
    let repo = temp_dir("git");
    let session = demo_in(&repo);
    let (mut screen, _link) = live_with(session.clone());
    assert!(acts(&screen).contains(&Action::ReviewAgain(None)));
    act(&mut screen, Action::ReviewAgain(None));
    assert!(screen.session.is_none());
    assert!(!screen.tree.is_hidden(screen.start.root));
    let form = start::read(&screen.tree, &screen.start);
    assert_eq!(form.target().unwrap(), session.target);
    assert_eq!(form.panel, panel_of(&session));
    assert_eq!(form.lead.kind, session.lead.kind);
    assert_eq!(form.lead.setup, session.lead.setup);
    assert_eq!(form.max_rounds, session.max_rounds);
    let _ = std::fs::remove_dir_all(repo);
}

#[test]
fn files_and_document_reviews_come_back_as_they_were_set_up() {
    let root = temp_dir("files");
    let mut files = crate::demo::session();
    files.target = review::model::types::Target {
        kind: TargetKind::Files,
        path: Some(root.clone()),
        base: None,
        head: review::model::types::Head::Worktree,
        paths: vec!["src".into(), "README.md".into()],
        text: None,
        workdir: None,
    };
    assert_eq!(
        start::Form::from_session(&files).target().unwrap(),
        files.target
    );
    let mut typed = files.clone();
    typed.target = review::model::types::Target {
        kind: TargetKind::Document,
        path: None,
        text: Some("\n  # Plan\n\nstep 1\n".into()),
        workdir: Some(root.clone()),
        ..files.target.clone()
    };
    typed.target.paths.clear();
    assert_eq!(
        start::Form::from_session(&typed).target().unwrap(),
        typed.target
    );
    let doc = root.join("plan.md");
    std::fs::write(&doc, "# plan\n").unwrap();
    let mut from_file = typed.clone();
    from_file.target.text = None;
    from_file.target.path = Some(doc);
    from_file.target.workdir = None;
    assert_eq!(
        start::Form::from_session(&from_file).target().unwrap(),
        from_file.target
    );
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn the_request_keeps_what_was_asked_and_adds_the_must_fix_to_check() {
    let session = crate::demo::session();
    let form = start::Form::from_session(&session);
    assert!(form.request.starts_with(session.request.trim_end()));
    let note = &form.request[session.request.trim_end().len()..];
    assert!(
        note.starts_with("\n\n（前回のレビュー rev-demo の総合判定: 要修正）"),
        "{note}"
    );
    for id in &session.verdict().unwrap().must_fix {
        let f = session.finding(id).unwrap();
        assert!(
            note.contains(&format!("- {id} [{}] {}", f.severity.label(), f.claim)),
            "{note}"
        );
    }
    // Reviewed again after that: the earlier note gives way to the new one.
    let mut again = session.clone();
    again.request = form.request.clone();
    let twice = start::Form::from_session(&again).request;
    assert_eq!(twice, form.request);
    assert_eq!(twice.matches("（前回のレビュー ").count(), 1);
}

#[test]
fn a_review_without_a_verdict_keeps_its_request_as_it_was() {
    let session = crate::demo::held();
    assert!(session.verdict().is_none());
    assert_eq!(
        start::Form::from_session(&session).request,
        session.request.trim_end()
    );
}

#[test]
fn past_reviews_can_be_reviewed_again_from_the_list() {
    let mut screen = Review::demo_start();
    let again: Vec<Action> = acts(&screen)
        .into_iter()
        .filter(|a| matches!(a, Action::ReviewAgain(Some(_))))
        .collect();
    // One per readable row; an unreadable log offers none.
    let readable = screen.past.iter().filter(|l| l.summary.is_ok()).count();
    assert!(screen.past.len() > readable);
    assert_eq!(again.len(), readable);
    let Action::ReviewAgain(Some(dir)) = again[0].clone() else {
        unreachable!()
    };
    let stored = screen.load_stored(&dir).unwrap();
    act(&mut screen, again[0].clone());
    let form = start::read(&screen.tree, &screen.start);
    assert_eq!(form.kind, stored.target.kind);
    assert_eq!(form.panel, panel_of(&stored));
    assert_eq!(form.max_rounds, stored.max_rounds);
}

#[test]
fn starting_the_filled_form_starts_the_same_review_in_two_steps() {
    let repo = temp_dir("start");
    let session = demo_in(&repo);
    let (mut screen, link) = live_with(session.clone());
    // Two steps: 同じ設定で再レビュー, then レビューを開始.
    act(&mut screen, Action::ReviewAgain(None));
    act(&mut screen, Action::Start);
    let started: Vec<Command> = commands(&link)
        .into_iter()
        .filter(|c| matches!(c, Command::Start { .. }))
        .collect();
    let [
        Command::Start {
            request,
            target,
            max_rounds,
            lead,
            panel,
            ..
        },
    ] = started.as_slice()
    else {
        panic!("{started:?}");
    };
    assert_eq!(**target, session.target);
    assert_eq!(*max_rounds, session.max_rounds);
    assert_eq!(lead.kind, session.lead.kind);
    assert_eq!(*panel, panel_of(&session));
    assert!(request.starts_with(session.request.trim_end()));
    assert!(request.contains("（前回のレビュー rev-demo"));
    let _ = std::fs::remove_dir_all(repo);
}

#[test]
fn reviewing_again_leaves_out_agents_that_are_not_installed() {
    let repo = temp_dir("installed");
    let (mut screen, _link, tx, mut text) = live_pending();
    report(&mut screen, &tx, found(&[AgentKind::Claude]), &mut text);
    screen.session = Some(Arc::new(demo_in(&repo)));
    screen.rebuild(&mut text);
    act(&mut screen, Action::ReviewAgain(None));
    let form = start::read(&screen.tree, &screen.start);
    assert!(
        form.panel
            .iter()
            .all(|row| row.agent.kind == AgentKind::Claude)
    );
    assert_eq!(form.lead.kind, AgentKind::Claude);
    let _ = std::fs::remove_dir_all(repo);
}

// ---- --repo ----

#[test]
fn the_focused_panes_directory_fills_the_repository_root_and_working_directory() {
    let (channel, _link) = Channel::fake();
    let screen = live_ready(channel).with_place(Path::new("/Users/me/src/noa"));
    let form = start::read(&screen.tree, &screen.start);
    assert_eq!(form.repo, "/Users/me/src/noa");
    assert_eq!(form.root, "/Users/me/src/noa");
    assert_eq!(form.document_workdir, "/Users/me/src/noa");
    // The rest stays the clean form.
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
    assert_eq!(form.root, "/Users/me/src/noa");
    // The untouched panel took the installed kinds as its defaults.
    assert!(
        form.panel
            .iter()
            .all(|row| row.agent.kind == AgentKind::Codex)
    );
}

// ---- 指摘の検索とファイルの絞り込み ----

/// Types `typed` into the findings search, as keystrokes do.
fn type_search(screen: &mut Review, typed: &str, text: &mut TextStore) {
    if !screen.search_open {
        act(screen, Action::ToggleSearch);
        assert_eq!(screen.tree.focused(), Some(screen.search));
    }
    // (A click in the field focuses it.)
    screen.tree.set_focus(Some(screen.search));
    screen.dispatch(noa_gpui_core::Event::Text(typed.to_owned()), text);
}

fn shown_ids(screen: &Review) -> Vec<String> {
    board::shown(screen.session.as_ref().unwrap(), &screen.filters)
        .into_iter()
        .map(|f| f.id.clone())
        .collect()
}

#[test]
fn typing_in_the_search_narrows_the_board_and_keeps_the_field() {
    let (mut screen, _link) = live_with(crate::demo::session());
    let mut text = TextStore::new(Default::default());
    let all = rows(&screen);
    assert!(all > 1);
    // "シェル" is in F2's claim only (its evidence too).
    type_search(&mut screen, "シェル", &mut text);
    assert_eq!(screen.filters.query, "シェル");
    assert_eq!(shown_ids(&screen), ["F2"]);
    assert_eq!(rows(&screen), 1);
    let summary = labels_under(&screen.tree, screen.tabs_host).join(" ");
    assert!(
        summary.contains(&format!(
            "表示 1 / 全 {}",
            screen.session.as_ref().unwrap().findings.len()
        )),
        "{summary}"
    );
    // The field is the same widget, still focused, still holding the text.
    assert_eq!(screen.tree.focused(), Some(screen.search));
    assert_eq!(
        screen.tree.get::<TextField>(screen.search).unwrap().text(),
        "シェル"
    );
    // Clearing the filters clears the search too.
    act(&mut screen, Action::ClearFilters);
    assert!(screen.filters.query.is_empty());
    assert!(
        screen
            .tree
            .get::<TextField>(screen.search)
            .unwrap()
            .text()
            .is_empty()
    );
}

#[test]
fn the_search_matches_ids_paths_and_evidence_in_any_case_and_every_word() {
    let session = crate::demo::session();
    let mut filters = Filters::default();
    let ids = |filters: &Filters| -> Vec<String> {
        board::shown(&session, filters)
            .into_iter()
            .map(|f| f.id.clone())
            .collect()
    };
    // F1 itself, and any finding whose text names it.
    filters.query = "f1".into();
    let named = ids(&filters);
    assert_eq!(named.first().map(String::as_str), Some("F1"), "{named:?}");
    assert!(named.len() < ids(&Filters::default()).len());
    filters.query = "STORE.RS".into();
    let by_path = ids(&filters);
    assert!(by_path.contains(&"F1".to_owned()), "{by_path:?}");
    // Evidence only a source holds ("touch" is in codex/2's evidence).
    filters.query = "touch".into();
    assert_eq!(ids(&filters), ["F2"]);
    // Every word must be there.
    filters.query = "touch store.rs".into();
    assert!(ids(&filters).is_empty());
    filters.query = "   ".into();
    assert_eq!(ids(&filters), ids(&Filters::default()));
}

#[test]
fn a_file_in_the_coverage_opens_the_board_on_its_findings() {
    let (mut screen, _link) = live_with(crate::demo::session());
    act(&mut screen, Action::ShowTab(CenterTab::Matrix));
    let files: Vec<String> = acts(&screen)
        .into_iter()
        .filter_map(|a| match a {
            Action::FilterFile(Some(path)) => Some(path),
            _ => None,
        })
        .collect();
    assert!(files.contains(&"src/store.rs".to_owned()), "{files:?}");
    // A file no finding points into has no button.
    let session = screen.session.clone().unwrap();
    for file in &session.manifest.files {
        let any = session
            .findings
            .iter()
            .any(|f| board::touches(&session, f, &file.path));
        assert_eq!(files.contains(&file.path), any, "{}", file.path);
    }
    act(&mut screen, Action::FilterFile(Some("src/store.rs".into())));
    assert_eq!(screen.tab, CenterTab::Findings);
    let shown = shown_ids(&screen);
    assert!(!shown.is_empty());
    for id in &shown {
        let f = session.finding(id).unwrap();
        assert!(board::touches(&session, f, "src/store.rs"), "{id}");
    }
    // The chip says which file and takes it off.
    assert!(acts(&screen).contains(&Action::FilterFile(None)));
    act(&mut screen, Action::FilterFile(None));
    assert_eq!(screen.filters.path, None);
}

#[test]
fn chip_counts_hold_with_a_query_and_a_file() {
    let (mut screen, _link) = live_with(crate::demo::session());
    screen.filters.query = "の".into();
    screen.filters.path = Some("src/store.rs".into());
    screen.rebuild(&mut TextStore::new(Default::default()));
    let session = screen.session.clone().unwrap();
    // Every severity chip's count is what choosing it shows.
    for s in Severity::ALL {
        let mut chosen = screen.filters.clone();
        chosen.severities = [s].into();
        let label = format!("{} {}", s.label(), board::count(&session, &chosen));
        assert!(
            screen
                .actions
                .iter()
                .any(|(id, a)| *a == Action::ToggleSeverity(s)
                    && screen
                        .tree
                        .get::<noa_gpui_core::widgets::Button>(*id)
                        .map(|b| b.label().to_owned())
                        == Some(label.clone())),
            "{label}"
        );
        assert_eq!(
            board::shown(&session, &chosen).len(),
            board::count(&session, &chosen)
        );
    }
}

#[test]
fn the_search_opens_on_demand_on_the_findings_tab_and_closing_drops_it() {
    let (mut screen, _link) = live_with(crate::demo::session());
    // Closed by default: it takes no line of the board.
    assert!(screen.tree.is_hidden(screen.search_row));
    act(&mut screen, Action::ShowTab(CenterTab::Matrix));
    // ⌘F (the button's shortcut) opens it on the findings tab, focused.
    let button = widget_for(&screen, &Action::ToggleSearch).unwrap();
    assert!(
        !screen
            .tree
            .get::<noa_gpui_core::widgets::Button>(button)
            .unwrap()
            .is_selected()
    );
    act(&mut screen, Action::ToggleSearch);
    assert_eq!(screen.tab, CenterTab::Findings);
    assert!(!screen.tree.is_hidden(screen.search_row));
    assert_eq!(screen.tree.focused(), Some(screen.search));
    assert_eq!(
        screen.tree.accessible_label(screen.search),
        Some("指摘を検索")
    );
    act(&mut screen, Action::ShowTab(CenterTab::Report));
    assert!(screen.tree.is_hidden(screen.search_row));
    act(&mut screen, Action::ShowTab(CenterTab::Findings));
    let mut text = TextStore::new(Default::default());
    type_search(&mut screen, "シェル", &mut text);
    assert_eq!(rows(&screen), 1);
    act(&mut screen, Action::ToggleSearch);
    assert!(screen.tree.is_hidden(screen.search_row));
    assert!(screen.filters.query.is_empty());
    assert!(rows(&screen) > 1);
}

// ---- 過去のレビューをさらに表示 ----

#[test]
fn past_reviews_beyond_the_first_page_can_be_listed() {
    let root = std::env::temp_dir().join(format!("noa-review-past-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let log = crate::demo::log();
    let n = start::PAST_LIMIT + 2;
    for i in 0..n {
        let dir = root.join(format!("rev-{}", 1_000 + i));
        let mut store = Store::create(&dir, &log[0]).unwrap();
        store.append_all(&log[1..]).unwrap();
    }
    assert_eq!(store::count_sessions(&root), n);
    let (channel, _link) = Channel::fake();
    let mut screen = live_ready(channel);
    screen.past_root = Some(root.clone());
    screen.refresh_past();
    screen.rebuild(&mut TextStore::new(Default::default()));
    let listed = |screen: &Review| {
        acts(screen)
            .iter()
            .filter(|a| matches!(a, Action::ReviewAgain(Some(_))))
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
    // The newest first, the oldest now reached.
    assert!(screen.past.last().unwrap().dir.ends_with("rev-1000"));
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn a_short_past_list_offers_no_more() {
    let screen = Review::demo_start();
    assert!(!acts(&screen).contains(&Action::MorePast));
}

// ---- 独立レビュー（codex）の指摘への回帰テスト ----

/// Presses the visible button carrying `action` from the keyboard (focus,
/// then Space), as a person would.
fn press_button(screen: &mut Review, action: &Action) {
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

/// ⌘F, as the keyboard sends it.
fn command_f(screen: &mut Review) {
    let mut text = TextStore::new(Default::default());
    noa_gpui_platform::Screen::dispatch(
        screen,
        noa_gpui_core::Event::KeyDown {
            key: noa_gpui_core::Key::Char('f'),
            modifiers: noa_gpui_core::Modifiers {
                command: true,
                meta: true,
                ..Default::default()
            },
        },
        &mut text,
    );
}

#[test]
fn the_fix_and_review_again_buttons_are_pressable_from_the_keyboard() {
    let (mut screen, link) = live_with_dirs(crate::demo::session());
    press_button(
        &mut screen,
        &Action::RequestFix {
            agent: AgentKind::Codex,
            scope: FixScope::MustFix,
        },
    );
    assert!(matches!(
        commands(&link).as_slice(),
        [Command::RequestFix(_)]
    ));

    // 同じ設定で再レビュー, then レビューを開始: two presses.
    let repo = temp_dir("press");
    let (mut screen, link) = live_with(demo_in(&repo));
    press_button(&mut screen, &Action::ReviewAgain(None));
    press_button(&mut screen, &Action::Start);
    let started = commands(&link)
        .into_iter()
        .filter(|c| matches!(c, Command::Start { .. }))
        .count();
    assert_eq!(started, 1);
    let _ = std::fs::remove_dir_all(repo);
}

#[test]
fn a_running_review_says_by_the_fix_buttons_that_reviewers_read_the_work_tree() {
    let note = "レビュアーは作業ツリーも参照する";
    let (running, _link) = live_with_dirs(crate::demo::held());
    assert!(!fix_actions(&running).is_empty());
    assert!(all_text(&running).contains(note));
    let (closed, _link) = live_with_dirs(crate::demo::session());
    assert!(!all_text(&closed).contains(note));
}

#[test]
fn a_place_ending_in_a_space_is_reviewed_as_given() {
    let root = temp_dir("space");
    std::fs::create_dir_all(root.join("repo")).unwrap();
    std::fs::create_dir_all(root.join("repo ")).unwrap();
    let (channel, link) = Channel::fake();
    let mut screen = live_ready(channel).with_place(&root.join("repo "));
    act(&mut screen, Action::Start);
    let started: Vec<Command> = commands(&link)
        .into_iter()
        .filter(|c| matches!(c, Command::Start { .. }))
        .collect();
    let [Command::Start { target, .. }] = started.as_slice() else {
        panic!("{started:?}");
    };
    assert_eq!(target.path.as_deref(), Some(root.join("repo ").as_path()));
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn refilling_replaces_only_the_note_it_wrote() {
    // No verdict: a request that names 前回のレビュー in a sentence stays whole.
    let mut held = crate::demo::held();
    held.request = "必ず確認（前回のレビュー で追加した条件も含む）。最後にテストを実行。".into();
    assert_eq!(start::Form::from_session(&held).request, held.request);

    // A note from an earlier review, with the person's words after it.
    let session = crate::demo::session();
    let mut earlier = session.clone();
    earlier.id = review::model::types::SessionId::new("rev-old");
    let old_note = start::again_note(&earlier).unwrap();
    let mut again = session.clone();
    again.request = format!("依頼A\n\n{old_note}\n最後にテストを実行。");
    let new_note = start::again_note(&session).unwrap();
    assert_eq!(
        start::Form::from_session(&again).request,
        format!("依頼A\n\n{new_note}\n最後にテストを実行。")
    );
    // A `- ` line the person added right after the note is theirs.
    again.request = format!("{old_note}\n- 利用者が追加したテスト条件");
    assert_eq!(
        start::Form::from_session(&again).request,
        format!("{new_note}\n- 利用者が追加したテスト条件")
    );
    // A note without its closing line is not taken for the widget's: it
    // stays, and the new note follows.
    let unclosed = old_note.replace("\n（前回のレビューの要修正はここまで）", "");
    again.request = unclosed.clone();
    assert_eq!(
        start::Form::from_session(&again).request,
        format!("{unclosed}\n\n{new_note}")
    );
    // Two refills from a request with an older, unclosed note and a line
    // of the person's after it: the line survives both.
    again.request = format!("依頼A\n\n{unclosed}\n- 手で追加した制約");
    let once = start::Form::from_session(&again).request;
    assert_eq!(once, format!("{}\n\n{new_note}", again.request));
    let mut twice = session.clone();
    twice.request = once.clone();
    let twice = start::Form::from_session(&twice).request;
    assert!(twice.contains("- 手で追加した制約"), "{twice}");
    assert_eq!(twice, once);
    assert_eq!(
        twice
            .matches("（前回のレビューの要修正はここまで）")
            .count(),
        1
    );
    // And with a sentence of the person's that only mentions the mark.
    again.request = "（前回のレビュー の件も見て）".into();
    assert_eq!(
        start::Form::from_session(&again).request,
        format!("（前回のレビュー の件も見て）\n\n{new_note}")
    );
}

#[test]
fn a_refill_keeps_a_limit_raised_past_ten() {
    let repo = temp_dir("rounds");
    let mut session = demo_in(&repo);
    session.max_rounds = 13;
    let (mut screen, link) = live_with(session);
    act(&mut screen, Action::ReviewAgain(None));
    let form = start::read(&screen.tree, &screen.start);
    assert_eq!((form.max_rounds, form.round_cap), (13, 13));
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
    assert_eq!((*max_rounds, *round_cap), (13, 13));
    // A new review is back to the usual limit.
    act(&mut screen, Action::NewReview);
    let form = start::read(&screen.tree, &screen.start);
    assert_eq!(form.round_cap, review::model::types::MAX_MAX_ROUNDS);
    let _ = std::fs::remove_dir_all(repo);
}

#[test]
fn a_refilled_panel_keeps_its_lenses_when_the_agent_check_reports_after() {
    // A review set up with the default panel (claude/正確性, codex/セキュリ
    // ティ, agy/設計, CLI defaults): refilled, it looks untouched.
    use review::model::types::{AgentSetup, BuiltinLens, Lens};
    let repo = temp_dir("late-check");
    let mut session = demo_in(&repo);
    session.lead.setup = AgentSetup::default();
    for (part, lens) in session.reviewers.iter_mut().zip([
        BuiltinLens::Correctness,
        BuiltinLens::Security,
        BuiltinLens::Design,
    ]) {
        part.setup = AgentSetup::default();
        part.lens = Some(Lens::Builtin { id: lens });
    }
    let (mut screen, _link, tx, mut text) = live_pending();
    screen.session = Some(Arc::new(session));
    screen.rebuild(&mut text);
    act(&mut screen, Action::ReviewAgain(None));
    report(
        &mut screen,
        &tx,
        found(&[AgentKind::Codex, AgentKind::Agy]),
        &mut text,
    );
    let form = start::read(&screen.tree, &screen.start);
    let rows: Vec<(AgentKind, LensChoice)> = form
        .panel
        .iter()
        .map(|r| (r.agent.kind, r.lens.clone()))
        .collect();
    // Only claude went; the others kept their lenses.
    assert_eq!(
        rows,
        [
            (AgentKind::Codex, LensChoice::Builtin(BuiltinLens::Security)),
            (AgentKind::Agy, LensChoice::Builtin(BuiltinLens::Design)),
        ]
    );
    let _ = std::fs::remove_dir_all(repo);
}

#[test]
fn a_filter_that_hides_the_selected_finding_lets_it_go() {
    let (mut screen, _link) = live_with_dirs(crate::demo::session());
    act(&mut screen, Action::SelectFinding(Some("F2".into())));
    act(&mut screen, Action::FilterFile(Some("src/store.rs".into())));
    assert_eq!(screen.selected, None);
    assert!(
        fix_actions(&screen)
            .iter()
            .all(|(_, s)| *s != FixScope::Finding("F2".into()))
    );
    // A finding the filter keeps stays selected.
    act(&mut screen, Action::SelectFinding(Some("F1".into())));
    act(&mut screen, Action::ToggleSeverity(Severity::High));
    assert_eq!(screen.selected.as_deref(), Some("F1"));
    // Words that hide it let it go too.
    let mut text = TextStore::new(Default::default());
    type_search(&mut screen, "シェル", &mut text);
    assert_eq!(screen.selected, None);
}

#[test]
fn the_tab_row_and_the_header_stay_in_the_pane_at_the_smallest_window() {
    let long = "crates/noa-app/src/app/helpers/a/rather/long/path/to/its/file.rs";
    let mut held = Review::demo_held();
    lay_out(&mut held, SMALLEST);
    assert_eq!(cut_off(&held, held.tabs_host), vec![]);
    assert_eq!(cut_off(&held, held.header_host), vec![]);
    for path in ["src/store.rs", long] {
        let (mut screen, _link) = live_with(crate::demo::held());
        act(&mut screen, Action::FilterFile(Some(path.into())));
        lay_out(&mut screen, SMALLEST);
        assert_eq!(cut_off(&screen, screen.tabs_host), vec![], "{path}");
        // 条件をすべて解除 and the chip are inside the pane, pressable.
        let pane = screen.tree.window_rect(screen.tabs_host).unwrap();
        for action in [Action::ClearFilters, Action::FilterFile(None)] {
            let at = screen
                .tree
                .window_rect(widget_for(&screen, &action).unwrap())
                .unwrap();
            assert!(
                at.right() <= pane.right() + 0.5,
                "{action:?} {at:?} {pane:?}"
            );
        }
    }
    assert!(board::short_path(long).starts_with("…/"));
    assert!(board::short_path(long).chars().count() <= 32);
    assert_eq!(board::short_path("src/store.rs"), "src/store.rs");
}

#[test]
fn command_f_from_another_tab_goes_to_the_search_and_keeps_its_words() {
    let (mut screen, _link) = live_with(crate::demo::session());
    command_f(&mut screen);
    assert!(screen.search_open);
    assert_eq!(screen.tree.focused(), Some(screen.search));
    let mut text = TextStore::new(Default::default());
    screen.dispatch(noa_gpui_core::Event::Text("シェル".into()), &mut text);
    assert_eq!(screen.filters.query, "シェル");
    act(&mut screen, Action::ShowTab(CenterTab::Report));
    command_f(&mut screen);
    assert_eq!(screen.tab, CenterTab::Findings);
    assert!(screen.search_open);
    assert_eq!(screen.filters.query, "シェル");
    assert_eq!(screen.tree.focused(), Some(screen.search));
    // Where it shows, ⌘F closes it.
    command_f(&mut screen);
    assert!(!screen.search_open);
    assert!(screen.filters.query.is_empty());
}

#[test]
fn reviewing_again_from_far_down_the_list_shows_the_form_with_focus() {
    let mut screen = Review::demo_start();
    screen
        .tree
        .update::<noa_gpui_core::widgets::ScrollView, _>(screen.start.root, |s, cx| {
            s.set_offset(cx, 2000.0)
        });
    let scrolled = |screen: &Review| {
        screen
            .tree
            .get::<noa_gpui_core::widgets::ScrollView>(screen.start.root)
            .unwrap()
            .offset()
    };
    assert!(scrolled(&screen) > 0.0);
    let again = acts(&screen)
        .into_iter()
        .find(|a| matches!(a, Action::ReviewAgain(Some(_))))
        .unwrap();
    press_button(&mut screen, &again);
    let offset = screen
        .tree
        .get::<noa_gpui_core::widgets::ScrollView>(screen.start.root)
        .unwrap()
        .offset();
    assert_eq!(offset, 0.0);
    assert_eq!(screen.tree.focused(), Some(screen.start.repo));
}

#[test]
fn the_coverage_counts_a_file_as_its_button_does() {
    // F1 now sits in src/engine.rs; its sources still point into store.rs.
    let mut session = crate::demo::session();
    let f1 = session.findings.iter_mut().find(|f| f.id == "F1").unwrap();
    let raw_path = session_raw_path(&crate::demo::session(), "F1");
    f1.location = Some(review::model::types::Location {
        path: "src/engine.rs".into(),
        line: Some(1),
        end_line: None,
    });
    let (mut screen, _link) = live_with(session);
    act(&mut screen, Action::ShowTab(CenterTab::Matrix));
    let labels = labels_under(&screen.tree, screen.center_host);
    let at = labels.iter().position(|l| *l == raw_path).unwrap();
    let row: Vec<&String> = labels[at..].iter().take(5).collect();
    assert!(
        acts(&screen).contains(&Action::FilterFile(Some(raw_path.clone()))),
        "{row:?}"
    );
    assert!(!row.iter().any(|l| l.as_str() == "指摘なし"), "{row:?}");
    assert!(row.iter().any(|l| l.contains("高 1")), "{row:?}");
}

/// The path F's first source points into.
fn session_raw_path(session: &Session, id: &str) -> String {
    let f = session.finding(id).unwrap();
    session
        .raw_finding(&f.sources[0])
        .unwrap()
        .location
        .as_ref()
        .unwrap()
        .path
        .clone()
}

/// `n` stored copies of `log` under a new `root`, `rev-<base + i>`.
fn stored_many(
    name: &str,
    log: &[Vec<review::model::reduce::Event>],
    base: usize,
    n: usize,
) -> PathBuf {
    let root = std::env::temp_dir().join(format!("noa-review-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    for i in 0..n {
        let dir = root.join(format!("rev-{}", base + i));
        let mut store = Store::create(&dir, &log[0]).unwrap();
        store.append_all(&log[1..]).unwrap();
    }
    root
}

fn past_screen(root: &Path) -> (Review, crate::live::FakeLink) {
    let (channel, link) = Channel::fake();
    let mut screen = live_ready(channel);
    screen.past_root = Some(root.to_owned());
    screen.refresh_past();
    screen.rebuild(&mut TextStore::new(Default::default()));
    (screen, link)
}

/// Presses さらに表示 until it is gone (at most `limit` times).
fn page_to_the_end(screen: &mut Review, limit: usize) {
    for _ in 0..limit {
        if !acts(screen).contains(&Action::MorePast) {
            return;
        }
        act(screen, Action::MorePast);
    }
    panic!("さらに表示 never went away");
}

fn listed_dirs(screen: &Review) -> Vec<PathBuf> {
    screen.past.iter().map(|l| l.dir.clone()).collect()
}

#[test]
fn more_past_reviews_rereads_a_changed_log_and_asks_the_lock_again() {
    let n = start::PAST_LIMIT + 3;
    let root = stored_many("paging", &crate::demo::held_log(), 2_000, n);
    let (mut screen, _link) = past_screen(&root);
    // The first listed (unfinished) review was finished by another
    // window meanwhile, and another window takes the second.
    let first = screen.past[0].dir.clone();
    assert!(!screen.past[0].summary.as_ref().unwrap().closed);
    std::fs::remove_file(first.join(store::EVENTS_FILE)).unwrap();
    let done = crate::demo::log();
    let mut store = Store::create(&first, &done[0]).unwrap();
    store.append_all(&done[1..]).unwrap();
    drop(store);
    let (_held, _) = Store::open(&screen.past[1].dir).unwrap();
    act(&mut screen, Action::MorePast);
    assert_eq!(screen.past.len(), n);
    // Read again: finished, so 表示, not 再開.
    assert!(screen.past[0].summary.as_ref().unwrap().closed);
    assert!(!acts(&screen).contains(&Action::ResumePast(first.clone())));
    // Asked again: the held one is busy now.
    assert!(screen.past[1].summary.as_ref().unwrap().busy);
    // The oldest page is there, and the unfinished oldest resumes itself.
    let oldest = screen.past.last().unwrap().dir.clone();
    assert!(oldest.ends_with("rev-2000"));
    assert!(acts(&screen).contains(&Action::ResumePast(oldest)));
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn past_reviews_added_or_gone_between_pages_end_without_gaps_or_doubles() {
    let log = crate::demo::log();
    let n = 2 * start::PAST_LIMIT + 1;
    // A newer review arrives after the first page.
    let root = stored_many("paging-new", &log, 3_000, n);
    let (mut screen, _link) = past_screen(&root);
    let dir = root.join("rev-9999");
    let mut store = Store::create(&dir, &log[0]).unwrap();
    store.append_all(&log[1..]).unwrap();
    drop(store);
    page_to_the_end(&mut screen, 5);
    let dirs = listed_dirs(&screen);
    assert_eq!(dirs.len(), n + 1);
    assert_eq!(screen.past_total, n + 1);
    let unique: std::collections::BTreeSet<_> = dirs.iter().collect();
    assert_eq!(unique.len(), dirs.len());
    let _ = std::fs::remove_dir_all(&root);

    // The last review of the first page is taken away.
    let root = stored_many("paging-gone", &log, 4_000, n);
    let (mut screen, _link) = past_screen(&root);
    let last = screen.past.last().unwrap().dir.clone();
    std::fs::remove_dir_all(&last).unwrap();
    page_to_the_end(&mut screen, 5);
    let dirs = listed_dirs(&screen);
    assert_eq!(dirs.len(), n - 1);
    assert_eq!(screen.past_total, n - 1);
    assert!(!dirs.contains(&last));
    let unique: std::collections::BTreeSet<_> = dirs.iter().collect();
    assert_eq!(unique.len(), dirs.len());
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn a_noa_older_than_0_9_13_gets_a_reason_instead_of_fix_buttons() {
    for (version, offered) in [("0.9.12", false), ("0.9.13", true)] {
        let (mut screen, link) = live_with_dirs(crate::demo::session());
        link.versions.send((1, version.to_owned())).unwrap();
        screen.poll_live(&mut TextStore::new(Default::default()));
        assert_eq!(!fix_actions(&screen).is_empty(), offered, "{version}");
        assert_eq!(
            all_text(&screen).contains("0.9.13 以降が必要"),
            !offered,
            "{version}"
        );
    }
}

#[test]
fn a_finding_a_new_state_hides_lets_the_selection_and_the_fix_scope_go() {
    let (mut screen, link) = live_with_dirs(crate::demo::held());
    act(&mut screen, Action::SelectFinding(Some("F2".into())));
    assert!(
        fix_actions(&screen)
            .iter()
            .any(|(_, s)| *s == FixScope::Finding("F2".into()))
    );
    // The lead rejects F2; the default filters hide rejected findings.
    let mut next = crate::demo::held();
    let f2 = next.findings.iter_mut().find(|f| f.id == "F2").unwrap();
    f2.status = FindingStatus::Rejected;
    next.rev += 1;
    link.snapshots
        .send(Snapshot {
            link: Link::Live,
            session: Some(Arc::new(next)),
            reply_root: Some(PathBuf::from("/w/.noa/reviews/rev-demo-held")),
            session_dir: Some(PathBuf::from("/data/noa/reviews/rev-demo-held")),
            generation: 0,
        })
        .unwrap();
    screen.poll_live(&mut TextStore::new(Default::default()));
    assert_eq!(screen.selected, None);
    assert!(
        fix_actions(&screen)
            .iter()
            .all(|(_, s)| *s != FixScope::Finding("F2".into()))
    );
    // A request for it, sent before the screen moved on, takes nothing.
    act(
        &mut screen,
        Action::RequestFix {
            agent: AgentKind::Codex,
            scope: FixScope::Finding("F2".into()),
        },
    );
    assert!(
        !commands(&link)
            .iter()
            .any(|c| matches!(c, Command::RequestFix(_)))
    );
}

#[test]
fn a_stored_review_another_window_runs_shows_the_running_note_and_follows_its_lock() {
    let note = "レビュアーは作業ツリーも参照する";
    let root = std::env::temp_dir().join(format!("noa-review-held-note-{}", std::process::id()));
    let dir = stored_unfinished(&root);
    let held = Store::open(&dir).unwrap();
    let (channel, _link) = Channel::fake();
    let mut screen = stored_ready(channel, &dir);
    assert!(screen.read_only);
    assert!(all_text(&screen).contains(note));
    // The other window lets go: the note follows at the next look.
    drop(held);
    screen.busy_checked = None;
    screen.poll_live(&mut TextStore::new(Default::default()));
    assert!(!all_text(&screen).contains(note));
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn a_past_review_that_could_not_be_read_is_tried_again() {
    use std::os::unix::fs::PermissionsExt;
    let root = stored_many("unreadable", &crate::demo::log(), 5_000, 2);
    let log = root.join("rev-5001").join(store::EVENTS_FILE);
    std::fs::set_permissions(&log, std::fs::Permissions::from_mode(0o000)).unwrap();
    let (mut screen, _link) = past_screen(&root);
    let row = |screen: &Review| {
        screen
            .past
            .iter()
            .find(|l| l.dir.ends_with("rev-5001"))
            .unwrap()
            .summary
            .is_ok()
    };
    assert!(!row(&screen));
    // Readable again, same size and time: the next listing reads it.
    std::fs::set_permissions(&log, std::fs::Permissions::from_mode(0o644)).unwrap();
    screen.refresh_past();
    assert!(row(&screen));
    let _ = std::fs::remove_dir_all(root);
}

/// Writes `log` as the log in `dir`, as another window's runner would.
fn rewrite_log(dir: &Path, log: &[Vec<review::model::reduce::Event>]) {
    std::fs::remove_file(dir.join(store::EVENTS_FILE)).unwrap();
    let mut store = Store::create(dir, &log[0]).unwrap();
    store.append_all(&log[1..]).unwrap();
}

/// The stored view's next look at its log and lock (its 1 s floor passed).
fn look_again(screen: &mut Review) {
    screen.busy_checked = None;
    screen.poll_live(&mut TextStore::new(Default::default()));
}

#[test]
fn a_stored_review_finished_elsewhere_drops_resume_on_the_next_look() {
    let root = std::env::temp_dir().join(format!("noa-review-finished-{}", std::process::id()));
    let dir = stored_unfinished(&root);
    let (channel, _link) = Channel::fake();
    let mut screen = stored_ready(channel, &dir);
    assert!(acts(&screen).contains(&Action::ResumeShown));
    rewrite_log(&dir, &crate::demo::log());
    look_again(&mut screen);
    assert!(screen.session.as_ref().unwrap().closed);
    assert!(!acts(&screen).contains(&Action::ResumeShown));
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn a_stored_review_extended_elsewhere_shows_the_running_note_with_its_fix_buttons() {
    let note = "レビュアーは作業ツリーも参照する";
    let root = std::env::temp_dir().join(format!("noa-review-extended-{}", std::process::id()));
    let dir = stored_demo(&root);
    let (channel, _link) = Channel::fake();
    let mut screen = stored_ready(channel, &dir);
    assert!(screen.session.as_ref().unwrap().closed);
    assert!(!all_text(&screen).contains(note));
    // Another window took it up again (+N) and holds its log.
    let mut extended = crate::demo::log();
    extended.push(vec![review::model::reduce::Event::RoundsExtended {
        max_rounds: 4,
        at: 9_999_999,
    }]);
    rewrite_log(&dir, &extended);
    let (_held, session) = Store::open(&dir).unwrap();
    assert!(!session.closed);
    look_again(&mut screen);
    assert!(!screen.session.as_ref().unwrap().closed);
    assert!(all_text(&screen).contains(note));
    assert!(!fix_actions(&screen).is_empty());
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn the_fix_card_follows_the_version_of_each_noa_connected() {
    let (mut screen, link) = live_with_dirs(crate::demo::session());
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
}

#[test]
fn a_stored_review_finished_while_it_was_being_shown_is_caught_at_the_next_look() {
    let root = std::env::temp_dir().join(format!("noa-review-mid-show-{}", std::process::id()));
    let dir = stored_unfinished(&root);
    let finished = dir.clone();
    // Another window finishes it right after the view read the log.
    AFTER_STORED_READ.with(|h| {
        *h.borrow_mut() = Some(Box::new(move || {
            rewrite_log(&finished, &crate::demo::log())
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
fn a_change_that_could_not_be_read_is_read_at_a_later_look() {
    use std::os::unix::fs::PermissionsExt;
    let root =
        std::env::temp_dir().join(format!("noa-review-unread-change-{}", std::process::id()));
    let dir = stored_unfinished(&root);
    let (channel, _link) = Channel::fake();
    let mut screen = stored_ready(channel, &dir);
    rewrite_log(&dir, &crate::demo::log());
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
    let root = std::env::temp_dir().join(format!("noa-review-late-report-{}", std::process::id()));
    let dir = stored_demo(&root);
    // The writer holds the log, and has not written its report yet.
    let (held, session) = Store::open(&dir).unwrap();
    let (channel, _link) = Channel::fake();
    let mut screen = stored_ready(channel, &dir);
    assert!(screen.report_stale);
    store::write_report(&dir, &export::report_markdown(&session)).unwrap();
    drop(held);
    look_again(&mut screen);
    assert!(screen.stored_free);
    assert!(!acts(&screen).contains(&Action::RegenerateReport));
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn an_older_connections_version_arriving_late_does_not_win() {
    let (mut screen, link) = live_with_dirs(crate::demo::session());
    // The runner reconnected to a 0.9.12 (hello 2); a fix request's report
    // from an earlier connection (hello 1, 0.9.13) arrives after it.
    for report in [(2, "0.9.12"), (1, "0.9.13")] {
        link.versions.send((report.0, report.1.to_owned())).unwrap();
        screen.poll_live(&mut TextStore::new(Default::default()));
    }
    assert!(fix_actions(&screen).is_empty());
    assert!(all_text(&screen).contains("0.9.13 以降が必要"));
    // A newer connection to a 0.9.13 does.
    link.versions.send((3, "0.9.13".to_owned())).unwrap();
    screen.poll_live(&mut TextStore::new(Default::default()));
    assert!(!fix_actions(&screen).is_empty());
}
