//! Kitty clipboard protocol (OSC 5522) on the app side: the policy, the
//! system clipboard, the confirmation prompt and paste events (mode 5522),
//! as Ghostty 1.4.0's `Surface.zig` does them. The grid has already parsed
//! the packets, run the write transaction and checked the session grants.

use noa_grid::kitty_clipboard::{
    ClipboardLocation, KittyClipboardRead, KittyClipboardRequest, KittyClipboardStatus,
    KittyClipboardWrite, MAX_PASTE_EVENT_MIMES, paste_event,
};

use super::super::*;
use super::clipboard_confirm::CLIPBOARD_READ_ARM_DELAY;

impl App {
    pub(in crate::app) fn handle_kitty_clipboard(
        &mut self,
        window_id: WindowId,
        pane_id: PaneId,
        request: KittyClipboardRequest,
    ) {
        let Some(window_id) = self.resolve_pane_window(window_id, pane_id) else {
            return;
        };
        match request {
            KittyClipboardRequest::Read(read) => {
                self.kitty_clipboard_read(window_id, pane_id, read)
            }
            KittyClipboardRequest::Write(write) => {
                self.kitty_clipboard_write(window_id, pane_id, write)
            }
        }
    }

    fn kitty_clipboard_read(
        &mut self,
        window_id: WindowId,
        pane_id: PaneId,
        read: KittyClipboardRead,
    ) {
        let reply = |status| read.reply_status(status);
        if self.config.clipboard_read == noa_config::ClipboardAccess::Deny {
            self.send_kitty_reply(window_id, pane_id, vec![reply(KittyClipboardStatus::Eperm)]);
            return;
        }
        // The data is read before any prompt, so the user approves exactly
        // what is sent.
        let data = match self.clipboard.read_mimes(read.location, &read.mimes) {
            Ok(Some(data)) => data,
            Ok(None) => {
                self.send_kitty_reply(
                    window_id,
                    pane_id,
                    vec![reply(KittyClipboardStatus::Enosys)],
                );
                return;
            }
            Err(err) => {
                log::warn!("failed to read the clipboard for OSC 5522: {err}");
                self.send_kitty_reply(window_id, pane_id, vec![reply(KittyClipboardStatus::Eio)]);
                return;
            }
        };
        let listing = if read.list {
            match self.clipboard.available_mimes(read.location) {
                Ok(listing) => listing.unwrap_or_default(),
                Err(err) => {
                    log::warn!("failed to list the clipboard's types for OSC 5522: {err}");
                    self.send_kitty_reply(
                        window_id,
                        pane_id,
                        vec![reply(KittyClipboardStatus::Eio)],
                    );
                    return;
                }
            }
        } else {
            Vec::new()
        };
        // Nothing to serve and no listing is an empty read, never a prompt;
        // otherwise any request for data asks unless a grant covers it.
        let unavailable = !read.list && data.iter().all(Option::is_none);
        let prompt = !unavailable
            && !read.mimes.is_empty()
            && self.config.clipboard_read == noa_config::ClipboardAccess::Ask
            && !read.granted;
        if !prompt {
            self.write_kitty_read_reply(window_id, pane_id, &read, &listing, &data);
            return;
        }
        // Name what was asked for: like Ghostty, the prompt appears even when
        // none of it is on the clipboard (only the listing would go out). The
        // names are the program's, so they are quoted and escaped rather
        // than able to pass for the prompt's own words.
        let types = read
            .mimes
            .iter()
            .map(|mime| format!("{mime:?}"))
            .collect::<Vec<_>>()
            .join(", ");
        let remember = !read.pw.is_empty();
        self.prompt_kitty_clipboard(
            window_id,
            format!("A program in this pane wants to read the clipboard ({types}). Send it?"),
            remember,
            ConfirmAction::KittyClipboardRead {
                window_id,
                pane_id,
                read,
                listing,
                data,
            },
        );
    }

