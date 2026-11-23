//! The feed's worker actions: a message never typed into a pane back at its
//! shell, and closing every ended worker of a filter at once.

use super::*;

/// `held_feed`, with the receivers of the feed's notices and of the
/// messages it gives back undelivered.
#[allow(clippy::type_complexity)]
fn told_feed(
    clock: &TestClock,
) -> (
    Feed,
    Receiver<Snapshot>,
    Sender<Command>,
    Receiver<String>,
    Receiver<Unsent>,
) {
    let (snapshots, published) = mpsc::channel();
    let (commands, command_rx) = mpsc::channel();
    let (notices, notice_rx) = mpsc::channel();
    let (unsent, unsent_rx) = mpsc::channel();
    let feed = Feed::new(
        snapshots,
        notices,
        unsent,
        command_rx,
        Arc::new(|| {}),
        clock.boxed(),
    );
    (feed, published, commands, notice_rx, unsent_rx)
}

fn send(commands: &Sender<Command>, text: &str) {
    commands
        .send(Command::Send {
            instance: "a".into(),
            pane: 2,
            text: text.into(),
        })
        .unwrap();
}

#[test]
fn both_writes_of_a_message_refuse_a_shell() {
    let clock = TestClock::new();
    let (mut feed, _published, commands, _notices, unsent) = told_feed(&clock);
    let mut noa = ScriptedNoa::showing("a", "$ ready", &clock);
    send(&commands, "continue");
    assert!(matches!(feed.follow(&mut noa), Err(Stop::Lost(_))));
    assert_eq!(noa.sent.len(), 2);
    assert_eq!(noa.refuse_shell, [true, true]);
    assert!(unsent.try_recv().is_err(), "a delivered message stays sent");
}

/// The agent exited back to the shell: Noa refuses the paste, nothing is
/// typed there, and the message comes back as the draft with a notice.
#[test]
fn a_message_for_a_pane_back_at_its_shell_comes_back_as_the_draft() {
    let clock = TestClock::new();
    let (mut feed, _published, commands, notices, unsent) = told_feed(&clock);
    let mut noa = ScriptedNoa::showing("a", "$ ", &clock);
    noa.shells.insert(2, 0);
    send(&commands, "rm -rf build");
    assert!(matches!(feed.follow(&mut noa), Err(Stop::Lost(_))));
    assert!(noa.sent.is_empty(), "{:?}", noa.sent);
    assert_eq!(noa.refuse_shell, [true]);
    assert_eq!(
        unsent.try_recv().unwrap(),
        Unsent {
            instance: "a".into(),
            pane: 2,
            text: "rm -rf build".into(),
        }
    );
    let said: Vec<String> = notices.try_iter().collect();
    assert_eq!(said, [SHELL_SEND_REFUSED]);
}

/// The agent exited between the paste and the Enter: the Enter is not
/// sent to the shell, and the notice says why.
#[test]
fn the_enter_after_a_paste_is_not_sent_to_a_shell() {
    let clock = TestClock::new();
    let (mut feed, _published, commands, notices, unsent) = told_feed(&clock);
    let mut noa = ScriptedNoa::showing("a", "$ ", &clock);
    noa.shells.insert(2, 1);
    send(&commands, "go on");
    assert!(matches!(feed.follow(&mut noa), Err(Stop::Lost(_))));
    assert_eq!(noa.sent, [(2, "go on".to_owned(), true)]);
    assert_eq!(noa.refuse_shell, [true, true]);
    assert!(unsent.try_recv().is_err(), "the paste was delivered");
    let said: Vec<String> = notices.try_iter().collect();
    assert_eq!(said, [SHELL_SUBMIT_REFUSED]);
}

/// A feed and the dashboard on its other end, with a frame and an input
/// as the window gives them.
struct Bench {
    feed: Feed,
    dashboard: Dashboard,
    text: TextStore,
}

const WINDOW: Size = Size::new(1440.0, 900.0);

