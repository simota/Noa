use noa_gpui_core::widgets::{Badge, Checkbox, ComboBox, PopUpButton, TextArea};
use noa_gpui_core::{Event, PointerButton};
use noa_gpui_scene::Point;

use discussion::model::types::{AgentChoice, AgentSetup, Debate, DebateStatus, MinuteSection};

use super::*;
use crate::live::{TestFeed, test_channel};

fn window() -> Size {
    Size::new(1440.0, 900.0)
}

fn laid_out() -> (Discussion, TextStore) {
    let mut screen = Discussion::demo();
    let mut text = TextStore::new(Default::default());
    screen.prepare(window(), &mut text);
    (screen, text)
}

fn p(id: &str) -> PartId {
    PartId::new(id)
}

fn i(id: &str) -> IssueId {
    IssueId::new(id)
}

fn widget_for(screen: &Discussion, action: &Action) -> WidgetId {
    screen
        .actions
        .iter()
        .find(|(_, a)| a == action)
        .map(|(id, _)| *id)
        .unwrap_or_else(|| panic!("no widget for {action:?}"))
}

fn has_widget_for(screen: &Discussion, action: &Action) -> bool {
    screen.actions.iter().any(|(_, a)| a == action)
}

fn click(screen: &mut Discussion, text: &mut TextStore, id: WidgetId) {
    let origin = screen.tree.window_origin(id).unwrap();
    let rect = screen.tree.layout(id).unwrap().rect;
    let pos = Point::new(origin.x + rect.width / 2.0, origin.y + rect.height / 2.0);
    let button = PointerButton::Primary;
    screen.dispatch(Event::PointerMove { pos }, text);
    screen.dispatch(Event::PointerDown { pos, button }, text);
    screen.dispatch(Event::PointerUp { pos, button }, text);
    screen.prepare(window(), text);
}

fn rect(tree: &UiTree, id: WidgetId) -> [f32; 4] {
    let origin = tree.window_origin(id).unwrap();
    let size = tree.layout(id).unwrap().rect;
    [origin.x, origin.y, size.width, size.height]
}

// ── Layout ──────────────────────────────────────────────────────────────────

#[test]
fn the_three_panes_sit_side_by_side() {
    let (screen, _text) = laid_out();
    let tree = &screen.tree;
    let [tx, ty, tw, th] = rect(tree, screen.tree_host);
    let [cx, _, cw, _] = rect(tree, screen.center_host);
    let [mx, _, mw, _] = rect(tree, screen.minutes_host);
    assert_eq!((tx, ty), (0.0, 0.0));
    assert!((tw - TREE_WIDTH).abs() < 1.0, "tree {tw}");
    assert!((mw - MINUTES_WIDTH).abs() < 1.0, "minutes {mw}");
    assert!(tx + tw <= cx && cx + cw <= mx, "{tx} {tw} {cx} {cw} {mx}");
    assert!(mx + mw <= window().width + 0.5);
    assert!(th > 800.0, "the panes fill the height: {th}");
    assert!(screen.tree.is_hidden(screen.start.root));
}

#[test]
fn at_the_smallest_window_every_pane_fits_inside_it() {
    let (w, h) = MIN_WINDOW;
    let mut screen = Discussion::demo();
    let mut text = TextStore::new(Default::default());
    screen.prepare(Size::new(w as f32, h as f32), &mut text);
    let [mx, _, mw, _] = rect(&screen.tree, screen.minutes_host);
    assert!(
        mx + mw <= w as f32 + 0.5,
        "minutes pane ends at {}",
        mx + mw
    );
    let [_, y, _, sh] = rect(&screen.tree, screen.composer.send);
    assert!(y + sh <= h as f32, "send button at {y}+{sh}");
}

#[test]
fn the_composer_stays_below_the_chat_in_view() {
    let (screen, _text) = laid_out();
    let [_, y, _, h] = rect(&screen.tree, screen.composer.send);
    assert!(y + h <= window().height, "send button at {y}+{h}");
    let [_, cy, _, ch] = rect(&screen.tree, screen.center_host);
    assert!(
        cy + ch <= y,
        "chat ends at {} below the composer at {y}",
        cy + ch
    );
}

#[test]
fn every_issue_row_and_minutes_link_is_clickable() {
    let (screen, _text) = laid_out();
    for issue in ["A", "B", "B1", "C", "D"] {
        assert!(
            has_widget_for(&screen, &Action::Filter(i(issue))),
            "{issue}"
        );
    }
    let session = screen.session.as_ref().unwrap();
    let linked = MinuteSection::ALL
        .iter()
        .flat_map(|s| session.minutes.section(*s))
        .flat_map(|item| &item.issues)
        .count();
    assert!(linked > 0);
}

// ── Issue filter ────────────────────────────────────────────────────────────

#[test]
fn clicking_an_issue_filters_the_chat_to_its_subtree() {
    let (mut screen, mut text) = laid_out();
    let session = screen.session.clone().unwrap();
    let all = chat::visible(&session, None).len();
    let row = widget_for(&screen, &Action::Filter(i("B")));
    click(&mut screen, &mut text, row);
    assert_eq!(screen.filter, Some(i("B")));

    let shown = chat::visible(&session, screen.filter.as_ref());
    assert!(!shown.is_empty() && shown.len() < all);
    let family = subtree(&session, &i("B"));
    assert_eq!(family, BTreeSet::from([i("B"), i("B1")]));
    for message in &shown {
        assert!(is_about(&session, message, &family), "{}", message.id);
    }
    // A sub-issue's messages and the debate on B come along.
    assert!(shown.iter().any(|m| m.issues.contains(&i("B1"))));
    assert!(shown.iter().any(|m| m.kind == MsgKind::Verdict));
    // Untagged messages stay out.
    assert!(shown.iter().all(|m| m.kind != MsgKind::Intervention));
}

#[test]
fn clicking_the_same_issue_again_or_clear_shows_everything() {
    let (mut screen, mut text) = laid_out();
    screen.run(Action::Filter(i("C")), &mut text);
    assert_eq!(screen.filter, Some(i("C")));
    screen.run(Action::Filter(i("C")), &mut text);
    assert_eq!(screen.filter, None);

    screen.run(Action::Filter(i("A")), &mut text);
    screen.prepare(window(), &mut text);
    let clear = widget_for(&screen, &Action::ClearFilter);
    click(&mut screen, &mut text, clear);
    assert_eq!(screen.filter, None);
}

#[test]
fn a_filter_on_an_issue_that_is_gone_falls_back() {
    let (mut screen, mut text) = laid_out();
    screen.run(Action::Filter(i("D")), &mut text);
    let mut session = (**screen.session.as_ref().unwrap()).clone();
    session.issues.issues.retain(|issue| issue.id != i("D"));
    session.rev += 1;
    screen.session = Some(std::sync::Arc::new(session));
    screen.rebuild(&mut text);
    assert_eq!(screen.filter, None);
}

