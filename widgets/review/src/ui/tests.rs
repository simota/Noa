use super::*;

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
