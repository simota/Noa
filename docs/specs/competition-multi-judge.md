# Competition widget: multi-judge scoring (複数審判採点制度): Spec

| | |
|---|---|
| slug | competition-multi-judge |
| status | draft (design only, 2026-11-24). No code yet. No owner sign-off |
| owner | simota |
| as-of | 2026-11-24 (HEAD 23b98f3f, branch `orch/design`) |
| crate | `widgets/competition` (binary `competition`, bundled as `Noa Competition`); no other crate changes |
| request | 審判を 1 人から最大 3 人の複数審判採点制度にする（受け入れ条件 C1–C7） |

Acceptance criteria from the request, as this spec numbers them:

- C1: the start view picks 1–3 judges (agent, model, effort per row; a kind may repeat; ids `judge`, `judge-2`, …); each judge runs in its own Noa pane.
- C2: every round, each judge scores independently (blind to the others), writes its own reply file, validated as today and sent back for correction on error.
- C3: a round's score per entry is the mean of the judges' scores; winner and ranking use the mean; the round is not settled until every judge's reply is accepted.
- C4: per-judge scores and the spread (max − min) are shown in the leaderboard, the judge board, the Markdown report and `compare.html`.
- C5: early close and the final verdict are decided by the lead (first) judge; the others only score.
- C6: competitions stored in the single-judge era open read-only and resume as before (log/session format backward compatible); rematch carries the judge set.
- C7: `scripts/verify.sh --full` passes; tests in `widgets/competition` cover engine, validation, reduction, store round-trip, legacy log load, and golden UI/report outputs where they exist.

## L0: Vision

- Problem: one judge agent scores every round alone (`JUDGE_ID = "judge"`). One model's taste and its blind spots decide the ranking, and nothing shows how far a second opinion would differ.
- Job to be done: seat 1–3 judges (agent, model, effort each), let every judge score every round alone, rank on the mean, and show where the judges disagree.
- Success definition: a competition with three judges (for instance claude/opus, codex, agy) runs to its final round in one Noa tab. Each round's score per entry is the mean of three independent scorecards, and the leaderboard, the judge board, the Markdown report and `compare.html` show each judge's score and the spread. The lead judge writes the final verdict after seeing the combined scores. A competition stored by v0.9.15 opens, resumes and renders exactly as it did.

## What exists today (read from the code, HEAD 23b98f3f)

