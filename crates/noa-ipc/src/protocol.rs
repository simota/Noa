//! JSON-RPC 2.0 wire types for the noa IPC protocol.
//!
//! Every method/notification in the locked spec (`docs/specs/noa-server.md`,
//! §L2 "JSON-RPC 2.0 メソッド表") has its params/result struct here. IDs
//! (`windowGroupId`/`windowId`/`paneId`/`subscriptionId`) are u64 internally
//! but always decimal strings on the wire (JS safe-integer limits).

use serde::de::{self, Visitor};
use serde::{Deserialize, Serialize};
use std::fmt;

/// Current protocol major version. Version 3 requires cell-exact seed replay
/// (`CSI > Ph;Pl;Pw $ v` and `CSI > Ph;Pl $ x`) on attach clients.
pub const PROTOCOL_VERSION: u64 = 3;

/// Default `getText` response cap, in UTF-8 bytes (FR-8).
pub const DEFAULT_TEXT_MAX_BYTES: usize = 256 * 1024;

/// Hard ceiling on a client-requested `getText` `maxBytes`, in UTF-8 bytes
/// (NFR-4). A client-supplied `maxBytes` larger than this is clamped down to
/// it before the request ever reaches the backend, so an authenticated
/// client can't force an unbounded scrollback walk under the terminal lock
/// by simply asking for a huge `maxBytes`.
pub const MAX_TEXT_MAX_BYTES: usize = 1024 * 1024;

/// Default `getGrid` response cap, in serialized bytes (FR-9 / NFR-4).
pub const DEFAULT_GRID_CAP_BYTES: usize = 256 * 1024;

/// A `windowGroupId` / `windowId` / `paneId` / `subscriptionId`: u64
/// internally, decimal string on the wire.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub struct WireId(pub u64);

impl From<u64> for WireId {
    fn from(v: u64) -> Self {
        WireId(v)
    }
}

impl Serialize for WireId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.0.to_string())
    }
}

impl<'de> Deserialize<'de> for WireId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct WireIdVisitor;
        impl Visitor<'_> for WireIdVisitor {
            type Value = WireId;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a decimal string or integer id")
            }
            fn visit_str<E>(self, v: &str) -> Result<WireId, E>
            where
                E: de::Error,
            {
                v.parse::<u64>()
                    .map(WireId)
                    .map_err(|_| E::custom("invalid decimal id"))
            }
            fn visit_u64<E>(self, v: u64) -> Result<WireId, E>
            where
                E: de::Error,
            {
                Ok(WireId(v))
            }
        }
        deserializer.deserialize_any(WireIdVisitor)
    }
}

/// `{ text, fg?, bg? }` color: `#rrggbb` truecolor or a palette index.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum SpanColor {
    Hex(#[serde(with = "hex_color")] (u8, u8, u8)),
    Palette(u8),
}

impl SpanColor {
    pub fn rgb(r: u8, g: u8, b: u8) -> Self {
        SpanColor::Hex((r, g, b))
    }
}

mod hex_color {
    use serde::{Deserialize, Deserializer, Serializer, de::Error};

    pub fn serialize<S>(v: &(u8, u8, u8), s: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        s.serialize_str(&format!("#{:02x}{:02x}{:02x}", v.0, v.1, v.2))
    }

    pub fn deserialize<'de, D>(d: D) -> Result<(u8, u8, u8), D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(d)?;
        let s = s
            .strip_prefix('#')
            .ok_or_else(|| D::Error::custom("expected #rrggbb"))?;
        // Length alone doesn't make the string ASCII; slicing a non-ASCII
        // payload by byte offset would panic instead of failing to parse.
        if s.len() != 6 || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(D::Error::custom("expected #rrggbb"));
        }
        let byte = |i: usize| -> Result<u8, D::Error> {
            u8::from_str_radix(&s[i..i + 2], 16).map_err(|_| D::Error::custom("invalid hex"))
        };
        Ok((byte(0)?, byte(2)?, byte(4)?))
    }
}

/// A cell rendition attribute flag, serialized as a lowercase string.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Attr {
    Bold,
    Faint,
    Italic,
    Underline,
    DoubleUnderline,
    CurlyUnderline,
    DottedUnderline,
    DashedUnderline,
    Blink,
    Inverse,
    Invisible,
    Strikethrough,
    Overline,
}

/// One color/style run within a [`Row`]. Consecutive same-style cells are
/// folded into a single span (PreviewSpan-equivalent), per spec §L2.
#[derive(Clone, Debug, Serialize, Deserialize, Default, PartialEq)]
pub struct Span {
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fg: Option<SpanColor>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bg: Option<SpanColor>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attrs: Option<Vec<Attr>>,
}

/// One grid/preview row: its absolute row index plus color runs.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Row {
    pub row: u64,
    pub spans: Vec<Span>,
}

/// `Panel` — mirrors `SessionCard`, the unit `noa.listPanels` enumerates.
/// `PartialEq` backs the `noa.stateChanged` diff (F-5: only changed/added
/// panels are broadcast, not the full list every wake).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Panel {
    pub window_group_id: WireId,
    pub window_id: WireId,
    pub pane_id: WireId,
    pub name: String,
    pub cwd: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub process: Option<String>,
    pub busy: bool,
    pub attention: bool,
    /// Whether the server exposes this pane through `noa.attach`.
    #[serde(default = "default_attachable")]
    pub attachable: bool,
    pub preview: Vec<Row>,
    /// The pane child's `NOA_SURFACE_ID`, so a process inside a pane can find
    /// its own panel (agent-orchestration D1). Absent for remote panes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub surface_id: Option<String>,
    /// Coding-agent state (agent-orchestration D1). Absent unless the
    /// foreground process is a recognized agent or a report arrived.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<PanelAgent>,
    /// Present only while the pane is held open after its child exited.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit: Option<PanelExit>,
    /// The orchestrator pane that spawned this one, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_pane_id: Option<WireId>,
    /// The label given at spawn, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// What the pane was started with, for a pane an IPC spawn started with
    /// a command. Absent otherwise, and from servers that predate it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spawn: Option<PanelSpawn>,
}

