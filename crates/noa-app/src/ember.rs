//! Ember: noa's companion in the macOS menu bar. Click the ghost to see every
//! pane ordered by what needs you first; pick one to jump there.

mod icon;
mod menu_bar;
mod model;

pub(crate) use menu_bar::EmberStatusItem;
pub(crate) use model::{Pane, Tracker};
