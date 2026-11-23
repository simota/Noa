//! The start view: the target (git diff, files or a document), what the
//! reviewers should know, the lead, the reviewer panel (rows of agent,
//! model, effort and lens), the round limit, and the list of past reviews to
//! open or resume.

use std::path::{Path, PathBuf};

use noa_gpui_core::style::{
    AlignItems, FlexDirection, Overflow, Point as LayoutPoint, Rect as LayoutRect,
    Size as LayoutSize, auto, length, percent,
};
use noa_gpui_core::widgets::{
    Button, ComboBox, Container, ControlSize, Label, NumberInput, PopUpButton, ScrollView,
    SegmentMode, SegmentedControl, TextArea, TextField, Tone,
};
use noa_gpui_core::{Role, Style, UiTree, WidgetId};
use noa_gpui_text::TextStore;

use review::model::types::{
    AgentChoice, AgentKind, AgentSetup, BuiltinLens, DEFAULT_MAX_ROUNDS, Head, LensChoice,
    MAX_MAX_ROUNDS, MAX_PANEL, MIN_PANEL, Millis, ReviewerChoice, RoundNo, Severity, Target,
    TargetKind,
};
use review::store::Listed;

use super::{Action, caption, clear_children, gap, spacer, tag};
use crate::live::Link;

/// How many past reviews the start view lists.
pub(super) const PAST_LIMIT: usize = 12;

const CARD_WIDTH: f32 = 660.0;
const FIELD_WIDTH: f32 = 612.0;

pub(super) struct Parts {
    pub(super) root: WidgetId,
    pub(super) kind: WidgetId,
    git: WidgetId,
    files: WidgetId,
    document: WidgetId,
    pub(super) repo: WidgetId,
    pub(super) base: WidgetId,
    pub(super) head: WidgetId,
    pub(super) filter: WidgetId,
    pub(super) root_dir: WidgetId,
    pub(super) paths: WidgetId,
    pub(super) text: WidgetId,
    pub(super) document_file: WidgetId,
    pub(super) document_workdir: WidgetId,
    pub(super) request: WidgetId,
    pub(super) max_rounds: WidgetId,
    pub(super) lead: WidgetId,
    pub(super) lead_setup: SetupParts,
    panel_list: WidgetId,
    pub(super) panel: Vec<PanelRow>,
    pub(super) add_row: WidgetId,
    pub(super) start_button: WidgetId,
    note: WidgetId,
    past: WidgetId,
}

/// One reviewer row: agent kind, model, effort, lens, custom lens text, and
/// its remove button.
pub(super) struct PanelRow {
    host: WidgetId,
    pub(super) kind: WidgetId,
    pub(super) setup: SetupParts,
    pub(super) lens: WidgetId,
    pub(super) custom: WidgetId,
    pub(super) remove: WidgetId,
}

/// What the start form holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Form {
    pub kind: TargetKind,
    pub repo: String,
    pub base: String,
    /// Empty: the work tree.
    pub head: String,
    pub filter: String,
    pub root: String,
    pub paths: String,
    pub text: String,
    pub document_file: String,
    pub document_workdir: String,
    pub request: String,
    pub max_rounds: RoundNo,
    pub lead: AgentChoice,
    pub panel: Vec<ReviewerChoice>,
}

impl Default for Form {
    fn default() -> Self {
        let row = |kind: AgentKind, lens| ReviewerChoice {
            agent: kind.into(),
            lens: LensChoice::Builtin(lens),
        };
        Self {
            kind: TargetKind::Git,
            repo: String::new(),
            base: String::new(),
            head: String::new(),
            filter: String::new(),
            root: String::new(),
            paths: String::new(),
            text: String::new(),
            document_file: String::new(),
            document_workdir: String::new(),
            request: String::new(),
            max_rounds: DEFAULT_MAX_ROUNDS,
            lead: AgentKind::Claude.into(),
            panel: vec![
                row(AgentKind::Claude, BuiltinLens::Correctness),
                row(AgentKind::Codex, BuiltinLens::Security),
                row(AgentKind::Agy, BuiltinLens::Design),
            ],
        }
    }
}

fn words(text: &str) -> Vec<String> {
    text.split_whitespace().map(str::to_owned).collect()
}

