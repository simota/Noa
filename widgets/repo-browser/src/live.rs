//! Background live feed connecting to Noa's control server (`noa-ipc`) and
//! inspecting local git worktrees for every active pane.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::time::{Duration, Instant};

use noa_ipc::{
    Client, ClientError, EventKind, Panel, Scope, ScopeSet, SendTextOptions, SpawnConfig,
    SpawnParams, TextSource,
};

use crate::git;
use crate::model::{AgentActivity, DiffMode, PaneSummary, RepoData, Worktree, group_repos};

const BACKOFF_MIN: Duration = Duration::from_millis(500);
const BACKOFF_MAX: Duration = Duration::from_secs(4);
const POLL_WAIT: Duration = Duration::from_millis(250);
const REFRESH_EVERY: Duration = Duration::from_secs(3);
/// The least time from one rebuild to the next that a `StateChanged`
/// notification asks for: agents change state many times a second.
const NOTIFY_MIN_GAP: Duration = Duration::from_secs(1);
const SCREEN_MAX_BYTES: usize = 8192;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Link {
    Connecting,
    Live,
    Down(String),
}

#[derive(Debug, Clone)]
pub struct Snapshot {
    pub data: RepoData,
    pub link: Link,
    pub can_focus: bool,
    pub can_send: bool,
    pub diff_mode: DiffMode,
    pub selected_diff: Option<(String, String, String)>,
    /// (worktree cwd, commit sha, `git show` text) of the commit being viewed.
    pub selected_commit: Option<(String, String, String)>,
}

#[derive(Debug, Clone)]
pub enum Command {
    Refresh,
    SetDiffMode(DiffMode),
    LoadDiff {
        cwd: String,
        path: String,
    },
    /// Show one commit of the worktree instead of a file.
    LoadCommit {
        cwd: String,
        sha: String,
    },
    Focus(u64),
    /// A new Noa tab whose shell starts in `cwd` (a worktree root).
    OpenShell {
        cwd: String,
    },
    /// A new Noa tab in `cwd` running the editor on `path` (relative to
    /// `cwd`) at `line`.
    OpenEditor {
        cwd: String,
        path: String,
        line: u32,
    },
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
        .name("repo-browser-live".into())
        .spawn(move || run(snapshot_tx, notice_tx, delivered_tx, command_rx, wake))
        .expect("spawn repo-browser-live thread");
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
    // Outlives reconnects: merge-tree reruns only when a commit moves.
    let mut merges = git::MergePredictions::default();
    let mut selected_commit: Option<(String, String)> = None;
    let mut commit_diff: Option<((String, String), String)> = None;
    let mut commit_cache = git::CommitCache::default();

