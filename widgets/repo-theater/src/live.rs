//! Background live feed connecting to Noa's control server (`noa-ipc`) and
//! inspecting local git worktrees for every active pane.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::time::{Duration, Instant};

use noa_ipc::{
    Client, ClientError, EventKind, Panel, Scope, ScopeSet, SendTextOptions, TextSource,
};

use crate::git;
use crate::model::{AgentActivity, DiffMode, PaneSummary, TheaterData, Worktree, group_repos};

const BACKOFF_MIN: Duration = Duration::from_millis(500);
const BACKOFF_MAX: Duration = Duration::from_secs(4);
const POLL_WAIT: Duration = Duration::from_millis(250);
const REFRESH_EVERY: Duration = Duration::from_secs(3);
const SCREEN_MAX_BYTES: usize = 8192;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Link {
    Connecting,
    Live,
    Down(String),
}

#[derive(Debug, Clone)]
pub struct Snapshot {
    pub data: TheaterData,
    pub link: Link,
    pub can_focus: bool,
    pub can_send: bool,
    pub diff_mode: DiffMode,
    pub selected_diff: Option<(String, String, String)>,
}

#[derive(Debug, Clone)]
pub enum Command {
    Refresh,
    SetDiffMode(DiffMode),
    LoadDiff {
        cwd: String,
        path: String,
    },
    Focus(u64),
    SendText {
        pane_id: u64,
        text: String,
        submit: bool,
    },
}

pub struct Channel {
    pub snapshots: Receiver<Snapshot>,
    pub notices: Receiver<String>,
    /// Review prompts (`SendText { submit: true }`) the pane accepted.
    pub delivered: Receiver<String>,
    commands: Sender<Command>,
}

impl Channel {
    pub fn send(&self, cmd: Command) {
        let _ = self.commands.send(cmd);
    }
}

pub fn start(wake: impl Fn() + Send + Sync + 'static) -> Channel {
    let (snapshot_tx, snapshots) = mpsc::channel();
    let (notice_tx, notices) = mpsc::channel();
    let (delivered_tx, delivered) = mpsc::channel();
    let (commands, command_rx) = mpsc::channel();
    let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(wake);
    std::thread::Builder::new()
        .name("repo-theater-live".into())
        .spawn(move || run(snapshot_tx, notice_tx, delivered_tx, command_rx, wake))
        .expect("spawn repo-theater-live thread");
    Channel {
        snapshots,
        notices,
        delivered,
        commands,
    }
}

