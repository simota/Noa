//! Runs one verification against a live Noa, on a thread of its own.
//!
//! STUB (U4 contract): the public items below are what `live.rs` and the
//! UI rely on; the bodies are filled in by the live part of U4.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender};

use verify::model::types::{
    AgentChoice, CriteriaEdit, Isolation, PartId, RoundNo, Session, Target, VerifierChoice,
};

use super::ipc::Connect;
use crate::live::Attachment;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Start {
    New(Box<NewVerification>),
    /// Continues the verification logged in `dir`.
    Resume {
        dir: PathBuf,
    },
    /// Gives the verification logged in `dir` `by` more rounds, then goes
    /// on as [`Start::Resume`].
    Extend {
        dir: PathBuf,
        by: RoundNo,
    },
}

/// What a new verification starts from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewVerification {
    pub request: String,
    pub target: Target,
    pub isolation: Isolation,
    pub user_criteria: Vec<String>,
    pub confirm: bool,
    pub max_rounds: RoundNo,
    pub round_cap: RoundNo,
    pub judge: AgentChoice,
    pub panel: Vec<VerifierChoice>,
    /// The store root (`<data dir>/noa/verifications`).
    pub root: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    EditCriteria {
        edit: CriteriaEdit,
    },
    StartVerify,
    Attach(Box<Attachment>),
    Intervene {
        to: Vec<PartId>,
        body: String,
    },
    Retry,
    Close,
    Focus {
        part: PartId,
    },
    SetHold {
        hold: bool,
    },
    Continue,
    Extend {
        by: RoundNo,
    },
    /// ペインをすべて閉じる: refused until the verification has ended.
    ClosePanes,
    /// ワークツリーを削除 on the ended verification this runner holds.
    RemoveWorktrees,
    Stop,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Link {
    Connecting,
    Live,
    Down(String),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Update {
    Session {
        session: Arc<Session>,
        /// The judge's reply dir.
        reply_root: PathBuf,
        session_dir: PathBuf,
    },
    Link(Link),
    Notice(String),
    Version(u64, String),
    Ended(Result<(), String>),
}

pub struct SessionHandle {
    pub updates: Receiver<Update>,
    actions: Sender<Action>,
}

impl SessionHandle {
    pub fn actions(&self) -> Sender<Action> {
        self.actions.clone()
    }
}

pub fn spawn_session(start: Start, wake: impl Fn() + Send + 'static) -> SessionHandle {
    let _ = (start, wake);
    let _ = (mpsc::channel::<Update>(), mpsc::channel::<Action>());
    todo!("U4 live: run the session")
}

/// How many of the stored verification's (in `dir`) judge and verifier
/// panes are open in Noa, found by their labels.
pub fn count_stored_panes<C: Connect>(connector: &mut C, dir: &Path) -> Result<usize, String> {
    let _ = (connector, dir);
    todo!("U4 live")
}

/// ペインをすべて閉じる for a finished stored verification: closes its
/// judge's and verifiers' panes found by label; the text is the notice.
pub fn close_stored_panes<C: Connect>(connector: &mut C, dir: &Path) -> Result<String, String> {
    let _ = (connector, dir);
    todo!("U4 live")
}

/// ワークツリーを削除 for a stored verification no runner holds.
pub fn remove_stored_worktrees(dir: &Path) -> Result<String, String> {
    let _ = dir;
    todo!("U4 live")
}

/// The login shell agents and the fix tab start in.
pub fn login_shell() -> String {
    todo!("U4 live")
}
