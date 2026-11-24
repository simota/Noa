//! The 不具合 tab: the judge's consolidated defects as review's findings
//! board shows findings: filters by severity, strategy, status, agreement
//! and verifier, then one row per defect in rank order with its repro
//! steps.

use std::collections::BTreeSet;

use noa_gpui_core::style::{AlignItems, Size as LayoutSize, auto, percent};
use noa_gpui_core::widgets::{Button, Container, ControlSize, Label, Pressable, Tone, Truncation};
use noa_gpui_core::{Role, Style, UiTree, WidgetId};

use verify::model::types::{Agreement, Defect, DefectStatus, PartId, Session, Severity};

use super::{Action, caption, keep_scroll, note, scroll_column, severity_tone, status_tone, tag};

/// What the board shows. An empty set means every value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Filters {
    pub severities: BTreeSet<Severity>,
    pub categories: BTreeSet<String>,
    pub statuses: BTreeSet<usize>,
    pub agreements: BTreeSet<usize>,
    /// Only defects this verifier raised or cross-checked.
    pub reporter: Option<PartId>,
}

/// [`DefectStatus`] in a set (by its order in `DefectStatus::ALL`).
pub fn status_key(status: DefectStatus) -> usize {
    DefectStatus::ALL
        .iter()
        .position(|s| *s == status)
        .unwrap_or(0)
}

/// [`Agreement`] in a set (by its order in `Agreement::ALL`).
pub fn agreement_key(agreement: Agreement) -> usize {
    Agreement::ALL
        .iter()
        .position(|a| *a == agreement)
        .unwrap_or(0)
}

impl Default for Filters {
    /// The live defects: open, agreed and disputed.
    fn default() -> Self {
        Self {
            severities: BTreeSet::new(),
            categories: BTreeSet::new(),
            statuses: [
                DefectStatus::Open,
                DefectStatus::Agreed,
                DefectStatus::Disputed,
            ]
            .into_iter()
            .map(status_key)
            .collect(),
            agreements: BTreeSet::new(),
            reporter: None,
        }
    }
}

/// Whether `id` names `defect` or one merged into it (directly or through
/// another merged one): every subject a cross-check counted with it names.
pub(super) fn names(session: &Session, defect: &Defect, id: &str) -> bool {
    let mut at = session.defect(id);
    let mut hops = 0;
    while let Some(d) = at {
        if d.id == defect.id {
            return true;
        }
        hops += 1;
        if hops > session.defects.len() {
            return false;
        }
        at = d
            .merged_into
            .as_deref()
            .and_then(|into| session.defect(into));
    }
    false
}

/// Whether `filters` let `d` through.
pub(super) fn passes(session: &Session, filters: &Filters, d: &Defect) -> bool {
    (filters.severities.is_empty() || filters.severities.contains(&d.severity))
        && (filters.categories.is_empty() || filters.categories.contains(&d.category))
        && (filters.statuses.is_empty() || filters.statuses.contains(&status_key(d.status)))
        && (filters.agreements.is_empty()
            || filters
                .agreements
                .contains(&agreement_key(session.tally(d).agreement)))
        && filters.reporter.as_ref().is_none_or(|part| {
            d.origins().contains(part)
                || session.cross_checks.iter().any(|c| {
                    &c.by == part
                        && matches!(&c.subject, verify::model::types::Subject::Defect { id } if names(session, d, id))
                })
        })
}

/// The defects `filters` let through, in rank order.
pub(super) fn shown<'a>(session: &'a Session, filters: &Filters) -> Vec<&'a Defect> {
    session
        .ranked()
        .into_iter()
        .filter(|d| passes(session, filters, d))
        .collect()
}

/// How many defects `filters` let through.
pub(super) fn count(session: &Session, filters: &Filters) -> usize {
    session
        .defects
        .iter()
        .filter(|d| passes(session, filters, d))
        .count()
}

fn chip(
    tree: &mut UiTree,
    parent: WidgetId,
    label: String,
    on: bool,
    action: Action,
    actions: &mut Vec<(WidgetId, Action)>,
) {
    let id = tree.insert(
        Some(parent),
        Button::new(label)
            .control_size(ControlSize::Small)
            .selected(on),
    );
    actions.push((id, action));
}

