# Verify widget (multi-agent verification): Spec

| | |
|---|---|
| slug | verify-widget |
| status | draft (design only, 2026-11-24). No code yet. No owner sign-off |
| owner | simota |
| as-of | 2026-11-24 (HEAD 23b98f3f, branch `main`); statements about existing code checked against 51ac2170 (`widgets/review` unchanged since 23b98f3f) |
| crate | `widgets/verify` (binary `verify`, bundled as `Noa Verify`) |
| request | 「reviewウィジェットやコンペウィジェットを参考に検証ウィジェットを作成したい。まずは設計からお願い。」 |

## L0: Vision

- Problem: the review widget (`widgets/review`) finds defects by reading a snapshot; nobody runs anything, and its brief forbids edits. The competition widget (`widgets/competition`) runs a check on what contestants built, but it scores entries; it does not decide whether one change does what it claims. Whether a change works is still decided by one agent saying "tests pass", with no record anyone else can check.
- Job to be done: pick a change, agree on what "works" means (acceptance criteria), seat 2–6 verifier agents, each with a strategy (仕様適合, 反証, 回帰, …) and its own isolated copy of the change, let each verify alone by running things, then let them reproduce or refute each other's results, and get, per criterion, a ruling backed by recorded, hashed evidence, a verdict, and a Markdown report.
- Success definition: from View → Verify, a branch's change is verified by three agents over two rounds against criteria the person confirmed, and ends in a verdict and `report.md` in which every criterion has a ruling, every ruling cites evidence the widget stored and hashed, and every command marked for re-run was run again, by a script the widget wrote, in the verifier's own pane, with its exit status compared.
- Assumed meaning of 検証 (the request does not define it): **dynamic verification of a change against acceptance criteria** — agents execute tests, the app and ad-hoc probes. Static reading is the review widget's job. See Decisions taken by default, 1.

## Reuse decision

As the review widget decided (`docs/specs/review-widget.md`, Reuse decision): `widgets/verify` is a fourth independent crate outside the workspace, with its own `Cargo.lock`; it depends on no other widget and no shared crate is extracted. The review widget is the newest and closest copy, so most pieces come from it:

| Piece | Source | Adaptation |
|---|---|---|
| Types (`string_id!`, `AgentKind`, `AgentSetup`, `Participant`, `Receipt`, `Brief`, `Round`, `Millis`, `RoundNo`), the brief lifecycle events, `create`/`apply`/`replay` | `review/src/model/{types,reduce}.rs` | `Participant.lens` → `strategy`; `Role` is `Judge`/`Verifier`; new events (L2-3) |
| Engine shape (Briefing → parallel work → one evaluator → Closed, timeouts, retries, correction notes, hold/continue/extend, `check_resumable`, `check_store_paths`) | `review/src/engine.rs` | a criteria phase before round 1; evidence ingest and re-run between a reply and the judge |
| Git capture (merge base, three-dot diff, worktree mode with untracked files, caps, manifest, digests) | `review/src/capture.rs` | Git and work-tree-state targets only (no Files/Document); the manifest also records the tree to rebuild |
| `Store`, `fsx` (`open_dir`, `write_new_at`), `installed.rs`, `live.rs`, `live/ipc.rs`, `live/runner.rs` | review | plus worktree creation/removal, the re-run script and its brief |
| 修正を依頼 (`live/fix.rs`), 同じ設定で再検証, `--repo`, stored views follow their log, search | review L1-9 … L1-12 | over failed criteria and defects instead of findings |
| Stance/consolidation folding, agreement tally, ranking | folding: `review/src/model/reduce.rs`; tally and ranking: `Session::tally`, `Session::ranked` in `review/src/model/types.rs`; review L2-4 | over criteria results and cross-checks |
| File manifest with size and FNV-1a digest | `competition/src/check.rs` (`fnv1a64`, `manifest.md`) | the evidence manifest (L1-5) |
| Start view rows, progress strip, cards, composer | `review/src/ui/*` | strategy column, criteria editor, isolation choice |

New code: the criteria phase and editor, per-verifier worktrees, evidence ingest and hashing, the re-run script and its watcher, the criteria × verifier matrix, the evidence view, and the report.

## Glossary

| Term | Meaning | Identifier |
|---|---|---|
| verification (検証) | one session: a target, criteria, a panel, rounds, rulings, a verdict | `Session`, id `ver-<millis>[-n]` |
| target | the change being verified, captured once | `Target`, `TargetManifest` |
| criterion (受け入れ基準) | one checkable statement of what "works" means | `Criterion`, id `AC-1`, `AC-2`, … |
| judge (判定役) | the agent that drafts the criteria, rules on each round and gives the verdict; runs nothing itself | `Role::Judge`, id `judge` |
| verifier (検証者) | an agent that runs checks in its own worktree and reports results | `Role::Verifier`, ids `claude`, `codex`, `claude-2`, … |
| strategy (手法) | the way a verifier is asked to verify (the review widget's lens) | `Strategy` |
| result (結果) | one verifier's outcome on one criterion in one round | `CriterionResult` |
| evidence (証跡) | a command run, a log, a screenshot or an observation a result rests on | `Evidence`, key `<verifier>/r<N>/<id>` |
| re-run (再実行) | a verifier's command run again in its own pane by a script the widget wrote, its exit status compared | `Rerun` |
| re-run brief | the short brief that asks a verifier to start the re-run script | `BriefKind::Rerun` |
| defect (不具合) | a problem a verifier found that no criterion covers | `Defect`, id `D1`, `D2`, … |
| cross-check (追試) | a verifier reproducing or refuting another's result in round ≥ 2 | `CrossCheck` |
| ruling (判定) | the judge's decision on one criterion | `Ruling` |
| verdict (総合判定) | the judge's decision on the change | `Verdict` |

## L1: Requirements

### L1-1. Target and capture

Two target kinds, both in a git work tree (a non-git directory refuses the start: 「検証は git の作業ツリーでのみ行えます」, since every verifier needs its own copy):

1. **Git 差分** (`TargetKind::Diff`): repository, base ref (empty = remote default branch, then `main`, then `master`), head (作業ツリー or a ref), optional pathspec. Captured exactly as review L1-1 item 1: `target/diff.patch`, post-images, manifest. An empty diff refuses the start. Criteria are about what the change does.
2. **現在の状態** (`TargetKind::State`): repository and head (作業ツリー or a ref), no base. Verifies the tree as it is (for instance "the feature in spec X is implemented"). The manifest lists no changed files; `target/diff.patch` holds the uncommitted changes against `HEAD` in work-tree mode, and is empty otherwise.

Common rules:
- Caps, binary handling and the manifest are review's (`MAX_DIFF_BYTES` 4 MiB etc.). Besides the review manifest, the capture records the **rebuild recipe**: the head commit SHA, and in work-tree mode `git diff --binary HEAD` (`target/worktree.patch`, cap 16 MiB) plus every untracked, non-ignored file (`target/untracked/<path>`, same text/size rules; a binary untracked file is copied as is up to 1 MiB, else refuses the start with its name). This recipe is what each verifier's worktree is built from (L1-3).
- No capture command writes to the person's index or work tree.
- A **spec file** may be named (optional, a path inside the repository, text, ≤ 1 MiB): copied to `target/spec.md` and given to the judge as the main source of criteria.

### L1-2. Criteria (受け入れ基準)

The first phase of every verification, before any verifier is briefed.

- Sources, in the judge's brief: the request text (依頼), the person's own criteria (start view text area, one per line, optional), the spec file, and the target (diff and manifest).
- The judge writes `c/criteria.json` (L1-6): a list of criteria, each with id `AC-<n>`, a one-line statement, how to verify it, its priority (`must` / `should`), and its source (`user`, `spec:<line>`, `request`, `inferred`). Every line the person gave must appear as exactly one criterion with source `user` and the person's wording kept in `statement` (the judge may add `how`).
- 基準を確認してから検証 (start view checkbox, default **on**): after the judge's criteria are accepted the session holds, and the 基準 tab shows them editable: change statement, how and priority, add, remove (an `inferred` or `request` criterion only; a `user` one can be edited but not removed), and reorder. 検証を開始 starts round 1. Every change is an event (`CriteriaEdited`), so the report can say which criteria the person changed. Off: round 1 starts as soon as the criteria are accepted.
- Limits: 1–30 criteria (`MAX_CRITERIA`); a statement 1–300 characters.
- Criteria are frozen once round 1 starts. A criterion that turns out wrong is ruled `not_applicable` by the judge with a reason; the judge cannot add criteria later, but may open defects.

### L1-3. Isolation: one worktree per verifier

Verifiers run things: builds, tests, the app, throwaway probes that write files. Running several in the person's work tree at once would mix their edits and outputs, and could change what the person is working on.

- 隔離 (start view): **個別 worktree** (default) or **共有（読み取りのみ）**.
- 個別 worktree: when round 1 starts the runner creates, for each verifier, a detached worktree at `<main worktree>-wt/verify-<session id>-<verifier>`, where the main worktree is the repository's (the first entry of `git worktree list --porcelain`, as `main_worktree` in `crates/noa-app/src/ctl/worktree.rs` picks the `<main>-wt/` parent `Noa +ctl spawn --worktree` uses; the session's `main_worktree`), also when the target is a linked worktree (a target in `/src/app-wt/feature` of `/src/app` gets `/src/app-wt/verify-…`): `git worktree add --detach <path> <head sha>`, then in work-tree mode `git apply --binary target/worktree.patch` and the untracked files copied in. It then checks the rebuilt tree against the manifest (each manifest file's digest); a mismatch refuses round 1 and names the files. No branch is created. The verifier's brief allows any edit inside its worktree (throwaway tests, debug prints) and forbids edits anywhere else.
- 共有（読み取りのみ）: every verifier runs in the target's checkout top level (`git rev-parse --show-toplevel` of the target's repository, the session's `workdir`; the linked worktree itself when the target is one); the brief forbids edits (as review's does) and allows only commands that do not write tracked files. For a quick verification, or a tree too large to check out several times. The report says the mode and that isolation was not enforced.
- The judge always runs in the session's reply dir of the target's checkout (`<workdir>/.noa/verify/<id>/`) and runs nothing (L1-4).
- Removal: when the verification closes (verdict, or 終了), the runner removes each worktree it made with `git worktree remove --force <path>` after checking, through `git worktree list --porcelain`, that the path is still a worktree of this repository at a detached HEAD whose path is the one the log records; anything else is left and named in a notice. An interrupted verification keeps its worktrees for 再開. The board's ワークツリーを削除 removes them on demand for a stored, ended or interrupted verification (an interrupted one then rebuilds them on 再開).
- Build caches: each worktree has its own `target/` (or language equivalent). The brief tells verifiers so; a shared cache is out of scope (concurrent builds would lock each other).