impl Form {
    /// The target the form describes, with typed paths checked.
    pub(crate) fn target(&self) -> Result<Target, String> {
        let blank = Target {
            kind: self.kind,
            path: None,
            base: None,
            head: Head::Worktree,
            paths: Vec::new(),
            text: None,
            workdir: None,
        };
        Ok(match self.kind {
            TargetKind::Git => Target {
                path: Some(dir_path(&self.repo, "リポジトリ")?),
                base: Some(self.base.trim().to_owned()).filter(|b| !b.is_empty()),
                head: match self.head.trim() {
                    "" => Head::Worktree,
                    other => Head::Ref(other.to_owned()),
                },
                paths: words(&self.filter),
                ..blank
            },
            TargetKind::Files => Target {
                path: Some(dir_path(&self.root, "ルートのフォルダ")?),
                paths: self
                    .paths
                    .lines()
                    .map(str::trim)
                    .filter(|l| !l.is_empty())
                    .map(str::to_owned)
                    .collect(),
                ..blank
            },
            TargetKind::Document => {
                let text = Some(self.text.trim().to_owned()).filter(|t| !t.is_empty());
                let path = match (&text, self.document_file.trim()) {
                    (None, file) if !file.is_empty() => Some(expand(file)?),
                    _ => None,
                };
                if text.is_none() && path.is_none() {
                    return Err("レビューする文書を入力するか、ファイルを指定してください".into());
                }
                let workdir = match self.document_workdir.trim() {
                    "" => None,
                    dir => Some(dir_path(dir, "作業ディレクトリ")?),
                };
                Target {
                    path,
                    text,
                    workdir,
                    ..blank
                }
            }
        })
    }
}

fn card_container() -> Container {
    Container::new(Style {
        flex_direction: FlexDirection::Column,
        flex_shrink: 0.0,
        gap: gap(12.0),
        padding: LayoutRect::length(24.0),
        size: LayoutSize {
            width: length(CARD_WIDTH),
            height: auto(),
        },
        max_size: LayoutSize {
            width: percent(1.0),
            height: auto(),
        },
        ..Default::default()
    })
    .background(Role::Surface)
    .border(1.0, Role::Separator)
    .radius(10.0)
}

fn section(tree: &mut UiTree, parent: WidgetId, title: &str) -> WidgetId {
    let col = tree.insert(Some(parent), Container::column().gap(6.0));
    tree.insert(Some(col), Label::new(title).font_size(13.0).weight(600));
    col
}

fn field(tree: &mut UiTree, parent: WidgetId, label: &str, placeholder: &str) -> WidgetId {
    caption(tree, parent, label);
    tree.insert(Some(parent), TextField::new(placeholder, FIELD_WIDTH))
}

