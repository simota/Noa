# Feature map

Where each feature's code and tests are. An index, not a description: read the
code for how it works. A path here that no longer exists is a bug in this file.

Most tests are inline `#[cfg(test)]` modules in the files listed under Code; the
Tests column names the dedicated test files where they exist. The files under
`crates/noa-grid/src/tests/` are `include!`d into one module, so their test
names carry no file name to filter on. Each command in
"Run only these" runs a non-empty set of tests. `scripts/verify.sh` picks the
related packages for a change on its own; these are for a tighter loop.

| Feature | Code | Tests | Run only these |
|---|---|---|---|
| VT parser and sequence dispatch | `crates/noa-vt/src/parser.rs`, `crates/noa-vt/src/stream.rs`, `crates/noa-vt/src/csi.rs`, `crates/noa-vt/src/sgr.rs` | `crates/noa-vt/src/tests.rs` | `cargo test -p noa-vt` |
| Terminal state, cursor, modes | `crates/noa-grid/src/terminal.rs`, `crates/noa-grid/src/screen.rs`, `crates/noa-grid/src/modes.rs` | `crates/noa-grid/src/tests/terminal_state.rs`, `crates/noa-grid/src/tests/vt_sequences.rs` | `cargo test -p noa-grid` |
| Scrollback and resize reflow | `crates/noa-grid/src/scrollback.rs`, `crates/noa-grid/src/screen/reflow.rs` | `crates/noa-grid/src/tests/text_resize.rs`, `crates/noa-grid/src/tests/scrollback_snapshot.rs` | `cargo test -p noa-grid resize`, `cargo test -p noa-grid reflow`, `cargo test -p noa-grid scrollback` |
| Selection, search, copy mode | `crates/noa-grid/src/selection.rs`, `crates/noa-grid/src/search.rs`, `crates/noa-grid/src/copy_mode.rs` | `crates/noa-grid/src/tests/selection_search.rs` | `cargo test -p noa-grid selection` |
| OSC: titles, cwd, hyperlinks, clipboard | `crates/noa-grid/src/osc.rs`, `crates/noa-grid/src/url.rs` | `crates/noa-grid/src/tests/osc.rs` | `cargo test -p noa-grid osc` |
| Kitty graphics and sixel | `crates/noa-vt/src/kitty_graphics.rs`, `crates/noa-vt/src/sixel.rs`, `crates/noa-grid/src/kitty.rs`, `crates/noa-grid/src/sixel.rs` | `crates/noa-grid/src/tests/kitty_graphics.rs` | `cargo test -p noa-grid kitty` |
| Keyboard, IME and paste encoding | `crates/noa-app/src/input.rs`, `crates/noa-app/src/input/` | `crates/noa-app/src/input/tests.rs` | `cargo test -p noa-app input::` |
| PTY spawn and shell integration | `crates/noa-pty/src/pty.rs`, `crates/noa-pty/src/shell_integration.rs`, `shell-integration/` | inline | `cargo test -p noa-pty` (needs openpty: sandbox off) |
| IO thread (pty → parser → grid) | `crates/noa-app/src/io_thread.rs`, `crates/noa-app/src/io_thread/` | inline | `cargo test -p noa-app io_thread` |
| GPU rendering | `crates/noa-render/src/renderer/`, `crates/noa-render/src/pipeline.rs`, `crates/noa-render/src/shaders/` | `crates/noa-render/tests/pipeline.rs` | `cargo test -p noa-render --test pipeline` (skips without a GPU) |
| Fonts, shaping, glyph atlas | `crates/noa-font/src/` | inline | `cargo test -p noa-font` |
| Configuration | `crates/noa-config/src/lib.rs`, `crates/noa-config/src/parser.rs` | inline | `cargo test -p noa-config` |
| Themes | `crates/noa-theme/src/`, `scripts/gen-themes` | inline | `cargo test -p noa-theme` |
| Splits | `crates/noa-app/src/split_tree.rs`, `crates/noa-app/src/app/split_ops.rs` | inline | `cargo test -p noa-app split_tree` |
| Tab Overview | `crates/noa-app/src/session_overview/`, `crates/noa-app/src/app/overview.rs` | inline | `cargo test -p noa-app session_overview` |
| Session sidebar | `crates/noa-app/src/sidebar.rs`, `crates/noa-app/src/app/sidebar.rs` | inline | `cargo test -p noa-app sidebar` |
| Command palette | `crates/noa-app/src/command_palette.rs`, `crates/noa-app/src/commands.rs` | inline | `cargo test -p noa-app command_palette` |
| Settings panel | `crates/noa-app/src/theme_settings.rs`, `crates/noa-app/src/theme_settings/` | `crates/noa-app/src/theme_settings/tests.rs` | `cargo test -p noa-app theme_settings` |
| Agent auto-approve | `crates/noa-app/src/auto_approve.rs`, `crates/noa-app/src/auto_approve/`, `crates/noa-app/src/app/auto_approve.rs` | inline | `cargo test -p noa-app auto_approve` |
| AppleScript | `crates/noa-app/src/macos_applescript.rs`, `crates/noa-app/src/app/applescript.rs`, `assets/Noa.sdef` | inline | `cargo test -p noa-app applescript` |
| noa-server (JSON-RPC / WebSocket) | `crates/noa-ipc/src/server.rs`, `crates/noa-ipc/src/protocol.rs`, `crates/noa-ipc/src/push.rs`, `crates/noa-ipc/src/client.rs`, `crates/noa-app/src/ipc_bridge.rs` | `crates/noa-ipc/tests/` | `cargo test -p noa-ipc` |
| Board Timeline RPC and publisher outbox | `crates/noa-ipc/src/timeline.rs`, `crates/noa-ipc/src/server.rs`, `crates/noa-ipc/src/client.rs`, `crates/noa-ipc/src/timeline_outbox.rs` | inline, `crates/noa-ipc/tests/server_tests.rs` | `cargo test -p noa-ipc` |
| Agent orchestration (`Noa +ctl`) | `crates/noa-app/src/ctl.rs`, `crates/noa-app/src/ctl/board.rs` (`+ctl board`), `crates/noa-app/src/ctl/` (`journal.rs`, `result.rs`, `ready.rs`, `prompt.rs`, `retry.rs`, `worktree.rs`), `crates/noa-app/src/app/ipc.rs` (`execute_ipc_spawn`), `crates/noa-ipc/src/client.rs` | inline, `crates/noa-ipc/tests/orchestration_tests.rs` | `cargo test -p noa-app ctl::` and `cargo test -p noa-ipc --test orchestration_tests` |
| Orchestration pane roles | `crates/noa-app/src/app/orchestration_chrome.rs`, `crates/noa-app/src/app/input_ops/layout.rs`, `crates/noa-app/src/app/render.rs` | inline | `cargo test -p noa-app orchestration_chrome` |
| Board workflow and timeline views | `crates/noa-app/src/ctl/board.rs`, `crates/noa-app/src/ctl/board/workflow.rs`, `crates/noa-app/src/ctl/board/timeline.rs` | inline | `cargo test -p noa-app ctl::board` |
| Session and scrollback restore | `crates/noa-app/src/session_persist.rs`, `crates/noa-app/src/scrollback_persist.rs`, `crates/noa-app/src/app/session_restore.rs` | inline | `cargo test -p noa-app persist` |
| Quick Terminal | `crates/noa-app/src/app/quick_terminal.rs` | inline | `cargo test -p noa-app quick_terminal` |
| Ghostty parity fixtures | `tests/parity/src/`, `tests/parity/fixtures/` | `tests/parity/tests/` | `cargo test -p noa-parity` |
| Agent status hook | `scripts/noa-agent-hook.py` | `scripts/test_noa_agent_hook.py` | `python3 -I -B scripts/test_noa_agent_hook.py` |
| Tasks workspace model, search and persistence | `widgets/tasks/src/workspace.rs` | inline (defaults, bulk/undo, relationships, dates, conflicts, damaged files, symlinks) | `cargo test --manifest-path widgets/tasks/Cargo.toml --locked workspace::` |
| Tasks list, navigation and property editor | `widgets/tasks/src/ui/organizer.rs`, `widgets/tasks/src/ui/planning.rs`, `widgets/tasks/src/ui/mod.rs` | `widgets/tasks/src/ui/organizer.rs` inline (controls, changed fields, draft conflicts) | `cargo test --manifest-path widgets/tasks/Cargo.toml --locked ui::organizer::` |
| Tasks commands, selection, saved views and run guards | `widgets/tasks/src/ui/planning.rs`, `widgets/tasks/src/main.rs`, `widgets/tasks/src/live.rs` | `widgets/tasks/src/ui/planning_tests.rs`, `widgets/tasks/src/ui/tests.rs`, inline | `cargo test --manifest-path widgets/tasks/Cargo.toml --locked ui::planning_tests::` |
| Tasks GitHub listing and detail refresh | `widgets/tasks/src/github.rs` | `widgets/tasks/src/github/tests.rs` | `cargo test --manifest-path widgets/tasks/Cargo.toml --locked github::` |
| Task links (task → runs, versions, Review / Verify sessions, PRs) | `widgets/tasks/src/task_links.rs` (byte-identical in `widgets/review/src/`, `widgets/verify/src/`), `widgets/tasks/src/sibling.rs` (same), `widgets/tasks/src/related.rs` | inline | `cargo test --locked --offline links_` in `widgets/tasks`, `widgets/review` or `widgets/verify` |
| Widget registration checker | `scripts/check-widget-registration.py`, `dev/scaffold/registry.json`, `docs/runbooks/add-widget.md` | `scripts/tests/test_check_widget_registration.py`, `scripts/tests/fixtures/widget-registration/` | `python3 -I -B -m unittest discover -s scripts/tests -p 'test_check_widget_registration.py'` |
| Development workflow editor | `widgets/workflow/src/`, `crates/noa-app/src/helper_program.rs` (`WORKFLOW`), `crates/noa-app/src/commands/command.rs` (`OpenWorkflow`) | inline widget tests; helper lookup, action/menu round-trip and palette tests in `noa-app` | `cargo test --manifest-path widgets/workflow/Cargo.toml --locked`; `cargo test -p noa-app workflow` |
| Ember personal agent and launch integration | `widgets/ember/src/`, `crates/noa-app/src/helper_program.rs` (`EMBER`), `crates/noa-app/src/commands/command.rs` (`OpenEmber`), `crates/noa-app/src/ember/menu_bar.rs` | widget logic tests; helper lookup/owner environment, action/menu round-trip, Japanese palette discovery and macOS menu tests | `cargo test --manifest-path widgets/ember/Cargo.toml --locked`; `cargo test -p noa-app ember`; `cargo test -p noa-app widget_discovery_tests` |
| Repository widget definitions and worktree discovery | `crates/noa-widget/src/`, `.noa-widgets/test-summary/` | `crates/noa-widget/tests/` | `cargo test -p noa-widget` |
| Repository widget definitions and worktree discovery | `crates/noa-widget/src/`, `.noa-widgets/test-summary/`, `.noa-widgets/agent-request/` (text inputs) | `crates/noa-widget/tests/` (`text_input.rs` for text-input/button `inputs`) | `cargo test -p noa-widget` |
| Repository widget packaged images (`assets`, `image` component, digest) | `crates/noa-widget/src/assets.rs`, `crates/noa-widget/src/manifest.rs`, `crates/noa-ipc/src/widget.rs` (`HostMessage::Asset`), `crates/noa-app/src/widget/mod.rs`, `widgets/widget-host/src/images.rs`, `.noa-widgets/image-gallery/` | `crates/noa-widget/tests/assets.rs`, `widgets/widget-host/tests/images.rs`, inline | `cargo test -p noa-widget --test assets` and `cargo test --manifest-path widgets/widget-host/Cargo.toml --locked images` |
| Repository widget CLI, lifecycle and durable events | `crates/noa-app/src/ctl/widget.rs`, `crates/noa-app/src/widget/mod.rs`, `crates/noa-ipc/src/widget.rs` | inline, `crates/noa-ipc/tests/widget_tests.rs` | `cargo test -p noa-app widget::` and `cargo test -p noa-ipc --test widget_tests` |
| Repository widget native host and fixture previews | `widgets/widget-host/src/`, `skills/noa-widgets/`, `skills/noa-widget-creator/` | inline, `widgets/widget-host/src/screen/text_input_tests.rs` (typing, IME, submit keys, limits), `widgets/widget-host/tests/` | `cargo test --manifest-path widgets/widget-host/Cargo.toml --locked` |
| Scaffold widget | `widgets/scaffold/src/` (`scan/` is the Rust port of the checker's rules), `crates/noa-app/src/helper_program.rs` (`SCAFFOLD`) | `widgets/scaffold/tests/` (`registration_parity.rs` reads the checker's fixtures), inline | `cargo test --locked --offline` in `widgets/scaffold` |
| Scaffold widget | `widgets/scaffold/src/setup.rs` (project inventory), `registry.rs` (asset registration), `handoff/setup.rs` (recoverable setup tasks), `ui/` (overview, previews, search), `scan/` (registration checker), `crates/noa-app/src/helper_program.rs` (`SCAFFOLD`); guide: `docs/SCAFFOLD.md` | inline setup/registry/handoff/backend/UI tests; `widgets/scaffold/tests/` (`registration_parity.rs` reads the unchanged checker fixtures, plus no-exec and snapshot tests) | `cargo test --manifest-path widgets/scaffold/Cargo.toml --locked --offline` |

