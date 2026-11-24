//! The 観点×重大度 tab: live findings counted per lens and severity (a
//! cell opens the board filtered to it), with lenses nobody holds greyed,
//! then file coverage: per target file its findings and who reviewed it.

use noa_gpui_core::style::{AlignItems, Size as LayoutSize, auto, length};
use noa_gpui_core::widgets::{Button, Container, ControlSize, Label, Pressable, Tone, Truncation};
use noa_gpui_core::{Role, Style, UiTree, WidgetId};

use review::model::types::{Session, Severity, covers};

use super::{Action, caption, heading, keep_scroll, note, scroll_column, severity_tone, tag};

/// The lens column's width when there is room; it gives way first in a
/// narrow pane (its label truncates).
const LENS_COL: f32 = 230.0;
/// The category of findings outside every lens (その他).
const OTHER: &str = "other";
/// A count column's width when there is room; the columns share what is
/// left over, and narrow no further than [`CELL_MIN`].
const CELL: f32 = 58.0;
const CELL_MIN: f32 = 40.0;

fn cell_box(tree: &mut UiTree, parent: WidgetId, width: f32) -> WidgetId {
    let justify = if width >= LENS_COL {
        noa_gpui_core::style::JustifyContent::START
    } else {
        noa_gpui_core::style::JustifyContent::CENTER
    };
    tree.insert(
        Some(parent),
        Container::new(Style {
            size: LayoutSize {
                width: auto(),
                height: length(30.0),
            },
            min_size: LayoutSize {
                width: length(if width >= LENS_COL { 0.0 } else { CELL_MIN }),
                height: auto(),
            },
            flex_basis: length(width),
            flex_grow: if width >= LENS_COL { 0.0 } else { 1.0 },
            flex_shrink: 1.0,
            align_items: AlignItems::CENTER,
            justify_content: justify,
            padding: noa_gpui_core::style::Rect {
                left: length(if width >= LENS_COL { 6.0 } else { 0.0 }),
                right: length(0.0),
                top: length(0.0),
                bottom: length(0.0),
            },
            ..Default::default()
        }),
    )
}