pub(super) fn insert(tree: &mut UiTree, parent: WidgetId) -> Parts {
    let root = tree.insert(
        Some(parent),
        ScrollView::new(Style {
            overflow: LayoutPoint {
                x: Overflow::Hidden,
                y: Overflow::Scroll,
            },
            ..super::fill()
        }),
    );
    let column = tree.insert(
        Some(root),
        Container::new(Style {
            flex_direction: FlexDirection::Column,
            align_items: AlignItems::CENTER,
            gap: gap(16.0),
            padding: LayoutRect::length(24.0),
            size: LayoutSize {
                width: percent(1.0),
                height: auto(),
            },
            ..Default::default()
        }),
    );
    let card = tree.insert(Some(column), card_container());
    tree.insert(
        Some(card),
        Label::new("新しいレビュー").font_size(17.0).weight(600),
    );
    tree.insert(
        Some(card),
        Label::new(
            "観点の違うレビュアーが同じ対象を独立にレビューし、次のラウンドで互いの指摘を\
             検証します。リードが毎ラウンド指摘を統合し、最終ラウンドで総合判定とレポートを出します。",
        )
        .font_size(12.0)
        .color(Role::Muted)
        .wrap(true),
    );

    let target = section(tree, card, "レビュー対象");
    let kind = tree.insert(
        Some(target),
        SegmentedControl::new(
            TargetKind::ALL.iter().map(|k| k.label()),
            SegmentMode::SelectOne,
        )
        .with_selected(&[0]),
    );
    let git = tree.insert(Some(target), Container::column().gap(6.0));
    let repo = field(
        tree,
        git,
        "リポジトリ（作業ツリー内のパス）",
        "例: ~/repos/my-app",
    );
    let refs = tree.insert(Some(git), Container::row().gap(12.0));
    let base_col = tree.insert(Some(refs), Container::column().gap(6.0));
    caption(tree, base_col, "比較元");
    let base = tree.insert(
        Some(base_col),
        TextField::new("自動（origin の既定ブランチ）", 300.0),
    );
    let head_col = tree.insert(Some(refs), Container::column().gap(6.0));
    caption(tree, head_col, "比較先");
    let head = tree.insert(
        Some(head_col),
        TextField::new("作業ツリー（未コミットを含む）", 300.0),
    );
    let filter = field(
        tree,
        git,
        "パスの絞り込み（任意・空白区切り）",
        "例: crates/noa-app src/",
    );

    let files = tree.insert(Some(target), Container::column().gap(6.0));
    let root_dir = field(
        tree,
        files,
        "ルートのフォルダ",
        "例: ~/repos/my-app/crates/store",
    );
    caption(tree, files, "パス（任意・1行に1つ。空欄でフォルダ全体）");
    let paths = tree.insert(Some(files), TextArea::new("src/\nCargo.toml").min_lines(2));

    let document = tree.insert(Some(target), Container::column().gap(6.0));
    caption(tree, document, "文書（貼り付け）");
    let text = tree.insert(
        Some(document),
        TextArea::new("レビューしたい設計書・計画・仕様をここに貼り付けます").min_lines(4),
    );
    let document_file = field(
        tree,
        document,
        "またはファイル（本文が空のとき）",
        "例: ~/repos/my-app/docs/plan.md",
    );
    let document_workdir = field(
        tree,
        document,
        "作業ディレクトリ（任意・参照用）",
        "例: ~/repos/my-app",
    );
    tree.insert(
        Some(target),
        Label::new(
            "開始時に対象をスナップショットとして保存し、全員が同じ内容をレビューします（作業ディレクトリ内の .noa/reviews/ に置かれ、git には載りません）。",
        )
        .font_size(11.0)
        .color(Role::Muted)
        .wrap(true),
    );

    let ask = section(tree, card, "依頼・重点（任意）");
    let request = tree.insert(
        Some(ask),
        TextArea::new("例: ストアの書き換えです。ロックと再開まわりを重点的に見てください")
            .min_lines(2),
    );

    let lead_col = section(tree, card, "リード（取りまとめ役）");
    let lead_row = tree.insert(
        Some(lead_col),
        Container::row().align_items(AlignItems::CENTER).gap(6.0),
    );
    let lead = tree.insert(
        Some(lead_row),
        PopUpButton::new(AgentKind::ALL.iter().map(|k| k.command())),
    );
    let lead_setup = insert_setup(tree, lead_row, AgentKind::ALL[0]);

    let panel_col = section(
        tree,
        card,
        &format!("レビュアー（{MIN_PANEL}〜{MAX_PANEL}体）・観点"),
    );
    let panel_list = tree.insert(Some(panel_col), Container::column().gap(8.0));
    let panel = Form::default()
        .panel
        .iter()
        .map(|choice| insert_row(tree, panel_list, choice))
        .collect();
    let panel_foot = tree.insert(
        Some(panel_col),
        Container::row().align_items(AlignItems::CENTER).gap(8.0),
    );
    let add_row = tree.insert(
        Some(panel_foot),
        Button::new("＋ レビュアーを追加").control_size(ControlSize::Small),
    );
    tree.insert(
        Some(panel_foot),
        Label::new("同じエージェントの2体目以降は claude-2 … として参加します")
            .font_size(11.0)
            .color(Role::Muted),
    );

    let rounds_col = section(tree, card, "ラウンド上限");
    let rounds_row = tree.insert(
        Some(rounds_col),
        Container::row().align_items(AlignItems::CENTER).gap(10.0),
    );
    let max_rounds = tree.insert(
        Some(rounds_row),
        NumberInput::new(
            1.0,
            f64::from(MAX_MAX_ROUNDS),
            f64::from(DEFAULT_MAX_ROUNDS),
        )
        .step(1.0)
        .decimals(0)
        .stepper(true)
        .width(80.0),
    );
    caption(
        tree,
        rounds_row,
        "R1 は独立レビュー、R2 以降は相互検証。最終ラウンドでリードが総合判定を出します",
    );

    let foot = tree.insert(
        Some(card),
        Container::row().align_items(AlignItems::CENTER).gap(8.0),
    );
    let note = tree.insert(Some(foot), Container::column().grow(1.0));
    let start_button = tree.insert(
        Some(foot),
        Button::new("レビューを開始").default_button(true),
    );
    let past = tree.insert(Some(column), card_container());
    let mut parts = Parts {
        root,
        kind,
        git,
        files,
        document,
        repo,
        base,
        head,
        filter,
        root_dir,
        paths,
        text,
        document_file,
        document_workdir,
        request,
        max_rounds,
        lead,
        lead_setup,
        panel_list,
        panel,
        add_row,
        start_button,
        note,
        past,
    };
    show_kind(tree, &parts, TargetKind::Git);
    sync_row_buttons(tree, &mut parts);
    parts
}

