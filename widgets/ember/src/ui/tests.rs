use super::*;
use ember::client::ClientError;
use ember::model::RunStatus;
use noa_gpui_core::Modifiers;
use noa_gpui_core::widgets::Button;
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

fn labels_below(screen: &EmberScreen, parent: WidgetId) -> String {
    let mut labels = String::new();
    if let Some(label) = screen.tree.get::<Label>(parent) {
        labels.push_str(label.text());
        labels.push('\n');
    }
    for child in screen.tree.children(parent) {
        labels.push_str(&labels_below(screen, *child));
    }
    labels
}

fn label_with_text(screen: &EmberScreen, parent: WidgetId, value: &str) -> Option<WidgetId> {
    if screen
        .tree
        .get::<Label>(parent)
        .is_some_and(|label| label.text() == value)
    {
        return Some(parent);
    }
    screen
        .tree
        .children(parent)
        .iter()
        .find_map(|child| label_with_text(screen, *child, value))
}

#[test]
fn long_bubbles_stay_in_the_reading_column_above_a_fixed_composer() {
    let (mut screen, mut text) = prepared("conversation");
    let mut view = screen.view.clone();
    view.state.messages[0].content = format!(
        "相談: {}",
        "長いメッセージも読みやすく折り返します。".repeat(20)
    );
    view.state.messages[1].content = format!(
        "回答: {}",
        "長いメッセージも読みやすく折り返します。".repeat(20)
    );
    let user_text = view.state.messages[0].content.clone();
    let assistant_text = view.state.messages[1].content.clone();
    screen.receive(Update::View(view));
    for size in [Size::new(720.0, 640.0), Size::new(1120.0, 780.0)] {
        screen.prepare(size, &mut text);
        let user = bounds(
            &screen,
            label_with_text(&screen, screen.ids.history, &user_text).unwrap(),
        );
        let assistant = bounds(
            &screen,
            label_with_text(&screen, screen.ids.history, &assistant_text).unwrap(),
        );
        for bubble in [user, assistant] {
            assert!(
                bubble.width > 200.0 && bubble.x >= 0.0 && bubble.x + bubble.width <= size.width,
                "{bubble:?} in {size:?}"
            );
        }
        assert!(
            user.x > assistant.x,
            "the user and Ember occupy opposite sides"
        );
        let history = bounds(&screen, screen.ids.history_scroll);
        let composer = bounds(&screen, screen.ids.composer);
        assert!(
            history.height > 100.0,
            "the conversation keeps a usable scroll area"
        );
        assert!(history.y + history.height <= composer.y + 1.0);
        assert!(composer.y + composer.height <= size.height);
    }
}

#[test]
fn profile_opens_activity_without_discarding_the_conversation_draft() {
    let (mut screen, mut text) = prepared("conversation");
    let composer = screen.ids.composer;
    screen.set_area(composer, "戻ってから続きを書く".into());
    let action = screen
        .actions
        .iter()
        .find(|(id, _)| *id == screen.ids.profile)
        .expect("the profile is an interactive entry to activity")
        .1
        .clone();
    screen.act(action, &mut text).unwrap();
    assert_eq!(screen.page, Page::Requests);
    screen
        .act(Action::Page(Page::Conversation), &mut text)
        .unwrap();
    assert_eq!(screen.ids.composer, composer);
    assert_eq!(screen.area(composer), "戻ってから続きを書く");
    assert!(screen.pending.is_none());
}

#[test]
fn opening_a_running_requests_details_clears_a_previous_completed_filter() {
    let (mut screen, mut text) = prepared("conversation");
    screen.act(Action::Page(Page::Requests), &mut text).unwrap();
    screen
        .act(Action::FilterActivity(ActivityFilter::Completed), &mut text)
        .unwrap();
    screen
        .act(Action::Page(Page::Conversation), &mut text)
        .unwrap();
    let action = screen
        .actions
        .iter()
        .find(|(id, _)| {
            screen
                .tree
                .ancestors(*id)
                .any(|id| id == screen.ids.history)
                && screen
                    .tree
                    .get::<Button>(*id)
                    .is_some_and(|button| button.label() == "依頼の詳細")
        })
        .expect("the conversation provides an entry to request details")
        .1
        .clone();
    screen.act(action, &mut text).unwrap();
    assert_eq!(screen.page, Page::Requests);
    assert_eq!(screen.activity_filter, ActivityFilter::All);
    assert!(
        labels_below(&screen, screen.ids.request_list)
            .contains(&screen.view.state.runs[2].objective)
    );
}

