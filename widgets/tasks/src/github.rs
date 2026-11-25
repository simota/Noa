//! The GitHub source (L1-3): the issues of the repository's `origin`, read
//! with `gh` through the login shell. **Read-only**: every `gh` call is
//! `repo view`, `issue list` or `issue view` (Decision 3). The last listing
//! is cached, so the window opens with it at once and offline.

use std::collections::BTreeSet;
use std::fs::File;
use std::io::{self, Read};
use std::os::fd::{AsRawFd, OwnedFd};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::fsx;
use crate::model::types::{
    Comment, GhFailure, GitHubView, IssueDetail, Millis, State, Task, TaskRef,
};
use crate::shell::login_shell;

#[cfg(test)]
mod tests;

/// Open issues listed (`--state open --limit`).
pub const OPEN_LIMIT: u32 = 200;
/// Recently closed issues listed, so a closed one reaches 完了.
pub const CLOSED_LIMIT: u32 = 30;
/// How often the listing is fetched again on its own.
pub const REFRESH_MS: Millis = 300_000;
/// A `gh` call still running after this is killed.
pub const GH_TIMEOUT: Duration = Duration::from_secs(20);

/// The GitHub source as the screen sees it.
pub trait IssueFeed: Send {
    /// What is known now; never blocks.
    fn view(&self) -> GitHubView;
    /// `r`; the feed also refreshes on start and every REFRESH_MS.
    fn refresh(&mut self);
    /// Fetches issue `number`'s body and comments (once).
    fn load_detail(&mut self, number: u64);
}

/// Runs `gh <args>` in `cwd`; the real one goes through shell::login_shell().
pub trait GhRun: Send + 'static {
    fn run(&mut self, cwd: &Path, args: &[String]) -> Result<String, GhFailure>;
}

/// Printed by the wrapper before `gh` runs: whatever an interactive rc file
/// prints on stdout comes before it and is dropped.
const MARKER: &str = "noa-tasks-gh-output";
/// How long the output is waited for after `gh` exited or was killed.
const DRAIN: Duration = Duration::from_millis(500);
const POLL: Duration = Duration::from_millis(20);
/// How often the worker looks at the clock when no command arrives.
const TICK: Duration = Duration::from_millis(200);

/// [`GhRun`] through the person's login shell (`$SHELL -l -i -c`), the
/// arguments passed as words, never as shell code.
pub struct LoginShellGh;

impl GhRun for LoginShellGh {
    fn run(&mut self, cwd: &Path, args: &[String]) -> Result<String, GhFailure> {
        run_shell(&login_shell(), cwd, args, GH_TIMEOUT, &[])
    }
}

/// The code a POSIX shell runs: fixed, holding no argument. The arguments
/// follow `-c <code> gh` as `"$@"` (`gh` is `$0`), so each stays one word
/// whatever it contains (AC-27).
fn posix_code() -> String {
    format!("printf '\\n{MARKER}\\n'; exec gh \"$@\"")
}

/// The same for fish, whose `-c` hands every word after the command to
/// `$argv` (there is no `$0`), each list element expanding to one word.
fn fish_code() -> String {
    format!("printf '\\n{MARKER}\\n'; exec gh $argv")
}

/// The program `shell` stands for and the words before `gh`'s arguments.
fn invocation(shell: &str) -> (String, Vec<String>) {
    match shell.rsplit('/').next().unwrap_or(shell) {
        "sh" | "bash" | "zsh" | "dash" | "ksh" | "mksh" | "yash" => (
            shell.to_owned(),
            vec![
                "-l".into(),
                "-i".into(),
                "-c".into(),
                posix_code(),
                "gh".into(),
            ],
        ),
        // `--` ends fish's own options, so the arguments are all `$argv`.
        "fish" => (
            shell.to_owned(),
            vec![
                "-l".into(),
                "-i".into(),
                "-c".into(),
                fish_code(),
                "--".into(),
            ],
        ),
        // csh, tcsh, nu and the like take neither `-l -i -c` nor `"$@"`:
        // `/bin/sh -c`, not a login shell, so `gh` is looked up on the
        // inherited PATH only (from Noa.app it may then be NoGh).
        _ => (
            "/bin/sh".to_owned(),
            vec!["-c".into(), posix_code(), "gh".into()],
        ),
    }
}

