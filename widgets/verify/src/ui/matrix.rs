//! The 基準 tab after round 1 starts: the criteria × verifier matrix. One
//! row per criterion (id, priority, statement, ruling), one column per
//! verifier, each cell that verifier's latest result (✓ ✗ ⊘ –) with a
//! re-run mark (↻ matched, ≠ differed) and a cross-check mark (⇄); a row
//! selects its criterion.

use noa_gpui_core::style::{
    AlignItems, FlexDirection, JustifyContent, Rect as LayoutRect, Size as LayoutSize, auto,
    length, percent,
};
use noa_gpui_core::widgets::{Container, Label, Pressable, Tone, Truncation};
use noa_gpui_core::{Role, Style, UiTree, WidgetId};

use verify::model::rules::History;
use verify::model::types::{Outcome, PartId, Rerun, Session, Subject};

use super::{Action, caption, keep_scroll, note, ruling_tone, scroll_column, tag};

/// A verifier column's width when there is room; the columns narrow no
/// further than [`CELL_MIN`].
const CELL: f32 = 76.0;
const CELL_MIN: f32 = 52.0;
const RULING: f32 = 76.0;

/// What one cell shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Cell {
    /// The latest result; `None` when the verifier reported none.
    pub outcome: Option<Outcome>,
    /// ≠ when a re-run of a command that result cites differed, else ↻
    /// when one matched.
    pub rerun: Option<&'static str>,
    /// Another verifier cross-checked the result shown: a check is about
    /// the result its verifier had as of the round before the check (L1-6
    /// rule 1), so one on an earlier result marks nothing here.
    pub crossed: bool,
}

impl Cell {
    /// `✗ ↻ ⇄`, as the cell's tag reads.
    pub(super) fn text(&self) -> String {
        let mut parts: Vec<&str> = vec![self.outcome.map_or("·", Outcome::mark)];
        parts.extend(self.rerun);
        if self.crossed {
            parts.push("⇄");
        }
        parts.join(" ")
    }
}

/// `part`'s cell for `criterion`.
pub(super) fn cell(session: &Session, part: &PartId, criterion: &str) -> Cell {
    let latest = session.latest_result(part, criterion);
    let reruns: Vec<&Rerun> = latest
        .into_iter()
        .flat_map(|r| &r.evidence)
        .filter_map(|key| session.evidence_item(key)?.rerun.as_ref())
        .collect();
    let rerun = if reruns.iter().any(|r| matches!(r, Rerun::Differed { .. })) {
        Some("≠")
    } else if reruns.iter().any(|r| matches!(r, Rerun::Matched { .. })) {
        Some("↻")
    } else {
        None
    };
    let crossed = latest.is_some_and(|latest| {
        History::of(session, criterion).checks.iter().any(|c| {
            matches!(&c.check.subject, Subject::Result { part: p, .. } if p == part)
                && c.about.is_some_and(|about| std::ptr::eq(about, latest))
        })
    });
    Cell {
        outcome: latest.map(|r| r.outcome),
        rerun,
        crossed,
    }
}

fn outcome_tone(outcome: Option<Outcome>) -> Tone {
    match outcome {
        Some(Outcome::Pass) => Tone::Success,
        Some(Outcome::Fail) => Tone::Critical,
        Some(Outcome::Blocked) => Tone::Warning,
        Some(Outcome::NotTested) | None => Tone::Neutral,
    }
}

fn cell_box(tree: &mut UiTree, parent: WidgetId, basis: f32, min: f32) -> WidgetId {
    tree.insert(
        Some(parent),
        Container::new(Style {
            size: LayoutSize {
                width: auto(),
                height: length(28.0),
            },
            min_size: LayoutSize {
                width: length(min),
                height: auto(),
            },
            flex_basis: length(basis),
            flex_grow: 0.0,
            flex_shrink: 1.0,
            align_items: AlignItems::CENTER,
            justify_content: JustifyContent::CENTER,
            ..Default::default()
        }),
    )
}

/// The criterion column: takes what the cells leave.
fn name_box(tree: &mut UiTree, parent: WidgetId) -> WidgetId {
    tree.insert(
        Some(parent),
        Container::new(Style {
            flex_direction: FlexDirection::Column,
            flex_grow: 1.0,
            flex_shrink: 1.0,
            flex_basis: length(0.0),
            min_size: LayoutSize {
                width: length(0.0),
                height: auto(),
            },
            gap: super::gap(2.0),
            padding: LayoutRect {
                left: length(6.0),
                right: length(4.0),
                top: length(4.0),
                bottom: length(4.0),
            },
            ..Default::default()
        }),
    )
}

