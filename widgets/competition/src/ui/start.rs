//! The start view: competition request, optional workdir, judge agent, round
//! limit, peer-reveal interval (default every 5 rounds), and contestant panel.

use std::path::PathBuf;

use noa_gpui_core::style::{
    AlignItems, FlexDirection, JustifyContent, Rect as LayoutRect, Size as LayoutSize, auto,
    length, percent,
};
use noa_gpui_core::widgets::{
    Button, CheckState, Checkbox, Container, Label, NumberInput, PopUpButton, TextArea, TextField,
};
use noa_gpui_core::{Role, Style, UiTree, WidgetId};
use noa_gpui_text::TextStore;

use competition::model::types::{AgentKind, DEFAULT_MAX_ROUNDS, DEFAULT_REVEAL_INTERVAL, RoundNo};

use super::{caption, clear_children, gap};
use crate::live::Link;

const MAX_MAX_ROUNDS: RoundNo = 50;
const MAX_REVEAL_INTERVAL: RoundNo = 25;

pub(super) struct Parts {
    pub(super) root: WidgetId,
    pub(super) request: WidgetId,
    pub(super) workdir: WidgetId,
    pub(super) max_rounds: WidgetId,
    pub(super) reveal_interval: WidgetId,
    pub(super) judge: WidgetId,
    pub(super) panel: Vec<WidgetId>,
    pub(super) start_button: WidgetId,
    note: WidgetId,
}

pub(crate) struct Form {
    pub request: String,
    pub workdir: String,
    pub max_rounds: RoundNo,
    pub reveal_interval: RoundNo,
    pub judge: AgentKind,
    pub panel: Vec<AgentKind>,
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
    let card = tree.insert(
        Some(root),
        Container::new(Style {
            flex_direction: FlexDirection::Column,
            gap: gap(12.0),
            padding: LayoutRect::length(24.0),
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
        .radius(10.0),
    );
    tree.insert(
        Some(card),
        Label::new("新しいコンペティション")
            .font_size(17.0)
            .weight(600),
    );
    tree.insert(
        Some(card),
        Label::new(
            "複数のエージェントが1つの依頼に対して独立して成果物を競作します。\
             通常ラウンドはお互いの成果物を知らずに進み、指定ラウンド毎（既定5ラウンド）に\
             相互閲覧を行って競争力を高めます。デザイン等は比較ビューアーで並べて確認できます。",
        )
        .font_size(12.0)
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
        .font_size(11.0)
        .color(Role::Muted)
        .wrap(true),
    );

    let row = tree.insert(
        Some(card),
        Container::row().align_items(AlignItems::START).gap(20.0),
    );
    let judge_col = tree.insert(Some(row), Container::column().gap(6.0));
    caption(tree, judge_col, "審判 (Judge)");
    let judge = tree.insert(
        Some(judge_col),
        PopUpButton::new(AgentKind::ALL.iter().map(|k| k.command())),
    );

    let rounds_col = tree.insert(Some(row), Container::column().gap(6.0));
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

    let reveal_col = tree.insert(Some(row), Container::column().gap(6.0));
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

    let panel_col = tree.insert(Some(row), Container::column().gap(6.0));
    caption(tree, panel_col, "参加パネル（2体以上）");
    let boxes = tree.insert(Some(panel_col), Container::row().gap(12.0));
    let panel = AgentKind::ALL
        .iter()
        .map(|kind| tree.insert(Some(boxes), Checkbox::new(kind.command()).checked(true)))
        .collect();

    let foot = tree.insert(
        Some(card),
        Container::row().align_items(AlignItems::CENTER).gap(8.0),
    );
    let note = tree.insert(Some(foot), Container::column().grow(1.0));
    let start_button = tree.insert(
        Some(foot),
        Button::new("コンペティションを開始").default_button(true),
    );
    Parts {
        root,
        request,
        workdir,
        max_rounds,
        reveal_interval,
        judge,
        panel,
        start_button,
        note,
    }
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
    let judge = tree
        .get::<PopUpButton>(parts.judge)
        .map_or(AgentKind::ALL[0], |p| AgentKind::ALL[p.selected()]);
    let panel = AgentKind::ALL
        .iter()
        .zip(&parts.panel)
        .filter(|(_, id)| {
            tree.get::<Checkbox>(**id)
                .is_some_and(|c| c.state() == CheckState::On)
        })
        .map(|(kind, _)| *kind)
        .collect();
    Form {
        request,
        workdir,
        max_rounds,
        reveal_interval,
        judge,
        panel,
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
