//! The first-run path: inspect the repository, review detected assets, and
//! explicitly register them or create a setup task. Previews keep the exact
//! reviewed content so a background scan cannot silently change a write.

use scaffold::setup::CheckState;

use super::*;

#[derive(Debug, Clone)]
pub(super) enum Preview {
    Register(Vec<Asset>),
    Task { key: String, text: String },
}

impl Scaffold {
    pub(super) fn build_setup(&mut self, selection: Option<&Item>) {
        if let Some(preview) = self.setup_preview.clone() {
            self.build_setup_preview(preview);
            return;
        }
        let card = self.tree.insert(
            Some(self.ids.list),
            Container::new(wide(style::column(8.0)))
                .background(Role::Surface)
                .border(1.0, Role::Separator)
                .radius(8.0)
                .padding(16.0),
        );
        self.tree.insert(
            Some(card),
            Label::new("開発を始める準備を確認")
                .font_size(crate::appearance::font(20.0))
                .weight(700)
                .wrap(true),
        );
        self.note(
            card,
            "手順書・検証設定・チームのルールを見つけ、足りない準備を次の作業につなげます。",
        );
        let stacks = if self.state.setup.stacks.is_empty() {
            "技術構成はまだ検出されていません".to_owned()
        } else {
            format!("技術構成: {}", self.state.setup.stacks.join(" · "))
        };
        let totals = self.tree.insert(Some(card), Container::new(wide(row(8.0))));
        let stack_host = self.tree.insert(Some(totals), Container::new(grow()));
        self.tree.insert(
            Some(stack_host),
            Label::new(stacks)
                .font_size(crate::appearance::font(12.0))
                .color(Role::Muted)
                .max_lines(1),
        );
        self.tree.insert(
            Some(totals),
            Tag::new(format!("検出 {} 件", self.state.setup.found_count())).tone(Tone::Success),
        );
        self.tree.insert(
            Some(totals),
            Tag::new(format!("要対応 {} 件", self.state.setup.attention_count())).tone(
                if self.state.setup.attention_count() == 0 {
                    Tone::Neutral
                } else {
                    Tone::Warning
                },
            ),
        );
        let registered = self
            .state
            .registry
            .as_ref()
            .map(|r| r.registry.assets.len())
            .unwrap_or(0);
        self.tree
            .insert(Some(totals), Tag::new(format!("登録資産 {registered} 件")));
        let pending = self.state.pending_assets();
        let invalid_registry = matches!(self.state.registry, Err(ref e) if !matches!(e, scaffold::registry::LoadError::Absent));
        if invalid_registry {
            self.note(
                card,
                "資産台帳を読み込めません。情報源で原因を確認してから登録してください。",
            );
        } else if !pending.is_empty() {
            self.note(card, format!("{} 件の開発資産が見つかりました。内容を確認して、チームで共有する台帳へ追加できます。", pending.len()));
        } else if registered > 0 {
            self.note(
                card,
                "検出した開発資産は登録済みです。資産タブで判断と検証の記録を残せます。",
            );
        } else {
            self.note(card, "登録できる資産はまだありません。下のチェック項目から、最初に用意するものを確認できます。");
        }
        if let Some((id, _)) = self.state.setup_task() {
            self.note(
                card,
                format!(
                    "セットアップのタスク {id} を作成済みです。{}",
                    self.state.task_destination(),
                ),
            );
        } else if self.state.setup.attention_count() == 0 && !self.state.setup.checks.is_empty() {
            self.note(
                card,
                "検出項目は揃っています。手順に沿って実環境で動作を確認できます。",
            );
        }
        let actions = self.tree.insert(Some(card), Container::new(row(8.0)));
        if let Some((id, _)) = self.state.setup_task() {
            self.button(actions, "作成したタスクを開く", Press::OpenTask(id));
        }
        if invalid_registry {
            self.button(actions, "情報源を確認", Press::GoTab(Tab::Sources));
        } else if !pending.is_empty() {
            self.button(actions, "登録内容を確認", Press::RegisterPreview);
        }
        if self.state.setup.attention_count() > 0 && self.state.setup_task().is_none() {
            self.button(
                actions,
                "不足を整える作業指示を確認",
                Press::SetupTaskPreview,
            );
        }
        self.note(card, "この一覧は構成ファイルの検出結果です。ツールのインストール状況やコマンドの実行結果は含みません。");
        let items = self.items(Tab::Setup);
        if items.is_empty() {
            if !self.state.setup.checks.is_empty() {
                self.filtered_empty();
            } else {
                self.empty(
                    "準備項目を取得できませんでした",
                    "リポジトリと情報源を確認して、再スキャンしてください。",
                );
                self.button(self.ids.list, "情報源を確認", Press::GoTab(Tab::Sources));
            }
            return;
        }
        self.note(
            self.ids.list,
            "チェック項目を選ぶと、根拠と次の手順を確認できます。",
        );
        for item in items {
            let Item::Setup(id) = &item else { continue };
            let Some(check) = self
                .state
                .setup
                .checks
                .iter()
                .find(|c| &c.id == id)
                .cloned()
            else {
                continue;
            };
            let r = self.row(item.clone(), selection == Some(&item), false);
            let top = self.tree.insert(Some(r), Container::new(wide(row(8.0))));
            let host = self.tree.insert(Some(top), Container::new(grow()));
            self.tree.insert(
                Some(host),
                Label::new(&check.title)
                    .font_size(crate::appearance::font(14.0))
                    .weight(600)
                    .max_lines(1),
            );
            self.tree.insert(
                Some(top),
                Tag::new(check.state.label()).tone(check_tone(&check.state)),
            );
            self.tree.insert(
                Some(r),
                Label::new(check.description)
                    .font_size(crate::appearance::font(12.0))
                    .max_lines(2)
                    .color(Role::Muted),
            );
        }
    }

