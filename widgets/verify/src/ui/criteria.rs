//! The 基準 tab before round 1 (L1-2): the judge's criteria held for the
//! person to confirm, editable — statement, how and priority changed, a
//! criterion added, removed (an `inferred` or `request` one only) or moved
//! — each change can be saved on its own. Starting saves every remaining
//! edit and freezes the criteria together. What is typed and not yet saved
//! survives the rebuilds a new state of the verification makes.

use std::collections::BTreeMap;

use noa_gpui_core::style::AlignItems;
use noa_gpui_core::widgets::{
    Button, Container, ControlSize, Label, PopUpButton, TextArea, TextField, Tone,
};
use noa_gpui_core::{UiTree, WidgetId};

use verify::model::types::{
    CriteriaEdit, Criterion, CriterionSource, MAX_CRITERIA, MAX_STATEMENT, Priority, Session,
};

use super::{Action, caption, card, keep_scroll, note, scroll_column, spacer, tag};

/// `あなた`, `依頼`, `推定`, `仕様 12 行目`.
pub(super) fn source_label(source: CriterionSource) -> String {
    match source {
        CriterionSource::User => "あなた".to_owned(),
        CriterionSource::Request => "依頼".to_owned(),
        CriterionSource::Inferred => "推定".to_owned(),
        CriterionSource::Spec { line } => format!("仕様 {line} 行目"),
    }
}

/// L1-2: only what the judge added on its own may go; the person's lines
/// and the spec's stay (they can be edited).
pub(super) fn removable(c: &Criterion) -> bool {
    matches!(
        c.source,
        CriterionSource::Inferred | CriterionSource::Request
    )
}

pub(super) struct Row {
    pub(super) id: String,
    pub(super) statement: WidgetId,
    pub(super) how: WidgetId,
    pub(super) priority: WidgetId,
    /// The statement and how the fields were filled with.
    built: (String, String),
}

pub(super) struct AddRow {
    pub(super) statement: WidgetId,
    pub(super) how: WidgetId,
    pub(super) priority: WidgetId,
}

/// The editor's widgets and what is typed in them, kept by the screen.
#[derive(Default)]
pub(super) struct Editor {
    pub(super) rows: Vec<Row>,
    pub(super) add: Option<AddRow>,
    /// Statement and how typed into a criterion's fields and not saved.
    pub(super) drafts: BTreeMap<String, (String, String)>,
    /// Statement, how and priority chosen in the add row.
    add_draft: AddDraft,
    pending_label: Option<WidgetId>,
}

pub(super) struct ConfirmationError {
    criterion: Option<String>,
    statement: bool,
    pub(super) reason: String,
}

/// What the add row holds while it is not sent.
#[derive(Clone)]
struct AddDraft {
    statement: String,
    how: String,
    priority: Priority,
}

impl Default for AddDraft {
    fn default() -> Self {
        Self {
            statement: String::new(),
            how: String::new(),
            priority: Priority::Must,
        }
    }
}

fn field_text(tree: &UiTree, id: WidgetId) -> String {
    tree.get::<TextField>(id)
        .map(|f| f.text().to_owned())
        .unwrap_or_default()
}

fn area_text(tree: &UiTree, id: WidgetId) -> String {
    tree.get::<TextArea>(id)
        .map(|a| a.text().to_owned())
        .unwrap_or_default()
}

fn priority_of(tree: &UiTree, id: WidgetId) -> Priority {
    tree.get::<PopUpButton>(id)
        .and_then(|p| Priority::ALL.get(p.selected()).copied())
        .unwrap_or(Priority::Must)
}

