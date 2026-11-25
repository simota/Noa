//! A task's 関連成果 (`docs/specs/artifact-links.md` L1-4, L1-6): its
//! runs, the changes each produced, and under each change the reviews and
//! verifications that judged exactly it, read from the link store by the
//! feed ([`RelatedView`]); each row's button ([`links::Nav`]), リンクを追加
//! and リンクを外す (asked first).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use noa_gpui_core::style::FlexDirection;
use noa_gpui_core::widgets::{Button, Container, ControlSize, Label, Tag, Tone};
use noa_gpui_core::{Role, Style, UiTree, WidgetId};
use noa_gpui_scene::Color;

use tasks::links::{self, ChangeEntry, Current, LinkId, Nav, Node, Presence, ResultEntry};
use tasks::model::types::{Millis, TaskRef};

use super::style::gap;

/// A missing record's colour.
const PROBLEM: Color = Color::hex(0xff9f0a);

/// What the feed read of a task's links (off the UI thread).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelatedView {
    pub task: TaskRef,
    /// `Err`: why the store could not be read.
    pub related: Result<links::Related, String>,
    pub current: Option<Current>,
    /// Each node's presence (`Unknown` when absent).
    pub presence: BTreeMap<Node, Presence>,
    /// `<data dir>/noa`: where a review's or verification's dir is rebuilt.
    pub data: PathBuf,
    /// The main worktree.
    pub repo: PathBuf,
    /// When it was read.
    pub at: Millis,
}

/// The kinds リンクを追加 offers, in order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkKind {
    Review,
    Verify,
    Commit,
    Run,
}

impl LinkKind {
    pub const ALL: [LinkKind; 4] = [
        LinkKind::Review,
        LinkKind::Verify,
        LinkKind::Commit,
        LinkKind::Run,
    ];

    pub fn label(self) -> &'static str {
        match self {
            LinkKind::Review => "レビュー",
            LinkKind::Verify => "検証",
            LinkKind::Commit => "コミット",
            LinkKind::Run => "実行",
        }
    }

    /// The ID field's placeholder.
    pub fn hint(self) -> &'static str {
        match self {
            LinkKind::Review => "rev-…",
            LinkKind::Verify => "ver-…",
            LinkKind::Commit => "SHA（7 文字以上）",
            LinkKind::Run => "run-…",
        }
    }
}

/// `<prefix><digits>[-<digits>]`.
fn numbered(id: &str, prefix: &str) -> bool {
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    match id.strip_prefix(prefix).map(|rest| rest.split_once('-')) {
        Some(Some((millis, n))) => digits(millis) && digits(n),
        Some(None) => digits(&id[prefix.len()..]),
        None => false,
    }
}

/// `id` typed for `kind`, trimmed (a SHA lowercased), when it has the
/// kind's form; else why not. Whether it is found is the feed's to check.
pub fn check_link_id(kind: LinkKind, id: &str) -> Result<String, String> {
    let id = id.trim();
    if id.is_empty() {
        return Err("ID を入力してください".to_owned());
    }
    let ok = match kind {
        LinkKind::Review => links::review_dir(Path::new("/"), id).is_some(),
        LinkKind::Verify => links::verify_dir(Path::new("/"), id).is_some(),
        LinkKind::Commit => {
            (7..=64).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_hexdigit())
        }
        LinkKind::Run => numbered(id, "run-"),
    };
    if !ok {
        return Err(format!(
            "{} の ID は {} の形式です",
            kind.label(),
            kind.hint()
        ));
    }
    Ok(match kind {
        LinkKind::Commit => id.to_ascii_lowercase(),
        _ => id.to_owned(),
    })
}

/// What a button of the section does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RelatedAction {
    /// A row's 開く / コピー.
    Nav(Nav),
    /// リンクを外す: asks first (`what` names the row).
    AskUnlink { what: String, links: Vec<LinkId> },
    /// The question's 外す.
    ConfirmUnlink,
    /// The question's やめる.
    CancelUnlink,
    /// リンクを追加: opens its form.
    AddLink,
}

/// A リンクを外す waiting for its answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unlinking {
    pub what: String,
    pub links: Vec<LinkId>,
}

