//! User-owned display preferences shared by the standalone native widgets.
//! Included as source so widgets keep their independent Cargo workspaces.

use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use noa_gpui_core::style::{Size as LayoutSize, percent};
use noa_gpui_core::widgets::{
    Button, ButtonClicked, Container, Label, PopUpButton, ScrollView, SelectionChanged,
};
use noa_gpui_core::{Appearance, Message, Role, Style, Theme, UiTree, WidgetId};
use noa_gpui_platform::{App, AppOptions, Screen};
use noa_gpui_text::TextStore;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Mode {
    #[default]
    System,
    Light,
    Dark,
}

#[derive(Clone, Debug, PartialEq)]
struct Settings {
    mode: Mode,
    font_scale: f32,
    compact: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            mode: Mode::System,
            font_scale: 1.0,
            compact: false,
        }
    }
}

impl Settings {
    fn decode(bytes: &[u8]) -> Result<Self, String> {
        let value: serde_json::Value =
            serde_json::from_slice(bytes).map_err(|e| format!("表示設定を読めません: {e}"))?;
        if value["schema_version"].as_u64() != Some(1) {
            return Err("未対応の表示設定です".into());
        }
        let mode = match value["appearance"].as_str() {
            Some("system") => Mode::System,
            Some("light") => Mode::Light,
            Some("dark") => Mode::Dark,
            _ => return Err("外観はsystem / light / darkで指定してください".into()),
        };
        let font_scale = value["font_scale"].as_f64().ok_or("文字倍率がありません")? as f32;
        if ![1.0, 1.15, 1.3, 1.5]
            .iter()
            .any(|scale| (font_scale - scale).abs() < 0.001)
        {
            return Err("文字倍率は1.0 / 1.15 / 1.3 / 1.5で指定してください".into());
        }
        let compact = match value["density"].as_str() {
            Some("standard") => false,
            Some("compact") => true,
            _ => return Err("表示密度はstandard / compactで指定してください".into()),
        };
        Ok(Self {
            mode,
            font_scale,
            compact,
        })
    }

    fn encode(&self) -> Vec<u8> {
        serde_json::to_vec_pretty(&serde_json::json!({
            "schema_version":1,
            "appearance":match self.mode { Mode::System=>"system", Mode::Light=>"light", Mode::Dark=>"dark" },
            "font_scale":self.font_scale,
            "density":if self.compact { "compact" } else { "standard" },
        })).expect("display settings contain finite numbers")
    }

    fn appearance(&self, system: Appearance) -> Appearance {
        match self.mode {
            Mode::System => system,
            Mode::Light => Appearance::Light,
            Mode::Dark => Appearance::Dark,
        }
    }
}

fn path() -> Result<PathBuf, String> {
    if let Some(path) = std::env::var_os("NOA_WIDGET_APPEARANCE_FILE") {
        let path = PathBuf::from(path);
        if !path.is_absolute() {
            return Err("表示設定の指定先には絶対パスが必要です".into());
        }
        return Ok(path);
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or("表示設定の保存先がありません")?;
    #[cfg(target_os = "macos")]
    let root = home.join("Library/Application Support");
    #[cfg(not(target_os = "macos"))]
    let root = std::env::var_os("XDG_DATA_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".local/share"));
    Ok(root.join("noa/widget-appearance.json"))
}

fn read(path: &Path) -> Result<Settings, String> {
    let meta = match fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Settings::default());
        }
        Err(error) => return Err(error.to_string()),
    };
    if !meta.is_file() || meta.file_type().is_symlink() || meta.len() > 4096 {
        return Err("表示設定は4KiB以内の通常ファイルが必要です".into());
    }
    let mut bytes = Vec::new();
    fs::File::open(path)
        .map_err(|e| e.to_string())?
        .take(4097)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() > 4096 {
        return Err("表示設定が大きすぎます".into());
    }
    Settings::decode(&bytes)
}

