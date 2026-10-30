//! Clipboard access kept at the app/platform boundary.

use std::path::PathBuf;
use std::sync::Arc;

use noa_grid::kitty_clipboard::ClipboardLocation;

#[derive(Debug, Default)]
pub(crate) struct SystemClipboard;

impl SystemClipboard {
    pub(crate) fn new() -> Self {
        Self
    }

    pub(crate) fn set_text(&mut self, text: &str) -> anyhow::Result<()> {
        platform::set_text(text)
    }

    /// Plain-text clipboard contents, for an OSC 52 read reply.
    pub(crate) fn get_text(&mut self) -> anyhow::Result<String> {
        platform::get_text()
    }

    /// Writes the X11/Wayland PRIMARY selection (`copy-on-select`). macOS
    /// has no primary selection, so this is a no-op there.
    pub(crate) fn set_primary_text(&mut self, text: &str) -> anyhow::Result<()> {
        platform::set_primary_text(text)
    }

    /// Reads the PRIMARY selection, for middle-click paste. Empty on macOS.
    pub(crate) fn get_primary_text(&mut self) -> anyhow::Result<String> {
        platform::get_primary_text()
    }

    /// Reads the clipboard the way Ghostty's paste command does: file URLs
    /// and images take priority over plain text, so pasting a Finder
    /// selection or a copied screenshot inserts a shell-escaped path instead
    /// of raw pasteboard data.
    pub(crate) fn get_paste_contents(&mut self) -> anyhow::Result<PasteContents> {
        platform::get_paste_contents()
    }

    /// Whether this platform has the clipboard at `location` (macOS has no
    /// primary selection).
    pub(crate) fn supports(&self, location: ClipboardLocation) -> bool {
        platform::supports(location)
    }

    /// The MIME types `location` holds (OSC 5522's `.` listing), without
    /// reading any data. `None` when this platform has no such clipboard.
    pub(crate) fn available_mimes(
        &mut self,
        location: ClipboardLocation,
    ) -> anyhow::Result<Option<Vec<String>>> {
        platform::available_mimes(location)
    }

    /// The data for each MIME type in `mimes` (`None` where unavailable);
    /// every text alias reads the clipboard's text. `None` when this platform
    /// has no such clipboard.
    pub(crate) fn read_mimes(
        &mut self,
        location: ClipboardLocation,
        mimes: &[String],
    ) -> anyhow::Result<Option<Vec<Option<Vec<u8>>>>> {
        platform::read_mimes(location, mimes)
    }

    /// Replace `location`'s contents with these representations; an empty
    /// list clears it. `None` when this platform has no such clipboard.
    pub(crate) fn write_mimes(
        &mut self,
        location: ClipboardLocation,
        contents: &[(String, Arc<[u8]>)],
    ) -> anyhow::Result<Option<()>> {
        platform::write_mimes(location, contents)
    }
}

/// A `file://` URL for an absolute path, percent-encoding everything but
/// unreserved characters and `/`.
#[cfg(unix)]
#[cfg_attr(target_os = "macos", allow(dead_code))]
fn file_url(path: &std::path::Path) -> String {
    use std::os::unix::ffi::OsStrExt;
    let mut url = String::from("file://");
    for &b in path.as_os_str().as_bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~' | b'/') {
            url.push(char::from(b));
        } else {
            url.push_str(&format!("%{b:02X}"));
        }
    }
    url
}

/// `text/plain` stands in for every text alias (Ghostty's apprt is always
/// asked for `text/plain`).
fn canonical_mime(mime: &str) -> &str {
    if noa_grid::kitty_clipboard::is_text_mime(mime) {
        "text/plain"
    } else {
        mime
    }
}

/// What was found on the clipboard for a paste, in priority order.
#[derive(Debug, PartialEq)]
pub(crate) enum PasteContents {
    FileUrls(Vec<PathBuf>),
    /// PNG-encoded image bytes.
    Image(Vec<u8>),
    Text(String),
    Empty,
}

fn non_empty_text(text: String) -> Option<String> {
    (!text.is_empty()).then_some(text)
}