### L1-4. Rounds

Phases: Criteria (基準策定) → [held for confirmation] → per round: Briefing (配信) → Verifying (検証) → Ingest (証跡収集, widget only) → Ruling (判定) → Closed.

1. **Round 1, 独立検証 (`RoundKind::Blind`)**: every verifier gets the request, the target, the frozen criteria, its strategy and checklist, its worktree, the rules and the reply format. For every criterion it reports a result (`pass` / `fail` / `blocked` / `not_tested`) with evidence, and it may report defects. It names no peer and must not read other verifiers' worktrees or replies.
2. **Ingest and re-run (every round)**: once every verifier has replied or failed, the widget copies each reply's evidence files into the judge's reply dir (`<repo>/.noa/verify/<id>/evidence/`, L1-10), hashes them, and has each verifier run its re-runs in its own pane (L1-5). Results that cite missing files are not rejected here (validation already checked existence); a file that vanished between validation and ingest makes that evidence `missing`.
3. **Ruling (every round)**: the judge gets every result with its evidence summaries, digests and re-run outcomes, every cross-check, and the defects, and writes `r<N>/judge.json`: a ruling per criterion (`verified` / `failed` / `inconclusive` / `not_applicable`, with rationale and the evidence keys it rests on), defect consolidation (add / merge / update / dismiss, as review's lead ops), and cross-check assignments for the next round.
4. **Rounds 2…, 追試 (`RoundKind::Cross`)**: every verifier gets the rulings so far, every result and evidence summary with identities, and its assignments: criteria and defects the judge asked it to reproduce or refute (default when the judge assigns none: every `fail` and every `pass` resting on a single verifier, given to a verifier other than the one who reported it, round-robin). For each assignment it writes a cross-check (`reproduced` / `not_reproduced` / `disputed`) with its own evidence from its own worktree. It may also update its own results with new evidence.
5. **The last round**: the judge must give a verdict; no criterion may stay unruled. Early end: the judge may set `close: true` (with a verdict) from round 2 on, or in round 1 when `max_rounds` is 1, or in round 1 when every criterion is `verified` by two or more verifiers with re-runs that matched.
6. Round limit default 2, 1–6. ラウンド後に停止, interventions, +1 / +3, 再試行 and a failed verifier not stopping the round as review L1-3 items 7–9; 終了 (ends without a verdict, asks first) as review L1-3 item 5 and L1-5 (Bottom).
7. Timeouts: start 90 s, reply **45 min** (builds and test suites are longer than reading), 2 retries.
8. Panes: one Noa tab per verification with auto approval, judge and verifiers in splits on the 3×3 grid; each verifier's pane starts in its worktree.

### L1-5. Evidence, hashing and re-runs

The point of the widget: a ruling rests on what the widget stored and checked, not on what an agent said.

- Evidence kinds a verifier reports (L1-6):
  - `command`: the shell command, its working directory (relative to the worktree), the exit status the verifier saw, the path of its saved output (`logs/<n>.log`, combined stdout and stderr, written by the verifier, e.g. `cmd > logs/3.log 2>&1; echo $?`), and `rerun: true|false`.
  - `file`: a file the verifier produced (screenshot, screen recording, generated output), path under its reply dir. Screenshots and videos follow L1-5a.
  - `observation`: text only (something seen in the GUI, say). Shown as 未裏付け and never enough alone for `verified` (L1-6 judge rules).
- Validation (at reply time; the runner inspects each cited path with `evidence::inspect` and passes the facts in with the reply, L2-2 `FileFact`, so the check itself is pure): every `logs/…` and `file` path exists under the verifier's reply dir, is a regular file (no link), and is at most 8 MiB (logs) / 15 MiB (other files; media per L1-5a); at most 40 evidence items and 512 MiB in total per reply.
- Ingest: each file is copied to `<reply dir>/evidence/r<N>/<verifier>/<path>`, the judge's reply dir in the target's checkout (`<repo>/.noa/verify/<id>/`, beside `target/`; Decision 11), its directories opened one component at a time from the repository and the file created exclusively (`fsx::open_dir`, `write_new_at`, no link followed), and its FNV-1a 64 digest and size recorded in `EvidenceIngested`. `manifest.md` per round lists every item (as competition's check manifest). From then on the stored copy is what the judge and the report cite; the worktree may change or go.
- Re-run (`rerun: true`), **in the verifier's own pane**: a command often depends on the environment the verifier ran it in — the variables Noa gives every pane (`NOA_BIN`, `NOA_SURFACE_ID`, set in `crates/noa-app/src/app.rs`), the agent's own shell and its sandbox. A re-run made by the widget as its own subprocess would lack them and report a difference that is the widget's, not the change's. So the re-run goes where the command first ran:
  1. When a verifier's reply is accepted (and it has `rerun: true` items; in 共有 mode re-runs are off, as they would run in the person's tree, and the report says so), the widget writes, under that verifier's reply dir, `r<N>/<verifier>/rerun/`: one file per command, `<n>.cmd`, holding the command exactly as reported (never parsed or quoted by the widget), and `run.sh`, a fixed script generated from constants and widget-chosen paths only (each single-quoted), with a fresh random nonce (16 bytes read from `/dev/urandom`, hex; no random-number crate is added). Their digests are logged in `RerunRequested { round, part, nonce, items, script_digest }`.
  2. `run.sh` starts with `set -m`, then runs the commands one after another: for each, `cd` to the reported `cwd` under the worktree, run `"${SHELL:-/bin/sh}" <n>.cmd < /dev/null > <n>.log 2>&1` in the background, write its pid to `<n>.pid`, wait, and write its exit status to `<n>.exit` (temporary file + `mv`, so a half-written status is never read). When all are done it writes `done` holding the nonce. It writes nothing outside `rerun/` except what the commands themselves do. `set -m` is what makes the timeout kill of step 4 possible: checked on macOS `/bin/sh` (bash), a background command started after `set -m` leads its own process group (pgid = its pid), while without it the command shares the script's group, so there would be no group of its own to signal. zsh refuses `set -m` in a script (`can't change option: -m`), which is why the script is always run with `sh`, never `$SHELL`.
  3. The widget sends the verifier a **re-run brief** (`BriefKind::Rerun`, through the usual lifecycle: it waits while the agent is working or sits on a prompt, `refuseShell`, start timeout): "Run exactly this with your shell tool, detached so your tool's own time limit does not cut it short, and do nothing else until the next brief: `nohup sh '<abs path>/run.sh' >/dev/null 2>&1 &`. Do not edit any file under `rerun/`. Reply nothing." The agent needs no reply file; the widget watches `rerun/`. #TODO(agent): confirm, in a live Noa (AC-35), that a process started with `nohup … &` from each agent's shell tool (claude, codex in its sandbox, agy) outlives the tool call; reading code cannot settle it. Fallback, built in from the start so an unconfirmed kind cannot hang a round:
     - Every poll, a command whose `.pid` exists but whose process is gone (`kill(pid, 0)` fails with `ESRCH`) and which has no `.exit` is `not_run` ("the re-run was stopped before it finished; the agent's tool may have ended it"), at once rather than at `RERUN_TIMEOUT`; the commands after it are `not_run` ("re-run did not finish") when `run.sh` itself is gone too (no new `.pid` within the start timeout).
     - A kind found to kill detached processes gets, in its re-run brief, a foreground run instead (`sh '<abs path>/run.sh'`, "with your shell tool's longest time limit"), and its per-command limit becomes the smaller of `RERUN_TIMEOUT` and that tool limit; a command cut off by the tool ends `not_run` by the rule above. Which kinds get it is one constant per `AgentKind` in `rerun.rs`, `true` (detached) for all until AC-35 says otherwise.
  4. The widget watches the `.exit` files (polling as replies are polled, through `rerun::poll`, which times each command from its `.pid` being first seen, does the kills below and gives each item's outcome; the runner keeps its timing state between polls). Each command has a 10 min limit (`RERUN_TIMEOUT`) counted from its `.pid` appearing; past it the widget sends SIGTERM to that pid's process group, SIGKILL 5 s later, and records `timed_out`. A script that has not written its first `.pid` within the start timeout after the brief was sent counts as not started: the brief is retried as any brief, then every item is `not_run` ("re-run did not start").
  5. At `done` (with the right nonce), or when every item has an outcome, the widget checks `run.sh` and every `<n>.cmd` against the logged digests (a mismatch makes every item of that verifier `not_run`: "re-run script was changed"), then ingests each `<n>.log` (capped at 8 MiB, the cut marked) as `<reply dir>/evidence/r<N>/<verifier>.rerun/<n>.log` (`store::rerun_log_path`) and records `RerunFinished`. The re-run logs get that sibling directory, never a name inside `evidence/r<N>/<verifier>/`, because that directory holds the verifier's own evidence at the paths it chose: a reported path is relative with no `..`, so it stays inside `<verifier>/`, and a verifier id (an agent kind with an optional `-<n>`) never holds a `.`, so no reported file can be stored where a re-run log is. Outcome: `matched` (same exit status), `differed` (both statuses recorded), `timed_out`, `not_run` (why: 共有 mode, pane gone, did not start, stopped before it finished, script changed). The outcome and its log are recorded apart: an item of any outcome whose `<n>.log` was written keeps that log, ingested as above, and an item whose log could not be stored keeps its outcome without one.
  - At most 8 re-runs per verifier per round; verifiers run theirs in parallel, each in its own pane and worktree.
- Trust (accepted limit): the re-run passes through the verifier's own agent, which starts the script and could, against its brief, alter what runs or what is written. The widget checks the files it wrote and the nonce, not the agent's process. A re-run therefore shows that the command gives the same status again in the same environment — it catches misreported exit codes, stale logs and flaky results — and is not a proof against a verifier that lies. The report says re-runs ran in the verifiers' panes; independent confirmation comes from the cross-check rounds, where another verifier runs the claim in its own worktree.
- Re-runs happen after the verifier's reply is accepted and before the judge is briefed; the verifier's reply brief tells it that a re-run brief may follow and not to touch the worktree until the next brief.

### L1-5a. Screenshots and videos (スクショ・動画)

GUI behaviour (a tab opening in the right directory, a dialog, an animation) is often only shown by a picture. Screenshots and screen recordings are first-class evidence, from verifiers and from the person.

- Media kinds, decided by the file's leading bytes (magic), not its extension; a mismatch between the two is a validation problem:
  - image: PNG, JPEG, GIF, WebP; at most 15 MiB (`MAX_IMAGE_BYTES`).
  - video: MP4 / QuickTime (`ftyp` box), WebM; at most 200 MiB (`MAX_VIDEO_BYTES`).
  - Anything else stays a plain `file` (opened, not previewed).
- A `file` evidence item that is media carries, besides `path`:
  - `caption` (required for media, 1–300 characters): what the picture shows, in words, so a judge or reader who cannot open the file still knows what it claims.
  - `marks` (video only, optional, at most 20): `[{"t": "00:12.5", "note": "new tab opens in /tmp"}]`, moments to look at; `t` is `mm:ss[.f]` or `hh:mm:ss[.f]` (the first field one or two digits, each later one two, minutes and seconds 00–59, `f` one to three digits of a second; `media::parse_mark`, whose error names the problem), non-decreasing.
  - `shows` (optional): the criteria ids it is evidence for, when the item is cited by results of several criteria and the picture shows only some of them.
- Capture: the verifier brief (L2-4, section Evidence) tells verifiers to save screenshots under `shots/` and recordings under `video/` in their reply dir, to use the platform's own capture tools, and to report `blocked` with the reason when capture is not allowed (on macOS, a screen-recording permission the agent's terminal lacks) rather than describing the screen as an `observation`. #TODO(agent): confirm which capture commands work for an agent inside a Noa pane on macOS (permission prompt per app) before writing the brief text.
- Person-attached media (証跡を追加): on the board, the selected criterion's thread and the evidence tab offer 証跡を追加 (a file chooser and dropping files onto the window), with a required caption and the criteria it shows. The widget validates it as above, copies it to `<reply dir>/evidence/user/<n>.<ext>` (the judge's reply dir, as ingest; exclusive create, no link followed), hashes it, and logs `EvidenceAttached { key: "user/<n>", round, body, stored, shows, at }` (`round`: the round current then, kept on the item with the time, so filters and cards show it after a replay). Allowed from round 1's start until the verification closes; the judge sees it in the next ruling brief. Person-attached evidence is cited like any other, and shown with the tag あなた; it counts as one independent source for `verified` (L1-6) only together with a verifier's `pass` on the same criterion.
- No pictures in the window: every media item shows as a **placeholder card** — kind (画像 / 動画), format (PNG, MOV, …), size in bytes, caption, verifier (or あなた), round, and for a video ▶ with its marks — wherever a preview would go (thread strip, gallery). The widget makes no thumbnails. Why (checked in the noa-gpui 0.2.1 sources `widgets/review` builds with): noa-gpui draws a picture only from RGBA8 pixels uploaded with `Renderer::create_image`, which a live window offers only while its screen is built (`noa_gpui_platform::Setup`; the `Screen` trait has no later hook, and `Headless::create_image` exists only headless), and evidence arrives after the window is built; and no public decoder exists (`noa-gpui-render-wgpu`'s `png::decode` is crate-private and reads only the stored-block PNGs it writes itself). The `png` crate in `widgets/review/Cargo.lock` is there only for `muda` / `tray-icon`; using it would be a new direct dependency, which this widget does not add.
- For a later version that draws previews (not built now): `qlmanage -t -s 480 -o <dir> <file>` writes `<dir>/<file name>.png` — the whole input name plus `.png` (`shot one.png` → `shot one.png.png`), 480 px on the long side (a 1024×1024 PNG gave 480×480) — and exits 0 in about 0.1 s; for a file whose name has no extension it recognises it writes nothing and never exits (still running when killed after 20 s), so it would need a copy named with the sniffed extension and a kill on timeout; it does not start inside the Claude Code macOS sandbox ("sandbox initialization failed: Operation not permitted"). Checked 2026-11-24 on macOS 26 (Darwin 25.6).
- Viewing: 開く opens the stored copy in the system viewer or player (`open <stored copy>`, run as a subprocess), images and videos alike, since the widget shows no picture and plays no video itself; each video mark is listed on its card with its time and note. Every media item has Finder で表示.
- What agents get: briefs list each media item with its kind, size, caption, marks and stored path. The judge is told it may open images by path when its CLI can read images, and that it cannot watch videos: a video's claim rests on its caption and marks, and the judge's rationale must say so when a ruling rests on a video alone.
- Digests, re-runs: media are hashed and stored as other files; they are never re-run. A ruling may cite them like any `file` evidence.

### L1-6. Agent output formats and validation

JSON files, one code fence tolerated, `deny_unknown_fields`, one problem per line in the correction note, `rejected_digest` logged, as review L1-4.

**Judge criteria** `c/criteria.json`:

```json
{
  "summary": "What the change claims and how the criteria cover it.",
  "criteria": [
    {"id": "AC-1", "statement": "Opening a tab inherits the focused pane's cwd.",
     "how": "Run the app, cd /tmp in a pane, open a tab, check pwd.",
     "priority": "must", "source": "user"}
  ]
}
```

Rules: 1–30 criteria; ids `AC-<n>` unique, from 1 without gaps; `priority` `must`|`should`; `source` `user`|`request`|`inferred`|`spec:<line>` (a line of `target/spec.md`); every user line appears once with `source: user`, matched by exact text after trimming (missing ones are listed); `how` non-empty.

**Verifier reply** `r<N>/<verifier>/reply.json` (the reply dir is `<worktree>/.noa/verify/<session id>/`, ignored by `.noa/.gitignore = *`, so sandboxed agents can write it; in 共有 mode the target checkout's):

```json
{
  "summary": "What I ran and what I found.",
  "environment": "macOS 26.1, rustc 1.92, debug build",
  "results": [
    {"criterion": "AC-1", "result": "fail",
     "note": "New tab opens in $HOME after `cd /tmp`.",
     "evidence": ["e1", "e2", "e3"]}
  ],
  "evidence": [
    {"id": "e1", "kind": "command", "cmd": "cargo test -p noa-app cwd_inherit",
     "cwd": ".", "exit": 101, "log": "logs/1.log", "rerun": true},
    {"id": "e2", "kind": "file", "path": "shots/tab.png", "caption": "pwd in the new tab"},
    {"id": "e3", "kind": "file", "path": "video/new-tab.mov", "caption": "cd /tmp, ⌘T, pwd",
     "marks": [{"t": "00:04", "note": "⌘T pressed"}, {"t": "00:06.5", "note": "new tab shows $HOME"}]}
  ],
  "defects": [
    {"id": "1", "claim": "…", "severity": "high", "category": "falsify", "confidence": "high",
     "location": {"path": "crates/…", "line": 40}, "repro": "1. … 2. …", "evidence": ["e1"]}
  ],
  "cross_checks": [
    {"subject": "codex/AC-2", "outcome": "not_reproduced", "note": "…", "evidence": ["e3"]}
  ]
}
```

Rules:
- `results`: round 1: exactly one per criterion (missing ones listed). Round ≥ 2: optional updates of its own results.
- `result` `pass` needs at least one `command` or `file` evidence; `fail` needs evidence and a note; `blocked` (could not run: missing tool, build broken) and `not_tested` need a note.
- `evidence` ids unique in the reply; every id cited exists; every evidence item is cited at least once. `cmd` 1–2000 characters, single-line or multi-line; `cwd` relative, no `..`; `exit` an integer; paths relative to the reply dir's `r<N>/<verifier>/`, no `..`, no leading `/`, and not under `rerun/`: the widget keeps that directory for its re-run script and logs (L1-5), so a `file` or `log` path starting with `rerun/` is one problem line.
- `defects`: at most 20; fields as review's raw findings plus `repro` (required) and `evidence` (at least one id).
- `cross_checks`: not allowed in round 1. From round 2, exactly one per assignment of this verifier (missing ones listed); `subject` is an assignment (`<verifier>/<AC id>` or a defect id); `outcome` `reproduced`|`not_reproduced`|`disputed`; evidence required except for `disputed` with a note.

**Judge ruling** `r<N>/judge.json`:

```json
{
  "summary": "…",
  "rulings": [
    {"criterion": "AC-1", "ruling": "failed", "rationale": "…",
     "evidence": ["claude/r1/e1", "codex/r2/e3"]}
  ],
  "defect_ops": [{"op": "add", "id": "D1", "from": ["claude/1"], "...": "as review lead ops"}],
  "assign": {"codex": ["claude/AC-1"], "claude": ["D1"]},
  "verdict": {"decision": "fail", "rationale": "…", "blocking": ["AC-1", "D1"]},
  "close": false
}
```

Rules (the widget enforces the evidence standard; the judge cannot rule past it):
- Every criterion gets one ruling per round (a ruling carried over unchanged is allowed: `"ruling": "same"`).
- `verified` needs at least one cited `command` evidence whose re-run `matched`, or `pass` results from two different verifiers each citing `command`/`file` evidence; and no unanswered `fail` result or `reproduced` refutation on that criterion (`AC-2: verified needs a matched re-run or two independent passes`).
- `failed` needs at least one cited `fail` result or `reproduced` defect-level evidence.
- `inconclusive` and `not_applicable` need a rationale.
- Cited evidence keys exist; an `observation` alone never satisfies `verified`.
- Defect ops: every raw defect of this round placed once (add / merge / dismiss), as review's unassigned-raw rule.
- `assign`: keys are verifier ids; values are `<verifier>/<AC id>` of another verifier or a defect id; a verifier is never assigned its own result.
- `verdict` (last round or `close`): `pass` (合格: every `must` criterion `verified`, no live `critical`/`high` defect), `pass_with_issues` (条件付き合格: every `must` `verified`; some `should` not, or live defects; `blocking` lists them), `fail` (不合格: some `must` `failed`; `blocking` non-empty), `inconclusive` (判定不能: some `must` `inconclusive`, none `failed`). The widget checks the decision against the rulings and refuses a mismatch with the reason.

### L1-7. UI (Japanese labels)

Window title 「Noa · 多角検証」 / "Noa · Multi-Agent Verification", app name `Noa Verify`; minimum window as review's.

1. **開始ビュー**: 対象 (「Git 差分 | 現在の状態」, repository, base, head, path filter), 仕様ファイル (optional path, 選択…), 依頼 (text), 受け入れ基準 (optional, one per line, placeholder 「空欄なら判定役が依頼と仕様から作成」), 判定役 row, 検証者 2–6 rows (agent / model / effort / 手法 / custom text), 隔離 (個別 worktree | 共有（読み取りのみ）), 基準を確認してから検証 (on), ラウンド上限 (1–6, default 2), 開始 with the refusal note, 過去の検証 (as review's past list: target, verdict tag, criteria tally 検証済 4 · 不合格 1 · 判定不能 1, age, state). Installed agents as review's.
2. **Board**:
   - Header: target label, status, verdict tag; コピー, レポート保存, 新しい検証.
   - Progress strip: 「ラウンド n / max · 独立検証｜追試」, steps 基準策定 → 配信 → 検証 → 証跡収集 → 判定, per-agent badges, re-run progress (再実行 3 / 5), 停止中 / 基準確認待ち.
   - Left cards: target card (base…head, files, +/−, isolation mode, スナップショットを開く); one card per verifier (agent · model · effort, strategy tag, badge, results this round ✓ n ✗ n ⊘ n, ペインを開く, ワークツリーを開く); the judge's card.
   - Center tabs:
     - **基準**: before round 1 (held), the editable criteria list (L1-2). After: the **criteria × verifier matrix**: one row per criterion (id, priority, statement, ruling tag), one column per verifier, each cell the latest result (✓ pass, ✗ fail, ⊘ blocked, – not tested) with a re-run mark (↻ matched, ≠ differed) and a cross-check mark; a click selects the criterion.
     - **証跡**: every evidence item, filterable by verifier (and あなた), round, kind (コマンド / 画像 / 動画 / ファイル / 観察), criterion and re-run outcome, shown as 一覧 (kind, command or path, exit status verifier's / re-run's, digest, size) or ギャラリー (media only: a grid of placeholder cards, L1-5a, grouped by criterion, each with kind, format, caption, verifier, round, size, and ▶ with its marks for a video); 開く opens the stored copy (a log in a read-only text view with search; an image or a video in the system viewer or player, L1-5a). 証跡を追加 attaches the person's own media.
     - **不具合**: consolidated defects as review's findings board (severity, status, tally, location, claim), with repro steps.
     - **レポート**: L1-8, 暫定 until a verdict.
   - Right, the selected criterion's thread: statement, how, source, priority; per verifier per round: result, note, evidence (with re-run outcome; media as a strip of placeholder cards, a click opens it); the person's attached media; 証跡を追加; cross-checks; the judge's ruling history with rationale and cited evidence. A selected defect shows its sources, repro, cross-checks and history. Nothing selected: the judge's latest summary and the verdict.
   - Bottom: composer (全員 / each verifier / 判定役), ラウンド後に停止, 次のラウンドへ, 検証を開始 (while held for criteria), +1, +3, 再試行, 終了.
3. 修正を依頼 (review L1-9; its tab opens in the target's checkout, `workdir`) offers scopes 不合格の基準 n 件, 不具合 n 件 and the selection; the task lists each failed criterion with its failing evidence (stored log excerpts, last 80 lines) and repro steps. 同じ設定で再検証 (review L1-10) refills the start view including the frozen criteria (as user criteria, so they are kept verbatim) and adds the previous verdict and blocking list to the request.

### L1-8. Report

`export::report_markdown(&Session, base: Option<&Path>)` (the reply dir for absolute media links, `None` for relative ones), Japanese:

1. `# 検証結果`（暫定）: target (repository, base…head SHAs, merge base, files, +/−), isolation mode, panel (verifier → strategy; judge), rounds, status.
2. `## 総合判定`: decision, rationale, blocking list.
3. `## 受け入れ基準`: a table (id, priority, statement, ruling, verifiers' results ✓✗⊘–, re-run ↻/≠), then per criterion the rationale and cited evidence (command, exit, re-run outcome, digest, stored path).
4. `## 不具合`: live defects ranked, with repro and evidence.
5. `## 追試`: every cross-check (subject, verifier, outcome, note).
6. `## 証跡一覧`: every evidence item with digest and size; re-runs that differed or timed out listed first.
7. `## 基準の変更`: criteria the person added, edited or removed before round 1; criteria ruled `not_applicable` and why.
8. `## 検証の欠落`: criteria with no `pass`/`fail` result, verifiers that failed a round, `blocked` results with their reasons, re-runs not run, 共有 mode (isolation not enforced, no re-runs), `observation`-only results.
9. `## ラウンド記録`: per round, kind and the judge's summary.

Media in the report: under each criterion, images are embedded and videos are linked with their caption and marks; person-attached ones are marked （あなたが添付）. Evidence paths are relative to the judge's reply dir (`evidence/r1/codex/shots/tab.png`), where the stored copies live. `report.md` stays in the data dir, so it links each one by its absolute path, the reply dir joined with that relative path (`![caption](/…/repo/.noa/verify/ver-1/evidence/r1/codex/shots/tab.png)`), percent-encoded per component, and its first line names the reply dir the links point into. `export::report_markdown` takes that base as an argument: the golden test passes a fixed one, and 証跡ごと書き出す passes none, which gives the relative paths. Text (stored paths in the evidence list) shows the relative path.

Written to `<session dir>/report.md` after every accepted judge output and at close, as review's. レポート保存 offers two forms: レポートのみ (`<id>.md`, media as the absolute links above, which resolve only on this machine while `<repo>/.noa/verify/<id>/` exists, said in a line at its top) and 証跡ごと書き出す (a chosen folder receives `report.md` with links relative to the folder and a copy of every cited evidence file at its path relative to the reply dir (`evidence/r<N>/…`, `evidence/user/…`), so the folder can be zipped and shared; no link is followed and no existing file is overwritten).

### L1-9. Strategies (手法)

| id | Label | Checklist (brief text, summarized) |
|---|---|---|
| `conformance` | 仕様適合 | exercise each criterion's `how` as written; record the command and its output; stop at the first deviation and record it |
| `falsify` | 反証 | try to make each criterion fail: boundaries, invalid input, empty/huge data, concurrency, interrupted runs; write throwaway tests in your worktree; a criterion you could not break is a pass only with the attempts as evidence |
| `regression` | 回帰 | run the repository's own checks (AGENTS.md / CLAUDE.md commands, CI scripts) and the tests of code the diff touches; compare with the base when a failure may be pre-existing (`git stash` is not allowed; check out the base in a second detached worktree of your own under your worktree's `.noa/`) |
| `e2e` | 実機・E2E | build and run the product as a user would (GUI, CLI, server); capture screenshots or transcripts as `file` evidence |
| `nonfunctional` | 非機能 | measure what the criteria or the change imply: time, memory, CPU, output size, security-relevant behaviour; give numbers with the command that produced them |

カスタム: free text 1–500 characters (`custom`, `custom-2`, …). Default panel: claude/仕様適合, codex/反証, claude/回帰 (`claude-2`). A verifier's strategy shapes its brief; every verifier still reports a result for every criterion in round 1.

### L1-10. Storage, locking, resume, flags, Noa integration

- Root `<data dir>/noa/verifications`, id `ver-<millis>`:

  ```text
  <data dir>/noa/verifications/<id>/
    events.jsonl  report.md
  <repo>/.noa/verify/<id>/         judge's reply dir (c/criteria.json, r<N>/judge.json, fix-<n>.md)
    target/                        manifest.json, diff.patch, files/<path> (post-images), worktree.patch, untracked/, spec.md
    evidence/r<N>/<verifier>/      ingested logs and files (evidence/r<N>/manifest.md beside them)
    evidence/r<N>/<verifier>.rerun/  re-run logs, <n>.log (L1-5 step 5)
    evidence/user/                 person-attached media
  <main>-wt/verify-<id>-<verifier>/.noa/verify/<id>/r<N>/<verifier>/   verifier reply dirs
  ```

  `<repo>` is the target's checkout top level (the session's `workdir`), `<main>` the repository's main worktree (`main_worktree`); they differ only when the target is a linked worktree.

  The snapshot sits in the judge's reply dir, not the data dir, as review's does whenever a session has a workdir (`store::target_dir(reply_dir)`, review L1-7), so a judge sandboxed to the repository can read it; a verification always has one (git only). The ingested evidence sits beside it for the same reason (Decision 11); the verifier worktrees removed at close hold none of it. Each `.noa/` (the repository's and each worktree's) gets its `.gitignore` holding `*` from `store::prepare_workdir`, as review's.

