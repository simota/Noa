//! Kitty graphics protocol — control-data parser.
//!
//! The Kitty graphics protocol rides on APC strings: `ESC _ G <control> ; <payload> ST`.
//! [`crate::Stream`] hands the bytes after the leading `G` to [`parse`], which
//! decodes the comma-separated `key=value` control data into a
//! [`KittyGraphicsCommand`]. The base64 `payload` is kept verbatim — decoding
//! (and image storage / replies) is the grid layer's job.
//!
//! This mirrors `sgr.rs`: a pure function from bytes to a semantic struct, with
//! no terminal state. Unknown keys are ignored (forward compatibility, matching
//! Kitty); only a value of the wrong type sets [`KittyGraphicsCommand::parse_error`].
//!
//! Ghostty analog: `terminal/kitty/graphics_command.zig` (`Command.parse`).

/// The `a=` action selector.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum KittyAction {
    /// `a=t` — transmit image data only.
    #[default]
    Transmit,
    /// `a=T` — transmit and immediately display.
    TransmitAndDisplay,
    /// `a=p` — display (put) a previously transmitted image.
    Put,
    /// `a=d` — delete images / placements (see [`KittyGraphicsCommand::delete`]).
    Delete,
    /// `a=q` — query: validate without storing, reply with the outcome.
    Query,
    /// `a=f` — transmit an animation frame onto an existing image.
    TransmitFrame,
    /// `a=a` — control animation playback (state, current frame, loops, gaps).
    Animate,
    /// `a=c` — compose one frame's pixels onto another.
    Compose,
}

/// The `f=` pixel format of a raw (non-PNG) transfer.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum KittyFormat {
    /// `f=24` — packed RGB, 3 bytes/pixel.
    Rgb,
    /// `f=32` — packed RGBA, 4 bytes/pixel (the default).
    #[default]
    Rgba,
    /// `f=100` — PNG container.
    Png,
}

/// The `t=` transmission medium.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum KittyMedium {
    /// `t=d` — data is in the (base64) payload directly (the default).
    #[default]
    Direct,
    /// `t=f` — payload is a path to a regular file.
    File,
    /// `t=t` — payload is a path to a temporary file (deleted after read).
    TempFile,
    /// `t=s` — POSIX shared-memory object.
    SharedMem,
}

/// The `o=` payload compression.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum KittyCompression {
    /// `o=z` — zlib (RFC 1950) deflate stream.
    Zlib,
}

/// The `d=` delete specifier (only meaningful when `a=d`).
///
/// Each Kitty specifier comes in a lowercase/uppercase pair; the uppercase form
/// also frees the underlying image data once no placement references it. That is
/// captured here as `free: bool` (uppercase ⇒ `free == true`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum KittyDelete {
    /// `d=a`/`A` — all placements on the active screen.
    All { free: bool },
    /// `d=i`/`I` — by image id (`i=`), optionally a placement (`p=`).
    ById { free: bool },
    /// `d=n`/`N` — by image number (`I=`).
    ByNumber { free: bool },
    /// `d=c`/`C` — placements intersecting the cursor.
    AtCursor { free: bool },
    /// `d=f`/`F` — an image's animation frames (keeps the root frame).
    AnimationFrames { free: bool },
    /// `d=p`/`P` — placements at a cell (`x=`,`y=`).
    AtCell { free: bool },
    /// `d=q`/`Q` — placements at a cell and z-index (`x=`,`y=`,`z=`).
    AtCellZ { free: bool },
    /// `d=r`/`R` — by image id range (`x=`..`y=`).
    ByIdRange { free: bool },
    /// `d=x`/`X` — placements intersecting a column (`x=`).
    ByColumn { free: bool },
    /// `d=y`/`Y` — placements intersecting a row (`y=`).
    ByRow { free: bool },
    /// `d=z`/`Z` — placements at a z-index (`z=`).
    ByZ { free: bool },
}