fn paste_contents_from_parts(
    file_urls: Vec<PathBuf>,
    image: Option<Vec<u8>>,
    text: Option<String>,
) -> PasteContents {
    if !file_urls.is_empty() {
        PasteContents::FileUrls(file_urls)
    } else if let Some(image) = image {
        PasteContents::Image(image)
    } else if let Some(text) = text.and_then(non_empty_text) {
        PasteContents::Text(text)
    } else {
        PasteContents::Empty
    }
}

/// Shell-escapes a string for use as a single word on a POSIX command line:
/// single-quotes it, escaping embedded single quotes as `'\''`. Strings made
/// up only of characters that are never special to a shell are left as-is.
pub(crate) fn shell_escape(s: &str) -> String {
    let is_safe = !s.is_empty()
        && s.bytes().all(|b| {
            b.is_ascii_alphanumeric()
                || matches!(
                    b,
                    b'-' | b'_' | b'.' | b'/' | b':' | b'@' | b'%' | b'+' | b'='
                )
        });
    if is_safe {
        return s.to_string();
    }

    let mut escaped = String::with_capacity(s.len() + 2);
    escaped.push('\'');
    for c in s.chars() {
        if c == '\'' {
            escaped.push_str("'\\''");
        } else {
            escaped.push(c);
        }
    }
    escaped.push('\'');
    escaped
}

/// Joins shell-escaped paths with spaces, the format `encode_paste` expects
/// for a multi-file paste.
pub(crate) fn file_urls_to_paste_string(paths: &[PathBuf]) -> String {
    paths
        .iter()
        .map(|path| shell_escape(&path.to_string_lossy()))
        .collect::<Vec<_>>()
        .join(" ")
}

/// How long a pasted-image temp file survives before [`write_temp_png`]'s
/// once-per-process sweep reclaims it. Generous because the pasted *path* may
/// still be referenced by a long-running program; without any sweep the files
/// accumulate forever on platforms whose temp dir is never cleaned.
const TEMP_PNG_MAX_AGE: std::time::Duration = std::time::Duration::from_secs(48 * 60 * 60);

/// Saves `png_bytes` to a fresh file in the system temp dir and returns its
/// path. Used to give pasted clipboard images a real path to paste, matching
/// Ghostty's paste-image-as-path behavior. The first write of the process
/// also sweeps stale `noa-paste-*` files left behind by earlier runs.
pub(crate) fn write_temp_png(png_bytes: &[u8]) -> anyhow::Result<PathBuf> {
    use std::time::{SystemTime, UNIX_EPOCH};

    static PRUNE_ONCE: std::sync::Once = std::sync::Once::new();
    PRUNE_ONCE.call_once(|| prune_stale_temp_pngs(&std::env::temp_dir(), SystemTime::now()));

    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    let path = std::env::temp_dir().join(format!("noa-paste-{}-{nanos}.png", std::process::id()));
    std::fs::write(&path, png_bytes)?;
    Ok(path)
}

