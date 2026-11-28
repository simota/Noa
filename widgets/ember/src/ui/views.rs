use noa_gpui_core::style::{
    AlignItems, FlexDirection, Rect as LayoutRect, Size as LayoutSize, auto, length,
};
use noa_gpui_core::widgets::{
    Button, Checkbox, Container, ControlSize, Label, PopUpButton, ScrollView, TextArea, TextField,
    TextView,
};
use noa_gpui_core::{Role, Style, UiTree, WidgetId};
use noa_gpui_text::TextStore;

use super::{Action, EmberScreen, Ids, Page, Preview, excerpt, scope_description, style, visible};
use ember::model::{RunKind, RunStatus, Scope};

fn space(value: f32) -> f32 {
    crate::appearance::space(value)
}

pub(super) fn shell_style(narrow: bool) -> Style {
    Style {
        flex_direction: if narrow {
            FlexDirection::Column
        } else {
            FlexDirection::Row
        },
        ..style::fill()
    }
}

pub(super) fn navigation_style(narrow: bool) -> Style {
    Style {
        flex_direction: if narrow {
            FlexDirection::Row
        } else {
            FlexDirection::Column
        },
        size: LayoutSize {
            width: if narrow { auto() } else { length(145.0) },
            height: auto(),
        },
        flex_shrink: 0.0,
        gap: LayoutSize {
            width: length(space(6.0)),
            height: length(space(7.0)),
        },
        padding: LayoutRect::length(space(13.0)),
        ..Default::default()
    }
}

fn label(
    tree: &mut UiTree,
    parent: WidgetId,
    value: impl Into<String>,
    size: f32,
    muted: bool,
) -> WidgetId {
    tree.insert(
        Some(parent),
        Label::new(value)
            .font_size(crate::appearance::font(size))
            .color(if muted { Role::Muted } else { Role::Text })
            .wrap(true),
    )
}

fn button(
    tree: &mut UiTree,
    parent: WidgetId,
    title: &str,
    action: Action,
    actions: &mut Vec<(WidgetId, Action)>,
) -> WidgetId {
    let id = tree.insert(
        Some(parent),
        Button::new(title).control_size(ControlSize::Small),
    );
    actions.push((id, action));
    id
}

fn column(tree: &mut UiTree, parent: WidgetId, padding: f32) -> WidgetId {
    let id = tree.insert(
        Some(parent),
        Container::column().padding(space(padding)).gap(space(10.0)),
    );
    style::wide(tree, id);
    id
}

fn row(tree: &mut UiTree, parent: WidgetId) -> WidgetId {
    tree.insert(
        Some(parent),
        Container::wrapping_row(space(8.0), space(6.0)).align_items(AlignItems::CENTER),
    )
}

fn input(tree: &mut UiTree, parent: WidgetId, name: &str) -> WidgetId {
    let id = tree.insert(Some(parent), TextField::new(name, 0.0));
    style::wide(tree, id);
    tree.set_accessible_label(id, name);
    id
}

fn area(tree: &mut UiTree, parent: WidgetId, name: &str, lines: u32, height: f32) -> WidgetId {
    let scroller = tree.insert(
        Some(parent),
        ScrollView::new(Style {
            size: LayoutSize {
                width: auto(),
                height: length(height),
            },
            min_size: LayoutSize {
                width: length(0.0),
                height: length(height),
            },
            flex_shrink: 0.0,
            ..Default::default()
        })
        .vertical(true),
    );
    let id = tree.insert(Some(scroller), TextArea::new(name).min_lines(lines));
    style::wide(tree, id);
    tree.set_accessible_label(id, name);
    id
}

