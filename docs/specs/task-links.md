# Task links (成果の関連リンク): Spec

| | |
|---|---|
| slug | task-links |
| status | proposed (2026-11-26); design only, nothing built; no owner sign-off |
| owner | simota |
| as-of | 2026-11-26 (HEAD 765da968) |
| crates | `widgets/tasks`, `widgets/review`, `widgets/verify` (each its own crate outside the workspace) |
| request | 「成果の関連リンク」: tie a task to everything its work produced (runs, changes/commits, Review sessions, Verify sessions, PRs) so that from one task's detail a person can trace what changed, what was checked against which version, and where the evidence is. Acceptance criteria AC-1 … AC-8 are fixed by the request (L3). |

This document is scaffolding, like `docs/specs/task-widget.md`: what outlives the build is the Decisions section and its reasons. Once a unit lands, its code is the only copy of its shape, and this file's L2 rows for that unit are deleted.

Everything stated about today's code was read at HEAD 765da968, with the file and line it came from. Statements this spec could not check are marked **[unverified]**.

## L0: Vision

- Problem: a task in the Tasks widget knows its runs (`runs.jsonl`) and nothing after them. The commits a run made sit on a `task-run-…` branch; a Review of that branch is a `rev-…` folder under `<data dir>/noa/reviews/`; a Verify of it is a `ver-…` folder under `<data dir>/noa/verifications/`; the PR is on GitHub. Nothing ties them back to the task, so "was the version we are about to merge the one that was reviewed and verified?" is answered by reading hashes by hand.
- Job to be done: open a task's detail and see, in one 「関連成果」 section, its runs, its change and the version it is at (v1, v2, …), every review and verification with the version it checked and its outcome, the latest version's missing checks (「未登録」), and its PRs; open each of them from there; and from a Review or Verify window go back to the task.
- Success definition: run a task in a new worktree, let the agent commit (v1), verify it (合格 on v1), let the agent commit again (v2), review it (承認 on v2): the detail shows 「変更 v2（現在）」, 「レビュー … v2 · 承認」, 「検証 … v1 · 合格」 and 「検証 v2 未登録」. After closing the run's pane and removing its worktree the detail shows the same.

## Reuse decision

The widgets already share code by **copying modules**, never by a shared crate: `fsx.rs` and `installed.rs` are copies (`docs/specs/task-widget.md`, L2-1: "`widgets/verify/src/fsx.rs` verbatim"), and the Tasks spec states "depending on no other widget; no shared crate is extracted". Their only path dependencies are `crates/noa-ipc` and `crates/noa-config` (`widgets/{tasks,review,verify}/Cargo.toml`). This feature follows that:

| Piece | Where | How it is shared |
|---|---|---|
| The link log format, its reader/writer, run matching, the way back to a task | `src/task_links.rs` in each of the three crates | one file, **byte-identical** in the three crates; a test in it `include_str!`s `../../tasks/src/task_links.rs` and compares itself to it, so a copy that drifts fails its crate's tests |
| Starting another widget (Review, Verify, Tasks) as a process of its own | `src/sibling.rs` in each of the three crates | byte-identical the same way; its body follows `widgets/agent-dashboard/src/repo_browser.rs` (`locate`: `Contents/Helpers/<bundled name>`, a plain `<crate>` beside the executable, then `widgets/<crate>/target/<profile>/<crate>`; `spawn`: no shell, `process_group(0)`, no stdio, `current_dir($HOME)`, a reaper thread) |

No new dependency: the shared files use `std`, `serde`, `serde_json`, `libc` and `noa_config`, which all three crates already depend on (their `Cargo.toml`s). No `Cargo.lock` changes, no workspace change, no change to `scripts/verify.sh` or CI (`widgets/tasks`, `widgets/review` and `widgets/verify` are already built and tested there, `AGENTS.md` Verification).

## Glossary

| Term | Meaning | Identifier |
|---|---|---|
| link log (関連ログ) | the append-only file of link events of one Tasks repository | `<data dir>/noa/tasks/<repo-key>/links.jsonl` |
| key dir | `<data dir>/noa/tasks/<repo-key>/`, Tasks' existing per-repository folder (`widgets/tasks/src/store.rs:3`) | `task_links::key_dirs` |
| related item (関連成果) | one row of the section: a run, the change, a review, a verification, a PR | `related::Related` |
| change (変更) | the commits the task's runs made | `Version`s |
| version (版, vN) | one distinct head commit of a task's change, numbered in the order the link log first names it | `Version { no, commit, run }` |
| base (着手時点) | the commit a run started from | `LinkEvent::ChangeBase` |
| session | a Review (`rev-…`) or a Verify (`ver-…`) | `SessionKind::{Review, Verify}` |
| origin (元のタスク) | the task (and run) a session was linked to | `task_links::Origin` |

## L1: Requirements

### L1-1. One link log per Tasks repository, outside every work tree