/// Remove `noa-paste-*.png` files in `dir` older than [`TEMP_PNG_MAX_AGE`].
/// Best-effort: an unreadable dir or a losing race with another noa process
/// deleting the same file is silently ignored.
fn prune_stale_temp_pngs(dir: &std::path::Path, now: std::time::SystemTime) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if !name.starts_with("noa-paste-") || !name.ends_with(".png") {
            continue;
        }
        let stale = entry
            .metadata()
            .and_then(|meta| meta.modified())
            .ok()
            .and_then(|modified| now.duration_since(modified).ok())
            .is_some_and(|age| age > TEMP_PNG_MAX_AGE);
        if stale {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use std::path::PathBuf;
    use std::process::Command;

    use anyhow::{Context, ensure};
    use objc2::msg_send;
    use objc2::rc::Retained;
    use objc2::runtime::{AnyClass, AnyObject};
    use objc2_app_kit::{
        NSBitmapImageFileType, NSBitmapImageRep, NSPasteboard, NSPasteboardTypeFileURL,
        NSPasteboardTypePNG, NSPasteboardTypeString, NSPasteboardTypeTIFF,
    };
    use objc2_foundation::{NSArray, NSCopying, NSData, NSDictionary, NSString, NSURL};

    use super::{Arc, ClipboardLocation, PasteContents};

    // `UTType` is looked up at runtime; make sure its framework is loaded.
    #[link(name = "UniformTypeIdentifiers", kind = "framework")]
    unsafe extern "C" {}

    fn ut_type_class() -> Option<&'static AnyClass> {
        AnyClass::get(c"UTType")
    }

    /// `UTType(identifier).preferredMIMEType`, folding
    /// `text/plain;charset=utf-8` into `text/plain`.
    fn mime_for_pasteboard_type(pasteboard_type: &NSString) -> Option<String> {
        let class = ut_type_class()?;
        let ut: Option<Retained<AnyObject>> =
            unsafe { msg_send![class, typeWithIdentifier: pasteboard_type] };
        let mime: Option<Retained<NSString>> = unsafe { msg_send![&*ut?, preferredMIMEType] };
        let mime = mime?.to_string();
        Some(if mime == "text/plain;charset=utf-8" {
            "text/plain".to_string()
        } else {
            mime
        })
    }

    /// The pasteboard type for a MIME type, as Ghostty maps it: `text/plain`
    /// is the string type, others go through `UTType(mimeType:)`, and an
    /// unknown MIME type is used as the identifier itself.
    fn pasteboard_type_for_mime(mime: &str) -> Retained<NSString> {
        if mime == "text/plain" {
            return unsafe { NSPasteboardTypeString }.copy();
        }
        let mime = NSString::from_str(mime);
        if let Some(class) = ut_type_class() {
            let ut: Option<Retained<AnyObject>> =
                unsafe { msg_send![class, typeWithMIMEType: &*mime] };
            if let Some(ut) = ut {
                let identifier: Retained<NSString> = unsafe { msg_send![&*ut, identifier] };
                return identifier;
            }
        }
        mime
    }

    pub(super) fn supports(location: ClipboardLocation) -> bool {
        location == ClipboardLocation::Standard
    }

    /// The standard pasteboard; macOS has no primary selection.
    fn pasteboard(location: ClipboardLocation) -> Option<Retained<NSPasteboard>> {
        (location == ClipboardLocation::Standard).then(NSPasteboard::generalPasteboard)
    }

    pub(super) fn available_mimes(
        location: ClipboardLocation,
    ) -> anyhow::Result<Option<Vec<String>>> {
        let Some(pasteboard) = pasteboard(location) else {
            return Ok(None);
        };
        let types: Vec<Retained<NSString>> =
            pasteboard.types().map(|t| t.to_vec()).unwrap_or_default();
        let file_url = unsafe { NSPasteboardTypeFileURL };
        let has_file_url = types.iter().any(|t| t.isEqualToString(file_url));
        let mimes: Vec<Option<String>> =
            types.iter().map(|t| mime_for_pasteboard_type(t)).collect();
        let mut result: Vec<String> = Vec::new();
        // Copied files serve as text (their escaped paths) and as a URI list;
        // both are inferred from the declared types without reading data.
        if has_file_url || mimes.iter().flatten().any(|m| m == "text/plain") {
            result.push("text/plain".to_string());
        }
        if has_file_url {
            result.push("text/uri-list".to_string());
        }
        for mime in mimes.into_iter().flatten() {
            if !result.contains(&mime) {
                result.push(mime);
            }
        }
        Ok(Some(result))
    }

    pub(super) fn read_mimes(
        location: ClipboardLocation,
        mimes: &[String],
    ) -> anyhow::Result<Option<Vec<Option<Vec<u8>>>>> {
        let Some(pasteboard) = pasteboard(location) else {
            return Ok(None);
        };
        Ok(Some(
            mimes
                .iter()
                .map(|mime| data_for_mime(&pasteboard, super::canonical_mime(mime)))
                .collect(),
        ))
    }

    /// Ghostty's `ghosttyData(forMime:)`: text is the paste text (a copied
    /// file's escaped path), copied files are also a `text/uri-list`.
    fn data_for_mime(pasteboard: &NSPasteboard, mime: &str) -> Option<Vec<u8>> {
        match mime {
            "text/plain" => opinionated_text(pasteboard).map(String::into_bytes),
            "text/uri-list" => {
                let urls = file_url_strings(pasteboard);
                (!urls.is_empty()).then(|| {
                    urls.iter()
                        .map(|url| format!("{url}\r\n"))
                        .collect::<String>()
                        .into_bytes()
                })
            }
            _ => pasteboard
                .dataForType(&pasteboard_type_for_mime(mime))
                .map(|data| data.to_vec()),
        }
    }

    /// Each item's file path (shell-escaped) or string, joined by spaces.
    fn opinionated_text(pasteboard: &NSPasteboard) -> Option<String> {
        let items = pasteboard.pasteboardItems()?;
        let strings: Vec<String> = items
            .iter()
            .filter_map(|item| {
                let file = item
                    .stringForType(unsafe { NSPasteboardTypeFileURL })
                    .and_then(|url| NSURL::URLWithString(&url))
                    .filter(|url| url.isFileURL())
                    .and_then(|url| url.path());
                match file {
                    Some(path) => Some(super::shell_escape(&path.to_string())),
                    None => item
                        .stringForType(unsafe { NSPasteboardTypeString })
                        .map(|s| s.to_string()),
                }
            })
            .collect();
        (!strings.is_empty()).then(|| strings.join(" "))
    }

    fn file_url_strings(pasteboard: &NSPasteboard) -> Vec<String> {
        let Some(items) = pasteboard.pasteboardItems() else {
            return Vec::new();
        };
        items
            .iter()
            .filter_map(|item| item.stringForType(unsafe { NSPasteboardTypeFileURL }))
            .filter_map(|url| NSURL::URLWithString(&url))
            .filter(|url| url.isFileURL())
            .filter_map(|url| url.absoluteString())
            .map(|url| url.to_string())
            .collect()
    }

    pub(super) fn write_mimes(
        location: ClipboardLocation,
        contents: &[(String, Arc<[u8]>)],
    ) -> anyhow::Result<Option<()>> {
        let Some(pasteboard) = pasteboard(location) else {
            return Ok(None);
        };
        // An empty write clears the clipboard with one empty text entry.
        let empty = [("text/plain".to_string(), Arc::<[u8]>::from(&[][..]))];
        let contents = if contents.is_empty() {
            &empty[..]
        } else {
            contents
        };
        let types: Vec<Retained<NSString>> = contents
            .iter()
            .map(|(mime, _)| pasteboard_type_for_mime(mime))
            .collect();
        let refs: Vec<&NSString> = types.iter().map(|t| &**t).collect();
        unsafe { pasteboard.declareTypes_owner(&NSArray::from_slice(&refs), None) };
        for (pasteboard_type, (_, data)) in types.iter().zip(contents) {
            let ok = pasteboard.setData_forType(Some(&NSData::with_bytes(data)), pasteboard_type);
            ensure!(
                ok,
                "failed to write {pasteboard_type} to the macOS clipboard"
            );
        }
        Ok(Some(()))
    }

    pub(super) fn get_text() -> anyhow::Result<String> {
        let pasteboard = NSPasteboard::generalPasteboard();
        if let Some(text) = pasteboard_text(&pasteboard) {
            return Ok(text);
        }

        pbpaste_text()
    }

    fn pbpaste_text() -> anyhow::Result<String> {
        // pbpaste converts through the locale encoding; without LC_CTYPE
        // (e.g. launched from Finder, where LANG is unset) UTF-8 output is
        // mojibake'd, so pin it.
        let output = Command::new("/usr/bin/pbpaste")
            .arg("-Prefer")
            .arg("txt")
            .env("LC_CTYPE", "UTF-8")
            .output()
            .context("failed to read macOS clipboard")?;
        ensure!(
            output.status.success(),
            "pbpaste exited with status {}",
            output.status
        );

        String::from_utf8(output.stdout).context("clipboard text was not valid UTF-8")
    }

    pub(super) fn set_text(text: &str) -> anyhow::Result<()> {
        // Write through NSPasteboard directly rather than pbcopy: pbcopy
        // decodes stdin via the locale encoding, so without LC_CTYPE (e.g.
        // launched from Finder, where LANG is unset) multibyte UTF-8 lands
        // on the clipboard mojibake'd.
        let pasteboard = NSPasteboard::generalPasteboard();
        pasteboard.clearContents();
        let ok = pasteboard
            .setString_forType(&NSString::from_str(text), unsafe { NSPasteboardTypeString });
        ensure!(ok, "failed to write text to macOS clipboard");
        Ok(())
    }

    pub(super) fn get_primary_text() -> anyhow::Result<String> {
        Ok(String::new())
    }

    pub(super) fn set_primary_text(_text: &str) -> anyhow::Result<()> {
        Ok(())
    }

    pub(super) fn get_paste_contents() -> anyhow::Result<PasteContents> {
        let pasteboard = NSPasteboard::generalPasteboard();

        let file_urls = file_urls(&pasteboard);
        if !file_urls.is_empty() {
            return Ok(super::paste_contents_from_parts(file_urls, None, None));
        }

        if let Some(png) = image_png(&pasteboard) {
            return Ok(super::paste_contents_from_parts(
                Vec::new(),
                Some(png),
                None,
            ));
        }

        let text = match pasteboard_text(&pasteboard) {
            Some(text) => Some(text),
            None => Some(pbpaste_text()?),
        };
        Ok(super::paste_contents_from_parts(Vec::new(), None, text))
    }

    fn pasteboard_text(pasteboard: &NSPasteboard) -> Option<String> {
        pasteboard
            .stringForType(unsafe { NSPasteboardTypeString })
            .map(|text| text.to_string())
            .and_then(super::non_empty_text)
    }

    fn file_urls(pasteboard: &NSPasteboard) -> Vec<PathBuf> {
        let Some(items) = pasteboard.pasteboardItems() else {
            return Vec::new();
        };
        items
            .iter()
            .filter_map(|item| item.stringForType(unsafe { NSPasteboardTypeFileURL }))
            .filter_map(|url_string| NSURL::URLWithString(&url_string))
            .filter_map(|url| url.path())
            .map(|path| PathBuf::from(path.to_string()))
            .collect()
    }

    fn image_png(pasteboard: &NSPasteboard) -> Option<Vec<u8>> {
        if let Some(data) = pasteboard.dataForType(unsafe { NSPasteboardTypePNG }) {
            return Some(data.to_vec());
        }

        let tiff = pasteboard.dataForType(unsafe { NSPasteboardTypeTIFF })?;
        let bitmap = NSBitmapImageRep::imageRepWithData(&tiff)?;
        let properties = NSDictionary::new();
        let png = unsafe {
            bitmap.representationUsingType_properties(NSBitmapImageFileType::PNG, &properties)
        }?;
        Some(png.to_vec())
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn mime_types_map_to_pasteboard_types_through_uttype() {
            assert_eq!(
                pasteboard_type_for_mime("text/plain").to_string(),
                "public.utf8-plain-text"
            );
            assert_eq!(
                pasteboard_type_for_mime("image/png").to_string(),
                "public.png"
            );
            assert_eq!(
                mime_for_pasteboard_type(&NSString::from_str("public.png")).as_deref(),
                Some("image/png")
            );
            assert_eq!(
                mime_for_pasteboard_type(&NSString::from_str("public.utf8-plain-text")).as_deref(),
                Some("text/plain")
            );
            assert_eq!(
                mime_for_pasteboard_type(&NSString::from_str("com.example.no-such-type")),
                None
            );
        }
    }
}