    fn kitty_clipboard_write(
        &mut self,
        window_id: WindowId,
        pane_id: PaneId,
        write: KittyClipboardWrite,
    ) {
        if self.config.clipboard_write == noa_config::ClipboardAccess::Deny {
            let bytes = write.reply_status(KittyClipboardStatus::Eperm);
            self.send_kitty_reply(window_id, pane_id, vec![bytes]);
            return;
        }
        if self.config.clipboard_write == noa_config::ClipboardAccess::Ask && !write.granted {
            if !self.clipboard.supports(write.location) {
                let bytes = write.reply_status(KittyClipboardStatus::Enosys);
                self.send_kitty_reply(window_id, pane_id, vec![bytes]);
                return;
            }
            let remember = !write.pw.is_empty();
            let message = format!(
                "A program in this pane wants to write to the clipboard: {}. Allow?",
                describe_write(&write.contents)
            );
            self.prompt_kitty_clipboard(
                window_id,
                message,
                remember,
                ConfirmAction::KittyClipboardWrite {
                    window_id,
                    pane_id,
                    write,
                },
            );
            return;
        }
        self.apply_kitty_clipboard_write(window_id, pane_id, &write);
    }

    fn apply_kitty_clipboard_write(
        &mut self,
        window_id: WindowId,
        pane_id: PaneId,
        write: &KittyClipboardWrite,
    ) {
        let status = match self.clipboard.write_mimes(write.location, &write.contents) {
            Ok(Some(())) => KittyClipboardStatus::Done,
            Ok(None) => KittyClipboardStatus::Enosys,
            Err(err) => {
                log::warn!("failed to write the clipboard for OSC 5522: {err}");
                KittyClipboardStatus::Eio
            }
        };
        self.send_kitty_reply(window_id, pane_id, vec![write.reply_status(status)]);
    }

    /// Send a successful read's replies; see [`Self::send_kitty_reply`].
    fn write_kitty_read_reply(
        &mut self,
        window_id: WindowId,
        pane_id: PaneId,
        read: &KittyClipboardRead,
        listing: &[String],
        data: &[Option<Vec<u8>>],
    ) {
        let packets = read.reply_packets(listing, data);
        let total: usize = packets.iter().map(Vec::len).sum();
        // A program that keeps asking without reading its replies would
        // otherwise grow the queue without bound: past the cap it gets EIO.
        if self.queued_kitty_reply_bytes(pane_id) + total > MAX_QUEUED_REPLY_BYTES {
            log::warn!("failing an OSC 5522 read: the pane has not read earlier replies");
            let eio = read.reply_status(KittyClipboardStatus::Eio);
            self.send_kitty_reply(window_id, pane_id, vec![eio]);
            return;
        }
        self.send_kitty_reply(window_id, pane_id, packets);
    }

    /// Send OSC 5522 reply packets (or a paste event) to the pane, in order
    /// with its earlier replies. A large reply (an image, say) would exceed the pane's
    /// pending-input budget as one write and be dropped whole, leaving the
    /// program waiting; a helper thread feeds it packet by packet instead,
    /// each waiting for budget to free up, and replies arriving meanwhile
    /// queue behind it.
    fn send_kitty_reply(&mut self, window_id: WindowId, pane_id: PaneId, packets: Vec<Vec<u8>>) {
        let total: usize = packets.iter().map(Vec::len).sum();
        let local = self
            .windows
            .get(&window_id)
            .and_then(|state| state.surfaces.get(&pane_id))
            .and_then(|surface| match &surface.transport {
                SurfaceTransport::Local(local) => {
                    Some((local.pty_input_tx.clone(), local.pty_writer.clone()))
                }
                SurfaceTransport::Remote(_) => None,
            });
        // Only panes with a reply still streaming need a queue.
        self.kitty_reply_queues
            .retain(|pane, queue| *pane == pane_id || queue.lock().draining);
        let queue = self.kitty_reply_queues.entry(pane_id).or_default().clone();
        let mut pending = queue.lock();
        if pending.draining {
            pending.queued_bytes += total;
            pending.packets.extend(packets);
            return;
        }
        let Some((input, writer)) = local else {
            drop(pending);
            self.write_pane_pty_bytes(window_id, pane_id, packets.concat());
            return;
        };
        // A small reply goes straight out when the budget has room; if it
        // doesn't, it waits in the queue like a large one rather than being
        // dropped.
        if total <= STREAMED_REPLY_BYTES {
            let bytes = packets.concat();
            if let Some(reserved) = input.reserve(bytes.clone().into_boxed_slice()) {
                if writer.write_reserved(reserved).is_err() {
                    log::warn!("failed to send an OSC 5522 reply: the pane is gone");
                }
                return;
            }
        }
        pending.queued_bytes += total;
        pending.packets.extend(packets);
        pending.draining = true;
        drop(pending);
        let drain = queue.clone();
        let spawned = std::thread::Builder::new()
            .name("noa-kitty-clipboard-reply".to_string())
            .spawn(move || drain_kitty_replies(&drain, &input, &writer));
        if let Err(err) = spawned {
            log::warn!("failed to start the OSC 5522 reply thread: {err}");
            let mut pending = queue.lock();
            pending.packets.clear();
            pending.queued_bytes = 0;
            pending.draining = false;
        }
    }

