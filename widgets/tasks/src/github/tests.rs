use std::collections::VecDeque;
use std::os::unix::fs::PermissionsExt;
use std::sync::atomic::{AtomicU64, Ordering};

use super::*;

const REPO_JSON: &str = include_str!("testdata/repo.json");
const OPEN: &str = include_str!("testdata/list_open.json");
const CLOSED: &str = include_str!("testdata/list_closed.json");
const VIEW: &str = include_str!("testdata/view.json");

const T42: Millis = 1_795_559_965_000; // 2026-11-24T22:39:25Z
const T7: Millis = 1_767_323_045_000; // 2026-01-02T03:04:05Z
const T9: Millis = 1_795_168_800_000; // 2026-11-20T10:00:00Z

fn scratch(name: &str) -> PathBuf {
    static N: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "tasks-github-{name}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn ac23_repo_list_and_view_parse_the_fixtures() {
    assert_eq!(parse_repo(REPO_JSON).unwrap(), "noa-dev/noa");
    assert!(parse_repo("{}").is_err());
    assert!(parse_repo("nope").is_err());

    let open = parse_list(OPEN, "noa-dev/noa").unwrap();
    assert_eq!(open.len(), 2);
    assert_eq!(
        open[0].r#ref,
        TaskRef::GitHub {
            repo: "noa-dev/noa".into(),
            number: 42
        }
    );
    assert_eq!(open[0].title, "Crash on resize: \"quoted\" $(title)");
    assert_eq!(open[0].labels, ["bug", "good first issue"]);
    assert_eq!(open[0].updated, T42);
    assert_eq!(open[0].state, State::Open);
    assert_eq!(
        open[0].url.as_deref(),
        Some("https://github.com/noa-dev/noa/issues/42")
    );
    assert_eq!(open[0].body, None);
    assert_eq!(open[1].updated, T7);
    assert!(open[1].labels.is_empty());

    let closed = parse_list(CLOSED, "noa-dev/noa").unwrap();
    assert_eq!(closed[0].state, State::Closed);
    assert_eq!(closed[0].updated, T9);
    assert!(parse_list("[]", "a/b").unwrap().is_empty());

    let detail = parse_view(VIEW).unwrap();
    assert_eq!(detail.body, "Steps:\r\n1. resize\r\n2. crash");
    assert_eq!(detail.comments.len(), 2);
    assert_eq!(detail.comments[0].author, "alice");
    assert_eq!(detail.comments[0].at, 1_795_514_400_000);
    assert_eq!(detail.comments[1].at, 1_795_519_815_250);

    for bad in ["", "{", "{\"number\":1}", "[{\"number\":1}]"] {
        assert!(parse_list(bad, "a/b").is_err(), "{bad}");
    }
    assert!(parse_view("[").is_err());
    assert!(
        parse_list(
            "[{\"number\":1,\"title\":\"t\",\"state\":\"OPEN\",\"labels\":[],\"updatedAt\":\"yesterday\",\"url\":\"u\"}]",
            "a/b"
        )
        .is_err()
    );
}

#[test]
fn time_offsets_and_epoch() {
    assert_eq!(parse_time("1970-01-01T00:00:00Z").unwrap(), 0);
    assert_eq!(parse_time("1970-01-01T09:00:00+09:00").unwrap(), 0);
    assert_eq!(parse_time("2000-02-29T00:00:00Z").unwrap(), 951_782_400_000);
}

/// An interactive rc file's own missing command (ghq, ghcup) is not gh
/// missing: the real reason behind gh's failure must come through.
#[test]
fn ac24_another_command_missing_in_the_rc_file_is_not_no_gh() {
    for rc in [
        "zsh: command not found: ghq",
        "zsh: command not found: ghcup",
        "bash: ghq: command not found",
        "sh: 1: ghcup: not found",
    ] {
        let stderr = format!("{rc}\nTo get started with GitHub CLI, please run:  gh auth login\n");
        assert_eq!(
            classify(true, Some(4), &stderr),
            GhFailure::NotAuthenticated,
            "{rc}"
        );
    }
    for missing in [
        "zsh: command not found: gh",
        "bash: gh: command not found",
        "bash: line 1: gh: command not found",
        "sh: 1: gh: not found",
        "gh: no such file or directory",
    ] {
        assert_eq!(
            classify(true, Some(1), missing),
            GhFailure::NoGh,
            "{missing}"
        );
    }
}

