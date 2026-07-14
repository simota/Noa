//! JSON-RPC 2.0 wire types for the noa IPC protocol.
//!
//! Every method/notification in the locked spec (`docs/specs/noa-server.md`,
//! §L2 "JSON-RPC 2.0 メソッド表") has its params/result struct here. IDs
//! (`windowGroupId`/`windowId`/`paneId`/`subscriptionId`) are u64 internally
//! but always decimal strings on the wire (JS safe-integer limits).

use serde::de::{self, Visitor};
use serde::{Deserialize, Serialize};
use std::fmt;

/// Current protocol major version (additive-only; a major bump is reserved
/// for breaking changes, per FR-19).
pub const PROTOCOL_VERSION: u64 = 1;

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
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
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
        if s.len() != 6 {
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
}

const fn default_attachable() -> bool {
    true
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
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    StateChanged,
    Output,
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
}

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
    pub direction: SplitDirection,
}

// ---- noa.subscribe / noa.unsubscribe ----

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubscribeParams {
    pub events: Vec<EventKind>,
    #[serde(default)]
    pub pane_ids: Option<Vec<WireId>>,
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

// ---- notifications ----

#[derive(Clone, Debug, Serialize)]
pub struct StateChangedParams {
    pub panels: Vec<Panel>,
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
    let start = text.len() - max_bytes;
    let mut idx = start;
    while idx < text.len() && !text.is_char_boundary(idx) {
        idx += 1;
    }
    (text[idx..].to_string(), true)
}

/// Reserved headroom subtracted from `cap_bytes` before summing row sizes
/// (R-4): `serde_json::to_vec(&row)` measures only each `Row` in isolation,
/// so raw per-row summation ignores the `,` array separators between rows,
/// `GetGridResult`'s sibling fields (`paneId`/`cols`/`startRow`/`hasMore`),
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