- Locking, delivery (`refuseShell`, 128 KiB brief spill to a file), resume refusals, read-only stored view, stored views follow their log: as review L1-7. Resume additionally refuses when `target/spec.md` no longer hashes as the logged spec file (`capture::verify` checks it with the manifest and the recipe), and when a recorded worktree exists but is not a detached worktree of the repository at the head SHA (named), and rebuilds a missing one from the recipe.
- Flags: `verify [--demo | --demo-start | --demo-held | --demo-criteria | --session=DIR | --repo=DIR] [--tab=criteria|evidence|defects|report] [--criterion=ID] [--snapshot=FILE.png [--size=WxH]]`.
- Noa integration: exactly the files review L1-8 lists, with `AppCommand::OpenVerify`, `OPEN_VERIFY_MENU_ID = "noa.view.open-verify"`, action `verify.open`, palette title "Open Verify", View → "Verify" after "Review", `helper_program::VERIFY` (`Noa Verify`, crate `verify`) receiving `--repo` as review does, `scripts/verify.sh` and `scripts/bundle-macos.sh` entries, `AGENTS.md`, `docs/FEATURES.md`. Two places review L1-8's table does not list also change: `crates/noa-app/src/commands/tests.rs` (a test like `the_review_widget_is_addressable_by_action_and_menu_id` for `verify.open` / `noa.view.open-verify`), and `helper_program::launch_args`, which today passes `--repo=<cwd>` only when `helper.crate_name == REVIEW.crate_name`, so it must match `VERIFY` too (its test `only_the_review_is_given_the_focused_panes_directory` and the module doc change with it).

