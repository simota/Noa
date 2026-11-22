//! The start view, shown until a session exists: the topic, the moderator's
//! agent kind and the panel.

use noa_gpui_core::style::{
    AlignItems, FlexDirection, JustifyContent, Rect as LayoutRect, Size as LayoutSize, auto,
    length, percent,
};
use noa_gpui_core::widgets::{
    Button, CheckState, Checkbox, Container, Label, PopUpButton, TextArea, TextField,
};
use noa_gpui_core::{Role, Style, UiTree, WidgetId};
use std::path::PathBuf;

use noa_gpui_text::TextStore;

use discussion::model::types::AgentKind;

use super::{caption, clear_children, gap};
use crate::live::Link;

pub(super) struct Parts {
    pub(super) root: WidgetId,
    pub(super) topic: WidgetId,
    pub(super) workdir: WidgetId,
    pub(super) moderator: WidgetId,
    /// One checkbox per agent kind, in `AgentKind::ALL` order.
    pub(super) panel: Vec<WidgetId>,
    pub(super) start_button: WidgetId,
    note: WidgetId,
}

pub(crate) struct Form {
    pub topic: String,
    /// As typed; empty runs the agents in the discussions directory.
    pub workdir: String,
    pub moderator: AgentKind,
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
                width: length(560.0),
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

    let row = tree.insert(
        Some(card),
        Container::row().align_items(AlignItems::START).gap(28.0),
    );
    let left = tree.insert(Some(row), Container::column().gap(6.0));
    caption(tree, left, "司会 (Noa) のエージェント");
    let moderator = tree.insert(
        Some(left),
        PopUpButton::new(AgentKind::ALL.iter().map(|k| k.command())),
    );
    let right = tree.insert(Some(row), Container::column().gap(6.0));
    caption(tree, right, "パネリスト");
    let boxes = tree.insert(Some(right), Container::row().gap(14.0));
    let panel = AgentKind::ALL
        .iter()
        .map(|kind| tree.insert(Some(boxes), Checkbox::new(kind.command()).checked(true)))
        .collect();

    let foot = tree.insert(
        Some(card),
        Container::row().align_items(AlignItems::CENTER).gap(8.0),
    );
    let note = tree.insert(Some(foot), Container::row().grow(1.0));
    let start_button = tree.insert(
        Some(foot),
        Button::new("ディスカッションを開始").default_button(true),
    );
    Parts {
        root,
        topic,
        workdir,
        moderator,
        panel,
        start_button,
        note,
    }
}

/// Says why starting may not work yet: the link to Noa.
pub(super) fn rebuild_note(tree: &mut UiTree, parts: &Parts, link: &Link, text: &mut TextStore) {
    clear_children(tree, parts.note, text);
    let note = match link {
        Link::Live => return,
        Link::Connecting => "Noa に接続しています…".to_owned(),
        Link::Down(why) => format!("Noa に接続できません: {why}"),
    };
    caption(tree, parts.note, note);
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
    let moderator = tree
        .get::<PopUpButton>(parts.moderator)
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
        topic,
        workdir,
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
