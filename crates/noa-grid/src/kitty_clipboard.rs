//! Kitty clipboard protocol (OSC 5522), as Ghostty 1.4.0 implements it
//! (`terminal/kitty/clipboard*.zig`, `termio/stream_handler.zig`).
//!
//! The grid parses and validates packets, runs write transactions, keeps the
//! session's password grants and encodes every reply. Reads and committed
//! writes go to the app as [`KittyClipboardRequest`]s, since only it can touch
//! the system clipboard and prompt the user. Behaviors carried over from
//! Ghostty (which follows kitty's reference implementation):
//!
//! - Malformed metadata (a record without `=`, including empty metadata) and
//!   a missing or unknown `type` drop the packet without a reply.
//! - All base64 is strict RFC 4648: standard alphabet, required padding, no
//!   whitespace. Metadata values and MIME lists must decode to UTF-8.
//! - An invalid value on `wdata`/`walias` aborts an open write with `EINVAL`;
//!   on `read`/`write` the packet is dropped.
//! - The `wdata` payloads for one MIME type form one base64 stream split at
//!   arbitrary packet boundaries; a packet ending on terminal padding
//!   restarts it.
//! - `id` keeps only `[a-zA-Z0-9-_+.]`, at most 512 bytes, and is echoed in
//!   every reply.

use std::collections::VecDeque;
use std::sync::Arc;

use noa_vt::OscTerminator;

use crate::osc::encode_base64;

/// The OSC number.
pub const OSC_NUMBER: &[u8] = b"5522;";
/// Ghostty's cap on an OSC body it buffers (`MAX_ALLOCATING_BUF`); a longer
/// OSC 5522 is dropped.
const MAX_BODY_BYTES: usize = 8 * 1024 * 1024;
const MAX_ID_LEN: usize = 512;
const MAX_PW_LEN: usize = 128;
pub const MAX_MIME_LEN: usize = 256;
const MAX_NAME_LEN: usize = 256;
/// Data MIME types served per read; later ones are never served.
const MAX_READ_MIMES: usize = 4;
const MAX_WRITE_MIMES: usize = 64;
const MAX_WRITE_ALIASES: usize = 64;
/// Largest raw chunk of one `DATA` reply packet.
const MAX_CHUNK: usize = 4096;
/// Largest raw `.` listing payload.
const MAX_LISTING: usize = 4096;
/// MIME types a paste event lists.
pub const MAX_PASTE_EVENT_MIMES: usize = 16;
const MAX_GRANTS: usize = 32;
/// The `.` pseudo type: a read asking for the list of available types.
const TARGETS_MIME: &str = ".";

/// Which clipboard a request targets (`loc=primary`, else the standard one).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClipboardLocation {
    Standard,
    Primary,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Operation {
    Read,
    Write,
    Wdata,
    Walias,
}

impl Operation {
    fn parse(value: &[u8]) -> Option<Self> {
        match value {
            b"read" => Some(Self::Read),
            b"write" => Some(Self::Write),
            b"wdata" => Some(Self::Wdata),
            b"walias" => Some(Self::Walias),
            _ => None,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Write => "write",
            Self::Wdata => "wdata",
            Self::Walias => "walias",
        }
    }
}

/// A reply `status`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KittyClipboardStatus {
    Ok,
    Data,
    Done,
    Efbig,
    Einval,
    Eio,
    Enosys,
    Eperm,
}

impl KittyClipboardStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "OK",
            Self::Data => "DATA",
            Self::Done => "DONE",
            Self::Efbig => "EFBIG",
            Self::Einval => "EINVAL",
            Self::Eio => "EIO",
            Self::Enosys => "ENOSYS",
            Self::Eperm => "EPERM",
        }
    }
}

/// Whether `mime` names plain text (Ghostty's `clipboard.isText`); the app
/// serves every one of them from the clipboard's text.
pub fn is_text_mime(mime: &str) -> bool {
    matches!(
        mime,
        "text/plain" | "text/plain;charset=utf-8" | "UTF8_STRING" | "TEXT" | "STRING"
    )
}