## L2: Detail

### L2-1. Crate layout

```text
widgets/verify/
  Cargo.toml  Cargo.lock  .gitignore
  src/lib.rs            library: capture, worktree, evidence, media, rerun, engine, fsx, store, model; #[cfg(test)] testkit
  src/main.rs           binary: demo, installed, live, `session` (= src/live/ via #[path = "live"]), ui — as review's main.rs
  src/demo.rs  src/installed.rs  src/fsx.rs  src/testkit.rs
  src/capture.rs        git capture + rebuild recipe (from review)
  src/worktree.rs       add / rebuild / check / remove (I/O); naming and porcelain parsing pure
  src/evidence.rs       inspect cited files for validation, ingest, attach, digest, manifest.md, 証跡ごと書き出す copy (I/O)
  src/media.rs          magic-byte sniffing, caps, marks parsing (pure)
  src/rerun.rs          run.sh generation and digests (pure); watching .pid/.exit/done, liveness, timeout kill, log ingest (I/O)
  src/engine.rs  src/engine_tests.rs   pure state machine (criteria phase, rounds, ingest step)
  src/store.rs
  src/model/{mod,types,reduce,brief,validate,validate_tests,export,strategy,rules}.rs   rules.rs = L1-6 judge evidence standard
  src/live.rs  src/live/{ipc,runner,fix}.rs
  src/ui/{mod,start,criteria,progress,cards,board,matrix,evidence,defects,thread,report,composer,fix,tests}.rs
  tests/cli.rs  tests/resume.rs  tests/golden.rs  tests/golden/{scenario.rs,briefs.txt,report.md}
```

