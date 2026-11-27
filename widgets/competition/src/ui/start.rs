//! The start view: competition request, optional workdir, the judge panel
//! (1–3 rows of agent, model and effort; the first is the lead), round
//! limit, peer-reveal interval (default every 5 rounds), and the contestant
//! panel (rows of agent, model, and effort; a kind may enter more than once),
//! above the list of past competitions to open, resume, or rematch.

use std::path::{Path, PathBuf};

use noa_gpui_core::style::{
    AlignItems, FlexDirection, Overflow, Point as LayoutPoint, Rect as LayoutRect,
    Size as LayoutSize, auto, length, percent,
};
use noa_gpui_core::widgets::{
    Button, ComboBox, Container, ControlSize, Label, NumberInput, PopUpButton, ScrollView,
    TextArea, TextField, Tone,
};
use noa_gpui_core::{Role, Style, UiTree, WidgetId};
use noa_gpui_text::TextStore;

use competition::model::types::{
    AgentChoice, AgentKind, AgentSetup, DEFAULT_MAX_ROUNDS, DEFAULT_REVEAL_INTERVAL, MAX_JUDGES,
    MAX_PANEL, Millis, Participant, RoundNo, Session,
};
use competition::store::Listed;

use super::{Action, caption, clear_children, gap, spacer, tag};
use crate::installed::Installed;
use crate::live::Link;

const MAX_MAX_ROUNDS: RoundNo = 50;
const MAX_REVEAL_INTERVAL: RoundNo = 25;

/// How many past competitions the start view lists.
pub(super) const PAST_LIMIT: usize = 12;

pub(super) struct Parts {
    pub(super) root: WidgetId,
    pub(super) request: WidgetId,
    pub(super) workdir: WidgetId,
    pub(super) max_rounds: WidgetId,
    pub(super) reveal_interval: WidgetId,
    /// Holds the judge's kind menu, so it can be rebuilt for other kinds.
    judge_host: WidgetId,
    pub(super) judge: WidgetId,
    /// The kinds the agent menus offer, in menu order: the menu index
    /// maps to a kind through this list, never through `AgentKind::ALL`.
    kinds: Vec<AgentKind>,
    /// The model and effort of the judge; rebuilt when its kind changes.
    pub(super) judge_setup: SetupParts,
    /// Holds the co-judges' rows, below the lead's.
    co_judge_list: WidgetId,
    /// One row per co-judge (`judge-2`, `judge-3`), in seat order.
    pub(super) co_judges: Vec<PanelRow>,
    pub(super) add_judge: WidgetId,
    /// Holds the panel's rows.
    panel_list: WidgetId,
    /// One row per entry, in panel order.
    pub(super) panel: Vec<PanelRow>,
    pub(super) add_row: WidgetId,
    pub(super) start_button: WidgetId,
    /// Holds the 「自動で割り当て」 control (`ui::auto`), above the judges.
    pub(super) auto_host: WidgetId,
    /// Under the note: what auto-assignment says (the assignment block, a
    /// failure, why it cannot run). Kept apart so the note's rebuild and
    /// this one's leave each other alone.
    pub(super) auto_note: WidgetId,
    note: WidgetId,
    /// Holds the list of past competitions.
    past: WidgetId,
}

impl Parts {
    pub(super) fn kinds(&self) -> &[AgentKind] {
        &self.kinds
    }
}

/// Which agents are installed, as far as the screen knows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Detection {
    /// The check is still running.
    Pending,
    Done(Installed),
}

impl Detection {
    /// Every kind installed: what the demo and stored views assume, so
    /// they stay deterministic.
    pub(super) fn all_installed() -> Self {
        Self::Done(Installed::Found(AgentKind::ALL.to_vec()))
    }

    /// The kinds the agent menus offer: the installed ones, or all of
    /// them while nothing is known (pending, unknown, none installed; a
    /// menu cannot be empty, and the start button is off in the last two
    /// cases it matters for).
    pub(super) fn menu_kinds(&self) -> Vec<AgentKind> {
        match self {
            Self::Done(Installed::Found(kinds)) if !kinds.is_empty() => kinds.clone(),
            _ => AgentKind::ALL.to_vec(),
        }
    }

    /// The kinds the buttons that hand work to an agent offer.
    pub(super) fn available(&self) -> Vec<AgentKind> {
        match self {
            Self::Pending => AgentKind::ALL.to_vec(),
            Self::Done(Installed::Found(kinds)) => kinds.clone(),
            Self::Done(Installed::Unknown(_)) => AgentKind::ALL.to_vec(),
        }
    }

    /// A competition can be started: the check is done and found an
    /// agent (or could not tell, and all are offered).
    pub(super) fn startable(&self) -> bool {
        match self {
            Self::Pending => false,
            Self::Done(Installed::Found(kinds)) => !kinds.is_empty(),
            Self::Done(Installed::Unknown(_)) => true,
        }
    }

    /// Why the start button is off (also what shows in the note), or the
    /// caveat when it is on.
    fn note(&self) -> Option<String> {
        match self {
            Self::Pending => Some("インストール済みのエージェントを確認しています…".to_owned()),
            Self::Done(Installed::Unknown(reason)) => Some(format!(
                "インストール済みエージェントを確認できませんでした（{reason}）。すべて表示しています"
            )),
            Self::Done(Installed::Found(kinds)) if kinds.is_empty() => Some(
                "claude / codex / agy のいずれもログインシェルの PATH に見つかりません".to_owned(),
            ),
            Self::Done(Installed::Found(_)) => None,
        }
    }