/// Shows the fields of target `kind` only.
fn show_kind(tree: &mut UiTree, parts: &Parts, kind: TargetKind) {
    tree.set_hidden(parts.git, kind != TargetKind::Git);
    tree.set_hidden(parts.files, kind != TargetKind::Files);
    tree.set_hidden(parts.document, kind != TargetKind::Document);
}

fn selected_target(tree: &UiTree, parts: &Parts) -> TargetKind {
    tree.get::<SegmentedControl>(parts.kind)
        .and_then(|s| s.selected().iter().position(|on| *on))
        .and_then(|i| TargetKind::ALL.get(i).copied())
        .unwrap_or(TargetKind::Git)
}

/// The target kind changed: its fields show; `false` when `source` is not
/// the kind control.
pub(super) fn target_kind_changed(tree: &mut UiTree, parts: &Parts, source: WidgetId) -> bool {
    if source != parts.kind {
        return false;
    }
    let kind = selected_target(tree, parts);
    show_kind(tree, parts, kind);
    true
}

/// The model box and effort menu of one agent.
pub(super) struct SetupParts {
    host: WidgetId,
    pub(super) model: WidgetId,
    pub(super) effort: WidgetId,
}

const DEFAULT_MODEL: &str = "既定のモデル";
const DEFAULT_EFFORT: &str = "既定のエフォート";

fn insert_setup(tree: &mut UiTree, parent: WidgetId, kind: AgentKind) -> SetupParts {
    let host = tree.insert(
        Some(parent),
        Container::row().align_items(AlignItems::CENTER).gap(6.0),
    );
    let (model, effort) = insert_setup_controls(tree, host, kind);
    SetupParts {
        host,
        model,
        effort,
    }
}

fn insert_setup_controls(
    tree: &mut UiTree,
    host: WidgetId,
    kind: AgentKind,
) -> (WidgetId, WidgetId) {
    let model = tree.insert(
        Some(host),
        ComboBox::new(kind.models().iter().copied(), 190.0).placeholder(DEFAULT_MODEL),
    );
    let effort = tree.insert(
        Some(host),
        PopUpButton::new(std::iter::once(DEFAULT_EFFORT).chain(kind.efforts().iter().copied())),
    );
    (model, effort)
}

fn set_setup(tree: &mut UiTree, parts: &SetupParts, kind: AgentKind, setup: &AgentSetup) {
    let model = setup.model.clone().unwrap_or_default();
    tree.update::<ComboBox, _>(parts.model, |c, cx| c.set_text(cx, model));
    let effort = setup
        .effort
        .as_deref()
        .and_then(|e| kind.efforts().iter().position(|k| *k == e))
        .map_or(0, |i| i + 1);
    tree.update::<PopUpButton, _>(parts.effort, |p, cx| p.set_selected(cx, effort));
}

/// The lens menu's entries: the built-in lenses, then カスタム.
fn lens_labels() -> Vec<&'static str> {
    BuiltinLens::ALL
        .iter()
        .map(|l| l.label())
        .chain(std::iter::once("カスタム"))
        .collect()
}