pub(super) fn filter_row(tree: &mut UiTree, parent: WidgetId, title: &str) -> WidgetId {
    let row = tree.insert(
        Some(parent),
        Container::wrapping_row(4.0, 4.0).align_items(AlignItems::CENTER),
    );
    tree.insert(
        Some(row),
        Label::new(title.to_owned())
            .font_size(11.0)
            .color(Role::Muted),
    );
    row
}

/// How many defects the filters let through, the verifier filter when one
/// is on, and 条件をすべて解除: in the tab row.
pub(super) fn summary(
    tree: &mut UiTree,
    row: WidgetId,
    session: &Session,
    filters: &Filters,
    actions: &mut Vec<(WidgetId, Action)>,
) {
    caption(
        tree,
        row,
        format!(
            "表示 {} / 全 {}",
            count(session, filters),
            session.defects.len()
        ),
    );
    if let Some(part) = &filters.reporter {
        let chip = tree.insert(
            Some(row),
            Button::new(format!("検証者: {part} ✕"))
                .control_size(ControlSize::Small)
                .selected(true),
        );
        actions.push((chip, Action::FilterReporter(None)));
    }
    chip(
        tree,
        row,
        "条件をすべて解除".into(),
        false,
        Action::ClearDefectFilters,
        actions,
    );
}

