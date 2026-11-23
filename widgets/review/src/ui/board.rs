//! The findings board: filters by severity, lens, status, agreement and
//! reviewer, then one row per finding in rank order.

use noa_gpui_core::style::{AlignItems, Size as LayoutSize, auto, percent};
use noa_gpui_core::widgets::{Button, Container, ControlSize, Label, Pressable, Tone, Truncation};
use noa_gpui_core::{Role, Style, UiTree, WidgetId};

use review::model::types::{Agreement, Finding, FindingStatus, Session, Severity};

use super::{
    Action, Filters, agreement_key, caption, keep_scroll, note, scroll_column, severity_tone,
    spacer, status_key, status_tone, tag,
};

/// The findings `filters` let through, in rank order.
pub(crate) fn shown<'a>(session: &'a Session, filters: &Filters) -> Vec<&'a Finding> {
    session
        .ranked()
        .into_iter()
        .filter(|f| filters.severities.is_empty() || filters.severities.contains(&f.severity))
        .filter(|f| filters.lenses.is_empty() || filters.lenses.contains(&f.category))
        .filter(|f| filters.statuses.is_empty() || filters.statuses.contains(&status_key(f.status)))
        .filter(|f| {
            filters.agreements.is_empty()
                || filters
                    .agreements
                    .contains(&agreement_key(session.tally(f).agreement))
        })
        .filter(|f| {
            filters
                .reviewer
                .as_ref()
                .is_none_or(|part| f.origins().contains(part) || f.stances.contains_key(part))
        })
        .collect()
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
            .default_button(on),
    );
    actions.push((id, action));
}

fn filter_row(tree: &mut UiTree, parent: WidgetId, title: &str) -> WidgetId {
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

pub(super) fn insert(
    tree: &mut UiTree,
    host: WidgetId,
    session: &Session,
    filters: &Filters,
    selected: Option<&str>,
    actions: &mut Vec<(WidgetId, Action)>,
) {
    let bar = tree.insert(
        Some(host),
        Container::column()
            .gap(4.0)
            .padding(10.0)
            .background(Role::SurfaceAlt),
    );
    let count =
        |pred: &dyn Fn(&Finding) -> bool| session.findings.iter().filter(|f| pred(f)).count();

    let row = filter_row(tree, bar, "重大度");
    for s in Severity::ALL {
        let n = count(&|f| f.severity == s && f.status.is_live());
        chip(
            tree,
            row,
            format!("{} {n}", s.label()),
            filters.severities.contains(&s),
            Action::ToggleSeverity(s),
            actions,
        );
    }
    let row = filter_row(tree, bar, "観点");
    let mut lenses: Vec<String> = session
        .panel_lenses()
        .iter()
        .map(|l| l.id().to_owned())
        .collect();
    for f in &session.findings {
        if !lenses.contains(&f.category) {
            lenses.push(f.category.clone());
        }
    }
    for id in lenses {
        let on = filters.lenses.contains(&id);
        chip(
            tree,
            row,
            session.category_label(&id),
            on,
            Action::ToggleLens(id),
            actions,
        );
    }
    let row = filter_row(tree, bar, "状態");
    for s in FindingStatus::ALL {
        let n = count(&|f| f.status == s);
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
        chip(
            tree,
            row,
            a.label().to_owned(),
            filters.agreements.contains(&agreement_key(a)),
            Action::ToggleAgreement(a),
            actions,
        );
    }

    let shown = shown(session, filters);
    let foot = tree.insert(
        Some(bar),
        Container::row().align_items(AlignItems::CENTER).gap(6.0),
    );
    caption(
        tree,
        foot,
        format!("表示 {} / 全 {}", shown.len(), session.findings.len()),
    );
    if let Some(part) = &filters.reviewer {
        tag(tree, foot, format!("レビュアー: {part}"), Tone::Accent);
    }
    spacer(tree, foot);
    chip(
        tree,
        foot,
        "条件をすべて解除".into(),
        false,
        Action::ClearFilters,
        actions,
    );

    let list = scroll_column(tree, host, 8.0);
    keep_scroll(tree, list, session, "board");
    if session.findings.is_empty() {
        note(
            tree,
            list,
            match session.current_round() {
                Some(r) if r.no == 1 && r.outcome.is_none() => {
                    "独立レビュー中です。リードが取りまとめると指摘がここに並びます。"
                }
                _ => "指摘はありません。",
            },
        );
        return;
    }
    if shown.is_empty() {
        note(tree, list, "条件に合う指摘はありません。");
    }
    for f in shown {
        let is_selected = selected == Some(f.id.as_str());
        let row = tree.insert(
            Some(list),
            Pressable::new(Style {
                size: LayoutSize {
                    width: percent(1.0),
                    height: auto(),
                },
                ..Default::default()
            })
            .label(format!("{} {}", f.id, f.claim))
            .selected(is_selected)
            .radius(8.0),
        );
        actions.push((
            row,
            Action::SelectFinding((!is_selected).then(|| f.id.clone())),
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
            Label::new(f.id.clone()).font_size(12.5).weight(700),
        );
        tag(tree, top, f.severity.label(), severity_tone(f.severity));
        tag(
            tree,
            top,
            session.category_label(&f.category),
            Tone::Neutral,
        );
        tag(tree, top, f.status.label(), status_tone(f.status));
        let tally = session.tally(f);
        caption(
            tree,
            top,
            format!("{} · {}", tally.label(), tally.agreement.label()),
        );
        tree.insert(
            Some(c),
            Label::new(f.claim.clone())
                .font_size(12.5)
                .wrap(true)
                .max_lines(2)
                .truncation(Truncation::Tail),
        );
        let bottom = tree.insert(
            Some(c),
            Container::wrapping_row(6.0, 4.0).align_items(AlignItems::CENTER),
        );
        match &f.location {
            Some(loc) => caption(tree, bottom, loc.to_string()),
            None => caption(tree, bottom, "対象全体"),
        }
        if !f.in_target {
            tag(tree, bottom, "対象外", Tone::Warning);
        } else if f.in_diff == Some(false) {
            tag(tree, bottom, "差分外", Tone::Neutral);
        }
        let origins: Vec<String> = f.origins().iter().map(|p| p.to_string()).collect();
        caption(tree, bottom, format!("提起: {}", origins.join(", ")));
    }
}