fn insert_row(tree: &mut UiTree, list: WidgetId, choice: &ReviewerChoice) -> PanelRow {
    let host = tree.insert(
        Some(list),
        Container::column()
            .gap(6.0)
            .padding(8.0)
            .background(Role::SurfaceAlt)
            .radius(6.0),
    );
    let top = tree.insert(
        Some(host),
        Container::row().align_items(AlignItems::CENTER).gap(6.0),
    );
    let index = AgentKind::ALL
        .iter()
        .position(|&k| k == choice.agent.kind)
        .unwrap_or(0);
    let kind = tree.insert(
        Some(top),
        PopUpButton::new(AgentKind::ALL.iter().map(|k| k.command())).selected_index(index),
    );
    let setup = insert_setup(tree, top, choice.agent.kind);
    set_setup(tree, &setup, choice.agent.kind, &choice.agent.setup);
    spacer(tree, top);
    let remove = tree.insert(
        Some(top),
        Button::new("削除").control_size(ControlSize::Small),
    );
    let bottom = tree.insert(
        Some(host),
        Container::row().align_items(AlignItems::CENTER).gap(6.0),
    );
    caption(tree, bottom, "観点");
    let (lens_index, custom_text) = match &choice.lens {
        LensChoice::Builtin(lens) => (
            BuiltinLens::ALL.iter().position(|l| l == lens).unwrap_or(0),
            String::new(),
        ),
        LensChoice::Custom(text) => (BuiltinLens::ALL.len(), text.clone()),
    };
    let lens = tree.insert(
        Some(bottom),
        PopUpButton::new(lens_labels()).selected_index(lens_index),
    );
    let custom = tree.insert(
        Some(bottom),
        TextField::new("観点を自由に記述（例: 国際化と翻訳漏れ）", 380.0),
    );
    tree.update::<TextField, _>(custom, |f, cx| f.set_text(cx, custom_text));
    tree.set_hidden(custom, lens_index < BuiltinLens::ALL.len());
    PanelRow {
        host,
        kind,
        setup,
        lens,
        custom,
        remove,
    }
}

fn sync_row_buttons(tree: &mut UiTree, parts: &mut Parts) {
    let removable = parts.panel.len() > MIN_PANEL;
    for row in &parts.panel {
        tree.set_enabled(row.remove, removable);
    }
    tree.set_enabled(parts.add_row, parts.panel.len() < MAX_PANEL);
}

/// Adds a row of the last row's kind on the first lens nobody holds yet.
pub(super) fn add_row(tree: &mut UiTree, parts: &mut Parts) {
    if parts.panel.len() >= MAX_PANEL {
        return;
    }
    let kind = parts
        .panel
        .last()
        .map_or(AgentKind::ALL[0], |row| selected_kind(tree, row.kind));
    let held: Vec<usize> = parts
        .panel
        .iter()
        .filter_map(|row| tree.get::<PopUpButton>(row.lens).map(PopUpButton::selected))
        .collect();
    let lens = BuiltinLens::ALL
        .iter()
        .enumerate()
        .find(|(i, _)| !held.contains(i))
        .map_or(BuiltinLens::Correctness, |(_, l)| *l);
    let row = insert_row(
        tree,
        parts.panel_list,
        &ReviewerChoice {
            agent: kind.into(),
            lens: LensChoice::Builtin(lens),
        },
    );
    parts.panel.push(row);
    sync_row_buttons(tree, parts);
}

pub(super) fn remove_row(tree: &mut UiTree, parts: &mut Parts, index: usize, text: &mut TextStore) {
    if parts.panel.len() <= MIN_PANEL || index >= parts.panel.len() {
        return;
    }
    let row = parts.panel.remove(index);
    tree.remove(row.host, text);
    sync_row_buttons(tree, parts);
}

/// Handles a pop-up change in a reviewer row (its kind, or its lens);
/// `false` when `source` is in no row.
pub(super) fn row_changed(
    tree: &mut UiTree,
    parts: &mut Parts,
    source: WidgetId,
    text: &mut TextStore,
) -> bool {
    if let Some(row) = parts.panel.iter().find(|row| row.lens == source) {
        let custom = tree
            .get::<PopUpButton>(row.lens)
            .is_some_and(|p| p.selected() >= BuiltinLens::ALL.len());
        tree.set_hidden(row.custom, !custom);
        return true;
    }
    let Some(row) = parts.panel.iter_mut().find(|row| row.kind == source) else {
        return false;
    };
    let kind = selected_kind(tree, row.kind);
    clear_children(tree, row.setup.host, text);
    let (model, effort) = insert_setup_controls(tree, row.setup.host, kind);
    row.setup.model = model;
    row.setup.effort = effort;
    true
}

pub(super) fn lead_kind_changed(tree: &mut UiTree, parts: &mut Parts, text: &mut TextStore) {
    let kind = selected_kind(tree, parts.lead);
    clear_children(tree, parts.lead_setup.host, text);
    let (model, effort) = insert_setup_controls(tree, parts.lead_setup.host, kind);
    parts.lead_setup.model = model;
    parts.lead_setup.effort = effort;
}