/// X11 / Wayland through `arboard`. One process-wide handle is kept alive:
/// on X11 the clipboard owner must keep serving the selection after a copy,
/// and arboard does that from a thread tied to the handle's lifetime.
#[cfg(all(unix, not(target_os = "macos")))]
mod platform {
    use std::path::PathBuf;
    use std::sync::{Mutex, MutexGuard};

    use anyhow::Context;
    use arboard::{Clipboard, GetExtLinux, LinuxClipboardKind, SetExtLinux};

    use super::{Arc, ClipboardLocation, PasteContents};

    fn kind(location: ClipboardLocation) -> LinuxClipboardKind {
        match location {
            ClipboardLocation::Standard => LinuxClipboardKind::Clipboard,
            ClipboardLocation::Primary => LinuxClipboardKind::Primary,
        }
    }

    fn text(
        clipboard: &mut Clipboard,
        location: ClipboardLocation,
    ) -> Result<String, arboard::Error> {
        text_or_empty(clipboard.get().clipboard(kind(location)).text())
    }

    /// Copied files, as `file://` URLs.
    fn file_urls(clipboard: &mut Clipboard, location: ClipboardLocation) -> Vec<String> {
        clipboard
            .get()
            .clipboard(kind(location))
            .file_list()
            .unwrap_or_default()
            .iter()
            .map(|path| super::file_url(path))
            .collect()
    }