impl Editor {
    /// Keeps what is typed in the fields about to be rebuilt.
    pub(super) fn harvest(&mut self, tree: &UiTree) {
        for row in &self.rows {
            let now = (field_text(tree, row.statement), area_text(tree, row.how));
            if now != row.built {
                self.drafts.insert(row.id.clone(), now);
            } else {
                self.drafts.remove(&row.id);
            }
        }
        if let Some(add) = &self.add {
            self.add_draft = AddDraft {
                statement: field_text(tree, add.statement),
                how: area_text(tree, add.how),
                priority: priority_of(tree, add.priority),
            };
        }
        self.rows.clear();
        self.add = None;
        self.pending_label = None;
    }

    /// Forgets every draft (another verification is shown).
    pub(super) fn reset(&mut self) {
        *self = Editor::default();
    }

    /// The criterion whose priority menu `source` is.
    pub(super) fn priority_source(&self, source: WidgetId) -> Option<&str> {
        self.rows
            .iter()
            .find(|r| r.priority == source)
            .map(|r| r.id.as_str())
    }

    /// The priority the menu of criterion `id` shows.
    pub(super) fn priority(&self, tree: &UiTree, id: &str) -> Option<Priority> {
        let row = self.rows.iter().find(|r| r.id == id)?;
        Some(priority_of(tree, row.priority))
    }

    fn values(&self, tree: &UiTree, criterion: &Criterion) -> (String, String) {
        if let Some(row) = self.rows.iter().find(|row| row.id == criterion.id) {
            return (field_text(tree, row.statement), area_text(tree, row.how));
        }
        self.drafts
            .get(&criterion.id)
            .cloned()
            .unwrap_or_else(|| (criterion.statement.clone(), criterion.how.clone()))
    }

    fn addition_draft(&self, tree: &UiTree) -> AddDraft {
        self.add.as_ref().map_or_else(
            || self.add_draft.clone(),
            |add| AddDraft {
                statement: field_text(tree, add.statement),
                how: area_text(tree, add.how),
                priority: priority_of(tree, add.priority),
            },
        )
    }

    pub(super) fn pending_count(&self, tree: &UiTree, session: &Session) -> usize {
        let changed = session
            .criteria
            .iter()
            .filter(|criterion| {
                let (statement, how) = self.values(tree, criterion);
                statement.trim() != criterion.statement || how.trim() != criterion.how
            })
            .count();
        let add = self.addition_draft(tree);
        changed + usize::from(!add.statement.trim().is_empty() || !add.how.trim().is_empty())
    }

    pub(super) fn update_pending(&self, tree: &mut UiTree, session: &Session) {
        if let Some(label) = self.pending_label {
            let count = self.pending_count(tree, session);
            let message = if count == 0 {
                "変更は保存済みです".to_owned()
            } else {
                format!("未保存の変更 {count}件（検証を始めるときにまとめて保存します）")
            };
            tree.update::<Label, _>(label, |label, cx| label.set_text(cx, message));
        }
    }

    pub(super) fn edits_field(&self, source: WidgetId) -> bool {
        self.rows.iter().any(|row| source == row.statement || source == row.how)
            || self.add.as_ref().is_some_and(|add| source == add.statement || source == add.how)
    }

    /// Collects the current fields, including drafts from another tab.
    /// A partly written addition must be completed or cleared, never lost
    /// when the existing criteria are frozen.
    pub(super) fn pending(
        &self,
        tree: &UiTree,
        session: &Session,
    ) -> Result<Vec<CriteriaEdit>, ConfirmationError> {
        let mut edits = Vec::new();
        for criterion in &session.criteria {
            let (statement, how) = self.values(tree, criterion);
            let statement = statement.trim().to_owned();
            let how = how.trim().to_owned();
            check(&statement, &how).map_err(|reason| ConfirmationError {
                criterion: Some(criterion.id.clone()),
                statement: !valid_statement(&statement),
                reason: format!("{}: {reason}", criterion.id),
            })?;
            let statement = (statement != criterion.statement).then_some(statement);
            let how = (how != criterion.how).then_some(how);
            if statement.is_some() || how.is_some() {
                edits.push(CriteriaEdit::Edit {
                    id: criterion.id.clone(),
                    statement,
                    how,
                    priority: None,
                });
            }
        }
        let add = self.addition_draft(tree);
        let statement = add.statement.trim().to_owned();
        let how = add.how.trim().to_owned();
        if !statement.is_empty() || !how.is_empty() {
            check(&statement, &how).map_err(|reason| ConfirmationError {
                criterion: None,
                statement: !valid_statement(&statement),
                reason: format!("追加する基準: {reason}（追加しない場合は入力を消してください）"),
            })?;
            if session.criteria.len() >= MAX_CRITERIA {
                return Err(ConfirmationError {
                    criterion: None,
                    statement: true,
                    reason: format!("基準は{MAX_CRITERIA}件までです。追加欄を空にするか、不要な基準を削除してください"),
                });
            }
            edits.push(CriteriaEdit::Add { statement, how, priority: add.priority });
        }
        Ok(edits)
    }