/// Fills `parent` (the detail's column) with the section. `view` is
/// `None` while it is being read; `asking` is a リンクを外す waiting for
/// its answer. The buttons it made, each with what a click does.
pub(super) fn insert(
    tree: &mut UiTree,
    parent: WidgetId,
    view: Option<&RelatedView>,
    asking: Option<&Unlinking>,
) -> Vec<(WidgetId, RelatedAction)> {
    let mut b = Builder {
        tree,
        actions: Vec::new(),
    };
    b.label(parent, "関連成果", 12.0, 700, Role::Muted);
    let bar = b
        .tree
        .insert(Some(parent), Container::wrapping_row(6.0, 6.0));
    b.button(bar, "リンクを追加", RelatedAction::AddLink);
    if let Some(ask) = asking {
        let frame = frame(b.tree, parent);
        b.label(
            frame,
            &format!("「{}」のリンクを外しますか？", ask.what),
            12.0,
            600,
            Role::Text,
        );
        let row = b
            .tree
            .insert(Some(frame), Container::wrapping_row(6.0, 6.0));
        b.button(row, "外す", RelatedAction::ConfirmUnlink);
        b.button(row, "やめる", RelatedAction::CancelUnlink);
    }
    let Some(view) = view else {
        b.muted(parent, "読み込み中…");
        return b.actions;
    };
    let related = match &view.related {
        Ok(related) => related,
        Err(why) => {
            b.problem(parent, why);
            return b.actions;
        }
    };
    if related.runs.is_empty() && related.changes.is_empty() && related.loose.is_empty() {
        b.muted(parent, "関連する成果はまだありません");
        return b.actions;
    }
    for run in &related.runs {
        let node = Node::Run(run.run.clone());
        let frame = frame(b.tree, parent);
        let title = match &run.info.label {
            Some(label) => format!("実行 {} · {label}", run.run),
            None => format!("実行 {}", run.run),
        };
        b.label(frame, &title, 12.0, 600, Role::Text);
        let place = match (&run.info.branch, &run.info.workdir) {
            (Some(branch), Some(dir)) => Some(format!("{branch} · {}", dir.display())),
            (None, Some(dir)) => Some(dir.display().to_string()),
            _ => None,
        };
        if let Some(place) = place {
            b.muted(frame, &place);
        }
        b.missing(frame, view, &node);
        let row = b
            .tree
            .insert(Some(frame), Container::wrapping_row(6.0, 6.0));
        b.button(row, "開く", RelatedAction::Nav(Nav::Run(run.run.clone())));
        b.unlink(row, format!("実行 {}", run.run), vec![run.link.clone()]);
        // A run in the repository itself (no branch): its current change
        // is whatever the checkout is at.
        let here = run.info.branch.is_none() && run.info.workdir.is_some();
        for change in &run.changes {
            b.change(frame, view, change, here);
        }
        if run.changes.is_empty() {
            b.muted(frame, "記録された版はありません");
        }
    }
    if !related.changes.is_empty() {
        b.label(parent, "このタスクにリンクした版", 12.0, 600, Role::Muted);
        let frame = frame(b.tree, parent);
        for change in &related.changes {
            b.change(frame, view, change, false);
        }
    }
    if !related.loose.is_empty() {
        b.label(parent, "対象の版なし", 12.0, 600, Role::Muted);
        let frame = frame(b.tree, parent);
        for result in &related.loose {
            b.result(frame, view, result);
        }
    }
    b.actions
}

/// A bordered column.
fn frame(tree: &mut UiTree, parent: WidgetId) -> WidgetId {
    tree.insert(
        Some(parent),
        Container::new(Style {
            flex_direction: FlexDirection::Column,
            gap: gap(6.0),
            padding: noa_gpui_core::style::Rect::length(10.0),
            flex_shrink: 0.0,
            ..Default::default()
        })
        .border(1.0, Role::Separator)
        .radius(8.0),
    )
}

/// How a row names a result's kind and id.
fn result_name(node: &Node) -> String {
    match node {
        Node::Review(id) => format!("レビュー {id}"),
        Node::Verify(id) => format!("検証 {id}"),
        other => format!("{other:?}"),
    }
}

struct Builder<'t> {
    tree: &'t mut UiTree,
    actions: Vec<(WidgetId, RelatedAction)>,
}