#[test]
fn ac24_classify_failures() {
    assert_eq!(classify(false, None, "no such file"), GhFailure::NoGh);
    assert_eq!(classify(true, Some(127), ""), GhFailure::NoGh);
    assert_eq!(
        classify(true, Some(1), "zsh: command not found: gh"),
        GhFailure::NoGh
    );
    assert_eq!(
        classify(
            true,
            Some(4),
            "To get started with GitHub CLI, please run:  gh auth login"
        ),
        GhFailure::NotAuthenticated
    );
    assert_eq!(
        classify(true, Some(1), "fatal: not a git repository (or any parent)"),
        GhFailure::NotGitHub
    );
    assert_eq!(
        classify(
            true,
            Some(1),
            "none of the git remotes configured for this repository point to a known GitHub host. To tell gh about a new GitHub host, please use `gh auth login`\n"
        ),
        GhFailure::NotGitHub
    );
    assert_eq!(
        classify(true, Some(1), "\n a\n\n b\n c\n d\n"),
        GhFailure::Failed("b / c / d".into())
    );
    assert_eq!(
        classify(true, Some(9), ""),
        GhFailure::Failed("終了コード 9".into())
    );
}

fn fake_gh(dir: &Path, script: &str) {
    let path = dir.join("gh");
    std::fs::write(&path, format!("#!/bin/sh\n{script}\n")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// A POSIX shell for `run_shell` that is interactive but not a login shell.
/// `invocation` passes `-l`, and a macOS login shell runs `/etc/profile`,
/// whose `path_helper` puts the system directories ahead of the test's
/// `PATH`, so a `gh` installed on the host (as on CI runners) shadows the
/// fake one. This script drops that `-l` and runs `/bin/sh` with the rest.
fn sh() -> String {
    let dir = scratch("shell");
    let path = dir.join("sh");
    std::fs::write(
        &path,
        "#!/bin/sh\n[ \"$1\" = -l ] && shift\nexec /bin/sh \"$@\"\n",
    )
    .unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path.to_str().unwrap().to_owned()
}

fn path_with(dir: &Path) -> String {
    format!(
        "{}:{}",
        dir.display(),
        std::env::var("PATH").unwrap_or_default()
    )
}

/// Waits until `pid` (written to `file`) is gone; zombies are reaped by
/// init once their parent died.
fn assert_dies(file: &Path) {
    let pid: libc::pid_t = std::fs::read_to_string(file)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    // SAFETY: signal 0 only checks that the process exists.
    wait(&format!("pid {pid} to die"), || unsafe {
        libc::kill(pid, 0) == -1
    });
}

#[test]
fn ac24_a_call_past_the_timeout_is_killed() {
    let dir = scratch("timeout");
    fake_gh(&dir, "exec sleep 30");
    let path = path_with(&dir);
    let started = Instant::now();
    let got = run_shell(
        &sh(),
        &dir,
        &["issue".into(), "list".into()],
        Duration::from_millis(400),
        &[("PATH", &path)],
    );
    assert_eq!(got, Err(GhFailure::Timeout));
    assert!(started.elapsed() < Duration::from_secs(10));
}

#[test]
fn ac24_the_timeout_kills_what_gh_started_too() {
    let dir = scratch("timeout-group");
    let pid = dir.join("pid");
    // gh's own child (git, ssh, a credential helper) holding the pipes.
    fake_gh(
        &dir,
        &format!("sleep 30 &\necho $! > '{}'\nwait", pid.display()),
    );
    let path = path_with(&dir);
    // On a loaded macOS a shell can sit in dyld for seconds before its first
    // line; killed before the fake gh wrote the pid, a run proves nothing,
    // so it is tried again.
    for tries in 1.. {
        let started = Instant::now();
        let got = run_shell(
            &sh(),
            &dir,
            &["issue".into(), "list".into()],
            Duration::from_secs(2),
            &[("PATH", &path)],
        );
        assert_eq!(got, Err(GhFailure::Timeout));
        assert!(started.elapsed() < Duration::from_secs(8));
        if std::fs::read_to_string(&pid).is_ok_and(|s| !s.trim().is_empty()) {
            break;
        }
        assert!(tries < 5, "gh never started in {tries} tries");
    }
    assert_dies(&pid);
}

#[test]
fn ac24_a_pipe_held_open_by_an_rc_job_loses_no_output() {
    let dir = scratch("held");
    let home = scratch("held-home");
    let pid = dir.join("pid");
    // An interactive rc file starting a job that keeps stdout and stderr.
    let rc = home.join("rc");
    std::fs::write(&rc, format!("sleep 30 &\necho $! > '{}'\n", pid.display())).unwrap();
    let path = path_with(&dir);
    let envs = [
        ("PATH", path.as_str()),
        ("HOME", home.to_str().unwrap()),
        ("ENV", rc.to_str().unwrap()),
    ];
    let args = ["issue".to_owned(), "list".to_owned()];

    fake_gh(&dir, "echo '[1]'");
    let started = Instant::now();
    let got = run_shell(&sh(), &dir, &args, Duration::from_secs(10), &envs);
    assert_eq!(got.unwrap().trim(), "[1]");
    assert!(started.elapsed() < Duration::from_secs(5));
    assert_dies(&pid);

    fake_gh(
        &dir,
        "echo 'To get started, please run:  gh auth login' >&2\nexit 4",
    );
    let got = run_shell(&sh(), &dir, &args, Duration::from_secs(10), &envs);
    assert_eq!(got, Err(GhFailure::NotAuthenticated));
    assert_dies(&pid);
}

#[test]
fn ac24_a_missing_gh_through_the_shell_is_no_gh() {
    let dir = scratch("nogh");
    let got = run_shell(
        &sh(),
        &dir,
        &["repo".into(), "view".into()],
        Duration::from_secs(10),
        &[("PATH", "/nonexistent-dir")],
    );
    assert_eq!(got, Err(GhFailure::NoGh));
    let got = run_shell("/nonexistent/zsh", &dir, &[], Duration::from_secs(10), &[]);
    assert_eq!(got, Err(GhFailure::NoGh));
}

// ---- the feed on a fake gh and clock

type Calls = Arc<Mutex<Vec<Vec<String>>>>;

struct Fake {
    calls: Calls,
    /// Replies in order of `issue list`; `repo view` and `issue view` are fixed.
    fail_lists: Arc<Mutex<VecDeque<GhFailure>>>,
    view_fails: Arc<Mutex<VecDeque<GhFailure>>>,
}

impl GhRun for Fake {
    fn run(&mut self, _cwd: &Path, args: &[String]) -> Result<String, GhFailure> {
        self.calls.lock().unwrap().push(args.to_vec());
        let a: Vec<&str> = args.iter().map(String::as_str).collect();
        match a.as_slice() {
            ["repo", "view", ..] => Ok(REPO_JSON.to_owned()),
            ["issue", "list", "--state", state, ..] => {
                if let Some(f) = self.fail_lists.lock().unwrap().pop_front() {
                    return Err(f);
                }
                Ok(if *state == "open" { OPEN } else { CLOSED }.to_owned())
            }
            ["issue", "view", ..] => match self.view_fails.lock().unwrap().pop_front() {
                Some(f) => Err(f),
                None => Ok(VIEW.to_owned()),
            },
            other => panic!("unexpected gh call {other:?}"),
        }
    }
}

struct Rig {
    feed: GhFeed,
    calls: Calls,
    clock: Arc<AtomicU64>,
    wakes: Arc<AtomicU64>,
    fail_lists: Arc<Mutex<VecDeque<GhFailure>>>,
    view_fails: Arc<Mutex<VecDeque<GhFailure>>>,
    cache: PathBuf,
}

fn rig(dir: &Path) -> Rig {
    rig_with(dir, Duration::ZERO)
}

/// A rig whose `wake` takes `wake_delay`.
fn rig_with(dir: &Path, wake_delay: Duration) -> Rig {
    let calls: Calls = Arc::default();
    let fail_lists: Arc<Mutex<VecDeque<GhFailure>>> = Arc::default();
    let view_fails: Arc<Mutex<VecDeque<GhFailure>>> = Arc::default();
    let clock = Arc::new(AtomicU64::new(1_000_000));
    let wakes = Arc::new(AtomicU64::new(0));
    let cache = dir.join("noa").join("github.json");
    let (c, w) = (Arc::clone(&clock), Arc::clone(&wakes));
    let feed = GhFeed::with(
        dir.to_path_buf(),
        cache.clone(),
        Box::new(Fake {
            calls: Arc::clone(&calls),
            fail_lists: Arc::clone(&fail_lists),
            view_fails: Arc::clone(&view_fails),
        }),
        Arc::new(move || c.load(Ordering::SeqCst)),
        Arc::new(move || {
            std::thread::sleep(wake_delay);
            w.fetch_add(1, Ordering::SeqCst);
        }),
    );
    Rig {
        feed,
        calls,
        clock,
        wakes,
        fail_lists,
        view_fails,
        cache,
    }
}

fn wait(what: &str, mut ok: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs(10);
    while !ok() {
        assert!(Instant::now() < end, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn list_calls(rig: &Rig) -> usize {
    rig.calls
        .lock()
        .unwrap()
        .iter()
        .filter(|c| c[..2] == ["issue", "list"])
        .count()
}

#[test]
fn ac25_first_fetch_fills_view_writes_cache_and_wakes() {
    let dir = scratch("first");
    let rig = rig(&dir);
    wait("first fetch", || rig.feed.view().fetched.is_some());
    let view = rig.feed.view();
    assert_eq!(view.repo.as_deref(), Some("noa-dev/noa"));
    assert!(!view.refreshing && view.failure.is_none());
    let numbers: Vec<_> = view
        .tasks
        .iter()
        .map(|t| match &t.r#ref {
            TaskRef::GitHub { number, .. } => *number,
            _ => unreachable!(),
        })
        .collect();
    // open and closed merged, newest first
    assert_eq!(numbers, [42, 9, 7]);
    assert_eq!(view.fetched, Some(1_000_000));
    // the cache is written before the view shows the listing
    let names: Vec<_> = std::fs::read_dir(rig.cache.parent().unwrap())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, ["github.json"]);
    let cached: Cache = serde_json::from_slice(&std::fs::read(&rig.cache).unwrap()).unwrap();
    assert_eq!(cached.tasks, view.tasks);
    // and the screen woken after it shows it
    wait("wake", || rig.wakes.load(Ordering::SeqCst) >= 1);
}

#[test]
fn ac25_the_next_refresh_counts_from_the_fetch_not_from_a_slow_wake() {
    let dir = scratch("slow-wake");
    let rig = rig_with(&dir, Duration::from_millis(300));
    wait("first fetch", || rig.feed.view().fetched.is_some());
    // The clock moves while `wake` still runs: the timer base is the
    // fetch's time, not the time `wake` returned.
    rig.clock.fetch_add(REFRESH_MS - 1, Ordering::SeqCst);
    std::thread::sleep(Duration::from_millis(700));
    assert_eq!(list_calls(&rig), 2, "not before 5 minutes");
    rig.clock.fetch_add(1, Ordering::SeqCst);
    wait("timed refresh", || list_calls(&rig) == 4);
}

#[test]
fn ac25_the_cached_repo_is_shown_until_gh_names_the_live_one() {
    let dir = scratch("moved");
    let cache = dir.join("noa").join("github.json");
    let stale = Task {
        r#ref: TaskRef::GitHub {
            repo: "alice/foo".into(),
            number: 1,
        },
        title: "old".into(),
        body: None,
        state: State::Open,
        labels: Vec::new(),
        updated: 5,
        url: None,
        closed: None,
        problem: None,
    };
    write_cache(
        &cache,
        &Cache {
            repo: Some("alice/foo".into()),
            tasks: vec![stale],
            fetched: 5,
        },
    );
    let mut rig = rig(&dir);
    wait("first fetch", || rig.feed.view().fetched == Some(1_000_000));
    let view = rig.feed.view();
    assert_eq!(view.repo.as_deref(), Some("noa-dev/noa"));
    assert!(view.tasks.iter().all(|t| matches!(
        &t.r#ref,
        TaskRef::GitHub { repo, .. } if repo == "noa-dev/noa"
    )));
    let cached: Cache = serde_json::from_slice(&std::fs::read(&cache).unwrap()).unwrap();
    assert_eq!(cached.repo.as_deref(), Some("noa-dev/noa"));
    let repo_views = |rig: &Rig| {
        rig.calls
            .lock()
            .unwrap()
            .iter()
            .filter(|c| c[..2] == ["repo", "view"])
            .count()
    };
    assert_eq!(repo_views(&rig), 1);
    assert_eq!(rig.calls.lock().unwrap()[0][..2], ["repo", "view"]);
    // once per feed: a refresh lists without asking again
    rig.feed.refresh();
    wait("refresh", || list_calls(&rig) == 4);
    assert_eq!(repo_views(&rig), 1);
}

#[test]
fn ac25_view_is_the_cache_before_the_first_fetch_returns() {
    let dir = scratch("cache");
    let first = rig(&dir);
    wait("first fetch", || first.feed.view().fetched.is_some());
    let expected = first.feed.view().tasks;
    drop(first);

    struct Stuck(mpsc::Receiver<()>);
    impl GhRun for Stuck {
        fn run(&mut self, _: &Path, _: &[String]) -> Result<String, GhFailure> {
            let _ = self.0.recv_timeout(Duration::from_secs(10));
            Err(GhFailure::NoGh)
        }
    }
    let (release, rx) = mpsc::channel();
    let feed = GhFeed::with(
        dir.clone(),
        dir.join("noa").join("github.json"),
        Box::new(Stuck(rx)),
        Arc::new(|| 5),
        Arc::new(|| {}),
    );
    let view = feed.view();
    assert_eq!(view.repo.as_deref(), Some("noa-dev/noa"));
    assert_eq!(view.tasks, expected);
    assert_eq!(view.fetched, Some(1_000_000));
    assert!(view.refreshing);
    drop(release);
}

#[test]
fn ac25_refetches_only_after_five_minutes_or_on_refresh() {
    let dir = scratch("again");
    let mut rig = rig(&dir);
    wait("first fetch", || rig.feed.view().fetched.is_some());
    assert_eq!(list_calls(&rig), 2);

    rig.clock.fetch_add(REFRESH_MS - 1, Ordering::SeqCst);
    std::thread::sleep(Duration::from_millis(600));
    assert_eq!(list_calls(&rig), 2, "not before 5 minutes");

    rig.clock.fetch_add(1, Ordering::SeqCst);
    wait("timed refresh", || list_calls(&rig) == 4);
    wait("timed refresh done", || !rig.feed.view().refreshing);

    rig.feed.refresh();
    wait("manual refresh", || list_calls(&rig) == 6);
    // the repo is asked once, not on every refresh
    let repo_views = rig
        .calls
        .lock()
        .unwrap()
        .iter()
        .filter(|c| c[..2] == ["repo", "view"])
        .count();
    assert_eq!(repo_views, 1);
}

#[test]
fn ac25_a_failure_keeps_the_tasks_and_a_success_clears_it() {
    let dir = scratch("fail");
    let mut rig = rig(&dir);
    wait("first fetch", || rig.feed.view().fetched.is_some());
    let before = rig.feed.view().tasks;
    let before_cache = std::fs::read(&rig.cache).unwrap();

    rig.clock.fetch_add(10, Ordering::SeqCst);
    rig.fail_lists
        .lock()
        .unwrap()
        .push_back(GhFailure::NotAuthenticated);
    rig.feed.refresh();
    wait("failure", || rig.feed.view().failure.is_some());
    wait("idle", || !rig.feed.view().refreshing);
    let view = rig.feed.view();
    assert_eq!(view.failure, Some(GhFailure::NotAuthenticated));
    assert_eq!(view.tasks, before);
    assert_eq!(view.fetched, Some(1_000_000));
    assert_eq!(std::fs::read(&rig.cache).unwrap(), before_cache);

    rig.feed.refresh();
    wait("recovery", || rig.feed.view().failure.is_none());
    assert_eq!(rig.feed.view().fetched, Some(1_000_010));
}

#[test]
fn ac25_load_detail_fetches_a_number_once_and_retries_a_failure() {
    let dir = scratch("detail");
    let mut rig = rig(&dir);
    wait("first fetch", || rig.feed.view().fetched.is_some());
    let views = |rig: &Rig| {
        rig.calls
            .lock()
            .unwrap()
            .iter()
            .filter(|c| c[..2] == ["issue", "view"])
            .count()
    };

    rig.view_fails
        .lock()
        .unwrap()
        .push_back(GhFailure::Failed("boom".into()));
    rig.feed.load_detail(42);
    rig.feed.load_detail(42); // already asked
    wait("failed detail", || {
        matches!(rig.feed.view().details.get(&42), Some(Err(_)))
    });
    assert_eq!(views(&rig), 1);

    rig.feed.load_detail(42); // Err: asks again
    wait("detail", || {
        matches!(rig.feed.view().details.get(&42), Some(Ok(_)))
    });
    assert_eq!(views(&rig), 2);
    let detail = rig.feed.view().details[&42].clone().unwrap();
    assert_eq!(detail.comments.len(), 2);

    rig.feed.load_detail(42); // Ok: never again
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(views(&rig), 2);
    let last = rig.calls.lock().unwrap().last().unwrap().clone();
    assert_eq!(
        last,
        [
            "issue",
            "view",
            "42",
            "--json",
            "body,comments",
            "--repo",
            "noa-dev/noa"
        ]
    );
}

#[test]
fn ac23_a_closed_issue_carries_when_it_was_closed() {
    let json = r#"[{"number":5,"title":"Old","state":"CLOSED","labels":[],
        "updatedAt":"2026-11-23T16:22:46Z","closedAt":"2026-11-14T05:56:39Z",
        "url":"https://github.com/o/n/issues/5"},
        {"number":6,"title":"Open","state":"OPEN","labels":[],
        "updatedAt":"2026-11-23T16:22:46Z","closedAt":null,
        "url":"https://github.com/o/n/issues/6"}]"#;
    let tasks = parse_list(json, "o/n").unwrap();
    assert_eq!(tasks[0].closed, Some(1_794_635_799_000));
    assert_eq!(tasks[1].closed, None);
}

