use super::*;
use crate::session_store::WallClock;

fn upsert(window: u64) -> SessionDelta {
    SessionDelta::Upsert {
        id: SessionCardId::new(SessionWindowId(window), PaneId::new(1)),
        seq: 1,
        name: "shell".to_string(),
        cwd: "/repo".to_string(),
        busy: false,
        updated_at: WallClock {
            year: 2026,
            month: 7,
            day: 5,
            hour: 10,
            minute: 0,
        },
        preview: Vec::new(),
    }
}

// F1 (FR-14/AC-16b): a quick-terminal window is ineligible, so its
// io-thread-posted Upsert/Bell are dropped at the apply boundary and never
// land in the store — even though the QT pane shares the app-wide publish
// gate. An eligible window's delta lands as normal.
#[test]
fn ineligible_window_deltas_are_dropped_at_the_apply_boundary() {
    let mut store = SessionStore::new();
    let delta = upsert(9);

    // Ineligible (quick-terminal) window: Upsert dropped, store stays empty.
    assert!(!session_delta_should_apply(&delta, false));
    if session_delta_should_apply(&delta, false) {
        store.apply(delta.clone());
    }
    assert_eq!(store.len(), 0);

    // Eligible window: the same Upsert lands.
    assert!(session_delta_should_apply(&delta, true));
    if session_delta_should_apply(&delta, true) {
        store.apply(delta);
    }
    assert_eq!(store.len(), 1);

    // Bell is gated the same way; Remove always applies (harmless for a QT).
    let id = SessionCardId::new(SessionWindowId(9), PaneId::new(1));
    assert!(!session_delta_should_apply(
        &SessionDelta::Bell { id },
        false
    ));
    assert!(!session_delta_should_apply(
        &SessionDelta::Attention { id },
        false
    ));
    assert!(session_delta_should_apply(
        &SessionDelta::Remove { id },
        false
    ));
}

// AC-10 (R2): windows_in_group returns exactly the target group's
// SessionWindowIds from a pair list mixing multiple groups and multiple
// tabs (distinct WindowIds) per group.
#[test]
fn windows_in_group_returns_only_the_target_groups_windows() {
    let group_a = WindowGroupId(1);
    let group_b = WindowGroupId(2);
    let pairs = [
        (SessionWindowId(10), group_a),
        (SessionWindowId(11), group_a), // sibling tab of window 10
        (SessionWindowId(20), group_b),
        (SessionWindowId(21), group_b),
    ];

    let result = windows_in_group(pairs, group_a);
    assert_eq!(
        result,
        [SessionWindowId(10), SessionWindowId(11)]
            .into_iter()
            .collect()
    );
}