/// SIGKILL to the shell's process group: the shell, `gh`, and whatever
/// either started (an rc subprocess, `git`, `ssh`, a background job).
fn kill_group(pid: u32) {
    // SAFETY: killpg only sends a signal; the group is the one the child
    // leads (setsid in run_shell), alive while any member is.
    unsafe {
        libc::killpg(pid as libc::pid_t, libc::SIGKILL);
    }
}

/// `gh args` in `cwd` through the login shell, killed after `timeout`.
fn run_shell(
    shell: &str,
    cwd: &Path,
    args: &[String],
    timeout: Duration,
    envs: &[(&str, &str)],
) -> Result<String, GhFailure> {
    let (program, words) = invocation(shell);
    let mut cmd = Command::new(&program);
    cmd.args(words)
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (k, v) in envs {
        cmd.env(k, v);
    }
    // Its own session, so its own process group to kill as one. Not just
    // process_group(0): an interactive shell in a background group of a
    // controlling terminal stops itself (SIGTTIN) waiting for the terminal;
    // a new session has no controlling terminal.
    // SAFETY: setsid is async-signal-safe and touches no parent memory.
    unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = cmd
        .spawn()
        .map_err(|e| classify(false, None, &format!("{program}: {e}")))?;
    let pid = child.id();
    // Read without blocking as the bytes come, so a chatty rc file cannot
    // fill a pipe and stall the shell, what was printed survives a pipe an
    // rc background job holds open, and no thread is left blocked on one.
    let mut pipes: [Option<File>; 2] = [
        child.stdout.take().map(|p| File::from(OwnedFd::from(p))),
        child.stderr.take().map(|p| File::from(OwnedFd::from(p))),
    ];
    for pipe in pipes.iter().flatten() {
        let fd = pipe.as_raw_fd();
        // SAFETY: fcntl on a descriptor this function owns.
        unsafe {
            let flags = libc::fcntl(fd, libc::F_GETFL);
            libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK);
        }
    }
    let mut bufs = [Vec::new(), Vec::new()];
    let deadline = Instant::now() + timeout;
    let mut exited = None;
    let status = loop {
        for (pipe, buf) in pipes.iter_mut().zip(&mut bufs) {
            drain(pipe, buf);
        }
        if exited.is_none() {
            match child.try_wait() {
                // A background job of an rc file may hold a pipe open:
                // wait for the rest only a while.
                Ok(Some(status)) => exited = Some((status, Instant::now() + DRAIN)),
                Ok(None) if Instant::now() < deadline => {}
                Ok(None) => {
                    kill_group(pid);
                    let _ = child.wait();
                    return Err(GhFailure::Timeout);
                }
                Err(e) => {
                    kill_group(pid);
                    let _ = child.wait();
                    return Err(GhFailure::Failed(e.to_string()));
                }
            }
        }
        if let Some((status, until)) = exited
            && (pipes.iter().all(Option::is_none) || Instant::now() >= until)
        {
            break status;
        }
        std::thread::sleep(POLL);
    };
    // Whatever the shell left behind in its group goes with it.
    kill_group(pid);
    let [out, err] = bufs;
    let out = String::from_utf8_lossy(&out);
    if status.success() {
        // Only what follows the marker line is `gh`'s.
        let after = out
            .split_once(&format!("\n{MARKER}\n"))
            .or_else(|| out.split_once(&format!("\n{MARKER}\r\n")))
            .map(|(_, rest)| rest)
            .unwrap_or(&out);
        return Ok(after.to_owned());
    }
    Err(classify(
        true,
        status.code(),
        &String::from_utf8_lossy(&err),
    ))
}

/// Appends what `pipe` has now to `buf`; at its end (or an error) the pipe
/// is closed.
fn drain(pipe: &mut Option<File>, buf: &mut Vec<u8>) {
    let Some(file) = pipe else { return };
    let mut chunk = [0u8; 8192];
    loop {
        match file.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => return,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => break,
        }
    }
    *pipe = None;
}

/// What the feed keeps for the screen and the worker.
struct Shared {
    view: GitHubView,
    /// Detail loads asked and not yet answered.
    inflight: BTreeSet<u64>,
}

fn lock(shared: &Mutex<Shared>) -> MutexGuard<'_, Shared> {
    shared.lock().unwrap_or_else(|e| e.into_inner())
}

