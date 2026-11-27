# Set up a project with Scaffold

Scaffold brings a project's development instructions, rules, and verification
entry points into one window. Start by seeing what is already present, review
the next step for anything missing, then register useful assets or hand a
concrete setup task to Tasks.

It works with an existing project directory before that project has a Noa
registry. It reads files; it does not install tools, run project commands, or
start an agent.

## Open a project

On macOS, choose **View → Scaffold**. You can also use **Open Scaffold** in
Noa's command palette. Noa passes the focused pane's directory, and Scaffold
finds the nearest enclosing Git worktree root. A directory without Git is
used directly.

The macOS app bundles the helper as `Noa Scaffold`. To choose a directory
explicitly:

```bash
"/Applications/Noa.app/Contents/Helpers/Noa Scaffold" --repo ~/src/my-project
```

From a Noa checkout:

```bash
cargo run --manifest-path widgets/scaffold/Cargo.toml --locked -- /path/to/project
cargo run --manifest-path widgets/scaffold/Cargo.toml --locked -- --demo
```

The standalone binary accepts `scaffold DIR`, `scaffold --repo DIR`, and
`scaffold --repo=DIR`. With no directory it uses the current directory. The
directory must exist; choose a folder rather than a file. `--help` shows the
options and examples.

Scaffold ships in the macOS app. It builds and has tests on Linux, but it is
not included in the Linux release package. Scaffold itself does not require
the Noa control server. Running an agent from Tasks uses Tasks' own setup;
see [Agent workflow](AGENT_WORKFLOW.md).

## 1. Read the setup overview

The initial **セットアップ** (Setup) tab shows six areas. Select a row to see
the file-and-line evidence and a suggested next step.

| Area | Examples of recognized entry points |
|---|---|
| 導入手順 — Getting started | `README.md`, `CONTRIBUTING.md`, `SETUP.md`, `docs/setup.md`, `docs/getting-started.md` |
| 作業ルール — Working agreements | `AGENTS.md`, `CLAUDE.md`, `CONTRIBUTING.md`, `.editorconfig` |
| 依存の定義 — Dependencies | `Cargo.toml`, `package.json`, `pyproject.toml`, `requirements.txt`, `go.mod` |
| 実行環境の指定 — Runtime settings | `rust-toolchain.toml`, `.nvmrc`, `.node-version`, `.python-version`, `.tool-versions`, `mise.toml`; recognized version fields in manifests |
| 検証の入口 — Verification entry points | `scripts/verify.sh`, package test/check/lint scripts, Make/Just/Task verification targets, pytest/tox/nox settings |
| CI の設定 — CI configuration | `.github/workflows/*.yml` or `*.yaml`, `.gitlab-ci.yml`, `.circleci/config.yml`, `azure-pipelines.yml`, `Jenkinsfile` |

These are known filenames and entry points, not a complete discovery of every
possible project structure. A custom script or a command documented elsewhere
may already meet your project's needs. Inspect the existing instructions
before adding another convention. A Rust or Go manifest alone, for example,
does not count as an explicit verification entry point; document the standard
command and its expected outcome when that is how the project works.
Documenting a custom or standard command does not by itself change this
detection result.

| Status | Meaning |
|---|---|
| 見つかりました — Found | Scaffold found a readable file or a recognized entry point. |
| これから整える — Missing | It did not find an entry point among the paths it checks. Review the suggested step against the project's conventions. |
| 確認できません — Unknown | It could not safely read or interpret a relevant source. Open **情報源** (Sources) for the reason. |

**Found does not mean installed, valid, or successfully tested.** The overview
does not inspect installed tools or execute commands. JSON is parsed; the
TOML and YAML checks are limited static recognition, not full configuration
validation. An unreadable, oversized, empty, or invalid UTF-8 file is unknown,
as is malformed JSON. Paths inside the project are read without following
symlinks, including intermediate directories.

The overview counts detected areas and areas needing attention. These counts
describe the inventory, not a project quality score.

## 2. Register the assets you found

When there are unregistered assets, choose **登録内容を確認** (Review
registration). The preview lists the titles, kinds, paths, and IDs of the
detected files, and shows the destination:
`dev/scaffold/registry.json`.

Choose **この内容で登録** (Register this content) to add the displayed set, or
close the preview to return without writing. This action registers the whole
previewed set; it is not a per-file selection editor.

Registered assets begin as **候補** (Candidate), with no adoption decision or
verification result. A path already in the registry is not added again. The
first registration creates the registry without replacing a file that another
window or editor has just created. An update refuses to overwrite a registry
whose bytes changed since it was read. Rescan and review again after a
conflict or a changed detection result.

The **資産** (Assets) tab then lets you record lifecycle decisions, reasons,
and evidence, or link existing Verify and Review sessions by ID. Adoption
requires evidence. Lifecycle and verification are separate: an adopted asset
is not automatically verified. A linked Verify session counts as verified
only after it finishes with a pass for the current commit.