    /// Bytes of OSC 5522 replies already waiting for the pane.
    fn queued_kitty_reply_bytes(&self, pane_id: PaneId) -> usize {
        self.kitty_reply_queues
            .get(&pane_id)
            .map_or(0, |queue| queue.lock().queued_bytes)
    }

    /// Raise the confirmation for an OSC 5522 request. Like an OSC 52 read,
    /// terminal output raises it, so it never displaces a dialog the user is
    /// already answering (the request is denied) and it ignores keys for
    /// [`CLIPBOARD_READ_ARM_DELAY`].
    fn prompt_kitty_clipboard(
        &mut self,
        window_id: WindowId,
        message: String,
        remember: bool,
        action: ConfirmAction,
    ) {
        if self.confirm_dialog.is_some() {
            log::info!("denying OSC 5522 request: another confirmation is open");
            self.cancel_confirm_action(action);
            return;
        }
        let hint = if remember {
            "Enter: allow    A: allow and remember    Esc: deny"
        } else {
            "Enter: allow    Esc: deny"
        };
        self.open_confirm_dialog_with_hint(
            window_id,
            message,
            hint.to_string(),
            action,
            Some(Instant::now() + CLIPBOARD_READ_ARM_DELAY),
        );
    }

    /// Complete an approved OSC 5522 request. `remember` grants its password
    /// for the rest of the session (offered only when it carried one).
    pub(in crate::app) fn confirm_kitty_clipboard(
        &mut self,
        action: ConfirmAction,
        remember: bool,
    ) {
        match action {
            ConfirmAction::KittyClipboardRead {
                window_id,
                pane_id,
                read,
                listing,
                data,
            } => {
                if remember {
                    self.grant_kitty_clipboard(window_id, pane_id, &read.pw, true, false);
                }
                self.write_kitty_read_reply(window_id, pane_id, &read, &listing, &data);
            }
            ConfirmAction::KittyClipboardWrite {
                window_id,
                pane_id,
                write,
            } => {
                if remember {
                    self.grant_kitty_clipboard(window_id, pane_id, &write.pw, false, true);
                }
                self.apply_kitty_clipboard_write(window_id, pane_id, &write);
            }
            _ => {}
        }
    }