/// `gh issue list` without a sort lists newest-created first, so an old
/// issue closed today would fall out of the 30 closed ones and vanish
/// instead of reaching 完了 (L1-3).
#[test]
fn ac23_closed_issues_are_listed_most_recently_updated_first() {
    let dir = scratch("closed-order");
    let rig = rig(&dir);
    wait("first fetch", || rig.feed.view().fetched.is_some());
    let calls = rig.calls.lock().unwrap().clone();
    let closed = calls
        .iter()
        .find(|c| {
            c.starts_with(&[
                "issue".into(),
                "list".into(),
                "--state".into(),
                "closed".into(),
            ])
        })
        .expect("a closed listing");
    let at = closed
        .iter()
        .position(|a| a == "--search")
        .expect("--search");
    assert_eq!(closed[at + 1], "sort:updated-desc", "{closed:?}");
}

#[test]
fn ac26_every_argv_is_read_only() {
    let dir = scratch("readonly");
    let mut rig = rig(&dir);
    wait("first fetch", || rig.feed.view().fetched.is_some());
    rig.feed.refresh();
    rig.feed.load_detail(7);
    wait("detail", || rig.feed.view().details.contains_key(&7));
    let calls = rig.calls.lock().unwrap().clone();
    assert!(calls.len() >= 5);
    for argv in &calls {
        let head = (argv[0].as_str(), argv[1].as_str());
        assert!(
            matches!(
                head,
                ("repo", "view") | ("issue", "list") | ("issue", "view")
            ),
            "{argv:?}"
        );
    }
}