/// A read request (`type=read`) for the app to answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KittyClipboardRead {
    pub location: ClipboardLocation,
    /// Data MIME types in request order (at most four; may repeat).
    pub mimes: Vec<String>,
    /// The `.` listing was requested.
    pub list: bool,
    /// The password, kept only when a non-empty `name` came with it.
    pub pw: String,
    /// A session grant covers it, so no prompt.
    pub granted: bool,
    id: String,
    terminator: OscTerminator,
}

impl KittyClipboardRead {
    /// A failure reply (`EPERM`, `ENOSYS`, `EIO`).
    pub fn reply_status(&self, status: KittyClipboardStatus) -> Vec<u8> {
        let mut out = Vec::new();
        Reply::new(Operation::Read, status, &self.id, self.terminator).encode(&mut out);
        out
    }

    /// The success replies: `OK`, the listing if asked, each requested type's
    /// data in `MAX_CHUNK` pieces, then `DONE`. `data[i]` answers
    /// `self.mimes[i]` (`None` or empty: unavailable, so nothing is sent).
    pub fn reply_success(&self, listing: &[String], data: &[Option<Vec<u8>>]) -> Vec<u8> {
        self.reply_packets(listing, data).concat()
    }

    /// [`Self::reply_success`] as separate packets, for a caller that sends a
    /// large reply piecewise.
    pub fn reply_packets(&self, listing: &[String], data: &[Option<Vec<u8>>]) -> Vec<Vec<u8>> {
        let mut packets = Vec::new();
        let mut push = |reply: Reply| {
            let mut out = Vec::new();
            reply.encode(&mut out);
            packets.push(out);
        };
        let reply = |status| Reply::new(Operation::Read, status, &self.id, self.terminator);
        push(Reply {
            primary: self.location == ClipboardLocation::Primary,
            ..reply(KittyClipboardStatus::Ok)
        });
        if self.list {
            let listing = encode_listing(listing);
            push(Reply {
                mime: Some(TARGETS_MIME),
                payload: Payload::Always(&listing),
                ..reply(KittyClipboardStatus::Data)
            });
        }
        for (mime, data) in self.mimes.iter().zip(data) {
            for chunk in data.as_deref().unwrap_or_default().chunks(MAX_CHUNK) {
                push(Reply {
                    mime: Some(mime),
                    payload: Payload::IfAny(chunk),
                    ..reply(KittyClipboardStatus::Data)
                });
            }
        }
        push(reply(KittyClipboardStatus::Done));
        packets
    }
}

/// `(mime, data)` representations of clipboard contents.
pub type ClipboardContents = Vec<(String, Arc<[u8]>)>;

/// A committed write (`type=write` … final `wdata`) for the app to apply.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KittyClipboardWrite {
    pub location: ClipboardLocation,
    /// `(mime, data)` in arrival order, then resolved aliases, which share
    /// their target's data (so aliases can't multiply the write limit). Empty
    /// clears the clipboard.
    pub contents: ClipboardContents,
    /// The password, kept only when a non-empty `name` came with it.
    pub pw: String,
    /// A session grant covers it, so no prompt.
    pub granted: bool,
    id: String,
    terminator: OscTerminator,
}

impl KittyClipboardWrite {
    /// The final reply: `DONE`, or a failure status.
    pub fn reply_status(&self, status: KittyClipboardStatus) -> Vec<u8> {
        write_status(status, &self.id, self.terminator)
    }
}

/// A request only the app can complete.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KittyClipboardRequest {
    Read(KittyClipboardRead),
    Write(KittyClipboardWrite),
}

/// The bytes of a paste event (mode 5522): `OK`, the type listing and `DONE`,
/// all carrying the one-time password the program reads the data with.
pub fn paste_event(pw: &str, location: ClipboardLocation, mimes: &[String]) -> Vec<u8> {
    let mut out = Vec::new();
    let reply = |status| Reply {
        pw: Some(pw),
        ..Reply::new(Operation::Read, status, "", OscTerminator::St)
    };
    Reply {
        primary: location == ClipboardLocation::Primary,
        ..reply(KittyClipboardStatus::Ok)
    }
    .encode(&mut out);
    let listing = encode_listing(&mimes[..mimes.len().min(MAX_PASTE_EVENT_MIMES)]);
    Reply {
        mime: Some(TARGETS_MIME),
        payload: Payload::Always(&listing),
        ..reply(KittyClipboardStatus::Data)
    }
    .encode(&mut out);
    reply(KittyClipboardStatus::Done).encode(&mut out);
    out
}

