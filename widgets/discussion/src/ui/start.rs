//! The start view, shown until a session exists: the topic, the moderator's
//! agent kind and the panel (rows of agent, model and effort; a kind may
//! take part more than once), beside the stored discussions to open or
//! resume. The agent menus offer only the installed kinds.

use noa_gpui_core::style::{
    AlignItems, FlexDirection, Overflow, Point as LayoutPoint, Rect as LayoutRect,
    Size as LayoutSize, auto, length, percent,
};
use noa_gpui_core::widgets::{
    Button, ComboBox, Container, ControlSize, Label, NumberInput, PopUpButton, ScrollView,
    TextArea, TextField, Tone, Truncation,
};
use noa_gpui_core::{Role, Style, UiTree, WidgetId};
use std::path::PathBuf;

use noa_gpui_text::TextStore;

use discussion::model::types::{AgentChoice, AgentKind, AgentSetup, MAX_PANEL, Millis, RoundNo};
use discussion::store::Summary;

/// The round limit the start view offers first.
pub(crate) const DEFAULT_MAX_ROUNDS: RoundNo = 5;
/// The highest round limit the start view takes.
const MAX_MAX_ROUNDS: RoundNo = 50;

use super::{Action, caption, clear_children, gap, spacer};
use crate::installed::Installed;
use crate::live::Link;

pub(super) struct Parts {
    pub(super) root: WidgetId,
    pub(super) topic: WidgetId,
    pub(super) workdir: WidgetId,
    pub(super) max_rounds: WidgetId,
    /// Holds the moderator's kind menu, so it can be rebuilt.
    moderator_host: WidgetId,
    pub(super) moderator: WidgetId,
    /// The model and effort of the moderator; rebuilt when its kind changes.
    pub(super) moderator_setup: SetupParts,
    /// Holds the panel's rows.
    panel_list: WidgetId,
    /// One row per panelist, in panel order.
    pub(super) panel: Vec<PanelRow>,
    pub(super) add_row: WidgetId,
    pub(super) auto_controls: WidgetId,
    pub(super) start_button: WidgetId,
    /// Says why starting may not work yet.
    pub(super) note: WidgetId,
    /// The list of stored discussions; rebuilt when the list changes.
    pub(super) history: WidgetId,
    /// The kinds every agent menu offers, in menu order: a menu's index
    /// is a position in this list.
    pub(super) available: Vec<AgentKind>,
}

/// One panelist: its agent kind, model and effort, and its remove button.
pub(super) struct PanelRow {
    pub(super) host: WidgetId,
    pub(super) kind: WidgetId,
    pub(super) setup: SetupParts,
    pub(super) remove: WidgetId,
}

pub(crate) struct Form {
    pub topic: String,
    /// As typed; empty runs the agents in the discussions directory.
    pub workdir: String,
    /// After this many rounds the discussion ends.
    pub max_rounds: RoundNo,
    pub moderator: AgentChoice,
    pub panel: Vec<AgentChoice>,
}

impl Form {
    pub(super) fn checked_workdir(&self) -> Result<Option<PathBuf>, String> {
        if self.topic.is_empty() {
            return Err("議題を入力してください".into());
        }
        if self.panel.is_empty() {
            return Err("パネリストを 1 人以上選んでください".into());
        }
        if self.panel.len() > MAX_PANEL {
            return Err(format!("パネリストは {MAX_PANEL} 体までです"));
        }
        workdir(&self.workdir)
    }
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
            // Centered while it fits; from the top once it scrolls.
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
    let card = tree.insert(
        Some(cards),
        Container::new(Style {
            flex_direction: FlexDirection::Column,
            flex_shrink: 0.0,
            gap: gap(12.0),
            padding: LayoutRect::length(24.0),
            size: LayoutSize {
                width: length(560.0),
                height: auto(),
            },
            ..Default::default()
        })
        .background(Role::Surface)
        .border(1.0, Role::Separator)
        .radius(10.0),
    );
    tree.insert(
        Some(card),
        Label::new("新しいディスカッション")
            .font_size(17.0)
            .weight(600),
    );
    tree.insert(
        Some(card),
        Label::new(
            "司会の Noa が議題をパネリストに渡し、毎ラウンド全員に同じ状況ブリーフを届けて要約します。\
             エージェントはそれぞれ Noa のペインで動きます。",
        )
        .font_size(12.0)
        .color(Role::Muted)
        .wrap(true),
    );
    caption(tree, card, "議題");
    let topic = tree.insert(
        Some(card),
        TextArea::new("例: 社内ツールの永続化層を SQLite のままにするか").min_lines(3),
    );
    caption(tree, card, "作業ディレクトリ（リポジトリ）");
    let workdir = tree.insert(
        Some(card),
        TextField::new("空欄: 議論専用フォルダ。例: ~/repos/my-app", 512.0),
    );
    tree.insert(
        Some(card),
        Label::new(
            "エージェントはここで起動し、コードを読んで議論します。返答ファイルは .noa/discussions に書かれます（git には載りません）。",
        )
        .font_size(11.0)
        .color(Role::Muted)
        .wrap(true),
    );

