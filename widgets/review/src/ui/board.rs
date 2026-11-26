//! The findings board: filters by severity, lens, status, agreement and
//! reviewer, then one row per finding in rank order.

use noa_gpui_core::style::{AlignItems, Size as LayoutSize, auto, percent};
use noa_gpui_core::widgets::{Button, Container, ControlSize, Label, Pressable, Tone, Truncation};
use noa_gpui_core::{Role, Style, UiTree, WidgetId};

use review::model::types::{Agreement, Finding, FindingStatus, Session, Severity};

use super::{
    Action, Filters, agreement_key, caption, keep_scroll, note, scroll_column, severity_tone,
    status_key, status_tone, tag,
};

/// Whether `filters` let `f` through.
pub(crate) fn passes(session: &Session, filters: &Filters, f: &Finding) -> bool {
    (filters.severities.is_empty() || filters.severities.contains(&f.severity))
        && (filters.lenses.is_empty() || filters.lenses.contains(&f.category))
        && (filters.statuses.is_empty() || filters.statuses.contains(&status_key(f.status)))
        && (filters.agreements.is_empty()
            || filters
                .agreements
                .contains(&agreement_key(session.tally(f).agreement)))
        && filters
            .reviewer
            .as_ref()
            .is_none_or(|part| f.origins().contains(part) || f.stances.contains_key(part))
        && filters
            .path
            .as_ref()
            .is_none_or(|path| touches(session, f, path))
        && holds(session, f, &filters.query)
}

/// `path` as a chip shows it: whole, or its last components after `…/`
/// when long, so the chip and what follows it stay in the pane.
pub(crate) fn short_path(path: &str) -> String {
    const MAX: usize = 32;
    if path.chars().count() <= MAX {
        return path.to_owned();
    }
    let mut tail = String::new();
    for part in path.rsplit('/') {
        let next = if tail.is_empty() {
            part.to_owned()
        } else {
            format!("{part}/{tail}")
        };
        if !tail.is_empty() && next.chars().count() > MAX - 2 {
            break;
        }
        tail = next;
    }
    if tail.chars().count() > MAX - 2 {
        let chars: Vec<char> = tail.chars().collect();
        tail = chars[chars.len() - (MAX - 2)..].iter().collect();
    }
    format!("…/{tail}")
}

/// Whether `f` points into `path`: its own location or a source's.
pub(crate) fn touches(session: &Session, f: &Finding, path: &str) -> bool {
    f.location.as_ref().is_some_and(|l| l.path == path)
        || f.sources.iter().any(|key| {
            session
                .raw_finding(key)
                .and_then(|r| r.location.as_ref())
                .is_some_and(|l| l.path == path)
        })
}

/// Whether `f` holds every word of `query`, case aside, in its id, claim,
/// rationale, fix or location, or a source's claim, evidence or fix.
fn holds(session: &Session, f: &Finding, query: &str) -> bool {
    let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
    if words.is_empty() {
        return true;
    }
    let mut hay = format!("{}\n{}\n{}", f.id, f.claim, f.rationale).to_lowercase();
    let mut add = |text: &str| {
        hay.push('\n');
        hay.push_str(&text.to_lowercase());
    };
    if let Some(fix) = &f.fix {
        add(fix);
    }
    if let Some(loc) = &f.location {
        add(&loc.to_string());
    }
    for raw in f.sources.iter().filter_map(|key| session.raw_finding(key)) {
        add(&raw.claim);
        add(&raw.evidence);
        if let Some(fix) = &raw.fix {
            add(fix);
        }
    }
    words.iter().all(|w| hay.contains(w.as_str()))
}

/// The findings `filters` let through, in rank order. A rebuild ranks once,
/// here, for the list it shows; counts go through [`count`], unranked.
pub(crate) fn shown<'a>(session: &'a Session, filters: &Filters) -> Vec<&'a Finding> {
    ranked(session)
        .into_iter()
        .filter(|f| passes(session, filters, f))
        .collect()
}

/// How many findings `filters` let through (a count needs no order).
pub(crate) fn count(session: &Session, filters: &Filters) -> usize {
    session
        .findings
        .iter()
        .filter(|f| passes(session, filters, f))
        .count()
}

#[cfg(test)]
thread_local! {
    /// How many times this thread ranked a session's findings.
    pub(super) static RANKINGS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

fn ranked(session: &Session) -> Vec<&Finding> {
    #[cfg(test)]
    RANKINGS.with(|n| n.set(n.get() + 1));
    session.ranked()
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

/// How many findings the filters let through, the reviewer filter when one
/// is on, and 条件をすべて解除: in the tab row, so the filter rows take no
/// line of their own for them.
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
            session.findings.len()
        ),
    );
    if let Some(part) = &filters.reviewer {
        tag(tree, row, format!("レビュアー: {part}"), Tone::Accent);
    }
    if let Some(path) = &filters.path {
        let chip = tree.insert(
            Some(row),
            Button::new(format!("ファイル: {} ✕", short_path(path)))
                .control_size(ControlSize::Small)
                .selected(true)
                .tooltip(path.clone()),
        );
        tree.set_accessible_label(chip, format!("ファイル {path} の絞り込みを解除"));
        actions.push((chip, Action::FilterFile(None)));
    }
    if has_constraints(filters) {
        chip(
            tree,
            row,
            "条件をすべて解除".into(),
            false,
            Action::ClearFilters,
            actions,
        );
    }
}