enum Cmd {
    Refresh,
    Detail(u64),
}

/// The cache file: the last listing and when it was fetched.
#[derive(Serialize, Deserialize)]
struct Cache {
    repo: Option<String>,
    tasks: Vec<Task>,
    fetched: Millis,
}

fn read_cache(path: &Path) -> Option<Cache> {
    let base = path.parent()?;
    let file = fsx::open_read(base, path).ok()?;
    let (bytes, over) = fsx::read_capped(file, 32 << 20).ok()?;
    if over {
        return None;
    }
    serde_json::from_slice(&bytes).ok()
}

/// Temp file + rename; a cache that cannot be written only costs the next
/// start its head start.
fn write_cache(path: &Path, cache: &Cache) {
    let Some(base) = path.parent() else { return };
    if std::fs::create_dir_all(base).is_err() {
        return;
    }
    if let Ok(bytes) = serde_json::to_vec(cache) {
        let _ = fsx::write_replace(base, path, &bytes);
    }
}

/// The real [`IssueFeed`]: `gh` off the UI thread, the cache on disk.
pub struct GhFeed {
    shared: Arc<Mutex<Shared>>,
    tx: Sender<Cmd>,
}

impl IssueFeed for GhFeed {
    fn view(&self) -> GitHubView {
        lock(&self.shared).view.clone()
    }

    fn refresh(&mut self) {
        let _ = self.tx.send(Cmd::Refresh);
    }

    fn load_detail(&mut self, number: u64) {
        {
            let mut shared = lock(&self.shared);
            if matches!(shared.view.details.get(&number), Some(Ok(_)))
                || !shared.inflight.insert(number)
            {
                return;
            }
        }
        if self.tx.send(Cmd::Detail(number)).is_err() {
            lock(&self.shared).inflight.remove(&number);
        }
    }
}

impl GhFeed {
    /// [`GhFeed::with`] on [`LoginShellGh`] and the system clock.
    pub fn start(repo: PathBuf, cache: PathBuf, wake: Arc<dyn Fn() + Send + Sync>) -> GhFeed {
        GhFeed::with(
            repo,
            cache,
            Box::new(LoginShellGh),
            Arc::new(|| {
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map_or(0, |d| d.as_millis() as Millis)
            }),
            wake,
        )
    }

    /// The feed of `repo`, cached in `cache`; `wake` is called whenever
    /// [`IssueFeed::view`] has something new.
    pub fn with(
        repo: PathBuf,
        cache: PathBuf,
        gh: Box<dyn GhRun>,
        clock: Arc<dyn Fn() -> Millis + Send + Sync>,
        wake: Arc<dyn Fn() + Send + Sync>,
    ) -> GhFeed {
        let mut view = GitHubView::default();
        if let Some(cached) = read_cache(&cache) {
            view.repo = cached.repo;
            view.tasks = cached.tasks;
            view.fetched = Some(cached.fetched);
        }
        // The first fetch starts at once.
        view.refreshing = true;
        let shared = Arc::new(Mutex::new(Shared {
            view,
            inflight: BTreeSet::new(),
        }));
        let (tx, rx) = mpsc::channel();
        let worker = Worker {
            repo,
            verified: None,
            cache,
            gh,
            clock,
            wake,
            shared: Arc::clone(&shared),
        };
        if std::thread::Builder::new()
            .name("tasks-github".into())
            .spawn(move || worker.run(rx))
            .is_err()
        {
            let mut s = lock(&shared);
            s.view.refreshing = false;
            s.view.failure = Some(GhFailure::Failed("スレッドを開始できません".into()));
        }
        GhFeed { shared, tx }
    }
}

struct Worker {
    repo: PathBuf,
    /// `owner/name` as `gh repo view` said in this process; the cached one
    /// is only shown until then, since the remote may have changed.
    verified: Option<String>,
    cache: PathBuf,
    gh: Box<dyn GhRun>,
    clock: Arc<dyn Fn() -> Millis + Send + Sync>,
    wake: Arc<dyn Fn() + Send + Sync>,
    shared: Arc<Mutex<Shared>>,
}

