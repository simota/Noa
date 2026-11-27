//! The palette entry is an offline catalogue. Managed instances are opened
//! by `Noa +ctl widget open`; this screen does not acquire a control token.

use std::path::Path;

use noa_gpui_core::widgets::{Button, ButtonClicked, Container, Label, ScrollView, Tag};
use noa_gpui_core::{Appearance, Message, Role, Style, Theme, UiTree, WidgetId};
use noa_gpui_platform::{Screen, WindowRequest};
use noa_gpui_text::TextStore;
use noa_widget::Manifest;

use crate::screen::WidgetScreen;
use crate::style;

pub struct Catalog {
    tree: UiTree,
    entries: Vec<(WidgetId, Manifest)>,
    requests: Vec<WindowRequest>,
    status: WidgetId,
}

impl Catalog {
    pub fn open(repo: &Path) -> Result<Self, String> {
        let context =
            noa_widget::discovery::repo_context(repo).map_err(|error| error.to_string())?;
        let definitions = noa_widget::discovery::discover(&context.worktree_root)
            .map_err(|error| error.to_string())?;
        let mut tree = UiTree::new();
        tree.set_theme(Theme::for_appearance(Appearance::Dark));
        let root = tree.insert(
            None,
            Container::new(Style {
                size: style::full(),
                ..style::column(0.0)
            })
            .background(Role::Background),
        );
        let header = tree.insert(
            Some(root),
            Container::new(style::wide(style::column(8.0)))
                .padding(20.0)
                .background(Role::Surface),
        );
        tree.insert(
            Some(header),
            Label::new("Repository Widgets").font_size(22.0).weight(700),
        );
        tree.insert(
            Some(header),
            Label::new(context.worktree_root.display().to_string())
                .font_size(12.0)
                .color(Role::Muted),
        );
        tree.insert(Some(header), Label::new("共有定義の初期データをプレビューできます。ライブ表示は担当エージェントから開きます。").font_size(13.0).max_lines(3));
        let scroll = tree.insert(Some(root), ScrollView::new(style::fill()));
        let list = tree.insert(
            Some(scroll),
            Container::new(style::wide(style::column(12.0))).padding(20.0),
        );
        let mut entries = Vec::new();
        for definition in definitions {
            let manifest = definition.manifest;
            let card = tree.insert(
                Some(list),
                Container::new(style::wide(style::column(8.0)))
                    .padding(16.0)
                    .background(Role::Surface)
                    .border(1.0, Role::Separator)
                    .radius(8.0),
            );
            let title = tree.insert(Some(card), Container::new(style::row(10.0)));
            tree.insert(
                Some(title),
                Label::new(&manifest.title).font_size(17.0).weight(700),
            );
            tree.insert(Some(title), Tag::new(format!("v{}", manifest.version)));
            tree.insert(
                Some(card),
                Label::new(&manifest.id).font_size(11.0).color(Role::Muted),
            );
            tree.insert(
                Some(card),
                Label::new(&manifest.description)
                    .font_size(13.0)
                    .max_lines(5),
            );
            let button = tree.insert(Some(card), Button::new("プレビュー"));
            entries.push((button, manifest));
        }
        if entries.is_empty() {
            tree.insert(
                Some(list),
                Label::new("ウィジェットがありません")
                    .font_size(18.0)
                    .weight(700),
            );
            tree.insert(
                Some(list),
                Label::new(".noa-widgets/<id>/widget.json に定義を追加してください。")
                    .font_size(14.0)
                    .max_lines(3),
            );
        }
        let footer = tree.insert(
            Some(root),
            Container::new(style::wide(style::column(0.0))).padding(14.0),
        );
        let status = tree.insert(
            Some(footer),
            Label::new(format!("{} 件の定義", entries.len()))
                .font_size(12.0)
                .color(Role::Muted)
                .max_lines(3),
        );
        Ok(Self {
            tree,
            entries,
            requests: Vec::new(),
            status,
        })
    }
}

impl Screen for Catalog {
    fn tree(&self) -> &UiTree {
        &self.tree
    }
    fn tree_mut(&mut self) -> &mut UiTree {
        &mut self.tree
    }

    fn handle(&mut self, message: Message, _text: &mut TextStore) {
        if message.downcast_ref::<ButtonClicked>().is_none() {
            return;
        }
        let Some((_, manifest)) = self.entries.iter().find(|(id, _)| *id == message.source) else {
            return;
        };
        match WidgetScreen::preview(manifest.clone(), manifest.initial_data.clone()) {
            Ok(screen) => self.requests.push(WindowRequest {
                title: format!("Noa · {} · Preview", manifest.title),
                size: (960.0, 720.0),
                screen: Box::new(screen),
                floating: false,
                titlebar: Default::default(),
                min_size: Some((480.0, 360.0)),
            }),
            Err(error) => {
                self.tree.update::<Label, _>(self.status, |label, cx| {
                    label.set_text(cx, format!("表示できません: {error}"))
                });
            }
        }
    }

    fn take_window_requests(&mut self) -> Vec<WindowRequest> {
        std::mem::take(&mut self.requests)
    }
}
