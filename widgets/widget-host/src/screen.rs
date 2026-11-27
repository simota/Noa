//! Canonical instance data only changes on a validated parent snapshot.
//! Button clicks are requests, not optimistic data mutations.

use noa_gpui_core::widgets::{
    Button, ButtonClicked, Container, Label, ScrollView, Tag, TextSubmitted,
};
use noa_gpui_core::{Appearance, Event, Key, Message, Role, Style, Theme, UiTree, WidgetId};
use noa_gpui_platform::Screen;
use noa_gpui_scene::ImageId;
use noa_gpui_text::TextStore;
use noa_ipc::widget::{HostMessage, HostRequest, OperationState, WidgetOperation};
use noa_widget::{Asset, Manifest};
use serde_json::Value;

use crate::appearance::{font, space};
use crate::bridge::{Bridge, Update};
use crate::images::Images;
use crate::render::{self, Action};
use crate::style;

struct Instance {
    id: String,
    digest: String,
    revision: u64,
    pending: Option<Pending>,
    nonce: String,
    next_action: u64,
    valid: bool,
}

struct Pending {
    request_id: String,
    request: HostRequest,
    uncertain: bool,
    /// Inputs to clear once the service accepts the event: (node id, the
    /// text that was sent). Text typed since then is kept.
    clear: Vec<(String, String)>,
}

struct PreviewFixtures {
    values: Vec<noa_widget::discovery::Fixture>,
    selected: usize,
    previous: WidgetId,
    next: WidgetId,
    label: WidgetId,
}

struct OperationUi {
    summary: WidgetId,
    context: WidgetId,
    toggle: WidgetId,
    scroll: WidgetId,
    list: WidgetId,
    expanded: bool,
    operations: Vec<WidgetOperation>,
    total: usize,
}

pub struct WidgetScreen {
    tree: UiTree,
    body: WidgetId,
    status: WidgetId,
    footer: WidgetId,
    retry: Option<WidgetId>,
    version: WidgetId,
    manifest: Manifest,
    manifest_value: Value,
    rendered: render::Node,
    data: Value,
    preview: Option<PreviewFixtures>,
    actions: Vec<(WidgetId, Action)>,
    inputs: render::Inputs,
    preview_events: Vec<Action>,
    instance: Option<Instance>,
    bridge: Option<Bridge>,
    close: bool,
    images: Images,
    operation_ui: Option<OperationUi>,
}

impl WidgetScreen {
    pub fn preview(manifest: Manifest, data: Value) -> Result<Self, String> {
        Self::new(manifest, data, None, &[])
    }

    /// An offline preview of a package, with its images read by noa-widget.
    pub fn preview_package(
        manifest: Manifest,
        data: Value,
        assets: &[Asset],
    ) -> Result<Self, String> {
        Self::new(manifest, data, None, assets)
    }

    pub fn from_snapshot(snapshot: HostMessage) -> Result<Self, String> {
        Self::from_bridge(snapshot, &[])
    }

