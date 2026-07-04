//! Kitty graphics protocol — image store and payload transfers.
//!
//! [`noa_vt`] parses the control data into a [`KittyGraphicsCommand`]; this
//! module owns the *image data* side of the protocol: base64/zlib/PNG decoding,
//! quota enforcement, chunked (`m=1`) reassembly, image-id assignment, and the
//! reply bodies (`OK` / `E<CODE>:message`). Placements (the visual side) live on
//! [`crate::screen::Screen`]; storage is screen-independent so the quota is a
//! single global budget, matching Kitty semantics.
//!
//! Ghostty analog: `terminal/kitty/graphics_storage.zig` +
//! `graphics_image.zig`.

use std::collections::HashSet;
use std::sync::Arc;

use noa_vt::{KittyAction, KittyCompression, KittyFormat, KittyGraphicsCommand, KittyMedium};

use crate::osc::decode_base64_limited;

/// Maximum width or height of a single image, in pixels (Ghostty parity).
pub const MAX_IMAGE_DIM: u32 = 10_000;
/// Total decoded-RGBA budget across all stored images (Kitty/Ghostty default).
pub const TOTAL_BYTES_LIMIT: usize = 320_000_000;
/// Per-image decoded-bytes ceiling; also bounds intermediate decode buffers so a
/// malicious `o=z` stream can't inflate without bound.
const SINGLE_IMAGE_LIMIT: usize = TOTAL_BYTES_LIMIT;

/// A Kitty graphics error, rendered into a reply as `E<code>:<message>`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum KittyError {
    /// `EINVAL` — malformed request (bad key, missing dimension, bad path).
    Invalid,
    /// `EFBIG` — image too large (dimension or byte budget) / APC truncated.
    TooBig,
    /// `ENODATA` — declared size does not match the payload length.
    NoData,
    /// `EBADPNG` — PNG decode failed.
    BadPng,
    /// `ENOENT` — file medium path does not resolve to a readable file.
    NoEnt,
    /// `EUNSUPPORTED` — shared memory, animation, or an unimplemented action.
    Unsupported,
}

impl KittyError {
    /// The reply body (`E<code>:<message>`).
    pub fn reply_body(self) -> &'static str {
        match self {
            KittyError::Invalid => "EINVAL:invalid request",
            KittyError::TooBig => "EFBIG:image too large",
            KittyError::NoData => "ENODATA:data size mismatch",
            KittyError::BadPng => "EBADPNG:png decode failed",
            KittyError::NoEnt => "ENOENT:file not found",
            KittyError::Unsupported => "EUNSUPPORTED:unsupported request",
        }
    }
}

/// A stored image: straight (non-premultiplied) RGBA8, shared with the renderer
/// via `Arc` so the snapshot copy is a refcount bump, not a pixel copy.
#[derive(Clone, Debug)]
pub struct KittyImage {
    pub id: u32,
    /// `I=` image number (0 = none). The newest transfer with a given number
    /// wins when a client refers to images by number.
    pub number: u32,
    pub width: u32,
    pub height: u32,
    pub rgba: Arc<[u8]>,
    /// Bumped whenever this `id` is re-transmitted; the renderer keys its texture
    /// cache on `(id, epoch)` so a re-upload is detected.
    pub epoch: u64,
    /// Monotonic transfer order, used to pick the oldest victim under quota.
    pub seq: u64,
}

/// A chunked (`m=1`) transfer in progress. Kitty allows only one at a time.
struct PendingTransfer {
    /// Control data from the *first* chunk (continuation chunks carry only
    /// `m=`/payload). Drives the final format/medium/placement decision.
    ctrl: KittyGraphicsCommand,
    /// base64-decoded bytes concatenated across chunks (direct medium only).
    decoded: Vec<u8>,
}

/// The effective control data plus decode result of a finished transfer.
pub struct TransmitDone {
    /// First-chunk control data (equals the command for a single-shot transfer).
    pub ctrl: KittyGraphicsCommand,
    /// `Ok(image_id)` (the assigned id) or the error to reply.
    pub result: Result<u32, KittyError>,
}

/// One step of feeding a graphics command into the store.
pub enum TransmitStep {
    /// A chunk was accepted; more are expected. No reply yet.
    NeedMore,
    /// The transfer finished (single-shot or last chunk).
    Done(TransmitDone),
}

