use super::*;
use ember::client::ClientError;
use ember::model::RunStatus;
use noa_gpui_core::Modifiers;
use noa_gpui_scene::Rect;

fn prepared(kind: &str) -> (EmberScreen, TextStore) {
    let mut screen = EmberScreen::demo(kind);
    let mut text = TextStore::new(Default::default());
    screen.prepare(Size::new(1120.0, 780.0), &mut text);
    (screen, text)
}

fn bounds(screen: &EmberScreen, id: WidgetId) -> Rect {
    let origin = screen.tree.window_origin(id).unwrap();
    let layout = screen.tree.layout(id).unwrap();
    Rect::new(origin.x, origin.y, layout.rect.width, layout.rect.height)
}

#[test]
fn incoming_results_and_window_resizing_preserve_draft_selection_and_ime() {
    let (mut screen, mut text) = prepared("conversation");
    let input = screen.ids.composer;
    screen.set_area(input, "書きかけの相談\n次の行".into());
    screen.tree.set_focus(Some(input));
    screen
        .tree
        .update::<TextArea, _>(input, |area, cx| area.select(cx, 0..6));
    screen.dispatch(
        Event::Composition {
            text: "へんかん".into(),
            cursor: Some((0, 12)),
        },
        &mut text,
    );
    let before = screen.tree.get::<TextArea>(input).unwrap().selection();
    let mut view = screen.view.clone();
    view.state.revision += 1;
    view.state.runs[2].status = RunStatus::Completed;
    screen.receive(Update::View(view));
    screen.prepare(Size::new(720.0, 640.0), &mut text);
    let area = screen.tree.get::<TextArea>(input).unwrap();
    assert_eq!(area.text(), "書きかけの相談\n次の行");
    assert_eq!(area.selection(), before);
    assert_eq!(area.composition(), "へんかん");
    assert_eq!(screen.tree.focused(), Some(input));
    assert_eq!(screen.ids.composer, input);
    screen.dispatch(
        Event::KeyDown {
            key: Key::Enter,
            modifiers: Modifiers {
                command: true,
                ..Default::default()
            },
        },
        &mut text,
    );
    assert!(screen.pending.is_none(), "IME confirmation is not a send");
    assert_eq!(screen.area(input), "書きかけの相談\n次の行");
}

#[test]
fn page_switches_keep_each_editor_in_the_same_tree() {
    let (mut screen, mut text) = prepared("conversation");
    let ids = [
        screen.ids.composer,
        screen.ids.objective,
        screen.ids.memory_text,
        screen.ids.model,
    ];
    screen.set_area(ids[0], "相談の下書き".into());
    screen.set_area(ids[1], "依頼の下書き".into());
    screen.set_area(ids[2], "記憶の下書き".into());
    for page in Page::ALL {
        screen.act(Action::Page(page), &mut text).unwrap();
        screen.prepare(Size::new(720.0, 640.0), &mut text);
    }
    assert!(ids.iter().all(|id| screen.tree.contains(*id)));
    assert_eq!(screen.area(ids[0]), "相談の下書き");
    assert_eq!(screen.area(ids[1]), "依頼の下書き");
    assert_eq!(screen.area(ids[2]), "記憶の下書き");
}

#[test]
fn sharing_is_explicit_and_cannot_cross_a_changed_scope() {
    let (mut screen, mut text) = prepared("conversation");
    let scope = screen.scope.clone();
    let attachment = Attachment {
        path: "/workspace/Noa/design.md".into(),
        text: "選択した資料".into(),
        bytes: 18,
        ..Default::default()
    };
    screen.receive(Update::Attachment {
        scope: scope.clone(),
        result: Ok(attachment.clone()),
    });
    assert!(screen.attachments.is_empty(), "previewing is not sharing");
    assert!(matches!(screen.preview, Some(Preview::Attachment { .. })));
    screen.act(Action::SharePreview, &mut text).unwrap();
    assert_eq!(
        screen.attachment_paths(),
        std::slice::from_ref(&attachment.path)
    );
    screen.attachments.clear();
    screen.receive(Update::Attachment {
        scope,
        result: Ok(attachment),
    });
    screen.scope = Scope::Global;
    assert!(screen.act(Action::SharePreview, &mut text).is_err());
    assert!(screen.attachments.is_empty());
}

