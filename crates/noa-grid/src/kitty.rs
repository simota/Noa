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
/// Configurable per terminal via [`ImageStore::set_byte_limit`].
pub const TOTAL_BYTES_LIMIT: usize = 320_000_000;
/// Default per-frame gap (ms) applied when a frame declares none (`z=0`),
/// matching kitty's animation default.
const DEFAULT_FRAME_GAP_MS: i32 = 40;

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
    /// `EUNSUPPORTED` — a request unsupported on this platform (e.g. shared
    /// memory off Unix).
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

/// One animation frame: a full image-canvas straight-RGBA8 buffer plus its gap.
#[derive(Clone, Debug)]
pub struct KittyFrame {
    /// Straight (non-premultiplied) RGBA8 for the whole image canvas.
    pub rgba: Arc<[u8]>,
    /// Gap before the next frame, in milliseconds. `< 0` marks a "gapless" frame
    /// (kitty) shown for zero duration; `0` is normalized to
    /// [`DEFAULT_FRAME_GAP_MS`] at creation.
    pub gap_ms: i32,
}

/// Playback state for a multi-frame image. A single-frame image never animates.
#[derive(Clone, Debug)]
struct Anim {
    running: bool,
    /// 1-based index of the frame currently shown.
    current: usize,
    /// Remaining loops; `None` = loop forever. A loop completes each time
    /// playback wraps from the last frame back to the first.
    loops_remaining: Option<u32>,
    /// Monotonic ms timestamp (app clock) at which `current` began showing;
    /// `None` until the first `advance_animations` seeds it.
    shown_at_ms: Option<u64>,
}

impl Default for Anim {
    fn default() -> Self {
        Anim {
            running: false,
            current: 1,
            loops_remaining: None,
            shown_at_ms: None,
        }
    }
}

/// A stored image: straight (non-premultiplied) RGBA8, shared with the renderer
/// via `Arc` so the snapshot copy is a refcount bump, not a pixel copy.
///
/// [`rgba`](Self::rgba) always aliases the currently displayed frame's pixels;
/// [`frames`](Self::frames) holds every frame (`frames[0]` is the root, frame 1).
/// A still image has exactly one frame.
#[derive(Clone, Debug)]
pub struct KittyImage {
    pub id: u32,
    /// `I=` image number (0 = none). The newest transfer with a given number
    /// wins when a client refers to images by number.
    pub number: u32,
    pub width: u32,
    pub height: u32,
    /// Pixels of the frame currently shown (aliases `frames[anim.current - 1]`).
    pub rgba: Arc<[u8]>,
    /// Bumped whenever this image's displayed pixels change: on re-transmit and
    /// on each animation frame advance. The renderer keys its texture cache on
    /// `(id, epoch)` so any change forces a re-upload.
    pub epoch: u64,
    /// Monotonic transfer order, used to pick the oldest victim under quota.
    pub seq: u64,
    /// All animation frames; `frames[0]` is the root (frame 1).
    frames: Vec<KittyFrame>,
    anim: Anim,
}

impl KittyImage {
    /// Number of animation frames (>= 1).
    pub fn frame_count(&self) -> usize {
        self.frames.len()
    }

    /// Total decoded bytes held across all frames.
    fn total_frame_bytes(&self) -> usize {
        self.frames.iter().map(|f| f.rgba.len()).sum()
    }

    /// Point `rgba` at the current frame and bump `epoch` so the renderer
    /// re-uploads. Clamps `anim.current` into range first.
    fn refresh_current(&mut self) {
        if self.anim.current == 0 || self.anim.current > self.frames.len() {
            self.anim.current = 1;
        }
        self.rgba = Arc::clone(&self.frames[self.anim.current - 1].rgba);
        self.epoch = self.epoch.wrapping_add(1);
    }
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

/// Result of [`ImageStore::advance_animations`].
pub struct AnimationTick {
    /// Some image advanced a frame — the caller should repaint.
    pub changed: bool,
    /// Soonest absolute-ms time at which another frame is due, if any animation
    /// is still running. The caller schedules its next wake-up from this.
    pub next_wake: Option<u64>,
}

/// One step of feeding a graphics command into the store.
pub enum TransmitStep {
    /// A chunk was accepted; more are expected. No reply yet.
    NeedMore,
    /// The transfer finished (single-shot or last chunk).
    Done(TransmitDone),
}

/// Screen-independent image storage with a global byte quota.
pub struct ImageStore {
    images: Vec<KittyImage>,
    total_bytes: usize,
    /// Configurable total-byte budget (`image-storage-limit`); doubles as the
    /// per-image / intermediate-decode ceiling so an inflating `o=z` stream or
    /// oversized frame can't exceed the whole terminal's budget.
    byte_limit: usize,
    next_auto_id: u32,
    next_epoch: u64,
    next_seq: u64,
    transfer: Option<PendingTransfer>,
}

impl Default for ImageStore {
    fn default() -> Self {
        ImageStore {
            images: Vec::new(),
            total_bytes: 0,
            byte_limit: TOTAL_BYTES_LIMIT,
            next_auto_id: 1,
            next_epoch: 0,
            next_seq: 0,
            transfer: None,
        }
    }
}

impl ImageStore {
    pub fn new() -> Self {
        ImageStore::default()
    }

