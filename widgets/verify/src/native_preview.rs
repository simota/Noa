//! Read-only native inspection of one stored image. It uses a separate
//! process because noa-gpui 0.2 uploads image textures during window setup.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use noa_gpui_core::style::{FlexDirection, Size as LayoutSize, length, percent};
use noa_gpui_core::widgets::{
    Button, ButtonClicked, Container, ImageScaling, Label, ScrollView, image_rect,
};
use noa_gpui_core::{
    Appearance, Event, Key, PaintCx, Role, Style, Theme, UiTree, Widget, WidgetId,
};
use noa_gpui_platform::{App, AppOptions, Screen};
use noa_gpui_scene::{ImageId, Size};
use noa_gpui_text::TextStore;

use verify::model::types::{EvidenceBody, EvidenceKey};
use verify::sibling::Launcher;

pub fn launch(session: &Path, key: &EvidenceKey) -> Result<(), String> {
    let mut directory = OsString::from("--session=");
    directory.push(session);
    verify::sibling::System.launch(
        verify::sibling::VERIFY,
        vec![directory, format!("--native-preview={key}").into()],
    )
}

fn parse(args: &[String]) -> Result<Option<(PathBuf, EvidenceKey)>, String> {
    if !args.iter().any(|arg| arg.starts_with("--native-preview=")) {
        return Ok(None);
    }
    let mut directory = None;
    let mut key = None;
    for arg in args {
        if let Some(value) = arg.strip_prefix("--session=") {
            if directory.is_some() || value.is_empty() {
                return Err("--session を1回指定してください".into());
            }
            directory = Some(std::path::absolute(value).map_err(|e| e.to_string())?);
        } else if let Some(value) = arg.strip_prefix("--native-preview=") {
            if key.is_some()
                || value.is_empty()
                || value.len() > 4096
                || value.chars().any(char::is_control)
            {
                return Err("画像の証跡 ID が不正です".into());
            }
            key = Some(EvidenceKey::new(value));
        } else {
            return Err("拡大プレビューは --session と --native-preview のみ指定できます".into());
        }
    }
    Ok(Some((
        directory.ok_or("拡大プレビューには --session が必要です")?,
        key.expect("flag was found"),
    )))
}

pub fn run_if_requested() -> Result<bool, String> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    let Some((directory, key)) = parse(&args)? else {
        return Ok(false);
    };
    // This is a fresh process, before its event loop begins. The live window
    // remains responsive, and read/decode retain the same byte/pixel limits.
    let loaded = (|| {
        let session = verify::store::read_session(&directory)
            .map_err(|e| format!("検証の記録を読めません: {e}"))?;
        let item = session
            .evidence_item(&key)
            .ok_or("指定した画像の証跡がありません")?;
        let image = verify::thumbnail::read(
            &verify::store::reply_base(&session),
            &verify::store::reply_dir(&session),
            item,
        )?;
        let caption = match &item.body {
            EvidenceBody::File { caption, .. } => caption.clone().unwrap_or_default(),
            _ => String::new(),
        };
        Ok::<_, String>((image, caption))
    })();
    let mut options = AppOptions::new("Noa · 証跡の拡大プレビュー");
    options.size = (1000.0, 760.0);
    options.min_size = Some((560.0, 420.0));
    App::new(options, move |setup| {
        let mut error = None;
        let mut picture = None;
        let mut caption = String::new();
        if let Ok((image, text)) = loaded {
            let limit = setup.device.limits().max_texture_dimension_2d;
            if image.width > limit || image.height > limit {
                error = Some(format!("元画像 {}×{} はこの GPU の表示上限 {limit} px を超えています。証跡の「開く」から外部ビューアで確認してください。", image.width, image.height));
            } else {
                let id = setup.renderer.create_image(setup.device, setup.queue, image.width, image.height, &image.pixels);
                picture = Some((id, Size::new(image.width as f32, image.height as f32)));
                caption = text;
            }
        } else if let Err(why) = loaded {
            error = Some(format!("画像を表示できません: {why}\n保存済み証跡は変更していません。元の画面で確認してください。"));
        }
        let mut screen = Preview::new(key, caption, picture, error);
        screen.wake = Some(setup.waker.clone());
        Box::new(screen)
    }).run().map_err(|e| e.to_string())?;
    Ok(true)
}