- `Session.judge: Participant` (id `judge`, `Role::Judge`); `Event::Created { judge, contestants, … }` carries it. `engine::participants(judge, panel)` makes the ids; `check_resumable` re-derives them and checks every reply path.
- A round is `Briefing → Competing → Judging → Closed`. When every contestant brief is replied or failed, `Engine::judge_round` commits `PhaseChanged(Judging)` + `BriefIssued` (to `judge`) in one batch and sends it. `Round.judge_brief: Option<Brief>` holds it. `Round::brief(to)`/`brief_mut(to)` find it beside the contestant briefs, so every receipt event (`BriefSent`, `BriefStarted`, `BriefRetried`, `BriefFailed`, `BriefReset`, `ReplyRejected`, `ReplyAccepted`) works on it by `(round, to)`.
- The judge replies `r<N>/judge.json` (`store::judge_reply_path`). `validate::parse_judge(text, session, round)` checks it (`deny_unknown_fields`, every contestant scored and ranked once, `winner` + `final_verdict` required on the last round or with `close`, breakdown sums, evidence bases, amendments only to earlier judged rounds). Each problem is its own line. A rejected reply goes back through `brief::correction_note` and fails after `Config::max_retries` (2).
- An accepted judgment commits `ReplyAccepted`, `JudgeAccepted { round, output: JudgeOutput }`, a `Judgment` message with the overview, a `FinalVerdict` message when the judge names a winner, `PhaseChanged(Closed)` and, with `close`, `SessionClosed`. `reduce` applies amendments to earlier rounds' `outcome` (clearing their breakdown, noting `AmendNote`, re-sorting that round by score) and sets `session.winner`.
- Everything downstream reads `Round.outcome`: `Session::score_history`, `latest_score`, `current_ranking`, `checklist_before`, `latest_evaluation_before`, `final_verdict`; `brief::contestant_brief` (feedback, standing line, score corrections, peer scores in reveal rounds); `ui/leaderboard.rs`, `ui/judge.rs`, `ui/compare.rs`, `ui/map.rs`, `model/map.rs`; `model/export.rs::result_markdown` (the Markdown report, copy and `adopt-<winner>.md`); `model/viewer.rs` (`compare.html`, with a `ViewerCache` whose `RoundStamp` decides when a closed round's JSON is reused).
- The runner spawns the judge's pane lazily on its first send (label `comp:<id>:judge`), in the 3×3 grid (`PANES_PER_AXIS`), so six contestants and the judge fit in one tab today. `in_play` and `all_parts` name the judge explicitly.
- The UI has one judge row in the start view (`Form.judge`, `Parts.judge`/`judge_setup`), one judge card in the leaderboard, one `審判 (judge)` chip in the progress strip, and one `審判 (Judge)` composer target. A rematch (`Form::of_session`) copies the judge.
- Fixed outputs: `tests/golden.rs` pins `briefs.txt` and five `compare-*.html` byte for byte; `src/model/testdata/before-round-control.events.jsonl` is a log from before round control, read by `store::tests::a_log_from_before_round_control_reads_as_it_did`. The Markdown report has no golden.

The request names `export/report/compare.html`; in this crate the Markdown report is `src/model/export.rs` and `compare.html` is rendered by `src/model/viewer.rs`. `docs/feature-map.md` has no competition entry; this spec does not add one.

## Glossary

| Term | Meaning | Identifier |
|---|---|---|
| judge panel (審判団) | the 1–3 judges of a competition, in seat order | `Session::judges()` |
| lead judge (リード審判) | the first judge; decides early close, publishes the checklist, writes the final verdict | `Session.judge`, id `judge` |
| co-judge (審判 2, 3) | a judge after the first; only scores | `Session.co_judges`, ids `judge-2`, `judge-3` |
| scorecard (採点票) | one judge's accepted reply for one round | `Round.scorecards[judge]: JudgeOutput` |
| combined judgment (合議結果) | the round's outcome built from every scorecard: mean scores, ranking by mean | `Round.outcome` |
| mean (平均点) | sum of the judges' scores ÷ number of judges, shown to one decimal | `PanelScores::mean_label` |
| spread (審判間の差) | max − min of the judges' scores for one entry in one round | `PanelScores::spread` |
| verdict step (最終講評) | the lead's second reply in a deciding round, written after the combined scores | `Round.verdict_brief`, `r<N>/verdict.json` |
| deciding round | a round whose combined judgment must end in a verdict: the last round, or one where the lead set `close` | |
| single-judge session | a session with no co-judge: every session stored before this change, and any new one with one judge row | `co_judges.is_empty()` |

## L1: Requirements

### L1-1. The judge panel (C1)

1. The start view's 審判 section is a list of 1–3 rows (`MAX_JUDGES = 3`). Each row has agent kind, model (ComboBox) and effort (PopUp), the same controls as a panel row, rebuilt when the kind changes (`judge_kind_changed` per row). Rows after the first have a remove button. `＋ 審判を追加` is disabled at three rows.
2. Seat order is row order. The first row is the lead judge (caption: 「1人目がリード審判です。早期終了と最終講評はリード審判が決め、ほかの審判は採点だけを行います」).
3. Ids follow the seat, not the kind: `judge`, `judge-2`, `judge-3`. One kind may sit in several rows (three claude judges on opus, sonnet and haiku is valid). Contestant ids are kind names (`claude`, `codex-2`), so the two never collide.
4. Every setup passes `AgentSetup::check`. `Engine::create_in` refuses 0 or more than 3 judges with 「審判は1〜3人です」.
5. Each judge runs in its own Noa pane, labelled `comp:<id>:<judge id>`, spawned lazily on its first send, as the judge is today. Six contestants and three judges are nine panes, which fills the 3×3 grid exactly.
6. The clean form, and the form after the installed-agents check on a pristine form, has one judge row (today's default). A competition with one judge is a single-judge session and behaves exactly as today (L2-8).

### L1-2. Independent scoring per round (C2)

1. When the round's contestant briefs are all replied or failed, the engine commits, in one batch, `PhaseChanged(Judging)` and a `BriefIssued` for every judge, then sends them all at once (in parallel, not in turn).
2. Each judge gets its own brief (`brief::judge_brief(session, round, judge, …)`), which tells it to write its reply to its own file: `r<N>/judge.json` for the lead (the path it has today), `r<N>/judge-2.json`, `r<N>/judge-3.json` for co-judges. This is the role the brief assigns, not an enforced guarantee: every seat writes into the same `r<N>/` directory and a reply is attributed by its file name only (see Risks).
3. Blindness between judges:
   - A judge brief holds nothing another judge wrote in this round: no score, overview, feedback or file path of another judge's reply.
   - It lists the judges (`judge (lead), judge-2, judge-3`) and tells the judge: score alone; do not open other judges' reply files in `r<N>/` (`judge*.json`, `verdict.json`) or read from or type into other panes; an entry's round score is the mean of all judges' scores.
   - Later rounds also carry nothing from other judges' scorecards. A judge keeps its own memory in its pane. The published checklist (the lead's) is the only judge output in a co-judge's brief, through section 4 as today.
4. Validation is today's `parse_judge` rules plus the seat rules of L1-4. A rejected reply is cleared, a correction note with every problem goes to that judge alone, the digest is logged (`ReplyRejected { digest }`), and after `max_retries` that judge's brief fails. Other judges are not affected.
5. Each accepted reply is logged as `ScorecardAccepted { round, judge, output }` in the same batch as its `ReplyAccepted`.

### L1-3. Combining and settling a round (C3)

1. The round is settled (combined) only when every judge's scorecard is accepted. The engine then commits, in the batch of the last `ScorecardAccepted`: `JudgeAccepted { round, output }` with the combined judgment, and a `Judgment` message holding the lead's overview. Then, if the round is not deciding, `PhaseChanged(Closed)` and the next round starts (or waits, when held). If it is deciding, `VerdictRequested` follows (L1-5).
2. The fold refuses a `JudgeAccepted` for a round of a multi-judge session that lacks a scorecard from any judge (`ReduceError`), so a log cannot settle a round early.
3. The combined judgment (`aggregate::combine`, L2-5):
   - score per entry: the mean of the judges' scores, rounded half up to an integer in `EntryEvaluation.score` (so the code that reads `score` keeps working); the exact mean (to one decimal), every judge's score and the spread are derived from `Round.scorecards` (`Session::panel_scores`).
   - ranking: by the sum of the judges' scores, descending (exact, since every judge scores every entry). Ties keep the lead's ranking order.
   - `overview`, `checklist`, and each entry's `strengths`, `feedback` and `evidence`: the lead's. `breakdown`: empty, since judges may split on different axes. Each judge's own breakdown stays on its scorecard.
   - `close`: the lead's `close`, or the last round. `winner`, `final_verdict`: none until the verdict step.
4. A judge that fails (no start, or no valid reply after the retries, or its pane exits `MAX_SPAWNS` times) holds the round in `Judging`. The progress strip shows `✗ judge-2: <why>`, and 再試行 (`UserRetry`) resets and resends only failed briefs, as today. The user may also pick the winner by hand (`UserSelectWinner`, which closes the competition) or end it (終了). No round is ever settled without a judge.

### L1-4. What each seat may write (C5)

The scorecard JSON shape does not change: same keys, so one template serves every judge. What a seat may put in it does change:

| Field | Single judge (today) | Lead of a panel | Co-judge |
|---|---|---|---|
| `evaluations`, `ranking`, `overview` | required | required | required |
| `checklist` | R1 publishes, later replaces | same | must be empty: 「checklist: only the lead judge (judge) publishes the checklist」 |
| `close` | ends early | asks for early close (verdict step follows) | must be false: 「close: only the lead judge (judge) can end the competition early」 |
| `winner`, `final_verdict` | required on the last round / with `close` | must be null: 「winner: leave null; the lead names the winner in the verdict step, after every judge has scored」 | must be null: 「winner: only the lead judge names the winner」 |
| `amendments` | earlier judged rounds | the judge's own earlier scorecards | the judge's own earlier scorecards |

An amendment by a panel judge corrects that judge's scorecard of an earlier round. It applies when the amending round is combined (in `JudgeAccepted`), and the corrected round is combined again (L2-5). A judge cannot amend another judge's score.

### L1-5. Early close and the final verdict (C5)

1. Only the lead decides to end early (`close: true` in its scorecard) and writes the final verdict. Co-judges cannot do either (L1-4). "Writes the verdict" is the role the brief assigns: whatever appears in `r<N>/verdict.json` is taken as the lead's, since a reply is attributed by its file name only (see Risks).
2. In a deciding round of a multi-judge session, after the combined judgment the engine commits `VerdictRequested { round, brief }` and sends the lead a verdict brief (`brief::verdict_brief`). The brief carries the combined table: per entry the mean, each judge's score and the spread, ranked by mean. It also carries each judge's overview, the user's interventions, and the entries with the highest mean. The reply path is `r<N>/verdict.json`.
3. The verdict reply is `{"winner": "<id>", "final_verdict": "<text>"}` (`deny_unknown_fields`, fence tolerated). `validate::parse_verdict` requires `winner` to be one of the entries with the highest mean (the lead breaks a tie; it cannot overrule the mean) and `final_verdict` to be non-empty. Errors go back as correction notes, with retries and failure as for any brief.
4. On acceptance: `ReplyAccepted`, `VerdictAccepted { round, winner, final_verdict }` (sets the round's `outcome.winner`/`final_verdict` and `session.winner`), a `FinalVerdict` message 「優勝: <winner>\n\n<verdict>」 authored by `judge`, `PhaseChanged(Closed)`, `SessionClosed`. `Session::final_verdict()` then returns it unchanged.
5. While a verdict is requested and not replied, `Session::extendable()` refuses with 「最終講評の待機中です」. Without this, an early close could be extended in the window between scores and verdict. `UserSelectWinner` and 終了 work as today.
6. A single-judge session has no verdict step: its judge names the winner and verdict in its scorecard, as today.

Why a second step: the lead scores blind, so a verdict written in the scorecard could praise an entry the mean does not put first. Asking after the combined scores keeps the lead's scoring independent (C2) and makes the verdict name the winner the mean chose (C3).

### L1-6. Interventions, holds, +N rounds (engine behaviour)

1. Interventions: the composer offers 全員, 審判全員 (every judge id), each judge by id when there are two or more, and each contestant. A judge brief lists every intervention up to its round with its addressees, as today. An intervention posted while judges are judging reaches the briefs issued after it: the verdict brief, and next round's briefs. It does not reach a judge brief already sent (today's rule).
2. ラウンド後に停止: the hold applies after the round is `Closed`, so after the combined judgment of a non-deciding round. `waits_to_continue` is unchanged.
3. +1 / +3: unchanged rules (`extendable`), plus L1-5.5. After a verdict that ended the competition at its limit, an extension clears the winner as today. The verdict stays with its round, and the new last round is a deciding round with its own verdict step.
4. 再試行 in `Judging` resets every failed judge brief, or the failed verdict brief.

### L1-7. Panes, deaths, and resume (engine and runner)

1. A judge's pane that exits before or during its turn follows today's per-brief path. `deliver_sends` starts it again (up to `MAX_SPAWNS`, then `SendFailed` → `retry_or_fail`); an `error` state retries; `start_timeout`/`reply_timeout` retry, then fail. Only that judge's brief is affected.
2. The verdict brief is self-contained (the combined table and every overview are in it). A lead whose pane was restarted after its scorecard can therefore write the verdict with no memory of the round.
3. Resume (`Start::Resume` / `Start::Extend`) mid-judging:
   - Judges whose scorecard is accepted are not briefed again.
   - Judges with a sent, unreplied brief get their full time again from now (`waiting`), and their reply file is watched. A brief lost with its pane is redelivered without a retry (`redeliver_lost`, over every judge brief and the verdict brief). A rejected leftover on disk is cleared (`clear_rejected_leftovers`, same set).
   - A judge brief issued but never sent is sent.
   - A round waiting for its verdict sends or watches the verdict brief.
   - Labelled panes of every judge are taken up again (`sync_panes` over `all_parts`, which lists every judge).
4. `check_resumable` re-derives every judge id from the logged setups. It refuses a log whose judge ids or roles differ, and checks every judge brief's reply path (`r<N>/<judge>.json`) and every verdict brief's (`r<N>/verdict.json`), as it does for contestants today.

### L1-8. What the user sees (C4)

Single-judge sessions show exactly what they show today. With two or more judges:

1. Leaderboard (`ui/leaderboard.rs`):
   - Each entry's tag is the latest mean (`81.3 pt`).
   - The history line uses means (`R1:78.7 → R2:81.3`).
   - A line under it gives the latest round's per-judge scores and spread: `judge 80 · judge-2 83 · judge-3 81 · 差 3`. A spread of `SPREAD_WARN` (15) or more is tagged with `Tone::Warning`.
   - Below the contestants there is one judge card per judge: 「リード審判 · claude (opus)」, 「審判 2 · codex」, …, each with its round status (the lead's shows the verdict brief's status once it is issued) and ペインを開く.
2. Judge board (`ui/judge.rs`), per judged round:
   - The lead's overview.
   - The ranking with means (`#1 codex (84.3)`).
   - A table of scores by judge, one row per entry with columns per judge and 差 (spread), with warnings as in the leaderboard.
   - Each co-judge's overview under a caption 「審判 2 (codex) の総評」.
   - A round waiting for its verdict says 「最終講評を待っています（judge）」.
3. Progress strip (`ui/progress.rs`): a chip per judge (`審判 (judge)`, `審判 (judge-2)`, …). Once requested, the verdict brief has its own chip `講評 (judge)`. The step label stays 採点, with `採点 2/3` counting accepted scorecards.
4. Markdown report (`model/export.rs::result_markdown`):
   - The 審判 line lists every judge with kind and setup, marking the lead.
   - The leaderboard lines use means and add the last round's per-judge scores and spread.
   - Each round record gives every entry `(81.3/100 · judge 80 / judge-2 83 / judge-3 81 · 差 3)`.
   - In both, a spread of `SPREAD_WARN` (15) or more is marked as the leaderboard marks it: `⚠ 差 18`.
   - Each round gets a `**審判別総評:**` list.
   - The final verdict section is unchanged.
   - The same text feeds 結果をコピー, レポート保存… and `adopt-<winner>.md`.
5. `compare.html` (`model/viewer.rs`):
   - Each entry card shows the mean (`81.3 / 100`), a `審判別スコア` block (`judge 80 · judge-2 83 · judge-3 81`), and `審判間の差 3`, highlighted at `SPREAD_WARN` or more.
   - The header data lists the judges.
   - The cards are ordered by the combined ranking.
6. Contestant briefs (what each contestant learns about its scores):
   - The mean, each judge's score, and each judge's strengths, feedback and breakdown, labelled by judge id. Only its own; the blind rule between contestants is unchanged.
   - The standing line uses means to one decimal (「you lead by 2.7 point(s)」).
   - A score correction names the judge and shows the new mean.
   - Peer scores in reveal rounds are means.
7. The in-app compare tab (`ui/compare.rs`), the direction map and the timeline read `outcome` and keep working with rounded means. They get no per-judge display in this change.

### L1-9. Stored competitions and rematch (C6)

1. Every log written before this change (no `co_judges`, no new events) folds to a single-judge session. It opens read-only and resumes exactly as today. Its briefs, its `compare.html`, and its report are byte-identical to what the current code produces.
2. A single-judge session started after this change writes the same events, bytes included, as today: no `co_judges` key, no `ScorecardAccepted`, no verdict events. An older widget therefore still reads it. A multi-judge log is not meant for older widgets: their fold fails at the first `scorecard_accepted` line and the start view lists it as unreadable with why. Before that line it would misread `judge-2` as a contestant; see Risks.
3. Rematch (同じ設定でリマッチ, and リマッチ on each listed competition): `Form::of_session` copies every judge in seat order with its kind and setup. `Form::restricted` follows the panel rule. A judge of a kind not installed is dropped, a dropped lead is replaced by the first co-judge left, or else by the first installed kind on its defaults. At least one judge stays, and the dropped kinds are reported in the left-out toast as today.

## L2: Detail

### L2-1. Types (`model/types.rs`)

```rust
pub const MAX_JUDGES: usize = 3;
pub const JUDGE_ID: &str = "judge";            // unchanged: the lead's id
pub const VERDICT_FILE: &str = "verdict.json"; // in store.rs
pub const SPREAD_WARN: u8 = 15;                // in model/aggregate.rs

/// The id of the judge in seat `seat` (0 = lead): judge, judge-2, judge-3.
pub fn judge_id(seat: usize) -> PartId;

pub struct Session {
    // … unchanged fields …
    pub judge: Participant,                    // the lead (unchanged)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub co_judges: Vec<Participant>,           // seats 2..=3, Role::Judge
}
impl Session {
    pub fn judges(&self) -> impl Iterator<Item = &Participant>; // lead first
    pub fn is_judge(&self, id: &PartId) -> bool;
    pub fn is_panel(&self) -> bool;            // !co_judges.is_empty()
    /// Every judge's score of `part` in round `no`, in seat order, when the
    /// round has scorecards (multi-judge); `None` for a single-judge round.
    pub fn panel_scores(&self, no: RoundNo, part: &PartId) -> Option<PanelScores>;
    /// `81.3` in a panel round, `81` otherwise (the label every view uses).
    pub fn score_label(&self, no: RoundNo, part: &PartId) -> Option<String>;
    // participant(): chains co_judges; extendable(): refuses while a verdict is pending;
    // author_name(): "審判 (Judge)" for judge, "審判 2 (judge-2)" for co-judges.
}

pub struct Round {
    // … unchanged fields, including judge_brief (the lead's scorecard brief) and outcome …
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub co_judge_briefs: Vec<Brief>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub scorecards: BTreeMap<PartId, JudgeOutput>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verdict_brief: Option<Brief>,
}
impl Round {
    /// Every judge brief now open or done, lead first: the lead's is the
    /// verdict brief once issued, else its scorecard brief.
    pub fn judge_briefs(&self) -> impl Iterator<Item = &Brief>;
    // brief(to)/brief_mut(to): for the lead, the verdict brief once issued
    // (its scorecard brief is replied by then); co-judge briefs searched too.
    // retryable(): in Judging, failed and unreplied among judge_briefs().
}
```

`PanelScores` (`model/aggregate.rs`): `scores: Vec<(PartId, u8)>` in seat order, with `sum()`, `mean_x10()` (`(20·sum + n) / 2n`, half up), `rounded()` (`(2·sum + n) / 2n`), `mean_label()` (`"81.3"`), `spread()` (max − min). Panel scores are derived from `scorecards` and never stored, so the mean cannot drift from the cards.

`EntryEvaluation`, `JudgeOutput`, `Amendment`, `AmendNote`, `Participant`, `Role` and `Phase` are unchanged. No new phase is added: the verdict step happens inside `Judging`. The step strip, `in_play` and `extendable` keep their phase logic, and the lead's open brief is told apart by `verdict_brief`.

### L2-2. Events (`model/reduce.rs`)

| Event | Change | Fold |
|---|---|---|
| `Created { …, judge, co_judges, … }` | `co_judges: Vec<Participant>`, `#[serde(default, skip_serializing_if = "Vec::is_empty")]` | sets `Session.co_judges` |
| `PaneAssigned` | — | slot search includes co-judges |
| `BriefIssued { round, brief }` | — | to the lead → `judge_brief`; to a co-judge → `co_judge_briefs`; else contestant briefs |
| `ScorecardAccepted { round, judge, output }` | new | `round.scorecards[judge] = output`; error unless `judge` is a judge of a panel session |
| `JudgeAccepted { round, output }` | same shape | single-judge rounds: exactly today's fold. Panel rounds: error unless every judge has a scorecard. Then applies every scorecard's amendments to that judge's earlier scorecard (score set, breakdown cleared, `AmendNote` on the card's evaluation), recombines each amended round's `outcome` (L2-5), and stores `output` as `outcome` |
| `VerdictRequested { round, brief }` | new | `round.verdict_brief = Some(brief)`; error unless the round's `outcome` is set and `outcome.close` |
| `VerdictAccepted { round, winner, final_verdict }` | new | sets `outcome.winner`, `outcome.final_verdict`, `session.winner`; error unless `winner` is a contestant and, in a panel session, the round's verdict brief was issued, `winner` is in `aggregate::top_set` for the round and `final_verdict` is not empty after trimming |
| receipt events, `PhaseChanged`, `MessagePosted`, `WinnerSelected`, `HoldSet`, `RoundsExtended`, `SessionClosed` | — | unchanged (receipts find the verdict brief through `brief_mut`) |

