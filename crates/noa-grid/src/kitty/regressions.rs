use super::*;

fn command(ctrl: &str, bytes: &[u8]) -> KittyGraphicsCommand {
    let mut full = format!("{ctrl};").into_bytes();
    crate::osc::encode_base64(bytes, &mut full);
    noa_vt::kitty_graphics::parse(&full, false)
}

fn transmit(store: &mut ImageStore, ctrl: &str, bytes: &[u8]) -> Result<u32, KittyError> {
    match store.transmit(&command(ctrl, bytes)) {
        TransmitStep::Done(done) => done.result,
        TransmitStep::NeedMore => panic!("expected a complete transfer"),
    }
}

#[test]
fn temporary_deletion_requires_both_directory_and_marker() {
    let temp = std::fs::canonicalize(std::env::temp_dir()).unwrap();
    assert!(!is_temp_path(&temp.join("ordinary-file")));
    assert!(!is_temp_path(std::path::Path::new(
        "/not-a-temp-dir/tty-graphics-protocol-image"
    )));
    assert!(is_temp_path(&temp.join("tty-graphics-protocol-image")));
    if let Ok(tmp) = std::fs::canonicalize("/tmp") {
        assert!(is_temp_path(&tmp.join("tty-graphics-protocol-image")));
    }
}

#[test]
fn temporary_medium_preserves_an_ordinary_file_in_the_temp_directory() {
    use std::io::Write;
    let path = std::env::temp_dir().join(format!("noa-ordinary-image-{}", std::process::id()));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .unwrap();
    file.write_all(&[1, 2, 3, 4]).unwrap();
    let result = transmit(
        &mut ImageStore::new(),
        "a=t,t=t,f=32,s=1,v=1,i=1",
        path.to_str().unwrap().as_bytes(),
    );
    let remaining = std::fs::read(&path);
    std::fs::remove_file(&path).unwrap();
    assert_eq!(result, Err(KittyError::Invalid));
    assert_eq!(remaining.unwrap(), [1, 2, 3, 4]);
}

#[test]
fn failed_placements_still_obey_image_quota() {
    let mut terminal = crate::Terminal::new(noa_core::GridSize::new(20, 4));
    terminal.set_pixel_metrics(10, 20, 200, 80);
    terminal.set_kitty_image_limit(8);
    for id in 1..=4 {
        noa_vt::Handler::kitty_graphics(
            &mut terminal,
            command(&format!("a=T,f=32,s=1,v=1,i={id},x=1,w=0"), &[1, 2, 3, 4]),
        );
        assert!(terminal.kitty_images.total_bytes() <= 8);
        assert!(terminal.kitty_visible_placements().is_empty());
        assert!(
            terminal
                .pending_writes
                .ends_with(b"EINVAL:invalid request\x1b\\")
        );
    }
}

fn png(
    width: u32,
    height: u32,
    color: png::ColorType,
    depth: png::BitDepth,
    transparency: Option<&[u8]>,
    data: &[u8],
) -> Vec<u8> {
    let mut bytes = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut bytes, width, height);
        encoder.set_color(color);
        encoder.set_depth(depth);
        if let Some(trns) = transparency {
            encoder.set_trns(trns);
        }
        encoder
            .write_header()
            .unwrap()
            .write_image_data(data)
            .unwrap();
    }
    bytes
}