    /// Set the total decoded-byte budget (`image-storage-limit`). Evicts down to
    /// the new limit immediately; images with a live placement are spared last.
    pub fn set_byte_limit(&mut self, bytes: usize) {
        self.byte_limit = bytes;
        self.enforce_quota(&HashSet::new());
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

    /// All stored image ids carrying image number `number` (`I=`).
    pub fn ids_with_number(&self, number: u32) -> Vec<u32> {
        self.images
            .iter()
            .filter(|img| img.number == number)
            .map(|img| img.id)
            .collect()
    }

    /// All stored image ids (used by the quota sweep's "referenced" set).
    pub fn contains(&self, id: u32) -> bool {
        self.images.iter().any(|img| img.id == id)
    }

    /// Drop the image with `id` and its bytes. Returns whether anything changed.
    pub fn remove(&mut self, id: u32) -> bool {
        if let Some(pos) = self.images.iter().position(|img| img.id == id) {
            self.total_bytes -= self.images[pos].total_frame_bytes();
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

    /// Store already-rasterized straight RGBA pixels, assigning an internal
    /// image id. Used by SIXEL, which has no protocol-level image id but can
    /// share the same renderer/cache path.
    pub fn insert_rgba(
        &mut self,
        width: u32,
        height: u32,
        rgba: Vec<u8>,
    ) -> Result<u32, KittyError> {
        if width == 0 || height == 0 || width > MAX_IMAGE_DIM || height > MAX_IMAGE_DIM {
            return Err(KittyError::TooBig);
        }
        let expected = (width as usize)
            .checked_mul(height as usize)
            .and_then(|px| px.checked_mul(4))
            .ok_or(KittyError::TooBig)?;
        if rgba.len() != expected {
            return Err(KittyError::NoData);
        }
        if rgba.len() > self.byte_limit {
            return Err(KittyError::TooBig);
        }
        let id = self.assign_auto_id();
        self.insert(id, 0, width, height, rgba);
        Ok(id)
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
        // Errors on a continuation chunk must be reported against the *start*
        // chunk's ctrl: only it carries the `i=`/`I=`/`q=` keys. Continuation
        // chunks omit them, so replying from `cmd` would trip the "no reply
        // without i= or I=" rule and silently swallow the error.
        let chunk = match decode_base64_limited(&cmd.payload, self.byte_limit) {
            Some(bytes) => bytes,
            None => {
                let ctrl = self.transfer.take().expect("transfer in progress").ctrl;
                return TransmitStep::Done(TransmitDone {
                    ctrl,
                    result: Err(KittyError::Invalid),
                });
            }
        };
        let transfer = self.transfer.as_mut().expect("transfer in progress");
        if transfer.decoded.len() + chunk.len() > self.byte_limit {
            let ctrl = self.transfer.take().expect("transfer in progress").ctrl;
            return TransmitStep::Done(TransmitDone {
                ctrl,
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
        decode_base64_limited(&cmd.payload, self.byte_limit).ok_or(KittyError::Invalid)
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
            KittyMedium::SharedMem => {
                let raw = self.read_shared_memory(cmd)?;
                self.decompress(cmd, raw)
            }
        }
    }

    /// Read a POSIX shared-memory payload (`t=s`): the base64 payload is the shm
    /// object name (kitty convention: a leading-slash name from `shm_open`). The
    /// object is `mmap`ped read-only, the requested byte range copied out, then
    /// `shm_unlink`ed — the terminal owns unlinking after a successful read, per
    /// the kitty spec. Honors `O=`/`S=` offset/size like the file medium.
    #[cfg(unix)]
    fn read_shared_memory(&self, cmd: &KittyGraphicsCommand) -> Result<Vec<u8>, KittyError> {
        use std::ffi::CString;
        let name = self.decode_base64(cmd)?;
        let cname = CString::new(name).map_err(|_| KittyError::Invalid)?;

        // SAFETY: `cname` is a valid NUL-terminated C string for the duration of
        // each call; all mapped pointers are checked before use and released on
        // every exit path.
        unsafe {
            let fd = libc::shm_open(cname.as_ptr(), libc::O_RDONLY, 0);
            if fd < 0 {
                return Err(KittyError::NoEnt);
            }
            let mut st: libc::stat = std::mem::zeroed();
            if libc::fstat(fd, &mut st) != 0 {
                libc::close(fd);
                return Err(KittyError::NoEnt);
            }
            // `fstat` on a POSIX shm object reports the size on Linux but returns
            // 0 on macOS, so the byte count is taken from `S=` (the declared
            // size) or computed from the raw format/dimensions, falling back to
            // the stat size only when neither is available.
            let stat_size = st.st_size.max(0) as u64;
            let offset = cmd.file_offset as u64;
            // The declared *data* size drives how much to read (the shm object
            // may be page-rounded larger): `S=` wins, then the raw
            // format/dimensions, then the stat size as a last resort.
            let want = if cmd.file_size != 0 {
                (cmd.file_size as u64).saturating_sub(offset)
            } else if let Some(e) = expected_raw_len(cmd) {
                e as u64
            } else {
                stat_size.saturating_sub(offset)
            };
            if want as usize > self.byte_limit {
                libc::close(fd);
                let _ = libc::shm_unlink(cname.as_ptr());
                return Err(KittyError::TooBig);
            }
            if want == 0 {
                libc::close(fd);
                let _ = libc::shm_unlink(cname.as_ptr());
                return Err(KittyError::NoData);
            }
            let map_len = (offset + want) as usize;
            let ptr = libc::mmap(
                std::ptr::null_mut(),
                map_len,
                libc::PROT_READ,
                libc::MAP_SHARED,
                fd,
                0,
            );
            libc::close(fd);
            if ptr == libc::MAP_FAILED {
                let _ = libc::shm_unlink(cname.as_ptr());
                return Err(KittyError::NoEnt);
            }
            let src = std::slice::from_raw_parts(
                (ptr as *const u8).add(offset as usize),
                want as usize,
            );
            let out = src.to_vec();
            libc::munmap(ptr, map_len);
            let _ = libc::shm_unlink(cname.as_ptr());
            Ok(out)
        }
    }

    #[cfg(not(unix))]
    fn read_shared_memory(&self, _cmd: &KittyGraphicsCommand) -> Result<Vec<u8>, KittyError> {
        Err(KittyError::Unsupported)
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
        if want as usize > self.byte_limit {
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
            Some(KittyCompression::Zlib) => inflate_bounded(&bytes, self.byte_limit),
        }
    }

    /// Finish a single-shot transfer whose raw bytes are already resolved.
    fn finalize(&mut self, cmd: &KittyGraphicsCommand, raw: Vec<u8>) -> Result<u32, KittyError> {
        self.build_and_store(cmd, raw)
    }

    /// Decode `raw` into RGBA per `f=`, then (for non-query actions) store it.
    /// `a=f` frame transfers branch to [`Self::store_frame`] instead.
    fn build_and_store(
        &mut self,
        cmd: &KittyGraphicsCommand,
        raw: Vec<u8>,
    ) -> Result<u32, KittyError> {
        if cmd.action == KittyAction::TransmitFrame {
            return self.store_frame(cmd, raw);
        }
        let (width, height, rgba) = decode_to_rgba(cmd, raw)?;
        if width == 0 || height == 0 || width > MAX_IMAGE_DIM || height > MAX_IMAGE_DIM {
            return Err(KittyError::TooBig);
        }
        if rgba.len() > self.byte_limit {
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
        self.assign_auto_id()
    }

    fn assign_auto_id(&mut self) -> u32 {
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
            // A re-transmit replaces the whole image, dropping any prior frames
            // and resetting animation state.
            self.total_bytes -= existing.total_frame_bytes();
            existing.epoch = existing.epoch.wrapping_add(1);
            existing.number = number;
            existing.width = width;
            existing.height = height;
            existing.rgba = Arc::clone(&rgba);
            existing.frames = vec![KittyFrame {
                rgba,
                gap_ms: DEFAULT_FRAME_GAP_MS,
            }];
            existing.anim = Anim::default();
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
                rgba: Arc::clone(&rgba),
                epoch,
                seq,
                frames: vec![KittyFrame {
                    rgba,
                    gap_ms: DEFAULT_FRAME_GAP_MS,
                }],
                anim: Anim::default(),
            });
            self.total_bytes += bytes;
        }
    }

    /// Evict images until the total byte budget is satisfied. Images whose id is
    /// not in `referenced` (no visible placement) are dropped first, oldest by
    /// `seq`; then, if still over budget, the oldest overall.
    pub fn enforce_quota(&mut self, referenced: &HashSet<u32>) {
        while self.total_bytes > self.byte_limit {
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

    // ── Animation (a=f / a=a / a=c) ─────────────────────────────────────

    /// Resolve the animation-command target image id: explicit `i=`, else the
    /// newest image with number `I=`.
    fn resolve_anim_target(&self, cmd: &KittyGraphicsCommand) -> Option<u32> {
        if cmd.image_id != 0 {
            return self.get(cmd.image_id).map(|_| cmd.image_id);
        }
        if cmd.image_number != 0 {
            return self.get_by_number(cmd.image_number).map(|img| img.id);
        }
        None
    }

    /// Store an `a=f` frame: `raw` is the decoded frame-data rectangle (per `f=`,
    /// `s=`/`v=`). It is composited over a base (frame `c=`, else the `Y=`
    /// background color) at pixel offset `x=`/`y=` with mode `X=`, then appended
    /// as a new frame or written into frame `r=`. Returns the target image id.
    fn store_frame(&mut self, cmd: &KittyGraphicsCommand, raw: Vec<u8>) -> Result<u32, KittyError> {
        let (data_w, data_h, data) = decode_to_rgba(cmd, raw)?;
        let Some(target_id) = self.resolve_anim_target(cmd) else {
            return Err(KittyError::NoEnt);
        };
        let img = self.get(target_id).expect("target resolved above");
        let (canvas_w, canvas_h) = (img.width, img.height);
        let canvas_px = (canvas_w as usize) * (canvas_h as usize) * 4;

        // The frame-data rectangle must fit within the image canvas. Use checked
        // arithmetic: a raw `off_x + data_w` wraps in release builds and lets an
        // out-of-bounds x=/y= slip past into composite_rect's indexing.
        let off_x = cmd.src_x;
        let off_y = cmd.src_y;
        let fits = off_x
            .checked_add(data_w)
            .zip(off_y.checked_add(data_h))
            .is_some_and(|(x_end, y_end)| x_end <= canvas_w && y_end <= canvas_h);
        if !fits {
            return Err(KittyError::Invalid);
        }

        // Base canvas: copy frame `c=` (1-based) or fill with the `Y=` color.
        let base_frame = cmd.columns; // c=
        let mut canvas: Vec<u8> = if base_frame != 0 {
            let idx = base_frame as usize;
            if idx > img.frames.len() {
                return Err(KittyError::Invalid);
            }
            img.frames[idx - 1].rgba.to_vec()
        } else {
            let bg = cmd.cell_y_off; // Y= background as 0xRRGGBBAA
            let px = bg.to_be_bytes();
            let mut v = Vec::with_capacity(canvas_px);
            for _ in 0..(canvas_w as usize * canvas_h as usize) {
                v.extend_from_slice(&px);
            }
            v
        };
        debug_assert_eq!(canvas.len(), canvas_px);

        let overwrite = cmd.cell_x_off == 1; // X=1 replaces, X=0 alpha-blends
        composite_rect(
            &mut canvas,
            canvas_w,
            &data,
            data_w,
            data_h,
            off_x,
            off_y,
            overwrite,
        );

        let gap_ms = normalize_gap(cmd.z_index);
        let edit_frame = cmd.rows; // r=
        let new_frame = KittyFrame {
            rgba: Arc::from(canvas),
            gap_ms,
        };
        let new_bytes = new_frame.rgba.len();

        let img = self
            .images
            .iter_mut()
            .find(|i| i.id == target_id)
            .expect("target resolved above");
        if edit_frame != 0 {
            let idx = edit_frame as usize;
            if idx > img.frames.len() {
                return Err(KittyError::Invalid);
            }
            self.total_bytes -= img.frames[idx - 1].rgba.len();
            img.frames[idx - 1] = new_frame;
            self.total_bytes += new_bytes;
        } else {
            if self.total_bytes + new_bytes > self.byte_limit {
                return Err(KittyError::TooBig);
            }
            img.frames.push(new_frame);
            self.total_bytes += new_bytes;
            // Adding a second frame auto-starts looping playback, matching
            // kitty's default of animating as soon as frames exist.
            if img.frames.len() == 2 {
                img.anim.running = true;
                img.anim.loops_remaining = None;
            }
        }
        // Refresh so a re-uploaded texture reflects any edit to the shown frame.
        let img = self
            .images
            .iter_mut()
            .find(|i| i.id == target_id)
            .expect("target resolved above");
        img.refresh_current();
        Ok(target_id)
    }

    /// Apply an `a=a` animation-control command (state `s=`, current frame `c=`,
    /// loop count `v=`, per-frame gap edit `r=`/`z=`).
    pub fn animate(&mut self, cmd: &KittyGraphicsCommand) -> Result<(), KittyError> {
        let Some(target_id) = self.resolve_anim_target(cmd) else {
            return Err(KittyError::NoEnt);
        };
        let img = self
            .images
            .iter_mut()
            .find(|i| i.id == target_id)
            .expect("target resolved above");

        // r= with z= edits that frame's gap without changing playback.
        if cmd.rows != 0 {
            let idx = cmd.rows as usize;
            if idx > img.frames.len() {
                return Err(KittyError::Invalid);
            }
            img.frames[idx - 1].gap_ms = normalize_gap(cmd.z_index);
        }

        // c= sets the current (displayed) frame.
        if cmd.columns != 0 {
            let idx = cmd.columns as usize;
            if idx > img.frames.len() {
                return Err(KittyError::Invalid);
            }
            img.anim.current = idx;
            img.anim.shown_at_ms = None; // re-seed the gap clock on the new frame
            img.refresh_current();
        }

        // v= loop count: 0 leaves it, 1 = infinite, n>1 = loop n-1 times.
        match cmd.height {
            0 => {}
            1 => img.anim.loops_remaining = None,
            n => img.anim.loops_remaining = Some(n - 1),
        }

        // s= state: 1 stop, 2/3 run.
        match cmd.width {
            1 => img.anim.running = false,
            2 | 3 => img.anim.running = true,
            _ => {}
        }
        Ok(())
    }

    /// Apply an `a=c` compose command: blend source frame `c=`'s pixels onto
    /// destination frame `r=` over the rectangle `x=`/`y=`/`w=`/`h=` (default the
    /// whole frame), with mode `X=`.
    pub fn compose(&mut self, cmd: &KittyGraphicsCommand) -> Result<(), KittyError> {
        let Some(target_id) = self.resolve_anim_target(cmd) else {
            return Err(KittyError::NoEnt);
        };
        let dst_idx = cmd.rows as usize; // r=
        let src_idx = cmd.columns as usize; // c=
        if dst_idx == 0 || src_idx == 0 {
            return Err(KittyError::Invalid);
        }
        let img = self
            .images
            .iter_mut()
            .find(|i| i.id == target_id)
            .expect("target resolved above");
        if dst_idx > img.frames.len() || src_idx > img.frames.len() {
            return Err(KittyError::Invalid);
        }
        let (canvas_w, canvas_h) = (img.width, img.height);
        let rect_x = cmd.src_x;
        let rect_y = cmd.src_y;
        let rect_w = if cmd.src_w != 0 { cmd.src_w } else { canvas_w };
        let rect_h = if cmd.src_h != 0 { cmd.src_h } else { canvas_h };
        // Reject attacker-controlled x=/y=/w=/h= geometry that overflows or falls
        // outside the canvas. Plain `rect_x + rect_w` wraps in release builds (no
        // overflow-checks), slipping past the bound into composite_from's indexing.
        let fits = rect_x
            .checked_add(rect_w)
            .zip(rect_y.checked_add(rect_h))
            .is_some_and(|(x_end, y_end)| x_end <= canvas_w && y_end <= canvas_h);
        if !fits {
            return Err(KittyError::Invalid);
        }
        let overwrite = cmd.cell_x_off == 1; // X=1 replaces
        let src = Arc::clone(&img.frames[src_idx - 1].rgba);
        let mut dst = img.frames[dst_idx - 1].rgba.to_vec();
        composite_from(
            &mut dst, &src, canvas_w, rect_x, rect_y, rect_w, rect_h, overwrite,
        );
        img.frames[dst_idx - 1].rgba = Arc::from(dst);
        img.refresh_current();
        Ok(())
    }

    /// Delete an image's animation frames (`a=d,d=f`), keeping the root frame and
    /// resetting playback. Returns whether anything changed.
    pub fn delete_frames(&mut self, id: u32) -> bool {
        let Some(img) = self.images.iter_mut().find(|i| i.id == id) else {
            return false;
        };
        if img.frames.len() <= 1 {
            return false;
        }
        let dropped: usize = img.frames[1..].iter().map(|f| f.rgba.len()).sum();
        img.frames.truncate(1);
        img.anim = Anim::default();
        img.refresh_current();
        self.total_bytes -= dropped;
        true
    }

    /// Advance every running animation to the frame due at monotonic time
    /// `now_ms`. Returns whether any image changed (so the caller repaints) and
    /// the soonest absolute-ms deadline at which another frame is due.
    pub fn advance_animations(&mut self, now_ms: u64) -> AnimationTick {
        let mut changed = false;
        let mut next_wake: Option<u64> = None;
        for img in &mut self.images {
            if !img.anim.running || img.frames.len() < 2 {
                continue;
            }
            let mut shown_at = *img.anim.shown_at_ms.get_or_insert(now_ms);
            // Walk forward frame-by-frame so a long stall crosses multiple gaps.
            loop {
                let gap = img.frames[img.anim.current - 1].gap_ms.max(0) as u64;
                let due = shown_at.saturating_add(gap);
                if now_ms < due {
                    next_wake = Some(next_wake.map_or(due, |w| w.min(due)));
                    break;
                }
                // Advance to the next frame, wrapping and counting a loop.
                let last = img.frames.len();
                if img.anim.current >= last {
                    match img.anim.loops_remaining {
                        Some(0) => {
                            img.anim.running = false;
                            break;
                        }
                        Some(n) => img.anim.loops_remaining = Some(n - 1),
                        None => {}
                    }
                    img.anim.current = 1;
                } else {
                    img.anim.current += 1;
                }
                shown_at = due;
                img.anim.shown_at_ms = Some(due);
                img.refresh_current();
                changed = true;
            }
        }
        AnimationTick { changed, next_wake }
    }

    /// Whether any stored image is currently animating (>= 2 frames, running).
    pub fn has_running_animation(&self) -> bool {
        self.images
            .iter()
            .any(|img| img.anim.running && img.frames.len() >= 2)
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

/// Expected raw payload length (bytes) for a fixed-format transfer, from `f=`
/// and `s=`/`v=`. `None` for PNG (`f=100`), whose size is not derivable here.
fn expected_raw_len(cmd: &KittyGraphicsCommand) -> Option<usize> {
    let px = (cmd.width as usize).checked_mul(cmd.height as usize)?;
    match cmd.format {
        KittyFormat::Rgba => px.checked_mul(4),
        KittyFormat::Rgb => px.checked_mul(3),
        KittyFormat::Png => None,
    }
}

/// Normalize a frame gap (`z=`) to milliseconds: `0` → the default gap, negative
/// values are kept (kitty "gapless" frames render for zero duration).
fn normalize_gap(z: i32) -> i32 {
    if z == 0 { DEFAULT_FRAME_GAP_MS } else { z }
}

/// Composite a `data_w`×`data_h` straight-RGBA source rectangle onto `canvas`
/// (of width `canvas_w`) at pixel offset (`off_x`, `off_y`). `overwrite` copies
/// source pixels verbatim; otherwise the source is alpha-blended over the base.
#[allow(clippy::too_many_arguments)]
fn composite_rect(
    canvas: &mut [u8],
    canvas_w: u32,
    data: &[u8],
    data_w: u32,
    data_h: u32,
    off_x: u32,
    off_y: u32,
    overwrite: bool,
) {
    for row in 0..data_h {
        for col in 0..data_w {
            // Index in usize so the arithmetic cannot wrap even if a future caller
            // forgets the bounds check the compose/store paths perform.
            let s = (row as usize * data_w as usize + col as usize) * 4;
            let d = ((off_y + row) as usize * canvas_w as usize + (off_x + col) as usize) * 4;
            blend_pixel(canvas, d, &data[s..s + 4], overwrite);
        }
    }
}

/// Composite `src` (same dimensions as the destination canvas, width `canvas_w`)
/// onto `dst` over the rectangle (`rx`,`ry`,`rw`,`rh`). Used by `a=c` compose.
#[allow(clippy::too_many_arguments)]
fn composite_from(
    dst: &mut [u8],
    src: &[u8],
    canvas_w: u32,
    rx: u32,
    ry: u32,
    rw: u32,
    rh: u32,
    overwrite: bool,
) {
    for row in 0..rh {
        for col in 0..rw {
            // Index in usize so the arithmetic cannot wrap even if a future caller
            // forgets the bounds check the compose path performs.
            let idx = ((ry + row) as usize * canvas_w as usize + (rx + col) as usize) * 4;
            let s: [u8; 4] = [src[idx], src[idx + 1], src[idx + 2], src[idx + 3]];
            blend_pixel(dst, idx, &s, overwrite);
        }
    }
}

/// Blend one straight-RGBA source pixel into `canvas` at byte offset `d`, using
/// the source-over operator (or a plain copy when `overwrite`).
fn blend_pixel(canvas: &mut [u8], d: usize, src: &[u8], overwrite: bool) {
    if overwrite {
        canvas[d..d + 4].copy_from_slice(src);
        return;
    }
    let sa = src[3] as u32;
    if sa == 0 {
        return;
    }
    if sa == 255 {
        canvas[d..d + 4].copy_from_slice(src);
        return;
    }
    let da = canvas[d + 3] as u32;
    // out_a = sa + da*(1-sa); work in 0..=255 fixed point (÷255).
    let out_a = sa + da * (255 - sa) / 255;
    for c in 0..3 {
        let sc = src[c] as u32;
        let dc = canvas[d + c] as u32;
        let num = sc * sa + dc * da * (255 - sa) / 255;
        canvas[d + c] = if out_a == 0 { 0 } else { (num / out_a) as u8 };
    }
    canvas[d + 3] = out_a as u8;
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

fn read_file_range(path: &std::path::Path, offset: u64, len: usize) -> Result<Vec<u8>, KittyError> {
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
    let frame = reader
        .next_frame(&mut buf)
        .map_err(|_| KittyError::BadPng)?;
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
        use flate2::{Compression, write::ZlibEncoder};
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
    fn continuation_chunk_error_reports_start_chunk_ctrl() {
        // A continuation chunk carries no `i=`/`I=`/`q=`; an error on it must be
        // reported against the start chunk's ctrl so the reply isn't suppressed.
        let mut store = ImageStore::new();
        let mut first = b"a=t,f=32,s=2,v=1,i=5,q=0,m=1;".to_vec();
        first.extend_from_slice(&b64(&[1, 2, 3, 4]));
        let c1 = noa_vt::kitty_graphics::parse(&first, false);
        assert!(matches!(store.transmit(&c1), TransmitStep::NeedMore));

        // Continuation chunk with invalid base64.
        let bad = noa_vt::kitty_graphics::parse(b"m=0;@@@not base64@@@", false);
        assert_eq!(bad.image_id, 0, "continuation chunk carries no i=");
        let TransmitStep::Done(done) = store.transmit(&bad) else {
            panic!("expected done")
        };
        assert_eq!(done.result, Err(KittyError::Invalid));
        assert_eq!(done.ctrl.image_id, 5, "reply must use start chunk's i=");
        assert_eq!(done.ctrl.quiet, 0);
        assert!(!store.transfer_in_progress());
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
    fn shared_memory_missing_object_is_enoent() {
        // A shm name that does not resolve to an object must report ENOENT, not
        // EUNSUPPORTED (the medium is implemented).
        let mut store = ImageStore::new();
        let cmd = direct("a=t,t=s,f=32,s=1,v=1,i=1", b"/noa-kitty-does-not-exist");
        let TransmitStep::Done(done) = store.transmit(&cmd) else {
            panic!()
        };
        assert_eq!(done.result, Err(KittyError::NoEnt));
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
            &[
                1, 2, 3, 0xff, 4, 5, 6, 0xff, 7, 8, 9, 0xff, 10, 11, 12, 0xff
            ]
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

    // ── Animation (a=f / a=a / a=c) ─────────────────────────────────────

    /// Store a 2×1 RGBA base image with id 1 and return the store.
    fn store_with_base() -> ImageStore {
        let mut store = ImageStore::new();
        let base = vec![10u8, 20, 30, 255, 40, 50, 60, 255]; // 2x1
        let cmd = direct("a=t,f=32,s=2,v=1,i=1", &base);
        let TransmitStep::Done(done) = store.transmit(&cmd) else {
            panic!("base transfer must complete")
        };
        assert_eq!(done.result, Ok(1));
        store
    }

    #[test]
    fn frame_transmit_appends_frame_and_autostarts() {
        let mut store = store_with_base();
        // a=f: full-canvas frame data (2x1 RGBA), overwrite mode (X=1).
        let frame = vec![1u8, 2, 3, 255, 4, 5, 6, 255];
        let cmd = direct("a=f,i=1,f=32,s=2,v=1,X=1", &frame);
        let TransmitStep::Done(done) = store.transmit(&cmd) else {
            panic!("frame transfer must complete")
        };
        assert_eq!(done.result, Ok(1));
        let img = store.get(1).unwrap();
        assert_eq!(img.frame_count(), 2);
        assert!(store.has_running_animation(), "2 frames auto-start playback");
    }

    #[test]
    fn frame_transmit_without_base_is_enoent() {
        let mut store = ImageStore::new();
        let frame = vec![0u8; 8];
        let cmd = direct("a=f,i=99,f=32,s=2,v=1", &frame);
        let TransmitStep::Done(done) = store.transmit(&cmd) else {
            panic!()
        };
        assert_eq!(done.result, Err(KittyError::NoEnt));
    }

    #[test]
    fn frame_transmit_out_of_bounds_is_invalid() {
        let mut store = store_with_base();
        // 2x1 data placed at x=1 spills past the 2px-wide canvas.
        let frame = vec![1u8, 2, 3, 255, 4, 5, 6, 255];
        let cmd = direct("a=f,i=1,f=32,s=2,v=1,x=1", &frame);
        let TransmitStep::Done(done) = store.transmit(&cmd) else {
            panic!()
        };
        assert_eq!(done.result, Err(KittyError::Invalid));
    }

    #[test]
    fn frame_compose_over_base_color() {
        // A 1px-wide frame filled with an opaque data pixel on a transparent
        // Y=0 background overwrites only column 0.
        let mut store = store_with_base();
        let data = vec![7u8, 8, 9, 255]; // 1x1
        let cmd = direct("a=f,i=1,f=32,s=1,v=1,x=0,y=0,X=1", &data);
        let TransmitStep::Done(done) = store.transmit(&cmd) else {
            panic!()
        };
        assert_eq!(done.result, Ok(1));
        let img = store.get(1).unwrap();
        // Frame 2 canvas: column 0 = data pixel, column 1 = transparent bg.
        let f2 = &img.frames[1].rgba;
        assert_eq!(&f2[0..4], &[7, 8, 9, 255]);
        assert_eq!(&f2[4..8], &[0, 0, 0, 0]);
    }

    #[test]
    fn animate_controls_state_and_current_frame() {
        let mut store = store_with_base();
        // Add a second frame.
        let frame = vec![1u8, 2, 3, 255, 4, 5, 6, 255];
        store.transmit(&direct("a=f,i=1,f=32,s=2,v=1,X=1", &frame));
        // Stop, then set current frame to 2.
        assert_eq!(store.animate(&direct("a=a,i=1,s=1", &[])), Ok(()));
        assert!(!store.has_running_animation());
        assert_eq!(store.animate(&direct("a=a,i=1,c=2", &[])), Ok(()));
        assert_eq!(&*store.get(1).unwrap().rgba, frame.as_slice());
        // Out-of-range current frame is rejected.
        assert_eq!(
            store.animate(&direct("a=a,i=1,c=9", &[])),
            Err(KittyError::Invalid)
        );
    }

    #[test]
    fn compose_action_overwrites_destination() {
        let mut store = store_with_base();
        // Frame 2 = distinct pixels.
        let frame = vec![100u8, 100, 100, 255, 200, 200, 200, 255];
        store.transmit(&direct("a=f,i=1,f=32,s=2,v=1,X=1", &frame));
        // a=c: copy frame 2 (c=2) onto frame 1 (r=1), overwrite (X=1).
        assert_eq!(
            store.compose(&direct("a=c,i=1,r=1,c=2,X=1", &[])),
            Ok(())
        );
        assert_eq!(&*store.get(1).unwrap().frames[0].rgba, frame.as_slice());
    }

    #[test]
    fn compose_rejects_overflowing_source_rect() {
        // Regression (Critical DoS): a hostile x=/w= near u32::MAX must not wrap
        // past the bounds check (release builds wrap on overflow) into an
        // out-of-bounds index in composite_from — that panic would kill the pty
        // io thread and permanently freeze the pane.
        let mut store = store_with_base(); // 2x1 canvas, image i=1
        let done = store.compose(&direct("a=c,i=1,r=1,c=1,x=4294967295,w=2", &[]));
        assert_eq!(done, Err(KittyError::Invalid));
        // The destination frame is left untouched.
        assert_eq!(
            &*store.get(1).unwrap().frames[0].rgba,
            &[10u8, 20, 30, 255, 40, 50, 60, 255]
        );
    }

    #[test]
    fn store_frame_rejects_overflowing_offset() {
        // Regression (Critical DoS): the a=f frame path shared the same unguarded
        // u32-addition bounds check; a hostile x= near u32::MAX must be rejected,
        // not wrapped into composite_rect's indexing.
        let mut store = store_with_base();
        let TransmitStep::Done(done) =
            store.transmit(&direct("a=f,i=1,f=32,s=1,v=1,x=4294967295", &[1u8, 2, 3, 4]))
        else {
            panic!("expected done");
        };
        assert_eq!(done.result, Err(KittyError::Invalid));
        // No frame was appended past the base frame.
        assert_eq!(store.get(1).unwrap().frame_count(), 1);
    }

    #[test]
    fn advance_animations_walks_frames_on_clock() {
        let mut store = store_with_base();
        // Frame 2 with an explicit 100ms gap (z=100).
        let frame = vec![1u8, 2, 3, 255, 4, 5, 6, 255];
        store.transmit(&direct("a=f,i=1,f=32,s=2,v=1,X=1,z=100", &frame));
        // Base frame keeps the default 40ms gap. At t=0 nothing is due yet;
        // first advance seeds the clock.
        let t0 = store.advance_animations(0);
        assert!(!t0.changed);
        assert_eq!(t0.next_wake, Some(40));
        // At 40ms the base frame's gap elapses → advance to frame 2.
        let t1 = store.advance_animations(40);
        assert!(t1.changed);
        assert_eq!(store.get(1).unwrap().anim.current, 2);
        // Frame 2's 100ms gap means the next flip is due at 140ms.
        assert_eq!(t1.next_wake, Some(140));
    }

    #[test]
    fn delete_frames_keeps_root() {
        let mut store = store_with_base();
        let frame = vec![1u8, 2, 3, 255, 4, 5, 6, 255];
        store.transmit(&direct("a=f,i=1,f=32,s=2,v=1,X=1", &frame));
        assert_eq!(store.get(1).unwrap().frame_count(), 2);
        assert!(store.delete_frames(1));
        assert_eq!(store.get(1).unwrap().frame_count(), 1);
        assert!(!store.has_running_animation());
    }

    #[test]
    fn frame_bytes_count_toward_quota() {
        let mut store = store_with_base();
        let before = store.total_bytes();
        let frame = vec![1u8, 2, 3, 255, 4, 5, 6, 255];
        store.transmit(&direct("a=f,i=1,f=32,s=2,v=1,X=1", &frame));
        assert_eq!(store.total_bytes(), before + frame.len());
    }

    #[test]
    fn set_byte_limit_evicts_immediately() {
        let mut store = ImageStore::new();
        for i in 1..=3u32 {
            store.transmit(&direct(&format!("a=t,f=32,s=1,v=1,i={i}"), &[0u8; 4]));
        }
        assert_eq!(store.len(), 3);
        // Shrink the budget below three images; the oldest are evicted.
        store.set_byte_limit(8);
        assert!(store.total_bytes() <= 8);
        assert!(store.len() < 3);
    }

    #[test]
    fn shared_memory_reads_and_unlinks() {
        // Create a real POSIX shm object, transfer via t=s, and confirm the
        // terminal unlinks it afterward. Skips gracefully when the sandbox
        // denies shm creation.
        use std::ffi::CString;
        let name = format!("/noa-kitty-shm-test-{}", std::process::id());
        let cname = CString::new(name.clone()).unwrap();
        let px = [11u8, 22, 33, 44]; // 1x1 RGBA
        // SAFETY: standard shm create/mmap/write sequence with checked returns.
        let created = unsafe {
            let fd = libc::shm_open(
                cname.as_ptr(),
                libc::O_CREAT | libc::O_RDWR | libc::O_EXCL,
                0o600,
            );
            if fd < 0 {
                None // sandbox or name clash — skip.
            } else if libc::ftruncate(fd, px.len() as libc::off_t) != 0 {
                libc::close(fd);
                let _ = libc::shm_unlink(cname.as_ptr());
                None
            } else {
                let ptr = libc::mmap(
                    std::ptr::null_mut(),
                    px.len(),
                    libc::PROT_WRITE,
                    libc::MAP_SHARED,
                    fd,
                    0,
                );
                libc::close(fd);
                if ptr == libc::MAP_FAILED {
                    let _ = libc::shm_unlink(cname.as_ptr());
                    None
                } else {
                    std::ptr::copy_nonoverlapping(px.as_ptr(), ptr as *mut u8, px.len());
                    libc::munmap(ptr, px.len());
                    Some(())
                }
            }
        };
        let Some(()) = created else {
            eprintln!("skipping shm test: shm_open denied (sandbox)");
            return;
        };

        let mut store = ImageStore::new();
        let cmd = direct("a=t,t=s,f=32,s=1,v=1,i=1", name.as_bytes());
        let TransmitStep::Done(done) = store.transmit(&cmd) else {
            // Clean up before asserting.
            unsafe {
                let _ = libc::shm_unlink(cname.as_ptr());
            }
            panic!("shm transfer must complete")
        };
        assert_eq!(done.result, Ok(1));
        assert_eq!(&*store.get(1).unwrap().rgba, &px);
        // The object must have been unlinked by the reader: a second open fails.
        let reopened = unsafe { libc::shm_open(cname.as_ptr(), libc::O_RDONLY, 0) };
        assert!(reopened < 0, "shm object must be unlinked after read");
        if reopened >= 0 {
            unsafe {
                libc::close(reopened);
                let _ = libc::shm_unlink(cname.as_ptr());
            }
        }
    }
}