// ── Receipts ────────────────────────────────────────────────────────────────

#[test]
fn the_brief_bar_shows_each_recipients_delivery() {
    let (screen, _text) = laid_out();
    let round = screen.session.as_ref().unwrap().current_round().unwrap();
    let state = |part: &str| Delivery::of(&round.brief(&p(part)).unwrap().receipt);
    assert_eq!(state("claude"), Delivery::Replied);
    assert_eq!(state("codex"), Delivery::Started);
    assert_eq!(state("agy"), Delivery::Sent);
    // No failure, no retry button.
    assert!(!has_widget_for(&screen, &Action::Retry));
}

#[test]
fn a_failed_brief_offers_a_retry() {
    let (channel, feed) = test_channel();
    let mut h = Harness::new(channel, feed);
    let mut session = crate::demo::session();
    let round = session.rounds.last_mut().unwrap();
    round.briefs[2].receipt.failed = Some("pane gone".into());
    assert_eq!(Delivery::of(&round.briefs[2].receipt), Delivery::Failed);
    h.deliver(Some(session));
    let retry = widget_for(&h.screen, &Action::Retry);
    click(&mut h.screen, &mut h.text, retry);
    assert_eq!(h.drain(), vec![Command::Retry]);
}

#[test]
fn no_retry_is_offered_when_the_engine_would_refuse_it() {
    use discussion::model::types::Phase;
    let (channel, feed) = test_channel();
    let mut h = Harness::new(channel, feed);
    // A panelist failed, the others replied, and the moderator is
    // summarizing: the engine retries only the moderator's brief now.
    let mut session = crate::demo::session();
    let round = session.rounds.last_mut().unwrap();
    round.briefs[2].receipt.failed = Some("pane gone".into());
    round.phase = Phase::Summarizing;
    h.deliver(Some(session.clone()));
    assert!(!has_widget_for(&h.screen, &Action::Retry));
    // Nor once the round is closed.
    session.rounds.last_mut().unwrap().phase = Phase::Closed;
    session.rev += 1;
    h.deliver(Some(session));
    assert!(!has_widget_for(&h.screen, &Action::Retry));
}

// ── Debates ─────────────────────────────────────────────────────────────────

#[test]
fn only_contested_issues_get_a_debate_button() {
    let (screen, _text) = laid_out();
    assert!(has_widget_for(&screen, &Action::DraftDebate(i("C"))));
    for issue in ["A", "B", "B1", "D"] {
        assert!(
            !has_widget_for(&screen, &Action::DraftDebate(i(issue))),
            "{issue}"
        );
    }
}

#[test]
fn a_drafted_debate_takes_sides_from_stances_and_can_swap_them() {
    let (channel, feed) = test_channel();
    let mut h = Harness::new(channel, feed);
    h.deliver(Some(crate::demo::session()));
    let sword = widget_for(&h.screen, &Action::DraftDebate(i("C")));
    click(&mut h.screen, &mut h.text, sword);
    let Center::Debate(view) = h.screen.center.clone() else {
        panic!("not in debate mode");
    };
    let session = h.screen.session.clone().unwrap();
    // agy is for C, codex against; claude took no stance.
    assert_eq!(
        view.sides(&session),
        Some((vec![p("agy")], vec![p("codex")]))
    );
    // Each side shows the message its member took the stance in.
    let [pro, mid, con] = debate::columns(&session, &view);
    assert_eq!(pro.len(), 1);
    assert!(mid.is_empty());
    assert_eq!(con[0].author, Author::Part(p("codex")));

    let swap = widget_for(&h.screen, &Action::SwapSides);
    click(&mut h.screen, &mut h.text, swap);
    let confirm = widget_for(&h.screen, &Action::ConfirmDebate);
    click(&mut h.screen, &mut h.text, confirm);
    assert_eq!(
        h.drain(),
        vec![Command::StartDebate {
            issue: i("C"),
            pro: vec![p("codex")],
            con: vec![p("agy")],
        }]
    );
}

#[test]
fn a_debate_with_an_empty_side_is_not_sent() {
    let (channel, feed) = test_channel();
    let mut h = Harness::new(channel, feed);
    h.deliver(Some(crate::demo::session()));
    // D has one stance only.
    h.screen.run(Action::DraftDebate(i("D")), &mut h.text);
    h.screen.run(Action::ConfirmDebate, &mut h.text);
    assert!(h.drain().is_empty());
    assert_eq!(h.screen.toasts.len(), 1, "{:?}", h.screen.toasts);
}

fn with_proposal() -> Session {
    let mut session = crate::demo::session();
    session.debates.push(Debate {
        id: DebateId::new("d2"),
        issue: i("C"),
        pro: vec![p("agy")],
        con: vec![p("codex")],
        max_exchanges: 2,
        status: DebateStatus::Proposed,
        turns: Vec::new(),
        verdict: None,
    });
    session.rev += 1;
    session
}

#[test]
fn a_proposed_debate_comes_up_and_is_approved_with_swapped_sides() {
    let (channel, feed) = test_channel();
    let mut h = Harness::new(channel, feed);
    h.deliver(Some(crate::demo::session()));
    assert_eq!(h.screen.center, Center::Chat, "an ended debate stays down");
    h.deliver(Some(with_proposal()));
    assert_eq!(
        h.screen.center,
        Center::Debate(DebateView::of(DebateId::new("d2")))
    );
    // While it waits, no other debate can be drafted.
    assert!(!has_widget_for(&h.screen, &Action::DraftDebate(i("C"))));

    h.screen.run(Action::SwapSides, &mut h.text);
    h.screen.prepare(window(), &mut h.text);
    let confirm = widget_for(&h.screen, &Action::ConfirmDebate);
    click(&mut h.screen, &mut h.text, confirm);
    assert_eq!(
        h.drain(),
        vec![Command::ApproveDebate {
            id: DebateId::new("d2"),
            pro: vec![p("codex")],
            con: vec![p("agy")],
        }]
    );
}

#[test]
fn a_proposed_debate_can_be_declined_and_left_from_the_banner() {
    let (channel, feed) = test_channel();
    let mut h = Harness::new(channel, feed);
    h.deliver(Some(with_proposal()));
    h.screen.run(Action::CancelDebate, &mut h.text);
    assert_eq!(
        h.drain(),
        vec![Command::DeclineDebate {
            id: DebateId::new("d2")
        }]
    );
    assert_eq!(h.screen.center, Center::Chat);
    // The next snapshot (still proposed) does not force it back up, and the
    // chat banner leads to it.
    let mut again = with_proposal();
    again.rev += 1;
    h.deliver(Some(again));
    assert_eq!(h.screen.center, Center::Chat);
    assert!(has_widget_for(
        &h.screen,
        &Action::ShowDebate(DebateId::new("d2"))
    ));
}