struct Preview {
    tree: UiTree,
    scroll: WidgetId,
    picture: Option<WidgetId>,
    fit: WidgetId,
    actual: WidgetId,
    close: WidgetId,
    original: Size,
    scale_factor: f32,
    wake: Option<noa_gpui_platform::Waker>,
    actual_size: bool,
    closing: bool,
}

impl Preview {
    fn new(
        key: EvidenceKey,
        caption: String,
        image: Option<(ImageId, Size)>,
        error: Option<String>,
    ) -> Self {
        let mut tree = UiTree::new();
        crate::appearance::install(&mut tree, Theme::for_appearance(Appearance::Dark));
        let root = tree.insert(
            None,
            Container::new(Style {
                flex_direction: FlexDirection::Column,
                size: LayoutSize {
                    width: percent(1.0),
                    height: percent(1.0),
                },
                ..Default::default()
            })
            .background(Role::Background),
        );
        let bar = tree.insert(
            Some(root),
            Container::wrapping_row(crate::appearance::space(8.0), crate::appearance::space(6.0))
                .padding(crate::appearance::space(12.0)),
        );
        let original = image.map_or(Size::new(0.0, 0.0), |(_, size)| size);
        tree.insert(
            Some(bar),
            Label::new(if image.is_some() {
                format!(
                    "{key} · 元画像 {}×{} px",
                    original.width as u32, original.height as u32
                )
            } else {
                key.to_string()
            })
            .font_size(crate::appearance::font(13.0))
            .wrap(true)
            .max_lines(2)
            .weight(600),
        );
        let fit = tree.insert(Some(bar), Button::new("全体を表示").selected(true));
        let actual = tree.insert(Some(bar), Button::new("100% / 原寸"));
        let close = tree.insert(Some(bar), Button::new("閉じる (Esc)"));
        if !caption.is_empty() {
            tree.insert(
                Some(root),
                Label::new(caption)
                    .font_size(crate::appearance::font(13.0))
                    .wrap(true)
                    .max_lines(3),
            );
        }
        if image.is_some() {
            tree.insert(Some(root), Label::new("保存時のサイズ・ハッシュを確認した元画像です。100% 表示ではスクロールして確認できます。").font_size(crate::appearance::font(11.0)).wrap(true).color(Role::Muted));
        }
        let scroll = tree.insert(
            Some(root),
            ScrollView::new(Style {
                flex_grow: 1.0,
                flex_shrink: 1.0,
                min_size: LayoutSize {
                    width: length(0.0),
                    height: length(0.0),
                },
                ..Default::default()
            })
            .horizontal(true)
            .vertical(true),
        );
        let picture = image.map(|(image, natural)| {
            tree.insert(
                Some(scroll),
                Picture {
                    image,
                    natural,
                    size: natural,
                    actual: false,
                    scale_factor: 1.0,
                },
            )
        });
        if let Some(why) = error {
            tree.insert(
                Some(scroll),
                Label::new(why)
                    .font_size(crate::appearance::font(14.0))
                    .wrap(true),
            );
            tree.set_hidden(fit, true);
            tree.set_hidden(actual, true);
        }
        Self {
            tree,
            scroll,
            picture,
            fit,
            actual,
            close,
            original,
            scale_factor: 1.0,
            wake: None,
            actual_size: false,
            closing: false,
        }
    }
}

impl Screen for Preview {
    fn tree(&self) -> &UiTree {
        &self.tree
    }
    fn tree_mut(&mut self) -> &mut UiTree {
        &mut self.tree
    }
    fn set_system_appearance(&mut self, appearance: Appearance) {
        crate::appearance::system(&mut self.tree, appearance);
    }
    fn wants_close(&self) -> bool {
        self.closing
    }

    fn frame_presented(&mut self, report: &noa_gpui_platform::FrameReport<'_>) {
        let scale = report.viewport.scale_factor;
        if scale.is_finite() && scale > 0.0 && self.scale_factor != scale {
            self.scale_factor = scale;
            if let Some(id) = self.picture {
                self.tree.update::<Picture, _>(id, |picture, cx| {
                    picture.scale_factor = scale;
                    cx.request_layout();
                    cx.request_paint();
                });
            }
            if let Some(wake) = &self.wake {
                wake.wake();
            }
        }
    }

