//! The event-sourced competition state, its briefs, its validation of agent
//! output, the judge panel's aggregation, Markdown export, and the
//! standalone HTML comparison viewer.

pub mod aggregate;
pub mod brief;
pub mod export;
pub mod map;
pub mod reduce;
pub mod types;
pub mod validate;
pub mod viewer;