fn pending_draft(screen: &mut EmberScreen, id: &str, draft: &str) {
    screen.set_area(screen.ids.composer, draft.into());
    screen.pending = Some(Pending {
        id: id.into(),
        command: Command::Send {
            text: draft.into(),
            scope: screen.scope.clone(),
            attachments: vec![],
        },
        completion: Completion::Composer(draft.into(), vec![]),
        in_flight: true,
    });
}

#[test]
fn uncertain_receipts_keep_the_same_id_and_rejections_leave_the_input_editable() {
    let (mut screen, _text) = prepared("conversation");
    pending_draft(&mut screen, "request-1", "依頼したいこと");
    screen.receive(Update::Command {
        request_id: "request-1".into(),
        result: Err(ClientError {
            message: "応答を確認できません".into(),
            outcome_unknown: true,
        }),
    });
    let pending = screen.pending.as_ref().unwrap();
    assert_eq!(pending.id, "request-1");
    assert!(!pending.in_flight);
    assert_eq!(screen.area(screen.ids.composer), "依頼したいこと");
    screen.receive(Update::Command {
        request_id: "request-1".into(),
        result: Err(ClientError {
            message: "入力を確認してください".into(),
            outcome_unknown: false,
        }),
    });
    assert!(screen.pending.is_none());
    assert_eq!(screen.area(screen.ids.composer), "依頼したいこと");
}

#[test]
fn an_acceptance_does_not_discard_text_typed_after_sending() {
    let (mut screen, _text) = prepared("conversation");
    pending_draft(&mut screen, "request-2", "送信済みの文章");
    screen.set_area(screen.ids.composer, "次の相談を書いている".into());
    screen.receive(Update::Command {
        request_id: "request-2".into(),
        result: Ok(screen.view.clone()),
    });
    assert!(screen.pending.is_none());
    assert_eq!(screen.area(screen.ids.composer), "次の相談を書いている");
}

#[test]
fn remembering_a_message_preserves_its_project_and_requires_a_separate_save() {
    let (mut screen, mut text) = prepared("conversation");
    screen.scope = Scope::Global;
    let count = screen.view.state.memories.len();
    screen
        .act(Action::Remember("message-demo-user".into()), &mut text)
        .unwrap();
    assert_eq!(screen.page, Page::Memory);
    assert_eq!(screen.memory_scope, crate::demo::scope());
    assert_eq!(screen.view.state.memories.len(), count);
    assert!(screen.pending.is_none());
    assert!(
        screen.act(Action::SaveMemory, &mut text).is_err(),
        "the demo never persists"
    );
}

#[test]
fn editing_settings_survives_a_service_snapshot() {
    let (mut screen, _text) = prepared("settings");
    screen.set_field(screen.ids.model, "my-model".into());
    screen.settings_dirty = true;
    let mut view = screen.view.clone();
    view.state.settings.model = "other-window-model".into();
    screen.receive(Update::View(view));
    assert_eq!(screen.field(screen.ids.model), "my-model");
}

#[test]
fn a_settings_receipt_preserves_edits_made_while_waiting() {
    let (mut screen, _text) = prepared("settings");
    let saved = screen.view.state.settings.clone();
    screen.pending = Some(Pending {
        id: "settings-1".into(),
        command: Command::UpdateSettings {
            settings: saved.clone(),
        },
        completion: Completion::Settings(saved),
        in_flight: true,
    });
    screen.set_field(screen.ids.model, "next-model".into());
    screen.settings_dirty = true;
    screen.receive(Update::Command {
        request_id: "settings-1".into(),
        result: Ok(screen.view.clone()),
    });
    assert!(screen.settings_dirty);
    assert_eq!(screen.field(screen.ids.model), "next-model");
}

#[test]
fn the_startup_project_does_not_override_a_scope_already_chosen() {
    let (mut screen, _text) = prepared("conversation");
    screen.scope = Scope::Global;
    screen.scope_chosen = true;
    screen.receive(Update::Project {
        initial: true,
        result: Ok(crate::demo::scope()),
    });
    assert_eq!(screen.scope, Scope::Global);
    assert_eq!(screen.project, Some(crate::demo::scope()));
    screen.receive(Update::Project {
        initial: false,
        result: Ok(crate::demo::scope()),
    });
    assert_eq!(screen.scope, crate::demo::scope());
}