    pub(super) fn setup_detail(&mut self, id: &str) {
        let Some(check) = self.state.setup.checks.iter().find(|c| c.id == id).cloned() else {
            return;
        };
        self.heading(check.title);
        self.tree.insert(
            Some(self.ids.info),
            Tag::new(check.state.label()).tone(check_tone(&check.state)),
        );
        self.line(check.description);
        self.section("確認の根拠");
        if check.evidence.is_empty() {
            self.line("対応する構成ファイルは見つかっていません。");
        }
        for evidence in &check.evidence {
            self.line(format!("{}:{}", evidence.path, evidence.line));
        }
        self.section("次の手順");
        self.line(check.next_step);
        if check.state != CheckState::Found && self.state.setup_task().is_none() {
            self.button(
                self.ids.info,
                "不足を整える作業指示を確認",
                Press::SetupTaskPreview,
            );
        }
    }

    fn build_setup_preview(&mut self, preview: Preview) {
        let card = self.tree.insert(
            Some(self.ids.list),
            Container::new(wide(style::column(10.0)))
                .background(Role::Surface)
                .border(1.0, Role::Accent)
                .radius(8.0)
                .padding(16.0),
        );
        self.button(card, "確認を閉じる (Esc)", Press::ClosePreview);
        match preview {
            Preview::Register(assets) => {
                self.tree.insert(
                    Some(card),
                    Label::new("登録する内容を確認")
                        .font_size(crate::appearance::font(18.0))
                        .weight(700),
                );
                self.note(card, format!("書込先: {REGISTRY}"));
                self.note(card, format!("次の {} 件を「候補」として資産台帳へ追加します。各資産の検証状態は「未実施」から始まります。", assets.len()));
                for asset in &assets {
                    let entry = self
                        .tree
                        .insert(Some(card), Container::new(wide(style::column(3.0))));
                    self.tree.insert(
                        Some(entry),
                        Label::new(&asset.title)
                            .font_size(crate::appearance::font(13.0))
                            .weight(600)
                            .wrap(true),
                    );
                    self.tree.insert(
                        Some(entry),
                        Label::new(format!(
                            "{} · {}\nid: {}",
                            asset.kind.label(),
                            asset.path,
                            asset.id
                        ))
                        .font_size(crate::appearance::font(12.0))
                        .color(Role::Muted)
                        .wrap(true)
                        .selectable(true),
                    );
                }
                if assets != self.state.pending_assets() {
                    self.note(card, "検出内容が変わりました。確認を閉じて、最新の登録内容をもう一度確認してください。");
                } else if assets.is_empty() {
                    self.note(card, "追加する資産はありません。");
                } else {
                    self.button(card, "この内容で登録", Press::RegisterAssets);
                }
            }
            Preview::Task { key, text } => {
                self.tree.insert(
                    Some(card),
                    Label::new("セットアップの作業指示")
                        .font_size(crate::appearance::font(18.0))
                        .weight(700),
                );
                self.note(
                    card,
                    format!(
                        "以下の内容を .noa/tasks/ に保存します。{}",
                        self.state.task_destination(),
                    ),
                );
                self.tree.insert(
                    Some(card),
                    Label::new(text)
                        .font_size(crate::appearance::font(12.0))
                        .wrap(true)
                        .selectable(true),
                );
                if self.state.setup_task().is_some() {
                    self.note(
                        card,
                        format!(
                            "この内容のタスクは作成済みです。{}",
                            self.state.task_destination(),
                        ),
                    );
                } else if key != self.state.setup.task_key() {
                    self.note(card, "準備状況が変わりました。確認を閉じて、最新の作業指示をもう一度確認してください。");
                } else if self.state.setup.attention_count() == 0 {
                    self.note(card, "不足している準備項目はありません。");
                } else {
                    self.button(card, "この内容でタスクを作成", Press::SetupTask);
                }
            }
        }
    }

