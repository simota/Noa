//! Agent usage widget: rate limits and local-log statistics for the coding
//! agents Noa runs (Claude Code, Codex, Antigravity). Everything but the UI.
//! See `docs/specs/agent-usage.md`.

pub mod history;
pub mod limits;
pub mod model;
pub mod paths;
pub mod stats;
pub mod time;