fn save(path: &Path, settings: &Settings) -> Result<(), String> {
    let parent = path.parent().ok_or("表示設定の保存先が不正です")?;
    fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    if let Ok(meta) = fs::symlink_metadata(path) {
        if !meta.is_file() || meta.file_type().is_symlink() {
            return Err("通常ファイルでない表示設定は置き換えません".into());
        }
    }
    let temp = parent.join(format!(".widget-appearance-{}.tmp", std::process::id()));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&temp).map_err(|e| e.to_string())?;
    let result = (|| {
        file.write_all(&settings.encode())
            .map_err(|e| e.to_string())?;
        file.sync_all().map_err(|e| e.to_string())?;
        fs::rename(&temp, path).map_err(|e| e.to_string())?;
        fs::File::open(parent)
            .and_then(|directory| directory.sync_all())
            .map_err(|e| e.to_string())
    })();
    if result.is_err() {
        let _ = fs::remove_file(temp);
    }
    result
}

fn settings() -> &'static Settings {
    static SETTINGS: OnceLock<Settings> = OnceLock::new();
    SETTINGS.get_or_init(|| {
        // Keep offline fixtures and tests independent of the user's preferences.
        // An explicit file selects a reproducible appearance for screenshots.
        if std::env::var_os("NOA_WIDGET_APPEARANCE_FILE").is_none()
            && (cfg!(test)
                || std::env::args()
                    .any(|arg| arg.starts_with("--demo") || arg.starts_with("--snapshot")))
        {
            Settings::default()
        } else {
            path().and_then(|path| read(&path)).unwrap_or_default()
        }
    })
}

pub fn font(points: f32) -> f32 {
    points * settings().font_scale
}

/// Density changes whitespace, without shrinking text or minimum click targets.
pub fn space(points: f32) -> f32 {
    points * if settings().compact { 0.75 } else { 1.0 }
}

pub fn install(tree: &mut UiTree, base: Theme) {
    install_settings(tree, base, settings());
}

fn install_settings(tree: &mut UiTree, base: Theme, settings: &Settings) {
    let appearance = settings.appearance(base.appearance);
    tree.set_theme(if appearance == base.appearance {
        base
    } else {
        base.with_appearance(appearance)
    });
    apply_control_fonts(tree, settings.font_scale);
}

fn apply_control_fonts(tree: &mut UiTree, scale: f32) {
    if scale != 1.0 {
        let mut css = String::new();
        // These editable controls and buttons measure their styled text and grow
        // vertically. GPUI 0.2.1 menus, segments and tags ignore font-size, so do
        // not advertise or emit an ineffective override for those controls.
        for control in ["Button", "TextField", "TextArea", "ComboBox", "TextView"] {
            css.push_str(&format!("{control} {{ font-size: {}px; }}\n", 14.0 * scale));
        }
        tree.set_stylesheet(
            noa_gpui_core::styling::StyleSheet::parse(&css)
                .expect("generated display styles are valid"),
        );
    } else {
        tree.set_stylesheet(Default::default());
    }
}

pub fn system(tree: &mut UiTree, system: Appearance) {
    let appearance = settings().appearance(system);
    if tree.theme().appearance != appearance {
        tree.set_theme(tree.theme().clone().with_appearance(appearance));
    }
}

/// Handle this before any widget-specific parsing or backend initialization.
pub fn run_if_requested() -> Result<bool, String> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if !args.iter().any(|arg| arg == "--widget-settings") {
        return Ok(false);
    }
    if args.len() != 1 {
        return Err("--widget-settingsは単独で指定してください".into());
    }
    let options = AppOptions {
        title: "Noa · ウィジェットの表示設定".into(),
        size: (560.0, 520.0),
        min_size: Some((520.0, 480.0)),
        ..AppOptions::new("Noa Widget Settings")
    };
    App::new(options, |_| Box::new(SettingsScreen::new()))
        .run()
        .map_err(|e| e.to_string())?;
    Ok(true)
}

struct SettingsScreen {
    tree: UiTree,
    chosen: Settings,
    system: Appearance,
    mode: WidgetId,
    font: WidgetId,
    density: WidgetId,
    sample: WidgetId,
    save: WidgetId,
    reset: WidgetId,
    notice: WidgetId,
}

impl SettingsScreen {
    fn new() -> Self {
        let loaded = path().and_then(|path| read(&path));
        let chosen = loaded.clone().unwrap_or_default();
        Self::from_settings(chosen, loaded.err())
    }

