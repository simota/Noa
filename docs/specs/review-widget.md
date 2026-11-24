# Review widget (thorough multi-agent review): Spec

| | |
|---|---|
| slug | review-widget |
| status | draft (design only, 2026-11-24). No code yet. No owner sign-off |
| owner | simota |
| as-of | 2026-11-24 (HEAD 309e44e4, branch `orch/review-widget`) |
| crate | `widgets/review` (binary `review`, bundled as `Noa Review`) |
| request | 「ディスカッション、コンペ機能を元に複数エージェントでの徹底レビューウィジェットを作成してください。多角的なフィードバックとレポートが行えるようにしてください。」 |

## L0: Vision

- Problem: a change, a set of files or a plan gets reviewed by one agent at a time, from one angle. The discussion widget (`widgets/discussion`) makes agents talk, and the competition widget (`widgets/competition`) makes them work blind and get scored, but neither produces review findings: located, ranked defects that several reviewers checked against each other.
- Job to be done: pick what to review, seat 2–6 reviewer agents, each on a lens (correctness, security, …), let each review alone, then let them see and challenge each other's findings, and get one consolidated, de-duplicated list with each reviewer's stance on each item, a verdict, and a Markdown report.
- Success definition: from View → Review, a branch diff is reviewed by three agents over three rounds and ends in a verdict and `report.md`, with every finding a reviewer wrote either in a consolidated finding or dismissed with a reason (none dropped silently).

## Reuse decision

`widgets/discussion` and `widgets/competition` are independent crates outside the workspace, each with its own `Cargo.lock`. The review widget is a third independent crate. It does not depend on either, and no shared crate is extracted:

- The copies have already diverged: competition's `Receipt` has `rejected_digest`, its rejection messages are Japanese and discussion's English, discussion's `Brief` has `since`/`through` deltas and competition's does not, competition's store carries a `LOCKS_FILE`/`LEGACY_QUIET` path for widgets from before the lock. A shared crate would mean migrating both shipped widgets and their byte-for-byte golden tests: a refactor of two widgets, not part of this request.
- What would be shared is about 600 lines of types and store boilerplate (agent kinds, setups, receipts, the log). Copying it costs less than a fourth crate with its own gate entry in `scripts/verify.sh`.

What is copied and from where (adapted, then owned by `widgets/review`):

