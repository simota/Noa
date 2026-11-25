# Agent Usage widget (エージェント使用量): Spec

| | |
|---|---|
| slug | agent-usage |
| status | draft (design only, 2026-11-24). No code yet. No owner sign-off |
| owner | simota |
| as-of | 2026-11-24 (HEAD 655eae2, branch `orch/design`) |
| crate | new `widgets/agent-usage` (binary `agent-usage`, bundled as `Noa Agent Usage`); integration in `crates/noa-app`, `scripts/`, docs |
| request | Noa が動かすコーディングエージェント (Claude Code, Codex CLI, Antigravity CLI `agy`) の使用量制限と、エージェント別の統計・グラフを出すウィジェット (AC1–AC8) |
| reference | `~/repos/github.com/llm-usage` (README, `docs/feasibility.md`, `linux/src/{model,claude,codex,agy,cli}.rs`), read only |

Acceptance criteria, as the request numbers them:

- AC1: standalone crate `widgets/agent-usage` (own `[workspace]`, noa-gpui), opened from View → Agent Usage and the palette ("Open Agent Usage"), launched like the other widgets (dev: `widgets/agent-usage/target`; bundle: `Contents/Helpers`), toast when it cannot be found or started.
- AC2: limits view for claude, codex, agy: each window with % used, reset time, pace tick, "over pace by N%", plan name when known, stale values marked with their age, independent failure, read-only, periodic and on-demand refresh.
- AC3: statistics per agent and combined from local logs, read-only: sessions, turns/messages, tokens by kind, per model, per project, per day, active time, tool calls, skill uses, estimated cost where priced; period today / 7d / 30d / all.
- AC4: graphs: daily tokens stacked by agent, per-model share, hour × weekday heatmap, cumulative trend, limit-usage history (samples recorded under `<data dir>/noa/agent-usage/`).
- AC5: `agent-usage --demo [--tab=limits|stats] --snapshot=FILE.png` renders canned data headlessly.
- AC6: parsers and aggregation unit-tested on fixture logs (malformed and partial lines included); `scripts/verify.sh` covers the crate (default and `--full`); `scripts/verify.sh --full` passes.
- AC7: `docs/FEATURES.md` documents the widget.
- AC8 (person): the window looks right on a real display; reported as unconfirmed.

## L0: Vision

- Problem: Noa runs claude, codex and agy side by side (orchestration, competition, review), and every one of them has its own rate limits and logs. Nothing in Noa says how close a limit is, whether the burn rate will hit it before it resets, or where the tokens went.
- Job to be done: one Noa window that answers "which limit runs out first, and when" (the llm-usage panel, all three sources) and "what did each agent spend, on what, when" (statistics from the local session logs).
- Success definition: on a machine logged into all three CLIs, View → Agent Usage shows three cards with live windows within ~15 s, the statistics tab fills within a few seconds on a large `~/.claude/projects` without the window stalling, and pulling the network cable (or logging out of one CLI) turns only the affected card stale, with its age.

## What exists today (read from the code and the machine, 2026-11-24)

- Widgets are crates outside the workspace with their own `[workspace]` and `Cargo.lock`, drawn with `noa-gpui-* 0.2.1` (`widgets/{agent-dashboard,repo-browser,discussion,competition,review}`). Each `main.rs` parses `--demo`, `--tab=`, `--snapshot=FILE.png`, `--size=WxH` and renders a `Screen` with `noa_gpui_platform::Headless::new_or_software()` (`widgets/agent-dashboard/src/main.rs` `snapshot`). Window titles follow the locale (`title(var, system_language)`). View prefs live in `<data dir>/noa/<widget>/view.json`.
- `noa_gpui_core::widgets::Chart` draws line and bar charts (`ChartKind::{Line,Bar}`, `.stacked(true)`, `ChartSeries::new(name, values).color(c)`, `f64::NAN` = missing). There is no pie or heatmap widget; custom drawing is a `Widget` with `paint(&mut PaintCx)` (`cx.bounds()`, `cx.line`, `cx.color(Role)`), as `widgets/competition/src/ui/map.rs` `Edges` does. `noa_gpui_core::date::DateTime` has civil-date arithmetic (`weekday`, `add_days`).
- Launch: `crates/noa-app/src/helper_program.rs` (`HelperProgram { title, bundled, crate_name }`, `open`, `locate` in `Contents/Helpers` then `widgets/<crate>/target/{debug,release}`, stderr tail → `on_failure` toast). Wiring per widget: `commands/command.rs` (enum variant, `*_MENU_ID`, `menu_id`, `from_menu_id`, `action_name`, `from_action_name`), `command_palette.rs` (title, description, list, `CommandCategory::View`, test list), `app/commands.rs` (`open_helper_program`), `app/helpers/dispatch.rs` (two scope matches), `split_tree/tree/commands.rs`, `macos_menu.rs` (View menu item), `commands/tests.rs`. The View menu exists only on macOS; on Linux the palette and keybind actions are the way in.
- `scripts/bundle-macos.sh` builds each widget (`for widget in …`) and copies it to `Contents/Helpers/Noa <Name>`. `scripts/package-linux.sh` ships no widget, so AC1's Linux clause does not apply.
- `scripts/verify.sh` builds each widget with `--all-targets` everywhere and runs its tests on macOS only (`widget_check`), and runs a widget's check when a changed path is under it or under a Noa crate it uses (`widget_uses`).
- The lock files hold `serde`, `serde_json`, `libc`, `dirs 6.0.0`, `tempfile 3.27.0`, `walkdir`, `crossbeam-channel`, `png`, `tiny-skia` in every widget lock, and `jiff 0.2.31` in the root lock only. No HTTP client, no SQLite, no protobuf crate.
- llm-usage (`linux/src`): one normalised model (`UsageWindow { id, label, used_percent, resets_at, window_minutes, is_active }`, `UsageSource { plan, windows, buckets, spend, note, last_updated, state, stale_after }`, `SourceState::{Ok, Stale{since}, Error, Unconfigured}`), pace math (`elapsed_fraction`, `pace_delta`, `projected_exhaustion`, over-pace threshold 0.10, severity 60/80/95 %), one provider thread per source, 300 s polls, last good sample kept and shown stale on failure.

## Data sources (verified on this machine; field names only, no content)

### Limits

