//! Scaffold widget: the repository's development assets (checkers, runbooks,
//! rules), the widget-registration gaps found in it, and their hand-off to an
//! agent as a local task. Everything but the UI. See
//! `docs/specs/scaffold-widget.md`.
//!
//! Nothing here runs a process: the registration scan is a port of
//! `scripts/check-widget-registration.py`'s rules (I3) that reads files only,
//! because running the repo's checker would execute repo-controlled code.

pub mod gitread;
pub mod handoff;
pub mod hash;
pub mod model;
pub mod registry;
pub mod scan;
pub mod store;
pub mod time;
pub mod verify_log;