/// `Panel.spawn`: the argv an IPC spawn ran and when the pane started.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PanelSpawn {
    pub argv: Vec<String>,
    /// Unix time in seconds.
    pub started_at: u64,
}

/// `Panel.agent`. `state`/`detail` mirror the last `OSC 777;noa-agent`
/// report; `seq` counts reports for this pane (each report, `clear`
/// included, advances it by one), so a waiter can ignore a report that
/// predates the input it sent.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PanelAgent {
    /// `claude` / `codex` / `agy`, from the foreground process name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// `running` / `permission` / `input` / `finished` / `error`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    pub seq: u64,
    /// What the agent is blocked on, `permission` or `question`: set only
    /// from an `OSC 7501` blocked report, so a `state: input` without it came
    /// from a hook or `OSC 777` report.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocked: Option<String>,
}

/// `Panel.exit`: the held-open pane's child exit code, when one was reported.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PanelExit {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<i32>,
}

const fn default_attachable() -> bool {
    true
}

/// One [`Panel`] key, as serialized, for a `noa.subscribe` /
/// `noa.updateSubscription` `fields` filter. Only a string deserializes:
/// an unknown name, or any other JSON value (including serde's `{"agent":
/// null}` map form of a variant), fails, which the server answers with
/// -32602.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum PanelField {
    WindowGroupId,
    WindowId,
    PaneId,
    Name,
    Cwd,
    Branch,
    Process,
    Busy,
    Attention,
    Attachable,
    Preview,
    SurfaceId,
    Agent,
    Exit,
    ParentPaneId,
    Label,
    Spawn,
}

/// The derived name → [`PanelField`] mapping, kept private so the only
/// public way in is [`PanelField`]'s string-only `Deserialize` (a derived
/// enum would also accept serde's map form of a variant). Its variants must
/// match `PanelField`'s; `panel_field_names_are_exactly_the_serialized_panel_keys`
/// parses every name through it.
#[derive(Deserialize)]
#[serde(remote = "PanelField", rename_all = "camelCase")]
enum PanelFieldName {
    WindowGroupId,
    WindowId,
    PaneId,
    Name,
    Cwd,
    Branch,
    Process,
    Busy,
    Attention,
    Attachable,
    Preview,
    SurfaceId,
    Agent,
    Exit,
    ParentPaneId,
    Label,
    Spawn,
}

impl<'de> Deserialize<'de> for PanelField {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let name = String::deserialize(deserializer)?;
        PanelFieldName::deserialize(de::IntoDeserializer::<D::Error>::into_deserializer(name))
    }
}

impl PanelField {
    /// Every field, in discriminant order.
    const ALL_FIELDS: [PanelField; 17] = [
        PanelField::WindowGroupId,
        PanelField::WindowId,
        PanelField::PaneId,
        PanelField::Name,
        PanelField::Cwd,
        PanelField::Branch,
        PanelField::Process,
        PanelField::Busy,
        PanelField::Attention,
        PanelField::Attachable,
        PanelField::Preview,
        PanelField::SurfaceId,
        PanelField::Agent,
        PanelField::Exit,
        PanelField::ParentPaneId,
        PanelField::Label,
        PanelField::Spawn,
    ];

    const fn bit(self) -> u32 {
        1 << (self as u32)
    }
}

/// A set of [`PanelField`]s, one bit per field.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct FieldMask(u32);

impl FieldMask {
    pub const EMPTY: FieldMask = FieldMask(0);
    /// Every field. A panel added since the previous listing carries this.
    pub const ALL: FieldMask = FieldMask((1 << PanelField::ALL_FIELDS.len()) - 1);

    pub fn from_fields(fields: &[PanelField]) -> FieldMask {
        FieldMask(fields.iter().fold(0, |mask, field| mask | field.bit()))
    }

    pub fn intersects(self, other: FieldMask) -> bool {
        self.0 & other.0 != 0
    }

    pub fn is_empty(self) -> bool {
        self.0 == 0
    }
}

impl Panel {
    /// The fields of `self` that differ from `previous`. Both panels are
    /// destructured without `..`, so a new `Panel` field does not compile
    /// until it is mapped to a [`PanelField`].
    pub fn changed_fields(&self, previous: &Panel) -> FieldMask {
        let Panel {
            window_group_id,
            window_id,
            pane_id,
            name,
            cwd,
            branch,
            process,
            busy,
            attention,
            attachable,
            preview,
            surface_id,
            agent,
            exit,
            parent_pane_id,
            label,
            spawn,
        } = self;
        let Panel {
            window_group_id: prev_window_group_id,
            window_id: prev_window_id,
            pane_id: prev_pane_id,
            name: prev_name,
            cwd: prev_cwd,
            branch: prev_branch,
            process: prev_process,
            busy: prev_busy,
            attention: prev_attention,
            attachable: prev_attachable,
            preview: prev_preview,
            surface_id: prev_surface_id,
            agent: prev_agent,
            exit: prev_exit,
            parent_pane_id: prev_parent_pane_id,
            label: prev_label,
            spawn: prev_spawn,
        } = previous;
        let diffs = [
            (
                window_group_id != prev_window_group_id,
                PanelField::WindowGroupId,
            ),
            (window_id != prev_window_id, PanelField::WindowId),
            (pane_id != prev_pane_id, PanelField::PaneId),
            (name != prev_name, PanelField::Name),
            (cwd != prev_cwd, PanelField::Cwd),
            (branch != prev_branch, PanelField::Branch),
            (process != prev_process, PanelField::Process),
            (busy != prev_busy, PanelField::Busy),
            (attention != prev_attention, PanelField::Attention),
            (attachable != prev_attachable, PanelField::Attachable),
            (preview != prev_preview, PanelField::Preview),
            (surface_id != prev_surface_id, PanelField::SurfaceId),
            (agent != prev_agent, PanelField::Agent),
            (exit != prev_exit, PanelField::Exit),
            (
                parent_pane_id != prev_parent_pane_id,
                PanelField::ParentPaneId,
            ),
            (label != prev_label, PanelField::Label),
            (spawn != prev_spawn, PanelField::Spawn),
        ];
        FieldMask(
            diffs
                .iter()
                .filter(|(changed, _)| *changed)
                .fold(0, |mask, (_, field)| mask | field.bit()),
        )
    }
}