| Agent | How | Exact source | Fields used |
|---|---|---|---|
| claude | `GET https://api.anthropic.com/api/oauth/usage` through `/usr/bin/curl` (unofficial endpoint) | token: macOS `/usr/bin/security find-generic-password -s "Claude Code-credentials" -w` (stdout JSON), else and on Linux `$CLAUDE_CONFIG_DIR/.credentials.json` or `~/.claude/.credentials.json` (present here, mode untouched) | credentials: `claudeAiOauth.accessToken`, `.expiresAt` (ms epoch), `.subscriptionType` (plan, e.g. `max`), `.rateLimitTier`. Response: `limits[]{kind: session/weekly_all/weekly_scoped/…, percent, resets_at (RFC 3339, fractional, offset), is_active, scope.model.display_name}`; fallback `five_hour/seven_day{utilization, resets_at}`; `spend{enabled, used/limit{amount_minor, currency, exponent}, percent}` |
| codex | `codex app-server` child, newline-delimited JSON-RPC on stdio: `initialize` → notify `initialized` → `account/read` → `account/rateLimits/read`; push `account/rateLimits/updated` triggers a re-read | `codex` resolved as below | `rateLimits{limitId, planType, primary/secondary{usedPercent, windowDurationMins, resetsAt (unix s)}, credits{hasCredits, unlimited, balance}, individualLimit{used, limit, remainingPercent}, rateLimitReachedType}`, `rateLimitsByLimitId{<id>: same}`, `account{email, planType}` |
| codex (fallback) | last `event_msg`/`token_count` of the newest rollout file | `~/.codex/sessions/YYYY/MM/DD/rollout-*.jsonl` | `payload.rate_limits{limit_id, primary/secondary{used_percent, window_minutes, resets_at}, plan_type, credits}` (snake_case) and the line's `timestamp` as the sample time |
| agy | `agy --version` (must be ≥ 1.1.11; older ones may run `/usage` as a model prompt), then `agy -p /usage --output-format json --print-timeout 10s` | `agy` resolved as below (1.5.16 here) | `status == "SUCCESS"`, `num_turns == 0`, `command.name == "usage"`, `command.data.groups[]{name, buckets[]{id, window: "5h"/"weekly", remaining_fraction (0..1, what remains), reset_time (RFC 3339)}}`. No plan or account |

Rules taken over from llm-usage:

