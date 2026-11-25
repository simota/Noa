# Tasks widget (agent task board over local tasks and GitHub issues): Spec

| | |
|---|---|
| slug | task-widget |
| status | proposed (2026-11-25); design only, nothing built; no owner sign-off |
| owner | simota |
| as-of | 2026-11-25 (HEAD 7468cca) |
| crate | `widgets/tasks` (binary `tasks`, bundled as `Noa Tasks`) |
| request | 「Noaで管理するエージェント用のタスク（issue）ボードウィジェットを設計してほしい。ローカル内とgithub issueも統一で見れるようになっているとよい。」 |

This document is scaffolding. What outlives the build is the Decisions section and the reasons in it; once a unit lands, the code is the only copy of its shape, and this file's L2 for that unit is deleted (see Implementation plan).

## L0: Vision

- Problem: the work Noa's agents do arrives from two places that never meet. GitHub issues live in the browser; local, not-yet-public tasks live in notes, `#TODO(agent):` markers, or nowhere. Handing either to an agent is a manual copy-paste into a pane (or `+ctl spawn --task`), and nothing records which agent took which task, whether it is still running, or what it reported.
- Job to be done: in one window per repository, see every task (local and GitHub issues) in one task list, hand one to an agent in a click, see it move through 未着手 → 実行中 → 確認待ち → 完了 from facts Noa already reports, and read the agent's report beside the task.
- Success definition: from View → Tasks on a repository with GitHub issues, a person sees local tasks and open issues in the same columns, starts `claude` on issue #N, watches the card move to 実行中 while the agent's pane is alive and to 確認待ち when it finishes with its result file written, reads that result in the detail pane, and marks the local task done (or sees the issue go to 完了 once it is closed on GitHub).

## Reuse decision

As review and verify decided (`docs/specs/verify-widget.md`, Reuse decision): a separate crate outside the workspace, with its own `Cargo.lock`, depending on no other widget; no shared crate is extracted.

| Piece | Source | Adaptation |
|---|---|---|
| `live/ipc.rs` (connect, `PaneSpec`, `Placement`, labels, `noa.listPanels`, agent state) | `widgets/verify/src/live/ipc.rs` | one agent per run, no rounds |
| Board tab + stowed float per agent, recovered from an adopted pane's parent, refused on a Noa without the `boardTab` capability | verify/review after #254 (`noa_ipc::Client::spawn_board_tab`, `supports_board_tab`, `crates/noa-ipc/src/client.rs:279`, `:301`) | one board tab per repository window, not per session |
| Agent argv through the login shell, prompt in an env var | `widgets/review/src/live/fix.rs` (`argv`, `PROMPT_VAR`) and `login_shell` (`widgets/review/src/live/runner.rs:204`) | prompt points at the run's task file |
| `installed.rs` (offer only installed agents), `fsx`, `store::prepare_workdir` (`.noa/.gitignore` = `*`, `widgets/verify/src/store.rs:200`) | verify | `installed`, `fsx` as is; `prepare_workdir` makes `.noa/tasks/` where verify's makes `.noa/verify/` |
| Result file convention `<cwd>/.noa/result-<label>.md`, `NOA_RESULT` in the worker env | `+ctl` (`crates/noa-app/src/ctl.rs:113`, agent-orchestration D18) | the widget sets `NOA_RESULT` itself |
| `--repo=<focused pane cwd>` | `helper_program::launch_args` (`crates/noa-app/src/helper_program.rs:76`) | add `TASKS` to its match |
| Change-detection by mtime/length, re-read at most every 2 s | `+ctl board` insight (agent-orchestration D18) | over `.noa/tasks/*.md` |

New code: the local task file format and its parser, the `gh` source, the unified `Task` model and its column derivation, the run log, the board UI (columns, cards, detail), and the run brief.

This widget keeps tasks **in the widget**, not in Noa's server. agent-orchestration rejected server-side task objects (`docs/specs/agent-orchestration.md:495`: one caller, the script can hold the order); a widget-side board is one more caller, still not a reason to add them to the server. No protocol or server change.

## Glossary

The word *board* already means two things in Noa: `+ctl board` (the worker TUI) and the **board tab** a widget's agents float on (#254). This widget never calls itself a board in identifiers or UI; it is **Tasks** / タスク, and its screen is the **task list** (columns). "board" in this spec means only the board tab.

| Term | Meaning | Identifier |
|---|---|---|
| task (タスク) | one unit of work from any source | `Task`, ref `TaskRef` |
| source (取得元) | where a task lives: local file or GitHub issue | `Source::{Local, GitHub}` |
| local task (ローカル) | a Markdown file in `<repo>/.noa/tasks/` | `TaskRef::Local("t-<millis>")` |
| issue | a GitHub issue of the repository's `origin` | `TaskRef::GitHub { repo: "owner/name", number }` |
| run (実行) | one agent started on one task | `Run`, id `run-<millis>`, pane label `task:<run-id>` |
| column (列) | 未着手 / 実行中 / 確認待ち / 完了, **derived**, never stored | `Column` |
| result (報告) | the file the run's agent writes, `NOA_RESULT` | `Run.result` |

## L1: Requirements

### L1-1. One model, two sources

```rust
pub enum TaskRef { Local(String), GitHub { repo: String, number: u64 } }
pub enum State { Open, Closed }               // the only state a source stores
pub struct Task {
    pub r#ref: TaskRef, pub title: String, pub body: Option<String>, // GitHub body loaded on select
    pub state: State, pub labels: Vec<String>, pub updated: Millis, pub url: Option<String>,
}
pub enum Column { Todo, Running, Review, Done }  // 未着手 実行中 確認待ち 完了
fn column(task: &Task, last_run: Option<&RunView>) -> Column;   // pure
```

Column rule (pure, first match): `Closed` → 完了; last run's pane alive and agent not `finished` → 実行中; last run ended (pane gone, agent `finished`/`error`, or result written) → 確認待ち; else 未着手.

