//! Log statistics (unit B owns this file; this is the D0 stub).

pub mod types;

use std::sync::Arc;

use crate::model::Unix;
use crate::paths::Roots;
use crate::time::Tz;
use types::{AgentFilter, Period, StatsReport};

/// Buckets, sessions, interned strings, workers and coverage of every parsed log.
#[derive(Debug, Default)]
pub struct Index;

pub struct StatsHandle {
    index: Arc<Index>,
}

pub fn start(_roots: Roots, _wake: Arc<dyn Fn() + Send + Sync>) -> StatsHandle {
    StatsHandle {
        index: Arc::new(Index),
    }
}

impl StatsHandle {
    pub fn rescan(&self) {}

    /// The latest index; a cheap `Arc` clone.
    pub fn index(&self) -> Arc<Index> {
        self.index.clone()
    }
}

pub fn report(
    _index: &Index,
    _period: Period,
    _filter: AgentFilter,
    now: Unix,
    _tz: &dyn Tz,
) -> StatsReport {
    StatsReport::empty((now, now))
}