    let rounds = tree.insert(Some(card), Container::column().gap(6.0));
    caption(tree, rounds, "ラウンド数の上限");
    let max_rounds = tree.insert(
        Some(rounds),
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
    // Until the check for installed agents is done, every kind is offered.
    let available = AgentKind::ALL.to_vec();
    let left = tree.insert(Some(card), Container::column().gap(6.0));
    let header = tree.insert(
        Some(left),
        Container::row().align_items(AlignItems::CENTER).gap(6.0),
    );
    caption(
        tree,
        header,
        "司会 (Noa) のエージェント・モデル・エフォート",
    );
    spacer(tree, header);
    let auto_controls = tree.insert(
        Some(header),
        Container::row().align_items(AlignItems::CENTER).gap(6.0),
    );
    let moderator_row = tree.insert(
        Some(left),
        Container::row().align_items(AlignItems::CENTER).gap(6.0),
    );
    let moderator_host = tree.insert(Some(moderator_row), Container::row());
    let moderator = tree.insert(
        Some(moderator_host),
        kind_menu(&available, AgentKind::ALL[0]),
    );
    let moderator_setup = insert_setup(tree, moderator_row, AgentKind::ALL[0]);

    let panel_col = tree.insert(Some(card), Container::column().gap(6.0));
    caption(
        tree,
        panel_col,
        format!("パネリスト（1〜{MAX_PANEL}体）・エージェント・モデル・エフォート"),
    );
    let panel_list = tree.insert(Some(panel_col), Container::column().gap(6.0));
    let panel = default_panel()
        .iter()
        .map(|choice| insert_row(tree, panel_list, choice, &available))
        .collect();
    let panel_foot = tree.insert(
        Some(panel_col),
        Container::row().align_items(AlignItems::CENTER).gap(8.0),
    );
    let add_row = tree.insert(
        Some(panel_foot),
        Button::new("＋ パネリストを追加").control_size(ControlSize::Small),
    );
    tree.insert(
        Some(panel_foot),
        Label::new("同じエージェントの2体目以降は claude-2, claude-3 … として参加します")
            .font_size(11.0)
            .color(Role::Muted),
    );

    let foot = tree.insert(
        Some(card),
        Container::row().align_items(AlignItems::CENTER).gap(8.0),
    );
    let note = tree.insert(Some(foot), Container::column().grow(1.0));
    let start_button = tree.insert(
        Some(foot),
        Button::new("ディスカッションを開始").default_button(true),
    );
    let history = insert_history_card(tree, cards);
    let mut parts = Parts {
        root,
        topic,
        workdir,
        max_rounds,
        moderator_host,
        moderator,
        moderator_setup,
        panel_list,
        panel,
        add_row,
        auto_controls,
        start_button,
        note,
        history,
        available,
    };
    sync_row_buttons(tree, &mut parts);
    parts
}

/// The panel the form opens with: one of each kind.
fn default_panel() -> Vec<AgentChoice> {
    AgentKind::ALL.iter().map(|&k| k.into()).collect()
}

const HISTORY_WIDTH: f32 = 380.0;
const HISTORY_HEIGHT: f32 = 600.0;

/// The card beside the form that lists the stored discussions; returns the
/// list's column.
fn insert_history_card(tree: &mut UiTree, parent: WidgetId) -> WidgetId {
    let card = tree.insert(
        Some(parent),
        Container::new(Style {
            flex_direction: FlexDirection::Column,
            flex_shrink: 1.0,
            size: LayoutSize {
                width: length(HISTORY_WIDTH),
                height: length(HISTORY_HEIGHT),
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
    super::heading(tree, card, "過去の議論");
    super::scroll_column(tree, card, 8.0)
}

/// Lists the stored discussions, newest first: each one opens read-only,
/// and one that has not ended can be resumed.
pub(super) fn rebuild_history(
    tree: &mut UiTree,
    parts: &Parts,
    history: &[Summary],
    actions: &mut Vec<(WidgetId, Action)>,
    text: &mut TextStore,
) {
    clear_children(tree, parts.history, text);
    if history.is_empty() {
        super::note(
            tree,
            parts.history,
            "保存された議論はまだありません。始めた議論はここに残り、あとから開いたり再開したりできます",
        );
        return;
    }
    for summary in history {
        history_row(tree, parts.history, summary, actions);
    }
}

fn history_row(
    tree: &mut UiTree,
    list: WidgetId,
    summary: &Summary,
    actions: &mut Vec<(WidgetId, Action)>,
) {
    let row = tree.insert(
        Some(list),
        Container::new(Style {
            flex_direction: FlexDirection::Column,
            flex_shrink: 0.0,
            gap: gap(4.0),
            padding: LayoutRect::length(8.0),
            size: LayoutSize {
                width: percent(1.0),
                height: auto(),
            },
            ..Default::default()
        })
        .background(Role::SurfaceAlt)
        .radius(6.0),
    );
    let outline = match &summary.state {
        Ok(outline) => outline,
        Err(why) => {
            tree.insert(
                Some(row),
                Label::new(summary.id.to_string())
                    .font_size(12.5)
                    .weight(600)
                    .truncation(Truncation::Tail),
            );
            super::note(tree, row, format!("読めません: {why}"));
            return;
        }
    };
    tree.insert(
        Some(row),
        Label::new(outline.topic.lines().next().unwrap_or_default().to_owned())
            .font_size(12.5)
            .weight(600)
            .truncation(Truncation::Tail),
    );
    let line = tree.insert(
        Some(row),
        Container::row().align_items(AlignItems::CENTER).gap(6.0),
    );
    let (state, tone) = if outline.closed {
        ("終了", Tone::Neutral)
    } else {
        ("進行中", Tone::Accent)
    };
    super::tag(tree, line, state, tone);
    caption(tree, line, rounds_label(outline.rounds, outline.max_rounds));
    caption(tree, line, local_date(outline.created_at));
    super::spacer(tree, line);
    let small = |label: &str| Button::new(label).control_size(ControlSize::Small);
    let open = tree.insert(Some(line), small("表示"));
    tree.set_accessible_label(open, format!("{} を表示（読み取りのみ）", outline.topic));
    actions.push((open, Action::Open(summary.dir.clone())));
    if !outline.closed {
        let resume = tree.insert(Some(line), small("再開"));
        tree.set_accessible_label(resume, format!("{} を再開", outline.topic));
        actions.push((resume, Action::Resume(summary.dir.clone())));
    }
}

/// `3 / 5 ラウンド`, or `3 ラウンド` without a limit.
pub(crate) fn rounds_label(rounds: usize, max: Option<RoundNo>) -> String {
    match max {
        Some(max) => format!("{rounds} / {max} ラウンド"),
        None => format!("{rounds} ラウンド"),
    }
}

/// `2026-11-23 14:05` in the local time zone.
fn local_date(at: Millis) -> String {
    let secs = (at / 1000) as i64;
    date_label(secs + utc_offset(secs))
}

/// The local time zone's offset from UTC at `secs`, in seconds.
fn utc_offset(secs: i64) -> i64 {
    let time = secs as libc::time_t;
    // SAFETY: `localtime_r` writes only into `tm`, which outlives the call.
    unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&time, &mut tm).is_null() {
            return 0;
        }
        tm.tm_gmtoff as i64
    }
}

/// `YYYY-MM-DD HH:MM` of `secs` since the epoch (already shifted to the
/// zone shown).
pub(crate) fn date_label(secs: i64) -> String {
    let (days, rest) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // Howard Hinnant's civil-from-days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02}",
        rest / 3600,
        rest % 3600 / 60
    )
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

/// A menu of the `available` kinds with `kind` picked (the first one when
/// `kind` is not among them).
fn kind_menu(available: &[AgentKind], kind: AgentKind) -> PopUpButton {
    let index = available.iter().position(|&k| k == kind).unwrap_or(0);
    PopUpButton::new(available.iter().map(|k| k.command())).selected_index(index)
}

/// One panel row showing `choice`: kind menu, model, effort, remove.
fn insert_row(
    tree: &mut UiTree,
    list: WidgetId,
    choice: &AgentChoice,
    available: &[AgentKind],
) -> PanelRow {
    let host = tree.insert(
        Some(list),
        Container::row().align_items(AlignItems::CENTER).gap(6.0),
    );
    let kind = tree.insert(Some(host), kind_menu(available, choice.kind));
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

/// Adding stops at [`MAX_PANEL`] rows (or when no kind is installed),
/// removing at one.
fn sync_row_buttons(tree: &mut UiTree, parts: &mut Parts) {
    let removable = parts.panel.len() > 1;
    for row in &parts.panel {
        tree.set_enabled(row.remove, removable);
    }
    tree.set_enabled(
        parts.add_row,
        parts.panel.len() < MAX_PANEL && !parts.available.is_empty(),
    );
}

/// Adds a row of the last row's kind (on its default model and effort),
/// so a second claude is one click away.
pub(super) fn add_row(tree: &mut UiTree, parts: &mut Parts) {
    if parts.panel.len() >= MAX_PANEL {
        return;
    }
    let Some(kind) = parts
        .panel
        .last()
        .map(|row| selected_kind(tree, row.kind, &parts.available))
        .or_else(|| parts.available.first().copied())
    else {
        return;
    };
    let row = insert_row(tree, parts.panel_list, &kind.into(), &parts.available);
    parts.panel.push(row);
    sync_row_buttons(tree, parts);
}

pub(super) fn remove_row(tree: &mut UiTree, parts: &mut Parts, index: usize, text: &mut TextStore) {
    if parts.panel.len() <= 1 || index >= parts.panel.len() {
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
    let Some(row) = parts.panel.iter_mut().find(|row| row.kind == source) else {
        return false;
    };
    let kind = selected_kind(tree, row.kind, &parts.available);
    clear_children(tree, row.setup.host, text);
    let (model, effort) = insert_setup_controls(tree, row.setup.host, kind);
    row.setup.model = model;
    row.setup.effort = effort;
    true
}

/// The start view's buttons, for the screen's action table.
pub(super) fn actions(parts: &Parts, actions: &mut Vec<(WidgetId, Action)>) {
    actions.push((parts.start_button, Action::Start));
    actions.push((parts.add_row, Action::AddPanelRow));
    for (index, row) in parts.panel.iter().enumerate() {
        actions.push((row.remove, Action::RemovePanelRow(index)));
    }
}

/// Rebuilds the moderator's model and effort controls for the kind now
/// picked, when it changed: each kind has its own models and levels.
pub(super) fn moderator_kind_changed(tree: &mut UiTree, parts: &mut Parts, text: &mut TextStore) {
    let kind = selected_kind(tree, parts.moderator, &parts.available);
    clear_children(tree, parts.moderator_setup.host, text);
    let (model, effort) = insert_setup_controls(tree, parts.moderator_setup.host, kind);
    parts.moderator_setup.model = model;
    parts.moderator_setup.effort = effort;
}

/// The kind picked in `popup`, a menu of the `available` kinds. With none
/// available the menu is empty, and the first kind of all stands in.
fn selected_kind(tree: &UiTree, popup: WidgetId, available: &[AgentKind]) -> AgentKind {
    tree.get::<PopUpButton>(popup)
        .and_then(|p| available.get(p.selected()))
        .or_else(|| available.first())
        .copied()
        .unwrap_or(AgentKind::ALL[0])
}

/// The agents a form asks for, fitted to the available kinds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Fitted {
    pub moderator: AgentChoice,
    pub panel: Vec<AgentChoice>,
    /// The kinds left out for not being available, each once, in
    /// [`AgentKind::ALL`] order.
    pub dropped: Vec<AgentKind>,
}

/// Fits `moderator` and `panel` to the `available` kinds: panelists of
/// another kind are left out (at most [`MAX_PANEL`] stay; an empty panel
/// gets one of the first available kind), and a moderator of another kind
/// becomes the first available kind. With nothing available, the panel is
/// empty and the moderator stays as it was.
pub(crate) fn fit(
    moderator: &AgentChoice,
    panel: &[AgentChoice],
    available: &[AgentKind],
) -> Fitted {
    let is_available = |kind: AgentKind| available.contains(&kind);
    let dropped = AgentKind::ALL
        .into_iter()
        .filter(|&kind| !is_available(kind))
        .filter(|&kind| moderator.kind == kind || panel.iter().any(|c| c.kind == kind))
        .collect();
    let mut panel: Vec<AgentChoice> = panel
        .iter()
        .filter(|c| is_available(c.kind))
        .take(MAX_PANEL)
        .cloned()
        .collect();
    let moderator = match available.first() {
        Some(&first) => {
            if panel.is_empty() {
                panel.push(first.into());
            }
            if is_available(moderator.kind) {
                moderator.clone()
            } else {
                first.into()
            }
        }
        None => moderator.clone(),
    };
    Fitted {
        moderator,
        panel,
        dropped,
    }
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

/// Puts `moderator` and `panel`, fitted to the available kinds, into the
/// form (a rematch, or the agents as they were when the installed kinds
/// became known); returns the kinds left out.
pub(super) fn fill_agents(
    tree: &mut UiTree,
    parts: &mut Parts,
    moderator: &AgentChoice,
    panel: &[AgentChoice],
    text: &mut TextStore,
) -> Vec<AgentKind> {
    let fitted = fit(moderator, panel, &parts.available);
    clear_children(tree, parts.moderator_host, text);
    parts.moderator = tree.insert(
        Some(parts.moderator_host),
        kind_menu(&parts.available, fitted.moderator.kind),
    );
    moderator_kind_changed(tree, parts, text);
    let kind = fitted.moderator.kind;
    set_setup(tree, &parts.moderator_setup, kind, &fitted.moderator.setup);
    for row in std::mem::take(&mut parts.panel) {
        tree.remove(row.host, text);
    }
    parts.panel = fitted
        .panel
        .iter()
        .map(|choice| insert_row(tree, parts.panel_list, choice, &parts.available))
        .collect();
    sync_row_buttons(tree, parts);
    fitted.dropped
}

/// Rebuilds every agent menu to offer only `available`, keeping the agents
/// picked where their kind is still offered; returns the kinds left out.
pub(super) fn set_available(
    tree: &mut UiTree,
    parts: &mut Parts,
    available: Vec<AgentKind>,
    text: &mut TextStore,
) -> Vec<AgentKind> {
    let (moderator, panel) = read_agents(tree, parts);
    parts.available = available;
    fill_agents(tree, parts, &moderator, &panel, text)
}

/// Whether the agents are still those the form opened with.
pub(super) fn agents_untouched(tree: &UiTree, parts: &Parts) -> bool {
    read_agents(tree, parts) == (AgentKind::ALL[0].into(), default_panel())
}

/// `インストールされていないため除外: codex, agy`.
pub(super) fn excluded_note(dropped: &[AgentKind]) -> String {
    let names: Vec<&str> = dropped.iter().map(|k| k.command()).collect();
    format!("インストールされていないため除外: {}", names.join(", "))
}

/// Says why starting may not work yet: the check for installed agents
/// (`None`: still running), and the link to Noa.
pub(super) fn rebuild_note(
    tree: &mut UiTree,
    parts: &Parts,
    link: &Link,
    installed: Option<&Installed>,
    text: &mut TextStore,
) {
    clear_children(tree, parts.note, text);
    let agents = match installed {
        None => Some("インストール済みのエージェントを確認しています…".to_owned()),
        Some(Installed::Unknown(why)) => Some(format!(
            "インストール済みエージェントを確認できませんでした（{why}）。すべて表示しています"
        )),
        Some(Installed::Found(kinds)) if kinds.is_empty() => Some(format!(
            "{} のいずれもログインシェルの PATH に見つかりません",
            AgentKind::ALL.map(AgentKind::command).join(" / ")
        )),
        Some(Installed::Found(_)) => None,
    };
    let link = match link {
        Link::Live | Link::Stored(_) => None,
        Link::Connecting => Some("Noa に接続しています…".to_owned()),
        Link::Down(why) => Some(format!("Noa に接続できません: {why}")),
    };
    for note in [agents, link].into_iter().flatten() {
        super::note(tree, parts.note, note);
    }
}

/// Empties the topic, for the next discussion.
pub(super) fn clear_topic(tree: &mut UiTree, parts: &Parts) {
    tree.update::<TextArea, _>(parts.topic, |a, cx| a.set_text(cx, ""));
}

pub(super) fn read(tree: &UiTree, parts: &Parts) -> Form {
    let topic = tree
        .get::<TextArea>(parts.topic)
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
    let (moderator, panel) = read_agents(tree, parts);
    Form {
        topic,
        workdir,
        max_rounds,
        moderator,
        panel,
    }
}

/// The moderator and the panel, as the form shows them.
fn read_agents(tree: &UiTree, parts: &Parts) -> (AgentChoice, Vec<AgentChoice>) {
    let choice = |popup: WidgetId, setup: &SetupParts| {
        let kind = selected_kind(tree, popup, &parts.available);
        AgentChoice {
            kind,
            setup: read_setup(tree, setup, kind),
        }
    };
    let moderator = choice(parts.moderator, &parts.moderator_setup);
    let panel = parts
        .panel
        .iter()
        .map(|row| choice(row.kind, &row.setup))
        .collect();
    (moderator, panel)
}

/// The working directory typed in the start view: empty is none, `~` is
/// the home directory, and anything else must be an existing directory.
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