    pub(super) fn blocked_reason(&self) -> String {
        self.note().unwrap_or_default()
    }
}

/// "インストールされていないため除外: codex, agy".
pub(super) fn left_out_message(kinds: &[AgentKind]) -> String {
    let names: Vec<&str> = kinds.iter().map(|k| k.command()).collect();
    format!("インストールされていないため除外: {}", names.join(", "))
}

/// One panel entry: its agent kind, model and effort, and its remove button.
pub(super) struct PanelRow {
    /// The row itself: who it is while rows around it come and go.
    pub(super) host: WidgetId,
    pub(super) kind: WidgetId,
    pub(super) setup: SetupParts,
    pub(super) remove: WidgetId,
}

/// What the start form holds; also what a rematch fills it with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Form {
    pub request: String,
    pub workdir: String,
    pub max_rounds: RoundNo,
    pub reveal_interval: RoundNo,
    /// The lead judge (the first row).
    pub judge: AgentChoice,
    /// The judges after the lead, in seat order (at most
    /// `MAX_JUDGES - 1`); empty for a single judge.
    pub co_judges: Vec<AgentChoice>,
    pub panel: Vec<AgentChoice>,
}

impl Default for Form {
    /// The clean form while every kind is on offer.
    fn default() -> Self {
        Self::for_kinds(&AgentKind::ALL)
    }
}

/// The fewest entries a panel has.
const MIN_PANEL: usize = 2;

impl Form {
    /// The clean form for the available `kinds` (not empty): the judge is
    /// the first, and the panel one row per kind, padded to the minimum
    /// by repeating the first.
    pub(crate) fn for_kinds(kinds: &[AgentKind]) -> Self {
        let first = kinds.first().copied().unwrap_or(AgentKind::ALL[0]);
        let mut panel: Vec<AgentChoice> = kinds.iter().take(MAX_PANEL).map(|&k| k.into()).collect();
        while panel.len() < MIN_PANEL {
            panel.push(first.into());
        }
        Self {
            request: String::new(),
            workdir: String::new(),
            max_rounds: DEFAULT_MAX_ROUNDS,
            reveal_interval: DEFAULT_REVEAL_INTERVAL,
            judge: first.into(),
            co_judges: Vec::new(),
            panel,
        }
    }

    /// The judge panel in seat order, lead first.
    pub(crate) fn judges(&self) -> Vec<AgentChoice> {
        std::iter::once(&self.judge)
            .chain(&self.co_judges)
            .cloned()
            .collect()
    }

    /// This form with only the available `kinds`: panel entries of
    /// another kind are dropped (the panel is padded back to the minimum
    /// with the first kind). Judges of another kind are dropped too: a
    /// dropped lead is replaced by the first co-judge left, or else by the
    /// first kind on its defaults, so one judge always stays. Also the
    /// kinds left out, in order, once each.
    fn restricted(mut self, kinds: &[AgentKind]) -> (Self, Vec<AgentKind>) {
        let mut left_out = Vec::new();
        let mut note = |kind: AgentKind| {
            if !left_out.contains(&kind) {
                left_out.push(kind);
            }
        };
        self.co_judges.truncate(MAX_JUDGES - 1);
        let lead_kept = kinds.contains(&self.judge.kind);
        if !lead_kept {
            note(self.judge.kind);
        }
        self.co_judges.retain(|choice| {
            let keep = kinds.contains(&choice.kind);
            if !keep {
                note(choice.kind);
            }
            keep
        });
        if !lead_kept {
            self.judge = if self.co_judges.is_empty() {
                kinds[0].into()
            } else {
                self.co_judges.remove(0)
            };
        }
        self.panel.retain(|choice| {
            let keep = kinds.contains(&choice.kind);
            if !keep {
                note(choice.kind);
            }
            keep
        });
        while self.panel.len() < MIN_PANEL {
            self.panel.push(kinds[0].into());
        }
        (self, left_out)
    }

    /// `session`'s settings, for a rematch. A session without a round
    /// limit gets the default one (the form always sets a limit).
    pub(crate) fn of_session(session: &Session) -> Self {
        let choice = |p: &Participant| AgentChoice {
            kind: p.kind,
            setup: p.setup.clone(),
        };
        Self {
            request: session.request.clone(),
            workdir: session
                .workdir
                .as_ref()
                .map(|w| w.display().to_string())
                .unwrap_or_default(),
            max_rounds: session
                .max_rounds
                .unwrap_or(DEFAULT_MAX_ROUNDS)
                .clamp(1, MAX_MAX_ROUNDS),
            reveal_interval: session.reveal_interval.min(MAX_REVEAL_INTERVAL),
            judge: choice(&session.judge),
            co_judges: session
                .co_judges
                .iter()
                .take(MAX_JUDGES - 1)
                .map(choice)
                .collect(),
            panel: session
                .contestants
                .iter()
                .take(MAX_PANEL)
                .map(choice)
                .collect(),
        }
    }
}