#[test]
fn ac27_arguments_reach_gh_word_for_word() {
    let dir = scratch("argv");
    let out = dir.join("out.txt");
    fake_gh(
        &dir,
        &format!(
            "for a in \"$@\"; do printf '[%s]\\n' \"$a\"; done > '{}'\necho '{{\"ok\":true}}'",
            out.display()
        ),
    );
    let path = path_with(&dir);
    let args: Vec<String> = [
        "issue",
        "list",
        "--search",
        "it's \"quoted\" $(touch pwned) `id` ; & *",
        "two words",
        "",
    ]
    .map(String::from)
    .to_vec();
    let got = run_shell(
        &sh(),
        &dir,
        &args,
        Duration::from_secs(10),
        &[("PATH", &path)],
    )
    .unwrap();
    assert_eq!(got.trim(), "{\"ok\":true}");
    let seen = std::fs::read_to_string(&out).unwrap();
    let want: String = args.iter().map(|a| format!("[{a}]\n")).collect();
    assert_eq!(seen, want);
    assert!(!dir.join("pwned").exists());
}

#[test]
fn ac27_rc_noise_on_stdout_is_dropped() {
    let dir = scratch("noise");
    fake_gh(&dir, "echo '[1]'");
    let path = path_with(&dir);
    let home = scratch("noise-home");
    // dash and bash read $ENV / rc files when interactive; a banner must not
    // reach the JSON.
    std::fs::write(home.join("rc"), "echo banner\n").unwrap();
    let got = run_shell(
        &sh(),
        &dir,
        &["issue".into(), "list".into()],
        Duration::from_secs(10),
        &[("PATH", &path), ("ENV", home.join("rc").to_str().unwrap())],
    )
    .unwrap();
    assert_eq!(got.trim(), "[1]");
}