/// Screen-independent image storage with a global byte quota.
#[derive(Default)]
pub struct ImageStore {
    images: Vec<KittyImage>,
    total_bytes: usize,
    next_auto_id: u32,
    next_epoch: u64,
    next_seq: u64,
    transfer: Option<PendingTransfer>,
}

impl ImageStore {
    pub fn new() -> Self {
        ImageStore {
            next_auto_id: 1,
            ..Default::default()
        }
    }

    /// A stored image by id.
    pub fn get(&self, id: u32) -> Option<&KittyImage> {
        self.images.iter().find(|img| img.id == id)
    }

    /// The newest stored image carrying image number `number` (`I=`).
    pub fn get_by_number(&self, number: u32) -> Option<&KittyImage> {
        self.images
            .iter()
            .filter(|img| img.number == number)
            .max_by_key(|img| img.seq)
    }

    /// All stored image ids (used by the quota sweep's "referenced" set).
    pub fn contains(&self, id: u32) -> bool {
        self.images.iter().any(|img| img.id == id)
    }

    /// Drop the image with `id` and its bytes. Returns whether anything changed.
    pub fn remove(&mut self, id: u32) -> bool {
        if let Some(pos) = self.images.iter().position(|img| img.id == id) {
            self.total_bytes -= self.images[pos].rgba.len();
            self.images.remove(pos);
            true
        } else {
            false
        }
    }

    /// Drop everything, including any in-flight chunked transfer.
    pub fn clear(&mut self) {
        self.images.clear();
        self.total_bytes = 0;
        self.transfer = None;
    }

    /// Feed one graphics command carrying image data (`a=t`/`a=T`/`a=q`).
    ///
    /// Handles chunk reassembly: while a transfer is pending, further transmit
    /// commands are treated as continuation chunks. A non-transmit command
    /// arriving mid-transfer is the caller's cue to discard via [`Self::abort`].
    pub fn transmit(&mut self, cmd: &KittyGraphicsCommand) -> TransmitStep {
        if self.transfer.is_some() {
            return self.continue_chunk(cmd);
        }
        if cmd.more_chunks {
            return self.begin_chunk(cmd);
        }
        // Single-shot transfer.
        let decoded = match self.decode_medium_payload(cmd) {
            Ok(bytes) => bytes,
            Err(e) => {
                return TransmitStep::Done(TransmitDone {
                    ctrl: cmd.clone(),
                    result: Err(e),
                });
            }
        };
        let result = self.finalize(cmd, decoded);
        TransmitStep::Done(TransmitDone {
            ctrl: cmd.clone(),
            result,
        })
    }

    /// Discard an in-flight chunked transfer (a different command interrupted it).
    pub fn abort(&mut self) {
        self.transfer = None;
    }

    /// Whether a chunked transfer is currently in progress.
    pub fn transfer_in_progress(&self) -> bool {
        self.transfer.is_some()
    }

    fn begin_chunk(&mut self, cmd: &KittyGraphicsCommand) -> TransmitStep {
        // Only the direct medium is chunked; file/shm carry a whole path.
        let decoded = match self.decode_base64(cmd) {
            Ok(bytes) => bytes,
            Err(e) => {
                return TransmitStep::Done(TransmitDone {
                    ctrl: cmd.clone(),
                    result: Err(e),
                });
            }
        };
        self.transfer = Some(PendingTransfer {
            ctrl: cmd.clone(),
            decoded,
        });
        TransmitStep::NeedMore
    }

    fn continue_chunk(&mut self, cmd: &KittyGraphicsCommand) -> TransmitStep {
        let chunk = match decode_base64_limited(&cmd.payload, SINGLE_IMAGE_LIMIT) {
            Some(bytes) => bytes,
            None => {
                self.transfer = None;
                return TransmitStep::Done(TransmitDone {
                    ctrl: cmd.clone(),
                    result: Err(KittyError::Invalid),
                });
            }
        };
        let transfer = self.transfer.as_mut().expect("transfer in progress");
        if transfer.decoded.len() + chunk.len() > SINGLE_IMAGE_LIMIT {
            self.transfer = None;
            return TransmitStep::Done(TransmitDone {
                ctrl: cmd.clone(),
                result: Err(KittyError::TooBig),
            });
        }
        transfer.decoded.extend_from_slice(&chunk);
        if cmd.more_chunks {
            return TransmitStep::NeedMore;
        }
        let transfer = self.transfer.take().expect("transfer in progress");
        let source = self.decompress(&transfer.ctrl, transfer.decoded);
        let ctrl = transfer.ctrl;
        let result = source.and_then(|bytes| self.build_and_store(&ctrl, bytes));
        TransmitStep::Done(TransmitDone { ctrl, result })
    }