#[test]
fn a_memory_receipt_does_not_discard_a_new_scope_or_an_edited_draft() {
    let (mut screen, _text) = prepared("memory");
    let memory = screen.view.state.memories[1].clone();
    screen.edit_memory(memory.clone());
    screen.pending = Some(Pending {
        id: "memory-1".into(),
        command: Command::SaveMemory {
            id: Some(memory.id.clone()),
            expected_revision: Some(memory.revision),
            scope: memory.scope.clone(),
            text: memory.text.clone(),
        },
        completion: Completion::Memory {
            id: Some(memory.id.clone()),
            scope: memory.scope,
            text: memory.text,
        },
        in_flight: true,
    });
    screen.set_area(screen.ids.memory_text, "保存待ちの間に修正した文章".into());
    let mut view = screen.view.clone();
    view.state.memories[1].revision += 1;
    screen.receive(Update::Command {
        request_id: "memory-1".into(),
        result: Ok(view),
    });
    assert_eq!(
        screen.area(screen.ids.memory_text),
        "保存待ちの間に修正した文章"
    );
    assert_eq!(screen.memory_edit.as_deref(), Some(memory.id.as_str()));
    assert_eq!(screen.memory_revision, Some(memory.revision + 1));
}

#[test]
fn all_editors_fit_the_minimum_width_without_rebuilding() {
    let (mut screen, mut text) = prepared("conversation");
    for (page, id) in [
        (Page::Conversation, screen.ids.composer),
        (Page::Requests, screen.ids.objective),
        (Page::Memory, screen.ids.memory_text),
        (Page::Settings, screen.ids.model),
    ] {
        screen.act(Action::Page(page), &mut text).unwrap();
        for size in [Size::new(720.0, 640.0), Size::new(1120.0, 780.0)] {
            screen.prepare(size, &mut text);
            let bounds = bounds(&screen, id);
            assert!(
                bounds.x >= -1.0
                    && bounds.width > 200.0
                    && bounds.x + bounds.width <= size.width + 1.0,
                "{page:?}: {bounds:?} in {size:?}"
            );
        }
    }
}

#[test]
fn a_long_project_path_cannot_push_the_composer_out_of_the_window() {
    let (mut screen, mut text) = prepared("conversation");
    let scope = Scope::Project {
        root: format!("/workspace/{}/project", "nested/".repeat(180)),
    };
    screen.scope = scope.clone();
    screen.project = Some(scope.clone());
    screen.memory_scope = scope;
    for (page, id) in [
        (Page::Conversation, screen.ids.composer),
        (Page::Memory, screen.ids.memory_text),
    ] {
        screen.act(Action::Page(page), &mut text).unwrap();
        screen.prepare(Size::new(720.0, 640.0), &mut text);
        let rect = bounds(&screen, id);
        assert!(
            rect.y >= 0.0 && rect.y + rect.height <= 640.0,
            "{page:?}: {rect:?}"
        );
    }
}

#[test]
fn offline_modes_have_no_execution_channel() {
    for kind in crate::demo::KINDS {
        let (mut screen, mut text) = prepared(kind);
        assert!(screen.feed.is_none());
        screen.set_area(screen.ids.composer, "送信しない".into());
        screen.set_area(screen.ids.objective, "実行しない".into());
        screen.set_field(screen.ids.attachment_path, "/not/read/by/the/demo".into());
        for action in [
            Action::Send,
            Action::Start,
            Action::PreviewFile,
            Action::Reconnect,
            Action::Export,
            Action::PrepareHandoff("run-demo-document".into()),
        ] {
            assert!(screen.act(action, &mut text).is_err());
            assert!(screen.pending.is_none());
        }
    }
}

#[test]
fn control_characters_are_visible_in_received_text() {
    assert_eq!(
        visible("a\u{001b}[31m\u{202e}b\n"),
        "a[U+001B][31m[U+202E]b\n"
    );
}