fn issue(number: u64, state: State, updated: Millis) -> Task {
    Task {
        r#ref: TaskRef::GitHub {
            repo: "a/b".into(),
            number,
        },
        title: format!("#{number}"),
        body: None,
        state,
        labels: Vec::new(),
        updated,
        url: None,
        closed: None,
        problem: None,
    }
}

#[test]
fn ac23_an_issue_closed_between_the_two_listings_is_listed_once() {
    // #42 open at 100, #99 at 150, #42 closed at 200 (the closed listing
    // ran after it was closed): #99 lies between the two #42s.
    let got = merge(vec![
        issue(42, State::Open, 100),
        issue(99, State::Open, 150),
        issue(42, State::Closed, 200),
    ]);
    assert_eq!(
        got,
        [issue(42, State::Closed, 200), issue(99, State::Open, 150)]
    );
    // closed within the same second: the closed one wins the tie
    let got = merge(vec![
        issue(42, State::Open, 100),
        issue(7, State::Open, 100),
        issue(42, State::Closed, 100),
    ]);
    assert_eq!(
        got,
        [issue(42, State::Closed, 100), issue(7, State::Open, 100)]
    );
}

#[test]
fn ac27_each_shell_gets_the_arguments_its_own_way() {
    let words = |w: &[&str]| w.iter().map(|w| (*w).to_owned()).collect::<Vec<_>>();
    let posix = "printf '\\nnoa-tasks-gh-output\\n'; exec gh \"$@\"";
    for shell in ["/bin/sh", "/bin/zsh", "/usr/local/bin/bash", "/bin/dash"] {
        assert_eq!(
            invocation(shell),
            (shell.to_owned(), words(&["-l", "-i", "-c", posix, "gh"])),
            "{shell}"
        );
    }
    // fish: no $0 after -c, and `--` so `--json` is not fish's option
    assert_eq!(
        invocation("/opt/homebrew/bin/fish"),
        (
            "/opt/homebrew/bin/fish".to_owned(),
            words(&[
                "-l",
                "-i",
                "-c",
                "printf '\\nnoa-tasks-gh-output\\n'; exec gh $argv",
                "--"
            ])
        )
    );
    for shell in ["/bin/tcsh", "/bin/csh", "/opt/homebrew/bin/nu"] {
        assert_eq!(
            invocation(shell),
            ("/bin/sh".to_owned(), words(&["-c", posix, "gh"])),
            "{shell}"
        );
    }
}

