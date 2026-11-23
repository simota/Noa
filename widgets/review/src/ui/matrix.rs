//! The 観点×重大度 tab: live findings counted per lens and severity (a
//! cell opens the board filtered to it), with lenses nobody holds greyed,
//! then file coverage: per target file its findings and who reviewed it.

use noa_gpui_core::style::{AlignItems, Size as LayoutSize, auto, length};
use noa_gpui_core::widgets::{Container, Label, Pressable, Tone, Truncation};
use noa_gpui_core::{Role, Style, UiTree, WidgetId};

use review::model::types::{Session, Severity, covers};

use super::{Action, caption, heading, keep_scroll, note, scroll_column, severity_tone, tag};

const LENS_COL: f32 = 230.0;
const CELL: f32 = 58.0;

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
                width: length(width),
                height: length(30.0),
            },
            flex_shrink: 0.0,
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
    let mut rows: Vec<(String, String, bool)> = session
        .panel_lenses()
        .iter()
        .map(|l| (l.id().to_owned(), l.label(), true))
        .collect();
    for f in &live {
        if !rows.iter().any(|(id, _, _)| *id == f.category) {
            rows.push((
                f.category.clone(),
                session.category_label(&f.category),
                true,
            ));
        }
    }
    for lens in session.unheld_lenses() {
        if !rows.iter().any(|(id, _, _)| id == lens.id()) {
            rows.push((lens.id().to_owned(), lens.label().to_owned(), false));
        }
    }
    for (id, label, held) in rows {
        let row = tree.insert(
            Some(grid),
            Container::row().gap(2.0).align_items(AlignItems::CENTER),
        );
        let name = cell_box(tree, row, LENS_COL);
        let line = tree.insert(
            Some(name),
            Container::row().gap(4.0).align_items(AlignItems::CENTER),
        );
        tree.insert(
            Some(line),
            Label::new(label)
                .font_size(12.0)
                .color(if held { Role::Text } else { Role::Muted })
                .truncation(Truncation::Tail),
        );
        let held_by_panel = session
            .reviewers
            .iter()
            .any(|p| p.lens.as_ref().is_some_and(|l| l.id() == id));
        if !held_by_panel {
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
        let mut counts = Vec::new();
        for s in Severity::ALL {
            let n = live
                .iter()
                .filter(|f| {
                    f.severity == s && f.location.as_ref().is_some_and(|l| l.path == file.path)
                })
                .count();
            if n > 0 {
                counts.push(format!("{} {n}", s.label()));
            }
        }
        caption(
            tree,
            top,
            if counts.is_empty() {
                "指摘なし".to_owned()
            } else {
                counts.join(" · ")
            },
        );
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
