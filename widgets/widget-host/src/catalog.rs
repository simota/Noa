//! The palette entry is an offline catalogue. Managed instances are opened
//! by `Noa +ctl widget open`; this screen never acquires a control token.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use noa_gpui_core::widgets::{Button, ButtonClicked, Container, Label, ScrollView, Tag};
use noa_gpui_core::{Appearance, Event, Key, Message, Role, Style, Theme, UiTree, WidgetId};
use noa_gpui_platform::Screen;
use noa_gpui_text::TextStore;

use crate::appearance::{font, space};
use crate::style;

pub struct Catalog {
    tree: UiTree,
    repo: PathBuf,
    list: WidgetId,
    entries: Vec<(WidgetId, String)>,
    reload: WidgetId,
    status: WidgetId,
}

impl Catalog {
    pub fn open(repo: &Path) -> Result<Self, String> {
        let context =
            noa_widget::discovery::repo_context(repo).map_err(|error| error.to_string())?;
        let mut tree = UiTree::new();
        crate::appearance::install(&mut tree, Theme::for_appearance(Appearance::Dark));
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
                .padding(space(20.0))
                .background(Role::Surface),
        );
        tree.insert(
            Some(header),
            Label::new("Repository Widgets")
                .font_size(font(22.0))
                .weight(700),
        );
        tree.insert(
            Some(header),
            Label::new(context.worktree_root.display().to_string())
                .font_size(font(12.0))
                .color(Role::Muted)
                .max_lines(3),
        );
        tree.insert(Some(header), Label::new("共有定義とフィクスチャをプレビューできます。ライブ表示は担当エージェントから開きます。").font_size(font(13.0)).max_lines(3));
        let reload = tree.insert(Some(header), Button::new("再読み込み · ⌘R / Ctrl+R"));
        let scroll = tree.insert(Some(root), ScrollView::new(style::fill()));
        let list = tree.insert(
            Some(scroll),
            Container::new(style::wide(style::column(12.0))).padding(space(20.0)),
        );
        let footer = tree.insert(
            Some(root),
            Container::new(style::wide(style::column(0.0))).padding(space(14.0)),
        );
        let status = tree.insert(
            Some(footer),
            Label::new("")
                .font_size(font(12.0))
                .color(Role::Muted)
                .max_lines(4),
        );
        let mut catalog = Self {
            tree,
            repo: context.worktree_root,
            list,
            entries: Vec::new(),
            reload,
            status,
        };
        catalog.populate();
        Ok(catalog)
    }

    fn notice(&mut self, message: impl Into<String>) {
        let message = message.into();
        self.tree
            .update::<Label, _>(self.status, |label, cx| label.set_text(cx, message));
    }

    fn populate(&mut self) {
        let found = match noa_widget::discovery::discover_with_diagnostics(&self.repo) {
            Ok(found) => found,
            Err(error) => {
                self.tree.insert(
                    Some(self.list),
                    Label::new("ウィジェット一覧を読み込めません")
                        .font_size(font(18.0))
                        .weight(700),
                );
                self.tree.insert(
                    Some(self.list),
                    Label::new(error.to_string())
                        .font_size(font(13.0))
                        .max_lines(8),
                );
                self.notice(
                    "定義を確認して再読み込みしてください。安全に読めないフォルダーは開きません。",
                );
                return;
            }
        };
        for definition in found.definitions {
            let manifest = definition.manifest;
            let card = self.tree.insert(
                Some(self.list),
                Container::new(style::wide(style::column(8.0)))
                    .padding(space(16.0))
                    .background(Role::Surface)
                    .border(1.0, Role::Separator)
                    .radius(8.0),
            );
            self.tree.insert(
                Some(card),
                Label::new(&manifest.title)
                    .font_size(font(17.0))
                    .weight(700)
                    .max_lines(3),
            );
            self.tree.insert(
                Some(card),
                Tag::new(format!("{} · v{}", manifest.id, manifest.version)),
            );
            self.tree.insert(
                Some(card),
                Label::new(&manifest.description)
                    .font_size(font(13.0))
                    .max_lines(5),
            );
            let button = self
                .tree
                .insert(Some(card), Button::new("プレビューとフィクスチャ"));
            self.entries.push((button, manifest.id));
        }
        for diagnostic in &found.diagnostics {
            let card = self.tree.insert(
                Some(self.list),
                Container::new(style::wide(style::column(8.0)))
                    .padding(space(16.0))
                    .background(Role::Surface)
                    .border(1.0, Role::Separator)
                    .radius(8.0),
            );
            self.tree.insert(
                Some(card),
                Label::new("定義を読み込めません")
                    .font_size(font(15.0))
                    .weight(700),
            );
            self.tree.insert(
                Some(card),
                Label::new(diagnostic.path.display().to_string())
                    .font_size(font(12.0))
                    .max_lines(4),
            );
            self.tree.insert(
                Some(card),
                Label::new(&diagnostic.message)
                    .font_size(font(12.0))
                    .max_lines(8),
            );
        }
        if self.entries.is_empty() && found.diagnostics.is_empty() {
            self.tree.insert(
                Some(self.list),
                Label::new("ウィジェットがありません")
                    .font_size(font(18.0))
                    .weight(700),
            );
            self.tree.insert(
                Some(self.list),
                Label::new(".noa-widgets/<id>/widget.json に定義を追加してください。")
                    .font_size(font(14.0))
                    .max_lines(3),
            );
        }
        self.notice(format!(
            "{} 件の有効な定義 · {} 件の要修正",
            self.entries.len(),
            found.diagnostics.len()
        ));
    }

    fn refresh(&mut self, text: &mut TextStore) {
        for child in self.tree.children(self.list).to_vec() {
            self.tree.remove(child, text);
        }
        self.entries.clear();
        self.populate();
        self.tree.set_focus(Some(self.reload));
    }

    fn preview(&mut self, id: &str) {
        // Validate immediately so the catalogue can explain edits since its
        // last scan. The child repeats this descriptor-relative load.
        if let Err(error) = noa_widget::discovery::load(&self.repo, id) {
            self.notice(format!(
                "表示できません: {error}。定義を確認して再読み込みしてください。"
            ));
            return;
        }
        let result = std::env::current_exe().and_then(|program| {
            // Only this running Noa helper is launched, with argument arrays.
            // A fresh renderer Setup uploads all verified assets, including
            // images added after a catalogue reload; no repository code runs.
            Command::new(program)
                .arg(format!("--repo={}", self.repo.display()))
                .arg(format!("--preview-id={id}"))
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .spawn()
        });
        match result {
            Ok(mut child) => {
                std::thread::spawn(move || {
                    let _ = child.wait();
                });
                self.notice(
                    "プレビューを起動しています。フィクスチャはプレビュー内で切り替えられます。",
                );
            }
            Err(error) => self.notice(format!("プレビューを開けません: {error}")),
        }
    }
}