#[cfg(test)]
mod panel_tests {
    use super::Panel;

    #[test]
    fn missing_attachable_field_defaults_to_true_for_protocol_v1_peers() {
        let panel: Panel = serde_json::from_value(serde_json::json!({
            "windowGroupId": "1",
            "windowId": "2",
            "paneId": "3",
            "name": "shell",
            "cwd": "/tmp",
            "busy": false,
            "attention": false,
            "preview": []
        }))
        .unwrap();

        assert!(panel.attachable);
    }
}

/// `source` for `noa.getText`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TextSource {
    Screen,
    Scrollback,
}

/// `direction` for `noa.split`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SplitDirection {
    Horizontal,
    Vertical,
}

/// Subscribable event kinds for `noa.subscribe`.
///
/// The derived `Deserialize` also accepts serde's map form of a variant
/// (`{"output": null}`). `noa.subscribe` has always accepted it and keeps
/// doing so (FR-19). `noa.updateSubscription` is newer and reads its
/// `events` through the private `EventKindName`, which takes only the strings.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    StateChanged,
    Output,
}

/// One `noa.updateSubscription` `events` element: an [`EventKind`] given as
/// the string `"state_changed"` or `"output"`. Any other JSON value fails,
/// including the map form that [`EventKind`]'s own `Deserialize` accepts,
/// and the server answers that with -32602.
struct EventKindName(EventKind);

impl<'de> Deserialize<'de> for EventKindName {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let name = String::deserialize(deserializer)?;
        EventKind::deserialize(de::IntoDeserializer::<D::Error>::into_deserializer(name))
            .map(EventKindName)
    }
}