/// Puts `form` into the start view.
pub(super) fn fill(tree: &mut UiTree, parts: &mut Parts, form: &Form, text: &mut TextStore) {
    let kind = TargetKind::ALL
        .iter()
        .position(|k| *k == form.kind)
        .unwrap_or(0);
    tree.update::<SegmentedControl, _>(parts.kind, |s, cx| s.set_selected(cx, &[kind]));
    show_kind(tree, parts, form.kind);
    for (id, value) in [
        (parts.repo, &form.repo),
        (parts.base, &form.base),
        (parts.head, &form.head),
        (parts.filter, &form.filter),
        (parts.root_dir, &form.root),
        (parts.document_file, &form.document_file),
        (parts.document_workdir, &form.document_workdir),
    ] {
        let value = value.clone();
        tree.update::<TextField, _>(id, |f, cx| f.set_text(cx, value));
    }
    for (id, value) in [
        (parts.paths, &form.paths),
        (parts.text, &form.text),
        (parts.request, &form.request),
    ] {
        let value = value.clone();
        tree.update::<TextArea, _>(id, |a, cx| a.set_text(cx, value));
    }
    let rounds = f64::from(form.max_rounds);
    tree.update::<NumberInput, _>(parts.max_rounds, |n, cx| n.set_value(cx, rounds));
    let lead = AgentKind::ALL
        .iter()
        .position(|&k| k == form.lead.kind)
        .unwrap_or(0);
    tree.update::<PopUpButton, _>(parts.lead, |p, cx| p.set_selected(cx, lead));
    lead_kind_changed(tree, parts, text);
    set_setup(tree, &parts.lead_setup, form.lead.kind, &form.lead.setup);
    for row in std::mem::take(&mut parts.panel) {
        tree.remove(row.host, text);
    }
    parts.panel = form
        .panel
        .iter()
        .take(MAX_PANEL)
        .map(|choice| insert_row(tree, parts.panel_list, choice))
        .collect();
    while parts.panel.len() < MIN_PANEL {
        let row = insert_row(
            tree,
            parts.panel_list,
            &Form::default().panel[parts.panel.len()],
        );
        parts.panel.push(row);
    }
    sync_row_buttons(tree, parts);
}

pub(super) fn actions(parts: &Parts, actions: &mut Vec<(WidgetId, Action)>) {
    actions.push((parts.start_button, Action::Start));
    actions.push((parts.add_row, Action::AddPanelRow));
    for (index, row) in parts.panel.iter().enumerate() {
        actions.push((row.remove, Action::RemovePanelRow(index)));
    }
}

fn selected_kind(tree: &UiTree, popup: WidgetId) -> AgentKind {
    tree.get::<PopUpButton>(popup)
        .map_or(AgentKind::ALL[0], |p| AgentKind::ALL[p.selected()])
}

fn read_setup(tree: &UiTree, parts: &SetupParts, kind: AgentKind) -> AgentSetup {
    let model = tree
        .get::<ComboBox>(parts.model)
        .map(|c| c.text().trim().to_owned())
        .filter(|m| !m.is_empty());
    let effort = tree
        .get::<PopUpButton>(parts.effort)
        .and_then(|p| p.selected().checked_sub(1))
        .and_then(|i| kind.efforts().get(i))
        .map(|e| (*e).to_owned());
    AgentSetup { model, effort }
}

pub(super) fn rebuild_note(tree: &mut UiTree, parts: &Parts, link: &Link, text: &mut TextStore) {
    clear_children(tree, parts.note, text);
    let note = match link {
        Link::Live => return,
        Link::Connecting => "Noa に接続しています…".to_owned(),
        Link::Down(why) => format!("Noa に接続できません: {why}"),
    };
    super::note(tree, parts.note, note);
}

/// Shows why the last start was refused under the form.
pub(super) fn show_refusal(tree: &mut UiTree, parts: &Parts, why: &str, text: &mut TextStore) {
    clear_children(tree, parts.note, text);
    let row = tree.insert(
        Some(parts.note),
        Container::row().align_items(AlignItems::START).gap(6.0),
    );
    tag(tree, row, "開始できません", Tone::Critical);
    tree.insert(
        Some(row),
        Label::new(why.to_owned()).font_size(11.5).wrap(true),
    );
}

