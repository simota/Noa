//! Limits sources (unit A owns this file; this is the D0 stub).

use std::sync::Arc;

use crate::model::UsageSource;
use crate::paths::Roots;

/// Dropping it stops every source thread and child process.
pub struct LimitsHandle;

pub fn start(_roots: Roots, _wake: Arc<dyn Fn() + Send + Sync>) -> LimitsHandle {
    LimitsHandle
}

impl LimitsHandle {
    /// Ask every source now (Claude: at least 180 s between requests).
    pub fn refresh(&self) {}

    /// Updates since the last call, newest last per agent.
    pub fn drain(&self) -> Vec<UsageSource> {
        Vec::new()
    }
}
