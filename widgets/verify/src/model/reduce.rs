//! The event log's fold: a session is `create` of the first batch, then
//! `apply` of every later one. Each batch bumps `rev` once. The events
//! themselves are in `types` (frozen with the other shared types).

use std::fmt;

pub use crate::model::types::Event;
use crate::model::types::{Defect, Session, Tally};

/// The log does not fold: an event names something that is not there, or
/// breaks the order the engine writes in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReduceError(pub String);

impl fmt::Display for ReduceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ReduceError {}

/// The session the first batch creates (it starts with `Created`).
pub fn create(_batch: &[Event]) -> Result<Session, ReduceError> {
    todo!("U2: reduce::create")
}

/// Applies one batch and bumps `rev`.
pub fn apply(_session: &mut Session, _batch: &[Event]) -> Result<(), ReduceError> {
    todo!("U2: reduce::apply")
}

/// The session a whole log folds into.
pub fn replay<I>(_batches: I) -> Result<Session, ReduceError>
where
    I: IntoIterator,
    I::Item: AsRef<[Event]>,
{
    todo!("U2: reduce::replay")
}

impl Session {
    /// Who supports and disputes `defect`, from its sources and the
    /// cross-checks on it (review L2-4).
    pub fn tally(&self, _defect: &Defect) -> Tally {
        todo!("U2: Session::tally")
    }

    /// Defects ranked (review L2-4): severity, status, confidence,
    /// supporters, opening round, then their id (D2 before D10).
    pub fn ranked(&self) -> Vec<&Defect> {
        todo!("U2: Session::ranked")
    }
}