    pub(super) fn supports(_location: ClipboardLocation) -> bool {
        true
    }

    // arboard reaches text and file lists only, so those are the types served.
    pub(super) fn available_mimes(
        location: ClipboardLocation,
    ) -> anyhow::Result<Option<Vec<String>>> {
        with_clipboard(|clipboard| {
            let mut mimes = Vec::new();
            if !text(clipboard, location)?.is_empty() {
                mimes.push("text/plain".to_string());
            }
            if !file_urls(clipboard, location).is_empty() {
                mimes.push("text/uri-list".to_string());
            }
            Ok(Some(mimes))
        })
    }

    pub(super) fn read_mimes(
        location: ClipboardLocation,
        mimes: &[String],
    ) -> anyhow::Result<Option<Vec<Option<Vec<u8>>>>> {
        with_clipboard(|clipboard| {
            let mut out = Vec::with_capacity(mimes.len());
            for mime in mimes {
                out.push(match super::canonical_mime(mime) {
                    "text/plain" => {
                        Some(text(clipboard, location)?.into_bytes()).filter(|d| !d.is_empty())
                    }
                    "text/uri-list" => {
                        let urls = file_urls(clipboard, location);
                        (!urls.is_empty()).then(|| {
                            urls.iter()
                                .map(|url| format!("{url}\r\n"))
                                .collect::<String>()
                                .into_bytes()
                        })
                    }
                    _ => None,
                });
            }
            Ok(Some(out))
        })
    }