/// A parsed Kitty graphics control-data command.
///
/// Fields carry Kitty's documented defaults when the corresponding key is
/// absent. [`parse_error`](Self::parse_error) is set on a type mismatch so the
/// grid layer can reply `EINVAL`; unknown keys never set it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct KittyGraphicsCommand {
    /// `a=` action (default [`KittyAction::Transmit`]).
    pub action: KittyAction,
    /// `f=` pixel format (default [`KittyFormat::Rgba`]).
    pub format: KittyFormat,
    /// `t=` transmission medium (default [`KittyMedium::Direct`]).
    pub medium: KittyMedium,
    /// `s=` — raw image width in pixels.
    pub width: u32,
    /// `v=` — raw image height in pixels.
    pub height: u32,
    /// `S=` — declared file size (file media).
    pub file_size: u32,
    /// `O=` — file read offset (file media).
    pub file_offset: u32,
    /// `o=` — payload compression, if any.
    pub compression: Option<KittyCompression>,
    /// `m=1` — more chunks follow.
    pub more_chunks: bool,
    /// `i=` — image id (0 = unspecified).
    pub image_id: u32,
    /// `I=` — image number (0 = unspecified).
    pub image_number: u32,
    /// `p=` — placement id (0 = unnamed).
    pub placement_id: u32,
    /// `q=` — quiet level: 0 all replies, 1 suppress OK, 2 suppress all.
    pub quiet: u8,
    /// `x=` — source crop x (pixels).
    pub src_x: u32,
    /// `y=` — source crop y (pixels).
    pub src_y: u32,
    /// `w=` — source crop width (pixels; 0 = full).
    pub src_w: u32,
    /// `h=` — source crop height (pixels; 0 = full).
    pub src_h: u32,
    /// `X=` — x pixel offset within the starting cell.
    pub cell_x_off: u32,
    /// `Y=` — y pixel offset within the starting cell.
    pub cell_y_off: u32,
    /// `c=` — number of columns to scale the image across (0 = natural).
    pub columns: u32,
    /// `r=` — number of rows to scale the image across (0 = natural).
    pub rows: u32,
    /// `z=` — z-index (signed).
    pub z_index: i32,
    /// `C=1` — do not move the cursor after display.
    pub cursor_no_move: bool,
    /// `U=1` — create a virtual placement (Unicode placeholder).
    pub virtual_placement: bool,
    /// `d=` specifier, resolved only when `a=d`.
    pub delete: Option<KittyDelete>,
    /// Raw (still base64-encoded) payload after the first `;`.
    pub payload: Vec<u8>,
    /// The APC exceeded the parser's capture limit.
    pub truncated: bool,
    /// A key carried a value of the wrong type (grid replies `EINVAL`).
    pub parse_error: bool,
}

impl KittyGraphicsCommand {
    fn empty(truncated: bool) -> Self {
        Self {
            action: KittyAction::default(),
            format: KittyFormat::default(),
            medium: KittyMedium::default(),
            width: 0,
            height: 0,
            file_size: 0,
            file_offset: 0,
            compression: None,
            more_chunks: false,
            image_id: 0,
            image_number: 0,
            placement_id: 0,
            quiet: 0,
            src_x: 0,
            src_y: 0,
            src_w: 0,
            src_h: 0,
            cell_x_off: 0,
            cell_y_off: 0,
            columns: 0,
            rows: 0,
            z_index: 0,
            cursor_no_move: false,
            virtual_placement: false,
            delete: None,
            payload: Vec::new(),
            truncated,
            parse_error: false,
        }
    }
}

/// A single ASCII byte value (e.g. `a=t`), or `None` if not exactly one byte.
fn single(v: &[u8]) -> Option<u8> {
    match v {
        [b] => Some(*b),
        _ => None,
    }
}

fn u32val(v: &[u8]) -> Option<u32> {
    std::str::from_utf8(v).ok()?.parse().ok()
}

fn i32val(v: &[u8]) -> Option<i32> {
    std::str::from_utf8(v).ok()?.parse().ok()
}

