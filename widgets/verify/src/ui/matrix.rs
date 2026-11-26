//! The 基準 tab after round 1 starts: the criteria × verifier matrix. One
//! row per criterion (id, priority, statement, ruling), one column per
//! verifier, each cell that verifier's latest result (✓ ✗ ⊘ –) with a
//! re-run mark and a cross-check outcome. A criterion selects its thread;
//! a result opens the evidence that result cites without losing filters.

use noa_gpui_core::style::{
    AlignItems, FlexDirection, JustifyContent, Rect as LayoutRect, Size as LayoutSize, auto,
    length, percent,
};
use noa_gpui_core::widgets::{Button, Container, ControlSize, Label, Pressable, Tone, Truncation};
use noa_gpui_core::{Role, Style, UiTree, WidgetId};

use verify::model::rules::History;
use verify::model::types::{
    Criterion, CrossOutcome, Outcome, PartId, Priority, Rerun, RulingKind, Session, Subject,
};

use super::{Action, caption, keep_scroll, note, ruling_tone, scroll_column, tag};

/// A verifier column's width when there is room; the columns narrow no
/// further than [`CELL_MIN`].
const CELL: f32 = 76.0;
const CELL_MIN: f32 = 52.0;
const RULING: f32 = 76.0;

/// A question a person can ask of the latest results and rulings.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Filter {
    #[default]
    All,
    MustPending,
    Failed,
    Differed,
    Blocked,
    Untested,
}

impl Filter {
    const ALL: [Self; 6] = [
        Self::All,
        Self::MustPending,
        Self::Failed,
        Self::Differed,
        Self::Blocked,
        Self::Untested,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::All => "すべて",
            Self::MustPending => "必須・未解決",
            Self::Failed => "不合格",
            Self::Differed => "再実行の相違",
            Self::Blocked => "実行不能",
            Self::Untested => "未実施",
        }
    }

    pub(super) fn passes(self, session: &Session, criterion: &Criterion) -> bool {
        let ruling = session.ruling(&criterion.id).map(|r| r.kind);
        match self {
            Self::All => true,
            Self::MustPending => {
                criterion.priority == Priority::Must
                    && !matches!(
                        ruling,
                        Some(RulingKind::Verified | RulingKind::NotApplicable)
                    )
            }
            Self::Failed => ruling == Some(RulingKind::Failed),
            Self::Differed => session
                .verifiers
                .iter()
                .filter_map(|part| session.latest_result(&part.id, &criterion.id))
                .flat_map(|result| &result.evidence)
                .any(|key| {
                    session
                        .evidence_item(key)
                        .is_some_and(|item| matches!(item.rerun, Some(Rerun::Differed { .. })))
                }),
            Self::Blocked => session.verifiers.iter().any(|part| {
                session
                    .latest_result(&part.id, &criterion.id)
                    .is_some_and(|r| r.outcome == Outcome::Blocked)
            }),
            Self::Untested => session.verifiers.iter().any(|part| {
                session
                    .latest_result(&part.id, &criterion.id)
                    .is_none_or(|r| r.outcome == Outcome::NotTested)
            }),
        }
    }
}

/// The outcomes reported about the result shown, not about an older one.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct Crossed {
    reproduced: bool,
    not_reproduced: bool,
    disputed: bool,
}

impl Crossed {
    fn mark(self) -> Option<&'static str> {
        match (self.reproduced, self.not_reproduced, self.disputed) {
            (false, false, false) => None,
            (true, false, false) => Some("⇄✓"),
            (false, true, false) => Some("⇄✗"),
            (false, false, true) => Some("⇄?"),
            _ => Some("⇄±"),
        }
    }

    fn label(self) -> &'static str {
        match self.mark() {
            None => "追試なし",
            Some("⇄✓") => "追試: 再現",
            Some("⇄✗") => "追試: 再現せず",
            Some("⇄?") => "追試: 異議・未解決",
            _ => "追試: 結果が分かれています",
        }
    }

    fn needs_attention(self) -> bool {
        self.not_reproduced || self.disputed
    }
}

/// What one cell shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Cell {
    /// The latest result; `None` when the verifier reported none.
    pub outcome: Option<Outcome>,
    /// A re-run problem takes precedence over a match; ↻! is a matched
    /// exit code whose log was not stored, ↻ a match with its log stored.
    pub rerun: Option<&'static str>,
    /// Another verifier cross-checked the result shown: a check is about
    /// the result its verifier had as of the round before the check (L1-6
    /// rule 1), so one on an earlier result marks nothing here.
    pub crossed: Crossed,
}