fn run(
    snapshot_tx: Sender<Snapshot>,
    notice_tx: Sender<String>,
    delivered_tx: Sender<String>,
    command_rx: Receiver<Command>,
    wake: Arc<dyn Fn() + Send + Sync>,
) {
    let mut backoff = BACKOFF_MIN;
    let mut mode = DiffMode::Unstaged;
    let mut selected_file: Option<(String, String)> = None;

    let _ = snapshot_tx.send(Snapshot {
        data: TheaterData::default(),
        link: Link::Connecting,
        can_focus: false,
        can_send: false,
        diff_mode: mode,
        selected_diff: None,
    });
    wake();

    loop {
        match connect() {
            Ok(mut client) => {
                backoff = BACKOFF_MIN;
                let scopes = client.granted_scopes();
                let can_focus = scopes.contains(Scope::Control);
                let can_send = scopes.contains(Scope::Input);
                let _ = client.subscribe(&[EventKind::StateChanged], None);

                let mut last_refresh = Instant::now() - REFRESH_EVERY;
                let mut dirty = true;

                loop {
                    while let Ok(cmd) = command_rx.try_recv() {
                        match cmd {
                            Command::Refresh => dirty = true,
                            Command::SetDiffMode(m) => {
                                mode = m;
                                dirty = true;
                            }
                            Command::LoadDiff { cwd, path } => {
                                selected_file = Some((cwd, path));
                                dirty = true;
                            }
                            Command::Focus(pane_id) => {
                                if let Err(err) = client.focus_pane(pane_id) {
                                    let _ = notice_tx.send(format!("フォーカス失敗: {err}"));
                                    wake();
                                } else {
                                    let _ = notice_tx
                                        .send(format!("ペイン #{pane_id} にフォーカスしました"));
                                    wake();
                                }
                            }
                            Command::SendText {
                                pane_id,
                                text,
                                submit,
                            } => {
                                let result =
                                    send_steps(&text, submit).into_iter().try_for_each(|step| {
                                        match step {
                                            SendStep::Type { text: chunk, paste } => {
                                                // A pane without bracketed paste would
                                                // still run each line: refuse instead.
                                                client.send_text_with(
                                                    pane_id,
                                                    &chunk,
                                                    SendTextOptions {
                                                        paste,
                                                        require_paste_mode: paste
                                                            && chunk.contains('\n'),
                                                    },
                                                )
                                            }
                                            SendStep::Pause(d) => {
                                                std::thread::sleep(d);
                                                Ok(())
                                            }
                                        }
                                    });
                                if let Err(err) = result {
                                    let _ = notice_tx.send(format!("送信失敗: {err}"));
                                } else {
                                    if submit {
                                        let _ = delivered_tx.send(text);
                                    }
                                    let _ = notice_tx
                                        .send(format!("ペイン #{pane_id} へ指示を送信しました"));
                                }
                                wake();
                            }
                        }
                    }

                    if dirty || last_refresh.elapsed() >= REFRESH_EVERY {
                        match build_snapshot(&mut client, mode, selected_file.as_ref()) {
                            Ok((data, selected_diff)) => {
                                if snapshot_tx
                                    .send(Snapshot {
                                        data,
                                        link: Link::Live,
                                        can_focus,
                                        can_send,
                                        diff_mode: mode,
                                        selected_diff,
                                    })
                                    .is_err()
                                {
                                    return;
                                }
                                wake();
                                last_refresh = Instant::now();
                                dirty = false;
                            }
                            Err(err) => {
                                let _ = snapshot_tx.send(Snapshot {
                                    data: TheaterData::default(),
                                    link: Link::Down(format!("接続が切れました: {err}")),
                                    can_focus: false,
                                    can_send: false,
                                    diff_mode: mode,
                                    selected_diff: None,
                                });
                                wake();
                                break;
                            }
                        }
                    }

                    match client.next_notification(POLL_WAIT) {
                        Ok(Some(_)) => dirty = true,
                        Ok(None) => {}
                        Err(err) => {
                            let _ = snapshot_tx.send(Snapshot {
                                data: TheaterData::default(),
                                link: Link::Down(format!("切断: {err}")),
                                can_focus: false,
                                can_send: false,
                                diff_mode: mode,
                                selected_diff: None,
                            });
                            wake();
                            break;
                        }
                    }
                }
            }
            Err(why) => {
                if snapshot_tx
                    .send(Snapshot {
                        data: TheaterData::default(),
                        link: Link::Down(why),
                        can_focus: false,
                        can_send: false,
                        diff_mode: mode,
                        selected_diff: None,
                    })
                    .is_err()
                {
                    return;
                }
                wake();
                let until = Instant::now() + backoff;
                while Instant::now() < until {
                    match command_rx.try_recv() {
                        Ok(Command::SetDiffMode(m)) => mode = m,
                        Ok(Command::LoadDiff { cwd, path }) => selected_file = Some((cwd, path)),
                        Ok(Command::Refresh) => break,
                        Ok(_) => {}
                        Err(TryRecvError::Disconnected) => return,
                        Err(TryRecvError::Empty) => std::thread::sleep(Duration::from_millis(100)),
                    }
                }
                backoff = (backoff * 2).min(BACKOFF_MAX);
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum SendStep {
    Type { text: String, paste: bool },
    Pause(Duration),
}

/// Pause between a pasted prompt and the Enter that submits it, so a TUI that
/// debounces paste sees two events (`noa +ctl send`'s `SUBMIT_DELAY`).
const SUBMIT_DELAY: Duration = Duration::from_millis(150);

/// How `text` is delivered to a pane: the text as one bracketed paste, so
/// embedded newlines are not each an Enter, then, after [`SUBMIT_DELAY`], a
/// raw CR when it should be submitted.
fn send_steps(text: &str, submit: bool) -> Vec<SendStep> {
    let mut steps = vec![SendStep::Type {
        text: text.to_owned(),
        paste: true,
    }];
    if submit {
        steps.push(SendStep::Pause(SUBMIT_DELAY));
        steps.push(SendStep::Type {
            text: "\r".to_owned(),
            paste: false,
        });
    }
    steps
}

type DiffEntry = (String, String, String);

fn build_snapshot(
    client: &mut Client,
    mode: DiffMode,
    selected: Option<&(String, String)>,
) -> Result<(TheaterData, Option<DiffEntry>), ClientError> {
    let panels = client.list_panels()?;
    let mut by_worktree: HashMap<String, (git::WorktreeGitInfo, Vec<PaneSummary>)> = HashMap::new();

    for panel in panels {
        let cwd = panel.cwd.trim();
        if cwd.is_empty() {
            continue;
        }
        let info = if let Some((existing, _)) = by_worktree.get(cwd) {
            existing.clone()
        } else {
            let Some(info) = git::inspect_worktree(cwd, mode) else {
                continue;
            };
            info
        };

        let tail = fetch_pane_tail(client, &panel);
        let summary = PaneSummary {
            pane_id: panel.pane_id.0,
            name: panel.name.clone(),
            label: panel.label.clone(),
            process: panel.process.clone(),
            activity: panel_activity(&panel),
            busy: panel.busy,
            attention: panel.attention,
            terminal_tail: tail,
        };
        let key = info.worktree_root.clone();
        by_worktree
            .entry(key)
            .or_insert_with(|| (info, Vec::new()))
            .1
            .push(summary);
    }

    let entries = by_worktree
        .into_values()
        .map(|(info, panes)| {
            let wt = Worktree {
                name: info.branch.clone(),
                cwd: info.worktree_root,
                branch: info.branch,
                ahead: info.ahead,
                behind: info.behind,
                panes,
                files: info.files,
                note: info.note,
            };
            (info.repo_name, info.repo_root, wt)
        })
        .collect();
    let data = group_repos(entries);

    let selected_diff = selected
        .map(|(cwd, path)| (cwd.clone(), path.clone(), git::file_diff(cwd, path, mode)))
        .or_else(|| {
            data.repos
                .first()
                .and_then(|r| r.worktrees.first())
                .and_then(|w| {
                    w.files.first().map(|f| {
                        (
                            w.cwd.clone(),
                            f.path.clone(),
                            git::file_diff(&w.cwd, &f.path, mode),
                        )
                    })
                })
        });

    Ok((data, selected_diff))
}

fn fetch_pane_tail(client: &mut Client, panel: &Panel) -> String {
    if let Ok(text) = client.get_text(panel.pane_id.0, TextSource::Screen, Some(SCREEN_MAX_BYTES))
        && !text.text.is_empty()
    {
        return text.text.replace('\n', "\r\n");
    }
    panel
        .preview
        .iter()
        .map(|row| {
            row.spans
                .iter()
                .map(|s| s.text.as_str())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\r\n")
}

fn panel_activity(panel: &Panel) -> AgentActivity {
    if let Some(agent) = &panel.agent {
        match agent.state.as_deref() {
            Some("permission" | "input") => return AgentActivity::Waiting,
            Some("running") => return AgentActivity::Running,
            Some("finished") => return AgentActivity::Done,
            _ => {}
        }
    }
    if panel.attention {
        AgentActivity::Waiting
    } else if panel.busy {
        AgentActivity::Running
    } else {
        AgentActivity::None
    }
}

fn connect() -> Result<Client, String> {
    let (config, _diagnostics) =
        noa_config::load_startup_config(noa_config::ConfigOverrides::default())
            .map_err(|err| format!("設定を読めません: {err:#}"))?;
    if !config.server_enable {
        return Err("設定でサーバーが無効です（server-enable = true が必要）".into());
    }
    let token = match config.server_token.filter(|token| !token.trim().is_empty()) {
        Some(token) => token,
        None => {
            let path = noa_config::server_token_path()
                .ok_or_else(|| "トークンの場所がわかりません".to_owned())?;
            std::fs::read_to_string(&path)
                .map(|token| token.trim().to_owned())
                .ok()
                .filter(|token| !token.is_empty())
                .ok_or_else(|| format!("トークンがありません: {}", path.display()))?
        }
    };
    let url = control_url(&config.server_bind, config.server_port);
    // Try read+control+input first; fall back to read+control, then read.
    for scopes in [
        &["read", "control", "input"][..],
        &["read", "control"][..],
        &["read"][..],
    ] {
        match Client::connect(&url, &token, ScopeSet::from_strings(scopes.iter().copied())) {
            Ok(client) => return Ok(client),
            Err(ClientError::Rpc { .. }) => continue,
            Err(other) => return Err(format!("{url} に接続できません: {other}")),
        }
    }
    Err(format!("{url} へのスコープ認可に失敗しました"))
}

fn control_url(bind: &str, port: u16) -> String {
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
    let ip = match bind.parse().unwrap_or(IpAddr::V4(Ipv4Addr::LOCALHOST)) {
        IpAddr::V4(v4) if v4.is_unspecified() => IpAddr::V4(Ipv4Addr::LOCALHOST),
        IpAddr::V6(v6) if v6.is_unspecified() => IpAddr::V6(Ipv6Addr::LOCALHOST),
        ip => ip,
    };
    format!("ws://{}/", SocketAddr::new(ip, port))
}

#[cfg(test)]
#[cfg(test)]
pub(crate) struct TestFeed {
    pub snapshots: Sender<Snapshot>,
    pub notices: Sender<String>,
    pub delivered: Sender<String>,
    pub commands: Receiver<Command>,
}

#[cfg(test)]
pub(crate) fn test_channel() -> (Channel, TestFeed) {
    let (snapshot_tx, snapshots) = mpsc::channel();
    let (notice_tx, notices) = mpsc::channel();
    let (delivered_tx, delivered) = mpsc::channel();
    let (commands, command_rx) = mpsc::channel();
    (
        Channel {
            snapshots,
            notices,
            delivered,
            commands,
        },
        TestFeed {
            snapshots: snapshot_tx,
            notices: notice_tx,
            delivered: delivered_tx,
            commands: command_rx,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn multiline_text_is_never_written_raw() {
        let text = "[main / a.rs]\n```diff\n+foo\n```\n修正依頼: 直して";
        let steps = send_steps(text, true);
        for step in &steps {
            if let SendStep::Type { text, paste } = step {
                assert!(
                    *paste || !text.contains('\n'),
                    "raw chunk with newline: {text:?}"
                );
            }
        }
        let typed = |text: &str, paste| SendStep::Type {
            text: text.to_owned(),
            paste,
        };
        assert_eq!(steps.first(), Some(&typed(text, true)));
        assert_eq!(steps.last(), Some(&typed("\r", false)));
        assert_eq!(send_steps("a b", false), vec![typed("a b", true)]);
    }

    #[test]
    fn enter_waits_for_the_paste_to_settle() {
        // Same pause `noa +ctl send` leaves (crates/noa-app/src/ctl.rs
        // SUBMIT_DELAY) so a TUI that debounces paste sees two events.
        let steps = send_steps("fix it", true);
        assert_eq!(steps.len(), 3, "{steps:?}");
        assert_eq!(steps[1], SendStep::Pause(Duration::from_millis(150)));
    }
}