    /// A live instance: the snapshot's definition digest must cover both
    /// the manifest and exactly the asset bytes received before it.
    pub fn from_bridge(snapshot: HostMessage, assets: &[Asset]) -> Result<Self, String> {
        let HostMessage::Snapshot {
            instance_id,
            definition_digest,
            data_revision,
            manifest,
            data,
        } = snapshot
        else {
            return Err("live widget needs an initial snapshot".into());
        };
        let manifest = parse_manifest(&manifest)?;
        let asset_digests = noa_widget::assets::digests(assets);
        if noa_widget::definition_digest(&manifest, &asset_digests)
            .map_err(|error| error.to_string())?
            != definition_digest
        {
            return Err("snapshot definition digest does not match its manifest and assets".into());
        }
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|error| error.to_string())?
            .as_nanos();
        let instance = Instance {
            id: instance_id,
            digest: definition_digest,
            revision: data_revision,
            pending: None,
            nonce: format!("ui-{}-{nanos}", std::process::id()),
            next_action: 0,
            valid: true,
        };
        Self::new(manifest, data, Some(instance), assets)
    }

    fn new(
        manifest: Manifest,
        data: Value,
        instance: Option<Instance>,
        assets: &[Asset],
    ) -> Result<Self, String> {
        manifest
            .validate_data(&data)
            .map_err(|error| error.to_string())?;
        let images = Images::new(assets, &manifest.view);
        let mut view = render::resolve(&manifest.view, &data)?;
        images.attach(&mut view);
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
            Container::new(style::wide(style::column(6.0)))
                .background(Role::Surface)
                .padding(space(20.0)),
        );
        let title = tree.insert(
            Some(header),
            Container::new(style::wide(style::row(12.0))).wrap(true),
        );
        tree.insert(
            Some(title),
            Label::new(&manifest.title)
                .font_size(font(20.0))
                .weight(700)
                .max_lines(3),
        );
        tree.insert(
            Some(title),
            Tag::new(if instance.is_some() {
                "ライブ"
            } else {
                "プレビュー"
            }),
        );
        tree.insert(
            Some(header),
            Label::new(&manifest.description)
                .font_size(font(12.0))
                .color(Role::Muted)
                .max_lines(3),
        );
        let version = tree.insert(
            Some(header),
            Label::new(version_label(&manifest, instance.as_ref()))
                .font_size(font(11.0))
                .color(Role::Muted),
        );
        let scroll = tree.insert(Some(root), ScrollView::new(style::fill()));
        let body = tree.insert(
            Some(scroll),
            Container::new(style::wide(style::column(16.0))).padding(space(20.0)),
        );
        let mut actions = Vec::new();
        let mut inputs = render::Inputs::default();
        render::insert(&mut tree, body, &view, &mut actions, &mut inputs);
        let footer = tree.insert(
            Some(root),
            Container::new(style::wide(style::column(8.0)))
                .padding(space(14.0))
                .background(Role::Surface),
        );
        let status = tree.insert(
            Some(footer),
            Label::new(match images.failures().first() {
                Some(error) => format!("画像を表示できません: {error}"),
                None if instance.is_some() => "操作は担当エージェントへ送信されます。".into(),
                None => "プレビューでは操作のイベントを表示します。処理は実行されません。".into(),
            })
            .font_size(font(12.0))
            .color(Role::Muted)
            .max_lines(4),
        );
        let operation_ui = instance.as_ref().map(|_| {
            let context = tree.insert(
                Some(footer),
                Label::new("更新情報を取得しています…")
                    .font_size(font(11.0))
                    .color(Role::Muted)
                    .max_lines(2),
            );
            let summary = tree.insert(
                Some(footer),
                Label::new("操作履歴はありません")
                    .font_size(font(13.0))
                    .max_lines(3),
            );
            let toggle = tree.insert(Some(footer), Button::new("操作履歴を表示"));
            let scroll = tree.insert(
                Some(footer),
                ScrollView::new(Style {
                    size: noa_gpui_core::style::Size {
                        width: noa_gpui_core::style::percent(1.0),
                        height: noa_gpui_core::style::length(150.0),
                    },
                    ..style::column(0.0)
                }),
            );
            let list = tree.insert(
                Some(scroll),
                Container::new(style::wide(style::column(8.0))),
            );
            tree.set_hidden(scroll, true);
            OperationUi {
                summary,
                context,
                toggle,
                scroll,
                list,
                expanded: false,
                operations: Vec::new(),
                total: 0,
            }
        });
        Ok(Self {
            tree,
            body,
            status,
            footer,
            retry: None,
            version,
            manifest_value: serde_json::to_value(&manifest).expect("validated manifest"),
            rendered: view,
            manifest,
            data,
            preview: None,
            actions,
            inputs,
            preview_events: Vec::new(),
            instance,
            bridge: None,
            close: false,
            images,
            operation_ui,
        })
    }

    /// Upload the decoded images. Call once, where the screen is set up
    /// (the app's `Setup`, or a headless renderer before a snapshot).
    pub fn upload_images(&mut self, create: &mut dyn FnMut(u32, u32, &[u8]) -> ImageId) {
        self.images.upload(create);
    }

    pub fn attach_bridge(&mut self, bridge: Bridge) {
        self.bridge = Some(bridge);
    }

    pub fn title(&self) -> &str {
        &self.manifest.title
    }

    pub fn preview_events(&self) -> &[Action] {
        &self.preview_events
    }

    fn notice(&mut self, notice: impl Into<String>) {
        let notice = notice.into();
        self.tree
            .update::<Label, _>(self.status, |label, cx| label.set_text(cx, notice));
    }

    fn update_buttons(&mut self) {
        let enabled = !self.close
            && self
                .instance
                .as_ref()
                .is_none_or(|instance| instance.valid && instance.pending.is_none());
        for (button, _) in &self.actions {
            self.tree.set_enabled(*button, enabled);
        }
        if let Some(retry) = self.retry {
            let enabled = !self.close
                && self.instance.as_ref().is_some_and(|instance| {
                    instance.valid
                        && instance
                            .pending
                            .as_ref()
                            .is_some_and(|pending| pending.uncertain)
                });
            self.tree.set_enabled(retry, enabled);
        }
    }

    fn apply_snapshot(
        &mut self,
        snapshot: HostMessage,
        text: &mut TextStore,
    ) -> Result<(), String> {
        let HostMessage::Snapshot {
            instance_id,
            definition_digest,
            data_revision,
            manifest,
            data,
        } = snapshot
        else {
            return Err("expected a snapshot".into());
        };
        let instance = self
            .instance
            .as_ref()
            .ok_or("preview cannot become a live instance")?;
        if instance.id != instance_id || instance.digest != definition_digest {
            return Err("the live widget's pinned identity changed".into());
        }
        // The definition is pinned and already validated at bootstrap. A data
        // update need not parse and validate the entire manifest again.
        if manifest != self.manifest_value {
            return Err("the live widget's pinned definition changed".into());
        }
        if data_revision < instance.revision
            || (data_revision == instance.revision && data != self.data)
        {
            return Err(
                "snapshot data revision moved backwards or changed without a revision".into(),
            );
        }
        self.manifest
            .validate_data(&data)
            .map_err(|error| error.to_string())?;
        let mut view = render::resolve(&self.manifest.view, &data)?;
        self.images.attach(&mut view);
        self.replace_view(view, text);
        self.data = data;
        let instance = self.instance.as_mut().expect("checked above");
        instance.revision = data_revision;
        instance.valid = true;
        let label = version_label(&self.manifest, Some(instance));
        self.tree
            .update::<Label, _>(self.version, |view, cx| view.set_text(cx, label));
        self.update_buttons();
        Ok(())
    }

    fn replace_view(&mut self, view: render::Node, text: &mut TextStore) {
        if view != self.rendered {
            // Typed text is not instance data. Move each input widget itself
            // into the rebuilt view by node id, so its caret, selection and
            // any IME composition in progress survive with the text.
            let focused = self.tree.focused().and_then(|focus| {
                self.inputs
                    .focus_nodes
                    .iter()
                    .find(|(_, widget)| *widget == focus)
                    .map(|(id, _)| id.clone())
            });
            let mut kept: Vec<_> = self
                .inputs
                .fields
                .iter()
                .filter_map(|field| {
                    self.tree
                        .take_subtree(field.widget)
                        .map(|subtree| (field.id.clone(), subtree))
                })
                .collect();
            for child in self.tree.children(self.body).to_vec() {
                self.tree.remove(child, text);
            }
            self.actions.clear();
            self.inputs = render::Inputs::default();
            render::insert(
                &mut self.tree,
                self.body,
                &view,
                &mut self.actions,
                &mut self.inputs,
            );
            for field in &mut self.inputs.fields {
                let Some(at) = kept.iter().position(|(id, _)| *id == field.id) else {
                    continue;
                };
                let (_, subtree) = kept.swap_remove(at);
                let parent = self
                    .tree
                    .parent(field.widget)
                    .expect("an input widget has a container");
                let index = self
                    .tree
                    .children(parent)
                    .iter()
                    .position(|&child| child == field.widget)
                    .expect("a child of its parent");
                self.tree.remove(field.widget, text);
                let replacement = self.tree.insert_subtree(Some(parent), subtree);
                for (_, widget) in &mut self.inputs.focus_nodes {
                    if *widget == field.widget {
                        *widget = replacement;
                    }
                }
                field.widget = replacement;
                self.tree.move_child(field.widget, index);
            }
            if let Some((_, widget)) = self
                .inputs
                .focus_nodes
                .iter()
                .find(|(id, _)| focused.as_ref() == Some(id))
            {
                self.tree.set_focus(Some(*widget));
            }
            self.rendered = view;
        }
    }

    /// Fixture controls only exist in an explicitly offline preview. Their
    /// contents are validated data; switching never contacts a producer.
    pub fn with_fixtures(mut self, fixtures: noa_widget::discovery::Fixtures) -> Self {
        if self.instance.is_some() {
            return self;
        }
        let mut values = vec![noa_widget::discovery::Fixture {
            name: "初期データ".into(),
            data: self.data.clone(),
        }];
        values.extend(fixtures.values);
        let toolbar = self.tree.insert(
            Some(self.footer),
            Container::new(style::wide(style::row(8.0))).wrap(true),
        );
        let previous = self.tree.insert(Some(toolbar), Button::new("前の例"));
        let next = self.tree.insert(Some(toolbar), Button::new("次の例"));
        let label = self.tree.insert(
            Some(self.footer),
            Label::new(format!("初期データ · 1 / {}", values.len()))
                .font_size(font(12.0))
                .max_lines(2),
        );
        self.tree.set_enabled(previous, false);
        self.tree.set_enabled(next, values.len() > 1);
        self.preview = Some(PreviewFixtures {
            values,
            selected: 0,
            previous,
            next,
            label,
        });
        if !fixtures.diagnostics.is_empty() {
            self.notice(format!(
                "{} 件のフィクスチャを読み込めません: {}",
                fixtures.diagnostics.len(),
                fixtures
                    .diagnostics
                    .iter()
                    .map(|item| item.message.as_str())
                    .collect::<Vec<_>>()
                    .join("; ")
            ));
        }
        self
    }

    fn switch_fixture(&mut self, forward: bool, text: &mut TextStore) {
        let Some(preview) = self.preview.as_mut() else {
            return;
        };
        let selected = if forward {
            (preview.selected + 1).min(preview.values.len() - 1)
        } else {
            preview.selected.saturating_sub(1)
        };
        let data = preview.values[selected].data.clone();
        let name = preview.values[selected].name.clone();
        let mut view = match self
            .manifest
            .validate_data(&data)
            .map_err(|error| error.to_string())
            .and_then(|()| render::resolve(&self.manifest.view, &data))
        {
            Ok(view) => view,
            Err(error) => {
                self.notice(format!("フィクスチャを表示できません: {error}"));
                return;
            }
        };
        self.images.attach(&mut view);
        preview.selected = selected;
        let label = preview.label;
        let label_text = format!("{} · {} / {}", name, selected + 1, preview.values.len());
        self.tree.set_enabled(preview.previous, selected > 0);
        self.tree
            .set_enabled(preview.next, selected + 1 < preview.values.len());
        self.tree
            .update::<Label, _>(label, |label, cx| label.set_text(cx, label_text));
        self.replace_view(view, text);
        self.data = data;
        self.notice(format!("フィクスチャ: {name}。処理は実行されません。"));
    }

    fn receive(&mut self, update: Update, text: &mut TextStore) {
        match update {
            Update::Message(HostMessage::Status {
                instance_id,
                data_updated_at_ms,
                owner_pane_id,
                operations,
                total_operations,
                pending_operations,
            }) => {
                if self
                    .instance
                    .as_ref()
                    .is_none_or(|instance| instance.id != instance_id)
                    || operations.len() > 32
                    || total_operations < operations.len()
                    || pending_operations > total_operations
                {
                    self.notice("処理状況の識別情報が一致しません。最新の表示を確認してください。");
                    return;
                }
                let Some(ui) = &mut self.operation_ui else {
                    return;
                };
                // A delayed observation may not overwrite a newer report of
                // the same operation. Data updates never change this state.
                if total_operations < ui.total
                    || operations.iter().any(|operation| {
                        ui.operations.iter().any(|old| {
                            old.event_id == operation.event_id && old.version > operation.version
                        })
                    })
                {
                    return;
                }
                let summary = match operations.first() {
                    Some(operation) => format!(
                        "#{} · {} · {}　未完了 {} 件",
                        operation.seq,
                        operation_label(operation.state),
                        operation.action_id,
                        pending_operations
                    ),
                    None => "操作履歴はありません".into(),
                };
                let context = format!(
                    "データ更新: {}{}",
                    local_time(data_updated_at_ms),
                    owner_pane_id
                        .map(|id| format!(" · 担当ペイン {}", id.0))
                        .unwrap_or_default()
                );
                self.tree
                    .update::<Label, _>(ui.summary, |label, cx| label.set_text(cx, summary));
                self.tree
                    .update::<Label, _>(ui.context, |label, cx| label.set_text(cx, context));
                for child in self.tree.children(ui.list).to_vec() {
                    self.tree.remove(child, text);
                }
                self.tree.insert(
                    Some(ui.list),
                    Label::new(format!(
                        "直近 {} / {} 件。処理状況は担当エージェントの報告です。",
                        operations.len(),
                        total_operations
                    ))
                    .font_size(font(11.0))
                    .color(Role::Muted)
                    .max_lines(3),
                );
                for operation in &operations {
                    let message = if operation.message.is_empty() {
                        String::new()
                    } else {
                        format!("\n{}", operation.message)
                    };
                    self.tree.insert(
                        Some(ui.list),
                        Label::new(format!(
                            "#{} · {} · {} · {}{}",
                            operation.seq,
                            operation_label(operation.state),
                            operation.action_id,
                            local_time(operation.updated_at_ms),
                            message
                        ))
                        .font_size(font(12.0))
                        .max_lines(6),
                    );
                }
                ui.operations = operations;
                ui.total = total_operations;
                // A durable operation carrying this UI request ID proves
                // event receipt even if the earlier receipt was uncertain.
                // It still says nothing about processing beyond its explicit
                // state above; unrelated operations cannot clear the request.
                let confirmed = self.instance.as_ref().and_then(|instance| {
                    instance.pending.as_ref().and_then(|pending| {
                        ui.operations
                            .iter()
                            .any(|operation| {
                                operation.action_request_id.as_deref()
                                    == Some(pending.request_id.as_str())
                            })
                            .then(|| pending.request_id.clone())
                    })
                });
                if let Some(request_id) = confirmed {
                    self.receive(
                        Update::Message(HostMessage::ActionResult {
                            request_id,
                            ok: true,
                            error: None,
                            error_kind: None,
                        }),
                        text,
                    );
                }
            }
            Update::Message(snapshot @ HostMessage::Snapshot { .. }) => {
                if let Err(error) = self.apply_snapshot(snapshot, text) {
                    if let Some(instance) = &mut self.instance {
                        instance.valid = false;
                    }
                    self.notice(format!("更新を表示できません: {error}"));
                    self.update_buttons();
                }
            }
            Update::Message(HostMessage::ActionResult {
                request_id,
                ok,
                error,
                error_kind,
            }) => {
                let Some(instance) = &mut self.instance else {
                    return;
                };
                if instance
                    .pending
                    .as_ref()
                    .map(|pending| pending.request_id.as_str())
                    != Some(&request_id)
                {
                    return;
                }
                if !ok && error_kind.as_deref() == Some("outcome_unknown") {
                    // The event may already be durable. A new click must not
                    // turn this uncertain receipt into a second operation.
                    instance.pending.as_mut().expect("matched above").uncertain = true;
                    self.notice(
                        "操作の受付をまだ確認できません。同じ操作の受付を再確認してください。",
                    );
                    if self.retry.is_none() {
                        self.retry = Some(
                            self.tree
                                .insert(Some(self.footer), Button::new("受付を再確認")),
                        );
                    }
                    self.update_buttons();
                    return;
                }
                let pending = instance.pending.take().expect("matched above");
                if let Some(button) = self.retry.take() {
                    self.tree.remove(button, text);
                }
                if ok {
                    self.clear_inputs(&pending.clear);
                }
                self.notice(if ok {
                    "操作を受け付けました。処理状況は操作履歴に表示されます。".into()
                } else {
                    format!(
                        "操作を送信できません: {}",
                        error.unwrap_or_else(|| "理由不明".into())
                    )
                });
                self.update_buttons();
            }
            Update::Message(HostMessage::Asset { .. }) => {
                // Images are pinned with the initial snapshot; a later frame
                // is a protocol error, never a replacement image.
                if let Some(instance) = &mut self.instance {
                    instance.valid = false;
                }
                self.notice("更新を表示できません: 画像は初期スナップショットでのみ受け付けます");
                self.update_buttons();
            }
            Update::Message(HostMessage::Closed) => {
                self.close = true;
                self.update_buttons();
            }
            Update::Failed(error) => {
                if let Some(instance) = &mut self.instance {
                    instance.valid = false;
                }
                self.notice(format!("接続が利用できません: {error}"));
                self.update_buttons();
            }
        }
    }

    fn activate(&mut self, action: Action) {
        self.activate_with(action, Vec::new());
    }

    /// Send a button's event with its inputs' current text inserted into the
    /// payload. The text is only a payload string; nothing evaluates it.
    fn submit_form(&mut self, button: &str) {
        let Some(form) = self.inputs.buttons.iter().find(|form| form.id == button) else {
            return;
        };
        let Some((_, action)) = self.actions.iter().find(|(id, _)| *id == form.widget) else {
            return;
        };
        if !self.tree.is_enabled(form.widget) {
            return;
        }
        let mut action = action.clone();
        let payload = action
            .payload
            .as_object_mut()
            .expect("validated button payloads are objects");
        let mut sent = Vec::new();
        for (property, input) in &form.inputs {
            let field = self
                .inputs
                .fields
                .iter()
                .find(|field| field.id == *input)
                .expect("validated inputs name text-input nodes");
            let value = field.text(&self.tree);
            let count = value.chars().count();
            if let Some(max) = field.max_length
                && count > max
            {
                self.notice(format!(
                    "入力 {input} は {max} 文字以内にしてください（現在 {count} 文字）。送信していません。"
                ));
                return;
            }
            payload.insert(property.clone(), Value::String(value.clone()));
            sent.push((input.clone(), value));
        }
        let clear = if form.clear { sent } else { Vec::new() };
        self.activate_with(action, clear);
    }

    fn clear_inputs(&mut self, sent: &[(String, String)]) {
        for (input, value) in sent {
            if let Some(field) = self.inputs.fields.iter().find(|field| field.id == *input)
                && field.text(&self.tree) == *value
            {
                field.clone().set_text(&mut self.tree, String::new());
            }
        }
    }

    /// The submit button of the focused input, unless an IME composition is
    /// in progress: Enter then confirms the conversion instead of sending.
    fn focused_submit(&self, multiline: bool) -> Option<String> {
        let focus = self.tree.focused()?;
        let field = self
            .inputs
            .fields
            .iter()
            .find(|field| field.widget == focus && field.multiline == multiline)?;
        if field.composing(&self.tree) {
            return None;
        }
        field.submit.clone()
    }

    fn activate_with(&mut self, action: Action, clear: Vec<(String, String)>) {
        if let Err(error) = self.manifest.validate_action(&action.id, &action.payload) {
            self.notice(format!("操作の入力が不正です: {error}"));
            return;
        }
        let Some(instance) = &mut self.instance else {
            if self.preview_events.len() == 32 {
                self.preview_events.remove(0);
            }
            self.preview_events.push(action.clone());
            let payload: String = action.payload.to_string().chars().take(300).collect();
            self.notice(format!(
                "プレビューイベント: {} {payload}（実行していません）",
                action.id
            ));
            self.clear_inputs(&clear);
            return;
        };
        if !instance.valid || instance.pending.is_some() || self.close {
            return;
        }
        instance.next_action += 1;
        let request_id = format!("{}-{}", instance.nonce, instance.next_action);
        let request = HostRequest::Action {
            request_id: request_id.clone(),
            action_id: action.id,
            data_revision: instance.revision,
            payload: action.payload,
        };
        let sent = self
            .bridge
            .as_ref()
            .ok_or_else(|| "bridge is unavailable".to_owned())
            .and_then(|bridge| bridge.send(request.clone()));
        match sent {
            Ok(()) => {
                instance.pending = Some(Pending {
                    request_id,
                    request,
                    uncertain: false,
                    clear,
                });
                self.notice("操作を送信しています…");
            }
            Err(error) => self.notice(format!("操作を送信できません: {error}")),
        }
        self.update_buttons();
    }

    fn retry_pending(&mut self) {
        let Some(instance) = &mut self.instance else {
            return;
        };
        let Some(pending) = &mut instance.pending else {
            return;
        };
        if !pending.uncertain || !instance.valid || self.close {
            return;
        }
        let sent = self
            .bridge
            .as_ref()
            .ok_or_else(|| "bridge is unavailable".to_owned())
            .and_then(|bridge| bridge.send(pending.request.clone()));
        match sent {
            Ok(()) => {
                pending.uncertain = false;
                self.notice("同じ操作の受付を再確認しています…");
            }
            Err(error) => self.notice(format!("受付を再確認できません: {error}")),
        }
        self.update_buttons();
    }
}