`Cargo.lock` starts as a copy of `widgets/review/Cargo.lock` with the package renamed (`name = "review"` → `"verify"` in the root `[[package]]`), then one `cargo metadata --offline --format-version 1 >/dev/null` in `widgets/verify`, which only moves that entry to its sorted place (checked 2026-11-24 on a copy: `--locked` accepts the renamed copy either way, and a run without `--locked` re-sorts it and changes nothing else); `Cargo.toml` has exactly review's `[dependencies]` and `[target.'cfg(target_os = "macos")'.dependencies]`. No new third-party crate: process timeouts with `std::process::Child::try_wait` polling, signals and `kill(pid, 0)` through `libc` (already a dependency), the nonce from `/dev/urandom`, opening files with `open` as a subprocess.

### L2-2. Types (new or changed vs. review)

```rust
pub enum TargetKind { Diff, State }
pub struct Rebuild { pub head_sha: String, pub worktree_patch: Option<u64 /*digest*/>, pub untracked: Vec<(String, u64)> }
pub enum Isolation { Worktree, Shared }
pub enum Priority { Must, Should }
pub enum CriterionSource { User, Request, Inferred, Spec { line: u32 } }
pub struct Criterion { pub id: String, pub statement: String, pub how: String,
                       pub priority: Priority, pub source: CriterionSource, pub edited: bool }
pub enum Outcome { Pass, Fail, Blocked, NotTested }
pub struct CriterionResult { pub round: RoundNo, pub part: PartId, pub criterion: String,
                             pub outcome: Outcome, pub note: String, pub evidence: Vec<EvidenceKey> }
pub enum EvidenceBody {
    Command { cmd: String, cwd: String, exit: i32, log: String, rerun: bool },
    File { path: String, caption: Option<String>, media: Option<Media>,
           marks: Vec<Mark>, shows: Vec<String> },
    Observation { text: String },
}
pub enum Media { Png, Jpeg, Gif, Webp, Mp4, QuickTime, Webm }   // from magic bytes
pub struct Mark { pub t_ms: u64, pub note: String }
pub struct Stored { pub path: PathBuf, pub bytes: u64, pub digest: u64 }
// What the runner found at a cited path when a reply was read (evidence::inspect),
// passed in with the reply so validation (L1-6) stays pure:
pub enum FileState { Regular { bytes: u64 }, Missing, Link, NotRegular }
pub struct FileFact { pub path: String, pub state: FileState,
                      pub sniffed: Option<Media> /* magic bytes */, pub by_ext: Option<Media> }
// `log` on every variant: the stored `<n>.log`, `None` when none was written or it could not be stored.
pub enum Rerun { Matched { exit: i32, log: Option<Stored> }, Differed { exit: Option<i32>, log: Option<Stored> },
                 TimedOut { log: Option<Stored> }, NotRun { why: String, log: Option<Stored> } }
pub struct Attachment { pub round: RoundNo, pub at: Millis }   // person-attached: the round current then
pub struct Evidence { pub key: EvidenceKey, pub body: EvidenceBody,
                      pub stored: Option<Stored>, pub rerun: Option<Rerun>,
                      pub attached: Option<Attachment> }
pub enum CrossOutcome { Reproduced, NotReproduced, Disputed }
pub struct CrossCheck { pub round: RoundNo, pub by: PartId, pub subject: Subject,
                        pub outcome: CrossOutcome, pub note: String, pub evidence: Vec<EvidenceKey> }
pub enum RulingKind { Verified, Failed, Inconclusive, NotApplicable }
pub struct Ruling { pub round: RoundNo, pub kind: RulingKind, pub rationale: String, pub evidence: Vec<EvidenceKey> }
pub enum Decision { Pass, PassWithIssues, Fail, Inconclusive }
pub struct Verdict { pub decision: Decision, pub rationale: String, pub blocking: Vec<String> }
pub enum Phase { Criteria, Confirming, Briefing, Verifying, Ingesting, Ruling, Closed }
// Defect / Finding fold copied from review with `repro: String` added.
```

