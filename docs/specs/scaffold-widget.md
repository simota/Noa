# Scaffold widget (Noa · 足場) and the widget-registration checker: Spec

| | |
|---|---|
| slug | scaffold-widget |
| status | implemented; setup workflow extended 2026-11-27 |
| owner | simota |
| as-of | 2026-11-27; original checker anchors below are from HEAD b91a776d |
| crate | `widgets/scaffold` (binary `scaffold`, bundled as `Noa Scaffold`, window title 「Noa · 足場」 / "Noa · Scaffold") |
| checker | `scripts/check-widget-registration.py` (python3 stdlib only) |
| request | Implemented Scaffold MVP (AC1–AC11) and general-project setup workflow; the I3 checker and I4 parity fixtures retain their contracts |

The widget and checker are implemented. [The user guide](../SCAFFOLD.md) describes the current workflow; `widgets/scaffold/src/` is the source of truth for implementation details. The Decisions and Interfaces preserve the checker and persistence contracts. Original implementation units and file:line anchors are retained as historical context, not as pending work or a record of completed manual verification.

## Original motivation and checker acceptance

- Original problem: adding a widget required changes in about fifteen places, without a check that they agreed. At b91a776d, `scripts/verify.sh:64-70` said "only widgets/agent-usage ships on Linux" while `scripts/package-linux.sh:48-55` shipped six widgets, with no recorded reason for leaving out `verify` and `tasks`. The implemented registration checker and registry now make those decisions explicit.
- Job to be done: (1) a checker that reads the repo and reports every registration that disagrees with the human-decided intent, run by `scripts/verify.sh`; (2) a window that lists the repo's development assets (checkers, runbooks, rules), the improvement candidates the registration scan finds, and their evidence. From a candidate the window writes a work brief and creates a local task for an agent. It never runs anything from the repo.
- Success definition: on the integration branch, `python3 -I -B scripts/check-widget-registration.py` prints `result: consistent` and exits 0 with `scaffold` registered everywhere (the self-check); deleting `scaffold`'s line from `scripts/bundle-macos.sh` makes it exit 1 naming `scaffold macos`; View → Scaffold opens the window on the focused pane's repo, which lists the two declared assets and no candidate; on a fixture with a gap it lists the candidate with file:line, and タスクにする creates a `.noa/tasks/t-….md` that the Tasks widget shows.

## Registration points (original anchors at b91a776d)

Each widget is registered in these places. The checker checks the ones marked ✓. The ones marked "compiler" are exhaustive `match`es: rustc refuses a missing arm, so the checker does not read them.

| # | Point | File:line (Tasks as the example) | Checked |
|---|---|---|---|
| 1 | crate | `widgets/tasks/Cargo.toml:1-8` (`[package] name`, `[workspace]`) | ✓ `crate` |
| 2 | launch const | `crates/noa-app/src/helper_program.rs:67-71` (`TASKS`) | ✓ `launch` |
| 3 | `--repo=` argument | `crates/noa-app/src/helper_program.rs:83-95` (`launch_args`) | ✓ `launch.repo_arg` vs `crate.accepts_repo` |
| 4 | dispatch to the helper | `crates/noa-app/src/app/commands.rs:251` | ✓ `command` (part `dispatch`) |
| 5 | variant | `crates/noa-app/src/commands/command.rs:122` | compiler (via 4) |
| 6 | menu id const + `menu_id` + `from_menu_id` | `crates/noa-app/src/commands/command.rs:311`, `:401`, `:480` | ✓ `command` (parts `menu-id`, `menu-id-lookup`) |
| 7 | action name both ways | `crates/noa-app/src/commands/command.rs:613` (`fixed_action_name`; `action_name` at `:509` only delegates, and `FloatAction::action_name` at `:184` comes first in the file), `:715` | ✓ `command` (parts `action`, `action-lookup`) |
| 8 | palette title | `crates/noa-app/src/command_palette.rs:136` | ✓ `palette` (part `title`) |
| 9 | palette description, category | `crates/noa-app/src/command_palette.rs:288`, `:702` | compiler |
| 10 | palette entry list | `crates/noa-app/src/command_palette.rs:397` (`command_palette_entries`) | ✓ `palette` (part `entry`) |
| 11 | command scopes | `crates/noa-app/src/app/helpers/dispatch.rs:426`, `:495`; `crates/noa-app/src/split_tree/tree/commands.rs:73` | compiler |
| 12 | View menu item (macOS) | `crates/noa-app/src/macos_menu.rs:418` | ✓ `menu` |
| 13 | tests | `crates/noa-app/src/commands/tests.rs:1256`, `crates/noa-app/src/command_palette.rs:1073`, `helper_program.rs` tests | no (runbook only) |
| 14 | verify gate | `scripts/verify.sh:77`, `:116`, `:135`, `:186-191`, `:205` | ✓ `verify` |
| 15 | macOS bundle | `scripts/bundle-macos.sh:73`, `:92`, `:102`, `:123`, `:205-206` | ✓ `macos_bundle` |
| 16 | Linux package | `scripts/package-linux.sh:48-55` (`WIDGETS=(`) | ✓ `linux_package` |
| 17 | docs | `AGENTS.md` (Verification), `docs/FEATURES.md:115-118`, `docs/feature-map.md` | no (runbook only) |

There is no Linux menu: on Linux the widgets open from the command palette (`docs/FEATURES.md:118`). `menu` is therefore a macOS-menu point that every widget needs; it is not a platform difference.

## Decisions

1. **The widget gets its findings from a Rust port of the checker's rules, and both implementations are tested against one set of fixture repos.** Running the checker would execute a file from the repo being scanned (`scripts/check-widget-registration.py` is repo code), which AC5 forbids. Reading a JSON file the checker wrote earlier gives stale or missing data, and nothing exists for an unknown repo. So the rules are written twice. To stop the two copies drifting: (a) this spec's Interface I3 is the one written definition of the rules; (b) the fixture repos and their expected `--json` output under `scripts/tests/fixtures/widget-registration/` are the shared oracle: the Python unittest compares the checker's output to `expected.json` byte for byte, and the Rust test `widgets/scaffold/tests/registration_parity.rs` compares the port's report to the same `expected.json` as JSON values; (c) every rule id of I3 appears in at least one `expected.json` (asserted by the Python test), so no rule goes untested on either side; (d) `scripts/verify.sh` runs the scaffold widget's tests whenever a fixture changes.
2. **Nothing runs a process, in either implementation.** The checker reads files only. The widget reads `.git/HEAD`, refs and `packed-refs` itself instead of calling `git`: `git` would honour the repo's own `.git/config` (`core.fsmonitor`, aliases, hooks), which is repo-controlled code. A source-level test in each implementation asserts this (no `subprocess`/`os.system`/`exec`/`eval`/`importlib`; no `std::process` in `widgets/scaffold/src`).
3. **The registry holds only human decisions** (`dev/scaffold/registry.json`, I1): the platforms each widget is meant to ship on plus the reason for any platform it leaves out; and the assets with the decision log that moves each through its lifecycle. Everything else is derived: which scripts ship a widget, names, `--repo`, an asset's current lifecycle (its last decision's `to`), verification state, scores and candidates. There is no `lifecycle` field.
4. **Registry writes require explicit asset-registration confirmation or 決定を記録.** The canonical format of I1 is unchanged. First registration publishes a complete file without replacing an existing path; updates refuse with 「registry.json が外部で変更されました。再読込してください」 if the file's bytes changed since it was read. Scanning and opening never write under the repo. Other project writes are local task files and their supporting ignore/coordination files under `.noa/` (Decision 8 and the setup workflow below).
5. **Crate dependencies are exactly `widgets/agent-usage`'s** (`noa-gpui-* 0.2.1`, `serde`, `serde_json`, `libc`, `dirs 6`, macOS `objc2-foundation`, dev `tempfile 3`). The lock is a copy of `widgets/agent-usage/Cargo.lock` (L2-1). It needs no `noa-ipc`: the widget drives no panes. Hand-off goes through a task file, which the Tasks widget picks up within 2 s (`docs/specs/task-widget.md` L1-2). It needs no `noa-config` either: `dirs::data_dir()` is what `noa_config::session_state_path` uses (`crates/noa-config/src/lib.rs:2247`), so `<data dir>/noa/scaffold` sits beside the other widgets' folders.
6. **Linux shipping.** `scaffold` ships on macOS only, with its reason in the registry (AC8; Q2), while its build and tests also run on Linux. `verify` and `tasks` have explicit macOS-only entries recording the still-open Linux shipping decision (Q1). The setup workflow does not change packaging or those platform decisions.
7. **Verify results are read conservatively** (L2-4). The widget never reduces a Verify log the way `widgets/verify` does. A session counts as finished only once its log holds a `session_closed` line. Its decision counts only if the last `judge_accepted` with a verdict comes after the last `round_started`. Anything it cannot place reads as 未実施 or 不明, never as 確認済み.
8. **Hand-off writes a task file and links by reference.** Registration candidates retain the I6 brief and its `scaffold-asset` / `scaffold-improvement` headers. Setup briefs use a `scaffold-setup` header and recover an existing task from that file independently of observations. Both use `<repo>/.noa/tasks/t-<millis>[-n].md` and the Tasks front-matter format. Review and Verify sessions are linked by id and folder path, recorded in the widget's own data dir; their content is never copied.
9. **Lifecycle belongs to assets; improvements have a derived status only** (未着手 / 引き渡し済み / 解消). The six-stage lifecycle and the verification state are two separate fields, computed by two separate pure functions (L2-3).
10. **Original implementation split: three units, with the widget crate kept together.** The suggested split (widget core / widget UI + launch path) would give the UI unit no compilable crate until the core merges. Splitting `src/lib.rs` from `src/ui/` cannot be done without the two units sharing `Cargo.toml` and `lib.rs`. The crate stays in one deep unit, and the `noa-app` launch path and packaging form their own unit, which builds in parallel.

