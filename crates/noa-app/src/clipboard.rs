//! Clipboard access kept at the app/platform boundary.

use std::path::PathBuf;

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

    /// Reads the clipboard the way Ghostty's paste command does: file URLs
    /// and images take priority over plain text, so pasting a Finder
    /// selection or a copied screenshot inserts a shell-escaped path instead
    /// of raw pasteboard data.
    pub(crate) fn get_paste_contents(&mut self) -> anyhow::Result<PasteContents> {
        platform::get_paste_contents()
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
    use objc2_app_kit::{
        NSBitmapImageFileType, NSBitmapImageRep, NSPasteboard, NSPasteboardTypeFileURL,
        NSPasteboardTypePNG, NSPasteboardTypeString, NSPasteboardTypeTIFF,
    };
    use objc2_foundation::{NSDictionary, NSString, NSURL};

    use super::PasteContents;

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
        let ok =
            pasteboard.setString_forType(&NSString::from_str(text), unsafe {
                NSPasteboardTypeString
            });
        ensure!(ok, "failed to write text to macOS clipboard");
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
}

#[cfg(not(target_os = "macos"))]
mod platform {
    pub(super) fn get_text() -> anyhow::Result<String> {
        anyhow::bail!("clipboard is only implemented on macOS")
    }

    pub(super) fn set_text(_text: &str) -> anyhow::Result<()> {
        anyhow::bail!("clipboard is only implemented on macOS")
    }

    pub(super) fn get_paste_contents() -> anyhow::Result<super::PasteContents> {
        anyhow::bail!("clipboard is only implemented on macOS")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
