//! The view saved as it changes and restored at the next start.

use std::path::Path;

use super::*;

fn temp(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "agent-dashboard-saved-view-{name}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    dir.join("view.json")
}

fn started(path: &Path, tab: Option<Tab>) -> (Dashboard, TextStore) {
    let mut store = PrefsStore::at(path.to_owned());
    let prefs = store.load().overridden(tab, None, None);
    let mut dashboard = Dashboard::with_prefs(&prefs, fleet::canned()).persisting(store);
    let mut text = TextStore::new(Default::default());
    dashboard.prepare(window(), &mut text);
    (dashboard, text)
}

#[test]
fn a_changed_view_comes_back_at_the_next_start() {
    let path = temp("restart");
    let (mut dashboard, mut text) = started(&path, None);
    assert!(!path.exists(), "nothing changed, nothing written");
    key(&mut dashboard, &mut text, Key::Char('g'));
    key(&mut dashboard, &mut text, Key::Char('3'));
    typed(&mut dashboard, &mut text, "/");
    typed(&mut dashboard, &mut text, "agent:claude");
    key(&mut dashboard, &mut text, Key::Enter);
    key(&mut dashboard, &mut text, Key::Char('t'));
    key(&mut dashboard, &mut text, Key::Char('t'));
    // On the map, its axis; then the timeline's scale. Their segmented
    // controls change the view so (`Screen::handle`); a window closing
    // saves what input did not.
    let view = View {
        map_axis: MapAxis::Branch,
        ..dashboard.view
    };
    dashboard.change_view(view, &mut text);
    key(&mut dashboard, &mut text, Key::Char('t'));
    let view = View {
        time_scale: TimeScale::Quarter,
        ..dashboard.view
    };
    dashboard.change_view(view, &mut text);
    dashboard.close_requested();
    drop(dashboard);

    let (again, _text) = started(&path, None);
    assert_eq!(again.view.tab, Tab::Timeline);
    assert_eq!(again.view.grouping, Grouping::Agent);
    assert_eq!(again.view.shown, [true, true, false, true, true]);
    assert_eq!(again.view.map_axis, MapAxis::Branch);
    assert_eq!(again.view.time_scale, TimeScale::Quarter);
    assert_eq!(again.filter.query, "agent:claude");
    let field = again
        .tree
        .get::<TextField>(again.header.filter_field)
        .unwrap();
    assert_eq!(field.text(), "agent:claude");

    // --tab wins, and is not written until the view changes.
    let (mut dashboard, mut text) = started(&path, Some(Tab::Analytics));
    assert_eq!(dashboard.view.tab, Tab::Analytics);
    assert_eq!(dashboard.view.grouping, Grouping::Agent);
    dashboard.close_requested();
    let (kept, _) = started(&path, None);
    assert_eq!(
        kept.view.tab,
        Tab::Timeline,
        "the --tab run changed nothing"
    );
    key(&mut dashboard, &mut text, Key::Char('g'));
    let (changed, _) = started(&path, None);
    assert_eq!(changed.view.tab, Tab::Analytics);
    assert_eq!(changed.view.grouping, Grouping::Model);
    std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
}

#[test]
fn a_corrupt_saved_view_starts_from_the_defaults() {
    let path = temp("corrupt");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, "{ not json").unwrap();
    let (dashboard, _text) = started(&path, None);
    assert_eq!(dashboard.view.tab, Tab::Operations);
    assert_eq!(dashboard.view.grouping, Grouping::Orchestrator);
    assert_eq!(dashboard.view.shown, [true; 5]);
    assert!(dashboard.filter.query.is_empty());
    std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
}