// ---- noa.hello ----

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HelloParams {
    pub protocol_version: u64,
    #[serde(default)]
    pub token: Option<String>,
    #[serde(default)]
    pub scopes: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HelloResult {
    pub protocol_version: u64,
    pub granted_scopes: Vec<String>,
    pub server_version: String,
    /// Random id minted once per server process. Pane ids are process-local
    /// counters, so a client that auto-reconnects after the server restarted
    /// must compare this against the id it attached under and refuse to
    /// silently re-attach to whatever pane now holds the same number (B01,
    /// 2026-09 audit). Empty when talking to a server predating the field.
    #[serde(default)]
    pub server_instance_id: String,
}

// ---- noa.listPanels ----

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ListPanelsResult {
    pub panels: Vec<Panel>,
}

// ---- noa.getText ----

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetTextParams {
    pub pane_id: WireId,
    pub source: TextSource,
    #[serde(default)]
    pub max_bytes: Option<usize>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetTextResult {
    pub pane_id: WireId,
    pub text: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub truncated: bool,
    /// The pane is on the alternate screen, which keeps no scrollback: a
    /// `scrollback` read returns only what the screen shows. Absent = false.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub alternate_screen: bool,
}

// ---- noa.getGrid ----

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetGridParams {
    pub pane_id: WireId,
    pub start_row: u64,
    pub row_count: u64,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GetGridResult {
    pub pane_id: WireId,
    pub cols: u32,
    pub start_row: u64,
    /// Opaque identity of the row-coordinate space. Clients must discard all
    /// cached rows when this value changes.
    pub coordinate_generation: u64,
    /// Oldest retained session-absolute row coordinate. Coordinates below
    /// this value have been evicted and will not be reused within this
    /// coordinate generation.
    pub oldest_row: u64,
    /// Exclusive end of the currently retained session-absolute row range.
    /// Clients use this to request the tail without guessing its location.
    pub next_row: u64,
    pub rows: Vec<Row>,
    pub has_more: bool,
}

// ---- noa.sendText ----

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SendTextParams {
    pub pane_id: WireId,
    pub text: String,
    /// Omitted or `true`: the existing behavior — `text` goes through the
    /// same bracketed-paste-aware encoding as an AppleScript `input text` or
    /// a clipboard paste. `false`: raw injection — `text`'s UTF-8 bytes are
    /// written to the pty as-is, bypassing the bracketed-paste wrap, so a
    /// lone `"\r"` acts as Enter for the running app (e.g. scripting a TUI).
    #[serde(default)]
    pub paste: Option<bool>,
    /// With `paste`: write nothing and answer `-32010` (NotReady) while the
    /// pane has not enabled bracketed paste (DECSET 2004), so multi-line text
    /// is never typed into a program that is still starting. Omitted = false.
    #[serde(default)]
    pub require_paste_mode: Option<bool>,
    /// Write nothing and answer `-32010` (NotReady) when the pane's
    /// foreground process, probed at the moment of the request, is a plain
    /// shell ([`crate::is_plain_shell`]): text meant for an agent would run
    /// there as commands. The panel's `process` field can lag the real
    /// foreground process by a few seconds, so callers cannot check this
    /// themselves. A pane whose process cannot be probed (a remote pane) is
    /// not refused. Omitted = false.
    #[serde(default)]
    pub refuse_shell: Option<bool>,
}

/// Whether `process` (a process name, possibly a path or a login shell's
/// `-zsh`) is a plain interactive shell, where pasted text followed by Enter
/// runs as a command line.
pub fn is_plain_shell(process: &str) -> bool {
    let name = process.rsplit('/').next().unwrap_or(process);
    let name = name.strip_prefix('-').unwrap_or(name);
    matches!(
        name,
        "sh" | "bash" | "zsh" | "fish" | "dash" | "ksh" | "tcsh" | "csh" | "nu" | "pwsh"
    )
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OkResult {
    pub ok: bool,
}

impl OkResult {
    pub fn ok() -> Self {
        OkResult { ok: true }
    }
}

// ---- noa.focusPane / noa.closePane ----

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PaneIdParams {
    pub pane_id: WireId,
}

// ---- noa.attach / noa.detach / noa.resizePane ----

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttachParams {
    pub pane_id: WireId,
}

/// Connection information for one reserved raw attach channel. Deliberately
/// does not implement `Debug`: `attach_token` is a one-time secret and must
/// not leak through routine structured logging.
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttachResult {
    pub attach_token: String,
    pub attach_url: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DetachParams {
    pub pane_id: WireId,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResizePaneParams {
    pub pane_id: WireId,
    pub cols: u16,
    pub rows: u16,
}

// ---- noa.newTab ----

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewTabParams {
    #[serde(default)]
    pub window_id: Option<WireId>,
    #[serde(flatten)]
    pub spawn: SpawnParams,
}

/// What a new pane runs (agent-orchestration D2): the IPC form of the
/// AppleScript `surface configuration`. `command` is an argv, spawned
/// directly (resolved through `env.PATH` when given) instead of the shell.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SpawnConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub env: std::collections::BTreeMap<String, String>,
    /// Written raw to the new pane's pty right after spawn.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub initial_input: Option<String>,
    /// Keep the pane open after its child exits (until closed), so its
    /// output and exit code stay readable.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub wait_after_command: bool,
}

impl SpawnConfig {
    /// Whether this config makes the new pane run or type something, which
    /// takes the `input` scope on top of `control`.
    pub fn needs_input_scope(&self) -> bool {
        self.command.is_some() || self.initial_input.is_some() || !self.env.is_empty()
    }
}

/// The spawn extensions `noa.newTab`/`noa.split` share (agent-orchestration
/// D2). All optional; a request with none of them behaves exactly as before.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SpawnParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config: Option<SpawnConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_pane_id: Option<WireId>,
    /// `false` keeps keyboard focus where it was. Omitted means `true`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub focus: Option<bool>,
    /// With `parentPaneId`: refuse with `-32011` (LimitReached) when that
    /// parent already has this many panes (exited-but-open ones included).
    /// The count and the spawn are one step on the app's UI thread, so
    /// concurrent spawns cannot overshoot it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_children: Option<u32>,
    /// `noa.newTab` only: seed the new tab's agent-CLI auto approval (the
    /// per-tab Toggle Auto Approve state) instead of the `auto-approve`
    /// config default. `true` needs the `input` scope.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_approve: Option<bool>,
}

impl SpawnParams {
    /// Whether any extension is present; `false` keeps the request on the
    /// pre-orchestration `new_tab`/`split` backend path.
    pub fn is_extended(&self) -> bool {
        self.config.is_some()
            || self.label.is_some()
            || self.parent_pane_id.is_some()
            || self.focus.is_some()
            || self.auto_approve.is_some()
    }
}

/// Upper bound on `SpawnParams.label`, in bytes.
pub const MAX_SPAWN_LABEL_BYTES: usize = 128;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PaneIdResult {
    pub pane_id: WireId,
}

// ---- noa.split ----

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SplitParams {
    pub pane_id: WireId,
    pub direction: SplitPlacement,
    #[serde(flatten)]
    pub spawn: SpawnParams,
}

/// `direction` for `noa.split`: a fixed direction, or `auto`
/// (agent-orchestration D2), where the server picks the pane to split and
/// the direction among `paneId` and the panes it spawned.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SplitPlacement {
    Horizontal,
    Vertical,
    Auto,
    /// A floating pane over `paneId`, not a split (floating-panes REQ-023
    /// as amended: a worker float is listed).
    Float,
}

impl SplitPlacement {
    /// The fixed direction, or `None` for `auto` and `float`.
    pub fn direction(self) -> Option<SplitDirection> {
        match self {
            Self::Horizontal => Some(SplitDirection::Horizontal),
            Self::Vertical => Some(SplitDirection::Vertical),
            Self::Auto | Self::Float => None,
        }
    }
}

// ---- noa.subscribe / noa.unsubscribe ----

/// `noa.subscribe` params, parsed with plain `serde_json::from_value` as
/// before `fields` existed (FR-19). By position, serde binds an array to the
/// fields in declaration order, `[events, paneIds, fields]`, so the order is
/// part of the wire format and `fields` is an optional third element.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubscribeParams {
    pub events: Vec<EventKind>,
    #[serde(default)]
    pub pane_ids: Option<Vec<WireId>>,
    /// `state_changed` only: deliver a changed panel only when one of these
    /// fields changed. Added panels and removals are always delivered.
    /// Field-name strings only, by name or by position (see [`PanelField`]).
    #[serde(default)]
    pub fields: Option<Vec<PanelField>>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SubscribeResult {
    pub subscription_id: WireId,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UnsubscribeParams {
    pub subscription_id: WireId,
}

/// `noa.updateSubscription` params. For each optional key the outer `Option`
/// means "key present": absent keeps the current value, an array replaces it,
/// and `null` (`paneIds` / `fields` only, as `Some(None)`) clears the filter.
/// `events: null` fails to deserialize, and so does an `events` element that
/// is not one of the strings (see `EventKindName`). The server takes these
/// params as an object only (`parse_named_params` in `server.rs`).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateSubscriptionParams {
    pub subscription_id: WireId,
    #[serde(
        default,
        deserialize_with = "present_event_names",
        skip_serializing_if = "Option::is_none"
    )]
    pub events: Option<Vec<EventKind>>,
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    pub pane_ids: Option<Option<Vec<WireId>>>,
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    pub fields: Option<Option<Vec<PanelField>>>,
}

/// Deserializes a key that is present: wraps its value in `Some`, so with
/// `#[serde(default)]` an absent key stays `None`. For `T = Option<_>` a
/// `null` becomes `Some(None)`; for any other `T` a `null` is an error.
fn present<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    T::deserialize(deserializer).map(Some)
}