    fn prepare(&mut self, viewport: Size, text: &mut TextStore) -> bool {
        // The toolbar can wrap, and captions/text scale change its height.
        // Measure it before fitting the image to the actual scroll viewport.
        let changed = self.tree.prepare(viewport, text).is_some();
        if let Some(id) = self.picture {
            let size = if self.actual_size {
                Size::new(
                    self.original.width / self.scale_factor,
                    self.original.height / self.scale_factor,
                )
            } else {
                let area = self.tree.window_rect(self.scroll).unwrap_or_default();
                Size::new(area.width.max(1.0), area.height.max(1.0))
            };
            let actual = self.actual_size;
            let scale = self.scale_factor;
            self.tree.update::<Picture, _>(id, |picture, cx| {
                if picture.size != size || picture.actual != actual || picture.scale_factor != scale
                {
                    picture.size = size;
                    picture.actual = actual;
                    picture.scale_factor = scale;
                    cx.request_layout();
                    cx.request_paint();
                }
            });
        }
        self.tree.prepare(viewport, text).is_some() || changed
    }

    fn dispatch(&mut self, event: Event, text: &mut TextStore) {
        if matches!(
            event,
            Event::KeyDown {
                key: Key::Escape,
                ..
            }
        ) {
            self.closing = true;
            return;
        }
        self.tree.dispatch(event, text);
        for message in self.tree.take_messages() {
            self.handle(message, text);
        }
    }

    fn handle(&mut self, message: noa_gpui_core::Message, _text: &mut TextStore) {
        if message.downcast_ref::<ButtonClicked>().is_some() {
            if message.source == self.close {
                self.closing = true;
            }
            if message.source == self.fit || message.source == self.actual {
                self.actual_size = message.source == self.actual;
                let actual = self.actual_size;
                self.tree
                    .update::<Button, _>(self.fit, |button, cx| button.set_selected(cx, !actual));
                self.tree
                    .update::<Button, _>(self.actual, |button, cx| button.set_selected(cx, actual));
            }
        }
    }
}

struct Picture {
    image: ImageId,
    natural: Size,
    size: Size,
    actual: bool,
    scale_factor: f32,
}

impl Widget for Picture {
    fn style(&self) -> Style {
        Style {
            size: LayoutSize {
                width: length(self.size.width),
                height: length(self.size.height),
            },
            flex_shrink: 0.0,
            ..Default::default()
        }
    }
    fn paint(&mut self, cx: &mut PaintCx) {
        let scaling = if self.actual {
            ImageScaling::Stretch
        } else {
            ImageScaling::FitDown
        };
        let natural = Size::new(
            self.natural.width / self.scale_factor,
            self.natural.height / self.scale_factor,
        );
        cx.image(image_rect(scaling, natural, cx.bounds()), self.image);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preview_requires_a_stored_session_and_never_accepts_run_options() {
        let parse_args =
            |args: &[&str]| parse(&args.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>());
        assert!(parse_args(&["--repo=/tmp"]).unwrap().is_none());
        assert!(parse_args(&["--native-preview=codex/r1/shot"]).is_err());
        assert!(
            parse_args(&[
                "--session=/tmp/session with spaces",
                "--native-preview=codex/r1/shot"
            ])
            .unwrap()
            .is_some()
        );
        assert!(
            parse_args(&[
                "--session=/tmp/session",
                "--native-preview=key",
                "--repo=/tmp"
            ])
            .is_err()
        );
        assert!(parse_args(&["--session=/tmp/session", "--native-preview=bad\nkey"]).is_err());
    }

    #[test]
    fn fitting_uses_the_measured_viewport_and_original_size_accounts_for_display_scale() {
        let mut screen = Preview::new(
            EvidenceKey::new("codex/r1/long-image-name"),
            "長い画像の説明。".repeat(30),
            Some((ImageId(1), Size::new(2560.0, 1440.0))),
            None,
        );
        let mut text = TextStore::new(Default::default());
        screen.prepare(Size::new(560.0, 420.0), &mut text);
        let viewport = screen.tree.window_rect(screen.scroll).unwrap();
        let picture = screen.tree.get::<Picture>(screen.picture.unwrap()).unwrap();
        assert!((picture.size.width - viewport.width).abs() < 0.1);
        assert!((picture.size.height - viewport.height).abs() < 0.1);
        screen.scale_factor = 2.0;
        screen.actual_size = true;
        screen.prepare(Size::new(560.0, 420.0), &mut text);
        let picture = screen.tree.get::<Picture>(screen.picture.unwrap()).unwrap();
        assert_eq!(picture.size, Size::new(1280.0, 720.0));
        assert_eq!(picture.scale_factor, 2.0);
    }
}
