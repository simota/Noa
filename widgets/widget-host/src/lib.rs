//! A renderer for data-only repository widgets. It never starts repository
//! programs, fetches URLs, evaluates source text, or reads a control token.

pub mod args;
pub mod bridge;
pub mod catalog;
pub mod images;
pub mod render;
pub mod screen;
mod style;

#[path = "../../shared/appearance.rs"]
pub mod appearance;
