//! The tasks widget's core (`docs/specs/task-widget.md`): one task model
//! over local task files and the repository's GitHub issues, the column
//! each task is in (derived, never stored), the run log of the agents
//! started on tasks, and the work trees they run in. The binary
//! (`main.rs`) adds the UI and the live link to Noa.

pub mod auto_assign;
pub mod code;
pub mod fsx;
pub mod github;
pub mod local;
pub mod model;
pub mod related;
pub mod runs;
pub mod shell;
pub mod sibling;
pub mod store;
pub mod task_links;
pub mod timeline;
pub mod workflow_links;
pub mod workspace;
pub mod worktree;

#[cfg(test)]
pub(crate) mod testkit;