pub(super) fn build(tree: &mut UiTree) -> (Ids, Vec<(WidgetId, Action)>) {
    let mut actions = Vec::new();
    let root = tree.insert(
        None,
        Container::new(style::root()).background(Role::Background),
    );
    let top = tree.insert(
        Some(root),
        Container::wrapping_row(space(18.0), space(8.0))
            .padding(space(18.0))
            .align_items(AlignItems::CENTER),
    );
    let brand = tree.insert(Some(top), Container::column().gap(space(3.0)).grow(1.0));
    tree.insert(
        Some(brand),
        Label::new("エンバー")
            .font_size(crate::appearance::font(24.0))
            .weight(600),
    );
    label(tree, brand, "マイエージェント", 12.0, true);
    let status = label(tree, top, "接続中…", 12.0, true);
    button(tree, top, "再接続", Action::Reconnect, &mut actions);
    let scope_bar = tree.insert(
        Some(root),
        Container::wrapping_row(space(12.0), space(6.0))
            .padding(space(10.0))
            .background(Role::Surface),
    );
    label(tree, scope_bar, "参照範囲", 12.0, true);
    let scope = tree.insert(
        Some(scope_bar),
        PopUpButton::new(["個人の会話", "選択したプロジェクト"]).control_size(ControlSize::Small),
    );
    tree.set_accessible_label(scope, "会話と依頼の参照範囲");
    let scope_label = tree.insert(
        Some(scope_bar),
        Label::new("すべての会話（個人）")
            .font_size(crate::appearance::font(12.0))
            .color(Role::Muted)
            .wrap(true)
            .max_lines(2),
    );
    let shell = tree.insert(Some(root), Container::new(shell_style(false)));
    let navigation = tree.insert(
        Some(shell),
        Container::new(navigation_style(false)).background(Role::Surface),
    );
    let navigation_buttons = Page::ALL.map(|page| {
        button(
            tree,
            navigation,
            page.label(),
            Action::Page(page),
            &mut actions,
        )
    });
    let content = tree.insert(Some(shell), Container::new(style::column()));
    let pages = Page::ALL.map(|_| tree.insert(Some(content), Container::new(style::column())));

    let conversation = pages[Page::Conversation.index()];
    let history_scroll = tree.insert(
        Some(conversation),
        ScrollView::new(style::fill())
            .vertical(true)
            .follows_end(true),
    );
    let history = column(tree, history_scroll, 22.0);
    let compose = column(tree, conversation, 16.0);
    let composer = area(tree, compose, "エンバーに相談する…", 3, 84.0);
    let compose_actions = row(tree, compose);
    button(
        tree,
        compose_actions,
        "資料を添える",
        Action::Attachments,
        &mut actions,
    );
    label(
        tree,
        compose_actions,
        if cfg!(target_os = "macos") {
            "⌘ Enter で送信"
        } else {
            "Ctrl Enter で送信"
        },
        11.0,
        true,
    );
    let send = button(tree, compose_actions, "送信", Action::Send, &mut actions);
    let stop = button(
        tree,
        compose_actions,
        "応答を停止",
        Action::StopConversation,
        &mut actions,
    );

    let requests = pages[Page::Requests.index()];
    let request_header = column(tree, requests, 16.0);
    let header_row = row(tree, request_header);
    label(tree, header_row, "依頼", 19.0, false);
    let archive_toggle = button(
        tree,
        header_row,
        "保管済みも表示",
        Action::ShowArchived,
        &mut actions,
    );
    label(
        tree,
        request_header,
        "Noaと端末が稼働している間、画面を閉じても開始済みの依頼は進みます。",
        12.0,
        true,
    );
    let request_scroll = tree.insert(
        Some(requests),
        ScrollView::new(style::fill()).vertical(true),
    );
    let request_list = column(tree, request_scroll, 16.0);
    let request_form = column(tree, requests, 16.0);
    let kind_row = row(tree, request_form);
    label(tree, kind_row, "新しい依頼", 13.0, false);
    let kind = tree.insert(
        Some(kind_row),
        PopUpButton::new(["文書の作成", "選択資料の調査"]).control_size(ControlSize::Small),
    );
    tree.set_accessible_label(kind, "依頼の種類");
    let objective = area(
        tree,
        request_form,
        "目的と、どんな成果物がほしいかを入力",
        3,
        76.0,
    );
    let request_buttons = row(tree, request_form);
    button(
        tree,
        request_buttons,
        "資料を添える",
        Action::Attachments,
        &mut actions,
    );
    let start = button(
        tree,
        request_buttons,
        "この内容を任せる",
        Action::Start,
        &mut actions,
    );

    let memory = pages[Page::Memory.index()];
    let memory_heading = column(tree, memory, 16.0);
    label(tree, memory_heading, "記憶", 19.0, false);
    label(
        tree,
        memory_heading,
        "自分で保存した内容だけを使います。現在の範囲で使える記憶を表示しています。",
        12.0,
        true,
    );
    let memory_scroll = tree.insert(Some(memory), ScrollView::new(style::fill()).vertical(true));
    let memory_list = column(tree, memory_scroll, 16.0);
    let memory_form = column(tree, memory, 16.0);
    let memory_row = row(tree, memory_form);
    label(tree, memory_row, "保存内容と範囲", 13.0, false);
    let memory_scope = tree.insert(
        Some(memory_row),
        PopUpButton::new(["すべての会話", "プロジェクト限定"]).control_size(ControlSize::Small),
    );
    tree.set_accessible_label(memory_scope, "記憶を使う範囲");
    let memory_scope_label = tree.insert(
        Some(memory_form),
        Label::new("")
            .font_size(crate::appearance::font(11.0))
            .color(Role::Muted)
            .wrap(true)
            .max_lines(2),
    );
    let memory_text = area(tree, memory_form, "覚えておいてほしいことを入力", 3, 76.0);
    let memory_buttons = row(tree, memory_form);
    let memory_save = button(
        tree,
        memory_buttons,
        "記憶に保存",
        Action::SaveMemory,
        &mut actions,
    );
    button(
        tree,
        memory_buttons,
        "入力をクリア",
        Action::NewMemory,
        &mut actions,
    );

    let settings = pages[Page::Settings.index()];
    let settings_scroll = tree.insert(
        Some(settings),
        ScrollView::new(style::fill()).vertical(true),
    );
    let settings_body = column(tree, settings_scroll, 22.0);
    label(tree, settings_body, "設定", 19.0, false);
    let backend_notice = label(
        tree,
        settings_body,
        "接続設定を確認しています。",
        12.0,
        true,
    );
    label(tree, settings_body, "モデル", 13.0, false);
    let model = input(tree, settings_body, "OpenAIのモデル名");
    let notifications = tree.insert(
        Some(settings_body),
        Checkbox::new("依頼の完了・失敗を知らせる"),
    );
    let settings_save = button(
        tree,
        settings_body,
        "モデルと通知を保存",
        Action::SaveSettings,
        &mut actions,
    );
    label(tree, settings_body, "参照するプロジェクト", 13.0, false);
    let project_path = input(tree, settings_body, "プロジェクトのフォルダーのパス");
    button(
        tree,
        settings_body,
        "このプロジェクトを選ぶ",
        Action::ApplyProject,
        &mut actions,
    );
    label(
        tree,
        settings_body,
        "選ぶだけではファイルを送信しません。資料は内容を確認してから添えます。",
        12.0,
        true,
    );
    let connection = tree.insert(
        Some(settings_body),
        Container::column()
            .padding(space(14.0))
            .gap(space(8.0))
            .background(Role::Surface)
            .radius(8.0),
    );
    label(tree, connection, "接続設定", 13.0, false);
    label(
        tree,
        connection,
        "OpenAI APIキーは NOA_EMBER_OPENAI_API_KEY または OPENAI_API_KEY から読み取ります。キーを会話や記憶に入力しないでください。",
        12.0,
        true,
    );
    label(
        tree,
        connection,
        "macOSではキーチェーンのサービス noa.ember.openai、アカウント api-key も利用できます。設定後は再接続してください。",
        12.0,
        true,
    );
    let responsibilities = tree.insert(
        Some(settings_body),
        Container::column()
            .padding(space(14.0))
            .gap(space(7.0))
            .background(Role::Surface)
            .radius(8.0),
    );
    label(tree, responsibilities, "仕事の進め方", 13.0, false);
    label(
        tree,
        responsibilities,
        "文章・選択資料の調査：エンバーで成果物を作成\n開発：Workflowの開始画面へ引き継ぐ\nコード公開：対応するWorkflowで内容を確認",
        12.0,
        true,
    );
    label(
        tree,
        settings_body,
        "Noa終了・端末スリープ・接続断の後は実行状態を確認します。確認できない結果を完了扱いにしません。",
        12.0,
        true,
    );
    let data_actions = row(tree, settings_body);
    button(
        tree,
        data_actions,
        "保存データを表示",
        Action::Export,
        &mut actions,
    );
    button(
        tree,
        data_actions,
        "この範囲の会話を削除",
        Action::ClearConversation,
        &mut actions,
    );

    let preview_page = tree.insert(
        Some(content),
        Container::new(style::column())
            .padding(space(18.0))
            .gap(space(12.0)),
    );
    let preview_row = row(tree, preview_page);
    button(
        tree,
        preview_row,
        "戻る",
        Action::ClosePreview,
        &mut actions,
    );
    let preview_title = label(tree, preview_row, "", 18.0, false);
    let preview_note = label(tree, preview_page, "", 12.0, true);
    let preview_body = tree.insert(
        Some(preview_page),
        TextView::new(400.0).fill_height().read_only(true),
    );
    style::wide(tree, preview_body);
    tree.set_accessible_label(preview_body, "資料と成果物の内容");
    let preview_actions = row(tree, preview_page);
    let preview_share = button(
        tree,
        preview_actions,
        "この内容を共有",
        Action::SharePreview,
        &mut actions,
    );
    let preview_open = button(
        tree,
        preview_actions,
        "Workflowの開始画面を開く",
        Action::OpenHandoff,
        &mut actions,
    );
    let preview_confirm = button(
        tree,
        preview_actions,
        "削除する",
        Action::Confirm,
        &mut actions,
    );

    let share_panel = column(tree, root, 12.0);
    label(tree, share_panel, "共有する資料", 13.0, false);
    let attachment_path = input(tree, share_panel, "資料ファイルの絶対パス");
    let attachment_buttons = row(tree, share_panel);
    let preview_file = button(
        tree,
        attachment_buttons,
        "内容を確認",
        Action::PreviewFile,
        &mut actions,
    );
    button(
        tree,
        attachment_buttons,
        "共有資料を外す",
        Action::ClearAttachments,
        &mut actions,
    );
    label(
        tree,
        share_panel,
        "内容を確認してから共有できます。送信時点の内容を読み取ります。",
        11.0,
        true,
    );
    let attachment_summary = label(tree, root, "", 11.0, true);
    let footer = tree.insert(
        Some(root),
        Container::wrapping_row(space(10.0), space(6.0))
            .padding(space(10.0))
            .background(Role::Surface),
    );
    let notice = tree.insert(
        Some(footer),
        Label::new("")
            .font_size(crate::appearance::font(11.0))
            .wrap(true)
            .max_lines(3)
            .color(Role::Muted),
    );
    let retry = button(
        tree,
        footer,
        "受付を再確認",
        Action::RetryPending,
        &mut actions,
    );
    (
        Ids {
            shell,
            navigation,
            navigation_buttons,
            pages,
            status,
            notice,
            retry,
            scope,
            scope_label,
            history,
            history_scroll,
            composer,
            send,
            stop,
            request_list,
            request_scroll,
            objective,
            kind,
            start,
            archive_toggle,
            memory_list,
            memory_scroll,
            memory_text,
            memory_scope,
            memory_scope_label,
            memory_save,
            model,
            notifications,
            project_path,
            settings_save,
            backend_notice,
            share_panel,
            attachment_path,
            attachment_summary,
            preview_file,
            preview_page,
            preview_title,
            preview_body,
            preview_note,
            preview_share,
            preview_open,
            preview_confirm,
        },
        actions,
    )
}

