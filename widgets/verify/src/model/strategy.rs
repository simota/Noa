//! The strategies a verifier can sit on (L1-9): their labels and the
//! checklists their briefs carry.

use crate::model::types::{BuiltinStrategy, Strategy};

impl BuiltinStrategy {
    /// 仕様適合, 反証, 回帰, 実機・E2E, 非機能.
    pub fn label(self) -> &'static str {
        todo!("U2: BuiltinStrategy::label")
    }

    /// What a verifier on this strategy is told to do (L1-9 checklist).
    pub fn checklist(self) -> &'static str {
        todo!("U2: BuiltinStrategy::checklist")
    }
}

impl Strategy {
    /// The short label the UI shows: a built-in label, or the first 16
    /// characters of a custom text ([`crate::model::types::custom_label`]).
    pub fn label(&self) -> String {
        todo!("U2: Strategy::label")
    }

    /// What the brief tells a verifier on it: the checklist, or the
    /// person's text.
    pub fn instructions(&self) -> &str {
        todo!("U2: Strategy::instructions")
    }
}