    pub(super) fn focus_error(&self, tree: &mut UiTree, error: &ConfirmationError) {
        let fields = match &error.criterion {
            Some(id) => self.rows.iter().find(|row| &row.id == id).map(|row| (row.statement, row.how)),
            None => self.add.as_ref().map(|add| (add.statement, add.how)),
        };
        if let Some((statement, how)) = fields {
            tree.set_focus(Some(if error.statement { statement } else { how }));
        }
    }

    /// The edit 変更を保存 sends for criterion `id`: the fields that
    /// differ from `session`'s, or why there is none to send.
    pub(super) fn edit(
        &self,
        tree: &UiTree,
        session: &Session,
        id: &str,
    ) -> Result<CriteriaEdit, String> {
        let row = self
            .rows
            .iter()
            .find(|r| r.id == id)
            .ok_or_else(|| format!("{id} は表示されていません"))?;
        let c = session
            .criterion(id)
            .ok_or_else(|| format!("{id} という基準はありません"))?;
        let statement = field_text(tree, row.statement).trim().to_owned();
        let how = area_text(tree, row.how).trim().to_owned();
        check(&statement, &how)?;
        let statement = (statement != c.statement).then_some(statement);
        let how = (how != c.how).then_some(how);
        if statement.is_none() && how.is_none() {
            return Err(format!("{id} に変更はありません"));
        }
        Ok(CriteriaEdit::Edit {
            id: id.to_owned(),
            statement,
            how,
            priority: None,
        })
    }

    /// The criterion the add row describes, or why it cannot be added.
    pub(super) fn addition(
        &self,
        tree: &UiTree,
        session: &Session,
    ) -> Result<CriteriaEdit, String> {
        let add = self.add.as_ref().ok_or("基準を追加できません")?;
        if session.criteria.len() >= MAX_CRITERIA {
            return Err(format!("基準は{MAX_CRITERIA}件までです"));
        }
        let statement = field_text(tree, add.statement).trim().to_owned();
        let how = area_text(tree, add.how).trim().to_owned();
        check(&statement, &how)?;
        Ok(CriteriaEdit::Add {
            statement,
            how,
            priority: priority_of(tree, add.priority),
        })
    }

    /// Empties the add row once its criterion was sent (its priority back
    /// at must once the row is built again).
    pub(super) fn clear_addition(&mut self, tree: &mut UiTree) {
        self.add_draft = AddDraft::default();
        if let Some(add) = &self.add {
            tree.update::<TextField, _>(add.statement, |f, cx| f.set_text(cx, ""));
            tree.update::<TextArea, _>(add.how, |a, cx| a.set_text(cx, ""));
        }
    }
}

/// L1-2's limits on what the person writes: a statement of 1–300
/// characters, and a way to check it.
fn valid_statement(statement: &str) -> bool {
    (1..=MAX_STATEMENT).contains(&statement.chars().count()) && !statement.contains('\n')
}