### L2-3. Events

`Created { id, request, target, manifest, rebuild, isolation, spec, user_criteria, workdir, main_worktree, max_rounds, judge, verifiers, confirm, at }`, `CriteriaAccepted { criteria }`, `CriteriaEdited { op: Add|Edit|Remove|Move, … , at }`, `CriteriaFrozen { at }`, `WorktreeMade { part, path }`, `WorktreeRemoved { part }`, `RoundStarted`, brief lifecycle events, `ReplyRecorded { round, part, summary, environment, results, evidence, defects, cross_checks }`, `EvidenceIngested { round, part, items: Vec<(EvidenceKey, Stored)>, tails }` (`tails`: the last 40 lines of each stored log of a command reported with a non-zero exit status, read by the runner at ingest), `RerunRequested { round, part, nonce, items, script_digest }`, `RerunFinished { key, rerun }`, `EvidenceAttached { key, round, body, stored, shows, at }` (person-attached media), `JudgeAccepted { round, output }`, `PhaseChanged`, `HoldSet`, `RoundsExtended`, `SessionClosed`, `MessagePosted`, `PaneAssigned`.

The engine stays pure: ingest and re-runs are I/O the runner performs on `Effect::Ingest { round }` / `Effect::Rerun { part }` (write the script, issue the re-run brief) and reports back as events; the engine moves Ingesting → Ruling when every re-run of the round has an outcome.

### L2-4. Briefs