## Widget usability

| Behavior | Implementation | Focused verification |
| --- | --- | --- |
| Shared appearance, font scale and density | `widgets/shared/appearance.rs`, each widget's startup and native screen | Shared `appearance::tests` in widget binary tests; all widget `cargo check --all-targets` |
| Japanese purpose discovery and display-settings launch | `crates/noa-app/src/command_palette.rs`, `commands/command.rs`, `app/commands.rs` | `cargo test -p noa-app widget_discovery_tests` |
| Explicit Tasks → Review/Verify handoff | Three identical `task_links.rs` copies; Review/Verify CLI, start form and live runner | `task_handoff_args_tests`; `explicit_handoff_does_not_switch_to_a_newer_run_of_the_same_commit` |
| Review compact panes and preserved draft | `widgets/review/src/ui/layout.rs`, `ui/start.rs` | `compact_layout_reuses_panes_and_preserves_the_search_draft` |
| Planned/actual calendar and compact Tasks | `widgets/tasks/src/ui/views.rs`, `ui/dates.rs`, `ui/mod.rs` | Inline and UI tests in Tasks |
| Workflow assignment presets and Tasks results | `widgets/workflow/src/model.rs`, `runner.rs`, `tasks.rs`; `widgets/tasks/src/workflow_links.rs` | Workflow library tests, Tasks handoff/result tests |
| Repository widget processing lifecycle | `crates/noa-ipc/src/widget.rs`, `crates/noa-app/src/widget/mod.rs`, `widgets/widget-host/src/screen.rs` | Widget operation, recovery and host status tests |
| Catalog fault isolation and fixture previews | `crates/noa-widget/src/discovery.rs`, `widgets/widget-host/src/catalog.rs` | `cargo test -p noa-widget`; widget-host catalog/image tests |
| Verify native evidence previews | `widgets/verify/src/thumbnail.rs`, `native_preview.rs`, `ui/images.rs`, `ui/evidence.rs` | Decoder/hash/bounds tests, image-cache tests, native-preview CLI and layout tests |