#[test]
fn png_expands_packed_grayscale_and_transparency() {
    let mut store = ImageStore::new();
    for (depth, packed, expected) in [
        (
            png::BitDepth::One,
            vec![0xaa],
            vec![255, 0, 255, 0, 255, 0, 255, 0],
        ),
        (
            png::BitDepth::Two,
            vec![0x1b, 0x1b],
            vec![0, 85, 170, 255, 0, 85, 170, 255],
        ),
        (
            png::BitDepth::Four,
            vec![0x0f; 4],
            vec![0, 255, 0, 255, 0, 255, 0, 255],
        ),
    ] {
        let bytes = png(8, 1, png::ColorType::Grayscale, depth, None, &packed);
        transmit(&mut store, "a=t,f=100,i=1", &bytes).unwrap();
        let actual: Vec<_> = store
            .get(1)
            .unwrap()
            .rgba
            .chunks_exact(4)
            .map(|p| p[0])
            .collect();
        assert_eq!(actual, expected);
    }
    let bytes = png(
        1,
        1,
        png::ColorType::Rgb,
        png::BitDepth::Eight,
        Some(&[0, 1, 0, 2, 0, 3]),
        &[1, 2, 3],
    );
    transmit(&mut store, "a=t,f=100,i=1", &bytes).unwrap();
    assert_eq!(&*store.get(1).unwrap().rgba, &[1, 2, 3, 0]);
    let bytes = png(
        1,
        1,
        png::ColorType::Grayscale,
        png::BitDepth::Eight,
        Some(&[0, 7]),
        &[7],
    );
    transmit(&mut store, "a=t,f=100,i=1", &bytes).unwrap();
    assert_eq!(&*store.get(1).unwrap().rgba, &[7, 7, 7, 0]);
}

#[test]
fn png_budget_is_checked_before_pixel_decoding() {
    let mut bytes = png(
        128,
        128,
        png::ColorType::Rgba,
        png::BitDepth::Eight,
        None,
        &vec![0; 128 * 128 * 4],
    );
    // Leave a valid IHDR and IDAT header, but corrupt the compressed pixels.
    // The dimensions alone must reject this image, before an IDAT decode error.
    bytes.truncate(41);
    let mut store = ImageStore::new();
    store.set_byte_limit(1024);
    assert_eq!(
        transmit(&mut store, "a=t,f=100,i=1", &bytes),
        Err(KittyError::TooBig)
    );
}

#[test]
fn png_expands_palette_colors_and_alpha() {
    let mut bytes = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut bytes, 2, 1);
        encoder.set_color(png::ColorType::Indexed);
        encoder.set_depth(png::BitDepth::One);
        encoder.set_palette(&[10, 20, 30, 40, 50, 60][..]);
        encoder.set_trns(&[0, 128][..]);
        encoder
            .write_header()
            .unwrap()
            .write_image_data(&[0x40])
            .unwrap();
    }
    let mut store = ImageStore::new();
    transmit(&mut store, "a=t,f=100,i=1", &bytes).unwrap();
    assert_eq!(
        &*store.get(1).unwrap().rgba,
        &[10, 20, 30, 0, 40, 50, 60, 128]
    );
}

#[test]
fn recreated_id_never_reuses_a_displayed_epoch() {
    let mut store = ImageStore::new();
    transmit(&mut store, "a=t,f=32,s=1,v=1,i=1", &[1; 4]).unwrap();
    transmit(&mut store, "a=t,f=32,s=1,v=1,i=1", &[2; 4]).unwrap();
    let previous = store.get(1).unwrap().epoch;
    store.remove(1);
    transmit(&mut store, "a=t,f=32,s=1,v=1,i=1", &[3; 4]).unwrap();
    assert_ne!(store.get(1).unwrap().epoch, previous);
    store.clear();
    transmit(&mut store, "a=t,f=32,s=1,v=1,i=1", &[4; 4]).unwrap();
    assert_ne!(store.get(1).unwrap().epoch, previous);
}

#[test]
fn image_indices_track_retransmission_numbers_and_removal() {
    let mut store = ImageStore::new();
    for id in 1..=3 {
        transmit(&mut store, &format!("a=t,f=32,s=1,v=1,i={id},I=9"), &[0; 4]).unwrap();
    }
    assert_eq!(store.get_by_number(9).unwrap().id, 3);
    transmit(&mut store, "a=t,f=32,s=1,v=1,i=1,I=8", &[1; 4]).unwrap();
    assert_eq!(store.ids_with_number(9), vec![2, 3]);
    store.remove(3);
    assert_eq!(store.get_by_number(9).unwrap().id, 2);
    store.set_byte_limit(4);
    assert!(store.get_by_number(9).is_none());
    assert_eq!(store.get_by_number(8).unwrap().id, 1);
    store.clear();
    assert!(store.get_by_number(8).is_none());
}

