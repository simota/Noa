//! The verify widget's core: target capture with its rebuild recipe, one
//! worktree per verifier, evidence ingest and hashing, media sniffing, the
//! re-run script, the event-sourced model, the engine that runs the
//! criteria phase and the verification rounds, and the session store. The
//! binary (`main.rs`) adds the UI and the live link to Noa.

pub mod capture;
pub mod engine;
pub mod evidence;
pub mod fsx;
pub mod links;
pub mod media;
pub mod model;
pub mod rerun;
pub mod store;
pub mod worktree;

#[cfg(test)]
pub(crate) mod testkit;