pub(super) fn insert(
    tree: &mut UiTree,
    host: WidgetId,
    session: &Session,
    selected: Option<&str>,
    actions: &mut Vec<(WidgetId, Action)>,
) {
    let list = scroll_column(tree, host, 10.0);
    keep_scroll(tree, list, session, "matrix");
    tree.insert(
        Some(list),
        Label::new("受け入れ基準 × 検証者（各検証者の最新の結果）")
            .font_size(13.0)
            .weight(600),
    );
    let grid = tree.insert(
        Some(list),
        Container::column()
            .gap(2.0)
            .padding(8.0)
            .background(Role::Surface)
            .border(1.0, Role::Separator)
            .radius(8.0),
    );
    let header = tree.insert(
        Some(grid),
        Container::row().gap(2.0).align_items(AlignItems::CENTER),
    );
    let h = name_box(tree, header);
    caption(tree, h, "基準");
    for part in &session.verifiers {
        let c = cell_box(tree, header, CELL, CELL_MIN);
        tree.insert(
            Some(c),
            Label::new(part.id.to_string())
                .font_size(11.0)
                .color(Role::Muted)
                .truncation(Truncation::Tail),
        );
    }
    let c = cell_box(tree, header, RULING, RULING);
    caption(tree, c, "判定");
    if session.criteria.is_empty() {
        note(tree, list, "受け入れ基準はまだありません");
    }
    for criterion in &session.criteria {
        let is_selected = selected == Some(criterion.id.as_str());
        let press = tree.insert(
            Some(grid),
            Pressable::new(Style {
                size: LayoutSize {
                    width: percent(1.0),
                    height: auto(),
                },
                ..Default::default()
            })
            .label(format!("{} {}", criterion.id, criterion.statement))
            .selected(is_selected)
            .radius(6.0),
        );
        actions.push((
            press,
            Action::SelectCriterion((!is_selected).then(|| criterion.id.clone())),
        ));
        let row = tree.insert(
            Some(press),
            Container::row()
                .gap(2.0)
                .align_items(AlignItems::CENTER)
                .background(if is_selected {
                    Role::SurfaceAlt
                } else {
                    Role::Surface
                })
                .radius(6.0)
                .grow(1.0),
        );
        let name = name_box(tree, row);
        let line = tree.insert(
            Some(name),
            Container::row().gap(4.0).align_items(AlignItems::CENTER),
        );
        tree.insert(
            Some(line),
            Label::new(criterion.id.clone()).font_size(12.0).weight(700),
        );
        tag(
            tree,
            line,
            criterion.priority.label(),
            match criterion.priority {
                verify::model::types::Priority::Must => Tone::Accent,
                verify::model::types::Priority::Should => Tone::Neutral,
            },
        );
        tree.insert(
            Some(name),
            Label::new(criterion.statement.clone())
                .font_size(11.5)
                .truncation(Truncation::Tail),
        );
        for part in &session.verifiers {
            let at = cell_box(tree, row, CELL, CELL_MIN);
            let cell = cell(session, &part.id, &criterion.id);
            if cell.outcome.is_none() && cell.rerun.is_none() && !cell.crossed {
                tree.insert(Some(at), Label::new("·").font_size(12.0).color(Role::Muted));
            } else {
                let tone = outcome_tone(cell.outcome);
                tag(tree, at, cell.text(), tone);
            }
        }
        let at = cell_box(tree, row, RULING, RULING);
        match session.ruling(&criterion.id) {
            Some(r) => tag(tree, at, r.kind.label(), ruling_tone(r.kind)),
            None => caption(tree, at, "未判定"),
        }
    }
    note(
        tree,
        list,
        "✓ 成功 · ✗ 失敗 · ⊘ 実行不能 · – 未実施 ／ ↻ 再実行が一致 · ≠ 再実行が相違 ／ ⇄ 他の検証者が追試。行を選ぶと右に詳細が出ます",
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cell_reads_the_latest_result_its_reruns_and_its_cross_checks() {
        let s = crate::demo::session();
        let at = |part: &str, c: &str| cell(&s, &PartId::new(part), c).text();
        // claude's fail matched on re-run; nobody cross-checked it.
        assert_eq!(at("claude", "AC-1"), "✗ ↻");
        // claude-2's pass differed on re-run, and codex cross-checked it.
        assert_eq!(at("claude-2", "AC-1"), "✓ ≠ ⇄");
        // codex's fail matched and claude-2 reproduced it.
        assert_eq!(at("codex", "AC-1"), "✗ ↻ ⇄");
        // claude's AC-5: the round-2 update, not the round-1 not_tested.
        assert_eq!(at("claude", "AC-5"), "✓ ↻");
        assert_eq!(at("codex", "AC-3"), "⊘");
        assert_eq!(at("claude-2", "AC-3"), "–");
    }
}
