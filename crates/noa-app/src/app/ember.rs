//! Ember in noa's own process: the menu bar pane list reads the session
//! cards directly and jumps through the same focus path as `focusPane`, so it
//! needs no server, token or scope.

use std::hash::{Hash, Hasher};

use super::*;
use crate::ember::{EmberStatusItem, Pane, Tracker};
use crate::session_store::SessionCardId;

/// The status item and the per-pane `done.` memory behind it.
pub(super) struct EmberState {
    item: EmberStatusItem,
    tracker: Tracker,
    /// Hash of everything a row shows; rows are rebuilt only when it moves.
    signature: Option<u64>,
    home: Option<String>,
}

impl App {
    /// Show or hide Ember for this session; `ember` only sets the launch state.
    pub(super) fn toggle_ember(&mut self) {
        self.set_ember_visible(self.ember.is_none());
    }

    pub(super) fn set_ember_visible(&mut self, visible: bool) {
        if visible == self.ember.is_some() {
            return;
        }
        self.ember = visible.then(|| EmberState {
            item: EmberStatusItem::install(self.proxy.clone()),
            tracker: Tracker::default(),
            signature: None,
            home: dirs::home_dir().map(|home| home.to_string_lossy().into_owned()),
        });
        if let Some(menu) = self.macos_menu.as_ref() {
            menu.set_ember_checked(visible);
        }
        self.sync_ember();
    }

    /// Hand Ember fresh rows when a pane's shown state changed (a no-op while
    /// Ember is hidden).
    pub(super) fn sync_ember(&mut self) {
        let Some(ember) = self.ember.as_mut() else {
            return;
        };
        // Runs every event-loop pass, including during output floods, so the
        // unchanged case only hashes borrowed card fields.
        let cards: Vec<_> = self
            .session_store
            .ordered_cards()
            .into_iter()
            .filter(|(id, _)| {
                self.windows
                    .get(&WindowId::from(id.window_id.0))
                    .is_some_and(|state| state.contains_pane(id.pane_id))
            })
            .collect();
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        for (id, card) in &cards {
            id.hash(&mut hasher);
            (card.display_name(), &card.cwd, &card.branch, &card.process).hash(&mut hasher);
            (card.busy, card.attention, card.agent_needs_attention()).hash(&mut hasher);
        }
        let signature = hasher.finish();
        if ember.signature == Some(signature) {
            return;
        }
        ember.signature = Some(signature);
        let panes: Vec<Pane> = cards
            .into_iter()
            .map(|(id, card)| Pane::from_card(id, card))
            .collect();
        ember.tracker.observe(&panes);
        ember
            .item
            .set_rows(ember.tracker.rows(&panes, ember.home.as_deref()));
    }

    /// A row was picked: focus that pane and bring its window forward.
    pub(super) fn ember_jump(&mut self, id: SessionCardId) {
        let window_id = WindowId::from(id.window_id.0);
        let Some(window) = self
            .windows
            .get(&window_id)
            .filter(|state| state.contains_pane(id.pane_id))
            .map(|state| state.window.clone())
        else {
            return;
        };
        self.focused = Some(window_id);
        self.focus_pane(window_id, id.pane_id);
        window.focus_window();
        if let Some(ember) = self.ember.as_mut() {
            ember.tracker.acknowledge(id);
            ember.signature = None;
        }
        self.sync_ember();
    }
}
