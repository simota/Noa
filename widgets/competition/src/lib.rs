//! The competition widget's core: the event-sourced model, the engine that
//! runs blind and peer-reveal rounds, the session store, and the comparison
//! viewer generator. The binary (`main.rs`) adds the UI and the live link to
//! Noa on top.

pub mod auto_assign;
pub mod check;
pub mod engine;
pub mod model;
pub mod preview;
pub mod shots;
pub mod store;