fn parse_manifest(value: &Value) -> Result<Manifest, String> {
    Manifest::from_slice(&serde_json::to_vec(value).map_err(|error| error.to_string())?)
        .map_err(|error| error.to_string())
}

fn operation_label(state: OperationState) -> &'static str {
    match state {
        OperationState::Accepted => "受付済み・応答待ち",
        OperationState::Running => "処理中",
        OperationState::Waiting => "応答待ち",
        OperationState::Completed => "完了",
        OperationState::Failed => "失敗",
    }
}

fn local_time(timestamp: Option<u64>) -> String {
    let Some(timestamp) = timestamp else {
        return "日時不明".into();
    };
    let Ok(seconds) = libc::time_t::try_from(timestamp / 1000) else {
        return "日時不明".into();
    };
    let mut value = std::mem::MaybeUninit::<libc::tm>::uninit();
    // SAFETY: both pointers refer to valid storage, and localtime_r writes a
    // complete tm on success without retaining either pointer.
    if unsafe { libc::localtime_r(&seconds, value.as_mut_ptr()) }.is_null() {
        return "日時不明".into();
    }
    // SAFETY: localtime_r succeeded and initialized value.
    let value = unsafe { value.assume_init() };
    format!(
        "{:04}/{:02}/{:02} {:02}:{:02}:{:02}",
        value.tm_year + 1900,
        value.tm_mon + 1,
        value.tm_mday,
        value.tm_hour,
        value.tm_min,
        value.tm_sec
    )
}