Each batch still bumps `rev` once. The batches that matter, panel session:

- judging starts: `[PhaseChanged(Judging), BriefIssued(judge), BriefIssued(judge-2), BriefIssued(judge-3)]`
- a scorecard that is not the last: `[BriefStarted?, ReplyAccepted, ScorecardAccepted]`
- the last scorecard of a round: `[BriefStarted?, ReplyAccepted, ScorecardAccepted, JudgeAccepted, MessagePosted(Judgment), PhaseChanged(Closed)]`, or, in a deciding round, `[…, JudgeAccepted, MessagePosted(Judgment), VerdictRequested]`, followed by the next round's batch when not held
- the verdict: `[BriefStarted?, ReplyAccepted, VerdictAccepted, MessagePosted(FinalVerdict), PhaseChanged(Closed), SessionClosed]`

### L2-3. Reading legacy logs

No migration and no version field. A log is read as a single-judge session whenever its `Created` has no `co_judges`, which covers every log written so far and every new single-judge log. All three new events and all three new `Round` fields are absent from such a log, and every serde default reproduces today's struct. In such a session the engine, the briefs, the views and the export take the exact code path of today (L2-8). Pinned by:

- the existing `before-round-control.events.jsonl` test, unchanged;
- a new fixture `src/model/testdata/single-judge-v0.9.15.events.jsonl`, generated with the code at HEAD 23b98f3f **before any change** (Unit 1's first commit). It is a two-round competition of claude and codex judged by agy, with a rejected judge reply, an amendment, a hold, an intervention and a final verdict, plus `single-judge-v0.9.15-judging.events.jsonl`, the same log cut after round 2's judge brief was sent;
- tests that fold them, resume them, and re-serialize every event to the same bytes (Units 1 and 2).

### L2-4. Engine flow for a round with N judges (`engine.rs`)

```text
Competing ──all contestant briefs replied/failed──▶ judge_round(no)
  commit [PhaseChanged(Judging), BriefIssued × N]; watch + send each
Judging, per judge J (in any order, in parallel):
  reply(J): open_brief(no, J)? → validate by seat
    • invalid → reject_reply (ClearReply, ReplyRejected{digest} or BriefFailed) — J only
    • valid, single-judge session → today's batch, unchanged
    • valid, panel, not last → [ReplyAccepted, ScorecardAccepted]
    • valid, panel, last    → combine → [ReplyAccepted, ScorecardAccepted, JudgeAccepted,
                               MessagePosted(Judgment)] +
                               (deciding ? [VerdictRequested] → watch + send verdict
                                         : [PhaseChanged(Closed)] → next_round)
  failure of J (timeouts, error state, pane deaths, retries exhausted) → J's brief failed;
  the round waits; UserRetry resets failed briefs only
Judging with verdict_brief (lead only):
  reply(lead) → parse_verdict → [ReplyAccepted, VerdictAccepted, MessagePosted(FinalVerdict),
                                 PhaseChanged(Closed), SessionClosed]
```

- `Setup.judge: AgentChoice` becomes `Setup.judges: &[AgentChoice]`. `participants(judges, panel)` returns `Seats { judge, co_judges, contestants }`.
- `is_judge(part)` checks every judge. `open_brief` treats every judge brief, and the verdict brief, as in play during `Judging`. `start()` (first start and resume) watches and sends every unsent, unfailed judge brief and the verdict brief, and gives every sent, unreplied one fresh time. `timeouts()` iterates `judge_briefs()`.
- "Deciding" = `outcome.close` of the combined judgment, which is the lead's `close` or the last round.
- The combined judgment is computed by the engine with `aggregate::combine` and logged whole. The fold trusts it as it trusts today's judge output, and recomputes only for amendments.

### L2-5. Aggregation (`model/aggregate.rs`, new)

```rust
/// The round's combined judgment from every judge's scorecard (seat order,
/// lead first). Pure; the engine logs its result in `JudgeAccepted`.
pub fn combine(session: &Session, round: RoundNo, cards: &BTreeMap<PartId, JudgeOutput>) -> JudgeOutput;
/// Recomputes `round`'s outcome from its scorecards after an amendment,
/// noting on each changed entry `AmendNote { from: <old rounded mean>,
/// by_round, reason: "<judge>: <reason>[; …]" }`.
pub fn recombine(session: &mut Session, round: RoundNo, by_round: RoundNo, reasons: …);
```

- Every judge scores every contestant (validation requires it), so all sums are over the same N and comparing sums compares means exactly.
- Rounding is half up, in integers, with no floating point. 80.5 → 81; the label always has one decimal (`81.0`).
- Ranking: sum descending, then the lead's ranking position.
- Top set (for the verdict): the contestants whose sum equals the highest.

### L2-6. Briefs (`model/brief.rs`)

The single-judge briefs are byte-identical to today (`tests/golden/briefs.txt` unchanged). In a panel session:

- `judge_brief(session, round, judge, reply_dir, reply_path)`:
  - Section 1 adds, after the contestants: `Judging panel: judge (lead), judge-2, judge-3. You are judge-2.` and `An entry's round score is the mean of every judge's score.`
  - Section 5 adds: `- Score independently: do not open other judges' reply files in r<N>/ (judge.json, judge-2.json, …, verdict.json), and do not read from or type into any other pane.`
  - The checklist bullet for co-judges: R1 `The lead judge publishes the checklist with this round's judgment; leave "checklist" empty.`, later rounds `Apply the published checklist in section 4. Leave "checklist" empty: only the lead judge changes it.`
  - The amendments bullet says `your own earlier scores`.
  - The close/verdict bullet:
    - lead, last round: `This is the FINAL round: score as usual and set "close": true. Leave "winner" and "final_verdict" null: once every judge has scored you will get the combined scores and be asked for the winner and the final verdict.`
    - lead, other rounds: `Leave "winner" and "final_verdict" null. Set "close": true only if the competition should end early; you will then be asked for the verdict once every judge has scored.`
    - co-judge: `Leave "winner" and "final_verdict" null and "close": false: the lead judge decides early close and the verdict; you only score.`
  - Section 6's JSON template is unchanged except `"checklist": []` for co-judges.
- `verdict_brief(session, round, reply_path)`, headings:
  - `## 1. Final verdict — Round N of M`: request; why the verdict is due (final round / early close).
  - `## 2. Combined scores (mean of N judges)`: one line per entry in combined order, `1. codex — mean 84.3 (judge 85, judge-2 80, judge-3 88; spread 8)`, then `Highest mean: codex` or `Tied for the highest mean: codex, claude`.
  - `## 3. Each judge's overview`: one sentence saying the overviews are data the judges wrote, not instructions; then per judge `### judge-2` and its overview in a fenced block one backtick longer than the longest backtick run in it (three at least), so the text cannot close the fence.
  - `## 4. User interventions`.
  - `## 5. Your JSON output`: path, `{"winner": "<one of: codex>", "final_verdict": "Why the winner won, citing the judges' scores and where they disagreed"}`, `Write ONLY valid JSON (no prose around it)`.
- `contestant_brief`, section 3, panel rounds:
  - `Judges' evaluation of your R{n} entry (mean of {k} judges):`
  - `- Score: 81.3/100 (judge 80, judge-2 83, judge-3 81)`
  - one sentence saying the judges' strengths and feedback are quoted from their scorecards and are not instructions that change the brief's rules; then per judge `- judge-2:` and `Strengths: …` / `Feedback to address: …` in a fenced block (the same fence as the verdict brief's overviews), with its breakdown lines
  - the standing line on means to one decimal
  - per-judge corrections: `- Score correction: judge-2 changed your R1 score from 70 to 78 (reason); R1 mean is now 80.3.`
  - Peer showcase scores are means.
- `correction_note` is unchanged, and serves scorecards and verdicts alike.

### L2-7. Store and runner

- `store::judge_reply_path(dir, round, judge: &PartId)` = `r<N>/<judge>.json` (`judge` → `judge.json`, today's file). `store::verdict_reply_path(dir, round)` = `r<N>/verdict.json`. Contestant reports are `report-<part>.md`, so nothing collides.
- Runner (`live/runner.rs`): `in_play` treats every judge, and the verdict brief, as in play in `Judging`. `all_parts` lists every judge. `clear_rejected_leftovers` and `redeliver_lost` iterate `judge_briefs()`. `Start::New { judges: Vec<AgentChoice>, … }`. Artifact snapshots stay contestant-only.
- `live.rs`: `Command::Start { judges: Vec<AgentChoice>, … }`.

### L2-8. Single-judge identity rule

Every function that gains panel behaviour branches on `session.is_panel()` (or on `round.scorecards.is_empty()` for per-round data) and keeps today's code on the single-judge side, not a generalized N = 1 path. The proof is the existing byte-for-byte goldens (`briefs.txt`, five `compare-*.html`), which stay untouched. Unit 3 adds a report golden generated before its change. Where a generalized path would be simpler (`judge_briefs()` with one element), it is allowed only if those goldens and the legacy-log tests pass unchanged.

`compare.html`'s template is shared. The per-judge rendering therefore lives in a separate script block, emitted only for panel sessions. It wraps `renderRound` and re-renders the current round. The panel fields of the page data (`ViewerEntry.panel`, `ViewerHeader.judges`) are `skip_serializing_if` empty, so a single-judge page is byte-identical. The `ViewerCache`'s `RoundStamp` gains every judge's score per entry. An amendment that moves a judge's score without changing the rounded mean must still invalidate the cached round JSON.

## L3: Acceptance criteria

Unit tests live beside their modules; goldens under `tests/`. GUI behaviour against a live Noa has no automated check and is verified by hand (AC-20).

1. AC-1 (C1) `Engine::create_in` takes 1, 2 and 3 judges and refuses 0 and 4 with 「審判は1〜3人です」; ids are `judge`, `judge-2`, `judge-3` whatever the kinds, including three judges of one kind; every judge's setup is checked.
2. AC-2 (C1) The start view adds judge rows up to three (the add button then disabled) and removes any row after the first; `start::read` returns them in order; each row's model/effort controls follow its kind.
3. AC-3 (C1) Six contestants and three judges fit in one Noa tab against the fake Noa; each judge has its own pane labelled `comp:<id>:judge-N`.
4. AC-4 (C2) Judging starts in one batch with a brief per judge; each brief names only its own reply path; no judge brief in any round of the multi-judge golden scenario contains another judge's overview, feedback, score or reply path.
5. AC-5 (C2) A rejected reply from `judge-2` is cleared, corrected and logged with its digest, without any effect on `judge` or `judge-3`; after `max_retries` only `judge-2`'s brief fails.
6. AC-6 (C2/C5) Validation table: a co-judge's `close`, `winner`, `final_verdict` and `checklist` are each their own problem; a panel lead's `winner`/`final_verdict` are refused; an amendment of another judge's score, or of a round without that judge's scorecard, is refused; a single judge's rules are unchanged (every existing `validate` test passes unchanged).
7. AC-7 (C3) The round stays in `Judging` until the last scorecard; the fold refuses a `JudgeAccepted` without every scorecard; the combined score is the half-up mean, the ranking is by exact mean with ties in the lead's order, and texts are the lead's.
8. AC-8 (C3) A failed judge holds the round; 再試行 resends only failed judge briefs; once that judge is accepted the round settles.
9. AC-9 (C3) An amendment by `judge-2` changes `judge-2`'s R1 card only, recombines R1 (new rounded score, re-sorted ranking, `AmendNote` naming `judge-2`), and the contestant's next brief states the change and the new mean.
10. AC-10 (C5) In the last round, and after the lead's `close`, the lead gets a verdict brief listing every mean, judge score and spread; a verdict naming an entry outside the top set is refused, a tied top accepts either; acceptance closes the competition with that winner and `Session::final_verdict()` returns the verdict; rounds cannot be added while the verdict is pending.
11. AC-11 (engine) Hold waits after a combined judgment; an intervention posted while judges judge reaches the verdict brief and the next round's briefs; +1 after a panel verdict reopens without a winner and the new last round has its own verdict step.
12. AC-12 (C6) Resume mid-judging (some scorecards in, one brief sent, one lost with its pane) briefs only the judges without a scorecard, redelivers the lost one without a retry, and takes up every judge's labelled pane; resume while the verdict is pending sends the verdict brief.
13. AC-13 (C6) `single-judge-v0.9.15.events.jsonl` folds to the recorded result, passes `check_resumable`, re-serializes byte-identically, opens read-only with the same views and report as before; the `-judging` cut resumes by re-sending the judge's brief as today; `before-round-control.events.jsonl` reads as it did.
14. AC-14 (C6) A new single-judge competition writes no `co_judges` key and no new event (its log lines equal today's for the same inputs).
15. AC-15 (C6) A rematch of a three-judge competition fills three judge rows with kinds and setups in order; uninstalled judge kinds are dropped by L1-9.3, and the toast names them.
16. AC-16 (C4) Leaderboard, judge board and progress strip of the multi-judge demo show per-judge scores, the spread (warning at ≥ 15), a card and ペインを開く per judge, and the verdict chip (UI tests on the tree).
17. AC-17 (C4) The Markdown report of a panel session lists the judges, per-judge scores and the spread per round and in the leaderboard (golden `tests/golden/multi/report.md`); a single-judge report is byte-identical to its golden generated before the change (`tests/golden/report-r3-final.md`).
18. AC-18 (C4) `compare.html` of a panel session shows means, per-judge scores and the spread (goldens `tests/golden/multi/compare-*.html`); the cached render equals the uncached one across an amendment that moves one judge's score but not the rounded mean; the five single-judge goldens are unchanged.
19. AC-19 (C7) `scripts/verify.sh --full` passes (it builds `widgets/competition` with `--all-targets`, so `examples/perf_probe.rs` and `examples/viewer_mem_probe.rs` must compile, and runs its tests on macOS).
20. AC-20 (manual) A live competition with three judges (two of one kind) runs two rounds and ends in the lead's verdict in one Noa tab; ペインを開く on each judge card focuses its pane; quitting mid-judging and 再開 takes the panes back.

## Implementation units

Three units, merged in order **1 → 2 → 3**. Unit 1 is the base for both others. Units 2 and 3 touch disjoint files and can be built in parallel worktrees once Unit 1 is on `main`. Unit 3 is rebased onto `main` after Unit 2 merges, and its manual check runs after that rebase. After every merge `main` is shippable. Until Unit 2 lands, `Engine::create_in` refuses more than one judge, so a start view from Unit 3 cannot start a panel the engine would mishandle.

### Unit 1: model, log, validation, aggregation (base)

- Files:
  - `src/model/types.rs`, `src/model/reduce.rs`, `src/model/aggregate.rs` (new), `src/model/mod.rs`, `src/model/validate.rs`, `src/store.rs`
  - in `src/engine.rs`, only `Setup`, `participants`, `check_resumable`, `create_in`'s checks (with the temporary refusal of more than one judge, marked `#TODO(agent): Unit 2 lifts this`) and the `is_judge`/`open_brief` helpers
  - `src/live.rs` and `src/live/runner.rs` (`judges` plumbing in `Command::Start`/`Start::New` only); `src/ui/mod.rs` (`judges: vec![form.judge]` only)
  - new fixtures `src/model/testdata/single-judge-v0.9.15*.events.jsonl`
  - mechanical literal updates for the new `Session`/`Round`/`Setup` fields in `src/model/{brief,viewer,map,export}.rs` tests, `src/ui/compare.rs` tests, `src/demo.rs`, `tests/golden/scenario.rs`, `examples/perf_probe.rs`, `examples/viewer_mem_probe.rs`
- Delivers: L2-1, L2-2, L2-3, L2-5, the validation of L1-4 and L1-5.3, the store paths of L2-7, `check_resumable` of L1-7.4, the extendable rule of L1-5.5.
- Serves: C2 (validation), C3 (fold + aggregation), C5 (seat rules, verdict parse), C6 (legacy fold), C7.
- Order: first. Generate the two fixtures from unmodified code in the unit's first commit.
- Done when `scripts/verify.sh --full` passes and these tests exist and pass:
  - `types::tests::judge_ids_follow_the_seat`
  - `aggregate::tests::{mean_rounds_half_up_and_spread_is_max_minus_min, ranking_is_by_exact_mean_with_ties_in_the_leads_order, combined_texts_are_the_leads}`
  - `reduce::tests::{a_panel_round_is_not_judged_before_every_scorecard, co_judge_briefs_are_not_contestant_briefs, an_amendment_corrects_its_own_judges_card_and_recombines_the_round, the_verdict_sets_the_winner_and_the_result, a_single_judge_created_event_has_no_co_judges_key}`
  - `validate::tests::{co_judges_only_score, a_panel_lead_leaves_the_winner_to_the_verdict_step, amendments_reach_only_the_judges_own_cards, a_verdict_names_an_entry_with_the_highest_mean}`
  - `store::tests::{judge_reply_paths_follow_the_judge_id, a_single_judge_log_from_v0_9_15_folds_and_round_trips}` (AC-13 fold/round-trip part)
  - `engine::tests::{check_resumable_checks_every_judge, more_than_three_judges_are_refused}`
  - `types::tests::rounds_are_not_added_while_a_verdict_is_pending`
  - every existing test, including `tests/golden.rs`, unchanged.

### Unit 2: engine flow, briefs, runner (depends on Unit 1)

- Files: `src/engine.rs` (flow: `judge_round`, `reply`, `start`, `timeouts`, `user_retry`, verdict step; lift the temporary refusal); `src/model/brief.rs` (`judge_brief` per judge, `verdict_brief`, panel sections of `contestant_brief`); `src/live/runner.rs` (`in_play`, `all_parts`, leftovers/redelivery over every judge brief, tests); new `tests/golden_multi.rs`, `tests/golden/multi/scenario.rs`, `tests/golden/multi/briefs.txt`.
- Delivers: L1-2, L1-3 (engine side), L1-5.2–4, L1-6, L1-7.1–3, L2-4, L2-6, the runner part of L2-7.
- Serves: C1 (own pane per judge), C2, C3, C5, C6 (resume), C7.
- Order: after Unit 1. Does not touch `src/ui/*`, `src/model/{export,viewer}.rs`, `src/demo.rs` or `tests/golden.rs`.
- Done when `scripts/verify.sh --full` passes and these tests exist and pass:
  - `engine::tests::{every_judge_is_briefed_in_one_batch, the_round_settles_only_after_every_judge, a_rejected_judge_reply_is_corrected_alone, a_failed_judge_holds_the_round_until_retried, the_lead_writes_the_verdict_after_the_combined_scores, a_co_judges_close_is_refused_and_the_leads_goes_to_the_verdict, a_held_panel_waits_after_the_combined_judgment, an_intervention_during_judging_reaches_the_verdict_and_next_briefs, a_panel_extended_after_its_verdict_has_a_new_verdict_step, resume_mid_judging_briefs_only_judges_without_a_scorecard, resume_while_the_verdict_is_pending_sends_it, a_single_judge_log_cut_while_judging_resumes_as_before}`
  - the `Logged` harness extended to three judges with `folds()` after each step
  - `brief::tests::{co_judge_briefs_say_they_only_score, a_judge_brief_holds_nothing_from_another_judge, contestant_brief_carries_the_mean_and_each_judges_feedback, verdict_brief_lists_combined_scores_and_the_top_set}`
  - `runner tests::{six_entries_and_three_judges_fit_in_one_tab, resume_takes_up_every_judges_pane, a_dead_judge_pane_is_restarted_for_its_brief}`
  - `tests/golden_multi.rs::multi_judge_briefs_match_the_fixture` (fixture generated once by this unit, reviewed by hand in its PR, covering AC-4 over every round)
  - `tests/golden.rs` unchanged and passing.

### Unit 3: start view, rematch, views, report, compare.html (depends on Unit 1; rebased after Unit 2)

- Files: `src/ui/start.rs` (judge rows, `Form.judges`, `of_session`, `restricted`), `src/ui/mod.rs` (start/rematch/installed plumbing, composer wiring, tests), `src/ui/leaderboard.rs`, `src/ui/judge.rs`, `src/ui/progress.rs`, `src/ui/composer.rs`, `src/model/export.rs`, `src/model/viewer.rs`, `src/demo.rs` (a three-judge demo), `src/main.rs` (`--demo --judges`), `tests/golden.rs` (one new report test), `tests/golden/report-r3-final.md` (new, generated before the export change), new `tests/golden_multi_view.rs` with `tests/golden/multi/session.rs` (a panel session folded from a fixed event list built through the Unit 1 reducer, so it does not need Unit 2's engine), `tests/golden/multi/{compare-*.html,report.md}`, `docs/FEATURES.md` (Competition entry), `CHANGELOG.md` (Unreleased).
- Delivers: L1-1.1–2 and 6, L1-6.1 (composer), L1-8, L1-9.3, the viewer part of L2-8.
- Serves: C1 (start view), C4, C6 (rematch, read-only views), C7.
- Order: after Unit 1, in parallel with Unit 2. Merge after Unit 2, rebased.
- Done when `scripts/verify.sh --full` passes and these tests exist and pass:
  - `ui::start::tests::{judge_rows_take_one_to_three, judges_of_one_kind_are_judge_and_judge_2}`
  - `ui::tests::{a_rematch_fills_every_judge_row, a_rematch_leaves_out_judges_not_installed, a_pristine_form_keeps_one_judge_after_the_agent_check, every_agent_and_every_judge_has_a_pane_button, the_leaderboard_shows_each_judges_score_and_the_spread, the_judge_board_lists_scores_by_judge, the_progress_strip_has_a_chip_per_judge_and_the_verdict, interventions_go_to_one_judge_or_every_judge}`
  - `tests/golden.rs::report_markdown_matches_main_byte_for_byte`
  - `tests/golden_multi_view.rs::{multi_judge_compare_html_matches_the_fixture, cached_multi_judge_compare_html_matches_uncached, multi_judge_report_matches_the_fixture}`
  - the existing `ui::tests::every_agent_and_the_judge_has_a_pane_button` and the three compare goldens unchanged
  - by hand: `competition --demo --judges --snapshot=…` renders the leaderboard and judge board, and AC-20 on a live Noa after the rebase onto Unit 2.

### Criterion → unit map

| Criterion | Unit 1 | Unit 2 | Unit 3 |
|---|---|---|---|
| C1 1–3 judges, own panes | ids, limits | panes, briefing | start view rows |
| C2 independent, validated, corrected | validation | per-judge briefs, corrections | — |
| C3 mean, settle after all | fold, aggregation | engine settling | — |
| C4 per-judge scores and spread shown | `PanelScores` | contestant briefs | leaderboard, judge board, report, compare.html |
| C5 lead decides close and verdict | seat rules, verdict parse | verdict step | lead marked in views |
| C6 legacy logs, resume, rematch | legacy fold, fixtures | resume flow | read-only views, rematch |
| C7 verify + tests | yes | yes | yes |

## Scope

In scope: L1-1 … L1-9.

Out of scope:
- Skipping a failed judge to settle a round with fewer scorecards (C3 forbids it; the user can retry, pick a winner or end).
- Weighted judges, trimmed means, or medians.
- Per-judge display in the in-app compare tab, the direction map and the timeline (they show rounded means).
- Changing the judge panel of a running or stored competition.
- Reading multi-judge logs with widgets from before this change.
- A shared crate with `widgets/review` (whose lead is a different role).

## Decisions taken by default

Open points the request left open, resolved here. Each can be changed before implementation:

1. **Verdict as a second step for panels.** The lead names the winner after seeing the combined scores, and only among the top-mean entries. The alternative, a blind verdict inside the scorecard, could crown or praise an entry the mean did not put first. It costs one more lead turn per deciding round.
2. **Seat ids, not kind ids, for judges** (`judge`, `judge-2`, `judge-3`). The request names them so. They never collide with contestant ids.
3. **Mean stored as a rounded integer, exact mean derived.** `EntryEvaluation.score` keeps its type, so every consumer of `score` works. The exact mean and spread come from the scorecards. Ranking uses exact sums, and ties fall to the lead's order.
4. **The lead's texts are the round's texts.** The combined judgment uses the lead's overview, checklist and per-entry feedback. Co-judges' feedback reaches contestants through `contestant_brief` from their scorecards, labelled by judge.
5. **Checklist is the lead's alone.** Co-judges must leave it empty, so contestants get one published checklist.
6. **Amendments correct only the amending judge's own scores.** They apply when the amending round is combined.
7. **Spread warning at 15 points** (`SPREAD_WARN`). It is a display threshold only; nothing in the engine reacts to it.
8. **No new `Phase`.** The verdict step lives in `Judging`, told apart by `verdict_brief`. The step strip, `in_play` and the phase checks stay as they are.
9. **Single-judge sessions keep today's code path and log bytes** (L2-8), so the existing goldens prove nothing changed and older widgets still read single-judge logs.
10. **`compare.html`'s per-judge rendering is a separate script block emitted only for panels.** The existing five goldens stay byte-identical instead of being regenerated.
11. **Default judge count stays 1.** Users opt into panels, and the clean form and its tests do not change.
12. **New multi-judge goldens come from the first implementation, reviewed by hand** in that unit's PR, since no earlier output exists to pin against.

## Risks

- **Cost and time:** each round runs up to 3× the judge work, plus one verdict turn in deciding rounds. The default stays one judge, and the start view's caption says that each judge scores every round.
- **Blindness is by instruction:** judges share the round directory and could read each other's replies. Briefs forbid it and hold nothing from other judges. A judge that replies after another may still have read its file; nothing detects it, as with contestants today.
- **Replies are trusted by file name:** every seat writes its reply into the same `r<N>/` directory, and a reply is attributed to a seat by its file name only. A participating agent able to write that directory can write a reply under another seat's name, a co-judge's scorecard (`judge-2.json`) or the lead's verdict (`verdict.json`), and it is accepted as that seat's. The feature trusts the participating agents not to; nothing detects or prevents it.
- **The lead sees the panel after a verdict:** if the competition is extended after a verdict, the lead's later scores are no longer blind to that round's panel. This is accepted; the verdict brief is the only place other judges' scores reach any judge.
- **A stuck judge stalls the round:** C3 requires every judge, and the existing retry, manual winner and 終了 paths are the way out.
- **Older widgets and multi-judge logs:** an older build folds a multi-judge log until the first `scorecard_accepted` line and then lists it as unreadable. A log cut before that line would show `judge-2` as a contestant there. Downgrades are not supported; noted in `CHANGELOG.md`.
- **Viewer cache staleness:** a closed round's JSON is reused while its `RoundStamp` matches. Without per-judge scores in the stamp, an amendment that keeps the rounded mean would serve a stale panel. Covered by AC-18.
- **Literal churn in Unit 1:** every `Session`/`Round`/`Setup` literal across the crate (tests, demo, examples, golden scenario) gains fields. Doing all of it in Unit 1 is what lets Units 2 and 3 touch disjoint files.
- **`--all-targets` builds the examples:** a field added without updating `examples/*.rs` fails `verify.sh`. Unit 1's file list includes them.

## Needs the maintainer's agreement before implementation

Nothing under `AGENTS.md` Safety: no new dependency, no `Cargo.lock` change, no change to `scripts/verify.sh`, CI or `AGENTS.md`. The goldens that exist stay unchanged, and new ones are added.
