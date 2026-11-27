//! The only live authority is the pair of pipes inherited from Noa. The
//! renderer neither discovers an IPC endpoint nor opens a token file.

use std::io::{BufRead, BufReader, Read, Stdin, Write};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, SyncSender};

use noa_ipc::widget::{
    ASSET_CHUNK_BYTES, HostMessage, HostRequest, MAX_BRIDGE_LINE_BYTES, base64_decode,
};
use noa_widget::Asset;
use noa_widget::assets::{MAX_ASSET_BYTES, MAX_ASSETS, MAX_PACKAGE_ASSET_BYTES};

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
pub fn bootstrap() -> Result<(HostMessage, Vec<Asset>, BufReader<Stdin>), String> {
    write_request(&mut std::io::stdout().lock(), &HostRequest::Ready)?;
    let mut reader = BufReader::new(std::io::stdin());
    let (snapshot, assets) = read_initial(&mut reader)?;
    Ok((snapshot, assets, reader))
}

/// Packaged images precede the initial snapshot as asset frames. Each asset
/// is checked against its digest here; the snapshot's definition digest then
/// pins the complete set (checked by the screen).
pub fn read_initial(reader: &mut impl BufRead) -> Result<(HostMessage, Vec<Asset>), String> {
    let mut frames = AssetFrames::default();
    loop {
        match read_message(reader)? {
            Some(snapshot @ HostMessage::Snapshot { .. }) => {
                return Ok((snapshot, frames.finish()?));
            }
            Some(asset @ HostMessage::Asset { .. }) => frames.push(asset)?,
            _ => return Err("widget bridge closed before its initial snapshot".into()),
        }
    }
}

/// Reassembles asset frames in order, within the package's limits.
#[derive(Default)]
struct AssetFrames {
    assets: Vec<Asset>,
    partial: Option<Partial>,
    total: usize,
}

struct Partial {
    id: String,
    digest: String,
    count: u32,
    next: u32,
    bytes: Vec<u8>,
}

impl AssetFrames {
    fn push(&mut self, message: HostMessage) -> Result<(), String> {
        let HostMessage::Asset {
            id,
            digest,
            index,
            count,
            data,
        } = message
        else {
            return Err("expected an asset frame".into());
        };
        let partial = match self.partial.take() {
            Some(partial) => partial,
            None => {
                let max_chunks = MAX_ASSET_BYTES.div_ceil(ASSET_CHUNK_BYTES) as u32;
                if index != 0 || count == 0 || count > max_chunks {
                    return Err("asset frames are out of order".into());
                }
                if self.assets.len() >= MAX_ASSETS || self.assets.iter().any(|a| a.id == id) {
                    return Err("too many or duplicate assets".into());
                }
                Partial {
                    id: id.clone(),
                    digest: digest.clone(),
                    count,
                    next: 0,
                    bytes: Vec::new(),
                }
            }
        };
        if partial.id != id || partial.digest != digest || partial.count != count {
            return Err("asset frames are interleaved".into());
        }
        if index != partial.next {
            return Err("asset frames are out of order".into());
        }
        let chunk = base64_decode(&data)?;
        let mut partial = partial;
        self.total += chunk.len();
        if partial.bytes.len() + chunk.len() > MAX_ASSET_BYTES
            || self.total > MAX_PACKAGE_ASSET_BYTES
        {
            return Err("assets exceed their size limit".into());
        }
        partial.bytes.extend(chunk);
        partial.next += 1;
        if partial.next == partial.count {
            let asset = Asset::verify(&partial.id, partial.bytes, Some(&partial.digest))
                .map_err(|error| error.to_string())?;
            self.assets.push(asset);
        } else {
            self.partial = Some(partial);
        }
        Ok(())
    }

    fn finish(self) -> Result<Vec<Asset>, String> {
        if self.partial.is_some() {
            return Err("an asset was incomplete before the snapshot".into());
        }
        Ok(self.assets)
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

    fn lines(messages: &[HostMessage]) -> Cursor<Vec<u8>> {
        let mut bytes = Vec::new();
        for message in messages {
            bytes.extend(serde_json::to_vec(message).unwrap());
            bytes.push(b'\n');
        }
        Cursor::new(bytes)
    }

    fn snapshot() -> HostMessage {
        HostMessage::Snapshot {
            instance_id: "wi_0123456789abcdef0123456789abcdef".into(),
            definition_digest: "sha256:00".into(),
            data_revision: 1,
            manifest: serde_json::json!({}),
            data: serde_json::json!({}),
        }
    }

    #[test]
    fn asset_frames_are_reassembled_and_checked_against_their_digest() {
        let logo = include_bytes!("../../../.noa-widgets/image-gallery/assets/logo.png");
        let digest = noa_widget::content_digest(logo);
        let mut frames: Vec<HostMessage> = logo
            .chunks(500)
            .enumerate()
            .map(|(index, chunk)| HostMessage::Asset {
                id: "logo".into(),
                digest: digest.clone(),
                index: index as u32,
                count: logo.len().div_ceil(500) as u32,
                data: noa_ipc::widget::base64_encode(chunk),
            })
            .collect();
        frames.push(snapshot());
        let (_, assets) = read_initial(&mut lines(&frames)).unwrap();
        assert_eq!(assets.len(), 1);
        assert_eq!(assets[0].bytes, logo);
        assert_eq!((assets[0].width, assets[0].height), (96, 96));

        let mut swapped = frames.clone();
        swapped.swap(0, 1);
        assert!(read_initial(&mut lines(&swapped)).is_err());
        let truncated = [frames[0].clone(), snapshot()];
        assert!(read_initial(&mut lines(&truncated)).is_err());
        let tampered: Vec<HostMessage> =
            noa_ipc::widget::asset_frames("logo", &noa_widget::content_digest(b"other"), logo)
                .into_iter()
                .chain([snapshot()])
                .collect();
        assert!(
            read_initial(&mut lines(&tampered))
                .unwrap_err()
                .contains("digest")
        );
        let mut duplicate = noa_ipc::widget::asset_frames("logo", &digest, logo);
        duplicate.extend(noa_ipc::widget::asset_frames("logo", &digest, logo));
        duplicate.push(snapshot());
        assert!(read_initial(&mut lines(&duplicate)).is_err());
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