#[test]
fn suggestions_prepare_the_matching_editor_without_submitting_work() {
    for (suggestion, page, kind) in [
        (Suggestion::Conversation, Page::Conversation, None),
        (Suggestion::Document, Page::Requests, Some(0)),
        (Suggestion::Research, Page::Requests, Some(1)),
    ] {
        let (mut screen, mut text) = prepared("conversation");
        let runs = screen.view.state.runs.len();
        screen.act(Action::Suggest(suggestion), &mut text).unwrap();
        assert_eq!(screen.page, page);
        let editor = if page == Page::Conversation {
            screen.ids.composer
        } else {
            screen.ids.objective
        };
        assert!(!screen.area(editor).trim().is_empty());
        assert_eq!(screen.tree.focused(), Some(editor));
        if let Some(kind) = kind {
            assert_eq!(
                screen
                    .tree
                    .get::<PopUpButton>(screen.ids.kind)
                    .unwrap()
                    .selected(),
                kind
            );
        }
        assert!(screen.pending.is_none());
        assert_eq!(screen.view.state.runs.len(), runs);
    }
}

#[test]
fn suggestions_do_not_replace_existing_drafts_or_retarget_their_request_kind() {
    let (mut screen, mut text) = prepared("conversation");
    screen.set_area(screen.ids.composer, "入力中の相談".into());
    screen.set_area(screen.ids.objective, "資料を根拠に比較する依頼".into());
    screen
        .tree
        .update::<PopUpButton, _>(screen.ids.kind, |picker, cx| picker.set_selected(cx, 1));
    for suggestion in [
        Suggestion::Conversation,
        Suggestion::Document,
        Suggestion::Research,
    ] {
        screen.act(Action::Suggest(suggestion), &mut text).unwrap();
        assert_eq!(screen.area(screen.ids.composer), "入力中の相談");
        assert_eq!(
            screen.area(screen.ids.objective),
            "資料を根拠に比較する依頼"
        );
        assert_eq!(
            screen
                .tree
                .get::<PopUpButton>(screen.ids.kind)
                .unwrap()
                .selected(),
            1
        );
        assert!(screen.pending.is_none());
    }
}

#[test]
fn a_suggestion_preserves_uncommitted_ime_text_in_an_empty_editor() {
    for (suggestion, page) in [
        (Suggestion::Conversation, Page::Conversation),
        (Suggestion::Document, Page::Requests),
    ] {
        let (mut screen, mut text) = prepared("conversation");
        screen.act(Action::Page(page), &mut text).unwrap();
        screen.prepare(Size::new(720.0, 640.0), &mut text);
        let editor = if page == Page::Conversation {
            screen.ids.composer
        } else {
            screen.ids.objective
        };
        screen.tree.set_focus(Some(editor));
        screen.dispatch(
            Event::Composition {
                text: "へんかん".into(),
                cursor: Some((0, 12)),
            },
            &mut text,
        );
        assert!(screen.area(editor).is_empty());
        screen.act(Action::Suggest(suggestion), &mut text).unwrap();
        let area = screen.tree.get::<TextArea>(editor).unwrap();
        assert!(area.text().is_empty());
        assert_eq!(area.composition(), "へんかん");
        assert_eq!(screen.tree.focused(), Some(editor));
    }
}

#[test]
fn history_applies_its_sixty_message_limit_after_filtering_the_selected_scope() {
    let (mut screen, mut text) = prepared("conversation");
    let mut view = screen.view.clone();
    view.state.messages.clear();
    for index in 0..65 {
        for (prefix, scope) in [
            ("project", screen.scope.clone()),
            ("personal", Scope::Global),
        ] {
            view.state.messages.push(ember::model::Message {
                id: format!("{prefix}-{index}"),
                scope,
                content: format!("history:{prefix}-{index}:end"),
                created_at: 1_795_843_200_000 + index * 1_000,
                ..Default::default()
            });
        }
    }
    screen.receive(Update::View(view));
    screen.prepare(Size::new(720.0, 640.0), &mut text);
    let history = labels_below(&screen, screen.ids.history);
    for index in 0..65 {
        assert_eq!(
            history.contains(&format!("history:project-{index}:end")),
            index >= 5,
            "project message {index}"
        );
        assert!(!history.contains(&format!("history:personal-{index}:end")));
    }
}

#[test]
fn a_scope_without_messages_offers_suggestions_even_if_another_scope_has_history() {
    let (mut screen, mut text) = prepared("conversation");
    screen.scope = Scope::Global;
    screen.refresh_needed = true;
    screen.prepare(Size::new(720.0, 640.0), &mut text);
    for suggestion in [
        Suggestion::Conversation,
        Suggestion::Document,
        Suggestion::Research,
    ] {
        assert!(screen.actions.iter().any(|(id, action)| {
            *action == Action::Suggest(suggestion)
                && screen
                    .tree
                    .ancestors(*id)
                    .any(|id| id == screen.ids.history)
        }));
    }
    let history = labels_below(&screen, screen.ids.history);
    assert!(!history.contains(&screen.view.state.messages[0].content));
}