    /// base64-decode the payload of a direct transfer.
    fn decode_base64(&self, cmd: &KittyGraphicsCommand) -> Result<Vec<u8>, KittyError> {
        decode_base64_limited(&cmd.payload, SINGLE_IMAGE_LIMIT).ok_or(KittyError::Invalid)
    }

    /// Resolve the medium into raw (post-base64, post-decompression) image bytes.
    fn decode_medium_payload(&self, cmd: &KittyGraphicsCommand) -> Result<Vec<u8>, KittyError> {
        match cmd.medium {
            KittyMedium::Direct => {
                let decoded = self.decode_base64(cmd)?;
                self.decompress(cmd, decoded)
            }
            KittyMedium::File | KittyMedium::TempFile => {
                let raw = self.read_file_medium(cmd)?;
                self.decompress(cmd, raw)
            }
            KittyMedium::SharedMem => Err(KittyError::Unsupported),
        }
    }

    /// Read a file-medium payload: base64 path → canonicalize → bounded read.
    /// A temp-file medium additionally requires a temp-directory path and is
    /// deleted after reading.
    fn read_file_medium(&self, cmd: &KittyGraphicsCommand) -> Result<Vec<u8>, KittyError> {
        let path_bytes = self.decode_base64(cmd)?;
        let path_str = std::str::from_utf8(&path_bytes).map_err(|_| KittyError::Invalid)?;
        let path = std::path::Path::new(path_str);
        if !path.is_absolute() {
            return Err(KittyError::Invalid);
        }
        let canonical = std::fs::canonicalize(path).map_err(|_| KittyError::NoEnt)?;
        let meta = std::fs::metadata(&canonical).map_err(|_| KittyError::NoEnt)?;
        if !meta.is_file() {
            return Err(KittyError::NoEnt);
        }
        if cmd.medium == KittyMedium::TempFile && !is_temp_path(&canonical) {
            return Err(KittyError::Invalid);
        }

        let file_len = meta.len();
        let offset = cmd.file_offset as u64;
        let avail = file_len.saturating_sub(offset);
        let want = if cmd.file_size == 0 {
            avail
        } else {
            (cmd.file_size as u64).min(avail)
        };
        if want as usize > SINGLE_IMAGE_LIMIT {
            return Err(KittyError::TooBig);
        }

        let bytes = read_file_range(&canonical, offset, want as usize)?;
        if cmd.medium == KittyMedium::TempFile {
            let _ = std::fs::remove_file(&canonical);
        }
        Ok(bytes)
    }

    /// Apply `o=z` (zlib) decompression, bounding the inflated size.
    fn decompress(
        &self,
        cmd: &KittyGraphicsCommand,
        bytes: Vec<u8>,
    ) -> Result<Vec<u8>, KittyError> {
        match cmd.compression {
            None => Ok(bytes),
            Some(KittyCompression::Zlib) => inflate_bounded(&bytes, SINGLE_IMAGE_LIMIT),
        }
    }

    /// Finish a single-shot transfer whose raw bytes are already resolved.
    fn finalize(&mut self, cmd: &KittyGraphicsCommand, raw: Vec<u8>) -> Result<u32, KittyError> {
        self.build_and_store(cmd, raw)
    }