    fn preview_top(&mut self) {
        self.reveal = false;
        self.tree
            .update::<ScrollView, _>(self.ids.list, |s, cx| s.set_offset(cx, 0.0));
    }

    pub(super) fn setup_press(&mut self, press: &Press, text: &mut TextStore) -> bool {
        match press {
            Press::RegisterPreview => {
                let assets = self.state.pending_assets();
                self.detail_open = false;
                self.close_form(text);
                self.setup_preview = Some(Preview::Register(assets));
                self.preview_top();
                self.rebuild(text);
            }
            Press::SetupTaskPreview => {
                self.tab = Tab::Setup;
                self.detail_open = false;
                self.close_form(text);
                self.setup_preview = Some(Preview::Task {
                    key: self.state.setup.task_key(),
                    text: self.state.setup.task_text(),
                });
                self.preview_top();
                self.rebuild(text);
            }
            Press::ClosePreview => {
                self.setup_preview = None;
                self.rebuild(text);
            }
            Press::RegisterAssets => {
                let valid = matches!(&self.setup_preview, Some(Preview::Register(assets))
                    if !assets.is_empty() && assets == &self.state.pending_assets());
                if !valid {
                    self.set_notice(Err(
                        "登録内容が変わりました。内容をもう一度確認してください。".into(),
                    ));
                    self.rebuild(text);
                } else if self.act(Action::RegisterAssets, text) {
                    self.setup_preview = None;
                    self.rebuild(text);
                }
            }
            Press::SetupTask => {
                let valid = matches!(&self.setup_preview, Some(Preview::Task { key, .. })
                    if key == &self.state.setup.task_key());
                if !valid
                    || self.state.setup.attention_count() == 0
                    || self.state.setup_task().is_some()
                {
                    self.set_notice(Err("準備状況が変わったか、この内容のタスクは作成済みです。セットアップを確認してください。".into()));
                    self.rebuild(text);
                } else if self.act(Action::SetupTask, text) {
                    self.setup_preview = None;
                    self.rebuild(text);
                }
            }
            _ => return false,
        }
        true
    }
}

fn check_tone(state: &CheckState) -> Tone {
    match state {
        CheckState::Found => Tone::Success,
        CheckState::Missing => Tone::Warning,
        CheckState::Unknown => Tone::Critical,
    }
}