fn check(statement: &str, how: &str) -> Result<(), String> {
    let n = statement.chars().count();
    if n == 0 {
        return Err("基準の文を入力してください".into());
    }
    if n > MAX_STATEMENT {
        return Err(format!("基準の文は{MAX_STATEMENT}文字までです（{n}文字）"));
    }
    if statement.contains('\n') {
        return Err("基準の文は1行で入力してください".into());
    }
    if how.is_empty() {
        return Err("確かめ方を入力してください".into());
    }
    Ok(())
}

fn priority_menu(tree: &mut UiTree, parent: WidgetId, priority: Priority) -> WidgetId {
    tree.insert(
        Some(parent),
        PopUpButton::new(Priority::ALL.iter().map(|p| p.label()))
            .selected_index(
                Priority::ALL
                    .iter()
                    .position(|p| *p == priority)
                    .unwrap_or(0),
            )
            .control_size(ControlSize::Small),
    )
}

/// The criteria list: editable with `editable`, else as read (a stored
/// verification, or one running in another window).
pub(super) fn insert(
    tree: &mut UiTree,
    host: WidgetId,
    session: &Session,
    editor: &mut Editor,
    editable: bool,
    actions: &mut Vec<(WidgetId, Action)>,
) {
    let list = scroll_column(tree, host, 10.0);
    keep_scroll(tree, list, session, "criteria");
    tree.insert(
        Some(list),
        Label::new("受け入れ基準の確認").font_size(13.0).weight(600),
    );
    note(
        tree,
        list,
        "判定役の基準を確かめ、必要なら直してから検証を始めます。未保存の変更と追加欄の基準も、開始時にまとめて保存します。変更はレポートの「基準の変更」に記録され、検証が始まると基準は固定されます。",
    );
    if editable {
        editor.pending_label = Some(tree.insert(Some(list), Label::new("").font_size(12.0).wrap(true)));
    }
    if !session.criteria_summary.is_empty() {
        let c = card(tree, list, true);
        caption(tree, c, "判定役のまとめ");
        super::body(tree, c, session.criteria_summary.clone());
    }
    let small = |label: &str| Button::new(label).control_size(ControlSize::Small);
    let count = session.criteria.len();
    for (i, c) in session.criteria.iter().enumerate() {
        // A draft the log has caught up with is no longer one.
        if editor
            .drafts
            .get(&c.id)
            .is_some_and(|(s, h)| s.trim() == c.statement && h.trim() == c.how)
        {
            editor.drafts.remove(&c.id);
        }
        let box_ = card(tree, list, false);
        let top = tree.insert(
            Some(box_),
            Container::wrapping_row(6.0, 4.0).align_items(AlignItems::CENTER),
        );
        tree.insert(
            Some(top),
            Label::new(c.id.clone()).font_size(12.5).weight(700),
        );
        tag(
            tree,
            top,
            format!("出典: {}", source_label(c.source)),
            match c.source {
                CriterionSource::User => Tone::Accent,
                _ => Tone::Neutral,
            },
        );
        if c.edited {
            tag(tree, top, "変更済", Tone::Warning);
        }
        if !editable {
            tag(tree, top, c.priority.label(), Tone::Neutral);
            super::body(tree, box_, c.statement.clone());
            caption(tree, box_, format!("確かめ方: {}", c.how));
            continue;
        }
        let priority = priority_menu(tree, top, c.priority);
        tree.set_accessible_label(priority, format!("{} の優先度", c.id));
        spacer(tree, top);
        let up = tree.insert(Some(top), small("↑"));
        tree.set_accessible_label(up, format!("{} を上へ", c.id));
        tree.set_enabled(up, i > 0);
        actions.push((up, Action::MoveCriterion(c.id.clone(), i.saturating_sub(1))));
        let down = tree.insert(Some(top), small("↓"));
        tree.set_accessible_label(down, format!("{} を下へ", c.id));
        tree.set_enabled(down, i + 1 < count);
        actions.push((
            down,
            Action::MoveCriterion(c.id.clone(), (i + 1).min(count - 1)),
        ));
        // As the engine takes a removal: its source, and one criterion
        // always left.
        let (can_remove, why) = if !removable(c) {
            (
                false,
                "あなたの基準と仕様の基準は削除できません（文言は直せます）",
            )
        } else if count <= 1 {
            (false, "基準は1件以上必要です（最後の基準は削除できません）")
        } else {
            (true, "この基準を外します")
        };
        let remove = tree.insert(Some(top), small("削除").tooltip(why));
        tree.set_accessible_label(remove, format!("{} を削除", c.id));
        tree.set_enabled(remove, can_remove);
        actions.push((remove, Action::RemoveCriterion(c.id.clone())));
        let (statement_text, how_text) = editor
            .drafts
            .get(&c.id)
            .cloned()
            .unwrap_or_else(|| (c.statement.clone(), c.how.clone()));
        caption(tree, box_, "基準");
        let statement = tree.insert(Some(box_), TextField::new("基準の文（1行）", 420.0));
        tree.update::<TextField, _>(statement, |f, cx| f.set_text(cx, statement_text));
        tree.set_accessible_label(statement, format!("{} の文", c.id));
        caption(tree, box_, "確かめ方");
        let how = tree.insert(
            Some(box_),
            TextArea::new("どう動かして確かめるか").min_lines(2),
        );
        tree.update::<TextArea, _>(how, |a, cx| a.set_text(cx, how_text));
        tree.set_accessible_label(how, format!("{} の確かめ方", c.id));
        let foot = tree.insert(Some(box_), Container::row().gap(6.0));
        let save = tree.insert(Some(foot), small("変更を保存"));
        tree.set_accessible_label(save, format!("{} の変更を保存", c.id));
        actions.push((save, Action::SaveCriterion(c.id.clone())));
        editor.rows.push(Row {
            id: c.id.clone(),
            statement,
            how,
            priority,
            built: (c.statement.clone(), c.how.clone()),
        });
    }
    if !session.removed_criteria.is_empty() {
        let ids: Vec<&str> = session
            .removed_criteria
            .iter()
            .map(|c| c.id.as_str())
            .collect();
        caption(tree, list, format!("削除した基準: {}", ids.join(", ")));
    }
    if !editable {
        return;
    }
    let add = card(tree, list, false);
    caption(tree, add, "基準を追加（出典: あなた）");
    let statement = tree.insert(Some(add), TextField::new("基準の文（1行）", 420.0));
    let AddDraft {
        statement: draft_statement,
        how: draft_how,
        priority: draft_priority,
    } = editor.add_draft.clone();
    tree.update::<TextField, _>(statement, |f, cx| f.set_text(cx, draft_statement));
    tree.set_accessible_label(statement, "追加する基準の文");
    let how = tree.insert(
        Some(add),
        TextArea::new("どう動かして確かめるか").min_lines(2),
    );
    tree.update::<TextArea, _>(how, |a, cx| a.set_text(cx, draft_how));
    tree.set_accessible_label(how, "追加する基準の確かめ方");
    let foot = tree.insert(
        Some(add),
        Container::row().align_items(AlignItems::CENTER).gap(6.0),
    );
    let priority = priority_menu(tree, foot, draft_priority);
    tree.set_accessible_label(priority, "追加する基準の優先度");
    let button = tree.insert(Some(foot), small("追加"));
    tree.set_enabled(button, count < MAX_CRITERIA);
    actions.push((button, Action::AddCriterion));
    editor.add = Some(AddRow {
        statement,
        how,
        priority,
    });
    let go = tree.insert(
        Some(list),
        Button::new("変更を保存して検証を開始").default_button(true),
    );
    actions.push((go, Action::StartVerify));
    editor.update_pending(tree, session);
}
