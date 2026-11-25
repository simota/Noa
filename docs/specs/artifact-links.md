# Artifact links (成果の関連リンク, #93 first scope): Spec

| | |
|---|---|
| slug | artifact-links |
| status | proposed (2026-11-26); design only, nothing built; no owner sign-off |
| owner | simota |
| as-of | 2026-11-26 (HEAD 6b4aaf9, branch `orch/design`) |
| crates | `widgets/tasks`, `widgets/review`, `widgets/verify` (no new crate, no new dependency) |
| request | #93 成果の関連リンク: from one task, follow the agent runs that worked on it, the changes they produced, and the Review / Verify results that judged each exact change; and back from Review / Verify to the task and change. |

Statements about existing code below were read at 6b4aaf9. Nothing here is built.

## L0: Vision

- Problem: a task's history is split across four records that never name each other. Tasks has its run log (`<data dir>/noa/tasks/<repo-key>/runs.jsonl`); a run's worktree `<main>-wt/task-<run-id>` holds its commits and uncommitted work; Review keeps `<data dir>/noa/reviews/<rev-id>/`; Verify keeps `<data dir>/noa/verifications/<ver-id>/`. To see whether a task's fix was reviewed, and whether the review judged the fix as it is now or an earlier version, a person has to match ids, branch names and times by hand. Once the worktree is removed, its share of that history is gone.
- Job to be done: in a task's detail, see its runs, the changes each run produced (commit SHAs, uncommitted-change snapshots), and under each change the reviews and verifications that judged that exact change. Results for an older change are shown as older, and the current change shows that it has none yet. From a review or a verification, open the task and the change it came from.
- Success definition: a person runs claude on a local task in a new worktree. The agent commits v1, and Review starts on the worktree. The agent then commits v2 after the review's findings, and Verify starts on v2. The task's 関連成果 shows run → v1 (Review, 旧版) → v2 (Verify, 現在). The Review window's target card names the task and v1, and opens the task. After `git worktree remove`, all of it is still listed, and anything that can no longer be found is marked 記録が見つからない.

## Reuse decision

The widgets' standing rule holds (`docs/specs/task-widget.md` and `docs/specs/review-widget.md`, Reuse decision): each widget is a separate crate outside the workspace with its own `Cargo.lock`, and none depends on another widget. The link code is one source file, `links.rs`, kept **byte-identical** in `widgets/tasks/src/`, `widgets/review/src/` and `widgets/verify/src/`. This follows the precedent of `fsx.rs`, which was copied verbatim (review's and verify's copies are still identical; tasks' has since diverged, and `links.rs` adds a drift test so that cannot happen silently). A shared crate would be a fourth gated crate and a change to the Reuse decision for a few hundred lines. The copy costs less, and a test in the file itself fails when the copies differ (U1).

| Piece | Source | Adaptation |
|---|---|---|
| Append-only JSONL with a torn-line cut, exclusive `flock` on `<log>.lock`, `O_NOFOLLOW` | `widgets/tasks/src/runs.rs` (`lock`, `append`, `replay`) | event type `LinkEvent`; dedupe under the lock |
| `repo_key` (`<dir name>-<FNV-1a 64 hex>`), `fnv1a` | `widgets/tasks/src/store.rs` | applied to the **main** worktree path |
| `git` without the person's `GIT_*` environment, `main_worktree` | `widgets/tasks/src/worktree.rs`, `store::without_git_env` | plus pinned diff options as `widgets/verify/src/capture.rs` pins them (L1-1 of `verify-widget.md`) |
| Data dir root | `noa_config::session_state_path()`'s parent, as `store::default_root` | `…/links` |

No crate is added. `libc` (flock), `serde`, `serde_json` and `noa-config` are already dependencies of all three widgets (checked in their `Cargo.toml`).

## Glossary

