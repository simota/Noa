//! The only live authority is the pair of pipes inherited from Noa. The
//! renderer neither discovers an IPC endpoint nor opens a token file.

use std::io::{BufRead, BufReader, Read, Stdin, Write};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, SyncSender};

use noa_ipc::widget::{HostMessage, HostRequest, MAX_BRIDGE_LINE_BYTES};

pub struct Bridge {
    incoming: Receiver<Update>,
    outgoing: SyncSender<HostRequest>,
}

pub enum Update {
    Message(HostMessage),
    Failed(String),
}

pub fn read_message(reader: &mut impl BufRead) -> Result<Option<HostMessage>, String> {
    let mut bytes = Vec::new();
    let count = reader
        .take(MAX_BRIDGE_LINE_BYTES as u64 + 1)
        .read_until(b'\n', &mut bytes)
        .map_err(|error| format!("widget bridge read: {error}"))?;
    if count == 0 {
        return Ok(None);
    }
    if count > MAX_BRIDGE_LINE_BYTES || bytes.last() != Some(&b'\n') {
        return Err("widget bridge frame is too large or missing its newline".into());
    }
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|error| format!("invalid widget bridge message: {error}"))
}

fn write_request(writer: &mut impl Write, request: &HostRequest) -> Result<(), String> {
    let mut bytes = serde_json::to_vec(request).map_err(|error| error.to_string())?;
    bytes.push(b'\n');
    if bytes.len() > MAX_BRIDGE_LINE_BYTES {
        return Err("widget bridge request exceeds its size limit".into());
    }
    writer
        .write_all(&bytes)
        .and_then(|()| writer.flush())
        .map_err(|error| format!("widget bridge write: {error}"))
}

/// Startup is before creating a window; only the pinned snapshot can set
/// the live window's definition. Ready never carries filesystem paths.
pub fn bootstrap() -> Result<(HostMessage, BufReader<Stdin>), String> {
    write_request(&mut std::io::stdout().lock(), &HostRequest::Ready)?;
    let mut reader = BufReader::new(std::io::stdin());
    match read_message(&mut reader)? {
        Some(snapshot @ HostMessage::Snapshot { .. }) => Ok((snapshot, reader)),
        _ => Err("widget bridge closed before its initial snapshot".into()),
    }
}

impl Bridge {
    #[cfg(test)]
    pub(crate) fn recording() -> (Self, Receiver<HostRequest>) {
        let (_incoming_tx, incoming) = mpsc::sync_channel(16);
        let (outgoing, sent) = mpsc::sync_channel(16);
        (Self { incoming, outgoing }, sent)
    }

    pub fn start(reader: BufReader<Stdin>, wake: impl Fn() + Send + Sync + 'static) -> Self {
        let (incoming_tx, incoming) = mpsc::sync_channel(16);
        let (outgoing, outgoing_rx) = mpsc::sync_channel(16);
        let wake = Arc::new(wake);
        let reader_wake = wake.clone();
        let reader_tx = incoming_tx.clone();
        std::thread::spawn(move || {
            let mut reader = reader;
            loop {
                match read_message(&mut reader) {
                    Ok(Some(HostMessage::Closed)) | Ok(None) => {
                        let _ = reader_tx.send(Update::Message(HostMessage::Closed));
                        reader_wake();
                        return;
                    }
                    Ok(Some(message)) => {
                        if reader_tx.send(Update::Message(message)).is_err() {
                            return;
                        }
                        reader_wake();
                    }
                    Err(error) => {
                        let _ = reader_tx.send(Update::Failed(error));
                        reader_wake();
                        return;
                    }
                }
            }
        });
        std::thread::spawn(move || {
            let stdout = std::io::stdout();
            while let Ok(request) = outgoing_rx.recv() {
                if let Err(error) = write_request(&mut stdout.lock(), &request) {
                    let _ = incoming_tx.send(Update::Failed(error));
                    wake();
                    return;
                }
            }
        });
        Self { incoming, outgoing }
    }

    pub fn send(&self, request: HostRequest) -> Result<(), String> {
        self.outgoing
            .try_send(request)
            .map_err(|error| format!("widget bridge cannot accept another action: {error}"))
    }

    pub fn drain(&self) -> Vec<Update> {
        self.incoming.try_iter().collect()
    }

    pub fn closed(&self) {
        // EOF also closes the instance. Closing a window never waits on a
        // parent which may have stopped reading its pipe.
        let _ = self.send(HostRequest::Closed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn bridge_frames_are_bounded_and_incomplete_frames_are_refused() {
        let mut reader = Cursor::new(b"{\"type\":\"closed\"}\n");
        assert!(matches!(
            read_message(&mut reader).unwrap(),
            Some(HostMessage::Closed)
        ));
        assert!(read_message(&mut reader).unwrap().is_none());
        assert!(read_message(&mut Cursor::new(b"{\"type\":\"closed\"}")).is_err());
        let bytes = vec![b' '; MAX_BRIDGE_LINE_BYTES + 2];
        let mut reader = Cursor::new(bytes);
        assert!(read_message(&mut reader).is_err());
        assert_eq!(reader.position(), MAX_BRIDGE_LINE_BYTES as u64 + 1);
    }

    #[test]
    fn requests_have_no_authority_to_select_another_instance() {
        let request = HostRequest::Action {
            request_id: "click-1".into(),
            action_id: "rerun".into(),
            data_revision: 7,
            payload: serde_json::json!({}),
        };
        let mut writer = Vec::new();
        write_request(&mut writer, &request).unwrap();
        assert_eq!(writer.last(), Some(&b'\n'));
        let value: serde_json::Value = serde_json::from_slice(&writer).unwrap();
        assert!(value.get("instanceId").is_none());
        assert_eq!(value["dataRevision"], 7);
    }
}