pub(super) fn insert(
    tree: &mut UiTree,
    host: WidgetId,
    session: &Session,
    actions: &mut Vec<(WidgetId, Action)>,
) {
    let list = scroll_column(tree, host, 10.0);
    keep_scroll(tree, list, session, "matrix");
    tree.insert(
        Some(list),
        Label::new("観点 × 重大度（未検証・合意・対立の指摘）")
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
    let header = tree.insert(Some(grid), Container::row().gap(2.0));
    let h = cell_box(tree, header, LENS_COL);
    caption(tree, h, "観点");
    for s in Severity::ALL {
        let c = cell_box(tree, header, CELL);
        caption(tree, c, s.label());
    }
    let c = cell_box(tree, header, CELL);
    caption(tree, c, "計");

    let live: Vec<_> = session
        .findings
        .iter()
        .filter(|f| f.status.is_live())
        .collect();
    // L1-5: the panel's lenses, その他 (always), then lenses nobody holds,
    // greyed and tagged whether or not a finding landed in them.
    let held = |id: &str| {
        id == OTHER
            || session
                .reviewers
                .iter()
                .any(|p| p.lens.as_ref().is_some_and(|l| l.id() == id))
    };
    let mut rows: Vec<(String, String)> = session
        .panel_lenses()
        .iter()
        .map(|l| (l.id().to_owned(), l.label()))
        .collect();
    let add = |rows: &mut Vec<(String, String)>, id: &str| {
        if !rows.iter().any(|(have, _)| have == id) {
            rows.push((id.to_owned(), session.category_label(id)));
        }
    };
    add(&mut rows, OTHER);
    for f in &live {
        add(&mut rows, &f.category);
    }
    for lens in session.unheld_lenses() {
        add(&mut rows, lens.id());
    }
    for (id, label) in rows {
        let held = held(&id);
        let row = tree.insert(
            Some(grid),
            Container::row().gap(2.0).align_items(AlignItems::CENTER),
        );
        let name = cell_box(tree, row, LENS_COL);
        let line = tree.insert(
            Some(name),
            Container::new(Style {
                flex_direction: noa_gpui_core::style::FlexDirection::Row,
                align_items: AlignItems::CENTER,
                gap: super::gap(4.0),
                min_size: LayoutSize {
                    width: length(0.0),
                    height: auto(),
                },
                ..Default::default()
            }),
        );
        tree.insert(
            Some(line),
            Label::new(label)
                .font_size(12.0)
                .color(if held { Role::Text } else { Role::Muted })
                .truncation(Truncation::Tail),
        );
        if !held {
            tag(tree, line, "担当なし", Tone::Neutral);
        }
        let mut total = 0;
        for s in Severity::ALL {
            let n = live
                .iter()
                .filter(|f| f.category == id && f.severity == s)
                .count();
            total += n;
            cell(
                tree,
                row,
                n,
                severity_tone(s),
                Action::ShowCell(id.clone(), Some(s)),
                actions,
            );
        }
        cell(
            tree,
            row,
            total,
            Tone::Accent,
            Action::ShowCell(id.clone(), None),
            actions,
        );
    }

    heading(tree, list, "ファイルのカバレッジ");
    let m = &session.manifest;
    if m.files.is_empty() && m.omitted.is_empty() {
        note(tree, list, "対象のファイルはありません");
    }
    for file in &m.files {
        let row = tree.insert(
            Some(list),
            Container::column()
                .gap(3.0)
                .padding(8.0)
                .background(Role::Surface)
                .border(1.0, Role::Separator)
                .radius(6.0),
        );
        let top = tree.insert(
            Some(row),
            Container::wrapping_row(6.0, 4.0).align_items(AlignItems::CENTER),
        );
        tree.insert(
            Some(top),
            Label::new(file.path.clone()).font_size(12.0).weight(600),
        );
        if !file.status.label().is_empty() {
            tag(tree, top, file.status.label(), Tone::Neutral);
        }
        if file.additions > 0 || file.deletions > 0 {
            caption(
                tree,
                top,
                format!("+{} −{}", file.additions, file.deletions),
            );
        }
        // The findings pointing into the file (their own location or a
        // source's), as the board's file filter takes them.
        let touching: Vec<_> = session
            .findings
            .iter()
            .filter(|f| super::board::touches(session, f, &file.path))
            .collect();
        let mut counts = Vec::new();
        for s in Severity::ALL {
            let n = live
                .iter()
                .filter(|f| f.severity == s && touching.iter().any(|t| t.id == f.id))
                .count();
            if n > 0 {
                counts.push(format!("{} {n}", s.label()));
            }
        }
        let settled = touching.iter().filter(|f| !f.status.is_live()).count();
        caption(
            tree,
            top,
            match (counts.is_empty(), settled) {
                (true, 0) => "指摘なし".to_owned(),
                (true, n) => format!("未解決の指摘なし（却下・取り下げ・統合 {n}）"),
                (false, _) => counts.join(" · "),
            },
        );
        if !touching.is_empty() {
            let show = tree.insert(
                Some(top),
                Button::new("このファイルの指摘").control_size(ControlSize::Small),
            );
            tree.set_accessible_label(show, format!("{} の指摘を表示", file.path));
            actions.push((show, Action::FilterFile(Some(file.path.clone()))));
        }
        let reviewed: Vec<String> = session
            .coverage
            .iter()
            .filter(|(_, c)| c.reviewed.iter().any(|e| covers(e, &file.path)))
            .map(|(p, _)| p.to_string())
            .collect();
        let skipped: Vec<String> = session
            .coverage
            .iter()
            .flat_map(|(p, c)| {
                c.skipped
                    .iter()
                    .filter(|(e, _)| covers(e, &file.path))
                    .map(move |(_, why)| format!("{p}（{why}）"))
            })
            .collect();
        let line = tree.insert(
            Some(row),
            Container::wrapping_row(6.0, 4.0).align_items(AlignItems::CENTER),
        );
        if reviewed.is_empty() {
            tag(tree, line, "未レビュー", Tone::Warning);
        } else {
            caption(tree, line, format!("レビュー済: {}", reviewed.join(", ")));
        }
        if !skipped.is_empty() {
            caption(tree, line, format!("スキップ: {}", skipped.join(", ")));
        }
    }
    for o in &m.omitted {
        let row = tree.insert(
            Some(list),
            Container::row()
                .gap(6.0)
                .align_items(AlignItems::CENTER)
                .padding(8.0),
        );
        tree.insert(
            Some(row),
            Label::new(o.path.clone())
                .font_size(12.0)
                .color(Role::Muted),
        );
        tag(tree, row, "スナップショット外", Tone::Neutral);
        caption(tree, row, o.reason.clone());
    }
}

fn cell(
    tree: &mut UiTree,
    row: WidgetId,
    n: usize,
    tone: Tone,
    action: Action,
    actions: &mut Vec<(WidgetId, Action)>,
) {
    let host = cell_box(tree, row, CELL);
    if n == 0 {
        tree.insert(
            Some(host),
            Label::new("·").font_size(12.0).color(Role::Muted),
        );
        return;
    }
    let press = tree.insert(
        Some(host),
        Pressable::new(Style {
            size: LayoutSize {
                width: length(CELL - 6.0),
                height: auto(),
            },
            ..Default::default()
        })
        .label(format!("{n} 件"))
        .radius(6.0),
    );
    actions.push((press, action));
    let inner = tree.insert(
        Some(press),
        Container::row()
            .align_items(AlignItems::CENTER)
            .padding(4.0)
            .grow(1.0),
    );
    tag(tree, inner, n.to_string(), tone);
}