impl Builder<'_> {
    fn label(&mut self, parent: WidgetId, text: &str, size: f32, weight: u16, role: Role) {
        self.tree.insert(
            Some(parent),
            Label::new(text)
                .font_size(size)
                .weight(weight)
                .wrap(true)
                .color(role),
        );
    }

    fn muted(&mut self, parent: WidgetId, text: &str) {
        self.label(parent, text, 11.0, 400, Role::Muted);
    }

    fn problem(&mut self, parent: WidgetId, text: &str) {
        self.tree.insert(
            Some(parent),
            Label::new(text).font_size(11.0).wrap(true).color(PROBLEM),
        );
    }

    fn button(&mut self, parent: WidgetId, label: &str, action: RelatedAction) {
        let id = self.tree.insert(
            Some(parent),
            Button::new(label).control_size(ControlSize::Small),
        );
        self.actions.push((id, action));
    }

    /// リンクを外す for the row's own links; nothing when it has none (a
    /// link at a run, a result's `Judged`: shared with other tasks).
    fn unlink(&mut self, parent: WidgetId, what: String, links: Vec<LinkId>) {
        if !links.is_empty() {
            self.button(
                parent,
                "リンクを外す",
                RelatedAction::AskUnlink { what, links },
            );
        }
    }

    /// 記録が見つからない, with what it pointed to, when `node` is missing.
    fn missing(&mut self, parent: WidgetId, view: &RelatedView, node: &Node) {
        if let Some(Presence::Missing { what }) = view.presence.get(node) {
            self.problem(parent, &format!("記録が見つからない（{what}）"));
        }
    }

    fn change(&mut self, parent: WidgetId, view: &RelatedView, change: &ChangeEntry, here: bool) {
        let node = Node::Change(change.change.clone());
        let box_ = self.tree.insert(
            Some(parent),
            Container::new(Style {
                flex_direction: FlexDirection::Column,
                gap: gap(4.0),
                padding: noa_gpui_core::style::Rect {
                    left: noa_gpui_core::style::length(10.0),
                    ..noa_gpui_core::style::Rect::length(0.0)
                },
                flex_shrink: 0.0,
                ..Default::default()
            }),
        );
        let head = self
            .tree
            .insert(Some(box_), Container::wrapping_row(6.0, 4.0));
        self.tree.insert(
            Some(head),
            Label::new(format!("版 {}", change.change.short()))
                .font_size(12.0)
                .weight(600),
        );
        if change.current {
            self.tree
                .insert(Some(head), Tag::new("現在").tone(Tone::Success));
        }
        if change.current && here {
            self.muted(box_, "この作業ツリーの現在の状態");
        }
        self.missing(box_, view, &node);
        let row = self
            .tree
            .insert(Some(box_), Container::wrapping_row(6.0, 6.0));
        self.button(
            row,
            "ID をコピー",
            RelatedAction::Nav(Nav::Copy(change.change.as_str().to_owned())),
        );
        if let Some(link) = &change.link {
            self.unlink(
                row,
                format!("版 {}", change.change.short()),
                vec![link.clone()],
            );
        }
        for result in &change.results {
            self.result(box_, view, result);
        }
        if change.results.is_empty() {
            self.muted(box_, "この版の結果はまだありません");
        }
    }

    fn result(&mut self, parent: WidgetId, view: &RelatedView, result: &ResultEntry) {
        let row = self.tree.insert(
            Some(parent),
            Container::new(Style {
                flex_direction: FlexDirection::Column,
                gap: gap(4.0),
                flex_shrink: 0.0,
                ..Default::default()
            }),
        );
        let mut title = result_name(&result.node);
        if let Some(outcome) = &result.info.outcome {
            title.push_str(&format!(" · {outcome}"));
        }
        self.label(row, &title, 12.0, 400, Role::Text);
        let target = match (&result.judged, result.stale) {
            (Some(change), true) => format!("旧版 {} に対する結果", change.short()),
            (Some(change), false) => format!("{} に対する結果", change.short()),
            (None, _) => "対象の版なし".to_owned(),
        };
        self.muted(row, &target);
        self.missing(row, view, &result.node);
        let buttons = self
            .tree
            .insert(Some(row), Container::wrapping_row(6.0, 6.0));
        if let Some(nav) = Nav::of(&result.node, &view.data, &view.repo) {
            self.button(buttons, "開く", RelatedAction::Nav(nav));
        }
        self.unlink(buttons, result_name(&result.node), result.links.clone());
    }
}