impl Worker {
    fn run(mut self, rx: Receiver<Cmd>) {
        let mut last = self.fetch_list();
        loop {
            match rx.recv_timeout(TICK) {
                Ok(Cmd::Detail(n)) => self.fetch_detail(n),
                Ok(Cmd::Refresh) => {
                    // One fetch answers every refresh already waiting.
                    let mut pending = Vec::new();
                    while let Ok(cmd) = rx.try_recv() {
                        if let Cmd::Detail(n) = cmd {
                            pending.push(n);
                        }
                    }
                    lock(&self.shared).view.refreshing = true;
                    (self.wake)();
                    last = self.fetch_list();
                    for n in pending {
                        self.fetch_detail(n);
                    }
                }
                Err(RecvTimeoutError::Timeout) => {
                    if (self.clock)().saturating_sub(last) >= REFRESH_MS {
                        lock(&self.shared).view.refreshing = true;
                        (self.wake)();
                        last = self.fetch_list();
                    }
                }
                Err(RecvTimeoutError::Disconnected) => return,
            }
        }
    }

    fn call(&mut self, args: &[&str]) -> Result<String, GhFailure> {
        let args: Vec<String> = args.iter().map(|a| (*a).to_owned()).collect();
        self.gh.run(&self.repo, &args)
    }

    fn list(&mut self) -> Result<(String, Vec<Task>), GhFailure> {
        let repo = match self.verified.clone() {
            Some(repo) => repo,
            None => self.verify()?,
        };
        let fields = "number,title,state,labels,updatedAt,closedAt,url";
        let (open, closed) = (OPEN_LIMIT.to_string(), CLOSED_LIMIT.to_string());
        let mut tasks = Vec::new();
        for (state, limit) in [("open", &open), ("closed", &closed)] {
            // Named outright: gh's own idea of this repository (its
            // default, the remote) may have moved since it was verified,
            // and every ref below names the verified one.
            let mut args = vec![
                "issue", "list", "--state", state, "--limit", limit, "--json", fields,
            ];
            if state == "closed" {
                // Unsorted, gh lists newest-created first: an old issue
                // closed today would miss the 30 and vanish rather than
                // reach 完了. Closing an issue updates it.
                args.extend(["--search", "sort:updated-desc"]);
            }
            args.extend(["--repo", &repo]);
            let out = self.call(&args)?;
            tasks.extend(parse_list(&out, &repo).map_err(GhFailure::Failed)?);
        }
        Ok((repo, merge(tasks)))
    }

    /// Fetches the listing and publishes it; returns when it was fetched,
    /// the base of the next timed refresh. The cache is written before the
    /// view shows the listing, and `wake` called after.
    fn fetch_list(&mut self) -> Millis {
        let result = self.list();
        let now = (self.clock)();
        if let Ok((repo, tasks)) = &result {
            write_cache(
                &self.cache,
                &Cache {
                    repo: Some(repo.clone()),
                    tasks: tasks.clone(),
                    fetched: now,
                },
            );
        }
        {
            let mut shared = lock(&self.shared);
            shared.view.refreshing = false;
            match result {
                Ok((repo, tasks)) => {
                    // Details are kept by number: another repository's #N
                    // is another issue, so those of the one before go.
                    if shared.view.repo.as_deref() != Some(repo.as_str()) {
                        shared.view.details.clear();
                    }
                    shared.view.repo = Some(repo);
                    shared.view.tasks = tasks;
                    shared.view.fetched = Some(now);
                    shared.view.failure = None;
                }
                Err(failure) => shared.view.failure = Some(failure),
            }
        }
        (self.wake)();
        now
    }

    /// `gh repo view`: the repository every listing and detail is then
    /// read from, by name.
    fn verify(&mut self) -> Result<String, GhFailure> {
        let out = self.call(&["repo", "view", "--json", "nameWithOwner"])?;
        let repo = parse_repo(&out).map_err(GhFailure::Failed)?;
        self.verified = Some(repo.clone());
        Ok(repo)
    }