- Path: `<data dir>/noa/tasks/<repo-key>/links.jsonl`, beside the existing `runs.jsonl` and `github.json` (`widgets/tasks/src/store.rs:1-7`); `<data dir>/noa/tasks` is `noa_config::session_state_path()`'s parent joined with `tasks` (`store::default_root`, `widgets/tasks/src/store.rs:25`), `<repo-key>` is `store::repo_key` of the toplevel Tasks opened (`widgets/tasks/src/live.rs:123-131`).
- Append-only JSON lines, one event per line (L2-2). Never rewritten, never compacted. A torn last line is ended before the next append and skipped on read, as `runs::append` / `runs::replay` do (`widgets/tasks/src/runs.rs:88-129`). Lines that do not parse (an unknown `event`, a newer writer) are skipped, never an error.
- Every append takes an exclusive `flock` on `links.jsonl.lock` beside it, opened `O_NOFOLLOW`, as `runs::lock` does (`widgets/tasks/src/runs.rs:60-85`): three processes (Tasks, Review, Verify) may append to one log.
- Survives pane close and worktree removal by construction: the log names runs by id, commits by full SHA and sessions by id; it holds no pane id and reads nothing from a work tree to fold. Runs themselves stay where they already are, in `runs.jsonl` (L1-2).

Writers:

| Event | Written by | When |
|---|---|---|
| `Repo` | Tasks | its live feed starts on a repository and the log's last `Repo` names another path or there is none |
| `ChangeBase` | Tasks | a run starts (after its work tree exists) |
| `ChangeSeen` | Tasks | its change poll sees a run's head move (L1-3) |
| `SessionLinked` | Review / Verify | a session is created whose target matches a task run (L1-4) |
| `SessionOutcome` | Review / Verify | a linked session's verdict or ended state changes |
| `PrLinked`, `LinkRemoved` | Tasks | the person adds or removes a link (L1-8) |

### L1-2. Runs: the run log is the task → run link

Starting a run already appends `RunStarted { run, task, … }` to `runs.jsonl` before the spawn (`widgets/tasks/src/model/types.rs`, `RunEvent`; `widgets/tasks/src/live/start.rs:36`). That line is the task → run link; nothing is duplicated into the link log. `runs.jsonl`'s format is not changed (its JSON lines are pinned by `ac03_each_run_event_is_a_fixed_json_line_and_reads_back`, `widgets/tasks/src/model/types.rs`).

Review and Verify read `runs.jsonl` too, but only its `RunStarted` lines and only the fields `run`, `task`, `workdir`, `branch`, `at` (other fields and events ignored), so Tasks may add fields there later without breaking them.

### L1-3. Change versions

