use noa_gpui_core::widgets::{Badge, Checkbox, PopUpButton, TextArea};
use noa_gpui_core::{Event, PointerButton};
use noa_gpui_scene::Point;

use discussion::model::types::{Debate, DebateStatus, MinuteSection};

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
    let mut session = screen.session.clone().unwrap();
    session.issues.issues.retain(|issue| issue.id != i("D"));
    session.rev += 1;
    screen.session = Some(session);
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
                session,
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
            moderator: AgentKind::Codex,
            panel: vec![AgentKind::Codex, AgentKind::Agy],
        }]
    );

    // The session arrives: the meeting replaces the start view.
    h.deliver(Some(crate::demo::session()));
    assert!(h.screen.tree.is_hidden(h.screen.start.root));
    assert!(!h.screen.tree.is_hidden(h.screen.meeting));
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
        session: Some(crate::demo::session()),
    });
    assert_eq!(status_badges(&down)[0], "オフライン");
    let live = Discussion::of_snapshot(Snapshot {
        link: Link::Live,
        session: Some(crate::demo::session()),
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