fn version_label(manifest: &Manifest, instance: Option<&Instance>) -> String {
    match instance {
        Some(instance) => format!(
            "{} · v{} · revision {}",
            manifest.id, manifest.version, instance.revision
        ),
        None => format!("{} · v{}", manifest.id, manifest.version),
    }
}

impl Screen for WidgetScreen {
    fn set_system_appearance(&mut self, appearance: Appearance) {
        crate::appearance::system(&mut self.tree, appearance);
        render::refresh_tones(&mut self.tree, &self.inputs);
    }

    fn tree(&self) -> &UiTree {
        &self.tree
    }
    fn tree_mut(&mut self) -> &mut UiTree {
        &mut self.tree
    }

    fn handle(&mut self, message: Message, text: &mut TextStore) {
        if message.downcast_ref::<ButtonClicked>().is_some()
            && let Some(ui) = &mut self.operation_ui
            && message.source == ui.toggle
        {
            ui.expanded = !ui.expanded;
            self.tree.set_hidden(ui.scroll, !ui.expanded);
            self.tree.update::<Button, _>(ui.toggle, |button, cx| {
                button.set_label(
                    cx,
                    if ui.expanded {
                        "操作履歴を閉じる"
                    } else {
                        "操作履歴を表示"
                    },
                );
            });
            return;
        }
        if message.downcast_ref::<ButtonClicked>().is_some()
            && let Some(preview) = &self.preview
            && (message.source == preview.previous || message.source == preview.next)
        {
            self.switch_fixture(message.source == preview.next, text);
            return;
        }
        if message.downcast_ref::<ButtonClicked>().is_some() && self.retry == Some(message.source) {
            self.retry_pending();
            return;
        }
        if message.downcast_ref::<ButtonClicked>().is_some()
            && let Some(form) = self
                .inputs
                .buttons
                .iter()
                .find(|form| form.widget == message.source)
        {
            self.submit_form(&form.id.clone());
            return;
        }
        if message.downcast_ref::<ButtonClicked>().is_some()
            && let Some((_, action)) = self.actions.iter().find(|(id, _)| *id == message.source)
        {
            self.activate(action.clone());
        }
        // A single-line field emits TextSubmitted on Return only when no IME
        // composition is in progress.
        if message.downcast_ref::<TextSubmitted>().is_some()
            && self.tree.focused() == Some(message.source)
            && let Some(button) = self.focused_submit(false)
        {
            self.submit_form(&button);
        }
    }

