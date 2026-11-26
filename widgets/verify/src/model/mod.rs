//! The event-sourced verification state, its briefs, the validation of what
//! agents write, the evidence standard rulings are held to, the strategies
//! and the Markdown report. Pure: no I/O and no clock.

pub mod brief;
pub mod export;
pub mod recheck;
pub mod reduce;
pub mod rules;
pub mod strategy;
pub mod types;
pub mod validate;
