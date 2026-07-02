//! Clipboard access kept at the app/platform boundary.

#[derive(Debug, Default)]
pub(crate) struct SystemClipboard;

impl SystemClipboard {
    pub(crate) fn new() -> Self {
        Self
    }

    pub(crate) fn get_text(&mut self) -> anyhow::Result<String> {
        platform::get_text()
    }

    pub(crate) fn set_text(&mut self, text: &str) -> anyhow::Result<()> {
        platform::set_text(text)
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use std::io::Write;
    use std::process::{Command, Stdio};

    use anyhow::{Context, ensure};

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
}

#[cfg(not(target_os = "macos"))]
mod platform {
    pub(super) fn get_text() -> anyhow::Result<String> {
        anyhow::bail!("clipboard is only implemented on macOS")
    }

    pub(super) fn set_text(_text: &str) -> anyhow::Result<()> {
        anyhow::bail!("clipboard is only implemented on macOS")
    }
}
