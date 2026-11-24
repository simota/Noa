//! 修正を依頼: the card in the right pane that starts a coding agent on
//! fixing failed criteria and defects (every failed criterion, every live
//! defect, or the selection), in a new Noa tab in the target's checkout.

use noa_gpui_core::style::AlignItems;
use noa_gpui_core::widgets::{Button, Container, ControlSize};
use noa_gpui_core::{UiTree, WidgetId};

use verify::model::types::{AgentKind, Isolation};

use super::{Action, caption, card, note};

/// What a fix request takes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FixScope {
    /// Every criterion whose latest ruling is 不合格.
    FailedCriteria,
    /// Every live defect (open, agreed, disputed).
    Defects,
    /// The selected criterion.
    Criterion(String),
    /// The selected defect.
    Defect(String),
}

impl FixScope {
    fn label(&self, n: usize) -> String {
        match self {
            FixScope::FailedCriteria => format!("不合格の基準 {n} 件"),
            FixScope::Defects => format!("不具合 {n} 件"),
            FixScope::Criterion(id) | FixScope::Defect(id) => format!("{id} のみ"),
        }
    }
}

/// What the card offers: the agents to pick from and the scopes with how
/// many criteria and defects each takes, or why a fix cannot be asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum FixOffer {
    Offered {
        kinds: Vec<AgentKind>,
        scopes: Vec<(FixScope, usize)>,
        /// The verification is still running, and how its verifiers see
        /// the checkout the fix changes.
        running: Option<Isolation>,
    },
    Unavailable(String),
}

pub(super) fn insert(
    tree: &mut UiTree,
    parent: WidgetId,
    offer: &FixOffer,
    actions: &mut Vec<(WidgetId, Action)>,
) {
    let c = card(tree, parent, false);
    caption(tree, c, "修正を依頼");
    let (kinds, scopes) = match offer {
        FixOffer::Unavailable(why) => {
            note(tree, c, why.clone());
            return;
        }
        FixOffer::Offered {
            kinds,
            scopes,
            running,
        } => {
            match running {
                Some(Isolation::Worktree) => note(
                    tree,
                    c,
                    "検証の実行中です。修正は対象のチェックアウトに入り、検証者の worktree には入りません（判定は開始時のスナップショットに基づきます）",
                ),
                Some(Isolation::Shared) => note(
                    tree,
                    c,
                    "検証の実行中です。共有モードの検証者は同じチェックアウトで動くため、修正が見えることがあります",
                ),
                None => {}
            }
            (kinds, scopes)
        }
    };
    if scopes.is_empty() {
        note(tree, c, "依頼できる不合格の基準・不具合はありません");
        return;
    }
    let small = |label: &str| Button::new(label).control_size(ControlSize::Small);
    for (scope, n) in scopes {
        let row = tree.insert(
            Some(c),
            Container::wrapping_row(6.0, 4.0).align_items(AlignItems::CENTER),
        );
        let label = scope.label(*n);
        caption(tree, row, format!("{label}:"));
        for &agent in kinds {
            let button = tree.insert(Some(row), small(agent.command()));
            tree.set_accessible_label(button, format!("{} に{label}の修正を依頼", agent.command()));
            actions.push((
                button,
                Action::RequestFix {
                    agent,
                    scope: scope.clone(),
                },
            ));
        }
    }
    note(
        tree,
        c,
        "新しいタブでエージェントが起動します（自動承認なし）。依頼の内容は対象のチェックアウトの .noa/verify に残ります",
    );
}