impl Bench {
    fn new(clock: &TestClock) -> Self {
        let (snapshot_tx, snapshots) = mpsc::channel();
        let (commands, command_rx) = mpsc::channel();
        let (notice_tx, notices) = mpsc::channel();
        let (unsent_tx, unsent) = mpsc::channel();
        let feed = Feed::new(
            snapshot_tx,
            notice_tx,
            unsent_tx,
            command_rx,
            Arc::new(|| {}),
            clock.boxed(),
        );
        let channel = Channel {
            snapshots,
            notices,
            unsent,
            commands,
            _heartbeat: mpsc::channel().0,
        };
        let mut screen = Bench {
            feed,
            dashboard: Dashboard::live(Grouping::Orchestrator, channel),
            text: TextStore::new(Default::default()),
        };
        screen.frame();
        screen
    }

    fn frame(&mut self) {
        self.dashboard.before_frame(&mut self.text);
        self.dashboard.prepare(WINDOW, &mut self.text);
    }

    fn input(&mut self, event: Event) {
        self.dashboard.dispatch(event, &mut self.text);
        self.dashboard.prepare(WINDOW, &mut self.text);
    }

    fn key(&mut self, key: Key) {
        self.input(Event::KeyDown {
            key,
            modifiers: Modifiers::default(),
        });
    }

    /// A letter as the platform gives it: the key, then its text (Shift
    /// held for a capital).
    fn letter(&mut self, letter: char) {
        let modifiers = Modifiers {
            shift: letter.is_ascii_uppercase(),
            ..Modifiers::default()
        };
        self.input(Event::KeyDown {
            key: Key::Char(letter.to_ascii_lowercase()),
            modifiers,
        });
        self.input(Event::Text(letter.to_string()));
    }

    /// Lets the feed follow `noa` until its script ends, then takes in
    /// what it sent.
    fn follow(&mut self, noa: &mut ScriptedNoa) {
        assert!(matches!(self.feed.follow(noa), Err(Stop::Lost(_))));
        self.frame();
    }

    fn labels(&self) -> Vec<String> {
        let tree = self.dashboard.tree();
        tree.root()
            .into_iter()
            .chain(tree.overlays())
            .flat_map(|root| labels_under(tree, root))
            .collect()
    }
}

#[test]
fn a_message_refused_at_a_shell_is_the_draft_again_with_a_notice() {
    let clock = TestClock::new();
    let mut screen = Bench::new(&clock);
    screen.follow(&mut ScriptedNoa::showing("a", "$ ready", &clock));
    screen.key(Key::Char('j'));
    screen.letter('s');
    screen.input(Event::Text("deploy".into()));
    screen.key(Key::Enter);
    let mut noa = ScriptedNoa::showing("a", "$ ", &clock);
    noa.shells.insert(2, 0);
    screen.follow(&mut noa);
    assert!(noa.sent.is_empty());
    assert!(
        field_texts(screen.dashboard.tree()).contains(&"deploy".to_owned()),
        "{:?}",
        field_texts(screen.dashboard.tree())
    );
    let labels = screen.labels();
    assert!(labels.iter().any(|l| l == SHELL_SEND_REFUSED), "{labels:?}");
}

/// Pane 1 orchestrates 2 (running), 3 (finished), 4 (failed) and 5
/// (waiting for an approval).
fn ended_and_not(instance: &str, clock: &TestClock) -> ScriptedNoa {
    let mut noa = ScriptedNoa::showing(instance, "$ ready", clock);
    let worker = |pane: &str, state: &str, exit: Option<i64>| {
        let mut value = serde_json::json!({
            "paneId": pane,
            "parentPaneId": "1",
            "label": format!("w{pane}"),
            "agent": { "seq": 1, "kind": "claude", "state": state },
        });
        if let Some(code) = exit {
            value["exit"] = serde_json::json!({ "code": code });
        }
        panel(value)
    };
    noa.panels.extend([
        worker("3", "finished", None),
        worker("4", "running", Some(2)),
        worker("5", "permission", None),
    ]);
    noa
}

