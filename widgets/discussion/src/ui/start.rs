//! The start view, shown until a session exists: the topic, the moderator's
//! agent kind and the panel, beside the stored discussions to open or
//! resume.

use noa_gpui_core::style::{
    AlignItems, FlexDirection, JustifyContent, Rect as LayoutRect, Size as LayoutSize, auto,
    length, percent,
};
use noa_gpui_core::widgets::{
    Button, CheckState, Checkbox, ComboBox, Container, ControlSize, Label, NumberInput,
    PopUpButton, TextArea, TextField, Tone, Truncation,
};
use noa_gpui_core::{Role, Style, UiTree, WidgetId};
use std::path::PathBuf;

use noa_gpui_text::TextStore;

use discussion::model::types::{AgentChoice, AgentKind, AgentSetup, Millis, RoundNo};
use discussion::store::Summary;

/// The round limit the start view offers first.
pub(crate) const DEFAULT_MAX_ROUNDS: RoundNo = 5;
/// The highest round limit the start view takes.
const MAX_MAX_ROUNDS: RoundNo = 50;

use super::{Action, caption, clear_children, gap};
use crate::live::Link;

pub(super) struct Parts {
    pub(super) root: WidgetId,
    pub(super) topic: WidgetId,
    pub(super) workdir: WidgetId,
    pub(super) max_rounds: WidgetId,
    pub(super) moderator: WidgetId,
    /// The model and effort of the moderator; rebuilt when its kind changes.
    pub(super) moderator_setup: SetupParts,
    /// One checkbox per agent kind, in `AgentKind::ALL` order.
    pub(super) panel: Vec<WidgetId>,
    /// The model and effort of each panel checkbox, in the same order.
    pub(super) panel_setup: Vec<SetupParts>,
    pub(super) start_button: WidgetId,
    note: WidgetId,
    /// The list of stored discussions; rebuilt when the list changes.
    pub(super) history: WidgetId,
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

pub(super) fn insert(tree: &mut UiTree, parent: WidgetId) -> Parts {
    let root = tree.insert(
        Some(parent),
        Container::new(Style {
            flex_direction: FlexDirection::Column,
            align_items: AlignItems::CENTER,
            justify_content: JustifyContent::CENTER,
            ..super::fill()
        }),
    );
    let cards = tree.insert(
        Some(root),
        Container::new(Style {
            flex_direction: FlexDirection::Row,
            align_items: AlignItems::START,
            gap: gap(16.0),
            max_size: LayoutSize {
                width: percent(1.0),
                height: percent(1.0),
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
    let left = tree.insert(Some(card), Container::column().gap(6.0));
    caption(tree, left, "司会 (Noa) のエージェント・モデル・エフォート");
    let moderator_row = tree.insert(
        Some(left),
        Container::row().align_items(AlignItems::CENTER).gap(6.0),
    );
    let moderator = tree.insert(
        Some(moderator_row),
        PopUpButton::new(AgentKind::ALL.iter().map(|k| k.command())),
    );
    let moderator_setup = insert_setup(tree, moderator_row, AgentKind::ALL[0]);
    let (panel, panel_setup) = insert_panel(tree, card, "パネリスト・モデル・エフォート");

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
    Parts {
        root,
        topic,
        workdir,
        max_rounds,
        moderator,
        moderator_setup,
        panel,
        panel_setup,
        start_button,
        note,
        history,
    }
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

/// One row per agent kind: whether it takes part, and its model and effort.
fn insert_panel(
    tree: &mut UiTree,
    card: WidgetId,
    title: &str,
) -> (Vec<WidgetId>, Vec<SetupParts>) {
    let column = tree.insert(Some(card), Container::column().gap(6.0));
    caption(tree, column, title);
    AgentKind::ALL
        .iter()
        .map(|&kind| {
            let row = tree.insert(
                Some(column),
                Container::row().align_items(AlignItems::CENTER).gap(10.0),
            );
            let check = tree.insert(
                Some(row),
                Container::new(Style {
                    size: LayoutSize {
                        width: length(84.0),
                        height: auto(),
                    },
                    ..Default::default()
                }),
            );
            let check = tree.insert(Some(check), Checkbox::new(kind.command()).checked(true));
            (check, insert_setup(tree, row, kind))
        })
        .unzip()
}

/// Rebuilds the moderator's model and effort controls for the kind now
/// picked, when it changed: each kind has its own models and levels.
pub(super) fn moderator_kind_changed(tree: &mut UiTree, parts: &mut Parts, text: &mut TextStore) {
    let kind = selected_kind(tree, parts.moderator);
    clear_children(tree, parts.moderator_setup.host, text);
    let (model, effort) = insert_setup_controls(tree, parts.moderator_setup.host, kind);
    parts.moderator_setup.model = model;
    parts.moderator_setup.effort = effort;
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

/// Says why starting may not work yet: the link to Noa.
pub(super) fn rebuild_note(tree: &mut UiTree, parts: &Parts, link: &Link, text: &mut TextStore) {
    clear_children(tree, parts.note, text);
    let note = match link {
        Link::Live | Link::Stored => return,
        Link::Connecting => "Noa に接続しています…".to_owned(),
        Link::Down(why) => format!("Noa に接続できません: {why}"),
    };
    super::note(tree, parts.note, note);
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
    let kind = selected_kind(tree, parts.moderator);
    let moderator = AgentChoice {
        kind,
        setup: read_setup(tree, &parts.moderator_setup, kind),
    };
    let panel = AgentKind::ALL
        .iter()
        .zip(&parts.panel)
        .zip(&parts.panel_setup)
        .filter(|((_, id), _)| {
            tree.get::<Checkbox>(**id)
                .is_some_and(|c| c.state() == CheckState::On)
        })
        .map(|((&kind, _), setup)| AgentChoice {
            kind,
            setup: read_setup(tree, setup, kind),
        })
        .collect();
    Form {
        topic,
        workdir,
        max_rounds,
        moderator,
        panel,
    }
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
