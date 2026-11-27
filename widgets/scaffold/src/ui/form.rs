//! The 関連付け / 省略にする / 決定を記録 forms, shown in the detail pane
//! under the selected item. The screen inserts a form once when it opens
//! (a rescan does not rebuild it), passes it every message, and closes it
//! on キャンセル (or Escape) and on a submit it accepts.

use noa_gpui_core::style::FlexDirection;
use noa_gpui_core::widgets::{
    Button, ButtonClicked, CheckState, Checkbox, Container, ControlSize, Label, SegmentMode,
    SegmentedControl, TextField, TextSubmitted,
};
use noa_gpui_core::{Message, Role, Style, UiTree, WidgetId};

use scaffold::model::{EvidenceKind, EvidenceRef, Lifecycle, Subject};
use scaffold::store::LinkKind;

use super::backend::Action;
use super::style;

const FIELD_WIDTH: f32 = 380.0;

/// Which form, and what it was opened on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Kind {
    Link {
        subject: Subject,
        kind: LinkKind,
    },
    Skip {
        subject: Subject,
    },
    Decide {
        asset: String,
        /// The stages the asset may move to.
        choices: Vec<Lifecycle>,
        /// The asset's links, offered as evidence.
        links: Vec<(LinkKind, String)>,
    },
}

/// What a message did to the form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Pending,
    Cancel,
    Submit(Action),
    Invalid(String),
}

#[derive(Debug, Clone, Default)]
struct Ids {
    choice: Option<WidgetId>,
    field: Option<WidgetId>,
    paths: Option<WidgetId>,
    checks: Vec<WidgetId>,
    submit: Option<WidgetId>,
    cancel: Option<WidgetId>,
}

#[derive(Debug, Clone)]
pub struct Form {
    pub kind: Kind,
    ids: Ids,
}

impl Form {
    pub fn new(kind: Kind) -> Form {
        Form {
            kind,
            ids: Ids::default(),
        }
    }