pub(super) fn read(tree: &UiTree, parts: &Parts) -> Form {
    let field = |id: WidgetId| {
        tree.get::<TextField>(id)
            .map(|f| f.text().trim().to_owned())
            .unwrap_or_default()
    };
    let area = |id: WidgetId| {
        tree.get::<TextArea>(id)
            .map(|a| a.text().to_owned())
            .unwrap_or_default()
    };
    let max_rounds = tree
        .get::<NumberInput>(parts.max_rounds)
        .map_or(DEFAULT_MAX_ROUNDS, |n| {
            n.value().round().clamp(1.0, f64::from(MAX_MAX_ROUNDS)) as RoundNo
        });
    let kind = selected_kind(tree, parts.lead);
    let lead = AgentChoice {
        kind,
        setup: read_setup(tree, &parts.lead_setup, kind),
    };
    let panel = parts
        .panel
        .iter()
        .map(|row| {
            let kind = selected_kind(tree, row.kind);
            let lens_index = tree
                .get::<PopUpButton>(row.lens)
                .map_or(0, PopUpButton::selected);
            let lens = match BuiltinLens::ALL.get(lens_index) {
                Some(lens) => LensChoice::Builtin(*lens),
                None => LensChoice::Custom(field(row.custom)),
            };
            ReviewerChoice {
                agent: AgentChoice {
                    kind,
                    setup: read_setup(tree, &row.setup, kind),
                },
                lens,
            }
        })
        .collect();
    Form {
        kind: selected_target(tree, parts),
        repo: field(parts.repo),
        base: field(parts.base),
        head: field(parts.head),
        filter: field(parts.filter),
        root: field(parts.root_dir),
        paths: area(parts.paths),
        text: area(parts.text),
        document_file: field(parts.document_file),
        document_workdir: field(parts.document_workdir),
        request: area(parts.request).trim().to_owned(),
        max_rounds,
        lead,
        panel,
    }
}

/// Rebuilds the list of past reviews, newest first: open read-only
/// (finished, or running in another window), or resume (unfinished).
pub(super) fn rebuild_past(
    tree: &mut UiTree,
    parts: &Parts,
    past: &[Listed],
    now: Millis,
    actions: &mut Vec<(WidgetId, Action)>,
    text: &mut TextStore,
) {
    clear_children(tree, parts.past, text);
    tree.insert(
        Some(parts.past),
        Label::new("過去のレビュー").font_size(15.0).weight(600),
    );
    if past.is_empty() {
        super::note(tree, parts.past, "まだレビューはありません");
        return;
    }
    let small = |label: &str| Button::new(label).control_size(ControlSize::Small);
    for listed in past {
        let row = tree.insert(
            Some(parts.past),
            Container::row().align_items(AlignItems::CENTER).gap(10.0),
        );
        let info = tree.insert(
            Some(row),
            Container::new(Style {
                flex_direction: FlexDirection::Column,
                flex_grow: 1.0,
                flex_shrink: 1.0,
                min_size: LayoutSize {
                    width: length(0.0),
                    height: auto(),
                },
                gap: gap(3.0),
                ..Default::default()
            }),
        );
        let summary = match &listed.summary {
            Ok(summary) => summary,
            Err(why) => {
                tree.insert(
                    Some(info),
                    Label::new(dir_name(&listed.dir))
                        .font_size(12.5)
                        .weight(600),
                );
                super::note(tree, info, format!("読めません: {why}"));
                continue;
            }
        };
        tree.insert(
            Some(info),
            Label::new(summary.label.clone())
                .font_size(12.5)
                .weight(600)
                .wrap(true),
        );
        let tags = tree.insert(
            Some(info),
            Container::wrapping_row(6.0, 4.0).align_items(AlignItems::CENTER),
        );
        let (status, tone) = match (summary.closed, summary.busy) {
            (true, _) => ("完了", Tone::Neutral),
            (false, true) => ("他のウィンドウで実行中", Tone::Success),
            (false, false) => ("中断", Tone::Warning),
        };
        tag(tree, tags, status, tone);
        tag(tree, tags, summary.kind.label(), Tone::Neutral);
        if let Some(decision) = summary.decision {
            tag(tree, tags, decision.label(), super::decision_tone(decision));
        }
        let counts: Vec<String> = Severity::ALL
            .iter()
            .zip(summary.severities)
            .filter(|(_, n)| *n > 0)
            .map(|(s, n)| format!("{} {n}", s.label()))
            .collect();
        if !counts.is_empty() {
            caption(tree, tags, counts.join(" · "));
        }
        caption(
            tree,
            tags,
            format!("{}/{} ラウンド", summary.rounds, summary.max_rounds),
        );
        caption(tree, tags, age(now, summary.created_at));

        let buttons = tree.insert(Some(row), Container::row().gap(6.0));
        let dir: PathBuf = listed.dir.clone();
        let open = if !summary.closed && !summary.busy {
            let resume = tree.insert(Some(buttons), small("再開").default_button(true));
            (resume, Action::ResumePast(dir.clone()))
        } else {
            (
                tree.insert(Some(buttons), small("表示")),
                Action::OpenPast(dir.clone()),
            )
        };
        actions.push(open);
        if !summary.closed && !summary.busy {
            let view = tree.insert(Some(buttons), small("表示"));
            actions.push((view, Action::OpenPast(dir)));
        }
    }
}

