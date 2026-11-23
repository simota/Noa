use super::*;

use review::model::types::{LensChoice, TargetKind};
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
    act(
        &mut screen,
        Action::FilterReviewer(Some(PartId::new("agy"))),
    );
    assert_eq!(
        rows(&screen),
        4,
        "agy took a stance on F1, F2, F3 and raised F5"
    );
    act(&mut screen, Action::ClearFilters);
    assert_eq!(rows(&screen), 5);
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
    let mut screen = Review::live(channel);
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
    let mut screen = Review::live(channel);
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