/// [`present`] for `UpdateSubscriptionParams::events`, reading each element
/// through the string-only [`EventKindName`].
fn present_event_names<'de, D>(deserializer: D) -> Result<Option<Vec<EventKind>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let names = Vec::<EventKindName>::deserialize(deserializer)?;
    let kinds = names.into_iter().map(|EventKindName(kind)| kind).collect();
    Ok(Some(kinds))
}

// ---- notifications ----

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StateChangedParams {
    pub panels: Vec<Panel>,
    /// Panes whose Surface closed since the previous notification, in scope
    /// for this connection. Omitted when empty (additive per FR-19).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub removed_pane_ids: Vec<WireId>,
    /// Set when a subscriber's push queue overflowed and dropped an older
    /// `stateChanged` before this one was sent (F-5, mirrors `noa.output`'s
    /// `dropped` — additive per FR-19, so older clients ignoring the field
    /// stay compatible).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub dropped: bool,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OutputParams {
    pub pane_id: WireId,
    /// Opaque identity of the row-coordinate space. Clients must discard all
    /// cached rows when this value changes.
    pub coordinate_generation: u64,
    pub lines: Vec<Row>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub dropped: bool,
}

/// Tail-priority truncation on a UTF-8 char boundary (FR-8 / AC-9). Returns
/// `(text, truncated)`. Used by the server on top of whatever the backend
/// returns from [`crate::backend::IpcBackend::get_text`] — the backend may
/// also apply its own bound as a memory-safety measure, but the server is
/// the source of truth for the `truncated` flag the client observes.
pub fn truncate_tail(text: &str, max_bytes: usize) -> (String, bool) {
    if text.len() <= max_bytes {
        return (text.to_string(), false);
    }
    (text[tail_start(text, max_bytes)..].to_string(), true)
}

/// Where [`truncate_tail`]'s kept tail begins, for `text` over `max_bytes`.
pub(crate) fn tail_start(text: &str, max_bytes: usize) -> usize {
    let mut idx = text.len() - max_bytes;
    while idx < text.len() && !text.is_char_boundary(idx) {
        idx += 1;
    }
    idx
}

/// Reserved headroom subtracted from `cap_bytes` before summing row sizes
/// (R-4): `serde_json::to_vec(&row)` measures only each `Row` in isolation,
/// so raw per-row summation ignores the `,` array separators between rows,
/// `GetGridResult`'s sibling fields (`paneId`/`cols`/`startRow`/
/// `coordinateGeneration`/`oldestRow`/`nextRow`/`hasMore`),
/// and the JSON-RPC envelope (`jsonrpc`/`id`/`result`) wrapping the whole
/// response — a response summed to exactly `cap_bytes` would then actually
/// serialize larger. A fixed margin is simpler than measuring the full
/// envelope per call and is generous relative to the envelope's real size
/// (well under 200 bytes for these fields).
const GRID_CAP_ENVELOPE_MARGIN: usize = 1024;

/// Per-row overhead outside the row's own serialized bytes: the `,` array
/// separator between consecutive rows (all but the first row in the array).
const GRID_CAP_ROW_SEPARATOR: usize = 1;

/// Caps a set of grid rows to a serialized-size budget (NFR-4 / AC-10 /
/// AC-19), tail is dropped with `hasMore:true`. A single row that alone
/// exceeds the effective cap is rejected by the caller with
/// [`crate::error::ErrorCode::PayloadTooLarge`] (this function reports that
/// case by returning `Err(())`; the caller maps it to the wire error).
pub fn cap_grid_rows(
    rows: Vec<Row>,
    cap_bytes: usize,
) -> Result<(Vec<Row>, bool), GridCapExceeded> {
    let effective_cap = cap_bytes.saturating_sub(GRID_CAP_ENVELOPE_MARGIN);
    let mut out = Vec::with_capacity(rows.len());
    let mut total = 0usize;
    let mut has_more = false;
    for row in rows {
        let size = serde_json::to_vec(&row).map(|v| v.len()).unwrap_or(0) + GRID_CAP_ROW_SEPARATOR;
        if size > effective_cap {
            if out.is_empty() {
                return Err(GridCapExceeded);
            }
            has_more = true;
            break;
        }
        if total + size > effective_cap {
            has_more = true;
            break;
        }
        total += size;
        out.push(row);
    }
    Ok((out, has_more))
}

/// A single grid row alone exceeds the response cap (spec: reject with
/// [`crate::error::ErrorCode::PayloadTooLarge`]).
#[derive(Debug)]
pub struct GridCapExceeded;

#[cfg(test)]
mod cap_grid_rows_tests {
    use super::*;