Reason: GitHub stores only open/closed. Storing a column per task would need either GitHub labels (writes to GitHub) or a local overlay that drifts from what Noa reports. Deriving it from `State` plus the run log removes both (Decision 2).

### L1-2. Local tasks

- One file per task: `<repo>/.noa/tasks/t-<millis>[-n].md`. One file each so the widget and agents never write the same file at once; the id from the clock as verify's `ver-<millis>`.
- Format: a fixed header of `key: value` lines between `---` lines, then the Markdown body. Keys: `title` (required), `state` (`open`|`closed`, default `open`), `labels` (comma-separated). Unknown keys are kept on rewrite. No YAML crate: the header is not YAML, and the parser is pure and line-based.
- Agents may create a task by writing such a file (documented in the run brief and in the `noa-tasks` skill, `skills/noa-tasks/SKILL.md`, which is the agent-facing copy of this format and of L1-3/L1-4's rules; change both together); the widget picks it up within 2 s. A file that does not parse shows as a card with its error, never dropped.
- The widget writes a local file only for: 新規作成, editing title/body/labels, 完了にする / 再開 (`state`). It writes via temp + rename.
- `<repo>/.noa/` is gitignored (`*`) by `prepare_workdir`, so local tasks are this checkout's only (Decision 1).

### L1-3. GitHub issues

- Repository: `gh repo view --json nameWithOwner` in `--repo`. Not a GitHub repo / `gh` missing / not authenticated → the GitHub source shows that reason in its header; local tasks work unchanged.
- List: `gh issue list --state open --limit 200 --json number,title,state,labels,updatedAt,closedAt,url` plus `--state closed --limit 30 --search sort:updated-desc` (recently closed, so a closed issue reaches 完了 rather than vanishing; unsorted, gh lists newest-created first, which drops an old issue closed today). 完了's 7 days count from `closedAt` (a local task: its mtime). Body and comments on select: `gh issue view N --json body,comments`.
- `gh` runs through the login shell (`$SHELL -l -i -c`, as `widgets/verify/src/installed.rs` asks it which agents are installed), because a widget started from `Noa.app` inherits a GUI PATH without `/opt/homebrew/bin`.
- Refresh: on open, every 5 min, and `r` (noa-gpui 0.2.1 gives a `Screen` no window-focus event, so there is no refresh on focus). The last listing is cached in the data dir so the window opens with it at once (marked 「n 分前の取得」) and offline.
- **Read-only.** The widget never writes to GitHub (Decision 3). 「GitHub で開く」 opens the URL; an issue reaches 完了 when it is closed on GitHub (by the person, or a merged PR with `Fixes #N`, which the brief suggests).

### L1-4. Runs (handing a task to an agent)

- 実行 on a card: choose agent (installed only: claude / codex / agy) and isolation (この作業ツリー / 新しい worktree `task-<run-id>` on a new branch, default worktree). The widget writes `<workdir>/.noa/tasks/runs/<run-id>/task.md` (title, body, source URL, `Fixes #N` hint for an issue, where to report), and starts the agent as a float on the repository's board tab, label `task:<run-id>`, env `NOA_RESULT=<workdir>/.noa/tasks/runs/<run-id>/result.md`, prompt `Read <task.md> and do the task it describes. Write your report to $NOA_RESULT.`
- Liveness and state come from `noa.listPanels` by label (as verify's ペインをすべて閉じる finds its panes): alive, agent state, exited. The result file is `lstat`'ed at most every 2 s.
- One live run per task: 実行 on a 実行中 card is refused (it offers ペインを開く instead). A new run on a 確認待ち card is a retry and is allowed.
- Actions on a run: ペインを開く (focus the float), 閉じる (close its pane), worktree の場所を開く. Nothing auto-merges, nothing auto-closes a task: 確認待ち → 完了 is the person's call (Decision 4).
- Runs are recorded in `<data dir>/noa/tasks/<repo-key>/runs.jsonl` (append-only: `RunStarted{run, task, agent, workdir, branch?, pane}`, `RunPaneAssigned`, `RunClosed`); a window reopened later re-attaches to live panes by label.

### L1-5. UI (Japanese labels)

- Four columns (未着手 / 実行中 / 確認待ち / 完了), each card: source badge (ローカル / `#N`), title, labels, the last run's agent and age. 完了 shows the last 7 days only.
- Filter line: source (すべて / ローカル / GitHub), label, text. Keys: `h/l` column, `j/k` card, `Enter` detail, `n` 新規, `x` 実行, `d` 完了にする (local only), `r` 再取得, `/` filter, `o` GitHub で開く.
- Detail pane: body (issue body + comments for GitHub), runs list with each run's result (rendered as text, capped as `+ctl board` caps it: 16 KiB / 200 lines).
- No drag-and-drop between columns: columns are derived (L1-1), so there is nothing to drop onto.
- Code markers (added 2026-11-25, the owner's choice of a read-only source over registering them as local tasks): a third source, コード, of the `TODO:` / `FIXME:` / `#TODO(agent):` markers (a word of their own followed by `:` or `(…):`, not in backticks) in the repository's tracked text files, read with `git grep` (`widgets/tasks/src/code.rs`) on open, every minute and on 再取得, leaving vendored directories (`vendor/`, `third_party/`, `third-party/`, `node_modules/`) out, 500 at most. A marker is `TaskRef::Code { path, key }`, `key` a digest of its whole line with the line just above it (`git grep -B1`), so its runs follow it while its line moves and two identical markers stay apart when one is removed (editing the line or the one above makes it another marker); it is always open, is never written, and leaves the list once removed from the code. Its brief names the file and says to remove the marker only when the work is done.
- Layouts (added 2026-11-25, the owner's choice of dates that happened over due dates): カンバン (the columns), リスト (a row per task the filter lets through, old 完了 included), タイムライン (the last 14 days; a task's runs as bars from start to end — RunClosed, else the report's write time, else now while live — and a dot at its close) and カレンダー (a month from Sunday; runs started and tasks closed each day, three and 「+N 件」). Switched by the filter line's control, `v`, or `--layout=`; `j`/`k` walk the rows, `[`/`]` the calendar's months.
- Flags: `tasks [--demo | --repo=DIR] [--snapshot=FILE.png [--size=WxH]]`, as the other widgets.

### L1-6. Noa integration

As verify L1-10 lists for its own: `AppCommand::OpenTasks`, `OPEN_TASKS_MENU_ID = "noa.view.open-tasks"`, action `tasks.open`, palette "Open Tasks", View → "Tasks", `helper_program::TASKS` (`Noa Tasks`, crate `tasks`) added to `launch_args`'s `--repo` match, its test, `scripts/verify.sh`, `scripts/bundle-macos.sh`, `AGENTS.md`, `docs/FEATURES.md`.

## L2: Detail

### L2-1. Crate layout

U1 creates every file below; after U1 merges each file has exactly one owner, and only the owner edits it. "frozen" = U1's, never edited again. "→ U2/U3" = U1 writes the `pub` surface with `todo!()` bodies (plus the bodies L2-2 marks *full*), that unit fills it.

```text
widgets/tasks/
  Cargo.toml  Cargo.lock  .gitignore   frozen   .gitignore = /target, *.png
  src/lib.rs                           frozen   pub mod fsx, github, local, model, runs, shell, store, worktree; #[cfg(test)] pub(crate) mod testkit
  src/model/mod.rs                     frozen   pub mod types; pub mod column;
  src/model/types.rs                   frozen   every shared type of L2-2
  src/model/column.rs                  frozen   column(), Filter, matches(): full, with their tests
  src/fsx.rs                           frozen   widgets/verify/src/fsx.rs verbatim, with its tests
  src/shell.rs                         frozen   login_shell() from widgets/verify/src/live/runner.rs:253; argv()/PROMPT_VAR from widgets/review/src/live/fix.rs:32,47
  src/testkit.rs                       frozen   temp dirs (std::env::temp_dir, no tempfile), Task builders
  src/store.rs                         → U2     paths full (U1); prepare_workdir and its helpers from widgets/verify/src/store.rs:200 on (U2)
  src/local.rs                         → U2     local task files: parse, render, scan, create, write
  src/runs.rs                          → U2     run log, brief, run view (path helpers full, U1)
  src/worktree.rs                      → U2     toplevel, main worktree, add (naming full, U1)
  src/github.rs                        → U3     the GitHub source; U3 may add src/github/*.rs (only U3)
  src/main.rs                          frozen   flags (--demo / --repo, default the current directory / --snapshot / --size / --select=ID: that card's detail open), title, Headless render
  src/demo.rs                          frozen   DemoFeed: a canned View with cards in every column (local `t-1`…, issues `#12`…), a broken local file, runs with reports
  src/installed.rs                     frozen   widgets/verify/src/installed.rs, imports changed to tasks::model::types::AgentKind and tasks::shell::login_shell
  src/ui/mod.rs                        frozen   Feed, View, Intent, the Tasks screen: layout, keys, filter line, GitHub header
  src/ui/columns.rs  src/ui/style.rs   frozen   four columns and cards
  src/ui/tests.rs                      frozen   U1's UI tests
  src/ui/detail.rs                     → U2     detail pane
  src/ui/forms.rs                      → U2     新規 / 編集 / 実行 forms
  src/live.rs                          → U2     LiveFeed; declares `pub mod ipc; pub mod start;`
  src/live/ipc.rs                      frozen   widgets/verify/src/live/ipc.rs trimmed to L2-2's trait
  src/live/start.rs                    → U2     starting, focusing, closing a run against the Noa trait
  tests/snapshot.rs                    frozen   as widgets/agent-usage/tests/snapshot.rs, temp dir from std
```

- `Cargo.toml` is `widgets/verify/Cargo.toml` with `name = "tasks"` and its own `description`; same `[dependencies]` and macOS dependencies, nothing added. `Cargo.lock` is `widgets/verify/Cargo.lock` with the root package line `name = "verify"` changed to `name = "tasks"`, then one `cargo tree --offline >/dev/null` in `widgets/tasks`, which only moves that entry to its sorted place (checked 2026-11-25 on a copy: `--locked` accepts the renamed copy either way, nothing else changes; `cargo metadata --offline`, which verify L2-1 used, fails on this machine because `redox_users` is not in the offline cache).
- The UI is source-agnostic: it renders `View` (L2-2) and sends `Intent`s to a `Feed`. U3 fills only `github`; no UI file changes for it.
- A unit's own tests live in its own files (an in-file `#[cfg(test)] mod tests`, or a new `tests/<unit>_*.rs`), never in `src/ui/tests.rs` or `src/testkit.rs`.
- UI tests assert widget labels, ids and order in the tree (as `widgets/agent-usage/src/ui/tests.rs`), never a position or text width: every test of this crate runs on Linux too (L2-3).
- UI stubs that the demo reaches (`detail::insert`, `forms::Form`) are no-ops in U1, not `todo!()`, so `--demo` and `--snapshot` work before U2.

### L2-2. Frozen contracts (written by U1)

Signatures only; they supersede L1-1's type block (adding `Task.problem`) and L1-4's event fields (`RunStarted` names the pane's label; its id comes with `RunPaneAssigned`). Serde derives on every type that is stored or cached. *full* = U1 writes the body.

```rust
// src/model/types.rs
pub type Millis = u64;
pub enum TaskRef { Local(String), GitHub { repo: String, number: u64 } }  // Ord, Hash
pub enum Source { Local, GitHub }
pub enum State { Open, Closed }
pub struct Task {
    pub r#ref: TaskRef, pub title: String,
    pub body: Option<String>,        // None for an issue until its detail loads
    pub state: State, pub labels: Vec<String>, pub updated: Millis, pub url: Option<String>,
    pub problem: Option<String>,     // a local file that does not parse: shown on its card
}
impl TaskRef { pub fn source(&self) -> Source; pub fn badge(&self) -> String }  // full: "ローカル" / "#N"
pub enum Column { Todo, Running, Review, Done }
impl Column { pub const ALL: [Column; 4]; pub fn label(self) -> &'static str }   // full: 未着手 実行中 確認待ち 完了
pub enum AgentKind { Claude, Codex, Agy }   // verify's: ALL, command(); no model/effort setup
pub enum Isolation { Worktree, Here }       // 新しい worktree (default) / この作業ツリー
pub struct RunId(String);                   // "run-<millis>[-n]", verify's string_id!
pub enum AgentState { Running, Permission, Input, Finished, Error }
#[serde(tag = "event")]
pub enum RunEvent {                          // one line of runs.jsonl
    RunStarted { run: RunId, task: TaskRef, agent: AgentKind, workdir: PathBuf,
                 branch: Option<String>, label: String, at: Millis },   // before the spawn
    RunPaneAssigned { run: RunId, pane: u64, board: u64, at: Millis },  // after it
    RunClosed { run: RunId, at: Millis },
}
pub struct Run { pub run: RunId, pub task: TaskRef, pub agent: AgentKind, pub workdir: PathBuf,
                 pub branch: Option<String>, pub pane: Option<u64>, pub board: Option<u64>,
                 pub started: Millis, pub closed: Option<Millis> }
pub struct PaneFact { pub pane_id: u64, pub parent: Option<u64>, pub label: Option<String>,
                      pub agent: Option<AgentState>, pub exited: bool }
pub enum Liveness { Starting, Alive { agent: Option<AgentState> }, Gone }
pub enum ResultFact { Missing, Written { bytes: u64, modified: Millis } }
pub struct Report { pub text: String, pub cut: bool }   // capped 16 KiB / 200 lines
pub struct RunView { pub run: Run, pub liveness: Liveness, pub result: ResultFact, pub report: Option<Report> }
pub enum GhFailure { NoGh, NotAuthenticated, NotGitHub, Timeout, Failed(String) }
impl GhFailure { pub fn reason(&self) -> String }   // full: the source header's Japanese line
pub struct Comment { pub author: String, pub body: String, pub at: Millis }
pub struct IssueDetail { pub body: String, pub comments: Vec<Comment> }
pub struct GitHubView {
    pub repo: Option<String>,              // owner/name once known
    pub tasks: Vec<Task>,                  // the last listing, from the cache until a fetch
    pub fetched: Option<Millis>,           // for 「n 分前の取得」
    pub refreshing: bool,
    pub failure: Option<GhFailure>,        // the last refresh's; tasks stay
    pub details: BTreeMap<u64, Result<IssueDetail, GhFailure>>,
}
pub const START_TIMEOUT_MS: Millis = 90_000;   // a run without a pane after this is Gone
pub const POLL_MS: Millis = 2_000;             // .noa/tasks scan, listPanels, result lstat
pub const DONE_DAYS: u64 = 7;

// src/model/column.rs (full)
/// L1-1, first match: Closed → Done; Starting or Alive{agent ∉ {Finished, Error}} → Running;
/// any other last run → Review; no run → Todo.
pub fn column(task: &Task, last_run: Option<&RunView>) -> Column;
pub struct Filter { pub source: Option<Source>, pub label: Option<String>, pub text: String }
pub fn matches(task: &Task, filter: &Filter) -> bool;   // text: title, id or #N, case-insensitive

// src/shell.rs (full)
pub fn login_shell() -> String;
pub const PROMPT_VAR: &str = "NOA_TASKS_PROMPT";
pub fn argv(kind: AgentKind) -> Vec<String>;   // [shell, -l, -i, -c, exec <agent> [-i] "$NOA_TASKS_PROMPT"]

// src/store.rs
pub fn default_root() -> Option<PathBuf>;           // full: <data dir>/noa/tasks (noa_config::session_state_path's parent)
pub fn repo_key(repo: &Path) -> String;             // full: <dir name>-<FNV-1a 64 hex of the path>
pub fn runs_log(root: &Path, key: &str) -> PathBuf; // full: <root>/<key>/runs.jsonl
pub fn gh_cache(root: &Path, key: &str) -> PathBuf; // full: <root>/<key>/github.json
pub fn prepare_workdir(workdir: &Path) -> io::Result<()>;   // U2

// src/local.rs (U2; TASKS_DIR and tasks_dir full)
pub const TASKS_DIR: &str = ".noa/tasks";
pub fn tasks_dir(repo: &Path) -> PathBuf;
pub struct LocalFile { pub title: String, pub state: State, pub labels: Vec<String>,
                       pub extra: Vec<(String, String)>, pub body: String }
pub fn parse(text: &str) -> Result<LocalFile, String>;
pub fn render(file: &LocalFile) -> String;          // parse(render(f)) == f; extra keys kept in order
pub fn task(id: &str, parsed: Result<LocalFile, String>, updated: Millis) -> Task;  // Err → card with problem
pub fn new_id(dir: &Path, now: Millis) -> String;   // t-<millis>, then -2, -3 …
pub fn create(repo: &Path, file: &LocalFile, now: Millis) -> io::Result<String>;
pub fn write(repo: &Path, id: &str, file: &LocalFile) -> io::Result<()>;      // fsx::write_replace
pub fn set_state(repo: &Path, id: &str, state: State) -> io::Result<()>;
pub struct Scan { .. }
impl Scan {
    pub fn new(repo: PathBuf) -> Scan;
    /// Top-level t-*.md only; Some when a file's (len, mtime) or the set changed, at most every POLL_MS.
    pub fn poll(&mut self, now: Millis) -> Option<Vec<Task>>;
}

// src/runs.rs (U2; label and the path helpers full)
pub fn label(run: &RunId) -> String;                        // "task:<run-id>"
pub fn run_dir(workdir: &Path, run: &RunId) -> PathBuf;     // <workdir>/.noa/tasks/runs/<run-id>
pub fn task_file(workdir: &Path, run: &RunId) -> PathBuf;   // …/task.md
pub fn result_file(workdir: &Path, run: &RunId) -> PathBuf; // …/result.md
pub fn new_id(now: Millis, taken: &[RunId]) -> RunId;
pub fn append(log: &Path, event: &RunEvent) -> io::Result<()>;
pub fn replay(log: &Path) -> io::Result<Vec<Run>>;          // a torn last line is skipped
pub fn fold(events: &[RunEvent]) -> Vec<Run>;
pub fn brief(task: &Task, workdir: &Path, result: &Path) -> String;  // task.md
pub fn prompt(task_file: &Path) -> String;                  // L1-4's line
pub fn result_fact(path: &Path) -> ResultFact;              // lstat; a link counts as Missing
pub fn read_report(path: &Path) -> Option<Report>;
pub fn view(run: &Run, panes: &[PaneFact], result: ResultFact, report: Option<Report>, now: Millis) -> RunView;
pub fn may_start(task: &Task, last: Option<&RunView>) -> Result<(), String>;  // refused while Running

// src/worktree.rs (U2; path_for and branch_for full)
pub fn toplevel(dir: &Path) -> Result<PathBuf, String>;
pub fn main_worktree(repo: &Path) -> Result<PathBuf, String>;
pub fn path_for(main: &Path, run: &RunId) -> PathBuf;   // <main>-wt/task-<run-id>
pub fn branch_for(run: &RunId) -> String;               // task-<run-id>
pub fn add(repo: &Path, run: &RunId) -> Result<(PathBuf, String), String>;  // git worktree add -b <branch> <path> HEAD

// src/github.rs (U3; constants full)
pub const OPEN_LIMIT: u32 = 200; pub const CLOSED_LIMIT: u32 = 30;
pub const REFRESH_MS: Millis = 300_000; pub const GH_TIMEOUT: Duration = Duration::from_secs(20);
pub trait IssueFeed: Send {
    fn view(&self) -> GitHubView;         // never blocks
    fn refresh(&mut self);                // `r`; the feed also refreshes on start and every REFRESH_MS
    fn load_detail(&mut self, number: u64);
}
/// Runs `gh <args>` in `cwd`; the real one goes through shell::login_shell().
pub trait GhRun: Send + 'static { fn run(&mut self, cwd: &Path, args: &[String]) -> Result<String, GhFailure>; }
pub struct LoginShellGh;  impl GhRun for LoginShellGh {}
pub struct GhFeed { .. }  impl IssueFeed for GhFeed {}
impl GhFeed {
    pub fn start(repo: PathBuf, cache: PathBuf, wake: Arc<dyn Fn() + Send + Sync>) -> GhFeed;  // LoginShellGh, system clock
    pub fn with(repo: PathBuf, cache: PathBuf, gh: Box<dyn GhRun>,
                clock: Arc<dyn Fn() -> Millis + Send + Sync>, wake: Arc<dyn Fn() + Send + Sync>) -> GhFeed;
}
pub fn classify(spawned: bool, status: Option<i32>, stderr: &str) -> GhFailure;
pub fn parse_repo(json: &str) -> Result<String, String>;
pub fn parse_list(json: &str, repo: &str) -> Result<Vec<Task>, String>;
pub fn parse_view(json: &str) -> Result<IssueDetail, String>;

// src/live/ipc.rs (frozen): verify's NoaError, PaneView, PaneSpec, Connect, IpcConnect, IpcNoa,
// no_board_tab as they are; Placement without NewTab; Noa trimmed to the methods below
// (whatever only the dropped ones use goes too):
pub enum Placement { Board, Float(u64) }
pub trait Noa {
    fn panes(&mut self) -> Result<Vec<PaneView>, NoaError>;
    fn spawn(&mut self, placement: Placement, spec: &PaneSpec) -> Result<u64, NoaError>;
    fn focus(&mut self, pane: u64) -> Result<(), NoaError>;
    fn close_pane(&mut self, pane: u64) -> Result<(), NoaError>;
    fn server_version(&self) -> String;
    fn supports_board_tab(&self) -> bool;
}
pub trait Connect { type Conn: Noa; fn connect(&mut self) -> Result<Self::Conn, String>; }
pub fn pane_fact(pane: &PaneView) -> PaneFact;   // full

// src/live/start.rs (U2)
pub struct RunRequest { pub task: Task, pub repo: PathBuf, pub agent: AgentKind, pub isolation: Isolation }
/// Workdir (worktree::add or repo), prepare_workdir, task.md, RunStarted, the board tab if
/// `board` is None (cwd repo), the float (label, shell::argv, env NOA_RESULT + PROMPT_VAR),
/// RunPaneAssigned. The float is `attended: true` (keyboard focus), as review's fix request
/// (Decision 7). Auto approval is the tab's and a float takes its board tab's, so the board
/// spec is `attended: true` too: ipc.rs then sends the board `autoApprove: false` (still no focus).
/// Refused, nothing written, without boardTab.
pub fn start<N: Noa>(noa: &mut N, board: &mut Option<u64>, log: &Path, req: &RunRequest, now: Millis) -> Result<RunId, String>;
pub fn board_of(panes: &[PaneView], runs: &[Run]) -> Option<u64>;   // a live run pane's parent

// src/ui/mod.rs (frozen)
pub struct View { pub repo: PathBuf, pub tasks: Vec<Task>, pub runs: Vec<RunView>,
                  pub github: GitHubView, pub agents: Option<Vec<AgentKind>>,
                  pub notice: Option<String>, pub now: Millis }
pub enum Intent {
    Create { title: String, body: String, labels: Vec<String> },
    Edit { id: String, title: String, body: String, labels: Vec<String> },  // other keys kept
    SetState { id: String, state: State },
    Run { task: TaskRef, agent: AgentKind, isolation: Isolation },
    OpenPane(RunId), ClosePane(RunId), OpenWorktree(RunId), OpenUrl(String),
    Refresh, LoadDetail(u64),
}
pub trait Feed {
    fn poll(&mut self) -> Option<View>;   // a new View when anything changed; never blocks
    fn act(&mut self, intent: Intent);    // outcomes come back as View.notice
}
// demo.rs: pub struct DemoFeed; impl Feed.  live.rs (U2):
impl LiveFeed<IpcConnect> { pub fn start(repo: PathBuf, wake: Arc<dyn Fn() + Send + Sync>) -> Result<Self, String>; }
impl<C: Connect + Send + 'static> LiveFeed<C> {
    /// `opener` opens a URL or a directory (`open` / `xdg-open` in `start`).
    pub fn with(repo: PathBuf, root: PathBuf, issues: Box<dyn IssueFeed>, connect: C,
                opener: Box<dyn FnMut(&str) + Send>, clock: Arc<dyn Fn() -> Millis + Send + Sync>,
                wake: Arc<dyn Fn() + Send + Sync>) -> Self;
}
impl<C: Connect + Send + 'static> Feed for LiveFeed<C> {}   // IPC, gh and file I/O off the UI thread

// src/ui/detail.rs (U2): the buttons it made, each with what a click does.
pub(super) fn insert(tree: &mut UiTree, host: WidgetId, task: &Task, runs: &[&RunView],
                     detail: Option<&Result<IssueDetail, GhFailure>>, now: Millis) -> Vec<(WidgetId, Intent)>;
// src/ui/forms.rs (U2)
pub(super) enum FormOutcome { Pending, Cancel, Submit(Intent) }
pub(super) struct Form { .. }
impl Form {
    pub fn new_task() -> Form; pub fn edit(id: &str, task: &Task) -> Form;
    pub fn run(task: &Task, agents: &[AgentKind]) -> Form;   // default Isolation::Worktree
    pub fn insert(&self, tree: &mut UiTree, host: WidgetId);
    pub fn handle(&mut self, message: &Message, tree: &UiTree) -> FormOutcome;
}
```

### L2-3. Tests on every platform

`widgets/tasks` runs its tests on Linux and macOS, as `widgets/agent-usage` does (`usage_check` in `scripts/verify.sh`). Tests use temp dirs, real `git` and `sh`, fakes for Noa (`Connect`), `gh` (`GhRun`), the clock and the opener; none needs a display, a network, `gh` or a running Noa. `--snapshot` tests assert the PNG's size only (`Headless::new_or_software`).

## L3: Acceptance Criteria

Each test is named `acNN_…` and runs with `cargo test --locked --offline acNN` in `widgets/tasks` unless stated. Manual ones need a person.

| AC | Check | Unit |
|---|---|---|
| AC-1 | `column()` table over State × Liveness × ResultFact × no run: L1-1 first match, Starting → 実行中, a result written while the agent still runs → 実行中 | U1 |
| AC-2 | `matches()`: source, label, text over title / `t-…` / `#N`, case-insensitive | U1 |
| AC-3 | Serde: each `RunEvent` variant serializes to a fixed JSON line (`{"event":"RunStarted",…}`) and back; `Task` round-trips | U1 |
| AC-4 | UI on the demo View: four columns labelled 未着手 実行中 確認待ち 完了 in order; each card in the column `column()` gives, badge ローカル / `#N`; 完了 hides tasks closed more than 7 days ago; a broken local file shows its problem | U1 |
| AC-5 | Keys on a recording fake Feed: `h/l/j/k` move the selection, `/` focuses the filter and filters the columns, `r` sends `Refresh`, `o` sends `OpenUrl` for an issue and nothing for a local task, `d` sends `SetState { Closed }` for a local task and nothing for an issue, `Enter` on an issue sends `LoadDetail` | U1 |
| AC-6 | GitHub header: `owner/name · n 分前の取得` from `fetched`/`now`, 取得中 while refreshing, each `GhFailure::reason()`; local cards unchanged by a failure | U1 |
| AC-7 | `tests/snapshot.rs`: `tasks --demo --snapshot=F.png` (also `--size=1200x800`, `--select=#12`) writes a PNG of twice the size and writes nothing under a temp `HOME` / `XDG_DATA_HOME`; `--snapshot` without `--demo` is refused | U1 |
| AC-8 | `local::parse`/`render`: `title` required, `state` default open and bad value refused, labels split and trimmed, unknown keys kept in order, body kept byte for byte, missing `---` refused; `parse(render(f)) == f` | U2 |
| AC-9 | The example task file in `skills/noa-tasks/SKILL.md` (its first `text` block, read from `../../skills/`) parses | U2 |
| AC-10 | Local files: `create` names `t-<millis>` then `-2`; writes by temp + rename and never through a link at the name; `set_state` changes only `state`; a file that does not parse is a card, never dropped; `runs/` and non-`t-*.md` files are ignored | U2 |
| AC-11 | `Scan::poll` with an injected clock: `None` when nothing changed or within 2 s; `Some` after a file is added, edited (same length, new mtime) or removed | U2 |
| AC-12 | Run log: `append` + `replay` fold the three events; a torn last line is skipped | U2 |
| AC-13 | `runs::view`: Starting until 90 s without a pane, then Gone; Alive with the agent state of the pane labelled `task:<run-id>`; Gone when that pane is absent or exited or the run has `RunClosed`; report capped at 16 KiB / 200 lines with `cut` | U2 |
| AC-14 | `runs::brief` for a local task and an issue: title, body, URL, `Fixes #N` hint only for the issue, workdir, result path; `prompt` is L1-4's line exactly | U2 |
| AC-15 | `start::start` on a fake Noa and a temp repo: Worktree isolation makes `<main>-wt/task-<run-id>` on branch `task-<run-id>`; Here uses the repo; one board tab for two runs; the float has the label, `shell::argv(agent)`, `NOA_RESULT` and `NOA_TASKS_PROMPT`, cwd the workdir, and `attended: true`, as does the board tab's spec; `RunStarted` before the spawn, `RunPaneAssigned` after; a Noa without `boardTab` is refused with nothing written | U2 |
| AC-16 | `may_start`: refused on a 実行中 card, allowed on 確認待ち (retry) | U2 |
| AC-17 | Re-attach: a `LiveFeed::with` over an existing `runs.jsonl` and a fake Noa holding a live labelled float finds the board from its parent; the next run floats on it | U2 |
| AC-18 | Run actions on a fake Noa / opener: ペインを開く → `focus`, 閉じる → `close_pane` + `RunClosed`, worktree の場所を開く → opener with the workdir, GitHub で開く → opener with the URL; none changes a task's state | U2 |
| AC-19 | Detail pane (UI test, demo View): body, issue comments, runs newest first with agent, age and report text; its buttons map to the matching `Intent`s | U2 |
| AC-20 | Forms (UI test): 新規 refuses an empty title and submits `Create`; 編集 submits `Edit`; 実行 lists only `agents`, defaults to 新しい worktree, Escape cancels | U2 |
| AC-21 | End to end on `LiveFeed::with` (temp repo, fake Noa, fake `IssueFeed`, fake clock): create a task → 未着手; run it → 実行中; the agent reports `finished` and the result file is written → 確認待ち with the report; `d` → file `state: closed` → 完了 | U2 |
| AC-22 | `prepare_workdir` on temp repos: writes `.noa/.gitignore` = `*` when missing and keeps an existing one; when git does not ignore `.noa/tasks/` (a tracked `.noa/.gitignore`) it appends the ignore line, and refuses with the reason if git still does not | U2 |
| AC-23 | `parse_repo` / `parse_list` / `parse_view` on fixtures: `TaskRef::GitHub { repo, number }`, label names, `updatedAt` as Millis, state, URL; open and closed lists merged; comments with author and time; bad JSON refused | U3 |
| AC-24 | `classify`: shell cannot run `gh` → NoGh; `gh auth login` in stderr → NotAuthenticated; not a git repo / no GitHub remote → NotGitHub; other → Failed with the stderr tail; past `GH_TIMEOUT` the child is killed → Timeout | U3 |
| AC-25 | `GhFeed::with` on a fake `GhRun` and clock: `view()` is the cache before the first fetch returns; fetches on start, again only after 5 min or `refresh()`; a failure keeps the tasks and sets `failure`; a success writes the cache by temp + rename and clears it; `load_detail` fetches a number once | U3 |
| AC-26 | Read-only: every argv the feed passes to `GhRun` starts with `repo view`, `issue list` or `issue view` | U3 |
| AC-27 | `LoginShellGh` with `SHELL=/bin/sh` and a fake `gh` script first on `PATH`: the argv reaches it word for word (a title-like argument with quotes and `$()` is not interpreted) | U3 |
| AC-28 | `cargo test -p noa-app --locked --offline ac28`: action `tasks.open`, menu id `noa.view.open-tasks`, palette "Open Tasks" resolve to `OpenTasks`; `launch_args(TASKS, Some(abs))` is `--repo=abs`, a relative cwd gives nothing; `TASKS` is found by its binary name | U4 |
| AC-29 | `scripts/verify.sh` with a change under `widgets/tasks/` runs a `tasks` check that builds and tests the widget on Linux; `scripts/verify.sh --full` runs it on Linux and macOS (CI) | U4 |
| AC-30 | macOS: `scripts/bundle-macos.sh` puts a signed `Contents/Helpers/Noa Tasks` in `Noa.app` | U4 |
| AC-31 | `AGENTS.md` lists `widgets/tasks` among the widgets whose tests run on Linux too; `docs/FEATURES.md` has a Tasks entry (`grep -n 'widgets/tasks' AGENTS.md`, `grep -n Tasks docs/FEATURES.md`) | U4 |
| AC-32 | **Manual, macOS GUI.** L0's success definition: View → Tasks on this repository shows local tasks and open issues together; `claude` on issue #N floats on the board tab labelled `task:<run-id>`, the card goes 実行中 → 確認待ち with the report shown; 完了にする on a local task | U4 (after U1–U3) |
| AC-33 | **Manual, macOS GUI.** Close and reopen the window during a run: the card is still 実行中 and its float stays on the same board tab; 閉じる closes the float | U4 (after U1–U3) |
| AC-34 | **Manual, real `gh`.** `tasks --repo=<this repo>` lists open issues within 3 s and recently closed ones in 完了; with `gh auth logout` the header gives the reason and local tasks work; offline it opens with the cached listing and its age | U3 |
| AC-35 | **Manual, macOS.** Falsification's load-bearing check: from `Noa.app`, the widget finds `gh` through the login shell | U3 (stays open until a person runs it; does not gate U3) |

## Scope

In scope: L1-1 … L1-6.

Excluded:
- Writing to GitHub: creating, editing, labelling, commenting on or closing issues; Projects (v2) columns; pull requests as tasks.
- Sharing local tasks through git (they live under the gitignored `.noa/`), and syncing a local task to an issue (「issue にする」).
- More than one repository per window; GitLab / Jira / other trackers.
- Dependencies between tasks, priorities, due dates, assignees beyond "which agent ran it".
- Auto-dispatch (picking up 未着手 tasks without the person), auto-merge, auto-close.
- Any change to Noa's IPC protocol or server.

## Decisions taken by default

Each can be changed before implementation:

1. **Local = per-checkout, gitignored** (`<repo>/.noa/tasks/`). Readable by a sandboxed agent (same reason as verify Decision 11), writable by agents, never committed. The alternative, a tracked directory (`docs/tasks/`), shares tasks through git but puts every agent's notes in history. This is the main assumption about 「ローカル内」.
2. **Columns are derived, not stored.** Only open/closed is stored, which GitHub also has; everything else comes from the run log and Noa's pane state.
3. **GitHub is read-only.** Every write to GitHub is outward-facing; v1 makes none.
4. **The person moves a task to 完了**, never the widget: an agent saying it finished is not the task being done.
5. **Default isolation: a new worktree per run**, so two runs never share a work tree.
6. Name **Tasks / タスク**, never "board" (Glossary).
7. **Runs are attended** (no auto approval): a run's agent edits a work tree, as review's fix request does (`widgets/review/src/live/fix.rs` spawns it `attended: true`). The person answers the agent's prompts in its float.

## Needs the maintainer's agreement before implementation

- `AGENTS.md` Safety: a new `widgets/tasks/Cargo.lock`, and changes to `scripts/verify.sh` and `AGENTS.md`.
- Decision 1 (where local tasks live).

## Falsification

This design is wrong if any of these holds; each is cheap to check before U2:

- **(load-bearing)** `gh` cannot be reached from a widget started by `Noa.app` on macOS even through the login shell, or `gh issue list --json` is too slow (> 3 s on 200 issues) to run on open. Check: from `Noa.app` (not a terminal) start the demo binary with a probe that runs `$SHELL -lc 'command -v gh && gh issue list --limit 200 --json number,title,state,labels,updatedAt,url'` and time it. Checked only on Linux from a terminal (2026-11-25: `gh` 2.105.0 found by the login shell); the macOS GUI case is unproved.
- The derived column is wrong in practice: agents commonly stay `finished` at an input prompt while still working, so 確認待ち fires early. Check against agent-orchestration's `--settle` finding (D14): if it fires early, 確認待ち must also require the result file to be written.
- Agents do not write `$NOA_RESULT` reliably enough for 確認待ち to show a report. Check: `.noa/result-*.md` in this repo already shows they do for `+ctl` workers.

## Implementation plan

Four units, each built by one agent in its own worktree. Thin end-to-end path first: one local task, one run, one column change (AC-21), before GitHub.

```text
U1 skeleton ──┬── U2 local + runs ──┐
              └── U3 GitHub ────────┼── AC-32, AC-33 (manual, U1–U3 merged)
U4 Noa integration (start now; merge after U1)
```

### Rules for every unit

- Copy only from the files L2-1 names; depend on no other widget's crate.
- U1's frozen files and every `pub` item of L2-2 (name, signature, fields, variants) are contracts. Units fill bodies and add private items, `pub` items in their own files, and their own tests (L2-1). A unit that needs a contract changed stops that part, writes `#TODO(agent): <change and why>` in its report, works around it in its own files where it can; the change lands as one follow-up commit after the units merge.
- No new dependency, no `cargo update`. Every cargo command runs with `--locked --offline` (inside `widgets/tasks` for U1–U3).
- Tests that need another unit's body (still `todo!()`) belong to that unit. Every test passes on Linux and macOS (L2-3).
- Done means: the unit's done commands pass, its ACs have passing tests named `acNN_…`, and its branch is rebased on the branch U1 merged into. Once a unit merges, delete its rows from L2-2 here (the code is the copy).

### U1 skeleton (start now; U2 and U3 wait for its merge)

- Creates every file of L2-1: frozen files in full; the others with L2-2's `pub` surface, doc comments and `todo!()` bodies, except the bodies L2-2 marks *full* and the no-op UI stubs.
- Spec: L1-1, L1-5 (columns, cards, filter line, keys, GitHub header, flags), L2-1, L2-2.
- ACs: 1–7.
- Done: in `widgets/tasks`, `cargo build --locked --offline --all-targets`, `cargo test --locked --offline` (fsx's tests and AC-1…7) and `cargo run --locked --offline -- --demo --snapshot=$TMPDIR/t.png` pass; `git diff --stat main` shows only `widgets/tasks/`.

### U2 local + runs (after U1)

- Fills: `src/store.rs`, `src/local.rs`, `src/runs.rs`, `src/worktree.rs`, `src/live.rs`, `src/live/start.rs`, `src/ui/detail.rs`, `src/ui/forms.rs`. May create `tests/local_*.rs`.
- Spec: L1-2, L1-4, L1-5 (detail pane, forms, 完了にする / 再開).
- ACs: 8–22. `LiveFeed::start` builds its `IssueFeed` with `github::GhFeed::start` (U3's body); U2's tests use a fake `IssueFeed` only; the live window (`--repo`) panics there until U3 merges.
- Done: in `widgets/tasks`, `cargo build --locked --offline --all-targets`, `cargo test --locked --offline` and `cargo run --locked --offline -- --demo --select=t-1 --snapshot=$TMPDIR/d.png` (a demo local task's id) pass.

### U3 GitHub (after U1, in parallel with U2)

- Fills: `src/github.rs`; may create `src/github/*.rs` and `src/github/testdata/*.json`.
- Spec: L1-3.
- ACs: 23–27; manual 34, 35.
- Calls nothing of U2 (no `local`, `runs`, `live`).
- Done: in `widgets/tasks`, `cargo build --locked --offline --all-targets` and `cargo test --locked --offline` pass. U3 is built and unit-tested whether or not AC-35 has been run: AC-34 and AC-35 are manual and stay open until a person runs them; neither gates U3's start or merge.

### U4 Noa integration (start now; merge after U1)

- Edits: `crates/noa-app/src/commands/command.rs`, `crates/noa-app/src/commands/tests.rs`, `crates/noa-app/src/command_palette.rs`, `crates/noa-app/src/app/commands.rs`, `crates/noa-app/src/app/helpers/dispatch.rs`, `crates/noa-app/src/split_tree/tree/commands.rs`, `crates/noa-app/src/macos_menu.rs`, `crates/noa-app/src/helper_program.rs` (`TASKS`, `launch_args`' `--repo` match, its tests), `scripts/verify.sh` (`tasks=widgets/tasks/Cargo.toml`, `run tasks usage_check "$tasks"` in `--full`, `widgets/tasks/*` and `widget_uses` in the changed-path mode, the comment listing the widgets), `scripts/bundle-macos.sh` (build loop, `TASKS_BIN`, `-x` check, copy to `Contents/Helpers/Noa Tasks`, `codesign`), `AGENTS.md` (Verification), `docs/FEATURES.md` (a **Tasks** entry after Verify). Nothing under `widgets/tasks/`.
- Spec: L1-6.
- ACs: 28–31; manual 32, 33 once U1–U3 are merged.
- Done: `cargo test -p noa-app --locked --offline` and, on Linux, `cargo clippy --workspace --all-targets --locked --offline -- -D warnings` pass (outside the sandbox if `noa-ipc`/`noa-pty` tests hit `PermissionDenied`); after rebasing on U1, `CARGO_NET_OFFLINE=true scripts/verify.sh` passes and runs the `tasks` check; AC-30 on macOS with `scripts/bundle-macos.sh`.