- A run's base: on start, Tasks appends `ChangeBase { run, commit }` with `git -C <workdir> rev-parse HEAD` of the run's work tree (the new worktree is made from `HEAD`, `widgets/tasks/src/worktree.rs:83-100`; a この作業ツリー run's work tree is the repository).
- Change poll, on the live feed's worker, on start, every `CHANGE_POLL_MS = 10_000` and on 再取得:
  - Runs with a branch (新しい worktree): one `git -C <repo> for-each-ref --format=%(refname:lstrip=2)%00%(objectname) refs/heads/task-run-*` (branch names are `task-<run-id>`, `worktree::branch_for`, `widgets/tasks/src/worktree.rs:78`). The branch ref lives in the repository, not in the worktree, so it is still read after the worktree is removed; a deleted branch stops new versions and keeps the recorded ones.
  - Runs without a branch (この作業ツリー): `git -C <workdir> rev-parse HEAD` while the run is `Starting` or `Alive`, and once more after it turns `Gone`.
  - A head that differs from the run's last recorded commit (its `ChangeSeen`, else its `ChangeBase`) is appended as `ChangeSeen { run, commit }`.
- Version (L2-3 gives the exact fold): the versions of a task are the distinct commits the link log names for the task's runs (`ChangeSeen`, and the `commit` of a `SessionLinked`, so whatever a review or verification targeted is always a version), excluding each run's base, numbered v1, v2, … in the order of their first line in the log. The current version is the last one. Versions span all runs of a task (a retry run's first commit after v2 is v3).
- A version is a commit, not a snapshot of uncommitted files. A session that checked uncommitted changes on top of commit C is shown as "on vC + 未コミット" and does not count as checking vC (L1-5).

### L1-4. Linking Review and Verify sessions

How they learn the task: **by matching their target against the run logs**, not by a flag. Any session whose target is a task run's work tree, branch or recorded commit is linked, however it was started (Noa's View menu passes `--repo=<focused pane cwd>`, `crates/noa-app/src/helper_program.rs:81-95`, so a Review opened from the run's float should target its worktree **[unverified]** in the GUI). No new flag is added to Review or Verify.

At session creation, right after its log is created (Review: `widgets/review/src/live/runner.rs:1506`, `Store::create`; Verify: `widgets/verify/src/live/runner.rs:2785`), the widget builds a `task_links::Target`:

| | Review (git targets only) | Verify (diff and state targets) |
|---|---|---|
| `toplevel` | `manifest.repo` (set from `repo_root`, `widgets/review/src/capture.rs:694`) | `Session.workdir` / `Setup.workdir`, "the top level of the target's checkout" (`widgets/verify/src/engine.rs:533`) |
| `head_ref` | `manifest.head_ref` (`Some` only for `Head::Ref`, `widgets/review/src/capture.rs:580-583`) | `manifest.head_ref` |
| `head_sha` | `manifest.head_sha` | `manifest.head_sha` (resolved before the kind split, `widgets/verify/src/capture.rs:1324`; that a state target's manifest stores it is **[unverified]**) |
| `uncommitted` | `target.head == Head::Worktree` and `git -C <toplevel> status --porcelain -z --untracked-files=normal -- <target.paths>` prints anything | `target.head == Head::Worktree` and the capture wrote a non-empty `target/worktree.patch` or copied any file to `target/untracked/` (`widgets/verify/src/capture.rs:53-55`; which value of the capture's output carries this is the unit's to find) |

`task_links::match_run(tasks_root, &target)` scans every key dir's `runs.jsonl` (and `links.jsonl` for rule c), first rule that matches, newest run first:

- (a) `worktree`: the run has a branch and its `workdir` is the target's `toplevel` (compared after `fs::canonicalize` when both exist, else as written).
- (b) `branch`: `head_ref` is the run's `branch` (`task-run-…`, unique by construction).
- (c) `commit`: `head_sha` is a commit that key dir's link log records for the run (`ChangeSeen`).

A この作業ツリー run's `workdir` is the repository itself, shared with every other session on it, so rule (a) never applies to it; it is linked only by (c).

On a match the widget appends `SessionLinked { kind, session, task, run, commit: head_sha, uncommitted, by }` to **the key dir the run was found in**. No match: nothing is written. A failure to read or write the logs never fails the session: the widget logs it and shows 「タスクとの関連を記録できません: <why>」 once.

Outcome: while it runs a linked session, the widget appends `SessionOutcome { kind, session, decision, closed }` whenever `(decision, closed)` changes from what it last wrote in this process: `decision` is `session.verdict().map(|v| v.decision.name())` (Review `widgets/review/src/model/types.rs:1371`, names `approve` / `approve_with_fixes` / `request_changes` / `reject`; Verify `widgets/verify/src/model/types.rs:2138`, names `pass` / `pass_with_issues` / `fail` / `inconclusive`), `closed` is `Session.closed`. A session resumed in a new process writes its outcome once more (harmless: the last line wins). The origin of a resumed or read-only session is found with `task_links::find_origin`.

Files and document reviews are not linked (they have no commit).

### L1-5. Version display and mapping

A review or verification maps to a version by its `commit`:

| `SessionLinked` | Shown | Counts as checking the current version |
|---|---|---|
| `commit` is version N, `uncommitted: false` | `vN` | when N is current |
| `commit` is version N, `uncommitted: true` | `vN+未コミット` | no |
| `commit` is the run's base | `着手時点` / `着手時点+未コミット` | no |
| `commit: null` | `版不明` | no |

Outcome labels (Tasks' own copy, keyed by the decision name; an unknown name is shown as written): レビュー 承認 / 修正後承認 / 要修正 / 差し戻し (Review's `Decision::label`, `widgets/review/src/model/types.rs:964`), 検証 合格 / 条件付き合格 / 不合格 / 判定不能 (Verify's, `widgets/verify/src/model/types.rs:1708`); no decision and not closed: 進行中; no decision and closed: 判定なしで終了.

「未登録」: when the task has a current version vN and no linked review (resp. verification) counts as checking it, the section shows 「レビュー vN 未登録」 (resp. 「検証 vN 未登録」). A session still running counts as registered. No version, no 未登録 row.

### L1-6. The 「関連成果」 section

In the Tasks detail pane (`widgets/tasks/src/ui/detail.rs`), after the task's buttons and before 本文. The existing 「実行 N」 section with each run's report stays as it is.

```text
関連成果
[実行]    run-1732… · claude · 終了              [ペインを開く] [worktree の場所を開く]
[変更]    v2（現在） 1a2b3c4 · task-run-1732…
          v1 9f8e7d6
[レビュー] rev-1732… · v2 · 承認                  [開く] [外す]
[検証]    ver-1732… · v1 · 合格                   [開く] [外す]
          検証 v2 未登録
[PR]      #45 https://github.com/o/n/pull/45     [開く] [外す]
[関連を追加 (A)]
```

- Each row starts with a `Tag` naming its kind: 実行 / 変更 / レビュー / 検証 / PR. Order: kinds as above; inside a kind newest first (versions: current first); a 未登録 row last in its kind.
- Rows: 実行 = run id, agent, the run's liveness word (detail.rs's `liveness`); 変更 = one line per version, `vN` (`（現在）` on the last), short SHA (7), the run's branch or `この作業ツリー`; レビュー / 検証 = session id, version text (L1-5), outcome label; PR = `#N` when the URL has one, and the URL.
- Empty: 「まだ関連する成果はありません」 and the 関連を追加 button.
- Keys: `a` opens 関連を追加 for the selected task (a key no layout uses today, `widgets/tasks/src/ui/mod.rs:669-790`). The row buttons are ordinary detail buttons (`detail::insert` returns them with their `Intent`, as today).

### L1-7. Cross links

From Tasks (new `Intent`s, L2-2):

| Row | Button | What happens |
|---|---|---|
| 実行 | ペインを開く (only while `Alive`) | existing `Intent::OpenPane` |
| 実行 | worktree の場所を開く | existing `Intent::OpenWorktree`; new: a work tree that no longer exists gives 「<dir> はもうありません（worktree は削除済み）」 instead of calling the opener (today it opens whatever path, `widgets/tasks/src/live.rs:676-682`) |
| レビュー | 開く | `Intent::OpenSession { kind: Review, session }` → `review --session=<data dir>/noa/reviews/<id>` (Review's existing read-only view of a stored review, `widgets/review/src/main.rs:28-40`) |
| 検証 | 開く | `Intent::OpenSession { kind: Verify, session }` → `verify --session=<data dir>/noa/verifications/<id>` (`widgets/verify/src/main.rs:31-44`) |
| PR | 開く | existing `Intent::OpenUrl(url)` |

The session folder is rebuilt from the id (`task_links::session_store`), never read from the log, and refused unless the id is one plain path component starting `rev-` / `ver-` and the folder holds `events.jsonl`.

From Review / Verify back to the task: a 「元のタスクを開く」 button beside ペインをすべて閉じる (Review `widgets/review/src/ui/mod.rs:1112`; Verify `widgets/verify/src/ui/board.rs:368`), shown when the session has an origin, in the live and the `--session` view. It starts Tasks with

```text
tasks --repo=<tasks repo> --select=<task id>
```

both flags existing today (`widgets/tasks/src/main.rs:25-27`). `<tasks repo>` is the key dir's last `Repo` path; `<task id>` is `t-<…>` for a local task, `#N` for an issue, and **new** `code:<key>` for a code marker (today `--select` names a marker by its path, which two markers in one file share, `widgets/tasks/src/ui/mod.rs:951-959`; Tasks' `names` learns `code:<key>`). No `Repo` path, or one that no longer exists: the button is disabled with 「タスクのリポジトリがわかりません」 / 「<path> はもうありません」.

Starting another widget uses `sibling.rs` (Reuse decision); a widget that cannot be found gives 「<Review|Verify|Tasks> が見つかりません（widgets/<crate> をビルドしてください）」.

### L1-8. Manual correction

- 関連を追加 (button or `a`): a form with one field 「PR の URL または番号」. Accepted:
  - `https://<host>/<owner>/<name>/pull/<N>` (anything after `<N>` dropped), stored as that URL;
  - `#N` or `N`, turned into `https://github.com/<owner/name>/pull/N` with the GitHub source's `owner/name` (`GitHubView.repo`); refused 「GitHub のリポジトリがわからないため番号では追加できません」 while it is unknown.
  - Anything else is refused with what is accepted; the same URL already on the task is refused 「この PR はもう関連づいています」.
  - Writes `PrLinked { id: "pr-<millis>[-n]", task, url }`.
- 外す on a PR, review or verification row writes `LinkRemoved { task, id }` (`id`: the PR's id or the session id). Runs and versions are facts of the run log and git; they have no 外す.
- Removal is final for that id (ids are never reused); a removed PR can be added again (a new id). A removed session link is not re-added by its widget (it writes `SessionLinked` only at creation) and stays reachable from Review's / Verify's own list of past sessions.
- Nothing is written to GitHub (the Tasks widget is read-only towards GitHub, `docs/specs/task-widget.md` Decision 3).

### L1-9. Many-to-many

- A task with several runs lists each under 実行; their commits share the task's version sequence (L1-3).
- One PR may be linked to several tasks: each `PrLinked` names one task, so the same URL added to t-1 and #12 is two events and shows on both; 外す on one leaves the other.
- A session is linked to one task (the matched run's). A commit shared by runs of two tasks matches the newest run (rule order in L1-4).

## L2: Detail

### L2-1. Files and owners

Each file has exactly one owning unit; only it edits the file. "new" = the unit creates it.

```text
widgets/tasks/src/task_links.rs     new  U1   the shared module (L2-2), the master copy
widgets/review/src/task_links.rs    new  U1   byte-identical copy
widgets/verify/src/task_links.rs    new  U1   byte-identical copy
widgets/tasks/src/sibling.rs        new  U1   the shared launcher (L2-2), master copy
widgets/review/src/sibling.rs       new  U1   byte-identical copy
widgets/verify/src/sibling.rs       new  U1   byte-identical copy
widgets/tasks/src/related.rs        new  U1   pub surface + todo!() bodies → U2 fills
widgets/tasks/src/lib.rs                 U1   + pub mod related, sibling, task_links
widgets/review/src/lib.rs                U1   + pub mod sibling, task_links
widgets/verify/src/lib.rs                U1   + pub mod sibling, task_links
docs/feature-map.md                      U1   + a "Task links" row
widgets/tasks/src/live.rs, live/start.rs, ui/mod.rs, ui/detail.rs, ui/forms.rs, demo.rs,
  and new tests/links_*.rs                          U2
widgets/review/src/live.rs, live/runner.rs, main.rs, ui/mod.rs, and new tests/links_*.rs   U3
widgets/verify/src/live.rs, live/runner.rs, main.rs, ui/board.rs, ui/mod.rs, and new tests/links_*.rs   U4
```

A unit that finds it needs another file stops that part and reports `#TODO(agent): <file and why>` (as the Tasks spec's rule, `docs/specs/task-widget.md`, Rules for every unit).

### L2-2. Frozen contracts (written by U1)

`task_links.rs` (identical in the three crates; no `crate::` import):

```rust
pub type Millis = u64;
pub const LINKS_FILE: &str = "links.jsonl";      // lock: links.jsonl.lock
pub const RUNS_FILE: &str = "runs.jsonl";        // Tasks' run log, read only here
pub const CHANGE_POLL_MS: Millis = 10_000;

/// <data dir>/noa/tasks (noa_config::session_state_path()'s parent + "tasks").
pub fn default_root() -> Option<PathBuf>;
/// The key dirs under `root` that hold a runs.jsonl or a links.jsonl, sorted.
pub fn key_dirs(root: &Path) -> Vec<PathBuf>;
pub fn links_log(key_dir: &Path) -> PathBuf;

/// Tasks' TaskRef as JSON ({"local":"t-1"}, {"github":{"repo":"o/n","number":12}},
/// {"code":{"path":"src/a.rs","key":"…"}}): opaque outside Tasks.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)] #[serde(transparent)]
pub struct TaskKey(pub serde_json::Value);

#[serde(rename_all = "lowercase")] pub enum SessionKind { Review, Verify }
#[serde(rename_all = "lowercase")] pub enum MatchedBy { Worktree, Branch, Commit }

#[serde(tag = "event")]
pub enum LinkEvent {
    Repo { path: PathBuf, at: Millis },
    ChangeBase { run: String, commit: String, at: Millis },
    ChangeSeen { run: String, commit: String, at: Millis },
    SessionLinked { kind: SessionKind, session: String, task: TaskKey, run: String,
                    commit: Option<String>, uncommitted: bool, by: MatchedBy, at: Millis },
    SessionOutcome { kind: SessionKind, session: String, decision: Option<String>,
                     closed: bool, at: Millis },
    PrLinked { id: String, task: TaskKey, url: String, at: Millis },
    LinkRemoved { task: TaskKey, id: String, at: Millis },
}

pub fn append(log: &Path, event: &LinkEvent) -> io::Result<()>;   // flock, torn-line repair, one write, sync_data
pub fn read(log: &Path) -> io::Result<Vec<LinkEvent>>;            // missing → empty; bad lines skipped
pub fn new_pr_id(now: Millis, events: &[LinkEvent]) -> String;    // pr-<now>, then -2, -3 …

pub struct RunRecord { pub key_dir: PathBuf, pub run: String, pub task: TaskKey,
                       pub workdir: PathBuf, pub branch: Option<String>, pub started: Millis }
pub fn read_runs(key_dir: &Path) -> Vec<RunRecord>;               // RunStarted lines only

pub struct Target { pub toplevel: PathBuf, pub head_ref: Option<String>, pub head_sha: Option<String> }
pub struct Matched { pub run: RunRecord, pub by: MatchedBy }
pub fn match_run(root: &Path, target: &Target) -> Option<Matched>;          // L1-4 rules a, b, c
pub fn link_session(m: &Matched, kind: SessionKind, session: &str, commit: Option<&str>,
                    uncommitted: bool, now: Millis) -> io::Result<()>;

pub struct Origin { pub key_dir: PathBuf, pub tasks_repo: Option<PathBuf>, pub task: TaskKey, pub run: String }
/// The SessionLinked of (kind, session) in any key dir, unless a LinkRemoved names it.
pub fn find_origin(root: &Path, kind: SessionKind, session: &str) -> Option<Origin>;
pub fn record_outcome(origin: &Origin, kind: SessionKind, session: &str,
                      decision: Option<&str>, closed: bool, now: Millis) -> io::Result<()>;

pub fn select_arg(task: &TaskKey) -> Option<String>;                 // "t-…" | "#N" | "code:<key>"
pub fn tasks_args(origin: &Origin) -> Result<Vec<OsString>, String>; // ["--repo=…", "--select=…"]
/// <tasks root>/../reviews/<id> or ../verifications/<id>; None for a bad id or no events.jsonl.
pub fn session_store(tasks_root: &Path, kind: SessionKind, session: &str) -> Option<PathBuf>;
```

The JSON lines, exactly (the golden test in `task_links.rs`, so in all three crates):

```text
{"event":"Repo","path":"/src/noa","at":1}
{"event":"ChangeBase","run":"run-5","commit":"<40 hex>","at":2}
{"event":"ChangeSeen","run":"run-5","commit":"<40 hex>","at":3}
{"event":"SessionLinked","kind":"review","session":"rev-9","task":{"github":{"repo":"o/n","number":12}},"run":"run-5","commit":"<40 hex>","uncommitted":false,"by":"worktree","at":4}
{"event":"SessionOutcome","kind":"review","session":"rev-9","decision":"approve","closed":true,"at":5}
{"event":"PrLinked","id":"pr-6","task":{"local":"t-1"},"url":"https://github.com/o/n/pull/45","at":6}
{"event":"LinkRemoved","task":{"local":"t-1"},"id":"pr-6","at":7}
```

`sibling.rs` (identical in the three crates):

```rust
#[derive(Clone, Copy)] pub struct Helper { pub bundled: &'static str, pub crate_name: &'static str, pub title: &'static str }
pub const REVIEW: Helper;   // "Noa Review", "review", "Review"   (crates/noa-app/src/helper_program.rs:55)
pub const VERIFY: Helper;   // "Noa Verify", "verify", "Verify"   (:61)
pub const TASKS: Helper;    // "Noa Tasks",  "tasks",  "Tasks"    (:67)
pub fn locate(exe: &Path, helper: Helper) -> Option<PathBuf>;
pub trait Launcher { fn launch(&mut self, helper: Helper, args: Vec<OsString>) -> Result<(), String>; }
pub struct System;  impl Launcher for System {}                     // current_exe → locate → spawn
pub struct Offline(pub &'static str);  impl Launcher for Offline {} // demo / snapshot: never starts anything
```

`related.rs` (Tasks only; U1 writes the surface, U2 the bodies):

```rust
pub struct Version { pub no: u32, pub commit: String, pub run: RunId, pub seen: Millis }
pub enum Outcome { Running, Decided(String), EndedWithout }
pub enum TargetVersion { Version { no: u32, uncommitted: bool }, Base { uncommitted: bool }, Unknown }
pub struct Checked { pub kind: SessionKind, pub session: String, pub run: RunId,
                     pub target: TargetVersion, pub outcome: Outcome, pub at: Millis }
pub struct PrLink { pub id: String, pub url: String, pub number: Option<u64>, pub at: Millis }
#[derive(Default)]
pub struct Related { pub runs: Vec<RunId>, pub versions: Vec<Version>, pub reviews: Vec<Checked>,
                     pub verifications: Vec<Checked>, pub prs: Vec<PrLink> }
impl Related {
    pub fn current(&self) -> Option<&Version>;
    pub fn registered(&self, kind: SessionKind) -> bool;   // L1-5
}
pub fn fold(runs: &[Run], events: &[LinkEvent]) -> BTreeMap<TaskRef, Related>;   // L2-3
pub fn version_text(target: &TargetVersion) -> String;                            // L1-5
pub fn outcome_label(kind: SessionKind, outcome: &Outcome) -> String;             // L1-5
pub fn parse_pr(input: &str, gh_repo: Option<&str>) -> Result<String, String>;    // L1-8, the URL
pub struct LinkScan { .. }   // links.jsonl re-read when (len, mtime) changed, at most every POLL_MS
impl LinkScan { pub fn new(log: PathBuf) -> Self; pub fn poll(&mut self, now: Millis) -> Option<Vec<LinkEvent>>; }
```

All are `Debug, Clone, PartialEq, Eq` (`Serialize, Deserialize` on the `task_links` events and keys).

### L2-3. The fold (pure, `related::fold`)

1. Removed = the set of `(task, id)` of every `LinkRemoved` in the log (ids are never reused, so the order does not matter).
2. Runs from `runs.jsonl` grouped by task, newest first.
3. Walk the link log in file order; a `TaskKey` that does not deserialize into a `TaskRef`, or a run id not in `runs.jsonl`, skips the line:
   - `ChangeBase { run, commit }`: the run's base (first one wins).
   - `ChangeSeen { run, commit }`: for the run's task, if `commit` is not the run's base and not already a version of the task, push `Version { no: len + 1, commit, run }`.
   - `SessionLinked` not in Removed, first per `(kind, session)`: if `commit` is set, not the run's base and not yet a version of `task`, push it as a version (as above); then push a `Checked` with `TargetVersion` per L1-5 and `Outcome::Running`.
   - `SessionOutcome` of a linked, not removed session: `decision` set → `Decided(name)`; else `closed` → `EndedWithout`; else `Running` (the last line wins).
   - `PrLinked` not in Removed: push a `PrLink` (`number` parsed from `/pull/<N>`).
4. `registered(kind)`: some `Checked` of that kind has `TargetVersion::Version { no: current, uncommitted: false }`.

The fold reads no clock, no git and no file, so AC-2 and AC-4 are tests over event lists.

### L2-4. Tasks: where the writes go (U2)

- `live.rs` worker: on start, `Repo` (L1-1); the change poll (L1-3) beside the existing 2 s poll (`POLL_MS`, `widgets/tasks/src/model/types.rs`), gated by `CHANGE_POLL_MS`; `LinkScan` + `fold` into `View.related: BTreeMap<TaskRef, Related>` (a new field of `View`, `widgets/tasks/src/ui/mod.rs:52`); jobs for the new intents.
- `live/start.rs`: `ChangeBase` after the work tree exists and `RunStarted` is logged. Its failure is a notice; the run goes on.
- New intents in `ui/mod.rs`: `OpenSession { kind: SessionKind, session: String }`, `AddLink { task: TaskRef, input: String }`, `RemoveLink { task: TaskRef, id: String }`. `LiveFeed::with` gains a `launcher: Box<dyn Launcher + Send>` parameter (`sibling::System` in `start`, a recording fake in tests).
- `ui/detail.rs`: `insert` gains a `related: Option<&Related>` parameter and draws L1-6. `ui/forms.rs`: `Form::add_link(task)`. `ui/mod.rs`: `a` key, `names` learns `code:<key>`. `demo.rs`: one demo task carries the L0 picture (v2 current, review 承認 on v2, verify 合格 on v1), so `--demo --select=<it> --snapshot` shows the section.

### L2-5. Review and Verify: where the writes go (U3, U4)

- `live/runner.rs`: after `Store::create`, the match and `SessionLinked` (L1-4); the origin kept with the running session (set by the match, or `find_origin` on `Resume` / `ExtendStored`); after each `append_all`, `SessionOutcome` when `(decision, closed)` changed. The tasks root is injectable (the runner's config), `task_links::default_root()` in production.
- `live.rs`: `Snapshot` gains `origin: Option<task_links::Origin>`. `main.rs`: the `--session` view finds its origin once with `find_origin`.
- UI: 「元のタスクを開く」 (L1-7) sends a new command/intent that calls `sibling::System.launch(TASKS, tasks_args(origin)?)`; its notice on failure.

## L3: Acceptance criteria

Tests are named `links_acN_…` (the Tasks crate already has `ac01_…` … `ac27_…`, so the prefix keeps `cargo test acN` from mixing the two specs). Each runs with `cargo test --locked --offline links_acN` in the crate named. `widgets/tasks` tests run on Linux and macOS; `widgets/review` and `widgets/verify` tests run on macOS only (`AGENTS.md`, Verification).

| AC | Approach and tests | Unit |
|---|---|---|
| AC-1 「関連成果」 section | `related::fold` over a fixture log (one run, two versions, a review, a verification, a PR) gives each item once; UI test (`detail.rs` tests, demo-style fixture `View`): the heading 関連成果, rows tagged 実行 / 変更 / レビュー / 検証 / PR in that order, each row's text and buttons mapped to their `Intent`s; snapshot: `tasks --demo --select=<demo task> --snapshot=F.png` writes a PNG (size only, as `tests/snapshot.rs`) | U2 |
| AC-2 persistence | U1: `append` / `read` round-trip every event of L2-2 exactly as its golden line; a torn last line and an unknown event are skipped; append holds the lock (a second appender waits) and refuses a link at `links.jsonl.lock`. U2: temp data root + temp git repo: start a run in a new worktree (fake Noa), commit twice, poll → v1, v2; a review linked; then `ClosePane` (`RunClosed`), `git worktree remove --force` and the directory gone; a new `LiveFeed::with` on the same root yields an equal `View.related` for the task | U1, U2 |
| AC-3 automatic recording | Tasks (U2): `RunStarted` is still the only run record (runs.jsonl golden unchanged); `ChangeBase` = the worktree's `HEAD`; commits on `task-run-…` appear as `ChangeSeen` once each, nothing on an unmoved head, a deleted branch adds nothing and keeps the versions; この作業ツリー run recorded only while live and once after. `match_run` (U1): rules a, b, c on fixture key dirs, rule order, newest run, a この作業ツリー run never matched by its workdir, a non-matching target gives `None`. Review (U3) and Verify (U4), runner tests on a temp repo with a fake Noa (as their existing runner tests): a session on a run's worktree → one `SessionLinked { by: worktree, commit: head_sha, uncommitted }` in that key dir; on the run's branch ref → `by: branch`; on a recorded commit → `by: commit`; uncommitted changes → `uncommitted: true`; an unrelated repository → nothing written; a verdict → `SessionOutcome { decision: "<name>" }`, once per change; an unwritable tasks root → the session is still created and the notice says why | U1, U2, U3, U4 |
| AC-4 version display | `links_ac4_current_v2_review_v2_verify_v1` (U2): runs + log giving versions c1, c2; review `SessionLinked` on c2 with outcome `approve`; verify on c1 with outcome `pass`. Asserts `current().no == 2`, `registered(Review)`, `!registered(Verify)`, and the section's texts exactly: 「v2（現在）」, the review row 「… · v2 · 承認」, the verify row 「… · v1 · 合格」, 「検証 v2 未登録」, and no 「レビュー v2 未登録」. Also: an uncommitted target shows `v2+未コミット` and leaves v2 未登録; a base target shows 着手時点 | U2 |
| AC-5 cross links | U1: `tasks_args` gives `--repo=<Repo path>` and `--select=t-1` / `#12` / `code:<key>`, `Err` without a `Repo`; `session_store` refuses `../x`, `a/b`, ids without `rev-` / `ver-`, a folder without `events.jsonl`; `locate` finds the bundled name, the shared-target name, then the dev target (temp dirs). U2 (fake `Launcher`, fake opener, fake Noa): 開く on a review → `launch(REVIEW, ["--session=<root>/../reviews/rev-…"])`, on a verification → `VERIFY`, on a PR → opener with the URL, ペインを開く → `focus(pane)`, worktree の場所を開く → opener with the workdir, and the 「もうありません」 notice once it is removed; `--select=code:<key>` selects that marker. U3 / U4: the button exists only with an origin and sends `launch(TASKS, ["--repo=…", "--select=#12"])`. **Manual, macOS GUI** (reported unconfirmed until a person does it): each button opens the right window with the task selected | U1, U2, U3, U4 |
| AC-6 manual correction | U2: `parse_pr` accepts `https://github.com/o/n/pull/45`, `https://github.com/o/n/pull/45/files` (→ …/45), `#45` and `45` with `o/n` known; refuses a number without the repo, an issue URL, `http://`, empty; `AddLink` appends `PrLinked` and the row appears; the same URL twice on a task is refused; `RemoveLink` appends `LinkRemoved` and the row (PR, or an automatic review link) is gone, also after a reload; a removed PR added again shows once | U2 |
| AC-7 many-to-many | U2: a task with two runs (two branches, three commits) lists both runs and v1…v3 across them; one PR URL added to `t-1` and to `#12` shows on both; removing it from `t-1` keeps it on `#12` | U2 |
| AC-8 `scripts/verify.sh --full` | Baseline: before its first change each unit runs `scripts/verify.sh --full` on its base commit (outside the sandbox where `noa-pty` / `noa-ipc` tests need it, `AGENTS.md`) and keeps the failing-check list from its output. Done: the same command at the unit's head fails no check that passed in the baseline. Run once more after all units are merged | all |

## Risks and falsification

- **(load-bearing)** Review / Verify target the run's worktree by its real path while `runs.jsonl` holds the path Tasks built (`worktree::path_for`, `widgets/tasks/src/worktree.rs:70`), e.g. `/tmp/…` against `/private/tmp/…` on macOS; rule (a) then never matches. Mitigation: compare canonicalized paths; rule (b) still matches a branch target. Check before U3: open Review from a run's float and compare `manifest.repo` with the run's `workdir` **[unverified]**.
- The change poll misses intermediate commits (only heads are versions) and may see a commit the person made in the repository during a この作業ツリー run. Accepted (Decision 2, 3).
- A review targeting an intermediate commit not yet seen makes that commit the newest version number although it is older in history (numbering follows the log, not git topology). Accepted for the first cut; the short SHA is shown on every row.
- Evidence and snapshots live in the target's work tree: Verify's judge reply dir is `<workdir>/.noa/verify/<id>/` (`widgets/verify/src/store.rs:96`), Review's snapshot is `<workdir>/.noa/reviews/<id>/target/` with a workdir (`widgets/review/src/store.rs:47`). Removing the task's worktree removes them; `events.jsonl` and `report.md` under the data dir stay. Whether `--session` renders well without the snapshot is **[unverified]**; moving evidence out of work trees is out of scope.
- The copies drift: guarded by the byte-identity test, which runs for review / verify only in `--full` (macOS) and for tasks on every change to it.
- A widget starting another from inside `Noa.app` relies on the Helpers layout `widgets/agent-dashboard/src/repo_browser.rs` already uses; its GUI behaviour from a bundled widget is **[unverified]** here (part of AC-5's manual check).

## Decisions taken by default

Each can be changed before implementation:

1. **Match, don't pass.** Review / Verify learn the task by matching their target against the run logs (L1-4), so a session started any way is linked and no flag is added. Alternative: Tasks starts Review / Verify with a `--task=` flag; rejected because it links only sessions started from Tasks.
2. **A version is a head commit** seen by the change poll or targeted by a session; uncommitted changes are never a version (L1-3, L1-5).
3. **この作業ツリー runs are recorded only while live**, and matched only by commit.
4. **All link events in Tasks' key dir**, written by three processes under one lock; Review's and Verify's own logs (`events.jsonl`) are not changed.
5. **Removing a link is final per id**; runs and versions cannot be removed.
6. **Copied modules, byte-identical and self-checked**, not a shared crate (Reuse decision): no `Cargo.lock` change.
7. **Only git reviews** are linked; files and document reviews are not.
8. The existing 「実行 N」 section stays; 関連成果 repeats runs as one-line rows.

## Not in this cut

Auto-detecting PRs created outside Noa (manual add covers it), PR state (open / merged), merging, pushing, any write to GitHub, moving evidence out of work trees, linking sessions by hand, and any change to Noa's IPC protocol or server.

## Implementation plan

Four units, each in its own worktree.

```text
U1 contract ──┬── U2 Tasks ──┐
              ├── U3 Review ─┼── AC-5 manual check, final verify.sh --full
              └── U4 Verify ─┘
```

### Rules for every unit

- Edit only the files L2-1 gives the unit; L2-2's `pub` items are contracts (a needed change: stop that part, `#TODO(agent): <change and why>` in the report, one follow-up commit after the units merge).
- No new dependency, no `cargo update`; cargo runs with `--locked --offline` inside the widget's directory.
- Take the AC-8 baseline before the first change.
- Done: the unit's tests (named `links_acN_…`) pass, `cargo build --locked --offline --all-targets` and `cargo test --locked --offline` pass in each crate it touched, and AC-8 holds.

### U1 contract (start now; U2, U3, U4 wait for its merge)

- Creates the six shared files and `widgets/tasks/src/related.rs` (surface, `todo!()` bodies), registers the modules in the three `lib.rs`, adds the feature-map row.
- Tests (in `task_links.rs` / `sibling.rs`, so they run in all three crates): golden lines, torn / unknown lines, lock, `new_pr_id`, `read_runs` ignoring other fields, `match_run`, `find_origin` with `LinkRemoved`, `select_arg`, `tasks_args`, `session_store`, `locate`, byte identity against `../../tasks/src/`.
- ACs: the U1 parts of AC-2, AC-3, AC-5.
- Done: `git diff --stat main` shows only L2-1's U1 files; all three crates build and test.

### U2 Tasks (after U1)

- Owns: `widgets/tasks/src/related.rs` bodies, `live.rs`, `live/start.rs`, `ui/mod.rs`, `ui/detail.rs`, `ui/forms.rs`, `demo.rs`, new `tests/links_*.rs`.
- ACs: AC-1, AC-2, AC-3 (Tasks part), AC-4, AC-5 (Tasks part), AC-6, AC-7.
- Done: also `cargo run --locked --offline -- --demo --select=<demo task> --snapshot=$TMPDIR/l.png` passes in `widgets/tasks`.

### U3 Review (after U1, parallel with U2 and U4)

- Owns: `widgets/review/src/live.rs`, `live/runner.rs`, `main.rs`, `ui/mod.rs`, new `tests/links_*.rs`.
- ACs: AC-3 and AC-5 (Review parts).

### U4 Verify (after U1, parallel with U2 and U3)

- Owns: `widgets/verify/src/live.rs`, `live/runner.rs`, `main.rs`, `ui/board.rs`, `ui/mod.rs`, new `tests/links_*.rs`.
- ACs: AC-3 and AC-5 (Verify parts).

## Open questions

- Decision 1 (match vs. a flag) and Decision 2 (versions are commits only) are the owner's to confirm.
- Review / Verify read Tasks' data dir: acceptable as a cross-widget coupling, or should the link log move to a neutral `<data dir>/noa/links/`? (Default: Tasks' key dir, so one folder holds a repository's task history.)
