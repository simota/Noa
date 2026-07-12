/// Encode pasted text for the pty. When DECSET 2004 is active, shells and
/// editors receive explicit paste boundaries and can avoid executing content
/// as if it were typed interactively.
pub fn encode_paste(text: &str, bracketed_paste: bool) -> Option<Vec<u8>> {
    if text.is_empty() {
        return None;
    }

    let payload = sanitize_paste_payload(text.as_bytes());
    if payload.is_empty() {
        return None;
    }
    if bracketed_paste {
        let mut bytes = Vec::with_capacity(payload.len() + b"\x1b[200~".len() + b"\x1b[201~".len());
        bytes.extend_from_slice(b"\x1b[200~");
        bytes.extend_from_slice(&payload);
        bytes.extend_from_slice(b"\x1b[201~");
        Some(bytes)
    } else {
        Some(payload)
    }
}

/// Largest `input text` payload accepted from AppleScript in one Apple Event
/// (applescript Amendment 1.5). Sized to the terminal's OSC 52 clipboard cap
/// (8 MiB decoded) so a scripted write can never queue more than an equivalent
/// clipboard paste; anything longer is truncated on a UTF-8 boundary.
pub(crate) const APPLESCRIPT_INPUT_TEXT_CAP: usize = 8 * 1024 * 1024;

/// Encode AppleScript `input text` for the pty (applescript R-7/AC-8). It
/// travels the exact same path as a clipboard paste — bracketed when DECSET
/// 2004 is active, raw otherwise — after first capping the payload to
/// [`APPLESCRIPT_INPUT_TEXT_CAP`] on a UTF-8 boundary. Pure and unit-tested so
/// the byte-level contract can be verified without an Apple Event.
pub(crate) fn applescript_input_bytes(text: &str, bracketed_paste: bool) -> Option<Vec<u8>> {
    encode_paste(cap_input_text(text), bracketed_paste)
}

/// Encode `noa.sendText`'s `paste: false` payload for the pty (noa-server
/// sendText paste param): the UTF-8 bytes of `text` written as-is, bypassing
/// `encode_paste` entirely — no bracketed-paste wrap, no stripping of
/// embedded `ESC[200~`/`ESC[201~` markers. This is keyboard-like injection
/// (e.g. a lone "\r" acts as Enter for the running app), not a paste, so it
/// gets none of paste's framing or sanitization. Still capped to
/// [`APPLESCRIPT_INPUT_TEXT_CAP`] on a UTF-8 boundary, matching the paste
/// path's bound on how much one RPC call can queue to the pty.
pub(crate) fn raw_input_bytes(text: &str) -> Option<Vec<u8>> {
    let capped = cap_input_text(text);
    if capped.is_empty() {
        None
    } else {
        Some(capped.as_bytes().to_vec())
    }
}

fn cap_input_text(text: &str) -> &str {
    if text.len() > APPLESCRIPT_INPUT_TEXT_CAP {
        let mut end = APPLESCRIPT_INPUT_TEXT_CAP;
        while end > 0 && !text.is_char_boundary(end) {
            end -= 1;
        }
        &text[..end]
    } else {
        text
    }
}

/// Whether a paste should be confirmed before being sent
/// (`clipboard-paste-protection`). A newline can submit a command line on its
/// own, so unbracketed multi-line pastes are the risk. In bracketed-paste
/// mode the receiving program frames the paste itself, so it is treated as
/// safe (Ghostty's `clipboard-paste-bracketed-safe`, on by default).
pub(crate) fn paste_is_unsafe(text: &str, bracketed_paste: bool) -> bool {
    !bracketed_paste && text.contains(['\n', '\r'])
}

fn sanitize_paste_payload(bytes: &[u8]) -> Vec<u8> {
    let mut sanitized = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i..].starts_with(b"\x1b[200~") {
            i += b"\x1b[200~".len();
        } else if bytes[i..].starts_with(b"\x1b[201~") {
            i += b"\x1b[201~".len();
        } else {
            sanitized.push(bytes[i]);
            i += 1;
        }
    }
    sanitized
}