#[test]
fn an_ended_debate_shows_sides_moderator_and_verdict() {
    let (mut screen, mut text) = laid_out();
    let id = DebateId::new("d1");
    screen.run(Action::ShowDebate(id.clone()), &mut text);
    let session = screen.session.clone().unwrap();
    let Center::Debate(view) = &screen.center else {
        panic!("not in debate mode");
    };
    assert!(!view.editable(&session));
    assert!(has_widget_for(&screen, &Action::BackToChat));
    assert!(!has_widget_for(&screen, &Action::SwapSides));
    let [pro, mid, con] = debate::columns(&session, view);
    let authors = |ms: &[&Message]| -> BTreeSet<String> {
        ms.iter()
            .filter(|m| m.kind == MsgKind::Argument)
            .map(|m| author_name(&m.author))
            .collect()
    };
    assert_eq!(authors(&pro), BTreeSet::from(["codex".to_owned()]));
    assert_eq!(authors(&con), BTreeSet::from(["claude".to_owned()]));
    assert!(mid.iter().any(|m| m.kind == MsgKind::Verdict));
    assert!(mid.iter().any(|m| m.author == Author::Part(p("agy"))));
    // Every debate message lands in exactly one column.
    let total = session
        .messages
        .iter()
        .filter(|m| m.debate.as_ref() == Some(&id))
        .count();
    assert_eq!(pro.len() + mid.len() + con.len(), total);

    screen.run(Action::BackToChat, &mut text);
    assert_eq!(screen.center, Center::Chat);
}

// ── Composer ────────────────────────────────────────────────────────────────

struct Harness {
    screen: Discussion,
    text: TextStore,
    feed: TestFeed,
}

impl Harness {
    fn new(channel: crate::live::Channel, feed: TestFeed) -> Self {
        let mut h = Harness {
            screen: Discussion::live(channel),
            text: TextStore::new(Default::default()),
            feed,
        };
        h.screen.prepare(window(), &mut h.text);
        h
    }

    fn deliver(&mut self, session: Option<Session>) {
        self.feed
            .snapshots
            .send(Snapshot {
                link: Link::Live,
                session: session.map(std::sync::Arc::new),
            })
            .unwrap();
        self.screen.prepare(window(), &mut self.text);
    }

    fn drain(&self) -> Vec<Command> {
        self.feed.commands.try_iter().collect()
    }

    fn set_text(&mut self, area: WidgetId, body: &str) {
        let body = body.to_owned();
        self.screen
            .tree
            .update::<TextArea, _>(area, |a, cx| a.set_text(cx, body));
    }
}

#[test]
fn the_recipient_menu_lists_everyone_the_moderator_and_each_panelist() {
    let session = crate::demo::session();
    let labels: Vec<String> = composer::choices(&session)
        .into_iter()
        .map(|(label, _)| label)
        .collect();
    assert_eq!(labels, ["全員", "Noa（司会）", "claude", "codex", "agy"]);
}

#[test]
fn an_intervention_goes_to_the_chosen_recipient_and_clears_once_logged() {
    let (channel, feed) = test_channel();
    let mut h = Harness::new(channel, feed);
    let session = crate::demo::session();
    h.deliver(Some(session.clone()));
    let menu = h.screen.composer.recipients;
    h.screen
        .tree
        .update::<PopUpButton, _>(menu, |m, cx| m.set_selected(cx, 3));
    h.set_text(h.screen.composer.area, "  コストも見てください  ");
    let send = h.screen.composer.send;
    click(&mut h.screen, &mut h.text, send);
    assert_eq!(
        h.drain(),
        vec![Command::Intervene {
            to: vec![p("codex")],
            body: "コストも見てください".into(),
        }]
    );
    // Kept until the log has it.
    h.deliver(Some(session.clone()));
    assert_eq!(
        composer::text(&h.screen.tree, &h.screen.composer),
        "コストも見てください"
    );

    let mut logged = session;
    let mut message = logged.messages.last().unwrap().clone();
    message.id = logged.next_message_id(0);
    message.author = Author::User;
    message.kind = MsgKind::Intervention;
    message.to = vec![p("codex")];
    message.body = "コストも見てください".into();
    logged.messages.push(message);
    logged.rev += 1;
    h.deliver(Some(logged));
    assert_eq!(composer::text(&h.screen.tree, &h.screen.composer), "");
}

#[test]
fn an_empty_intervention_is_not_sent() {
    let (channel, feed) = test_channel();
    let mut h = Harness::new(channel, feed);
    h.deliver(Some(crate::demo::session()));
    h.screen.run(Action::Send, &mut h.text);
    assert!(h.drain().is_empty());
    assert_eq!(h.screen.toasts, ["介入の本文が空です"]);
}

// ── Start view ──────────────────────────────────────────────────────────────

#[test]
fn without_a_session_the_start_view_shows_and_starts_one() {
    let (channel, feed) = test_channel();
    let mut h = Harness::new(channel, feed);
    h.deliver(None);
    assert!(!h.screen.tree.is_hidden(h.screen.start.root));
    assert!(h.screen.tree.is_hidden(h.screen.meeting));

    let start = h.screen.start.start_button;
    click(&mut h.screen, &mut h.text, start);
    assert!(h.drain().is_empty(), "no topic yet");

    h.set_text(h.screen.start.topic, "テストの方針");
    let moderator = h.screen.start.moderator;
    h.screen
        .tree
        .update::<PopUpButton, _>(moderator, |m, cx| m.set_selected(cx, 1));
    let claude = h.screen.start.panel[0];
    h.screen.tree.update::<Checkbox, _>(claude, |c, cx| {
        c.set_state(cx, noa_gpui_core::widgets::CheckState::Off)
    });
    click(&mut h.screen, &mut h.text, start);
    assert_eq!(
        h.drain(),
        vec![Command::Start {
            topic: "テストの方針".into(),
            workdir: None,
            max_rounds: Some(5),
            moderator: AgentKind::Codex.into(),
            panel: vec![AgentKind::Codex.into(), AgentKind::Agy.into()],
        }]
    );

    // The session arrives: the meeting replaces the start view.
    h.deliver(Some(crate::demo::session()));
    assert!(h.screen.tree.is_hidden(h.screen.start.root));
    assert!(!h.screen.tree.is_hidden(h.screen.meeting));
}