    fn small_row(i: u64) -> Row {
        Row {
            row: i,
            spans: vec![Span {
                text: "x".repeat(64),
                fg: None,
                bg: None,
                attrs: None,
            }],
        }
    }

    /// Enough small rows to comfortably exceed any `cap_bytes` this module
    /// tests with, so `has_more` is always true regardless of cap size.
    const BOUNDARY_ROW_COUNT: u64 = 20_000;

    /// R-4: raw per-row summation ignores the `,` array separators,
    /// `GetGridResult`'s sibling fields, and the JSON-RPC envelope — so a
    /// response summed to exactly `DEFAULT_GRID_CAP_BYTES` used to exceed it
    /// once actually serialized. With many small rows landing right at the
    /// boundary, the *fully serialized* JSON-RPC response must still fit
    /// within the nominal cap.
    fn assert_boundary_fits_in_cap(cap_bytes: usize) {
        // Rows sized so ~cap_bytes/row_size of them land close to the cap,
        // exercising the boundary rather than stopping far short of it.
        let rows: Vec<Row> = (0..BOUNDARY_ROW_COUNT).map(small_row).collect();
        let (capped, has_more) = cap_grid_rows(rows, cap_bytes).expect("no single row exceeds cap");
        assert!(has_more, "test setup should produce more rows than fit");

        let result = GetGridResult {
            pane_id: WireId(1),
            cols: 120,
            start_row: 0,
            coordinate_generation: 0,
            oldest_row: 0,
            next_row: BOUNDARY_ROW_COUNT,
            rows: capped,
            has_more,
        };
        let envelope = serde_json::json!({
            "jsonrpc": "2.0",
            "id": "1",
            "result": result,
        });
        let serialized = serde_json::to_vec(&envelope).unwrap();
        assert!(
            serialized.len() <= cap_bytes,
            "serialized response {} bytes exceeds cap {cap_bytes} bytes",
            serialized.len()
        );
    }

    #[test]
    fn serialized_response_fits_default_cap() {
        assert_boundary_fits_in_cap(DEFAULT_GRID_CAP_BYTES);
    }

    #[test]
    fn serialized_response_fits_small_cap() {
        // A tighter cap exercises the boundary with fewer rows fitting,
        // catching an off-by-one in the margin/separator accounting that a
        // huge cap might absorb unnoticed.
        assert_boundary_fits_in_cap(4096);
    }
}

#[cfg(test)]
mod hex_color_tests {
    use super::*;

    #[test]
    fn hex_color_rejects_non_ascii_without_panicking() {
        // Six bytes but not six ASCII hex digits: must be an Err, not a
        // byte-offset slice panic inside a multibyte char.
        for input in ["\"#あabc\"", "\"#12345\"", "\"#gggggg\"", "\"123456\""] {
            assert!(serde_json::from_str::<SpanColor>(input).is_err(), "{input}");
        }
        assert_eq!(
            serde_json::from_str::<SpanColor>("\"#0A1b2C\"").unwrap(),
            SpanColor::Hex((0x0a, 0x1b, 0x2c))
        );
    }

    #[test]
    fn hello_result_without_instance_id_still_deserializes() {
        let result: HelloResult = serde_json::from_str(
            r#"{"protocolVersion":2,"grantedScopes":["read"],"serverVersion":"0.1.0"}"#,
        )
        .unwrap();
        assert_eq!(result.server_instance_id, "");
    }
}

#[cfg(test)]
mod subscription_tests {
    use super::*;
    use std::collections::HashSet;

    fn base_panel() -> Panel {
        Panel {
            window_group_id: WireId(1),
            window_id: WireId(2),
            pane_id: WireId(3),
            name: "shell".into(),
            cwd: "/tmp".into(),
            branch: None,
            process: None,
            busy: false,
            attention: false,
            attachable: true,
            preview: vec![],
            surface_id: None,
            agent: None,
            exit: None,
            parent_pane_id: None,
            label: None,
            spawn: None,
        }
    }

    /// Changes exactly `field` on `panel`. The exhaustive match means a new
    /// `PanelField` needs a mutation here before this compiles.
    fn mutate(panel: &mut Panel, field: PanelField) {
        match field {
            PanelField::WindowGroupId => panel.window_group_id = WireId(100),
            PanelField::WindowId => panel.window_id = WireId(100),
            PanelField::PaneId => panel.pane_id = WireId(100),
            PanelField::Name => panel.name = "other".into(),
            PanelField::Cwd => panel.cwd = "/other".into(),
            PanelField::Branch => panel.branch = Some("main".into()),
            PanelField::Process => panel.process = Some("vim".into()),
            PanelField::Busy => panel.busy = !panel.busy,
            PanelField::Attention => panel.attention = !panel.attention,
            PanelField::Attachable => panel.attachable = !panel.attachable,
            PanelField::Preview => {
                panel.preview = vec![Row {
                    row: 0,
                    spans: vec![],
                }]
            }
            PanelField::SurfaceId => panel.surface_id = Some("s".into()),
            PanelField::Agent => {
                panel.agent = Some(PanelAgent {
                    kind: None,
                    state: None,
                    detail: None,
                    seq: 1,
                    blocked: None,
                })
            }
            PanelField::Exit => panel.exit = Some(PanelExit { code: Some(0) }),
            PanelField::ParentPaneId => panel.parent_pane_id = Some(WireId(7)),
            PanelField::Label => panel.label = Some("w".into()),
            PanelField::Spawn => {
                panel.spawn = Some(PanelSpawn {
                    argv: vec!["true".into()],
                    started_at: 1,
                })
            }
        }
    }

    fn field_name(field: PanelField) -> String {
        serde_json::to_value(field)
            .unwrap()
            .as_str()
            .unwrap()
            .to_string()
    }