Registration changes the registry, not the detected scripts and documents.
It also works when Noa's personal data folder is unavailable.

## 3. Turn missing preparation into a task

Choose **不足を整える作業指示を確認** (Review instructions for missing
preparation). The preview contains the current evidence, the next steps for
each missing or unknown area, acceptance conditions, and reporting
instructions. Choose **この内容でタスクを作成** (Create this task) to save it,
then open **View → Tasks** to continue.

The task is a local Markdown file at `.noa/tasks/t-<millis>.md` (with a suffix
if a name is already taken). Creating it does not start an agent. The task
asks the eventual worker to distinguish completed verification from anything
it could not run, and to report through `$NOA_RESULT` under Tasks' execution
instructions.

For the same setup plan, Scaffold reuses an existing task instead of creating
another. It recovers the task from its `scaffold-setup` header after reopening
the window, even if saving the optional observation failed or the personal
data folder is unavailable. Closed tasks are also reused; reopen the task in
Tasks if the same work should continue. If you delete the task, rescan to make
task creation available again. A changed setup plan can produce a new task;
registering assets by itself does not change that plan.

If another window is creating the task, Scaffold asks you to rescan rather
than waiting or creating a duplicate. A changed plan also requires a fresh
preview before creation.

## Navigate and focus on what needs attention

The four tabs are **セットアップ** (Setup), **資産** (Assets), **改善候補**
(Registration candidates), and **情報源** (Sources). Search is case-insensitive
and matches every space-separated term against the current tab's names,
paths, and relevant descriptions or status text. Search and the
**すべて / 要対応** (All / Needs attention) filter apply across tabs and survive
rescans within the window.

The attention filter shows missing or unknown setup areas; active assets
without a current verification or an explicit skip, plus contradictory
decision histories; registration candidates not yet handed off; and unreadable
sources. Empty filtered lists offer a way to clear the filters.

| Key | Action |
|---|---|
| `0` / `1` / `2` / `3` | Setup / Assets / Registration candidates / Sources |
| `/` or `⌘F` / `Ctrl+F` | Focus the search field |
| `j` / `k` or `↓` / `↑` | Move through the visible rows |
| `Enter` | Open the selected row's details |
| `R` | Rescan |
| `T` | Hand the selected registration candidate to Tasks |
| `L` | Link a Verify session to the selected asset or registration candidate |
| `Esc` | Close the form or preview, clear or leave focused search, or close details, depending on the active view |

Single-letter shortcuts do not fire while you are editing text. Setup
registration and setup-task creation use the preview's explicit confirmation
buttons; `Enter` is not a confirmation shortcut for those writes. Scans run
on opening, on refresh, and about every 30 seconds. Selection follows the
same item across a rescan; a disappearing selection does not silently open a
different item's details.

## Noa's registration checker

For Noa-style repositories, the Registration candidates tab also shows gaps
in widget registration, with their file-and-line evidence. This profile is
enabled by the Noa helper entry point itself or by declared widgets in the
registry. General projects are not asked to add Noa's internal source files.

The registration score **整合 n/m** means widgets with no registration finding
divided by all widgets. It appears only with a valid registration source and
no unknown scan source. It does not measure the contents of built artifacts.
The standalone checker and its Python/Rust parity rules remain documented in
[Add a widget](runbooks/add-widget.md) and the
[Scaffold specification](specs/scaffold-widget.md).

## What is saved

| Action | Destination |
|---|---|
| Open, search, filter, or rescan | No project files are written. Noa may save registration scans and candidate history in its own data folder. |
| Confirm asset registration or record a lifecycle decision | `dev/scaffold/registry.json` |
| Confirm a setup task or hand off a registration candidate | A new local task under `.noa/tasks/`; missing task directories and `.noa/.gitignore` are created as needed. Existing task files are not replaced. |
| Create or reuse a setup task | `.noa/tasks/.scaffold-setup.lock` coordinates simultaneous windows. The lock file remains; the lock is held only during the short lookup/create operation. |
| Link or unlink Verify/Review sessions, or record a skip | Noa's data folder under `noa/scaffold/<repo-key>/`; the linked session's contents are not copied. |

When created by Scaffold, `.noa/.gitignore` contains `*`. An existing ignore
file is preserved. The registry is intended to be a shared project file;
task files and local observations serve the local workflow.

## Try the demo

`--demo` uses sample data without reading or changing a project. It cannot be
combined with a project directory. For a deterministic headless screenshot:

```bash
cargo run --manifest-path widgets/scaffold/Cargo.toml --locked -- \
  --demo --snapshot scaffold.png --size 1280x860
```

`--snapshot` requires `--demo`; `--size` requires `--snapshot`. The PNG is
rendered at twice the logical dimensions. Equal demo snapshots are expected
on the same platform and font environment, not across different platforms.
