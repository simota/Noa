//! A fix request (修正を依頼). STUB (U4 contract): filled in by the live
//! part of U4.

use std::path::PathBuf;
use std::sync::Arc;

use verify::model::types::{AgentKind, Session};

use super::ipc::Connect;

/// What the screen asks for: these failed criteria and defects of the
/// verification stored in `dir`, fixed by `agent`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FixRequest {
    pub session: Arc<Session>,
    /// The verification's store directory.
    pub dir: PathBuf,
    pub agent: AgentKind,
    /// Failed criterion ids.
    pub criteria: Vec<String>,
    /// Defect ids.
    pub defects: Vec<String>,
}

/// Carries out `req`; `Ok` and `Err` both say what happened.
pub fn request<C: Connect>(
    connector: &mut C,
    req: &FixRequest,
    seen: &mut Option<(u64, String)>,
) -> Result<String, String> {
    let _ = (connector, req, seen);
    todo!("U4 live")
}
