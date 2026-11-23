//! 差分を見る: the Repo Browser on a worker's worktree, from the drawer's
//! button or D.

use std::cell::RefCell;
use std::rc::Rc;

use noa_gpui_core::widgets::Button;

use crate::repo_browser::{self, Launcher};

use super::*;

const BENCH: u64 = (1 << 63) | 1;
const TYPO: u64 = (1 << 63) | 2;

/// A Repo Browser that is not started: the cwds asked for, and what each
/// start answers.
#[derive(Clone, Default)]
struct Fake {
    asked: Rc<RefCell<Vec<String>>>,
    fails: Option<String>,
}

impl Launcher for Fake {
    fn launch(&mut self, cwd: &str) -> Result<(), String> {
        self.asked.borrow_mut().push(cwd.to_owned());
        self.fails.clone().map_or(Ok(()), Err)
    }
}

fn demo_with(fake: &Fake) -> (Dashboard, TextStore) {
    let mut dashboard =
        Dashboard::of_snapshot(Grouping::Orchestrator, live(fleet::canned_with_closed(), 0));
    dashboard.set_launcher(fake.clone());
    let mut text = TextStore::new(Default::default());
    dashboard.prepare(window(), &mut text);
    (dashboard, text)
}

#[test]
fn d_opens_the_worktree_of_an_open_worker() {
    let fake = Fake::default();
    let (mut dashboard, mut text) = demo_with(&fake);
    dashboard.select(Some(11), &mut text);
    key(&mut dashboard, &mut text, Key::Char('d'));
    assert_eq!(*fake.asked.borrow(), ["/work/noa-wt/api-refactor"]);
    assert_eq!(
        notice_of(&dashboard),
        "api-refactor の作業ツリーを Repo Browser で開きます"
    );
}

#[test]
fn the_drawers_button_opens_the_worktree_of_a_closed_worker() {
    let fake = Fake::default();
    let (mut dashboard, mut text) = demo_with(&fake);
    dashboard.select(Some(BENCH), &mut text);
    key(&mut dashboard, &mut text, Key::Space);
    let (button, pane) = dashboard.diff_button.expect("offered");
    assert_eq!(pane, BENCH);
    assert_eq!(
        dashboard.tree.get::<Button>(button).unwrap().label(),
        drawer::DIFF
    );
    let pos = center(&dashboard.tree, button);
    click(&mut dashboard, &mut text, pos);
    assert_eq!(*fake.asked.borrow(), ["/work/noa-wt/bench-ascii"]);
}

#[test]
fn a_worker_with_no_cwd_is_not_offered_it() {
    let fake = Fake::default();
    let (mut dashboard, mut text) = demo_with(&fake);
    dashboard.select(Some(TYPO), &mut text);
    key(&mut dashboard, &mut text, Key::Space);
    assert_eq!(dashboard.view.detail, Some(TYPO));
    assert!(dashboard.diff_button.is_none());
    key(&mut dashboard, &mut text, Key::Char('d'));
    assert!(fake.asked.borrow().is_empty());
    assert_eq!(
        notice_of(&dashboard),
        "docs-typo の作業ディレクトリがわかりません"
    );
}

#[test]
fn a_start_that_fails_says_why() {
    let fake = Fake {
        fails: Some(repo_browser::NOT_FOUND.to_owned()),
        ..Fake::default()
    };
    let (mut dashboard, mut text) = demo_with(&fake);
    dashboard.select(Some(17), &mut text);
    key(&mut dashboard, &mut text, Key::Char('d'));
    assert_eq!(notice_of(&dashboard), repo_browser::NOT_FOUND);
}

/// The real launcher from the test binary (`target/debug/deps/…`, neither
/// a bundle nor a dashboard build): nothing is found, nothing is started.
#[test]
fn no_repo_browser_beside_the_test_binary_starts_nothing() {
    let exe = std::env::current_exe().unwrap();
    assert_eq!(repo_browser::locate(&exe), None);
    let (mut dashboard, mut text) = laid_out(Grouping::Orchestrator);
    dashboard.select(Some(17), &mut text);
    key(&mut dashboard, &mut text, Key::Char('d'));
    assert_eq!(notice_of(&dashboard), repo_browser::NOT_FOUND);
}

#[test]
fn nothing_selected_says_so() {
    let fake = Fake::default();
    let (mut dashboard, mut text) = demo_with(&fake);
    key(&mut dashboard, &mut text, Key::Char('d'));
    assert!(fake.asked.borrow().is_empty());
    assert_eq!(notice_of(&dashboard), NOTHING_SELECTED);
}

/// The demo and a snapshot never start a program: D says so instead.
#[test]
fn the_demo_and_a_snapshot_start_nothing() {
    let mut demo = Dashboard::with_prefs(&Prefs::default(), fleet::canned());
    let mut text = TextStore::new(Default::default());
    demo.prepare(window(), &mut text);
    demo.select(Some(11), &mut text);
    key(&mut demo, &mut text, Key::Char('d'));
    assert_eq!(notice_of(&demo), DEMO_NO_LAUNCH);
    assert!(notice_of(&demo).starts_with("[DEMO]"));

    let (mut snap, mut text) = {
        let mut d =
            Dashboard::of_snapshot(Grouping::Orchestrator, live(fleet::canned_with_closed(), 0));
        let mut text = TextStore::new(Default::default());
        d.prepare(window(), &mut text);
        (d, text)
    };
    snap.select(Some(11), &mut text);
    key(&mut snap, &mut text, Key::Char('d'));
    assert_eq!(notice_of(&snap), SNAPSHOT_NO_LAUNCH);
}