#[test]
fn the_start_view_sends_each_agents_model_and_effort() {
    let (channel, feed) = test_channel();
    let mut h = Harness::new(channel, feed);
    h.deliver(None);
    h.set_text(h.screen.start.topic, "テストの方針");

    // Picking codex as moderator offers codex's models and levels.
    let moderator = h.screen.start.moderator;
    h.screen
        .tree
        .update::<PopUpButton, _>(moderator, |m, cx| m.set_selected(cx, 1));
    start::moderator_kind_changed(&mut h.screen.tree, &mut h.screen.start, &mut h.text);
    let setup = &h.screen.start.moderator_setup;
    let (model, effort) = (setup.model, setup.effort);
    let items = h
        .screen
        .tree
        .get::<ComboBox>(model)
        .unwrap()
        .items()
        .to_vec();
    assert_eq!(items, AgentKind::Codex.models());
    h.screen
        .tree
        .update::<ComboBox, _>(model, |c, cx| c.set_text(cx, " gpt-6.2-astra "));
    // 0 is the CLI default; codex's sixth level is ultra.
    h.screen
        .tree
        .update::<PopUpButton, _>(effort, |p, cx| p.set_selected(cx, 6));

    let claude = &h.screen.start.panel_setup[0];
    let (model, effort) = (claude.model, claude.effort);
    h.screen
        .tree
        .update::<ComboBox, _>(model, |c, cx| c.set_text(cx, "opus"));
    h.screen
        .tree
        .update::<PopUpButton, _>(effort, |p, cx| p.set_selected(cx, 3));

    let start = h.screen.start.start_button;
    click(&mut h.screen, &mut h.text, start);
    let setup = |model: Option<&str>, effort: Option<&str>| AgentSetup {
        model: model.map(Into::into),
        effort: effort.map(Into::into),
    };
    assert_eq!(
        h.drain(),
        vec![Command::Start {
            topic: "テストの方針".into(),
            workdir: None,
            max_rounds: Some(5),
            moderator: AgentChoice {
                kind: AgentKind::Codex,
                setup: setup(Some("gpt-6.2-astra"), Some("ultra")),
            },
            panel: vec![
                AgentChoice {
                    kind: AgentKind::Claude,
                    setup: setup(Some("opus"), Some("high")),
                },
                AgentKind::Codex.into(),
                AgentKind::Agy.into(),
            ],
        }]
    );
}

#[test]
fn a_start_without_panelists_is_refused() {
    let (channel, feed) = test_channel();
    let mut h = Harness::new(channel, feed);
    h.deliver(None);
    h.set_text(h.screen.start.topic, "x");
    for id in h.screen.start.panel.clone() {
        h.screen.tree.update::<Checkbox, _>(id, |c, cx| {
            c.set_state(cx, noa_gpui_core::widgets::CheckState::Off)
        });
    }
    h.screen.run(Action::Start, &mut h.text);
    assert!(h.drain().is_empty());
    assert_eq!(h.screen.toasts, ["パネリストを 1 人以上選んでください"]);
}

// ── Stored discussions ──────────────────────────────────────────────────────

impl Harness {
    fn deliver_with(&mut self, link: Link, session: Option<Session>) {
        self.feed
            .snapshots
            .send(Snapshot {
                link,
                session: session.map(std::sync::Arc::new),
            })
            .unwrap();
        self.screen.prepare(window(), &mut self.text);
    }

    fn list(&mut self, history: Vec<discussion::store::Summary>) {
        self.feed
            .history
            .send(std::sync::Arc::new(history))
            .unwrap();
        self.screen.prepare(window(), &mut self.text);
    }
}

fn demo_dir(id: &str) -> std::path::PathBuf {
    std::path::PathBuf::from("/demo/discussions").join(id)
}

#[test]
fn the_start_view_lists_stored_discussions_to_open_or_resume() {
    let (channel, feed) = test_channel();
    let mut h = Harness::new(channel, feed);
    h.deliver(None);
    h.list(crate::demo::history());
    // Each readable one opens read-only; only the one still going resumes;
    // the damaged one offers neither.
    for id in ["disc-demo-3", "disc-demo-1"] {
        assert!(
            has_widget_for(&h.screen, &Action::Open(demo_dir(id))),
            "{id}"
        );
    }
    assert!(has_widget_for(
        &h.screen,
        &Action::Resume(demo_dir("disc-demo-3"))
    ));
    assert!(!has_widget_for(
        &h.screen,
        &Action::Resume(demo_dir("disc-demo-1"))
    ));
    assert!(!has_widget_for(
        &h.screen,
        &Action::Open(demo_dir("disc-demo-2"))
    ));

    let open = widget_for(&h.screen, &Action::Open(demo_dir("disc-demo-1")));
    click(&mut h.screen, &mut h.text, open);
    let resume = widget_for(&h.screen, &Action::Resume(demo_dir("disc-demo-3")));
    click(&mut h.screen, &mut h.text, resume);
    assert_eq!(
        h.drain(),
        vec![
            Command::Open {
                dir: demo_dir("disc-demo-1")
            },
            Command::Resume {
                dir: demo_dir("disc-demo-3")
            },
        ]
    );
}

#[test]
fn an_empty_history_says_so_and_still_starts() {
    let (channel, feed) = test_channel();
    let mut h = Harness::new(channel, feed);
    h.deliver(None);
    h.list(Vec::new());
    assert!(
        !h.screen
            .actions
            .iter()
            .any(|(_, a)| matches!(a, Action::Open(_) | Action::Resume(_)))
    );
    assert!(has_widget_for(&h.screen, &Action::Start));
}

#[test]
fn a_stored_discussion_shows_read_only_without_controls() {
    let (channel, feed) = test_channel();
    let mut h = Harness::new(channel, feed);
    h.deliver(None);
    h.list(crate::demo::history());
    let open = widget_for(&h.screen, &Action::Open(demo_dir("disc-demo-1")));
    click(&mut h.screen, &mut h.text, open);
    h.drain();
    // A failed brief, an issue that could be debated, a working directory:
    // live, each would offer a control.
    let mut session = crate::demo::session();
    session.workdir = Some(std::path::PathBuf::from("/repo"));
    session.rounds.last_mut().unwrap().briefs[0].receipt.failed = Some("gone".into());
    session.closed = true;
    h.deliver_with(Link::Stored, Some(session));
    assert!(!h.screen.tree.is_hidden(h.screen.meeting));
    assert_eq!(status_badges(&h.screen)[0], "記録");
    for action in h.screen.actions.iter().map(|(_, a)| a) {
        assert!(!action.acts_on_session(), "{action:?} offered read-only");
    }
    assert!(h.screen.tree.is_hidden(h.screen.composer.root));
    // Reading stays: filters, the map, copying the result.
    assert!(has_widget_for(&h.screen, &Action::Filter(i("B"))));
    assert!(has_widget_for(&h.screen, &Action::CopyResult));
    // Ended: nothing to resume; it can be left for a new discussion.
    assert!(
        !h.screen
            .actions
            .iter()
            .any(|(_, a)| matches!(a, Action::Resume(_)))
    );
    let new = widget_for(&h.screen, &Action::NewDiscussion);
    click(&mut h.screen, &mut h.text, new);
    assert_eq!(h.drain(), vec![Command::NewDiscussion]);
}