## Implemented setup workflow (2026-11-27)

### Inventory and scope

`src/setup.rs` adds an independent, read-only inventory. `Setup` contains
`stacks`, six `checks`, detected `assets`, observed `sources`, and
`registration_applicable`. Each check has a stable id, title, description,
`Found` / `Missing` / `Unknown` state, file:line evidence, and a next step.

| Check id | Area |
|---|---|
| `getting-started` | README and setup documentation |
| `working-agreements` | Human/agent rules and editor conventions |
| `dependencies` | Rust, Node.js, Python, and Go dependency definitions |
| `runtime` | Toolchain and language-version settings |
| `verification` | Explicit scripts, targets, or test configuration |
| `continuous-integration` | Recognized CI/workflow files |

The inventory recognizes known paths and a bounded GitHub workflow directory.
`Found` means a readable file or recognized entry point exists, not that tools
are installed, the configuration is correct, or verification passed. Missing
optional filenames do not fill the Sources tab with errors. A custom project
layout can require a documented alternative. JSON is parsed; TOML/YAML
recognition does not promise full syntax validation.

Descriptor-relative reads with `O_NOFOLLOW` reject intermediate and final
symlinks inside the selected project. Reads are bounded to 1 MiB per file and
8 MiB per inventory, with 64 workflows and 256 directory entries as discovery
limits. Oversized, empty, unreadable, invalid UTF-8, and malformed JSON sources
are `Unknown`. Opening the project performs no project write or execution.

The Noa registration profile applies when the registry declares widgets or
the complete Noa helper path itself is observed. An unreadable or linked
ancestor such as `crates/` alone does not identify a Noa repository. Other
projects get a setup-source report, without diagnostics demanding Noa
internals. `scan::scan`, I2/I3, the Python checker, and the I4 parity fixtures
are unchanged. The registration score retains its existing meaning; a general
project's header instead counts detected setup areas.

### Four tabs and explicit previews

`src/ui/mod.rs` opens on Setup (`0`), followed by Assets (`1`), Registration
candidates (`2`), and Sources (`3`). Search uses case-insensitive AND matching
of whitespace-separated terms against each tab's relevant names, paths,
descriptions, and status text. The shared All / Needs attention filter keeps
missing/unknown setup checks, active assets without verification or a skip
(and conflicting histories), open registration candidates, and unknown
sources. Selection is keyed by item identity across rescans.

`src/ui/setup.rs` holds the exact asset list or setup task key/text being
reviewed. Opening a preview does not write. Confirmation is an explicit button
and rechecks the relevant state; a changed preview requires a fresh review.
Search or automatic refresh does not silently replace the reviewed content.
`Enter` and list navigation do not confirm a setup preview.

Confirmed registration appends detected, unregistered assets to schema 1 with
empty decision histories. Their lifecycle is Candidate and their verification
state starts unrun. Existing ids or paths are not duplicated. First creation
uses a complete temporary file plus a no-clobber publish; existing registries
retain the byte-conflict check. Registration can work without the optional
personal data folder. It does not modify detected project files.

### Durable setup task hand-off

`Setup::task_text` produces a Tasks-compatible brief with evidence, missing or
unknown preparation, acceptance conditions, and `$NOA_RESULT` reporting.
`Setup::task_key` includes the setup plan and relevant unreadable evidence,
without timestamps, absolute paths, or unrelated registry/helper diagnostics.
Registering assets alone does not change the key.

`src/handoff/setup.rs` treats the task file as the durable hand-off record.
It finds `t-*.md` files with a matching `scaffold-setup` header, a title, and an
open or closed state. Header reads are bounded to 8 KiB; the task directory
scan is bounded to 4096 entries. It reuses an existing task across restarts,
even without an observation log or data folder. A missing task can be created
again; a mere stale observation cannot suppress recreation.

Lookup and creation run under a short, nonblocking `flock` on
`.noa/tasks/.scaffold-setup.lock`. The lock inode remains in place; closing the
file releases the lock. A concurrent creator gets a rescan message instead
of blocking the UI or creating another task. Tasks keep the existing
`t-<millis>[-n].md` naming and never overwrite an existing file. Missing
`.noa/tasks/` directories and `.noa/.gitignore` are created only as part of a
hand-off, and an existing ignore file is preserved.

The optional observation is a reference. If recording it fails after the task
was saved, the action reports the saved task and the failure; later scans
recover the reference from the task header. Creating a task does not launch
an agent or open Tasks automatically.

### Entry points and regression coverage

The CLI accepts `DIR`, `--repo DIR`, and `--repo=DIR`; it validates the selected
folder before opening a window. `--demo` uses fixed sample data and excludes a
project directory. `--snapshot FILE` requires the demo; `--size WxH` requires
a snapshot. Both separated values and `--name=value` are accepted.

Inline setup, registry, hand-off, backend, and UI tests cover the generic
project path, no implicit project writes, symlinks and bounded sources,
registration conflicts, preview changes, task recovery/concurrent creation,
search/filter selection, and retained form state. The existing registration
parity, no-execution, and snapshot tests remain in place. Source-level and
headless checks do not replace manual macOS application verification.

## Interfaces (implemented checker and persistence contracts)

### I1. `dev/scaffold/registry.json`

```json
{
  "schema": 1,
  "widgets": {
    "agent-dashboard": {
      "ships": ["linux", "macos"]
    },
    "scaffold": {
      "ships": ["macos"],
      "reason": "…"
    }
  },
  "assets": [
    {
      "id": "check-widget-registration",
      "kind": "checker",
      "title": "ウィジェット登録の整合チェック",
      "path": "scripts/check-widget-registration.py",
      "decisions": [
        {
          "to": "trial",
          "at": "2026-11-26",
          "reason": "…",
          "evidence": []
        }
      ]
    }
  ]
}
```