| Term | Meaning | Code |
|---|---|---|
| node | a record a link points at: task, run, change, review, verify | `links::Node` |
| change | one exact state of code: a commit, or a snapshot of uncommitted work on a commit | `links::ChangeId` |
| snapshot id | the digest that names one uncommitted state (L1-3) | `ChangeId::Snapshot` |
| link | one directed relation between two nodes, by id | `links::Link` |
| current change | the change a task's work is at now (L1-3) | `links::current_change` |
| 旧版の結果 | a result whose judged change is not the task's current change | `ResultEntry::stale` |

## L1: Requirements

### L1-1. Data model (reuses existing ids)

Nodes reuse the ids the records already have. No record is copied or merged into the link store.

| Node | Existing id | Where the record lives | "Found" when |
|---|---|---|---|
| `Task(TaskKey)` | `tasks::model::types::TaskRef` (`Local(String)` `t-<millis>[-n]`, `GitHub{repo, number}`, `Code{path, key}`) | `<repo>/.noa/tasks/<id>.md`, a GitHub issue, a code marker | local: its file exists; GitHub: always (no network check: shown with its URL); code: its file exists |
| `Run(String)` | `RunId` `run-<millis>[-n]` | the `RunStarted` line in the runs log that started it | the runs log named in its info holds that run id |
| `Change(ChangeId)` | commit SHA (40 or 64 hex); snapshot id `snap-<head sha>-<16 hex>` (new, L1-3) | git object store; a snapshot's capture dir (`<workdir>/.noa/reviews/<id>/target/` or `.noa/verify/<id>/target/`) | commit: `git cat-file -e <sha>^{commit}` in the main worktree; snapshot: its recorded capture dir exists |
| `Review(String)` | `review::model::types::SessionId` `rev-<millis>[-n]` | `<data dir>/noa/reviews/<id>/events.jsonl` | that file exists |
| `Verify(String)` | verify `SessionId` `ver-<millis>[-n]` | `<data dir>/noa/verifications/<id>/events.jsonl` | that file exists |

`TaskKey` is `links.rs`'s own enum with **the same serde shape** as `TaskRef` (`#[serde(rename_all = "lowercase")]`: `{"local":"t-1"}`, `{"github":{"repo":"o/n","number":12}}`, `{"code":{"path":"…","key":"…"}}`). Review and Verify can use it without depending on Tasks. A test in tasks checks that the two JSON forms are equal both ways.

```rust
// links.rs (identical in the three widgets)
pub enum Node { Task(TaskKey), Run(String), Change(ChangeId), Review(String), Verify(String) }
pub enum ChangeId { Commit(String), Snapshot(String) }
pub enum Rel {
    Ran,       // Task   → Run
    Produced,  // Run | Task → Change
    Judged,    // Review | Verify → Change   (the exact change the result is about)
    About,     // Review | Verify → Task | Run (where it came from)
}
pub enum Origin { Auto, Manual }
pub struct LinkId(String);   // "lnk-<millis>[-n]", unique in the store
pub struct Info {            // what a node was when last seen; latest Observed wins per field
    pub label: Option<String>,    // task title, run "claude · task-run-5", review target label
    pub record: Option<PathBuf>,  // the file/dir whose existence is "found" (table above)
    pub workdir: Option<PathBuf>, // run's work tree; snapshot's work tree
    pub branch: Option<String>,   // run's branch (None for Isolation::Here)
    pub head: Option<String>,     // run: commit it started on; snapshot: its head commit
    pub outcome: Option<String>,  // review/verify verdict label (承認 …, 合格 …)
}
#[serde(tag = "event")]
pub enum LinkEvent {
    Linked   { id: LinkId, from: Node, to: Node, rel: Rel, origin: Origin, at: Millis },
    Unlinked { id: LinkId, at: Millis },
    Observed { node: Node, info: Info, at: Millis },
}
pub struct Link { pub id: LinkId, pub from: Node, pub to: Node, pub rel: Rel, pub origin: Origin, pub at: Millis }
pub struct LinkSet { pub links: Vec<Link>, pub info: BTreeMap<Node, Info>, /* + unlinked triples */ }
```