fn has_constraints(filters: &Filters) -> bool {
    !filters.severities.is_empty()
        || detail_count(filters) > 0
        || filters.reviewer.is_some()
        || filters.path.is_some()
        || !filters.query.trim().is_empty()
}

fn detail_count(filters: &Filters) -> usize {
    usize::from(!filters.lenses.is_empty())
        + usize::from(!filters.statuses.is_empty())
        + usize::from(!filters.agreements.is_empty())
}

fn detail_summary(session: &Session, filters: &Filters) -> String {
    let mut labels = Vec::new();
    if !filters.lenses.is_empty() {
        labels.push(format!(
            "観点: {}",
            filters
                .lenses
                .iter()
                .map(|id| session.category_label(id))
                .collect::<Vec<_>>()
                .join("・")
        ));
    }
    if filters.statuses == Filters::default().statuses {
        labels.push("状態: 対応中の指摘".to_owned());
    } else if !filters.statuses.is_empty() {
        labels.push(format!(
            "状態: {}",
            FindingStatus::ALL
                .iter()
                .filter(|s| filters.statuses.contains(&status_key(**s)))
                .map(|s| s.label())
                .collect::<Vec<_>>()
                .join("・")
        ));
    }
    if !filters.agreements.is_empty() {
        labels.push(format!(
            "合意度: {}",
            Agreement::ALL
                .iter()
                .filter(|a| filters.agreements.contains(&agreement_key(**a)))
                .map(|a| a.label())
                .collect::<Vec<_>>()
                .join("・")
        ));
    }
    labels.join("  /  ")
}

pub(super) fn insert(
    tree: &mut UiTree,
    host: WidgetId,
    session: &Session,
    filters: &Filters,
    selected: Option<&str>,
    expanded: bool,
    actions: &mut Vec<(WidgetId, Action)>,
) {
    // The filters head the scrolled list, so a short pane gives its height
    // to the findings once scrolled past them.
    let list = scroll_column(tree, host, 8.0);
    keep_scroll(tree, list, session, "board");
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
    let controls = tree.insert(
        Some(bar),
        Container::wrapping_row(6.0, 4.0).align_items(AlignItems::CENTER),
    );
    let groups = detail_count(filters);
    let label = if expanded {
        "詳細条件を閉じる".to_owned()
    } else if groups > 0 {
        format!("詳細条件 · {groups} 適用")
    } else {
        "詳細条件".to_owned()
    };
    chip(
        tree,
        controls,
        label,
        expanded,
        Action::ToggleFilterDetails,
        actions,
    );
    if !expanded {
        let summary = detail_summary(session, filters);
        if !summary.is_empty() {
            note(tree, bar, summary);
        }
    }
    if expanded {
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
    }

    let shown = shown(session, filters);
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
        unplaced(tree, list, session);
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
        tag(tree, top, f.severity.label(), severity_tone(f.severity));
        caption(tree, top, f.id.clone());
        super::spacer(tree, top);
        tag(tree, top, f.status.label(), status_tone(f.status));
        tree.insert(
            Some(c),
            Label::new(f.claim.clone())
                .font_size(14.0)
                .weight(500)
                .wrap(true)
                .max_lines(2)
                .truncation(Truncation::Tail),
        );
        let bottom = tree.insert(
            Some(c),
            Container::wrapping_row(6.0, 4.0).align_items(AlignItems::CENTER),
        );
        match &f.location {
            Some(loc) => {
                let location = loc.to_string();
                let label = tree.insert(
                    Some(bottom),
                    Label::new(short_path(&location))
                        .font_size(12.0)
                        .color(Role::Muted),
                );
                tree.set_accessible_label(label, location);
            }
            None => caption(tree, bottom, "対象全体"),
        }
        caption(tree, bottom, session.category_label(&f.category));
        if !f.in_target {
            tag(tree, bottom, "対象外", Tone::Warning);
        } else if f.in_diff == Some(false) {
            tag(tree, bottom, "差分外", Tone::Neutral);
        }
        let tally = session.tally(f);
        if tally.agreement == Agreement::Split {
            tag(tree, bottom, tally.agreement.label(), Tone::Warning);
        }
        if is_selected {
            let origins: Vec<String> = f.origins().iter().map(|p| p.to_string()).collect();
            note(
                tree,
                c,
                format!(
                    "{} · {} · 提起: {}",
                    tally.label(),
                    tally.agreement.label(),
                    origins.join(", ")
                ),
            );
        }
    }
    unplaced(tree, list, session);
}

/// An ended review's raw findings the lead never consolidated, listed
/// under the board (they are in no finding, so no filter shows them).
fn unplaced(tree: &mut UiTree, list: WidgetId, session: &Session) {
    let raws = session.unplaced();
    if !session.closed || raws.is_empty() {
        return;
    }
    note(
        tree,
        list,
        format!(
            "取りまとめられずに終わった指摘 {} 件（リードの取りまとめ前に終了しました）",
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
