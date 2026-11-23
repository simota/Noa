//! Viewer-condition screenshots of each submitted entry.
//!
//! After a snapshot, the runner captures the entry's self-contained preview
//! (see [`crate::preview`]) with a headless Chrome at a few fixed viewports
//! and saves them under `r<N>/checks/<part>/`. The judge brief points the
//! judge at that folder, so every round is judged from the same images the
//! comparison viewer shows instead of a hand-built harness.

use std::fmt::Write as _;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Viewports captured for every entry: desktop, tablet, narrow phone.
pub const VIEWPORTS: [(u32, u32); 3] = [(1440, 900), (768, 1024), (320, 568)];

/// How every screenshot is taken; stated in briefs and manifests so judge and
/// contestants work from the same conditions.
pub const CONDITIONS: &str = "headless Chrome; the viewer copy framed in an `<iframe sandbox=\"allow-scripts\">` of exactly the viewport size (as `compare.html` frames it); `prefers-color-scheme: light`; reduced motion not requested; 5 s of virtual time before the capture; viewports 1440x900, 768x1024 and 320x568, where the height is the first view.";

/// Environment variable naming the Chrome/Chromium binary to use.
pub const CHROME_ENV: &str = "NOA_COMPETITION_CHROME";

/// How long one capture may take before it is abandoned.
const CAPTURE_TIMEOUT: Duration = Duration::from_secs(30);

/// File name of the screenshot for one viewport (`1440x900.png`).
pub fn file_name((width, height): (u32, u32)) -> String {
    format!("{width}x{height}.png")
}

/// Finds a Chrome-compatible browser: `$NOA_COMPETITION_CHROME`, then the
/// usual macOS app bundles, then `google-chrome`/`chromium` on `PATH`, then a
/// Playwright-installed headless shell.
pub fn find_chrome() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os(CHROME_ENV).map(PathBuf::from) {
        return path.is_file().then_some(path);
    }
    let bundles = [
        "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
        "/Applications/Chromium.app/Contents/MacOS/Chromium",
    ];
    if let Some(found) = bundles.iter().map(PathBuf::from).find(|p| p.is_file()) {
        return Some(found);
    }
    let names = [
        "google-chrome",
        "google-chrome-stable",
        "chromium",
        "chromium-browser",
    ];
    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path) {
            if let Some(found) = names.iter().map(|n| dir.join(n)).find(|p| p.is_file()) {
                return Some(found);
            }
        }
    }
    let cache = std::env::var_os("HOME")
        .map(PathBuf::from)?
        .join("Library/Caches/ms-playwright");
    let mut shells: Vec<PathBuf> = std::fs::read_dir(cache)
        .ok()?
        .filter_map(Result::ok)
        .filter(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with("chromium_headless_shell-")
        })
        .map(|e| {
            e.path()
                .join("chrome-headless-shell-mac-arm64/chrome-headless-shell")
        })
        .filter(|p| p.is_file())
        .collect();
    shells.sort();
    shells.pop()
}

/// A `file://` URL for an absolute path.
fn file_url(path: &Path) -> String {
    let mut url = String::from("file://");
    for byte in path.to_string_lossy().bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~/".contains(&byte) {
            url.push(byte as char);
        } else {
            let _ = write!(url, "%{byte:02X}");
        }
    }
    url
}

/// Captures `page` at every [`VIEWPORTS`] size into `out_dir` with `chrome`.
/// Returns one result per viewport, in order.
pub fn capture(chrome: &Path, page: &Path, out_dir: &Path) -> Vec<io::Result<PathBuf>> {
    if let Err(e) = std::fs::create_dir_all(out_dir) {
        return VIEWPORTS
            .iter()
            .map(|_| Err(io::Error::new(e.kind(), e.to_string())))
            .collect();
    }
    VIEWPORTS
        .iter()
        .map(|&viewport| capture_one(chrome, page, &out_dir.join(file_name(viewport)), viewport))
        .collect()
}

fn capture_one(
    chrome: &Path,
    page: &Path,
    out: &Path,
    (width, height): (u32, u32),
) -> io::Result<PathBuf> {
    let _ = std::fs::remove_file(out);
    // A throwaway profile, so a Chrome the user has open is never touched.
    let profile = std::env::temp_dir().join(format!(
        "noa-competition-shot-{}-{width}x{height}",
        std::process::id()
    ));
    // Frame the page the way `compare.html` does: a sandboxed iframe of the
    // requested size. Desktop Chrome's headless mode keeps the window at
    // least ~500px wide, so loading the page directly would lay a 320px
    // capture out wider than 320px; the iframe pins the layout width, and
    // any extra window width shows as the grey backdrop beside it.
    std::fs::create_dir_all(&profile)?;
    let frame = profile.join("frame.html");
    std::fs::write(
        &frame,
        format!(
            "<!doctype html><html><body style=\"margin:0;background:#808080\">\
             <iframe sandbox=\"allow-scripts\" src=\"{}\" \
             style=\"display:block;border:0;width:{width}px;height:{height}px\"></iframe>\
             </body></html>",
            file_url(page)
        ),
    )?;
    let mut child = Command::new(chrome)
        .args([
            "--headless",
            "--disable-gpu",
            "--hide-scrollbars",
            "--no-first-run",
            "--no-default-browser-check",
            // Let scripts and late layout settle before the capture.
            "--virtual-time-budget=5000",
            // Pin the color scheme so a capture does not follow the OS theme.
            "--blink-settings=preferredColorScheme=1",
        ])
        .arg(format!("--user-data-dir={}", profile.display()))
        .arg(format!("--window-size={width},{height}"))
        .arg(format!("--screenshot={}", out.display()))
        .arg(file_url(&frame))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if started.elapsed() >= CAPTURE_TIMEOUT {
            let _ = child.kill();
            let _ = child.wait();
            let _ = std::fs::remove_dir_all(&profile);
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!("{width}x{height}: no screenshot after {CAPTURE_TIMEOUT:?}"),
            ));
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    let _ = std::fs::remove_dir_all(&profile);
    if status.success() && out.is_file() {
        Ok(out.to_owned())
    } else {
        Err(io::Error::other(format!(
            "{width}x{height}: browser exited with {status} and no screenshot"
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_urls_escape_spaces_and_non_ascii() {
        assert_eq!(
            file_url(Path::new("/a b/ノア.html")),
            "file:///a%20b/%E3%83%8E%E3%82%A2.html"
        );
    }

    #[test]
    fn screenshot_names_carry_the_viewport() {
        assert_eq!(file_name((320, 568)), "320x568.png");
    }
}