/// `t1 t2 …\n`, stopping at the first type that would take it past
/// `MAX_LISTING`; empty for no types.
fn encode_listing(mimes: &[String]) -> Vec<u8> {
    let mut out = Vec::new();
    if mimes.is_empty() {
        return out;
    }
    for (i, mime) in mimes.iter().enumerate() {
        let sep = usize::from(i > 0);
        if out.len() + sep + mime.len() + 1 > MAX_LISTING {
            break;
        }
        if sep == 1 {
            out.push(b' ');
        }
        out.extend_from_slice(mime.as_bytes());
    }
    out.push(b'\n');
    out
}

enum Payload<'a> {
    None,
    /// Sent only when non-empty.
    IfAny(&'a [u8]),
    /// Sent even when empty (the `.` listing).
    Always(&'a [u8]),
}

/// One reply packet: `ESC ] 5522 ; type=…:status=…[:loc=primary][:id=…]
/// [:mime=…][:pw=…][;payload] <terminator>`, keys in that fixed order.
struct Reply<'a> {
    op: Operation,
    status: KittyClipboardStatus,
    primary: bool,
    id: &'a str,
    mime: Option<&'a str>,
    pw: Option<&'a str>,
    payload: Payload<'a>,
    terminator: OscTerminator,
}

impl<'a> Reply<'a> {
    fn new(
        op: Operation,
        status: KittyClipboardStatus,
        id: &'a str,
        terminator: OscTerminator,
    ) -> Self {
        Self {
            op,
            status,
            primary: false,
            id,
            mime: None,
            pw: None,
            payload: Payload::None,
            terminator,
        }
    }

    fn encode(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(b"\x1b]5522;type=");
        out.extend_from_slice(self.op.as_str().as_bytes());
        out.extend_from_slice(b":status=");
        out.extend_from_slice(self.status.as_str().as_bytes());
        if self.primary {
            out.extend_from_slice(b":loc=primary");
        }
        if !self.id.is_empty() {
            out.extend_from_slice(b":id=");
            out.extend_from_slice(self.id.as_bytes());
        }
        if let Some(mime) = self.mime {
            out.extend_from_slice(b":mime=");
            encode_base64(mime.as_bytes(), out);
        }
        if let Some(pw) = self.pw {
            out.extend_from_slice(b":pw=");
            encode_base64(pw.as_bytes(), out);
        }
        match self.payload {
            Payload::IfAny(data) if !data.is_empty() => {
                out.push(b';');
                encode_base64(data, out);
            }
            Payload::Always(data) => {
                out.push(b';');
                encode_base64(data, out);
            }
            _ => {}
        }
        out.extend_from_slice(match self.terminator {
            OscTerminator::Bel => b"\x07",
            OscTerminator::St => b"\x1b\\",
        });
    }
}

fn write_status(status: KittyClipboardStatus, id: &str, terminator: OscTerminator) -> Vec<u8> {
    let mut out = Vec::new();
    Reply::new(Operation::Write, status, id, terminator).encode(&mut out);
    out
}

// ── Metadata ────────────────────────────────────────────────────────

/// The decoded metadata of one packet.
struct Metadata {
    op: Operation,
    location: ClipboardLocation,
    id: String,
    /// Empty means absent.
    mime: String,
    pw: String,
    name: String,
}

/// A decoded metadata value was not valid (bad base64 or UTF-8, or too long).
struct InvalidValue;

#[derive(Default)]
struct RawMetadata<'a> {
    op: Option<&'a [u8]>,
    loc: &'a [u8],
    id: &'a [u8],
    mime: &'a [u8],
    pw: &'a [u8],
    name: &'a [u8],
}