    fn from_settings(chosen: Settings, error: Option<String>) -> Self {
        let mut tree = UiTree::new();
        tree.set_theme(Theme::for_appearance(chosen.appearance(Appearance::Dark)));
        let scroll = tree.insert(
            None,
            ScrollView::new(Style {
                size: LayoutSize {
                    width: percent(1.0),
                    height: percent(1.0),
                },
                ..Default::default()
            })
            .vertical(true),
        );
        let root = tree.insert(Some(scroll), Container::column().padding(24.0).gap(12.0));
        tree.insert(
            Some(root),
            Label::new("ウィジェットの表示設定")
                .font_size(21.0)
                .weight(700),
        );
        tree.insert(
            Some(root),
            Label::new("すべてのNoaウィジェットに適用します。").font_size(13.0),
        );
        tree.insert(Some(root), Label::new("外観").font_size(12.0));
        let mode = tree.insert(
            Some(root),
            PopUpButton::new(["システムに合わせる", "ライト", "ダーク"]).selected_index(
                match chosen.mode {
                    Mode::System => 0,
                    Mode::Light => 1,
                    Mode::Dark => 2,
                },
            ),
        );
        tree.set_accessible_label(mode, "外観");
        tree.insert(
            Some(root),
            Label::new("本文・入力の文字サイズ").font_size(12.0),
        );
        let font = tree.insert(
            Some(root),
            PopUpButton::new(["100%", "115%", "130%", "150%"]).selected_index(
                [1.0, 1.15, 1.3, 1.5]
                    .iter()
                    .position(|s| (chosen.font_scale - s).abs() < 0.001)
                    .unwrap_or(0),
            ),
        );
        tree.set_accessible_label(font, "本文・入力の文字サイズ");
        tree.insert(Some(root), Label::new("表示密度").font_size(12.0));
        let density = tree.insert(
            Some(root),
            PopUpButton::new(["標準", "コンパクト"]).selected_index(usize::from(chosen.compact)),
        );
        tree.set_accessible_label(density, "表示密度");
        let sample = tree.insert(
            Some(root),
            Label::new("表示例: 変更を確認して、次の作業へ。")
                .font_size(13.0 * chosen.font_scale)
                .wrap(true),
        );
        tree.insert(
            Some(root),
            Label::new("メニュー・切替タブ・タグは標準の文字サイズです。")
                .font_size(12.0)
                .color(Role::Muted)
                .wrap(true),
        );
        tree.insert(
            Some(root),
            Label::new("保存後、開いているウィジェットを開き直すと反映されます。")
                .font_size(12.0)
                .color(Role::Muted)
                .wrap(true),
        );
        let row = tree.insert(Some(root), Container::row().gap(12.0));
        let save = tree.insert(Some(row), Button::new("保存"));
        let reset = tree.insert(Some(row), Button::new("標準に戻す"));
        let notice = tree.insert(
            Some(root),
            Label::new(error.unwrap_or_default())
                .font_size(12.0)
                .wrap(true),
        );
        let mut screen = Self {
            tree,
            chosen,
            system: Appearance::Dark,
            mode,
            font,
            density,
            sample,
            save,
            reset,
            notice,
        };
        screen.preview();
        screen
    }
    fn preview(&mut self) {
        self.tree
            .set_theme(Theme::for_appearance(self.chosen.appearance(self.system)));
        apply_control_fonts(&mut self.tree, self.chosen.font_scale);
        let size = 13.0 * self.chosen.font_scale;
        self.tree.update::<Label, _>(self.sample, |label, cx| {
            *label = Label::new("表示例: 変更を確認して、次の作業へ。")
                .font_size(size)
                .wrap(true);
            cx.request_layout();
        });
    }
    fn notice(&mut self, message: String) {
        self.tree
            .update::<Label, _>(self.notice, |label, cx| label.set_text(cx, message));
    }
}