    #[test]
    fn state_changed_params_omit_removed_pane_ids_when_empty() {
        let params = StateChangedParams {
            panels: vec![],
            removed_pane_ids: vec![],
            dropped: false,
        };
        assert_eq!(
            serde_json::to_value(&params).unwrap(),
            serde_json::json!({ "panels": [] })
        );
    }

    #[test]
    fn state_changed_params_serialize_removed_pane_ids_as_decimal_strings() {
        let params = StateChangedParams {
            panels: vec![],
            removed_pane_ids: vec![WireId(13), WireId(14)],
            dropped: true,
        };
        assert_eq!(
            serde_json::to_value(&params).unwrap(),
            serde_json::json!({ "panels": [], "removedPaneIds": ["13", "14"], "dropped": true })
        );
    }

    /// A `Panel` with every optional field `Some`, written out without
    /// [`PanelField::ALL_FIELDS`], so each `Panel` key is serialized whether
    /// or not that list names it. The struct literal has no `..`, so a new
    /// `Panel` field does not compile until it is set here.
    fn panel_with_every_field_set() -> Panel {
        Panel {
            window_group_id: WireId(1),
            window_id: WireId(2),
            pane_id: WireId(3),
            name: "shell".into(),
            cwd: "/tmp".into(),
            branch: Some("main".into()),
            process: Some("vim".into()),
            busy: false,
            attention: false,
            attachable: true,
            preview: vec![],
            surface_id: Some("s".into()),
            agent: Some(PanelAgent {
                kind: None,
                state: None,
                detail: None,
                seq: 1,
                blocked: None,
            }),
            exit: Some(PanelExit { code: Some(0) }),
            parent_pane_id: Some(WireId(7)),
            label: Some("w".into()),
            spawn: Some(PanelSpawn {
                argv: vec!["true".into()],
                started_at: 1,
            }),
        }
    }

    #[test]
    fn panel_field_names_are_exactly_the_serialized_panel_keys() {
        let value = serde_json::to_value(panel_with_every_field_set()).unwrap();
        let keys: HashSet<String> = value.as_object().unwrap().keys().cloned().collect();
        let names: HashSet<String> = PanelField::ALL_FIELDS.into_iter().map(field_name).collect();
        assert_eq!(
            names, keys,
            "PanelField::ALL_FIELDS vs the serialized Panel keys"
        );
        for name in &names {
            let parsed: PanelField = serde_json::from_value(serde_json::json!(name)).unwrap();
            assert_eq!(&field_name(parsed), name);
        }
    }

    #[test]
    fn each_panel_field_has_a_distinct_bit() {
        let mut seen = 0u32;
        for field in PanelField::ALL_FIELDS {
            let bit = FieldMask::from_fields(&[field]);
            assert_eq!(bit.0.count_ones(), 1, "{field:?}");
            assert!(seen & bit.0 == 0, "{field:?} shares a bit");
            seen |= bit.0;
        }
        assert_eq!(FieldMask(seen), FieldMask::ALL);
        assert_eq!(
            FieldMask::from_fields(&PanelField::ALL_FIELDS),
            FieldMask::ALL
        );
        assert!(FieldMask::EMPTY.is_empty());
        assert!(FieldMask::from_fields(&[]).is_empty());
    }

    #[test]
    fn an_unknown_field_name_fails_to_deserialize() {
        assert!(serde_json::from_value::<PanelField>(serde_json::json!("agnt")).is_err());
        let params = serde_json::json!({ "events": ["state_changed"], "fields": ["agnt"] });
        assert!(serde_json::from_value::<SubscribeParams>(params).is_err());
    }

    #[test]
    fn a_field_name_must_be_a_string() {
        // serde's externally tagged map form of a unit variant.
        for value in [
            serde_json::json!({ "agent": null }),
            serde_json::json!({ "agent": [] }),
            serde_json::json!(12),
            serde_json::json!(null),
        ] {
            assert!(
                serde_json::from_value::<PanelField>(value.clone()).is_err(),
                "{value}"
            );
            let subscribe = serde_json::json!({ "events": ["state_changed"], "fields": [value] });
            assert!(serde_json::from_value::<SubscribeParams>(subscribe).is_err());
            let update = serde_json::json!({ "subscriptionId": "4", "fields": [value] });
            assert!(serde_json::from_value::<UpdateSubscriptionParams>(update).is_err());
        }
        assert_eq!(
            serde_json::from_value::<PanelField>(serde_json::json!("agent")).unwrap(),
            PanelField::Agent
        );
        assert_eq!(
            serde_json::to_value(PanelField::ParentPaneId).unwrap(),
            serde_json::json!("parentPaneId")
        );
    }

    #[test]
    fn subscribe_params_take_an_event_kind_as_a_string_or_in_map_form_as_before() {
        assert_eq!(
            serde_json::to_value([EventKind::StateChanged, EventKind::Output]).unwrap(),
            serde_json::json!(["state_changed", "output"])
        );
        for (value, kind) in [
            (serde_json::json!("state_changed"), EventKind::StateChanged),
            (serde_json::json!("output"), EventKind::Output),
            // serde's externally tagged map form of a unit variant, which
            // `noa.subscribe` has always accepted.
            (
                serde_json::json!({ "state_changed": null }),
                EventKind::StateChanged,
            ),
            (serde_json::json!({ "output": null }), EventKind::Output),
        ] {
            assert_eq!(
                serde_json::from_value::<EventKind>(value.clone()).unwrap(),
                kind,
                "{value}"
            );
            let subscribe = serde_json::json!({ "events": [value] });
            let params: SubscribeParams = serde_json::from_value(subscribe).unwrap();
            assert_eq!(params.events, vec![kind], "{value}");
        }
        for value in [
            serde_json::json!("stateChanged"),
            serde_json::json!(1),
            serde_json::json!(null),
        ] {
            let subscribe = serde_json::json!({ "events": [value] });
            assert!(
                serde_json::from_value::<SubscribeParams>(subscribe).is_err(),
                "{value}"
            );
        }
    }