fn dir_name(dir: &Path) -> String {
    dir.file_name().map_or_else(
        || dir.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    )
}

/// How long ago `at` was: `たった今`, `5分前`, `3時間前`, `2日前`.
pub(super) fn age(now: Millis, at: Millis) -> String {
    let minutes = now.saturating_sub(at) / 60_000;
    match minutes {
        0 => "たった今".to_owned(),
        1..60 => format!("{minutes}分前"),
        60..1440 => format!("{}時間前", minutes / 60),
        _ => format!("{}日前", minutes / 1440),
    }
}

/// `typed` with a leading `~` expanded; it must be absolute.
fn expand(typed: &str) -> Result<PathBuf, String> {
    let typed = typed.trim();
    let path = match typed.strip_prefix('~') {
        Some(rest) if rest.is_empty() || rest.starts_with('/') => {
            let Some(home) = std::env::var_os("HOME") else {
                return Err("ホームディレクトリが分かりません".into());
            };
            PathBuf::from(home).join(rest.trim_start_matches('/'))
        }
        _ => PathBuf::from(typed),
    };
    if !path.is_absolute() {
        return Err(format!("{typed} は絶対パスではありません"));
    }
    Ok(path)
}

/// A typed directory, which must exist.
fn dir_path(typed: &str, what: &str) -> Result<PathBuf, String> {
    if typed.trim().is_empty() {
        return Err(format!("{what}を指定してください"));
    }
    let path = expand(typed)?;
    if !path.is_dir() {
        return Err(format!("{} はフォルダではありません", path.display()));
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_past_review_shows_its_age() {
        let now = 10 * 86_400_000;
        assert_eq!(age(now, now - 30_000), "たった今");
        assert_eq!(age(now, now - 5 * 60_000), "5分前");
        assert_eq!(age(now, now - 3 * 3_600_000), "3時間前");
        assert_eq!(age(now, now - 2 * 86_400_000), "2日前");
        assert_eq!(age(now, now + 1), "たった今");
    }

    #[test]
    fn the_form_turns_into_each_kind_of_target() {
        let dir = std::env::temp_dir();
        let dir_text = dir.display().to_string();
        let git = Form {
            repo: dir_text.clone(),
            head: "feat/x".into(),
            filter: "src/  docs/".into(),
            ..Form::default()
        };
        let target = git.target().unwrap();
        assert_eq!(target.kind, TargetKind::Git);
        assert_eq!(target.head, Head::Ref("feat/x".into()));
        assert_eq!(target.base, None);
        assert_eq!(target.paths, ["src/", "docs/"]);
        assert!(
            Form::default()
                .target()
                .unwrap_err()
                .contains("リポジトリを指定")
        );

        let files = Form {
            kind: TargetKind::Files,
            root: dir_text.clone(),
            paths: "a.rs\n\n b/ \n".into(),
            ..Form::default()
        };
        assert_eq!(files.target().unwrap().paths, ["a.rs", "b/"]);

        let doc = Form {
            kind: TargetKind::Document,
            text: "  plan  ".into(),
            document_workdir: dir_text,
            ..Form::default()
        };
        let target = doc.target().unwrap();
        assert_eq!(target.text.as_deref(), Some("plan"));
        assert_eq!(target.workdir.as_deref(), Some(dir.as_path()));
        let empty = Form {
            kind: TargetKind::Document,
            ..Form::default()
        };
        assert!(empty.target().is_err());
        let relative = Form {
            kind: TargetKind::Document,
            document_file: "plan.md".into(),
            ..Form::default()
        };
        assert!(relative.target().unwrap_err().contains("絶対パス"));
    }
}