#[test]
fn tiny_images_and_frames_have_independent_metadata_caps() {
    let mut store = ImageStore::new();
    for id in 1..=MAX_STORED_IMAGES {
        transmit(&mut store, &format!("a=t,f=32,s=1,v=1,i={id}"), &[0; 4]).unwrap();
    }
    assert_eq!(
        transmit(&mut store, "a=t,f=32,s=1,v=1,i=9000", &[0; 4]),
        Err(KittyError::TooBig)
    );
    store.remove(1);
    transmit(&mut store, "a=t,f=32,s=1,v=1,i=9000", &[0; 4]).unwrap();
    assert_eq!(store.len(), MAX_STORED_IMAGES);
    assert!(!store.has_running_animation());
    store.clear();
    transmit(&mut store, "a=t,f=32,s=1,v=1,i=1", &[0; 4]).unwrap();
    for _ in 1..MAX_STORED_FRAMES {
        transmit(&mut store, "a=f,f=32,s=1,v=1,i=1", &[0; 4]).unwrap();
    }
    assert_eq!(
        transmit(&mut store, "a=f,f=32,s=1,v=1,i=1", &[0; 4]),
        Err(KittyError::TooBig)
    );
    assert!(store.delete_frames(1));
    transmit(&mut store, "a=f,f=32,s=1,v=1,i=1", &[1; 4]).unwrap();
}

#[cfg(unix)]
struct SharedMemory {
    name: std::ffi::CString,
    fd: std::os::fd::OwnedFd,
}

#[cfg(unix)]
impl SharedMemory {
    fn new(len: usize) -> Self {
        use std::os::fd::{AsRawFd, FromRawFd};
        static ID: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let id = ID.fetch_add(1, Ordering::Relaxed);
        let name = std::ffi::CString::new(format!("/noarg{}-{id}", std::process::id())).unwrap();
        // SAFETY: this test creates an exclusive object and only maps its own allocation.
        unsafe {
            let fd = libc::shm_open(
                name.as_ptr(),
                libc::O_CREAT | libc::O_EXCL | libc::O_RDWR,
                0o600,
            );
            assert!(fd >= 0, "{}", std::io::Error::last_os_error());
            let shm = Self {
                name,
                fd: std::os::fd::OwnedFd::from_raw_fd(fd),
            };
            assert_eq!(libc::ftruncate(shm.fd.as_raw_fd(), len as libc::off_t), 0);
            let p = libc::mmap(
                std::ptr::null_mut(),
                len,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                fd,
                0,
            );
            assert_ne!(p, libc::MAP_FAILED);
            std::ptr::write_bytes(p, 7, len);
            assert_eq!(libc::munmap(p, len), 0);
            shm
        }
    }
}

#[cfg(unix)]
impl Drop for SharedMemory {
    fn drop(&mut self) {
        unsafe {
            libc::shm_unlink(self.name.as_ptr());
        }
    }
}

#[test]
#[cfg(unix)]
fn shared_memory_size_is_a_length_not_an_end_offset() {
    let shm = SharedMemory::new(16384);
    let mut store = ImageStore::new();
    assert_eq!(
        transmit(
            &mut store,
            "a=t,t=s,f=32,s=2,v=1,i=1,S=8,O=4",
            shm.name.as_bytes()
        ),
        Ok(1)
    );
    assert_eq!(&*store.get(1).unwrap().rgba, &[7; 8]);
}

#[test]
#[cfg(unix)]
fn shared_memory_rejects_ranges_past_the_object() {
    let shm = SharedMemory::new(16384);
    assert_eq!(
        transmit(
            &mut ImageStore::new(),
            "a=t,t=s,f=32,s=2,v=1,i=1,S=8,O=16380",
            shm.name.as_bytes()
        ),
        Err(KittyError::NoData)
    );
}

#[test]
#[cfg(all(unix, not(target_os = "macos")))]
fn shrinking_shared_memory_after_stat_returns_a_read_error() {
    use std::os::fd::AsRawFd;
    let shm = SharedMemory::new(4096);
    assert_eq!(unsafe { libc::ftruncate(shm.fd.as_raw_fd(), 0) }, 0);
    assert_eq!(
        read_shared_range(shm.fd.try_clone().unwrap(), 0, 4096),
        Err(KittyError::NoData)
    );
}