    #[test]
    fn update_subscription_events_must_be_one_of_the_strings() {
        let params: UpdateSubscriptionParams = serde_json::from_value(
            serde_json::json!({ "subscriptionId": "4", "events": ["state_changed", "output"] }),
        )
        .unwrap();
        assert_eq!(
            params.events,
            Some(vec![EventKind::StateChanged, EventKind::Output])
        );
        for value in [
            // The map form that `noa.subscribe` still accepts.
            serde_json::json!({ "output": null }),
            serde_json::json!({ "state_changed": null }),
            serde_json::json!("stateChanged"),
            serde_json::json!(1),
            serde_json::json!(null),
        ] {
            let update = serde_json::json!({ "subscriptionId": "4", "events": [value] });
            assert!(
                serde_json::from_value::<UpdateSubscriptionParams>(update).is_err(),
                "{value}"
            );
        }
    }

    /// By position `SubscribeParams` binds as serde binds any struct, in
    /// declaration order: `[events]` and `[events, paneIds]` as before
    /// `fields` existed, and `fields` as an optional third element under the
    /// same string-only rule as by name.
    #[test]
    fn subscribe_params_by_position_take_fields_as_an_optional_third_element() {
        let params: SubscribeParams =
            serde_json::from_value(serde_json::json!([["state_changed"]])).unwrap();
        assert_eq!(params.events, vec![EventKind::StateChanged]);
        assert_eq!(params.pane_ids, None);
        assert_eq!(params.fields, None);

        let params: SubscribeParams =
            serde_json::from_value(serde_json::json!([["output"], ["12"]])).unwrap();
        assert_eq!(params.events, vec![EventKind::Output]);
        assert_eq!(params.pane_ids, Some(vec![WireId(12)]));
        assert_eq!(params.fields, None);

        let params: SubscribeParams =
            serde_json::from_value(serde_json::json!([["state_changed"], null, ["agent"]]))
                .unwrap();
        assert_eq!(params.pane_ids, None);
        assert_eq!(params.fields, Some(vec![PanelField::Agent]));

        let params: SubscribeParams =
            serde_json::from_value(serde_json::json!([["state_changed"], ["12"], null])).unwrap();
        assert_eq!(params.pane_ids, Some(vec![WireId(12)]));
        assert_eq!(params.fields, None);

        for params in [
            serde_json::json!([["state_changed"], null, [{ "agent": null }]]),
            serde_json::json!([["state_changed"], null, ["agnt"]]),
            // More elements than fields.
            serde_json::json!([["state_changed"], null, null, null]),
        ] {
            assert!(
                serde_json::from_value::<SubscribeParams>(params.clone()).is_err(),
                "{params}"
            );
        }
    }

    #[test]
    fn changed_fields_reports_exactly_the_mutated_field() {
        for field in PanelField::ALL_FIELDS {
            let previous = base_panel();
            let mut current = base_panel();
            mutate(&mut current, field);
            assert_eq!(
                current.changed_fields(&previous),
                FieldMask::from_fields(&[field]),
                "{field:?}"
            );
        }

        // Two fields at once: both bits, not just the first difference.
        let previous = base_panel();
        let mut current = base_panel();
        mutate(&mut current, PanelField::Busy);
        mutate(&mut current, PanelField::Agent);
        assert_eq!(
            current.changed_fields(&previous),
            FieldMask::from_fields(&[PanelField::Busy, PanelField::Agent])
        );
    }

    #[test]
    fn changed_fields_of_equal_panels_is_empty() {
        let mut panel = base_panel();
        for field in PanelField::ALL_FIELDS {
            mutate(&mut panel, field);
        }
        assert_eq!(panel.changed_fields(&panel.clone()), FieldMask::EMPTY);
        assert_eq!(base_panel().changed_fields(&base_panel()), FieldMask::EMPTY);
    }

    #[test]
    fn update_subscription_params_tell_absent_null_and_array_apart() {
        let absent: UpdateSubscriptionParams =
            serde_json::from_value(serde_json::json!({ "subscriptionId": "4" })).unwrap();
        assert_eq!(absent.subscription_id, WireId(4));
        assert!(absent.events.is_none() && absent.pane_ids.is_none() && absent.fields.is_none());

        let cleared: UpdateSubscriptionParams = serde_json::from_value(
            serde_json::json!({ "subscriptionId": "4", "paneIds": null, "fields": null }),
        )
        .unwrap();
        assert_eq!(cleared.pane_ids, Some(None));
        assert_eq!(cleared.fields, Some(None));

        let replaced: UpdateSubscriptionParams = serde_json::from_value(serde_json::json!({
            "subscriptionId": "4", "events": [], "paneIds": ["12"], "fields": ["agent"]
        }))
        .unwrap();
        assert_eq!(replaced.events, Some(vec![]));
        assert_eq!(replaced.pane_ids, Some(Some(vec![WireId(12)])));
        assert_eq!(replaced.fields, Some(Some(vec![PanelField::Agent])));

        assert!(
            serde_json::from_value::<UpdateSubscriptionParams>(
                serde_json::json!({ "subscriptionId": "4", "events": null })
            )
            .is_err()
        );

        assert_eq!(
            serde_json::to_value(&cleared).unwrap(),
            serde_json::json!({ "subscriptionId": "4", "paneIds": null, "fields": null })
        );
        assert_eq!(
            serde_json::to_value(&absent).unwrap(),
            serde_json::json!({ "subscriptionId": "4" })
        );
    }
}