#[test]
fn ac27_a_shell_neither_sh_nor_fish_runs_gh_through_bin_sh() {
    let dir = scratch("csh");
    fake_gh(&dir, "printf '[%s]' \"$@\"");
    let path = path_with(&dir);
    let got = run_shell(
        "/nonexistent/tcsh",
        &dir,
        &["repo".into(), "view".into(), "a b".into()],
        Duration::from_secs(10),
        &[("PATH", &path)],
    );
    assert_eq!(got.unwrap(), "[repo][view][a b]");
}

/// A gh whose own idea of "this repository" (`gh repo set-default`, the
/// remote) moves from `a/one` to `b/two` after the first `repo view`:
/// listings and details without `--repo` come from wherever it points now.
struct Moving {
    calls: Calls,
    default: Arc<Mutex<String>>,
}

impl GhRun for Moving {
    fn run(&mut self, _cwd: &Path, args: &[String]) -> Result<String, GhFailure> {
        self.calls.lock().unwrap().push(args.to_vec());
        let pinned = args
            .iter()
            .position(|a| a == "--repo")
            .map(|i| args[i + 1].clone());
        let repo = pinned.unwrap_or_else(|| self.default.lock().unwrap().clone());
        let a: Vec<&str> = args.iter().map(String::as_str).collect();
        Ok(match a.as_slice() {
            ["repo", "view", ..] => {
                let out = format!("{{\"nameWithOwner\":\"{repo}\"}}");
                *self.default.lock().unwrap() = "b/two".into();
                out
            }
            ["issue", "list", "--state", "open", ..] => format!(
                "[{{\"number\":1,\"title\":\"from {repo}\",\"state\":\"OPEN\",\"labels\":[],\
                 \"updatedAt\":\"2026-11-20T10:00:00Z\",\"closedAt\":null,\
                 \"url\":\"https://github.com/{repo}/issues/1\"}}]"
            ),
            ["issue", "list", ..] => "[]".into(),
            ["issue", "view", ..] => format!("{{\"body\":\"from {repo}\",\"comments\":[]}}"),
            other => panic!("unexpected gh call {other:?}"),
        })
    }
}