- `schema`: `1`.
- `widgets`: object keyed by the widget's directory name under `widgets/`. `ships`: a non-empty list of distinct values from `"linux"`, `"macos"`, sorted. `reason`: string, required in meaning (not by the parser) whenever `ships` is not `["linux", "macos"]`. A missing or empty `reason` on such an entry is the finding `os-difference-unexplained` (I3), not a parse error.
- `assets`: list. `id` matches `^[a-z0-9][a-z0-9-]*$` and is unique. `kind` is one of `checker`, `runbook`, `rule`. `title` is any string. `path` is relative to the repo, with no `..` and no leading `/`. `decisions` lists entries in order: `to` is one of `candidate`, `trial`, `evaluating`, `adopted`, `reviewing`, `retired`; `at` is `YYYY-MM-DD`; `reason` is a non-empty string; `evidence` is a list of `{"kind": "verify"|"review"|"task"|"path"|"commit", "ref": "<id, repo-relative path or sha>"}`. Only the widget validates `assets` beyond "is a list". The checker reads `widgets` only.
- Canonical format, which both U1's committed file and the widget's writer follow: UTF-8, non-ASCII not escaped, 2-space indent, `": "` and `,` separators, one key per line, empty lists as `[]`, a trailing newline. Key order: top `schema, widgets, assets`; widget entry `ships, reason` (no `reason` key when absent); asset `id, kind, title, path, decisions`; decision `to, at, reason, evidence`; evidence `kind, ref`. Keys of `widgets` are sorted. This is what `serde_json::to_string_pretty` produces for structs in that field order with a `BTreeMap` for `widgets`, plus `"\n"`. It is also what Python's `json.dumps(obj, indent=2, ensure_ascii=False) + "\n"` produces for dicts built in that order.
- Content U1 commits:
  - widgets: `agent-dashboard`, `agent-usage`, `competition`, `discussion`, `repo-browser` and `review` ship `["linux", "macos"]`.
  - `scaffold`: `["macos"]` with the reason "Linux では未確認（AC11 の手動確認は macOS のみ）。Linux 配布は確認後に判断する。"
  - `tasks` and `verify`: `["macos"]` with the reason "Linux 配布は未判断（追加時に scripts/package-linux.sh に入らず、理由の記録がない）。docs/specs/scaffold-widget.md Q1 で判断する。"
  - assets: `check-widget-registration` (checker, the path above) and `add-widget-runbook` (runbook, `docs/runbooks/add-widget.md`), each with one decision `to: trial`, `at: 2026-11-26`, a one-line reason, and `evidence: []`.

### I2. Checker CLI and output

`python3 -I -B scripts/check-widget-registration.py [--repo=DIR] [--json]`

- `--repo` defaults to the parent of the script's directory. Paths in the output are relative to it and use `/`. No absolute path, time or host name appears in the output.
- Exit: `2` if any source is `unknown`; else `1` if `findings` is non-empty; else `0`. `status` is `unknown` / `inconsistent` / `consistent` to match.
- The scope sentence, verbatim: `Checked: registration consistency only. Not checked: the contents of built artifacts.`

`--json` prints `json.dumps(report, indent=2, ensure_ascii=False, sort_keys=True) + "\n"`:

```json
{
  "exit": 1,
  "explained": [
    {"evidence": [{"line": 12, "path": "dev/scaffold/registry.json"}],
     "reason": "…", "rule": "os-difference", "target": "linux", "widget": "beta"}
  ],
  "findings": [
    {"evidence": [{"line": 2, "path": "widgets/gamma/Cargo.toml"},
                  {"line": 40, "path": "crates/noa-app/src/helper_program.rs"}],
     "parts": [], "rule": "missing-registration", "target": "launch", "widget": "gamma"}
  ],
  "schema": 1,
  "scope": "Checked: registration consistency only. Not checked: the contents of built artifacts.",
  "sources": [
    {"error": null, "path": "crates/noa-app/src/app/commands.rs", "state": "ok"}
  ],
  "status": "inconsistent",
  "widgets": [
    {"intent": {"evidence": [{"line": 4, "path": "dev/scaffold/registry.json"}],
                "reason": null, "ships": ["linux", "macos"]},
     "points": {
       "command":       {"action": "alpha.open", "evidence": [...], "missing": [], "state": "present", "variant": "OpenAlpha"},
       "crate":         {"accepts_repo": true, "binary": "alpha", "evidence": [...], "missing": [], "state": "present"},
       "launch":        {"bundled": "Noa Alpha", "const": "ALPHA", "evidence": [...], "missing": [], "repo_arg": true, "state": "present"},
       "linux_package": {"evidence": [...], "missing": [], "name": "Noa Alpha", "state": "present"},
       "macos_bundle":  {"evidence": [...], "missing": [], "name": "Noa Alpha", "state": "present"},
       "menu":          {"evidence": [...], "missing": [], "state": "present"},
       "palette":       {"evidence": [...], "missing": [], "state": "present"},
       "verify":        {"check": "usage_check", "evidence": [...], "missing": [], "state": "present"}
     },
     "widget": "alpha"}
  ]
}
```

- `state` of a point is one of `present`, `partial` (the presence part was found but `missing` names other parts), `absent`, `unknown` (a source it needs is unknown, or it was not traced: see I3). Each point always carries exactly its fact keys, `null` when not found: `crate` {`binary`, `accepts_repo`}, `launch` {`const`, `bundled`, `repo_arg`}, `command` {`variant`, `action`}, `verify` {`check`}, `macos_bundle` {`name`}, `linux_package` {`name`}; `menu` and `palette` have none. All eight points are present for every widget. `intent` is `null` for a widget with no registry entry.
- `findings` and `explained` items have exactly the keys shown (`parts` only on findings; `[]` unless the rule is `incomplete-registration`).
- Ordering: `sources` by `path`; `widgets` by `widget`; `findings` and `explained` by `(widget, rule, target)`; each `evidence` list by `(path, line)` with duplicates removed; `parts` and `missing` in I3's part order. Byte order (`str` comparison, Rust `Ord` on `String`).

Text output (default), line for line:

```text
Checked: registration consistency only. Not checked: the contents of built artifacts.
sources: <n ok> ok, <n unknown> unknown
  unknown <path>: <error>                      (one line per unknown source, in source order)
widgets: <count> (<comma-separated names>)
explained differences: <count>
  <widget> <target>: <rule> — <reason> (<path>:<line>[, <path>:<line>…])
unexplained differences: <count>
  <widget> <target>: <rule>[ (<parts joined by ", ">)] (<path>:<line>[, …])
result: consistent|inconsistent|unknown
```

Section headers are printed with a count of 0 too. No colour, no trailing spaces.

### I3. Derivation rules (the one definition both implementations follow)

**Reading a source.** `lstat` the path relative to the repo. If it does not exist: `missing`. If it is not a regular file (a symlink, a directory): `not a regular file`. If it is larger than 1 MiB (1048576 bytes): `too large`. If it is not valid UTF-8: `not UTF-8`. Otherwise the text is split into lines on `\n` (a `\r` before it is dropped), numbered from 1. Any of these errors makes the source `unknown` with that exact error string. A source whose first missing anchor (in the order listed below) is not found is `unknown` with `anchor not found: <anchor>`. A source's **anchor line** is the line of its first anchor; for `widgets/<w>/src/main.rs`, which has no anchor, it is 1.