impl Cell {
    /// Compact marks; the accessible name also spells their meaning out.
    pub(super) fn text(&self) -> String {
        let mut parts: Vec<&str> = vec![self.outcome.map_or("·", Outcome::mark)];
        parts.extend(self.rerun);
        parts.extend(self.crossed.mark());
        parts.join(" ")
    }

    pub(super) fn tone(&self) -> Tone {
        if self.outcome == Some(Outcome::Fail) {
            Tone::Critical
        } else if matches!(self.rerun, Some("≠" | "⏱" | "↻–" | "↻!"))
            || self.crossed.needs_attention()
        {
            Tone::Warning
        } else {
            outcome_tone(self.outcome)
        }
    }

    fn description(&self) -> String {
        let rerun = match self.rerun {
            Some("≠") => "再実行の終了コードが相違（出力内容は未比較）",
            Some("↻") => "再実行の終了コードが一致・ログ保存済み（出力内容は未比較）",
            Some("↻!") => "再実行の終了コードが一致・ログ未保存（出力内容は未比較）",
            Some("⏱") => "再実行が時間切れ",
            Some("↻–") => "再実行されていません",
            _ => "再実行の結果なし",
        };
        format!(
            "{} · {rerun} · {}",
            self.outcome.map_or("結果なし", Outcome::label),
            self.crossed.label()
        )
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
    } else if reruns.iter().any(|r| matches!(r, Rerun::TimedOut { .. })) {
        Some("⏱")
    } else if reruns.iter().any(|r| matches!(r, Rerun::NotRun { .. })) {
        Some("↻–")
    } else if reruns
        .iter()
        .any(|r| matches!(r, Rerun::Matched { log: None, .. }))
    {
        Some("↻!")
    } else if reruns.iter().any(|r| matches!(r, Rerun::Matched { .. })) {
        Some("↻")
    } else {
        None
    };
    let mut crossed = Crossed::default();
    if let Some(latest) = latest {
        for c in History::of(session, criterion).checks {
            if matches!(&c.check.subject, Subject::Result { part: p, .. } if p == part)
                && c.about.is_some_and(|about| std::ptr::eq(about, latest))
            {
                match c.check.outcome {
                    CrossOutcome::Reproduced => crossed.reproduced = true,
                    CrossOutcome::NotReproduced => crossed.not_reproduced = true,
                    CrossOutcome::Disputed => crossed.disputed = true,
                }
            }
        }
    }
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
    filter: Filter,
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
    let filters = tree.insert(Some(list), Container::wrapping_row(6.0, 4.0));
    for candidate in Filter::ALL {
        let count = session
            .criteria
            .iter()
            .filter(|c| candidate.passes(session, c))
            .count();
        let button = tree.insert(
            Some(filters),
            Button::new(format!("{} {count}", candidate.label()))
                .control_size(ControlSize::Small)
                .selected(filter == candidate),
        );
        actions.push((button, Action::FilterCriteria(candidate)));
    }
    let shown = session
        .criteria
        .iter()
        .filter(|c| filter.passes(session, c))
        .count();
    caption(
        tree,
        list,
        format!("表示 {shown} / 全 {} 基準", session.criteria.len()),
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
    if shown == 0 && !session.criteria.is_empty() {
        note(
            tree,
            list,
            "条件に合う基準はありません。「すべて」で全基準に戻れます",
        );
    }
    for criterion in session
        .criteria
        .iter()
        .filter(|c| filter.passes(session, c))
    {
        let is_selected = selected == Some(criterion.id.as_str());
        let press = tree.insert(
            Some(grid),
            Container::new(Style {
                size: LayoutSize {
                    width: percent(1.0),
                    height: auto(),
                },
                ..Default::default()
            })
            .radius(6.0),
        );
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
        let name = tree.insert(
            Some(name),
            Pressable::new(Style {
                flex_direction: FlexDirection::Column,
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
            name,
            Action::SelectCriterion((!is_selected).then(|| criterion.id.clone())),
        ));
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
            if cell.outcome.is_none() {
                tree.insert(Some(at), Label::new("·").font_size(12.0).color(Role::Muted));
            } else {
                let press = tree.insert(
                    Some(at),
                    Pressable::new(Style::default())
                        .label(format!(
                            "{} {}: {}。引用した証跡を表示",
                            criterion.id,
                            part.id,
                            cell.description()
                        ))
                        .radius(4.0),
                );
                tag(tree, press, cell.text(), cell.tone());
                actions.push((
                    press,
                    Action::ShowResultEvidence(part.id.clone(), criterion.id.clone()),
                ));
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
        "✓ 成功 · ✗ 失敗 · ⊘ 実行不能 · – 未実施 ／ ↻ 終了コード一致・ログ保存済み · ↻! 終了コード一致・ログ未保存 · ≠ 終了コード相違（出力内容はいずれも未比較） · ⏱ 時間切れ · ↻– 未実行 ／ ⇄✓ 追試で再現 · ⇄✗ 再現せず · ⇄? 異議 · ⇄± 結果が混在。基準を選ぶと詳細、結果を選ぶと引用した証跡を表示します",
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
        assert_eq!(at("claude-2", "AC-1"), "✓ ≠ ⇄✗");
        // codex's fail matched and claude-2 reproduced it.
        assert_eq!(at("codex", "AC-1"), "✗ ↻ ⇄✓");
        // claude's AC-5: the round-2 update, not the round-1 not_tested.
        assert_eq!(at("claude", "AC-5"), "✓ ↻");
        assert_eq!(at("codex", "AC-3"), "⊘");
        assert_eq!(at("claude-2", "AC-3"), "–");
    }

    #[test]
    fn a_reported_pass_with_a_different_or_incomplete_rerun_is_a_warning() {
        let mut session = crate::demo::session();
        let part = PartId::new("claude-2");
        let key = session.latest_result(&part, "AC-1").unwrap().evidence[0].clone();
        session.cross_checks.clear();
        for (rerun, mark) in [
            (
                Rerun::Differed {
                    exit: Some(1),
                    log: None,
                },
                "≠",
            ),
            (Rerun::TimedOut { log: None }, "⏱"),
            (
                Rerun::NotRun {
                    why: "pane gone".into(),
                    log: None,
                },
                "↻–",
            ),
        ] {
            session.evidence_item_mut(&key).unwrap().rerun = Some(rerun);
            let shown = cell(&session, &part, "AC-1");
            assert_eq!(shown.rerun, Some(mark));
            assert!(matches!(shown.tone(), Tone::Warning));
        }
    }

    #[test]
    fn a_matched_exit_without_its_log_warns_and_a_reported_failure_stays_critical() {
        let mut session = crate::demo::session();
        let part = PartId::new("claude-2");
        let key = session.latest_result(&part, "AC-1").unwrap().evidence[0].clone();
        session.cross_checks.clear();
        let item = session.evidence_item_mut(&key).unwrap();
        let stored = item.stored.clone().unwrap();
        item.rerun = Some(Rerun::Matched { exit: 0, log: None });
        let mut shown = cell(&session, &part, "AC-1");
        assert_eq!(shown.rerun, Some("↻!"));
        assert!(
            shown
                .description()
                .contains("ログ未保存（出力内容は未比較）")
        );
        assert!(matches!(shown.tone(), Tone::Warning));
        shown.outcome = Some(Outcome::Fail);
        assert!(matches!(shown.tone(), Tone::Critical));

        session.evidence_item_mut(&key).unwrap().rerun = Some(Rerun::Matched {
            exit: 0,
            log: Some(stored),
        });
        let shown = cell(&session, &part, "AC-1");
        assert_eq!(shown.rerun, Some("↻"));
        assert!(
            shown
                .description()
                .contains("ログ保存済み（出力内容は未比較）")
        );
        assert!(matches!(shown.tone(), Tone::Success));
    }

    #[test]
    fn mixed_cross_checks_do_not_look_like_an_unqualified_reproduction() {
        let mut session = crate::demo::session();
        let part = PartId::new("claude-2");
        let check = session.cross_checks.iter().find(|check| {
            matches!(&check.subject, Subject::Result { part: p, criterion } if p == &part && criterion == "AC-1")
        }).unwrap().clone();
        let mut reproduced = check;
        reproduced.outcome = CrossOutcome::Reproduced;
        session.cross_checks.push(reproduced);
        let shown = cell(&session, &part, "AC-1");
        assert!(shown.text().contains("⇄±"));
        assert!(matches!(shown.tone(), Tone::Warning));
    }

    #[test]
    fn filters_distinguish_unresolved_musts_differed_runs_and_blocked_checks() {
        let session = crate::demo::session();
        let ac1 = session.criterion("AC-1").unwrap();
        let ac2 = session.criterion("AC-2").unwrap();
        let ac3 = session.criterion("AC-3").unwrap();
        assert!(Filter::MustPending.passes(&session, ac1));
        assert!(!Filter::MustPending.passes(&session, ac2));
        assert!(!Filter::MustPending.passes(&session, ac3));
        assert!(Filter::Differed.passes(&session, ac1));
        assert!(Filter::Blocked.passes(&session, ac3));
        assert!(Filter::Untested.passes(&session, ac3));
    }
}
