# Adding or changing a widget

For a repository-specific view made from existing components, use a declarative
`.noa-widgets/<id>/widget.json` package instead; see
[Repository widgets](../REPOSITORY_WIDGETS.md) and the `noa-widget-creator`
skill. These packages use the already registered `widget-host` and need none
of the application registration steps below. This runbook covers native
helper crates, including changes to the generic host itself.

Target: a crate under `widgets/` (its own workspace, outside the root one) that Noa starts as a window of its own. A widget is registered in about fifteen places. `scripts/check-widget-registration.py` reads them and reports every one that disagrees with `dev/scaffold/registry.json`; `scripts/verify.sh` runs it every time. Spec: `docs/specs/scaffold-widget.md` (I1 the registry, I2 the output, I3 the rules).

The checker reads files only. It checks that the registrations agree; it does not check what a built artifact contains, and it does not check the tests (point 13) or the docs (point 17): those are on you.

## 1. The points

File:line are today's examples, taken from the Tasks widget at HEAD b91a776d. They move; the checker prints the current ones. "Checked" is the name the checker gives the point in its output.

| # | Point | Example | Checked |
|---|---|---|---|
| 1 | crate | `widgets/tasks/Cargo.toml:1-8` (`[package] name`, `[workspace]`) | `crate` |
| 2 | launch const | `crates/noa-app/src/helper_program.rs:67-71` (`TASKS`) | `launch` |
| 3 | `--repo=` argument | `crates/noa-app/src/helper_program.rs:83-95` (`launch_args`) | `launch` (`repo_arg` against the crate's `accepts_repo`) |
| 4 | dispatch to the helper | `crates/noa-app/src/app/commands.rs:251` | `command` (part `dispatch`) |
| 5 | variant | `crates/noa-app/src/commands/command.rs:122` | compiler (through 4) |
| 6 | menu id const, `menu_id`, `from_menu_id` | `crates/noa-app/src/commands/command.rs:311`, `:401`, `:480` | `command` (parts `menu-id`, `menu-id-lookup`) |
| 7 | action name, both ways | `crates/noa-app/src/commands/command.rs:613` (`fixed_action_name`), `:715` (`from_action_name`) | `command` (parts `action`, `action-lookup`) |
| 8 | palette title | `crates/noa-app/src/command_palette.rs:136` | `palette` (part `title`) |
| 9 | palette description, category | `crates/noa-app/src/command_palette.rs:288`, `:702` | compiler |
| 10 | palette entry list | `crates/noa-app/src/command_palette.rs:397` (`command_palette_entries`) | `palette` (part `entry`) |
| 11 | command scopes | `crates/noa-app/src/app/helpers/dispatch.rs:426`, `:495`; `crates/noa-app/src/split_tree/tree/commands.rs:73` | compiler |
| 12 | View menu item (macOS) | `crates/noa-app/src/macos_menu.rs:418` | `menu` |
| 13 | tests | `crates/noa-app/src/commands/tests.rs:1256`, `crates/noa-app/src/command_palette.rs:1073`, the tests of `helper_program.rs` | no |
| 14 | verify gate | `scripts/verify.sh:77`, `:116`, `:135`, `:186-191`, `:205` | `verify` |
| 15 | macOS bundle | `scripts/bundle-macos.sh:73`, `:92`, `:102`, `:123`, `:205-206` | `macos_bundle` |
| 16 | Linux package | `scripts/package-linux.sh:48-55` (`WIDGETS=(`) | `linux_package` |
| 17 | docs | `AGENTS.md` (Verification), `docs/FEATURES.md`, `docs/feature-map.md` | no |

"Compiler" points are exhaustive `match`es: rustc refuses a missing arm, so the checker does not read them. Add the `AppCommand` variant (point 5) and let the compiler list the rest.

There is no Linux menu: on Linux a widget opens from the command palette. `menu` is a macOS-menu point that every widget needs, on every platform it ships on.

## 2. Steps

1. Create `widgets/<name>/` with its own `Cargo.toml` (`[package] name` equal to the directory name, `[workspace]`), its own `Cargo.lock` and `src/main.rs`. If the widget takes `--repo=DIR`, `main.rs` must contain the text `"--repo="`; then point 3 must list it too, and the other way round (`repo-arg-mismatch`).
2. Add the `HelperProgram` const in `helper_program.rs` (`crate_name` is the directory name, `bundled` is `Noa <Title>`), and its `launch_args` clause if it takes `--repo=`.
3. Add the `AppCommand` variant and follow the compiler: the dispatch in `app/commands.rs`, the menu id const with `menu_id` / `from_menu_id`, the action name `<name>.open` with `fixed_action_name` / `from_action_name`, the palette title, description, category and entry, the command scopes. Add the View menu item in `macos_menu.rs`.
4. Add the tests of point 13: the dev-build-dirs and the helper lookup tests in `helper_program.rs`, `commands/tests.rs` (addressable by action and menu id), the palette tests.
5. `scripts/verify.sh`: the manifest variable, the `in_part widgets && run` line in `--full` (or `in_part core` for a widget assigned to the core CI job), the changed-path `case`, the `widget_uses` block and the changed-run line. Use `usage_check` if the widget's tests do not depend on macOS font widths, else `widget_check`.
6. `scripts/bundle-macos.sh`: the name in the `for widget in` list, the `<NAME>_BIN` variable, the `-x` check, the `cp` into `Contents/Helpers/<bundled>`, the `codesign` line.
7. `scripts/package-linux.sh`: a `"<name>:<bundled>"` line in `WIDGETS=(`, **if** the widget ships on Linux.
8. Register the decision in `dev/scaffold/registry.json` (below).
9. Docs: `docs/FEATURES.md`, `docs/feature-map.md`, and the widget list in the Verification section of `AGENTS.md`.
10. Run the checker, then `scripts/verify.sh`.

## 3. The registry entry

`dev/scaffold/registry.json` holds only what a person decides; everything else is derived from the files. A widget's entry says where it ships:

```json
"<name>": { "ships": ["linux", "macos"] }
```

`ships` is a non-empty, sorted list of `"linux"` and `"macos"`. When a widget does not ship on both, `"reason"` is required in meaning: without it the checker reports `os-difference-unexplained` and exits 1. With it, the difference is listed as explained and does not fail.

```json
"<name>": { "ships": ["macos"], "reason": "why the other platform is left out" }
```

Keep the canonical format (UTF-8 without escaping, 2-space indent, widgets sorted by name, a trailing newline): `python3 -c 'import json,sys; sys.stdout.write(json.dumps(json.load(open("dev/scaffold/registry.json")), indent=2, ensure_ascii=False) + "\n")' | diff - dev/scaffold/registry.json` prints nothing. The `assets` list is for the Scaffold widget (checkers, runbooks, rules and the decisions that move them through their lifecycle); the checker does not read it.

## 4. Running the checker

```sh
python3 -I -B scripts/check-widget-registration.py            # text
python3 -I -B scripts/check-widget-registration.py --json     # the full report, every point of every widget
python3 -I -B scripts/check-widget-registration.py --repo=DIR # another checkout
```

| Exit | Meaning |
|---|---|
| 0 | `result: consistent` |
| 1 | `result: inconsistent`: "unexplained differences" lists each as `<widget> <target>: <rule> (<path>:<line>, …)` |
| 2 | `result: unknown`: a source it reads is missing, too large, not UTF-8, or has lost an anchor (a function it looks for was renamed or reflowed) |

Exit 2 never means "fine". When a source changed shape on purpose, update the rules in the spec (I3), `scripts/check-widget-registration.py`, the Rust port in `widgets/scaffold/src/scan/` and the fixtures under `scripts/tests/fixtures/widget-registration/` together: the fixtures' `expected.json` is the one oracle both implementations are tested against.

## 5. `scripts/verify.sh`

`scripts/verify.sh` runs `registration` (the checker) on every invocation, including both macOS CI parts, and `registration-test` (the unittest below) in `--full` (the core part when split) and whenever the checker, `scripts/tests/` or a fixture changed:

```sh
python3 -I -B -m unittest discover -s scripts/tests -p 'test_check_widget_registration.py'
```

A failing check is named in the output; its log is `.verify/<check>.log`.
