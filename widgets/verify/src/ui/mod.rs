//! The verification window. STUB (U4 contract): the items below are what
//! `main.rs` relies on; the UI part of U4 replaces this file.

use std::path::Path;
use std::sync::mpsc::Receiver;

use noa_gpui_core::{Message as UiMessage, UiTree};
use noa_gpui_platform::Screen;
use noa_gpui_text::TextStore;

use crate::installed::Installed;
use crate::live::{Channel, Snapshot};

pub(crate) const MIN_WINDOW: (f64, f64) = (1144.0, 640.0);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CenterTab {
    Criteria,
    Evidence,
    /// The evidence tab in ギャラリー mode.
    Gallery,
    Defects,
    Report,
}

impl CenterTab {
    pub fn parse(name: &str) -> Option<Self> {
        Some(match name {
            "criteria" => CenterTab::Criteria,
            "evidence" => CenterTab::Evidence,
            "gallery" => CenterTab::Gallery,
            "defects" => CenterTab::Defects,
            "report" => CenterTab::Report,
            _ => return None,
        })
    }
}

pub struct Verify {
    tree: UiTree,
}

impl Verify {
    pub fn demo() -> Self {
        todo!("U4 ui")
    }
    pub fn demo_start() -> Self {
        todo!("U4 ui")
    }
    pub fn demo_held() -> Self {
        todo!("U4 ui")
    }
    pub fn demo_criteria() -> Self {
        todo!("U4 ui")
    }
    pub fn stored(dir: &Path) -> Result<Self, String> {
        let _ = dir;
        todo!("U4 ui")
    }
    pub fn of_snapshot(snapshot: Snapshot) -> Self {
        let _ = snapshot;
        todo!("U4 ui")
    }
    pub fn live(channel: Channel, installed: Receiver<Installed>) -> Self {
        let _ = (channel, installed);
        todo!("U4 ui")
    }
    pub fn live_stored(channel: Channel, installed: Receiver<Installed>, dir: &Path) -> Self {
        let _ = (channel, installed, dir);
        todo!("U4 ui")
    }
    /// Puts `dir` in the start view's repository field (`--repo`).
    pub fn with_place(self, dir: &Path) -> Self {
        let _ = dir;
        self
    }
    /// Picks the center tab and the selected criterion.
    pub fn with_view(self, tab: Option<CenterTab>, criterion: Option<String>) -> Self {
        let _ = (tab, criterion);
        self
    }
}

impl Screen for Verify {
    fn tree(&self) -> &UiTree {
        &self.tree
    }
    fn tree_mut(&mut self) -> &mut UiTree {
        &mut self.tree
    }
    fn handle(&mut self, message: UiMessage, text: &mut TextStore) {
        let _ = (message, text);
    }
}
