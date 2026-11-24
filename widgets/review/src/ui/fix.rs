//! 修正を依頼: the card in the right pane that starts a coding agent on
//! fixing findings (the verdict's must-fix ones, the ones the board shows,
//! or the selected one), in a new Noa tab in the review's workdir.

use noa_gpui_core::style::AlignItems;
use noa_gpui_core::widgets::{Button, Container, ControlSize};
use noa_gpui_core::{UiTree, WidgetId};

use review::model::types::AgentKind;

use super::{Action, caption, card, note};

/// Which findings a fix request takes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FixScope {
    /// The live findings the verdict must have fixed.
    MustFix,
    /// The findings the board shows with its filters.
    Shown,
    /// One finding (the selected one).
    Finding(String),
}

impl FixScope {
    fn label(&self, n: usize) -> String {
        match self {
            FixScope::MustFix => format!("要修正 {n} 件"),
            FixScope::Shown => format!("表示中の {n} 件"),
            FixScope::Finding(id) => format!("{id} のみ"),
        }
    }
}

/// What the card offers: the agents to pick from and the scopes with how
/// many findings each takes, or why a fix cannot be asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum FixOffer {
    Offered {
        kinds: Vec<AgentKind>,
        scopes: Vec<(FixScope, usize)>,
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
        FixOffer::Offered { kinds, scopes } => (kinds, scopes),
    };
    if scopes.is_empty() {
        note(tree, c, "依頼できる指摘はありません");
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
        "新しいタブでエージェントが起動します（自動承認なし）。依頼の内容は作業ディレクトリの .noa/reviews に残ります",
    );
}