/// The repository named once is the one listed and read from, so an
/// issue's ref always names the repository it came from.
#[test]
fn listings_and_details_come_from_the_repository_their_refs_name() {
    let dir = scratch("pinned");
    let calls: Calls = Arc::default();
    let mut feed = GhFeed::with(
        dir.clone(),
        dir.join("noa").join("github.json"),
        Box::new(Moving {
            calls: Arc::clone(&calls),
            default: Arc::new(Mutex::new("a/one".into())),
        }),
        Arc::new(|| 1_000_000),
        Arc::new(|| {}),
    );
    wait("first fetch", || feed.view().fetched.is_some());
    let view = feed.view();
    assert_eq!(view.repo.as_deref(), Some("a/one"));
    for task in &view.tasks {
        let TaskRef::GitHub { repo, .. } = &task.r#ref else {
            panic!("{task:?}")
        };
        let url = task.url.as_deref().unwrap();
        assert!(
            url.starts_with(&format!("https://github.com/{repo}/")),
            "{url} listed as {repo}"
        );
    }
    feed.load_detail(1);
    wait("detail", || feed.view().details.contains_key(&1));
    let detail = feed.view().details[&1].clone().unwrap();
    assert_eq!(detail.body, "from a/one");
}

/// gh now names `b/two`; listing fails, so the cached `a/one` issues stay
/// on screen. Their detail comes from `a/one`, the repository their refs
/// name, not from wherever gh points now.
struct ListFails;

impl GhRun for ListFails {
    fn run(&mut self, _cwd: &Path, args: &[String]) -> Result<String, GhFailure> {
        let pinned = args
            .iter()
            .position(|a| a == "--repo")
            .map(|i| args[i + 1].clone())
            .unwrap_or_else(|| "b/two".into());
        let a: Vec<&str> = args.iter().map(String::as_str).collect();
        match a.as_slice() {
            ["repo", "view", ..] => Ok("{\"nameWithOwner\":\"b/two\"}".into()),
            ["issue", "list", ..] => Err(GhFailure::Failed("network".into())),
            ["issue", "view", ..] => Ok(format!("{{\"body\":\"from {pinned}\",\"comments\":[]}}")),
            other => panic!("unexpected gh call {other:?}"),
        }
    }
}