#[test]
fn an_unfinished_stored_discussion_can_be_resumed_from_its_view() {
    let (channel, feed) = test_channel();
    let mut h = Harness::new(channel, feed);
    h.deliver(None);
    h.list(crate::demo::history());
    let open = widget_for(&h.screen, &Action::Open(demo_dir("disc-demo-3")));
    click(&mut h.screen, &mut h.text, open);
    h.drain();
    h.deliver_with(Link::Stored, Some(crate::demo::session()));
    assert!(!has_widget_for(&h.screen, &Action::Close));
    let resume = widget_for(&h.screen, &Action::Resume(demo_dir("disc-demo-3")));
    click(&mut h.screen, &mut h.text, resume);
    assert_eq!(
        h.drain(),
        vec![Command::Resume {
            dir: demo_dir("disc-demo-3")
        }]
    );
    // Resumed, the runner's session is live again: the controls are back.
    h.deliver(Some(crate::demo::session()));
    assert!(has_widget_for(&h.screen, &Action::Close));
    assert!(!h.screen.tree.is_hidden(h.screen.composer.root));
}

#[test]
fn a_new_discussion_returns_to_the_start_view_with_a_fresh_form_state() {
    let (channel, feed) = test_channel();
    let mut h = Harness::new(channel, feed);
    let session = crate::demo::session();
    h.deliver(Some(session.clone()));
    h.screen.run(Action::Filter(i("B")), &mut h.text);
    h.screen.prepare(window(), &mut h.text);
    let new = widget_for(&h.screen, &Action::NewDiscussion);
    click(&mut h.screen, &mut h.text, new);
    assert_eq!(h.drain(), vec![Command::NewDiscussion]);
    // The live layer stops the runner, then lists and shows the start view.
    h.list(crate::demo::history());
    h.deliver(None);
    assert!(!h.screen.tree.is_hidden(h.screen.start.root));
    assert!(h.screen.tree.is_hidden(h.screen.meeting));
    assert_eq!(h.screen.filter, None);
    assert!(has_widget_for(
        &h.screen,
        &Action::Open(demo_dir("disc-demo-1"))
    ));
    // And a new start goes out as the first one did.
    h.set_text(h.screen.start.topic, "次の議題");
    let start = h.screen.start.start_button;
    click(&mut h.screen, &mut h.text, start);
    assert!(matches!(&h.drain()[..], [Command::Start { topic, .. }] if topic == "次の議題"));
    // The same session shown again later is built afresh.
    h.deliver(Some(session));
    assert!(!h.screen.tree.is_hidden(h.screen.meeting));
}

#[test]
fn stored_dates_and_round_counts_read_plainly() {
    use crate::ui::start::{date_label, rounds_label};
    assert_eq!(date_label(0), "1970-01-01 00:00");
    // 2026-11-23 14:05:59 UTC.
    assert_eq!(date_label(1_795_442_759), "2026-11-23 14:05");
    assert_eq!(date_label(951_782_400), "2000-02-29 00:00");
    assert_eq!(rounds_label(3, Some(5)), "3 / 5 ラウンド");
    assert_eq!(rounds_label(2, None), "2 ラウンド");
}

// ── Jumping to a pane ───────────────────────────────────────────────────────

#[test]
fn a_receipt_tag_or_an_agents_name_focuses_its_pane() {
    let (channel, feed) = test_channel();
    let mut h = Harness::new(channel, feed);
    let session = crate::demo::session();
    h.deliver(Some(session.clone()));
    let round = session.current_round().unwrap();
    // One link per receipt in the brief bar, the moderator's included.
    for brief in round.briefs.iter().chain(&round.moderator_brief) {
        assert!(has_widget_for(&h.screen, &Action::Focus(brief.to.clone())));
    }
    let codex = h
        .screen
        .actions
        .iter()
        .find(|(_, a)| *a == Action::Focus(p("codex")))
        .map(|(id, _)| *id)
        .unwrap();
    click(&mut h.screen, &mut h.text, codex);
    assert_eq!(h.drain(), vec![Command::Focus { part: p("codex") }]);
    // Every agent's message names a link too; the user's own does not.
    let links = h
        .screen
        .actions
        .iter()
        .filter(|(_, a)| matches!(a, Action::Focus(_)))
        .count();
    let briefs = round.briefs.len() + usize::from(round.moderator_brief.is_some());
    let agent_messages = chat::visible(&session, None)
        .iter()
        .filter(|m| matches!(m.author, Author::Part(_)))
        .count();
    assert_eq!(links, briefs + agent_messages);
}

#[test]
fn a_stored_discussion_has_no_panes_to_jump_to() {
    let (channel, feed) = test_channel();
    let mut h = Harness::new(channel, feed);
    h.deliver_with(Link::Stored, Some(crate::demo::session()));
    assert!(
        !h.screen
            .actions
            .iter()
            .any(|(_, a)| matches!(a, Action::Focus(_)))
    );
}

#[test]
fn the_demo_only_says_which_pane_it_would_show() {
    let (mut screen, mut text) = laid_out();
    screen.run(Action::Focus(p(MODERATOR_ID)), &mut text);
    assert_eq!(screen.toasts, ["[DEMO] Noa のペインを表示します"]);
}

// ── Holding and extending ───────────────────────────────────────────────────

fn limited(mut session: Session, max: RoundNo) -> Session {
    session.max_rounds = Some(max);
    session.user_max_rounds = Some(max);
    // Another rev, as the log would have: the panes are built anew.
    session.rev += 100;
    session
}

#[test]
fn the_hold_toggle_sends_the_other_state() {
    let (channel, feed) = test_channel();
    let mut h = Harness::new(channel, feed);
    let mut session = crate::demo::session();
    h.deliver(Some(session.clone()));
    assert!(!has_widget_for(&h.screen, &Action::Continue));
    let hold = widget_for(&h.screen, &Action::SetHold(true));
    click(&mut h.screen, &mut h.text, hold);
    assert_eq!(h.drain(), vec![Command::SetHold { hold: true }]);
    session.hold = true;
    session.rev += 1;
    h.deliver(Some(session));
    // Held mid-round: nothing to continue yet.
    assert!(has_widget_for(&h.screen, &Action::SetHold(false)));
    assert!(!has_widget_for(&h.screen, &Action::Continue));
}