impl EmberScreen {
    pub(super) fn sync_chrome(&mut self) {
        let status = if self.demo {
            "デモ · 外部通信なし"
        } else if !self.connected {
            "未接続"
        } else if !self.view.backend_available {
            "接続設定を確認"
        } else {
            "エンバー接続済み"
        };
        self.label(self.ids.status, status);
        let notice = if !self.notice.is_empty() {
            self.notice.clone()
        } else {
            self.view
                .notice
                .clone()
                .unwrap_or_else(|| "会話と保存した記憶は、この端末に保管されます。".into())
        };
        self.label(self.ids.notice, excerpt(&notice, 700));
        self.label(self.ids.scope_label, scope_description(&self.scope));
        self.tree
            .update::<PopUpButton, _>(self.ids.scope, |picker, cx| {
                picker.set_selected(cx, usize::from(matches!(self.scope, Scope::Project { .. })))
            });
        self.tree
            .update::<PopUpButton, _>(self.ids.memory_scope, |picker, cx| {
                picker.set_selected(
                    cx,
                    usize::from(matches!(self.memory_scope, Scope::Project { .. })),
                )
            });
        self.label(
            self.ids.memory_scope_label,
            format!("保存先：{}", scope_description(&self.memory_scope)),
        );
        self.tree
            .update::<Button, _>(self.ids.memory_save, |button, cx| {
                button.set_label(
                    cx,
                    if self.memory_edit.is_some() {
                        "記憶を更新"
                    } else {
                        "記憶に保存"
                    },
                )
            });
        self.tree
            .update::<Button, _>(self.ids.archive_toggle, |button, cx| {
                button.set_label(
                    cx,
                    if self.show_archived {
                        "保管済みを隠す"
                    } else {
                        "保管済みも表示"
                    },
                )
            });
        for page in Page::ALL {
            self.tree.set_hidden(
                self.ids.pages[page.index()],
                self.preview.is_some() || self.page != page,
            );
            self.tree
                .update::<Button, _>(self.ids.navigation_buttons[page.index()], |button, cx| {
                    button.set_selected(cx, self.page == page)
                });
        }
        self.tree
            .set_hidden(self.ids.preview_page, self.preview.is_none());
        self.tree.set_hidden(
            self.ids.preview_share,
            !matches!(self.preview, Some(Preview::Attachment { .. })),
        );
        self.tree.set_hidden(
            self.ids.preview_open,
            !matches!(self.preview, Some(Preview::Handoff(_))),
        );
        self.tree.set_hidden(
            self.ids.preview_confirm,
            !matches!(self.preview, Some(Preview::Confirm(_))),
        );
        self.tree.set_hidden(
            self.ids.share_panel,
            !self.share_open
                || self.preview.is_some()
                || !matches!(self.page, Page::Conversation | Page::Requests),
        );
        self.tree.set_hidden(
            self.ids.attachment_summary,
            self.attachments.is_empty()
                || self.preview.is_some()
                || !matches!(self.page, Page::Conversation | Page::Requests),
        );
        let names = self
            .attachments
            .iter()
            .map(|attachment| {
                std::path::Path::new(&attachment.path)
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
            })
            .collect::<Vec<_>>()
            .join("、");
        self.label(
            self.ids.attachment_summary,
            format!(
                "  次の送信で共有する資料 {}件：{}",
                self.attachments.len(),
                excerpt(&names, 120)
            ),
        );
        let pending = self.pending.is_some();
        let writable = self.connected && !self.demo && !pending;
        let can_send = writable && self.view.backend_available;
        self.tree.set_enabled(
            self.ids.send,
            can_send
                && !self.area(self.ids.composer).trim().is_empty()
                && self.active_conversation().is_none(),
        );
        self.tree.set_enabled(
            self.ids.start,
            can_send && !self.area(self.ids.objective).trim().is_empty(),
        );
        self.tree.set_enabled(
            self.ids.stop,
            writable && self.active_conversation().is_some(),
        );
        self.tree
            .set_hidden(self.ids.stop, self.active_conversation().is_none());
        self.tree.set_enabled(
            self.ids.memory_save,
            writable && !self.area(self.ids.memory_text).trim().is_empty(),
        );
        self.tree
            .set_enabled(self.ids.settings_save, writable && self.settings_dirty);
        self.tree.set_enabled(
            self.ids.preview_file,
            !self.demo && !self.field(self.ids.attachment_path).trim().is_empty(),
        );
        self.tree.set_hidden(
            self.ids.retry,
            self.pending
                .as_ref()
                .is_none_or(|pending| pending.in_flight),
        );
        self.label(self.ids.backend_notice,if self.demo {
            "接続状態のサンプルです。キーは読み取っていません。"
        }else if self.view.backend_available {
            "接続設定を読み込みました。モデルの利用可否は送信時に確認されます。"
        }else {
            "接続設定を確認してください。APIキーを設定して再接続すると、会話と依頼を送信できます。"
        });
        for (id, action) in &self.actions {
            match action {
                Action::DeleteMemory(_)
                | Action::Cancel(_)
                | Action::Archive(..)
                | Action::DeleteRun(_)
                | Action::ClearConversation
                | Action::Confirm
                | Action::PrepareHandoff(_)
                | Action::OpenHandoff
                | Action::Export => self.tree.set_enabled(*id, writable),
                Action::Reconnect | Action::ApplyProject => self.tree.set_enabled(*id, !self.demo),
                _ => {}
            }
        }
    }