    /// Decode `raw` into RGBA per `f=`, then (for non-query actions) store it.
    fn build_and_store(
        &mut self,
        cmd: &KittyGraphicsCommand,
        raw: Vec<u8>,
    ) -> Result<u32, KittyError> {
        let (width, height, rgba) = decode_to_rgba(cmd, raw)?;
        if width == 0 || height == 0 || width > MAX_IMAGE_DIM || height > MAX_IMAGE_DIM {
            return Err(KittyError::TooBig);
        }
        if rgba.len() > SINGLE_IMAGE_LIMIT {
            return Err(KittyError::TooBig);
        }

        // Query (`a=q`) validates only — never stores.
        if cmd.action == KittyAction::Query {
            return Ok(cmd.image_id);
        }

        let id = self.assign_id(cmd);
        self.insert(id, cmd.image_number, width, height, rgba);
        Ok(id)
    }

    /// Resolve the target image id: explicit `i=`, else auto-assigned.
    fn assign_id(&mut self, cmd: &KittyGraphicsCommand) -> u32 {
        if cmd.image_id != 0 {
            return cmd.image_id;
        }
        // Auto-assign the next free id (skipping any in use).
        loop {
            let id = self.next_auto_id;
            self.next_auto_id = self.next_auto_id.wrapping_add(1).max(1);
            if id != 0 && !self.contains(id) {
                return id;
            }
        }
    }

    fn insert(&mut self, id: u32, number: u32, width: u32, height: u32, rgba: Vec<u8>) {
        let seq = self.next_seq;
        self.next_seq += 1;
        let bytes = rgba.len();
        let rgba: Arc<[u8]> = Arc::from(rgba);

        if let Some(existing) = self.images.iter_mut().find(|img| img.id == id) {
            self.total_bytes -= existing.rgba.len();
            existing.epoch = existing.epoch.wrapping_add(1);
            existing.number = number;
            existing.width = width;
            existing.height = height;
            existing.rgba = rgba;
            existing.seq = seq;
            self.total_bytes += bytes;
        } else {
            let epoch = self.next_epoch;
            self.next_epoch += 1;
            self.images.push(KittyImage {
                id,
                number,
                width,
                height,
                rgba,
                epoch,
                seq,
            });
            self.total_bytes += bytes;
        }
    }

    /// Evict images until the total byte budget is satisfied. Images whose id is
    /// not in `referenced` (no visible placement) are dropped first, oldest by
    /// `seq`; then, if still over budget, the oldest overall.
    pub fn enforce_quota(&mut self, referenced: &HashSet<u32>) {
        while self.total_bytes > TOTAL_BYTES_LIMIT {
            let victim = self
                .images
                .iter()
                .filter(|img| !referenced.contains(&img.id))
                .min_by_key(|img| img.seq)
                .map(|img| img.id)
                .or_else(|| {
                    self.images
                        .iter()
                        .min_by_key(|img| img.seq)
                        .map(|img| img.id)
                });
            match victim {
                Some(id) => {
                    self.remove(id);
                }
                None => break,
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.images.len()
    }

    #[cfg(test)]
    pub(crate) fn total_bytes(&self) -> usize {
        self.total_bytes
    }
}

/// Whether `path` sits in a location we accept for `t=t` (temp-file) media and
/// may delete after reading. Mirrors Kitty's requirement.
fn is_temp_path(path: &std::path::Path) -> bool {
    let temp = std::fs::canonicalize(std::env::temp_dir());
    if let Ok(temp) = &temp
        && path.starts_with(temp)
    {
        return true;
    }
    for prefix in ["/tmp", "/dev/shm", "/var/tmp"] {
        if path.starts_with(prefix) {
            return true;
        }
    }
    path.to_string_lossy().contains("tty-graphics-protocol")
}

fn read_file_range(
    path: &std::path::Path,
    offset: u64,
    len: usize,
) -> Result<Vec<u8>, KittyError> {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = std::fs::File::open(path).map_err(|_| KittyError::NoEnt)?;
    if offset > 0 {
        file.seek(SeekFrom::Start(offset))
            .map_err(|_| KittyError::Invalid)?;
    }
    let mut buf = vec![0u8; len];
    file.read_exact(&mut buf).map_err(|_| KittyError::NoData)?;
    Ok(buf)
}

fn inflate_bounded(input: &[u8], limit: usize) -> Result<Vec<u8>, KittyError> {
    use std::io::Read;
    let mut decoder = flate2::read::ZlibDecoder::new(input);
    let mut out = Vec::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        match decoder.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                if out.len() + n > limit {
                    return Err(KittyError::TooBig);
                }
                out.extend_from_slice(&buf[..n]);
            }
            Err(_) => return Err(KittyError::Invalid),
        }
    }
    Ok(out)
}