#[test]
fn a_held_discussion_offers_the_next_round_and_says_it_waits() {
    let (channel, feed) = test_channel();
    let mut h = Harness::new(channel, feed);
    let session = crate::demo::held();
    assert!(session.waits_to_continue());
    h.deliver(Some(session.clone()));
    assert!(status_badges(&h.screen).contains(&"一時停止中".to_owned()));
    let go = widget_for(&h.screen, &Action::Continue);
    click(&mut h.screen, &mut h.text, go);
    assert_eq!(h.drain(), vec![Command::Continue]);
    // The composer stays: an intervention now reaches the next brief.
    assert!(!h.screen.tree.is_hidden(h.screen.composer.root));
    // Read-only, a stored held discussion offers neither.
    h.deliver_with(Link::Stored, Some(session));
    assert!(!has_widget_for(&h.screen, &Action::Continue));
    assert!(
        !h.screen
            .actions
            .iter()
            .any(|(_, a)| matches!(a, Action::SetHold(_)))
    );
}

#[test]
fn more_rounds_are_offered_where_the_engine_takes_them() {
    let (channel, feed) = test_channel();
    let mut h = Harness::new(channel, feed);
    // No round limit: nothing to extend.
    h.deliver(Some(crate::demo::session()));
    assert!(!has_widget_for(&h.screen, &Action::Extend(1)));
    // Running with a limit: offered.
    let running = limited(crate::demo::session(), 8);
    h.deliver(Some(running.clone()));
    for by in EXTEND_BY {
        assert!(has_widget_for(&h.screen, &Action::Extend(by)));
    }
    let three = widget_for(&h.screen, &Action::Extend(3));
    click(&mut h.screen, &mut h.text, three);
    assert_eq!(h.drain(), vec![Command::Extend { by: 3 }]);
    // Ended early (round 5 of 8 was cut short): not offered.
    let mut early = running.clone();
    early.closed = true;
    early.rev += 1;
    h.deliver(Some(early));
    assert!(!has_widget_for(&h.screen, &Action::Extend(1)));
    // Ran out of rounds: offered, live and read-only (which resumes it).
    let mut ran_out = limited(crate::demo::held(), 4);
    ran_out.closed = true;
    ran_out.rev += 1;
    h.deliver(Some(ran_out.clone()));
    assert!(has_widget_for(&h.screen, &Action::Extend(1)));
    h.deliver_with(Link::Stored, Some(ran_out));
    let one = widget_for(&h.screen, &Action::Extend(1));
    click(&mut h.screen, &mut h.text, one);
    assert_eq!(h.drain(), vec![Command::Extend { by: 1 }]);
    // A stored discussion still going is resumed instead.
    h.deliver_with(Link::Stored, Some(limited(running, 8)));
    assert!(!has_widget_for(&h.screen, &Action::Extend(1)));
}

#[test]
fn every_status_control_fits_the_smallest_window() {
    let (w, h_) = MIN_WINDOW;
    let size = Size::new(w as f32, h_ as f32);
    let (channel, feed) = test_channel();
    let mut h = Harness::new(channel, feed);
    let mut session = limited(crate::demo::held(), 9);
    session.workdir = Some(std::path::PathBuf::from("/repo"));
    h.deliver(Some(session));
    h.screen.prepare(size, &mut h.text);
    assert!(has_widget_for(&h.screen, &Action::Continue));
    assert!(has_widget_for(&h.screen, &Action::Extend(3)));
    for &child in h.screen.tree.children(h.screen.status_trailing) {
        let [x, _, cw, _] = rect(&h.screen.tree, child);
        assert!(x >= 0.0 && x + cw <= size.width + 0.5, "{x}+{cw}");
    }
    let [lx, _, lw, _] = rect(&h.screen.tree, h.screen.status_leading);
    let [tx, _, _, _] = rect(&h.screen.tree, h.screen.status_trailing);
    assert!(
        lx + lw <= tx + 0.5,
        "leading {lx}+{lw} runs into trailing at {tx}"
    );
}

// ── Link state ──────────────────────────────────────────────────────────────

fn status_badges(screen: &Discussion) -> Vec<String> {
    screen
        .tree
        .children(screen.status_leading)
        .iter()
        .filter_map(|id| screen.tree.get::<Badge>(*id).and_then(Badge::label))
        .collect()
}

#[test]
fn the_status_bar_shows_the_link() {
    let (screen, _text) = laid_out();
    assert_eq!(status_badges(&screen)[0], "DEMO");
    let down = Discussion::of_snapshot(Snapshot {
        link: Link::Down("refused".into()),
        session: Some(std::sync::Arc::new(crate::demo::session())),
    });
    assert_eq!(status_badges(&down)[0], "オフライン");
    let live = Discussion::of_snapshot(Snapshot {
        link: Link::Live,
        session: Some(std::sync::Arc::new(crate::demo::session())),
    });
    assert_eq!(status_badges(&live)[0], "LIVE");
}

#[test]
fn an_unchanged_snapshot_does_not_rebuild_the_panes() {
    let (channel, feed) = test_channel();
    let mut h = Harness::new(channel, feed);
    h.deliver(Some(crate::demo::session()));
    let before = h.screen.tree.children(h.screen.center_host).to_vec();
    h.deliver(Some(crate::demo::session()));
    assert_eq!(h.screen.tree.children(h.screen.center_host), before);
}

#[test]
fn notices_from_the_live_layer_are_toasted() {
    let (channel, feed) = test_channel();
    let mut h = Harness::new(channel, feed);
    h.feed.notices.send("送信失敗: pane gone".into()).unwrap();
    h.screen.prepare(window(), &mut h.text);
    assert_eq!(h.screen.toasts, ["送信失敗: pane gone"]);
}

#[test]
fn the_start_views_workdir_is_empty_home_relative_or_an_existing_directory() {
    use crate::ui::start::workdir;
    assert_eq!(workdir(""), Ok(None));
    let home = std::path::PathBuf::from(std::env::var_os("HOME").unwrap());
    assert_eq!(workdir("~"), Ok(Some(home.clone())));
    let tmp = std::env::temp_dir();
    assert_eq!(workdir(tmp.to_str().unwrap()), Ok(Some(tmp.clone())));
    assert!(workdir("relative/dir").unwrap_err().contains("絶対パス"));
    assert!(
        workdir("/no/such/dir/for/noa")
            .unwrap_err()
            .contains("ディレクトリではありません")
    );
}

#[test]
fn the_round_limit_is_sent_with_the_start() {
    let (channel, feed) = test_channel();
    let mut h = Harness::new(channel, feed);
    h.deliver(None);
    h.set_text(h.screen.start.topic, "t");
    let rounds = h.screen.start.max_rounds;
    h.screen
        .tree
        .update::<noa_gpui_core::widgets::NumberInput, _>(rounds, |n, cx| n.set_value(cx, 3.0));
    let start = h.screen.start.start_button;
    click(&mut h.screen, &mut h.text, start);
    let sent = h.drain();
    assert!(
        matches!(
            &sent[..],
            [Command::Start {
                max_rounds: Some(3),
                ..
            }]
        ),
        "{sent:?}"
    );
}