    pub(super) fn refresh_lists(&mut self, text: &mut TextStore) {
        let focused = self.tree.focused().and_then(|focused| {
            self.actions
                .iter()
                .find(|(id, _)| *id == focused)
                .map(|(_, action)| action.clone())
        });
        let scrolls = [
            self.ids.history_scroll,
            self.ids.request_scroll,
            self.ids.memory_scroll,
        ];
        let offsets = scrolls.map(|id| {
            self.tree
                .get::<ScrollView>(id)
                .map(|scroll| (scroll.scroll_position(), scroll.is_following_end()))
        });
        for parent in [
            self.ids.history,
            self.ids.request_list,
            self.ids.memory_list,
        ] {
            for child in self.tree.children(parent).to_vec() {
                self.tree.remove(child, text);
            }
        }
        self.actions.retain(|(id, _)| self.tree.contains(*id));
        self.history();
        self.requests();
        self.memories();
        for (id, offset) in scrolls.into_iter().zip(offsets) {
            if let Some((offset, following_end)) = offset {
                self.tree.update::<ScrollView, _>(id, |scroll, cx| {
                    if following_end {
                        scroll.scroll_to_end(cx);
                    } else {
                        scroll.set_scroll_position(cx, offset);
                    }
                });
            }
        }
        if let Some(action) = focused
            && let Some((id, _)) = self
                .actions
                .iter()
                .find(|(_, candidate)| *candidate == action)
        {
            self.tree.set_focus(Some(*id));
        }
        self.refresh_needed = false;
        self.sync_chrome();
    }

