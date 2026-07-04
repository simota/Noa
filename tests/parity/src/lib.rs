//! `noa-parity` — fixture-based screen-dump regression harness.
//!
//! Feeds raw byte streams through the real parse→state pipeline
//! ([`noa_vt::Stream`] → [`noa_grid::Terminal`]) and renders the final screen
//! as a stable, diffable text dump. Fixture files under `fixtures/` pin the
//! observable behavior; the same runner is the seam where a Ghostty oracle
//! (and esctest2/vttest corpora) plugs in later. See `README.md` in this
//! directory for the fixture format and the bless workflow.

pub mod diff;
pub mod dump;
pub mod fixture;

#[cfg(test)]
mod tests;

pub use diff::render_diff;
pub use dump::{DumpMode, run_fixture, run_fixture_with_mode};
pub use fixture::Fixture;