**Prototype check.** A throwaway Python run of these rules over HEAD b91a776d gave no finding for any of the 8 widgets, and showed `tasks` and `verify` as not shipped on Linux. That is why the action part reads `fixed_action_name`, not `action_name`.

**Function body.** The function body of `fn NAME` is the lines from the first line containing `fn NAME(` up to and including the first later line that equals that line's leading whitespace followed by `}`.

**Widgets.** The direct children of `widgets/` that are directories (not symlinks), whose names do not start with `.`, and that contain a `Cargo.toml`, sorted. If `widgets/` is missing, the source `widgets` is `unknown` (`missing`) and there are no widgets.

**Sources and anchors:**

| Source | Anchors, in order |
|---|---|
| `dev/scaffold/registry.json` | must parse as JSON → else `invalid JSON`; then I1 checks → `invalid registry: schema is not 1` / `invalid registry: widgets is not an object` / `invalid registry: ships of <w> is not a non-empty list of linux/macos` / `invalid registry: reason of <w> is not a string` / `invalid registry: assets is not a list` (first failing, widgets in sorted order). Its anchor line is the line of `"widgets"` |
| `widgets/<w>/Cargo.toml` | `[package] name`: a line `[package]` followed (before the next line starting with `[`) by a line matching `^name = "([^"]+)"$` |
| `widgets/<w>/src/main.rs` | none |
| `crates/noa-app/src/helper_program.rs` | `fn launch_args`; `HelperProgram const` (at least one match of the const regex below) |
| `crates/noa-app/src/app/commands.rs` | `fn open_helper_program` |
| `crates/noa-app/src/commands/command.rs` | `fn menu_id`; `fn from_menu_id`; `fn fixed_action_name`; `fn from_action_name` |
| `crates/noa-app/src/macos_menu.rs` | `MenuItem::with_id` |
| `crates/noa-app/src/command_palette.rs` | `fn command_palette_title`; `fn command_palette_entries` |
| `scripts/verify.sh` | `widget_check()` |
| `scripts/bundle-macos.sh` | `for widget in` (a line matching `^for widget in (.+); do$`) |
| `scripts/package-linux.sh` | `WIDGETS=(`: a line `WIDGETS=(` followed by a line `)` |

**Points of widget `w`.** `V` is the variant, `C` the const, `N` the bundled name. A part named in quotes is a part name. A point is `unknown` if a source it reads is unknown.

- `crate` (`widgets/w/Cargo.toml`, `widgets/w/src/main.rs`): `present` whenever the source is ok. `binary`: the first `name = "…"` after a `[[bin]]` line (before the next `[` line), else the package name. `accepts_repo`: `main.rs` contains the 9 characters `"--repo="`. Evidence: the package `name` line, and the first `"--repo="` line when true.
- `launch` (`helper_program.rs`): the consts are the matches of `pub\(crate\) const ([A-Z][A-Z0-9_]*): HelperProgram = HelperProgram \{(.*?)\};` (dot matches newline), with fields `(\w+): "([^"]*)"` inside. `C` is the first const whose `crate_name` equals `w`. Found → `present`, with `bundled` = N and evidence = the line holding `crate_name: "w"`; not found → `absent`. `repo_arg`: the body of `fn launch_args` contains `helper.crate_name == C.crate_name`.
- `command` (traced only if `launch` is present; else `unknown` with no finding): part `"dispatch"` is a match in `app/commands.rs` of `AppCommand::([A-Za-z0-9]+)\s*=>\s*\{?\s*self\.open_helper_program\(\s*crate::helper_program::C\s*\)`, which gives `V`; its evidence line holds `crate::helper_program::C`. Without dispatch the point is `absent`. Then, in `commands/command.rs`:
  - `"action"`: a body line of `fn fixed_action_name` matching `Self::V => "([a-z0-9-]+\.open)"`, which gives `action`.
  - `"action-lookup"`: a body line of `fn from_action_name` containing both `"<action>"` and `=> Some(Self::V)`.
  - `"menu-id"`: a body line of `fn menu_id` matching `AppCommand::V => Self::([A-Z0-9_]+)`, which gives `M`.
  - `"menu-id-lookup"`: a body line of `fn from_menu_id` containing `Self::M => Some(Self::V)`.

  Missing parts make the point `partial`. Evidence: the dispatch line and the action line.
- `menu` (traced only if `command` has `V`): `macos_menu.rs` contains `AppCommand::V.menu_id()` → `present` (evidence: that line), else `absent`.
- `palette` (traced only if `command` has `V`): part `"entry"` is a body line of `fn command_palette_entries` matching `^\s*AppCommand::V,\s*$`; part `"title"` is a body line of `fn command_palette_title` matching `AppCommand::V => "`. No entry → `absent`; entry but no title → `partial`. Evidence: the entry line and the title line.
- `verify` (`scripts/verify.sh`): part `"manifest"` is a line `^(\w+)=widgets/w/Cargo\.toml$`, which gives the variable `X`. The presence part `"full-run"` is a line `^\s*(?:in_part (?:core|widgets) && )?run (\S+) (widget_check|usage_check) "\$X"\s*$`, which gives `check`. No manifest or no full-run → `absent`. Other parts:
  - `"changed-path"`: a line `^\s*case \$path in ([^)]*)\)` whose `|`-separated, trimmed alternatives include `widgets/w/*`.
  - `"changed-run"`: a line `^\s*\[ "\$\w+" = 1 \] && run \S+ (widget_check|usage_check) "\$X"\s*$`.
  - `"widget-uses"`: a line containing `widget_uses "$X"`.

  Evidence: the manifest and full-run lines.
- `macos_bundle` (`scripts/bundle-macos.sh`): part `"binary"` is a line `^(\w+)="\$TARGET_DIR/w"$`, which gives `B`. The presence part `"copy"` is a line `^cp "\$B" "\$CONTENTS/Helpers/([^"]+)"$`, which gives `name`. No binary or no copy → `absent`. Other parts:
  - `"build"`: the `for widget in` list (split on spaces) contains `w`.
  - `"check"`: a line containing `[ -x "$B" ]`.
  - `"sign"`: a line containing `--sign "$NOA_SIGN_IDENTITY" "$CONTENTS/Helpers/<name>"`.

  Evidence: the copy line.
- `linux_package` (`scripts/package-linux.sh`): between `WIDGETS=(` and `)`, a line `^\s*"([a-z0-9-]+):([^"]+)"\s*$` with group 1 = `w` → `present`, with `name` = group 2 and evidence = that line; else `absent`.

Part order, for `missing` and `parts`: `dispatch, action, action-lookup, menu-id, menu-id-lookup`; `entry, title`; `manifest, full-run, changed-path, changed-run, widget-uses`; `binary, copy, build, check, sign`.

**Shipped on** a platform: `macos` if `macos_bundle` is `present` or `partial`; `linux` if `linux_package` is `present`.

**Rules.** Rules are evaluated only on points that are not `unknown`. `E(x)` is the evidence of x. `A(s)` is the anchor line of source s.