    fn dispatch(&mut self, event: Event, text: &mut TextStore) {
        // A multi-line input inserts a line break on Return, Cmd+Return too,
        // so the submit shortcut is taken before the event reaches it.
        if let Event::KeyDown {
            key: Key::Enter,
            modifiers,
        } = &event
            && modifiers.command
            && let Some(button) = self.focused_submit(true)
        {
            self.submit_form(&button);
            self.after_input();
            return;
        }
        self.tree.dispatch(event, text);
        for message in self.tree.take_messages() {
            self.handle(message, text);
        }
        self.after_input();
    }

    fn woken(&mut self, text: &mut TextStore) {
        let updates = self.bridge.as_ref().map(Bridge::drain).unwrap_or_default();
        for update in updates {
            self.receive(update, text);
        }
    }

    fn wants_close(&self) -> bool {
        self.close
    }

    fn close_requested(&mut self) {
        if let Some(bridge) = &self.bridge {
            bridge.closed();
        }
    }
}

#[cfg(test)]
mod text_input_tests;

#[cfg(test)]
mod usability_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use noa_gpui_core::widgets::Button;
    use noa_gpui_core::{Event, Key, Modifiers};
    use serde_json::json;
    use std::path::Path;

    fn manifest() -> Manifest {
        Manifest::from_slice(include_bytes!(
            "../../../.noa-widgets/test-summary/widget.json"
        ))
        .unwrap()
    }

    fn snapshot(manifest: &Manifest, revision: u64, data: Value) -> HostMessage {
        HostMessage::Snapshot {
            instance_id: "wi_0123456789abcdef0123456789abcdef".into(),
            definition_digest: manifest.digest(),
            data_revision: revision,
            manifest: serde_json::to_value(manifest).unwrap(),
            data,
        }
    }

    fn labels(tree: &UiTree, parent: WidgetId) -> Vec<String> {
        let own = tree
            .get::<Label>(parent)
            .map(|label| label.text().to_owned())
            .or_else(|| tree.get::<Tag>(parent).map(|tag| tag.text().to_owned()))
            .or_else(|| {
                tree.get::<Button>(parent)
                    .map(|button| button.label().to_owned())
            });
        own.into_iter()
            .chain(
                tree.children(parent)
                    .iter()
                    .flat_map(|child| labels(tree, *child)),
            )
            .collect()
    }

    #[test]
    fn preview_button_records_an_event_and_does_not_change_data() {
        let manifest = manifest();
        let original = manifest.initial_data.clone();
        let mut screen = WidgetScreen::preview(manifest, original.clone()).unwrap();
        let button = screen.actions[0].0;
        screen.tree.set_focus(Some(button));
        let mut text = TextStore::new(Default::default());
        screen.dispatch(
            Event::KeyDown {
                key: Key::Space,
                modifiers: Modifiers::default(),
            },
            &mut text,
        );
        assert_eq!(
            screen.preview_events(),
            &[Action {
                id: "rerun-failed".into(),
                payload: json!({})
            }]
        );
        assert_eq!(screen.data, original);
    }

    #[test]
    fn invalid_or_replaced_live_snapshots_preserve_the_complete_old_view() {
        let manifest = manifest();
        let initial = manifest.initial_data.clone();
        let mut screen =
            WidgetScreen::from_snapshot(snapshot(&manifest, 0, initial.clone())).unwrap();
        let before = labels(&screen.tree, screen.body);
        let mut text = TextStore::new(Default::default());
        assert!(
            screen
                .apply_snapshot(snapshot(&manifest, 1, json!({})), &mut text)
                .is_err()
        );
        assert_eq!(labels(&screen.tree, screen.body), before);
        let mut changed = manifest.clone();
        changed.title = "Replacement".into();
        assert!(
            screen
                .apply_snapshot(snapshot(&changed, 1, initial), &mut text)
                .is_err()
        );
        assert_eq!(labels(&screen.tree, screen.body), before);
        let data = json!({"summary":"2 passed", "passed":2, "failed":0, "failures":[]});
        screen
            .apply_snapshot(snapshot(&manifest, 1, data.clone()), &mut text)
            .unwrap();
        assert!(labels(&screen.tree, screen.body).contains(&"2 passed".into()));
        assert_eq!(screen.data, data);
        assert!(
            screen
                .apply_snapshot(
                    snapshot(&manifest, 0, manifest.initial_data.clone()),
                    &mut text
                )
                .is_err()
        );
    }

    #[test]
    fn a_parent_close_closes_the_window() {
        let manifest = manifest();
        let mut screen =
            WidgetScreen::from_snapshot(snapshot(&manifest, 0, manifest.initial_data.clone()))
                .unwrap();
        screen.receive(
            Update::Message(HostMessage::Closed),
            &mut TextStore::new(Default::default()),
        );
        assert!(screen.wants_close());
    }

    #[test]
    fn an_unknown_receipt_retries_the_exact_request_after_the_data_changes() {
        let manifest = manifest();
        let mut screen =
            WidgetScreen::from_snapshot(snapshot(&manifest, 1, manifest.initial_data.clone()))
                .unwrap();
        let (bridge, requests) = Bridge::recording();
        screen.attach_bridge(bridge);
        screen.activate(screen.actions[0].1.clone());
        let first = requests.try_recv().unwrap();
        let first_json = serde_json::to_value(&first).unwrap();
        let request_id = first_json["requestId"].as_str().unwrap().to_owned();
        let mut text = TextStore::new(Default::default());
        screen.receive(
            Update::Message(HostMessage::ActionResult {
                request_id: request_id.clone(),
                ok: false,
                error: Some("directory sync failed".into()),
                error_kind: Some("outcome_unknown".into()),
            }),
            &mut text,
        );
        assert!(
            screen
                .instance
                .as_ref()
                .unwrap()
                .pending
                .as_ref()
                .unwrap()
                .uncertain
        );
        assert!(screen.retry.is_some());
        // The original action may already have run. A data update must not
        // discard the receipt or turn a retry into a new request/revision.
        let updated = json!({"summary":"done", "passed":1, "failed":0, "failures":[]});
        screen
            .apply_snapshot(snapshot(&manifest, 2, updated), &mut text)
            .unwrap();
        screen.activate(screen.actions[0].1.clone());
        assert!(
            requests.try_recv().is_err(),
            "an uncertain receipt blocks new actions"
        );
        screen.tree.set_focus(screen.retry);
        screen.dispatch(
            Event::KeyDown {
                key: Key::Space,
                modifiers: Modifiers::default(),
            },
            &mut text,
        );
        let retried = requests.try_recv().unwrap();
        assert_eq!(serde_json::to_value(retried).unwrap(), first_json);
        assert_eq!(screen.instance.as_ref().unwrap().next_action, 1);
        screen.receive(
            Update::Message(HostMessage::ActionResult {
                request_id,
                ok: true,
                error: None,
                error_kind: None,
            }),
            &mut text,
        );
        assert!(screen.instance.as_ref().unwrap().pending.is_none());
        assert!(screen.retry.is_none());
    }

    fn gallery() -> (Manifest, Vec<Asset>) {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.noa-widgets/image-gallery");
        let manifest =
            Manifest::from_slice(&std::fs::read(dir.join("widget.json")).unwrap()).unwrap();
        let assets = noa_widget::assets::load(&dir, &manifest).unwrap();
        (manifest, assets)
    }

    fn gallery_snapshot(
        manifest: &Manifest,
        assets: &[Asset],
        revision: u64,
        data: Value,
    ) -> HostMessage {
        let digest =
            noa_widget::definition_digest(manifest, &noa_widget::assets::digests(assets)).unwrap();
        HostMessage::Snapshot {
            instance_id: "wi_0123456789abcdef0123456789abcdef".into(),
            definition_digest: digest,
            data_revision: revision,
            manifest: serde_json::to_value(manifest).unwrap(),
            data,
        }
    }

    #[test]
    fn a_live_definition_with_assets_is_pinned_to_the_received_bytes() {
        let (manifest, assets) = gallery();
        let data = manifest.initial_data.clone();
        let snapshot = gallery_snapshot(&manifest, &assets, 1, data.clone());
        assert!(WidgetScreen::from_bridge(snapshot.clone(), &assets).is_ok());
        // Missing, extra or different bytes do not match the pinned digest.
        assert!(WidgetScreen::from_bridge(snapshot.clone(), &assets[1..]).is_err());
        let mut changed = assets.clone();
        let mut bytes = changed[0].bytes.clone();
        *bytes.last_mut().unwrap() ^= 1;
        changed[0] = Asset::verify(&changed[0].id, bytes, None).unwrap();
        assert!(WidgetScreen::from_bridge(snapshot.clone(), &changed).is_err());
        // A manifest-only digest cannot stand in for one covering the assets.
        let manifest_only = HostMessage::Snapshot {
            instance_id: "wi_0123456789abcdef0123456789abcdef".into(),
            definition_digest: manifest.digest(),
            data_revision: 1,
            manifest: serde_json::to_value(&manifest).unwrap(),
            data,
        };
        assert!(WidgetScreen::from_bridge(manifest_only, &assets).is_err());
    }

    #[test]
    fn a_data_update_can_point_an_image_at_another_declared_asset() {
        let (manifest, assets) = gallery();
        let mut screen = WidgetScreen::from_bridge(
            gallery_snapshot(&manifest, &assets, 1, manifest.initial_data.clone()),
            &assets,
        )
        .unwrap();
        let mut uploaded = 0;
        screen.upload_images(&mut |_, _, _| {
            uploaded += 1;
            ImageId(uploaded)
        });
        // Three assets, each square and (the view has a circle) round.
        assert_eq!(uploaded, 6);
        let mut text = TextStore::new(Default::default());
        let data = json!({"featured":"pattern","caption":"Pattern"});
        screen
            .apply_snapshot(
                gallery_snapshot(&manifest, &assets, 2, data.clone()),
                &mut text,
            )
            .unwrap();
        assert_eq!(screen.data, data);
        assert!(labels(&screen.tree, screen.body).contains(&"Pattern".into()));
        // An undeclared asset id never reaches the tree.
        let invalid = json!({"featured":"missing","caption":"Missing"});
        assert!(
            screen
                .apply_snapshot(gallery_snapshot(&manifest, &assets, 3, invalid), &mut text)
                .is_err()
        );
        assert_eq!(screen.data, data);
    }

    #[test]
    fn an_asset_frame_after_the_snapshot_invalidates_the_instance() {
        let (manifest, assets) = gallery();
        let mut screen = WidgetScreen::from_bridge(
            gallery_snapshot(&manifest, &assets, 1, manifest.initial_data.clone()),
            &assets,
        )
        .unwrap();
        let late = noa_ipc::widget::asset_frames("logo", &assets[1].digest, &assets[1].bytes);
        screen.receive(
            Update::Message(late[0].clone()),
            &mut TextStore::new(Default::default()),
        );
        assert!(!screen.instance.as_ref().unwrap().valid);
    }

    #[test]
    fn images_without_their_bytes_show_alt_text() {
        let (manifest, _) = gallery();
        // Without asset bytes (e.g. the catalogue's preview windows), every
        // image falls back to its alt text.
        let screen =
            WidgetScreen::preview(manifest.clone(), manifest.initial_data.clone()).unwrap();
        let shown = labels(&screen.tree, screen.body);
        assert!(shown.contains(&"Noa ring logo, small".into()));
        assert!(shown.contains(&"A packaged JPEG landscape".into()));
    }
}
