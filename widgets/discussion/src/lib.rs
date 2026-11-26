//! The discussion widget's core: the event-sourced model, the engine that
//! runs rounds, and the session store. The binary (`main.rs`) adds the UI
//! and the live link to Noa on top.

pub mod auto_assign;
pub mod engine;
pub mod model;
pub mod store;