| Rule id | Target | Fires when | Evidence |
|---|---|---|---|
| `missing-registration` | `launch`, `command`, `menu`, `palette`, `verify` | that point is `absent` | `E(crate)` package line + `A(source of the point)` |
| `incomplete-registration` | `command`, `palette`, `verify`, `macos_bundle` | that point is `partial` (`parts` = its `missing`) | `E(point)` + `A(source)` |
| `name-mismatch` | `crate` | `binary` ≠ `w` | `E(crate)` |
| `name-mismatch` | `macos_bundle`, `linux_package` | the point's `name` ≠ `launch.bundled` (both found) | `E(launch)` + `E(point)` |
| `repo-arg-mismatch` | `launch` | `launch` present and `crate.accepts_repo` ≠ `launch.repo_arg` | `E(crate)` + `A(helper_program.rs)` |
| `no-intent` | `registry` | the widget has no registry entry | `E(crate)` + `A(registry)` |
| `stale-intent` | `registry` | a registry entry names no widget (its `widget` is the entry's key) | the entry's line: the first line matching `^\s*"<key>"\s*:` after the `"widgets"` line |
| `ship-missing` | `linux`, `macos` | intent ships the platform; the widget is not shipped on it | entry line + `A(script)` |
| `ship-unintended` | `linux`, `macos` | intent does not ship the platform; the widget is shipped on it | entry line + `E(point)` |
| `os-difference-unexplained` | each platform not in `ships` | `ships` ≠ `["linux","macos"]` and `reason` absent or empty after trimming | entry line |
| `os-difference` (goes to `explained`, not `findings`) | each platform not in `ships` | as above, with a reason | entry line; `reason` = the trimmed reason |

Every rule except `stale-intent` applies only to discovered widgets: a registry entry with no crate gets `stale-intent` and nothing else. `A(script)` is the anchor line of `bundle-macos.sh` for `macos` and of `package-linux.sh` for `linux`. A widget whose `launch` is absent gets one `missing-registration launch` and nothing for `command`, `menu` or `palette` (their state is `unknown`, but this does not make any source unknown and does not change the exit code).

**Candidate id** (widget only): `imp-` followed by 16 lowercase hex digits of FNV-1a 64 over the UTF-8 of `<repo identity>\n<rule>\n<widget>/<target>`. The repo identity is the canonical path of the git common dir (L2-5), or the canonical repo path when no `.git` is found.

### I4. Fixture repos (`scripts/tests/fixtures/widget-registration/<case>/`)

Each case is `repo/` (a trimmed tree holding only the sources of I3, written so that every anchor holds unless the case breaks it on purpose), plus `expected.json` (exact `--json` output), `expected.txt` (exact text output) and nothing else. The exit code is `expected.json`'s `exit`. Fixture widgets are `alpha`, `beta`, `gamma` and so on. Fixture text contains no `TODO`/`FIXME` (the Tasks widget would list them as code markers).

| Case | Content | Exit | Covers |
|---|---|---|---|
| `clean` | `alpha` (accepts `--repo`, in `launch_args`) and `beta` (`usage_check`), both shipped on both platforms, the registry agreeing | 0 | AC2a |
| `missing-launch` | `clean` + `gamma` with no `HelperProgram` const | 1 | AC2b launch |
| `missing-verify` | `clean` + `gamma` with no `verify.sh` lines | 1 | AC2b verify |
| `missing-packaging` | `clean` + `gamma`, intent both platforms, in neither packaging script | 1 | AC2b packaging (`ship-missing` ×2) |
| `os-difference-explained` | `beta` intent `["macos"]` with a reason, not in `package-linux.sh` | 0 | AC2c accepted |
| `os-difference-unexplained` | same, with no `reason` | 1 | AC2c reported |
| `unparseable-script` | `package-linux.sh` without the `WIDGETS=(` block | 2 | AC2d |
| `unparseable-registry` | `registry.json` that is not JSON | 2 | AC2d |
| `mismatches` | `name-mismatch` (bundle name), `repo-arg-mismatch`, `incomplete-registration` (no `sign`; no `widget-uses`), `ship-unintended`, `no-intent`, `stale-intent` | 1 | every remaining rule id |

### I5. The widget's own files (never in the repo)

`<data dir>/noa/scaffold/<repo-key>/`, where `<data dir>` is `dirs::data_dir()` and `<repo-key>` is `<dir name>-<FNV-1a 64 hex of the canonical repo path>` (as `widgets/tasks/src/store.rs:33`):

- `scan.json`: the last scan, `{"schema":1,"at":<ms>,"commit":"<sha>"|null,"report":<I2 report>}`, replaced by temp file + rename.
- `improvements.json`: `{"schema":1,"improvements":[{"id","rule","widget","target","evidence":[…],"first_seen":<ms>,"last_seen":<ms>,"resolved":<ms>|null}]}`, sorted by id. Merging a scan is by id: a seen id updates `evidence` and `last_seen` and clears `resolved`; a stored id the scan no longer finds gets `resolved = now` if it was null. It is never removed and never duplicated.
- `observations.jsonl`, append-only, one event per line, a torn last line skipped:

```json
{"event":"linked","subject":{"asset":"check-widget-registration"},"kind":"verify","ref":"ver-1732600000000","path":"/…/noa/verifications/ver-1732600000000","at":1732600000000}
{"event":"linked","subject":{"improvement":"imp-0123456789abcdef"},"kind":"task","ref":"t-1732600000000","path":"/repo/.noa/tasks/t-1732600000000.md","at":1732600000000}
{"event":"unlinked","subject":{"asset":"check-widget-registration"},"kind":"review","ref":"rev-1732600000000","at":1732600000000}
{"event":"skipped","subject":{"asset":"add-widget-runbook"},"reason":"…","commit":"<sha>"|null,"at":1732600000000}
```

  `kind` is one of `verify`, `review`, `task`. A `verify` / `review` ref must be one path component matching `^(ver|rev)-[A-Za-z0-9._-]+$`, and `<data dir>/noa/verifications/<ref>/events.jsonl` or `<data dir>/noa/reviews/<ref>/events.jsonl` must exist, or the link is refused.

### I6. Hand-off task file

`<repo>/.noa/tasks/t-<millis>[-n].md`, written as `skills/noa-tasks/SKILL.md` prescribes: create `.noa/tasks` and a `.noa/.gitignore` holding `*` if missing; refuse if `.noa` or `.noa/tasks` is a symlink; write a hidden temp file in the folder, `std::fs::hard_link` it to the first free name, remove the temp file.

```text
---
title: [足場] <widget> <target>: <rule の日本語ラベル>
state: open
labels: scaffold
scaffold-asset: check-widget-registration
scaffold-improvement: imp-0123456789abcdef
---
<brief>
```

The brief, in Markdown, has these sections in order:
- 改善候補: id, rule, target.
- 根拠: one `- path:line` per evidence item.
- 関連: linked Verify/Review ids with their paths, if any.
- 完了の条件: `python3 -I -B scripts/check-widget-registration.py` no longer reports `<widget> <target>: <rule>`; follow `docs/runbooks/add-widget.md`.
- 報告: write to `$NOA_RESULT`, as the Tasks run brief asks.

### I7. Noa identifiers

`AppCommand::OpenScaffold`, `OPEN_SCAFFOLD_MENU_ID = "noa.view.open-scaffold"`, action `scaffold.open`, palette title "Open Scaffold", palette description "Show the repository's development assets (checkers, runbooks, rules) and the registration gaps found in it, and hand a gap to an agent as a task, in a window of its own.", View menu item "Scaffold" after "Tasks", `helper_program::SCAFFOLD = HelperProgram { title: "Scaffold", bundled: "Noa Scaffold", crate_name: "scaffold" }`.

## Original MVP criteria and implementation traceability

| AC | Approach | Files | Unit |
|---|---|---|---|
| AC1 | Implement I2 and I3 in one stdlib-only script (`argparse` not required; `json`, `os`, `re`, `stat`, `sys` suffice). Read files only. Exit codes and ordering as in I2 | `scripts/check-widget-registration.py`, `dev/scaffold/registry.json` | U1 |
| AC2 | `scripts/tests/test_check_widget_registration.py`, run by `python3 -I -B -m unittest discover -s scripts/tests -p 'test_check_widget_registration.py'`. For each I4 case it runs the checker as a subprocess (`sys.executable -I -B <checker> --repo=<case>/repo [--json]`) twice per format and asserts: equal bytes across the two runs (e); equal to `expected.json` / `expected.txt`; the exit code; the scope sentence as line 1 of the text and in `scope` (f); every I3 rule id present in some `expected.json`; the checker source contains none of `subprocess`, `os.system`, `os.popen`, `os.exec`, `exec(`, `eval(`, `compile(`, `runpy`, `importlib`, `__import__`. Cases a–d as in I4 | `scripts/tests/test_check_widget_registration.py`, `scripts/tests/fixtures/widget-registration/**` | U1 |
| AC3 | `verify.sh` runs `registration` (the checker) in both modes, right after `boundaries`; `registration-test` (AC2's unittest) in `--full` and when `scripts/check-widget-registration.py` or `scripts/tests/*` changed. The comment at `scripts/verify.sh:64-70` drops "only widgets/agent-usage ships on Linux" and says that which widget ships where is `dev/scaffold/registry.json`'s to say, checked by the checker. Green on the integration branch, with U1's registry entry for `scaffold` and U2/U3's registrations | `scripts/verify.sh` | U3 (file), U1+U2+U3 (green) |
| AC4 | `widgets/scaffold`: own `[workspace]` and `Cargo.lock`, deps of Decision 5, `[lib] name = "scaffold"`, `[[bin]] name = "scaffold"`, `AppOptions::new("Noa Scaffold")`, title 「Noa · 足場」 (ja, macOS) / "Noa · Scaffold" (otherwise, and always off macOS, as `widgets/tasks/src/main.rs:59-70`). CLI `scaffold [DIR \| --repo DIR]` or `scaffold --demo [--snapshot FILE.png [--size WxH]]`; `--snapshot` needs `--demo` and `--demo` excludes `--repo`, as `widgets/tasks/src/main.rs:124-133`; `--repo` defaults to the current directory, made absolute. Lock produced offline (L2-1) | `widgets/scaffold/{Cargo.toml,Cargo.lock,.gitignore,check-lock.sh}`, `src/main.rs` | U2 |
| AC5 | `scan::scan(repo) -> Report`, the I3 port, reads files only. The UI shows the registry assets (evidence: the `"id"` line in `registry.json`), the candidates (one per finding, I3 id, its evidence), and the sources with 不明 and the error for unknown ones. A repo with no `registry.json` still gets the setup inventory and explicit registration/task previews; registration candidates and the registration score remain unavailable until its registry exists. Score = 「整合 n/m」 (m widgets, n with no finding naming them), shown only when there is a registry and no unknown source. Tests: rescanning merges without duplicates; scanning a repo seeded with traps (a `.git/config` with `core.fsmonitor` and `.git/hooks/*` that would create a marker file, an executable `scripts/verify.sh` that would too) creates no marker; no `std::process` in `src` | `src/scan/*.rs`, `src/store.rs`, `src/gitread.rs`, `src/ui/*`, `tests/registration_parity.rs`, `tests/no_exec.rs` | U2 |
| AC6 | L2-3 types and pure functions. Tests: adopting with no evidence ref is refused; a linked Verify that is only registered, or still running, gives 未実施, never 確認済み; a finished `pass` on another commit gives 現版未確認; on the current commit, 確認済み. Decisions are written only into `registry.json`; scans, observations and links only into I5 (test: a scan + link + skip leaves the repo tree byte-identical) | `src/model.rs`, `src/verify_log.rs`, `src/registry.rs`, `src/store.rs` | U2 |
| AC7 | `handoff::brief`, `handoff::create_task` (I6), then a `linked … kind: task` observation. Tests: a golden file `src/testdata/handoff-task.md`; the written file passes a check of the skill's format rules (fences, `title`, `state`, `labels`, `key: value` lines); the asset id is in the header; linking a review or verification stores only ref + path (no other file appears under the data dir or the repo) | `src/handoff.rs`, `src/testdata/handoff-task.md` | U2 |
| AC8 | `SCAFFOLD` const, added to `launch_args`' `--repo` match and the module doc; tests extended: the dev-build-dirs list, `ac28_only_the_review_the_verify_and_the_tasks_…` gains `SCAFFOLD` in its first loop (name kept), a new `scaffold_is_found_by_its_own_binary_name`. I7 identifiers in every file of points 4–13, plus `commands/tests.rs` `scaffold_is_addressable_by_action_and_menu_id`. `bundle-macos.sh`: build loop, `SCAFFOLD_BIN`, `-x` check, copy to `Helpers/Noa Scaffold`, `codesign`, comment. Linux: not shipped (registry reason, U1) | `crates/noa-app/src/helper_program.rs`, `commands/command.rs`, `commands/tests.rs`, `command_palette.rs`, `app/commands.rs`, `app/helpers/dispatch.rs`, `split_tree/tree/commands.rs`, `macos_menu.rs`, `scripts/bundle-macos.sh` | U3 |
| AC9 | Runbook: the I3 points table with today's file:line examples, the compiler-checked points, the registry entry, "run the checker", and `scripts/verify.sh`. `docs/feature-map.md`: rows "Widget registration checker" and "Scaffold widget". `docs/FEATURES.md`: a **Scaffold** entry after Tasks. `AGENTS.md` Verification: add `widgets/scaffold` to the widget list (its tests run on Linux too); one line saying that `scripts/check-widget-registration.py` runs in every `scripts/verify.sh` and that a new or changed widget follows `docs/runbooks/add-widget.md` | `docs/runbooks/add-widget.md`, `docs/feature-map.md`, `docs/FEATURES.md`, `AGENTS.md` | U1 |
| AC10 | `verify.sh`: `scaffold=widgets/scaffold/Cargo.toml`; `in_part widgets && run scaffold usage_check "$scaffold"` in `--full`; `scf` set by `widgets/scaffold/* \| scripts/tests/fixtures/widget-registration/* \| dev/scaffold/*`; a `widget_uses "$scaffold"` block; `[ "$scf" = 1 ] && run scaffold usage_check "$scaffold"`. Judged against `.verify/baseline-<main sha>` | `scripts/verify.sh` | U3 |
| AC11 | Manual: `scripts/bundle-macos.sh`, View → Scaffold on this repo, then on a scratch copy with a gap: タスクにする, then View → Tasks shows the task with `scaffold-asset` | none | person |

## L2: Widget design (U2)

### L2-1. Crate and lock

```text
widgets/scaffold/
  Cargo.toml  Cargo.lock  .gitignore (/target, *.png)  check-lock.sh
  src/lib.rs           pub mod gitread, handoff, hash, model, registry, scan, setup, store, time, verify_log
  src/hash.rs          fnv1a64
  src/time.rs          now_ms, ms → YYYY-MM-DD (UTC civil date)
  src/gitread.rs       git_dir, common_dir, head_commit: from files only (L2-5)
  src/registry.rs      load, create (no clobber), save (canonical, conflict check), line_of
  src/scan/mod.rs      Report and friends (serde, I2 shape), scan(repo) -> Report
  src/scan/source.rs   reading (I3), anchors, function bodies
  src/scan/points.rs   the eight points
  src/scan/rules.rs    findings, explained, status, exit
  src/setup.rs         general-project inventory, task brief and stable key
  src/model.rs         L2-3
  src/verify_log.rs    L2-4
  src/store.rs         I5
  src/handoff.rs       I6
  src/handoff/setup.rs durable setup task lookup, recovery, nonblocking lock
  src/testdata/handoff-task.md
  src/main.rs  src/demo.rs  src/ui/mod.rs  src/ui/setup.rs  src/ui/tests.rs
  tests/registration_parity.rs  tests/no_exec.rs  tests/snapshot.rs
```

- `Cargo.toml`: `widgets/agent-usage/Cargo.toml` with `name = "scaffold"`, its own `description`, `[lib] name = "scaffold"`, `[[bin]] name = "scaffold"`. Same `[dependencies]`, macOS dependencies, `[dev-dependencies]` and `[profile.dev.package.*]`, nothing added.
- `Cargo.lock`: copy `widgets/agent-usage/Cargo.lock` and rename the root package entry `name = "agent-usage"` to `name = "scaffold"`. Then run `cargo tree --offline --manifest-path widgets/scaffold/Cargo.toml >/dev/null` once to move the entry to its sorted place, as `docs/specs/task-widget.md` L2-1 did. Then `cargo build --offline --locked`. Checked 2026-11-26 on a scratch copy with stub sources: `cargo tree --offline` and `cargo tree --offline --locked` both pass, and the lock's set of lines is unchanged apart from the rename. `check-lock.sh` is `widgets/agent-usage/check-lock.sh` with `agent-usage` replaced by `scaffold`: every `name version` pair must occur in the root lock or another widget lock. If `cargo` wants a crate that is not in the offline cache, stop and report it; do not fetch.
- UI tests assert labels, ids and order, never widths, so every test also runs on Linux (`usage_check`).

### L2-2. Report types (serde; the I2 JSON)

```rust
pub struct Evidence { pub path: String, pub line: u32 }
pub enum SourceState { Ok, Unknown }                     // "ok" | "unknown"
pub struct Source { pub path: String, pub state: SourceState, pub error: Option<String> }
pub enum PointState { Present, Partial, Absent, Unknown }
pub struct Point { pub state: PointState, pub missing: Vec<String>, pub evidence: Vec<Evidence>,
                   #[serde(flatten)] pub facts: BTreeMap<String, serde_json::Value> }
pub struct Intent { pub ships: Vec<String>, pub reason: Option<String>, pub evidence: Vec<Evidence> }
pub struct WidgetReport { pub widget: String, pub intent: Option<Intent>, pub points: BTreeMap<String, Point> }
pub struct Finding { pub rule: String, pub widget: String, pub target: String, pub parts: Vec<String>, pub evidence: Vec<Evidence> }
pub struct Explained { pub rule: String, pub widget: String, pub target: String, pub reason: String, pub evidence: Vec<Evidence> }
pub struct Report { pub schema: u32, pub scope: String, pub status: String, pub exit: u8,
                    pub sources: Vec<Source>, pub widgets: Vec<WidgetReport>,
                    pub findings: Vec<Finding>, pub explained: Vec<Explained> }
pub fn scan(repo: &Path) -> Report;
```

The tests in `tests/registration_parity.rs` are named `parity_*`, one per I4 case. Each loads its case's `expected.json` as a `serde_json::Value` and compares it with `serde_json::to_value(scan(case/repo))`. A further test, `parity_every_case_is_tested`, fails if the fixture directory is missing or holds a case that has no `parity_*` test.

### L2-3. Model (pure, unit-tested)

```rust
pub enum AssetKind { Checker, Runbook, Rule }
pub enum Lifecycle { Candidate, Trial, Evaluating, Adopted, Reviewing, Retired }
//                   候補       試作    評価中      採用     見直し     廃止
pub enum EvidenceKind { Verify, Review, Task, Path, Commit }
pub struct EvidenceRef { pub kind: EvidenceKind, pub r#ref: String }
pub struct Decision { pub to: Lifecycle, pub at: String, pub reason: String, pub evidence: Vec<EvidenceRef> }
pub struct Asset { pub id: String, pub kind: AssetKind, pub title: String, pub path: String, pub decisions: Vec<Decision> }
pub enum DecisionError { InvalidTransition { from: Lifecycle, to: Lifecycle }, NoEvidence, EmptyReason }
impl Asset {
    pub fn lifecycle(&self) -> Lifecycle;                          // last decision's `to`, else Candidate
    pub fn decide(&mut self, d: Decision) -> Result<(), DecisionError>;
}
pub fn check_history(asset: &Asset) -> Result<(), (usize, DecisionError)>;  // a hand-edited log: shown as 記録に矛盾

pub enum VerifyState { Verified, Failed, NotRun, Skipped, NotRunnable, Unknown, Stale }
//                     確認済み  失敗    未実施   省略     実行不能     不明     現版未確認
pub enum Subject { Asset(String), Improvement(String) }
pub enum SessionFact { Missing, Unreadable(String), Registered, Running,
                       Finished { decision: Option<String>, commit: Option<String> } }
pub enum Observation {
    Verify { subject: Subject, session: String, path: PathBuf, at: Millis },
    Review { subject: Subject, session: String, path: PathBuf, at: Millis },
    Task   { subject: Subject, id: String, path: PathBuf, at: Millis },
    Skipped { subject: Subject, reason: String, commit: Option<String>, at: Millis },
}
pub fn verification(obs: &[(Observation, Option<SessionFact>)], current: Option<&str>) -> VerifyState;

pub struct Improvement { pub id: String, pub rule: String, pub widget: String, pub target: String,
                         pub evidence: Vec<Evidence>, pub first_seen: Millis, pub last_seen: Millis,
                         pub resolved: Option<Millis> }
pub enum ImprovementStatus { Open, HandedOff(String), Resolved }   // 未着手 / 引き渡し済み(t-…) / 解消
```

Transitions:

| From | Allowed `to` |
|---|---|
| candidate | trial, retired |
| trial | evaluating, retired |
| evaluating | adopted, trial, retired |
| adopted | reviewing, retired |
| reviewing | adopted, evaluating, retired |
| retired | none |

Every decision needs a non-empty `reason`. `to: adopted` needs at least one evidence ref (`NoEvidence`).

`verification` decides from the subject's observations, first match:
1. Take the Verify observations with `Finished` facts and the `Skipped` observations, and pick the newest by `at`. If it is `Skipped` → Skipped. If it is `Finished`:
   - `commit` is None (uncommitted changes were verified) → Stale;
   - else `current` is None → Unknown;
   - else `commit` ≠ `current` → Stale;
   - else decision `pass` → Verified; `fail` or `pass_with_issues` → Failed (Q3); `inconclusive` or None (closed without a verdict) → NotRunnable.
2. Otherwise, any Verify fact that is `Missing` or `Unreadable` → Unknown.
3. Otherwise → NotRun. This covers no observation at all, and `Registered` or `Running` sessions: a registered or running Verify is never read as passed.

### L2-4. Reading a Verify session (`verify_log.rs`)

`fact(path) -> SessionFact` reads `<path>/events.jsonl` line by line as `serde_json::Value`, using only the `event` tag (`#[serde(tag = "event", rename_all = "snake_case")]`, `widgets/verify/src/model/types.rs:2218`). A missing file gives `Missing`. An unreadable file, or a first line that is not `created`, gives `Unreadable`. From `created`: `commit` = `rebuild.head_sha` when `rebuild.worktree_patch` is absent and `rebuild.untracked` is absent or empty (`widgets/verify/src/model/types.rs:636-651`), else None. `round_started` clears the pending decision. `judge_accepted` with `output.verdict.decision` sets it. `session_closed` marks the session closed. Result: closed → `Finished { decision: pending, commit }`; not closed with more than one line → `Running`; only `created` → `Registered`. A test reads a hand-written log for each outcome.

### L2-5. Git facts without git (`gitread.rs`)

- The git dir: `<repo>/.git` if it is a directory. If it is a file `gitdir: <p>`, then `<p>` (relative to `<repo>`).
- The common dir: the contents of `<gitdir>/commondir` (relative to the git dir), else the git dir.
- HEAD: `<gitdir>/HEAD` is either 40 hex characters, or `ref: <name>`. A ref is resolved through `<gitdir>/<name>`, then `<common>/<name>`, then a `<sha> <name>` line of `<common>/packed-refs`. At most 5 symbolic hops.

Any failure gives None (shown as 不明). Paths are lstat'ed and read with the I3 size cap; nothing is executed. Tests cover a plain repo, a linked worktree, packed refs and a detached HEAD, all built as files in a temp dir.

### L2-6. UI (Japanese labels)

- Header: repo path, short commit (or unknown), last scan, and either the Noa registration score or the count of detected setup areas.
- Four tabs (`0` / `1` / `2` / `3`): Setup with six areas, evidence, next steps, and explicit write previews; Assets with lifecycle and verification; Registration candidates with findings and hand-off status; Sources with paths and read errors.
- Search (`/`, `⌘F` / `Ctrl+F`) and All / Needs attention apply across tabs. Filters and selection survive rescans in the current window; no cross-launch view-preference persistence is promised.
- Detail actions retain Verify/Review links, unlinking, skips, and lifecycle decisions. Registration candidates retain their work-brief preview and `T` hand-off; setup writes use their own explicit confirmation buttons.
- Keys outside text inputs: `j`/`k` or arrows move, `Enter` opens details, `R` rescans, `L` links a Verify session, and `Esc` backs out of the active form/search/preview/detail. Text editing does not trigger single-letter commands.
- Scans run on open, on refresh, and about every 30 seconds. A refresh preserves active form input. Previews retain the content being reviewed and refuse changed confirmation targets.
- `--demo` provides a fixed setup inventory, report, registry, and observations; `--demo --snapshot` repeats on the same platform/font environment.

## Original implementation units (historical)

These units record the original delivery plan. Their branch instructions are historical; the setup workflow above describes the current implementation.

No two units edit the same file. Every cargo command runs with `--offline --locked`. No unit touches `.github/`, `package-linux.sh`, or any file not listed under its "Owns".

### U1 `checker` (tier: standard; deps: none)

- Owns:
  - `scripts/check-widget-registration.py`
  - `scripts/tests/test_check_widget_registration.py`
  - `scripts/tests/fixtures/widget-registration/**`
  - `dev/scaffold/registry.json`
  - `docs/runbooks/add-widget.md`
  - `docs/feature-map.md`
  - `docs/FEATURES.md`
  - `AGENTS.md`
- Builds: I1 (content as listed), I2, I3, I4; AC1, AC2, AC9.
- Done:
  - `python3 -I -B -m unittest discover -s scripts/tests -p 'test_check_widget_registration.py'` passes.
  - On U1's own branch, `python3 -I -B scripts/check-widget-registration.py` exits 1 with exactly one unexplained difference, `scaffold registry: stale-intent` (the crate does not exist yet), and exactly `tasks linux` and `verify linux` under explained differences.
  - `git diff --stat main` shows only the owned files.

### U2 `scaffold-crate` (tier: deep; deps: none to start; its parity and registry round-trip tests need U1 merged)

- Owns: `widgets/scaffold/**`.
- Builds: L2-1 … L2-6, I5, I6; AC4–AC7.
- Done, in `widgets/scaffold`:
  - `cargo build --offline --locked --all-targets` passes.
  - `sh check-lock.sh` prints `lock check ok`.
  - `cargo test --offline --locked -- --skip parity --skip registry_round_trip` passes before U1 lands. After merging U1's branch locally, `cargo test --offline --locked` passes (`registration_parity` and `registry::tests::registry_round_trip`: loading and saving the committed `dev/scaffold/registry.json` gives identical bytes).
  - `cargo run --offline --locked -- --demo --snapshot=$TMPDIR/s.png` writes a PNG.
  - `cargo clippy --offline --locked --all-targets -- -D warnings` is clean.
  - `git diff --stat main` shows only `widgets/scaffold/`.

### U3 `launch-path` (tier: standard; deps: none to start; `verify.sh` is green only once U1 and U2 are merged)

- Owns:
  - `crates/noa-app/src/helper_program.rs`
  - `crates/noa-app/src/commands/command.rs`
  - `crates/noa-app/src/commands/tests.rs`
  - `crates/noa-app/src/command_palette.rs`
  - `crates/noa-app/src/app/commands.rs`
  - `crates/noa-app/src/app/helpers/dispatch.rs`
  - `crates/noa-app/src/split_tree/tree/commands.rs`
  - `crates/noa-app/src/macos_menu.rs`
  - `scripts/bundle-macos.sh`
  - `scripts/verify.sh`
- Builds: I7; AC8, the `verify.sh` side of AC3 and AC10.
- Done:
  - `cargo test -p noa-app --offline --locked` passes (`helper_program::`, `commands::`, `command_palette::`).
  - `cargo clippy -p noa-app --all-targets --offline --locked -- -D warnings` is clean on Linux (on macOS, only no new warning compared with main, `scripts/verify.sh:104-106`).
  - `bash -n scripts/verify.sh scripts/bundle-macos.sh` passes.
  - `git diff --stat main` shows only the owned files.

### Integration (after U1, U2, U3 merge; no file edits)

- `python3 -I -B scripts/check-widget-registration.py` prints `result: consistent` and exits 0 (AC3, AC8 self-check).
- `CARGO_NET_OFFLINE=true scripts/verify.sh --full`, with the sandbox off for `noa-pty`/`noa-ipc`, has no failure beyond `.verify/baseline-<main sha>`, and runs `registration`, `registration-test` and `scaffold` (AC10).
- The negative check: delete `scaffold`'s `cp` line from `scripts/bundle-macos.sh` in a scratch copy; the checker exits 1 naming `scaffold macos: ship-missing`. Revert it.
- AC11 by hand on macOS.

## Original implementation approval scope

`AGENTS.md` Safety covers these: the new `widgets/scaffold/Cargo.lock` (no new crate, a copy of the agent-usage lock), the `scripts/verify.sh` change (new checks), and the `AGENTS.md` edit. The request's AC3, AC9 and AC10 ask for all three; the PR must still say so.

## Risks

- **Regex-reading Rust and shell is brittle.** A rustfmt reflow or a refactor of `command.rs` can break an anchor. That turns the source `unknown` (exit 2, `verify.sh` red); it never produces a wrong OK. The fix is to update I3, both implementations and the fixtures together. Their cost is the price of not executing code.
- **Two rule implementations.** The fixtures only exercise the branches they contain. A rule's corner case that no fixture hits can still differ. The "every rule id in some fixture" assertion reduces this but does not close it.
- **Verify's log format** is read from outside its crate (L2-4). If it changes, the widget falls back to 未実施 or 不明 (the safe direction), not to a wrong 確認済み.
- **Registry ids for Verify/Review sessions are machine-local.** A decision's evidence `ver-…` means nothing on another machine. It is a reference, not proof.
- **The repo identity is a path.** Moving the clone changes candidate ids, so old candidates show as 解消 and new ones appear once.
- **CI time:** one more widget build plus a test run on macOS and Linux (cold, roughly as long as `widgets/tasks`).

## Retained platform decisions and original defaults

- **Q1** Why are `verify` and `tasks` not shipped on Linux? Default: record them as macOS-only with the "未判断" reason of I1, so the checker is green and the gap is visible in the registry. The maintainer either writes the real reason or adds them to `scripts/package-linux.sh` (outside this scope).
- **Q2** Should `scaffold` ship on Linux? Default: no, with the reason in I1. It builds and its tests run on Linux.
- **Q3** Is a Verify `pass_with_issues` 確認済み or 失敗? Default: 失敗 (conservative: issues remain).
- **Q4** Should the widget write decisions into `registry.json`, or only display them? Default: write, on explicit action only, in canonical form, with conflict refusal (Decision 4).
- **Q5** Should the score count widgets or registration points? Default: widgets with no finding / all widgets.
- **Q6** After タスクにする, should the widget open the Tasks widget (`sibling.rs`, byte-identical in three crates)? Default: no in the MVP; the person opens View → Tasks.