#[test]
fn ending_the_discussion_is_one_click_until_it_has_ended() {
    let (channel, feed) = test_channel();
    let mut h = Harness::new(channel, feed);
    let mut session = crate::demo::session();
    h.deliver(Some(session.clone()));
    let end = widget_for(&h.screen, &Action::Close);
    click(&mut h.screen, &mut h.text, end);
    assert_eq!(h.drain(), vec![Command::Close]);
    session.closed = true;
    session.rev += 1;
    h.deliver(Some(session));
    assert!(!has_widget_for(&h.screen, &Action::Close));
}

// ── Results ─────────────────────────────────────────────────────────────────

#[test]
fn copying_the_result_puts_its_markdown_on_the_clipboard() {
    let (channel, feed) = test_channel();
    let mut h = Harness::new(channel, feed);
    let session = crate::demo::session();
    h.deliver(Some(session.clone()));
    let copy = widget_for(&h.screen, &Action::CopyResult);
    click(&mut h.screen, &mut h.text, copy);
    assert_eq!(
        h.screen.tree.clipboard().read_text(),
        Some(discussion::model::export::result_markdown(&session))
    );
    assert!(h.screen.toasts.iter().any(|t| t.contains("コピー")));
}

#[test]
fn exporting_the_minutes_asks_where_and_writes_the_file() {
    use noa_gpui_platform::FileDialogResult;
    let (channel, feed) = test_channel();
    let mut h = Harness::new(channel, feed);
    let session = crate::demo::session();
    h.deliver(Some(session.clone()));
    let export = widget_for(&h.screen, &Action::ExportMinutes);
    click(&mut h.screen, &mut h.text, export);
    let dialogs = h.screen.take_file_dialogs();
    assert_eq!(dialogs.len(), 1);
    assert!(dialogs[0].save);
    assert!(
        dialogs[0].file_name.ends_with("-minutes.md"),
        "{}",
        dialogs[0].file_name
    );
    let path =
        std::env::temp_dir().join(format!("noa-discussion-minutes-{}.md", std::process::id()));
    h.screen
        .file_dialog_finished(FileDialogResult::Saved(path.clone()), &mut h.text);
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        discussion::model::export::minutes_markdown(&session)
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn an_implementation_pane_is_offered_only_with_a_working_directory() {
    let (channel, feed) = test_channel();
    let mut h = Harness::new(channel, feed);
    let mut session = crate::demo::session();
    h.deliver(Some(session.clone()));
    assert!(!has_widget_for(
        &h.screen,
        &Action::Implement(AgentKind::Claude)
    ));
    session.workdir = Some(std::path::PathBuf::from("/repo"));
    session.rev += 1;
    h.deliver(Some(session));
    for kind in AgentKind::ALL {
        assert!(has_widget_for(&h.screen, &Action::Implement(kind)));
    }
    let codex = widget_for(&h.screen, &Action::Implement(AgentKind::Codex));
    click(&mut h.screen, &mut h.text, codex);
    assert_eq!(
        h.drain(),
        vec![Command::Implement {
            agent: AgentKind::Codex
        }]
    );
}

#[test]
fn an_ended_discussion_offers_no_retry_and_no_debate_controls() {
    let (channel, feed) = test_channel();
    let mut h = Harness::new(channel, feed);
    // A failed brief and a proposed debate, then the user ends it.
    let mut session = with_proposal();
    let round = session.rounds.last_mut().unwrap();
    round.briefs[2].receipt.failed = Some("pane gone".into());
    session.closed = true;
    h.deliver(Some(session));
    assert!(!has_widget_for(&h.screen, &Action::Retry));
    assert!(!has_widget_for(
        &h.screen,
        &Action::ShowDebate(DebateId::new("d2"))
    ));
    assert_eq!(
        h.screen.center,
        Center::Chat,
        "the proposal does not come up"
    );
    // Opened anyway, the proposed debate is read-only.
    h.screen
        .run(Action::ShowDebate(DebateId::new("d2")), &mut h.text);
    assert!(!has_widget_for(&h.screen, &Action::ConfirmDebate));
    assert!(!has_widget_for(&h.screen, &Action::SwapSides));
}

// ── Map ─────────────────────────────────────────────────────────────────────

#[test]
fn the_map_tab_shows_every_issue_and_a_click_filters_the_chat() {
    let (mut screen, mut text) = laid_out();
    let map = widget_for(&screen, &Action::ShowMap);
    click(&mut screen, &mut text, map);
    assert_eq!(screen.center, Center::Map);
    // Every issue has a node on the map that opens it.
    let node_for = |s: &Discussion, issue: &IssueId| {
        s.actions
            .iter()
            .find(|(id, a)| s.map_nodes.contains(id) && *a == Action::Filter(issue.clone()))
            .map(|(id, _)| *id)
    };
    let issues: Vec<IssueId> = screen
        .session
        .as_ref()
        .unwrap()
        .issues
        .issues
        .iter()
        .map(|i| i.id.clone())
        .collect();
    assert!(!issues.is_empty());
    for issue in &issues {
        assert!(node_for(&screen, issue).is_some(), "no node for {issue}");
    }
    // A map node opens the chat filtered to its issue.
    let node = node_for(&screen, &i("C")).unwrap();
    click(&mut screen, &mut text, node);
    assert_eq!(screen.center, Center::Chat);
    assert_eq!(screen.filter, Some(i("C")));
}

#[test]
fn the_map_lays_its_nodes_out_without_overlap() {
    let (mut screen, mut text) = laid_out();
    screen.run(Action::ShowMap, &mut text);
    screen.prepare(window(), &mut text);
    let nodes: Vec<[f32; 4]> = screen
        .map_nodes
        .iter()
        .map(|id| rect(&screen.tree, *id))
        .collect();
    let session = screen.session.as_ref().unwrap();
    assert_eq!(
        nodes.len(),
        discussion::model::map::layout(session).nodes.len()
    );
    for (n, a) in nodes.iter().enumerate() {
        for b in &nodes[n + 1..] {
            let apart = a[0] + a[2] <= b[0] + 0.5
                || b[0] + b[2] <= a[0] + 0.5
                || a[1] + a[3] <= b[1] + 0.5
                || b[1] + b[3] <= a[1] + 0.5;
            assert!(apart, "{a:?} overlaps {b:?}");
        }
    }
    let chat = widget_for(&screen, &Action::ShowChat);
    click(&mut screen, &mut text, chat);
    assert_eq!(screen.center, Center::Chat);
}

#[test]
fn the_map_scrolls_both_ways() {
    let (mut screen, mut text) = laid_out();
    screen.run(Action::ShowMap, &mut text);
    // A small window: the demo's map is wider and taller than the pane.
    let small = Size::new(1122.0, 600.0);
    screen.prepare(small, &mut text);
    let topic = screen.map_nodes[0];
    let before = rect(&screen.tree, topic);
    let pos = Point::new(before[0] + 10.0, before[1] + 10.0);
    screen.dispatch(
        Event::Wheel {
            pos,
            delta: Point::new(-120.0, -80.0),
        },
        &mut text,
    );
    screen.prepare(small, &mut text);
    let after = rect(&screen.tree, topic);
    assert!(
        after[0] < before[0] - 1.0,
        "no horizontal scroll: {before:?} -> {after:?}"
    );
    assert!(
        after[1] < before[1] - 1.0,
        "no vertical scroll: {before:?} -> {after:?}"
    );
}

#[test]
fn the_map_saves_as_a_png_of_the_whole_map() {
    use noa_gpui_platform::FileDialogResult;
    let (channel, feed) = test_channel();
    let mut h = Harness::new(channel, feed);
    let session = crate::demo::session();
    h.deliver(Some(session.clone()));
    // Only on the map.
    assert!(!has_widget_for(&h.screen, &Action::ExportMap));
    h.screen.run(Action::ShowMap, &mut h.text);
    h.screen.prepare(window(), &mut h.text);
    let save = widget_for(&h.screen, &Action::ExportMap);
    click(&mut h.screen, &mut h.text, save);
    let dialogs = h.screen.take_file_dialogs();
    assert_eq!(dialogs.len(), 1);
    assert!(dialogs[0].save);
    assert!(
        dialogs[0].file_name.ends_with("-map.png"),
        "{}",
        dialogs[0].file_name
    );
    let path = std::env::temp_dir().join(format!("noa-discussion-map-{}.png", std::process::id()));
    h.screen
        .file_dialog_finished(FileDialogResult::Saved(path.clone()), &mut h.text);
    let png = std::fs::read(&path).unwrap();
    assert_eq!(&png[1..4], b"PNG");
    let width = u32::from_be_bytes(png[16..20].try_into().unwrap());
    let height = u32::from_be_bytes(png[20..24].try_into().unwrap());
    let map = discussion::model::map::layout(&session);
    let (w, hgt) = (map.width.ceil() as u32, map.height.ceil() as u32);
    // At 2x: the whole map, not the part the pane shows.
    assert_eq!((width, height), (w * 2, hgt * 2));
    assert!(
        h.screen.toasts.iter().any(|t| t.contains("画像")),
        "{:?}",
        h.screen.toasts
    );
    let _ = std::fs::remove_file(&path);
}

/// The first scroll view under `host`, depth first.
fn scroll_under(tree: &UiTree, host: WidgetId) -> WidgetId {
    let mut stack = vec![host];
    while let Some(id) = stack.pop() {
        if tree.get::<ScrollView>(id).is_some() {
            return id;
        }
        stack.extend(tree.children(id).iter().rev().copied());
    }
    panic!("no scroll view under {host:?}");
}

fn scroll_position(tree: &UiTree, id: WidgetId) -> Point {
    tree.get::<ScrollView>(id).unwrap().scroll_position()
}

/// Scrolls the first scroll view under `host` by `delta`, then delivers the
/// same session one revision later, as a receipt update does. Returns the
/// position before and after the update.
fn scrolled_across_an_update(
    h: &mut Harness,
    host: WidgetId,
    delta: Point,
    size: Size,
) -> (Point, Point) {
    h.screen.prepare(size, &mut h.text);
    let scroll = scroll_under(&h.screen.tree, host);
    let [x, y, w, height] = rect(&h.screen.tree, scroll);
    h.screen.dispatch(
        Event::Wheel {
            pos: Point::new(x + w / 2.0, y + height / 2.0),
            delta,
        },
        &mut h.text,
    );
    h.screen.prepare(size, &mut h.text);
    let before = scroll_position(&h.screen.tree, scroll);
    let mut session = (**h.screen.session.as_ref().unwrap()).clone();
    session.rev += 1;
    h.feed
        .snapshots
        .send(Snapshot {
            link: Link::Live,
            session: Some(std::sync::Arc::new(session)),
        })
        .unwrap();
    h.screen.prepare(size, &mut h.text);
    let after = scroll_position(&h.screen.tree, scroll_under(&h.screen.tree, host));
    (before, after)
}

#[test]
fn a_live_update_keeps_the_chat_where_the_user_scrolled_it() {
    let (channel, feed) = test_channel();
    let mut h = Harness::new(channel, feed);
    h.deliver(Some(crate::demo::session()));
    let host = h.screen.center_host;
    // Up from the end, where the chat opens.
    let (before, after) = scrolled_across_an_update(&mut h, host, Point::new(0.0, 300.0), window());
    let scroll = scroll_under(&h.screen.tree, host);
    assert!(
        !h.screen
            .tree
            .get::<ScrollView>(scroll)
            .unwrap()
            .is_following_end(),
        "the chat went back to its end"
    );
    assert!((after.y - before.y).abs() < 1.0, "{before:?} -> {after:?}");
}

#[test]
fn a_live_update_keeps_the_map_where_the_user_panned_it() {
    let (channel, feed) = test_channel();
    let mut h = Harness::new(channel, feed);
    h.deliver(Some(crate::demo::session()));
    h.screen.run(Action::ShowMap, &mut h.text);
    h.screen.prepare(window(), &mut h.text);
    let host = h.screen.center_host;
    let (before, after) =
        scrolled_across_an_update(&mut h, host, Point::new(-120.0, -80.0), window());
    assert!(before.x > 1.0, "the map did not scroll: {before:?}");
    assert!(
        (after.x - before.x).abs() < 1.0 && (after.y - before.y).abs() < 1.0,
        "{before:?} -> {after:?}"
    );
}

#[test]
fn a_live_update_keeps_the_tree_and_the_minutes_where_they_were() {
    let (channel, feed) = test_channel();
    let mut h = Harness::new(channel, feed);
    // Taller than both panes.
    let mut session = crate::demo::session();
    for n in 0..12 {
        let mut issue = session.issues.issues[0].clone();
        issue.id = IssueId::new(format!("Z{n}"));
        issue.parent = None;
        session.issues.issues.push(issue);
        let item = session.minutes.section(MinuteSection::Decisions)[0].clone();
        session
            .minutes
            .section_mut(MinuteSection::Decisions)
            .push(item);
    }
    h.deliver(Some(session));
    for host in [h.screen.tree_host, h.screen.minutes_host] {
        let (before, after) = scrolled_across_an_update(
            &mut h,
            host,
            Point::new(0.0, -200.0),
            Size::new(MIN_WINDOW.0 as f32, MIN_WINDOW.1 as f32),
        );
        assert!(before.y > 1.0, "did not scroll: {before:?}");
        assert!((after.y - before.y).abs() < 1.0, "{before:?} -> {after:?}");
    }
}