/// A bulk close of `panes` as the dashboard confirms it: each as
/// `ended_and_not` lists it ended — 3 finished, 4 failed, any other
/// finished (it was, when the dashboard showed it) — with report count 1.
fn close_many(commands: &Sender<Command>, instance: &str, panes: &[u64]) {
    let targets = panes
        .iter()
        .map(|&pane| CloseTarget {
            pane,
            state: if pane == 4 {
                State::Error
            } else {
                State::Finished
            },
            seq: 1,
        })
        .collect();
    commands
        .send(Command::CloseMany {
            instance: instance.into(),
            targets,
        })
        .unwrap();
}

#[test]
fn a_bulk_close_closes_only_what_is_as_confirmed_and_lists_once_after() {
    let clock = TestClock::new();
    let (mut feed, published, commands, notices, _unsent) = told_feed(&clock);
    let mut noa = ended_and_not("a", &clock);
    close_many(&commands, "a", &[2, 3, 4, 5]);
    assert!(matches!(feed.follow(&mut noa), Err(Stop::Lost(_))));
    assert_eq!(noa.closed, [3, 4]);
    // The first list on connecting, one right before each pane, and the
    // one list after.
    assert_eq!(noa.lists, 6);
    let last = published.try_iter().last().expect("a snapshot");
    let left: Vec<u64> = last.fleet.workers.iter().map(|w| w.pane).collect();
    assert_eq!(left, [2, 5]);
    let said: Vec<String> = notices.try_iter().collect();
    assert_eq!(
        said,
        ["2 件は確認した後に状態が変わったため閉じませんでした: claude、w5"]
    );
}

#[test]
fn a_bulk_close_chosen_on_another_instance_closes_nothing() {
    let clock = TestClock::new();
    let (mut feed, _published, commands, notices, _unsent) = told_feed(&clock);
    let mut restarted = ended_and_not("b", &clock);
    close_many(&commands, "a", &[3, 4]);
    assert!(matches!(feed.follow(&mut restarted), Err(Stop::Lost(_))));
    assert!(restarted.closed.is_empty(), "{:?}", restarted.closed);
    let said: Vec<String> = notices.try_iter().collect();
    assert_eq!(said, ["Noa が再起動したためペインを閉じませんでした"]);
}

#[test]
fn a_bulk_close_while_the_link_is_down_is_refused() {
    let clock = TestClock::new();
    let (mut feed, _published, commands, notices, _unsent) = told_feed(&clock);
    close_many(&commands, "a", &[3]);
    assert!(feed.pause(Duration::from_millis(1)));
    let said: Vec<String> = notices.try_iter().collect();
    assert_eq!(said, ["ペインを閉じられません: Noa に接続していません"]);
}

/// Review #1: another client gives pane 4 more work while pane 3 closes
/// (it runs again, or it already finished that work too: its report count
/// moved). Each pane is read again right before it is closed, and one
/// whose state or report count is not what the dashboard confirmed stays.
#[test]
fn a_pane_given_work_during_a_bulk_close_is_not_closed() {
    for (state, seq) in [("running", 2), ("finished", 2)] {
        let clock = TestClock::new();
        let (mut feed, _published, commands, notices, _unsent) = told_feed(&clock);
        let mut noa = ended_and_not("a", &clock);
        noa.after_close = Some(Box::new(move |closed, panels| {
            if closed == 3
                && let Some(four) = panels.iter_mut().find(|p| p.pane_id.0 == 4)
            {
                four.exit = None;
                let agent = four.agent.as_mut().unwrap();
                agent.state = Some(state.to_owned());
                agent.seq = seq;
            }
        }));
        close_many(&commands, "a", &[3, 4]);
        assert!(matches!(feed.follow(&mut noa), Err(Stop::Lost(_))));
        assert_eq!(noa.closed, [3], "{state} seq {seq}");
        let said: Vec<String> = notices.try_iter().collect();
        assert_eq!(
            said,
            ["1 件は確認した後に状態が変わったため閉じませんでした: w4"]
        );
    }
}