impl<'a> RawMetadata<'a> {
    /// `None` when a record lacks `=`. The last duplicate wins; unknown keys
    /// are ignored.
    fn parse(raw: &'a [u8]) -> Option<Self> {
        let mut out = Self::default();
        for record in raw.split(|&b| b == b':') {
            let eq = record.iter().position(|&b| b == b'=')?;
            let (key, value) = (&record[..eq], &record[eq + 1..]);
            match key {
                b"type" => out.op = Some(value),
                b"loc" => out.loc = value,
                b"id" => out.id = value,
                b"mime" => out.mime = value,
                b"pw" => out.pw = value,
                b"name" => out.name = value,
                _ => {}
            }
        }
        Some(out)
    }

    fn operation(&self) -> Option<Operation> {
        Operation::parse(self.op?)
    }
}

impl Metadata {
    /// `Ok(None)` drops the packet silently.
    fn parse(raw: &[u8]) -> Result<Option<Self>, InvalidValue> {
        let Some(fields) = RawMetadata::parse(raw) else {
            return Ok(None);
        };
        let Some(op) = fields.operation() else {
            return Ok(None);
        };
        let mime = decode_value(fields.mime, MAX_MIME_LEN)?.ok_or(InvalidValue)?;
        // An over-long password can never match a grant: treat it as absent.
        let pw = decode_value(fields.pw, MAX_PW_LEN)?.unwrap_or_default();
        let name = decode_value(fields.name, MAX_NAME_LEN)?.ok_or(InvalidValue)?;
        Ok(Some(Self {
            op,
            location: if fields.loc == b"primary" {
                ClipboardLocation::Primary
            } else {
                ClipboardLocation::Standard
            },
            id: sanitize_id(fields.id),
            mime,
            pw,
            name,
        }))
    }

    /// The password, which counts only with a non-empty name.
    fn effective_pw(&self) -> String {
        if self.name.is_empty() {
            String::new()
        } else {
            self.pw.clone()
        }
    }
}

fn sanitize_id(value: &[u8]) -> String {
    value
        .iter()
        .filter(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'+' | b'.'))
        .take(MAX_ID_LEN)
        .map(|&b| char::from(b))
        .collect()
}

/// A base64 UTF-8 metadata value: `Ok(None)` when it decodes longer than
/// `max_len`.
fn decode_value(value: &[u8], max_len: usize) -> Result<Option<String>, InvalidValue> {
    if value.len() > max_len.div_ceil(3) * 4 {
        return Ok(None);
    }
    let decoded = decode_strict(value).ok_or(InvalidValue)?;
    let text = String::from_utf8(decoded).map_err(|_| InvalidValue)?;
    Ok((text.len() <= max_len).then_some(text))
}

/// A base64 payload holding a whitespace-separated UTF-8 list.
fn decode_list(payload: &[u8]) -> Option<Vec<String>> {
    let text = String::from_utf8(decode_strict(payload)?).ok()?;
    Some(
        text.split(|c: char| c.is_ascii_whitespace())
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .collect(),
    )
}

// ── Strict base64 ───────────────────────────────────────────────────