    fn fetch_detail(&mut self, number: u64) {
        let n = number.to_string();
        // The repository the listed issues' refs name (the last listing's,
        // or the cache's while a refresh fails): the detail must be that
        // issue's, wherever gh points now.
        let listed = lock(&self.shared).view.repo.clone();
        let repo = match listed.clone() {
            Some(repo) => Ok(repo),
            None => self.verify(),
        };
        let result = repo
            .and_then(|repo| {
                self.call(&[
                    "issue",
                    "view",
                    &n,
                    "--json",
                    "body,comments",
                    "--repo",
                    &repo,
                ])
            })
            .and_then(|out| parse_view(&out).map_err(GhFailure::Failed));
        {
            let mut shared = lock(&self.shared);
            shared.inflight.remove(&number);
            // Kept only while the listing still names the repository it was
            // read for.
            if listed.is_none() || shared.view.repo == listed {
                shared.view.details.insert(number, result);
            }
        }
        (self.wake)();
    }
}

/// The open and closed listings as one, newest first: an issue in both
/// (closed between the two calls) once, as it was updated last, closed on a
/// tie.
fn merge(mut tasks: Vec<Task>) -> Vec<Task> {
    tasks.sort_by_key(|t| {
        (
            t.r#ref.clone(),
            std::cmp::Reverse(t.updated),
            t.state != State::Closed,
        )
    });
    tasks.dedup_by(|a, b| a.r#ref == b.r#ref);
    tasks.sort_by_key(|t| std::cmp::Reverse((t.updated, t.r#ref.clone())));
    tasks
}

/// What a `gh` call that failed means: whether it could be spawned, its
/// exit status, its stderr.
/// Whether a (lowercased) stderr line is a shell saying `gh` itself is
/// missing, not another command an rc file runs (`ghq`, `ghcup`):
/// zsh's `command not found: gh`, or `gh: command not found` / `gh: not
/// found` / `gh: no such file…` with `gh` a whole word.
fn says_gh_is_missing(line: &str) -> bool {
    let line = line.trim();
    if line.strip_suffix("command not found: gh").is_some() {
        return true;
    }
    ["gh: command not found", "gh: not found", "gh: no such file"]
        .iter()
        .any(|says| {
            line.match_indices(says).any(|(at, _)| {
                line[..at]
                    .chars()
                    .next_back()
                    .is_none_or(|c| c == ' ' || c == ':')
            })
        })
}

pub fn classify(spawned: bool, status: Option<i32>, stderr: &str) -> GhFailure {
    let low = stderr.to_lowercase();
    if !spawned || status == Some(127) || low.lines().any(says_gh_is_missing) {
        return GhFailure::NoGh;
    }
    // Before the login hint: gh's "no GitHub remote" message ends with one
    // ("To tell gh about a new GitHub host, please use `gh auth login`").
    if low.contains("not a git repository")
        || low.contains("no git remotes")
        || low.contains("none of the git remotes")
        || low.contains("could not resolve to a repository")
    {
        return GhFailure::NotGitHub;
    }
    if low.contains("gh auth login") || low.contains("not logged in") {
        return GhFailure::NotAuthenticated;
    }
    let tail: Vec<&str> = stderr
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    let tail = tail[tail.len().saturating_sub(3)..].join(" / ");
    let tail: String = tail.chars().take(300).collect();
    GhFailure::Failed(match (tail.is_empty(), status) {
        (true, Some(code)) => format!("終了コード {code}"),
        (true, None) => "シグナルで終了".to_owned(),
        (false, _) => tail,
    })
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RepoJson {
    name_with_owner: String,
}

/// `owner/name` from `gh repo view --json nameWithOwner`.
pub fn parse_repo(json: &str) -> Result<String, String> {
    let repo: RepoJson = serde_json::from_str(json.trim()).map_err(|e| e.to_string())?;
    if repo
        .name_with_owner
        .split('/')
        .filter(|p| !p.is_empty())
        .count()
        != 2
    {
        return Err(format!(
            "owner/name ではありません: {}",
            repo.name_with_owner
        ));
    }
    Ok(repo.name_with_owner)
}

#[derive(Deserialize)]
struct LabelJson {
    name: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct IssueJson {
    number: u64,
    title: String,
    state: String,
    #[serde(default)]
    labels: Vec<LabelJson>,
    updated_at: String,
    /// `null` for an open issue; absent from an older cached listing.
    #[serde(default)]
    closed_at: Option<String>,
    url: String,
}

/// The tasks of a `gh issue list --json number,title,state,labels,updatedAt,closedAt,url`.
pub fn parse_list(json: &str, repo: &str) -> Result<Vec<Task>, String> {
    let issues: Vec<IssueJson> = serde_json::from_str(json.trim()).map_err(|e| e.to_string())?;
    issues
        .into_iter()
        .map(|i| {
            let state = match i.state.to_ascii_uppercase().as_str() {
                "OPEN" => State::Open,
                "CLOSED" => State::Closed,
                other => return Err(format!("#{}: 不明な state {other}", i.number)),
            };
            Ok(Task {
                r#ref: TaskRef::GitHub {
                    repo: repo.to_owned(),
                    number: i.number,
                },
                title: i.title,
                body: None,
                state,
                labels: i.labels.into_iter().map(|l| l.name).collect(),
                updated: parse_time(&i.updated_at)?,
                url: Some(i.url),
                closed: i.closed_at.as_deref().map(parse_time).transpose()?,
                problem: None,
            })
        })
        .collect()
}

#[derive(Deserialize)]
struct AuthorJson {
    login: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CommentJson {
    author: Option<AuthorJson>,
    #[serde(default)]
    body: String,
    created_at: String,
}

#[derive(Deserialize)]
struct ViewJson {
    #[serde(default)]
    body: String,
    #[serde(default)]
    comments: Vec<CommentJson>,
}

/// The detail of a `gh issue view N --json body,comments`.
pub fn parse_view(json: &str) -> Result<IssueDetail, String> {
    let view: ViewJson = serde_json::from_str(json.trim()).map_err(|e| e.to_string())?;
    let comments = view
        .comments
        .into_iter()
        .map(|c| {
            Ok(Comment {
                // A deleted account has no author.
                author: c.author.map_or_else(|| "ghost".to_owned(), |a| a.login),
                body: c.body,
                at: parse_time(&c.created_at)?,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    Ok(IssueDetail {
        body: view.body,
        comments,
    })
}

/// `2026-11-24T22:39:25Z` (fraction and `±hh:mm` accepted) as epoch millis.
fn parse_time(text: &str) -> Result<Millis, String> {
    let bad = || format!("日時を読めません: {text}");
    let b = text.as_bytes();
    let num = |from: usize, len: usize| -> Option<i64> {
        let s = text.get(from..from + len)?;
        s.bytes()
            .all(|c| c.is_ascii_digit())
            .then(|| s.parse().ok())?
    };
    if b.len() < 20 || b[4] != b'-' || b[7] != b'-' || !matches!(b[10], b'T' | b' ') {
        return Err(bad());
    }
    let (y, mo, d) = (
        num(0, 4).ok_or_else(bad)?,
        num(5, 2).ok_or_else(bad)?,
        num(8, 2).ok_or_else(bad)?,
    );
    let (h, mi, s) = (
        num(11, 2).ok_or_else(bad)?,
        num(14, 2).ok_or_else(bad)?,
        num(17, 2).ok_or_else(bad)?,
    );
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) || h > 23 || mi > 59 || s > 60 {
        return Err(bad());
    }
    let mut at = 19;
    let mut millis = 0;
    if b.get(at) == Some(&b'.') {
        let digits = text[at + 1..]
            .bytes()
            .take_while(u8::is_ascii_digit)
            .count();
        let frac = &text[at + 1..at + 1 + digits];
        millis = format!("{frac:0<3}")[..3]
            .parse::<i64>()
            .map_err(|_| bad())?;
        at += 1 + digits;
    }
    let offset = match &text[at..] {
        "Z" | "z" => 0,
        rest if rest.len() == 6
            && matches!(rest.as_bytes()[0], b'+' | b'-')
            && &rest[3..4] == ":" =>
        {
            let oh: i64 = rest[1..3].parse().map_err(|_| bad())?;
            let om: i64 = rest[4..6].parse().map_err(|_| bad())?;
            let secs = oh * 3600 + om * 60;
            if rest.starts_with('-') { -secs } else { secs }
        }
        _ => return Err(bad()),
    };
    // Days from civil (Howard Hinnant).
    let (yy, mm) = if mo <= 2 {
        (y - 1, mo + 9)
    } else {
        (y, mo - 3)
    };
    let era = yy.div_euclid(400);
    let yoe = yy - era * 400;
    let doy = (153 * mm + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    let secs = days * 86_400 + h * 3600 + mi * 60 + s - offset;
    u64::try_from(secs * 1000 + millis).map_err(|_| bad())
}
