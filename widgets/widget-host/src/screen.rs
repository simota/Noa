//! Canonical instance data only changes on a validated parent snapshot.
//! Button clicks are requests, not optimistic data mutations.

use noa_gpui_core::widgets::{Button, ButtonClicked, Container, Label, ScrollView, Tag};
use noa_gpui_core::{Appearance, Message, Role, Style, Theme, UiTree, WidgetId};
use noa_gpui_platform::Screen;
use noa_gpui_text::TextStore;
use noa_ipc::widget::{HostMessage, HostRequest};
use noa_widget::Manifest;
use serde_json::Value;

use crate::bridge::{Bridge, Update};
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
}

pub struct WidgetScreen {
    tree: UiTree,
    body: WidgetId,
    status: WidgetId,
    footer: WidgetId,
    retry: Option<WidgetId>,
    version: WidgetId,
    manifest: Manifest,
    data: Value,
    actions: Vec<(WidgetId, Action)>,
    preview_events: Vec<Action>,
    instance: Option<Instance>,
    bridge: Option<Bridge>,
    close: bool,
}

impl WidgetScreen {
    pub fn preview(manifest: Manifest, data: Value) -> Result<Self, String> {
        Self::new(manifest, data, None)
    }

    pub fn from_snapshot(snapshot: HostMessage) -> Result<Self, String> {
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
        if manifest.digest() != definition_digest {
            return Err("snapshot definition digest does not match its manifest".into());
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
        Self::new(manifest, data, Some(instance))
    }

    fn new(manifest: Manifest, data: Value, instance: Option<Instance>) -> Result<Self, String> {
        manifest
            .validate_data(&data)
            .map_err(|error| error.to_string())?;
        let view = render::resolve(&manifest.view, &data)?;
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
            Container::new(style::wide(style::column(6.0)))
                .background(Role::Surface)
                .padding(20.0),
        );
        let title = tree.insert(Some(header), Container::new(style::wide(style::row(12.0))));
        tree.insert(
            Some(title),
            Label::new(&manifest.title).font_size(20.0).weight(700),
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
                .font_size(12.0)
                .color(Role::Muted)
                .max_lines(3),
        );
        let version = tree.insert(
            Some(header),
            Label::new(version_label(&manifest, instance.as_ref()))
                .font_size(11.0)
                .color(Role::Muted),
        );
        let scroll = tree.insert(Some(root), ScrollView::new(style::fill()));
        let body = tree.insert(
            Some(scroll),
            Container::new(style::wide(style::column(16.0))).padding(20.0),
        );
        let mut actions = Vec::new();
        render::insert(&mut tree, body, &view, &mut actions);
        let footer = tree.insert(
            Some(root),
            Container::new(style::wide(style::column(8.0)))
                .padding(14.0)
                .background(Role::Surface),
        );
        let status = tree.insert(
            Some(footer),
            Label::new(if instance.is_some() {
                "操作は担当エージェントへ送信されます。"
            } else {
                "プレビューでは操作のイベントを表示します。処理は実行されません。"
            })
            .font_size(12.0)
            .color(Role::Muted)
            .max_lines(4),
        );
        Ok(Self {
            tree,
            body,
            status,
            footer,
            retry: None,
            version,
            manifest,
            data,
            actions,
            preview_events: Vec::new(),
            instance,
            bridge: None,
            close: false,
        })
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
        if parse_manifest(&manifest)?.digest() != instance.digest {
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
        let view = render::resolve(&self.manifest.view, &data)?;
        if data != self.data {
            for child in self.tree.children(self.body).to_vec() {
                self.tree.remove(child, text);
            }
            self.actions.clear();
            render::insert(&mut self.tree, self.body, &view, &mut self.actions);
        }
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

    fn receive(&mut self, update: Update, text: &mut TextStore) {
        match update {
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
                instance.pending = None;
                if let Some(button) = self.retry.take() {
                    self.tree.remove(button, text);
                }
                self.notice(if ok {
                    "操作を担当エージェントに送信しました。".into()
                } else {
                    format!(
                        "操作を送信できません: {}",
                        error.unwrap_or_else(|| "理由不明".into())
                    )
                });
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
    fn tree(&self) -> &UiTree {
        &self.tree
    }
    fn tree_mut(&mut self) -> &mut UiTree {
        &mut self.tree
    }

    fn handle(&mut self, message: Message, _text: &mut TextStore) {
        if message.downcast_ref::<ButtonClicked>().is_some() && self.retry == Some(message.source) {
            self.retry_pending();
            return;
        }
        if message.downcast_ref::<ButtonClicked>().is_some()
            && let Some((_, action)) = self.actions.iter().find(|(id, _)| *id == message.source)
        {
            self.activate(action.clone());
        }
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
mod tests {
    use super::*;
    use noa_gpui_core::widgets::Button;
    use noa_gpui_core::{Event, Key, Modifiers};
    use serde_json::json;

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
}