    /// Only text is written here: the first text representation is kept and
    /// the others are dropped. A write with no text at all (but not an empty
    /// one, which clears) is unsupported, leaving the clipboard untouched.
    pub(super) fn write_mimes(
        location: ClipboardLocation,
        contents: &[(String, Arc<[u8]>)],
    ) -> anyhow::Result<Option<()>> {
        let text = contents
            .iter()
            .find(|(mime, _)| noa_grid::kitty_clipboard::is_text_mime(mime))
            .map(|(_, data)| String::from_utf8_lossy(data).into_owned());
        let text = match text {
            Some(text) => text,
            None if contents.is_empty() => String::new(),
            None => return Ok(None),
        };
        with_clipboard(|clipboard| clipboard.set().clipboard(kind(location)).text(text))?;
        Ok(Some(()))
    }

    static CLIPBOARD: Mutex<Option<Clipboard>> = Mutex::new(None);

    fn with_clipboard<T>(
        f: impl FnOnce(&mut Clipboard) -> Result<T, arboard::Error>,
    ) -> anyhow::Result<T> {
        let mut guard: MutexGuard<'_, Option<Clipboard>> = CLIPBOARD
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if guard.is_none() {
            *guard = Some(Clipboard::new().context("failed to open the system clipboard")?);
        }
        let clipboard = guard.as_mut().expect("clipboard initialized above");
        f(clipboard).context("clipboard access failed")
    }