fn resolve_delete(c: u8) -> Option<KittyDelete> {
    let free = c.is_ascii_uppercase();
    Some(match c.to_ascii_lowercase() {
        b'a' => KittyDelete::All { free },
        b'i' => KittyDelete::ById { free },
        b'n' => KittyDelete::ByNumber { free },
        b'c' => KittyDelete::AtCursor { free },
        b'f' => KittyDelete::AnimationFrames { free },
        b'p' => KittyDelete::AtCell { free },
        b'q' => KittyDelete::AtCellZ { free },
        b'r' => KittyDelete::ByIdRange { free },
        b'x' => KittyDelete::ByColumn { free },
        b'y' => KittyDelete::ByRow { free },
        b'z' => KittyDelete::ByZ { free },
        _ => return None,
    })
}

/// Parse Kitty graphics control data (the bytes after the leading `G`).
///
/// `data` is `<control>[;<payload>]`; `truncated` is threaded from the APC
/// capture so an over-limit transfer still reaches the grid as `EFBIG`.
pub fn parse(data: &[u8], truncated: bool) -> KittyGraphicsCommand {
    let mut cmd = KittyGraphicsCommand::empty(truncated);

    let (ctrl, payload): (&[u8], &[u8]) = match data.iter().position(|&b| b == b';') {
        Some(i) => (&data[..i], &data[i + 1..]),
        None => (data, &[]),
    };
    cmd.payload = payload.to_vec();

    // The `d=` specifier is resolved after the loop, once `a=d` is known.
    let mut delete_char: Option<u8> = None;

    for pair in ctrl.split(|&b| b == b',') {
        if pair.is_empty() {
            continue;
        }
        let Some(eq) = pair.iter().position(|&b| b == b'=') else {
            continue; // malformed fragment without '=' — ignore
        };
        let key = &pair[..eq];
        let val = &pair[eq + 1..];
        let [key] = key else {
            continue; // multi-char / empty key — unknown, ignore
        };

        match key {
            b'a' => match single(val) {
                Some(b't') => cmd.action = KittyAction::Transmit,
                Some(b'T') => cmd.action = KittyAction::TransmitAndDisplay,
                Some(b'p') => cmd.action = KittyAction::Put,
                Some(b'd') => cmd.action = KittyAction::Delete,
                Some(b'q') => cmd.action = KittyAction::Query,
                Some(b'f') => cmd.action = KittyAction::TransmitFrame,
                Some(b'a') => cmd.action = KittyAction::Animate,
                Some(b'c') => cmd.action = KittyAction::Compose,
                _ => cmd.parse_error = true,
            },
            b'f' => match u32val(val) {
                Some(24) => cmd.format = KittyFormat::Rgb,
                Some(32) => cmd.format = KittyFormat::Rgba,
                Some(100) => cmd.format = KittyFormat::Png,
                _ => cmd.parse_error = true,
            },
            b't' => match single(val) {
                Some(b'd') => cmd.medium = KittyMedium::Direct,
                Some(b'f') => cmd.medium = KittyMedium::File,
                Some(b't') => cmd.medium = KittyMedium::TempFile,
                Some(b's') => cmd.medium = KittyMedium::SharedMem,
                _ => cmd.parse_error = true,
            },
            b'o' => match single(val) {
                Some(b'z') => cmd.compression = Some(KittyCompression::Zlib),
                _ => cmd.parse_error = true,
            },
            b'm' => match u32val(val) {
                Some(0) => cmd.more_chunks = false,
                Some(1) => cmd.more_chunks = true,
                _ => cmd.parse_error = true,
            },
            b'q' => match u32val(val) {
                Some(n @ 0..=2) => cmd.quiet = n as u8,
                _ => cmd.parse_error = true,
            },
            b'C' => match u32val(val) {
                Some(0) => cmd.cursor_no_move = false,
                Some(1) => cmd.cursor_no_move = true,
                _ => cmd.parse_error = true,
            },
            b'U' => match u32val(val) {
                Some(0) => cmd.virtual_placement = false,
                Some(1) => cmd.virtual_placement = true,
                _ => cmd.parse_error = true,
            },
            b'd' => match single(val) {
                Some(c) => delete_char = Some(c),
                None => cmd.parse_error = true,
            },
            b'z' => match i32val(val) {
                Some(n) => cmd.z_index = n,
                None => cmd.parse_error = true,
            },
            b's' => set_u32(&mut cmd.width, val, &mut cmd.parse_error),
            b'v' => set_u32(&mut cmd.height, val, &mut cmd.parse_error),
            b'S' => set_u32(&mut cmd.file_size, val, &mut cmd.parse_error),
            b'O' => set_u32(&mut cmd.file_offset, val, &mut cmd.parse_error),
            b'i' => set_u32(&mut cmd.image_id, val, &mut cmd.parse_error),
            b'I' => set_u32(&mut cmd.image_number, val, &mut cmd.parse_error),
            b'p' => set_u32(&mut cmd.placement_id, val, &mut cmd.parse_error),
            b'x' => set_u32(&mut cmd.src_x, val, &mut cmd.parse_error),
            b'y' => set_u32(&mut cmd.src_y, val, &mut cmd.parse_error),
            b'w' => set_u32(&mut cmd.src_w, val, &mut cmd.parse_error),
            b'h' => set_u32(&mut cmd.src_h, val, &mut cmd.parse_error),
            b'X' => set_u32(&mut cmd.cell_x_off, val, &mut cmd.parse_error),
            b'Y' => set_u32(&mut cmd.cell_y_off, val, &mut cmd.parse_error),
            b'c' => set_u32(&mut cmd.columns, val, &mut cmd.parse_error),
            b'r' => set_u32(&mut cmd.rows, val, &mut cmd.parse_error),
            _ => {} // unknown key — ignore (forward compat)
        }
    }

    if cmd.action == KittyAction::Delete {
        // Absent `d=` defaults to `a` (all placements on the active screen).
        match resolve_delete(delete_char.unwrap_or(b'a')) {
            Some(d) => cmd.delete = Some(d),
            None => cmd.parse_error = true,
        }
    }

    cmd
}