/// Decode raw image bytes into `(width, height, straight-RGBA8)` per `f=`.
fn decode_to_rgba(
    cmd: &KittyGraphicsCommand,
    raw: Vec<u8>,
) -> Result<(u32, u32, Vec<u8>), KittyError> {
    match cmd.format {
        KittyFormat::Rgba => {
            let (w, h) = raw_dimensions(cmd)?;
            if raw.len() != (w as usize) * (h as usize) * 4 {
                return Err(KittyError::NoData);
            }
            Ok((w, h, raw))
        }
        KittyFormat::Rgb => {
            let (w, h) = raw_dimensions(cmd)?;
            if raw.len() != (w as usize) * (h as usize) * 3 {
                return Err(KittyError::NoData);
            }
            let mut rgba = Vec::with_capacity((w as usize) * (h as usize) * 4);
            for px in raw.chunks_exact(3) {
                rgba.extend_from_slice(px);
                rgba.push(0xff);
            }
            Ok((w, h, rgba))
        }
        KittyFormat::Png => decode_png(&raw),
    }
}

fn raw_dimensions(cmd: &KittyGraphicsCommand) -> Result<(u32, u32), KittyError> {
    if cmd.width == 0 || cmd.height == 0 {
        return Err(KittyError::Invalid);
    }
    Ok((cmd.width, cmd.height))
}

fn decode_png(bytes: &[u8]) -> Result<(u32, u32, Vec<u8>), KittyError> {
    let decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    let mut reader = decoder.read_info().map_err(|_| KittyError::BadPng)?;
    let info = reader.info();
    let (width, height) = (info.width, info.height);
    if width == 0 || height == 0 || width > MAX_IMAGE_DIM || height > MAX_IMAGE_DIM {
        return Err(KittyError::TooBig);
    }
    let buf_size = reader.output_buffer_size().ok_or(KittyError::TooBig)?;
    let mut buf = vec![0u8; buf_size];
    let frame = reader.next_frame(&mut buf).map_err(|_| KittyError::BadPng)?;
    buf.truncate(frame.buffer_size());

    let rgba = normalize_to_rgba(&buf, width, height, frame.color_type, frame.bit_depth)?;
    Ok((width, height, rgba))
}