English, constant section headings (as review's):

- Judge, criteria: `## 1. Verification request` · `## 2. Target` · `## 3. Spec` · `## 4. Criteria the person gave` · `## 5. How to write criteria` (checkable, one behaviour each, `how` concrete enough to run) · `## 6. Your output`.
- Verifier, round 1: `## 1. Request and round` · `## 2. Target and your worktree` (path, built from which SHA and patch, edits allowed only here, own build cache) · `## 3. Criteria` · `## 4. Your strategy` · `## 5. Evidence` (save every command's output under `logs/`, mark `rerun: true` for commands that should give the same exit status when run again without your session state; do not touch the worktree after writing the reply) · `## 6. Rules` (blind, no other panes or worktrees, no push/publish, mark unverified) · `## 7. User interventions` · `## 8. How to reply`.
- Verifier, round ≥ 2: 1–4, then `## 5. Rulings and results so far` · `## 6. Your assignments` · `## 7. Evidence` · `## 8. User interventions` · `## 9. How to reply`.
- Judge, ruling: `## 1. Request and round` · `## 2. Criteria` · `## 3. Panel and receipts` · `## 4. Results this round` (per criterion, per verifier, evidence with stored digest and re-run outcome; log tails of failing commands, last 40 lines (`JUDGE_TAIL_LINES`), the ones `EvidenceIngested` logged) · `## 5. Cross-checks` · `## 6. Defects` · `## 7. Evidence standard` (L1-6 rules verbatim) · `## 8. This round` · `## 9. Your output`.
- Re-run brief (verifier, between its reply and the judge): `## Re-run` only — the one command line of L1-5 step 3, the reminder not to edit `rerun/` or the worktree, and "reply nothing". Short on purpose, so it fits a paste and does not restart the verifier's work.

## L3: Acceptance Criteria

Unit and golden tests as review's; GUI behaviour against a live Noa is verified by hand (AC-26…28, AC-34, AC-35).

1. AC-1 Capture on a temp repository writes the review-compatible snapshot plus `worktree.patch` and `untracked/` in work-tree mode; the person's `git status` and index are unchanged (test).
2. AC-2 A non-git directory, an empty diff (Diff kind), an untracked binary over 1 MiB and each cap refuse the start with the reason (test).
3. AC-3 `worktree::add` builds a detached worktree whose tracked and untracked files match the manifest digests, from a ref and from work-tree mode; a tampered patch refuses round 1 naming the files; no branch is created (test).
4. AC-4 `worktree::remove` removes only a detached worktree of the repository at the recorded path, and leaves (and names) one that was re-pointed to a branch or moved (test).
5. AC-5 Criteria validation: every person-given line appears once with `source: user`; ids without gaps; 0 and 31 criteria refused; each rule its own problem line (table test).
6. AC-6 With 基準を確認してから検証 on, the session holds after criteria; edits are events; a `user` criterion cannot be removed; 検証を開始 freezes them; with it off round 1 starts at once (engine test).
7. AC-7 Round-1 verifier briefs name no peer, no peer worktree and no peer reply path (golden).
8. AC-8 Verifier validation: missing results per criterion, `pass` without evidence, an uncited or dangling evidence id, a log path with `..` or a link, a file over the cap, cross-checks in round 1, a missing assignment answer — each its own problem line (table test).
9. AC-9 Ingest copies each file into the judge's reply dir's `evidence/r<N>/<verifier>/` (nothing under the data dir but `events.jsonl` and `report.md`), records size and digest, writes `manifest.md`, and never follows a link swapped in after validation (test with a link race fixture).
10. AC-10 Re-run: with `run.sh` started by a fake agent in a temp worktree, a command that exits 0 twice is `matched`; one that exits differently is `differed` with both statuses; a sleeping command is `timed_out` at the limit (shortened in the test) and its process group killed; output beyond 8 MiB is cut and marked (test).
11. AC-11 Re-runs never run in the person's tree: in 共有 mode every `rerun: true` item is `not_run` with the reason, and no script or re-run brief is made (test).
12. AC-12 A command holding quotes, `$()` and newlines reaches the shell only as its `<n>.cmd` file and runs as written; `run.sh` holds no reported text; a changed `run.sh` or `.cmd`, a `done` with the wrong nonce, and a script that never writes a `.pid` each end as `not_run` with the reason (test).
13. AC-13 Judge rules: `verified` without a matched re-run or two independent passes is refused; with an unanswered `fail` refused; `observation`-only refused; `failed` without a fail result refused (table test).
14. AC-14 Verdict consistency: `pass` with a `must` not verified, or a live high defect, is refused; `fail` without a failed `must` is refused; each with the reason (table test).
15. AC-15 Default assignments: every `fail` and every single-verifier `pass` is assigned to another verifier, round-robin, never to its reporter (test).
16. AC-16 Engine: Briefing → Verifying → Ingesting → Ruling; the judge is briefed only after every re-run of the round has an outcome; a verifier that fails the round does not block it (engine test).
17. AC-17 Early close in round 1 is accepted only when every criterion is verified by two verifiers with matched re-runs (engine test).
18. AC-18 Defect ops: every raw defect placed once; merge moves sources (copied review tests adapted).
19. AC-19 Resume rebuilds a missing worktree from the recipe, takes up labelled panes, and refuses a worktree that is not detached at the head SHA (test).
20. AC-20 The report matches `tests/golden/report.md` for the golden scenario: every section of L1-8, re-runs that differed listed first, criteria changes listed (golden).
21. AC-21 Briefs match `tests/golden/briefs.txt` (golden).
22. AC-22 The criteria × verifier matrix shows the latest result per cell with re-run and cross-check marks; a click selects the criterion (UI test).
23. AC-23 Evidence tab filters by verifier, round, kind and re-run outcome; 開く opens the stored copy, not the worktree file (UI test).
24. AC-24 修正を依頼 writes a task listing each failed criterion with its stored log tail and repro steps, and opens a tab with auto approval off (test, as review's fix tests).
25. AC-25 `--snapshot` renders every view (including the gallery) (`--demo`, `--demo-start`, `--demo-held`, `--demo-criteria`, each `--tab`, `--criterion`) headlessly.
26. AC-26 (manual) From View → Verify in a live Noa, a branch of this repository is verified by three agents over two rounds, each in its own worktree; the report is written and the worktrees are removed at close.
27. AC-27 (manual) ペインを開く / ワークツリーを開く bring the pane forward / open Finder.
28. AC-28 (manual) An interrupted verification resumes from 過去の検証 with its worktrees.
29. AC-29 `scripts/verify.sh --full` builds and tests `widgets/verify`; `scripts/bundle-macos.sh` bundles and signs `Noa Verify`; `cargo test -p noa-app` passes with the new command (CI).
30. AC-30 Media validation: a PNG named `.mp4`, a video over 200 MiB, an image over 15 MiB, media without a caption, marks out of order or in a bad format, and `shows` naming an unknown criterion — each its own problem line; a JPEG, GIF, WebP, MP4, MOV and WebM fixture are each sniffed right (table test).
31. AC-31 Person-attached media: copied to `<reply dir>/evidence/user/<n>.<ext>` without following a link, hashed, logged as `EvidenceAttached`, listed in the next judge brief; refused before round 1 and after close; alone it does not satisfy `verified`, with a verifier's `pass` it does (test).
32. AC-32 A media item shows as a placeholder card (kind, format, size, caption, and for a video ▶ and its marks) in the thread strip and the gallery, no picture is drawn and no thumbnail file is written; 開く asks for the stored copy to be opened with `open`, not the worktree file (UI test with a fake opener).
33. AC-33 The report embeds images and links videos with captions and marks, by absolute path into the reply dir (golden with a fixed base); 証跡ごと書き出す writes a report with relative links, copies every cited file to its path relative to the reply dir, and refuses to overwrite (golden + test).
34. AC-34 (manual) In a live Noa, a verifier's screenshot shows as a placeholder card in the thread and the gallery, 開く opens it in the system image viewer, and a video opens in the system player.
35. AC-35 (manual) In a live Noa, a claude and a codex verifier each start `run.sh` from the re-run brief; a command using `$NOA_BIN` matches; a command longer than the agent tool's own time limit still finishes.

## Scope

In scope: L1-1 … L1-10.

Out of scope:
- Static review of the change (the review widget does it; 同じ設定で再検証 does not start a review).
- Files and document targets; non-git directories.
- A shared build cache across worktrees; running verifiers on other machines.
- Mutation testing run by the widget (the 反証 strategy may do it inside its worktree).
- The widget deciding results itself beyond re-runs (it does not parse test output).
- Posting results to GitHub or CI.
- The judge adding criteria after round 1 starts.
- Playing video inside the widget (the system player opens it), editing or annotating media, and audio-only evidence.
- Drawing image previews or making thumbnails (L1-5a: noa-gpui 0.2.1 cannot upload a picture after the window is built and has no public decoder).

## Decisions taken by default

Each can be changed before implementation:

1. 検証 means running things against criteria (dynamic), not reading (L0). This is the main assumption of the design.
2. Criteria come first, from the judge, and the person confirms them by default, because a ruling is only as good as what it is measured against.
3. One detached worktree per verifier by default; 共有 mode exists for trees too large to check out several times.
4. Re-runs run in the verifier's own pane (the person's decision, 2026-11-24), started by the verifier's agent from a widget-written script, so they see the pane's environment, the agent's shell and its sandbox; the cost is that the agent mediates (L1-5, Trust), and independence comes from cross-checks.
5. `verified` needs a matched re-run or two independent passes; the widget enforces it, the judge cannot rule past it.
6. Evidence is copied into the judge's reply dir (Decision 11) and hashed at ingest, so the report stays checkable after the worktrees are removed.
7. Round limit default 2 (independent + one cross-check round), reply timeout 45 min.
8. The judge runs nothing; verifiers run everything.
9. Worktrees are removed with `--force` at close (they hold only the verifier's throwaway edits), after checking each is the one the log recorded; interrupted sessions keep them.
10. Media show as placeholder cards and open in the system viewer; the window draws no picture (L1-5a). Real previews would need a decoder crate (a new dependency) and a way to upload pictures after the window is built.
11. Ingested evidence is kept in the judge's reply dir, `<repo>/.noa/verify/<id>/evidence/`, beside `target/`, not in the data dir; `events.jsonl` and `report.md` stay in the data dir (the orchestrator's decision, 2026-11-24). Reason: the judge is told it may open images by path (L1-5a), and a judge whose sandbox reaches only the repository cannot read the data dir; this is the same reason review keeps its snapshot in the workdir. Whether codex's sandbox in particular can read the data dir was not checked. The cost: `report.md` links media by absolute path into the repository (L1-8), and deleting `<repo>/.noa/` loses the evidence while the log remains.

## Needs the maintainer's agreement before implementation

- `AGENTS.md` (Safety): implementing adds `widgets/verify/Cargo.lock` and changes `scripts/verify.sh` and `AGENTS.md`, as the review widget did.
- Decision 9 (`git worktree remove --force` on widget-made worktrees).

## Implementation plan

Five units, each built by one agent in its own worktree. No two units edit the same file. Unit 1 lands first; units 2–4 start from it, unit 5 can start at once.

```text
U1 skeleton ──┬── U2 core ──┐
              ├── U3 io ────┼── U4 app (tests go green after U2 + U3 merge)
              │             │
U5 integration (noa-app part any time; widget checks after U1; AC-29 after U1–U4)
```

### Rules for every unit

- Copy from `widgets/review` (and `widgets/competition/src/check.rs` for the manifest), then adapt as this spec says. Do not depend on another widget's crate.
- After U1 merges, these files are **frozen**: `Cargo.toml`, `Cargo.lock`, `.gitignore`, `src/lib.rs`, `src/model/mod.rs`, `src/model/types.rs`, `src/fsx.rs`. No unit edits them. Behaviour on a shared type goes in an `impl` block in the unit's own file (Rust allows inherent impls in any module of the crate).
- Every `pub` item U1 writes (name, signature, fields, variants) is a contract. Units fill in bodies and may add private items, `pub` items in their own files, and their own tests. They do not change a U1 signature. A unit that needs a change to a frozen file or to a U1 signature stops that part, writes `#TODO(agent): <change and why>` in its report, and works around it inside its own files where it can. The change then lands as one follow-up commit after the units merge.
- No new dependency, no `cargo update`. Every command runs with `--locked --offline` inside `widgets/verify`.
- Tests that need another unit's code (still `todo!()` before that unit merges) go into that later unit, not into this one.
- Done means: the done commands below pass, the unit's ACs have passing tests named `acNN_…` (as review's tests are named), and the branch is rebased on the branch U1 merged into.

### U1 `skeleton` (start: now; others wait for its merge)

- Creates: `widgets/verify/{Cargo.toml, Cargo.lock, .gitignore}` (L2-1: review's dependencies, lock renamed and re-sorted; `.gitignore` = `/target`, `*.png`); `src/lib.rs` (the library modules of L2-1, `#[cfg(test)] pub(crate) mod testkit;`); `src/main.rs` (just `fn main() {}`; U4 owns it after this); `src/model/mod.rs`; `src/model/types.rs`; `src/fsx.rs` (review's, verbatim, with its tests); and one stub file for every other library module of L2-1 (`capture`, `worktree`, `evidence`, `media`, `rerun`, `engine`, `store`, `model/{reduce,brief,validate,export,strategy,rules}`, `testkit`).
- `types.rs` holds **every** shared type, written in full with serde derives: review's types that are kept (`string_id!`, `SessionId`, `PartId`, `MessageId`, `AgentKind`, `AgentSetup`, `AgentChoice`, `Participant` with `strategy`, `Role { Judge, Verifier }`, `Head`, `TargetManifest` and its file types, `Severity`, `Location`, the defect/finding fold types with `repro`, `Receipt`, `Brief` with `BriefKind { Work, Rerun }`, `Round`, `RoundKind`, `Message`, `Session`), every type of L2-2 including `FileFact`/`FileState`, `Strategy` (L1-9 built-ins plus custom), the constants (`MAX_CRITERIA` 30, `DEFAULT_MAX_ROUNDS` 2, `MAX_MAX_ROUNDS` 6, `MIN_PANEL` 2, `MAX_PANEL` 6, `JUDGE_ID` `"judge"`, the media caps, `MAX_RERUNS` 8, `RERUN_TIMEOUT`), and the **`Event` enum of L2-3** (here, not in `reduce.rs` as review has it, so it is frozen with the other types; `reduce.rs` re-exports it as `pub use crate::model::types::Event;`).
- Each stub module holds its full `pub` surface with doc comments and `todo!()` bodies. Shared constants (`capture::DIFF_FILE`, `FILES_DIR`, `MANIFEST_FILE`, `WORKTREE_PATCH`, `UNTRACKED_DIR`, `SPEC_FILE`, `store::EVENTS_FILE`, `REPORT_FILE`, `TARGET_DIR`, `EVIDENCE_DIR` = `"evidence"` (under the reply dir, beside `TARGET_DIR`), `USER_EVIDENCE_DIR` = `"user"`, …) get their values. A body of a few lines that several units call is written in full: `capture::digest_bytes` (FNV-1a 64, review's), `store::digest`, path helpers such as `store::session_dir`, `reply_dir`, `target_dir`, `evidence_dir(reply_dir)` (`<reply dir>/evidence`), `rerun_log_path` (`<reply dir>/evidence/r<N>/<verifier>.rerun/<n>.log`), `worktree::path_for`; and `media::parse_mark`, which U2's reply validation calls (AC-30), with its unit tests. The surfaces to write:
  - `engine`: `Config` (start 90 s, reply 45 min, 2 retries), `AgentState`, `Input` (review's plus `ReplyFileWritten { …, files: Vec<FileFact> }`, `CriteriaEdit`, `UserStartVerify`, `WorktreeMade`/`WorktreeFailed`, `Ingested` (with the failing logs' tails), `RerunReported`, `AttachEvidence`), `Effect` (review's plus `MakeWorktrees`, `Ingest { round }`, `Rerun { part }`, `RemoveWorktrees`), `Output`, `EngineError`, `Setup`, `Engine` with review's methods, `participants`, `check_panel`, `check_resumable`, `check_store_paths`.
  - `capture`: `capture`, `repo_root`, `default_base`, `verify` (review's, plus the rebuild recipe and the spec file). `worktree`: `main_worktree`, `add`, `check`, `remove`, `list` (porcelain parse). `evidence`: `inspect`, `ingest`, `attach`, `write_manifest`, `export_bundle`. `media`: `sniff`, `by_extension`, `parse_mark` (in full), caps. `rerun`: `script`, `write_request`, `poll` (outcome per item, timed by a `Watch` the runner keeps per script, killing past the limit), `kill_group`, `ingest_logs`, the per-`AgentKind` detached/foreground constant.
  - `store`: review's surface (`Store`, `list_sessions`, `page_sessions`, `Summary`, `Listed`, `prepare_workdir`, reply/report paths, `write_fix_task`). `model::reduce`: `create`, `apply`, `replay`. `model::validate`: `parse_criteria`, `parse_reply`, `parse_judge`, `may_close` (given the round's draft rulings). `model::rules`: the evidence standard and verdict check. `model::brief`: one function per brief of L2-4 plus `correction_note`. `model::export`: `report_markdown`, `fix_task`, `failures`. `model::strategy`: labels and checklists.
- Spec: L2-1, L2-2, L2-3 (types only). ACs: none.
- Done: `cargo build --locked --offline --all-targets` and `cargo test --locked --offline` (fsx's tests plus a serde round trip of `Event` and `Session`) pass in `widgets/verify`; `git diff --stat` shows only `widgets/verify/`.

### U2 `core` (waits for U1)

- Edits (fills): `src/model/{reduce,validate,rules,strategy,brief,export}.rs`, `src/engine.rs`, `src/store.rs`, `src/testkit.rs`. Creates: `src/model/validate_tests.rs`, `src/engine_tests.rs`, `tests/golden.rs`, `tests/golden/{scenario.rs,briefs.txt,report.md}` (blessed with `VERIFY_BLESS=1`, as review's `REVIEW_BLESS`).
- Spec: L1-2; L1-4 (phases, rounds, default assignments, early close, timeouts); L1-5 validation rules and the engine side of ingest and re-runs (what moves Ingesting → Ruling, `not_run` in 共有 mode); L1-5a media validation over `FileFact`s (marks through `media::parse_mark`, U1's), what the briefs say about media, the `EvidenceAttached` rule; L1-6; L1-8 report text; L1-9; L1-7 item 3 task text (`export::fix_task`); L1-10 store, locking and the engine's resume checks; L2-3; L2-4. The screenshot TODO of L1-5a stays open: write the brief with its fallback (report `blocked` with the reason).
- ACs: 5, 6, 7, 8, 11, 13, 14, 15, 16, 17, 18, 20, 21; and the model halves of 24 (task text), 30 (one problem line per media rule, given `FileFact`s), 31 (event, next judge brief, counts only with a verifier's `pass`), 33 (report embeds images, links videos with captions and marks: golden).
- May assume: U1's types and signatures. Must not call `capture`, `worktree`, `evidence`, `media` or `rerun` bodies in its tests (stubs until U3 merges), except the bodies U1 wrote in full (such as `media::parse_mark`): tests feed `FileFact`s, `Stored`s and re-run outcomes as inputs. `engine::check_resumable` calls `capture::verify` and `worktree::check`; its tests are U4's (AC-19).
- Done: `cargo build --locked --offline --all-targets`, `cargo test --locked --offline` and `cargo test --locked --offline --test golden` pass.

### U3 `io` (waits for U1)

- Edits (fills): `src/capture.rs`, `src/worktree.rs`, `src/evidence.rs`, `src/media.rs`, `src/rerun.rs`, each with its own `#[cfg(test)]` tests (temp git repositories, real `git` and `sh`).
- Spec: L1-1; L1-3 (main worktree lookup, worktree add / rebuild / check / remove, naming at `<main>-wt/verify-<id>-<verifier>`); L1-5 ingest, `run.sh` and `.cmd` writing, nonce, digests, watching `.pid` / `.exit` / `done`, liveness, timeout kill of the process group, log caps; L1-5a sniffing, caps, attach copy (marks parsing is U1's `media::parse_mark`, written in full; U3 leaves it as it is); L1-8 証跡ごと書き出す copy.
- ACs: 1, 2, 3, 4, 9, 10, 12; and the I/O halves of 19 (`worktree::check` refuses a worktree that is not detached at the head SHA; rebuild from the recipe), 30 (each fixture sniffed right; a PNG named `.mp4` reported as a mismatch), 31 (copy to `<reply dir>/evidence/user/<n>.<ext>` without following a link, hashed), 33 (export copies every cited file at its path relative to the reply dir and refuses to overwrite).
- May assume: U1's types, constants and `fsx`. Calls nothing of U2 (no engine, no reducer).
- Done: `cargo build --locked --offline --all-targets` and `cargo test --locked --offline` pass. If a test needs something the sandbox blocks (it should not: temp dirs, `git`, `sh`, signals to its own children), say which in the report.

### U4 `app` (waits for U1; its tests go green only after U2 and U3 merge)

- Edits: `src/main.rs`. Creates: `src/demo.rs`, `src/installed.rs`, `src/live.rs`, `src/live/{ipc,runner,fix}.rs`, `src/ui/{mod,start,criteria,progress,cards,board,matrix,evidence,defects,thread,report,composer,fix,tests}.rs`, `tests/cli.rs`, `tests/resume.rs` (resume through the library API: `engine::check_resumable` with real captures and worktrees).
- Spec: L1-3 runner side (when worktrees are made and removed, ワークツリーを削除); L1-4 items 6–8 (interventions, hold, extend, panes); L1-5 runner side (issuing the re-run brief, polling through `rerun::poll`, kills, ingest calls); L1-5a viewing (placeholder cards, `open`, Finder で表示) and 証跡を追加 (file chooser and dropped files, both in noa-gpui 0.2.1: `FileDialogRequest`, `DroppedFile`); L1-7 UI; L1-10 flags, resume, read-only view, stored views follow their log, `--repo`.
- ACs: 19 (resume flow end to end), 22, 23, 24 (the request: task file written through `fsx`, tab opened with auto approval off; test with review's fake connection), 25, 32. Manual, closed by hand in a live Noa and not by tests: **26, 27, 28, 34, 35** (AC-35 also settles the nohup TODO of L1-5: if a kind kills detached processes, its per-kind constant in `rerun.rs` changes in a follow-up commit after the units merge).
- May assume: U1's surfaces. Until U2 and U3 merge it builds against stubs, so only the build is checked; then it rebases onto them and runs the tests.
- Done: before U2 + U3 merge, `cargo build --locked --offline --all-targets`; after, `cargo test --locked --offline` (UI tests expect macOS system fonts, as review's) and `cargo run --locked --offline -- --demo --snapshot=$TMPDIR/v.png` for each `--demo*` and `--tab` value without a panic.

### U5 `integration` (noa-app part: start now; widget checks: after U1; AC-29: after U1–U4)

- Edits: `crates/noa-app/src/commands/command.rs`, `crates/noa-app/src/commands/tests.rs`, `crates/noa-app/src/command_palette.rs`, `crates/noa-app/src/app/commands.rs`, `crates/noa-app/src/app/helpers/dispatch.rs`, `crates/noa-app/src/split_tree/tree/commands.rs`, `crates/noa-app/src/macos_menu.rs`, `crates/noa-app/src/helper_program.rs` (including `launch_args` passing `--repo` to `VERIFY`), `scripts/verify.sh` (`verify=widgets/verify/Cargo.toml`, `run verify widget_check` in `--full`, `widgets/verify/*` and `widget_uses` in the changed-path mode), `scripts/bundle-macos.sh` (build loop, `VERIFY_BIN`, `-x` check, copy to `Contents/Helpers/Noa Verify`, `codesign`), `AGENTS.md` (Verification list), `docs/FEATURES.md` (a **Verify** entry after Review). Nothing under `widgets/verify/`.
- Spec: L1-10 Noa integration (and review L1-8's table, which it follows).
- ACs: 29.
- Done: `cargo test -p noa-app --offline` and `cargo clippy --workspace --offline` pass (outside the sandbox if `noa-ipc`/`noa-pty` tests hit `PermissionDenied`, as CLAUDE.md says); after U1 merges, `scripts/verify.sh` passes with the widget; AC-29 closes when `scripts/verify.sh --full` and `scripts/bundle-macos.sh` pass with U1–U4 merged.