fn set_u32(field: &mut u32, val: &[u8], parse_error: &mut bool) {
    match u32val(val) {
        Some(n) => *field = n,
        None => *parse_error = true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_when_only_action() {
        let c = parse(b"a=q", false);
        assert_eq!(c.action, KittyAction::Query);
        assert_eq!(c.format, KittyFormat::Rgba);
        assert_eq!(c.medium, KittyMedium::Direct);
        assert_eq!(c.quiet, 0);
        assert_eq!(c.compression, None);
        assert!(!c.more_chunks);
        assert!(!c.parse_error);
        assert!(c.payload.is_empty());
    }

    #[test]
    fn empty_control_data_is_all_defaults() {
        let c = parse(b"", false);
        assert_eq!(c.action, KittyAction::Transmit);
        assert_eq!(c.format, KittyFormat::Rgba);
        assert!(!c.parse_error);
    }

    #[test]
    fn full_key_set_parses() {
        let c = parse(
            b"a=T,f=24,t=f,s=10,v=20,S=100,O=8,o=z,m=1,i=7,I=3,p=9,q=1,\
              x=1,y=2,w=3,h=4,X=5,Y=6,c=8,r=9,z=-2,C=1,U=1",
            false,
        );
        assert_eq!(c.action, KittyAction::TransmitAndDisplay);
        assert_eq!(c.format, KittyFormat::Rgb);
        assert_eq!(c.medium, KittyMedium::File);
        assert_eq!((c.width, c.height), (10, 20));
        assert_eq!((c.file_size, c.file_offset), (100, 8));
        assert_eq!(c.compression, Some(KittyCompression::Zlib));
        assert!(c.more_chunks);
        assert_eq!((c.image_id, c.image_number, c.placement_id), (7, 3, 9));
        assert_eq!(c.quiet, 1);
        assert_eq!((c.src_x, c.src_y, c.src_w, c.src_h), (1, 2, 3, 4));
        assert_eq!((c.cell_x_off, c.cell_y_off), (5, 6));
        assert_eq!((c.columns, c.rows), (8, 9));
        assert_eq!(c.z_index, -2);
        assert!(c.cursor_no_move);
        assert!(c.virtual_placement);
        assert!(!c.parse_error);
    }

    #[test]
    fn payload_split_on_first_semicolon() {
        let c = parse(b"a=T,f=100;iVBORw0KGgo=", false);
        assert_eq!(c.action, KittyAction::TransmitAndDisplay);
        assert_eq!(c.format, KittyFormat::Png);
        assert_eq!(c.payload, b"iVBORw0KGgo=");
        assert!(!c.parse_error);
    }

    #[test]
    fn payload_may_contain_semicolons() {
        // Only the first ';' splits; base64 never contains ';' but be robust.
        let c = parse(b"a=t;AA;BB", false);
        assert_eq!(c.payload, b"AA;BB");
    }

    #[test]
    fn unknown_key_is_ignored_not_error() {
        let c = parse(b"a=t,Q=99,zz=1", false);
        assert_eq!(c.action, KittyAction::Transmit);
        assert!(!c.parse_error, "unknown keys must not flag parse_error");
    }

    #[test]
    fn bad_value_flags_parse_error() {
        assert!(parse(b"i=notanumber", false).parse_error);
        assert!(parse(b"f=99", false).parse_error);
        assert!(parse(b"t=x", false).parse_error);
        assert!(parse(b"q=5", false).parse_error);
        assert!(parse(b"z=1.5", false).parse_error);
    }

    #[test]
    fn animation_actions_parse() {
        assert_eq!(parse(b"a=f", false).action, KittyAction::TransmitFrame);
        assert_eq!(parse(b"a=a", false).action, KittyAction::Animate);
        assert_eq!(parse(b"a=c", false).action, KittyAction::Compose);
    }

    #[test]
    fn delete_specifiers_lowercase_and_uppercase() {
        let cases: &[(&[u8], KittyDelete)] = &[
            (b"a=d", KittyDelete::All { free: false }), // default d=a
            (b"a=d,d=a", KittyDelete::All { free: false }),
            (b"a=d,d=A", KittyDelete::All { free: true }),
            (b"a=d,d=i", KittyDelete::ById { free: false }),
            (b"a=d,d=I", KittyDelete::ById { free: true }),
            (b"a=d,d=n", KittyDelete::ByNumber { free: false }),
            (b"a=d,d=N", KittyDelete::ByNumber { free: true }),
            (b"a=d,d=c", KittyDelete::AtCursor { free: false }),
            (b"a=d,d=C", KittyDelete::AtCursor { free: true }),
            (b"a=d,d=f", KittyDelete::AnimationFrames { free: false }),
            (b"a=d,d=F", KittyDelete::AnimationFrames { free: true }),
            (b"a=d,d=p", KittyDelete::AtCell { free: false }),
            (b"a=d,d=P", KittyDelete::AtCell { free: true }),
            (b"a=d,d=q", KittyDelete::AtCellZ { free: false }),
            (b"a=d,d=Q", KittyDelete::AtCellZ { free: true }),
            (b"a=d,d=r", KittyDelete::ByIdRange { free: false }),
            (b"a=d,d=R", KittyDelete::ByIdRange { free: true }),
            (b"a=d,d=x", KittyDelete::ByColumn { free: false }),
            (b"a=d,d=X", KittyDelete::ByColumn { free: true }),
            (b"a=d,d=y", KittyDelete::ByRow { free: false }),
            (b"a=d,d=Y", KittyDelete::ByRow { free: true }),
            (b"a=d,d=z", KittyDelete::ByZ { free: false }),
            (b"a=d,d=Z", KittyDelete::ByZ { free: true }),
        ];
        for (input, expected) in cases {
            let c = parse(input, false);
            assert_eq!(c.action, KittyAction::Delete, "input {input:?}");
            assert_eq!(c.delete, Some(*expected), "input {input:?}");
            assert!(!c.parse_error, "input {input:?}");
        }
    }

    #[test]
    fn delete_specifier_only_resolved_for_delete_action() {
        // d= present but a!=d ⇒ no delete resolution.
        let c = parse(b"a=t,d=i", false);
        assert_eq!(c.delete, None);
    }

    #[test]
    fn bad_delete_specifier_flags_error() {
        let c = parse(b"a=d,d=!", false);
        assert!(c.parse_error);
    }

    #[test]
    fn truncated_flag_threaded_through() {
        assert!(parse(b"a=T,f=100", true).truncated);
    }
}