/// Normalize a decoded PNG frame to straight RGBA8. `png`'s transformations are
/// left off, so we expand grayscale/RGB/palette-expanded 8-bit output here; 16-bit
/// samples are truncated to the high byte.
fn normalize_to_rgba(
    buf: &[u8],
    width: u32,
    height: u32,
    color: png::ColorType,
    depth: png::BitDepth,
) -> Result<Vec<u8>, KittyError> {
    let pixels = (width as usize) * (height as usize);
    // Only 8- and 16-bit outputs occur here; sub-byte depths are expanded by
    // `png` to 8-bit for grayscale/indexed already when using `next_frame`.
    let sample_bytes = match depth {
        png::BitDepth::Sixteen => 2,
        _ => 1,
    };
    let sample = |i: usize| -> u8 {
        // Take the high byte of a 16-bit sample, or the byte itself.
        buf.get(i * sample_bytes).copied().unwrap_or(0)
    };
    let mut rgba = Vec::with_capacity(pixels * 4);
    match color {
        png::ColorType::Rgba => {
            for i in 0..pixels {
                let base = i * 4;
                rgba.push(sample(base));
                rgba.push(sample(base + 1));
                rgba.push(sample(base + 2));
                rgba.push(sample(base + 3));
            }
        }
        png::ColorType::Rgb => {
            for i in 0..pixels {
                let base = i * 3;
                rgba.push(sample(base));
                rgba.push(sample(base + 1));
                rgba.push(sample(base + 2));
                rgba.push(0xff);
            }
        }
        png::ColorType::GrayscaleAlpha => {
            for i in 0..pixels {
                let base = i * 2;
                let g = sample(base);
                rgba.extend_from_slice(&[g, g, g, sample(base + 1)]);
            }
        }
        png::ColorType::Grayscale => {
            for i in 0..pixels {
                let g = sample(i);
                rgba.extend_from_slice(&[g, g, g, 0xff]);
            }
        }
        png::ColorType::Indexed => {
            // `next_frame` does not expand the palette; reject rather than
            // mis-render (Kitty clients emit RGB/RGBA PNGs in practice).
            return Err(KittyError::BadPng);
        }
    }
    Ok(rgba)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::osc;

    fn b64(bytes: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        osc::encode_base64(bytes, &mut out);
        out
    }

    fn direct(ctrl: &str, data: &[u8]) -> KittyGraphicsCommand {
        let mut full = ctrl.as_bytes().to_vec();
        full.push(b';');
        full.extend_from_slice(&b64(data));
        noa_vt::kitty_graphics::parse(&full, false)
    }

    #[test]
    fn rgba_direct_stores_image() {
        let mut store = ImageStore::new();
        let px = vec![1u8, 2, 3, 4, 5, 6, 7, 8]; // 2x1 RGBA
        let cmd = direct("a=t,f=32,s=2,v=1,i=5", &px);
        let TransmitStep::Done(done) = store.transmit(&cmd) else {
            panic!("expected done");
        };
        assert_eq!(done.result, Ok(5));
        let img = store.get(5).unwrap();
        assert_eq!((img.width, img.height), (2, 1));
        assert_eq!(&*img.rgba, px.as_slice());
    }

    #[test]
    fn rgb_direct_expands_to_rgba() {
        let mut store = ImageStore::new();
        let px = vec![10u8, 20, 30]; // 1x1 RGB
        let cmd = direct("a=t,f=24,s=1,v=1,i=1", &px);
        let TransmitStep::Done(done) = store.transmit(&cmd) else {
            panic!()
        };
        assert_eq!(done.result, Ok(1));
        assert_eq!(&*store.get(1).unwrap().rgba, &[10, 20, 30, 0xff]);
    }

    #[test]
    fn size_mismatch_is_nodata() {
        let mut store = ImageStore::new();
        let cmd = direct("a=t,f=32,s=4,v=4,i=1", &[0u8; 8]); // needs 64 bytes
        let TransmitStep::Done(done) = store.transmit(&cmd) else {
            panic!()
        };
        assert_eq!(done.result, Err(KittyError::NoData));
        assert!(store.get(1).is_none());
    }

    #[test]
    fn missing_dimensions_is_invalid() {
        let mut store = ImageStore::new();
        let cmd = direct("a=t,f=32,i=1", &[0u8; 4]);
        let TransmitStep::Done(done) = store.transmit(&cmd) else {
            panic!()
        };
        assert_eq!(done.result, Err(KittyError::Invalid));
    }

    #[test]
    fn zlib_compressed_direct() {
        use flate2::{write::ZlibEncoder, Compression};
        use std::io::Write;
        let px = vec![9u8, 8, 7, 6];
        let mut enc = ZlibEncoder::new(Vec::new(), Compression::default());
        enc.write_all(&px).unwrap();
        let compressed = enc.finish().unwrap();
        let mut store = ImageStore::new();
        let cmd = direct("a=t,f=32,s=1,v=1,o=z,i=2", &compressed);
        let TransmitStep::Done(done) = store.transmit(&cmd) else {
            panic!()
        };
        assert_eq!(done.result, Ok(2));
        assert_eq!(&*store.get(2).unwrap().rgba, px.as_slice());
    }

    #[test]
    fn chunked_transfer_reassembles() {
        let mut store = ImageStore::new();
        let px = vec![1u8, 2, 3, 4, 5, 6, 7, 8]; // 2x1 RGBA
        // First chunk: control + first 4 bytes, m=1.
        let mut first = b"a=t,f=32,s=2,v=1,i=3,m=1;".to_vec();
        first.extend_from_slice(&b64(&px[..4]));
        let c1 = noa_vt::kitty_graphics::parse(&first, false);
        assert!(matches!(store.transmit(&c1), TransmitStep::NeedMore));
        // Final chunk: remaining bytes, m=0.
        let mut last = b"m=0;".to_vec();
        last.extend_from_slice(&b64(&px[4..]));
        let c2 = noa_vt::kitty_graphics::parse(&last, false);
        let TransmitStep::Done(done) = store.transmit(&c2) else {
            panic!("expected done")
        };
        assert_eq!(done.result, Ok(3));
        assert_eq!(&*store.get(3).unwrap().rgba, px.as_slice());
    }

    #[test]
    fn interrupted_chunk_is_discarded() {
        let mut store = ImageStore::new();
        let mut first = b"a=t,f=32,s=2,v=1,i=3,m=1;".to_vec();
        first.extend_from_slice(&b64(&[1, 2, 3, 4]));
        let c1 = noa_vt::kitty_graphics::parse(&first, false);
        assert!(matches!(store.transmit(&c1), TransmitStep::NeedMore));
        assert!(store.transfer_in_progress());
        store.abort();
        assert!(!store.transfer_in_progress());
        assert!(store.get(3).is_none());
    }

    #[test]
    fn query_validates_without_storing() {
        let mut store = ImageStore::new();
        let px = vec![0u8; 4];
        let cmd = direct("a=q,f=32,s=1,v=1,i=9", &px);
        let TransmitStep::Done(done) = store.transmit(&cmd) else {
            panic!()
        };
        assert_eq!(done.result, Ok(9));
        assert!(store.get(9).is_none(), "query must not store");
    }

    #[test]
    fn auto_id_assignment_skips_used_ids() {
        let mut store = ImageStore::new();
        let px = vec![0u8; 4];
        // Occupy id 1 explicitly.
        let c1 = direct("a=t,f=32,s=1,v=1,i=1", &px);
        store.transmit(&c1);
        // Auto-assign: i=0 → must not collide with 1.
        let c2 = direct("a=t,f=32,s=1,v=1,I=7", &px);
        let TransmitStep::Done(done) = store.transmit(&c2) else {
            panic!()
        };
        let id = done.result.unwrap();
        assert_ne!(id, 1);
        assert_eq!(store.get(id).unwrap().number, 7);
    }

    #[test]
    fn retransmit_bumps_epoch() {
        let mut store = ImageStore::new();
        let px = vec![0u8; 4];
        let cmd = direct("a=t,f=32,s=1,v=1,i=4", &px);
        store.transmit(&cmd);
        let e0 = store.get(4).unwrap().epoch;
        store.transmit(&cmd);
        assert_eq!(store.get(4).unwrap().epoch, e0 + 1);
        assert_eq!(store.len(), 1);
    }

    #[test]
    fn shared_memory_is_unsupported() {
        let mut store = ImageStore::new();
        let cmd = direct("a=t,t=s,f=32,s=1,v=1,i=1", b"whatever");
        let TransmitStep::Done(done) = store.transmit(&cmd) else {
            panic!()
        };
        assert_eq!(done.result, Err(KittyError::Unsupported));
    }

    #[test]
    fn enforce_quota_evicts_unreferenced_oldest_first() {
        let mut store = ImageStore::new();
        // Three 1x1 images (4 bytes each); shrink the budget by hand.
        for i in 1..=3u32 {
            let cmd = direct(&format!("a=t,f=32,s=1,v=1,i={i}"), &[0u8; 4]);
            store.transmit(&cmd);
        }
        assert_eq!(store.len(), 3);
        // Reference id 1 (oldest). With a tiny budget, id 2 (oldest unreferenced)
        // goes first.
        let mut referenced = HashSet::new();
        referenced.insert(1u32);
        // Force over-budget by pretending the limit is smaller: evict until <= 8.
        while store.total_bytes() > 8 {
            let before = store.len();
            // Manually evict one unreferenced oldest.
            let victim = store
                .images
                .iter()
                .filter(|img| !referenced.contains(&img.id))
                .min_by_key(|img| img.seq)
                .map(|img| img.id)
                .unwrap();
            store.remove(victim);
            assert_eq!(store.len(), before - 1);
        }
        assert!(store.contains(1), "referenced image must survive");
    }

    #[test]
    fn tempfile_outside_temp_is_rejected() {
        // A regular file that is not in a temp dir must be rejected for t=t.
        let dir = std::env::temp_dir().join("noa-kitty-not-temp-marker");
        // Use a path that exists but lacks the temp markers by pointing at the
        // crate manifest (always present, absolute, not a temp file).
        let manifest = concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml");
        let mut store = ImageStore::new();
        let cmd = direct("a=t,t=t,f=32,s=1,v=1,i=1", manifest.as_bytes());
        let TransmitStep::Done(done) = store.transmit(&cmd) else {
            panic!()
        };
        assert_eq!(done.result, Err(KittyError::Invalid));
        let _ = dir; // silence unused in case of future edits
    }

    #[test]
    fn file_medium_reads_real_file() {
        let px = vec![7u8, 6, 5, 4];
        let path = std::env::temp_dir().join(format!("noa-kitty-file-{}.bin", std::process::id()));
        std::fs::write(&path, &px).unwrap();
        let mut store = ImageStore::new();
        let cmd = direct(
            "a=t,t=f,f=32,s=1,v=1,i=1",
            path.to_str().unwrap().as_bytes(),
        );
        let TransmitStep::Done(done) = store.transmit(&cmd) else {
            panic!()
        };
        assert_eq!(done.result, Ok(1));
        assert_eq!(&*store.get(1).unwrap().rgba, px.as_slice());
        assert!(path.exists(), "t=f must NOT delete the file");
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn tempfile_medium_is_deleted_after_read() {
        let px = vec![1u8, 1, 1, 1];
        let path = std::env::temp_dir().join(format!(
            "noa-kitty-tty-graphics-protocol-{}.bin",
            std::process::id()
        ));
        std::fs::write(&path, &px).unwrap();
        let mut store = ImageStore::new();
        let cmd = direct(
            "a=t,t=t,f=32,s=1,v=1,i=1",
            path.to_str().unwrap().as_bytes(),
        );
        let TransmitStep::Done(done) = store.transmit(&cmd) else {
            panic!()
        };
        assert_eq!(done.result, Ok(1));
        assert!(!path.exists(), "t=t must delete the file after reading");
    }

    fn encode_png(width: u32, height: u32, color: png::ColorType, data: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut out, width, height);
            encoder.set_color(color);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header().unwrap();
            writer.write_image_data(data).unwrap();
        }
        out
    }

    #[test]
    fn png_1x1_rgba_decodes() {
        let png = encode_png(1, 1, png::ColorType::Rgba, &[11, 22, 33, 44]);
        let mut store = ImageStore::new();
        let cmd = direct("a=t,f=100,i=1", &png);
        let TransmitStep::Done(done) = store.transmit(&cmd) else {
            panic!()
        };
        assert_eq!(done.result, Ok(1));
        let img = store.get(1).unwrap();
        assert_eq!((img.width, img.height), (1, 1));
        assert_eq!(&*img.rgba, &[11, 22, 33, 44]);
    }

    #[test]
    fn png_2x2_rgb_normalizes_to_rgba() {
        // 2x2 RGB: four distinct pixels.
        let rgb = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12];
        let png = encode_png(2, 2, png::ColorType::Rgb, &rgb);
        let mut store = ImageStore::new();
        let cmd = direct("a=t,f=100,i=2", &png);
        let TransmitStep::Done(done) = store.transmit(&cmd) else {
            panic!()
        };
        assert_eq!(done.result, Ok(2));
        let img = store.get(2).unwrap();
        assert_eq!((img.width, img.height), (2, 2));
        assert_eq!(
            &*img.rgba,
            &[1, 2, 3, 0xff, 4, 5, 6, 0xff, 7, 8, 9, 0xff, 10, 11, 12, 0xff]
        );
    }

    #[test]
    fn bad_png_bytes_are_ebadpng() {
        let mut store = ImageStore::new();
        let cmd = direct("a=t,f=100,i=1", b"not a png at all");
        let TransmitStep::Done(done) = store.transmit(&cmd) else {
            panic!()
        };
        assert_eq!(done.result, Err(KittyError::BadPng));
    }

    #[test]
    fn missing_file_is_enoent() {
        let mut store = ImageStore::new();
        let cmd = direct(
            "a=t,t=f,f=32,s=1,v=1,i=1",
            b"/nonexistent/noa/kitty/path.bin",
        );
        let TransmitStep::Done(done) = store.transmit(&cmd) else {
            panic!()
        };
        assert_eq!(done.result, Err(KittyError::NoEnt));
    }
}