    fn history(&mut self) {
        let parent = self.ids.history;
        if self.view.state.messages.is_empty() {
            label(
                &mut self.tree,
                parent,
                "今日は、何を進めましょう。",
                21.0,
                false,
            );
            label(
                &mut self.tree,
                parent,
                "相談する、選んだ資料を調べる、文書をまとめる。依頼は画面を閉じても追跡できます。",
                14.0,
                true,
            );
            button(
                &mut self.tree,
                parent,
                "文書や調査を任せる",
                Action::Page(Page::Requests),
                &mut self.actions,
            );
            return;
        }
        let skip = self.view.state.messages.len().saturating_sub(60);
        if skip > 0 {
            label(
                &mut self.tree,
                parent,
                format!(
                    "直近60件を表示中。全履歴は設定の「保存データを表示」で確認できます（以前の{skip}件）。"
                ),
                11.0,
                true,
            );
        }
        let messages = self
            .view
            .state
            .messages
            .iter()
            .skip(skip)
            .cloned()
            .collect::<Vec<_>>();
        for message in messages {
            let card = self.tree.insert(
                Some(parent),
                Container::column()
                    .padding(space(14.0))
                    .gap(space(9.0))
                    .background(if message.role == ember::model::Role::User {
                        Role::Surface
                    } else {
                        Role::Background
                    })
                    .radius(9.0),
            );
            label(
                &mut self.tree,
                card,
                format!("{}  ·  {}", message.role.label(), message.scope.label()),
                12.0,
                true,
            );
            label(
                &mut self.tree,
                card,
                excerpt(&message.content, 1800),
                14.0,
                false,
            );
            let actions = row(&mut self.tree, card);
            button(
                &mut self.tree,
                actions,
                "全文を表示",
                Action::ReadMessage(message.id.clone()),
                &mut self.actions,
            );
            button(
                &mut self.tree,
                actions,
                "記憶に保存…",
                Action::Remember(message.id),
                &mut self.actions,
            );
        }
        if let Some(id) = self.active_conversation()
            && let Some(run) = self.view.state.runs.iter().find(|run| run.id == id)
        {
            label(
                &mut self.tree,
                parent,
                format!("エンバー · {}", run.status.label()),
                12.0,
                true,
            );
        }
        let latest = self
            .view
            .state
            .runs
            .iter()
            .rev()
            .find(|run| run.kind == RunKind::Conversation && run.scope == self.scope);
        if let Some(run) = latest
            && let Some(error) = &run.error
        {
            label(
                &mut self.tree,
                parent,
                format!("{}：{}", run.status.label(), excerpt(error, 500)),
                12.0,
                true,
            );
        }
        if let Some(run) = latest {
            if run.status == RunStatus::Canceled {
                label(
                    &mut self.tree,
                    parent,
                    "ローカルの応答受信を停止しました。API側の処理・利用料金は未確認です。",
                    12.0,
                    true,
                );
            } else if run.status == RunStatus::Uncertain {
                label(
                    &mut self.tree,
                    parent,
                    "結果が確定していないため、自動では同じ処理をやり直しません。",
                    12.0,
                    true,
                );
            }
        }
        let recent = self
            .view
            .state
            .runs
            .iter()
            .rev()
            .find(|run| {
                run.kind != RunKind::Conversation && !run.archived && run.scope == self.scope
            })
            .cloned();
        if let Some(run) = recent {
            let card = self.tree.insert(
                Some(parent),
                Container::column()
                    .padding(space(15.0))
                    .gap(space(8.0))
                    .background(Role::Surface)
                    .border(1.0, Role::Separator)
                    .radius(9.0),
            );
            label(
                &mut self.tree,
                card,
                format!("{} · {}", run.kind.label(), run.status.label()),
                12.0,
                true,
            );
            label(
                &mut self.tree,
                card,
                excerpt(&run.objective, 100),
                14.0,
                false,
            );
            let controls = row(&mut self.tree, card);
            if run.artifact.is_some() {
                button(
                    &mut self.tree,
                    controls,
                    "成果物を見る",
                    Action::Artifact(run.id),
                    &mut self.actions,
                );
            }
            button(
                &mut self.tree,
                controls,
                "依頼の詳細",
                Action::Page(Page::Requests),
                &mut self.actions,
            );
        }
    }