pub(super) fn insert(
    tree: &mut UiTree,
    host: WidgetId,
    session: &Session,
    filters: &Filters,
    selected: Option<&str>,
    actions: &mut Vec<(WidgetId, Action)>,
) {
    let list = scroll_column(tree, host, 8.0);
    keep_scroll(tree, list, session, "defects");
    let bar = tree.insert(
        Some(list),
        Container::column()
            .gap(4.0)
            .padding(8.0)
            .background(Role::SurfaceAlt)
            .radius(8.0),
    );
    // A chip counts what choosing its value alone in its group shows, the
    // other groups' filters as they are: count and filter always agree.
    let only = |change: &dyn Fn(&mut Filters)| {
        let mut alone = filters.clone();
        change(&mut alone);
        count(session, &alone)
    };
    let row = filter_row(tree, bar, "重大度");
    for s in Severity::ALL {
        let n = only(&|f| f.severities = [s].into());
        chip(
            tree,
            row,
            format!("{} {n}", s.label()),
            filters.severities.contains(&s),
            Action::ToggleSeverity(s),
            actions,
        );
    }
    let row = filter_row(tree, bar, "手法");
    let mut categories: Vec<String> = session
        .verifiers
        .iter()
        .filter_map(|p| p.strategy.as_ref().map(|s| s.id().to_owned()))
        .collect();
    for d in &session.defects {
        categories.push(d.category.clone());
    }
    let mut seen = BTreeSet::new();
    categories.retain(|c| seen.insert(c.clone()));
    for id in categories {
        let on = filters.categories.contains(&id);
        chip(
            tree,
            row,
            session.category_label(&id),
            on,
            Action::ToggleCategory(id),
            actions,
        );
    }
    let row = filter_row(tree, bar, "状態");
    for s in DefectStatus::ALL {
        let n = only(&|f| f.statuses = [status_key(s)].into());
        chip(
            tree,
            row,
            format!("{} {n}", s.label()),
            filters.statuses.contains(&status_key(s)),
            Action::ToggleStatus(s),
            actions,
        );
    }
    let row = filter_row(tree, bar, "合意度");
    for a in Agreement::ALL {
        let n = only(&|f| f.agreements = [agreement_key(a)].into());
        chip(
            tree,
            row,
            format!("{} {n}", a.label()),
            filters.agreements.contains(&agreement_key(a)),
            Action::ToggleAgreement(a),
            actions,
        );
    }
    let row = filter_row(tree, bar, "検証者");
    for part in &session.verifiers {
        let on = filters.reporter.as_ref() == Some(&part.id);
        chip(
            tree,
            row,
            part.id.to_string(),
            on,
            Action::FilterReporter((!on).then(|| part.id.clone())),
            actions,
        );
    }

    let shown = shown(session, filters);
    if session.defects.is_empty() {
        note(
            tree,
            list,
            match session.current_round() {
                Some(r) if r.no == 1 && r.outcome.is_none() => {
                    "独立検証中です。判定役が取りまとめると不具合がここに並びます。"
                }
                None => "検証が始まると、検証者が見つけた不具合がここに並びます。",
                _ => "不具合はありません。",
            },
        );
        unplaced(tree, list, session);
        return;
    }
    if shown.is_empty() {
        note(tree, list, "条件に合う不具合はありません。");
    }
    for d in shown {
        let is_selected = selected == Some(d.id.as_str());
        let row = tree.insert(
            Some(list),
            Pressable::new(Style {
                size: LayoutSize {
                    width: percent(1.0),
                    height: auto(),
                },
                ..Default::default()
            })
            .label(format!("{} {}", d.id, d.claim))
            .selected(is_selected)
            .radius(8.0),
        );
        actions.push((
            row,
            Action::SelectDefect((!is_selected).then(|| d.id.clone())),
        ));
        let c = tree.insert(
            Some(row),
            Container::column()
                .gap(4.0)
                .padding(10.0)
                .background(if is_selected {
                    Role::SurfaceAlt
                } else {
                    Role::Surface
                })
                .border(
                    1.0,
                    if is_selected {
                        Role::Accent
                    } else {
                        Role::Separator
                    },
                )
                .radius(8.0)
                .grow(1.0),
        );
        let top = tree.insert(
            Some(c),
            Container::wrapping_row(6.0, 4.0).align_items(AlignItems::CENTER),
        );
        tree.insert(
            Some(top),
            Label::new(d.id.clone()).font_size(12.5).weight(700),
        );
        tag(tree, top, d.severity.label(), severity_tone(d.severity));
        tag(
            tree,
            top,
            session.category_label(&d.category),
            Tone::Neutral,
        );
        tag(tree, top, d.status.label(), status_tone(d.status));
        let tally = session.tally(d);
        caption(
            tree,
            top,
            format!("{} · {}", tally.label(), tally.agreement.label()),
        );
        tree.insert(
            Some(c),
            Label::new(d.claim.clone())
                .font_size(12.5)
                .wrap(true)
                .max_lines(2)
                .truncation(Truncation::Tail),
        );
        tree.insert(
            Some(c),
            Label::new(format!("再現手順: {}", d.repro))
                .font_size(11.5)
                .color(Role::Muted)
                .wrap(true)
                .max_lines(2)
                .truncation(Truncation::Tail),
        );
        let bottom = tree.insert(
            Some(c),
            Container::wrapping_row(6.0, 4.0).align_items(AlignItems::CENTER),
        );
        match &d.location {
            Some(loc) => caption(tree, bottom, loc.to_string()),
            None => caption(tree, bottom, "対象全体"),
        }
        if !d.in_target {
            tag(tree, bottom, "対象外", Tone::Warning);
        } else if d.in_diff == Some(false) {
            tag(tree, bottom, "差分外", Tone::Neutral);
        }
        let origins: Vec<String> = d.origins().iter().map(|p| p.to_string()).collect();
        caption(tree, bottom, format!("提起: {}", origins.join(", ")));
    }
    unplaced(tree, list, session);
}

/// An ended verification's raw defects the judge never placed, listed under
/// the board (they are in no defect, so no filter shows them).
fn unplaced(tree: &mut UiTree, list: WidgetId, session: &Session) {
    let raws = session.unplaced();
    if !session.closed || raws.is_empty() {
        return;
    }
    note(
        tree,
        list,
        format!(
            "取りまとめられずに終わった不具合 {} 件（判定役の取りまとめ前に終了しました）",
            raws.len()
        ),
    );
    for raw in raws {
        let row = tree.insert(
            Some(list),
            Container::column()
                .gap(4.0)
                .padding(10.0)
                .background(Role::Surface)
                .border(1.0, Role::Separator)
                .radius(8.0),
        );
        let top = tree.insert(
            Some(row),
            Container::wrapping_row(6.0, 4.0).align_items(AlignItems::CENTER),
        );
        tag(tree, top, raw.key.to_string(), Tone::Neutral);
        tag(tree, top, raw.severity.label(), severity_tone(raw.severity));
        caption(tree, top, format!("R{}", raw.round));
        tree.insert(
            Some(row),
            Label::new(raw.claim.clone())
                .font_size(12.5)
                .wrap(true)
                .max_lines(2)
                .truncation(Truncation::Tail),
        );
    }
}
