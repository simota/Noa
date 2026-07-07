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
