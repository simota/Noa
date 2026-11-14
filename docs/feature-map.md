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
| noa-server (JSON-RPC / WebSocket) | `crates/noa-ipc/src/server.rs`, `crates/noa-ipc/src/protocol.rs`, `crates/noa-app/src/ipc_bridge.rs` | `crates/noa-ipc/tests/` | `cargo test -p noa-ipc` |
| Session and scrollback restore | `crates/noa-app/src/session_persist.rs`, `crates/noa-app/src/scrollback_persist.rs`, `crates/noa-app/src/app/session_restore.rs` | inline | `cargo test -p noa-app persist` |
| Quick Terminal | `crates/noa-app/src/app/quick_terminal.rs` | inline | `cargo test -p noa-app quick_terminal` |
| Ghostty parity fixtures | `tests/parity/src/`, `tests/parity/fixtures/` | `tests/parity/tests/` | `cargo test -p noa-parity` |
| Agent status hook | `scripts/noa-agent-hook.py` | `scripts/test_noa_agent_hook.py` | `python3 -I -B scripts/test_noa_agent_hook.py` |