- Many-to-many falls out of the shape: one task has `Ran` links to many runs, and one change has `Judged` links from many reviews and verifies.
- `Rel` fixes the allowed `(from, to)` kinds. `append` refuses any other pair, so a store edited by hand cannot make `fold` produce a nonsense graph: a line with a bad pair is skipped on read.
- Dedupe: under the lock, an `Auto` link is not appended when the same `(from, to, rel)` was **ever** linked, including one later unlinked. This keeps a person's removal from being undone by the next automatic recording. A `Manual` link is appended when no live link has that triple. An `Observed` event is not appended when it equals the node's current folded info.

### L1-2. Store: location, format, writers, concurrency

- Path: `<data dir>/noa/links/<repo-key>/links.jsonl`. `<repo-key>` is `repo_key(main_worktree(repo))`: the same key from the main checkout and from every linked worktree of it, so Tasks opened anywhere in the repository, and Review and Verify on any worktree, all read and write one store. The data dir sits beside the runs log, the reviews and the verifications (`noa_config::session_state_path()`'s parent). Closing a pane, removing a worktree, deleting a branch or restarting Noa removes nothing there.
- Format: one `LinkEvent` JSON object per line, append-only, written as `runs::append` writes (`O_NOFOLLOW`, a torn last line ended first, one `write_all` of the whole line, `sync_data`). `replay` skips lines that do not parse. A removal is an `Unlinked` tombstone and never rewrites a line.
- Writers:

  | Writer | Event | When |
  |---|---|---|
  | Tasks `live/start.rs::start` | `Linked Ran(task→run)`, `Observed Run{label, record: runs log, workdir, branch, head: start commit}`, `Observed Task{label: title, record}` | right after `RunStarted` is logged |
  | Tasks worker (`live.rs`) | `Linked Ran` backfill for runs in its log that have none | on the first poll after open |
  | Tasks worker | `Linked Produced(run→commit)` per commit in `git rev-list --reverse <head>..HEAD` (cap 50), and `Produced(run→snapshot)` when the tree is dirty | when a run's liveness becomes `Gone` or its agent `Finished`/`Error`, and when the selected task's detail is refreshed (L1-4), while the run's worktree exists |
  | Review `live/runner.rs::new_review`, Verify `live/runner.rs::new_verification` | `Linked Judged(record→change)`, `Observed Change` (snapshot: `record` = capture dir, `head`, `workdir`), `Observed Review/Verify{label, record}`, and the source `About` links (L1-5) | after `Store::create` succeeds |
  | Review / Verify runner | `Observed Review/Verify{outcome}` | when they write `report.md` and the verdict label differs from the last observed one |
  | Tasks UI (manual) | `Linked … Manual`, `Unlinked` | 関連成果's リンクを追加 / リンクを外す (L1-6) |

- Concurrency: every append takes the exclusive `flock` on `links.jsonl.lock` (as `runs::lock`) around "replay, dedupe, append", so two windows recording the same link write it once, and two link ids taken at the same millisecond differ (`lnk-<now>`, then `-2`, as `runs::new_id`). Git commands and file stats run **before** the lock is taken, so the lock is held only for one read and one write. Readers fold without the lock and tolerate a torn last line. A link write that fails never fails what triggered it: the run, review or verification goes ahead, and the notice says which link was not recorded.

### L1-3. Target version: which exact change, and the current one

A result is linked to one `ChangeId`, and a result is shown as a result for change X only if its `Judged` link names X exactly. A branch name is never stored as a target and never compared, so a result cannot follow a branch to a newer commit.

- **Commit**: `ChangeId::Commit(<full SHA>)`.
- **Snapshot** (uncommitted work): `links::snapshot_id(workdir)`:
  1. `head = git rev-parse --verify HEAD^{commit}` (an unborn HEAD: no change, `Err`).
  2. The work tree is dirty when `git status --porcelain=v1 -z --untracked-files=all --ignore-submodules=none -- :(exclude).noa` prints anything; a clean tree is `Commit(head)`, never a snapshot.
  3. Digest: 64-bit FNV-1a over `b"noa-snap-v1\0"`, `head`, `\0`, the bytes of `git diff --binary --no-color --no-ext-diff --no-textconv --ignore-submodules=none --no-renames -U3 --src-prefix=a/ --dst-prefix=b/ HEAD -- :(exclude).noa`, then for each untracked file from `git ls-files --others --exclude-standard -z -- :(exclude).noa`, sorted: its path, `\0`, its length as u64 LE, and its bytes (a link: its target path, not followed). Every git call is made without the person's `GIT_*` variables and with the settings that shape the diff pinned with `-c` (`core.quotePath=false`, `diff.noprefix=false`, `diff.mnemonicPrefix=false`, `diff.relative=false`, `diff.ignoreSubmodules=none`, `core.fsmonitor=false`, `color.ui=false`), as verify's capture does.
  4. Over 16 MiB of diff and untracked bytes together (verify's `worktree.patch` cap): `Err("大きすぎて識別できません")`, and no snapshot link is made.
  5. Id: `snap-<head>-<16 lowercase hex>`. `.noa/` is excluded so the widgets' own files (reply dirs, verify's `.noa/.gitignore` line) never change the id.

  Every widget computes the id with this one function, so equal states get equal ids whichever widget names them. Review's `diff_digest` and verify's `worktree.patch` are not reused: they cover other ranges (from the merge base) or other files.
- **What Review / Verify judged**: head `Ref(r)` → `Commit(manifest.head_sha)`. Head 作業ツリー → `current_change(repo)` taken **before and after** `capture::capture`. When the two differ (the tree moved during capture), no `Judged` link is written and the notice says 「取り込み中に作業ツリーが変わったため、対象の版を記録していません」, so the result is never tied to a state it did not see. A Files or Document review has no change and records no `Judged` link (it may still record `About` through L1-5 when its root is a run's worktree).
- **Current change of a task**: `links::current_change(workdir)` (= the clean or dirty rule above) for the task's newest run (by `started`) whose `workdir` still exists. When that worktree is gone, the current change is the newest `Produced` change recorded for that run. With no run, there is no current change and every result is listed without a 現在 mark. For an `Isolation::Here` run the work tree is the repository itself, so its current change is whatever the main checkout is at (Risks).

### L1-4. Tasks: 関連成果 section (AC3, AC5, AC6)

`links::related(&LinkSet, &TaskKey, current: Option<&ChangeId>) -> Related` is pure, and its `Related` is what the section draws:

```rust
pub struct Related { pub runs: Vec<RunEntry>, pub loose: Vec<ResultEntry> }
pub struct RunEntry { pub run: String, pub link: LinkId, pub info: Info, pub changes: Vec<ChangeEntry> }
pub struct ChangeEntry { pub change: ChangeId, pub link: Option<LinkId>, pub current: bool, pub results: Vec<ResultEntry> }
pub struct ResultEntry { pub node: Node, pub link: LinkId, pub judged: Option<ChangeId>, pub stale: bool, pub info: Info }
```

- Order: runs by start (oldest first), and within a run its changes in the order first recorded, each with the reviews and verifies that judged it (by time). Read top to bottom, this is the history of implementation, review, fix and verification. Results linked `About` the task or one of its runs whose `Judged` change is in no run go to `loose` (「対象の版なし」), each still naming its judged change if it has one.
- Current change: marked 現在. When it is not among the recorded changes, a `ChangeEntry` with `link: None` and no results is added under the newest run and shown as 「この版の結果はまだありません」. Every result whose `judged` differs from the current change has `stale: true` and is shown as 「旧版 <short id> に対する結果」. Every result line names its change (`abc1234`, or `abc1234 + 未コミット (snap …)` for a snapshot).
- Missing (AC6): before drawing, the worker checks each node's presence (L1-1's "Found when"; git and stat run on the worker thread). A missing node keeps its row and its links and is shown as 「記録が見つからない」 with what it pointed to (id plus `record`/`workdir` path, or the SHA). `Presence::Unknown` (git failed, a GitHub issue) is shown without the mark.
- Refresh: when a task's detail opens, every 10 s while it stays open, and on 再取得. This is less often than `POLL_MS`, because `current_change` runs a diff.
- Navigation: each row's button gives a `links::Nav`:

  | Row | Nav | What happens |
  |---|---|---|
  | review | `Nav::Review { dir }` | starts `review --session=<dir>` (read-only stored view) |
  | verify | `Nav::Verify { dir }` | starts `verify --session=<dir>` |
  | run | `Nav::Run(run)` | in Tasks: the existing ペインを開く when its pane is alive, else worktree の場所を開く when the worktree exists |
  | change | `Nav::Copy(id)` | copies the SHA / snapshot id (no widget owns a commit) |

  `dir` is accepted only if it is `<data dir>/noa/reviews/<id>` (or `verifications/<id>`) with `id` matching `rev-\d+(-\d+)?` (`ver-…`). The path is rebuilt from the id, never taken from the store as given. `links::launch(&Nav)` finds the sibling program the way `crates/noa-app/src/helper_program.rs::locate` does, starting from the widget's own executable: first `<exe dir>/<bundled name>` (`Noa Review`, `Noa Verify`, `Noa Tasks` in `Contents/Helpers`), then `<exe dir>/../../../<crate>/target/{debug,release}/<crate>` (a development build under `widgets/<self>/target/<profile>/`). It spawns the program detached and reaps it on a thread. A program it cannot find gives a notice that names the command to run.

### L1-5. Review and Verify: back-links (AC2, AC4)

- Source at hand-off. When a review or verification is created on change C in work tree T, it records `About` links found from the store, exactly:
  1. If some run has a `Produced` link to C, then `About(record→run)` and `About(record→task)` for each `Ran` link to that run.
  2. Otherwise, if exactly one run's `Observed` info has `workdir == T` and a `branch` (a run made with `Isolation::Worktree`: each such worktree belongs to one run), then `About` to that run and its task, plus `Produced(run→C)`.
  3. Otherwise there is no source link. That includes the main checkout shared by `Isolation::Here` runs, and a commit no run is known to have produced. A person adds the link by hand (L1-6).

  No time window, branch-name pattern or other guess is used: candidates of that kind are out of scope.
- Display: the target card (`ui/cards.rs` in each widget) gets two rows, 「タスク: <badge> <title>」 and 「対象の版: <short id>」, each with 開く. For a review or verification with several sources, the task row lists each one. A source that cannot be found is shown 「記録が見つからない（…）」 and has no button. With no link, it says 「関連するタスクはありません」.
- Navigation: タスクを開く → `Nav::Task { repo: <main worktree>, select }` starts `tasks --repo=<main> --select=<t-… | #N | path>`; the `--select` forms are the ones `ui::names` already takes. 版を開く opens the task's detail the same way when the change has a task (its 関連成果 lists it), else copies the id.
- Read: `links::backlinks(&LinkSet, &Node) -> BackLinks` when the session is shown (live or `--session`), and again at the stored view's existing look at its log (at most once a second, `review-widget.md` "Stored views follow their log").

### L1-6. Manual add and remove (AC7)

- Tasks only (Review / Verify show links, they do not edit them). Under 関連成果: リンクを追加 opens a small form with 種類 (レビュー | 検証 | コミット | 実行) and ID (`rev-…`, `ver-…`, a SHA or an unambiguous SHA prefix of 7 or more characters, `run-…`). It appends `Linked { origin: Manual }`: `About(review|verify→task)`, `Produced(task→commit)` or `Ran(task→run)`. The input is checked at this boundary. The id must have its kind's format and be found (the session dir exists, `git rev-parse --verify <sha>^{commit}` resolves it to the full SHA, the run is in the store or the runs log), or the add is refused with the reason. A link that already exists is refused as 「既にリンクされています」.
- Every row of 関連成果 that comes from a link (automatic or manual) has リンクを外す, which asks first and then appends `Unlinked`. Automatic recording never brings it back (L1-1, dedupe).
- Both persist in the store. A reopened window, another Tasks window, and the Review / Verify back-links all see the change on their next read.

### L1-7. Out of scope

PR linking; inferred 関連候補 from branch names or time; #51, #52, #91; a レビューに渡す / 検証に渡す button in Tasks (recording happens where Review / Verify are started, L1-5); compaction of the store; links for Discussion / Competition.

## L2: Approach per acceptance criterion

| AC | Approach | Files | Checked by |
|---|---|---|---|
| AC1 store | `links.rs`: types, `append` / `replay` / `fold` / `lock` / `new_id`, dedupe, `related`, `backlinks`, `path(root, repo)` with `repo_key(main_worktree)` | `widgets/{tasks,review,verify}/src/links.rs`, their `lib.rs` | `cargo test --manifest-path widgets/tasks/Cargo.toml --locked --offline links::` (write, reload, query; many-to-many; torn line; bad pair skipped; lock exclusive; a store under a removed worktree's key survives `git worktree remove`) |
| AC2 automatic | Tasks: `start` + worker backfill/`Produced`; Review/Verify: after `Store::create` | `widgets/tasks/src/live/start.rs`, `widgets/tasks/src/live.rs`, `widgets/review/src/live/runner.rs`, `widgets/verify/src/live/runner.rs` | tasks `live::start` test (fake Noa): `Ran` + `Observed Run` written beside `RunStarted`; review/verify runner tests (temp repo): commit target → `Judged(Commit)`; dirty work tree → `Judged(Snapshot)`; tree moved mid-capture → no `Judged`; run worktree → `About` run+task; `links::source_of` unit tests in tasks for rules 1–3 |
| AC3 関連成果 | `related()` (pure) + detail section + row `Nav` | `links.rs`, `widgets/tasks/src/ui/detail.rs`, `ui/mod.rs`, `ui/tests.rs`, `live.rs`, `demo.rs` | `links::` model tests (grouping, order, loose); `ui::tests` on a fake feed: rows in order, a click yields the expected `Nav` (`Review{dir}` → argv `["--session=<dir>"]`) |
| AC4 back-links | `backlinks()` + target card rows + `Nav::Task` | `links.rs`, `widgets/{review,verify}/src/ui/cards.rs`, `ui/mod.rs`, `ui/tests.rs` | `links::` tests for `backlinks`/`Nav::Task` argv; review/verify `ui::tests`: card shows task + version, button → `Nav::Task{select:"t-1"}` |
| AC5 target version | `snapshot_id`, `current_change`, `stale` in `related()` | `links.rs` | `links::` tests: v1 commit judged, v2 commit current → v1 result `stale`, v2 「結果なし」; same branch moved → still v1; snapshot on v1's head ≠ v1; clean tree → `Commit`; same dirty bytes → same id, one byte more → other id; `.noa/` edits do not change it |
| AC6 missing | `presence()` per node kind; rows kept | `links.rs`, `widgets/tasks/src/ui/detail.rs`, review/verify `ui/cards.rs` | `links::` tests: removed worktree's snapshot dir, removed session dir, unknown SHA, run absent from its log → `Missing{what}` and the link still in `related()`; `ui::tests` shows 記録が見つからない with the path |
| AC7 manual | add/remove form and intents → worker → `append` | `widgets/tasks/src/ui/forms.rs`, `ui/mod.rs`, `ui/detail.rs`, `live.rs` | `links::` tests (manual add, refused duplicate, unlink then auto not resurrected, reload); `ui::tests`: form → `Intent::Link`, リンクを外す → confirm → `Intent::Unlink`; `live` test: intents persist across a new feed |
| AC8 gate + docs | drift test; `verify.sh` path trigger; docs | `scripts/verify.sh`, `docs/FEATURES.md`, `docs/feature-map.md`, this spec | `scripts/verify.sh --full` passes against the starting baseline |
| AC9 | manual, macOS GUI: the L0 success definition end to end | — | reported unconfirmed until a person runs it |

Tests use temp dirs, real `git`, fakes for Noa and for the launcher (`Nav` is checked as argv, never spawned). On Linux, `scripts/verify.sh` builds the review and verify widgets but does not run their tests (`widget_check`). Only tasks' tests run everywhere (`usage_check`). The logic therefore lives in `links.rs`, whose tests run on Linux in tasks, and the review/verify hooks stay thin. Their own tests run on macOS (CI `--full`).

## Risks

1. **Copies drift.** Three copies of `links.rs`. Mitigation: a test in the file reads `CARGO_MANIFEST_DIR/../{tasks,review,verify}/src/links.rs` and fails unless all three are byte-equal. U5 makes `scripts/verify.sh` run the tasks check when any copy changes, so the Linux gate catches a lone edit to review's or verify's copy.
2. **Snapshot cost and races.** `current_change` runs a diff on each refresh of an open detail (10 s) and at each hand-off. It is capped at 16 MiB, beyond which no snapshot is linked. A tree edited during capture gets no `Judged` link (L1-3) rather than a wrong one.
3. **FNV-1a 64 is not collision-resistant.** It is adequate for telling a person's own states apart, not against a crafted collision. The store is in the data dir, outside any agent's work tree, so no agent needs to write it.
4. **Commits can disappear.** After a worktree's branch is deleted, `git gc` may prune its commits. Those changes then show 記録が見つからない, which is the AC6 behaviour. A squash merge makes a new SHA that is linked to nothing until a person adds it.
5. **`Isolation::Here` runs share the main checkout.** Their current change includes anyone's edits there, and Review / Verify cannot tell which Here-run a change came from (rule 3: no link). The section labels the current change of a Here-run 「この作業ツリーの現在の状態」.
6. **Runs log keys differ from the store key.** The runs log is keyed by the toplevel the Tasks window was opened in, while links are keyed by the main worktree. So a run's presence is checked against the runs log recorded in its `Observed.record`, never against the window's own log.
7. **Sibling launch is a second copy of `helper_program::locate`.** If the bundle layout changes, navigation from a widget gives a notice naming the command instead of opening the window. A test pins both candidate paths.
8. **Store growth.** It is append-only, with no compaction (out of scope). Observed events are deduped, so growth follows real events: a few lines per run, review or verification.
9. **Review/verify hooks are untested on Linux CI.** See L2. The hooks call one `links::record_handoff` function, which is tested in tasks.

## Implementation plan

Units own disjoint files. U1 lands first. U2, U3 and U4 each start from U1's merge and run in parallel. U5 can be drafted from the start and merges last.

### U1 `links-core` (start now; U2–U4 wait for its merge). Tier: deep

- Owns: `widgets/tasks/src/links.rs`, `widgets/review/src/links.rs`, `widgets/verify/src/links.rs` (one file, three byte-identical copies), and the `pub mod links;` line in `widgets/tasks/src/lib.rs`, `widgets/review/src/lib.rs`, `widgets/verify/src/lib.rs`.
- Contents: L1-1 types; L1-2 `path`, `default_root`, `repo_key`, `lock`, `new_id`, `append` (with dedupe and pair check), `replay`, `fold`; L1-3 `snapshot_id`, `current_change`; `presence`; `related`; `backlinks`; `source_of` and `record_handoff(store, record: Node, change: Option<ChangeId>, workdir, info)` (L1-5 rules); `Nav`, `Nav::argv`, `locate_sibling`, `launch`. Self-contained tests: its own temp-dir helper, not a widget's `testkit`.
- Done: `cargo test --manifest-path widgets/tasks/Cargo.toml --locked --offline links::` passes (AC1, AC5, AC6 model tests, the `TaskKey`↔`TaskRef` JSON test, the drift test), and `cargo build --locked --offline --all-targets` passes in `widgets/review` and `widgets/verify`.

### U2 `tasks-related` (after U1). Tier: standard

- Owns: `widgets/tasks/src/live/start.rs`, `widgets/tasks/src/live.rs`, `widgets/tasks/src/ui/detail.rs`, `widgets/tasks/src/ui/mod.rs`, `widgets/tasks/src/ui/forms.rs`, `widgets/tasks/src/ui/tests.rs`, `widgets/tasks/src/demo.rs`.
- Does: the run-start links and the backfill / `Produced` recording (L1-2); the 関連成果 section, its refresh, presence and `Nav` (L1-4); リンクを追加 / リンクを外す (L1-6); new `Intent::{Link, Unlink, Navigate}` and the matching worker jobs; demo data so `--demo --select=… --snapshot` renders the section.
- Done: `cargo test --manifest-path widgets/tasks/Cargo.toml --locked --offline` passes, including the AC2 (start), AC3, AC6 (UI) and AC7 tests named in L2.

### U3 `review-backlinks` (after U1, parallel with U2 and U4). Tier: standard

- Owns: `widgets/review/src/live/runner.rs`, `widgets/review/src/ui/cards.rs`, `widgets/review/src/ui/mod.rs`, `widgets/review/src/ui/tests.rs`.
- Does: `record_handoff` after `Store::create` in `new_review`, with the before/after change check; `Observed{outcome}` when `report.md` is written; the target card rows and `Nav::Task` (L1-5).
- Done: on macOS, `cargo test --manifest-path widgets/review/Cargo.toml --locked --offline` passes (AC2 hand-off, AC4); on Linux, `cargo build --locked --offline --all-targets` passes there.

### U4 `verify-backlinks` (after U1, parallel with U2 and U3). Tier: standard

- Owns: `widgets/verify/src/live/runner.rs`, `widgets/verify/src/ui/cards.rs`, `widgets/verify/src/ui/mod.rs`, `widgets/verify/src/ui/tests.rs`.
- Does: U3's work for `new_verification`. The before/after change check uses `captured.rebuild.head_sha` for the head and runs before `store::ignore_verify_dir`.
- Done: as U3, for `widgets/verify`.

### U5 `docs-gate` (draft now; merge after U1–U4). Tier: light

- Owns: `scripts/verify.sh` (the changed-path case that sets `tsk=1` also matches `widgets/review/src/links.rs` and `widgets/verify/src/links.rs`), `docs/FEATURES.md` (a 関連成果 paragraph under Tasks, and the back-link rows under Review and Verify), `docs/feature-map.md` (a row: Artifact links → `widgets/*/src/links.rs` → `cargo test --manifest-path widgets/tasks/Cargo.toml --locked --offline links::`), `docs/specs/artifact-links.md` (status line).
- Done: `scripts/verify.sh --full` passes against the starting baseline (AC8). AC9 stays open, reported as unconfirmed, until a person runs the L0 success definition in `Noa.app`.

## Open questions (each with the default taken)

1. Store under the data dir or under the main checkout's `.noa/`? **Default: the data dir** (`<data dir>/noa/links/<repo-key>/`). It sits beside the runs log, the reviews and the verifications, and it survives worktree removal as the main checkout's `.noa/` would. It is also outside every agent's work tree, so an agent working in the main checkout (an `Isolation::Here` run) cannot edit it.
2. A shared crate or a copied file? **Default: the byte-identical `links.rs` with a drift test**, keeping the "no widget depends on another, no shared crate" decision.
3. A レビューに渡す / 検証に渡す button in Tasks? **Default: none in this scope.** Recording happens where a review or verification is created, whoever starts it.
4. How a widget opens another: a new Noa control-server method, or the widget starts the sibling program itself? **Default: the widget starts it** (L1-4). This needs no change to `noa-app` or `noa-ipc`. A Noa method would centralise `locate` and can replace this later.
5. Manual add of an id that cannot be found? **Default: refused with the reason.** Links recorded automatically whose record later disappears are kept (AC6).
6. Backfill `Ran` links for runs logged before this feature? **Default: yes**, automatically, once per window.
7. Is a removed automatic link recorded again by a later automatic pass? **Default: no.** The tombstone wins, and only a manual add restores it.
8. A work-tree target that is clean: commit or snapshot? **Default: commit.** A clean tree is exactly `HEAD`.
9. Are Files / Document reviews linked? **Default: there is no `Judged` link (no change to name).** `About` still applies when the root is a run's worktree.