#[test]
fn a_cached_issues_detail_comes_from_the_repository_its_ref_names() {
    let dir = scratch("stale-detail");
    let cache = dir.join("noa").join("github.json");
    std::fs::create_dir_all(cache.parent().unwrap()).unwrap();
    let cached = parse_list(
        "[{\"number\":1,\"title\":\"cached\",\"state\":\"OPEN\",\"labels\":[],\
          \"updatedAt\":\"2026-11-20T10:00:00Z\",\"url\":\"https://github.com/a/one/issues/1\"}]",
        "a/one",
    )
    .unwrap();
    std::fs::write(
        &cache,
        serde_json::json!({ "repo": "a/one", "tasks": cached, "fetched": 1 }).to_string(),
    )
    .unwrap();
    let mut feed = GhFeed::with(
        dir.clone(),
        cache,
        Box::new(ListFails),
        Arc::new(|| 1_000_000),
        Arc::new(|| {}),
    );
    wait("the failed refresh", || feed.view().failure.is_some());
    assert_eq!(
        feed.view().tasks[0].r#ref,
        TaskRef::GitHub {
            repo: "a/one".into(),
            number: 1
        }
    );
    feed.load_detail(1);
    wait("detail", || feed.view().details.contains_key(&1));
    assert_eq!(feed.view().details[&1].clone().unwrap().body, "from a/one");
}

/// gh names `b/two`; the first listing fails (the cached `a/one` issues
/// stay, and #1's detail is read from `a/one`), the next succeeds.
struct FailsOnce {
    lists: Arc<Mutex<u32>>,
}

impl GhRun for FailsOnce {
    fn run(&mut self, _cwd: &Path, args: &[String]) -> Result<String, GhFailure> {
        let pinned = args
            .iter()
            .position(|a| a == "--repo")
            .map(|i| args[i + 1].clone())
            .unwrap_or_else(|| "b/two".into());
        let a: Vec<&str> = args.iter().map(String::as_str).collect();
        match a.as_slice() {
            ["repo", "view", ..] => Ok("{\"nameWithOwner\":\"b/two\"}".into()),
            ["issue", "list", "--state", state, ..] => {
                let mut n = self.lists.lock().unwrap();
                *n += 1;
                if *n == 1 {
                    return Err(GhFailure::Failed("network".into()));
                }
                Ok(if *state == "open" {
                    format!(
                        "[{{\"number\":1,\"title\":\"from {pinned}\",\"state\":\"OPEN\",\"labels\":[],\
                         \"updatedAt\":\"2026-11-20T10:00:00Z\",\"url\":\"https://github.com/{pinned}/issues/1\"}}]"
                    )
                } else {
                    "[]".into()
                })
            }
            ["issue", "view", ..] => Ok(format!("{{\"body\":\"from {pinned}\",\"comments\":[]}}")),
            other => panic!("unexpected gh call {other:?}"),
        }
    }
}

/// Once the listing names another repository, a detail read from the
/// one before is not shown for the same number: it is read again.
#[test]
fn a_detail_from_the_repository_before_is_dropped_when_the_listing_moves() {
    let dir = scratch("detail-moves");
    let cache = dir.join("noa").join("github.json");
    std::fs::create_dir_all(cache.parent().unwrap()).unwrap();
    let cached = parse_list(
        "[{\"number\":1,\"title\":\"cached\",\"state\":\"OPEN\",\"labels\":[],\
          \"updatedAt\":\"2026-11-20T10:00:00Z\",\"url\":\"https://github.com/a/one/issues/1\"}]",
        "a/one",
    )
    .unwrap();
    std::fs::write(
        &cache,
        serde_json::json!({ "repo": "a/one", "tasks": cached, "fetched": 1 }).to_string(),
    )
    .unwrap();
    let mut feed = GhFeed::with(
        dir.clone(),
        cache,
        Box::new(FailsOnce {
            lists: Arc::default(),
        }),
        Arc::new(|| 1_000_000),
        Arc::new(|| {}),
    );
    wait("the failed refresh", || feed.view().failure.is_some());
    feed.load_detail(1);
    wait("a/one's detail", || feed.view().details.contains_key(&1));
    assert_eq!(feed.view().details[&1].clone().unwrap().body, "from a/one");
    feed.refresh();
    wait("b/two listed", || {
        feed.view().repo.as_deref() == Some("b/two")
    });
    let view = feed.view();
    assert!(
        !matches!(view.details.get(&1), Some(Ok(d)) if d.body == "from a/one"),
        "b/two#1 shows a/one's detail: {:?}",
        view.details.get(&1)
    );
}