fn card_container() -> Container {
    Container::new(Style {
        flex_direction: FlexDirection::Column,
        flex_shrink: 0.0,
        gap: gap(12.0),
        padding: LayoutRect::length(crate::appearance::space(24.0)),
        size: LayoutSize {
            width: length(620.0),
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

const PAST_WIDTH: f32 = 380.0;
const PAST_HEIGHT: f32 = 600.0;

/// The card beside the form that lists the past runs; returns the list's
/// column.
fn insert_past_card(tree: &mut UiTree, parent: WidgetId) -> WidgetId {
    let card = tree.insert(
        Some(parent),
        Container::new(Style {
            flex_direction: FlexDirection::Column,
            flex_shrink: 1.0,
            size: LayoutSize {
                width: length(PAST_WIDTH),
                height: length(PAST_HEIGHT),
            },
            min_size: LayoutSize {
                width: length(0.0),
                height: length(0.0),
            },
            max_size: LayoutSize {
                width: percent(1.0),
                height: percent(1.0),
            },
            ..Default::default()
        })
        .background(Role::Surface)
        .border(1.0, Role::Separator)
        .radius(10.0),
    );
    super::heading(tree, card, "過去のコンペ");
    super::scroll_column(tree, card, 12.0)
}

pub(super) fn insert(tree: &mut UiTree, parent: WidgetId) -> Parts {
    // Scrolls as a whole: six panel rows outgrow a short window.
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
    let centered = tree.insert(
        Some(root),
        Container::new(Style {
            flex_direction: FlexDirection::Column,
            align_items: AlignItems::CENTER,
            padding: LayoutRect::length(crate::appearance::space(24.0)),
            size: LayoutSize {
                width: percent(1.0),
                height: auto(),
            },
            min_size: LayoutSize {
                width: auto(),
                height: percent(1.0),
            },
            ..Default::default()
        }),
    );
    let cards = tree.insert(
        Some(centered),
        Container::new(Style {
            flex_direction: FlexDirection::Row,
            align_items: AlignItems::START,
            gap: gap(16.0),
            margin: LayoutRect {
                left: length(0.0),
                right: length(0.0),
                top: auto(),
                bottom: auto(),
            },
            max_size: LayoutSize {
                width: percent(1.0),
                height: auto(),
            },
            ..Default::default()
        }),
    );
    let card = tree.insert(Some(cards), card_container());
    tree.insert(
        Some(card),
        Label::new("新しいコンペティション")
            .font_size(crate::appearance::font(17.0))
            .weight(600),
    );
    tree.insert(
        Some(card),
        Label::new(
            "複数のエージェントが1つの依頼に対して独立して成果物を競作します。\
             通常ラウンドはお互いの成果物を知らずに進み、指定ラウンド毎（既定5ラウンド）に\
             相互閲覧を行って競争力を高めます。デザイン等は比較ビューアーで並べて確認できます。",
        )
        .font_size(crate::appearance::font(12.0))
        .color(Role::Muted)
        .wrap(true),
    );
    caption(tree, card, "依頼内容");
    let request = tree.insert(
        Some(card),
        TextArea::new(
            "例: スペシャルティコーヒー焙煎所のブランドLPをデザイン・実装する（index.html で確認可能にすること）",
        )
        .min_lines(3),
    );
    caption(tree, card, "参照ディレクトリ（任意・リポジトリ）");
    let workdir = tree.insert(
        Some(card),
        TextField::new("空欄: コンペ専用フォルダ。例: ~/repos/my-app", 572.0),
    );
    tree.insert(
        Some(card),
        Label::new(
            "各参加者の成果物は専用フォルダ（.noa/competitions/<id>/entries/<panel>/）に分離出力されます（git には載りません）。",
        )
        .font_size(crate::appearance::font(11.0))
        .color(Role::Muted)
        .wrap(true),
    );

    let row = tree.insert(
        Some(card),
        Container::row()
            .align_items(AlignItems::START)
            .gap(crate::appearance::space(20.0)),
    );
    let rounds_col = tree.insert(
        Some(row),
        Container::column().gap(crate::appearance::space(6.0)),
    );
    caption(tree, rounds_col, "ラウンド上限");
    let max_rounds = tree.insert(
        Some(rounds_col),
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

    let reveal_col = tree.insert(
        Some(row),
        Container::column().gap(crate::appearance::space(6.0)),
    );
    caption(tree, reveal_col, "相互閲覧の間隔");
    let reveal_interval = tree.insert(
        Some(reveal_col),
        NumberInput::new(
            0.0,
            f64::from(MAX_REVEAL_INTERVAL),
            f64::from(DEFAULT_REVEAL_INTERVAL),
        )
        .step(1.0)
        .decimals(0)
        .stepper(true)
        .width(80.0),
    );

    let auto_host = tree.insert(
        Some(card),
        Container::row()
            .align_items(AlignItems::CENTER)
            .gap(crate::appearance::space(8.0)),
    );
    let judge_col = tree.insert(
        Some(card),
        Container::column().gap(crate::appearance::space(6.0)),
    );
    caption(
        tree,
        judge_col,
        format!("審判 (Judge)（1〜{MAX_JUDGES}人）・エージェント・モデル・エフォート"),
    );
    let judge_row = tree.insert(
        Some(judge_col),
        Container::row()
            .align_items(AlignItems::CENTER)
            .gap(crate::appearance::space(6.0)),
    );
    // Nothing is known of what is installed yet: every kind is offered
    // until the check reports ([`apply_kinds`]).
    let kinds = AgentKind::ALL.to_vec();
    let judge_host = tree.insert(Some(judge_row), Container::row());
    let judge = insert_kind_menu(tree, judge_host, &kinds, kinds[0]);
    let judge_setup = insert_setup(tree, judge_row, kinds[0]);
    let co_judge_list = tree.insert(
        Some(judge_col),
        Container::column().gap(crate::appearance::space(6.0)),
    );
    let judge_foot = tree.insert(
        Some(judge_col),
        Container::row()
            .align_items(AlignItems::CENTER)
            .gap(crate::appearance::space(8.0)),
    );
    let add_judge = tree.insert(
        Some(judge_foot),
        Button::new("＋ 審判を追加").control_size(ControlSize::Small),
    );
    tree.insert(
        Some(judge_col),
        Label::new(
            "1人目がリード審判です。早期終了と最終講評はリード審判が決め、\
             ほかの審判は採点だけを行います。審判は毎ラウンド全員が採点し、\
             各参加者のスコアは審判の平均点になります",
        )
        .font_size(crate::appearance::font(11.0))
        .color(Role::Muted)
        .wrap(true),
    );

    let panel_col = tree.insert(
        Some(card),
        Container::column().gap(crate::appearance::space(6.0)),
    );
    caption(
        tree,
        panel_col,
        format!("参加パネル（2〜{MAX_PANEL}体）・エージェント・モデル・エフォート"),
    );
    let panel_list = tree.insert(
        Some(panel_col),
        Container::column().gap(crate::appearance::space(6.0)),
    );
    let panel = Form::for_kinds(&kinds)
        .panel
        .iter()
        .map(|choice| insert_row(tree, panel_list, &kinds, choice))
        .collect();
    let panel_foot = tree.insert(
        Some(panel_col),
        Container::row()
            .align_items(AlignItems::CENTER)
            .gap(crate::appearance::space(8.0)),
    );
    let add_row = tree.insert(
        Some(panel_foot),
        Button::new("＋ 参加者を追加").control_size(ControlSize::Small),
    );
    tree.insert(
        Some(panel_foot),
        Label::new("同じエージェントの2体目以降は claude-2, claude-3 … として参加します")
            .font_size(crate::appearance::font(11.0))
            .color(Role::Muted),
    );

    let foot = tree.insert(
        Some(card),
        Container::row()
            .align_items(AlignItems::CENTER)
            .gap(crate::appearance::space(8.0)),
    );
    let notes = tree.insert(
        Some(foot),
        Container::column()
            .grow(1.0)
            .gap(crate::appearance::space(4.0)),
    );
    let note = tree.insert(Some(notes), Container::column());
    let auto_note = tree.insert(
        Some(notes),
        Container::column().gap(crate::appearance::space(2.0)),
    );
    let start_button = tree.insert(
        Some(foot),
        Button::new("コンペティションを開始").default_button(true),
    );
    let past = insert_past_card(tree, cards);
    let mut parts = Parts {
        root,
        request,
        workdir,
        max_rounds,
        reveal_interval,
        judge_host,
        judge,
        kinds,
        judge_setup,
        co_judge_list,
        co_judges: Vec::new(),
        add_judge,
        panel_list,
        panel,
        add_row,
        start_button,
        auto_host,
        auto_note,
        note,
        past,
    };
    sync_row_buttons(tree, &mut parts);
    parts
}

/// The model box and effort menu of one agent.
pub(super) struct SetupParts {
    /// Holds the two controls, so they can be rebuilt for another kind.
    host: WidgetId,
    pub(super) model: WidgetId,
    pub(super) effort: WidgetId,
}

const DEFAULT_MODEL: &str = "既定のモデル";
const DEFAULT_EFFORT: &str = "既定のエフォート";

/// A model box suggesting `kind`'s models (any name may be typed; empty
/// is the CLI's default) and an effort menu of `kind`'s levels.
fn insert_setup(tree: &mut UiTree, parent: WidgetId, kind: AgentKind) -> SetupParts {
    let host = tree.insert(
        Some(parent),
        Container::row()
            .align_items(AlignItems::CENTER)
            .gap(crate::appearance::space(6.0)),
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

/// Shows `setup` in the model box and effort menu of `kind`.
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

/// A menu of `kinds`, `selected` picked (the first when it is not one).
fn insert_kind_menu(
    tree: &mut UiTree,
    host: WidgetId,
    kinds: &[AgentKind],
    selected: AgentKind,
) -> WidgetId {
    tree.insert(
        Some(host),
        PopUpButton::new(kinds.iter().map(|k| k.command()))
            .selected_index(kind_index(kinds, selected)),
    )
}

/// Where `kind` is in the menu of `kinds`; the first when it is not.
fn kind_index(kinds: &[AgentKind], kind: AgentKind) -> usize {
    kinds.iter().position(|&k| k == kind).unwrap_or(0)
}

/// One panel row showing `choice`: kind menu, model, effort, remove.
fn insert_row(
    tree: &mut UiTree,
    list: WidgetId,
    kinds: &[AgentKind],
    choice: &AgentChoice,
) -> PanelRow {
    let host = tree.insert(
        Some(list),
        Container::row()
            .align_items(AlignItems::CENTER)
            .gap(crate::appearance::space(6.0)),
    );
    let kind = insert_kind_menu(tree, host, kinds, choice.kind);
    let setup = insert_setup(tree, host, choice.kind);
    set_setup(tree, &setup, choice.kind, &choice.setup);
    spacer(tree, host);
    let remove = tree.insert(
        Some(host),
        Button::new("削除").control_size(ControlSize::Small),
    );
    PanelRow {
        host,
        kind,
        setup,
        remove,
    }
}

/// Adding stops at [`MAX_PANEL`] rows, removing at two; judge rows stop
/// at [`MAX_JUDGES`] (every co-judge row can be removed).
fn sync_row_buttons(tree: &mut UiTree, parts: &mut Parts) {
    let removable = parts.panel.len() > 2;
    for row in &parts.panel {
        tree.set_enabled(row.remove, removable);
    }
    tree.set_enabled(parts.add_row, parts.panel.len() < MAX_PANEL);
    tree.set_enabled(parts.add_judge, 1 + parts.co_judges.len() < MAX_JUDGES);
}

/// Adds a judge row of the last judge's kind (on its default model and
/// effort), up to [`MAX_JUDGES`] judges.
pub(super) fn add_judge_row(tree: &mut UiTree, parts: &mut Parts) {
    if 1 + parts.co_judges.len() >= MAX_JUDGES {
        return;
    }
    let last = parts.co_judges.last().map_or(parts.judge, |row| row.kind);
    let kind = selected_kind(tree, last, &parts.kinds);
    let row = insert_row(tree, parts.co_judge_list, &parts.kinds, &kind.into());
    parts.co_judges.push(row);
    sync_row_buttons(tree, parts);
}

/// Removes the co-judge row `index` (0 is `judge-2`); the lead's row
/// has no remove button.
pub(super) fn remove_judge_row(
    tree: &mut UiTree,
    parts: &mut Parts,
    index: usize,
    text: &mut TextStore,
) {
    if index >= parts.co_judges.len() {
        return;
    }
    let row = parts.co_judges.remove(index);
    tree.remove(row.host, text);
    sync_row_buttons(tree, parts);
}

/// Adds a row of the last row's kind (on its default model and effort),
/// so a second claude is one click away.
pub(super) fn add_row(tree: &mut UiTree, parts: &mut Parts) {
    if parts.panel.len() >= MAX_PANEL {
        return;
    }
    let kind = parts.panel.last().map_or(parts.kinds[0], |row| {
        selected_kind(tree, row.kind, &parts.kinds)
    });
    let row = insert_row(tree, parts.panel_list, &parts.kinds, &kind.into());
    parts.panel.push(row);
    sync_row_buttons(tree, parts);
}

pub(super) fn remove_row(tree: &mut UiTree, parts: &mut Parts, index: usize, text: &mut TextStore) {
    if parts.panel.len() <= 2 || index >= parts.panel.len() {
        return;
    }
    let row = parts.panel.remove(index);
    tree.remove(row.host, text);
    sync_row_buttons(tree, parts);
}

/// Rebuilds the model and effort controls of the row whose kind menu is
/// `source`; `false` when no row's is.
pub(super) fn row_kind_changed(
    tree: &mut UiTree,
    parts: &mut Parts,
    source: WidgetId,
    text: &mut TextStore,
) -> bool {
    let Some(row) = parts
        .panel
        .iter_mut()
        .chain(&mut parts.co_judges)
        .find(|row| row.kind == source)
    else {
        return false;
    };
    let kind = selected_kind(tree, row.kind, &parts.kinds);
    clear_children(tree, row.setup.host, text);
    let (model, effort) = insert_setup_controls(tree, row.setup.host, kind);
    row.setup.model = model;
    row.setup.effort = effort;
    true
}

/// Puts `form` into the start view: a rematch, or a clean form. Entries
/// of a kind the menus do not offer are left out; those kinds come back.
pub(super) fn fill(
    tree: &mut UiTree,
    parts: &mut Parts,
    form: &Form,
    text: &mut TextStore,
) -> Vec<AgentKind> {
    let request = form.request.clone();
    tree.update::<TextArea, _>(parts.request, |a, cx| a.set_text(cx, request));
    let workdir = form.workdir.clone();
    tree.update::<TextField, _>(parts.workdir, |f, cx| f.set_text(cx, workdir));
    let (rounds, reveal) = (f64::from(form.max_rounds), f64::from(form.reveal_interval));
    tree.update::<NumberInput, _>(parts.max_rounds, |n, cx| n.set_value(cx, rounds));
    tree.update::<NumberInput, _>(parts.reveal_interval, |n, cx| n.set_value(cx, reveal));
    let (form, left_out) = form.clone().restricted(&parts.kinds);
    let judge = kind_index(&parts.kinds, form.judge.kind);
    tree.update::<PopUpButton, _>(parts.judge, |p, cx| p.set_selected(cx, judge));
    judge_kind_changed(tree, parts, text);
    set_setup(tree, &parts.judge_setup, form.judge.kind, &form.judge.setup);
    for row in std::mem::take(&mut parts.co_judges) {
        tree.remove(row.host, text);
    }
    parts.co_judges = form
        .co_judges
        .iter()
        .map(|choice| insert_row(tree, parts.co_judge_list, &parts.kinds, choice))
        .collect();
    for row in std::mem::take(&mut parts.panel) {
        tree.remove(row.host, text);
    }
    parts.panel = form
        .panel
        .iter()
        .take(MAX_PANEL)
        .map(|choice| insert_row(tree, parts.panel_list, &parts.kinds, choice))
        .collect();
    while parts.panel.len() < MIN_PANEL {
        let row = insert_row(tree, parts.panel_list, &parts.kinds, &parts.kinds[0].into());
        parts.panel.push(row);
    }
    sync_row_buttons(tree, parts);
    left_out
}

/// Shows `judges` (lead first) and `panel` in the agent rows already there,
/// in place: each row keeps its identity ([`PanelRow::host`]) and its
/// remove button. Counts beyond the rows' are ignored; the caller fills a
/// form of the same shape (auto-assignment fills only the rows it found).
pub(super) fn show_agents(
    tree: &mut UiTree,
    parts: &mut Parts,
    judges: &[AgentChoice],
    panel: &[AgentChoice],
    text: &mut TextStore,
) {
    let kinds = parts.kinds.clone();
    if let Some(lead) = judges.first() {
        show_choice(
            tree,
            &kinds,
            parts.judge,
            &mut parts.judge_setup,
            lead,
            text,
        );
    }
    for (row, choice) in parts.co_judges.iter_mut().zip(judges.iter().skip(1)) {
        show_choice(tree, &kinds, row.kind, &mut row.setup, choice, text);
    }
    for (row, choice) in parts.panel.iter_mut().zip(panel) {
        show_choice(tree, &kinds, row.kind, &mut row.setup, choice, text);
    }
}

/// Picks `choice`'s kind in the menu `popup` and rebuilds the model and
/// effort controls for it, showing its setup.
fn show_choice(
    tree: &mut UiTree,
    kinds: &[AgentKind],
    popup: WidgetId,
    setup: &mut SetupParts,
    choice: &AgentChoice,
    text: &mut TextStore,
) {
    let index = kind_index(kinds, choice.kind);
    tree.update::<PopUpButton, _>(popup, |p, cx| p.set_selected(cx, index));
    clear_children(tree, setup.host, text);
    let (model, effort) = insert_setup_controls(tree, setup.host, choice.kind);
    setup.model = model;
    setup.effort = effort;
    set_setup(tree, setup, choice.kind, &choice.setup);
}

/// Makes the agent menus offer `kinds` (rebuilt: a menu's entries are
/// fixed once made) and shows `form` in them; the kinds it had to leave
/// out come back.
pub(super) fn apply_kinds(
    tree: &mut UiTree,
    parts: &mut Parts,
    kinds: Vec<AgentKind>,
    form: &Form,
    text: &mut TextStore,
) -> Vec<AgentKind> {
    parts.kinds = kinds;
    clear_children(tree, parts.judge_host, text);
    parts.judge = insert_kind_menu(tree, parts.judge_host, &parts.kinds, parts.kinds[0]);
    fill(tree, parts, form, text)
}

/// The start view's buttons, for the screen's action table.
pub(super) fn actions(parts: &Parts, actions: &mut Vec<(WidgetId, Action)>) {
    actions.push((parts.start_button, Action::Start));
    actions.push((parts.add_row, Action::AddPanelRow));
    for (index, row) in parts.panel.iter().enumerate() {
        actions.push((row.remove, Action::RemovePanelRow(index)));
    }
    actions.push((parts.add_judge, Action::AddJudgeRow));
    for (index, row) in parts.co_judges.iter().enumerate() {
        actions.push((row.remove, Action::RemoveJudgeRow(index)));
    }
}

/// Rebuilds the judge's model and effort controls for the kind now
/// picked, when it changed: each kind has its own models and levels.
pub(super) fn judge_kind_changed(tree: &mut UiTree, parts: &mut Parts, text: &mut TextStore) {
    let kind = selected_kind(tree, parts.judge, &parts.kinds);
    clear_children(tree, parts.judge_setup.host, text);
    let (model, effort) = insert_setup_controls(tree, parts.judge_setup.host, kind);
    parts.judge_setup.model = model;
    parts.judge_setup.effort = effort;
}

/// The kind `popup` shows: its index into the menu's `kinds`.
fn selected_kind(tree: &UiTree, popup: WidgetId, kinds: &[AgentKind]) -> AgentKind {
    tree.get::<PopUpButton>(popup)
        .and_then(|p| kinds.get(p.selected()).copied())
        .unwrap_or(kinds[0])
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

pub(super) fn rebuild_note(
    tree: &mut UiTree,
    parts: &Parts,
    link: &Link,
    detection: &Detection,
    text: &mut TextStore,
) {
    clear_children(tree, parts.note, text);
    tree.set_enabled(parts.start_button, detection.startable());
    let notes = [
        match link {
            Link::Live => None,
            Link::Connecting => Some("Noa に接続しています…".to_owned()),
            Link::Down(why) => Some(format!("Noa に接続できません: {why}")),
        },
        detection.note(),
    ];
    for note in notes.into_iter().flatten() {
        super::note(tree, parts.note, note);
    }
}

pub(super) fn read(tree: &UiTree, parts: &Parts) -> Form {
    let request = tree
        .get::<TextArea>(parts.request)
        .map(|a| a.text().trim().to_owned())
        .unwrap_or_default();
    let workdir = tree
        .get::<TextField>(parts.workdir)
        .map(|f| f.text().trim().to_owned())
        .unwrap_or_default();
    let max_rounds = tree
        .get::<NumberInput>(parts.max_rounds)
        .map_or(DEFAULT_MAX_ROUNDS, |n| {
            n.value().round().clamp(1.0, f64::from(MAX_MAX_ROUNDS)) as RoundNo
        });
    let reveal_interval = tree
        .get::<NumberInput>(parts.reveal_interval)
        .map_or(DEFAULT_REVEAL_INTERVAL, |n| {
            n.value().round().clamp(0.0, f64::from(MAX_REVEAL_INTERVAL)) as RoundNo
        });
    let kind = selected_kind(tree, parts.judge, &parts.kinds);
    let judge = AgentChoice {
        kind,
        setup: read_setup(tree, &parts.judge_setup, kind),
    };
    let row_choice = |row: &PanelRow| {
        let kind = selected_kind(tree, row.kind, &parts.kinds);
        AgentChoice {
            kind,
            setup: read_setup(tree, &row.setup, kind),
        }
    };
    Form {
        request,
        workdir,
        max_rounds,
        reveal_interval,
        judge,
        co_judges: parts.co_judges.iter().map(row_choice).collect(),
        panel: parts.panel.iter().map(row_choice).collect(),
    }
}

/// Rebuilds the list of past competitions, newest first, each with what
/// can be done with it: open read-only (finished, or running in another
/// window), resume (unfinished), rematch.
pub(super) fn rebuild_past(
    tree: &mut UiTree,
    parts: &Parts,
    past: &[Listed],
    now: Millis,
    actions: &mut Vec<(WidgetId, Action)>,
    text: &mut TextStore,
) {
    clear_children(tree, parts.past, text);
    if past.is_empty() {
        super::note(tree, parts.past, "まだコンペはありません");
        return;
    }
    let small = |label: &str| Button::new(label).control_size(ControlSize::Small);
    for listed in past {
        let row = tree.insert(
            Some(parts.past),
            Container::row()
                .align_items(AlignItems::CENTER)
                .gap(crate::appearance::space(10.0)),
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
                        .font_size(crate::appearance::font(12.5))
                        .weight(600),
                );
                super::note(tree, info, format!("読めません: {why}"));
                continue;
            }
        };
        let tags = tree.insert(
            Some(info),
            Container::wrapping_row(6.0, 4.0).align_items(AlignItems::CENTER),
        );
        let (status, tone) = match (summary.closed, summary.busy, summary.maybe_running) {
            (true, _, _) => ("終了", Tone::Neutral),
            (false, true, _) => ("実行中", Tone::Success),
            (false, false, true) => ("旧版で実行中の可能性", Tone::Warning),
            (false, false, false) => ("中断", Tone::Warning),
        };
        tag(tree, tags, status, tone);
        caption(tree, tags, age(now, summary.created_at));
        caption(tree, tags, format!("{}ラウンド", summary.rounds));
        if let Some(winner) = &summary.winner {
            tag(tree, tags, format!("🏆 {winner}"), Tone::Success);
        }
        tree.insert(
            Some(info),
            Label::new(excerpt(&summary.request))
                .font_size(crate::appearance::font(12.5))
                .weight(600)
                .wrap(true),
        );
        super::note(tree, info, summary.contestants.join(" · "));

        let buttons = tree.insert(
            Some(row),
            Container::row().gap(crate::appearance::space(6.0)),
        );
        let dir: PathBuf = listed.dir.clone();
        let open = if !summary.closed && !summary.busy && !summary.maybe_running {
            let resume = tree.insert(Some(buttons), small("再開").default_button(true));
            (resume, Action::ResumePast(dir.clone()))
        } else {
            let label = if summary.closed { "開く" } else { "閲覧" };
            (
                tree.insert(Some(buttons), small(label)),
                Action::OpenPast(dir.clone()),
            )
        };
        actions.push(open);
        // A competition that ran all its rounds goes on with more.
        if summary.closed && summary.extendable && !summary.busy && !summary.maybe_running {
            for by in super::EXTEND_BY {
                let more = tree.insert(Some(buttons), small(&format!("+{by}")));
                tree.set_accessible_label(more, format!("ラウンドを {by} 追加して再開"));
                actions.push((more, Action::ExtendPast(dir.clone(), by)));
            }
        }
        let rematch = tree.insert(Some(buttons), small("リマッチ"));
        actions.push((rematch, Action::RematchPast(dir)));
    }
}

fn dir_name(dir: &Path) -> String {
    dir.file_name().map_or_else(
        || dir.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    )
}

const EXCERPT_CHARS: usize = 60;

/// The request's first line, cut to [`EXCERPT_CHARS`].
fn excerpt(request: &str) -> String {
    let line = request
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or_default();
    match line.char_indices().nth(EXCERPT_CHARS) {
        Some((cut, _)) => format!("{}…", &line[..cut]),
        None => line.to_owned(),
    }
}

/// How long ago `at` was: `たった今`, `5分前`, `3時間前`, `2日前`.
fn age(now: Millis, at: Millis) -> String {
    let minutes = now.saturating_sub(at) / 60_000;
    match minutes {
        0 => "たった今".to_owned(),
        1..60 => format!("{minutes}分前"),
        60..1440 => format!("{}時間前", minutes / 60),
        _ => format!("{}日前", minutes / 1440),
    }
}

pub(super) fn workdir(typed: &str) -> Result<Option<PathBuf>, String> {
    if typed.is_empty() {
        return Ok(None);
    }
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
    if !path.is_dir() {
        return Err(format!("{} はディレクトリではありません", path.display()));
    }
    Ok(Some(path))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_clean_form_has_a_row_per_available_kind_and_at_least_two() {
        let kinds = |form: &Form| form.panel.iter().map(|c| c.kind).collect::<Vec<_>>();
        assert_eq!(kinds(&Form::default()), AgentKind::ALL);
        let form = Form::for_kinds(&[AgentKind::Codex, AgentKind::Agy]);
        assert_eq!(kinds(&form), [AgentKind::Codex, AgentKind::Agy]);
        assert_eq!(form.judge.kind, AgentKind::Codex);
        let form = Form::for_kinds(&[AgentKind::Agy]);
        assert_eq!(kinds(&form), [AgentKind::Agy, AgentKind::Agy]);
        assert_eq!(form.judge.kind, AgentKind::Agy);
    }

    #[test]
    fn a_form_restricted_to_installed_kinds_says_what_it_left_out() {
        let form = Form {
            judge: AgentKind::Claude.into(),
            panel: vec![
                AgentKind::Codex.into(),
                AgentKind::Claude.into(),
                AgentKind::Codex.into(),
            ],
            ..Form::default()
        };
        let (form, left_out) = form.restricted(&[AgentKind::Codex]);
        assert_eq!(left_out, [AgentKind::Claude]);
        assert_eq!(form.judge.kind, AgentKind::Codex);
        assert_eq!(form.panel.len(), 2);
    }

    #[test]
    fn a_past_competition_shows_its_age_and_the_start_of_its_request() {
        let now = 10 * 86_400_000;
        assert_eq!(age(now, now - 30_000), "たった今");
        assert_eq!(age(now, now - 5 * 60_000), "5分前");
        assert_eq!(age(now, now - 3 * 3_600_000), "3時間前");
        assert_eq!(age(now, now - 2 * 86_400_000), "2日前");
        assert_eq!(age(now, now + 1), "たった今");
        assert_eq!(excerpt("\n  LP を作る\n詳細"), "LP を作る");
        let long = "あ".repeat(EXCERPT_CHARS + 5);
        assert_eq!(excerpt(&long).chars().count(), EXCERPT_CHARS + 1);
        assert!(excerpt(&long).ends_with('…'));
    }

    fn screen() -> (UiTree, Parts, TextStore) {
        let mut tree = UiTree::new();
        let root = tree.insert(None, Container::column());
        let parts = insert(&mut tree, root);
        (tree, parts, TextStore::new(Default::default()))
    }

    #[test]
    fn judge_rows_take_one_to_three() {
        let (mut tree, mut parts, mut text) = screen();
        // One judge by default, and room for two more.
        assert!(read(&tree, &parts).co_judges.is_empty());
        assert!(tree.is_enabled(parts.add_judge));
        for _ in 0..4 {
            add_judge_row(&mut tree, &mut parts);
        }
        assert_eq!(read(&tree, &parts).judges().len(), MAX_JUDGES);
        assert!(!tree.is_enabled(parts.add_judge));
        // Each co-judge row's controls follow its own kind.
        let row = parts.co_judges[1].kind;
        tree.update::<PopUpButton, _>(row, |p, cx| p.set_selected(cx, 2));
        assert!(row_kind_changed(&mut tree, &mut parts, row, &mut text));
        let models = tree
            .get::<ComboBox>(parts.co_judges[1].setup.model)
            .unwrap()
            .items()
            .to_vec();
        let agy: Vec<String> = AgentKind::Agy
            .models()
            .iter()
            .map(|m| m.to_string())
            .collect();
        assert_eq!(models, agy);
        let effort = parts.co_judges[1].setup.effort;
        tree.update::<PopUpButton, _>(effort, |p, cx| p.set_selected(cx, 1));
        let form = read(&tree, &parts);
        let kinds: Vec<AgentKind> = form.judges().iter().map(|c| c.kind).collect();
        assert_eq!(
            kinds,
            [AgentKind::Claude, AgentKind::Claude, AgentKind::Agy]
        );
        assert_eq!(
            form.co_judges[1].setup.effort.as_deref(),
            AgentKind::Agy.efforts().first().copied()
        );
        // Any row after the first goes; the lead's row stays.
        remove_judge_row(&mut tree, &mut parts, 0, &mut text);
        let kinds: Vec<AgentKind> = read(&tree, &parts)
            .judges()
            .iter()
            .map(|c| c.kind)
            .collect();
        assert_eq!(kinds, [AgentKind::Claude, AgentKind::Agy]);
        assert!(tree.is_enabled(parts.add_judge));
        remove_judge_row(&mut tree, &mut parts, 0, &mut text);
        remove_judge_row(&mut tree, &mut parts, 0, &mut text);
        assert_eq!(read(&tree, &parts).judges().len(), 1);
    }

    #[test]
    fn judges_of_one_kind_are_judge_and_judge_2() {
        let (mut tree, mut parts, mut text) = screen();
        let on = |model: &str| AgentChoice {
            kind: AgentKind::Claude,
            setup: AgentSetup {
                model: Some(model.into()),
                effort: None,
            },
        };
        let form = Form {
            judge: on("opus"),
            co_judges: vec![on("sonnet")],
            panel: vec![AgentKind::Claude.into(), AgentKind::Codex.into()],
            ..Form::default()
        };
        assert!(fill(&mut tree, &mut parts, &form, &mut text).is_empty());
        let read = read(&tree, &parts);
        assert_eq!(read, form);
        let seats = competition::engine::participants(&read.judges(), &read.panel);
        let judges: Vec<(&str, Option<&str>)> = std::iter::once(&seats.judge)
            .chain(&seats.co_judges)
            .map(|p| (p.id.as_str(), p.setup.model.as_deref()))
            .collect();
        assert_eq!(
            judges,
            [("judge", Some("opus")), ("judge-2", Some("sonnet"))]
        );
        let contestants: Vec<&str> = seats.contestants.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(contestants, ["claude", "codex"]);
    }

    #[test]
    fn a_rematch_form_keeps_the_judges_in_seat_order_within_the_limit() {
        let mut session = crate::demo::panel_session();
        let mut extra = session.co_judges[0].clone();
        extra.id = competition::model::types::judge_id(3);
        session.co_judges.push(extra);
        let form = Form::of_session(&session);
        let kinds: Vec<AgentKind> = form.judges().iter().map(|c| c.kind).collect();
        assert_eq!(kinds, [AgentKind::Claude, AgentKind::Codex, AgentKind::Agy]);
        // A dropped co-judge leaves the lead where it is.
        let (form, left_out) = form.restricted(&[AgentKind::Claude, AgentKind::Agy]);
        let kinds: Vec<AgentKind> = form.judges().iter().map(|c| c.kind).collect();
        assert_eq!(kinds, [AgentKind::Claude, AgentKind::Agy]);
        assert_eq!(left_out, [AgentKind::Codex]);
    }
}