    fn requests(&mut self) {
        let runs = self
            .view
            .state
            .runs
            .iter()
            .rev()
            .filter(|run| {
                run.kind != RunKind::Conversation && (self.show_archived || !run.archived)
            })
            .cloned()
            .collect::<Vec<_>>();
        if runs.is_empty() {
            label(
                &mut self.tree,
                self.ids.request_list,
                "まだ依頼はありません。目的を入力して、文書や調査を任せられます。",
                14.0,
                true,
            );
        }
        for run in runs {
            let card = self.tree.insert(
                Some(self.ids.request_list),
                Container::column()
                    .padding(space(15.0))
                    .gap(space(9.0))
                    .background(Role::Surface)
                    .border(1.0, Role::Separator)
                    .radius(9.0),
            );
            label(
                &mut self.tree,
                card,
                format!(
                    "{} · {} · {}{}",
                    run.scope.label(),
                    run.kind.label(),
                    run.status.label(),
                    if run.archived { " · 保管済み" } else { "" }
                ),
                12.0,
                true,
            );
            label(
                &mut self.tree,
                card,
                excerpt(&run.objective, 180),
                14.0,
                false,
            );
            if let Some(error) = &run.error {
                label(&mut self.tree, card, excerpt(error, 350), 12.0, true);
            }
            if run.status == RunStatus::Uncertain {
                label(
                    &mut self.tree,
                    card,
                    "結果が確定していないため、自動では同じ処理をやり直しません。",
                    12.0,
                    true,
                );
            }
            if run.status == RunStatus::Canceled {
                label(
                    &mut self.tree,
                    card,
                    "ローカルの応答受信を停止しました。API側の処理・利用料金は未確認です。",
                    12.0,
                    true,
                );
            }
            let controls = row(&mut self.tree, card);
            if run.artifact.is_some() {
                button(
                    &mut self.tree,
                    controls,
                    "成果物を見る",
                    Action::Artifact(run.id.clone()),
                    &mut self.actions,
                );
            }
            if !run.status.terminal() && run.status != RunStatus::CancelRequested {
                button(
                    &mut self.tree,
                    controls,
                    "停止",
                    Action::Cancel(run.id.clone()),
                    &mut self.actions,
                );
            }
            if run.status == RunStatus::Completed && matches!(run.scope, Scope::Project { .. }) {
                button(
                    &mut self.tree,
                    controls,
                    "開発の引継ぎを準備",
                    Action::PrepareHandoff(run.id.clone()),
                    &mut self.actions,
                );
            }
            if run.status.terminal() {
                button(
                    &mut self.tree,
                    controls,
                    if run.archived {
                        "一覧に戻す"
                    } else {
                        "保管"
                    },
                    Action::Archive(run.id.clone(), !run.archived),
                    &mut self.actions,
                );
                button(
                    &mut self.tree,
                    controls,
                    "削除…",
                    Action::DeleteRun(run.id),
                    &mut self.actions,
                );
            }
        }
    }

    fn memories(&mut self) {
        let entries = self
            .view
            .state
            .memories
            .iter()
            .filter(|memory| memory.scope == Scope::Global || memory.scope == self.scope)
            .cloned()
            .collect::<Vec<_>>();
        if entries.is_empty() {
            label(
                &mut self.tree,
                self.ids.memory_list,
                "この範囲で使う記憶はありません。会話の「記憶に保存…」から内容を選ぶこともできます。",
                14.0,
                true,
            );
        }
        for memory in entries {
            let card = self.tree.insert(
                Some(self.ids.memory_list),
                Container::column()
                    .padding(space(14.0))
                    .gap(space(8.0))
                    .background(Role::Surface)
                    .radius(8.0),
            );
            label(
                &mut self.tree,
                card,
                scope_description(&memory.scope),
                11.0,
                true,
            );
            label(&mut self.tree, card, visible(&memory.text), 14.0, false);
            let controls = row(&mut self.tree, card);
            button(
                &mut self.tree,
                controls,
                "編集",
                Action::EditMemory(memory.id.clone()),
                &mut self.actions,
            );
            button(
                &mut self.tree,
                controls,
                "削除",
                Action::DeleteMemory(memory.id),
                &mut self.actions,
            );
        }
    }
}
