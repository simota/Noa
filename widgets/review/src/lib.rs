//! The review widget's core: target capture, the event-sourced model, the
//! engine that runs blind and cross-examination rounds, and the session
//! store. The binary (`main.rs`) adds the UI and the live link to Noa.

pub mod capture;
pub mod engine;
pub mod fsx;
pub mod model;
pub mod store;

#[cfg(test)]
pub(crate) mod testkit;