| Piece | Source | Adaptation |
|---|---|---|
| `string_id!`, `AgentKind`, `AgentSetup` (+`check`, `flags`, `label`), `AgentChoice`, `Participant`, `Receipt` (with `rejected_digest`), `Brief` (no `since`/`through`), `Round`, `Millis`, `RoundNo` | `competition/src/model/types.rs` | `Participant` gains `lens: Option<Lens>`; `Role` is `Lead`/`Reviewer` |
| Brief lifecycle events (`BriefIssued/Sent/Started/Retried/Failed/Reset`, `ReplyRejected{digest}`, `ReplyAccepted`, `PhaseChanged`, `HoldSet`, `RoundsExtended`, `SessionClosed`), `create`/`apply`/`replay` | `competition/src/model/reduce.rs` | new events for findings (L2) |
| Engine shape: Briefing → parallel work → one evaluator → Closed; start/reply timeouts, retries, correction notes, `Redeliver`, hold/continue/extend, `retryable`, `extendable`, `check_resumable` | `competition/src/engine.rs` | the evaluator is the lead; per-reviewer questions |
| The evaluator asks each worker a next question; rounds-left notes; `correction_note` | `discussion/src/model/brief.rs` (`moderator_brief`, `questions`, `convergence_note`) | worded for review |
| `ValidationError`, `strip_fence`, `list`, `is_valid_issue_id` | `discussion/src/model/validate.rs` | |
| `Store` (exclusive `flock` on `events.jsonl`, `LOCK_GRACE`, torn-line cut, `read_session`, `is_busy`, `list_sessions` → `Listed`/`Summary`), `prepare_workdir`, `workdir_reply_dir`, `reply_dir`, `digest` | `competition/src/store.rs` | no `LOCKS_FILE`/`LEGACY_QUIET`/`MaybeRunning`: no review widget from before the lock exists; no artifact snapshot, viewer or checks |
| `live.rs` (`Channel`, generation counter, `Command`, `Leave`, `Resume`, `Focus`, notices, `released`) and `live/ipc.rs` (the `Noa` port, auto approval on the session's tab) | competition | commands per L1-7 |
| `live/runner.rs`: connect/backoff, spawn under the login shell, Claude trust prompt, paste readiness, reply polling (`REPLY_STABLE_FOR`), 3×3 pane grid (`placements`), resume taking labelled panes back | competition | minus snapshot/check/preview/viewer/adopt; plus target capture at start |
| `main.rs` flag handling (`--demo`, `--demo-start`, `--demo-held`, `--session`, `--snapshot`, `--size`), locale title | discussion (flag style) + competition (`read_session` before the window opens) | plus `--tab`, `--finding` |
| Start view panel rows (kind / model ComboBox / effort PopUp, 2–6 rows, remove/add), past list, progress strip, composer, agent cards with ペインを開く | `competition/src/ui/{start,progress,composer,leaderboard,judge}.rs` | lens column, target picker |
| Stance tags, thread-like message rendering | `discussion/src/ui/{chat,tree}.rs` | |

New code: target capture (`capture.rs`), the finding model and its fold, the reviewer/lead briefs and validation, the findings board, matrix/coverage, finding thread and report views, and the report export.

## Glossary

| Term | Meaning | Identifier |
|---|---|---|
| review | one session: a target, a panel, rounds, findings, a verdict | `Session`, id `rev-<millis>[-n]` |
| target | what is reviewed, captured once into a snapshot | `Target`, `TargetManifest` |
| snapshot | the captured copy of the target every agent reads | `<reply dir>/target/` |
| reviewer | an agent that writes findings and stances | `Role::Reviewer`, ids `claude`, `codex`, `agy`, `claude-2`, … |
| lead (リード) | the agent that consolidates each round and gives the verdict; writes no findings of its own | `Role::Lead`, id `lead` |
| lens (観点) | the angle a reviewer is asked to review from | `Lens` |
| raw finding | one finding as a reviewer wrote it | `RawFinding`, key `<reviewer>/<id>` (e.g. `codex/3`) |
| finding (指摘) | a consolidated finding the lead keeps, made of one or more raw findings | `Finding`, id chosen by the lead (`F1`, `F2`, …) |
| stance (スタンス) | a reviewer's position on a finding in a cross-examination round | `Stance` |
| verdict (総合判定) | the lead's decision on the target | `Verdict` |

## L1: Requirements

### L1-1. What can be reviewed, and how it is captured

The start view offers three target kinds. Whatever the kind, the widget captures the target once, when the review starts, into a snapshot under `<reply dir>/target/`, and records its manifest in the log's `Created` event. Every reviewer and the lead are pointed at the snapshot, so all of them review the same bytes even if the workspace moves on. Line numbers in findings refer to the snapshot.

1. **Git 差分** (`TargetKind::Git`): a repository path (any path inside a work tree; its `git rev-parse --show-toplevel` is used, so a linked worktree works), a base ref, a head, and an optional pathspec filter.
   - Base: a ref typed by the user. When empty, the remote default branch (`git symbolic-ref --short refs/remotes/origin/HEAD`), else `main`, else `master`; none found → start refused with why.
   - Head: 作業ツリー (default: `HEAD` plus uncommitted and untracked changes) or a ref.
   - The diff runs from `git merge-base <base> <head commit>` (three-dot semantics): `git diff --no-color --no-ext-diff --find-renames <merge-base> [<head ref>] -- <paths>`. In 作業ツリー mode untracked files (`git ls-files --others --exclude-standard -z`) are added as new files. No command writes to the index or the work tree.
   - Snapshot: `target/diff.patch`, and the post-image of every changed text file under `target/files/<path>` (read from disk in 作業ツリー mode, from `git show <ref>:<path>` otherwise). A deleted file appears in the diff only.
   - An empty diff refuses the start: 「差分がありません」.
2. **ファイル / フォルダ** (`TargetKind::Files`): a root directory and an optional list of relative paths (files or directories, one per line; empty = the whole root). Inside a git work tree the files are `git ls-files -co --exclude-standard -z -- <paths>`; elsewhere a walk that skips hidden directories, `target` and `node_modules`. Copied to `target/files/<path>`.
3. **文書・計画** (`TargetKind::Document`): text typed or pasted in the start view, or one file chosen by path, plus an optional working directory given to the agents as read-only context. Saved as `target/document.md`; the original file name, if any, is in the manifest.

Common rules:
- Only text files are captured (no NUL byte in the first 8 KiB, valid UTF-8). A binary file, or one over `MAX_FILE_BYTES` (1 MiB), is left out and listed in the manifest's `omitted` with the reason.
- The start is refused, with the numbers and a hint to narrow the paths, when the target has more than `MAX_TARGET_FILES` (400) files, more than `MAX_TARGET_BYTES` (24 MiB) in total, a diff over `MAX_DIFF_BYTES` (4 MiB), or a document over 1 MiB.
- The manifest (`TargetManifest`, L2-2) holds per file: path, status (added / modified / deleted / renamed from / untracked / plain), line count, bytes, additions/deletions, the changed line ranges of the post-image (git), and a digest; plus base/head refs and SHAs, the merge base, the filter, the omitted list, the diff's digest and size, and the capture time.
- The snapshot is captured by the runner (I/O) before the `Created` event is written. A capture that fails leaves the start view with the reason in its note and starts no pane.

### L1-2. Panel and lenses

- The lead: one row of agent (claude / codex / agy), model, effort. Default claude, CLI defaults.
- Reviewers: 2–6 rows (`MAX_PANEL` = 6) of agent, model, effort and lens. A kind may sit more than once (`claude`, `claude-2`, …, as competition numbers them). Two reviewers may share a lens.
- Model and effort follow `AgentSetup::check` (plain-word model names, the kind's effort list).
- Built-in lenses, each with a Japanese label and an English checklist paragraph that goes into the reviewer's brief:

| id | Label | Checklist (brief text, summarized) |
|---|---|---|
| `correctness` | 正確性 | logic errors, edge cases, error handling, concurrency, data loss, contract violations, regressions |
| `security` | セキュリティ | validation at system boundaries, injection, authn/authz, secrets, path traversal, unsafe deserialization, privilege, dependency risk |
| `performance` | 性能 | complexity, allocations and copies on hot paths, blocking I/O, unbounded growth, caching, startup cost |
| `design` | 設計・アーキテクチャ | boundaries and layering (the repo's AGENTS.md/CLAUDE.md rules), coupling, duplication, API shape, more code than the problem needs |
| `tests` | テスト | changed behaviour without tests, weak assertions, flakiness, untested error paths |
| `ux_docs` | UX・ドキュメント | user-visible text, error messages, accessibility, docs/CHANGELOG/feature-list drift, comments |

- カスタム: the row's lens is free text (1–500 characters) the user writes; its id is `custom`, `custom-2`, … in panel order, its label the text's first 16 characters.
- Default panel: claude/正確性, codex/セキュリティ, agy/設計・アーキテクチャ.
- A finding's `category` may be any built-in lens id, any custom lens id of the session, or `other`: a reviewer focuses on its lens but is not forbidden to report outside it.

### L1-3. Rounds

A round runs Briefing (配信) → Reviewing (レビュー) → Consolidating (取りまとめ) → Closed (完了), then the next round.

1. **Round 1, 独立レビュー (`RoundKind::Blind`)**: every reviewer gets a brief with the request, the target summary, its lens and checklist, the rules and the reply format, and writes its findings to `r1/<reviewer>.json`. The brief names no peer and no peer file; it tells the reviewer not to open `r1/` files other than its own, not to contact other panes, and not to modify the workspace or the snapshot (it reviews, it does not fix).
2. **Consolidation (every round)**: once every reviewer has replied or failed, the lead gets a brief with every raw finding of the round and every finding so far with all stances, and writes `r<N>/lead.json`. It must place every raw finding of the round into a finding (`add` or `merge`) or dismiss it with a reason, set severity, confidence and status per finding, and may ask each reviewer a question for the next round. Nothing a reviewer wrote is dropped without a recorded reason.
3. **Rounds 2…, 相互検証 (`RoundKind::Cross`)**: every reviewer gets every finding with its sources (each raw finding with its reviewer, claim, evidence and fix), the lead's status and rationale, every stance so far, the lead's summary and its question to this reviewer. It must give a stance (`confirm` / `dispute` / `refine` / `unsure`) on every live finding it did not raise and has not yet taken a stance on, may revise earlier stances, may `refine` or `withdraw` its own, and may add new findings. Identities are visible from round 2 on.
4. **The last round** (`round >= max_rounds`): the lead's output must hold a `verdict`, and no finding may be left `open`: each ends `agreed`, `disputed` or `rejected` (or `withdrawn`/`merged`, L1-4).
5. **Early end**: the lead may set `close: true` (with a verdict) from round 2 on, or in round 1 when `max_rounds` is 1. The user's 終了 ends the review without a verdict.
6. Round limit: default 3, 1–10 in the start view. The lead cannot move it.
7. As in both existing widgets: ラウンド後に停止 holds after each consolidation until 次のラウンドへ (turning it off while held starts the next round); interventions (to everyone, one reviewer, or the lead) reach the next briefs; +1 / +3 raise the limit of a running review or continue one that ran all its rounds (the verdict it had stays with its round; the new last round needs a new one), not while the last round is consolidating, and not for one the lead closed early or the user ended; a stored review is extended before any pane starts. 再試行 resends failed briefs of the current phase.
8. A reviewer that fails a round (no start or no valid reply after the retries) does not stop the round; the lead's brief says "no reply: <why>", and the report lists it under coverage gaps. It is briefed again next round.
9. Timeouts and retries: `Config` as competition (start 90 s, reply 15 min, 2 retries). A resend waits while the agent works, sits on a prompt, or its reply file is on disk.
10. Panes: one Noa tab per review, opened with auto approval (agents run unattended, as competition's), lead and reviewers in splits on the 3×3 grid (at most 7 panes). Each agent runs in the workdir: the repository top level (git), the root (files), or the document's working directory when given, else the session directory.

### L1-4. Agent output formats and validation

Agents write JSON files (one Markdown code fence around the whole file is tolerated, as today). Every problem is reported precisely, one per line, and sent back in a correction note; after `max_retries` the brief is marked failed. The digest of a turned-down reply is logged (`rejected_digest`) so a copy left by a crash is not taken again. Unknown fields are rejected (`deny_unknown_fields`).

**Reviewer reply** `r<N>/<reviewer>.json`:

```json
{
  "summary": "One paragraph: what you reviewed and your overall assessment.",
  "findings": [
    {
      "id": "1",
      "location": {"path": "src/store.rs", "line": 120, "end_line": 134},
      "severity": "high",
      "category": "correctness",
      "confidence": "medium",
      "claim": "A torn last line is kept, so the next append joins two batches.",
      "evidence": "store.rs:128 returns before set_len when …; reproduced by …",
      "fix": "Truncate to `kept` before appending."
    }
  ],
  "stances": [
    {"finding": "F2", "stance": "dispute", "note": "The lock is taken at 331.", "evidence": "…", "severity": "low"}
  ],
  "coverage": {
    "reviewed": ["src/store.rs", "src/live/"],
    "skipped": [{"path": "src/ui/", "reason": "outside my lens"}]
  }
}
```

Rules (each violation is one problem line):
- `summary`: non-empty.
- `findings`: at most 50 per reply (more → asked to merge and prioritize).
  - `id`: 1–16 ASCII letters, digits, `-`, `_`; unique in the reply and not one this reviewer used in an earlier round. Stored as the key `<reviewer>/<id>`.
  - `location`: `null` (about the target as a whole) or `{path, line?, end_line?}`. `path` is relative, without `..` or a leading `/`. A path in the manifest's files: `line` in `1..=lines`, `end_line` in `line..=lines`. A path in `omitted`: no line check. Any other relative path is accepted as a context location (`in_target: false`), shown as 対象外. For a document target the path is `document.md` or the location is `null`.
  - `severity`: `critical` | `high` | `medium` | `low` | `info`.
  - `category`: a built-in lens id, a custom lens id of the session, or `other`.
  - `confidence`: `high` | `medium` | `low`.
  - `claim`: one line, 1–300 characters. `evidence`: non-empty. `fix`: optional.
- `stances`: not allowed in round 1. From round 2:
  - `finding`: a finding id that exists; at most one stance per finding per reply.
  - `stance`: `confirm` | `dispute` | `refine` | `unsure` | `withdraw`. A reviewer that raised the finding (an origin) may give only `refine` or `withdraw`; others may not give `withdraw`.
  - `note`: required for `dispute` and `refine`. `evidence`, `severity` (a proposal): optional.
  - Required: a stance on every finding whose status is `open`, `agreed` or `disputed`, that this reviewer did not raise and has no stance on from an earlier round. Missing ones are listed: `stances: missing F3, F7 (give confirm, dispute, refine or unsure)`.
- `coverage`: optional. Each `reviewed` / `skipped[].path` entry is a manifest file, a directory prefix ending in `/` that matches at least one file, or `*`.
- `reviewer`: optional; if present it must equal the reviewer's own id.

**Lead output** `r<N>/lead.json`:

```json
{
  "summary": "What this round found and what is still in dispute.",
  "ops": [
    {"op": "add", "id": "F1", "from": ["claude/1", "codex/2"], "claim": "…",
     "location": {"path": "src/store.rs", "line": 120}, "severity": "high",
     "category": "correctness", "confidence": "high", "status": "open",
     "rationale": "Both traced the same path; reproduced.", "fix": "…"},
    {"op": "merge", "id": "F1", "from": ["agy/4", "F6"]},
    {"op": "update", "id": "F2", "severity": "medium", "status": "disputed", "confidence": "low", "rationale": "…"},
    {"op": "dismiss", "raw": "agy/5", "reason": "Not a defect: the value is checked at 88."}
  ],
  "questions": {"codex": "F2: does the lock at 331 cover the resume path?"},
  "verdict": {"decision": "request_changes", "rationale": "…", "must_fix": ["F1"]},
  "close": false
}
```

Rules:
- `summary`: non-empty.
- Every raw finding recorded in this round appears in exactly one `add.from`, `merge.from` or `dismiss.raw`; the rest are listed: `unassigned raw findings: agy/5, claude/7`. A raw finding placed in an earlier round cannot be placed again.
- `add`: `id` 1–16 ASCII letters, digits, `-`, `_`, not taken (the brief suggests the next `F<n>`); `from` non-empty; `claim`, `severity`, `category`, `confidence`, `status`, `rationale` as for reviewers; `location` as for reviewers; `fix` optional.
- `merge`: `id` an existing live finding; `from` holds raw keys of this round and/or other finding ids, which become `merged` into `id` (their sources and stances move to it; the merged one keeps a pointer).
- `update`: an existing finding; changes at least one of `claim`, `location`, `severity`, `category`, `confidence`, `status`, `rationale`, `fix`.
- `status` the lead may set: `open` (未検証), `agreed` (合意), `disputed` (対立), `rejected` (却下; needs `rationale`). `withdrawn` (取り下げ) is set by the fold when every origin has withdrawn; `merged` (統合) by `merge`.
- `questions`: keys are reviewer ids; values non-empty. A reviewer with no question gets a default one.
- `verdict`: required in the last round and with `close`; not allowed otherwise. `decision`: `approve` (承認, `must_fix` empty) | `approve_with_fixes` (修正後承認, `must_fix` non-empty) | `request_changes` (要修正, `must_fix` non-empty) | `reject` (差し戻し). `rationale` non-empty. `must_fix`: live finding ids (not `rejected`, `withdrawn` or `merged`).
- With a verdict, no finding may stay `open`: `F3 is still open; set agreed, disputed or rejected`.
- `close`: not allowed in round 1 unless `max_rounds` is 1.

### L1-5. UI views (Japanese labels)

Window title 「Noa · 多角レビュー」 (Japanese locale) / "Noa · Multi-Agent Review", app name `Noa Review`. Minimum window as competition's (left 280 + center 520 + right 310 + dividers, 600 high).

1. **開始ビュー** (no session yet):
   - 対象: a segmented control 「Git 差分 | ファイル / フォルダ | 文書・計画」 and the fields of L1-1 (repository / root / document file; the document file has 選択…, an open panel (the platform's panel has no folder mode, so the repository and root are typed); base ref with placeholder 「自動（origin の既定ブランチ）」; head 「作業ツリー（未コミットを含む）」 or a ref; path filter; document text area; context working directory).
   - 依頼・重点 (optional text area): what the reviewers should know or look at first.
   - リード: agent / model / effort.
   - レビュアー: 2–6 rows of agent / model / effort / 観点 (pop-up of the six built-ins and カスタム) / custom lens text (shown for カスタム), with 追加 and a remove button per row.
   - ラウンド上限 (1–10, default 3). 開始 and a note that says why the start is refused (empty target, panel size, bad setup, capture error).
   - 過去のレビュー: the newest 12 under `<data dir>/noa/reviews`: target label, kind, verdict tag, live finding counts by severity (致命的 1 · 高 3 · …), rounds, age, and 完了 / 中断 / 他のウィンドウで実行中; an unreadable log is listed with why. 表示 opens one read-only; 再開 resumes an unfinished one that no window holds.
   - Installed agents: `src/installed.rs` asks the user's login shell (`$SHELL -l -i -c`, as agents are started) once, on a background thread, which of claude / codex / agy it can run. The lead menu, every reviewer row's menu and the default panel offer only those kinds (menu index maps through the offered list); the start button waits for the check and stays off when none is installed, so a started review has only installed kinds. 再開, and +N on a read-only stored review, are refused with the missing kinds named when its lead or a reviewer uses one that is not installed. A failed probe or a non-POSIX shell exit is `Installed::Unknown`: every kind is offered and nothing is blocked. `--demo*` and `--session` assume all installed.
2. **Board** (a session is on screen):
   - Header: target label and kind tag, status, verdict tag once given; コピー, レポート保存, 新しいレビュー.
   - **進捗ストリップ**: 「ラウンド n / max · 独立レビュー｜相互検証」, steps 配信 → レビュー → 取りまとめ → ラウンド完了 with the current one marked, a badge per agent (待機中 / 送信済 / レビュー中 / 提出済 / 失敗) with elapsed time, and 停止中 while held.
   - Left, **レビュアーカード**: one per reviewer (id, agent · model · effort, lens tag, this round's badge, raw findings written, stances given, ペインを開く) and one for the lead (リード, badge, ペインを開く). ペインを開く brings the pane forward (`noa.focusPane`); without a pane or a link it shows a notice; a read-only stored review shows no button. Above them a target card: kind, base…head short SHAs, files (+additions −deletions), omitted count, スナップショットを開く (opens `target/` in Finder).
   - Center tabs:
     - **指摘** (findings board): filters by 重大度 (致命的 / 高 / 中 / 低 / 情報), 観点 (the session's lenses and その他), 状態 (未検証 / 合意 / 対立 / 却下 / 取り下げ / 統合; default all but 却下, 取り下げ and 統合), 合意度 (全員一致 / 多数 / 対立 / 単独, L2-4), and レビュアー; 「表示 n / 全 m」. A row per finding: id, severity, lens, status, the tally (支持 3 · 反論 1 · 未表明 1), location (`path:line`, with 差分外 or 対象外 tags), claim, origins. Sorted per L2-4. A click selects it for the thread.
     - **マトリクス**: lens × severity counts of live findings (rows: the session's lenses, その他, and built-in lenses nobody holds, greyed 「担当なし」; columns 致命的…情報 and 計). A click on a cell opens 指摘 filtered to it. Below, file coverage: per target file, +/−, findings by severity, reviewers that reported it reviewed, reviewers that skipped it (with reason); a file no reviewer reported reviewing is marked 未レビュー; omitted files are listed greyed with the reason.
     - **レポート**: the report of L1-6 as it stands (headed 暫定 until a verdict), with コピー and 保存.
   - Right, **指摘スレッド** for the selected finding: id, claim, severity / lens / status / confidence, location (opens the snapshot file), the lead's rationale and fix; 出典: each raw finding (reviewer, round, proposed severity, claim, evidence, fix); スタンス: per reviewer, by round (支持 / 反論 / 補足 / 不明 / 取り下げ, note, evidence, proposed severity); 履歴: status and severity changes by round. With nothing selected: the lead's latest summary and the verdict.
   - Bottom: composer (宛先 全員 / each reviewer / リード; 送信) and ラウンド後に停止, 次のラウンドへ (while held), +1, +3, 再試行 (when something is retryable), 終了 (asks first: an alert whose default, キャンセル, answers Return and Escape, since a review ended before its last round can be neither resumed nor extended).
   - A read-only stored review has no composer and no round controls, except +1 / +3 when extendable (they resume it) and 再開 when unfinished.

### L1-6. Report

`export::report_markdown(&Session) -> String`, Japanese:

1. `# レビュー結果` (`（暫定）` before a verdict): target (kind, label, repository, base…head SHAs and merge base, files, +/−, filter), panel (reviewer → lens, agent · model · effort; lead), rounds and limit, status.
2. `## 総合判定`: decision label, rationale, must-fix list (`F1`, `F3`, with claims); `判定なし（ユーザーが終了）` when the user ended it.
3. `## 指摘一覧`: live findings ranked (L2-4), each with id, severity, lens, status, tally, confidence, location (差分外 / 対象外 marked), claim, evidence (from the lead's chosen source or rationale), fix, origins.
4. `## 観点別`: per lens, its findings (ids and claims) and the reviewers holding it.
5. `## 対立している指摘`: each `disputed` finding with every stance (reviewer, stance, note).
6. `## 却下・取り下げ・統合`: rejected findings with the lead's rationale, withdrawn ones, merged ones (→ target id), and every dismissed raw finding with the reason.
7. `## カバレッジの欠落`: built-in lenses no reviewer held, target files no reviewer reported reviewing, omitted files, and reviewers that failed a round (round, why).
8. `## ラウンド記録`: per round, its kind and the lead's summary.

It is written to `<session dir>/report.md` (temporary file + rename) after every accepted lead output and when the review closes. コピー puts it on the clipboard; レポート保存 opens a save dialog with `<id>.md`. A read-only view renders it in memory and writes nothing.

### L1-7. Storage, locking, resume, read-only view, flags

- Root: `<data dir>/noa/reviews` (`noa_config::session_state_path()`'s parent + `reviews`). Session id `rev-<millis>`, `-<n>` added when taken.
- Layout:

  ```text
  <data dir>/noa/reviews/<id>/
    events.jsonl          one event batch per line
    report.md             the report as of the last accepted lead output
    target/, r<N>/        here only when the review has no workdir
  <workdir>/.noa/reviews/<id>/   (reply dir when the review has a workdir; .noa/.gitignore = *)
    target/manifest.json  the manifest (also in the Created event)
    target/diff.patch     git targets
    target/files/<path>   captured files
    target/document.md    document targets
    r<N>/<reviewer>.json  reviewer replies
    r<N>/lead.json        lead output
  ```

  The reply dir sits in the workdir for the same reason as competition's: agents sandboxed to their workspace can read the snapshot and write replies there. A files target rooted at the workdir never captures `.noa/` (hidden, and ignored by git).
- Locking: `Store::open`/`create` hold an exclusive `flock` on `events.jsonl` while a runner has it (`LOCK_GRACE` 2 s); `is_busy` probes with a shared lock; `read_session` folds without locking.
- Delivery: every brief paste and its Enter go with `refuseShell`, so nothing is typed into a pane back at its shell (a refused Enter leaves the brief pending); a Noa older than 0.9.12, which ignores the field, is not written to. A brief whose JSON encoding exceeds 128 KiB (the control server takes frames of at most 256 KiB) is written to `r<N>/brief-<part>.md` and the agent is sent a pointer to it.
- Resume (再開, `Start::Resume`): refused when the log is held elsewhere, the workdir is gone, the log's id does not match its directory, a setup fails `check`, participant ids do not re-derive from the panel, a reply path is not the one the store gives, or the snapshot is missing or its diff/file digests differ from the manifest (「レビュー対象のスナップショットが変更されています」). Agents still in their labelled panes are taken up; the others start anew and get their open brief again. Resuming an ended review opens it read-only.
- 新しいレビュー leaves the shown review (its runner stops, panes stay) and returns to a clean start view.
- Command line: `review [--demo | --demo-start | --demo-held | --session=DIR] [--tab=findings|matrix|report] [--finding=ID] [--snapshot=FILE.png [--size=WxH]]`. `--session` shows a stored review read-only (no runner, no agents; a log that cannot be read fails before the window opens); `--demo` a canned three-round review, `--demo-start` the start view with canned past reviews, `--demo-held` the demo held after a round; `--tab` and `--finding` pick the center tab and the selected finding, so every view can be rendered headlessly with `--snapshot`. Mutually exclusive sources are refused as discussion refuses them.

### L1-8. Noa integration

As competition was added (`git show --stat db77229b`):

| File | Change |
|---|---|
| `crates/noa-app/src/commands/command.rs` | `AppCommand::OpenReview` (doc comment), `OPEN_REVIEW_MENU_ID = "noa.view.open-review"`, `menu_id()`, menu-id lookup, action name `review.open` both ways |
| `crates/noa-app/src/command_palette.rs` | title "Open Review"; description "Run a thorough multi-agent review of a diff, files or a document with blind reviews, cross-examination and a consolidated report, in a window of its own."; in `command_palette_entries()`; `CommandCategory::View`; the test's command list |
| `crates/noa-app/src/app/commands.rs` | `AppCommand::OpenReview => self.open_helper_program(crate::helper_program::REVIEW)` |
| `crates/noa-app/src/app/helpers/dispatch.rs` | `OpenReview` in `command_scope` and `overview_command_scope` (`CommandScope::App`) |
| `crates/noa-app/src/split_tree/tree/commands.rs` | `OpenReview` in `resolve_pane_command_target`'s app-level arm |
| `crates/noa-app/src/macos_menu.rs` | View → "Review" after "Competition" |
| `crates/noa-app/src/helper_program.rs` | `REVIEW = HelperProgram { title: "Review", bundled: "Noa Review", crate_name: "review" }`; module doc; dev-build-dirs test list; a `review_is_found_by_its_own_binary_name` test |
| `scripts/verify.sh` | `review=widgets/review/Cargo.toml`; `run review widget_check` in `--full`; `widgets/review/*` and `widget_uses` in the changed-files path |
| `scripts/bundle-macos.sh` | build loop, `REVIEW_BIN`, the `-x` check, copy to `Contents/Helpers/Noa Review`, `codesign` with the entitlements |
| `AGENTS.md` | `widgets/review` in the Verification list |
| `docs/FEATURES.md` | a **Review** entry after Competition, in the style of the two above |
| `widgets/review/` | `Cargo.toml` (same dependencies as competition, no new crate), `Cargo.lock`, `.gitignore` (`/target`, `*.png`) |

## L2: Detail

### L2-1. Crate layout

```text
widgets/review/
  Cargo.toml  Cargo.lock  .gitignore
  src/lib.rs            pub mod capture, engine, model, store
  src/main.rs           flags, title, snapshot rendering
  src/demo.rs           canned sessions
  src/capture.rs        git / files / document capture → TargetManifest (I/O)
  src/engine.rs         pure round state machine
  src/store.rs          log, lock, paths, listing
  src/model/{types,reduce,brief,validate,export,lens}.rs
  src/live.rs  src/live/{ipc,runner}.rs
  src/ui/{mod,start,progress,cards,board,matrix,thread,report,composer}.rs
  tests/golden.rs  tests/golden/{scenario.rs,briefs.txt,report.md}
```

`Cargo.lock` starts as a copy of `widgets/competition/Cargo.lock` with the package entry renamed, so the review widget builds against the same resolved versions; `cargo build --offline --locked` must then pass with no other change.

### L2-2. Types (new or changed)

```rust
pub enum TargetKind { Git, Files, Document }
pub enum Head { Worktree, Ref(String) }
pub struct Target {                    // what the user asked for
    pub kind: TargetKind,
    pub path: Option<PathBuf>,         // repo / root / document file
    pub base: Option<String>, pub head: Head,
    pub paths: Vec<String>,            // filter (git) or selection (files)
    pub text: Option<String>,          // document typed in
}
pub enum FileStatus { Added, Modified, Deleted, Renamed { from: String }, Untracked, Plain }
pub struct TargetFile {
    pub path: String, pub status: FileStatus, pub lines: u32, pub bytes: u64,
    pub additions: u32, pub deletions: u32,
    pub changed: Vec<(u32, u32)>,      // post-image line ranges (git)
    pub digest: u64,
}
pub struct TargetManifest {
    pub label: String,                 // e.g. "feat/x … main (12 files)"
    pub repo: Option<PathBuf>, pub base_ref: Option<String>, pub base_sha: Option<String>,
    pub head_ref: Option<String>, pub head_sha: Option<String>, pub merge_base: Option<String>,
    pub files: Vec<TargetFile>, pub omitted: Vec<(String, String)>,
    pub diff_bytes: u64, pub diff_digest: Option<u64>, pub captured_at: Millis,
}
pub enum BuiltinLens { Correctness, Security, Performance, Design, Tests, UxDocs }
pub enum Lens { Builtin(BuiltinLens), Custom { id: String, text: String } }
pub enum Severity { Critical, High, Medium, Low, Info }      // ordered
pub enum Confidence { High, Medium, Low }
pub struct Location { pub path: String, pub line: Option<u32>, pub end_line: Option<u32> }
pub struct RawKey { pub part: PartId, pub id: String }       // "codex/3"
pub struct RawFinding {
    pub key: RawKey, pub round: RoundNo, pub location: Option<Location>,
    pub in_target: bool, pub in_diff: Option<bool>,          // set by the widget
    pub severity: Severity, pub category: String, pub confidence: Confidence,
    pub claim: String, pub evidence: String, pub fix: Option<String>,
}
pub enum StanceKind { Confirm, Dispute, Refine, Unsure, Withdraw }
pub struct Stance { pub round: RoundNo, pub kind: StanceKind, pub note: String,
                    pub evidence: Option<String>, pub severity: Option<Severity> }
pub enum FindingStatus { Open, Agreed, Disputed, Rejected, Withdrawn, Merged { into: String } }
pub struct Finding {
    pub id: String, pub claim: String, pub location: Option<Location>,
    pub in_target: bool, pub in_diff: Option<bool>,
    pub severity: Severity, pub category: String, pub confidence: Confidence,
    pub status: FindingStatus, pub rationale: String, pub fix: Option<String>,
    pub sources: Vec<RawKey>, pub opened_in: RoundNo, pub changed_rev: u64,
    pub stances: BTreeMap<PartId, Vec<Stance>>,              // oldest first
    pub history: Vec<(RoundNo, FindingStatus, Severity)>,
}
pub struct Dismissal { pub raw: RawKey, pub round: RoundNo, pub reason: String }
pub enum Decision { Approve, ApproveWithFixes, RequestChanges, Reject }
pub struct Verdict { pub decision: Decision, pub rationale: String, pub must_fix: Vec<String> }
pub enum RoundKind { Blind, Cross }                          // Blind iff no == 1
pub enum Phase { Briefing, Reviewing, Consolidating, Closed }
pub struct Session {
    pub id: SessionId, pub request: String, pub target: Target, pub manifest: TargetManifest,
    pub workdir: Option<PathBuf>, pub max_rounds: RoundNo, pub rev: u64, pub created_at: Millis,
    pub lead: Participant, pub reviewers: Vec<Participant>,
    pub raw: Vec<RawFinding>, pub findings: Vec<Finding>, pub dismissed: Vec<Dismissal>,
    pub summaries: Vec<ReviewerSummary>,                    // per round, per reviewer
    pub coverage: BTreeMap<PartId, Coverage>,               // latest per reviewer
    pub messages: Vec<Message>,                             // interventions, lead summaries
    pub rounds: Vec<Round>,                                 // Round.outcome: Option<LeadOutput>
    pub closed: bool, pub hold: bool,
}
```

`Session::verdict()` is the verdict in the latest round's accepted lead output, and `None` when that round has none (for instance after +1 opened a new round), as competition's `final_verdict` leaves an earlier verdict with its round.

### L2-3. Events

`Created { id, request, target, manifest, workdir, max_rounds, lead, reviewers, at }` (first and only first), `PaneAssigned`, `RoundStarted { no, kind, at }`, `MessagePosted`, the brief lifecycle events copied from competition, `ReviewRecorded { round, part, summary, findings: Vec<RawFinding>, stances: Vec<(String, Stance)>, coverage: Option<Coverage> }` (applies the raw findings, the stances to their findings, and `withdrawn` when every origin of a finding has withdrawn), `LeadAccepted { round, output: LeadOutput }` (applies `ops` in order, posts the summary, stores questions; sets the verdict), `PhaseChanged`, `HoldSet`, `RoundsExtended { max_rounds, at }`, `SessionClosed`. Each batch bumps `rev` once.

### L2-4. Agreement and ranking

For a live finding, over the reviewers briefed in at least one round since it opened and not failed in the latest round:
- supporters = origins (not withdrawn) ∪ reviewers whose latest stance is `confirm` or `refine`; disputers = latest stance `dispute`; the rest are 未表明 (no stance or `unsure`).
- 全員一致: no disputer and every counted reviewer a supporter. 単独: one supporter, no disputer, everyone else 未表明. 対立: disputers ≥ supporters. 多数: otherwise.

Ranking (board default, report): severity (critical first) → status (`agreed`, `disputed`, `open`) → confidence (high first) → supporters (more first) → opening round → id.

### L2-5. Briefs

Section headings are constants (as discussion's `PANELIST_SECTIONS`), English like the existing briefs:

- Reviewer, round 1: `## 1. Review request and round` · `## 2. Target` (kind, label, SHAs, the snapshot path, the file table with status/lines/+/−/changed ranges, omitted files, "line numbers refer to the snapshot"; the workdir as read-only context that may have changed since capture) · `## 3. Your lens` (label and checklist or custom text; "report outside it as `other` or the matching lens") · `## 4. Rules` (blind, no edits, no other panes, evidence from the files, mark what you could not verify as unverified) · `## 5. User interventions` · `## 6. How to reply` (the path, the JSON shape of L1-4, the allowed values, "write the whole file in one go; do not answer in the terminal").
- Reviewer, round ≥ 2: sections 1–3, then `## 4. Findings so far` (each live finding with sources, lead status/rationale, every stance by round; rejected/withdrawn/merged listed by id and claim only) · `## 5. The lead's summary and your question` · `## 6. User interventions` · `## 7. How to reply` (stances required on: the list; rounds left; FINAL in the last round).
- Lead: `## 1. Review request and round` · `## 2. Target` · `## 3. Panel` (reviewers, lenses, this round's receipt: replied / no reply: why) · `## 4. Raw findings this round` (every one, full text) · `## 5. Findings so far` (with this round's stances) · `## 6. User interventions` · `## 7. This round` (rounds left; verdict required or not; close rules; settle every open finding in the last round) · `## 8. Your output` (path, JSON shape, rules, next free `F<n>`).
- Correction note: as discussion's `correction_note` ("Your review JSON / lead JSON could not be used: … Fix it and write the whole file again at: …").

## L3: Acceptance Criteria

Unit tests live beside the modules; golden tests in `tests/`; headless snapshots via `--snapshot`. GUI behaviour against a live Noa has no automated check and is verified by hand (AC-30…33).

1. AC-1 Git capture: on a temp repository with a branch, `capture` in ref mode writes `diff.patch` equal to `git diff <merge-base> <ref>`, post-images under `files/`, and a manifest with SHAs, statuses, line counts and changed ranges; no `git status` change in the repository (test).
2. AC-2 Worktree mode includes uncommitted and untracked files and leaves the index untouched (`git diff --cached` empty before and after) (test).
3. AC-3 An empty diff, a missing base, and each cap (files, total bytes, diff bytes, document size) refuse the start with the reason; a binary file and a file over 1 MiB are listed in `omitted` (test).
4. AC-4 Files capture inside a git work tree follows `.gitignore` and never captures `.noa/`; outside one it skips hidden dirs, `target`, `node_modules` (test).
5. AC-5 Document capture writes `target/document.md` from typed text and from a file (test).
6. AC-6 The engine refuses a panel of 1 or 7 reviewers, a bad model name or effort, a custom lens of 0 or 501 characters, and an empty target (test).
7. AC-7 Participant ids: `claude`, `codex`, `claude-2` for that panel; `lead` for the lead; custom lens ids `custom`, `custom-2` (test).
8. AC-8 A round-1 reviewer brief names no other reviewer, no peer reply path and no peer finding (test over every round-1 brief of the golden scenario).
9. AC-9 Reviewer validation reports each rule of L1-4 as its own problem line, including out-of-range lines against the manifest, a reused id, stances in round 1, `withdraw` by a non-origin, and the missing-stances list (table test, one case per rule).
10. AC-10 Lead validation reports unassigned raw findings, a raw finding placed twice, an unknown or taken id, `rejected` without rationale, a verdict outside the last round without `close`, `approve` with `must_fix`, `request_changes` without, an `open` finding with a verdict, and `close` in round 1 of a multi-round review (table test).
11. AC-11 A rejected reply is cleared, a correction note with every problem is sent, its digest is logged, and after `max_retries` the brief is failed; a failed reviewer does not hold the round, and the lead brief says "no reply: <why>" (engine test).
12. AC-12 Every raw finding of a closed round is in exactly one finding's sources or in `dismissed` (property checked after each `LeadAccepted` in the golden scenario).
13. AC-13 `merge` moves sources and stances into the target, marks the merged finding `merged { into }`, and the board hides it by default (test).
14. AC-14 Every origin withdrawing marks the finding `withdrawn` (test).
15. AC-15 Agreement classes and ranking follow L2-4 on a fixed set of findings (test).
16. AC-16 The last round's lead output without a verdict is rejected; with one, the session closes and `Session::verdict()` returns it (engine test).
17. AC-17 Hold: with ラウンド後に停止 on, the next round waits after consolidation until continue; turning hold off while waiting starts it; an intervention sent while held is in the next briefs (engine test).
18. AC-18 +1/+3: allowed while running before the last consolidation and after a review that ran all its rounds (the old verdict stays with its round, `verdict()` is `None` until the new last round), refused while the last round consolidates, after an early close, and after the user's 終了 (engine test).
19. AC-19 Store: two `Store::open` on one log → the second gets `Busy`; a torn last line is cut; `read_session` works while locked; `list_sessions` orders newest first and lists an unreadable log with why (test).
20. AC-20 Resume is refused for each case in L1-7 (one test each, including a changed snapshot digest), and takes up agents still in their labelled panes against the fake Noa (runner test).
21. AC-21 Golden: `tests/golden/briefs.txt` pins every brief and correction note of a fixed three-round scenario (blind round with three reviewers and one rejected reply; consolidation with add, merge and dismiss; a cross round with a dispute, a refine, a withdraw and a new finding; a final round with a verdict and `must_fix`), and `tests/golden/report.md` pins the report at its close. The fixtures are generated once from the first implementation and reviewed by hand in its pull request; afterwards a difference is a behaviour change.
22. AC-22 The report holds the eight sections of L1-6 in order; disputed findings list every stance; coverage gaps list unheld built-in lenses, unreviewed files, omitted files and failed reviewers (covered by AC-21 plus a unit test for the empty cases).
23. AC-23 `report.md` is rewritten after each accepted lead output and at close; a read-only view writes nothing to the session directory (runner test; file mtimes in the read-only test).
24. AC-24 `review --demo-start --snapshot=…` renders the start view with the three target kinds, the panel rows and canned past reviews (snapshot, inspected by hand).
25. AC-25 `review --demo --tab=findings|matrix|report --finding=F2 --snapshot=…` renders the board, the matrix (with a 担当なし row and a 未レビュー file), the report, and the thread of F2 with its stances (snapshots, inspected by hand); filtering by each of the five filters changes the shown count (UI test on the tree, like `discussion/src/ui/tests.rs`).
26. AC-26 `review --session=DIR --snapshot=…` on a stored review renders it read-only with no composer; on an unreadable log it exits non-zero before a window opens.
27. AC-27 The window title follows the locale (test, as both widgets).
28. AC-28 Noa: `review.open` and the menu id round-trip through `AppCommand`; the palette lists "Open Review" under View; `helper_program::locate(REVIEW, …)` finds `Noa Review` in Helpers and `review` in `widgets/review/target` (tests in `noa-app`).
29. AC-29 `scripts/verify.sh --full` builds and (on macOS) tests `widgets/review`; a change under `widgets/review/` alone runs that check; `scripts/bundle-macos.sh` puts a signed `Noa Review` in `Contents/Helpers`.
30. AC-30 (manual) View → Review and the palette's Open Review start the window; without the control server the window's start view says why as soon as it opens (as the discussion, competition and agent-dashboard widgets report it in their own window; Noa's toast covers only a helper that fails to start).
31. AC-31 (manual) A git review of a real branch with three reviewers runs three rounds in one Noa tab with auto approval, ends in a verdict, and writes `report.md`.
32. AC-32 (manual) ペインを開く on every card brings that pane forward; 再開 after quitting the widget mid-round takes the panes back.
33. AC-33 (manual) ラウンド後に停止 / 次のラウンドへ / +1 / 終了 / an intervention to one reviewer behave as AC-17/18 describe, on a live review.

## Scope

In scope: L1-1 … L1-8 above.

Out of scope:
- Starting an agent to fix the findings (discussion's implementation task, competition's adopt). The report is plain Markdown and can be handed over by hand.
- Debates (discussion's pro/con rounds); the cross-examination rounds take their place.
- Scores and a leaderboard of reviewers; a browser viewer (`compare.html`).
- Posting findings to GitHub or any other service.
- Reviewing binary files, images, or more than the caps of L1-1.
- The lead changing the round limit.
- Detecting edits a reviewer makes to the workspace despite the rule (the snapshot is what is reviewed, so such edits do not change the target).
- A shared crate for the agent widgets (see Reuse decision).

## Decisions taken by default

Open questions the request left open, resolved here; each can be changed before implementation:

1. Agents write JSON, not Markdown with a header, because findings need fields the board filters on; the existing fence tolerance and retries cover formatting slips.
2. The lead consolidates after every round, not only at the end, so cross-examination works on de-duplicated findings.
3. The lead writes no findings of its own; it may read the target to adjudicate.
4. Round 1 is blind and every later round is cross-examination; there is no reveal interval.
5. Default round limit 3; the lead may close early only from round 2 (or in round 1 of a one-round review), so the default review always cross-examines.
6. A verdict forbids `open` findings: every finding is adjudicated by the end.
7. Line numbers refer to the post-image snapshot; deleted lines are cited through the diff (`location` without a line).
8. Context locations outside the target are accepted and tagged 対象外, rather than rejected, since a defect in a change often shows in a caller.
9. The review tab runs with auto approval, as competition's does, so agents can run read-only commands (tests, `git log`) unattended; the brief forbids edits.
10. No legacy-lock path (`LOCKS_FILE`, `LEGACY_QUIET`): no review widget from before the lock exists.
11. Golden fixtures come from the first implementation, reviewed by hand, since there is no earlier `main` output to pin against.
12. `--tab` and `--finding` exist so every view has a headless snapshot for AC-25.

## Needs the maintainer's agreement before implementation

`AGENTS.md` (Safety) asks for the maintainer's agreement before changes to `Cargo.lock` files, `scripts/verify.sh` and `AGENTS.md`. Implementing this spec adds `widgets/review/Cargo.lock` (no new third-party crate) and changes `scripts/verify.sh` and `AGENTS.md` as L1-8 lists, exactly as the competition widget did (#232).