    pub fn heading(&self) -> &'static str {
        match &self.kind {
            Kind::Link { .. } => "関連付け",
            Kind::Skip { .. } => "省略にする",
            Kind::Decide { .. } => "決定を記録",
        }
    }

    /// The text field to focus when the form opens.
    pub fn first_field(&self) -> Option<WidgetId> {
        self.ids.field
    }

    /// Builds the form's widgets under `parent`.
    pub fn insert(&mut self, tree: &mut UiTree, parent: WidgetId) {
        let column = tree.insert(
            Some(parent),
            Container::new(Style {
                flex_direction: FlexDirection::Column,
                ..style::column(8.0)
            })
            .background(Role::Surface)
            .border(1.0, Role::Separator)
            .radius(6.0)
            .padding(10.0),
        );
        tree.insert(
            Some(column),
            Label::new(self.heading())
                .font_size(crate::appearance::font(13.0))
                .weight(700),
        );
        let mut ids = Ids::default();
        match &self.kind {
            Kind::Link { kind, .. } => {
                let selected = usize::from(*kind == LinkKind::Review);
                ids.choice = Some(
                    tree.insert(
                        Some(column),
                        SegmentedControl::new(["検証", "レビュー"], SegmentMode::SelectOne)
                            .control_size(ControlSize::Small)
                            .with_selected(&[selected]),
                    ),
                );
                ids.field = Some(tree.insert(
                    Some(column),
                    TextField::new("ver-… または rev-…", FIELD_WIDTH),
                ));
            }
            Kind::Skip { .. } => {
                ids.field =
                    Some(tree.insert(Some(column), TextField::new("省略する理由", FIELD_WIDTH)));
            }
            Kind::Decide { choices, links, .. } => {
                tree.insert(
                    Some(column),
                    Label::new("移す先")
                        .font_size(crate::appearance::font(11.0))
                        .color(Role::Muted),
                );
                ids.choice = Some(
                    tree.insert(
                        Some(column),
                        SegmentedControl::new(
                            choices.iter().map(|c| c.label()),
                            SegmentMode::SelectOne,
                        )
                        .control_size(ControlSize::Small)
                        .with_selected(&[0]),
                    ),
                );
                ids.field = Some(tree.insert(Some(column), TextField::new("理由", FIELD_WIDTH)));
                tree.insert(
                    Some(column),
                    Label::new("根拠（関連付けから選ぶ・パスを書く）")
                        .font_size(crate::appearance::font(11.0))
                        .color(Role::Muted),
                );
                for (kind, id) in links {
                    ids.checks.push(
                        tree.insert(
                            Some(column),
                            Checkbox::new(format!("{} {id}", kind.label()))
                                .control_size(ControlSize::Small),
                        ),
                    );
                }
                ids.paths = Some(tree.insert(
                    Some(column),
                    TextField::new("docs/x.md, scripts/y.sh", FIELD_WIDTH),
                ));
            }
        }
        let buttons = tree.insert(Some(column), Container::new(style::row(8.0)));
        let submit = match &self.kind {
            Kind::Link { .. } => "関連付ける",
            Kind::Skip { .. } => "省略を記録",
            Kind::Decide { .. } => "記録する",
        };
        ids.submit = Some(tree.insert(Some(buttons), Button::new(submit)));
        ids.cancel = Some(tree.insert(Some(buttons), Button::new("キャンセル")));
        self.ids = ids;
    }

    fn text(tree: &UiTree, id: Option<WidgetId>) -> String {
        id.and_then(|id| tree.get::<TextField>(id))
            .map(|f| f.text().to_owned())
            .unwrap_or_default()
    }

    fn choice(tree: &UiTree, id: Option<WidgetId>) -> usize {
        id.and_then(|id| tree.get::<SegmentedControl>(id))
            .and_then(|s| s.selected().iter().position(|on| *on))
            .unwrap_or(0)
    }

    /// Takes in a message from the tree.
    pub fn handle(&self, tree: &UiTree, message: &Message) -> Outcome {
        let source = Some(message.source);
        let clicked = message.downcast_ref::<ButtonClicked>().is_some();
        let entered = message.downcast_ref::<TextSubmitted>().is_some()
            && (source == self.ids.field || source == self.ids.paths);
        if clicked && source == self.ids.cancel {
            return Outcome::Cancel;
        }
        if (clicked && source == self.ids.submit) || entered {
            return self.submit(tree);
        }
        Outcome::Pending
    }

    /// The action of the form as filled in, or why it cannot be sent.
    pub fn submit(&self, tree: &UiTree) -> Outcome {
        let field = Self::text(tree, self.ids.field).trim().to_owned();
        match &self.kind {
            Kind::Link { subject, .. } => {
                if field.is_empty() {
                    return Outcome::Invalid("id を入れてください".into());
                }
                let kind = if Self::choice(tree, self.ids.choice) == 1 {
                    LinkKind::Review
                } else {
                    LinkKind::Verify
                };
                Outcome::Submit(Action::Link {
                    subject: subject.clone(),
                    kind,
                    id: field,
                })
            }
            Kind::Skip { subject } => {
                if field.is_empty() {
                    return Outcome::Invalid("省略の理由を書いてください".into());
                }
                Outcome::Submit(Action::Skip {
                    subject: subject.clone(),
                    reason: field,
                })
            }
            Kind::Decide {
                asset,
                choices,
                links,
            } => {
                let Some(to) = choices.get(Self::choice(tree, self.ids.choice)) else {
                    return Outcome::Invalid("移す先がありません".into());
                };
                let mut evidence = Vec::new();
                for ((kind, id), check) in links.iter().zip(&self.ids.checks) {
                    let on = tree
                        .get::<Checkbox>(*check)
                        .is_some_and(|c| c.state() == CheckState::On);
                    if on {
                        evidence.push(EvidenceRef {
                            kind: match kind {
                                LinkKind::Verify => EvidenceKind::Verify,
                                LinkKind::Review => EvidenceKind::Review,
                                LinkKind::Task => EvidenceKind::Task,
                            },
                            r#ref: id.clone(),
                        });
                    }
                }
                for path in Self::text(tree, self.ids.paths).split(',') {
                    let path = path.trim();
                    if path.is_empty() {
                        continue;
                    }
                    if path.starts_with('/') || path.split('/').any(|p| p == "..") {
                        return Outcome::Invalid(format!(
                            "根拠のパスはリポジトリからの相対パスで書いてください: {path}"
                        ));
                    }
                    evidence.push(EvidenceRef {
                        kind: EvidenceKind::Path,
                        r#ref: path.to_owned(),
                    });
                }
                Outcome::Submit(Action::Decide {
                    asset: asset.clone(),
                    to: *to,
                    reason: field,
                    evidence,
                })
            }
        }
    }
}