    /// Answer a denied or dismissed request: OSC 5522 replies `EPERM` to a
    /// pane that still exists; other dialogs need nothing.
    pub(in crate::app) fn cancel_confirm_action(&mut self, action: ConfirmAction) {
        let (pane_id, bytes) = match &action {
            ConfirmAction::KittyClipboardRead { pane_id, read, .. } => {
                (*pane_id, read.reply_status(KittyClipboardStatus::Eperm))
            }
            ConfirmAction::KittyClipboardWrite { pane_id, write, .. } => {
                (*pane_id, write.reply_status(KittyClipboardStatus::Eperm))
            }
            _ => return,
        };
        let window_id = match &action {
            ConfirmAction::KittyClipboardRead { window_id, .. }
            | ConfirmAction::KittyClipboardWrite { window_id, .. } => *window_id,
            _ => return,
        };
        if let Some(window_id) = self.resolve_pane_window(window_id, pane_id) {
            self.send_kitty_reply(window_id, pane_id, vec![bytes]);
        }
    }

    fn grant_kitty_clipboard(
        &self,
        window_id: WindowId,
        pane_id: PaneId,
        pw: &str,
        read: bool,
        write: bool,
    ) {
        if let Some(surface) = self
            .windows
            .get(&window_id)
            .and_then(|state| state.surfaces.get(&pane_id))
        {
            surface
                .terminal
                .lock()
                .grant_kitty_clipboard(pw, read, write, false);
        }
    }

    /// A user paste into a pane with Kitty paste events on (mode 5522):
    /// instead of the text, send the program the clipboard's MIME types and
    /// a one-time password to read them with. Returns `false` when the mode
    /// is off, so the caller pastes normally. No data is read, so this is
    /// sent even when `clipboard-read` is `deny`.
    pub(in crate::app) fn send_kitty_paste_event(
        &mut self,
        window_id: WindowId,
        pane_id: PaneId,
        location: ClipboardLocation,
    ) -> bool {
        let Some(surface) = self
            .windows
            .get(&window_id)
            .and_then(|state| state.surfaces.get(&pane_id))
        else {
            return false;
        };
        // A remote pane's program reads the clipboard through the remote
        // host's terminal, which never sees this password: paste normally.
        if matches!(surface.transport, SurfaceTransport::Remote(_)) {
            return false;
        }
        let terminal = surface.terminal.clone();
        if !terminal.lock().modes.kitty_paste_events() {
            return false;
        }
        let mimes = match self.clipboard.available_mimes(location) {
            Ok(Some(mimes)) => mimes,
            Ok(None) => return true,
            Err(err) => {
                log::warn!("failed to list the clipboard's types for a paste event: {err}");
                return true;
            }
        };
        let pw = match one_time_password() {
            Ok(pw) => pw,
            Err(err) => {
                log::warn!("no secure randomness for a paste event password: {err}");
                return true;
            }
        };
        // Grant before the program can see the password. Unlike Ghostty,
        // which revokes it if the write fails, a queued event's outcome is
        // not known here; an undelivered password is simply never used.
        terminal
            .lock()
            .grant_kitty_clipboard(&pw, true, false, true);
        let bytes = paste_event(
            &pw,
            location,
            &mimes[..mimes.len().min(MAX_PASTE_EVENT_MIMES)],
        );
        // Through the reply queue, so it can't land inside a read reply
        // that is still streaming.
        self.send_kitty_reply(window_id, pane_id, vec![bytes]);
        true
    }
}

/// Helper-thread body for [`App::send_kitty_reply`]: write queued packets
/// until the queue is empty, then hand back to direct writes.
fn drain_kitty_replies(
    queue: &Mutex<KittyReplyQueue>,
    input: &crate::io_thread::PtyInputQueue,
    writer: &noa_pty::PtyWriter,
) {
    loop {
        // `draining` stays set until a packet has been written and nothing
        // is left, so a reply sent meanwhile queues rather than overtakes.
        let Some(packet) = ({
            let mut pending = queue.lock();
            let next = pending.packets.pop_front();
            if let Some(packet) = &next {
                pending.queued_bytes -= packet.len();
            } else {
                pending.draining = false;
            }
            next
        }) else {
            return;
        };
        // A program that stops reading leaves the budget full; give up
        // rather than wait on it forever.
        let deadline = std::time::Instant::now() + REPLY_STALL_LIMIT;
        let reserved = loop {
            match input.reserve(packet.clone().into_boxed_slice()) {
                Some(reserved) => break Some(reserved),
                None if std::time::Instant::now() >= deadline => break None,
                None => std::thread::sleep(std::time::Duration::from_millis(5)),
            }
        };
        let written = reserved.is_some_and(|reserved| writer.write_reserved(reserved).is_ok());
        if !written {
            log::warn!("abandoning OSC 5522 replies: the pane stopped reading or closed");
            let mut pending = queue.lock();
            pending.packets.clear();
            pending.queued_bytes = 0;
            pending.draining = false;
            return;
        }
    }
}

