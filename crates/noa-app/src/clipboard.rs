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

/// Saves `png_bytes` to a fresh file in the system temp dir and returns its
/// path. Used to give pasted clipboard images a real path to paste, matching
/// Ghostty's paste-image-as-path behavior.
pub(crate) fn write_temp_png(png_bytes: &[u8]) -> anyhow::Result<PathBuf> {
    use std::time::{SystemTime, UNIX_EPOCH};

    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    let path = std::env::temp_dir().join(format!("noa-paste-{}-{nanos}.png", std::process::id()));
    std::fs::write(&path, png_bytes)?;
    Ok(path)
}

#[cfg(target_os = "macos")]
mod platform {
    use std::io::Write;
    use std::path::PathBuf;
    use std::process::{Command, Stdio};

    use anyhow::{Context, ensure};
    use objc2_app_kit::{
        NSBitmapImageFileType, NSBitmapImageRep, NSPasteboard, NSPasteboardTypeFileURL,
        NSPasteboardTypePNG, NSPasteboardTypeTIFF,
    };
    use objc2_foundation::{NSDictionary, NSURL};

    use super::PasteContents;

    pub(super) fn get_text() -> anyhow::Result<String> {
        let output = Command::new("/usr/bin/pbpaste")
            .arg("-Prefer")
            .arg("txt")
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
        let mut child = Command::new("/usr/bin/pbcopy")
            .stdin(Stdio::piped())
            .spawn()
            .context("failed to open macOS clipboard for writing")?;
        let mut stdin = child.stdin.take().context("failed to open pbcopy stdin")?;
        stdin
            .write_all(text.as_bytes())
            .context("failed to write text to macOS clipboard")?;
        drop(stdin);

        let status = child.wait().context("failed to finish pbcopy")?;
        ensure!(status.success(), "pbcopy exited with status {status}");
        Ok(())
    }

    pub(super) fn get_paste_contents() -> anyhow::Result<PasteContents> {
        let pasteboard = NSPasteboard::generalPasteboard();

        let file_urls = file_urls(&pasteboard);
        if !file_urls.is_empty() {
            return Ok(PasteContents::FileUrls(file_urls));
        }

        if let Some(png) = image_png(&pasteboard) {
            return Ok(PasteContents::Image(png));
        }

        match get_text() {
            Ok(text) if !text.is_empty() => Ok(PasteContents::Text(text)),
            _ => Ok(PasteContents::Empty),
        }
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
}