- The Claude token is never refreshed (the refresh token rotates server-side; refreshing here would break the user's Claude Code login). Expired (`expiresAt` ≤ now) or HTTP 401/403 → the card goes stale with "Token expired (launch Claude Code to renew)". The credential is re-read every cycle.
- `User-Agent: claude-code/<version>` is mandatory (without it the endpoint throttles hard). `<version>` is the `version` field of the newest record in `~/.claude/projects/**/*.jsonl` (`2.1.331` here), else the first word of `claude --version`, else `0.0.0`.
- Poll every 300 s per source; Claude never more often than every 180 s, manual refresh included (a refresh inside that gap is deferred and the card says when it runs). 429 or no connection backs off 30 → 60 → 120 → 300 s, or `Retry-After` when given.
- Severity and pace: `elapsed = clamp((now − (resets_at − window)) / window, 0, 1)`, `pace_delta = used/100 − elapsed`; over-pace text when `pace_delta > 0` rounds to ≥ 1 %, severity escalated to critical above 0.10; `stale_after` claude 900 s, codex 600 s, agy 900 s; a window whose `resets_at` passed after the sample is stale at once.

How Claude's request runs (no HTTP crate exists in the locks):

```text
/usr/bin/curl --silent --show-error --max-time 15 --proto =https --max-redirs 0 \
  --max-filesize 1048576 --dump-header - --write-out '\n%{http_code}' --config - \
  https://api.anthropic.com/api/oauth/usage
stdin (then closed):  header = "Authorization: Bearer <token>"
                      header = "User-Agent: claude-code/<version>"
```

The token reaches curl only through its stdin config (`--config -`), so it is in no argument list (`ps`) and no environment. `curl` is `/usr/bin/curl` (present on macOS and here); if absent, `curl` from the resolved PATH; if none, the card says "curl not found". stderr is dropped; the response is split into header block (status, `Retry-After`) and body; a non-200 never shows the body. The token string is never formatted into a log, error, note or file.

Tool resolution (claude, codex, agy, curl fallback): the inherited `PATH`, then `~/.local/bin`, `/opt/homebrew/bin`, `/usr/local/bin`, `~/.npm-global/bin`, `~/.bun/bin`, `~/.cargo/bin`, then (once, cached) the login shell's PATH from `$SHELL -lic 'printf "\nnoa-usage-path=%s\n" "$PATH"'` with a 5 s timeout, as llm-usage's `cli::resolve` and competition's `installed.rs` do (Noa started from Finder has a bare PATH). Every child runs with a timeout (security 10 s, curl 20 s, agy 3 s/12 s, codex requests 20 s), stdin closed unless used, stdout capped at 1 MiB, and is killed and reaped on timeout or on exit of the widget.

### Statistics (all read-only; nothing is ever written under these roots)

| Agent | Files | Record → meaning |
|---|---|---|
| claude | `$CLAUDE_CONFIG_DIR/projects` or `~/.claude/projects`: `<encoded-cwd>/<sessionId>.jsonl` and `<encoded-cwd>/<sessionId>/subagents/agent-*.jsonl` (26 files, 51 MiB here) | `type == "assistant"`: `message.id`, `requestId`, `message.model` (skip `<synthetic>`), `message.usage{input_tokens, output_tokens, cache_read_input_tokens, cache_creation_input_tokens, cache_creation{ephemeral_5m_input_tokens, ephemeral_1h_input_tokens}, output_tokens_details.thinking_tokens}`, `message.content[]{type: tool_use, id, name}`, `timestamp` (RFC 3339 `Z`), `sessionId`, `cwd`, `isSidechain`, `version`. One API message is written as several lines (one per content block) carrying the same `message.id` and the same usage: usage counts once per `message.id` (59 lines → 32 ids in the sampled file). `type == "user"` with `message.content` a string or holding a `text` block, and no `toolUseResult`: one turn. Every other `type` (`attachment`, `api-request`, `api-request-blob`, `ai-title`, `mode`, …) is skipped |
| codex | `$CODEX_HOME/sessions` or `~/.codex/sessions/YYYY/MM/DD/rollout-<ts>-<uuid>.jsonl`, plus `~/.codex/archived_sessions/` when present (28 files, 234 MiB here) | `session_meta.payload{id, cwd, cli_version, parent_thread_id}`: one session (a thread with `parent_thread_id` is a sub-thread of it); `turn_context.payload{model, cwd, effort}` sets the model for the records after it; `token_usage_record.payload{response_id, usage{input_tokens, cached_input_tokens, cache_write_input_tokens, output_tokens, reasoning_output_tokens}}`: one model response (when a file has none, `event_msg`/`token_count` `payload.info.last_token_usage` instead, skipping one whose `info.total_token_usage` equals the previous); `event_msg`/`task_started`: one turn; `response_item`/`message` with `role == "assistant"`: one message; `response_item` of type `function_call`, `custom_tool_call`, `local_shell_call`, `web_search_call`: one tool call (`name` when present). `input_tokens` includes `cached_input_tokens`: fresh input = input − cached |
| agy | `~/.gemini/antigravity-cli/history.jsonl`; `~/.gemini/antigravity-cli/conversations/<uuid>.db` (file count and mtimes only) | history line `{timestamp (ms epoch), workspace, conversationId?, type?}`: a line without `type == "slash_command"` is one turn; sessions = distinct `conversationId`s plus `.db` files never named in history (first/last activity from the file's mtime). The `display` field (the prompt text) is never read into any structure. Tokens, models and tool calls are not available: `conversations/*.db` are SQLite files whose rows are protobuf blobs (`steps.step_payload`, `gen_metadata.data`), there is no SQLite or protobuf reader in the locks and no `sqlite3` binary on this Linux machine; `log/cli-*.log` hold no token counts |
| Noa | `<data dir>/noa/ctl/*.jsonl` (`noa_config::ctl_journal_path` layout; 4 files here) | `{"ev":"spawn","ts" (unix s),"argv":[…],"tier","worktree","label","pane"}`: one worker of the agent `basename(argv[0])` when it is `claude`, `codex` or `agy` (others, e.g. `./scripts/verify.sh`, are not agents) |

Skill uses (only the field names and values below are read; no prompt, message or tool output text is kept, and a skill name is kept only once the record proves it is a skill):

| Agent | What counts as one use | Not counted |
|---|---|---|
| claude | (a) an `assistant` `message.content[]` block `{type: "tool_use", name: "Skill", input: {skill}}`, once per `tool_use.id`, named by `input.skill` (a leading `/` dropped; `plugin:skill` kept). (b) a slash command that ran a skill: a `user` record whose string content starts with `<command-message>` or `<command-name>` and holds `<command-name>/NAME</command-name>`, counted only when the next `user` record has `isMeta: true`, no `sourceToolUseID`, `parentUuid` equal to the command's `uuid`, and a `text` block starting with `Base directory for this skill:`. Claude Code writes that record for a skill and never for a built-in (sampled here: 212 of 212 skill commands followed by it, 0 of 179 other commands: `clear`, `reload-skills`, `goal`, `model`, `reload-plugins`, `effort`, `compact`, `login`) | built-in commands; the skill record of a `Skill` call (`sourceToolUseID` set), which is the same use as (a); text that only mentions a command (content not starting with the command tags) |
| codex | an `event_msg`/`item_completed` with `item.type == "CommandExecution"` and an `item.parsed_cmd[]` entry `{type: "read", path}` whose `path` is absolute or `~/`-led, ends in `skills/<name>/SKILL.md` or `skills/.system/<name>/SKILL.md`, and has a `.codex` or `.agents` directory above `skills` (`~/.codex/skills`, a plugin under `~/.codex/plugins/cache`, a repo's `.agents/skills`); once per skill per turn (`payload.turn_id`; `task_started` also starts a new set), since a skill's file is often read twice. Codex has no skill event: it uses a skill by reading its `SKILL.md` (182 such reads here, all `read`, all of a skill directory) | the skills list in the instructions (`developer` message, `session_meta`, `world_state`), which names every skill's path and is never searched; `search`/`list_files` entries; relative paths; `SKILL.md` outside a `.codex`/`.agents` tree. `item_completed` lines are not activity events (active time is unchanged) |
| agy | unavailable | `history.jsonl` marks a slash command only with `type: "slash_command"`; its name is in `display`, the prompt text, which is never read. `skills/` lists the installed skills, not uses; the conversation stores are unreadable (above) |

Each use is kept per (UTC hour, skill, session key, project) with its count and latest time, and adds to the hour bucket's `skills` counter, so per-agent, per-model and per-project totals include it. A Codex sub-thread's use counts toward its root session.

`<data dir>` is `dirs::data_dir()`: `~/.local/share` on Linux, `~/Library/Application Support` on macOS.

## Glossary

| Term | Meaning | Identifier |
|---|---|---|
| source | one agent's limits feed | `UsageSource` (`agent: Agent`) |
| window | one rate-limit window (5h, 7d, a model-scoped bucket's window) | `UsageWindow` |
| pace tick | the window's elapsed fraction drawn on its gauge | `UsageWindow::elapsed_fraction` |
| over pace | used % ahead of elapsed %; shown as "+N% over pace" | `UsageWindow::pace_delta` |
| stale | a value older than `stale_after`, or one whose window reset after it was read | `SourceState::Stale { since }` |
| index | the per-hour aggregate of every parsed log file, kept on disk | `stats::Index`, `index-v1.json` |
| skill use | one invocation of an agent skill, by the rules under Statistics | `Counters::skills`, `SkillRow` |
| hour bucket | counters for one (UTC hour, agent, model, project) | `stats::types::Bucket` |
| active time | sum of gaps ≤ 5 min between consecutive events of one session | `Counts::active_secs` |
| limit sample | one window's used % at one time, appended to the history file | `LimitSample` |
| estimated cost | API list price × tokens; what an API key would have paid, not a subscription bill | `Counts::cost_usd` |

## L1: Requirements

### L1-1. Crate and launch (AC1)

1. `widgets/agent-usage` is a crate of its own (`[workspace]` in its manifest, its own `Cargo.lock`), binary `agent-usage`, library `agent_usage` (everything but the UI). Dependencies: `noa-gpui-platform 0.2.1` (`native`), `noa-gpui-core`, `noa-gpui-scene`, `noa-gpui-text` (0.2.1), `serde` (derive), `serde_json`, `libc`, `dirs = "6"`; macOS `objc2-foundation` (title locale, as the others); dev `tempfile = "3"`. No `noa-ipc`: the widget needs neither the control server nor `server-enable`.
2. Noa: `AppCommand::OpenAgentUsage`, menu id `noa.view.open-agent-usage`, action `agent-usage.open`, palette title "Open Agent Usage", description "Show Claude Code, Codex and Antigravity usage limits and statistics in a window of its own.", category View; macOS View menu item "Agent Usage" after "Review"; `helper_program::AGENT_USAGE = { title: "Agent Usage", bundled: "Noa Agent Usage", crate_name: "agent-usage" }`, no launch arguments. Not found / not started / exited with an error → the existing toast path (`open_helper_program`).
3. `scripts/bundle-macos.sh` builds it and copies `$TARGET_DIR/agent-usage` to `Contents/Helpers/Noa Agent Usage`, with the same `[ -x ]` check. `scripts/package-linux.sh` is unchanged (it ships no widget).
4. Window: title "Noa · エージェント使用量" for a Japanese locale, else "Noa · Agent Usage" (the dashboard's `title` rule); `AppOptions::new("Noa Agent Usage")`, 1280 × 860. Two tabs: 制限 (limits) and 統計 (statistics). UI labels are Japanese like the other widgets; reset/over-pace notation is llm-usage's English (`in 3h08m`, `11/28(Fri)`, `+27% over pace`).

### L1-2. Limits view (AC2)

1. One card per agent in the fixed order Claude Code, Codex, Antigravity. Header: name, plan (`Max`, `Pro`, … capitalised from `subscriptionType` / `planType`; none for agy), state mark, and "N分前に取得" / "updated 2m ago". The account e-mail is not shown (screenshots get shared).
2. One row per window: label (`5h`, `7d`, `Spark 5h`, `Gemini 5h`, `Claude/GPT 7d`), a gauge filled to used % in the severity colour with the pace tick (▽ at `elapsed_fraction`, omitted when the window length or reset is unknown), `NN%`, the reset (`in 48m` / `in 3h08m` / `11/28(Fri)` / `now` / `—`), and under it "+N% over pace · maxes out 11/27(Thu) at this pace" when over pace. Claude `weekly_scoped` limits are rows under the 7d row (indented, no pace: they carry no window length). A spend allowance (Claude credits, Codex spend) is a last row with `$79.71`-style detail and no pace tick. Codex notes (limit reached, free resets, credit balance) and agy's "Shared with the desktop app and SDK" go under the rows.
3. Above the cards, a summary line: the window that runs out first among current (non-stale) sources by `projected_exhaustion`, e.g. "Codex 7d 56% · maxes out 11/28(Fri) at this pace", else "No limit is on course to run out"; nothing current → "現在値なし".
4. Stale: the card's rows and gauges are drawn at 50 % opacity colours (`Role::TextSecondary` instead of severity), the state mark is an outline, and the header says "古い値 · 12分前" with the reason (note) on the next line. An error with no earlier value: the card shows only the reason ("agy not found (install Antigravity CLI)", "Token expired (launch Claude Code to renew)", "codex not found in PATH", …). Unconfigured (CLI absent and no log fallback): "未設定" and how to set it up.
5. Codex falls back to the newest rollout's `token_count.rate_limits` when `codex` is missing or the app-server fails: the card shows those windows with "ログから · 3時間前" and stale styling once past `stale_after`.
6. Each source runs on its own thread; a failure, a hang (timeouts above) or a dead thread affects only its card (a disconnected channel turns it into `Error("internal error")`). Refresh: automatic per L1-2 rules, plus 更新 (button, `R`) which asks every source at once.
7. Every successful, fresh sample of every window is offered to the history store (L1-4.5).

### L1-3. Statistics view (AC3)

1. Controls: period (今日 / 7日 / 30日 / 全期間, `P` cycles) and agent (全体 / Claude / Codex / agy, `A` cycles). Periods are local calendar days: today = since local midnight; 7d / 30d = today and the 6 / 29 days before; all = everything indexed.
2. KPI row for the selection: sessions, turns, messages, tool calls, active time, total tokens (and by kind: input, output, cache read, cache write, reasoning), estimated cost (USD, "API換算" caption). A value an agent cannot provide is "—", never 0 (agy tokens, messages, tool calls, cost; Codex cost).
3. Tables: per agent (all counts), per model (agent, model, tokens by kind, messages, cost), per project/cwd (top 15 by tokens, then "その他 N件"; agy by `workspace`; with the project's skill uses, "—" when only agy is selected), top tools (name, count; Claude `tool_use.name`, Codex call `name`), skills (スキル: skill, agent, uses, distinct sessions, distinct projects, last use as local "MM/DD HH:MM"; most used first, top 15 then "その他 N件"; "agy はスキル使用を記録しません" under it when agy is selected), Noa workers spawned per agent (from the ctl journals) in the period.
4. Parsing never blocks the UI: an indexing pass runs on a background thread; until the first pass ends the view shows what the on-disk index already has with "集計中 n / m ファイル", and a pass is re-run every 60 s (stat only; changed files parsed) and on 更新.
5. Cost: tokens × the built-in price table (L2-6); models not in the table count as unpriced, and the cost cell says "一部未計上 (N tokens)" when any are. Claude cache writes are priced by TTL (5m / 1h).

### L1-4. Graphs (AC4)

1. 日別トークン: bar chart, one bar per local day of the period (today: per hour), stacked by agent (claude, codex; agy has no tokens and is not a series), `Chart::bar(labels).stacked(true)`. For 全期間 longer than 92 days, one bar per ISO week.
2. モデル別シェア: one horizontal 100 % bar split by model (token share), with a legend of model, tokens and percent; models under 2 % fold into "その他". Custom `ShareBar` widget.
3. 時間帯ヒートマップ: 7 rows (月–日) × 24 columns (0–23 local), cell = turns + messages in that slot for the period, colour scaled to the maximum cell, with row/column labels and the max in a caption. Custom `Heatmap` widget.
4. 累積トレンド: line chart of cumulative tokens per agent over the period's days (`Chart::line`).
5. 制限の推移: line chart of used % (0–100 fixed range) over the period for every window seen in the history file, one series per window (`Claude 5h`, `Codex 7d`, …); a gap of more than 2 × the poll interval breaks the line (`NAN`).
6. The history file is `<data dir>/noa/agent-usage/limits.jsonl`, one `LimitSample` per line, appended with `O_APPEND` (lines < 512 bytes, so concurrent writers do not interleave). A sample is written only when the window's value is fresh (`SourceState::Ok`, `Origin::Live`) and either 5 min have passed since that window's last sample or its used % changed by ≥ 1 point. At start, samples older than 400 days are dropped by rewriting the file (temp file + rename) when more than 10 % of it is old.

### L1-5. Headless rendering (AC5)

`agent-usage [--demo] [--tab=limits|stats] [--period=today|7d|30d|all] [--agent=all|claude|codex|agy] [--prefs=FILE] [--snapshot=FILE.png [--size=WxH]]`. `--demo` uses canned sources, a canned statistics report and canned history, with a fixed clock (`2026-11-24T12:00:00+09:00`) and fixed time zone (+09:00), so a snapshot is the same bytes on every run of one platform. `--snapshot` renders with `Headless::new_or_software()` at scale 2 and exits; without `--demo` it is refused ("--snapshot needs --demo"), so no snapshot ever starts CLIs or reads credentials. The snapshot never reads or writes prefs unless `--prefs` is given. Tab, period and agent persist in `<data dir>/noa/agent-usage/view.json`; flags win over it; a damaged file falls back to defaults.

### L1-6. Tests and verification (AC6)

Fixture logs (hand-written, synthetic: no real prompts, paths or ids) cover every record kind in the Data sources tables, plus: a malformed JSON line, a line cut without `\n` at end of file, a line over the 8 MiB cap, a duplicated `message.id`, a subagent file, a Codex file without `token_usage_record`, an agy history with slash commands and missing `conversationId`, a ctl journal with non-agent spawns. Library tests (`agent_usage::*`) do not touch fonts or GPUs and run on Linux as well as macOS (L2-9).

### L1-7. Documentation (AC7)

`docs/FEATURES.md` gets an **Agent Usage** entry after **Review** (what each tab shows, where each number comes from, the read-only rules, `curl`/keychain use, stale marking, files written under `<data dir>/noa/agent-usage/`, flags, toast, bundle/dev locations). `CHANGELOG.md` Unreleased gets one entry. `AGENTS.md`'s widget list in Verification gains `widgets/agent-usage`.

## L2: Detail

### L2-1. Crate layout and file owners

```text
widgets/agent-usage/
  Cargo.toml  Cargo.lock  .gitignore                 D0
  src/lib.rs        module list (final)               D0
  src/model.rs      limits types + pace math + labels D0 (contract)
  src/time.rs       RFC 3339, civil dates, Tz         D0 (contract)
  src/paths.rs      every root and file path          D0 (contract)
  src/stats/types.rs  stats query/report types        D0 (contract)
  src/limits/mod.rs   start/handle                    A (D0 stub)
  src/limits/{claude,codex,agy,curl,cli}.rs           A
  src/limits/testdata/*                               A
  src/stats/mod.rs    Indexer, report                 B (D0 stub)
  src/stats/{claude,codex,agy,journal,index,aggregate,prices}.rs  B
  src/stats/testdata/**                               B
  src/history.rs      limit sample store              B (D0 stub)
  src/main.rs         flags, title, snapshot, App     C (D0 stub)
  src/demo.rs  src/ui/{mod,limits,stats,charts,style,prefs}.rs   C
  tests/snapshot.rs                                   C
```

`lib.rs` (D0, final): `pub mod model; pub mod time; pub mod paths; pub mod limits; pub mod stats; pub mod history;`. `stats/mod.rs` declares `pub mod types;` plus B's modules; D0's stub declares only `types`.

`Cargo.lock` starts as a copy of `widgets/competition/Cargo.lock` with the package renamed; D0 runs `cargo build --offline` (no `--locked`) once so cargo drops the packages the manifest does not use and adds only locked-elsewhere versions, then checks that every `name version` pair of the new lock occurs in `Cargo.lock` or a `widgets/*/Cargo.lock` (script in D0's done condition). After that, `--locked` everywhere.

### L2-2. `src/model.rs` (contract, D0)

Ported from llm-usage `model.rs` (types and math; no jiff: times are `i64` unix seconds).

```rust
pub type Unix = i64;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Agent { Claude, Codex, Agy }
impl Agent {
    pub const ALL: [Agent; 3];
    pub fn id(self) -> &'static str;           // "claude" | "codex" | "agy"
    pub fn display(self) -> &'static str;      // "Claude Code" | "Codex" | "Antigravity"
    pub fn parse(s: &str) -> Option<Agent>;
    pub fn index(self) -> usize;               // 0, 1, 2
}

pub struct UsageWindow { pub id: String, pub label: String, pub used_percent: f64,
    pub resets_at: Option<Unix>, pub window_minutes: Option<i64>, pub is_active: bool }
impl UsageWindow {
    pub fn elapsed_fraction(&self, now: Unix) -> Option<f64>;
    pub fn pace_delta(&self, now: Unix) -> Option<f64>;
    pub fn projected_exhaustion(&self, now: Unix) -> Option<Unix>;
    pub fn severity(&self, now: Unix) -> Severity;   // from used %, escalated over pace
}
pub struct UsageBucket { pub id: String, pub label: String, pub used_percent: f64 }
pub struct UsageSpend  { pub label: String, pub used_percent: f64, pub detail: String }
pub enum SourceState { Ok, Stale { since: Unix }, Error(String), Unconfigured }
pub enum Origin { Live, Log }
pub struct UsageSource { pub agent: Agent, pub plan: Option<String>, pub windows: Vec<UsageWindow>,
    pub buckets: Vec<UsageBucket>, pub spend: Option<UsageSpend>, pub notes: Vec<String>,
    pub last_updated: Option<Unix>, pub state: SourceState, pub stale_after: i64, pub origin: Origin }
impl UsageSource {
    pub fn placeholder(agent: Agent) -> Self;           // Unconfigured, no windows
    pub fn aged_state(&self, now: Unix) -> SourceState; // Ok → Stale by age or rollover
    pub fn is_current(&self, now: Unix) -> bool;
}
pub enum Severity { Normal, Caution, Warning, Critical }  // <60, <80, <95, ≥95
pub const OVER_PACE_THRESHOLD: f64 = 0.10;
pub fn binding_window(sources: &[UsageSource], now: Unix) -> Option<(Agent, &UsageWindow)>;

#[derive(Serialize, Deserialize)]
pub struct LimitSample { pub ts: Unix, pub agent: Agent, pub window_id: String, pub label: String,
    pub used_percent: f64, pub resets_at: Option<Unix>, pub window_minutes: Option<i64> }

pub mod format {   // all take a Tz for local dates
    pub fn resets_in(at: Option<Unix>, now: Unix, tz: &dyn Tz) -> String;   // "in 48m" …
    pub fn over_pace(w: &UsageWindow, now: Unix) -> Option<String>;          // "+27% over pace"
    pub fn exhaustion(w: &UsageWindow, now: Unix, tz: &dyn Tz) -> Option<String>;
    pub fn relative(at: Option<Unix>, now: Unix) -> String;                  // "12s ago"
    pub fn percent(v: f64) -> String;                                        // half away from zero
}
```

All structs derive `Clone, Debug, PartialEq`; the ones written to disk also `Serialize, Deserialize`.

### L2-3. `src/time.rs` and `src/paths.rs` (contracts, D0)

```rust
pub trait Tz: Send + Sync { fn offset_secs(&self, at: Unix) -> i32; }
pub struct SystemTz;          // libc::localtime_r → tm_gmtoff
pub struct FixedTz(pub i32);
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct LocalDate { pub year: i32, pub month: u8, pub day: u8 }
pub struct LocalTime { pub date: LocalDate, pub hour: u8, pub weekday: u8 /* 0 = Monday */ }
pub fn parse_rfc3339(s: &str) -> Option<Unix>;      // Z / ±hh:mm, optional fraction
pub fn local(at: Unix, tz: &dyn Tz) -> LocalTime;
pub fn day_start(date: LocalDate, tz: &dyn Tz) -> Unix;
pub fn add_days(date: LocalDate, days: i64) -> LocalDate;
pub fn now() -> Unix;
```

```rust
pub struct Roots { pub claude_projects: Option<PathBuf>, pub claude_credentials: Option<PathBuf>,
    pub codex_sessions: Vec<PathBuf>, pub agy_dir: Option<PathBuf>, pub ctl_dir: Option<PathBuf>,
    pub own_dir: Option<PathBuf> }                    // <data dir>/noa/agent-usage
impl Roots { pub fn system() -> Roots; pub fn under(home: &Path, data_dir: &Path) -> Roots; }
pub fn index_path(own: &Path) -> PathBuf;            // index-v1.json
pub fn history_path(own: &Path) -> PathBuf;          // limits.jsonl
pub fn prefs_path(own: &Path) -> PathBuf;            // view.json
```

`Roots::system` honours `CLAUDE_CONFIG_DIR` and `CODEX_HOME`. Only `own_dir` is ever created or written.

### L2-4. `src/stats/types.rs` (contract, D0)

```rust
pub enum Period { Today, Days7, Days30, All }      // parse "today" | "7d" | "30d" | "all"
pub enum AgentFilter { All, One(Agent) }
#[derive(Default, Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Tokens { pub input: u64 /* fresh, uncached */, pub output: u64 /* incl. reasoning */,
    pub cache_read: u64, pub cache_write: u64, pub cache_write_1h: u64 /* ⊂ cache_write */,
    pub reasoning: u64 /* ⊂ output */ }
impl Tokens { pub fn total(&self) -> u64 /* input+output+cache_read+cache_write */; pub fn add(&mut self, o: &Tokens); }
#[derive(Default, Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Counters { pub turns: u64, pub messages: u64, pub tool_calls: u64, pub active_secs: u64, pub tokens: Tokens }
pub struct Bucket { pub hour: Unix /* UTC hour start */, pub agent: Agent, pub model: u32, pub project: u32, pub c: Counters }
pub struct SessionSpan { pub agent: Agent, pub key: u64, pub project: u32, pub hours: Vec<Unix> }
pub enum Availability { Full, NoTokens, Missing(String) }  // agy = NoTokens
pub struct Counts { pub sessions: u64, pub c: Counters, pub cost_usd: Option<f64>, pub unpriced_tokens: u64, pub workers: u64 }
pub struct ModelRow { pub agent: Agent, pub model: String, pub counts: Counts }
pub struct KeyRow { pub key: String, pub counts: Counts }
pub struct Coverage { pub files_total: u64, pub files_done: u64, pub lines_bad: u64, pub lines_skipped: u64, pub running: bool }
pub struct StatsReport {
    pub range: (Unix, Unix), pub total: Counts,
    pub per_agent: Vec<(Agent, Counts, Availability)>, pub per_model: Vec<ModelRow>,
    pub per_project: Vec<KeyRow>, pub other_projects: u64, pub tools: Vec<(String, u64)>,
    pub days: Vec<(String /* label */, [u64; 3] /* tokens per Agent::index */)>,
    pub cumulative: Vec<(String, [u64; 3])>, pub heatmap: [[u64; 24]; 7],
    pub coverage: Coverage }
```

### L2-5. Interfaces between units

Unit A (`limits/mod.rs`):

```rust
pub struct LimitsHandle;                       // Drop stops every thread and child
pub fn start(roots: Roots, wake: Arc<dyn Fn() + Send + Sync>) -> LimitsHandle;
impl LimitsHandle {
    pub fn refresh(&self);                     // ask every source now (Claude: ≥180 s gap)
    pub fn drain(&self) -> Vec<UsageSource>;   // updates since last call, newest last per agent
}
```

D0 stub: `start` returns a handle whose `drain` returns `[]`.

Unit B (`stats/mod.rs`, `history.rs`):

```rust
pub struct StatsHandle;
pub fn start(roots: Roots, wake: Arc<dyn Fn() + Send + Sync>) -> StatsHandle;
impl StatsHandle {
    pub fn rescan(&self);
    pub fn index(&self) -> Arc<Index>;         // latest; cheap clone of an Arc
}
pub struct Index;                              // buckets, sessions, interned strings, workers, coverage
pub fn report(index: &Index, period: Period, filter: AgentFilter, now: Unix, tz: &dyn Tz) -> StatsReport;

// history.rs
pub struct History;
impl History {
    pub fn open(path: PathBuf) -> History;     // missing file = empty
    pub fn offer(&mut self, source: &UsageSource, now: Unix) -> std::io::Result<()>; // L1-4.6 throttle
    pub fn samples(&self, since: Unix) -> &[LimitSample];
}
pub struct Series { pub label: String, pub points: Vec<(Unix, f64)> }
pub fn series(samples: &[LimitSample], range: (Unix, Unix), max_gap: i64) -> Vec<Series>;
```

D0 stub: an empty `Index`, `report` returning an empty report, `History` holding nothing and writing nothing.

Unit C calls only these. On each `wake`, C drains limits, offers each fresh source to `History`, and rebuilds the stats report from `index()` when the index's generation changed or the period/filter changed.

### L2-6. Unit B internals

- Incremental parse: per file the index keeps `{path, len, mtime, head_hash (first 4 KiB), offset, last_event_ts, tail state}`. A file that grew with the same head resumes at `offset`; a shrunk file or a changed head is parsed again from 0 and its earlier buckets removed (buckets carry a file id). Only complete lines (ending in `\n`) advance `offset`; a cut last line is read again next pass. Lines over 8 MiB are skipped and counted (`lines_skipped`); lines that fail to parse are counted (`lines_bad`) and skipped.
- Claude dedupe: per file a set of `hash(message.id)` and of `hash(tool_use.id)` in the index; usage counts once per message id, tool calls once per tool-use id. Subagent files count toward their `sessionId`'s session.
- Typed lean decoding (`#[derive(Deserialize)]` structs with only the fields used, `#[serde(default)]`), so ignored large fields (`api-request-blob`, tool results) are skipped without building values. No prompt or result text is copied into any structure.
- Active time: per session, events sorted by time; a gap ≤ 300 s adds to the hour bucket of the later event; the last event time per file is kept for resumes.
- Index file `<own_dir>/index-v1.json` (`{"version":4, …}`; 4 added skill uses), written via temp file + rename at most every 30 s and at the end of a pass; another version or a damaged file is ignored and rebuilt. Interned model and project strings are stored once.
- Projects: Claude `cwd`, Codex `session_meta.cwd` (or the latest `turn_context.cwd`), agy `workspace`; shown with `~` for the home directory.
- Prices (`prices.rs`), USD per million tokens, Anthropic first-party list prices as published (cached 2026-11-03), matched on the exact model id or the id followed by `-<date>`:

| model | input | output | cache write 5m | cache write 1h | cache read |
|---|---|---|---|---|---|
| claude-opus-5-5 | 4.00 | 20.00 | 5.00 | 8.00 | 0.20 |
| claude-sonnet-5-5 | 2.00 | 10.00 | 2.50 | 4.00 | 0.10 |
| claude-haiku-5-5 | 0.10 | 0.50 | 0.125 | 0.20 | 0.01 |
| claude-fable-5-2 | 8.00 | 40.00 | 10.00 | 16.00 | 0.20 |

Writes are 1.25× / 2× input. Haiku 5.5's cache rows are derived from those standard multipliers (input 0.1× for a read) and its input/output are its ≤ 100K-prompt rates; both are marked `// derived` in the table. Codex models (`gpt-6.2-sol`, `gpt-6.1-sol`, `gpt-6.2-astra`, `codex-auto-review` here) have no table: their tokens count as unpriced.

### L2-7. Unit A internals

- `cli.rs`: `resolve(tool) -> Option<Resolution { path, env }>` (L1 rules, cached), `run(path, args, stdin: Option<&[u8]>, timeout, ok_codes) -> Result<Vec<u8>, RunFailure { NotRun, TimedOut, Exit(i32), TooLarge }>`; children in their own process group, killed and reaped on timeout and on handle drop.
- `curl.rs`: `config_text(token, user_agent) -> String` (escapes `\` and `"`), `args(url) -> Vec<String>`, `parse(raw) -> HttpResponse { status, retry_after, body }`; `type HttpFn = Arc<dyn Fn(&str, &str, &str) -> HttpResponse + Send + Sync>` injected for tests.
- `claude.rs`: credential read (`Credential::{Token{token, plan}, Expired, Missing, Unreadable(String), Malformed}`), normalise (llm-usage `claude.rs` `normalise`, windows `claude-5h`/`claude-7d`, scoped buckets, spend), retry state machine. Plan from `subscriptionType`.
- `codex.rs`: app-server session (one long-lived child, generation counter, request timeouts, reconnect on EOF), normalise (llm-usage `codex.rs`), log fallback (newest rollout by mtime, last 256 KiB scanned backwards for a `token_count` with `rate_limits`).
- `agy.rs`: version gate, `/usage` envelope validation, normalise (`remaining_fraction` → used %, `weekly` → `7d`/10080 min, `5h` → 300 min, group names shortened "Gemini Models" → "Gemini", "Claude and GPT models" → "Claude/GPT").
- Each source keeps its last good `UsageSource` and republishes it as `Stale { since }` with the failure as a note.

### L2-8. Unit C internals

- `ui/mod.rs`: `AgentUsage` implements `noa_gpui_platform::Screen`; holds `Option<LimitsHandle>`, `Option<StatsHandle>`, `History`, prefs, `Clock` (`Live` or `Fixed(now, FixedTz)` for demo), current sources and report. Keys: `1`/`2` or `T` tabs, `R` refresh, `P` period, `A` agent.
- `ui/charts.rs`: `Gauge { used, tick: Option<f64>, color, dimmed }`, `Heatmap { cells: [[u64;24];7], labels }`, `ShareBar { parts: Vec<(String, u64, Color)> }`, each a `Widget` with fixed `style()` size and `paint`.
- Agent colours: claude `#d97757`, codex `#10a37f`, agy `#4285f4`; severity colours from `Role` (normal = accent, caution yellow, warning orange, critical red).
- `demo.rs`: three sources (one stale, one over pace, one with a scoped bucket and spend), a report with 30 days of data for all charts, history for 7 days; built from the contract types only.

### L2-9. Verification changes (D1)

In `scripts/verify.sh`: `usage=widgets/agent-usage/Cargo.toml`; `run usage widget_check "$usage"` in `--full`; `widgets/agent-usage/*` sets the flag; `widget_uses "$usage"` (it uses no Noa crate by path, so only its own changes trigger it). Plus, for this crate only, the library tests run on Linux too: `cargo test --manifest-path "$usage" --locked --lib` when `$os` is not Darwin (the lib has no font-dependent test). The comment block and `AGENTS.md` name the new crate.

## L3: Acceptance criteria (checks)

| AC | Check |
|---|---|
| AC1 | `commands::tests::agent_usage_open_round_trips` (action name, menu id, palette title "Open Agent Usage", category View); `helper_program::tests::agent_usage_is_found_by_its_own_binary_name` (Helpers `Noa Agent Usage`, dev `agent-usage`); `scripts/bundle-macos.sh` on macOS leaves `Noa.app/Contents/Helpers/Noa Agent Usage`; by hand: palette entry opens the window; renaming the binary away shows the toast |
| AC2 | model tests (pace, exhaustion, staleness, labels); A's normalisation and failure tests; C's `limits_view_*` tests on demo data; by hand against real CLIs |
| AC3 | B's parser and aggregation tests on fixtures; C's `stats_view_*` tests |
| AC4 | B's `history::*` tests; C's chart tests (series and cells from a known report) |
| AC5 | `tests/snapshot.rs` renders both tabs with `--demo` flags to PNGs of the expected size; `agent-usage --snapshot=x.png` without `--demo` exits non-zero |
| AC6 | the named tests below exist and pass; `scripts/verify.sh --full` passes on Linux and macOS (CI) |
| AC7 | the FEATURES entry exists and names every flag and file above |
| AC8 | unconfirmed until a person looks at both tabs on a display (light and dark) |

## Implementation units

Order: **D0 → (A ∥ B ∥ C ∥ D1)**. D0 is the integrating unit's skeleton, merged to `main` before the others branch. After D0 every file has exactly one owner unit (L2-1); A, B, C and D1 touch disjoint files and can run in parallel worktrees, merged in any order. Each merge leaves `main` building and passing, because D0's stubs keep every interface callable (live mode with only C merged shows empty cards and "集計中 0 / 0").

### Unit D0: crate skeleton and contracts (first)

- Files: `widgets/agent-usage/{Cargo.toml,Cargo.lock,.gitignore}`, `src/lib.rs`, `src/model.rs`, `src/time.rs`, `src/paths.rs`, `src/stats/types.rs`; stubs `src/limits/mod.rs`, `src/stats/mod.rs`, `src/history.rs`, `src/main.rs` (prints the usage line, exits 0).
- Delivers: L1-1.1, L2-1 to L2-5 contracts; pace and label math.
- Done when `cargo build --offline --locked --all-targets` and `cargo test --offline --locked --lib` pass in the crate, the lock check passes (`awk` over `name`/`version` pairs: every pair of `widgets/agent-usage/Cargo.lock` is in the root or another widget lock), and these tests exist and pass: `model::tests::{elapsed_fraction_and_pace_delta, earlier_exhaustion_wins_across_window_lengths, rolled_over_window_is_stale, over_pace_escalates_to_critical, reset_labels_follow_the_notation, binding_window_skips_stale_sources}`, `time::tests::{rfc3339_with_and_without_fraction_and_offsets, local_dates_cross_midnight_in_fixed_tz, weekday_is_monday_zero}`, `paths::tests::roots_under_home_follow_env_overrides`.

### Unit A: limits sources

- Files: `src/limits/mod.rs`, `src/limits/{cli,curl,claude,codex,agy}.rs`, `src/limits/testdata/*` (synthetic responses).
- Delivers: L1-2.5, 6 (source side), the Data sources limits rules, L2-7.
- Test: `cargo test --offline --locked --lib limits` with injected `HttpFn`, fake credential readers and fake `codex`/`agy` shell scripts in a temp dir (as llm-usage's tests do). Done when these pass: `curl::tests::{token_is_only_in_the_config_text, args_hold_no_secret, response_splits_headers_body_and_status, config_escapes_quotes}`, `claude::tests::{limits_array_normalises_to_5h_7d_buckets_and_spend, flat_fallback_when_no_limits, expired_token_is_stale_not_error_after_a_good_sample, unauthorized_marks_token_expired, rate_limit_backs_off_and_honours_retry_after, manual_refresh_inside_180s_is_deferred, plan_comes_from_subscription_type, credentials_file_is_never_written}`, `codex::tests::{app_server_handshake_then_rate_limits, push_update_triggers_a_read, extra_limit_ids_become_labelled_windows, server_exit_keeps_last_value_stale, log_fallback_reads_the_newest_token_count}`, `agy::tests::{old_version_is_refused_before_usage_runs, remaining_fraction_is_inverted, invalid_envelope_is_unexpected, timeout_is_reported}`, `cli::tests::{timeout_kills_the_child, login_shell_path_is_parsed_from_marker}`, `mod::tests::{one_source_failing_leaves_the_others, dead_source_thread_reports_internal_error}`.

### Unit B: log statistics, index, history

- Files: `src/stats/mod.rs`, `src/stats/{claude,codex,agy,journal,index,aggregate,prices}.rs`, `src/stats/testdata/**`, `src/history.rs`.
- Delivers: L1-3.4–5, L1-4.6, L2-6, the statistics rows of Data sources.
- Test: `cargo test --offline --locked --lib stats history`. Done when these pass: `claude::tests::{usage_counts_once_per_message_id, tool_uses_count_once_per_id, turns_skip_tool_results, synthetic_model_is_skipped, subagent_file_joins_its_session, malformed_and_oversized_lines_are_counted_and_skipped}`, `codex::tests::{fresh_input_excludes_cached, usage_records_preferred_over_token_count, token_count_used_when_no_records_and_repeats_skipped, model_follows_turn_context, tool_call_kinds_counted, sub_thread_is_flagged}`, `agy::tests::{slash_commands_are_not_turns, sessions_from_conversation_ids_and_db_files, prompt_text_is_not_kept}`, `journal::tests::only_agent_spawns_count_as_workers`, `index::tests::{grown_file_resumes_at_offset, cut_last_line_is_read_next_pass, shrunk_or_rewritten_file_is_reparsed, index_round_trips_and_bad_version_rebuilds, nothing_is_written_outside_own_dir}`, `aggregate::tests::{periods_are_local_calendar_days, per_model_and_project_totals_match_fixture, active_time_caps_gaps_at_five_minutes, heatmap_cells_by_local_weekday_and_hour, cost_is_priced_by_ttl_and_unpriced_counted, agy_tokens_are_unavailable_not_zero, long_all_period_uses_weeks}`, `prices::tests::dated_model_ids_match`, `history::tests::{fresh_samples_are_throttled_to_five_minutes_or_one_point, stale_and_log_values_are_not_recorded, old_samples_are_pruned_by_rename, series_break_at_gaps, concurrent_appends_do_not_interleave}`; plus `stats::tests::large_tree_is_indexed_off_the_calling_thread` (a generated 200-file tree; `start` returns before parsing ends).

### Unit C: UI, charts, demo, headless snapshot

- Files: `src/main.rs`, `src/demo.rs`, `src/ui/{mod,limits,stats,charts,style,prefs}.rs`, `tests/snapshot.rs`.
- Delivers: L1-1.4, L1-2.1–4 and 7 (UI side), L1-3.1–3, L1-4.1–5, L1-5, L2-8.
- Test: `cargo test --offline --locked` in the crate (UI tests run on macOS in `verify.sh`; on Linux they must build). Done when these pass: `ui::tests::{limits_view_shows_every_window_with_reset_and_percent, pace_tick_and_over_pace_line_appear_when_ahead, stale_card_is_dimmed_with_age_and_reason, failed_source_does_not_hide_others, summary_names_the_binding_window, stats_view_shows_dash_for_unavailable_values, period_and_agent_keys_cycle_and_persist, refresh_key_calls_every_source}`, `charts::tests::{gauge_tick_position_matches_elapsed, heatmap_scales_to_max_cell, share_bar_folds_small_models}`, `main::tests::{title_follows_the_locale, snapshot_without_demo_is_refused, flags_override_prefs}`, `tests/snapshot.rs::{demo_limits_snapshot_renders, demo_stats_snapshot_renders}` (PNG exists, 2× the logical size); by hand: `agent-usage --demo --tab=stats --snapshot=/tmp/s.png` looks complete.

### Unit D1: Noa integration, packaging, verification, docs

- Files: `crates/noa-app/src/{helper_program.rs,command_palette.rs,macos_menu.rs}`, `crates/noa-app/src/commands/{command.rs,tests.rs}`, `crates/noa-app/src/app/commands.rs`, `crates/noa-app/src/app/helpers/dispatch.rs`, `crates/noa-app/src/split_tree/tree/commands.rs`, `scripts/bundle-macos.sh`, `scripts/verify.sh`, `AGENTS.md`, `docs/FEATURES.md`, `CHANGELOG.md`.
- Delivers: L1-1.2–3, L1-6 verify part, L1-7, L2-9.
- Test: `scripts/verify.sh --full` passes (it now builds and checks `widgets/agent-usage`), and these exist and pass: `commands::tests::agent_usage_open_round_trips`, `helper_program::tests::agent_usage_is_found_by_its_own_binary_name`, the palette's existing every-command test list extended with `OpenAgentUsage`. By hand on macOS: `scripts/bundle-macos.sh` → `Noa.app/Contents/Helpers/Noa Agent Usage` exists; View → Agent Usage opens it; with the helper removed, the toast "Agent Usage is not installed (build widgets/agent-usage)".

### Criterion → unit map

| AC | D0 | A | B | C | D1 |
|---|---|---|---|---|---|
| AC1 | crate | — | — | window, title | menu, palette, launch, bundle, toast |
| AC2 | model, pace math | sources, refresh, staleness | — | cards, gauges, summary | — |
| AC3 | report types | — | parsers, index, aggregation, prices | tables, KPIs, controls | — |
| AC4 | sample type | — | history store, series | charts | — |
| AC5 | — | — | — | flags, demo, snapshot | — |
| AC6 | contract tests | tests | tests | tests | verify.sh |
| AC7 | — | — | — | — | FEATURES, CHANGELOG, AGENTS |
| AC8 | — | — | — | by hand | — |

## Scope

In: L1-1 … L1-7. Out: writing to any agent's files or credentials; refreshing tokens; reading agy's SQLite/protobuf conversation stores; OpenAI price table; a Linux package for widgets; live (non-demo) snapshots; notifications or a menu-bar/tray item; account e-mails in the UI.

## Decisions taken by default

- No `jiff`: it is in the root lock only, and times as `i64` with a `Tz` trait over `localtime_r` keep the new lock a subset of the competition lock. Hour buckets are UTC hours, so zones with half-hour offsets shift day and heatmap boundaries by up to 30 min.
- Plan names come from the credentials file (`subscriptionType`) rather than `claude auth status --json`, which avoids starting `claude` every cycle; the account is never shown.
- `claude --version` is spawned only when no Claude log records a `version`.
- Over-pace text shows from +1 %; red severity only above +10 % (llm-usage's threshold).
- Snapshot only with `--demo`, so a snapshot never touches credentials or starts CLIs.
- Cost is an API-price estimate and says so; subscription plans are not billed per token.
- History retention 400 days; index rebuild on any format change.

## Risks

- The Claude usage endpoint is unofficial and can change or disappear; the card then degrades to an error and nothing else breaks. Running llm-usage on the same machine doubles the request rate against the same token (300 s each); 429 back-off covers it.
- `codex app-server` initialises its own SQLite state under `~/.codex` when it starts: that write is codex's own, not this widget's, but the widget causes it. If that is unacceptable, Codex can run in log-fallback mode only (open question 3).
- Codex and Claude log formats are internal and change between CLI versions (e.g. Claude's `api-request` records and `output_tokens_details` are recent); unknown record types are skipped and counted, never fatal, and the fixture tests pin today's shapes.
- agy shows no token statistics until a reader for its conversation store exists.
- Parsing a very large `~/.claude/projects` the first time takes as long as reading it once; the UI stays live (background thread, partial results), and later starts read only appended bytes.
- Hand-drawn charts (gauge, heatmap, share bar) and the windows' look have no automated visual check (AC8).

## Needs the maintainer's agreement before implementation

Per `AGENTS.md` Safety: the new `widgets/agent-usage/Cargo.lock` (no new crate, versions from existing locks), the `scripts/verify.sh` change (new widget check and Linux `--lib` tests) and the `AGENTS.md` edit. AC6 asks for the verify change, which is taken as the request; the PR must still say so.

## Open questions

1. Is a `--lib` test run on Linux for this crate acceptable in `verify.sh` (the other widgets run tests on macOS only)? Default: yes, as L2-9.
2. agy statistics: accept turns/sessions/projects only, or plan a later unit that decodes `conversations/*.db` (needs a SQLite reader: a new dependency or a `sqlite3` binary, absent on this Linux machine)? Default: turns/sessions only.
3. Codex limits through `codex app-server` (official, but starts codex and lets it write its own state) or log fallback only? Default: app-server, log fallback when it fails.
4. Price table: Anthropic list prices as of 2026-11-03, Haiku 5.5 cache prices derived; no OpenAI prices. Default: as L2-6, Codex cost "—".