impl Screen for Catalog {
    fn set_system_appearance(&mut self, appearance: Appearance) {
        crate::appearance::system(&mut self.tree, appearance);
    }

    fn tree(&self) -> &UiTree {
        &self.tree
    }
    fn tree_mut(&mut self) -> &mut UiTree {
        &mut self.tree
    }

    fn handle(&mut self, message: Message, text: &mut TextStore) {
        if message.downcast_ref::<ButtonClicked>().is_none() {
            return;
        }
        if message.source == self.reload {
            self.refresh(text);
        } else if let Some((_, id)) = self
            .entries
            .iter()
            .find(|(button, _)| *button == message.source)
        {
            self.preview(&id.clone());
        }
    }

    fn dispatch(&mut self, event: Event, text: &mut TextStore) {
        if matches!(&event, Event::KeyDown { key: Key::Char('r'), modifiers }
            if modifiers.command || modifiers.ctrl)
        {
            self.refresh(text);
            return;
        }
        self.tree.dispatch(event, text);
        for message in self.tree.take_messages() {
            self.handle(message, text);
        }
        self.after_input();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reload_recovers_a_repaired_definition_and_keeps_valid_entries_visible() {
        let repo = tempfile::tempdir().unwrap();
        assert!(
            Command::new("git")
                .args(["init", "-q"])
                .arg(repo.path())
                .status()
                .unwrap()
                .success()
        );
        let package = repo.path().join(".noa-widgets/test-summary");
        std::fs::create_dir_all(&package).unwrap();
        std::fs::write(
            package.join("widget.json"),
            include_bytes!("../../../.noa-widgets/test-summary/widget.json"),
        )
        .unwrap();
        let broken = repo.path().join(".noa-widgets/agent-request");
        std::fs::create_dir_all(&broken).unwrap();
        std::fs::write(broken.join("widget.json"), b"bad json").unwrap();
        let mut catalog = Catalog::open(repo.path()).unwrap();
        assert_eq!(catalog.entries.len(), 1);
        assert!(
            catalog
                .tree
                .get::<Label>(catalog.status)
                .unwrap()
                .text()
                .contains("1 件の要修正")
        );
        std::fs::write(
            broken.join("widget.json"),
            include_bytes!("../../../.noa-widgets/agent-request/widget.json"),
        )
        .unwrap();
        let mut text = TextStore::new(Default::default());
        catalog.refresh(&mut text);
        assert_eq!(catalog.entries.len(), 2);
        assert_eq!(catalog.tree.focused(), Some(catalog.reload));
        assert!(
            catalog
                .tree
                .get::<Label>(catalog.status)
                .unwrap()
                .text()
                .contains("0 件の要修正")
        );
    }
}