    /// An empty selection is not an error for a read: it pastes nothing.
    fn text_or_empty(result: Result<String, arboard::Error>) -> Result<String, arboard::Error> {
        match result {
            Err(arboard::Error::ContentNotAvailable) => Ok(String::new()),
            other => other,
        }
    }

    pub(super) fn get_text() -> anyhow::Result<String> {
        with_clipboard(|clipboard| text_or_empty(clipboard.get_text()))
    }

    pub(super) fn set_text(text: &str) -> anyhow::Result<()> {
        with_clipboard(|clipboard| clipboard.set_text(text))
    }

    pub(super) fn get_primary_text() -> anyhow::Result<String> {
        with_clipboard(|clipboard| {
            text_or_empty(
                clipboard
                    .get()
                    .clipboard(LinuxClipboardKind::Primary)
                    .text(),
            )
        })
    }

    pub(super) fn set_primary_text(text: &str) -> anyhow::Result<()> {
        with_clipboard(|clipboard| {
            clipboard
                .set()
                .clipboard(LinuxClipboardKind::Primary)
                .text(text)
        })
    }

    pub(super) fn get_paste_contents() -> anyhow::Result<PasteContents> {
        with_clipboard(|clipboard| {
            // A file-manager copy publishes `text/uri-list`; prefer it so the
            // paste inserts shell-escaped paths, as on macOS.
            let file_urls: Vec<PathBuf> = clipboard.get().file_list().unwrap_or_default();
            let text = text_or_empty(clipboard.get_text())?;
            Ok(super::paste_contents_from_parts(
                file_urls,
                None,
                Some(text),
            ))
        })
    }
}

#[cfg(not(unix))]
mod platform {
    pub(super) fn get_text() -> anyhow::Result<String> {
        anyhow::bail!("clipboard is not implemented on this platform")
    }

    pub(super) fn set_text(_text: &str) -> anyhow::Result<()> {
        anyhow::bail!("clipboard is not implemented on this platform")
    }

    pub(super) fn get_primary_text() -> anyhow::Result<String> {
        anyhow::bail!("clipboard is not implemented on this platform")
    }

    pub(super) fn set_primary_text(_text: &str) -> anyhow::Result<()> {
        anyhow::bail!("clipboard is not implemented on this platform")
    }

    pub(super) fn get_paste_contents() -> anyhow::Result<super::PasteContents> {
        anyhow::bail!("clipboard is not implemented on this platform")
    }

    pub(super) fn supports(_location: super::ClipboardLocation) -> bool {
        false
    }

    pub(super) fn available_mimes(
        _location: super::ClipboardLocation,
    ) -> anyhow::Result<Option<Vec<String>>> {
        Ok(None)
    }

    pub(super) fn read_mimes(
        _location: super::ClipboardLocation,
        _mimes: &[String],
    ) -> anyhow::Result<Option<Vec<Option<Vec<u8>>>>> {
        Ok(None)
    }