fn base64_value(b: u8) -> Option<u8> {
    match b {
        b'A'..=b'Z' => Some(b - b'A'),
        b'a'..=b'z' => Some(b - b'a' + 26),
        b'0'..=b'9' => Some(b - b'0' + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    }
}

/// Decode one complete group: padding only as its last one or two
/// characters. Returns whether it was padded.
fn decode_group(group: [u8; 4], out: &mut Vec<u8>) -> Option<bool> {
    let pad = match (group[2], group[3]) {
        (b'=', b'=') => 2,
        (_, b'=') => 1,
        _ => 0,
    };
    let mut n = 0u32;
    for (i, &b) in group.iter().enumerate() {
        let v = if i >= 4 - pad { 0 } else { base64_value(b)? };
        n = (n << 6) | u32::from(v);
    }
    let bytes = n.to_be_bytes();
    out.extend_from_slice(&bytes[1..4 - pad]);
    Some(pad > 0)
}

/// Strict RFC 4648 decode with required padding.
fn decode_strict(input: &[u8]) -> Option<Vec<u8>> {
    if !input.len().is_multiple_of(4) {
        return None;
    }
    let mut out = Vec::with_capacity(input.len() / 4 * 3);
    let groups = input.len() / 4;
    for (i, group) in input.chunks_exact(4).enumerate() {
        let padded = decode_group(group.try_into().ok()?, &mut out)?;
        if padded && i + 1 != groups {
            return None;
        }
    }
    Some(out)
}

/// One base64 stream fed in pieces (Ghostty's `simd.base64.Streaming`): a
/// group decodes once all four characters arrive, and terminal padding must
/// end the piece that carries it, which also restarts the stream.
#[derive(Default)]
struct StreamingBase64 {
    carry: Vec<u8>,
}

impl StreamingBase64 {
    fn valid_partial(pos: usize, b: u8) -> bool {
        base64_value(b).is_some() || (pos >= 2 && b == b'=')
    }

    /// Decode `input`'s complete groups into `out`; `None` when invalid.
    fn feed(&mut self, mut input: &[u8], out: &mut Vec<u8>) -> Option<()> {
        if input.is_empty() {
            return Some(());
        }
        if !self.carry.is_empty() {
            let take = (4 - self.carry.len()).min(input.len());
            for (i, &b) in input[..take].iter().enumerate() {
                if !Self::valid_partial(self.carry.len() + i, b) {
                    return None;
                }
            }
            self.carry.extend_from_slice(&input[..take]);
            input = &input[take..];
            if self.carry.len() < 4 {
                return Some(());
            }
            let group: [u8; 4] = std::mem::take(&mut self.carry).try_into().ok()?;
            if decode_group(group, out)? {
                return input.is_empty().then_some(());
            }
        }
        let bulk = input.len() - input.len() % 4;
        let decoded = decode_strict(&input[..bulk])?;
        out.extend_from_slice(&decoded);
        if bulk > 0 && input[bulk - 1] == b'=' {
            return (bulk == input.len()).then_some(());
        }
        let tail = &input[bulk..];
        if !tail
            .iter()
            .enumerate()
            .all(|(i, &b)| Self::valid_partial(i, b))
        {
            return None;
        }
        self.carry = tail.to_vec();
        Some(())
    }

    /// End the stream: a carried partial group means it was not padded.
    fn finish(&mut self) -> Option<()> {
        std::mem::take(&mut self.carry).is_empty().then_some(())
    }
}

// ── Write transactions ──────────────────────────────────────────────

struct WriteEntry {
    mime: String,
    start: usize,
    len: usize,
}

/// One open write: a `type=write` plus its `wdata` and `walias` packets.
struct WriteState {
    location: ClipboardLocation,
    id: String,
    pw: String,
    name: String,
    /// All decoded data; re-sent types keep counting against the limit.
    spool: Vec<u8>,
    entries: Vec<WriteEntry>,
    /// `(alias, target)`.
    aliases: Vec<(String, String)>,
    /// Captured at `type=write`, so a later config change doesn't apply.
    max_size: Option<usize>,
    current: Option<usize>,
    decoder: StreamingBase64,
}

enum WriteError {
    TooLarge,
    Invalid,
}

impl WriteState {
    fn new(meta: &Metadata, max_size: Option<usize>) -> Self {
        Self {
            location: meta.location,
            id: meta.id.clone(),
            pw: meta.pw.clone(),
            name: meta.name.clone(),
            spool: Vec::new(),
            entries: Vec::new(),
            aliases: Vec::new(),
            max_size,
            current: None,
            decoder: StreamingBase64::default(),
        }
    }

    fn finish_current(&mut self) -> Result<(), WriteError> {
        if let Some(idx) = self.current {
            self.decoder.finish().ok_or(WriteError::Invalid)?;
            self.entries[idx].len = self.spool.len() - self.entries[idx].start;
        }
        Ok(())
    }

    fn data(&mut self, mime: &str, payload: &[u8]) -> Result<(), WriteError> {
        if self
            .current
            .is_none_or(|idx| self.entries[idx].mime != mime)
        {
            self.finish_current()?;
            // Re-using an earlier type starts it over.
            if let Some(idx) = self.entries.iter().position(|e| e.mime == mime) {
                self.entries[idx].start = self.spool.len();
                self.entries[idx].len = 0;
                self.current = Some(idx);
            } else if self.entries.len() >= MAX_WRITE_MIMES {
                self.current = None;
                return Ok(());
            } else {
                self.entries.push(WriteEntry {
                    mime: mime.to_owned(),
                    start: self.spool.len(),
                    len: 0,
                });
                self.current = Some(self.entries.len() - 1);
            }
        }
        let before = self.spool.len();
        self.decoder
            .feed(payload, &mut self.spool)
            .ok_or(WriteError::Invalid)?;
        if self.max_size.is_some_and(|max| self.spool.len() > max) {
            self.spool.truncate(before);
            return Err(WriteError::TooLarge);
        }
        Ok(())
    }

    fn alias(&mut self, target: &str, payload: &[u8]) -> Result<(), WriteError> {
        let names = decode_list(payload).ok_or(WriteError::Invalid)?;
        for name in names {
            if name.len() > MAX_MIME_LEN {
                continue;
            }
            if let Some(alias) = self.aliases.iter_mut().find(|(a, _)| *a == name) {
                alias.1 = target.to_owned();
            } else if self.aliases.len() >= MAX_WRITE_ALIASES {
                return Ok(());
            } else {
                self.aliases.push((name, target.to_owned()));
            }
        }
        Ok(())
    }

    /// The final contents: entries in arrival order, then aliases resolved
    /// one after another (so chains work). An alias of a type with no data is
    /// dropped; one naming an existing entry replaces its data.
    fn commit(&mut self) -> Result<ClipboardContents, WriteError> {
        self.finish_current()?;
        let mut contents: ClipboardContents = self
            .entries
            .iter()
            .map(|e| {
                (
                    e.mime.clone(),
                    Arc::from(&self.spool[e.start..e.start + e.len]),
                )
            })
            .collect();
        for (alias, target) in &self.aliases {
            let Some(data) = contents
                .iter()
                .find(|(m, _)| m == target)
                .map(|(_, d)| d.clone())
            else {
                continue;
            };
            match contents.iter_mut().find(|(m, _)| m == alias) {
                Some(existing) => existing.1 = data,
                None => contents.push((alias.clone(), data)),
            }
        }
        Ok(contents)
    }
}

// ── Grants ──────────────────────────────────────────────────────────

/// Session password grants (Ghostty's `clipboard_grants.zig`).
#[derive(Default)]
pub(crate) struct Grants {
    entries: VecDeque<Grant>,
}

struct Grant {
    pw: String,
    read: bool,
    write: bool,
    one_time: bool,
}

impl Grants {
    /// Allow `pw` for reads and/or writes. Granting a known password adds the
    /// direction; it stays one-time only if both grants were. The oldest of
    /// more than `MAX_GRANTS` is evicted.
    pub(crate) fn grant(&mut self, pw: &str, read: bool, write: bool, one_time: bool) {
        if pw.is_empty() {
            return;
        }
        if let Some(grant) = self.entries.iter_mut().find(|g| g.pw == pw) {
            grant.read |= read;
            grant.write |= write;
            grant.one_time &= one_time;
            return;
        }
        if self.entries.len() >= MAX_GRANTS {
            self.entries.pop_front();
        }
        self.entries.push_back(Grant {
            pw: pw.to_owned(),
            read,
            write,
            one_time,
        });
    }

    /// Whether `pw` is granted for the direction. A one-time grant is used up
    /// by any check, even one for the other direction.
    fn use_grant(&mut self, pw: &str, write: bool) -> bool {
        if pw.is_empty() {
            return false;
        }
        let Some(idx) = self.entries.iter().position(|g| g.pw == pw) else {
            return false;
        };
        let grant = &self.entries[idx];
        let allowed = if write { grant.write } else { grant.read };
        if grant.one_time {
            self.entries.remove(idx);
        }
        allowed
    }

    pub(crate) fn clear(&mut self) {
        self.entries.clear();
    }
}

// ── Packet handling ─────────────────────────────────────────────────

/// Per-terminal protocol state.
#[derive(Default)]
pub(crate) struct KittyClipboardState {
    pub(crate) grants: Grants,
    write: Option<WriteState>,
}

/// What the terminal's policy allows when a packet arrives.
pub(crate) struct WritePolicy {
    pub(crate) deny: bool,
    pub(crate) max_size: Option<usize>,
}

impl KittyClipboardState {
    /// Handle the OSC body after `5522;`. Immediate replies go to `replies`;
    /// requests for the app to `requests`.
    pub(crate) fn handle(
        &mut self,
        body: &[u8],
        terminator: OscTerminator,
        policy: &WritePolicy,
        replies: &mut Vec<u8>,
        requests: &mut Vec<KittyClipboardRequest>,
    ) {
        if body.len() > MAX_BODY_BYTES {
            return;
        }
        let (raw, payload) = match body.iter().position(|&b| b == b';') {
            Some(i) => (&body[..i], &body[i + 1..]),
            None => (body, &body[body.len()..]),
        };
        let meta = match Metadata::parse(raw) {
            Ok(Some(meta)) => meta,
            Ok(None) => return,
            Err(InvalidValue) => {
                let op = RawMetadata::parse(raw).and_then(|f| f.operation());
                if matches!(op, Some(Operation::Wdata | Operation::Walias)) {
                    self.finish_write(KittyClipboardStatus::Einval, terminator, replies);
                }
                return;
            }
        };
        match meta.op {
            Operation::Read => self.read(&meta, payload, terminator, requests),
            Operation::Write => {
                self.write = None;
                if policy.deny {
                    replies.extend(write_status(
                        KittyClipboardStatus::Eperm,
                        &meta.id,
                        terminator,
                    ));
                } else {
                    self.write = Some(WriteState::new(&meta, policy.max_size));
                }
            }
            Operation::Wdata => {
                let Some(state) = self.write.as_mut() else {
                    return;
                };
                if meta.mime.is_empty() {
                    self.commit(terminator, replies, requests);
                    return;
                }
                match state.data(&meta.mime, payload) {
                    Ok(()) => {}
                    Err(WriteError::TooLarge) => {
                        self.finish_write(KittyClipboardStatus::Efbig, terminator, replies)
                    }
                    Err(WriteError::Invalid) => {
                        self.finish_write(KittyClipboardStatus::Einval, terminator, replies)
                    }
                }
            }
            Operation::Walias => {
                let Some(state) = self.write.as_mut() else {
                    return;
                };
                if meta.mime.is_empty() || state.alias(&meta.mime, payload).is_err() {
                    self.finish_write(KittyClipboardStatus::Einval, terminator, replies);
                }
            }
        }
    }

    fn read(
        &mut self,
        meta: &Metadata,
        payload: &[u8],
        terminator: OscTerminator,
        requests: &mut Vec<KittyClipboardRequest>,
    ) {
        let Some(names) = decode_list(payload) else {
            return;
        };
        let mut list = false;
        let mut mimes = Vec::new();
        for name in names {
            if name == TARGETS_MIME {
                list = true;
            } else if mimes.len() < MAX_READ_MIMES {
                mimes.push(name);
            }
        }
        let pw = meta.effective_pw();
        // A listing-only read never consults the grants, so it can't burn a
        // paste event's one-time password before the data read.
        let granted = !mimes.is_empty() && self.grants.use_grant(&pw, false);
        requests.push(KittyClipboardRequest::Read(KittyClipboardRead {
            location: meta.location,
            mimes,
            list,
            pw,
            granted,
            id: meta.id.clone(),
            terminator,
        }));
    }

    fn commit(
        &mut self,
        terminator: OscTerminator,
        replies: &mut Vec<u8>,
        requests: &mut Vec<KittyClipboardRequest>,
    ) {
        let Some(mut state) = self.write.take() else {
            return;
        };
        let id = state.id.clone();
        let Ok(contents) = state.commit() else {
            replies.extend(write_status(KittyClipboardStatus::Einval, &id, terminator));
            return;
        };
        let pw = if state.name.is_empty() {
            String::new()
        } else {
            state.pw
        };
        let granted = self.grants.use_grant(&pw, true);
        requests.push(KittyClipboardRequest::Write(KittyClipboardWrite {
            location: state.location,
            contents,
            pw,
            granted,
            id,
            terminator,
        }));
    }

    /// Fail and drop the open write (if any), replying with its `id`.
    fn finish_write(
        &mut self,
        status: KittyClipboardStatus,
        terminator: OscTerminator,
        replies: &mut Vec<u8>,
    ) {
        if let Some(state) = self.write.take() {
            replies.extend(write_status(status, &state.id, terminator));
        }
    }
}