impl Screen for SettingsScreen {
    fn tree(&self) -> &UiTree {
        &self.tree
    }
    fn tree_mut(&mut self) -> &mut UiTree {
        &mut self.tree
    }
    fn set_system_appearance(&mut self, appearance: Appearance) {
        self.system = appearance;
        self.preview();
    }
    fn handle(&mut self, message: Message, _text: &mut TextStore) {
        if message.downcast_ref::<SelectionChanged>().is_some() {
            let selected = self
                .tree
                .get::<PopUpButton>(message.source)
                .map_or(0, PopUpButton::selected);
            if message.source == self.mode {
                self.chosen.mode = match selected {
                    1 => Mode::Light,
                    2 => Mode::Dark,
                    _ => Mode::System,
                };
            }
            if message.source == self.font {
                self.chosen.font_scale = [1.0, 1.15, 1.3, 1.5][selected.min(3)];
            }
            if message.source == self.density {
                self.chosen.compact = selected == 1;
            }
            self.preview();
        } else if message.downcast_ref::<ButtonClicked>().is_some() {
            if message.source == self.save {
                self.notice(match path().and_then(|path| save(&path, &self.chosen)) {
                    Ok(()) => "保存しました。ウィジェットを開き直してください。".into(),
                    Err(error) => format!("保存できません: {error}"),
                });
            } else if message.source == self.reset {
                self.chosen = Settings::default();
                for id in [self.mode, self.font, self.density] {
                    self.tree
                        .update::<PopUpButton, _>(id, |control, cx| control.set_selected(cx, 0));
                }
                self.preview();
                self.notice("標準に戻しました。保存すると適用されます。".into());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use noa_gpui_core::widgets::{ComboBox, TextArea, TextField};
    use noa_gpui_scene::Size;
    #[test]
    fn display_settings_roundtrip_and_reject_unbounded_values() {
        let settings = Settings {
            mode: Mode::Light,
            font_scale: 1.3,
            compact: true,
        };
        assert_eq!(Settings::decode(&settings.encode()).unwrap(), settings);
        for scale in [-1.0, 0.0, 1e20] {
            let mut json: serde_json::Value = serde_json::from_slice(&settings.encode()).unwrap();
            json["font_scale"] = serde_json::json!(scale);
            assert!(Settings::decode(&serde_json::to_vec(&json).unwrap()).is_err());
        }
        assert_eq!(settings.appearance(Appearance::Dark), Appearance::Light);
        assert_eq!(
            Settings::default().appearance(Appearance::Dark),
            Appearance::Dark
        );
    }

    #[test]
    fn large_text_controls_grow_without_clipping_or_overlapping() {
        for scale in [1.15, 1.3, 1.5] {
            let mut tree = UiTree::new();
            install_settings(
                &mut tree,
                Theme::for_appearance(Appearance::Light),
                &Settings {
                    font_scale: scale,
                    ..Settings::default()
                },
            );
            let root = tree.insert(None, Container::column().padding(12.0).gap(8.0));
            let button = tree.insert(Some(root), Button::new("変更を保存"));
            let field = tree.insert(Some(root), TextField::new("日本語のタイトル", 360.0));
            let combo = tree.insert(Some(root), ComboBox::new(["候補を選ぶ"], 360.0));
            let area = tree.insert(Some(root), TextArea::new("実現したいこと").min_lines(3));
            let mut text = TextStore::new(Default::default());
            tree.prepare(Size::new(420.0, 700.0), &mut text);
            let line = noa_gpui_text::TextStyle::new(14.0 * scale).line_height;
            let mut bottom = 0.0;
            for id in [button, field, combo, area] {
                let rect = tree.layout(id).unwrap().rect;
                assert!(rect.height >= line, "scale {scale}: {rect:?}");
                assert!(rect.y >= bottom, "controls overlap at scale {scale}");
                assert!(rect.x >= 0.0 && rect.x + rect.width <= 420.0);
                bottom = rect.y + rect.height;
            }
            assert!(tree.layout(field).unwrap().rect.height >= line + 14.0);
            assert!(tree.layout(area).unwrap().rect.height >= line * 3.0);
            assert!(bottom <= 700.0);
        }
    }

    #[test]
    fn settings_remain_scrollable_at_the_minimum_window_size() {
        let mut screen = SettingsScreen::from_settings(
            Settings {
                font_scale: 1.5,
                ..Settings::default()
            },
            None,
        );
        let mut text = TextStore::new(Default::default());
        screen.prepare(Size::new(520.0, 480.0), &mut text);
        let root = screen.tree.root().unwrap();
        assert!(screen.tree.get::<ScrollView>(root).is_some());
        let layout = screen.tree.layout(root).unwrap();
        assert!(layout.rect.height <= 480.0);
        assert!(layout.content_size.height >= layout.rect.height);
        let save = screen.tree.layout(screen.save).unwrap().rect;
        assert!(save.height >= noa_gpui_text::TextStyle::new(21.0).line_height);
    }
}