    pub(super) fn write_mimes(
        _location: super::ClipboardLocation,
        _contents: &[(String, super::Arc<[u8]>)],
    ) -> anyhow::Result<Option<()>> {
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn file_url_percent_encodes_the_path() {
        assert_eq!(
            file_url(std::path::Path::new("/tmp/a b/日.txt")),
            "file:///tmp/a%20b/%E6%97%A5.txt"
        );
    }

    #[test]
    fn text_aliases_read_as_text_plain() {
        for mime in [
            "text/plain",
            "text/plain;charset=utf-8",
            "UTF8_STRING",
            "TEXT",
            "STRING",
        ] {
            assert_eq!(canonical_mime(mime), "text/plain");
        }
        assert_eq!(canonical_mime("image/png"), "image/png");
    }

    #[test]
    fn shell_escape_leaves_plain_tokens_unescaped() {
        assert_eq!(shell_escape("/tmp/plain-file_1.0"), "/tmp/plain-file_1.0");
    }

    #[test]
    fn shell_escape_quotes_spaces() {
        assert_eq!(shell_escape("/tmp/my file.txt"), "'/tmp/my file.txt'");
    }

    #[test]
    fn shell_escape_escapes_embedded_single_quotes() {
        assert_eq!(shell_escape("it's/a/path"), "'it'\\''s/a/path'");
    }

    #[test]
    fn shell_escape_quotes_unicode() {
        assert_eq!(shell_escape("/tmp/日本語.txt"), "'/tmp/日本語.txt'");
    }

    #[test]
    fn shell_escape_quotes_empty_string() {
        assert_eq!(shell_escape(""), "''");
    }

    #[test]
    fn file_urls_join_with_spaces() {
        let paths = vec![
            PathBuf::from("/tmp/a.txt"),
            PathBuf::from("/tmp/my file.txt"),
        ];
        assert_eq!(
            file_urls_to_paste_string(&paths),
            "/tmp/a.txt '/tmp/my file.txt'"
        );
    }

    #[test]
    fn paste_contents_prioritizes_file_urls_over_image_and_text() {
        let paths = vec![PathBuf::from("/tmp/a.txt")];

        assert_eq!(
            paste_contents_from_parts(paths.clone(), Some(vec![1, 2, 3]), Some("text".to_string())),
            PasteContents::FileUrls(paths)
        );
    }

    #[test]
    fn paste_contents_prioritizes_image_over_text() {
        assert_eq!(
            paste_contents_from_parts(Vec::new(), Some(vec![1, 2, 3]), Some("text".to_string())),
            PasteContents::Image(vec![1, 2, 3])
        );
    }

    #[test]
    fn paste_contents_treats_empty_text_as_empty_clipboard() {
        assert_eq!(
            paste_contents_from_parts(Vec::new(), None, Some(String::new())),
            PasteContents::Empty
        );
    }

    #[test]
    fn file_urls_paste_string_round_trips_through_encode_paste() {
        let paths = vec![PathBuf::from("/tmp/a's file.txt")];
        let text = file_urls_to_paste_string(&paths);
        assert_eq!(text, "'/tmp/a'\\''s file.txt'");

        assert_eq!(
            crate::input::encode_paste(&text, false),
            Some(text.as_bytes().to_vec())
        );
        assert_eq!(
            crate::input::encode_paste(&text, true),
            Some(
                [b"\x1b[200~".as_slice(), text.as_bytes(), b"\x1b[201~"]
                    .concat()
                    .to_vec()
            )
        );
    }

    #[test]
    fn write_temp_png_writes_readable_file() {
        let bytes = b"fake-png-bytes";
        let path = write_temp_png(bytes).expect("write_temp_png should succeed");
        let contents = std::fs::read(&path).expect("temp file should be readable");
        assert_eq!(contents, bytes);
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn prune_removes_only_stale_noa_paste_pngs() {
        let dir = std::env::temp_dir().join(format!("noa-prune-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let stale = dir.join("noa-paste-1-1.png");
        let fresh = dir.join("noa-paste-1-2.png");
        let unrelated = dir.join("keep-me.png");
        for path in [&stale, &fresh, &unrelated] {
            std::fs::write(path, b"x").unwrap();
        }

        let future = std::time::SystemTime::now() + TEMP_PNG_MAX_AGE * 2;
        filetime_backdate(&stale, TEMP_PNG_MAX_AGE * 3);
        prune_stale_temp_pngs(&dir, std::time::SystemTime::now());
        assert!(!stale.exists(), "stale paste file should be removed");
        assert!(fresh.exists(), "fresh paste file must survive");
        assert!(unrelated.exists(), "non-noa-paste files must survive");

        // Everything ages out against a far-future now.
        prune_stale_temp_pngs(&dir, future);
        assert!(!fresh.exists());
        assert!(unrelated.exists());

        std::fs::remove_dir_all(&dir).ok();
    }

    /// Shift a file's mtime `age` into the past (test helper).
    fn filetime_backdate(path: &std::path::Path, age: std::time::Duration) {
        let past = std::time::SystemTime::now() - age;
        let file = std::fs::File::open(path).unwrap();
        file.set_modified(past).unwrap();
    }
}