/// Read replies above this go out packet by packet; see
/// [`App::send_kitty_reply`].
const STREAMED_REPLY_BYTES: usize = 1 << 20;
/// Most reply bytes queued for one pane; a read past it fails with EIO.
const MAX_QUEUED_REPLY_BYTES: usize = 64 * 1024 * 1024;
/// How long a streamed reply waits for the pane to accept its next packet.
const REPLY_STALL_LIMIT: std::time::Duration = std::time::Duration::from_secs(30);

/// What a write would put on the clipboard, for the prompt: its
/// `text/plain` (the representation a paste uses, as Ghostty's prompt shows),
/// or its types and sizes.
fn describe_write(contents: &[(String, std::sync::Arc<[u8]>)]) -> String {
    if let Some((_, text)) = contents.iter().find(|(mime, _)| mime == "text/plain") {
        let text = String::from_utf8_lossy(text);
        let mut preview: String = text.chars().take(80).collect();
        if preview.len() < text.len() {
            preview.push('…');
        }
        return format!("{preview:?}");
    }
    if contents.is_empty() {
        return "clear it".to_string();
    }
    contents
        .iter()
        .map(|(mime, data)| format!("{mime:?} ({} bytes)", data.len()))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Ghostty's paste event password: 22 characters from an alphabet without
/// look-alikes, drawn from the OS's secure randomness (never a weaker
/// fallback).
fn one_time_password() -> std::io::Result<String> {
    use std::io::Read;
    const ALPHABET: &[u8] = b"23456789abcdefghijkmnopqrstuvwxyzABCDEFGHJKLMNPQRSTUVWXYZ";
    // Rejection sampling keeps every character equally likely.
    let limit = 256 - 256 % ALPHABET.len();
    let mut random = std::fs::File::open("/dev/urandom")?;
    let mut pw = String::with_capacity(22);
    let mut byte = [0u8; 1];
    while pw.len() < 22 {
        random.read_exact(&mut byte)?;
        if usize::from(byte[0]) < limit {
            pw.push(char::from(ALPHABET[usize::from(byte[0]) % ALPHABET.len()]));
        }
    }
    Ok(pw)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_time_passwords_use_ghosttys_alphabet() {
        let a = one_time_password().unwrap();
        let b = one_time_password().unwrap();
        assert_eq!(a.len(), 22);
        assert_ne!(a, b);
        assert!(
            a.chars()
                .all(|c| c.is_ascii_alphanumeric() && !"01lIO".contains(c))
        );
    }

    #[test]
    fn a_write_is_described_by_its_text_or_types() {
        let png = ("image/png".to_string(), std::sync::Arc::from(&[0u8; 3][..]));
        let alias = ("TEXT".to_string(), std::sync::Arc::from(&b"other"[..]));
        let text = ("text/plain".to_string(), std::sync::Arc::from(&b"hi\n"[..]));
        assert_eq!(describe_write(&[png.clone(), alias, text]), "\"hi\\n\"");
        assert_eq!(describe_write(&[png]), "\"image/png\" (3 bytes)");
        // A program-chosen name can't break out of its quotes.
        let sneaky = (
            "a\u{202e}\". OK".to_string(),
            std::sync::Arc::from(&[0u8][..]),
        );
        assert_eq!(
            describe_write(&[sneaky]),
            "\"a\\u{202e}\\\". OK\" (1 bytes)"
        );
        assert_eq!(describe_write(&[]), "clear it");
    }
}