    let _ = snapshot_tx.send(Snapshot {
        data: RepoData::default(),
        link: Link::Connecting,
        can_focus: false,
        can_send: false,
        diff_mode: mode,
        selected_diff: None,
        selected_commit: None,
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
                let mut notified = false;
                let mut last: Option<LastBuild> = None;
                let deliver = |snapshot: Snapshot| {
                    let sent = snapshot_tx.send(snapshot).is_ok();
                    wake();
                    sent
                };

                loop {
                    while let Ok(cmd) = command_rx.try_recv() {
                        match cmd {
                            Command::Refresh => dirty = true,
                            Command::SetDiffMode(m) => {
                                mode = m;
                                dirty = true;
                            }
                            // Answered at once from the last refresh; the
                            // worktrees are rescanned on the usual schedule.
                            Command::LoadDiff { cwd, path } => {
                                selected_commit = None;
                                match last.as_mut().filter(|l| l.mode == mode) {
                                    Some(l) => {
                                        let diff = git::file_diff(&cwd, &path, mode);
                                        l.selected_diff = Some((cwd.clone(), path.clone(), diff));
                                        if !deliver(l.snapshot(can_focus, can_send, None)) {
                                            return;
                                        }
                                    }
                                    None => dirty = true,
                                }
                                selected_file = Some((cwd, path));
                            }
                            Command::LoadCommit { cwd, sha } => {
                                selected_commit = Some((cwd, sha));
                                match &last {
                                    Some(l) => {
                                        let commit = commit_entry(
                                            &mut commit_diff,
                                            selected_commit.as_ref(),
                                        );
                                        if !deliver(l.snapshot(can_focus, can_send, commit)) {
                                            return;
                                        }
                                    }
                                    None => dirty = true,
                                }
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
                            Command::OpenShell { cwd } => {
                                let params = shell_spawn(&cwd);
                                let notice = match client.spawn_tab(None, &params) {
                                    Ok(pane_id) => {
                                        format!(
                                            "新しいタブでシェルを開きました (ペイン #{pane_id})"
                                        )
                                    }
                                    Err(err) => format!("シェルを開けませんでした: {err}"),
                                };
                                let _ = notice_tx.send(notice);
                                wake();
                            }
                            Command::OpenEditor { cwd, path, line } => {
                                let params = editor_spawn(
                                    &cwd,
                                    &path,
                                    line,
                                    std::env::var("SHELL").ok().as_deref(),
                                    editor_from_env().as_deref(),
                                );
                                let notice = match client.spawn_tab(None, &params) {
                                    Ok(pane_id) => format!(
                                        "新しいタブで {path}:{line} を開きました (ペイン #{pane_id})"
                                    ),
                                    Err(err) => format!("エディタを開けませんでした: {err}"),
                                };
                                let _ = notice_tx.send(notice);
                                wake();
                            }
                            Command::SendText {
                                pane_id,
                                text,
                                submit,
                            } => {
                                let result =
                                    send_steps(&text, submit).into_iter().try_for_each(|step| {
                                        match step {
                                            SendStep::Type { text: chunk, paste } => client
                                                .send_text_with(
                                                    pane_id,
                                                    &chunk,
                                                    SendTextOptions {
                                                        paste,
                                                        require_paste_mode: paste
                                                            && needs_paste_mode(&chunk),
                                                        // A review prompt is for an agent:
                                                        // Noa checks the real foreground
                                                        // process now, since the snapshot's
                                                        // can be seconds old.
                                                        refuse_shell: submit,
                                                    },
                                                ),
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
                                    let what = if submit { "指示" } else { "パス" };
                                    let _ = notice_tx
                                        .send(format!("ペイン #{pane_id} へ{what}を送信しました"));
                                }
                                wake();
                            }
                        }
                    }

                    if rebuild_due(last_refresh.elapsed(), dirty, notified) {
                        let built = build_snapshot(
                            &mut client,
                            mode,
                            selected_file.as_ref(),
                            &mut merges,
                            &mut commit_cache,
                        );
                        match built {
                            Ok((data, selected_diff)) => {
                                let selected_commit =
                                    commit_entry(&mut commit_diff, selected_commit.as_ref());
                                let build = LastBuild {
                                    data,
                                    mode,
                                    selected_diff,
                                };
                                if !deliver(build.snapshot(can_focus, can_send, selected_commit)) {
                                    return;
                                }
                                last = Some(build);
                                last_refresh = Instant::now();
                                dirty = false;
                                notified = false;
                            }
                            Err(err) => {
                                let _ = snapshot_tx.send(Snapshot {
                                    data: RepoData::default(),
                                    link: Link::Down(format!("接続が切れました: {err}")),
                                    can_focus: false,
                                    can_send: false,
                                    diff_mode: mode,
                                    selected_diff: None,
                                    selected_commit: None,
                                });
                                wake();
                                break;
                            }
                        }
                    }

                    match client.next_notification(POLL_WAIT) {
                        Ok(Some(_)) => notified = true,
                        Ok(None) => {}
                        Err(err) => {
                            let _ = snapshot_tx.send(Snapshot {
                                data: RepoData::default(),
                                link: Link::Down(format!("切断: {err}")),
                                can_focus: false,
                                can_send: false,
                                diff_mode: mode,
                                selected_diff: None,
                                selected_commit: None,
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
                        data: RepoData::default(),
                        link: Link::Down(why),
                        can_focus: false,
                        can_send: false,
                        diff_mode: mode,
                        selected_diff: None,
                        selected_commit: None,
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
                        Ok(Command::LoadDiff { cwd, path }) => {
                            selected_file = Some((cwd, path));
                            selected_commit = None;
                        }
                        Ok(Command::LoadCommit { cwd, sha }) => selected_commit = Some((cwd, sha)),
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

/// Whether the refresh loop rebuilds now, `since_last` after its last
/// rebuild: at once when the UI asked, every [`REFRESH_EVERY`], and for a
/// state-change notification no sooner than [`NOTIFY_MIN_GAP`].
fn rebuild_due(since_last: Duration, dirty: bool, notified: bool) -> bool {
    dirty || since_last >= REFRESH_EVERY || (notified && since_last >= NOTIFY_MIN_GAP)
}

/// What the last full refresh built, which a `LoadDiff`/`LoadCommit` is
/// answered from without rescanning every worktree.
struct LastBuild {
    data: RepoData,
    /// The mode `data` was built in.
    mode: DiffMode,
    selected_diff: Option<DiffEntry>,
}

impl LastBuild {
    fn snapshot(
        &self,
        can_focus: bool,
        can_send: bool,
        selected_commit: Option<DiffEntry>,
    ) -> Snapshot {
        Snapshot {
            data: self.data.clone(),
            link: Link::Live,
            can_focus,
            can_send,
            diff_mode: self.mode,
            selected_diff: self.selected_diff.clone(),
            selected_commit,
        }
    }
}

/// `$VISUAL`, else `$EDITOR`, when set and non-blank in this process.
fn editor_from_env() -> Option<String> {
    ["VISUAL", "EDITOR"]
        .into_iter()
        .filter_map(|name| std::env::var(name).ok())
        .find(|value| !value.trim().is_empty())
}

/// A new tab (focused: the server's default) with the shell in `cwd`.
fn shell_spawn(cwd: &str) -> SpawnParams {
    SpawnParams {
        config: Some(SpawnConfig {
            cwd: Some(cwd.to_owned()),
            ..SpawnConfig::default()
        }),
        focus: Some(true),
        ..SpawnParams::default()
    }
}

/// A new tab (focused) in `cwd` running the editor on `path` at `line`.
fn editor_spawn(
    cwd: &str,
    path: &str,
    line: u32,
    shell: Option<&str>,
    editor: Option<&str>,
) -> SpawnParams {
    SpawnParams {
        config: Some(SpawnConfig {
            command: Some(editor_argv(shell, path, line, editor)),
            cwd: Some(cwd.to_owned()),
            ..SpawnConfig::default()
        }),
        focus: Some(true),
        ..SpawnParams::default()
    }
}

/// The argv that opens `path` at `line` in the user's editor.
///
/// Noa.app starts this widget, so its environment may lack the editor the
/// user set in their shell rc files (and a `PATH` that finds it). The editor
/// is therefore resolved by the user's login shell: `editor` (from this
/// process's `$VISUAL`/`$EDITOR`) is used when present, otherwise the script
/// reads `${VISUAL:-${EDITOR:-vi}}` itself. The script is plain `sh`/`bash`/
/// `zsh` syntax (not fish). VS Code style editors get `--goto ./path:LINE`,
/// everything else `+LINE ./path`.
///
/// `path` is relative to the worktree root and passed as `./<path>`, so a
/// file named `+!cmd` or `-S` is never read as an editor command or option.
fn editor_argv(shell: Option<&str>, path: &str, line: u32, editor: Option<&str>) -> Vec<String> {
    let shell = shell.filter(|s| !s.trim().is_empty()).unwrap_or("/bin/zsh");
    let ed = match editor {
        Some(editor) => crate::ui::shell_escape(editor),
        None => "${VISUAL:-${EDITOR:-vi}}".to_owned(),
    };
    // `f` holds the path as one quoted literal; `eval` only re-splits `$ed`,
    // which may carry arguments ("code --wait").
    let script = format!(
        "ed={ed}; f={f}; w=${{ed%% *}}; b=${{w##*/}}; \
         case $b in \
         code|code-insiders|codium|cursor|windsurf) eval \"exec $ed --goto \\\"\\$f:{line}\\\"\";; \
         *) eval \"exec $ed +{line} \\\"\\$f\\\"\";; \
         esac",
        f = crate::ui::shell_escape(&dot_relative(path)),
    );
    vec![shell.to_owned(), "-lc".to_owned(), script]
}

/// `path` (relative to the worktree root) as `./<path>`; an absolute path,
/// which cannot be read as an option either, is kept as it is.
fn dot_relative(path: &str) -> String {
    if path.starts_with('/') {
        path.to_owned()
    } else {
        format!("./{path}")
    }
}

/// Whether pasting `text` into a pane without bracketed paste would act on
/// it rather than insert it, so the send must be refused there.
fn needs_paste_mode(text: &str) -> bool {
    // Without bracketed paste every control character reaches the line
    // editor as a key: CR/LF run a line, Ctrl-U erases it, ESC starts a
    // sequence, Tab completes.
    text.chars().any(char::is_control)
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

/// Worktrees by canonical root, with the panes whose cwd is inside each.
type ByWorktree = HashMap<String, (git::WorktreeGitInfo, Vec<PaneSummary>)>;
/// The `git show` entry for the viewed commit. A commit never changes, so its
/// text is read once and kept until another commit is asked for.
fn commit_entry(
    memo: &mut Option<((String, String), String)>,
    wanted: Option<&(String, String)>,
) -> Option<DiffEntry> {
    let wanted = wanted?;
    if memo.as_ref().is_none_or(|(key, _)| key != wanted) {
        *memo = Some((wanted.clone(), git::commit_diff(&wanted.0, &wanted.1)));
    }
    let (_, diff) = memo.as_ref()?;
    Some((wanted.0.clone(), wanted.1.clone(), diff.clone()))
}

fn build_snapshot(
    client: &mut Client,
    mode: DiffMode,
    selected: Option<&(String, String)>,
    merges: &mut git::MergePredictions,
    commit_cache: &mut git::CommitCache,
) -> Result<(RepoData, Option<DiffEntry>), ClientError> {
    let panels = client.list_panels()?;
    let mut bases = git::BaseRefs::default();
    let mut by_worktree = place_panes(&panels, mode, &mut bases, |panel| {
        pane_summary(panel, fetch_pane_tail(client, panel))
    });
    add_paneless_worktrees(&mut by_worktree, mode, &mut bases);
    let data = repo_data(by_worktree, merges, commit_cache, &mut bases);

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

/// The worktree of each panel's cwd, with the panes in it as `summary` makes
/// them; a panel whose cwd is no worktree is left out. A cwd that resolves
/// to the same directory as one already inspected successfully this time
/// joins that worktree instead of being inspected again; one that failed,
/// or cannot be resolved, is inspected again, as main does for every cwd.
fn place_panes(
    panels: &[Panel],
    mode: DiffMode,
    bases: &mut git::BaseRefs,
    mut summary: impl FnMut(&Panel) -> PaneSummary,
) -> ByWorktree {
    let mut by_worktree: ByWorktree = HashMap::new();
    // Resolved cwd -> the worktree its successful inspection found. Keyed by
    // the resolved path, so a symlink retargeted between two panels is
    // inspected again and lands where it points now.
    let mut inspected: HashMap<PathBuf, String> = HashMap::new();
    for panel in panels {
        let cwd = panel.cwd.trim();
        if cwd.is_empty() {
            continue;
        }
        let (key, info) = if by_worktree.contains_key(cwd) {
            (cwd.to_owned(), None)
        } else {
            let resolved = std::fs::canonicalize(cwd).ok();
            if let Some(root) = resolved.as_ref().and_then(|dir| inspected.get(dir)) {
                // Inspecting it again would find this worktree, which keeps
                // the info of its first inspection.
                (root.clone(), None)
            } else {
                let Some(info) = git::inspect_worktree(cwd, mode, bases) else {
                    continue;
                };
                if let Some(dir) = resolved {
                    inspected.insert(dir, info.worktree_root.clone());
                }
                (info.worktree_root.clone(), Some(info))
            }
        };
        let summary = summary(panel);
        match info {
            Some(info) => by_worktree
                .entry(key)
                .or_insert_with(|| (info, Vec::new()))
                .1
                .push(summary),
            None => {
                if let Some((_, panes)) = by_worktree.get_mut(&key) {
                    panes.push(summary);
                }
            }
        }
    }
    by_worktree
}

fn pane_summary(panel: &Panel, terminal_tail: String) -> PaneSummary {
    PaneSummary {
        pane_id: panel.pane_id.0,
        name: panel.name.clone(),
        label: panel.label.clone(),
        process: panel.process.clone(),
        activity: panel_activity(panel),
        busy: panel.busy,
        attention: panel.attention,
        terminal_tail,
    }
}

/// Adds every other worktree of each repository a pane is in, from
/// `git worktree list`, with no panes: a worker's worktree and branch stay
/// to be reviewed and merged after its pane closed.
fn add_paneless_worktrees(by_worktree: &mut ByWorktree, mode: DiffMode, bases: &mut git::BaseRefs) {
    let mut repos = HashSet::new();
    let anchors: Vec<String> = by_worktree
        .values()
        .filter(|(info, _)| repos.insert(info.repo_root.clone()))
        .map(|(info, _)| info.worktree_root.clone())
        .collect();
    for anchor in anchors {
        for path in git::list_worktrees(&anchor) {
            if by_worktree.contains_key(&path) {
                continue;
            }
            if let Some(info) = git::inspect_worktree(&path, mode, bases) {
                by_worktree
                    .entry(info.worktree_root.clone())
                    .or_insert_with(|| (info, Vec::new()));
            }
        }
    }
}

/// Groups the worktrees into repositories, each with its predicted merge
/// conflicts against the base.
fn repo_data(
    by_worktree: ByWorktree,
    merges: &mut git::MergePredictions,
    commit_cache: &mut git::CommitCache,
    bases: &mut git::BaseRefs,
) -> RepoData {
    merges.retain(|root| by_worktree.contains_key(root));
    let entries = by_worktree
        .into_values()
        .map(|(info, panes)| {
            let base = bases.get(&info.repo_root, Path::new(&info.worktree_root));
            let merge_conflicts = merges
                .predict(&info.worktree_root, &info.branch, base.as_ref())
                .unwrap_or_default();
            let commits = commit_cache.commits(&info.worktree_root, base.as_ref());
            let wt = Worktree {
                name: info.branch.clone(),
                cwd: info.worktree_root,
                branch: info.branch,
                ahead: info.ahead,
                behind: info.behind,
                panes,
                files: info.files,
                note: info.note,
                merge_conflicts,
                commits,
            };
            (info.repo_name, info.repo_root, wt)
        })
        .collect();
    group_repos(entries)
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

    use crate::git::tests::{TempRepo, add_worktree, commit_file, temp_repo};

    fn pane(pane_id: u64) -> PaneSummary {
        PaneSummary {
            pane_id,
            name: "zsh".into(),
            label: None,
            process: None,
            activity: AgentActivity::None,
            busy: false,
            attention: false,
            terminal_tail: String::new(),
        }
    }

    #[test]
    fn worktree_without_a_pane_is_listed_with_its_conflicts() {
        let repo = temp_repo("live-paneless", "main");
        let _cleanup = TempRepo(format!("{}-wt", repo.0.display()).into());
        let foo = add_worktree(&repo, "foo", "orch/foo");
        commit_file(&foo, "old.rs", "fn theirs() {}\n");
        commit_file(&repo.0, "old.rs", "fn ours() {}\n");
        // Both worktrees also have the same file edited, uncommitted.
        std::fs::write(repo.0.join("old.rs"), "main edit\n").unwrap();
        std::fs::write(foo.join("old.rs"), "foo edit\n").unwrap();

        let mut bases = git::BaseRefs::default();
        let main = git::inspect_worktree(repo.0.to_str().unwrap(), DiffMode::Unstaged, &mut bases)
            .unwrap();
        let mut by_worktree: ByWorktree = HashMap::new();
        by_worktree.insert(main.worktree_root.clone(), (main, vec![pane(7)]));
        add_paneless_worktrees(&mut by_worktree, DiffMode::Unstaged, &mut bases);
        let data = repo_data(
            by_worktree,
            &mut git::MergePredictions::default(),
            &mut git::CommitCache::default(),
            &mut bases,
        );

        assert_eq!(data.repos.len(), 1);
        let wts = &data.repos[0].worktrees;
        let branches: Vec<&str> = wts.iter().map(|w| w.branch.as_str()).collect();
        assert_eq!(branches, vec!["main", "orch/foo"]);
        let (main, foo) = (&wts[0], &wts[1]);
        assert_eq!(main.panes.len(), 1);
        assert!(!foo.has_pane());
        assert_eq!(foo.merge_conflicts, vec!["old.rs"]);
        assert!(main.merge_conflicts.is_empty());
        assert_eq!(foo.files[0].conflict_worktrees, vec!["main"]);
        assert_eq!(main.files[0].conflict_worktrees, vec!["orch/foo"]);
    }

    /// One refresh of `repo` (two worktrees with commits) in `mode`, the way
    /// `build_snapshot` does it, returning how often a base was resolved.
    fn base_resolutions_in_one_refresh(repo: &TempRepo, mode: DiffMode) -> usize {
        let mut bases = git::BaseRefs::default();
        let main = git::inspect_worktree(repo.0.to_str().unwrap(), mode, &mut bases).unwrap();
        let mut by_worktree: ByWorktree = HashMap::new();
        by_worktree.insert(main.worktree_root.clone(), (main, vec![pane(7)]));
        add_paneless_worktrees(&mut by_worktree, mode, &mut bases);
        let data = repo_data(
            by_worktree,
            &mut git::MergePredictions::default(),
            &mut git::CommitCache::default(),
            &mut bases,
        );
        let foo = data.repos[0]
            .worktrees
            .iter()
            .find(|w| w.branch == "orch/foo")
            .unwrap();
        assert_eq!(foo.commits.len(), 1, "{mode:?}");
        assert_eq!(foo.merge_conflicts, vec!["old.rs"], "{mode:?}");
        bases.resolutions
    }

    #[test]
    fn a_refresh_resolves_the_base_once_per_repository() {
        let repo = temp_repo("live-base-once", "main");
        let _cleanup = TempRepo(format!("{}-wt", repo.0.display()).into());
        let foo = add_worktree(&repo, "foo", "orch/foo");
        commit_file(&foo, "old.rs", "fn theirs() {}\n");
        commit_file(&repo.0, "old.rs", "fn ours() {}\n");
        add_worktree(&repo, "bar", "orch/bar");
        for mode in DiffMode::ALL {
            assert_eq!(base_resolutions_in_one_refresh(&repo, mode), 1, "{mode:?}");
        }
    }

    /// Pins what one refresh reports for a repository holding every kind of
    /// worktree: ahead with commits and a predicted conflict, detached, on an
    /// unborn branch, and the base itself, from a pane in a subdirectory.
    #[test]
    fn a_refresh_reports_every_kind_of_worktree() {
        let repo = temp_repo("live-kinds", "main");
        let _cleanup = TempRepo(format!("{}-wt", repo.0.display()).into());
        let foo = add_worktree(&repo, "foo", "orch/foo");
        commit_file(&foo, "old.rs", "fn theirs() {}\n");
        commit_file(&foo, "two.rs", "2\n");
        commit_file(&repo.0, "old.rs", "fn ours() {}\n");
        std::fs::write(foo.join("old.rs"), "dirty\n").unwrap();
        let detached = format!("{}-wt/detached", repo.0.display());
        crate::git::tests::git(&repo.0, &["worktree", "add", "-q", "--detach", &detached]);
        let unborn = format!("{}-wt/unborn", repo.0.display());
        // An orphan worktree without `git worktree add --orphan` (Git 2.42+):
        // a detached one switched to a new unborn branch, emptied.
        crate::git::tests::git(&repo.0, &["worktree", "add", "-q", "--detach", &unborn]);
        crate::git::tests::git(
            Path::new(&unborn),
            &["checkout", "-q", "--orphan", "unborn"],
        );
        crate::git::tests::git(Path::new(&unborn), &["rm", "-rfq", "."]);
        std::fs::write(Path::new(&unborn).join("u.rs"), "u\n").unwrap();
        std::fs::create_dir_all(repo.0.join("sub")).unwrap();
        let sub = repo.0.join("sub");
        for mode in DiffMode::ALL {
            let mut merges = git::MergePredictions::default();
            let mut commits = git::CommitCache::default();
            for _ in 0..2 {
                let (data, _) =
                    offline_refresh(sub.to_str().unwrap(), mode, &mut merges, &mut commits);
                assert_eq!(data.repos.len(), 1, "{mode:?}");
                let got: Vec<_> = data.repos[0]
                    .worktrees
                    .iter()
                    .map(|w| {
                        (
                            w.branch.clone(),
                            w.has_pane(),
                            w.ahead,
                            w.behind,
                            w.files
                                .iter()
                                .map(|f| {
                                    format!("{} {} +{}-{}", f.status, f.path, f.added, f.deleted)
                                })
                                .collect::<Vec<_>>(),
                            w.merge_conflicts.clone(),
                            w.commits
                                .iter()
                                .map(|c| c.subject.clone())
                                .collect::<Vec<_>>(),
                        )
                    })
                    .collect();
                let s = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
                let (foo_files, unborn_files) = match mode {
                    DiffMode::Unstaged => (s(&["M old.rs +1-1"]), s(&["? u.rs +0-0"])),
                    DiffMode::Staged => (s(&[]), s(&[])),
                    DiffMode::VsMain => (s(&["M old.rs +1-1", "A two.rs +1-0"]), s(&[])),
                };
                assert_eq!(
                    got,
                    vec![
                        ("(detached)".into(), false, 0, 0, s(&[]), s(&[]), s(&[])),
                        ("main".into(), true, 0, 0, s(&[]), s(&[]), s(&[])),
                        (
                            "orch/foo".into(),
                            false,
                            0,
                            0,
                            foo_files,
                            s(&["old.rs"]),
                            s(&["two.rs", "old.rs"]),
                        ),
                        ("unborn".into(), false, 0, 0, unborn_files, s(&[]), s(&[])),
                    ],
                    "{mode:?}"
                );
            }
            // The second refresh is answered from the caches.
            assert_eq!(merges.runs(), 2, "{mode:?}");
        }
    }

    #[test]
    fn panes_are_placed_in_the_worktree_of_their_cwd() {
        let repo = temp_repo("live-panes", "main");
        let _cleanup = TempRepo(format!("{}-wt", repo.0.display()).into());
        let foo = add_worktree(&repo, "foo", "orch/foo");
        let plain = TempRepo(format!("{}-plain", repo.0.display()).into());
        std::fs::create_dir_all(plain.0.join("sub")).unwrap();
        std::fs::create_dir_all(repo.0.join("sub")).unwrap();
        // Resolved paths, so that whether the temporary directory is reached
        // through a symlink (`/var` on macOS) changes nothing: the root is
        // then the key of the worktree its subdirectory found.
        let real = |p: &Path| std::fs::canonicalize(p).unwrap();
        let root = real(&repo.0);
        let link = real(&plain.0).join("link");
        std::os::unix::fs::symlink(root.join("sub"), &link).unwrap();
        let s = |p: &Path| p.to_str().unwrap().to_owned();
        let (sub, foo, plain, link) = (
            s(&root.join("sub")),
            s(&real(&foo)),
            s(&real(&plain.0).join("sub")),
            s(&link),
        );
        let missing = format!("{}/missing", s(&root));
        let root = s(&root);
        let cwds = [
            &sub, &sub, &plain, &root, &foo, &plain, "  ", &missing, &sub, &foo, &link,
        ];
        let panels: Vec<Panel> = cwds
            .iter()
            .enumerate()
            .map(|(i, cwd)| panel(i as u64 + 1, cwd))
            .collect();
        for mode in DiffMode::ALL {
            let mut bases = git::BaseRefs::default();
            let mut summarized = Vec::new();
            let before = git::INSPECTIONS.with(std::cell::Cell::get);
            let by_worktree = place_panes(&panels, mode, &mut bases, |panel| {
                summarized.push(panel.pane_id.0);
                pane(panel.pane_id.0)
            });
            assert_eq!(summarized, vec![1, 2, 4, 5, 9, 10, 11], "{mode:?}");
            // sub, plain, foo, plain again (a failed inspection is retried)
            // and missing; the root is the worktree sub found, and a repeat
            // or a symlink to an inspected directory is not inspected again.
            let inspected = git::INSPECTIONS.with(std::cell::Cell::get) - before;
            assert_eq!(inspected, 5, "{mode:?}");
            assert_eq!(
                placed(&by_worktree),
                [("main", vec![1, 2, 4, 9, 11]), ("orch/foo", vec![5, 10])],
                "{mode:?}"
            );
        }
    }

    #[test]
    fn a_symlink_retargeted_between_panes_is_inspected_again() {
        let repo = temp_repo("live-relink", "main");
        let _cleanup = TempRepo(format!("{}-wt", repo.0.display()).into());
        let foo = add_worktree(&repo, "foo", "orch/foo");
        let links = TempRepo(format!("{}-links", repo.0.display()).into());
        std::fs::create_dir_all(&links.0).unwrap();
        let link = links.0.join("link");
        let panels = [
            panel(1, link.to_str().unwrap()),
            panel(2, link.to_str().unwrap()),
        ];
        for (second, want) in [
            (foo.clone(), vec![("main", vec![1]), ("orch/foo", vec![2])]),
            (repo.0.join("gone"), vec![("main", vec![1])]),
        ] {
            let _ = std::fs::remove_file(&link);
            std::os::unix::fs::symlink(&repo.0, &link).unwrap();
            // Retargeted while the first pane is summarized, as its terminal
            // tail is fetched in a refresh.
            let by_worktree = place_panes(
                &panels,
                DiffMode::Unstaged,
                &mut git::BaseRefs::default(),
                |panel| {
                    if panel.pane_id.0 == 1 {
                        std::fs::remove_file(&link).unwrap();
                        std::os::unix::fs::symlink(&second, &link).unwrap();
                    }
                    pane(panel.pane_id.0)
                },
            );
            assert_eq!(placed(&by_worktree), want, "{second:?}");
        }
    }

    /// Each worktree's branch with the panes placed in it, sorted.
    fn placed(by_worktree: &ByWorktree) -> Vec<(&str, Vec<u64>)> {
        let mut got: Vec<(&str, Vec<u64>)> = by_worktree
            .values()
            .map(|(info, panes)| {
                (
                    info.branch.as_str(),
                    panes.iter().map(|p| p.pane_id).collect(),
                )
            })
            .collect();
        got.sort();
        got
    }

    /// The git half of `build_snapshot` for one pane in `cwd`, without a Noa
    /// server: every worktree of its repository, then the first file's diff.
    fn offline_refresh(
        cwd: &str,
        mode: DiffMode,
        merges: &mut git::MergePredictions,
        commit_cache: &mut git::CommitCache,
    ) -> (RepoData, Option<DiffEntry>) {
        offline_refresh_of(&[panel(1, cwd)], mode, merges, commit_cache)
    }

    /// A panel of pane `id` in `cwd`, as `noa.listPanels` sends it.
    fn panel(id: u64, cwd: &str) -> Panel {
        serde_json::from_value(serde_json::json!({
            "windowGroupId": "1",
            "windowId": "1",
            "paneId": id.to_string(),
            "name": "zsh",
            "cwd": cwd,
            "busy": false,
            "attention": false,
            "preview": [],
        }))
        .unwrap()
    }

    /// [`offline_refresh`] with `panels` for panes.
    fn offline_refresh_of(
        panels: &[Panel],
        mode: DiffMode,
        merges: &mut git::MergePredictions,
        commit_cache: &mut git::CommitCache,
    ) -> (RepoData, Option<DiffEntry>) {
        let mut bases = git::BaseRefs::default();
        let mut by_worktree = place_panes(panels, mode, &mut bases, |panel| pane(panel.pane_id.0));
        add_paneless_worktrees(&mut by_worktree, mode, &mut bases);
        let data = repo_data(by_worktree, merges, commit_cache, &mut bases);
        let diff = data
            .repos
            .first()
            .and_then(|r| r.worktrees.iter().find(|w| !w.files.is_empty()))
            .and_then(|w| {
                let f = w.files.first()?;
                Some((
                    w.cwd.clone(),
                    f.path.clone(),
                    git::file_diff(&w.cwd, &f.path, mode),
                ))
            });
        (data, diff)
    }

    /// Times refreshes of `REPO_BROWSER_BENCH_DIR` (default: this crate's
    /// repository) in every mode, `REPO_BROWSER_BENCH_RUNS` times each after
    /// one warm-up, with `REPO_BROWSER_BENCH_PANES` panes (default 1) there.
    /// Run with a logging `git` first on `PATH` to count spawns.
    #[test]
    #[ignore = "benchmark: cargo test -- --ignored --nocapture bench_refresh"]
    fn bench_refresh() {
        let dir = std::env::var("REPO_BROWSER_BENCH_DIR")
            .unwrap_or_else(|_| env!("CARGO_MANIFEST_DIR").to_owned());
        // At least one run, so there is a median to report.
        let runs: usize = std::env::var("REPO_BROWSER_BENCH_RUNS")
            .ok()
            .and_then(|n| n.parse().ok())
            .unwrap_or(10)
            .max(1);
        let panes: u64 = std::env::var("REPO_BROWSER_BENCH_PANES")
            .ok()
            .and_then(|n| n.parse().ok())
            .unwrap_or(1);
        let panels: Vec<Panel> = (1..=panes).map(|id| panel(id, &dir)).collect();
        for mode in DiffMode::ALL {
            let mut merges = git::MergePredictions::default();
            let mut commits = git::CommitCache::default();
            let mut times = Vec::new();
            let (mut allocs, mut peaks) = (Vec::new(), Vec::new());
            let mut worktrees = 0;
            for i in 0..=runs {
                let t = Instant::now();
                let ((data, _), n, peak) = crate::heap::measure(|| {
                    offline_refresh_of(&panels, mode, &mut merges, &mut commits)
                });
                if i > 0 {
                    times.push(t.elapsed().as_secs_f64() * 1000.0);
                    allocs.push(n);
                    peaks.push(peak);
                }
                worktrees = data.repos.iter().map(|r| r.worktrees.len()).sum();
            }
            times.sort_by(f64::total_cmp);
            allocs.sort();
            peaks.sort();
            println!(
                "bench_refresh {mode:?}: {worktrees} worktrees, median {:.1} ms, min {:.1}, max {:.1} ({runs} runs)",
                times[times.len() / 2],
                times[0],
                times[times.len() - 1]
            );
            println!(
                "bench_heap {mode:?}: allocations median {} (min {}, max {}), peak heap median {} B (min {}, max {})",
                allocs[allocs.len() / 2],
                allocs[0],
                allocs[allocs.len() - 1],
                peaks[peaks.len() / 2],
                peaks[0],
                peaks[peaks.len() - 1]
            );
        }
    }

    #[test]
    fn notifications_rebuild_at_most_once_a_second() {
        let ms = Duration::from_millis;
        // A notification right after a rebuild waits for the gap.
        assert!(!rebuild_due(ms(0), false, true));
        assert!(!rebuild_due(ms(999), false, true));
        assert!(rebuild_due(ms(1000), false, true));
        // UI commands are not throttled, nor is the 3 s timer.
        assert!(rebuild_due(ms(0), true, false));
        assert!(rebuild_due(ms(10), true, true));
        assert!(!rebuild_due(ms(2999), false, false));
        assert!(rebuild_due(REFRESH_EVERY, false, false));
    }

    #[test]
    fn a_load_is_answered_from_the_last_build() {
        let last = LastBuild {
            data: RepoData {
                repos: vec![crate::model::Repo {
                    name: "repo".into(),
                    root: "/repo".into(),
                    worktrees: Vec::new(),
                }],
            },
            mode: DiffMode::Staged,
            selected_diff: Some(("/repo".into(), "a.rs".into(), "+new".into())),
        };
        let commit = Some(("/repo".into(), "abc".into(), "commit abc".into()));
        let snap = last.snapshot(true, false, commit.clone());
        assert_eq!(snap.data, last.data);
        assert_eq!(snap.diff_mode, DiffMode::Staged);
        assert_eq!(snap.link, Link::Live);
        assert!(snap.can_focus && !snap.can_send);
        assert_eq!(snap.selected_diff, last.selected_diff);
        assert_eq!(snap.selected_commit, commit);
    }

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

    #[test]
    fn any_control_character_requires_bracketed_paste() {
        // Ctrl-U then CR: an unbracketed paste would erase the line and run `id`.
        assert!(needs_paste_mode("/tmp/a\x15id\r"));
        assert!(needs_paste_mode("line\rnext"));
        assert!(needs_paste_mode("a\x1b[A"));
        assert!(needs_paste_mode("a\x7f"));
        assert!(!needs_paste_mode("'/Users/x/Screen Shot 1.png' 日本語.txt"));
    }

    /// Runs the argv `editor_argv` builds against a stub editor that records
    /// its arguments one per line, and returns them.
    fn run_editor(
        tag: &str,
        stub_name: &str,
        editor_suffix: Option<&str>,
        env: &[(&str, &str)],
        path: &str,
        line: u32,
    ) -> Vec<String> {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("rb-editor-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let out = dir.join("args.txt");
        let stub = dir.join(stub_name);
        std::fs::write(
            &stub,
            format!(
                "#!/bin/sh\nfor a in \"$@\"; do printf '%s\\n' \"$a\"; done > {}\n",
                out.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();
        let editor = editor_suffix.map(|s| format!("{}{s}", stub.display()));
        let argv = editor_argv(Some("/bin/sh"), path, line, editor.as_deref());
        let mut cmd = std::process::Command::new(&argv[0]);
        cmd.args(&argv[1..]).current_dir(&dir);
        cmd.env_remove("VISUAL").env_remove("EDITOR");
        for (k, v) in env {
            cmd.env(k, v.replace("STUB", &stub.display().to_string()));
        }
        let status = cmd.status().unwrap();
        assert!(status.success(), "{argv:?}");
        let args = std::fs::read_to_string(&out).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        args.lines().map(str::to_owned).collect()
    }

    #[test]
    fn vi_style_editors_get_plus_line_before_the_path() {
        for name in ["vi", "nvim", "nano", "emacs", "hx", "micro"] {
            let args = run_editor(name, name, Some(""), &[], "src/a.rs", 42);
            assert_eq!(args, vec!["+42", "./src/a.rs"], "{name}");
        }
    }

    #[test]
    fn vs_code_style_editors_get_goto() {
        for name in ["code", "cursor", "codium"] {
            let args = run_editor(name, name, Some(""), &[], "src/a.rs", 42);
            assert_eq!(args, vec!["--goto", "./src/a.rs:42"], "{name}");
        }
        // Arguments in $EDITOR stay in front.
        let args = run_editor("wait", "code", Some(" --wait"), &[], "a.rs", 3);
        assert_eq!(args, vec!["--wait", "--goto", "./a.rs:3"]);
    }

    #[test]
    fn paths_with_spaces_and_quotes_arrive_as_one_argument() {
        for path in [
            "dir with space/it's \"q\".rs",
            "$(touch pwned).rs",
            "`touch pwned`.rs",
            "a;b&c|d.rs",
            // An editor reads `+cmd` as a command and `-S` as an option.
            "+!touch x",
            "-S",
        ] {
            let args = run_editor("nasty", "vim", Some(""), &[], path, 7);
            assert_eq!(args, vec!["+7".to_owned(), format!("./{path}")], "{path}");
            let args = run_editor("nasty-code", "code", Some(""), &[], path, 7);
            assert_eq!(
                args,
                vec!["--goto".to_owned(), format!("./{path}:7")],
                "{path}"
            );
        }
    }

    #[test]
    fn without_a_process_editor_the_login_shell_reads_visual_then_editor() {
        let args = run_editor("v", "nvim", None, &[("VISUAL", "STUB")], "a.rs", 5);
        assert_eq!(args, vec!["+5", "./a.rs"]);
        let args = run_editor("e", "code", None, &[("EDITOR", "STUB")], "a.rs", 5);
        assert_eq!(args, vec!["--goto", "./a.rs:5"]);
        // $VISUAL wins over $EDITOR.
        let args = run_editor(
            "both",
            "hx",
            None,
            &[("VISUAL", "STUB"), ("EDITOR", "/nonexistent/editor")],
            "a.rs",
            5,
        );
        assert_eq!(args, vec!["+5", "./a.rs"]);
    }

    #[test]
    fn spawn_params_carry_the_worktree_cwd_and_focus() {
        let shell = shell_spawn("/repo wt");
        let config = shell.config.as_ref().unwrap();
        assert_eq!(config.cwd.as_deref(), Some("/repo wt"));
        assert_eq!(shell.focus, Some(true));
        // A plain shell needs `control` only; the editor command adds `input`.
        assert!(!config.needs_input_scope());

        let editor = editor_spawn("/repo wt", "a.rs", 9, Some("/bin/bash"), None);
        let config = editor.config.as_ref().unwrap();
        assert_eq!(config.cwd.as_deref(), Some("/repo wt"));
        let argv = config.command.as_ref().unwrap();
        assert_eq!((argv[0].as_str(), argv[1].as_str()), ("/bin/bash", "-lc"));
        assert!(config.needs_input_scope());
        assert_eq!(editor.focus, Some(true));
        assert_eq!(editor_argv(None, "a", 1, None)[0], "/bin/zsh");
    }
}