#[test]
fn activity_filters_combine_status_scope_and_archived_visibility() {
    let (mut screen, mut text) = prepared("requests");
    let statuses = [
        ("queued", RunStatus::Queued),
        ("running", RunStatus::Running),
        ("stopping", RunStatus::CancelRequested),
        ("completed", RunStatus::Completed),
        ("failed", RunStatus::Failed),
        ("uncertain", RunStatus::Uncertain),
        ("canceled", RunStatus::Canceled),
    ];
    let mut view = screen.view.clone();
    view.state.runs = statuses
        .iter()
        .map(|(name, status)| ember::model::Run {
            id: (*name).into(),
            kind: RunKind::Document,
            scope: screen.scope.clone(),
            objective: format!("request:{name}:end"),
            status: *status,
            ..Default::default()
        })
        .collect();
    let mut archived = view.state.runs[3].clone();
    archived.id = "archived".into();
    archived.objective = "request:archived:end".into();
    archived.archived = true;
    view.state.runs.push(archived);
    let mut other_scope = view.state.runs[3].clone();
    other_scope.id = "other-scope".into();
    other_scope.objective = "request:other-scope:end".into();
    other_scope.scope = Scope::Global;
    view.state.runs.push(other_scope);
    screen.receive(Update::View(view));

    for (filter, expected) in [
        (
            ActivityFilter::All,
            vec![
                "queued",
                "running",
                "stopping",
                "completed",
                "failed",
                "uncertain",
                "canceled",
            ],
        ),
        (
            ActivityFilter::Active,
            vec!["queued", "running", "stopping"],
        ),
        (ActivityFilter::Completed, vec!["completed"]),
        (ActivityFilter::NeedsAttention, vec!["failed", "uncertain"]),
    ] {
        screen
            .act(Action::FilterActivity(filter), &mut text)
            .unwrap();
        let labels = labels_below(&screen, screen.ids.request_list);
        for (name, _) in statuses {
            assert_eq!(
                labels.contains(&format!("request:{name}:end")),
                expected.contains(&name),
                "{filter:?}: {name}"
            );
        }
        assert!(!labels.contains("request:archived:end"));
        assert!(!labels.contains("request:other-scope:end"));
    }
    screen
        .act(Action::FilterActivity(ActivityFilter::Completed), &mut text)
        .unwrap();
    screen.act(Action::ShowArchived, &mut text).unwrap();
    let labels = labels_below(&screen, screen.ids.request_list);
    assert!(labels.contains("request:completed:end"));
    assert!(labels.contains("request:archived:end"));
    assert!(!labels.contains("request:other-scope:end"));
}

#[test]
fn live_activity_changes_leave_the_request_draft_and_ime_in_place() {
    let (mut screen, mut text) = prepared("requests");
    let objective = screen.ids.objective;
    screen.set_area(objective, "次に任せる依頼".into());
    screen.tree.set_focus(Some(objective));
    screen.dispatch(
        Event::Composition {
            text: "つづき".into(),
            cursor: Some((0, 9)),
        },
        &mut text,
    );
    screen
        .act(Action::FilterActivity(ActivityFilter::Active), &mut text)
        .unwrap();
    let mut view = screen.view.clone();
    let objective_text = view.state.runs[2].objective.clone();
    view.state.runs[2].status = RunStatus::Completed;
    screen.receive(Update::View(view));
    screen.prepare(Size::new(720.0, 640.0), &mut text);
    assert!(!labels_below(&screen, screen.ids.request_list).contains(&objective_text));
    let area = screen.tree.get::<TextArea>(objective).unwrap();
    assert_eq!(area.text(), "次に任せる依頼");
    assert_eq!(area.composition(), "つづき");
    assert_eq!(screen.tree.focused(), Some(objective));
}

#[test]
fn recent_active_requests_do_not_hide_the_latest_completed_artifact() {
    let (mut screen, mut text) = prepared("conversation");
    let mut view = screen.view.clone();
    let completed = view.state.runs[1].clone();
    let running = view.state.runs[2].clone();
    view.state.runs = vec![completed.clone()];
    for index in 0..4 {
        let mut run = running.clone();
        run.id = format!("running-{index}");
        run.created_at += index * 1_000;
        run.updated_at += index * 1_000;
        view.state.runs.push(run);
    }
    screen.receive(Update::View(view));
    screen.prepare(Size::new(1120.0, 780.0), &mut text);
    assert!(
        screen.actions.iter().any(|(id, action)| {
            *action == Action::Artifact(completed.id.clone())
                && screen
                    .tree
                    .ancestors(*id)
                    .any(|id| id == screen.ids.history)
        }),
        "a ready artifact stays reachable from the conversation while other work runs"
    );
}
