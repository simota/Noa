//! The strategies a verifier can sit on (L1-9): their labels and the
//! checklists their briefs carry.

use crate::model::types::{BuiltinStrategy, Strategy, custom_label};

impl BuiltinStrategy {
    /// 仕様適合, 反証, 回帰, 実機・E2E, 非機能.
    pub fn label(self) -> &'static str {
        match self {
            BuiltinStrategy::Conformance => "仕様適合",
            BuiltinStrategy::Falsify => "反証",
            BuiltinStrategy::Regression => "回帰",
            BuiltinStrategy::E2e => "実機・E2E",
            BuiltinStrategy::Nonfunctional => "非機能",
        }
    }

    /// What a verifier on this strategy is told to do (L1-9 checklist).
    pub fn checklist(self) -> &'static str {
        match self {
            BuiltinStrategy::Conformance => {
                "Exercise each criterion's `how` as written. Record every command you run and \
                 save its output. Stop at the first deviation from what the criterion states \
                 and record it as the result, with the evidence that shows it."
            }
            BuiltinStrategy::Falsify => {
                "Try to make each criterion fail: boundaries, invalid input, empty and huge \
                 data, concurrency, interrupted runs. Write throwaway tests in your worktree. \
                 A criterion you could not break is a pass only with your attempts as \
                 evidence."
            }
            BuiltinStrategy::Regression => {
                "Run the repository's own checks (the commands in AGENTS.md or CLAUDE.md, the \
                 CI scripts) and the tests of the code the change touches. When a failure may \
                 be pre-existing, compare with the base: `git stash` is not allowed; check the \
                 base out in a second detached worktree of your own under your worktree's \
                 `.noa/`."
            }
            BuiltinStrategy::E2e => {
                "Build and run the product as a user would (GUI, CLI or server) and exercise \
                 each criterion through it. Capture screenshots, recordings or transcripts as \
                 `file` evidence."
            }
            BuiltinStrategy::Nonfunctional => {
                "Measure what the criteria or the change imply: time, memory, CPU, output \
                 size, security-relevant behaviour. Give numbers, each with the command that \
                 produced them."
            }
        }
    }
}

impl Strategy {
    /// The short label the UI shows: a built-in label, or the first 16
    /// characters of a custom text ([`crate::model::types::custom_label`]).
    pub fn label(&self) -> String {
        match self {
            Strategy::Builtin { id } => id.label().to_owned(),
            Strategy::Custom { text, .. } => custom_label(text),
        }
    }

    /// What the brief tells a verifier on it: the checklist, or the
    /// person's text.
    pub fn instructions(&self) -> &str {
        match self {
            Strategy::Builtin { id } => id.checklist(),
            Strategy::Custom { text, .. } => text.trim(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_builtin_strategy_has_a_label_and_a_checklist() {
        let labels: Vec<&str> = BuiltinStrategy::ALL.iter().map(|s| s.label()).collect();
        assert_eq!(labels, ["仕様適合", "反証", "回帰", "実機・E2E", "非機能"]);
        for s in BuiltinStrategy::ALL {
            assert!(!s.checklist().is_empty());
            let strategy = Strategy::Builtin { id: s };
            assert_eq!(strategy.label(), s.label());
            assert_eq!(strategy.instructions(), s.checklist());
        }
        let custom = Strategy::Custom {
            id: "custom".into(),
            text: "  Try every locale the app ships with  ".into(),
        };
        assert_eq!(custom.label(), "Try every locale…");
        assert_eq!(custom.instructions(), "Try every locale the app ships with");
    }
}
