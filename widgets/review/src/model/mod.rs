//! The event-sourced review state, its briefs, the validation of what agents
//! write, and the Markdown report. Pure: no I/O and no clock.

pub mod brief;
pub mod export;
pub mod reduce;
pub mod types;
pub mod validate;