#[test]
fn only_a_noa_from_0_9_12_is_known_to_refuse_a_shell() {
    for (version, honours) in [
        ("0.9.12", true),
        ("0.9.14", true),
        ("0.9.12-dev", true),
        ("0.10.0", true),
        ("1.0.0", true),
        ("0.9.11", false),
        ("0.8.40", false),
        ("0.9", false),
        ("", false),
        ("garbage", false),
    ] {
        assert_eq!(honours_refuse_shell(version), honours, "{version}");
    }
}

/// Review #3: a Noa not known to honour `refuseShell` (older, or its
/// version unread) gets no message: it would type it into a shell.
#[test]
fn nothing_is_sent_to_a_noa_that_may_type_into_a_shell() {
    let clock = TestClock::new();
    let (mut feed, published, commands, notices, unsent) = told_feed(&clock);
    let mut noa = ScriptedNoa::showing("a", "$ ready", &clock);
    noa.refuses_shell_known = false;
    send(&commands, "go on");
    assert!(matches!(feed.follow(&mut noa), Err(Stop::Lost(_))));
    assert!(noa.sent.is_empty(), "{:?}", noa.sent);
    assert_eq!(unsent.try_recv().unwrap().text, "go on");
    let said: Vec<String> = notices.try_iter().collect();
    assert_eq!(said, [UNSAFE_SEND]);
    let last = published.try_iter().last().unwrap();
    assert!(!last.safe_send, "the dashboard is told");
}

/// The dashboard does not even ask: the drawer says why, and S's Enter
/// keeps the draft.
#[test]
fn the_dashboard_sends_nothing_to_a_noa_that_may_type_into_a_shell() {
    let clock = TestClock::new();
    let mut screen = Bench::new(&clock);
    let mut noa = ScriptedNoa::showing("a", "$ ready", &clock);
    noa.refuses_shell_known = false;
    screen.follow(&mut noa);
    screen.key(Key::Char('j'));
    screen.letter('s');
    screen.input(Event::Text("deploy".into()));
    screen.key(Key::Enter);
    let labels = screen.labels();
    assert!(
        labels.iter().filter(|l| *l == UNSAFE_SEND).count() >= 2,
        "{labels:?}"
    );
    assert!(field_texts(screen.dashboard.tree()).contains(&"deploy".to_owned()));
    let mut noa = ScriptedNoa::showing("a", "$ ready", &clock);
    noa.refuses_shell_known = false;
    screen.follow(&mut noa);
    assert!(noa.sent.is_empty());
}

/// The version comes from a `noa.hello` of our own, over a real socket.
#[test]
fn the_server_version_is_read_from_a_hello() {
    let listener = match std::net::TcpListener::bind("127.0.0.1:0") {
        Ok(listener) => listener,
        // A sandbox without loopback binding: nothing to test against.
        Err(err) if err.kind() == std::io::ErrorKind::PermissionDenied => {
            eprintln!("skipped: cannot bind a loopback port here: {err}");
            return;
        }
        Err(err) => panic!("{err}"),
    };
    let port = listener.local_addr().unwrap().port();
    let server = std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut socket = tungstenite::accept(stream).unwrap();
        let hello: serde_json::Value =
            serde_json::from_str(socket.read().unwrap().to_text().unwrap()).unwrap();
        assert_eq!(hello["method"], "noa.hello");
        assert_eq!(hello["params"]["token"], "secret");
        let reply = serde_json::json!({
            "jsonrpc": "2.0", "id": hello["id"],
            "result": { "protocolVersion": 3, "grantedScopes": ["read"], "serverVersion": "0.9.14" },
        });
        socket
            .send(tungstenite::Message::Text(reply.to_string()))
            .unwrap();
        let _ = socket.read();
    });
    let version = server_version(&format!("ws://127.0.0.1:{port}/"), "secret");
    server.join().unwrap();
    assert_eq!(version.as_deref(), Some("0.9.14"));
}
