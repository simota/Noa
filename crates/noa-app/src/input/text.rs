/// Encode already-committed text for the pty.
pub(crate) fn encode_text(text: &str) -> Option<Vec<u8>> {
    if text.is_empty() {
        None
    } else {
        Some(text.as_bytes().to_vec())
    }
}
